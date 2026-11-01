//! 星空 / 夜景堆栈：把一组同机位帧按统计量合成一张。
//!
//! 延时与星空的交叉地带一直缺一个工具：星轨要 **lighten**（取最亮，轨迹连成线）、
//! 降噪要 **average**（平均压随机噪声）、去卫星/飞机要 **median**（中值直接丢掉少数派）、
//! 而杂散光与热噪点要 **σ-clip**（先剔离群再平均）。StarStaX 只做前两样、Sequator 只有
//! Windows、两者都不读 RAW——用户得先在别处出 16bit TIFF。这里三件一起给。
//!
//! 全部在**线性 f32** 上做。这不是洁癖：gamma 域平均会让结果系统性变暗
//! （`mean(γ(x)) < γ(mean(x))`），星点与暗部最先糊掉——那正是"效果不理想"的常见来源。
//!
//! # Star-field stacking
//!
//! Star trails want **lighten** (max: trails connect), noise reduction wants **average**,
//! removing satellites/aircraft wants **median** (the minority is discarded outright), and
//! stray light plus hot pixels want **σ-clip** (drop outliers, then average). StarStaX ships
//! only the first two, Sequator is Windows-only, and neither reads RAW -- users have to produce
//! 16-bit TIFFs elsewhere first.
//!
//! Everything runs on **linear f32**. That is not fussiness: averaging in a gamma domain darkens
//! the result systematically (`mean(γ(x)) < γ(mean(x))`), and stars and shadows are the first to
//! smear -- exactly the usual source of "the result looks wrong".

/// 一次堆栈最多吃多少帧。中值是 O(N log N) 排序，64 帧已经是很深的栈了；
/// 再深就该先分段再合，而不是把一张 45MP 的帧乘上三位数的内存。
///
/// The most frames one stack accepts. Median sorts at O(N log N), and 64 is already a deep
/// stack; beyond that, stack in passes rather than multiply a 45MP frame by three digits of
/// memory.
pub const MAX_FRAMES: usize = 64;

/// 合成方式。
///
/// How the stack is combined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 取最亮：星轨连成线，云与车流也连成线。
    ///
    /// Take the brightest: star trails connect, and so do clouds and traffic.
    Lighten,
    /// 算术平均：随机噪声按 √N 下降，移动物体变成半透明的鬼影。
    ///
    /// Arithmetic mean: random noise falls as √N, moving objects become translucent ghosts.
    Average,
    /// 逐通道中值：少数派（卫星、飞机、单帧热噪）被整帧丢掉，代价是算力。
    ///
    /// Per-channel median: the minority (satellites, aircraft, single-frame hot pixels) is
    /// discarded outright, at the cost of compute.
    Median,
    /// σ 截尾平均：先按中值与 MAD 估出离散度，剔除超过 k·σ 的样本再平均。
    ///
    /// Sigma-clipped mean: estimate the spread from the median and MAD, drop samples beyond k·σ,
    /// then average what is left.
    SigmaClip,
}

impl Mode {
    /// 全部模式，界面按它铺按钮。
    ///
    /// Every mode, in the order the UI lays out its buttons.
    pub const ALL: [Mode; 4] = [Mode::Lighten, Mode::Average, Mode::Median, Mode::SigmaClip];

    /// 稳定的短键，落盘与命令面用它。
    ///
    /// Stable short key, used on disk and on the command surface.
    pub fn key(self) -> &'static str {
        match self {
            Mode::Lighten => "lighten",
            Mode::Average => "average",
            Mode::Median => "median",
            Mode::SigmaClip => "sigma-clip",
        }
    }

    /// 中文原名（可翻译的模板键）。
    ///
    /// Chinese source name, used as the translatable template key.
    pub fn cn(self) -> &'static str {
        match self {
            Mode::Lighten => "亮化",
            Mode::Average => "平均",
            Mode::Median => "中值",
            Mode::SigmaClip => "σ 截尾",
        }
    }

    /// 从短键解析。
    ///
    /// Parse from the short key.
    pub fn parse(raw: &str) -> Option<Mode> {
        let low = raw.trim().to_lowercase();
        Mode::ALL.into_iter().find(|m| m.key() == low)
    }
}

/// 堆栈参数。
///
/// Stack settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub mode: Mode,
    /// σ 截尾的倍数（只对 [`Mode::SigmaClip`] 生效）。默认 3.0。
    ///
    /// The σ multiplier (only for [`Mode::SigmaClip`]). Defaults to 3.0.
    pub clip_sigma: f32,
    /// 补洞：合成后仍为全黑的像素，用邻域非零值补上（星轨之间的空隙）。
    ///
    /// Gap fill: pixels that stay fully black after the merge take the mean of their non-zero
    /// neighbours (the gaps between star trails).
    pub fill_gaps: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Lighten,
            clip_sigma: 3.0,
            fill_gaps: true,
        }
    }
}

