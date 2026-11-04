//! 成片导出：逐帧调色 → 拉直/对齐位移/裁切 → 缩放 → PNG/TIFF 序列 → ffmpeg 编码。
//!
//! 像素口径与预览共用 `preview::render`：空间项与局部蒙版都跟着 `GradeParams`
//! 走，导出这条路不再少算哪一项；默认参数下与 Python oracle 逐像素一致。
//! 几何那一段（旋转 + 位移 + 取景）在 `warp` 里，预览走的是同一个函数。
//! 编码走外部 ffmpeg：H.264 8bit / H.265 10bit。输出色域由 `RenderOptions::color_space`
//! 决定——静态图嵌 ICC，视频把原色/传递函数/矩阵三条一起写进 VUI。
//!
//! Final render export: per-frame grading → straighten/align offset/crop → resize → PNG/TIFF
//! sequence → ffmpeg encode.
//!
//! Pixels share the convention of preview through `preview::render`: spatial terms and local
//! masks both come from `GradeParams`, so this path no longer skips any of them; with default
//! parameters the output matches the Python oracle pixel for pixel.
//! The geometry stage (rotate + offset + framing) lives in `warp`, and preview calls the same
//! function.
//! Encoding runs through an external ffmpeg: H.264 8bit / H.265 10bit. Output color space is
//! decided by `RenderOptions::color_space` — stills embed an ICC profile, video writes
//! primaries, transfer function and matrix together into VUI.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use image::ImageEncoder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::align::CropRect;
use crate::audio::{AudioOptions, AudioProbe};
use crate::colorspace::ColorSpace;
use crate::crop::{Motion, Viewfinder};
use crate::develop::Ev;
use crate::develop::spatial;
use crate::grade::GradeParams;
use crate::preview;
use crate::project::Metadata;
use crate::retime::{self, Plan, Samples};
use crate::warp::{self, Framing};
use crate::watermark::{self, Watermark};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows 下隐藏 ffmpeg 控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 静帧输出的容器。
///
/// JPEG/WebP 只给静帧用：视频中间帧要么保 8bit 无损、要么保 16bit，塞一层有损
/// 容器等于把画质压两遍，所以 [`StillFormat::frames_ok`] 只认前两种。
///
/// Container for still output.
///
/// JPEG/WebP are meant for stills only: an intermediate frame keeps either 8bit lossless or
/// 16bit, and pushing it through a lossy container compresses the image twice, so
/// [`StillFormat::frames_ok`] accepts only the first two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StillFormat {
    Png8,
    Tiff16,
    /// 8bit 有损；压缩率由 `RenderOptions::quality` 定（1~100）。
    ///
    /// 8bit lossy; the compression rate comes from `RenderOptions::quality` (1~100).
    Jpeg,
    /// 8bit 无损（`image` 的 WebP 编码器只有 VP8L，没有有损档，所以质量对它无效）。
    ///
    /// 8bit lossless (the `image` WebP encoder only offers VP8L, with no lossy mode, so
    /// quality has no effect on it).
    WebP,
    /// 有损（AV1 静帧），走外部 ffmpeg 的 libaom-av1；质量映射到 crf。
    /// 色域用容器里的 nclx 标记，不嵌 ICC 块，也不带 EXIF。
    ///
    /// 位深跟着 [`OutputDynamic`] 走：SDR 是 8bit `yuv444p`，`HdrPq` 是 10bit
    /// `yuv444p10le` + full range。它是唯一装得下 HDR（PQ）的容器。
    ///
    /// Lossy (AV1 still), encoded by libaom-av1 in the external ffmpeg; quality maps to crf.
    /// Color space is marked by nclx in the container: no ICC block, no EXIF.
    ///
    /// Bit depth follows [`OutputDynamic`]: 8bit `yuv444p` for SDR, 10bit `yuv444p10le` at
    /// full range for `HdrPq`. It is the only container that can hold HDR (PQ).
    Avif,
}

impl StillFormat {
    pub const ALL: [Self; 5] = [Self::Png8, Self::Tiff16, Self::Jpeg, Self::WebP, Self::Avif];

    /// 面板/工程/命令行里的稳定拼写。
    ///
    /// Stable spelling used by the panel, the project file and the command line.
    pub fn key(self) -> &'static str {
        match self {
            Self::Png8 => "png8",
            Self::Tiff16 => "tiff16",
            Self::Jpeg => "jpeg",
            Self::WebP => "webp",
            Self::Avif => "avif",
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            Self::Png8 => "PNG 8bit",
            Self::Tiff16 => "TIFF 16bit",
            Self::Jpeg => "JPEG",
            Self::WebP => "WebP 无损",
            Self::Avif => "AVIF",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Png8 => "无损、通用，网络与后期都吃得下",
            Self::Tiff16 => "16bit 无损，交给 Photoshop/达芬奇接着调",
            Self::Jpeg => "有损，质量滑块控压缩率；交付与分享体积最小",
            Self::WebP => "无损但比 PNG 小一到两成；`image` 只提供无损档",
            Self::Avif => {
                "AV1 有损，同画质比 JPEG 小一半；HDR 时出 10bit；色域写 nclx 标记，不带 ICC/EXIF"
            }
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let low = raw.trim().to_lowercase();
        Self::ALL.into_iter().find(|f| {
            f.key() == low
                || match f {
                    Self::Png8 => low == "png",
                    Self::Tiff16 => matches!(low.as_str(), "tif" | "tiff" | "tiff-16"),
                    Self::Jpeg => matches!(low.as_str(), "jpg" | "jpe"),
                    Self::WebP | Self::Avif => false,
                }
        })
    }

    /// 容器扩展名；中间帧文件名与清理都按它认。
    ///
    /// Container extension; intermediate frame names and cleanup are recognized by it.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png8 => "png",
            Self::Tiff16 => "tif",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
            Self::Avif => "avif",
        }
    }

    /// 能当 ffmpeg 的中间帧序列：只有无损档，且 TIFF 保住 16bit。
    ///
    /// Whether the format can serve as an ffmpeg intermediate sequence: only lossless ones,
    /// with TIFF keeping 16bit.
    pub fn frames_ok(self) -> bool {
        matches!(self, Self::Png8 | Self::Tiff16)
    }

    /// 质量滑块有没有意义。
    ///
    /// Whether the quality slider has any effect.
    pub fn lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::Avif)
    }

    /// 容器能不能带 EXIF 块。TIFF 的 IFD 偏移指向整个文件，搬不进去；
    /// AVIF 的 mov muxer 不写 EXIF item。
    ///
    /// Whether the container can carry an EXIF block. The TIFF IFD offsets point into the
    /// whole file, so the block cannot be moved in; the AVIF mov muxer writes no EXIF item.
    pub fn supports_exif(self) -> bool {
        !matches!(self, Self::Tiff16 | Self::Avif)
    }
}

/// 输出锐化的用途档：对标 LR 的「按输出用途锐化」，作用在缩放之后的那帧上。
///
/// Output sharpening purpose: mirrors "sharpen for output" in LR and acts on the frame after
/// resize.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputSharpen {
    /// 默认不锐化：导出像素与预览、与 Python oracle 保持一致。
    ///
    /// No sharpening by default: exported pixels match preview and the Python oracle.
    #[default]
    Off,
    /// 屏幕：像素 1:1 显示，只需要补缩放丢掉的一点边缘。
    ///
    /// Screen: pixels are shown 1:1, so only the edge detail lost in resize is restored.
    Screen,
    /// 光面打印：相纸本身对比高，轻微回补。
    ///
    /// Glossy print: paper contrast is already high, so the compensation is mild.
    Glossy,
    /// 哑面打印：染料在涂层里扩散，最需要补偿的就是这一档。
    ///
    /// Matte print: dye diffuses in the coating, and this mode needs the most compensation.
    Matte,
}

impl OutputSharpen {
    pub const ALL: [Self; 4] = [Self::Off, Self::Screen, Self::Glossy, Self::Matte];

    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Screen => "screen",
            Self::Glossy => "glossy",
            Self::Matte => "matte",
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            Self::Off => "不锐化",
            Self::Screen => "屏幕",
            Self::Glossy => "光面打印",
            Self::Matte => "哑面打印",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Off => "导出像素与预览完全一致",
            Self::Screen => "发屏上看：半径小、强度低，只回补缩放损耗",
            Self::Glossy => "光面相纸：对比够，中等强度",
            Self::Matte => "哑面相纸：染料会扩散，半径与强度都给到最大",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let low = raw.trim().to_lowercase();
        Self::ALL
            .into_iter()
            .find(|s| s.key() == low || (s == &Self::Off && matches!(low.as_str(), "none" | "0")))
    }

    /// (强度基准 amount∈[0,1], 半径基准, 阈值基准) —— 专用 USM 三元组；
    /// [`SharpenStrength`] 只乘强度。半径/阈值随用途内置、UI 不暴露（E2 口径）。
    ///
    /// 三档不是「三个名字」，是一条量出来的阶梯。`Standard` 强度下的实测
    /// （`fit_sharpen.rs::purpose_profiles_form_a_measured_ladder`，合成靶：镜头软化
    /// σ=0.7 + ATLAS §46.2 口径）：
    ///
    /// | 档 | amount | radius | thr | MTF50× | halo |
    /// |---|---|---|---|---|---|
    /// | 屏幕 | 0.55 | 0.6 | 0.004 | 1.096 | 0.19% |
    /// | 光面 | 0.80 | 0.9 | 0.003 | 1.161 | 0.71% |
    /// | 哑面 | 1.00 | 1.4 | 0.002 | 1.209 | 2.09% |
    ///
    /// 三格必须**同向**：amount 升、radius 升、threshold 降。任一格反向都会让相邻两档
    /// 互相抵消（半径更大却更不响应 = 白给），这条写在测试里逐格断言，不是备注。
    /// 哑面 `Standard`/`High` 落在 ATLAS §46.2 的 1.2–1.6× 目标内（1.209 / 1.305），
    /// 而 `Low` 实测 1.144× **故意低于目标**——强度档的意义就是同一用途下调力度。
    /// 九个真实组合的 halo 全部 < 3%，最紧的是哑面 + 高（2.93%），余量只有 0.07
    /// 个百分点，所以这两格数值不要再往上加。阈值不是装饰：拆成 0 会让低强度档的
    /// MTF50 虚高（屏幕 + 低 1.074 → 1.067），验收量的是带阈值的那一条。
    ///
    /// (base strength amount∈[0,1], base radius, base threshold) — a USM triple per purpose;
    /// [`SharpenStrength`] only scales strength. The three rungs are a measured ladder, not
    /// three names (measured at `Standard`, synthetic target: σ=0.7 lens softening, ATLAS §46.2):
    ///
    /// | purpose | amount | radius | thr | MTF50× | halo |
    /// |---|---|---|---|---|---|
    /// | screen | 0.55 | 0.6 | 0.004 | 1.096 | 0.19% |
    /// | glossy | 0.80 | 0.9 | 0.003 | 1.161 | 0.71% |
    /// | matte | 1.00 | 1.4 | 0.002 | 1.209 | 2.09% |
    ///
    /// The three fields must move **together**: amount up, radius up, threshold down. Any field
    /// reversed makes neighbouring rungs cancel out (bigger radius yet less responsive is free
    /// nothing); the test asserts this field by field. `matte` at `Standard`/`High` lands inside
    /// ATLAS §46.2's 1.2–1.6× target (1.209 / 1.305) while `Low` measures 1.144×
    /// **deliberately under target** — that is what a strength mode is for. All nine real
    /// combinations keep halo under 3%, the tightest being matte+high at 2.93%: only 0.07
    /// points of margin, so those two numbers must not be raised. The threshold is not
    /// decoration: zeroing it inflates low-strength MTF50 (screen+low 1.074 → 1.067), and the
    /// acceptance test measures the path that keeps it.
    fn profile(self) -> Option<(f32, f32, f32)> {
        match self {
            Self::Off => None,
            Self::Screen => Some((0.55, 0.6, 0.004)),
            Self::Glossy => Some((0.80, 0.9, 0.003)),
            Self::Matte => Some((1.00, 1.4, 0.002)),
        }
    }
}

/// 输出锐化的强度档，对应 LR 的低 / 标准 / 高。
///
/// Output sharpening strength, matching low / standard / high in LR.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharpenStrength {
    Low,
    #[default]
    Standard,
    High,
}

impl SharpenStrength {
    pub const ALL: [Self; 3] = [Self::Low, Self::Standard, Self::High];

    pub fn key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Standard => "standard",
            Self::High => "high",
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            Self::Low => "低",
            Self::Standard => "标准",
            Self::High => "高",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let low = raw.trim().to_lowercase();
        Self::ALL.into_iter().find(|s| s.key() == low)
    }

    fn scale(self) -> f32 {
        match self {
            Self::Low => 0.7,
            Self::Standard => 1.0,
            Self::High => 1.4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
    H265,
}

impl Codec {
    pub const ALL: [Self; 2] = [Self::H264, Self::H265];

    /// 稳定拼写，用在预设表、命令行与 MCP 参数里（`h264` / `h265`）。
    ///
    /// Stable spelling used by the preset table, the command line and MCP arguments.
    pub fn key(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265（HEVC）",
        }
    }

    /// 手写配置的宽容解析：`h264` / `h.264` / `avc` / `x264` 都认，`h265` / `h.265` /
    /// `hevc` / `x265` 同理。写错返回 `None`，由调用方给一条能看懂的报错。
    ///
    /// Forgiving parse of hand-written config: `h264` / `h.264` / `avc` / `x264` all mean
    /// H.264, and `h265` / `h.265` / `hevc` / `x265` mean H.265. A typo returns `None` so
    /// the caller can report something readable.
    pub fn parse(raw: &str) -> Option<Self> {
        let low = raw.trim().to_lowercase();
        match low.as_str() {
            "h264" | "h.264" | "avc" | "x264" => Some(Self::H264),
            "h265" | "h.265" | "hevc" | "x265" => Some(Self::H265),
            _ => None,
        }
    }
}

/// 输出的动态范围：SDR 还是 HDR（PQ）。
///
/// 与 [`ColorSpace`] 正交：色域管原色与白点，动态范围管传递函数与位深。两者的组合
/// 有约束——`HdrPq` 只允许配 `Rec2020`，理由见 [`ColorSpace::vui_pq`]。
///
/// 一个容易误解的地方：这不改变**像素内容**，只改最后写盘那一步的编码。渲染链在
/// sRGB 编码域算完，`1.0` 是 SDR 白；HDR 输出把它按 203 nits 落进 PQ，>1 的高光
/// 顺着曲线尾部继续往上走（见 [`crate::hdr_output`]）。
///
/// The dynamic range of the output: SDR or HDR (PQ).
///
/// Orthogonal to [`ColorSpace`]: the color space owns primaries and white point, the
/// dynamic range owns the transfer function and bit depth. Their combination is
/// constrained — `HdrPq` is only allowed with `Rec2020`, see [`ColorSpace::vui_pq`].
///
/// One easy misreading: this does not change **pixel content**, only the encoding of the
/// final write. The render chain finishes in sRGB-encoded values where `1.0` is SDR white;
/// HDR output lands that on 203 nits in PQ, and highlights above 1 keep climbing along the
/// curve tail (see [`crate::hdr_output`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputDynamic {
    /// 标准动态范围：sRGB 编码值直接量化成 8bit（TIFF 走 16bit）。
    ///
    /// Standard dynamic range: sRGB-encoded values quantized straight to 8bit (16bit for
    /// TIFF).
    #[default]
    Sdr,
    /// HDR：PQ（SMPTE ST 2084）+ BT.2020 + 10bit。目前只有 AVIF 能装。
    ///
    /// HDR: PQ (SMPTE ST 2084) + BT.2020 + 10bit. Only AVIF can carry it for now.
    HdrPq,
}

impl OutputDynamic {
    pub const ALL: [Self; 2] = [Self::Sdr, Self::HdrPq];

