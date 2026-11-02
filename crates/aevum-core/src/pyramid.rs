//! 金字塔合成：Burt–Adelson 拉普拉斯塔上的多尺度焦点融合。
//!
//! # 为什么是「多尺度的 Weighted」
//!
//! [`crate::focus::Method::Weighted`] 用**一个**权重面把整帧混一遍：权重在纹理处很陡，
//! 但低频（亮度底座、大面积渐变）也跟着同一个权重走，于是纹理边界外会出现反转光晕。
//! 金字塔把融合拆到不同尺度上：细层只决定「这一小块细节从哪帧来」，粗层只决定
//! 「底色是多少」，互不串扰。`W_l = GaussPyr(m^4)` 与 Weighted 用的是同一个测度、
//! 同一个四次方，所以 `levels = 1`（塔里只有一层、没有下采样）时必须**严格**退化成
//! Weighted——这既是实现自检锚，也是 `P-C1` 判据。
//!
//! # 为什么最粗两层等权
//!
//! 连续 α(l) 收紧曲线没有判据可锚、不可复现。这里改成分段常量：最粗的两层用
//! `1/count`，其余层用 `m^4` 归一。等权让最粗层退化成「全帧平均底图」，掐断低频层
//! 的高频扩散（光晕的主源），也让常数场塌缩天然成立。`levels < 3` 时只有一层能收紧，
//! `levels = 1` 时一层都不收紧（否则会破坏退化一致性）。
//!
//! # 内存与耗时（T10 实测，2026-11-03）
//!
//! 逐帧流式：**按层下采样 → 边算 L_l 边融进输出塔 → 只留相邻两层**，绝不把 32 帧
//! 的完整塔一起驻留。塌缩从最粗层起**原地**逐层上采样累加，不另开全分辨率缓冲。
//!
//! 实测口径：release 构建、24 MP（6000×4000）× 8 帧、`levels = auto = 6`、
//! 同一台机器（i5-12600KF / 31.8 GB RAM / Windows）各跑 3 次取中位数，峰值取进程
//! `PeakWorkingSetSize`。输入 8 帧本身驻留 2.32 GB（8 × 288 MB + 解码器余量），表里
//! 「融合增量」是峰值减去融合开始前的常驻，才是本法自己的开销。
//!
//! | 方法 | 墙钟 | 进程峰值 | 融合增量 |
//! |---|---|---|---|
//! | Weighted | 1.97 s | 3.19 GB | 864 MB |
//! | Pyramid | 29.7 s | 4.43 GB | **2.10 GB** |
//! | 倍数 | **15.0×** | 1.39× | **2.4×** |
//!
//! 早先按「输出塔 384 MB + 权重塔 128 MB」推算的 ≈1.0 GB **是错的**（差 2 倍多）：
//! 那个算式漏了 Pass A 的 `best` / `gate` 两个全分辨率面（各 96 MB）、掩膜塔（128 MB）、
//! 每帧的测度面（96 MB）与当前帧的拉普拉斯面（288 MB）。耗时 15× 而非「2–3×」，主要
//! 因为 `focus::measure_plane` 每帧跑**两遍**（Pass A 求分位线、Pass B 求权重）。
//! 这两个数是实测值，不是推算值；改实现就重测，别照着旧数字改注释。
//!
//! `feather` / `smooth_z` 在本法下**不适用**：融合只在塔各层上按权重归一，没有
//! 深度跳变、没有羽化掩膜，也没有模值滤波。调用方传了也只读校验、不生效。
//!
//! # Pyramid fusion
//!
//! Burt-Adelson Laplacian fusion on multiple scales.
//!
//! # Why it is "multi-scale Weighted"
//!
//! [`crate::focus::Method::Weighted`] blends the whole frame with **one** weight plane. The
//! weight is steep at texture, but low frequencies (the base tone, broad gradients) follow
//! that same weight, which produces inversion halos just outside texture boundaries. A
//! pyramid splits the blend across scales: fine levels decide which frame supplies a detail,
//! coarse levels decide the base tone, and neither leaks into the other. `W_l =
//! GaussPyr(m^4)` uses the very same measure and the very same fourth power as Weighted,
//! so `levels = 1` (one level, no downsampling) must **strictly** degenerate to Weighted --
//! both an implementation anchor and assertion `P-C1`.
//!
//! # Why the two coarsest levels use equal weights
//!
//! A continuous alpha(l) tightening curve has no assertion to anchor it and is not
//! reproducible. The constant-segment rule is: the two coarsest levels use `1/count`, all
//! other levels use normalised `m^4`. Equal weights turn the coarsest level into a
//! whole-frame average base, cutting the high-frequency spread at low frequencies (the
//! main source of halos) and making the constant-field collapse hold by construction. With
//! `levels < 3` only one level is tightened; with `levels = 1` none is, or the degeneration
//! would break.
//!
//! # Memory and time (measured for T10, 2026-11-03)
//!
//! Streaming, one frame at a time: reduce level by level, compute `L_l` and fold it into the
//! output tower immediately, keeping only two adjacent planes. The full towers of all 32
//! frames are never resident. Collapse runs **in place** from the coarsest level, with no
//! extra full-resolution buffer.
//!
//! Measured how: release build, 24 MP (6000x4000) x 8 frames, `levels = auto = 6`, one
//! machine (i5-12600KF / 31.8 GB RAM / Windows), three runs each, median; peak is the
//! process's `PeakWorkingSetSize`. The eight input frames alone stay resident at 2.32 GB
//! (8 x 288 MB plus decoder slack); the "fusion delta" column is the peak minus what was
//! already resident when fusion started, i.e. this method's own cost.
//!
//! | Method | Wall clock | Process peak | Fusion delta |
//! |---|---|---|---|
//! | Weighted | 1.97 s | 3.19 GB | 864 MB |
//! | Pyramid | 29.7 s | 4.43 GB | **2.10 GB** |
//! | Ratio | **15.0x** | 1.39x | **2.4x** |
//!
//! The earlier arithmetic ("output tower 384 MB + weight tower 128 MB") guessed about
//! 1.0 GB and **is wrong by more than 2x**: it forgot Pass A's two full-resolution `best` /
//! `gate` planes (96 MB each), the mask tower (128 MB), the per-frame measure plane (96 MB)
//! and the current frame's Laplacian (288 MB). The 15x wall clock (not "2-3x") is mostly
//! `focus::measure_plane` running **twice** per frame (Pass A for the percentile cut, Pass B
//! for the weights). These are measurements, not estimates: change the implementation and
//! re-measure -- do not edit the comment to match the old numbers.
//!
//! `feather` and `smooth_z` do **not apply** here: fusion is a per-level weighted
//! normalisation, with no depth jumps, no feather mask and no mode filter. Callers may pass
//! them; they are validated but not read.

