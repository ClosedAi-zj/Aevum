//! 界面外壳：Photoshop 式菜单行 / 选项栏 / 工具轨 / 标尺画布，
//! 配 Lightroom 式右侧直方图、折叠调整组和底部胶片条。
//!
//! 只借布局与交互习惯，图标、配色与控件全部自绘。
//!
//! UI shell: a Photoshop-style menu row, options bar, tool rail and ruled canvas,
//! paired with a Lightroom-style histogram on the right, collapsible adjustment groups
//! and a bottom filmstrip.
//!
//! Only the layout and interaction habits are borrowed; icons, colour schemes and
//! controls are all drawn in-house.

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use self::canvas::canvas_panel;
use self::chrome::window_resize;
use self::dock::dock_panel;
use self::menu::handle_input;
use self::menu::menu_row;
use self::toolbar::options_bar;
use self::toolbar::status_bar;
use self::toolbar::status_bar_needed;
use self::toolbar::{RailHalf, tool_rail};
use self::windows::about_window;
use self::windows::error_dialog;
use self::windows::look_window;
use self::windows::market_window;
use self::windows::presets_window;
use self::windows::rollback_window;
use self::windows::update_window;
use crate::icons::{self, Glyph};
use crate::layout::StatusSlot;
use crate::platform;
use crate::state::AppState;
use crate::theme::*;

// ---- 模块划分（性能重构设计-2026-10-30 §7/§10）----
mod adjust;
mod bench;
mod canvas;
mod canvas_cards;
mod chrome;
mod common;
mod dialogs;
mod dock;
pub(crate) mod export_dialog;
mod export_ws;
pub(crate) mod flow;
mod flow_panel;
mod library;
mod markdown;
mod mask;
mod menu;
mod pages;
mod palette;
mod seq_table;
mod tabs;
mod tasks_ws;
mod timeline;
mod toolbar;
mod windows;

// 对外（crate 内其他模块）仍走 crate::shell::X 路径，重新导出保持外部不变：
//
// `reset_group` 只有测试在用（`controls.rs` 的复位穷举），生产代码走的是
// `common.rs` 里的相对路径 `super::adjust::reset_group`。不加 `cfg(test)`
// 的话，普通构建下这条重导出没人引用，clippy 会当作 unused import 报警。
#[cfg(test)]
pub(crate) use adjust::reset_group;
/// 编号工作台（显影 / 帧表 / 导出 / 任务）：外壳与状态共用同一个枚举。
///
/// The numbered bench (develop / table / export / tasks): shell and state share one enum.
pub(crate) use bench::Bench;
pub(crate) use mask::{
    DeflCurveCache, HistMeshCache, MaskGeo, MaskGeoCache, mask_comp_object, mask_geo_fingerprint,
    mask_geos, mask_handles, mask_radial_points, mask_stroke_lines,
};

/// 画布标尺槽宽：留到能站稳 4 位刻度。
const RULER: f32 = 24.0;
/// 压在图像上的微标签（标尺刻度、胶片序号）：比界面最小字阶再小一档，
/// 但不许低于 9px，否则嵌进来的静态字重会发虚。
const MICRO: f32 = 9.0;

const UV: Rect = Rect {
    min: Pos2::new(0.0, 0.0),
    max: Pos2::new(1.0, 1.0),
};

/// 首帧应用一次 `AEVUM_WORKSPACE`（见 `ui()` 里的说明）。
///
/// Apply `AEVUM_WORKSPACE` once, on the first frame (see the note in `ui()`).
/// 首帧挂上导出确认窗：`AEVUM_EXPORT_CONFIRM=1`（整段）/ `=current`（当前帧）。
///
/// 这个窗口只有点了「导出」才会出现，`--shot` 点不了按钮，所以它一直是截图回归的
/// 死角——用户在 2026-11-08 报告的「点击导出有问题」正是发生在这一段。钩子把 pending
/// 标记直接摆上去（与按钮点击写的是同一个 `Id`），窗口就能被拍下来比对。
///
/// Mounts the export confirmation on the first frame: `AEVUM_EXPORT_CONFIRM=1` (whole
/// sequence) or `=current` (current frame). The dialog only appears after clicking export,
/// which `--shot` cannot do, so it stayed outside screenshot regression -- exactly the area
/// the user reported problems in on 2026-11-08. The hook sets the same `Id` the button sets.
fn debug_export_confirm(ctx: &egui::Context) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(raw) = std::env::var_os("AEVUM_EXPORT_CONFIRM") else {
        return;
    };
    let kind: u8 = match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "current" | "frame" | "2" => 2,
        _ => 1,
    };
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("aevum-export-pending"), kind));
}

