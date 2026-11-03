//! 焦点合成：把一组不同对焦距离的帧融成一张全景深的图。
//!
//! 逆向调研（`docs/焦点合成-调研.md`）定下的口径：商业工具的三条路里，
//! **深度图法**质量上限最接近标杆且保色彩（不做金字塔重构，没有对比度/色彩漂移），
//! **加权平均**实现最简单、低噪栈质量好，1.4.0 里只落了这两条；1.5 起补上
//! **金字塔法**（`pyramid.rs`）：多尺度融合自带过渡，代价是低频层可能出现反转
//! 光晕，所以最粗两层改成等权底图，且它**不替代**深度图法作为默认值。三条都在这里。
//!
//! 设计里最重要的一条是**无细节区兜底**：焦点测度只在有纹理处有意义，
//! 表盘、天空、抛光面这类平坦区，argmax 只是放大噪声——所有测度驱动的工具
//! 都在这里出斑块（Zerene DMap 要用户手调对比度阈值滑杆就是为了这个）。
//! 这里把「测度太弱」的像素整块判给参考帧（序列中位帧），斑块直接消失。
//! 这与去鬼影的「素材帧兜底」是同一个思想：拿不准就原样用素材，别造。
//!
//! 全部在**线性 f32** 上进出；测度在 γ2.0 亮度上算——散焦是镜头的空间属性，
//! 但测度要反映感知对比，线性亮度会把暗部纹理的测度压得不成比例。
//!
//! # Focus stacking
//!
//! Fuse a bracket of frames taken at different focus distances into one all-in-focus
//! image. The reverse-engineering survey (`docs/焦点合成-调研.en.md`) set the course:
//! among the three families shipped by commercial tools, the **depth map** method has
//! the quality ceiling closest to the benchmark and preserves colour (no pyramid
//! reconstruction, hence no contrast/colour drift), **weighted average** is the simplest
//! and good on low-noise stacks, and 1.4.0 shipped only those two. From 1.5 the
//! **pyramid method** (`pyramid.rs`) joins them: multi-scale fusion transitions on its own,
//! at the price of possible inversion halos in the low frequencies, so its two coarsest
//! levels use equal weights as a base and it does **not** replace the depth map as the
//! default. All three live here.
//!
//! The single most important design decision is the **featureless fallback**: a focus
//! measure only means something where texture exists -- watch faces, skies and polished
//! surfaces turn argmax into pure noise amplification, and every measure-driven tool
//! blotches there (Zerene's DMap makes the user tune a contrast-threshold slider for
//! exactly this reason). Here pixels whose best measure falls below a percentile of the
//! best-measure distribution are handed wholesale to the reference frame (the middle
//! frame of the sequence), so the blotching disappears. It is the same idea as the
//! deghosting owner fallback: when unsure, use the source untouched instead of inventing.
//!
//! Everything passes in and out as **linear f32**; the measure itself runs on γ2.0
//! luma -- defocus is a spatial property of the lens, but the measure should track
//! perceived contrast, and linear luma crushes dark textures out of proportion.

use crate::stack::StackError;

/// 焦点合成语境下的错误串。
///
/// [`StackError`] 是堆栈与焦点合成共用的枚举，`StackError::cn()` 说的是「堆栈的帧尺寸
/// 不一致」——焦点合成的用户手上是一个对焦包围（bracket），不是星轨栈，看到「堆栈」
/// 两个字会莫名其妙。枚举共用、文案按场景分：这里给焦点这份，堆栈那份原样不动。
///
/// 返回的是可翻译的中文模板键，界面仍要过一遍当前语言的表（`crate::i18n::tr`）。
///
/// Error text in the focus-stacking context.
///
/// [`StackError`] is shared between the star-field stacker and focus stacking, and
/// `StackError::cn()` says "the stack's frames differ in size" -- a focus-stacking user
/// holds a focus bracket, not a star-trail stack, and the word "stack" there is confusing.
/// The enum stays shared while the wording splits by scenario: this is the focus one, and
/// the stacker's stays untouched.
///
/// The return value is a translatable Chinese template key; the UI still runs it through
/// the current language's table (`crate::i18n::tr`).
pub fn error_text(e: StackError) -> &'static str {
    match e {
        StackError::Empty => "焦点合成没有输入帧",
        StackError::SizeMismatch => "参与焦点合成的帧尺寸不一致",
        StackError::ZeroSize => "焦点合成尺寸为零",
    }
}

/// 一次融合最多吃多少帧。这个上限是**按帧驻留内存**定的：融合期全部帧同时驻留，
/// 而内存主体正是帧本身，不是测度平面（测度平面已经边算边比，不整批留）。
///
/// 实测口径（24 MP 一帧 = 2400 万像素 × 3 通道 × 4 B = 288 MB）：
///
/// * 32 帧 24 MP 峰值约 **9.6 GB**：帧自身 9.22 GB 是主体，融合期只多三个面
///   （输出面 + 归一化面 + 一张羽化掩膜，各 96 MB 上下）；
/// * 32 帧 6 MP 才约 2.3 GB——早先这里写的 2.3 GB 是 **6 MP** 的数，按它给
///   24 MP 栈挑机器会低估 5.6 倍，16 GB 机器直接换页。
///
/// 估算式：`帧数 × 像素数 × 12 B + 输出三个面`。库调用方拿它自查——24 MP 栈需要
/// 10 GB 以上空闲内存；更深的栈先分段再合（与星空堆栈同一个建议）。
///
/// The most frames one fusion accepts. The cap is set by **frame residency memory**:
/// every frame must be resident during the fusion, and the frames themselves, not the
/// measure planes, are the bulk of the bill (the planes are compared on the fly and never
/// held as a batch).
///
/// Measured on 24 MP (24 Mpx x 3 channels x 4 B = 288 MB per frame):
///
/// * 32 frames of 24 MP peak at about **9.6 GB**: the frames' own 9.22 GB is the bulk,
///   and fusion adds only three planes (output, normalisation and one feathered mask,
///   roughly 96 MB each);
/// * 32 frames of 6 MP is only about 2.3 GB -- the 2.3 GB this note used to claim was the
///   **6 MP** figure, and sizing a 24 MP stack by it underestimates by 5.6x, which pages
///   on a 16 GB machine.
///
/// Estimate: `frames x pixels x 12 B + three output planes`. Library callers can check
/// their own case with it: a 24 MP stack needs more than 10 GB free, and deeper sequences
/// should be fused in passes (the same advice the star-field stacker gives).
///
/// 金字塔法的增量口径**不同**：它不把 32 帧的完整塔一起驻留，而是逐帧流式下采样、
/// 边算边融，常驻的是输出塔（RGB ≈ 384 MB）+ 权重和塔（单通道 ≈ 128 MB），当前帧
/// 手里只有相邻两层。24 MP 峰值约 1.0 GB（推算值，实测由 T10 替换）；塌缩原地，
/// 不另开全分辨率缓冲。所以这条上限仍是**帧自身**定的，金字塔不会把 9.6 GB 再翻一倍。
///
/// The pyramid method's increment is **different**: it never holds all 32 full towers at
/// once, reducing and folding frame by frame. What stays resident is the output tower
/// (RGB, about 384 MB) plus the weight-sum tower (mono, about 128 MB), and each frame keeps
/// only two adjacent planes. A 24 MP peak is about 1.0 GB (an estimate, to be replaced by
/// T10's measurement); collapse is in place, with no extra full-resolution buffer. The cap
/// is therefore still set by the **frames themselves** -- the pyramid does not double the
/// 9.6 GB again.
pub const MAX_FRAMES: usize = 32;