use crate::focus::{self, Options};

/// 二项式 5-tap 核（下采样用）。和恒为 1，常数场经过 reduce 仍是常数场。
///
/// The binomial 5-tap kernel used for reduction; it sums to 1, so a constant field stays
/// constant through `reduce`.
const K5: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// 权重和低于这个数就认为「没有任何帧认领这一格」，退回参考帧。
///
/// A weight sum below this means no frame claims the pixel; fall back to the reference.
const WSUM_EPS: f32 = 1e-6;

/// 显式层数的硬上限：超过它一律夹紧，不许拿一张 48px 小图开 100 层。
///
/// Hard ceiling for an explicit level count; anything above it is clamped so a 48 px test
/// frame can never request a hundred levels.
pub const MAX_LEVELS: usize = 12;

/// 一层平面：自带尺寸与通道数，`data` 按 `y * w * channels + x * channels + c` 排布。
///
/// RGB 塔层是 3 通道，权重与掩膜塔层是 1 通道。尺寸随层走，不给上层留「第 l 层
/// 到底多大」的心算负担——下采样遇到奇数边长是 `ceil(w/2)`，不是 `w >> l`。
///
/// One plane of a tower: it carries its own size and channel count, with `data` laid out as
/// `y * w * channels + x * channels + c`. RGB levels have 3 channels; weight and mask
/// levels have 1. Keeping the size with the plane removes the need to recompute "how large
/// is level l" upstairs: reduction of an odd side is `ceil(w/2)`, not `w >> l`.
#[derive(Clone, Debug)]
pub struct Plane {
    /// 宽（像素）。
    ///
    /// Width in pixels.
    pub w: usize,
    /// 高（像素）。
    ///
    /// Height in pixels.
    pub h: usize,
    /// 通道数（RGB = 3，权重/掩膜 = 1）。
    ///
    /// Channel count (RGB = 3, weight/mask = 1).
    pub channels: usize,
    /// 交错存放的样本。
    ///
    /// Interleaved samples.
    pub data: Vec<f32>,
}

