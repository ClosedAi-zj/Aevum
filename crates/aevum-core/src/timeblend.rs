//! 时间合成：把不同时刻拍下的画面，按区域合成为一张。
//!
//! 风光里最经典的做法是「天用日落后那张、地用最亮那张」，或者更极端的
//! **时间切片**：把画面横切成若干带，每一带取序列里不同时刻的一帧，天空的云因此
//! 被拉成一段连续的时间。PS 里这两件事都能做，但都要靠手：先分层、再刷蒙版、
//! 再接缝处拉曲线，换一组素材全部重来。
//!
//! 这里把它拆成三件可以分别验证的事：
//!
//! 1. **选帧**：按带（或按地平线分天地）决定每个区域来自哪一帧；
//! 2. **羽化**：带边界用 smoothstep 过渡，两个相邻带的权重恒等于 1（`s(t)+s(1-t)=1`），
//!    不会因为叠加而变亮或变暗；
//! 3. **曝光匹配**：不同时刻的帧本来差着档位，先把来源帧的亮度对齐到参考帧再混，
//!    否则接缝处一定是一道台阶。
//! 4. **去鬼影**：云、树、车流这类移动物在羽化区里会出现两份（同一物体在两个时刻
//!    位置不同）。照 ACR 多曝光合并那条链处理：低分辨率块均值 → 与 3×3 空间中位数
//!    的偏离（EV）→ 二值标签 → 软化边缘 → 混合时按标签衰减离群帧的权重，全剔光时
//!    兜底回权重最大的「素材帧」。
//!
//! 全部在**线性 f32** 上做。混合完再过显影链——绝不在每帧先调色，
//! 那等于把两条非线性曲线叠加，接缝再也对不齐。
//!
//! # Time compositing
//!
//! The classic landscape recipe is "sky from after sunset, ground from the brightest frame";
//! the extreme version is a **time slice**: cut the frame into horizontal bands, each taken from a
//! different moment of the sequence, so the clouds stretch into a continuous span of time. Both
//! are possible in PS, but only by hand: split layers, brush masks, then bend curves at the seam --
//! and start over for the next set of frames.
//!
//! Here it is split into three separately verifiable steps:
//!
//! 1. **Frame selection**: bands (or a sky/ground split at the horizon) decide where each region
//!    comes from;
//! 2. **Feathering**: band edges use a smoothstep transition whose pair of weights always sums to
//!    one (`s(t)+s(1-t)=1`), so overlapping never brightens or darkens the seam;
//! 3. **Exposure matching**: frames taken at different times sit stops apart; align the source
//!    frame's brightness to the reference before blending, or the seam is guaranteed to show a
//!    step.
//! 4. **Deghosting**: moving subjects (clouds, trees, traffic) double up inside the feather
//!    zone, because the same object sits in different places at two moments. Handled with the
//!    ACR multi-exposure chain: low-resolution tile means → deviation from the 3×3 spatial
//!    median (in EV) → binary labels → softened edges → the outlier's weight decays by its
//!    label during blending, falling back to the highest-weight "owner" frame when everything
//!    is flagged.
//!
//! All of it runs on **linear f32**, and the develop chain comes afterwards -- never grade each
//! frame first, which would stack two nonlinear curves and make the seam impossible to align.

use crate::stack::StackError;

/// 一次时间合成最多吃多少帧：带数上限是 32，再多每条带就只有几行像素，
/// 接缝会密到看不出「时间」而只剩条纹。
///
/// The most frames one time blend accepts: bands cap at 32, and beyond that each band is only a
/// few rows tall -- the seams crowd out the sense of time and leave stripes.
pub const MAX_FRAMES: usize = 32;

/// 合成的布局。
///
/// How the composite is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// 时间切片：横向分带，每带取不同时刻的一帧。
    ///
    /// Time slice: horizontal bands, each from a different moment.
    Bands,
    /// 天地：上半取最后一帧（通常是日落后），下半取第一帧（通常是地面还亮着时）。
    ///
    /// Sky and ground: the top comes from the last frame (usually after sunset) and the bottom
    /// from the first (when the ground was still lit).
    SkyGround,
}

impl Layout {
    /// 两种布局，界面按它铺选项。
    ///
    /// Both layouts, in the order the UI lists them.
    pub const ALL: [Layout; 2] = [Layout::SkyGround, Layout::Bands];

    /// 稳定的短键。
    ///
    /// Stable short key.
    pub fn key(self) -> &'static str {
        match self {
            Layout::Bands => "bands",
            Layout::SkyGround => "sky-ground",
        }
    }

    /// 中文原名（可翻译的模板键）。
    ///
    /// Chinese source name, used as the translatable template key.
    pub fn cn(self) -> &'static str {
        match self {
            Layout::Bands => "时间切片",
            Layout::SkyGround => "天地合成",
        }
    }

    /// 从短键解析。
    ///
    /// Parse from the short key.
    pub fn parse(raw: &str) -> Option<Layout> {
        let low = raw.trim().to_lowercase();
        Layout::ALL.into_iter().find(|l| l.key() == low)
    }
}

