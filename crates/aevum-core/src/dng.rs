//! DNG 读取：TIFF 头 → IFD0 标记 → CFA 马赛克 → 16bit RGB。
//!
//! 只解主图：单通道 CFA / 线性原始数据、条带存储、压缩方式 1（不压缩）与
//! 8（Adobe Deflate）。瓦片（Tiles）、无损 JPEG、PackBits、浮点采样、多通道、
//! 需要色彩矩阵的 C/M/Y 图案都直接拒绝——猜出「看起来正常」的错像素比报错糟糕得多。
//!
//! 与 `exif::` 各写一份 TIFF 解析是有意的：那边是「读不到就给中性值」的 JPEG
//! 容错语义，这边要把坏文件明确拒掉，两套错误语义不该挤在同一个解析层里。
//!
//! DNG reading: TIFF header to IFD0 tags to CFA mosaic to 16bit RGB.
//!
//! Only the main image is decoded: single-channel CFA or linear raw data, strip storage,
//! compression 1 (uncompressed) and 8 (Adobe Deflate).
//! Tiles, lossless JPEG, PackBits, floating point samples, multiple channels, and C/M/Y patterns
//! that need a color matrix are rejected outright: guessing wrong pixels that look plausible is
//! far worse than an error.
//!
//! Writing a separate TIFF parser from `exif::` is deliberate.
//! That side follows a JPEG tolerance convention where an unreadable value becomes a neutral one,
//! while this side must reject a bad file outright, and two error conventions should not be
//! squeezed into one parsing layer.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use thiserror::Error;

/// 文件大小上限。再大的 DNG 当坏文件处理，别让一次 `read_to_end` 吃掉整机内存。
const MAX_FILE_BYTES: u64 = 512 << 20;
/// 嗅探只读头部这么多：绝大多数写入器把 IFD0 紧跟在 8 字节头后面。
const SNIFF_BYTES: u64 = 1 << 20;
/// 解码后像素数上限。24MP 已覆盖常见机身，再大基本是伪造宽高。
const MAX_PIXELS: u64 = 24_000_000;
/// 条带数量上限，防伪造的 RowsPerStrip=1 把循环拉长。
const MAX_STRIPS: usize = 100_000;
/// 单个标记值（条带数组、CFA 图案、电平表）的字节上限。
const MAX_VALUE_BYTES: usize = 4 << 20;

const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
/// TIFF 正统的 BitsPerSample。DNG 主图用 371，两边都认，优先 371。
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_MAKE: u16 = 271;
const TAG_MODEL: u16 = 272;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_PLANAR_CONFIG: u16 = 284;
const TAG_PREDICTOR: u16 = 317;
/// 子 IFD 偏移表。iPhone 这类写入器把 CFA 主图放这里，IFD0 只留一张预览。
///
/// Sub-IFD offset table. Writers like iPhone's put the CFA main image here and leave only a
/// preview in IFD0.
const TAG_SUB_IFDS: u16 = 330;
/// 位 0 = 缩减分辨率（即预览/缩略图）。选主图就靠这一位。
///
/// Bit 0 = reduced resolution, that is, a preview or thumbnail. Picking the main image uses it.
const TAG_NEW_SUBFILE_TYPE: u16 = 254;
const TAG_CFA_REPEAT_DIM: u16 = 33421;
const TAG_CFA_PATTERN: u16 = 33422;
const TAG_BITS_PER_SAMPLE_DNG: u16 = 371;
// 下面四个编号按 Adobe DNG 规范的标记表来：50706 DNGVersion、50708 UniqueCameraModel、
// 50714 BlackLevel、50717 WhiteLevel。别和邻居混了——50721 是 ColorMatrix1、50727 是
// AnalogBalance、50728 是 AsShotNeutral，那三个这版不解析，必须当未知标记跳过。
const TAG_DNG_VERSION: u16 = 50706;
const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_BLACK_LEVEL: u16 = 50714;
const TAG_WHITE_LEVEL: u16 = 50717;
/// 机身记录的白平衡（相机 RGB 下的中性色），DCP 流程要用；解码本身不套它。
const TAG_AS_SHOT_NEUTRAL: u16 = 50728;

const PHOTO_CFA: u32 = 32803;
const PHOTO_LINEAR_RAW: u32 = 34892;

const COMP_NONE: u16 = 1;
const COMP_ADOBE_DEFLATE: u16 = 8;

/// image 0.25 把 `Rgb16Image` 别名留在 crate 内（`pub(crate)`），这里给出等价类型。
///
/// image 0.25 keeps the `Rgb16Image` alias crate-internal (`pub(crate)`), so an equivalent type
/// is defined here.
pub type Rgb16Image = image::ImageBuffer<image::Rgb<u16>, Vec<u16>>;

