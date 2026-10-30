//! 序列去闪烁：逐帧测光 → 稳健平滑 → 曝光偏移（EV）。
//!
//! 口径照搬 Python 实现（`lapse/deflicker.py`）：中值压单帧跳变，高斯平滑避免
//! 校正后出现台阶，偏移单位等于调色台的曝光 EV。`oracle/golden/deflicker.json`
//! 是同一算法的冻结输出，单测按 1e-4 比对。

use std::path::{Path, PathBuf};

use image::RgbImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub const WINDOW_MIN: usize = 3;
pub const WINDOW_MAX: usize = 199;
/// 单帧最多补几档：再大就不是闪烁而是坏帧。
pub const MAX_EV: f64 = 3.0;
/// sRGB 亮度系数，通道顺序 R、G、B。
pub const LUMA_RGB: [f64; 3] = [0.2126, 0.7152, 0.0722];
/// 测光只读缩略图：整段序列的耗时几乎全在读图，长边 320 足够定出趋势。
pub const SCAN_LONG: u32 = 320;

/// 一帧的线性光测光：整帧亮度与 R/G/B 分通道各一份。
///
/// 与胶片条用的 gamma 域平均亮度（`metrics::FrameMetrics`）是两套口径：
/// 去闪要在线性光里做截尾均值，否则明暗跳会被 sRGB 编码压扁，压不住彩色闪烁。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameMeans {
    /// 线性亮度的 1%~99% 截尾均值，0~1。
    pub luma: f64,
    /// R、G、B 各自的截尾均值，0~1。
    pub rgb: [f64; 3],
}

/// 256 级 sRGB→线性查表；与 Python 的 `_SRGB_LUT` 同为 float64 算完再降 f32。
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
pub fn measure_path(path: impl AsRef<Path>) -> Option<FrameMeans> {
    let img = crate::metrics::decode_rgb(path, SCAN_LONG).ok()?;
    measure_image(&img)
}

/// 并行测光；返回与 `paths` 等长的表和 (帧号, 错误) 列表。
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
    pub jitter_before: f64,
    pub jitter_after: f64,
    /// 分通道时三个通道里最抖的那个，校正前后。
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
pub fn analyze_luma(means: &[Option<f64>], window: f64, strength: f64, multi: bool) -> Analysis {
    let matrix = Matrix::from_luma(means);
    analyze(&matrix, window, strength, multi)
}

/// 分通道（R/G/B）去闪烁，用于压彩色场景的色闪。
pub fn analyze_rgb(
    means: &[Option<[f64; 3]>],
    window: f64,
    strength: f64,
    multi: bool,
) -> Analysis {
    let matrix = Matrix::from_rgb(means);
    analyze(&matrix, window, strength, multi)
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
}
