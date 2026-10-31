//! 成片导出：逐帧调色 → 拉直/对齐位移/裁切 → 缩放 → PNG/TIFF 序列 → ffmpeg 编码。
//!
//! 像素口径与预览共用 `preview::render`：空间项与局部蒙版都跟着 `GradeParams`
//! 走，导出这条路不再少算哪一项；默认参数下与 Python oracle 逐像素一致。
//! 几何那一段（旋转 + 位移 + 取景）在 `warp` 里，预览走的是同一个函数。
//! 编码走外部 ffmpeg：H.264 8bit / H.265 10bit。输出色域由 `RenderOptions::color_space`
//! 决定——静态图嵌 ICC，视频把原色/传递函数/矩阵三条一起写进 VUI。

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
use thiserror::Error;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StillFormat {
    Png8,
    Tiff16,
    /// 8bit 有损；压缩率由 `RenderOptions::quality` 定（1~100）。
    Jpeg,
    /// 8bit 无损（`image` 的 WebP 编码器只有 VP8L，没有有损档，所以质量对它无效）。
    WebP,
    /// 8bit 有损（AV1 静帧），走外部 ffmpeg 的 libaom-av1；质量映射到 crf。
    /// 色域用容器里的 nclx 标记，不嵌 ICC 块，也不带 EXIF。
    Avif,
}

impl StillFormat {
    pub const ALL: [Self; 5] = [Self::Png8, Self::Tiff16, Self::Jpeg, Self::WebP, Self::Avif];

    /// 面板/工程/命令行里的稳定拼写。
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
            Self::Avif => "AV1 有损，同画质比 JPEG 小一半；色域写 nclx 标记，不带 ICC/EXIF",
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
    pub fn frames_ok(self) -> bool {
        matches!(self, Self::Png8 | Self::Tiff16)
    }

    /// 质量滑块有没有意义。
    pub fn lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::Avif)
    }

    /// 容器能不能带 EXIF 块。TIFF 的 IFD 偏移指向整个文件，搬不进去；
    /// AVIF 的 mov muxer 不写 EXIF item。
    pub fn supports_exif(self) -> bool {
        !matches!(self, Self::Tiff16 | Self::Avif)
    }
}

/// 输出锐化的用途档：对标 LR 的「按输出用途锐化」，作用在缩放之后的那帧上。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputSharpen {
    /// 默认不锐化：导出像素与预览、与 Python oracle 保持一致。
    #[default]
    Off,
    /// 屏幕：像素 1:1 显示，只需要补缩放丢掉的一点边缘。
    Screen,
    /// 光面打印：相纸本身对比高，轻微回补。
    Glossy,
    /// 哑面打印：染料在涂层里扩散，最需要补偿的就是这一档。
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
    /// [`SharpenStrength`] 只乘强度。半径/阈值随用途内置、UI 不暴露（E2 口径），
    /// 数值沿旧 profile 量级重标定，全部 TODO(fit)。
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputMode {
    /// 只写 PNG/TIFF 序列。
    Stills,
    /// 写序列后编码成视频，成功后按 `keep_frames` 决定是否清掉中间帧。
    Video,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderOptions {
    pub mode: OutputMode,
    pub format: StillFormat,
    /// 有损容器的压缩质量（1~100）；无损档忽略。
    pub quality: u8,
    /// 把源 JPEG 的 EXIF 块原样搬进 PNG/JPEG/WebP。
    pub keep_exif: bool,
    /// 说明与版权信息：静帧写进 EXIF，成片写进容器标签。四项全空就一个字节不动。
    pub metadata: Metadata,
    /// 每张导出的静帧旁边写一个同名 `.xmp` 旁车（`dc:` 四项 + 这一张的 `crs:` 调色）。
    /// 只对静帧模式生效：视频的中间帧是过程产物，不给它们发旁车。
    pub xmp_sidecar: bool,
    /// 输出锐化用途档；默认 `Off`，保持导出像素与预览一致。
    pub sharpen_for: OutputSharpen,
    pub sharpen_strength: SharpenStrength,
    /// 中间帧输出目录。
    pub out_dir: PathBuf,
    /// 成片文件。
    pub video_out: PathBuf,
    pub crop: Option<CropRect>,
    /// 拉直角度（度，正角=画面逆时针）；在裁切之前整幅绕中心转过去。
    pub rotate: f64,
    /// 运镜（Ken Burns）：取景框从 `from` 走到 `to`，逐帧把画面重画进输出画幅。
    /// 默认关着；两个框重合时也当作没开——那种情况一像素都不重采样，不掉清晰度。
    /// 框是归一化的，作用在「拉直 + 裁切之后」的那一幅上，所以跟 `crop`/`rotate`
    /// 是两段独立的几何，先后顺序不会互相吞选区。
    pub motion: Motion,
    /// 按长边缩放；`None` 保持裁切后的原始尺寸。
    pub long_side: Option<u32>,
    /// 精确输出宽高：等比 cover 放大后居中裁切。与 `long_side` 同时给时以它为准。
    pub target_size: Option<(u32, u32)>,
    pub fps: f64,
    pub codec: Codec,
    pub crf: u8,
    pub preset: String,
    pub audio: Option<PathBuf>,
    /// 配乐的音量 / 延迟 / 淡入淡出 / 循环等选项；没有配乐时忽略。
    pub audio_opts: AudioOptions,
    pub keep_frames: bool,
    /// 断点续跑：输出目录里已经写完的那一帧（连带该带的旁车）原样留着，只补没写出来的。
    /// 默认关，跟改造前一样每帧重渲——开着它重跑一次不会重新调色，改了参数还想重导就别开。
    /// 判据是「文件在且非空」，所以静帧一律先写同名 `.part` 再改名占用（见 `write_still`），
    /// 被杀在中途的那张不会留下半个文件骗过续跑。
    /// 成片模式下它必然走「先落中间帧再编码」两段路：管道直连是一条不落盘的流，没得补。
    pub resume: bool,
    /// 输出色域：静态图嵌 ICC，视频写 VUI。默认 sRGB，跟改造前的行为一致。
    pub color_space: ColorSpace,
    /// 水印：盖在缩放与锐化之后、色域搬运之前；默认关，一像素都不动。
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
            watermark: Watermark::default(),
        }
    }
}