#[derive(Debug, Error)]
pub enum DngError {
    #[error("打开 DNG 失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("不是 TIFF/DNG：字节序标记或 magic 42 不对")]
    NotTiff,
    #[error("文件被截断，标记或数据指向了文件之外")]
    Truncated,
    #[error("文件超过 {0} 字节上限")]
    FileTooLarge(u64),
    #[error("缺少必要标记 {0}")]
    MissingTag(u16),
    #[error("标记 {0} 的类型或数量不合法")]
    BadTag(u16),
    #[error("不支持的压缩方式 {0}")]
    UnsupportedCompression(u16),
    #[error("不支持的影像解释 {0}")]
    UnsupportedPhotometric(u16),
    #[error("不支持的位深 {0}（只接 8 或 16）")]
    UnsupportedBits(u16),
    #[error("不支持的平面配置 {0}")]
    UnsupportedPlanar(u16),
    #[error("每像素 {0} 个采样不支持（只认单通道原始数据）")]
    UnsupportedSamples(u16),
    #[error("CFA 图案不是可用的 2×2 RGBG 排列")]
    UnsupportedCfa,
    #[error("尺寸 {width}×{height} 超出上限")]
    TooLarge { width: u32, height: u32 },
    #[error("条带越界：偏移 {offset} 长度 {length} 落在文件外")]
    StripOutOfRange { offset: u64, length: u64 },
    #[error("条带数量 {found} 与宽高推出的 {want} 不一致")]
    StripCountMismatch { found: usize, want: usize },
    #[error("条带字节数 {found} 与解码所需的 {want} 不一致")]
    StripSizeMismatch { found: usize, want: usize },
    #[error("黑场 {black} 不低于白场 {white}，电平非法")]
    BadLevels { black: u32, white: u32 },
    #[error("deflate 数据非法：{0}")]
    Deflate(&'static str),
}

impl DngError {
    /// 换个解码器还可能成功吗——即「内置解析器的能力边界」而非「文件坏了」。
    ///
    /// 这条判据决定要不要回落 `raw::backend`：内置只解单通道 CFA/LinearRaw 的
    /// 无压缩或 deflate 条带，**三通道 LinearRaw 是真实存在的常见写法**（iPhone
    /// 导出的 DNG 就是），拒绝它是能力边界，不是文件损坏。字节层面坏掉的
    /// （TIFF 头不对、条带越界、deflate 流非法）换谁来解都一样，回落只是白读一遍。
    ///
    /// 与 `raw::RawError::soft_fallback` 同一套语义，别让两边对「软错」说两句话。
    ///
    /// Whether another decoder might still succeed, that is, whether this is a limit of the
    /// built-in parser rather than a broken file.
    ///
    /// This decides whether to fall back to `raw::backend`: the built-in parser only handles
    /// single-channel CFA or LinearRaw in uncompressed or deflate strips, while **three-channel
    /// LinearRaw is a real and common layout** (DNGs exported by iPhone use it). Rejecting it is a
    /// capability limit, not corruption. Byte-level damage (bad TIFF header, strip out of range,
    /// invalid deflate stream) fails the same way everywhere, so falling back would only re-read
    /// the file for nothing.
    ///
    /// Same semantics as `raw::RawError::soft_fallback`; the two sides must not disagree about
    /// what counts as a soft error.
    pub fn soft_fallback(&self) -> bool {
        matches!(
            self,
            Self::MissingTag(_)
                | Self::UnsupportedCompression(_)
                | Self::UnsupportedPhotometric(_)
                | Self::UnsupportedBits(_)
                | Self::UnsupportedPlanar(_)
                | Self::UnsupportedSamples(_)
                | Self::UnsupportedCfa
                | Self::TooLarge { .. }
        )
    }
}

/// 主图元数据，只放解码真要用的字段。
///
/// Main image metadata, holding only the fields decoding actually needs.
#[derive(Clone, Debug, PartialEq)]
pub struct DngInfo {
    pub width: u32,
    pub height: u32,
    /// 每采样位深，只可能是 8 或 16。
    ///
    /// Bit depth per sample; only 8 or 16 is possible.
    pub bits: u16,
    /// 每像素采样数。**1 = 单通道马赛克（本模块解得动）**；3 = 线性 RGB，
    /// 元数据报得出来但像素要交给 `raw::backend`。
    ///
    /// Samples per pixel. **1 = single-channel mosaic (this module can decode it)**; 3 = linear
    /// RGB, whose metadata is reportable but whose pixels go to `raw::backend`.
    pub samples: u16,
    pub make: Option<String>,
    pub model: Option<String>,
    /// 黑场电平（原始 ADC 值）：标记里多个值取平均，缺标记时为 0。
    ///
    /// Black level as a raw ADC value: multiple tag values are averaged, and a missing tag gives 0.
    pub black: u16,
    /// 白场电平（饱和值）：标记里多个值取最大，缺标记时按位深满量程。
    ///
    /// White level as the saturation value: the maximum of multiple tag values, with a missing tag
    /// falling back to the full scale of the bit depth.
    pub white: u16,
    /// 2×2 CFA 图案的通道索引（0=R、1=G、2=B，按左上、右上、左下、右下的存储顺序）。
    /// 线性原始数据没有图案时为 `None`。
    ///
    /// Why expose it: the panel shows the user their own sensor's arrangement (RGGB vs
    /// BGGR), which is the one fact that tells apart "highlights turned magenta because
    /// the pattern is misread" from a real colour cast.
    ///
    /// Channel indices of the 2×2 CFA pattern (0=R, 1=G, 2=B, in stored order: top-left,
    /// top-right, bottom-left, bottom-right). `None` for linear raw data.
    pub cfa: Option<[u8; 4]>,
}

impl DngInfo {
    /// 机型一行。Make 常是 Model 的前缀，重复就只留一次。
    ///
    /// One line for the camera model.
    /// Make is often a prefix of Model, so a duplicate is kept only once.
    pub fn camera_line(&self) -> Option<String> {
        match (self.make.as_deref(), self.model.as_deref()) {
            (Some(make), Some(model)) if model.starts_with(make) => self.model.clone(),
            (Some(make), Some(model)) => Some(format!("{make} {model}")),
            (Some(make), None) => Some(make.to_owned()),
            (None, Some(model)) => Some(model.to_owned()),
            (None, None) => None,
        }
    }
}

/// 结构上算不算 DNG：先看扩展名，否则嗅头部——要求 TIFF magic 且 IFD0 带
/// `DNGVersion`，这样普通 TIFF/JPEG 里的 TIFF 不会被误认成 DNG。
///
/// 头部嗅探要读 1 MiB，只适合扫描目录时认领文件用。逐帧解码走
/// [`is_named_dng`]，别让每帧都白读一兆。
///
/// Whether a file is structurally a DNG: the extension is checked first, otherwise the header is
/// sniffed, requiring the TIFF magic and a `DNGVersion` in IFD0, so that a TIFF inside an ordinary
/// TIFF or JPEG is not mistaken for a DNG.
///
/// Header sniffing reads 1 MiB and suits only claiming files while scanning a directory.
/// Per-frame decoding goes through [`is_named_dng`], so that every frame does not read a megabyte
/// for nothing.
pub fn is_dng(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    is_named_dng(path) || has_dng_version(path)
}

/// 只看扩展名的轻量版本：改了名的 DNG 在这里不认领，空 `.dng` 照样认领、交给 `read`
/// 报错。逐帧解码走这条；要按内容认领文件的调用方才用 [`is_dng`]。
///
/// Lightweight version that looks only at the extension: a renamed DNG is not claimed here, while
/// an empty `.dng` is still claimed and left to `read` to report the error.
/// Per-frame decoding uses this one; callers that must claim files by content use [`is_dng`].
pub fn is_named_dng(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dng"))
}

fn has_dng_version(path: &Path) -> bool {
    let Ok(data) = load(path, SNIFF_BYTES) else {
        return false;
    };
    let Ok(tiff) = Tiff::parse(&data) else {
        return false;
    };
    let Ok(ifd) = tiff.ifd0() else {
        return false;
    };
    // 只认 BYTE[4] 的规范写法；类型不对留给 `read` 报具体错误。
    matches!(
        tiff.entry(ifd, TAG_DNG_VERSION),
        Ok(Some(e)) if e.typ == 1 && e.count == 4
    )
}

/// 只读元数据，不解码像素。
///
/// Read metadata only, without decoding pixels.
pub fn info(path: impl AsRef<Path>) -> Result<DngInfo, DngError> {
    let data = load(path.as_ref(), MAX_FILE_BYTES)?;
    info_bytes(&data)
}

pub fn info_bytes(data: &[u8]) -> Result<DngInfo, DngError> {
    let t = Tiff::parse(data)?;
    // 报的是**主图**的元数据，不是 IFD0 那张预览的：界面上「4032×3024 · 16 位」和
    // 画布上真正显示的图必须是同一张。
    let m = meta(&t, t.main_ifd()?)?;
    Ok(DngInfo {
        width: m.width,
        height: m.height,
        bits: m.bits,
        samples: m.spp,
        make: m.make,
        model: m.model,
        black: m.black as u16,
        white: m.white as u16,
        cfa: m.cfa,
    })
}

/// 只读 `AsShotNeutral`，给 DCP 的白平衡用。缺标记返回 `None`：
/// 没有白平衡的相机 RGB 是绿的，调用方必须自己决定是报错还是退。
///
/// Read only `AsShotNeutral`, for the DCP white balance.
/// A missing tag returns `None`: camera RGB without white balance is green, and the caller must
/// decide whether to fail or fall back.
pub fn as_shot_neutral(path: impl AsRef<Path>) -> Result<Option<[f64; 3]>, DngError> {
    let data = load(path.as_ref(), MAX_FILE_BYTES)?;
    let t = Tiff::parse(&data)?;
    let ifd = t.ifd0()?;
    let Some(entry) = t.entry(ifd, TAG_AS_SHOT_NEUTRAL)? else {
        return Ok(None);
    };
    let vals = t.levels(&entry, TAG_AS_SHOT_NEUTRAL)?;
    if vals.len() != 3 || vals.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
        return Ok(None);
    }
    Ok(Some([vals[0], vals[1], vals[2]]))
}