/// 首帧挂上工作流面板：`AEVUM_FLOW=1` 展开，`=develop|merge|timelapse` 同时选流程。
///
/// `--shot` 点不了左轨按钮，这面板与三套步骤条否则永远进不了截图回归。只读环境
/// 变量，不落盘、不进工程。
///
/// Mounts the workflow panel on the first frame: `AEVUM_FLOW=1` expands it, and
/// `=develop|merge|timelapse` also selects a flow. `--shot` cannot click rail buttons,
/// so without this hook neither the panel nor the three step sets would ever appear in
/// screenshot regression. Reads the environment only; nothing is persisted.
fn debug_apply_flow(state: &mut AppState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(raw) = std::env::var_os("AEVUM_FLOW") else {
        return;
    };
    let v = raw.to_string_lossy().trim().to_ascii_lowercase();
    if v.is_empty() || matches!(v.as_str(), "0" | "off" | "false") {
        return;
    }
    state.show_flow = true;
    if let Some(t) = flow::FlowTask::parse(&v) {
        state.flow_task = t;
    }
}

/// 首帧打开一扇浮窗：`AEVUM_WINDOW=market|look|presets|about|update`。
///
/// 用户两轮截图都报「子窗口太大 / 比例不对」，而 `--shot` 点不了菜单。给一个
/// 只读钩子，五扇窗在 1280×800 下的真实尺寸才有证据图可比对。
///
/// Opens one floating window on the first frame:
/// `AEVUM_WINDOW=market|look|presets|about|update`. Two rounds of user screenshots
/// complained about oversized / oddly proportioned windows, and `--shot` cannot click
/// a menu. This read-only hook makes the real 1280x800 sizes capturable.
/// 首帧弹一次「打开照片」对话框：`AEVUM_OPEN_PHOTOS=1`。
///
/// 十个系统对话框全部跑在 UI 线程上（winit 事件处理内重入），2026-11-08 用户
/// 实测点「打开照片」后主窗口被压成一条。没有这个钩子就无法在自动化里触发它
/// ——对话框只有真的弹出来，窗口矩形才能量。
///
/// Opens the "open photos" dialog once on the first frame: `AEVUM_OPEN_PHOTOS=1`.
/// The dialog runs on the hub's worker thread, so the main window stays visible and
/// interactive the whole time -- exactly the state that used to break (2026-11-08).
/// Without this hook the automation cannot trigger a system dialog at all: only a
/// really-open dialog lets the window rectangle be measured.
fn debug_open_photos(state: &mut AppState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    if std::env::var_os("AEVUM_OPEN_PHOTOS").is_none() {
        return;
    }
    dialogs::open_photos_dialog(state);
}

fn debug_apply_window(state: &mut AppState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(raw) = std::env::var_os("AEVUM_WINDOW") else {
        return;
    };
    match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "market" | "plugins" => state.market_open = true,
        "look" => state.look_open = true,
        "presets" => state.show_left = true,
        "about" => state.about_open = true,
        "update" => state.update_open = true,
        _ => {}
    }
}

/// 首帧打开逐帧亮度曲线：`AEVUM_CURVES=1`。
///
/// 曲线块的开关在胶片卡的头行里，`--shot` 点不了它。没有这个只读钩子，截图回归
/// 永远只拍得到「曲线关着」的卡，而设计稿的默认态是开着的。与 `AEVUM_BENCH`
/// 同一类用途：只读环境变量，不落盘、不进工程。
///
/// Turns the frame-luminance curve on for the first frame: `AEVUM_CURVES=1`.
///
/// The toggle lives in the film card's header and `--shot` cannot click it, so without
/// this read-only hook screenshot regression would only ever capture the card with the
/// curve off. Same class as `AEVUM_BENCH`: reads the environment, persists nothing.
fn debug_show_curves(state: &mut AppState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    if std::env::var_os("AEVUM_CURVES").is_some() {
        state.show_curves = true;
    }
}

/// 首帧切工作台：`AEVUM_BENCH=develop|table|export|tasks`。
///
/// `--shot` 点不了导航卡，四个工作台因此拍不到证据图；这与 `AEVUM_WORKSPACE`
/// 是同一类只读钩子（只读环境变量，不落盘、不进工程）。
///
/// Switch benches on the first frame: `AEVUM_BENCH=develop|table|export|tasks`.
///
/// `--shot` cannot click the nav card, so the four benches would have no screenshots;
/// this is the same class of read-only hook as `AEVUM_WORKSPACE`.
fn debug_apply_bench(state: &mut AppState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(raw) = std::env::var_os("AEVUM_BENCH") else {
        return;
    };
    let b = match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "table" | "帧表" => Bench::Table,
        "export" | "导出" => Bench::Export,
        "tasks" | "任务" => Bench::Tasks,
        "develop" | "显影" => Bench::Develop,
        _ => return,
    };
    state.set_bench(b);
}