/// 融合方式。
///
/// How the stack is fused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// 深度图：每像素挑测度最强的帧，无细节区兜底参考帧，过渡带羽化。
    ///
    /// Depth map: each pixel takes the frame with the strongest measure, featureless
    /// areas fall back to the reference frame, and transitions are feathered.
    DepthMap,
    /// 加权平均：测度直接当权重（四次方拉开差距），全场按权重混。
    ///
    /// Weighted average: the measure is the weight (raised to the 4th power to spread
    /// the field), blended across every frame.
    Weighted,
    /// 金字塔：Burt–Adelson 拉普拉斯塔按层融合，细层取清晰细节、最粗两层等权当底图。
    /// 多尺度自带过渡，`feather` / `smooth_z` 不适用；`levels = 1` 严格退化为加权平均。
    ///
    /// Pyramid: Burt-Adelson Laplacian fusion per level -- fine levels take the sharp
    /// detail, the two coarsest levels use equal weights as the base. Multi-scale fusion
    /// transitions on its own, so `feather` / `smooth_z` do not apply; `levels = 1`
    /// degenerates strictly to the weighted average.
    Pyramid,
}

impl Method {
    /// 全部方法，界面按它铺按钮。
    ///
    /// Every method, in the order the UI lays out its buttons.
    pub const ALL: [Method; 3] = [Method::DepthMap, Method::Weighted, Method::Pyramid];

    /// 稳定的短键，落盘与命令面用它。
    ///
    /// Stable short key, used on disk and on the command surface.
    pub fn key(self) -> &'static str {
        match self {
            Method::DepthMap => "depthmap",
            Method::Weighted => "weighted",
            Method::Pyramid => "pyramid",
        }
    }

    /// 中文原名（可翻译的模板键）。
    ///
    /// Chinese source name, used as the translatable template key.
    pub fn cn(self) -> &'static str {
        match self {
            Method::DepthMap => "深度图",
            Method::Weighted => "加权平均",
            Method::Pyramid => "金字塔",
        }
    }

    /// 从短键解析；落盘的老值要能读回来。
    ///
    /// Parse from the short key; values already on disk must read back.
    pub fn parse(raw: &str) -> Option<Method> {
        Method::ALL.iter().copied().find(|m| m.key() == raw)
    }
}

/// 一套融合参数。全部带默认值：不传就是深度图法 + 温和的羽化。
///
/// One set of fusion parameters. Everything defaults: nothing passed means the depth map
/// with a gentle feather.
#[derive(Clone, Debug)]
pub struct Options {
    /// 融合方式。
    ///
    /// Which fusion method.
    pub method: Method,
    /// 测度的分析半径（px）：改进拉普拉斯算子在这么大的窗口里做盒平均，
    /// 0 表示逐像素原始响应。对应商业工具的 Radius。
    ///
    /// The measure's analysis radius (px): the modified-Laplacian response is box-averaged
    /// over a window this size; 0 keeps the raw per-pixel response. Mirrors the Radius knob
    /// of commercial tools.
    pub radius: usize,
    /// 无细节阈值：全图最佳测度的分位数（0–100），低于这条线的像素整块判给参考帧。
    /// 0 = 关闭兜底。对应 DMap 的对比度阈值滑杆，但按分位数表达，跨序列可迁移。
    ///
    /// Featureless threshold: a percentile (0-100) of the per-image best-measure
    /// distribution; pixels below the cut are handed wholesale to the reference frame.
    /// 0 = fallback off. Mirrors DMap's contrast-threshold slider but expressed as a
    /// percentile, so it transfers across sequences.
    pub threshold: f32,
    /// 深度图去斑半径（px，模值滤波）：把孤立深度点并进周围多数派。
    /// 只对 [`Method::DepthMap`] 生效。
    ///
    /// Depth-map despeckle radius (px, mode filter): isolated depth points join the
    /// surrounding majority. Only [`Method::DepthMap`] uses it.
    pub smooth_z: usize,
    /// 过渡羽化半径（px）：深度跳变处两张素材按高斯权重交叠。< 0.5 视为硬切。
    /// 只对 [`Method::DepthMap`] 生效。
    ///
    /// Transition feather radius (px): across a depth jump the two sources overlap with
    /// Gaussian weights; below 0.5 the cut is hard. Only [`Method::DepthMap`] uses it.
    pub feather: f32,
    /// 金字塔层数：`0 = auto`（`clamp(floor(log2(min(w,h)/32)), 3, 8)`），显式值
    /// 夹紧到 `1..=12`。只对 [`Method::Pyramid`] 生效；**界面与 MCP 不暴露**，
    /// 判据与库调用方才能设。`levels = 1` 时金字塔严格退化为 [`Method::Weighted`]。
    ///
    /// Pyramid level count: `0 = auto`
    /// (`clamp(floor(log2(min(w,h)/32)), 3, 8)`), explicit values clamp into `1..=12`.
    /// Only [`Method::Pyramid`] reads it; the UI and MCP do **not** expose it, only
    /// assertions and library callers may set it. `levels = 1` degenerates strictly to
    /// [`Method::Weighted`].
    pub levels: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            method: Method::DepthMap,
            radius: 2,
            threshold: 0.0,
            smooth_z: 1,
            feather: 2.0,
            levels: 0,
        }
    }
}

/// 每帧的焦点测度平面：改进拉普拉斯（横向 + 纵向二阶差分的绝对值），
/// 在 γ2.0 亮度上算，再按 `radius` 盒平均（积分图，O(n)）。
///
/// One focus-measure plane per frame: the modified Laplacian (absolute horizontal plus
/// vertical second differences) on γ2.0 luma, box-averaged over `radius` via a summed-area
/// table (O(n)).
pub(crate) fn measure_plane(frame: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let ml = modified_laplacian(frame, w, h);
    if radius == 0 {
        return ml;
    }
    box_average(&ml, w, h, radius)
}

