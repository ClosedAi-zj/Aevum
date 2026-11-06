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

use crate::deflicker;

#[derive(Debug)]
pub enum MetricsError {
    Io(std::io::Error),
    Image(image::ImageError),
    Dng(crate::dng::DngError),
    Raw(crate::raw::RawError),
    Profile(String),
    /// 解码器在坏字节上 panic 了，被 `raw::guard` 收成错误。
    ///
    /// 与 [`Self::Raw`] 分开是因为它可能来自**普通图片**（jpg/png）的 `image` 解码，
    /// 那时候说「RAW 解码失败」是错的；这一条只讲「谁崩了」，不假设文件类型。
    ///
    /// A decoder panicked on bad bytes and `raw::guard` caught it.
    ///
    /// This is separate from [`Self::Raw`] because it can come from the `image` decoder of a
    /// **plain picture** (jpg/png), where "RAW decode failed" would be wrong; this variant
    /// says who crashed and assumes nothing about the file type.
    DecoderPanic(crate::raw::guard::DecoderPanic),
}

impl crate::errmsg::Msg for MetricsError {
    fn msg(&self) -> (&'static str, Vec<crate::errmsg::Arg<'_>>) {
        match self {
            Self::Io(v) => (
                "读取失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::Image(v) => (
                "解码失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::Dng(v) => ("DNG 解码失败：{}", vec![crate::errmsg::Arg::Nested(v)]),
            Self::Raw(v) => ("RAW 解码失败：{}", vec![crate::errmsg::Arg::Nested(v)]),
            Self::Profile(v) => (
                "相机配置处理失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::DecoderPanic(p) => match p.detail() {
                Some(detail) => (
                    "{} 解码器内部出错，这张先跳过（不是你的文件坏了）：{}",
                    vec![
                        crate::errmsg::Arg::Text(p.backend.to_owned()),
                        crate::errmsg::Arg::Text(detail.to_owned()),
                    ],
                ),
                None => (
                    "{} 解码器内部出错，这张先跳过（不是你的文件坏了）",
                    vec![crate::errmsg::Arg::Text(p.backend.to_owned())],
                ),
            },
        }
    }
}

crate::impl_msg_display!(MetricsError);

impl std::error::Error for MetricsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Dng(e) => Some(e),
            Self::Raw(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MetricsError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<image::ImageError> for MetricsError {
    fn from(e: image::ImageError) -> Self {
        Self::Image(e)
    }
}

impl From<crate::dng::DngError> for MetricsError {
    fn from(e: crate::dng::DngError) -> Self {
        Self::Dng(e)
    }
}

impl From<crate::raw::guard::DecoderPanic> for MetricsError {
    fn from(e: crate::raw::guard::DecoderPanic) -> Self {
        Self::DecoderPanic(e)
    }
}

impl From<crate::raw::RawError> for MetricsError {
    fn from(e: crate::raw::RawError) -> Self {
        Self::Raw(e)
    }
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

/// 像素是从哪条路解出来的。**白平衡口径由它决定**：内置 DNG 解析不解白平衡，
/// RAW 后端已经按文件里的 AsShotNeutral 做过一次增益，配置再乘一次就是双份。
///
/// 分开报是因为「谁做的白平衡」不能靠文件后缀猜：同一张 `.dng` 既可能被内置解析器
/// 接下来（单通道 CFA），也可能落到后端（三通道 LinearRaw），两条路的像素口径不同。
///
/// Which route produced the pixels. **The white-balance convention depends on it**: the built-in
/// DNG parser does not apply white balance, while the RAW backend has already applied the gains
/// from the file's AsShotNeutral, so applying the profile's again would double it.
///
/// It is reported separately because "who did the white balance" cannot be guessed from the file
/// suffix: the same `.dng` may be handled by the built-in parser (single-channel CFA) or fall
/// through to the backend (three-channel LinearRaw), and the two routes yield different pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelOrigin {
    /// 普通图像文件（JPEG/PNG/TIFF…），image 库解的，已是成品色彩。
    ///
    /// An ordinary image file (JPEG/PNG/TIFF, ...) decoded by the image crate, already
    /// colour-finished.
    Image,
    /// 内置 DNG 解析器（单通道 CFA/LinearRaw），**没做白平衡**。
    ///
    /// The built-in DNG parser (single-channel CFA/LinearRaw), with **no white balance**.
    DngBuiltin,
    /// RAW 后端（rawler 等），**已按文件里的增益做过白平衡**。
    ///
    /// The RAW backend (rawler, ...), with white balance **already applied from the file**.
    RawBackend,
}

/// 解码并按 EXIF 方向转正。
///
/// image 库不会自己应用方向（只把它挂在 `decoder.orientation()` 上），手机/相机
/// 竖拍的 JPEG 直接解出来是横躺的。预览、测光、缩略图、导出、对齐都从这一个口子
/// 进，方向的语义只有一份。
///
/// `.dng` 也从这个口子进：image 库不认 CFA 马赛克，只有 `dng::read` 解得开，解出来
/// 是 16bit RGB，所以下游的位深判定天然按高位深走。内置解析器只解单通道原始数据，
/// **三通道 LinearRaw（iPhone 导出的 DNG 就是）会回落 RAW 后端**——拒绝它是能力
/// 边界而不是文件坏了，见 [`crate::dng::DngError::soft_fallback`]。
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
/// path by construction. The built-in parser only handles single-channel raw data, so
/// **three-channel LinearRaw (which is what an iPhone-exported DNG is) falls back to the RAW
/// backend**: rejecting it is a capability limit, not corruption, see
/// [`crate::dng::DngError::soft_fallback`].
///
/// Vendor RAW (`.rw2`/`.nef`/`.arw`/`.cr2`/`.cr3`/`.orf`/`.rwl`) works the same way: both paths of
/// `raw::read` (sensor CFA or camera-embedded preview) return 16bit RGB, keeping a single
/// bit-depth convention; **fidelity** is not expressed here, and callers that need to label it
/// read the `raw::RawPath` returned by `raw::read`/`raw::info`, where a preview is not sensor
/// data.
pub fn decode(path: impl AsRef<Path>) -> Result<DynamicImage, MetricsError> {
    Ok(decode_with_origin(path)?.0)
}

/// 普通图片（非 RAW、非 DNG）的单边像素上限。容器格式大多用 u16/u32 存宽高，
/// 65535 是「格式说得出口」的上限；判据里那张声明 100000×100000 的靶子要靠它拒。
///
/// Per-side pixel cap for plain pictures (not RAW, not DNG). Most containers store width and
/// height as u16/u32, so 65535 is the largest value a format can legitimately express; the
/// fixture declaring 100000×100000 in the assertions relies on it to be refused.
const MAX_IMAGE_SIDE: u32 = 65_535;

/// [`decode`] 的完整版：连「像素从哪条路来」一起报。
///
/// 这一层同时是**panic 的最后一道闸**：从路径到像素只有这一个漏斗（序列解码、测光、
/// 缩略图、导出都走它），而漏斗里既有第三方解码器，也有本仓自己写的 TIFF/DNG 解析器——
/// 后者同样吃不可信字节。兜在这里，任何一条路径的 panic 都只变成「这一帧跳过」，
/// 不会掀掉整个序列。分级的精度仍然在各条支路自己的守卫上（那里知道是谁崩的）。
///
/// [`decode`] with the route also reported.
///
/// This layer is also the **last panic gate**: there is exactly one funnel from a path to
/// pixels (sequence decode, metering, thumbnails and export all pass through it), and the
/// funnel holds third-party decoders as well as this repository's own TIFF/DNG parsers, which
/// eat untrusted bytes just the same. Catching here turns any path's panic into "skip this
/// frame" rather than losing the whole sequence. Precise attribution still lives in each
/// branch's own guard, which knows who crashed.
pub fn decode_with_origin(
    path: impl AsRef<Path>,
) -> Result<(DynamicImage, PixelOrigin), MetricsError> {
    let path = path.as_ref();
    // `&Path` 是 `UnwindSafe`：守卫只读它，panic 之后没有任何半成品被复用。
    // `&Path` is `UnwindSafe`: the guard only reads it and no half-built state survives.
    crate::raw::guard::catch_decoder_panic("aevum", move || decode_dispatch(path))
        .map_err(MetricsError::from)?
}

/// [`decode_with_origin`] 的真身：分派到 DNG / 厂商 RAW / 普通图片三条支路。
///
/// The real body of [`decode_with_origin`]: dispatch to the DNG, vendor RAW and plain image
/// branches.
fn decode_dispatch(path: &Path) -> Result<(DynamicImage, PixelOrigin), MetricsError> {
    // 认得但这一版没有解码器的后缀（HEIC/HEIF、各家 RAW）：说得出名字就说名字。
    // 让它掉进下面 image crate 的「格式无法确定」会把「我们不做」说成「你的文件坏了」。
    if let Some(err) = crate::raw::unsupported_extension(path) {
        return Err(err.into());
    }
    let is_dng = crate::dng::is_named_dng(path);
    if is_dng {
        match crate::dng::read(path) {
            Ok(mosaic) => {
                return Ok((
                    crate::exif::oriented(
                        DynamicImage::ImageRgb16(mosaic),
                        crate::exif::read(path).orientation,
                    ),
                    PixelOrigin::DngBuiltin,
                ));
            }
            // 能力边界（三通道 LinearRaw、瓦片、非 8/16 位…）：后端再试一次。
            // 字节层面坏掉的不在此列，白读一遍文件不解决问题，直接把内置的具体错误串
            // 抛出去——它对 TIFF 结构的描述比「后端解不动」有用得多。
            Err(e) if e.soft_fallback() => {}
            Err(e) => return Err(e.into()),
        }
    }
    if is_dng || crate::raw::is_raw_name(path) {
        // 走了哪条路在这里不能丢：`raw::read` 已经把它当第二个返回值摆出来了。
        // 方向用后端报的：CR3 的 Exif item 在 BMFF 容器里，`exif::read` 读不到，
        // rawler 能——竖拍躺平就是在这里漏的方向。
        let (mosaic, _path, orientation) = crate::raw::read_full(path)?;
        return Ok((
            crate::exif::oriented(DynamicImage::ImageRgb16(mosaic), orientation),
            PixelOrigin::RawBackend,
        ));
    }
    let mut reader = image::ImageReader::open(path)?.with_guessed_format()?;
    // `ImageReader` 默认就带 `Limits`（单边不设限，`max_alloc` 512 MB），所以
    // 「200 KB 的 PNG 解出 4 GB」这条路本来就走不通——分配在 512 MB 处被拒。
    // 这一道是**再往前推一步**：单边也钉死，让「声明 100000×100000」的文件在
    // 读 IHDR 之后立刻被拒，而不是先尝试分配再失败。100 MP 的全景扫描件远在下面。
    //
    // `ImageReader` already carries `Limits` by default (no per-side limit, `max_alloc` 512
    // MB), so "a 200 KB PNG expanding to 4 GB" is not reachable -- the allocation is refused
    // at 512 MB. This adds **one step earlier**: the sides are pinned too, so a file
    // declaring 100000×100000 is refused right after the header instead of being refused
    // after an allocation attempt. A 100 MP panorama scan sits far below this.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    reader.limits(limits);
    // 截断 JPEG 的静默半截：zune-jpeg 对截断流**不报错**，把缺的行填成中性灰
    // (128,128,128) 照常返回成功——半张灰图会被测光放行、被合成烤进成片，全链路
    // 无一句提示（PNG/TIFF/WebP 的解码器都会报错，这道校验只钉 JPEG）。EOI 标记
    // （FF D9）是熵编码数据的合法终点：熵数据里的 FF 都被 00 填充（byte stuffing），
    // 正文不会出现裸 FFD9，所以「最后一个 EOI 之后只许剩填充字节」是完整的充要近似。
    // EXIF 内嵌缩略图也有自己的 EOI，但它在前段、后面跟着大段非填充字节，会被这
    // 条规则正确判成截断。
    //
    // Truncated-JPEG silent half: zune-jpeg does **not** error on a truncated stream; it
    // fills the missing rows with neutral gray (128,128,128) and returns success -- half a
    // gray image passes metering, gets baked into stacks, with no warning anywhere (the
    // PNG/TIFF/WebP decoders all error; this check pins JPEG only). The EOI marker (FF D9)
    // is the only legal end of entropy-coded data: every FF inside it is 00-stuffed (byte
    // stuffing), so a bare FFD9 never appears in the payload and "only padding may follow
    // the last EOI" is a sound approximation of completeness. An embedded EXIF thumbnail
    // carries its own EOI early in the file, followed by a large run of non-padding bytes,
    // which this rule correctly reports as truncated.
    if reader.format() == Some(image::ImageFormat::Jpeg) {
        let bytes = std::fs::read(path).map_err(MetricsError::Io)?;
        if !jpeg_stream_complete(&bytes) {
            return Err(MetricsError::Image(image::ImageError::Decoding(
                image::error::DecodingError::new(
                    image::error::ImageFormatHint::Exact(image::ImageFormat::Jpeg),
                    "文件不完整：JPEG 流没有正常的结束标记（多半是传输或拷贝中断）",
                ),
            )));
        }
    }
    // 普通图的解压同样在第三方代码里（jpge/zune/zune-inflate），坏流一样会 panic。
    // 守卫只包 `decode()`：`open`/`with_guessed_format` 的失败要保持原来的
    // 「读取失败」/「解码失败」两种口径，不能都改说成 panic。
    //
    // Decompression of a plain picture also happens in third-party code (jpge/zune), and a
    // broken stream panics there too. The guard wraps only `decode()`: failures from
    // `open`/`with_guessed_format` must keep their original "read failed" / "decode failed"
    // wording instead of being reported as panics.
    let img = crate::raw::guard::catch_decoder_panic(
        "image",
        std::panic::AssertUnwindSafe(move || reader.decode()),
    )
    .map_err(MetricsError::from)??;
    Ok((
        crate::exif::oriented(img, crate::exif::read(path).orientation),
        PixelOrigin::Image,
    ))
}

/// JPEG 流完整性：从首个 SOI 起按段结构走，最后一个 SOS 的熵数据必须以 EOI 收尾。
///
/// 为什么不按「最后一个 EOI 之后只许填充」判：真机（Panasonic）的 JPEG 在主流 EOI
/// 之后还有几百字节厂商尾注元数据，那条规则会把完好的真机文件误杀成截断。
/// 为什么不按「最后一个 SOS 之后有没有 EOI」裸扫：EXIF 缩略图是嵌在 APP1 载荷里的
/// 完整 JPEG（自带 SOI/SOS/EOI），裸扫会把缩略图的 SOS 当主流的。所以老老实实按段
/// 走：常规段按长度跳过（APP1 连同里面的缩略图整体跳过），SOS 之后进入熵数据扫描。
/// 熵数据经过字节填充（数据里的 0xFF 一律写成 0xFF 0x00），流内唯一的合法标记是
/// RSTn，所以扫描不会把数据误认成标记边界。截断的流死在熵数据中途：走不到 EOI
/// 就撞上 EOF → false；完整流在 EOI 收尾，EOI 之后的尾注/填充不再看 → true。
fn jpeg_stream_complete(bytes: &[u8]) -> bool {
    // 首个 SOI（FF D8）：没有就不是 JPEG 流。
    let Some(start) =
        (0..bytes.len().saturating_sub(1)).find(|&i| bytes[i] == 0xFF && bytes[i + 1] == 0xD8)
    else {
        return false;
    };
    let mut i = start + 2;
    loop {
        // 扫到下一个标记：0xFF 后跟非填充（0x00/0xFF）、非 RSTn 的字节。
        let marker = loop {
            let Some(off) = bytes[i..].iter().position(|&b| b == 0xFF) else {
                return false; // EOF 前再无 0xFF：熵数据没写完
            };
            i += off;
            match bytes.get(i + 1).copied() {
                None => return false,                            // 0xFF 是最后一个字节：截断
                Some(0x00) => i += 2, // 熵数据字节填充：0xFF 0x00 代表数据 0xFF
                Some(0xFF) => i += 1, // 填充串：越过这个 0xFF 继续
                Some(m) if (0xD0..=0xD7).contains(&m) => i += 2, // RSTn：流内合法标记
                Some(m) => break m,
            }
        };
        match marker {
            // EOI：流完整收尾。之后的字节（厂商尾注/填充）不再看。
            0xD9 => return true,
            // TEM / 独立 SOI：无长度段。
            0x01 | 0xD8 => i += 2,
            _ => {
                // 常规段：跳过 FF xx + 2 字节长度 + 载荷。SOS 的长度只盖头部，
                // 熵数据的边界未知，跳完头部由下一轮扫描去找终止标记。
                let Some(len) = bytes
                    .get(i + 2..i + 4)
                    .map(|l| u16::from_be_bytes([l[0], l[1]]) as usize)
                else {
                    return false; // 长度字段被截断
                };
                if len < 2 {
                    return false; // 长度不含自身，小于 2 是坏流
                }
                i += 2 + len;
                if i > bytes.len() {
                    return false; // 段载荷被截断
                }
            }
        }
    }
}

/// 解码并等比缩到长边不超过 `max_side`，返回 sRGB 像素，供画布预览用。
///
/// Decode and scale proportionally so the long side does not exceed `max_side`, returning sRGB
/// pixels for canvas preview.
pub fn decode_rgb(path: impl AsRef<Path>, max_side: u32) -> Result<image::RgbImage, MetricsError> {
    let (img, origin) = decode_with_origin(path)?;
    // 传感器源（DngBuiltin / RawBackend）在此做 sRGB 显示编码后再缩 8 位：
    // 这是画布预览/测光/缩略图走的口子，与 `decode_rgb_f32_with_origin` 的全量口
    // 同一约定（「复位变黑」修复）。成品文件（Image）不动。编码在缩放前做，
    // 8 位量化才落在编码域的正确台阶上。
    //
    // Sensor sources (DngBuiltin / RawBackend) get sRGB display encoding before the
    // 8-bit scale: this is the canvas-preview/metering/thumbnail exit and it follows
    // the same convention as the full-size exit (`decode_rgb_f32_with_origin`, the
    // "reset goes black" fix). Finished files (Image) are untouched. Encoding before
    // scaling puts the 8-bit quantisation on the right encoded-domain steps.
    let img = if !matches!(origin, PixelOrigin::Image) {
        let mut rgb16 = img.to_rgb16();
        for p in rgb16.pixels_mut() {
            for c in &mut p.0 {
                *c = enc16(*c);
            }
        }
        image::DynamicImage::ImageRgb16(rgb16)
    } else {
        img
    };
    Ok(fit_long_side(&img.to_rgb8(), max_side))
}

/// 16 位线性 → 16 位 sRGB 编码（`enc` 的 u16 版，256 级台阶对齐表法）。
///
/// 16-bit linear to 16-bit sRGB (the u16 twin of [`enc`], table-exact at 256 levels).
fn enc16(v: u16) -> u16 {
    let lin = v as f32 / 65535.0;
    let s = enc(lin, true);
    (s * 65535.0).round().clamp(0.0, 65535.0) as u16
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
    let (rgb, w, h, deep, _origin) = decode_rgb_f32_with_origin(path)?;
    Ok((rgb, w, h, deep))
}

/// [`decode_rgb_f32`] 的完整版：连「像素从哪条路来」一起报。
///
/// 相机配置（DCP）要按它决定白平衡乘不乘第二次：内置 DNG 解析不解白平衡，
/// 后端已经解过。见 [`PixelOrigin`]。
///
/// [`decode_rgb_f32`] with the route also reported.
///
/// The camera profile (DCP) reads it to decide whether to apply white balance a second time: the
/// built-in DNG parser does not apply it, the backend already did. See [`PixelOrigin`].
pub fn decode_rgb_f32_with_origin(
    path: impl AsRef<Path>,
) -> Result<(Vec<f32>, u32, u32, bool, PixelOrigin), MetricsError> {
    let (img, origin) = decode_with_origin(path)?;
    let (w, h) = (img.width(), img.height());
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    // 传感器路径（内置 DNG / RAW 后端）出口做 sRGB 显示编码：显影主链假设输入
    // 已在编码域（JPEG/PNG 天生如此），线性传感器数据直接进链，中间调被压暗
    // 整整一个 gamma——用户实测「复位后图片立马变黑」的根因（JPEG 序列没事、
    // RAW 一复位就黑）。编码放在唯一解码口，预览/测光/对齐/导出四条消费线
    // 一次对齐；`PixelOrigin::Image`（成品文件）保持逐位不动。
    // 与 LR/Camera Raw 的「默认基准显示变换」同构。
    //
    // Sensor-route exits (built-in DNG / RAW backend) get sRGB display encoding
    // here: the develop chain assumes encoded-domain input (JPEG/PNG already is),
    // and feeding linear sensor data darkens midtones by a whole gamma -- the root
    // cause of the user-reported "reset makes the image go black" (JPEG sequences
    // were fine, RAW went black). Encoding at the single decode exit aligns all four
    // consumers (preview / metrics / align / export) at once; `PixelOrigin::Image`
    // stays bit-identical. Isomorphic to LR/Camera Raw's default base display
    // transform.
    let encode = !matches!(origin, PixelOrigin::Image);
    let deep = match img.color() {
        image::ColorType::L16
        | image::ColorType::La16
        | image::ColorType::Rgb16
        | image::ColorType::Rgba16 => {
            for p in img.to_rgb16().pixels() {
                rgb.extend_from_slice(&[
                    enc(p.0[0] as f32 / 65535.0, encode),
                    enc(p.0[1] as f32 / 65535.0, encode),
                    enc(p.0[2] as f32 / 65535.0, encode),
                ]);
            }
            true
        }
        image::ColorType::Rgb32F | image::ColorType::Rgba32F => {
            for p in img.to_rgb32f().pixels() {
                rgb.extend_from_slice(&[
                    enc(p.0[0], encode),
                    enc(p.0[1], encode),
                    enc(p.0[2], encode),
                ]);
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
    Ok((rgb, w, h, deep, origin))
}

/// 传感器数据的 sRGB 显示编码（IEC 61966-2-1，含 0.04045 线性段）。
/// `encode=false` 时逐位透传——成品文件（PixelOrigin::Image）不许动。
///
/// sRGB display encoding for sensor data (IEC 61966-2-1, with the 0.04045 linear
/// segment). `encode=false` is a bit-transparent pass-through: finished files
/// (`PixelOrigin::Image`) must not be touched.
fn enc(v: f32, encode: bool) -> f32 {
    if !encode {
        return v;
    }
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
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

/// 草图解码的结果。
///
/// `half` 是**这条回包走的是哪条路**的事实，不是估算：`true` = 传感器半尺寸直解
/// （rawler 后端把 2×2 单元估计直接交出来），`false` = 退回「整幅解码再 2×2 均值减半」。
/// 首屏判据断言的就是它——比时间和比值都可靠，因为它不受机器快慢影响。
///
/// The result of a sketch decode.
///
/// `half` is the **fact** of which route produced these pixels, not an estimate: `true` means
/// the sensor was decoded at half size (the rawler backend hands out the 2x2 cell estimates),
/// `false` means the fallback "decode fully, then 2x2 mean". The first-screen fence asserts
/// this flag -- it is more reliable than both the wall clock and the ratio, because it does
/// not depend on machine speed.
pub struct SketchSource {
    /// sRGB f32 `[0,1]`，已缩到 `sketch_side`。
    ///
    /// sRGB f32 `[0,1]`, already scaled to `sketch_side`.
    pub rgb: Vec<f32>,
    /// 宽（像素，缩过之后）。
    ///
    /// Width after scaling.
    pub w: usize,
    /// 高（像素，缩过之后）。
    ///
    /// Height after scaling.
    pub h: usize,
    /// 全量预览的边长（画布逻辑尺寸靠它）。
    ///
    /// Side lengths of the full preview (the canvas logical size comes from it).
    pub full: (usize, usize),
    /// 是否走了半尺寸快路。
    ///
    /// Whether the half-size fast route was taken.
    pub half: bool,
}

/// 草图解码：按「目标长边 + 半尺寸」解一张小图，并报出全量预览的边长。
///
/// 返回（像素、宽、高、全量预览边长）。像素是 sRGB f32 `[0,1]`，与
/// [`decode_profile_preview`] 同一口径；尺寸是缩到 `sketch_side` 之后的结果。
///
/// # 两条路，一套色彩
///
/// - **RAW（rawler 后端）**：走 [`crate::raw::read_half`]，去马赛克停在 2×2 单元估计上，
///   输出就是传感器边长的一半，再缩到 `sketch_side`。省掉的是「展开成 24MP RGB 再缩
///   回来」这一来一回，不是色彩管线：容器解析、电平、白平衡、DCP 之后的那条调色链
///   全都没变。
/// - **其余（内置 TIFF/DNG、普通图片、带相机配置）**：退回整幅解码再 2×2 均值减半，
///   与改动前的草图逐像素相同。它们本来就不是 24MP RAW 那条首屏路径。
///
/// 全量预览边长按 [`fit_long_side`] 同一套舍入算出来，调用方拿它当画布上的逻辑尺寸：
/// 草图与全量的逻辑尺寸必须逐像素相等，否则松手换成全量时画面会跳一格。
///
/// # Sketch decode: small target plus half resolution, and the full preview's side lengths
///
/// Returns (pixels, width, height, full-preview side lengths), pixels in sRGB f32 `[0,1]`
/// like [`decode_profile_preview`]. For RAW on the rawler backend it goes through
/// [`crate::raw::read_half`]: the demosaic stops at the 2x2 cell estimates, so the output is
/// half the sensor's side and is then scaled to `sketch_side`. What disappears is the
/// expand-to-24MP-and-scale-back round trip, not the colour pipeline -- container parse,
/// levels, white balance and everything after DCP are untouched. Every other source (built-in
/// TIFF/DNG, plain pictures, a camera profile) falls back to a full decode plus a 2x2 mean
/// halving, pixel-identical to the previous sketch. Full-preview side lengths use the same
/// rounding as [`fit_long_side`]; the canvas needs them so that the sketch and the full
/// preview report the same logical size.
pub fn decode_preview_sketch(
    path: &Path,
    profile: &str,
    sketch_side: u32,
    full_side: u32,
) -> Result<SketchSource, MetricsError> {
    if profile.is_empty()
        && let Some((img, (fw, fh), orientation)) = crate::raw::read_half(path)
    {
        let img = crate::exif::oriented(DynamicImage::ImageRgb16(img), orientation);
        // 转正后宽高互换（5..8 是带 90° 的四种）。全量那边也是先转正再缩，
        // 这里必须用同一对边长去算逻辑尺寸。
        //
        // Orientations 5..8 rotate by 90 degrees, so the sides swap; the full path also
        // orients before scaling, so the logical size must come from the same pair.
        let (fw, fh) = if orientation >= 5 { (fh, fw) } else { (fw, fh) };
        let full = fit_dims(fw as usize, fh as usize, full_side);
        let small = fit_long_side(&img.to_rgb8(), sketch_side);
        let (w, h) = (small.width() as usize, small.height() as usize);
        // 与全量口同一约定：传感器数据在出口做 sRGB 显示编码（见
        // decode_rgb_f32_with_origin 的注释），草图与全量必须同域，否则
        // 开图那一帧会「草图亮、全量暗」跳一下。
        //
        // Same convention as the full-size exit: sensor data is sRGB-display-encoded
        // here (see decode_rgb_f32_with_origin); sketch and full preview must share the
        // domain or the first frame would jump bright-sketch to dark-full.
        let pixels = small
            .as_raw()
            .iter()
            .map(|&v| enc(v as f32 / 255.0, true))
            .collect();
        return Ok(SketchSource {
            rgb: pixels,
            w,
            h,
            full,
            half: true,
        });
    }
    let (pixels, fw, fh) = decode_profile_preview(path, profile, full_side)?;
    let (rgb, w, h) = halve_2x2(&pixels, fw, fh);
    Ok(SketchSource {
        rgb,
        w,
        h,
        full: (fw, fh),
        half: false,
    })
}

/// [`fit_long_side`] 的尺寸算法（不出像素），供草图算全量预览的逻辑尺寸。
///
/// [`fit_long_side`]'s size arithmetic without producing pixels, so the sketch can report the
/// full preview's logical size.
fn fit_dims(w: usize, h: usize, max_side: u32) -> (usize, usize) {
    let long = w.max(h).max(1);
    if long <= max_side as usize {
        return (w, h);
    }
    let s = max_side as f64 / long as f64;
    (
        ((w as f64 * s).round() as usize).max(1),
        ((h as f64 * s).round() as usize).max(1),
    )
}

/// 2×2 均值减半（sRGB f32）。宽高为奇数时最后一列/行并到边缘。
///
/// 2x2 mean halving for sRGB f32; the last column/row folds onto the edge when a side is
/// odd.
fn halve_2x2(src: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let hw = (w / 2).max(1);
    let hh = (h / 2).max(1);
    let mut out = vec![0.0f32; hw * hh * 3];
    out.par_chunks_mut(hw * 3).enumerate().for_each(|(y, row)| {
        let y0 = (y * 2).min(h - 1);
        let y1 = (y * 2 + 1).min(h - 1);
        for x in 0..hw {
            let x0 = (x * 2).min(w - 1);
            let x1 = (x * 2 + 1).min(w - 1);
            let a = (y0 * w + x0) * 3;
            let b = (y0 * w + x1) * 3;
            let c = (y1 * w + x0) * 3;
            let d = (y1 * w + x1) * 3;
            let o = x * 3;
            for k in 0..3 {
                row[o + k] = (src[a + k] + src[b + k] + src[c + k] + src[d + k]) * 0.25;
            }
        }
    });
    (out, hw, hh)
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
    let raw_stem = path.file_stem()?.to_string_lossy();
    // 文件名里掺一段**完整文件名**（含扩展名）的 FNV-32。清洗会把非 ASCII 与分隔符
    // 抹成 `_`：`风景.jpg` / `人像.jpg` / `照片.jpg` 都是 `__`（`..jpg` 是 `_.`）；而
    // 相机的 RAW+JPEG 同门对（`IMG_1234.CR2` / `IMG_1234.JPG`）共用同一个 stem。只要
    // 再撞上同秒 mtime 与精确相同的字节数（批量拷贝保留时间戳），两个不同帧就会共用
    // 一个缓存文件，胶片条里显示别人的图。摄取**完整文件名**把「同后缀不同名」与
    // 「同名不同后缀」两类都分开；只哈希 stem 挡不住后一类。旧缓存按新名找不到，自然
    // 淘汰成孤儿文件，不影响正确性。
    //
    // Mix an FNV-32 of the **full file name** (extension included) into the cache name.
    // Sanitising maps every non-ASCII character and separator to `_`, so three different
    // Chinese names all become `__` (and `..jpg` becomes `_.`), while a camera's RAW+JPEG
    // pair (`IMG_1234.CR2` / `IMG_1234.JPG`) shares one stem. With the same second-level
    // mtime and an identical byte count (batch copies preserve timestamps) two distinct
    // frames would share one cache file and the filmstrip would show someone else's picture.
    // Hashing the whole name separates both "same suffix, different stem" and "same stem,
    // different suffix"; hashing the stem alone misses the second class. Old caches stop
    // being found and are simply orphaned.
    let tag = fnv32(path.file_name()?.to_string_lossy().as_bytes());
    let stem: String = raw_stem
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
    let suffix = format!("-{tag:08x}-{mtime}-{}-{max_side}.png", meta.len());
    // 整条路径有平台上限（Windows 默认 260 含结尾 NUL）。stem 是文件全名清洗出来的，
    // 相机连拍号 + 长中文名轻易把它顶过上限：写入静默失败、读取也永远打不开，这张帧就
    // **每次都重算**——不报错，只是白干。所以按「目录 + 后缀还剩下多少字符」截断 stem。
    // 唯一性不受影响：`tag` 已经是完整文件名（含后缀）的 FNV-32，截断只砍展示用的前缀。
    // 目录自己就超预算时返回 `None`：缓存放不下，退回每帧重算（与修复前一致，不假装成功）。
    //
    // The whole path has a platform limit (260 including the trailing NUL on Windows by
    // default). The stem comes from the frame's full name, and a burst number plus a long
    // Chinese name pushes it over easily: the write fails silently and the read never opens,
    // so this frame is **recomputed every time** -- no error, just wasted work. Truncate the
    // stem to whatever is left after the directory and the suffix. Uniqueness is unaffected:
    // `tag` is already the FNV-32 of the complete file name (extension included), so the cut
    // only removes a display prefix. A directory that already blows the budget yields `None`:
    // no cache is possible, so every frame is recomputed (as before, without pretending).
    let budget = THUMB_PATH_BUDGET
        .checked_sub(dir.to_string_lossy().chars().count() + 1 + suffix.chars().count())?;
    let stem: String = if stem.chars().count() > budget {
        stem.chars().take(budget).collect()
    } else {
        stem
    };
    Some(dir.join(format!("{stem}{suffix}")))
}

/// 缩略图缓存整条路径的字符预算，比 Windows 的 260 少留 20 个字符的余量。
///
/// Character budget for a whole thumbnail-cache path: 20 below the Windows limit of 260,
/// leaving headroom for platform differences and short-name fallbacks.
const THUMB_PATH_BUDGET: usize = 240;

/// FNV-1a 32：只用来把两个「清洗后同名」的帧分开，不做安全用途。
///
/// FNV-1a 32: only there to tell two frames with the same sanitised stem apart; not a
/// security primitive.
fn fnv32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c_9dc5u32;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn read_thumb_cache(path: &Path, max_side: u32) -> Option<RgbaImage> {
    let file = thumb_cache_path(path, max_side)?;
    // 缓存文件是我们自己写的，但它在用户的盘上待过：可能被截断、被别的程序写过。
    // 读坏了就当缓存不存在（返回 `None`，走慢路径重算），绝不让它崩掉整张缩略图。
    //
    // 上界就是 `max_side` 本身，不是「比目标大几倍也认」：写入路径只经过
    // `resize_to_rgba`，它保证长边 ≤ `max_side`（原图比目标小就原样返回）。所以一张长边
    // 超过目标的缓存**不可能**是我们写下的，只能是被人替换过或从别的尺寸挪过来的——
    // 那就必须 miss，交给慢路径重算。以前这里给的是 4×，于是长边介于 `max_side` 与
    // 4×`max_side` 之间的替换品仍算命中，把一张不是这张帧的图喂给了胶片条。
    // 顺带这一条仍是解压炸弹的闸门：`Limits` 在解出像素之前就按宽高拒绝。
    //
    // 残留口径（不吹牛）：同尺寸范围内的替换挡不住——那只能靠路径里的 mtime + 字节数
    // 这两项元数据证据。尺寸契约只负责「比目标大的冒牌货」这一整类。
    //
    // The cap is `max_side` itself, not a multiple of it: the write path only goes through
    // `resize_to_rgba`, which guarantees a long side of at most `max_side` (a smaller source
    // is returned as-is). A cache bigger than the target therefore cannot be one we wrote --
    // it was replaced or moved in from another size -- and must miss so the slow path
    // recomputes. The old 4x cap let replacements between `max_side` and 4x hit, feeding the
    // filmstrip a picture of some other frame. It still doubles as the decompression-bomb
    // gate: `Limits` rejects on the declared width/height before any pixels are allocated.
    let cap = max_side.max(1);
    let img = crate::raw::guard::catch_decoder_panic("image", move || -> Option<DynamicImage> {
        let mut reader = image::ImageReader::open(file).ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(cap);
        limits.max_image_height = Some(cap);
        reader.limits(limits);
        reader.decode().ok()
    })
    .unwrap_or(None)?;
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
    fn sensor_decode_exits_display_encoded_and_images_pass_through() {
        // 「复位后变黑」的根因钉在这里：显影链吃编码域输入，传感器数据出口
        // 必须做 sRGB 显示编码；成品文件（PixelOrigin::Image）逐位不动。
        // Pinned root cause of "reset goes black": the develop chain takes
        // encoded-domain input, so sensor exits must display-encode; finished
        // files pass through bit-identically.
        // 18% 中灰线性值编码后应 ≈ 0.461（sRGB 定义，±0.002）。
        let mid = super::enc(0.18, true);
        assert!(
            (mid - 0.461).abs() < 0.002,
            "18% 灰编码后 {mid}，sRGB 定义值 0.461"
        );
        // 线性段两端与纯黑。
        assert!((super::enc(0.0025, true) - 0.0025 * 12.92).abs() < 1e-6);
        assert!(super::enc(0.0, true).abs() < 1e-9);
        assert!((super::enc(1.0, true) - 1.0).abs() < 1e-6);
        // 透传：encode=false 一位不动。
        assert!((super::enc(0.18, false) - 0.18).abs() < 1e-9);
    }

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

    /// 截断 JPEG 不许静默成功（缺陷排查 D1）：zune-jpeg 会把缺的行填灰返回成功，
    /// 半张灰图会被测光放行、被合成烤进成片。EOI 校验必须在 decode 之前拦住。
    ///
    /// A truncated JPEG must not silently succeed (defect D1): zune-jpeg fills the missing
    /// rows with gray and returns success, half a gray image passes metering and gets baked
    /// into stacks. The EOI check must intercept before decode.
    #[test]
    fn truncated_jpeg_is_an_error_not_a_gray_half_image() {
        // 噪声纹理压不出小体积，保证截断点落在熵数据而不是头部。
        //
        // Noise texture does not compress small, so the cut lands in entropy data, not headers.
        let img = image::RgbImage::from_fn(600, 600, |x, y| {
            let v = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(83_492_791)) as u8;
            image::Rgb([v, v.wrapping_add(17), v.wrapping_sub(9)])
        });
        let dir = std::env::temp_dir().join("aevum-trunc-jpeg-test");
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let full = dir.join("full.jpg");
        img.save_with_format(&full, image::ImageFormat::Jpeg)
            .expect("写 JPEG");
        let bytes = std::fs::read(&full).expect("读回");
        assert!(bytes.len() > 20_000, "夹具必须足够大：{} B", bytes.len());

        // 完整文件：解码成功。
        let ok = super::decode(&full);
        assert!(ok.is_ok(), "完整 JPEG 必须能解：{:?}", ok.err());

        // 截掉后 40%：必须报「不完整」，而不是半张灰图。
        let cut = dir.join("cut.jpg");
        std::fs::write(&cut, &bytes[..bytes.len() * 6 / 10]).expect("写截断 JPEG");
        let err = super::decode(&cut).expect_err("截断 JPEG 必须报错");
        assert!(
            err.to_string().contains("不完整"),
            "错误要说是文件不完整：{err}"
        );

        // EOI 之后的零填充（某些写卡器会补零）：不许误伤。
        let padded = dir.join("padded.jpg");
        let mut with_pad = bytes.clone();
        with_pad.extend_from_slice(&[0u8; 512]);
        std::fs::write(&padded, with_pad).expect("写补零 JPEG");
        assert!(
            super::decode(&padded).is_ok(),
            "EOI 后补零的完整 JPEG 必须照常解码"
        );

        // 规则本身的确定性单测（不依赖 image 的解码行为）。字节形状都是最小化的：
        // ① 最小完整流（SOI + SOS + EOI）→ true；② 完整流 + EOI 后厂商尾注 → true——
        // 正是真机 Panasonic 的形状，旧规则在这里误杀；③ SOS 后无 EOI（熵数据中途截断）
        // ③ SOS 后无 EOI（熵数据中途截断）→ false；
        // ④a EXIF 缩略图嵌在 APP1 里（自带 SOI/EOI）、主流 SOS 之后截断 → false
        // ——注意这个形状对裸扫版没有判别力（裸扫恰好也能判 false），留它只因
        // 它本身是合法的截断形状；
        // ④b 缩略图自带完整 SOI+SOS+EOI、主流整体缺失 → false——裸扫版会拿
        // 缩略图的 SOS 当「最后一个 SOS」、拿它的 EOI 当终点而误判 true，
        // 只有按段走能把它抓出来，这才是对裸扫回归真正有牙的形状；
        // ⑤ 连 SOI 都没有 → false。
        assert!(super::jpeg_stream_complete(&[
            0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9
        ]));
        assert!(super::jpeg_stream_complete(&[
            0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9, 0xAB, 0xCD, 0xEF
        ]));
        assert!(!super::jpeg_stream_complete(&[
            0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02, 0x12, 0x34
        ]));
        // APP1 len=6：载荷 = 缩略图的 SOI+EOI（4 字节）+ 2 字节填充。（④a）
        assert!(!super::jpeg_stream_complete(&[
            0xFF, 0xD8, // SOI
            0xFF, 0xE1, 0x00, 0x06, 0xFF, 0xD8, 0xFF, 0xD9, 0x00, 0x00, // APP1 含缩略图
            0xFF, 0xDA, 0x00, 0x02, // 主流 SOS
            0x56, 0x78, // 熵数据，截断处没有 EOI
        ]));
        // ④b：APP1 len=9，载荷 = 缩略图的完整 SOI+SOS(空)+EOI（7 字节），没有主流。
        assert!(!super::jpeg_stream_complete(&[
            0xFF, 0xD8, // SOI
            0xFF, 0xE1, 0x00, 0x09, 0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9, // APP1
        ]));
        // 同上但主流收了尾：缩略图 EOI 在前不算数，主流自己的 EOI 才是终点。
        assert!(super::jpeg_stream_complete(&[
            0xFF, 0xD8, //
            0xFF, 0xE1, 0x00, 0x06, 0xFF, 0xD8, 0xFF, 0xD9, 0x00, 0x00, //
            0xFF, 0xDA, 0x00, 0x02, 0x56, 0x78, 0xFF, 0xD9,
        ]));
        assert!(!super::jpeg_stream_complete(&[0x12, 0x34, 0x56]));

        std::fs::remove_file(&full).ok();
        std::fs::remove_file(&cut).ok();
        std::fs::remove_file(&padded).ok();
        std::fs::remove_dir(&dir).ok();
    }

    // ---- 缩略图磁盘缓存的守卫判据 ----
    //
    // 缓存在用户盘上待过：可能被截断、被别的程序覆盖、被换成一张 4 GB 的巨图。
    // 下面三条判据分别钉住「超限拒绝」「正常命中」「坏文件」三种形状；超限那条必须
    // 在把 `cap` 放宽到 `u32::MAX`（等于删掉上限）时变红，否则它只是在测「文件坏了」。
    //
    // ---- Teeth for the thumbnail disk-cache guard ----
    //
    // The cache has lived on the user's disk: it may be truncated, overwritten by another
    // program, or swapped for a 4 GB image. The tests below pin "normal hit", "broken file",
    // "oversize rejected" (`oversized_thumb_cache_is_a_miss` must go red when the cap is
    // widened to `u32::MAX`), "a replacement between the target and the old 4x cap is
    // rejected" (red when the cap goes back to 4x), and "a very long name still caches and
    // hits" (red when the path-budget truncation is removed).

    /// 每个用例一个独立目录：文件名带测试名与进程号，并行跑也不打架。
    ///
    /// One directory per case: the name carries the test name and pid, so parallel runs
    /// do not collide.
    fn thumb_case(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("aevum-thumb-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");
        // 源帧只用元数据（mtime + 字节数）算缓存路径，内容是什么无关紧要。
        //
        // The source frame only contributes metadata (mtime + byte count) to the cache path;
        // its contents do not matter.
        let src = root.join("frame.jpg");
        std::fs::write(&src, b"src-frame-placeholder").expect("写源帧占位");
        (root, src)
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut crc_src = Vec::with_capacity(4 + data.len());
        crc_src.extend_from_slice(kind);
        crc_src.extend_from_slice(data);
        let mut out = Vec::with_capacity(12 + data.len());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_src).to_be_bytes());
        out
    }

    /// 一张「结构合法、只有尺寸离谱」的 PNG：IHDR 里写多少就号称多少，IDAT 是合法的
    /// 空 zlib 流。CRC 都算对，好让解码器一定走到尺寸判定，而不是先被 CRC/格式挡掉。
    ///
    /// A PNG that is structurally valid and only absurd in size: IHDR claims whatever is
    /// asked, IDAT is a valid empty zlib stream. The CRCs are correct so the decoder really
    /// reaches the dimension check instead of tripping over a CRC or format error first.
    fn png_declaring(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 位 RGB，非隔行
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        out.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
        // zlib 头 + 空的 final 定长块 + 空流的 adler32。
        out.extend_from_slice(&png_chunk(
            b"IDAT",
            &[0x78, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
        ));
        out.extend_from_slice(&png_chunk(b"IEND", &[]));
        out
    }

    /// 清洗后同名的两帧不许共用一个缓存文件。
    ///
    /// 清洗把 `风景` / `人像` / `照片` 全抹成 `__`；只要字节数相同、mtime 又落在同一秒
    /// （批量拷贝保留时间戳就是这个形状），两帧就会共用一个缓存文件，胶片条里显示别人
    /// 的图。文件名里的 stem 哈希必须把它们分开，同时同一帧的路径要保持稳定。
    ///
    /// Two frames whose sanitised stems collide must not share one cache file.
    ///
    /// Sanitising collapses `风景` / `人像` / `照片` to `__`; with an identical byte count and
    /// an mtime in the same second (what batch copies that preserve timestamps produce) the
    /// two frames would share one cache file and the filmstrip would show someone else's
    /// picture. The stem hash in the name must keep them apart while a frame's own path stays
    /// stable.
    #[test]
    fn two_frames_with_the_same_sanitised_stem_do_not_share_a_cache_file() {
        let root = std::env::temp_dir().join(format!("aevum-thumb-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");
        let a = root.join("风景.jpg");
        let b = root.join("人像.jpg");
        let payload = b"identical-length-payload";
        std::fs::write(&a, payload).expect("写 a");
        std::fs::write(&b, payload).expect("写 b");
        let when = std::time::SystemTime::now();
        for p in [&a, &b] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .expect("打开源帧")
                .set_modified(when)
                .expect("拨 mtime");
        }

        let pa = super::thumb_cache_path(&a, 128).expect("a 的缓存路径");
        let pb = super::thumb_cache_path(&b, 128).expect("b 的缓存路径");
        assert_ne!(
            pa,
            pb,
            "清洗后同名的两帧共用了缓存文件，会串图：{}",
            pa.display()
        );
        assert_eq!(
            pa,
            super::thumb_cache_path(&a, 128).expect("a 的缓存路径"),
            "同一帧的缓存路径必须稳定"
        );

        // 相机的 RAW+JPEG 同门对：stem 相同、后缀不同，也要分开——只哈希 stem 挡不住
        // 这一类。用同一个 stem、同字节数、同一时刻 mtime 复现。
        //
        // A camera's RAW+JPEG pair shares the stem and differs only in the suffix, so the
        // hash must cover the extension: reproduce it with the same stem, byte count and
        // mtime.
        let raw = root.join("IMG_1234.CR2");
        let jpg = root.join("IMG_1234.JPG");
        std::fs::write(&raw, payload).expect("写 raw");
        std::fs::write(&jpg, payload).expect("写 jpg");
        for p in [&raw, &jpg] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .expect("打开同门对")
                .set_modified(when)
                .expect("拨 mtime");
        }
        assert_ne!(
            super::thumb_cache_path(&raw, 128).expect("raw 缓存路径"),
            super::thumb_cache_path(&jpg, 128).expect("jpg 缓存路径"),
            "同 stem 不同后缀的两帧共用了缓存文件，会串图"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn thumb_cache_hit_returns_the_same_pixels() {
        let (root, src) = thumb_case("hit");
        let max_side = 128u32;
        let img = RgbaImage::from_fn(64, 48, |x, y| {
            image::Rgba([(x * 3) as u8, (y * 5) as u8, 200, 255])
        });
        super::write_thumb_cache(&src, max_side, &img);

        let hit = super::read_thumb_cache(&src, max_side).expect("刚写下的缓存必须命中");
        assert_eq!((hit.width(), hit.height()), (64, 48), "尺寸不许被守卫改掉");
        assert_eq!(hit.as_raw(), img.as_raw(), "像素必须逐字节一致");

        // 路径里带 max_side：换了目标尺寸就该是另一个文件，也就不能命中。
        //
        // max_side is part of the path: a different target size is a different file and
        // must not hit.
        assert_ne!(
            super::thumb_cache_path(&src, max_side),
            super::thumb_cache_path(&src, max_side * 2)
        );
        assert!(
            super::read_thumb_cache(&src, max_side * 2).is_none(),
            "不同尺寸不得共用缓存"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn oversized_thumb_cache_is_a_miss() {
        let (root, src) = thumb_case("oversize");
        let max_side = 128u32;
        // 上界就是目标长边本身：写入路径经 `resize_to_rgba`，长边恒 ≤ `max_side`。
        //
        // The cap is the target long side itself: the write path runs through
        // `resize_to_rgba`, whose long side never exceeds `max_side`.
        let cap = max_side;
        let cache = super::thumb_cache_path(&src, max_side).expect("缓存路径");
        std::fs::create_dir_all(cache.parent().expect("缓存目录")).expect("建缓存目录");

        // 宽度只超出一格，但整张图是真实可解的：不设上限就能解出来。没有这一句，
        // 「读不出来」也可能只是因为夹具坏了，抓不到「把上限删掉」这次变异。
        //
        // Only slightly over the cap, but genuinely decodable: without the limit it decodes.
        // Without that check, "returns None" could just mean the fixture was broken, and
        // removing the cap would not be caught.
        let wide = image::RgbImage::from_pixel(cap + 8, 4, image::Rgb([200, 40, 40]));
        wide.save_with_format(&cache, image::ImageFormat::Png)
            .expect("写夹具");
        let healthy = image::ImageReader::open(&cache)
            .expect("打开夹具")
            .decode()
            .expect("夹具本身必须可解");
        assert_eq!(healthy.width(), cap + 8);

        assert!(
            super::read_thumb_cache(&src, max_side).is_none(),
            "长边超过 {cap} 的缓存必须当作没命中，交给慢路径重算"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 长边落在「目标」与「旧的 4× 上限」之间的缓存：那是别处的图挪过来的，必须 miss。
    ///
    /// 修复前 `cap` 是 `max_side * 4`，所以 2×`max_side` 的替换品会被当成命中，胶片条
    /// 显示的是别人的图。判据构造的是**完全可解**的同键文件，所以「把 cap 改回 4×」必红，
    /// 而「夹具坏了」不会伪装成绿。修复后写入路径只经 `resize_to_rgba`，长边恒 ≤
    /// `max_side`，「比目标大」等价于「不是我们写的」。
    ///
    /// A cache whose long side falls between the target and the old 4x cap is somebody
    /// else's picture and must miss. Before the fix `cap` was `max_side * 4`, so a 2x
    /// replacement hit and the filmstrip showed another frame. The fixture is fully
    /// decodable, so widening the cap back to 4x goes red while a broken fixture cannot
    /// masquerade as green.
    #[test]
    fn a_cache_between_the_target_and_the_old_fourfold_cap_is_a_miss() {
        let (root, src) = thumb_case("mid-size");
        let max_side = 128u32;
        let cache = super::thumb_cache_path(&src, max_side).expect("缓存路径");
        std::fs::create_dir_all(cache.parent().expect("缓存目录")).expect("建缓存目录");

        let swapped =
            image::RgbImage::from_pixel(max_side * 2, max_side, image::Rgb([12, 200, 60]));
        swapped
            .save_with_format(&cache, image::ImageFormat::Png)
            .expect("写替换品");
        assert_eq!(
            image::ImageReader::open(&cache)
                .expect("打开替换品")
                .decode()
                .expect("替换品本身必须可解")
                .width(),
            max_side * 2,
            "夹具必须是 2×目标、可解的 PNG，否则判据抓不到真变异"
        );

        assert!(
            super::read_thumb_cache(&src, max_side).is_none(),
            "长边 2×目标的缓存是替换品，必须当没命中"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 超长文件名也要真的写出缓存并命中：路径长到接近平台上限时，缓存不许静默失效。
    ///
    /// 修复前缓存名是「240 字符 stem + 后缀」，前面再挂 `目录/.aevum/thumbs`，Windows 上
    /// 直接越过 260：写入静默不落盘、读取永远 miss，这一帧**每次都重算**。
    ///
    /// **主断言是平台路径上限**（Windows `MAX_PATH` = 260，判据里独立写死），不是
    /// 「OS 拒绝写入」也不是被测的预算常量：这台开发机开着长路径支持，297 字符的路径
    /// 照样写成功——第一版判据在这里变异全绿；第二版拿预算常量当基准，改大常量后又
    /// 自洽地绿。两版都没牙，第三版才咬住。「真实落盘 + 第二次读出同样像素」留着，
    /// 是为了在有长路径限制的机器上抓真实后果。
    ///
    /// The **primary assertion is the platform limit** (Windows `MAX_PATH` = 260, written
    /// independently in the test), not "the OS refuses the write" and not the budget constant
    /// under test: this dev box has long paths enabled and happily wrote a 297-character path
    /// (first version green under mutation), and using the budget constant as the baseline
    /// made the second version green as well once the mutation enlarged it. Only the third
    /// version bites. "Really on disk, second read returns the same pixels" stays so a machine
    /// with the classic 260 limit catches the real consequence too.
    #[test]
    fn a_very_long_frame_name_still_gets_a_cache_file_that_hits() {
        let root = std::env::temp_dir().join(format!("aevum-thumb-long-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建临时目录");

        // 名字长度按实际目录长度反推，让**源帧**路径落在 245 字符附近（还能建），而
        // 不截断的缓存路径「目录/.aevum/thumbs + 同长 stem + 后缀」必然越过预算。
        //
        // Derive the name length from the real directory length so the **source** path lands
        // near 245 characters (still creatable) while an untruncated cache path, with
        // `.aevum/thumbs` and the suffix, goes over the budget.
        let root_len = root.to_string_lossy().chars().count() + 1 + ".jpg".len();
        let name_len = 245usize.saturating_sub(root_len).max(60);
        let src = root.join(format!("{}.jpg", "n".repeat(name_len)));
        std::fs::write(&src, b"src-frame-placeholder").expect("写源帧占位");

        let max_side = 128u32;
        let img = RgbaImage::from_pixel(64, 48, image::Rgba([9, 99, 199, 255]));
        super::write_thumb_cache(&src, max_side, &img);

        let cache = super::thumb_cache_path(&src, max_side).expect("缓存路径");
        let cache_chars = cache.to_string_lossy().chars().count();
        // 基准取**平台事实**（Windows `MAX_PATH` = 260），不取 `THUMB_PATH_BUDGET`：
        // 拿被测常量当基准的话，变异把它改大，断言就跟着变松、判据自洽地绿——这条判据
        // 的第一版正是这么写的，变异实测真绿。基准必须来自被测物之外。
        //
        // The baseline is the **platform fact** (Windows `MAX_PATH` = 260), not
        // `THUMB_PATH_BUDGET`: taking the constant under test as its own baseline makes the
        // assertion loosen with the mutation and pass by construction -- exactly what the
        // first version of this test did, and the mutation proved it green. Baselines must
        // come from outside the thing under test.
        const PLATFORM_PATH_LIMIT: usize = 260;
        assert!(
            cache_chars < PLATFORM_PATH_LIMIT,
            "缓存路径 {cache_chars} 字符，撞上平台上限 {PLATFORM_PATH_LIMIT}：长名帧每次都会重算（{}）",
            cache.display()
        );
        assert!(
            cache.exists(),
            "超长名的缓存必须真的落盘：{}",
            cache.display()
        );
        let hit = super::read_thumb_cache(&src, max_side).expect("第二次必须命中");
        assert_eq!((hit.width(), hit.height()), (64, 48), "尺寸不许被守卫改掉");
        assert_eq!(hit.as_raw(), img.as_raw(), "像素必须逐字节一致");

        // 截断只砍展示前缀：两个前段相同、只有尾部不同的长名不许共用缓存文件
        // （唯一性靠完整文件名的 FNV-32，截断不会把两帧并成一个）。名字总长与上面
        // 的源帧一致，保证两条路径都还在可创建的范围内。
        //
        // Truncation only drops a display prefix: two long names sharing their leading
        // characters but differing at the tail must not share a cache file -- uniqueness
        // comes from the FNV-32 of the whole name, not from the truncated prefix. Both names
        // are the same total length as the source above, so both stay creatable.
        let prefix = "n".repeat(name_len.saturating_sub(8));
        let a = root.join(format!("{prefix}aaaa.jpg"));
        let b = root.join(format!("{prefix}bbbb.jpg"));
        std::fs::write(&a, b"same-bytes").expect("写 a");
        std::fs::write(&b, b"same-bytes").expect("写 b");
        let when = std::time::SystemTime::now();
        for p in [&a, &b] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .expect("打开源帧")
                .set_modified(when)
                .expect("拨 mtime");
        }
        assert_ne!(
            super::thumb_cache_path(&a, max_side).expect("a 的缓存路径"),
            super::thumb_cache_path(&b, max_side).expect("b 的缓存路径"),
            "截断后同前缀的两个长名共用了缓存文件，会串图"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn thumb_cache_declaring_gigantic_dimensions_is_a_miss() {
        let (root, src) = thumb_case("huge-dims");
        let cache = super::thumb_cache_path(&src, 128).expect("缓存路径");
        std::fs::create_dir_all(cache.parent().expect("缓存目录")).expect("建缓存目录");
        // 65535² 的 8 位 RGB 解出来约 12.9 GB——正是注释里「128px 缓存解出 4 GB」
        // 那种形状。路径上必须是一声不响的 None，且不能真去分配。
        //
        // 65535² in 8-bit RGB is about 12.9 GB — exactly the "a 128px cache expanding to
        // 4 GB" shape from the comment. It must be a quiet `None`, with no real allocation.
        std::fs::write(&cache, png_declaring(65535, 65535)).expect("写巨图头");
        assert!(
            super::read_thumb_cache(&src, 128).is_none(),
            "声称 65535² 的缓存必须当作没命中"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn garbage_or_truncated_thumb_cache_is_a_miss() {
        let (root, src) = thumb_case("broken");
        let max_side = 128u32;
        let cache = super::thumb_cache_path(&src, max_side).expect("缓存路径");
        std::fs::create_dir_all(cache.parent().expect("缓存目录")).expect("建缓存目录");

        std::fs::write(&cache, b"\x89PNG\r\n\x1a\n not really a png").expect("写垃圾");
        assert!(
            super::read_thumb_cache(&src, max_side).is_none(),
            "非图片垃圾必须当作没命中"
        );

        // 截断：先写一张合法缓存，再从中间砍断。半张图不许当缓存用。
        //
        // Truncation: write a valid cache, then cut it in half. Half an image is not a cache.
        let img = RgbaImage::from_pixel(64, 48, image::Rgba([10, 20, 30, 255]));
        super::write_thumb_cache(&src, max_side, &img);
        let bytes = std::fs::read(&cache).expect("读回缓存");
        assert!(bytes.len() > 40, "缓存夹具太小：{} B", bytes.len());
        std::fs::write(&cache, &bytes[..bytes.len() / 2]).expect("写截断缓存");
        assert!(
            super::read_thumb_cache(&src, max_side).is_none(),
            "截断的缓存必须当作没命中"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