impl RenderOptions {
    /// 输出锐化的实际参数：用途档给基准（强度 amount∈[0,1]、半径、阈值），
    /// 强度档只乘强度。`Off` 返回 `None`，那条路上导出像素与预览逐位一致。
    pub fn sharpen_profile(&self) -> Option<(f32, f32, f32)> {
        let (amount, radius, threshold) = self.sharpen_for.profile()?;
        Some((amount * self.sharpen_strength.scale(), radius, threshold))
    }

    /// 第 `i` 个**输出**帧（共 `total` 帧）的取景框；没开运镜或两框重合时是 `None`。
    ///
    /// 用的是输出序号而不是源帧序号：变速把 480 源帧压成 240 输出帧时，运镜照样要
    /// 在 240 帧里走完，不然片尾的框永远停在半路。
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

    /// 单帧导出用的默认设定：GUI「导出当前帧」只需要成片档那几个旋钮。
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
#[derive(Clone, Copy, Default)]
pub struct StillMeta<'a> {
    pub icc: Option<&'a [u8]>,
    pub exif: Option<&'a [u8]>,
    /// 源是 16bit 且容器是 PNG 时保住位深——这是单帧那条路有意和批量不同的地方。
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

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("已取消")]
    Cancelled,
    #[error("没有可导出的帧")]
    Empty,
    #[error("读图失败 {path}：{source}")]
    Decode {
        path: String,
        source: crate::metrics::MetricsError,
    },
    #[error("写图失败 {path}：{source}")]
    Write {
        path: String,
        source: image::ImageError,
    },
    #[error("找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）")]
    NoFfmpeg,
    #[error("ffmpeg 失败：{0}")]
    Ffmpeg(String),
    #[error("参数不合法：{0}")]
    Invalid(String),
    #[error("IO 失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("写 XMP 旁车失败：{0}")]
    Xmp(#[from] crate::xmp::XmpError),
}

/// 优先 `AEVUM_FFMPEG`（旧名 `LUMEN_FFMPEG` 回退），其次 PATH 里的 ffmpeg。
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
pub fn pattern_for(dir: &Path, format: StillFormat) -> PathBuf {
    dir.join(format!("frame_%06d.{}", format.ext()))
}