impl Plane {
    /// 全零平面。
    ///
    /// An all-zero plane.
    pub fn zeros(w: usize, h: usize, channels: usize) -> Self {
        Self {
            w,
            h,
            channels,
            data: vec![0.0f32; w * h * channels],
        }
    }

    /// 由一帧交错 RGB 拷出最细层。
    ///
    /// Copy the finest level out of an interleaved RGB frame.
    pub fn from_rgb(frame: &[f32], w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            channels: 3,
            data: frame.to_vec(),
        }
    }

    /// 由一张单通道平面拷出最细层。
    ///
    /// Copy the finest level out of a single-channel plane.
    pub fn from_mono(plane: &[f32], w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            channels: 1,
            data: plane.to_vec(),
        }
    }

    /// 第 `p` 个像素的第 `c` 通道在 `data` 里的起点。
    ///
    /// Start offset of channel `c` of pixel `p` inside `data`.
    #[inline]
    fn offset(&self, p: usize, c: usize) -> usize {
        p * self.channels + c
    }
}

/// 解析层数：`0 = auto`，显式值夹紧到 `1..=MAX_LEVELS`。
///
/// auto 口径是 `clamp(floor(log2(min(w,h)/32)), 3, 8)`：24 MP（6000×4000）得 6，
/// 48px 判据夹具落到下限 3。夹紧是**硬要求**：不夹紧时 48px 的 auto 会算出 0 层，
/// 输出塔为空，塌缩直接越界；显式 1000 层也会把小图一路降到 1×1 再反复空转。
///
/// Resolve the level count: `0 = auto`, explicit values clamp into `1..=MAX_LEVELS`.
///
/// Auto is `clamp(floor(log2(min(w,h)/32)), 3, 8)`: 24 MP (6000x4000) gives 6 and a 48 px
/// fixture hits the lower bound of 3. The clamp is a hard requirement: without it a 48 px
/// auto resolves to 0 levels, the output tower is empty and collapse indexes out of bounds;
/// an explicit 1000 would drive a small frame down to 1x1 and then spin on empty levels.
pub fn resolve_levels(w: usize, h: usize, requested: usize) -> usize {
    if requested != 0 {
        return requested.clamp(1, MAX_LEVELS);
    }
    let short = w.min(h).max(1) as f64;
    let raw = (short / 32.0).log2().floor();
    // log2 的参数恒 > 0；NaN 只可能来自 0×∞ 之类，这里不会出现，仍做一次兜底。
    //
    // The log2 argument is always > 0; NaN cannot arise here, but guard anyway.
    if !raw.is_finite() {
        return 3;
    }
    (raw as i64).clamp(3, 8) as usize
}

/// 每一层的尺寸，长度恰为 `levels`。
///
/// The size of every level, with exactly `levels` entries.
pub fn level_dims(w: usize, h: usize, levels: usize) -> Vec<(usize, usize)> {
    let mut dims = Vec::with_capacity(levels);
    let (mut cw, mut ch) = (w.max(1), h.max(1));
    for _ in 0..levels {
        dims.push((cw, ch));
        cw = cw.div_ceil(2);
        ch = ch.div_ceil(2);
    }
    dims
}

