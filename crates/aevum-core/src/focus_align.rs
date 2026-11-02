//! 焦点专用自动对齐：邻居链式估计 + 8×6 网格平移场 + 收益校验与三态决策。
//!
//! # 为什么不裸复用 `align::measure`
//!
//! `align::measure` 是给「序列去抖」写的：底部 30% 模板、8bit CLAHE 的 NCC、
//! 全局平移、`min_score = 0.35`。焦点栈里三处都不合适——模板只看画面下沿、
//! 失焦帧本来就该糊（NCC 会把「糊」误判成「没对准」）、focus breathing 是局部
//! 放大率变化（一个全局平移表根本表达不了）。这里另起一条链路：1/2 luma 代理上
//! 逐帧**邻居链式**（只需同时驻留相邻两帧），每帧把全幅切成 8×6 块各自估平移，
//! 块评分用**焦点测度比**而不是 NCC，最后用 1/8 RGB 代理上的相对测度增益决定
//! 「采用 / 收益不足回退 / 估计失败」。
//!
//! # 三态与「不叠加」
//!
//! `decide` 是纯函数：给两份测度统计与失败块数，就给出 `FocusAlignReport`。
//! 失败块占比 >25% → `Failed`；否则相对增益 `< 0.005` → `RevertedInsufficientGain`；
//! 其余 `Adopted`。`align:false` 的路径不经过本模块，T4 接线时 `align:true` 的
//! offset 完全由本模块给出，手动 `alignment_at()` 的 offset 一律不用（拍板 ⑥）。
//! 逐块 warp 与 24px 接缝淡化是 T4 的事，本模块只产出位移场与决策。
//!
//! # 内存口径
//!
//! 估计常驻 2 张 1/2 单通道 luma 代理（长边 ≤2048）≈ 48 MB；收益校验用 1/8 RGB
//! 代理（长边 ≤800），对齐/未对齐两份共 ≈ 288 MB。合计 ≈ 0.34 GB，且只在
//! `align:true` 时发生。数字是量级推算，T10 用实测替换。
//!
//! # Focus-specific auto alignment
//!
//! Neighbour-chained estimation on half-size luma proxies, an 8x6 grid of per-block phase
//! shifts, and a gain check that decides between adopted / reverted / failed.
//!
//! # Why `align::measure` is not reused as-is
//!
//! `align::measure` was written for sequence stabilisation: a bottom 30% template, 8-bit
//! CLAHE NCC, a single global translation and `min_score = 0.35`. All three choices are
//! wrong for a focus stack -- the template only sees the lower frame, a defocused frame is
//! *supposed* to be soft (NCC reads "soft" as "misaligned"), and focus breathing is a local
//! scale change that one global offset cannot express. This module instead chains neighbour
//! pairs on 1/2 luma proxies (only two frames resident), estimates a translation on each of
//! 8x6 blocks, scores blocks by a **focus-measure ratio** rather than NCC, and uses the
//! relative measure gain on 1/8 RGB proxies to decide.
//!
//! # Three states and "no stacking"
//!
//! `decide` is pure: two measure statistics plus a failed-block count produce a
//! `FocusAlignReport`. More than 25% failed blocks means `Failed`; otherwise a relative gain
//! below 0.005 means `RevertedInsufficientGain`; anything else is `Adopted`. The `align:false`
//! path never enters this module; when T4 wires `align:true`, offsets come solely from here
//! and manual `alignment_at()` offsets are ignored (ruling 6). Per-block warping and the 24 px
//! seam feather belong to T4 -- this module only produces the shift field and the decision.
//!
//! # Memory
//!
//! Estimation keeps two 1/2 single-channel luma proxies (long side <= 2048), about 48 MB;
//! the gain check uses 1/8 RGB proxies (long side <= 800), two copies about 288 MB. About
//! 0.34 GB in total, and only when `align:true`. These are order-of-magnitude estimates;
//! T10 replaces them with measurements.

use crate::align::{self, CropRect};
use crate::focus::{self, Method, Options};

