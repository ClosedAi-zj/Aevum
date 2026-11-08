//! 应用状态与后台任务。
//!
//! 所有重活（测光、缩略图、预览渲染、导出）都在线程里做，界面线程只收消息、
//! 传纹理。消息带令牌：换序列或换帧后，晚到的旧结果直接丢弃。
//!
//! Application state and background jobs.
//!
//! Every heavy job (metering, thumbnails, preview rendering, export) runs on a
//! thread; the UI thread only receives messages and uploads textures. Messages carry
//! a token: after switching sequence or frame, late results from the old one are
//! dropped outright.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
use aevum_core::delivery::DeliveryPreset;
use aevum_core::develop::Ev;
use aevum_core::exif::Exif;
use aevum_core::export::{
    self, Codec, JobProgress, OutputDynamic, OutputMode, OutputSharpen, RenderOptions,
    SharpenStrength, Stage, StillFormat,
};
use aevum_core::focus_align::{self, AlignStatus};
use aevum_core::grade::{self, GradeParams, PRESET_NAMES};
use aevum_core::interp;
use aevum_core::journal;
use aevum_core::mask;
use aevum_core::metrics::{self, FrameMetrics};
use aevum_core::perspective;
use aevum_core::project::{self, AlignState, GradeCopy, ProjectFile, RenderState};
use aevum_core::qa::{self, Finding};
use aevum_core::queue::{self, QueueItem, QueueStatus};
use aevum_core::retime::{self, Retime, SpeedKey};
use aevum_core::retouch;
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
use crate::shell::flow::FlowTask;

pub const PREVIEW_SIDE: u32 = 1600;
/// 草图贴图的长边：全量预览的一半，也是首屏真正要的那一档。
///
/// 草图只出现在画布临时贴图上——全量预览、直方图/曲线分析、导出仍取 [`PREVIEW_SIDE`]
/// 那一档的整幅解码结果（见 `decoded_preview_source`）。
///
/// Long side of the sketch texture: half of the full preview, and the size the first screen
/// actually needs. The sketch only ever reaches the canvas's temporary texture -- the full
/// preview, histogram/curve analysis and export keep using the [`PREVIEW_SIDE`] decode
/// (`decoded_preview_source`).
pub const SKETCH_SIDE: u32 = 800;
pub const THUMB_SIDE: u32 = 128;
pub const PLAY_FPS: f32 = 24.0;

/// 播放代理贴图的长边：比胶片条的 [`THUMB_SIDE`]（128px）大一档，仍远小于全量预览
/// （[`PREVIEW_SIDE`]）。播放态画布吃这一档，暂停后自动回全质量。
///
/// 代理**仍走机身内嵌原片、不经显影链**——只放大尺寸，不改显影路径；它和缩略图
/// 是同一份「未显影」数据，只是解码边长不同（`metrics::thumbnail` 的缓存键已含
/// max_side，换尺寸会自动生成另一份缓存）。
///
/// Long side of the playback proxy texture: one notch above the filmstrip's
/// [`THUMB_SIDE`] (128px) and still far below the full preview ([`PREVIEW_SIDE`]). The
/// canvas uses this during playback and returns to full quality on pause.
///
/// The proxy **still comes from the camera-embedded original and never enters the develop
/// chain** -- only the size grows, the pipeline is unchanged; it is the same "undeveloped"
/// data as the thumbnail, decoded at a different edge length (the cache key of
/// `metrics::thumbnail` already includes max_side, so another size is cached separately).
pub const PROXY_SIDE: u32 = 640;
/// 播放时提前取回多少帧的代理。代理解码比缩略图贵，故只预取一秒的画面（24fps）。
///
/// How many frames of proxies to prefetch during playback. Proxy decode costs more than a
/// thumbnail, so only one second of footage is prefetched (24fps).
pub const PLAY_PROXY_AHEAD: usize = 24;
/// 色标序号对应的名字：0 是没打，1…4 跟 `shell::label_color` 同序。
///
/// Names for the colour label indices: 0 means unlabelled, 1..4 follow the same order
/// as `shell::label_color`.
pub const LABEL_NAMES: [&str; 5] = ["无", "红", "黄", "绿", "蓝"];
/// 状态栏常驻告警最多留几条。护带目录、override 这类问题一句话就说完，
/// 留 8 条够覆盖「一个工程上同时坏了几处」，再多就把状态栏挤没了。
const NOTICE_LIMIT: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Module {
    Sequence,
    Deflicker,
    Develop,
    Frame,
    Render,
    Audio,
    /// 合成：HDR / 全景 / 星空堆栈 / 时间合成 / 焦点合成——「多帧进来、一张图出去」
    /// 的文件级工具，2026-11-04 从显影面板分出来（见 docs/界面布局-ACR化-2026-11-04.md
    /// §4）。**追加在末尾而不是插在显影后**：`ui.module` 命令与 Alt+数字快捷键共用
    /// 0-5 序号契约，中途插入会把「3=构图」这类既有含义挪位。
    ///
    /// Merge: HDR / panorama / star-stack / time-blend / focus-stack -- file-level
    /// "frames in, one image out" tools split out of the Develop panel on 2026-11-04
    /// (docs/界面布局-ACR化-2026-11-04.md §4). Appended last rather than inserted
    /// after Develop: the `ui.module` command and the Alt+digit shortcuts share the
    /// 0-5 index contract, and inserting mid-row would silently move "3=Frame".
    Merge,
}

impl Module {
    pub const ALL: [Module; 7] = [
        Module::Sequence,
        Module::Deflicker,
        Module::Develop,
        Module::Frame,
        Module::Render,
        Module::Audio,
        Module::Merge,
    ];

    pub fn cn(self) -> &'static str {
        match self {
            Module::Sequence => "序列",
            Module::Deflicker => "去闪",
            Module::Develop => "显影",
            Module::Frame => "构图",
            Module::Render => "成片",
            Module::Audio => "声音",
            Module::Merge => "合成",
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
            Module::Merge => "MERGE",
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
            Module::Merge => Glyph::Merge,
        }
    }

    /// 模块序号（0-6），控制通道与 AI 面板共用。
    ///
    /// Module index (0-6), shared by the control channel and the AI panel.
    pub fn index(self) -> usize {
        Module::ALL
            .iter()
            .position(|m| *m == self)
            .unwrap_or_default()
    }

    /// 认中文名、英文名和 0-6 序号；入口参数与控制命令共用同一套写法。
    ///
    /// Accepts the Chinese name, the English name, or the 0-6 index; the CLI argument and
    /// the control commands use the same set of spellings.
    pub fn from_name(name: &str) -> Option<Module> {
        match name.trim() {
            "序列" | "sequence" | "seq" | "0" => Some(Module::Sequence),
            "去闪" | "deflicker" | "def" | "1" => Some(Module::Deflicker),
            "显影" | "develop" | "dev" | "2" => Some(Module::Develop),
            "构图" | "frame" | "3" => Some(Module::Frame),
            "成片" | "render" | "4" => Some(Module::Render),
            "声音" | "audio" | "5" => Some(Module::Audio),
            "合成" | "merge" | "6" => Some(Module::Merge),
            _ => None,
        }
    }
}

/// 面板分组的登记表：键、所属模块、首屏是否展开。
///
/// 一张表供三处用，所以必须与 `group_header(…)` 的调用点一一对应：
///
/// * **首屏折叠** —— [`AppState::new`] 按 `open == false` 播种 `collapsed`；
/// * **右键「只看这一组」** —— 按 `module` 把同模块的其他组折起来；
/// * **落盘回查** —— `settings.json` 里只存键名，读回来要在这里查成 `&'static str`
///   （`collapsed` 存的是 `&'static str`，不查表就没法从 JSON 变回来）。
///
/// 「常用/不常用」的判断口径只有一条：**每张照片、每次导出都会动的那几组展开**，
/// 设一次就不再碰的（相机镜头配置、水印、HDR/全景合并…）折叠。参考 Lightroom
/// 的默认态——打开一张图只看见「基本」是展开的，其余都是折叠的标题条；面板是
/// 按需打开的抽屉，不是一屏到底的清单。
///
/// 漏登记、重复登记、或者登记了却没人调用，都会在 `tests/suite/ui_tokens.rs` 里变红。
///
/// Registry of panel groups: key, owning module, and whether it opens on the first
/// screen.
///
/// One table serves three purposes, so it must correspond one-to-one with the
/// `group_header(...)` call sites:
///
/// * **first-screen folding** -- [`AppState::new`] seeds `collapsed` from
///   `open == false`;
/// * **right-click "show only this group"** -- folds the other groups of the same
///   module, matched by `module`;
/// * **persistence round-trip** -- `settings.json` stores key names only, and they
///   must be looked up here to become `&'static str` again (`collapsed` holds
///   `&'static str`, which cannot come back from JSON without a table).
///
/// There is exactly one criterion for "frequent / infrequent": **the groups touched
/// on every photo and every export stay open**, the ones set once and never touched
/// again (camera and lens profiles, watermark, HDR / panorama merge ...) are folded.
/// This follows Lightroom's default state -- open one image and only Basic is
/// expanded, everything else is a folded header bar; panels are drawers you open as
/// needed, not one long list end to end.
///
/// A missing registration, a duplicate, or a registration nobody calls turns red in
/// `tests/suite/ui_tokens.rs`.
pub struct GroupSpec {
    pub key: &'static str,
    /// 挂在哪个模块下面。`None` = 挂在 dock 标签页（信息 / 代理 / 图库）而不是模块：
    /// 这几个面板不随模块切换，右键「只看这一组」只该收同页的组。
    ///
    /// Which module this group belongs to. `None` = it lives in a dock tab (info / agent
    /// / library) rather than under a module: those panels do not follow the module
    /// switch, so "show only this group" should only fold groups on the same page.
    pub module: Option<Module>,
    /// dock 页的归属页名（`module` 为 `None` 时才有意义）。不带页名的话，
    /// 「只看这一组」会把快捷键页、代理页一起收掉——切过去就是一屏折着的面板。
    ///
    /// The dock page this group belongs to (only meaningful when `module` is `None`).
    /// Without the page name, "show only this group" would fold the shortcuts page and
    /// the agent page too -- switching there shows a screen full of folded panels.
    pub page: &'static str,
    /// 首屏展开（`false` = 首屏折叠，点标题条展开）。
    ///
    /// Open on the first screen (`false` = folded, expand by clicking the header bar).
    pub open: bool,
}

/// 模块下的分组（绝大多数是这种）。
const fn in_module(key: &'static str, module: Module, open: bool) -> GroupSpec {
    GroupSpec {
        key,
        module: Some(module),
        page: "",
        open,
    }
}

/// dock 标签页里的分组（信息 / 代理 / 图库那几页）。
const fn in_tab(page: &'static str, key: &'static str, open: bool) -> GroupSpec {
    GroupSpec {
        key,
        module: None,
        page,
        open,
    }
}

pub const GROUP_SPECS: &[GroupSpec] = &[
    // ── 序列 ────────────────────────────────────────────────────────────
    // 帧列表与体检是这一屏的主体，其余三组都是「跑完看一次」的读数。
    in_module("seq", Module::Sequence, true),
    in_module("seq-issues", Module::Sequence, true),
    in_module("seq-qa", Module::Sequence, true),
    in_module("seq-cull", Module::Sequence, false),
    in_module("seq-align", Module::Sequence, false),
    in_module("seq-align-report", Module::Sequence, false),
    in_module("seq-spec", Module::Sequence, false),
    // ── 去闪 ────────────────────────────────────────────────────────────
    in_module("deflicker", Module::Deflicker, true),
    in_module("def-stats", Module::Deflicker, true),
    in_module("def-exposure", Module::Deflicker, false),
    // ── 显影 ────────────────────────────────────────────────────────────
    // ACR 九分区（docs/界面布局-ACR化-2026-11-04.md §3）：基本 → 曲线 →
    // 颜色混合器 → 颜色分级 → 细节 → 光学 → 效果 → 外观 → 蒙版。旧组名
    // color/parametric/levels/hsl/mono/grain/lens/profiles 已并入新组，
    // 落盘的旧折叠键由 `legacy_group_key` 映射过来，用户的折叠偏好不丢。
    // 首屏只有基本/细节/蒙版展开——ACR 默认也只有基本展开，其余是深加工。
    in_module("basic", Module::Develop, true),
    in_module("curve", Module::Develop, false),
    in_module("mixer", Module::Develop, false),
    in_module("grade", Module::Develop, false),
    in_module("detail", Module::Develop, true),
    in_module("optics", Module::Develop, false),
    // 几何与修复紧跟光学：镜头校正先摆平畸变与暗角，然后才谈拉正与点修，
    // 两者都属于「整幅画面的一次性处理」，排在逐点调色之前读起来更顺。
    //
    // Geometry and repair sit right after optics: lens correction squares up distortion and
    // vignetting first, and only then do straightening and spot repair make sense. Both are
    // whole-frame one-off fixes, so they read better before the per-pixel grading groups.
    in_module("geometry", Module::Develop, false),
    in_module("retouch", Module::Develop, false),
    in_module("effects", Module::Develop, false),
    in_module("look", Module::Develop, false),
    in_module("masks", Module::Develop, true),
    in_module("mask-params", Module::Develop, true),
    // 2026-11-04 起五组文件级工具归属「合成」模块（docs/界面布局-ACR化-2026-11-04.md §4）：
    // 键名不变（用户的折叠偏好照常读回），只是所属模块换了——「只看这一组」的
    // 作用域随之切到合成页。
    //
    // Since 2026-11-04 the five file-level tools belong to the Merge module (plan §4):
    // keys are unchanged (persisted collapse preferences still load), only the owning
    // module moved -- "solo this group" scope follows into the Merge page.
    in_module("hdr", Module::Merge, true),
    in_module("pano", Module::Merge, false),
    // 星空堆栈与时间合成也都是「一次合一张图」的文件级工具，和 HDR / 全景同一档。
    in_module("stack", Module::Merge, false),
    in_module("timeblend", Module::Merge, true),
    // 焦点合成与它们同一档：一组帧进来、一张成片出去。
    in_module("focus", Module::Merge, false),
    // ── 构图 ────────────────────────────────────────────────────────────
    // 这一屏只有两组，且辅助线只是三个勾选框：没有折叠的必要。
    in_module("frame", Module::Frame, true),
    in_module("frame-guides", Module::Frame, true),
    // ── 成片 ────────────────────────────────────────────────────────────
    // 输出/时间/编码/导出/队列是导一次片必经的五步；水印、变速、运镜是单条片子
    // 才用的创作项，折起来。平台交付摆在最前：先定投哪儿，再调后面的细节。
    in_module("render-delivery", Module::Render, true),
    in_module("render-out", Module::Render, true),
    in_module("render-time", Module::Render, true),
    in_module("render-codec", Module::Render, true),
    in_module("render-export", Module::Render, true),
    in_module("render-queue", Module::Render, true),
    in_module("render-watermark", Module::Render, false),
    in_module("render-retime", Module::Render, false),
    in_module("render-motion", Module::Render, false),
    // ── 声音 ────────────────────────────────────────────────────────────
    in_module("audio", Module::Audio, true),
    in_module("audio-time", Module::Audio, true),
    in_module("audio-mix", Module::Audio, false),
    in_module("audio-timeline", Module::Audio, false),
    // ── dock 标签页 ─────────────────────────────────────────────────────
    // 信息页是读数（EXIF / 采集 / 引擎 / 诊断），本来就是「摊开看一眼」的用法；
    // 快捷键与代理日志同理。这几页只有两三组，折叠省不下多少，保持展开。
    in_tab("info", "info-meta", true),
    in_tab("info", "info-capture", true),
    in_tab("info", "info-frame", true),
    in_tab("info", "info-print", true),
    in_tab("info", "info-engine", true),
    in_tab("info", "info-diag", true),
    in_tab("keys", "keys", true),
    in_tab("ai", "ai-link", true),
    in_tab("ai", "ai-cmds", true),
    in_tab("ai", "ai-log", true),
    in_tab("ai", "ai-console", true),
];

/// 首屏折叠的分组键集合。用户没表过态时用它，之后以他点出来的为准。
///
/// The set of group keys folded on the first screen. Used when the user has expressed
/// no preference; afterwards his own clicks win.
/// 「预设与快照」浮窗的三节。次序可拖，但键名是**落盘契约**：改了名字，用户调好的
/// 次序就丢了。历史一节 2026-11-04 删除（dock「历史」页有完整撤销/重做+步骤列表，
/// 侧栏那份是重复视图，docs/界面布局-ACR化-2026-11-04.md §2）；调色副本同日迁入
/// ——它本质是「存一套参数+关键帧」，与快照同类。老落盘里的 "history" 键会被
/// `normalise_preset_order` 当未知键丢弃、"copies" 按缺键补到末尾，无需迁移代码。
///
/// The three sections of the Presets & Snapshots window. The order is draggable, but the
/// keys are a **persistence contract**: renaming one throws away the order the user
/// arranged. The history section was removed on 2026-11-04 (the dock's History tab has
/// the full undo/redo + step list; the sidebar copy was a duplicate view, plan §2), and
/// grade copies moved in the same day -- a copy is "one saved set of parameters plus
/// keyframes", the same species as a snapshot. A persisted "history" key is dropped by
/// `normalise_preset_order` as unknown and "copies" is appended as missing; no migration
/// code needed.
pub const PRESET_SECTIONS: [&str; 3] = ["builtin", "snapshots", "copies"];

/// 「预设与快照」浮窗的默认次序：内置 → 快照 → 副本（从"别人的"到"自己的"）。
///
/// Default window order: built-in, then snapshots, then copies -- from "someone else's"
/// to "mine".
pub fn default_preset_order() -> Vec<&'static str> {
    PRESET_SECTIONS.to_vec()
}

/// 把落盘 / 手改过的次序规整成合法排列：丢掉认不出的键、去掉重复、补上缺的（按默认
/// 次序追加到末尾）。
///
/// 三种坏输入都必须能开起来：老版本没有这一项（空表）、用户删掉一节（少键）、手改成
/// 了 `["history","bogus"]`（未知键）。规整之后恒为 `PRESET_SECTIONS` 的一个排列，
/// 界面那一侧就不用再写防御代码。
///
/// Normalise a persisted / hand-edited order into a valid permutation: drop unknown keys,
/// remove duplicates, append whatever is missing in default order.
///
/// Three kinds of bad input must still open fine: an older file without the field (empty),
/// a user who deleted a section (missing key), a hand edit like `["history","bogus"]`
/// (unknown key). The result is always a permutation of `PRESET_SECTIONS`, so no defensive
/// code is needed on the drawing side.
pub fn normalise_preset_order(raw: &[String]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::with_capacity(PRESET_SECTIONS.len());
    for k in raw {
        if let Some(known) = PRESET_SECTIONS.iter().find(|s| **s == k.as_str())
            && !out.contains(known)
        {
            out.push(known);
        }
    }
    for k in PRESET_SECTIONS {
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

pub fn default_collapsed() -> HashSet<&'static str> {
    GROUP_SPECS
        .iter()
        .filter(|s| !s.open)
        .map(|s| s.key)
        .collect()
}

/// 按 `collapsed` 里的键名查表：查不到就是历史遗留键（旧版本有、这一版删了），丢掉。
///
/// Look up a key from `collapsed`: a miss means a legacy key (present in an older
/// version, removed in this one) and it is dropped.
pub fn group_spec(key: &str) -> Option<&'static GroupSpec> {
    GROUP_SPECS.iter().find(|s| s.key == key)
}

/// 把落盘的折叠键映射到当前键名，查不到登记表就丢弃。
/// 2026-11-04 显影面板按 ACR 九分区重排（docs/界面布局-ACR化-2026-11-04.md §3），
/// 旧组名并入新组：color→basic、parametric/levels→curve、hsl/mono→mixer、
/// grain→effects、lens/profiles→optics；"copies" 随调色副本搬去浮窗，直接丢弃。
/// 改键名而不做映射，等于悄悄清掉老用户的折叠偏好。
///
/// Map a persisted collapse key to the current one, dropping unknown keys. The Develop
/// panel was reorganised into the ACR nine sections on 2026-11-04 (plan §3); old group
/// names fold into new ones, and "copies" (moved to the presets window) is dropped.
/// Renaming without mapping would silently wipe the user's persisted preferences.
pub fn legacy_group_key(raw: &str) -> Option<&'static str> {
    let mapped = match raw {
        "color" => "basic",
        "parametric" | "levels" => "curve",
        "hsl" | "mono" => "mixer",
        "grain" => "effects",
        "lens" | "profiles" => "optics",
        other => other,
    };
    group_spec(mapped).map(|s| s.key)
}