/// 合成参数。
///
/// Composite settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub layout: Layout,
    /// 带数（只对 [`Layout::Bands`] 生效），1 表示整幅取一帧。
    ///
    /// Number of bands (only for [`Layout::Bands`]); 1 means one frame for the whole image.
    pub bands: usize,
    /// 羽化宽度，相对整幅高度的比例（0–0.5）。
    ///
    /// Feather width as a fraction of the image height (0-0.5).
    pub feather: f32,
    /// 地平线位置，从顶部算起的比例（只对 [`Layout::SkyGround`] 生效）。
    ///
    /// Horizon position as a fraction from the top (only for [`Layout::SkyGround`]).
    pub horizon: f32,
    /// 自动推地平线：忽略 `horizon`，混合前按行亮度梯度找天空与地面的分界
    /// （找不到就用 `horizon` 兜底）。
    ///
    /// Suggest the horizon automatically: ignore `horizon` and find the sky-ground boundary
    /// from the per-row brightness gradient before blending (falling back to `horizon` when
    /// nothing is found).
    pub horizon_auto: bool,
    /// 混合前是否把来源帧的亮度对齐到参考帧。
    ///
    /// Whether to align each source frame's brightness to the reference before blending.
    pub match_exposure: bool,
    /// 去鬼影阈值（EV）：某帧某块与它自己 3×3 空间中位数的偏离超过这个值就算「离群」，
    /// 混合时把它的权重按标签衰减。0 = 关闭。默认 0.5 EV——足够抓住真实移动物，
    /// 又不会被噪声与压缩伪影骗到。
    ///
    /// The deghosting threshold (EV): a frame's tile counts as an outlier once its deviation
    /// from its own 3×3 spatial median passes this, and its weight decays by the label during
    /// blending. 0 = off. The 0.5 EV default catches real moving subjects without tripping on
    /// noise or compression artefacts.
    pub deghost: f32,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            layout: Layout::SkyGround,
            bands: 8,
            feather: 0.04,
            horizon: 0.5,
            horizon_auto: false,
            match_exposure: true,
            deghost: 0.5,
        }
    }
}

impl Spec {
    /// 这套参数下实际会用到几帧（越界时由调用方截断）。
    ///
    /// How many frames this spec actually needs (the caller clamps to what it has).
    pub fn frames_needed(&self) -> usize {
        match self.layout {
            Layout::SkyGround => 2,
            Layout::Bands => self.bands.max(1),
        }
    }
}

/// 每一条带的上下边界（以行为单位）。
///
/// The top and bottom row of every band.
fn band_edges(h: usize, bands: usize) -> Vec<(f32, f32)> {
    let bands = bands.max(1);
    (0..bands)
        .map(|i| {
            (
                h as f32 * i as f32 / bands as f32,
                h as f32 * (i + 1) as f32 / bands as f32,
            )
        })
        .collect()
}

/// 某一行在某个带里的权重（0–1）。相邻两带的权重之和恒为 1。
///
/// `first` / `last` 标记首带与末带：它们朝外的那条边是画面的边、不是接缝，
/// 那里权重必须恒为 1——否则整幅的顶端与底端会各留一条半透明的边
/// （顶上那一行只有 0.84 的权重，归一化后仍在，但那一行就再没有第二个带来补）。
///
/// The weight of one row inside one band (0-1). The weights of two adjacent bands always sum
/// to 1. `first` / `last` mark the outer bands: their outward edge is the frame border, not a
/// seam, so the weight there is a flat 1.
fn band_weight(y: f32, top: f32, bottom: f32, feather: f32, first: bool, last: bool) -> f32 {
    let f = feather.max(1.0);
    // 上沿 0→1，下沿 1→0；smoothstep 的对称性保证交界处两侧相加为 1。
    //
    // Ramp 0→1 at the top edge and 1→0 at the bottom edge; smoothstep's symmetry makes the two
    // sides of a boundary add up to one.
    let up = if first {
        1.0
    } else {
        smoothstep((y - (top - f)) / (2.0 * f))
    };
    let down = if last {
        1.0
    } else {
        smoothstep(((bottom + f) - y) / (2.0 * f))
    };
    up.min(down).clamp(0.0, 1.0)
}

/// 平滑阶跃 `3t²-2t³`，输入自动夹到 0–1。
///
/// Smoothstep `3t²-2t³`, input clamped to 0-1.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 一帧的亮度中位数（线性域，RGB 取平均）。用于曝光匹配。
///
/// The median brightness of one frame (linear, averaged over RGB). Used for exposure matching.
fn median_luma(img: &[f32]) -> f32 {
    let px = img.len() / 3;
    if px == 0 {
        return 0.0;
    }
    // 抽样排序：整幅排序没必要（一张 45MP 的帧要排 4500 万个数），
    // 均匀取到多 4096 个样本足够稳，而且不必先摊一份整幅的副本。
    //
    // Sample instead of sorting everything (a 45MP frame would sort 45 million numbers): up to
    // 4096 evenly spaced samples are stable enough, and no full-frame copy is needed.
    let step = (px / 4096).max(1);
    let mut s: Vec<f32> = (0..px)
        .step_by(step)
        .map(|i| (img[i * 3] + img[i * 3 + 1] + img[i * 3 + 2]) / 3.0)
        .collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[s.len() / 2]
}

/// 曝光匹配的增益：把来源帧对齐到参考帧，钳在 ±2 EV 之内。
///
/// The exposure-matching gain that aligns a source frame to the reference, clamped to ±2 EV.
fn match_gain(source: f32, reference: f32) -> f32 {
    // 非有限或近零的来源帧没有可用的比值，直接放弃匹配（宁可不匹配，也不要把它放大成一片噪点）。
    //
    // A non-finite or near-zero source has no usable ratio: skip matching rather than amplify it
    // into a field of noise.
    if !source.is_finite() || !reference.is_finite() || source <= 1e-6 || reference <= 1e-6 {
        return 1.0;
    }
    (reference / source).clamp(0.25, 4.0)
}

