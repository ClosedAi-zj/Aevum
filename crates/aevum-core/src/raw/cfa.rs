//! CFA → RGB：2×2 单元平均与 AsShotNeutral 增益。
//!
//! 口径与 `dng::demosaic` 一致，刻意保持同一套粗估器：两条路（DNG 与厂商 RAW）解出来的
//! 图在视觉上不该打架。没有共享代码是因为 `dng.rs` 是冻结的——动它的内部就要过它自己的
//! 金标测试，代价比复制 60 行大得多。
//!
//! 各段循环都按像素/单元切开并行，每个输出元素的算术顺序不变，结果与串行逐位一致。
//!
//! 这里**不是**相机配置文件意义上的准确显色：不做 ColorMatrix/相机色域/预设基线，不做
//! 镜头暗角与行增益补偿，也不查任何机型数据库。用了色彩矩阵才正确的 C/M/Y 图案、四拜尔
//! 与 X-Trans 一律在上层被拒、走预览兜底。白平衡只有 AsShotNeutral 的三通道增益，缺标记
//! 时完全不调色。
//!
//! CFA to RGB: 2×2 cell averaging and AsShotNeutral gains.
//!
//! The convention matches `dng::demosaic`, and the same coarse estimator is kept on purpose:
//! the images produced by the two paths (DNG and vendor RAW) must not visibly disagree. No
//! code is shared because `dng.rs` is frozen: touching its internals means passing its own
//! golden test, which costs more than copying 60 lines.
//!
//! Every loop is split by pixel or by cell and runs in parallel; the arithmetic order of each
//! output element is unchanged, so results are bit-identical to the serial version.
//!
//! This is **not** accurate color reproduction in the sense of a camera profile: no
//! ColorMatrix, no camera gamut, no preset baseline, no lens vignetting or row gain
//! compensation, and no camera model database. C/M/Y patterns, four-color Bayer and X-Trans,
//! which are only correct with a color matrix, are rejected upstream and fall back to the
//! preview path. White balance is the three-channel AsShotNeutral gain only; with the tag
//! missing, no grading is applied at all.

use std::sync::atomic::{AtomicU8, Ordering};

use rayon::prelude::*;

/// 2×2 单元平均的去拜尔。`cfa` 为 `None` 时输入本来就是线性亮度，三通道同值。
pub(crate) fn demosaic(mosaic: &[u16], w: usize, h: usize, cfa: Option<[u8; 4]>) -> Vec<u16> {
    let mut rgb = vec![0u16; w * h * 3];
    let Some(cfa) = cfa else {
        rgb.par_chunks_mut(3)
            .zip(mosaic.par_iter())
            .for_each(|(px, v)| px.copy_from_slice(&[*v, *v, *v]));
        return rgb;
    };
    let bw = w.div_ceil(2);
    let bh = h.div_ceil(2);
    let mut est = vec![[0u16; 3]; bw * bh];
    let mut known = vec![[false; 3]; bw * bh];
    // 每个 2×2 单元只读自己那四个像素，按单元行并行。
    est.par_chunks_mut(bw)
        .zip(known.par_chunks_mut(bw))
        .enumerate()
        .for_each(|(by, (est_row, known_row))| {
            for bx in 0..bw {
                let mut sum = [0u64; 3];
                let mut cnt = [0u32; 3];
                for dy in 0..2 {
                    for dx in 0..2 {
                        // 奇数宽高时边缘像素重复一次，这是唯一需要钳位的地方。
                        let x = (bx * 2 + dx).min(w - 1);
                        let y = (by * 2 + dy).min(h - 1);
                        let ch = cfa[(y % 2) * 2 + x % 2] as usize;
                        sum[ch] += u64::from(mosaic[y * w + x]);
                        cnt[ch] += 1;
                    }
                }
                for ch in 0..3 {
                    if cnt[ch] > 0 {
                        est_row[bx][ch] = round_div(sum[ch], u64::from(cnt[ch]));
                        known_row[bx][ch] = true;
                    }
                }
            }
        });
    // 缺通道只可能出现在边缘被重复的那一行/列上。对它们做邻居扩散，一轮没有进展就停
    // （整图确实没这个通道时留 0），不给大图反复复制整张估计表的机会。
    let mut todo: Vec<usize> = (0..bw * bh)
        .filter(|c| known[*c].iter().any(|k| !k))
        .collect();
    while !todo.is_empty() {
        let mut filled = 0usize;
        todo.retain(|&cell| {
            let (bx, by) = (cell % bw, cell / bw);
            let mut still_open = false;
            for ch in 0..3 {
                if known[cell][ch] {
                    continue;
                }
                let mut sum = 0u64;
                let mut cnt = 0u64;
                for (dx, dy) in [(1i32, 0), (-1, 0), (0, 1), (0, -1)] {
                    let nx = bx as i32 + dx;
                    let ny = by as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= bw as i32 || ny >= bh as i32 {
                        continue;
                    }
                    let n = ny as usize * bw + nx as usize;
                    if known[n][ch] {
                        sum += u64::from(est[n][ch]);
                        cnt += 1;
                    }
                }
                if cnt > 0 {
                    est[cell][ch] = round_div(sum, cnt);
                    known[cell][ch] = true;
                    filled += 1;
                } else {
                    still_open = true;
                }
            }
            still_open
        });
        if filled == 0 {
            break;
        }
    }
    rgb.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let cells = &est[(y / 2) * bw..][..bw];
        for x in 0..w {
            row[x * 3..x * 3 + 3].copy_from_slice(&cells[x / 2]);
        }
    });
    rgb
}

