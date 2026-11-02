//! 焦点专用自动对齐的五条判据（A-C1…A-C5）。
//!
//! # 为什么单独一个文件
//!
//! `align_golden.rs` / `align_synthetic.rs` 钉的是序列去抖那套模板匹配；这里钉的是
//! **另起的一条链路**：1/2 luma 代理上的邻居链式 + 8×6 网格平移场 + 1/8 RGB 代理上的
//! 收益校验。五条各守一个失效模式（已知位移、端到端收益、回退不误伤、低纹理失败可见、
//! breathing 优于全局平移），放一处才看得出三段口径有没有互相打架。
//!
//! # 夹具与冻结口径
//!
//! A-C1 用 128² 的两次 3×3 盒模糊带限纹理测相位相关本身的整数/亚像素精度。A-C2 起
//! 改用**白噪声**底图：两次模糊的带限纹理三帧错位平均只掉 17% 测度，`clean/unaligned`
//! 上限 1.204 < 判据的 1.3×，物理上凑不出收益；白噪声把上限抬到 1.57，才让「对齐
//! 真的把测度找回来」这件事有牙。错位取 **8px 偶数**（设计 §4 写 7px，落地冻结为
//! 8px）：7px 在 1/2 代理上落在 3.5px 半像素相位，估成 −6.89，对齐后只回到干净基准
//! 的 90.2% 贴 0.9 线；8px 是代理整数相位，估成 −8.002，回到 99.4%。
//!
//! A-C5 的统计口径按拍板 ⑧ **冻结**为「四个角块各自外角 1/4 tile 区的合成测度均值」。
//! 夹具从设计的「单帧 1.02× 中心缩放」改为 **5 帧 1.00→1.02 渐进 breathing 焦点栈**：
//! 设计给的 1.02× 是 6000×4000 上角落 72px 位移的量级，512² 夹具只有 5px；且单帧构造
//! 里参考帧全清晰，Weighted 的测度⁴ 软选择会压倒性挑它，网格 warp 的非整数插值反而
//! 比不动更糊（实测比值 0.9996）。多帧渐进让各帧都经过一次插值、清晰度相当，才把
//! 「网格位移场优于单一全局平移」这个相对结论显出来。逐块 warp 在判据里用**硬切**
//! 双线性采样做替身，T4 的 24px 接缝淡化不属本批，不影响相对结论。
//!
//! # Five assertions for focus alignment (A-C1...A-C5)
//!
//! # Why a separate file
//!
//! `align_golden.rs` / `align_synthetic.rs` pin the template matcher used for sequence
//! stabilisation. This file pins the **separate chain**: neighbour-chained shifts on 1/2 luma
//! proxies, an 8x6 grid field, and a gain check on 1/8 RGB proxies. Each of the five
//! assertions guards one failure mode (known shifts, end-to-end gain, harmless revert,
//! visible low-texture failure, breathing beating a global translation); keeping them
//! together shows whether the three stages agree.
//!
//! # Fixtures and the frozen statistic
//!
//! A-C1 uses a 128x128 twice-blurred band-limited texture to measure the phase correlator's
//! integer / sub-pixel accuracy. From A-C2 on the fixture is **white noise**: a twice-blurred
//! texture loses only 17% of its measure under a three-frame offset average, so
//! `clean/unaligned` caps at 1.204 < the assertion's 1.3x and the gain is physically
//! unreachable; white noise lifts the cap to 1.57 and gives the assertion teeth. The offset
//! is **8 px, even** (design section 4 says 7 px; landing freezes it at 8 px): 7 px becomes a
//! 3.5 px half-pixel phase on the half-size proxy and is estimated as -6.89, recovering only
//! 90.2% of the clean baseline -- right on the 0.9 line. 8 px lands on an integer proxy
//! phase, is estimated as -8.002, and recovers 99.4%.
//!
//! A-C5 freezes the statistic from ruling 8: the mean composite measure over the **outer
//! quarter of each of the four corner tiles**. The fixture changes from the design's single
//! 1.02x centre zoom to a **5-frame 1.00->1.02 progressive breathing focus stack**: the
//! design's 1.02x assumes roughly 72 px of corner displacement on 6000x4000, but a 512x512
//! fixture only has 5 px; moreover with a single zoomed frame the fully sharp reference wins
//! the measure-to-the-4th soft selection outright, and the grid's non-integer bilinear
//! resampling ends up softer than doing nothing (measured ratio 0.9996). Progressive frames
//! all pass through one interpolation and have comparable sharpness, which exposes the
//! relative "grid field beats a single global translation" conclusion. The per-block warp
//! here uses a hard bilinear substitution; T4's 24 px seam feather is out of scope and does
//! not change the relative conclusion.