    /// 面板/工程/命令行里的稳定拼写。
    ///
    /// Stable spelling used by the panel, the project file and the command line.
    pub fn key(self) -> &'static str {
        match self {
            Self::Sdr => "sdr",
            Self::HdrPq => "hdr-pq",
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            // SDR 直接用技术名，不写「标准」：那个词在这个界面里已经属于界面密度档，
            // 同一个词指两件事会让人以为在选字号。
            //
            // SDR keeps its technical name rather than "standard": that word already means
            // UI density in this interface, and one word for two things reads like a font
            // size.
            Self::Sdr => "SDR",
            Self::HdrPq => "HDR（PQ）",
        }
    }

    /// 一句话说明，给面板悬停与 agent 用。
    ///
    /// One-line description for panel tooltips and agents.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Sdr => "SDR：多数屏幕与网络发布的默认",
            Self::HdrPq => "HDR：PQ 传递函数 + BT.2020 + 10bit，只有 AVIF 装得下；参考白 203 nits",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let low = raw.trim().to_lowercase();
        Self::ALL.into_iter().find(|d| {
            d.key() == low
                || match d {
                    Self::Sdr => matches!(low.as_str(), "sdr" | "standard" | "rec709"),
                    Self::HdrPq => matches!(
                        low.as_str(),
                        "hdr" | "hdr-pq" | "hdrpq" | "pq" | "smpte2084"
                    ),
                }
        })
    }

    /// 只有 10bit 容器装得下 PQ；8bit 会让 PQ 的暗部只剩几十级。
    ///
    /// **TIFF / PNG 的 16bit HDR 是明确不做的**（台账 B3），理由三条，都不是"懒得做"：
    ///
    /// 1. **没有哪条路能让消费级软件读对。** TIFF/PNG 里声明「这段数据是 PQ」只能靠
    ///    ICC：`parametricCurveType` 装不下 PQ 这种形状，只能塞 `curveType` 的 4096 点
    ///    采样表。Photoshop、Windows 照片、各类看图器都不解析它，读出来是一片灰——比
    ///    直接拒绝更糟，因为文件"看起来"成功了。
    /// 2. **16bit 线性也不成立。** 去掉传递函数当线性 16bit 交出去，确实能被 Nuke /
    ///    DaVinci 读对，但同样一批软件（以及 Photoshop）默认按 gamma 解释 16bit TIFF，
    ///    结果同样错，而且错得更隐蔽（看着像过曝而不是像灰）。
    /// 3. **专业链条本来就不用这两个容器。** HDR 静帧交付走 EXR / DNG；TIFF 在专业
    ///    链条里的角色是**已经过完色的 SDR 底片**，不是 HDR 母版。为它造一套只有本程序
    ///    认的 PQ 约定，只会产出一堆到别处就坏的文件。
    ///
    /// 所以这里的做法是**拒绝 + 说清**，而不是悄悄退回 SDR（那是最坏的失败方式：
    /// 界面写 HDR、文件是 SDR）。面板侧由 `AppState::apply_dynamic` 当场把组合补齐到
    /// AVIF + Rec.2020，用户不会先按导出再挨一句错误。
    ///
    /// Only a 10bit container can hold PQ; 8bit leaves the shadows with a few dozen levels.
    ///
    /// **16-bit HDR in TIFF/PNG is deliberately not implemented** (backlog B3), for three
    /// reasons, none of them laziness:
    ///
    /// 1. **Nothing consumer-grade would read it correctly.** Declaring "this TIFF/PNG is PQ"
    ///    can only go through ICC, and `parametricCurveType` cannot express PQ — it would take
    ///    a 4096-entry `curveType` sample table. Photoshop, Windows Photos and every image
    ///    viewer ignore it and show flat grey, which is worse than refusing: the file *looks*
    ///    like it worked.
    /// 2. **16-bit linear does not work either.** Dropping the transfer function and handing
    ///    out linear 16-bit is read correctly by Nuke/DaVinci, but the same set of tools (and
    ///    Photoshop) default to treating 16-bit TIFF as gamma-encoded — equally wrong and more
    ///    deceptive, since it looks overexposed rather than grey.
    /// 3. **The professional chain does not use these containers for HDR anyway.** HDR stills
    ///    ship as EXR/DNG; TIFF's role there is the *already graded SDR* plate, not the HDR
    ///    master. Inventing a PQ convention only this program understands would just produce
    ///    files that break elsewhere.
    ///
    /// So the choice is **refuse and explain**, never quietly fall back to SDR (the worst
    /// failure mode: the UI says HDR, the file is SDR). The panel side fixes the combination
    /// through `AppState::apply_dynamic`, so the user never presses export first and gets an
    /// error afterwards.
    pub fn fits(self, format: StillFormat) -> bool {
        match self {
            Self::Sdr => true,
            Self::HdrPq => matches!(format, StillFormat::Avif),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputMode {
    /// 只写 PNG/TIFF 序列。
    ///
    /// Writes the PNG/TIFF sequence only.
    Stills,
    /// 写序列后编码成视频，成功后按 `keep_frames` 决定是否清掉中间帧。
    ///
    /// Writes the sequence, then encodes a video; on success `keep_frames` decides whether the
    /// intermediate frames are removed.
    Video,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderOptions {
    pub mode: OutputMode,
    pub format: StillFormat,
    /// 有损容器的压缩质量（1~100）；无损档忽略。
    ///
    /// Compression quality for lossy containers (1~100); ignored by lossless formats.
    pub quality: u8,
    /// 把源 JPEG 的 EXIF 块原样搬进 PNG/JPEG/WebP。
    ///
    /// Copies the EXIF block of the source JPEG unchanged into PNG/JPEG/WebP.
    pub keep_exif: bool,
    /// 说明与版权信息：静帧写进 EXIF，成片写进容器标签。四项全空就一个字节不动。
    ///
    /// Description and copyright: written into EXIF for stills and into container tags for the
    /// final render. When all four fields are empty, not a byte is touched.
    pub metadata: Metadata,
    /// 每张导出的静帧旁边写一个同名 `.xmp` 旁车（`dc:` 四项 + 这一张的 `crs:` 调色）。
    /// 只对静帧模式生效：视频的中间帧是过程产物，不给它们发旁车。
    ///
    /// Writes a same-named `.xmp` sidecar next to every exported still (the four `dc:` fields
    /// plus this image's `crs:` grading).
    /// Only applies in still mode: intermediate frames of a video are intermediate products
    /// and get no sidecar.
    pub xmp_sidecar: bool,
    /// 输出锐化用途档；默认 `Off`，保持导出像素与预览一致。
    ///
    /// Output sharpening purpose; defaults to `Off`, keeping exported pixels equal to preview.
    pub sharpen_for: OutputSharpen,
    pub sharpen_strength: SharpenStrength,
    /// 中间帧输出目录。
    ///
    /// Output directory for intermediate frames.
    pub out_dir: PathBuf,
    /// 成片文件。
    ///
    /// Final render file.
    pub video_out: PathBuf,
    pub crop: Option<CropRect>,
    /// 拉直角度（度，正角=画面逆时针）；在裁切之前整幅绕中心转过去。
    ///
    /// Straighten angle in degrees (positive = counter-clockwise); the whole frame is rotated
    /// about its center before cropping.
    pub rotate: f64,
    /// 运镜（Ken Burns）：取景框从 `from` 走到 `to`，逐帧把画面重画进输出画幅。
    /// 默认关着；两个框重合时也当作没开——那种情况一像素都不重采样，不掉清晰度。
    /// 框是归一化的，作用在「拉直 + 裁切之后」的那一幅上，所以跟 `crop`/`rotate`
    /// 是两段独立的几何，先后顺序不会互相吞选区。
    ///
    /// Camera move (Ken Burns): the viewfinder travels from `from` to `to` and the picture is
    /// redrawn into the output aspect frame by frame.
    /// Off by default; two coincident boxes also count as off — that case resamples nothing
    /// and loses no sharpness.
    /// The boxes are normalized and act on the image after straighten + crop, so they form a
    /// geometry stage separate from `crop`/`rotate` and the order never swallows a selection.
    pub motion: Motion,
    /// 按长边缩放；`None` 保持裁切后的原始尺寸。
    ///
    /// Scales by the long side; `None` keeps the original size after crop.
    pub long_side: Option<u32>,
    /// 精确输出宽高：等比 cover 放大后居中裁切。与 `long_side` 同时给时以它为准。
    ///
    /// Exact output width and height: cover-scale up, then crop centered. Takes priority when
    /// given together with `long_side`.
    pub target_size: Option<(u32, u32)>,
    pub fps: f64,
    pub codec: Codec,
    pub crf: u8,
    pub preset: String,
    pub audio: Option<PathBuf>,
    /// 配乐的音量 / 延迟 / 淡入淡出 / 循环等选项；没有配乐时忽略。
    ///
    /// Volume / delay / fade / loop options for the soundtrack; ignored when there is none.
    pub audio_opts: AudioOptions,
    pub keep_frames: bool,
    /// 断点续跑：输出目录里已经写完的那一帧（连带该带的旁车）原样留着，只补没写出来的。
    /// 默认关，跟改造前一样每帧重渲——开着它重跑一次不会重新调色，改了参数还想重导就别开。
    /// 判据是「文件在且非空」，所以静帧一律先写同名 `.part` 再改名占用（见 `write_still`），
    /// 被杀在中途的那张不会留下半个文件骗过续跑。
    /// 成片模式下它必然走「先落中间帧再编码」两段路：管道直连是一条不落盘的流，没得补。
    ///
    /// Resume: a frame already written in the output directory (together with its sidecar) is
    /// kept as is, and only the missing ones are rendered.
    /// Off by default, so every frame is re-rendered as before — turning it on reruns without
    /// re-grading; do not enable it when parameters changed and a re-export is wanted.
    /// The test is "file exists and is non-empty", so a still is always written to a
    /// same-named `.part` file and then renamed (see `write_still`), and a frame killed midway
    /// never leaves half a file that fools resume.
    /// In final render mode it necessarily takes the two-stage path of writing intermediate
    /// frames and then encoding: the direct pipe is a stream with nothing on disk to fill in.
    pub resume: bool,
    /// 输出色域：静态图嵌 ICC，视频写 VUI。默认 sRGB，跟改造前的行为一致。
    ///
    /// Output color space: stills embed ICC, video writes VUI. Defaults to sRGB, matching the
    /// behavior before the rework.
    pub color_space: ColorSpace,
    /// 输出动态范围：默认 SDR，与改造前逐位一致。
    ///
    /// `HdrPq` 会改最后一步的传递函数（PQ，参考白 203 nits）并切到 10bit 编码，
    /// 只允许配 AVIF + Rec.2020（见 [`Self::check_dynamic`]）。
    ///
    /// Output dynamic range: SDR by default, bit-identical to before this rework.
    ///
    /// `HdrPq` swaps the final transfer function (PQ, 203-nit reference white) and switches
    /// to 10bit coding; AVIF + Rec.2020 only (see [`Self::check_dynamic`]).
    pub dynamic: OutputDynamic,
    /// 水印：盖在缩放与锐化之后、色域搬运之前；默认关，一像素都不动。
    ///
    /// Watermark: applied after resize and sharpening and before the color space move; off by
    /// default, touching no pixel.
    pub watermark: Watermark,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            mode: OutputMode::Video,
            format: StillFormat::Png8,
            quality: 90,
            keep_exif: false,
            metadata: Metadata::default(),
            xmp_sidecar: false,
            sharpen_for: OutputSharpen::default(),
            sharpen_strength: SharpenStrength::default(),
            out_dir: PathBuf::new(),
            video_out: PathBuf::new(),
            crop: None,
            rotate: 0.0,
            motion: Motion::default(),
            long_side: None,
            target_size: None,
            fps: 24.0,
            codec: Codec::H265,
            crf: 18,
            preset: "medium".to_owned(),
            audio: None,
            audio_opts: AudioOptions::default(),
            keep_frames: false,
            resume: false,
            color_space: ColorSpace::default(),
            dynamic: OutputDynamic::default(),
            watermark: Watermark::default(),
        }
    }
}

impl RenderOptions {
    /// 输出锐化的实际参数：用途档给基准（强度 amount∈[0,1]、半径、阈值），
    /// 强度档只乘强度。`Off` 返回 `None`，那条路上导出像素与预览逐位一致。
    ///
    /// Effective output sharpening parameters: the purpose sets the base (strength
    /// amount∈[0,1], radius, threshold) and the strength mode only multiplies strength.
    /// `Off` returns `None`, a path where exported pixels equal preview bit for bit.
    pub fn sharpen_profile(&self) -> Option<(f32, f32, f32)> {
        let (amount, radius, threshold) = self.sharpen_for.profile()?;
        Some((amount * self.sharpen_strength.scale(), radius, threshold))
    }

    /// 第 `i` 个**输出**帧（共 `total` 帧）的取景框；没开运镜或两框重合时是 `None`。
    ///
    /// 用的是输出序号而不是源帧序号：变速把 480 源帧压成 240 输出帧时，运镜照样要
    /// 在 240 帧里走完，不然片尾的框永远停在半路。
    ///
    /// Viewfinder of the `i`-th **output** frame out of `total`; `None` when the camera move
    /// is off or the two boxes coincide.
    ///
    /// The index is the output index, not the source frame index: when retime squeezes 480
    /// source frames into 240 output frames, the move still has to finish within those 240
    /// frames, otherwise the box stops halfway at the end of the clip.
    pub fn frame_view(&self, i: usize, total: usize) -> Option<Viewfinder> {
        self.motion
            .is_active()
            .then(|| self.motion.view_of(i, total))
    }

    /// 中间帧序列必须是无损容器：JPEG/WebP 只当最终交付格式，压两遍的片子不认。
    fn check_frames_ok(&self) -> Result<(), ExportError> {
        if self.format.frames_ok() {
            return Ok(());
        }
        Err(ExportError::Invalid(format!(
            "视频中间帧只能用 PNG/TIFF：{} 是有损或不受 ffmpeg 支持的序列容器",
            self.format.cn()
        )))
    }

    /// 动态范围、容器、色域三者的组合必须成立，否则报错而不是悄悄出个错的文件。
    ///
    /// 两条约束：
    /// - HDR（PQ）只有 AVIF 装得下（10bit + nclx）。选 PNG/TIFF/JPEG/WebP 时直接报错：
    ///   悄悄退回 SDR 会让人以为导出了 HDR，而文件里根本没有。
    /// - HDR（PQ）必须配 Rec.2020。见 [`ColorSpace::vui_pq`]。
    ///
    /// The combination of dynamic range, container and color space must hold; report an
    /// error instead of quietly writing a wrong file.
    ///
    /// Two constraints:
    /// - HDR (PQ) only fits in AVIF (10bit + nclx). With PNG/TIFF/JPEG/WebP this errors out:
    ///   silently falling back to SDR would look like HDR was exported when the file has none.
    /// - HDR (PQ) requires Rec.2020. See [`ColorSpace::vui_pq`].
    pub fn check_dynamic(&self) -> Result<(), ExportError> {
        // 成片那条路有两道额外的关：① 只有 H.265 能带 10bit + PQ（H.264 那条写死
        // `yuv420p`，8bit 会让 PQ 的暗部只剩几十级）；② PNG 序列承载不了 PQ——PNG 里
        // 没有地方声明「这些数是 PQ」，中转一次就被当成 sRGB 解释。
        //
        // 两道都写成「拒绝 + 说清」，不悄悄退回 SDR：界面写着 HDR、文件是 SDR 是最坏的
        // 失败方式。
        //
        // The video path has two extra gates: (1) only H.265 can carry 10bit + PQ (the H.264
        // arm is hard-wired to `yuv420p`, and 8bit leaves PQ's shadows with a few dozen
        // levels); (2) a PNG sequence cannot carry PQ — PNG has nowhere to declare "these
        // numbers are PQ", so one round trip gets them read as sRGB.
        //
        // Both are "refuse and explain", never a silent fall back to SDR: a UI that says HDR
        // while the file is SDR is the worst failure mode.
        if self.mode == OutputMode::Video && self.dynamic == OutputDynamic::HdrPq {
            if self.codec != Codec::H265 {
                return Err(ExportError::Invalid(
                    "HDR（PQ）成片只能用 H.265：H.264 那条是 8bit，PQ 的暗部会只剩几十级"
                        .to_owned(),
                ));
            }
            if self.color_space.nclx_pq().is_none() {
                return Err(ExportError::Invalid(
                    "HDR（PQ）成片必须配 Rec.2020：PQ 配 sRGB / P3 原色没有播放器认".to_owned(),
                ));
            }
            return Ok(());
        }
        if !self.dynamic.fits(self.format) {
            return Err(ExportError::Invalid(format!(
                "{} 装不下 HDR（PQ）：10bit + nclx 目前只有 AVIF 支持",
                self.format.cn()
            )));
        }
        if self.dynamic == OutputDynamic::HdrPq && self.color_space.nclx_pq().is_none() {
            return Err(ExportError::Invalid(
                "HDR（PQ）必须配 Rec.2020：PQ 配 sRGB / P3 原色没有任何播放器认".to_owned(),
            ));
        }
        Ok(())
    }

    /// 单帧导出用的默认设定：GUI「导出当前帧」只需要成片档那几个旋钮。
    ///
    /// Defaults for single-frame export: the GUI "Export Current Frame" needs only the final
    /// render knobs.
    pub fn still(format: StillFormat) -> Self {
        Self {
            mode: OutputMode::Stills,
            format,
            ..Default::default()
        }
    }
}

/// 落一张静帧时除了像素之外还要捎上的东西。`icc` / `exif` 由调用方备好：
/// 批量导出整路任务共用一份 ICC（逐帧重算是白干的活），单帧导出就地算。
///
/// Extra data carried along with the pixels when a still is persisted. `icc` / `exif` are
/// prepared by the caller: a batch export shares one ICC across the whole job (recomputing it
/// per frame is wasted work), while single-frame export computes it in place.
#[derive(Clone, Copy, Default)]
pub struct StillMeta<'a> {
    pub icc: Option<&'a [u8]>,
    pub exif: Option<&'a [u8]>,
    /// 源是 16bit 且容器是 PNG 时保住位深——这是单帧那条路有意和批量不同的地方。
    ///
    /// Keeps the bit depth when the source is 16bit and the container is PNG — this is where
    /// the single-frame path intentionally differs from batch.
    pub deep: bool,
}