/// 多帧的**峰值**测度：先逐帧取改进拉普拉斯的最大值，再盒平均一次。
///
/// 金字塔的分位阈值只需要它，不需要每帧一张全分辨率测度面——那是它比 Weighted 慢
/// 一倍的主因。两种口径的关系：
///
/// - `t = 0`（默认，也就是「这一格有没有任何纹理」）时**逐像素严格等价**：
///   `max_f box(ml_f) > 0` ⟺ `∃f ∃q∈窗口(p): ml_f[q] > 0` ⟺ `box(max_f ml_f) > 0`，
///   因为 `ml >= 0`。所以 `t = 0` 的成片与改前**逐点相同**（有判据钉着）。
/// - `t > 0` 时分位线落在两个**不同**的分布上：一个是「每帧各自盒平均后取最大值」，
///   一个是「先取最大值再盒平均」。二者**不等价**：`box(max ml)` 与 `max(box ml)`
///   不可交换——纹理尺度与窗口（radius）可比时差异可观（第四轮回归实测：块状纹理
///   夹具 threshold = 25 下 65% 像素不同、max|Δ| = 0.23，见 `pyramid.rs` 模块头），
///   纹理远细于窗口时趋近零。`t = 0` 的严格等价主张有夹具实测背书（49152/49152
///   像素 max|Δ| = 0）。
///
/// Peak measure across frames: the per-frame modified Laplacian maxima, box-averaged once.
///
/// The pyramid's percentile threshold needs only this, not one full-resolution measure plane
/// per frame -- and computing those is exactly why it used to be twice as slow as Weighted.
/// The two forms relate as follows:
///
/// - At `t = 0` (the default, meaning "does anything here have texture at all") they are
///   **strictly equivalent per pixel**: `max_f box(ml_f) > 0` iff `exists f, q in window(p)`
///   with `ml_f[q] > 0` iff `box(max_f ml_f) > 0`, because `ml >= 0`. So the `t = 0` output is
///   **point-for-point identical** to before (an assertion pins this; regression round 4
///   measured 49152/49152 pixels, max|Δ| = 0 on the fixture).
/// - At `t > 0` the cut lands on two **different** distributions:
///   "box each frame, then take the max" versus "take the max, then box". These are **not**
///   equivalent -- `box(max ml)` and `max(box ml)` do not commute: when the texture scale is
///   comparable to the window the difference is material (regression round 4, block-texture
///   fixture at threshold = 25: 65% of pixels differ, max|Δ| = 0.23; see `pyramid.rs`'s
///   module header); for textures much finer than the window it tends to zero.
pub(crate) fn measure_peak(frames: &[&[f32]], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let n = w * h;
    let mut peak = vec![0.0f32; n];
    for f in frames {
        let ml = modified_laplacian(f, w, h);
        for (p, &m) in ml.iter().enumerate() {
            if m > peak[p] {
                peak[p] = m;
            }
        }
    }
    if radius == 0 {
        return peak;
    }
    box_average(&peak, w, h, radius)
}

/// γ2.0 亮度上的改进拉普拉斯（逐像素，未盒平均，O(n)）。
///
/// The modified Laplacian on γ2.0 luma, per pixel, before any box averaging (O(n)).
pub(crate) fn modified_laplacian(frame: &[f32], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    // γ2.0 亮度：sqrt 是把线性亮度抬到感知域最便宜的一步。
    //
    // γ2.0 luma: sqrt is the cheapest step that lifts linear luma into the perceptual domain.
    let mut gray = vec![0.0f32; n];
    for (p, g) in gray.iter_mut().enumerate() {
        let j = p * 3;
        let luma = 0.2126 * frame[j] + 0.7152 * frame[j + 1] + 0.0722 * frame[j + 2];
        *g = luma.max(0.0).sqrt();
    }
    // 改进拉普拉斯，边界夹边取。
    //
    // Modified Laplacian with clamped edges.
    let mut ml = vec![0.0f32; n];
    for y in 0..h {
        for x in 0..w {
            let p = y * w + x;
            let c = gray[p];
            let l = gray[y * w + x.saturating_sub(1)];
            let r = gray[y * w + (x + 1).min(w - 1)];
            let u = gray[y.saturating_sub(1) * w + x];
            let d = gray[(y + 1).min(h - 1) * w + x];
            ml[p] = (2.0 * c - l - r).abs() + (2.0 * c - u - d).abs();
        }
    }
    ml
}

/// 盒平均（积分图，O(n)）；`radius = 0` 时原样返回。
///
/// Box average via a summed-area table (O(n)); `radius = 0` returns the input as is.
pub(crate) fn box_average(ml: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    if radius == 0 {
        return ml.to_vec();
    }
    let n = w * h;
    // 盒平均：积分图把 O(n·k²) 压成 O(n)。边界窗口收缩（不再夹边——
    // 夹边会把窗口外的自己重复计入，边缘测度虚高）。
    //
    // Box average: a summed-area table turns O(n·k²) into O(n). Windows shrink at the
    // borders (no clamping -- clamping would double-count the pixel itself and inflate
    // edge measures).
    let mut sat = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row_sum = 0.0f64;
        for x in 0..w {
            row_sum += ml[y * w + x] as f64;
            sat[(y + 1) * (w + 1) + (x + 1)] = sat[y * (w + 1) + (x + 1)] + row_sum;
        }
    }
    let mut out = vec![0.0f32; n];
    for y in 0..h {
        let top = y.saturating_sub(radius);
        let bottom = (y + radius + 1).min(h);
        for x in 0..w {
            let left = x.saturating_sub(radius);
            let right = (x + radius + 1).min(w);
            let area = ((right - left) * (bottom - top)) as f32;
            let sum = sat[bottom * (w + 1) + right] + sat[top * (w + 1) + left]
                - sat[top * (w + 1) + right]
                - sat[bottom * (w + 1) + left];
            out[y * w + x] = (sum / area as f64) as f32;
        }
    }
    out
}

