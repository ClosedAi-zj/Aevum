//! 工程文件：让参数、评级、关键帧、对齐与成片设置随序列一起落盘。
//!
//! 位置固定为 `<序列根>/.lumen/project.json`；缺字段一律取默认值，
//! 所以旧工程、只写了一半的工程都能打开。保存先写临时文件再替换。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::align::{AlignIssue, CropRect};
use crate::audio::AudioOptions;
use crate::grade::GradeParams;

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AlignState {
    pub reference: usize,
    pub estimator: String,
    pub offsets: Vec<[f64; 2]>,
    pub scores: Vec<f64>,
    pub issues: Vec<AlignIssue>,
    pub crop: Option<CropRect>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderState {
    pub mode: String,
    pub format: String,
    /// 有损容器（JPEG）的压缩质量 1~100；无损档忽略。
    pub quality: u8,
    /// 把源 JPEG 的 EXIF 块搬进静帧输出。
    pub keep_exif: bool,
    /// 每张静帧旁写一个同名 `.xmp` 旁车（`dc:` 四项 + 这一张的调色参数）。
    pub xmp_sidecar: bool,
    /// 输出锐化用途档键（`off`/`screen`/`glossy`/`matte`）。
    pub sharpen_for: String,
    /// 输出锐化强度档键（`low`/`standard`/`high`）。
    pub sharpen_strength: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub codec: String,
    pub crf: u8,
    pub preset: String,
    pub out_dir: String,
    pub video_out: String,
    pub audio: String,
    /// 配乐的延迟 / 音量 / 淡入淡出 / 循环 / 编码；旧工程缺省即为默认。
    pub audio_opts: AudioOptions,
    pub long_side: Option<u32>,
    pub keep_frames: bool,
    /// 输出色域键（`srgb`/`p3`/`rec2020`）；旧工程没这个字段时按 sRGB 走，行为不变。
    pub color_space: String,
    /// 水印整块设定；旧工程没有这一段就是没开水印。
    pub watermark: crate::watermark::Watermark,
    /// 时间重映射（变速 + 运动模糊）；旧工程没这一段就是 1:1 原速。
    pub retime: crate::retime::Retime,
    /// 运镜（Ken Burns）：取景框从起点走到终点；旧工程没这一段就是不开。
    pub motion: crate::crop::Motion,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            mode: "video".to_owned(),
            format: "png8".to_owned(),
            quality: 90,
            keep_exif: false,
            xmp_sidecar: false,
            sharpen_for: crate::export::OutputSharpen::default().key().to_owned(),
            sharpen_strength: crate::export::SharpenStrength::default().key().to_owned(),
            width: 3840,
            height: 2160,
            fps: 24.0,
            codec: "h265".to_owned(),
            crf: 18,
            preset: "medium".to_owned(),
            out_dir: String::new(),
            video_out: String::new(),
            audio: String::new(),
            audio_opts: AudioOptions::default(),
            long_side: None,
            keep_frames: false,
            color_space: crate::colorspace::ColorSpace::default().key().to_owned(),
            watermark: crate::watermark::Watermark::default(),
            retime: crate::retime::Retime::default(),
            motion: crate::crop::Motion::default(),
        }
    }
}

/// 一个目录的说明与版权信息（IPTC 的那几项，按都柏林核心写进 XMP / EXIF）。
///
/// 跟着工程文件走，不另开数据库：图库、序列与导出读的是同一份，改一处三边都认。
/// 全字段可选，空字符串就是没填——写元数据时按空跳过，不往文件里塞空标签。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    /// 标题（`dc:title` / EXIF 无对应，只进 XMP 与视频容器）。
    pub title: String,
    /// 说明 / 字幕（`dc:description` / EXIF `ImageDescription`）。
    pub description: String,
    /// 创作者（`dc:creator` / EXIF `Artist`）。
    pub creator: String,
    /// 版权声明（`dc:rights` / EXIF `Copyright`）。
    pub copyright: String,
}