use aevum_core::focus::{Method, Options, focus_stack};
use aevum_core::focus_align::{
    self, AlignStatus, BlockField, DecideInput, FocusAlignReport, GRID_COLS, GRID_ROWS,
};

const W: usize = 512;
const H: usize = 512;
/// A-C2 错位栈的位移（px，偶数以保证 1/2 代理上是整数相位）。
///
/// Offset of the A-C2 stack (px; even so it is an integer phase on the half-size proxy).
const SHIFT: f64 = 8.0;
/// A-C5 渐进 breathing 焦点栈的帧数。
///
/// Frame count of the A-C5 progressive breathing focus stack.
const BREATH_FRAMES: usize = 5;
/// A-C5 最后一帧相对第一帧的中心缩放倍率（拍板 ⑧ 的 1.02×）。
///
/// Centre-zoom factor of the last A-C5 frame relative to the first (ruling 8's 1.02x).
const BREATH_ZOOM: f64 = 1.02;

/// 确定性像素哈希，返回 0..1。
///
/// Deterministic per-pixel hash in 0..1.
fn hash(x: usize, y: usize, seed: u64) -> f32 {
    let mut n = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    n ^= n >> 30;
    n = n.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    n ^= n >> 27;
    (((n >> 40) & 0xFFFF) as f32) / 65535.0
}

/// 3×3 盒模糊（夹边），把白噪声压成带限纹理，亚像素插值才不会混叠。
///
/// 3x3 box blur with clamped edges: turns white noise into band-limited texture so
/// sub-pixel interpolation does not alias.
fn blur3(plane: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0f32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let yy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                    let xx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    acc += plane[yy * w + xx];
                }
            }
            out[y * w + x] = acc / 9.0;
        }
    }
    out
}

/// 单通道带限纹理（两次 3×3 盒模糊）。
///
/// Single-channel band-limited texture (two 3x3 box-blur passes).
fn mono_texture(w: usize, h: usize, seed: u64) -> Vec<f32> {
    let raw: Vec<f32> = (0..h)
        .flat_map(|y| (0..w).map(move |x| hash(x, y, seed)))
        .collect();
    let once = blur3(&raw, w, h);
    blur3(&once, w, h)
}

/// 可调锐度的交错 RGB 纹理：`passes` 是 3×3 盒模糊遍数（0 = 白噪声）。
///
/// Interleaved RGB texture with adjustable sharpness: `passes` is the number of 3x3 box
/// blur passes (0 = white noise).
fn sharp_texture(w: usize, h: usize, seed: u64, passes: usize) -> Vec<f32> {
    let mut mono: Vec<f32> = (0..h)
        .flat_map(|y| (0..w).map(move |x| hash(x, y, seed)))
        .collect();
    for _ in 0..passes {
        mono = blur3(&mono, w, h);
    }
    let mut out = vec![0.0f32; w * h * 3];
    for (p, &v) in mono.iter().enumerate() {
        let j = p * 3;
        out[j] = v;
        out[j + 1] = v;
        out[j + 2] = v;
    }
    out
}

/// 交错 RGB 带限纹理（三通道同值，测度只看亮度）。
///
/// Interleaved RGB band-limited texture (all three channels equal, the measure reads luma).
fn texture(w: usize, h: usize, seed: u64) -> Vec<f32> {
    sharp_texture(w, h, seed, 2)
}

/// RGB 双线性采样（夹边）。
///
/// Bilinear RGB sampling with clamped edges.
fn sample_rgb(src: &[f32], w: usize, h: usize, x: f64, y: f64) -> [f32; 3] {
    let fx = x.floor();
    let fy = y.floor();
    let tx = x - fx;
    let ty = y - fy;
    let x0 = (fx as isize).clamp(0, w as isize - 1) as usize;
    let y0 = (fy as isize).clamp(0, h as isize - 1) as usize;
    let xa = (x0 + 1).min(w - 1);
    let ya = (y0 + 1).min(h - 1);
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let v00 = src[(y0 * w + x0) * 3 + c] as f64;
        let v10 = src[(y0 * w + xa) * 3 + c] as f64;
        let v01 = src[(ya * w + x0) * 3 + c] as f64;
        let v11 = src[(ya * w + xa) * 3 + c] as f64;
        let top = v00 * (1.0 - tx) + v10 * tx;
        let bot = v01 * (1.0 - tx) + v11 * tx;
        out[c] = (top * (1.0 - ty) + bot * ty) as f32;
    }
    out
}