/// 网格列数：全幅切 8 列。
///
/// Number of grid columns: the full frame is split into 8 columns.
pub const GRID_COLS: usize = 8;
/// 网格行数：全幅切 6 行，与列数相乘得 48 块。
///
/// Number of grid rows: 6 rows times the columns give 48 blocks.
pub const GRID_ROWS: usize = 6;
/// 块评分下限：低于它记失败块。
///
/// Lower block score threshold: below it the block counts as failed.
pub const BLOCK_MIN_SCORE: f32 = 0.2;
/// 失败块占比超过这个值就整体判估计失败（严格大于 25%）。
///
/// Above this failed-block ratio the whole estimation is a failure (strictly greater than 25%).
pub const FAIL_RATIO: f32 = 0.25;
/// 相对增益下限：低于 0.005（0.5%）回退未对齐。
///
/// Lower bound on relative gain: below 0.005 (0.5%) fall back to the unaligned geometry.
pub const GAIN_MIN: f32 = 0.005;
/// 1/2 luma 代理的长边上限（px）。
///
/// Long-side cap (px) of the half-size luma proxy.
pub const PROXY_LUMA_MAX_SIDE: usize = 2048;
/// 1/8 RGB 代理的长边上限（px）。
///
/// Long-side cap (px) of the eighth-size RGB proxy.
pub const PROXY_RGB_MAX_SIDE: usize = 800;
/// 「已采用」的可翻译模板键。
///
/// Translatable template key for "adopted".
pub const REASON_ADOPTED: &str = "已采用";
/// 「收益不足已回退」的可翻译模板键。
///
/// Translatable template key for "reverted: gain too small".
pub const REASON_REVERTED: &str = "收益不足已回退";
/// 「估计失败」的可翻译模板键。
///
/// Translatable template key for "estimation failed".
pub const REASON_FAILED: &str = "估计失败";

/// 对齐结果的三态。
///
/// Three possible outcomes of focus alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignStatus {
    Adopted,
    RevertedInsufficientGain,
    Failed,
}

impl AlignStatus {
    /// 稳定的机器键，命令面与结果 JSON 用它（**不要**手写字面量，改枚举时这里编译报错）。
    ///
    /// 与 [`crate::focus_align::REASON_ADOPTED`] 的中文模板键分开：那些是给人看的译文键，
    /// 这些是给脚本与 MCP 客户端判读的短串。MCP 侧没有 i18n 漏斗，把中文模板键原样
    /// 透传出去英文用户必见中文，所以对外一律走这一组。
    ///
    /// Stable machine key, used by the command surface and the result JSON (**never** hand
    /// write these literals -- changing the enum should break compilation here).
    ///
    /// Kept apart from the Chinese template keys ([`crate::focus_align::REASON_ADOPTED`]):
    /// those are translation keys for humans, these are short strings for scripts and MCP
    /// clients. MCP has no i18n funnel, so leaking a Chinese template key there would show
    /// Chinese to English users; everything outward-facing goes through this set.
    pub fn key(self) -> &'static str {
        match self {
            Self::Adopted => "adopted",
            Self::RevertedInsufficientGain => "reverted_insufficient_gain",
            Self::Failed => "failed",
        }
    }
}

/// 一次焦点对齐的完整报告，供接线层出图与面板显示。
///
/// Full report of one focus-alignment pass, consumed by the wiring layer and the panel.
#[derive(Clone, Debug)]
pub struct FocusAlignReport {
    /// 三态决策。
    ///
    /// The three-state decision.
    pub status: AlignStatus,
    /// 相对增益（0.012 = +1.2%）。
    ///
    /// Relative gain (0.012 means +1.2%).
    pub gain: f32,
    /// 逐帧全局位移，全分辨率像素，供 `common_crop` 与面板显示。
    ///
    /// Per-frame global warp offsets in full-resolution pixels, for `common_crop` and the UI.
    pub offsets: Vec<[f64; 2]>,
    /// 逐帧 8×6 网格位移场，全分辨率像素。
    ///
    /// Per-frame 8x6 grid shift field in full-resolution pixels.
    pub blocks: Vec<Vec<[f64; 2]>>,
    /// 自动裁切（无手动 report 时给出）。
    ///
    /// Automatic crop, supplied when no manual alignment report exists.
    pub crop: Option<CropRect>,
    /// 单个邻居对里失败块的最大数量。
    ///
    /// Largest failed-block count among the neighbour pairs.
    pub failed_blocks: usize,
    /// 可翻译模板键：已采用 / 收益不足已回退 / 估计失败。
    ///
    /// Translatable template key: adopted / reverted / failed.
    pub reason: &'static str,
}