/// 深度图：每像素测度最强的帧号；`threshold` 分位线以下的像素改判参考帧；
/// 再做一轮模值滤波把孤立深度点并进周围多数派。
///
/// 测度平面**边算边比**，不整批留着：24 MP × 32 帧全留就是 3.07 GB，而调用方
/// 一个平面都用不上（只认帧号）。单趟比较的并列语义与「先存全部平面再逐像素取
/// argmax」完全一致——严格大于、先到的（帧号小的）赢。
///
/// The depth map: per pixel the frame with the strongest measure; pixels below the
/// `threshold` percentile are re-assigned to the reference frame; a mode filter then folds
/// isolated depth points into the surrounding majority.
///
/// The measure planes are **compared as they are computed** rather than kept as a batch:
/// 24 MP x 32 frames would be 3.07 GB of planes the caller never reads (it only wants the
/// frame index). The tie semantics of the single pass match "store every plane, then take
/// the argmax per pixel" exactly -- strictly greater wins, so the earlier (smaller) frame
/// index keeps a tie.
fn compute_owner_map(frames: &[&[f32]], w: usize, h: usize, options: &Options) -> Vec<u16> {
    let n = w * h;
    let count = frames.len();
    let mut best = vec![0.0f32; n];
    let mut owner = vec![0u16; n];
    for (i, f) in frames.iter().enumerate() {
        let m = measure_plane(f, w, h, options.radius);
        for p in 0..n {
            if m[p] > best[p] {
                best[p] = m[p];
                owner[p] = i as u16;
            }
        }
    }
    // 分位数兜底：直方图 4096 桶，避免对几百万像素排序。
    //
    // Percentile fallback: a 4096-bin histogram, sparing a sort over millions of pixels.
    let t = options.threshold.clamp(0.0, 100.0);
    if t > 0.0 && count > 1 {
        let max = best.iter().cloned().fold(0.0f32, f32::max);
        if max > 0.0 {
            let bins = 4096usize;
            let mut hist = vec![0u64; bins];
            let scale = (bins - 1) as f32 / max;
            for b in &best {
                hist[(b * scale) as usize] += 1;
            }
            let want = ((t / 100.0) as f64 * n as f64).floor() as u64;
            let mut acc = 0u64;
            let mut cut_bin = bins - 1;
            for (i, c) in hist.iter().enumerate() {
                acc += c;
                if acc >= want.max(1) {
                    cut_bin = i;
                    break;
                }
            }
            let cut = cut_bin as f32 / scale;
            let reference = (count / 2) as u16;
            for p in 0..n {
                if best[p] <= cut {
                    owner[p] = reference;
                }
            }
        }
    }
    // 模值滤波：窗口里出现最多的帧号获胜（并列取中间那个，稳定）。
    // 半径 0 跳过。O(n·k²)，默认半径 1（3×3）很便宜。
    //
    // Mode filter: the most frequent frame index in the window wins (ties go to the middle
    // value, deterministically). Radius 0 skips. O(n·k²); the default radius 1 (3x3) is cheap.
    if options.smooth_z > 0 {
        let r = options.smooth_z;
        let mut counts = vec![0u32; count];
        let mut filtered = owner.clone();
        for y in 0..h {
            let top = y.saturating_sub(r);
            let bottom = (y + r + 1).min(h);
            for x in 0..w {
                let left = x.saturating_sub(r);
                let right = (x + r + 1).min(w);
                for c in counts.iter_mut() {
                    *c = 0;
                }
                for yy in top..bottom {
                    for xx in left..right {
                        counts[owner[yy * w + xx] as usize] += 1;
                    }
                }
                let own = owner[y * w + x] as usize;
                let mut best_c = counts[own];
                let mut best_i = own;
                for (i, c) in counts.iter().enumerate() {
                    if *c > best_c || (*c == best_c && i < best_i) {
                        best_c = *c;
                        best_i = i;
                    }
                }
                filtered[y * w + x] = best_i as u16;
            }
        }
        owner = filtered;
    }
    owner
}

/// 一帧的 one-hot 选中掩膜做高斯模糊（σ = feather，边界夹边）。
/// feather < 0.5（含 NaN）时退回硬掩膜（零开销路径）。
///
/// 两条护栏，`Options` 是 `pub` 的、库调用方真会踩：
///
/// * 半径 `clamp(1, 1024)`：`feather = 1e9` 时内核半径按 1e9 建，
///   `d * d` 在 `i32` 上先溢出——实测 panic（`attempt to multiply with overflow`）；
///   `f32 → usize` 的 `as` 转换是饱和的，NaN 落到 0，再由 clamp 兜到 1；
/// * 早退判据写成 `feather.is_nan() || feather < 0.5`：NaN 与任何数比较恒
///   false，只写 `feather < 0.5` 会让 NaN 溜进卷积分支，除出全 NaN 掩膜。
///
/// Gaussian-blur one frame's one-hot selection mask (σ = feather, clamped edges).
/// A feather below 0.5 (NaN included) degenerates to the hard mask (the zero-overhead
/// path).
///
/// Two guard rails, because `Options` is `pub` and library callers do hit them:
///
/// * the radius is `clamp(1, 1024)`: with `feather = 1e9` the kernel radius follows
///   `1e9` and `d * d` overflows `i32` first -- measured panic (`attempt to multiply
///   with overflow`); `f32 -> usize` casts saturate, so NaN lands on 0 and the clamp
///   lifts it to 1;
/// * the early-out is written `feather.is_nan() || feather < 0.5` rather than a bare
///   `feather < 0.5`: NaN compares false against everything, so the bare form let NaN
///   into the convolution branch and divided its way to an all-NaN mask.
fn feather_mask(onehot: &[u8], w: usize, h: usize, feather: f32) -> Vec<f32> {
    let n = w * h;
    if feather.is_nan() || feather < 0.5 {
        return onehot.iter().map(|&v| v as f32).collect();
    }
    let sigma = feather;
    let k = (feather.ceil() as usize).clamp(1, 1024);
    let mut kernel = Vec::with_capacity(2 * k + 1);
    for d in -(k as i32)..=(k as i32) {
        kernel.push((-((d * d) as f32) / (2.0 * sigma * sigma)).exp());
    }
    let ksum: f32 = kernel.iter().sum();
    // 横向卷积到临时面，再纵向一步到位地累加进调用方的输出面。
    //
    // Convolve horizontally into a temp plane, then fold vertically in one pass.
    let mut tmp = vec![0.0f32; n];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0f32;
            for (ki, &kv) in kernel.iter().enumerate() {
                let d = ki as i32 - k as i32;
                let xx = (x as i32 + d).clamp(0, w as i32 - 1) as usize;
                acc += kv * onehot[y * w + xx] as f32;
            }
            tmp[y * w + x] = acc / ksum;
        }
    }
    let mut out = vec![0.0f32; n];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0f32;
            for (ki, &kv) in kernel.iter().enumerate() {
                let d = ki as i32 - k as i32;
                let yy = (y as i32 + d).clamp(0, h as i32 - 1) as usize;
                acc += kv * tmp[yy * w + x];
            }
            out[y * w + x] = acc / ksum;
        }
    }
    out
}

