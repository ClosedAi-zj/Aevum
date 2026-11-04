//! 序列去闪烁：逐帧测光 → 稳健平滑 → 曝光偏移（EV）。
//!
//! 口径照搬 Python 实现（`acceptance/reference/deflicker.py`）：中值压单帧跳变，高斯平滑避免
//! 校正后出现台阶，偏移单位等于调色台的曝光 EV。`acceptance/oracle/golden/deflicker.json`
//! 是同一算法的冻结输出，单测按 1e-4 比对。
//!
//! Sequence deflicker: per-frame metering → robust smoothing → exposure offset (EV).
//!
//! The convention follows the Python implementation (`acceptance/reference/deflicker.py`):
//! a median suppresses single-frame jumps, Gaussian smoothing keeps the correction from
//! introducing steps, and the offset unit equals the exposure EV of the grading console.
//! `acceptance/oracle/golden/deflicker.json` is the frozen output of the same algorithm,
//! and the unit tests compare against it at 1e-4.

use std::path::{Path, PathBuf};

use image::RgbImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub const WINDOW_MIN: usize = 3;
pub const WINDOW_MAX: usize = 199;
/// 单帧最多补几档：再大就不是闪烁而是坏帧。
///
/// Most stops a single frame can be corrected by: anything beyond that is not flicker but
/// a bad frame.
pub const MAX_EV: f64 = 3.0;
/// sRGB 亮度系数，通道顺序 R、G、B。
///
/// sRGB luma coefficients, with the channels in R, G, B order.
pub const LUMA_RGB: [f64; 3] = [0.2126, 0.7152, 0.0722];
/// 测光只读缩略图：整段序列的耗时几乎全在读图，长边 320 足够定出趋势。
///
/// Metering reads thumbnails only: nearly all the time over a sequence goes to decoding,
/// and a 320 long edge is enough to establish the trend.
pub const SCAN_LONG: u32 = 320;

/// 一帧的线性光测光：整帧亮度与 R/G/B 分通道各一份。
///
/// 与胶片条用的 gamma 域平均亮度（`metrics::FrameMetrics`）是两套口径：
/// 去闪要在线性光里做截尾均值，否则明暗跳会被 sRGB 编码压扁，压不住彩色闪烁。
///
/// Linear-light metering of one frame: one whole-frame luma value plus one value per
/// R/G/B channel.
///
/// This is a separate convention from the gamma-domain mean luma used by filmstrips
/// (`metrics::FrameMetrics`): deflicker needs the trimmed mean in linear light, because
/// sRGB encoding flattens brightness jumps and would leave color flicker uncorrected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameMeans {
    /// 线性亮度的 1%~99% 截尾均值，0~1。
    ///
    /// 1% to 99% trimmed mean of linear luma, 0 to 1.
    pub luma: f64,
    /// R、G、B 各自的截尾均值，0~1。
    ///
    /// Trimmed mean of each of R, G and B, 0 to 1.
    pub rgb: [f64; 3],
}

/// 256 级 sRGB→线性查表；与 Python 的 `_SRGB_LUT` 同为 float64 算完再降 f32。
///
/// 256-entry sRGB to linear lookup table; like Python's `_SRGB_LUT`, the values are
/// computed in float64 and then narrowed to f32.
pub fn srgb_lut() -> [f32; 256] {
    let mut table = [0.0f32; 256];
    for (i, v) in table.iter_mut().enumerate() {
        let x = i as f64 / 255.0;
        *v = if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        } as f32;
    }
    table
}

/// 1%~99% 截尾均值：抗坏点与高光，压不住整帧的明暗跳。全被裁掉时退回普通均值。
///
/// 1% to 99% trimmed mean: resists hot pixels and highlights, but cannot flatten a
/// whole-frame brightness jump. Falls back to the plain mean when everything is cut.
pub fn trimmed_mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let lo = quantile_sorted(&sorted, 0.01);
    let hi = quantile_sorted(&sorted, 0.99);
    let mut sum = 0.0;
    let mut kept = 0usize;
    for v in values {
        if *v >= lo && *v <= hi {
            sum += v;
            kept += 1;
        }
    }
    if kept == 0 {
        values.iter().sum::<f64>() / values.len() as f64
    } else {
        sum / kept as f64
    }
}

/// sRGB 像素 -> 线性光测光。调用方负责先把长边缩到 `SCAN_LONG`。
///
/// sRGB pixels to linear-light metering. The caller is responsible for shrinking the long
/// edge to `SCAN_LONG` first.
pub fn measure_image(img: &RgbImage) -> Option<FrameMeans> {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let lut = srgb_lut();
    let n = (w as usize) * (h as usize);
    let mut chans = [
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    ];
    let mut lum = Vec::with_capacity(n);
    for p in img.pixels() {
        let mut lin = [0.0f64; 3];
        for (c, v) in lin.iter_mut().enumerate() {
            *v = lut[p.0[c] as usize] as f64;
            chans[c].push(*v);
        }
        lum.push(lin[0] * LUMA_RGB[0] + lin[1] * LUMA_RGB[1] + lin[2] * LUMA_RGB[2]);
    }
    Some(FrameMeans {
        luma: trimmed_mean(&lum),
        rgb: [
            trimmed_mean(&chans[0]),
            trimmed_mean(&chans[1]),
            trimmed_mean(&chans[2]),
        ],
    })
}

/// 读一张图并测光；读不了返回 None，绝不把异常抛给整段分析。
///
/// Reads one image and meters it; an unreadable file returns None, and no error is ever
/// thrown at the whole analysis.
pub fn measure_path(path: impl AsRef<Path>) -> Option<FrameMeans> {
    let img = crate::metrics::decode_rgb(path, SCAN_LONG).ok()?;
    measure_image(&img)
}