/// 1/2 分辨率单通道 luma 代理。
///
/// Half-resolution single-channel luma proxy.
#[derive(Clone, Debug)]
pub struct MonoProxy {
    /// 代理宽。
    ///
    /// Proxy width.
    pub w: usize,
    /// 代理高。
    ///
    /// Proxy height.
    pub h: usize,
    /// 原图 / 代理 的倍率，用来把代理像素位移换回全分辨率。
    ///
    /// Source-to-proxy ratio, used to convert proxy-pixel shifts back to full resolution.
    pub scale: f64,
    /// 线性 luma，逐行存放。
    ///
    /// Linear luma in row-major order.
    pub data: Vec<f32>,
}

/// 1/8 分辨率 RGB 代理。
///
/// Eighth-resolution RGB proxy.
#[derive(Clone, Debug)]
pub struct RgbProxy {
    /// 代理宽。
    ///
    /// Proxy width.
    pub w: usize,
    /// 代理高。
    ///
    /// Proxy height.
    pub h: usize,
    /// 原图 / 代理 的倍率。
    ///
    /// Source-to-proxy ratio.
    pub scale: f64,
    /// 交错 RGB。
    ///
    /// Interleaved RGB.
    pub data: Vec<f32>,
}

/// 一个邻居对的 8×6 位移场与验收结果。
///
/// The 8x6 shift field and acceptance result for one neighbour pair.
#[derive(Clone, Debug)]
pub struct BlockField {
    /// 48 个块的位移，全分辨率像素；失败块为 `[0,0]`。
    ///
    /// Shifts of the 48 blocks in full-resolution pixels; failed blocks hold `[0,0]`.
    pub shifts: Vec<[f64; 2]>,
    /// 48 个块的焦点测度比评分。
    ///
    /// Focus-measure ratio score of each of the 48 blocks.
    pub scores: Vec<f32>,
    /// 失败块数量。
    ///
    /// Number of failed blocks.
    pub failed: usize,
}

impl BlockField {
    /// 全零位移场（参考帧 / 退化输入用）。
    ///
    /// An all-zero shift field, used for the reference frame and degenerate inputs.
    pub fn zeros() -> Self {
        let n = GRID_COLS * GRID_ROWS;
        Self {
            shifts: vec![[0.0f64, 0.0f64]; n],
            scores: vec![0.0f32; n],
            failed: 0,
        }
    }
}

/// 决策输入：两份测度统计、失败块数与几何。
///
/// Inputs to `decide`: two measure statistics, a failed-block count and the geometry.
#[derive(Clone, Debug)]
pub struct DecideInput {
    /// 对齐后合成的平均焦点测度。
    ///
    /// Mean focus measure of the aligned composite.
    pub mean_aligned: f32,
    /// 未对齐合成的平均焦点测度。
    ///
    /// Mean focus measure of the unaligned composite.
    pub mean_unaligned: f32,
    /// 单个邻居对里失败块的最大数量。
    ///
    /// Largest failed-block count among the neighbour pairs.
    pub failed_blocks: usize,
    /// 单个邻居对的块总数（正常是 48）。
    ///
    /// Block count in one neighbour pair (normally 48).
    pub total_blocks: usize,
    /// 逐帧全局位移。
    ///
    /// Per-frame global warp offsets.
    pub offsets: Vec<[f64; 2]>,
    /// 逐帧 8×6 网格位移场。
    ///
    /// Per-frame 8x6 grid shift field.
    pub blocks: Vec<Vec<[f64; 2]>>,
    /// 自动裁切。
    ///
    /// Automatic crop.
    pub crop: Option<CropRect>,
}

