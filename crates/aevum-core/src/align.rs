//! 序列对齐：估计每帧相对参考帧的位移 → 平滑 → 计算统一裁切。
//!
//! 约定与 Python `acceptance/reference/align.py` 一致：`offset` 是 warpAffine 要加的偏移，
//! 即 `dst(x, y) = src(x - off_x, y - off_y)`。模板估计器是 lr3 成片用的那套：
//! 底部模板 + CLAHE 灰度 + 归一化互相关 + 抛物线亚像素。
//!
//! 实现要点：
//! - 模板区域、护带、搜索边距（150 px）照抄 Python `_setup_template`。
//! - 4K 模板（约 3590×1030）的 TM_CCOEFF_NORMED 用自写的 2D FFT 做互相关，
//!   再用积分/滑动窗口算窗口方差；峰值邻域用原图直接算 3×3，保证亚像素精度。
//!   FFT 只管搜索窗大小（4096×2048），不保留整帧 f32 副本。
//! - `Phase` 用同一套 FFT 实现相位相关（ROI、归一化、Hann 同 Python），
//!   不引入第三方依赖；与 OpenCV `phaseCorrelate` 的响应/亚像素口径略有差异。
//! - `residual_scores` 按 `verify_residuals` 的口径取底部 ROI，warp 用双线性
//!   采样逐像素完成，不生成整幅 warp 结果。
//!
//! Sequence alignment: estimate each frame's shift relative to the reference frame → smooth
//! → compute a shared crop.
//!
//! The convention matches Python `acceptance/reference/align.py`: `offset` is the shift that
//! warpAffine adds, i.e. `dst(x, y) = src(x - off_x, y - off_y)`. The template estimator is
//! the one used for lr3 final renders: bottom template + CLAHE grayscale + normalized cross
//! correlation + parabolic subpixel.
//!
//! Implementation notes:
//! - The template region, guard band and search margin (150 px) are copied from Python
//!   `_setup_template`.
//! - TM_CCOEFF_NORMED on a 4K template (about 3590×1030) uses a hand-written 2D FFT for the
//!   cross correlation, then integral and sliding-window sums for the window variance. The
//!   peak neighborhood is scored on the source image over 3×3 to keep subpixel accuracy, and
//!   the FFT covers the search window (4096×2048) only, so no full-frame f32 copy is kept.
//! - `Phase` reuses the same FFT for phase correlation (ROI, normalization and Hann window as
//!   in Python) with no third-party dependency; its response and subpixel convention differ
//!   slightly from OpenCV `phaseCorrelate`.
//! - `residual_scores` takes the bottom ROI with the `verify_residuals` convention and warps
//!   per pixel with bilinear sampling; no full warped image is materialized.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Python `measure_offsets` 的默认搜索边距。
const SEARCH_MARGIN: usize = 150;
/// Python `_setup_template` 的默认起点。
const TEMPLATE_START: f64 = 0.7;
/// CLAHE 的网格与裁剪系数，与 Python 一致。
const CLAHE_GRID: usize = 8;
const CLAHE_CLIP: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Estimator {
    /// 底部模板匹配，对相机下坠造成的垂直漂移最稳。
    ///
    /// Bottom template matching, most robust against vertical drift from a sinking camera.
    Template,
    /// 相位相关，适合纯平移、光比稳定的序列。
    ///
    /// Phase correlation, suited to sequences with pure translation and a stable lighting ratio.
    Phase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AlignOptions {
    pub reference: usize,
    pub estimator: Estimator,
    /// 搜索半径，单位像素。
    ///
    /// Search radius in pixels.
    pub max_shift: u32,
    /// 平滑窗口；0 表示不平滑。
    ///
    /// Smoothing window; 0 disables smoothing.
    pub smooth_window: usize,
    /// 相邻帧位移跳变超过这个值，保留原值不参与平滑。
    ///
    /// A shift jump past this value between adjacent frames keeps the raw value unsmoothed.
    pub jump_px: f64,
    /// 相关系数低于这个值记为异常帧。
    ///
    /// Frames whose correlation score falls below this value are reported as anomalies.
    pub min_score: f64,
}

impl Default for AlignOptions {
    fn default() -> Self {
        Self {
            reference: 0,
            estimator: Estimator::Template,
            max_shift: 64,
            smooth_window: 15,
            jump_px: 24.0,
            min_score: 0.35,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AlignIssue {
    pub frame: usize,
    /// `score` / `jump` / `edge`，供界面翻译成人话。
    ///
    /// `score` / `jump` / `edge`, translated into readable text by the UI.
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AlignReport {
    pub size: [u32; 2],
    pub offsets: Vec<[f64; 2]>,
    pub scores: Vec<f64>,
    pub issues: Vec<AlignIssue>,
    pub crop: CropRect,
    pub elapsed_secs: f64,
}

#[derive(Debug)]
pub enum AlignError {
    Empty,
    Reference(usize),
    Decode {
        path: String,
        source: crate::metrics::MetricsError,
    },
    Message(String),
}

impl crate::errmsg::Msg for AlignError {
    fn msg(&self) -> (&'static str, Vec<crate::errmsg::Arg<'_>>) {
        match self {
            Self::Empty => ("没有可对齐的帧", vec![]),
            Self::Reference(v) => (
                "参考帧 {} 超出范围",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::Decode { path, source } => (
                "读图失败 {}：{}",
                vec![
                    crate::errmsg::Arg::Text(path.to_string()),
                    crate::errmsg::Arg::Text(source.to_string()),
                ],
            ),
            Self::Message(v) => (
                "内部错误：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
        }
    }
}

crate::impl_msg_display!(AlignError);

impl std::error::Error for AlignError {}

/// 估计整段序列的位移并给出统一裁切。
///
/// Estimates shifts for the whole sequence and derives a shared crop.
pub fn measure(paths: &[PathBuf], options: &AlignOptions) -> Result<AlignReport, AlignError> {
    measure_with_progress(paths, options, &|_, _| {})
}

/// 同一件事，但每完成一帧回调一次 `(已完成, 总数)`。
///
/// 回调在 worker 线程里执行；计数用原子量并加锁保证观测到的 `done` 严格递增。
///
/// Same job, but calls back once per finished frame with `(done, total)`.
///
/// The callback runs on worker threads; an atomic counter plus a lock makes the observed
/// `done` values increase strictly.
pub fn measure_with_progress(
    paths: &[PathBuf],
    options: &AlignOptions,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<AlignReport, AlignError> {
    let started = Instant::now();
    if paths.is_empty() {
        return Err(AlignError::Empty);
    }
    if options.reference >= paths.len() {
        return Err(AlignError::Reference(options.reference));
    }
    let total = paths.len();
    let ref_rgb = decode_rgb(&paths[options.reference])?;
    let (rw, rh) = (ref_rgb.width() as usize, ref_rgb.height() as usize);
    let ref_gray = gray_from_rgb(&ref_rgb);
    drop(ref_rgb);

    let done = AtomicUsize::new(0);
    let gate = Mutex::new(());
    let tick = |i: usize, out: FrameOut| -> (usize, FrameOut) {
        // 计数与回调放在同一把锁内，保证观测到的 done 严格递增。
        let guard = gate.lock().unwrap_or_else(|e| e.into_inner());
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress(n, total);
        drop(guard);
        (i, out)
    };

    let mut outs: Vec<FrameOut> = vec![FrameOut::default(); total];
    match options.estimator {
        Estimator::Template => {
            let mut ref_gray = ref_gray;
            clahe(&mut ref_gray, rw, rh);
            let setup = TemplateSetup::new(&ref_gray, rw, rh, options.max_shift)?;
            drop(ref_gray);
            let jobs = frame_jobs(rw * rh, total);
            for chunk in (0..total).collect::<Vec<_>>().chunks(jobs) {
                let batch: Vec<(usize, FrameOut)> = chunk
                    .par_iter()
                    .map(|&i| tick(i, setup.process(&paths[i], rw, rh, options.min_score)))
                    .collect();
                for (i, out) in batch {
                    outs[i] = out;
                }
            }
        }
        Estimator::Phase => {
            let setup = PhaseSetup::new(&ref_gray, rw, rh)?;
            drop(ref_gray);
            let jobs = frame_jobs(rw * rh, total);
            for chunk in (0..total).collect::<Vec<_>>().chunks(jobs) {
                let batch: Vec<(usize, FrameOut)> = chunk
                    .par_iter()
                    .map(|&i| tick(i, setup.process(&paths[i], rw, rh, options.min_score)))
                    .collect();
                for (i, out) in batch {
                    outs[i] = out;
                }
            }
        }
    }

    let mut offsets = vec![[0.0f64, 0.0f64]; total];
    let mut scores = vec![0.0f64; total];
    let mut issues = Vec::new();
    for (i, out) in outs.into_iter().enumerate() {
        offsets[i] = out.offset;
        scores[i] = out.score;
        if let Some((kind, detail)) = out.issue {
            issues.push(AlignIssue {
                frame: i,
                kind,
                detail,
            });
        }
    }
    // 相邻帧大跳变单列出来（不动 offsets，只提示）。
    for i in 1..total {
        let jump = (offsets[i][0] - offsets[i - 1][0])
            .abs()
            .max((offsets[i][1] - offsets[i - 1][1]).abs());
        if jump > options.jump_px {
            issues.push(AlignIssue {
                frame: i,
                kind: "jump".to_owned(),
                detail: format!("相邻帧位移跳变 {jump:.1} px"),
            });
        }
    }
    issues.sort_by_key(|x| x.frame);

    let crop = common_crop(&offsets, rw as u32, rh as u32);
    Ok(AlignReport {
        size: [rw as u32, rh as u32],
        offsets,
        scores,
        issues,
        crop,
        elapsed_secs: started.elapsed().as_secs_f64(),
    })
}

/// 就地平滑位移。返回被保留（未平滑）的异常帧下标。
///
/// 沿用 Python `preserve_outliers`：局部中值做基线，偏离超过 `jump_px` 的帧
/// 只有 score 达标（相机真的动了）才保留原值；低分帧视为误匹配，用中值压平。
/// 返回的是检测到的跳变帧下标，两类都会列出来，供界面提示。
///
/// Smooths shifts in place. Returns the indices of anomalous frames kept unsmoothed.
///
/// Follows Python `preserve_outliers`: a local median is the baseline; a frame deviating by
/// more than `jump_px` keeps its raw value only when its score passes (the camera really
/// moved); a low-score frame is treated as a mismatch and flattened to the median. The return
/// value holds the detected jump indices, covering both kinds, for UI hints.
pub fn smooth_offsets(
    offsets: &mut [[f64; 2]],
    scores: &[f64],
    options: &AlignOptions,
) -> Vec<usize> {
    let n = offsets.len();
    if n == 0 || options.smooth_window == 0 {
        return Vec::new();
    }
    let half = (options.smooth_window / 2).max(1);
    let xs: Vec<f64> = (0..n).map(|i| offsets[i][0]).collect();
    let ys: Vec<f64> = (0..n).map(|i| offsets[i][1]).collect();
    let (new_x, keep_x) = preserve_outliers(&xs, scores, half, options.jump_px, options.min_score);
    let (new_y, keep_y) = preserve_outliers(&ys, scores, half, options.jump_px, options.min_score);
    for i in 0..n {
        offsets[i] = [new_x[i], new_y[i]];
    }
    let mut keep = keep_x;
    keep.extend(keep_y);
    keep.sort_unstable();
    keep.dedup();
    keep
}

/// 让所有帧都不出画的最小裁切框。
///
/// Smallest crop rectangle that keeps every frame inside the image.
pub fn common_crop(offsets: &[[f64; 2]], width: u32, height: u32) -> CropRect {
    if offsets.is_empty() || width == 0 || height == 0 {
        return CropRect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
    }
    let (w, h) = (width as f64, height as f64);
    let mut x1 = 0.0f64;
    let mut x2 = w;
    let mut y1 = 0.0f64;
    let mut y2 = h;
    for o in offsets {
        if !o[0].is_finite() || !o[1].is_finite() {
            continue;
        }
        x1 = x1.max(o[0]);
        x2 = x2.min(w + o[0]);
        y1 = y1.max(o[1]);
        y2 = y2.min(h + o[1]);
    }
    // 保守取整：左/上向上取整，右/下向下取整，整数坐标下一定不出画。
    let mut xi1 = x1.ceil().clamp(0.0, w) as u32;
    let mut xi2 = x2.floor().clamp(0.0, w) as u32;
    let mut yi1 = y1.ceil().clamp(0.0, h) as u32;
    let mut yi2 = y2.floor().clamp(0.0, h) as u32;
    if xi2 <= xi1 {
        // 位移把画面吃光时至少留 1 px，保证宽高为正。
        xi1 = xi1.min(width - 1);
        xi2 = xi1 + 1;
    }
    if yi2 <= yi1 {
        yi1 = yi1.min(height - 1);
        yi2 = yi1 + 1;
    }
    CropRect {
        x: xi1,
        y: yi1,
        w: xi2 - xi1,
        h: yi2 - yi1,
    }
}

/// 对齐后再测一次底部区域的残差相关系数，用来验收平滑是否真的压住了抖动。
///
/// 返回与 `paths` 等长的皮尔逊相关系数：参考帧为 1.0，读不出来的帧为 0.0。
/// warp 按 `dst(x,y) = src(x-dx, y-dy)` 做双线性采样，只算 ROI 内的像素。
///
/// Measures the residual correlation of the bottom region after alignment, as an acceptance
/// check that smoothing really suppressed the shake.
///
/// Returns one Pearson correlation per path, same length as `paths`: 1.0 for the reference
/// frame, 0.0 for frames that cannot be read. The warp samples bilinearly as
/// `dst(x,y) = src(x-dx, y-dy)`, over ROI pixels only.
pub fn residual_scores(
    paths: &[PathBuf],
    offsets: &[[f64; 2]],
    crop: CropRect,
    reference: usize,
) -> Result<Vec<f64>, AlignError> {
    if reference >= paths.len() {
        return Err(AlignError::Reference(reference));
    }
    let mut scores = vec![0.0f64; paths.len()];
    let refer = match decode_gray(&paths[reference]) {
        Ok(g) => g,
        Err(_) => return Ok(scores),
    };
    let (rw, rh) = (refer.width, refer.height);
    let roi = residual_roi(crop, rw, rh);
    if roi.2 <= roi.0 || roi.3 <= roi.1 {
        return Ok(scores);
    }
    let ref_off = offsets.get(reference).copied().unwrap_or([0.0, 0.0]);
    let base = sample_roi(&refer.pixels, rw, rh, roi, ref_off);
    scores[reference] = 1.0;
    for (i, path) in paths.iter().enumerate() {
        if i == reference {
            continue;
        }
        let Ok(g) = decode_gray(path) else { continue };
        if g.width != rw || g.height != rh {
            continue;
        }
        let off = offsets.get(i).copied().unwrap_or([0.0, 0.0]);
        let patch = sample_roi(&g.pixels, rw, rh, roi, off);
        scores[i] = pearson(&base, &patch);
    }
    Ok(scores)
}

// ---------------------------------------------------------------------------
// 灰度、CLAHE
// ---------------------------------------------------------------------------

/// 解成 8bit RGB 供模板匹配用。
///
/// 方向与 DNG 都交给 `metrics::decode`：竖拍帧和横拍帧混在一起时，对齐位移是两套
/// 坐标系，必须先转正再比模板。
fn decode_rgb(path: &PathBuf) -> Result<image::RgbImage, AlignError> {
    let img = crate::metrics::decode(path).map_err(|source| AlignError::Decode {
        path: path.display().to_string(),
        source,
    })?;
    Ok(img.into_rgb8())
}

/// cv2 的 BGR2GRAY 定点系数（0.299R + 0.587G + 0.114B）。
fn gray_from_rgb(rgb: &image::RgbImage) -> Vec<u8> {
    rgb.pixels()
        .map(|p| {
            let [r, g, b] = p.0;
            ((r as u32 * 4899 + g as u32 * 9617 + b as u32 * 1868 + 8192) >> 14) as u8
        })
        .collect()
}

struct Gray {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
}

fn decode_gray(path: &PathBuf) -> Result<Gray, AlignError> {
    let rgb = decode_rgb(path)?;
    let (width, height) = (rgb.width() as usize, rgb.height() as usize);
    Ok(Gray {
        pixels: gray_from_rgb(&rgb),
        width,
        height,
    })
}

/// 尺寸不同时按 INTER_AREA 口径缩到参考尺寸（对应 Python `_match_shape`）。
fn resize_gray_area(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    if (sw, sh) == (dw, dh) || dw == 0 || dh == 0 {
        return src.to_vec();
    }
    let wx = area_weights(sw, dw);
    let wy = area_weights(sh, dh);
    let mut tmp = vec![0.0f32; dw * sh];
    for y in 0..sh {
        for (dx, ws) in wx.iter().enumerate() {
            let mut acc = 0.0f32;
            for &(sx, wt) in ws {
                acc += src[y * sw + sx] as f32 * wt as f32;
            }
            tmp[y * dw + dx] = acc;
        }
    }
    let mut out = vec![0u8; dw * dh];
    for (dy, ws) in wy.iter().enumerate() {
        for x in 0..dw {
            let mut acc = 0.0f32;
            for &(sy, wt) in ws {
                acc += tmp[sy * dw + x] * wt as f32;
            }
            out[dy * dw + x] = acc.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn area_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f64)>> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|d| {
            let start = d as f64 * scale;
            let end = (d + 1) as f64 * scale;
            let span = (end - start).max(1e-9);
            let mut v = Vec::new();
            let mut j = start.floor() as usize;
            while (j as f64) < end {
                let wt = (end.min(j as f64 + 1.0) - start.max(j as f64)).max(0.0);
                if wt > 0.0 {
                    v.push((j.min(src - 1), wt / span));
                }
                j += 1;
            }
            v
        })
        .collect()
}

/// CLAHE（clipLimit=2.0，8×8 网格）。LUT + 双线性插值，口径对齐 OpenCV。
fn clahe(gray: &mut [u8], w: usize, h: usize) {
    if w == 0 || h == 0 {
        return;
    }
    let gx = CLAHE_GRID.min(w.max(1));
    let gy = CLAHE_GRID.min(h.max(1));
    let tw = w.div_ceil(gx).max(1);
    let th = h.div_ceil(gy).max(1);
    let mut luts = vec![0u8; gx * gy * 256];
    let mut hist = vec![0i64; 256];
    for ty in 0..gy {
        for tx in 0..gx {
            let x1 = tx * tw;
            let y1 = ty * th;
            let x2 = (x1 + tw).min(w);
            let y2 = (y1 + th).min(h);
            let lut = &mut luts[(ty * gx + tx) * 256..][..256];
            if x1 >= x2 || y1 >= y2 {
                for (i, v) in lut.iter_mut().enumerate() {
                    *v = i as u8;
                }
                continue;
            }
            hist.fill(0);
            for y in y1..y2 {
                for &v in &gray[y * w + x1..y * w + x2] {
                    hist[v as usize] += 1;
                }
            }
            let area = (x2 - x1) * (y2 - y1);
            let mut clip = ((CLAHE_CLIP * area as f64) / 256.0) as i64;
            clip = clip.max(1);
            let mut clipped = 0i64;
            for v in hist.iter_mut() {
                if *v > clip {
                    clipped += *v - clip;
                    *v = clip;
                }
            }
            let batch = clipped / 256;
            let mut residual = clipped - batch * 256;
            for v in hist.iter_mut() {
                *v += batch;
            }
            if residual > 0 {
                let step = ((256 / residual) as usize).max(1);
                let mut i = 0usize;
                while i < 256 && residual > 0 {
                    hist[i] += 1;
                    residual -= 1;
                    i += step;
                }
            }
            let mut cdf = 0i64;
            let scale = 255.0 / area as f64;
            for (i, v) in hist.iter().enumerate() {
                cdf += *v;
                lut[i] = (cdf as f64 * scale).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let fy = y as f64 / th as f64 - 0.5;
        let y0 = fy.floor();
        let wy = fy - y0;
        let ya = (y0 as isize).clamp(0, gy as isize - 1) as usize;
        let yb = ((y0 as isize) + 1).clamp(0, gy as isize - 1) as usize;
        for x in 0..w {
            let fx = x as f64 / tw as f64 - 0.5;
            let x0 = fx.floor();
            let wx = fx - x0;
            let xa = (x0 as isize).clamp(0, gx as isize - 1) as usize;
            let xb = ((x0 as isize) + 1).clamp(0, gx as isize - 1) as usize;
            let v = gray[y * w + x] as usize;
            let la = (ya * gx + xa) * 256 + v;
            let lb = (ya * gx + xb) * 256 + v;
            let lc = (yb * gx + xa) * 256 + v;
            let ld = (yb * gx + xb) * 256 + v;
            let top = luts[la] as f64 * (1.0 - wx) + luts[lb] as f64 * wx;
            let bot = luts[lc] as f64 * (1.0 - wx) + luts[ld] as f64 * wx;
            out[y * w + x] = (top * (1.0 - wy) + bot * wy).round().clamp(0.0, 255.0) as u8;
        }
    }
    gray.copy_from_slice(&out);
}

// ---------------------------------------------------------------------------
// FFT（自写，避免新增依赖）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default, Debug)]
struct C {
    re: f32,
    im: f32,
}

impl C {
    #[inline]
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }

    #[inline]
    fn conj(self) -> C {
        C {
            re: self.re,
            im: -self.im,
        }
    }

    #[inline]
    fn norm(self) -> f32 {
        (self.re * self.re + self.im * self.im).sqrt()
    }
}

struct FftPlan {
    n: usize,
    rev: Vec<u32>,
    tw: Vec<C>,
}

impl FftPlan {
    fn new(n: usize) -> Self {
        let bits = n.trailing_zeros() as usize;
        let mut rev = vec![0u32; n];
        for (i, slot) in rev.iter_mut().enumerate() {
            *slot = (i.reverse_bits() >> (usize::BITS as usize - bits)) as u32;
        }
        let half = n / 2;
        let tw = (0..half)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                C {
                    re: a.cos() as f32,
                    im: a.sin() as f32,
                }
            })
            .collect();
        Self { n, rev, tw }
    }
}

fn fft_1d(buf: &mut [C], plan: &FftPlan, inverse: bool) {
    let n = plan.n;
    if n <= 1 {
        return;
    }
    for i in 0..n {
        let j = plan.rev[i] as usize;
        if i < j {
            buf.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let step = n / len;
        let mut start = 0;
        while start < n {
            for k in 0..half {
                let mut w = plan.tw[k * step];
                if inverse {
                    w.im = -w.im;
                }
                let a = buf[start + k];
                let b = buf[start + k + half].mul(w);
                buf[start + k] = C {
                    re: a.re + b.re,
                    im: a.im + b.im,
                };
                buf[start + k + half] = C {
                    re: a.re - b.re,
                    im: a.im - b.im,
                };
            }
            start += len;
        }
        len <<= 1;
    }
    if inverse {
        let s = 1.0 / n as f32;
        for v in buf.iter_mut() {
            v.re *= s;
            v.im *= s;
        }
    }
}

#[derive(Clone, Copy)]
struct SendPtr(*mut C);
// 安全说明：叉乘到不同列，各线程写入的元素互不重叠。
// Safety: every worker touches only its own column (`y * fw + x` for a fixed `x`), and the
// columns of a row-major buffer never overlap, so the concurrent writes are disjoint and no
// two threads ever alias the same `C`.
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

impl SendPtr {
    #[inline]
    fn at(self, i: usize) -> *mut C {
        unsafe { self.0.add(i) }
    }
}

fn fft2(buf: &mut [C], fw: usize, fh: usize, rp: &FftPlan, cp: &FftPlan, inverse: bool) {
    buf.par_chunks_mut(fw)
        .for_each(|row| fft_1d(row, rp, inverse));
    // 裸指针必须在**这里**取，不能提到上面去：`par_chunks_mut` 通过 `&mut buf` 重借用
    // 整个缓冲，早取的指针 tag 会被这次重借用作废，第二段再用它就是 Stacked Borrows
    // 下的 UB（数据竞争本身不存在——每线程只碰自己那一列）。
    //
    // The raw pointer has to be taken **here**, not above: `par_chunks_mut` reborrows the
    // whole buffer through `&mut buf`, which invalidates the tag of any pointer created
    // earlier, and using it afterwards is UB under Stacked Borrows (there is no data race
    // -- each worker only touches its own column).
    let ptr = SendPtr(buf.as_mut_ptr());
    (0..fw).into_par_iter().for_each_init(
        || vec![C::default(); fh],
        |tmp, x| {
            for y in 0..fh {
                tmp[y] = unsafe { *ptr.at(y * fw + x) };
            }
            fft_1d(tmp, cp, inverse);
            for y in 0..fh {
                unsafe { *ptr.at(y * fw + x) = tmp[y] };
            }
        },
    );
}

// ---------------------------------------------------------------------------
// 模板估计器
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct FrameOut {
    offset: [f64; 2],
    score: f64,
    issue: Option<(String, String)>,
}

struct TemplateSetup {
    tpl: Vec<f64>,
    tw: usize,
    th: usize,
    sum_t2: f64,
    sx: usize,
    sy: usize,
    sw: usize,
    sh: usize,
    pos: (usize, usize),
    fw: usize,
    fh: usize,
    tpl_spec: Vec<C>,
    rp: FftPlan,
    cp: FftPlan,
}

impl TemplateSetup {
    fn new(ref_gray: &[u8], w: usize, h: usize, max_shift: u32) -> Result<Self, AlignError> {
        let ty = (h as f64 * TEMPLATE_START) as usize;
        let tx = (w as f64 * 0.2) as usize;
        let tw = (w as f64 * 0.6) as usize;
        let guard = ((h as f64 * 0.04).round() as usize).max(8);
        let guard = guard.min((h.saturating_sub(ty) / 3).max(8));
        let (th, tw) = (h.saturating_sub(guard).saturating_sub(ty), tw);
        if tw < 2 || th < 2 || tx + tw > w {
            return Err(AlignError::Message("参考帧太小，模板区域为空".to_owned()));
        }
        let margin = SEARCH_MARGIN.max(max_shift as usize);
        let sx = tx.saturating_sub(margin);
        let sy = ty.saturating_sub(margin);
        let ex = (tx + tw + margin).min(w);
        let ey = (ty + th + margin).min(h);
        if ex <= tx || ey <= ty || ex - tw < sx || ey - th < sy {
            return Err(AlignError::Message("搜索区域太小".to_owned()));
        }
        let (sw, sh) = (ex - sx, ey - sy);

        let mut tpl = Vec::with_capacity(tw * th);
        let mut sum_t = 0.0f64;
        for y in 0..th {
            for x in 0..tw {
                let v = ref_gray[(ty + y) * w + tx + x] as f64;
                tpl.push(v);
                sum_t += v;
            }
        }
        let mean_t = sum_t / (tw * th) as f64;
        let mut sum_t2 = 0.0f64;
        for v in tpl.iter_mut() {
            *v -= mean_t;
            sum_t2 += *v * *v;
        }

        let fw = sw.next_power_of_two();
        let fh = sh.next_power_of_two();
        let rp = FftPlan::new(fw);
        let cp = FftPlan::new(fh);
        let mut tpl_spec = vec![C::default(); fw * fh];
        for y in 0..th {
            for x in 0..tw {
                tpl_spec[y * fw + x].re = tpl[y * tw + x] as f32;
            }
        }
        fft2(&mut tpl_spec, fw, fh, &rp, &cp, false);

        Ok(Self {
            tpl,
            tw,
            th,
            sum_t2,
            sx,
            sy,
            sw,
            sh,
            pos: (tx, ty),
            fw,
            fh,
            tpl_spec,
            rp,
            cp,
        })
    }

    fn process(&self, path: &PathBuf, rw: usize, rh: usize, min_score: f64) -> FrameOut {
        let rgb = match decode_rgb(path) {
            Ok(v) => v,
            Err(e) => {
                return FrameOut {
                    issue: Some(("score".to_owned(), format!("读取失败：{e}"))),
                    ..Default::default()
                };
            }
        };
        let (gw, gh) = (rgb.width() as usize, rgb.height() as usize);
        let mut gray = gray_from_rgb(&rgb);
        drop(rgb);
        if (gw, gh) != (rw, rh) {
            gray = resize_gray_area(&gray, gw, gh, rw, rh);
        }
        clahe(&mut gray, rw, rh);
        let mut region = vec![0u8; self.sw * self.sh];
        for y in 0..self.sh {
            let src = &gray[(self.sy + y) * rw + self.sx..][..self.sw];
            region[y * self.sw..y * self.sw + self.sw].copy_from_slice(src);
        }
        drop(gray);

        let (rw_res, rh_res) = (self.sw - self.tw + 1, self.sh - self.th + 1);
        if rw_res == 0 || rh_res == 0 {
            return FrameOut {
                issue: Some(("edge".to_owned(), "搜索区域太小".to_owned())),
                ..Default::default()
            };
        }

        // FFT 互相关：c[k] = Σ I[k+m]·T'[m]，有效位移不产生环绕。
        let mut buf = vec![C::default(); self.fw * self.fh];
        for y in 0..self.sh {
            for x in 0..self.sw {
                buf[y * self.fw + x].re = region[y * self.sw + x] as f32;
            }
        }
        fft2(&mut buf, self.fw, self.fh, &self.rp, &self.cp, false);
        for (v, t) in buf.iter_mut().zip(self.tpl_spec.iter()) {
            *v = v.mul(t.conj());
        }
        fft2(&mut buf, self.fw, self.fh, &self.rp, &self.cp, true);

        let (mut px, mut py, mut pscore) = peak_scores(&buf, self.fw, &region, self);
        drop(buf);

        // 峰值 3×3 用原图精确重算，抛物线拟合才有可靠输入。
        let n = (self.tw * self.th) as f64;
        let (mut best_s, mut best_x, mut best_y) = (f64::NEG_INFINITY, px, py);
        for dy in -1isize..=1 {
            for dx in -1isize..=1 {
                let x = px as isize + dx;
                let y = py as isize + dy;
                if x < 0 || y < 0 || x >= rw_res as isize || y >= rh_res as isize {
                    continue;
                }
                let (s, _, _) = self.direct_score(&region, n, x as usize, y as usize);
                if s > best_s {
                    best_s = s;
                    best_x = x as usize;
                    best_y = y as usize;
                }
            }
        }
        px = best_x;
        py = best_y;
        if best_s.is_finite() {
            pscore = best_s;
        }

        let sub_x = if px > 0 && px + 1 < rw_res {
            let (l, _, _) = self.direct_score(&region, n, px - 1, py);
            let (c, _, _) = self.direct_score(&region, n, px, py);
            let (r, _, _) = self.direct_score(&region, n, px + 1, py);
            parab(l, c, r)
        } else {
            0.0
        };
        let sub_y = if py > 0 && py + 1 < rh_res {
            let (u, _, _) = self.direct_score(&region, n, px, py - 1);
            let (c, _, _) = self.direct_score(&region, n, px, py);
            let (d, _, _) = self.direct_score(&region, n, px, py + 1);
            parab(u, c, d)
        } else {
            0.0
        };

        let tx = self.pos.0 as f64;
        let ty = self.pos.1 as f64;
        let offset = [
            tx - (self.sx as f64 + px as f64 + sub_x),
            ty - (self.sy as f64 + py as f64 + sub_y),
        ];
        let score = pscore.clamp(-1.0, 1.0);

        let still_x = self.pos.0 as isize - self.sx as isize;
        let still_y = self.pos.1 as isize - self.sy as isize;
        let at_edge = ((px == 0 || px + 1 == rw_res) && px as isize != still_x)
            || ((py == 0 || py + 1 == rh_res) && py as isize != still_y);
        let issue = if at_edge {
            Some(("edge".to_owned(), "峰值贴边，位移被搜索窗夹住".to_owned()))
        } else if score < min_score {
            Some((
                "score".to_owned(),
                format!("匹配分 {score:.3} 低于 {min_score:.2}"),
            ))
        } else {
            None
        };
        FrameOut {
            offset,
            score,
            issue,
        }
    }

    /// 指定整数峰值处的精确 TM_CCOEFF_NORMED 值。
    fn direct_score(&self, region: &[u8], n: f64, kx: usize, ky: usize) -> (f64, f64, f64) {
        let mut num = 0.0f64;
        let mut sum = 0.0f64;
        let mut sum2 = 0.0f64;
        for y in 0..self.th {
            let base = (y + ky) * self.sw + kx;
            for x in 0..self.tw {
                let v = region[base + x] as f64;
                num += v * self.tpl[y * self.tw + x];
                sum += v;
                sum2 += v * v;
            }
        }
        let var_i = (sum2 - sum * sum / n).max(0.0);
        let den = (var_i * self.sum_t2).sqrt();
        let score = if den > 1e-12 {
            (num / den).clamp(-1.0, 1.0)
        } else if num.abs() < 1e-12 {
            1.0
        } else {
            0.0
        };
        (score, num, sum)
    }
}

/// 用滑动窗口累加方差，扫描互相关图找峰值。
fn peak_scores(buf: &[C], fw: usize, region: &[u8], setup: &TemplateSetup) -> (usize, usize, f64) {
    let sw = setup.sw;
    let (tw, th) = (setup.tw, setup.th);
    let rw_res = sw - tw + 1;
    let rh_res = setup.sh - th + 1;
    let n = (tw * th) as f64;
    let mut col_sum = vec![0.0f64; sw];
    let mut col_sq = vec![0.0f64; sw];
    for y in 0..th {
        for x in 0..sw {
            let v = region[y * sw + x] as f64;
            col_sum[x] += v;
            col_sq[x] += v * v;
        }
    }
    let (mut best, mut bx, mut by) = (f64::NEG_INFINITY, 0usize, 0usize);
    for ky in 0..rh_res {
        if ky > 0 {
            let add = ky + th - 1;
            let del = ky - 1;
            for x in 0..sw {
                let a = region[add * sw + x] as f64;
                let d = region[del * sw + x] as f64;
                col_sum[x] += a - d;
                col_sq[x] += a * a - d * d;
            }
        }
        let mut s = 0.0f64;
        let mut s2 = 0.0f64;
        for x in 0..tw {
            s += col_sum[x];
            s2 += col_sq[x];
        }
        for kx in 0..rw_res {
            if kx > 0 {
                s += col_sum[kx + tw - 1] - col_sum[kx - 1];
                s2 += col_sq[kx + tw - 1] - col_sq[kx - 1];
            }
            let var = (s2 - s * s / n).max(0.0);
            let den = (var * setup.sum_t2).sqrt();
            let num = buf[ky * fw + kx].re as f64;
            let score = if den > 1e-12 {
                (num / den).clamp(-1.0, 1.0)
            } else if num.abs() < 1e-12 {
                1.0
            } else {
                0.0
            };
            if score > best {
                best = score;
                bx = kx;
                by = ky;
            }
        }
    }
    (bx, by, best)
}

fn parab(a: f64, b: f64, c: f64) -> f64 {
    let den = a - 2.0 * b + c;
    if den.abs() < 1e-12 {
        0.0
    } else {
        (0.5 * (a - c) / den).clamp(-1.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 相位相关（近似实现）
// ---------------------------------------------------------------------------

struct PhaseSetup {
    x0: usize,
    y0: usize,
    fw: usize,
    fh: usize,
    ref_spec: Vec<C>,
    rp: FftPlan,
    cp: FftPlan,
}

impl PhaseSetup {
    fn new(ref_gray: &[u8], w: usize, h: usize) -> Result<Self, AlignError> {
        if w < 16 || h < 16 {
            return Err(AlignError::Message("参考帧太小，无法做相位相关".to_owned()));
        }
        // Python 默认 roi=(0, 0.6, 1, 1)，取最靠近中心的 2 的幂子窗。
        let ry = ((h as f64 * 0.6).round() as usize).min(h - 1);
        let roi_w = w;
        let roi_h = h - ry;
        let fw = pow2_floor(roi_w.min(4096));
        let fh = pow2_floor(roi_h.min(2048));
        if fw < 16 || fh < 16 {
            return Err(AlignError::Message("参考帧太小，无法做相位相关".to_owned()));
        }
        let x0 = (roi_w - fw) / 2;
        let y0 = ry + (roi_h - fh) / 2;
        let rp = FftPlan::new(fw);
        let cp = FftPlan::new(fh);
        let patch = phase_patch(ref_gray, w, x0, y0, fw, fh);
        let mut spec: Vec<C> = patch
            .iter()
            .map(|v| C {
                re: *v as f32,
                im: 0.0,
            })
            .collect();
        fft2(&mut spec, fw, fh, &rp, &cp, false);
        Ok(Self {
            x0,
            y0,
            fw,
            fh,
            ref_spec: spec,
            rp,
            cp,
        })
    }

    fn process(&self, path: &PathBuf, rw: usize, rh: usize, min_score: f64) -> FrameOut {
        let rgb = match decode_rgb(path) {
            Ok(v) => v,
            Err(e) => {
                return FrameOut {
                    issue: Some(("score".to_owned(), format!("读取失败：{e}"))),
                    ..Default::default()
                };
            }
        };
        let (gw, gh) = (rgb.width() as usize, rgb.height() as usize);
        let mut gray = gray_from_rgb(&rgb);
        drop(rgb);
        if (gw, gh) != (rw, rh) {
            gray = resize_gray_area(&gray, gw, gh, rw, rh);
        }
        let patch = phase_patch(&gray, rw, self.x0, self.y0, self.fw, self.fh);
        drop(gray);
        let mut cur: Vec<C> = patch
            .iter()
            .map(|v| C {
                re: *v as f32,
                im: 0.0,
            })
            .collect();
        fft2(&mut cur, self.fw, self.fh, &self.rp, &self.cp, false);
        for (v, r) in cur.iter_mut().zip(self.ref_spec.iter()) {
            let mut p = r.mul(v.conj());
            let m = p.norm();
            if m > 1e-12 {
                p.re /= m;
                p.im /= m;
            }
            *v = p;
        }
        fft2(&mut cur, self.fw, self.fh, &self.rp, &self.cp, true);

        let (mut px, mut py) = (0usize, 0usize);
        let mut peak = f32::NEG_INFINITY;
        for y in 0..self.fh {
            for x in 0..self.fw {
                let v = cur[y * self.fw + x].re;
                if v > peak {
                    peak = v;
                    px = x;
                    py = y;
                }
            }
        }
        let at = |x: isize, y: isize| -> f64 {
            let xx = x.rem_euclid(self.fw as isize) as usize;
            let yy = y.rem_euclid(self.fh as isize) as usize;
            cur[yy * self.fw + xx].re as f64
        };
        let sub_x = parab(
            at(px as isize - 1, py as isize),
            peak as f64,
            at(px as isize + 1, py as isize),
        );
        let sub_y = parab(
            at(px as isize, py as isize - 1),
            peak as f64,
            at(px as isize, py as isize + 1),
        );
        let mut sx = px as f64 + sub_x;
        let mut sy = py as f64 + sub_y;
        if sx > self.fw as f64 * 0.5 {
            sx -= self.fw as f64;
        }
        if sy > self.fh as f64 * 0.5 {
            sy -= self.fh as f64;
        }
        let score = (peak as f64).clamp(0.0, 1.0);
        let issue = if score < min_score {
            Some((
                "score".to_owned(),
                format!("相位相关响应 {score:.3} 低于 {min_score:.2}"),
            ))
        } else {
            None
        };
        FrameOut {
            offset: [-sx, -sy],
            score,
            issue,
        }
    }
}

fn pow2_floor(n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    1usize << (usize::BITS as usize - 1 - n.leading_zeros() as usize)
}

/// 底部 ROI 的中心子窗：去均值、除标准差，再乘 Hann 窗（同 Python）。
fn phase_patch(gray: &[u8], w: usize, x0: usize, y0: usize, fw: usize, fh: usize) -> Vec<f64> {
    let n = (fw * fh) as f64;
    let mut sum = 0.0f64;
    let mut sum2 = 0.0f64;
    for y in 0..fh {
        for x in 0..fw {
            let v = gray[(y0 + y) * w + x0 + x] as f64;
            sum += v;
            sum2 += v * v;
        }
    }
    let mean = sum / n;
    let std = (sum2 / n - mean * mean).max(0.0).sqrt() + 1e-6;
    let mut out = Vec::with_capacity(fw * fh);
    let hx: Vec<f64> = (0..fw)
        .map(|x| {
            if fw <= 1 {
                1.0
            } else {
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * x as f64 / (fw - 1) as f64).cos())
            }
        })
        .collect();
    let hy: Vec<f64> = (0..fh)
        .map(|y| {
            if fh <= 1 {
                1.0
            } else {
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * y as f64 / (fh - 1) as f64).cos())
            }
        })
        .collect();
    for y in 0..fh {
        for x in 0..fw {
            let v = gray[(y0 + y) * w + x0 + x] as f64;
            out.push(((v - mean) / std) * hx[x] * hy[y]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 焦点专用块级相位相关（1.5 第二批）
// ---------------------------------------------------------------------------

/// 焦点专用块级相位相关：估 `cur` 相对 `prev` 的**对齐位移**（warp offset）与峰值响应。
///
/// 与 `PhaseSetup` 的区别：不用底部 30% 模板、不用 8bit CLAHE、不用 NCC 低分判失败。
/// 输入是 1/2 分辨率的线性 luma 代理，取块中心的 2 的幂子窗，去均值 / 除标准差 / 乘
/// Hann，再用**同一套** `fft2` 与 `parab` 求带符号峰值与抛物线亚像素。返回的 `[sx, sy]`
/// 直接就是 `dst(x,y) = src(x - sx, y - sy)` 里的 offset，`chain_offsets` 可原样累加。
///
/// Focus-specific block phase correlation: estimate the **warp offset** that aligns `cur`
/// onto `prev`, plus the normalised peak response. Unlike `PhaseSetup` there is no bottom
/// 30% template, no 8-bit CLAHE and no NCC low-score rejection. The input is the half-size
/// linear luma proxy; a centred power-of-two sub-window is de-meaned, divided by its standard
/// deviation and multiplied by a Hann window, then the very same `fft2` / `parab` code yields
/// a signed peak with parabolic sub-pixel refinement. The returned `[sx, sy]` is already the
/// offset in `dst(x,y) = src(x - sx, y - sy)`, so `chain_offsets` accumulates it verbatim.
pub(crate) fn focus_phase_shift(prev: &[f32], cur: &[f32], w: usize, h: usize) -> ([f64; 2], f32) {
    if w < 16 || h < 16 || prev.len() < w * h || cur.len() < w * h {
        return ([0.0, 0.0], 0.0);
    }
    let fw = pow2_floor(w).max(16).min(w);
    let fh = pow2_floor(h).max(16).min(h);
    let x0 = (w - fw) / 2;
    let y0 = (h - fh) / 2;
    let a = focus_patch(prev, w, x0, y0, fw, fh);
    let b = focus_patch(cur, w, x0, y0, fw, fh);
    let rp = FftPlan::new(fw);
    let cp = FftPlan::new(fh);
    let mut sa: Vec<C> = a
        .iter()
        .map(|&v| C {
            re: v as f32,
            im: 0.0,
        })
        .collect();
    let mut sb: Vec<C> = b
        .iter()
        .map(|&v| C {
            re: v as f32,
            im: 0.0,
        })
        .collect();
    fft2(&mut sa, fw, fh, &rp, &cp, false);
    fft2(&mut sb, fw, fh, &rp, &cp, false);
    // R = F_prev · conj(F_cur)：相位差只留位移，幅度归一。
    //
    // R = F_prev * conj(F_cur): phase difference carries the shift, amplitude is normalised.
    for (v, r) in sb.iter_mut().zip(sa.iter()) {
        let mut p = r.mul(v.conj());
        let m = p.norm();
        if m > 1e-12 {
            p.re /= m;
            p.im /= m;
        }
        *v = p;
    }
    fft2(&mut sb, fw, fh, &rp, &cp, true);

    let (mut px, mut py) = (0usize, 0usize);
    let mut peak = f32::NEG_INFINITY;
    for y in 0..fh {
        for x in 0..fw {
            let v = sb[y * fw + x].re;
            if v > peak {
                peak = v;
                px = x;
                py = y;
            }
        }
    }
    let at = |x: isize, y: isize| -> f64 {
        let xx = x.rem_euclid(fw as isize) as usize;
        let yy = y.rem_euclid(fh as isize) as usize;
        sb[yy * fw + xx].re as f64
    };
    // 抛物线亚像素是 A-C1 的负向锚：拿掉它就只剩整数峰，0.5px 步进夹具立刻红。
    //
    // Parabolic sub-pixel is A-C1's negative anchor: without it only the integer peak
    // remains and the 0.5 px-step fixture immediately goes red.
    let sub_x = parab(
        at(px as isize - 1, py as isize),
        peak as f64,
        at(px as isize + 1, py as isize),
    );
    let sub_y = parab(
        at(px as isize, py as isize - 1),
        peak as f64,
        at(px as isize, py as isize + 1),
    );
    let mut sx = px as f64 + sub_x;
    let mut sy = py as f64 + sub_y;
    if sx > fw as f64 * 0.5 {
        sx -= fw as f64;
    }
    if sy > fh as f64 * 0.5 {
        sy -= fh as f64;
    }
    ([sx, sy], (peak as f64).clamp(0.0, 1.0) as f32)
}

/// 块子窗预处理：去均值 / 除标准差 / Hann，与 `phase_patch` 同口径，但吃 f32 luma。
///
/// Block sub-window preprocessing: de-mean / divide by std / Hann, same convention as
/// `phase_patch`, but on f32 luma instead of 8-bit gray.
fn focus_patch(src: &[f32], w: usize, x0: usize, y0: usize, fw: usize, fh: usize) -> Vec<f64> {
    let n = (fw * fh) as f64;
    let mut sum = 0.0f64;
    let mut sum2 = 0.0f64;
    for y in 0..fh {
        for x in 0..fw {
            let v = src[(y0 + y) * w + x0 + x] as f64;
            sum += v;
            sum2 += v * v;
        }
    }
    let mean = sum / n;
    let std = (sum2 / n - mean * mean).max(0.0).sqrt() + 1e-6;
    let hx: Vec<f64> = (0..fw)
        .map(|x| {
            if fw <= 1 {
                1.0
            } else {
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * x as f64 / (fw - 1) as f64).cos())
            }
        })
        .collect();
    let hy: Vec<f64> = (0..fh)
        .map(|y| {
            if fh <= 1 {
                1.0
            } else {
                0.5 * (1.0 - (2.0 * std::f64::consts::PI * y as f64 / (fh - 1) as f64).cos())
            }
        })
        .collect();
    let mut out = Vec::with_capacity(fw * fh);
    for y in 0..fh {
        for x in 0..fw {
            let v = src[(y0 + y) * w + x0 + x] as f64;
            out.push(((v - mean) / std) * hx[x] * hy[y]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 平滑与残差
// ---------------------------------------------------------------------------

/// 局部中值基线 + 跳变保留。返回检测到的跳变下标（去重、有序）。
fn preserve_outliers(
    vals: &[f64],
    scores: &[f64],
    half: usize,
    jump_px: f64,
    min_score: f64,
) -> (Vec<f64>, Vec<usize>) {
    let n = vals.len();
    let mut out = vals.to_vec();
    let mut keep = Vec::new();
    let mut buf = Vec::new();
    for k in 0..n {
        let lo = k.saturating_sub(half);
        let hi = (k + half + 1).min(n);
        buf.clear();
        buf.extend_from_slice(&vals[lo..hi]);
        let med = median(&mut buf);
        if (vals[k] - med).abs() > jump_px {
            keep.push(k);
            let score = scores.get(k).copied().unwrap_or(0.0);
            if score < min_score {
                out[k] = med;
            }
        } else {
            out[k] = med;
        }
    }
    (out, keep)
}

/// 同 numpy.median：偶数个取中间两数均值。
fn median(buf: &mut [f64]) -> f64 {
    if buf.is_empty() {
        return 0.0;
    }
    buf.sort_by(f64::total_cmp);
    let n = buf.len();
    if n % 2 == 1 {
        buf[n / 2]
    } else {
        (buf[n / 2 - 1] + buf[n / 2]) * 0.5
    }
}

fn residual_roi(crop: CropRect, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let x1 = (crop.x as usize).min(w);
    let y1 = (crop.y as usize).min(h);
    let x2 = (crop.x as usize + crop.w as usize).min(w);
    let y2 = (crop.y as usize + crop.h as usize).min(h);
    let top = y1 + ((y2 - y1) as f64 * 0.6).round() as usize;
    (x1, top.min(y2), x2, y2)
}

/// 在 ROI 内按位移做双线性 warp 采样。
fn sample_roi(
    gray: &[u8],
    w: usize,
    h: usize,
    roi: (usize, usize, usize, usize),
    off: [f64; 2],
) -> Vec<f32> {
    let (x1, y1, x2, y2) = roi;
    let (dx, dy) = (off[0], off[1]);
    let mut out = Vec::with_capacity((x2 - x1) * (y2 - y1));
    for y in y1..y2 {
        for x in x1..x2 {
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
            let v00 = gray[y0 * w + x0] as f64;
            let v10 = gray[y0 * w + xa] as f64;
            let v01 = gray[ya * w + x0] as f64;
            let v11 = gray[ya * w + xa] as f64;
            let top = v00 * (1.0 - tx) + v10 * tx;
            let bot = v01 * (1.0 - tx) + v11 * tx;
            out.push((top * (1.0 - ty) + bot * ty) as f32);
        }
    }
    out
}

fn pearson(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let nf = n as f64;
    let mut sa = 0.0f64;
    let mut sb = 0.0f64;
    let mut saa = 0.0f64;
    let mut sbb = 0.0f64;
    let mut sab = 0.0f64;
    for i in 0..n {
        let (x, y) = (a[i] as f64, b[i] as f64);
        sa += x;
        sb += y;
        saa += x * x;
        sbb += y * y;
        sab += x * y;
    }
    let cov = sab - sa * sb / nf;
    let va = (saa - sa * sa / nf).max(0.0);
    let vb = (sbb - sb * sb / nf).max(0.0);
    let den = (va * vb).sqrt();
    if den < 1e-9 {
        if cov.abs() < 1e-9 { 1.0 } else { 0.0 }
    } else {
        (cov / den).clamp(-1.0, 1.0)
    }
}

/// 每批并行处理的帧数：按像素量限制并发放大，控制峰值内存。
fn frame_jobs(pixels: usize, total: usize) -> usize {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let cap = if pixels > 4_000_000 { 4 } else { 8 };
    threads.min(cap).min(total).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parabola_hits_peak() {
        assert!((parab(0.0, 1.0, 0.0) - 0.0).abs() < 1e-12);
        assert!((parab(0.0, 0.9, 1.0) - 0.625).abs() < 1e-9);
        assert!(parab(1.0, 1.0, 1.0).abs() < 1e-12);
    }

    #[test]
    fn median_matches_numpy_even() {
        let mut v = [4.0, 1.0, 3.0, 2.0];
        assert!((median(&mut v) - 2.5).abs() < 1e-12);
        let mut v = [9.0, 1.0, 5.0];
        assert!((median(&mut v) - 5.0).abs() < 1e-12);
    }

    #[test]
    fn clahe_keeps_shape_and_range() {
        let (w, h) = (64, 48);
        let mut gray: Vec<u8> = (0..w * h).map(|i| ((i * 37) % 256) as u8).collect();
        let before = gray.clone();
        clahe(&mut gray, w, h);
        assert_eq!(gray.len(), before.len());
        assert!(gray.windows(2).any(|p| p[0] != p[1]));
    }

    #[test]
    fn common_crop_never_leaves_frame() {
        let (w, h) = (320u32, 240u32);
        let offs = [[3.2, -7.5], [0.0, 0.0], [-11.3, 2.4], [1.0, 9.0]];
        let c = common_crop(&offs, w, h);
        assert!(c.w > 0 && c.h > 0);
        for o in offs {
            assert!(c.x as f64 - o[0] >= -1e-9);
            assert!((c.x + c.w) as f64 - o[0] <= w as f64 + 1e-9);
            assert!(c.y as f64 - o[1] >= -1e-9);
            assert!((c.y + c.h) as f64 - o[1] <= h as f64 + 1e-9);
        }
    }
}