/// 黑场扣除 + 拉满 16 位量程。整数运算，测试能钉住逐位结果。
pub(crate) fn scale_levels(mosaic: &mut [u16], black: u32, white: u32) {
    let range = u64::from(white.saturating_sub(black)).max(1);
    mosaic.par_iter_mut().for_each(|v| {
        let raw = u64::from(*v).saturating_sub(u64::from(black));
        *v = (raw * 65535 / range).min(65535) as u16;
    });
}

/// 四舍五入除法；输入都是 u16 的均值，结果一定还在 u16 量程里。
fn round_div(sum: u64, cnt: u64) -> u16 {
    ((sum + cnt / 2) / cnt).min(u64::from(u16::MAX)) as u16
}

// ---------------------------------------------------------------------------
// 去马赛克方法注册表（A1 接缝：将来接 CNN 精修实现同一 trait 即可挂进来）
// ---------------------------------------------------------------------------

/// 去马赛克引擎接口。`cfa` 为 `None` 时输入本来就是线性亮度，三通道同值直通。
/// `name` 目前只有诊断用途（将来 CNN 精修与面板报告后端时用），暂未接线。
pub(crate) trait Demosaic: Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn run(&self, mosaic: &[u16], w: usize, h: usize, cfa: Option<[u8; 4]>) -> Vec<u16>;
}

/// 默认方法：2×2 单元平均（与 DNG 路径同口径）。directional 稳定一轮后再切，
/// 切换只改这一个常量。
///
/// Default method: 2×2 cell averaging (same convention as the DNG path). Switch once
/// directional has been stable for a release cycle; the switch touches this constant only.
pub const DEFAULT_DEMOSAIC: &str = "cell_average";

struct CellAverage;

impl Demosaic for CellAverage {
    fn name(&self) -> &'static str {
        "cell_average"
    }
    fn run(&self, mosaic: &[u16], w: usize, h: usize, cfa: Option<[u8; 4]>) -> Vec<u16> {
        demosaic(mosaic, w, h, cfa)
    }
}

struct Directional;

impl Demosaic for Directional {
    fn name(&self) -> &'static str {
        "directional"
    }
    fn run(&self, mosaic: &[u16], w: usize, h: usize, cfa: Option<[u8; 4]>) -> Vec<u16> {
        demosaic_directional(mosaic, w, h, cfa)
    }
}

/// 方法注册表：按名字挑引擎，未知名字退默认（cell_average）。
/// 解码路径统一走 [`selected_demosaic`]（诊断面板可切换）；显式传名字的
/// `demosaic_with` 留给测试与将来的分路径对比。
pub(crate) fn demosaic_with(
    method: &str,
    mosaic: &[u16],
    w: usize,
    h: usize,
    cfa: Option<[u8; 4]>,
) -> Vec<u16> {
    let engine: &dyn Demosaic = match method {
        "directional" => &Directional,
        _ => &CellAverage,
    };
    engine.run(mosaic, w, h, cfa)
}

/// 诊断面板可选的去马赛克方法，与注册表一一对应；下标 0 即 [`DEFAULT_DEMOSAIC`]。
///
/// Demosaic methods offered by the diagnostics panel, one-to-one with the registry; index 0
/// is [`DEFAULT_DEMOSAIC`].
pub const DEMOSAIC_METHODS: [&str; 2] = [DEFAULT_DEMOSAIC, "directional"];

/// 当前生效的方法在 [`DEMOSAIC_METHODS`] 里的下标。解码发生在后台线程（导出与
/// 测光多帧并行），用原子量而不是锁：读路径热，写只发生在诊断面板切换的一瞬。
static SELECTED_DEMOSAIC: AtomicU8 = AtomicU8::new(0);

/// 当前生效的去马赛克方法名。解码侧配置，不随影调参数落盘。
pub(crate) fn selected_demosaic() -> &'static str {
    DEMOSAIC_METHODS[SELECTED_DEMOSAIC.load(Ordering::Relaxed) as usize]
}