/// 由一帧线性 RGB 建 1/2 单通道 luma 代理（长边 ≤2048 封顶）。
///
/// Build a half-size single-channel linear luma proxy from one linear RGB frame, with the
/// long side capped at 2048 px.
pub fn proxy_luma(frame: &[f32], w: usize, h: usize) -> MonoProxy {
    let long = w.max(h).max(1);
    let scale = 2.0f64.max(long as f64 / PROXY_LUMA_MAX_SIDE as f64);
    let dw = ((w as f64 / scale).round() as usize).max(1);
    let dh = ((h as f64 / scale).round() as usize).max(1);
    let data = area_mono(frame, w, h, 3, dw, dh);
    MonoProxy {
        w: dw,
        h: dh,
        scale,
        data,
    }
}

/// 由一帧线性 RGB 建 1/8 RGB 代理（长边 ≤800 封顶）。
///
/// Build an eighth-size RGB proxy from one linear RGB frame, with the long side capped at
/// 800 px.
pub fn proxy_rgb(frame: &[f32], w: usize, h: usize) -> RgbProxy {
    let long = w.max(h).max(1);
    let scale = 8.0f64.max(long as f64 / PROXY_RGB_MAX_SIDE as f64);
    let dw = ((w as f64 / scale).round() as usize).max(1);
    let dh = ((h as f64 / scale).round() as usize).max(1);
    let data = area_rgb(frame, w, h, dw, dh);
    RgbProxy {
        w: dw,
        h: dh,
        scale,
        data,
    }
}

/// 焦点测度平面（与 `focus_stack` 同一口径），给判据与库调用方用。
///
/// The focus-measure plane (same convention as `focus_stack`), exposed for assertions and
/// library callers.
pub fn measure_plane(frame: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    focus::measure_plane(frame, w, h, radius)
}

/// 一块区域的平均焦点测度；`margin` 用来切掉插值边界。
///
/// Mean focus measure over the inner region, with `margin` cutting off interpolation edges.
pub fn mean_measure(frame: &[f32], w: usize, h: usize, radius: usize, margin: usize) -> f32 {
    let m = measure_plane(frame, w, h, radius);
    let mx = margin.min(w / 2);
    let my = margin.min(h / 2);
    let x1 = w.saturating_sub(mx);
    let y1 = h.saturating_sub(my);
    if x1 <= mx || y1 <= my {
        return 0.0;
    }
    let mut sum = 0.0f64;
    let mut cnt = 0u64;
    for y in my..y1 {
        for x in mx..x1 {
            sum += m[y * w + x] as f64;
            cnt += 1;
        }
    }
    if cnt == 0 {
        0.0
    } else {
        (sum / cnt as f64) as f32
    }
}

/// 按 `dst(x,y) = src(x - off_x, y - off_y)` 做双线性 warp（RGB，边界夹边）。
///
/// Bilinear warp of an RGB frame under `dst(x,y) = src(x - off_x, y - off_y)`, with clamped
/// edges.
pub fn warp_rgb(frame: &[f32], w: usize, h: usize, off: [f64; 2]) -> Vec<f32> {
    warp_channels(frame, w, h, 3, off)
}

/// 块级相位相关的公开入口（内部委托 `align.rs` 的 FFT/parab 私有基建）。
///
/// Public entry for block phase correlation; delegates to the private FFT/parab machinery
/// inside `align.rs`.
pub fn phase_shift(prev: &[f32], cur: &[f32], w: usize, h: usize) -> ([f64; 2], f32) {
    align::focus_phase_shift(prev, cur, w, h)
}