/// 解成 16bit RGB。低于 16 位的源已按白场拉到满量程。
///
/// Decode into 16bit RGB.
/// Sources below 16 bits are already stretched to full scale by the white level.
pub fn read(path: impl AsRef<Path>) -> Result<Rgb16Image, DngError> {
    let data = load(path.as_ref(), MAX_FILE_BYTES)?;
    read_bytes(&data)
}

/// 字节入口：测试与已经握在手里的一段 DNG（例如从别的容器里抠出来的）走这条。
///
/// **解像素需要单通道马赛克**。三通道 LinearRaw（iPhone 导出的 DNG 就是）在这里被拒，
/// 报的是 [`DngError::UnsupportedSamples`]——它属于 [`DngError::soft_fallback`]，
/// 调用方该把这份文件交给 `raw::backend` 再试一次，而不是告诉用户「文件不支持」。
/// 元数据（宽高、位深、机型、电平）那条路不受影响，见 [`info`]。
///
/// Byte entry point: tests and a DNG already in hand, for example one extracted from another
/// container, take this path.
///
/// **Decoding pixels needs a single-channel mosaic.** Three-channel LinearRaw (which is what an
/// iPhone-exported DNG uses) is rejected here with [`DngError::UnsupportedSamples`], which belongs
/// to [`DngError::soft_fallback`]: the caller should hand the file to `raw::backend` for another
/// attempt rather than telling the user the file is unsupported. The metadata route is unaffected,
/// see [`info`].
pub fn read_bytes(data: &[u8]) -> Result<Rgb16Image, DngError> {
    let tiff = Tiff::parse(data)?;
    let ifd = tiff.main_ifd()?;
    let m = meta(&tiff, ifd)?;
    if m.spp != 1 {
        return Err(DngError::UnsupportedSamples(m.spp));
    }
    let l = layout(&tiff, ifd, m.height)?;
    let mut mosaic = decode_mosaic(&tiff, &m, &l)?;
    scale_levels(&mut mosaic, m.black, m.white);
    let rgb = demosaic(&mosaic, m.width as usize, m.height as usize, m.cfa);
    Rgb16Image::from_raw(m.width, m.height, rgb).ok_or(DngError::TooLarge {
        width: m.width,
        height: m.height,
    })
}

fn load(path: &Path, limit: u64) -> Result<Vec<u8>, DngError> {
    let file = File::open(path)?;
    let mut buf = Vec::new();
    // 多读 1 字节就能区分「正好到上限」与「超限」，不用先 metadata 再 seek。
    file.take(limit + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        return Err(DngError::FileTooLarge(limit));
    }
    Ok(buf)
}

// ------------------------------------------------------------------ TIFF 解析

#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

impl Order {
    fn u16(self, b: &[u8]) -> u16 {
        match self {
            Self::Little => u16::from_le_bytes([b[0], b[1]]),
            Self::Big => u16::from_be_bytes([b[0], b[1]]),
        }
    }

    fn u32(self, b: &[u8]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            Self::Big => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        }
    }
}

/// 只读 IFD0 的小 TIFF 视图：所有取值都要过 `range`，越界只会变成错误。
struct Tiff<'a> {
    d: &'a [u8],
    order: Order,
}

struct Entry<'a> {
    typ: u16,
    count: u32,
    data: &'a [u8],
}