/// 单通道双线性 warp，供 A-C1 造已知位移夹具。
///
/// Single-channel bilinear warp, used to build A-C1's known-shift fixtures.
fn warp_mono(src: &[f32], w: usize, h: usize, off: [f64; 2]) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let sx = x as f64 - off[0];
            let sy = y as f64 - off[1];
            let fx = sx.floor();
            let fy = sy.floor();
            let tx = sx - fx;
            let ty = sy - fy;
            let x0 = (fx as isize).clamp(0, w as isize - 1) as usize;
            let y0 = (fy as isize).clamp(0, h as isize - 1) as usize;
            let xa = (x0 + 1).min(w - 1);
            let ya = (y0 + 1).min(h - 1);
            let top = src[y0 * w + x0] as f64 * (1.0 - tx) + src[y0 * w + xa] as f64 * tx;
            let bot = src[ya * w + x0] as f64 * (1.0 - tx) + src[ya * w + xa] as f64 * tx;
            out[y * w + x] = (top * (1.0 - ty) + bot * ty) as f32;
        }
    }
    out
}

/// 中心缩放：`dst(x) = src((x - c)/factor + c)`。
///
/// Centre zoom: `dst(x) = src((x - c)/factor + c)`.
fn zoom_center(src: &[f32], w: usize, h: usize, factor: f64) -> Vec<f32> {
    let cx = (w - 1) as f64 * 0.5;
    let cy = (h - 1) as f64 * 0.5;
    let mut out = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let sx = (x as f64 - cx) / factor + cx;
            let sy = (y as f64 - cy) / factor + cy;
            let v = sample_rgb(src, w, h, sx, sy);
            let j = (y * w + x) * 3;
            out[j..j + 3].copy_from_slice(&v);
        }
    }
    out
}

/// 逐块（硬切）warp：判据里替身 T4 的接缝淡化，块位移取该像素所属网格块。
///
/// Hard per-block warp: a stand-in for T4's feathered seams, using the grid block that owns
/// each pixel.
fn warp_grid(src: &[f32], w: usize, h: usize, shifts: &[[f64; 2]]) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h * 3];
    let cols = GRID_COLS;
    let rows = GRID_ROWS;
    for y in 0..h {
        for x in 0..w {
            let bx = (x * cols / w).min(cols - 1);
            let by = (y * rows / h).min(rows - 1);
            let off = shifts[by * cols + bx];
            let v = sample_rgb(src, w, h, x as f64 - off[0], y as f64 - off[1]);
            let j = (y * w + x) * 3;
            out[j..j + 3].copy_from_slice(&v);
        }
    }
    out
}

/// 加权平均融合（与 `focus_stack` 的 Weighted 分支同参）。
///
/// Weighted-average fusion (same options as `focus_stack`'s Weighted branch).
fn fuse(frames: &[Vec<f32>], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let opts = Options {
        method: Method::Weighted,
        radius,
        threshold: 0.0,
        smooth_z: 0,
        feather: 0.0,
        levels: 0,
    };
    let refs: Vec<&[f32]> = frames.iter().map(Vec::as_slice).collect();
    focus_stack(&refs, w, h, &opts).expect("weighted fusion")
}

/// 平均焦点测度（切掉 16px 插值边界）。
///
/// Mean focus measure with a 16 px interpolation border cut off.
fn measure(out: &[f32], w: usize, h: usize, radius: usize) -> f32 {
    focus_align::mean_measure(out, w, h, radius, 16)
}

/// 邻居对里失败块最大数与一次管道的完整结果。
///
/// Full pipeline result plus the largest failed-block count among neighbour pairs.
struct Pipeline {
    report: FocusAlignReport,
    out: Vec<f32>,
    unaligned: Vec<f32>,
    offsets: Vec<[f64; 2]>,
    fields: Vec<BlockField>,
}