/// 面板折叠的三个整批动作（右键分组头调）。放在 state 而不是界面里，是因为
/// 「作用域」这件事得能单测：只看这一组只该收**同模块/同 dock 页**的组。
impl AppState {
    /// 只看这一组：同作用域的兄弟组全折起来，自己展开。
    ///
    /// Show only this group: fold every sibling in the same scope, keep this one open.
    pub fn solo_group(&mut self, key: &str) {
        let Some(me) = group_spec(key) else { return };
        self.collapsed = GROUP_SPECS
            .iter()
            .filter(|s| (s.module, s.page) == (me.module, me.page) && s.key != key)
            .map(|s| s.key)
            .collect();
        self.settings_dirty = true;
    }

    /// 全部收起（只影响当前作用域之外的组也一起收：这是「清屏」的语义）。
    ///
    /// Collapse everything (groups outside the current scope are folded too: this is the
    /// "clear the screen" meaning).
    pub fn collapse_all_groups(&mut self) {
        self.collapsed = GROUP_SPECS.iter().map(|s| s.key).collect();
        self.settings_dirty = true;
    }

    /// 全部展开。
    ///
    /// Expand everything.
    pub fn expand_all_groups(&mut self) {
        self.collapsed.clear();
        self.settings_dirty = true;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Aspect {
    Native,
    Wide,
    ThreeTwo,
    FourThree,
    FiveFour,
    Square,
    FourFive,
    Vertical,
    TwoThree,
}

impl Aspect {
    /// 预设比例的排布照 Photoshop 裁剪工具的惯例：横构图一排、方一档、
    /// 竖构图一排，最前面是「原生」（不约束）。3:2 与 4:3 是相机传感器的
    /// 两个原生形状，5:4 / 4:5 是八大打印幅面最常用的两档，竖版给镜像。
    ///
    /// The preset order follows Photoshop's crop tool: one row of landscape, one square,
    /// one row of portrait, with the unconstrained one first. 3:2 and 4:3 are the two
    /// native sensor shapes; 5:4 / 4:5 are the two most used of the eight print sizes,
    /// mirrored for portrait.
    pub const ALL: [Aspect; 9] = [
        Aspect::Native,
        Aspect::Wide,
        Aspect::ThreeTwo,
        Aspect::FourThree,
        Aspect::FiveFour,
        Aspect::Square,
        Aspect::FourFive,
        Aspect::Vertical,
        Aspect::TwoThree,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Aspect::Native => "原生",
            Aspect::Wide => "16:9",
            Aspect::ThreeTwo => "3:2",
            Aspect::FourThree => "4:3",
            Aspect::FiveFour => "5:4",
            Aspect::Square => "1:1",
            Aspect::FourFive => "4:5",
            Aspect::Vertical => "9:16",
            Aspect::TwoThree => "2:3",
        }
    }

    pub fn ratio(self) -> Option<f32> {
        match self {
            Aspect::Native => None,
            Aspect::Wide => Some(16.0 / 9.0),
            Aspect::ThreeTwo => Some(3.0 / 2.0),
            Aspect::FourThree => Some(4.0 / 3.0),
            Aspect::FiveFour => Some(5.0 / 4.0),
            Aspect::Square => Some(1.0),
            Aspect::FourFive => Some(4.0 / 5.0),
            Aspect::Vertical => Some(9.0 / 16.0),
            Aspect::TwoThree => Some(2.0 / 3.0),
        }
    }

    /// 整数分子分母：定比例的推进步长要按它成对走（见 `aevum_core::crop`）。
    ///
    /// Integer numerator and denominator: the step of a fixed-ratio crop advances by pairs
    /// of these (see `aevum_core::crop`).
    pub fn pair(self) -> Option<(u32, u32)> {
        match self {
            Aspect::Native => None,
            Aspect::Wide => Some((16, 9)),
            Aspect::ThreeTwo => Some((3, 2)),
            Aspect::FourThree => Some((4, 3)),
            Aspect::FiveFour => Some((5, 4)),
            Aspect::Square => Some((1, 1)),
            Aspect::FourFive => Some((4, 5)),
            Aspect::Vertical => Some((9, 16)),
            Aspect::TwoThree => Some((2, 3)),
        }
    }

    /// 从存档标签找回比例；空串或认不出的都按「原生」，旧工程文件照样能开。
    ///
    /// Recover the ratio from its stored label; an empty or unrecognised label means
    /// "unconstrained", so older project files still open.
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
    /// 修复画笔：在画面上点按落笔，把斑点处的像素按周边重新生成。
    ///
    /// The healing brush: tap on the canvas to lay a stroke that regenerates the patch from
    /// its surroundings.
    Heal,
    Crop,
    Compare,
    Grid,
}

impl Tool {
    pub const ALL: [Tool; 6] = [
        Tool::Hand,
        Tool::Zoom,
        Tool::Heal,
        Tool::Crop,
        Tool::Compare,
        Tool::Grid,
    ];

    pub fn cn(self) -> &'static str {
        match self {
            Tool::Hand => "抓手",
            Tool::Zoom => "缩放",
            Tool::Heal => "修复",
            Tool::Crop => "裁剪",
            Tool::Compare => "对比",
            Tool::Grid => "网格",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Tool::Hand => "抓手 (H) · 拖动画布",
            Tool::Zoom => "缩放 (Z) · 单击放大，Alt 单击缩小",
            Tool::Heal => "修复 (J) · 点按斑点亮斑修复，拖动连成一片；仿制模式下先点源再点目标",
            Tool::Crop => "裁剪 (C) · 画幅与辅助线",
            Tool::Compare => "前后对比 (\\) · 左右分割",
            Tool::Grid => "网格 (G) · 三分线",
        }
    }

    pub fn glyph(self) -> Glyph {
        match self {
            Tool::Hand => Glyph::Hand,
            Tool::Zoom => Glyph::Zoom,
            Tool::Heal => Glyph::Heal,
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
    ///
    /// The raw command plus its arguments as the user typed them.
    pub input: String,
    pub ok: bool,
    /// 成功是返回 JSON，失败是中文错误。
    ///
    /// A JSON result on success, an error message on failure.
    pub detail: String,
}

pub struct Thumb {
    pub texture: TextureHandle,
    pub size: [u32; 2],
    /// 这张贴图是按哪一份参数显影的。与当前 `preview_params()` 不一致就是过期，
    /// 需要重调色；代理与图库缩略图不走显影链，恒为 `None`。
    ///
    /// Which parameter set this texture was developed with. A mismatch with the current
    /// `preview_params()` means it is stale and must be re-graded; proxies and library
    /// thumbnails skip the develop chain and stay `None`.
    pub grade: Option<GradeParams>,
}

/// 缩略图的解码源：与主画布同一条解码口径的 sRGB 浮点像素，**未调色**。
///
/// 缩略图要跟主画布一个口径（用户 2026-11-07：「预览图应该跟主图一致」），换参数
/// 就得重走一遍显影链。解码（读盘 + RAW 半尺寸去马赛克，约 270ms）比 128px 上的
/// 一次调色贵三个量级，所以源像素留在这里，参数变了只重调色、不重解码。
/// `profile` 记下这份源是按哪份相机配置解的：配置一换源就作废，必须重解——只重调色
/// 会把旧配置的色彩留在新配置上。
///
/// Decoded source for a thumbnail: sRGB float pixels at the canvas's decoding convention,
/// **ungraded**. The filmstrip has to match the canvas, so a parameter change re-runs the
/// develop chain. Decoding (disk read plus a half-size RAW demosaic, about 270 ms) costs
/// three orders of magnitude more than grading 128 px, so the source stays here and a
/// parameter change re-grades only. `profile` records which camera profile produced this
/// source: a profile change invalidates it and forces a re-decode -- re-grading alone would
/// keep the old profile's colour.
pub struct ThumbSrc {
    pub rgb: Vec<f32>,
    pub w: usize,
    pub h: usize,
    pub profile: String,
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

/// 打开序列的第一份回包：EXIF、分辨率与保真度口径。
///
/// 这一包**不背测光结果**。千帧序列测一遍要几十秒，首帧的分辨率与 EXIF 几百
/// 毫秒就到了：镜头匹配、信息面板、直方图下方那行参数都只等这一包，没有理由
/// 陪整条测光一起堵在后面。测光另走 [`Measured`] 分块回包。
///
/// The first reply after opening a sequence: EXIF, resolution and the fidelity note.
///
/// It deliberately carries **no metering results**. Metering a thousand-frame sequence
/// takes tens of seconds, while the first frame's resolution and EXIF arrive in a few
/// hundred milliseconds: lens matching, the info panel and the capture line under the
/// histogram only wait for this package and have no reason to queue behind the whole
/// metering pass. Metering arrives separately, in chunks, via [`Measured`].
struct Loaded {
    token: u64,
    resolution: Option<(u32, u32)>,
    /// 首帧的保真度口径（RAW 才有：传感器 CFA 还是机身预览）。
    source_note: Option<String>,
    exif: Vec<Exif>,
}

/// 一块测光结果：从第 `lo` 帧起的连续切片。
///
/// 分段回包的理由是「看得见」：状态栏进度、去闪曲线、可读/缺帧计数在千帧导入
/// 里一路更新，而不是最后一次性跳出来。
///
/// One chunk of metering results: a contiguous slice starting at frame `lo`.
///
/// Chunked replies exist so the work is **visible**: the status-line progress, the
/// deflicker curve and the readable/missing counts update along the way instead of
/// appearing all at once at the end.
struct Measured {
    token: u64,
    lo: usize,
    metrics: Vec<Option<FrameMetrics>>,
    def_means: Vec<Option<FrameMeans>>,
    issues: Vec<(usize, String)>,
    /// 最后一块：清 `measuring` 并写汇总。
    ///
    /// The last chunk: clears `measuring` and writes the summary.
    done: bool,
}

struct ThumbMsg {
    index: usize,
    image: ColorImage,
    size: [u32; 2],
    /// 解码源：重调色只换 `image`，源像素原样留着（见 [`ThumbSrc`]）。
    ///
    /// The decoded source: a re-grade swaps only `image`; the source pixels stay.
    src: Arc<ThumbSrc>,
    /// `image` 是按哪一份参数显影的。
    ///
    /// Which parameter set `image` was developed with.
    grade: GradeParams,
}

/// 播放代理回包：形状与 [`ThumbMsg`] 一致，但走自己的消息与纹理命名，绝不与 128px
/// 缩略图共用 `thumb.{i}` 纹理槽（否则两种尺寸会互相 `load_texture` 覆盖）。
///
/// Playback-proxy reply: same shape as [`ThumbMsg`], but it travels on its own message and
/// its own texture name, never sharing the `thumb.{i}` slot with the 128px thumbnail (else
/// the two sizes would overwrite each other via `load_texture`).
struct ProxyMsg {
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

/// 多帧合一的任务种类：HDR 融合、全景拼接、星空堆栈、时间合成。四条路共用一套
/// 后台任务、进度与状态命令，差别只在读帧之后怎么合。
///
/// Kinds of multi-frame merge: HDR fusion, panorama stitching, star stacking and time
/// blending. All four share one background job, progress reporting and status command; only the
/// combining step differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeKind {
    Hdr,
    Pano,
    Stack,
    TimeBlend,
    /// 焦点合成：一组不同对焦距离的帧融成一张全景深。
    ///
    /// Focus stacking: fuse a bracket taken at different focus distances into one
    /// all-in-focus image.
    Focus,
}

impl MergeKind {
    pub(crate) fn cn(self) -> &'static str {
        match self {
            Self::Hdr => "HDR 合并",
            Self::Pano => "全景拼接",
            Self::Stack => "星空堆栈",
            Self::TimeBlend => "时间合成",
            Self::Focus => "焦点合成",
        }
    }

    /// 控制面 `kind` 字段与成片文件名前缀用的短名。
    fn tag(self) -> &'static str {
        match self {
            Self::Hdr => "hdr",
            Self::Pano => "pano",
            Self::Stack => "stack",
            Self::TimeBlend => "timeblend",
            Self::Focus => "focus",
        }
    }

    /// 没设输出目录时落在序列根下的目录名。
    fn dir_name(self) -> &'static str {
        match self {
            Self::Hdr => "HDR",
            Self::Pano => "全景",
            Self::Stack => "堆栈",
            Self::TimeBlend => "时间合成",
            Self::Focus => "焦点合成",
        }
    }

    /// 拼进中文句子的那部分（「写 HDR 成片失败」），与 `cn` 分开是因为 `cn` 自带动作。
    fn noun(self) -> &'static str {
        match self {
            Self::Hdr => "HDR",
            Self::Pano => "全景",
            Self::Stack => "堆栈",
            Self::TimeBlend => "时间合成",
            Self::Focus => "焦点",
        }
    }

    /// 一次能吃的帧数上限，来自各自的核心模块。
    fn max_frames(self) -> usize {
        match self {
            Self::Hdr => aevum_core::hdr::MAX_FRAMES,
            Self::Pano => aevum_core::pano::MAX_FRAMES,
            Self::Stack => aevum_core::stack::MAX_FRAMES,
            Self::TimeBlend => aevum_core::timeblend::MAX_FRAMES,
            Self::Focus => aevum_core::focus::MAX_FRAMES,
        }
    }

    /// 合成前是否要求每帧对齐后同尺寸：HDR、堆栈、时间合成都是同机位叠帧，
    /// 尺寸不齐就必须先对齐或裁齐；全景靠特征点自己求几何，尺寸可以不同。
    ///
    /// Whether every frame must share one size after alignment. HDR, stacking and time blending
    /// all overlay frames from a fixed camera, so mismatched sizes must be aligned or cropped
    /// first; panoramas get their geometry from the features and may differ.
    fn needs_uniform_size(self) -> bool {
        !matches!(self, Self::Pano)
    }
}

/// 合并时用的那套参数：堆栈走统计模式，时间合成走布局与羽化；HDR / 全景不要额外参数。
///
/// The parameter set a merge runs with: a statistical mode for stacking, a layout and feather
/// for time blending; HDR and panorama need nothing extra.
#[derive(Clone, Debug)]
enum MergeSpec {
    Plain,
    Stack(aevum_core::stack::Options),
    TimeBlend(aevum_core::timeblend::Spec),
    Focus(aevum_core::focus::Options),
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
    spec: MergeSpec,
    /// 焦点合成是否先做**合成前自动对齐**（估计 focus breathing 位移场）。
    /// 默认关闭；只有 `focus.merge` 带 `align: true` 时才为真，其余合并种类恒为假。
    ///
    /// Whether this focus merge first runs **pre-fusion auto alignment** (estimating the
    /// focus-breathing shift field). Off by default; true only when `focus.merge` is called
    /// with `align: true`, and always false for the other merge kinds.
    align: bool,
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
///
/// A plugin job in flight: completion receiver, cancel flag and metadata.
///
/// Cancel only raises the flag; the worker thread reaps the child process itself --
/// the UI thread never joins. A hung plugin would freeze the whole UI under `join`,
/// and reaping belongs to whoever does the work.
pub struct PluginJob {
    /// 插件 id（清单里的那份），崩溃计数按它记。
    ///
    /// The plugin id (the one in its manifest); crash counts are keyed on it.
    pub id: String,
    /// 插件显示名，状态栏与消息里露脸的是它。
    ///
    /// The plugin display name -- this is what appears in the status bar and messages.
    pub name: String,
    /// 起任务的时刻，超时兜底与"跑了多久"都用它。
    ///
    /// When the job started; used by the timeout fallback and by "how long has it been
    /// running".
    pub started: Instant,
    rx: Receiver<PluginMsg>,
    cancel: Arc<AtomicBool>,
}

enum Msg {
    Loaded(Box<Loaded>),
    Measured(Box<Measured>),
    Preview(Box<PreviewMsg>),
    Thumbs(Vec<ThumbMsg>),
    /// 解码失败的帧：清掉 pending 标记并计数，失败帧不许把重试通道堵死。
    ///
    /// Frames that failed to decode: clear their pending flags and count them, so one
    /// bad file never deadlocks the retry channel.
    ThumbsFailed(Vec<usize>),
    /// 播放代理回包（[`PROXY_SIDE`] 长边），与胶片条缩略图分开落账。
    ///
    /// Playback-proxy reply ([`PROXY_SIDE`] long edge), recorded separately from the
    /// filmstrip thumbnails.
    Proxies(Vec<ProxyMsg>),
    LibThumbs(Vec<LibThumbMsg>),
    Saved(String),
    /// 预览解码失败。**不碰 `export_busy`**：预览可以随时重算，导出是独占的长任务，
    /// 两者共用一个错误消息会让一次预览失败把导出锁解开，用户就能起第二路导出写同一份
    /// 成片。导出的失败由 `ExportFailed` 自己收尾。
    ///
    /// Preview decode failure. It must **not** touch `export_busy`: a preview can be retried
    /// at any time, while an export is an exclusive long task. Sharing one error message let a
    /// preview failure unlock the export and start a second writer on the same output; export
    /// failures are closed by `ExportFailed` only.
    PreviewFailed(String),
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
    /// `token` 是发任务时的 `reload_token`，换了序列的迟到回包直接丢；`job` 是本次
    /// 合并任务的编号，只有当前任务才有权清 `merge_busy`——旧任务的迟到回包不能把
    /// 新任务的忙碌标记抹掉。
    ///
    /// `token` is the `reload_token` at launch, so a late reply from a switched-away sequence
    /// is dropped; `job` identifies the merge run, and only the current run may clear
    /// `merge_busy` — a stale reply must not wipe the new run's busy flag.
    Merged {
        token: u64,
        job: u64,
        kind: MergeKind,
        result: Result<Value, String>,
    },
    /// 序列体检的后台任务回包：判定清单，或中文失败原因。
    Checked {
        token: u64,
        result: Result<Vec<Finding>, String>,
    },
    /// 时间合成预览的回包：半分辨率合成图 + 顺带带回的解码缓存。
    /// `key` 是发任务时的参数指纹：不是最新指纹的迟到回包只收缓存、不上屏。
    ///
    /// Time-blend preview reply: a half-resolution composite plus the decode cache that
    /// came with it. `key` is the parameter fingerprint at dispatch; a late reply whose
    /// key is stale only refreshes the cache, never the on-screen texture.
    TimeblendPreview {
        token: u64,
        key: u64,
        image: Box<ColorImage>,
        frames: Option<Arc<TbPreviewData>>,
    },
    /// 时间合成预览失败：不致命，分组里挂一行小字，参数再动就重试。
    TimeblendPreviewFail {
        token: u64,
        key: u64,
        error: String,
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
    /// 剔除成子序列的后台任务回包：成功带状态栏 / 历史用的那句话，失败带原因。
    /// `token` 是发任务时的 `reload_token`，换了序列的迟到回包只清忙碌标记、不落账。
    ///
    /// The cull-extract background reply: the sentence for the status bar / history on
    /// success, the reason on failure. `token` is the `reload_token` at launch; a late reply
    /// from a switched-away sequence only clears the busy flag and records nothing.
    CullExtracted {
        token: u64,
        result: Result<String, String>,
    },
    /// 一键自动的后台测光回包：256 长边代理解码完成（或失败）。
    /// 结果进 `auto_probe` 缓存；只有仍是当前帧的回包才立刻重算曝光。
    ///
    /// The one-click-auto background metering reply: the 256-px proxy decoded (or failed).
    /// The result lands in the `auto_probe` cache; only a reply for the frame still on screen
    /// recomputes the exposure right away.
    AutoProbed {
        token: u64,
        path: PathBuf,
        result: Result<(Vec<f32>, usize, usize), String>,
    },
    /// 配乐探针的回包：后台 ffmpeg 读到的编码与时长（或失败原因）。
    /// 只有仍在用这份配乐的回包才落账，换文件的迟到回包直接丢。
    ///
    /// The soundtrack probe reply: the codec and duration a background ffmpeg read (or the
    /// reason it failed). Only a reply for the soundtrack still in use lands; a late reply
    /// for a switched file is dropped.
    AudioProbed {
        path: PathBuf,
        result: Result<AudioProbe, String>,
    },
    /// 更新检查的后台回包：结论 + 发布说明 + 清单地址。
    ///
    /// `token` 是发任务时的 `update_job`。更新任务与序列无关，所以不复用
    /// `reload_token`：换序列不该丢掉一次正在跑的更新检查。
    ///
    /// The update-check background reply: the conclusion plus the release notes and the
    /// manifest URL.
    ///
    /// `token` is `update_job` at dispatch. Update jobs have nothing to do with
    /// sequences, so `reload_token` is not reused -- switching sequence must not throw
    /// away a running check.
    UpdateChecked {
        token: u64,
        result: Box<UpdateCheckOut>,
    },
    /// 下载安装准备的后台回包：备好的脚本，或失败原因。
    /// `job` 与检查共用一套编号：一次新的下载会让更早的下载回包作废。
    ///
    /// The download-and-prepare background reply: the prepared script, or the reason it
    /// failed. `job` shares the numbering with checks: a newer download invalidates the
    /// replies of older ones.
    UpdatePrepared {
        job: u64,
        result: Result<PreparedInstall, String>,
    },
}

/// 状态栏剩余时间的滑动窗口。
///
/// 只留最近 8 秒、最多 16 个 `(时刻, 已完成)` 采样：既够抹平单帧抖动，也不会把
/// 任务刚起跑时那一下的慢速一直背到收尾。任务换了或进度回退就整窗重开。
///
/// Sliding window behind the status bar's remaining-time estimate. It keeps at most 16
/// `(instant, done)` samples from the last eight seconds: enough to smooth single-frame
/// jitter, without carrying the slow start all the way to the end. A task change or a
/// progress reset starts a fresh window.
#[derive(Default)]
struct EtaWindow {
    /// 采样所属的任务名（`导出` / `队列` / `HDR 合并` …）。
    ///
    /// The task the samples belong to.
    key: String,
    /// `(时刻, 已完成数)`，按时间升序。
    ///
    /// `(instant, done)` pairs, oldest first.
    samples: Vec<(Instant, usize)>,
    /// 上一次看到的进度值，用来识别「回退到 0 = 换了一轮」。
    ///
    /// The last progress value seen; a drop back to 0 means a new run.
    last_done: usize,
}

/// 更新窗口的阶段：空闲 / 正在检查 / 正在下载 / 已备好可安装。
///
/// 为什么用一个枚举而不是三个布尔（`busy` + `ready` + `downloading`）：这三个状态
/// 互斥，三个布尔能拼出八种组合，其中五种没有意义，而界面上「现在该显示哪个按钮」
/// 恰好是这四种之一。枚举让非法组合根本写不出来，也让窗口里的 `match` 一眼穷尽。
///
/// Update window phase: idle / checking / downloading / ready to install.
///
/// Why an enum instead of three booleans (`busy` + `ready` + `downloading`): the three
/// states are mutually exclusive, and three booleans spell eight combinations of which
/// five mean nothing -- while "which button shows now" is exactly one of these four. The
/// enum makes the illegal combinations unrepresentable and lets the window's `match`
/// exhaust at a glance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdatePhase {
    /// 没有任务在跑（也可能是上一次失败了，原因在 `update_error` 里）。
    ///
    /// Nothing running (the last attempt may have failed; the reason sits in
    /// `update_error`).
    #[default]
    Idle,
    /// 后台线程正在拉发布信息。
    ///
    /// A background thread is pulling the release metadata.
    Checking,
    /// 后台线程正在下载、校验、解包、写安装脚本。
    ///
    /// A background thread is downloading, verifying, unpacking and writing the
    /// install script.
    Downloading,
    /// 安装脚本已备好，等用户点「立即重启并安装」。
    ///
    /// The install script is ready and waits for "restart and install".
    Ready,
}

/// 一次更新检查的全部产出：给界面看的结论、发布说明正文、清单地址。
///
/// 为什么把三者装在一个结构里：它们来自**同一份**发布 JSON，分成三个字段落账就
/// 可能出现「结论是新版本、正文是上一次的」。一个结构 = 一次发布的快照。
///
/// Everything one update check produced: the conclusion for the UI, the release-notes
/// body, and the manifest URL.
///
/// Why bundle the three: they come from **one** release JSON, and landing them in three
/// separate fields lets "the conclusion is the new version, the body is the previous
/// one" happen. One struct = one release snapshot.
struct UpdateCheckOut {
    /// 结论：当前 / 远端 / 资产 / 一行说明。
    ///
    /// The conclusion: current / latest / asset / one-line note.
    check: aevum_core::update::UpdateCheck,
    /// 发布说明正文，窗口里滚动显示。
    ///
    /// Release-notes body, shown in a scrolling area.
    body: String,
    /// `SHA256SUMS` 资产的直链；发布里没有这一项时为 `None`（那就装不了）。
    ///
    /// Direct URL of the `SHA256SUMS` asset; `None` when the release has no such asset
    /// (which means installing is not possible).
    sums_url: Option<String>,
}

/// 后台线程准备好的安装：版本号（给文案用）+ 待拉起的 `.cmd` 路径。
///
/// The install prepared by the background thread: the version (for the copy) plus the
/// path of the `.cmd` to launch.
#[derive(Clone)]
pub struct PreparedInstall {
    /// 远端版本号（`vX.Y.Z`）。
    ///
    /// The remote version (`vX.Y.Z`).
    pub version: String,
    /// 写好的批处理脚本，内含替换顺序与回滚备份。
    ///
    /// The written batch script, carrying the replacement order and the rollback backup.
    pub script: PathBuf,
    /// 解包出来的新 exe（脚本的 `%~1`）。
    ///
    /// The unpacked new exe (the script's `%~1`).
    pub new_exe: PathBuf,
    /// 要被替换的 exe（脚本的 `%~2`）。
    ///
    /// The exe to replace (the script's `%~2`).
    pub target: PathBuf,
}

pub struct AppState {
    pub seq: Option<Sequence>,
    pub metrics: Vec<Option<FrameMetrics>>,
    pub issues: Vec<(usize, String)>,
    pub analysis: Option<Analysis>,
    /// 逐帧去闪曝光偏移（整帧亮度项），分析后缓存，画曲线与预览共用。
    ///
    /// Per-frame deflicker exposure offset (the whole-frame luminance term), cached after
    /// analysis and shared by the curve and the preview.
    pub corr: Vec<f32>,
    /// 逐帧分通道残差：各通道偏移 − 三通道均值，只在开「分通道」时非零。
    ///
    /// Per-frame per-channel residual: each channel's offset minus the three-channel mean;
    /// non-zero only when per-channel mode is on.
    pub corr_rgb: Vec<[f32; 3]>,
    /// 线性光测光结果，分通道去闪的输入；随导入那趟解码一起算，不重复读图。
    ///
    /// Linear-light metering results, the input for per-channel deflicker; computed during
    /// the import decode pass so the images are not read twice.
    pub def_means: Vec<Option<FrameMeans>>,
    /// 逐帧 EXIF，跟导入那趟一起读（只定位读 IFD，不整读文件）。方向已经应用在解码
    /// 里，这份缓存是给「信息」面板和曝光台阶检测看的。
    ///
    /// Per-frame EXIF, read during the import pass (locates and reads the IFD only, never
    /// the whole file). Orientation has already been applied during decode; this cache
    /// feeds the info panel and the exposure-step detection.
    pub exif: Vec<Exif>,