fn debug_apply_workspace(state: &mut AppState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(raw) = std::env::var_os("AEVUM_WORKSPACE") else {
        return;
    };
    let raw = raw.to_string_lossy();
    let w = match raw.trim().to_ascii_lowercase().as_str() {
        "序列" | "sequence" | "seq" => crate::layout::Workspace::Sequence,
        "调色" | "develop" => crate::layout::Workspace::Develop,
        "成片" | "render" => crate::layout::Workspace::Render,
        _ => return,
    };
    state.apply_workspace(w);
}

pub fn ui(state: &mut AppState, root: &mut Ui) {
    // 每帧第一件事：把 core 压下的告警搬进状态栏。护带目录写不进去这类事
    // 必须在用户点保存之前就看见——晚一帧，可能已经点下去了。
    state.drain_notices();
    let ctx = root.ctx().clone();
    let full = root.max_rect();
    // 调试钩子：`AEVUM_WORKSPACE=序列|调色|成片` 在首帧应用一次工作区。
    // `--shot` 点不了按钮，三档布局因此没法出证据图；这与 `AEVUM_PALETTE`、
    // `AEVUM_DBG_RECT` 是同一类用途（只读环境变量，不落盘、不进工程）。
    //
    // Debug hook: `AEVUM_WORKSPACE=sequence|develop|render` applies a workspace on the first
    // frame. `--shot` cannot click, so the three layouts could not be captured otherwise --
    // same class of hook as `AEVUM_PALETTE` and `AEVUM_DBG_RECT` (reads the environment only;
    // nothing is persisted and nothing enters the project file).
    debug_apply_workspace(state);
    debug_apply_bench(state);
    debug_show_curves(state);
    debug_apply_flow(state);
    debug_apply_window(state);
    debug_export_confirm(&ctx);
    debug_open_photos(state);
    ctx.set_zoom_factor(FONT_ZOOMS[state.font_level.min(FONT_ZOOMS.len() - 1)]);
    // 撤销历史按帧记账：先给这一帧编号，帧末把控件登记过的改动并成一步。
    state.history.begin_frame();
    handle_input(state, &ctx);

    // ---------------------------------------------------------------
    // 区域装配：这里只看 `state.layout`，各区域自己的绘制函数不认识布局。
    // 顺序即层级 —— egui 的 Panel 先声明的先占边，后声明的往中间挤。
    // ---------------------------------------------------------------
    let layout = state.layout;

    // 1 标题栏：品牌 │ 五项菜单 │ 工作区胶囊 │ 导出 │ 窗口按钮，一行到底。
    //   底色对齐界面稿 ttl-bg（比 chrome 暗半档、贴着 header）：标题栏是"骨架"，
    //   不是内容面板，不能跟下面的选项栏同层。
    egui::Panel::top("menu-row")
        .exact_size(platform::chrome().titlebar_h)
        .frame(egui::Frame::NONE.fill(header()))
        .show(root, |ui| menu_row(state, ui, &ctx));

    // 状态栏放到标题栏之下时，它读起来是抬头：先读到在哪个文件、什么色彩空间。
    // 只在真有话要说（告警 / 长任务 / 边车）时才占这一行。
    //
    // A status bar under the title bar reads as a header line; it only takes the row
    // when it actually has something to say (notice / long task / sidecar).
    if layout.status == StatusSlot::Top && status_bar_needed(state) {
        egui::Panel::top("status-bar")
            .exact_size(STATUS_H)
            .frame(egui::Frame::NONE.fill(header()))
            .show(root, |ui| status_bar(state, ui));
    }

    // 2 工具栏：当前工具 │ 工具参数 │ 上下文 ……… 右侧缩放。
    // 只在显影工作台出现：帧表/导出/任务是整页视图，一条工具参数栏在那里既没有
    // 对象（没有当前工具）也没有意义（design `.wsview` 只有页头 + 二级工具条）。
    //
    // The options bar belongs to the develop bench only: table / export / tasks are
    // full-page views, and a tool-options row there has neither a subject (no current
    // tool) nor meaning (the design's `.wsview` is page head + secondary tool strip).
    // 底色用 dock 而不是 chrome（B0 裁决）：选项栏服务于右侧参数面板，与面板同层
    // 才读得出「骨架（菜单行/状态栏，header）→ 内容（选项栏/dock，dock）→ 控件
    // （elev）」三级；chrome 让给工具轨，形成「外框稍亮、内容沉下去」的 ACR 结构。
    //
    // The options bar uses `dock` rather than `chrome` (B0 ruling): it serves the
    // parameter panel on the right, so sitting on the same layer makes the three-level
    // reading work -- skeleton (menu/status bar, header) -> content (options bar and
    // dock) -> controls (elev). `chrome` is left to the tool rail, so the outer frame
    // reads slightly brighter than the content, as in ACR.
    let bench = state.bench;
    let develop = bench == Bench::Develop;
    if develop {
        egui::Panel::top("options-bar")
            .exact_size(OPTIONS_H)
            .frame(egui::Frame::NONE.fill(dock()).inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: 0,
                bottom: 0,
            }))
            .show(root, |ui| options_bar(state, ui, &ctx));
    }

    // 顶部步骤栏（LapseOne 式）：多帧序列才摆，单张与空工程没有流程。
    // 声明在选项栏之后，正好落在它下面一条。
    //
    // Top step bar (LapseOne-style): only for multi-frame sequences; single photos and
    // empty projects have no flow. Declared after the options bar, so it stacks right
    // beneath it.
    // 常显：LapseOne 的顶部标签就是主导航，不藏在面板开关里（用户实测
    // show_flow 关着时「什么都没有」）。
    //
    // Always on: the LapseOne-style tabs ARE the primary navigation, never hidden
    // behind the panel switch (user measured "nothing is there" while show_flow was off).
    if develop && state.frame_count() > 1 {
        egui::Panel::top("flow-top")
            .exact_size(30.0)
            .frame(egui::Frame::NONE.fill(header()).inner_margin(egui::Margin {
                left: 10,
                right: 10,
                top: 0,
                bottom: 0,
            }))
            .show(root, |ui| flow_panel::flow_top_bar(state, ui));
    }

    // 3 底部状态栏：只在真有话要说时占窗口最底边。胶片条已改成画布上的浮卡
    //   （见中央面板之后的 `film-card`），不再与它争这一行。
    //
    // The bottom status bar only takes the window's bottom edge when it has something
    // to say; the filmstrip is a card over the canvas now (see `film-card` after the
    // centre panel) and no longer competes for that row.
    if develop && layout.status == StatusSlot::Bottom && status_bar_needed(state) {
        egui::Panel::bottom("status-bar")
            .exact_size(STATUS_H)
            .frame(egui::Frame::NONE.fill(header()))
            .show(root, |ui| status_bar(state, ui));
    }

    // 4 侧栏。同一侧「先声明的在外」，顺序即由外到内。
    // 工具轨拆成两根（用户 2026-11-09）：上半「工作流 / 面板入口」靠右缘、
    // 下半「工具组 / 轨底设置」靠左缘，两侧各 7 枚，视觉对称。
    // 「外观与布局 → 工具轨 右/左」整体镜像这两根轨。
    //
    // 4 Side panels: on one side the first declaration is outermost. The tool rail splits
    // into two (user, 2026-11-09): the upper workflow / panel entries at the right edge,
    // the lower tools / foot at the left edge -- seven entries each, symmetric. The
    // appearance window's "tool rail right/left" mirrors both.
    let rail_left = layout.rail.is_left();
    let dock_left = layout.dock_side.is_left();
    let panels_on_right = !rail_left;

    // 工具轨、右坞、工作流面板都只属于显影工作台。帧表 / 导出 / 任务是整页视图
    // （design `.wsview`），任何侧栏在那里都会把 300px 宽的表格挤出屏幕。
    //
    // Rails, the dock and the flow panel belong to the develop bench. Table / export /
    // tasks are full-page views (the design's `.wsview`); any side panel there would
    // squeeze the 300 px-wide table off screen.
    if develop {
        // 右缘：贴边的那一根先声明，坞再向内叠。
        let (right_id, right_half) = if panels_on_right {
            ("rail-panels", RailHalf::Panels)
        } else {
            ("rail-tools", RailHalf::Tools)
        };
        egui::Panel::right(right_id)
            .exact_size(TOOL_W)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(chrome()))
            .show(root, |ui| tool_rail(state, ui, right_half, false));
        if state.show_dock && !dock_left {
            floating_dock(root, state, true);
        }

        // 左缘：另一根贴边；左坞与工作流面板依次向内。
        let (left_id, left_half) = if panels_on_right {
            ("rail-tools", RailHalf::Tools)
        } else {
            ("rail-panels", RailHalf::Panels)
        };
        egui::Panel::left(left_id)
            .exact_size(TOOL_W)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(chrome()))
            .show(root, |ui| tool_rail(state, ui, left_half, true));
        if state.show_dock && dock_left {
            floating_dock(root, state, false);
        }
        // 5 工作流面板：最后声明，所以贴着画布（同侧先声明的在外）。窄窗强制收起；
        // 没导入序列时不收起——面板会给出「导入照片…」，不能点了开关却什么都没有。
        //
        // 5 Workflow panel: declared last so it sits next to the canvas (on the same side the
        // first declaration is outermost). Forced closed on narrow windows, but **not** when
        // no sequence is loaded -- the panel then offers "Import photos...", because a toggle
        // that visibly does nothing is a silent failure.
        if flow_panel::flow_panel_visible(state, full.width()) {
            egui::Panel::left("flow-panel")
                .exact_size(FLOW_W)
                .resizable(false)
                .frame(
                    egui::Frame::NONE
                        .fill(dock())
                        .inner_margin(egui::Margin::symmetric(10, 6)),
                )
                .show(root, |ui| flow_panel::flow_panel(state, ui, &ctx));
        }
    }

    // 画布右边界按已知的右侧面板宽度夹一次：切模块时 dock 内容一换，egui 的
    // Panel 会用它记着的「上次宽度」去缩剩余区，而面板自己的 rect 已经是
    // `exact_size`，两者会连着几十帧对不上（画布因此压在 dock 页签上，见判据
    // `ui_tokens::text_never_overlaps_in_any_module`）。让位量集中在 `layout.rs`，
    // 用它算边界比依赖第三方内部状态稳。
    //
    // Clamp the canvas right edge from the known right-hand panel widths: switching
    // modules swaps the dock content, and egui's Panel shrinks the remainder using the
    // width it remembers while its own rect already honours `exact_size`, so the two
    // disagree for tens of frames (the canvas ends up sitting on the dock tabs -- see
    // `text_never_overlaps_in_any_module`). The insets live in `layout.rs`, and deriving
    // the edge from them beats depending on a third party's internal state.
    // 内侧那一侧要按 `layout.dock_side` 算：右坞时夹右边、左坞时夹左边。老写法
    // 一律当右边算，用户把面板摆到左边（布局预设允许，且会落盘）画布就凭空少
    // 372px，右半边留白。
    //
    // The inset goes on the side the dock actually sits on: clamp the right edge for a
    // right dock, the left edge for a left one. The old code always clamped the right,
    // so moving the panes to the left (a persisted layout choice) stole 372 px from the
    // canvas and left a blank band on the right.
    // 画布让位量只有一个来源：`layout::canvas_insets`（面板宽 + 10px 呼吸带）。
    // 中央面板、画布内部排布、呼吸带绘制全部从这两个数反推，不许再各自加一次 10。
    //
    // The canvas insets have a single source: `layout::canvas_insets` (panel width plus
    // the gutter). The centre panel, the canvas' own layout and the gutter painting all
    // derive from these two numbers; nothing adds the 10 a second time.
    let insets =
        crate::layout::canvas_insets(state.show_dock, dock_left, state.dock_w + DOCK_GUTTER * 2.0);
    // 中间区自己也得钉住：`insets` 只交代画布**内部**怎么排，管不了
    // CentralPanel 占多宽。而 CentralPanel 拿的是「所有面板之后剩下的空间」——
    // 右坞内容一旦比面板宽，egui 会把面板 rect 整体往右推（`set_rect_size` 保
    // 右边界），剩下的空间就伸进面板里，画布底色盖掉页签与推子左缘（2026-11-05
    // 合成模块英文界面实测：溢出 86px）。给 CentralPanel 一个算出来的 max_rect，
    // 它就永远压不到面板上，不管面板内容多长。
    //
    // The centre area needs pinning too: `insets` only says how the canvas lays
    // itself out, not how wide CentralPanel may be, and CentralPanel takes whatever is
    // left after all panels. Let the dock content exceed the panel width and egui pushes
    // the panel rect right (`set_rect_size` keeps the right edge), the remainder then
    // spills into the panel, and the canvas fill paints over the tabs and the left edge
    // of the sliders (measured on the English Merge module, 2026-11-05: 86 px of
    // overflow). Handing CentralPanel a computed max_rect keeps it off the panel no
    // matter how long the panel's content gets.
    let available = root.available_rect_before_wrap();
    let centre = if develop {
        crate::layout::centre_rect(full, available, insets)
    } else {
        // 整页工作台：没有侧栏，内容区就是顶栏以下的全部（design `.wsview{inset:44px 0 0 0}`）。
        //
        // Full-page bench: no side panels, so the content area is everything below the top
        // bar (the design's `.wsview{inset:44px 0 0 0}`).
        available
    };
    let mut centre_ui = root.new_child(egui::UiBuilder::new().max_rect(centre).id_salt("centre"));
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(if develop { surround() } else { bench_bg() }))
        .show(&mut centre_ui, |ui| {
            // 三步三页（用户 2026-11-09）：序列管理看整条序列、调色与修复看画布、
            // 导出视频看队列与设置。单张与空工程恒为画布。
            //
            // Three steps, three pages (user, 2026-11-09): sequence manager shows the
            // whole sequence, develop shows the canvas, export shows queue and settings.
            // Single photos and an empty project always get the canvas.
            // 截图钩子（仅 `--shot` 用）：`AEVUM_DEMO_PAGE=export|sequence` 强制
            // 打开对应页面。导入流程会把手选步重置回第一段，所以在渲染决策处判定
            // 才可靠——否则 headless 永远只能截到派生页。
            //
            // Screenshot hook (`--shot` only): `AEVUM_DEMO_PAGE=export|sequence` forces
            // the page. Import resets the hand-picked step, so deciding here is the only
            // reliable way for a headless shot to reach another page.
            let forced = match std::env::var("AEVUM_DEMO_PAGE").as_deref() {
                Ok("export") => Some(flow_panel::FlowPage::Export),
                Ok("sequence") => Some(flow_panel::FlowPage::Sequence),
                _ => None,
            };
            if !develop {
                pages::bench_page(state, ui, &ctx);
                return;
            }
            match forced.unwrap_or_else(|| flow_panel::current_page(state)) {
                flow_panel::FlowPage::Sequence => pages::sequence_page(state, ui, &ctx),
                flow_panel::FlowPage::Export => pages::export_page(state, ui, &ctx),
                flow_panel::FlowPage::Develop => canvas_panel(state, ui, insets.right),
            }
        });

    // 胶片卡：浮在画布左下角（设计稿 `.filmcard{left:12;bottom:12}`）。声明在
    // 中央面板之后，所以它压在照片上；`Order::Middle` 让它高于画布、低于工作台
    // 导航卡与各类浮窗。锚点贴**画布**左缘与下缘：左侧那根 24px 工具轨一直伸到
    // 窗口底（用窗口左缘会把轨底的设置钮压住），底部可能还有一条状态栏。
    // 整页视图（项目库 / 设置）不摆——它们与「当前序列的帧」无关。
    //
    // The film card floats over the canvas' bottom-left corner (design `.filmcard`).
    // Declared after the centre panel so it sits on the photo; `Order::Middle` keeps it
    // above the canvas and below the bench nav card and the floating windows. It hugs
    // the **canvas'** left and bottom edges: the 24 px tool rail runs to the window
    // bottom (the window edge would bury its foot button) and a status bar may sit
    // below. Full-page benches get no card -- they have no "current sequence frame".
    if develop && layout.film.allowed() && state.frame_count() > 0 {
        let card_h = timeline::card_height(state);
        let pos = Pos2::new(centre.left() + 12.0, centre.bottom() - 12.0 - card_h);
        // 卡宽不超过窗口一半：再宽就把画布切成两半，浮卡也就失去意义。
        //
        // Never wider than half the window: any wider splits the canvas and the
        // floating card loses its point.
        let max_w = (full.width() * 0.5).max(320.0);
        egui::Area::new(egui::Id::unique("film-card"))
            .order(egui::Order::Middle)
            .fixed_pos(pos)
            .show(&ctx, |ui| timeline::card(state, ui, &ctx, max_w));
    }

    // 面板与画布之间那条呼吸带：与画布同底色的中性灰，面板内缘压一根发丝线。
    // 只画在文档区高度内，不越过菜单行 / 选项栏。
    //
    // The gutter between dock and canvas: neutral surround grey with a hairline on the
    // dock's inner edge. Drawn only across the document area, never over the menu row
    // or options bar.
    if develop && let Some((band, line_x)) = crate::layout::canvas_gutter(available, centre, insets)
    {
        let p = root.painter().clone();
        p.rect_filled(band, 0.0, surround());
        let x = line_x.round() + 0.5;
        p.line_segment(
            [Pos2::new(x, band.top()), Pos2::new(x, band.bottom())],
            Stroke::new(1.0, border()),
        );
    }

    // 工作台导航卡：浮在内容之上（design `.wsbar{position:absolute;top:56px;z-index:45}`），
    // 顶栏下 12px、水平居中。放在 Content 之后画，才压得住媒体与整页视图。
    //
    // The bench nav card floats above the content (design `.wsbar`: absolute, top 56 px,
    // z-index 45) -- 12 px under the top bar, horizontally centred. Painted after the
    // content so it sits above both the media and the full-page views.
    // 导航卡的位置按工作台分别算，避免和顶栏下面的两条带子叠压：
    // - 显影：浮在**画布**上沿内 12px、画布水平居中（设计稿 `.wsbar` 浮在媒体上）；
    // - 整页工作台：顶栏下 12px、窗口水平居中（设计稿页头中央，两侧被标题/动作占着，
    //   中间这段本来就是空的）。
    //
    // The nav card is positioned per bench so it never collides with the two strips under
    // the top bar: in develop it floats 12 px inside the canvas top, centred on the canvas
    // (the design floats `.wsbar` over the media); on the full-page benches it sits 12 px
    // under the top bar, centred in the window (the design's page-head centre, which the
    // title and actions leave free).
    let (nav_center_x, nav_top) = if develop {
        (centre.center().x, centre.top() + 12.0)
    } else {
        (full.center().x, full.top() + MENU_H + 12.0)
    };
    egui::Area::new(egui::Id::unique("bench-nav"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(nav_center_x - 300.0, nav_top))
        .show(&ctx, |ui| {
            ui.set_min_size(Vec2::new(600.0, 34.0));
            ui.set_max_width(600.0);
            bench::nav(state, ui, Pos2::new(nav_center_x, nav_top));
        });

    state.history_commit();
    // 新错误抬成弹窗：四十余处 `state.error = Some(...)` 靠这一处统一兜住。
    //
    // Promote new errors to a dialog: one place covers the forty-odd assignment sites.
    state.sync_error_dialog();
    presets_window(state, &ctx);
    about_window(state, &ctx);
    look_window(state, &ctx);
    market_window(state, &ctx);
    // 更新窗口与回滚确认：都排在最后，压在其它浮窗之上——它们是「这一版要不要换掉」
    // 这种全局问题，不该被面板挡住。
    //
    // The update window and the rollback confirmation draw last, above the other floating
    // windows: both answer a global question ("should this build be replaced") and must not
    // hide behind a panel.
    update_window(state, &ctx);
    rollback_window(state, &ctx);
    export_dialog::export_confirm_window(state, &ctx, full);
    // 错误弹窗排在最后：它是对「刚才那次操作失败」的确认，必须压住其它浮窗。
    //
    // The error dialog draws last: it acknowledges a failed action and must sit above
    // everything else.
    // 命令面板压在普通浮窗之上、错误弹窗之下：它能跳到任何窗口，
    // 但不该盖住"刚才那步失败了"这句话。
    //
    // The palette sits above ordinary windows and below the error dialog: it can reach any
    // window, but must not cover "the last step failed".
    palette::command_palette(state, &ctx);
    error_dialog(state, &ctx);
    window_resize(root, &ctx, full);

    state.ensure_preview(&ctx);
}