/// 端到端管道：代理 → 块位移场 → 链式累计 → 收益统计 → 三态决策 → 出图。
///
/// `status != Adopted` 时**不套位移**（回退/失败都出未对齐几何），这正是 A-C3/A-C4
/// 要守的行为。T4 的逐块 warp 在这里用全局位移替身，不影响五条判据的相对结论。
///
/// End-to-end pipeline: proxies -> block fields -> chained offsets -> gain stats -> three-state
/// decision -> output. When `status != Adopted` no warp is applied (both revert and failure
/// emit the unaligned geometry), which is exactly what A-C3/A-C4 guard. A global warp stands in
/// for T4's per-block warp here; the five assertions are relative and unaffected.
fn pipeline(frames: &[Vec<f32>], w: usize, h: usize) -> Pipeline {
    let refs: Vec<&[f32]> = frames.iter().map(Vec::as_slice).collect();
    let unaligned = fuse(frames, w, h, 2);
    let proxies: Vec<_> = frames
        .iter()
        .map(|f| focus_align::proxy_luma(f, w, h))
        .collect();
    let mut fields = vec![BlockField::zeros()];
    for k in 1..frames.len() {
        fields.push(focus_align::block_shifts(&proxies[k - 1], &proxies[k], 1));
    }
    let offsets = focus_align::chain_offsets(&fields);
    let blocks = focus_align::chain_blocks(&fields);
    let failed = fields.iter().skip(1).map(|f| f.failed).max().unwrap_or(0);
    let (ma, mu) = focus_align::gain_stats(&refs, w, h, &offsets, 1);
    let crop = focus_align::auto_crop(&offsets, w as u32, h as u32);
    let input = DecideInput {
        mean_aligned: ma,
        mean_unaligned: mu,
        failed_blocks: failed,
        total_blocks: GRID_COLS * GRID_ROWS,
        offsets: offsets.clone(),
        blocks,
        crop: Some(crop),
    };
    let report = focus_align::decide(&input);
    let out = if report.status == AlignStatus::Adopted {
        let warped: Vec<Vec<f32>> = frames
            .iter()
            .enumerate()
            .map(|(i, f)| focus_align::warp_rgb(f, w, h, offsets[i]))
            .collect();
        fuse(&warped, w, h, 2)
    } else {
        unaligned.clone()
    };
    Pipeline {
        report,
        out,
        unaligned,
        offsets,
        fields,
    }
}

/// 8px 错位栈（白噪声）：帧 0 不动、帧 1 右移 8px、帧 2 上移 8px。
///
/// 8 px-offset stack on white noise: frame 0 still, frame 1 shifted right 8 px, frame 2
/// shifted up 8 px.
fn offset_stack() -> Vec<Vec<f32>> {
    let base = sharp_texture(W, H, 7, 0);
    vec![
        base.clone(),
        focus_align::warp_rgb(&base, W, H, [SHIFT, 0.0]),
        focus_align::warp_rgb(&base, W, H, [0.0, -SHIFT]),
    ]
}

/// 「干净栈」：只有 0.5px 级亚像素抖动，没有成块错位；1/8 代理上看不出收益。
///
/// "Clean" stack: only 0.5 px-class sub-pixel jitter, no block-scale misalignment; the 1/8
/// proxies see no gain at all.
fn clean_stack() -> Vec<Vec<f32>> {
    let base = texture(W, H, 13);
    vec![
        base.clone(),
        focus_align::warp_rgb(&base, W, H, [0.5, 0.0]),
        focus_align::warp_rgb(&base, W, H, [0.0, -0.5]),
    ]
}

/// 5 帧 1.00→1.02 渐进 breathing 焦点栈：每帧都是同一底图的一次中心缩放插值。
///
/// A 5-frame 1.00->1.02 progressive breathing focus stack: every frame is one centre-zoom
/// interpolation of the same base image.
fn breath_stack() -> Vec<Vec<f32>> {
    let base = texture(W, H, 23);
    (0..BREATH_FRAMES)
        .map(|i| {
            let factor = 1.0 + (BREATH_ZOOM - 1.0) * i as f64 / (BREATH_FRAMES - 1) as f64;
            zoom_center(&base, W, H, factor)
        })
        .collect()
}

/// 四角区合成测度：四角块各自外角 1/4 tile 区的测度均值再平均（拍板 ⑧ 冻结口径）。
///
/// Corner measure: average of the mean composite measure over the outer quarter of each of
/// the four corner tiles (the statistic frozen by ruling 8).
fn corner_measure(out: &[f32], w: usize, h: usize, radius: usize) -> f32 {
    let m = focus_align::measure_plane(out, w, h, radius);
    let tw = w / GRID_COLS;
    let th = h / GRID_ROWS;
    let qw = (tw / 2).max(1);
    let qh = (th / 2).max(1);
    let corners: [(usize, usize); 4] = [(0, 0), (w - qw, 0), (0, h - qh), (w - qw, h - qh)];
    let mut acc = 0.0f64;
    let mut cnt = 0u64;
    for (x0, y0) in corners {
        for y in y0..(y0 + qh).min(h) {
            for x in x0..(x0 + qw).min(w) {
                acc += m[y * w + x] as f64;
                cnt += 1;
            }
        }
    }
    if cnt == 0 {
        0.0
    } else {
        (acc / cnt as f64) as f32
    }
}