/// 并行测光；返回与 `paths` 等长的表和 (帧号, 错误) 列表。
///
/// Meters in parallel; returns a table as long as `paths` plus a list of (frame number,
/// error) pairs.
pub fn measure_many(paths: &[PathBuf]) -> (Vec<Option<FrameMeans>>, Vec<(usize, String)>) {
    let results: Vec<(usize, Option<FrameMeans>)> = paths
        .par_iter()
        .enumerate()
        .map(|(i, p)| (i, measure_path(p)))
        .collect();
    let mut out = vec![None; paths.len()];
    let mut issues = Vec::new();
    for (i, m) in results {
        match m {
            Some(m) => out[i] = Some(m),
            None => issues.push((i, format!("测光读取失败：{}", paths[i].display()))),
        }
    }
    issues.sort_by_key(|(i, _)| *i);
    (out, issues)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// 相邻帧亮度 P95 差，单位 EV。
    ///
    /// P95 luma difference between neighbouring frames, in EV.
    pub jitter_before: f64,
    pub jitter_after: f64,
    /// 分通道时三个通道里最抖的那个，校正前后。
    ///
    /// When splitting by channel, the most jittery of the three channels, before and
    /// after correction.
    pub color_before: f64,
    pub color_after: f64,
    pub channels: bool,
    pub max_ev: f64,
    pub mean_abs_ev: f64,
    pub frames: usize,
    pub valid: usize,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    /// 行优先的 n×cols 偏移表，单位 EV，正 = 提亮。
    ///
    /// Row-major n×cols offset table in EV, where positive means brighten.
    pub offsets: Vec<f64>,
    pub cols: usize,
    pub stats: Stats,
}

impl Analysis {
    pub fn rows(&self) -> usize {
        self.offsets.len().checked_div(self.cols).unwrap_or(0)
    }

    pub fn offset(&self, row: usize, col: usize) -> f64 {
        self.offsets[row * self.cols + col]
    }

    /// 单通道结果的逐帧偏移；分通道时取三通道平均。
    ///
    /// Per-frame offset of a single-channel result; when splitting by channel, the three
    /// channels are averaged.
    pub fn exposure_per_frame(&self) -> Vec<f64> {
        (0..self.rows())
            .map(|i| {
                let s: f64 = (0..self.cols).map(|c| self.offset(i, c)).sum();
                s / self.cols.max(1) as f64
            })
            .collect()
    }

    /// 逐帧的分通道残差（各通道偏移 − 三通道均值）。
    ///
    /// 整帧曝光吃掉均值、这里只留色差，两者相加正好还原该通道的偏移；
    /// 单通道结果全为 0，于是与只补亮度的老口径逐位一致。
    ///
    /// Per-frame per-channel residual (each channel offset minus the three-channel mean).
    ///
    /// The whole-frame exposure carries the mean and only the color difference is left
    /// here; adding the two restores that channel's offset exactly. A single-channel
    /// result is all zeros, so it matches the older brightness-only convention bit for
    /// bit.
    pub fn chan_residual_per_frame(&self) -> Vec<[f64; 3]> {
        (0..self.rows())
            .map(|i| {
                if self.cols < 3 {
                    return [0.0; 3];
                }
                let v = [self.offset(i, 0), self.offset(i, 1), self.offset(i, 2)];
                let mean = (v[0] + v[1] + v[2]) / 3.0;
                [v[0] - mean, v[1] - mean, v[2] - mean]
            })
            .collect()
    }
}

/// 单通道（亮度）去闪烁。`None` 表示这一帧读不出来，按插值补洞。
///
/// Single-channel (luma) deflicker. `None` marks a frame that could not be read, and the
/// gap is filled by interpolation.
pub fn analyze_luma(means: &[Option<f64>], window: f64, strength: f64, multi: bool) -> Analysis {
    let matrix = Matrix::from_luma(means);
    analyze(&matrix, window, strength, multi)
}

/// 分通道（R/G/B）去闪烁，用于压彩色场景的色闪。
///
/// Per-channel (R/G/B) deflicker, used to suppress color flicker in colorful scenes.
pub fn analyze_rgb(
    means: &[Option<[f64; 3]>],
    window: f64,
    strength: f64,
    multi: bool,
) -> Analysis {
    let matrix = Matrix::from_rgb(means);
    analyze(&matrix, window, strength, multi)
}

/// 全部有效 EV 的中位数——预对齐的锚点。比取第一帧稳：坏帧、缺帧都不挪锚。
///
/// The median of all valid EVs, the anchor of EXIF pre-alignment. Steadier than the first
/// frame: bad and missing frames never move the anchor.
fn exif_anchor(evs: &[Option<f64>]) -> Option<f64> {
    let mut valid: Vec<f64> = evs
        .iter()
        .filter_map(|e| e.filter(|v| v.is_finite()))
        .collect();
    if valid.is_empty() {
        return None;
    }
    valid.sort_by(f64::total_cmp);
    let mid = valid.len() / 2;
    Some(if valid.len() % 2 == 1 {
        valid[mid]
    } else {
        (valid[mid - 1] + valid[mid]) / 2.0
    })
}