/// 各字段的收口上限，都要留得住 `exif` 那边读回来的余地：
/// ASCII 标签按字节数读（`exif::MAX_TEXT` = 512 字节，含结尾 NUL），截断了多字节
/// 字符会让 UTF-8 解码失败、自己写的说明自己读不回来；标题走 UCS2，按字符算。
const LIMIT_TITLE_CHARS: usize = 200;
const LIMIT_DESCRIPTION_BYTES: usize = 480;
const LIMIT_PERSON_BYTES: usize = 120;

impl Metadata {
    /// 四项全空（或只剩空白）。导出按它决定要不要动元数据。
    pub fn is_empty(&self) -> bool {
        [
            &self.title,
            &self.description,
            &self.creator,
            &self.copyright,
        ]
        .iter()
        .all(|s| s.trim().is_empty())
    }

    /// 收干净的副本：去首尾空白、把换行与连排空白折成单个空格、按上限截断。
    /// EXIF 的 ASCII 标签放不下换行，说明里的换行也得折掉。
    pub fn cleaned(&self) -> Self {
        Self {
            title: self
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(LIMIT_TITLE_CHARS)
                .collect(),
            description: cut_bytes(&self.description, LIMIT_DESCRIPTION_BYTES),
            creator: cut_bytes(&self.creator, LIMIT_PERSON_BYTES),
            copyright: cut_bytes(&self.copyright, LIMIT_PERSON_BYTES),
        }
    }

