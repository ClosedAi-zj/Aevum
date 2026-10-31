//! 应用状态与后台任务。
//!
//! 所有重活（测光、缩略图、预览渲染、导出）都在线程里做，界面线程只收消息、
//! 传纹理。消息带令牌：换序列或换帧后，晚到的旧结果直接丢弃。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::{Duration, Instant};

use aevum_core::align::{self, AlignReport, CropRect, Estimator};
use aevum_core::audio::{AudioCodec, AudioOptions, AudioProbe};
use aevum_core::colorspace::ColorSpace;
use aevum_core::crop::{self, Ease, Handle, Motion, Viewfinder};
use aevum_core::cube;
use aevum_core::deflicker::{self, Analysis, FrameMeans};
use aevum_core::develop::Ev;
use aevum_core::exif::Exif;
use aevum_core::export::{
    self, Codec, JobProgress, OutputMode, OutputSharpen, RenderOptions, SharpenStrength, Stage,
    StillFormat,
};
use aevum_core::grade::{self, GradeParams, PRESET_NAMES};
use aevum_core::interp;
use aevum_core::journal;
use aevum_core::mask;
use aevum_core::metrics::{self, FrameMetrics};
use aevum_core::project::{self, AlignState, GradeCopy, ProjectFile, RenderState};
use aevum_core::qa::{self, Finding};
use aevum_core::queue::{self, QueueItem, QueueStatus};
use aevum_core::retime::{self, Retime, SpeedKey};
use aevum_core::sequence::{Sequence, SequenceError};
use aevum_core::warp::{self, Framing};
use aevum_core::watermark::{Spot, Watermark, WatermarkKind};
use aevum_core::xmp;
use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions, Vec2};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::control::{self, ControlEvent, ControlFile, ControlServer};
use crate::history::{self, History, Snapshot};
use crate::icons::Glyph;

pub const PREVIEW_SIDE: u32 = 1600;
pub const THUMB_SIDE: u32 = 128;
pub const PLAY_FPS: f32 = 24.0;
/// 播放时提前取回多少帧的缩略图。按 24fps 算是两秒的画面，够盖住一次机身预览
/// 解码加纹理上传的往返；再长就只是把内存花在还没播到的帧上。
const PLAY_THUMB_AHEAD: usize = 48;
/// 色标序号对应的名字：0 是没打，1…4 跟 `shell::label_color` 同序。
pub const LABEL_NAMES: [&str; 5] = ["无", "红", "黄", "绿", "蓝"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Module {
    Sequence,
    Deflicker,
    Develop,
    Frame,
    Render,
    Audio,
}

impl Module {
    pub const ALL: [Module; 6] = [
        Module::Sequence,
        Module::Deflicker,
        Module::Develop,
        Module::Frame,
        Module::Render,
        Module::Audio,
    ];

    pub fn cn(self) -> &'static str {
        match self {
            Module::Sequence => "序列",
            Module::Deflicker => "去闪",
            Module::Develop => "显影",
            Module::Frame => "构图",
            Module::Render => "成片",
            Module::Audio => "声音",
        }
    }

    pub fn en(self) -> &'static str {
        match self {
            Module::Sequence => "SEQUENCE",
            Module::Deflicker => "DEFLICKER",
            Module::Develop => "DEVELOP",
            Module::Frame => "FRAME",
            Module::Render => "RENDER",
            Module::Audio => "AUDIO",
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            Module::Sequence => Glyph::Sequence,
            Module::Deflicker => Glyph::Deflicker,
            Module::Develop => Glyph::Develop,
            Module::Frame => Glyph::Frame,
            Module::Render => Glyph::Render,
            Module::Audio => Glyph::Audio,
        }
    }

    /// 模块序号（0-5），控制通道与 AI 面板共用。
    pub fn index(self) -> usize {
        Module::ALL
            .iter()
            .position(|m| *m == self)
            .unwrap_or_default()
    }

    /// 认中文名、英文名和 0-5 序号；入口参数与控制命令共用同一套写法。
    pub fn from_name(name: &str) -> Option<Module> {
        match name.trim() {
            "序列" | "sequence" | "seq" | "0" => Some(Module::Sequence),
            "去闪" | "deflicker" | "def" | "1" => Some(Module::Deflicker),
            "显影" | "develop" | "dev" | "2" => Some(Module::Develop),
            "构图" | "frame" | "3" => Some(Module::Frame),
            "成片" | "render" | "4" => Some(Module::Render),
            "声音" | "audio" | "5" => Some(Module::Audio),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Aspect {
    Native,
    Wide,
    FourThree,
    Square,
    Vertical,
}

impl Aspect {
    pub const ALL: [Aspect; 5] = [
        Aspect::Native,
        Aspect::Wide,
        Aspect::FourThree,
        Aspect::Square,
        Aspect::Vertical,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Aspect::Native => "原生",
            Aspect::Wide => "16:9",
            Aspect::FourThree => "4:3",
            Aspect::Square => "1:1",
            Aspect::Vertical => "9:16",
        }
    }

    pub fn ratio(self) -> Option<f32> {
        match self {
            Aspect::Native => None,
            Aspect::Wide => Some(16.0 / 9.0),
            Aspect::FourThree => Some(4.0 / 3.0),
            Aspect::Square => Some(1.0),
            Aspect::Vertical => Some(9.0 / 16.0),
        }
    }

    /// 整数分子分母：定比例的推进步长要按它成对走（见 `aevum_core::crop`）。
    pub fn pair(self) -> Option<(u32, u32)> {
        match self {
            Aspect::Native => None,
            Aspect::Wide => Some((16, 9)),
            Aspect::FourThree => Some((4, 3)),
            Aspect::Square => Some((1, 1)),
            Aspect::Vertical => Some((9, 16)),
        }
    }

    /// 从存档标签找回比例；空串或认不出的都按「原生」，旧工程文件照样能开。
    pub fn from_label(label: &str) -> Aspect {
        Aspect::ALL
            .into_iter()
            .find(|a| a.label() == label)
            .unwrap_or(Aspect::Native)
    }
}

/// 画布上正在拖的裁剪交互。按下时定性和起点，拖动过程只改草稿，松手才落账。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CropDrag {
    /// 在框外按下：以按下点为角拉一个新框。
    Draw { anchor: [f64; 2] },
    /// 在框内按下：整体平移，`grab` 记住按下点相对框左上角的偏移。
    /// `orig` 是按下那一刻的框：拖动全程以它为锚，`user_crop` 只在真的拖出
    /// 位移后才落值——纯点击（按下又松开）不碰任何可撤销状态，也就不会
    /// 在历史里留下一条「什么都没改」的裁剪步。
    Move { orig: CropRect, grab: [f64; 2] },
    /// 按住某个把手缩放。`orig` 是按下那一刻的框，拖动全程以它为锚，
    /// 免得每帧重新锚定把框一点点吃进去。
    Resize { handle: Handle, orig: CropRect },
}

/// 运镜取景框有两端，画布上同一时刻只拖其中一端。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionBox {
    Start,
    End,
}

impl MotionBox {
    pub fn cn(self) -> &'static str {
        match self {
            MotionBox::Start => "起点",
            MotionBox::End => "终点",
        }
    }

    pub fn other(self) -> MotionBox {
        match self {
            MotionBox::Start => MotionBox::End,
            MotionBox::End => MotionBox::Start,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Hand,
    Zoom,
    Crop,
    Compare,
    Grid,
}

impl Tool {
    pub const ALL: [Tool; 5] = [
        Tool::Hand,
        Tool::Zoom,
        Tool::Crop,
        Tool::Compare,
        Tool::Grid,
    ];

    pub fn cn(self) -> &'static str {
        match self {
            Tool::Hand => "抓手",
            Tool::Zoom => "缩放",
            Tool::Crop => "裁剪",
            Tool::Compare => "对比",
            Tool::Grid => "网格",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Tool::Hand => "抓手 (H) · 拖动画布",
            Tool::Zoom => "缩放 (Z) · 单击放大，Alt 单击缩小",
            Tool::Crop => "裁剪 (C) · 画幅与辅助线",
            Tool::Compare => "前后对比 (\\) · 左右分割",
            Tool::Grid => "网格 (G) · 三分线",
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            Tool::Hand => Glyph::Hand,
            Tool::Zoom => Glyph::Zoom,
            Tool::Crop => Glyph::Crop,
            Tool::Compare => Glyph::Compare,
            Tool::Grid => Glyph::Grid,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DockTab {
    Adjust,
    Info,
    Keys,
    Ai,
    History,
    Library,
}

impl DockTab {
    pub const ALL: [DockTab; 6] = [
        DockTab::Adjust,
        DockTab::Info,
        DockTab::Keys,
        DockTab::Ai,
        DockTab::History,
        DockTab::Library,
    ];

    pub fn cn(self) -> &'static str {
        match self {
            DockTab::Adjust => "调整",
            DockTab::Info => "信息",
            DockTab::Keys => "关键帧",
            DockTab::Ai => "AI",
            DockTab::History => "历史",
            DockTab::Library => "图库",
        }
    }

    pub fn index(self) -> usize {
        match self {
            DockTab::Adjust => 0,
            DockTab::Info => 1,
            DockTab::Keys => 2,
            DockTab::Ai => 3,
            DockTab::History => 4,
            DockTab::Library => 5,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "调整" | "adjust" | "0" => Some(DockTab::Adjust),
            "信息" | "info" | "1" => Some(DockTab::Info),
            "关键帧" | "keys" | "key" | "2" => Some(DockTab::Keys),
            "ai" | "agent" | "3" => Some(DockTab::Ai),
            "历史" | "history" | "undo" | "4" => Some(DockTab::History),
            "图库" | "library" | "lib" | "5" => Some(DockTab::Library),
            _ => None,
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            DockTab::Adjust => Glyph::Sliders,
            DockTab::Info => Glyph::Info,
            DockTab::Keys => Glyph::Key,
            DockTab::Ai => Glyph::Star,
            DockTab::History => Glyph::History,
            DockTab::Library => Glyph::Folder,
        }
    }
}

/// AI 面板里的一条执行记录（只保留最近若干条）。
#[derive(Clone, Debug)]
pub struct AiLogEntry {
    /// 用户输入的原始命令 + 参数。
    pub input: String,
    pub ok: bool,
    /// 成功是返回 JSON，失败是中文错误。
    pub detail: String,
}

pub struct Thumb {
    pub texture: TextureHandle,
    pub size: [u32; 2],
}

/// 进不了 [`Snapshot`]、但照样要写进工程文件那部分的签名。
///
/// 对齐报告是外部效果，不进历史快照，取景裁切又是由它推出的——只看 `Snapshot`
/// 会让「重跑一次对齐」这种改动永远不落盘，所以自动保存额外比一下这个。
#[derive(Clone, Debug, PartialEq)]
struct ProjectStamp {
    align_ref: usize,
    estimator: &'static str,
    offsets: usize,
    issues: usize,
    crop: Option<(u32, u32, u32, u32)>,
}

struct Loaded {
    token: u64,
    metrics: Vec<Option<FrameMetrics>>,
    def_means: Vec<Option<FrameMeans>>,
    issues: Vec<(usize, String)>,
    resolution: Option<(u32, u32)>,
    /// 首帧的保真度口径（RAW 才有：传感器 CFA 还是机身预览）。
    source_note: Option<String>,
    exif: Vec<Exif>,
}

struct ThumbMsg {
    index: usize,
    image: ColorImage,
    size: [u32; 2],
}

/// 图库缩略图回包：键是文件路径，与序列缩略图的帧号键分开。
struct LibThumbMsg {
    path: PathBuf,
    image: ColorImage,
    size: [u32; 2],
}

/// `render_preview_file` 的回包形状：调色后 / 原图（只在分屏时才出）/ 尺寸 / 三通道直方图。
type PreviewParts = (ColorImage, Option<ColorImage>, [u32; 2], [[u32; 256]; 3]);

/// 预览回包。整包三 KB 出头，直接当 `Msg` 的变体摆着，每条消息（哪怕只是一句状态
/// 文本）都得按最大变体分配；渲染线程每帧都发，所以装箱。
struct PreviewMsg {
    stamp: u64,
    /// 这张是拖动草图还是全量：草图欠一张全量，见 `preview_owes_full`。
    sketch: bool,
    graded: ColorImage,
    /// 分屏用的「之前」：没开分屏就是 `None`，省掉一次取景与 6.8MB 上传。
    original: Option<ColorImage>,
    size: [u32; 2],
    hist: [[u32; 256]; 3],
}

/// HDR / 全景合并的任务种类。两条路共用一套后台任务、进度与状态命令，
/// 差别只在读帧之后是曝光融合还是特征点拼接。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeKind {
    Hdr,
    Pano,
}

impl MergeKind {
    fn cn(self) -> &'static str {
        match self {
            Self::Hdr => "HDR 合并",
            Self::Pano => "全景拼接",
        }
    }

    /// 控制面 `kind` 字段与成片文件名前缀用的短名。
    fn tag(self) -> &'static str {
        match self {
            Self::Hdr => "hdr",
            Self::Pano => "pano",
        }
    }

    /// 没设输出目录时落在序列根下的目录名。
    fn dir_name(self) -> &'static str {
        match self {
            Self::Hdr => "HDR",
            Self::Pano => "全景",
        }
    }

    /// 拼进中文句子的那部分（「写 HDR 成片失败」），与 `cn` 分开是因为 `cn` 自带动作。
    fn noun(self) -> &'static str {
        match self {
            Self::Hdr => "HDR",
            Self::Pano => "全景",
        }
    }

    /// 一次能吃的帧数上限，来自各自的核心模块。
    fn max_frames(self) -> usize {
        match self {
            Self::Hdr => aevum_core::hdr::MAX_FRAMES,
            Self::Pano => aevum_core::pano::MAX_FRAMES,
        }
    }
}

/// 一次合并的全部输入。界面线程抓完就交给后台线程，线程里碰不到 `AppState`。
/// `framing` 与 `srcs` 一一对应；全景靠特征点自己求几何，给全幅零位移。
struct MergeJob {
    kind: MergeKind,
    indices: Vec<usize>,
    srcs: Vec<PathBuf>,
    framing: Vec<(Option<CropRect>, [f64; 2])>,
    dst: PathBuf,
    options: RenderOptions,
}

/// 插件后台任务往界面线程送的消息：进度心跳，或最终结果。
enum PluginMsg {
    /// (已完成块数, 总块数)。
    Progress(u32, u32),
    Done(Result<aevum_core::plugin_host::Applied, String>),
}

/// 在飞的插件任务：完成接收端 + 取消旗 + 元信息。
///
/// 取消只立旗，子进程由工作线程自己收尸 —— 界面线程绝不 join：
/// 插件卡死时 join 会把整个界面一起冻住，收尸的活归干活的人。
pub struct PluginJob {
    /// 插件 id（清单里的那份），崩溃计数按它记。
    pub id: String,
    /// 插件显示名，状态栏与消息里露脸的是它。
    pub name: String,
    /// 起任务的时刻，超时兜底与"跑了多久"都用它。
    pub started: Instant,
    rx: Receiver<PluginMsg>,
    cancel: Arc<AtomicBool>,
}

enum Msg {
    Loaded(Box<Loaded>),
    Preview(Box<PreviewMsg>),
    Thumbs(Vec<ThumbMsg>),
    LibThumbs(Vec<LibThumbMsg>),
    Saved(String),
    Error(String),
    Aligned {
        token: u64,
        report: AlignReport,
    },
    AlignFailed(String),
    Exported {
        token: u64,
        path: PathBuf,
    },
    ExportFailed(String),
    /// HDR / 全景合并的后台任务回包：成功带成片 JSON，失败带中文原因。
    /// `token` 是发任务时的 `reload_token`，换了序列的迟到回包直接丢。
    Merged {
        token: u64,
        kind: MergeKind,
        result: Result<Value, String>,
    },
    /// 序列体检的后台任务回包：判定清单，或中文失败原因。
    Checked {
        token: u64,
        result: Result<Vec<Finding>, String>,
    },
    QueueRow {
        index: usize,
        status: QueueStatus,
        note: String,
    },
    QueueFinished {
        ok: usize,
        failed: usize,
        cancelled: bool,
    },
}

pub struct AppState {
    pub seq: Option<Sequence>,
    pub metrics: Vec<Option<FrameMetrics>>,
    pub issues: Vec<(usize, String)>,
    pub analysis: Option<Analysis>,
    /// 逐帧去闪曝光偏移（整帧亮度项），分析后缓存，画曲线与预览共用。
    pub corr: Vec<f32>,
    /// 逐帧分通道残差：各通道偏移 − 三通道均值，只在开「分通道」时非零。
    pub corr_rgb: Vec<[f32; 3]>,
    /// 线性光测光结果，分通道去闪的输入；随导入那趟解码一起算，不重复读图。
    pub def_means: Vec<Option<FrameMeans>>,
    /// 逐帧 EXIF，跟导入那趟一起读（只定位读 IFD，不整读文件）。方向已经应用在解码
    /// 里，这份缓存是给「信息」面板和曝光台阶检测看的。
    pub exif: Vec<Exif>,

    pub selected: usize,
    pub module: Module,
    pub tool: Tool,
    pub dock_tab: DockTab,
    pub collapsed: HashSet<&'static str>,
    pub status: String,
    pub error: Option<String>,
    /// 打开序列时检测到的同目录 XMP 边车（第一帧）。只提示不自动套用；
    /// 状态栏给一枚「应用边车」，点了才进参数（见 `apply_sidecar_hint`）。
    pub sidecar_hint: Option<PathBuf>,
    pub measuring: bool,

    pub params: GradeParams,
    pub preset: String,
    pub auto_note: Option<String>,
    /// 自动校正的 256 长边代理：同一帧重复点自动不重复解码。
    auto_probe: Option<(PathBuf, Vec<f32>, usize, usize)>,

    pub def_window: f64,
    pub def_strength: f64,
    pub def_multi: bool,
    /// 分通道（RGB）去闪烁：压彩色场景的色闪，代价是可能引入轻微色偏漂移。
    pub def_channels: bool,

    pub aspect: Aspect,
    pub grid: bool,
    pub cross: bool,
    pub split: bool,
    /// 前后对比分割线的位置（0..1，图宽比例）。视图状态：不进历史快照、
    /// 不落盘——LR 也不记录你把分割线拉到了哪儿。双击手柄回到居中。
    pub split_pos: f32,
    /// 胶片条的用户高度。None = 自动（按缩略图比例适配图片）；Some(h) =
    /// 拖过之后钉住的高度。拖动条在胶片条上边缘，双击回到自动。
    pub film_h: Option<f32>,
    pub zoom: f32,
    /// 画布这一帧算出的「适应窗口」倍率，供缩放读数与 1:1 使用。
    pub fit_scale: f32,
    pub pan: Vec2,

    pub playing: bool,
    last_play: Instant,
    pub font_level: usize,

    pub texture: Option<TextureHandle>,
    pub texture_orig: Option<TextureHandle>,
    pub preview_size: [u32; 2],
    pub preview_stamp: u64,
    pub preview_dirty: bool,
    pub preview_inflight: bool,
    /// 上一帧鼠标是否按住：按住期间出的都是草图。
    preview_dragging: bool,
    /// 屏幕上还是草图、欠一张全量：松手那一帧补，或者草图回包时鼠标已抬起就当场补。
    preview_owes_full: bool,
    /// 手上有没有分屏用的「之前」图。分屏关着时不渲染也不上传它，重新打开要补一张。
    preview_has_orig: bool,

    pub thumbs: HashMap<usize, Thumb>,
    thumb_pending: HashSet<usize>,
    pub scroll_to_selected: bool,

    pub ratings: HashMap<usize, u8>,
    pub flags: HashMap<usize, i8>,
    pub labels: HashMap<usize, u8>,
    /// 打开中的序列自己的关键词；工程保存时原样写回，图库改词也不丢。
    pub keywords: HashMap<usize, Vec<String>>,
    /// 打开中的序列所属目录的堆叠（已归一化）；工程保存时原样写回。
    pub stacks: Vec<Vec<usize>>,
    /// 打开中的序列所属目录的说明与版权信息；随工程文件落盘，导出静帧与成片都按它写。
    pub meta: project::Metadata,

    /// 图库：跨目录的根目录清单（galleries.json）与当前浏览的编目缓存。
    /// 缩略图按路径键，换序列不清——图库浏览的是没打开的目录。
    pub lib_roots: Vec<PathBuf>,
    pub lib_sel: Option<usize>,
    /// 编目里选中的行（键盘 0~5/P/X/6~9 作用在这一行）。
    pub lib_sel_file: Option<usize>,
    /// 编目筛选：星级下限、只看选用、色标。
    pub lib_filter: aevum_core::library::Filter,
    /// 编目里勾选的行（Ctrl 多选 / Shift 连选）；键盘和右键作用在它们身上。
    pub lib_checked: std::collections::BTreeSet<usize>,
    /// Shift 连选的锚点。
    pub lib_anchor: Option<usize>,
    /// 编目里展开的叠（目录序号, 叠号）；纯视图状态，不进工程、不进历史。
    pub lib_stack_open: HashSet<(usize, usize)>,
    pub lib_entries: Vec<Vec<aevum_core::library::LibFile>>,
    /// 手动集合与当前打开的集合视图；新建集合的名字输入框也住这儿。
    pub lib_cols: Vec<aevum_core::library::Collection>,
    pub lib_col: Option<usize>,
    pub lib_col_name: String,
    /// 编目里给选中/勾选行加词的输入框。
    pub lib_keyword: String,
    /// 图库信息面板的编辑缓冲，以及它当前是哪个目录的（换目录要重新读，
    /// 否则会把上一个目录的四项写到这个目录头上）。
    pub lib_meta: project::Metadata,
    pub lib_meta_root: Option<PathBuf>,
    /// 信息面板开没开（编目视图右上角的「信息」）。
    pub lib_meta_open: bool,
    pub lib_thumbs: HashMap<PathBuf, Thumb>,
    lib_thumb_pending: HashSet<PathBuf>,
    /// 撤销 / 重做历史；只收参数类写入，见 [`history`]。
    pub history: History,
    pub hist: Option<[[u32; 256]; 3]>,

    pub show_dock: bool,
    /// 左侧预设栏（预设 / 快照 / 历史）开没开。
    pub show_left: bool,
    /// 最外那条 28px 竖条（网格 / 胶片 / 曲线）开没开。
    pub show_strip: bool,
    /// 手动存的快照：界面稿左栏「快照」那一节的内容。
    ///
    /// 和撤销历史是两回事：历史每动一次参数就记一步，几天后翻不到"那天满意的版本"；
    /// 快照是用户自己按下去钉住的一版，只有手动删。存的是整份 [`Snapshot`]，
    /// 点一下就把参数整组换回去（同样进历史，可以再撤）。
    pub snapshots: Vec<(String, history::Snapshot)>,
    /// 外观参数（明暗 / 皮肤 / 主色 / 圆角）。改动后要同步给 `theme::set_appearance`
    /// 并重刷一次 `theme::apply`，再落盘。
    pub appearance: crate::theme::Appearance,
    /// 布局：状态栏 / 工具轨 / 调整面板 的位置，以及底部胶片条显隐。
    pub layout: crate::layout::Layout,
    /// 外观窗是否打开。
    pub look_open: bool,
    /// 插件注册表：内置清单 + `plugins/` 下扫到的清单。
    pub registry: aevum_core::plugin::Registry,
    /// 插件市场窗口是否打开。
    pub market_open: bool,
    /// 最近一次全量预览的 CPU 像素（RGB8 + 尺寸）。
    ///
    /// 插件跑在别的进程里，拿不到 GPU 纹理，只能给它字节。只存全量帧：
    /// 拖动中的草图帧每帧都会来，每帧转一份 8 MB 不值得。
    pub preview_rgb: Option<(Vec<u8>, [u32; 2])>,
    /// 剪切警告（PS/LR 的黑白场检查）：高光遮罩开关。开着时画布上把
    /// r、g、b 全部顶满的像素盖成红色——过曝一眼见底。
    pub clip_highlights: bool,
    /// 剪切警告：阴影遮罩开关。r、g、b 全部压到底的像素盖成蓝色。
    pub clip_shadows: bool,
    /// 剪切遮罩纹理：随最近一次全量预览生成（红=高光裁切、蓝=阴影裁切、
    /// 其余透明）。草图帧不重算——遮罩不需要跟手，等全量帧补上。
    pub clip_mask: Option<TextureHandle>,
    /// 生成遮罩时对应的预览版本；预览一换就作废重算。
    pub clip_mask_stamp: u64,
    /// 插件算完的结果纹理。有它时画布显示它，参数一变就作废。
    pub plugin_result: Option<TextureHandle>,
    /// 结果对应的预览版本。拿它判断"底图到底变没变"——
    /// 只看"又收到一张预览"是不够的：同一张图重传一次会把结果白扔。
    pub plugin_stamp: u64,
    /// 在飞的插件后台任务；`None` = 空闲。见 [`PluginJob`]。
    pub plugin_job: Option<PluginJob>,
    /// 插件进度 (done, total)，每帧从任务通道拉最新；空闲时是 (0, 0)。
    pub plugin_progress: (u32, u32),
    /// 每个插件 id 的失败/崩溃次数（用户取消不计）。连续 3 次就在错误里
    /// 提示去插件市场停用 —— 只提示，不自动禁用，别替用户做决定。
    pub plugin_crashes: HashMap<String, u32>,
    /// 市场左栏的类别筛选；`None` = 全部。
    pub market_filter: Option<aevum_core::plugin::Kind>,
    /// 市场搜索框。
    pub market_query: String,
    /// 截图演示：让停靠面板直接滚到最底部。
    pub dock_scroll_end: bool,
    /// 截图演示：把某个分组（`group_header` 的 key）滚到面板顶部。
    pub dock_focus: Option<String>,
    pub show_film: bool,
    pub show_curves: bool,
    /// 画布标尺。界面稿的画布是净图，没有那两条 24px 的刻度槽，
    /// 所以默认关；要量像素时从「视图」打开。
    pub rulers: bool,
    pub about_open: bool,

    pub keyframes: BTreeMap<usize, GradeParams>,
    /// 命名调色副本（当前序列工程文件里的那一份）；存/删/改名都进撤销栈。
    pub copies: Vec<GradeCopy>,
    /// 导出时改用某套副本的调色（`export.stills` / `export.video` 的 copy 参数）；
    /// 界面上点「导出」走当前调色，这个字段只在命令通道指定过之后非空，用完即清。
    pub export_copy: Option<String>,
    /// 「调色副本」分组里的名字输入框；纯视图缓冲，不进工程也不进历史。
    pub copy_name: String,
    /// 「HDR 合并」分组的包围半径：当前帧前后各取几帧；纯视图缓冲。
    pub hdr_around: u32,
    /// 最近一次 HDR 合并写出的路径 / 报错，分组里回显一行；纯视图缓冲。
    pub hdr_last: Option<String>,
    pub hdr_error: Option<String>,
    /// 「全景合并」分组的相邻半径：从当前帧起向后取几张接片；纯视图缓冲。
    pub pano_around: u32,
    /// 最近一次全景合并写出的路径 / 报错，分组里回显一行；纯视图缓冲。
    pub pano_last: Option<String>,
    pub pano_error: Option<String>,
    pub resolution: Option<(u32, u32)>,
    /// 首帧读出来的保真度口径，只有 RAW 才有；「信息」面板用它标注预览/传感器两条路。
    pub source_note: Option<String>,

    /// 当前选中的局部蒙版；画布手柄与右侧面板共用。
    pub mask_sel: Option<usize>,
    /// 选中蒙版组里当前编辑的形状（0 基）；换组时回到第一个。
    pub mask_comp: usize,
    /// 正在拖动的 (蒙版序号, 手柄 0/1)。
    pub mask_drag: Option<(usize, u8)>,
    /// 正在涂抹的画笔笔画：(蒙版序号, 形状序号)。松手结束，一笔进一步历史。
    pub mask_paint: Option<(usize, usize)>,
    /// 下一笔的画笔设定：半径按短边百分比（0.2~50），硬度 / 流量 0~100。
    /// 只影响新笔画，不回写已画的；这是画笔的「当前工具状态」，不进撤销历史。
    pub brush_radius: f64,
    pub brush_hardness: f64,
    pub brush_flow: f64,
    pub brush_erase: bool,
    /// 吸管取色态：下一次画布单击把像素色相写进选中蒙版的颜色范围。
    pub mask_pick: bool,
    /// 取色期间留一份当前预览像素供点击读色，取完或退出即清。
    pub(crate) mask_pick_img: Option<ColorImage>,

    /// 蒙版几何三层缓存（性能重构设计 §3 第 1 层）：指纹命中零分配，
    /// 派生屏幕折线随画布矩形复用。失效以指纹自校验为主，
    /// `masks_changed()` / `reset_session` 整体清空兜底。
    pub(crate) mask_geo_cache: crate::shell::MaskGeoCache,

    /// 曲线 PCHIP 插值表缓存（§3 第 2 层）：指纹自校验，无需手动失效。
    pub(crate) curve_lut_cache: Option<crate::widgets::CurveLutCache>,

    /// 直方图 Mesh 缓存的版本号：poll 收到新预览写 `hist` 时 +1（§3 第 3 层）。
    pub(crate) hist_version: u64,
    /// 直方图 4 个 Mesh 形状缓存：键 = (hist_version, rect)。
    pub(crate) hist_mesh_cache: Option<crate::shell::HistMeshCache>,
    /// 去闪修正版本号：`recompute_deflicker()` 入口 +1。
    pub(crate) corr_version: u64,
    /// 去闪曲线点集缓存：键 = (corr_version, 帧数, inner rect)。
    pub(crate) defl_curve_cache: Option<crate::shell::DeflCurveCache>,

    /// 镜头配置文件匹配说明（面板与 `lens.set` 都显示这一行）。
    pub lens_profile_note: String,
    /// 用户关掉自动匹配时暂存的命中校正，再开时原样恢复。
    pub lens_profile_saved: Option<aevum_core::lensdb::Correction>,

    /// DCP 相机配置的显示名与说明；路径本身在 `params.dcp_profile`。
    pub dcp_name: String,
    pub dcp_note: String,
    /// LCP 镜头配置的说明；校正系数本身在 `params.lcp_profile`。
    pub lcp_note: String,

    /// 用户拖出来的裁剪框（对齐后坐标，原图像素）。`None` = 没手动裁，
    /// 取景框退化成「画幅比例在安全框里居中内接」。
    pub user_crop: Option<CropRect>,
    /// 拉直角度（度，正角=画面逆时针）。取景框量的是转过之后的那一帧，
    /// 所以角度一变安全框就跟着缩，框伸到空白角里的部分导出时会被削掉。
    pub rotate: f64,
    /// 画布上正在进行的裁剪拖拽。拖框只动这个矢量覆盖层，不重画预览：
    /// 裁剪态下预览本来就是整幅画面，取景框松手后离开裁剪态才落到像素上。
    pub crop_drag: Option<CropDrag>,
    /// 画布上正在拖的运镜框（与裁剪拖框互斥：同一时刻只有一个人拿鼠标）。
    /// 几何与裁剪同构，所以直接复用 `CropDrag` 的三种定性。
    pub motion_drag: Option<CropDrag>,
    /// 画布交给运镜取景框编辑：构图面板的「拖取景框」开关。开着时画布上的把手
    /// 拉的是起点/终点框（由 `motion_box` 定），裁剪框只画不拖。
    pub motion_picking: bool,
    /// 现在拖的是起点框还是终点框。
    pub motion_box: MotionBox,

    /// HSL 面板当前编辑的色相带（0..7，与 `lut::HSL_CENTERS` 同序）。
    pub hsl_band: usize,
    /// 曲线面板当前编辑的通道：0=RGB、1=红、2=绿、3=蓝。
    pub curve_chan: usize,
    /// 曲线 / 色阶正在拖动的控制点或把手序号。
    pub curve_grab: Option<usize>,
    pub levels_grab: Option<usize>,
    /// 速度曲线正在拖动的键序号（视图状态，不进历史也不进工程）。
    pub retime_grab: Option<usize>,

    pub output_w: u32,
    pub output_h: u32,
    pub output_fps: f32,
    pub codec: String,
    pub export_busy: bool,
    pub last_saved: Option<String>,
    /// 无显示环境（selftest）下不启动任何读盘线程。
    pub headless: bool,
    /// 是否允许开回环控制服务。`--shot` 这类一次性模式关掉，避免和常驻 GUI
    /// 抢同一份 control.json。
    pub control_enabled: bool,

    pub align_report: Option<AlignReport>,
    pub align_busy: bool,
    pub align_estimator: Estimator,
    pub align_ref: usize,
    pub align_preview: bool,
    pub align_msg: Option<String>,
    align_done: Arc<AtomicUsize>,

    pub render_mode: OutputMode,
    pub render_format: StillFormat,
    /// JPEG 的压缩质量（1~100）；无损容器忽略它。
    pub render_quality: u8,
    /// 把源 JPEG 的 EXIF 块搬进静帧输出（版权、机身、曝光跟着走）。
    pub render_keep_exif: bool,
    /// 每张导出的静帧旁边写一个同名 `.xmp` 旁车（说明版权四项 + 这一张的调色）。
    pub render_xmp_sidecar: bool,
    /// 输出锐化：按用途分档，只在缩放之后的成片上生效。
    pub render_sharpen_for: OutputSharpen,
    pub render_sharpen_strength: SharpenStrength,
    /// 输出色域：静态图嵌 ICC，视频写 VUI。跟 render_format 一样是成片级设置。
    pub render_color_space: ColorSpace,
    /// 水印：整块设定跟着工程文件与 `render.set` 走；默认关，导出像素一个不动。
    pub render_watermark: Watermark,
    /// 时间重映射（变速 + 运动模糊）：跟着 `retime.set` 与工程文件走；默认关，
    /// 导出走原来那条 1:1 快路，一个像素都不重采样。
    pub retime: Retime,
    /// 运镜（Ken Burns）：取景框从起点走到终点，跟着 `motion.set` 与工程文件走；
    /// 默认关，且两个框重合时也当作没开。
    pub motion: Motion,
    pub render_crf: u8,
    pub render_preset: String,
    pub render_audio: Option<PathBuf>,
    pub render_audio_opts: AudioOptions,
    /// 配乐探针结果；文件换了才重新读，避免每帧都起进程。
    pub audio_info: Option<AudioProbe>,
    pub audio_probe_err: Option<String>,
    pub render_out_dir: PathBuf,
    pub render_video_out: PathBuf,
    pub render_keep_frames: bool,
    /// 断点续跑：输出目录里已经写完的帧原样留着，这一趟只补没写出来的那些。
    pub render_resume: bool,
    /// 上一次导出按续跑跳过了几帧，只为状态栏那句「跳过 N」服务，不参与任何判定。
    /// 数帧的活与帧数成正比，所以它在导出线程里跑、这儿只放一个读数。
    export_skipped_shared: Arc<AtomicUsize>,
    pub export_stage: Option<Stage>,
    export_done: Arc<AtomicUsize>,
    export_total: Arc<AtomicUsize>,
    export_stage_shared: Arc<AtomicU8>,
    cancel_export: Arc<AtomicBool>,

    /// HDR / 全景合并的长任务。9 张 6K 帧融一遍要 6 秒，全在界面线程上就是
    /// 一整段没有重绘的死窗口，所以与导出同构：后台跑，界面只读计数。
    pub merge_busy: bool,
    pub merge_kind: Option<MergeKind>,
    /// 最近一次成功的成片 JSON（路径、帧号、尺寸），`merge.status` 与面板都读它。
    pub merge_result: Option<Value>,
    pub merge_error: Option<String>,
    merge_done: Arc<AtomicUsize>,
    merge_total: Arc<AtomicUsize>,
    cancel_merge: Arc<AtomicBool>,

    /// 序列体检的后台任务。测光随导入那趟就算好了，这里只是逐帧 `stat` 一遍
    /// 再跑判定——五千帧的 `stat` 也要 100ms 量级，所以与合并同构放后台，
    /// 不放界面线程上。
    pub qa_busy: bool,
    /// 最近一次的体检清单（`qa_run` 为假就是还没跑过，空表是「跑过、干净」）。
    pub qa_findings: Vec<Finding>,
    pub qa_run: bool,
    pub qa_error: Option<String>,
    /// 判定松紧：离群门槛是本序列波动的多少倍。面板滑杆与 `sequence.check` 共用。
    pub qa_sig: f64,
    qa_done: Arc<AtomicUsize>,
    qa_total: Arc<AtomicUsize>,
    cancel_qa: Arc<AtomicBool>,

    /// 批量渲染队列；存盘位置在漫游配置目录，与工程分开。
    pub queue: Vec<QueueItem>,
    pub queue_path: PathBuf,
    pub queue_busy: bool,
    pub queue_current: Option<usize>,
    queue_done: Arc<AtomicUsize>,
    queue_total: Arc<AtomicUsize>,
    queue_stage: Arc<AtomicU8>,
    cancel_queue: Arc<AtomicBool>,

    last_auto_save: Instant,
    /// 上一次真正写进工程文件的内容。当前状态和它一致就不碰盘。
    /// `None` 表示这份序列还没存过（打开序列后的第一次心跳必存）。
    saved_snap: Option<Snapshot>,
    /// 同上，但管的是进不了 [`Snapshot`] 的那部分：对齐报告与取景裁切。
    saved_stamp: Option<ProjectStamp>,
    reload_token: u64,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    /// 本机回环控制服务；只在打开序列后存在，Drop 时清理 control.json。
    pub control: Option<ControlServer>,
    /// AI 面板的输入行与最近记录；与回环控制共用同一个命令执行器。
    pub ai_input: String,
    pub ai_log: Vec<AiLogEntry>,
    control_tx: Sender<ControlEvent>,
    control_rx: Receiver<ControlEvent>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// 剪切遮罩的纯计算：RGB8 像素 → RGBA 遮罩图。红=高光裁切（r/g/b 全部
/// ≥250）、蓝=阴影裁切（全部 ≤5）、其余透明。拆成自由函数是为了测试
/// 不用拉起 GPU 纹理。
fn clip_mask_image(
    rgb: &[u8],
    size: [u32; 2],
    highlights: bool,
    shadows: bool,
) -> Option<egui::ColorImage> {
    if !highlights && !shadows {
        return None;
    }
    let w = size[0] as usize;
    let h = size[1] as usize;
    if w == 0 || h == 0 || rgb.len() < w * h * 3 {
        return None;
    }
    let n = w * h;
    let mut img = vec![0u8; n * 4];
    const HI: u8 = 250;
    const LO: u8 = 5;
    for i in 0..n {
        let (r, g, b) = (rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]);
        // 先判高光再判阴影：纯黑纯白互斥，顺序无所谓，短路优先贵的那个。
        let hit = if highlights && r >= HI && g >= HI && b >= HI {
            Some([255u8, 46, 46, 170])
        } else if shadows && r <= LO && g <= LO && b <= LO {
            Some([56, 110, 255, 170])
        } else {
            None
        };
        if let Some(c) = hit {
            img[i * 4..i * 4 + 4].copy_from_slice(&c);
        }
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w, h], &img))
}

impl AppState {
    /// 把一个滤镜插件作用到当前预览上（看板 B06：后台任务版）。
    ///
    /// 起一条工作线程：真进程 → 握手 → 按 512 的块往返。每块之间向界面报进度，
    /// 用户随时可以取消。结果由 [`AppState::plugin_poll`] 每帧收：成功换一张纹理
    /// 盖在画布上（并等着下一次全量预览把它冲掉），失败进错误横幅并按 id 记一笔。
    /// 重算剪切遮罩：从最近一次全量预览的 CPU 像素里挑出高光/阴影裁切的
    /// 像素，生成一张与预览同尺寸的 RGBA 遮罩（红=高光、蓝=阴影、其余透明）。
    ///
    /// 只在全量帧到达（!sketch）后调用；草图帧用旧遮罩顶着——遮罩是示意
    /// 罩层，与草图帧的宽高比一致，拉伸显示对得上，精度无关紧要。
    /// 阈值取 LR 同款口径：r、g、b 全部 ≥250 视为高光裁切，全部 ≤5 视为阴影。
    pub fn rebuild_clip_mask(&mut self, ctx: &Context, stamp: u64) {
        if !self.clip_highlights && !self.clip_shadows {
            if self.clip_mask.take().is_some() {
                self.clip_mask_stamp = stamp;
            }
            return;
        }
        if self.clip_mask_stamp == stamp && self.clip_mask.is_some() {
            return;
        }
        let Some((rgb, size)) = &self.preview_rgb else {
            return;
        };
        let Some(color) = clip_mask_image(rgb, *size, self.clip_highlights, self.clip_shadows)
        else {
            return;
        };
        match &mut self.clip_mask {
            Some(tex) => tex.set(color, TextureOptions::LINEAR),
            None => {
                self.clip_mask =
                    Some(ctx.load_texture("preview.clip", color, TextureOptions::LINEAR))
            }
        }
        self.clip_mask_stamp = stamp;
    }

    /// J 键循环剪切警告：关 → 高光 → 两开 → 阴影 → 关（LR 的口径）。
    /// 置 stamp 为 MAX 强制下一帧重算。
    pub fn clip_cycle(&mut self) {
        match (self.clip_highlights, self.clip_shadows) {
            (false, false) => self.clip_highlights = true,
            (true, false) => self.clip_shadows = true,
            (true, true) => self.clip_highlights = false,
            (false, true) => {
                self.clip_shadows = false;
            }
        }
        self.clip_mask_stamp = u64::MAX;
    }

    pub fn plugin_start(&mut self, ctx: &Context, id: &str) -> Result<String, String> {
        use aevum_core::plugin_host::Host;
        // 一次只跑一个：画布上盖着的插件结果只有一个，两个任务抢同一张纹理
        // 谁后到谁赢，用户根本看不懂。
        if self.plugin_job.is_some() {
            return Err("已有插件在运行".to_owned());
        }
        // 先把要用的东西抄出来，免得同时持有 `registry` 的不可变借与自己的可变借。
        let (name, plugin_id, entry, params) = {
            let p = self
                .registry
                .get(id)
                .ok_or_else(|| format!("找不到插件：{id}"))?;
            let params: BTreeMap<String, f64> = p
                .manifest
                .params
                .item
                .iter()
                .map(|s| (s.key.clone(), s.default))
                .collect();
            (
                p.manifest.plugin.name.clone(),
                p.manifest.plugin.id.clone(),
                p.entry_path(),
                params,
            )
        };
        let entry = entry.ok_or_else(|| format!("{name} 由宿主内部实现，还没接上"))?;
        // 512 分块只读，整份克隆最省心：别让工作线程借用 self，
        // 也不跟预览缓冲复用那条路抢同一块内存。
        let (rgb, size) = self
            .preview_rgb
            .as_ref()
            .ok_or_else(|| "还没有可用的预览图".to_owned())?
            .clone();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cols = size[0].div_ceil(aevum_core::plugin_host::TILE);
        let rows = size[1].div_ceil(aevum_core::plugin_host::TILE);
        self.plugin_progress = (0, cols * rows);
        self.status = format!("{name} 运行中…");
        self.plugin_job = Some(PluginJob {
            id: plugin_id.clone(),
            name: name.clone(),
            started: Instant::now(),
            rx,
            cancel: Arc::clone(&cancel),
        });
        thread::spawn(move || {
            match Host::start(&entry, &plugin_id, &params) {
                Ok(mut host) => {
                    // 取消旗桥进进度回调：宿主在每块收发之间问一次"还要不要继续"。
                    let result = host.apply_progress(size[0], size[1], &rgb, |done, total| {
                        let _ = tx.send(PluginMsg::Progress(done, total));
                        !cancel.load(Ordering::Relaxed)
                    });
                    // 无论成败都要收尾：失败时留下的进程会一直占着管道。
                    host.shutdown();
                    let _ = tx.send(PluginMsg::Done(result));
                }
                Err(e) => {
                    let _ = tx.send(PluginMsg::Done(Err(e)));
                }
            }
        });
        // 进度条要动起来：空闲时 egui 不会自己重绘。
        ctx.request_repaint_after(Duration::from_millis(50));
        Ok(name)
    }

    /// 每帧收一次插件任务的进度与结果。挂在 [`AppState::poll`] 里，不用单独调。
    pub fn plugin_poll(&mut self, ctx: &Context) {
        let Some(job) = self.plugin_job.as_mut() else {
            return;
        };
        let mut finished = None;
        loop {
            match job.rx.try_recv() {
                Ok(msg) => match msg {
                    PluginMsg::Progress(done, total) => {
                        // 只进不退：迟到的旧心跳直接丢。
                        if done >= self.plugin_progress.0 {
                            self.plugin_progress = (done, total);
                        }
                    }
                    PluginMsg::Done(result) => {
                        finished = Some(result);
                        break;
                    }
                },
                // 通道空了但没挂：还在跑，等下一帧。
                Err(TryRecvError::Empty) => break,
                // 发送端没了且队列空：插件线程死透了也没人发 Done——之前
                // 这里直接当"还在跑"，状态栏的进度与取消永远挂着收不了场。
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("插件进程意外退出（没有返回结果）".to_owned()));
                    break;
                }
            }
        }
        let Some(result) = finished else {
            // 还在跑：保一个低频心跳，进度条与取消按钮才动得起来。
            ctx.request_repaint_after(Duration::from_millis(60));
            return;
        };
        let job = self.plugin_job.take().expect("上面刚确认过它在飞");
        self.plugin_progress = (0, 0);
        match result {
            Ok(out) => {
                let image =
                    ColorImage::from_rgb([out.width as usize, out.height as usize], &out.pixels);
                self.plugin_result =
                    Some(ctx.load_texture("plugin.result", image, TextureOptions::LINEAR));
                self.plugin_stamp = self.preview_stamp;
                self.status = format!(
                    "{} · {:.0} ms",
                    job.name,
                    out.elapsed.as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                if e.contains(aevum_core::plugin_host::ERR_CANCELLED) {
                    // 用户取消不是失败：只清状态，不进错误横幅、不计崩溃。
                    self.status = format!("{} 已取消", job.name);
                } else {
                    let crashes = self.plugin_crashes.entry(job.id).or_insert(0);
                    *crashes += 1;
                    let hint = if *crashes >= 3 {
                        "（连续失败多次，可在插件市场里停用它）"
                    } else {
                        ""
                    };
                    self.error = Some(format!("{}：{e}{hint}", job.name));
                }
            }
        }
    }

    /// 用户点了取消：立旗就回。剩下的交给工作线程 —— 它在下一块边界退出、
    /// 把子进程收尸干净，[`AppState::plugin_poll`] 收到回包后清掉在飞状态。
    pub fn plugin_cancel(&mut self) {
        if let Some(job) = &self.plugin_job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = format!("正在取消 {}…", job.name);
        }
    }

    /// 有没有插件正在后台跑。在飞期间再点别的插件只会得到一句提示。
    pub fn plugin_running(&self) -> bool {
        self.plugin_job.is_some()
    }

    /// 当前偏好的快照（外观 + 布局 + 字号），落盘与回填都用它。
    pub fn settings(&self) -> crate::settings::Settings {
        crate::settings::Settings {
            appearance: self.appearance,
            layout: self.layout,
            font_level: self.font_level,
        }
    }

    /// 启动时把落盘的偏好灌回来。不动任何图像状态，只改外观与布局。
    pub fn adopt_settings(&mut self, s: crate::settings::Settings) {
        self.appearance = s.appearance;
        self.layout = s.layout;
        self.font_level = s.font_level.min(3);
    }

    pub fn new() -> Self {
        let (tx, rx) = channel();
        let (control_tx, control_rx) = channel();
        let mut s = Self {
            seq: None,
            metrics: Vec::new(),
            issues: Vec::new(),
            analysis: None,
            corr: Vec::new(),
            corr_rgb: Vec::new(),
            def_means: Vec::new(),
            exif: Vec::new(),
            selected: 0,
            module: Module::Sequence,
            tool: Tool::Hand,
            dock_tab: DockTab::Adjust,
            collapsed: HashSet::new(),
            status: "打开一个帧文件夹，或把文件夹拖进窗口".to_owned(),
            error: None,
            sidecar_hint: None,
            measuring: false,
            params: GradeParams::default(),
            preset: "原始".to_owned(),
            auto_note: None,
            auto_probe: None,
            def_window: 15.0,
            def_strength: 100.0,
            def_multi: false,
            def_channels: false,
            aspect: Aspect::Native,
            grid: false,
            cross: false,
            split: false,
            split_pos: 0.5,
            film_h: None,
            zoom: 1.0,
            fit_scale: 1.0,
            pan: Vec2::ZERO,
            playing: false,
            last_play: Instant::now(),
            font_level: 0,
            texture: None,
            texture_orig: None,
            preview_size: [0, 0],
            preview_stamp: 0,
            preview_dirty: false,
            preview_inflight: false,
            preview_dragging: false,
            preview_owes_full: false,
            preview_has_orig: false,
            thumbs: HashMap::new(),
            thumb_pending: HashSet::new(),
            scroll_to_selected: false,
            ratings: HashMap::new(),
            flags: HashMap::new(),
            labels: HashMap::new(),
            keywords: HashMap::new(),
            stacks: Vec::new(),
            meta: project::Metadata::default(),
            lib_roots: Vec::new(),
            lib_sel: None,
            lib_sel_file: None,
            lib_filter: aevum_core::library::Filter::default(),
            lib_checked: std::collections::BTreeSet::new(),
            lib_anchor: None,
            lib_stack_open: HashSet::new(),
            lib_entries: Vec::new(),
            lib_cols: Vec::new(),
            lib_col: None,
            lib_col_name: String::new(),
            lib_keyword: String::new(),
            lib_meta: project::Metadata::default(),
            lib_meta_root: None,
            lib_meta_open: false,
            lib_thumbs: HashMap::new(),
            lib_thumb_pending: HashSet::new(),
            history: History::new(),
            hist: None,
            show_dock: true,
            show_left: true,
            show_strip: true,
            snapshots: Vec::new(),
            appearance: crate::theme::Appearance::default(),
            layout: crate::layout::Layout::default(),
            look_open: false,
            registry: {
                // 内置清单先立住，再叠目录里扫到的：同 id 以文件为准，
                // 用户从市场更新过的版本不该被编译进二进制的那份盖回去。
                let root = crate::settings::plugin_dir();
                aevum_core::plugin::Registry::default()
                    .with_builtin(aevum_core::plugin::builtin(), &root)
                    .merge(aevum_core::plugin::Registry::scan(
                        &root,
                        aevum_core::plugin::Source::User,
                    ))
            },
            market_open: false,
            preview_rgb: None,
            clip_highlights: false,
            clip_shadows: false,
            clip_mask: None,
            clip_mask_stamp: u64::MAX,
            plugin_result: None,
            plugin_stamp: 0,
            plugin_job: None,
            plugin_progress: (0, 0),
            plugin_crashes: HashMap::new(),
            market_filter: None,
            market_query: String::new(),
            dock_scroll_end: false,
            dock_focus: None,
            show_film: true,
            // 默认关：胶片条那一行要留给缩略图。波形是去闪时的检视工具，
            // 常年摊在底部会把 176px 的胶片条挤成 44px 的窄缝。
            show_curves: false,
            rulers: false,
            about_open: false,
            keyframes: BTreeMap::new(),
            copies: Vec::new(),
            export_copy: None,
            copy_name: String::new(),
            hdr_around: 1,
            hdr_last: None,
            hdr_error: None,
            pano_around: 1,
            pano_last: None,
            pano_error: None,
            resolution: None,
            source_note: None,
            mask_sel: None,
            mask_comp: 0,
            mask_drag: None,
            mask_paint: None,
            brush_radius: 6.0,
            brush_hardness: 70.0,
            brush_flow: 100.0,
            brush_erase: false,
            mask_pick: false,
            mask_pick_img: None,
            mask_geo_cache: Default::default(),
            curve_lut_cache: None,
            hist_version: 0,
            hist_mesh_cache: None,
            corr_version: 0,
            defl_curve_cache: None,
            lens_profile_note: String::new(),
            lens_profile_saved: None,
            dcp_name: String::new(),
            dcp_note: String::new(),
            lcp_note: String::new(),
            user_crop: None,
            rotate: 0.0,
            crop_drag: None,
            motion_drag: None,
            motion_picking: false,
            motion_box: MotionBox::End,
            hsl_band: 0,
            curve_chan: 0,
            curve_grab: None,
            levels_grab: None,
            retime_grab: None,
            output_w: 3840,
            output_h: 2160,
            output_fps: 24.0,
            codec: "HEVC".to_owned(),
            export_busy: false,
            last_saved: None,
            headless: false,
            control_enabled: true,
            align_report: None,
            align_busy: false,
            align_estimator: Estimator::Template,
            align_ref: 0,
            align_preview: true,
            align_msg: None,
            align_done: Arc::new(AtomicUsize::new(0)),
            render_mode: OutputMode::Video,
            render_format: StillFormat::Png8,
            render_quality: 90,
            render_keep_exif: false,
            render_xmp_sidecar: false,
            render_sharpen_for: OutputSharpen::default(),
            render_sharpen_strength: SharpenStrength::default(),
            render_color_space: ColorSpace::default(),
            render_watermark: Watermark::default(),
            retime: Retime::default(),
            motion: Motion::default(),
            render_crf: 18,
            render_preset: "medium".to_owned(),
            render_audio: None,
            render_audio_opts: AudioOptions::default(),
            audio_info: None,
            audio_probe_err: None,
            render_out_dir: PathBuf::new(),
            render_video_out: PathBuf::new(),
            render_keep_frames: false,
            render_resume: false,
            export_skipped_shared: Arc::new(AtomicUsize::new(0)),
            export_stage: None,
            export_done: Arc::new(AtomicUsize::new(0)),
            export_total: Arc::new(AtomicUsize::new(0)),
            export_stage_shared: Arc::new(AtomicU8::new(0)),
            cancel_export: Arc::new(AtomicBool::new(false)),
            merge_busy: false,
            merge_kind: None,
            merge_result: None,
            merge_error: None,
            merge_done: Arc::new(AtomicUsize::new(0)),
            merge_total: Arc::new(AtomicUsize::new(0)),
            cancel_merge: Arc::new(AtomicBool::new(false)),
            qa_busy: false,
            qa_findings: Vec::new(),
            qa_run: false,
            qa_error: None,
            qa_sig: 3.0,
            qa_done: Arc::new(AtomicUsize::new(0)),
            qa_total: Arc::new(AtomicUsize::new(0)),
            cancel_qa: Arc::new(AtomicBool::new(false)),
            queue: Vec::new(),
            queue_path: queue::default_path(),
            queue_busy: false,
            queue_current: None,
            queue_done: Arc::new(AtomicUsize::new(0)),
            queue_total: Arc::new(AtomicUsize::new(0)),
            queue_stage: Arc::new(AtomicU8::new(0)),
            cancel_queue: Arc::new(AtomicBool::new(false)),
            last_auto_save: Instant::now(),
            saved_snap: None,
            saved_stamp: None,
            reload_token: 0,
            tx,
            rx,
            control: None,
            ai_input: String::new(),
            ai_log: Vec::new(),
            control_tx,
            control_rx,
        };
        // 历史先落下第 0 步：不这么做，会话里第一次改动就没有「之前」可以回。
        s.history_reset("空状态");
        // 图库清单跨会话常驻：启动就载入，编目等展开目录时再扫。
        let store = aevum_core::library::load(&aevum_core::library::default_path());
        s.lib_roots = store.roots;
        s.lib_cols = store.collections;
        s.lib_entries = vec![Vec::new(); s.lib_roots.len()];
        s
    }

    pub fn frame_count(&self) -> usize {
        self.seq.as_ref().map(|s| s.len()).unwrap_or(0)
    }

    /// 素材是不是只有一张照片。序列级的功能（对齐、去闪）靠邻居帧才有意义。
    pub fn is_single(&self) -> bool {
        self.frame_count() == 1
    }

    pub fn offset_at(&self, i: usize) -> f32 {
        self.corr.get(i).copied().unwrap_or(0.0)
    }

    /// 这一帧的去闪补偿，直接喂给显影链。
    ///
    /// 单通道时 `chan` 全零，与只补亮度的老口径逐位一致；开分通道后 `add` 吃三
    /// 通道均值、`chan` 只留色差，两者相加正好还原该通道原本的偏移。
    pub fn ev_at(&self, i: usize) -> Ev {
        Ev {
            add: self.offset_at(i),
            chan: self.corr_rgb.get(i).copied().unwrap_or([0.0; 3]),
        }
    }

    pub fn open(&mut self, ctx: &Context, path: PathBuf) {
        self.open_paths(ctx, std::slice::from_ref(&path));
    }

    /// 拖进来或选进来的一堆路径：单个可以是目录也可以是照片，多个按照片列表当素材。
    ///
    /// 单张照片不能退化成「打开它所在的整个文件夹」——那样用户挑的那张会被邻居淹没，
    /// 护带和成片还会写进一个他根本没选的目录。
    pub fn open_paths(&mut self, ctx: &Context, paths: &[PathBuf]) {
        let seq = match paths {
            [] => return,
            [only] => Sequence::open_any(only),
            many => Sequence::open_files(many).or_else(|e| match e {
                SequenceError::NoUsableFile => match many.iter().find(|p| p.is_dir()) {
                    Some(dir) => Sequence::open(dir),
                    None => Err(SequenceError::NoUsableFile),
                },
                other => Err(other),
            }),
        };
        let seq = match seq {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(format!("{e}"));
                return;
            }
        };
        self.reload_token += 1;
        let token = self.reload_token;
        let n = seq.len();
        let single = seq.is_single();
        let first = seq
            .frames
            .first()
            .map(|f| f.path.display().to_string())
            .unwrap_or_default();

        let root = seq.root.clone();
        let seq_name = seq.display_name();
        self.seq = Some(seq);
        self.unload_jobs();
        self.align_report = None;
        self.align_busy = false;
        self.align_msg = None;
        self.align_ref = 0;
        self.align_preview = true;
        self.align_done.store(0, Ordering::Relaxed);
        self.render_mode = OutputMode::Video;
        self.render_out_dir = root.join("成片");
        self.render_video_out = root.join(format!("{seq_name}_成片.mp4"));
        self.render_audio = None;
        self.audio_info = None;
        self.audio_probe_err = None;
        self.metrics = vec![None; n];
        self.exif.clear();
        self.issues.clear();
        self.analysis = None;
        self.corr.clear();
        self.corr_rgb.clear();
        self.def_means.clear();
        self.selected = 0;
        self.texture = None;
        self.texture_orig = None;
        self.reset_session();
        // 换序列等于换画幅：上一个序列的取景框和拉直角度对新尺寸没有意义。
        self.user_crop = None;
        self.rotate = 0.0;
        self.crop_drag = None;
        self.saved_snap = None;
        self.saved_stamp = None;
        self.ratings.clear();
        self.flags.clear();
        self.labels.clear();
        // 关键词、堆叠与元数据都跟着工程走：新目录没有工程文件时也得清空，否则上一个
        // 序列的词和版权信息会赖在这儿，等自动保存写进新目录的工程里。
        self.keywords.clear();
        self.stacks.clear();
        self.meta = project::Metadata::default();
        self.hist = None;
        self.keyframes.clear();
        self.copies.clear();
        self.export_copy = None;
        self.hdr_last = None;
        self.hdr_error = None;
        self.pano_last = None;
        self.pano_error = None;
        self.merge_kind = None;
        self.merge_result = None;
        self.merge_error = None;
        if self.merge_busy {
            // 换序列时正在跑的合并作废：回包带 token，迟到那条会在 poll 里被丢掉。
            self.cancel_merge.store(true, Ordering::SeqCst);
            self.merge_busy = false;
        }
        // 体检清单是这一段的诊断结论，换序列必须清空，否则上一段的坏帧还挂在表里。
        self.qa_findings.clear();
        self.qa_run = false;
        self.qa_error = None;
        if self.qa_busy {
            self.cancel_qa.store(true, Ordering::SeqCst);
            self.qa_busy = false;
        }
        self.params = GradeParams::default();
        self.preset = "原始".to_owned();
        self.auto_note = None;
        self.sidecar_hint = None;
        self.reset_mask_state();
        self.zoom = 1.0;
        self.fit_scale = 1.0;
        self.pan = Vec2::ZERO;
        self.playing = false;
        self.scroll_to_selected = true;
        self.error = None;
        self.measuring = true;
        self.resolution = None;
        self.source_note = None;
        self.status = if single {
            "正在读取这张照片…".to_owned()
        } else {
            format!("正在测量 {n} 帧…")
        };
        self.module = Module::Develop;
        self.load_project(&root);
        // 边车只提示不套用：检测到同目录 .xmp 时状态栏给一键应用（架构师建议、本轮采纳）。
        self.detect_sidecar_hint();
        // RAW 首次导入（工程文件不存在）时套默认锐化/降噪；有工程文件的用户参数不动。
        self.apply_raw_import_defaults(&root);
        // 工程恢复完才是这盘棋的起点：历史从头记，撤销不会把用户带回上一个序列。
        // 默认参数挂在 history_reset 之前，进初始历史、可撤销。
        self.history_reset("打开序列");

        let paths = self.seq.as_ref().map(|s| s.paths()).unwrap_or_default();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let cache_root = root.clone();
        thread::spawn(move || {
            let exif = aevum_core::exif::read_many(&paths);
            // 竖拍帧的显示尺寸是旋转之后的：EXIF 方向 5~8 交换长短边。
            // `metrics::decode` 对 RAW 同样会转正，所以这条交换三条分支都要吃，
            // 否则竖拍 RAW 的状态栏尺寸和画面差一个 90°。
            let orientation = exif.first().map_or(1, |e| e.orientation);
            let rotated = |(w, h): (u32, u32)| {
                if (5..=8).contains(&orientation) {
                    (h, w)
                } else {
                    (w, h)
                }
            };
            // `image` 库不认 CFA 容器，RAW/DNG 的宽高只能问我们自己的解析器；
            // 顺带把「传感器还是机身预览」这条口径带出来，信息面板用它标注保真度。
            let (resolution, source_note) = match paths.first() {
                None => (None, None),
                Some(p) if aevum_core::raw::is_raw_name(p) => match aevum_core::raw::info(p) {
                    Ok(i) => (
                        Some(rotated((i.width, i.height))),
                        Some(format!("{} · {} 位", i.path.short(), i.bits)),
                    ),
                    Err(_) => (None, None),
                },
                Some(p) if aevum_core::dng::is_named_dng(p) => match aevum_core::dng::info(p) {
                    Ok(i) => (
                        Some(rotated((i.width, i.height))),
                        Some(format!("DNG · 传感器直解 · {} 位", i.bits)),
                    ),
                    Err(_) => (None, None),
                },
                Some(p) => (image::image_dimensions(p).ok().map(rotated), None),
            };
            let (metrics, def_means, issues) =
                aevum_core::measures::measure_sequence(&cache_root, &paths, 512);
            let _ = tx.send(Msg::Loaded(Box::new(Loaded {
                token,
                metrics,
                def_means,
                issues,
                resolution,
                source_note,
                exif,
            })));
            ctx2.request_repaint();
        });
        self.status = if single {
            format!("单张 · {first}")
        } else {
            format!("{n} 帧 · 首帧 {first}")
        };
    }

    /// 关闭当前序列，回到空状态；不碰磁盘上的素材。
    pub fn close(&mut self) {
        self.save_project();
        self.unload_jobs();
        self.reload_token += 1;
        self.align_report = None;
        self.align_msg = None;
        self.align_ref = 0;
        self.render_out_dir.clear();
        self.render_video_out.clear();
        self.seq = None;
        self.metrics.clear();
        self.exif.clear();
        self.issues.clear();
        self.analysis = None;
        self.corr.clear();
        self.corr_rgb.clear();
        self.def_means.clear();
        self.selected = 0;
        self.texture = None;
        self.texture_orig = None;
        self.reset_session();
        // 换序列等于换画幅：上一个序列的取景框和拉直角度对新尺寸没有意义。
        self.user_crop = None;
        self.rotate = 0.0;
        self.crop_drag = None;
        self.saved_snap = None;
        self.saved_stamp = None;
        self.ratings.clear();
        self.flags.clear();
        self.labels.clear();
        self.keywords.clear();
        self.stacks.clear();
        self.meta = project::Metadata::default();
        self.hist = None;
        self.keyframes.clear();
        self.copies.clear();
        self.export_copy = None;
        self.hdr_last = None;
        self.hdr_error = None;
        self.pano_last = None;
        self.pano_error = None;
        self.merge_kind = None;
        self.merge_result = None;
        self.merge_error = None;
        if self.merge_busy {
            // 换序列时正在跑的合并作废：回包带 token，迟到那条会在 poll 里被丢掉。
            self.cancel_merge.store(true, Ordering::SeqCst);
            self.merge_busy = false;
        }
        // 体检清单是这一段的诊断结论，换序列必须清空，否则上一段的坏帧还挂在表里。
        self.qa_findings.clear();
        self.qa_run = false;
        self.qa_error = None;
        if self.qa_busy {
            self.cancel_qa.store(true, Ordering::SeqCst);
            self.qa_busy = false;
        }
        self.params = GradeParams::default();
        self.preset = "原始".to_owned();
        self.auto_note = None;
        self.sidecar_hint = None;
        self.reset_mask_state();
        self.resolution = None;
        self.source_note = None;
        self.measuring = false;
        self.playing = false;
        self.zoom = 1.0;
        self.fit_scale = 1.0;
        self.pan = Vec2::ZERO;
        self.module = Module::Sequence;
        self.last_saved = None;
        self.error = None;
        self.status = "打开一个帧文件夹，或把文件夹拖进窗口".to_owned();
        self.history_reset("空状态");
    }

    /// 会话级清理：close 与换序列两条路的公共段，一处维护（性能重构设计 E7）。
    /// 字段清单收编自原 open 与 close 里两段**逐字相同**的块（收编前 diff 为零，
    /// 见 docs/基准-改前-2026-10-30.md 的核对记录）。
    fn reset_session(&mut self) {
        // 插件结果与 CPU 像素都是"这一张图"的衍生物，换序列一并作废；
        // 剪切遮罩跟着 preview 走，也在这里清掉。（这段清单在 close 与
        // 换序列两条路上重复出现——后续应抽成 reset_session 一处维护。）
        self.plugin_result = None;
        self.plugin_stamp = 0;
        self.plugin_progress = (0, 0);
        self.preview_rgb = None;
        self.clip_mask = None;
        self.clip_mask_stamp = u64::MAX;
        self.mask_paint = None;
        self.preview_size = [0, 0];
        self.preview_dirty = false;
        self.preview_inflight = false;
        self.thumbs.clear();
        self.thumb_pending.clear();
        // 四类缓存跟着会话走：换序列/关图整体清空（指纹自校验之外的保守兜底）。
        self.mask_geo_cache = Default::default();
        self.curve_lut_cache = None;
        self.hist_mesh_cache = None;
        self.defl_curve_cache = None;
    }

    /// 蒙版五连复位（性能重构设计 E8）：选中/形状/拖拽/取色态/取色像素
    /// 是一组共进退的界面状态，收成一处，避免手抄漏字段造成跨会话泄漏。
    fn reset_mask_state(&mut self) {
        self.mask_sel = None;
        self.mask_comp = 0;
        self.mask_drag = None;
        self.mask_pick = false;
        self.mask_pick_img = None;
    }

    pub fn rating_at(&self, i: usize) -> u8 {
        self.ratings.get(&i).copied().unwrap_or(0)
    }

    pub fn set_rating(&mut self, i: usize, stars: u8) {
        let stars = stars.min(5);
        if stars == 0 {
            self.ratings.remove(&i);
        } else {
            self.ratings.insert(i, stars);
        }
        history::note("rating", format!("帧 {i:04} 评 {stars} 星", i = i + 1));
    }

    pub fn selected_rating(&self) -> u8 {
        self.rating_at(self.selected)
    }

    pub fn rate_selected(&mut self, stars: u8) {
        self.set_rating(self.selected, stars);
    }

    pub fn flag_at(&self, i: usize) -> i8 {
        self.flags.get(&i).copied().unwrap_or(0)
    }

    pub fn set_flag(&mut self, i: usize, flag: i8) {
        if flag == 0 {
            self.flags.remove(&i);
        } else {
            self.flags.insert(i, flag.signum());
        }
        history::note(
            "flag",
            format!(
                "帧 {i:04} {}",
                match flag {
                    1 => "选用",
                    -1 => "排除",
                    _ => "取消标记",
                },
                i = i + 1
            ),
        );
    }

    pub fn selected_flag(&self) -> i8 {
        self.flag_at(self.selected)
    }

    pub fn set_selected_flag(&mut self, flag: i8) {
        let i = self.selected;
        self.set_flag(i, flag);
    }

    pub fn label_at(&self, i: usize) -> u8 {
        self.labels.get(&i).copied().unwrap_or(0)
    }

    pub fn set_label(&mut self, i: usize, label: u8) {
        if label == 0 {
            self.labels.remove(&i);
        } else {
            self.labels.insert(i, label.min(4));
        }
        history::note(
            "label",
            format!(
                "帧 {i:04} 色标「{name}」",
                i = i + 1,
                name = LABEL_NAMES[label.min(4) as usize]
            ),
        );
    }

    pub fn selected_label(&self) -> u8 {
        self.label_at(self.selected)
    }

    pub fn set_selected_label(&mut self, label: u8) {
        let i = self.selected;
        self.set_label(i, label);
    }

    pub fn rated_count(&self) -> usize {
        self.ratings.values().filter(|v| **v > 0).count()
    }

    pub fn set_frame(&mut self, i: usize) {
        let n = self.frame_count();
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        if i != self.selected {
            self.selected = i;
            self.preview_dirty = true;
            self.scroll_to_selected = true;
        }
    }

    pub fn step(&mut self, delta: i64) {
        let n = self.frame_count() as i64;
        if n == 0 {
            return;
        }
        let i = (self.selected as i64 + delta).clamp(0, n - 1);
        self.set_frame(i as usize);
    }

    pub fn toggle_play(&mut self) {
        if self.frame_count() == 0 {
            return;
        }
        self.playing = !self.playing;
        self.last_play = Instant::now();
        if !self.playing {
            self.preview_dirty = true;
        }
    }

    pub fn tick(&mut self, ctx: &Context) {
        if self.playing {
            let n = self.frame_count();
            if n == 0 {
                self.playing = false;
            } else {
                let dt = 1.0 / self.output_fps.max(1.0);
                let elapsed = self.last_play.elapsed().as_secs_f32();
                if elapsed >= dt {
                    let steps = (elapsed / dt).floor().max(1.0) as usize;
                    self.last_play = Instant::now();
                    let next = (self.selected + steps) % n;
                    self.selected = next;
                    self.scroll_to_selected = true;
                    self.preview_dirty = true;
                    // 播放时画布直接吃 128px 缩略图：提前把前面 48 帧取回来，
                    // 否则指针跑到哪儿才现解到哪儿，画面就是一串空位闪。
                    // 只在上一批落地之后再要下一批，一次一批而不是一帧一个线程。
                    if self.thumb_pending.is_empty() {
                        self.ensure_thumbs(ctx, next, next + PLAY_THUMB_AHEAD);
                    }
                }
                ctx.request_repaint_after(Duration::from_secs_f32(dt * 0.5));
            }
        }
    }

    pub fn poll(&mut self, ctx: &Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Loaded(loaded) => {
                    if loaded.token != self.reload_token {
                        continue;
                    }
                    let Loaded {
                        metrics,
                        def_means,
                        issues,
                        resolution,
                        source_note,
                        exif,
                        ..
                    } = *loaded;
                    self.metrics = metrics;
                    self.def_means = def_means;
                    self.issues = issues;
                    self.resolution = resolution;
                    self.source_note = source_note;
                    self.exif = exif;
                    // 首帧 EXIF 到手就能匹配镜头配置文件：命中折进参数，
                    // 预览标脏，没命中留一行说明。
                    self.match_lens_profile();
                    self.measuring = false;
                    self.recompute_deflicker();
                    self.preview_dirty = true;
                    let valid = self.metrics.iter().filter(|m| m.is_some()).count();
                    self.status = format!(
                        "{} 帧 · 可读 {valid} · 缺帧 {}",
                        self.frame_count(),
                        self.issues.len()
                    );
                }
                Msg::Preview(p) => {
                    self.preview_inflight = false;
                    let PreviewMsg {
                        stamp,
                        sketch,
                        graded,
                        original,
                        size,
                        hist,
                    } = *p;
                    if stamp == self.preview_stamp {
                        self.hist = Some(hist);
                        // 新预览落地：直方图 Mesh 缓存换版（键之一，见 HistMeshCache）。
                        self.hist_version += 1;
                        // 吸管取色要读像素：取色期间留一份，退出取色就清。
                        if self.mask_pick {
                            self.mask_pick_img = Some(graded.clone());
                        } else {
                            self.mask_pick_img = None;
                        }
                        // 插件要作用在"用户看到的这一张"上，所以留一份 CPU 像素。
                        // 底图一变，上一次的插件结果就作废 —— 留着会让用户对着
                        // 一张旧结果的图继续调参数。
                        if !sketch {
                            // 复用同一块缓冲：预览一帧就是 8 MB，每帧重新分配会让
                            // 分配器在拖动松手那一串全量渲染里一直忙。
                            let mut rgb = self
                                .preview_rgb
                                .take()
                                .map(|(mut v, _)| {
                                    v.clear();
                                    v
                                })
                                .unwrap_or_default();
                            rgb.reserve(graded.pixels.len() * 3);
                            for c in &graded.pixels {
                                rgb.extend_from_slice(&[c.r(), c.g(), c.b()]);
                            }
                            self.preview_rgb = Some((rgb, size));
                            // 底图换了一张，旧结果就没意义了。同一张重传不算。
                            if self.plugin_result.is_some() && stamp != self.plugin_stamp {
                                self.plugin_result = None;
                            }
                        }
                        // 纹理按名字复用：每帧新建一次 TextureHandle 就要在 GPU 上
                        // 分配/释放一次，`set` 只重传像素。
                        match &mut self.texture {
                            Some(tex) => tex.set(graded, TextureOptions::LINEAR),
                            None => {
                                self.texture = Some(ctx.load_texture(
                                    "preview.graded",
                                    graded,
                                    TextureOptions::LINEAR,
                                ));
                            }
                        }
                        self.texture_orig = if self.split {
                            original.map(|img| {
                                ctx.load_texture("preview.original", img, TextureOptions::LINEAR)
                            })
                        } else {
                            None
                        };
                        self.preview_has_orig = self.texture_orig.is_some();
                        self.preview_size = size;
                        // 剪切警告跟着全量帧走：草图帧不重算（见 rebuild 注释）。
                        if !sketch {
                            self.rebuild_clip_mask(ctx, stamp);
                        }
                        // 草图欠一张全量：还按着就等松手那一帧补，已经松手就当场补。
                        self.preview_owes_full = sketch;
                        if sketch && !self.preview_dragging {
                            self.preview_dirty = true;
                        }
                    }
                }
                Msg::Thumbs(items) => {
                    for t in items {
                        self.thumb_pending.remove(&t.index);
                        let tex = ctx.load_texture(
                            format!("thumb.{}", t.index),
                            t.image,
                            TextureOptions::LINEAR,
                        );
                        self.thumbs.insert(
                            t.index,
                            Thumb {
                                texture: tex,
                                size: t.size,
                            },
                        );
                    }
                }
                Msg::LibThumbs(items) => {
                    for t in items {
                        self.lib_thumb_pending.remove(&t.path);
                        let tex = ctx.load_texture(
                            format!("lib-thumb.{}", t.path.display()),
                            t.image,
                            TextureOptions::LINEAR,
                        );
                        self.lib_thumbs.insert(
                            t.path,
                            Thumb {
                                texture: tex,
                                size: t.size,
                            },
                        );
                    }
                }
                Msg::Saved(path) => {
                    self.export_busy = false;
                    self.last_saved = Some(path.clone());
                    self.status = format!("已保存 {path}");
                }
                Msg::Error(e) => {
                    self.preview_inflight = false;
                    self.export_busy = false;
                    self.error = Some(e);
                }
                Msg::Aligned { token, report } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.align_busy = false;
                    self.align_msg = Some(format!(
                        "对齐完成 · {} 帧 · 裁切 {}×{} · {:.1}s · 异常 {}",
                        report.offsets.len(),
                        report.crop.w,
                        report.crop.h,
                        report.elapsed_secs,
                        report.issues.len()
                    ));
                    self.status = self.align_msg.clone().unwrap_or_default();
                    self.align_report = Some(report);
                    self.preview_dirty = true;
                }
                Msg::AlignFailed(e) => {
                    self.align_busy = false;
                    self.align_msg = Some(format!("对齐失败：{e}"));
                    self.error = Some(format!("对齐失败：{e}"));
                }
                Msg::Exported { token, path } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.export_busy = false;
                    self.export_stage = None;
                    self.last_saved = Some(path.display().to_string());
                    let skipped = self.export_skipped();
                    self.status = if skipped > 0 {
                        format!("已导出 {}（续跑跳过 {skipped} 帧）", path.display())
                    } else {
                        format!("已导出 {}", path.display())
                    };
                }
                Msg::ExportFailed(e) => {
                    self.export_busy = false;
                    self.export_stage = None;
                    if e.contains("已取消") {
                        self.status = "导出已取消".to_owned();
                    } else {
                        self.error = Some(format!("导出失败：{e}"));
                    }
                }
                Msg::Merged {
                    token,
                    kind,
                    result,
                } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.merge_busy = false;
                    self.merge_kind = None;
                    // 面板那两行回显按任务种类分头落账：HDR 的失败不该顶掉全景的结果。
                    let (last, err) = match kind {
                        MergeKind::Hdr => (&mut self.hdr_last, &mut self.hdr_error),
                        MergeKind::Pano => (&mut self.pano_last, &mut self.pano_error),
                    };
                    match result {
                        Ok(v) => {
                            let path = v["path"].as_str().unwrap_or_default().to_owned();
                            *last = Some(path.clone());
                            *err = None;
                            self.merge_result = Some(v);
                            self.merge_error = None;
                            self.status = format!("{}完成：{path}", kind.cn());
                        }
                        Err(e) => {
                            self.merge_result = None;
                            self.merge_error = Some(e.clone());
                            *last = None;
                            *err = Some(e.clone());
                            if e.contains("已取消") {
                                self.status = format!("{}已取消", kind.cn());
                            } else {
                                self.status = format!("{}失败", kind.cn());
                                self.error = Some(format!("{}失败：{e}", kind.cn()));
                            }
                        }
                    }
                }
                Msg::Checked { token, result } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.qa_busy = false;
                    match result {
                        Ok(v) => {
                            let bad = v.len();
                            self.qa_findings = v;
                            self.qa_run = true;
                            self.qa_error = None;
                            self.status = if bad == 0 {
                                "体检完成：没发现问题".to_owned()
                            } else {
                                format!("体检完成：发现 {bad} 处")
                            };
                        }
                        Err(e) => {
                            self.qa_error = Some(e.clone());
                            if e.contains("已取消") {
                                self.status = "体检已取消".to_owned();
                            } else {
                                self.error = Some(format!("体检失败：{e}"));
                            }
                        }
                    }
                }
                Msg::QueueRow {
                    index,
                    status,
                    note,
                } => {
                    if let Some(item) = self.queue.get_mut(index) {
                        item.status = status;
                        item.note = note;
                        self.queue_current = Some(index);
                        self.status = format!(
                            "队列第 {}/{} 段：{}",
                            index + 1,
                            self.queue.len(),
                            status.label()
                        );
                    }
                    self.save_queue();
                }
                Msg::QueueFinished {
                    ok,
                    failed,
                    cancelled,
                } => {
                    self.queue_busy = false;
                    self.queue_current = None;
                    self.queue_stage.store(0, Ordering::Relaxed);
                    self.status = if cancelled {
                        format!("队列已取消：完成 {ok} 段，失败 {failed} 段")
                    } else if failed > 0 {
                        format!("队列完成：成功 {ok} 段，失败 {failed} 段")
                    } else {
                        format!("队列完成：{ok} 段")
                    };
                    if failed > 0 && !cancelled {
                        self.error = Some(format!("队列有 {failed} 段没渲成，逐行查看原因"));
                    }
                    self.save_queue();
                }
            }
        }

        // 插件任务：收进度、收结果。空闲时是空操作。
        self.plugin_poll(ctx);

        if self.align_busy || self.export_busy || self.queue_busy || self.merge_busy || self.qa_busy
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.seq.is_some() {
            // egui 空闲时不会自己重绘；自动保存与后台进度都需要一个低频心跳。
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        if self.seq.is_some() && self.last_auto_save.elapsed() >= Duration::from_secs(5) {
            self.last_auto_save = Instant::now();
            // 只在内容真的变了、而且用户没在动手的时候写盘：序列化加写替换要几毫秒，
            // 正好落在拖滑块或播放的那一帧上就是一下可见的卡顿；空闲时每小时写一次
            // 和每 5 秒写一次，对盘上结果没有任何区别。
            let busy = self.playing || ctx.input(|i| i.pointer.any_down());
            if !busy && !self.project_unchanged() {
                self.write_project(true);
            }
        }

        // 回环控制：先把在线程里排队的请求在本帧里执行完，再让服务跟随当前序列。
        loop {
            let event = match self.control_rx.try_recv() {
                Ok(event) => event,
                Err(_) => break,
            };
            let result = self.run_ui_command(ctx, &event.id, &event.args);
            let _ = event.reply.send(result);
        }
        self.sync_control(ctx);
    }

    /// 保证控制服务与「当前有没有序列」一致：没序列就不写 control.json，
    /// 换了序列就重启服务（端口与令牌一起换新，旧文件由 Drop 清掉）。
    fn sync_control(&mut self, ctx: &Context) {
        if self.headless || !self.control_enabled {
            self.control = None;
            return;
        }
        // 每帧都要问一遍「还是这个序列吗」，所以这里不留分配：路径先只借来比一下，
        // 真要重启服务时才拷一份。
        let open = self
            .seq
            .as_ref()
            .map(|s| s.root.as_path())
            .filter(|path| path.is_dir());
        let same = match (&self.control, open) {
            (Some(server), Some(root)) => server.root() == root,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let root = open.map(|path| path.to_path_buf());
        // 先停旧的（Drop 会删自己写的那份 control.json）。
        self.control = None;
        let Some(root) = root else { return };

        let tx = Arc::new(Mutex::new(self.control_tx.clone()));
        let ctx = ctx.clone();
        let executor = move |id: &str, args: &Value| -> Result<Value, String> {
            let (reply, wait) = channel();
            {
                let sender = tx.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                sender
                    .send(ControlEvent {
                        id: id.to_owned(),
                        args: args.clone(),
                        reply,
                    })
                    .map_err(|_| "界面线程已关闭，控制通道不可用".to_owned())?;
            }
            // 立刻唤醒界面：空闲的 egui 不会自己重绘，请求会一直排队。
            ctx.request_repaint();
            wait.recv_timeout(control::UI_TIMEOUT).map_err(|_| {
                format!(
                    "命令 {id} 超时：界面 {:.0} 秒内没有响应",
                    control::UI_TIMEOUT.as_secs_f64()
                )
            })?
        };
        match ControlServer::start(&root, Arc::new(executor)) {
            Ok(server) => self.control = Some(server),
            Err(e) => self.error = Some(format!("控制服务启动失败：{e}")),
        }
    }

    /// 给界面展示用的控制文件信息；没开服务时为 None。
    pub fn control_file(&self) -> Option<&ControlFile> {
        self.control.as_ref().map(ControlServer::file)
    }

    pub fn control_path(&self) -> Option<PathBuf> {
        self.control.as_ref().map(|s| s.path().to_path_buf())
    }

    // ------------------------------------------------------------ 撤销 / 重做

    /// 抓一份可撤销状态。视图（缩放、面板、当前帧）与只读结果（测光、直方图）不在
    /// 里面，见 [`Snapshot`]。
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            params: self.params.clone(),
            preset: self.preset.clone(),
            def_window: self.def_window,
            def_strength: self.def_strength,
            def_multi: self.def_multi,
            def_channels: self.def_channels,
            aspect: self.aspect,
            crop: self.user_crop,
            rotate: self.rotate,
            grid: self.grid,
            cross: self.cross,
            split: self.split,
            keyframes: self.keyframes.clone(),
            copies: self.copies.clone(),
            ratings: self.ratings.clone(),
            flags: self.flags.clone(),
            labels: self.labels.clone(),
            keywords: self.keywords.clone(),
            output_w: self.output_w,
            output_h: self.output_h,
            output_fps: self.output_fps,
            codec: self.codec.clone(),
            render_mode: self.render_mode,
            render_format: self.render_format,
            render_quality: self.render_quality,
            render_keep_exif: self.render_keep_exif,
            render_xmp_sidecar: self.render_xmp_sidecar,
            render_sharpen_for: self.render_sharpen_for,
            render_sharpen_strength: self.render_sharpen_strength,
            render_color_space: self.render_color_space,
            render_watermark: self.render_watermark.clone(),
            retime: self.retime.clone(),
            motion: self.motion,
            render_crf: self.render_crf,
            render_preset: self.render_preset.clone(),
            render_keep_frames: self.render_keep_frames,
            render_resume: self.render_resume,
            render_audio: self.render_audio.clone(),
            render_audio_opts: self.render_audio_opts.clone(),
        }
    }

    /// 回放快照：收尾和人在面板上改完是同一套——该重算去闪的重算，该重新探针的探针。
    pub fn restore(&mut self, snap: Snapshot) {
        let def_changed = self.def_window != snap.def_window
            || self.def_strength != snap.def_strength
            || self.def_multi != snap.def_multi
            || self.def_channels != snap.def_channels;
        let audio_changed = self.render_audio != snap.render_audio;
        self.params = snap.params;
        self.preset = snap.preset;
        self.def_window = snap.def_window;
        self.def_strength = snap.def_strength;
        self.def_multi = snap.def_multi;
        self.def_channels = snap.def_channels;
        self.aspect = snap.aspect;
        // 裁剪框是跟着比例一起定的设置：直接落到快照里的值，不能再走 set_aspect 重铺。
        self.user_crop = snap.crop;
        self.rotate = snap.rotate;
        self.crop_drag = None;
        self.grid = snap.grid;
        self.cross = snap.cross;
        self.split = snap.split;
        self.keyframes = snap.keyframes;
        self.copies = snap.copies;
        self.ratings = snap.ratings;
        self.flags = snap.flags;
        self.labels = snap.labels;
        self.keywords = snap.keywords;
        self.output_w = snap.output_w;
        self.output_h = snap.output_h;
        self.output_fps = snap.output_fps;
        self.codec = snap.codec;
        self.render_mode = snap.render_mode;
        self.render_format = snap.render_format;
        self.render_quality = snap.render_quality;
        self.render_keep_exif = snap.render_keep_exif;
        self.render_xmp_sidecar = snap.render_xmp_sidecar;
        self.render_sharpen_for = snap.render_sharpen_for;
        self.render_sharpen_strength = snap.render_sharpen_strength;
        self.render_color_space = snap.render_color_space;
        self.render_watermark = snap.render_watermark;
        self.retime = snap.retime;
        self.motion = snap.motion;
        self.render_crf = snap.render_crf;
        self.render_preset = snap.render_preset;
        self.render_keep_frames = snap.render_keep_frames;
        self.render_resume = snap.render_resume;
        self.render_audio_opts = snap.render_audio_opts;
        // 蒙版可能比快照里少了，选中项要跟着收，不然画布手柄指向越界的下标。
        if self.mask_sel.is_some_and(|i| i >= self.params.masks.len()) {
            self.mask_sel = None;
            self.mask_comp = 0;
            self.mask_pick = false;
        }
        if def_changed {
            self.recompute_deflicker();
        }
        if audio_changed {
            // 走同一个入口：换配乐要重新读时长，别把上一个文件的探针结果留下。
            self.set_render_audio(snap.render_audio);
        }
        self.preview_dirty = true;
    }

    /// 换序列、关序列：历史从头开始，上一步属于上一个工程。
    pub fn history_reset(&mut self, label: &str) {
        history::clear_note();
        let snap = self.snapshot();
        self.history.reset(snap, label);
    }

    /// 把当前参数钉成一份命名快照，挂在左栏「快照」那一节。
    ///
    /// 名字取当前预设 + 序号：`原始 · 2`。比纯序号好认——一列「快照 1/2/3」
    /// 过两天谁也分不清哪份是满意的。
    pub fn save_snapshot(&mut self) {
        let label = format!("{} · {}", self.preset, self.snapshots.len() + 1);
        let snap = self.snapshot();
        self.snapshots.push((label.clone(), snap));
        self.status = format!("已存快照「{label}」");
    }

    /// 回到某一份快照。整组参数换回去，照样进历史，撤得回来。
    pub fn restore_snapshot(&mut self, i: usize) {
        let Some((label, snap)) = self.snapshots.get(i).cloned() else {
            return;
        };
        self.restore(snap);
        self.status = format!("已回到快照「{label}」");
    }

    pub fn delete_snapshot(&mut self, i: usize) {
        if i < self.snapshots.len() {
            self.snapshots.remove(i);
        }
    }

    /// 界面帧末尾把当前状态和光标处的快照比一下：不一样就记一步。
    ///
    /// 判据是状态本身，不是「有没有控件登记过」——画布手柄、色轮、以后新加的控件忘了
    /// 登记文案，改动照样进历史，只是那一行写成字段名。登记的文案优先。
    pub fn history_commit(&mut self) {
        let snap = self.snapshot();
        if !self.history.dirty(&snap) {
            history::clear_note();
            return;
        }
        let (key, label) = match history::take_note() {
            Some((key, label)) => (key, label),
            None => {
                let hint = self.history.diff_hint(&snap);
                (hint.to_owned(), hint.to_owned())
            }
        };
        self.history.push(snap, &key, label);
    }

    /// 历史行里的参数名换成中文短名。面板上的长名在 shell.rs 的滑杆里，这里只要求
    /// 一眼看出改了哪一类，所以同组的细分项并成一个词。认不出的原样返回：MCP 按英文
    /// 名写入，留着英文反而对得上参数。
    fn cn_field(name: &str) -> &str {
        const GROUPS: &[(&str, &str)] = &[
            ("grain_", "颗粒"),
            ("vig_", "暗角"),
            ("sharp_", "锐化"),
            ("nr_", "降噪"),
            ("grade_", "色彩分级"),
            ("lv_", "色阶"),
            ("par_", "参数曲线"),
            ("hsl_", "HSL"),
            ("curve_", "曲线"),
            ("lens_", "镜头"),
            ("mono_", "单色"),
        ];
        match name {
            "exposure" => "曝光",
            "contrast" => "对比度",
            "highlights" => "高光",
            "shadows" => "阴影",
            "whites" => "白色",
            "blacks" => "黑色",
            "temperature" => "色温",
            "tint" => "色调",
            "vibrance" => "自然饱和度",
            "saturation" => "饱和度",
            "clarity" => "清晰度",
            "texture" => "纹理",
            "dehaze" => "去朦胧",
            "grain" => "颗粒",
            "vignette" => "暗角",
            "mono" => "单色",
            "masks" => "蒙版",
            "look_cube" => "外观",
            "grade_balance" => "分级平衡",
            "grade_blending" => "分级强度",
            "hsl_h" => "HSL 色相",
            "hsl_s" => "HSL 饱和",
            "hsl_l" => "HSL 明度",
            "curve_rgb" => "RGB 曲线",
            "curve_r" => "红曲线",
            "curve_g" => "绿曲线",
            "curve_b" => "蓝曲线",
            other => GROUPS
                .iter()
                .find(|(prefix, _)| other.starts_with(prefix))
                .map_or(other, |(_, label)| *label),
        }
    }

    /// 蒙版类型在命令行里是英文，历史行用界面那套中文说法。
    fn cn_mask_kind(kind: &str) -> &str {
        match kind {
            "radial" => "径向",
            "linear" => "线性",
            other => other,
        }
    }

    /// 命令 → 历史里的一行。`None` 是只读命令，不记步；第三个字段 false 表示效果
    /// 已经落到盘上（导出、对齐、存工程），撤销只回参数、不删产物。
    ///
    /// 这套判定和 `journal::is_write_op` 有意不同：日志要审计队列与存盘，历史只关心
    /// 能不能回到上一个状态；反过来 `deflicker.set` 进日志是漏的，但必须能撤销。
    fn history_label(id: &str, args: &Value) -> Option<(String, String, bool)> {
        let named = |key: &str| -> String {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned()
        };
        Some(match id {
            "grade.set" => {
                let obj = args.get("params").filter(|p| p.is_object()).unwrap_or(args);
                let fields: Vec<String> = obj
                    .as_object()
                    .map(|m| {
                        m.keys()
                            .take(3)
                            .map(|name| Self::cn_field(name).to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let more = if obj.as_object().is_some_and(|m| m.len() > 3) {
                    "…"
                } else {
                    ""
                };
                (
                    "grade.set".to_owned(),
                    format!("改 {}", [fields.join("、"), more.to_owned()].join("")),
                    true,
                )
            }
            "grade.preset" => (
                "grade.preset".to_owned(),
                format!("预设 {}", named("name")),
                true,
            ),
            "grade.reset" => ("grade.reset".to_owned(), "复位全部参数".to_owned(), true),
            "grade.copy" => {
                let label = match args.get("op").and_then(Value::as_str).unwrap_or("list") {
                    "save" => format!("存副本 {}", named("name")),
                    "apply" => format!("套用副本 {}", named("name")),
                    "delete" => format!("删副本 {}", named("name")),
                    "rename" => format!("副本改名 {}", named("to")),
                    _ => "调色副本".to_owned(),
                };
                ("grade.copy".to_owned(), label, true)
            }
            "mask.add" => {
                // 没带 kind 时命令按 linear 走，历史行也跟着写线性。
                let kind = if named("kind").eq_ignore_ascii_case("radial") {
                    "radial"
                } else {
                    "linear"
                };
                (
                    "mask.add".to_owned(),
                    format!("加{}蒙版", Self::cn_mask_kind(kind)),
                    true,
                )
            }
            "mask.remove" => ("mask.remove".to_owned(), "删蒙版".to_owned(), true),
            "mask.update" => ("mask.update".to_owned(), "改蒙版".to_owned(), true),
            "mask.component" => ("mask.component".to_owned(), "改蒙版形状".to_owned(), true),
            "deflicker.set" => ("deflicker.set".to_owned(), "去闪烁设置".to_owned(), true),
            "lens.set" => ("lens.set".to_owned(), "镜头配置文件".to_owned(), true),
            "profile.set" => {
                let kind = if named("kind").eq_ignore_ascii_case("lcp") {
                    "镜头配置"
                } else {
                    "相机配置"
                };
                ("profile.set".to_owned(), kind.to_owned(), true)
            }
            "crop.set" => {
                // 一行说清这次改的是框、拉直还是比例；纯读回没有改动，后面 dirty 判定会吞掉这一步。
                let label = if args.get("reset").and_then(Value::as_bool).unwrap_or(false) {
                    "取消取景".to_owned()
                } else if args.get("x").is_some() {
                    format!(
                        "取景框 {} × {}",
                        args["width"].as_u64().unwrap_or(0),
                        args["height"].as_u64().unwrap_or(0)
                    )
                } else if let Some(deg) = args.get("rotate").and_then(Value::as_f64) {
                    format!("拉直 {deg:+.1}°")
                } else {
                    format!("画幅 {}", named("aspect"))
                };
                ("crop.set".to_owned(), label, true)
            }
            "audio.config" => ("audio.config".to_owned(), "配乐设置".to_owned(), true),
            "render.set" => {
                // 一行点出这次动的是哪个设置；纯查询没有改动，dirty 判定会吞掉这步。
                let label = if args.get("format").is_some() {
                    format!("输出容器 {}", named("format"))
                } else if args.get("quality").is_some() {
                    format!("输出质量 {}", args["quality"].as_u64().unwrap_or(0))
                } else if args.get("sharpen_for").is_some() {
                    format!("输出锐化 {}", named("sharpen_for"))
                } else if args.get("keep_exif").is_some() {
                    "EXIF 搬运".to_owned()
                } else if args.get("xmp_sidecar").is_some() {
                    "XMP 旁车".to_owned()
                } else if args.get("watermark").is_some() {
                    "水印".to_owned()
                } else {
                    "输出设置".to_owned()
                };
                ("render.set".to_owned(), label, true)
            }
            "retime.set" => {
                let num = |v: Option<&Value>| -> String {
                    v.and_then(Value::as_f64)
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".to_owned())
                };
                // 变速整块只有一行：一行点出这次动的是曲线还是快门。
                let label = if args.get("reset").is_some() {
                    "清除变速".to_owned()
                } else if args.get("speed").is_some() {
                    format!("倍速 {}×", num(args.get("speed")))
                } else if let Some(ease) = args.get("ease") {
                    format!("缓入缓出到 {}×", num(ease.get("peak")))
                } else if args.get("keys").is_some() {
                    "速度曲线".to_owned()
                } else if args.get("shutter").is_some() {
                    format!("运动模糊 {}", num(args.get("shutter")))
                } else {
                    "变速开关".to_owned()
                };
                ("retime.set".to_owned(), label, true)
            }
            "motion.set" => {
                // 运镜一行点出这次动的是哪个框、哪条坡。
                let label = if args.get("reset").is_some() {
                    "清除运镜".to_owned()
                } else if args.get("swap").is_some() {
                    "运镜反向".to_owned()
                } else if args.get("ease").is_some() {
                    format!("运动曲线 {}", named("ease"))
                } else if args.get("refit").is_some() {
                    "运镜换比例".to_owned()
                } else if args.get("to").is_some() {
                    "运镜终点框".to_owned()
                } else if args.get("from").is_some() {
                    "运镜起点框".to_owned()
                } else {
                    "运镜开关".to_owned()
                };
                ("motion.set".to_owned(), label, true)
            }
            "xmp.import" => ("xmp.import".to_owned(), "导入 XMP 预设".to_owned(), true),
            "cube.import" => ("cube.import".to_owned(), "导入 .cube 调色".to_owned(), true),
            "align.run" => (String::new(), "对齐序列".to_owned(), false),
            "export.video" => (String::new(), "导出成片".to_owned(), false),
            "export.stills" => (String::new(), "导出静帧".to_owned(), false),
            "hdr.merge" => (String::new(), "HDR 合并".to_owned(), false),
            "pano.merge" => (String::new(), "全景合并".to_owned(), false),
            "queue.run" => (String::new(), "批量渲染队列".to_owned(), false),
            "queue.add" | "queue.remove" | "queue.clear" => {
                (String::new(), "改渲染队列".to_owned(), false)
            }
            "project.save" => (String::new(), "保存工程".to_owned(), false),
            "xmp.export" => (String::new(), "导出 XMP".to_owned(), false),
            "cube.export" => (String::new(), "导出 .cube".to_owned(), false),
            _ => return None,
        })
    }

    /// 命令驱动的写入：参数类的记一步，导出/对齐这类外部效果记成不可撤销。
    fn history_note_command(&mut self, id: &str, args: &Value) {
        let step = Self::history_label(id, args);
        // 命令执行期间控件路径也会登记改动（mask.* 就是这样：命令内部走 masks_changed）。
        // 命令自己要记步时把它吞掉，一次写入只留一行；只读命令则原样退回，帧末照旧落账。
        let pending = history::take_note();
        let Some((key, label, revertible)) = step else {
            if let Some((k, l)) = pending {
                history::note(&k, l);
            }
            return;
        };
        let _ = pending;
        let snap = self.snapshot();
        if revertible {
            // 参数一点没变就不占一步（把同一个预设再套一次）。
            if self.history.dirty(&snap) {
                self.history.push(snap, &key, label);
            }
        } else {
            // 外部效果照旧记：产物已经在盘上了，这一步撤销不了，但要在历史里看得见。
            self.history.push_external(snap, label);
        }
    }

    fn history_move_json(&self, m: &history::Move, action: &str, step: &str) -> Value {
        json!({
            "action": action,
            "step": step,
            "at": m.cursor + 1,
            "total": m.total,
            "undoable": m.cursor,
            "redoable": m.redoable,
            "warning": if m.external {
                "这一步有已经写到盘上的产物：参数回退了，产物不会删除"
            } else {
                ""
            },
        })
    }

    fn cmd_history_undo(&mut self) -> Result<Value, String> {
        let m = self
            .history
            .undo()
            .ok_or_else(|| "没有可以撤销的改动".to_owned())?;
        let out = self.history_move_json(&m, "undo", &m.label.clone());
        self.restore(m.snap);
        self.status = format!("撤销 · {step}", step = m.label);
        Ok(out)
    }

    fn cmd_history_redo(&mut self) -> Result<Value, String> {
        let m = self
            .history
            .redo()
            .ok_or_else(|| "没有可以重做的改动".to_owned())?;
        let out = self.history_move_json(&m, "redo", &m.label.clone());
        self.restore(m.snap);
        self.status = format!("重做 · {step}", step = m.label);
        Ok(out)
    }

    fn cmd_history_jump(&mut self, args: &Value) -> Result<Value, String> {
        let raw = args
            .get("index")
            .or_else(|| args.get("step"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "缺少 index：跳到第几步（1 开始）".to_owned())?;
        if raw == 0 || raw as usize > self.history.steps().len() {
            return Err(format!(
                "index 超出范围：1..={}",
                self.history.steps().len().max(1)
            ));
        }
        let m = self
            .history
            .jump(raw as usize - 1)
            .ok_or_else(|| "历史记录已改变，请重新查询".to_owned())?;
        let label = m.label.clone();
        let out = self.history_move_json(&m, "jump", &label.clone());
        self.restore(m.snap);
        self.status = format!("回到 · {label}");
        Ok(out)
    }

    /// 历史列表 + 当前光标；面板和 MCP 共用这一份口径。
    pub fn history_json(&self) -> Value {
        let steps: Vec<Value> = self
            .history
            .steps()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({
                    "index": i + 1,
                    "step": s.label,
                    "applied": i == self.history.cursor(),
                    "revertible": s.revertible,
                })
            })
            .collect();
        json!({
            "count": steps.len(),
            "current": self.history.cursor() + 1,
            "undoable": self.history.undoable(),
            "redoable": steps.len().saturating_sub(self.history.cursor() + 1),
            "steps": steps,
        })
    }

    // ------------------------------------------------------------ 命令执行器

    pub fn require_sequence(&self) -> Result<&Sequence, String> {
        self.seq
            .as_ref()
            .ok_or_else(|| "还没有打开序列：先用 sequence.open 指定帧文件夹".to_owned())
    }

    /// 唯一的命令执行入口：控制服务（回环）与右侧 AI 面板都走这里。
    ///
    /// 返回 JSON 给调用方；错误是可直接展示的中文文案。长任务
    /// （`align.run` / `export.video` / `export.stills` / `hdr.merge` / `pano.merge` /
    /// `ui.screenshot`）只负责启动后台线程并立即返回，进度用对应的 status 命令查询
    /// （合并是 `merge.status`，可用 `merge.cancel` 取消）。
    pub fn run_ui_command(
        &mut self,
        ctx: &Context,
        id: &str,
        args: &Value,
    ) -> Result<Value, String> {
        let result = self.dispatch_ui_command(ctx, id, args);
        // 成功的写操作追加进序列目录的 op_journal.jsonl；日志失败不影响命令本身。
        if result.is_ok()
            && journal::is_write_op(id)
            && let Some(root) = self.seq.as_ref().map(|s| s.root.clone())
        {
            let _ = journal::append(&root, "gui", id, args);
        }
        // 同时记进撤销历史：无头模式不会走界面帧，所以在这里就地落账。
        if result.is_ok() && id != "sequence.open" {
            self.history_note_command(id, args);
        }
        result
    }

    fn dispatch_ui_command(
        &mut self,
        ctx: &Context,
        id: &str,
        args: &Value,
    ) -> Result<Value, String> {
        match id {
            "sequence.info" => self.cmd_sequence_info(),
            "sequence.open" => self.cmd_sequence_open(ctx, args),
            "sequence.check" => self.cmd_sequence_check(ctx, args),
            "sequence.status" => Ok(self.qa_status_json()),
            "sequence.cancel" => {
                let busy = self.qa_busy;
                self.stop_qa();
                Ok(json!({
                    "cancelling": busy,
                    "note": if busy {
                        "已请求取消体检；在一批文件之间生效，清单保持上一次的结果".to_owned()
                    } else {
                        "当前没有进行中的体检".to_owned()
                    },
                }))
            }
            "library.add" => self.cmd_library_add(args),
            "library.remove" => self.cmd_library_remove(args),
            "library.list" => self.cmd_library_list(args),
            "library.open" => self.cmd_library_open(ctx, args),
            "library.organize" => self.cmd_library_organize(args),
            "library.keyword" => self.cmd_library_keyword(args),
            "library.metadata" => self.cmd_library_metadata(args),
            "library.stack" => self.cmd_library_stack(args),
            "collection.create" => self.cmd_collection_create(args),
            "collection.add" => self.cmd_collection_add(args),
            "collection.remove" => self.cmd_collection_remove(args),
            "collection.delete" => self.cmd_collection_delete(args),
            "collection.list" => self.cmd_collection_list(args),
            "frame.select" => self.cmd_frame_select(args),
            "ui.module" => self.cmd_ui_module(args),
            "ui.dock" => self.cmd_ui_dock(args),
            "grade.get" => Ok(self.grade_json()),
            "grade.auto" => {
                self.auto_exposure();
                let mut out = self.grade_json();
                if let Some(note) = &self.auto_note
                    && let Some(obj) = out.as_object_mut()
                {
                    obj.insert("note".to_owned(), json!(note));
                }
                Ok(out)
            }
            "grade.set" => self.cmd_grade_set(args),
            "grade.preset" => self.cmd_grade_preset(args),
            "grade.reset" => {
                self.reset_params();
                Ok(self.grade_json())
            }
            "grade.copy" => self.cmd_grade_copy(args),
            "hdr.merge" => self.cmd_hdr_merge(ctx, args),
            "pano.merge" => self.cmd_pano_merge(ctx, args),
            "merge.status" => Ok(self.merge_status_json()),
            "merge.cancel" => {
                let busy = self.merge_busy;
                let kind = self.merge_kind.map(MergeKind::cn);
                self.stop_merge();
                Ok(json!({
                    "cancelling": busy,
                    "note": match (busy, kind) {
                        (true, Some(k)) => format!("已请求取消{k}；在两帧之间生效，已写下的帧不会删除"),
                        (true, None) => "已请求取消；在两帧之间生效".to_owned(),
                        (false, _) => "当前没有进行中的合并".to_owned(),
                    },
                }))
            }
            "mask.list" => Ok(self.mask_list_json()),
            "mask.add" => self.cmd_mask_add(args),
            "mask.remove" => self.cmd_mask_remove(args),
            "mask.update" => self.cmd_mask_update(args),
            "mask.component" => self.cmd_mask_component(args),
            "align.run" => self.cmd_align_run(ctx, args),
            "align.status" => Ok(self.align_status_json()),
            "deflicker.set" => self.cmd_deflicker_set(args),
            "lens.set" => self.cmd_lens_set(args),
            "profile.set" => self.cmd_profile_set(args),
            "crop.set" => self.cmd_crop_set(args),
            "render.set" => self.cmd_render_set(args),
            "retime.set" => self.cmd_retime_set(args),
            "retime.status" => Ok(self.retime_status_json()),
            "motion.set" => self.cmd_motion_set(args),
            "motion.status" => Ok(self.motion_status_json()),
            "export.video" | "export.stills" => self.cmd_export(ctx, id, args),
            "export.status" => Ok(self.export_status_json()),
            "export.cancel" => {
                let busy = self.export_busy;
                self.stop_export();
                Ok(json!({
                    "cancelling": busy,
                    "note": if busy { "已请求取消；已写下的素材不会删除" } else { "当前没有进行中的导出" },
                }))
            }
            "project.save" => self.cmd_project_save(),
            "project.get" => self.cmd_project_get(),
            "project.metadata" => self.cmd_project_metadata(args),
            "journal.list" => self.cmd_journal_list(args),
            "history.list" => Ok(self.history_json()),
            "history.undo" => self.cmd_history_undo(),
            "history.redo" => self.cmd_history_redo(),
            "history.jump" => self.cmd_history_jump(args),
            "xmp.import" => self.cmd_xmp_import(args),
            "xmp.export" => self.cmd_xmp_export(args),
            "cube.import" => self.cmd_cube_import(args),
            "cube.export" => self.cmd_cube_export(args),
            "audio.config" => self.configure_audio(args),
            "queue.list" => Ok(self.queue_json()),
            "queue.add" => self.cmd_queue_add(args),
            "queue.remove" => self.cmd_queue_remove(args),
            "queue.clear" => self.cmd_queue_clear(),
            "queue.run" => self.cmd_queue_run(ctx, args),
            "queue.status" => Ok(self.queue_status_json()),
            "queue.cancel" => {
                let busy = self.queue_busy;
                self.stop_queue();
                Ok(json!({
                    "cancelling": busy,
                    "note": if busy { "已请求取消；已渲完的段保留，未开始的仍待渲" } else { "队列没有在跑" },
                }))
            }
            "ui.screenshot" => self.cmd_screenshot(ctx, args),
            other => Err(format!(
                "未知命令：{other}（用 system.commands 或 AI 面板的命令清单查看可用命令）"
            )),
        }
    }

    /// 去闪设置 + 最近一次分析摘要；`sequence.info` 与 `deflicker.set` 共用一份口径。
    fn deflicker_json(&self) -> Value {
        let analysis = match &self.analysis {
            Some(analysis) => json!({
                "analyzed": true,
                "frames": analysis.stats.frames,
                "valid": analysis.stats.valid,
                "jitter_before": round6(analysis.stats.jitter_before),
                "jitter_after": round6(analysis.stats.jitter_after),
                "channels": analysis.stats.channels,
                "color_before": round6(analysis.stats.color_before),
                "color_after": round6(analysis.stats.color_after),
                "max_ev": round6(analysis.stats.max_ev),
            }),
            None => json!({"analyzed": false}),
        };
        json!({
            "window": self.def_window,
            "strength": self.def_strength,
            "multi": self.def_multi,
            "channels": self.def_channels,
            "measured": self.def_means.iter().filter(|m| m.is_some()).count(),
            "applied": self.corr.iter().filter(|v| v.abs() > 0.001).count(),
            "analysis": analysis,
        })
    }

    /// 读或改去闪设置；任何改动都立刻重算，回一条与 `sequence.info` 同口径的摘要。
    fn cmd_deflicker_set(&mut self, args: &Value) -> Result<Value, String> {
        if let Some(v) = args.get("window") {
            let x = v.as_f64().ok_or("window 必须是数字（奇数窗口）")?;
            if !x.is_finite() || !(3.0..=199.0).contains(&x) {
                return Err("window 必须在 3..199".to_owned());
            }
            self.def_window = ((x.round() as i64).clamp(3, 199) | 1) as f64;
        }
        if let Some(v) = args.get("strength") {
            let x = v.as_f64().ok_or("strength 必须是数字（百分比）")?;
            if !x.is_finite() || !(0.0..=150.0).contains(&x) {
                return Err("strength 必须在 0..150".to_owned());
            }
            self.def_strength = x;
        }
        if let Some(v) = args.get("multi") {
            self.def_multi = v.as_bool().ok_or("multi 必须是布尔")?;
        }
        if let Some(v) = args.get("channels") {
            self.def_channels = v.as_bool().ok_or("channels 必须是布尔")?;
        }
        self.recompute_deflicker();
        Ok(self.deflicker_json())
    }

    /// 镜头配置文件摘要：`lens.set` 的回执与面板显示同一份口径。
    fn lens_profile_json(&self) -> Value {
        json!({
            "profile": self.params.lens_profile.as_ref().map(|c| c.to_json()),
            "lcp": self.params.lcp_profile,
            "note": self.lens_profile_note,
        })
    }

    /// 打开序列后按首帧 EXIF 匹配镜头配置文件。lensfun 数据库在磁盘上找，
    /// 没有库、没接镜头、EXIF 缺信息都静默降级——手动三项照旧，只留一行说明。
    pub fn match_lens_profile(&mut self) {
        self.lens_profile_note.clear();
        let Some(ex) = self.exif.first() else {
            self.lens_profile_note = "没有 EXIF，无从匹配镜头配置".to_owned();
            return;
        };
        let Some(make) = ex.make().map(str::to_owned) else {
            self.lens_profile_note = "EXIF 里没有机身厂商，无从匹配镜头配置".to_owned();
            return;
        };
        let model = ex.model().unwrap_or("").to_owned();
        let lens = ex.lens().unwrap_or("").to_owned();
        let focal = ex.focal_mm().unwrap_or(0.0);
        let aperture = ex.fnum();
        self.lens_match_apply(&make, &model, &lens, focal, aperture);
    }

    /// 用给定的 EXIF 三件套跑匹配并落进参数。`match_lens_profile` 的主体，
    /// 独立出来供测试直呼。
    pub fn lens_match_apply(
        &mut self,
        make: &str,
        model: &str,
        lens: &str,
        focal: f64,
        aperture: Option<f64>,
    ) {
        let db = aevum_core::lensdb::load_auto();
        if db.lenses.is_empty() {
            self.lens_profile_note =
                "没找到 lensfun 数据库（设 AEVUM_LENSFUN，或把库放 exe 旁的 lensfun/ 目录）"
                    .to_owned();
            return;
        }
        match aevum_core::lensdb::match_correction(&db, make, model, lens, focal, aperture) {
            Some(corr) => {
                self.lens_profile_note = format!(
                    "已匹配 {}（{}mm · 裁切 {:.1}）",
                    corr.lens, corr.focal_mm, corr.crop
                );
                self.lens_profile_saved = Some(corr.clone());
                self.params.lens_profile = Some(corr);
                self.preview_dirty = true;
            }
            None => {
                self.lens_profile_note =
                    format!("数据库里没有 {make} {lens} 的标定，畸变与暗角仍走手动三项");
            }
        }
    }

    /// 读或开关镜头配置文件校正：`enabled` 开关（关掉暂存、再开恢复）、
    /// `rematch: true` 强制按当前 EXIF 重新匹配；不带参数即查询。
    fn cmd_lens_set(&mut self, args: &Value) -> Result<Value, String> {
        if args
            .get("rematch")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.match_lens_profile();
        }
        if let Some(v) = args.get("enabled") {
            let on = v.as_bool().ok_or("enabled 必须是布尔")?;
            if on {
                if self.params.lens_profile.is_none()
                    && let Some(c) = self.lens_profile_saved.take()
                {
                    self.params.lens_profile = Some(c);
                    self.preview_dirty = true;
                }
            } else {
                self.lens_profile_saved = self.params.lens_profile.take();
                self.preview_dirty = true;
            }
        }
        Ok(self.lens_profile_json())
    }

    /// 相机/镜头配置摘要：面板与 `profile.set` 共用一份口径。
    fn profile_json(&self) -> Value {
        json!({
            "dcp": {
                "path": self.params.dcp_profile,
                "name": self.dcp_name,
                "note": self.dcp_note,
            },
            "lcp": {
                "correction": self.params.lcp_profile.as_ref().map(|c| c.to_json()),
                "note": self.lcp_note,
            },
        })
    }

    /// 从首帧 EXIF 折出 LCP 的匹配请求。
    fn exif_lens_request(&self) -> Result<aevum_core::lcp::Request, String> {
        let ex = self
            .exif
            .first()
            .ok_or_else(|| "没有 EXIF，无从匹配镜头配置".to_owned())?;
        Ok(aevum_core::lcp::Request {
            make: ex.make().unwrap_or("").to_owned(),
            model: ex.model().unwrap_or("").to_owned(),
            lens: ex.lens().unwrap_or("").to_owned(),
            focal_mm: ex.focal_mm().unwrap_or(0.0),
            focal35_mm: None,
            aperture: ex.fnum(),
        })
    }

    /// 按 EXIF 机身找 DCP；找不到只留一行说明，不改动当前参数。
    pub fn match_dcp_profile(&mut self) {
        self.dcp_note.clear();
        let Some(ex) = self.exif.first() else {
            self.dcp_note = "没有 EXIF，无从匹配相机配置".to_owned();
            return;
        };
        let make = ex.make().unwrap_or("").to_owned();
        let model = ex.model().unwrap_or("").to_owned();
        match aevum_core::dcp::auto_match(&make, &model) {
            Ok(p) => {
                self.dcp_note = format!("已套用「{}」（{}）", p.name, p.camera);
                self.dcp_name = p.name.clone();
                self.params.dcp_profile = p.path;
                self.preview_dirty = true;
            }
            Err(e) => self.dcp_note = e,
        }
    }

    /// 指定一份 `.dcp`。机型对不上就拒绝——配置拿错比不套更糟。
    pub fn set_dcp_profile(&mut self, path: &str) -> Result<(), String> {
        let p = aevum_core::dcp::Profile::load(path)?;
        if let Some(ex) = self.exif.first() {
            let make = ex.make().unwrap_or("");
            let model = ex.model().unwrap_or("");
            if !model.is_empty() && !p.matches(make, model) {
                return Err(format!(
                    "配置是给「{}」的，当前机型是「{} {}」",
                    p.camera, make, model
                ));
            }
        }
        self.dcp_name = p.name.clone();
        self.dcp_note = format!("已套用「{}」（{}）", p.name, p.camera);
        self.params.dcp_profile = p.path.clone();
        self.preview_dirty = true;
        Ok(())
    }

    pub fn clear_dcp_profile(&mut self) {
        self.params.dcp_profile.clear();
        self.dcp_name.clear();
        self.dcp_note.clear();
        self.preview_dirty = true;
    }

    /// 按 EXIF 在默认目录里找 LCP 并折出校正。
    pub fn match_lcp_profile(&mut self) {
        self.lcp_note.clear();
        let req = match self.exif_lens_request() {
            Ok(req) => req,
            Err(e) => {
                self.lcp_note = e;
                return;
            }
        };
        match aevum_core::lcp::auto_match(&req) {
            Ok((path, corr)) => {
                let file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.lcp_note = format!(
                    "已套用 {}（{} · {:.0}mm）",
                    file, corr.lens, corr.focal35_mm
                );
                self.params.lcp_profile = Some(corr);
                self.preview_dirty = true;
            }
            Err(e) => self.lcp_note = e,
        }
    }

    /// 指定一份 `.lcp`，按当前 EXIF 选帧。
    pub fn set_lcp_profile(&mut self, path: &str) -> Result<(), String> {
        let file = aevum_core::lcp::load(path)?;
        let req = self.exif_lens_request()?;
        let corr = aevum_core::lcp::match_correction(&file, &req)?;
        self.lcp_note = format!(
            "已套用 {}（{} · {:.0}mm）",
            file.profile, corr.lens, corr.focal35_mm
        );
        self.params.lcp_profile = Some(corr);
        self.preview_dirty = true;
        Ok(())
    }

    pub fn clear_lcp_profile(&mut self) {
        self.params.lcp_profile = None;
        self.lcp_note.clear();
        self.preview_dirty = true;
    }

    /// 读取或套用相机/镜头配置。`kind` 为 `dcp` 或 `lcp`：
    /// `path` 指定文件、`auto` 按 EXIF 自动找、`clear` 清掉；都不带即查询。
    fn cmd_profile_set(&mut self, args: &Value) -> Result<Value, String> {
        let kind = match args.get("kind").and_then(Value::as_str) {
            Some("dcp") => "dcp",
            Some("lcp") => "lcp",
            Some(other) => return Err(format!("未知的配置类型：{other}（dcp 或 lcp）")),
            None => return Err("缺少 kind（dcp 或 lcp）".to_owned()),
        };
        if args.get("clear").and_then(Value::as_bool).unwrap_or(false) {
            if kind == "dcp" {
                self.clear_dcp_profile();
            } else {
                self.clear_lcp_profile();
            }
            return Ok(self.profile_json());
        }
        if let Some(path) = args.get("path").and_then(Value::as_str) {
            let path = path.trim();
            if path.is_empty() {
                return Err("path 不能为空".to_owned());
            }
            if kind == "dcp" {
                self.set_dcp_profile(path)?;
            } else {
                self.set_lcp_profile(path)?;
            }
            return Ok(self.profile_json());
        }
        if args.get("auto").and_then(Value::as_bool).unwrap_or(false) {
            if kind == "dcp" {
                self.match_dcp_profile();
            } else {
                self.match_lcp_profile();
            }
        }
        Ok(self.profile_json())
    }

    /// 取景框摘要：`crop.set` 与 `sequence.info` 共用一份口径。
    /// `manual` 是用户拖出来的框（没拖过为 null），`effective` 是叠上拉直安全框、
    /// 对齐安全框与画幅比例后、真正会导出的那个。
    fn crop_json(&self) -> Value {
        let (w, h) = self.resolution.unwrap_or((0, 0));
        json!({
            "frame": {"width": w, "height": h},
            "aspect": self.aspect.label(),
            "rotate": self.rotate,
            "manual": self.user_crop.map(rect_json),
            "effective": self.effective_crop().map(rect_json),
            "editing": self.crop_editing(),
        })
    }

    /// 读或改写取景框。`x/y/width/height` 一起给就整体替换（源像素，几何规则交给
    /// `aevum_core::crop`：取偶、夹进画面）；`reset` 取消手动框和拉直；`aspect` 换画幅比例；
    /// `rotate` 拉直（度，正角=画面逆时针，±45 内）；什么都不给就是读回当前取景。
    fn cmd_crop_set(&mut self, args: &Value) -> Result<Value, String> {
        if let Some(v) = args.get("aspect") {
            let text = v.as_str().ok_or("aspect 必须是字符串，例如 \"16:9\"")?;
            self.set_aspect(aspect_arg(text)?);
        }
        if let Some(v) = args.get("rotate") {
            let deg = v.as_f64().ok_or("rotate 必须是数字（度）")?;
            if !deg.is_finite() || deg.abs() > 45.0 {
                return Err("拉直角度要在 -45..45 之间".to_owned());
            }
            self.set_rotate(deg);
        }
        if args.get("reset").and_then(Value::as_bool).unwrap_or(false) {
            self.crop_reset();
            self.set_rotate(0.0);
            return Ok(self.crop_json());
        }
        let keys = ["x", "y", "width", "height"];
        if keys.iter().any(|k| args.get(*k).is_some()) {
            if !keys.iter().all(|k| args.get(*k).is_some()) {
                return Err(
                    "取景框要一次性给全 x / y / width / height，缺一个只会画出半套框".to_owned(),
                );
            }
            let (fw, fh) = self.resolution.ok_or("还没有打开序列，无从取景")?;
            let mut v = [0u32; 4];
            for (i, k) in keys.iter().enumerate() {
                let x = args[*k]
                    .as_f64()
                    .ok_or_else(|| format!("{k} 必须是数字（源像素）"))?;
                if !x.is_finite() || x < 0.0 || x > f64::from(fw.max(fh)) {
                    return Err(format!("{k} 超出画面范围（0..{}）", fw.max(fh)));
                }
                v[i] = x.round() as u32;
            }
            let next = crop::norm_rect(
                f64::from(v[0]),
                f64::from(v[1]),
                f64::from(v[0] + v[2]),
                f64::from(v[1] + v[3]),
                fw,
                fh,
            );
            self.user_crop = Some(self.clamp_to_view(next));
            self.crop_drag = None;
            self.preview_dirty = true;
        }
        Ok(self.crop_json())
    }

    /// 成片输出摘要：`render.set` 的回执与查询用同一份口径。
    /// `*_applies` 那几项是诚实开关——容器不吃的参数摆出来就要说清它现在不起作用。
    fn render_json(&self) -> Value {
        json!({
            "mode": mode_name(self.render_mode),
            "format": self.render_format.key(),
            "format_cn": self.render_format.cn(),
            "quality": self.render_quality,
            "quality_applies": self.render_format.lossy(),
            "keep_exif": self.render_keep_exif,
            "exif_applies": self.render_format.supports_exif(),
            "xmp_sidecar": self.render_xmp_sidecar,
            // 旁车是静帧的事：视频模式导出的中间帧不发旁车。
            "sidecar_applies": self.render_mode == OutputMode::Stills,
            "color_space": self.render_color_space.key(),
            "width": self.output_w,
            "height": self.output_h,
            "sharpen_for": self.render_sharpen_for.key(),
            "sharpen_strength": self.render_sharpen_strength.key(),
            // 水印整块照 `Watermark` 自己的形状吐出去：查到的那份可以原样写回来。
            "watermark": self.render_watermark,
            "watermark_applies": self.render_watermark.is_on(),
            "watermark_spots": Spot::ALL.iter().map(|s| s.key()).collect::<Vec<_>>(),
            "watermark_kinds": WatermarkKind::ALL.iter().map(|k| k.key()).collect::<Vec<_>>(),
            "out_dir": self.render_out_dir.display().to_string(),
            "formats": StillFormat::ALL.iter().map(|f| f.key()).collect::<Vec<_>>(),
            "sharpen_uses": OutputSharpen::ALL.iter().map(|s| s.key()).collect::<Vec<_>>(),
        })
    }

    /// 落一个容器：视频模式的中间帧只能无损，撞上有损档直接报错，别让 core 的
    /// 守卫等到导出那一步才说话。
    fn set_render_format(&mut self, format: StillFormat) -> Result<(), String> {
        if self.render_mode == OutputMode::Video && !format.frames_ok() {
            return Err(format!(
                "{} 只能出静帧；视频的中间帧要 PNG 或 TIFF（先把 mode 改成 stills）",
                format.cn()
            ));
        }
        self.render_format = format;
        Ok(())
    }

    /// 改水印：只写要改的那几键，其余保持面板当前值。
    ///
    /// 合并是把当前设定先摊平成 map 再覆盖——`Watermark` 上的 `serde(default)` 只补
    /// 结构里根本没有的键，所以「改个不透明度把正文清空」这种惊喜不会出现，写进来的
    /// 键名也和工程文件、`render.set` 查询回执用同一套词。
    fn patch_watermark(&mut self, value: &Value) -> Result<(), String> {
        let incoming = value
            .as_object()
            .ok_or("watermark 必须是对象（enabled / kind / text / image / spot / opacity / size / margin / color）")?;
        let base = serde_json::to_value(&self.render_watermark)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let mut merged = base.clone();
        for (key, v) in incoming {
            if !base.contains_key(key) {
                let keys = base
                    .keys()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ");
                return Err(format!("未知的水印参数 {key}；可用 {keys}"));
            }
            merged.insert(key.clone(), v.clone());
        }
        let next: Watermark = serde_json::from_value(Value::Object(merged))
            .map_err(|e| format!("水印参数不对：{e}"))?;
        if incoming.contains_key("image")
            && !next.image.as_os_str().is_empty()
            && !next.image.is_file()
        {
            let shown = next.image.display();
            return Err(format!("水印图片不存在：{shown}（传空串表示不换/清空）"));
        }
        for (key, slot, max) in [
            ("opacity", i32::from(next.opacity), 100),
            ("size", i32::from(next.size), 40),
            ("margin", i32::from(next.margin), 40),
        ] {
            if incoming.contains_key(key) && slot > max {
                return Err(format!("{key} 要在 0..{max}（相对输出尺寸的百分比）"));
            }
        }
        self.render_watermark = next;
        Ok(())
    }

    /// 读或改成片输出设置：容器、质量、EXIF 搬运、色域、规格、输出锐化、水印、目录。
    fn cmd_render_set(&mut self, args: &Value) -> Result<Value, String> {
        // 模式与容器是一对：先各自解析，凑不成合法组合就整条拒绝，
        // 别改一半留下「视频模式 + 有损中间帧」这种导不出、又没记进历史的态。
        let mode = match args.get("mode") {
            Some(v) => {
                let raw = v.as_str().ok_or("mode 必须是字符串（stills / video）")?;
                match raw.trim().to_ascii_lowercase().as_str() {
                    "stills" | "still" | "images" => OutputMode::Stills,
                    "video" | "mp4" => OutputMode::Video,
                    other => return Err(format!("未知输出模式 {other}；可用 stills / video")),
                }
            }
            None => self.render_mode,
        };
        let format = match args.get("format") {
            Some(v) => {
                let raw = v.as_str().ok_or("format 必须是字符串")?;
                StillFormat::parse(raw).ok_or_else(|| {
                    format!("未知容器 {raw}；可用 png8 / tiff16 / jpeg / webp / avif")
                })?
            }
            None => self.render_format,
        };
        if mode == OutputMode::Video && !format.frames_ok() {
            return Err(format!(
                "{} 只能出静帧；视频的中间帧要 PNG 或 TIFF（同一条命令里把 format 一起改掉）",
                format.cn()
            ));
        }
        self.render_mode = mode;
        self.render_format = format;
        if let Some(q) = opt_u32(args, "quality")? {
            if !(1..=100).contains(&q) {
                return Err("quality 要在 1..100".to_owned());
            }
            self.render_quality = q as u8;
        }
        if let Some(v) = args.get("keep_exif") {
            self.render_keep_exif = v.as_bool().ok_or("keep_exif 必须是布尔")?;
        }
        if let Some(v) = args.get("xmp_sidecar") {
            self.render_xmp_sidecar = v.as_bool().ok_or("xmp_sidecar 必须是布尔")?;
        }
        if let Some(v) = args.get("resume") {
            self.render_resume = v.as_bool().ok_or("resume 必须是布尔")?;
        }
        if let Some(v) = args.get("sharpen_for") {
            let raw = v.as_str().ok_or("sharpen_for 必须是字符串")?;
            self.render_sharpen_for = OutputSharpen::parse(raw)
                .ok_or_else(|| format!("未知锐化用途 {raw}；可用 off / screen / glossy / matte"))?;
        }
        if let Some(v) = args.get("sharpen_strength") {
            let raw = v.as_str().ok_or("sharpen_strength 必须是字符串")?;
            self.render_sharpen_strength = SharpenStrength::parse(raw)
                .ok_or_else(|| format!("未知锐化强度 {raw}；可用 low / standard / high"))?;
        }
        if let Some(raw) = opt_str(args, "color_space")? {
            self.render_color_space = ColorSpace::parse(raw)
                .ok_or_else(|| format!("未知色域 {raw}；可用 srgb / p3 / rec2020"))?;
        }
        if let Some(v) = args.get("watermark") {
            self.patch_watermark(v)?;
        }
        match (opt_u32(args, "width")?, opt_u32(args, "height")?) {
            (Some(width), Some(height)) => {
                if width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1 {
                    return Err(format!(
                        "width/height 必须是偶数且不小于 2：{width}×{height}"
                    ));
                }
                self.output_w = width;
                self.output_h = height;
            }
            (None, None) => {}
            _ => return Err("width 与 height 需同时给出（或都不给，保持当前规格）".to_owned()),
        }
        if let Some(raw) = opt_str(args, "out_dir")? {
            if raw.trim().is_empty() {
                return Err("out_dir 不能为空；省略则保持当前目录".to_owned());
            }
            let dir = PathBuf::from(raw);
            if let Some(seq) = &self.seq {
                ensure_out_dir_safe(&seq.root, &dir)?;
            }
            self.render_out_dir = dir;
        }
        Ok(self.render_json())
    }

    /// 当前帧的拍摄参数。整块读不到（PNG、无 EXIF 的处理过的图）返回 `null`，
    /// 面板和 agent 都按「这条没有」处理，不摆一排空值。
    pub fn exif_json(&self) -> Value {
        let Some(e) = self.exif.get(self.selected) else {
            return Value::Null;
        };
        if e == &Exif::default() {
            return Value::Null;
        }
        json!({
            "make": e.make(),
            "model": e.model(),
            "lens": e.lens(),
            "datetime": e.datetime(),
            "iso": e.iso(),
            "shutter": e.shutter_text(),
            "shutter_sec": e.exposure_sec().map(round6),
            "aperture": e.fnum().map(|v| (v * 10.0).round() / 10.0),
            "focal_mm": e.focal_mm().map(|v| (v * 10.0).round() / 10.0),
            "ev": e.ev().map(round6),
            "orientation": e.orientation,
        })
    }

    fn cmd_sequence_info(&self) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let resolution = match self.resolution {
            Some((w, h)) => json!({"width": w, "height": h}),
            None => Value::Null,
        };
        let frame_name = seq
            .frames
            .get(self.selected)
            .map(|f| f.name.clone())
            .unwrap_or_default();
        let issues: Vec<Value> = self
            .issues
            .iter()
            .take(50)
            .map(|(i, msg)| json!({"frame": i + 1, "error": msg}))
            .collect();
        let align = match &self.align_report {
            Some(report) => json!({
                "available": true,
                "reference": self.align_ref + 1,
                "estimator": estimator_name(self.align_estimator),
                "frames": report.offsets.len(),
                "crop": rect_json(report.crop),
                "issues": report.issues.len(),
                "elapsed_secs": round6(report.elapsed_secs),
            }),
            None => json!({
                "available": false,
                "reference": self.align_ref + 1,
                "estimator": estimator_name(self.align_estimator),
            }),
        };
        let project_path = project::project_path(&seq.root);
        Ok(json!({
            "root": seq.root.display().to_string(),
            "name": seq.display_name(),
            "frames": seq.len(),
            "resolution": resolution,
            "missing_frames": issues,
            "missing_count": self.issues.len(),
            "measured": self.metrics.iter().filter(|m| m.is_some()).count(),
            "measuring": self.measuring,
            "selected": self.selected + 1,
            "frame_name": frame_name,
            "exif": self.exif_json(),
            "module": self.module.cn(),
            "module_index": self.module.index(),
            "preset": self.preset,
            "deflicker": self.deflicker_json(),
            "align": align,
            "crop": self.crop_json(),
            "export": {
                "busy": self.export_busy,
                "mode": mode_name(self.render_mode),
                "last_saved": self.last_saved,
            },
            "project": {
                "path": project_path.display().to_string(),
                "exists": project_path.is_file(),
            },
        }))
    }

    fn cmd_sequence_open(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let raw = arg_str(args, "root")?;
        let path = PathBuf::from(raw);
        if !path.is_dir() && !path.is_file() {
            return Err(format!("路径不存在：{}", path.display()));
        }
        // open 只在真正换成新序列时才递增 reload_token；用它判断是否打开成功，
        // 否则失败时会误把旧序列当成新打开的序列返回给 agent。
        let token = self.reload_token;
        self.open(ctx, path);
        if self.reload_token == token {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "打开失败：这里没有可用的图片".to_owned()));
        }
        let mut info = self.cmd_sequence_info()?;
        if let Some(object) = info.as_object_mut() {
            object.insert("opened".to_owned(), json!(true));
        }
        Ok(info)
    }

    // ---------------------------------------------------------------- 图库命令

    fn cmd_library_add(&mut self, args: &Value) -> Result<Value, String> {
        let raw = arg_str(args, "path")?;
        let path = PathBuf::from(raw);
        if !path.is_dir() {
            return Err(format!("不是目录：{}", path.display()));
        }
        let index = self.lib_add(&path);
        let summary = self
            .lib_entries
            .get(index)
            .map(|f| aevum_core::library::summarize(f))
            .unwrap_or_default();
        self.status = format!("图库加目录：{}", path.display());
        Ok(json!({
            "index": index + 1,
            "roots": self.lib_roots.len(),
            "summary": summary,
        }))
    }

    fn cmd_library_remove(&mut self, args: &Value) -> Result<Value, String> {
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        if index == 0 || index as usize > self.lib_roots.len() {
            return Err(format!("index 超出范围：1..={}", self.lib_roots.len()));
        }
        let path = self.lib_roots[index as usize - 1].display().to_string();
        self.lib_remove(index as usize - 1);
        self.status = format!("图库移除目录：{path}");
        Ok(json!({"removed": path, "roots": self.lib_roots.len()}))
    }

    fn cmd_library_list(&mut self, args: &Value) -> Result<Value, String> {
        let scan = opt_u32(args, "index")?;
        if let Some(n) = scan {
            if n == 0 || n as usize > self.lib_roots.len() {
                return Err(format!("index 超出范围：1..={}", self.lib_roots.len()));
            }
            self.lib_scan(n as usize - 1);
        }
        let roots: Vec<Value> = self
            .lib_roots
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let files = self.lib_entries.get(i);
                let mut row = json!({
                    "index": i + 1,
                    "path": r.display().to_string(),
                    "name": r
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| r.display().to_string()),
                    "scanned": files.is_some_and(|f| !f.is_empty()),
                });
                if let Some(f) = files.filter(|f| !f.is_empty()) {
                    row["summary"] = json!(aevum_core::library::summarize(f));
                    if scan == Some(i as u32 + 1) {
                        row["files"] = json!(
                            f.iter()
                                .map(|e| json!({
                                    "frame": e.index + 1,
                                    "name": e.name,
                                    "path": e.path.display().to_string(),
                                    "rating": e.rating,
                                    "flag": e.flag,
                                    "label": e.label,
                                    "keywords": e.keywords,
                                }))
                                .collect::<Vec<_>>()
                        );
                        row["keywords"] = json!(
                            aevum_core::library::keyword_counts(f)
                                .into_iter()
                                .map(|(w, n)| json!({"keyword": w, "count": n}))
                                .collect::<Vec<_>>()
                        );
                    }
                }
                row
            })
            .collect();
        let collections: Vec<Value> = self
            .lib_cols
            .iter()
            .enumerate()
            .map(|(i, c)| json!({"index": i + 1, "name": c.name, "count": c.files.len()}))
            .collect();
        Ok(json!({
            "roots": roots,
            "collections": collections,
            "selected": self.lib_sel.map(|i| i + 1),
        }))
    }

    fn cmd_library_open(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        if index == 0 || index as usize > self.lib_roots.len() {
            return Err(format!("index 超出范围：1..={}", self.lib_roots.len()));
        }
        let dir = self.lib_roots[index as usize - 1].clone();
        // 与 sequence.open 同一套口径：reload_token 没动说明打开失败，
        // 别把旧序列当成新目录回给 agent。
        let token = self.reload_token;
        self.open(ctx, dir);
        if self.reload_token == token {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "打开失败：这里没有可用的图片".to_owned()));
        }
        let mut info = self.cmd_sequence_info()?;
        if let Some(object) = info.as_object_mut() {
            object.insert("opened".to_owned(), json!(true));
        }
        Ok(info)
    }

    /// 图库里的根目录序号：省略就用当前展开的那个。
    fn lib_root_arg(&self, args: &Value) -> Result<usize, String> {
        match opt_u32(args, "root")? {
            Some(0) => Err("root 从 1 开始".to_owned()),
            Some(n) if n as usize > self.lib_roots.len() => {
                Err(format!("root 超出范围：1..={}", self.lib_roots.len()))
            }
            Some(n) => Ok(n as usize - 1),
            None => self
                .lib_sel
                .ok_or_else(|| "还没展开图库目录：先给 root，或 library.list 带 index".to_owned()),
        }
    }

    /// 编目里的帧号（1 开始）→（目录序号, 帧序），顺带校验帧数。
    fn lib_frame_arg(&self, args: &Value, root: usize) -> Result<(usize, usize), String> {
        let frame =
            opt_u32(args, "frame")?.ok_or_else(|| "缺少整数参数 frame（从 1 开始）".to_owned())?;
        let count = self.lib_entries.get(root).map(|v| v.len()).unwrap_or(0);
        if count == 0 {
            return Err("这个目录还没编目：先 library.list 带 index".to_owned());
        }
        if frame == 0 || frame as usize > count {
            return Err(format!("frame 超出范围：1..={count}"));
        }
        Ok((root, frame as usize - 1))
    }

    /// `frame`（单帧）或 `frames`（数组）→ 0 开始的帧号；两者只能给一个。
    fn lib_frames_arg(&self, args: &Value, root: usize) -> Result<Vec<usize>, String> {
        match args.get("frames") {
            None | Some(Value::Null) => {
                let (_, frame) = self.lib_frame_arg(args, root)?;
                Ok(vec![frame])
            }
            Some(Value::Array(raw)) => {
                if !matches!(args.get("frame"), None | Some(Value::Null)) {
                    return Err("frame 与 frames 只能给一个".to_owned());
                }
                if raw.is_empty() {
                    return Err("frames 不能是空数组".to_owned());
                }
                let count = self.lib_entries.get(root).map(|v| v.len()).unwrap_or(0);
                if count == 0 {
                    return Err("这个目录还没编目：先 library.list 带 index".to_owned());
                }
                raw.iter()
                    .map(|v| {
                        v.as_u64()
                            .filter(|&n| n >= 1 && n as usize <= count)
                            .map(|n| n as usize - 1)
                            .ok_or_else(|| format!("frames 里的帧号超出范围：1..={count}"))
                    })
                    .collect()
            }
            Some(_) => Err("frames 必须是帧号数组".to_owned()),
        }
    }

    fn cmd_library_keyword(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.lib_root_arg(args)?;
        let frames = self.lib_frames_arg(args, root)?;
        let add = words_arg(args, "add")?;
        let remove = words_arg(args, "remove")?;
        if add.is_empty() && remove.is_empty() {
            return Err("至少给 add 或 remove（关键词）".to_owned());
        }
        let touched = self.lib_keyword_edit_many(root, &frames, &add, &remove)?;
        let words = self
            .lib_entries
            .get(root)
            .and_then(|v| v.get(frames[0]))
            .map(|f| f.keywords.clone())
            .unwrap_or_default();
        self.status = format!(
            "图库：{} 张改词（+{} / -{}），第 {} 帧现有 {} 个",
            touched,
            add.len(),
            remove.len(),
            frames[0] + 1,
            words.len()
        );
        Ok(json!({
            "root": root + 1,
            "frames": frames.iter().map(|f| f + 1).collect::<Vec<_>>(),
            "touched": touched,
            "keywords": words,
        }))
    }

    /// 图库里某个目录的说明与版权：不带参数是查询，带了就改那几项并立刻落盘。
    /// 目录正好是打开着的序列时以内存那份为准，改完两边同步——不然面板上还没
    /// 自动保存的编辑会被这条命令读丢，或者被工程写回盖掉。
    fn cmd_library_metadata(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.lib_root_arg(args)?;
        let dir = self
            .lib_roots
            .get(root)
            .cloned()
            .ok_or_else(|| "这个目录已经从图库移除了".to_owned())?;
        let open_here = self.seq.as_ref().is_some_and(|s| s.root == dir);
        let mut meta = if open_here {
            self.meta.clone()
        } else {
            aevum_core::library::metadata(&dir)
        };
        let changed = apply_metadata_args(&mut meta, args)?;
        if changed {
            meta = aevum_core::library::set_metadata(&dir, &meta)
                .map_err(|e| format!("写元数据失败：{e}"))?;
            if open_here {
                self.meta = meta.clone();
            }
            if self.lib_meta_root.as_deref() == Some(dir.as_path()) {
                self.lib_meta = meta.clone();
            }
            self.status = format!(
                "图库：{} 的说明与版权已写入工程",
                dir.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| dir.display().to_string())
            );
        }
        let mut out = metadata_json(&meta);
        out["root"] = json!(root + 1);
        out["path"] = json!(project::project_path(&dir).display().to_string());
        out["saved"] = json!(changed);
        Ok(out)
    }

    /// 图库堆叠：`op` = list（默认）/ create / dissolve / auto，都原子写回该目录的工程文件。
    /// 叠号在返回里一律 1 开始，帧号也一样；create 会把成员从原叠里摘走，
    /// 老叠摘剩一张就自动消失，所以每次都要看返回的整份堆叠，别缓存旧叠号。
    fn cmd_library_stack(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.lib_root_arg(args)?;
        let op = match args.get("op") {
            None | Some(Value::Null) => "list",
            Some(Value::String(s)) => s.as_str(),
            Some(_) => return Err("op 必须是字符串".to_owned()),
        };
        let dir = self.lib_dir(root)?;
        let stacks = match op {
            "list" => aevum_core::library::stacks(&dir),
            "create" => {
                let frames = self.lib_frames_arg(args, root)?;
                if frames.len() < 2 {
                    return Err("堆叠至少要两帧：用 frames 数组一次给全".to_owned());
                }
                self.lib_stack_create(root, &frames)?
            }
            "dissolve" => {
                let n = opt_u32(args, "stack")?
                    .ok_or_else(|| "dissolve 要带 stack（叠号，1 开始）".to_owned())?;
                if n == 0 {
                    return Err("stack 从 1 开始".to_owned());
                }
                self.lib_stack_dissolve(root, n as usize - 1)?
            }
            "auto" => self.lib_stack_auto(root)?,
            other => {
                return Err(format!(
                    "认不出来的 op：{other}（要 list / create / dissolve / auto）"
                ));
            }
        };
        if op != "list" {
            self.status = format!(
                "图库：{} 现在有 {} 叠",
                dir.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| dir.display().to_string()),
                stacks.len()
            );
        }
        let rows: Vec<Value> = stacks
            .iter()
            .enumerate()
            .map(|(i, group)| {
                json!({
                    "stack": i + 1,
                    "head": group[0] + 1,
                    "count": group.len(),
                    "frames": group.iter().map(|f| f + 1).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({
            "root": root + 1,
            "op": op,
            "count": stacks.len(),
            "stacks": rows,
        }))
    }

    fn cmd_library_organize(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.lib_root_arg(args)?;
        let patch = self.organize_patch(args)?;
        let frames = self.lib_frames_arg(args, root)?;
        if let [frame] = frames[..] {
            self.lib_organize(root, frame, patch)?;
            let f = self
                .lib_entries
                .get(root)
                .and_then(|v| v.get(frame))
                .cloned()
                .ok_or_else(|| "编目行没了（目录可能已移除）".to_owned())?;
            self.status = format!(
                "图库 {}：{} 星 · {} · 色标{}",
                f.name,
                f.rating,
                match f.flag {
                    1 => "选用",
                    -1 => "排除",
                    _ => "未标记",
                },
                LABEL_NAMES[f.label.min(4) as usize]
            );
            return Ok(json!({
                "root": root + 1,
                "frame": frame + 1,
                "name": f.name,
                "rating": f.rating,
                "flag": f.flag,
                "label": f.label,
            }));
        }
        // 批量的写回在 lib_organize_many 里一次落盘，这里只补内存与汇报。
        self.lib_organize_many(root, &frames, patch)?;
        self.status = format!(
            "图库：{} 张已整理（{} 星 · {} · 色标{}）",
            frames.len(),
            patch.rating.unwrap_or(0).min(5),
            match patch.flag.unwrap_or(0) {
                1 => "选用",
                -1 => "排除",
                _ => "未标记",
            },
            LABEL_NAMES[patch.label.unwrap_or(0).min(4) as usize]
        );
        let frames: Vec<usize> = frames.iter().map(|f| f + 1).collect();
        Ok(json!({
            "root": root + 1,
            "frames": frames,
            "count": frames.len(),
        }))
    }

    /// `rating` / `flag` / `label` 三个整理参数：范围校验 + 至少给一个。
    fn organize_patch(&self, args: &Value) -> Result<aevum_core::library::Organize, String> {
        let rating = opt_u32(args, "rating")?;
        let label = opt_u32(args, "label")?;
        let flag = match args.get("flag") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_i64()
                    .filter(|f| (-1..=1).contains(f))
                    .ok_or_else(|| "flag 只能是 -1（排除）/ 0 / 1（选用）".to_owned())?,
            ),
        };
        if let Some(r) = rating
            && r > 5
        {
            return Err("rating 只能是 0~5".to_owned());
        }
        if let Some(l) = label
            && l > 4
        {
            return Err("label 只能是 0~4（0=无，1=红，2=黄，3=绿，4=蓝）".to_owned());
        }
        if rating.is_none() && label.is_none() && flag.is_none() {
            return Err("至少给 rating / flag / label 之一".to_owned());
        }
        Ok(aevum_core::library::Organize {
            rating: rating.map(|r| r as u8),
            flag: flag.map(|f| f as i8),
            label: label.map(|l| l as u8),
        })
    }

    /// 集合寻址：`index`（1 开始）或 `name` 二选一。
    fn collection_at(&self, args: &Value) -> Result<usize, String> {
        if let Some(n) = opt_u32(args, "index")? {
            if n == 0 || n as usize > self.lib_cols.len() {
                return Err(format!("index 超出范围：1..={}", self.lib_cols.len()));
            }
            return Ok(n as usize - 1);
        }
        let name = opt_str(args, "name")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "缺少 index（1 开始）或 name（集合名）".to_owned())?;
        self.lib_cols
            .iter()
            .position(|c| c.name == name)
            .ok_or_else(|| format!("没有叫「{name}」的集合：先 collection.create"))
    }

    fn cmd_collection_create(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let index = self.lib_collection_create(name);
        let name = self.lib_cols[index].name.clone();
        self.status = format!("新建集合「{name}」");
        Ok(json!({
            "index": index + 1,
            "name": name,
            "count": self.lib_cols[index].files.len(),
        }))
    }

    fn cmd_collection_add(&mut self, args: &Value) -> Result<Value, String> {
        let col = self.collection_at(args)?;
        let root = self.lib_root_arg(args)?;
        let (root, frame) = self.lib_frame_arg(args, root)?;
        let path = self.lib_entries[root][frame].path.clone();
        let added = self.lib_collection_put(col, &path)?;
        let c = &self.lib_cols[col];
        self.status = if added {
            format!("「{}」加入集合「{}」", path.display(), c.name)
        } else {
            format!("「{}」已经在集合「{}」里", path.display(), c.name)
        };
        Ok(json!({
            "index": col + 1,
            "name": c.name,
            "added": added,
            "count": c.files.len(),
        }))
    }

    fn cmd_collection_remove(&mut self, args: &Value) -> Result<Value, String> {
        let col = self.collection_at(args)?;
        // 成员按路径寻址：集合视图里还没编目的行也能拿掉。
        let path = match opt_str(args, "path")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(path) => PathBuf::from(path),
            None => {
                let root = self.lib_root_arg(args)?;
                let (root, frame) = self.lib_frame_arg(args, root)?;
                self.lib_entries[root][frame].path.clone()
            }
        };
        let removed = self.lib_collection_drop(col, &path);
        let c = &self.lib_cols[col];
        Ok(json!({
            "index": col + 1,
            "name": c.name,
            "removed": removed,
            "count": c.files.len(),
        }))
    }

    fn cmd_collection_delete(&mut self, args: &Value) -> Result<Value, String> {
        let col = self.collection_at(args)?;
        let name = self
            .lib_cols
            .get(col)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.lib_collection_delete(col);
        self.status = format!("删除集合「{name}」");
        Ok(json!({"deleted": name, "collections": self.lib_cols.len()}))
    }

    fn cmd_collection_list(&mut self, args: &Value) -> Result<Value, String> {
        let open = match opt_u32(args, "index")? {
            Some(n) => Some(self.collection_at(&json!({"index": n}))?),
            None => match opt_str(args, "name")? {
                Some(_) => Some(self.collection_at(args)?),
                None => None,
            },
        };
        let rows: Vec<Value> = self
            .lib_cols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut row = json!({
                    "index": i + 1,
                    "name": c.name,
                    "count": c.files.len(),
                });
                if open == Some(i) {
                    row["files"] = json!(c.files);
                }
                row
            })
            .collect();
        Ok(json!({"collections": rows}))
    }

    fn cmd_frame_select(&mut self, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        if index == 0 || index as usize > n {
            return Err(format!("index 超出范围：1..={n}"));
        }
        self.set_frame(index as usize - 1);
        self.preview_dirty = true;
        let name = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.name.clone())
            .unwrap_or_default();
        self.status = format!("已跳到第 {} 帧 · {name}", self.selected + 1);
        Ok(json!({
            "selected": self.selected + 1,
            "frames": n,
            "name": name,
        }))
    }

    fn cmd_ui_module(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let module = Module::from_name(name)
            .ok_or_else(|| format!("未知模块 {name}；可用 序列/去闪/显影/构图/成片/声音 或 0-5"))?;
        self.set_module(module);
        self.status = format!("模块：{}", module.cn());
        Ok(json!({"module": module.cn(), "index": module.index()}))
    }

    fn cmd_ui_dock(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let tab = DockTab::from_name(name)
            .ok_or_else(|| format!("未知面板 {name}；可用 调整/信息/关键帧/AI 或 0-3"))?;
        self.dock_tab = tab;
        self.show_dock = true;
        self.status = format!("面板：{}", tab.cn());
        Ok(json!({
            "tab": tab.cn(),
            "index": tab.index(),
            "visible": self.show_dock,
        }))
    }

    fn grade_json(&self) -> Value {
        json!({
            "preset": self.preset,
            "params": serde_json::to_value(&self.params).unwrap_or(Value::Null),
            "copies": self.copies.iter().map(|c| json!({
                "name": c.name,
                "keyframes": c.keyframes.len(),
            })).collect::<Vec<_>>(),
            "active_copy": self.active_copy(),
        })
    }

    /// 命名调色副本：list / save / apply / delete / rename。
    /// 存与覆盖记的是「当前调色 + 关键帧」整份；套用把副本取回当前。
    fn cmd_grade_copy(&mut self, args: &Value) -> Result<Value, String> {
        let op = args.get("op").and_then(Value::as_str).unwrap_or("list");
        let name = args.get("name").and_then(Value::as_str).unwrap_or("");
        match op {
            "list" => Ok(self.grade_json()),
            "save" => {
                let overwrite = args
                    .get("overwrite")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let index = self.save_copy(name, overwrite)?;
                Ok(json!({
                    "saved": self.copies[index].name,
                    "index": index + 1,
                    "copies": self.copies.len(),
                }))
            }
            "apply" => {
                self.apply_copy(name)?;
                Ok(self.grade_json())
            }
            "delete" => {
                self.delete_copy(name)?;
                Ok(json!({"deleted": name, "copies": self.copies.len()}))
            }
            "rename" => {
                let to = args.get("to").and_then(Value::as_str).unwrap_or("");
                self.rename_copy(name, to)?;
                Ok(json!({"renamed": to, "copies": self.copies.len()}))
            }
            other => Err(format!(
                "未知 op {other}；可用 list / save / apply / delete / rename"
            )),
        }
    }

    /// HDR 合并：同场景多张不同曝光融成一张静帧（Mertens 曝光融合）。
    /// 帧源二选一：`frames` 显式帧号数组，或 `center` ± `around`（默认当前帧 ±1）。
    /// 对齐跑过就先套统一裁切 + 逐帧位移，与导出同一条几何路。
    ///
    /// 长任务：只校验参数、抓一份输入快照，然后起后台线程并立刻返回。
    /// 9 张 6K 帧融一遍要 6 秒，放在界面线程上就是一段死窗口。
    fn cmd_hdr_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let n = seq.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(format!(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                self.merge_kind.map(MergeKind::cn).unwrap_or("合并")
            ));
        }
        if !confirm(args) {
            return Err("hdr.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let indices: Vec<usize> = match args.get("frames") {
            Some(list) => list
                .as_array()
                .ok_or_else(|| "frames 要是帧号数组，例如 [0,1,2]".to_owned())?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|i| ((i as usize) < n).then_some(i as usize))
                        .ok_or_else(|| format!("帧号越界：{v}（序列共 {n} 帧）"))
                })
                .collect::<Result<_, _>>()?,
            None => {
                let around = opt_u32(args, "around")?.unwrap_or(1) as usize;
                let center = opt_u32(args, "center")?
                    .map(|v| v as usize)
                    .unwrap_or(self.selected.min(n - 1));
                if center >= n {
                    return Err(format!("center 越界：{center}（序列共 {n} 帧）"));
                }
                (center.saturating_sub(around)..=center.saturating_add(around).min(n - 1)).collect()
            }
        };
        let kind = MergeKind::Hdr;
        if indices.len() < 2 {
            return Err("HDR 合并至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
        }
        if indices.len() > kind.max_frames() {
            return Err(format!(
                "HDR 合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑关键几帧",
                kind.max_frames(),
                indices.len()
            ));
        }
        let root = seq.root.clone();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| format!("建输出目录失败：{e}"))?;

        let job = self.build_merge_job(kind, indices, &out_dir)?;
        Ok(self.start_merge(ctx, job))
    }

    /// 合并的落盘目录：`out_dir` > 成片面板的输出目录 > 序列根下的默认子目录。
    fn merge_out_dir(&self, args: &Value, root: &Path, kind: MergeKind) -> Result<PathBuf, String> {
        match opt_str(args, "out_dir")? {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                if dir.as_os_str().is_empty() {
                    return Err("out_dir 不能为空；省略则用当前输出目录".to_owned());
                }
                Ok(dir)
            }
            None if self.render_out_dir.as_os_str().is_empty() => Ok(root.join(kind.dir_name())),
            None => Ok(self.render_out_dir.clone()),
        }
    }

    /// 抓一份合并任务的输入：帧路径、逐帧几何、成片路径与输出旋钮。
    /// 后台线程拿不到 `AppState`，所以该读的都在这里读完。
    fn build_merge_job(
        &mut self,
        kind: MergeKind,
        indices: Vec<usize>,
        out_dir: &Path,
    ) -> Result<MergeJob, String> {
        // 调用方大多先过 require_sequence，但 MCP 通道可能绕过：缺序列就
        // 报错返回，而不是在后台线程里 panic（panic 只会静默吞掉任务）。
        let seq = self
            .seq
            .as_ref()
            .ok_or_else(|| "合并任务需要先打开一个序列".to_owned())?;
        let paths = seq.paths();
        let srcs: Vec<PathBuf> = indices.iter().map(|i| paths[*i].clone()).collect();
        let framing: Vec<(Option<CropRect>, [f64; 2])> = match kind {
            MergeKind::Hdr => indices.iter().map(|i| self.alignment_at(*i)).collect(),
            // 全景靠特征点自己求单应，几何全交给拼接，不套对齐位移。
            MergeKind::Pano => indices.iter().map(|_| (None, [0.0, 0.0])).collect(),
        };
        let options = self.frame_render_options();
        let dst = out_dir.join(format!(
            "{}_{:06}_{}f.{}",
            kind.tag(),
            indices[0],
            indices.len(),
            options.format.ext()
        ));
        Ok(MergeJob {
            kind,
            indices,
            srcs,
            framing,
            dst,
            options,
        })
    }

    /// 起后台合并任务并立刻返回一份「已启动」的 JSON；结果与进度走 `merge.status`
    /// 与面板回显（`Msg::Merged` 落账）。
    fn start_merge(&mut self, ctx: &Context, job: MergeJob) -> Value {
        let kind = job.kind;
        let frames = job.indices.len();
        let indices = job.indices.clone();
        let out = job.dst.clone();
        self.merge_busy = true;
        self.merge_kind = Some(kind);
        self.merge_result = None;
        self.merge_error = None;
        match kind {
            MergeKind::Hdr => {
                self.hdr_last = None;
                self.hdr_error = None;
            }
            MergeKind::Pano => {
                self.pano_last = None;
                self.pano_error = None;
            }
        }
        self.cancel_merge.store(false, Ordering::SeqCst);
        self.merge_done.store(0, Ordering::Relaxed);
        // 进度 = 读好的帧数 + 最后那一段（融合或拼接再写盘）。
        self.merge_total.store(frames + 1, Ordering::Relaxed);
        self.status = format!("正在{}…", kind.cn());

        let token = self.reload_token;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let done = self.merge_done.clone();
        let total = self.merge_total.clone();
        let cancel = self.cancel_merge.clone();
        thread::spawn(move || {
            let result = run_merge_job(&job, &done, &total, &cancel);
            let _ = tx.send(Msg::Merged {
                token,
                kind,
                result,
            });
            ctx2.request_repaint();
        });
        json!({
            "started": true,
            "kind": kind.tag(),
            "frames": frames,
            "indices": indices,
            "out": out.display().to_string(),
            "note": "长任务已启动；用 merge.status 查询进度与成片路径，merge.cancel 取消",
        })
    }

    /// `merge.status` 的 JSON：忙碌标记、进度计数、最近一次结果或失败原因。
    fn merge_status_json(&self) -> Value {
        json!({
            "busy": self.merge_busy,
            "kind": self.merge_kind.map(MergeKind::tag),
            "done": self.merge_done.load(Ordering::Relaxed),
            "total": self.merge_total.load(Ordering::Relaxed),
            "result": self.merge_result,
            "error": self.merge_error,
        })
    }

    /// 请求取消正在进行的合并；核心算法是一整块，取消只在两帧之间生效。
    pub fn stop_merge(&mut self) {
        if self.merge_busy {
            self.cancel_merge.store(true, Ordering::SeqCst);
            if let Some(kind) = self.merge_kind {
                self.status = format!("正在取消{}…（帧间生效）", kind.cn());
            }
        }
    }

    /// 面板画进度用的三元组：种类 + 已完成 + 总数。没在跑就 `None`。
    pub fn merge_progress(&self) -> Option<(MergeKind, usize, usize)> {
        if !self.merge_busy {
            return None;
        }
        Some((
            self.merge_kind.unwrap_or(MergeKind::Hdr),
            self.merge_done.load(Ordering::Relaxed),
            self.merge_total.load(Ordering::Relaxed),
        ))
    }

    /// 序列体检：`sequence.check` 起后台判定，立刻返回「已启动」。
    ///
    /// 测光随导入那趟已经算好（`self.metrics`），这里只逐帧 `stat` 一遍文件大小
    /// 再跑纯数组判定——几千帧也就几十毫秒，但仍在后台跑：界面线程不该为任何
    /// 与帧数成正比的活停下来。
    fn cmd_sequence_check(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let (n, paths) = {
            let seq = self.require_sequence()?;
            (seq.len(), seq.paths())
        };
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.qa_busy {
            return Err("体检正在跑；用 sequence.status 查询进度，sequence.cancel 取消".to_owned());
        }
        let sig = match args.get("sig") {
            Some(v) => {
                let s = v
                    .as_f64()
                    .ok_or_else(|| "sig 要是一个数，例如 3".to_owned())?;
                if !s.is_finite() || !(0.5..=20.0).contains(&s) {
                    return Err("sig 得在 0.5~20 之间：越小越灵敏，越大越宽松".to_owned());
                }
                s
            }
            None => self.qa_sig,
        };
        self.qa_sig = sig;
        let opts = qa::Options {
            sig,
            blur_sig: sig,
            ..qa::Options::default()
        };
        // 测光表可能比帧表短（还在导），缺的那几帧按「读不出来」判，正好是体检要报的。
        let frames: Vec<(PathBuf, Option<FrameMetrics>)> = paths
            .into_iter()
            .enumerate()
            .map(|(i, p)| (p, self.metrics.get(i).copied().flatten()))
            .collect();

        self.qa_busy = true;
        self.qa_error = None;
        self.cancel_qa.store(false, Ordering::SeqCst);
        self.qa_done.store(0, Ordering::Relaxed);
        self.qa_total.store(n + 1, Ordering::Relaxed);
        self.status = "正在体检…".to_owned();

        let token = self.reload_token;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let done = self.qa_done.clone();
        let cancel = self.cancel_qa.clone();
        thread::spawn(move || {
            let result = run_qa_job(&frames, &opts, &done, &cancel);
            let _ = tx.send(Msg::Checked { token, result });
            ctx2.request_repaint();
        });
        Ok(json!({
            "started": true,
            "frames": n,
            "sig": sig,
            "note": "长任务已启动；用 sequence.status 查询进度与清单，sequence.cancel 取消",
        }))
    }

    /// `sequence.status` 的 JSON：忙碌标记、进度、最近一次清单与分类计数。
    fn qa_status_json(&self) -> Value {
        json!({
            "busy": self.qa_busy,
            "done": self.qa_done.load(Ordering::Relaxed),
            "total": self.qa_total.load(Ordering::Relaxed),
            "ran": self.qa_run,
            "sig": self.qa_sig,
            "frames": self.frame_count(),
            "counts": qa::count_by_kind(&self.qa_findings),
            "findings": qa::findings_json(&self.qa_findings),
            "error": self.qa_error,
        })
    }

    /// 请求取消正在跑的体检；`stat` 是按批停的，最后一批跑完才真正退出。
    pub fn stop_qa(&mut self) {
        if self.qa_busy {
            self.cancel_qa.store(true, Ordering::SeqCst);
            self.status = "正在取消体检…（批间生效）".to_owned();
        }
    }

    /// 面板画进度用的二元组：已完成 / 总数。没在跑就 `None`。
    pub fn qa_progress(&self) -> Option<(usize, usize)> {
        if !self.qa_busy {
            return None;
        }
        Some((
            self.qa_done.load(Ordering::Relaxed),
            self.qa_total.load(Ordering::Relaxed),
        ))
    }

    /// 面板上的「开始体检」：与 `sequence.check` 同一条路径，阈值用面板当前值。
    pub fn run_qa(&mut self, ctx: &Context) {
        if self.qa_busy || self.headless {
            return;
        }
        let _ = self.cmd_sequence_check(ctx, &Value::Null);
    }

    /// 全景拼接：相邻几张有重叠的照片接成一张宽幅（特征点 + RANSAC + 羽化）。
    /// 与 HDR 同一套长任务通道：校验、抓输入、起后台线程，立刻返回。
    fn cmd_pano_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let n = seq.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(format!(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                self.merge_kind.map(MergeKind::cn).unwrap_or("合并")
            ));
        }
        if !confirm(args) {
            return Err("pano.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let indices: Vec<usize> = match args.get("frames") {
            Some(list) => list
                .as_array()
                .ok_or_else(|| "frames 要是帧号数组，例如 [0,1,2]".to_owned())?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|i| ((i as usize) < n).then_some(i as usize))
                        .ok_or_else(|| format!("帧号越界：{v}（序列共 {n} 帧）"))
                })
                .collect::<Result<_, _>>()?,
            None => {
                let around = opt_u32(args, "around")?.unwrap_or(1) as usize;
                let start = opt_u32(args, "start")?
                    .map(|v| v as usize)
                    .unwrap_or(self.selected.min(n - 1));
                if start >= n {
                    return Err(format!("start 越界：{start}（序列共 {n} 帧）"));
                }
                (start..=start.saturating_add(around).min(n - 1)).collect()
            }
        };
        let kind = MergeKind::Pano;
        if indices.len() < 2 {
            return Err("全景合并至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
        }
        if indices.len() > kind.max_frames() {
            return Err(format!(
                "全景合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑相邻几张",
                kind.max_frames(),
                indices.len()
            ));
        }
        let root = seq.root.clone();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| format!("建输出目录失败：{e}"))?;

        let job = self.build_merge_job(kind, indices, &out_dir)?;
        Ok(self.start_merge(ctx, job))
    }

    fn cmd_grade_set(&mut self, args: &Value) -> Result<Value, String> {
        // 允许 {"params": {...}}，也允许字段直接放顶层。
        let partial = match args.get("params") {
            Some(value) => value,
            None => args,
        };
        let object = partial
            .as_object()
            .ok_or_else(|| "params 必须是对象".to_owned())?;
        if object.is_empty() {
            return Err("没有要设置的调色字段（例如 {\"exposure\":0.5}）".to_owned());
        }
        let mut current =
            serde_json::to_value(&self.params).map_err(|e| format!("序列化参数失败：{e}"))?;
        let current_object = current
            .as_object_mut()
            .ok_or_else(|| "内部错误：GradeParams 不是对象".to_owned())?;
        let mut unknown = Vec::new();
        for (key, value) in object {
            if current_object.contains_key(key) {
                current_object.insert(key.clone(), value.clone());
            } else {
                unknown.push(key.clone());
            }
        }
        if !unknown.is_empty() {
            unknown.sort();
            return Err(format!("未知调色字段：{}", unknown.join("、")));
        }
        let params: GradeParams =
            serde_json::from_value(current).map_err(|e| format!("参数格式不对：{e}"))?;
        // 换了外观路径就当场验一遍：渲染热路径读不出来时只会按"没挂"处理，
        // 报错必须在这里发生，否则界面表现为"改了没反应"。
        if object.contains_key("look_cube") && !params.look_cube.is_empty() {
            cube::refresh(&params.look_cube).map_err(|e| format!("外观 .cube 读不出来：{e}"))?;
        }
        self.params = params;
        self.preset = "自定义".to_owned();
        self.auto_note = None;
        self.preview_dirty = true;
        self.status = "调色参数已更新（来自命令通道）".to_owned();
        Ok(self.grade_json())
    }

    fn cmd_grade_preset(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        if !PRESET_NAMES.contains(&name) {
            return Err(format!(
                "未知预设 {name}；可用：{}",
                PRESET_NAMES.join("、")
            ));
        }
        self.apply_preset(name);
        self.status = format!("已套用预设 {name}");
        Ok(self.grade_json())
    }

    // ------------------------------------------------------------ 蒙版命令

    fn mask_list_json(&self) -> Value {
        let list: Vec<Value> = self
            .params
            .masks
            .iter()
            .enumerate()
            .map(|(i, raw)| {
                let parsed = mask::parse_mask(raw);
                let shapes: Vec<Value> = parsed
                    .as_ref()
                    .map(|m| {
                        m.components
                            .iter()
                            .map(|c| {
                                json!({
                                    "kind": c.shape.as_str(),
                                    "blend": c.blend.as_str(),
                                    "invert": c.invert,
                                    "x1": c.x1,
                                    "y1": c.y1,
                                    "x2": c.x2,
                                    "y2": c.y2,
                                    "feather": c.feather,
                                    "midpoint": c.midpoint,
                                    "angle": c.angle,
                                    "strokes": if c.shape == mask::ShapeKind::Brush {
                                        json!(c.strokes.len())
                                    } else {
                                        Value::Null
                                    },
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({
                    "index": i + 1,
                    "id": parsed.as_ref().map(|m| m.id.clone()).unwrap_or_default(),
                    "shape": shapes
                        .first()
                        .and_then(|s| s["kind"].as_str())
                        .unwrap_or("linear"),
                    "components": shapes.len(),
                    "shapes": shapes,
                    "amount": parsed.as_ref().map(|m| m.amount).unwrap_or(0.0),
                    "invert": parsed.as_ref().map(|m| m.invert).unwrap_or(false),
                    "range": parsed
                        .as_ref()
                        .and_then(|m| m.range.as_ref())
                        .map(lum_range_json),
                    "params": parsed.as_ref().map(|m| m.params.clone()).unwrap_or_default(),
                })
            })
            .collect();
        json!({
            "count": list.len(),
            "selected": self.mask_sel.map(|i| i + 1),
            "component": self.mask_comp + 1,
            "masks": list,
        })
    }

    fn cmd_mask_add(&mut self, args: &Value) -> Result<Value, String> {
        self.require_sequence()?;
        let kind = opt_str(args, "kind")?.unwrap_or("linear");
        let kind = kind.trim().to_ascii_lowercase();
        if kind != "linear" && kind != "radial" && kind != "brush" {
            return Err(format!("未知蒙版形状 {kind}；可用 linear / radial / brush"));
        }
        let index = self.add_mask(&kind);
        self.status = format!(
            "已添加{}蒙版；拖动画布手柄调整范围",
            match kind.as_str() {
                "radial" => "径向",
                "brush" => "画笔",
                _ => "线性",
            }
        );
        let mut out = self.mask_list_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("added".to_owned(), json!(index + 1));
        }
        Ok(out)
    }

    fn cmd_mask_remove(&mut self, args: &Value) -> Result<Value, String> {
        self.require_sequence()?;
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        if index == 0 || index as usize > self.params.masks.len() {
            return Err(format!("index 超出范围：1..={}", self.params.masks.len()));
        }
        self.remove_mask(index as usize - 1);
        self.status = format!("已删除第 {index} 张蒙版");
        let mut out = self.mask_list_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("removed".to_owned(), json!(index));
        }
        Ok(out)
    }

    fn cmd_mask_update(&mut self, args: &Value) -> Result<Value, String> {
        self.require_sequence()?;
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        let slot = index as usize;
        if slot == 0 || slot > self.params.masks.len() {
            return Err(format!("index 超出范围：1..={}", self.params.masks.len()));
        }
        let component = opt_u32(args, "component")?.unwrap_or(0) as usize;
        let mut raw = self.params.masks[slot - 1].clone();
        let grouped = raw
            .as_object()
            .is_some_and(|o| o.contains_key("components"));
        let shapes = mask::parse_mask(&raw)
            .map(|m| m.components.len())
            .unwrap_or(0)
            .max(1);
        if component >= shapes {
            return Err(format!("component 超出范围：0..{}", shapes - 1));
        }

        if let Some(amount) = opt_f64(args, "amount")? {
            let object = raw
                .as_object_mut()
                .ok_or_else(|| "蒙版格式不对：不是对象".to_owned())?;
            object.insert("amount".to_owned(), json!(amount.clamp(0.0, 100.0)));
        }
        if let Some(invert) = args.get("invert") {
            let value = invert
                .as_bool()
                .ok_or_else(|| "invert 必须是布尔值".to_owned())?;
            let object = raw
                .as_object_mut()
                .ok_or_else(|| "蒙版格式不对：不是对象".to_owned())?;
            object.insert("invert".to_owned(), json!(value));
        }
        if let Some(params) = args.get("params") {
            let params = params
                .as_object()
                .ok_or_else(|| "params 必须是对象".to_owned())?;
            if !grouped {
                // 扁平老格式没有 params 字段，parse_mask 也不读它；先转成组再改。
                raw = mask::as_group(&raw);
            }
            let object = raw
                .as_object_mut()
                .ok_or_else(|| "蒙版格式不对：不是对象".to_owned())?;
            let target = object
                .entry("params".to_owned())
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| "蒙版 params 格式不对".to_owned())?;
            let mut unknown = Vec::new();
            for (key, value) in params {
                if !mask::MASK_KEYS.contains(&key.as_str()) {
                    unknown.push(key.clone());
                    continue;
                }
                if value.is_null() {
                    target.remove(key);
                } else {
                    let number = value
                        .as_f64()
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| format!("局部参数 {key} 必须是有限数值或 null"))?;
                    target.insert(key.clone(), json!(number));
                }
            }
            if !unknown.is_empty() {
                unknown.sort();
                return Err(format!("未知局部参数：{}", unknown.join("、")));
            }
        }
        if let Some(geometry) = args.get("geometry") {
            let geometry = geometry
                .as_object()
                .ok_or_else(|| "geometry 必须是对象".to_owned())?;
            apply_mask_geometry(&mut raw, component, geometry)?;
        }
        if let Some(range) = args.get("range") {
            let range = range
                .as_object()
                .ok_or_else(|| "range 必须是对象".to_owned())?;
            apply_mask_range(&mut raw, range)?;
        }

        // 先确认改完还能解析，再落回参数，避免坏数据覆盖好的蒙版。
        if mask::parse_mask(&raw).is_none() {
            return Err("改完的蒙版无法解析；请检查 geometry / invert 字段".to_owned());
        }
        self.params.masks[slot - 1] = raw;
        self.mask_sel = Some(slot - 1);
        self.mask_comp = component;
        self.masks_changed();
        self.status = format!("已更新第 {index} 张蒙版（来自命令通道）");
        let mut out = self.mask_list_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("updated".to_owned(), json!(index));
        }
        Ok(out)
    }

    /// 组内形状的增删：`action` 为 add（默认）或 remove。
    fn cmd_mask_component(&mut self, args: &Value) -> Result<Value, String> {
        self.require_sequence()?;
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        let masks = self.params.masks.len();
        if index == 0 || index as usize > masks {
            return Err(format!("index 超出范围：1..={masks}"));
        }
        let slot = index as usize - 1;
        let action = opt_str(args, "action")?
            .unwrap_or("add")
            .trim()
            .to_ascii_lowercase();
        let (detail, message) = match action.as_str() {
            "add" => {
                let kind = opt_str(args, "kind")?
                    .unwrap_or("linear")
                    .trim()
                    .to_ascii_lowercase();
                if kind != "linear" && kind != "radial" && kind != "brush" {
                    return Err(format!("未知蒙版形状 {kind}；可用 linear / radial / brush"));
                }
                let added = self
                    .add_mask_component(slot, &kind)
                    .ok_or_else(|| "这一组蒙版格式不对，加不了形状".to_owned())?;
                (
                    json!(added + 1),
                    format!(
                        "已在第 {index} 张蒙版里加了第 {} 个{}形状",
                        added + 1,
                        match kind.as_str() {
                            "radial" => "径向",
                            "brush" => "画笔",
                            _ => "线性",
                        }
                    ),
                )
            }
            "remove" => {
                let current = if self.mask_sel == Some(slot) {
                    self.mask_comp
                } else {
                    0
                };
                let comp = opt_u32(args, "component")?.unwrap_or(current as u32) as usize;
                if !self.remove_mask_component(slot, comp) {
                    return Err("这一组只剩一个形状；删整张蒙版请用 mask.remove".to_owned());
                }
                (
                    json!(comp + 1),
                    format!("已删掉第 {index} 张蒙版里的第 {} 个形状", comp + 1),
                )
            }
            other => return Err(format!("未知动作 {other}；可用 add / remove")),
        };
        self.status = message;
        let mut out = self.mask_list_json();
        if let Some(object) = out.as_object_mut() {
            object.insert(action.to_owned(), detail);
        }
        Ok(out)
    }

    fn cmd_align_run(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.headless {
            return Err("当前是无界面模式，不能启动对齐".to_owned());
        }
        if self.align_busy {
            return Err("对齐已在进行中；用 align.status 查询进度".to_owned());
        }
        let reference = opt_u32(args, "reference")?;
        if let Some(reference) = reference
            && (reference == 0 || reference as usize > n)
        {
            return Err(format!("reference 超出范围：1..={n}"));
        }
        let estimator = match opt_str(args, "estimator")? {
            Some(name) => Some(match name.trim().to_ascii_lowercase().as_str() {
                "template" | "模板" => Estimator::Template,
                "phase" | "相位" => Estimator::Phase,
                other => return Err(format!("未知估计器 {other}；可用 template / phase")),
            }),
            None => None,
        };
        // 参数全部校验通过后再改状态，避免失败的命令留下半套设置。
        if let Some(reference) = reference {
            self.align_ref = reference as usize - 1;
        }
        if let Some(estimator) = estimator {
            self.align_estimator = estimator;
        }
        self.run_align(ctx);
        Ok(json!({
            "started": true,
            "frames": n,
            "reference": self.align_ref + 1,
            "estimator": estimator_name(self.align_estimator),
            "note": "长任务已启动；用 align.status 查询进度",
        }))
    }

    fn align_status_json(&self) -> Value {
        let report = self.align_report.as_ref().map(|report| {
            json!({
                "frames": report.offsets.len(),
                "crop": rect_json(report.crop),
                "issues": report.issues.iter().take(8).map(|issue| json!({
                    "frame": issue.frame + 1,
                    "kind": issue.kind,
                    "detail": issue.detail,
                })).collect::<Vec<_>>(),
                "elapsed_secs": round6(report.elapsed_secs),
            })
        });
        let progress = self
            .align_progress()
            .map(|(done, total)| json!({"done": done, "total": total}));
        json!({
            "busy": self.align_busy,
            "progress": progress,
            "message": self.align_msg,
            "report": report,
        })
    }

    /// 面板读数：按速度曲线估输出帧数与时长。只积分曲线，不建整段取样计划——
    /// 几十万帧的计划每帧 UI 算一遍会把界面拖住。
    pub fn retime_estimate(&self) -> Result<(usize, f64), String> {
        let n = self.frame_count();
        let fps = self.output_fps.max(1.0) as f64;
        if n == 0 {
            return Ok((0, 0.0));
        }
        if self.retime.is_identity() {
            return Ok((n, n as f64 / fps));
        }
        let retime = self.retime.normalized()?;
        let frames = retime::output_frames(&retime.keys, n)?;
        Ok((frames, frames as f64 / fps))
    }

    /// 变速面板与命令的当前值，外加按序列帧数换算的帧数、时长。
    ///
    /// `frames`/`seconds` 是曲线积分出来的真实输出量：常速或没开变速就等于源帧数，
    /// 面板上那行「N 源帧 → M 帧 · X 秒」直接念这两个数，不用自己估。
    fn retime_status_json(&self) -> Value {
        let n = self.frame_count();
        let fps = self.output_fps.max(1.0) as f64;
        let (frames, seconds, error) = match self.retime_estimate() {
            Ok((frames, seconds)) => (Some(frames), Some(round6(seconds)), None),
            Err(e) => (None, None, Some(e)),
        };
        json!({
            "enabled": self.retime.enabled,
            "shutter": round6(self.retime.shutter),
            "keys": self
                .retime
                .keys
                .iter()
                .map(|k| json!({"at": round6(k.at), "speed": round6(k.speed)}))
                .collect::<Vec<_>>(),
            "identity": self.retime.is_identity(),
            "source_frames": n,
            "fps": round6(fps),
            "frames": frames,
            "seconds": seconds,
            "max_output_frames": retime::MAX_OUTPUT_FRAMES,
            "error": error,
        })
    }

    /// 写变速设定。`speed` / `keys` / `ease` 三种写法互斥：都收会让「传了两个」
    /// 的调用悄悄只用其中一个，不如当场拒绝。
    fn cmd_retime_set(&mut self, args: &Value) -> Result<Value, String> {
        let mut next = self.retime.clone();
        if let Some(v) = args.get("enabled") {
            next.enabled = v.as_bool().ok_or("enabled 必须是布尔")?;
        }
        if let Some(v) = args.get("reset") {
            if !v.as_bool().ok_or("reset 必须是布尔")? {
                return Err(
                    "reset 只接受 true：要改回去就逐项写 enabled / speed / shutter".to_owned(),
                );
            }
            next = Retime::default();
        }
        let curve_given = ["speed", "keys", "ease"]
            .iter()
            .filter(|key| args.get(*key).is_some_and(|v| v != &Value::Null))
            .copied()
            .collect::<Vec<_>>();
        if curve_given.len() > 1 {
            return Err(format!(
                "{} 同时给了：变速曲线只能选一种写法（speed 全程一个倍速 / keys 多点曲线 / ease 缓入缓出）",
                curve_given.join("、")
            ));
        }
        if !curve_given.is_empty() {
            // 给了曲线就是当真要变速，省掉「先 enabled 再 speed」两步。
            next.enabled = true;
        }
        if let Some(speed) = opt_f64(args, "speed")? {
            next = Retime::uniform(speed, next.shutter);
        }
        if let Some(raw) = args.get("keys") {
            next.keys = parse_speed_keys(raw)?;
        }
        if let Some(ease) = args.get("ease") {
            if !ease.is_object() {
                return Err("ease 要写成对象：{ peak, edge?, in_at?, out_at? }".to_owned());
            }
            let edge = opt_f64(ease, "edge")?.unwrap_or(1.0);
            let peak = opt_f64(ease, "peak")?
                .ok_or_else(|| "ease 里必须给 peak（中段倍速）".to_owned())?;
            let in_at = opt_f64(ease, "in_at")?.unwrap_or(0.25);
            let out_at = opt_f64(ease, "out_at")?.unwrap_or(0.75);
            next = Retime::ease(edge, peak, in_at, out_at, next.shutter);
        }
        if let Some(shutter) = opt_f64(args, "shutter")? {
            if !(0.0..=1.0).contains(&shutter) {
                return Err(format!(
                    "shutter 是曝光区间占本帧源跨度的比例，要在 0..1（0 锐利、1 全平均），实得 {shutter}"
                ));
            }
            next.shutter = shutter;
        }
        // 关着的时候不做规整：`normalized()` 会把整块摊平，面板关一下再开曲线就没了。
        // 数字写错了也留到开变速那天再报，反正那时导出前一定会校验。
        if next.enabled {
            next = next.normalized()?;
        }
        self.retime = next;
        Ok(self.retime_status_json())
    }

    /// 运镜作用的那一幅像素尺寸：拉直 + 导出取景之后的宽高，与导出里 `bw × bh` 同口径。
    /// 取景框是相对这一幅归一化的，所以画布换算与推近读数都认它。
    pub fn motion_frame_size(&self) -> (u32, u32) {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        match self.effective_crop() {
            Some(c) => (c.w.max(1), c.h.max(1)),
            None => (w, h),
        }
    }

    /// 运镜的输出画幅：成片面板给了精确宽高就认它，否则就是那一幅原样铺出去。
    pub fn motion_output_size(&self) -> (u32, u32) {
        if self.output_w >= 2 && self.output_h >= 2 {
            (self.output_w, self.output_h)
        } else {
            self.motion_frame_size()
        }
    }

    /// 按输出帧号取当前运镜的取景框；没开就返回 `None`，预览照原来的路走。
    pub fn motion_view_at(&self, i: usize, total: usize) -> Option<Viewfinder> {
        self.motion
            .is_active()
            .then(|| self.motion.view_of(i, total.max(1)))
    }

    fn motion_status_json(&self) -> Value {
        let (fw, fh) = self.motion_frame_size();
        let (ow, oh) = self.motion_output_size();
        let box_json = |v: &Viewfinder| {
            let s = v.normalized();
            [round6(s.x), round6(s.y), round6(s.w), round6(s.h)].to_vec()
        };
        json!({
            "enabled": self.motion.enabled,
            "active": self.motion.is_active(),
            "ease": self.motion.ease.key(),
            "ease_cn": self.motion.ease.cn(),
            "from": box_json(&self.motion.from),
            "to": box_json(&self.motion.to),
            "frame": [fw, fh],
            "output": [ow, oh],
            // 推近倍数：>1 就是拿少数像素撑大画面，面板按它给一句「再推就糊了」。
            "zoom_from": round6(self.motion.from.zoom(ow, oh, fw, fh)),
            "zoom_to": round6(self.motion.to.zoom(ow, oh, fw, fh)),
            "source_frames": self.frame_count(),
        })
    }

    /// 写运镜设定：解析交给 `Motion::patched`（核心里那份，MCP 与控制通道同一条路），
    /// 这里只负责落状态与回报。
    fn cmd_motion_set(&mut self, args: &Value) -> Result<Value, String> {
        self.motion = self.motion.patched(args)?;
        // 取景框一动，预览就得重出：画布上那块像素跟着框走。
        self.preview_dirty = true;
        Ok(self.motion_status_json())
    }

    fn cmd_export(&mut self, ctx: &Context, id: &str, args: &Value) -> Result<Value, String> {
        let video = id == "export.video";
        let seq = self.require_sequence()?;
        let n = seq.len();
        let root = seq.root.clone();
        let seq_name = seq.display_name();
        let sources = seq.paths();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.headless {
            return Err("当前是无界面模式，不能从命令通道启动导出".to_owned());
        }
        if self.export_busy {
            return Err("导出已在进行中；用 export.status 查询，或 export.cancel 取消".to_owned());
        }
        // 批量写操作的硬要求：覆盖已有输出、整段导出都必须显式确认。
        if !confirm(args) {
            return Err(format!("{id} 是批量写操作，需参数 confirm: true"));
        }
        // 先把所有参数校验完，再一次性落到状态里，失败时不留半套设置。
        let size = match (opt_u32(args, "width")?, opt_u32(args, "height")?) {
            (Some(width), Some(height)) => {
                if width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1 {
                    return Err(format!(
                        "width/height 必须是偶数且不小于 2：{width}×{height}"
                    ));
                }
                Some((width, height))
            }
            (None, None) => None,
            _ => {
                return Err("width 与 height 需同时给出（或都不给，保持当前规格）".to_owned());
            }
        };
        let out_dir = match opt_str(args, "out_dir")? {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                if dir.as_os_str().is_empty() {
                    return Err("out_dir 不能为空；省略则用当前输出目录".to_owned());
                }
                dir
            }
            None if self.render_out_dir.as_os_str().is_empty() => root.join("成片"),
            None => self.render_out_dir.clone(),
        };
        ensure_out_dir_safe(&root, &out_dir)?;
        let fps = opt_f64(args, "fps")?;
        if let Some(fps) = fps
            && !(fps > 0.0 && fps <= 240.0)
        {
            return Err("fps 必须在 (0, 240]".to_owned());
        }
        let crf = opt_u32(args, "crf")?;
        if let Some(crf) = crf
            && crf > 51
        {
            return Err("crf 必须在 0..51".to_owned());
        }
        let codec = match opt_str(args, "codec")? {
            Some(codec) => Some(match codec.trim().to_ascii_lowercase().as_str() {
                "h264" | "264" | "avc" => "H.264".to_owned(),
                "h265" | "265" | "hevc" => "HEVC".to_owned(),
                other => return Err(format!("未知编码 {other}；可用 h264 / h265")),
            }),
            None => None,
        };
        // 色域静帧和成片都用同一条参数：省略就保持面板当前选择。
        let color_space = opt_str(args, "color_space")?
            .map(|raw| {
                ColorSpace::parse(raw)
                    .ok_or_else(|| format!("未知色域 {raw}；可用 srgb / p3 / rec2020"))
            })
            .transpose()?;
        // 容器与质量只在静帧那一路有意义：视频模式撞上有损档现在就拦，别等 core。
        let format = opt_str(args, "format")?
            .map(|raw| {
                StillFormat::parse(raw).ok_or_else(|| {
                    format!("未知容器 {raw}；可用 png8 / tiff16 / jpeg / webp / avif")
                })
            })
            .transpose()?;
        let quality = opt_u32(args, "quality")?;
        if let Some(q) = quality
            && !(1..=100).contains(&q)
        {
            return Err("quality 要在 1..100".to_owned());
        }
        // copy：这一次导出改用某套副本的调色；省略就是面板上当前这套。
        let copy = match opt_str(args, "copy")? {
            Some(raw) => {
                let name = raw.trim().to_owned();
                if self.copy_index(&name).is_none() {
                    let have: Vec<&str> = self.copies.iter().map(|c| c.name.as_str()).collect();
                    return Err(format!(
                        "没有叫 {name} 的副本；现有：{}",
                        if have.is_empty() {
                            "（还没有副本）".to_owned()
                        } else {
                            have.join("、")
                        }
                    ));
                }
                Some(name)
            }
            None => None,
        };
        let video_out = if video {
            if args.get("out_dir").is_some() || self.render_video_out.as_os_str().is_empty() {
                out_dir.join(format!("{seq_name}_成片.mp4"))
            } else {
                self.render_video_out.clone()
            }
        } else {
            self.render_video_out.clone()
        };
        // 工程文件里的 video_out 可能被手改过；绝不允许它指向源帧。
        if video && sources.iter().any(|source| path_eq(source, &video_out)) {
            return Err(format!("拒绝覆盖素材：{}", video_out.display()));
        }
        if video {
            // 缺 ffmpeg 就别先渲染一整段帧；提前失败，消息更直接。
            if export::find_ffmpeg().is_none() {
                return Err("找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned());
            }
        }

        // 校验通过，开始落设置。
        if let Some((width, height)) = size {
            self.output_w = width;
            self.output_h = height;
        }
        self.render_out_dir = out_dir;
        if let Some(space) = color_space {
            self.render_color_space = space;
        }
        // 模式先落，容器才知道自己是不是要当 ffmpeg 的中间帧。
        self.render_mode = if video {
            OutputMode::Video
        } else {
            OutputMode::Stills
        };
        if let Some(format) = format {
            self.set_render_format(format)?;
        }
        if let Some(q) = quality {
            self.render_quality = q as u8;
        }
        if video {
            self.render_video_out = video_out;
            if let Some(fps) = fps {
                self.output_fps = fps as f32;
            }
            if let Some(crf) = crf {
                self.render_crf = crf as u8;
            }
            if let Some(codec) = codec {
                self.codec = codec;
            }
        }

        self.export_copy = copy.clone();
        self.run_export(ctx);
        if !self.export_busy {
            return Err("导出没有启动：检查是否已在导出或无界面模式".to_owned());
        }
        Ok(json!({
            "started": true,
            "mode": mode_name(self.render_mode),
            "format": self.render_format.key(),
            "quality": self.render_quality,
            "color_space": self.render_color_space.key(),
            "copy": copy,
            "frames": n,
            "out_dir": self.render_out_dir.display().to_string(),
            "video_out": if video { Some(self.render_video_out.display().to_string()) } else { None },
            "note": "长任务已启动；用 export.status 查询进度，export.cancel 取消",
        }))
    }

    fn export_status_json(&self) -> Value {
        let progress = self.export_progress().map(|(done, total, stage)| {
            json!({
                "done": done,
                "total": total,
                "stage": match stage {
                    Stage::Frames => "frames",
                    Stage::Encode => "encode",
                },
            })
        });
        json!({
            "busy": self.export_busy,
            "stage": self.export_stage.map(|stage| match stage {
                Stage::Frames => "frames",
                Stage::Encode => "encode",
            }),
            "progress": progress,
            "mode": mode_name(self.render_mode),
            "out_dir": self.render_out_dir.display().to_string(),
            "video_out": self.render_video_out.display().to_string(),
            "last_saved": self.last_saved,
            "last_error": self.error,
            "status": self.status,
        })
    }

    fn cmd_project_save(&mut self) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let path = project::project_path(&seq.root);
        if self.headless {
            return Err("当前是无界面模式，不写工程文件".to_owned());
        }
        // 直接写一份快照，失败就把原因带回去；不走 quiet 自动保存，
        // 免得被更早的无关错误干扰判断。
        let snapshot = self.project_snapshot();
        project::save(&path, &snapshot).map_err(|e| format!("保存工程失败：{e}"))?;
        self.last_auto_save = Instant::now();
        self.status = format!("工程已保存 {}", path.display());
        Ok(json!({"path": path.display().to_string(), "saved": true}))
    }

    fn cmd_project_get(&self) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let path = project::project_path(&seq.root);
        let snapshot = self.project_snapshot();
        Ok(json!({
            "path": path.display().to_string(),
            "exists": path.is_file(),
            "project": serde_json::to_value(&snapshot).unwrap_or(Value::Null),
        }))
    }

    /// 当前序列所属目录的说明与版权：不带参数是查询，带了就改那几项。
    /// 改完立刻写一次工程文件，MCP 那头不用等 5 秒的自动保存。
    fn cmd_project_metadata(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.require_sequence()?.root.clone();
        let changed = apply_metadata_args(&mut self.meta, args)?;
        if changed {
            self.meta = self.meta.cleaned();
            self.write_project(true);
            self.status = if self.meta.is_empty() {
                "已清空说明与版权信息".to_owned()
            } else {
                "已更新说明与版权信息".to_owned()
            };
        }
        let mut out = metadata_json(&self.meta);
        out["path"] = json!(project::project_path(&root).display().to_string());
        out["saved"] = json!(changed);
        Ok(out)
    }

    fn cmd_journal_list(&self, args: &Value) -> Result<Value, String> {
        let root = self.require_sequence()?.root.clone();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(50)
            .clamp(1, 500);
        let entries = journal::tail(&root, limit);
        Ok(json!({
            "path": journal::path(&root).display().to_string(),
            "count": entries.len(),
            "entries": entries,
        }))
    }

    // ------------------------------------------------------------ 配乐

    /// 换配乐并立刻读一次编码与时长；没有 ffmpeg 只记原因，不影响导出前的报错。
    pub fn set_render_audio(&mut self, path: Option<PathBuf>) {
        self.render_audio = path;
        self.audio_info = None;
        self.audio_probe_err = None;
        let Some(path) = &self.render_audio else {
            return;
        };
        if self.headless {
            return;
        }
        let Some(ffmpeg) = export::find_ffmpeg() else {
            self.audio_probe_err = Some("找不到 ffmpeg，无法读取配乐时长".to_owned());
            return;
        };
        match export::probe_audio(&ffmpeg, path) {
            Ok(info) => self.audio_info = Some(info),
            Err(e) => self.audio_probe_err = Some(e.to_string()),
        }
    }

    /// 画面 / 配乐 / 成片三段时长；没有探针结果时返回 `None`。
    pub fn audio_timeline(&self) -> Option<(f64, f64, f64)> {
        let info = self.audio_info.as_ref()?;
        let vdur = self.frame_count() as f64 / self.output_fps.max(1.0) as f64;
        Some((
            vdur,
            info.duration,
            self.render_audio_opts.total_dur(vdur, info.duration),
        ))
    }

    fn audio_status_json(&self) -> Value {
        let vdur = self.frame_count() as f64 / self.output_fps.max(1.0) as f64;
        let adur = self.audio_info.as_ref().map(|i| i.duration).unwrap_or(0.0);
        let filter = self
            .audio_info
            .as_ref()
            .and_then(|_| self.render_audio_opts.filter(vdur, adur))
            .map(|f| f.complex);
        let out_ext = self
            .render_video_out
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp4");
        let copy = self
            .audio_info
            .as_ref()
            .is_some_and(|i| self.render_audio_opts.copy_ok(out_ext, &i.codec));
        json!({
            "path": self.render_audio.as_ref().map(|p| p.display().to_string()),
            "probe": self.audio_info,
            "probe_error": self.audio_probe_err,
            "options": self.render_audio_opts,
            "video_seconds": round6(vdur),
            "total_seconds": if self.audio_info.is_some() {
                Some(round6(self.render_audio_opts.total_dur(vdur, adur)))
            } else {
                None
            },
            "filter": filter,
            "copy": copy,
        })
    }

    /// 一次改多项配乐设置；`path` 传空串或 null 表示清空。
    pub fn configure_audio(&mut self, args: &Value) -> Result<Value, String> {
        let mut opts = self.render_audio_opts.clone();
        if let Some(v) = args.get("delay") {
            let d = v.as_f64().ok_or("delay 必须是数字（秒）")?;
            if !(d.is_finite() && (0.0..=3600.0).contains(&d)) {
                return Err("delay 必须在 0..3600 秒".to_owned());
            }
            opts.delay = d;
        }
        if let Some(v) = args.get("gain") {
            let g = v.as_f64().ok_or("gain 必须是数字（百分比）")?;
            if !(g.is_finite() && (0.0..=400.0).contains(&g)) {
                return Err("gain 必须在 0..400（100 为原始音量）".to_owned());
            }
            opts.gain = g;
        }
        for (key, slot) in [
            ("fade_in", &mut opts.fade_in),
            ("fade_out", &mut opts.fade_out),
        ] {
            if let Some(v) = args.get(key) {
                let f = v.as_f64().ok_or(format!("{key} 必须是数字（秒）"))?;
                if !(f.is_finite() && (0.0..=3600.0).contains(&f)) {
                    return Err(format!("{key} 必须在 0..3600 秒"));
                }
                *slot = f;
            }
        }
        if let Some(v) = args.get("loop") {
            opts.looped = v.as_bool().ok_or("loop 必须是布尔值")?;
        }
        if let Some(v) = args.get("trim") {
            opts.trim = v.as_bool().ok_or("trim 必须是布尔值")?;
        }
        if let Some(v) = args.get("acodec") {
            let text = v.as_str().ok_or("acodec 必须是字符串")?;
            opts.acodec = AudioCodec::parse(text).ok_or_else(|| {
                format!("未知 acodec {text}；可用 auto / copy / aac128 / aac192 / aac256 / aac320")
            })?;
        }
        let mut audio = self.render_audio.clone();
        let mut changed_file = false;
        if let Some(v) = args.get("path") {
            changed_file = true;
            match v {
                Value::Null => audio = None,
                Value::String(s) if s.trim().is_empty() => audio = None,
                Value::String(s) => {
                    let path = PathBuf::from(s);
                    if !path.is_file() {
                        return Err(format!("配乐文件不存在：{}", path.display()));
                    }
                    if !aevum_core::audio::is_audio_path(&path) {
                        return Err(format!(
                            "不支持这个扩展名：{}；可用 mp3 / wav / aac / m4a / flac / ogg / opus / wma / aif(m4b)",
                            path.display()
                        ));
                    }
                    audio = Some(path);
                }
                _ => return Err("path 必须是字符串或 null".to_owned()),
            }
        }
        self.render_audio_opts = opts;
        if changed_file || (self.render_audio.is_some() && self.audio_info.is_none()) {
            self.set_render_audio(audio);
        } else {
            self.render_audio = audio;
        }
        self.status = "配乐设置已更新".to_owned();
        Ok(self.audio_status_json())
    }

    // ------------------------------------------------------------ XMP 预设

    /// 导入 XMP 预设：整体替换全局调色参数。
    ///
    /// 与 Python `read_xmp` + `merge` 同口径：预设没写的键回到默认值，
    /// 局部蒙版也随默认值清空（Lightroom 预设不含本工程的蒙版模型）。
    pub fn import_xmp_preset(&mut self, path: &Path) -> Result<Value, String> {
        let params = xmp::read(path).map_err(|e| format!("读不进这个预设：{e}"))?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "XMP 预设".to_owned());
        self.params = params;
        self.preset = name.clone();
        self.reset_mask_state();
        self.auto_note = None;
        self.preview_dirty = true;
        self.status = format!("已导入 XMP 预设 {name}");
        let mut out = self.grade_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("path".to_owned(), json!(path.display().to_string()));
            object.insert("imported".to_owned(), json!(name));
        }
        Ok(out)
    }

    /// 把当前参数写成 XMP 预设；覆盖已有文件必须显式确认。
    pub fn export_xmp_preset(&mut self, path: &Path, confirm: bool) -> Result<Value, String> {
        let is_xmp = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xmp"));
        if !is_xmp {
            return Err("path 必须以 .xmp 结尾".to_owned());
        }
        if path.exists() && !confirm {
            return Err(format!("目标已存在，需 confirm: true：{}", path.display()));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        xmp::write(path, &self.params).map_err(|e| format!("写 XMP 失败：{e}"))?;
        self.status = format!("已导出 XMP 预设 {}", path.display());
        Ok(json!({
            "path": path.display().to_string(),
            "preset": self.preset,
        }))
    }

    fn cmd_xmp_import(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        if !path.is_file() {
            return Err(format!("文件不存在：{}", path.display()));
        }
        self.import_xmp_preset(&path)
    }

    fn cmd_xmp_export(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        self.export_xmp_preset(&path, confirm(args))
    }

    // ------------------------------------------------------------ .cube 外观命令

    /// 读入一张 .cube 并挂成"外观"（`apply: false` 时只体检、不改参数）。
    fn cmd_cube_import(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        if !path.is_file() {
            return Err(format!("文件不存在：{}", path.display()));
        }
        let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(true);
        let key = path.display().to_string();
        let lut = cube::refresh(&key).map_err(|e| format!("读 .cube 失败：{e}"))?;
        let info = cube::describe(&lut);
        let mut result = json!({
            "path": key,
            "title": info.title,
            "size": info.size,
            "nodes": info.nodes,
            "domain_min": info.domain_min,
            "domain_max": info.domain_max,
            "neutral": info.neutral,
            "applied": false,
        });
        if apply {
            self.params.look_cube = key.clone();
            self.preview_dirty = true;
            self.preset = "自定义".to_owned();
            self.status = format!("已挂外观 {}", key);
            result["applied"] = json!(true);
        }
        Ok(result)
    }

    /// 把当前调色（含已挂的外观）烘成一张 .cube 交出去。
    fn cmd_cube_export(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        let is_cube = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cube"));
        if !is_cube {
            return Err("path 必须以 .cube 结尾".to_owned());
        }
        let size = opt_u32(args, "size")?.unwrap_or(cube::DEFAULT_SIZE as u32);
        if !(cube::MIN_SIZE as u32..=cube::MAX_SIZE as u32).contains(&size) {
            return Err(format!(
                "size 必须在 {}..={}",
                cube::MIN_SIZE,
                cube::MAX_SIZE
            ));
        }
        if path.exists() && !confirm(args) {
            return Err(format!("目标已存在，需 confirm: true：{}", path.display()));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        let params = self.params.clone();
        let size = size as usize;
        cube::export(&path, &params, size).map_err(|e| format!("写 .cube 失败：{e}"))?;
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.status = format!("已导出外观 {}", path.display());
        Ok(json!({
            "path": path.display().to_string(),
            "size": size,
            "nodes": size * size * size,
            "bytes": bytes,
            "look": if params.look_cube.is_empty() { Value::Null } else { json!(params.look_cube) },
        }))
    }

    fn cmd_screenshot(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let raw = arg_str(args, "path")?;
        let path = PathBuf::from(raw);
        let is_png = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if !is_png {
            return Err("path 必须以 .png 结尾".to_owned());
        }
        if let Ok(seq) = self.require_sequence()
            && seq.frames.iter().any(|frame| path_eq(&frame.path, &path))
        {
            return Err(format!("拒绝覆盖素材：{}", path.display()));
        }
        if path.exists() && !confirm(args) {
            return Err(format!("目标已存在，需 confirm: true：{}", path.display()));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败：{e}"))?;
        }
        let out = path.clone();
        ctx.request_screenshot(move |image| {
            if let Err(e) = control::save_png(&out, &image) {
                eprintln!("截图保存失败：{e}");
            }
        });
        Ok(json!({
            "started": true,
            "path": path.display().to_string(),
            "note": "截图在下一帧写盘；稍后确认文件即可",
        }))
    }

    // ------------------------------------------------------------ AI 面板

    /// 解析并执行一行「命令 ID + JSON 参数」，结果写入最近记录。
    pub fn run_ai_line(&mut self, ctx: &Context, line: &str) -> Result<Value, String> {
        let line = line.trim();
        if line.is_empty() {
            return Err("先输入一条命令，例如 sequence.info".to_owned());
        }
        let split = line.find(char::is_whitespace).unwrap_or(line.len());
        let id = &line[..split];
        let rest = line[split..].trim();
        let args = if rest.is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(rest).map_err(|e| format!("参数不是合法 JSON：{e}"))?
        };
        if !args.is_object() {
            return Err("参数必须是 JSON 对象，例如 {\"exposure\":0.5}".to_owned());
        }
        let result = self.run_ui_command(ctx, id, &args);
        let (ok, detail) = match &result {
            Ok(value) => (
                true,
                serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned()),
            ),
            Err(message) => (false, message.clone()),
        };
        self.ai_log.push(AiLogEntry {
            input: line.to_owned(),
            ok,
            detail: truncate(&detail, 600),
        });
        let keep = 8;
        if self.ai_log.len() > keep {
            let drop = self.ai_log.len() - keep;
            self.ai_log.drain(0..drop);
        }
        result
    }

    pub fn recompute_deflicker(&mut self) {
        // 去闪输入（测光/窗口/强度/通道）或结果一变就换版：曲线缓存键之一。
        self.corr_version += 1;
        if self.metrics.is_empty() {
            self.analysis = None;
            self.corr.clear();
            self.corr_rgb.clear();
            return;
        }
        // 线性光测光是 Python 金标的口径；拿不到（演示序列、自检）才退回 gamma 均值。
        let measured = self.def_means.iter().any(|m| m.is_some());
        let a = if self.def_channels && measured {
            let rgb: Vec<Option<[f64; 3]>> =
                self.def_means.iter().map(|m| m.map(|m| m.rgb)).collect();
            deflicker::analyze_rgb(&rgb, self.def_window, self.def_strength, self.def_multi)
        } else if measured {
            let luma: Vec<Option<f64>> = self.def_means.iter().map(|m| m.map(|m| m.luma)).collect();
            deflicker::analyze_luma(&luma, self.def_window, self.def_strength, self.def_multi)
        } else {
            let luma: Vec<Option<f64>> = self
                .metrics
                .iter()
                .map(|m| m.map(|m| m.mean * 255.0))
                .collect();
            deflicker::analyze_luma(&luma, self.def_window, self.def_strength, self.def_multi)
        };
        self.corr = a
            .exposure_per_frame()
            .into_iter()
            .map(|v| v as f32)
            .collect();
        self.corr_rgb = a
            .chan_residual_per_frame()
            .into_iter()
            .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
            .collect();
        self.analysis = Some(a);
        self.preview_dirty = true;
    }

    pub fn apply_preset(&mut self, name: &str) {
        self.params = grade::preset(name);
        self.preset = name.to_owned();
        self.auto_note = None;
        self.reset_mask_state();
        self.preview_dirty = true;
        history::note("preset", format!("预设 {name}"));
    }

    pub fn reset_params(&mut self) {
        self.params = GradeParams::default();
        self.preset = "原始".to_owned();
        self.auto_note = None;
        self.reset_mask_state();
        self.preview_dirty = true;
        history::note("reset", "复位全部参数");
    }

    /// 选项栏上下文徽标（界面稿 .ctx .n「N 项调整」）：有几组参数偏离默认值。
    /// 按「组」计数而不是按参数 —— 徽标只回答"动没动过、动了多大片"，
    /// 逐参数细数是 dock 页签自己的职责。
    pub fn changed_groups(&self) -> usize {
        const KEYS: [&str; 12] = [
            "basic",
            "color",
            "effects",
            "look",
            "levels",
            "parametric",
            "curve",
            "hsl",
            "mono",
            "grade",
            "detail",
            "grain",
        ];
        KEYS.iter().filter(|k| self.group_changed(k)).count()
    }

    /// 分组头右缘的 accent 圆点开关：这一组里有参数偏离默认值才亮。
    /// 字段清单与 `shell::reset_group` 的复位口径一一对应 —— 那边复位哪些字段，
    /// 这边就比对哪些字段，两边必须同进退。
    pub fn group_changed(&self, key: &str) -> bool {
        let d = GradeParams::default();
        let p = &self.params;
        // 推子参数组：按规格表比对（唯一事实源，见 controls 模块头注释）。
        // 表外的组内字段——参数曲线的分界、黑白色带、分级的色轮——单独比。
        if crate::controls::specs_of_group(key).next().is_some() {
            let hit = crate::controls::specs_of_group(key)
                .any(|s| crate::controls::param_get(p, s.field) != s.default);
            let extra = match key {
                "parametric" => {
                    p.par_split1 != d.par_split1
                        || p.par_split2 != d.par_split2
                        || p.par_split3 != d.par_split3
                }
                "mono" => p.mono_h != d.mono_h,
                // 曲线组在规格表里有 HDR 尾部一条，圆点还得看四条点曲线动没动。
                "curve" => {
                    p.curve_rgb != d.curve_rgb
                        || p.curve_r != d.curve_r
                        || p.curve_g != d.curve_g
                        || p.curve_b != d.curve_b
                }
                "grade" => {
                    p.grade_sh_h != d.grade_sh_h
                        || p.grade_sh_s != d.grade_sh_s
                        || p.grade_sh_l != d.grade_sh_l
                        || p.grade_mid_h != d.grade_mid_h
                        || p.grade_mid_s != d.grade_mid_s
                        || p.grade_mid_l != d.grade_mid_l
                        || p.grade_hi_h != d.grade_hi_h
                        || p.grade_hi_s != d.grade_hi_s
                        || p.grade_hi_l != d.grade_hi_l
                }
                _ => false,
            };
            return hit || extra;
        }
        match key {
            "look" => !p.look_cube.is_empty(),
            "levels" => {
                p.lv_black != d.lv_black
                    || p.lv_white != d.lv_white
                    || p.lv_gamma != d.lv_gamma
                    || p.lv_out_black != d.lv_out_black
                    || p.lv_out_white != d.lv_out_white
            }
            "curve" => {
                p.curve_rgb != d.curve_rgb
                    || p.curve_r != d.curve_r
                    || p.curve_g != d.curve_g
                    || p.curve_b != d.curve_b
            }
            "hsl" => p.hsl_h != d.hsl_h || p.hsl_s != d.hsl_s || p.hsl_l != d.hsl_l,
            _ => false,
        }
    }

    // ------------------------------------------------------------ 局部蒙版

    /// 新建一张单组件蒙版并选中；返回 0 基序号。
    pub fn add_mask(&mut self, kind: &str) -> usize {
        let shape = match kind {
            "radial" => mask::ShapeKind::Radial,
            "brush" => mask::ShapeKind::Brush,
            _ => mask::ShapeKind::Linear,
        };
        self.params.masks.push(mask::new_mask(shape));
        let index = self.params.masks.len() - 1;
        self.select_mask(index);
        self.masks_changed();
        index
    }

    pub fn remove_mask(&mut self, index: usize) -> bool {
        if index >= self.params.masks.len() {
            return false;
        }
        self.params.masks.remove(index);
        self.mask_sel = match self.mask_sel {
            Some(sel) if sel == index => {
                if self.params.masks.is_empty() {
                    None
                } else {
                    Some(sel.min(self.params.masks.len() - 1))
                }
            }
            Some(sel) if sel > index => Some(sel - 1),
            other => other,
        };
        if self.mask_drag.is_some_and(|(i, _)| i == index) {
            self.mask_drag = None;
        }
        if self.mask_paint.is_some_and(|(i, _)| i == index) {
            self.mask_paint = None;
        }
        self.mask_comp = 0;
        self.masks_changed();
        true
    }

    /// 选中某张蒙版，并回到组内第一个形状。
    pub fn select_mask(&mut self, index: usize) {
        self.mask_sel = Some(index);
        self.mask_comp = 0;
    }

    /// 组内加一个形状；扁平老格式先升级成单组件组。返回新形状的 0 基序号。
    pub fn add_mask_component(&mut self, index: usize, kind: &str) -> Option<usize> {
        if index >= self.params.masks.len() {
            return None;
        }
        let shape = match kind {
            "radial" => mask::ShapeKind::Radial,
            "brush" => mask::ShapeKind::Brush,
            _ => mask::ShapeKind::Linear,
        };
        let added = {
            let raw = &mut self.params.masks[index];
            if !raw
                .as_object()
                .is_some_and(|o| o.contains_key("components"))
            {
                *raw = mask::as_group(raw);
            }
            {
                let list = raw.get_mut("components").and_then(Value::as_array_mut)?;
                list.push(mask::new_component(shape));
                list.len() - 1
            }
        };
        if self.mask_sel == Some(index) {
            self.mask_comp = added;
        }
        if self.mask_drag.is_some_and(|(i, _)| i == index) {
            self.mask_drag = None;
        }
        self.masks_changed();
        Some(added)
    }

    /// 删掉组内一个形状；只剩一个时不删，蒙版不能空组。
    pub fn remove_mask_component(&mut self, index: usize, comp: usize) -> bool {
        if index >= self.params.masks.len() {
            return false;
        }
        let left = {
            let Some(list) = self.params.masks[index]
                .get_mut("components")
                .and_then(Value::as_array_mut)
            else {
                return false;
            };
            if list.len() < 2 || comp >= list.len() {
                return false;
            }
            list.remove(comp);
            list.len()
        };
        if self.mask_sel == Some(index) && self.mask_comp >= left {
            self.mask_comp = left - 1;
        }
        if self.mask_drag.is_some_and(|(i, _)| i == index) {
            self.mask_drag = None;
        }
        self.masks_changed();
        true
    }

    /// 蒙版改动后统一走这里：重画预览，并把预设标成自定义。
    pub fn masks_changed(&mut self) {
        self.preview_dirty = true;
        // 指纹本来会自校验，这里整体清一道是保守兜底（宁可多失效）。
        self.mask_geo_cache = Default::default();
        if self.preset != "自定义" {
            self.preset = "自定义".to_owned();
        }
        // 蒙版的所有写路径（面板、画布手柄、命令）最后都汇到这里，历史记一处就够。
        history::note("mask", "改局部蒙版");
    }

    // ------------------------------------------------------------- 三层缓存取值

    /// 蒙版几何取值：指纹命中直接还缓存切片，零分配（E1/E2/E3 三调用点统一入口）。
    /// 未命中重解析 `params.masks[sel]`；派生量在各自取值方法里按 img 键补建。
    pub(crate) fn mask_geos_cached(&mut self, sel: usize) -> &[crate::shell::MaskGeo] {
        if sel >= self.params.masks.len() {
            self.mask_geo_cache = Default::default();
            return &[];
        }
        let fp = crate::shell::mask_geo_fingerprint(&self.params.masks[sel]);
        if self.mask_geo_cache.fp != fp {
            let geos = crate::shell::mask_geos(&self.params.masks[sel]);
            self.mask_geo_cache = crate::shell::MaskGeoCache {
                fp,
                geos,
                ..Default::default()
            };
        }
        &self.mask_geo_cache.geos
    }

    /// 画布帧准备：一帧只做一次指纹/img 双键检查，确保几何与全部派生量就绪，
    /// 返回形状个数；随后的 `mask_geo_get` / `mask_*_now` 是纯读，零哈希。
    /// （每帧多次指纹重哈希在多形状蒙版上可测，见 T03 复测记录。）
    pub(crate) fn mask_prep(&mut self, sel: usize, img: egui::Rect, w: f64, h: f64) -> usize {
        self.mask_geos_cached(sel);
        let current = self
            .mask_comp
            .min(self.mask_geo_cache.geos.len().saturating_sub(1));
        // 派生量重建键必须含「img rect + 像素尺寸 + 当前形状」三者：
        // 漏 comp 会让手柄停在旧形状上（切换形状不跟随），
        // 漏 (w,h) 会让换预览分辨率后按旧像素尺寸映射（径向/笔画错位）。
        if self.mask_geo_cache.img != Some(img)
            || self.mask_geo_cache.px != (w, h)
            || self.mask_geo_cache.comp != current
        {
            self.refresh_mask_derived(sel, current, img, w, h);
        }
        self.mask_geo_cache.geos.len()
    }

    /// 纯读：当前缓存里第 `index` 个形状的几何（需先 `mask_prep`）。
    pub(crate) fn mask_geo_get(&self, index: usize) -> Option<crate::shell::MaskGeo> {
        self.mask_geo_cache.geos.get(index).copied()
    }

    /// 纯读：当前形状的手柄（需先 `mask_prep`）。
    pub(crate) fn mask_handles_now(&self) -> &[(egui::Pos2, bool)] {
        &self.mask_geo_cache.handles
    }

    /// 纯读：第 `comp` 个形状的径向轮廓（需先 `mask_prep`）。
    pub(crate) fn mask_radial_now(&self, comp: usize) -> &[egui::Pos2] {
        self.mask_geo_cache
            .radial
            .get(comp)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 纯读：第 `comp` 个形状的笔画折线（需先 `mask_prep`）。
    pub(crate) fn mask_strokes_now(&self, comp: usize) -> &[(Vec<egui::Pos2>, bool)] {
        self.mask_geo_cache
            .strokes
            .get(comp)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 派生量重建：只在「指纹刚换或 img 变了」时跑；几何解析层不重算。
    fn refresh_mask_derived(
        &mut self,
        sel: usize,
        current: usize,
        img: egui::Rect,
        w: f64,
        h: f64,
    ) {
        let geos: Vec<crate::shell::MaskGeo> = self.mask_geo_cache.geos.clone();
        let Some(raw) = self.params.masks.get(sel) else {
            return;
        };
        let handles = geos
            .get(current)
            .map(|g| crate::shell::mask_handles(g, img, w, h))
            .unwrap_or_default();
        let mut radial = Vec::with_capacity(geos.len());
        let mut strokes = Vec::with_capacity(geos.len());
        for (i, g) in geos.iter().enumerate() {
            radial.push(if g.radial {
                crate::shell::mask_radial_points(g, img, w, h)
            } else {
                Vec::new()
            });
            strokes.push(match crate::shell::mask_comp_object(raw, i) {
                Some(obj) if g.brush => crate::shell::mask_stroke_lines(obj, img),
                _ => Vec::new(),
            });
        }
        let cache = &mut self.mask_geo_cache;
        cache.img = Some(img);
        cache.px = (w, h);
        cache.comp = current;
        cache.handles = handles;
        cache.radial = radial;
        cache.strokes = strokes;
    }

    /// 一键自动：先按 1/50/99 百分位提一版白平衡与影调，再走真实链验收。
    ///
    /// 写的是绝对值，不是在当前值上再加一档——所以连点几次结果不变；已经曝好的
    /// 图可能只校白平衡，那不是没生效，是这张图本来就不用动。
    pub fn auto_exposure(&mut self) {
        let Some(path) = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.path.clone())
        else {
            self.auto_note = Some("还没有可测光的帧".to_owned());
            return;
        };
        let cached = self
            .auto_probe
            .as_ref()
            .filter(|(p, ..)| *p == path)
            .map(|(_, px, w, h)| (px.clone(), *w, *h));
        let (probe, w, h) = match cached {
            Some(v) => v,
            None => match metrics::decode_rgb(&path, 256) {
                Ok(img) => {
                    let (w, h) = (img.width() as usize, img.height() as usize);
                    let px: Vec<f32> = img.as_raw().iter().map(|v| *v as f32 / 255.0).collect();
                    self.auto_probe = Some((path.clone(), px.clone(), w, h));
                    (px, w, h)
                }
                Err(e) => {
                    self.auto_note = Some(format!("自动校正失败：{e}"));
                    return;
                }
            },
        };
        let q = aevum_core::auto::auto_params(&probe, w, h);
        self.params.exposure = q.exposure;
        self.params.contrast = q.contrast;
        self.params.highlights = q.highlights;
        self.params.shadows = q.shadows;
        self.params.whites = q.whites;
        self.params.blacks = q.blacks;
        self.params.temperature = q.temperature;
        self.params.tint = q.tint;
        self.auto_note = Some(if q.touches_tone() {
            format!(
                "自动：{:+.2} EV · 对比 {:+.0} · 高光 {:+.0} · 阴影 {:+.0} · 白 {:+.0} · 黑 {:+.0} · 色温 {:+.0} · 色调 {:+.0}",
                q.exposure,
                q.contrast,
                q.highlights,
                q.shadows,
                q.whites,
                q.blacks,
                q.temperature,
                q.tint
            )
        } else {
            format!(
                "影调已在安全范围，只校白平衡（色温 {:+.0} · 色调 {:+.0}）",
                q.temperature, q.tint
            )
        });
        self.preview_dirty = true;
        history::note(
            "auto-exposure",
            format!(
                "自动校正 {:+.2} EV · 色温 {:+.0}",
                q.exposure, q.temperature
            ),
        );
    }

    pub fn ensure_preview(&mut self, ctx: &Context) {
        // 按住鼠标就是在拖某个推子/曲线/蒙版把手：这时候先要的是跟手，不是那几项
        // 最贵的空间细节，所以出草图。松手才补全量——起手那一帧什么都不做，
        // 否则点一下任意按钮都会让画面闪一下草图。
        let dragging = ctx.input(|i| i.pointer.any_down());
        if self.preview_dragging && !dragging && self.preview_owes_full {
            self.preview_dirty = true;
        }
        // 刚开分屏：手边没有「之前」那张，得补一张带原图的。
        if self.split && !self.preview_has_orig && !self.preview_inflight {
            self.preview_dirty = true;
        }
        self.preview_dragging = dragging;
        if self.headless || !self.preview_dirty || self.preview_inflight || self.playing {
            return;
        }
        let sketch = dragging;
        let want_original = self.split;
        let Some(path) = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.path.clone())
        else {
            return;
        };
        self.preview_dirty = false;
        self.preview_inflight = true;
        self.preview_stamp += 1;
        let stamp = self.preview_stamp;
        let params = self.params.clone();
        let gain = self.ev_at(self.selected);
        let (crop, offset) = if self.align_preview {
            self.alignment_at(self.selected)
        } else {
            (None, [0.0, 0.0])
        };
        // 裁剪态下画整块可见区：框外那圈像素得露出来才拖得动，取景框只是覆盖层。
        // 拉直之后可见区就是安全框——转出去的反射角不画出来，画布上不会看到鬼影。
        let crop = if self.crop_editing() {
            (self.rotate != 0.0).then(|| self.crop_view())
        } else {
            self.aspect_crop(crop)
        };
        let full_size = self.resolution;
        let framing = Framing {
            angle_deg: self.rotate,
            off: offset,
        };
        // 运镜预览按源帧序取框（开了变速时成片由输出帧号定，预览只作近似）。裁剪态
        // 画的是整块可见区，拖取景框时画的是取景基准那一幅——两种态下框都只是盖在
        // 像素上的矢量，不重画。
        let n = self.frame_count();
        let view = if self.vector_editing() {
            None
        } else {
            self.motion_view_at(self.selected.min(n.saturating_sub(1)), n)
        };
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            match render_preview_file(
                &path,
                &params,
                gain,
                full_size,
                crop,
                framing,
                view,
                sketch,
                want_original,
            ) {
                Ok((graded, original, size, hist)) => {
                    let _ = tx.send(Msg::Preview(Box::new(PreviewMsg {
                        stamp,
                        sketch,
                        graded,
                        original,
                        size,
                        hist,
                    })));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("预览失败：{e}")));
                }
            }
            ctx2.request_repaint();
        });
    }

    pub fn ensure_thumbs(&mut self, ctx: &Context, lo: usize, hi: usize) {
        if self.headless {
            return;
        }
        let Some(seq) = &self.seq else { return };
        let missing: Vec<(usize, PathBuf)> = (lo..hi.min(seq.len()))
            .filter(|i| !self.thumbs.contains_key(i) && !self.thumb_pending.contains(i))
            .take(24)
            .map(|i| (i, seq.frames[i].path.clone()))
            .collect();
        if missing.is_empty() {
            return;
        }
        for (i, _) in &missing {
            self.thumb_pending.insert(*i);
        }
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let out: Vec<ThumbMsg> = thumb_pool().install(|| {
                missing
                    .into_par_iter()
                    .filter_map(|(index, path)| {
                        let img = metrics::thumbnail(&path, THUMB_SIDE).ok()?;
                        let size = [img.width(), img.height()];
                        let image = ColorImage::from_rgba_unmultiplied(
                            [size[0] as usize, size[1] as usize],
                            img.as_raw(),
                        );
                        Some(ThumbMsg { index, image, size })
                    })
                    .collect()
            });
            let _ = tx.send(Msg::Thumbs(out));
            ctx2.request_repaint();
        });
    }

    pub fn thumb(&self, i: usize) -> Option<&Thumb> {
        self.thumbs.get(&i)
    }

    // ---------------------------------------------------------------- 图库

    pub fn lib_save(&self) {
        let store = aevum_core::library::Store {
            roots: self.lib_roots.clone(),
            collections: self.lib_cols.clone(),
        };
        let _ = aevum_core::library::save(&aevum_core::library::default_path(), &store);
    }

    /// 展开一个根目录：编目（含评级/色标/旗标）从该目录自己的工程文件折出来。
    pub fn lib_scan(&mut self, index: usize) {
        while self.lib_entries.len() < self.lib_roots.len() {
            self.lib_entries.push(Vec::new());
        }
        let Some(dir) = self.lib_roots.get(index).cloned() else {
            return;
        };
        self.lib_entries[index] = aevum_core::library::scan_root(&dir).unwrap_or_default();
        self.lib_sel = Some(index);
        self.lib_sel_file = None;
        self.lib_checked.clear();
        self.lib_anchor = None;
        self.lib_col = None;
        self.lib_load_meta(index);
    }

    /// 把某个目录工程里的说明与版权读进信息面板的编辑缓冲。
    /// 只在展开/重扫目录时调用：面板每帧重绘，逐帧读会把正在输入的内容冲掉。
    pub fn lib_load_meta(&mut self, index: usize) {
        let Some(dir) = self.lib_roots.get(index).cloned() else {
            return;
        };
        self.lib_meta = aevum_core::library::metadata(&dir);
        self.lib_meta_root = Some(dir);
    }

    /// 信息面板的「存」：写回该目录的工程文件，其余字段原样保留。
    pub fn lib_save_meta(&mut self) -> Result<(), String> {
        let dir = self
            .lib_meta_root
            .clone()
            .ok_or_else(|| "还没选定图库目录".to_owned())?;
        let saved = aevum_core::library::set_metadata(&dir, &self.lib_meta)
            .map_err(|e| format!("写元数据失败：{e}"))?;
        self.lib_meta = saved.clone();
        // 这个目录正好开着：内存那份也得跟上，否则自动保存会用旧值把它盖回去。
        if self.seq.as_ref().is_some_and(|s| s.root == dir) {
            self.meta = saved.clone();
        }
        self.status = if saved.is_empty() {
            "已清空这个目录的说明与版权".to_owned()
        } else {
            "说明与版权已写进这个目录的工程文件".to_owned()
        };
        Ok(())
    }

    /// 图库加目录：同一目录只留一行并挪到末尾，随即展开编目。
    pub fn lib_add(&mut self, dir: &Path) -> usize {
        let index = aevum_core::library::add(&mut self.lib_roots, dir);
        while self.lib_entries.len() < self.lib_roots.len() {
            self.lib_entries.push(Vec::new());
        }
        self.lib_save();
        self.lib_scan(index);
        index
    }

    pub fn lib_remove(&mut self, index: usize) {
        if aevum_core::library::remove(&mut self.lib_roots, index).is_some() {
            if index < self.lib_entries.len() {
                self.lib_entries.remove(index);
            }
            self.lib_sel = None;
            self.lib_sel_file = None;
            self.lib_checked.clear();
            self.lib_anchor = None;
            // 目录序号整体前移，展开集里的键会对到别的目录：一次清空最省事。
            self.lib_stack_open.clear();
            self.lib_save();
        }
    }

    /// 在图库里直接改一张的评级/旗标/色标：写回该目录自己的工程文件，
    /// 内存编目同步改，不重扫整个目录。
    pub fn lib_organize(
        &mut self,
        root: usize,
        frame: usize,
        patch: aevum_core::library::Organize,
    ) -> Result<(), String> {
        let Some(dir) = self.lib_roots.get(root).cloned() else {
            return Err(format!("目录序号超出范围：1..={}", self.lib_roots.len()));
        };
        aevum_core::library::organize(&dir, frame, &patch).map_err(|e| e.to_string())?;
        if let Some(f) = self
            .lib_entries
            .get_mut(root)
            .and_then(|v| v.get_mut(frame))
        {
            if let Some(rating) = patch.rating {
                f.rating = rating.min(5);
            }
            if let Some(flag) = patch.flag {
                f.flag = flag.signum();
            }
            if let Some(label) = patch.label {
                f.label = label.min(4);
            }
        }
        Ok(())
    }

    /// 批量改一类帧：整批一次落盘，内存编目同步改。
    pub fn lib_organize_many(
        &mut self,
        root: usize,
        frames: &[usize],
        patch: aevum_core::library::Organize,
    ) -> Result<(), String> {
        let Some(dir) = self.lib_roots.get(root).cloned() else {
            return Err(format!("目录序号超出范围：1..={}", self.lib_roots.len()));
        };
        if frames.is_empty() {
            return Err("没有选中的照片".to_owned());
        }
        aevum_core::library::organize_many(&dir, frames, &patch).map_err(|e| e.to_string())?;
        if let Some(entries) = self.lib_entries.get_mut(root) {
            for &frame in frames {
                let Some(f) = entries.get_mut(frame) else {
                    continue;
                };
                if let Some(rating) = patch.rating {
                    f.rating = rating.min(5);
                }
                if let Some(flag) = patch.flag {
                    f.flag = flag.signum();
                }
                if let Some(label) = patch.label {
                    f.label = label.min(4);
                }
            }
        }
        Ok(())
    }

    /// 编目行被单击：普通点选一行，Ctrl 切换勾选，Shift 从锚点连选。
    pub fn lib_click(&mut self, row: usize, ctrl: bool, shift: bool) {
        if shift && let Some(anchor) = self.lib_anchor {
            let (lo, hi) = (anchor.min(row), anchor.max(row));
            self.lib_checked.extend(lo..=hi);
            self.lib_sel_file = Some(row);
            return;
        }
        if ctrl {
            if !self.lib_checked.insert(row) {
                self.lib_checked.remove(&row);
            }
            self.lib_sel_file = Some(row);
            self.lib_anchor = Some(row);
            return;
        }
        self.lib_checked.clear();
        self.lib_checked.insert(row);
        self.lib_sel_file = Some(row);
        self.lib_anchor = Some(row);
    }

    /// 键盘/右键的目标行：有勾选就作用于全部勾选，否则只作用于选中行。
    pub fn lib_targets(&self) -> Vec<(usize, usize)> {
        let Some(root) = self.lib_sel else {
            return Vec::new();
        };
        if self.lib_checked.is_empty() {
            return self
                .lib_sel_file
                .map(|f| vec![(root, f)])
                .unwrap_or_default();
        }
        self.lib_checked.iter().map(|&f| (root, f)).collect()
    }

    /// 对当前目标行套一次整理状态；返回改了几张。
    pub fn lib_organize_targets(
        &mut self,
        patch: aevum_core::library::Organize,
    ) -> Result<usize, String> {
        let targets = self.lib_targets();
        let Some(&(root, _)) = targets.first() else {
            return Err("还没选中照片".to_owned());
        };
        let frames: Vec<usize> = targets.iter().map(|&(_, f)| f).collect();
        self.lib_organize_many(root, &frames, patch)?;
        Ok(frames.len())
    }

    /// 给目标行加/去关键词（勾选优先）；整批只读写一次工程文件。
    pub fn lib_keyword_edit(&mut self, add: &[String], remove: &[String]) -> Result<usize, String> {
        let targets = self.lib_targets();
        let Some(&(root, _)) = targets.first() else {
            return Err("还没选中照片".to_owned());
        };
        let frames: Vec<usize> = targets.iter().map(|&(_, f)| f).collect();
        self.lib_keyword_edit_many(root, &frames, add, remove)
    }

    pub fn lib_keyword_edit_many(
        &mut self,
        root: usize,
        frames: &[usize],
        add: &[String],
        remove: &[String],
    ) -> Result<usize, String> {
        let Some(dir) = self.lib_roots.get(root).cloned() else {
            return Err(format!("目录序号超出范围：1..={}", self.lib_roots.len()));
        };
        if frames.is_empty() {
            return Err("没有选中的照片".to_owned());
        }
        let touched = aevum_core::library::keyword_edit(&dir, frames, add, remove)
            .map_err(|e| e.to_string())?;
        // 内存编目同步：词表归一化、排序的规则只在 core 里有一份，改完重读一次工程。
        let proj =
            aevum_core::project::load(&aevum_core::project::project_path(&dir)).unwrap_or_default();
        if let Some(entries) = self.lib_entries.get_mut(root) {
            for &frame in frames {
                if let Some(f) = entries.get_mut(frame) {
                    f.keywords = proj.keywords.get(&frame).cloned().unwrap_or_default();
                }
            }
        }
        Ok(touched)
    }

    /// 堆叠改完之后把结果同步回内存：编目行的 `stack` 全部重算（叠号会整体挪），
    /// 打开着的序列正好在这个目录时 `self.stacks` 也跟着走，否则自动保存会用旧值盖回去。
    fn lib_stack_sync(&mut self, root: usize, stacks: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        if let Some(entries) = self.lib_entries.get_mut(root) {
            for f in entries.iter_mut() {
                f.stack = aevum_core::library::stack_of(&stacks, f.index);
            }
        }
        // 叠号会整体挪动，旧的展开集对不上新叠：这个目录一律收回折叠态。
        self.lib_stack_open.retain(|&(r, _)| r != root);
        if let Some(dir) = self.lib_roots.get(root)
            && self.seq.as_ref().is_some_and(|s| &s.root == dir)
        {
            self.stacks = stacks.clone();
        }
        stacks
    }

    fn lib_dir(&self, root: usize) -> Result<PathBuf, String> {
        self.lib_roots
            .get(root)
            .cloned()
            .ok_or_else(|| format!("目录序号超出范围：1..={}", self.lib_roots.len()))
    }

    /// 并成一叠：写一次盘，返回归一化后的全部堆叠。少于两张由调用方拦。
    pub fn lib_stack_create(
        &mut self,
        root: usize,
        frames: &[usize],
    ) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let stacks = aevum_core::library::stack_create(&dir, frames).map_err(|e| e.to_string())?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 拆散一叠（叠号 0 起）；越界当没这叠。
    pub fn lib_stack_dissolve(
        &mut self,
        root: usize,
        stack: usize,
    ) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let stacks = aevum_core::library::stack_dissolve(&dir, stack).map_err(|e| e.to_string())?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 按文件名前缀自动分叠（`A001_0001`…/`B002_0001`… 各成一叠）：整目录重写堆叠，
    /// 评级、关键词、元数据一律不动。
    pub fn lib_stack_auto(&mut self, root: usize) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let names: Vec<String> = match self.lib_entries.get(root) {
            Some(entries) if !entries.is_empty() => {
                entries.iter().map(|f| f.name.clone()).collect()
            }
            _ => return Err("这个目录还没编目：先展开它".to_owned()),
        };
        let stacks = aevum_core::library::stack_auto(&dir, &names).map_err(|e| e.to_string())?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 把目标行（勾选优先）并成一叠；返回这一叠的张数。
    pub fn lib_stack_targets(&mut self) -> Result<usize, String> {
        let targets = self.lib_targets();
        let Some(&(root, first)) = targets.first() else {
            return Err("还没选中照片".to_owned());
        };
        let frames: Vec<usize> = targets.iter().map(|&(_, f)| f).collect();
        if frames.len() < 2 {
            return Err("堆叠至少要选两张照片（勾住它们再试）".to_owned());
        }
        let stacks = self.lib_stack_create(root, &frames)?;
        Ok(stacks
            .get(aevum_core::library::stack_of(&stacks, first).unwrap_or(usize::MAX))
            .map(|g| g.len())
            .unwrap_or(0))
    }

    /// 拆散目标行所在的那一叠；返回被放出来的张数。目标行不在叠里就是错。
    pub fn lib_stack_dissolve_targets(&mut self) -> Result<usize, String> {
        let targets = self.lib_targets();
        let Some(&(root, first)) = targets.first() else {
            return Err("还没选中照片".to_owned());
        };
        let Some(stack) = self
            .lib_entries
            .get(root)
            .and_then(|entries| entries.get(first))
            .and_then(|f| f.stack)
        else {
            return Err("这张照片不在任何叠里".to_owned());
        };
        let members = self
            .lib_entries
            .get(root)
            .map(|entries| entries.iter().filter(|f| f.stack == Some(stack)).count())
            .unwrap_or(0);
        self.lib_stack_dissolve(root, stack)?;
        Ok(members)
    }

    /// 选中的编目行 →（目录序号, 帧号, 文件），键盘与「加入集合」共用。
    pub fn lib_selected(&self) -> Option<(usize, usize, aevum_core::library::LibFile)> {
        let (root, frame) = (self.lib_sel?, self.lib_sel_file?);
        let f = self.lib_entries.get(root)?.get(frame)?.clone();
        Some((root, frame, f))
    }

    /// 编目里上下挪选中行；跳过被筛掉的行。
    pub fn lib_step_row(&mut self, delta: i64) {
        let Some(root) = self.lib_sel else { return };
        let Some(entries) = self.lib_entries.get(root) else {
            return;
        };
        let n = entries.len() as i64;
        if n == 0 {
            return;
        }
        let step = delta.signum();
        let mut i = self
            .lib_sel_file
            .map(|v| v as i64)
            .unwrap_or(if step > 0 { -1 } else { n });
        loop {
            i += step;
            if i < 0 || i >= n {
                return;
            }
            if self.lib_filter.keeps(&entries[i as usize]) {
                self.lib_sel_file = Some(i as usize);
                self.lib_checked.clear();
                self.lib_anchor = Some(i as usize);
                return;
            }
        }
    }

    pub fn lib_collection_create(&mut self, name: &str) -> usize {
        let index = aevum_core::library::collection_create(&mut self.lib_cols, name);
        self.lib_save();
        index
    }

    pub fn lib_collection_delete(&mut self, index: usize) -> bool {
        let deleted = aevum_core::library::collection_delete(&mut self.lib_cols, index).is_some();
        if deleted {
            self.lib_col = None;
            self.lib_save();
        }
        deleted
    }

    /// 把一张照片放进集合；已存在返回 `false`。
    pub fn lib_collection_put(&mut self, col: usize, path: &Path) -> Result<bool, String> {
        if col >= self.lib_cols.len() {
            return Err(format!("集合序号超出范围：1..={}", self.lib_cols.len()));
        }
        let added = aevum_core::library::collection_put(&mut self.lib_cols, col, path);
        if added {
            self.lib_save();
        }
        Ok(added)
    }

    /// 一次把几张放进集合；返回新加进去的张数（已在里面的不算）。
    pub fn lib_collection_put_many(&mut self, col: usize, paths: &[PathBuf]) -> usize {
        let mut added = 0;
        for path in paths {
            if aevum_core::library::collection_put(&mut self.lib_cols, col, path) {
                added += 1;
            }
        }
        if added > 0 {
            self.lib_save();
        }
        added
    }

    pub fn lib_collection_drop(&mut self, col: usize, path: &Path) -> bool {
        let dropped = aevum_core::library::collection_drop(&mut self.lib_cols, col, path);
        if dropped {
            self.lib_save();
        }
        dropped
    }

    /// 集合视图的行：成员路径先在已编目的根目录里找（评级与帧号跟着来），
    /// 找不到的给一行只带路径的占位——目录还没展开过时也能看集合有哪些照片。
    pub fn lib_collection_rows(&self, col: usize) -> Vec<aevum_core::library::LibFile> {
        let Some(collection) = self.lib_cols.get(col) else {
            return Vec::new();
        };
        collection
            .files
            .iter()
            .map(|path| {
                let path = PathBuf::from(path);
                self.lib_entries
                    .iter()
                    .flatten()
                    .find(|f| f.path == path)
                    .cloned()
                    .unwrap_or_else(|| aevum_core::library::LibFile {
                        name: path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string()),
                        path,
                        index: 0,
                        rating: 0,
                        label: 0,
                        flag: 0,
                        keywords: Vec::new(),
                        // 集合是跨目录的路径清单，没有"这个目录里的第几叠"可言。
                        stack: None,
                    })
            })
            .collect()
    }

    /// 图库可见行的缩略图：按路径键，后台线程与序列缩略图共用同一个池。
    pub fn lib_ensure_thumbs(&mut self, ctx: &Context, lo: usize, hi: usize) {
        let Some(index) = self.lib_sel else { return };
        let Some(entries) = self.lib_entries.get(index) else {
            return;
        };
        let hi = hi.min(entries.len());
        let paths: Vec<PathBuf> = entries
            .get(lo..hi)
            .unwrap_or(&[])
            .iter()
            .map(|f| f.path.clone())
            .collect();
        self.lib_ensure_thumb_paths(ctx, paths);
    }

    /// 任意一批照片的缩略图（集合视图也走这里）。
    pub fn lib_ensure_thumb_paths(&mut self, ctx: &Context, paths: Vec<PathBuf>) {
        if self.headless {
            return;
        }
        let mut missing: Vec<PathBuf> = Vec::new();
        for path in paths {
            if self.lib_thumbs.contains_key(&path) || self.lib_thumb_pending.contains(&path) {
                continue;
            }
            missing.push(path);
            if missing.len() >= 24 {
                break;
            }
        }
        if missing.is_empty() {
            return;
        }
        for p in &missing {
            self.lib_thumb_pending.insert(p.clone());
        }
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let out: Vec<LibThumbMsg> = thumb_pool().install(|| {
                missing
                    .into_par_iter()
                    .filter_map(|path| {
                        let img = metrics::thumbnail(&path, THUMB_SIDE).ok()?;
                        let size = [img.width(), img.height()];
                        let image = ColorImage::from_rgba_unmultiplied(
                            [size[0] as usize, size[1] as usize],
                            img.as_raw(),
                        );
                        Some(LibThumbMsg { path, image, size })
                    })
                    .collect()
            });
            let _ = tx.send(Msg::LibThumbs(out));
            ctx2.request_repaint();
        });
    }

    pub fn keyframe_near(&self, tol: usize) -> Option<usize> {
        self.keyframes
            .keys()
            .find(|k| (**k as i64 - self.selected as i64).unsigned_abs() as usize <= tol)
            .copied()
    }

    pub fn add_keyframe(&mut self) {
        self.keyframes.insert(self.selected, self.params.clone());
        history::note(
            "keyframe-add",
            format!("记录关键帧 帧{:04}", self.selected + 1),
        );
    }

    pub fn remove_keyframe(&mut self, i: usize) {
        if self.keyframes.remove(&i).is_some() {
            history::note("keyframe-del", format!("删除关键帧 帧{i:04}", i = i + 1));
        }
    }

    pub fn goto_keyframe(&mut self, i: usize) {
        if let Some(p) = self.keyframes.get(&i).cloned() {
            self.selected = i.min(self.frame_count().saturating_sub(1));
            self.params = p;
            self.preset = "关键帧".to_owned();
            self.preview_dirty = true;
            self.scroll_to_selected = true;
            history::note("keyframe-goto", format!("套用关键帧 帧{i:04}", i = i + 1));
        }
    }

    // ------------------------------------------------------------ 调色副本

    pub fn copy_index(&self, name: &str) -> Option<usize> {
        self.copies.iter().position(|c| c.name == name)
    }

    /// 当前调色等于哪套副本：整表现算，不留一个改参后会过期的字段。
    pub fn active_copy(&self) -> Option<&str> {
        self.copies
            .iter()
            .find(|c| c.params == self.params && c.keyframes == self.keyframes)
            .map(|c| c.name.as_str())
    }

    pub fn save_copy(&mut self, name: &str, overwrite: bool) -> Result<usize, String> {
        let name = clean_copy_name(name)?;
        match self.copy_index(&name) {
            Some(_) if !overwrite => Err(format!("副本「{name}」已存在；要覆盖给 overwrite: true")),
            Some(i) => {
                self.copies[i].params = self.params.clone();
                self.copies[i].keyframes = self.keyframes.clone();
                history::note("copy-save", format!("覆盖副本 {name}"));
                Ok(i)
            }
            None => {
                self.copies.push(GradeCopy {
                    name: name.clone(),
                    params: self.params.clone(),
                    keyframes: self.keyframes.clone(),
                });
                history::note("copy-save", format!("存副本 {name}"));
                Ok(self.copies.len() - 1)
            }
        }
    }

    pub fn apply_copy(&mut self, name: &str) -> Result<(), String> {
        let Some(copy) = self.copies.iter().find(|c| c.name == name).cloned() else {
            return Err(format!("没有叫 {name} 的副本"));
        };
        self.params = copy.params;
        self.keyframes = copy.keyframes;
        self.preset = format!("副本 {}", copy.name);
        self.preview_dirty = true;
        history::note("copy-apply", format!("套用副本 {}", copy.name));
        Ok(())
    }

    pub fn delete_copy(&mut self, name: &str) -> Result<(), String> {
        let Some(i) = self.copy_index(name) else {
            return Err(format!("没有叫 {name} 的副本"));
        };
        self.copies.remove(i);
        history::note("copy-del", format!("删副本 {name}"));
        Ok(())
    }

    pub fn rename_copy(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = clean_copy_name(to)?;
        let Some(i) = self.copy_index(from) else {
            return Err(format!("没有叫 {from} 的副本"));
        };
        if self.copy_index(&to).is_some() {
            return Err(format!("副本「{to}」已存在"));
        }
        self.copies[i].name = to.clone();
        history::note("copy-rename", format!("副本 {from} 改名 {to}"));
        Ok(())
    }

    fn unload_jobs(&mut self) {
        self.cancel_export.store(true, Ordering::SeqCst);
        self.preview_inflight = false;
        self.align_busy = false;
        self.export_busy = false;
        self.export_stage = None;
        self.align_done.store(0, Ordering::Relaxed);
        self.export_done.store(0, Ordering::Relaxed);
        self.export_total.store(0, Ordering::Relaxed);
        self.export_stage_shared.store(0, Ordering::Relaxed);
        // 插件在飞也要收：换序列时旧结果没有意义，让任务自己撞取消旗。
        if self.plugin_job.is_some() {
            self.plugin_cancel();
        }
    }

    /// 对齐用的几何：裁切框 + 当前帧位移；没跑过对齐就是全画幅零位移。
    fn alignment_at(&self, i: usize) -> (Option<CropRect>, [f64; 2]) {
        match &self.align_report {
            Some(r) => (
                Some(r.crop),
                r.offsets.get(i).copied().unwrap_or([0.0, 0.0]),
            ),
            None => (None, [0.0, 0.0]),
        }
    }

    /// 拉直削边：整幅转过 `rotate` 之后仍然完全有像素的那块居中区域。
    /// 它既是导出的上限，也是画布上拖框的上限——框外没有像素，拖出去只能削成空白。
    fn straighten_bounds(&self, base: Option<CropRect>) -> Option<CropRect> {
        if self.rotate.abs() <= 1e-6 {
            return base;
        }
        let (rw, rh) = self.resolution?;
        let safe = crop::safe_rect(rw, rh, self.rotate, self.aspect.pair());
        Some(match base {
            Some(b) => crop::intersect(b, safe).unwrap_or(safe),
            None => safe,
        })
    }

    /// 画幅比例锁定的取景框（原图像素）：用户框优先，没拖过框才按比例内接。
    fn aspect_crop(&self, base: Option<CropRect>) -> Option<CropRect> {
        let base = self.straighten_bounds(base);
        let Some(user) = self.user_crop else {
            // 原生比例不内接，这时直接返回拉直削出来的框：丢掉它等于把转出去的
            // 反射角留在导出里。
            return fit_aspect(base, self.resolution, self.aspect).or(base);
        };
        // 用户框是事实来源，对齐安全框只负责削边；削完再兜一次比例，
        // 免得导出的边比画布上看到的多出一条。
        let framed = match base {
            Some(b) => crop::intersect(user, b).unwrap_or(user),
            None => user,
        };
        fit_aspect(Some(framed), self.resolution, self.aspect).or(Some(framed))
    }

    /// 导出取景：对齐裁切（始终生效，和队列同口径）再叠用户框与画幅比例。
    fn effective_crop(&self) -> Option<CropRect> {
        self.aspect_crop(self.align_report.as_ref().map(|r| r.crop))
    }

    /// 画布当前是不是裁剪态：裁剪工具或构图模块。这时预览画整帧，
    /// 取景框只是盖在上面的一层矢量，拖它不重画像素。
    pub fn crop_editing(&self) -> bool {
        self.tool == Tool::Crop || self.module == Module::Frame
    }

    /// 画布上是「像素不动、只拖矢量框」的两种态之一：拖裁剪框或拖运镜取景框。
    /// 这两种态下预览都不把取景框叠进像素，所以拖动跟手。
    pub fn vector_editing(&self) -> bool {
        self.crop_editing() || self.motion_editing()
    }

    /// 画布上要画的那个框：拖过的用拖出来的，没拖过给当前取景（再兜底整块可见区）。
    pub fn crop_box(&self) -> CropRect {
        self.user_crop
            .or_else(|| self.effective_crop())
            .unwrap_or_else(|| self.crop_view())
    }

    /// 画布上画出来的那块像素（旋转后坐标）：整幅，或拉直削出来的安全区。
    /// 裁剪态的覆盖层按它换算屏幕坐标，所以框永远只在有像素的地方走。
    pub fn crop_view(&self) -> CropRect {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        let full = CropRect { x: 0, y: 0, w, h };
        match self.straighten_bounds(None) {
            Some(safe) => crop::intersect(safe, full).unwrap_or(full),
            None => full,
        }
    }

    /// 拖出来的框不能伸进转出来的空白角：越出去的部分只是反射像素。
    fn clamp_to_view(&self, r: CropRect) -> CropRect {
        match self.straighten_bounds(None) {
            Some(safe) => crop::intersect(r, safe).unwrap_or(safe),
            None => r,
        }
    }

    /// 按下画布：命中把手就缩放，命中框内就平移，落在框外就重拉一个框。
    ///
    /// 这里只摆拖拽姿态，不写 `user_crop`、不登记历史——「按下」本身不是编辑。
    /// 框的固化和历史登记都在 `crop_drag_to`：一次拖动 = 一步历史，
    /// 点一下画布什么都不会留下（v0.3.0 之前纯点击也会挤出一条裁剪历史）。
    pub fn crop_begin(&mut self, at: [f64; 2], hit: crop::Hit) {
        let cur = self.crop_box();
        self.crop_drag = Some(match hit {
            crop::Hit::Corner(handle) | crop::Hit::Edge(handle) => {
                CropDrag::Resize { handle, orig: cur }
            }
            crop::Hit::Inside => CropDrag::Move {
                orig: cur,
                grab: [at[0] - cur.x as f64, at[1] - cur.y as f64],
            },
            crop::Hit::Outside => CropDrag::Draw { anchor: at },
        });
    }

    /// 拖动中：几何规则全在 `aevum_core::crop`，这里只喂坐标。
    pub fn crop_drag_to(&mut self, at: [f64; 2]) {
        let Some(drag) = self.crop_drag else { return };
        let Some((w, h)) = self.resolution else {
            return;
        };
        let aspect = self.aspect.pair();
        let next = match drag {
            CropDrag::Draw { anchor } => {
                crop::draw_rect((anchor[0], anchor[1]), at[0], at[1], w, h, aspect)
            }
            CropDrag::Move { orig, grab } => {
                crop::move_rect(orig, at[0] - grab[0], at[1] - grab[1], w, h)
            }
            CropDrag::Resize { handle, orig } => {
                crop::resize_rect(handle, orig, at[0], at[1], w, h, aspect)
            }
        };
        let next = self.clamp_to_view(next);
        self.user_crop = Some(next);
        // 每帧都记一次同一个键：连续拖动合成一步，文案跟着框的尺寸走。
        history::note(
            "crop-drag",
            format!("裁剪 {w} × {h}", w = next.w, h = next.h),
        );
    }

    /// 松手：框铺满可见区等于没裁，退回默认取景，省掉一次无谓的重画。
    pub fn crop_end(&mut self) {
        self.crop_drag = None;
        let view = self.crop_view();
        if let Some(u) = self.user_crop
            && (u.x, u.y, u.w, u.h) == (view.x, view.y, view.w, view.h)
        {
            self.user_crop = None;
        }
    }

    /// 方向键微调取景框：`step` 一般给 2，按住 Shift 给 10。
    pub fn crop_nudge(&mut self, dx: i64, dy: i64, step: u32) {
        let Some((w, h)) = self.resolution else {
            return;
        };
        let cur = self.crop_box();
        self.user_crop = Some(self.clamp_to_view(crop::nudge_rect(cur, dx, dy, step, w, h)));
        history::note("crop-nudge", format!("裁剪微调 {dx:+} {dy:+}"));
    }

    /// 取消手动裁剪，回到「按比例内接」的默认取景。
    pub fn crop_reset(&mut self) {
        if self.user_crop.take().is_some() {
            self.crop_drag = None;
            self.preview_dirty = true;
            history::note("crop-reset", "取消裁剪");
        }
    }

    /// 画布是不是在拖运镜取景框：这时预览画的是取景基准那一幅（不是框里的内容），
    /// 两个框只是盖在上面的矢量，所以拖动一个像素都不重画。裁剪态优先——
    /// 同一时刻只有一层矢量框拿鼠标。
    pub fn motion_editing(&self) -> bool {
        self.motion_picking && !self.crop_editing()
    }

    /// 开关画布的取景框编辑：进入时要重画一次「整幅」预览，离开时要重画回
    /// 框里的内容，两头都得脏一次。
    pub fn set_motion_picking(&mut self, on: bool) {
        if self.motion_picking == on {
            return;
        }
        self.motion_picking = on;
        // 与裁剪工具抢同一只手鼠标：哪一头开，另一头的拖拽就当场作废。
        if on {
            self.crop_drag = None;
            if self.crop_editing() {
                self.tool = Tool::Hand;
            }
        } else {
            self.motion_drag = None;
        }
        self.preview_dirty = true;
    }

    pub fn set_motion_box(&mut self, which: MotionBox) {
        if self.motion_box == which {
            return;
        }
        self.motion_box = which;
        self.motion_drag = None;
    }

    /// 取景框归一化的基准那一幅（拉直 + 导出取景之后的像素框，对齐坐标）。
    /// 画布把预览铺回这一幅，框才画在它对得上的位置上。
    pub fn motion_base_rect(&self) -> CropRect {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        self.effective_crop()
            .unwrap_or(CropRect { x: 0, y: 0, w, h })
    }

    /// 取景框换算成基准那一幅里的整数像素框（对齐坐标，带基准原点偏移）。
    /// 画布画它、拖它都走这个口径，归一化与像素的往返只在这里发生一次。
    pub fn motion_rect_of(&self, v: Viewfinder) -> CropRect {
        let base = self.motion_base_rect();
        let v = v.normalized();
        let local = crop::norm_rect(
            v.x * f64::from(base.w),
            v.y * f64::from(base.h),
            (v.x + v.w) * f64::from(base.w),
            (v.y + v.h) * f64::from(base.h),
            base.w,
            base.h,
        );
        CropRect {
            x: base.x + local.x,
            y: base.y + local.y,
            w: local.w,
            h: local.h,
        }
    }

    /// 某一端当前占的那块像素。
    pub fn motion_box_rect(&self, which: MotionBox) -> CropRect {
        self.motion_rect_of(match which {
            MotionBox::Start => self.motion.from,
            MotionBox::End => self.motion.to,
        })
    }

    /// 选中帧上运镜走到的那一格：画布上给一条「现在在哪」的参照框。
    pub fn motion_current_rect(&self) -> Option<CropRect> {
        let n = self.frame_count();
        self.motion_view_at(self.selected.min(n.saturating_sub(1)), n)
            .map(|v| self.motion_rect_of(v))
    }

    /// 拖出来的整数像素框写回归一化取景框。基准尺寸不整除时按像素认，
    /// 画布上看到的就是最后留下的画面。
    fn motion_box_write(&mut self, r: CropRect) {
        let base = self.motion_base_rect();
        let (bw, bh) = (f64::from(base.w).max(1.0), f64::from(base.h).max(1.0));
        let v = Viewfinder {
            x: (f64::from(r.x) - f64::from(base.x)) / bw,
            y: (f64::from(r.y) - f64::from(base.y)) / bh,
            w: f64::from(r.w) / bw,
            h: f64::from(r.h) / bh,
        }
        .normalized();
        if self.motion_box == MotionBox::Start {
            self.motion.from = v;
        } else {
            self.motion.to = v;
        }
    }

    /// 运镜框的比例钉在输出画幅上：框和最后那一条一样形，画布上看到的就是成片，
    /// 免得拖完还要在心里再裁一次。
    ///
    /// 比要约到最简（3840×2160 → 16:9）：`crop` 的定比例几何以「两个单位」为最小
    /// 步长，直接拿像素尺寸当比例会算出一个比画面还宽的框，落点就被夹飞了。
    /// 约完仍然撑不进基准那一幅（小图配超宽画幅）就退回自由比例，让 `view_window`
    /// 在导出时收那一格。
    fn motion_box_aspect(&self) -> Option<(u32, u32)> {
        let (ow, oh) = self.motion_output_size();
        let (fw, fh) = self.motion_frame_size();
        if ow < 2 || oh < 2 {
            return None;
        }
        let (mut a, mut b) = (ow, oh);
        while b != 0 {
            let t = a % b;
            a = b;
            b = t;
        }
        let (num, den) = (ow / a.max(1), oh / a.max(1));
        (num * 2 <= fw && den * 2 <= fh).then_some((num, den))
    }

    /// 按下画布：把手=缩放，框内=平移，框外=重拉。与裁剪同一套判定，只是坐标
    /// 换算落在基准那一幅上、落账写进 `motion.from` 或 `motion.to`。
    ///
    /// 与 `crop_begin` 同一条规矩：按下只摆姿态，不落账不记历史。「点一下就
    /// 偷偷启用运镜并挤出一条历史」是 v0.3.0 的实测毛病；启用发生在真的拖动时。
    pub fn motion_begin(&mut self, at: [f64; 2], hit: crop::Hit) {
        let cur = self.motion_box_rect(self.motion_box);
        self.motion_drag = Some(match hit {
            crop::Hit::Corner(handle) | crop::Hit::Edge(handle) => {
                CropDrag::Resize { handle, orig: cur }
            }
            crop::Hit::Inside => CropDrag::Move {
                orig: cur,
                grab: [at[0] - f64::from(cur.x), at[1] - f64::from(cur.y)],
            },
            crop::Hit::Outside => CropDrag::Draw { anchor: at },
        });
    }

    /// 拖动中：只改草稿对应的取景框，不重画预览。
    pub fn motion_drag_to(&mut self, at: [f64; 2]) {
        let Some(drag) = self.motion_drag else { return };
        let base = self.motion_base_rect();
        if base.w < 2 || base.h < 2 {
            return;
        }
        let aspect = self.motion_box_aspect();
        // 拖拽坐标是基准那一幅的局部像素，几何就在局部里算，宽高自然兜得住。
        let (bx, by) = (f64::from(base.x), f64::from(base.y));
        let local = |r: CropRect| CropRect {
            x: r.x - base.x,
            y: r.y - base.y,
            w: r.w,
            h: r.h,
        };
        let next = match drag {
            CropDrag::Draw { anchor } => crop::draw_rect(
                (anchor[0] - bx, anchor[1] - by),
                at[0] - bx,
                at[1] - by,
                base.w,
                base.h,
                aspect,
            ),
            CropDrag::Move { orig, grab } => crop::move_rect(
                local(orig),
                at[0] - bx - grab[0],
                at[1] - by - grab[1],
                base.w,
                base.h,
            ),
            CropDrag::Resize { handle, orig } => crop::resize_rect(
                handle,
                local(orig),
                at[0] - bx,
                at[1] - by,
                base.w,
                base.h,
                aspect,
            ),
        };
        // 开关没推上去就拖：框是有效的，运镜就该生效，不能让人白拖一趟。
        // 放在真拖动这里而不是 begin：纯点击不启用、不记历史。
        self.motion.enabled = true;
        self.motion_box_write(CropRect {
            x: base.x + next.x,
            y: base.y + next.y,
            w: next.w,
            h: next.h,
        });
        history::note(
            "motion-drag",
            format!(
                "运镜{} {w} × {h}",
                self.motion_box.cn(),
                w = next.w,
                h = next.h
            ),
        );
    }

    /// 松手：预览不用重画（拖的那一层是矢量），只把这次的尺寸留在历史上。
    pub fn motion_end(&mut self) {
        self.motion_drag = None;
    }

    /// 方向键微调当前拖的那一端：`step` 一般给 2，按住 Shift 给 10。只在取景框编辑态用。
    pub fn motion_nudge(&mut self, dx: i64, dy: i64, step: u32) {
        let base = self.motion_base_rect();
        if base.w < 2 || base.h < 2 {
            return;
        }
        let cur = self.motion_box_rect(self.motion_box);
        let local = CropRect {
            x: cur.x - base.x,
            y: cur.y - base.y,
            w: cur.w,
            h: cur.h,
        };
        let moved = crop::nudge_rect(local, dx, dy, step, base.w, base.h);
        self.motion_box_write(CropRect {
            x: base.x + moved.x,
            y: base.y + moved.y,
            w: moved.w,
            h: moved.h,
        });
        history::note(
            "motion-nudge",
            format!("运镜{}微调 {dx:+} {dy:+}", self.motion_box.cn()),
        );
    }

    /// 开关运镜：关掉就是不动，预览得从「框里的内容」重画回整幅。
    pub fn set_motion_enabled(&mut self, on: bool) {
        if self.motion.enabled == on {
            return;
        }
        self.motion.enabled = on;
        self.preview_dirty = true;
        history::note("motion-toggle", if on { "开启运镜" } else { "关闭运镜" });
    }

    pub fn set_motion_ease(&mut self, e: Ease) {
        if self.motion.ease == e {
            return;
        }
        self.motion.ease = e;
        self.preview_dirty = true;
        history::note("motion-ease", format!("运动曲线 {}", e.cn()));
    }

    /// 起点终点互换：倒放同一条运动，框不用重拖。
    pub fn motion_swap(&mut self) {
        std::mem::swap(&mut self.motion.from, &mut self.motion.to);
        self.preview_dirty = true;
        history::note("motion-swap", "运镜反向");
    }

    /// 把当前拖的那一端铺满整幅：想「从全景推到细节」时先按一下，省得拉把手拉半天。
    pub fn motion_fill_box(&mut self) {
        match self.motion_box {
            MotionBox::Start => self.motion.from = crop::FULL,
            MotionBox::End => self.motion.to = crop::FULL,
        }
        self.motion.enabled = true;
        self.preview_dirty = true;
        history::note("motion-fill", format!("运镜{}铺满", self.motion_box.cn()));
    }

    /// 清除运镜：整块回到默认（关着、起点整幅、终点往里收一档）。
    pub fn motion_reset(&mut self) {
        if self.motion == Motion::default() {
            return;
        }
        self.motion = Motion::default();
        self.motion_drag = None;
        self.preview_dirty = true;
        history::note("motion-reset", "清除运镜");
    }

    pub fn set_aspect(&mut self, a: Aspect) {
        if self.aspect == a {
            return;
        }
        self.aspect = a;
        // 比例变了，用户框绕自己的中心重铺，不把它甩到别处去。
        if let (Some(u), Some((w, h))) = (self.user_crop, self.resolution) {
            self.user_crop = Some(crop::refit_rect(u, w, h, a.pair()));
        }
        // 运镜的两个框不用重铺：取景基准就是导出取景那一幅，画幅一改基准自己就换形，
        // 框按归一化跟着走，像素比例自动贴上新比例。再 refit 一次就是双重收缩，
        // 选好的构图会凭空多推近一档。
        self.preview_dirty = true;
    }

    /// 拉直：把整幅按 `deg` 度（正角=画面逆时针）转过去。0 附近吸一下，
    /// 因为推子停在 -0.3 这种值上时，人想要的一定是「没转」。
    pub fn set_rotate(&mut self, deg: f64) {
        let snapped = if deg.abs() <= 0.2 { 0.0 } else { deg };
        if (self.rotate - snapped).abs() <= 1e-6 {
            return;
        }
        self.rotate = snapped;
        // 角度是绕中心转的，安全框只会往里收：已有框伸进新空白角的话当场削回来，
        // 免得画布上一个框、导出另一个框。
        if let Some(cur) = self.user_crop {
            self.user_crop = Some(self.clamp_to_view(cur));
        }
        self.preview_dirty = true;
    }

    pub fn set_tool(&mut self, t: Tool) {
        // 裁剪工具和运镜拖框抢的是同一只手鼠标：切到裁剪就退出取景框编辑。
        if t == Tool::Crop {
            self.set_motion_picking(false);
        }
        let before = (self.crop_editing(), self.motion_editing());
        self.tool = t;
        if before != (self.crop_editing(), self.motion_editing()) {
            self.crop_drag = None;
            self.motion_drag = None;
            self.preview_dirty = true;
        }
    }

    pub fn set_module(&mut self, m: Module) {
        let before = (self.crop_editing(), self.motion_editing());
        self.module = m;
        if before != (self.crop_editing(), self.motion_editing()) {
            self.crop_drag = None;
            self.motion_drag = None;
            self.preview_dirty = true;
        }
    }

    pub fn run_align(&mut self, ctx: &Context) {
        if self.align_busy || self.headless {
            return;
        }
        let Some(seq) = &self.seq else { return };
        if seq.is_empty() {
            return;
        }
        let paths = seq.paths();
        let options = align::AlignOptions {
            reference: self.align_ref.min(seq.len() - 1),
            estimator: self.align_estimator,
            ..align::AlignOptions::default()
        };
        self.align_busy = true;
        self.align_msg = None;
        self.align_done.store(0, Ordering::Relaxed);
        self.status = format!("正在对齐 {} 帧…", paths.len());

        let token = self.reload_token;
        let done = self.align_done.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let progress = |n: usize, _total: usize| {
                done.store(n, Ordering::Relaxed);
            };
            match align::measure_with_progress(&paths, &options, &progress) {
                Ok(report) => {
                    let _ = tx.send(Msg::Aligned { token, report });
                }
                Err(e) => {
                    let _ = tx.send(Msg::AlignFailed(e.to_string()));
                }
            }
            ctx2.request_repaint();
        });
    }

    pub fn run_export(&mut self, ctx: &Context) {
        if self.export_busy || self.headless {
            return;
        }
        let Some(seq) = &self.seq else { return };
        let paths = seq.paths();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        let mode = self.render_mode;
        let out_root = if self.render_out_dir.as_os_str().is_empty() {
            seq.root.join("成片")
        } else {
            self.render_out_dir.clone()
        };
        // 视频导出的中间帧单独放一层，收尾时只清理这一层里我们自己写的文件。
        let out_dir = if mode == OutputMode::Video {
            out_root.join("_序列帧")
        } else {
            out_root.clone()
        };
        let video_out = if self.render_video_out.as_os_str().is_empty() {
            out_root.join("成片.mp4")
        } else {
            self.render_video_out.clone()
        };
        // 命令通道指定过副本时，这一次导出用那套副本的调色；取走即清，
        // 界面上再点导出回到当前调色，不会有人忘了它还挂着。
        let grade = self.export_copy.take().and_then(|name| {
            self.copies
                .iter()
                .find(|c| c.name == name)
                .map(|c| (c.params.clone(), c.keyframes.clone()))
        });
        let (base_params, base_keys) = match &grade {
            Some((params, keys)) => (params, keys),
            None => (&self.params, &self.keyframes),
        };
        let frame_params: Vec<GradeParams> = (0..n)
            .map(|i| interp::params_at(base_params, base_keys, i))
            .collect();
        let gains: Vec<Ev> = (0..n).map(|i| self.ev_at(i)).collect();
        let mut offsets = vec![[0.0, 0.0]; n];
        if let Some(report) = &self.align_report {
            for (i, off) in report.offsets.iter().take(n).enumerate() {
                offsets[i] = *off;
            }
        }
        // 导出取景和画布上那个框是同一份几何：对齐安全框 ∘ 用户框 ∘ 画幅比例。
        let crop = self.effective_crop();
        let options = RenderOptions {
            mode,
            format: self.render_format,
            quality: self.render_quality,
            keep_exif: self.render_keep_exif,
            xmp_sidecar: self.render_xmp_sidecar,
            metadata: self.meta.clone(),
            sharpen_for: self.render_sharpen_for,
            sharpen_strength: self.render_sharpen_strength,
            out_dir,
            video_out,
            crop,
            rotate: self.rotate,
            long_side: None,
            // 成片面板里的「4K / 1080p」是精确画幅：cover 放大后居中裁切，
            // 不再只限长边（裁切后的 1278×718 也要铺满 3840×2160）。
            target_size: if self.output_w == 0 || self.output_h == 0 {
                None
            } else {
                Some((self.output_w, self.output_h))
            },
            fps: self.output_fps.max(1.0) as f64,
            codec: if self.codec.contains("264") {
                Codec::H264
            } else {
                Codec::H265
            },
            crf: self.render_crf,
            preset: self.render_preset.clone(),
            audio: self.render_audio.clone(),
            audio_opts: self.render_audio_opts.clone(),
            keep_frames: self.render_keep_frames,
            resume: self.render_resume,
            color_space: self.render_color_space,
            watermark: self.render_watermark.clone(),
            // 运镜跟着这一次导出走：面板上定好的取景框，成片里就该照着走。
            motion: self.motion,
        };

        // 变速计划跟这一次导出绑定：线程起来之后就算面板上又动了倍速，
        // 跑的还是启动那一刻算出来的帧序，进度与产物都不会自相矛盾。
        let plan = match export::plan_for(&self.retime, n, self.output_fps.max(1.0) as f64) {
            Ok(plan) => plan,
            Err(e) => {
                self.status = format!("变速参数不对：{e}");
                return;
            }
        };

        self.cancel_export.store(false, Ordering::SeqCst);
        self.export_busy = true;
        self.export_stage = Some(Stage::Frames);
        self.export_done.store(0, Ordering::Relaxed);
        let out_frames = plan.as_ref().map_or(n, |p| p.frames());
        self.export_total.store(out_frames, Ordering::Relaxed);
        self.export_stage_shared.store(0, Ordering::Relaxed);
        // 「跳过 N」是数出来的，而数帧与帧数成正比——这份活不放界面线程，
        // 交给下面那条导出线程，结果写进原子量由状态栏的 100ms 心跳去读。
        self.export_skipped_shared.store(0, Ordering::Relaxed);
        self.status = if self.render_resume {
            "正在导出…（续跑：先数已导出的帧）".to_owned()
        } else {
            "正在导出…".to_owned()
        };

        let token = self.reload_token;
        let cancel = self.cancel_export.clone();
        let done = self.export_done.clone();
        let skipped = self.export_skipped_shared.clone();
        let total = self.export_total.clone();
        let stage = self.export_stage_shared.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            // 续跑先数一遍已经写完的帧，好把「跳过 N」显示出来：只报「已导出」
            // 看不出这一趟几乎没干活，续跑和重跑得区分得开。没开续跑时它是 0。
            skipped.store(
                export::existing_frames(&options, out_frames),
                Ordering::Relaxed,
            );
            let progress = move |p: JobProgress| {
                done.store(p.done, Ordering::Relaxed);
                total.store(p.total, Ordering::Relaxed);
                stage.store(
                    match p.stage {
                        Stage::Frames => 0,
                        Stage::Encode => 1,
                    },
                    Ordering::Relaxed,
                );
            };
            let result = (|| -> Result<PathBuf, String> {
                if options.mode == OutputMode::Video {
                    let ffmpeg = export::find_ffmpeg()
                        .ok_or_else(|| "找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned())?;
                    let video = export::export_video_planned(
                        &ffmpeg,
                        &paths,
                        &frame_params,
                        &gains,
                        &offsets,
                        &options,
                        plan.as_ref(),
                        &cancel,
                        &progress,
                    )
                    .map_err(|e| e.to_string())?;
                    if !options.keep_frames {
                        cleanup_frames(&options.out_dir, options.format);
                    }
                    Ok(video)
                } else {
                    export::export_stills_planned(
                        &paths,
                        &frame_params,
                        &gains,
                        &offsets,
                        &options,
                        plan.as_ref(),
                        &cancel,
                        &progress,
                    )
                    .map_err(|e| e.to_string())?;
                    Ok(options.out_dir.clone())
                }
            })();
            match result {
                Ok(path) => {
                    let _ = tx.send(Msg::Exported { token, path });
                }
                Err(e) => {
                    let _ = tx.send(Msg::ExportFailed(e));
                }
            }
            ctx2.request_repaint();
        });
    }

    pub fn stop_export(&mut self) {
        if self.export_busy {
            self.cancel_export.store(true, Ordering::SeqCst);
            self.status = "正在取消导出…".to_owned();
        }
    }

    pub fn export_progress(&self) -> Option<(usize, usize, Stage)> {
        if !self.export_busy {
            return None;
        }
        let stage = if self.export_stage_shared.load(Ordering::Relaxed) == 1 {
            Stage::Encode
        } else {
            Stage::Frames
        };
        Some((
            self.export_done.load(Ordering::Relaxed),
            self.export_total.load(Ordering::Relaxed),
            stage,
        ))
    }

    /// 这一趟续跑跳过了几帧：没开续跑、或者还没数完，都是 0。
    pub fn export_skipped(&self) -> usize {
        self.export_skipped_shared.load(Ordering::Relaxed)
    }

    /// 只有测试能摆这个数：真机上它只由导出线程写，界面这边没有别的入口。
    #[cfg(test)]
    pub(crate) fn set_export_skipped_for_test(&self, n: usize) {
        self.export_skipped_shared.store(n, Ordering::Relaxed);
    }

    pub fn align_progress(&self) -> Option<(usize, usize)> {
        if !self.align_busy {
            return None;
        }
        Some((self.align_done.load(Ordering::Relaxed), self.frame_count()))
    }

    // ------------------------------------------------------------ 渲染队列

    /// 启动时从漫游配置读队列；测试用 `AppState::new` 不会碰这份文件。
    pub fn load_queue(&mut self) {
        self.queue = queue::load(&self.queue_path);
    }

    fn save_queue(&mut self) {
        if let Err(e) = queue::save(&self.queue_path, &self.queue) {
            self.error = Some(format!("队列保存失败：{e}"));
        }
    }

    pub fn queue_json(&self) -> Value {
        let items: Vec<Value> = self
            .queue
            .iter()
            .enumerate()
            .map(|(index, item)| {
                json!({
                    "index": index + 1,
                    "root": item.root,
                    "out": item.out,
                    "width": item.width,
                    "height": item.height,
                    "fps": round6(item.fps),
                    "codec": item.codec,
                    "crf": item.crf,
                    "preset": item.preset,
                    "keep_frames": item.keep_frames,
                    "resume": item.resume,
                    "copy": item.copy,
                    "status": item.status.label(),
                    "note": item.note,
                })
            })
            .collect();
        json!({
            "path": self.queue_path.display().to_string(),
            "count": self.queue.len(),
            "items": items,
        })
    }

    pub fn queue_status_json(&self) -> Value {
        let done = self
            .queue
            .iter()
            .filter(|i| i.status == QueueStatus::Done)
            .count();
        let failed = self
            .queue
            .iter()
            .filter(|i| i.status == QueueStatus::Failed)
            .count();
        let progress = if self.queue_busy {
            Some(json!({
                "frame_done": self.queue_done.load(Ordering::Relaxed),
                "frame_total": self.queue_total.load(Ordering::Relaxed),
                "stage": if self.queue_stage.load(Ordering::Relaxed) == 1 { "encode" } else { "frames" },
            }))
        } else {
            None
        };
        json!({
            "busy": self.queue_busy,
            "current": self.queue_current.map(|i| i + 1),
            "items": self.queue.len(),
            "done": done,
            "failed": failed,
            "pending": self.queue.len() - done - failed,
            "progress": progress,
            "path": self.queue_path.display().to_string(),
        })
    }

    pub fn queue_progress(&self) -> Option<(usize, usize, Stage)> {
        if !self.queue_busy {
            return None;
        }
        let stage = if self.queue_stage.load(Ordering::Relaxed) == 1 {
            Stage::Encode
        } else {
            Stage::Frames
        };
        Some((
            self.queue_done.load(Ordering::Relaxed),
            self.queue_total.load(Ordering::Relaxed),
            stage,
        ))
    }

    /// 把当前序列与成片设置排进队列；同一目录重复加入等于更新规格。
    pub fn add_current_to_queue(&mut self) -> Result<usize, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消或等它跑完".to_owned());
        }
        let (root, name) = {
            let seq = self.require_sequence()?;
            (seq.root.clone(), seq.display_name())
        };
        let out = if self.render_video_out.as_os_str().is_empty() {
            root.join(format!("{name}_成片.mp4"))
        } else {
            self.render_video_out.clone()
        };
        let item = QueueItem {
            width: self.output_w,
            height: self.output_h,
            fps: self.output_fps as f64,
            codec: if self.codec.contains("264") {
                "h264"
            } else {
                "h265"
            }
            .to_owned(),
            crf: self.render_crf,
            preset: self.render_preset.clone(),
            keep_frames: self.render_keep_frames,
            resume: self.render_resume,
            ..QueueItem::new(&root, out)
        };
        let index = queue::add(&mut self.queue, item)?;
        self.save_queue();
        self.status = format!("已加入队列：第 {} 段", index + 1);
        Ok(index)
    }

    /// 开始逐段渲染；立即返回，进度看 `queue.status`。
    pub fn start_queue(&mut self, ctx: &Context) -> Result<usize, String> {
        if self.headless {
            return Err("当前是无界面模式，不能启动队列".to_owned());
        }
        if self.queue_busy {
            return Err("队列已在进行中；用 queue.status 查询进度".to_owned());
        }
        if self.export_busy {
            return Err("正在导出当前序列，等导出结束再跑队列".to_owned());
        }
        if self.queue.is_empty() {
            return Err("队列是空的：先把当前序列加入队列".to_owned());
        }
        let ffmpeg = export::find_ffmpeg()
            .ok_or_else(|| "找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned())?;
        let n = self.queue.len();
        for item in &mut self.queue {
            item.status = QueueStatus::Pending;
            item.note.clear();
        }
        self.save_queue();
        let items = self.queue.clone();

        self.queue_busy = true;
        self.queue_current = Some(0);
        self.cancel_queue.store(false, Ordering::SeqCst);
        self.queue_done.store(0, Ordering::Relaxed);
        self.queue_total.store(0, Ordering::Relaxed);
        self.queue_stage.store(0, Ordering::Relaxed);
        self.status = format!("队列渲染 0/{n}");

        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let cancel = self.cancel_queue.clone();
        let done = self.queue_done.clone();
        let total = self.queue_total.clone();
        let stage = self.queue_stage.clone();
        thread::spawn(move || {
            let (mut ok, mut failed) = (0usize, 0usize);
            let mut cancelled = false;
            let progress = |p: JobProgress| {
                done.store(p.done, Ordering::Relaxed);
                total.store(p.total, Ordering::Relaxed);
                stage.store(
                    match p.stage {
                        Stage::Frames => 0,
                        Stage::Encode => 1,
                    },
                    Ordering::Relaxed,
                );
            };
            for (index, item) in items.iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    break;
                }
                done.store(0, Ordering::Relaxed);
                total.store(0, Ordering::Relaxed);
                stage.store(0, Ordering::Relaxed);
                match queue::render_item(item, &ffmpeg, &cancel, &progress) {
                    Ok(path) => {
                        ok += 1;
                        let _ = tx.send(Msg::QueueRow {
                            index,
                            status: QueueStatus::Done,
                            note: path.display().to_string(),
                        });
                    }
                    Err(e) => {
                        if cancel.load(Ordering::SeqCst) {
                            cancelled = true;
                            let _ = tx.send(Msg::QueueRow {
                                index,
                                status: QueueStatus::Pending,
                                note: "已取消".to_owned(),
                            });
                            break;
                        }
                        failed += 1;
                        let _ = tx.send(Msg::QueueRow {
                            index,
                            status: QueueStatus::Failed,
                            note: e.to_string(),
                        });
                    }
                }
                ctx2.request_repaint();
            }
            let _ = tx.send(Msg::QueueFinished {
                ok,
                failed,
                cancelled,
            });
            ctx2.request_repaint();
        });
        Ok(n)
    }

    pub fn stop_queue(&mut self) {
        if self.queue_busy {
            self.cancel_queue.store(true, Ordering::SeqCst);
            self.status = "正在取消队列…".to_owned();
        }
    }

    /// 面板按钮用：按 0 基序号移除一段。
    pub fn remove_queue_at(&mut self, index: usize) -> bool {
        if self.queue_busy || index >= self.queue.len() {
            return false;
        }
        self.queue.remove(index);
        self.save_queue();
        true
    }

    pub fn clear_queue_rows(&mut self) -> usize {
        if self.queue_busy {
            return 0;
        }
        let cleared = self.queue.len();
        self.queue.clear();
        self.save_queue();
        cleared
    }

    fn cmd_queue_add(&mut self, args: &Value) -> Result<Value, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消或等它跑完".to_owned());
        }
        let root = match opt_str(args, "root")? {
            Some(text) => PathBuf::from(text),
            None => self.require_sequence()?.root.clone(),
        };
        let current = self.seq.as_ref().map(|s| s.root.clone());
        let default_out = match &current {
            Some(cur) if path_eq(&root, cur) && !self.render_video_out.as_os_str().is_empty() => {
                self.render_video_out.clone()
            }
            _ => {
                let name = root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "序列".to_owned());
                root.join(format!("{name}_成片.mp4"))
            }
        };
        let out = match opt_str(args, "out")? {
            Some(text) => PathBuf::from(text),
            None => default_out,
        };
        let width = opt_u32(args, "width")?.unwrap_or(self.output_w);
        let height = opt_u32(args, "height")?.unwrap_or(self.output_h);
        if (width == 0) != (height == 0) {
            return Err("width 与 height 需同时为 0（保持原生）或同时给出".to_owned());
        }
        if width > 0 && (width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1) {
            return Err(format!(
                "width/height 必须是偶数且不小于 2：{width}×{height}"
            ));
        }
        let fps = opt_f64(args, "fps")?.unwrap_or(self.output_fps as f64);
        if !(1.0..=240.0).contains(&fps) {
            return Err("fps 必须在 1..=240".to_owned());
        }
        let crf = opt_u32(args, "crf")?.unwrap_or(self.render_crf as u32);
        if crf > 51 {
            return Err("crf 必须在 0..51".to_owned());
        }
        let codec = match opt_str(args, "codec")? {
            Some(codec) => match codec.trim().to_ascii_lowercase().as_str() {
                "h264" | "264" | "avc" => "h264",
                "h265" | "265" | "hevc" => "h265",
                other => return Err(format!("未知编码 {other}；可用 h264 / h265")),
            },
            None if self.codec.contains("264") => "h264",
            None => "h265",
        };
        let preset = match opt_str(args, "preset")? {
            Some(preset) => {
                let preset = preset.trim();
                if !queue::PRESETS.contains(&preset) {
                    return Err(format!("未知编码预设 {preset}"));
                }
                preset.to_owned()
            }
            None => self.render_preset.clone(),
        };
        let keep_frames = match args.get("keep_frames") {
            None | Some(Value::Null) => self.render_keep_frames,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "keep_frames 必须是布尔值".to_owned())?,
        };
        let resume = match args.get("resume") {
            None | Some(Value::Null) => self.render_resume,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "resume 必须是布尔值".to_owned())?,
        };
        // 副本名只校验非空：该序列工程里有没有这套副本，渲染那一段时才晓得，
        // 队列允许先排进还没调完色的序列。
        let copy = match opt_str(args, "copy")? {
            Some(raw) => {
                let name = raw.trim().to_owned();
                if name.is_empty() {
                    return Err("copy 不能为空串；省略才表示用主调色".to_owned());
                }
                name
            }
            None => String::new(),
        };
        let item = QueueItem {
            root: root.display().to_string(),
            out: out.display().to_string(),
            width,
            height,
            fps,
            codec: codec.to_owned(),
            crf: crf as u8,
            preset,
            keep_frames,
            resume,
            copy,
            ..QueueItem::default()
        };
        let index = queue::add(&mut self.queue, item)?;
        self.save_queue();
        self.status = format!("已加入队列：第 {} 段", index + 1);
        let mut out = self.queue_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("added".to_owned(), json!(index + 1));
        }
        Ok(out)
    }

    fn cmd_queue_remove(&mut self, args: &Value) -> Result<Value, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消再改队列".to_owned());
        }
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少 index（从 1 开始）".to_owned())? as usize;
        if index == 0 || index > self.queue.len() {
            return Err(format!("index 超出范围：1..={}", self.queue.len()));
        }
        self.queue.remove(index - 1);
        self.save_queue();
        Ok(json!({"removed": index, "items": self.queue.len()}))
    }

    fn cmd_queue_clear(&mut self) -> Result<Value, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消再清空".to_owned());
        }
        let cleared = self.queue.len();
        self.queue.clear();
        self.save_queue();
        Ok(json!({"cleared": cleared, "items": 0}))
    }

    fn cmd_queue_run(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        if !confirm(args) {
            return Err("queue.run 会批量写视频，需参数 confirm: true".to_owned());
        }
        let n = self.start_queue(ctx)?;
        Ok(json!({
            "started": true,
            "items": n,
            "note": "长任务已启动；用 queue.status 查询进度",
        }))
    }

    pub fn project_snapshot(&self) -> ProjectFile {
        let root = self
            .seq
            .as_ref()
            .map(|s| s.root.display().to_string())
            .unwrap_or_default();
        let long_side = if self.output_w == 0 || self.output_h == 0 {
            None
        } else {
            Some(self.output_w.max(self.output_h))
        };
        let align = self.align_report.as_ref().map(|r| AlignState {
            reference: self.align_ref,
            estimator: match self.align_estimator {
                Estimator::Template => "template",
                Estimator::Phase => "phase",
            }
            .to_owned(),
            offsets: r.offsets.clone(),
            scores: r.scores.clone(),
            issues: r.issues.clone(),
            crop: Some(r.crop),
        });
        ProjectFile {
            version: project::VERSION,
            root,
            reference: self.align_ref,
            params: self.params.clone(),
            preset: self.preset.clone(),
            ratings: self.ratings.iter().map(|(k, v)| (*k, *v)).collect(),
            flags: self.flags.iter().map(|(k, v)| (*k, *v)).collect(),
            labels: self.labels.iter().map(|(k, v)| (*k, *v)).collect(),
            keywords: self.keywords.iter().map(|(k, v)| (*k, v.clone())).collect(),
            stacks: self.stacks.clone(),
            // 落盘的是归一化后的那一份：换行折成空格、超长截断，读回来才和写进去一致。
            metadata: self.meta.cleaned(),
            keyframes: self.keyframes.clone(),
            copies: self.copies.clone(),
            def_window: self.def_window,
            def_strength: self.def_strength,
            def_multi: self.def_multi,
            def_channels: self.def_channels,
            def_offsets: self.corr.clone(),
            def_chan: self.corr_rgb.clone(),
            crop: self.effective_crop(),
            user_crop: self.user_crop,
            rotate: self.rotate,
            aspect: self.aspect.label().to_owned(),
            align,
            render: RenderState {
                mode: match self.render_mode {
                    OutputMode::Stills => "stills",
                    OutputMode::Video => "video",
                }
                .to_owned(),
                format: self.render_format.key().to_owned(),
                quality: self.render_quality,
                keep_exif: self.render_keep_exif,
                xmp_sidecar: self.render_xmp_sidecar,
                sharpen_for: self.render_sharpen_for.key().to_owned(),
                sharpen_strength: self.render_sharpen_strength.key().to_owned(),
                width: self.output_w,
                height: self.output_h,
                fps: self.output_fps as f64,
                codec: if self.codec.contains("264") {
                    "h264"
                } else {
                    "h265"
                }
                .to_owned(),
                crf: self.render_crf,
                preset: self.render_preset.clone(),
                out_dir: self.render_out_dir.display().to_string(),
                video_out: self.render_video_out.display().to_string(),
                audio: self
                    .render_audio
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                audio_opts: self.render_audio_opts.clone(),
                long_side,
                keep_frames: self.render_keep_frames,
                color_space: self.render_color_space.key().to_owned(),
                watermark: self.render_watermark.clone(),
                retime: self.retime.clone(),
                motion: self.motion,
            },
            font_level: self.font_level,
        }
    }

    fn apply_project(&mut self, p: ProjectFile) {
        self.params = p.params;
        self.preset = p.preset;
        self.ratings = p.ratings.into_iter().collect();
        self.flags = p.flags.into_iter().collect();
        self.labels = p.labels.into_iter().collect();
        self.keywords = p.keywords.into_iter().collect();
        self.stacks = aevum_core::library::normalize_stacks(&p.stacks);
        self.meta = p.metadata;
        self.keyframes = p.keyframes;
        self.copies = p.copies;
        self.def_window = p.def_window;
        self.def_strength = p.def_strength;
        self.def_multi = p.def_multi;
        self.def_channels = p.def_channels;
        self.font_level = p.font_level.min(3);
        self.align_ref = p.reference;

        if let Some(a) = p.align {
            self.align_estimator = if a.estimator == "phase" {
                Estimator::Phase
            } else {
                Estimator::Template
            };
            self.align_ref = a.reference;
            if !a.offsets.is_empty() {
                let crop = a.crop.unwrap_or_else(|| {
                    let (w, h) = self.resolution.unwrap_or((0, 0));
                    CropRect { x: 0, y: 0, w, h }
                });
                let size = self.resolution.unwrap_or((crop.w, crop.h));
                self.align_report = Some(AlignReport {
                    size: [size.0, size.1],
                    offsets: a.offsets,
                    scores: a.scores,
                    issues: a.issues,
                    crop,
                    elapsed_secs: 0.0,
                });
            }
        }

        // 取景：比例、角度和用户框一起回，直接落值——重铺是 set_aspect 的事，这里不能再改形状。
        // `p.crop` 是给渲染队列用的最终取景，界面上要接着拖得用 `user_crop`。
        self.aspect = Aspect::from_label(&p.aspect);
        self.user_crop = p.user_crop;
        self.rotate = p.rotate;
        self.crop_drag = None;

        self.render_mode = if p.render.mode == "stills" {
            OutputMode::Stills
        } else {
            OutputMode::Video
        };
        // 认不出来的容器当 PNG8：旧工程只有那两种，手改错了也不能让面板显示空值。
        self.render_format = StillFormat::parse(&p.render.format).unwrap_or(StillFormat::Png8);
        self.render_quality = p.render.quality.clamp(1, 100);
        self.render_keep_exif = p.render.keep_exif;
        self.render_xmp_sidecar = p.render.xmp_sidecar;
        self.render_sharpen_for = OutputSharpen::parse(&p.render.sharpen_for).unwrap_or_default();
        self.render_sharpen_strength =
            SharpenStrength::parse(&p.render.sharpen_strength).unwrap_or_default();
        // 工程里的色域认不出来就当 sRGB：旧工程没这个字段，手改错了也不能让面板显示空值。
        self.render_color_space = ColorSpace::parse(&p.render.color_space).unwrap_or_default();
        // 水印是一整块：旧工程没这一段，读进来就是 `Watermark::default()`，也就是没开。
        self.render_watermark = p.render.watermark.clone();
        // 变速同理：旧工程读到的是关着的重映射，导出与以前逐字节一致。
        self.retime = p.render.retime.clone();
        // 运镜同理：旧工程没这一段，读回来就是关着的取景框。
        self.motion = p.render.motion;
        if p.render.width > 0 && p.render.height > 0 {
            self.output_w = p.render.width;
            self.output_h = p.render.height;
        }
        if p.render.fps.is_finite() && p.render.fps > 0.0 {
            self.output_fps = p.render.fps as f32;
        }
        self.codec = if p.render.codec == "h264" {
            "H.264".to_owned()
        } else {
            "HEVC".to_owned()
        };
        self.render_crf = p.render.crf;
        if !p.render.preset.is_empty() {
            self.render_preset = p.render.preset;
        }
        if !p.render.out_dir.is_empty() {
            self.render_out_dir = PathBuf::from(p.render.out_dir);
        }
        if !p.render.video_out.is_empty() {
            self.render_video_out = PathBuf::from(p.render.video_out);
        }
        self.render_audio = if p.render.audio.is_empty() {
            None
        } else {
            Some(PathBuf::from(p.render.audio))
        };
        self.render_audio_opts = p.render.audio_opts;
        self.audio_info = None;
        self.audio_probe_err = None;
        self.render_keep_frames = p.render.keep_frames;

        // 先把落盘的逐帧补偿放回去；有测光数据时下面会按当前参数重算覆盖。
        self.corr = p.def_offsets;
        self.corr_rgb = p.def_chan;
        if !self.metrics.is_empty() {
            self.recompute_deflicker();
        }
        self.preview_dirty = true;
    }

    fn write_project(&mut self, quiet: bool) {
        if self.headless {
            return;
        }
        let Some(seq) = &self.seq else { return };
        let path = project::project_path(&seq.root);
        let snap = self.project_snapshot();
        match project::save(&path, &snap) {
            Ok(()) => {
                self.saved_snap = Some(self.snapshot());
                self.saved_stamp = Some(self.project_stamp());
                if !quiet {
                    self.status = format!("工程已保存 {}", path.display());
                }
            }
            Err(e) => {
                if !quiet {
                    self.error = Some(format!("工程保存失败：{e}"));
                }
            }
        }
    }

    /// 当前状态是否和最后一次落盘的一致。`Snapshot` 是逐字段比较，几微秒的事，
    /// 比序列化加写盘便宜三个数量级。
    fn project_unchanged(&self) -> bool {
        self.saved_snap.as_ref() == Some(&self.snapshot())
            && self.saved_stamp.as_ref() == Some(&self.project_stamp())
    }

    fn project_stamp(&self) -> ProjectStamp {
        let report = self.align_report.as_ref();
        ProjectStamp {
            align_ref: self.align_ref,
            estimator: match self.align_estimator {
                Estimator::Template => "template",
                Estimator::Phase => "phase",
            },
            offsets: report.map_or(0, |r| r.offsets.len()),
            issues: report.map_or(0, |r| r.issues.len()),
            crop: self.effective_crop().map(|c| (c.x, c.y, c.w, c.h)),
        }
    }

    pub fn save_project(&mut self) {
        self.write_project(false);
        self.last_auto_save = Instant::now();
    }

    pub fn load_project(&mut self, root: &Path) {
        let path = project::project_path(root);
        if !path.is_file() {
            return;
        }
        match project::load(&path) {
            Ok(p) => self.apply_project(p),
            Err(e) => {
                self.error = Some(format!("读取工程失败：{e}"));
            }
        }
    }

    /// 打开序列时检测同目录 `.xmp` 边车：只记下路径给状态栏提示，
    /// **不做静默自动套用**——LR 用户对「打开即所见」有预期，但静默改参数
    /// 也会让「我从哪张图开始调的」说不清，应用与否交给一键按钮。
    fn detect_sidecar_hint(&mut self) {
        self.sidecar_hint = None;
        let Some(seq) = &self.seq else { return };
        for frame in seq.paths() {
            let sidecar = xmp::sidecar_for(frame.as_path());
            if sidecar.is_file() {
                self.sidecar_hint = Some(sidecar);
                return;
            }
        }
    }

    /// RAW 首次导入的默认参数：锐化 40 / 锐化保护 20 / 亮度降噪 25。
    ///
    /// 挂在 `open_paths` 的 `load_project` 之后、`history_reset` 之前：
    /// 「只一次」由工程文件保证——首次导入后工程落盘（含这些默认值），之后打开
    /// 读工程、条件不再成立；普通图片序列不含 RAW 帧，零影响。
    /// 默认值进初始历史，撤销一挡就能回到干净状态。
    ///
    /// 注意 `GradeParams::default()` 的锐化保护仍是 0（旧工程 JSON 缺字段也反序列化
    /// 成 0、渲染走旧路径）：20 只在这里给新建工程，不进引擎默认、不进预设。
    pub fn apply_raw_import_defaults(&mut self, root: &Path) {
        if project::project_path(root).is_file() {
            return;
        }
        let Some(seq) = &self.seq else { return };
        if !seq.paths().iter().any(aevum_core::raw::is_raw_name) {
            return;
        }
        self.params.sharp_amount = 40.0;
        self.params.sharp_protect = 20.0;
        self.params.nr_luma = 25.0;
        self.status = "RAW 序列已套用默认：锐化 40 · 锐化保护 20 · 亮度降噪 25".to_owned();
    }

    /// 状态栏「应用边车」的一键动作：把检测到的边车当预设导入（整体替换
    /// 全局参数，与 `xmp.import` 同口径），失败落 error、成功记历史。
    pub fn apply_sidecar_hint(&mut self) {
        let Some(path) = self.sidecar_hint.take() else {
            return;
        };
        match self.import_xmp_preset(&path) {
            Ok(_) => {
                self.status = format!(
                    "已应用 XMP 边车 {}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                );
                history::note("xmp-sidecar", format!("应用 XMP 边车 {}", path.display()));
            }
            Err(e) => {
                self.error = Some(e);
            }
        }
    }

    /// 单帧导出读的成片档旋钮：跟批量导出同一组，面板上选了什么这里就是什么。
    fn frame_render_options(&self) -> RenderOptions {
        RenderOptions {
            quality: self.render_quality,
            keep_exif: self.render_keep_exif,
            xmp_sidecar: self.render_xmp_sidecar,
            metadata: self.meta.clone(),
            sharpen_for: self.render_sharpen_for,
            sharpen_strength: self.render_sharpen_strength,
            color_space: self.render_color_space,
            watermark: self.render_watermark.clone(),
            ..RenderOptions::still(self.render_format)
        }
    }

    pub fn export_current(&mut self, ctx: &Context) {
        if self.export_busy {
            return;
        }
        let Some(seq) = &self.seq else { return };
        let Some(frame) = seq.frames.get(self.selected) else {
            return;
        };
        let stem = std::path::Path::new(&frame.name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "frame".to_owned());
        let format = self.render_format;
        let Some(picked) = rfd::FileDialog::new()
            .set_file_name(format!("{stem}_graded.{}", format.ext()))
            .add_filter(format.cn(), &[format.ext()])
            .save_file()
        else {
            return;
        };
        let path = force_container(picked, format);
        self.export_busy = true;
        self.status = format!("正在导出当前帧（{}）…", format.cn());
        let params = self.params.clone();
        let gain = self.ev_at(self.selected);
        // 导出按画幅比例裁：面板上的取景框就是最终画面，对齐裁切照旧先叠一层。
        let crop = self.effective_crop();
        let (_, offset) = self.alignment_at(self.selected);
        let framing = Framing {
            angle_deg: self.rotate,
            off: offset,
        };
        let full_size = self.resolution;
        let render = self.frame_render_options();
        let src = frame.path.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            match render_full_file(FrameExport {
                src: &src,
                dst: &path,
                params: &params,
                gain,
                full_size,
                crop,
                framing,
                render,
            }) {
                Ok(()) => {
                    let _ = tx.send(Msg::Saved(path.display().to_string()));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("导出失败：{e}")));
                }
            }
            ctx2.request_repaint();
        });
    }

    /// 测试与截图用的演示状态：不碰磁盘，序列与测光都是合成的。
    pub fn demo() -> Self {
        use aevum_core::sequence::Frame;
        let n = 48usize;
        let frames: Vec<Frame> = (0..n)
            .map(|i| Frame {
                index: i,
                name: format!("demo_{i:04}.jpg"),
                path: PathBuf::from(format!("demo_{i:04}.jpg")),
            })
            .collect();
        let mut s = Self::new();
        s.seq = Some(Sequence {
            root: PathBuf::from("演示序列"),
            frames,
        });
        s.metrics = (0..n)
            .map(|i| {
                let t = i as f64 / n as f64;
                let flicker = if i % 7 == 0 { -0.04 } else { 0.0 };
                let mean = 0.28 + 0.18 * t + flicker;
                Some(FrameMetrics {
                    mean,
                    median: mean,
                    center: mean * 1.05,
                    sharp: 120.0 + 10.0 * (i as f64).sin(),
                })
            })
            .collect();
        // 线性光测光也合成一份，形状跟着胶片条同调：每 7 帧一次整体掉档（曝光跳，
        // 三通道一起掉），每 5 帧一次偏绿（色闪，只有分通道看得见）。演示序列不带
        // 这两样，验收时「分通道」就是个死控件。
        s.def_means = (0..n)
            .map(|i| {
                let t = i as f64 / n as f64;
                let base = 0.045 + 0.03 * t;
                let scale = if i % 7 == 0 { 0.8 } else { 1.0 };
                let green = if i % 5 == 0 { 0.05 } else { 0.0 };
                let rgb = [
                    base * scale,
                    (base * 1.1 + green) * scale,
                    base * 1.3 * scale,
                ];
                let luma = deflicker::LUMA_RGB
                    .iter()
                    .zip(rgb)
                    .map(|(w, v)| w * v)
                    .sum::<f64>();
                Some(FrameMeans { luma, rgb })
            })
            .collect();
        s.issues = vec![(17, "解码失败：演示用缺帧".to_owned())];
        s.resolution = Some((4096, 2160));
        s.recompute_deflicker();
        s.ratings = (0..n)
            .filter_map(|i| match i % 11 {
                0 => Some((i, 5)),
                4 => Some((i, 3)),
                7 => Some((i, 4)),
                _ => None,
            })
            .collect();
        s.flags = (0..n)
            .filter_map(|i| match i % 9 {
                0 => Some((i, 1)),
                5 => Some((i, -1)),
                _ => None,
            })
            .collect();
        s.labels = (0..n)
            .filter(|i| i % 13 == 0)
            .map(|i| (i, 1 + (i / 13) as u8 % 4))
            .collect();
        s.status = "48 帧 · 演示序列（无磁盘读写）".to_owned();
        s.module = Module::Develop;
        s.selected = 12;
        s.apply_demo_edit();
        // 演示编辑不算用户改动：历史从这一份状态重新开始。
        s.history_reset("演示序列");
        s.headless = true;
        s
    }

    /// 截图用的一套演示编辑：一张径向局部蒙版，选中并展开面板。
    pub fn apply_demo_edit(&mut self) {
        let mut demo_mask = mask::new_mask(mask::ShapeKind::Radial);
        if let Some(comp) = demo_mask
            .get_mut("components")
            .and_then(Value::as_array_mut)
            .and_then(|list| list.first_mut())
            .and_then(Value::as_object_mut)
        {
            comp.insert("x1".to_owned(), json!(0.36));
            comp.insert("y1".to_owned(), json!(0.46));
            comp.insert("x2".to_owned(), json!(0.60));
            comp.insert("y2".to_owned(), json!(0.72));
            comp.insert("feather".to_owned(), json!(55));
        }
        demo_mask["amount"] = json!(85);
        demo_mask["invert"] = json!(false);
        demo_mask["params"] = json!({"exposure": 0.9, "vibrance": 20});
        // 再挖一个线性形状进去，演示与截图都能直接看到布尔和亮度范围。
        if let Some(list) = demo_mask["components"].as_array_mut() {
            let mut cut = mask::new_component(mask::ShapeKind::Linear);
            cut["blend"] = json!("subtract");
            cut["x1"] = json!(0.08);
            cut["y1"] = json!(0.70);
            cut["x2"] = json!(0.42);
            cut["y2"] = json!(0.86);
            cut["feather"] = json!(28);
            list.push(cut);
        }
        demo_mask["range"] = json!({
            "lum_on": true, "lum_min": 0.12, "lum_max": 0.62, "lum_feather": 0.08
        });
        self.params.masks.push(demo_mask);
        self.mask_sel = Some(self.params.masks.len() - 1);
        self.preset = "自定义".to_owned();
        self.collapsed.insert("basic");
        self.collapsed.insert("color");
        self.collapsed.insert("effects");
        self.collapsed.remove("masks");
        self.preview_dirty = true;
    }

    /// 给无显示环境铺一套合成纹理，让画布与胶片条的绘制路径也被走到。
    pub fn install_demo_textures(&mut self, ctx: &Context) {
        let (w, h) = (960usize, 540usize);
        let mut pixels = Vec::with_capacity(w * h);
        let mut hist = [[0u32; 256]; 3];
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / w as f32;
                let v = y as f32 / h as f32;
                let horizon = 0.62;
                let k = (v / horizon).clamp(0.0, 1.0);
                let k = k * k * (3.0 - 2.0 * k);
                let r = (22.0 + (240.0 - 22.0) * k) * (1.0 - v * 0.35);
                let g = (30.0 + (150.0 - 30.0) * k) * (1.0 - v * 0.35);
                let b = (60.0 + (92.0 - 60.0) * k) * (1.0 - v * 0.15);
                let mut c = [
                    (r + t * 6.0).clamp(0.0, 255.0) as u8,
                    (g + t * 4.0).clamp(0.0, 255.0) as u8,
                    b.clamp(0.0, 255.0) as u8,
                ];
                if v > 0.72 + (t * 9.0).sin() * 0.02 {
                    c = [12, 14, 18];
                }
                for (ch, value) in c.iter().enumerate() {
                    hist[ch][*value as usize] += 1;
                }
                pixels.push(Color32::from_rgb(c[0], c[1], c[2]));
            }
        }
        self.hist = Some(hist);
        self.texture = Some(ctx.load_texture(
            "demo.preview",
            ColorImage::new([w, h], pixels),
            TextureOptions::LINEAR,
        ));
        self.texture_orig = self.texture.clone();
        self.preview_size = [w as u32, h as u32];
        // 合成直方图也是一次「预览落地」：直方图 Mesh 缓存换版。
        self.hist_version += 1;
        let n = self.frame_count();
        for i in 0..n {
            let shade = 30u8 + (i as u8).wrapping_mul(3);
            let img = ColorImage::filled([128, 72], Color32::from_rgb(shade, shade / 2, 40));
            let tex = ctx.load_texture(format!("demo.thumb.{i}"), img, TextureOptions::LINEAR);
            self.thumbs.insert(
                i,
                Thumb {
                    texture: tex,
                    size: [128, 72],
                },
            );
        }
        self.headless = false;
        self.preview_dirty = false;
    }
}

/// 胶片条专用线程池：拿一半核心，其余留给预览渲染。
///
/// 缩略图是「一次一拨」的突发负载（滚动一下就要 24 张），用全局池会和正在拖动的
/// 预览抢核，把 50ms 的拖动帧拖成 90ms。独立池只影响胶片条自己的速度。
fn thumb_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(4);
        rayon::ThreadPoolBuilder::new()
            .num_threads((cores / 2).max(2))
            .thread_name(|i| format!("aevum-thumb-{i}"))
            .build()
            .expect("缩略图线程池")
    })
}

/// 源文件指纹：路径之外还认修改时间与字节数，帧被外部重写过就不会拿到旧像素。
type SourceStamp = (Option<std::time::SystemTime>, u64);

fn source_stamp(path: &std::path::Path) -> SourceStamp {
    std::fs::metadata(path)
        .map(|m| (m.modified().ok(), m.len()))
        .unwrap_or((None, 0))
}

/// 画幅比例锁定的取景框：在 `base`（对齐裁切，缺省整帧）里居中内接。
///
/// 画布上的取景框一直按这个比例画，导出与队列过去却按整帧走——右栏写着「画幅决定
/// 导出裁切」，实际导出的边一条没少。现在两处共用这一份几何。
fn fit_aspect(
    base: Option<CropRect>,
    resolution: Option<(u32, u32)>,
    aspect: Aspect,
) -> Option<CropRect> {
    let ratio = aspect.ratio()? as f64;
    let (rw, rh) = resolution?;
    let full = base.unwrap_or(CropRect {
        x: 0,
        y: 0,
        w: rw,
        h: rh,
    });
    if full.w < 2 || full.h < 2 {
        return base;
    }
    let (w, h) = (full.w as f64, full.h as f64);
    let (cw, ch) = if w / h > ratio {
        (((h * ratio).round() as u32).max(1), full.h)
    } else {
        (full.w, ((w / ratio).round() as u32).max(1))
    };
    let cw = cw.min(full.w);
    let ch = ch.min(full.h);
    let x = full.x + (full.w - cw) / 2;
    let y = full.y + (full.h - ch) / 2;
    // 最后过一遍 norm_rect：按比例换算出来的边常常是奇数（3:2 源内接 16:9 就是 3375），
    // 而 4:2:0 的裁切窗口必须成对，否则 ffmpeg 静悄悄挪掉一像素，导出的边比画布上看到的多一条。
    Some(crop::norm_rect(
        f64::from(x),
        f64::from(y),
        f64::from(x + cw),
        f64::from(y + ch),
        rw,
        rh,
    ))
}

/// 预览源：像素、自身尺寸，以及全量预览源的尺寸（草图贴图减半时靠它报逻辑尺寸）。
type PreviewSourceView = (Vec<f32>, usize, usize, (usize, usize));

// 参数都是这条链的事实输入，打包成结构体只是把同样的字段换个地方摆。
#[allow(clippy::too_many_arguments)]
fn render_preview_file(
    path: &std::path::Path,
    params: &GradeParams,
    gain: Ev,
    full_size: Option<(u32, u32)>,
    crop: Option<CropRect>,
    framing: Framing,
    view: Option<Viewfinder>,
    sketch: bool,
    want_original: bool,
) -> Result<PreviewParts, String> {
    let stamp = source_stamp(path);
    let (mut buf, w, h, full) = decoded_preview_source(path, stamp, sketch, &params.dcp_profile)?;
    let (fw, fh) = full;
    let scale = match full_size {
        Some((img_w, _)) if img_w > 0 => fw as f64 / img_w as f64,
        _ => 1.0,
    };
    // 取景按全分辨率预览算；草图贴图只有一半大，画布那边仍按这个逻辑尺寸铺满。
    let crop = crop.and_then(|c| scale_crop(c, scale, fw, fh));
    let mut framing = framing;
    framing.off = [framing.off[0] * scale, framing.off[1] * scale];
    let logical = output_size(fw, fh, crop);
    // 分屏关着时这张图既不显示也不上传，没必要每帧取景再拷一份。
    let original = if !want_original {
        None
    } else if let Some(hit) = cached_original(path, stamp, crop, framing, &params.dcp_profile) {
        Some(hit)
    } else if !sketch && params.dcp_profile.is_empty() {
        Some(cropped_original(path, stamp, crop, framing, &buf, w, h, ""))
    } else {
        // 分屏的「原图」要和调色图走同一个相机配置：DCP 之前的像素是相机线性
        // RGB，拿它当对照只会显得调色前又暗又绿，不是用户想要的「原片」。
        let (src, sw, sh, _) = decoded_preview_source(path, stamp, false, &params.dcp_profile)?;
        Some(cropped_original(
            path,
            stamp,
            crop,
            framing,
            &src,
            sw,
            sh,
            &params.dcp_profile,
        ))
    };
    let (crop, framing) = if sketch {
        (
            crop.and_then(|c| scale_crop(c, 0.5, w, h)),
            Framing {
                angle_deg: framing.angle_deg,
                off: [framing.off[0] * 0.5, framing.off[1] * 0.5],
            },
        )
    } else {
        (crop, framing)
    };

    if sketch {
        aevum_core::render_sketch(&mut buf, w, h, params, gain);
    } else {
        aevum_core::render_preview(&mut buf, w, h, params, gain);
    }
    let (graded, gw, gh) = shift_crop(&buf, w, h, framing, crop);
    // 取景框作用在「拉直 + 裁切之后」的那一幅上，走的是导出同一条采样路
    // （`warp::view_into`）：预览推近多少，导出的画面就推近多少。
    let graded = match view {
        Some(v) => {
            let win = v.view_window(gw as u32, gh as u32, gw as u32, gh as u32);
            let mut moved = Vec::new();
            warp::view_into(&graded, gw, gh, win, gw, gh, &mut moved);
            moved
        }
        None => graded,
    };
    let (graded, hist) = color_image_with_hist(&graded, gw, gh);
    // 分屏的原图要跟着同一个框走：不然左边整幅、右边推近，对照比的是构图不是调色。
    let original = match (original, view) {
        (Some(img), Some(v)) => Some(view_color_image(img, Some(v))),
        (img, _) => img,
    };
    Ok((graded, original, [logical.0 as u32, logical.1 as u32], hist))
}

/// 解码一张预览尺寸的源图，带单槽缓存。
///
/// 拖一次推子能连发十几帧渲染，而源图在拖动期间一个字都不会变：6000×4000 的原片
/// 解码加缩放到 1600 边要 400ms 上下，比整条调色链还贵，每帧重算是卡顿的第一来源。
/// 只留一槽是刻意的——换帧时旧帧立刻让位，常驻内存封顶一张预览图。
///
/// `half` 要的是减半副本（拖动草图的源）：第一次用的时候从全量源图 2×2 均值降下来，
/// 之后也挂在同一槽里复用。
fn decoded_preview_source(
    path: &std::path::Path,
    stamp: SourceStamp,
    half: bool,
    profile: &str,
) -> Result<PreviewSourceView, String> {
    let mut slot = source_slot(path, stamp, profile)?;
    let src = slot.as_mut().expect("source_slot 填过才有返回值");
    let full = (src.w, src.h);
    if !half {
        return Ok((src.rgb.clone(), src.w, src.h, full));
    }
    if src.half.is_none() {
        src.half = Some(downscale_half(&src.rgb, src.w, src.h));
    }
    let h = src.half.as_ref().expect("刚算过");
    Ok((h.rgb.clone(), h.w, h.h, full))
}

/// 缓存槽：没有、换了帧、换了相机配置或换了去马赛克方法就重新解码，
/// 然后把锁交给调用方。
fn source_slot(
    path: &std::path::Path,
    stamp: SourceStamp,
    profile: &str,
) -> Result<std::sync::MutexGuard<'static, Option<PreviewSource>>, String> {
    let mut slot = PREVIEW_SRC
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 去马赛克方法是解码侧配置：诊断面板切换后旧缓存必须作废重解。
    let demosaic = aevum_core::raw::demosaic_method();
    let fresh = !matches!(
        slot.as_ref(),
        Some(c) if c.path == path
            && c.stamp == stamp
            && c.side == PREVIEW_SIDE
            && c.profile == profile
            && c.demosaic == demosaic
    );
    if fresh {
        let (buf, w, h) = metrics::decode_profile_preview(path, profile, PREVIEW_SIDE)
            .map_err(|e| e.to_string())?;
        if w == 0 || h == 0 {
            return Err("解码结果为空".to_owned());
        }
        *slot = Some(PreviewSource {
            path: path.to_path_buf(),
            stamp,
            side: PREVIEW_SIDE,
            profile: profile.to_owned(),
            demosaic,
            w,
            h,
            rgb: buf,
            half: None,
        });
    }
    Ok(slot)
}

/// 2×2 均值减半。宽高为奇数时最后一列/行并到边缘，尺寸取 `floor(n/2)`。
fn downscale_half(src: &[f32], w: usize, h: usize) -> HalfSource {
    let hw = (w / 2).max(1);
    let hh = (h / 2).max(1);
    let mut rgb = vec![0.0f32; hw * hh * 3];
    rgb.par_chunks_mut(hw * 3).enumerate().for_each(|(y, row)| {
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
    HalfSource { w: hw, h: hh, rgb }
}

struct PreviewSource {
    path: PathBuf,
    stamp: SourceStamp,
    side: u32,
    /// 解码时套的相机配置路径；换配置必须重新解码。
    profile: String,
    /// 解码时的去马赛克方法；诊断面板切换后必须重新解码。
    demosaic: &'static str,
    w: usize,
    h: usize,
    /// 归一化到 `[0,1]` 的 sRGB 像素；调用方拿走一份副本再调色。
    rgb: Vec<f32>,
    /// 拖动草图用的减半副本，第一次要的时候才算。
    half: Option<HalfSource>,
}

struct HalfSource {
    w: usize,
    h: usize,
    rgb: Vec<f32>,
}

/// 预览源图缓存。渲染都排在一个在途槽上，同一时刻只有一张图在算，一槽就够。
static PREVIEW_SRC: Mutex<Option<PreviewSource>> = Mutex::new(None);

/// 「原图」那一路的取景结果：只跟源文件和取景几何有关，参数怎么拖都不变。
/// 取景、转换再拷一份，拖动期间每帧约 9ms，是草图整帧的三分之一，所以也缓存一份。
#[allow(clippy::too_many_arguments)]
fn cropped_original(
    path: &std::path::Path,
    stamp: SourceStamp,
    crop: Option<CropRect>,
    framing: Framing,
    src: &[f32],
    w: usize,
    h: usize,
    profile: &str,
) -> ColorImage {
    if let Some(hit) = cached_original(path, stamp, crop, framing, profile) {
        return hit;
    }
    let (orig, ow, oh) = shift_crop(src, w, h, framing, crop);
    let image = color_image_from_f32(&orig, ow, oh);
    let mut slot = PREVIEW_ORIG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(OriginalView {
        path: path.to_path_buf(),
        stamp,
        crop,
        framing,
        profile: profile.to_owned(),
        demosaic: aevum_core::raw::demosaic_method(),
        image: image.clone(),
    });
    image
}

fn cached_original(
    path: &std::path::Path,
    stamp: SourceStamp,
    crop: Option<CropRect>,
    framing: Framing,
    profile: &str,
) -> Option<ColorImage> {
    let slot = PREVIEW_ORIG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // 「原图」的像素来自源解码：去马赛克方法换了，旧缓存同样作废。
    let demosaic = aevum_core::raw::demosaic_method();
    slot.as_ref()
        .filter(|c| {
            c.path == path
                && c.stamp == stamp
                && c.crop == crop
                && c.framing == framing
                && c.profile == profile
                && c.demosaic == demosaic
        })
        .map(|c| c.image.clone())
}

struct OriginalView {
    path: PathBuf,
    stamp: SourceStamp,
    crop: Option<CropRect>,
    framing: Framing,
    profile: String,
    /// 生成这份取景时的去马赛克方法；切换后缓存作废。
    demosaic: &'static str,
    image: ColorImage,
}

static PREVIEW_ORIG: Mutex<Option<OriginalView>> = Mutex::new(None);

/// 单帧导出的入参。位置参数已经快到 clippy 的 7 个上限，而且批量导出那套成片档
/// 旋钮也要一起捎上，所以打包成结构体，别再往函数签名后面加逗号。
struct FrameExport<'a> {
    src: &'a Path,
    dst: &'a Path,
    params: &'a GradeParams,
    gain: Ev,
    full_size: Option<(u32, u32)>,
    crop: Option<CropRect>,
    framing: Framing,
    render: RenderOptions,
}

fn render_full_file(job: FrameExport<'_>) -> Result<(), String> {
    let FrameExport {
        src,
        dst,
        params,
        gain,
        full_size,
        crop,
        framing,
        render,
    } = job;
    let (mut buf, w, h, deep) =
        metrics::decode_for_grade(src, params).map_err(|e| e.to_string())?;
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return Err("解码结果为空".to_owned());
    }
    let scale = match full_size {
        Some((fw, _)) if fw > 0 => w as f64 / fw as f64,
        _ => 1.0,
    };
    let crop = crop.and_then(|c| scale_crop(c, scale, w, h));
    let framing = Framing {
        angle_deg: framing.angle_deg,
        off: [framing.off[0] * scale, framing.off[1] * scale],
    };

    aevum_core::render_preview(&mut buf, w, h, params, gain);
    let (mut out_buf, ow, oh) = shift_crop(&buf, w, h, framing, crop);
    // 尾部动作交给 core：色域搬运 + ICC + 输出锐化 + EXIF + 容器编码，
    // 和批量导出共用同一条路，两条不许各调一刀。`deep` 是单帧有意保留的差别：
    // 高位深源选 PNG 仍写 16bit，别让最后一步把精度砍回 256 级。
    aevum_core::export::write_single_still(src, dst, &mut out_buf, ow, oh, deep, &render)
        .map_err(|e| e.to_string())?;
    // 旁车是静帧旁边的另一个文件，不在容器编码那条尾巴里：单帧这一路自己调一次。
    aevum_core::export::write_sidecar(dst, params, &render)
        .map_err(|e| e.to_string())
        .map(|_| ())
}

/// 对话框里的文件名和面板选的容器对齐：编码器看的是字节不是后缀，
/// 用户手输 `.jpg` 而面板开着 PNG 时，落盘的仍是 PNG，后缀不能骗人。
fn force_container(path: PathBuf, format: StillFormat) -> PathBuf {
    let wanted = format.ext();
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(wanted))
    {
        return path;
    }
    let mut renamed = path;
    renamed.set_extension(wanted);
    renamed
}

fn scale_crop(crop: CropRect, scale: f64, w: usize, h: usize) -> Option<CropRect> {
    let x = (crop.x as f64 * scale).round().clamp(0.0, w as f64) as u32;
    let y = (crop.y as f64 * scale).round().clamp(0.0, h as f64) as u32;
    let cw = (crop.w as f64 * scale).round().max(0.0) as u32;
    let ch = (crop.h as f64 * scale).round().max(0.0) as u32;
    let cw = cw.min(w as u32 - x);
    let ch = ch.min(h as u32 - y);
    if cw == 0 || ch == 0 {
        None
    } else {
        Some(CropRect { x, y, w: cw, h: ch })
    }
}

/// 预览的取景几何：拉直 + 对齐位移 + 裁切，和导出共用 `warp` 那一趟采样。
///
/// 以前预览自己有一套 `shift_crop`，边界按 replicate、逐像素串行，跟导出的
/// reflect101 对不上——分屏看着没问题，导出一比就多一圈边。现在两边同函数同口径。
fn shift_crop(
    src: &[f32],
    w: usize,
    h: usize,
    framing: Framing,
    crop: Option<CropRect>,
) -> (Vec<f32>, usize, usize) {
    let rect = crop.map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize));
    let mut out = Vec::new();
    let (cw, ch) = warp::transform_into(src, w, h, framing, rect, &mut out);
    (out, cw, ch)
}

/// 把取景框叠到一张已经取景好的 u8 图上（分屏「原图」那一侧）。
///
/// 原图缓存里存的是「拉直 + 裁切」的结果，框在取回之后再叠：缓存键不用带运镜，
/// 拖一下取景框也不会把整张原图重算一遍。
fn view_color_image(img: ColorImage, view: Option<Viewfinder>) -> ColorImage {
    let Some(v) = view else { return img };
    let [w, h] = img.size;
    let win = v.view_window(w as u32, h as u32, w as u32, h as u32);
    let mut src = Vec::with_capacity(w * h * 3);
    for px in &img.pixels {
        src.extend_from_slice(&[
            px.r() as f32 / 255.0,
            px.g() as f32 / 255.0,
            px.b() as f32 / 255.0,
        ]);
    }
    let mut out = Vec::new();
    warp::view_into(&src, w, h, win, w, h, &mut out);
    if out.is_empty() {
        return img;
    }
    color_image_from_f32(&out, w, h)
}

/// 裁切窗口，按源图尺寸夹一遍。
fn crop_window(w: usize, h: usize, crop: Option<CropRect>) -> (usize, usize, usize, usize) {
    let (cx, cy, cw, ch) = crop
        .map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize))
        .unwrap_or((0, 0, w, h));
    (
        cx,
        cy,
        cw.min(w.saturating_sub(cx)),
        ch.min(h.saturating_sub(cy)),
    )
}

/// 取景后的逻辑尺寸。草图贴图减半时，报给画布的仍是这个尺寸。
fn output_size(w: usize, h: usize, crop: Option<CropRect>) -> (usize, usize) {
    let (_, _, cw, ch) = crop_window(w, h, crop);
    (cw, ch)
}

fn color_image_from_f32(buf: &[f32], w: usize, h: usize) -> ColorImage {
    let mut pixels = Vec::with_capacity(w * h);
    for c in buf.chunks_exact(3) {
        pixels.push(Color32::from_rgb(
            (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        ));
    }
    ColorImage::new([w, h], pixels)
}

/// f32 像素 → 纹理像素 + 三通道直方图，一次搞定。
///
/// 这是每帧都要走的一段（拖动时也是），1067×1600 串行要 10~15ms，占一帧的五分之一。
/// 两件事都按块并行：像素互不相干，直方图用每线程一份的局部表再合并，计数精确不变。
fn color_image_with_hist(buf: &[f32], w: usize, h: usize) -> (ColorImage, [[u32; 256]; 3]) {
    let pixels: Vec<Color32> = buf
        .par_chunks_exact(3)
        .map(|c| {
            Color32::from_rgb(
                (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            )
        })
        .collect();
    let empty = || [[0u32; 256]; 3];
    let hist = pixels
        .par_chunks(8192)
        .fold(empty, |mut acc, chunk| {
            for p in chunk {
                acc[0][p.r() as usize] += 1;
                acc[1][p.g() as usize] += 1;
                acc[2][p.b() as usize] += 1;
            }
            acc
        })
        .reduce(empty, |mut a, b| {
            for ch in 0..3 {
                for i in 0..256 {
                    a[ch][i] += b[ch][i];
                }
            }
            a
        });
    (ColorImage::new([w, h], pixels), hist)
}

/// 只清理导出链自己写的 `frame_*`，不碰目录里的其它文件。
fn cleanup_frames(dir: &Path, format: StillFormat) {
    let ext = format.ext();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some(ext) {
            continue;
        }
        let ours = path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|n| n.starts_with("frame_"))
            .unwrap_or(false);
        if ours {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ---------------------------------------------------------------- 命令参数与路径

/// 取景框的 JSON 口径：`sequence.info`、`crop.set`、对齐报告里的 crop 都用这一份。
fn rect_json(r: CropRect) -> Value {
    json!({"x": r.x, "y": r.y, "width": r.w, "height": r.h})
}

/// 命令里写画幅比例：中英文都收，认不出来直接报错，不静默退回「原生」。
fn aspect_arg(text: &str) -> Result<Aspect, String> {
    let t = text.trim();
    Ok(match t {
        "原生" | "native" | "free" | "0" => Aspect::Native,
        "16:9" | "wide" | "1" => Aspect::Wide,
        "4:3" | "standard" | "2" => Aspect::FourThree,
        "1:1" | "square" | "3" => Aspect::Square,
        "9:16" | "vertical" | "4" => Aspect::Vertical,
        other => {
            return Err(format!(
                "画幅比例不认：{other}（可用：{}）",
                Aspect::ALL
                    .iter()
                    .map(|a| a.label())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
        }
    })
}

fn estimator_name(estimator: Estimator) -> &'static str {
    match estimator {
        Estimator::Template => "template",
        Estimator::Phase => "phase",
    }
}

fn mode_name(mode: OutputMode) -> &'static str {
    match mode {
        OutputMode::Stills => "stills",
        OutputMode::Video => "video",
    }
}

fn round6(value: f64) -> f64 {
    (value * 1e6).round() / 1e6
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn clean_copy_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("副本名不能为空".to_owned());
    }
    if name.chars().count() > 40 {
        return Err("副本名至多 40 个字符".to_owned());
    }
    Ok(name.to_owned())
}

fn confirm(args: &Value) -> bool {
    args.get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 命令通道改蒙版几何：坐标是 0~1 归一化（允许少量越界，渲染时裁掉）。
fn apply_mask_geometry(
    raw: &mut Value,
    component: usize,
    geometry: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let grouped = raw
        .as_object()
        .is_some_and(|o| o.contains_key("components"));
    let count = if grouped {
        raw.get("components")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0)
    } else {
        1
    };
    if component >= count {
        return Err(format!(
            "component 超出范围：0..{}",
            count.saturating_sub(1)
        ));
    }
    let target: &mut Value = if grouped {
        raw.as_object_mut()
            .and_then(|o| o.get_mut("components"))
            .and_then(Value::as_array_mut)
            .and_then(|list| list.get_mut(component))
            .ok_or_else(|| "蒙版 components 格式不对".to_owned())?
    } else {
        &mut *raw
    };
    let object = target
        .as_object_mut()
        .ok_or_else(|| "蒙版组件不是对象".to_owned())?;
    for (key, value) in geometry {
        match key.as_str() {
            "kind" | "shape" => {
                let kind = value
                    .as_str()
                    .map(str::trim)
                    .map(str::to_ascii_lowercase)
                    .ok_or_else(|| "kind 必须是 linear / radial / brush".to_owned())?;
                if kind != "linear" && kind != "radial" && kind != "brush" {
                    return Err(format!("未知蒙版形状 {kind}；可用 linear / radial / brush"));
                }
                object.insert("kind".to_owned(), json!(kind));
                if kind == "brush" {
                    object
                        .entry("strokes".to_owned())
                        .or_insert_with(|| json!([]));
                }
            }
            "blend" => {
                let blend = value
                    .as_str()
                    .map(str::trim)
                    .map(str::to_ascii_lowercase)
                    .ok_or_else(|| "blend 必须是 add / subtract / intersect".to_owned())?;
                if !matches!(blend.as_str(), "add" | "subtract" | "intersect") {
                    return Err(format!(
                        "未知叠加方式 {blend}；可用 add / subtract / intersect"
                    ));
                }
                object.insert("blend".to_owned(), json!(blend));
            }
            "invert" => {
                let invert = value
                    .as_bool()
                    .ok_or_else(|| "geometry.invert 必须是布尔值".to_owned())?;
                object.insert("invert".to_owned(), json!(invert));
            }
            "x1" | "y1" | "x2" | "y2" => {
                let number = geometry_number(key, value)?;
                object.insert(key.clone(), json!(number.clamp(-0.5, 1.5)));
            }
            "feather" => {
                let number = geometry_number(key, value)?;
                object.insert(key.clone(), json!(number.clamp(0.0, 100.0)));
            }
            "midpoint" => {
                let number = geometry_number(key, value)?;
                object.insert(key.clone(), json!(number.clamp(0.0, 100.0)));
            }
            "angle" => {
                let number = geometry_number(key, value)?;
                object.insert(key.clone(), json!(number.rem_euclid(360.0)));
            }
            // 画笔笔画整组替换：命令面没有「接着上一笔涂」的会话概念，一次给全。
            "strokes" => {
                let list = value
                    .as_array()
                    .ok_or_else(|| "geometry.strokes 必须是数组（每项一条笔画）".to_owned())?;
                if list.len() > mask::MAX_STROKES {
                    return Err(format!("strokes 最多 {} 条笔画", mask::MAX_STROKES));
                }
                let mut out = Vec::with_capacity(list.len());
                for (i, s) in list.iter().enumerate() {
                    let obj = s
                        .as_object()
                        .ok_or_else(|| format!("第 {} 条笔画必须是对象", i + 1))?;
                    for key in obj.keys() {
                        if !matches!(
                            key.as_str(),
                            "points" | "radius" | "hardness" | "flow" | "erase"
                        ) {
                            return Err(format!("未知笔画字段 {key}"));
                        }
                    }
                    let points = obj
                        .get("points")
                        .ok_or_else(|| format!("第 {} 条笔画缺 points", i + 1))?
                        .as_array()
                        .ok_or_else(|| format!("第 {} 条笔画的 points 必须是数组", i + 1))?;
                    if points.len() > mask::MAX_STROKE_POINTS {
                        return Err(format!("一条笔画最多 {} 个点", mask::MAX_STROKE_POINTS));
                    }
                    let mut pts = Vec::with_capacity(points.len());
                    for p in points {
                        let pair = p
                            .as_array()
                            .filter(|a| a.len() == 2)
                            .ok_or_else(|| "笔画点必须是 [x, y]".to_owned())?;
                        let x = pair[0]
                            .as_f64()
                            .filter(|v| v.is_finite())
                            .ok_or_else(|| "笔画点必须是 [x, y]（0~1 归一化）".to_owned())?;
                        let y = pair[1]
                            .as_f64()
                            .filter(|v| v.is_finite())
                            .ok_or_else(|| "笔画点必须是 [x, y]（0~1 归一化）".to_owned())?;
                        pts.push(json!([x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)]));
                    }
                    if pts.is_empty() {
                        return Err(format!("第 {} 条笔画至少要一个点", i + 1));
                    }
                    let mut stroke = serde_json::Map::new();
                    stroke.insert("points".to_owned(), Value::Array(pts));
                    if let Some(r) = obj.get("radius") {
                        stroke.insert(
                            "radius".to_owned(),
                            json!(geometry_number("radius", r)?.clamp(0.002, 0.5)),
                        );
                    }
                    if let Some(h) = obj.get("hardness") {
                        stroke.insert(
                            "hardness".to_owned(),
                            json!(geometry_number("hardness", h)?.clamp(0.0, 1.0)),
                        );
                    }
                    if let Some(f) = obj.get("flow") {
                        stroke.insert(
                            "flow".to_owned(),
                            json!(geometry_number("flow", f)?.clamp(0.01, 1.0)),
                        );
                    }
                    if let Some(e) = obj.get("erase") {
                        let erase = e.as_bool().ok_or_else(|| "erase 必须是布尔值".to_owned())?;
                        stroke.insert("erase".to_owned(), json!(erase));
                    }
                    out.push(Value::Object(stroke));
                }
                object.insert("strokes".to_owned(), Value::Array(out));
            }
            other => return Err(format!("未知几何字段 {other}")),
        }
    }
    Ok(())
}

fn geometry_number(key: &str, value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("geometry.{key} 必须是有限数值"))
}

/// 组级亮度范围的存盘形状：0~1 的区间 + 开关。
pub fn lum_range_json(range: &aevum_core::mask::LumRange) -> Value {
    json!({
        "lum_on": range.on,
        "lum_min": range.min,
        "lum_max": range.max,
        "lum_feather": range.feather,
    })
}

fn range_stored(target: &serde_json::Map<String, Value>, key: &str, default: f64) -> f64 {
    target
        .get(key)
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

fn range_on(target: &serde_json::Map<String, Value>) -> bool {
    target
        .get("lum_on")
        .and_then(|v| v.as_bool().or_else(|| v.as_f64().map(|n| n != 0.0)))
        .unwrap_or(false)
}

/// 合并组级亮度范围，口径同 Python `range_set`：存的 0~1，下限不越过上限，
/// 羽化至少 0.001；开关与非开关字段可以一次同时给。
fn apply_mask_range(raw: &mut Value, range: &serde_json::Map<String, Value>) -> Result<(), String> {
    if !raw
        .as_object()
        .is_some_and(|o| o.contains_key("components"))
    {
        *raw = mask::as_group(raw);
    }
    let object = raw
        .as_object_mut()
        .ok_or_else(|| "蒙版格式不对：不是对象".to_owned())?;
    let target = object
        .entry("range".to_owned())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| "蒙版 range 格式不对".to_owned())?;
    let on = match range.get("lum_on") {
        Some(v) => v
            .as_bool()
            .or_else(|| v.as_f64().map(|n| n != 0.0))
            .ok_or_else(|| "range.lum_on 必须是布尔值".to_owned())?,
        None => range_on(target),
    };
    let mut min = range_stored(target, "lum_min", 0.0);
    let mut max = range_stored(target, "lum_max", 0.5);
    let mut feather = range_stored(target, "lum_feather", 0.1);
    let mut hue = range_stored(target, "hue", 0.0);
    let mut hue_width = range_stored(target, "hue_width", 40.0);
    let mut hue_feather = range_stored(target, "hue_feather", 20.0);
    let mut hue_sat = range_stored(target, "hue_sat", 0.05);
    let mut hue_touched = false;
    for (key, value) in range {
        let number = |v: &Value| {
            v.as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("range.{key} 必须是有限数值"))
        };
        match key.as_str() {
            "lum_on" | "hue_on" => continue,
            "lum_min" => min = number(value)?.clamp(0.0, 1.0).min(max),
            "lum_max" => max = number(value)?.clamp(0.0, 1.0).max(min),
            "lum_feather" => feather = number(value)?.clamp(0.0, 1.0).max(0.001),
            "hue" => {
                hue = number(value)?.clamp(0.0, 360.0);
                hue_touched = true;
            }
            "hue_width" => {
                hue_width = number(value)?.clamp(0.0, 360.0);
                hue_touched = true;
            }
            "hue_feather" => {
                hue_feather = number(value)?.clamp(0.0, 180.0);
                hue_touched = true;
            }
            "hue_sat" => {
                hue_sat = number(value)?.clamp(0.0, 1.0);
                hue_touched = true;
            }
            other => {
                return Err(format!(
                    "未知范围字段 {other}（亮度用 lum_*，颜色用 hue_*）"
                ));
            }
        }
    }
    target.insert("lum_on".to_owned(), json!(on));
    target.insert("lum_min".to_owned(), json!(min));
    target.insert("lum_max".to_owned(), json!(max));
    target.insert("lum_feather".to_owned(), json!(feather));
    // 命令没提颜色就别凭空长出 hue_* 键；提了就按完整一组写。
    if hue_touched || target.contains_key("hue_on") {
        let on = match range.get("hue_on") {
            Some(v) => v
                .as_bool()
                .or_else(|| v.as_f64().map(|n| n != 0.0))
                .ok_or_else(|| "range.hue_on 必须是布尔值".to_owned())?,
            None => mask_hue_flag(target),
        };
        target.insert("hue_on".to_owned(), json!(on));
        target.insert("hue".to_owned(), json!(hue));
        target.insert("hue_width".to_owned(), json!(hue_width));
        target.insert("hue_feather".to_owned(), json!(hue_feather));
        target.insert("hue_sat".to_owned(), json!(hue_sat));
    }
    Ok(())
}

/// 已存的 hue_on 布尔（可能是 true 也可能是 1）。
fn mask_hue_flag(target: &serde_json::Map<String, Value>) -> bool {
    target
        .get("hue_on")
        .is_some_and(|v| v.as_bool().unwrap_or(false) || v.as_f64().is_some_and(|n| n != 0.0))
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少字符串参数 {key}"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是字符串")),
    }
}

fn opt_u32(args: &Value, key: &str) -> Result<Option<u32>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let n = value
                .as_u64()
                .or_else(|| {
                    value.as_f64().and_then(|f| {
                        (f.is_finite() && f >= 0.0 && f.fract() == 0.0).then_some(f as u64)
                    })
                })
                .and_then(|n| u32::try_from(n).ok());
            n.map(Some).ok_or_else(|| format!("{key} 必须是非负整数"))
        }
    }
}

fn opt_f64(args: &Value, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是有限数值")),
    }
}

/// 速度键数组：`[{ at, speed }, …]`。`at` 是源进度，夹进 0..1。
fn parse_speed_keys(raw: &Value) -> Result<Vec<SpeedKey>, String> {
    let items = raw.as_array().ok_or_else(|| {
        "keys 要写成数组：[{ at: 0, speed: 1 }, { at: 0.5, speed: 4 }]".to_owned()
    })?;
    if items.is_empty() {
        return Err("keys 是空数组：要全程常速就写 speed: 1，别把速度键清空".to_owned());
    }
    items
        .iter()
        .map(|item| {
            if !item.is_object() {
                return Err(format!("速度键要写成对象，实得 {item}"));
            }
            let at = opt_f64(item, "at")?
                .ok_or_else(|| "每个速度键都要有 at（源进度 0..1）".to_owned())?;
            let speed = opt_f64(item, "speed")?
                .ok_or_else(|| "每个速度键都要有 speed（倍速，大于 0）".to_owned())?;
            Ok(SpeedKey {
                at: at.clamp(0.0, 1.0),
                speed,
            })
        })
        .collect()
}

/// 关键词参数：字符串（按 `,`/`，`/`;` 拆开）或字符串数组；空白词丢掉。
fn words_arg(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let raw: Vec<&str> = match args.get(key) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(s)) => s.split([',', '，', ';', '；']).collect(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| format!("{key} 里的元素必须是字符串"))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(format!("{key} 必须是字符串或字符串数组")),
    };
    Ok(raw
        .into_iter()
        .filter_map(aevum_core::library::clean_keyword)
        .collect())
}

/// 元数据参数：`title`/`description`/`creator`/`copyright` 四项，给了哪几项就改哪几项
/// （空串或 null 是清掉那一项）。返回有没有给——一个都没给就是纯查询，不落盘。
fn apply_metadata_args(meta: &mut project::Metadata, args: &Value) -> Result<bool, String> {
    let mut any = false;
    for (key, slot) in [
        ("title", &mut meta.title),
        ("description", &mut meta.description),
        ("creator", &mut meta.creator),
        ("copyright", &mut meta.copyright),
    ] {
        let Some(v) = args.get(key) else { continue };
        any = true;
        *slot = match v {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            other => return Err(format!("{key} 必须是字符串，收到 {other}")),
        };
    }
    Ok(any)
}

/// 元数据的返回体：一律归一化后的值，跟落盘的那一份逐字一致。
fn metadata_json(meta: &project::Metadata) -> Value {
    let c = meta.cleaned();
    json!({
        "title": c.title,
        "description": c.description,
        "creator": c.creator,
        "copyright": c.copyright,
        "filled": !c.is_empty(),
    })
}

/// 词法清理（不碰文件系统）：写目标可能还不存在，canonicalize 会失败。
fn lexical(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn path_eq(a: &Path, b: &Path) -> bool {
    let (a, b) = (lexical(a), lexical(b));
    let a: Vec<_> = a.components().collect();
    let b: Vec<_> = b.components().collect();
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| component_eq(*x, *y))
}

fn path_starts_with(child: &Path, parent: &Path) -> bool {
    let (child, parent) = (lexical(child), lexical(parent));
    let mut child = child.components();
    for expected in parent.components() {
        match child.next() {
            Some(actual) if component_eq(actual, expected) => {}
            _ => return false,
        }
    }
    true
}

#[cfg(windows)]
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

#[cfg(not(windows))]
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a == b
}

/// 导出目录不得是序列根，也不得是根的上级：批量写与收尾清理都可能碰到源帧。
fn ensure_out_dir_safe(root: &Path, out: &Path) -> Result<(), String> {
    if path_eq(out, root) {
        return Err("输出目录不能是序列根目录，会污染源帧".to_owned());
    }
    if path_starts_with(root, out) {
        return Err("输出目录不能是序列根目录的上级".to_owned());
    }
    Ok(())
}

/// 后台合并任务：逐帧读盘（报进度、可取消），再走各自的核心算法写一张静帧。
///
/// HDR 与全景共用这条读帧循环：全景的 `framing` 是全幅零位移，走的是同一段
/// 恒等分支，真正分岔只在最后一步（曝光融合 vs 特征点拼接）。取消只在两帧之间
/// 检查一次——核心算法是一整块，中途打断没有意义。
fn run_merge_job(
    job: &MergeJob,
    done: &AtomicUsize,
    total: &AtomicUsize,
    cancel: &AtomicBool,
) -> Result<Value, String> {
    let mut frames: Vec<(Vec<f32>, usize, usize)> = Vec::with_capacity(job.srcs.len());
    let mut dims: Option<(usize, usize)> = None;
    for (k, (src, (crop, off))) in job.srcs.iter().zip(job.framing.iter()).enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消：还在读帧，没写成片".to_owned());
        }
        let (rgb, w, h) = export::load_rgb_f32(src).map_err(|e| format!("读帧失败：{e}"))?;
        let rect = crop.map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize));
        let framing = Framing {
            angle_deg: 0.0,
            off: *off,
        };
        let (buf, got) = if rect.is_none() && framing.is_identity() {
            (rgb, (w as usize, h as usize))
        } else {
            let mut out = Vec::new();
            let (cw, ch) =
                warp::transform_into(&rgb, w as usize, h as usize, framing, rect, &mut out);
            (out, (cw, ch))
        };
        if job.kind == MergeKind::Hdr {
            match dims {
                None => dims = Some(got),
                Some(prev) if prev != got => {
                    return Err(format!(
                        "第 {} 帧与第 {k} 帧对齐后尺寸不一致（{}×{} vs {}×{}）：先跑对齐或裁成同尺寸",
                        job.indices[0], prev.0, prev.1, got.0, got.1
                    ));
                }
                Some(_) => {}
            }
        }
        frames.push((buf, got.0, got.1));
        done.store(k + 1, Ordering::Relaxed);
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("已取消：读完了帧，还没开始算".to_owned());
    }

    let (mut pixels, ow, oh) = match job.kind {
        MergeKind::Hdr => {
            let (w, h) = dims.unwrap_or((0, 0));
            let refs: Vec<&[f32]> = frames.iter().map(|(b, _, _)| b.as_slice()).collect();
            (aevum_core::hdr::merge(&refs, w, h), w, h)
        }
        // 全景按每张自己的尺寸求单应，混尺寸也能接。
        MergeKind::Pano => {
            let refs: Vec<(&[f32], u32, u32)> = frames
                .iter()
                .map(|(b, w, h)| (b.as_slice(), *w as u32, *h as u32))
                .collect();
            let (pano, cw, ch) =
                aevum_core::pano::stitch(&refs).map_err(|e| format!("拼接失败：{e}"))?;
            (pano, cw as usize, ch as usize)
        }
    };
    if pixels.is_empty() {
        return Err(format!("{}算出来是空图：检查帧是否可读", job.kind.noun()));
    }

    let deep = job.options.format == StillFormat::Tiff16;
    export::write_single_still(
        &job.srcs[0],
        &job.dst,
        &mut pixels,
        ow,
        oh,
        deep,
        &job.options,
    )
    .map_err(|e| format!("写{}成片失败：{e}", job.kind.noun()))?;
    done.store(total.load(Ordering::Relaxed), Ordering::Relaxed);
    Ok(json!({
        "path": job.dst.display().to_string(),
        "kind": job.kind.tag(),
        "frames": job.indices.len(),
        "indices": job.indices,
        "width": ow,
        "height": oh,
    }))
}

// ---------------------------------------------------------------- 序列体检

/// 一次 `stat` 多少帧再查一次取消。64 帧约几毫秒，够让取消按下去就有反应，
/// 又不至于每帧做一次原子读。
const QA_CHUNK: usize = 64;

/// 后台体检任务：逐帧 `stat` 攒出判定输入（报进度、可取消），最后跑一次纯数组判定。
///
/// 测光随导入那趟已经算好，这里不再解一张图；判定本身是 O(n·window)，放在读盘之后
/// 整块跑，中途打断只会浪费已经攒好的表，所以只在批间检查取消。
fn run_qa_job(
    frames: &[(PathBuf, Option<FrameMetrics>)],
    opts: &qa::Options,
    done: &AtomicUsize,
    cancel: &AtomicBool,
) -> Result<Vec<Finding>, String> {
    if frames.is_empty() {
        return Err("序列里没有帧".to_owned());
    }
    let mut probes = Vec::with_capacity(frames.len());
    for part in frames.chunks(QA_CHUNK) {
        if cancel.load(Ordering::SeqCst) {
            return Err("体检已取消".to_owned());
        }
        for (path, sample) in part {
            probes.push(qa::probe(path, *sample));
        }
        done.store(probes.len(), Ordering::Relaxed);
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("体检已取消".to_owned());
    }
    Ok(qa::audit(&probes, opts))
}

// ---------------------------------------------------------------- 命令单测

#[cfg(test)]
mod frame_tests {
    use super::*;

    fn rect(c: CropRect) -> (u32, u32, u32, u32) {
        (c.x, c.y, c.w, c.h)
    }

    /// 画幅取景必须居中、贴边、不越界，且和画布上画的框同一套数；
    /// 四条边全部成对，4:2:0 编码才不会偷偷挪一像素。
    #[test]
    fn aspect_crop_centers_inside_the_frame() {
        let res = Some((6000, 4000));
        assert_eq!(
            rect(fit_aspect(None, res, Aspect::Wide).unwrap()),
            (0, 312, 6000, 3374)
        );
        assert_eq!(
            rect(fit_aspect(None, res, Aspect::Vertical).unwrap()),
            (1874, 0, 2250, 4000)
        );
        assert_eq!(
            rect(fit_aspect(None, res, Aspect::Square).unwrap()),
            (1000, 0, 4000, 4000)
        );
        assert!(fit_aspect(None, res, Aspect::Native).is_none());
        assert!(fit_aspect(None, None, Aspect::Wide).is_none());
    }

    /// 对齐裁切先叠一层：画幅在对齐后的框里内接，不能跑出对齐框。
    #[test]
    fn aspect_crop_nests_inside_alignment_crop() {
        let base = CropRect {
            x: 100,
            y: 200,
            w: 5000,
            h: 3000,
        };
        let c = fit_aspect(Some(base), Some((6000, 4000)), Aspect::Square).unwrap();
        assert_eq!(rect(c), (1100, 200, 3000, 3000));
        let wide = fit_aspect(Some(base), Some((6000, 4000)), Aspect::Wide).unwrap();
        assert_eq!(rect(wide), (100, 294, 5000, 2812));
        assert!(wide.y + wide.h <= base.y + base.h, "不许越出对齐裁切");
    }

    /// 拖框交互走的是 `aevum_core::crop` 那一套几何：拉、推、挪、微调都留在画面内，
    /// 坐标成对；松手拖满一格退回默认取景，不白占一次重画。
    #[test]
    fn crop_drag_roundtrip_uses_the_core_geometry() {
        let mut state = AppState::new();
        state.resolution = Some((60, 48));
        assert_eq!(rect(state.crop_box()), (0, 0, 60, 48), "没裁过就是整帧");

        state.crop_begin([20.0, 20.0], crop::Hit::Outside);
        state.crop_drag_to([40.0, 40.0]);
        state.crop_end();
        assert_eq!(state.user_crop.map(rect), Some((20, 20, 20, 20)));

        state.crop_begin([25.0, 25.0], crop::Hit::Inside);
        state.crop_drag_to([30.0, 35.0]);
        // 平移后左上角照样取偶：25 落回 24，这是 Python 侧同一条规则。
        assert_eq!(state.user_crop.map(rect), Some((24, 28, 20, 20)));

        state.crop_begin([45.0, 44.0], crop::Hit::Corner(crop::Handle::Se));
        state.crop_drag_to([50.0, 45.0]);
        state.crop_end();
        assert_eq!(
            state.user_crop.map(rect),
            Some((24, 28, 26, 16)),
            "拉右下角只动那两边，尺寸取偶"
        );

        state.crop_nudge(-1, 1, 4);
        assert_eq!(state.user_crop.map(rect), Some((20, 32, 26, 16)));

        state.crop_begin([0.0, 0.0], crop::Hit::Outside);
        state.crop_drag_to([60.0, 48.0]);
        state.crop_end();
        assert_eq!(state.user_crop, None);

        // 没有用户框时，画幅比例就是取景框，和画布上画的同一套数（顶边取偶后 6）。
        state.set_aspect(Aspect::Wide);
        assert_eq!(rect(state.crop_box()), (0, 6, 60, 34));

        // 用户框优先，比例只负责在它里面内接；改比例会把框绕中心重铺。
        state.user_crop = Some(CropRect {
            x: 10,
            y: 10,
            w: 40,
            h: 30,
        });
        // f32 的比例精度决定这里落到 22 而不是 23，取景只需保持居中且不越出用户框。
        assert_eq!(rect(state.effective_crop().unwrap()), (10, 14, 40, 22));
        state.set_aspect(Aspect::Square);
        // 重铺后的左上角照样取偶：5 落回 4，与 Python 的 norm_rect 同一口径。
        assert_eq!(state.user_crop.map(rect), Some((10, 4, 40, 40)));
        assert_eq!(rect(state.effective_crop().unwrap()), (10, 4, 40, 40));
    }

    /// 拉直之后，画布上能拖的范围只有安全框：伸进空白角的那部分当场被削回来，
    /// 画布上的框和导出的框才是同一个矩形。
    #[test]
    fn crop_drag_stays_inside_the_straighten_safe_box() {
        let mut state = AppState::new();
        state.resolution = Some((60, 48));
        state.set_rotate(12.0);
        let view = state.crop_view();
        assert!(
            view.x > 0 && view.y > 0 && view.w < 60 && view.h < 48,
            "转过 12° 后安全框应收进画面里"
        );
        assert_eq!(rect(state.crop_box()), rect(view), "没拖过框就是可见区");

        // 从角上拖满整个画面：削边后正好铺满安全框，等价于没裁。
        state.crop_begin([0.0, 0.0], crop::Hit::Outside);
        state.crop_drag_to([60.0, 48.0]);
        state.crop_end();
        assert_eq!(state.user_crop, None, "铺满可见区不该留下一层白裁");

        // 只拖左上半幅：左上角在空白里，落点得顶到安全框的左沿和上沿。
        state.crop_begin([0.0, 0.0], crop::Hit::Outside);
        state.crop_drag_to([30.0, 24.0]);
        state.crop_end();
        let box_ = state.user_crop.expect("半幅拖框应留下手动取景");
        assert_eq!((box_.x, box_.y), (view.x, view.y), "被削到安全框的左上角");
        assert!(box_.x + box_.w <= view.x + view.w && box_.y + box_.h <= view.y + view.h);
        assert_eq!(
            rect(state.effective_crop().unwrap()),
            rect(box_),
            "导出框与画布框同源"
        );

        // 先拉直再加大角度：已有的框得跟着往里收，不能留在空白角上。
        state.set_rotate(0.0);
        state.user_crop = Some(CropRect {
            x: 2,
            y: 2,
            w: 56,
            h: 44,
        });
        state.set_rotate(25.0);
        let safe = state.crop_view();
        let cur = state.user_crop.expect("削边不该把框清没");
        assert!(
            cur.x >= safe.x
                && cur.y >= safe.y
                && cur.x + cur.w <= safe.x + safe.w
                && cur.y + cur.h <= safe.y + safe.h,
            "角度加大后框不许伸进空白角"
        );
    }

    /// 拖取景框走的是裁剪那套几何，只是落账写进归一化的两端：像素框 ↔ 归一化
    /// 必须往返一致，画布上的框和导出里的窗口才是同一个矩形。
    #[test]
    fn motion_drag_lays_the_box_on_the_output_aspect() {
        let mut state = AppState::new();
        state.resolution = Some((60, 48));
        state.output_w = 60;
        state.output_h = 48;
        state.set_motion_picking(true);
        assert!(state.motion_editing(), "拖取景框态接手画布");
        // 默认终点往里收一档：按 5:4 取偶后落在 (4,4,50,40)。
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::End)),
            (4, 4, 50, 40),
            "起点整幅、终点内收，一开就看得见幅度"
        );

        state.motion_begin([10.0, 10.0], crop::Hit::Outside);
        state.motion_drag_to([35.0, 30.0]);
        state.motion_end();
        assert!(state.motion.enabled, "给了框就当真空开运镜");
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::End)),
            (10, 10, 20, 16),
            "定比例拉框：两轴取更大的那个推进，宽高成对"
        );
        // 归一化写回再换算回来，一个像素都不差。
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::End)),
            (10, 10, 20, 16)
        );

        state.motion_begin([20.0, 20.0], crop::Hit::Inside);
        state.motion_drag_to([25.0, 28.0]);
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::End)),
            (14, 18, 20, 16),
            "框内平移只挪位置，左上角照样取偶"
        );

        state.motion_begin([35.0, 34.0], crop::Hit::Corner(crop::Handle::Se));
        state.motion_drag_to([45.0, 42.0]);
        state.motion_end();
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::End)),
            (14, 18, 30, 24),
            "拉右下角以对角为锚，比例不破"
        );

        state.motion_nudge(1, -1, 2);
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::End)),
            (16, 16, 30, 24)
        );

        // 起点那一端照旧：拖终点不该把起点带跑。
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::Start)),
            (0, 0, 60, 48)
        );
    }

    /// 取景框归一化的基准就是导出取景那一幅：画幅一改，基准自己换形，框按份额跟着走，
    /// 不需要再重铺一次；小图撑不下定比例的最小步长时退回自由比例。
    #[test]
    fn motion_box_follows_the_export_framing_basis() {
        let mut state = AppState::new();
        state.resolution = Some((60, 48));
        // 输出画幅跟基准同一个比例（60:34 → 30:17），框才铺得满整幅。
        state.output_w = 60;
        state.output_h = 34;
        state.set_motion_picking(true);
        state.set_aspect(Aspect::Wide);
        let base = state.motion_base_rect();
        assert_eq!(rect(base), (0, 6, 60, 34), "基准跟着画幅换形");
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::Start)),
            rect(base),
            "起点铺满基准 = 画布上看到的就是那一条"
        );

        // 从画面外拖进来：框只能落在基准那一幅里，越界的部分被夹回边沿。
        state.motion_begin([-8.0, -8.0], crop::Hit::Outside);
        state.motion_drag_to([70.0, 60.0]);
        state.motion_end();
        assert_eq!(rect(state.motion_box_rect(MotionBox::End)), rect(base));
        assert_eq!(state.motion.to, crop::FULL, "铺满基准就是整幅，不留一圈白");
        assert!(
            !state.motion.is_active(),
            "两框重合时导出走快路，一像素都不重采样"
        );

        // 输出画幅约完仍撑不进基准（小图配超宽比）：退回自由比例，别把框夹飞。
        state.set_aspect(Aspect::Native);
        state.output_w = 4096;
        state.output_h = 2160;
        state.motion_begin([10.0, 10.0], crop::Hit::Outside);
        state.motion_drag_to([35.0, 30.0]);
        state.motion_end();
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::End)),
            (10, 10, 24, 20),
            "自由比例：框就是两个角夹出来的那块，宽高取偶"
        );

        // 铺满按钮直接把当前那一端顶到整幅，不用把手拉半天。
        state.motion_box = MotionBox::Start;
        state.motion_fill_box();
        assert_eq!(
            rect(state.motion_box_rect(MotionBox::Start)),
            (0, 0, 60, 48)
        );
    }

    /// 裁剪框与运镜框抢的是同一只手鼠标：谁开谁拿把手，另一头的半截拖拽当场作废。
    #[test]
    fn motion_picking_and_crop_never_share_the_mouse() {
        let mut state = AppState::new();
        state.resolution = Some((60, 48));
        state.set_motion_picking(true);
        assert!(state.motion_editing());

        // 切裁剪工具就退出取景框编辑：两头都要把手，只能留一个。
        state.set_tool(Tool::Crop);
        assert!(!state.motion_picking, "选裁剪工具就退出取景框编辑");
        assert!(state.crop_editing() && !state.motion_editing());

        // 反过来：在裁剪态里按「拖取景框」，裁剪工具当场放下，运镜才握得住把手。
        state.set_module(Module::Frame);
        state.set_motion_picking(true);
        assert_eq!(state.tool, Tool::Hand, "切回运镜时把裁剪工具放下");
        assert!(state.motion_picking);
        assert!(
            !state.motion_editing(),
            "构图模块里裁剪框优先，运镜框让开一层"
        );
        state.set_module(Module::Render);
        assert!(state.motion_editing(), "回到成片模块运镜框接手");

        // 另一头的半截拖拽不能留在手里。
        state.crop_begin([10.0, 10.0], crop::Hit::Outside);
        assert!(state.crop_drag.is_some());
        state.set_motion_picking(false);
        state.set_motion_picking(true);
        assert!(state.crop_drag.is_none(), "进运镜就把裁剪那半截丢掉");
        state.motion_begin([0.0, 0.0], crop::Hit::Outside);
        assert!(state.motion_drag.is_some());
        state.set_tool(Tool::Crop);
        assert!(state.motion_drag.is_none(), "出运镜就把取景框那半截丢掉");

        // 离开/进入取景框编辑各重画一次：一次是整幅，一次是框里的内容。
        state.preview_dirty = false;
        state.set_motion_picking(true);
        assert!(state.preview_dirty);
        state.preview_dirty = false;
        state.set_motion_picking(false);
        assert!(state.preview_dirty);
    }
}

#[cfg(test)]
mod auto_tests {
    use super::*;

    fn dark_frame(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aevum-ui-auto-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join("dark.png");
        image::RgbImage::from_pixel(64, 48, image::Rgb([34, 34, 34]))
            .save(&path)
            .expect("写测试帧");
        path
    }

    fn state_on(path: &PathBuf) -> AppState {
        let mut state = AppState::new();
        state.seq = Some(Sequence::open_files(std::slice::from_ref(path)).expect("开单帧"));
        state.selected = 0;
        state
    }

    #[test]
    fn auto_writes_absolute_values_and_clicking_twice_is_stable() {
        let path = dark_frame("stable");
        let mut state = state_on(&path);
        state.params.exposure = 2.5;
        state.auto_exposure();
        assert!(
            state.params.exposure > 0.3,
            "暗图该提亮：{state:?}",
            state = state.params.exposure
        );
        let first = (
            state.params.exposure,
            state.params.contrast,
            state.params.highlights,
            state.params.shadows,
            state.params.whites,
            state.params.blacks,
            state.params.temperature,
            state.params.tint,
        );
        state.auto_exposure();
        let second = (
            state.params.exposure,
            state.params.contrast,
            state.params.highlights,
            state.params.shadows,
            state.params.whites,
            state.params.blacks,
            state.params.temperature,
            state.params.tint,
        );
        assert_eq!(first, second, "重复点自动只能在原值上写回同一组绝对值");
        assert!(state.auto_note.as_deref().is_some_and(|n| n.contains("EV")));
    }

    #[test]
    fn auto_does_not_add_manual_exposure() {
        let path = dark_frame("absolute");
        let mut clean = state_on(&path);
        clean.auto_exposure();
        let mut tweaked = state_on(&path);
        tweaked.params.exposure = 3.0;
        tweaked.auto_exposure();
        assert_eq!(clean.params.exposure, tweaked.params.exposure);
    }
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aevum-ui-prev-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn render(path: &std::path::Path, sketch: bool) -> PreviewParts {
        render_preview_file(
            path,
            &GradeParams::default(),
            Ev::NONE,
            None,
            None,
            Framing::default(),
            None,
            sketch,
            false,
        )
        .expect("渲染预览")
    }

    /// 草图贴图减半，但报给画布的逻辑尺寸必须和全量一致，不然拖动时画布会跳一下。
    #[test]
    fn sketch_texture_is_half_but_logical_size_matches_full() {
        let dir = temp_dir("sketch");
        let path = dir.join("frame.png");
        let mut img = image::RgbImage::new(120, 80);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 2) as u8, (y * 3) as u8, ((x + y) * 3) as u8]);
        }
        img.save(&path).expect("写测试图");

        let full = render(&path, false);
        let sketch = render(&path, true);
        assert_eq!(full.2, [120, 80], "全量逻辑尺寸");
        assert_eq!(sketch.2, full.2, "草图的逻辑尺寸应与全量一致");
        assert_eq!(full.0.size, [120, 80], "全量贴图尺寸");
        assert_eq!(sketch.0.size, [60, 40], "草图贴图应是半分辨率");
        assert_eq!(sketch.0.pixels.len(), 60 * 40, "草图像素数应与贴图尺寸一致");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 画布上那张图和导出走的是同一趟取景几何：转过 90°，白点就该出现在画面上方。
    #[test]
    fn preview_straighten_rotates_the_pixels_it_shows() {
        let dir = temp_dir("rotate");
        let path = dir.join("frame.png");
        let mut img = image::RgbImage::new(9, 9);
        *img.get_pixel_mut(6, 4) = image::Rgb([255, 255, 255]);
        img.save(&path).expect("写测试图");

        let (graded, _, size, _) = render_preview_file(
            &path,
            &GradeParams::default(),
            Ev::NONE,
            None,
            None,
            Framing {
                angle_deg: 90.0,
                off: [0.0, 0.0],
            },
            None,
            false,
            false,
        )
        .expect("渲染拉直预览");
        assert_eq!(size, [9, 9]);
        let bright = |x: usize, y: usize| graded.pixels[y * 9 + x].r() > 200;
        assert!(bright(4, 2), "中心右边的点该转到上方");
        assert!(!bright(6, 4), "原来的位置不该还留着白的");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预览的取景框要和导出同一条采样路：把右下 1/4 铺满整幅，落在框内角上那颗白点
    /// 就该被推到输出的左上角，而不是留在源位。
    #[test]
    fn preview_view_pushes_in_the_pixels_it_shows() {
        let dir = temp_dir("view");
        let path = dir.join("frame.png");
        let mut img = image::RgbImage::new(8, 8);
        *img.get_pixel_mut(4, 4) = image::Rgb([255, 255, 255]);
        img.save(&path).expect("写测试图");

        let plain = render(&path, false).0;
        let (graded, _, size, _) = render_preview_file(
            &path,
            &GradeParams::default(),
            Ev::NONE,
            None,
            None,
            Framing::default(),
            Some(Viewfinder {
                x: 0.5,
                y: 0.5,
                w: 0.5,
                h: 0.5,
            }),
            false,
            false,
        )
        .expect("渲染运镜预览");
        assert_eq!(size, [8, 8], "取景框不该改输出尺寸");
        assert!(plain.pixels[4 * 8 + 4].r() > 200, "不动时白点在源位");
        // 窗口 (4,4,4,4) 铺到 8×8 = 推近 2×：采样点 px=3.75+0.5col，源 (4,4) 那点白
        // 被双线性摊成 0.75/0.75/0.25 的权重表——四格 143、八格 48、一格 16，剩下全黑。
        // 逐格对数，比「有没有变亮」钉得住采样相位。
        let w = |i: usize| -> f32 {
            match i {
                0 | 1 => 0.75,
                2 => 0.25,
                _ => 0.0,
            }
        };
        for y in 0..8usize {
            for x in 0..8usize {
                let want = (255.0 * w(x) * w(y)).round() as i32;
                let got = graded.pixels[y * 8 + x].r() as i32;
                assert!(
                    (got - want).abs() <= 2,
                    "({x},{y}) 的亮度 {got} 对不上推近 2× 的双线性权重表：期望 {want}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aevum-ui-cmd-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn profile_set_validates_kind_and_clears() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let err = state
            .run_ui_command(&ctx, "profile.set", &json!({}))
            .unwrap_err();
        assert!(err.contains("kind"), "{err}");
        let err = state
            .run_ui_command(&ctx, "profile.set", &json!({"kind": "xmp"}))
            .unwrap_err();
        assert!(err.contains("未知"), "{err}");
        let err = state
            .run_ui_command(&ctx, "profile.set", &json!({"kind": "dcp", "path": "  "}))
            .unwrap_err();
        assert!(err.contains("path"), "{err}");
        let out = state
            .run_ui_command(&ctx, "profile.set", &json!({"kind": "dcp", "clear": true}))
            .expect("清相机配置");
        assert_eq!(out["dcp"]["path"], json!(""));
        assert_eq!(out["dcp"]["name"], json!(""));
        let out = state
            .run_ui_command(&ctx, "profile.set", &json!({"kind": "lcp", "clear": true}))
            .expect("清镜头配置");
        assert!(out["lcp"]["correction"].is_null());
        // 没有序列/EXIF 时自动匹配只留说明，不动参数。
        state.match_lcp_profile();
        assert!(state.lcp_note.contains("EXIF"), "{}", state.lcp_note);
        state.match_dcp_profile();
        assert!(state.dcp_note.contains("EXIF"), "{}", state.dcp_note);
    }

    #[test]
    fn grade_set_merges_and_rejects_unknown_fields() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let value = state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5, "contrast": 12}))
            .expect("合并参数");
        assert_eq!(value["params"]["exposure"], json!(0.5));
        assert_eq!(value["params"]["contrast"], json!(12.0));
        assert_eq!(value["preset"], json!("自定义"));

        let err = state
            .run_ui_command(&ctx, "grade.set", &json!({"params": {"bogus": 1}}))
            .unwrap_err();
        assert!(err.contains("bogus"), "{err}");
    }

    #[test]
    fn grade_auto_command_writes_absolute_values_and_repeats_identically() {
        let dir = temp_dir("auto");
        let img = image::RgbImage::from_pixel(64, 48, image::Rgb([34, 34, 34]));
        img.save(dir.join("dark.png")).expect("写暗帧");
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        state.params.exposure = 2.5;
        let first = state
            .run_ui_command(&ctx, "grade.auto", &json!({}))
            .expect("自动校正");
        let exposure = first["params"]["exposure"].as_f64().expect("数值");
        assert!(exposure > 0.3, "暗图该提亮：{first}");
        assert!(first["note"].as_str().unwrap_or("").contains("自动"));
        let second = state
            .run_ui_command(&ctx, "grade.auto", &json!({}))
            .expect("再自动一次");
        assert_eq!(
            first["params"], second["params"],
            "同一帧重复自动校正必须给出同一组绝对值"
        );
    }

    #[test]
    fn mask_commands_add_update_and_remove() {
        let dir = temp_dir("mask");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");

        let added = state
            .run_ui_command(&ctx, "mask.add", &json!({"kind": "linear"}))
            .expect("加蒙版");
        assert_eq!(added["count"], 1);
        assert_eq!(added["added"], 1);
        assert_eq!(added["selected"], 1);
        assert_eq!(state.preset, "自定义");

        let updated = state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({
                    "index": 1,
                    "amount": 80,
                    "invert": true,
                    "params": {"exposure": 1.5, "contrast": null},
                    "geometry": {
                        "x1": 0.2, "y1": 0.3, "x2": 0.8, "y2": 0.7,
                        "angle": 400.0, "feather": 20
                    }
                }),
            )
            .expect("改蒙版");
        assert_eq!(updated["updated"], 1);
        let parsed = mask::parse_mask(&state.params.masks[0]).expect("改完仍可解析");
        assert_eq!(parsed.amount, 80.0);
        assert!(parsed.invert);
        assert_eq!(parsed.params.get("exposure"), Some(&1.5));
        assert!(!parsed.params.contains_key("contrast"));
        let comp = &parsed.components[0];
        assert_eq!(comp.feather, 20.0);
        assert_eq!(comp.x1, 0.2);
        assert!((comp.angle - 40.0).abs() < 1e-9, "{}", comp.angle);

        let err = state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({"index": 1, "params": {"bogus": 1}}),
            )
            .unwrap_err();
        assert!(err.contains("bogus"), "{err}");
        let err = state
            .run_ui_command(&ctx, "mask.update", &json!({"index": 2}))
            .unwrap_err();
        assert!(err.contains("1..=1"), "{err}");

        let removed = state
            .run_ui_command(&ctx, "mask.remove", &json!({"index": 1}))
            .expect("删蒙版");
        assert_eq!(removed["count"], 0);
        assert!(state.params.masks.is_empty());
        assert_eq!(state.mask_sel, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lens_profile_matches_from_exif_and_toggles() {
        let dir = temp_dir("lensdb");
        let db_path = dir.join("test-lensfun.xml");
        std::fs::write(
            &db_path,
            r#"<?xml version="1.0"?>
<lensdatabase version="1">
  <camera>
    <maker>Nikon</maker>
    <model>Z 8</model>
    <cropfactor>1.0</cropfactor>
  </camera>
  <lens>
    <maker>Nikon</maker>
    <model>NIKKOR Z 24-70mm f/2.8 S</model>
    <cropfactor>1.0</cropfactor>
    <calibration>
      <distortion model="ptlens" focal="24" a="0.012" b="-0.031" c="0.018"/>
      <vignetting model="pa" focal="24" aperture="2.8" distance="10" k1="-0.39" k2="0.35" k3="-0.12"/>
    </calibration>
  </lens>
</lensdatabase>
"#,
        )
        .expect("写测试库");
        unsafe { std::env::set_var("AEVUM_LENSFUN", db_path.as_os_str()) };

        let mut state = AppState::new();
        // EXIF 三件套命中：折出的校正进参数，面板说明带上型号。
        state.lens_match_apply(
            "NIKON CORPORATION",
            "NIKON Z 8",
            "NIKKOR Z 24-70mm f/2.8 S",
            47.0,
            Some(2.8),
        );
        assert!(state.params.lens_profile.is_some());
        assert!(state.lens_profile_note.contains("NIKKOR Z 24-70mm"));
        // 命令面查询与开关：关掉暂存、再开恢复。
        let out = state.cmd_lens_set(&json!({})).expect("查询");
        assert_eq!(out["profile"]["lens"], json!("NIKKOR Z 24-70mm f/2.8 S"));
        assert_eq!(out["profile"]["dist"]["model"], json!("ptlens"));
        let out = state.cmd_lens_set(&json!({"enabled": false})).expect("关");
        assert!(out["profile"].is_null());
        assert!(state.params.lens_profile.is_none());
        let out = state.cmd_lens_set(&json!({"enabled": true})).expect("开");
        assert!(out["profile"].is_object());
        // 没命中的镜头：参数不动，留一行说明。
        state
            .cmd_lens_set(&json!({"enabled": false}))
            .expect("再关");
        state.lens_match_apply("Canon", "EOS R5", "RF 100mm", 100.0, None);
        assert!(state.params.lens_profile.is_none());
        assert!(state.lens_profile_note.contains("没有"));
        // grade.set 合并别的字段不会把已匹配的配置抖掉。
        state.lens_match_apply(
            "NIKON CORPORATION",
            "NIKON Z 8",
            "NIKKOR Z 24-70mm f/2.8 S",
            47.0,
            Some(2.8),
        );
        let ctx = Context::default();
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5}))
            .expect("改曝光");
        assert!(
            state.params.lens_profile.is_some(),
            "grade.set 保留镜头配置"
        );
        unsafe { std::env::remove_var("AEVUM_LENSFUN") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `AEVUM_LIBRARY` 是进程级环境变量：两个用它的测试并行跑时会把对方的图库清单
    /// 指到别处（一边 set、一边 remove），读到空清单就索引越界。串起来跑。
    static LIBRARY_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn library_commands_catalog_and_open() {
        let _env = LIBRARY_ENV
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = temp_dir("library");
        let store = dir.join("galleries.json");
        // 清单写到临时处，别把测试目录记进用户真的图库。
        unsafe { std::env::set_var("AEVUM_LIBRARY", store.as_os_str()) };
        let mut roots = dir.clone();
        roots.push("seq");
        std::fs::create_dir_all(&roots).unwrap();
        for i in 0..3u8 {
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 20, 40, 60]));
            img.save(roots.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        std::fs::create_dir_all(aevum_core::project::guard_dir(&roots)).unwrap();
        let mut proj = aevum_core::project::ProjectFile::default();
        proj.ratings.insert(1, 4);
        proj.flags.insert(2, 1);
        aevum_core::project::save(&aevum_core::project::project_path(&roots), &proj)
            .expect("写工程");

        let ctx = Context::default();
        let mut state = AppState::new();
        assert!(state.lib_roots.is_empty(), "临时清单从空开始");

        // 加目录：编目即扫描，汇总带上张数。
        let out = state
            .run_ui_command(
                &ctx,
                "library.add",
                &json!({"path": roots.display().to_string()}),
            )
            .expect("图库加目录");
        assert_eq!(out["index"], json!(1));
        assert_eq!(out["summary"], json!("3 张 · 1 星级 · 1 选用"));

        // 清单确实落到了 AEVUM_LIBRARY 指的地方。
        assert_eq!(state.lib_roots, vec![roots.clone()]);

        // 列清单 + 编目：逐文件评级/旗标跟着自然序帧号走。
        let out = state
            .run_ui_command(&ctx, "library.list", &json!({"index": 1}))
            .expect("列图库");
        assert_eq!(out["roots"][0]["scanned"], json!(true));
        assert_eq!(out["roots"][0]["files"].as_array().unwrap().len(), 3);
        assert_eq!(out["roots"][0]["files"][1]["rating"], json!(4));
        assert_eq!(out["roots"][0]["files"][2]["flag"], json!(1));
        assert_eq!(out["roots"][0]["files"][1]["frame"], json!(2));

        // 写回：library.organize 只改目标帧，其它帧原样；内存编目同步更新。
        let out = state
            .run_ui_command(
                &ctx,
                "library.organize",
                &json!({"frame": 2, "rating": 5, "label": 2}),
            )
            .expect("改评级与色标");
        assert_eq!(out["rating"], json!(5));
        assert_eq!(out["label"], json!(2));
        let reloaded =
            aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
        assert_eq!(reloaded.ratings.get(&1), Some(&5));
        assert_eq!(reloaded.flags.get(&2), Some(&1), "旗标不动");
        assert_eq!(reloaded.labels.get(&1), Some(&2));
        assert_eq!(state.lib_entries[0][1].rating, 5, "内存编目同步");

        // 多选：Ctrl 加选后键盘目标变成整批，整批一次写回。
        state.lib_click(0, false, false);
        state.lib_click(2, true, false);
        assert_eq!(state.lib_checked.len(), 2);
        let changed = state
            .lib_organize_targets(aevum_core::library::Organize {
                flag: Some(-1),
                ..Default::default()
            })
            .expect("批量改旗标");
        assert_eq!(changed, 2);
        let reloaded =
            aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
        assert_eq!(reloaded.flags.get(&0), Some(&-1));
        assert_eq!(reloaded.flags.get(&2), Some(&-1));
        assert_eq!(reloaded.ratings.get(&1), Some(&5), "别的帧不动");
        // Shift 连选从锚点铺到点击行。
        state.lib_click(0, false, false);
        state.lib_click(2, false, true);
        assert_eq!(state.lib_checked.len(), 3);

        // 命令面同一条口径：frames 数组一次改多张。
        let out = state
            .run_ui_command(
                &ctx,
                "library.organize",
                &json!({"frames": [1, 3], "rating": 3}),
            )
            .expect("批量命令");
        assert_eq!(out["count"], json!(2));
        assert_eq!(out["frames"], json!([1, 3]));
        for bad in [
            json!({"frame": 1, "frames": [1, 2], "rating": 3}),
            json!({"frames": [], "rating": 3}),
            json!({"frames": [0], "rating": 3}),
            json!({"frames": [4], "rating": 3}),
            json!({"frames": "1,2", "rating": 3}),
        ] {
            assert!(
                state
                    .run_ui_command(&ctx, "library.organize", &bad)
                    .is_err(),
                "{bad}"
            );
        }

        // 坏参不落盘。
        for bad in [
            json!({"frame": 0, "rating": 1}),
            json!({"frame": 4, "rating": 1}),
            json!({"frame": 1, "rating": 6}),
            json!({"frame": 1, "label": 5}),
            json!({"frame": 1, "flag": 2}),
            json!({"frame": 1}),
        ] {
            assert!(
                state
                    .run_ui_command(&ctx, "library.organize", &bad)
                    .is_err(),
                "{bad}"
            );
        }

        // 关键词：字符串按逗号拆、归一化去重；批量一次落盘；编目与词表同步。
        let out = state
            .run_ui_command(
                &ctx,
                "library.keyword",
                &json!({"frame": 1, "add": "日出， 前景 "}),
            )
            .expect("加词");
        assert_eq!(out["keywords"], json!(["前景", "日出"]), "字典序");
        assert_eq!(out["touched"], json!(1));
        let out = state
            .run_ui_command(
                &ctx,
                "library.keyword",
                &json!({"frames": [1, 2], "add": ["日出", "日出"], "remove": "云海"}),
            )
            .expect("批量改词");
        assert_eq!(out["touched"], json!(1), "没变的帧不算改动");
        let reloaded =
            aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
        assert_eq!(reloaded.keywords.get(&0).unwrap(), &["前景", "日出"]);
        assert_eq!(reloaded.keywords.get(&1).unwrap(), &["日出"]);
        let out = state
            .run_ui_command(&ctx, "library.list", &json!({"index": 1}))
            .expect("列编目");
        assert_eq!(
            out["roots"][0]["files"][0]["keywords"],
            json!(["前景", "日出"])
        );
        assert_eq!(out["roots"][0]["files"][1]["keywords"], json!(["日出"]));
        assert_eq!(
            out["roots"][0]["keywords"],
            json!([{"keyword": "前景", "count": 1}, {"keyword": "日出", "count": 2}])
        );
        // 按词筛选：只留下带这个词的行。
        state.lib_filter.keyword = "日出".to_owned();
        assert!(state.lib_filter.keeps(&state.lib_entries[0][0]));
        assert!(!state.lib_filter.keeps(&state.lib_entries[0][2]));
        state.lib_filter = aevum_core::library::Filter::default();
        for bad in [
            json!({"frame": 1}),
            json!({"frame": 1, "add": "   "}),
            json!({"frame": 1, "frames": [1], "add": "x"}),
            json!({"frame": 1, "add": 3}),
        ] {
            assert!(
                state.run_ui_command(&ctx, "library.keyword", &bad).is_err(),
                "{bad}"
            );
        }

        // 说明与版权：目录级，落进这个目录的工程文件；不带参数只查询、不抹值。
        let out = state
            .run_ui_command(
                &ctx,
                "library.metadata",
                &json!({"title": " 云海  日出 ", "copyright": "© 2026 张三"}),
            )
            .expect("写元数据");
        assert_eq!(out["title"], json!("云海 日出"), "空白折叠");
        assert_eq!(out["saved"], json!(true));
        assert_eq!(out["filled"], json!(true));
        assert_eq!(
            state.lib_meta.title, "云海 日出",
            "图库信息面板的缓冲跟着刷新"
        );
        let reloaded =
            aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
        assert_eq!(reloaded.metadata.title, "云海 日出");
        assert_eq!(reloaded.metadata.copyright, "© 2026 张三");
        assert_eq!(reloaded.metadata.description, "");
        let out = state
            .run_ui_command(&ctx, "library.metadata", &json!({}))
            .expect("查元数据");
        assert_eq!(out["saved"], json!(false), "只读不写盘");
        assert_eq!(out["copyright"], json!("© 2026 张三"));
        let out = state
            .run_ui_command(
                &ctx,
                "library.metadata",
                &json!({"title": "", "copyright": null}),
            )
            .expect("清元数据");
        assert_eq!(out["filled"], json!(false));
        let reloaded =
            aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
        assert_eq!(reloaded.metadata, aevum_core::project::Metadata::default());
        assert_eq!(
            reloaded.keywords.get(&0).unwrap(),
            &["前景", "日出"],
            "别动关键词"
        );
        for bad in [json!({"title": 3}), json!({"root": 9})] {
            assert!(
                state
                    .run_ui_command(&ctx, "library.metadata", &bad)
                    .is_err(),
                "{bad}"
            );
        }

        // 集合：新建、加入、去重、落盘、移出、删除。
        let out = state
            .run_ui_command(&ctx, "collection.create", &json!({"name": "精选"}))
            .expect("建集合");
        assert_eq!(out["index"], json!(1));
        let out = state
            .run_ui_command(&ctx, "collection.add", &json!({"name": "精选", "frame": 1}))
            .expect("加入集合");
        assert_eq!(out["added"], json!(true));
        let out = state
            .run_ui_command(&ctx, "collection.add", &json!({"index": 1, "frame": 1}))
            .expect("重复加入");
        assert_eq!(out["added"], json!(false), "同一张不重复");
        state
            .run_ui_command(&ctx, "collection.add", &json!({"index": 1, "frame": 3}))
            .expect("再加一张");
        let out = state
            .run_ui_command(&ctx, "collection.list", &json!({"index": 1}))
            .expect("列集合");
        assert_eq!(out["collections"][0]["count"], json!(2));
        assert_eq!(out["collections"][0]["files"].as_array().unwrap().len(), 2);
        let saved = aevum_core::library::load(&store);
        assert_eq!(saved.collections[0].files.len(), 2, "集合随清单落盘");
        // 一次收一批：重复路径只算一次，整批只落一次盘。
        state.lib_collection_create("备选");
        let batch = vec![
            state.lib_entries[0][0].path.clone(),
            state.lib_entries[0][2].path.clone(),
            state.lib_entries[0][0].path.clone(),
        ];
        assert_eq!(state.lib_collection_put_many(1, &batch), 2);
        assert_eq!(state.lib_cols[1].files.len(), 2);
        assert!(state.lib_collection_delete(1));
        let out = state
            .run_ui_command(&ctx, "collection.remove", &json!({"index": 1, "frame": 1}))
            .expect("移出集合");
        assert_eq!(out["removed"], json!(true));
        assert_eq!(out["count"], json!(1));
        // 集合视图的行能对上编目里的帧号与评级。
        let rows = state.lib_collection_rows(0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].index, 2);
        let out = state
            .run_ui_command(&ctx, "collection.delete", &json!({"index": 1}))
            .expect("删集合");
        assert_eq!(out["deleted"], json!("精选"));
        assert!(state.lib_cols.is_empty());

        // 越界与坏参都要报错。
        let err = state
            .run_ui_command(&ctx, "library.remove", &json!({"index": 2}))
            .unwrap_err();
        assert!(err.contains("1..=1"), "{err}");
        let err = state
            .run_ui_command(&ctx, "library.add", &json!({"path": "不存在的目录"}))
            .unwrap_err();
        assert!(err.contains("不是目录"), "{err}");

        // 从图库打开序列：与 sequence.open 同一口径。
        let out = state
            .run_ui_command(&ctx, "library.open", &json!({"index": 1}))
            .expect("从图库打开");
        assert_eq!(out["opened"], json!(true));
        assert_eq!(out["frames"], json!(3));
        assert_eq!(state.frame_count(), 3);
        // 打开序列时关键词也读进来，序列这边保存工程不会把它们写丢。
        assert_eq!(state.keywords.get(&1), Some(&vec!["日出".to_owned()]));

        // 移除目录：清单落盘，编目跟着清。
        let out = state
            .run_ui_command(&ctx, "library.remove", &json!({"index": 1}))
            .expect("移除目录");
        assert_eq!(out["roots"], json!(0));
        assert_eq!(state.lib_sel, None);
        assert_eq!(state.lib_entries.len(), 0);

        unsafe { std::env::remove_var("AEVUM_LIBRARY") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 图库堆叠：并叠 / 拆叠 / 按文件名自动分叠都写回该目录的工程文件，评级与关键词
    /// 一律不动；这个目录正好开着序列时内存那份也得跟上，否则自动保存会用旧值盖回去。
    #[test]
    fn library_stack_commands_fold_and_unfold() {
        let _env = LIBRARY_ENV
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = temp_dir("library_stack");
        let store = dir.join("galleries.json");
        unsafe { std::env::set_var("AEVUM_LIBRARY", store.as_os_str()) };
        let mut roots = dir.clone();
        roots.push("seq");
        std::fs::create_dir_all(&roots).unwrap();
        for name in [
            "A001_0001.png",
            "A001_0002.png",
            "A001_0003.png",
            "B002_0001.png",
            "B002_0002.png",
            "单张.png",
        ] {
            image::RgbImage::from_pixel(8, 6, image::Rgb([10, 20, 30]))
                .save(roots.join(name))
                .expect("写测试帧");
        }
        std::fs::create_dir_all(aevum_core::project::guard_dir(&roots)).unwrap();
        let mut proj = aevum_core::project::ProjectFile::default();
        proj.ratings.insert(0, 5);
        proj.keywords.insert(1, vec!["日出".to_owned()]);
        aevum_core::project::save(&aevum_core::project::project_path(&roots), &proj)
            .expect("写工程");

        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "library.add",
                &json!({"path": roots.display().to_string()}),
            )
            .expect("图库加目录");

        // 没叠：list 是空表，编目行也没有叠号。
        let out = state
            .run_ui_command(&ctx, "library.stack", &json!({}))
            .expect("查堆叠");
        assert_eq!(out["op"], json!("list"));
        assert_eq!(out["count"], json!(0));
        assert!(state.lib_entries[0].iter().all(|f| f.stack.is_none()));

        // 并叠：帧号 1 开始，返回整份堆叠；编目行立刻标上叠号。
        let out = state
            .run_ui_command(
                &ctx,
                "library.stack",
                &json!({"op": "create", "frames": [1, 2]}),
            )
            .expect("并叠");
        assert_eq!(out["count"], json!(1));
        assert_eq!(
            out["stacks"][0],
            json!({"stack": 1, "head": 1, "count": 2, "frames": [1, 2]})
        );
        assert_eq!(state.lib_entries[0][0].stack, Some(0));
        assert_eq!(state.lib_entries[0][1].stack, Some(0));
        assert_eq!(state.lib_entries[0][2].stack, None, "没勾的不进叠");
        let reloaded =
            aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
        assert_eq!(reloaded.stacks, vec![vec![0, 1]]);
        assert_eq!(reloaded.ratings.get(&0), Some(&5), "评级不动");
        assert_eq!(reloaded.keywords.get(&1).unwrap(), &["日出"], "关键词不动");

        // 跨叠搬帧：老叠被摘剩一张就消失，新叠按头帧排回去。
        let out = state
            .run_ui_command(
                &ctx,
                "library.stack",
                &json!({"op": "create", "frames": [2, 4]}),
            )
            .expect("跨叠并叠");
        assert_eq!(
            out["stacks"],
            json!([{"stack": 1, "head": 2, "count": 2, "frames": [2, 4]}])
        );
        assert_eq!(state.lib_entries[0][0].stack, None, "被摘走的帧回到散张");
        assert_eq!(state.lib_entries[0][3].stack, Some(0));

        // 拆叠：叠号越界当没这叠，不报错也不改盘。
        let out = state
            .run_ui_command(
                &ctx,
                "library.stack",
                &json!({"op": "dissolve", "stack": 9}),
            )
            .expect("越界叠号");
        assert_eq!(out["count"], json!(1));
        let out = state
            .run_ui_command(
                &ctx,
                "library.stack",
                &json!({"op": "dissolve", "stack": 1}),
            )
            .expect("拆叠");
        assert_eq!(out["count"], json!(0));
        assert!(state.lib_entries[0].iter().all(|f| f.stack.is_none()));

        // 自动分叠：去扩展名与结尾数字串后同前缀的归一叠，单张不成叠。
        let out = state
            .run_ui_command(&ctx, "library.stack", &json!({"op": "auto"}))
            .expect("自动分叠");
        assert_eq!(out["count"], json!(2));
        assert_eq!(out["stacks"][0]["frames"], json!([1, 2, 3]));
        assert_eq!(out["stacks"][1]["frames"], json!([4, 5]));
        assert_eq!(
            state.lib_entries[0][5].stack, None,
            "「单张」没前缀，不成叠"
        );

        // 这个目录开着序列：工程读进来的堆叠与命令改完的堆叠是同一份。
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": roots.display().to_string()}),
            )
            .expect("打开序列");
        assert_eq!(state.stacks, vec![vec![0, 1, 2], vec![3, 4]]);
        let out = state
            .run_ui_command(
                &ctx,
                "library.stack",
                &json!({"op": "create", "frames": [1, 6]}),
            )
            .expect("开着序列时并叠");
        assert_eq!(
            out["stacks"],
            json!([
                {"stack": 1, "head": 1, "count": 2, "frames": [1, 6]},
                {"stack": 2, "head": 2, "count": 2, "frames": [2, 3]},
                {"stack": 3, "head": 4, "count": 2, "frames": [4, 5]},
            ]),
            "第 1 帧被摘走，老叠剩 2、3 两张；B002 那叠没碰"
        );
        assert_eq!(
            state.stacks,
            vec![vec![0, 5], vec![1, 2], vec![3, 4]],
            "内存那份跟上，自动保存才不会盖回去"
        );
        assert_eq!(
            state.project_snapshot().stacks,
            vec![vec![0, 5], vec![1, 2], vec![3, 4]]
        );

        // 界面上那条路：勾选优先，少于两张要拦住。
        state.lib_click(3, false, false);
        let err = state.lib_stack_targets().unwrap_err();
        assert!(err.contains("至少"), "{err}");
        state.lib_click(4, false, false);
        state.lib_click(5, true, false);
        assert_eq!(state.lib_stack_targets().expect("勾选并叠"), 2);
        assert_eq!(state.lib_entries[0][4].stack, state.lib_entries[0][5].stack);
        // 拆散目标行所在的那一叠：返回被放出来的张数。
        assert_eq!(state.lib_stack_dissolve_targets().expect("拆叠"), 2);
        let err = state.lib_stack_dissolve_targets().unwrap_err();
        assert!(err.contains("不在任何叠里"), "{err}");

        // 叠号一改，旧的展开集就对不上号：一律收回折叠态。
        state.lib_stack_open.insert((0, 0));
        state.lib_stack_auto(0).expect("再自动分叠");
        assert!(state.lib_stack_open.is_empty(), "展开集该清掉");

        for bad in [
            json!({"op": "nope"}),
            json!({"op": 3}),
            json!({"op": "create", "frames": [1]}),
            json!({"op": "create", "frame": 1}),
            json!({"op": "create", "frames": [1, 9]}),
            json!({"op": "dissolve"}),
            json!({"op": "dissolve", "stack": 0}),
            json!({"root": 9}),
        ] {
            assert!(
                state.run_ui_command(&ctx, "library.stack", &bad).is_err(),
                "{bad}"
            );
        }
        let reloaded =
            aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
        assert_eq!(reloaded.ratings.get(&0), Some(&5), "坏参不落盘、也不动评级");

        unsafe { std::env::remove_var("AEVUM_LIBRARY") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mask_brush_command_replaces_strokes() {
        let dir = temp_dir("mask_brush");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");

        let added = state
            .run_ui_command(&ctx, "mask.add", &json!({"kind": "brush"}))
            .expect("加画笔蒙版");
        assert_eq!(added["masks"][0]["shape"], json!("brush"));

        // strokes 整组替换 + 局部参数，一条命令把蒙版画活：先涂后擦。
        let updated = state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({
                    "index": 1,
                    "params": {"exposure": 0.8},
                    "geometry": {"strokes": [
                        {"points": [[0.5, 0.5], [0.7, 0.5]], "radius": 0.1,
                         "hardness": 0.8, "flow": 1.0},
                        {"points": [[0.5, 0.5]], "radius": 0.05, "erase": true}
                    ]}
                }),
            )
            .expect("写笔画");
        assert_eq!(updated["masks"][0]["shapes"][0]["strokes"], json!(2));
        assert_eq!(updated["masks"][0]["shapes"][0]["kind"], json!("brush"));
        let parsed = mask::parse_mask(&state.params.masks[0]).expect("画笔蒙版解析得动");
        assert_eq!(parsed.components[0].shape, mask::ShapeKind::Brush);
        assert_eq!(parsed.components[0].strokes.len(), 2);
        assert!(parsed.components[0].strokes[1].erase);
        let mut m = parsed;
        m.build_field(40, 40, None, None);
        assert!(
            m.field[20 * 40 + 20] < 1e-6,
            "中心先涂后擦该是 0：{}",
            m.field[20 * 40 + 20]
        );
        assert!(m.field[20 * 40 + 27] > 0.9, "笔画末端还在");

        // 再发一次 strokes 是替换不是追加；缺省键按 core 默认补。
        state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({
                    "index": 1,
                    "geometry": {"strokes": [{"points": [[0.9, 0.9]]}]}
                }),
            )
            .expect("替换笔画");
        let parsed = mask::parse_mask(&state.params.masks[0]).unwrap();
        assert_eq!(parsed.components[0].strokes.len(), 1);
        assert_eq!(parsed.components[0].strokes[0].points.len(), 1);
        assert!(
            (parsed.components[0].strokes[0].radius - 0.06).abs() < 1e-9,
            "缺省半径 6%"
        );

        // 坏笔画要报错且不落盘：未知字段、点不是 [x, y]。
        let err = state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({
                    "index": 1,
                    "geometry": {"strokes": [{"points": [[0.5, 0.5]], "nope": 1}]}
                }),
            )
            .unwrap_err();
        assert!(err.contains("nope"), "{err}");
        let err = state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({"index": 1, "geometry": {"strokes": [{"points": [[0.5]]}]}}),
            )
            .unwrap_err();
        assert!(err.contains("[x, y]"), "{err}");
        let parsed = mask::parse_mask(&state.params.masks[0]).unwrap();
        assert_eq!(parsed.components[0].strokes.len(), 1, "失败的替换不落盘");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mask_component_boolean_and_luminance_range() {
        let dir = temp_dir("mask_comp");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        state
            .run_ui_command(&ctx, "mask.add", &json!({"kind": "radial"}))
            .expect("加蒙版");

        // 组里再加一个线性形状，并自动选中新形状。
        let added = state
            .run_ui_command(
                &ctx,
                "mask.component",
                &json!({"index": 1, "kind": "linear"}),
            )
            .expect("加形状");
        assert_eq!(added["add"], json!(2));
        assert_eq!(added["masks"][0]["components"], json!(2));
        assert_eq!(added["component"], json!(2));
        assert_eq!(state.mask_comp, 1);
        assert!(mask::parse_mask(&state.params.masks[0]).is_some());

        // 第二个形状用「减去」挖掉一块，几何只改它自己。
        let updated = state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({
                    "index": 1,
                    "component": 1,
                    "geometry": {"blend": "subtract", "x1": 0.1, "feather": 30},
                    "range": {"lum_on": true, "lum_min": 0.2, "lum_max": 0.9, "lum_feather": 5}
                }),
            )
            .expect("改形状与亮度范围");
        let parsed = mask::parse_mask(&state.params.masks[0]).expect("仍可解析");
        assert_eq!(parsed.components[0].blend, mask::Blend::Add);
        assert_eq!(parsed.components[1].blend, mask::Blend::Subtract);
        assert_eq!(parsed.components[1].x1, 0.1);
        assert_eq!(parsed.components[0].x1, 0.5, "别把第一个形状也改了");
        let range = parsed.range.as_ref().expect("亮度范围已存盘");
        assert!(range.on);
        assert_eq!(range.min, 0.2);
        assert_eq!(range.max, 0.9);
        assert_eq!(range.feather, 1.0, "超出 0~1 的羽化按上限收");
        assert_eq!(updated["masks"][0]["range"]["lum_on"], json!(true));
        assert_eq!(
            updated["component"],
            json!(2),
            "mask.update 把编辑目标停在 component 1"
        );

        // 反常识参数：越界 component、未知 range 字段、越界 blend 都要报错且不落盘。
        let err = state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({"index": 1, "component": 2, "amount": 50}),
            )
            .unwrap_err();
        assert!(err.contains("component 超出范围"), "{err}");
        let err = state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({"index": 1, "range": {"lum_min": 0.3, "bogus": 1}}),
            )
            .unwrap_err();
        assert!(err.contains("bogus"), "{err}");
        assert_eq!(
            mask::parse_mask(&state.params.masks[0])
                .expect("坏请求没写坏蒙版")
                .amount,
            100.0
        );

        // 颜色范围：hue_* 一组键落进同一个 range 对象，亮度键不丢。
        state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({"index": 1, "range": {"hue_on": true, "hue": 210,
                        "hue_width": 70, "hue_feather": 25, "hue_sat": 0.1}}),
            )
            .expect("写颜色范围");
        let parsed = mask::parse_mask(&state.params.masks[0]).expect("仍可解析");
        let hue = parsed.hue.as_ref().expect("颜色范围已存盘");
        assert!(hue.on);
        assert_eq!(hue.hue, 210.0);
        assert_eq!(hue.width, 70.0);
        assert_eq!(hue.feather, 25.0);
        assert_eq!(hue.sat_min, 0.1);
        let range = parsed.range.as_ref().expect("亮度范围还在");
        assert!(range.on, "颜色范围落盘不能抹掉亮度范围");
        assert_eq!(range.min, 0.2);
        // 只给部分 hue 键：缺的按已存/默认补齐，不开 on。
        state
            .run_ui_command(
                &ctx,
                "mask.update",
                &json!({"index": 1, "range": {"hue": 30}}),
            )
            .expect("只改目标色相");
        let parsed = mask::parse_mask(&state.params.masks[0]).expect("仍可解析");
        let hue = parsed.hue.as_ref().expect("颜色范围还在");
        assert_eq!(hue.hue, 30.0);
        assert!(hue.on, "已开启的颜色范围不许被单键更新关掉");
        assert_eq!(hue.width, 70.0);

        // 删形状：删到只剩一个之后不许再删，整张蒙版请用 mask.remove。
        let removed = state
            .run_ui_command(
                &ctx,
                "mask.component",
                &json!({"index": 1, "action": "remove", "component": 1}),
            )
            .expect("删形状");
        assert_eq!(removed["remove"], json!(2));
        assert_eq!(removed["masks"][0]["components"], json!(1));
        let err = state
            .run_ui_command(
                &ctx,
                "mask.component",
                &json!({"index": 1, "action": "remove", "component": 0}),
            )
            .unwrap_err();
        assert!(err.contains("只剩一个形状"), "{err}");
        assert_eq!(
            mask::parse_mask(&state.params.masks[0])
                .expect("仍是单组")
                .components
                .len(),
            1
        );

        // 非法 kind / action 走清晰报错，不给半截状态。
        assert!(
            state
                .run_ui_command(
                    &ctx,
                    "mask.component",
                    &json!({"index": 1, "kind": "banana"})
                )
                .unwrap_err()
                .contains("banana")
        );
        assert!(
            state
                .run_ui_command(
                    &ctx,
                    "mask.component",
                    &json!({"index": 1, "action": "invert"})
                )
                .unwrap_err()
                .contains("未知动作")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queue_commands_add_dedupe_and_guard_run() {
        let dir = temp_dir("queue");
        let first = dir.join("first");
        let second = dir.join("second");
        for root in [&first, &second] {
            std::fs::create_dir_all(root).expect("建序列目录");
            for i in 0..2u8 {
                let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
                img.save(root.join(format!("f{i:03}.png")))
                    .expect("写测试帧");
            }
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        // 别碰真实漫游配置里的 render_queue.json。
        state.queue_path = dir.join("render_queue.json");
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": first.display().to_string()}),
            )
            .expect("打开序列");

        // 不带参数：当前序列 + 默认成片名。
        let added = state
            .run_ui_command(&ctx, "queue.add", &json!({}))
            .expect("加入当前序列");
        assert_eq!(added["added"], json!(1));
        assert_eq!(added["count"], json!(1));
        let item = &added["items"][0];
        assert_eq!(item["root"], json!(first.display().to_string()));
        assert!(
            item["out"].as_str().unwrap().ends_with("first_成片.mp4"),
            "{}",
            item["out"]
        );
        assert_eq!(item["status"], json!("待渲"));

        // 另一条序列：显式规格；再次加入同一目录是更新而不是新增。
        let out = dir.join("second.mp4");
        state
            .run_ui_command(
                &ctx,
                "queue.add",
                &json!({
                    "root": second.display().to_string(),
                    "out": out.display().to_string(),
                    "width": 1920, "height": 1080, "fps": 30,
                    "codec": "hevc", "crf": 20, "preset": "slow",
                    "keep_frames": true,
                }),
            )
            .expect("加入第二段");
        let dedup = state
            .run_ui_command(
                &ctx,
                "queue.add",
                &json!({"root": first.display().to_string()}),
            )
            .expect("重复加入");
        assert_eq!(dedup["count"], json!(2));
        // 重复加入会挪到队尾（与 Python 版一致），所以这一段成了第 2 项。
        assert_eq!(dedup["added"], json!(2));
        assert_eq!(
            dedup["items"][1]["root"],
            json!(first.display().to_string())
        );
        let second_item = &dedup["items"][0];
        assert_eq!(second_item["codec"], json!("h265"));
        assert_eq!(second_item["width"], json!(1920));
        assert_eq!(second_item["keep_frames"], json!(true));

        for (args, needle) in [
            (json!({"width": 0}), "同时"),
            (json!({"width": 1921, "height": 1080}), "偶数"),
            (json!({"fps": 0}), "fps"),
            (json!({"crf": 52}), "crf"),
            (json!({"codec": "av1"}), "h264"),
            (json!({"preset": "turbo"}), "预设"),
            (
                json!({"root": dir.join("missing").display().to_string()}),
                "不存在",
            ),
        ] {
            let err = state.run_ui_command(&ctx, "queue.add", &args).unwrap_err();
            assert!(err.contains(needle), "{args}：{err}");
        }

        // 批量写操作必须确认；无界面模式不能自己跑队列。
        let err = state
            .run_ui_command(&ctx, "queue.run", &json!({}))
            .unwrap_err();
        assert!(err.contains("confirm"), "{err}");
        state.headless = true;
        let err = state
            .run_ui_command(&ctx, "queue.run", &json!({"confirm": true}))
            .unwrap_err();
        assert!(err.contains("无界面"), "{err}");
        state.headless = false;

        let err = state
            .run_ui_command(&ctx, "queue.remove", &json!({"index": 3}))
            .unwrap_err();
        assert!(err.contains("1..=2"), "{err}");
        let removed = state
            .run_ui_command(&ctx, "queue.remove", &json!({"index": 2}))
            .expect("移除排在队尾的第一段");
        assert_eq!(removed["items"], json!(1));
        let status = state
            .run_ui_command(&ctx, "queue.status", &json!({}))
            .expect("队列状态");
        assert_eq!(status["pending"], json!(1));
        assert_eq!(status["busy"], json!(false));
        assert!(status["progress"].is_null());

        // 队列变化立即落盘，换一次启动也能读回。
        let on_disk = queue::load(&state.queue_path);
        assert_eq!(on_disk.len(), 1);
        assert_eq!(on_disk[0].root, second.display().to_string());

        let cleared = state
            .run_ui_command(&ctx, "queue.clear", &json!({}))
            .expect("清空队列");
        assert_eq!(cleared["cleared"], json!(1));
        assert!(queue::load(&state.queue_path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn xmp_import_and_export_roundtrip() {
        const PRESET: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:Version="17.3.1"
    crs:Exposure2012="1.25"
    crs:Contrast2012="18"
    crs:Vibrance="30">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

        let dir = temp_dir("xmp");
        let preset = dir.join("Look.xmp");
        std::fs::write(&preset, PRESET).expect("写测试预设");
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .params
            .masks
            .push(mask::new_mask(mask::ShapeKind::Radial));
        state.mask_sel = Some(0);

        let imported = state
            .run_ui_command(
                &ctx,
                "xmp.import",
                &json!({"path": preset.display().to_string()}),
            )
            .expect("导入预设");
        assert_eq!(imported["imported"], json!("Look"));
        assert_eq!(state.preset, "Look");
        assert!((state.params.exposure - 1.25).abs() < 1e-12);
        assert!((state.params.contrast - 18.0).abs() < 1e-12);
        assert!((state.params.vibrance - 30.0).abs() < 1e-12);
        // 预设之外没有的键回到默认；蒙版不属于 XMP 模型，随默认清空。
        assert!(state.params.masks.is_empty());
        assert_eq!(state.mask_sel, None);

        let err = state
            .run_ui_command(
                &ctx,
                "xmp.export",
                &json!({"path": dir.join("bad.txt").display().to_string()}),
            )
            .unwrap_err();
        assert!(err.contains(".xmp"), "{err}");

        let out = dir.join("out.xmp");
        state
            .run_ui_command(
                &ctx,
                "xmp.export",
                &json!({"path": out.display().to_string()}),
            )
            .expect("导出预设");
        assert!(out.is_file());
        let back = xmp::read(&out).expect("读回预设");
        assert!((back.exposure - 1.25).abs() < 1e-9);
        assert!((back.contrast - 18.0).abs() < 1e-9);

        let err = state
            .run_ui_command(
                &ctx,
                "xmp.export",
                &json!({"path": out.display().to_string()}),
            )
            .unwrap_err();
        assert!(err.contains("confirm"), "{err}");
        state
            .run_ui_command(
                &ctx,
                "xmp.export",
                &json!({"path": out.display().to_string(), "confirm": true}),
            )
            .expect("确认后覆盖");

        let err = state
            .run_ui_command(
                &ctx,
                "xmp.import",
                &json!({"path": dir.join("nope.xmp").display().to_string()}),
            )
            .unwrap_err();
        assert!(err.contains("不存在"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cube_look_export_and_import_roundtrip() {
        let dir = temp_dir("cube");
        let ctx = Context::default();
        let mut state = AppState::new();

        state
            .run_ui_command(
                &ctx,
                "grade.set",
                &json!({"exposure": 0.6, "contrast": 20.0}),
            )
            .expect("设置参数");
        let out = dir.join("look.cube");
        let exported = state
            .run_ui_command(
                &ctx,
                "cube.export",
                &json!({"path": out.display().to_string(), "size": 17}),
            )
            .expect("导出 .cube");
        assert_eq!(exported["size"], json!(17));
        assert_eq!(exported["nodes"], json!(17 * 17 * 17));
        assert!(out.is_file(), "导出的 .cube 没落盘");

        let err = state
            .run_ui_command(
                &ctx,
                "cube.export",
                &json!({"path": dir.join("bad.txt").display().to_string()}),
            )
            .unwrap_err();
        assert!(err.contains(".cube"), "{err}");
        let err = state
            .run_ui_command(
                &ctx,
                "cube.export",
                &json!({"path": out.display().to_string(), "size": 17}),
            )
            .unwrap_err();
        assert!(err.contains("confirm"), "{err}");
        let err = state
            .run_ui_command(
                &ctx,
                "cube.export",
                &json!({"path": out.display().to_string(), "size": 512, "confirm": true}),
            )
            .unwrap_err();
        assert!(err.contains("256"), "{err}");

        // 挂上：默认 apply，参数与预设都要跟着改，否则界面表现为"改了没反应"。
        let imported = state
            .run_ui_command(
                &ctx,
                "cube.import",
                &json!({"path": out.display().to_string()}),
            )
            .expect("导入外观");
        assert_eq!(imported["applied"], json!(true));
        assert_eq!(imported["size"], json!(17));
        assert_eq!(imported["title"], json!("grade"));
        assert_eq!(
            imported["neutral"],
            json!(false),
            "曝光 0.6 的表不该被判成恒等"
        );
        assert_eq!(state.params.look_cube, out.display().to_string());
        assert_eq!(state.preset, "自定义");
        assert!(state.preview_dirty);

        // 只体检不改参数。
        let peeked = state
            .run_ui_command(
                &ctx,
                "cube.import",
                &json!({"path": out.display().to_string(), "apply": false}),
            )
            .expect("只解析");
        assert_eq!(peeked["applied"], json!(false));

        // 路径改动当场校验：坏文件不能拖到渲染时才静默退化成"没挂"。
        let err = state
            .run_ui_command(
                &ctx,
                "grade.set",
                &json!({"look_cube": dir.join("nope.cube").display().to_string()}),
            )
            .unwrap_err();
        assert!(err.contains("读不出来"), "{err}");
        let junk = dir.join("junk.cube");
        std::fs::write(&junk, "LUT_3D_SIZE 17\n0.0 0.0 0.0\n").expect("写坏表");
        let err = state
            .run_ui_command(
                &ctx,
                "cube.import",
                &json!({"path": junk.display().to_string()}),
            )
            .unwrap_err();
        assert!(err.contains("读 .cube"), "{err}");

        state
            .run_ui_command(&ctx, "grade.set", &json!({"look_cube": ""}))
            .expect("摘掉外观");
        assert_eq!(state.params.look_cube, "");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_config_updates_options_and_validates() {
        let dir = temp_dir("audio");
        let music = dir.join("score.wav");
        std::fs::write(&music, b"RIFF-not-really-a-wave").unwrap();
        let ctx = Context::default();
        let mut state = AppState::new();
        state.headless = true;

        let out = state
            .run_ui_command(
                &ctx,
                "audio.config",
                &json!({
                    "path": music.display().to_string(),
                    "delay": 1.5,
                    "gain": 80.0,
                    "fade_in": 2.0,
                    "fade_out": 3.0,
                    "loop": true,
                    "trim": false,
                    "acodec": "aac192",
                }),
            )
            .expect("配置配乐");
        assert_eq!(state.render_audio.as_deref(), Some(music.as_path()));
        assert!((state.render_audio_opts.delay - 1.5).abs() < 1e-12);
        assert!((state.render_audio_opts.gain - 80.0).abs() < 1e-12);
        assert!((state.render_audio_opts.fade_in - 2.0).abs() < 1e-12);
        assert!((state.render_audio_opts.fade_out - 3.0).abs() < 1e-12);
        assert!(state.render_audio_opts.looped);
        assert!(!state.render_audio_opts.trim);
        assert_eq!(state.render_audio_opts.acodec, AudioCodec::Aac192);
        assert_eq!(out["options"]["acodec"], json!("aac192"));
        assert_eq!(out["options"]["loop"], json!(true));

        std::fs::write(dir.join("note.txt"), b"x").unwrap();
        for (args, needle) in [
            (json!({"acodec": "flac"}), "acodec"),
            (json!({"delay": -1.0}), "delay"),
            (json!({"gain": 900.0}), "gain"),
            (
                json!({"path": dir.join("missing.wav").display().to_string()}),
                "不存在",
            ),
            (
                json!({"path": dir.join("note.txt").display().to_string()}),
                "扩展名",
            ),
        ] {
            let err = state
                .run_ui_command(&ctx, "audio.config", &args)
                .unwrap_err();
            assert!(err.contains(needle), "{args}：{err}");
        }

        // 空参数只查询，不改设置。
        let status = state
            .run_ui_command(&ctx, "audio.config", &json!({}))
            .expect("查询配乐");
        assert_eq!(status["path"], json!(music.display().to_string()));
        assert!(status["total_seconds"].is_null());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_module_accepts_names_and_indexes() {
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(&ctx, "ui.module", &json!({"name": "成片"}))
            .expect("中文模块名");
        assert_eq!(state.module, Module::Render);
        state
            .run_ui_command(&ctx, "ui.module", &json!({"name": "1"}))
            .expect("序号");
        assert_eq!(state.module, Module::Deflicker);
        let err = state
            .run_ui_command(&ctx, "ui.module", &json!({"name": "不存在"}))
            .unwrap_err();
        assert!(err.contains("未知模块"), "{err}");
    }

    #[test]
    fn ui_dock_opens_the_requested_panel() {
        let ctx = Context::default();
        let mut state = AppState::new();
        state.show_dock = false;
        let opened = state
            .run_ui_command(&ctx, "ui.dock", &json!({"name": "AI"}))
            .expect("中文面板名");
        assert_eq!(state.dock_tab, DockTab::Ai);
        assert!(state.show_dock);
        assert_eq!(opened["index"], 3);
        state
            .run_ui_command(&ctx, "ui.dock", &json!({"name": "1"}))
            .expect("序号");
        assert_eq!(state.dock_tab, DockTab::Info);
        let err = state
            .run_ui_command(&ctx, "ui.dock", &json!({"name": "不存在"}))
            .unwrap_err();
        assert!(err.contains("未知面板"), "{err}");
    }

    /// 分通道去闪要有线性测光才有意义；开了之后偏移必须拆成"曝光 + 色残差"，
    /// 关掉要逐位退回单亮度口径。
    #[test]
    fn per_channel_deflicker_splits_exposure_and_color_residual() {
        let ctx = Context::default();
        let mut state = AppState::demo();
        let n = state.metrics.len();
        state.def_means.clear();
        state
            .run_ui_command(&ctx, "deflicker.set", &json!({"channels": true}))
            .expect("切分通道");
        assert!(
            state.corr_rgb.iter().all(|c| *c == [0.0; 3]),
            "没有线性测光时不该硬编出色闪残差"
        );

        // 每 7 帧一次绿色通道跳变：正是亮度单通道压不住的彩色闪烁。
        state.def_means = (0..n)
            .map(|i| {
                let kick = if i % 7 == 0 { 0.06 } else { 0.0 };
                Some(FrameMeans {
                    luma: 0.2,
                    rgb: [0.12, 0.1 + kick, 0.08],
                })
            })
            .collect();
        state.recompute_deflicker();
        let a = state.analysis.clone().expect("分析过了");
        assert!(a.stats.channels, "统计要说这是分通道结果");
        let hot = state.corr_rgb[7];
        assert!(
            hot[1].abs() > 1e-4,
            "跳变帧的绿通道该被补回来，实得 {hot:?}"
        );
        for (i, c) in state.corr_rgb.iter().enumerate() {
            for (k, v) in c.iter().enumerate() {
                let want = a.offset(i, k) as f32;
                assert!(
                    (state.corr[i] + v - want).abs() < 1e-6,
                    "[{i}][{k}] 曝光 + 残差没还原通道偏移"
                );
            }
        }
        assert_eq!(state.ev_at(7).chan, hot, "预览取的就是这份合成");

        state
            .run_ui_command(&ctx, "deflicker.set", &json!({"channels": false}))
            .expect("关分通道");
        assert!(!state.analysis.as_ref().unwrap().stats.channels);
        assert!(
            state.corr_rgb.iter().all(|c| *c == [0.0; 3]),
            "关掉后必须逐帧只剩亮度"
        );
    }

    /// 演示序列自带两种闪烁：每 7 帧整体掉档（亮度）+ 每 5 帧偏绿（色闪）。
    /// 开分通道必须能看到色闪被压下去，否则这个开关在验收时无从判断。
    #[test]
    fn demo_sequence_exercises_the_channel_toggle() {
        let mut state = AppState::demo();
        let plain = state.analysis.clone().unwrap().stats;
        assert!(!plain.channels);
        assert!(plain.jitter_after < plain.jitter_before, "亮度闪烁该被压住");
        state.def_channels = true;
        state.recompute_deflicker();
        let st = state.analysis.clone().unwrap().stats;
        assert!(st.channels && st.color_before > 0.0);
        assert!(
            st.color_after < st.color_before,
            "分通道该压住色闪：{} → {}",
            st.color_before,
            st.color_after
        );
        assert!(
            state.corr_rgb.iter().any(|c| c.iter().any(|v| *v != 0.0)),
            "演示序列开了分通道就该有残差"
        );
    }

    #[test]
    fn commands_without_a_sequence_return_readable_errors() {
        let ctx = Context::default();
        let mut state = AppState::new();
        for (id, args) in [
            ("sequence.info", json!({})),
            ("frame.select", json!({"index": 1})),
            ("project.get", json!({})),
            ("project.metadata", json!({})),
        ] {
            let err = state.run_ui_command(&ctx, id, &args).unwrap_err();
            assert!(err.contains("还没有打开序列"), "{id}: {err}");
        }
    }

    #[test]
    fn project_metadata_command_saves_and_reaches_export() {
        let dir = temp_dir("meta");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        let out = state
            .run_ui_command(
                &ctx,
                "project.metadata",
                &json!({
                    "title": "云海日出",
                    "description": "间隔 3s\n牛背山 东平台",
                    "creator": "张三",
                    "copyright": "© 2026 张三",
                }),
            )
            .expect("写信息");
        assert_eq!(out["saved"], json!(true));
        assert_eq!(
            out["description"],
            json!("间隔 3s 牛背山 东平台"),
            "换行折成空格"
        );
        assert_eq!(
            out["path"],
            json!(project::project_path(&dir).display().to_string())
        );
        let on_disk = project::load(&project::project_path(&dir)).expect("读工程");
        assert_eq!(on_disk.metadata.title, "云海日出");
        assert_eq!(on_disk.metadata.creator, "张三");
        assert_eq!(on_disk.metadata.copyright, "© 2026 张三");
        // 导出选项要带上这一份，静帧的 EXIF 与成片的容器标签才有得写。
        assert_eq!(state.frame_render_options().metadata, on_disk.metadata);
        assert_eq!(state.project_snapshot().metadata, on_disk.metadata);
        // 空串清空，四项都空就是没填。
        let out = state
            .run_ui_command(
                &ctx,
                "project.metadata",
                &json!({"title": null, "description": "", "creator": "", "copyright": ""}),
            )
            .expect("清信息");
        assert_eq!(out["filled"], json!(false));
        let on_disk = project::load(&project::project_path(&dir)).expect("读工程");
        assert_eq!(on_disk.metadata, project::Metadata::default());
        assert!(
            state
                .run_ui_command(&ctx, "project.metadata", &json!({"title": 3}))
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_commands_are_journaled_without_recording_reads_or_failures() {
        let dir = temp_dir("journal");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        let journal_file = aevum_core::journal::path(&dir);
        assert!(!journal_file.exists(), "只读命令不该写日志");

        state
            .run_ui_command(&ctx, "grade.set", &json!({"params": {"exposure": 0.4}}))
            .expect("调色");
        let err = state
            .run_ui_command(&ctx, "grade.set", &json!({"params": {}}))
            .unwrap_err();
        assert!(err.contains("没有要设置的调色字段"), "{err}");

        let listed = state
            .run_ui_command(&ctx, "journal.list", &json!({"limit": 20}))
            .expect("读日志");
        assert_eq!(listed["path"], json!(journal_file.display().to_string()));
        let entries = listed["entries"].as_array().expect("日志数组");
        assert_eq!(entries.len(), 1, "失败与只读命令都不该进日志：{listed}");
        assert_eq!(entries[0]["source"], "gui");
        assert_eq!(entries[0]["op"], "grade.set");
        assert_eq!(entries[0]["detail"]["params"]["exposure"], 0.4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_catalog_command_reaches_the_executor() {
        // 命令清单和执行器的 match 是两份代码；这条测试防止只加了一边。
        let ctx = Context::default();
        let mut state = AppState::new();
        for spec in crate::control::command_catalog() {
            if let Err(e) = state.run_ui_command(&ctx, spec.id, &json!({})) {
                assert!(!e.starts_with("未知命令"), "{} 没接上执行器：{e}", spec.id);
            }
        }
    }

    /// 取景框命令：写框、换比例、取消、撤销走的是画布上同一套几何。
    #[test]
    fn crop_set_writes_rect_aspect_and_undoes() {
        let dir = temp_dir("crop");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(64, 48, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        // 分辨率是预览解码回来才回填的，单测里没有那条异步链，直接给定。
        state.resolution = Some((64, 48));

        let out = state
            .run_ui_command(
                &ctx,
                "crop.set",
                &json!({"x": 8, "y": 6, "width": 32, "height": 24}),
            )
            .expect("写取景框");
        assert_eq!(
            out["manual"],
            json!({"x": 8, "y": 6, "width": 32, "height": 24})
        );
        assert_eq!(
            out["effective"], out["manual"],
            "原生比例下取景框就是用户框"
        );

        // 换方形：已有框绕自己的中心重铺成画面里放得下的最大方形。
        let out = state
            .run_ui_command(&ctx, "crop.set", &json!({"aspect": "1:1"}))
            .expect("换画幅");
        assert_eq!(out["aspect"], json!("1:1"));
        assert_eq!(
            out["manual"],
            json!({"x": 8, "y": 2, "width": 32, "height": 32})
        );

        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤画幅");
        assert_eq!(state.aspect, Aspect::Native);
        assert_eq!(
            state.user_crop.map(|c| (c.x, c.y, c.w, c.h)),
            Some((8, 6, 32, 24))
        );

        let out = state
            .run_ui_command(&ctx, "crop.set", &json!({"reset": true}))
            .expect("取消裁剪");
        assert_eq!(out["manual"], json!(null));
        assert_eq!(state.user_crop, None);

        // 空参数是读回，不该占一步历史。
        let steps = state.history.steps().len();
        let out = state
            .run_ui_command(&ctx, "crop.set", &json!({}))
            .expect("读回取景");
        assert_eq!(out["frame"], json!({"width": 64, "height": 48}));
        assert_eq!(state.history.steps().len(), steps, "只读命令不记步");

        // 半套坐标和认不出的比例都得报错，不能静默画个歪框。
        let err = state
            .run_ui_command(&ctx, "crop.set", &json!({"x": 4, "width": 10}))
            .unwrap_err();
        assert!(err.contains("x / y / width / height"), "{err}");
        let err = state
            .run_ui_command(&ctx, "crop.set", &json!({"aspect": "banana"}))
            .unwrap_err();
        assert!(err.contains("画幅比例"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 输出设置命令：容器、质量、EXIF、锐化、色域、规格一处写定，撤销走同一份快照。
    #[test]
    fn render_set_writes_the_output_knobs_and_undoes() {
        let dir = temp_dir("render");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(64, 48, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        state.resolution = Some((64, 48));

        // 空参数是读回，默认是视频 + PNG 无损档。
        let out = state
            .run_ui_command(&ctx, "render.set", &json!({}))
            .expect("读回输出设置");
        assert_eq!(out["mode"], json!("video"));
        assert_eq!(out["format"], json!("png8"));
        assert_eq!(out["quality_applies"], json!(false));
        assert_eq!(out["sharpen_for"], json!("off"));

        // 视频模式的中间帧只能无损，有损容器现在就撞回。
        let err = state
            .run_ui_command(&ctx, "render.set", &json!({"format": "jpeg"}))
            .unwrap_err();
        assert!(err.contains("只能出静帧"), "{err}");

        let out = state
            .run_ui_command(
                &ctx,
                "render.set",
                &json!({
                    "mode": "stills",
                    "format": "jpeg",
                    "quality": 72,
                    "keep_exif": true,
                    "xmp_sidecar": true,
                    "sharpen_for": "screen",
                    "sharpen_strength": "high",
                    "color_space": "p3",
                    "width": 1920,
                    "height": 1080,
                }),
            )
            .expect("改成 JPEG 成片");
        assert_eq!(out["format"], json!("jpeg"));
        assert_eq!(out["quality"], json!(72));
        assert_eq!(out["quality_applies"], json!(true));
        assert_eq!(out["exif_applies"], json!(true));
        assert_eq!(out["xmp_sidecar"], json!(true));
        assert_eq!(out["sidecar_applies"], json!(true), "静帧模式该发旁车");
        assert_eq!(out["sharpen_for"], json!("screen"));
        assert_eq!(out["sharpen_strength"], json!("high"));
        assert_eq!(out["color_space"], json!("p3"));

        // WebP 是无损档，质量参数对它不起作用。
        let out = state
            .run_ui_command(&ctx, "render.set", &json!({"format": "webp"}))
            .expect("换 WebP");
        assert_eq!(out["quality_applies"], json!(false));

        // 切回视频：容器是有损档就整条命令拒绝，不留「视频模式 + WebP 中间帧」。
        let err = state
            .run_ui_command(&ctx, "render.set", &json!({"mode": "video"}))
            .unwrap_err();
        assert!(err.contains("只能出静帧"), "{err}");
        assert_eq!(
            state.render_mode,
            OutputMode::Stills,
            "被拒的命令不该改一半"
        );
        let out = state
            .run_ui_command(
                &ctx,
                "render.set",
                &json!({"mode": "video", "format": "png8"}),
            )
            .expect("换回视频 + PNG");
        assert_eq!(out["mode"], json!("video"));
        assert_eq!(out["format"], json!("png8"));
        assert_eq!(
            out["sidecar_applies"],
            json!(false),
            "视频模式的中间帧不发旁车"
        );

        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤回静帧");
        assert_eq!(state.render_format, StillFormat::WebP);
        assert_eq!(state.render_mode, OutputMode::Stills);
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤 JPEG");
        assert_eq!(state.render_format, StillFormat::Jpeg);
        assert_eq!(state.render_quality, 72);
        assert_eq!(state.render_sharpen_for, OutputSharpen::Screen);
        assert_eq!((state.output_w, state.output_h), (1920, 1080));
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤整套成片设置");
        assert_eq!(state.render_mode, OutputMode::Video);
        assert_eq!(state.render_format, StillFormat::Png8);
        assert_eq!(state.render_quality, 90);
        assert!(!state.render_keep_exif);
        assert!(!state.render_xmp_sidecar, "撤销要连旁车开关一起退回去");
        assert_eq!(state.render_sharpen_for, OutputSharpen::Off);
        assert_eq!((state.output_w, state.output_h), (3840, 2160));

        // 半套规格、越界质量、认不出的容器都要说话。
        let err = state
            .run_ui_command(&ctx, "render.set", &json!({"width": 1920}))
            .unwrap_err();
        assert!(err.contains("同时给出"), "{err}");
        let err = state
            .run_ui_command(&ctx, "render.set", &json!({"quality": 0}))
            .unwrap_err();
        assert!(err.contains("quality"), "{err}");
        let err = state
            .run_ui_command(&ctx, "render.set", &json!({"format": "banana"}))
            .unwrap_err();
        assert!(err.contains("未知容器"), "{err}");

        // 水印整块合并：只写 opacity 不许把正文清空，认不出的键要说话。
        let out = state
            .run_ui_command(
                &ctx,
                "render.set",
                &json!({
                    "watermark": {
                        "enabled": true,
                        "text": "© 恒晷",
                        "spot": "top-left",
                        "opacity": 70,
                    }
                }),
            )
            .expect("开水印");
        assert_eq!(out["watermark"]["enabled"], json!(true));
        assert_eq!(out["watermark"]["text"], json!("© 恒晷"));
        assert_eq!(out["watermark"]["spot"], json!("top-left"));
        assert_eq!(out["watermark_applies"], json!(true));
        assert_eq!(state.render_watermark.opacity, 70);
        assert_eq!(state.render_watermark.size, 4, "没写的键保持当前值");
        assert_eq!(state.render_watermark.margin, 2);
        assert_eq!(state.render_watermark.kind, WatermarkKind::Text);

        let out = state
            .run_ui_command(&ctx, "render.set", &json!({"watermark": {"opacity": 30}}))
            .expect("只改不透明度");
        assert_eq!(
            out["watermark"]["text"],
            json!("© 恒晷"),
            "只写一个键不该动正文"
        );
        assert_eq!(state.render_watermark.opacity, 30);

        let err = state
            .run_ui_command(
                &ctx,
                "render.set",
                &json!({"watermark": {"spot": "banana"}}),
            )
            .unwrap_err();
        assert!(err.contains("水印参数"), "{err}");
        let err = state
            .run_ui_command(&ctx, "render.set", &json!({"watermark": {"nope": 1}}))
            .unwrap_err();
        assert!(err.contains("未知的水印参数"), "{err}");
        let err = state
            .run_ui_command(
                &ctx,
                "render.set",
                &json!({"watermark": {"image": "E:/没有这张图.png"}}),
            )
            .unwrap_err();
        assert!(err.contains("水印图片不存在"), "{err}");
        let err = state
            .run_ui_command(&ctx, "render.set", &json!({"watermark": {"size": 90}}))
            .unwrap_err();
        assert!(err.contains("size"), "{err}");

        // 撤销：水印整块同进同退，不许只回一半。
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤改不透明度");
        assert_eq!(state.render_watermark.text, "© 恒晷");
        assert_eq!(state.render_watermark.opacity, 70);
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤开水印");
        assert_eq!(state.render_watermark, Watermark::default());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 变速：三种曲线写法、快门与读数走同一条命令通道；整块进撤销，坏参数不留半套。
    #[test]
    fn retime_set_takes_every_curve_shape_and_undoes() {
        let dir = temp_dir("retime");
        for i in 0..8u8 {
            let img = image::RgbImage::from_pixel(16, 12, image::Rgb([i * 8, 30, 60]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");

        // 没开变速：读数就是源帧数，帧率 24 下 8 帧 = 1/3 秒。
        let out = state
            .run_ui_command(&ctx, "retime.status", &json!({}))
            .expect("读变速");
        assert_eq!(out["enabled"], json!(false));
        assert_eq!(out["identity"], json!(true));
        assert_eq!(out["frames"], json!(8));
        assert!(
            (out["seconds"].as_f64().unwrap() - 8.0 / 24.0).abs() < 1e-6,
            "{}",
            out["seconds"]
        );

        let out = state
            .run_ui_command(&ctx, "retime.set", &json!({"speed": 2.0}))
            .expect("设倍速");
        assert_eq!(out["enabled"], json!(true), "给了曲线就自动开变速");
        assert_eq!(out["frames"], json!(4), "2× 把 8 帧压成 4 帧");
        assert_eq!(out["keys"].as_array().unwrap().len(), 2);

        let out = state
            .run_ui_command(&ctx, "retime.set", &json!({"shutter": 0.5}))
            .expect("设快门");
        assert_eq!(out["shutter"], json!(0.5));
        assert_eq!(out["frames"], json!(4), "快门只改混多少帧，不改输出帧数");

        let out = state
            .run_ui_command(
                &ctx,
                "retime.set",
                &json!({"keys": [
                    {"at": 0.0, "speed": 1.0},
                    {"at": 0.5, "speed": 4.0},
                    {"at": 1.0, "speed": 1.0},
                ]}),
            )
            .expect("写速度曲线");
        assert_eq!(out["keys"].as_array().unwrap().len(), 3);
        let frames = out["frames"].as_u64().unwrap();
        assert!((2..8).contains(&frames), "两头常速中段 4×：{frames} 帧");

        let out = state
            .run_ui_command(&ctx, "retime.set", &json!({"ease": {"peak": 4.0}}))
            .expect("缓入缓出");
        assert_eq!(out["keys"].as_array().unwrap().len(), 4);

        // 坏参数整条拒绝，不许留下半套曲线。
        let err = state
            .run_ui_command(
                &ctx,
                "retime.set",
                &json!({"speed": 2.0, "keys": [{"at": 0.0, "speed": 2.0}]}),
            )
            .unwrap_err();
        assert!(err.contains("只能选一种写法"), "{err}");
        let err = state
            .run_ui_command(&ctx, "retime.set", &json!({"speed": 0}))
            .unwrap_err();
        assert!(err.contains("倍速必须大于 0"), "{err}");
        let err = state
            .run_ui_command(&ctx, "retime.set", &json!({"shutter": 1.5}))
            .unwrap_err();
        assert!(err.contains("shutter"), "{err}");
        let err = state
            .run_ui_command(&ctx, "retime.set", &json!({"keys": [{"at": 0.5}]}))
            .unwrap_err();
        assert!(err.contains("speed"), "{err}");
        assert_eq!(state.retime.keys.len(), 4, "失败的命令一个键都没动");

        // 撤销：开关、曲线、快门同进同退，四步回到默认。
        for _ in 0..4 {
            state
                .run_ui_command(&ctx, "history.undo", &json!({}))
                .expect("撤销变速");
        }
        assert_eq!(state.retime, Retime::default(), "变速整块回到默认");

        let out = state
            .run_ui_command(&ctx, "retime.set", &json!({"speed": 0.5}))
            .expect("慢放");
        assert_eq!(out["frames"], json!(16), "0.5× 把 8 帧拆成 16 帧");

        // 关变速只改开关：曲线留着，读数回 1:1，开回来还是同一条曲线。
        let out = state
            .run_ui_command(&ctx, "retime.set", &json!({"enabled": false}))
            .expect("关变速");
        assert_eq!(out["identity"], json!(true));
        assert_eq!(out["frames"], json!(8));
        assert_eq!(state.retime.keys.len(), 2);
        let out = state
            .run_ui_command(&ctx, "retime.set", &json!({"reset": true}))
            .expect("清变速");
        assert_eq!(state.retime, Retime::default());
        assert!(out["keys"].as_array().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 变速跟着工程文件走：存盘、重新载入后曲线与快门原样回来。
    #[test]
    fn retime_survives_a_project_roundtrip() {
        let dir = temp_dir("retime-proj");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(16, 12, image::Rgb([i * 40, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        state
            .run_ui_command(
                &ctx,
                "retime.set",
                &json!({"keys": [{"at": 0.0, "speed": 1.0}, {"at": 1.0, "speed": 3.0}], "shutter": 0.75}),
            )
            .expect("写曲线");
        state
            .run_ui_command(&ctx, "project.save", &json!({}))
            .expect("存工程");

        let mut fresh = AppState::new();
        fresh
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("重新打开");
        assert_eq!(fresh.retime, state.retime, "工程载入把变速整块带回来");
        assert_eq!(fresh.retime.shutter, 0.75);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 运镜：框、曲线、反向、换比例都走同一条命令通道；坏参数不动状态，
    /// 撤销一步回整块（开关不许只回一半），工程往返把两个框原样带回。
    #[test]
    fn motion_set_walks_the_box_and_undoes_as_one_block() {
        let dir = temp_dir("motion");
        for i in 0..4u8 {
            let img = image::RgbImage::from_pixel(16, 12, image::Rgb([i * 40, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        // 推近倍数按输出画幅算：这里让成片就是 16×12，读数才对得上肉眼。
        state.resolution = Some((16, 12));
        state.output_w = 16;
        state.output_h = 12;

        let out = state
            .run_ui_command(&ctx, "motion.status", &json!({}))
            .expect("读运镜");
        assert_eq!(out["enabled"], json!(false));
        assert_eq!(out["active"], json!(false), "关着就不动");
        assert_eq!(out["from"], json!([0.0, 0.0, 1.0, 1.0]));
        assert_eq!(out["frame"], json!([16, 12]));
        assert_eq!(out["source_frames"], json!(4));

        // 只写终点：开关自动推上去，起点照旧。
        let out = state
            .run_ui_command(&ctx, "motion.set", &json!({"to": [0.25, 0.25, 0.5, 0.5]}))
            .expect("写终点框");
        assert_eq!(out["enabled"], json!(true));
        assert_eq!(out["active"], json!(true));
        assert_eq!(out["to"], json!([0.25, 0.25, 0.5, 0.5]));
        assert_eq!(out["from"], json!([0.0, 0.0, 1.0, 1.0]));
        // 终点框 8×6 撑满 16×12 就是 2×，起点铺满整幅是 1×。
        assert_eq!(out["zoom_to"], json!(2.0));
        assert_eq!(out["zoom_from"], json!(1.0));

        let out = state
            .run_ui_command(&ctx, "motion.set", &json!({"ease": "linear"}))
            .expect("换曲线");
        assert_eq!(out["ease"], json!("linear"));
        assert_eq!(out["ease_cn"], json!("匀速"));
        assert_eq!(out["to"], json!([0.25, 0.25, 0.5, 0.5]), "换曲线不动框");

        let err = state
            .run_ui_command(&ctx, "motion.set", &json!({"ease": "bouncy"}))
            .unwrap_err();
        assert!(err.contains("smooth / linear / in / out"), "{err}");
        assert_eq!(state.motion.ease, Ease::Linear, "失败的命令一个键都没动");
        let err = state
            .run_ui_command(&ctx, "motion.set", &json!({"from": [0.1, 0.2]}))
            .unwrap_err();
        assert!(err.contains("from 要写成四个数"), "{err}");

        let out = state
            .run_ui_command(&ctx, "motion.set", &json!({"swap": true}))
            .expect("反向");
        assert_eq!(out["from"], json!([0.25, 0.25, 0.5, 0.5]));
        assert_eq!(out["to"], json!([0.0, 0.0, 1.0, 1.0]));

        // 换比例重铺：两个框一起按新跨度走，幅度不会被夹成同一个框。
        let out = state
            .run_ui_command(&ctx, "motion.set", &json!({"refit": [1.0, 0.5]}))
            .expect("重铺");
        assert_eq!(out["from"][3], json!(0.5));
        assert!(out["from"] != out["to"], "重铺不许把运镜幅度抹平");

        // 撤销一步回整块：一次命令改两处（终点框 + 曲线），撤销不许只回一半。
        let before = state.motion;
        state
            .run_ui_command(
                &ctx,
                "motion.set",
                &json!({"to": [0.4, 0.4, 0.4, 0.4], "ease": "in"}),
            )
            .expect("一次改两处");
        assert_ne!(state.motion, before);
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤销运镜");
        assert_eq!(state.motion, before, "开关、框、曲线一起退，不许只回一半");
        state
            .run_ui_command(&ctx, "history.redo", &json!({}))
            .expect("重做运镜");
        assert_eq!(state.motion.ease, Ease::In);
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("再撤一次");

        let out = state
            .run_ui_command(
                &ctx,
                "motion.set",
                &json!({"to": { "x": 0.2, "y": 0.1, "w": 0.6, "h": 0.8 }}),
            )
            .expect("对象写法");
        assert_eq!(out["to"], json!([0.2, 0.1, 0.6, 0.8]));
        state
            .run_ui_command(&ctx, "project.save", &json!({}))
            .expect("存工程");
        let mut fresh = AppState::new();
        fresh
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("重新打开");
        assert_eq!(fresh.motion, state.motion, "工程载入把运镜整块带回来");

        let out = state
            .run_ui_command(&ctx, "motion.set", &json!({"reset": true}))
            .expect("清运镜");
        assert_eq!(state.motion, Motion::default());
        assert_eq!(out["enabled"], json!(false));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 拉直：角度进命令、进导出取景、进撤销，且和框一起走同一份快照。
    #[test]
    fn straighten_shrinks_the_export_framing_and_undoes() {
        let dir = temp_dir("straighten");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(64, 48, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        state.resolution = Some((64, 48));

        // 0°、原生比例、没拖过框：不裁，取景是空的。
        let out = state
            .run_ui_command(&ctx, "crop.set", &json!({}))
            .expect("读回取景");
        assert_eq!(out["rotate"], json!(0.0));
        assert_eq!(out["effective"], json!(null));

        let out = state
            .run_ui_command(&ctx, "crop.set", &json!({"rotate": 5.0}))
            .expect("拉直 5°");
        assert_eq!(out["rotate"], json!(5.0));
        let eff = &out["effective"];
        let (x, y) = (eff["x"].as_u64().unwrap(), eff["y"].as_u64().unwrap());
        let (w, h) = (
            eff["width"].as_u64().unwrap(),
            eff["height"].as_u64().unwrap(),
        );
        assert!(x > 0 && y > 0, "转过之后取景该往里收：{out}");
        assert!(w < 64 && h < 48, "安全框该比整幅小：{out}");
        assert_eq!(x % 2, 0);
        assert_eq!(y % 2, 0);

        // 0.2° 以内当没转：不然推子停在 -0.1 时导出一圈无谓的黑边。
        state.set_rotate(0.1);
        assert_eq!(state.rotate, 0.0);

        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤拉直");
        assert_eq!(state.rotate, 0.0);
        assert_eq!(state.effective_crop(), None, "撤到底该回到不取景");

        // 角度和框是一套设置：拉直后取消取景，两者一起回默认。
        state
            .run_ui_command(&ctx, "crop.set", &json!({"rotate": -8.0}))
            .expect("拉直 -8°");
        state
            .run_ui_command(
                &ctx,
                "crop.set",
                &json!({"x": 4, "y": 4, "width": 40, "height": 30}),
            )
            .expect("写框");
        let out = state
            .run_ui_command(&ctx, "crop.set", &json!({"reset": true}))
            .expect("取消取景");
        assert_eq!(out["rotate"], json!(0.0));
        assert_eq!(out["manual"], json!(null));
        assert_eq!(state.user_crop, None);

        let err = state
            .run_ui_command(&ctx, "crop.set", &json!({"rotate": 90.0}))
            .unwrap_err();
        assert!(err.contains("-45..45"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_and_select_work_and_export_needs_confirm() {
        let dir = temp_dir("open");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
            img.save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        let ctx = Context::default();
        let mut state = AppState::new();
        let info = state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        assert_eq!(info["frames"], json!(2));
        assert_eq!(info["opened"], json!(true));

        // 打开一个空文件夹必须报错，不能把旧序列当成新打开的结果。
        let empty = temp_dir("empty");
        let err = state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": empty.display().to_string()}),
            )
            .unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(state.frame_count(), 2, "失败时旧序列应保持原样");
        let _ = std::fs::remove_dir_all(&empty);

        state
            .run_ui_command(&ctx, "frame.select", &json!({"index": 2}))
            .expect("选第二帧");
        assert_eq!(state.selected, 1);
        let err = state
            .run_ui_command(&ctx, "frame.select", &json!({"index": 3}))
            .unwrap_err();
        assert!(err.contains("1..=2"), "{err}");

        let err = state
            .run_ui_command(&ctx, "export.video", &json!({"confirm": false}))
            .unwrap_err();
        assert!(err.contains("confirm"), "{err}");

        // 就算工程文件把 video_out 指向素材，也必须拒绝写入。
        state.render_video_out = dir.join("f000.png");
        let err = state
            .run_ui_command(&ctx, "export.video", &json!({"confirm": true}))
            .unwrap_err();
        assert!(err.contains("拒绝覆盖素材"), "{err}");

        let project = state
            .run_ui_command(&ctx, "project.get", &json!({}))
            .expect("读工程");
        assert!(project["path"].as_str().unwrap().ends_with("project.json"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ai_line_records_success_and_failure() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let value = state
            .run_ai_line(&ctx, "grade.get")
            .expect("执行 grade.get");
        assert!(value["preset"].is_string());
        let err = state
            .run_ai_line(&ctx, "grade.set {\"bogus\": 1}")
            .unwrap_err();
        assert!(err.contains("bogus"), "{err}");
        assert_eq!(state.ai_log.len(), 2);
        assert!(state.ai_log[0].ok);
        assert!(!state.ai_log[1].ok);

        let err = state.run_ai_line(&ctx, "grade.set {oops").unwrap_err();
        assert!(err.contains("JSON"), "{err}");
    }

    #[test]
    fn screenshot_requires_png_and_never_overwrites_sources() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let err = state
            .run_ui_command(&ctx, "ui.screenshot", &json!({"path": "shot.jpg"}))
            .unwrap_err();
        assert!(err.contains("png"), "{err}");
    }

    #[test]
    fn export_cancel_is_idempotent() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let value = state
            .run_ui_command(&ctx, "export.cancel", &json!({}))
            .expect("取消");
        assert_eq!(value["cancelling"], json!(false));
    }

    #[test]
    fn out_dir_safety_rejects_root_and_parent() {
        let root = temp_dir("safe");
        assert!(ensure_out_dir_safe(&root, &root).is_err());
        assert!(ensure_out_dir_safe(&root, root.parent().unwrap()).is_err());
        assert!(ensure_out_dir_safe(&root, &root.join("成片")).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 「导出当前帧」这条单帧路以前无条件走 8bit：批量导出早就保位深了，两条路
    /// 对同一个源给出不同精度是说不过去的。这条守住 GUI 侧不再截位深。
    #[test]
    fn single_frame_export_keeps_16bit_source() {
        use image::{ImageBuffer, Rgb};

        let dir = temp_dir("bitdepth");
        let params = GradeParams::default();

        let deep = dir.join("deep.png");
        let mut img: ImageBuffer<Rgb<u16>, Vec<u16>> =
            ImageBuffer::from_pixel(2, 1, Rgb([1000, 20000, 40000]));
        img.get_pixel_mut(1, 0).0[0] = 1010;
        img.save(&deep).unwrap();
        let deep_out = dir.join("deep_graded.png");
        render_full_file(FrameExport {
            src: &deep,
            dst: &deep_out,
            params: &params,
            gain: Ev::NONE,
            full_size: None,
            crop: None,
            framing: Framing::default(),
            render: RenderOptions::still(StillFormat::Png8),
        })
        .expect("导出 16bit 帧");
        let got = image::open(&deep_out).unwrap().to_rgb16();
        assert_eq!(
            image::open(&deep_out).unwrap().color(),
            image::ColorType::Rgb16
        );
        let a = got.get_pixel(0, 0).0[0];
        let b = got.get_pixel(1, 0).0[0];
        assert!(a.abs_diff(b) >= 8, "相邻 16bit 级又被并了：{a} vs {b}");

        let flat = dir.join("flat.png");
        image::RgbImage::from_pixel(2, 1, Rgb([4, 78, 156]))
            .save(&flat)
            .unwrap();
        let flat_out = dir.join("flat_graded.png");
        render_full_file(FrameExport {
            src: &flat,
            dst: &flat_out,
            params: &params,
            gain: Ev::NONE,
            full_size: None,
            crop: None,
            framing: Framing::default(),
            render: RenderOptions::still(StillFormat::Png8),
        })
        .expect("导出 8bit 帧");
        assert_eq!(
            image::open(&flat_out).unwrap().color(),
            image::ColorType::Rgb8,
            "8bit 源不该被顺手升成 16bit，白让文件大一倍"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 单帧导出真正读面板那排旋钮：容器、色域搬运、输出锐化都要落到磁盘上。
    /// 这条以前是静默忽略——面板切 JPEG 却写 PNG，选 P3 却不搬像素。
    #[test]
    fn single_frame_export_follows_the_output_panel() {
        use image::Rgb;

        let dir = temp_dir("panel");
        let src = dir.join("edge.png");
        // 一半饱和红、一半暗灰：红是 P3 里收得动的原色，边本身又是强亮度边，
        // 色域搬运和输出锐化两件事都能在这一张图上验出来。
        image::RgbImage::from_fn(160, 90, |x, _| {
            if x < 80 {
                Rgb([255, 0, 0])
            } else {
                Rgb([30, 30, 30])
            }
        })
        .save(&src)
        .unwrap();

        // 容器：面板切到 JPEG，落盘的字节就得是 JPEG。
        let jpg = dir.join("edge.jpg");
        export_still(
            &src,
            &jpg,
            RenderOptions {
                quality: 92,
                ..RenderOptions::still(StillFormat::Jpeg)
            },
        );
        let bytes = std::fs::read(&jpg).unwrap();
        assert_eq!(&bytes[..2], b"\xFF\xD8", "选了 JPEG 写出来的却不是 JPEG");
        assert_eq!(
            image::open(&jpg).unwrap().color(),
            image::ColorType::Rgb8,
            "JPEG 没有 16bit 档，深源在这里要落到 8bit"
        );

        // WebP 也是静帧容器，无损但另一套封装。
        let webp = dir.join("edge.webp");
        export_still(&src, &webp, RenderOptions::still(StillFormat::WebP));
        let webp_bytes = std::fs::read(&webp).unwrap();
        assert_eq!(&webp_bytes[8..12], b"WEBP", "选了 WebP 写出来的却不是 WebP");

        // 色域：Display P3 会真的搬像素，sRGB 档一个字节都不动。
        let srgb = dir.join("srgb.png");
        export_still(&src, &srgb, RenderOptions::still(StillFormat::Png8));
        let p3 = dir.join("p3.png");
        export_still(
            &src,
            &p3,
            RenderOptions {
                color_space: ColorSpace::DisplayP3,
                ..RenderOptions::still(StillFormat::Png8)
            },
        );
        let base = image::open(&srgb).unwrap().to_rgb8();
        let moved = image::open(&p3).unwrap().to_rgb8();
        assert_ne!(
            base.as_raw(),
            moved.as_raw(),
            "选了 Display P3 却一个像素都没搬"
        );
        // 方向也要对：饱和红在 P3 里不是原色，编码值必须收、蓝必须被抬起。
        let red_off = base.get_pixel(10, 45).0;
        let red_on = moved.get_pixel(10, 45).0;
        assert!(
            red_on[0] < red_off[0] && red_on[2] > red_off[2],
            "P3 转换方向反了：{red_off:?} → {red_on:?}"
        );

        // 锐化：默认关，开到「屏幕」只许在边上动手。
        let soft = dir.join("soft.png");
        export_still(&src, &soft, RenderOptions::still(StillFormat::Png8));
        let hard = dir.join("hard.png");
        export_still(
            &src,
            &hard,
            RenderOptions {
                sharpen_for: OutputSharpen::Screen,
                ..RenderOptions::still(StillFormat::Png8)
            },
        );
        let off = image::open(&soft).unwrap().to_rgb8();
        let on = image::open(&hard).unwrap().to_rgb8();
        assert_eq!(
            off.as_raw(),
            image::open(&src).unwrap().to_rgb8().as_raw(),
            "默认设定下单帧导出要和源逐位一致"
        );
        assert_eq!(off.as_raw(), base.as_raw(), "锐化默认没关");
        assert_ne!(on.as_raw(), off.as_raw(), "输出锐化对单帧这条路是空转");
        for y in 0..90u32 {
            assert_eq!(off.get_pixel(5, y).0, on.get_pixel(5, y).0, "平场被锐化了");
            assert_eq!(
                off.get_pixel(155, y).0,
                on.get_pixel(155, y).0,
                "平场被锐化了"
            );
        }
        assert!(
            (78..82).any(|x| off.get_pixel(x, 45).0 != on.get_pixel(x, 45).0),
            "边缘一点变化都没有，输出锐化等于没接上"
        );

        // XMP 旁车：勾上就在静帧旁边多一个同名 .xmp（dc: 四项 + crs: 调色），
        // 没勾的那些输出一份旁车都不许有。
        assert!(!dir.join("srgb.xmp").exists(), "没勾旁车却写出来了");
        let sidecar_png = dir.join("sidecar.png");
        export_still(
            &src,
            &sidecar_png,
            RenderOptions {
                xmp_sidecar: true,
                metadata: project::Metadata {
                    title: "云海 & 日出".to_owned(),
                    creator: "张三".to_owned(),
                    ..project::Metadata::default()
                },
                ..RenderOptions::still(StillFormat::Png8)
            },
        );
        let text = std::fs::read_to_string(dir.join("sidecar.xmp")).expect("旁车没写出来");
        assert!(
            text.contains("云海 &amp; 日出"),
            "标题没做 XML 转义：{text}"
        );
        assert!(
            text.contains("<rdf:li>张三</rdf:li>"),
            "创作者没进 dc:creator"
        );
        assert!(text.contains("crs:Exposure2012"), "旁车缺 crs: 参数");
        assert!(!text.contains("dc:description"), "空字段不该写成空标签");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 副本的存/覆盖/套用/删都走命令通道，且每一步都能撤销。
    #[test]
    fn grade_copies_save_apply_delete_and_undo() {
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.8}))
            .expect("改参");
        state
            .run_ui_command(&ctx, "grade.copy", &json!({"op": "save", "name": "日光版"}))
            .expect("存副本");
        let err = state
            .run_ui_command(&ctx, "grade.copy", &json!({"op": "save", "name": "日光版"}))
            .unwrap_err();
        assert!(err.contains("overwrite"), "{err}");
        state
            .run_ui_command(
                &ctx,
                "grade.copy",
                &json!({"op": "save", "name": "日光版", "overwrite": true}),
            )
            .expect("覆盖");
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": -0.4}))
            .expect("再改参");
        let out = state
            .run_ui_command(
                &ctx,
                "grade.copy",
                &json!({"op": "apply", "name": "日光版"}),
            )
            .expect("套用");
        assert_eq!(out["params"]["exposure"], json!(0.8));
        assert_eq!(out["active_copy"], json!("日光版"));
        // 套用后再动一下参数，当前调色就不等于任何副本了。
        state
            .run_ui_command(&ctx, "grade.set", &json!({"contrast": 12.0}))
            .expect("改");
        let out = state
            .run_ui_command(&ctx, "grade.copy", &json!({}))
            .expect("list");
        assert_eq!(out["active_copy"], json!(null));
        assert_eq!(out["copies"].as_array().unwrap().len(), 1);
        state
            .run_ui_command(
                &ctx,
                "grade.copy",
                &json!({"op": "delete", "name": "日光版"}),
            )
            .expect("删");
        let out = state
            .run_ui_command(&ctx, "grade.copy", &json!({}))
            .expect("list");
        assert!(out["copies"].as_array().unwrap().is_empty());
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤删除");
        let out = state
            .run_ui_command(&ctx, "grade.copy", &json!({}))
            .expect("list");
        assert_eq!(out["copies"][0]["name"], json!("日光版"));
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤改参");
        // 退回到「刚套用完」那一步：参数回来了，副本还在。
        assert!((state.params.exposure - 0.8).abs() < 1e-4);
        assert_eq!(state.active_copy(), Some("日光版"));
    }

    /// 导出指定副本：名字不对要在启动前拦下；对了才起后台任务。
    #[test]
    fn export_stills_validates_the_copy_name_first() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let dir = super::history_tests::two_frame_dir("copy-export");
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        let out = dir.join("成片");
        let err = state
            .run_ui_command(
                &ctx,
                "export.stills",
                &json!({"confirm": true, "copy": "没有这版", "out_dir": out.display().to_string()}),
            )
            .unwrap_err();
        assert!(err.contains("没有叫 没有这版 的副本"), "{err}");
        assert!(!state.export_busy, "校验失败不该起导出");
        state
            .run_ui_command(
                &ctx,
                "grade.copy",
                &json!({"op": "save", "name": "没有这版"}),
            )
            .expect("存副本");
        let ok = state
            .run_ui_command(
                &ctx,
                "export.stills",
                &json!({"confirm": true, "copy": "没有这版", "out_dir": out.display().to_string()}),
            )
            .expect("按副本导出");
        assert_eq!(ok["copy"], json!("没有这版"));
        state.stop_export();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// HDR 合并：批量写要 confirm、至少 2 帧；正常两帧融成一张并落地成文件。
    /// 命令只负责启动后台任务，成片路径要等 `Msg::Merged` 落账才拿得到。
    #[test]
    fn hdr_merge_fuses_the_bracket_and_guards_input() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let dir = super::history_tests::two_frame_dir("hdr-merge");
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        let need_confirm = state
            .run_ui_command(&ctx, "hdr.merge", &json!({"frames": [0, 1]}))
            .unwrap_err();
        assert!(need_confirm.contains("confirm"), "{need_confirm}");
        let too_few = state
            .run_ui_command(&ctx, "hdr.merge", &json!({"frames": [0], "confirm": true}))
            .unwrap_err();
        assert!(too_few.contains("至少要 2 帧"), "{too_few}");
        assert!(!state.merge_busy, "校验失败不该起任务");
        let out_dir = dir.join("HDR");
        let ok = state
            .run_ui_command(
                &ctx,
                "hdr.merge",
                &json!({"frames": [0, 1], "out_dir": out_dir.display().to_string(), "confirm": true}),
            )
            .expect("启动 HDR 合并");
        assert_eq!(ok["started"], json!(true));
        assert_eq!(ok["kind"], json!("hdr"));
        assert_eq!(ok["frames"], json!(2));
        assert!(state.merge_busy, "启动后应标记忙碌");
        wait_merge(&mut state, &ctx);
        let done = state.merge_result.clone().expect("合并结果落账");
        assert_eq!(done["kind"], json!("hdr"));
        assert_eq!(done["width"], json!(8));
        assert_eq!(done["height"], json!(6));
        let path = done["path"].as_str().expect("成片路径");
        assert!(Path::new(path).is_file(), "HDR 成片没写出来：{path}");
        assert_eq!(state.hdr_last.as_deref(), Some(path), "面板回显要跟着落账");
        assert_eq!(state.merge_status_json()["busy"], json!(false));
        assert_eq!(
            state.merge_status_json()["done"],
            state.merge_status_json()["total"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 三张左移 24 像素的接片序列：中灰背景 + 40 个灰度各异的方块，角点全图唯一。
    fn pano_frame_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aevum-pano-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        fn blob(k: usize) -> (i64, i64, i64, u8) {
            let bx = ((k * 37) % 100) as i64;
            let by = (((k * 23) % 40) + 4) as i64;
            let size = 4 + (k % 3) as i64;
            let gray = (25.0 + 205.0 * (k as f32) / 40.0) as u8;
            (bx, by, size, gray)
        }
        for i in 0..3u32 {
            let shift = 24 * i as i64;
            let image = image::RgbImage::from_fn(64, 48, |x, y| {
                let wx = x as i64 + shift;
                let wy = y as i64;
                let mut v = 128u8;
                for k in 0..40 {
                    let (bx, by, size, gray) = blob(k);
                    if wx >= bx && wx < bx + size && wy >= by && wy < by + size {
                        v = gray;
                        break;
                    }
                }
                image::Rgb([v, v, v])
            });
            image
                .save(dir.join(format!("p{i:03}.png")))
                .expect("写全景测试帧");
        }
        dir
    }

    /// 全景拼接：命令只启动后台任务；三张接片拼成更宽的一张并落地。
    #[test]
    fn pano_merge_stitches_and_guards_input() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let dir = pano_frame_dir("cmd");
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        let need_confirm = state
            .run_ui_command(&ctx, "pano.merge", &json!({"frames": [0, 1, 2]}))
            .unwrap_err();
        assert!(need_confirm.contains("confirm"), "{need_confirm}");
        let too_few = state
            .run_ui_command(&ctx, "pano.merge", &json!({"frames": [0], "confirm": true}))
            .unwrap_err();
        assert!(too_few.contains("至少要 2 帧"), "{too_few}");
        let out_dir = dir.join("全景");
        let ok = state
            .run_ui_command(
                &ctx,
                "pano.merge",
                &json!({"frames": [0, 1, 2], "out_dir": out_dir.display().to_string(), "confirm": true}),
            )
            .expect("启动全景拼接");
        assert_eq!(ok["started"], json!(true));
        assert_eq!(ok["kind"], json!("pano"));
        assert_eq!(ok["frames"], json!(3));
        // 拼接期间同一类任务不许再起一次（面板按钮置灰之外的第二道门）。
        let busy = state
            .run_ui_command(
                &ctx,
                "pano.merge",
                &json!({"frames": [0, 1, 2], "confirm": true}),
            )
            .unwrap_err();
        assert!(busy.contains("全景拼接在进行中"), "{busy}");
        let hdr_busy = state
            .run_ui_command(
                &ctx,
                "hdr.merge",
                &json!({"frames": [0, 1], "confirm": true}),
            )
            .unwrap_err();
        assert!(hdr_busy.contains("全景拼接在进行中"), "{hdr_busy}");
        wait_merge(&mut state, &ctx);
        let done = state.merge_result.clone().expect("拼接结果落账");
        assert_eq!(done["kind"], json!("pano"));
        assert_eq!(done["height"], json!(48));
        let width = done["width"].as_u64().expect("宽度");
        assert!(width > 64, "拼接后应更宽：{width}");
        let path = done["path"].as_str().expect("成片路径");
        assert!(Path::new(path).is_file(), "全景成片没写出来：{path}");
        assert_eq!(state.pano_last.as_deref(), Some(path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 单测没有界面帧，等后台合并收尾只能自己把消息队列抽干。
    fn wait_merge(state: &mut AppState, ctx: &Context) {
        for _ in 0..2000 {
            state.poll(ctx);
            if !state.merge_busy {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("合并任务没收尾");
    }

    /// 没跑任务时 merge.status / merge.cancel 都要答得清楚，不能报错。
    #[test]
    fn merge_status_reports_idle_without_a_task() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let status = state
            .run_ui_command(&ctx, "merge.status", &json!({}))
            .expect("查询合并状态");
        assert_eq!(status["busy"], json!(false));
        assert_eq!(status["kind"], Value::Null);
        assert_eq!(status["result"], Value::Null);
        assert_eq!(status["error"], Value::Null);
        let cancelled = state
            .run_ui_command(&ctx, "merge.cancel", &json!({}))
            .expect("取消合并");
        assert_eq!(cancelled["cancelling"], json!(false));
        assert!(
            cancelled["note"]
                .as_str()
                .unwrap_or_default()
                .contains("没有进行中"),
            "{cancelled}"
        );
    }

    /// 取消旗只在真跑着任务时立；`merge_busy` 只由 `poll` 清零，所以后台线程
    /// 抢先跑完也不会让这里的断言变绿——忙碌期的拒绝入口另有测试覆盖。
    #[test]
    fn merge_cancel_only_sets_the_flag_while_running() {
        let mut state = AppState::new();
        state.stop_merge();
        assert!(
            !state.cancel_merge.load(Ordering::SeqCst),
            "空闲时不该转取消旗"
        );
        state.merge_busy = true;
        state.merge_kind = Some(MergeKind::Hdr);
        state.stop_merge();
        assert!(
            state.cancel_merge.load(Ordering::SeqCst),
            "忙碌时应转取消旗"
        );
        assert!(state.status.contains("正在取消"), "{}", state.status);
    }

    /// 体检用的序列：编号 001…026，缺 010；005 全黑、014 糊成一团、020 与 021 逐字节相同。
    ///
    /// 每帧都是同一张 8×6 色盘的换序——48 格各用一次，所以**均值一分不动**，只有纹理
    /// 在变。这样体检报出来的离群只可能是真坏帧，而不是我造数据的手法。
    fn qa_frame_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aevum-qa-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let palette: Vec<u8> = (0..48u32).map(|k| (24 + k * 168 / 47) as u8).collect();
        let grid = |shift: usize| -> image::RgbImage {
            image::RgbImage::from_fn(64, 48, |x, y| {
                let cell = (y as usize / 8) * 8 + x as usize / 8;
                let v = palette[(cell + shift) % 48];
                image::Rgb([v, v, v])
            })
        };
        for num in 1..=26usize {
            if num == 10 {
                continue;
            }
            let img = match num {
                5 => image::RgbImage::from_pixel(64, 48, image::Rgb([0, 0, 0])),
                14 => image::imageops::blur(&grid(num), 3.0),
                21 => grid(20),
                _ => grid(num),
            };
            img.save(dir.join(format!("frame_{num:03}.png")))
                .expect("写体检测试帧");
        }
        dir
    }

    /// 等后台测光收尾——没测完就跑体检，每一帧都会被判成「读不出来」。
    fn wait_measure(state: &mut AppState, ctx: &Context) {
        for _ in 0..4000 {
            state.poll(ctx);
            if !state.measuring {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("测光没收尾");
    }

    fn wait_qa(state: &mut AppState, ctx: &Context) {
        for _ in 0..4000 {
            state.poll(ctx);
            if !state.qa_busy {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("体检没收尾");
    }

    /// 体检：命令只启动后台任务，清单由 `poll` 落账；四类问题各就各位、坏输入先挡住。
    #[test]
    fn sequence_check_runs_in_background_and_lists_findings() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let dir = qa_frame_dir("check");
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        // 门槛越界、没有序列这两种输入都不该起线程。
        let bad_sig = state
            .run_ui_command(&ctx, "sequence.check", &json!({"sig": 0.1}))
            .unwrap_err();
        assert!(bad_sig.contains("0.5~20"), "{bad_sig}");
        assert!(!state.qa_busy, "校验失败不该起任务");
        wait_measure(&mut state, &ctx);
        assert_eq!(state.frame_count(), 25, "缺 010 应该是 25 帧");

        let idle = state
            .run_ui_command(&ctx, "sequence.status", &json!({}))
            .expect("空闲状态");
        assert_eq!(idle["busy"], json!(false));
        assert_eq!(idle["ran"], json!(false));
        assert!(idle["findings"].as_array().unwrap().is_empty());

        let ok = state
            .run_ui_command(&ctx, "sequence.check", &json!({"sig": 3.0}))
            .expect("启动体检");
        assert_eq!(ok["started"], json!(true));
        assert_eq!(ok["frames"], json!(25));
        assert_eq!(ok["sig"], json!(3.0));
        assert!(state.qa_busy, "启动后应标记忙碌");
        let again = state
            .run_ui_command(&ctx, "sequence.check", &json!({}))
            .unwrap_err();
        assert!(again.contains("体检正在跑"), "{again}");
        wait_qa(&mut state, &ctx);

        let status = state
            .run_ui_command(&ctx, "sequence.status", &json!({}))
            .expect("体检状态");
        assert_eq!(status["busy"], json!(false));
        assert_eq!(status["ran"], json!(true));
        assert_eq!(status["error"], Value::Null);
        // 对外帧号是 1 基下标：005 全黑→5、缺口后那帧 011→10、014 糊→13、重复报在后者 021→20。
        let got: Vec<(u64, String)> = status["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["frame"].as_u64().unwrap(),
                    f["kind"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert!(got.contains(&(5, "black".to_owned())), "{got:?}");
        assert!(got.contains(&(10, "gap".to_owned())), "{got:?}");
        assert!(got.contains(&(13, "blur".to_owned())), "{got:?}");
        assert!(got.contains(&(20, "dup".to_owned())), "{got:?}");
        assert!(
            !got.iter().any(|(_, k)| k == "read" || k == "luma_out"),
            "坏帧与亮度离群都不该有：{got:?}"
        );
        // 缺口没有对应文件，不能勾「排除这几帧」；其余都可排除。
        let gap = status["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["kind"] == json!("gap"))
            .expect("缺口条目");
        assert_eq!(gap["excludable"], json!(false));
        assert_eq!(gap["label"], json!("序列缺口"));
        assert_eq!(status["counts"]["black"], json!(1));
        assert_eq!(status["counts"]["gap"], json!(1));
        assert_eq!(status["counts"]["dup"], json!(1));
        assert_eq!(status["counts"]["blur"], json!(1));

        // 阈值写进状态：面板下次直接沿用，`sig` 缺省时也读它。
        assert_eq!(state.qa_sig, 3.0);
        assert!(state.status.contains("体检完成"), "{}", state.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 松一档门槛应当只让清单变长，不会把上面那条判据反过来。
    #[test]
    fn looser_threshold_adds_findings_without_removing_them() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let dir = qa_frame_dir("tight");
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        wait_measure(&mut state, &ctx);
        state
            .run_ui_command(&ctx, "sequence.check", &json!({"sig": 3.0}))
            .expect("严体检");
        wait_qa(&mut state, &ctx);
        let strict: Vec<(u64, String)> = state.qa_status_json()["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["frame"].as_u64().unwrap(),
                    f["kind"].as_str().unwrap().to_owned(),
                )
            })
            .collect();

        state
            .run_ui_command(&ctx, "sequence.check", &json!({"sig": 0.5}))
            .expect("松体检");
        wait_qa(&mut state, &ctx);
        let loose: Vec<(u64, String)> = state.qa_status_json()["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["frame"].as_u64().unwrap(),
                    f["kind"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        for item in &strict {
            assert!(
                loose.contains(item),
                "{item:?} 在严清单里，松清单里反而没了"
            );
        }
        assert!(loose.len() >= strict.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 取消旗只在真跑着任务时立；`qa_busy` 只由 `poll` 清零。
    #[test]
    fn qa_cancel_only_sets_the_flag_while_running() {
        let mut state = AppState::new();
        state.stop_qa();
        assert!(
            !state.cancel_qa.load(Ordering::SeqCst),
            "空闲时不该转取消旗"
        );
        state.qa_busy = true;
        state.stop_qa();
        assert!(state.cancel_qa.load(Ordering::SeqCst), "忙碌时应转取消旗");
        assert!(state.status.contains("正在取消"), "{}", state.status);
        assert_eq!(state.qa_progress(), Some((0, 0)));
    }

    /// 单帧导出的一句话入口：测试里只关心成片档旋钮，几何一律走默认。
    fn export_still(src: &Path, dst: &Path, render: RenderOptions) {
        let params = GradeParams::default();
        render_full_file(FrameExport {
            src,
            dst,
            params: &params,
            gain: Ev::NONE,
            full_size: None,
            crop: None,
            framing: Framing::default(),
            render,
        })
        .expect("导出当前帧");
    }

    /// 面板那六个旋钮要一个不落地进单帧导出；漏掉哪个都是「设了没用」。
    /// 几何与视频专用的字段保持默认——「导出当前帧」不许偷偷改画幅。
    #[test]
    fn frame_export_reads_every_output_knob() {
        let mut state = AppState::new();
        state.render_format = StillFormat::Jpeg;
        state.render_quality = 63;
        state.render_keep_exif = true;
        state.render_sharpen_for = OutputSharpen::Glossy;
        state.render_sharpen_strength = SharpenStrength::High;
        state.render_color_space = ColorSpace::Rec2020;
        state.render_watermark.enabled = true;
        state.render_watermark.text = "© 恒晷".to_owned();

        let o = state.frame_render_options();
        assert_eq!(o.mode, OutputMode::Stills);
        assert_eq!(o.format, StillFormat::Jpeg);
        assert_eq!(o.quality, 63);
        assert!(o.keep_exif);
        assert_eq!(o.sharpen_for, OutputSharpen::Glossy);
        assert_eq!(o.sharpen_strength, SharpenStrength::High);
        assert_eq!(o.color_space, ColorSpace::Rec2020);
        assert!(o.watermark.is_on(), "水印没跟进单帧导出");
        assert!(o.crop.is_none() && o.target_size.is_none());
        assert!(o.out_dir.as_os_str().is_empty() && o.video_out.as_os_str().is_empty());
    }

    /// 对话框里的后缀要跟面板选的容器一致：编码器认字节不认文件名，
    /// 用户在「另存为」里手输 .png 也不能把 JPEG 说成 PNG。
    #[test]
    fn force_container_rewrites_the_extension() {
        let jpg = PathBuf::from("D:/x/帧_0001_graded.png");
        assert_eq!(
            force_container(jpg.clone(), StillFormat::Jpeg)
                .extension()
                .unwrap(),
            "jpg"
        );
        // 已经是目标容器就别再动，避免把 `.JPG` 这类大小写改写掉。
        assert_eq!(
            force_container(PathBuf::from("D:/x/a.JPG"), StillFormat::Jpeg).to_string_lossy(),
            "D:/x/a.JPG"
        );
        assert_eq!(
            force_container(PathBuf::from("D:/x/a"), StillFormat::Tiff16).to_string_lossy(),
            "D:/x/a.tif"
        );
    }

    /// 剪切遮罩只罩住真正裁切的像素：全白标红、全黑标蓝（两开时）、
    /// 中间调与「只红通道满」的半裁切不算。
    #[test]
    fn clip_mask_marks_only_fully_clipped_pixels() {
        // 白 / 黑 / 灰 / 只 R 顶满
        let rgb = vec![255, 255, 255, 0, 0, 0, 128, 128, 128, 255, 200, 60];
        let img = clip_mask_image(&rgb, [4, 1], true, true).expect("有开关就有图");
        let px = |i: usize| {
            let p = img.pixels[i];
            [p.r(), p.g(), p.b(), p.a()]
        };
        assert_eq!(px(0)[3], 170, "纯白 → 高光裁切");
        // Color32 内部预乘 alpha：红 255 × 170/255 = 170，绿蓝远小于红。
        assert_eq!(px(0)[0], 170, "罩色预乘后仍是红主导");
        assert!(px(0)[1] < 100 && px(0)[2] < 100, "绿蓝通道远小于红");
        assert_eq!(px(1)[3], 170, "纯黑 → 阴影裁切");
        assert_eq!(px(1)[2], 170, "罩色预乘后蓝主导");
        assert!(px(1)[0] < 100 && px(1)[1] < 100, "红绿通道远小于蓝");
        assert_eq!(px(2)[3], 0, "中间调不罩");
        assert_eq!(px(3)[3], 0, "单通道顶满不算裁切");
        // 只开阴影：白的也不罩了。
        let img = clip_mask_image(&rgb, [4, 1], false, true).unwrap();
        assert_eq!(img.pixels[0].a(), 0);
        assert_eq!(img.pixels[1].a(), 170);
        // 像素不足时拒绝生成，不 panic。
        assert!(clip_mask_image(&rgb[..3], [4, 1], true, true).is_none());
    }

    /// J 键循环：关 → 高光 → 两开 → 阴影 → 关，五拍走完一圈。
    #[test]
    fn clip_cycle_walks_off_high_both_shadow_off() {
        let mut state = AppState::new();
        assert!(!state.clip_highlights && !state.clip_shadows);
        state.clip_cycle();
        assert!(state.clip_highlights && !state.clip_shadows, "第一拍：高光");
        state.clip_cycle();
        assert!(state.clip_highlights && state.clip_shadows, "第二拍：两开");
        state.clip_cycle();
        assert!(!state.clip_highlights && state.clip_shadows, "第三拍：阴影");
        state.clip_cycle();
        assert!(
            !state.clip_highlights && !state.clip_shadows,
            "第四拍：回关"
        );
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    pub(super) fn two_frame_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aevum-hist-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        for i in 0..2u8 {
            image::RgbImage::from_pixel(8, 6, image::Rgb([i * 20, 40, 60]))
                .save(dir.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
        dir
    }

    /// 模拟界面帧：开一帧 → 直接改状态（相当于推子动了）→ 帧末落账。
    fn edit_in_frame(state: &mut AppState, key: &str, label: &str, f: impl FnOnce(&mut AppState)) {
        state.history.begin_frame();
        f(state);
        history::note(key, label);
        state.history_commit();
    }

    #[test]
    fn command_writes_undo_and_redo() {
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5}))
            .expect("改曝光");
        state
            .run_ui_command(&ctx, "grade.set", &json!({"contrast": 12}))
            .expect("改对比");
        assert_eq!(state.history.steps().len(), 3, "第 0 步是初始状态");

        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤对比");
        assert_eq!(state.params.contrast, 0.0);
        assert_eq!(state.params.exposure, 0.5, "只退一步，别的改动不动");

        state
            .run_ui_command(&ctx, "history.redo", &json!({}))
            .expect("重做对比");
        assert_eq!(state.params.contrast, 12.0);

        let err = state
            .run_ui_command(&ctx, "history.redo", &json!({}))
            .unwrap_err();
        assert!(err.contains("重做"), "{err}");
        // 撤到底：第 0 步是起点，前面没有可回的状态。
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("再撤");
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤到起点");
        let err = state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .unwrap_err();
        assert!(err.contains("撤销"), "{err}");
        assert_eq!(state.params.exposure, 0.0);
    }

    #[test]
    fn drag_merges_contiguous_frames_into_one_step() {
        let ctx = Context::default();
        let mut state = AppState::new();
        for k in 1..=5i32 {
            let label = format!("曝光 {k}");
            edit_in_frame(&mut state, "曝光", &label, |s| {
                s.params.exposure = k as f64
            });
        }
        assert_eq!(state.history.steps().len(), 2, "一根推子拖 5 帧只算一步");
        assert_eq!(state.history.steps()[1].label, "曝光 5", "留最后一次的文案");
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤销");
        assert_eq!(state.params.exposure, 0.0, "撤销回到拖动之前");

        // 换控件就开新步。
        edit_in_frame(&mut state, "对比度", "对比度 3", |s| {
            s.params.contrast = 3.0
        });
        assert_eq!(state.history.steps().len(), 2);
    }

    /// v0.3.0 实测回归：在裁剪态点一下画布（没拖），历史不该多出一条
    /// 「裁剪 768 × 368」。按下只摆姿态，真的拖出位移才落 `user_crop`、
    /// 才登记历史；运镜同理，点一下也不许把开关偷偷打开。
    #[test]
    fn crop_click_without_drag_leaves_no_history() {
        let mut state = AppState::new();
        state.resolution = Some((60, 48));
        let base = state.history.steps().len();
        state.history.begin_frame();

        state.crop_begin([20.0, 20.0], crop::Hit::Inside);
        state.crop_end();
        assert_eq!(state.user_crop, None, "纯点击不固化取景框");
        state.history_commit();
        assert_eq!(state.history.steps().len(), base, "点一下画布不产生历史步");

        // 运镜同一条规矩：按下又松开，开关不动、历史不动。
        let enabled_before = state.motion.enabled;
        state.motion_begin([10.0, 10.0], crop::Hit::Inside);
        state.motion_end();
        assert_eq!(state.motion.enabled, enabled_before);
    }

    /// 一次拖动里鼠标停了几帧（帧距不再严格相邻）不能把一步拆成十几步；
    /// 停得足够久（超过容差）才允许开新步。
    #[test]
    fn drag_merge_tolerates_a_few_idle_frames() {
        let mut state = AppState::new();
        state.resolution = Some((60, 48));
        let base = state.history.steps().len();

        // 第 1 帧开始拖，第 2 帧继续，第 3、4 帧停手（没有改动），第 5 帧接着拖。
        state.history.begin_frame();
        state.crop_begin([0.0, 0.0], crop::Hit::Outside);
        state.crop_drag_to([20.0, 20.0]);
        state.history_commit();
        let after_first = state.history.steps().len();
        assert_eq!(after_first, base + 1, "第一段拖动 = 一步");

        for _ in 0..3 {
            state.history.begin_frame();
            state.history_commit();
        }
        state.history.begin_frame();
        state.crop_drag_to([40.0, 30.0]);
        state.history_commit();
        assert_eq!(
            state.history.steps().len(),
            after_first,
            "停三帧仍在容差内，并成同一步"
        );
        assert_eq!(
            state.history.steps().last().map(|s| s.label.as_str()),
            Some("裁剪 40 × 30"),
            "合并步的文案跟最后状态走"
        );

        // 超过容差（31 帧）再拖：这才是一次新的编辑，开新步。
        for _ in 0..(crate::history::MERGE_FRAME_TOLERANCE as usize) {
            state.history.begin_frame();
            state.history_commit();
        }
        state.history.begin_frame();
        state.crop_drag_to([10.0, 10.0]);
        state.history_commit();
        assert_eq!(
            state.history.steps().len(),
            after_first + 1,
            "隔了半秒以上，算新的一步"
        );
    }

    #[test]
    fn unnotured_widgets_still_enter_history() {
        // 面板上直接赋值、忘了登记文案的控件（选项栏那些 chip）靠帧末比对兜住。
        let ctx = Context::default();
        let mut state = AppState::new();
        state.history.begin_frame();
        state.grid = true;
        state.history_commit();
        assert_eq!(state.history.steps().len(), 2);
        assert_eq!(
            state.history.steps()[1].label,
            "网格",
            "比对出的字段名当文案"
        );

        // 没有改动的帧不产生步。
        state.history.begin_frame();
        state.history_commit();
        assert_eq!(state.history.steps().len(), 2);

        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤销");
        assert!(!state.grid);
    }

    #[test]
    fn command_and_slider_do_not_double_record() {
        let ctx = Context::default();
        let mut state = AppState::new();
        let dir = two_frame_dir("dedup");
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        // 命令内部会走 masks_changed → 登记一次；命令自己也记一步，只能留一行。
        state
            .run_ui_command(&ctx, "mask.add", &json!({"kind": "linear"}))
            .expect("加蒙版");
        state.history.begin_frame();
        state.history_commit();
        assert_eq!(
            state.history.steps().len(),
            2,
            "起点 + 加蒙版，命令不算两步"
        );
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤销");
        assert!(state.params.masks.is_empty(), "撤销把蒙版也带回去");
        assert_eq!(state.mask_sel, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ratings_and_keyframes_are_undoable() {
        let ctx = Context::default();
        let mut state = AppState::new();
        edit_in_frame(&mut state, "rating", "帧 0001 评 3 星", |s| {
            s.set_rating(0, 3)
        });
        assert_eq!(state.ratings.get(&0), Some(&3));
        edit_in_frame(&mut state, "keyframe-add", "记录关键帧", |s| {
            s.add_keyframe()
        });
        assert_eq!(state.keyframes.len(), 1);
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤关键帧");
        assert!(state.keyframes.is_empty());
        assert_eq!(
            state.ratings.get(&0),
            Some(&3),
            "星级是另一步，不该被顺手撤掉"
        );
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤星级");
        assert!(state.ratings.is_empty());
    }

    #[test]
    fn external_steps_warn_and_keep_products() {
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5}))
            .expect("改曝光");
        state
            .run_ui_command(&ctx, "queue.clear", &json!({}))
            .expect("清队列");
        assert_eq!(state.history.steps().len(), 3);
        let out = state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤到外部步");
        assert_eq!(out["at"], 2);
        assert!(
            out["warning"].as_str().unwrap_or("").contains("产物"),
            "外部效果要提醒：{out}"
        );
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤曝光");
        assert_eq!(state.params.exposure, 0.0);
    }

    #[test]
    fn label_table_splits_revertible_from_external() {
        let set =
            AppState::history_label("grade.set", &json!({"exposure": 0.5})).expect("改参数可撤");
        assert!(set.2);
        assert_eq!(set.1, "改 曝光", "历史行说中文");
        let many = AppState::history_label(
            "grade.set",
            &json!({"exposure": 0.5, "contrast": 12, "sharp_amount": 40, "nr_color": 30}),
        )
        .expect("一次改多项");
        // serde_json 的 map 按字典序走，所以这一行的字段顺序是确定的。
        assert_eq!(many.1, "改 对比度、曝光、降噪…", "同组参数并成一个词");
        let mask =
            AppState::history_label("mask.add", &json!({"kind": "radial"})).expect("加蒙版可撤");
        assert_eq!(mask.1, "加径向蒙版");
        assert_eq!(
            AppState::history_label("mask.add", &json!({}))
                .expect("缺省 kind")
                .1,
            "加线性蒙版"
        );
        for id in ["align.run", "export.video", "project.save", "queue.run"] {
            let step =
                AppState::history_label(id, &json!({})).unwrap_or_else(|| panic!("{id} 要记步"));
            assert!(!step.2, "{id} 是外部效果");
        }
        assert!(
            AppState::history_label("grade.get", &json!({})).is_none(),
            "只读命令不记步"
        );
    }

    #[test]
    fn jump_and_history_json_agree_with_the_panel() {
        let ctx = Context::default();
        let mut state = AppState::new();
        for k in 1..=3i32 {
            state
                .run_ui_command(&ctx, "grade.set", &json!({"exposure": k as f64}))
                .expect("改曝光");
        }
        let list = state
            .run_ui_command(&ctx, "history.list", &json!({}))
            .expect("列历史");
        assert_eq!(list["count"], 4);
        assert_eq!(list["current"], 4);
        assert_eq!(list["steps"][0]["step"], "空状态");

        // 同样的值再下一次命令：什么都没变，不占一步。
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": 3.0}))
            .expect("重复设置");
        assert_eq!(
            state
                .run_ui_command(&ctx, "history.list", &json!({}))
                .expect("列历史")["count"],
            4
        );

        state
            .run_ui_command(&ctx, "history.jump", &json!({"index": 2}))
            .expect("跳到第 2 步");
        assert_eq!(state.params.exposure, 1.0, "第 2 步是第一次改动之后");
        let err = state
            .run_ui_command(&ctx, "history.jump", &json!({"index": 9}))
            .unwrap_err();
        assert!(err.contains("超出范围"), "{err}");
        state
            .run_ui_command(&ctx, "history.redo", &json!({}))
            .expect("重做");
        assert_eq!(state.params.exposure, 2.0);
    }

    #[test]
    fn history_is_bounded() {
        let ctx = Context::default();
        let mut state = AppState::new();
        for k in 0..(history::HISTORY_LIMIT as i32 + 40) {
            state
                .run_ui_command(&ctx, "grade.set", &json!({"exposure": k as f64}))
                .expect("改曝光");
        }
        assert_eq!(state.history.steps().len(), history::HISTORY_LIMIT);
        assert_eq!(state.history.cursor(), history::HISTORY_LIMIT - 1);
        assert!(state.history.can_undo());
    }

    #[test]
    fn open_resets_the_timeline() {
        let ctx = Context::default();
        let mut state = AppState::new();
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5}))
            .expect("改曝光");
        let dir = two_frame_dir("reset");
        state
            .run_ui_command(
                &ctx,
                "sequence.open",
                &json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        assert_eq!(state.history.steps().len(), 1, "换序列属于新的一条时间线");
        assert_eq!(state.history.steps()[0].label, "打开序列");
        assert!(
            state
                .run_ui_command(&ctx, "history.undo", &json!({}))
                .is_err()
        );
        state
            .run_ui_command(&ctx, "grade.reset", &json!({}))
            .expect("复位");
        assert_eq!(
            state.history.steps().len(),
            1,
            "打开序列已经把参数清成默认，复位是空操作"
        );
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.4}))
            .expect("改曝光");
        assert_eq!(
            state.history.steps().len(),
            2,
            "新序列的第一次改动接在起点后面"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod raw_import_tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aevum-ui-raw-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    /// RAW 首次导入：锐化 40 / 亮度降噪 25；普通图片序列零影响；
    /// 工程文件在场（二次打开）不再触发——「只一次」的保证。
    #[test]
    fn raw_defaults_apply_only_for_first_raw_import() {
        // 只看扩展名认领，序列构建不解码像素，占位字节即可。
        let raw_dir = temp_dir("raw");
        std::fs::write(raw_dir.join("f001.nef"), b"placeholder").expect("写占位 RAW");
        let mut st = AppState::new();
        st.seq = Some(aevum_core::sequence::Sequence::open(&raw_dir).expect("开 RAW 序列"));
        st.apply_raw_import_defaults(&raw_dir);
        assert_eq!(st.params.sharp_amount, 40.0, "RAW 首次导入要套锐化 40");
        assert_eq!(st.params.sharp_protect, 20.0, "RAW 首次导入要套锐化保护 20");
        assert_eq!(st.params.nr_luma, 25.0, "RAW 首次导入要套亮度降噪 25");
        assert!(st.status.contains("40"), "状态栏要说明默认参数");

        // 普通图片序列：条件不满足，参数不动。
        let png_dir = temp_dir("png");
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([20, 40, 60]));
        img.save(png_dir.join("f001.png")).expect("写 PNG");
        st.params = GradeParams::default();
        st.seq = Some(aevum_core::sequence::Sequence::open(&png_dir).expect("开 PNG 序列"));
        st.apply_raw_import_defaults(&png_dir);
        assert_eq!(st.params.sharp_amount, 0.0, "普通序列不该套 RAW 默认");

        // 工程文件在场：用户已保存过参数，不得覆盖。
        let proj = ProjectFile::default();
        project::save(&project::project_path(&raw_dir), &proj).expect("存工程");
        st.seq = Some(aevum_core::sequence::Sequence::open(&raw_dir).expect("再开 RAW 序列"));
        st.apply_raw_import_defaults(&raw_dir);
        assert_eq!(st.params.sharp_amount, 0.0, "有工程文件时不得覆盖");
        let _ = std::fs::remove_dir_all(&raw_dir);
        let _ = std::fs::remove_dir_all(&png_dir);
    }

    /// 边车检测：同目录有 .xmp 就记录提示，只提示不套用。
    #[test]
    fn sidecar_hint_detects_but_never_applies() {
        let dir = temp_dir("sidecar");
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([20, 40, 60]));
        img.save(dir.join("f001.png")).expect("写 PNG");
        std::fs::write(dir.join("f001.xmp"), b"<x/>").expect("写边车");

        let mut st = AppState::new();
        st.seq = Some(aevum_core::sequence::Sequence::open(&dir).expect("开序列"));
        st.detect_sidecar_hint();
        let hint = st.sidecar_hint.clone().expect("要检测到边车");
        assert!(hint.file_name().unwrap() == "f001.xmp");

        // 只提示：参数保持打开时的样子，不因检测而改变。
        assert_eq!(st.params.exposure, 0.0);

        // 无边车的序列：提示为空。
        let plain = temp_dir("plain");
        img.save(plain.join("f001.png")).expect("写 PNG");
        st.seq = Some(aevum_core::sequence::Sequence::open(&plain).expect("开普通序列"));
        st.detect_sidecar_hint();
        assert!(st.sidecar_hint.is_none(), "没有边车不该有提示");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&plain);
    }
}

#[cfg(test)]
mod reset_cache_tests {
    use super::*;
    use aevum_core::mask::{self, ShapeKind};

    #[test]
    fn reset_session_clears_all_caches() {
        let mut st = AppState::new();
        st.params.masks.push(mask::new_mask(ShapeKind::Linear));
        let _ = st.mask_geos_cached(0);
        st.curve_lut_cache = Some(crate::widgets::CurveLutCache {
            fp: 7,
            lut: vec![0.0; 129],
        });
        st.hist_mesh_cache = Some(crate::shell::HistMeshCache {
            version: 1,
            rect: egui::Rect::ZERO,
            shapes: Vec::new(),
        });
        st.defl_curve_cache = Some(crate::shell::DeflCurveCache {
            version: 1,
            n: 1,
            rect: egui::Rect::ZERO,
            pts_raw: [].into(),
            pts_fix: [].into(),
            lo: 0.0,
            hi: 1.0,
        });
        st.reset_session();
        assert_eq!(st.mask_geo_cache.fp, 0, "会话清理必须清空蒙版缓存");
        assert!(st.curve_lut_cache.is_none());
        assert!(st.hist_mesh_cache.is_none());
        assert!(st.defl_curve_cache.is_none());
        // 会话清单本体逐字段核（E7 收编后的回归防线）。
        assert!(st.plugin_result.is_none());
        assert_eq!(st.plugin_stamp, 0);
        assert_eq!(st.plugin_progress, (0, 0));
        assert_eq!(st.preview_rgb, None);
        assert!(st.clip_mask.is_none());
        assert_eq!(st.clip_mask_stamp, u64::MAX);
        assert_eq!(st.mask_paint, None);
        assert_eq!(st.preview_size, [0, 0]);
        assert!(!st.preview_dirty);
        assert!(!st.preview_inflight);
        assert!(st.thumbs.is_empty());
        assert!(st.thumb_pending.is_empty());
    }

    #[test]
    fn reset_mask_state_clears_five_fields() {
        let mut st = AppState::new();
        st.mask_sel = Some(0);
        st.mask_comp = 1;
        st.mask_drag = Some((0, 1));
        st.mask_pick = true;
        st.mask_pick_img = Some(ColorImage::new([2, 2], vec![egui::Color32::BLACK; 4]));
        st.reset_mask_state();
        assert_eq!(st.mask_sel, None);
        assert_eq!(st.mask_comp, 0);
        assert_eq!(st.mask_drag, None);
        assert!(!st.mask_pick);
        assert!(st.mask_pick_img.is_none());
    }
}