    pub selected: usize,
    pub module: Module,
    pub tool: Tool,
    pub dock_tab: DockTab,
    /// 折叠中的面板分组。初值来自 [`default_collapsed`]（低频组首屏折起来），
    /// 之后以用户点出来的为准，并随 `settings.json` 落盘——面板记得住你的摆法是
    /// 「减少信息负担」的一半，另一半是首屏别全摊开。
    ///
    /// Panel groups currently folded. The initial value comes from
    /// [`default_collapsed`] (low-frequency groups start folded); afterwards the user's
    /// clicks win and the set is persisted to `settings.json` -- a panel that remembers
    /// your arrangement is half of "reduce the information load", and the other half is
    /// not unrolling everything on the first screen.
    pub collapsed: HashSet<&'static str>,
    /// 分组模式：`true` = 单开（点开一组就收起同模块其他组，ACR 默认），
    /// `false` = 多开。落 `settings.json`，右键分组头可切换。
    ///
    /// Group mode: `true` = single-open (opening a group folds its siblings, ACR's
    /// default), `false` = multi-open. Persisted to `settings.json`; toggled from a
    /// group header's context menu.
    pub panel_solo: bool,
    /// 预览里打开「白以上检视」：把白以上的亮度折进最后一段码值，让 SDR 面板能看见
    /// HDR 高光里还有没有细节。落 `settings.json`（观看方式，不是一次性动作）。
    ///
    /// Preview shows "above white" inspection: fold above-white brightness into the last
    /// slice of the code range so an SDR panel can show whether HDR highlights still carry
    /// detail. Persisted in `settings.json` (a way of looking, not a one-off action).
    pub hdr_preview: bool,
    /// 预设栏三节的次序（`PRESET_SECTIONS` 的一个排列）。拖节头换序，落
    /// `settings.json`；读回时由 [`normalise_preset_order`] 补全成完整排列。
    ///
    /// Order of the three preset-column sections (a permutation of `PRESET_SECTIONS`).
    /// Dragging a section header reorders it and persists to `settings.json`; it is
    /// completed by [`normalise_preset_order`] on load.
    pub preset_order: Vec<&'static str>,
    /// 正在被拖的预设节（`None` = 没在拖）。松手后清空。
    ///
    /// The preset section currently being dragged (`None` = not dragging). Cleared on
    /// release.
    pub drag_section: Option<&'static str>,
    /// 浮窗上次落在哪儿（键 = 窗口标识，见 `shell::windows::WIN_KEYS`）。启动时从
    /// `settings.json` 灌回来，拖动结束后写回同一份文件——见 [`AppState::note_win_pos`]。
    ///
    /// Where each floating window last sat (key = window id, see
    /// `shell::windows::WIN_KEYS`). Loaded from `settings.json` at startup and written
    /// back after a drag ends -- see [`AppState::note_win_pos`].
    pub win_pos: std::collections::BTreeMap<String, [f32; 2]>,
    /// 偏好（折叠态等）改动过、等帧尾统一落盘。界面代码只置位，不直接写文件：
    /// 一次拖动折叠会连着改几处，逐处写会把 `settings.json` 反复搓。
    ///
    /// A preference (folding state and the like) changed and is waiting for the end-of-frame
    /// single write. UI code only sets the flag and never writes the file itself: one fold
    /// drag changes several entries, and writing per entry would grind `settings.json`
    /// over and over.
    pub settings_dirty: bool,
    pub status: String,
    pub error: Option<String>,
    /// 状态栏的常驻告警：护带目录写不进去、override 路径失效之类。
    ///
    /// 与 `error` 语义不同——那是要用户当下处理的弹窗式错误，一出就盖住画面；
    /// 这里是「一直挂在状态栏、点一下才消一条」的背景事实。来源是 core 的
    /// `env::note` 队列，每帧开头 [`AppState::drain_notices`] 搬进来。
    ///
    /// Persistent status-bar warnings: the guard directory is not writable, an override
    /// path went stale, and so on.
    ///
    /// Semantically different from `error` -- that one is a modal error the user must deal
    /// with right now, and it covers the screen the moment it appears; these are standing
    /// background facts that sit in the status bar and are dismissed one at a time. The
    /// source is core's `env::note` queue, drained into here at the start of every frame
    /// by [`AppState::drain_notices`].
    pub notices: Vec<String>,
    /// 打开序列时检测到的同目录 XMP 边车（第一帧）。只提示不自动套用；
    /// 状态栏给一枚「应用边车」，点了才进参数（见 `apply_sidecar_hint`）。
    ///
    /// An XMP sidecar found next to the sequence (checked on the first frame). Only hinted,
    /// never applied automatically; the status bar offers an "apply sidecar" button, and the
    /// parameters change only once it is clicked (see `apply_sidecar_hint`).
    pub sidecar_hint: Option<PathBuf>,
    /// 打开序列后的测光进度：`Some(帧数)` 表示正在测量这么多帧，`None` 表示已经测完。
    /// 存帧数而不是一个 bool，是因为状态栏那句「正在测量 N 帧…」要能一直说到
    /// 最后一块测光落账为止——中途任何一次状态改写都不该把它换成一个假的汇总。
    ///
    /// Metering progress after opening a sequence: `Some(frame count)` while that many frames
    /// are being measured, `None` once done. A count rather than a bool so the "measuring N
    /// frames…" status can stand until the last chunk lands, and no intermediate status write
    /// replaces it with a summary that claims the work is already done.
    pub measuring: Option<usize>,
    /// 当前这一趟测光的取消旗。换序列 / 关图时置位，后台线程在块与块之间看一眼就
    /// 收工——否则千帧被测完之前切走，旧序列的解码会继续把核吃满。
    ///
    /// 每趟新开一个（与 `cancel_merge` 同理）：沿用同一个原子会让新一趟的
    /// `store(false)` 把旧一趟的取消要求抹掉。
    ///
    /// Cancel flag of the current metering pass. Raised on sequence switch / close; the
    /// background thread checks it between chunks and stops -- otherwise switching away
    /// before a thousand frames finish leaves the old sequence's decodes eating cores. A
    /// fresh atom **per pass** (like `cancel_merge`): sharing one lets the new pass's
    /// `store(false)` undo the old pass's cancellation.
    cancel_measure: Arc<AtomicBool>,

    pub params: GradeParams,
    pub preset: String,
    pub auto_note: Option<String>,
    /// 自动校正的 256 长边代理：同一帧重复点自动不重复解码。
    auto_probe: Option<(PathBuf, Vec<f32>, usize, usize)>,
    /// 正在后台解码测光代理的帧路径：同一帧的在飞请求不重复发（防抖），
    /// 回包落地时清空。
    ///
    /// The frame whose 256-px auto probe is decoding in the background: while one request is
    /// in flight, repeats for the same frame are suppressed (debounce); cleared when the
    /// reply lands.
    auto_probe_inflight: Option<PathBuf>,

    pub def_window: f64,
    pub def_strength: f64,
    pub def_multi: bool,
    /// 分通道（RGB）去闪烁：压彩色场景的色闪，代价是可能引入轻微色偏漂移。
    ///
    /// Per-channel (RGB) deflicker: suppresses the colour flicker of colourful scenes at the
    /// cost of possible slight hue drift.
    pub def_channels: bool,
    /// 按 EXIF 曝光三要素预对齐（圣杯过渡）：把相机补偿剥掉，只压真正的测光误差。
    ///
    /// Pre-align on the EXIF exposure triplet (the holy-grail transition): strip the camera
    /// compensation so deflicker only trims real metering error.
    pub def_align_exif: bool,

    pub aspect: Aspect,
    pub grid: bool,
    pub cross: bool,
    pub split: bool,
    /// 前后对比分割线的位置（0..1，图宽比例）。视图状态：不进历史快照、
    /// 不落盘——LR 也不记录你把分割线拉到了哪儿。双击手柄回到居中。
    ///
    /// Position of the before/after split line (0..1, as a fraction of image width). View
    /// state: not part of the history snapshot and not persisted -- Lightroom does not
    /// remember where you dragged the split either. Double-clicking the handle re-centres
    /// it.
    pub split_pos: f32,
    /// 胶片条的用户高度。None = 自动（按缩略图比例适配图片）；Some(h) =
    /// 拖过之后钉住的高度。拖动条在胶片条上边缘，双击回到自动。
    ///
    /// The user's filmstrip height. None = automatic (fit to the thumbnail aspect); Some(h)
    /// = pinned after a drag. The drag handle is the filmstrip's top edge; double-click
    /// returns to automatic.
    pub film_h: Option<f32>,
    pub zoom: f32,
    /// 画布这一帧算出的「适应窗口」倍率，供缩放读数与 1:1 使用。
    ///
    /// The "fit to window" scale computed for this frame's canvas, used by the zoom readout
    /// and by 1:1.
    pub fit_scale: f32,
    pub pan: Vec2,

    pub playing: bool,
    last_play: Instant,
    pub font_level: usize,
    /// 界面语言偏好。**只存在 settings.json 里，不进工程文件** —— 工程是跨机器
    /// 打开的东西，语言该跟随打开它的那台机器，不该被作者的语言钉住。
    ///
    /// UI language preference. **Lives only in settings.json, never in the project file** --
    /// a project is opened across machines, and the language should follow the machine
    /// opening it rather than being pinned by the author's.
    pub lang: crate::i18n::LangPref,

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
    /// 「眼睛」旁路：这些分组的参数暂不参与预览渲染。只活在内存里——不进工程、
    /// 不进 settings、不进历史（它是对比工具，不是调色状态），重启即清空。
    ///
    /// The "eye" bypass: parameters of these groups temporarily stay out of the preview
    /// render. Memory-only -- not in the project, not in settings, not in history (it is
    /// a comparison aid, not grading state) -- and cleared on restart.
    pub bypassed: BTreeSet<&'static str>,

    pub thumbs: HashMap<usize, Thumb>,
    /// 缩略图淘汰的上次运行时刻：节流用，别每帧排序上千个键。
    ///
    /// Last thumbnail-trim instant: a throttle, so we do not sort a thousand-plus keys
    /// every frame.
    pub(crate) thumb_trim_at: Option<std::time::Instant>,
    thumb_pending: HashSet<usize>,
    /// 缩略图的解码源（见 [`ThumbSrc`]）：只给「参数变了重调色」用，不参与绘制。
    ///
    /// Decoded thumbnail sources (see [`ThumbSrc`]): only for re-grading when parameters
    /// change, never drawn directly.
    thumb_src: HashMap<usize, Arc<ThumbSrc>>,
    /// 正在后台重调色的帧号：一批没回来之前不重复排队，免得拖推子时堆一串任务。
    ///
    /// Frames whose re-grade is in flight: no duplicate queueing until the batch returns,
    /// so a slider drag cannot pile up jobs.
    thumb_regrade: HashSet<usize>,
    /// 播放用的较大尺寸代理纹理（纯内存，不落盘、不进 Snapshot）。播放态画布吃这一档，
    /// 暂停后回全质量；纹理名用 `proxy.{i}`，与 128px 的 `thumb.{i}` 分开。
    ///
    /// Larger proxy textures used during playback (memory only, never persisted, never in
    /// the Snapshot). The canvas uses them while playing and returns to full quality on
    /// pause; texture names are `proxy.{i}`, distinct from the 128px `thumb.{i}`.
    pub proxies: HashMap<usize, Thumb>,
    /// 正在后台取代理的帧号，避免重复排队。
    ///
    /// Frames whose proxy is being fetched, so we do not queue twice.
    pub proxy_pending: HashSet<usize>,
    /// 当前裁剪拖动的世代号（纯内存）。`crop_begin` 递增置位、`crop_end` 清零；
    /// 历史合并据此把「同一次拖动内即使手停很久」的连续改动并成一步，详见
    /// [`crate::history`]。0 = 不在拖动中。
    ///
    /// The current crop-drag epoch (memory only). `crop_begin` increments and sets it,
    /// `crop_end` clears it; the history merge uses it so that all changes inside one drag
    /// share a single step no matter how long the hand pauses -- see [`crate::history`].
    /// 0 = not dragging.
    pub drag_epoch: u64,
    /// 胶片条星级筛选下限（纯内存，不落盘、不进 Snapshot）：0 = 不过滤，1..=5 = 只显示
    /// 该星级及以上的帧。供胶片条使用。
    ///
    /// Filmstrip minimum-star filter (memory only, never persisted, never in the Snapshot):
    /// 0 = no filter, 1..=5 = show only frames at this rating or above. Consumed by the
    /// filmstrip.
    pub film_filter_stars: u8,
    /// 胶片条旗标筛选（纯内存）：0 = 不过滤，1 = 只看选用，-1 = 只看排除。
    ///
    /// Filmstrip flag filter (memory only): 0 = no filter, 1 = picks only, -1 = rejects only.
    pub film_filter_flag: i8,
    /// 胶片条色标筛选（纯内存）：0 = 不过滤，1..=4 = 只看该色标。
    ///
    /// Filmstrip colour-label filter (memory only): 0 = no filter, 1..=4 = that label only.
    pub film_filter_label: u8,
    pub scroll_to_selected: bool,

    pub ratings: HashMap<usize, u8>,
    pub flags: HashMap<usize, i8>,
    pub labels: HashMap<usize, u8>,
    /// 打开中的序列自己的关键词；工程保存时原样写回，图库改词也不丢。
    ///
    /// Keywords belonging to the open sequence; written back unchanged when the project is
    /// saved, so keyword edits made in the library are not lost.
    pub keywords: HashMap<usize, Vec<String>>,
    /// 打开中的序列所属目录的堆叠（已归一化）；工程保存时原样写回。
    ///
    /// Stacks of the folder the open sequence belongs to (already normalised); written back
    /// unchanged when the project is saved.
    pub stacks: Vec<Vec<usize>>,
    /// 打开中的序列所属目录的说明与版权信息；随工程文件落盘，导出静帧与成片都按它写。
    ///
    /// Description and copyright of the folder the open sequence belongs to; persisted with
    /// the project file and applied to both still and video exports.
    pub meta: project::Metadata,