fn unit_bytes(typ: u16) -> Option<usize> {
    match typ {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

fn out_of_range(start: u64, len: u64) -> DngError {
    DngError::StripOutOfRange {
        offset: start,
        length: len,
    }
}

/// 定长单元迭代；尾巴凑不齐一个单元就是坏数据，不静默截断。
fn walk<T, F>(data: &[u8], size: usize, f: F) -> Result<Vec<T>, DngError>
where
    F: FnMut(&[u8]) -> Result<T, DngError>,
{
    // `size` 只来自内部常量（2/4/8），这里仍按 0 收口：`size` 为 0 时
    // `is_multiple_of` 会直接 panic，所以先短路掉。
    if size == 0 || !data.len().is_multiple_of(size) {
        return Err(DngError::Truncated);
    }
    data.chunks(size).map(f).collect()
}

impl<'a> Tiff<'a> {
    fn parse(d: &'a [u8]) -> Result<Self, DngError> {
        let head = d.get(0..8).ok_or(DngError::NotTiff)?;
        let order = match &head[0..2] {
            b"II" => Order::Little,
            b"MM" => Order::Big,
            _ => return Err(DngError::NotTiff),
        };
        if order.u16(&head[2..4]) != 42 {
            return Err(DngError::NotTiff);
        }
        Ok(Self { d, order })
    }

    fn ifd0(&self) -> Result<usize, DngError> {
        // 0~7 是头部，IFD 指回头部是坏文件；BIGTIFF（magic 43）不在这条路上。
        let off = self.u32(4)? as usize;
        if off < 8 {
            return Err(DngError::Truncated);
        }
        Ok(off)
    }

    /// 主图所在的 IFD。
    ///
    /// DNG 的主图是「`NewSubfileType` 位 0 为 0」的那一张，**不一定是 IFD0**：iPhone
    /// 这类写入器把 852×640 的 YCbCr 预览放在 IFD0，把 4032×3024 的 CFA 主图放在
    /// `SubIFDs` 里。只看 IFD0 的实现，轻则去解一张预览、重则因为「photometric=6
    /// （YCbCr）」把整个文件判成不支持——真实样本上两种情况都出现过。
    ///
    /// 候选是 IFD0 加上 `SubIFDs` 的每一项；跳过标了「缩减分辨率」的，剩下取面积
    /// 最大的（DNG 允许多张同分辨率主图，大的那张是用户要的）。全被跳过或都没有
    /// 标记时退回 IFD0，与老文件的语义一致。
    ///
    /// The IFD holding the main image.
    ///
    /// A DNG main image is the one with `NewSubfileType` bit 0 clear, which **is not necessarily
    /// IFD0**: writers like iPhone's put an 852×640 YCbCr preview in IFD0 and the 4032×3024 CFA
    /// main image in `SubIFDs`. An implementation that only reads IFD0 either decodes a preview or
    /// rejects the whole file because of "photometric=6 (YCbCr)"; both were observed on a real
    /// sample.
    ///
    /// The candidates are IFD0 plus every entry of `SubIFDs`; those marked reduced-resolution are
    /// skipped and the largest remaining area wins (DNG allows several full-resolution images and
    /// the user wants the big one). If everything is skipped or nothing carries the tag, IFD0 is
    /// returned, matching the older convention.
    fn main_ifd(&self) -> Result<usize, DngError> {
        let ifd0 = self.ifd0()?;
        let mut candidates = vec![ifd0];
        if let Some(e) = self.entry(ifd0, TAG_SUB_IFDS)? {
            for off in self.scalars(&e, TAG_SUB_IFDS)? {
                let off = off as usize;
                // 越界的子 IFD 不猜：跳过，由主图判据决定还剩谁。
                if off >= 8 && off < self.d.len() {
                    candidates.push(off);
                }
            }
        }

        let mut best: Option<(u64, usize)> = None;
        for off in candidates {
            // 缺标记按 0 处理：老文件不写 NewSubfileType，那就是主图。
            let reduced = match self.entry(off, TAG_NEW_SUBFILE_TYPE)? {
                Some(e) => self.scalar(&e, TAG_NEW_SUBFILE_TYPE)? & 1 != 0,
                None => false,
            };
            if reduced {
                continue;
            }
            let area = self.pixels_of(off)?;
            if best.is_none_or(|(a, _)| area > a) {
                best = Some((area, off));
            }
        }
        Ok(best.map_or(ifd0, |(_, off)| off))
    }

    /// 某个 IFD 声明的像素数；宽高缺失或是 0 就返回 0（候选排序用，不报错）。
    ///
    /// Pixels declared by one IFD; 0 when width or height is missing or zero. Used for ranking
    /// candidates, so it never fails.
    fn pixels_of(&self, ifd: usize) -> Result<u64, DngError> {
        let width = match self.entry(ifd, TAG_IMAGE_WIDTH)? {
            Some(e) => self.scalar(&e, TAG_IMAGE_WIDTH)?,
            None => return Ok(0),
        };
        let height = match self.entry(ifd, TAG_IMAGE_LENGTH)? {
            Some(e) => self.scalar(&e, TAG_IMAGE_LENGTH)?,
            None => return Ok(0),
        };
        Ok(u64::from(width) * u64::from(height))
    }

    fn u16(&self, at: usize) -> Result<u16, DngError> {
        let b = self.d.get(at..at + 2).ok_or(DngError::Truncated)?;
        Ok(self.order.u16(b))
    }

    fn u32(&self, at: usize) -> Result<u32, DngError> {
        let b = self.d.get(at..at + 4).ok_or(DngError::Truncated)?;
        Ok(self.order.u32(b))
    }

    /// 唯一的取字节口子：全 checked 算术，坏偏移/长度只会得到 `StripOutOfRange`。
    fn range(&self, start: u64, len: u64) -> Result<&'a [u8], DngError> {
        let bad = || out_of_range(start, len);
        let start = usize::try_from(start).map_err(|_| bad())?;
        let len = usize::try_from(len).map_err(|_| bad())?;
        let end = start.checked_add(len).ok_or_else(bad)?;
        self.d.get(start..end).ok_or_else(bad)
    }

    /// IFD0 线性扫描找标记：只有几十项，不值得建表。重复标记取第一个。
    fn entry(&self, ifd: usize, tag: u16) -> Result<Option<Entry<'a>>, DngError> {
        let count = self.u16(ifd)? as usize;
        for i in 0..count {
            let at = ifd
                .checked_add(2)
                .and_then(|v| v.checked_add(i.checked_mul(12)?))
                .ok_or(DngError::Truncated)?;
            let rec = self.d.get(at..at + 12).ok_or(DngError::Truncated)?;
            if self.order.u16(&rec[0..2]) != tag {
                continue;
            }
            let typ = self.order.u16(&rec[2..4]);
            let count = self.order.u32(&rec[4..8]);
            let unit = unit_bytes(typ).ok_or(DngError::BadTag(tag))?;
            let total = unit
                .checked_mul(count as usize)
                .ok_or(DngError::BadTag(tag))?;
            let data = if total <= 4 {
                // 短值直接放在偏移字段里，只取实际长度。
                rec.get(8..8 + total).ok_or(DngError::BadTag(tag))?
            } else {
                if total > MAX_VALUE_BYTES {
                    return Err(DngError::BadTag(tag));
                }
                let at = u64::from(self.order.u32(&rec[8..12]));
                self.range(at, total as u64)
                    .map_err(|_| DngError::StripOutOfRange {
                        offset: at,
                        length: total as u64,
                    })?
            };
            return Ok(Some(Entry { typ, count, data }));
        }
        Ok(None)
    }

    fn required(&self, ifd: usize, tag: u16) -> Result<Entry<'a>, DngError> {
        self.entry(ifd, tag)?.ok_or(DngError::MissingTag(tag))
    }

    /// SHORT/LONG/BYTE 标量，统一收进 u32。
    fn scalar(&self, e: &Entry<'_>, tag: u16) -> Result<u32, DngError> {
        match e.typ {
            3 if e.data.len() >= 2 => Ok(self.order.u16(e.data) as u32),
            4 if e.data.len() >= 4 => Ok(self.order.u32(e.data)),
            1 if !e.data.is_empty() => Ok(e.data[0] as u32),
            _ => Err(DngError::BadTag(tag)),
        }
    }

    /// SHORT/LONG/BYTE 数组，收进 u32。条带偏移、字节数、CFA 图案走这里。
    fn scalars(&self, e: &Entry<'_>, tag: u16) -> Result<Vec<u32>, DngError> {
        let order = self.order;
        match e.typ {
            3 => walk(e.data, 2, |b| Ok(order.u16(b) as u32)),
            4 => walk(e.data, 4, |b| Ok(order.u32(b))),
            1 => Ok(e.data.iter().map(|b| *b as u32).collect()),
            _ => Err(DngError::BadTag(tag)),
        }
    }

    /// 电平类标记：SHORT/LONG/RATIONAL/SRATIONAL，值可能是小数。
    fn levels(&self, e: &Entry<'_>, tag: u16) -> Result<Vec<f64>, DngError> {
        let order = self.order;
        match e.typ {
            3 => walk(e.data, 2, |b| Ok(order.u16(b) as f64)),
            4 => walk(e.data, 4, |b| Ok(order.u32(b) as f64)),
            5 => walk(e.data, 8, |b| {
                let den = order.u32(&b[4..8]);
                if den == 0 {
                    return Err(DngError::BadTag(tag));
                }
                Ok(order.u32(&b[0..4]) as f64 / den as f64)
            }),
            10 => walk(e.data, 8, |b| {
                let num = order.u32(&b[0..4]) as i32;
                let den = order.u32(&b[4..8]) as i32;
                if den == 0 {
                    return Err(DngError::BadTag(tag));
                }
                Ok(num as f64 / den as f64)
            }),
            _ => Err(DngError::BadTag(tag)),
        }
    }

    fn text(&self, e: &Entry<'_>) -> Option<String> {
        if e.typ != 2 {
            return None;
        }
        let end = e.data.iter().position(|b| *b == 0).unwrap_or(e.data.len());
        let s = String::from_utf8_lossy(&e.data[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

/// 元数据 + 存储布局是解码要的两层，分别由 [`meta`] 与 [`layout`] 产出：前者给
/// [`info`] 报尺寸，后者只在解像素时校验。拆开是为了让「元数据报得出来」不被
/// 「像素存储不认」连坐。
///
/// Metadata and storage layout are the two layers decoding needs, produced by [`meta`] and
/// [`layout`] respectively: the first reports size for [`info`], the second is only checked when
/// decoding pixels. They are separate so that "metadata is reportable" is not taken down by
/// "this pixel layout is not supported".
struct Meta {
    width: u32,
    height: u32,
    /// 每采样位深。多个声明值必须一致，不一致的写法不猜。
    ///
    /// Bits per sample. Several declared values must agree; a disagreeing layout is not guessed.
    bits: u16,
    /// 每像素采样数。**1 才解像素**（CFA/LinearRaw），其余只用于报元数据：
    /// 三通道 LinearRaw 是 iPhone 导出的 DNG 的常规写法，读不出它的宽高而说
    /// 「文件不支持」，会让信息面板和预览各说一套。
    ///
    /// Samples per pixel. Only 1 decodes pixels (CFA/LinearRaw); other values are metadata-only:
    /// three-channel LinearRaw is the normal layout of an iPhone-exported DNG, and failing to
    /// report its size as "unsupported file" would make the info panel and the preview disagree.
    spp: u16,
    make: Option<String>,
    model: Option<String>,
    black: u32,
    white: u32,
    /// 2×2 CFA 的通道索引（0=R、1=G、2=B）；线性原始数据没有图案时为 `None`。
    cfa: Option<[u8; 4]>,
}

/// 像素的存储布局：哪些字节属于哪一条带、怎么压的。
///
/// 与 [`Meta`] 分开是刻意的：存储布局的校验（压缩方式、瓦片、predictor）只属于解码，
/// 提到了元数据层就会连累 [`info`]——真实相机 DNG 用 lossless JPEG 瓦片，那样连尺寸
/// 都报不出来。
///
/// Pixel storage layout: which bytes belong to which strip and how they are compressed.
///
/// Keeping it apart from [`Meta`] is deliberate: storage checks (compression, tiles, predictor)
/// belong to decoding only, and promoting them to the metadata layer would drag [`info`] down with
/// them. Real camera DNGs use lossless-JPEG tiles, which would cost even the size report.
struct Layout {
    compression: u16,
    predictor: u16,
    rows_per_strip: u32,
    strips: Vec<(u64, u64)>,
}

fn meta(t: &Tiff<'_>, ifd: usize) -> Result<Meta, DngError> {
    let width = t.scalar(&t.required(ifd, TAG_IMAGE_WIDTH)?, TAG_IMAGE_WIDTH)?;
    let height = t.scalar(&t.required(ifd, TAG_IMAGE_LENGTH)?, TAG_IMAGE_LENGTH)?;
    if width == 0 || height == 0 || width as u64 * height as u64 > MAX_PIXELS {
        return Err(DngError::TooLarge { width, height });
    }

    let spp = t.scalar(
        &t.required(ifd, TAG_SAMPLES_PER_PIXEL)?,
        TAG_SAMPLES_PER_PIXEL,
    )?;
    // 这里**不拒绝**多采样：宽高、位深、机型、电平都是元数据，与采样数无关，
    // 报得出来就该报。拒绝发生在解像素那一步（见 `read_bytes`），那条路才需要
    // 单通道马赛克。
    //
    // Multiple samples are **not** rejected here: width, height, bit depth, camera model, and
    // levels are metadata unrelated to the sample count, so report what can be reported. The
    // rejection happens at the pixel-decoding step (see `read_bytes`), which is the step that needs
    // a single-channel mosaic.
    if spp == 0 {
        return Err(DngError::UnsupportedSamples(0));
    }

    let bits_entry = match t.entry(ifd, TAG_BITS_PER_SAMPLE_DNG)? {
        Some(e) => e,
        None => t.required(ifd, TAG_BITS_PER_SAMPLE)?,
    };
    let bits = t.scalars(&bits_entry, TAG_BITS_PER_SAMPLE_DNG)?;
    // 每采样一个声明值，多通道就是多个；各通道位深不一致的写法不猜。12/14 位的
    // packed 写法与 32 位浮点都不在这条路上。
    //
    // One declared value per sample, so multiple channels give several; a layout whose channels
    // disagree is not guessed. The 12/14-bit packed layouts and 32-bit float are off this path.
    if bits.is_empty() || bits.iter().any(|b| *b != bits[0]) || (bits[0] != 8 && bits[0] != 16) {
        return Err(DngError::UnsupportedBits(
            bits.first().copied().unwrap_or(0) as u16,
        ));
    }

    let photometric = t.scalar(&t.required(ifd, TAG_PHOTOMETRIC)?, TAG_PHOTOMETRIC)?;
    if photometric != PHOTO_CFA && photometric != PHOTO_LINEAR_RAW {
        return Err(DngError::UnsupportedPhotometric(photometric as u16));
    }

    // 逐行/逐列黑场在 DNG 里允许多值，第一版取平均，误差 LSB 级；白场取最大。
    let black = match t.entry(ifd, TAG_BLACK_LEVEL)? {
        Some(e) => {
            let v = t.levels(&e, TAG_BLACK_LEVEL)?;
            if v.is_empty() {
                0.0
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        }
        None => 0.0,
    }
    .max(0.0)
    .round() as u32;
    let white = match t.entry(ifd, TAG_WHITE_LEVEL)? {
        Some(e) => {
            let v = t.levels(&e, TAG_WHITE_LEVEL)?;
            // 声明了白场就照声明来：15 位传感器写 32767 是正常值，不能顶回满量程。
            v.iter().fold(0f64, |a, b| a.max(*b)).max(0.0)
        }
        None => {
            if bits[0] == 8 {
                255.0
            } else {
                65535.0
            }
        }
    }
    .round() as u32;
    if white <= black {
        return Err(DngError::BadLevels { black, white });
    }

    let make = t.entry(ifd, TAG_MAKE)?.as_ref().and_then(|e| t.text(e));
    // 有些转换工具只写 UniqueCameraModel，Model 空着时拿它兜底。
    let mut model = t.entry(ifd, TAG_MODEL)?.as_ref().and_then(|e| t.text(e));
    if model.is_none()
        && let Some(e) = t.entry(ifd, TAG_UNIQUE_CAMERA_MODEL)?
    {
        model = t.text(&e);
    }

    let cfa = cfa_pattern(t, ifd, photometric)?;

    Ok(Meta {
        width,
        height,
        bits: bits[0] as u16,
        spp: spp as u16,
        make,
        model,
        black,
        white,
        cfa,
    })
}

/// 存储布局：解像素才需要的那一层。
///
/// 校验都放在这里，`meta` 不碰——把「用了瓦片」「压的是 lossless JPEG」这类**像素存储
/// 问题**提到元数据层，代价是真实相机 DNG 连尺寸都报不出来。它们确实是软错
/// （[`DngError::soft_fallback`]）：换后端解得动，因为后端认识这些写法。
///
/// Storage layout: the layer only pixel decoding needs.
///
/// All the checks live here and `meta` never touches them: promoting pixel-storage facts such as
/// "uses tiles" or "lossless JPEG" to the metadata layer costs a real camera DNG even its size
/// report. They are genuinely soft errors ([`DngError::soft_fallback`]): another backend can decode
/// them because it knows these layouts.
fn layout(t: &Tiff<'_>, ifd: usize, height: u32) -> Result<Layout, DngError> {
    let compression = t.scalar(&t.required(ifd, TAG_COMPRESSION)?, TAG_COMPRESSION)? as u16;
    if compression != COMP_NONE && compression != COMP_ADOBE_DEFLATE {
        return Err(DngError::UnsupportedCompression(compression));
    }

    let planar = match t.entry(ifd, TAG_PLANAR_CONFIG)? {
        Some(e) => t.scalar(&e, TAG_PLANAR_CONFIG)? as u16,
        None => 1,
    };
    // 单通道时「按采样分面」(2) 的条带布局与 (1) 完全一致，所以两种都收。
    if planar != 1 && planar != 2 {
        return Err(DngError::UnsupportedPlanar(planar));
    }

    let predictor = match t.entry(ifd, TAG_PREDICTOR)? {
        Some(e) => t.scalar(&e, TAG_PREDICTOR)? as u16,
        None => 1,
    };
    // 3（浮点差分）要拆指数/尾数，超出第一版范围，宁可拒绝。
    if predictor != 1 && predictor != 2 {
        return Err(DngError::BadTag(TAG_PREDICTOR));
    }

    let offsets = t.scalars(&t.required(ifd, TAG_STRIP_OFFSETS)?, TAG_STRIP_OFFSETS)?;
    let counts = t.scalars(
        &t.required(ifd, TAG_STRIP_BYTE_COUNTS)?,
        TAG_STRIP_BYTE_COUNTS,
    )?;
    if offsets.len() != counts.len() || offsets.is_empty() || offsets.len() > MAX_STRIPS {
        return Err(DngError::BadTag(TAG_STRIP_OFFSETS));
    }
    let mut rows_per_strip = t.scalar(&t.required(ifd, TAG_ROWS_PER_STRIP)?, TAG_ROWS_PER_STRIP)?;
    if rows_per_strip == 0 || rows_per_strip > height {
        // TIFF 允许 0 表示整图一条带；比图高也按整图处理。
        rows_per_strip = height;
    }
    let want = height.div_ceil(rows_per_strip) as usize;
    if offsets.len() != want {
        return Err(DngError::StripCountMismatch {
            found: offsets.len(),
            want,
        });
    }
    let strips = offsets
        .iter()
        .zip(counts.iter())
        .map(|(o, c)| (u64::from(*o), u64::from(*c)))
        .collect();

    Ok(Layout {
        compression,
        predictor,
        rows_per_strip,
        strips,
    })
}

fn cfa_pattern(t: &Tiff<'_>, ifd: usize, photometric: u32) -> Result<Option<[u8; 4]>, DngError> {
    let Some(dim) = t.entry(ifd, TAG_CFA_REPEAT_DIM)? else {
        // 32803 没图案就没法去拜尔；34892 的线性数据本来就没有 CFA。
        return if photometric == PHOTO_CFA {
            Err(DngError::MissingTag(TAG_CFA_REPEAT_DIM))
        } else {
            Ok(None)
        };
    };
    let dims = t.scalars(&dim, TAG_CFA_REPEAT_DIM)?;
    let pat = t.required(ifd, TAG_CFA_PATTERN)?;
    let bytes = t.scalars(&pat, TAG_CFA_PATTERN)?;
    // 4×4 四拜尔、X-Trans 之类都要专门的邻域模板，这版只接 2×2。
    if dims.len() != 2 || dims[0] != 2 || dims[1] != 2 || bytes.len() != 4 {
        return Err(DngError::UnsupportedCfa);
    }
    let mut out = [0u8; 4];
    let mut seen = [false; 3];
    for (i, b) in bytes.iter().enumerate() {
        // TIFF-EP / DNG 规范里 CFAPattern 的取值是 **0 起**：0=R、1=G、2=B，
        // 3=C、4=M、5=Y、6=White（DNG 1.6 §4.6.2 Table 8）。
        // 只有 0..=2 是这条拜耳路能去的；3 以上是互补色/全色滤镜，要配色彩矩阵才回得到
        // 三原色，超出本解析器的范围，宁可拒绝也不猜。
        //
        // 这里曾经按 2/3/4 读，等于整体偏移一位：任何**合规**的 DNG（RGGB 写
        // `[0,1,1,2]`）都会被判成 UnsupportedCfa，只有按错误约定写的合成靶能过——
        // 那种"靶和实现对错得一样"的自证闭环，正是这个 bug 长期没被发现的原因。
        //
        // TIFF-EP / DNG put CFAPattern values at a **zero** base: 0=R, 1=G, 2=B,
        // 3=C, 4=M, 5=Y, 6=White (DNG 1.6 §4.6.2 Table 8). Only 0..=2 fit this Bayer path;
        // 3 and above are complementary-colour or panchromatic filters that need a colour
        // matrix to return to primaries, which is outside this parser, so we refuse rather
        // than guess.
        //
        // This used to read 2/3/4, an off-by-one across the board: every **conforming** DNG
        // (RGGB written as `[0,1,1,2]`) was rejected as UnsupportedCfa, and only synthetic
        // fixtures written to the same wrong convention passed, a self-confirming loop that
        // is exactly why the bug survived so long.
        let ch = match b {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => return Err(DngError::UnsupportedCfa),
        };
        out[i] = ch;
        seen[ch as usize] = true;
    }
    if !seen.iter().all(|s| *s) {
        return Err(DngError::UnsupportedCfa);
    }
    Ok(Some(out))
}

// ------------------------------------------------------------------ 条带解码

fn decode_mosaic(t: &Tiff<'_>, m: &Meta, l: &Layout) -> Result<Vec<u16>, DngError> {
    let w = m.width as usize;
    let mut mosaic = vec![0u16; w * m.height as usize];
    let bytes_per_sample = if m.bits == 8 { 1 } else { 2 };
    for (i, (offset, length)) in l.strips.iter().enumerate() {
        let y0 = i as u32 * l.rows_per_strip;
        if y0 >= m.height {
            return Err(DngError::StripCountMismatch {
                found: l.strips.len(),
                want: m.height.div_ceil(l.rows_per_strip) as usize,
            });
        }
        let rows = l.rows_per_strip.min(m.height - y0) as usize;
        let samples = rows * w;
        let need = samples * bytes_per_sample;
        let src = t.range(*offset, *length)?;
        let dst = &mut mosaic[y0 as usize * w..][..samples];
        match l.compression {
            COMP_NONE => {
                if src.len() != need {
                    return Err(DngError::StripSizeMismatch {
                        found: src.len(),
                        want: need,
                    });
                }
                fill_samples(src, t.order, m.bits, dst);
            }
            // 到这里压缩方式只可能是 8（`meta` 已经筛过），别的值早就报错返回了。
            _ => {
                // 分配只按「应有解码长度」，跟声明的压缩字节数无关：声明 4 GB 也
                // 不会申请 4 GB，越界检查在 inflate 之前就用文件长度挡掉了。
                let mut raw = vec![0u8; need];
                adobe_inflate(src, &mut raw)?;
                fill_samples(&raw, t.order, m.bits, dst);
            }
        }
        if l.predictor == 2 {
            // 横向差分按条带内每一行独立还原，行首值不带入下一行。
            for row in dst.chunks_mut(w) {
                let mut prev = row[0];
                for v in &mut row[1..] {
                    prev = v.wrapping_add(prev);
                    *v = prev;
                }
            }
        }
    }
    Ok(mosaic)
}

fn fill_samples(src: &[u8], order: Order, bits: u16, out: &mut [u16]) {
    if bits == 8 {
        for (o, b) in out.iter_mut().zip(src) {
            *o = *b as u16;
        }
        return;
    }
    for (o, b) in out.iter_mut().zip(src.chunks_exact(2)) {
        *o = order.u16(b);
    }
}

/// 黑场扣除 + 拉满 16 位量程。整数运算，测试能钉住逐位结果。
fn scale_levels(mosaic: &mut [u16], black: u32, white: u32) {
    let range = u64::from(white.saturating_sub(black)).max(1);
    for v in mosaic.iter_mut() {
        let raw = u64::from(*v).saturating_sub(u64::from(black));
        *v = (raw * 65535 / range).min(65535) as u16;
    }
}

// ------------------------------------------------------------------ 去拜尔

/// 2×2 单元平均：每个单元估出 R/G/B，单元内四像素同色；缺通道的边缘单元由邻居扩散补齐。
fn demosaic(mosaic: &[u16], w: usize, h: usize, cfa: Option<[u8; 4]>) -> Vec<u16> {
    let mut rgb = vec![0u16; w * h * 3];
    let Some(cfa) = cfa else {
        // 线性原始数据（无 CFA）：一个采样就是亮度，三通道同值。
        for (i, v) in mosaic.iter().enumerate() {
            rgb[i * 3..][..3].copy_from_slice(&[*v, *v, *v]);
        }
        return rgb;
    };
    let bw = w.div_ceil(2);
    let bh = h.div_ceil(2);
    let mut est = vec![[0u16; 3]; bw * bh];
    let mut known = vec![[false; 3]; bw * bh];
    for by in 0..bh {
        for bx in 0..bw {
            let cell = by * bw + bx;
            let mut sum = [0u64; 3];
            let mut cnt = [0u32; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    // 奇数宽高时边缘像素重复一次，这是唯一需要钳位的地方。
                    let x = (bx * 2 + dx).min(w - 1);
                    let y = (by * 2 + dy).min(h - 1);
                    let ch = cfa[(y % 2) * 2 + x % 2] as usize;
                    sum[ch] += mosaic[y * w + x] as u64;
                    cnt[ch] += 1;
                }
            }
            for ch in 0..3 {
                if cnt[ch] > 0 {
                    est[cell][ch] = round_div(sum[ch], cnt[ch] as u64);
                    known[cell][ch] = true;
                }
            }
        }
    }
    // 缺通道只可能出现在边缘重复的行/列上。只对这些单元做扩散，一轮没有任何
    // 进展就停（整图就没这个通道时留 0），避免为大图反复复制整张估计表。
    let mut todo: Vec<usize> = (0..bw * bh)
        .filter(|c| known[*c].iter().any(|k| !k))
        .collect();
    while !todo.is_empty() {
        let mut filled = 0usize;
        todo.retain(|&cell| {
            let (bx, by) = (cell % bw, cell / bw);
            let mut still_open = false;
            for ch in 0..3 {
                if known[cell][ch] {
                    continue;
                }
                let mut sum = 0u64;
                let mut cnt = 0u64;
                for (dx, dy) in [(1i32, 0), (-1, 0), (0, 1), (0, -1)] {
                    let nx = bx as i32 + dx;
                    let ny = by as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= bw as i32 || ny >= bh as i32 {
                        continue;
                    }
                    let n = ny as usize * bw + nx as usize;
                    if known[n][ch] {
                        sum += est[n][ch] as u64;
                        cnt += 1;
                    }
                }
                if cnt > 0 {
                    est[cell][ch] = round_div(sum, cnt);
                    known[cell][ch] = true;
                    filled += 1;
                } else {
                    still_open = true;
                }
            }
            still_open
        });
        if filled == 0 {
            break;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let cell = (y / 2) * bw + x / 2;
            rgb[(y * w + x) * 3..][..3].copy_from_slice(&est[cell]);
        }
    }
    rgb
}

/// 四舍五入除法；输入都是 u16 的均值，结果一定还在 u16 量程里。
fn round_div(sum: u64, cnt: u64) -> u16 {
    ((sum + cnt / 2) / cnt).min(u64::from(u16::MAX)) as u16
}

// ------------------------------------------------------------------ Adobe Deflate

/// DNG 的压缩方式 8：整条带一个 deflate 流。规范写法带 2 字节 zlib 头（可能没有
/// adler32 尾巴），在野的写入器也有写裸 deflate 的，所以头一眼不对就退回去按裸流
/// 再试一次。两条路都要求解出的字节数严格等于条带应有长度：对不上就是布局猜错
/// （例如按行分块、每行带长度的变体），直接报错，绝不返回半套像素。
fn adobe_inflate(input: &[u8], out: &mut [u8]) -> Result<(), DngError> {
    let want = out.len();
    let skip = if zlib_header(input) { 2 } else { 0 };
    match inflate(input, skip, out) {
        Ok(n) if n == want => Ok(()),
        Ok(n) => Err(DngError::StripSizeMismatch { found: n, want }),
        Err(first) => {
            if skip == 0 {
                return Err(first);
            }
            match inflate(input, 0, out) {
                Ok(n) if n == want => Ok(()),
                Ok(n) => Err(DngError::StripSizeMismatch { found: n, want }),
                // 保留第一次的错误：那条路更接近文件真实的写法。
                Err(_) => Err(first),
            }
        }
    }
}

fn zlib_header(input: &[u8]) -> bool {
    let Some(head) = input.get(0..2) else {
        return false;
    };
    let (cmf, flg) = (head[0], head[1]);
    (cmf & 0x0f) == 8 && (cmf >> 4) <= 7 && (u16::from(cmf) << 8 | u16::from(flg)) % 31 == 0
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// 动态块里「码长的码」自己的符号顺序（RFC 1951 定死的）。
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn deflate_err<T>(msg: &'static str) -> Result<T, DngError> {
    Err(DngError::Deflate(msg))
}

struct Bits<'a> {
    d: &'a [u8],
    byte: usize,
    acc: u64,
    n: u32,
}

impl Bits<'_> {
    /// LSB 先行取 `count` 位（deflate 的位序约定）。
    fn take(&mut self, count: u32) -> Result<u32, DngError> {
        if count > 16 {
            return deflate_err("位宽超范围");
        }
        while self.n < count {
            let b = *self
                .d
                .get(self.byte)
                .ok_or(DngError::Deflate("输入提前结束"))?;
            self.byte += 1;
            self.acc |= (b as u64) << self.n;
            self.n += 8;
        }
        let v = (self.acc & ((1u64 << count) - 1)) as u32;
        self.acc >>= count;
        self.n -= count;
        Ok(v)
    }

    /// 跳到字节边界：还没消费的整字节退回流里，不足一字的残余位丢掉。
    fn align(&mut self) -> Result<(), DngError> {
        let whole = (self.n / 8) as usize;
        self.byte = self
            .byte
            .checked_sub(whole)
            .ok_or(DngError::Deflate("对齐失败"))?;
        self.acc = 0;
        self.n = 0;
        Ok(())
    }
}

/// 正则（canonical）霍夫曼表：只存每长度的码字数与按 (长度, 符号) 排序的符号。
struct Huff {
    counts: [u32; 16],
    symbols: Vec<u16>,
}

fn huff_build(lengths: &[u8]) -> Result<Huff, DngError> {
    let mut counts = [0u32; 16];
    for &l in lengths {
        if l > 15 {
            return deflate_err("码长超 15");
        }
        if l > 0 {
            counts[l as usize - 1] += 1;
        }
    }
    let total: usize = counts.iter().sum::<u32>() as usize;
    let mut slot = [0usize; 16];
    let mut acc = 0usize;
    for len in 0..16 {
        slot[len] = acc;
        acc += counts[len] as usize;
    }
    let mut symbols = vec![0u16; total];
    for (s, &l) in lengths.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let at = slot[l as usize - 1];
        symbols[at] = s as u16;
        slot[l as usize - 1] = at + 1;
    }
    Ok(Huff { counts, symbols })
}

impl Huff {
    fn decode(&self, br: &mut Bits<'_>) -> Result<u16, DngError> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 0..15 {
            code |= br.take(1)? as i32;
            let cnt = self.counts[len] as i32;
            if code - first < cnt {
                let at = (index + code - first) as usize;
                return self
                    .symbols
                    .get(at)
                    .copied()
                    .ok_or(DngError::Deflate("霍夫曼表越界"));
            }
            index += cnt;
            first = (first + cnt) << 1;
            code <<= 1;
        }
        deflate_err("霍夫曼码过长")
    }
}

