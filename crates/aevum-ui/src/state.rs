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
/// 播放时提前取回多少帧的缩略图。按 24fps 算是两秒的画面，够盖住一次机身预览
/// 解码加纹理上传的往返；再长就只是把内存花在还没播到的帧上。
const PLAY_THUMB_AHEAD: usize = 48;
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
    Preview(Box<PreviewMsg>),
    Thumbs(Vec<ThumbMsg>),
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
    /// `Msg::Loaded` 落账为止——中途任何一次状态改写都不该把它换成一个假的汇总。
    ///
    /// Metering progress after opening a sequence: `Some(frame count)` while that many frames
    /// are being measured, `None` once done. A count rather than a bool so the "measuring N
    /// frames…" status can stand until `Msg::Loaded` lands, and no intermediate status write
    /// replaces it with a summary that claims the work is already done.
    pub measuring: Option<usize>,

    pub params: GradeParams,
    pub preset: String,
    pub auto_note: Option<String>,
    /// 自动校正的 256 长边代理：同一帧重复点自动不重复解码。
    auto_probe: Option<(PathBuf, Vec<f32>, usize, usize)>,

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
    thumb_pending: HashSet<usize>,
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

    pub show_dock: bool,
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
    /// 正在拖动的 (蒙版序号, 手柄 0/1)。
    ///
    /// The (mask index, handle 0/1) currently being dragged.
    pub mask_drag: Option<(usize, u8)>,
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

impl AppState {
    /// 重算剪切遮罩：从最近一次全量预览的 CPU 像素里挑出高光/阴影裁切的
    /// 像素，生成一张与预览同尺寸的 RGBA 遮罩（红=高光、蓝=阴影、其余透明）。
    ///
    /// 只在全量帧到达（!sketch）后调用；草图帧用旧遮罩顶着——遮罩是示意
    /// 罩层，与草图帧的宽高比一致，拉伸显示对得上，精度无关紧要。
    /// 阈值取 LR 同款口径：r、g、b 全部 ≥250 视为高光裁切，全部 ≤5 视为阴影。
    ///
    /// Recompute the clipping mask: pick the highlight / shadow clipped pixels out of the
    /// most recent full preview's CPU pixels and build an RGBA mask of the same size as the
    /// preview (red = highlights, blue = shadows, everything else transparent).
    ///
    /// Called only after a full frame arrives (!sketch); sketch frames keep the old mask --
    /// the mask is an indicative overlay with the same aspect ratio as the sketch, so
    /// stretching it lines up and precision does not matter.
    /// Thresholds follow Lightroom's: r, g and b all >= 250 counts as highlight clipping,
    /// all <= 5 as shadow.
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
    ///
    /// The J key cycles the clipping warning: off -> highlights -> both -> shadows -> off
    /// (Lightroom's order).
    /// Sets the stamp to MAX to force a recompute on the next frame.
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

