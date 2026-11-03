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
    /// 这一组是给脚本与 MCP 客户端判读的短串，**唯一**的对外表达。报告结构里原先还有
    /// 一个 `reason: &'static str` 字段装中文模板键，本意是给人看的译文键，但生产侧
    /// 一个消费者都没有：结果 JSON 不外发它，面板按 `status` 选整句，只有测试在断言它。
    /// 留着它等于给下次「顺手透传出去」留了个入口，字段与那三个常量已一并删除。要加
    /// 人话，加在 UI 侧按 `status` 分支，不要给报告结构加字段。
    ///
    /// Stable machine key, used by the command surface and the result JSON (**never** hand
    /// write these literals -- changing the enum should break compilation here).
    ///
    /// These short strings are for scripts and MCP clients and are the **only** outward
    /// expression. The report used to carry a `reason: &'static str` field holding a Chinese
    /// template key, meant as a human-facing translation key, but nothing in production
    /// consumed it: the result JSON never emitted it, the panel picks a whole sentence from
    /// `status`, and only tests asserted it. Keeping it was an open door for "just pass it
    /// through" next time, so the field and its three constants are gone. Human sentences
    /// belong in the UI, branched on `status` -- never as a new field on the report.
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

/// 一次完整的估计：代理 → 邻居链式网格场 → 收益统计 → 三态决策。
///
/// 这是 UI 与无头 MCP 的**同一条**编排，只是取消语义由调用方给：`cancelled` 每对
/// 相邻帧之间被问一次（大图上这一步不便宜），返回 `None` 表示调用方要求放弃。
/// 只估计、不碰帧、不写盘——三态里只有 `Adopted` 值得上层去 warp。
///
/// 两边现在怎么传：UI 传真的取消标志（面板上的取消按钮真的会把它置位，因为 UI 是
/// 轮询式重绘）；MCP 传 `&|| false`，**不是偷懒**——`aevum_focus_merge` 是同步调用，
/// 阻塞 stdio 主循环，`aevum_cancel` 那一行在它跑完之前根本读不进来，而且它也没注册成
/// 后台 job，`cancel()` 找的是注册过的 job。所以在这条路径上接真旗标只是装饰。要让它
/// 真能取消，得先把它改成后台 job（`background: true` + `session.start_background`）。
///
/// The complete estimate: proxies -> neighbour-chained grid field -> gain statistics ->
/// three-state decision.
///
/// This is the **same** orchestration for the UI and the headless MCP; only cancellation
/// comes from the caller: `cancelled` is asked once between each neighbour pair (this step
/// is not cheap on a large frame), and `None` means the caller asked to give up. It only
/// estimates -- no frame is modified and nothing is written; only `Adopted` is worth warping
/// for upstream.
///
/// What each side passes today: the UI passes its real cancel flag (the panel's cancel
/// button does set it, because the UI redraws on a poll); the MCP passes `&|| false`, and
/// that is **not laziness** -- `aevum_focus_merge` is a synchronous call that blocks the
/// stdio main loop, so `aevum_cancel` is never even read until it finishes, and it is not
/// registered as a background job either, which is what `cancel()` looks for. Wiring a real
/// flag into this path would be decoration. Making it genuinely cancellable means turning it
/// into a background job first (`background: true` plus `session.start_background`).
pub fn estimate(
    frames: &[&[f32]],
    w: usize,
    h: usize,
    radius: usize,
    manual_crop: Option<CropRect>,
    cancelled: &dyn Fn() -> bool,
) -> Option<FocusAlignReport> {
    let failed = || FocusAlignReport {
        status: AlignStatus::Failed,
        gain: 0.0,
        offsets: Vec::new(),
        blocks: Vec::new(),
        crop: Some(manual_crop.unwrap_or(CropRect {
            x: 0,
            y: 0,
            w: w as u32,
            h: h as u32,
        })),
        failed_blocks: GRID_COLS * GRID_ROWS,
    };
    if frames.len() < 2 || w == 0 || h == 0 {
        return Some(failed());
    }
    if cancelled() {
        return None;
    }
    // 代理按 1/2 分辨率建（长边封顶 2048），相邻两帧同时驻留即可链式累计。
    //
    // 链式实现：只留上一帧代理，`block_shifts` 用完即弃——原来这里 `collect` 了全部 n 张
    // （24MP×32 约 768 MB），与这段注释的说法正好相反。后面的 `gain_stats` 用的是原始
    // 帧、自己另建 1/8 RGB 代理，不依赖这些 1/2 代理，所以可以边算边丢。
    //
    // Proxies are built at half resolution (long side capped at 2048); keeping two
    // neighbouring frames resident is enough for chained accumulation.
    //
    // The chain keeps only the previous proxy and drops each one after `block_shifts` -- the
    // old code `collect`ed all n of them (about 768 MB at 24MP x 32), the opposite of what the
    // note above claims. The later `gain_stats` works from the raw frames and builds its own
    // 1/8 RGB proxies, so it does not depend on these 1/2 proxies and they can go as we go.
    let mut fields: Vec<BlockField> = Vec::with_capacity(frames.len());
    fields.push(BlockField::zeros());
    let mut prev = proxy_luma(frames[0], w, h);
    for k in 1..frames.len() {
        if cancelled() {
            return None;
        }
        let cur = proxy_luma(frames[k], w, h);
        fields.push(block_shifts(&prev, &cur, radius));
        prev = cur;
    }
    let offsets = chain_offsets(&fields);
    let blocks = chain_blocks(&fields);
    let failed_blocks = fields.iter().skip(1).map(|f| f.failed).max().unwrap_or(0);
    let (mean_aligned, mean_unaligned) = gain_stats(frames, w, h, &offsets, radius);
    let crop = manual_crop.unwrap_or_else(|| auto_crop(&offsets, w as u32, h as u32));
    let report = decide(&DecideInput {
        mean_aligned,
        mean_unaligned,
        failed_blocks,
        total_blocks: GRID_COLS * GRID_ROWS,
        offsets,
        blocks,
        crop: Some(crop),
    });
    if cancelled() {
        return None;
    }
    Some(report)
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
    FocusAlignReport {
        status,
        gain,
        offsets: input.offsets.clone(),
        blocks: input.blocks.clone(),
        crop: input.crop,
        failed_blocks: input.failed_blocks,
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

/// 接缝淡化带宽度（px）：相邻块各自的位移不同，硬拼会在块界留下断点。
///
/// 取 24px 的理由与设计拍板 ⑦ 一致：典型手持位移 ~5px，24px ≥ 4 倍，足以把断点糊成
/// 渐变；又远小于块宽（24MP 上约 750px），不会侵到块内真正有效的区域。
///
/// Seam feather width (px): neighbouring blocks carry different shifts, and a hard join
/// leaves a visible break on the boundary.
///
/// 24 px matches design ruling 7: typical handheld displacement is ~5 px, so 24 px is four
/// times that and smears the break into a gradient; and it is far below the block width
/// (about 750 px on 24 MP), so it never eats into the block's genuinely useful interior.
pub const BLOCK_SEAM_FEATHER: usize = 24;

/// 逐块 warp 一帧：按拍板 ⑦ 的「裁块 → 块内位移 → 裁回 → 淡化拼装」，不新写稠密内核。
///
/// 每块先按 `ceil(max_shift) + feather + 2` 外扩裁出（同时留出位移越界与淡化带所需
/// 的来源像素），块内用 `warp::transform_into` 施加该块自己的位移，再按 24px 线性
/// 交叉淡化的权重累加进输出、权重和归一。
///
/// Warp one frame block by block, following ruling 7's "crop the block -> apply the block
/// shift -> crop back -> feather into the output"; no new dense warp kernel is written.
///
/// Each block is first cropped with a `ceil(max_shift) + feather + 2` margin (wide enough for
/// both the shift excursion and the feather band), the block's own shift is applied inside
/// with `warp::transform_into`, and the result is accumulated into the output with 24 px
/// linear cross-fade weights, normalised by their sum.
pub fn warp_by_block_field(
    src: &[f32],
    w: usize,
    h: usize,
    shifts: &[[f64; 2]],
    crop: Option<(usize, usize, usize, usize)>,
) -> (Vec<f32>, usize, usize) {
    let (cx, cy, cw, ch) = {
        let (x, y, ww, hh) = crop.unwrap_or((0, 0, w, h));
        let x = x.min(w);
        let y = y.min(h);
        (x, y, ww.min(w - x), hh.min(h - y))
    };
    if cw == 0 || ch == 0 || w == 0 || h == 0 {
        return (Vec::new(), 0, 0);
    }
    let mut out = vec![0.0f32; cw * ch * 3];
    let mut wsum = vec![0.0f32; cw * ch];
    let cols = GRID_COLS;
    let rows = GRID_ROWS;
    let max_shift = shifts
        .iter()
        .fold(0.0f64, |m, s| m.max(s[0].abs()).max(s[1].abs()));
    let feather = BLOCK_SEAM_FEATHER;
    // 外扩量要同时覆盖两件事：位移越界（`ceil(max_shift)`）与接缝淡化带（`feather`）——
    // 淡化带里的像素虽然属于邻块，本块也要为它们提供样本，否则它们会落到裁好的样本
    // 矩形之外，被 `transform_into` 的边界镜像折回来，接缝处就出现折痕。
    //
    // The margin must cover both the shift excursion (`ceil(max_shift)`) and the seam feather
    // band (`feather`): pixels inside the feather band belong to the neighbour, but this
    // block still has to supply their samples -- otherwise they fall outside the cropped
    // sample rectangle and get folded back by `transform_into`'s border mirroring, leaving a
    // crease along the seam.
    let margin = max_shift.ceil() as usize + feather + 2;

    for by in 0..rows {
        for bx in 0..cols {
            let x0 = bx * w / cols;
            let x1 = (bx + 1) * w / cols;
            let y0 = by * h / rows;
            let y1 = (by + 1) * h / rows;
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let shift = shifts.get(by * cols + bx).copied().unwrap_or([0.0, 0.0]);
            // 外扩后的来源矩形（夹进画面），位移样本才不会读到块外已丢掉的像素。
            //
            // The expanded source rectangle (clamped to the frame), so shifted samples do
            // not miss pixels the block crop dropped.
            let sx0 = x0.saturating_sub(margin);
            let sy0 = y0.saturating_sub(margin);
            let sx1 = (x1 + margin).min(w);
            let sy1 = (y1 + margin).min(h);
            if sx1 <= sx0 || sy1 <= sy0 {
                continue;
            }
            let mut sample = Vec::new();
            crate::warp::transform_into(
                src,
                w,
                h,
                crate::warp::Framing {
                    angle_deg: 0.0,
                    off: [0.0, 0.0],
                },
                Some((sx0, sy0, sx1 - sx0, sy1 - sy0)),
                &mut sample,
            );
            let bw = sx1 - sx0;
            let bh = sy1 - sy0;
            let mut warped = Vec::new();
            crate::warp::transform_into(
                &sample,
                bw,
                bh,
                crate::warp::Framing {
                    angle_deg: 0.0,
                    off: shift,
                },
                None,
                &mut warped,
            );
            // 淡化权重：到块边界的有符号距离，边界为 0.5、向内 feather 处为 1、
            // 向外 feather 处为 0。两块在接缝两侧的权重接近且都非零（0.5 / 0.48 量级），
            // 归一后就是平滑过渡——既没有硬断点，也不会出现权重和远小于 1 的暗带。
            //
            // Feather weight from the signed distance to the block edge: 0.5 on the edge, 1
            // one feather inside, 0 one feather outside. Two neighbours' weights near a seam
            // are close and both non-zero (about 0.5 / 0.48), so normalisation yields a
            // smooth transition -- no hard break and no dark band from a tiny sum.
            let ramp = |d: f64| -> f32 {
                if feather == 0 {
                    if d >= 0.0 { 1.0 } else { 0.0 }
                } else {
                    let t = 0.5 + d / (2.0 * feather as f64);
                    t.clamp(0.0, 1.0) as f32
                }
            };
            // 输出窗口与「块矩形 + feather 带」的交集。与窗口求交是必须的：窗口可能已经
            // 被 auto_crop 裁小，块界落在窗口外时不该往窗口里写。下界还要夹到来源矩形
            // 之内：feather（24px）通常大于采样外扩量（位移 + 2px），越界的像素在
            // warped 缓冲里没有对应样本。
            //
            // Intersection of the output window with "block rectangle plus feather band".
            // Intersecting with the window matters: auto_crop may already have shrunk it, and
            // a block edge outside the window must not write into it. The lower bound is also
            // clamped to the source rectangle: the feather (24 px) is usually wider than the
            // sample margin (shift + 2 px), and pixels beyond it have no counterpart in the
            // warped buffer.
            let px0 = x0.saturating_sub(feather).max(cx).max(sx0);
            let px1 = (x1 + feather).min(cx + cw).min(w).min(sx1);
            let py0 = y0.saturating_sub(feather).max(cy).max(sy0);
            let py1 = (y1 + feather).min(cy + ch).min(h).min(sy1);
            for py in py0..py1 {
                // 纵向权重：到块上/下边界的距离取较小者（块内为正、带内为负）。
                //
                // Vertical weight: the smaller distance to the top/bottom edge (positive
                // inside the block, negative in the feather band).
                let dy = (py as f64 - y0 as f64).min(y1 as f64 - 1.0 - py as f64);
                let wy = ramp(dy);
                if wy <= 0.0 {
                    continue;
                }
                for px in px0..px1 {
                    let dx = (px as f64 - x0 as f64).min(x1 as f64 - 1.0 - px as f64);
                    let wt = axis_weight(ramp(dx), wy);
                    if wt <= 0.0 {
                        continue;
                    }
                    // warped 与 sample 同原点（只施加了块位移），减来源矩形原点即可。
                    //
                    // The warped buffer shares the sample's origin (only the block shift was
                    // applied), so subtracting the source rectangle origin is enough.
                    let wx = px - sx0;
                    let wyi = py - sy0;
                    if wx >= bw || wyi >= bh {
                        continue;
                    }
                    let si = (wyi * bw + wx) * 3;
                    let oi = ((py - cy) * cw + (px - cx)) * 3;
                    for c in 0..3 {
                        out[oi + c] += wt * warped[si + c];
                    }
                    wsum[(py - cy) * cw + (px - cx)] += wt;
                }
            }
        }
    }
    // 权重和归一；理论上每像素都被至少一块覆盖，权重和近零时用参考帧兜底（不留下黑块）。
    //
    // Normalise by the weight sum; every pixel should be covered by at least one block, and
    // a near-zero sum falls back to the reference frame rather than leaving a black hole.
    for (p, s) in wsum.iter().enumerate() {
        let s = *s;
        let j = p * 3;
        if s > 1e-6 {
            out[j] /= s;
            out[j + 1] /= s;
            out[j + 2] /= s;
        } else {
            let sx = ((cx + p % cw).min(w - 1) + (cy + p / cw).min(h - 1) * w) * 3;
            out[j] = src[sx];
            out[j + 1] = src[sx + 1];
            out[j + 2] = src[sx + 2];
        }
    }
    (out, cw, ch)
}

/// 两个方向权重的乘积：两个方向各自在接缝两侧连续，乘积也连续。
///
/// Product of the two per-axis weights: each axis is continuous across a seam, so the
/// product is too.
fn axis_weight(a: f32, b: f32) -> f32 {
    a * b
}