/// 自动推地平线：把所有帧逐行平均成一条亮度剖面，平滑后找相邻行差最大的地方——
/// 天空通常整片亮、地面整片暗，最强的纵向台阶就是地平线。返回从顶部算起的比例。
///
/// 找不到（画面太平、太小、没有帧）返回 `None`，调用方退回手动值。搜索窗口限制在
/// 画面高度的 10%–90%：贴边的台阶多半是暗角或裁切边，不是地平线。
///
/// Suggest the horizon: average every frame into one per-row brightness profile, smooth it,
/// and take the strongest step between adjacent rows -- sky is uniformly bright and ground
/// uniformly dark, so the strongest vertical step is the horizon. Returns the fraction from
/// the top, or `None` when nothing suggests one (flat frames, tiny frames, no frames) and the
/// caller falls back to the manual value. The search window is limited to 10%-90% of the
/// height: a step hugging the border is a vignette or a crop edge, not a horizon.
pub fn suggest_horizon(frames: &[&[f32]], w: usize, h: usize) -> Option<f32> {
    if frames.is_empty() || w == 0 || h < 8 {
        return None;
    }
    if frames.iter().any(|f| f.len() != w * h * 3) {
        return None;
    }
    // 逐行平均亮度（跨帧再平均，物影与单帧噪声被摊掉）。
    //
    // Per-row mean luma (averaged across frames too, so subject shadows and single-frame
    // noise wash out).
    let mut profile = vec![0.0f64; h];
    for f in frames {
        for y in 0..h {
            let row = y * w * 3;
            let mut sum = 0.0f64;
            for j in (row..row + w * 3).step_by(3) {
                sum += (f[j] + f[j + 1] + f[j + 2]) as f64;
            }
            profile[y] += sum / (w as f64);
        }
    }
    for v in profile.iter_mut() {
        *v /= frames.len() as f64;
    }
    // 平滑（半径 2 的滑动平均）：单行噪声不该赢过整片天与地之间的台阶。
    //
    // Smooth (a radius-2 moving average): one noisy row must not beat the sky-ground step.
    let smooth: Vec<f64> = (0..h)
        .map(|y| {
            let lo = y.saturating_sub(2);
            let hi = (y + 3).min(h);
            let n = (hi - lo) as f64;
            profile[lo..hi].iter().sum::<f64>() / n
        })
        .collect();
    let lo = (h / 10).max(1);
    let hi = h * 9 / 10;
    if lo >= hi {
        return None;
    }
    let (best_y, best_step) = (lo..hi)
        .map(|y| (y, (smooth[y + 1] - smooth[y]).abs()))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))?;
    // 两条门槛，都不过就交回 None 让调用方用手动值：
    // ① 完美台阶经半径 2 平滑后，相邻行差最多是总动态的 1/5；真实分界至少要拿到
    //   理想值的一半，否则这只是噪声在赢。
    // ② 天与地之间至少要差 0.5 EV——阴天平光画面里没有可用的分界。
    //
    // Two gates, both required before handing back None for the manual value:
    // (1) a perfect step smoothed at radius 2 spreads into adjacent diffs of at most 1/5 the
    //     total dynamic range; a real boundary must carry at least half of that ideal, or this
    //     is just noise winning;
    // (2) sky and ground must differ by at least 0.5 EV -- flat overcast has no usable
    //     boundary.
    let hi = smooth.iter().cloned().fold(f64::MIN, f64::max);
    let lo = smooth.iter().cloned().fold(f64::MAX, f64::min);
    let dynamic = hi - lo;
    let contrast_ok = hi.max(1e-9) / lo.max(1e-9) >= 2f64.powf(0.5);
    if dynamic <= 0.0 || best_step < dynamic / 5.0 * 0.5 || !contrast_ok {
        return None;
    }
    Some((best_y + 1) as f32 / h as f32)
}

/// 鬼影标签的分块边长（像素）：太大定位不准，太小会被噪声与压缩伪影骗到。
///
/// The tile edge (in pixels) for ghost labels: too large misses the subject, too small trips
/// on noise and compression artefacts.
const GHOST_TILE: usize = 16;

/// 每帧一份的鬼影标签图：低分辨率网格，每格 0–1（1 = 这一帧在这一格离群，已软化）。
///
/// One label grid per frame: a low-resolution grid of 0-1 values (1 = this frame is an
/// outlier in this tile, already softened).
struct GhostMaps {
    tw: usize,
    th: usize,
    per_frame: Vec<Vec<f32>>,
}

impl GhostMaps {
    /// 一个像素的保留系数：1 = 照常参与混合，0 = 这一帧在这里被剔掉。
    ///
    /// A pixel's keep factor: 1 blends as usual, 0 drops this frame at this pixel.
    fn keep(&self, frame: usize, x: usize, y: usize) -> f32 {
        1.0 - self.per_frame[frame][self.index(x, y)]
    }

    /// 像素坐标 → 格子下标（最近邻上采样：越界直接夹到边格）。
    ///
    /// Pixel coordinates → tile index (nearest-neighbour upsample: clamp at the border).
    fn index(&self, x: usize, y: usize) -> usize {
        let tx = (x / GHOST_TILE).min(self.tw - 1);
        let ty = (y / GHOST_TILE).min(self.th - 1);
        ty * self.tw + tx
    }
}