/// 检查器浮卡的外距：设计稿 `.dock{top:12px;right:12px;width:300px}`。
///
/// The inspector card's outer gutter, per the design's `.dock` (12 px from the edge).
const DOCK_GUTTER: f32 = 12.0;

/// 把检查器画成**浮在画布上的卡**：面板本身透明、只占位，卡片在 12px 内距里
/// 圆角 12、深投影、1px 环。
///
/// 设计稿的 `.dock` 是浮卡而不是贴边侧栏；贴边侧栏把画布切成两半，照片永远是
/// 「被面板挤窄的」，而浮卡让画面吃满整个窗口。面板宽度因此多出两侧 12px，
/// `canvas_insets` 那边同步加，画布不会被压到卡下面。
///
/// Draw the inspector as a **card floating over the canvas**: the panel itself is
/// transparent and only reserves space; the card sits in a 12 px gutter with a 12 px
/// radius, a deep shadow and a 1 px ring.
///
/// The design's `.dock` floats rather than docking to the edge -- a docked column cuts
/// the canvas in half and the photo always reads as "squeezed by the panel", while a
/// floating card lets the image fill the window. The panel is therefore two gutters
/// wider, and `canvas_insets` is widened in step so the canvas never slides under it.
fn floating_dock(root: &mut Ui, state: &mut AppState, right: bool) {
    let id = if right { "dock" } else { "dock-left" };
    let pnl = if right {
        egui::Panel::right(id)
    } else {
        egui::Panel::left(id)
    };
    pnl.exact_size(state.dock_w + DOCK_GUTTER * 2.0)
        .resizable(false)
        .frame(
            egui::Frame::NONE
                .fill(canvas())
                .inner_margin(egui::Margin::same(DOCK_GUTTER as i8)),
        )
        .show(root, |ui| {
            let card = ui.max_rect();
            let p = ui.painter().clone();
            p.add(
                egui::epaint::Shadow {
                    offset: [0, 8],
                    blur: 24,
                    spread: 0,
                    color: Color32::from_black_alpha(115),
                }
                .as_shape(card, 12.0),
            );
            p.rect_filled(card, 12.0, panel());
            p.rect_stroke(
                card,
                12.0,
                Stroke::new(1.0, Color32::from_white_alpha(15)),
                StrokeKind::Inside,
            );
            dock_panel(state, ui);
        });
}