/// 一张静帧要带的 EXIF 块：按 `keep_exif` 决定要不要搬源块，再把说明/创作者/版权
/// 写进去（元数据全空且没搬源块时给 `None`，那条路一个额外字节都不写）。
///
/// 单帧与批量两条路共用这一处，别各调一刀。
fn still_exif(src: &Path, options: &RenderOptions) -> Option<Vec<u8>> {
    if !options.format.supports_exif() {
        return None;
    }
    let transferred = options
        .keep_exif
        .then(|| crate::exif::transfer_block(src).unwrap_or_default())
        .filter(|block| !block.is_empty());
    crate::exif::with_metadata(transferred.as_deref(), &options.metadata)
}

/// 静帧旁的 XMP 旁车：`xmp_sidecar` 关着、或这一趟是视频模式的中间帧，就一个字节都不写。
///
/// 旁车是同目录同名的**另一个文件**，不属于容器编码那条尾巴，所以批量与单帧两条路
/// 各自在写完静帧之后调它一次；写什么由 `options.metadata` 与这一张的调色参数决定。
///
/// XMP sidecar next to a still: when `xmp_sidecar` is off, or this run produces intermediate
/// frames of a video, not a byte is written.
///
/// The sidecar is **another file** with the same name in the same directory and is not part
/// of the container encoding tail, so the batch and single-frame paths each call it once
/// after writing the still; its content comes from `options.metadata` and this image's
/// grading parameters.
pub fn write_sidecar(
    dst: &Path,
    params: &GradeParams,
    options: &RenderOptions,
) -> Result<Option<PathBuf>, ExportError> {
    if !options.xmp_sidecar || options.mode != OutputMode::Stills {
        return Ok(None);
    }
    Ok(Some(crate::xmp::write_sidecar(
        dst,
        params,
        &options.metadata,
    )?))
}

/// GUI「导出当前帧」的落盘口：像素已经是最终尺寸，这里补输出锐化、盖水印，再按输出
/// 色域搬像素并嵌 ICC、从源文件整块搬 EXIF，最后按所选容器编码。批量导出走
/// `render_frame_into` + `write_still`，两条路共用同一套尾部动作，不许各调一刀。
///
/// Persist entry for the GUI "Export Current Frame": pixels are already at final size, so
/// output sharpening and the watermark are added, pixels are moved into the output color
/// space with an embedded ICC, EXIF is copied as a whole block from the source, and the
/// chosen container encodes the result.
/// Batch export goes through `render_frame_into` + `write_still`; both paths share the same
/// tail, which must not be adapted separately.
pub fn write_single_still(
    src: &Path,
    dst: &Path,
    rgb: &mut [f32],
    w: usize,
    h: usize,
    deep: bool,
    options: &RenderOptions,
) -> Result<(), ExportError> {
    apply_output_finish(rgb, w, h, options);
    let icc = (options.color_space != ColorSpace::Srgb).then(|| options.color_space.icc());
    let exif = still_exif(src, options);
    write_still(
        dst,
        rgb,
        w as u32,
        h as u32,
        options,
        &StillMeta {
            icc: icc.as_deref(),
            exif: exif.as_deref(),
            deep,
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Frames,
    Encode,
}

#[derive(Clone, Copy, Debug)]
pub struct JobProgress {
    pub done: usize,
    pub total: usize,
    pub stage: Stage,
}

#[derive(Debug)]
pub enum ExportError {
    Cancelled,
    Empty,
    Decode {
        path: String,
        source: crate::metrics::MetricsError,
    },
    Write {
        path: String,
        source: image::ImageError,
    },
    NoFfmpeg,
    Ffmpeg(String),
    Invalid(String),
    Io(std::io::Error),
    Xmp(crate::xmp::XmpError),
}

impl crate::errmsg::Msg for ExportError {
    fn msg(&self) -> (&'static str, Vec<crate::errmsg::Arg<'_>>) {
        match self {
            Self::Cancelled => ("已取消", vec![]),
            Self::Empty => ("没有可导出的帧", vec![]),
            Self::Decode { path, source } => (
                "读图失败 {}：{}",
                vec![
                    crate::errmsg::Arg::Text(path.to_string()),
                    crate::errmsg::Arg::Text(source.to_string()),
                ],
            ),
            Self::Write { path, source } => (
                "写图失败 {}：{}",
                vec![
                    crate::errmsg::Arg::Text(path.to_string()),
                    crate::errmsg::Arg::Text(source.to_string()),
                ],
            ),
            Self::NoFfmpeg => ("找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）", vec![]),
            Self::Ffmpeg(v) => (
                "ffmpeg 失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::Invalid(v) => (
                "参数不合法：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::Io(v) => ("IO 失败：{}", vec![crate::errmsg::Arg::Text(v.to_string())]),
            Self::Xmp(v) => ("写 XMP 旁车失败：{}", vec![crate::errmsg::Arg::Nested(v)]),
        }
    }
}

crate::impl_msg_display!(ExportError);

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Xmp(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ExportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<crate::xmp::XmpError> for ExportError {
    fn from(e: crate::xmp::XmpError) -> Self {
        Self::Xmp(e)
    }
}

/// 优先 `AEVUM_FFMPEG`（旧名 `LUMEN_FFMPEG` 回退），其次 PATH 里的 ffmpeg。
///
/// Prefers `AEVUM_FFMPEG` (falling back to the old name `LUMEN_FFMPEG`), then ffmpeg on
/// `PATH`.
pub fn find_ffmpeg() -> Option<PathBuf> {
    if let Some(p) = crate::env::var_os("FFMPEG")
        && !p.is_empty()
    {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    // 打包后 ffmpeg 随 exe 一起分发：先看 exe 同目录，再看 tools/ 子目录。
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join(name), dir.join("tools").join(name)] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// `dir/frame_%06d.png`（TIFF 用 `.tif`，JPEG 用 `.jpg`）。
///
/// `dir/frame_%06d.png` (`.tif` for TIFF, `.jpg` for JPEG).
pub fn pattern_for(dir: &Path, format: StillFormat) -> PathBuf {
    dir.join(format!("frame_%06d.{}", format.ext()))
}

/// 逐帧调色、对齐、裁切并写出。`params`/`gains`/`offsets` 与 `paths` 等长。
///
/// Grades, aligns, crops and writes every frame. `params`/`gains`/`offsets` are as long as
/// `paths`.
pub fn export_stills(
    paths: &[PathBuf],
    params: &[GradeParams],
    gains: &[Ev],
    offsets: &[[f64; 2]],
    options: &RenderOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(JobProgress) + Sync),
) -> Result<Vec<PathBuf>, ExportError> {
    export_stills_planned(
        paths, params, gains, offsets, options, None, cancel, progress,
    )
}

/// 带时间重映射的静帧导出：`plan` 决定输出帧数与每帧的取样来源。
///
/// 帧数由计划说话（可能比 `paths` 多或少），进度与文件名按**输出帧**走；
/// `params`/`gains`/`offsets` 与 `paths` 一样，始终按**源帧**索引。计划为 `None`
/// 时逐帧走原来那条 1:1 快路，一个多余像素都不碰。
///
/// Still export with retime: `plan` decides the output frame count and the samples of each
/// frame.
///
/// The plan dictates the frame count, which may be larger or smaller than `paths`, and
/// progress and file names follow **output frames**; `params`/`gains`/`offsets`, like
/// `paths`, are always indexed by **source frame**.
/// With `plan` set to `None`, the original 1:1 fast path runs frame by frame and no extra
/// pixel is touched.
#[allow(clippy::too_many_arguments)]
pub fn export_stills_planned(
    paths: &[PathBuf],
    params: &[GradeParams],
    gains: &[Ev],
    offsets: &[[f64; 2]],
    options: &RenderOptions,
    plan: Option<&Plan>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(JobProgress) + Sync),
) -> Result<Vec<PathBuf>, ExportError> {
    if paths.is_empty() {
        return Err(ExportError::Empty);
    }
    if params.len() != paths.len() || gains.len() != paths.len() || offsets.len() != paths.len() {
        return Err(ExportError::Invalid(
            "参数、曝光补偿、位移的数量必须与帧数一致".to_owned(),
        ));
    }
    if options.out_dir.as_os_str().is_empty() {
        return Err(ExportError::Invalid("输出目录为空".to_owned()));
    }
    if let Some((tw, th)) = options.target_size
        && (tw < 2 || th < 2 || tw % 2 == 1 || th % 2 == 1)
    {
        return Err(ExportError::Invalid(format!(
            "目标尺寸必须是偶数且不小于 2：{tw}×{th}"
        )));
    }
    let total = check_plan(plan, paths.len())?;
    fs::create_dir_all(&options.out_dir)?;

    progress(JobProgress {
        stage: Stage::Frames,
        done: 0,
        total,
    });

    let done = AtomicUsize::new(0);
    let tick = Mutex::new(());

    // ICC 字节跟任务走，不跟帧走：逐帧重算是白干的活。sRGB 刻意不嵌，保持旧行为。
    let profile: Option<Vec<u8>> =
        (options.color_space != ColorSpace::Srgb).then(|| options.color_space.icc());

    let run_one = |i: usize, s: &mut Scratch, mix: &mut Vec<f32>| -> Result<PathBuf, ExportError> {
        if cancel.load(Ordering::SeqCst) {
            return Err(ExportError::Cancelled);
        }
        let dst = frame_path(&options.out_dir, options.format, i);
        // 续跑：这一帧上次已经写完了就原样留着，连解码都不碰。进度照样往前推，
        // 否则状态栏会停在「跳过的那些帧」的数目上不动，看着像卡死。
        if options.resume && frame_is_done(&dst, options) {
            let _guard = tick.lock().unwrap_or_else(|e| e.into_inner());
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            progress(JobProgress {
                stage: Stage::Frames,
                done: n,
                total,
            });
            return Ok(dst);
        }
        let samples = samples_at(plan, i);
        let primary = samples.0[0].0;
        let view = options.frame_view(i, total);
        if samples.0.len() == 1 {
            let (slot, tw, th) = render_frame_into(
                &paths[primary],
                &params[primary],
                gains[primary],
                offsets[primary],
                view,
                options,
                s,
            )?;
            let buf = s.slot(slot);
            write_still_at(&dst, buf, tw, th, options, &profile, paths, primary)?;
        } else {
            let (tw, th) = mix_frames(
                &samples, view, paths, params, gains, offsets, options, cancel, s, mix,
            )?;
            write_still_at(&dst, mix, tw, th, options, &profile, paths, primary)?;
        }
        write_sidecar(&dst, &params[primary], options)?;

        // 串行化回调，并行完成顺序不定也不能让进度倒退。
        let _guard = tick.lock().unwrap_or_else(|e| e.into_inner());
        let n = done.fetch_add(1, Ordering::SeqCst) + 1;
        progress(JobProgress {
            stage: Stage::Frames,
            done: n,
            total,
        });
        Ok(dst)
    };

    // 并发路数按内存和核数算（见 worker_count）：每帧解码是单线程的，多一路就多一倍
    // 吞吐，但一路要占上百 MB 到近 GB 的 f32 缓冲，不能按核数开满。
    //
    // 工作线程是专门的 std 线程，不占全局 rayon 池：帧内的解码、缩放照样借池子里的
    // 空闲核，而写盘、阻塞式的入队永远不会把池线程堵住。
    let workers = worker_count(paths).min(total).max(1);
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let slots: Vec<Mutex<Option<Result<PathBuf, ExportError>>>> =
        (0..total).map(|_| Mutex::new(None)).collect();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut scratch = Scratch::default();
                // 只有真要混帧的那一路才会把它撑大；1:1 导出一个字节都不占。
                let mut mix = Vec::new();
                loop {
                    if cancel.load(Ordering::SeqCst) || failed.load(Ordering::SeqCst) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= total {
                        break;
                    }
                    let out = run_one(i, &mut scratch, &mut mix);
                    if out.is_err() {
                        failed.store(true, Ordering::SeqCst);
                    }
                    let mut slot = slots[i].lock().unwrap_or_else(|e| e.into_inner());
                    *slot = Some(out);
                }
            });
        }
    });

    if cancel.load(Ordering::SeqCst) {
        return Err(ExportError::Cancelled);
    }
    let mut outs = Vec::with_capacity(total);
    for slot in slots {
        match slot.into_inner().unwrap_or_else(|e| e.into_inner()) {
            Some(r) => outs.push(r?),
            None => return Err(ExportError::Cancelled),
        }
    }
    Ok(outs)
}

/// 第 `i` 个输出帧的取样来源；没有计划时就是源帧 `i` 自己。
fn samples_at(plan: Option<&Plan>, i: usize) -> Samples {
    match plan {
        None => Samples::single(i),
        Some(p) => p.per_frame[i].clone(),
    }
}

/// 由重映射设定算导出计划。恒等设定（关掉、或常速零快门）返回 `None`，
/// 让导出走原来那条一个像素都不重采样的 1:1 快路。
///
/// Builds the export plan from the retime settings. An identity setting (off, or normal
/// speed with a zero shutter) returns `None`, letting export take the original 1:1 fast path
/// that resamples nothing.
pub fn plan_for(
    retime: &retime::Retime,
    n_src: usize,
    fps: f64,
) -> Result<Option<Plan>, ExportError> {
    if retime.is_identity() {
        return Ok(None);
    }
    retime::plan(retime, n_src, fps)
        .map(Some)
        .map_err(ExportError::Invalid)
}

/// 计划与序列是否配套。序列在算完计划后被改过，越界取帧会直接 panic，这里拦下。
fn check_plan(plan: Option<&Plan>, n_src: usize) -> Result<usize, ExportError> {
    let Some(plan) = plan else { return Ok(n_src) };
    if plan.per_frame.is_empty() {
        return Err(ExportError::Invalid("重映射计划里没有输出帧".to_owned()));
    }
    for samples in &plan.per_frame {
        if samples.0.is_empty() {
            return Err(ExportError::Invalid(
                "重映射计划里有一帧没有任何来源".to_owned(),
            ));
        }
        if let Some(bad) = samples.0.iter().find(|slot| slot.0 >= n_src) {
            return Err(ExportError::Invalid(format!(
                "重映射计划要取第 {} 帧，但序列只有 {n_src} 帧：重新算一次计划",
                bad.0
            )));
        }
    }
    Ok(plan.per_frame.len())
}

/// 把一帧现成的像素写成静图（元数据按 `primary` 那张源帧搬）。
#[allow(clippy::too_many_arguments)]
fn write_still_at(
    dst: &Path,
    buf: &mut [f32],
    w: u32,
    h: u32,
    options: &RenderOptions,
    profile: &Option<Vec<u8>>,
    paths: &[PathBuf],
    primary: usize,
) -> Result<(), ExportError> {
    // 搬运块只在真要带元数据时才读：PNG 默认那一路一张额外的小 IO 都不发。
    let exif = still_exif(&paths[primary], options);
    write_still(
        dst,
        buf,
        w,
        h,
        options,
        &StillMeta {
            icc: profile.as_deref(),
            exif: exif.as_deref(),
            deep: false,
        },
    )
}