    /// 把一个滤镜插件作用到当前预览上（看板 B06：后台任务版）。
    ///
    /// 起一条工作线程：真进程 → 握手 → 按 512 的块往返。每块之间向界面报进度，
    /// 用户随时可以取消。结果由 [`AppState::plugin_poll`] 每帧收：成功换一张纹理
    /// 盖在画布上（并等着下一次全量预览把它冲掉），失败进错误横幅并按 id 记一笔。
    ///
    /// Apply a filter plugin to the current preview (kanban B06: background-job version).
    ///
    /// Starts a worker thread: real process -> handshake -> 512-pixel tile round trips.
    /// Progress is reported to the UI between tiles and the user can cancel at any time.
    /// Results are collected each frame by [`AppState::plugin_poll`]: success swaps in a
    /// texture over the canvas (and waits for the next full preview to wash it away),
    /// failure raises the error banner and records one against the id.
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
                .ok_or_else(|| crate::i18n::tr_fmt("找不到插件：{}", &[&id]))?;
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
        let entry =
            entry.ok_or_else(|| crate::i18n::tr_fmt("{} 由宿主内部实现，还没接上", &[&name]))?;
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
        self.status = crate::i18n::tr_fmt("{} 运行中…", &[&name]);
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
    ///
    /// Collect plugin job progress and results once per frame. Called from
    /// [`AppState::poll`]; no separate call needed.
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
                    self.status = crate::i18n::tr_fmt("{} 已取消", &[&job.name]);
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
    ///
    /// The user pressed cancel: raise the flag and return. The rest is up to the worker
    /// thread -- it exits at the next tile boundary, reaps the child process cleanly, and
    /// [`AppState::plugin_poll`] clears the in-flight state once the reply arrives.
    pub fn plugin_cancel(&mut self) {
        if let Some(job) = &self.plugin_job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = crate::i18n::tr_fmt("正在取消 {}…", &[&job.name]);
        }
    }

    /// 有没有插件正在后台跑。在飞期间再点别的插件只会得到一句提示。
    ///
    /// Whether a plugin is running in the background. Clicking another plugin while one is
    /// in flight only yields a message.
    pub fn plugin_running(&self) -> bool {
        self.plugin_job.is_some()
    }

    /// 当前偏好的快照（外观 + 布局 + 字号 + 语言 + 折叠态），落盘与回填都用它。
    ///
    /// A snapshot of the current preferences (appearance + layout + font step + language +
    /// folding state), used for both persisting and restoring.
    pub fn settings(&self) -> crate::settings::Settings {
        // 折叠态按登记表次序输出：`HashSet` 的遍历顺序每次进程都不同，原样写文件
        // 会让「什么都没改」也产生 diff，也让 golden/回归比对没法做。排序之后
        // 同一份界面状态永远写出一份字节相同的 JSON。
        let collapsed: Vec<String> = GROUP_SPECS
            .iter()
            .filter(|s| self.collapsed.contains(s.key))
            .map(|s| s.key.to_owned())
            .collect();
        crate::settings::Settings {
            appearance: self.appearance,
            layout: self.layout,
            font_level: self.font_level,
            lang: self.lang,
            collapsed: Some(collapsed),
            panel_solo: Some(self.panel_solo),
            hdr_preview: Some(self.hdr_preview),
            preset_order: Some(self.preset_order.iter().map(|k| (*k).to_owned()).collect()),
            win_pos: (!self.win_pos.is_empty()).then(|| self.win_pos.clone()),
            // 合并参数是全局偏好：结果态（*_last / *_error / 预览纹理）不进来。
            merge: crate::settings::MergeParams {
                hdr_around: self.hdr_around,
                stack_around: self.stack_around,
                stack_mode: self.stack_mode.key().to_owned(),
                stack_sigma: self.stack_sigma,
                stack_fill: self.stack_fill,
                tb_around: self.tb_around,
                tb_layout: self.tb_layout.key().to_owned(),
                tb_bands: self.tb_bands,
                tb_feather: self.tb_feather,
                tb_horizon: self.tb_horizon,
                tb_horizon_auto: self.tb_horizon_auto,
                tb_deghost: self.tb_deghost,
                tb_match: self.tb_match,
                focus_around: self.focus_around,
                focus_method: self.focus_method.key().to_owned(),
                focus_radius: self.focus_radius,
                focus_threshold: self.focus_threshold,
                focus_smooth_z: self.focus_smooth_z,
                focus_feather: self.focus_feather,
                focus_align: self.focus_align,
            },
        }
    }

    /// 启动时把落盘的偏好灌回来。不动任何图像状态，只改外观、布局与合并面板参数。
    ///
    /// Pour the persisted preferences back in at startup. Touches no image state, only
    /// appearance, layout and the merge panels' parameters.
    pub fn adopt_settings(&mut self, s: crate::settings::Settings) {
        self.appearance = s.appearance;
        self.layout = s.layout;
        self.font_level = s.font_level.min(3);
        self.lang = s.lang;
        // `None` = 老设置文件里没有这一项（或者从没打开过外观窗）→ 用首屏默认态；
        // `Some(vec![])` = 用户把每一组都展开了，这是明确表态，必须尊重。
        self.collapsed = match s.collapsed {
            Some(list) => list.iter().filter_map(|k| legacy_group_key(k)).collect(),
            None => default_collapsed(),
        };
        self.panel_solo = s.panel_solo.unwrap_or(true);
        self.win_pos = s.win_pos.unwrap_or_default();
        self.hdr_preview = s.hdr_preview.unwrap_or(false);
        self.preset_order = normalise_preset_order(&s.preset_order.unwrap_or_default());
        // 合并参数是全局偏好：坏 key / 越界值当场夹回默认，不让手改过的
        // settings.json 把首轮合成带进一个不存在的模式。
        //
        // Merge parameters are a global preference: unparsable keys / out-of-range
        // values are clamped back to the defaults here, so a hand-edited settings.json
        // cannot drag the first merge into a mode that does not exist.
        let m = &s.merge;
        self.hdr_around = m.hdr_around.clamp(1, 4);
        self.stack_around = m.stack_around.clamp(1, 32);
        self.stack_mode = aevum_core::stack::Mode::parse(&m.stack_mode)
            .unwrap_or(aevum_core::stack::Mode::Lighten);
        self.stack_sigma = if m.stack_sigma.is_finite() && m.stack_sigma > 0.0 {
            m.stack_sigma.clamp(1.0, 8.0)
        } else {
            3.0
        };
        self.stack_fill = m.stack_fill;
        self.tb_around = m.tb_around.clamp(1, 31);
        self.tb_layout = aevum_core::timeblend::Layout::parse(&m.tb_layout)
            .unwrap_or(aevum_core::timeblend::Layout::SkyGround);
        self.tb_bands = m.tb_bands.clamp(1, 32);
        self.tb_feather = m.tb_feather.clamp(0.0, 0.5);
        self.tb_horizon = m.tb_horizon.clamp(0.0, 1.0);
        self.tb_horizon_auto = m.tb_horizon_auto;
        self.tb_deghost = if m.tb_deghost.is_finite() {
            m.tb_deghost.clamp(0.0, 4.0)
        } else {
            0.5
        };
        self.tb_match = m.tb_match;
        self.focus_around = m.focus_around.clamp(1, 31);
        self.focus_method = aevum_core::focus::Method::parse(&m.focus_method)
            .unwrap_or(aevum_core::focus::Method::DepthMap);
        self.focus_radius = m.focus_radius.min(32);
        self.focus_threshold = if m.focus_threshold.is_finite() {
            m.focus_threshold.clamp(0.0, 100.0)
        } else {
            0.0
        };
        self.focus_smooth_z = m.focus_smooth_z.min(16);
        self.focus_feather = if m.focus_feather.is_finite() {
            m.focus_feather.clamp(0.0, 64.0)
        } else {
            2.0
        };
        // 布尔无需清洗：serde 的 `#[serde(default)]` 已保证缺字段落 `false`。
        //
        // Nothing to sanitise for a boolean: `#[serde(default)]` already lands a missing
        // field on `false`.
        self.focus_align = m.focus_align;
    }

    /// 把某一节拖到第 `to` 位。越界夹住、原地不动就不置脏。
    ///
    /// Move a section to position `to`. Out-of-range values are clamped, and dropping it
    /// where it already is does not set the dirty flag.
    pub fn move_preset_section(&mut self, key: &'static str, to: usize) {
        let Some(from) = self.preset_order.iter().position(|k| *k == key) else {
            return;
        };
        let to = to.min(self.preset_order.len().saturating_sub(1));
        if from == to {
            return;
        }
        let k = self.preset_order.remove(from);
        self.preset_order.insert(to, k);
        self.settings_dirty = true;
    }

    /// 记一个浮窗的新位置，等帧尾统一落盘。
    ///
    /// `pointer_busy` 传 `ctx.input(|i| i.pointer.any_down())`：拖动过程中每帧坐标都在变，逐帧
    /// 落盘等于拿 `settings.json` 当草稿纸。松手那一刻值才稳定，也只在那时记一次。
    /// 若窗口被拖回原位，`get != Some(&v)` 不成立，连脏标记都不会置。
    ///
    /// Record a floating window's new position for the end-of-frame write.
    ///
    /// `pointer_busy` is `ctx.input(|i| i.pointer.any_down())`: while dragging, the coordinates change
    /// every frame and persisting each one turns `settings.json` into scratch paper. Only
    /// when the button is released is the value stable, and only then is it recorded. If the
    /// window was dragged back where it started, `get != Some(&v)` fails and not even the
    /// dirty flag is set.
    pub fn note_win_pos(&mut self, key: &str, pos: egui::Pos2, pointer_busy: bool) {
        if pointer_busy {
            return;
        }
        let v = [pos.x, pos.y];
        if self.win_pos.get(key) != Some(&v) {
            self.win_pos.insert(key.to_owned(), v);
            self.settings_dirty = true;
        }
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
            collapsed: default_collapsed(),
            panel_solo: true,
            hdr_preview: false,
            preset_order: default_preset_order(),
            drag_section: None,
            win_pos: std::collections::BTreeMap::new(),
            settings_dirty: false,
            status: "打开一个帧文件夹，或把文件夹拖进窗口".to_owned(),
            error: None,
            notices: Vec::new(),
            sidecar_hint: None,
            measuring: None,
            params: GradeParams::default(),
            preset: "原始".to_owned(),
            auto_note: None,
            auto_probe: None,
            def_window: 15.0,
            def_strength: 100.0,
            def_multi: false,
            def_channels: false,
            def_align_exif: false,
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
            lang: crate::i18n::LangPref::default(),
            texture: None,
            texture_orig: None,
            preview_size: [0, 0],
            preview_stamp: 0,
            preview_dirty: false,
            preview_inflight: false,
            preview_dragging: false,
            preview_owes_full: false,
            preview_has_orig: false,
            bypassed: BTreeSet::new(),
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
            // 星轨默认给 9 帧（±4）：再少星轨连不成线，再多读盘就成了等待。
            stack_around: 4,
            stack_mode: aevum_core::stack::Mode::Lighten,
            stack_sigma: 3.0,
            stack_fill: true,
            stack_last: None,
            stack_error: None,
            // 时间切片默认 8 帧、4% 羽化，正好铺满一屏八带。
            tb_around: 7,
            tb_layout: aevum_core::timeblend::Layout::SkyGround,
            tb_bands: 8,
            tb_feather: 0.04,
            tb_horizon: 0.5,
            tb_horizon_auto: false,
            tb_deghost: 0.5,
            tb_match: true,
            tb_last: None,
            tb_error: None,
            tb_preview: None,
            tb_preview_img: None,
            tb_preview_key: 0,
            tb_preview_inflight: false,
            tb_preview_data: None,
            tb_preview_error: None,
            // 焦点合成默认 8 帧（±7 从起始帧往后铺）、深度图法、半径 2、兜底关、3×3 去斑、2 px 羽化。
            // 合成前自动对齐默认关（与 `MergeParams::default()` 同步）。
            focus_around: 7,
            focus_method: aevum_core::focus::Method::DepthMap,
            focus_radius: 2,
            focus_threshold: 0.0,
            focus_smooth_z: 1,
            focus_feather: 2.0,
            focus_align: false,
            focus_last: None,
            focus_error: None,
            focus_depth_tex: None,
            focus_depth_note: None,
            // 判据默认值与核心一致：±1 EV 之外判废、相邻突变 1 EV、清晰度不设限。
            cull_ev_low: -1.0,
            cull_ev_high: 1.0,
            cull_jump: 1.0,
            cull_sharp: 0.0,
            cull_note: None,
            cull_target: String::new(),
            stamp_on: false,
            resolution: None,
            source_note: None,
            mask_sel: None,
            mask_comp: 0,
            mask_drag: None,
            mask_paint: None,
            heal_radius: 2.0,
            heal_feather: 35.0,
            heal_clone: false,
            heal_src: None,
            heal_last: None,
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
            curve_tab: 0,
            mixer_tab: 0,
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
            render_dynamic: OutputDynamic::default(),
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
            merge_job: 0,
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
            eta: EtaWindow::default(),
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
    ///
    /// Whether the material is a single photo. Sequence-level features (alignment,
    /// deflicker) only make sense with neighbouring frames.
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
    ///
    /// This frame's deflicker compensation, fed straight into the develop chain.
    ///
    /// In single-channel mode `chan` is all zeros, bit-identical to the old brightness-only
    /// definition; with per-channel on, `add` carries the three-channel mean and `chan` keeps
    /// only the colour difference, and the two together restore each channel's original
    /// offset exactly.
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
    ///
    /// A batch of paths dropped or picked: a single one may be a directory or a photo,
    /// several are taken as a photo list.
    ///
    /// A single photo must not degrade into "open the whole folder it sits in" -- the one the
    /// user picked would drown among its neighbours, and the guard directory and the exports
    /// would land in a directory he never chose.
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
        self.stack_last = None;
        self.stack_error = None;
        self.tb_last = None;
        self.tb_error = None;
        self.tb_preview = None;
        self.tb_preview_img = None;
        self.tb_preview_key = 0;
        self.tb_preview_inflight = false;
        self.tb_preview_data = None;
        self.tb_preview_error = None;
        // 深度图预览只对「这一个序列的这一组参数」成立：换序列 / 关闭一律作废。
        //
        // The depth-map preview is only valid for one selection in one sequence: switching
        // or closing voids it.
        self.focus_depth_tex = None;
        self.focus_depth_note = None;
        self.merge_kind = None;
        self.merge_result = None;
        self.merge_error = None;
        if self.merge_busy {
            // 换序列时正在跑的合并作废：只转取消旗，忙碌标记留给旧 job 自己的回包清。
            // 这里若抢清 `merge_busy`，用户就能在新序列上再起一个合并，而旧线程还在
            // 往上一份成片里写——同一个出口两条线程。
            //
            // A sequence switch only raises the cancel flag; the busy flag waits for the old
            // run's own reply. Clearing it here would let a second merge start while the old
            // thread is still writing the previous output.
            self.cancel_merge.store(true, Ordering::SeqCst);
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
        self.measuring = Some(n);
        self.resolution = None;
        self.source_note = None;
        self.status = if single {
            "正在读取这张照片…".to_owned()
        } else {
            crate::i18n::tr_fmt("正在测量 {} 帧…", &[&n])
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
                        Some(crate::i18n::tr_fmt(
                            "{} · {} 位",
                            &[&i.path.short(), &i.bits],
                        )),
                    ),
                    Err(_) => (None, None),
                },
                Some(p) if aevum_core::dng::is_named_dng(p) => match aevum_core::dng::info(p) {
                    Ok(i) => (
                        Some(rotated((i.width, i.height))),
                        Some(crate::i18n::tr_fmt("DNG · 传感器直解 · {} 位", &[&i.bits])),
                    ),
                    // 内置解析只认单通道原始数据，三通道 LinearRaw（iPhone 导出的 DNG）
                    // 会回落后端解码——尺寸和保真度就得问后端。不回落的话，面板会对着
                    // 一张能正常显示的图说「尺寸不明」，两处说法自相矛盾。
                    Err(e) if e.soft_fallback() => match aevum_core::raw::info(p) {
                        Ok(i) => (
                            Some(rotated((i.width, i.height))),
                            Some(crate::i18n::tr_fmt(
                                "{} · {} 位",
                                &[&i.path.short(), &i.bits],
                            )),
                        ),
                        Err(_) => (None, None),
                    },
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
        // 状态栏留在「正在测量 N 帧…」：这时候测光才刚起跑，先把帧数写在前面
        // 等于告诉用户已经读完了。真正的汇总由 `Msg::Loaded` 落账时替换。
        //
        // Keep the "measuring N frames…" status: the metering pass has only just started, and
        // writing the summary here would claim it is already done. `Msg::Loaded` replaces it.
    }

    /// 关闭当前序列，回到空状态；不碰磁盘上的素材。
    ///
    /// Close the current sequence and return to the empty state; never touches footage on
    /// disk.
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
        self.stack_last = None;
        self.stack_error = None;
        self.tb_last = None;
        self.tb_error = None;
        self.tb_preview = None;
        self.tb_preview_img = None;
        self.tb_preview_key = 0;
        self.tb_preview_inflight = false;
        self.tb_preview_data = None;
        self.tb_preview_error = None;
        // 深度图预览只对「这一个序列的这一组参数」成立：换序列 / 关闭一律作废。
        //
        // The depth-map preview is only valid for one selection in one sequence: switching
        // or closing voids it.
        self.focus_depth_tex = None;
        self.focus_depth_note = None;
        self.merge_kind = None;
        self.merge_result = None;
        self.merge_error = None;
        if self.merge_busy {
            // 换序列时正在跑的合并作废：只转取消旗，忙碌标记留给旧 job 自己的回包清。
            // 这里若抢清 `merge_busy`，用户就能在新序列上再起一个合并，而旧线程还在
            // 往上一份成片里写——同一个出口两条线程。
            //
            // A sequence switch only raises the cancel flag; the busy flag waits for the old
            // run's own reply. Clearing it here would let a second merge start while the old
            // thread is still writing the previous output.
            self.cancel_merge.store(true, Ordering::SeqCst);
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
        self.measuring = None;
        self.playing = false;
        self.zoom = 1.0;
        self.fit_scale = 1.0;
        self.pan = Vec2::ZERO;
        self.module = Module::Sequence;
        self.last_saved = None;
        self.error = None;
        self.status = crate::i18n::t!("打开一个帧文件夹，或把文件夹拖进窗口").to_owned();
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
        history::note(
            "rating",
            crate::i18n::tr_fmt("帧 {} 评 {} 星", &[&format!("{:04}", i + 1), &stars]),
        );
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
        let mark = match flag {
            1 => "选用",
            -1 => "排除",
            _ => "取消标记",
        };
        history::note(
            "flag",
            crate::i18n::tr_fmt(
                "帧 {} {}",
                &[&format!("{:04}", i + 1), &crate::i18n::tr(mark)],
            ),
        );
    }

    pub fn selected_flag(&self) -> i8 {
        self.flag_at(self.selected)
    }

    /// 一次标一批帧（筛选台用）：整批只记一行历史，不逐帧刷。
    ///
    /// Flag a batch of frames at once (the cull station): one history line for the whole batch
    /// instead of one per frame.
    pub fn flag_many(&mut self, indices: &[usize], flag: i8) {
        for i in indices {
            if flag == 0 {
                self.flags.remove(i);
            } else {
                self.flags.insert(*i, flag.signum());
            }
        }
        if !indices.is_empty() {
            let verb = if flag < 0 { "排除" } else { "选用" };
            history::note(
                "flag",
                crate::i18n::tr_fmt("{} 帧{}", &[&indices.len(), &crate::i18n::tr(verb)]),
            );
        }
    }

    /// 画布叠印的那一行字：帧号 · 拍摄时间 · 机型与曝光。缺什么省什么。
    ///
    /// The line burned onto the canvas: frame number, capture time, camera and exposure.
    /// Anything missing is simply left out.
    pub fn stamp_text(&self) -> Option<String> {
        let total = self.frame_count();
        if total == 0 {
            return None;
        }
        let index = self.selected.min(total - 1);
        Some(stamp_line(index, total, self.exif.get(index)))
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
            crate::i18n::tr_fmt(
                "帧 {} 色标「{}」",
                &[
                    &format!("{:04}", i + 1),
                    &crate::i18n::tr(LABEL_NAMES[label.min(4) as usize]),
                ],
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

    /// 把 core 压下的诊断提示搬进状态栏。
    ///
    /// 三条口径：
    /// - **保序**：core 那边先说的先显示，同一件事的因果不会被倒过来读；
    /// - **去重**：同一句话只留一条（用户点掉之后再来的同一句才重新出现）；
    /// - **上限 8 条，旧的先走**：新的问题必须留得住，否则刚触发的那条会被老账顶掉。
    ///
    /// 每帧开头调一次就够：`env::drain_notes` 是取走并清空，重复调用不会丢东西。
    ///
    /// Move the diagnostics core has queued into the status bar.
    ///
    /// Three rules:
    /// - **order preserved**: whatever core said first is shown first, so cause and effect
    ///   are never read backwards;
    /// - **deduplicated**: one copy per sentence (the same sentence reappears only after the
    ///   user dismissed it);
    /// - **capped at 8, oldest leaves first**: new problems must survive, otherwise the one
    ///   just triggered would be pushed out by old debts.
    ///
    /// Once at the start of each frame is enough: `env::drain_notes` takes and clears, so
    /// repeated calls lose nothing.
    pub fn drain_notices(&mut self) {
        for msg in aevum_core::env::drain_notes() {
            if self.notices.iter().any(|m| m == &msg) {
                continue;
            }
            self.notices.push(msg);
            if self.notices.len() > NOTICE_LIMIT {
                self.notices.remove(0);
            }
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
        // 偏好（折叠态）落盘的唯一出口。界面那边只置 `settings_dirty`，帧尾在这里
        // 写一次：右键「全部收起」会一口气改十几个键，逐处写会把文件反复搓。
        // 写失败不打断用户（`settings::save` 自己会出声），置回 false 免得每帧重试。
        if self.settings_dirty {
            self.settings_dirty = false;
            crate::settings::save(&self.settings());
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
                    self.measuring = None;
                    self.recompute_deflicker();
                    self.preview_dirty = true;
                    let valid = self.metrics.iter().filter(|m| m.is_some()).count();
                    self.status = crate::i18n::tr_fmt(
                        "{} 帧 · 可读 {} · 缺帧 {}",
                        &[&self.frame_count(), &valid, &self.issues.len()],
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
                    self.status = crate::i18n::tr_fmt("已保存 {}", &[&path]);
                }
                Msg::PreviewFailed(e) => {
                    self.preview_inflight = false;
                    self.error = Some(e);
                }
                Msg::Aligned { token, report } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.align_busy = false;
                    self.align_msg = Some(crate::i18n::tr_fmt(
                        "对齐完成 · {} 帧 · 裁切 {}×{} · {}s · 异常 {}",
                        &[
                            &report.offsets.len(),
                            &report.crop.w,
                            &report.crop.h,
                            &format!("{:.1}", report.elapsed_secs),
                            &report.issues.len(),
                        ],
                    ));
                    self.status = self.align_msg.clone().unwrap_or_default();
                    self.align_report = Some(report);
                    self.preview_dirty = true;
                }
                Msg::AlignFailed(e) => {
                    self.align_busy = false;
                    self.align_msg = Some(crate::i18n::tr_fmt("对齐失败：{}", &[&e]));
                    self.error = Some(crate::i18n::tr_fmt("对齐失败：{}", &[&e]));
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
                        crate::i18n::tr_fmt(
                            "已导出 {}（续跑跳过 {} 帧）",
                            &[&path.display(), &skipped],
                        )
                    } else {
                        crate::i18n::tr_fmt("已导出 {}", &[&path.display()])
                    };
                }
                Msg::ExportFailed(e) => {
                    self.export_busy = false;
                    self.export_stage = None;
                    if e.contains("已取消") {
                        self.status = crate::i18n::t!("导出已取消").to_owned();
                    } else {
                        self.error = Some(crate::i18n::tr_fmt("导出失败：{}", &[&e]));
                    }
                }
                Msg::Merged {
                    token,
                    job,
                    kind,
                    result,
                } => {
                    // 只有当前这个 job 的回包能动忙碌标记：换序列时旧的取消旗已经立起来，
                    // 但旧线程可能还在两帧之间，`merge_busy` 要等它自己收尾。
                    //
                    // Only the current run's reply may touch the busy flag: on a sequence
                    // switch the old cancel flag is raised, but the old thread may still be
                    // between frames, so `merge_busy` waits for it to finish on its own.
                    if job != self.merge_job {
                        continue;
                    }
                    self.merge_busy = false;
                    self.merge_kind = None;
                    if token != self.reload_token {
                        continue;
                    }
                    // 面板那两行回显按任务种类分头落账：HDR 的失败不该顶掉全景的结果。
                    let (last, err) = match kind {
                        MergeKind::Hdr => (&mut self.hdr_last, &mut self.hdr_error),
                        MergeKind::Pano => (&mut self.pano_last, &mut self.pano_error),
                        MergeKind::Stack => (&mut self.stack_last, &mut self.stack_error),
                        MergeKind::TimeBlend => (&mut self.tb_last, &mut self.tb_error),
                        MergeKind::Focus => (&mut self.focus_last, &mut self.focus_error),
                    };
                    match result {
                        Ok(v) => {
                            let path = v["path"].as_str().unwrap_or_default().to_owned();
                            *last = Some(path.clone());
                            *err = None;
                            self.merge_result = Some(v);
                            self.merge_error = None;
                            self.status = crate::i18n::tr_fmt(
                                "{}完成：{}",
                                &[&crate::i18n::tr(kind.cn()), &path],
                            );
                        }
                        Err(e) => {
                            self.merge_result = None;
                            self.merge_error = Some(e.clone());
                            *last = None;
                            *err = Some(e.clone());
                            if e.contains("已取消") {
                                self.status =
                                    crate::i18n::tr_fmt("{}已取消", &[&crate::i18n::tr(kind.cn())]);
                            } else {
                                self.status =
                                    crate::i18n::tr_fmt("{}失败", &[&crate::i18n::tr(kind.cn())]);
                                self.error = Some(crate::i18n::tr_fmt(
                                    "{}失败：{}",
                                    &[&crate::i18n::tr(kind.cn()), &e],
                                ));
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
                                crate::i18n::tr_fmt("体检完成：发现 {} 处", &[&bad])
                            };
                        }
                        Err(e) => {
                            self.qa_error = Some(e.clone());
                            if e.contains("已取消") {
                                self.status = "体检已取消".to_owned();
                            } else {
                                self.error = Some(crate::i18n::tr_fmt("体检失败：{}", &[&e]));
                            }
                        }
                    }
                }
                Msg::TimeblendPreview {
                    token,
                    key,
                    image,
                    frames,
                } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.tb_preview_inflight = false;
                    if let Some(data) = frames {
                        // 解码缓存照收：哪怕参数又变了，帧没变就还能复用。
                        self.tb_preview_data = Some(data);
                    }
                    if key != self.tb_preview_key {
                        continue; // 迟到的旧参数回包：不上屏
                    }
                    let img = *image;
                    self.tb_preview_img = Some(Arc::new(img.clone()));
                    // 纹理按名字复用：同参数只重传像素，不重新在 GPU 上分配。
                    match &mut self.tb_preview {
                        Some((k, tex)) if *k == key => tex.set(img, TextureOptions::LINEAR),
                        _ => {
                            self.tb_preview = Some((
                                key,
                                ctx.load_texture("timeblend.preview", img, TextureOptions::LINEAR),
                            ));
                        }
                    }
                }
                Msg::TimeblendPreviewFail { token, key, error } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.tb_preview_inflight = false;
                    if key == self.tb_preview_key {
                        self.tb_preview_error = Some(error);
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
                        self.status = crate::i18n::tr_fmt(
                            "队列第 {}/{} 段：{}",
                            &[
                                &(index + 1),
                                &self.queue.len(),
                                &crate::i18n::tr(status.label()),
                            ],
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
                        crate::i18n::tr_fmt("队列已取消：完成 {} 段，失败 {} 段", &[&ok, &failed])
                    } else if failed > 0 {
                        crate::i18n::tr_fmt("队列完成：成功 {} 段，失败 {} 段", &[&ok, &failed])
                    } else {
                        crate::i18n::tr_fmt("队列完成：{} 段", &[&ok])
                    };
                    if failed > 0 && !cancelled {
                        self.error = Some(crate::i18n::tr_fmt(
                            "队列有 {} 段没渲成，逐行查看原因",
                            &[&failed],
                        ));
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
        self.drain_control_events(ctx);
        self.sync_control(ctx);
    }

    /// 收尾控制通道：把在线程里排队的请求在本帧里执行掉。
    ///
    /// 迟到的命令不执行：客户端最多等 [`control::UI_TIMEOUT`] 就报超时，
    /// 一条排队超过这个上限才轮到的事件，说明客户端多半已经超时走人、
    /// 甚至可能重试了一遍。照常执行的话 export.video / queue.run /
    /// hdr.merge 这类批量写命令会被跑两遍（审查确认的重复执行路径）。
    /// 回复照发（对端通常已断开，发送失败无妨），至少语义是完整的。
    /// 独立成方法是为了让"迟到不执行"这条规则能脱离整帧 UI 直接测试。
    fn drain_control_events(&mut self, ctx: &Context) {
        loop {
            let event = match self.control_rx.try_recv() {
                Ok(event) => event,
                Err(_) => break,
            };
            let waited = event.queued_at.elapsed();
            if waited > control::UI_TIMEOUT {
                let _ = event.reply.send(Err(crate::i18n::tr_fmt(
                    "命令 {} 排队 {} 秒才轮到执行，超过 {} 秒等待上限：为避免与客户端超时重试撞车，已放弃执行",
                    &[
                        &event.id,
                        &format!("{:.0}", waited.as_secs_f64()),
                        &format!("{:.0}", control::UI_TIMEOUT.as_secs_f64()),
                    ],
                )));
                continue;
            }
            let result = self.run_ui_command(ctx, &event.id, &event.args);
            let _ = event.reply.send(result);
        }
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
                        queued_at: Instant::now(),
                        reply,
                    })
                    .map_err(|_| "界面线程已关闭，控制通道不可用".to_owned())?;
            }
            // 立刻唤醒界面：空闲的 egui 不会自己重绘，请求会一直排队。
            ctx.request_repaint();
            wait.recv_timeout(control::UI_TIMEOUT).map_err(|_| {
                crate::i18n::tr_fmt(
                    "命令 {} 超时：界面 {} 秒内没有响应",
                    &[&id, &format!("{:.0}", control::UI_TIMEOUT.as_secs_f64())],
                )
            })?
        };
        match ControlServer::start(&root, Arc::new(executor)) {
            Ok(server) => self.control = Some(server),
            Err(e) => self.error = Some(crate::i18n::tr_fmt("控制服务启动失败：{}", &[&e])),
        }
    }

    /// 给界面展示用的控制文件信息；没开服务时为 None。
    ///
    /// The control file info for display; None when the server is not running.
    pub fn control_file(&self) -> Option<&ControlFile> {
        self.control.as_ref().map(ControlServer::file)
    }

    pub fn control_path(&self) -> Option<PathBuf> {
        self.control.as_ref().map(|s| s.path().to_path_buf())
    }

    // ------------------------------------------------------------ 撤销 / 重做

    /// 抓一份可撤销状态。视图（缩放、面板、当前帧）与只读结果（测光、直方图）不在
    /// 里面，见 [`Snapshot`]。
    ///
    /// Capture an undoable state. View state (zoom, panels, current frame) and read-only
    /// results (metering, histogram) are not part of it; see [`Snapshot`].
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            params: self.params.clone(),
            preset: self.preset.clone(),
            def_window: self.def_window,
            def_strength: self.def_strength,
            def_multi: self.def_multi,
            def_channels: self.def_channels,
            def_align_exif: self.def_align_exif,
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
            render_dynamic: self.render_dynamic,
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
    ///
    /// Replay a snapshot: the wind-down is the same as after a human edit in the panel --
    /// deflicker recomputes when it should, probes re-probe when they should.
    pub fn restore(&mut self, snap: Snapshot) {
        let def_changed = self.def_window != snap.def_window
            || self.def_strength != snap.def_strength
            || self.def_multi != snap.def_multi
            || self.def_channels != snap.def_channels
            || self.def_align_exif != snap.def_align_exif;
        let audio_changed = self.render_audio != snap.render_audio;
        self.params = snap.params;
        self.preset = snap.preset;
        self.def_window = snap.def_window;
        self.def_strength = snap.def_strength;
        self.def_multi = snap.def_multi;
        self.def_channels = snap.def_channels;
        self.def_align_exif = snap.def_align_exif;
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
        self.render_dynamic = snap.render_dynamic;
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
    ///
    /// Switching or closing a sequence: the history restarts, because the previous step
    /// belongs to the previous project.
    pub fn history_reset(&mut self, label: &str) {
        history::clear_note();
        let snap = self.snapshot();
        self.history.reset(snap, label);
    }

    /// 把当前参数钉成一份命名快照，挂在左栏「快照」那一节。
    ///
    /// 名字取当前预设 + 序号：`原始 · 2`。比纯序号好认——一列「快照 1/2/3」
    /// 过两天谁也分不清哪份是满意的。
    ///
    /// Pin the current parameters as a named snapshot, listed in the left column's snapshot
    /// section.
    ///
    /// The name is the current preset plus an index. That beats a bare index -- a column of
    /// "Snapshot 1/2/3" leaves nobody able to tell which one they liked two days later.
    pub fn save_snapshot(&mut self) {
        // 预设名按当前语言翻译进 label：快照列表直接显示这串。序号与
        // 分隔符跟在后面 —— 一列「快照 1/2/3」过两天谁也分不清哪份是满意的。
        let preset = crate::i18n::tr(&self.preset);
        let label = crate::i18n::tr_fmt("{} · {}", &[&preset, &(self.snapshots.len() + 1)]);
        let snap = self.snapshot();
        self.snapshots.push((label.clone(), snap));
        self.status = crate::i18n::tr_fmt("已存快照「{}」", &[&label]);
    }

    /// 回到某一份快照。整组参数换回去，照样进历史，撤得回来。
    ///
    /// Go back to one snapshot. The whole parameter set is swapped back and still enters the
    /// history, so it can be undone.
    pub fn restore_snapshot(&mut self, i: usize) {
        let Some((label, snap)) = self.snapshots.get(i).cloned() else {
            return;
        };
        self.restore(snap);
        self.status = crate::i18n::tr_fmt("已回到快照「{}」", &[&label]);
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
    ///
    /// At the end of a UI frame, compare the current state with the snapshot at the cursor:
    /// record a step if they differ.
    ///
    /// The criterion is the state itself, not "did any control register" -- canvas handles,
    /// colour wheels, and future controls that forgot to register a label still reach the
    /// history; the row is merely worded as a field name. A registered label wins over that.
    pub fn history_commit(&mut self) {
        let snap = self.snapshot();
        if !self.history.dirty(&snap) {
            history::clear_note();
            return;
        }
        match history::take_note() {
            Some((key, label)) => self.history.push(snap, &key, label),
            // 没登记控件的路径：整步标成「字段比对」，合并按类认（见 Step::cat）。
            None => {
                let hint = self.history.diff_hint(&snap);
                self.history.push_diff(snap, hint);
            }
        }
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
                    crate::i18n::tr_fmt("改 {}", &[&[fields.join("、"), more.to_owned()].join("")]),
                    true,
                )
            }
            "grade.preset" => (
                "grade.preset".to_owned(),
                crate::i18n::tr_fmt("预设 {}", &[&named("name")]),
                true,
            ),
            "grade.reset" => ("grade.reset".to_owned(), "复位全部参数".to_owned(), true),
            "grade.copy" => {
                let label = match args.get("op").and_then(Value::as_str).unwrap_or("list") {
                    "save" => crate::i18n::tr_fmt("存副本 {}", &[&named("name")]),
                    "apply" => crate::i18n::tr_fmt("套用副本 {}", &[&named("name")]),
                    "delete" => crate::i18n::tr_fmt("删副本 {}", &[&named("name")]),
                    "rename" => crate::i18n::tr_fmt("副本改名 {}", &[&named("to")]),
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
                    crate::i18n::tr_fmt("加{}蒙版", &[&Self::cn_mask_kind(kind)]),
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
                    crate::i18n::tr_fmt(
                        "取景框 {} × {}",
                        &[
                            &args["width"].as_u64().unwrap_or(0),
                            &args["height"].as_u64().unwrap_or(0),
                        ],
                    )
                } else if let Some(deg) = args.get("rotate").and_then(Value::as_f64) {
                    crate::i18n::tr_fmt("拉直 {}°", &[&format!("{deg:+.1}")])
                } else {
                    crate::i18n::tr_fmt("画幅 {}", &[&named("aspect")])
                };
                ("crop.set".to_owned(), label, true)
            }
            "audio.config" => ("audio.config".to_owned(), "配乐设置".to_owned(), true),
            "render.set" => {
                // 一行点出这次动的是哪个设置；纯查询没有改动，dirty 判定会吞掉这步。
                let label = if args.get("format").is_some() {
                    crate::i18n::tr_fmt("输出容器 {}", &[&named("format")])
                } else if args.get("quality").is_some() {
                    crate::i18n::tr_fmt("输出质量 {}", &[&args["quality"].as_u64().unwrap_or(0)])
                } else if args.get("sharpen_for").is_some() {
                    crate::i18n::tr_fmt("输出锐化 {}", &[&named("sharpen_for")])
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
                    crate::i18n::tr_fmt("倍速 {}×", &[&num(args.get("speed"))])
                } else if let Some(ease) = args.get("ease") {
                    crate::i18n::tr_fmt("缓入缓出到 {}×", &[&num(ease.get("peak"))])
                } else if args.get("keys").is_some() {
                    "速度曲线".to_owned()
                } else if args.get("shutter").is_some() {
                    crate::i18n::tr_fmt("运动模糊 {}", &[&num(args.get("shutter"))])
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
                    crate::i18n::tr_fmt("运动曲线 {}", &[&named("ease")])
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
            "cull.evaluate" => (String::new(), "筛选评估".to_owned(), false),
            "cull.extract" => (String::new(), "剔除成子序列".to_owned(), false),
            "stack.merge" => (String::new(), "星空堆栈".to_owned(), false),
            "timeblend.merge" => (String::new(), "时间合成".to_owned(), false),
            "focus.merge" => (String::new(), "焦点合成".to_owned(), false),
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
        self.status = crate::i18n::tr_fmt("撤销 · {}", &[&m.label]);
        Ok(out)
    }

    fn cmd_history_redo(&mut self) -> Result<Value, String> {
        let m = self
            .history
            .redo()
            .ok_or_else(|| "没有可以重做的改动".to_owned())?;
        let out = self.history_move_json(&m, "redo", &m.label.clone());
        self.restore(m.snap);
        self.status = crate::i18n::tr_fmt("重做 · {}", &[&m.label]);
        Ok(out)
    }

    fn cmd_history_jump(&mut self, args: &Value) -> Result<Value, String> {
        let raw = args
            .get("index")
            .or_else(|| args.get("step"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "缺少 index：跳到第几步（1 开始）".to_owned())?;
        if raw == 0 || raw as usize > self.history.steps().len() {
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.history.steps().len().max(1)],
            ));
        }
        let m = self
            .history
            .jump(raw as usize - 1)
            .ok_or_else(|| "历史记录已改变，请重新查询".to_owned())?;
        let label = m.label.clone();
        let out = self.history_move_json(&m, "jump", &label.clone());
        self.restore(m.snap);
        self.status = crate::i18n::tr_fmt("回到 · {}", &[&label]);
        Ok(out)
    }

    /// 历史列表 + 当前光标；面板和 MCP 共用这一份口径。
    ///
    /// The history list plus the current cursor; the panel and MCP share this one
    /// definition.
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
            "cull.evaluate" => self.cmd_cull_evaluate(args),
            "cull.extract" => self.cmd_cull_extract(args),
            "stack.merge" => self.cmd_stack_merge(ctx, args),
            "timeblend.merge" => self.cmd_timeblend_merge(ctx, args),
            "focus.merge" => self.cmd_focus_merge(ctx, args),
            "merge.status" => Ok(self.merge_status_json()),
            "merge.cancel" => {
                let busy = self.merge_busy;
                let kind = self.merge_kind.map(MergeKind::cn);
                self.stop_merge();
                Ok(json!({
                    "cancelling": busy,
                    "note": match (busy, kind) {
                        (true, Some(k)) => crate::i18n::tr_fmt("已请求取消{}；在两帧之间生效，已写下的帧不会删除", &[&k]),
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
            other => Err(crate::i18n::tr_fmt(
                "未知命令：{}（用 system.commands 或 AI 面板的命令清单查看可用命令）",
                &[&other],
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
            "align_exif": self.def_align_exif,
            "ev_frames": self.exif_ev_frames(),
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
        if let Some(v) = args.get("align_exif") {
            self.def_align_exif = v.as_bool().ok_or("align_exif 必须是布尔")?;
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
    ///
    /// After opening a sequence, match a lens profile from the first frame's EXIF. The
    /// lensfun database is looked up on disk; a missing database, an unattached lens, or
    /// missing EXIF all degrade silently -- the three manual values stay as they are, with one
    /// explanatory line.
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
    ///
    /// Run the match with the given EXIF triple and write it into the parameters. The body of
    /// `match_lens_profile`, split out so tests can call it directly.
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
                self.lens_profile_note = crate::i18n::tr_fmt(
                    "已匹配 {}（{}mm · 裁切 {}）",
                    &[&corr.lens, &corr.focal_mm, &format!("{:.1}", corr.crop)],
                );
                self.lens_profile_saved = Some(corr.clone());
                self.params.lens_profile = Some(corr);
                self.preview_dirty = true;
            }
            None => {
                self.lens_profile_note = crate::i18n::tr_fmt(
                    "数据库里没有 {} {} 的标定，畸变与暗角仍走手动三项",
                    &[&make, &lens],
                );
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
    ///
    /// Find a DCP by the EXIF camera body; not finding one only leaves an explanatory line
    /// and changes no current parameter.
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
                self.dcp_note = crate::i18n::tr_fmt("已套用「{}」（{}）", &[&p.name, &p.camera]);
                self.dcp_name = p.name.clone();
                self.params.dcp_profile = p.path;
                self.preview_dirty = true;
            }
            Err(e) => self.dcp_note = e,
        }
    }

    /// 指定一份 `.dcp`。机型对不上就拒绝——配置拿错比不套更糟。
    ///
    /// Specify a `.dcp`. Rejected when the camera model does not match -- the wrong profile
    /// is worse than none.
    pub fn set_dcp_profile(&mut self, path: &str) -> Result<(), String> {
        let p = aevum_core::dcp::Profile::load(path)?;
        if let Some(ex) = self.exif.first() {
            let make = ex.make().unwrap_or("");
            let model = ex.model().unwrap_or("");
            if !model.is_empty() && !p.matches(make, model) {
                return Err(crate::i18n::tr_fmt(
                    "配置是给「{}」的，当前机型是「{} {}」",
                    &[&p.camera, &make, &model],
                ));
            }
        }
        self.dcp_name = p.name.clone();
        self.dcp_note = crate::i18n::tr_fmt("已套用「{}」（{}）", &[&p.name, &p.camera]);
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
    ///
    /// Find an LCP in the default directories by EXIF and fold its correction in.
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
                self.lcp_note = crate::i18n::tr_fmt(
                    "已套用 {}（{} · {}mm）",
                    &[&file, &corr.lens, &format!("{:.0}", corr.focal35_mm)],
                );
                self.params.lcp_profile = Some(corr);
                self.preview_dirty = true;
            }
            Err(e) => self.lcp_note = e,
        }
    }

    /// 指定一份 `.lcp`，按当前 EXIF 选帧。
    ///
    /// Specify a `.lcp`, picking the frame by the current EXIF.
    pub fn set_lcp_profile(&mut self, path: &str) -> Result<(), String> {
        let file = aevum_core::lcp::load(path)?;
        let req = self.exif_lens_request()?;
        let corr = aevum_core::lcp::match_correction(&file, &req)?;
        self.lcp_note = crate::i18n::tr_fmt(
            "已套用 {}（{} · {}mm）",
            &[
                &file.profile,
                &corr.lens,
                &format!("{:.0}", corr.focal35_mm),
            ],
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
            Some(other) => {
                return Err(crate::i18n::tr_fmt(
                    "未知的配置类型：{}（dcp 或 lcp）",
                    &[&other],
                ));
            }
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
                    .ok_or_else(|| crate::i18n::tr_fmt("{} 必须是数字（源像素）", &[&k]))?;
                if !x.is_finite() || x < 0.0 || x > f64::from(fw.max(fh)) {
                    return Err(crate::i18n::tr_fmt(
                        "{} 超出画面范围（0..{}）",
                        &[&k, &fw.max(fh)],
                    ));
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
            "dynamic": self.render_dynamic.key(),
            // HDR 只对 AVIF 成立，容器装不下时面板该让人看出「这个组合不成立」。
            "dynamic_fits": self.render_dynamic.fits(self.render_format),
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
            return Err(crate::i18n::tr_fmt(
                "{} 只能出静帧；视频的中间帧要 PNG 或 TIFF（先把 mode 改成 stills）",
                &[&crate::i18n::tr(format.cn())],
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
                return Err(crate::i18n::tr_fmt(
                    "未知的水印参数 {}；可用 {}",
                    &[&key, &keys],
                ));
            }
            merged.insert(key.clone(), v.clone());
        }
        let next: Watermark = serde_json::from_value(Value::Object(merged))
            .map_err(|e| crate::i18n::tr_fmt("水印参数不对：{}", &[&e]))?;
        if incoming.contains_key("image")
            && !next.image.as_os_str().is_empty()
            && !next.image.is_file()
        {
            let shown = next.image.display();
            return Err(crate::i18n::tr_fmt(
                "水印图片不存在：{}（传空串表示不换/清空）",
                &[&shown],
            ));
        }
        for (key, slot, max) in [
            ("opacity", i32::from(next.opacity), 100),
            ("size", i32::from(next.size), 40),
            ("margin", i32::from(next.margin), 40),
        ] {
            if incoming.contains_key(key) && slot > max {
                return Err(crate::i18n::tr_fmt(
                    "{} 要在 0..{}（相对输出尺寸的百分比）",
                    &[&key, &max],
                ));
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
                    other => {
                        return Err(crate::i18n::tr_fmt(
                            "未知输出模式 {}；可用 stills / video",
                            &[&other],
                        ));
                    }
                }
            }
            None => self.render_mode,
        };
        let format = match args.get("format") {
            Some(v) => {
                let raw = v.as_str().ok_or("format 必须是字符串")?;
                StillFormat::parse(raw).ok_or_else(|| {
                    crate::i18n::tr_fmt(
                        "未知容器 {}；可用 png8 / tiff16 / jpeg / webp / avif",
                        &[&raw],
                    )
                })?
            }
            None => self.render_format,
        };
        if mode == OutputMode::Video && !format.frames_ok() {
            return Err(crate::i18n::tr_fmt(
                "{} 只能出静帧；视频的中间帧要 PNG 或 TIFF（同一条命令里把 format 一起改掉）",
                &[&crate::i18n::tr(format.cn())],
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
            self.render_sharpen_for = OutputSharpen::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt(
                    "未知锐化用途 {}；可用 off / screen / glossy / matte",
                    &[&raw],
                )
            })?;
        }
        if let Some(v) = args.get("sharpen_strength") {
            let raw = v.as_str().ok_or("sharpen_strength 必须是字符串")?;
            self.render_sharpen_strength = SharpenStrength::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt("未知锐化强度 {}；可用 low / standard / high", &[&raw])
            })?;
        }
        if let Some(raw) = opt_str(args, "color_space")? {
            self.render_color_space = ColorSpace::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt("未知色域 {}；可用 srgb / p3 / rec2020", &[&raw])
            })?;
        }
        if let Some(raw) = opt_str(args, "dynamic")? {
            self.render_dynamic = OutputDynamic::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt("未知动态范围 {}；可用 sdr / hdr-pq", &[&raw])
            })?;
            // 命令行给 HDR 时同样补齐：色域自动 rec2020、容器自动 avif。
            self.apply_dynamic();
        }
        if let Some(v) = args.get("watermark") {
            self.patch_watermark(v)?;
        }
        match (opt_u32(args, "width")?, opt_u32(args, "height")?) {
            (Some(width), Some(height)) => {
                if width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1 {
                    return Err(crate::i18n::tr_fmt(
                        "width/height 必须是偶数且不小于 2：{}×{}",
                        &[&width, &height],
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
    ///
    /// The current frame's shooting parameters. Returns `null` when the whole block is
    /// unreadable (PNG, or a processed image with no EXIF); the panel and the agent both treat
    /// that as "this one has none" rather than showing a row of empty values.
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
            "measuring": self.measuring.is_some(),
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
            return Err(crate::i18n::tr_fmt("路径不存在：{}", &[&path.display()]));
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
            return Err(crate::i18n::tr_fmt("不是目录：{}", &[&path.display()]));
        }
        let index = self.lib_add(&path);
        let summary = self
            .lib_entries
            .get(index)
            .map(|f| aevum_core::library::summarize(f))
            .unwrap_or_default();
        self.status = crate::i18n::tr_fmt("图库加目录：{}", &[&path.display()]);
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
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        }
        let path = self.lib_roots[index as usize - 1].display().to_string();
        self.lib_remove(index as usize - 1);
        self.status = crate::i18n::tr_fmt("图库移除目录：{}", &[&path]);
        Ok(json!({"removed": path, "roots": self.lib_roots.len()}))
    }

    fn cmd_library_list(&mut self, args: &Value) -> Result<Value, String> {
        let scan = opt_u32(args, "index")?;
        if let Some(n) = scan {
            if n == 0 || n as usize > self.lib_roots.len() {
                return Err(crate::i18n::tr_fmt(
                    "index 超出范围：1..={}",
                    &[&self.lib_roots.len()],
                ));
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
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
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
            Some(n) if n as usize > self.lib_roots.len() => Err(crate::i18n::tr_fmt(
                "root 超出范围：1..={}",
                &[&self.lib_roots.len()],
            )),
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
            return Err(crate::i18n::tr_fmt("frame 超出范围：1..={}", &[&count]));
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
                            .ok_or_else(|| {
                                crate::i18n::tr_fmt("frames 里的帧号超出范围：1..={}", &[&count])
                            })
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
        self.status = crate::i18n::tr_fmt(
            "图库：{} 张改词（+{} / -{}），第 {} 帧现有 {} 个",
            &[
                &touched,
                &add.len(),
                &remove.len(),
                &(frames[0] + 1),
                &words.len(),
            ],
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
                .map_err(|e| crate::i18n::tr_fmt("写元数据失败：{}", &[&e]))?;
            if open_here {
                self.meta = meta.clone();
            }
            if self.lib_meta_root.as_deref() == Some(dir.as_path()) {
                self.lib_meta = meta.clone();
            }
            // 目录名先算成局部变量：`tr_fmt` 只认 `{}`，把整条链式调用塞进实参
            // 会让这一行长得读不动。
            let dir_name = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| dir.display().to_string());
            self.status = crate::i18n::tr_fmt("图库：{} 的说明与版权已写入工程", &[&dir_name]);
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
                return Err(crate::i18n::tr_fmt(
                    "认不出来的 op：{}（要 list / create / dissolve / auto）",
                    &[&other],
                ));
            }
        };
        if op != "list" {
            let dir_name = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| dir.display().to_string());
            self.status = crate::i18n::tr_fmt("图库：{} 现在有 {} 叠", &[&dir_name, &stacks.len()]);
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
            // 标记与色标名都是中文表里的取值，各自过一遍 tr 再拼进句子。
            let flag = match f.flag {
                1 => "选用",
                -1 => "排除",
                _ => "未标记",
            };
            self.status = crate::i18n::tr_fmt(
                "图库 {}：{} 星 · {} · 色标{}",
                &[
                    &f.name,
                    &f.rating,
                    &crate::i18n::tr(flag),
                    &crate::i18n::tr(LABEL_NAMES[f.label.min(4) as usize]),
                ],
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
        let flag = match patch.flag.unwrap_or(0) {
            1 => "选用",
            -1 => "排除",
            _ => "未标记",
        };
        self.status = crate::i18n::tr_fmt(
            "图库：{} 张已整理（{} 星 · {} · 色标{}）",
            &[
                &frames.len(),
                &patch.rating.unwrap_or(0).min(5),
                &crate::i18n::tr(flag),
                &crate::i18n::tr(LABEL_NAMES[patch.label.unwrap_or(0).min(4) as usize]),
            ],
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
                return Err(crate::i18n::tr_fmt(
                    "index 超出范围：1..={}",
                    &[&self.lib_cols.len()],
                ));
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
            .ok_or_else(|| {
                crate::i18n::tr_fmt("没有叫「{}」的集合：先 collection.create", &[&name])
            })
    }

    fn cmd_collection_create(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let index = self.lib_collection_create(name);
        let name = self.lib_cols[index].name.clone();
        self.status = crate::i18n::tr_fmt("新建集合「{}」", &[&name]);
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
            crate::i18n::tr_fmt("「{}」加入集合「{}」", &[&path.display(), &c.name])
        } else {
            crate::i18n::tr_fmt("「{}」已经在集合「{}」里", &[&path.display(), &c.name])
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
        self.status = crate::i18n::tr_fmt("删除集合「{}」", &[&name]);
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
            return Err(crate::i18n::tr_fmt("index 超出范围：1..={}", &[&n]));
        }
        self.set_frame(index as usize - 1);
        self.preview_dirty = true;
        let name = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.name.clone())
            .unwrap_or_default();
        self.status = crate::i18n::tr_fmt("已跳到第 {} 帧 · {}", &[&(self.selected + 1), &name]);
        Ok(json!({
            "selected": self.selected + 1,
            "frames": n,
            "name": name,
        }))
    }

    fn cmd_ui_module(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let module = Module::from_name(name).ok_or_else(|| {
            crate::i18n::tr_fmt(
                "未知模块 {}；可用 序列/去闪/显影/构图/成片/声音/合成 或 0-6",
                &[&name],
            )
        })?;
        self.set_module(module);
        self.status = crate::i18n::tr_fmt("模块：{}", &[&crate::i18n::tr(module.cn())]);
        Ok(json!({"module": module.cn(), "index": module.index()}))
    }

    fn cmd_ui_dock(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let tab = DockTab::from_name(name).ok_or_else(|| {
            crate::i18n::tr_fmt("未知面板 {}；可用 调整/信息/关键帧/AI 或 0-3", &[&name])
        })?;
        self.dock_tab = tab;
        self.show_dock = true;
        self.status = crate::i18n::tr_fmt("面板：{}", &[&crate::i18n::tr(tab.cn())]);
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
            other => Err(crate::i18n::tr_fmt(
                "未知 op {}；可用 list / save / apply / delete / rename",
                &[&other],
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
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
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
                        .ok_or_else(|| {
                            crate::i18n::tr_fmt(
                                "帧号越界：{}（序列共 {} 帧）",
                                &[&aevum_core::errmsg::clip_echo(&v.to_string()), &n],
                            )
                        })
                })
                .collect::<Result<_, _>>()?,
            None => {
                let around = opt_u32(args, "around")?.unwrap_or(1) as usize;
                let center = opt_u32(args, "center")?
                    .map(|v| v as usize)
                    .unwrap_or(self.selected.min(n - 1));
                if center >= n {
                    return Err(crate::i18n::tr_fmt(
                        "center 越界：{}（序列共 {} 帧）",
                        &[&center, &n],
                    ));
                }
                (center.saturating_sub(around)..=center.saturating_add(around).min(n - 1)).collect()
            }
        };
        let kind = MergeKind::Hdr;
        if indices.len() < 2 {
            return Err("HDR 合并至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "HDR 合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑关键几帧",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let root = seq.root.clone();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;

        // HDR / 全景不做合成前自动对齐：HDR 是同场景多曝光，全景几何交给拼接。
        //
        // HDR / panorama never run pre-fusion alignment: HDR is one scene at several
        // exposures, and panorama geometry belongs to the stitcher.
        let job = self.build_merge_job(kind, indices, &out_dir, MergeSpec::Plain, false)?;
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
        spec: MergeSpec,
        align: bool,
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
            // 全景靠特征点自己求单应，几何全交给拼接，不套对齐位移；
            // 其余三种都是同机位叠帧，对齐位移与统一裁切必须套上，否则星点会拖成双线。
            MergeKind::Pano => indices.iter().map(|_| (None, [0.0, 0.0])).collect(),
            // 开了合成前自动对齐的焦点合成走另一套几何（拍板 ⑥「不叠加」）：手动
            // 序列对齐的位移一律不套，只保留它的裁切框；位移完全由本次估计给出。
            // 估计就在这个已裁切的坐标系里做。
            //
            // A focus merge with pre-fusion auto alignment uses a different geometry
            // (ruling 6, "no stacking"): manual sequence-alignment offsets are never
            // applied, only its crop rectangle survives; the offsets come entirely from
            // this run's estimate, which therefore works in the already-cropped frame.
            MergeKind::Focus if align => indices
                .iter()
                .map(|i| (self.alignment_at(*i).0, [0.0, 0.0]))
                .collect(),
            _ => indices.iter().map(|i| self.alignment_at(*i)).collect(),
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
            spec,
            align,
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
            MergeKind::Stack => {
                self.stack_last = None;
                self.stack_error = None;
            }
            MergeKind::TimeBlend => {
                self.tb_last = None;
                self.tb_error = None;
            }
            MergeKind::Focus => {
                self.focus_last = None;
                self.focus_error = None;
            }
        }
        self.merge_job += 1;
        let run = self.merge_job;
        // 新任务新开一个取消旗：上一轮的旗（可能已经立着）与这一轮无关，绝不能拿它
        // 当共同开关——旧线程还在跑时把它清零，等于让旧任务继续写盘。
        //
        // A fresh cancel flag per run: the previous one (possibly already raised) must not be
        // shared, or clearing it while the old thread is still alive revives that run.
        self.cancel_merge = Arc::new(AtomicBool::new(false));
        self.merge_done.store(0, Ordering::Relaxed);
        // 进度 = 读好的帧数 + 最后那一段（融合或拼接再写盘）。
        self.merge_total.store(frames + 1, Ordering::Relaxed);
        self.status = crate::i18n::tr_fmt("正在{}…", &[&crate::i18n::tr(kind.cn())]);

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
                job: run,
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
    ///
    /// Request cancellation of the merge in progress; the core algorithm is one block, so
    /// cancellation only takes effect between two frames.
    pub fn stop_merge(&mut self) {
        if self.merge_busy {
            self.cancel_merge.store(true, Ordering::SeqCst);
            if let Some(kind) = self.merge_kind {
                self.status =
                    crate::i18n::tr_fmt("正在取消{}…（帧间生效）", &[&crate::i18n::tr(kind.cn())]);
            }
        }
    }

    /// 面板画进度用的三元组：种类 + 已完成 + 总数。没在跑就 `None`。
    ///
    /// The triple the panel draws progress from: kind + done + total. `None` when nothing is
    /// running.
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
        self.status = crate::i18n::t!("正在体检…").to_owned();

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
    ///
    /// Request cancellation of the running health check; `stat` stops per batch, so it really
    /// exits only after the last batch finishes.
    pub fn stop_qa(&mut self) {
        if self.qa_busy {
            self.cancel_qa.store(true, Ordering::SeqCst);
            self.status = crate::i18n::t!("正在取消体检…（批间生效）").to_owned();
        }
    }

    /// 面板画进度用的二元组：已完成 / 总数。没在跑就 `None`。
    ///
    /// The pair the panel draws progress from: done / total. `None` when nothing is
    /// running.
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
    ///
    /// The panel's "start health check": the same path as `sequence.check`, using the
    /// panel's current threshold.
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
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
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
                        .ok_or_else(|| {
                            crate::i18n::tr_fmt(
                                "帧号越界：{}（序列共 {} 帧）",
                                &[&aevum_core::errmsg::clip_echo(&v.to_string()), &n],
                            )
                        })
                })
                .collect::<Result<_, _>>()?,
            None => {
                let around = opt_u32(args, "around")?.unwrap_or(1) as usize;
                let start = opt_u32(args, "start")?
                    .map(|v| v as usize)
                    .unwrap_or(self.selected.min(n - 1));
                if start >= n {
                    return Err(crate::i18n::tr_fmt(
                        "start 越界：{}（序列共 {} 帧）",
                        &[&start, &n],
                    ));
                }
                (start..=start.saturating_add(around).min(n - 1)).collect()
            }
        };
        let kind = MergeKind::Pano;
        if indices.len() < 2 {
            return Err("全景合并至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "全景合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑相邻几张",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let root = seq.root.clone();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;

        // HDR / 全景不做合成前自动对齐：HDR 是同场景多曝光，全景几何交给拼接。
        //
        // HDR / panorama never run pre-fusion alignment: HDR is one scene at several
        // exposures, and panorama geometry belongs to the stitcher.
        let job = self.build_merge_job(kind, indices, &out_dir, MergeSpec::Plain, false)?;
        Ok(self.start_merge(ctx, job))
    }

    /// 把 `frames`（帧号数组）或锚点 ± `around` 解析成一串合法帧号。
    ///
    /// Resolve either a `frames` array or an anchor plus an `around` radius into valid indices.
    fn pick_indices(
        &self,
        args: &Value,
        n: usize,
        anchor: IndicesAnchor,
        default_around: u32,
    ) -> Result<Vec<usize>, String> {
        if let Some(list) = args.get("frames") {
            return list
                .as_array()
                .ok_or_else(|| "frames 要是帧号数组，例如 [0,1,2]".to_owned())?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|i| ((i as usize) < n).then_some(i as usize))
                        .ok_or_else(|| {
                            crate::i18n::tr_fmt(
                                "帧号越界：{}（序列共 {} 帧）",
                                &[&aevum_core::errmsg::clip_echo(&v.to_string()), &n],
                            )
                        })
                })
                .collect();
        }
        let around = opt_u32(args, "around")?.unwrap_or(default_around) as usize;
        let (anchor_value, name) = match anchor {
            IndicesAnchor::Center => (opt_u32(args, "center")?, "center"),
            IndicesAnchor::Start => (opt_u32(args, "start")?, "start"),
        };
        let at = anchor_value
            .map(|v| v as usize)
            .unwrap_or(self.selected.min(n - 1));
        if at >= n {
            return Err(crate::i18n::tr_fmt(
                "{} 越界：{}（序列共 {} 帧）",
                &[&name, &at, &n],
            ));
        }
        let from = match anchor {
            IndicesAnchor::Center => at.saturating_sub(around),
            IndicesAnchor::Start => at,
        };
        Ok((from..=at.saturating_add(around).min(n - 1)).collect())
    }

    /// `cull.evaluate`：按四条判据逐帧裁决，可选直接把判废的帧标成「排除」。
    ///
    /// `cull.evaluate`: judge every frame with the four rules, optionally marking the rejected
    /// ones as excluded right away.
    fn cmd_cull_evaluate(&mut self, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        // 判据吃的是导入那趟已经算好的逐帧测光，不再解图：这条命令应当立刻返回。
        //
        // The rules live on the per-frame measurements taken during import, so nothing is
        // decoded here and the call returns at once.
        let means: Vec<Option<f64>> = self.metrics.iter().map(|m| m.map(|x| x.mean)).collect();
        let sharp: Vec<Option<f64>> = self.metrics.iter().map(|m| m.map(|x| x.sharp)).collect();
        let mut rule = aevum_core::cull::CullRule {
            ev_low: self.cull_ev_low,
            ev_high: self.cull_ev_high,
            jump_ev: self.cull_jump,
            sharp_ratio: self.cull_sharp,
        };
        if args
            .get("suggest")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let (lo, hi) = aevum_core::cull::suggest_band(&means);
            rule.ev_low = lo;
            rule.ev_high = hi;
        }
        if let Some(v) = opt_f64(args, "ev_low")? {
            rule.ev_low = v;
        }
        if let Some(v) = opt_f64(args, "ev_high")? {
            rule.ev_high = v;
        }
        if let Some(v) = opt_f64(args, "jump_ev")? {
            rule.jump_ev = v;
        }
        if let Some(v) = opt_f64(args, "sharp_ratio")? {
            rule.sharp_ratio = v.clamp(0.0, 1.0);
        }
        if rule.ev_low > rule.ev_high {
            return Err(crate::i18n::tr_fmt(
                "曝光下限 {} 不该高于上限 {}",
                &[&rule.ev_low, &rule.ev_high],
            ));
        }
        let verdict = aevum_core::cull::evaluate(&means, &sharp, &rule);
        let frames: Vec<Value> = verdict
            .frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                json!({
                    "index": i,
                    "reject": f.reject,
                    "reason": f.reason.map(|r| r.key()),
                })
            })
            .collect();
        let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(false);
        let rejected = verdict.rejected_indices();
        if apply {
            self.flag_many(&rejected, -1);
        }
        self.cull_ev_low = rule.ev_low;
        self.cull_ev_high = rule.ev_high;
        self.cull_jump = rule.jump_ev;
        self.cull_sharp = rule.sharp_ratio;
        self.cull_note = Some(if rejected.is_empty() {
            crate::i18n::tr("这一遍没有帧越界").to_owned()
        } else {
            crate::i18n::tr_fmt("{} 帧会被判废", &[&rejected.len()])
        });
        self.status = self.cull_note.clone().unwrap_or_default();
        Ok(json!({
            "band": [rule.ev_low, rule.ev_high],
            "rule": {
                "ev_low": rule.ev_low,
                "ev_high": rule.ev_high,
                "jump_ev": rule.jump_ev,
                "sharp_ratio": rule.sharp_ratio,
            },
            "rejected": rejected.len(),
            "applied": if apply { rejected.len() } else { 0 },
            "indices": rejected,
            "jumps": verdict
                .jumps
                .iter()
                .map(|(i, step)| json!({"index": i, "step_ev": step}))
                .collect::<Vec<Value>>(),
            "frames": frames,
        }))
    }

    /// `cull.extract`：把没被拒收的帧复制成一条新序列（剔除成子序列）。
    ///
    /// `cull.extract`: copy every frame that was not rejected into a new sequence.
    fn cmd_cull_extract(&mut self, args: &Value) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        if seq.is_empty() {
            return Err("序列里没有帧".to_owned());
        }
        let target = args
            .get("target")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| "target 要是目标目录的绝对路径".to_owned())?;
        let target = std::path::PathBuf::from(target);
        let root = seq.root.clone();
        let dry_run = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !dry_run && !confirm(args) {
            return Err(
                "cull.extract 是批量写操作，需参数 confirm: true（只看体积用 dry_run: true）"
                    .to_owned(),
            );
        }
        // 路径安全先行：源内部、盘根、非空目录都在写任何东西之前拒绝。
        //
        // Path safety first: inside the source, a drive root and a non-empty folder are all
        // refused before anything is written.
        aevum_core::cull::check_target(&root, &target)
            .map_err(|e| crate::i18n::tr(e.cn()).to_owned())?;
        let paths = seq.paths();
        let kept: Vec<PathBuf> = (0..paths.len())
            .filter(|i| self.flag_at(*i) != -1)
            .map(|i| paths[i].clone())
            .collect();
        if kept.is_empty() {
            return Err("每一帧都被判废了：没有可保留的帧".to_owned());
        }
        if dry_run {
            let plan = aevum_core::cull::plan_extract(&kept);
            return Ok(json!({
                "dry_run": true,
                "frames": plan.kept.len(),
                "bytes": plan.bytes,
                "excluded": paths.len() - plan.kept.len(),
                "target": target.display().to_string(),
            }));
        }
        let bytes = aevum_core::cull::extract(&root, &kept, &target).map_err(|e| match e {
            aevum_core::cull::ExtractError::Target(t) => crate::i18n::tr(t.cn()).to_owned(),
            aevum_core::cull::ExtractError::Io(e) => crate::i18n::tr_fmt("复制失败：{}", &[&e]),
        })?;
        let note = crate::i18n::tr_fmt(
            "已导出 {} 帧到 {}",
            &[&kept.len(), &target.display().to_string()],
        );
        history::note("cull.extract", note.clone());
        self.status = note.clone();
        // 开发参数不跟过去（它们属于源序列的工程）；测光缓存已按 mtime 带走，
        // 新序列打开时直接命中，不用重测。
        //
        // Develop settings do not follow (they belong to the source project); the metering
        // cache was carried with mtimes, so the new sequence opens without a re-measure.
        Ok(json!({
            "copied": kept.len(),
            "excluded": paths.len() - kept.len(),
            "bytes": bytes,
            "target": target.display().to_string(),
            "note": note,
        }))
    }

    /// `stack.merge`：把一段同机位帧按统计量合成一张（星轨 / 降噪 / 去卫星 / σ 截尾）。
    ///
    /// `stack.merge`: combine a fixed-camera run of frames into one still using a statistical
    /// mode (star trails / noise reduction / satellite removal / sigma clipping).
    fn cmd_stack_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
            ));
        }
        if !confirm(args) {
            return Err("stack.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let kind = MergeKind::Stack;
        let indices = self.pick_indices(args, n, IndicesAnchor::Center, self.stack_around)?;
        if indices.len() < 2 {
            return Err("堆栈至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "堆栈一次最多 {} 帧，实得 {} 帧：把 around 调小，或先分段再合",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let mode = match opt_str(args, "mode")? {
            Some(raw) => aevum_core::stack::Mode::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt(
                    "未知堆栈模式 {}；可用 lighten / average / median / sigma-clip",
                    &[&raw],
                )
            })?,
            None => self.stack_mode,
        };
        let clip_sigma = opt_f64(args, "clip_sigma")?
            .map(|v| v as f32)
            .unwrap_or(self.stack_sigma);
        if !clip_sigma.is_finite() || clip_sigma <= 0.0 {
            return Err("clip_sigma 要大于 0（常用 2–4：3.0 只剔真离群）".to_owned());
        }
        let fill_gaps = args
            .get("fill_gaps")
            .and_then(Value::as_bool)
            .unwrap_or(self.stack_fill);
        let root = self
            .seq
            .as_ref()
            .map(|s| s.root.clone())
            .unwrap_or_default();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;
        let spec = MergeSpec::Stack(aevum_core::stack::Options {
            mode,
            clip_sigma,
            fill_gaps,
        });
        // 堆栈不做合成前自动对齐：同机位叠帧，几何由序列对齐负责。
        //
        // Stacking never runs pre-fusion alignment: fixed-camera frames, geometry belongs
        // to sequence alignment.
        let job = self.build_merge_job(kind, indices, &out_dir, spec, false)?;
        Ok(self.start_merge(ctx, job))
    }

    /// 时间合成预览：面板每帧调用；参数或帧选择变了就在后台重算一张半分辨率小图。
    ///
    /// 派发要同时满足：序列开着、至少两帧、没在合并、没有同参数任务在飞。回包拿
    /// 参数指纹（`tb_preview_key`）对账——迟到的旧参数回包只收解码缓存、不上屏。
    ///
    /// The time-blend preview tick, called by the panel every frame; a changed parameter
    /// or frame selection re-runs a half-resolution composite in the background. Dispatch
    /// requires an open sequence with at least two frames, no running merge and no
    /// in-flight job for the same parameters. Replies reconcile against the parameter
    /// fingerprint (`tb_preview_key`): stale replies only refresh the decode cache.
    pub fn timeblend_preview_tick(&mut self, ctx: &Context) {
        if self.merge_busy || self.tb_preview_inflight {
            return;
        }
        let Some(seq) = self.seq.as_ref() else {
            return;
        };
        let n = seq.len();
        if n < 2 {
            return;
        }
        // 帧选择与 timeblend.merge 的 Start 锚点一致：当前帧起 tb_around 帧。
        let at = self.selected.min(n - 1);
        let indices: Vec<usize> =
            (at..=at.saturating_add(self.tb_around as usize).min(n - 1)).collect();
        if indices.len() < 2 {
            return;
        }
        let layout = self.tb_layout;
        let mut bands = self.tb_bands as usize;
        if layout == aevum_core::timeblend::Layout::Bands {
            bands = bands.clamp(1, indices.len());
        }
        let spec = aevum_core::timeblend::Spec {
            layout,
            bands,
            feather: self.tb_feather.clamp(0.0, 0.5),
            horizon: self.tb_horizon.clamp(0.0, 1.0),
            horizon_auto: self.tb_horizon_auto,
            deghost: self.tb_deghost,
            match_exposure: self.tb_match,
        };
        // 指纹：帧选择（序列根 + 帧号）先散一遍归解码缓存，合成参数叠在其上做对账。
        let root = seq.root.display().to_string();
        let mut hf = fnv1a(0xcbf2_9ce4_8422_2325, root.as_bytes());
        for i in &indices {
            hf = fnv1a(hf, &(*i as u64).to_le_bytes());
        }
        let key_frames = hf;
        let mut h = key_frames;
        h = fnv1a(h, layout.key().as_bytes());
        h = fnv1a(h, &(bands as u64).to_le_bytes());
        h = fnv1a(h, &spec.feather.to_bits().to_le_bytes());
        h = fnv1a(h, &spec.horizon.to_bits().to_le_bytes());
        h = fnv1a(h, &spec.deghost.to_bits().to_le_bytes());
        h = fnv1a(
            h,
            &[u8::from(spec.horizon_auto), u8::from(spec.match_exposure)],
        );
        let key = h;
        let paths = seq.paths();
        let srcs: Vec<PathBuf> = indices.iter().map(|i| paths[*i].clone()).collect();
        let framing: Vec<(Option<CropRect>, [f64; 2])> =
            indices.iter().map(|i| self.alignment_at(*i)).collect();
        let token = self.reload_token;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let cached = self.tb_preview_data.clone();
        if self.tb_preview_key == key {
            return; // 已上屏（或已失败）的同一组参数：不重发，失败也不打转
        }
        self.tb_preview_key = key;
        self.tb_preview_inflight = true;
        self.tb_preview_error = None;
        thread::spawn(move || {
            match run_timeblend_preview(cached, srcs, indices, framing, spec, key_frames) {
                Ok((data, lin, w, h)) => {
                    let _ = tx.send(Msg::TimeblendPreview {
                        token,
                        key,
                        image: Box::new(tb_preview_image(&lin, w, h)),
                        frames: Some(data),
                    });
                }
                Err(e) => {
                    let _ = tx.send(Msg::TimeblendPreviewFail {
                        token,
                        key,
                        error: e,
                    });
                }
            }
            ctx2.request_repaint();
        });
    }

    /// `timeblend.merge`：按带或按地平线把不同时刻的帧合成为一张。
    ///
    /// `timeblend.merge`: composite frames from different moments, either as horizontal bands or
    /// split at the horizon.
    fn cmd_timeblend_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
            ));
        }
        if !confirm(args) {
            return Err("timeblend.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let kind = MergeKind::TimeBlend;
        let indices = self.pick_indices(args, n, IndicesAnchor::Start, self.tb_around)?;
        if indices.len() < 2 {
            return Err(
                "时间合成至少要 2 帧：给 frames 数组，或把 around 调大（帧号升序 = 时间顺序）"
                    .to_owned(),
            );
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "时间合成一次最多 {} 帧，实得 {} 帧：把 around 调小",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let layout = match opt_str(args, "layout")? {
            Some(raw) => aevum_core::timeblend::Layout::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt("未知布局 {}；可用 sky-ground / bands", &[&raw])
            })?,
            None => self.tb_layout,
        };
        let bands = opt_u32(args, "bands")?.unwrap_or(self.tb_bands) as usize;
        // bands 只对时间切片有意义：天地合成不用它，不该被面板的默认值卡住。
        //
        // bands only matters for the time slice: sky-ground ignores it and must not be blocked
        // by the panel's default.
        if layout == aevum_core::timeblend::Layout::Bands && (bands == 0 || bands > indices.len()) {
            return Err(crate::i18n::tr_fmt(
                "bands 要在 1..={} 之间（等于帧数时一带一帧）",
                &[&indices.len()],
            ));
        }
        let feather = opt_f64(args, "feather")?
            .map(|v| v as f32)
            .unwrap_or(self.tb_feather)
            .clamp(0.0, 0.5);
        let horizon_auto = args
            .get("horizon_auto")
            .and_then(Value::as_bool)
            .unwrap_or(self.tb_horizon_auto);
        let horizon = opt_f64(args, "horizon")?
            .map(|v| v as f32)
            .unwrap_or(self.tb_horizon)
            .clamp(0.0, 1.0);
        let deghost = opt_f64(args, "deghost")?
            .map(|v| v as f32)
            .unwrap_or(self.tb_deghost);
        if !deghost.is_finite() || deghost < 0.0 || deghost > 4.0 {
            return Err("deghost 要在 0..=4 之间（EV；0 = 关闭去鬼影）".to_owned());
        }
        let match_exposure = args
            .get("match_exposure")
            .and_then(Value::as_bool)
            .unwrap_or(self.tb_match);
        let root = self
            .seq
            .as_ref()
            .map(|s| s.root.clone())
            .unwrap_or_default();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;
        let spec = MergeSpec::TimeBlend(aevum_core::timeblend::Spec {
            layout,
            bands,
            feather,
            horizon,
            horizon_auto,
            deghost,
            match_exposure,
        });
        // 时间合成不做合成前自动对齐：天地/星轨的位移是内容，不能被"对齐掉"。
        //
        // Time blending never runs pre-fusion alignment: motion is the content here, and
        // aligning it away would destroy the very thing the user is composing.
        let job = self.build_merge_job(kind, indices, &out_dir, spec, false)?;
        Ok(self.start_merge(ctx, job))
    }

    /// `focus.merge`：把一组不同对焦距离的帧融成一张全景深。
    ///
    /// `focus.merge`: fuse a bracket taken at different focus distances into one all-in-focus
    /// image.
    fn cmd_focus_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
            ));
        }
        if !confirm(args) {
            return Err("focus.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let kind = MergeKind::Focus;
        let indices = self.pick_indices(args, n, IndicesAnchor::Start, self.focus_around)?;
        if indices.len() < 2 {
            return Err(
                "焦点合成至少要 2 帧：给 frames 数组，或把 around 调大（从起始帧往后铺）"
                    .to_owned(),
            );
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "焦点合成一次最多 {} 帧，实得 {} 帧：把 around 调小或分段合成",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let method = match opt_str(args, "method")? {
            Some(raw) => aevum_core::focus::Method::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt(
                    "未知融合方式 {}；可用 depthmap / weighted / pyramid",
                    &[&raw],
                )
            })?,
            None => self.focus_method,
        };
        let radius = opt_u32(args, "radius")?.unwrap_or(self.focus_radius) as usize;
        if radius > 32 {
            return Err("radius 要在 0..=32 之间（px；0 = 逐像素原始响应）".to_owned());
        }
        let threshold = opt_f64(args, "threshold")?
            .map(|v| v as f32)
            .unwrap_or(self.focus_threshold);
        if !threshold.is_finite() || !(0.0..=100.0).contains(&threshold) {
            return Err("threshold 要在 0..=100 之间（百分位；0 = 关闭无细节兜底）".to_owned());
        }
        let smooth_z = opt_u32(args, "smooth_z")?.unwrap_or(self.focus_smooth_z) as usize;
        if smooth_z > 16 {
            return Err("smooth_z 要在 0..=16 之间（px）".to_owned());
        }
        let feather = opt_f64(args, "feather")?
            .map(|v| v as f32)
            .unwrap_or(self.focus_feather);
        if !feather.is_finite() || !(0.0..=64.0).contains(&feather) {
            return Err("feather 要在 0..=64 之间（px；小于 0.5 视为硬切）".to_owned());
        }
        // 合成前自动对齐：缺省关；给了但不是布尔值就当场报错，别把 "true" 字符串
        // 静默当成假（那会让脚本以为开了对齐，实际没开）。
        //
        // Pre-fusion auto alignment: off by default; a non-boolean value errors out
        // immediately instead of being read as false, which would let a script believe
        // alignment ran when it did not.
        let align = match args.get("align") {
            None | Some(Value::Null) => false,
            Some(value) => value.as_bool().ok_or_else(|| {
                crate::i18n::tr_fmt("{} 要布尔值（true / false）", &[&"align"]).to_owned()
            })?,
        };
        let root = self
            .seq
            .as_ref()
            .map(|s| s.root.clone())
            .unwrap_or_default();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;
        let spec = MergeSpec::Focus(aevum_core::focus::Options {
            method,
            radius,
            threshold,
            smooth_z,
            feather,
            // 界面不暴露 levels，0 = auto（见 focus::Options）。
            // The UI does not expose levels; 0 = auto (see focus::Options).
            levels: 0,
            // 界面同样不暴露测度缓存上界，用默认 8（见 focus::Options::measure_cache）。
            // The UI does not expose the measure-cache bound either; default 8
            // (see focus::Options::measure_cache).
            measure_cache: 8,
        });
        let job = self.build_merge_job(kind, indices, &out_dir, spec, align)?;
        let mut started = self.start_merge(ctx, job);
        // 这次到底套没套对齐位移，调用方（面板与脚本）都该直接读到，而不是回头去查
        // 对齐报告再猜。
        //
        // Whether this run applied the alignment offsets belongs in the reply, so both
        // the panel and a script can read it instead of consulting the align report.
        let aligned = self.align_report.is_some();
        if let Some(object) = started.as_object_mut() {
            object.insert("aligned".to_owned(), serde_json::Value::Bool(aligned));
            // 自动对齐请求也回一句：`aligned` 说的是**手动**序列对齐，两者不是一个东西。
            //
            // Also echo the auto-alignment request: `aligned` talks about **manual**
            // sequence alignment, which is a different thing.
            object.insert("align".to_owned(), serde_json::Value::Bool(align));
        }
        Ok(started)
    }

    /// 深度图预览：按下「预览深度图」时前台算一张降采样的归属图。
    ///
    /// 取帧与滑杆参数和 `focus.merge` 同一口径（当前帧起向后 `focus_around` 帧）；
    /// 帧先按长边 [`FOCUS_PREVIEW_LONG_EDGE`] 降采样再进 `focus::depth_map`，预览图
    /// 天然就小，前台同步算也不卡帧——全分辨率是合成任务自己的事，走后台线程。
    /// 不写任何文件：成片仍由「合成」按钮走命令通道。
    ///
    /// Depth-map preview: pressing "preview depth map" computes a downsampled ownership
    /// map on the spot. The frame selection and slider parameters follow the same
    /// convention as `focus.merge` (from the current frame, `focus_around` frames
    /// forward); frames are downsampled to the [`FOCUS_PREVIEW_LONG_EDGE`] long edge
    /// before entering `focus::depth_map`, so the preview bitmap is small by
    /// construction and the synchronous foreground computation stays responsive -- full
    /// resolution is the merge job's business, on the background thread. No file is
    /// written: the final still still goes through the "merge" button and the command
    /// channel.
    pub fn focus_depth_preview(&mut self, ctx: &Context) {
        if self.merge_busy {
            return;
        }
        let n = match self.seq.as_ref() {
            Some(seq) => seq.len(),
            None => return,
        };
        if n < 2 {
            return;
        }
        let indices = match self.pick_indices(
            &serde_json::json!({}),
            n,
            IndicesAnchor::Start,
            self.focus_around,
        ) {
            Ok(v) => v,
            Err(_) => return,
        };
        if indices.len() < 2 {
            self.focus_depth_tex = None;
            self.focus_depth_note =
                Some(crate::i18n::t!("预览至少要 2 帧：把取帧范围调大").to_owned());
            return;
        }
        let started = std::time::Instant::now();
        // 与 run_merge_job 同一条几何路：有对齐结果就套统一裁切与逐帧位移，
        // 否则预览选区和成片对不上，用户会以为深度图选错了帧。
        //
        // Same geometry path as run_merge_job: apply the alignment crop and per-frame
        // offsets when available, otherwise the preview selection disagrees with the
        // final still and reads as "the depth map picked the wrong frame".
        let paths = self.seq.as_ref().expect("seq checked above").paths();
        let mut buffers: Vec<Vec<f32>> = Vec::with_capacity(indices.len());
        let mut dims: Option<(usize, usize)> = None;
        for i in &indices {
            let (crop, off) = self.alignment_at(*i);
            let (rgb, w, h) = match export::load_rgb_f32(&paths[*i]) {
                Ok(v) => v,
                Err(e) => {
                    self.focus_depth_tex = None;
                    self.focus_depth_note = Some(crate::i18n::tr_fmt("预览读帧失败：{}", &[&e]));
                    return;
                }
            };
            let rect = crop.map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize));
            let framing = Framing {
                angle_deg: 0.0,
                off,
            };
            let (buf, got) = if rect.is_none() && framing.is_identity() {
                (rgb, (w as usize, h as usize))
            } else {
                let mut out = Vec::new();
                let (cw, ch) =
                    warp::transform_into(&rgb, w as usize, h as usize, framing, rect, &mut out);
                (out, (cw, ch))
            };
            match dims {
                None => dims = Some(got),
                Some(prev) if prev != got => {
                    self.focus_depth_tex = None;
                    self.focus_depth_note = Some(crate::i18n::tr_fmt(
                        "预览跳过：第 {} 帧与第 {} 帧尺寸不一致（{}×{} vs {}×{}）",
                        &[&indices[0], i, &prev.0, &prev.1, &got.0, &got.1],
                    ));
                    return;
                }
                Some(_) => {}
            }
            buffers.push(buf);
        }
        let (w, h) = dims.unwrap_or((0, 0));
        // 同尺寸帧降一次就够了：所有帧同一 (w, h)，降完仍然同尺寸。
        //
        // Downsampling once suffices: every frame shares one (w, h) and stays shared
        // after the downscale.
        let (w, h) = {
            let first = downscale_long_edge(
                std::mem::take(&mut buffers[0]),
                w,
                h,
                FOCUS_PREVIEW_LONG_EDGE,
            );
            let (nw, nh) = (first.1, first.2);
            buffers[0] = first.0;
            for b in buffers.iter_mut().skip(1) {
                let (small, sw, sh) =
                    downscale_long_edge(std::mem::take(b), w, h, FOCUS_PREVIEW_LONG_EDGE);
                debug_assert_eq!((sw, sh), (nw, nh), "同尺寸帧降采样后尺寸不同");
                *b = small;
            }
            (nw, nh)
        };
        let opts = aevum_core::focus::Options {
            method: self.focus_method,
            radius: self.focus_radius as usize,
            threshold: self.focus_threshold,
            smooth_z: self.focus_smooth_z as usize,
            feather: self.focus_feather,
            // 深度图预览不需要塔，0 = auto。
            // The depth-map preview needs no tower; 0 = auto.
            levels: 0,
            // 深度图不读金字塔缓存，用默认 8（见 focus::Options::measure_cache）。
            // The depth map never reads the pyramid cache; default 8
            // (see focus::Options::measure_cache).
            measure_cache: 8,
        };
        let refs: Vec<&[f32]> = buffers.iter().map(|b| b.as_slice()).collect();
        let owner = match aevum_core::focus::depth_map(&refs, w, h, &opts) {
            Ok(v) => v,
            Err(e) => {
                self.focus_depth_tex = None;
                self.focus_depth_note = Some(crate::i18n::tr_fmt(
                    "预览失败：{}",
                    &[&crate::i18n::tr(aevum_core::focus::error_text(e))],
                ));
                return;
            }
        };
        let count = refs.len();
        // 参考帧口径与 focus.rs 一致：中位帧（count / 2）。
        //
        // The reference frame follows focus.rs: the middle frame (count / 2).
        let reference = (count / 2) as u16;
        let mut pixels: Vec<u8> = Vec::with_capacity(owner.len() * 3);
        for &o in &owner {
            let [r, g, b] = depth_preview_color(o, count, reference);
            pixels.extend_from_slice(&[r, g, b]);
        }
        let img = ColorImage::from_rgb([w, h], &pixels);
        // 纹理按名字复用：同参数只重传像素，不重新在 GPU 上分配（与时间合成预览同路）。
        match &mut self.focus_depth_tex {
            Some(tex) => tex.set(img, TextureOptions::LINEAR),
            None => {
                self.focus_depth_tex =
                    Some(ctx.load_texture("focus.depth", img, TextureOptions::LINEAR));
            }
        }
        // 耗时写进说明行：预览是前台同步算的，用户有权知道自己等了多久。
        let ms = started.elapsed().as_millis().to_string();
        self.focus_depth_note = Some(crate::i18n::tr_fmt(
            "预览 {}×{} · {} 帧 · {} ms · 第 {} 帧为参考帧",
            &[&w, &h, &count, &ms, &indices[reference as usize]],
        ));
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
        let mut current = serde_json::to_value(&self.params)
            .map_err(|e| crate::i18n::tr_fmt("序列化参数失败：{}", &[&e]))?;
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
            return Err(crate::i18n::tr_fmt(
                "未知调色字段：{}",
                &[&unknown.join("、")],
            ));
        }
        let params: GradeParams = serde_json::from_value(current)
            .map_err(|e| crate::i18n::tr_fmt("参数格式不对：{}", &[&e]))?;
        // 换了外观路径就当场验一遍：渲染热路径读不出来时只会按"没挂"处理，
        // 报错必须在这里发生，否则界面表现为"改了没反应"。
        if object.contains_key("look_cube") && !params.look_cube.is_empty() {
            cube::refresh(&params.look_cube)
                .map_err(|e| crate::i18n::tr_fmt("外观 .cube 读不出来：{}", &[&e]))?;
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
            return Err(crate::i18n::tr_fmt(
                "未知预设 {}；可用：{}",
                &[&name, &PRESET_NAMES.join("、")],
            ));
        }
        self.apply_preset(name);
        self.status = crate::i18n::tr_fmt("已套用预设 {}", &[&name]);
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
            return Err(crate::i18n::tr_fmt(
                "未知蒙版形状 {}；可用 linear / radial / brush",
                &[&kind],
            ));
        }
        let index = self.add_mask(&kind);
        let shape = match kind.as_str() {
            "radial" => "径向",
            "brush" => "画笔",
            _ => "线性",
        };
        self.status = crate::i18n::tr_fmt(
            "已添加{}蒙版；拖动画布手柄调整范围",
            &[&crate::i18n::tr(shape)],
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
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.params.masks.len()],
            ));
        }
        self.remove_mask(index as usize - 1);
        self.status = crate::i18n::tr_fmt("已删除第 {} 张蒙版", &[&index]);
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
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.params.masks.len()],
            ));
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
            return Err(crate::i18n::tr_fmt(
                "component 超出范围：0..{}",
                &[&(shapes - 1)],
            ));
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
                    let number = value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| {
                        crate::i18n::tr_fmt("局部参数 {} 必须是有限数值或 null", &[&key])
                    })?;
                    target.insert(key.clone(), json!(number));
                }
            }
            if !unknown.is_empty() {
                unknown.sort();
                return Err(crate::i18n::tr_fmt(
                    "未知局部参数：{}",
                    &[&unknown.join("、")],
                ));
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
        self.status = crate::i18n::tr_fmt("已更新第 {} 张蒙版（来自命令通道）", &[&index]);
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
            return Err(crate::i18n::tr_fmt("index 超出范围：1..={}", &[&masks]));
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
                    return Err(crate::i18n::tr_fmt(
                        "未知蒙版形状 {}；可用 linear / radial / brush",
                        &[&kind],
                    ));
                }
                let added = self
                    .add_mask_component(slot, &kind)
                    .ok_or_else(|| "这一组蒙版格式不对，加不了形状".to_owned())?;
                let shape = match kind.as_str() {
                    "radial" => "径向",
                    "brush" => "画笔",
                    _ => "线性",
                };
                (
                    json!(added + 1),
                    crate::i18n::tr_fmt(
                        "已在第 {} 张蒙版里加了第 {} 个{}形状",
                        &[&index, &(added + 1), &crate::i18n::tr(shape)],
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
                    crate::i18n::tr_fmt(
                        "已删掉第 {} 张蒙版里的第 {} 个形状",
                        &[&index, &(comp + 1)],
                    ),
                )
            }
            other => {
                return Err(crate::i18n::tr_fmt(
                    "未知动作 {}；可用 add / remove",
                    &[&other],
                ));
            }
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
            return Err(crate::i18n::tr_fmt("reference 超出范围：1..={}", &[&n]));
        }
        let estimator = match opt_str(args, "estimator")? {
            Some(name) => Some(match name.trim().to_ascii_lowercase().as_str() {
                "template" | "模板" => Estimator::Template,
                "phase" | "相位" => Estimator::Phase,
                other => {
                    return Err(crate::i18n::tr_fmt(
                        "未知估计器 {}；可用 template / phase",
                        &[&other],
                    ));
                }
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
    ///
    /// Panel readout: estimate the output frame count and duration from the speed curve.
    /// Integrates the curve only and never builds a whole sampling plan -- a plan over
    /// hundreds of thousands of frames, recomputed every UI frame, would stall the UI.
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
            return Err(crate::i18n::tr_fmt(
                "{} 同时给了：变速曲线只能选一种写法（speed 全程一个倍速 / keys 多点曲线 / ease 缓入缓出）",
                &[&curve_given.join("、")],
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
                return Err(crate::i18n::tr_fmt(
                    "shutter 是曝光区间占本帧源跨度的比例，要在 0..1（0 锐利、1 全平均），实得 {}",
                    &[&shutter],
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
    ///
    /// The pixel size the motion acts on: width and height after straightening and export
    /// cropping, the same definition as the export's frame size.
    /// The viewfinder is normalised against this frame, so the canvas conversion and the
    /// push-in readout both go by it.
    pub fn motion_frame_size(&self) -> (u32, u32) {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        match self.effective_crop() {
            Some(c) => (c.w.max(1), c.h.max(1)),
            None => (w, h),
        }
    }

    /// 运镜的输出画幅：成片面板给了精确宽高就认它，否则就是那一幅原样铺出去。
    ///
    /// The motion output frame: the exact width and height from the render panel when given,
    /// otherwise that frame laid out as is.
    pub fn motion_output_size(&self) -> (u32, u32) {
        if self.output_w >= 2 && self.output_h >= 2 {
            (self.output_w, self.output_h)
        } else {
            self.motion_frame_size()
        }
    }

    /// 按输出帧号取当前运镜的取景框；没开就返回 `None`，预览照原来的路走。
    ///
    /// The viewfinder of the current motion at the given output frame; `None` when it is
    /// off, and the preview takes the original path.
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
            return Err(crate::i18n::tr_fmt(
                "{} 是批量写操作，需参数 confirm: true",
                &[&id],
            ));
        }
        // 先把所有参数校验完，再一次性落到状态里，失败时不留半套设置。
        let size = match (opt_u32(args, "width")?, opt_u32(args, "height")?) {
            (Some(width), Some(height)) => {
                if width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1 {
                    return Err(crate::i18n::tr_fmt(
                        "width/height 必须是偶数且不小于 2：{}×{}",
                        &[&width, &height],
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
                other => {
                    return Err(crate::i18n::tr_fmt(
                        "未知编码 {}；可用 h264 / h265",
                        &[&other],
                    ));
                }
            }),
            None => None,
        };
        // 色域静帧和成片都用同一条参数：省略就保持面板当前选择。
        let color_space = opt_str(args, "color_space")?
            .map(|raw| {
                ColorSpace::parse(raw).ok_or_else(|| {
                    crate::i18n::tr_fmt("未知色域 {}；可用 srgb / p3 / rec2020", &[&raw])
                })
            })
            .transpose()?;
        // 动态范围与色域同理：省略就保持面板当前选择。
        let dynamic = opt_str(args, "dynamic")?
            .map(|raw| {
                OutputDynamic::parse(raw).ok_or_else(|| {
                    crate::i18n::tr_fmt("未知动态范围 {}；可用 sdr / hdr-pq", &[&raw])
                })
            })
            .transpose()?;
        // 容器与质量只在静帧那一路有意义：视频模式撞上有损档现在就拦，别等 core。
        let format = opt_str(args, "format")?
            .map(|raw| {
                StillFormat::parse(raw).ok_or_else(|| {
                    crate::i18n::tr_fmt(
                        "未知容器 {}；可用 png8 / tiff16 / jpeg / webp / avif",
                        &[&raw],
                    )
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
                    let have_text = if have.is_empty() {
                        crate::i18n::tr("（还没有副本）").to_owned()
                    } else {
                        have.join(crate::i18n::tr("、"))
                    };
                    return Err(crate::i18n::tr_fmt(
                        "没有叫 {} 的副本；现有：{}",
                        &[&name, &have_text],
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
            return Err(crate::i18n::tr_fmt(
                "拒绝覆盖素材：{}",
                &[&video_out.display()],
            ));
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
        if let Some(dyn_range) = dynamic {
            self.render_dynamic = dyn_range;
            self.apply_dynamic();
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
            "dynamic": self.render_dynamic.key(),
            // HDR 只对 AVIF 成立，容器装不下时面板该让人看出「这个组合不成立」。
            "dynamic_fits": self.render_dynamic.fits(self.render_format),
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
        project::save(&path, &snapshot)
            .map_err(|e| crate::i18n::tr_fmt("保存工程失败：{}", &[&e]))?;
        self.last_auto_save = Instant::now();
        self.status = crate::i18n::tr_fmt("工程已保存 {}", &[&path.display()]);
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
    ///
    /// Swap the soundtrack and immediately probe its codec and duration once; without ffmpeg
    /// only the reason is recorded, which does not change the pre-export error.
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
            Err(e) => self.audio_probe_err = Some(crate::i18n::err_text(&e)),
        }
    }

    /// 画面 / 配乐 / 成片三段时长；没有探针结果时返回 `None`。
    ///
    /// The three durations: picture / soundtrack / result. `None` without a probe result.
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
    ///
    /// Change several soundtrack settings at once; an empty string or null for `path` clears
    /// it.
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
                let f = v
                    .as_f64()
                    .ok_or(crate::i18n::tr_fmt("{} 必须是数字（秒）", &[&key]))?;
                if !(f.is_finite() && (0.0..=3600.0).contains(&f)) {
                    return Err(crate::i18n::tr_fmt("{} 必须在 0..3600 秒", &[&key]));
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
                crate::i18n::tr_fmt(
                    "未知 acodec {}；可用 auto / copy / aac128 / aac192 / aac256 / aac320",
                    &[&text],
                )
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
                        return Err(crate::i18n::tr_fmt(
                            "配乐文件不存在：{}",
                            &[&path.display()],
                        ));
                    }
                    if !aevum_core::audio::is_audio_path(&path) {
                        return Err(crate::i18n::tr_fmt(
                            "不支持这个扩展名：{}；可用 mp3 / wav / aac / m4a / flac / ogg / opus / wma / aif(m4b)",
                            &[&path.display()],
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
    ///
    /// Import an XMP preset: replaces the global grading parameters wholesale.
    ///
    /// Same definition as Python's `read_xmp` + `merge`: keys the preset does not mention
    /// return to their defaults, and local masks are cleared along with them (a Lightroom
    /// preset does not carry this project's mask model).
    pub fn import_xmp_preset(&mut self, path: &Path) -> Result<Value, String> {
        let params =
            xmp::read(path).map_err(|e| crate::i18n::tr_fmt("读不进这个预设：{}", &[&e]))?;
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
        self.status = crate::i18n::tr_fmt("已导入 XMP 预设 {}", &[&name]);
        let mut out = self.grade_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("path".to_owned(), json!(path.display().to_string()));
            object.insert("imported".to_owned(), json!(name));
        }
        Ok(out)
    }

    /// 把当前参数写成 XMP 预设；覆盖已有文件必须显式确认。
    ///
    /// Write the current parameters out as an XMP preset; overwriting an existing file
    /// requires explicit confirmation.
    pub fn export_xmp_preset(&mut self, path: &Path, confirm: bool) -> Result<Value, String> {
        let is_xmp = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xmp"));
        if !is_xmp {
            return Err("path 必须以 .xmp 结尾".to_owned());
        }
        if path.exists() && !confirm {
            return Err(crate::i18n::tr_fmt(
                "目标已存在，需 confirm: true：{}",
                &[&path.display()],
            ));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)
                .map_err(|e| crate::i18n::tr_fmt("创建目录失败：{}", &[&e]))?;
        }
        xmp::write(path, &self.params)
            .map_err(|e| crate::i18n::tr_fmt("写 XMP 失败：{}", &[&e]))?;
        self.status = crate::i18n::tr_fmt("已导出 XMP 预设 {}", &[&path.display()]);
        Ok(json!({
            "path": path.display().to_string(),
            "preset": self.preset,
        }))
    }

    fn cmd_xmp_import(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        if !path.is_file() {
            return Err(crate::i18n::tr_fmt("文件不存在：{}", &[&path.display()]));
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
            return Err(crate::i18n::tr_fmt("文件不存在：{}", &[&path.display()]));
        }
        let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(true);
        let key = path.display().to_string();
        let lut =
            cube::refresh(&key).map_err(|e| crate::i18n::tr_fmt("读 .cube 失败：{}", &[&e]))?;
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
            // 挂外观就是想看到它：顺手摘掉外观组的旁路（UI 与 MCP 同走这里）。
            //
            // Applying a LUT means wanting to see it: lift the Look group's bypass
            // (both the UI dialog and MCP land here).
            self.bypassed.remove("look");
            self.preview_dirty = true;
            self.preset = "自定义".to_owned();
            self.status = crate::i18n::tr_fmt("已挂外观 {}", &[&key]);
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
            return Err(crate::i18n::tr_fmt(
                "size 必须在 {}..={}",
                &[&cube::MIN_SIZE, &cube::MAX_SIZE],
            ));
        }
        if path.exists() && !confirm(args) {
            return Err(crate::i18n::tr_fmt(
                "目标已存在，需 confirm: true：{}",
                &[&path.display()],
            ));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)
                .map_err(|e| crate::i18n::tr_fmt("创建目录失败：{}", &[&e]))?;
        }
        let params = self.params.clone();
        let size = size as usize;
        cube::export(&path, &params, size)
            .map_err(|e| crate::i18n::tr_fmt("写 .cube 失败：{}", &[&e]))?;
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.status = crate::i18n::tr_fmt("已导出外观 {}", &[&path.display()]);
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
            return Err(crate::i18n::tr_fmt("拒绝覆盖素材：{}", &[&path.display()]));
        }
        if path.exists() && !confirm(args) {
            return Err(crate::i18n::tr_fmt(
                "目标已存在，需 confirm: true：{}",
                &[&path.display()],
            ));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)
                .map_err(|e| crate::i18n::tr_fmt("创建目录失败：{}", &[&e]))?;
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
    ///
    /// Parse and execute one line of "command ID + JSON arguments"; the result goes into the
    /// recent records.
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
            serde_json::from_str::<Value>(rest)
                .map_err(|e| crate::i18n::tr_fmt("参数不是合法 JSON：{}", &[&e]))?
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
        let (window, strength, multi) = (self.def_window, self.def_strength, self.def_multi);
        let align = self.def_align_exif;
        let a = if self.def_channels && measured {
            let rgb: Vec<Option<[f64; 3]>> =
                self.def_means.iter().map(|m| m.map(|m| m.rgb)).collect();
            let evs = self.exif_evs(rgb.len());
            if align {
                deflicker::analyze_rgb_exif(&rgb, &evs, window, strength, multi)
            } else {
                deflicker::analyze_rgb(&rgb, window, strength, multi)
            }
        } else if measured {
            let luma: Vec<Option<f64>> = self.def_means.iter().map(|m| m.map(|m| m.luma)).collect();
            let evs = self.exif_evs(luma.len());
            if align {
                deflicker::analyze_luma_exif(&luma, &evs, window, strength, multi)
            } else {
                deflicker::analyze_luma(&luma, window, strength, multi)
            }
        } else {
            let luma: Vec<Option<f64>> = self
                .metrics
                .iter()
                .map(|m| m.map(|m| m.mean * 255.0))
                .collect();
            let evs = self.exif_evs(luma.len());
            if align {
                deflicker::analyze_luma_exif(&luma, &evs, window, strength, multi)
            } else {
                deflicker::analyze_luma(&luma, window, strength, multi)
            }
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

    /// 每帧的相对 EV（缺三要素的是 `None`），长度**钉死为 `n`**：去闪分析是逐行对齐的，
    /// 表短了会让校正比帧数少，表长了会在尾部多出没人认领的行。
    ///
    /// Per-frame relative EV (`None` where the triplet is missing), length **pinned to `n`**:
    /// the deflicker analysis is row-aligned, so a short table would yield fewer corrections
    /// than frames and a long one would leave unclaimed rows at the tail.
    fn exif_evs(&self, n: usize) -> Vec<Option<f64>> {
        (0..n)
            .map(|i| self.exif.get(i).and_then(|e| e.ev()))
            .collect()
    }

    /// 有曝光三要素的帧数。面板拿它说明预对齐到底生效了几帧——一个 EV 都没有时开关
    /// 是空转的，不说清楚就是假开关。
    ///
    /// How many frames carry the exposure triplet. The panel uses it to say how many frames
    /// the pre-alignment actually covers: with no EV at all the switch is a no-op, and
    /// staying silent about that would be a fake switch.
    pub fn exif_ev_frames(&self) -> usize {
        self.exif.iter().filter(|e| e.ev().is_some()).count()
    }

    pub fn apply_preset(&mut self, name: &str) {
        self.params = grade::preset(name);
        self.preset = name.to_owned();
        self.auto_note = None;
        self.reset_mask_state();
        self.preview_dirty = true;
        history::note(
            "preset",
            crate::i18n::tr_fmt("预设 {}", &[&crate::i18n::tr(name)]),
        );
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
    ///
    /// The context badge in the options bar (the ".ctx .n" entry in the design): how many
    /// groups deviate from their defaults.
    /// Counted by group rather than by parameter -- the badge only answers "was anything
    /// touched, and how much of it", while per-parameter detail is the dock tabs' job.
    pub fn changed_groups(&self) -> usize {
        // 与 `GROUP_SPECS` 里显影模块的组一一对应（ACR 九分区去掉蒙版/蒙版参数
        // 这两组纯动作组——它们没有「偏离默认值」可言；外观挂着 .cube，算一组）。
        //
        // One entry per Develop group in `GROUP_SPECS` that owns grading parameters
        // (the ACR nine sections minus Masks/mask-params, which are action groups with
        // nothing to deviate from; Look counts because it holds a .cube path).
        const KEYS: [&str; 8] = [
            "basic", "curve", "mixer", "grade", "detail", "optics", "effects", "look",
        ];
        KEYS.iter().filter(|k| self.group_changed(k)).count()
    }

    /// 分组头右缘的 accent 圆点开关：这一组里有参数偏离默认值才亮。
    /// 字段清单与 `shell::reset_group` 的复位口径一一对应 —— 那边复位哪些字段，
    /// 这边就比对哪些字段，两边必须同进退。
    ///
    /// The accent dot at the right edge of a group header: lit when any parameter in the
    /// group deviates from its default.
    /// The field list corresponds one-to-one with `shell::reset_group` -- whichever fields
    /// that one resets, this one compares; the two must move together.
    pub fn group_changed(&self, key: &str) -> bool {
        let d = GradeParams::default();
        let p = &self.params;
        // 推子参数组：按规格表比对（唯一事实源，见 controls 模块头注释）。
        // 表外的组内字段——参数曲线的分界、黑白色带、分级的色轮——单独比。
        if crate::controls::specs_of_group(key).next().is_some() {
            let hit = crate::controls::specs_of_group(key)
                .any(|s| crate::controls::param_get(p, s.field) != s.default);
            let extra = match key {
                // 曲线区（ACR 的 Curve 面板）：参数曲线 / 点曲线 / 色阶三种调法共用
                // 一个分区。表里只有 HDR 尾部一条，圆点还得看三条分界、五档色阶
                // 与四条点曲线。
                "curve" => {
                    p.par_split1 != d.par_split1
                        || p.par_split2 != d.par_split2
                        || p.par_split3 != d.par_split3
                        || p.lv_black != d.lv_black
                        || p.lv_white != d.lv_white
                        || p.lv_gamma != d.lv_gamma
                        || p.lv_out_black != d.lv_out_black
                        || p.lv_out_white != d.lv_out_white
                        || p.curve_rgb != d.curve_rgb
                        || p.curve_r != d.curve_r
                        || p.curve_g != d.curve_g
                        || p.curve_b != d.curve_b
                }
                // 颜色混合器（HSL / 黑白两种模式）：表里只有黑白强度一条，
                // 八段色带与八段黑白亮度单独比。
                "mixer" => {
                    p.hsl_h != d.hsl_h
                        || p.hsl_s != d.hsl_s
                        || p.hsl_l != d.hsl_l
                        || p.mono_h != d.mono_h
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
        // 表外组只剩外观一条（蒙版/蒙版参数是动作组，从不亮圆点）。
        //
        // Look is the only group left with no spec table; Masks/mask-params are action
        // groups and never light the dot.
        match key {
            "look" => !p.look_cube.is_empty(),
            _ => false,
        }
    }

    // ------------------------------------------------------------ 局部蒙版

    /// 新建一张单组件蒙版并选中；返回 0 基序号。
    ///
    /// Create a single-component mask and select it; returns its 0-based index.
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
    ///
    /// Select one mask and return to the first shape inside its group.
    pub fn select_mask(&mut self, index: usize) {
        self.mask_sel = Some(index);
        self.mask_comp = 0;
    }

    /// 组内加一个形状；扁平老格式先升级成单组件组。返回新形状的 0 基序号。
    ///
    /// Add a shape to a group; a legacy flat mask is upgraded to a single-component group
    /// first. Returns the new shape's 0-based index.
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
    ///
    /// Remove one shape from a group; the last one is kept, because a mask cannot be an
    /// empty group.
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
    ///
    /// The single path after any mask change: re-render the preview and mark the preset as
    /// custom.
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

    // ------------------------------------------------------------ 修复与几何

    /// 预览实际使用的那块取景（非裁剪态）。`None` 表示预览显示整幅。
    /// 与 `ensure_preview` 用的是同一段判断——两处一旦分叉，画布上的落点就会错位。
    ///
    /// The framing the preview actually uses (outside crop-edit mode). `None` means the preview
    /// shows the whole frame. This is the same decision `ensure_preview` makes: if the two
    /// diverge, taps on the canvas land in the wrong place.
    pub fn preview_crop(&self) -> Option<CropRect> {
        if self.crop_editing() {
            return (self.rotate != 0.0).then(|| self.crop_view());
        }
        let base = if self.align_preview {
            self.alignment_at(self.selected).0
        } else {
            None
        };
        self.aspect_crop(base)
    }

    /// 画布归一化坐标 → 整幅原图归一化坐标。
    ///
    /// Canvas-normalized coordinates → whole-frame normalized coordinates.
    pub fn canvas_to_frame(&self, u: f64, v: f64) -> (f64, f64) {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        match self.preview_crop() {
            Some(c) if c.w > 0 && c.h > 0 => (
                (f64::from(c.x) + u * f64::from(c.w)) / f64::from(w.max(1)),
                (f64::from(c.y) + v * f64::from(c.h)) / f64::from(h.max(1)),
            ),
            _ => (u, v),
        }
    }

    /// 整幅原图归一化坐标 → 画布归一化坐标（落点轮廓回画用）。
    ///
    /// Whole-frame normalized coordinates → canvas-normalized coordinates, used to draw the
    /// outlines of placed strokes.
    pub fn frame_to_canvas(&self, u: f64, v: f64) -> (f64, f64) {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        match self.preview_crop() {
            Some(c) if c.w > 0 && c.h > 0 => (
                (u * f64::from(w.max(1)) - f64::from(c.x)) / f64::from(c.w),
                (v * f64::from(h.max(1)) - f64::from(c.y)) / f64::from(c.h),
            ),
            _ => (u, v),
        }
    }

    /// 现在能不能在画布上落点。拉直、推近取景、裁剪态下画布显示的都不是整幅，
    /// 落点会错位——宁可不接，也不要让用户修错地方。
    ///
    /// Whether a tap can land right now. Under straightening, a zoomed viewfinder or crop-edit
    /// mode the canvas does not show the whole frame, so taps would land in the wrong place:
    /// better to refuse than to repair the wrong spot.
    pub fn heal_can_place(&self) -> bool {
        if self.crop_editing() || self.rotate != 0.0 {
            return false;
        }
        let n = self.frame_count();
        if n == 0 {
            return false;
        }
        self.motion_view_at(self.selected.min(n.saturating_sub(1)), n)
            .is_none()
    }

    /// 修复/透视改了参数之后统一走这里：预览重画、预设转自定义、历史记一条。
    ///
    /// Everything that changes repair or perspective parameters ends here: the preview is
    /// redrawn, the preset turns custom, and one history entry is recorded.
    pub fn retouch_changed(&mut self, note: &str) {
        self.preview_dirty = true;
        if self.preset != "自定义" {
            self.preset = "自定义".to_owned();
        }
        history::note("retouch", note);
    }

    /// 落一笔。`at` 是**整幅归一化**坐标；返回值表示这一下到底有没有落成——
    /// 仿制模式下第一下是取源，落不成。
    ///
    /// Lay one stroke. `at` is in **whole-frame normalized** coordinates; the return value says
    /// whether it actually landed — in clone mode the first tap only picks the source.
    pub fn heal_add(&mut self, at: (f64, f64)) -> bool {
        let r = (self.heal_radius / 100.0).clamp(0.001, f64::from(retouch::SpotSpec::MAX_RADIUS))
            as f32;
        let source = if self.heal_clone {
            let Some(src) = self.heal_src else {
                self.heal_src = Some((at.0 as f32, at.1 as f32));
                self.preview_dirty = true;
                return false;
            };
            // 偏移在像素域算、再折成「长边单位」：x 的归一化分母是宽，y 的是高，
            // 直接相减会得到一个非等比缩放过的偏移。
            //
            // The offset is computed in pixels and then folded into long-edge units: `x` is
            // normalized by the width and `y` by the height, so subtracting them directly
            // would give a non-uniformly scaled offset.
            let (w, h) = self.resolution.unwrap_or((1, 1));
            let long = w.max(h).max(1) as f32;
            Some((
                (at.0 as f32 - src.0) * w as f32 / long,
                (at.1 as f32 - src.1) * h as f32 / long,
            ))
        } else {
            None
        };
        self.params.retouch.push(retouch::SpotSpec {
            x: at.0 as f32,
            y: at.1 as f32,
            r,
            feather: (self.heal_feather / 100.0) as f32,
            source,
        });
        self.retouch_changed("修复笔触");
        true
    }

    /// 撤掉最后一笔；画面上没有笔触时返回 `false`（不产生一条空历史）。
    ///
    /// Drop the last stroke; returns `false` when there is none, so no empty history entry is
    /// recorded.
    pub fn heal_undo(&mut self) -> bool {
        if self.params.retouch.pop().is_none() {
            return false;
        }
        self.retouch_changed("撤销修复笔触");
        true
    }

    /// 清空全部笔触。
    ///
    /// Clear every stroke.
    pub fn heal_clear(&mut self) -> bool {
        if self.params.retouch.is_empty() {
            return false;
        }
        self.params.retouch.clear();
        self.retouch_changed("清空修复笔触");
        true
    }

    /// 自动直立：用当前帧的草图找主竖线，反推梯形四边形。
    /// 返回 `Err` 时给的是给用户看的一句话，界面直接显示。
    ///
    /// Auto-upright: find the dominant vertical lines in the current frame's sketch and derive
    /// the keystone quad. On `Err` the string is meant for the user and shown verbatim.
    pub fn upright_auto(&mut self) -> Result<(), String> {
        let Some(path) = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.path.clone())
        else {
            return Err(crate::i18n::t!("先打开一张照片").to_owned());
        };
        let stamp = source_stamp(&path);
        // 草图就够：`auto_upright` 自己会缩到 640 边再找线，解全幅只是白花几百毫秒。
        //
        // The sketch is enough: `auto_upright` shrinks to a 640px edge before looking for
        // lines, so decoding full size would just waste a few hundred milliseconds.
        let (buf, w, h, _) = decoded_preview_source(&path, stamp, true, "")?;
        let q = perspective::auto_upright(&buf, w, h)
            .ok_or_else(|| crate::i18n::t!("画面里没有可用的竖直主直线").to_owned())?;
        let q = q
            .clamped()
            .ok_or_else(|| crate::i18n::t!("算出的四边形不合法").to_owned())?;
        self.params.upright = Some(q);
        self.retouch_changed("自动直立");
        Ok(())
    }

    /// 卸下透视校正（回到整幅）。
    ///
    /// Drop the perspective correction (back to the whole frame).
    pub fn upright_clear(&mut self) -> bool {
        if self.params.upright.take().is_none() {
            return false;
        }
        self.retouch_changed("清除透视校正");
        true
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
    ///
    /// One-click auto: first propose a version of white balance and tone from the 1 / 50 / 99
    /// percentiles, then accept it through the real chain.
    ///
    /// It writes absolute values rather than adding a step to the current ones -- so
    /// clicking it several times gives the same result; an already well-exposed image may only
    /// get its white balance corrected, which is not a no-op, it is an image that needed
    /// nothing else.
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
                    self.auto_note = Some(crate::i18n::tr_fmt("自动校正失败：{}", &[&e]));
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
            crate::i18n::tr_fmt(
                "自动：{} EV · 对比 {} · 高光 {} · 阴影 {} · 白 {} · 黑 {} · 色温 {} · 色调 {}",
                &[
                    &format!("{:+.2}", q.exposure),
                    &format!("{:+.0}", q.contrast),
                    &format!("{:+.0}", q.highlights),
                    &format!("{:+.0}", q.shadows),
                    &format!("{:+.0}", q.whites),
                    &format!("{:+.0}", q.blacks),
                    &format!("{:+.0}", q.temperature),
                    &format!("{:+.0}", q.tint),
                ],
            )
        } else {
            crate::i18n::tr_fmt(
                "影调已在安全范围，只校白平衡（色温 {} · 色调 {}）",
                &[
                    &format!("{:+.0}", q.temperature),
                    &format!("{:+.0}", q.tint),
                ],
            )
        });
        self.preview_dirty = true;
        history::note(
            "auto-exposure",
            crate::i18n::tr_fmt(
                "自动校正 {} EV · 色温 {}",
                &[
                    &format!("{:+.2}", q.exposure),
                    &format!("{:+.0}", q.temperature),
                ],
            ),
        );
    }

    /// 预览用的参数：旁路里的分组写回默认。**导出不走这一份**——眼睛是对比
    /// 工具不是调色状态，按着眼睛看到的样子不该溜进成片；成片永远用
    /// `self.params` 原样渲染。
    ///
    /// Parameters for the preview: bypassed groups are written back to their defaults.
    /// **Export never goes through this** -- the eye is a comparison aid, not grading
    /// state, and what you saw while holding it must not leak into the deliverable;
    /// exports always render `self.params` as-is.
    pub fn preview_params(&self) -> GradeParams {
        let mut p = self.params.clone();
        for g in &self.bypassed {
            crate::controls::neutralise_group(&mut p, g);
        }
        p
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
        let params = self.preview_params();
        let gain = self.ev_at(self.selected);
        let (crop, offset) = if self.align_preview {
            self.alignment_at(self.selected)
        } else {
            (None, [0.0, 0.0])
        };
        // 裁剪态下画整块可见区：框外那圈像素得露出来才拖得动，取景框只是覆盖层。
        // 拉直之后可见区就是安全框——转出去的反射角不画出来，画布上不会看到鬼影。
        // 与画布上的落点换算共用 `preview_crop`：两处各写一份，迟早分叉。
        //
        // Shares `preview_crop` with the canvas coordinate mapping: two copies would drift.
        let _ = crop;
        let crop = self.preview_crop();
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
        // 检视开关在**发任务那一刻**定下来：拖动中途切开关，这一帧仍按旧值渲染完，
        // 下一帧才换——比中途换链导致画面跳一半要稳。
        //
        // The inspection flag is fixed **when the job is posted**: toggling mid-drag lets
        // this frame finish on the old value and switches on the next one, which is steadier
        // than changing chains halfway through a frame.
        let hdr_fold = self.hdr_preview;
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
                hdr_fold,
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
                    let _ = tx.send(Msg::PreviewFailed(crate::i18n::tr_fmt(
                        "预览失败：{}",
                        &[&e],
                    )));
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
    ///
    /// Expand one root folder: the catalogue (with rating / colour label / flag) is folded out
    /// of that folder's own project file.
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
    ///
    /// Read a folder project's description and copyright into the info panel's editing buffer.
    /// Called only when expanding or rescanning a folder: the panel repaints every frame, and
    /// reading per frame would wipe what is being typed.
    pub fn lib_load_meta(&mut self, index: usize) {
        let Some(dir) = self.lib_roots.get(index).cloned() else {
            return;
        };
        self.lib_meta = aevum_core::library::metadata(&dir);
        self.lib_meta_root = Some(dir);
    }

    /// 信息面板的「存」：写回该目录的工程文件，其余字段原样保留。
    ///
    /// The info panel's Save: writes back to that folder's project file and keeps every other
    /// field as it is.
    pub fn lib_save_meta(&mut self) -> Result<(), String> {
        let dir = self
            .lib_meta_root
            .clone()
            .ok_or_else(|| "还没选定图库目录".to_owned())?;
        let saved = aevum_core::library::set_metadata(&dir, &self.lib_meta)
            .map_err(|e| crate::i18n::tr_fmt("写元数据失败：{}", &[&e]))?;
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
    ///
    /// Add a folder to the library: the same folder keeps one row and moves to the end, then
    /// its catalogue is expanded immediately.
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
    ///
    /// Change one image's rating / flag / colour label straight from the library: written back
    /// to that folder's own project file, with the in-memory catalogue updated in step and no
    /// whole-folder rescan.
    pub fn lib_organize(
        &mut self,
        root: usize,
        frame: usize,
        patch: aevum_core::library::Organize,
    ) -> Result<(), String> {
        let Some(dir) = self.lib_roots.get(root).cloned() else {
            return Err(crate::i18n::tr_fmt(
                "目录序号超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        };
        aevum_core::library::organize(&dir, frame, &patch)
            .map_err(|e| crate::i18n::err_text(&e))?;
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
    ///
    /// Change a class of frames in bulk: one write for the whole batch, with the in-memory
    /// catalogue updated in step.
    pub fn lib_organize_many(
        &mut self,
        root: usize,
        frames: &[usize],
        patch: aevum_core::library::Organize,
    ) -> Result<(), String> {
        let Some(dir) = self.lib_roots.get(root).cloned() else {
            return Err(crate::i18n::tr_fmt(
                "目录序号超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        };
        if frames.is_empty() {
            return Err("没有选中的照片".to_owned());
        }
        aevum_core::library::organize_many(&dir, frames, &patch)
            .map_err(|e| crate::i18n::err_text(&e))?;
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
    ///
    /// A catalogue row was clicked: a plain click selects one row, Ctrl toggles its check
    /// mark, Shift extends from the anchor.
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
    ///
    /// The rows the keyboard and context menu act on: every checked row when there are any,
    /// otherwise just the selected row.
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
    ///
    /// Apply one organise state to the current target rows; returns how many images
    /// changed.
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
    ///
    /// Add / remove keywords on the target rows (checked rows take priority); the project file
    /// is read and written once for the whole batch.
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
            return Err(crate::i18n::tr_fmt(
                "目录序号超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        };
        if frames.is_empty() {
            return Err("没有选中的照片".to_owned());
        }
        let touched = aevum_core::library::keyword_edit(&dir, frames, add, remove)
            .map_err(|e| crate::i18n::err_text(&e))?;
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
        self.lib_roots.get(root).cloned().ok_or_else(|| {
            crate::i18n::tr_fmt("目录序号超出范围：1..={}", &[&self.lib_roots.len()])
        })
    }

    /// 并成一叠：写一次盘，返回归一化后的全部堆叠。少于两张由调用方拦。
    ///
    /// Merge into a stack: one disk write, returns all stacks after normalisation. Fewer than
    /// two images is rejected by the caller.
    pub fn lib_stack_create(
        &mut self,
        root: usize,
        frames: &[usize],
    ) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let stacks = aevum_core::library::stack_create(&dir, frames)
            .map_err(|e| crate::i18n::err_text(&e))?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 拆散一叠（叠号 0 起）；越界当没这叠。
    ///
    /// Dissolve one stack (0-based stack index); out of range is treated as no such stack.
    pub fn lib_stack_dissolve(
        &mut self,
        root: usize,
        stack: usize,
    ) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let stacks = aevum_core::library::stack_dissolve(&dir, stack)
            .map_err(|e| crate::i18n::err_text(&e))?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 按文件名前缀自动分叠（`A001_0001`…/`B002_0001`… 各成一叠）：整目录重写堆叠，
    /// 评级、关键词、元数据一律不动。
    ///
    /// Group automatically by filename prefix (`A001_0001`..., `B002_0001`... each become a
    /// stack): rewrites the stacks of the whole folder and leaves ratings, keywords and
    /// metadata untouched.
    pub fn lib_stack_auto(&mut self, root: usize) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let names: Vec<String> = match self.lib_entries.get(root) {
            Some(entries) if !entries.is_empty() => {
                entries.iter().map(|f| f.name.clone()).collect()
            }
            _ => return Err("这个目录还没编目：先展开它".to_owned()),
        };
        let stacks =
            aevum_core::library::stack_auto(&dir, &names).map_err(|e| crate::i18n::err_text(&e))?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 把目标行（勾选优先）并成一叠；返回这一叠的张数。
    ///
    /// Merge the target rows (checked rows take priority) into one stack; returns its size.
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
    ///
    /// Dissolve the stack the target rows belong to; returns how many images were released.
    /// An error when the target rows are in no stack.
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
    ///
    /// The selected catalogue row -> (folder index, frame index, file); shared by the keyboard
    /// and "add to collection".
    pub fn lib_selected(&self) -> Option<(usize, usize, aevum_core::library::LibFile)> {
        let (root, frame) = (self.lib_sel?, self.lib_sel_file?);
        let f = self.lib_entries.get(root)?.get(frame)?.clone();
        Some((root, frame, f))
    }

    /// 编目里上下挪选中行；跳过被筛掉的行。
    ///
    /// Move the selected row up or down in the catalogue, skipping filtered-out rows.
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
    ///
    /// Put one photo into a collection; returns `false` when it is already there.
    pub fn lib_collection_put(&mut self, col: usize, path: &Path) -> Result<bool, String> {
        if col >= self.lib_cols.len() {
            return Err(crate::i18n::tr_fmt(
                "集合序号超出范围：1..={}",
                &[&self.lib_cols.len()],
            ));
        }
        let added = aevum_core::library::collection_put(&mut self.lib_cols, col, path);
        if added {
            self.lib_save();
        }
        Ok(added)
    }

    /// 一次把几张放进集合；返回新加进去的张数（已在里面的不算）。
    ///
    /// Put several photos into a collection at once; returns how many were newly added (ones
    /// already inside do not count).
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
    ///
    /// The rows of the collection view: member paths are looked up in the catalogued roots
    /// first (so ratings and frame indices come along), and misses get a path-only placeholder
    /// row -- the collection's contents are visible even before a folder has ever been
    /// expanded.
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
    ///
    /// Thumbnails for the visible library rows: keyed by path, and the background thread
    /// shares the same pool as the sequence thumbnails.
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
    ///
    /// Thumbnails for an arbitrary batch of photos (the collection view goes through here
    /// too).
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
            crate::i18n::tr_fmt("记录关键帧 帧{}", &[&format!("{:04}", self.selected + 1)]),
        );
    }

    pub fn remove_keyframe(&mut self, i: usize) {
        if self.keyframes.remove(&i).is_some() {
            history::note(
                "keyframe-del",
                crate::i18n::tr_fmt("删除关键帧 帧{}", &[&format!("{:04}", i + 1)]),
            );
        }
    }

    pub fn goto_keyframe(&mut self, i: usize) {
        if let Some(p) = self.keyframes.get(&i).cloned() {
            self.selected = i.min(self.frame_count().saturating_sub(1));
            self.params = p;
            self.preset = "关键帧".to_owned();
            self.preview_dirty = true;
            self.scroll_to_selected = true;
            history::note(
                "keyframe-goto",
                crate::i18n::tr_fmt("套用关键帧 帧{}", &[&format!("{:04}", i + 1)]),
            );
        }
    }

    // ------------------------------------------------------------ 调色副本

    pub fn copy_index(&self, name: &str) -> Option<usize> {
        self.copies.iter().position(|c| c.name == name)
    }

    /// 当前调色等于哪套副本：整表现算，不留一个改参后会过期的字段。
    ///
    /// Which copy the current grade equals: computed from the whole table every time, with no
    /// field that could go stale after a parameter change.
    pub fn active_copy(&self) -> Option<&str> {
        self.copies
            .iter()
            .find(|c| c.params == self.params && c.keyframes == self.keyframes)
            .map(|c| c.name.as_str())
    }

    pub fn save_copy(&mut self, name: &str, overwrite: bool) -> Result<usize, String> {
        let name = clean_copy_name(name)?;
        match self.copy_index(&name) {
            Some(_) if !overwrite => Err(crate::i18n::tr_fmt(
                "副本「{}」已存在；要覆盖给 overwrite: true",
                &[&name],
            )),
            Some(i) => {
                self.copies[i].params = self.params.clone();
                self.copies[i].keyframes = self.keyframes.clone();
                history::note("copy-save", crate::i18n::tr_fmt("覆盖副本 {}", &[&name]));
                Ok(i)
            }
            None => {
                self.copies.push(GradeCopy {
                    name: name.clone(),
                    params: self.params.clone(),
                    keyframes: self.keyframes.clone(),
                });
                history::note("copy-save", crate::i18n::tr_fmt("存副本 {}", &[&name]));
                Ok(self.copies.len() - 1)
            }
        }
    }

    pub fn apply_copy(&mut self, name: &str) -> Result<(), String> {
        let Some(copy) = self.copies.iter().find(|c| c.name == name).cloned() else {
            return Err(crate::i18n::tr_fmt("没有叫 {} 的副本", &[&name]));
        };
        self.params = copy.params;
        self.keyframes = copy.keyframes;
        // 存中文标识而不是译文：`preset` 会写进工程文件，也要和 `PRESET_NAMES`
        // 逐字比较，翻了之后中文会话里会看到上一次英文会话留下的 "Copy x"。
        // 显示时由 `i18n::tr_preset` 认「副本 」前缀再翻。
        self.preset = format!("副本 {}", copy.name);
        self.preview_dirty = true;
        history::note(
            "copy-apply",
            crate::i18n::tr_fmt("套用副本 {}", &[&copy.name]),
        );
        Ok(())
    }

    pub fn delete_copy(&mut self, name: &str) -> Result<(), String> {
        let Some(i) = self.copy_index(name) else {
            return Err(crate::i18n::tr_fmt("没有叫 {} 的副本", &[&name]));
        };
        self.copies.remove(i);
        history::note("copy-del", crate::i18n::tr_fmt("删副本 {}", &[&name]));
        Ok(())
    }

    pub fn rename_copy(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = clean_copy_name(to)?;
        let Some(i) = self.copy_index(from) else {
            return Err(crate::i18n::tr_fmt("没有叫 {} 的副本", &[&from]));
        };
        if self.copy_index(&to).is_some() {
            return Err(crate::i18n::tr_fmt("副本「{}」已存在", &[&to]));
        }
        self.copies[i].name = to.clone();
        history::note(
            "copy-rename",
            crate::i18n::tr_fmt("副本 {} 改名 {}", &[&from, &to]),
        );
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
    ///
    /// Whether the canvas is in crop mode: the crop tool, or the composition module. In that
    /// mode the preview draws the whole frame and the viewfinder is only a vector layer on top,
    /// so dragging it repaints no pixels.
    pub fn crop_editing(&self) -> bool {
        self.tool == Tool::Crop || self.module == Module::Frame
    }

    /// 画布上是「像素不动、只拖矢量框」的两种态之一：拖裁剪框或拖运镜取景框。
    /// 这两种态下预览都不把取景框叠进像素，所以拖动跟手。
    ///
    /// Whether the canvas is in one of the two states where "pixels stay put, only a vector
    /// box is dragged": dragging the crop box, or dragging the motion viewfinder. In both, the
    /// preview does not bake the box into the pixels, which is what makes the drag feel attached
    /// to the cursor.
    pub fn vector_editing(&self) -> bool {
        self.crop_editing() || self.motion_editing()
    }

    /// 画布上要画的那个框：拖过的用拖出来的，没拖过给当前取景（再兜底整块可见区）。
    ///
    /// The box to draw on the canvas: the dragged one when there is one, otherwise the current
    /// framing (with the whole visible area as a final fallback).
    pub fn crop_box(&self) -> CropRect {
        self.user_crop
            .or_else(|| self.effective_crop())
            .unwrap_or_else(|| self.crop_view())
    }

    /// 画布上画出来的那块像素（旋转后坐标）：整幅，或拉直削出来的安全区。
    /// 裁剪态的覆盖层按它换算屏幕坐标，所以框永远只在有像素的地方走。
    ///
    /// The block of pixels drawn on the canvas (post-rotation coordinates): the whole frame,
    /// or the safe area left after straightening.
    /// The crop-mode overlay converts screen coordinates against it, so the box can never travel
    /// where there are no pixels.
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
    ///
    /// Mouse down on the canvas: hitting a handle resizes, hitting inside the box pans, and
    /// landing outside starts a new box.
    ///
    /// This only sets the drag posture; it does not write `user_crop` and registers no history --
    /// "mouse down" on its own is not an edit. Committing the box and recording history both
    /// happen in `crop_drag_to`: one drag equals one history step, and clicking the canvas leaves
    /// nothing behind (before v0.3.0 a bare click squeezed out a crop history entry).
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
    ///
    /// During the drag: all the geometry rules live in `aevum_core::crop`; this only feeds
    /// coordinates.
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
            crate::i18n::tr_fmt("裁剪 {} × {}", &[&next.w, &next.h]),
        );
    }

    /// 松手：框铺满可见区等于没裁，退回默认取景，省掉一次无谓的重画。
    ///
    /// Mouse up: a box covering the whole visible area means no crop at all, so it falls back to
    /// the default framing and saves a pointless repaint.
    pub fn crop_end(&mut self) {
        self.crop_drag = None;
        let view = self.crop_view();
        if let Some(u) = self.user_crop
            && (u.x, u.y, u.w, u.h) == (view.x, view.y, view.w, view.h)
        {
            self.user_crop = None;
            // 退回默认也是一次取景改动：登记进同一条拖动键，收尾并进这一步，
            // 而不是让字段比对路径在下一帧另起一行「裁剪框」（成对刷屏的另一半）。
            history::note(
                "crop-drag",
                crate::i18n::tr_fmt("裁剪 {} × {}", &[&view.w, &view.h]),
            );
        }
    }

    /// 方向键微调取景框：`step` 一般给 2，按住 Shift 给 10。
    ///
    /// Arrow-key nudge of the viewfinder: `step` is normally 2, and 10 with Shift held.
    pub fn crop_nudge(&mut self, dx: i64, dy: i64, step: u32) {
        let Some((w, h)) = self.resolution else {
            return;
        };
        let cur = self.crop_box();
        self.user_crop = Some(self.clamp_to_view(crop::nudge_rect(cur, dx, dy, step, w, h)));
        history::note(
            "crop-nudge",
            crate::i18n::tr_fmt("裁剪微调 {} {}", &[&format!("{dx:+}"), &format!("{dy:+}")]),
        );
    }

    /// 取消手动裁剪，回到「按比例内接」的默认取景。
    ///
    /// Cancel the manual crop and return to the default "aspect ratio inscribed" framing.
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
    ///
    /// Whether the canvas is dragging the motion viewfinder: the preview then draws the reference
    /// frame (not the contents of the box), and the two boxes are only a vector layer on top, so
    /// dragging repaints nothing.
    /// Crop mode wins -- only one vector layer holds the mouse at a time.
    pub fn motion_editing(&self) -> bool {
        self.motion_picking && !self.crop_editing()
    }

    /// 开关画布的取景框编辑：进入时要重画一次「整幅」预览，离开时要重画回
    /// 框里的内容，两头都得脏一次。
    ///
    /// Toggle canvas viewfinder editing: entering must repaint the whole-frame preview once,
    /// and leaving must repaint the contents of the box once -- both ends need one dirty
    /// frame.
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
    ///
    /// The frame the viewfinder is normalised against (the pixel box after straightening and
    /// export cropping, in align coordinates).
    /// The canvas lays the preview back onto this frame, which is what puts the box where it
    /// visually belongs.
    pub fn motion_base_rect(&self) -> CropRect {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        self.effective_crop()
            .unwrap_or(CropRect { x: 0, y: 0, w, h })
    }

    /// 取景框换算成基准那一幅里的整数像素框（对齐坐标，带基准原点偏移）。
    /// 画布画它、拖它都走这个口径，归一化与像素的往返只在这里发生一次。
    ///
    /// Convert a viewfinder into an integer pixel box inside the reference frame (align
    /// coordinates, with the reference origin offset applied).
    /// Drawing and dragging both go through this definition; the normalised to pixel round trip
    /// happens exactly once, here.
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
    ///
    /// The pixels either end currently occupies.
    pub fn motion_box_rect(&self, which: MotionBox) -> CropRect {
        self.motion_rect_of(match which {
            MotionBox::Start => self.motion.from,
            MotionBox::End => self.motion.to,
        })
    }

    /// 选中帧上运镜走到的那一格：画布上给一条「现在在哪」的参照框。
    ///
    /// Where the motion has reached on the selected frame: a reference box on the canvas saying
    /// "you are here".
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
    ///
    /// Mouse down on the canvas: handle = resize, inside = pan, outside = start a new box. The
    /// same test as cropping, except the coordinate conversion lands on the reference frame and
    /// the result is written into `motion.from` or `motion.to`.
    ///
    /// Same rule as `crop_begin`: mouse down only sets the posture, writes nothing and records no
    /// history. "One click silently enables the motion and squeezes out a history entry" was a
    /// real problem measured in v0.3.0; enabling happens on an actual drag.
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
    ///
    /// During the drag: only the draft's viewfinder changes, and the preview is not
    /// repainted.
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
            crate::i18n::tr_fmt(
                "运镜{} {} × {}",
                &[&crate::i18n::tr(self.motion_box.cn()), &next.w, &next.h],
            ),
        );
    }

    /// 松手：预览不用重画（拖的那一层是矢量），只把这次的尺寸留在历史上。
    ///
    /// Mouse up: the preview needs no repaint (the dragged layer is a vector); only this size is
    /// left in the history.
    pub fn motion_end(&mut self) {
        self.motion_drag = None;
    }

    /// 方向键微调当前拖的那一端：`step` 一般给 2，按住 Shift 给 10。只在取景框编辑态用。
    ///
    /// Arrow-key nudge of the end being dragged: `step` is normally 2, and 10 with Shift held.
    /// Used only in viewfinder editing mode.
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
            crate::i18n::tr_fmt(
                "运镜{}微调 {} {}",
                &[
                    &crate::i18n::tr(self.motion_box.cn()),
                    &format!("{dx:+}"),
                    &format!("{dy:+}"),
                ],
            ),
        );
    }

    /// 开关运镜：关掉就是不动，预览得从「框里的内容」重画回整幅。
    ///
    /// Toggle the motion: turning it off means no movement, so the preview must repaint from the
    /// contents of the box back to the whole frame.
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
        history::note(
            "motion-ease",
            crate::i18n::tr_fmt("运动曲线 {}", &[&crate::i18n::tr(e.cn())]),
        );
    }

    /// 起点终点互换：倒放同一条运动，框不用重拖。
    ///
    /// Swap start and end: plays the same movement backwards without dragging the boxes
    /// again.
    pub fn motion_swap(&mut self) {
        std::mem::swap(&mut self.motion.from, &mut self.motion.to);
        self.preview_dirty = true;
        history::note("motion-swap", "运镜反向");
    }

    /// 把当前拖的那一端铺满整幅：想「从全景推到细节」时先按一下，省得拉把手拉半天。
    ///
    /// Fill the whole frame with the end being dragged: press this first when you want a push-in
    /// from the whole view to a detail, instead of hauling the handle around.
    pub fn motion_fill_box(&mut self) {
        match self.motion_box {
            MotionBox::Start => self.motion.from = crop::FULL,
            MotionBox::End => self.motion.to = crop::FULL,
        }
        self.motion.enabled = true;
        self.preview_dirty = true;
        history::note(
            "motion-fill",
            crate::i18n::tr_fmt("运镜{}铺满", &[&crate::i18n::tr(self.motion_box.cn())]),
        );
    }

    /// 清除运镜：整块回到默认（关着、起点整幅、终点往里收一档）。
    ///
    /// Clear the motion: the whole block returns to its defaults (off, start = whole frame, end
    /// inset by one step).
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
    ///
    /// Straighten: rotate the whole frame by `deg` degrees (positive = image
    /// counter-clockwise). It snaps near 0, because when the fader sits at -0.3 the user
    /// certainly meant "not rotated".
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
        self.status = crate::i18n::tr_fmt("正在对齐 {} 帧…", &[&paths.len()]);

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
                    let _ = tx.send(Msg::AlignFailed(crate::i18n::err_text(&e)));
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
            dynamic: self.render_dynamic,
            watermark: self.render_watermark.clone(),
            // 运镜跟着这一次导出走：面板上定好的取景框，成片里就该照着走。
            motion: self.motion,
        };

        // 变速计划跟这一次导出绑定：线程起来之后就算面板上又动了倍速，
        // 跑的还是启动那一刻算出来的帧序，进度与产物都不会自相矛盾。
        let plan = match export::plan_for(&self.retime, n, self.output_fps.max(1.0) as f64) {
            Ok(plan) => plan,
            Err(e) => {
                self.status = crate::i18n::tr_fmt("变速参数不对：{}", &[&e]);
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
            crate::i18n::t!("正在导出…（续跑：先数已导出的帧）").to_owned()
        } else {
            crate::i18n::t!("正在导出…").to_owned()
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
                    .map_err(|e| crate::i18n::err_text(&e))?;
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
                    .map_err(|e| crate::i18n::err_text(&e))?;
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
            self.status = crate::i18n::t!("正在取消导出…").to_owned();
        }
    }

    /// 状态栏「约剩 Xs」：按最近 8 秒的推进速度估。
    ///
    /// 只吃「当前显示的任务」的采样：任务换了、或者进度回退（新任务从 0 起），就把
    /// 窗口清空重开，免得上一轮的速率借给下一轮。同一进度值重复画帧不记样本，否则
    /// 界面帧率会被误当成任务的推进速度；`done` 为 0 或已经到终点时直接不给数字。
    ///
    /// ETA for the status bar, from the last eight seconds of progress. Only the task
    /// currently on screen contributes samples: switching tasks, or a progress reset,
    /// starts a fresh window so one run's rate never leaks into the next. Repeated frames
    /// at the same progress value are not sampled -- otherwise the UI frame rate would be
    /// mistaken for the task's throughput. No number at `done == 0` or at completion.
    pub fn eta_secs(&mut self, task: &str, done: usize, total: usize) -> Option<u64> {
        if done == 0 || total == 0 || done >= total {
            self.eta = EtaWindow::default();
            return None;
        }
        let now = Instant::now();
        if self.eta.key != task || done < self.eta.last_done {
            self.eta = EtaWindow {
                key: task.to_owned(),
                ..EtaWindow::default()
            };
        }
        self.eta.last_done = done;
        if self.eta.samples.last().is_none_or(|(_, d)| *d != done) {
            self.eta.samples.push((now, done));
        }
        // 只留最近 8 秒、最多 16 个点；至少留一个，否则永远算不出速率。
        while self.eta.samples.len() > 16
            || (self.eta.samples.len() > 1
                && now.duration_since(self.eta.samples[0].0) > Duration::from_secs(8))
        {
            self.eta.samples.remove(0);
        }
        let (t0, d0) = *self.eta.samples.first()?;
        let secs = now.duration_since(t0).as_secs_f32();
        if secs <= 0.0 || done <= d0 {
            return None;
        }
        let rate = (done - d0) as f32 / secs;
        if rate <= 0.0 {
            return None;
        }
        Some(((total - done) as f32 / rate).ceil() as u64)
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
    ///
    /// How many frames this run skipped through resume: 0 when resume is off or the count is not
    /// in yet.
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
    ///
    /// Read the queue from the roaming config at startup; `AppState::new` in tests never touches
    /// that file.
    pub fn load_queue(&mut self) {
        self.queue = queue::load(&self.queue_path);
    }

    fn save_queue(&mut self) {
        if let Err(e) = queue::save(&self.queue_path, &self.queue) {
            self.error = Some(crate::i18n::tr_fmt("队列保存失败：{}", &[&e]));
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
    ///
    /// Queue the current sequence and its render settings; adding the same folder again updates
    /// its spec.
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
            // 加入那一刻的工程快照：排队后再改调色不影响这一段
            // （界面承诺"每段读取加入时的工程参数"）。之前渲染时读磁盘，
            // 自动保存可能已把加入之后的调色写进去，渲出来的成片会缺
            // 最后几次调色（审查确认）。
            project: Some(self.project_snapshot()),
            ..QueueItem::new(&root, out)
        };
        let index = queue::add(&mut self.queue, item)?;
        self.save_queue();
        self.status = crate::i18n::tr_fmt("已加入队列：第 {} 段", &[&(index + 1)]);
        Ok(index)
    }

    /// 开始逐段渲染；立即返回，进度看 `queue.status`。
    ///
    /// Start rendering segment by segment; returns immediately, watch `queue.status` for
    /// progress.
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
        self.status = crate::i18n::tr_fmt("队列渲染 0/{}", &[&n]);

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
                            note: crate::i18n::err_text(&e),
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
            self.status = crate::i18n::t!("正在取消队列…").to_owned();
        }
    }

    /// 面板按钮用：按 0 基序号移除一段。
    ///
    /// For the panel buttons: remove one segment by 0-based index.
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
            return Err(crate::i18n::tr_fmt(
                "width/height 必须是偶数且不小于 2：{}×{}",
                &[&width, &height],
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
                other => {
                    return Err(crate::i18n::tr_fmt(
                        "未知编码 {}；可用 h264 / h265",
                        &[&other],
                    ));
                }
            },
            None if self.codec.contains("264") => "h264",
            None => "h265",
        };
        let preset = match opt_str(args, "preset")? {
            Some(preset) => {
                let preset = preset.trim();
                if !queue::PRESETS.contains(&preset) {
                    return Err(crate::i18n::tr_fmt("未知编码预设 {}", &[&preset]));
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
        // 快照"此刻"的工程：指定的序列就是当前序列时用内存参数（控制通道
        // 这头可能刚改完参数还没到自动保存的 5 秒）；指定的别的序列只能读
        // 它磁盘上的工程，读不到就不带快照，渲染照旧回落磁盘。
        let project = match &current {
            Some(cur) if path_eq(&root, cur) => Some(self.project_snapshot()),
            _ => project::load(&project::project_path(&root)).ok(),
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
            project,
            ..QueueItem::default()
        };
        let index = queue::add(&mut self.queue, item)?;
        self.save_queue();
        self.status = crate::i18n::tr_fmt("已加入队列：第 {} 段", &[&(index + 1)]);
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
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.queue.len()],
            ));
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
            def_align_exif: self.def_align_exif,
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
                dynamic: self.render_dynamic.key().to_owned(),
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
        self.def_align_exif = p.def_align_exif;
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
        // 动态范围同理：旧工程没这个字段就是 SDR，认不出来的手改值也回落到 SDR。
        self.render_dynamic = OutputDynamic::parse(&p.render.dynamic).unwrap_or_default();
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
                    self.status = crate::i18n::tr_fmt("工程已保存 {}", &[&path.display()]);
                }
            }
            Err(e) => {
                if !quiet {
                    self.error = Some(crate::i18n::tr_fmt("工程保存失败：{}", &[&e]));
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
                self.error = Some(crate::i18n::tr_fmt("读取工程失败：{}", &[&e]));
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
    ///
    /// Default parameters for a first RAW import: sharpen 40 / sharpen guard 20 / luminance
    /// denoise 25.
    ///
    /// Hooked in after `load_project` and before `history_reset` in `open_paths`: "only once" is
    /// guaranteed by the project file -- the first import writes the project (including these
    /// defaults), and later opens read it, so the condition no longer holds; an ordinary image
    /// sequence contains no RAW frames and is unaffected.
    /// The defaults enter the initial history, so one undo returns to a clean state.
    ///
    /// Note that `GradeParams::default()` still has a sharpen guard of 0 (project JSON missing
    /// the field also deserialises to 0 and renders the old path): 20 is given only to newly
    /// created projects, never to the engine defaults or the presets.
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
    ///
    /// The status bar's one-click "apply sidecar": import the detected sidecar as a preset
    /// (replacing the global parameters wholesale, the same definition as `xmp.import`), raising
    /// `error` on failure and recording history on success.
    pub fn apply_sidecar_hint(&mut self) {
        let Some(path) = self.sidecar_hint.take() else {
            return;
        };
        match self.import_xmp_preset(&path) {
            Ok(_) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.status = crate::i18n::tr_fmt("已应用 XMP 边车 {}", &[&name]);
                history::note(
                    "xmp-sidecar",
                    crate::i18n::tr_fmt("应用 XMP 边车 {}", &[&path.display()]),
                );
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
            dynamic: self.render_dynamic,
            watermark: self.render_watermark.clone(),
            ..RenderOptions::still(self.render_format)
        }
    }

    /// 动态范围与环境不匹配时把组合补齐：HDR 强制 Rec.2020 + AVIF。
    ///
    /// 三个开关互相约束，与其让人点完导出才被 core 的守卫撞回来，不如让选择本身一致。
    /// 补齐是**可见的**（色域与容器的选中态会跳过去），不是静默改值。
    ///
    /// Fills in a legal combination when the dynamic range does not fit: HDR forces
    /// Rec.2020 + AVIF.
    ///
    /// The three switches constrain each other; better to keep the selection consistent than
    /// to let the user hit core's guard after pressing export. The adjustment is **visible**
    /// (the colour space and container selections move), not a silent rewrite.
    /// 套用一条平台交付预设：画幅、帧率（预设给了才改）、编码器、质量档、速度档一次写进去。
    ///
    /// 只动这五项，裁切/拉直/运镜/水印/色域/配乐一概不碰——预设是**交付口径**，不是
    /// 「重置导出设置」。字段口径与 `aevum_core::delivery::DeliveryPreset::apply_to`
    /// 一一对应，那边有判据（core 端到端会把每个数字验到命令与像素上）。
    ///
    /// Applies one platform delivery preset: frame size, frame rate (only when the preset
    /// states one), encoder, quality target and encoder speed. Nothing else is touched --
    /// crop, straighten, camera move, watermark, colour space and audio stay as they are. A
    /// preset is a delivery convention, not a reset button. The mapping mirrors
    /// `DeliveryPreset::apply_to` in core, where the end-to-end assertion checks every number
    /// against the command line and the pixels.
    pub fn apply_delivery(&mut self, preset: &DeliveryPreset) {
        self.output_w = preset.width;
        self.output_h = preset.height;
        if let Some(fps) = preset.fps {
            self.output_fps = fps as f32;
        }
        self.codec = codec_label_for(preset.codec).to_owned();
        self.render_crf = preset.crf;
        self.render_preset = preset.encoder_preset.clone();
        // H.264 是 8bit，装不下 PQ：可见地把动态范围退回 SDR，而不是留一个按下导出才
        // 报错的组合。
        //
        // H.264 is 8bit and cannot carry PQ: drop the dynamic range to SDR visibly instead of
        // leaving a combination that only fails when export is pressed.
        if preset.codec == Codec::H264 {
            self.render_dynamic = OutputDynamic::Sdr;
        }
    }

    /// 当前设置是否**正好等于**这条平台口径。高亮由数值反推：改掉任何一项就自动熄灭，
    /// 不会出现「按钮亮着、设置其实已经不是它」的假象。
    ///
    /// 编码速度档不参与比较——那是性能偏好，不是平台口径。
    ///
    /// Whether the current settings **exactly match** this convention. The highlight is
    /// inferred from the numbers, so changing any one turns it off rather than leaving a lit
    /// chip that no longer matches. Encoder speed is excluded: a performance preference, not
    /// part of the convention.
    pub fn matches_delivery(&self, preset: &DeliveryPreset) -> bool {
        self.output_w == preset.width
            && self.output_h == preset.height
            && self.codec == codec_label_for(preset.codec)
            && self.render_crf == preset.crf
            && preset
                .fps
                .is_none_or(|f| (self.output_fps - f as f32).abs() < 0.5)
    }

    pub fn apply_dynamic(&mut self) {
        if self.render_dynamic != OutputDynamic::HdrPq {
            return;
        }
        // 两个前提：色域必须 Rec.2020（PQ 配窄色域没人认），容器 / 编码器必须装得下
        // 10bit。静帧是 AVIF，成片是 HEVC——两条都当场补齐，别让用户点导出才发现。
        //
        // Two prerequisites: Rec.2020 primaries (PQ in a narrow gamut is recognised by
        // nobody) and a container/codec that holds 10bit. AVIF for stills, HEVC for video —
        // both fixed here rather than at export time.
        self.render_color_space = ColorSpace::Rec2020;
        match self.render_mode {
            OutputMode::Stills => {
                if !self.render_dynamic.fits(self.render_format) {
                    self.render_format = StillFormat::Avif;
                }
            }
            OutputMode::Video => self.codec = "HEVC".to_owned(),
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
        self.status =
            crate::i18n::tr_fmt("正在导出当前帧（{}）…", &[&crate::i18n::tr(format.cn())]);
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
                    let _ = tx.send(Msg::ExportFailed(e));
                }
            }
            ctx2.request_repaint();
        });
    }

    /// 测试与截图用的演示状态：不碰磁盘，序列与测光都是合成的。
    ///
    /// Demo state for tests and screenshots: touches no disk, with a synthetic sequence and
    /// synthetic metering.
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
    ///
    /// The demo edit used for screenshots: one radial local mask, selected, with its panel
    /// expanded.
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
        self.collapsed.insert("optics");
        self.collapsed.insert("effects");
        self.collapsed.remove("masks");
        self.preview_dirty = true;
    }

    /// 给无显示环境铺一套合成纹理，让画布与胶片条的绘制路径也被走到。
    ///
    /// Lay down a set of synthetic textures for display-less environments, so the canvas and
    /// filmstrip drawing paths are exercised too.
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
    hdr_fold: bool,
) -> Result<PreviewParts, String> {
    // 基准开着的时候把解码与调色各量一段（见 `PreviewPhases`）；关着时这里只是一次
    // relaxed 原子读，热路径上没有别的代价。
    //
    // While the benchmark is on, decode and grade are timed separately (see
    // `PreviewPhases`); when it is off this is a single relaxed atomic load.
    let trace = PREVIEW_TRACE_ON.load(Ordering::Relaxed);
    let t0 = Instant::now();
    let stamp = source_stamp(path);
    let (mut buf, w, h, full) = decoded_preview_source(path, stamp, sketch, &params.dcp_profile)?;
    let decode_dt = t0.elapsed();
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
    // 草图坐标 = 全量坐标 × (草图边长 / 全量边长)。不能写死 0.5：草图按 `SKETCH_SIDE`
    // 解码，边长比是算出来的（1600×1067 的草图是 800×533，比正好 0.5；小图原样不减半，
    // 比就是 1.0）。取景框与位移必须按这个比换算，否则松手换全量时构图会跳。
    //
    // Sketch coordinates are full coordinates times (sketch side / full side). The ratio is
    // computed rather than pinned at 0.5: the sketch decodes at `SKETCH_SIDE`, so for a
    // 1600x1067 preview it is 800x533 (exactly 0.5) while a picture small enough not to be
    // scaled keeps 1.0. Framing and offset must follow it or the composition jumps when the
    // full frame replaces the sketch.
    let (crop, framing) = if sketch {
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