/// A-C1：已知整数位移误差 ≤0.5px；0.5px 步进亚像素误差 ≤0.2px。
///
/// 负向验证：拿掉 `align.rs::focus_phase_shift` 的抛物线亚像素，只剩整数峰，
/// 亚像素夹具误差跳到 ~0.5px，本条红。
///
/// A-C1: known integer shifts within 0.5 px; 0.5 px-step sub-pixel shifts within 0.2 px.
///
/// Negative check: remove the parabolic sub-pixel fit in `align.rs::focus_phase_shift` and
/// only the integer peak remains, so the sub-pixel error jumps to about 0.5 px.
#[test]
fn a_c1_known_integer_and_subpixel_shifts() {
    let (w, h) = (128usize, 128usize);
    let base = mono_texture(w, h, 3);
    let mut worst_int = 0.0f64;
    let mut worst_sub = 0.0f64;
    for d in [3.0f64, -5.0, 7.0] {
        let cur = warp_mono(&base, w, h, [d, 0.0]);
        let (est, _peak) = focus_align::phase_shift(&base, &cur, w, h);
        let err = (est[0] + d).abs();
        worst_int = worst_int.max(err);
        assert!(err <= 0.5, "整数位移 {d} 估计 {est:?}，误差 {err}");
        assert!(est[1].abs() <= 0.5, "整数位移 {d} 的 y 分量漂移 {}", est[1]);
    }
    for d in [0.5f64, 1.5, 2.5, 3.5] {
        let cur = warp_mono(&base, w, h, [d, 0.0]);
        let (est, _peak) = focus_align::phase_shift(&base, &cur, w, h);
        let err = (est[0] + d).abs();
        worst_sub = worst_sub.max(err);
        assert!(err <= 0.2, "亚像素位移 {d} 估计 {est:?}，误差 {err}");
    }
    eprintln!("A-C1 整数最大误差 = {worst_int} px，亚像素最大误差 = {worst_sub} px");
}

/// A-C2：8px 错位栈对齐后纹理测度 ≥1.3× 未对齐，且 ≥0.9× 干净基准；干净栈不得被采用。
///
/// 负向验证 a：把 `chain_offsets` 的代表位移清零 → 对齐等于没对齐，比值掉到 1.0。
/// 负向验证 b：把 `decide` 的收益门槛改成「永不回退」→ 干净栈被错误采用，本条与
/// A-C3 同时红（设计 §4 的 A-C2 负向）。
///
/// A-C2: on the 8 px-offset stack the aligned composite measure is at least 1.3x the
/// unaligned one and at least 0.9x the clean baseline; a clean stack must not be adopted.
///
/// Negative a: zero the representative shift in `chain_offsets` and the ratio drops to 1.0.
/// Negative b: turn `decide`'s gain threshold into "never revert" and the clean stack is
/// wrongly adopted, making this assertion and A-C3 red together (design section 4).
#[test]
fn a_c2_end_to_end_gain_on_a_shifted_stack() {
    let frames = offset_stack();
    let p = pipeline(&frames, W, H);
    let base = sharp_texture(W, H, 7, 0);
    let clean = fuse(&[base.clone(), base.clone(), base.clone()], W, H, 2);
    let m_un = measure(&p.unaligned, W, H, 2);
    let m_al = measure(&p.out, W, H, 2);
    let m_clean = measure(&clean, W, H, 2);
    eprintln!(
        "A-C2 未对齐 {m_un}，对齐 {m_al}，干净基准 {m_clean}，比值 {:.4}，回干净基准 {:.4}",
        m_al / m_un.max(1e-9),
        m_al / m_clean.max(1e-9)
    );
    assert!(m_al >= 1.3 * m_un, "对齐收益不足：{m_al} < 1.3×{m_un}");
    assert!(
        m_al >= 0.9 * m_clean,
        "对齐后没回到干净基准：{m_al} < 0.9×{m_clean}"
    );
    assert_eq!(p.report.status, AlignStatus::Adopted, "错位栈不该回退");
    assert!(p.report.gain >= focus_align::GAIN_MIN, "错位栈增益应达标");
    // 白噪声每帧都有充分纹理，块评分不该判失败；这条同时让 `fields` 不沦为死字段。
    //
    // White noise is textured in every block, so no block score may fail; this also keeps
    // `fields` from becoming dead code.
    assert!(
        p.fields.iter().all(|f| f.failed == 0),
        "错位栈不该有失败块：{:?}",
        p.fields.iter().map(|f| f.failed).collect::<Vec<_>>()
    );

    // 干净栈必须因收益不足回退；这条同时也是「永不回退」负向的红点。
    //
    // A clean stack must revert for insufficient gain; this is also the red point for the
    // "never revert" negative mutation.
    let pc = pipeline(&clean_stack(), W, H);
    assert_eq!(
        pc.report.status,
        AlignStatus::RevertedInsufficientGain,
        "干净栈被错误对齐（增益 {}）",
        pc.report.gain
    );
}