/// 块评分：把当前块按估计位移挪回去，取「融合块测度 / 两帧中较好的测度」。
///
/// 用**焦点测度比**而不是 NCC：失焦帧本来就该糊，NCC 会把「糊」当成「没对准」；
/// 除以两帧里较好的那一个，糊帧就不会把分数拖到 0。平场两帧测度都近零，分母也近零，
/// 直接给 0 分。返回 0..1 的分数，低于 `BLOCK_MIN_SCORE` 记失败块。
///
/// Block score: warp the current block back by the estimated shift, then take the ratio of
/// the fused block's focus measure to the better of the two input measures. A **focus-measure
/// ratio** instead of NCC -- a defocused frame is supposed to be soft, and NCC would read
/// "soft" as "misaligned", whereas dividing by the better of the two keeps the score high.
/// A flat field has near-zero measures on both sides, so the score is 0. Returns 0..1;
/// below `BLOCK_MIN_SCORE` the block is failed.
pub fn block_score(
    prev: &[f32],
    cur: &[f32],
    w: usize,
    h: usize,
    radius: usize,
    shift: [f64; 2],
) -> f32 {
    let m_prev = mean_measure_mono(prev, w, h, radius);
    let m_cur = mean_measure_mono(cur, w, h, radius);
    let aligned = warp_channels(cur, w, h, 1, shift);
    let fused: Vec<f32> = prev
        .iter()
        .zip(aligned.iter())
        .map(|(&a, &b)| 0.5 * (a + b))
        .collect();
    let m_fused = mean_measure_mono(&fused, w, h, radius);
    let best = m_prev.max(m_cur);
    if best <= 1e-6 {
        0.0
    } else {
        (m_fused / best).clamp(0.0, 1.0)
    }
}

/// 一个邻居对的 8×6 网格位移场：块级相位相关 + 抛物线亚像素 + 测度比验收。
///
/// The 8x6 grid shift field for one neighbour pair: block phase correlation with parabolic
/// sub-pixel refinement, accepted by a focus-measure ratio.
pub fn block_shifts(prev: &MonoProxy, cur: &MonoProxy, radius: usize) -> BlockField {
    let n = GRID_COLS * GRID_ROWS;
    let mut field = BlockField::zeros();
    if prev.w != cur.w || prev.h != cur.h || prev.w < 16 || prev.h < 16 {
        field.failed = n;
        return field;
    }
    let (pw, ph) = (prev.w, prev.h);
    for by in 0..GRID_ROWS {
        for bx in 0..GRID_COLS {
            let x0 = bx * pw / GRID_COLS;
            let x1 = (bx + 1) * pw / GRID_COLS;
            let y0 = by * ph / GRID_ROWS;
            let y1 = (by + 1) * ph / GRID_ROWS;
            let (bw, bh) = (x1 - x0, y1 - y0);
            let idx = by * GRID_COLS + bx;
            if bw < 16 || bh < 16 {
                field.failed += 1;
                continue;
            }
            let a = crop_mono(&prev.data, pw, x0, y0, bw, bh);
            let b = crop_mono(&cur.data, pw, x0, y0, bw, bh);
            let (proxy_shift, _peak) = align::focus_phase_shift(&a, &b, bw, bh);
            let score = block_score(&a, &b, bw, bh, radius, proxy_shift);
            field.scores[idx] = score;
            // 失败块不贡献位移，也不给自信的错值（A-C4 的负向锚）。
            //
            // A failed block contributes no shift and no confident wrong value (A-C4's
            // negative anchor).
            if score < BLOCK_MIN_SCORE {
                field.failed += 1;
                field.shifts[idx] = [0.0, 0.0];
            } else {
                field.shifts[idx] = [proxy_shift[0] * prev.scale, proxy_shift[1] * prev.scale];
            }
        }
    }
    // `field.shifts` 长度恒为 48；上面 `zeros()` 已保证。
    debug_assert_eq!(field.shifts.len(), n);
    field
}