/// 逐帧调色、对齐、裁切并写出。`params`/`gains`/`offsets` 与 `paths` 等长。
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

/// 渲一个输出帧并压成 rgb24，供直连 ffmpeg 的管道喂给编码器。
///
/// 单来源那一路直接在 scratch 里出，跟改造前的 1:1 路径逐字节相同；只有真的要混
/// 几帧时才用 `mix` 多占一块全幅缓冲。
#[allow(clippy::too_many_arguments)]
fn output_frame_rgb24(
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
    Ok((to_rgb24(rgb, options.color_space), tw, th))
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
    preview::render(a, w as usize, h as usize, params, gain);

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
        // 链的量。SDR 单路；HDR（PQ）输出编码预留 OutputEncoding::HdrPq。
        spatial::output_sharpen_usm(
            rgb,
            w,
            h,
            spatial::UsmSettings::new(amount, radius, threshold),
            spatial::OutputEncoding::Sdr,
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

/// 用序列帧编码视频；`-progress pipe:1` 的帧计数转成进度。
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
    let (first, fw, fh) = output_frame_rgb24(
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
                    let (bytes, w, h) = output_frame_rgb24(
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
        "rgb24".to_owned(),
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
    let (primaries, transfer, matrix) = options.color_space.vui();
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
pub fn cover_size(w: u32, h: u32, tw: u32, th: u32) -> (u32, u32) {
    let scale = (tw as f64 / w.max(1) as f64).max(th as f64 / h.max(1) as f64);
    (
        ((w as f64 * scale).round() as u32).max(tw),
        ((h as f64 * scale).round() as u32).max(th),
    )
}

/// 给定输入尺寸与长边限制，返回偶数宽高（编码器要求）。
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
pub fn frame_path(dir: &Path, format: StillFormat, index: usize) -> PathBuf {
    let name = format!("frame_{index:06}.{}", format.ext());
    dir.join(name)
}

/// 这一帧是不是已经导完了：静帧本身在盘上且非空，该带的 XMP 旁车也在盘上且非空。
///
/// 「非空」不是多余的一眼：`write_still` 写的是同名 `.part` 再改名占用，正常路径上
/// 不会有零字节文件，但用户手工截出来的空文件、或者别的程序占位留下的空文件，
/// 在这儿一律当成没导过——重渲一张比把坏文件当成品留着便宜。
pub fn frame_is_done(dst: &Path, options: &RenderOptions) -> bool {
    let filled = |p: &Path| p.metadata().is_ok_and(|m| m.is_file() && m.len() > 0);
    filled(dst)
        && (!options.xmp_sidecar
            || options.mode != OutputMode::Stills
            || filled(&crate::xmp::sidecar_for(dst)))
}

/// 断点续跑之前先数一遍已经导完多少帧，给状态栏写「跳过 N · 补写 M」。
/// 只按文件名挨个 stat，一帧一次 IO，4K 序列几千帧也就几毫秒。
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
fn frame_pixels(path: &Path) -> u64 {
    let info = if crate::raw::is_raw_name(path) {
        crate::raw::info(path)
            .map(|i| Some((i.width, i.height)))
            .unwrap_or(None)
    } else if crate::dng::is_named_dng(path) {
        crate::dng::info(path)
            .map(|i| Some((i.width, i.height)))
            .unwrap_or(None)
    } else {
        image::ImageReader::open(path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.into_dimensions().ok())
    };
    info.map_or(0, |(w, h)| w as u64 * h as u64)
}

/// 一路工作线程一帧同时要占的字节数：源 f32（3×4B/px）加上裁切副本、缩放中间层
/// 和输出，按源的三倍封顶。24MP 约 0.86GB。
fn frame_working_set(px: u64) -> u64 {
    (px * 3 * 4 * 3).max(64 << 20)
}

/// 并行路数：每帧的解码是单线程的，路数就是倍数；上限由物理内存的四分之一和核数
/// （留一半给界面与编码器）共同决定。24MP 一路约 0.6GB，四路之后再快也只能省下
/// 0.1s 上下（实测），却要多占一倍内存，所以大图封顶四路。
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
    let cap = if px > 12_000_000 {
        4
    } else {
        (cores / 2).clamp(1, 8) as u64
    };
    by_ram.min(cap) as usize
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
    use super::{area_weights, avif_crf};

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