/// 5-tap 二项式下采样（逐轴可分离、边界夹边），尺寸取 `ceil(src/2)`。
///
/// 夹边时五个 tap 仍然全部参与（重复的只是样本坐标），核和恒为 1，所以**不需要**
/// 再归一也不会让边缘发暗；这是 P-C2「常数场逐点不变」的一半保证。
///
/// Separable binomial 5-tap reduction with clamped edges and `ceil(src/2)` output size.
///
/// All five taps still participate at a clamped edge (only the sample coordinate repeats),
/// so the kernel sum stays 1 with no renormalisation and no dark border; that is half of
/// `P-C2`'s "a constant field stays constant" guarantee.
pub fn reduce(src: &Plane) -> Plane {
    let dw = src.w.div_ceil(2);
    let dh = src.h.div_ceil(2);
    let mut out = Plane::zeros(dw, dh, src.channels);
    for y in 0..dh {
        let sy = (2 * y) as i32;
        let ys: Vec<usize> = (-2i32..=2)
            .map(|d| (sy + d).clamp(0, src.h as i32 - 1) as usize)
            .collect();
        for x in 0..dw {
            let sx = (2 * x) as i32;
            let xs: Vec<usize> = (-2i32..=2)
                .map(|d| (sx + d).clamp(0, src.w as i32 - 1) as usize)
                .collect();
            let dst = (y * dw + x) * src.channels;
            for c in 0..src.channels {
                let mut acc = 0.0f32;
                for (ky, &yy) in ys.iter().enumerate() {
                    for (kx, &xx) in xs.iter().enumerate() {
                        acc += K5[ky] * K5[kx] * src.data[(yy * src.w + xx) * src.channels + c];
                    }
                }
                out.data[dst + c] = acc;
            }
        }
    }
    out
}

/// 掩膜塔用的下采样：2×2 取最大，不做平滑。
///
/// 0/1 的「无细节」标记若被高斯抹成 0.25，粗层就会拿等权把参考帧混没；取最大能
/// 让「这一格里只要有一个弱像素，整格都算弱」——保守地退回参考帧，不造。
///
/// Reduction for the mask tower: a 2x2 max, no smoothing.
///
/// If a 0/1 "no detail" flag were Gaussian-blurred into 0.25, the coarse levels would let
/// equal weights wash the reference frame away. A max keeps "one weak pixel in this cell
/// makes the whole cell weak": conservatively fall back instead of inventing.
fn reduce_mask(src: &[f32], sw: usize, sh: usize) -> (Vec<f32>, usize, usize) {
    let dw = sw.div_ceil(2);
    let dh = sh.div_ceil(2);
    let mut out = vec![0.0f32; dw * dh];
    for y in 0..dh {
        for x in 0..dw {
            let mut best = 0.0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let yy = (2 * y + dy).min(sh - 1);
                    let xx = (2 * x + dx).min(sw - 1);
                    best = best.max(src[yy * sw + xx]);
                }
            }
            out[y * dw + x] = best;
        }
    }
    (out, dw, dh)
}

/// 0/1 门限塔：第 0 层就是原样，以后每层 2×2 取最大。
///
/// A 0/1 gate pyramid: level 0 is the input itself, later levels are 2x2 maxima.
fn mask_pyramid(gate: &[f32], w: usize, h: usize, levels: usize) -> Vec<Vec<f32>> {
    let mut tower = Vec::with_capacity(levels);
    let (mut cur, mut cw, mut ch) = (gate.to_vec(), w, h);
    for _ in 0..levels {
        tower.push(cur.clone());
        let next = reduce_mask(&cur, cw, ch);
        cur = next.0;
        cw = next.1;
        ch = next.2;
    }
    tower
}

/// 上采样的一维 tap：把目标坐标 `x` 映回源坐标 `x/2`。
///
/// 偶数目标点用 `[1,4,6,4,1]/16`；奇数（半像素）点用相邻两个整数相核的平均
/// `[0.5,2.5,5,5,2.5,0.5]/16`。两者和都是 1，边界夹边后再显式归一一次——
/// 这就是 P-C2 负向验证里「去掉归一就会漂移」的那一处。
///
/// One-dimensional interpolation taps: target coordinate `x` maps back to `x/2` in the
/// source. Even targets use `[1,4,6,4,1]/16`; odd (half-pixel) targets use the average of
/// two adjacent integer kernels, `[0.5,2.5,5,5,2.5,0.5]/16`. Both sum to 1, and we
/// normalise once more after clamping -- the exact spot whose removal makes `P-C2` drift.
fn taps(dst_len: usize, src_len: usize) -> Vec<Vec<(usize, f32)>> {
    let even = [1.0f32, 4.0, 6.0, 4.0, 1.0];
    let odd = [0.5f32, 2.5, 5.0, 5.0, 2.5, 0.5];
    let mut all = Vec::with_capacity(dst_len);
    for x in 0..dst_len {
        let base = (x / 2) as i32;
        let raw: &[f32] = if x % 2 == 0 { &even } else { &odd };
        let start = base - 2;
        // 先按 5-tap / 半像素核的原始系数取 tap（和为 16），夹边之后**统一归一一次**：
        // 这一步是常数场不变的唯一保证，也是 P-C2 负向验证要拿掉的那一行。
        //
        // Collect taps with the raw 5-tap / half-pixel coefficients (sum 16), then
        // normalise **once** after clamping. This is the sole guarantee that a constant
        // field stays constant, and the exact line P-C2's negative check removes.
        let list: Vec<(usize, f32)> = raw
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                let idx = (start + k as i32).clamp(0, src_len as i32 - 1) as usize;
                (idx, v)
            })
            .collect();
        let sum: f32 = list.iter().map(|t| t.1).sum();
        all.push(list.into_iter().map(|(idx, v)| (idx, v / sum)).collect());
    }
    all
}

