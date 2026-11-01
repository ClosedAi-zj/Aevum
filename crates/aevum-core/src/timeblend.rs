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
    /// 混合前是否把来源帧的亮度对齐到参考帧。
    ///
    /// Whether to align each source frame's brightness to the reference before blending.
    pub match_exposure: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            layout: Layout::SkyGround,
            bands: 8,
            feather: 0.04,
            horizon: 0.5,
            match_exposure: true,
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
            for y in 0..h {
                let yf = y as f32 + 0.5;
                let row = y * w * 3;
                let mut wsum = 0.0f32;
                for (i, &(top, bottom)) in edges.iter().enumerate() {
                    let last = i + 1 == edges.len();
                    let wt = band_weight(yf, top, bottom, feather, i == 0, last);
                    if wt <= 0.0 {
                        continue;
                    }
                    let f = frames[pick(i)];
                    let g = gains[pick(i)] * wt;
                    for j in row..row + w * 3 {
                        out[j] += f[j] * g;
                    }
                    wsum += wt;
                }
                if wsum > 0.0 {
                    let inv = 1.0 / wsum;
                    for j in row..row + w * 3 {
                        out[j] *= inv;
                    }
                } else {
                    // 理论上不会发生（权重和恒为 1），留一条兜底以免整行全黑。
                    //
                    // Should not happen (weights sum to one); kept so a row can never go black.
                    let f = frames[pick(edges.len() / 2)];
                    out[row..row + w * 3].copy_from_slice(&f[row..row + w * 3]);
                }
            }
        }
        Layout::SkyGround => {
            let ground = frames[0];
            let sky = frames[count - 1];
            let gg = gains[0];
            let sg = gains[count - 1];
            let horizon = spec.horizon.clamp(0.0, 1.0) * h as f32;
            for y in 0..h {
                // 天空权重：地平线以上 1、以下 0，交界羽化。
                //
                // Sky weight: 1 above the horizon, 0 below, feathered across the boundary.
                let wt_sky = smoothstep(((y as f32 + 0.5) - horizon) / (2.0 * feather) + 0.5);
                let wt_ground = 1.0 - wt_sky;
                let row = y * w * 3;
                for x in 0..w {
                    let j = row + x * 3;
                    for c in 0..3 {
                        out[j + c] = sky[j + c] * sg * wt_sky + ground[j + c] * gg * wt_ground;
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 线性域里 1 EV 就是 ×2。/ One stop is ×2 in the linear domain.
    fn lin(ev_above_mid: f32) -> f32 {
        0.18 * 2f32.powf(ev_above_mid)
    }

    fn flat(w: usize, h: usize, v: f32) -> Vec<f32> {
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
        let ground = flat(4, 50, lin(0.0));
        let sky = flat(4, 50, lin(0.0));
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
        // 两帧同亮度时，输出必须处处等于它——羽化不能凭空造出亮暗。
        //
        // With both frames equal the output must equal them everywhere: feathering cannot invent
        // brightness.
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
}