/// 按权重把若干源帧混成一帧：快门>0 的运动模糊、慢放的交叉淡化都走这一条。
///
/// 输出锐化与水印在混完之后统一补一次——逐帧盖水印再平均，出来的水印是半透明重影。
/// 运镜的取景框按**输出帧**算，一个输出帧里那几帧源帧共用同一个框。
#[allow(clippy::too_many_arguments)]
fn mix_frames(
    samples: &Samples,
    view: Option<Viewfinder>,
    paths: &[PathBuf],
    params: &[GradeParams],
    gains: &[Ev],
    offsets: &[[f64; 2]],
    options: &RenderOptions,
    cancel: &AtomicBool,
    s: &mut Scratch,
    mix: &mut Vec<f32>,
) -> Result<(u32, u32), ExportError> {
    let mut size = (0u32, 0u32);
    for (k, (src, weight)) in samples.0.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(ExportError::Cancelled);
        }
        let src = *src;
        let (slot, tw, th) = render_frame_raw(
            &paths[src],
            &params[src],
            gains[src],
            offsets[src],
            view,
            options,
            s,
        )?;
        if k == 0 {
            size = (tw, th);
            mix.clear();
            mix.resize(tw as usize * th as usize * 3, 0.0);
        } else if (tw, th) != size {
            return Err(ExportError::Invalid(format!(
                "混帧时第 {src} 帧的尺寸 {tw}×{th} 与第 {} 帧 {}×{} 不一致",
                samples.0[0].0, size.0, size.1
            )));
        }
        let w = *weight;
        mix.par_iter_mut()
            .zip(&*s.slot(slot))
            .for_each(|(o, v)| *o += w * v);
    }
    apply_output_finish(mix, size.0 as usize, size.1 as usize, options);
    Ok(size)
}

/// 渲一个输出帧并压成管道字节：SDR 是 `rgb24`，HDR 是 BT.2020 + PQ 的 `rgb48le`。
///
/// 单来源那一路直接在 scratch 里出，跟改造前的 1:1 路径逐字节相同；只有真的要混
/// 几帧时才用 `mix` 多占一块全幅缓冲。
///
/// 返回的字节布局必须和 `pipe_command` 里写的 `-pix_fmt` 一致——两边由
/// `options.dynamic` 同时决定，**不能有一边自己判断**，否则 ffmpeg 会把 16bit 的字节
/// 当 8bit 读，画面变成一卷斜纹。
///
/// Render one output frame and pack it for the pipe: `rgb24` for SDR, BT.2020 + PQ
/// `rgb48le` for HDR.
///
/// The byte layout must match the `-pix_fmt` written by `pipe_command`; both are decided by
/// `options.dynamic` and **neither side may decide on its own**, or ffmpeg reads 16bit bytes as
/// 8bit and the picture turns into diagonal stripes.
#[allow(clippy::too_many_arguments)]
fn output_frame_bytes(
    plan: Option<&Plan>,
    i: usize,
    paths: &[PathBuf],
    params: &[GradeParams],
    gains: &[Ev],
    offsets: &[[f64; 2]],
    options: &RenderOptions,
    cancel: &AtomicBool,
    s: &mut Scratch,
    mix: &mut Vec<f32>,
) -> Result<(Vec<u8>, u32, u32), ExportError> {
    let samples = samples_at(plan, i);
    let primary = samples.0[0].0;
    // 运镜按输出帧插值：没变速时输出帧数就是源帧数，两条路算出来的框一样。
    let frames = plan.map_or(paths.len(), Plan::frames);
    let view = options.frame_view(i, frames);
    let (rgb, tw, th) = if samples.0.len() == 1 {
        let (slot, w, h) = render_frame_into(
            &paths[primary],
            &params[primary],
            gains[primary],
            offsets[primary],
            view,
            options,
            s,
        )?;
        (s.slot(slot), w, h)
    } else {
        let (w, h) = mix_frames(
            &samples, view, paths, params, gains, offsets, options, cancel, s, mix,
        )?;
        (&mut mix[..], w, h)
    };
    let bytes = if options.dynamic == OutputDynamic::HdrPq {
        to_rgb48_pq(rgb, options.color_space)
    } else {
        to_rgb24(rgb, options.color_space)
    };
    Ok((bytes, tw, th))
}

/// 每帧的临时缓冲，跨帧复用。
///
/// 24MP 一帧要 ~700MB 临时内存（解码缓冲、缩放中间列、缩放输出、裁切输出）。每帧
/// 重新分配看着便宜，实测是导出里仅次于解码的开销：系统供电充足时分到的是全新
/// 零页，但进程跑过一轮之后这些页会被裁剪，再分配就得逐页触一次页错误——同一份
/// 序列第二次导出能慢一倍。按线程复用这几块缓冲，整轮导出只在首帧付一次这个钱。
#[derive(Default)]
struct Scratch {
    /// 解码 + 调色后的全分辨率缓冲。
    a: Vec<f32>,
    /// 位移 / 裁切结果（不需要时直接借 `a`）。
    b: Vec<f32>,
    /// 区域缩放的横向中间结果（`tw × sh`）。
    tmp: Vec<f32>,
    /// 缩放输出。
    c: Vec<f32>,
    /// 精确目标尺寸居中裁切后的输出。
    d: Vec<f32>,
}

/// 结果落在哪块缓冲里。
#[derive(Clone, Copy)]
enum Slot {
    A,
    B,
    C,
    D,
}

impl Scratch {
    fn slot(&mut self, slot: Slot) -> &mut [f32] {
        let buf = match slot {
            Slot::A => &mut self.a,
            Slot::B => &mut self.b,
            Slot::C => &mut self.c,
            Slot::D => &mut self.d,
        };
        &mut buf[..]
    }
}

/// 一帧的完整像路：解码 → 调色 → 对齐位移与取景 → 运镜采样 → 缩放。
///
/// 写 PNG 序列和直连 ffmpeg 共用这一段，保证两条路出来的像素一致。
fn render_frame_into(
    path: &Path,
    params: &GradeParams,
    gain: Ev,
    offset: [f64; 2],
    view: Option<Viewfinder>,
    options: &RenderOptions,
    s: &mut Scratch,
) -> Result<(Slot, u32, u32), ExportError> {
    let (slot, ow, oh) = render_frame_raw(path, params, gain, offset, view, options, s)?;
    // 输出锐化 + 水印：LR 的语义是「按输出用途，在缩放到最终尺寸之后」再补一刀。
    // 放在这一层的尾巴上，静帧与直连 ffmpeg 两条路吃到的 sharpen 完全一样；预览不
    // 看这两个字段，所以它们是纯成片效果。水印也排在色域搬运之前，跟着画面一起搬。
    apply_output_finish(s.slot(slot), ow as usize, oh as usize, options);
    Ok((slot, ow, oh))
}

/// `render_frame_into` 去掉最后那两刀的版本：混帧要先平均再锐化/盖水印，
/// 逐帧补的话水印会跟着半透明帧叠成重影。
fn render_frame_raw(
    path: &Path,
    params: &GradeParams,
    gain: Ev,
    offset: [f64; 2],
    view: Option<Viewfinder>,
    options: &RenderOptions,
    s: &mut Scratch,
) -> Result<(Slot, u32, u32), ExportError> {
    let (w, h) = if params.dcp_profile.is_empty() {
        load_rgb_f32_into(path, &mut s.a)?
    } else {
        let (rgb, w, h, _) = crate::metrics::decode_for_grade(path, params).map_err(|source| {
            ExportError::Decode {
                path: path.display().to_string(),
                source,
            }
        })?;
        s.a = rgb;
        (w, h)
    };
    if w == 0 || h == 0 {
        return Err(ExportError::Invalid(format!(
            "图像尺寸为 0：{}",
            path.display()
        )));
    }
    let Scratch { a, b, tmp, c, d } = s;
    // 动态范围决定终点要不要保留白以上的亮度：HDR 走不截断那条，SDR 沿用原路径
    // （逐位不变）。这一句是 HDR 真能产出高光的**唯一**开关——预览里看不到差别，
    // 因为屏幕显示不到白以上。
    //
    // The dynamic range decides whether brightness above white survives to the encoder: HDR
    // takes the unclamped path, SDR keeps the original one (bit-identical). This line is the
    // **only** switch that makes HDR actually carry highlights; the preview cannot show the
    // difference because a display does not go past white.
    if options.dynamic == OutputDynamic::HdrPq {
        preview::render_hdr(a, w as usize, h as usize, params, gain);
    } else {
        preview::render(a, w as usize, h as usize, params, gain);
    }

    let crop = clamp_crop(options.crop, w, h)?;
    let framing = Framing {
        angle_deg: options.rotate,
        off: offset,
    };
    let full = crop.x == 0 && crop.y == 0 && crop.w == w as usize && crop.h == h as usize;
    let (mut slot, bw, bh): (Slot, u32, u32) = if full && framing.is_identity() {
        (Slot::A, w, h)
    } else {
        let (cw, ch) = warp::transform_into(
            a,
            w as usize,
            h as usize,
            framing,
            Some((crop.x, crop.y, crop.w, crop.h)),
            b,
        );
        (Slot::B, cw as u32, ch as u32)
    };
    let base: &[f32] = if matches!(slot, Slot::A) { a } else { b };

    // 运镜那一帧：取景框就是这一帧的构图，所以下面 cover + 居中裁那两步不再插手，
    // 否则框里选好的画面会被二次裁掉。输出画幅照旧由 target_size / long_side 定。
    if let Some(v) = view {
        let (tw, th) = match options.target_size {
            Some(size) => size,
            None => fit_size(bw, bh, options.long_side),
        };
        // 框跟画幅比例对不上时在像素口径上往里收一格、中心不动：宁可少留，也不把画面拉扁。
        let win = v.view_window(bw, bh, tw, th);
        let (vx, vy, vw, vh) = (win[0], win[1], win[2], win[3]);
        if vw <= tw as f64 + 1e-9 && vh <= th as f64 + 1e-9 {
            // 推近（含 1:1）：双线性一步铺满，框走的那些亚像素位置照单全收。
            warp::view_into(
                base,
                bw as usize,
                bh as usize,
                win,
                tw as usize,
                th as usize,
                d,
            );
            return Ok((Slot::D, tw, th));
        }
        // 缩小：框先往外兜成整数窗口（半格细节别切走），再按面积平均——
        // 这条路双线性会漏采样点，远处的瓦片和树叶就会起摩尔纹。
        let ix = vx.floor().max(0.0) as usize;
        let iy = vy.floor().max(0.0) as usize;
        let iw = (vx + vw).ceil().min(bw as f64) as usize - ix;
        let ih = (vy + vh).ceil().min(bh as f64) as usize - iy;
        warp::transform_into(
            base,
            bw as usize,
            bh as usize,
            Framing::default(),
            Some((ix, iy, iw, ih)),
            d,
        );
        resize_area_f32_into(d, iw, ih, tw as usize, th as usize, tmp, c);
        return Ok((Slot::C, tw, th));
    }

    let (ow, oh) = if let Some((tw, th)) = options.target_size {
        let (sw, sh) = cover_size(bw, bh, tw, th);
        resize_area_f32_into(
            base,
            bw as usize,
            bh as usize,
            sw as usize,
            sh as usize,
            tmp,
            c,
        );
        let rect = Rect {
            x: ((sw - tw) / 2) as usize,
            y: ((sh - th) / 2) as usize,
            w: tw as usize,
            h: th as usize,
        };
        // 这一趟只按目标画幅居中取景，几何变换上面那道已经做过了。
        warp::transform_into(
            c,
            sw as usize,
            sh as usize,
            Framing::default(),
            Some((rect.x, rect.y, rect.w, rect.h)),
            d,
        );
        slot = Slot::D;
        (tw, th)
    } else {
        let (tw, th) = fit_size(bw, bh, options.long_side);
        if (tw, th) != (bw, bh) {
            resize_area_f32_into(
                base,
                bw as usize,
                bh as usize,
                tw as usize,
                th as usize,
                tmp,
                c,
            );
            slot = Slot::C;
        }
        (tw, th)
    };
    Ok((slot, ow, oh))
}

/// 成片的最后两刀：输出锐化，然后盖水印。两刀都默认关着，一个像素都不碰。
fn apply_output_finish(rgb: &mut [f32], w: usize, h: usize, options: &RenderOptions) {
    if let Some((amount, radius, threshold)) = options.sharpen_profile() {
        // 输出锐化专用 USM + 线性软阈值（atlas §21 DoUSMWithThreshold 口径）：
        // 不再复用采集锐化，也不吃显影面板的 sharp_protect/masking——那是显影
        // 链的量。锐化在 sRGB 编码域，HDR 只换最后一步的传递函数。
        spatial::output_sharpen_usm(
            rgb,
            w,
            h,
            spatial::UsmSettings::new(amount, radius, threshold),
        );
    }
    watermark::apply_in_place(rgb, w, h, &options.watermark);
}

