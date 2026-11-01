//! 逐帧测光：胶片条曲线、序列 QA、曝光台阶的共同输入。
//!
//! 亮度是 gamma 编码的灰度（「看起来多亮」），与去闪烁的线性域测光互相独立。
//! 缩到 512 长边用区域性平均（对应 OpenCV 的 INTER_AREA），比双线性更接近
//! 缩略图的真实平均亮度。
//!
//! Per-frame metering: the shared input of the filmstrip curve, sequence QA, and exposure
//! ladder.
//!
//! Brightness is gamma-encoded gray level, that is, how bright it looks, and is independent of
//! the linear-domain metering used by deflicker.
//! Downscaling to a 512 long side uses area averaging (OpenCV `INTER_AREA`), which stays closer
//! to the true average brightness of a thumbnail than bilinear sampling.

use std::path::{Path, PathBuf};

use image::{DynamicImage, RgbaImage};
use rayon::prelude::*;
use thiserror::Error;

use crate::deflicker;

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("读取失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("解码失败：{0}")]
    Image(#[from] image::ImageError),
    #[error("DNG 解码失败：{0}")]
    Dng(#[from] crate::dng::DngError),
    #[error("RAW 解码失败：{0}")]
    Raw(#[from] crate::raw::RawError),
    #[error("相机配置处理失败：{0}")]
    Profile(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameMetrics {
    /// 全帧平均亮度，0~1。
    ///
    /// Full-frame average brightness, 0~1.
    pub mean: f64,
    pub median: f64,
    /// 中间三分之一的平均亮度，避开暗角和边缘。
    ///
    /// Average brightness of the middle third, avoiding vignetting and the frame edges.
    pub center: f64,
    /// 拉普拉斯方差，找虚焦帧用。
    ///
    /// Laplacian variance, used to find out-of-focus frames.
    pub sharp: f64,
}

impl FrameMetrics {
    pub fn means(v: &[Option<FrameMetrics>]) -> Vec<Option<f64>> {
        v.iter().map(|m| m.map(|m| m.mean)).collect()
    }

    pub fn sharpness(v: &[Option<FrameMetrics>]) -> Vec<Option<f64>> {
        v.iter().map(|m| m.map(|m| m.sharp)).collect()
    }
}

pub fn measure_file(path: impl AsRef<Path>, long_side: u32) -> Result<FrameMetrics, MetricsError> {
    let img = decode(path)?;
    Ok(measure_image(&img, long_side))
}

/// 一次解码同时给出胶片条指标与去闪测光，省掉第二遍读图。
///
/// A single decode yields both the filmstrip metrics and the deflicker metering, saving a second
/// pass over the image.
pub fn measure_file_def(
    path: impl AsRef<Path>,
    long_side: u32,
) -> Result<(FrameMetrics, deflicker::FrameMeans), MetricsError> {
    let rgb = decode(path)?.to_rgb8();
    let scan =
        deflicker::measure_image(&fit_long_side(&rgb, deflicker::SCAN_LONG)).unwrap_or_default();
    Ok((measure_rgb8(&rgb, long_side), scan))
}

/// 等比缩到长边不超过 `max_side`；已经够小就原样返回。
///
/// Scale proportionally so the long side does not exceed `max_side`; an image that is already
/// small enough is returned unchanged.
pub fn fit_long_side(img: &image::RgbImage, max_side: u32) -> image::RgbImage {
    let (w, h) = (img.width(), img.height());
    let long = w.max(h).max(1);
    if long <= max_side {
        return img.clone();
    }
    let s = max_side as f64 / long as f64;
    resize_area(
        img,
        ((w as f64 * s).round() as u32).max(1),
        ((h as f64 * s).round() as u32).max(1),
    )
}

pub fn measure_image(img: &DynamicImage, long_side: u32) -> FrameMetrics {
    measure_rgb8(&img.to_rgb8(), long_side)
}

pub fn measure_rgb8(img: &image::RgbImage, long_side: u32) -> FrameMetrics {
    let (w, h) = (img.width(), img.height());
    let long = w.max(h).max(1);
    let (tw, th) = if long > long_side {
        let s = long_side as f64 / long as f64;
        (
            ((w as f64 * s).round() as u32).max(1),
            ((h as f64 * s).round() as u32).max(1),
        )
    } else {
        (w, h)
    };
    let gray_rgb = if (tw, th) == (w, h) {
        img.clone()
    } else {
        resize_area(img, tw, th)
    };
    let gray: Vec<u8> = gray_rgb
        .pixels()
        .map(|p| {
            let [r, g, b] = p.0;
            ((r as u32 * 4899 + g as u32 * 9617 + b as u32 * 1868 + 8192) >> 14) as u8
        })
        .collect();
    measure_gray(&gray, tw as usize, th as usize)
}

pub fn measure_gray(gray: &[u8], w: usize, h: usize) -> FrameMetrics {
    let n = gray.len();
    if n == 0 || w == 0 || h == 0 {
        return FrameMetrics {
            mean: 0.0,
            median: 0.0,
            center: 0.0,
            sharp: 0.0,
        };
    }
    let sum: f64 = gray.iter().map(|v| *v as f64).sum();
    let mean = sum / n as f64;

    let mut sorted = gray.to_vec();
    sorted.sort_unstable();
    let median = {
        let k = (n - 1) as f64 * 0.5;
        let lo = k.floor() as usize;
        let hi = k.ceil() as usize;
        if lo == hi {
            sorted[lo] as f64
        } else {
            (sorted[lo] as f64 + sorted[hi] as f64) * 0.5
        }
    };

    let cy = h / 3;
    let cx = w / 3;
    let center = if cy == 0 || cx == 0 {
        mean
    } else {
        let mut acc = 0.0;
        let mut cnt = 0usize;
        for y in cy..(2 * cy).min(h) {
            let row = &gray[y * w..y * w + w];
            for v in &row[cx..(2 * cx).min(w)] {
                acc += *v as f64;
                cnt += 1;
            }
        }
        if cnt == 0 { mean } else { acc / cnt as f64 }
    };

    FrameMetrics {
        mean: mean / 255.0,
        median: median / 255.0,
        center: center / 255.0,
        sharp: laplacian_var(gray, w, h),
    }
}

/// 3×3 拉普拉斯（核 [[0,1,0],[1,-4,1],[0,1,0]]）的方差，边界按 reflect101。
fn laplacian_var(gray: &[u8], w: usize, h: usize) -> f64 {
    if w < 3 || h < 3 {
        return 0.0;
    }
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    for y in 0..h {
        for x in 0..w {
            let c = gray[y * w + x] as f64;
            let up = gray[reflect101(y as isize - 1, h) * w + x] as f64;
            let down = gray[reflect101(y as isize + 1, h) * w + x] as f64;
            let left = gray[y * w + reflect101(x as isize - 1, w)] as f64;
            let right = gray[y * w + reflect101(x as isize + 1, w)] as f64;
            let v = up + down + left + right - 4.0 * c;
            sum += v;
            sum_sq += v * v;
        }
    }
    let n = (w * h) as f64;
    let mean = sum / n;
    (sum_sq / n - mean * mean).max(0.0)
}

fn reflect101(mut i: isize, n: usize) -> usize {
    let n = n as isize;
    if n <= 1 {
        return 0;
    }
    while i < 0 || i >= n {
        if i < 0 {
            i = -i;
        } else {
            i = 2 * (n - 1) - i;
        }
    }
    i as usize
}

/// 区域性平均缩放（INTER_AREA 的加权平均），先横后纵。
///
/// 两趟都按输出行并行：横趟每行只读自己那条源行、只写自己那条临时行，纵趟每行也只写
/// 自己的目标行，所以拆线程不改任何一个数值。素材是 24MP RAW 时这一趟本来要
/// 200~400ms，缩略图、测光、预览全都卡在它上面。
///
/// Area-averaging resize, the weighted mean of INTER_AREA, with the horizontal pass first and
/// the vertical pass second.
///
/// Both passes run in parallel over output rows: in the horizontal pass each row reads only its
/// own source row and writes only its own temporary row, and in the vertical pass each row writes
/// only its own target row, so splitting across threads changes no value.
/// On 24MP RAW material this pass used to take 200~400ms, and thumbnails, metering, and previews
/// all waited on it.
pub fn resize_area(img: &image::RgbImage, tw: u32, th: u32) -> image::RgbImage {
    let (sw, sh) = (img.width(), img.height());
    if (sw, sh) == (tw, th) || tw == 0 || th == 0 {
        return img.clone();
    }
    let (twu, swu) = (tw as usize, sw as usize);
    let wx = area_weights(sw, tw);
    let wy = area_weights(sh, th);
    let src = img.as_raw();
    let mut tmp = vec![0.0f32; twu * sh as usize * 3];
    tmp.par_chunks_mut(twu * 3)
        .enumerate()
        .for_each(|(y, row)| {
            let base = y * swu * 3;
            for (dx, ws) in wx.iter().enumerate() {
                let mut acc = [0.0f32; 3];
                for (sx, wt) in ws {
                    let p = &src[base + *sx as usize * 3..][..3];
                    acc[0] += p[0] as f32 * wt;
                    acc[1] += p[1] as f32 * wt;
                    acc[2] += p[2] as f32 * wt;
                }
                row[dx * 3..dx * 3 + 3].copy_from_slice(&acc);
            }
        });
    let mut out = image::RgbImage::new(tw, th);
    out.as_mut()
        .par_chunks_mut(twu * 3)
        .enumerate()
        .for_each(|(dy, row)| {
            for (x, px) in row.chunks_exact_mut(3).enumerate() {
                let mut acc = [0.0f32; 3];
                for (sy, wt) in &wy[dy] {
                    let o = (*sy as usize * twu + x) * 3;
                    acc[0] += tmp[o] * wt;
                    acc[1] += tmp[o + 1] * wt;
                    acc[2] += tmp[o + 2] * wt;
                }
                px[0] = acc[0].round().clamp(0.0, 255.0) as u8;
                px[1] = acc[1].round().clamp(0.0, 255.0) as u8;
                px[2] = acc[2].round().clamp(0.0, 255.0) as u8;
            }
        });
    out
}

fn area_weights(src: u32, dst: u32) -> Vec<Vec<(u32, f32)>> {
    let scale = src as f64 / dst as f64;
    let last = src as f64;
    (0..dst)
        .map(|d| {
            // 两端都钉回 [0, src]：`end` 在浮点下可能比 src 大出 1e-13，
            // 那一丁点误差会让最后一个采样下标正好落在数组之外。
            let start = (d as f64 * scale).min(last);
            let end = ((d + 1) as f64 * scale).clamp(start, last);
            let span = (end - start).max(1e-9);
            let mut v = Vec::new();
            let mut j = start.floor() as u32;
            while (j as f64) < end {
                let w = (end.min(j as f64 + 1.0) - start.max(j as f64)).max(0.0);
                if w > 0.0 {
                    v.push((j, (w / span) as f32));
                }
                j += 1;
            }
            v
        })
        .collect()
}

/// 解码并按 EXIF 方向转正。
///
/// image 库不会自己应用方向（只把它挂在 `decoder.orientation()` 上），手机/相机
/// 竖拍的 JPEG 直接解出来是横躺的。预览、测光、缩略图、导出、对齐都从这一个口子
/// 进，方向的语义只有一份。
///
/// `.dng` 也从这个口子进：image 库不认 CFA 马赛克，只有 `dng::read` 解得开，解出来
/// 是 16bit RGB，所以下游的位深判定天然按高位深走。
///
/// 厂商 RAW（`.rw2`/`.nef`/`.arw`/`.cr2`/`.cr3`/`.orf`/`.rwl`）同理由：`raw::read`
/// 两条路（传感器 CFA 或机身内嵌预览）都返回 16bit RGB，位深口径只有一套；
/// **保真度**不在这里表达，要标注的调用方读 `raw::read`/`raw::info` 返回的
/// `raw::RawPath`（预览不是传感器数据）。
///
/// Decode and rotate upright according to the EXIF orientation.
///
/// The image crate does not apply orientation on its own, exposing it only through
/// `decoder.orientation()`, so a portrait JPEG from a phone or camera decodes lying on its side.
/// Preview, metering, thumbnails, export, and alignment all enter through this one door, so the
/// semantics of orientation exist in a single place.
///
/// `.dng` also enters here: the image crate does not understand CFA mosaics and only `dng::read`
/// can decode them, yielding 16bit RGB, so downstream bit-depth decisions follow the high-bit-depth
/// path by construction.
///
/// Vendor RAW (`.rw2`/`.nef`/`.arw`/`.cr2`/`.cr3`/`.orf`/`.rwl`) works the same way: both paths of
/// `raw::read` (sensor CFA or camera-embedded preview) return 16bit RGB, keeping a single
/// bit-depth convention; **fidelity** is not expressed here, and callers that need to label it
/// read the `raw::RawPath` returned by `raw::read`/`raw::info`, where a preview is not sensor
/// data.
pub fn decode(path: impl AsRef<Path>) -> Result<DynamicImage, MetricsError> {
    let path = path.as_ref();
    if crate::dng::is_named_dng(path) {
        let mosaic = DynamicImage::ImageRgb16(crate::dng::read(path)?);
        return Ok(crate::exif::oriented(
            mosaic,
            crate::exif::read(path).orientation,
        ));
    }
    if crate::raw::is_raw_name(path) {
        // 走了哪条路在这里不能丢：`raw::read` 已经把它当第二个返回值摆出来了。
        // 方向用后端报的：CR3 的 Exif item 在 BMFF 容器里，`exif::read` 读不到，
        // rawler 能——竖拍躺平就是在这里漏的方向。
        let (mosaic, _path, orientation) = crate::raw::read_full(path)?;
        return Ok(crate::exif::oriented(
            DynamicImage::ImageRgb16(mosaic),
            orientation,
        ));
    }
    let reader = image::ImageReader::open(path)?.with_guessed_format()?;
    let img = reader.decode()?;
    Ok(crate::exif::oriented(
        img,
        crate::exif::read(path).orientation,
    ))
}

/// 解码并等比缩到长边不超过 `max_side`，返回 sRGB 像素，供画布预览用。
///
/// Decode and scale proportionally so the long side does not exceed `max_side`, returning sRGB
/// pixels for canvas preview.
pub fn decode_rgb(path: impl AsRef<Path>, max_side: u32) -> Result<image::RgbImage, MetricsError> {
    let img = decode(path)?;
    Ok(fit_long_side(&img.to_rgb8(), max_side))
}

/// 按源位深解码成 sRGB f32 `[0,1]`，返回（像素、宽、高、源是否高于 8bit）。
///
/// 预览和测光可以继续在 8bit 缩略图上跑，但导出不能：`to_rgb8()` 会把 16bit 源
/// 一次砍到 256 级，后面所有算法都在 f32 里也算不回被丢掉的位深。位深判定跟着
/// `export::load_rgb_f32` 同一套 match，两条导出路对「什么是高位深源」要说一样的话。
///
/// Decode into sRGB f32 `[0,1]` according to the source bit depth, returning the pixels, width,
/// height, and whether the source is above 8bit.
///
/// Preview and metering can keep running on an 8bit thumbnail, but export cannot: `to_rgb8()` cuts
/// a 16bit source down to 256 levels in one step, and no later algorithm in f32 can recover the
/// lost bit depth.
/// The bit-depth decision uses the same match as `export::load_rgb_f32`, so both export paths say
/// the same thing about what counts as a high-bit-depth source.
pub fn decode_rgb_f32(path: impl AsRef<Path>) -> Result<(Vec<f32>, u32, u32, bool), MetricsError> {
    let img = decode(path)?;
    let (w, h) = (img.width(), img.height());
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    let deep = match img.color() {
        image::ColorType::L16
        | image::ColorType::La16
        | image::ColorType::Rgb16
        | image::ColorType::Rgba16 => {
            for p in img.to_rgb16().pixels() {
                rgb.extend_from_slice(&[
                    p.0[0] as f32 / 65535.0,
                    p.0[1] as f32 / 65535.0,
                    p.0[2] as f32 / 65535.0,
                ]);
            }
            true
        }
        image::ColorType::Rgb32F | image::ColorType::Rgba32F => {
            for p in img.to_rgb32f().pixels() {
                rgb.extend_from_slice(&[p.0[0], p.0[1], p.0[2]]);
            }
            true
        }
        _ => {
            for p in img.to_rgb8().pixels() {
                rgb.extend_from_slice(&[
                    p.0[0] as f32 / 255.0,
                    p.0[1] as f32 / 255.0,
                    p.0[2] as f32 / 255.0,
                ]);
            }
            false
        }
    };
    Ok((rgb, w, h, deep))
}

pub fn decode_for_grade(
    path: impl AsRef<Path>,
    params: &crate::grade::GradeParams,
) -> Result<(Vec<f32>, u32, u32, bool), MetricsError> {
    if params.dcp_profile.is_empty() {
        decode_rgb_f32(path)
    } else {
        crate::dcp::decode(path.as_ref(), Path::new(&params.dcp_profile))
            .map_err(MetricsError::Profile)
    }
}

pub fn decode_profile_preview(
    path: &Path,
    profile: &str,
    max_side: u32,
) -> Result<(Vec<f32>, usize, usize), MetricsError> {
    if profile.is_empty() {
        let rgb = decode_rgb(path, max_side)?;
        let (w, h) = (rgb.width() as usize, rgb.height() as usize);
        let pixels = rgb.as_raw().iter().map(|v| *v as f32 / 255.0).collect();
        return Ok((pixels, w, h));
    }
    let (rgb, w, h, _) =
        crate::dcp::decode(path, Path::new(profile)).map_err(MetricsError::Profile)?;
    let scale = (max_side as f64 / w.max(h) as f64).min(1.0);
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    if (nw, nh) == (w, h) {
        return Ok((rgb, w as usize, h as usize));
    }
    let image = image::Rgb32FImage::from_raw(w, h, rgb)
        .ok_or_else(|| MetricsError::Profile("相机配置输出尺寸错误".to_owned()))?;
    let resized = image::imageops::resize(&image, nw, nh, image::imageops::FilterType::Triangle);
    Ok((resized.into_raw(), nw as usize, nh as usize))
}

/// 解码并等比缩到长边不超过 `max_side`，供画布和胶片条用。
///
/// RAW/DNG 先试机身内嵌预览：胶片条要的是一张 128 边的图，为此把 24MP 传感器数据
/// 解出来（约 300ms）纯属浪费。内嵌预览的像素本来就是机身按 As Shot 出的一张 8 位
/// JPEG，缩到这个尺寸看不出差别；没有内嵌预览才退回完整解码。
///
/// Decode and scale proportionally to a long side of at most `max_side`, for the canvas and the
/// filmstrip.
///
/// For RAW/DNG the camera-embedded preview is tried first: the filmstrip needs a 128-pixel image,
/// and decoding 24MP sensor data for it (about 300ms) is pure waste.
/// The embedded preview pixels are an 8-bit JPEG the camera rendered with As Shot settings, and
/// the difference is invisible at this size; a full decode is the fallback only when no embedded
/// preview exists.
pub fn thumbnail(path: impl AsRef<Path>, max_side: u32) -> Result<RgbaImage, MetricsError> {
    let path = path.as_ref();
    if let Some(hit) = read_thumb_cache(path, max_side) {
        return Ok(hit);
    }
    let img = match embedded_preview(path, max_side) {
        Some(img) => img,
        None => decode(path)?,
    };
    let out = resize_to_rgba(&img.to_rgb8(), max_side);
    write_thumb_cache(path, max_side, &out);
    Ok(out)
}

/// 缩略图磁盘缓存：`<序列根>/.aevum/thumbs/<名>-<mtime>-<字节数>-<边长>.png`。
///
/// 文件名里带修改时间与字节数，源帧被覆盖就自然失效，不需要另存一份索引。写失败一律
/// 当成没写（只读目录、盘满都算）——缓存坏掉最多退回到慢路径，不该让缩略图消失。
fn thumb_cache_path(path: &Path, max_side: u32) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let stem: String = path
        .file_stem()?
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = crate::project::guard_dir(path.parent()?).join("thumbs");
    Some(dir.join(format!("{stem}-{mtime}-{}-{max_side}.png", meta.len())))
}

fn read_thumb_cache(path: &Path, max_side: u32) -> Option<RgbaImage> {
    let file = thumb_cache_path(path, max_side)?;
    let img = image::open(file).ok()?;
    Some(img.to_rgba8())
}

fn write_thumb_cache(path: &Path, max_side: u32, img: &RgbaImage) {
    let Some(file) = thumb_cache_path(path, max_side) else {
        return;
    };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = img.save(&file);
}

fn embedded_preview(path: &Path, max_side: u32) -> Option<DynamicImage> {
    if !(crate::raw::is_raw_name(path) || crate::dng::is_named_dng(path)) {
        return None;
    }
    // 只要有目标两倍大的那张：128 边缩略图取 1920 边预览，解它十几毫秒；
    // 机身只写了一张全尺寸预览时才退到它，不够大（比目标还小）也退。
    let mosaic = crate::raw::preview_at_least(path, max_side.saturating_mul(2))
        .ok()
        .or_else(|| crate::raw::preview_only(path).ok())?;
    Some(crate::exif::oriented(
        DynamicImage::ImageRgb16(mosaic),
        crate::exif::read(path).orientation,
    ))
}

fn resize_to_rgba(rgb: &image::RgbImage, max_side: u32) -> RgbaImage {
    let (w, h) = (rgb.width(), rgb.height());
    let long = w.max(h).max(1);
    if long <= max_side {
        return DynamicImage::ImageRgb8(rgb.clone()).to_rgba8();
    }
    let s = max_side as f64 / long as f64;
    let tw = ((w as f64 * s).round() as u32).max(1);
    let th = ((h as f64 * s).round() as u32).max(1);
    let small = resize_area(rgb, tw, th);
    let mut out = RgbaImage::new(tw, th);
    for (x, y, p) in small.enumerate_pixels() {
        out.put_pixel(x, y, image::Rgba([p.0[0], p.0[1], p.0[2], 255]));
    }
    out
}

/// 一帧的两套测光结果：胶片条指标 + 去闪用的线性域均值。
type FrameMeasures = Result<(FrameMetrics, deflicker::FrameMeans), MetricsError>;

/// `measure_many` 的返回：逐帧指标、逐帧均值、`(帧号, 原因)` 形式的坏帧清单。
///
/// Return value of `measure_many`: per-frame metrics, per-frame means, and a list of bad frames
/// in `(frame number, reason)` form.
pub type MeasurePass = (
    Vec<Option<FrameMetrics>>,
    Vec<Option<deflicker::FrameMeans>>,
    Vec<(usize, String)>,
);

/// 并行测一整个序列：一次解码同时出胶片条指标与去闪测光（线性域、长边 320），
/// 结果顺序与输入一致；读不出来的帧留 `None` 并记一条 issue。
///
/// Meter an entire sequence in parallel: a single decode yields both the filmstrip metrics and the
/// deflicker metering (linear domain, long side 320), and the result order matches the input; a
/// frame that cannot be read stays `None` and records one issue.
pub fn measure_many(paths: &[PathBuf], long_side: u32) -> MeasurePass {
    let results: Vec<(usize, FrameMeasures)> = paths
        .par_iter()
        .enumerate()
        .map(|(i, p)| (i, measure_file_def(p, long_side)))
        .collect();
    let mut out = vec![None; paths.len()];
    let mut defs = vec![None; paths.len()];
    let mut issues = Vec::new();
    for (i, r) in results {
        match r {
            Ok((m, d)) => {
                out[i] = Some(m);
                defs[i] = Some(d);
            }
            Err(e) => issues.push((i, format!("{e}"))),
        }
    }
    issues.sort_by_key(|(i, _)| *i);
    (out, defs, issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_weights_sum_to_one() {
        for (s, d) in [(1920u32, 512u32), (100, 33), (7, 3), (512, 512)] {
            for ws in area_weights(s, d) {
                let total: f32 = ws.iter().map(|(_, w)| *w).sum();
                assert!((total - 1.0).abs() < 1e-4, "{s}->{d}: {total}");
            }
        }
    }

    #[test]
    fn area_weights_indices_stay_inside_source() {
        // 4000→85 是真实触发过的一档：松下 RW2 的 6000×4000 预览缩到胶片条宽度时，
        // `end` 浮点算出 4000.0000000000005，最后一个采样下标正好越到数组外。
        for s in [1u32, 7, 85, 512, 1000, 4000, 6000, 6001] {
            for d in 1u32..=140 {
                for ws in area_weights(s, d) {
                    for &(i, _) in &ws {
                        assert!(i < s, "{s}->{d} 取到了源外下标 {i}");
                    }
                }
            }
        }
    }

    #[test]
    fn flat_gray_has_zero_lap_variance() {
        let g = vec![128u8; 32 * 24];
        let m = measure_gray(&g, 32, 24);
        assert!((m.mean - 128.0 / 255.0).abs() < 1e-12);
        assert_eq!(m.sharp, 0.0);
    }

    #[test]
    fn reflect101_clamps_edges() {
        assert_eq!(reflect101(-1, 5), 1);
        assert_eq!(reflect101(5, 5), 3);
        assert_eq!(reflect101(-3, 5), 3);
    }
}