/// EXIF 预对齐（圣杯过渡）：把每帧亮度折算到「同一组拍摄参数下会看到的亮度」，在
/// 场景域跑一遍去闪，再把预对齐增益**并回 offsets**。
///
/// 日转夜延时里相机在不停补偿（拉长曝光 / 开大光圈 / 提 ISO），测光器看到的是
/// 「场景光 × 相机补偿」，去闪器分不清哪部分是真实的光线变化、哪部分是相机在动——
/// 结果要么把台阶当闪烁压掉（吃掉日落），要么把真实变暗当闪烁压掉（吃掉日夜过渡）。
/// 对齐用 EXIF 三要素的相对 EV 把相机补偿剥掉：`aligned = luma × 2^(EV − EV_ref)`，
/// `EV_ref` 取全部有效 EV 的中位数（比取第一帧稳：坏帧、缺帧都不挪锚）。剥完之后
/// 趋势就是场景光本身——台阶没了，日落还在。
///
/// **为什么增益必须并回 offsets（而不是只在对齐域跑完就算）**：渲染管线只会
/// 「原始帧 × 2^corr」。观众要看到的是场景域的画面 = 对齐帧 × 2^offset，于是
/// `corr = offset + (EV − EV_ref)`——这个和最大可到整个圣杯跨度（8 EV 级），是这次
/// 导出里**真实发生的重曝光**。只在对齐域算完不并回，渲染根本拿不到这层校正，
/// 输出还是被测光拉平的画面（判据
/// `the_aligned_output_restores_the_day_night_transition_the_meter_ate` 咬的就是这个）。
/// 增益在 [`analyze`] **之后**并回，不经过单帧 ±[`MAX_EV`] 的限幅——限幅管的是
/// 「压闪烁别过头」，管不着「本来就计划好的重曝光」。
///
/// 由此 `Stats` 的口径变化：`jitter_before/after` 仍在**场景域**报告（对齐后显著变小，
/// 正是收益所在）；`max_ev` / `mean_abs_ev` 是**总校正**（含计划内的重曝光），圣杯
/// 序列上超过 [`MAX_EV`] 是预期，不是坏帧。
///
/// 缺 EV 的帧**不对齐也不猜**（与 `exif::ev` 缺三要素返回 `None` 同一哲学）：带着未剥离
/// 的台阶进分析会造成假跳变，所以按洞处理，交给去闪插值；那张帧自己保持原样（校正
/// 为 0）。一个 EV 都没有时整体退回普通去闪——开关等于没开，而不是造出一个假对齐。
///
/// 锚点取**中位数**还有一层用处：并回后的总校正中位数为 0，整条片子不会被预对齐顺手
/// 整体提亮或压暗——对齐只重分配每帧的曝光，不改总曝光。
///
/// EXIF pre-alignment (the holy-grail transition): rescale every frame's brightness to what
/// the same exposure settings would have seen, run deflicker in that scene domain, then fold
/// the alignment gain **back into the offsets**.
///
/// In a day-to-night timelapse the camera compensates constantly (longer shutter, wider
/// aperture, higher ISO), and the meter sees "scene light × camera compensation" -- deflicker
/// cannot tell real light change from camera motion, so it either eats the steps (killing the
/// sunset) or eats the real darkening (killing the day-night transition). Alignment strips
/// the compensation with the relative EV from the EXIF triplet:
/// `aligned = luma × 2^(EV − EV_ref)`, anchored at the median of all valid EVs (steadier
/// than the first frame: bad and missing frames never move the anchor). The trend then IS
/// the scene light -- the steps are gone and the sunset stays.
///
/// **Why the gain must be folded back into the offsets** (rather than done in the aligned
/// domain and forgotten): the render pipeline can only multiply the **original** frame by
/// 2^corr. What the viewer should see is the scene-domain picture = aligned frame × 2^offset,
/// so `corr = offset + (EV − EV_ref)` -- a sum that reaches the whole holy-grail span
/// (8 EV class) and is the **real re-exposure** this export performs. Skipping the fold
/// means the render never sees that layer and the output stays the meter-flattened picture
/// (assertion `the_aligned_output_restores_the_day_night_transition_the_meter_ate` bites
/// exactly this). The gain is folded after [`analyze`], outside the per-frame ±[`MAX_EV`]
/// clamp -- the clamp governs "don't over-suppress flicker", not a planned re-exposure.
///
/// Consequence for `Stats`: `jitter_before/after` stay in the **scene domain** (notably
/// smaller after alignment -- that is the whole win); `max_ev` / `mean_abs_ev` are the
/// **total** correction including the planned re-exposure, so exceeding [`MAX_EV`] on a
/// holy-grail sequence is expected, not a bad frame.
///
/// A frame without an EV is **not aligned and not guessed** (the same philosophy as
/// `exif::ev` returning `None` when a leg is missing): an unstripped step would enter the
/// analysis as a false jump, so it becomes a hole for the interpolator, and that frame stays
/// untouched (correction 0). With no EV at all this falls back to plain deflicker -- the
/// switch becomes a no-op rather than a fake alignment.
///
/// The median anchor has a second job: the folded total correction has a median of zero, so
/// pre-alignment never brightens or darkens the whole clip -- it redistributes per-frame
/// exposure without touching total exposure.
pub fn analyze_luma_exif(
    means: &[Option<f64>],
    evs: &[Option<f64>],
    window: f64,
    strength: f64,
    multi: bool,
) -> Analysis {
    let Some(anchor) = exif_anchor(evs) else {
        return analyze_luma(means, window, strength, multi);
    };
    let aligned = align_luma_to_exif(means, evs, anchor);
    let mut a = analyze_luma(&aligned, window, strength, multi);
    fold_gain_into_offsets(&mut a.offsets, means, evs, anchor);
    a
}