/// A-C3：干净栈 `align:true` 最终输出与 `align:false` 逐点一致；错位栈达标不回退。
///
/// 负向验证：去掉 `decide` 的回退分支（改成永不回退）→ 干净栈被套上估计位移，
/// 三态断言与逐点断言一起红。
///
/// A-C3: on a clean stack, `align:true` output equals `align:false` sample by sample; a
/// qualifying offset stack must not revert.
///
/// Negative check: remove `decide`'s revert branch ("never revert") and the clean stack gets
/// warped by estimated offsets, so both the status and the point-by-point assertions go red.
#[test]
fn a_c3_revert_does_not_touch_a_clean_stack() {
    let clean = clean_stack();
    let p = pipeline(&clean, W, H);
    let unaligned = fuse(&clean, W, H, 2);
    assert_eq!(
        p.report.status,
        AlignStatus::RevertedInsufficientGain,
        "干净栈应因收益不足回退"
    );
    assert!(
        p.report.gain < focus_align::GAIN_MIN,
        "干净栈增益 {} 不该达标",
        p.report.gain
    );
    assert_eq!(p.out.len(), unaligned.len(), "回退仍要出图，长度不能变");
    for (i, (a, b)) in p.out.iter().zip(unaligned.iter()).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "样本 {i} 被改动：{a} vs {b}");
    }
    // 估计位移非零，负向（永不回退）才会真的改动成片，而不是「本来就没位移」。
    //
    // The estimated offsets are non-zero, so the "never revert" negative really changes the
    // image instead of warping by nothing.
    assert!(
        p.offsets
            .iter()
            .any(|o| o[0].abs() > 1e-3 || o[1].abs() > 1e-3),
        "干净栈的夹具应给出非零估计位移，判据才有牙：{:?}",
        p.offsets
    );
    assert_eq!(
        pipeline(&offset_stack(), W, H).report.status,
        AlignStatus::Adopted,
        "错位栈达标时不该回退"
    );
}

/// A-C4：纯平场 → `Failed`、失败块全量、位移全零、仍出图且等于未对齐几何。
///
/// 负向验证：删掉 `block_shifts` 的失败块判据 → 平场不再报 `Failed`、
/// `failed_blocks` 归零，本条红。
///
/// A-C4: a pure flat field yields `Failed`, all blocks failed, zero offsets, and still an
/// image equal to the unaligned geometry.
///
/// Negative check: delete the failed-block criterion in `block_shifts` and the flat field no
/// longer reports `Failed` while `failed_blocks` drops to zero.
#[test]
fn a_c4_low_texture_failure_is_visible() {
    let flat = vec![0.4f32; W * H * 3];
    let frames = vec![flat.clone(), flat.clone(), flat.clone()];
    let p = pipeline(&frames, W, H);
    assert_eq!(p.report.status, AlignStatus::Failed, "平场不得报 Adopted");
    assert_eq!(
        p.report.failed_blocks,
        GRID_COLS * GRID_ROWS,
        "平场全部 48 块都该判失败"
    );
    assert!(
        p.report
            .offsets
            .iter()
            .all(|o| o[0].abs() < 1e-9 && o[1].abs() < 1e-9),
        "平场不得输出自信的错误位移：{:?}",
        p.report.offsets
    );
    let unaligned = fuse(&frames, W, H, 2);
    assert_eq!(p.out.len(), W * H * 3, "估计失败仍要出图");
    assert!(p.out.iter().all(|v| v.is_finite()), "成片不能有非有限值");
    for (i, (a, b)) in p.out.iter().zip(unaligned.iter()).enumerate() {
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "样本 {i} 偏离未对齐几何：{a} vs {b}"
        );
    }
}

