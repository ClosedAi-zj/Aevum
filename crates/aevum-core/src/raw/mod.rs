//! 相机 RAW 直读：`.rw2` `.nef` `.arw` `.cr2` `.cr3` `.orf` `.rwl`。
//!
//! 口径是用户选的「通用兜底：CFA + 内嵌预览」，两条路都必须如实报告走了哪条
//! （[`RawPath`]），界面才能给保真度贴标签，而不是让调用方去 parse 错误串。
//!
//! 1. **传感器路径**（[`RawPath::Sensor`]，16 位）：只有 IFD 自己说得清楚才解——单通道
//!    CFA 或线性原始数据、不压缩、8 或 16 位容器、条带长度能按行整除。
//! 2. **预览兜底**（[`RawPath::Preview`]，机身写的完整 JPEG）：布局不认识时（12/14 位
//!    行内打包、无损 JPEG、CRX、瓦片存储、私有 CFA 位置标记……）退到内嵌预览。
//!    预览本身是 8 位，但返回时按 `v * 257` 顶到 16 位：下游用颜色类型判断位深
//!    （`metrics::decode_rgb_f32`），两条路返回同一个类型才只有一套位深语义。
//!    **这只是位宽表示，不新增精度**，保真度由 `RawPath` 说明、不由位深冒充。
//!
//! 绝不猜像素。会产出「看着正常、其实错位」的猜测一律拒绝，宁可退预览：
//!
//! - 12/14 位打包：「每行独立打包」与「整图连续打包」在常见宽度下（14 位、8160 列）
//!   条带总字节数**完全相同**，长度算术区分不了这两种写法，猜错的后果是整幅图横向错位。
//!   `unpack::` 留了两套打包原语和逐位往返测试，但元数据层不选它们——那需要能核对的规范
//!   条文。真实样本已经证实这条取舍是对的：松下 DC-S5M2 的 RW2 是 14 位私有打包，走预览。
//! - 私有 CFA 位置标记：只认标准 `CFARepeatPatternDim`(33421)/`CFAPattern`(33422)。
//!   各家的 32xxx 私有图案标记不解读，读不到标准标记就退预览，不靠「RGBG 大概是这个顺序」。
//! - 色彩矩阵（ColorMatrix1/2、相机色域、预设基线）不做，见 `cfa::`。白平衡只用
//!   AsShotNeutral(50728) 的三通道增益，缺标记就不调色。
//! - BigTIFF/RRIF（magic 43、`II`+`UU`）与瓦片存储不支持：语义是「按行读条带」，
//!   换容器不该换掉这条，给具名错误比半吊子实现诚实。
//!
//! **CR3 不是 TIFF**：它是 ISO base media 容器，原始数据是佳能自有的 CRX 压缩，公开规范里
//! 没有可校验的解码路径，内置路径**不解 CRX**。CR3 在内置路径只做一件事：在容器字节里找
//! 机身写好的完整 JPEG 预览，找到就按 [`RawPath::Preview`] 返回，找不到给具名错误。别把
//! 这条分支当成「CR3 已支持传感器解码」。
//!
//! **M2 起 `read` 经 `backend::` 路由**：默认 feature（`raw-rawler`）下先试 rawler
//! 传感器直解——CRX、12/14 位打包这些内置拒绝的布局由它补齐，走 [`RawPath::Sensor`]；
//! 它解不动再退上面的内置双路径。`--no-default-features` 编译时没有 rawler，行为与
//! v0.3.0 完全一致。本文件的模块头描述的是**内置后端**的取舍，那些「绝不猜像素」的
//! 原则对任何后端都成立。
//!
//! 与 `dng::Tiff`、`exif` 各写一份 TIFF 解析是有意的第三层，理由见 [`tiff`] 模块头。
//!
//! Camera RAW direct read: `.rw2` `.nef` `.arw` `.cr2` `.cr3` `.orf` `.rwl`.
//!
//! The convention is the user-chosen "generic fallback: CFA + embedded preview".
//! Both routes must report which one ran ([`RawPath`]), so the UI can label fidelity
//! rather than force callers to parse error strings.
//!
//! 1. **Sensor route** ([`RawPath::Sensor`], 16-bit): decoded only when the IFD is explicit:
//!    single-channel CFA or linear raw data, no compression, an 8- or 16-bit container, and
//!    a strip length divisible by the row length.
//! 2. **Preview fallback** ([`RawPath::Preview`], full JPEG written by the camera): when the
//!    layout is unknown (12/14-bit in-row packing, lossless JPEG, CRX, tiled storage,
//!    private CFA position markers, ...) the embedded preview is used instead.
//!    The preview itself is 8-bit, but it is scaled to 16-bit by `v * 257` on return:
//!    downstream detects bit depth from the color type (`metrics::decode_rgb_f32`), so both
//!    routes returning one type keeps a single bit-depth semantics.
//!    **This is width representation only, not added precision**; fidelity is stated by
//!    `RawPath` and is never faked by bit depth.
//!
//! Pixels are never guessed. A guess that yields "looks normal, actually misaligned" is
//! rejected, and the preview is preferred instead:
//!
//! - 12/14-bit packing: "per-row packing" and "continuous whole-image packing" give exactly
//!   the same total strip byte count at common widths (14-bit, 8160 columns), so length
//!   arithmetic cannot tell the two apart, and a wrong guess shears the whole frame.
//!   `unpack::` keeps two packing primitives plus bit-exact round-trip tests, but the
//!   metadata layer does not pick them: that needs spec text that can be checked.
//!   Real samples confirm the trade-off: the Panasonic DC-S5M2 RW2 is 14-bit private
//!   packing and takes the preview.
//! - Private CFA position markers: only the standard `CFARepeatPatternDim`(33421) and
//!   `CFAPattern`(33422) are recognized.
//!   Vendor private 32xxx pattern markers are not interpreted; when the standard marker is
//!   absent the preview is used, without assuming "RGBG is probably this order".
//! - Color matrices (ColorMatrix1/2, camera gamut, preset baseline) are out of scope, see
//!   `cfa::`. White balance uses only the three channel gains from AsShotNeutral(50728);
//!   without that marker no grading is applied.
//! - BigTIFF/RRIF (magic 43, `II`+`UU`) and tiled storage are unsupported: the semantics
//!   here are "read rows by strip", and changing the container must not change that;
//!   a named error is more honest than a half implementation.
//!
//! **CR3 is not TIFF**: it is an ISO base media container, and the raw data uses Canon's
//! proprietary CRX compression, for which public specs offer no verifiable decoding path,
//! so the built-in path **does not decode CRX**.
//! On the built-in path CR3 does exactly one thing: find the full JPEG preview the camera
//! wrote inside the container bytes, return it as [`RawPath::Preview`] when found, and give
//! a named error when not.
//! This branch must not be read as "CR3 sensor decoding is supported".
//!
//! **Since M2, `read` routes through `backend::`**: under the default feature
//! (`raw-rawler`) the rawler sensor decode is tried first: CRX and the 12/14-bit packing
//! layouts the built-in path rejects are covered there, returning [`RawPath::Sensor`].
//! When rawler cannot decode, the built-in two routes above take over.
//! Built with `--no-default-features` there is no rawler, and behavior is identical to
//! v0.3.0.
//! The module header here describes the trade-offs of the **built-in backend**; those
//! "never guess pixels" principles hold for every backend.
//!
//! Keeping a third TIFF parser alongside `dng::Tiff` and `exif` is deliberate; the reasons
//! are in the [`tiff`] module header.

