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
/// 边界：缺邻居的方向不可用，只从可用方向里挑；都没有时退原样本。
/// 方向分数阈值是保守拟合起点，需实测调优 TODO(fit)。
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
    // 方向分数阈值：gx/gy 差小于它就当平手走四邻平均（0.031*65535，需拟合 TODO(fit)）。
    let grad_t = 0.031 * 65535.0f32;
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
                // G 邻居：标准拜尔里非 G 位的正交邻居都是 G。越界沿该轴镜像
                // （x=-1 → x=1），显式检查通道，退化图案不会把别的通道当 G 用。
                let reflect = |nx: isize, ny: isize| -> (usize, usize) {
                    let rx = if nx < 0 {
                        -nx
                    } else if nx >= w as isize {
                        2 * (w as isize - 1) - nx
                    } else {
                        nx
                    };
                    let ry = if ny < 0 {
                        -ny
                    } else if ny >= h as isize {
                        2 * (h as isize - 1) - ny
                    } else {
                        ny
                    };
                    (
                        rx.clamp(0, w as isize - 1) as usize,
                        ry.clamp(0, h as isize - 1) as usize,
                    )
                };
                let g_at = |dx: isize, dy: isize| -> Option<f32> {
                    let (nx, ny) = reflect(x as isize + dx, y as isize + dy);
                    if site_ch(nx, ny) == 1 {
                        Some(mosaic[ny * w + nx] as f32)
                    } else {
                        None
                    }
                };
                // 同色 distance-2 样本（二阶修正项）；越界钳到边内。
                let c2 = |dx: isize, dy: isize| -> f32 {
                    let nx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                    let ny = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                    mosaic[ny * w + nx] as f32
                };
                let (gl, gr, gu, gd) = (g_at(-1, 0), g_at(1, 0), g_at(0, -1), g_at(0, 1));
                let gx = match (gl, gr) {
                    (Some(l), Some(r)) => {
                        Some((l - r).abs() + (2.0 * v - c2(-2, 0) - c2(2, 0)).abs())
                    }
                    _ => None,
                };
                let gy = match (gu, gd) {
                    (Some(u), Some(d)) => {
                        Some((u - d).abs() + (2.0 * v - c2(0, -2) - c2(0, 2)).abs())
                    }
                    _ => None,
                };
                match (gx, gy) {
                    (Some(gx), Some(gy)) => {
                        if gx + grad_t < gy {
                            (gl.unwrap() + gr.unwrap()) / 2.0
                                + (2.0 * v - c2(-2, 0) - c2(2, 0)) / 4.0
                        } else if gy + grad_t < gx {
                            (gu.unwrap() + gd.unwrap()) / 2.0
                                + (2.0 * v - c2(0, -2) - c2(0, 2)) / 4.0
                        } else {
                            let n = [gl, gr, gu, gd].into_iter().flatten().collect::<Vec<_>>();
                            n.iter().sum::<f32>() / n.len() as f32
                        }
                    }
                    (Some(_), None) => {
                        (gl.unwrap() + gr.unwrap()) / 2.0 + (2.0 * v - c2(-2, 0) - c2(2, 0)) / 4.0
                    }
                    (None, Some(_)) => {
                        (gu.unwrap() + gd.unwrap()) / 2.0 + (2.0 * v - c2(0, -2) - c2(0, 2)) / 4.0
                    }
                    (None, None) => v,
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
    rgb
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
}