/// 按名字切换方法并让后续解码生效；未知名字返回 `false` 且保持原样。
pub(crate) fn set_selected_demosaic(name: &str) -> bool {
    match DEMOSAIC_METHODS.iter().position(|m| *m == name) {
        Some(i) => {
            SELECTED_DEMOSAIC.store(i as u8, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// 方向自适应去拜尔：Hamilton–Adams 1999 边向 G 插值 + 色差恒定假设插 R/B
/// （公开数学的独立实现，clean-room；不涉任何反编译伪代码）。
///
/// u16 定点改为 f32 工作缓冲——整数单元平均的逐位钉子只保留给旧路径。
/// 边界：缺邻居的方向不可用，只从可用方向里挑；都没有时退界内 G 邻居均值。
/// 颜色：色差平面过一遍 3×3 中值（[`suppress_false_color`]）压掉色度马赛克，
/// 绿通道一个像素不动。
/// 阈值 [`GRAD_T`] 与质量数据见 tests 模块的 `gradient_threshold_sweep`。
fn demosaic_directional(mosaic: &[u16], w: usize, h: usize, cfa: Option<[u8; 4]>) -> Vec<u16> {
    let mut rgb = vec![0u16; w * h * 3];
    let Some(cfa) = cfa else {
        rgb.par_chunks_mut(3)
            .zip(mosaic.par_iter())
            .for_each(|(px, v)| px.copy_from_slice(&[*v, *v, *v]));
        return rgb;
    };
    if w == 0 || h == 0 {
        return rgb;
    }
    demosaic_directional_with(mosaic, w, h, cfa, &mut rgb, GRAD_T, true);
    rgb
}

/// 方向分数阈值：`gx`/`gy` 之差小于它就当平手，退四邻平均。
///
/// 由 `gradient_threshold_sweep` 在四类合成靶上扫出来：0 → 0.4 之间平均 PSNR
/// **单调下降**（27.67 → 27.18 dB），所以最优在 0。取 0.005 而不是 0 是留一条
/// 0.5% 满量程的平局带——差值只有 1 LSB 时两个方向本来就分不出高低，按噪声选
/// 就会沿边来回翻（拉链伪影），代价只有 0.03 dB。
const GRAD_T: f32 = 0.005 * 65535.0;

/// [`demosaic_directional`] 的可调参版本：`grad_t` 与色差中值都显式给，
/// 供扫参与性能测量逐档重跑。
fn demosaic_directional_with(
    mosaic: &[u16],
    w: usize,
    h: usize,
    cfa: [u8; 4],
    rgb: &mut [u16],
    grad_t: f32,
    chroma_median: bool,
) {
    let site_ch = |x: usize, y: usize| cfa[(y % 2) * 2 + x % 2];

    // 阶段 1：G 重建。G 位直接取样本；非 G 位在水平/垂直两个 Hamilton–Adams
    // 候选里挑梯度小的（梯度 = 邻 G 差 + 同色 distance-2 二阶差分），
    // 平手取可用 G 邻居的平均。行与行互不相干，按行并行。
    let g_plane: Vec<f32> = mosaic
        .par_chunks_exact(w)
        .enumerate()
        .flat_map_iter(|(y, row)| {
            (0..w).map(move |x| {
                let v = row[x] as f32;
                if site_ch(x, y) == 1 {
                    return v;
                }
                // 边界口径刻意**不反射**。镜像会把 x=-1 映到 x=1，与另一侧的邻居撞成同一
                // 个像素，`|l - r|` 恒为 0 ——梯度假性归零，最外两列整片判成「水平边」。
                // 改成：参与比较的四个样本（两个 G 邻居 + 两个同色 distance-2）缺一个就
                // 判该方向不可用；两个方向都不可用时退到界内 G 邻居均值，一张都不剩才用
                // 原样本。这样只有最外两圈走退化路径，界内判据不受影响。
                let xi = x as isize;
                let yi = y as isize;
                let coord = |dx: isize, dy: isize| -> Option<(usize, usize)> {
                    let nx = xi + dx;
                    let ny = yi + dy;
                    if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                        None
                    } else {
                        Some((nx as usize, ny as usize))
                    }
                };
                let g_at = |dx: isize, dy: isize| -> Option<f32> {
                    let (nx, ny) = coord(dx, dy)?;
                    if site_ch(nx, ny) == 1 {
                        Some(mosaic[ny * w + nx] as f32)
                    } else {
                        None
                    }
                };
                let c2 = |dx: isize, dy: isize| -> Option<f32> {
                    let (nx, ny) = coord(dx, dy)?;
                    Some(mosaic[ny * w + nx] as f32)
                };
                // 一个方向的候选值：`(分数, 插值结果)`，样本不齐就整个方向退出。
                let axis = |a: Option<f32>, b: Option<f32>, ca: Option<f32>, cb: Option<f32>| {
                    let (a, b, ca, cb) = (a?, b?, ca?, cb?);
                    let corr = 2.0 * v - ca - cb;
                    Some(((a - b).abs() + corr.abs(), (a + b) / 2.0 + corr / 4.0))
                };
                let hz = axis(g_at(-1, 0), g_at(1, 0), c2(-2, 0), c2(2, 0));
                let vt = axis(g_at(0, -1), g_at(0, 1), c2(0, -2), c2(0, 2));
                // 退化路径的均值：只累加界内的 G 邻居，不建临时 Vec（热点，每像素一次）。
                let mean_gn = || {
                    let mut sum = 0.0f32;
                    let mut n = 0u32;
                    for g in [g_at(-1, 0), g_at(1, 0), g_at(0, -1), g_at(0, 1)]
                        .into_iter()
                        .flatten()
                    {
                        sum += g;
                        n += 1;
                    }
                    if n > 0 { sum / n as f32 } else { v }
                };
                match (hz, vt) {
                    (Some((gx, cx)), Some((gy, cy))) => {
                        if gx + grad_t < gy {
                            cx
                        } else if gy + grad_t < gx {
                            cy
                        } else {
                            mean_gn()
                        }
                    }
                    (Some((_, c)), None) | (None, Some((_, c))) => c,
                    (None, None) => mean_gn(),
                }
            })
        })
        .collect();

    // 阶段 2：R/B 用色差恒定假设——同色邻域内 C−G 近似常数。取邻域里同通道
    // 位点的 (样本 − G 估计) 均值补到当前点的 G 上；一个同色邻居都没有时
    // 色差取 0（退到 G 估计值）。输出行与行互不相干，按行并行。
    let pack = |f: f32| -> u16 { f.round().clamp(0.0, 65535.0) as u16 };
    rgb.par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, out_row)| {
            for x in 0..w {
                let i = y * w + x;
                let v = mosaic[i];
                let g = g_plane[i];
                let (r, gg, b) = match site_ch(x, y) {
                    1 => (
                        chroma_at(0, x, y, g, mosaic, w, h, &cfa, &g_plane),
                        g,
                        chroma_at(2, x, y, g, mosaic, w, h, &cfa, &g_plane),
                    ),
                    0 => (
                        v as f32,
                        g,
                        chroma_at(2, x, y, g, mosaic, w, h, &cfa, &g_plane),
                    ),
                    _ => (
                        chroma_at(0, x, y, g, mosaic, w, h, &cfa, &g_plane),
                        g,
                        v as f32,
                    ),
                };
                out_row[x * 3] = pack(r);
                out_row[x * 3 + 1] = pack(gg);
                out_row[x * 3 + 2] = pack(b);
            }
        });
    if chroma_median {
        suppress_false_color(rgb, &g_plane, w, h);
    }
}

/// 伪色抑制：对色差平面 `(R−G, B−G)` 做 3×3 中值，再把色差加回绿通道。
///
/// 方向插值只恢复绿通道，R/B 靠色差恒定假设补——插值残留的色度锯齿全部落在
/// `R−G` / `B−G` 上，表现为细纹理和 45° 边上的彩色马赛克。逐像素比合成靶
/// （`gradient_threshold_sweep`）：细纹理色差 712 → 545、p99 1973 → 1615，
/// 彩色边 p99 17375 → 15500，即峰值伪色降 11–18%；代价是 24MP 全解码
/// 116 ms → 256 ms，且 45° 边上 PSNR 略降 0.18 dB（真色细节被抹掉一点）。
/// 净判断：伪色是肉眼最刺眼的一类缺陷，PSNR 看不见它，所以留着这一遍。
/// 若哪天解码延迟成了瓶颈，把 `chroma_median` 传 `false` 就能省掉这 140 ms。
///
/// 用中值而不是均值：真色边界要保住台阶，均值会把两侧颜色抹成中间色。
/// **绿通道一个像素都不动**——方向插值挣来的亮度细节必须全部留下。
fn suppress_false_color(rgb: &mut [u16], g_plane: &[f32], w: usize, h: usize) {
    let n = w * h;
    let mut rc = vec![0.0f32; n];
    let mut bc = vec![0.0f32; n];
    rc.par_iter_mut()
        .zip(bc.par_iter_mut())
        .enumerate()
        .for_each(|(i, (r, b))| {
            *r = f32::from(rgb[i * 3]) - g_plane[i];
            *b = f32::from(rgb[i * 3 + 2]) - g_plane[i];
        });
    // 三行基址在 x 循环外算好，9 个下标就退化成加法；越界只钳一次（每条边一个值），
    // 不做逐邻居 clamp。这是把这一段的成本从 270 ms 压下来的主要手段。
    rgb.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let r0 = y.saturating_sub(1) * w;
        let r1 = y * w;
        let r2 = (y + 1).min(h - 1) * w;
        for x in 0..w {
            let xm = x.saturating_sub(1);
            let xp = (x + 1).min(w - 1);
            let ids = [
                r0 + xm,
                r0 + x,
                r0 + xp,
                r1 + xm,
                r1 + x,
                r1 + xp,
                r2 + xm,
                r2 + x,
                r2 + xp,
            ];
            let mut vr = [0.0f32; 9];
            let mut vb = [0.0f32; 9];
            for k in 0..9 {
                vr[k] = rc[ids[k]];
                vb[k] = bc[ids[k]];
            }
            let g = g_plane[r1 + x];
            row[x * 3] = (g + median9(&mut vr)).round().clamp(0.0, 65535.0) as u16;
            row[x * 3 + 2] = (g + median9(&mut vb)).round().clamp(0.0, 65535.0) as u16;
        }
    });
}