/// 邻居链式累计：`d_k` 是第 k 帧相对第 k−1 帧的位移，第 i 帧累计 `Σ_{k≤i} d_k`。
///
/// Neighbour chaining: `d_k` is frame k's displacement relative to frame k-1, and frame i
/// accumulates `sum_{k<=i} d_k`.
pub fn chain_offsets(fields: &[BlockField]) -> Vec<[f64; 2]> {
    let mut out = vec![[0.0f64, 0.0f64]; fields.len()];
    let mut acc = [0.0f64, 0.0f64];
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            let rep = representative(&f.shifts);
            acc[0] += rep[0];
            acc[1] += rep[1];
        }
        out[i] = acc;
    }
    out
}

/// 逐帧累计 48 块位移场：第 i 帧每块的位移是 `Σ_{k≤i} d_k[块]`。
///
/// `chain_offsets` 是它的"全局代表值"版本（每帧取一块中位数），本函数保留完整的
/// 8×6 网格，供 T4 逐块 warp 与 A-C5「网格优于全局平移」判据使用。第 0 帧恒为全零
/// 参考场；把每帧场都填成同一个代表值，就退化成 1×1 全局平移——那正是 A-C5 的
/// 负向锚：网格版与全局版逐点相同，四角测度比值掉到 1.0。
///
/// Per-frame accumulated 48-block shift field: block b of frame i holds
/// `sum_{k<=i} d_k[b]`. `chain_offsets` is its single-representative counterpart (one median
/// per frame); this function keeps the full 8x6 grid for T4's per-block warp and for the
/// A-C5 "grid beats global translation" assertion. Frame 0 is always the all-zero reference.
/// Filling every frame's field with one representative value degenerates it into a 1x1 global
/// translation -- exactly A-C5's negative anchor: the grid and global versions then become
/// point-wise identical and the corner-measure ratio drops to 1.0.
pub fn chain_blocks(fields: &[BlockField]) -> Vec<Vec<[f64; 2]>> {
    let n = GRID_COLS * GRID_ROWS;
    let mut acc = vec![[0.0f64, 0.0f64]; n];
    let mut out = Vec::with_capacity(fields.len());
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            for b in 0..n {
                let s = f.shifts.get(b).copied().unwrap_or([0.0, 0.0]);
                acc[b][0] += s[0];
                acc[b][1] += s[1];
            }
        }
        out.push(acc.clone());
    }
    out
}

/// 1/8 RGB 代理上的收益统计：`(对齐合成测度, 未对齐合成测度)`。
///
/// Gain statistics on 1/8 RGB proxies: `(aligned composite measure, unaligned composite
/// measure)`.
pub fn gain_stats(
    frames: &[&[f32]],
    w: usize,
    h: usize,
    offsets: &[[f64; 2]],
    radius: usize,
) -> (f32, f32) {
    if frames.is_empty() {
        return (0.0, 0.0);
    }
    let proxies: Vec<RgbProxy> = frames.iter().map(|f| proxy_rgb(f, w, h)).collect();
    let (pw, ph) = (proxies[0].w, proxies[0].h);
    let scale = proxies[0].scale;
    let opts = Options {
        method: Method::Weighted,
        radius,
        threshold: 0.0,
        smooth_z: 1,
        feather: 2.0,
        levels: 0,
    };
    let unaligned: Vec<Vec<f32>> = proxies.iter().map(|p| p.data.clone()).collect();
    let aligned: Vec<Vec<f32>> = proxies
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let off = offsets.get(i).copied().unwrap_or([0.0, 0.0]);
            warp_rgb(&p.data, pw, ph, [off[0] / scale, off[1] / scale])
        })
        .collect();
    let a_refs: Vec<&[f32]> = aligned.iter().map(Vec::as_slice).collect();
    let u_refs: Vec<&[f32]> = unaligned.iter().map(Vec::as_slice).collect();
    let a_out =
        focus::focus_stack(&a_refs, pw, ph, &opts).unwrap_or_else(|_| vec![0.0f32; pw * ph * 3]);
    let u_out =
        focus::focus_stack(&u_refs, pw, ph, &opts).unwrap_or_else(|_| vec![0.0f32; pw * ph * 3]);
    (
        mean_measure(&a_out, pw, ph, radius, 2),
        mean_measure(&u_out, pw, ph, radius, 2),
    )
}