/// 堆栈失败的原因。
///
/// Why a stack failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackError {
    /// 一帧都没给。
    ///
    /// No frames were given.
    Empty,
    /// 尺寸不一致（调用方应该先对齐再进来）。
    ///
    /// Sizes differ (the caller is expected to align first).
    SizeMismatch,
    /// 尺寸为零。
    ///
    /// A zero dimension.
    ZeroSize,
}

impl StackError {
    /// 可翻译的模板键。
    ///
    /// Translatable template key.
    pub fn cn(self) -> &'static str {
        match self {
            StackError::Empty => "堆栈没有输入帧",
            StackError::SizeMismatch => "堆栈的帧尺寸不一致",
            StackError::ZeroSize => "堆栈尺寸为零",
        }
    }
}

/// 把多帧合成为一帧。输入是**已经对齐**的线性 RGB `f32`，长度必须等于 `w*h*3`。
///
/// Combine frames into one. Inputs are **already aligned** linear RGB `f32`, each `w*h*3` long.
pub fn combine(
    frames: &[&[f32]],
    w: usize,
    h: usize,
    opts: &Options,
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

    let mut out = vec![0.0f32; n];
    let mut vals = vec![0.0f32; frames.len()];

    for i in 0..n {
        for (k, f) in frames.iter().enumerate() {
            vals[k] = f[i];
        }
        out[i] = match opts.mode {
            Mode::Lighten => vals.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            Mode::Average => vals.iter().sum::<f32>() / frames.len() as f32,
            Mode::Median => median_of(&mut vals),
            Mode::SigmaClip => sigma_clip(&mut vals, opts.clip_sigma),
        };
    }

    if opts.fill_gaps {
        fill_holes(&mut out, w, h);
    }
    Ok(out)
}

/// 就地排一半的中值（样本数 ≤ 栈深，通常几十，排序完全够用）。
///
/// Median via a sort of the (typically tens-large) stack.
fn median_of(vals: &mut [f32]) -> f32 {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = vals.len() / 2;
    if vals.len() % 2 == 1 {
        vals[mid]
    } else {
        (vals[mid - 1] + vals[mid]) * 0.5
    }
}

/// σ 截尾平均：中值 + MAD 估离散度，剔掉 k·σ 之外的样本，剩下的取均值。
///
/// Sigma-clipped mean: median + MAD for the spread, drop samples beyond k·σ, average the rest.
fn sigma_clip(vals: &mut [f32], k: f32) -> f32 {
    let med = median_of(vals);
    let mut dev: Vec<f32> = vals.iter().map(|v| (v - med).abs()).collect();
    let mad = median_of(&mut dev);
    let sigma = 1.4826 * mad;
    if sigma <= 0.0 {
        // 全等（或全零，或非有限）：截尾没有意义，直接给中值。
        //
        // Everything identical (or all zero, or not finite): clipping means nothing, return the
        // median.
        return med;
    }
    let lim = k * sigma;
    let mut sum = 0.0;
    let mut cnt = 0usize;
    for v in vals.iter().copied() {
        if (v - med).abs() <= lim {
            sum += v;
            cnt += 1;
        }
    }
    if cnt == 0 { med } else { sum / cnt as f32 }
}