/// 融合。输入是**已经对齐**的线性 RGB `f32`，长度必须等于 `w*h*3`；
/// 哪帧在最前最后都行——深度图法只认测度，加权平均只认权重，没有方向假设。
///
/// Fuse. Inputs are **already aligned** linear RGB `f32`, each `w*h*3`; frame order carries
/// no direction assumption -- the depth map only reads measures and the weighted average
/// only reads weights.
pub fn focus_stack(
    frames: &[&[f32]],
    w: usize,
    h: usize,
    options: &Options,
) -> Result<Vec<f32>, StackError> {
    if frames.is_empty() {
        return Err(StackError::Empty);
    }
    if w == 0 || h == 0 {
        return Err(StackError::ZeroSize);
    }
    let n = w * h * 3;
    if frames.iter().any(|f| f.len() != n) {
        return Err(StackError::SizeMismatch);
    }
    let count = frames.len();
    if count == 1 {
        return Ok(frames[0].to_vec());
    }
    let reference = count / 2;

    match options.method {
        Method::DepthMap => {
            let owner = compute_owner_map(frames, w, h, options);
            // 每帧一张 one-hot 掩膜 → 羽化 → 直接累加输出；归一化面同步累加。
            // 内存峰值 = 输出面 + 归一化面 + 一张羽化掩膜。
            //
            // One one-hot mask per frame -> feather -> accumulate straight into the output;
            // the normalization plane accumulates alongside. Peak memory = output + weights
            // + one feathered mask.
            let mut out = vec![0.0f32; n];
            let mut wsum = vec![0.0f32; w * h];
            for i in 0..count {
                let onehot: Vec<u8> = owner.iter().map(|&o| u8::from(o == i as u16)).collect();
                let mask = feather_mask(&onehot, w, h, options.feather);
                let f = frames[i];
                for (p, wt) in mask.iter().enumerate() {
                    if *wt != 0.0 {
                        let j = p * 3;
                        out[j] += wt * f[j];
                        out[j + 1] += wt * f[j + 1];
                        out[j + 2] += wt * f[j + 2];
                        wsum[p] += wt;
                    }
                }
            }
            let rf = frames[reference];
            for (p, ws) in wsum.iter().enumerate() {
                if *ws > 1e-6 {
                    let j = p * 3;
                    out[j] /= *ws;
                    out[j + 1] /= *ws;
                    out[j + 2] /= *ws;
                } else {
                    let j = p * 3;
                    out[j] = rf[j];
                    out[j + 1] = rf[j + 1];
                    out[j + 2] = rf[j + 2];
                }
            }
            Ok(out)
        }
        Method::Weighted => {
            // 测度四次方当权重：强纹理与弱噪声之间四个数量级的差距，
            // 软选择才接近「选对的那帧」而不是「平均成糊的」。
            //
            // The measure to the 4th power is the weight: four orders of magnitude between
            // strong texture and weak noise keep the soft selection near "pick the sharp
            // frame" instead of "average into mush".
            let mut out = vec![0.0f32; n];
            let mut wsum = vec![0.0f32; w * h];
            for f in frames {
                let m = measure_plane(f, w, h, options.radius);
                for p in 0..w * h {
                    let wt = m[p] * m[p] * m[p] * m[p];
                    if wt > 0.0 {
                        let j = p * 3;
                        out[j] += wt * f[j];
                        out[j + 1] += wt * f[j + 1];
                        out[j + 2] += wt * f[j + 2];
                        wsum[p] += wt;
                    }
                }
            }
            let rf = frames[reference];
            for (p, ws) in wsum.iter().enumerate() {
                if *ws > 0.0 {
                    let j = p * 3;
                    out[j] /= *ws;
                    out[j + 1] /= *ws;
                    out[j + 2] /= *ws;
                } else {
                    let j = p * 3;
                    out[j] = rf[j];
                    out[j + 1] = rf[j + 1];
                    out[j + 2] = rf[j + 2];
                }
            }
            Ok(out)
        }
        Method::Pyramid => {
            // 这里**不读** `options.feather` 与 `options.smooth_z`：金字塔按层归一自带
            // 过渡，没有深度跳变，也没有 one-hot 掩膜可羽化。命令面照旧校验它们，
            // 传了不影响输出（X-C3）。层数、测度半径与阈值由 `levels/radius/threshold` 管。
            //
            // `options.feather` and `options.smooth_z` are **not read** here: per-level
            // normalisation transitions on its own, with no depth jump and no one-hot mask
            // to feather. The command surface still validates them, and passing them does
            // not change the output (X-C3). `levels/radius/threshold` govern this path.
            Ok(crate::pyramid::fuse_pyramid(frames, w, h, options))
        }
    }
}