/// 造鬼影标签，照 ACR 多曝光合并那条链：低分辨率块均值 → 与 3×3 **空间**中位数的
/// 偏离（EV）→ 二值标签 → 3×3 软化（再对软化结果取 max，保住被标记的块不被自己
/// 的软化稀释）。网格太小（不足 3×3 格）时放弃，返回 `None`。
///
/// 参考用**空间**中位数而不是跨帧中位数：天地合成只有两帧，跨帧中位数就是平均值，
/// 一个移动物会让两帧同时离群、谁都剔不掉。空间中位数只看本帧自己的邻域——
/// 静态的地平线、云边在两帧的同位置一起离群 → 两帧都标 → 全剔 → 兜底回权重最大的
/// 素材帧，输出不变；而只在单帧出现的物体（鬼影）只有它离群，被干净帧顶替。
///
/// Build the ghost labels along the ACR multi-exposure chain: low-resolution tile means →
/// deviation from the 3×3 **spatial** median (in EV) → binary labels → 3×3 softening (a max
/// against the softened values keeps a flagged tile from being diluted by its own blur). Grids
/// smaller than 3×3 tiles give up and return `None`.
///
/// The reference is **spatial**, not cross-frame: a sky-ground blend has two frames, so the
/// cross-frame median is just the mean and one moving subject flags both frames at once. The
/// spatial median only looks at a frame's own neighbourhood -- a static horizon or cloud edge
/// is an outlier in both frames at the same place, so both get flagged, everything drops, and
/// the blend falls back to the highest-weight owner frame unchanged; a subject present in one
/// frame only (a ghost) is an outlier there alone and gets replaced by the clean frame.
fn build_ghost_maps(
    frames: &[&[f32]],
    w: usize,
    h: usize,
    gains: &[f32],
    threshold: f32,
) -> Option<GhostMaps> {
    let tw = w / GHOST_TILE;
    let th = h / GHOST_TILE;
    if tw < 3 || th < 3 {
        return None;
    }
    // 每帧每格的平均亮度（曝光匹配之后的世界：均值直接乘增益）。
    //
    // Mean luma per tile per frame (in the exposure-matched world: the mean scales by the gain).
    let area = (GHOST_TILE * GHOST_TILE) as f32;
    let means: Vec<Vec<f32>> = frames
        .iter()
        .enumerate()
        .map(|(fi, f)| {
            let mut m = vec![0.0f32; tw * th];
            for y in 0..th * GHOST_TILE {
                let ty = y / GHOST_TILE;
                for x in 0..tw * GHOST_TILE {
                    let j = (y * w + x) * 3;
                    m[ty * tw + x / GHOST_TILE] += (f[j] + f[j + 1] + f[j + 2]) / 3.0;
                }
            }
            for v in m.iter_mut() {
                *v = *v / area * gains[fi];
            }
            m
        })
        .collect();

    let labels: Vec<Vec<f32>> = means
        .iter()
        .map(|m| {
            let mut lab = vec![0.0f32; tw * th];
            // 边格也参与：邻域用夹边取，一样能判。边框上如果有真实内容
            // （暗角、边框），两帧会在同位置一起离群 → 全剔 → 兜底帧，输出不变。
            //
            // Border tiles take part too, with a clamped neighbourhood. Real border content
            // (vignette, frame line) is an outlier in both frames at the same place →
            // everything drops → owner fallback, output unchanged.
            for ty in 0..th {
                for tx in 0..tw {
                    let mut nb: Vec<f32> = Vec::with_capacity(9);
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            let ny = (ty as i32 + dy).clamp(0, th as i32 - 1) as usize;
                            let nx = (tx as i32 + dx).clamp(0, tw as i32 - 1) as usize;
                            nb.push(m[ny * tw + nx]);
                        }
                    }
                    nb.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let reference = nb[4];
                    let own = m[ty * tw + tx];
                    // 非有限或近黑的格子没有可比的比值，不算离群——判不出就不冤枉。
                    //
                    // A non-finite or near-black tile has no comparable ratio: not an outlier.
                    let dev = if reference > 1e-6 && own.is_finite() {
                        (own / reference).log2().abs()
                    } else {
                        0.0
                    };
                    if dev > threshold {
                        lab[ty * tw + tx] = 1.0;
                    }
                }
            }
            soften_labels(&lab, tw, th)
        })
        .collect();
    Some(GhostMaps {
        tw,
        th,
        per_frame: labels,
    })
}

/// 软化标签边缘：3×3 盒滤波，再对原值取 max——被标记的块保持完整剔除，
/// 只有它周围一圈拿到部分系数，边界不再是一刀切的硬边。
///
/// Soften label edges: a 3×3 box filter maxed against the original -- flagged tiles keep their
/// full exclusion and only the ring around them picks up partial factors, so the edge is not a
/// hard cut.
fn soften_labels(labels: &[f32], tw: usize, th: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; labels.len()];
    for ty in 0..th {
        for tx in 0..tw {
            let mut sum = 0.0f32;
            let mut n = 0u32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let ny = ty as i32 + dy;
                    let nx = tx as i32 + dx;
                    if ny < 0 || nx < 0 || ny as usize >= th || nx as usize >= tw {
                        continue;
                    }
                    sum += labels[ny as usize * tw + nx as usize];
                    n += 1;
                }
            }
            let i = ty * tw + tx;
            out[i] = labels[i].max(sum / n as f32);
        }
    }
    out
}

/// 把一行按贡献帧的权重混进 `out`。权重可被鬼影标签衰减（`keep`）；
/// 全部剔光时兜底到基础权重最大的「素材帧」——比输出全黑强。
///
/// Blend one row into `out` with per-frame weights, decayed by the ghost labels (`keep`).
/// When everything is flagged, fall back to the highest-weight owner frame -- better than a
/// black row.
fn blend_row(
    out: &mut [f32],
    frames: &[&[f32]],
    gains: &[f32],
    y: usize,
    w: usize,
    contrib: &[(usize, f32)],
    ghosts: Option<&GhostMaps>,
) {
    let row = y * w * 3;
    let owner = contrib
        .iter()
        .copied()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(fi, _)| fi)
        .unwrap_or(0);
    for x in 0..w {
        let j = row + x * 3;
        let mut acc = [0.0f32; 3];
        let mut wsum = 0.0f32;
        for &(fi, wt) in contrib {
            let keep = match ghosts {
                Some(gm) => gm.keep(fi, x, y),
                None => 1.0,
            };
            if keep <= 0.0 {
                continue;
            }
            let g = gains[fi] * wt * keep;
            acc[0] += frames[fi][j] * g;
            acc[1] += frames[fi][j + 1] * g;
            acc[2] += frames[fi][j + 2] * g;
            wsum += wt * keep;
        }
        if wsum > 0.0 {
            out[j] = acc[0] / wsum;
            out[j + 1] = acc[1] / wsum;
            out[j + 2] = acc[2] / wsum;
        } else {
            let f = frames[owner];
            out[j] = f[j];
            out[j + 1] = f[j + 1];
            out[j + 2] = f[j + 2];
        }
    }
}