fn fixed_tables() -> Result<(Huff, Huff), DngError> {
    let mut lit = vec![8u8; 288];
    for (i, l) in lit.iter_mut().enumerate() {
        *l = match i {
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let dist = vec![5u8; 32];
    Ok((huff_build(&lit)?, huff_build(&dist)?))
}

fn dynamic_tables(br: &mut Bits<'_>) -> Result<(Huff, Huff), DngError> {
    let hlit = br.take(5)? as usize + 257;
    let hdist = br.take(5)? as usize + 1;
    let hclen = br.take(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return deflate_err("动态表规模越界");
    }
    let mut order = [0u8; 19];
    for i in 0..hclen {
        order[CL_ORDER[i]] = br.take(3)? as u8;
    }
    let cl = huff_build(&order)?;
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let s = cl.decode(br)?;
        // 16 = 重复上一个码长，17/18 = 连续写 0。
        let (repeat, reuse) = match s {
            0..=15 => (1usize, false),
            16 => (br.take(2)? as usize + 3, true),
            17 => (br.take(3)? as usize + 3, false),
            18 => (br.take(7)? as usize + 11, false),
            _ => return deflate_err("码长符号越界"),
        };
        let fill = if s <= 15 {
            s as u8
        } else if reuse {
            if i == 0 {
                return deflate_err("码长重复没有前值");
            }
            lengths[i - 1]
        } else {
            0
        };
        for _ in 0..repeat {
            if i >= lengths.len() {
                return deflate_err("码长表超长");
            }
            lengths[i] = fill;
            i += 1;
        }
    }
    Ok((huff_build(&lengths[..hlit])?, huff_build(&lengths[hlit..])?))
}

/// 解一个 deflate 流到 `out`，返回实际写出的字节数；调用方要求它严格等于条带长度。
fn inflate(input: &[u8], skip: usize, out: &mut [u8]) -> Result<usize, DngError> {
    let data = input.get(skip..).ok_or(DngError::Deflate("输入太短"))?;
    let mut br = Bits {
        d: data,
        byte: 0,
        acc: 0,
        n: 0,
    };
    let mut pos = 0usize;
    loop {
        let last = br.take(1)?;
        let btype = br.take(2)?;
        match btype {
            0 => {
                br.align()?;
                let len = br.take(16)? as usize;
                let nlen = br.take(16)? as u16;
                if len as u16 != !nlen {
                    return deflate_err("stored 块长度校验失败");
                }
                let end = br
                    .byte
                    .checked_add(len)
                    .ok_or(DngError::Deflate("stored 块越界"))?;
                let src =
                    br.d.get(br.byte..end)
                        .ok_or(DngError::Deflate("stored 块越界"))?;
                for &b in src {
                    *out.get_mut(pos)
                        .ok_or(DngError::Deflate("输出超出条带长度"))? = b;
                    pos += 1;
                }
                br.byte = end;
            }
            1 => {
                let (lit, dist) = fixed_tables()?;
                pos = inflate_block(&mut br, &lit, &dist, out, pos)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut br)?;
                pos = inflate_block(&mut br, &lit, &dist, out, pos)?;
            }
            _ => return deflate_err("保留了块类型"),
        }
        if last == 1 {
            break;
        }
    }
    Ok(pos)
}