/// 三态决策（纯函数）：失败块占比 >25% → Failed；否则增益 <0.005 → 回退；否则采用。
///
/// Three-state decision (pure): more than 25% failed blocks means `Failed`; otherwise a gain
/// below 0.005 means revert; anything else is adopted.
pub fn decide(input: &DecideInput) -> FocusAlignReport {
    let denom = input.mean_unaligned;
    let gain = if denom.is_finite() && denom > 0.0 && input.mean_aligned.is_finite() {
        (input.mean_aligned - denom) / denom
    } else {
        0.0
    };
    let too_many_failures = input.total_blocks > 0
        && (input.failed_blocks as f32 / input.total_blocks as f32) > FAIL_RATIO;
    let status = if too_many_failures {
        AlignStatus::Failed
    } else if gain < GAIN_MIN {
        // 收益门槛是 A-C3 的负向锚：改成「永不回退」干净栈就会被错误对齐。
        //
        // The gain threshold is A-C3's negative anchor: make it "never revert" and a clean
        // stack is aligned for no reason.
        AlignStatus::RevertedInsufficientGain
    } else {
        AlignStatus::Adopted
    };
    let reason = match status {
        AlignStatus::Adopted => REASON_ADOPTED,
        AlignStatus::RevertedInsufficientGain => REASON_REVERTED,
        AlignStatus::Failed => REASON_FAILED,
    };
    FocusAlignReport {
        status,
        gain,
        offsets: input.offsets.clone(),
        blocks: input.blocks.clone(),
        crop: input.crop,
        failed_blocks: input.failed_blocks,
        reason,
    }
}

/// 无手动 report 时的自动裁切（直接复用 `align::common_crop`）。
///
/// Automatic crop when no manual report exists (delegates to `align::common_crop`).
pub fn auto_crop(offsets: &[[f64; 2]], w: u32, h: u32) -> CropRect {
    align::common_crop(offsets, w, h)
}

/// 面积平均降采样单通道（线性 luma）。
///
/// Area-average downsampling of a single channel (linear luma).
fn area_mono(
    frame: &[f32],
    sw: usize,
    sh: usize,
    channels: usize,
    dw: usize,
    dh: usize,
) -> Vec<f32> {
    let mut out = vec![0.0f32; dw * dh];
    for dy in 0..dh {
        let sy0 = dy * sh / dh;
        let sy1 = (((dy + 1) * sh / dh).max(sy0 + 1)).min(sh);
        for dx in 0..dw {
            let sx0 = dx * sw / dw;
            let sx1 = (((dx + 1) * sw / dw).max(sx0 + 1)).min(sw);
            let mut acc = 0.0f64;
            let mut cnt = 0u64;
            for y in sy0..sy1 {
                for x in sx0..sx1 {
                    let j = (y * sw + x) * channels;
                    acc += 0.2126 * frame[j] as f64
                        + 0.7152 * frame[j + 1] as f64
                        + 0.0722 * frame[j + 2] as f64;
                    cnt += 1;
                }
            }
            out[dy * dw + dx] = if cnt == 0 {
                0.0
            } else {
                (acc / cnt as f64) as f32
            };
        }
    }
    out
}

/// 面积平均降采样三通道。
///
/// Area-average downsampling of three interleaved channels.
fn area_rgb(frame: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; dw * dh * 3];
    for dy in 0..dh {
        let sy0 = dy * sh / dh;
        let sy1 = (((dy + 1) * sh / dh).max(sy0 + 1)).min(sh);
        for dx in 0..dw {
            let sx0 = dx * sw / dw;
            let sx1 = (((dx + 1) * sw / dw).max(sx0 + 1)).min(sw);
            let mut acc = [0.0f64; 3];
            let mut cnt = 0u64;
            for y in sy0..sy1 {
                for x in sx0..sx1 {
                    let j = (y * sw + x) * 3;
                    for c in 0..3 {
                        acc[c] += frame[j + c] as f64;
                    }
                    cnt += 1;
                }
            }
            let d = (dy * dw + dx) * 3;
            for c in 0..3 {
                out[d + c] = if cnt == 0 {
                    0.0
                } else {
                    (acc[c] / cnt as f64) as f32
                };
            }
        }
    }
    out
}