/// 9 个元素的中值：19 次比较交换的无分支中值网络（Paeth 版），比插入排序少一半
/// 指令且没有数据相关的分支——这一段的输入是图像噪声，分支预测基本猜不准。
/// 正确性由 `median9_matches_reference` 用随机数组对拍 `sort` 保证。
fn median9(v: &mut [f32; 9]) -> f32 {
    #[inline(always)]
    fn ce(v: &mut [f32; 9], a: usize, b: usize) {
        // `min`/`max` 编译成 minss/maxss，没有分支。
        let (lo, hi) = (v[a].min(v[b]), v[a].max(v[b]));
        v[a] = lo;
        v[b] = hi;
    }
    ce(v, 1, 2);
    ce(v, 4, 5);
    ce(v, 7, 8);
    ce(v, 0, 1);
    ce(v, 3, 4);
    ce(v, 6, 7);
    ce(v, 1, 2);
    ce(v, 4, 5);
    ce(v, 7, 8);
    ce(v, 0, 3);
    ce(v, 5, 8);
    ce(v, 4, 7);
    ce(v, 3, 6);
    ce(v, 1, 4);
    ce(v, 2, 5);
    ce(v, 4, 7);
    ce(v, 4, 2);
    ce(v, 6, 4);
    ce(v, 4, 2);
    v[4]
}

/// 色差恒定法的一步：`R(site) = G(site) + mean{ C(j) − G(j) : j 同色邻域 }`。
/// 候选邻域按站点类型给：G 位看正交 distance-1（标准拜尔里横/竖一对同色），
/// R/B 位看对角 distance-1（对角正是另一色）与正交 distance-2（同色自身）。
/// 只收界内且通道匹配的候选；空集返回 G 估计本身。
#[allow(clippy::too_many_arguments)]
fn chroma_at(
    ch: u8,
    x: usize,
    y: usize,
    g_site: f32,
    mosaic: &[u16],
    w: usize,
    h: usize,
    cfa: &[u8; 4],
    g_plane: &[f32],
) -> f32 {
    let site_is_g = cfa[(y % 2) * 2 + x % 2] == 1;
    // 站点自身通道不用插值（调用方直接给样本），这里只补另一个色度通道。
    let offsets: &[(isize, isize)] = if site_is_g {
        &[(1, 0), (-1, 0), (0, 1), (0, -1)]
    } else {
        &[
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
            (2, 0),
            (-2, 0),
            (0, 2),
            (0, -2),
        ]
    };
    let mut sum = 0.0f32;
    let mut cnt = 0usize;
    for (dx, dy) in offsets {
        let nx = x as isize + dx;
        let ny = y as isize + dy;
        if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
            continue;
        }
        let (nx, ny) = (nx as usize, ny as usize);
        if cfa[(ny % 2) * 2 + nx % 2] != ch {
            continue;
        }
        let j = ny * w + nx;
        sum += mosaic[j] as f32 - g_plane[j];
        cnt += 1;
    }
    if cnt > 0 {
        g_site + sum / cnt as f32
    } else {
        g_site
    }
}