/// 分通道版的 EXIF 预对齐：三个通道乘**同一个**增益——相机补偿不挑颜色，把它剥掉
/// 是整帧的事；色闪照旧由分通道去闪处理。口径与 [`analyze_luma_exif`] 相同。
///
/// The per-channel variant: all three channels scale by **the same** gain -- camera
/// compensation is color-blind, so stripping it is a whole-frame operation; color flicker
/// remains the per-channel deflicker's job. Same conventions as [`analyze_luma_exif`].
pub fn analyze_rgb_exif(
    means: &[Option<[f64; 3]>],
    evs: &[Option<f64>],
    window: f64,
    strength: f64,
    multi: bool,
) -> Analysis {
    let Some(anchor) = exif_anchor(evs) else {
        return analyze_rgb(means, window, strength, multi);
    };
    let aligned = align_rgb_to_exif(means, evs, anchor);
    let mut a = analyze_rgb(&aligned, window, strength, multi);
    fold_gain_into_offsets(&mut a.offsets, means, evs, anchor);
    a
}

/// 把预对齐增益（`EV − anchor`，log2 域）并进每行的总校正；有 EV 的帧才并——洞帧的
/// 校正保持 0（那张帧不动）。多通道时一行三列并同一个增益：补偿不挑颜色。
///
/// Folds the alignment gain (`EV − anchor`, log2 domain) into each row's total correction;
/// only frames that have an EV get the fold -- a hole's correction stays 0 (that frame is
/// left alone). With multiple columns one row folds the same gain into all of them:
/// compensation is color-blind.
fn fold_gain_into_offsets<T>(
    offsets: &mut [f64],
    means: &[Option<T>],
    evs: &[Option<f64>],
    anchor: f64,
) {
    for (i, off) in offsets.iter_mut().enumerate() {
        if let (Some(Some(_)), Some(ev)) = (
            means.get(i),
            evs.get(i).and_then(|e| e.filter(|v| v.is_finite())),
        ) {
            *off += ev - anchor;
        }
    }
}

/// EXIF 预对齐的第一步：`aligned = luma × 2^(EV − anchor)`。缺 EV 的帧变洞。
///
/// Step one of EXIF pre-alignment. A frame without an EV becomes a hole.
fn align_luma_to_exif(means: &[Option<f64>], evs: &[Option<f64>], anchor: f64) -> Vec<Option<f64>> {
    means
        .iter()
        .zip(evs.iter())
        .map(|(m, e)| match (m, e) {
            (Some(v), Some(ev)) if ev.is_finite() => Some(v * (ev - anchor).exp2()),
            _ => None,
        })
        .collect()
}

/// 分通道版：三个通道乘**同一个**因子。
///
/// The per-channel variant: all three channels scale by the same factor.
fn align_rgb_to_exif(
    means: &[Option<[f64; 3]>],
    evs: &[Option<f64>],
    anchor: f64,
) -> Vec<Option<[f64; 3]>> {
    means
        .iter()
        .zip(evs.iter())
        .map(|(m, e)| match (m, e) {
            (Some(v), Some(ev)) if ev.is_finite() => {
                Some([v[0], v[1], v[2]].map(|c| c * (ev - anchor).exp2()))
            }
            _ => None,
        })
        .collect()
}

struct Matrix {
    rows: usize,
    cols: usize,
    data: Vec<f64>,
    valid: Vec<bool>,
}

impl Matrix {
    fn from_luma(v: &[Option<f64>]) -> Self {
        let rows = v.len();
        let mut data = vec![f64::NAN; rows];
        let mut valid = vec![false; rows];
        for (i, x) in v.iter().enumerate() {
            if let Some(x) = x
                && x.is_finite()
                && *x > 0.0
            {
                data[i] = *x;
                valid[i] = true;
            }
        }
        Self {
            rows,
            cols: 1,
            data,
            valid,
        }
    }

    fn from_rgb(v: &[Option<[f64; 3]>]) -> Self {
        let rows = v.len();
        let mut data = vec![f64::NAN; rows * 3];
        let mut valid = vec![false; rows];
        for (i, x) in v.iter().enumerate() {
            if let Some(x) = x {
                let ok = x.iter().all(|c| c.is_finite() && *c > 0.0);
                for (c, val) in x.iter().enumerate() {
                    data[i * 3 + c] = *val;
                }
                valid[i] = ok;
            }
        }
        Self {
            rows,
            cols: 3,
            data,
            valid,
        }
    }

    fn get(&self, row: usize, col: usize) -> f64 {
        self.data[row * self.cols + col]
    }

    /// 每列各自线性插值补洞；三列都好的帧才算有效帧。
    /// 与 `np.interp` 一致：超出两端时取端点值。
    fn filled(&self) -> Matrix {
        let mut data = self.data.clone();
        for c in 0..self.cols {
            let good: Vec<usize> = (0..self.rows)
                .filter(|&i| {
                    let v = self.get(i, c);
                    v.is_finite() && v > 0.0
                })
                .collect();
            if good.is_empty() {
                continue;
            }
            for i in 0..self.rows {
                let v = self.get(i, c);
                if v.is_finite() && v > 0.0 {
                    continue;
                }
                data[i * self.cols + c] = interp(i, &good, |k| self.get(k, c));
            }
        }
        Matrix {
            rows: self.rows,
            cols: self.cols,
            data,
            valid: self.valid.clone(),
        }
    }
}

fn interp(x: usize, xs: &[usize], at: impl Fn(usize) -> f64) -> f64 {
    match xs.binary_search(&x) {
        Ok(k) => at(xs[k]),
        Err(0) => at(xs[0]),
        Err(k) if k >= xs.len() => at(*xs.last().unwrap()),
        Err(k) => {
            let (x0, x1) = (xs[k - 1], xs[k]);
            let (y0, y1) = (at(x0), at(x1));
            let t = (x as f64 - x0 as f64) / (x1 as f64 - x0 as f64);
            y0 + (y1 - y0) * t
        }
    }
}