    /// 图库：跨目录的根目录清单（galleries.json）与当前浏览的编目缓存。
    /// 缩略图按路径键，换序列不清——图库浏览的是没打开的目录。
    ///
    /// Library: the cross-folder root list (galleries.json) and the catalogue cache for the
    /// folder being browsed. Thumbnails are keyed by path and survive a sequence switch --
    /// the library browses folders that are not open.
    pub lib_roots: Vec<PathBuf>,
    pub lib_sel: Option<usize>,
    /// 编目里选中的行（键盘 0~5/P/X/6~9 作用在这一行）。
    ///
    /// The selected row in the catalogue (the keyboard's 0-5 / P / X / 6-9 act on this
    /// row).
    pub lib_sel_file: Option<usize>,
    /// 编目筛选：星级下限、只看选用、色标。
    ///
    /// Catalogue filter: minimum star rating, picks only, colour label.
    pub lib_filter: aevum_core::library::Filter,
    /// 编目里勾选的行（Ctrl 多选 / Shift 连选）；键盘和右键作用在它们身上。
    ///
    /// Checked rows in the catalogue (Ctrl multi-select / Shift range); the keyboard and the
    /// context menu act on these.
    pub lib_checked: std::collections::BTreeSet<usize>,
    /// Shift 连选的锚点。
    ///
    /// Anchor for Shift range selection.
    pub lib_anchor: Option<usize>,
    /// 编目里展开的叠（目录序号, 叠号）；纯视图状态，不进工程、不进历史。
    ///
    /// Expanded stacks in the catalogue (folder index, stack index); pure view state, not
    /// part of the project and not part of the history.
    pub lib_stack_open: HashSet<(usize, usize)>,
    pub lib_entries: Vec<Vec<aevum_core::library::LibFile>>,
    /// 手动集合与当前打开的集合视图；新建集合的名字输入框也住这儿。
    ///
    /// Manual collections and the currently open collection view; the name input for a new
    /// collection lives here too.
    pub lib_cols: Vec<aevum_core::library::Collection>,
    pub lib_col: Option<usize>,
    pub lib_col_name: String,
    /// 编目里给选中/勾选行加词的输入框。
    ///
    /// The input box for adding keywords to the selected or checked rows in the catalogue.
    pub lib_keyword: String,
    /// 图库信息面板的编辑缓冲，以及它当前是哪个目录的（换目录要重新读，
    /// 否则会把上一个目录的四项写到这个目录头上）。
    ///
    /// Editing buffer for the library info panel, plus which folder it currently belongs to
    /// (switching folders must re-read, otherwise the previous folder's four fields get
    /// written onto this one's header).
    pub lib_meta: project::Metadata,
    pub lib_meta_root: Option<PathBuf>,
    /// 信息面板开没开（编目视图右上角的「信息」）。
    ///
    /// Whether the info panel is open (the "info" toggle at the top-right of the catalogue
    /// view).
    pub lib_meta_open: bool,
    pub lib_thumbs: HashMap<PathBuf, Thumb>,
    lib_thumb_pending: HashSet<PathBuf>,
    /// 撤销 / 重做历史；只收参数类写入，见 [`history`]。
    ///
    /// Undo / redo history; accepts parameter-class writes only, see [`history`].
    pub history: History,
    pub hist: Option<[[u32; 256]; 3]>,