/// 把 `src` 上采样到 `(tw, th)`；核归一，常数场不变。
///
/// Upsample `src` to `(tw, th)` with a normalised kernel; a constant field is preserved.
pub fn upsample(src: &Plane, tw: usize, th: usize) -> Plane {
    let mut out = Plane::zeros(tw, th, src.channels);
    accumulate_upsample(src, &mut out);
    out
}

/// 把 `src` 上采样后**原地累加**进 `dst`（`dst` 的尺寸就是目标尺寸）。
///
/// 塌缩用的就是这条：不另开一张全分辨率缓冲，直接把粗层的插值结果加进细层。
///
/// Upsample `src` and accumulate it **in place** into `dst` (`dst` carries the target
/// size). Collapse uses exactly this: no extra full-resolution buffer, just add the coarse
/// interpolation into the finer level.
pub fn accumulate_upsample(src: &Plane, dst: &mut Plane) {
    let xt = taps(dst.w, src.w);
    let yt = taps(dst.h, src.h);
    for (y, ytaps) in yt.iter().enumerate() {
        for (x, xtaps) in xt.iter().enumerate() {
            let d = (y * dst.w + x) * dst.channels;
            for c in 0..dst.channels.min(src.channels) {
                let mut acc = 0.0f32;
                for &(sy, wy) in ytaps {
                    for &(sx, wx) in xtaps {
                        acc += wy * wx * src.data[(sy * src.w + sx) * src.channels + c];
                    }
                }
                dst.data[d + c] += acc;
            }
        }
    }
}

/// 由一帧交错 RGB 建高斯塔：`G_0` 是原帧，之后逐级 `reduce`，长度恰为 `levels`。
///
/// `fuse_pyramid` 自己不用它（那边逐帧流式、只留相邻两层），这里是给库调用方与
/// 判据用的**整塔视图**：常数场塌缩、逐层尺寸、构造自己的融合塔都能从这一个入口验。
///
/// Build a Gaussian tower from one interleaved RGB frame: `G_0` is the frame itself and
/// each later level is a `reduce`, with exactly `levels` entries.
///
/// `fuse_pyramid` does not use it (it streams frames, keeping only two adjacent planes);
/// this is the **whole-tower view** for library callers and assertions -- constant-field
/// collapse, per-level sizes and building a custom fusion tower all enter here.
pub fn gaussian_pyramid(frame: &[f32], w: usize, h: usize, levels: usize) -> Vec<Plane> {
    let mut cur = Plane::from_rgb(frame, w, h);
    let mut tower = Vec::with_capacity(levels);
    for l in 0..levels {
        if l + 1 == levels {
            tower.push(cur);
            break;
        }
        tower.push(cur.clone());
        cur = reduce(&cur);
    }
    tower
}

/// `L_l = G_l − upsample(G_{l+1})`。两个参数就是「相邻的那一对」，不给整塔。
///
/// `L_l = G_l - upsample(G_{l+1})`. The two arguments are exactly the adjacent pair; no
/// whole tower is needed.
pub fn laplacian_from_gaussian(fine: &Plane, coarse: &Plane) -> Plane {
    let up = upsample(coarse, fine.w, fine.h);
    let mut out = fine.clone();
    for (o, u) in out.data.iter_mut().zip(up.data.iter()) {
        *o -= u;
    }
    out
}