fn analyze(m: &Matrix, window: f64, strength: f64, multi: bool) -> Analysis {
    let n = m.rows;
    let cols = m.cols;
    let amount = if strength.is_finite() {
        (strength / 100.0).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let w = odd_window(window, n);
    let valid = m.valid.iter().filter(|v| **v).count();
    if n == 0 {
        return Analysis {
            offsets: Vec::new(),
            cols,
            stats: Stats::default(),
        };
    }
    let filled = m.filled();
    if w == 0 || valid < 3 {
        return Analysis {
            offsets: vec![0.0; n * cols],
            cols,
            stats: Stats {
                frames: n,
                valid,
                ..Default::default()
            },
        };
    }

    let mut log = vec![0.0; n * cols];
    for i in 0..n {
        for c in 0..cols {
            log[i * cols + c] = filled.get(i, c).max(1e-7).log2();
        }
    }
    let mut ev = vec![0.0; n * cols];
    for c in 0..cols {
        let col: Vec<f64> = (0..n).map(|i| log[i * cols + c]).collect();
        let mut acc = vec![0.0; n];
        for win in pass_windows(w, multi) {
            let resid: Vec<f64> = (0..n).map(|i| col[i] + acc[i]).collect();
            let target = gauss_edge(&median_edge(&resid, win), (win as f64 / 4.0).max(0.8));
            for i in 0..n {
                acc[i] = (acc[i] + (target[i] - resid[i]) * amount).clamp(-MAX_EV, MAX_EV);
            }
        }
        for i in 0..n {
            ev[i * cols + c] = if m.valid[i] { acc[i] } else { 0.0 };
        }
    }

    let stats = stats(&filled, &ev, cols, n);
    Analysis {
        offsets: ev,
        cols,
        stats,
    }
}

fn stats(filled: &Matrix, ev: &[f64], cols: usize, n: usize) -> Stats {
    let valid = filled.valid.iter().filter(|v| **v).count();
    let mut s = Stats {
        channels: cols > 1,
        frames: n,
        valid,
        ..Default::default()
    };
    s.max_ev = ev.iter().fold(0.0f64, |a, b| a.max(b.abs()));
    let moved: Vec<f64> = ev.iter().map(|v| v.abs()).filter(|v| *v > 1e-9).collect();
    s.mean_abs_ev = if moved.is_empty() {
        0.0
    } else {
        moved.iter().sum::<f64>() / moved.len() as f64
    };

    let log_cols: Vec<Vec<f64>> = (0..cols)
        .map(|c| (0..n).map(|i| filled.get(i, c).max(1e-7).log2()).collect())
        .collect();
    if valid == 0 {
        return s;
    }
    if cols > 1 {
        s.color_before = (0..cols)
            .map(|c| jitter(&log_cols[c]))
            .fold(0.0f64, f64::max);
        let after_cols: Vec<Vec<f64>> = (0..cols)
            .map(|c| (0..n).map(|i| log_cols[c][i] + ev[i * cols + c]).collect())
            .collect();
        s.color_after = after_cols.iter().map(|v| jitter(v)).fold(0.0f64, f64::max);
        let lum_before: Vec<f64> = (0..n)
            .map(|i| {
                let v: f64 = (0..3)
                    .map(|c| filled.get(i, c).max(1e-7) * LUMA_RGB[c])
                    .sum();
                v.log2()
            })
            .collect();
        let lum_after: Vec<f64> = (0..n)
            .map(|i| {
                let v: f64 = (0..3)
                    .map(|c| (log_cols[c][i] + ev[i * cols + c]).exp2().max(1e-7) * LUMA_RGB[c])
                    .sum();
                v.log2()
            })
            .collect();
        s.jitter_before = jitter(&lum_before);
        s.jitter_after = jitter(&lum_after);
    } else if n >= 2 {
        s.jitter_before = jitter(&log_cols[0]);
        let after: Vec<f64> = (0..n).map(|i| log_cols[0][i] + ev[i]).collect();
        s.jitter_after = jitter(&after);
    }
    s
}

fn pass_windows(w: usize, multi: bool) -> Vec<usize> {
    if !multi || w <= 3 {
        vec![w]
    } else {
        vec![3, w]
    }
}

fn odd_window(window: f64, n: usize) -> usize {
    let mut w = if window.is_finite() {
        window.round() as i64
    } else {
        15
    };
    w = w.clamp(WINDOW_MIN as i64, WINDOW_MAX as i64);
    if w % 2 == 0 {
        w += 1;
    }
    let n = n as i64;
    if w > n {
        w = if n % 2 == 1 { n } else { n - 1 };
    }
    if w >= WINDOW_MIN as i64 {
        w as usize
    } else {
        0
    }
}

/// 边界复制的中值滤波。
fn median_edge(values: &[f64], window: usize) -> Vec<f64> {
    let n = values.len();
    if window < 3 || n == 0 {
        return values.to_vec();
    }
    let pad = window / 2;
    let mut out = vec![0.0; n];
    let mut buf = Vec::with_capacity(window);
    for i in 0..n {
        buf.clear();
        for k in 0..window {
            let idx = i as isize + k as isize - pad as isize;
            let idx = idx.clamp(0, n as isize - 1) as usize;
            buf.push(values[idx]);
        }
        buf.sort_by(f64::total_cmp);
        out[i] = buf[window / 2];
    }
    out
}

/// 边界复制的高斯平滑，sigma 与半径规则同 Python。
fn gauss_edge(values: &[f64], sigma: f64) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    let radius = ((sigma * 3.0).round() as usize).max(1);
    let mut kernel = Vec::with_capacity(2 * radius + 1);
    let mut sum = 0.0;
    for k in 0..=(2 * radius) {
        let x = k as f64 - radius as f64;
        let v = (-0.5 * (x / sigma).powi(2)).exp();
        kernel.push(v);
        sum += v;
    }
    for v in kernel.iter_mut() {
        *v /= sum;
    }
    (0..n)
        .map(|i| {
            let mut acc = 0.0;
            for (k, kv) in kernel.iter().enumerate() {
                let idx = i as isize + k as isize - radius as isize;
                let idx = idx.clamp(0, n as isize - 1) as usize;
                acc += values[idx] * kv;
            }
            acc
        })
        .collect()
}