/// 从一张大平面裁一块连续子窗。
///
/// Crop a contiguous sub-window out of a larger plane.
fn crop_mono(src: &[f32], sw: usize, x0: usize, y0: usize, bw: usize, bh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; bw * bh];
    for y in 0..bh {
        let s = (y0 + y) * sw + x0;
        out[y * bw..(y + 1) * bw].copy_from_slice(&src[s..s + bw]);
    }
    out
}

/// 单通道 luma 的平均焦点测度（扩成 RGB 后复用 `measure_plane` 的 γ2.0 口径）。
///
/// Mean focus measure of a single-channel luma plane (expanded to RGB so `measure_plane`
/// applies its gamma-2.0 luma convention).
fn mean_measure_mono(plane: &[f32], w: usize, h: usize, radius: usize) -> f32 {
    let mut rgb = vec![0.0f32; w * h * 3];
    for (p, &v) in plane.iter().enumerate() {
        let j = p * 3;
        rgb[j] = v;
        rgb[j + 1] = v;
        rgb[j + 2] = v;
    }
    mean_measure(&rgb, w, h, radius, 2)
}

/// 通用双线性 warp；`channels` 为 1（luma）或 3（RGB）。
///
/// Generic bilinear warp; `channels` is 1 (luma) or 3 (RGB).
fn warp_channels(src: &[f32], w: usize, h: usize, channels: usize, off: [f64; 2]) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h * channels];
    let (dx, dy) = (off[0], off[1]);
    for y in 0..h {
        for x in 0..w {
            let sx = x as f64 - dx;
            let sy = y as f64 - dy;
            let fx = sx.floor();
            let fy = sy.floor();
            let tx = sx - fx;
            let ty = sy - fy;
            let x0 = (fx as isize).clamp(0, w as isize - 1) as usize;
            let y0 = (fy as isize).clamp(0, h as isize - 1) as usize;
            let xa = (x0 + 1).min(w - 1);
            let ya = (y0 + 1).min(h - 1);
            let j00 = (y0 * w + x0) * channels;
            let j10 = (y0 * w + xa) * channels;
            let j01 = (ya * w + x0) * channels;
            let j11 = (ya * w + xa) * channels;
            let d = (y * w + x) * channels;
            for c in 0..channels {
                let top = src[j00 + c] as f64 * (1.0 - tx) + src[j10 + c] as f64 * tx;
                let bot = src[j01 + c] as f64 * (1.0 - tx) + src[j11 + c] as f64 * tx;
                out[d + c] = (top * (1.0 - ty) + bot * ty) as f32;
            }
        }
    }
    out
}

/// 一个位移场的代表值：x/y 各自取中位数，抗住个别块的野值。
///
/// Representative shift of one field: the median of x and of y separately, robust against
/// a few outlier blocks.
fn representative(shifts: &[[f64; 2]]) -> [f64; 2] {
    if shifts.is_empty() {
        return [0.0, 0.0];
    }
    let mut xs: Vec<f64> = shifts.iter().map(|s| s[0]).collect();
    let mut ys: Vec<f64> = shifts.iter().map(|s| s[1]).collect();
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);
    let mid = xs.len() / 2;
    let mx = if xs.len() % 2 == 1 {
        xs[mid]
    } else {
        0.5 * (xs[mid - 1] + xs[mid])
    };
    let my = if ys.len() % 2 == 1 {
        ys[mid]
    } else {
        0.5 * (ys[mid - 1] + ys[mid])
    };
    [mx, my]
}