    /// 错误弹窗的正文。`Some` 时以模态框显示；用户关掉后清空。
    ///
    /// 与 `error` 的分工：`error` 是状态栏那一行常驻摘要（可点掉），这里是**必须
    /// 看见一次**的弹窗。失败不许只在状态栏滑过去，更不许只打控制台。
    ///
    /// Body of the error dialog. When `Some`, a modal shows it; closing clears it.
    ///
    /// Division of labour with `error`: `error` is the persistent status-bar summary (can be
    /// dismissed in place), while this is the dialog that must be seen once. A failure may
    /// not slip by in the status bar, and certainly not only in the console.
    pub error_modal: Option<String>,
    /// 命令面板是否打开（`Ctrl+K`）。
    ///
    /// 纯会话态，不落盘：面板开着重启没有意义，反而会挡住第一眼看到的画面。
    ///
    /// Whether the command palette is open (`Ctrl+K`). Session only -- reopening the app
    /// with a palette on screen would have no meaning and would cover the first glance.
    pub palette_open: bool,
    /// 面板里的搜索串。
    ///
    /// The palette's search string.
    pub palette_query: String,
    /// 当前高亮行（命中列表的下标）。
    ///
    /// Index of the highlighted row in the hit list.
    pub palette_sel: usize,
    /// 刚打开的那一帧要把焦点交给输入框，之后不再抢。
    ///
    /// True for the frame the palette opened, so focus is handed to the field exactly once.
    pub palette_fresh: bool,
    /// 异步回调里的失败（截图保存这类「命令先返回、结果晚到」的路径）。
    ///
    /// 命令已经 `Ok` 返回，错误只能在回调线程里产生，那里拿不到 `&mut AppState`。
    /// 用一个共享槽把消息带回来，`poll()` 每帧取一次并抬成 `error` —— 否则这类失败
    /// 只会打在控制台上，用户永远看不见。
    ///
    /// Failures from async callbacks (screenshot saving and other paths that return first and
    /// finish later). The command already returned `Ok`; the error can only appear in the
    /// callback thread, which has no `&mut AppState`. A shared slot carries the message back,
    /// and `poll()` promotes it to `error` each frame -- otherwise such failures only reach
    /// the console and the user never sees them.
    pub async_errors: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// 上一帧已展示过的错误内容，用来判断「这一帧是不是新错误」。
    ///
    /// The error text already surfaced last frame, used to tell whether this frame carries a
    /// new failure.
    error_seen: Option<String>,
    pub show_dock: bool,
    /// 左侧工作流面板是否展开。
    ///
    /// 与 `show_left`（预设与快照浮窗）是两个独立开关：前者是"现在做到哪一步、下一步是什么"，
    /// 后者是"我存过的预设"。合并成一个会让用户为了看步骤而打开预设列表。
    ///
    /// Whether the left workflow panel is expanded. Independent of `show_left` (the presets
    /// window): one answers "which step am I on, what is next", the other "what have I saved".
    /// Merging them would force opening a preset list just to see the steps.
    pub show_flow: bool,
    /// 用户在步骤条里点选的步骤（0 基）。视图状态，不是进度——高亮与工作流面板
    /// 跟随它；`None` 表示跟随派生的当前步。点「下一步」或切换工作流时清掉。
    ///
    /// The step the user clicked in the step bar (0-based). View state, not progress:
    /// the highlight and the workflow panel follow it; `None` means "follow the derived
    /// current step". Cleared by "next" or when switching workflows.
    pub flow_step: Option<usize>,
    /// 当前选中的工作流（延时 / 单张修图 / 多帧合成）。用户手选，不跟着模块跳。
    ///
    /// The selected workflow (timelapse / single-photo / multi-frame merge). Chosen by the
    /// user; it does not follow the active module.
    pub flow_task: FlowTask,
    /// 上一帧的布局开关快照。帧首比对一次就自动置 `settings_dirty`，免得每个
    /// 新增的开关赋值点都要记得手工标脏（漏一处就"重启后又不记得了"）。
    ///
    /// Last frame's layout switches. Diffing once at frame start marks `settings_dirty`
    /// automatically, so a new toggle site cannot forget to persist itself.
    layout_switches: (bool, bool, bool, &'static str),
    /// 右侧调整面板宽度（可拖动）。默认 `theme::DOCK_W`，夹在 300–560 之间。
    ///
    /// 面板宽度原本是编译期常量，用户一旦在窄窗里被挤窄就再也调不回来
    /// （2026-11-07 反馈「变成这样似乎变不回去了」）。改成运行期量，由面板内缘
    /// 的拖拽手柄改它；只存会话内，不落盘。
    ///
    /// Width of the right-hand panel, draggable. Defaults to `theme::DOCK_W` and is
    /// clamped to 300–560. It used to be a compile-time constant, so a panel squeezed in a
    /// narrow window could never be restored (user feedback, 2026-11-07). It now lives at
    /// runtime and is changed by the drag handle on the panel's inner edge; session only,
    /// not persisted.
    pub dock_w: f32,
    /// 对话框中枢：待弹队列、在弹标记、结果通道，见 `ops_dialog`。
    ///
    /// Dialog hub: queue, in-flight flag and the reply channel; see `ops_dialog`.
    pub(crate) dialog_queue: std::collections::VecDeque<crate::state::ops_dialog::DialogRequest>,
    pub(crate) dialog_in_flight: bool,
    pub(crate) dialog_tx: std::sync::mpsc::Sender<(
        crate::state::ops_dialog::DialogPurpose,
        Option<crate::state::ops_dialog::DialogPick>,
    )>,
    pub(crate) dialog_rx: std::sync::mpsc::Receiver<(
        crate::state::ops_dialog::DialogPurpose,
        Option<crate::state::ops_dialog::DialogPick>,
    )>,
    /// 「另存为」入队时选定的容器：后缀以按下那一刻为准。
    ///
    /// The container fixed at "save as" enqueue time; the extension follows it.
    pub(crate) dialog_still: Option<aevum_core::export::StillFormat>,
    /// 「预设与快照」浮窗（预设 / 快照 / 调色副本）开没开。字段名沿用旧左栏
    /// （`show_left`）——settings.json 的键没变，老用户的开关状态照常读回；
    /// 2026-11-04 起它不再是常驻侧栏而是浮窗（docs/界面布局-ACR化-2026-11-04.md §2）。
    ///
    /// Whether the Presets & Snapshots window (presets / snapshots / grade copies) is
    /// open. The field name carries over from the old left column (`show_left`) -- the
    /// settings.json key is unchanged, so existing toggles still load; since
    /// 2026-11-04 it is a floating window rather than a resident sidebar (plan §2).
    pub show_left: bool,
    /// 最外那条 28px 竖条（网格 / 胶片 / 曲线）开没开。
    ///
    /// Whether the outermost 28px rail (grid / filmstrip / curve) is open.
    pub show_strip: bool,
    /// 手动存的快照：界面稿左栏「快照」那一节的内容。
    ///
    /// 和撤销历史是两回事：历史每动一次参数就记一步，几天后翻不到"那天满意的版本"；
    /// 快照是用户自己按下去钉住的一版，只有手动删。存的是整份 [`Snapshot`]，
    /// 点一下就把参数整组换回去（同样进历史，可以再撤）。
    ///
    /// Manually saved snapshots: the content of the "snapshots" section in the left column.
    ///
    /// A different thing from the undo history: the history records a step per parameter
    /// change, and a few days later you cannot find "the version I liked that day"; a
    /// snapshot is a version the user pinned deliberately and only he deletes. It stores a
    /// whole [`Snapshot`], and one click swaps the entire parameter set back (which itself
    /// enters the history and can be undone).
    pub snapshots: Vec<(String, history::Snapshot)>,
    /// 外观参数（明暗 / 皮肤 / 主色 / 圆角）。改动后要同步给 `theme::set_appearance`
    /// 并重刷一次 `theme::apply`，再落盘。
    ///
    /// Appearance parameters (light/dark, skin, accent, radius). After a change, hand them to
    /// `theme::set_appearance`, re-run `theme::apply`, then persist.
    pub appearance: crate::theme::Appearance,
    /// 布局：状态栏 / 工具轨 / 调整面板 的位置，以及底部胶片条显隐。
    ///
    /// Layout: positions of the status bar, tool rail and adjustment panel, plus whether the
    /// bottom filmstrip shows.
    pub layout: crate::layout::Layout,
    /// 外观窗是否打开。
    ///
    /// Whether the appearance window is open.
    pub look_open: bool,
    /// 插件注册表：内置清单 + `plugins/` 下扫到的清单。
    ///
    /// Plugin registry: the built-in list plus the manifests scanned under `plugins/`.
    pub registry: aevum_core::plugin::Registry,
    /// 插件市场窗口是否打开。
    ///
    /// Whether the plugin marketplace window is open.
    pub market_open: bool,
    /// 最近一次全量预览的 CPU 像素（RGB8 + 尺寸）。
    ///
    /// 插件跑在别的进程里，拿不到 GPU 纹理，只能给它字节。只存全量帧：
    /// 拖动中的草图帧每帧都会来，每帧转一份 8 MB 不值得。
    ///
    /// CPU pixels of the most recent full preview (RGB8 plus dimensions).
    ///
    /// Plugins run in another process and cannot see GPU textures, so they get bytes.
    /// Only full frames are stored: drag-time sketch frames arrive every frame, and
    /// converting 8 MB per frame is not worth it.
    pub preview_rgb: Option<(Vec<u8>, [u32; 2])>,
    /// 剪切警告（PS/LR 的黑白场检查）：高光遮罩开关。开着时画布上把
    /// r、g、b 全部顶满的像素盖成红色——过曝一眼见底。
    ///
    /// Clipping warning (the black/white point check from PS / LR): highlight mask toggle.
    /// When on, pixels where r, g and b are all maxed are painted red on the canvas --
    /// overexposure becomes obvious at a glance.
    pub clip_highlights: bool,
    /// 剪切警告：阴影遮罩开关。r、g、b 全部压到底的像素盖成蓝色。
    ///
    /// Clipping warning: shadow mask toggle. Pixels where r, g and b are all bottomed out
    /// are painted blue.
    pub clip_shadows: bool,
    /// 剪切遮罩纹理：随最近一次全量预览生成（红=高光裁切、蓝=阴影裁切、
    /// 其余透明）。草图帧不重算——遮罩不需要跟手，等全量帧补上。
    ///
    /// Clipping mask texture, generated with the most recent full preview (red = highlight
    /// clipping, blue = shadow clipping, everything else transparent). Sketch frames do not
    /// recompute it -- the mask does not need to keep up with the drag, it waits for the
    /// full frame.
    pub clip_mask: Option<TextureHandle>,
    /// 生成遮罩时对应的预览版本；预览一换就作废重算。
    ///
    /// The preview version this mask was generated for; a new preview invalidates it and
    /// it is recomputed.
    pub clip_mask_stamp: u64,
    /// 插件算完的结果纹理。有它时画布显示它，参数一变就作废。
    ///
    /// The result texture the plugin produced. While it exists the canvas shows it; any
    /// parameter change invalidates it.
    pub plugin_result: Option<TextureHandle>,
    /// 结果对应的预览版本。拿它判断"底图到底变没变"——
    /// 只看"又收到一张预览"是不够的：同一张图重传一次会把结果白扔。
    ///
    /// The preview version this result corresponds to. This is what decides "did the
    /// underlying image actually change" -- merely seeing "another preview arrived" is not
    /// enough: re-sending the same image would throw the result away for nothing.
    pub plugin_stamp: u64,
    /// 在飞的插件后台任务；`None` = 空闲。见 [`PluginJob`]。
    ///
    /// The plugin background job in flight; `None` = idle. See [`PluginJob`].
    pub plugin_job: Option<PluginJob>,
    /// 插件进度 (done, total)，每帧从任务通道拉最新；空闲时是 (0, 0)。
    ///
    /// Plugin progress (done, total), pulled from the job channel each frame; (0, 0) when
    /// idle.
    pub plugin_progress: (u32, u32),
    /// 每个插件 id 的失败/崩溃次数（用户取消不计）。连续 3 次就在错误里
    /// 提示去插件市场停用 —— 只提示，不自动禁用，别替用户做决定。
    ///
    /// Failure / crash count per plugin id (user cancellation is not counted). After three
    /// in a row the error suggests disabling it in the marketplace -- suggest only, never
    /// disable automatically; that decision is not ours to make.
    pub plugin_crashes: HashMap<String, u32>,
    /// 市场左栏的类别筛选；`None` = 全部。
    ///
    /// Category filter in the marketplace's left column; `None` = all.
    pub market_filter: Option<aevum_core::plugin::Kind>,
    /// 市场搜索框。
    ///
    /// The marketplace search box.
    pub market_query: String,
    /// 截图演示：让停靠面板直接滚到最底部。
    ///
    /// Screenshot demo: make the dock panel scroll straight to the bottom.
    pub dock_scroll_end: bool,
    /// 截图演示：把某个分组（`group_header` 的 key）滚到面板顶部。
    ///
    /// Screenshot demo: scroll one group (a `group_header` key) to the top of the panel.
    pub dock_focus: Option<String>,
    pub show_film: bool,
    pub show_curves: bool,
    /// 画布标尺。界面稿的画布是净图，没有那两条 24px 的刻度槽，
    /// 所以默认关；要量像素时从「视图」打开。
    ///
    /// Canvas rulers. The design's canvas is a clean image without those two 24px scale
    /// troughs, so this is off by default; open it from the View menu when you need to
    /// measure pixels.
    pub rulers: bool,
    pub about_open: bool,

    /// 更新窗口是否打开（菜单「帮助 → 检查更新…」）。
    ///
    /// Whether the update window is open (Help -> Check for updates).
    pub update_open: bool,
    /// 最近一次检查的结论；`None` = 这个进程里还没查过。
    ///
    /// 启动静默检查失败时这里是一个「失败」结论（`latest` 为空），界面照它显示一行
    /// 原因，菜单文案则因为 `installable()` 为假而保持「检查更新…」。
    ///
    /// The conclusion of the latest check; `None` = not checked in this process yet.
    ///
    /// When the silent startup check fails this holds a "failed" conclusion (`latest` is
    /// empty): the window shows the reason verbatim, while the menu keeps saying "check for
    /// updates" because `installable()` is false.
    pub update_check: Option<aevum_core::update::UpdateCheck>,
    /// 发布说明正文（同上，与结论同源）。
    ///
    /// Release-notes body (same source as the conclusion above).
    pub update_body: String,
    /// `SHA256SUMS` 的直链；下载校验要用，`UpdateCheck` 里装不下（那是「装哪个包」
    /// 的结论，不是发布资产清单）。
    ///
    /// Direct URL of `SHA256SUMS`, needed by the download-and-verify step. `UpdateCheck`
    /// cannot carry it: that type answers "which package to install", not "what assets
    /// the release has".
    pub(crate) update_sums_url: Option<String>,
    /// 更新流程当前阶段，界面按它决定显示哪颗按钮。
    ///
    /// The current phase of the update flow; the window picks its button from it.
    pub update_phase: UpdatePhase,
    /// 上一次失败的原因（下载 / 校验 / 解包 / 写脚本），红字一行显示；成功后清空。
    ///
    /// Why the last attempt failed (download / verify / unpack / write), shown as one red
    /// line and cleared on success.
    pub update_error: Option<String>,
    /// 下载进度：`(已写字节, 资产声明字节)`。后台线程写、界面线程读。
    ///
    /// 为什么用共享内存而不是每条进度发一条消息：`download_to_file` 的进度回调
    /// 每 50 ms 一次，一秒二十条消息只为了把一个数搬进 `AppState`，而 `poll` 每帧
    /// 都要把它们全部排空；共享一个元组只要一次无竞争的加锁，而且不会因为界面卡了
    /// 两帧就积压出二十条过期的进度。
    ///
    /// Download progress: `(bytes written, bytes declared)`. Written by the background
    /// thread, read by the UI thread.
    ///
    /// Why shared memory instead of one message per tick: `download_to_file` calls the
    /// progress callback every 50 ms, so a message per tick is twenty messages a second to
    /// move one number into `AppState`, all of which `poll` then has to drain every frame;
    /// a shared tuple costs one uncontended lock and cannot pile up twenty stale readings
    /// when the UI stalls for two frames.
    pub update_progress: Arc<Mutex<(u64, u64)>>,
    /// 已备好的安装（脚本 + 三个路径），只有 `update_phase == Ready` 时非空。
    ///
    /// The prepared install (script plus the three paths), non-empty only when
    /// `update_phase == Ready`.
    pub update_ready: Option<PreparedInstall>,
    /// 发任务的编号：每起一次更新任务 +1，迟到回包按它丢弃。
    ///
    /// The job counter: bumped for every update job, so a late reply can be dropped.
    pub(crate) update_job: u64,
    /// 上次检查完成的时间（Unix 秒），随 `settings.json` 落盘给静默检查做节流。
    ///
    /// When the last check finished (Unix seconds); persisted through `settings.json` to
    /// throttle the silent check.
    pub last_update_check: Option<u64>,
    /// 「回滚到上一版」的二次确认窗是否打开。
    ///
    /// Whether the "roll back to the previous version" confirmation is open.
    pub rollback_confirm: bool,

    pub keyframes: BTreeMap<usize, GradeParams>,
    /// 命名调色副本（当前序列工程文件里的那一份）；存/删/改名都进撤销栈。
    ///
    /// Named grading copies (the ones in the current sequence's project file); saving,
    /// deleting and renaming all enter the undo stack.
    pub copies: Vec<GradeCopy>,
    /// 导出时改用某套副本的调色（`export.stills` / `export.video` 的 copy 参数）；
    /// 界面上点「导出」走当前调色，这个字段只在命令通道指定过之后非空，用完即清。
    ///
    /// Use one copy's grade for the export (the `copy` argument of `export.stills` /
    /// `export.video`); clicking Export in the UI uses the current grade, so this is
    /// non-empty only after the command channel specified it, and it is cleared after
    /// use.
    pub export_copy: Option<String>,
    /// 「调色副本」分组里的名字输入框；纯视图缓冲，不进工程也不进历史。
    ///
    /// The name input in the grading copies group; a pure view buffer, not part of the
    /// project or the history.
    pub copy_name: String,
    /// 「HDR 合并」分组的包围半径：当前帧前后各取几帧；纯视图缓冲。
    ///
    /// Bracket radius of the HDR merge group: how many frames to take on each side of the
    /// current one; a pure view buffer.
    pub hdr_around: u32,
    /// 最近一次 HDR 合并写出的路径 / 报错，分组里回显一行；纯视图缓冲。
    ///
    /// Path or error of the most recent HDR merge, echoed as one line inside the group; a
    /// pure view buffer.
    pub hdr_last: Option<String>,
    pub hdr_error: Option<String>,
    /// 「全景合并」分组的相邻半径：从当前帧起向后取几张接片；纯视图缓冲。
    ///
    /// Neighbour radius of the panorama merge group: how many frames to stitch forward from
    /// the current one; a pure view buffer.
    pub pano_around: u32,
    /// 最近一次全景合并写出的路径 / 报错，分组里回显一行；纯视图缓冲。
    ///
    /// Path or error of the most recent panorama merge, echoed as one line inside the group;
    /// a pure view buffer.
    pub pano_last: Option<String>,
    pub pano_error: Option<String>,
    /// 「星空堆栈」分组的包围半径、统计模式与两个旋钮；纯视图缓冲，不落工程。
    ///
    /// Bracket radius, statistical mode and the two knobs of the star-stacking group; pure view
    /// buffers, never written to the project.
    pub stack_around: u32,
    pub stack_mode: aevum_core::stack::Mode,
    pub stack_sigma: f32,
    pub stack_fill: bool,
    /// 最近一次堆栈写出的路径 / 报错，分组里回显一行；纯视图缓冲。
    ///
    /// Path or error of the most recent stack, echoed as one line inside the group; a pure view
    /// buffer.
    pub stack_last: Option<String>,
    pub stack_error: Option<String>,
    /// 「时间合成」分组的半径、布局与羽化；纯视图缓冲，不落工程。
    ///
    /// Radius, layout and feather of the time-blend group; pure view buffers, never written to
    /// the project.
    pub tb_around: u32,
    pub tb_layout: aevum_core::timeblend::Layout,
    pub tb_bands: u32,
    pub tb_feather: f32,
    pub tb_horizon: f32,
    /// 自动推地平线（B7）：按行亮度梯度找天与地的分界，找不到退回 `tb_horizon`。
    ///
    /// Auto horizon (B7): find the sky-ground boundary from the per-row brightness gradient,
    /// falling back to `tb_horizon` when nothing is found.
    pub tb_horizon_auto: bool,
    /// 去鬼影阈值（EV，0 = 关，B6）：羽化区里移动物不再拖成两份。
    ///
    /// Deghosting threshold (EV, 0 = off, B6): moving subjects stop doubling up in the feather
    /// zone.
    pub tb_deghost: f32,
    pub tb_match: bool,
    /// 最近一次时间合成写出的路径 / 报错，分组里回显一行；纯视图缓冲。
    ///
    /// Path or error of the most recent time blend, echoed as one line inside the group; a pure
    /// view buffer.
    pub tb_last: Option<String>,
    pub tb_error: Option<String>,
    /// 时间合成预览：已上屏的纹理与其参数指纹。
    ///
    /// Time-blend preview: the on-screen texture with its parameter fingerprint.
    pub tb_preview: Option<(u64, TextureHandle)>,
    /// 预览的原始像素（与纹理同源）：测试读它断言内容，也留给别的面板复用。
    ///
    /// The preview's raw pixels (same source as the texture): tests assert on it and other
    /// panels may reuse it.
    pub tb_preview_img: Option<Arc<ColorImage>>,
    /// 最近一次请求（或已失败）的参数指纹：相同指纹不重发任务。
    ///
    /// The latest requested (or failed) parameter fingerprint; identical keys never re-dispatch.
    pub tb_preview_key: u64,
    pub tb_preview_inflight: bool,
    /// 半分辨率解码缓存：帧选择没变就复用，换帧才重新解码。
    ///
    /// Half-resolution decode cache: reused while the frame selection is unchanged.
    pub tb_preview_data: Option<Arc<TbPreviewData>>,
    /// 预览失败的小字说明；参数一变就清。
    ///
    /// The small failure note for the preview; cleared as soon as parameters change.
    pub tb_preview_error: Option<String>,
    /// 「焦点合成」分组的参数与回显；纯视图缓冲，不落工程。
    ///
    /// Parameters and echo line of the focus-stacking group; pure view buffers, never
    /// written to the project.
    pub focus_around: u32,
    pub focus_method: aevum_core::focus::Method,
    pub focus_radius: u32,
    pub focus_threshold: f32,
    pub focus_smooth_z: u32,
    pub focus_feather: f32,
    /// 合成前自动对齐（默认关）：先估每帧 8×6 网格位移、再逐块补偿，最后按 0.5% 的收益
    /// 门槛决定采用还是回退。只影响 `focus.merge`，其余合并种类一律不用。
    ///
    /// Pre-fusion auto alignment (off by default): estimate each frame's 8x6 grid shift,
    /// compensate block by block, then adopt or revert against a 0.5% gain floor. Only
    /// `focus.merge` reads it; the other merge kinds never do.
    pub focus_align: bool,
    pub focus_last: Option<String>,
    pub focus_error: Option<String>,
    /// 深度图预览纹理：「预览深度图」按下时前台算出的降采样归属图。
    ///
    /// The depth-map preview texture: the downsampled ownership map computed on the
    /// spot when "preview depth map" is pressed.
    pub focus_depth_tex: Option<TextureHandle>,
    /// 预览下方的一行说明（尺寸 / 耗时 / 参考帧）；失败时改放原因。参数或取帧
    /// 一变就整块作废，与纹理同生共死。
    ///
    /// The caption under the preview (size / elapsed / reference frame); holds the
    /// failure reason instead when the computation fails. Voided wholesale as soon
    /// as parameters or the frame selection change, living and dying with the texture.
    pub focus_depth_note: Option<String>,
    /// 筛选台的四条判据阈值；纯视图缓冲，不落工程（拒收标记本身落在 `flags` 里）。
    ///
    /// The four thresholds of the cull station; pure view buffers, never written to the project
    /// (the rejection marks themselves live in `flags`).
    pub cull_ev_low: f64,
    pub cull_ev_high: f64,
    pub cull_jump: f64,
    pub cull_sharp: f64,
    /// 上一次评估的一句话结论（「12 帧会被判废」/「干净」），分组里回显。
    ///
    /// One-line result of the last evaluation ("12 frames would be culled" / "clean"), echoed
    /// inside the group.
    pub cull_note: Option<String>,
    /// 「剔除成子序列」的目标目录输入框缓冲；纯视图缓冲，不落工程。
    ///
    /// The target-folder input buffer of the subsequence extraction; a pure view buffer, never
    /// written to the project.
    pub cull_target: String,
    /// 画布上叠印逐帧时间戳（帧号 · 拍摄时间 · 机型曝光）。
    ///
    /// Burn the per-frame timestamp onto the canvas (frame number, capture time, camera and
    /// exposure).
    pub stamp_on: bool,
    pub resolution: Option<(u32, u32)>,
    /// 首帧读出来的保真度口径，只有 RAW 才有；「信息」面板用它标注预览/传感器两条路。
    ///
    /// The fidelity setting read from the first frame, present only for RAW; the info panel
    /// uses it to label the preview and sensor paths.
    pub source_note: Option<String>,

    /// 当前选中的局部蒙版；画布手柄与右侧面板共用。
    ///
    /// The local mask currently selected; shared by the canvas handles and the right panel.
    pub mask_sel: Option<usize>,
    /// 选中蒙版组里当前编辑的形状（0 基）；换组时回到第一个。
    ///
    /// The shape being edited inside the selected mask group (0-based); returns to the first
    /// when the group changes.
    pub mask_comp: usize,
    /// 正在拖动的 (蒙版序号, 手柄 0/1)。手柄 2 = 整体移动（抓在形状主体上）。
    ///
    /// The (mask index, handle 0/1) currently being dragged. Handle 2 = move the whole
    /// shape (grabbed on its body).
    pub mask_drag: Option<(usize, u8)>,
    /// 整体移动的锚点：(按下时指针, 按下时的四个端点坐标)。拖动 = 快照 + 位移，
    /// 松手即弃；手柄拖动不走这里。
    ///
    /// Anchor for whole-shape moves: (pointer at press, the four endpoint coords at
    /// press). Drag = snapshot + delta, dropped on release; handle drags bypass it.
    pub mask_move_anchor: Option<(egui::Pos2, [f64; 4])>,
    /// 正在涂抹的画笔笔画：(蒙版序号, 形状序号)。松手结束，一笔进一步历史。
    ///
    /// The brush stroke being painted: (mask index, shape index). Ends on mouse release;
    /// one stroke, one history step.
    pub mask_paint: Option<(usize, usize)>,
    /// 下一笔的画笔设定：半径按短边百分比（0.2~50），硬度 / 流量 0~100。
    /// 只影响新笔画，不回写已画的；这是画笔的「当前工具状态」，不进撤销历史。
    ///
    /// Brush settings for the next stroke: radius as a percentage of the short edge
    /// (0.2-50), hardness / flow 0-100.
    /// Affects new strokes only and never rewrites strokes already painted; this is the
    /// brush's current tool state and does not enter the undo history.
    pub brush_radius: f64,
    pub brush_hardness: f64,
    /// 修复笔刷半径：占画面**长边**的百分比（0.5~8）。定尺寸的是落笔那一刻的值，
    /// 之后改推子不回头改已经落下的点——和画笔半径同一条规矩。
    ///
    /// Healing-brush radius as a percentage of the frame's **long edge** (0.5-8). Each stroke
    /// takes the value at the moment it lands; moving the slider later does not resize it,
    /// the same rule as the mask brush radius.
    pub heal_radius: f64,
    /// 修复笔刷羽化：占半径的百分比（0~100）。0 是硬边，100 从中心淡到边缘。
    ///
    /// Healing-brush feather as a percentage of the radius (0-100). 0 is a hard edge, 100
    /// fades from the centre all the way out.
    pub heal_feather: f64,
    /// 仿制模式：先点一下**取源**，之后每一点都从「源 + 位移」搬运像素。
    /// 关着是修复（斑内解拉普拉斯，用周边像素当边界）。
    ///
    /// Clone mode: the first tap **picks the source**, and every later tap transports pixels
    /// from "source plus offset". Off means heal (solve the Laplacian inside the spot with the
    /// surroundings as the boundary).
    pub heal_clone: bool,
    /// 已取的仿制源（整幅归一化坐标）。`None` 表示下一次点击是取源而不是落笔。
    ///
    /// The picked clone source (normalized frame coordinates). `None` means the next tap picks
    /// the source instead of laying a stroke.
    pub heal_src: Option<(f32, f32)>,
    /// 上一次落点的画布坐标：拖动时按间距补点，免得一帧一个点把笔触刷成一片。
    ///
    /// The canvas position of the last stroke: dragging lays points at a spacing, otherwise one
    /// point per frame would smear the stroke into a blob.
    pub heal_last: Option<egui::Pos2>,
    /// 四角拖拽态：画布画出透视四边形，四个角可以拖。自动直立解不出来时的兜底路径——
    /// 自动解需要「画面里有一条像样的竖直主直线」，室内广角往往没有。
    ///
    /// Corner-drag mode: the canvas draws the perspective quad and its four corners can be
    /// dragged. This is the fallback when auto-upright finds nothing, which is common in wide
    /// interiors with no usable dominant vertical.
    pub upright_editing: bool,
    pub brush_flow: f64,
    pub brush_erase: bool,
    /// 吸管取色态：下一次画布单击把像素色相写进选中蒙版的颜色范围。
    ///
    /// Eyedropper state: the next canvas click writes the pixel hue into the selected mask's
    /// colour range.
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
    ///
    /// The lens profile match description (shown both in the panel and by `lens.set`).
    pub lens_profile_note: String,
    /// 用户关掉自动匹配时暂存的命中校正，再开时原样恢复。
    ///
    /// The matched correction stashed when the user turns automatic matching off, so that
    /// turning it back on restores it verbatim.
    pub lens_profile_saved: Option<aevum_core::lensdb::Correction>,

    /// DCP 相机配置的显示名与说明；路径本身在 `params.dcp_profile`。
    ///
    /// Display name and description of the DCP camera profile; the path itself is in
    /// `params.dcp_profile`.
    pub dcp_name: String,
    pub dcp_note: String,
    /// LCP 镜头配置的说明；校正系数本身在 `params.lcp_profile`。
    ///
    /// Description of the LCP lens profile; the correction coefficients themselves are in
    /// `params.lcp_profile`.
    pub lcp_note: String,

    /// 用户拖出来的裁剪框（对齐后坐标，原图像素）。`None` = 没手动裁，
    /// 取景框退化成「画幅比例在安全框里居中内接」。
    ///
    /// The crop box the user dragged out (post-align coordinates, original image pixels).
    /// `None` = no manual crop, and the viewfinder degrades to "the aspect ratio inscribed
    /// and centred in the safe frame".
    pub user_crop: Option<CropRect>,
    /// 拉直角度（度，正角=画面逆时针）。取景框量的是转过之后的那一帧，
    /// 所以角度一变安全框就跟着缩，框伸到空白角里的部分导出时会被削掉。
    ///
    /// Straightening angle (degrees, positive = image counter-clockwise). The box is
    /// measured on the frame after rotation, so changing the angle shrinks the safe frame
    /// with it, and the parts of the box reaching into the blank corners get cut on
    /// export.
    pub rotate: f64,
    /// 画布上正在进行的裁剪拖拽。拖框只动这个矢量覆盖层，不重画预览：
    /// 裁剪态下预览本来就是整幅画面，取景框松手后离开裁剪态才落到像素上。
    ///
    /// The crop drag in progress on the canvas. Dragging the box moves only this vector
    /// overlay and does not re-render the preview: in crop mode the preview is the whole
    /// frame anyway, and the box lands on pixels only once the mouse is released and crop
    /// mode is left.
    pub crop_drag: Option<CropDrag>,
    /// 画布上正在拖的运镜框（与裁剪拖框互斥：同一时刻只有一个人拿鼠标）。
    /// 几何与裁剪同构，所以直接复用 `CropDrag` 的三种定性。
    ///
    /// The motion box being dragged on the canvas (mutually exclusive with the crop drag:
    /// only one of them holds the mouse at a time). The geometry is isomorphic to cropping,
    /// so the three variants of `CropDrag` are reused directly.
    pub motion_drag: Option<CropDrag>,
    /// 画布交给运镜取景框编辑：构图面板的「拖取景框」开关。开着时画布上的把手
    /// 拉的是起点/终点框（由 `motion_box` 定），裁剪框只画不拖。
    ///
    /// The canvas is handed over to editing the motion viewfinder: the "drag viewfinder"
    /// toggle in the composition panel. While on, canvas handles pull the start / end box
    /// (chosen by `motion_box`) and the crop box is drawn but not draggable.
    pub motion_picking: bool,
    /// 现在拖的是起点框还是终点框。
    ///
    /// Whether the start box or the end box is being dragged right now.
    pub motion_box: MotionBox,

    /// HSL 面板当前编辑的色相带（0..7，与 `lut::HSL_CENTERS` 同序）。
    ///
    /// The hue band the HSL panel is editing (0..7, same order as `lut::HSL_CENTERS`).
    pub hsl_band: usize,
    /// 曲线面板当前编辑的通道：0=RGB、1=红、2=绿、3=蓝。
    ///
    /// The channel the curve panel is editing: 0=RGB, 1=red, 2=green, 3=blue.
    pub curve_chan: usize,
    /// 曲线区的模式片（ACR 的 Curve 面板同构）：0=参数曲线、1=点曲线、2=色阶。
    /// 纯视图状态，不进历史也不进工程，重启回到参数曲线。
    ///
    /// Mode chips of the Curve section (mirrors ACR's Curve panel): 0=parametric,
    /// 1=point, 2=levels. Pure view state -- not part of the history or the project;
    /// resets to the parametric tab on restart.
    pub curve_tab: usize,
    /// 颜色混合器区的模式片（ACR 的 Color Mixer 同构）：0=HSL、1=黑白。
    ///
    /// Mode chips of the Color Mixer section (mirrors ACR): 0=HSL, 1=B&W.
    pub mixer_tab: usize,
    /// 「全部」页当前属性：0=色相 1=饱和度 2=明亮度。
    ///
    /// The "All" tab property: 0 = hue, 1 = saturation, 2 = luminance.
    pub hsl_all_prop: usize,
    /// 曲线 / 色阶正在拖动的控制点或把手序号。
    ///
    /// Index of the control point or handle being dragged on the curve / levels.
    pub curve_grab: Option<usize>,
    pub levels_grab: Option<usize>,
    /// 速度曲线正在拖动的键序号（视图状态，不进历史也不进工程）。
    ///
    /// Index of the key being dragged on the speed curve (view state; not part of the
    /// history or the project).
    pub retime_grab: Option<usize>,

    pub output_w: u32,
    pub output_h: u32,
    pub output_fps: f32,
    pub codec: String,
    pub export_busy: bool,
    pub last_saved: Option<String>,
    /// 无显示环境（selftest）下不启动任何读盘线程。
    ///
    /// Under a display-less environment (selftest) start no disk threads at all.
    pub headless: bool,
    /// 是否允许开回环控制服务。`--shot` 这类一次性模式关掉，避免和常驻 GUI
    /// 抢同一份 control.json。
    ///
    /// Whether the loopback control server may start. One-shot modes such as `--shot` turn
    /// it off so they do not fight the resident GUI over the same control.json.
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
    ///
    /// JPEG compression quality (1-100); lossless containers ignore it.
    pub render_quality: u8,
    /// 把源 JPEG 的 EXIF 块搬进静帧输出（版权、机身、曝光跟着走）。
    ///
    /// Carry the source JPEG's EXIF block into still exports (copyright, camera body and
    /// exposure travel with it).
    pub render_keep_exif: bool,
    /// 每张导出的静帧旁边写一个同名 `.xmp` 旁车（说明版权四项 + 这一张的调色）。
    ///
    /// Write a same-named `.xmp` sidecar next to every exported still (the four copyright
    /// fields plus this image's grade).
    pub render_xmp_sidecar: bool,
    /// 输出锐化：按用途分档，只在缩放之后的成片上生效。
    ///
    /// Output sharpening: graded by use, applied only to the resized result.
    pub render_sharpen_for: OutputSharpen,
    pub render_sharpen_strength: SharpenStrength,
    /// 输出色域：静态图嵌 ICC，视频写 VUI。跟 render_format 一样是成片级设置。
    ///
    /// Output colour space: stills embed an ICC profile, video writes the VUI. Like
    /// render_format, this is a per-export setting.
    pub render_color_space: ColorSpace,
    /// 输出动态范围：SDR 或 HDR（PQ）。跟色域一样是成片级设置。
    ///
    /// `HdrPq` 会改最后一步的传递函数（参考白 203 nits）并切到 10bit，只允许配
    /// AVIF + Rec.2020——[`Self::apply_dynamic`] 负责把不成立的组合补齐。
    ///
    /// Output dynamic range: SDR or HDR (PQ). A per-export setting like the colour space.
    ///
    /// `HdrPq` swaps the final transfer function (203-nit reference white) and switches to
    /// 10bit, allowed only with AVIF + Rec.2020; [`Self::apply_dynamic`] fills in an illegal
    /// combination.
    pub render_dynamic: OutputDynamic,
    /// 水印：整块设定跟着工程文件与 `render.set` 走；默认关，导出像素一个不动。
    ///
    /// Watermark: the whole block travels with the project file and `render.set`; off by
    /// default, and while it is off not a single exported pixel changes.
    pub render_watermark: Watermark,
    /// 时间重映射（变速 + 运动模糊）：跟着 `retime.set` 与工程文件走；默认关，
    /// 导出走原来那条 1:1 快路，一个像素都不重采样。
    ///
    /// Time remapping (speed plus motion blur): travels with `retime.set` and the project
    /// file; off by default, and while it is off the export takes the original 1:1 fast path
    /// and resamples nothing.
    pub retime: Retime,
    /// 运镜（Ken Burns）：取景框从起点走到终点，跟着 `motion.set` 与工程文件走；
    /// 默认关，且两个框重合时也当作没开。
    ///
    /// Motion (Ken Burns): the viewfinder travels from the start box to the end box, driven
    /// by `motion.set` and the project file; off by default, and treated as off when the two
    /// boxes coincide.
    pub motion: Motion,
    pub render_crf: u8,
    pub render_preset: String,
    pub render_audio: Option<PathBuf>,
    pub render_audio_opts: AudioOptions,
    /// 配乐探针结果；文件换了才重新读，避免每帧都起进程。
    ///
    /// Soundtrack probe result; re-read only when the file changes, so no process is spawned
    /// every frame.
    pub audio_info: Option<AudioProbe>,
    pub audio_probe_err: Option<String>,
    /// 配乐探针在飞：后台 ffmpeg 还在读这份配乐的编码与时长。探针期间配乐页的
    /// 控件保持可看不可动，回包落地后自动恢复。
    ///
    /// A soundtrack probe is in flight: a background ffmpeg is still reading this file's codec
    /// and duration. Audio-page controls stay visible but inert until the reply lands.
    pub audio_probe_busy: bool,
    pub render_out_dir: PathBuf,
    pub render_video_out: PathBuf,
    pub render_keep_frames: bool,
    /// 断点续跑：输出目录里已经写完的帧原样留着，这一趟只补没写出来的那些。
    ///
    /// Resume: frames already written in the output directory are left as they are, and this
    /// pass only fills in the missing ones.
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
    ///
    /// Long-running HDR / panorama merge. Fusing nine 6K frames takes about six seconds,
    /// which as a blocking UI-thread operation is a dead window with no repaint, so it is
    /// shaped like an export: runs in the background, the UI only reads counters.
    pub merge_busy: bool,
    pub merge_kind: Option<MergeKind>,
    /// 最近一次成功的成片 JSON（路径、帧号、尺寸），`merge.status` 与面板都读它。
    ///
    /// The most recent successful merge JSON (paths, frame count, dimensions); read by
    /// `merge.status` and by the panel.
    pub merge_result: Option<Value>,
    pub merge_error: Option<String>,
    merge_done: Arc<AtomicUsize>,
    merge_total: Arc<AtomicUsize>,
    /// 当前合并任务的取消旗。**每个 job 新开一个**：沿用同一个原子会让新任务
    /// `store(false)` 把旧任务的取消要求抹掉，旧线程继续往成片里写。
    ///
    /// Cancel flag of the current merge. A fresh atom **per job**: sharing one lets a new
    /// run's `store(false)` undo the old run's cancellation, and the old thread keeps writing.
    cancel_merge: Arc<AtomicBool>,
    /// 合并任务编号，每次 `start_merge` 递增；回包按它对号，旧任务的迟到回包既不落账
    /// 也不清忙碌标记。
    ///
    /// Merge run id, bumped by every `start_merge`; replies are matched against it so a stale
    /// reply neither records results nor clears the busy flag.
    merge_job: u64,

    /// 序列体检的后台任务。测光随导入那趟就算好了，这里只是逐帧 `stat` 一遍
    /// 再跑判定——五千帧的 `stat` 也要 100ms 量级，所以与合并同构放后台，
    /// 不放界面线程上。
    ///
    /// The sequence health-check background job. Metering is already computed during the
    /// import pass; this only `stat`s each frame and runs the verdict -- and `stat` over
    /// five thousand frames is still on the order of 100 ms, so it runs in the background
    /// like the merge rather than on the UI thread.
    pub qa_busy: bool,
    /// 最近一次的体检清单（`qa_run` 为假就是还没跑过，空表是「跑过、干净」）。
    ///
    /// The findings of the most recent check (a false `qa_run` means it has never run; an
    /// empty list means "it ran and came back clean").
    pub qa_findings: Vec<Finding>,
    pub qa_run: bool,
    pub qa_error: Option<String>,
    /// 判定松紧：离群门槛是本序列波动的多少倍。面板滑杆与 `sequence.check` 共用。
    ///
    /// Verdict tightness: the outlier threshold as a multiple of this sequence's
    /// fluctuation. Shared by the panel slider and `sequence.check`.
    pub qa_sig: f64,
    qa_done: Arc<AtomicUsize>,
    qa_total: Arc<AtomicUsize>,
    cancel_qa: Arc<AtomicBool>,

    /// 剔除成子序列（`cull.extract`）的后台复制任务。GB 级整批复制放在界面线程上
    /// 就是几秒到几分钟的死窗口，与合并同构：后台跑，回包经 [`Msg::CullExtracted`]
    /// 落账；复制期间面板上的「导出子序列」按钮禁用。
    ///
    /// The background copy job for `cull.extract`. A GB-scale bulk copy on the UI thread is a
    /// dead window lasting seconds to minutes, so it runs in the background like a merge; the
    /// reply lands via [`Msg::CullExtracted`], and the "extract subsequence" button is
    /// disabled while the copy is in flight.
    pub cull_export_busy: bool,

    /// 批量渲染队列；存盘位置在漫游配置目录，与工程分开。
    ///
    /// The batch render queue; its file lives in the roaming config directory, separate
    /// from the project.
    pub queue: Vec<QueueItem>,
    pub queue_path: PathBuf,
    pub queue_busy: bool,
    pub queue_current: Option<usize>,
    queue_done: Arc<AtomicUsize>,
    queue_total: Arc<AtomicUsize>,
    queue_stage: Arc<AtomicU8>,
    cancel_queue: Arc<AtomicBool>,

    last_auto_save: Instant,
    /// 状态栏「约剩 Xs」的取样窗口，只服务当前显示的那个长任务。
    ///
    /// Sampling window behind the status bar's "about Xs left"; it serves only the long task
    /// currently on screen.
    eta: EtaWindow,
    /// 上一次真正写进工程文件的内容。当前状态和它一致就不碰盘。
    /// `None` 表示这份序列还没存过（打开序列后的第一次心跳必存）。
    saved_snap: Option<Snapshot>,
    /// 同上，但管的是进不了 [`Snapshot`] 的那部分：对齐报告与取景裁切。
    saved_stamp: Option<ProjectStamp>,
    reload_token: u64,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,

    /// 本机回环控制服务；只在打开序列后存在，Drop 时清理 control.json。
    ///
    /// The loopback control server on this machine; exists only while a sequence is open,
    /// and cleans up control.json when dropped.
    pub control: Option<ControlServer>,
    /// AI 面板的输入行与最近记录；与回环控制共用同一个命令执行器。
    ///
    /// The AI panel's input line and recent records; shares one command executor with the
    /// loopback control channel.
    pub ai_input: String,
    pub ai_log: Vec<AiLogEntry>,
    control_tx: Sender<ControlEvent>,
    control_rx: Receiver<ControlEvent>,
}

/// 界面里编码器的两个字符串口径：`"H.264"` 与 `"HEVC"`——面板 chips、工程文件与
/// `contains("264")` 的判断都认这两个，别处不许再冒出第三种写法。
///
/// The two codec spellings the UI uses: `"H.264"` and `"HEVC"`. The panel chips, the project
/// file and every `contains("264")` test agree on exactly these; a third spelling would
/// silently disagree with all of them.
fn codec_label_for(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::H265 => "HEVC",
    }
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

// AppState 的操作面按域拆到 `state/ops_*.rs`：一个文件一个子系统，文件头写明边界。
// 全在同一个 crate 里，固有 impl 可以分散到子模块，私有字段对子模块可见。
//
// `AppState`'s operation surface is split by domain into `state/ops_*.rs`: one file per
// subsystem, with its boundary stated in the file header. All of it is the same crate,
// so inherent impls can live in submodules and still see the private fields.
mod ops_agent;
mod ops_align_cmd;
mod ops_audio;
mod ops_cache;
mod ops_command;
mod ops_copy;
mod ops_crop;
mod ops_demo;
pub(crate) mod ops_dialog;
mod ops_export_run;
mod ops_export_write;
mod ops_library;
mod ops_library_cmd;
mod ops_mask;
mod ops_mask_cmd;
mod ops_merge_cmd;
mod ops_motion;
mod ops_presets;
mod ops_project;
mod ops_queue;
mod ops_retouch;
mod ops_session;
mod ops_undo;
mod ops_update;
mod ops_view;

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

/// 测光专用线程池：**不占满全核**。
///
/// 导入千帧时测光要解每帧的整幅像素（整幅 RAW 去马赛克或整幅 JPEG），走全局池
/// 会把每个逻辑核排满，界面线程与预览渲染一起被挤下去（用户 2026-11-07 报告：
/// 1387 帧导入期间只剩 4 fps）。这里给界面 / 预览留两个核，测光的并行度按
/// `核数 - 2` 封顶。
///
/// Dedicated metering pool: **it does not take every core**.
///
/// Importing a thousand frames decodes each frame's full pixels (full RAW demosaic or a
/// full-size JPEG), and running that on the global pool queues every logical core, pushing
/// the UI thread and preview rendering aside (user report, 2026-11-07: 4 fps while
/// importing 1387 frames). Two cores are left to the UI / preview, and metering's own
/// parallelism is capped at `cores - 2`.
fn measure_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(4);
        rayon::ThreadPoolBuilder::new()
            .num_threads(cores.saturating_sub(2).max(1))
            .thread_name(|i| format!("aevum-measure-{i}"))
            .build()
            .expect("测光线程池")
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
    decode_half: bool,
    want_original: bool,
    hdr_fold: bool,
) -> Result<PreviewParts, String> {
    // 两件事必须分开，别再绑成一个 `sketch`：
    //
    // * `sketch`   —— **减 pass**：跳过降噪/锐化/清晰度/纹理这些最贵的空间算子。
    // * `decode_half` —— **减分辨率**：按 `SKETCH_SIDE`（800px）解码，省一半像素。
    //
    // 绑在一起时，拖动期间画布拿到的是 800px 贴图放大到 1600 逻辑宽，看起来就是糊的
    // （用户 2026-11-08 实测：「滑动调色条时照片会模糊，这里不能这样」）。减分辨率
    // 只在**一帧画面都还没有**的时候划算——那是首屏抢时间；一旦已经在看图，模糊比
    // 慢一拍更刺眼，此时减 pass 不减分辨率：清晰度不变，只是细节略简化，松手补全量。
    //
    // Two things must stay separate instead of riding on one `sketch` flag:
    //
    // * `sketch`      -- fewer passes: skip the expensive spatial operators (denoise,
    //                   sharpening, clarity, texture).
    // * `decode_half` -- fewer pixels: decode at `SKETCH_SIDE` (800 px).
    //
    // Tied together, a drag showed an 800 px texture stretched to a 1600 px logical width,
    // which simply looks blurry (user report, 2026-11-08: "the photo blurs while I drag the
    // slider, that cannot happen"). Dropping resolution only pays off when there is no
    // picture on screen yet -- that is first-screen latency. Once the user is looking at the
    // image, blur is worse than one slower frame, so keep the resolution and only cut passes.

    // 基准开着的时候把解码与调色各量一段（见 `PreviewPhases`）；关着时这里只是一次
    // relaxed 原子读，热路径上没有别的代价。
    //
    // While the benchmark is on, decode and grade are timed separately (see
    // `PreviewPhases`); when it is off this is a single relaxed atomic load.
    let trace = PREVIEW_TRACE_ON.load(Ordering::Relaxed);
    let t0 = Instant::now();
    let stamp = source_stamp(path);
    let (mut buf, w, h, full) =
        decoded_preview_source(path, stamp, decode_half, &params.dcp_profile)?;
    let decode_dt = t0.elapsed();
    let (fw, fh) = full;
    let scale = match full_size {
        Some((img_w, _)) if img_w > 0 => fw as f64 / img_w as f64,
        _ => 1.0,
    };
    // 取景按全分辨率预览算；半尺寸草图只有一半大，画布那边仍按这个逻辑尺寸铺满。
    //
    // Framing is computed against the full preview; a half-size sketch is only half as large
    // and the canvas still lays it out at this logical size.
    let crop = crop.and_then(|c| scale_crop(c, scale, fw, fh));
    let mut framing = framing;
    framing.off = [framing.off[0] * scale, framing.off[1] * scale];
    let logical = output_size(fw, fh, crop);
    // 分屏关着时这张图既不显示也不上传，没必要每帧取景再拷一份。
    let original = if !want_original {
        None
    } else if let Some(hit) = cached_original(path, stamp, crop, framing, &params.dcp_profile) {
        Some(hit)
    } else if !decode_half && params.dcp_profile.is_empty() {
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
    // 半尺寸草图的坐标 = 全量坐标 × (草图边长 / 全量边长)。不能写死 0.5：半尺寸按
    // `SKETCH_SIDE` 解码，边长比是算出来的（1600×1067 的草图是 800×533，比正好 0.5；
    // 小图原样不减半，比就是 1.0）。取景框与位移必须按这个比换算，否则松手换全量时
    // 构图会跳。全分辨率草图这一项恒为 1.0，走同一条路不需要特例。
    //
    // Half-size sketch coordinates are full coordinates times (sketch side / full side). The
    // ratio is computed rather than pinned at 0.5: the half decode uses `SKETCH_SIDE`, so a
    // 1600x1067 preview becomes 800x533 (exactly 0.5) while a picture small enough not to be
    // scaled keeps 1.0. A full-resolution sketch keeps 1.0 and needs no special case.
    let (crop, framing) = if decode_half {
        let k = w as f64 / fw.max(1) as f64;
        (
            crop.and_then(|c| scale_crop(c, k, w, h)),
            Framing {
                angle_deg: framing.angle_deg,
                off: [framing.off[0] * k, framing.off[1] * k],
            },
        )
    } else {
        (crop, framing)
    };

    // HDR 检视：渲染保留白以上，再折进可见段。两条都做——只渲不折会被成图那一步
    // 钳成 1.0（和 SDR 一模一样），只折不渲则根本没有白以上可折。
    //
    // HDR inspection: render keeping above-white, then fold it into the visible range. Both
    // steps are needed -- rendering alone gets clamped when the texture is made (identical to
    // SDR), and folding alone has nothing above white to fold.
    let t1 = Instant::now();
    if sketch {
        if hdr_fold {
            aevum_core::render_preview_hdr_sketch(&mut buf, w, h, params, gain);
        } else {
            aevum_core::render_sketch(&mut buf, w, h, params, gain);
        }
    } else if hdr_fold {
        aevum_core::render_preview_hdr(&mut buf, w, h, params, gain);
    } else {
        aevum_core::render_preview(&mut buf, w, h, params, gain);
    }
    if hdr_fold {
        aevum_core::fold_above_white_in_place(&mut buf);
    }
    let grade_dt = t1.elapsed();
    if trace {
        *PREVIEW_PHASES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(PreviewPhases {
            decode: decode_dt,
            grade: grade_dt,
            src: [w, h],
        });
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

/// 解码一张预览尺寸的源图。全量与草图各占一槽。
///
/// 拖一次推子能连发十几帧渲染，而源图在拖动期间一个字都不会变：6000×4000 的原片
/// 解码加缩放到 1600 边要 300ms 上下，比整条调色链还贵，每帧重算是卡顿的第一来源。
/// 每档只留一槽是刻意的——换帧时旧帧立刻让位，常驻内存封顶两张预览图。
///
/// `half` 要的是草图（拖动与首屏的源）：**按 [`SKETCH_SIDE`] 直接解小图**，不再
/// 「整幅解到 1600 再减半」——后者把解码成本原样付了一遍，减半只省了调色那 1ms。
/// 草图槽记着全量预览的边长（逻辑尺寸靠它报，草图与全量必须一致），但**不含**全量
/// 像素：全量预览、直方图/曲线分析、导出照旧走 `half = false` 那一槽的整幅解码结果。
/// 草图欠一张全量这件事没变，见 `preview_owes_full`。
///
/// 草图的像素与全量**不完全相同**：RAW 走的是半尺寸去马赛克（2×2 单元估计，不展开），
/// 其余源退回整幅解码再 2×2 均值减半。两条路的舍入不同，差一两个码值属预期——草图只
/// 在拖动与首屏上出现几帧，随后必然被全量替换。
///
/// Decodes one preview-sized source. Full and sketch each hold their own slot. `half` is the
/// sketch (drag and first screen) and **decodes small at [`SKETCH_SIDE`]** instead of decoding
/// to 1600 and halving -- the latter paid the decode in full and saved only the 1ms grade.
/// The sketch slot remembers the full preview's side lengths (the logical size has to match)
/// but holds **no** full pixels: the full preview, histogram/curve analysis and export keep
/// using the `half = false` slot. The sketch still owes a full frame (`preview_owes_full`).
/// Sketch pixels are **not** bit-identical to the full preview: RAW goes through the
/// half-resolution demosaic (2x2 cell estimates, never expanded) and every other source falls
/// back to a full decode plus a 2x2 mean. The rounding differs by a code value or two, which
/// is expected -- the sketch only lives for the drag and the first screen.
fn decoded_preview_source(
    path: &std::path::Path,
    stamp: SourceStamp,
    half: bool,
    profile: &str,
) -> Result<PreviewSourceView, String> {
    let (mut slots, sketch) = source_slot(path, stamp, profile, half)?;
    let src = if sketch {
        slots.sketch.as_mut()
    } else {
        slots.full.as_mut()
    }
    .expect("source_slot 填过才有返回值");
    // 记下「这一次」草图走的路。放在线程本地而不是读回全局槽：判据的几条测试是并行跑
    // 的，它们共用同一个 `PREVIEW_SRC`，事后读槽会读到别人清空/换掉的槽——2026-11-04
    // 就是这么读到 `None` 的，与解码走哪条路无关。
    //
    // Record the route this very render took. Thread-local rather than reading the global slot
    // back: the fence's tests run in parallel and share `PREVIEW_SRC`, so a read-back can see
    // another test's cleared slot -- that is how a `None` showed up on 2026-11-04, saying
    // nothing about which route the decoder took.
    LAST_SKETCH_ROUTE.with(|cell| cell.set(if sketch { Some(src.half) } else { None }));
    Ok((src.rgb.clone(), src.w, src.h, src.full))
}

/// 缓存槽：没有、换了帧、换了相机配置、换了去马赛克方法或换了档位就重新解码，
/// 然后把锁交给调用方。
fn source_slot(
    path: &std::path::Path,
    stamp: SourceStamp,
    profile: &str,
    sketch: bool,
) -> Result<(std::sync::MutexGuard<'static, PreviewSlots>, bool), String> {
    let side = if sketch { SKETCH_SIDE } else { PREVIEW_SIDE };
    let mut slots = PREVIEW_SRC
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let slot = if sketch {
        &mut slots.sketch
    } else {
        &mut slots.full
    };
    // 去马赛克方法是解码侧配置：诊断面板切换后旧缓存必须作废重解。
    let demosaic = aevum_core::raw::demosaic_method();
    let fresh = !matches!(
        slot.as_ref(),
        Some(c) if c.path == path
            && c.stamp == stamp
            && c.side == side
            && c.profile == profile
            && c.demosaic == demosaic
    );
    if fresh {
        let (buf, w, h, full, half) = if sketch {
            let src = metrics::decode_preview_sketch(path, profile, SKETCH_SIDE, PREVIEW_SIDE)
                .map_err(|e| crate::i18n::err_text(&e))?;
            (src.rgb, src.w, src.h, src.full, src.half)
        } else {
            let (buf, w, h) = metrics::decode_profile_preview(path, profile, PREVIEW_SIDE)
                .map_err(|e| crate::i18n::err_text(&e))?;
            (buf, w, h, (w, h), false)
        };
        if w == 0 || h == 0 || full.0 == 0 || full.1 == 0 {
            return Err("解码结果为空".to_owned());
        }
        *slot = Some(PreviewSource {
            path: path.to_path_buf(),
            stamp,
            side,
            profile: profile.to_owned(),
            demosaic,
            w,
            h,
            rgb: buf,
            full,
            half,
        });
    }
    Ok((slots, sketch))
}

/// 基准用的分步耗时：解码与调色各一段，其余由调用方从总时间里减出来。
///
/// 只在 `PREVIEW_TRACE_ON` 打开时记录。关着的时候热路径上只有一次 relaxed 原子读。
///
/// Per-phase timings for the benchmark: one segment for decode and one for grading; the rest
/// is derived from the total. Recorded only while `PREVIEW_TRACE_ON` is set; when it is off the
/// hot path pays a single relaxed atomic load.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct PreviewPhases {
    /// 解码（含相机配置与缩放）耗时。
    ///
    /// Time spent decoding (including the camera profile and the scaling).
    pub decode: Duration,
    /// 调色耗时。
    ///
    /// Time spent grading.
    pub grade: Duration,
    /// 送进调色的缓冲尺寸。
    ///
    /// Size of the buffer handed to grading.
    pub src: [usize; 2],
}

/// 分步计时的开关。
///
/// The switch for per-phase timing.
static PREVIEW_TRACE_ON: AtomicBool = AtomicBool::new(false);
/// 最近一次分步计时。
///
/// The latest per-phase timings.
static PREVIEW_PHASES: Mutex<Option<PreviewPhases>> = Mutex::new(None);

/// 打开分步计时并清掉上一份结果。
///
/// Turn per-phase timing on and drop the previous record.
pub(crate) fn trace_start_for_bench() {
    *PREVIEW_PHASES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    PREVIEW_TRACE_ON.store(true, Ordering::Relaxed);
}

/// 关掉分步计时并取回最近一份结果。
///
/// Turn per-phase timing off and take the latest record.
pub(crate) fn trace_take_for_bench() -> Option<PreviewPhases> {
    PREVIEW_TRACE_ON.store(false, Ordering::Relaxed);
    PREVIEW_PHASES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// 基准与判据用的探针：走 `render_preview_file` 原路，把结果摊开给外面量。
///
/// 只给 `preview_bench`（`--bench-preview` 与首屏预算判据）用，不参与界面逻辑。
///
/// Probe for the benchmark and its fence: it runs the same `render_preview_file` path and
/// hands the result out for measuring. Used only by `preview_bench`; the UI never touches it.
pub(crate) struct PreviewProbe {
    /// 调色后的贴图像素（与界面拿到的同一份）。
    ///
    /// The graded texture pixels, the same ones the UI receives.
    pub graded: ColorImage,
    /// 报给画布的逻辑尺寸。
    ///
    /// Logical size reported to the canvas.
    pub size: [u32; 2],
    /// 解码与调色各自的耗时，外加送进调色的缓冲尺寸。
    ///
    /// Per-phase timings of decode and grade, plus the size of the buffer handed to grading.
    pub phases: PreviewPhases,
    /// 草图这次解码走的是哪条路：`Some(true)` = 半尺寸快路，`Some(false)` = 整幅解码再
    /// 减半，`None` = 这次要的是全量预览。
    ///
    /// 首屏判据断言的就是它：这是**代码走没走快路**的事实，不受机器快慢影响。
    ///
    /// Which route produced the sketch: `Some(true)` for the half-size fast route,
    /// `Some(false)` for the full decode plus halving, `None` when this render asked for the
    /// full preview. The first-screen fence asserts this flag -- a fact about the code path,
    /// independent of machine speed.
    pub sketch_half: Option<bool>,
}

/// 清掉预览源与原图缓存，模拟「这张图第一次被打开」。
///
/// 首屏预算量的是冷路径：缓存命中之后草图只需几毫秒，拿那个数字当首屏等于不测。
///
/// Clear the preview source and original caches, i.e. "this frame is opened for the first
/// time". The first-screen budget is about the cold path: with a cache hit the sketch costs a
/// few milliseconds, and reporting that as the first screen would measure nothing.
pub(crate) fn clear_preview_caches_for_bench() {
    *PREVIEW_SRC
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = PreviewSlots::default();
    *PREVIEW_ORIG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// 走一遍 `render_preview_file`（探针版）。
///
/// Run one pass of `render_preview_file` (probe flavour).
pub(crate) fn render_preview_for_bench(
    path: &std::path::Path,
    sketch: bool,
) -> Result<PreviewProbe, String> {
    trace_start_for_bench();
    let (graded, _original, size, hist) = render_preview_file(
        path,
        &GradeParams::default(),
        Ev::NONE,
        None,
        None,
        Framing::default(),
        None,
        sketch,
        // 基准量的是**冷首屏**：草图按半尺寸解，这才是要卡的延迟口径。
        //
        // The benchmark measures the **cold first screen**: the sketch decodes at half size,
        // which is the latency this is meant to bound.
        sketch,
        false,
        false,
    )?;
    let phases = trace_take_for_bench().unwrap_or_default();
    // 这次解码走的是哪条路，由 `decoded_preview_source` 在解码完成时记在本线程上。
    //
    // Which route this decode took, recorded on this thread by `decoded_preview_source` at the
    // moment the decode finished.
    let sketch_half = LAST_SKETCH_ROUTE.with(std::cell::Cell::get);
    // 直方图不进探针：判据比对的是像素，直方图每个回包都带着，拷进探针只是多一份。
    //
    // The histogram stays out of the probe: the fence compares pixels, and every reply already
    // carries a histogram, so copying it here would just be another copy.
    let _ = hist;
    Ok(PreviewProbe {
        graded,
        size,
        phases,
        sketch_half,
    })
}

struct PreviewSource {
    path: PathBuf,
    stamp: SourceStamp,
    /// 解码时按哪一个档位（[`PREVIEW_SIDE`] 或 [`SKETCH_SIDE`]）解出来的。
    side: u32,
    /// 解码时套的相机配置路径；换配置必须重新解码。
    profile: String,
    /// 解码时的去马赛克方法；诊断面板切换后必须重新解码。
    demosaic: &'static str,
    w: usize,
    h: usize,
    /// 归一化到 `[0,1]` 的 sRGB 像素；调用方拿走一份副本再调色。
    rgb: Vec<f32>,
    /// **全量预览**的边长（草图槽也记，靠它报画布上的逻辑尺寸）。
    ///
    /// Side lengths of the **full** preview; the sketch slot records them too so the canvas
    /// gets the same logical size in both cases.
    full: (usize, usize),
    /// 草图槽专用：这份像素是半尺寸快路解的（`true`）还是整幅解码再减半（`false`）。
    /// 全量槽不读这个字段。
    ///
    /// Sketch slot only: whether these pixels came from the half-size fast route (`true`) or
    /// from a full decode plus halving (`false`). The full slot never reads it.
    half: bool,
}

/// 两档缓存：全量与草图各一槽。加起来仍然封顶两张预览图。
///
/// Two slots, full and sketch. Together they still cap out at two preview images.
#[derive(Default)]
struct PreviewSlots {
    full: Option<PreviewSource>,
    sketch: Option<PreviewSource>,
}

/// 预览源图缓存。渲染都排在在途槽上，同一时刻只有一张图在算，各档一槽就够。
///
/// The preview source cache: each tier gets one slot, and only one render is in flight at a
/// time, so that is enough.
static PREVIEW_SRC: Mutex<PreviewSlots> = Mutex::new(PreviewSlots {
    full: None,
    sketch: None,
});

thread_local! {
    /// 本线程最近一次预览解码走的路：`Some(true)` = 草图半尺寸快路，`Some(false)` = 草图
    /// 整幅解码再减半，`None` = 那一次要的是全量预览（或还没解过）。
    ///
    /// 只给基准探针读。用线程本地是因为判据的测试并行跑、共用 `PREVIEW_SRC`：事后读全局
    /// 槽拿到的是「此刻缓存里有什么」，而不是「这次渲染走了哪条路」，两者在多线程下不等。
    ///
    /// The route this thread's latest preview decode took: `Some(true)` for the sketch's
    /// half-size fast path, `Some(false)` for the sketch's full decode plus halving, `None`
    /// when that render asked for the full preview (or nothing ran yet). Read by the bench
    /// probe only. Thread-local because the fence's tests run in parallel over one shared
    /// `PREVIEW_SRC`: a slot read-back answers "what is cached now", not "which route this
    /// render took", and those differ across threads.
    static LAST_SKETCH_ROUTE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

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

/// 单帧导出的渲染那一刀：动态范围决定要不要让白以上活下来。
///
/// 这是和批量导出同一个开关（`export::render_frame_raw`）。少了它，单帧 HDR 导出
/// 会在写盘前就被钳回 [0,1]，PQ 编码器再努力也只能接到一片白——用户看到的
/// 仍是 SDR 成片，只是文件名写着 HDR。
///
/// The render pass for a single still export: the dynamic range decides whether brightness
/// above white survives. This is the same switch the batch path uses; without it a single
/// HDR still is clamped to [0,1] before the encoder, so PQ only ever sees flat white.
fn grade_still_buffer(
    buf: &mut [f32],
    w: usize,
    h: usize,
    params: &GradeParams,
    gain: Ev,
    dynamic: OutputDynamic,
) {
    if dynamic == OutputDynamic::HdrPq {
        aevum_core::render_preview_hdr(buf, w, h, params, gain);
    } else {
        aevum_core::render_preview(buf, w, h, params, gain);
    }
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
        metrics::decode_for_grade(src, params).map_err(|e| crate::i18n::err_text(&e))?;
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

    grade_still_buffer(&mut buf, w, h, params, gain, render.dynamic);
    let (mut out_buf, ow, oh) = shift_crop(&buf, w, h, framing, crop);
    // 尾部动作交给 core：色域搬运 + ICC + 输出锐化 + EXIF + 容器编码，
    // 和批量导出共用同一条路，两条不许各调一刀。`deep` 是单帧有意保留的差别：
    // 高位深源选 PNG 仍写 16bit，别让最后一步把精度砍回 256 级。
    aevum_core::export::write_single_still(src, dst, &mut out_buf, ow, oh, deep, &render)
        .map_err(|e| crate::i18n::err_text(&e))?;
    // 旁车是静帧旁边的另一个文件，不在容器编码那条尾巴里：单帧这一路自己调一次。
    aevum_core::export::write_sidecar(dst, params, &render)
        .map_err(|e| crate::i18n::err_text(&e))
        .map(|_| ())
}

/// 单帧导出的默认落点：`out_dir` 为空时落到 `<序列根>/成片`。
///
/// 抽成纯函数是为了能被判据直接量：路径规则一旦退回去（比如又带上保存对话框的
/// 返回值、或者丢掉了 `_graded` 后缀），`tests/suite/export_path.rs` 会当场红。
/// 这也是批量导出一直在用的口径（`run_export` 的 `out_root`），两条路必须一致，
/// 否则"批量导到 A、单帧导到 B"这种事会悄悄回来。
///
/// Where a single-frame export lands by default: `<sequence root>/成片` when `out_dir` is
/// empty. A pure function so the fence can measure it directly -- if the rule regresses (a
/// dialog return value sneaks back in, or the `_graded` suffix disappears) the test goes red.
/// It is the same rule batch export already uses (`out_root` in `run_export`); the two must
/// agree, or "batch goes to A, single frame goes to B" would quietly return.
pub fn auto_frame_dst(
    out_dir: &std::path::Path,
    seq_root: &std::path::Path,
    stem: &str,
    format: StillFormat,
) -> std::path::PathBuf {
    let dir = if out_dir.as_os_str().is_empty() {
        seq_root.join("成片")
    } else {
        out_dir.to_path_buf()
    };
    force_container(dir.join(format!("{stem}_graded.{}", format.ext())), format)
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
        "3:2" => Aspect::ThreeTwo,
        "4:3" | "standard" | "2" => Aspect::FourThree,
        "5:4" => Aspect::FiveFour,
        "1:1" | "square" | "3" => Aspect::Square,
        "4:5" => Aspect::FourFive,
        "9:16" | "vertical" | "4" => Aspect::Vertical,
        "2:3" => Aspect::TwoThree,
        other => {
            let known = Aspect::ALL
                .iter()
                .map(|a| a.label())
                .collect::<Vec<_>>()
                .join(" / ");
            return Err(crate::i18n::tr_fmt(
                "画幅比例不认：{}（可用：{}）",
                &[&other, &known],
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

/// EXIF 的 `2026:11:02 18:24:07` 换成人读得顺的 `2026-11-02 18:24:07`：
/// 日期段的冒号换短横，时间段原样。长度不够或不像日期就原样返回。
///
/// Turn EXIF's `2026:11:02 18:24:07` into the readable `2026-11-02 18:24:07`: the date colons
/// become dashes and the time part stays. Anything short or unlike a date comes back as is.
fn pretty_datetime(dt: &str) -> String {
    match dt.char_indices().nth(10) {
        Some((cut, _)) if dt.is_char_boundary(cut) && dt[..cut].contains(':') => {
            format!("{}{}", dt[..cut].replace(':', "-"), &dt[cut..])
        }
        _ => dt.to_owned(),
    }
}

/// 叠印文字：缺 EXIF 时只出帧号，有就一路补上时间、机型与曝光。
///
/// The overlay line: just the frame number without EXIF, extended with the capture time, camera
/// and exposure when they are there.
fn stamp_line(index: usize, total: usize, exif: Option<&Exif>) -> String {
    let mut parts = vec![format!("#{:04}/{}", index + 1, total)];
    if let Some(e) = exif {
        if let Some(dt) = e.datetime() {
            parts.push(pretty_datetime(dt));
        }
        if let Some(line) = e.camera_line() {
            parts.push(line);
        }
        if let Some(shutter) = e.shutter_text() {
            match e.fnum() {
                Some(f) => parts.push(format!("{shutter} · f/{f:.1}")),
                None => parts.push(shutter),
            }
        }
        if let Some(iso) = e.iso() {
            parts.push(format!("ISO {}", iso.round() as i64));
        }
    }
    parts.join("  ·  ")
}

/// 帧号区间的锚点：包围（当前帧前后各取几张）还是起点（从当前帧往后取几张）。
///
/// Where a frame range is anchored: centred on the current frame, or starting at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndicesAnchor {
    Center,
    Start,
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
        return Err(crate::i18n::tr_fmt(
            "component 超出范围：0..{}",
            &[&count.saturating_sub(1)],
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
                    return Err(crate::i18n::tr_fmt(
                        "未知蒙版形状 {}；可用 linear / radial / brush",
                        &[&kind],
                    ));
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
                    return Err(crate::i18n::tr_fmt(
                        "未知叠加方式 {}；可用 add / subtract / intersect",
                        &[&blend],
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
                    return Err(crate::i18n::tr_fmt(
                        "strokes 最多 {} 条笔画",
                        &[&mask::MAX_STROKES],
                    ));
                }
                let mut out = Vec::with_capacity(list.len());
                for (i, s) in list.iter().enumerate() {
                    let obj = s.as_object().ok_or_else(|| {
                        crate::i18n::tr_fmt("第 {} 条笔画必须是对象", &[&(i + 1)])
                    })?;
                    for key in obj.keys() {
                        if !matches!(
                            key.as_str(),
                            "points" | "radius" | "hardness" | "flow" | "erase"
                        ) {
                            return Err(crate::i18n::tr_fmt("未知笔画字段 {}", &[&key]));
                        }
                    }
                    let points = obj
                        .get("points")
                        .ok_or_else(|| crate::i18n::tr_fmt("第 {} 条笔画缺 points", &[&(i + 1)]))?
                        .as_array()
                        .ok_or_else(|| {
                            crate::i18n::tr_fmt("第 {} 条笔画的 points 必须是数组", &[&(i + 1)])
                        })?;
                    if points.len() > mask::MAX_STROKE_POINTS {
                        return Err(crate::i18n::tr_fmt(
                            "一条笔画最多 {} 个点",
                            &[&mask::MAX_STROKE_POINTS],
                        ));
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
                        return Err(crate::i18n::tr_fmt("第 {} 条笔画至少要一个点", &[&(i + 1)]));
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
            other => return Err(crate::i18n::tr_fmt("未知几何字段 {}", &[&other])),
        }
    }
    Ok(())
}

fn geometry_number(key: &str, value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| crate::i18n::tr_fmt("geometry.{} 必须是有限数值", &[&key]))
}

/// 组级亮度范围的存盘形状：0~1 的区间 + 开关。
///
/// The persisted shape of a group-level luminance range: an interval in 0..1 plus a toggle.
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
                .ok_or_else(|| crate::i18n::tr_fmt("range.{} 必须是有限数值", &[&key]))
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
                return Err(crate::i18n::tr_fmt(
                    "未知范围字段 {}（亮度用 lum_*，颜色用 hue_*）",
                    &[&other],
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
        .ok_or_else(|| crate::i18n::tr_fmt("缺少字符串参数 {}", &[&key]))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| crate::i18n::tr_fmt("{} 必须是字符串", &[&key])),
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
            n.map(Some)
                .ok_or_else(|| crate::i18n::tr_fmt("{} 必须是非负整数", &[&key]))
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
            .ok_or_else(|| crate::i18n::tr_fmt("{} 必须是有限数值", &[&key])),
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
                return Err(crate::i18n::tr_fmt("速度键要写成对象，实得 {}", &[&item]));
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
                    .ok_or_else(|| crate::i18n::tr_fmt("{} 里的元素必须是字符串", &[&key]))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(crate::i18n::tr_fmt("{} 必须是字符串或字符串数组", &[&key])),
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
            other => {
                return Err(crate::i18n::tr_fmt(
                    "{} 必须是字符串，收到 {}",
                    &[&key, &other],
                ));
            }
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

/// 词法比较两个路径是否指向同一处（不碰文件系统）。
///
/// 只做词法——穿不过符号链接 / junction，所以**不能**用来判「输出目录安不安全」；
/// 那条判定在 `aevum_core::paths`（会比较规范化后的真实路径）。这里留着它是因为
/// 界面别处只关心「这两个路径字符串是不是同一个文件」（源帧比对、工程落点比对）。
///
/// Whether two paths point at the same place, compared lexically (touches no filesystem).
///
/// Lexical only -- it cannot see through a symlink or junction, so it must **not** be used
/// to decide whether an output folder is safe; that judgement lives in
/// `aevum_core::paths`, which compares resolved real paths. It stays here because other UI
/// sites only care whether two path strings name the same file (source-frame checks, the
/// project landing spot).
fn path_eq(a: &Path, b: &Path) -> bool {
    let (a, b) = (lexical(a), lexical(b));
    let a: Vec<_> = a.components().collect();
    let b: Vec<_> = b.components().collect();
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| component_eq(*x, *y))
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

/// 导出目录安全检查：转调 `aevum_core::paths`（一份实现，headless 与界面共用），
/// 只在这里多加一层 i18n。
///
/// 判定本身（含符号链接 / junction 的规范化比较）不在这里——两份拷贝已经删了；
/// 这里剩下的唯一职责是把 core 给出的中文错误串过一遍当前语言的表。
///
/// Export-folder safety check: delegates to `aevum_core::paths` (one implementation shared
/// with the headless side) and only adds the i18n layer here.
///
/// The judgement itself (including the resolved-path comparison across symlinks and
/// junctions) is not here -- the second copy is gone. All that remains is running core's
/// Chinese error string through the current language's table.
fn ensure_out_dir_safe(root: &Path, out: &Path) -> Result<(), String> {
    aevum_core::paths::ensure_out_dir_safe(root, out)
        .map_err(|msg| crate::i18n::tr(&msg).to_owned())
}

/// 后台合并任务：逐帧读盘（报进度、可取消），再走各自的核心算法写一张静帧。
///
/// HDR 与全景共用这条读帧循环：全景的 `framing` 是全幅零位移，走的是同一段
/// 恒等分支，真正分岔只在最后一步（曝光融合 vs 特征点拼接）。取消只在两帧之间
/// 检查一次——核心算法是一整块，中途打断没有意义。
/// 一次焦点自动对齐的估计结果，供接线层出图与结果 JSON 使用。
///
/// One focus auto-alignment estimate, consumed by the wiring layer for the image and the
/// result JSON.
struct FocusAlignPass {
    /// 三态本体。结果 JSON 的字符串由 [`AlignStatus::key`] 给出，不在这里手写。
    ///
    /// The three states themselves. The result JSON's string comes from
    /// [`AlignStatus::key`]; no literal is hand-written here.
    status: AlignStatus,
    /// 相对增益（0.012 = +1.2%）。
    ///
    /// Relative gain (0.012 means +1.2%).
    gain: f32,
    /// 逐帧 8×6 累计位移场（全分辨率 px），采用时逐块 warp 用。
    ///
    /// Per-frame accumulated 8x6 shift field in full-resolution px, used by the per-block
    /// warp when adopted.
    blocks: Vec<Vec<[f64; 2]>>,
    /// 自动裁切（采用且没有手动裁切框时给出）。
    ///
    /// Automatic crop (supplied when adopted and there is no manual crop rectangle).
    crop: CropRect,
}

/// 焦点合成的前置估计：调 core 的同一条编排，把取消旗翻成它的 `cancelled` 回调。
///
/// 编排本身在 [`aevum_core::focus_align::estimate`]——无头 MCP 走的是同一份，UI 只负责
/// 「怎么取消」和「三态里 adopted 才带块场」。重复写两遍编排必然漂移，所以这里只剩翻译。
///
/// The estimate in front of a focus merge: call core's single orchestration, translating the
/// cancel flag into its `cancelled` callback.
///
/// The orchestration itself lives in [`aevum_core::focus_align::estimate`] -- the headless
/// MCP uses the same one. The UI only decides "how to cancel" and "only adopted carries the
/// block field". Two copies of the orchestration would drift, so this is just translation.
fn focus_align_pass(
    frames: &[(Vec<f32>, usize, usize)],
    w: usize,
    h: usize,
    radius: usize,
    manual_crop: Option<CropRect>,
    cancel: &AtomicBool,
) -> Result<FocusAlignPass, String> {
    let refs: Vec<&[f32]> = frames.iter().map(|(b, _, _)| b.as_slice()).collect();
    let report = focus_align::estimate(&refs, w, h, radius, manual_crop, &|| {
        cancel.load(Ordering::SeqCst)
    })
    .ok_or_else(|| "已取消：自动对齐估到一半".to_owned())?;
    let blocks = if report.status == AlignStatus::Adopted {
        report.blocks.clone()
    } else {
        Vec::new()
    };
    Ok(FocusAlignPass {
        status: report.status,
        gain: report.gain,
        blocks,
        crop: report.crop.unwrap_or(CropRect {
            x: 0,
            y: 0,
            w: w as u32,
            h: h as u32,
        }),
    })
}

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
        let (rgb, w, h) =
            export::load_rgb_f32(src).map_err(|e| crate::i18n::tr_fmt("读帧失败：{}", &[&e]))?;
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
        if job.kind.needs_uniform_size() {
            match dims {
                None => dims = Some(got),
                Some(prev) if prev != got => {
                    // 帧号取 `job.indices[k]`，不是循环下标 `k`：序列 [10,11,12] 出错时
                    // 报「第 10 帧与第 2 帧」是错的（第 2 帧根本不在这批里）。
                    //
                    // The frame number is `job.indices[k]`, not the loop index `k`: on the
                    // sequence [10, 11, 12] reporting "frame 10 vs frame 2" is wrong
                    // (frame 2 is not in this batch at all).
                    return Err(crate::i18n::tr_fmt(
                        "第 {} 帧与第 {} 帧对齐后尺寸不一致（{}×{} vs {}×{}）：先跑对齐或裁成同尺寸",
                        &[
                            &job.indices[0],
                            &job.indices[k],
                            &prev.0,
                            &prev.1,
                            &got.0,
                            &got.1,
                        ],
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

    // 自动对齐的三态结果：`align:false`（以及其余合并种类）恒为 `off` / 0 / 空。
    //
    // The three-state outcome of auto alignment: `off` / 0 / empty when `align: false`
    // (and for every other merge kind).
    let mut align_status = "off";
    let mut align_gain = 0.0f32;

    let (mut pixels, ow, oh) = match job.kind {
        MergeKind::Hdr => {
            let (w, h) = dims.unwrap_or((0, 0));
            let refs: Vec<&[f32]> = frames.iter().map(|(b, _, _)| b.as_slice()).collect();
            (aevum_core::hdr::merge(&refs, w, h), w, h)
        }
        // 堆栈与时间合成都是同机位叠帧：尺寸在上面的循环里已经查齐，这里直接按首帧算。
        MergeKind::Stack => {
            let (w, h) = dims.unwrap_or((0, 0));
            let refs: Vec<&[f32]> = frames.iter().map(|(b, _, _)| b.as_slice()).collect();
            let opts = match &job.spec {
                MergeSpec::Stack(o) => o.clone(),
                _ => aevum_core::stack::Options::default(),
            };
            let out = aevum_core::stack::combine(&refs, w, h, &opts)
                .map_err(|e| crate::i18n::tr_fmt("堆栈失败：{}", &[&crate::i18n::tr(e.cn())]))?;
            (out, w, h)
        }
        // frames[0] 是最早的一帧：帧号升序就是时间顺序，天地合成靠它分早晚。
        MergeKind::TimeBlend => {
            let (w, h) = dims.unwrap_or((0, 0));
            let refs: Vec<&[f32]> = frames.iter().map(|(b, _, _)| b.as_slice()).collect();
            let spec = match &job.spec {
                MergeSpec::TimeBlend(s) => s.clone(),
                _ => aevum_core::timeblend::Spec::default(),
            };
            let out = aevum_core::timeblend::time_slice(&refs, w, h, &spec).map_err(|e| {
                crate::i18n::tr_fmt("时间合成失败：{}", &[&crate::i18n::tr(e.cn())])
            })?;
            (out, w, h)
        }
        // 焦点合成：同机位叠帧，尺寸在上面已经查齐；测度只看纹理，帧序不带方向含义。
        //
        // Focus stacking: overlaid frames from a fixed camera, sizes already checked above;
        // the measure only reads texture, so frame order carries no direction.
        MergeKind::Focus => {
            let (w, h) = dims.unwrap_or((0, 0));
            let opts = match &job.spec {
                MergeSpec::Focus(o) => o.clone(),
                _ => aevum_core::focus::Options::default(),
            };
            // 合成前自动对齐（默认关闭）：任一帧都没读进来之前不碰任何代理，所以
            // `align: false` 这条路的成片与 1.4.0 逐点一致（X-C1）。
            //
            // Pre-fusion auto alignment (off by default): no proxy is built unless asked,
            // so the `align: false` path stays point-for-point identical to 1.4.0 (X-C1).
            let mut work = frames;
            let (mut ow, mut oh) = (w, h);
            if job.align {
                if cancel.load(Ordering::SeqCst) {
                    return Err("已取消：还没开始估计自动对齐".to_owned());
                }
                let manual_crop = job.framing.first().and_then(|(c, _)| *c);
                let outcome = focus_align_pass(&work, w, h, opts.radius, manual_crop, cancel)?;
                align_status = outcome.status.key();
                align_gain = outcome.gain;
                // 估计失败与收益不足都**照样出图**，用未对齐几何：不许伪装成功，
                // 也不许让整个任务失败（三态处置见设计 §1.2）。
                //
                // Both a failed estimate and insufficient gain still produce an image, on
                // the unaligned geometry: never pretend success, and never fail the job
                // (the three-state handling is in design section 1.2).
                if outcome.status == AlignStatus::Adopted {
                    let window = if manual_crop.is_some() {
                        // 读帧循环已经按手动裁切框裁过，输出窗口就是整幅。
                        //
                        // The read loop already cropped to the manual rectangle, so the
                        // output window is the whole frame.
                        None
                    } else {
                        let c = outcome.crop;
                        Some((c.x as usize, c.y as usize, c.w as usize, c.h as usize))
                    };
                    // 逐块 warp + 24px 接缝淡化（拍板 ⑦）：breathing 是径向放大率变化，
                    // 单一全局平移表达不了，必须让每块各走各的位移。
                    //
                    // Per-block warp with 24 px seam feathering (ruling 7): breathing is a
                    // radial magnification change that one global translation cannot
                    // express, so every block must follow its own shift.
                    let mut warped: Vec<(Vec<f32>, usize, usize)> = Vec::with_capacity(work.len());
                    for (i, (buf, _, _)) in work.iter().enumerate() {
                        if cancel.load(Ordering::SeqCst) {
                            return Err("已取消：自动对齐已完成，还没开始融合".to_owned());
                        }
                        let field = outcome.blocks.get(i).map(Vec::as_slice).unwrap_or(&[]);
                        let (out, nw, nh) =
                            focus_align::warp_by_block_field(buf, w, h, field, window);
                        warped.push((out, nw, nh));
                    }
                    if let Some((nw, nh)) = warped.first().map(|(_, nw, nh)| (*nw, *nh)) {
                        if nw == 0 || nh == 0 {
                            return Err("自动对齐的裁切窗口是空的：检查对齐位移是否异常".to_owned());
                        }
                        ow = nw;
                        oh = nh;
                    }
                    work = warped;
                }
            }
            let refs: Vec<&[f32]> = work.iter().map(|(b, _, _)| b.as_slice()).collect();
            // 焦点合成有自己的错误语境：`StackError` 的 `cn()` 说的是「堆栈」，
            // 对焦包围的用户看到那句会莫名其妙（见 `focus::error_text`）。
            //
            // Focus stacking has its own error context: `StackError`'s `cn()` talks about a
            // "stack", which reads wrong for a focus bracket (see `focus::error_text`).
            let out = aevum_core::focus::focus_stack(&refs, ow, oh, &opts).map_err(|e| {
                crate::i18n::tr_fmt(
                    "焦点合成失败：{}",
                    &[&crate::i18n::tr(aevum_core::focus::error_text(e))],
                )
            })?;
            (out, ow, oh)
        }
        // 全景按每张自己的尺寸求单应，混尺寸也能接。
        MergeKind::Pano => {
            let refs: Vec<(&[f32], u32, u32)> = frames
                .iter()
                .map(|(b, w, h)| (b.as_slice(), *w as u32, *h as u32))
                .collect();
            let (pano, cw, ch) = aevum_core::pano::stitch(&refs)
                .map_err(|e| crate::i18n::tr_fmt("拼接失败：{}", &[&e]))?;
            (pano, cw as usize, ch as usize)
        }
    };
    if pixels.is_empty() {
        return Err(crate::i18n::tr_fmt(
            "{}算出来是空图：检查帧是否可读",
            &[&job.kind.noun()],
        ));
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
    .map_err(|e| crate::i18n::tr_fmt("写{}成片失败：{}", &[&job.kind.noun(), &e]))?;
    done.store(total.load(Ordering::Relaxed), Ordering::Relaxed);
    Ok(json!({
        "path": job.dst.display().to_string(),
        "kind": job.kind.tag(),
        "frames": job.indices.len(),
        "indices": job.indices,
        "width": ow,
        "height": oh,
        // 自动对齐的三态与增益。`align:false` 时为 "off" / 0：调用方看状态就知道这次
        // 到底跑没跑，不用去猜「gain 是 0 还是没算」。
        //
        // **不外发中文 reason**：MCP 与脚本没有 i18n 漏斗，中文模板键原样出去英文用户
        // 必见中文。状态键已经够判读，给人看的整句由各端自己按状态选（面板走 i18n 表，
        // MCP 只报机器键）。
        //
        // The three states and the gain of auto alignment. With `align: false` these are
        // "off" / 0, so a caller can tell "not run" from "ran and gained nothing" without
        // guessing.
        //
        // The Chinese reason is **never** emitted: MCP and scripts have no i18n funnel, and
        // shipping a Chinese template key would show Chinese to English users. The status
        // key is enough to branch on; the human sentence is chosen per front end (the panel
        // goes through the i18n table, MCP only sees the machine keys).
        "align_status": align_status,
        "align_gain": align_gain,
    }))
}

/// 时间合成预览的半分辨率解码缓存：帧选择没变就整包复用，参数变了只重算合成。
///
/// Half-resolution decode cache for the time-blend preview: reused wholesale while the
/// frame selection stays the same, so parameter tweaks only redo the composite.
pub struct TbPreviewData {
    /// 帧选择指纹：序列根路径 + 选中帧号，变了缓存作废。
    ///
    /// Frame-selection fingerprint: sequence root plus chosen indices; a change voids it.
    pub key_frames: u64,
    pub indices: Vec<usize>,
    pub frames: Vec<(Vec<f32>, usize, usize)>,
}

/// 2×2 盒滤波降半分辨率：预览不必逐像素精确，省一半内存与解码后处理。
///
/// 2x2 box-filter downscale by half: previews need not be pixel-exact, and this halves
/// both memory and post-decode work.
fn downscale2(rgb: Vec<f32>, w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let (w2, h2) = (w / 2, h / 2);
    if w2 == 0 || h2 == 0 {
        return (rgb, w, h);
    }
    let mut out = vec![0.0f32; w2 * h2 * 3];
    for y in 0..h2 {
        for x in 0..w2 {
            for c in 0..3 {
                let a = rgb[((y * 2) * w + x * 2) * 3 + c];
                let b = rgb[((y * 2) * w + x * 2 + 1) * 3 + c];
                let d = rgb[((y * 2 + 1) * w + x * 2) * 3 + c];
                let e = rgb[((y * 2 + 1) * w + x * 2 + 1) * 3 + c];
                out[(y * w2 + x) * 3 + c] = (a + b + d + e) * 0.25;
            }
        }
    }
    (out, w2, h2)
}

/// 深度图预览的降采样上限：长边压到这条线以内再进 `focus::depth_map`。
///
/// 全分辨率（24 MP × 32 帧）的归属图前台同步算会卡 UI；预览不需要逐像素精确，
/// 长边压到 1024 之后量级掉三个数量级，实测见 `focus_depth_tests` 的注释。
///
/// The downsample cap for the depth-map preview: the long edge is squeezed under this
/// line before entering `focus::depth_map`.
///
/// A full-resolution ownership map (24 MP x 32 frames) computed synchronously on the
/// foreground would stall the UI; a preview does not need pixel-exactness, and the
/// 1024 long edge cuts the work by three orders of magnitude -- measured numbers live
/// in the `focus_depth_tests` comments.
pub(crate) const FOCUS_PREVIEW_LONG_EDGE: usize = 1024;

/// 盒滤波降采样：长边超过 `max_edge` 就按整数步长分块平均，否则原样返回。
///
/// 余数行列直接丢掉（预览不需要逐像素精确）；空图、零尺寸或上限为零都原样
/// 返回，不 panic——这些退化输入在判据里逐一钉死。实测（release，6008×4016）：
/// 一步 step² 块平均约 34 ms/帧，8 帧 24 MP 全程（含 depth_map）约 0.33 s，
/// 在「按一下等半秒」的体感线内，不再做更花哨的分段优化。
///
/// Box-filter downscale: when the long edge exceeds `max_edge`, average in integer
/// step blocks; otherwise return untouched.
///
/// Remainder rows/columns are dropped (a preview does not need pixel-exactness);
/// empty input, zero sizes or a zero cap all return untouched without panicking --
/// each degenerate input is pinned down in the tests. Measured (release, 6008x4016):
/// the one-shot step-squared block average costs about 34 ms per frame, so a full
/// 8-frame 24 MP preview (depth_map included) lands near 0.33 s -- inside the
/// "press and wait half a second" feel, so no fancier staged scheme is warranted.
pub(crate) fn downscale_long_edge(
    rgb: Vec<f32>,
    w: usize,
    h: usize,
    max_edge: usize,
) -> (Vec<f32>, usize, usize) {
    if w == 0 || h == 0 || max_edge == 0 {
        return (rgb, w, h);
    }
    let long = w.max(h);
    if long <= max_edge {
        return (rgb, w, h);
    }
    let step = long.div_ceil(max_edge);
    let (w2, h2) = (w / step, h / step);
    if w2 == 0 || h2 == 0 {
        return (rgb, w, h);
    }
    let mut out = vec![0.0f32; w2 * h2 * 3];
    for y in 0..h2 {
        for x in 0..w2 {
            for c in 0..3 {
                let mut acc = 0.0f32;
                for dy in 0..step {
                    for dx in 0..step {
                        acc += rgb[((y * step + dy) * w + x * step + dx) * 3 + c];
                    }
                }
                out[(y * w2 + x) * 3 + c] = acc / (step * step) as f32;
            }
        }
    }
    (out, w2, h2)
}

/// 帧号 → 预览色：色相环上按 `count` 均匀取样，参考帧给中灰。
///
/// 相邻帧号的色相至少差 360/count 度，同帧号永远同色——「每一帧一眼认出来」
/// 是预览的全部意义；参考帧（兜底主力）用灰色单列，避免它伪装成「第 k 帧区」。
///
/// Frame number → preview colour: sampled evenly around the hue wheel by `count`,
/// with the reference frame set apart in mid-grey.
///
/// Adjacent frame numbers differ by at least 360/count degrees of hue and the same
/// frame number always yields the same colour -- "recognise each frame at a glance"
/// is the whole point of the preview. The reference frame (the fallback workhorse)
/// gets grey so it cannot masquerade as "the frame-k region".
pub(crate) fn depth_preview_color(owner: u16, count: usize, reference: u16) -> [u8; 3] {
    if count == 0 || owner == reference {
        return [128, 128, 128];
    }
    let hue = (owner as f32 / count as f32) * 360.0;
    hsl_to_rgb(hue, 0.65, 0.55)
}

/// HSL → RGB（h 0..360，s / l 0..1）：帧号上色的唯一出口，测试直接钉在这里。
///
/// HSL → RGB (h 0..360, s/l 0..1): the single exit for colouring frame numbers,
/// pinned down by tests directly.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    [f(r1), f(g1), f(b1)]
}

/// FNV-1a 64 位散列：给预览参数做指纹，快而且够散。
///
/// FNV-1a 64-bit hash: fingerprints the preview parameters; fast and well spread.
fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// 归一化 RGB 转 ColorImage：读帧管线不做传输变换，成片写盘也只是缩放回 0..=255，
/// 所以预览同样恒等缩放——预览与成片的像素才对得上。
///
/// Normalized RGB to a ColorImage: the decode pipeline applies no transfer change and the
/// still writer just scales back to 0..=255, so the preview scales identically and its
/// pixels line up with the final still.
fn tb_preview_image(lin: &[f32], w: usize, h: usize) -> ColorImage {
    let mut px = Vec::with_capacity(w * h);
    for c in lin.chunks_exact(3) {
        let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        px.push(egui::Color32::from_rgb(to8(c[0]), to8(c[1]), to8(c[2])));
    }
    ColorImage::new([w, h], px)
}

/// 后台预览任务：缓存失效就半分辨率解码一遍，再用同一套核心算法合成。
///
/// 与 `run_merge_job` 的读帧循环同构（对齐位移与裁切都套上），只是多了一步降半，
/// 这样预览的构图与最终成片一致。
///
/// The background preview job: decodes at half resolution when the cache is stale, then
/// composites with the same core algorithm. The decode loop mirrors `run_merge_job`
/// (alignment offsets and crops applied), only with an extra downscale, so the preview
/// framing matches the final still.
fn run_timeblend_preview(
    cached: Option<Arc<TbPreviewData>>,
    srcs: Vec<PathBuf>,
    indices: Vec<usize>,
    framing: Vec<(Option<CropRect>, [f64; 2])>,
    spec: aevum_core::timeblend::Spec,
    key_frames: u64,
) -> Result<(Arc<TbPreviewData>, Vec<f32>, usize, usize), String> {
    let data = match cached {
        Some(d) if d.key_frames == key_frames && d.indices == indices => d,
        _ => {
            let mut frames: Vec<(Vec<f32>, usize, usize)> = Vec::with_capacity(srcs.len());
            for (src, (crop, off)) in srcs.iter().zip(framing.iter()) {
                let (rgb, w, h) = export::load_rgb_f32(src)
                    .map_err(|e| crate::i18n::tr_fmt("读帧失败：{}", &[&e]))?;
                let rect = crop.map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize));
                let fr = Framing {
                    angle_deg: 0.0,
                    off: *off,
                };
                let (buf, got) = if rect.is_none() && fr.is_identity() {
                    (rgb, (w as usize, h as usize))
                } else {
                    let mut out = Vec::new();
                    let (cw, ch) =
                        warp::transform_into(&rgb, w as usize, h as usize, fr, rect, &mut out);
                    (out, (cw, ch))
                };
                frames.push(downscale2(buf, got.0, got.1));
            }
            Arc::new(TbPreviewData {
                key_frames,
                indices,
                frames,
            })
        }
    };
    let (w, h) = data
        .frames
        .first()
        .map(|(_, w, h)| (*w, *h))
        .unwrap_or((0, 0));
    if w == 0 || h == 0 {
        return Err("预览没有可读的帧".to_owned());
    }
    let refs: Vec<&[f32]> = data.frames.iter().map(|(b, _, _)| b.as_slice()).collect();
    let lin = aevum_core::timeblend::time_slice(&refs, w, h, &spec)
        .map_err(|e| crate::i18n::tr_fmt("时间合成失败：{}", &[&crate::i18n::tr(e.cn())]))?;
    Ok((data, lin, w, h))
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
#[path = "state/tests/auto_tests.rs"]
mod auto_tests;
#[cfg(test)]
#[path = "state/tests/bypass_tests.rs"]
mod bypass_tests;
#[cfg(test)]
#[path = "state/tests/command_tests.rs"]
mod command_tests;
#[cfg(test)]
#[path = "state/tests/dialog_tests.rs"]
mod dialog_tests;
#[cfg(test)]
#[path = "state/tests/export_write_tests.rs"]
mod export_write_tests;
#[cfg(test)]
#[path = "state/tests/focus_align_wiring_tests.rs"]
mod focus_align_wiring_tests;
#[cfg(test)]
#[path = "state/tests/focus_depth_tests.rs"]
mod focus_depth_tests;
#[cfg(test)]
#[path = "state/tests/frame_tests.rs"]
mod frame_tests;
#[cfg(test)]
#[path = "state/tests/heal_tests.rs"]
mod heal_tests;
#[cfg(test)]
#[path = "state/tests/history_tests.rs"]
mod history_tests;
#[cfg(test)]
#[path = "state/tests/merge_settings_tests.rs"]
mod merge_settings_tests;
#[cfg(test)]
#[path = "state/tests/notice_tests.rs"]
mod notice_tests;
#[cfg(test)]
#[path = "state/tests/panel_tests.rs"]
mod panel_tests;
#[cfg(test)]
#[path = "state/tests/preview_tests.rs"]
mod preview_tests;
#[cfg(test)]
#[path = "state/tests/raw_import_tests.rs"]
mod raw_import_tests;
#[cfg(test)]
#[path = "state/tests/reset_cache_tests.rs"]
mod reset_cache_tests;