/// 相邻帧差的 P95，线性插值分位数（同 numpy 默认）。
fn jitter(log_values: &[f64]) -> f64 {
    if log_values.len() < 2 {
        return 0.0;
    }
    let mut diffs: Vec<f64> = log_values.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
    percentile(&mut diffs, 0.95)
}

fn percentile(values: &mut [f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    quantile_sorted(values, q)
}

/// 已排序序列的线性插值分位数（同 numpy 默认）。
fn quantile_sorted(sorted: &[f64], q: f64) -> f64 {
    let idx = (sorted.len() - 1) as f64 * q;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] + (sorted[hi] - sorted[lo]) * (idx - lo as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_sequence_gets_no_correction() {
        let means: Vec<Option<f64>> = (0..20).map(|_| Some(0.2)).collect();
        let a = analyze_luma(&means, 15.0, 100.0, false);
        assert_eq!(a.offsets.len(), 20);
        assert!(a.offsets.iter().all(|v| v.abs() < 1e-12));
        assert_eq!(a.stats.valid, 20);
    }

    #[test]
    fn one_dark_frame_is_lifted() {
        let mut means: Vec<Option<f64>> = (0..21).map(|_| Some(0.2)).collect();
        means[10] = Some(0.1);
        let a = analyze_luma(&means, 15.0, 100.0, false);
        assert!(
            a.offset(10, 0) > 0.9,
            "暗帧应提亮，实得 {}",
            a.offset(10, 0)
        );
        assert!(a.stats.jitter_after < a.stats.jitter_before);
    }

    #[test]
    fn missing_frames_are_interpolated_and_left_alone() {
        let mut means: Vec<Option<f64>> = (0..11).map(|_| Some(0.2)).collect();
        means[5] = None;
        let a = analyze_luma(&means, 15.0, 100.0, false);
        assert_eq!(a.offset(5, 0), 0.0);
        assert_eq!(a.stats.valid, 10);
    }

    #[test]
    fn even_window_becomes_odd_and_clamps_to_length() {
        assert_eq!(odd_window(15.0, 40), 15);
        assert_eq!(odd_window(16.0, 40), 17);
        assert_eq!(odd_window(15.0, 9), 9);
        assert_eq!(odd_window(15.0, 8), 7);
        assert_eq!(odd_window(15.0, 2), 0);
        assert_eq!(odd_window(1.0, 40), 3);
        assert_eq!(odd_window(999.0, 400), 199);
    }

    #[test]
    fn rgb_analysis_reports_color_jitter() {
        let mut means: Vec<Option<[f64; 3]>> = (0..21).map(|_| Some([0.1, 0.12, 0.14])).collect();
        means[9] = Some([0.1, 0.2, 0.14]);
        let a = analyze_rgb(&means, 15.0, 100.0, false);
        assert_eq!(a.cols, 3);
        assert!(a.stats.color_before > 0.0);
        assert!(a.stats.color_after < a.stats.color_before);
    }

    #[test]
    fn trimmed_mean_cuts_both_tails() {
        // 0..=99：1% 分位 0.99、99% 分位 98.01，留下 1..=98，均值 49.5。
        let v: Vec<f64> = (0..100).map(|i| i as f64).collect();
        assert!((trimmed_mean(&v) - 49.5).abs() < 1e-12);
        // 单个亮点被裁掉，整帧明暗裁不掉。
        let mut hot = v.clone();
        hot[50] = 9000.0;
        assert!((trimmed_mean(&hot) - 50.0).abs() < 1e-9);
        // 全同值（含 0）走 kept==全部 这条路；空表中规矩地给 0。
        assert_eq!(trimmed_mean(&[0.25; 8]), 0.25);
        assert_eq!(trimmed_mean(&[7.0]), 7.0);
        assert_eq!(trimmed_mean(&[]), 0.0);
    }

    #[test]
    fn measure_image_uses_the_linear_lut() {
        let lut = srgb_lut();
        // 纯色小图：截尾裁不掉任何东西，结果正好是查表值。
        let img = RgbImage::from_pixel(4, 3, image::Rgb([10, 20, 30]));
        let m = measure_image(&img).expect("纯色图该有测光");
        let want = [lut[10] as f64, lut[20] as f64, lut[30] as f64];
        assert_eq!(m.rgb, want);
        assert!(
            (m.luma - (want[0] * LUMA_RGB[0] + want[1] * LUMA_RGB[1] + want[2] * LUMA_RGB[2]))
                .abs()
                < 1e-12,
            "亮度该是线性光的 Rec.709 加权和，实得 {}",
            m.luma
        );
        assert_eq!(measure_image(&RgbImage::new(0, 1)), None);
    }

    #[test]
    fn channel_residual_reconstructs_the_offset() {
        let mut means: Vec<Option<[f64; 3]>> = (0..21).map(|_| Some([0.1, 0.12, 0.14])).collect();
        means[9] = Some([0.1, 0.2, 0.14]);
        let a = analyze_rgb(&means, 15.0, 100.0, false);
        let expo = a.exposure_per_frame();
        let chan = a.chan_residual_per_frame();
        assert_eq!(chan.len(), a.rows());
        for (i, c) in chan.iter().enumerate() {
            assert!((c[0] + c[1] + c[2]).abs() < 1e-12, "残差不该带亮度分量");
            for (k, v) in c.iter().enumerate() {
                assert!(
                    (expo[i] + v - a.offset(i, k)).abs() < 1e-12,
                    "[{i}][{k}] 曝光 + 残差没还原偏移"
                );
            }
        }
        // 只补亮度时没有色闪残差，走的就是老口径。
        let luma: Vec<Option<f64>> = (0..21).map(|_| Some(0.2)).collect();
        assert!(
            analyze_luma(&luma, 15.0, 100.0, false)
                .chan_residual_per_frame()
                .iter()
                .all(|c| *c == [0.0; 3])
        );
    }

    /// 合成日转夜（手动圣杯）：场景光 8 EV 匀速变暗，相机在 M 档每 20 帧拧一档补偿，
    /// 外加 ±0.03 EV 的测光误差。相机把画面拉住 → 原始帧只有约 1 EV 的锯齿波动，
    /// **日夜过渡被测光吃掉了**；EXIF 里留着 8 EV 的参数台阶。
    ///
    /// 返回（原始帧线性亮度，每帧相对 EV）。确定性：噪声用 sin 伪随机，不引随机依赖。
    ///
    /// A synthetic day-to-night (manual holy grail): the scene drops 8 EV at a steady rate
    /// while the camera, locked in M, gains one stop every 20 frames, plus ±0.03 EV of
    /// metering error. The meter holds the picture, so the raw frames carry only a ~1 EV
    /// sawtooth -- **the day-night transition has been eaten by the meter** -- while the EXIF
    /// keeps an 8 EV staircase of parameter changes.
    ///
    /// Returns (raw linear luma, per-frame relative EV). Deterministic: the noise is a sine
    /// pseudo-random, no RNG dependency.
    fn holy_grail(n: usize) -> (Vec<Option<f64>>, Vec<Option<f64>>) {
        let mut means = Vec::with_capacity(n);
        let mut evs = Vec::with_capacity(n);
        for i in 0..n {
            let scene = -8.0 * i as f64 / (n - 1).max(1) as f64; // 场景光，log2 域
            let comp = (i / 20) as f64; // 手动补偿，每 20 帧 +1 档
            let noise = 0.03 * (i as f64 * 1.7).sin(); // 测光误差
            // 原始亮度 ∝ 场景光 × 相机增益；comp ≈ −scene 把画面拉住。
            let raw = 0.5 * 2f64.powf(scene + comp + noise);
            // 相机参数的 EV 与补偿增益反号（EV 高 = 进光少）；常数被中位数锚吸收。
            let ev = -comp;
            means.push(Some(raw));
            evs.push(Some(ev));
        }
        (means, evs)
    }

    fn max_neighbour_jump(values: &[f64]) -> f64 {
        values
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f64::max)
    }

    /// 把一组线性亮度摊成 log2 序列（洞按插值填），用于量「相邻帧差几档光」。
    ///
    /// Turns a row of linear luminances into a log2 series (holes interpolated), the unit in
    /// which "how many stops the picture jumps between neighbouring frames" is measured.
    fn log2_series(means: &[Option<f64>]) -> Vec<f64> {
        let m = Matrix::from_luma(means).filled();
        (0..means.len())
            .map(|i| m.get(i, 0).max(1e-7).log2())
            .collect()
    }

    /// 圣杯的「去闪前残差」必须在对齐后降一个量级：原始域里 1 EV 的是**参数台阶**，
    /// 对齐后 0.1 EV 的是**测光误差**——去闪器该压的是后者，前者是日夜过渡本身。
    ///
    /// 把对齐的增益改成 1（等于没对齐）必红。
    ///
    /// The pre-deflicker residual of a holy-grail sequence must drop by an order of magnitude
    /// once aligned: the ~1 EV jumps in the raw domain are the **parameter staircase**, the
    /// ~0.1 EV wobble after alignment is the **metering error** -- the latter is flicker to
    /// suppress, the former is the day-night transition itself. Red when the alignment gain
    /// is forced to 1.
    #[test]
    fn exif_alignment_drops_the_pre_deflicker_residual_by_an_order_of_magnitude() {
        let (means, evs) = holy_grail(160);
        let raw = analyze_luma(&means, 15.0, 100.0, false);
        let anchor = exif_anchor(&evs).expect("夹具里每帧都有 EV");
        let aligned_means = align_luma_to_exif(&means, &evs, anchor);
        let aligned = analyze_luma(&aligned_means, 15.0, 100.0, false);

        let raw_jump = max_neighbour_jump(&log2_series(&means));
        let aligned_jump = max_neighbour_jump(&log2_series(&aligned_means));

        assert!(
            raw_jump > 0.5,
            "夹具失效：原始域得有台阶级的相邻差（实得 {raw_jump} EV）"
        );
        assert!(
            aligned_jump * 5.0 < raw_jump,
            "对齐后去闪前残差没降一个量级：raw {raw_jump:.3} EV → aligned {aligned_jump:.3} EV"
        );
        // 判据口径用的是**最大**相邻差而不是产品的 P95：台阶只占帧数的 5%（每 20 帧拧
        // 一档），P95 恰好落在台阶分布的边缘上，抓不住它；最大差直接咬台阶本身。
        let _ = (raw.stats.jitter_before, aligned.stats.jitter_before);
    }

    /// 圣杯的本质判据：**输出必须把被测光吃掉的日夜过渡还回来**。
    ///
    /// 关闭对齐时，去闪的输入就是相机拍到的画面（被拉平，落差 ~1 EV），输出也一样——
    /// 视频里看不到日落。开启对齐后，输入是场景光（8 EV 的真实变暗），去闪只压测光
    /// 误差，输出的落差必须保留 8 EV 的四分之三以上。这条咬的是「去闪不再吃掉日转夜」，
    /// 也是 EXIF 对齐存在的理由。
    ///
    /// The essential holy-grail assertion: **the output must hand back the transition the
    /// meter ate**. Without alignment the input is what the camera saw (flattened, ~1 EV of
    /// range) and the output matches -- no visible sunset. With alignment the input is the
    /// scene light (a real 8 EV drop), deflicker only trims the metering error, and the output
    /// must keep at least three quarters of that range.
    #[test]
    fn the_aligned_output_restores_the_day_night_transition_the_meter_ate() {
        let (means, evs) = holy_grail(160);
        let range = |a: &Analysis| -> f64 {
            let ev = a.exposure_per_frame();
            let mut out: Vec<f64> = means
                .iter()
                .zip(ev.iter())
                .map(|(m, o)| m.map_or(f64::NAN, |m| m.log2() + o))
                .collect();
            out.retain(|v| v.is_finite());
            let max = out.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let min = out.iter().cloned().fold(f64::INFINITY, f64::min);
            max - min
        };

        let without = range(&analyze_luma(&means, 15.0, 100.0, false));
        let with = range(&analyze_luma_exif(&means, &evs, 15.0, 100.0, false));
        let raw_jump = max_neighbour_jump(&log2_series(&means));

        assert!(
            without < 2.0,
            "夹具失效：关闭对齐时输出不该有大的过渡（实得 {without:.2} EV）"
        );
        assert!(
            with > 6.0,
            "开启对齐后输出只保留了 {with:.2} EV 的过渡——日转夜被去闪吃掉了"
        );
        // 压闪烁该动的量也要小一个量级：对齐域的去闪只压测光误差，不再跟参数台阶搏斗
        // （jitter_after 相对 jitter_before 的降幅不受预对齐增益影响——增益是常数项）。
        let a = analyze_luma_exif(&means, &evs, 15.0, 100.0, false);
        assert!(
            a.stats.jitter_after * 5.0 < raw_jump,
            "对齐后去闪没能把场景域残差压小：after {:.3} vs 原始台阶 {:.3}",
            a.stats.jitter_after,
            raw_jump
        );
    }

    /// 缺 EV 的帧按洞处理、不对齐也不猜；一个 EV 都没有时整体退回普通去闪。
    ///
    /// Frames without an EV become holes -- never guessed; with no EV at all the whole call
    /// falls back to plain deflicker.
    #[test]
    fn missing_evs_become_holes_and_no_evs_at_all_is_a_noop() {
        let (means, mut evs) = holy_grail(160);
        // 中间挖掉 5 帧的 EV：它们必须变洞，而不是按 gain=1 混进去。
        let holes: Vec<usize> = (70..75).collect();
        for i in &holes {
            evs[*i] = None;
        }
        let anchor = exif_anchor(&evs).expect("夹具里还有大量有效 EV");
        let aligned = align_luma_to_exif(&means, &evs, anchor);
        for i in &holes {
            assert!(aligned[*i].is_none(), "第 {i} 帧缺 EV 却没有被当洞处理");
        }
        let good = (0..160usize).filter(|i| !holes.contains(i)).count();
        assert_eq!(
            aligned.iter().filter(|m| m.is_some()).count(),
            good,
            "有 EV 的帧必须全部保留"
        );
        // 洞帧的校正必须保持 0：那张帧不许被猜出来的增益动过。
        let a = analyze_luma_exif(&means, &evs, 15.0, 100.0, false);
        for i in &holes {
            assert_eq!(
                a.offset(*i, 0),
                0.0,
                "第 {i} 帧缺 EV，校正却不是 0——应用侧会把这帧重曝光"
            );
        }

        // 锚点取 EV 中位数不是随手挑的：并回以后全场校正的中位数必须贴着 0，
        // 否则预对齐会顺手把整条片子整体提亮/压暗（锚点改成 0 必红：中位数会跳到
        // 一整段 EV）。这条咬的是「对齐只重分配，不改总曝光」。
        let mut folded: Vec<f64> = (0..means.len()).map(|i| a.offset(i, 0)).collect();
        folded.sort_by(f64::total_cmp);
        let median = folded[folded.len() / 2];
        assert!(
            median.abs() < 1.0,
            "预对齐把整条片子整体挪了 {median:.2} EV——锚点不该是 EV 中位数之外的数"
        );

        // 全部没有 EV：退回普通去闪（offsets 逐位一致），开关等于没开。
        let none_evs: Vec<Option<f64>> = vec![None; 160];
        let fallback = analyze_luma_exif(&means, &none_evs, 15.0, 100.0, false);
        let plain = analyze_luma(&means, 15.0, 100.0, false);
        assert_eq!(
            fallback.offsets, plain.offsets,
            "无 EV 时必须与普通去闪逐位一致"
        );
        assert_eq!(fallback.stats, plain.stats);

        // RGB 路同理：同因子乘三通道，缺 EV 的帧整帧是洞。
        let rgb: Vec<Option<[f64; 3]>> = means.iter().map(|m| m.map(|v| [v; 3])).collect();
        let aligned_rgb = align_rgb_to_exif(&rgb, &evs, anchor);
        assert!(aligned_rgb[70].is_none());
        let v = aligned_rgb[0].unwrap();
        let r = rgb[0].unwrap();
        let gain = 2f64.powf(evs[0].unwrap() - anchor);
        for k in 0..3 {
            assert!(
                (v[k] - r[k] * gain).abs() < 1e-12,
                "RGB 通道 {k} 的对齐因子不对"
            );
        }
    }
}