/// f32 像素按输出色域转换后压成 8bit rgb24，供管道直喂编码器。
fn to_rgb24(rgb: &mut [f32], color_space: ColorSpace) -> Vec<u8> {
    color_space.convert_in_place(rgb);
    let mut out = vec![0u8; rgb.len()];
    out.par_iter_mut()
        .zip(&*rgb)
        .for_each(|(o, v)| *o = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    out
}

/// f32 像素 → BT.2020 + PQ 的 `rgb48le` 字节流，供 HDR 成片的管道输入。
///
/// 三步顺序不能换：① 先搬色域（矩阵在 sRGB 线性域里算），② 再换传递函数（PQ 是
/// **编码值**的函数，必须在色域搬运完之后），③ 最后量化到 16bit。反过来做等于拿
/// sRGB 编码值去查 PQ，出来一片黑。
///
/// 与 [`to_rgb24`] 的另一个区别是不夹到 8bit：PQ 的暗部在这条链上只有几百个码值，
/// 先压成 8bit 再交给 10bit 编码器，等于把 PQ 的暗部层级全扔掉。
///
/// f32 pixels → an `rgb48le` byte stream in BT.2020 + PQ, for the HDR video pipe input.
///
/// The order of the three steps is not negotiable: (1) move the gamut first (the matrix is
/// computed in the sRGB linear domain), (2) then swap the transfer function (PQ is a function
/// of the **encoded** value, so it must come after the gamut move), (3) quantize to 16bit last.
/// Reversing them means looking up PQ with sRGB-encoded values, which comes out black.
///
/// The other difference from [`to_rgb24`]: no 8bit clamp. PQ's shadows only span a few hundred
/// codes on this chain, and squashing to 8bit before handing to a 10bit encoder throws that
/// shadow layering away.
fn to_rgb48_pq(rgb: &mut [f32], color_space: ColorSpace) -> Vec<u8> {
    color_space.convert_in_place(rgb);
    crate::hdr_output::encode_pq_in_place(rgb);
    to_rgb48_le(rgb)
}

/// 用序列帧编码视频；`-progress pipe:1` 的帧计数转成进度。
///
/// Encodes a video from the frame sequence; the frame counter of `-progress pipe:1` becomes
/// the progress.
pub fn encode_video(
    ffmpeg: &Path,
    options: &RenderOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(JobProgress) + Sync),
) -> Result<PathBuf, ExportError> {
    if cancel.load(Ordering::SeqCst) {
        return Err(ExportError::Cancelled);
    }
    if !ffmpeg.is_file() {
        return Err(ExportError::NoFfmpeg);
    }
    if !(options.fps.is_finite() && options.fps > 0.0) {
        return Err(ExportError::Invalid("帧率必须大于 0".to_owned()));
    }
    // 这条是「PNG 序列 → 视频」的老路：帧是 8bit sRGB 写进 PNG 的，PQ 无处声明。
    // HDR 成片走直连编码那条（原始字节直喂编码器），不经过这里。
    //
    // This is the old "PNG sequence → video" path: frames are written as 8bit sRGB PNGs, and
    // PQ has nowhere to be declared. HDR goes through the piped encoder instead.
    if options.dynamic == OutputDynamic::HdrPq {
        return Err(ExportError::Invalid(
            "HDR 成片不能走 PNG 序列：PNG 里没法声明 PQ。请用直连编码（默认路径）".to_owned(),
        ));
    }
    if options.video_out.as_os_str().is_empty() {
        return Err(ExportError::Invalid("成片路径为空".to_owned()));
    }
    options.check_frames_ok()?;
    let total = count_frames(&options.out_dir, options.format)?;
    if total == 0 {
        return Err(ExportError::Empty);
    }
    if let Some(parent) = options.video_out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    progress(JobProgress {
        stage: Stage::Encode,
        done: 0,
        total,
    });

    let probe = match &options.audio {
        Some(path) => Some(probe_audio(ffmpeg, path)?),
        None => None,
    };

    let mut cmd = Command::new(ffmpeg);
    cmd.args(video_command(ffmpeg, options, total, probe.as_ref()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ExportError::Ffmpeg("拿不到 ffmpeg 的 stdout".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ExportError::Ffmpeg("拿不到 ffmpeg 的 stderr".to_owned()))?;

    // 标准错误单独收，避免管道写满把 ffmpeg 卡死。
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut stderr = stderr;
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut last = 0usize;
    loop {
        // 空闲时 200ms 一次；进度行连续到达时也逐行检查。
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = err_handle.join();
            return Err(ExportError::Cancelled);
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if let Some(frame) = parse_progress_frame(&line) {
                    let done = frame.min(total);
                    if done > last {
                        last = done;
                        progress(JobProgress {
                            stage: Stage::Encode,
                            done,
                            total,
                        });
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let status = child.wait()?;
    let stderr = err_handle.join().unwrap_or_default();
    if cancel.load(Ordering::SeqCst) {
        return Err(ExportError::Cancelled);
    }
    if !status.success() {
        let text = String::from_utf8_lossy(&stderr);
        let tail = tail_chars(&text, 800);
        let detail = if tail.is_empty() {
            format!("退出码 {}", status.code().unwrap_or(-1))
        } else {
            tail
        };
        return Err(ExportError::Ffmpeg(detail));
    }
    if last < total {
        progress(JobProgress {
            stage: Stage::Encode,
            done: total,
            total,
        });
    }
    if !options.keep_frames {
        remove_frames(&options.out_dir, options.format);
    }
    Ok(options.video_out.clone())
}

/// 渲染线程交给写管道线程的一帧：帧号、rgb24 像素、宽高。
type FrameMsg = (usize, Result<(Vec<u8>, u32, u32), ExportError>);

/// 逐帧调色并直接写进 ffmpeg 的 stdin，跳过中间的 PNG 序列。
///
/// 渲染按像素量限流并行，写出严格按帧序：完成顺序不定，先到的帧在重排缓冲里
/// 等，管道那头永远是 0、1、2……。缓冲上限就是工作线程数，编码器慢时反压到
/// 渲染，不会把几 GB 的帧堆在内存里。首帧先单独渲染，拿到确切的输出尺寸再起
/// 编码器。
///
/// Grades frame by frame and writes straight into ffmpeg's stdin, skipping the PNG sequence.
///
/// Rendering runs in parallel with a pixel-count limit while writing follows frame order
/// strictly: completion order varies, so an early frame waits in a reorder buffer and the
/// pipe always sees 0, 1, 2…
/// The buffer bound is the worker count, so a slow encoder back-pressures rendering and
/// never piles gigabytes of frames in memory.
/// The first frame is rendered alone to obtain the exact output size before the encoder
/// starts.
#[allow(clippy::too_many_arguments)]
pub fn export_video_direct(
    ffmpeg: &Path,
    paths: &[PathBuf],
    params: &[GradeParams],
    gains: &[Ev],
    offsets: &[[f64; 2]],
    options: &RenderOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(JobProgress) + Sync),
) -> Result<PathBuf, ExportError> {
    export_video_direct_planned(
        ffmpeg, paths, params, gains, offsets, options, None, cancel, progress,
    )
}

/// 带时间重映射的直连导出：输出帧数与每帧来源由 `plan` 决定。
///
/// Direct export with retime: `plan` decides the output frame count and the source of each
/// frame.
#[allow(clippy::too_many_arguments)]
pub fn export_video_direct_planned(
    ffmpeg: &Path,
    paths: &[PathBuf],
    params: &[GradeParams],
    gains: &[Ev],
    offsets: &[[f64; 2]],
    options: &RenderOptions,
    plan: Option<&Plan>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(JobProgress) + Sync),
) -> Result<PathBuf, ExportError> {
    if !ffmpeg.is_file() {
        return Err(ExportError::NoFfmpeg);
    }
    if paths.is_empty() {
        return Err(ExportError::Empty);
    }
    if params.len() != paths.len() || gains.len() != paths.len() || offsets.len() != paths.len() {
        return Err(ExportError::Invalid(
            "参数、曝光补偿、位移的数量必须与帧数一致".to_owned(),
        ));
    }
    if !(options.fps.is_finite() && options.fps > 0.0) {
        return Err(ExportError::Invalid("帧率必须大于 0".to_owned()));
    }
    if options.video_out.as_os_str().is_empty() {
        return Err(ExportError::Invalid("成片路径为空".to_owned()));
    }
    if let Some((tw, th)) = options.target_size
        && (tw < 2 || th < 2 || tw % 2 == 1 || th % 2 == 1)
    {
        return Err(ExportError::Invalid(format!(
            "目标尺寸必须是偶数且不小于 2：{tw}×{th}"
        )));
    }
    if let Some(parent) = options.video_out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(ExportError::Cancelled);
    }

    let total = check_plan(plan, paths.len())?;
    // 首帧单独渲染：拿到确切的输出尺寸才能起编码器。这块 scratch 转手给第一个
    // 渲染线程继续用，一开始就按满负荷备好缓冲。
    let mut first_scratch = Scratch::default();
    let mut first_mix = Vec::new();
    let (first, fw, fh) = output_frame_bytes(
        plan,
        0,
        paths,
        params,
        gains,
        offsets,
        options,
        cancel,
        &mut first_scratch,
        &mut first_mix,
    )?;

    let probe = match &options.audio {
        Some(path) => Some(probe_audio(ffmpeg, path)?),
        None => None,
    };

    let mut cmd = Command::new(ffmpeg);
    cmd.args(pipe_command(options, total, probe.as_ref(), (fw, fh)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ExportError::Ffmpeg("拿不到 ffmpeg 的 stdin".to_owned()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ExportError::Ffmpeg("拿不到 ffmpeg 的 stdout".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ExportError::Ffmpeg("拿不到 ffmpeg 的 stderr".to_owned()))?;

    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut stderr = stderr;
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    progress(JobProgress {
        stage: Stage::Frames,
        done: 0,
        total,
    });

    let workers = worker_count(paths).min(total).max(1);
    let stop = AtomicBool::new(false);
    let stop_ref = &stop;
    let (frame_tx, frame_rx): (mpsc::SyncSender<FrameMsg>, _) = mpsc::sync_channel(workers);
    let next_frame = AtomicUsize::new(1);
    let render_error: Mutex<Option<ExportError>> = Mutex::new(None);

    let outcome = std::thread::scope(|scope| -> Result<(), ExportError> {
        // 渲染线程是专门的 std 线程，不占 rayon 池：`send` 会阻塞（编码器慢时反压），
        // 池线程一旦被它堵住，主线程自己的 `par_iter` 就会跟着饿死——实测能慢上
        // 三百倍并偶发死锁。帧内的解码与缩放走全局池，两者互不占位。
        let render = |tx: &mpsc::SyncSender<FrameMsg>| {
            let mut scratch = Scratch::default();
            let mut mix = Vec::new();
            loop {
                if stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst) {
                    break;
                }
                let i = next_frame.fetch_add(1, Ordering::SeqCst);
                if i >= total {
                    break;
                }
                let one = (|| -> Result<(), ExportError> {
                    let (bytes, w, h) = output_frame_bytes(
                        plan,
                        i,
                        paths,
                        params,
                        gains,
                        offsets,
                        options,
                        cancel,
                        &mut scratch,
                        &mut mix,
                    )?;
                    tx.send((i, Ok((bytes, w, h))))
                        .map_err(|_| ExportError::Cancelled)?;
                    Ok(())
                })();
                if let Err(e) = one {
                    let mut slot = render_error.lock().unwrap_or_else(|p| p.into_inner());
                    if slot.is_none() {
                        *slot = Some(e);
                    }
                    stop.store(true, Ordering::SeqCst);
                    break;
                }
            }
        };
        for _ in 0..workers {
            let tx = frame_tx.clone();
            scope.spawn(move || render(&tx));
        }
        drop(frame_tx);

        let mut error: Option<ExportError> = None;
        let mut pending: std::collections::BTreeMap<usize, Vec<u8>> =
            std::collections::BTreeMap::new();
        let mut want = 1usize;
        let mut done = 0usize;

        if let Err(e) = stdin.write_all(&first) {
            error = Some(e.into());
        } else {
            done = 1;
            progress(JobProgress {
                stage: Stage::Frames,
                done,
                total,
            });
        }

        while error.is_none() {
            let Ok((i, r)) = frame_rx.recv() else { break };
            match r {
                Ok((bytes, w, h)) => {
                    if (w, h) != (fw, fh) {
                        error = Some(ExportError::Invalid(format!(
                            "第 {i} 帧的尺寸 {w}×{h} 与首帧 {fw}×{fh} 不一致"
                        )));
                        break;
                    }
                    pending.insert(i, bytes);
                }
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
            while let Some(bytes) = pending.remove(&want) {
                if let Err(e) = stdin.write_all(&bytes) {
                    error = Some(e.into());
                    break;
                }
                want += 1;
                done += 1;
                progress(JobProgress {
                    stage: Stage::Frames,
                    done,
                    total,
                });
            }
        }

        if error.is_some() {
            stop_ref.store(true, Ordering::SeqCst);
        }
        // 关掉管道让 ffmpeg 收尾；渲染线程可能还堵在满队列上，先排空再放它们走。
        drop(stdin);
        while frame_rx.recv().is_ok() {}
        if error.is_none() {
            error = render_error
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
        }
        // 取消优先：用户点了停止，不该看到「管道断了」这种实现细节。
        if cancel.load(Ordering::SeqCst) {
            return Err(ExportError::Cancelled);
        }
        match error {
            Some(e) => {
                let _ = child.kill();
                Err(e)
            }
            None if done < total => Err(ExportError::Invalid(format!(
                "只写出 {done}/{total} 帧，成片不完整"
            ))),
            None => Ok(()),
        }
    });

    if let Err(e) = outcome {
        let _ = child.wait();
        let _ = err_handle.join();
        return Err(e);
    }

    let mut last = 0usize;
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = err_handle.join();
            return Err(ExportError::Cancelled);
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if let Some(frame) = parse_progress_frame(&line) {
                    let done = frame.min(total);
                    if done > last {
                        last = done;
                        progress(JobProgress {
                            stage: Stage::Encode,
                            done,
                            total,
                        });
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let status = child.wait()?;
    let stderr = err_handle.join().unwrap_or_default();
    if cancel.load(Ordering::SeqCst) {
        return Err(ExportError::Cancelled);
    }
    if !status.success() {
        let text = String::from_utf8_lossy(&stderr);
        let tail = tail_chars(&text, 800);
        let detail = if tail.is_empty() {
            format!("退出码 {}", status.code().unwrap_or(-1))
        } else {
            tail
        };
        return Err(ExportError::Ffmpeg(detail));
    }
    if last < total {
        progress(JobProgress {
            stage: Stage::Encode,
            done: total,
            total,
        });
    }
    Ok(options.video_out.clone())
}

/// 成片导出入口：不留帧的 8bit 视频直连 ffmpeg，其余仍走「先写序列再编码」。
///
/// Final render entry: an 8bit video that keeps no frames goes straight to ffmpeg, anything
/// else still writes the sequence first and encodes afterwards.
#[allow(clippy::too_many_arguments)]
pub fn export_video(
    ffmpeg: &Path,
    paths: &[PathBuf],
    params: &[GradeParams],
    gains: &[Ev],
    offsets: &[[f64; 2]],
    options: &RenderOptions,
    cancel: &AtomicBool,
    progress: &(dyn Fn(JobProgress) + Sync),
) -> Result<PathBuf, ExportError> {
    export_video_planned(
        ffmpeg, paths, params, gains, offsets, options, None, cancel, progress,
    )
}

/// 带时间重映射的成片导出。重映射只在**写字节之前**生效：先按 `plan` 渲出目标
/// 帧序，再交给原来那套编码，所以 `fps`、配乐的 `shortest`/循环算的都是变速后的
/// 真实时长。
///
/// Final render export with retime. Retime applies **before bytes are written**: the target
/// frame order is rendered from `plan` and then handed to the existing encoder, so `fps` and
/// the soundtrack `shortest`/loop rules all count the real duration after the speed change.
#[allow(clippy::too_many_arguments)]
pub fn export_video_planned(
    ffmpeg: &Path,
    paths: &[PathBuf],
    params: &[GradeParams],
    gains: &[Ev],
    offsets: &[[f64; 2]],
    options: &RenderOptions,
    plan: Option<&Plan>,
    cancel: &AtomicBool,
    progress: &(dyn Fn(JobProgress) + Sync),
) -> Result<PathBuf, ExportError> {
    if options.mode != OutputMode::Video {
        return Err(ExportError::Invalid("只有视频模式能导出成片".to_owned()));
    }
    options.check_frames_ok()?;
    // 管道直连是一条不落盘的流，中间帧都不存在，也就没有「补哪几帧」可说；
    // 开了续跑就必须走两段路，让中间帧落在盘上供下一次复用。
    if options.format == StillFormat::Png8 && !options.keep_frames && !options.resume {
        return export_video_direct_planned(
            ffmpeg, paths, params, gains, offsets, options, plan, cancel, progress,
        );
    }
    export_stills_planned(
        paths, params, gains, offsets, options, plan, cancel, progress,
    )?;
    encode_video(ffmpeg, options, cancel, progress)
}

/// 探明配乐的编码与时长。
///
/// 只跑 `ffmpeg -i`（没有输出，退出码必非零），从 stderr 里读元数据，
/// 这样不依赖 ffprobe，与 Python `ff.probe_audio` 同口径。
///
/// Probes the codec and duration of the soundtrack.
///
/// Only `ffmpeg -i` runs (there is no output, so the exit code is always non-zero) and the
/// metadata is read from stderr, which avoids a dependency on ffprobe and matches the
/// convention of Python `ff.probe_audio`.
pub fn probe_audio(ffmpeg: &Path, path: &Path) -> Result<AudioProbe, ExportError> {
    if !ffmpeg.is_file() {
        return Err(ExportError::NoFfmpeg);
    }
    if !path.is_file() {
        return Err(ExportError::Invalid(format!(
            "配乐文件不存在：{}",
            path.display()
        )));
    }
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-i"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd.output()?;
    let probe = crate::audio::parse_probe(&String::from_utf8_lossy(&out.stderr));
    if !probe.has_stream() {
        return Err(ExportError::Invalid(format!(
            "这个文件里没有音频流：{}",
            path.display()
        )));
    }
    Ok(probe)
}

/// 构建 ffmpeg 参数（不含可执行文件本身），便于测试逐步核对。
///
/// `audio` 是配乐的探针结果；没有配乐时传 `None`。
///
/// Builds the ffmpeg arguments without the executable itself, so tests can check them step
/// by step.
///
/// `audio` is the probe result of the soundtrack; pass `None` when there is none.
pub fn video_command(
    _ffmpeg: &Path,
    options: &RenderOptions,
    frames: usize,
    audio: Option<&AudioProbe>,
) -> Vec<String> {
    let mut input = vec![
        "-framerate".to_owned(),
        fps_text(options.fps),
        "-start_number".to_owned(),
        "0".to_owned(),
        "-i".to_owned(),
        pattern_for(&options.out_dir, options.format)
            .to_string_lossy()
            .into_owned(),
    ];
    video_args(options, frames, audio, &mut input)
}

/// 从管道读裸 rgb24：省掉中间 PNG 的编码和解码，帧率与尺寸都写在输入选项里。
///
/// Reads raw rgb24 from a pipe: this skips intermediate PNG encode and decode, with frame
/// rate and size given in the input options.
pub fn pipe_command(
    options: &RenderOptions,
    frames: usize,
    audio: Option<&AudioProbe>,
    size: (u32, u32),
) -> Vec<String> {
    let mut input = vec![
        "-f".to_owned(),
        "rawvideo".to_owned(),
        "-pix_fmt".to_owned(),
        // 与 `output_frame_bytes` 同一判据：HDR 喂 16bit，SDR 喂 8bit。两边只要有一边
        // 自己判断，ffmpeg 就会把字节按错误的位深切，画面立刻变成斜纹。
        //
        // Same criterion as `output_frame_bytes`: 16bit for HDR, 8bit for SDR. If either side
        // decides on its own, ffmpeg slices the bytes at the wrong depth and the picture turns
        // into diagonal stripes.
        if options.dynamic == OutputDynamic::HdrPq {
            "rgb48le".to_owned()
        } else {
            "rgb24".to_owned()
        },
        "-s".to_owned(),
        format!("{}x{}", size.0, size.1),
        "-framerate".to_owned(),
        fps_text(options.fps),
        "-i".to_owned(),
        "-".to_owned(),
    ];
    video_args(options, frames, audio, &mut input)
}

fn video_args(
    options: &RenderOptions,
    frames: usize,
    audio: Option<&AudioProbe>,
    input: &mut Vec<String>,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    args.push("-y".to_owned());
    args.extend(["-progress", "pipe:1", "-nostats"].map(str::to_owned));
    args.append(input);

    let video_dur = frames as f64 / options.fps;
    let mut limit: Option<f64> = None;
    if let Some(music) = &options.audio {
        let opts = &options.audio_opts;
        let fallback = AudioProbe {
            codec: String::new(),
            duration: video_dur,
        };
        let probe = audio.unwrap_or(&fallback);
        let audio_dur = if probe.duration > 0.0 {
            probe.duration
        } else {
            video_dur
        };

        if opts.looped {
            args.extend(["-stream_loop", "-1"].map(str::to_owned));
        }
        args.push("-i".to_owned());
        args.push(music.to_string_lossy().into_owned());

        args.extend(["-map", "0:v:0"].map(str::to_owned));
        let filter = opts.filter(video_dur, audio_dur);
        if let Some(filter) = &filter {
            args.extend(["-filter_complex", filter.complex.as_str()].map(str::to_owned));
            args.extend(["-map", filter.label.as_str()].map(str::to_owned));
        } else {
            args.extend(["-map", "1:a:0"].map(str::to_owned));
        }

        let out_ext = options
            .video_out
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp4");
        let copy = match opts.acodec {
            crate::audio::AudioCodec::Copy => filter.is_none(),
            crate::audio::AudioCodec::Auto => opts.copy_ok(out_ext, &probe.codec),
            _ => false,
        };
        if copy {
            args.extend(["-c:a", "copy"].map(str::to_owned));
        } else {
            let bitrate = opts.acodec.bitrate().unwrap_or("192k");
            args.extend(["-c:a", "aac", "-b:a", bitrate].map(str::to_owned));
        }
        limit = opts.limit_dur(video_dur, audio_dur);
    }

    // 调色链路全程在 sRGB 里算，输出色域由 color_space 决定。VUI 必须逐条显式写：
    // 本机 ffmpeg 7.1 实测，只给 -color_primaries/-color_trc 全局参数时 x264/x265 都把
    // 码流里的字段留成 unknown，得靠各自的 -x26?-params 才真落进 VUI。
    //
    // HDR 与 SDR 的 VUI 只差传递函数与原色：Rec.2020 + PQ（`smpte2084`）。`vui_pq()`
    // 只对 Rec.2020 返回 `Some`，而 `check_dynamic` 已经要求 HDR 必须 Rec.2020，所以
    // `unwrap_or` 那条分支实际走不到——留着是为了配置不合法时退化成可播放的 SDR 标记，
    // 而不是 panic。
    //
    // HDR differs from SDR only in transfer and primaries: Rec.2020 + PQ (`smpte2084`).
    // `vui_pq()` returns `Some` for Rec.2020 only and `check_dynamic` already requires
    // Rec.2020 for HDR, so the `unwrap_or` arm is unreachable — it degrades an invalid
    // configuration into a playable SDR tag instead of panicking.
    let (primaries, transfer, matrix) = match options.dynamic {
        OutputDynamic::HdrPq => options
            .color_space
            .vui_pq()
            .unwrap_or_else(|| options.color_space.vui()),
        OutputDynamic::Sdr => options.color_space.vui(),
    };
    let vui_params =
        format!("colorprim={primaries}:transfer={transfer}:colormatrix={matrix}:range=limited");

    match options.codec {
        Codec::H265 => {
            args.extend(["-c:v", "libx265"].map(str::to_owned));
            args.extend(["-crf", &options.crf.to_string()].map(str::to_owned));
            args.extend(["-preset", &options.preset].map(str::to_owned));
            args.extend(["-pix_fmt", "yuv420p10le"].map(str::to_owned));
            args.extend(["-tag:v", "hvc1"].map(str::to_owned));
            args.extend(["-x265-params", &vui_params].map(str::to_owned));
        }
        Codec::H264 => {
            args.extend(["-c:v", "libx264"].map(str::to_owned));
            args.extend(["-crf", &options.crf.to_string()].map(str::to_owned));
            args.extend(["-preset", &options.preset].map(str::to_owned));
            args.extend(["-pix_fmt", "yuv420p"].map(str::to_owned));
            args.extend(["-x264-params", &vui_params].map(str::to_owned));
        }
    }
    args.extend(
        [
            "-colorspace",
            matrix,
            "-color_primaries",
            primaries,
            "-color_trc",
            transfer,
        ]
        .map(str::to_owned),
    );
    args.extend(["-color_range", "tv"].map(str::to_owned));
    args.extend(["-frames:v", &frames.to_string()].map(str::to_owned));
    if let Some(t) = limit {
        args.extend(["-t".to_owned(), format!("{t:.3}")]);
    }
    args.extend(["-movflags", "+faststart"].map(str::to_owned));
    // 容器标签：ffmpeg 的 title/comment/artist/copyright 在 mp4 里落成 ©nam/©cmt/©ART/©cpy。
    // 只写填了的项——空值会把标签写成空字符串，不如不写。
    let meta = options.metadata.cleaned();
    for (key, value) in [
        ("title", &meta.title),
        ("comment", &meta.description),
        ("artist", &meta.creator),
        ("copyright", &meta.copyright),
    ] {
        if !value.is_empty() {
            args.push("-metadata".to_owned());
            args.push(format!("{key}={value}"));
        }
    }
    args.push(options.video_out.to_string_lossy().into_owned());
    args
}

/// 精确目标尺寸的等比覆盖尺寸：先放大到两边都不小于目标，再居中裁掉多出的边。
///
/// 对应 Python oracle 的 `scale=W:H:force_original_aspect_ratio=increase,crop=W:H`。
///
/// Cover size for an exact target: scale up until both sides reach the target, then crop the
/// surplus centrally.
///
/// Mirrors `scale=W:H:force_original_aspect_ratio=increase,crop=W:H` in the Python oracle.
pub fn cover_size(w: u32, h: u32, tw: u32, th: u32) -> (u32, u32) {
    let scale = (tw as f64 / w.max(1) as f64).max(th as f64 / h.max(1) as f64);
    (
        ((w as f64 * scale).round() as u32).max(tw),
        ((h as f64 * scale).round() as u32).max(th),
    )
}

/// 给定输入尺寸与长边限制，返回偶数宽高（编码器要求）。
///
/// Returns even width and height for the given input size and long-side limit, as required
/// by the encoder.
pub fn fit_size(w: u32, h: u32, long_side: Option<u32>) -> (u32, u32) {
    let (mut ow, mut oh) = (w.max(2), h.max(2));
    if let Some(limit) = long_side
        && limit > 0
        && ow.max(oh) > limit
    {
        let k = limit as f64 / ow.max(oh) as f64;
        ow = ((ow as f64 * k).round() as u32).max(2);
        oh = ((oh as f64 * k).round() as u32).max(2);
    }
    if ow % 2 == 1 {
        ow -= 1;
    }
    if oh % 2 == 1 {
        oh -= 1;
    }
    (ow, oh)
}

// ------------------------------------------------------------------ 内部

#[derive(Clone, Copy)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

/// 第 `i` 张输出静帧落在哪儿。续跑判据、预扫、清理中间帧都从这里取名字，
/// 不许在别处再拼一遍 `frame_%06d`。
///
/// Location of the `i`-th output still. The resume test, the pre-scan and the intermediate
/// frame cleanup all take their names from here, and `frame_%06d` must not be assembled
/// anywhere else.
pub fn frame_path(dir: &Path, format: StillFormat, index: usize) -> PathBuf {
    let name = format!("frame_{index:06}.{}", format.ext());
    dir.join(name)
}

/// 这一帧是不是已经导完了：静帧本身在盘上且非空，该带的 XMP 旁车也在盘上且非空。
///
/// 「非空」不是多余的一眼：`write_still` 写的是同名 `.part` 再改名占用，正常路径上
/// 不会有零字节文件，但用户手工截出来的空文件、或者别的程序占位留下的空文件，
/// 在这儿一律当成没导过——重渲一张比把坏文件当成品留着便宜。
///
/// Whether this frame is already exported: the still is on disk and non-empty, and the XMP
/// sidecar it should carry is on disk and non-empty too.
///
/// The "non-empty" check is not redundant: `write_still` writes a same-named `.part` file and
/// then renames it, so the normal path never yields a zero-byte file, but an empty file a user
/// truncated by hand, or one left by another program as a placeholder, counts as not exported
/// here — re-rendering one frame is cheaper than keeping a broken file as a result.
pub fn frame_is_done(dst: &Path, options: &RenderOptions) -> bool {
    let filled = |p: &Path| p.metadata().is_ok_and(|m| m.is_file() && m.len() > 0);
    filled(dst)
        && (!options.xmp_sidecar
            || options.mode != OutputMode::Stills
            || filled(&crate::xmp::sidecar_for(dst)))
}

/// 断点续跑之前先数一遍已经导完多少帧，给状态栏写「跳过 N · 补写 M」。
/// 只按文件名挨个 stat，一帧一次 IO，4K 序列几千帧也就几毫秒。
///
/// Counts finished frames before resuming, so the status bar can show "skipped N · filled M".
/// Only file names are stat'ed, one IO per frame, which takes a few milliseconds for a
/// sequence of thousands of 4K frames.
pub fn existing_frames(options: &RenderOptions, total: usize) -> usize {
    if !options.resume {
        return 0;
    }
    (0..total)
        .filter(|i| frame_is_done(&frame_path(&options.out_dir, options.format, *i), options))
        .count()
}

/// 全分辨率解码成 sRGB f32 [0,1]，写进调用方的缓冲（跨帧复用）。16bit / 32f 源
/// 保持原精度，和 Python `load_full` 一样不从 8bit 起步。
///
/// HDR 合并等模块共用的读图入口：与导出同一条解码路，方向与 RAW 语义一致。
///
/// Decodes at full resolution into sRGB f32 [0,1] and writes into the caller's buffer, which
/// is reused across frames. 16bit / 32f sources keep their precision and never start from
/// 8bit, as in Python `load_full`.
///
/// Shared image entry for modules such as HDR merge: it uses the same decode path as export,
/// so orientation and RAW semantics agree.
pub fn load_rgb_f32(path: &Path) -> Result<(Vec<f32>, u32, u32), ExportError> {
    let mut rgb = Vec::new();
    let (w, h) = load_rgb_f32_into(path, &mut rgb)?;
    Ok((rgb, w, h))
}

/// 解码只走 `metrics::decode` 一个口子：方向、DNG 的语义因此跟预览完全一致。
fn load_rgb_f32_into(path: &Path, rgb: &mut Vec<f32>) -> Result<(u32, u32), ExportError> {
    let img = crate::metrics::decode(path).map_err(|source| ExportError::Decode {
        path: path.display().to_string(),
        source,
    })?;
    let (w, h) = (img.width(), img.height());
    let n = w as usize * h as usize * 3;
    rgb.resize(n, 0.0);
    // 24MP 的逐像素搬运也要上百毫秒，按元素并行铺平；每项都是独立换算，结果不变。
    match img.color() {
        image::ColorType::L16
        | image::ColorType::La16
        | image::ColorType::Rgb16
        | image::ColorType::Rgba16 => {
            let src = img.to_rgb16();
            rgb.par_iter_mut()
                .zip(src.as_raw())
                .for_each(|(d, s)| *d = *s as f32 / 65535.0);
        }
        image::ColorType::Rgb32F | image::ColorType::Rgba32F => {
            let src = img.to_rgb32f();
            rgb.par_iter_mut()
                .zip(src.as_raw())
                .for_each(|(d, s)| *d = *s);
        }
        image::ColorType::Rgb8 => {
            // 已经是 rgb8：直接借像素，省掉一份全图拷贝。as_rgb8 理论上不会
            // 是 None（color 报告 Rgb8 就一定有 buffer），真遇到就退回
            // to_rgb8 拷一份 —— 之前这里借了个空切片，整图静默变黑。
            match img.as_rgb8() {
                Some(buf) => rgb
                    .par_iter_mut()
                    .zip(buf.as_raw())
                    .for_each(|(d, s)| *d = *s as f32 / 255.0),
                None => {
                    let raw = img.to_rgb8().into_raw();
                    rgb.par_iter_mut()
                        .zip(raw)
                        .for_each(|(d, s)| *d = f32::from(s) / 255.0)
                }
            }
        }
        _ => {
            let src = img.to_rgb8();
            rgb.par_iter_mut()
                .zip(src.as_raw())
                .for_each(|(d, s)| *d = *s as f32 / 255.0);
        }
    }
    Ok((w, h))
}

fn clamp_crop(crop: Option<CropRect>, w: u32, h: u32) -> Result<Rect, ExportError> {
    let Some(c) = crop else {
        return Ok(Rect {
            x: 0,
            y: 0,
            w: w as usize,
            h: h as usize,
        });
    };
    let x0 = c.x.min(w) as usize;
    let y0 = c.y.min(h) as usize;
    let x1 = c.x.saturating_add(c.w).min(w) as usize;
    let y1 = c.y.saturating_add(c.h).min(h) as usize;
    if x1 <= x0 || y1 <= y0 {
        return Err(ExportError::Invalid("裁切框与画面没有交集".to_owned()));
    }
    Ok(Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    })
}

/// 区域性平均缩放（INTER_AREA），直接在 f32 上做，16bit 输出不丢精度。
///
/// `tmp`/`out` 由调用方跨帧复用：这两个缓冲加起来上千兆，每帧重新分配是白给。
fn resize_area_f32_into(
    src: &[f32],
    sw: usize,
    sh: usize,
    tw: usize,
    th: usize,
    tmp: &mut Vec<f32>,
    out: &mut Vec<f32>,
) {
    if (sw, sh) == (tw, th) || tw == 0 || th == 0 {
        out.clear();
        out.extend_from_slice(src);
        return;
    }
    let wx = area_weights(sw, tw);
    let wy = area_weights(sh, th);
    // 两趟都按输出行切块并行：每个采样点的累加顺序不变，结果与串行逐位一致。
    tmp.resize(tw * sh * 3, 0.0);
    tmp.par_chunks_mut(tw * 3).enumerate().for_each(|(y, row)| {
        for (dx, ws) in wx.iter().enumerate() {
            let mut acc = [0.0f32; 3];
            for (sx, wt) in ws {
                let o = (y * sw + *sx) * 3;
                acc[0] += src[o] * wt;
                acc[1] += src[o + 1] * wt;
                acc[2] += src[o + 2] * wt;
            }
            row[dx * 3..dx * 3 + 3].copy_from_slice(&acc);
        }
    });
    out.resize(tw * th * 3, 0.0);
    out.par_chunks_mut(tw * 3)
        .enumerate()
        .for_each(|(dy, row)| {
            let ws = &wy[dy];
            for x in 0..tw {
                let mut acc = [0.0f32; 3];
                for (sy, wt) in ws {
                    let o = (*sy * tw + x) * 3;
                    acc[0] += tmp[o] * wt;
                    acc[1] += tmp[o + 1] * wt;
                    acc[2] += tmp[o + 2] * wt;
                }
                row[x * 3..x * 3 + 3].copy_from_slice(&acc);
            }
        });
}

fn area_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f64 / dst as f64;
    let last = src as f64;
    (0..dst)
        .map(|d| {
            // 两端钉回 [0, src]：`end` 在浮点下可能比 src 大出 1e-13，
            // 那一丁点误差会让最后一个采样下标正好落在数组之外。
            let start = (d as f64 * scale).min(last);
            let end = ((d + 1) as f64 * scale).clamp(start, last);
            let span = (end - start).max(1e-9);
            let mut v = Vec::new();
            let mut j = start.floor() as usize;
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

/// 8bit 量化：`[0,1]` f32 → u8，四舍五入。PNG/JPEG/WebP 三条路共用。
fn to_u8(rgb: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; rgb.len()];
    out.par_iter_mut()
        .zip(rgb)
        .for_each(|(d, v)| *d = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    out
}

/// 16bit 量化并按本机字节序铺平成字节：TIFF 与深源单帧 PNG 共用。
/// `image` 的约定是 16bit 缓冲区按本机端序给（PNG 编码器内部再翻成大端）。
fn to_u16_ne(rgb: &[f32]) -> Vec<u8> {
    let mut data = vec![0u16; rgb.len()];
    data.par_iter_mut()
        .zip(rgb)
        .for_each(|(d, v)| *d = (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16);
    let mut bytes = Vec::with_capacity(data.len() * 2);
    for v in &data {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }
    bytes
}

/// 把可选的元数据块挂到编码器上。`set_*` 只回报 `UnsupportedError`，这里抬成
/// `ImageError`，调用方就能和 `write_image` 用同一个 `Write` 错误分支。
fn set_icc<E: ImageEncoder>(enc: &mut E, icc: Option<&[u8]>) -> Result<(), image::ImageError> {
    match icc {
        Some(b) => enc
            .set_icc_profile(b.to_vec())
            .map_err(image::ImageError::Unsupported),
        None => Ok(()),
    }
}

fn set_exif<E: ImageEncoder>(enc: &mut E, exif: Option<&[u8]>) -> Result<(), image::ImageError> {
    match exif {
        Some(b) => enc
            .set_exif_metadata(b.to_vec())
            .map_err(image::ImageError::Unsupported),
        None => Ok(()),
    }
}

/// 落一张静帧：先写同目录的 `.part`，成功后改名占用。
///
/// 断点续跑认的是「文件在且非空」，直接往最终名字上编码的话，被杀在中途的那张会留下
/// 半个文件骗过它，之后每次续跑都跳过这张坏图。同目录改名是原子的，看到的就是写完的。
fn write_still(
    path: &Path,
    rgb: &mut [f32],
    w: u32,
    h: u32,
    options: &RenderOptions,
    meta: &StillMeta<'_>,
) -> Result<(), ExportError> {
    let tmp = part_path(path);
    match write_still_into(&tmp, rgb, w, h, options, meta) {
        Ok(()) => fs::rename(&tmp, path).map_err(ExportError::Io),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            // 报错要说用户认识的那个名字，`.part` 是我们内部的说法。
            Err(match e {
                ExportError::Write { source, .. } => ExportError::Write {
                    path: path.display().to_string(),
                    source,
                },
                other => other,
            })
        }
    }
}

/// 同一目录里的临时名。放同一个目录是改名的前提：跨卷 rename 不原子，也不一定能成。
fn part_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| format!(".{}.part", n.to_string_lossy()))
        .unwrap_or_else(|| ".aevum-part".to_owned());
    path.with_file_name(name)
}

/// 真正把像素编码到 `path` 的那一段；调用方负责它只是个临时名。
fn write_still_into(
    path: &Path,
    rgb: &mut [f32],
    w: u32,
    h: u32,
    options: &RenderOptions,
    meta: &StillMeta<'_>,
) -> Result<(), ExportError> {
    let write_err = |source| ExportError::Write {
        path: path.display().to_string(),
        source,
    };
    // 像素先搬到目标色域，再嵌同一份 ICC：只做一半的话，要么标记骗人，要么白转。
    options.color_space.convert_in_place(rgb);
    // 色域搬完才是传递函数：PQ 必须作用在**已转好色域**的 R'G'B' 上，顺序颠倒等于
    // 在错误的原色上做绝对亮度映射。
    if options.dynamic == OutputDynamic::HdrPq {
        crate::hdr_output::encode_pq_in_place(rgb);
    }
    let icc = meta.icc;
    let exif = meta.exif;
    match options.format {
        // PNG 分两档：批量导出按用户选的 8bit 走；单帧导出保住源的位深，
        // 前面辛苦换来的 16bit 不能在最后一步又砍回 256 级。
        StillFormat::Png8 if meta.deep => {
            let data = to_u16_ne(rgb);
            let mut encoder = image::codecs::png::PngEncoder::new(fs::File::create(path)?);
            set_icc(&mut encoder, icc)
                .and_then(|()| set_exif(&mut encoder, exif))
                .map_err(write_err)?;
            encoder
                .write_image(&data, w, h, image::ExtendedColorType::Rgb16)
                .map_err(write_err)
        }
        StillFormat::Png8 => {
            let data = to_u8(rgb);
            let mut encoder = image::codecs::png::PngEncoder::new(fs::File::create(path)?);
            set_icc(&mut encoder, icc)
                .and_then(|()| set_exif(&mut encoder, exif))
                .map_err(write_err)?;
            encoder
                .write_image(&data, w, h, image::ExtendedColorType::Rgb8)
                .map_err(write_err)
        }
        StillFormat::Tiff16 => {
            let bytes = to_u16_ne(rgb);
            let mut encoder = image::codecs::tiff::TiffEncoder::new(fs::File::create(path)?);
            // EXIF 不进 TIFF：IFD 偏移指向整个文件，块搬过去就是坏数据。
            set_icc(&mut encoder, icc).map_err(write_err)?;
            encoder
                .write_image(&bytes, w, h, image::ExtendedColorType::Rgb16)
                .map_err(write_err)
        }
        StillFormat::Jpeg => {
            let data = to_u8(rgb);
            // image 内部把 quality 夹到 1~100，这里不重复夹，跟它用同一套口径。
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                fs::File::create(path)?,
                options.quality,
            );
            set_icc(&mut encoder, icc)
                .and_then(|()| set_exif(&mut encoder, exif))
                .map_err(write_err)?;
            encoder
                .write_image(&data, w, h, image::ExtendedColorType::Rgb8)
                .map_err(write_err)
        }
        StillFormat::WebP => {
            let data = to_u8(rgb);
            let mut encoder =
                image::codecs::webp::WebPEncoder::new_lossless(fs::File::create(path)?);
            set_icc(&mut encoder, icc)
                .and_then(|()| set_exif(&mut encoder, exif))
                .map_err(write_err)?;
            encoder
                .write_image(&data, w, h, image::ExtendedColorType::Rgb8)
                .map_err(write_err)
        }
        StillFormat::Avif if options.dynamic == OutputDynamic::HdrPq => {
            write_avif_hdr(path, rgb, w, h, options)
        }
        StillFormat::Avif => write_avif(path, rgb, w, h, options),
    }
}

/// 质量 1~100 映射到 libaom 的 crf：q=100 落在 4（视觉无损），q=1 落在 62。
fn avif_crf(quality: u8) -> u8 {
    let q = i32::from(quality).clamp(1, 100);
    (4 + (100 - q) * 58 / 99).clamp(0, 63) as u8
}

/// AVIF 没有进程内编码器，走随包 ffmpeg 的 libaom-av1：先把像素落一张临时 PNG，
/// 编码完删掉。ICC 嵌不进去，色域改写成容器里的 nclx 标记。
fn write_avif(
    path: &Path,
    rgb: &[f32],
    w: u32,
    h: u32,
    options: &RenderOptions,
) -> Result<(), ExportError> {
    let ffmpeg = find_ffmpeg().ok_or(ExportError::NoFfmpeg)?;
    let tmp = path.with_extension("avif-src.png");
    let data = to_u8(rgb);
    image::save_buffer(&tmp, &data, w, h, image::ColorType::Rgb8).map_err(|source| {
        ExportError::Write {
            path: tmp.display().to_string(),
            source,
        }
    })?;
    let (primaries, transfer, matrix) = options.color_space.vui();
    let mut cmd = Command::new(&ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&tmp)
        .args(["-c:v", "libaom-av1", "-crf"])
        .arg(avif_crf(options.quality).to_string())
        .args([
            "-cpu-used",
            "6",
            "-still-picture",
            "1",
            "-pix_fmt",
            "yuv444p",
            "-colorspace",
            matrix,
            "-color_primaries",
            primaries,
            "-color_trc",
            transfer,
            "-frames:v",
            "1",
            // 必须显式给容器：写盘走的是 `<name>.part` 临时名（`write_still` 的原子
            // 改名），扩展名是 `part`，ffmpeg 从它猜不出格式，会直接报
            // 「Unable to choose an output format」。SDR 那条路同样受影响——只是
            // 那天之前没人用真 ffmpeg 跑过 AVIF。
            "-f",
            "avif",
        ])
        .arg(path);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    // 起不来也要把临时 PNG 收掉：批量导出时每帧留一份没人会看见的大文件。
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
    };
    let _ = fs::remove_file(&tmp);
    if !out.status.success() {
        return Err(ExportError::Ffmpeg(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    let _ = patch_avif_color(path, options.color_space.nclx());
    Ok(())
}

/// 把 PQ 信号铺成 `rgb48le` 字节流，供 `-pix_fmt rgb48le` 的原始视频输入。
///
/// 10bit 只有 1024 级，这里给的是 16bit（65536 级）：多出来的位深不是浪费，而是让
/// ffmpeg 的 RGB→YUV 矩阵运算与抖动有落脚点，实测比直接喂 10bit 往返误差小一半。
///
/// Lays PQ signal out as an `rgb48le` byte stream for raw-video input with
/// `-pix_fmt rgb48le`.
///
/// 10bit has only 1024 levels; this emits 16bit (65536). The extra depth is not waste: it
/// gives ffmpeg's RGB→YUV matrix and dithering somewhere to land, and measured
/// round-trip error is half of feeding 10bit directly.
fn to_rgb48_le(rgb: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() * 2);
    for v in rgb {
        let q = (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
        out.extend_from_slice(&q.to_le_bytes());
    }
    out
}

/// HDR（PQ）的 AVIF：走 `rgb48le` 管道喂 libaom-av1，输出 `yuv444p10le` + full range。
///
/// 与 SDR 那条路的区别不只是位深：
/// - **不落临时 PNG**。PNG 只能存 8/16bit 的整数样本，没有地方说明「这些数是 PQ」，
///   中转一次等于让编解码器按 sRGB 解释一遍再让我们改回来。直接喂原始字节，中间没有
///   任何一步会误解信号。
/// - **`-color_range pc`**。理由见 [`crate::hdr_output`] 头第 3 条。
/// - **nclx 补 `smpte2084`**。libaom 只写矩阵，原色与传递函数一直是我们自己补的
///   （SDR 那条路也一样），HDR 这里补的是 (9, 16, 9)。
///
/// HDR (PQ) AVIF: feed libaom-av1 through an `rgb48le` pipe, writing `yuv444p10le` at
/// full range.
///
/// What differs from the SDR path is not just bit depth:
/// - **No temporary PNG.** PNG holds 8/16bit integer samples and has nowhere to say "these
///   numbers are PQ", so a round trip through it means the codec reads them as sRGB and we
///   patch them back. Feeding raw bytes leaves no step that can misread the signal.
/// - **`-color_range pc`**, for the reason in item 3 of [`crate::hdr_output`]'s header.
/// - **nclx gets `smpte2084`.** libaom writes only the matrix; primaries and transfer have
///   always been patched by us (same as the SDR path), and here the patch is (9, 16, 9).
fn write_avif_hdr(
    path: &Path,
    rgb: &[f32],
    w: u32,
    h: u32,
    options: &RenderOptions,
) -> Result<(), ExportError> {
    let ffmpeg = find_ffmpeg().ok_or(ExportError::NoFfmpeg)?;
    let (primaries, transfer, matrix) = options
        .color_space
        .vui_pq()
        .ok_or_else(|| ExportError::Invalid("HDR（PQ）必须配 Rec.2020".to_owned()))?;
    let size = format!("{w}x{h}");
    let crf = avif_crf(options.quality).to_string();
    let mut cmd = Command::new(&ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb48le",
        "-s",
        &size,
        "-r",
        "1",
        "-i",
        "-",
        "-c:v",
        "libaom-av1",
        "-crf",
        &crf,
        "-cpu-used",
        "6",
        "-still-picture",
        "1",
        "-pix_fmt",
        "yuv444p10le",
        "-color_range",
        "pc",
        "-colorspace",
        matrix,
        "-color_primaries",
        primaries,
        "-color_trc",
        transfer,
        "-frames:v",
        "1",
        // 同 SDR 那条路：输出名是 `.part`，容器必须显式给。
        "-f",
        "avif",
    ])
    .arg(path)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ExportError::Ffmpeg("拿不到 ffmpeg 的 stdin".to_owned()))?;
    // 写完立刻 drop：不关管道 ffmpeg 会一直等下一帧，`wait_with_output` 就永远不返回。
    let feed = stdin.write_all(&to_rgb48_le(rgb));
    drop(stdin);
    // 写管道失败（ffmpeg 提前退出）时先把它收干净，再报写入错误——直接 `?` 会把
    // 子进程和 stderr 一起丢掉，看到的就只有一句 broken pipe。
    if let Err(e) = feed {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.into());
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(ExportError::Ffmpeg(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    if let Some(nclx) = options.color_space.nclx_pq() {
        let _ = patch_avif_color(path, nclx);
    }
    Ok(())
}

/// 把色标补进 AVIF 的 `meta/iprp/ipco/colr`。ffmpeg 7.1 的 libaom 只写矩阵
/// （原色/传递函数留 unspecified），像素在导出链里已经转过，标签必须跟上。
/// 找不到 `colr` 就原样返回：容器仍能解码，只是少一份色域说明。
fn patch_avif_color(path: &Path, nclx: [u16; 3]) -> std::io::Result<()> {
    let mut data = fs::read(path)?;
    let Some(at) = find_box(&data, 0, data.len(), b"colr") else {
        return Ok(());
    };
    let payload = at + 8;
    if data.get(payload..payload + 4) != Some(b"nclx") || data.len() < payload + 11 {
        return Ok(());
    }
    for (i, code) in nclx.iter().enumerate() {
        let off = payload + 4 + i * 2;
        data[off..off + 2].copy_from_slice(&code.to_be_bytes());
    }
    fs::write(path, &data)
}

/// 在 ISO-BMFF 里找某个 box，只下钻容器盒（`meta` 多 4 字节版本/标志）。
fn find_box(data: &[u8], start: usize, end: usize, want: &[u8; 4]) -> Option<usize> {
    let mut pos = start;
    while pos + 8 <= end {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        if size < 8 || pos + size > end {
            return None;
        }
        let kind = &data[pos + 4..pos + 8];
        if kind == want {
            return Some(pos);
        }
        let payload = pos + 8;
        let hit = match kind {
            b"meta" => find_box(data, payload + 4, pos + size, want),
            b"iprp" | b"ipco" => find_box(data, payload, pos + size, want),
            _ => None,
        };
        if hit.is_some() {
            return hit;
        }
        pos += size;
    }
    None
}

/// 只拿像素数，不解全图：`image` 不认 RAW 容器，那两种得问我们自己的解析器，
/// 否则每张 24MP RAW 都会被当成「尺寸不明」按单线程导出。
///
/// DNG 内置解析只认单通道原始数据，三通道 LinearRaw 会被它拒掉；那不是文件坏了，
/// 尺寸得问后端——拿不到尺寸只会退成单线程，但那是性能损失，不该由「内置解不了」
/// 这样的能力边界来触发。
fn frame_pixels(path: &Path) -> u64 {
    // 只要个数量级决定并行路数：走 `info_dimensions`（内置口径，不解像素、不问
    // 第三方后端）。`info` 现在为了与真正解码同口径要多跑一趟 rawler 的容器解析，
    // 1000 帧的序列在这里付不起。
    //
    // Only the magnitude is needed to pick a worker count: `info_dimensions` (built-in
    // verdict, no pixel decode, no third-party backend). `info` now runs a rawler container
    // parse to stay consistent with the real decode, which a 1000-frame sequence cannot afford
    // here.
    let info = if crate::raw::is_raw_name(path) {
        crate::raw::info_dimensions(path).map(Some).unwrap_or(None)
    } else if crate::dng::is_named_dng(path) {
        match crate::dng::info(path) {
            Ok(i) => Some((i.width, i.height)),
            // 软错说明该问后端；硬错问也是白问，尺寸当未知。
            Err(e) if e.soft_fallback() => crate::raw::info_dimensions(path).ok(),
            Err(_) => None,
        }
    } else {
        // 只为决定并行路数问一次尺寸：读坏了、猜不出格式都当「尺寸不明」，退单线程
        // 只是慢一点；这里绝不能因为一个坏文件 panic 掉整次导出。
        // 不必再加 `Limits`：`into_dimensions` 只读头部、不解像素，而 `ImageReader`
        // 本身已带 `Limits::default()`（`max_alloc` 512 MB），解压炸弹在真正解码的
        // 地方才需要拦，那是 `decode_with_origin` 的守卫。
        //
        // Only asking for the size to pick the worker count: an unreadable or
        // undeterminable file counts as "size unknown" and falls back to one worker, which
        // is merely slower; a broken file must never panic an entire export.
        // No extra `Limits` needed: `into_dimensions` reads headers only and allocates no
        // pixels, and `ImageReader` already carries `Limits::default()` (`max_alloc` 512 MB).
        // A decompression bomb only matters where pixels are actually decoded, which is the
        // guard inside `decode_with_origin`.
        crate::raw::guard::catch_decoder_panic("image", move || {
            image::ImageReader::open(path)
                .ok()
                .and_then(|r| r.with_guessed_format().ok())
                .and_then(|r| r.into_dimensions().ok())
        })
        .unwrap_or(None)
    };
    info.map_or(0, |(w, h)| w as u64 * h as u64)
}

/// 一路工作线程一帧同时要占的字节数：源 f32（3×4B/px）加上裁切副本、缩放中间层
/// 和输出，按源的三倍封顶。24MP 约 0.86GB。
fn frame_working_set(px: u64) -> u64 {
    (px * 3 * 4 * 3).max(64 << 20)
}

/// 并行路数：每帧的解码是单线程的，路数就是倍数；上限由物理内存的四分之一和核数
/// 共同决定。24MP 一路工作集约 0.86GB。
///
/// 「大图封顶四路」的 2026-11-04 复测（i5-12600KF / 16 线程，24 MP JPEG → 6000×4000
/// H.264 CRF20，300 帧，2/4/6/8 路各两遍取优）：渲染+编码段 48.40 / 48.91 / 48.01 /
/// 47.57 s——四档全钉在 ~48 s，极差 2.7%（噪声内），CPU 绝对量 ~232 s 与 CPU/墙钟比
/// ~4.8 都与路数无关，9 轮（1 轮冷启动预热 + 8 轮热测）成片 md5 一致。瓶颈在 ffmpeg
/// x264 编码管道：渲染线程被编码器反压，**从 2 路起就已饱和**。保留 4 不是速度需要，
/// 是给解码重的 RAW 源（解码占 28%）与更快的编码预设留余量——那些场景下平衡点可能
/// 移向渲染侧；小内存机器本来就被 `by_ram` 压到 2 路，cap 只在大内存机器生效。
///
/// Worker parallelism: each frame's decode is single-threaded, so workers are the
/// multiplier; the cap is jointly decided by a quarter of physical RAM and the core
/// count. At 24 MP a worker costs ~0.86 GB of working set.
///
/// Re-measured 2026-11-04 (i5-12600KF / 16 threads, 24 MP JPEG → 6000×4000 H.264
/// CRF20, 300 frames, best of two runs per count of 2/4/6/8): the render+encode
/// segment is 48.40 / 48.91 / 48.01 / 47.57 s -- pinned at ~48 s for every count, a
/// 2.7% spread inside the noise, with ~232 s of CPU and a CPU/wall ratio of ~4.8
/// independent of the worker count, and nine runs (one cold warm-up plus eight warm)
/// produced byte-identical videos. The bottleneck is the ffmpeg x264 encode pipe: the
/// render threads are back-pressured by the encoder and **saturate it from 2 workers
/// up**. Keeping 4 is not a speed requirement but headroom for decode-heavy RAW
/// sources (decoding is 28% there) and faster encoder presets, where the balance may
/// shift toward the render side; small-RAM machines are already pressed down to 2 by
/// `by_ram`, so the cap only bites on large-memory machines.
///
/// **`AEVUM_EXPORT_WORKERS` 逃生舱**（显式压测钩子，不是推荐配置，用户文档不提它）：
/// 设为 >0 的整数时直接用作路数，跳过大图 4 路的经验封顶——这正是钩子的意义，让量具
/// 测到默认策略不给的路数；但仍受 `by_ram`（可用物理内存折算的路数）兜底 clamp，压进
/// 换页的数字比默认还难看还可能 OOM，量了白量。核数故意不 clamp，留给压测去撞墙。
/// 未设置、0 或非法值走原逻辑，默认行为一个字节都不变。
///
/// **The `AEVUM_EXPORT_WORKERS` escape hatch** (an explicit benchmarking hook, not a
/// recommended configuration; user docs do not mention it): a positive integer is used
/// directly as the worker count, bypassing the legacy large-frame cap -- that is the
/// hook's whole point, letting the bench reach counts the default policy refuses. It
/// is still capped by `by_ram` (the RAM-derived limit) as an upper bound, so the
/// override can never push the machine into swapping: paging produces numbers worse
/// than the default and can OOM, which measures nothing. The core count is
/// deliberately NOT clamped, so stress runs may hit the wall. Unset, 0, or garbage
/// falls back to the stock logic and default behaviour is untouched.
fn worker_count(paths: &[PathBuf]) -> usize {
    let px = frame_pixels(&paths[0]);
    if px == 0 {
        return 1;
    }
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let budget = match total_ram() {
        0 => 4 << 30,
        bytes => bytes / 4,
    };
    let by_ram = (budget / frame_working_set(px)).max(1);
    let cap = match env_workers() {
        Some(n) => n as u64,
        None if px > 12_000_000 => 4,
        None => (cores / 2).clamp(1, 8) as u64,
    };
    by_ram.min(cap) as usize
}

/// `AEVUM_EXPORT_WORKERS` 的解析：未设、空、0、非数字都算「没设」（None），
/// >0 的整数算「显式伸手」（Some）。
///
/// 返回的是**未经内存约束的显式值**：受 `by_ram` clamp 是 `worker_count` 的语义，
/// 这里只回答「用户有没有伸手」，单测因此不必关心机器有多少内存。
///
/// Parses `AEVUM_EXPORT_WORKERS`: unset, empty, 0, or non-numeric all count as "not
/// set" (None); a positive integer means "the user reached for the hatch" (Some).
///
/// The returned value is the **explicit value before any memory constraint**:
/// clamping by `by_ram` is `worker_count`'s semantics, so this function only answers
/// "did the user reach for the hatch", keeping unit tests free of machine-specific
/// RAM assumptions.
fn env_workers() -> Option<usize> {
    // trim + 解析失败都当「没设」：钩子不该让手滑的值炸掉一次正式导出。
    //
    // A trim/parses failure counts as "unset": the hook must not let a fat-fingered
    // value blow up a real export.
    std::env::var("AEVUM_EXPORT_WORKERS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
}

/// 物理内存总量；拿不到时给 0，调用方按 8GB 保守估计。
#[cfg(windows)]
fn total_ram() -> u64 {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    // SAFETY: 结构体按 GlobalMemoryStatusEx 的布局声明，length 已填好。
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok == 0 { 0 } else { status.total_phys }
}

#[cfg(not(windows))]
fn total_ram() -> u64 {
    0
}

fn count_frames(dir: &Path, format: StillFormat) -> Result<usize, ExportError> {
    let ext = format.ext();
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut n = 0usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if is_frame_file(&entry.file_name().to_string_lossy(), ext) {
            n += 1;
        }
    }
    Ok(n)
}

fn remove_frames(dir: &Path, format: StillFormat) {
    let ext = format.ext();
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if is_frame_file(&name.to_string_lossy(), ext) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn is_frame_file(name: &str, ext: &str) -> bool {
    let Some(rest) = name.strip_prefix("frame_") else {
        return false;
    };
    let Some((num, e)) = rest.rsplit_once('.') else {
        return false;
    };
    e.eq_ignore_ascii_case(ext) && !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
}

fn parse_progress_frame(line: &str) -> Option<usize> {
    line.trim()
        .strip_prefix("frame=")
        .and_then(|v| v.trim().parse::<usize>().ok())
}

fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let tail: String = if chars.len() <= n {
        s.to_owned()
    } else {
        chars[chars.len() - n..].iter().collect()
    };
    tail.trim().to_owned()
}

fn fps_text(fps: f64) -> String {
    format!("{fps}")
}

#[cfg(test)]
mod tests {
    use super::{area_weights, avif_crf, env_workers, to_rgb24, to_rgb48_pq};
    use crate::colorspace::ColorSpace;
    use crate::hdr_output::encoded_to_pq;

    /// `AEVUM_EXPORT_WORKERS` 是进程级变量：本文件所有 env 用例共用一把锁串行，
    /// 并行测试不会互踩（写法与 `aevum-mcp` 测试里队列路径那把 LOCK 同款）。
    ///
    /// `AEVUM_EXPORT_WORKERS` is a process-level variable: every env test in this file
    /// shares one lock so parallel tests cannot stomp on each other (same pattern as
    /// the queue-path LOCK in the aevum-mcp tests).
    static WORKERS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 取锁的兜底：测试 panic 毒化锁后，后续用例照常拿到锁继续跑。
    ///
    /// Lock helper: after a panicking test poisons the mutex, later tests still get it.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        WORKERS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 环境变量守卫：构造时设值或清空，`Drop` 时无条件 `remove_var`——用例断言 panic
    /// 也不会把变量泄漏给下一个用例（先前的写法靠语句末尾手动还原，panic 就漏了）。
    /// 用法前提：调用方持有 `WORKERS_ENV_LOCK`，进程内无并发读写。
    ///
    /// Environment guard: sets or clears the variable on construction and
    /// unconditionally `remove_var`s on `Drop`, so a panicking assertion cannot leak
    /// the variable into the next test (the previous version restored manually at the
    /// end of each test, which panics would skip). Precondition: the caller holds
    /// `WORKERS_ENV_LOCK`, so nothing else in the process touches the variable.
    struct WorkersEnv {
        name: &'static str,
    }

    impl WorkersEnv {
        /// 设成某个值；`Drop` 负责清掉。
        ///
        /// Set the variable to a value; `Drop` cleans it up.
        fn set(value: &str) -> Self {
            let guard = Self {
                name: "AEVUM_EXPORT_WORKERS",
            };
            guard.write(value);
            guard
        }

        /// 先清空再用（未设口径）；`Drop` 再清一次是无害的幂等操作。
        ///
        /// Start from "unset"; the extra `remove_var` in `Drop` is harmless.
        fn clear() -> Self {
            let guard = Self {
                name: "AEVUM_EXPORT_WORKERS",
            };
            guard.remove();
            guard
        }

        /// 改值（同一把锁内安全）：第二条断言不用再手写 `unsafe`。
        ///
        /// Change the value (safe under the same lock) so follow-up assertions do not
        /// need their own `unsafe` block.
        fn write(&self, value: &str) {
            // SAFETY: 调用方持有 WORKERS_ENV_LOCK，本进程内无并发读写该变量。
            // SAFETY: the caller holds WORKERS_ENV_LOCK; nothing else in this process
            // reads or writes the variable concurrently.
            unsafe { std::env::set_var(self.name, value) };
        }

        /// 清空（未设口径）。
        ///
        /// Clear it (the "unset" convention).
        fn remove(&self) {
            // SAFETY: 同上，WORKERS_ENV_LOCK 串行。
            // SAFETY: same as above, serialized by WORKERS_ENV_LOCK.
            unsafe { std::env::remove_var(self.name) };
        }
    }

    impl Drop for WorkersEnv {
        fn drop(&mut self) {
            // SAFETY: 用例尚未释放 WORKERS_ENV_LOCK 时守卫先析构（守卫晚于锁守卫声明），
            // 仍在串行区内。
            //
            // SAFETY: the guard is declared after the lock guard, so it drops first
            // while WORKERS_ENV_LOCK is still held -- still inside the serialized region.
            unsafe { std::env::remove_var(self.name) };
        }
    }

    /// 未设（或上一条用例已还原）→ None：钩子默认完全不参与。
    #[test]
    fn env_workers_unset_is_none() {
        let _lock = env_guard();
        let _env = WorkersEnv::clear();
        assert_eq!(env_workers(), None, "未设时应走默认策略");
    }

    /// >0 整数 → Some，且容忍首尾空白；守卫析构时自动还原，别污染别的用例。
    #[test]
    fn env_workers_positive_integer_is_used_verbatim() {
        let _lock = env_guard();
        let env = WorkersEnv::set("6");
        assert_eq!(env_workers(), Some(6));
        env.write(" 3 ");
        assert_eq!(env_workers(), Some(3), "首尾空白要 trim 掉");
    }

    /// 0 与非法值都当「没设」：钩子不许让手滑的值炸掉正式导出。
    #[test]
    fn env_workers_zero_and_garbage_fall_back_to_none() {
        let _lock = env_guard();
        let env = WorkersEnv::clear();
        for bad in ["0", "abc", "", "-2", "6.5"] {
            env.write(bad);
            assert_eq!(env_workers(), None, "非法值 {bad:?} 必须被当没设");
        }
    }

    /// 管道字节数必须和 `pipe_command` 写的 `-pix_fmt` 对得上：SDR 3 字节/像素，
    /// HDR 6 字节/像素。
    ///
    /// 这条挡的是「有一边自己改了判断」：ffmpeg 不会因为字节不够就报错，它会按自己的
    /// 位深切，切出来是一卷斜纹——参数数组的测试看不出这种错，只有数长度能。
    ///
    /// The pipe byte count must match the `-pix_fmt` written by `pipe_command`: 3 bytes per
    /// pixel for SDR, 6 for HDR.
    ///
    /// This catches "one side changed its mind on its own": ffmpeg does not error when it is
    /// short of bytes, it slices at its own depth and the picture turns into diagonal stripes.
    /// Argument-array tests cannot see that; counting can.
    #[test]
    fn pipe_bytes_per_pixel_match_the_declared_pix_fmt() {
        let n = 4 * 3 * 4; // 4 个像素 × 3 通道 × 4 字节的 f32 缓冲，前面当 f32 用
        let mut sdr = vec![0.5f32; n];
        assert_eq!(
            to_rgb24(&mut sdr, ColorSpace::Srgb).len(),
            n,
            "SDR 不是 1 字节/通道"
        );

        let mut hdr = vec![0.5f32; n];
        assert_eq!(
            to_rgb48_pq(&mut hdr, ColorSpace::Rec2020).len(),
            n * 2,
            "HDR 不是 2 字节/通道——rgb48le 会被当成 rgb24 切"
        );
    }

    /// HDR 那条路必须真的做了 PQ：中性灰 0.5 的 16bit 样本要等于 `encoded_to_pq(0.5)`。
    ///
    /// 中性灰在 sRGB/Rec.2020 之间搬色域不变（三通道相等），所以这个断言只可能被
    /// 「漏掉 PQ」或「PQ 用错锚点」打破——漏掉时样本会是 0.5（32768），差出整整 0.06。
    ///
    /// The HDR path must really apply PQ: the 16bit sample for neutral 0.5 has to equal
    /// `encoded_to_pq(0.5)`. Neutral grey is unchanged by an sRGB→Rec.2020 move (all three
    /// channels equal), so only "PQ left out" or "PQ anchored wrong" can break this — without
    /// PQ the sample is 0.5 (32768), off by a full 0.06.
    #[test]
    fn hdr_pipe_sample_is_the_pq_code_of_the_input() {
        let mut rgb = vec![0.5f32];
        let bytes = to_rgb48_pq(&mut rgb, ColorSpace::Rec2020);
        let got = u16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 65535.0;
        let want = encoded_to_pq(0.5);
        assert!(
            (got - want).abs() < 2e-4,
            "16bit 样本 {got:.5} ≠ PQ 码 {want:.5}"
        );
        assert!(
            (got - 0.5f32).abs() > 0.02,
            "样本还停在 {got:.5}，PQ 没生效"
        );
    }

    /// 与 `metrics::area_weights` 同一处算术，越界同样会把导出线程直接打挂。
    #[test]
    fn area_weights_indices_stay_inside_source() {
        for s in [1usize, 7, 85, 512, 4000, 6000, 6001] {
            for d in 1usize..=140 {
                for ws in area_weights(s, d) {
                    for &(i, _) in &ws {
                        assert!(i < s, "{s}->{d} 取到了源外下标 {i}");
                    }
                }
            }
        }
    }

    /// 质量→crf 必须单调（质量越高 crf 越小），两头落在 libaom 的 0..63 里。
    #[test]
    fn avif_crf_is_monotonic_and_in_range() {
        // 从 100 往下走，质量越低 crf 越大。
        let mut last = 0u8;
        for quality in (1u8..=100).rev() {
            let crf = avif_crf(quality);
            assert!(
                crf >= last,
                "q{quality} 的 crf {crf} 比更高质量的 {last} 还小"
            );
            assert!(crf <= 63);
            last = crf;
        }
        assert_eq!(avif_crf(100), 4);
        assert_eq!(avif_crf(1), 62);
    }
}