/// A-C5：1.00→1.02 渐进 breathing，四角区合成测度 `网格版 ≥ 全局平移版 ×1.05`。
///
/// 网格版逐帧用 `chain_blocks` 的 48 块累计位移场逐块 warp；全局版逐帧用
/// `chain_offsets` 的单一代表位移填满 48 块，两版走**同一个** `warp_grid` 替身，
/// 唯一差别就是位移场是不是网格。负向验证：把 `chain_blocks` 退化成「每帧场全填
/// 代表值」（即 1×1 全局平移）→ 两版逐点相同，比值掉到 1.0，本条红。
///
/// A-C5: on the 1.00->1.02 progressive breathing fixture the corner composite measure of
/// the grid version is at least 1.05x the global-translation version.
///
/// The grid version warps each frame with the 48-block accumulated field from `chain_blocks`;
/// the global version fills all 48 blocks with the single representative shift from
/// `chain_offsets`. Both go through the **same** `warp_grid` substitution, so the field being
/// a grid is the only difference. Negative check: degenerate `chain_blocks` into "fill every
/// frame's field with the representative value" (a 1x1 global translation) and the two
/// versions become point-wise identical, dropping the ratio to 1.0.
#[test]
fn a_c5_breathing_grid_beats_global_translation() {
    let imgs = breath_stack();
    let nf = imgs.len();
    let proxies: Vec<_> = imgs
        .iter()
        .map(|f| focus_align::proxy_luma(f, W, H))
        .collect();
    let mut fields = vec![BlockField::zeros()];
    for k in 1..nf {
        fields.push(focus_align::block_shifts(&proxies[k - 1], &proxies[k], 1));
    }
    for (k, f) in fields.iter().enumerate().skip(1) {
        assert!(
            f.failed < GRID_COLS * GRID_ROWS / 4,
            "breathing 夹具第 {k} 对不该整片判失败：{}",
            f.failed
        );
    }
    let blocks = focus_align::chain_blocks(&fields);
    let offsets = focus_align::chain_offsets(&fields);
    // 位移场必须真的是网格（四角累计位移明显偏离单一代表值），负向锚才有牙。
    //
    // The field must really be a grid (corner accumulated shifts clearly differ from the
    // single representative value), otherwise the negative anchor has no teeth.
    let last = nf - 1;
    let corner = blocks[last][0];
    let rep = offsets[last];
    assert!(
        (corner[0] - rep[0]).abs() > 1.0 || (corner[1] - rep[1]).abs() > 1.0,
        "breathing 夹具没产生网格位移场：角块 {corner:?} vs 代表 {rep:?}"
    );

    let mut grid_frames = Vec::with_capacity(nf);
    let mut global_frames = Vec::with_capacity(nf);
    for k in 0..nf {
        grid_frames.push(warp_grid(&imgs[k], W, H, &blocks[k]));
        let flat = vec![offsets[k]; GRID_COLS * GRID_ROWS];
        global_frames.push(warp_grid(&imgs[k], W, H, &flat));
    }
    let gc = corner_measure(&fuse(&grid_frames, W, H, 2), W, H, 2);
    let gg = corner_measure(&fuse(&global_frames, W, H, 2), W, H, 2);
    eprintln!(
        "A-C5 四角测度 网格 {gc}，全局 {gg}，比值 {:.4}",
        gc / gg.max(1e-9)
    );
    assert!(
        gc >= gg * 1.05,
        "网格平移场没赢过全局平移：{gc} < 1.05×{gg}"
    );
    assert!(gc.is_finite() && gg > 0.0, "四角测度口径失效：{gc} / {gg}");
}