    /// 只留填了的项，给导出与 XMP 用：`(键, 值)` 的非空子集，值已归一化。
    pub fn present(&self) -> Vec<(&'static str, String)> {
        let c = self.cleaned();
        [
            ("title", c.title),
            ("description", c.description),
            ("creator", c.creator),
            ("copyright", c.copyright),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect()
    }
}

/// 折空白并按字节数截断，截在字符边界上（切半个汉字会让整段文本解不出来）。
fn cut_bytes(raw: &str, bytes: usize) -> String {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= bytes {
        return flat;
    }
    let mut end = bytes;
    while end > 0 && !flat.is_char_boundary(end) {
        end -= 1;
    }
    flat[..end].to_owned()
}

/// 一套命名调色：与主链同构（基础参数 + 关键帧）。同一个序列的多版调色
/// 并存互不覆盖，随时取回当前，或指定给某一次导出 / 队列段。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradeCopy {
    pub name: String,
    pub params: GradeParams,
    pub keyframes: BTreeMap<usize, GradeParams>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectFile {
    pub version: u32,
    pub root: String,
    pub reference: usize,
    pub params: GradeParams,
    pub preset: String,
    pub ratings: BTreeMap<usize, u8>,
    pub flags: BTreeMap<usize, i8>,
    pub labels: BTreeMap<usize, u8>,
    /// 逐帧关键词（归一化、排序）；图库按它筛选，序列里也能读。
    pub keywords: BTreeMap<usize, Vec<String>>,
    /// 堆叠：一叠 = 一组帧索引（0 起）。落盘前经 `library::normalize_stacks`
    /// 归一（升序、≥2 帧、叠间不重叠、按头帧排序），旧工程没这一段就是没叠。
    pub stacks: Vec<Vec<usize>>,
    /// 这个目录的说明与版权信息；旧工程没这一段就是四项全空。
    pub metadata: Metadata,
    pub keyframes: BTreeMap<usize, GradeParams>,
    /// 命名调色副本；旧工程没这一段就是没有副本。
    pub copies: Vec<GradeCopy>,
    pub def_window: f64,
    pub def_strength: f64,
    pub def_multi: bool,
    /// 逐帧去闪烁曝光补偿（EV），渲染队列据此复现效果。
    pub def_offsets: Vec<f32>,
    /// 分通道去闪的逐帧色差残差（R,G,B）；单通道去闪时全为零。
    pub def_chan: Vec<[f32; 3]>,
    /// 去闪烁是否按 R/G/B 各测各的（压彩色场景的色闪）。
    pub def_channels: bool,
    /// 画幅比例标签（`原生 / 16:9 / 4:3 / 1:1 / 9:16`）。裁剪框是按这个比例定形的，
    /// 两者必须一起存，否则重新打开工程时框和比例会对不上。
    pub aspect: String,
    /// 用户拖出来的裁剪框（对齐后坐标，原图像素）。`None` = 没手动裁，
    /// 取景按画幅比例在对齐安全框内居中内接。GUI 恢复的是这一项，
    /// 好让框还能接着拖；`crop` 是它叠完对齐与画幅之后的最终取景。
    pub user_crop: Option<CropRect>,
    /// 拉直角度（度，正角=画面逆时针）。取景框是在转过之后的帧里量的，
    /// 所以角度和框必须一起存：只存框会在重开时把画面转回去、框却还留着。
    pub rotate: f64,
    /// 导出取景：GUI 把对齐裁切、用户框、画幅比例叠好后写下来的最终框，
    /// 渲染队列照它裁，不再自己重算几何。
    pub crop: Option<CropRect>,
    pub align: Option<AlignState>,
    pub render: RenderState,
    pub font_level: usize,
}

impl Default for ProjectFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            root: String::new(),
            reference: 0,
            params: GradeParams::default(),
            preset: "原始".to_owned(),
            ratings: BTreeMap::new(),
            flags: BTreeMap::new(),
            labels: BTreeMap::new(),
            keywords: BTreeMap::new(),
            stacks: Vec::new(),
            metadata: Metadata::default(),
            keyframes: BTreeMap::new(),
            copies: Vec::new(),
            def_window: 15.0,
            def_strength: 100.0,
            def_multi: false,
            def_offsets: Vec::new(),
            def_chan: Vec::new(),
            def_channels: false,
            aspect: String::new(),
            user_crop: None,
            rotate: 0.0,
            crop: None,
            align: None,
            render: RenderState::default(),
            font_level: 0,
        }
    }
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("读写工程失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("工程文件不是合法 JSON：{0}")]
    Json(#[from] serde_json::Error),
    #[error("工程版本 {0} 比程序支持的 {VERSION} 新，请升级程序")]
    Version(u32),
}

/// 序列根目录下的护带目录：工程文件、控制令牌、操作日志都在这儿。
/// 名字只这一处定义，`journal`、`control` 与迁移脚本都跟着它走。
pub const GUARD_DIR: &str = ".lumen";

/// 2026-10 之前叫 `.lapse`。
const LEGACY_GUARD_DIR: &str = ".lapse";

/// 打开序列时把旧护带目录搬成新名字：老工程不用先跑迁移脚本也能接着用。
///
/// 两边都在时不动——那说明用户自己动过，猜哪边是对的会覆盖数据。
/// 只认目录：根上还有 `.lapse_ratings.json` 这类同名前缀的文件，那是 Python 版的。
pub fn migrate_guard_dir(root: &Path) -> bool {
    let old = root.join(LEGACY_GUARD_DIR);
    let new = root.join(GUARD_DIR);
    if !old.is_dir() || new.exists() {
        return false;
    }
    std::fs::rename(&old, &new).is_ok()
}

/// `<root>/.lumen/project.json`
pub fn project_path(root: &Path) -> PathBuf {
    root.join(GUARD_DIR).join("project.json")
}

pub fn load(path: &Path) -> Result<ProjectFile, ProjectError> {
    let text = std::fs::read_to_string(path)?;
    let project: ProjectFile = serde_json::from_str(&text)?;
    if project.version > VERSION {
        return Err(ProjectError::Version(project.version));
    }
    Ok(project)
}

/// 先写同目录临时文件再替换，避免崩溃时留下半个工程。
pub fn save(path: &Path, project: &ProjectFile) -> Result<(), ProjectError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(project)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    if path.exists() {
        // Windows 的 rename 不能覆盖已存在的目标。
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