fn inflate_block(
    br: &mut Bits<'_>,
    lit: &Huff,
    dist: &Huff,
    out: &mut [u8],
    mut pos: usize,
) -> Result<usize, DngError> {
    loop {
        let s = lit.decode(br)?;
        if s < 256 {
            *out.get_mut(pos)
                .ok_or(DngError::Deflate("输出超出条带长度"))? = s as u8;
            pos += 1;
            continue;
        }
        if s == 256 {
            return Ok(pos);
        }
        let li = s as usize - 257;
        if li >= LEN_BASE.len() {
            return deflate_err("长度符号越界");
        }
        let len = LEN_BASE[li] as usize + br.take(LEN_EXTRA[li] as u32)? as usize;
        let di = dist.decode(br)? as usize;
        if di >= DIST_BASE.len() {
            return deflate_err("距离符号越界");
        }
        let distance = DIST_BASE[di] as usize + br.take(DIST_EXTRA[di] as u32)? as usize;
        if distance > pos {
            return deflate_err("回指超出输出");
        }
        let from = pos - distance;
        for k in 0..len {
            let b = *out.get(from + k).ok_or(DngError::Deflate("回指越界"))?;
            *out.get_mut(pos + k)
                .ok_or(DngError::Deflate("输出超出条带长度"))? = b;
        }
        pos += len;
    }
}