// ---------------------------------------------------------------- 画布辅看浮层

/// 画布右下角的辅看开关：网格 / 胶片 / 曲线。
///
/// 这三个开关原先占一条 28px 竖条贴在窗口最右缘，问题有三：① 它把右坞从窗口
/// 边缘切开 28px，视觉上像被裁掉一条；② 三枚无字图标挂在面板外侧，归属不明；
/// ③ 它管的是"画布上叠加什么"，却被放进了"改什么参数"的那一列。
///
/// 改成画布内的实色浮层：常驻、一键可切、归属清楚（辅看属于画布），右坞直接
/// 贴到窗口右缘，画布还多回 28px。**必须实色**——半透明或模糊会透出图像，
/// 干扰对明度与色相的判断。
///
/// Canvas overlay toggles (grid / filmstrip / curves), bottom-right of the canvas.
///
/// They used to occupy a 28 px rail pinned to the window's right edge, which (a) cut the
/// dock off the edge, (b) left three unlabelled icons outside the panel with no visible
/// owner, and (c) put a *canvas* concern into the *parameter* column. As a solid overlay
/// they stay persistent and one-click, their owner is obvious, the dock reaches the
/// window edge, and the canvas wins 28 px back. It must be solid -- translucency or blur
/// would let the image through and bias the judgement of tone and hue.
pub(super) fn overlay_toggles(state: &mut AppState, ui: &mut Ui, content: Rect) {
    const W: f32 = 96.0;
    const H: f32 = 26.0;
    let at = Rect::from_min_size(
        Pos2::new(content.right() - W - 12.0, content.bottom() - H - 12.0),
        Vec2::new(W, H),
    );
    let p = ui.painter().clone();
    p.rect_filled(at, radius_control(), elev());
    p.rect_stroke(
        at,
        radius_control(),
        Stroke::new(1.0, divider()),
        egui::StrokeKind::Inside,
    );
    let items: [(Glyph, &str, bool); 3] = [
        (Glyph::Grid, "网格 (G) · 三分线", state.grid),
        (Glyph::Frame, crate::i18n::t!("胶片"), state.show_film),
        (Glyph::Sliders, "亮度曲线", state.show_curves),
    ];
    let mut flip = [false; 3];
    for (i, (glyph, tip, on)) in items.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(at.left() + 3.0 + i as f32 * 30.0, at.top() + 2.0),
            Vec2::new(22.0, 22.0),
        );
        let resp = ui.interact(
            r,
            ui.make_persistent_id(("overlay-toggle", i)),
            Sense::click(),
        );
        if *on {
            p.rect_filled(r, radius_control(), accent_dim());
        } else if resp.hovered() {
            p.rect_filled(r, radius_control(), elev_hover());
        }
        icons::paint(
            &p,
            r.shrink(4.0),
            *glyph,
            if *on {
                accent()
            } else if resp.hovered() {
                text()
            } else {
                dim()
            },
        );
        let clicked = resp.clicked();
        resp.on_hover_text(*tip);
        if clicked {
            flip[i] = true;
        }
    }
    if flip[0] {
        state.grid = !state.grid;
    }
    if flip[1] {
        state.show_film = !state.show_film;
    }
    if flip[2] {
        state.show_curves = !state.show_curves;
    }
}

// ---------------------------------------------------------------- 选项栏

/// 工具栏控件高度：设计图 `.chip-opt{height:28px}`，选项栏 40px 高，上下各留 6px 呼吸。
///
/// Options-bar control height: the design's `.chip-opt` is 28 px inside a 40 px bar,
/// leaving 6 px of air above and below.
const BAR_H: f32 = 28.0;