/// A-C2′：`decide` 的收益阈值与失败块比例**构造性**钉死，不靠夹具的偶然数值。
///
/// 为什么需要这条：A-C3 用的是「干净栈 gain = −0.0271」，它锚的是「回退分支还在」，
/// 而不是「阈值是 0.005」——把 `GAIN_MIN` 改成 `0.0` 这条依然绿，因为负增益本来就
/// 小于任何非负阈值。`decide` 是纯函数，直接喂构造好的统计量，阈值一改立刻红。
///
/// A-C2': pin `decide`'s gain threshold and failed-block ratio **constructively**, not via
/// whatever numbers a fixture happens to produce.
///
/// Why this is needed: A-C3 uses "clean stack gain = -0.0271", which anchors "the revert
/// branch still exists" rather than "the threshold is 0.005" -- setting `GAIN_MIN` to `0.0`
/// keeps it green, since a negative gain is below any non-negative threshold. `decide` is a
/// pure function; feeding it constructed statistics makes any threshold change go red.
#[test]
fn decide_pins_the_gain_floor_and_the_failure_ratio_by_construction() {
    use aevum_core::focus_align::{DecideInput, GAIN_MIN};

    // 契约钉：`GAIN_MIN` 的字面值必须还是 PRD ALN-2 的 0.5%。
    //
    // 这条**不是**行为判据——下面那几条才是。行为判据只能把阈值夹在 (0.003, 0.01] 这个
    // 区间里：0.004 也好 0.006 也好，行为判据全绿，只有契约本身变了才能红。而 PRD 里
    // 写的「收益不足 0.5% 就回退」是**对用户**的承诺，改它必须同时改判据、改文档、
    // 改面板 hover、改 MCP schema 描述，所以这里直接把字面值钉住，让改动者一次踩到全部。
    //
    // Contract pin: `GAIN_MIN`'s literal must still be PRD ALN-2's 0.5%.
    //
    // This is **not** a behaviour assertion -- the ones below are. Behaviour assertions only
    // bracket the threshold into (0.003, 0.01]: 0.004 and 0.006 both pass them, so only a
    // change to the contract itself can turn this red. And "revert when the gain is under
    // 0.5%" is a promise made **to users**, so changing it must change the tests, the docs,
    // the panel hover text and the MCP schema description in one go -- pinning the literal
    // here makes the next person trip over all of them at once.
    assert!(
        (GAIN_MIN - 0.005).abs() < 1e-9,
        "GAIN_MIN 是 PRD ALN-2 的 0.5% 契约，改它必须同时改判据与文档（现值 {GAIN_MIN}）"
    );

    let input = |mean_aligned: f32, mean_unaligned: f32, failed: usize| DecideInput {
        mean_aligned,
        mean_unaligned,
        failed_blocks: failed,
        total_blocks: GRID_COLS * GRID_ROWS,
        offsets: vec![[0.0, 0.0]],
        blocks: vec![],
        crop: None,
    };

    // 阈值下沿：(1.003 − 1.0) / 1.0 = +0.003 < 0.005 → 必须回退。
    //
    // Just below the floor: (1.003 - 1.0) / 1.0 = +0.003 < 0.005 -> must revert.
    let below = aevum_core::focus_align::decide(&input(1.003, 1.0, 0));
    eprintln!(
        "A-C2' 阈值下沿 gain={:.6} status={:?}",
        below.gain, below.status
    );
    assert!(
        (below.gain - 0.003).abs() < 1e-6,
        "增益该是 +0.003，实得 {}",
        below.gain
    );
    assert_eq!(
        below.status,
        AlignStatus::RevertedInsufficientGain,
        "gain 0.003 < GAIN_MIN({GAIN_MIN}) 必须回退"
    );

    // 阈值上沿：+0.01 ≥ 0.005 → 必须采用。
    //
    // Just above the floor: +0.01 >= 0.005 -> must adopt.
    let above = aevum_core::focus_align::decide(&input(1.01, 1.0, 0));
    eprintln!(
        "A-C2' 阈值上沿 gain={:.6} status={:?}",
        above.gain, above.status
    );
    assert!(
        (above.gain - 0.01).abs() < 1e-6,
        "增益该是 +0.01，实得 {}",
        above.gain
    );
    assert_eq!(
        above.status,
        AlignStatus::Adopted,
        "gain 0.01 ≥ GAIN_MIN({GAIN_MIN}) 必须采用"
    );

    // 负增益：−0.03 → 必须回退（这条把「阈值改成 0」钉死：0 时它会被误判采用）。
    //
    // Negative gain: -0.03 -> must revert (this pins "threshold set to 0": that would
    // wrongly adopt it).
    let negative = aevum_core::focus_align::decide(&input(0.97, 1.0, 0));
    eprintln!(
        "A-C2' 负增益 gain={:.6} status={:?}",
        negative.gain, negative.status
    );
    assert!(
        (negative.gain + 0.03).abs() < 1e-6,
        "增益该是 −0.03，实得 {}",
        negative.gain
    );
    assert_eq!(
        negative.status,
        AlignStatus::RevertedInsufficientGain,
        "负增益必须回退"
    );

    // 失败块比例：25% 不过线（严格大于才失败），27% 过线。
    //
    // Failed-block ratio: 25% does not cross (strictly greater fails), 27% does.
    let quarter = aevum_core::focus_align::decide(&input(1.01, 1.0, 12));
    assert_ne!(
        quarter.status,
        AlignStatus::Failed,
        "12/48 = 25% 不该判失败（严格大于才失败）"
    );
    let over_quarter = aevum_core::focus_align::decide(&input(1.01, 1.0, 13));
    eprintln!("A-C2' 失败块 13/48 status={:?}", over_quarter.status);
    assert_eq!(
        over_quarter.status,
        AlignStatus::Failed,
        "13/48 ≈ 27% > 25% 必须判失败"
    );

    // 失败块判定优先于收益判定：就算 gain 达标，块失败过多也是 Failed。
    //
    // The failure test outranks the gain test: even with a passing gain, too many failed
    // blocks means Failed.
    let both = aevum_core::focus_align::decide(&input(2.0, 1.0, 48));
    assert_eq!(both.status, AlignStatus::Failed, "全块失败必须判失败");
}