pub mod backend;
mod cfa;
mod preview;
// `pub(crate)` 是给 `crate::exif` 的：magic 白名单必须只有一份，见 `tiff::header_at`。
pub(crate) mod tiff;
pub mod unpack;
// rawler 适配器只在 feature 下编译；离线环境 `--no-default-features` 即可回退，
// 行为与 v0.3.0 完全一致（见 `backend::` 模块头）。
#[cfg(feature = "raw-rawler")]
mod rawler_backend;

pub use backend::{BuiltinBackend, RawBackend, RawDecode};
pub use cfa::DEMOSAIC_METHODS;

/// 当前生效的去马赛克方法名（`"cell_average"` | `"directional"`）。
///
/// 解码侧配置：不进影调参数、不落盘到工程/预设，诊断面板读它标注现状，
/// 切换走 [`set_demosaic_method`]。只影响传感器直解（内置 CFA 路与 rawler 路）；
/// 机身内嵌预览与缩略图不走去马赛克，不受它影响。
///
/// Name of the active demosaic method (`"cell_average"` | `"directional"`).
///
/// Decode-side configuration: not part of tone parameters, not persisted to a project or
/// preset file; the diagnostics panel reads it to label the current state and switches via
/// [`set_demosaic_method`].
/// It affects sensor decode only (the built-in CFA route and the rawler route); the camera
/// embedded preview and thumbnails do not run demosaic and are unaffected.
pub fn demosaic_method() -> &'static str {
    cfa::selected_demosaic()
}

/// 切换去马赛克方法并让后续解码生效；未知名字返回 `false` 且不改动。
///
/// Switch the demosaic method for subsequent decodes; an unknown name returns `false` and
/// leaves the current method unchanged.
pub fn set_demosaic_method(name: &str) -> bool {
    cfa::set_selected_demosaic(name)
}

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rayon::prelude::*;

use tiff::{Header, Tiff};

pub use crate::dng::Rgb16Image;

/// 文件大小上限。与 `dng::MAX_FILE_BYTES` 同一条线：中画幅原始文件能到几百兆，
/// 再大的当坏文件处理，别让一次 `read_to_end` 吃掉整机内存。
const MAX_FILE_BYTES: u64 = 512 << 20;
/// 解码后像素数上限。比 `dng` 的 24MP 高一档，因为厂商预览常常就是全传感器尺寸，
/// 33~45MP 的机身不该在兜底路径上报错。峰值内存按这个数算：马赛克 2 字节/像素 +
/// RGB 6 字节/像素 ≈ 320MB，到顶了也还在能忍的范围，再往上就是拿用户的机器赌。
const MAX_PIXELS: u64 = 40_000_000;
/// 条带数量上限，防伪造的 RowsPerStrip=1 把循环拉长。
const MAX_STRIPS: usize = 100_000;
/// 行步幅允许的填充余量上限。真实写入器把每行补齐到几十字节的对齐边界；余量大到
/// 塞得下第二个采样行时，说明我们对布局的判断已经不可信，报错走预览比继续猜好。
const MAX_ROW_PAD: usize = 512;

const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
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
const TAG_TILE_WIDTH: u16 = 322;
const TAG_FILL_ORDER: u16 = 327;
const TAG_PREDICTOR: u16 = 317;
const TAG_CFA_REPEAT_DIM: u16 = 33421;
const TAG_CFA_PATTERN: u16 = 33422;
/// 原始数据的 BitsPerSample（DNG 用这个编号，避免和 258 的 sRGB 位深打架）；
/// 厂商文件两边都可能出现，优先 371。
const TAG_BITS_PER_SAMPLE_RAW: u16 = 371;
const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_BLACK_LEVEL: u16 = 50714;
const TAG_WHITE_LEVEL: u16 = 50717;
const TAG_AS_SHOT_NEUTRAL: u16 = 50728;

const PHOTO_CFA: u32 = 32803;
const PHOTO_LINEAR_RAW: u32 = 34892;

const COMP_NONE: u32 = 1;
/// TIFF 里的 JPEG（含厂商写法）。它不是「不支持」，而是预览候选，交给 `preview::` 处理。
const COMP_JPEG: u32 = 6;
const COMP_JPEG_OLD: u32 = 7;

/// 认这些扩展名。`dng` 不在表里：那条路属于 `dng::is_named_dng`，`metrics::decode`
/// 先按它分流，同一个后缀有两个解法只会让位深与保真度口径分裂。
const RAW_EXTENSIONS: [&str; 7] = ["rw2", "nef", "arw", "cr2", "cr3", "orf", "rwl"];

/// 这张图到底是传感器数据还是机身预览。
///
/// Whether a decode came from sensor data or from the camera embedded preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawPath {
    /// 传感器 CFA/线性原始数据，按 16 位解出来。
    ///
    /// Sensor CFA or linear raw data, decoded as 16-bit.
    Sensor,
    /// 机身内嵌 JPEG 预览。8 位真实精度，返回时顶到 16 位。
    ///
    /// Camera embedded JPEG preview. Real 8-bit precision, scaled to 16-bit on return.
    Preview,
}