/// AsShotNeutral（R,G,B 的中性反射比）转成定点增益，格式是 1/65536。
///
/// 增益按绿通道归一，所以 G 恒等于 1.0。中性值里有非正数就当作没有白平衡信息：
/// 宁可原样返回、由调用方按无增益处理，也不拿一个可疑系数刷全图。
/// 极端倍率（>16×）会被顶到上限，避免高光刷白。
pub(crate) fn gains_from_neutral(neutral: &[f64]) -> Option<[u32; 3]> {
    if neutral.len() != 3 {
        return None;
    }
    let green = *neutral.get(1)?;
    if !green.is_finite() || green <= 0.0 {
        return None;
    }
    let mut gains = [0u32; 3];
    for (c, g) in gains.iter_mut().enumerate() {
        let n = neutral[c];
        if !n.is_finite() || n <= 0.0 {
            return None;
        }
        let ratio = green / n;
        // 上限 16×：再大的增益只会把高光整片推成纯白，那是失真不是校正。
        let q = (ratio.clamp(1.0 / 16.0, 16.0) * 65536.0).round();
        // 到这里 q 一定落在 [4096, 1048576]，`as` 只是把已知的整数收回 u32。
        *g = q as u32;
    }
    Some(gains)
}

/// 逐通道增益。整数定点，保证同一输入永远同一输出。
pub(crate) fn apply_gains(rgb: &mut [u16], gains: [u32; 3]) {
    rgb.par_chunks_mut(3).for_each(|px| {
        for (i, v) in px.iter_mut().enumerate() {
            let gain = gains[i];
            if gain == 65536 {
                continue;
            }
            *v = ((u64::from(*v) * u64::from(gain)) >> 16).min(65535) as u16;
        }
    });
}