/// 从最粗层起原地逐层上采样累加，返回最细层的数据（塔内不再保留副本）。
///
/// Collapse from the coarsest level in place by accumulating upsampled values downwards,
/// then return the finest level's data without keeping another copy in the tower.
pub fn collapse_in_place(tower: &mut [Plane]) -> Vec<f32> {
    if tower.is_empty() {
        return Vec::new();
    }
    for l in (0..tower.len() - 1).rev() {
        let (lo, hi) = tower.split_at_mut(l + 1);
        accumulate_upsample(&hi[0], &mut lo[l]);
    }
    std::mem::take(&mut tower[0].data)
}

/// 4096 桶直方图求「权重下限分位」，口径与 `compute_owner_map` 逐字一致。
///
/// The 4096-bin histogram for the lower weight percentile, identical in convention to
/// `compute_owner_map`.
fn percentile_cut(best: &[f32], t: f32) -> Option<f32> {
    if t <= 0.0 || !t.is_finite() || best.is_empty() {
        return None;
    }
    let max = best.iter().cloned().fold(0.0f32, f32::max);
    if max <= 0.0 {
        return None;
    }
    let bins = 4096usize;
    let mut hist = vec![0u64; bins];
    let scale = (bins - 1) as f32 / max;
    for &b in best {
        hist[((b * scale) as usize).min(bins - 1)] += 1;
    }
    let want = ((t / 100.0) as f64 * best.len() as f64).floor() as u64;
    let mut acc = 0u64;
    let mut cut_bin = bins - 1;
    for (i, c) in hist.iter().enumerate() {
        acc += c;
        if acc >= want.max(1) {
            cut_bin = i;
            break;
        }
    }
    Some(cut_bin as f32 / scale)
}

/// 单尺度路径：`levels = 1` 时塔里只有 `G_0`，融合就是 Weighted 的逐点加权。
///
/// 这里**特意另写一遍**而不去调 Weighted：左边是「被测的实现」，右边是「已知正确的
/// 参照」，两边必须互相独立才有判据价值（把 `m^4` 改成 `m^2` 时 P-C1 要能红）。
/// 运算顺序与 Weighted 完全一致，所以逐点偏差是 0，不是「约等于」。
///
/// The single-scale path: at `levels = 1` the tower holds only `G_0`, so fusion is exactly
/// Weighted's per-pixel blend.
///
/// This is deliberately a second implementation rather than a call into Weighted: the
/// left side is the code under test and the right side the known-good reference, and they
/// must stay independent for the assertion to mean anything (change `m^4` to `m^2` and
/// `P-C1` must go red). The operation order matches Weighted exactly, so the per-pixel
/// difference is 0 rather than "about equal".
fn fuse_single_scale(frames: &[&[f32]], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let n = w * h;
    let mut out = vec![0.0f32; n * 3];
    let mut wsum = vec![0.0f32; n];
    for f in frames {
        let m = focus::measure_plane(f, w, h, radius);
        for p in 0..n {
            let mm = m[p];
            let wt = mm * mm * mm * mm;
            if wt > 0.0 {
                let j = p * 3;
                out[j] += wt * f[j];
                out[j + 1] += wt * f[j + 1];
                out[j + 2] += wt * f[j + 2];
                wsum[p] += wt;
            }
        }
    }
    let rf = frames[frames.len() / 2];
    for p in 0..n {
        let j = p * 3;
        if wsum[p] > 0.0 {
            out[j] /= wsum[p];
            out[j + 1] /= wsum[p];
            out[j + 2] /= wsum[p];
        } else {
            out[j] = rf[j];
            out[j + 1] = rf[j + 1];
            out[j + 2] = rf[j + 2];
        }
    }
    out
}

/// 把一帧在某一层的贡献按权重累进输出塔和权重和塔。
///
/// Accumulate one frame's contribution at one level into the output and weight-sum towers.
fn accumulate_level(out: &mut Plane, wsum: &mut [f32], plane: &Plane, weight: &[f32]) {
    let n = out.w * out.h;
    for p in 0..n {
        let wt = weight[p];
        if wt > 0.0 {
            for c in 0..3 {
                let o = out.offset(p, c);
                out.data[o] += wt * plane.data[o];
            }
            wsum[p] += wt;
        }
    }
}