/// 合成。输入是**已经对齐**的线性 RGB `f32`，长度必须等于 `w*h*3`；
/// `frames[0]` 视为「早」（地面还亮），`frames[len-1]` 视为「晚」（天空最好）。
///
/// Composite. Inputs are **already aligned** linear RGB `f32`, each `w*h*3`; `frames[0]` is
/// treated as "early" (ground still lit) and `frames[len-1]` as "late" (best sky).
pub fn time_slice(
    frames: &[&[f32]],
    w: usize,
    h: usize,
    spec: &Spec,
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

    let feather = (spec.feather.clamp(0.0, 0.5) * h as f32).max(0.5);
    let ref_luma = median_luma(frames[count / 2]);

    // 每一帧的增益先算好，逐帧再乘；比在像素循环里反复算便宜得多。
    //
    // Precompute one gain per frame; far cheaper than recomputing it inside the pixel loop.
    let gains: Vec<f32> = frames
        .iter()
        .map(|f| {
            if spec.match_exposure {
                match_gain(median_luma(f), ref_luma)
            } else {
                1.0
            }
        })
        .collect();

    // 去鬼影开着才算标签图：关着时零开销走老路。
    //
    // Build the label maps only when deghosting is on; off means the old zero-overhead path.
    let ghosts = if spec.deghost > 0.0 && count >= 2 {
        build_ghost_maps(frames, w, h, &gains, spec.deghost)
    } else {
        None
    };

    let mut out = vec![0.0f32; n];
    match spec.layout {
        Layout::Bands => {
            let bands = spec.bands.max(1).min(count);
            let edges = band_edges(h, bands);
            // 带 i 取第 i 帧（从早到晚铺满整幅），上下都要羽化。
            //
            // Band i takes frame i (early to late down the frame), feathered on both sides.
            let pick = |i: usize| -> usize {
                if bands == 1 {
                    0
                } else {
                    i * (count - 1) / (bands - 1)
                }
            };
            let mut contrib: Vec<(usize, f32)> = Vec::with_capacity(edges.len());
            for y in 0..h {
                let yf = y as f32 + 0.5;
                contrib.clear();
                for (i, &(top, bottom)) in edges.iter().enumerate() {
                    let wt = band_weight(yf, top, bottom, feather, i == 0, i + 1 == edges.len());
                    if wt > 0.0 {
                        contrib.push((pick(i), wt));
                    }
                }
                blend_row(&mut out, frames, &gains, y, w, &contrib, ghosts.as_ref());
            }
        }
        Layout::SkyGround => {
            // 自动地平线：按行亮度梯度找天与地的分界，找不到退回手动值。
            //
            // Auto horizon: find the sky-ground boundary from the per-row brightness gradient,
            // falling back to the manual value when nothing is found.
            let horizon_frac = if spec.horizon_auto {
                suggest_horizon(frames, w, h).unwrap_or(spec.horizon)
            } else {
                spec.horizon
            }
            .clamp(0.0, 1.0);
            let horizon = horizon_frac * h as f32;
            for y in 0..h {
                // 天空权重：地平线以上（y 小）1、以下 0，交界羽化。分子必须是
                // horizon − y：写成 y − horizon 会把天与地对调（上一版的真 bug，
                // 旧判据全用同内容帧，方向错了也测不出来）。
                //
                // Sky weight: 1 above the horizon (small y), 0 below, feathered across the
                // boundary. The numerator must be horizon - y; writing y - horizon swaps sky
                // and ground (a real bug in the previous version that the old identical-frame
                // signals could not see).
                let wt_sky = smoothstep((horizon - (y as f32 + 0.5)) / (2.0 * feather) + 0.5);
                let wt_ground = 1.0 - wt_sky;
                let contrib = [(count - 1, wt_sky), (0, wt_ground)];
                blend_row(&mut out, frames, &gains, y, w, &contrib, ghosts.as_ref());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 线性域里 1 EV 就是 ×2。/ One stop is ×2 in the linear domain.
    pub(super) fn lin(ev_above_mid: f32) -> f32 {
        0.18 * 2f32.powf(ev_above_mid)
    }

    pub(super) fn flat(w: usize, h: usize, v: f32) -> Vec<f32> {
        vec![v; w * h * 3]
    }

    #[test]
    fn band_weights_always_sum_to_one_across_a_boundary() {
        // 这是羽化的核心不变量：两侧相加为 1，否则接缝处会整体变亮或变暗。
        //
        // The core feathering invariant: the two sides add up to one, otherwise the seam would
        // brighten or darken as a whole.
        let f = 6.0;
        for y in 0..40 {
            let yf = y as f32 + 0.5;
            let a = band_weight(yf, 0.0, 20.0, f, true, false);
            let b = band_weight(yf, 20.0, 40.0, f, false, true);
            assert!((a + b - 1.0).abs() < 1e-5, "y={y}: {a} + {b}");
        }
    }

    #[test]
    fn a_single_band_is_the_identity() {
        let f: Vec<f32> = (0..4 * 3 * 3).map(|i| i as f32 / 100.0).collect();
        let out = time_slice(
            &[&f],
            4,
            3,
            &Spec {
                layout: Layout::Bands,
                bands: 3,
                ..Spec::default()
            },
        )
        .unwrap();
        assert_eq!(out, f, "只有一帧时必须逐位不变");
    }

    #[test]
    fn time_slice_puts_early_frames_on_top_and_late_ones_at_the_bottom() {
        let early = flat(2, 40, lin(-2.0));
        let late = flat(2, 40, lin(1.0));
        let out = time_slice(
            &[&early, &late],
            2,
            40,
            &Spec {
                layout: Layout::Bands,
                bands: 2,
                feather: 0.02,
                match_exposure: false,
                ..Spec::default()
            },
        )
        .unwrap();
        let top = out[0];
        let bottom = out[40 * 2 * 3 - 3];
        assert!((top - lin(-2.0)).abs() < 1e-4, "顶部应是早帧：{top}");
        assert!((bottom - lin(1.0)).abs() < 1e-4, "底部应是晚帧：{bottom}");
    }

    #[test]
    fn sky_ground_splits_at_the_horizon_without_a_step() {
        // 首帧整幅是「地面亮度」，末帧整幅是「天空亮度」：哪一帧铺在上面一目了然。
        //
        // The first frame is uniformly "ground bright" and the last uniformly "sky bright":
        // which one ends up on top is unmistakable.
        let ground = flat(4, 50, lin(0.0));
        let sky = flat(4, 50, lin(2.0));
        let out = time_slice(
            &[&ground, &sky],
            4,
            50,
            &Spec {
                layout: Layout::SkyGround,
                horizon: 0.5,
                feather: 0.04,
                match_exposure: false,
                ..Spec::default()
            },
        )
        .unwrap();
        // 方向：顶部必须来自末帧（天），底部必须来自首帧（地）。
        //
        // Direction: the top must come from the last frame (sky), the bottom from the first
        // (ground).
        assert!(
            (out[0] - lin(2.0)).abs() < 1e-5,
            "顶部应是天空帧：{}",
            out[0]
        );
        let last = out.len() - 3;
        assert!(
            (out[last] - lin(0.0)).abs() < 1e-5,
            "底部应是地面帧：{}",
            out[last]
        );
        // 同亮度帧的旧要求仍然成立：羽化不能凭空造出亮暗。
        //
        // The old equal-frame requirement still holds: feathering cannot invent brightness.
        let equal = flat(4, 50, lin(0.0));
        let out = time_slice(
            &[&equal, &equal],
            4,
            50,
            &Spec {
                layout: Layout::SkyGround,
                horizon: 0.5,
                feather: 0.04,
                match_exposure: false,
                ..Spec::default()
            },
        )
        .unwrap();
        for v in &out {
            assert!((v - lin(0.0)).abs() < 1e-5, "{v}");
        }
    }

    #[test]
    fn exposure_matching_removes_the_seam_a_stop_apart_two_frames_would_show() {
        // 地面 0.18、天空 0.36（差 1 EV）。不匹配时接缝处是 0.18 的台阶；匹配之后应几乎为零。
        //
        // Ground 0.18, sky 0.36 (one stop apart). Unmatched leaves a 0.18 step at the seam;
        // matched it should be nearly zero.
        let ground = flat(4, 80, lin(0.0));
        let sky = flat(4, 80, lin(1.0));
        let horizon_row = 40usize;
        let at = |img: &[f32], y: usize| img[y * 4 * 3];

        let unmatched = time_slice(
            &[&ground, &sky],
            4,
            80,
            &Spec {
                layout: Layout::SkyGround,
                horizon: 0.5,
                feather: 0.01,
                match_exposure: false,
                ..Spec::default()
            },
        )
        .unwrap();
        let step_unmatched = (at(&unmatched, horizon_row) - at(&unmatched, horizon_row - 1)).abs();
        assert!(
            step_unmatched > 0.1,
            "不匹配时接缝本就该有明显台阶（否则这条判据测不到东西）：{step_unmatched}"
        );

        let matched = time_slice(
            &[&ground, &sky],
            4,
            80,
            &Spec {
                layout: Layout::SkyGround,
                horizon: 0.5,
                feather: 0.01,
                match_exposure: true,
                ..Spec::default()
            },
        )
        .unwrap();
        let step_matched = (at(&matched, horizon_row) - at(&matched, horizon_row - 1)).abs();
        assert!(
            step_matched < 0.02,
            "曝光匹配后接缝不该还有台阶：{step_matched}（未匹配时 {step_unmatched}）"
        );
    }

    #[test]
    fn exposure_matching_is_clamped_so_a_black_frame_cannot_blow_up() {
        // 全黑来源帧：增益不该变成无穷大。
        //
        // An all-black source must not turn the gain into infinity.
        assert_eq!(match_gain(0.0, 0.5), 1.0);
        assert_eq!(match_gain(0.5, 0.0), 1.0);
        let g = match_gain(0.001, 1.0);
        assert!((0.25..=4.0).contains(&g), "{g}");
    }

    #[test]
    fn degenerate_inputs_are_reported() {
        let a = flat(2, 2, 0.1);
        assert_eq!(
            time_slice(&[], 2, 2, &Spec::default()),
            Err(StackError::Empty)
        );
        assert_eq!(
            time_slice(&[&a], 0, 2, &Spec::default()),
            Err(StackError::ZeroSize)
        );
        assert_eq!(
            time_slice(&[&a, &[0.0; 6]], 2, 2, &Spec::default()),
            Err(StackError::SizeMismatch)
        );
    }

    /// 一只在单帧里移动的物体：去鬼影关着时羽化区里是一团半亮鬼影，
    /// 开着时同一位置回到干净帧的背景亮度。
    ///
    /// A subject that moves in one frame: with deghosting off the feather zone holds a
    /// half-bright ghost; with it on, the same spot returns to the clean frame's background.
    #[test]
    fn a_moved_subject_leaves_no_ghost_in_the_feather_zone() {
        let w = 48;
        let h = 48;
        let bg = lin(0.0);
        let obj = lin(2.0);
        let mut early = flat(w, h, bg);
        let mut late = flat(w, h, bg);
        // 物体在 early 的左中（整格），在 late 的右中：羽化区（y≈24）里两份都在。
        //
        // The subject sits mid-left in `early` and mid-right in `late`: both copies fall in the
        // feather zone (y≈24).
        let stamp = |img: &mut Vec<f32>, x0: usize| {
            for y in 16..32 {
                for x in x0..x0 + 16 {
                    let j = (y * w + x) * 3;
                    img[j] = obj;
                    img[j + 1] = obj;
                    img[j + 2] = obj;
                }
            }
        };
        stamp(&mut early, 0);
        stamp(&mut late, 32);
        let spec = |deghost: f32| Spec {
            layout: Layout::Bands,
            bands: 2,
            feather: 0.2,
            match_exposure: false,
            deghost,
            ..Spec::default()
        };
        // 探针：early 的物体格中心、羽化带正中间的一行。
        //
        // Probe: the centre of early's subject tile, on a row in the middle of the feather band.
        let probe = 24 * w * 3 + 8 * 3;
        let off = time_slice(&[&early, &late], w, h, &spec(0.0)).unwrap();
        let on = time_slice(&[&early, &late], w, h, &spec(0.5)).unwrap();
        // 反向验证：阈值 0（关闭）时鬼影必须真的在——否则判据测了个寂寞。
        //
        // Negative control: with the threshold at 0 (off) the ghost must really be there,
        // otherwise this signal proves nothing.
        assert!(
            off[probe] > bg * 1.5,
            "关闭去鬼影时应有鬼影：{} vs bg {}",
            off[probe],
            bg
        );
        assert!(
            (on[probe] - bg).abs() < 0.02 * bg,
            "开启后鬼影应被剔掉：{} vs bg {}",
            on[probe],
            bg
        );
    }

    /// 静态内容（地平线、两张同拍）会被两帧同时标记，全剔之后兜底回素材帧，
    /// 输出与去鬼影关闭时一致。
    ///
    /// Static content (a horizon, two identical frames) gets flagged in both frames at once;
    /// after everything drops the owner fallback keeps the output identical to deghosting off.
    #[test]
    fn a_clean_stack_is_unchanged_by_deghosting() {
        let w = 48;
        let h = 48;
        // 上下两块平色：每个格子对它自己的空间中位数都可能有大偏离（块边界穿过格子），
        // 但两帧在同一位置一起离群 → 全剔 → 兜底帧。
        //
        // Two flat halves: tiles can deviate from their own spatial median (the edge crosses
        // tiles), but both frames deviate at the same places → everything drops → owner
        // fallback.
        let mut a = flat(w, h, lin(-1.0));
        for j in (0..w * (h / 2) * 3).step_by(3) {
            a[j] = lin(1.5);
            a[j + 1] = lin(1.5);
            a[j + 2] = lin(1.5);
        }
        let b = a.clone();
        let spec = |deghost: f32| Spec {
            layout: Layout::Bands,
            bands: 2,
            match_exposure: false,
            deghost,
            ..Spec::default()
        };
        let off = time_slice(&[&a, &b], w, h, &spec(0.0)).unwrap();
        let on = time_slice(&[&a, &b], w, h, &spec(0.5)).unwrap();
        assert_eq!(on, off, "静态内容不该被去鬼影改动");
    }

    /// 两帧在同一位置都有同一件物体：两帧一起被标 → 兜底回素材帧 → 物体保住。
    ///
    /// The same subject in the same place in both frames: both get flagged → owner fallback →
    /// the subject survives.
    #[test]
    fn a_shared_subject_survives_through_the_owner_fallback() {
        let w = 48;
        let h = 48;
        let mut a = flat(w, h, lin(0.0));
        let mut b = flat(w, h, lin(0.0));
        for img in [&mut a, &mut b] {
            for y in 16..32 {
                for x in 16..32 {
                    let j = (y * w + x) * 3;
                    img[j] = lin(2.0);
                    img[j + 1] = lin(2.0);
                    img[j + 2] = lin(2.0);
                }
            }
        }
        let out = time_slice(
            &[&a, &b],
            w,
            h,
            &Spec {
                layout: Layout::Bands,
                bands: 2,
                feather: 0.2,
                match_exposure: false,
                deghost: 0.5,
                ..Spec::default()
            },
        )
        .unwrap();
        let probe = 24 * w * 3 + 24 * 3;
        assert!(out[probe] > lin(1.5), "共享物体不该被剔掉：{}", out[probe]);
    }

    /// 去鬼影只在画面足够大（至少 3×3 格）时工作；小图直接走普通混合，不出错。
    ///
    /// Deghosting only runs when the frame is at least 3×3 tiles; small frames fall through to
    /// the plain blend without errors.
    #[test]
    fn deghosting_skips_frames_too_small_for_a_grid() {
        let a = flat(32, 32, lin(0.0));
        let b = flat(32, 32, lin(0.5));
        let out = time_slice(
            &[&a, &b],
            32,
            32,
            &Spec {
                layout: Layout::Bands,
                bands: 2,
                deghost: 0.5,
                ..Spec::default()
            },
        )
        .unwrap();
        assert!(out.iter().all(|v| v.is_finite()), "小图也要产出有限值");
        // 32×32 只有 2×2 格：标签图造不出来，输出应与去鬼影关闭完全一致。
        //
        // 32×32 is only 2×2 tiles: no label map, so the output must match deghosting off
        // exactly.
        let plain = time_slice(
            &[&a, &b],
            32,
            32,
            &Spec {
                layout: Layout::Bands,
                bands: 2,
                deghost: 0.0,
                ..Spec::default()
            },
        )
        .unwrap();
        assert_eq!(out, plain);
    }
}

#[cfg(test)]
mod horizon_tests {
    use super::tests::{flat, lin};
    use super::*;

    /// 亮天压着暗地：建议的地平线应落在真实分界附近（一行以内）。
    ///
    /// Bright sky over dark ground: the suggested horizon lands within a row of the real
    /// boundary.
    #[test]
    fn the_suggested_horizon_finds_a_bright_sky_over_dark_ground() {
        let w = 8;
        let h = 48;
        let split = 15usize; // 行 0..15 是天，15..48 是地 / rows 0..15 sky, 15..48 ground
        let mut img = vec![lin(-2.0); w * h * 3];
        for y in 0..split {
            for j in (y * w * 3)..((y + 1) * w * 3) {
                img[j] = lin(1.5);
            }
        }
        let got = suggest_horizon(&[&img], w, h).expect("应该找得到地平线");
        let want = split as f32 / h as f32;
        assert!(
            (got - want).abs() < 1.0 / h as f32,
            "建议 {got} 应贴着真实分界 {want}"
        );
    }

    /// 平光画面（阴天、纯天空）没有分界：返回 None 让调用方退回手动值，
    /// 而不是瞎指一条线。贴边的台阶（暗角、裁切边）不在搜索窗口里，同样交回 None。
    ///
    /// A flat frame (overcast, pure sky) has no boundary: None, so the caller keeps the manual
    /// value instead of pointing at nothing. A step hugging the border (vignette, crop edge)
    /// sits outside the search window: None again.
    #[test]
    fn a_flat_frame_suggests_nothing() {
        let img = flat(8, 48, lin(0.2));
        assert_eq!(suggest_horizon(&[&img], 8, 48), None);
        let mut edged = flat(8, 48, lin(0.0));
        for j in 0..8 * 3 {
            edged[j] = lin(3.0);
        }
        assert_eq!(suggest_horizon(&[&edged], 8, 48), None);
    }

    /// 自动地平线找到后，天地合成的分界真的落在那里：素材在 30% 处有天/地分界，
    /// 输出在 30% 之上应是天、50% 附近应是地——而不是围着默认的 50% 转。
    ///
    /// With the horizon found automatically, the split really lands there: on a scene whose
    /// boundary sits at 30%, the output is sky above 30% and ground near 50% -- not centred on
    /// the default 50%.
    #[test]
    fn auto_horizon_moves_the_split_to_the_detected_boundary() {
        let w = 8;
        let h = 40;
        let split = 12usize; // 30%
        // 两帧都带着同一条 30% 的天/地分界（剖面因此找得到），但天与地的亮度都
        // 随时间变了（真实延时正是如此）：分界若真搬到 30%，30%–50% 之间的行
        // 取自首帧（暗地 lin(-1)）；若还停在默认 50%，那里按天混合（亮得多）。
        //
        // Both frames carry the same 30% sky-ground boundary (so the profile finds it), but
        // sky and ground both change brightness over time (as in a real time-lapse): if the
        // split really moves to 30%, the rows between 30% and 50% come from the first frame
        // (dark ground lin(-1)); stuck at the default 50%, they blend sky (much brighter).
        let mut early = vec![lin(-1.0); w * h * 3];
        let mut late = vec![lin(0.5); w * h * 3];
        for y in 0..split {
            for j in (y * w * 3)..((y + 1) * w * 3) {
                early[j] = lin(1.0);
                late[j] = lin(2.5);
            }
        }
        let at = |out: &[f32], row: usize| out[row * w * 3];
        let auto = time_slice(
            &[&early, &late],
            w,
            h,
            &Spec {
                layout: Layout::SkyGround,
                horizon_auto: true,
                feather: 0.05,
                match_exposure: false,
                ..Spec::default()
            },
        )
        .unwrap();
        // 天空区取末帧（更亮的那帧），证明「天取末帧」。
        //
        // The sky region takes the last frame (the brighter one), proving "sky from the last".
        assert!(
            at(&auto, 6) > lin(2.0),
            "30% 之上应是末帧的天：{}",
            at(&auto, 6)
        );
        assert!(
            at(&auto, 19) < lin(-0.5),
            "30%–50% 之间应是首帧的地（分界搬过来了）：{}",
            at(&auto, 19)
        );
        // 反向验证：关掉自动、停在默认 50% 时，同一行按天混合，明显更亮。
        //
        // Negative control: with auto off the split stays at 50%, so the same row blends sky
        // and is clearly brighter.
        let manual = time_slice(
            &[&early, &late],
            w,
            h,
            &Spec {
                layout: Layout::SkyGround,
                horizon_auto: false,
                horizon: 0.5,
                feather: 0.05,
                match_exposure: false,
                ..Spec::default()
            },
        )
        .unwrap();
        assert!(
            at(&manual, 19) > lin(0.0),
            "停在 50% 时该行应按天混合：{}",
            at(&manual, 19)
        );
    }

    /// 退化输入不 panic、不瞎指。
    ///
    /// Degenerate inputs neither panic nor point at anything.
    #[test]
    fn degenerate_horizon_inputs_return_none() {
        let a = flat(8, 4, lin(0.3));
        assert_eq!(suggest_horizon(&[], 8, 48), None);
        assert_eq!(suggest_horizon(&[&a], 8, 4), None, "图太小，剖面立不起来");
        let wrong = flat(7, 48, lin(0.3));
        assert_eq!(suggest_horizon(&[&wrong], 8, 48), None);
    }
}