/// 把全黑像素用 3×3 邻域的非零均值补上（星轨之间断开的空隙）。
///
/// Fill fully black pixels from the non-zero mean of their 3×3 neighbourhood (the gaps between
/// star trails).
fn fill_holes(img: &mut [f32], w: usize, h: usize) {
    let src = img.to_vec();
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            if src[i] != 0.0 || src[i + 1] != 0.0 || src[i + 2] != 0.0 {
                continue;
            }
            let (mut acc, mut cnt) = ([0.0f32; 3], 0usize);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = (ny as usize * w + nx as usize) * 3;
                    if src[j] == 0.0 && src[j + 1] == 0.0 && src[j + 2] == 0.0 {
                        continue;
                    }
                    for c in 0..3 {
                        acc[c] += src[j + c];
                    }
                    cnt += 1;
                }
            }
            if cnt > 0 {
                for c in 0..3 {
                    img[i + c] = acc[c] / cnt as f32;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, v: f32) -> Vec<f32> {
        vec![v; w * h * 3]
    }

    #[test]
    fn lighten_keeps_the_brightest_sample_everywhere() {
        let a = frame(2, 2, 0.1);
        let mut b = frame(2, 2, 0.1);
        b[9] = 0.9; // 星点 / a star
        let opts = Options {
            mode: Mode::Lighten,
            fill_gaps: false,
            ..Options::default()
        };
        let out = combine(&[&a, &b], 2, 2, &opts).unwrap();
        assert_eq!(out[9], 0.9);
        assert_eq!(out[0], 0.1);
    }

    #[test]
    fn average_is_the_arithmetic_mean() {
        let a = frame(1, 1, 0.2);
        let b = frame(1, 1, 0.4);
        let opts = Options {
            mode: Mode::Average,
            fill_gaps: false,
            ..Options::default()
        };
        let out = combine(&[&a, &b], 1, 1, &opts).unwrap();
        for v in &out {
            assert!((v - 0.3).abs() < 1e-6, "{v}");
        }
    }

    #[test]
    fn median_discards_a_single_hot_frame() {
        // 九帧 0.2，其中一帧 1.0（卫星划过）——中值必须还原本底，平均则会被带走。
        //
        // Nine frames at 0.2 with one at 1.0 (a satellite): the median restores the base level
        // while the mean would be dragged away.
        let frames: Vec<Vec<f32>> = (0..9)
            .map(|i| frame(1, 1, if i == 4 { 1.0 } else { 0.2 }))
            .collect();
        let refs: Vec<&[f32]> = frames.iter().map(|f| f.as_slice()).collect();
        let med = combine(
            &refs,
            1,
            1,
            &Options {
                mode: Mode::Median,
                fill_gaps: false,
                ..Options::default()
            },
        )
        .unwrap();
        assert!((med[0] - 0.2).abs() < 1e-6, "中值 {}", med[0]);

        let avg = combine(
            &refs,
            1,
            1,
            &Options {
                mode: Mode::Average,
                fill_gaps: false,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(
            avg[0] > 0.25,
            "平均会被离群帧拽走（这正是要用中值的原因）：{}",
            avg[0]
        );
    }

    #[test]
    fn sigma_clip_drops_the_outlier_and_keeps_the_rest() {
        let mut vals: Vec<f32> = vec![0.10; 20];
        vals[7] = 0.95;
        let clipped = sigma_clip(&mut vals.clone(), 3.0);
        assert!(
            (clipped - 0.10).abs() < 1e-5,
            "截尾后应接近 0.10，实得 {clipped}"
        );
    }

    #[test]
    fn sigma_clip_is_not_a_no_op_on_a_uniform_stack() {
        let mut vals = vec![0.33f32; 8];
        assert!((sigma_clip(&mut vals, 3.0) - 0.33).abs() < 1e-6);
    }

    #[test]
    fn a_single_frame_passes_through_unchanged_in_every_mode() {
        let a: Vec<f32> = (0..12).map(|i| i as f32 / 12.0).collect();
        for mode in Mode::ALL {
            let out = combine(
                &[&a],
                2,
                2,
                &Options {
                    mode,
                    fill_gaps: false,
                    ..Options::default()
                },
            )
            .unwrap();
            assert_eq!(out, a, "{mode:?} 下单帧必须逐位不变");
        }
    }

    #[test]
    fn gap_fill_never_overwrites_real_pixels() {
        let mut a = frame(2, 2, 0.5);
        a[0] = 0.0;
        a[1] = 0.0;
        a[2] = 0.0;
        let out = combine(
            &[&a],
            2,
            2,
            &Options {
                mode: Mode::Lighten,
                fill_gaps: true,
                ..Options::default()
            },
        )
        .unwrap();
        // 全黑像素被邻域补上；原本非零的像素一个不动。
        //
        // The black pixel takes its neighbourhood; every non-black pixel is untouched.
        assert!(out[0] > 0.0, "空洞没补上");
        assert_eq!(out[3], 0.5);
        assert_eq!(out[9], 0.5);
    }

    #[test]
    fn degenerate_inputs_are_reported() {
        assert_eq!(
            combine(&[], 1, 1, &Options::default()),
            Err(StackError::Empty)
        );
        assert_eq!(
            combine(&[&[0.0; 3]], 0, 1, &Options::default()),
            Err(StackError::ZeroSize)
        );
        assert_eq!(
            combine(&[&[0.0; 3], &[0.0; 6]], 1, 1, &Options::default()),
            Err(StackError::SizeMismatch)
        );
    }

    #[test]
    fn mode_keys_round_trip() {
        for m in Mode::ALL {
            assert_eq!(Mode::parse(m.key()), Some(m));
        }
        assert_eq!(Mode::parse("nope"), None);
    }
}