// ---------------------------------------------------------------------------
// 单测：合成 Bayer 图，验证 directional 在边缘处显著优于 cell_average
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const RGGB: [u8; 4] = [0, 1, 1, 2];

    /// 竖条纹亮度边缘（宽度 5，奇数宽专门让边缘跨进 2×2 单元内部），
    /// 两侧色差恒定（R−G、B−G 不变）——这是 Hamilton–Adams 的典型目标场景：
    /// 亮度边缘靠方向插值保住，色度靠色差恒定假设补回。
    /// 返回 (马赛克, 全分辨率真值 RGB)。
    fn stripe_bayer(w: usize, h: usize) -> (Vec<u16>, Vec<[u16; 3]>) {
        let truth = [[40000u16, 22000, 8000], [34000, 16000, 2000]];
        let mut mosaic = vec![0u16; w * h];
        let mut full = vec![[0u16; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                // 每 5 列换一次颜色；再叠一个纵向的轻微亮度斜坡
                let ramp = (y * 40 / h.max(1)) as u16;
                // 纵向斜坡是纯亮度增量，不破坏两侧的色差恒定（C−G 不变）。
                let t = truth[(x / 5) % 2].map(|v| v + ramp);
                full[y * w + x] = t;
                mosaic[y * w + x] = match (y % 2) * 2 + x % 2 {
                    0 => t[0],
                    1 | 2 => t[1],
                    _ => t[2],
                };
            }
        }
        (mosaic, full)
    }

    fn mae_per_chroma(out: &[u16], full: &[[u16; 3]], ch: usize) -> f64 {
        let n = full.len();
        out.chunks_exact(3)
            .zip(full)
            .map(|(px, t)| (px[ch] as i64 - t[ch] as i64).unsigned_abs() as f64)
            .sum::<f64>()
            / n as f64
    }

    #[test]
    fn directional_beats_cell_average_at_stripes() {
        let (w, h) = (40, 32);
        let (mosaic, full) = stripe_bayer(w, h);
        let cell = demosaic_with("cell_average", &mosaic, w, h, Some(RGGB));
        let dir = demosaic_with("directional", &mosaic, w, h, Some(RGGB));
        for ch in [0usize, 2] {
            let e_cell = mae_per_chroma(&cell, &full, ch);
            let e_dir = mae_per_chroma(&dir, &full, ch);
            println!("通道{ch}: cell_average MAE {e_cell:.1}，directional MAE {e_dir:.1}");
            assert!(
                e_dir < e_cell * 0.6,
                "通道{ch} 方向插值应显著更准：dir {e_dir:.1} vs cell {e_cell:.1}"
            );
        }
    }

    #[test]
    fn directional_is_exact_on_constant_mosaic() {
        // 恒色场：梯度全 0 → G 四邻平均恰为真值；色差恒为 0 → R/B 精确。
        let (w, h) = (8, 8);
        let mosaic = {
            let mut m = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    m.push(match (y % 2) * 2 + x % 2 {
                        0 => 1000u16,
                        1 | 2 => 500,
                        _ => 200,
                    });
                }
            }
            m
        };
        let dir = demosaic_with("directional", &mosaic, w, h, Some(RGGB));
        for px in dir.chunks_exact(3) {
            assert_eq!(px, &[1000, 500, 200]);
        }
        // 无 CFA 的线性亮度直通与 cell_average 同口径（三通道同值展开）。
        let lum: Vec<u16> = vec![777; w * h];
        assert_eq!(
            demosaic_with("directional", &lum, w, h, None),
            vec![777u16; w * h * 3]
        );
        // 注册表：未知名字退默认。
        let (mos, _) = stripe_bayer(8, 8);
        assert_eq!(
            demosaic_with("no_such_method", &mos, 8, 8, Some(RGGB)),
            demosaic_with("cell_average", &mos, 8, 8, Some(RGGB))
        );
    }

    // -----------------------------------------------------------------------
    // 合成靶与扫参：把 GRAD_T 从「猜的」变成「量出来的」
    //
    // 这台机器上没有任何厂商 RAW 样本（见 tests/raw_reads.rs 头注），所以质量只能
    // 对着**带真值的合成场景**量：先搭全分辨率 RGB 真值，按 CFA 前向采样成马赛克，
    // 再去拜尔，最后把输出和真值逐像素比。
    //
    // 两个指标分工明确：
    // - `g_mae` 只看绿通道。绿是亮度骨架，方向插值存在的理由就是它——这项不行
    //   就说明方向判据坏了。
    // - `chroma_mae` 只看色差 `(R−G, B−G)` 相对真值的偏差。场景里的真色差都是
    //   分区域恒定的，所以任何非零偏差都是算法自己制造出来的**伪色**。
    // -----------------------------------------------------------------------

    type Scene = fn(usize, usize) -> Vec<[f32; 3]>;

    /// 扫参用的预生成数据：场景名、全分辨率真值、投过 CFA 的马赛克。
    type Prepared = (&'static str, Vec<[f32; 3]>, Vec<u16>);

    /// 两侧色差各自恒定、亮度差 4 倍。方向判据的「简单题」。
    fn scene_stripes(w: usize, h: usize) -> Vec<[f32; 3]> {
        const DARK: [f32; 3] = [12000.0, 8000.0, 4000.0];
        const BRIGHT: [f32; 3] = [48000.0, 32000.0, 16000.0];
        (0..w * h)
            .map(|i| {
                let x = i % w;
                if (x / 5).is_multiple_of(2) {
                    DARK
                } else {
                    BRIGHT
                }
            })
            .collect()
    }

    /// 45° 斜边：`x + y` 决定在哪一侧。两个方向的梯度处处接近相等，专压平手判据——
    /// `GRAD_T` 小了会沿边来回翻（拉链），大了退化成四邻平均。
    fn scene_diagonal(w: usize, h: usize) -> Vec<[f32; 3]> {
        const A: [f32; 3] = [40000.0, 22000.0, 9000.0];
        const B: [f32; 3] = [10000.0, 9000.0, 7000.0];
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if ((x + y) / 6).is_multiple_of(2) {
                    A
                } else {
                    B
                }
            })
            .collect()
    }

    /// 细纹理：亮度逐像素伪随机后再带限，色差恒定。这是**伪色的主要来源**——
    /// 方向判据把噪声当边，绿通道在非 G 位上被邻居投票，票数一偏就在色度上留下马赛克。
    fn scene_texture(w: usize, h: usize) -> Vec<[f32; 3]> {
        let mut seed = 0x2545_f491u32;
        let mut v: Vec<[f32; 3]> = (0..w * h)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let t = ((seed >> 8) & 0xFF) as f32 / 255.0;
                let y = 8000.0 + t * 24000.0;
                [y + 5000.0, y, y - 3000.0]
            })
            .collect();
        band_limit(&mut v, w, h);
        v
    }

    /// 彩色斜边：两侧**色差不同**（暖/冷），考的是色差恒定假设在真色边界上会不会
    /// 把颜色抹过去。
    fn scene_color_edge(w: usize, h: usize) -> Vec<[f32; 3]> {
        const WARM: [f32; 3] = [46000.0, 26000.0, 12000.0];
        const COOL: [f32; 3] = [14000.0, 24000.0, 42000.0];
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if ((x + y) / 5).is_multiple_of(2) {
                    WARM
                } else {
                    COOL
                }
            })
            .collect()
    }

    /// 可分离 `[1,2,1]/4` 两遍，把场景带限到大约 0.35 cyc/px——与真实镜头 MTF 同量级。
    /// 逐像素白色的靶没有意义：马赛克采不到的那些点，真值是谁都不知道。
    fn band_limit(img: &mut [[f32; 3]], w: usize, h: usize) {
        let pass = |src: &[[f32; 3]]| -> Vec<[f32; 3]> {
            let at = |x: usize, y: usize| src[y * w + x];
            let mut out = vec![[0.0f32; 3]; w * h];
            for y in 0..h {
                for x in 0..w {
                    let (l, r) = (x.saturating_sub(1), (x + 1).min(w - 1));
                    let (u, d) = (y.saturating_sub(1), (y + 1).min(h - 1));
                    for c in 0..3 {
                        out[y * w + x][c] = (at(l, y)[c]
                            + 2.0 * at(x, y)[c]
                            + at(r, y)[c]
                            + at(x, u)[c]
                            + 2.0 * at(x, y)[c]
                            + at(x, d)[c])
                            / 8.0;
                    }
                }
            }
            out
        };
        let a = pass(img);
        let b = pass(&a);
        img.copy_from_slice(&b);
    }

    fn bayer_sample(truth: &[[f32; 3]], w: usize, h: usize, cfa: [u8; 4]) -> Vec<u16> {
        let mut m = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                let ch = cfa[(y % 2) * 2 + x % 2] as usize;
                m[y * w + x] = truth[y * w + x][ch].round().clamp(0.0, 65535.0) as u16;
            }
        }
        m
    }

    /// 绿通道平均绝对误差：方向插值存在的理由。
    fn g_mae(out: &[u16], truth: &[[f32; 3]]) -> f32 {
        let s: f64 = out
            .chunks_exact(3)
            .zip(truth)
            .map(|(p, t)| (f64::from(p[1]) - f64::from(t[1])).abs())
            .sum();
        (s / truth.len() as f64) as f32
    }

    /// 色差平均绝对误差：非零即伪色（场景真色差按区域恒定）。
    fn chroma_mae(out: &[u16], truth: &[[f32; 3]]) -> f32 {
        let s: f64 = out
            .chunks_exact(3)
            .zip(truth)
            .map(|(p, t)| {
                let (orr, og, ob) = (f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
                let (tr, tg, tb) = (f64::from(t[0]), f64::from(t[1]), f64::from(t[2]));
                (((orr - og) - (tr - tg)).abs() + ((ob - og) - (tb - tg)).abs()) / 2.0
            })
            .sum();
        (s / truth.len() as f64) as f32
    }

    /// 整图 PSNR（满量程 65535）。比 MAE 更能反映「肉眼看整体像不像」：
    /// MAE 被少量极端像素主导，PSNR 是均方口径，对大面积轻微偏差更敏感。
    fn psnr(out: &[u16], truth: &[[f32; 3]]) -> f32 {
        let se: f64 = out
            .chunks_exact(3)
            .zip(truth)
            .map(|(p, t)| {
                (0..3)
                    .map(|c| {
                        let d = f64::from(p[c]) - f64::from(t[c]);
                        d * d
                    })
                    .sum::<f64>()
            })
            .sum();
        let mse = se / (truth.len() * 3) as f64;
        if mse <= 0.0 {
            return f32::INFINITY;
        }
        (10.0 * (65535.0f64 * 65535.0 / mse).log10()) as f32
    }

    /// 色差误差的 99 分位。伪色是**局部脉冲**型缺陷：均值会被大片正常区域稀释，
    /// p99 才能看出「最刺眼的那批点到底有多偏」。
    fn chroma_p99(out: &[u16], truth: &[[f32; 3]]) -> f32 {
        let mut v: Vec<f32> = out
            .chunks_exact(3)
            .zip(truth)
            .map(|(p, t)| {
                let (orr, og, ob) = (f64::from(p[0]), f64::from(p[1]), f64::from(p[2]));
                let (tr, tg, tb) = (f64::from(t[0]), f64::from(t[1]), f64::from(t[2]));
                ((((orr - og) - (tr - tg)).abs() + ((ob - og) - (tb - tg)).abs()) / 2.0) as f32
            })
            .collect();
        v.sort_by(|a, b| a.partial_cmp(b).expect("无 NaN"));
        v[(v.len() * 99) / 100]
    }

    fn directional_at(
        mosaic: &[u16],
        w: usize,
        h: usize,
        cfa: [u8; 4],
        grad_t: f32,
        chroma_median: bool,
    ) -> Vec<u16> {
        let mut out = vec![0u16; w * h * 3];
        demosaic_directional_with(mosaic, w, h, cfa, &mut out, grad_t, chroma_median);
        out
    }

    /// 扫参出表：`GRAD_T` 取哪一档由这张表定。
    ///
    /// ```text
    /// cargo test -p aevum-core --lib raw::cfa::tests::gradient_threshold_sweep -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "扫参出表，不参与门槛"]
    fn gradient_threshold_sweep() {
        let (w, h) = (96, 96);
        let scenes: [(&str, Scene); 4] = [
            ("竖条纹", scene_stripes),
            ("45°斜边", scene_diagonal),
            ("细纹理", scene_texture),
            ("彩色边", scene_color_edge),
        ];
        let prepared: Vec<Prepared> = scenes
            .into_iter()
            .map(|(name, f)| {
                let truth = f(w, h);
                let mos = bayer_sample(&truth, w, h, RGGB);
                (name, truth, mos)
            })
            .collect();

        let cell: Vec<(f32, f32)> = prepared
            .iter()
            .map(|(_, t, m)| {
                let out = demosaic_with("cell_average", m, w, h, Some(RGGB));
                (g_mae(&out, t), chroma_mae(&out, t))
            })
            .collect();
        println!(
            "\n基线 cell_average: G-MAE {:.1}，色差MAE {:.1}",
            cell.iter().map(|c| c.0).sum::<f32>() / cell.len() as f32,
            cell.iter().map(|c| c.1).sum::<f32>() / cell.len() as f32
        );
        println!(
            "逐场景: {}",
            prepared
                .iter()
                .zip(&cell)
                .map(|((n, _, _), (g, c))| format!("{n} G{g:.1}/C{c:.1}"))
                .collect::<Vec<_>>()
                .join("  ")
        );

        println!(
            "\n{:>12} {:>8} {:>8} {:>9}  逐场景 G/色差",
            "阈值/满量程", "G-MAE", "色差MAE", "平均PSNR"
        );
        for k in [0.0f32, 0.005, 0.01, 0.02, 0.031, 0.05, 0.08, 0.15, 0.4] {
            let t = k * 65535.0;
            let mut gs = 0.0;
            let mut cs = 0.0;
            let mut ps = 0.0;
            let mut detail = Vec::new();
            for (name, truth, mos) in &prepared {
                let out = directional_at(mos, w, h, RGGB, t, true);
                let g = g_mae(&out, truth);
                let c = chroma_mae(&out, truth);
                gs += g;
                cs += c;
                ps += psnr(&out, truth);
                detail.push(format!("{name} {g:.1}/{c:.1}"));
            }
            println!(
                "{k:>12.3} {:>8.1} {:>8.1} {:>9.2}  {}",
                gs / prepared.len() as f32,
                cs / prepared.len() as f32,
                ps / prepared.len() as f32,
                detail.join("  ")
            );
        }

        println!(
            "\n{:<10} {:>7} {:>7} {:>7} | {:>7} {:>7} {:>7} | {:>7} {:>7} {:>7}",
            "场景",
            "cell色差",
            "cellp99",
            "cellPSNR",
            "中值色差",
            "中值p99",
            "中值PSNR",
            "无中值色差",
            "无中值p99",
            "无中值PSNR"
        );
        for (name, truth, mos) in &prepared {
            let cell = demosaic_with("cell_average", mos, w, h, Some(RGGB));
            let med = directional_at(mos, w, h, RGGB, GRAD_T, true);
            let raw = directional_at(mos, w, h, RGGB, GRAD_T, false);
            println!(
                "{name:<10} {:>7.0} {:>7.0} {:>7.2} | {:>7.0} {:>7.0} {:>7.2} | \
                 {:>7.0} {:>7.0} {:>7.2}",
                chroma_mae(&cell, truth),
                chroma_p99(&cell, truth),
                psnr(&cell, truth),
                chroma_mae(&med, truth),
                chroma_p99(&med, truth),
                psnr(&med, truth),
                chroma_mae(&raw, truth),
                chroma_p99(&raw, truth),
                psnr(&raw, truth)
            );
        }
        println!();
    }

    /// 验收：方向插值在每类场景上都该赢得或打平绿通道，且不该明显恶化伪色。
    /// 具体倍率与阈值由 `gradient_threshold_sweep` 的表定，改算法时这张表要重跑。
    #[test]
    fn directional_wins_on_green_without_false_color() {
        let (w, h) = (96, 96);
        let scenes: [(&str, Scene); 4] = [
            ("竖条纹", scene_stripes),
            ("45°斜边", scene_diagonal),
            ("细纹理", scene_texture),
            ("彩色边", scene_color_edge),
        ];
        for (name, f) in scenes {
            let truth = f(w, h);
            let mos = bayer_sample(&truth, w, h, RGGB);
            let cell = demosaic_with("cell_average", &mos, w, h, Some(RGGB));
            let dir = demosaic_with("directional", &mos, w, h, Some(RGGB));
            let (pc, pd) = (psnr(&cell, &truth), psnr(&dir, &truth));
            println!(
                "{name}: PSNR {pd:.2} vs {pc:.2} dB；绿 {:.1} vs {:.1}；色差 {:.0} vs {:.0}",
                g_mae(&dir, &truth),
                g_mae(&cell, &truth),
                chroma_mae(&dir, &truth),
                chroma_mae(&cell, &truth)
            );
            // 主判据用 PSNR：实测四场景 +3.0 … +10.6 dB，留 2 dB 余量防止悄悄退化。
            // 不用单通道 MAE 当主判据——它被少数极端像素主导，会漏掉整体改善
            // （彩色边上方向插值的 R 通道 MAE 反而更大，PSNR 却高 4.6 dB）。
            assert!(
                pd > pc + 2.0,
                "{name}：整体 PSNR 应至少高 2 dB（{pd:.2} vs {pc:.2}）"
            );
        }

        // 方向插值的目标场景：有明确亮度结构的条纹与细纹理，绿通道必须大幅领先。
        for (name, f) in [
            ("竖条纹", scene_stripes as Scene),
            ("细纹理", scene_texture),
        ] {
            let truth = f(w, h);
            let mos = bayer_sample(&truth, w, h, RGGB);
            let gc = g_mae(
                &demosaic_with("cell_average", &mos, w, h, Some(RGGB)),
                &truth,
            );
            let gd = g_mae(
                &demosaic_with("directional", &mos, w, h, Some(RGGB)),
                &truth,
            );
            assert!(
                gd < gc * 0.5,
                "{name}：绿通道误差该减半以上（{gd:.1} vs {gc:.1}）"
            );
        }
    }

    /// 中值网络对拍：随机数组的中值必须等于排序后的第 5 个。网络写错一个下标就会
    /// 静默给出「接近中值但不等于」的结果，肉眼看不出来，只能这样验。
    #[test]
    fn median9_matches_reference() {
        let mut seed = 0x1234_5678u32;
        for _ in 0..20_000 {
            let mut v = [0.0f32; 9];
            for s in v.iter_mut() {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *s = ((seed >> 8) & 0xFFFF) as f32;
            }
            let mut sorted = v;
            sorted.sort_by(|a, b| a.partial_cmp(b).expect("测试数据无 NaN"));
            assert_eq!(median9(&mut v), sorted[4]);
        }
    }

    /// 退化输入不能炸：1×1、单行、单列、宽度奇偶混合。
    #[test]
    fn directional_survives_degenerate_sizes() {
        for (w, h) in [(1usize, 1usize), (1, 7), (7, 1), (2, 2), (3, 3), (5, 2)] {
            let truth = scene_stripes(w, h);
            let mos = bayer_sample(&truth, w, h, RGGB);
            let out = demosaic_with("directional", &mos, w, h, Some(RGGB));
            assert_eq!(out.len(), w * h * 3, "{w}×{h} 输出尺寸不对");
        }
    }

    /// 24MP 成本对比。设计文档风险表写的是「方向自适应约 3-4× 单元平均」，
    /// 这里给实测数，切默认前必须看这一行。
    ///
    /// ```text
    /// cargo test -p aevum-core --lib raw::cfa::tests::demosaic_cost_at_24mp -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "性能测量，不参与门槛"]
    fn demosaic_cost_at_24mp() {
        // 先小图热一遍：第一次调用要付 rayon 线程池与内存首次触碰的账。
        let warm = scene_texture(64, 64);
        let warm_mos = bayer_sample(&warm, 64, 64, RGGB);
        let _ = demosaic_with("directional", &warm_mos, 64, 64, Some(RGGB));

        let (w, h) = (6000, 4000);
        let truth = scene_texture(w, h);
        let mos = bayer_sample(&truth, w, h, RGGB);
        let mut base = 0.0;
        for name in DEMOSAIC_METHODS {
            let t0 = std::time::Instant::now();
            let out = demosaic_with(name, &mos, w, h, Some(RGGB));
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if name == DEFAULT_DEMOSAIC {
                base = ms;
            }
            println!(
                "{name:<12} {ms:>7.0} ms  ({:.1}×，{} 像素)",
                ms / base,
                out.len() / 3
            );
        }
    }
}