/// 只算深度图（阈值与去斑之后的最终归属，不含羽化）：判据直接断言它，将来的面板预览
/// 也走这一个入口。
///
/// Compute just the depth map (the final assignment after threshold and despeckle, before
/// feathering): the tests assert on it, and a future panel preview would enter through the
/// same door.
pub fn depth_map(
    frames: &[&[f32]],
    w: usize,
    h: usize,
    options: &Options,
) -> Result<Vec<u16>, StackError> {
    if frames.is_empty() {
        return Err(StackError::Empty);
    }
    if w == 0 || h == 0 {
        return Err(StackError::ZeroSize);
    }
    let n = w * h * 3;
    if frames.iter().any(|f| f.len() != n) {
        return Err(StackError::SizeMismatch);
    }
    if frames.len() == 1 {
        return Ok(vec![0u16; w * h]);
    }
    let owner = compute_owner_map(frames, w, h, options);
    Ok(owner)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 48;
    const H: usize = 48;

    /// 平坦色块。
    ///
    /// A flat color.
    fn flat(v: f32) -> Vec<f32> {
        vec![v; W * H * 3]
    }

    /// 竖条纹栅栏（高频纹理），周期 4px。
    ///
    /// Vertical plank stripes (high-frequency texture), a 4 px period.
    fn planks() -> Vec<f32> {
        let mut f = vec![0.0f32; W * H * 3];
        for y in 0..H {
            for x in 0..W {
                let v = if (x / 2).is_multiple_of(2) { 0.8 } else { 0.2 };
                let j = (y * W + x) * 3;
                f[j] = v;
                f[j + 1] = v;
                f[j + 2] = v;
            }
        }
        f
    }

    /// 横条纹栅栏。
    ///
    /// Horizontal plank stripes.
    fn planks_h() -> Vec<f32> {
        let mut f = vec![0.0f32; W * H * 3];
        for y in 0..H {
            for x in 0..W {
                let v = if (y / 2).is_multiple_of(2) { 0.8 } else { 0.2 };
                let j = (y * W + x) * 3;
                f[j] = v;
                f[j + 1] = v;
                f[j + 2] = v;
            }
        }
        f
    }

    /// 把帧做 5×5 盒模糊（当「失焦」用）。
    ///
    /// Box-blur a frame 5x5 (standing in for defocus).
    fn blur5(f: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0f32; f.len()];
        for y in 0..H {
            for x in 0..W {
                for c in 0..3 {
                    let mut acc = 0.0f32;
                    let mut cnt = 0u32;
                    for dy in -2i32..=2 {
                        for dx in -2i32..=2 {
                            let yy = (y as i32 + dy).clamp(0, H as i32 - 1) as usize;
                            let xx = (x as i32 + dx).clamp(0, W as i32 - 1) as usize;
                            acc += f[(yy * W + xx) * 3 + c];
                            cnt += 1;
                        }
                    }
                    out[(y * W + x) * 3 + c] = acc / cnt as f32;
                }
            }
        }
        out
    }

    #[test]
    fn the_depth_map_assigns_each_region_to_the_frame_holding_the_sharp_texture() {
        // 三帧各管三分之一：上 1/3 只有帧 0 清晰，中 1/3 只有帧 1，下 1/3 只有帧 2。
        //
        // Three frames, one third each: only frame 0 is sharp in the top third, only
        // frame 1 in the middle, only frame 2 in the bottom.
        let sharp0 = planks();
        let sharp1 = planks_h();
        let sharp2 = {
            let mut f = planks();
            f.reverse();
            f
        };
        let mut f0 = blur5(&sharp1);
        f0[..W * 16 * 3].copy_from_slice(&sharp0[..W * 16 * 3]);
        let mut f1 = blur5(&sharp0);
        f1[W * 16 * 3..W * 32 * 3].copy_from_slice(&sharp1[W * 16 * 3..W * 32 * 3]);
        let mut f2 = blur5(&sharp0);
        f2[W * 32 * 3..].copy_from_slice(&sharp2[W * 32 * 3..]);
        let opts = Options {
            feather: 0.0,
            ..Options::default()
        };
        let dm = depth_map(&[&f0, &f1, &f2], W, H, &opts).expect("depth map");
        // 分界线两侧各留 radius + smooth_z 的边距：测度窗口会吃到邻区纹理，
        // 边界那一两行归谁在物理上就是模糊的（这是方法的属性，不是缺陷）。
        //
        // Leave a margin of radius + smooth_z on each side of a seam: the measure window
        // reaches into the neighbouring region, so ownership of the boundary rows is
        // genuinely ambiguous (a property of the method, not a defect).
        let margin = opts.radius + opts.smooth_z;
        assert!(
            dm[margin * W..(16 - margin) * W].iter().all(|&o| o == 0),
            "上 1/3（去掉边距）应归帧 0"
        );
        assert!(
            dm[(16 + margin) * W..(32 - margin) * W]
                .iter()
                .all(|&o| o == 1),
            "中 1/3（去掉边距）应归帧 1"
        );
        assert!(
            dm[(32 + margin) * W..(H - margin) * W]
                .iter()
                .all(|&o| o == 2),
            "下 1/3（去掉边距）应归帧 2"
        );
        // 负向验证：全失焦时深度图变不出清晰度——输出必须跟着糊，不能凭空造出锐利。
        //
        // Negative check: with everything defocused the depth map cannot invent sharpness --
        // the output must follow the blur.
        let b0 = blur5(&f0);
        let b1 = blur5(&f1);
        let b2 = blur5(&f2);
        let out_blurred = focus_stack(&[&b0, &b1, &b2], W, H, &opts).expect("stack of blurred");
        let probe = (8 * W + 8) * 3;
        assert!(
            out_blurred[probe] < 0.75,
            "全失焦后输出必须仍是糊的（不能凭空变锐利），实际 {}（清晰时是 0.8）",
            out_blurred[probe]
        );
    }

    #[test]
    fn a_featureless_field_falls_back_to_the_reference_frame() {
        // 帧 0：纯平 0.5 + 中央一块清晰纹理；帧 1：0.45 底 + 缓纹波平场
        // （测度弱但不为零——纯线性渐变的二阶导是零，测度会与纯平打平）；
        // 帧 2 与帧 0 同。参考帧 = 帧 1（中位）。阈值开大后，平坦区整块归帧 1。
        //
        // Frame 0: dead-flat 0.5 plus one central sharp patch; frame 1: a 0.45 base with a
        // gentle rippled field (weak but nonzero measure -- a pure linear gradient has zero
        // second derivative and would tie with the dead-flat frame); frame 2 same as 0. The
        // reference is frame 1 (the middle). With a high threshold the flat areas go
        // wholesale to frame 1.
        let mut f0 = flat(0.5);
        for y in 16..32 {
            for x in 16..32 {
                let j = (y * W + x) * 3;
                let v = if ((y * W + x) / 2).is_multiple_of(2) {
                    0.8
                } else {
                    0.2
                };
                f0[j] = v;
                f0[j + 1] = v;
                f0[j + 2] = v;
            }
        }
        let mut f1 = vec![0.0f32; W * H * 3];
        for y in 0..H {
            for x in 0..W {
                let v = 0.45 + 0.01 * (x as f32 * 0.5).sin();
                let j = (y * W + x) * 3;
                f1[j] = v;
                f1[j + 1] = v;
                f1[j + 2] = v;
            }
        }
        let f2 = f0.clone();
        // 负向验证先来：兜底关着（threshold=0）时，平坦区跟随 argmax（纹波帧测度高）。
        //
        // The negative check first: with the fallback off (threshold=0), flat areas follow
        // the argmax (the rippled frame has the higher measure there).
        let off = Options {
            threshold: 0.0,
            feather: 0.0,
            smooth_z: 0,
            ..Options::default()
        };
        let out_off = focus_stack(&[&f0, &f1, &f2], W, H, &off).expect("stack");
        // 左上角远离纹理块，平坦：out 应取帧 1 的纹波值（0.45 附近），而不是帧 0 的 0.5。
        //
        // The top-left corner is far from the patch, flat: out takes frame 1's rippled
        // value (around 0.45), not frame 0's 0.5.
        let p = (4 * W + 4) * 3;
        assert!(
            (out_off[p] - f1[p]).abs() < 1e-3,
            "兜底关闭时平坦区应跟随纹波帧（{}），实际 {}",
            f1[p],
            out_off[p]
        );
        assert!(
            (out_off[p] - 0.5).abs() > 0.03,
            "兜底关闭时平坦区不该是纯平帧的 0.5，实际 {}",
            out_off[p]
        );
        // 兜底开到 90 分位：平坦区整块归参考帧……参考帧就是帧 1，数值上分不出来。
        // 所以这里换参考帧的设计核验：深度图应把平坦区标成帧 1，纹理块标成帧 0。
        //
        // Threshold at the 90th percentile: the flat areas go to the reference frame --
        // which IS frame 1, indistinguishable by value. So assert on the depth map
        // instead: flat areas marked as frame 1, the patch as frame 0.
        let on = Options {
            threshold: 90.0,
            feather: 0.0,
            smooth_z: 0,
            ..Options::default()
        };
        let dm = depth_map(&[&f0, &f1, &f2], W, H, &on).expect("depth map");
        assert_eq!(dm[4 * W + 4], 1, "兜底后平坦区应归参考帧（帧 1）");
        let patch_center = 24 * W + 24;
        assert_eq!(dm[patch_center], 0, "纹理块应仍归帧 0");
    }

    #[test]
    fn feathering_blends_across_depth_jumps_without_a_step() {
        // 左半只有帧 0 清晰、右半只有帧 1 清晰；羽化开着时跨界的样本必须落在
        // 两帧之间（单调），羽化关着时边界像素就是硬切。
        //
        // Only frame 0 is sharp on the left half, only frame 1 on the right; with feathering
        // on, samples across the seam must sit between the two frames (monotonic), and with
        // feathering off the seam is a hard cut.
        let sharp0 = planks();
        let sharp1 = planks_h();
        let mut f0 = vec![0.5f32; W * H * 3]; // 帧 0 的右半是平场 0.5
        for y in 0..H {
            let src = (y * W) * 3;
            let dst = (y * W) * 3;
            f0[dst..dst + (W / 2) * 3].copy_from_slice(&sharp0[src..src + (W / 2) * 3]);
        }
        let mut f1 = vec![0.5f32; W * H * 3]; // 帧 1 的左半是平场 0.5
        for y in 0..H {
            let src = (y * W + W / 2) * 3;
            let dst = (y * W + W / 2) * 3;
            f1[dst..dst + (W / 2) * 3].copy_from_slice(&sharp1[src..src + (W / 2) * 3]);
        }
        let hard = Options {
            feather: 0.0,
            smooth_z: 0,
            ..Options::default()
        };
        let out_hard = focus_stack(&[&f0, &f1], W, H, &hard).expect("stack");
        // 硬切：左边界像素 = 帧 0 的纹理值（x=23 是暗纹 0.2），右边界 = 帧 1 的亮纹 0.8。
        //
        // Hard cut: the left seam pixel is frame 0's texture value (x=23 is a dark plank,
        // 0.2) and the right seam pixel is frame 1's bright plank, 0.8.
        let seam_l = (24 * W + 23) * 3;
        let seam_r = (24 * W + 24) * 3;
        assert!(
            (out_hard[seam_l] - f0[seam_l]).abs() < 1e-4,
            "硬切左边界应逐点等于帧 0（{}），实际 {}",
            f0[seam_l],
            out_hard[seam_l]
        );
        // 帧 1 的横条纹在 y=24 是 (24/2)%2==0 → 亮纹 0.8。
        //
        // Frame 1's horizontal planks at y=24: (24/2)%2==0 -> a bright plank, 0.8.
        assert!(
            (out_hard[seam_r] - f1[seam_r]).abs() < 1e-4,
            "硬切右边界应逐点等于帧 1（{}），实际 {}",
            f1[seam_r],
            out_hard[seam_r]
        );
        // 羽化：feather=4 时跨界的样本必须落在两帧之间。
        //
        // Feathering: with feather=4 samples across the seam must sit between the frames.
        let soft = Options {
            feather: 4.0,
            smooth_z: 0,
            ..Options::default()
        };
        let out_soft = focus_stack(&[&f0, &f1], W, H, &soft).expect("stack");
        let mid = (24 * W + 24) * 3;
        assert!(
            out_soft[mid] > 0.3 && out_soft[mid] < 0.85,
            "羽化后跨界样本应落在两帧之间，实际 {}",
            out_soft[mid]
        );
        // 负向验证：羽化面上离边界 ≥ 2×feather 的地方必须还是硬切值。
        //
        // Negative check: on the feathered result, at least 2x feather away from the seam
        // the values must match the hard cut.
        let far_l = (24 * W + 10) * 3;
        assert!(
            (out_soft[far_l] - out_hard[far_l]).abs() < 0.02,
            "远离边界处羽化不应改变取值：soft={} hard={}",
            out_soft[far_l],
            out_hard[far_l]
        );
    }

    #[test]
    fn weighted_average_concentrates_weight_on_the_sharp_frame() {
        // 两帧只有一处不同：帧 0 那里清晰、帧 1 失焦。加权平均后该区域必须几乎等于
        // 清晰帧；而全平区域（两帧测度同零）必须退回参考帧（帧 1）。
        //
        // Two frames differing in exactly one region: frame 0 is sharp there, frame 1
        // defocused. After the weighted average that region must be nearly the sharp frame;
        // a fully flat region (both measures zero) must fall back to the reference frame 1.
        let sharp = planks();
        let mut f0 = flat(0.5);
        f0[..W * 16 * 3].copy_from_slice(&sharp[..W * 16 * 3]);
        let mut f1 = f0.clone();
        {
            let blurred = blur5(&sharp);
            f1[..W * 16 * 3].copy_from_slice(&blurred[..W * 16 * 3]);
        }
        let opts = Options {
            method: Method::Weighted,
            ..Options::default()
        };
        let out = focus_stack(&[&f0, &f1], W, H, &opts).expect("stack");
        let sharp_px = (8 * W + 8) * 3;
        assert!(
            (out[sharp_px] - sharp[sharp_px]).abs() < 0.02,
            "纹理区应几乎等于清晰帧：out={} sharp={}",
            out[sharp_px],
            sharp[sharp_px]
        );
        let flat_px = (40 * W + 8) * 3;
        assert!(
            (out[flat_px] - 0.5).abs() < 1e-4,
            "全平区两帧相同，输出应就是 0.5，实际 {}",
            out[flat_px]
        );
        // 负向验证：纹理区不能是两帧的平均（0.5 与 0.8 的平均 ≈0.65 那种糊值）。
        //
        // Negative check: the textured region must not be the two-frame average (the mushy
        // middle of 0.5 and 0.8).
        assert!(
            (out[sharp_px] - 0.65).abs() > 0.1,
            "纹理区不该是糊成一团的平均值，实际 {}",
            out[sharp_px]
        );
    }

    #[test]
    fn an_oversized_feather_does_not_panic() {
        // `Options` 是 `pub` 的：`feather` 可以来自工程文件、命令面或库调用方。
        // 1e9 与 NaN 都必须返回 Ok——不许 panic，也不许把 NaN 漏进成片。
        //
        // 负向验证：去掉 `feather_mask` 里的 `.clamp(1, 1024)` 重跑，1e9 那一路会在
        // `d * d` 上整数溢出 panic（`attempt to multiply with overflow`），这条立刻红。
        //
        // `Options` is public: `feather` can arrive from a project file, the command
        // surface or a library caller. Both 1e9 and NaN must return Ok -- no panic, and no
        // NaN leaking into the output.
        //
        // Negative check: drop the `.clamp(1, 1024)` in `feather_mask` and rerun; the 1e9
        // path overflows `i32` on `d * d` (`attempt to multiply with overflow`) and this
        // test goes red immediately.
        let a = planks();
        let b = planks_h();
        for feather in [1e9f32, f32::NAN] {
            let opts = Options {
                feather,
                smooth_z: 0,
                ..Options::default()
            };
            let out = focus_stack(&[&a, &b], W, H, &opts)
                .unwrap_or_else(|e| panic!("feather={feather} 应返回 Ok，实得 {e:?}"));
            assert_eq!(out.len(), W * H * 3);
            assert!(
                out.iter().all(|v| v.is_finite()),
                "feather={feather} 时成片里有非有限值"
            );
        }
    }

    #[test]
    fn focus_errors_speak_the_focus_context() {
        // 焦点合成说「焦点」，堆栈说「堆栈」：共用一个枚举，但文案不许串场。
        //
        // 负向验证：把 `error_text(e)` 换回 `e.cn()`，这条立刻红（那正是要修的病）。
        //
        // Focus stacking says "focus", the stacker says "stack": one enum, but the wording
        // must not cross over.
        //
        // Negative check: swap `error_text(e)` back to `e.cn()` and this goes red -- that
        // is exactly the bug being fixed.
        assert_eq!(
            error_text(StackError::Empty),
            "焦点合成没有输入帧",
            "空输入要按焦点语境说话"
        );
        assert_ne!(
            error_text(StackError::Empty),
            StackError::Empty.cn(),
            "焦点合成的错误串不能复用堆栈那句"
        );
        assert_ne!(
            error_text(StackError::SizeMismatch),
            StackError::SizeMismatch.cn()
        );
        assert_ne!(error_text(StackError::ZeroSize), StackError::ZeroSize.cn());
        // 三句都得带「焦点」，否则迟早又被当成堆栈文案看走眼。
        //
        // All three must mention focus, or they will be mistaken for stacker copy again.
        for e in [
            StackError::Empty,
            StackError::SizeMismatch,
            StackError::ZeroSize,
        ] {
            assert!(error_text(e).contains("焦点"), "{e:?} 的文案没提焦点");
        }
        // 真出错时也要走这条路：空输入经 `focus_stack` 拿到的原因与文案对得上。
        //
        // The real error path too: an empty input yields the variant whose text we assert.
        let err = focus_stack(&[], W, H, &Options::default()).expect_err("空输入必须报错");
        assert_eq!(error_text(err), "焦点合成没有输入帧");
    }

    #[test]
    fn degenerate_inputs_are_handled() {
        let f = planks();
        assert!(matches!(
            focus_stack(&[], W, H, &Options::default()),
            Err(StackError::Empty)
        ));
        assert!(matches!(
            focus_stack(&[&f], 0, H, &Options::default()),
            Err(StackError::ZeroSize)
        ));
        let short = vec![0.0f32; 10];
        assert!(matches!(
            focus_stack(&[&f, &short], W, H, &Options::default()),
            Err(StackError::SizeMismatch)
        ));
        // 单帧原样返回。
        //
        // A single frame returns untouched.
        let out = focus_stack(&[&f], W, H, &Options::default()).expect("single");
        assert_eq!(out, f);
        // 半径 0 与模值滤波 0 都是合法参数。
        //
        // Radius 0 and a mode-filter radius of 0 are both legal.
        let g = flat(0.4);
        let opts = Options {
            radius: 0,
            smooth_z: 0,
            ..Options::default()
        };
        let out = focus_stack(&[&f, &g], W, H, &opts).expect("r0");
        assert_eq!(out.len(), W * H * 3);
        // method 短键往返。
        //
        // Method key round-trip.
        for m in Method::ALL {
            assert_eq!(Method::parse(m.key()), Some(m));
        }
        assert_eq!(Method::parse("nope"), None);
    }

    /// D1b（台账开放项收口）：`measure_peak` 的次序语义——先逐帧取逐像素最大、
    /// 再一次盒平均（`box(max ml)`），**不是**「每帧各自盒平均后取最大」
    /// （`max(box ml)`）。第四轮回归把盒平均半径变异为 0 后 P-C1…P-C6 仍全绿，
    /// 说明窗口分支没有任何判据；这条把次序与半径分支都钉住。
    ///
    /// D1b (ledger open item): `measure_peak`'s order semantics -- take the per-pixel
    /// max across frames first, then one box average (`box(max ml)`), **not** "box each
    /// frame, then take the max" (`max(box ml)`). Regression round 4 mutated the box
    /// radius to 0 and P-C1..P-C6 stayed green, so the window branch had no anchor;
    /// this pins both the order and the radius branch.
    #[test]
    fn measure_peak_boxes_the_cross_frame_maximum_not_the_maximum_of_boxes() {
        // 两帧各一个孤立亮斑，水平相邻：同在半径 2 的 5×5 窗口内、分属不同帧。
        // 只有这种「相邻异帧」夹具能把 box(max) 与 max(box) 分开——box(max) 把
        // 两个斑累进同一个窗，max(box) 各自摊开再择一。
        //
        // Two frames, one isolated bright pixel each, horizontally adjacent: both fall
        // inside one radius-2 5x5 window yet belong to different frames. Only such a
        // fixture separates box(max) from max(box) -- box(max) accumulates both spikes
        // into the same window, max(box) spreads each and picks one.
        let w = 9;
        let h = 9;
        let mut a = vec![0.25f32; w * h * 3]; // γ2.0 亮度 0.5 / luma 0.5
        let mut b = a.clone();
        let spike = |f: &mut Vec<f32>, x: usize, y: usize, v: f32| {
            let p = (y * w + x) * 3;
            f[p] = v;
            f[p + 1] = v;
            f[p + 2] = v;
        };
        spike(&mut a, 4, 4, 0.81); // 亮度 0.9 / luma 0.9
        spike(&mut b, 5, 4, 0.81);

        let frames = [a.as_slice(), b.as_slice()];
        // 半径 0：就是逐帧 ML 的逐像素最大，一个值都不能动。
        //
        // Radius 0: exactly the per-pixel max of the per-frame MLs, untouched.
        let peak0 = measure_peak(&frames, w, h, 0);
        let ml_a = modified_laplacian(&a, w, h);
        let ml_b = modified_laplacian(&b, w, h);
        for i in 0..w * h {
            assert_eq!(peak0[i], ml_a[i].max(ml_b[i]), "pixel {i}");
        }
        // 半径 2：等于对「跨帧最大面」做一次盒平均——次序钉死。
        //
        // Radius 2: one box average over the cross-frame max plane -- order pinned.
        let peak2 = measure_peak(&frames, w, h, 2);
        let boxed_max = box_average(&peak0, w, h, 2);
        for i in 0..w * h {
            assert_eq!(peak2[i], boxed_max[i], "pixel {i}");
        }
        // 反序在同一点上给出不同的值：box(max) 累加两帧的斑，max(box) 择一。
        //
        // The reversed order gives a different value at the same pixel: box(max)
        // accumulates both frames' spikes, max(box) picks one.
        let box_a = box_average(&ml_a, w, h, 2);
        let box_b = box_average(&ml_b, w, h, 2);
        let p = 4 * w + 4;
        assert!(
            (peak2[p] - box_a[p].max(box_b[p])).abs() > 1e-6,
            "box(max) 与 max(box) 在该夹具上应当可分：{} vs {}",
            peak2[p],
            box_a[p].max(box_b[p])
        );
        // 半径 >0 真的在平均：与半径 0 的面在斑点上严格不同（窗口分支活着）。
        //
        // Radius > 0 really averages: strictly different from the radius-0 plane at
        // the spike (the window branch is alive).
        assert!(peak2[p] != peak0[p]);
    }
}