/// 逐帧流式融合，返回线性 RGB `f32`（`w*h*3`）。
///
/// 输入契约与 [`crate::focus::focus_stack`] 相同（已对齐、逐帧 `w*h*3`）；调用方
/// 负责校验尺寸与帧数。`threshold` 在这里是**权重下限分位**：分位线以下的像素在
/// 所有层都不参与融合，整块退回参考帧（序列中位帧）——注意这不只影响最细层，
/// 掩膜塔会把它带到每一层，否则粗层的等权会把参考帧又混掉。
///
/// Stream the fusion frame by frame and return linear RGB `f32` (`w*h*3`).
///
/// The input contract matches [`crate::focus::focus_stack`] (already aligned, `w*h*3` per
/// frame); the caller validates sizes and frame count. Here `threshold` is the **lower
/// weight percentile**: pixels below the cut contribute nothing at any level and fall back
/// wholesale to the reference frame (the sequence's middle frame). This reaches every level
/// through the mask tower -- otherwise equal weights at coarse levels would blend the
/// reference frame away again.
pub fn fuse_pyramid(frames: &[&[f32]], w: usize, h: usize, options: &Options) -> Vec<f32> {
    let levels = resolve_levels(w, h, options.levels);
    let count = frames.len();
    let reference = count / 2;
    let radius = options.radius;
    if levels == 1 {
        // 没有多尺度结构，threshold 的权重分位兜底也就没有意义；退化要求覆盖全部参数。
        //
        // With no multi-scale structure the percentile gate has nothing to act on; the
        // degeneration must hold for every parameter set.
        return fuse_single_scale(frames, w, h, radius);
    }

    let t = if options.threshold.is_nan() {
        0.0
    } else {
        options.threshold.clamp(0.0, 100.0)
    };
    // Pass A：只留一个 best 面，既求分位线也认「全帧无细节」，不驻留任何测度塔。
    // `threshold=0` 时下限就是 0，于是门限等价于「这一格有没有任何纹理」——这与
    // 深度图法的「0 = 关闭兜底」不同，金字塔里 0 表示「分位线取在零」。
    //
    // Pass A: keep one "best" plane, used for both the percentile cut and the
    // "no frame has detail here" test; never a tower of measures. With `threshold=0` the
    // cut sits at zero, so the gate becomes "is there any texture in this cell" -- unlike
    // the depth map's "0 = fallback off", here 0 means "put the percentile cut at zero".
    let mut best = vec![0.0f32; w * h];
    for f in frames {
        let m = focus::measure_plane(f, w, h, radius);
        for p in 0..w * h {
            let mm = m[p];
            best[p] = best[p].max(mm * mm * mm * mm);
        }
    }
    let cut = percentile_cut(&best, t).unwrap_or(0.0);
    let gate: Vec<f32> = best
        .iter()
        .map(|&b| if b > cut { 1.0 } else { 0.0 })
        .collect();
    let weak: Vec<bool> = gate.iter().map(|&g| g <= 0.0).collect();
    let masks = mask_pyramid(&gate, w, h, levels);
    let dims = level_dims(w, h, levels);
    let mut out_tower: Vec<Plane> = dims
        .iter()
        .map(|&(dw, dh)| Plane::zeros(dw, dh, 3))
        .collect();
    let mut wsum_tower: Vec<Vec<f32>> =
        dims.iter().map(|&(dw, dh)| vec![0.0f32; dw * dh]).collect();
    // 最粗两层等权；`max(1)` 保证 levels=1 的退化路径不会误收紧，levels=2 只收紧一层。
    //
    // Equal weights on the two coarsest levels; `max(1)` keeps the levels=1 degeneration
    // untouched and tightens exactly one level when levels=2.
    let tight_from = (levels - 2).max(1);

    // Pass B：逐帧流式；每帧只保留相邻两层的高斯面与权重面。
    //
    // Pass B: stream frame by frame, keeping only two adjacent Gaussian and weight planes.
    for f in frames {
        let m = focus::measure_plane(f, w, h, radius);
        let mut w0 = Vec::with_capacity(w * h);
        for p in 0..w * h {
            let mm = m[p];
            w0.push(if gate[p] > 0.0 {
                mm * mm * mm * mm
            } else {
                0.0
            });
        }
        let mut g = Plane::from_rgb(f, w, h);
        let mut wplane = Plane::from_mono(&w0, w, h);
        for l in 0..levels - 1 {
            let g_next = reduce(&g);
            let w_next = reduce(&wplane);
            let lap = laplacian_from_gaussian(&g, &g_next);
            let (dw, dh) = dims[l];
            let n = dw * dh;
            let weight: Vec<f32> = if l >= tight_from {
                let inv = 1.0 / count as f32;
                (0..n).map(|p| masks[l][p] * inv).collect()
            } else {
                wplane.data.clone()
            };
            accumulate_level(&mut out_tower[l], &mut wsum_tower[l], &lap, &weight);
            g = g_next;
            wplane = w_next;
        }
        // 最粗层留 G_L，永远是等权（levels-1 >= tight_from）。
        //
        // The coarsest level keeps G_L and is always equal-weight (levels-1 >= tight_from).
        let (dw, dh) = dims[levels - 1];
        let n = dw * dh;
        let inv = 1.0 / count as f32;
        let weight: Vec<f32> = (0..n).map(|p| masks[levels - 1][p] * inv).collect();
        accumulate_level(
            &mut out_tower[levels - 1],
            &mut wsum_tower[levels - 1],
            &g,
            &weight,
        );
    }

    // 权重和近零的格子：由参考帧自己的拉普拉斯塔补上，仍然只驻留相邻两层。
    //
    // For cells whose weight sum is near zero, fill from the reference frame's own
    // Laplacian tower -- still only two adjacent planes resident.
    let needs_fallback = wsum_tower.iter().any(|ws| ws.iter().any(|&v| v < WSUM_EPS));
    if needs_fallback {
        let mut g = Plane::from_rgb(frames[reference], w, h);
        for l in 0..levels - 1 {
            let g_next = reduce(&g);
            let lap = laplacian_from_gaussian(&g, &g_next);
            for p in 0..lap.w * lap.h {
                if wsum_tower[l][p] < WSUM_EPS {
                    for c in 0..3 {
                        let o = out_tower[l].offset(p, c);
                        out_tower[l].data[o] = lap.data[o];
                    }
                }
            }
            g = g_next;
        }
        for p in 0..g.w * g.h {
            if wsum_tower[levels - 1][p] < WSUM_EPS {
                for c in 0..3 {
                    let o = out_tower[levels - 1].offset(p, c);
                    out_tower[levels - 1].data[o] = g.data[o];
                }
            }
        }
    }
    for l in 0..levels {
        let n = dims[l].0 * dims[l].1;
        for p in 0..n {
            let ws = wsum_tower[l][p];
            if ws >= WSUM_EPS {
                for c in 0..3 {
                    let o = out_tower[l].offset(p, c);
                    out_tower[l].data[o] /= ws;
                }
            }
        }
    }
    let mut out = collapse_in_place(&mut out_tower);
    // 无细节区（以及被分位线割掉的区）整块退回参考帧。塌缩是线性的，但各层的
    // 兜底范围不可能逐像素对齐；若只在细层兜底、粗层照旧等权混，就会在暗纹边界
    // 外留下一圈亮晕（P-C5 要掐掉的正是它）。在塌缩**之后**按最细层的门限整块
    // 覆盖，才是「这一格用参考帧」的准确含义，也不会把 NaN 带进成片。
    //
    // Featureless areas (and everything below the percentile cut) fall back wholesale to
    // the reference frame. Collapse is linear, but per-level fallback regions cannot align
    // pixel for pixel; falling back only at the fine level while coarse levels still blend
    // leaves a bright ring outside a dark edge -- exactly what P-C5 kills. Overwriting whole
    // pixels **after** collapse, using the finest-level gate, is the precise meaning of
    // "use the reference frame here", and keeps NaN out of the result.
    let rf = frames[reference];
    for p in 0..w * h {
        if weak[p] {
            let j = p * 3;
            out[j] = rf[j];
            out[j + 1] = rf[j + 1];
            out[j + 2] = rf[j + 2];
        }
    }
    out
}