impl RawPath {
    /// 给「信息」面板的一句口径说明。UI 集成方要标注保真度就读这个，别去猜错误串。
    ///
    /// One-line convention note for the info panel.
    /// UI integrators read this to label fidelity instead of guessing from the error string.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Sensor => "传感器原始数据（CFA 直解，16 位）",
            Self::Preview => "机身内嵌 JPEG 预览（真实 8 位，非传感器数据）",
        }
    }

    /// [`Self::describe`] 的单行版：面板里那一行是右对齐且不换行的，
    /// 长句会被面板左边缘裁掉（「TIFF/RW2」只剩「F/RW2」），所以紧凑口径单独给一条。
    ///
    /// Single-line version of [`Self::describe`]: the panel row is right-aligned and does not
    /// wrap, so a long sentence is clipped at the left edge ("TIFF/RW2" keeps only "F/RW2");
    /// the compact convention therefore gets its own entry.
    pub fn short(self) -> &'static str {
        match self {
            Self::Sensor => "传感器直解",
            Self::Preview => "机身预览",
        }
    }
}

impl std::fmt::Display for RawPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.describe())
    }
}

/// 不解像素就能拿到的信息。
///
/// Information available without decoding pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct RawInfo {
    pub width: u32,
    pub height: u32,
    /// 每采样位深：传感器路径是容器位深（8/16），预览路径固定报 8——那是它真实的精度。
    ///
    /// Bits per sample: the sensor route reports the container depth (8/16), while the
    /// preview route always reports 8, which is its real precision.
    pub bits: u16,
    pub make: Option<String>,
    pub model: Option<String>,
    /// 走了哪条路。信息面板必须显示它，否则预览和传感器数据在界面上长得一样。
    ///
    /// Which route ran. The info panel must show it, otherwise a preview and sensor data
    /// look the same in the UI.
    pub path: RawPath,
    /// 容器种类（`TIFF/NEF`、`TIFF/RO`、`ISO/BMFF`……），只用于把「为什么走预览」说清楚。
    ///
    /// Container kind (`TIFF/NEF`, `TIFF/RO`, `ISO/BMFF`, ...), used only to explain why the
    /// preview route ran.
    pub container: &'static str,
}

impl RawInfo {
    /// 机型一行。Make 常是 Model 的前缀，重复就只留一次。
    ///
    /// Camera line. Make is often a prefix of Model; a duplicate is kept once.
    pub fn camera_line(&self) -> Option<String> {
        match (self.make.as_deref(), self.model.as_deref()) {
            (Some(make), Some(model)) if model.starts_with(make) => self.model.clone(),
            (Some(make), Some(model)) => Some(format!("{make} {model}")),
            (Some(make), None) => Some(make.to_owned()),
            (None, Some(model)) => Some(model.to_owned()),
            (None, None) => None,
        }
    }

    /// 尺寸 + 位深 + 来源一行，给信息面板直接显示。
    ///
    /// Size + bit depth + source in one line, shown directly by the info panel.
    pub fn fidelity_line(&self) -> String {
        format!(
            "{}×{} · {} 位 · {}",
            self.width,
            self.height,
            self.bits,
            self.path.describe()
        )
    }
}

#[derive(Debug)]
pub enum RawError {
    Io(std::io::Error),
    NotTiff,
    BadMagic(u16),
    BigTiff(u16),
    Truncated,
    FileTooLarge(u64),
    TooManyIfds(usize),
    MissingTag(u16),
    BadTag(u16),
    UnsupportedCompression(u16),
    UnsupportedPhotometric(u16),
    UnsupportedBits(u16),
    UnsupportedPlanar(u16),
    UnsupportedSamples(u16),
    UnsupportedFillOrder(u16),
    UnsupportedPredictor(u16),
    UnsupportedTiles,
    UnsupportedCfa,
    TooLarge { width: u32, height: u32 },
    StripOutOfRange { offset: u64, length: u64 },
    StripCountMismatch { found: usize, want: usize },
    StripSizeMismatch { found: usize, want: usize },
    BadLevels { black: u32, white: u32 },
    NoCfaIfd,
    NoRawData(Box<RawError>),
    NoContainerPreview,
    PreviewTooLarge { width: u32, height: u32 },
    Image(image::ImageError),
    Rawler(String),
}

impl crate::errmsg::Msg for RawError {
    fn msg(&self) -> (&'static str, Vec<crate::errmsg::Arg<'_>>) {
        match self {
            Self::Io(v) => (
                "打开 RAW 失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::NotTiff => ("不是 TIFF 家族容器：字节序标记 II/MM 不对", vec![]),
            Self::BadMagic(v) => (
                "不支持的 TIFF magic {}（认 42、0x0202、RO、RW2 的 0x0055；BigTIFF/RRIF 另有具名错误）",
                vec![crate::errmsg::Arg::Text(format!("{:#06x}", v))],
            ),
            Self::BigTiff(v) => (
                "BigTIFF/RRIF（magic {}）不支持：偏移是 64 位、IFD 计数是 8 字节，与这层的行/条带读法不兼容；请先转成 DNG",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::Truncated => ("文件被截断，标记或数据指向了文件之外", vec![]),
            Self::FileTooLarge(v) => (
                "文件超过 {} 字节上限",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::TooManyIfds(v) => (
                "IFD 数量超过 {} 层上限，这个结构不像相机写的",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::MissingTag(v) => (
                "缺少必要标记 {}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::BadTag(v) => (
                "标记 {} 的类型或数量不合法",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::UnsupportedCompression(v) => (
                "不支持的压缩方式 {}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::UnsupportedPhotometric(v) => (
                "不支持的影像解释 {}（只认 CFA 32803 与线性原始数据 34892）",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::UnsupportedBits(v) => (
                "不支持的位深 {}：传感器路径只接 8/16 位容器，12/14 位打包无法区分行内与整图连续两种写法（见模块头）",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::UnsupportedPlanar(v) => (
                "不支持的平面配置 {}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::UnsupportedSamples(v) => (
                "每像素 {} 个采样不支持（只认单通道原始数据）",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::UnsupportedFillOrder(v) => (
                "不支持的取样顺序 FillOrder {}（只按高位在前读行）",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::UnsupportedPredictor(v) => (
                "不支持的差分预测 {}（不压缩的原始数据不该带预测）",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::UnsupportedTiles => ("瓦片（Tiles）存储不支持：这层只按条带读行", vec![]),
            Self::UnsupportedCfa => (
                "CFA 图案不是可用的 2×2 RGBG 排列，或用了需要色彩矩阵的 C/M/Y 图案",
                vec![],
            ),
            Self::TooLarge { width, height } => (
                "尺寸 {}×{} 超出上限",
                vec![
                    crate::errmsg::Arg::Text(width.to_string()),
                    crate::errmsg::Arg::Text(height.to_string()),
                ],
            ),
            Self::StripOutOfRange { offset, length } => (
                "条带越界：偏移 {} 长度 {} 落在文件外",
                vec![
                    crate::errmsg::Arg::Text(offset.to_string()),
                    crate::errmsg::Arg::Text(length.to_string()),
                ],
            ),
            Self::StripCountMismatch { found, want } => (
                "条带数量 {} 与宽高推出的 {} 不一致",
                vec![
                    crate::errmsg::Arg::Text(found.to_string()),
                    crate::errmsg::Arg::Text(want.to_string()),
                ],
            ),
            Self::StripSizeMismatch { found, want } => (
                "条带字节数 {} 与行布局对不上（每行按位深至少要 {} 的整数倍）",
                vec![
                    crate::errmsg::Arg::Text(found.to_string()),
                    crate::errmsg::Arg::Text(want.to_string()),
                ],
            ),
            Self::BadLevels { black, white } => (
                "黑场 {} 不低于白场 {}，电平非法",
                vec![
                    crate::errmsg::Arg::Text(black.to_string()),
                    crate::errmsg::Arg::Text(white.to_string()),
                ],
            ),
            Self::NoCfaIfd => ("IFD 链里没有任何单通道原始数据图层", vec![]),
            Self::NoRawData(v) => (
                "没有可解码的原始影像，也没有可用的内嵌预览：{}",
                vec![crate::errmsg::Arg::Nested(&**v)],
            ),
            Self::NoContainerPreview => (
                "ISO/BMFF 容器里没有可解的完整 JPEG 预览；原始数据是厂商自有的 CRX 压缩，本版不解",
                vec![],
            ),
            Self::PreviewTooLarge { width, height } => (
                "内嵌预览尺寸 {}×{} 超出上限",
                vec![
                    crate::errmsg::Arg::Text(width.to_string()),
                    crate::errmsg::Arg::Text(height.to_string()),
                ],
            ),
            Self::Image(v) => (
                "内嵌预览解码失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::Rawler(v) => (
                "第三方解码器（rawler）失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
        }
    }
}

crate::impl_msg_display!(RawError);

impl std::error::Error for RawError {}

impl From<std::io::Error> for RawError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<image::ImageError> for RawError {
    fn from(e: image::ImageError) -> Self {
        Self::Image(e)
    }
}

impl RawError {
    /// 能不能退到内嵌预览。
    ///
    /// 只有「这个布局我不认识」可以退。「文件坏了」「超出上限」「IFD 结构不像相机写的」
    /// 都不得退：坏文件静默换一张预览，界面上一切正常，用户却以为拿到的是传感器数据——
    /// 这比报错糟糕得多。条带字节数对不上属于前者：它说明我们对打包的判断错了，
    /// 而不是数据坏了。
    ///
    /// Whether decode may fall back to the embedded preview.
    ///
    /// Only "this layout is unknown" may fall back.
    /// "File is corrupt", "over the limit" and "the IFD structure is not camera-written" must
    /// not.
    /// Silently swapping in a preview for a broken file leaves the UI looking fine while the
    /// user believes sensor data was returned, which is far worse than an error.
    /// A strip byte count mismatch belongs to the first group: it means the packing judgement
    /// was wrong, not that the data is broken.
    pub fn soft_fallback(&self) -> bool {
        matches!(
            self,
            Self::MissingTag(_)
                | Self::BadTag(_)
                | Self::UnsupportedCompression(_)
                | Self::UnsupportedPhotometric(_)
                | Self::UnsupportedBits(_)
                | Self::UnsupportedPlanar(_)
                | Self::UnsupportedSamples(_)
                | Self::UnsupportedFillOrder(_)
                | Self::UnsupportedPredictor(_)
                | Self::UnsupportedTiles
                | Self::UnsupportedCfa
                | Self::StripSizeMismatch { .. }
                | Self::NoCfaIfd
        )
    }
}

/// 只看扩展名的认领：逐帧解码的热路径用这个，绝不嗅文件内容。
///
/// 与 `dng::is_named_dng` 同一套取舍——改了名的 RAW 在这里不认领，空 `cr2` 照样认领、
/// 交给 `read` 报错。认领错一个后缀的代价是每帧白读一兆，报错的代价是序列里少一帧。
///
/// Claim by extension only: the per-frame decode hot path uses this and never sniffs content.
///
/// Same trade-off as `dng::is_named_dng`: a renamed RAW is not claimed here, while an empty
/// `cr2` is still claimed and left for `read` to reject.
/// The cost of claiming a wrong suffix is a wasted megabyte per frame; the cost of rejecting
/// is a missing frame in the sequence.
pub fn is_raw_name(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RAW_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// 结构上像不像 RAW（扩展名或头部认领）。目录扫描时用，逐帧解码用 [`is_raw_name`]。
///
/// Whether this looks like RAW structurally (claimed by extension or header).
/// Used by directory scans; per-frame decode uses [`is_raw_name`].
pub fn is_raw(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    if is_raw_name(path) {
        return true;
    }
    // 只读头部这么多就够判断 magic：与 `dng::is_dng` 一样的取舍，别为嗅探吃下整个文件。
    let Ok(data) = load(path, SNIFF_BYTES) else {
        return false;
    };
    Tiff::parse(&data).is_ok() || is_bmff(&data)
}

/// 嗅探只读头部这么多字节。
const SNIFF_BYTES: u64 = 1 << 20;

/// 只读元数据，不解码像素。
///
/// Read metadata only, without decoding pixels.
pub fn info(path: impl AsRef<Path>) -> Result<RawInfo, RawError> {
    let data = load(path.as_ref(), MAX_FILE_BYTES)?;
    info_bytes(&data)
}

pub fn info_bytes(data: &[u8]) -> Result<RawInfo, RawError> {
    if is_bmff(data) {
        let found = preview::find(data).ok_or(RawError::NoContainerPreview)?;
        return Ok(preview_info(&found, "ISO/BMFF"));
    }
    let tiff = Tiff::parse(data)?;
    match plan(&tiff) {
        Ok(p) => Ok(sensor_info(&tiff, &p.meta)),
        Err(err) => {
            if !err.soft_fallback() {
                return Err(err);
            }
            match preview::find(data) {
                Some(found) => Ok(preview_info_meta(&tiff, &found)),
                None => Err(RawError::NoRawData(Box::new(err))),
            }
        }
    }
}

/// 解成 16 位 RGB，同时报告走的是传感器还是预览路径。
///
/// M2 起经 `backend::` 路由：默认 feature 下先试 rawler 传感器直解（补齐 CRX 与
/// 12/14 位打包），解不动再退内置双路径。签名与 [`RawPath`] 语义不变，`--no-default-features`
/// 时行为与 v0.3.0 逐位一致。
///
/// Decode to 16-bit RGB and report whether the sensor or the preview route ran.
///
/// Since M2 this routes through `backend::`: under the default feature the rawler sensor
/// decode is tried first (covering CRX and 12/14-bit packing), and the built-in two routes
/// take over when it cannot decode.
/// The signature and the [`RawPath`] semantics are unchanged; with `--no-default-features`
/// the behavior is bit-for-bit identical to v0.3.0.
pub fn read(path: impl AsRef<Path>) -> Result<(Rgb16Image, RawPath), RawError> {
    let (image, path, _orientation) = read_full(path)?;
    Ok((image, path))
}

/// [`read`] 的完整版：带上 EXIF 方向码（1..8，1=不转）。
///
/// 方向由后端报告——BMFF 容器（CR3）的 Exif item 我们的 `exif::read` 定位不到，
/// rawler 解容器时顺带就有；调用方拿它喂 `exif::oriented`，别再自己读一遍。
///
/// Full version of [`read`]: it also returns the EXIF orientation code (1..8, 1 = no
/// rotation).
///
/// The orientation is reported by the backend: `exif::read` cannot locate the Exif item of a
/// BMFF container (CR3), while rawler obtains it while decoding the container.
/// Callers feed it to `exif::oriented` instead of reading it again.
pub fn read_full(path: impl AsRef<Path>) -> Result<(Rgb16Image, RawPath, u8), RawError> {
    let decoded = backend::full(path.as_ref())?;
    Ok((decoded.image, decoded.path, decoded.orientation))
}

/// 只要机身内嵌的 JPEG 预览，绝不去马赛克。
///
/// 给「几百像素就够」的用途（胶片条、任何列表缩略图）：24MP RAW 走完整解码要
/// 300ms 上下，抽内嵌预览是它的十分之一，128 边的缩略图用哪条路的像素看不出差别。
/// 没有内嵌预览的文件返回 [`RawError::NoContainerPreview`]，调用方自己决定退回 [`read`]。
///
/// Embedded JPEG preview only; demosaic is never run.
///
/// For uses where a few hundred pixels are enough (filmstrip, any list thumbnail): a full
/// decode of a 24MP RAW takes around 300ms, while extracting the embedded preview is a tenth
/// of that, and a 128px thumbnail shows no difference between the two routes.
/// A file without an embedded preview returns [`RawError::NoContainerPreview`]; the caller
/// decides whether to fall back to [`read`].
pub fn preview_only(path: impl AsRef<Path>) -> Result<Rgb16Image, RawError> {
    let data = load(path.as_ref(), MAX_FILE_BYTES)?;
    let found = preview::find(&data).ok_or(RawError::NoContainerPreview)?;
    preview::decode(&found)
}

/// 长边不少于 `min_side` 的候选里挑像素最少的一张内嵌预览。
///
/// 缩略图优先走这条：够用就行，解 1920 边那张比解 6000 边那张便宜十倍。
///
/// Pick the embedded preview with the fewest pixels among candidates whose long side is at
/// least `min_side`.
///
/// Thumbnails prefer this route: good enough is enough, and decoding the 1920px candidate
/// costs a tenth of decoding the 6000px one.
pub fn preview_at_least(path: impl AsRef<Path>, min_side: u32) -> Result<Rgb16Image, RawError> {
    let data = load(path.as_ref(), MAX_FILE_BYTES)?;
    let found = preview::find_at_least(&data, min_side).ok_or(RawError::NoContainerPreview)?;
    preview::decode(&found)
}

/// 字节入口：测试与已经握在手里的一段 RAW（例如从容器里抠出来的）走这条。
///
/// Byte entry point: used by tests and for RAW bytes already in hand (for example, extracted
/// from a container).
pub fn read_bytes(data: &[u8]) -> Result<(Rgb16Image, RawPath), RawError> {
    if is_bmff(data) {
        let found = preview::find(data).ok_or(RawError::NoContainerPreview)?;
        return Ok((preview::decode(&found)?, RawPath::Preview));
    }
    let tiff = Tiff::parse(data)?;
    let meta = match plan(&tiff) {
        Ok(p) => p.meta,
        Err(err) => {
            if !err.soft_fallback() {
                return Err(err);
            }
            match preview::find(data) {
                Some(found) => return Ok((preview::decode(&found)?, RawPath::Preview)),
                None => return Err(RawError::NoRawData(Box::new(err))),
            }
        }
    };
    // 解到这里布局已经认下了，之后的失败都是数据坏了，不再改走预览。
    let mosaic = decode_mosaic(&tiff, &meta)?;
    Ok((build_rgb16(mosaic, &meta)?, RawPath::Sensor))
}

fn load(path: &Path, limit: u64) -> Result<Vec<u8>, RawError> {
    let file = File::open(path)?;
    let mut buf = Vec::new();
    // 多读 1 字节就能区分「正好到上限」与「超限」，不用先 metadata 再 seek。
    file.take(limit + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        return Err(RawError::FileTooLarge(limit));
    }
    Ok(buf)
}

/// ISO base media 容器（CR3 这类）：magic 不在偏移 2~4，而是偏移 4~8 的 `ftyp`。
fn is_bmff(data: &[u8]) -> bool {
    data.get(4..8).is_some_and(|b| b == *b"ftyp")
}

// ------------------------------------------------------------------ IFD 挑选

/// 一个候选 IFD 的探测结果。
enum Probed {
    /// 认下了的原始数据图层。
    Raw(Meta),
    /// JPEG 图层：不当「不支持」报告，它是预览候选。
    PreviewIfd,
    /// 明确拒绝，并带上具名理由。
    Rejected(RawError),
}

struct Plan {
    meta: Meta,
    /// 面积，用于在多个可解图层里挑主图。
    px: u64,
}

/// 一个可解图层需要的全部信息，字段与解码顺序一一对应。
struct Meta {
    width: u32,
    height: u32,
    bits: u16,
    packing: unpack::Packing,
    rows_per_strip: u32,
    strips: Vec<(u64, u64)>,
    black: u32,
    white: u32,
    cfa: Option<[u8; 4]>,
    gains: Option<[u32; 3]>,
    make: Option<String>,
    model: Option<String>,
}

/// 摊开链与 SubIFD，挑**面积最大**的可解图层。
///
/// 厂商文件里 IFD0 往往只有缩略图与机身信息，主图在链上后面的 IFD 或 SubIFD 里；
/// 只读 IFD0 会挑到一张小 JPEG。全被拒时返回面积最大的那条拒绝理由——那最可能是主图。
fn plan(t: &Tiff<'_>) -> Result<Plan, RawError> {
    let cands = t.candidates()?;
    let mut best: Option<Plan> = None;
    let mut worst: Option<(u64, RawError)> = None;
    for &ifd in &cands {
        match probe(t, ifd) {
            Probed::Raw(meta) => {
                let px = u64::from(meta.width) * u64::from(meta.height);
                if best.as_ref().is_none_or(|p| px > p.px) {
                    best = Some(Plan { meta, px });
                }
            }
            Probed::PreviewIfd => {}
            Probed::Rejected(err) => {
                let px = area(t, ifd);
                if worst.as_ref().is_none_or(|(p, _)| px > *p) {
                    worst = Some((px, err));
                }
            }
        }
    }
    match best {
        Some(mut p) => {
            // 机身名字常常只在 IFD0：主图图层里没有就按候选顺序找第一个写得出来的。
            if p.meta.make.is_none() || p.meta.model.is_none() {
                let (make, model) = camera_from_chain(t, &cands);
                if p.meta.make.is_none() {
                    p.meta.make = make;
                }
                if p.meta.model.is_none() {
                    p.meta.model = model;
                }
            }
            Ok(p)
        }
        // 一条候选都没被拒（全是 JPEG 图层）时也要给具体错误，不能报「空」。
        None => match worst {
            Some((_, err)) => Err(err),
            None => Err(RawError::NoCfaIfd),
        },
    }
}

fn probe(t: &Tiff<'_>, ifd: usize) -> Probed {
    // 压缩方式 6/7 的图层是预览候选，不该占用「拒绝理由」的名额：不然报错说的会是
    // 一张缩略图，而真正的元凶是隔壁那位 14 位打包的主图。
    if let Ok(Some(e)) = t.entry(ifd, TAG_COMPRESSION)
        && let Ok(c) = t.scalar(&e, TAG_COMPRESSION)
        && (c == COMP_JPEG || c == COMP_JPEG_OLD)
    {
        return Probed::PreviewIfd;
    }
    match read_meta(t, ifd) {
        Ok(meta) => Probed::Raw(meta),
        Err(err) => Probed::Rejected(err),
    }
}

/// 图层面积，读不到宽高就是 0（只用于比较，不参与解码）。
fn area(t: &Tiff<'_>, ifd: usize) -> u64 {
    let width = t
        .entry(ifd, TAG_IMAGE_WIDTH)
        .ok()
        .flatten()
        .and_then(|e| t.scalar(&e, TAG_IMAGE_WIDTH).ok());
    let height = t
        .entry(ifd, TAG_IMAGE_LENGTH)
        .ok()
        .flatten()
        .and_then(|e| t.scalar(&e, TAG_IMAGE_LENGTH).ok());
    match (width, height) {
        (Some(w), Some(h)) => u64::from(w).saturating_mul(u64::from(h)),
        _ => 0,
    }
}

fn read_meta(t: &Tiff<'_>, ifd: usize) -> Result<Meta, RawError> {
    let width = t.scalar(&t.required(ifd, TAG_IMAGE_WIDTH)?, TAG_IMAGE_WIDTH)?;
    let height = t.scalar(&t.required(ifd, TAG_IMAGE_LENGTH)?, TAG_IMAGE_LENGTH)?;
    if width == 0
        || height == 0
        || u64::from(width)
            .checked_mul(u64::from(height))
            .is_none_or(|px| px > MAX_PIXELS)
    {
        return Err(RawError::TooLarge { width, height });
    }

    let photometric = t.scalar(&t.required(ifd, TAG_PHOTOMETRIC)?, TAG_PHOTOMETRIC)?;
    if photometric != PHOTO_CFA && photometric != PHOTO_LINEAR_RAW {
        return Err(RawError::UnsupportedPhotometric(photometric as u16));
    }

    let spp = t.scalar(
        &t.required(ifd, TAG_SAMPLES_PER_PIXEL)?,
        TAG_SAMPLES_PER_PIXEL,
    )?;
    if spp != 1 {
        return Err(RawError::UnsupportedSamples(spp as u16));
    }

    let compression = t.scalar(&t.required(ifd, TAG_COMPRESSION)?, TAG_COMPRESSION)?;
    // 压缩方式 6/7（TIFF 里的 JPEG）在 `probe` 里已经被认成预览候选，到这里的其它值
    // 都是真的不认识：无损 JPEG（NEF/CR2 的主图写法）、CRX、LZW、PackBits 一律拒绝。
    if compression != COMP_NONE {
        return Err(RawError::UnsupportedCompression(compression as u16));
    }

    // 瓦片与条带是两套布局，这层只读条带。
    if t.has(ifd, TAG_TILE_WIDTH)? {
        return Err(RawError::UnsupportedTiles);
    }

    let bits_entry = match t.entry(ifd, TAG_BITS_PER_SAMPLE_RAW)? {
        Some(e) => e,
        None => t.required(ifd, TAG_BITS_PER_SAMPLE)?,
    };
    let bits = t.scalars(&bits_entry, TAG_BITS_PER_SAMPLE_RAW)?;
    // 单通道只允许一个位深声明。12/14 位的打包写法在这里拒掉，理由见模块头。
    if bits.len() != 1 || (bits[0] != 8 && bits[0] != 16) {
        return Err(RawError::UnsupportedBits(
            bits.first().copied().unwrap_or(0) as u16,
        ));
    }
    let bits = bits[0] as u16;
    let packing = match (bits, t.order) {
        (8, _) => unpack::Packing::U8,
        (16, tiff::Order::Little) => unpack::Packing::U16Le,
        (16, tiff::Order::Big) => unpack::Packing::U16Be,
        _ => return Err(RawError::UnsupportedBits(bits)),
    };

    let planar = match t.entry(ifd, TAG_PLANAR_CONFIG)? {
        Some(e) => t.scalar(&e, TAG_PLANAR_CONFIG)? as u16,
        None => 1,
    };
    // 单通道时「按采样分面」(2) 的条带布局与 (1) 完全一致，所以两种都收。
    if planar != 1 && planar != 2 {
        return Err(RawError::UnsupportedPlanar(planar));
    }

    // 位序只有「高位在前」这一种读法；2（低位在前）要按位反转整行，猜不得。
    let fill_order = match t.entry(ifd, TAG_FILL_ORDER)? {
        Some(e) => t.scalar(&e, TAG_FILL_ORDER)?,
        None => 1,
    };
    if fill_order != 1 {
        return Err(RawError::UnsupportedFillOrder(fill_order as u16));
    }
    // 差分预测在原始数据上不压缩时没有意义，出现就说明这图层不是我们认的样子。
    let predictor = match t.entry(ifd, TAG_PREDICTOR)? {
        Some(e) => t.scalar(&e, TAG_PREDICTOR)?,
        None => 1,
    };
    if predictor != 1 {
        return Err(RawError::UnsupportedPredictor(predictor as u16));
    }

    let offsets = t.scalars(&t.required(ifd, TAG_STRIP_OFFSETS)?, TAG_STRIP_OFFSETS)?;
    let counts = t.scalars(
        &t.required(ifd, TAG_STRIP_BYTE_COUNTS)?,
        TAG_STRIP_BYTE_COUNTS,
    )?;
    if offsets.len() != counts.len() || offsets.is_empty() || offsets.len() > MAX_STRIPS {
        return Err(RawError::BadTag(TAG_STRIP_OFFSETS));
    }
    let mut rows_per_strip = t.scalar(&t.required(ifd, TAG_ROWS_PER_STRIP)?, TAG_ROWS_PER_STRIP)?;
    if rows_per_strip == 0 || rows_per_strip > height {
        // TIFF 允许 0 表示整图一条带；比图高也按整图处理。
        rows_per_strip = height;
    }
    let want = height.div_ceil(rows_per_strip) as usize;
    if offsets.len() != want {
        return Err(RawError::StripCountMismatch {
            found: offsets.len(),
            want,
        });
    }
    let strips = offsets
        .iter()
        .zip(counts.iter())
        .map(|(o, c)| (u64::from(*o), u64::from(*c)))
        .collect();

    // 电平：厂商 RAW 没有统一的黑白场标记，只有按 DNG 编号写出来的才用；
    // 缺标记时黑场按 0、白场按位深满量程。机身其实只填了高 14 位而没写白场时，
    // 结果是整幅偏暗——那是可见的保守失败，不会解出错位的像素。
    let black = match t.entry(ifd, TAG_BLACK_LEVEL)? {
        Some(e) => {
            let v = t.numbers(&e, TAG_BLACK_LEVEL)?;
            let finite: Vec<f64> = v.into_iter().filter(|x| x.is_finite()).collect();
            if finite.is_empty() {
                0.0
            } else {
                finite.iter().sum::<f64>() / finite.len() as f64
            }
        }
        None => 0.0,
    }
    .max(0.0)
    .round() as u32;
    let white = match t.entry(ifd, TAG_WHITE_LEVEL)? {
        Some(e) => {
            let v = t.numbers(&e, TAG_WHITE_LEVEL)?;
            v.into_iter()
                .filter(|x| x.is_finite())
                .fold(0f64, |a, b| a.max(b))
                .max(0.0)
        }
        None => {
            if bits == 8 {
                255.0
            } else {
                65535.0
            }
        }
    }
    .round() as u32;
    if white <= black {
        return Err(RawError::BadLevels { black, white });
    }

    let make = t.entry(ifd, TAG_MAKE)?.as_ref().and_then(|e| t.text(e));
    let mut model = t.entry(ifd, TAG_MODEL)?.as_ref().and_then(|e| t.text(e));
    if model.is_none()
        && let Some(e) = t.entry(ifd, TAG_UNIQUE_CAMERA_MODEL)?
    {
        model = t.text(&e);
    }

    let cfa = cfa_pattern(t, ifd, photometric)?;
    let gains = t
        .entry(ifd, TAG_AS_SHOT_NEUTRAL)?
        .as_ref()
        .and_then(|e| t.numbers(e, TAG_AS_SHOT_NEUTRAL).ok())
        .and_then(|v| cfa::gains_from_neutral(&v));

    Ok(Meta {
        width,
        height,
        bits,
        packing,
        rows_per_strip,
        strips,
        black,
        white,
        cfa,
        gains,
        make,
        model,
    })
}

fn cfa_pattern(t: &Tiff<'_>, ifd: usize, photometric: u32) -> Result<Option<[u8; 4]>, RawError> {
    let Some(dim) = t.entry(ifd, TAG_CFA_REPEAT_DIM)? else {
        // 32803 没图案就没法去拜尔；34892 的线性数据本来就没有 CFA。
        return if photometric == PHOTO_CFA {
            Err(RawError::MissingTag(TAG_CFA_REPEAT_DIM))
        } else {
            Ok(None)
        };
    };
    let dims = t.scalars(&dim, TAG_CFA_REPEAT_DIM)?;
    let pat = t.required(ifd, TAG_CFA_PATTERN)?;
    let bytes = t.scalars(&pat, TAG_CFA_PATTERN)?;
    // 4×4 四拜尔、X-Trans 都要专门的邻域模板；厂商私有的图案标记另说，这层不猜。
    if dims.len() != 2 || dims[0] != 2 || dims[1] != 2 || bytes.len() != 4 {
        return Err(RawError::UnsupportedCfa);
    }
    let mut out = [0u8; 4];
    let mut seen = [false; 3];
    for (i, b) in bytes.iter().enumerate() {
        // CFAPattern 取值 2/3/4 = R/G/B；C/M/Y/全色要配色彩矩阵，超出这条路。
        let ch = match b {
            2 => 0,
            3 => 1,
            4 => 2,
            _ => return Err(RawError::UnsupportedCfa),
        };
        out[i] = ch;
        seen[ch as usize] = true;
    }
    if !seen.iter().all(|s| *s) {
        return Err(RawError::UnsupportedCfa);
    }
    Ok(Some(out))
}

/// 从候选 IFD 里找第一个写了 Make/Model 的图层。主图图层常常不写机身名。
fn camera_from_chain(t: &Tiff<'_>, cands: &[usize]) -> (Option<String>, Option<String>) {
    let mut make = None;
    let mut model = None;
    for &ifd in cands {
        if make.is_some() && model.is_some() {
            break;
        }
        if make.is_none()
            && let Ok(Some(e)) = t.entry(ifd, TAG_MAKE)
            && let Some(s) = t.text(&e)
        {
            make = Some(s);
        }
        if model.is_none()
            && let Ok(Some(e)) = t.entry(ifd, TAG_MODEL)
            && let Some(s) = t.text(&e)
        {
            model = Some(s);
        }
    }
    (make, model)
}

// ------------------------------------------------------------------ 像素解码

/// 按条带解出采样表。行步幅容许每行补齐字节，见 `MAX_ROW_PAD`。
fn decode_mosaic(t: &Tiff<'_>, m: &Meta) -> Result<Vec<u16>, RawError> {
    let w = m.width as usize;
    let mut mosaic = vec![0u16; w * m.height as usize];
    let row_bytes = m
        .packing
        .row_bytes(w)
        .ok_or(RawError::UnsupportedBits(m.bits))?;
    for (i, (offset, length)) in m.strips.iter().enumerate() {
        let y0 = i as u32 * m.rows_per_strip;
        if y0 >= m.height {
            return Err(RawError::StripCountMismatch {
                found: m.strips.len(),
                want: m.height.div_ceil(m.rows_per_strip) as usize,
            });
        }
        let rows = m.rows_per_strip.min(m.height - y0) as usize;
        let src = t.range(*offset, *length)?;
        let need = rows.checked_mul(row_bytes).ok_or(RawError::TooLarge {
            width: m.width,
            height: m.height,
        })?;
        let stride = if src.len() == need {
            row_bytes
        } else if rows > 0 && src.len() > need && src.len().is_multiple_of(rows) {
            // 每行补齐到对齐边界（CR2 一类写法）：步幅整除、够放整行、余量有限。
            let candidate = src.len() / rows;
            if candidate - row_bytes > MAX_ROW_PAD {
                return Err(RawError::StripSizeMismatch {
                    found: src.len(),
                    want: need,
                });
            }
            candidate
        } else {
            return Err(RawError::StripSizeMismatch {
                found: src.len(),
                want: need,
            });
        };
        let dst = &mut mosaic[y0 as usize * w..][..rows * w];
        if !fill_rows(src, m.packing, w, rows, stride, dst) {
            return Err(RawError::StripSizeMismatch {
                found: src.len(),
                want: need,
            });
        }
    }
    Ok(mosaic)
}

/// 逐行解包。`out` 必须正好 `rows * width` 个采样。
fn fill_rows(
    src: &[u8],
    packing: unpack::Packing,
    width: usize,
    rows: usize,
    stride: usize,
    out: &mut [u16],
) -> bool {
    // 行与行互不相干：24MP 的 24 万行拆到各核上解，结果逐位不变。
    debug_assert_eq!(out.len(), rows * width);
    out.par_chunks_mut(width)
        .enumerate()
        .map(
            |(r, out_row)| match src.get(r * stride..r * stride + stride) {
                Some(src_row) => packing.unpack_row(src_row, width, out_row),
                None => false,
            },
        )
        .reduce(|| true, |a, b| a && b)
}

/// 电平映射 + 去拜尔 + 白平衡增益。
fn build_rgb16(mut mosaic: Vec<u16>, m: &Meta) -> Result<Rgb16Image, RawError> {
    cfa::scale_levels(&mut mosaic, m.black, m.white);
    let (w, h) = (m.width as usize, m.height as usize);
    // 走方法注册表：诊断面板可切换（cell_average 为默认，与 DNG 路径同口径）。
    let mut rgb = cfa::demosaic_with(cfa::selected_demosaic(), &mosaic, w, h, m.cfa);
    if let Some(gains) = m.gains {
        cfa::apply_gains(&mut rgb, gains);
    }
    // 宽高在 `read_meta` 里已经乘过并查过上限，走到 `None` 只剩申请不到内存这一种可能；
    // 报尺寸错误而不是返回一张空图——空图是「看着正常的错像素」。
    Rgb16Image::from_raw(m.width, m.height, rgb).ok_or(RawError::TooLarge {
        width: m.width,
        height: m.height,
    })
}

// ------------------------------------------------------------------ 信息组装

fn sensor_info(t: &Tiff<'_>, m: &Meta) -> RawInfo {
    RawInfo {
        width: m.width,
        height: m.height,
        bits: m.bits,
        make: m.make.clone(),
        model: m.model.clone(),
        path: RawPath::Sensor,
        container: header_name(t),
    }
}

fn preview_info(found: &preview::Found<'_>, container: &'static str) -> RawInfo {
    RawInfo {
        width: found.width,
        height: found.height,
        // 预览按 8 位报：那是它真实的精度。返回 16 位只是位宽表示（见模块头）。
        bits: 8,
        make: None,
        model: None,
        path: RawPath::Preview,
        container,
    }
}

/// TIFF 容器走预览时，机身名还在 IFD0 里，尺寸则来自预览图。
fn preview_info_meta(t: &Tiff<'_>, found: &preview::Found<'_>) -> RawInfo {
    let mut info = preview_info(found, header_name(t));
    if let Ok(cands) = t.candidates() {
        let (make, model) = camera_from_chain(t, &cands);
        info.make = make;
        info.model = model;
    }
    info
}

fn header_name(t: &Tiff<'_>) -> &'static str {
    match t.header {
        Header::Classic => "TIFF",
        Header::CanonCr2 => "TIFF/CR2",
        Header::NikonNef => "TIFF/NEF",
        Header::RoMagic => "TIFF/RO",
        Header::Rw2 => "TIFF/RW2",
    }
}
