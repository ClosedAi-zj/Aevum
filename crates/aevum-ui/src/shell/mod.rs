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
use crate::state::{AppState, Tool};
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
mod legacy;
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
/// 对外公开：`AppState::bench` 是 pub 字段，集成测试要能切到帧表工作台才会
/// 渲染那一页的表头/页脚/摘要卡，否则 i18n 覆盖 sweep 永远扫不到它们。
///
/// The numbered bench (develop / table / export / tasks): shell and state share one enum.
/// Public because `AppState::bench` is a public field and integration tests must be
/// able to switch to the Table bench; otherwise the i18n coverage sweep can never
/// render that page's header, footer and summary cards.
pub use bench::Bench;
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

/// 首帧打开「仅显示已改动」：`AEVUM_ONLY_CHANGED=1`。
///
/// 开关在检查器里，`--shot` 点不到；没有这个钩子就拍不到「过滤后只剩改动项」
/// 的证据图。只读环境变量，不落盘。
///
/// Turn on "only show changed" on the first frame: `AEVUM_ONLY_CHANGED=1`.
///
/// The switch lives in the inspector and `--shot` cannot click it; without this hook
/// there is no screenshot proving the filter drops untouched rows. Environment only.
fn debug_only_changed(state: &mut AppState) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    if std::env::var_os("AEVUM_ONLY_CHANGED").is_some() {
        state.only_changed = true;
    }
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
    debug_only_changed(state);
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
    // 老式界面布局是可选方案：默认 `Modern` 时下面两条 Panel 一律不声明，
    // 现代路径（浮卡检查器 + 胶片卡）一行不改。
    //
    // The legacy arrangement is opt-in: with the default `Modern` neither Panel
    // below is declared, and the modern path (floating inspector + filmstrip
    // card) stays byte-for-byte the same.
    let legacy = layout.scheme == crate::layout::Scheme::Legacy;

    // 1 标题栏：品牌 │ 五项菜单 │ 工作区胶囊 │ 导出 │ 窗口按钮，一行到底。
    //   底色对齐界面稿 ttl-bg（比 chrome 暗半档、贴着 header）：标题栏是"骨架"，
    //   不是内容面板，不能跟下面的选项栏同层。
    egui::Panel::top("menu-row")
        .exact_size(platform::chrome().titlebar_h)
        .frame(egui::Frame::NONE.fill(bar()))
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
    // 工具选项栏默认收起（设计稿没有这条带子）；换成任何非手型/缩放工具时自动出现，
    // 否则裁剪的「应用/取消」会无处可点。
    //
    // The options bar is folded away by default (the design has no such strip) and comes
    // back for any tool other than hand/zoom -- otherwise crop's apply/cancel would have
    // nowhere to live.
    let tool_needs_bar = !matches!(
        state.tool,
        crate::state::Tool::Hand | crate::state::Tool::Zoom
    );
    if develop && (state.show_options || tool_needs_bar) {
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
    if develop && state.show_flow && state.frame_count() > 1 {
        egui::Panel::top("flow-top")
            .exact_size(34.0)
            // 去悬浮贴纸化（用户 2026-11-11 二次反馈）：面板**全宽平底**——
            // 与页签行同一材质（header）+ 底缘 hairline，读作窗口结构而非
            // 浮在画布上的胶囊。左右内距归零，步骤段自己留呼吸。
            //
            // De-stickered: the flow bar is now a full-width flat strip with the
            // same header material and a bottom hairline -- window structure, not a
            // floating pill over the canvas. Insets are zero; segments keep their own
            // breathing room.
            .frame(
                egui::Frame::NONE
                    .fill(header())
                    .stroke(egui::Stroke::new(1.0, border_strong()))
                    .inner_margin(egui::Margin::symmetric(0, 0)),
            )
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
            .frame(egui::Frame::NONE.fill(bar()))
            .show(root, |ui| status_bar(state, ui));
    }

    // 老式布局两处贴边区（用户可选）：右侧一列调色盘 + 底部一条铺满的图片列表。
    // 顺序即层级 —— right 先声明贯穿整个高度，bottom 后声明只占剩余宽度的底边，
    // 于是图片列表横贯到调色盘左缘，正是旧版稿 `.film` 的样子（用户实测驳回
    // 「左下角一小块浮卡、右边空一大片」）。两者都复用既有组件，不新增控件：
    // 调色盘用 `dock_panel`，图片列表用 `timeline::card` 的贴边形态。
    //
    // Two Legacy docked regions (opt-in): a right-hand palette column plus a
    // bottom image list spanning the full width. Declaration order sets the
    // layering -- `right` is declared first and runs the full height, then
    // `bottom` takes the remaining width, so the thumbnails reach the palette's
    // left edge, matching the old mockup's `.film`. The user rejected the
    // "small floating card in the corner with a large empty gap". Both reuse
    // existing widgets: `dock_panel` and the docked form of `timeline::card`.
    if develop && legacy {
        let palette_w = state.dock_w;
        egui::Panel::right("legacy-palette")
            .exact_size(palette_w)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(dock()))
            .show(root, |ui| {
                legacy::palette_column(state, ui, palette_w, ui.available_height())
            });
        if layout.film.allowed() && state.frame_count() > 0 {
            egui::Panel::bottom("legacy-film")
                .exact_size(timeline::card_height(state))
                .resizable(false)
                .frame(egui::Frame::NONE.fill(panel()))
                .show(root, |ui| {
                    legacy::film_strip(state, ui, &ctx);
                });
        }
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
        // 工具轨：设计稿没有这两根 56px 竖轨，默认收起（视图菜单可开）。
        // 轨上是工具组与面板入口；收起后工具仍可快捷键切换（H 抓手 / Z 缩放 …），
        // 面板入口在 ☰ 与检查器页签里各有一份。
        //
        // Rails: the design has no 56 px tool columns, so they start folded (the View menu
        // brings them back). They carry the tool groups and panel entries; with them folded
        // the tools still switch by keyboard (H hand / Z zoom ...), and the panel entries
        // live in ☰ and in the inspector tabs.
        if state.show_rails {
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
        }
        // 检查器：贴窗口右缘的浮卡，与工具轨开不开无关。
        //
        // The inspector: a card floating at the window's right edge, independent of the
        // rails' visibility.
        // 老式布局的调色盘已经是贴边 Panel，再画一遍浮卡就成了两层调色盘。
        //
        // In Legacy the palette is already a docked Panel; painting the floating
        // card on top would give two palettes stacked over each other.
        if state.show_dock && !legacy {
            floating_dock(&ctx, state, !dock_left, full);
        }
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
    // 画布让位：**零**。检查器已经是浮卡（`floating_dock`），不再占布局宽度，
    // 照片铺到窗口右缘、卡片浮在照片上——这正是设计稿 `.dock` 的读法。让位量仍
    // 走 `layout::canvas_insets`（单源不变），只是「有没有占位侧栏」= 没有。
    //
    // Canvas insets: **zero**. The inspector is a floating card now (`floating_dock`),
    // it no longer takes layout width, so the photo runs to the window's right edge with
    // the card floating over it -- exactly the design's `.dock`. The insets still come
    // from `layout::canvas_insets` (one source), only "a real column reserves space" is
    // now false.
    let insets = crate::layout::canvas_insets(false, dock_left, 0.0);
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
    // 胶片卡（现代布局的左下浮卡）。老式改由 `legacy-film` 贴边 Panel 铺满底边，
    // 这里再画一次就成了两条图片列表，故老式跳过。
    //
    // Filmstrip card for Modern (floating at the lower left). Legacy uses the
    // `legacy-film` docked Panel across the bottom edge instead; painting both
    // would duplicate the image list, so Legacy skips this one.
    if develop && !legacy && layout.film.allowed() && state.frame_count() > 0 {
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
            .show(&ctx, |ui| timeline::card(state, ui, &ctx, max_w, false));
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
    // 层级只到 Middle：中央面板在 Background，Middle 已经压得住它；而设置窗口、
    // 错误弹窗这些 `egui::Window` 也是 Middle，且在下面更晚绘制，于是浮窗反压导航卡。
    // 早先钉在 Foreground，会把「外观与布局」的标题栏盖掉——标题栏正是拖动区，
    // 用户因此看到「子窗口被覆盖、拖不动」（2026-11-10 实测截图）。
    //
    // The bench nav card floats above the content (design `.wsbar`: absolute, top 56 px,
    // z-index 45) -- 12 px under the top bar, horizontally centred. Painted after the
    // content so it sits above both the media and the full-page views.
    //
    // It stops at Middle: the central panel is Background, which Middle already covers,
    // while the `egui::Window`s (settings, error dialog) are also Middle but painted
    // later below, so they outrank the card. Pinning it to Foreground used to bury the
    // settings window's title bar -- its drag handle -- making the sub-window look
    // covered and impossible to move (reproduced in the 2026-11-10 screenshot).
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
        .order(egui::Order::Middle)
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

/// 浮卡圆角（设计稿 `border-radius:12px`）。
///
/// The floating card's corner radius (the design's `border-radius: 12px`).
/// 检查器浮卡四角：一律从 `radius_panel()` 派生，别写死。
///
/// 这里曾是 `const dock_radius(): f32 = 12.0`——「外观 → 圆角」调了半天，
/// 主窗口（走 `radius_window()`）会变、浮卡却纹丝不动，用户实测两轮都是
/// 「子窗口不生效」。默认 `appearance.radius = 8` 时 `radius_panel()` 正好
/// 也是 12，接回派生链观感不变，但这一档终于跟着旋钮一起走。
///
/// The inspector card's corners derive from `radius_panel()`, never a literal.
/// A hard-coded 12.0 used to leave the floating card frozen while the main window
/// (via `radius_window()`) followed the corner-radius knob -- which is exactly the
/// "sub-window doesn't respond" bug users reported twice. At the default
/// `appearance.radius = 8`, `radius_panel()` is also 12, so the look is unchanged.
fn dock_radius() -> f32 {
    radius_panel()
}
/// 检查器浮卡的最小高度：约等于目标图显示到「色彩分级」分组的高度。
///
/// Minimum height of the floating inspector: about as tall as the reference that
/// shows down to the Colour grading group.
const DOCK_MIN_H: f32 = 470.0;
/// 检查器浮卡的最小宽度：低于它推子行的「标签 + 读数」会把轨道吃光。
///
/// Minimum width of the floating inspector: below it the slider row's label and
/// readout eat the rail.
const DOCK_MIN_W: f32 = 300.0;

/// 把检查器画成**浮在照片上的卡**：宽 300（`state.dock_w`）、默认高随内容，
/// 默认贴窗口右缘 12px、顶栏下 12px；页签行右侧的把手可拖动整卡，右下角
/// 手柄可同时改宽与高（用户要求「不要固定这个框、可以改大小、可以移动」）。
///
/// 设计稿的 `.dock` 是 `position:absolute; top:12px; right:12px; width:300px;
/// max-height:calc(100% - 24px); overflow:auto` —— 一张**浮卡**，不是贴边侧栏。
/// 侧栏要占布局宽度，照片永远「被面板挤窄」；浮卡让照片铺满整个窗口，卡片自己
/// 浮在照片的右边一窄条上，高度只到内容为止（内容再长就在卡内滚，卡不变高）。
///
/// 实现要点：
/// * 位置：默认右缘 `right-12-width`、顶边 `MENU_H+12`；拖过把手后用
///   `state.dock_pos`，每帧夹在窗口内留 12px 边距；
/// * 高度：默认随内容（上限 `窗口高-MENU_H-24`）；拖过右下角手柄后钉在
///   `state.dock_h`（最小 470），ScrollArea 上限随它，内容不够则卡身留白；
/// * 拖动把手是页签行右侧的独立小矩形、独立 id，不拦卡内推子/按钮/页签；
/// * 底色先占一个 `Shape::Noop` 槽位、画完内容再回填——卡高是布局之后才知道的，
///   底色若跟着内容画就会压在文字上；
/// * 拖动推子时整卡降到 82%（`slider-drag` 临时数据），松手立刻回到实色。
///
/// Draw the inspector as a **card floating over the photo**: 300 wide (`state.dock_w`),
/// as tall as its content by default, 12 px from the window's right edge and 12 px under
/// the top bar. A handle in the tab row moves it and a corner handle resizes it (the user
/// asked for a card that is "not fixed, resizable, movable, with a minimum size").
///
/// The design's `.dock` is `position:absolute; top:12px; right:12px; width:300px;
/// max-height:calc(100% - 24px); overflow:auto` -- a **floating card**, not a docked
/// column. A docked column takes layout width, so the photo always reads as "squeezed by
/// the panel"; the card lets the photo fill the window and floats over its right sliver,
/// growing only as tall as its content (beyond that it scrolls inside and stays put).
fn floating_dock(ctx: &egui::Context, state: &mut AppState, right: bool, full: Rect) {
    let w = state.dock_w;
    // 高度上限：窗口高减顶栏、再减上下两道 12px（设计稿 `max-height:calc(100% - 24px)`
    // 是相对 `.dock` 的定位父级说的；这里父级是标题栏以下的内容区）。
    //
    // Height cap: window height minus the top bar and the two 12 px gutters (the design's
    // `max-height: calc(100% - 24px)` is relative to `.dock`'s containing block; here that
    // block starts under the title bar).
    let win_max_h = (full.height() - MENU_H - DOCK_GUTTER * 2.0).max(120.0);
    // `dock_h` 的 `0.0` 哨兵 = 跟随内容；拖过右下角手柄后是用户钉住的高度。
    // 传给 dock_panel 的 cap 决定 ScrollArea 上限：用户设过就用它，内容能一次
    // 显示到那么高，否则沿用窗口上限。
    //
    // `0.0` in `dock_h` means "follow content"; after dragging the corner handle it is
    // the height the user pinned. The cap handed to `dock_panel` bounds the ScrollArea:
    // a pinned height shows that much content at once, otherwise the window cap.
    let user_h = (state.dock_h >= DOCK_MIN_H).then_some(state.dock_h.min(win_max_h));
    // 默认上限 = 窗口高减去右下角视图工具条的预留带。按「比例砍」会把内容
    // 截在组中间（色彩分级/更多分组看不见）；真正的约束只是卡片底边不能压住
    // 工具条。窗口够高时内容一次显示完，窗口矮时卡内滚动。用户拖过 resize
    // 手柄后以他钉住的高度为准（仍夹在窗口内）。
    //
    // Default cap = window height minus the bottom-right view toolbar's reserve.
    // A percentage cut chops content in the middle of a group; the real constraint
    // is only that the card's bottom edge must not cover the toolbar. Tall windows
    // show everything at once, short ones scroll inside the card. A user-pinned
    // height still wins (clamped to the window).
    const VIEWTOOLS_RESERVE: f32 = 60.0;
    let default_cap = (win_max_h - VIEWTOOLS_RESERVE).max(DOCK_MIN_H);
    let cap = user_h.unwrap_or(default_cap);

    let default_pos = Pos2::new(
        if right {
            full.right() - DOCK_GUTTER - w
        } else {
            full.left() + DOCK_GUTTER
        },
        full.top() + MENU_H + DOCK_GUTTER,
    );
    // 位置每帧夹在窗口内（留 12px）：下沿按「钉住的高度或最小高度」估计，
    // 用户拖到角落也不会把整卡推出屏幕。
    //
    // Clamp the position inside the window (12 px gutter) every frame: the bottom
    // edge is estimated from "pinned height or min height", so dragging into a corner
    // can never push the whole card off screen.
    let h_est = user_h.unwrap_or(DOCK_MIN_H);
    let min_pos = Pos2::new(full.left() + DOCK_GUTTER, full.top() + MENU_H + DOCK_GUTTER);
    let max_pos = Pos2::new(
        (full.right() - DOCK_GUTTER - w).max(min_pos.x),
        (full.bottom() - DOCK_GUTTER - h_est).max(min_pos.y),
    );
    let pos = state
        .dock_pos
        .map(|p| {
            Pos2::new(
                p.x.clamp(min_pos.x, max_pos.x),
                p.y.clamp(min_pos.y, max_pos.y),
            )
        })
        .unwrap_or(default_pos);

    let id = if right { "dock" } else { "dock-left" };
    egui::Area::new(egui::Id::unique(id))
        .order(egui::Order::Middle)
        // 整卡不交给 Area 拖动：那会让卡内任意一次按下都可能变成移窗口。
        // 拖动只认页签行右侧那枚独立把手，见下方 `dock-drag`。
        //
        // Do not let Area drag the whole card: that would turn any press inside the
        // card into "move the window". Only the small handle in the tab row drags.
        .movable(false)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            ui.set_width(w);
            ui.set_max_height(cap);
            // 卡片底色/投影/描边先占槽位，内容画完再回填：`Painter::set` 写回的是
            // 同一个绘制序号，所以它们仍排在内容**之下**，但能拿到最终高度。
            //
            // Reserve the slot for the card's fill / shadow / ring, and write it back once
            // the content is laid out: `Painter::set` targets the same shape index, so the
            // paint still lands *under* the content while knowing the final height.
            //
            // 槽位用**裁剪前**的画笔：投影要糊到卡外 24px，卡内的裁剪框会把它切平。
            //
            // The slot is added with the **pre-clip** painter: the shadow blurs 24 px past
            // the card, and the card's clip rect would cut it flat.
            let wide = ui.painter().clone();
            let bg = wide.add(egui::Shape::Noop);
            // 卡内内容一律裁在卡的边界里：浮卡没有外层 Panel 兜底，一段超宽文案
            // 会直接画到照片上（贴边侧栏时代由 Panel 的裁剪框挡住）。裁剪框同时
            // 是判据 `dock_fit` 认「这段文字属于检查器」的依据。
            //
            // Clip everything inside the card to the card's bounds: a floating card has no
            // enclosing panel to fall back on, so one over-wide caption would paint straight
            // onto the photo (the docked column was covered by the panel's clip rect). That
            // clip rect is also what `dock_fit` uses to tell "this text belongs to the dock".
            ui.set_clip_rect(Rect::from_min_size(
                ui.min_rect().min,
                Vec2::new(w, win_max_h),
            ));
            dock_panel(state, ui, w, cap);
            let mut card = ui.min_rect();
            // 钉住的高度：内容不够就把卡身补到那么高（留白），ScrollArea 已按
            // cap 限制内容，不会溢出。
            //
            // Pinned height: when content is shorter, pad the card up to that height;
            // the ScrollArea already caps the content so nothing spills out.
            if let Some(h) = user_h {
                card = Rect::from_min_size(card.min, Vec2::new(w, card.height().max(h)));
            }

            // ---- 页签行空白区：整段可拖 + 还原/放大按钮（用户 2026-11-10）----
            // 此前整卡只有页签行右端 22px 暗点把手能拖——用户按住卡片拖不动，
            // 读成「调色板不能移动」。页签改为内容宽右对齐后，左端空出的槽位
            // 放一颗明确的「放大 / 还原」钮，按钮右侧到页签左缘的空白整段注册
            // 为拖动区；缩小之后不用去找右下角的缩放手柄，一键回默认尺寸与位置。
            // 旧暗点把手已删：拖动由本区承担，省回的 22px 让给页签全名。
            //
            // ---- Tab-strip blank: draggable as a whole + restore button ----
            // Only a 22 px dotted grip at the strip's right end could move the card,
            // so dragging the card body read as "the panel cannot be moved". With
            // content-sized right-aligned tabs, the slot at the strip's left end holds
            // an explicit maximise / restore button and the blank between it and the
            // first tab is one drag zone; a shrunken card grows back without hunting
            // for the corner handle. The old dotted grip is gone: this zone owns the
            // dragging, and its 22 px go back to full tab labels.
            let widths = dock::tab_widths(&ui.painter().clone(), w, !state.keyframes.is_empty());
            let tabs_left = card.right() - dock::TAB_RESERVE_W - widths.iter().sum::<f32>();
            let zone = Rect::from_min_max(
                Pos2::new(card.left() + dock::TAB_SLOT_W + 2.0, card.top() + 2.0),
                Pos2::new(
                    (tabs_left - 4.0).max(card.left() + dock::TAB_SLOT_W + 6.0),
                    card.top() + 26.0,
                ),
            );
            let strip_drag = ui.interact(
                zone,
                ui.make_persistent_id("dock-strip-drag"),
                Sense::click_and_drag(),
            );
            if strip_drag.dragged() {
                state.dock_pos = Some(Pos2::new(
                    (pos.x + strip_drag.drag_delta().x).clamp(min_pos.x, max_pos.x),
                    (pos.y + strip_drag.drag_delta().y).clamp(min_pos.y, max_pos.y),
                ));
            }
            if strip_drag.hovered() || strip_drag.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                wide.rect_filled(
                    zone,
                    dock_radius(),
                    Color32::from_white_alpha(if strip_drag.dragged() { 14 } else { 8 }),
                );
            }
            strip_drag.on_hover_text(crate::i18n::t!("拖动检查器浮卡"));

            // 还原 / 放大：钉住过（拖过位置或高度）就回默认；在默认位就钉满
            // 可用高度。四角括号图标两态共用，动作由 tooltip 说明。
            //
            // Restore / maximise: a pinned card (moved or height-pinned) returns to
            // default; a default card pins to the full available height. One
            // corner-bracket icon for both states; the tooltip names the action.
            let pinned = state.dock_pos.is_some() || state.dock_h >= DOCK_MIN_H;
            let btn = Rect::from_min_size(
                Pos2::new(card.left() + 4.0, card.top() + 5.0),
                Vec2::splat(18.0),
            );
            let br = ui.interact(btn, ui.make_persistent_id("dock-restore"), Sense::click());
            if br.hovered() {
                wide.rect_filled(btn, radius_chip(), elev());
            }
            let ink = if br.hovered() { text() } else { dim() };
            // 四角括号：↖ 与 ↘ 两只 L，读作「调整尺寸」。
            //
            // Corner brackets: two L hooks reading as "resize".
            let a = 5.0;
            for (cx, cy, dx, dy) in [
                (btn.left(), btn.top(), 1.0, 1.0),
                (btn.right(), btn.bottom(), -1.0, -1.0),
            ] {
                wide.line_segment(
                    [Pos2::new(cx + dx * a, cy), Pos2::new(cx, cy)],
                    Stroke::new(1.2, ink),
                );
                wide.line_segment(
                    [Pos2::new(cx, cy), Pos2::new(cx, cy + dy * a)],
                    Stroke::new(1.2, ink),
                );
            }
            if br.clicked() {
                if pinned {
                    state.dock_pos = None;
                    state.dock_h = 0.0;
                    state.dock_w = DOCK_MIN_W;
                } else {
                    state.dock_h = default_cap;
                }
            }
            br.on_hover_text(crate::i18n::t!("还原检查器大小与位置"));

            // ---- 右下角 14px resize 手柄：同时改宽与高 ----
            //
            // 14 px corner resize handle: changes width and height together.
            let corner = Rect::from_min_size(
                Pos2::new(card.right() - 14.0, card.bottom() - 14.0),
                Vec2::new(14.0, 14.0),
            );
            let rz = ui.interact(
                corner,
                ui.make_persistent_id("dock-corner-resize"),
                Sense::drag(),
            );
            let rz_delta = rz.drag_delta();
            let rz_on = rz.dragged();
            let rz_hover = rz.hovered();
            if rz_hover || rz_on {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
            }
            // 三道斜线角标（常见的窗口缩放角）。
            //
            // Three diagonal ticks (the usual window-resize corner).
            for k in 0..3 {
                let t = 3.0 + k as f32 * 4.0;
                wide.line_segment(
                    [
                        Pos2::new(corner.right() - 2.0 - t, corner.bottom() - 2.0),
                        Pos2::new(corner.right() - 2.0, corner.bottom() - 2.0 - t),
                    ],
                    Stroke::new(1.0, if rz_on { accent() } else { dim() }),
                );
            }
            if rz_on {
                // 第一帧把「跟随内容」固化成当前卡高，避免高度从 0 突然跳到 470。
                //
                // On the first dragged frame, freeze "follow content" to the current
                // card height so it does not jump from 0 straight to 470.
                let base_h = if state.dock_h >= DOCK_MIN_H {
                    state.dock_h
                } else {
                    card.height()
                };
                state.dock_h = (base_h + rz_delta.y).clamp(DOCK_MIN_H, win_max_h);
                let w_max = 560.0_f32
                    .min(full.width() - DOCK_GUTTER * 2.0)
                    .max(DOCK_MIN_W);
                state.dock_w = (state.dock_w + rz_delta.x).clamp(DOCK_MIN_W, w_max);
            }
            rz.on_hover_text(crate::i18n::t!("拖动调整检查器大小"));

            // 帧尾用**实际卡高**把位置夹死并写回：帧首那次 clamp 用的是估计
            // 高度（内容高要画完才知道），这里补成精确值，整卡才真的留 12px 边距。
            //
            // At frame end, clamp with the **actual card height** and write it back:
            // the frame-start clamp used an estimate (content height is only known
            // after layout), so this makes the 12 px gutter exact.
            if let Some(p) = state.dock_pos {
                let max_y = (full.bottom() - DOCK_GUTTER - card.height()).max(min_pos.y);
                state.dock_pos = Some(Pos2::new(
                    p.x.clamp(min_pos.x, max_pos.x),
                    p.y.clamp(min_pos.y, max_y),
                ));
            }

            // 拖动推子时半透明：手在滑块上，眼睛要看得见画面怎么变。平时是不透明
            // 实色（色彩判断的参照系不能被照片染色）；一旦按住任何控件，卡片降到
            // 82% 让照片透出来，松手立刻回到实色。
            //
            // Translucent while dragging: the hand is on a slider, the eye needs to watch
            // the frame change. At rest the card is opaque (the colour-judgement reference
            // must not be tinted by the photo); while any control is held it drops to 82 %
            // so the photo shows through, and snaps back to opaque on release.
            let base = panel();
            let dragging = ui
                .ctx()
                .data(|d| d.get_temp::<bool>(egui::Id::unique("slider-drag")))
                .unwrap_or(false);
            let fill = if dragging {
                Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 210)
            } else {
                base
            };
            let shadow = egui::epaint::Shadow {
                offset: [0, 8],
                blur: 24,
                spread: 0,
                color: Color32::from_black_alpha(115),
            }
            .as_shape(card, dock_radius());
            // 1px 内描边用 `group_ring()`：与参数分组卡同一枚环，卡片才有同一层
            // 材质感；外描边会在深底上多出一圈亮边。
            //
            // The 1 px ring uses `group_ring()`: the same ring as the parameter group
            // cards, so the card shares their material; an outside stroke would add a
            // bright halo on the dark backdrop.
            let body = egui::epaint::RectShape::new(
                card,
                dock_radius(),
                fill,
                Stroke::new(1.0, group_ring()),
                StrokeKind::Inside,
            );
            wide.set(
                bg,
                egui::Shape::Vec(vec![egui::Shape::Rect(shadow), egui::Shape::Rect(body)]),
            );
            // 短窗内容被截断时，细滚动条在深底上几乎看不见：再叠一条 12px
            // 由透明到卡底色的渐隐带，明确告诉用户「下面还有分组」。内容能
            // 完整显示时不画，避免凭空多一条灰带。
            //
            // In short windows the thin scrollbar is nearly invisible on a dark
            // surface: add a 12 px transparent-to-card fade at the bottom to say
            // "there are more groups below". Not drawn when everything fits.
            if card.height() >= cap - 2.0 {
                let overlay = ui.painter().clone();
                for k in 0..4 {
                    let alpha = ((k + 1) as f32 / 4.0 * 225.0) as u8;
                    let band = Rect::from_min_size(
                        Pos2::new(card.left(), card.bottom() - 12.0 + k as f32 * 3.0),
                        Vec2::new(card.width(), 3.0),
                    );
                    overlay.rect_filled(
                        band,
                        0.0,
                        Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha),
                    );
                }
            }
        });
}

// ---------------------------------------------------------------- 画布视图工具条

/// 画布右下角的浮动视图工具条（设计稿 `.viewtools`）。
///
/// 设计稿 `docs/aevum-v10-complete-refined.html:197` 定义
/// `.viewtools{right:12px;bottom:12px;display:flex;gap:2px;padding:3px}`、
/// `.viewtools .ibtn{width:24px;height:24px}` —— 一条深黑胶囊（圆角约 18）、
/// 细边框、投影，内含 5 枚 14px 图标，选中态底部 2px 蓝色条。这条工具条在
/// 早前的实现里丢了（右下角只剩三枚辅看开关），这里按设计稿捡回。
///
/// 5 枚每枚都接一个真实状态，不许放假按钮：
/// * 网格 → `state.grid`（三分线叠加开关，原辅看浮层的同款实现）；
/// * 左右分割 → `state.set_tool(Tool::Compare)` + `state.split`（前后对比）；
/// * ∧ → `state.show_film`（折叠 / 展开胶片条，与胶片卡头行同款）；
/// * 矩形框 → `state.canvas_fill`（cover / contain）；
/// * 减号 → `state.show_viewtools = false`（收起本条，☰「视图」可重新打开）。
///
/// 底必须实色：半透明或模糊会透出照片，干扰对明度与色相的判断（浮层纪律）。
/// 与设计稿 `.is-fill` 时隐藏工具条不同，这里常驻——减号是唯一的收起入口，
/// 否则切到 canvas_fill 后用户再也点不回来。
///
/// The floating view toolbar at the canvas' bottom-right (the design's `.viewtools`).
/// The design defines `right:12px;bottom:12px;display:flex;gap:2px;padding:3px` with
/// 24x24 icon buttons -- a dark capsule (radius ~18), hairline ring and a shadow, five
/// 14 px glyphs, and a 2 px blue bar under the active ones. It had been lost (only a
/// three-icon overlay remained); this brings it back to the design.
///
/// Every one of the five drives a real state, no dummy buttons:
/// * grid -> `state.grid` (thirds overlay, same toggle as the old overlay);
/// * split -> `state.set_tool(Tool::Compare)` + `state.split` (before/after);
/// * chevron -> `state.show_film` (collapse / expand the filmstrip);
/// * frame -> `state.canvas_fill` (cover / contain);
/// * minus -> `state.show_viewtools = false` (hide; the View menu reopens it).
///
/// The fill must be opaque: translucency or blur would let the photo through and bias
/// tone / hue judgement. Unlike the design's `.is-fill` rule that hides the toolbar,
/// this stays put -- the minus button is the only way to hide it, otherwise switching
/// canvas_fill on would make it impossible to switch back.
pub(super) fn view_tools(state: &mut AppState, ui: &mut Ui, content: Rect) {
    if !state.show_viewtools {
        return;
    }
    // 设计稿 `.viewtools`：5 枚 24×24、gap 2、内距 3、圆角 18。
    //
    // 圆角同检查器浮卡一起从 `radius_panel()` 派生（默认 `radius_panel()=12`，
    // +6 即设计稿的 18）：写死 18 会让这条工具条在调圆角时独自不动。
    //
    // The design's `.viewtools`: five 24x24 buttons, gap 2, padding 3, radius 18.
    // The radius derives from `radius_panel()` just like the inspector card
    // (default 12, +6 gives the design's 18): a literal 18 would leave this strip
    // frozen while every other corner follows the knob.
    const ICON: f32 = 24.0;
    const GAP: f32 = 2.0;
    const PAD: f32 = 3.0;
    let radius = radius_panel() + 6.0;
    let w = ICON * 5.0 + GAP * 4.0 + PAD * 2.0;
    let h = ICON + PAD * 2.0;
    let at = Rect::from_min_size(
        Pos2::new(content.right() - 12.0 - w, content.bottom() - 12.0 - h),
        Vec2::new(w, h),
    );
    let p = ui.painter().clone();
    // 投影：offset [0,6] / blur 18 / black_alpha(100)，先画再铺底。
    //
    // Shadow: offset [0,6], blur 18, black_alpha(100), painted before the fill.
    let shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(100),
    }
    .as_shape(at, radius);
    p.add(egui::Shape::Rect(shadow));
    // 实色 panel 底 + 与另两张浮卡同款的 1px 描边 / 上沿光。
    //
    // Opaque `panel` fill plus the same 1 px outline / top edge light as the
    // other two floating cards.
    p.rect_filled(at, radius, panel());
    // 四张浮卡（检查器 / 直方图 / 胶片 / 视图工具条）必须共享同一套材质。
    // 这条贴在暗部照片上时 6% 白环会被吞掉，读成「一块黑斑」；换成与其它卡
    // 相同的 border_strong 描边 + 1px 上沿光，边界才在任意底图上都成立。
    //
    // All four floating cards must share one material. Against a dark part of the
    // photo a 6% white ring disappears and reads as a black smudge; the same
    // border_strong outline plus the 1 px top edge light keeps the rim legible
    // over any image.
    p.rect_stroke(
        at,
        radius,
        Stroke::new(1.0, border_strong()),
        egui::StrokeKind::Inside,
    );
    p.line_segment(
        [
            Pos2::new(at.left() + radius, at.top() + 0.5),
            Pos2::new(at.right() - radius, at.top() + 0.5),
        ],
        Stroke::new(1.0, Color32::from_white_alpha(20)),
    );

    // 5 枚：图标 / tooltip / 选中态。tooltip 写明各自接的真实状态。
    //
    // Five buttons: glyph / tooltip / active state. Each tooltip names its real state.
    let items: [(Glyph, &str, bool); 5] = [
        (
            Glyph::Grid,
            crate::i18n::t!("网格叠加：开 / 关（三分线）"),
            state.grid,
        ),
        (
            Glyph::Compare,
            crate::i18n::t!("前后对比：开 / 关（左右分割）"),
            state.tool == Tool::Compare,
        ),
        (
            Glyph::ChevronUp,
            crate::i18n::t!("折叠 / 展开胶片条"),
            state.show_film,
        ),
        (
            Glyph::Frame,
            crate::i18n::t!("画布铺满：开 / 关（cover / contain）"),
            state.canvas_fill,
        ),
        (
            Glyph::Minus,
            crate::i18n::t!("收起视图工具条（视图菜单可重新打开）"),
            false,
        ),
    ];
    let mut flip = [false; 5];
    for (i, (glyph, tip, on)) in items.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(at.left() + PAD + i as f32 * (ICON + GAP), at.top() + PAD),
            Vec2::new(ICON, ICON),
        );
        let resp = ui.interact(r, ui.make_persistent_id(("viewtool", i)), Sense::click());
        // 背景层必须画在图标之前：选中态填充若后画会盖住 glyph，短窗里就读成
        // 一块没有图标的深色方块。选中优先于 hover，只铺一次。
        //
        // The background layer must precede the glyph: painting the active fill
        // afterwards covers the icon and reads as a blank dark square in short
        // windows. Active wins over hover, painted once.
        if *on {
            p.rect_filled(r, radius_control(), elev());
        } else if resp.hovered() {
            p.rect_filled(r, radius_control(), elev_hover());
        }
        // 14px 图标：24 的槽里各边收 5。
        //
        // 14 px glyph: inset 5 on each side of the 24 px slot.
        icons::paint(
            &p,
            r.shrink(5.0),
            *glyph,
            if *on {
                accent()
            } else if resp.hovered() {
                text()
            } else {
                dim()
            },
        );
        // 选中态：底部 2px 蓝色条（设计稿 `.ibtn.on{box-shadow:inset 0 -2px 0 var(--blue)}`）。
        //
        // Active state: a 2 px blue bar at the bottom (the design's `.ibtn.on`).
        if *on {
            // 选中态的抬升底已在背景层铺过；这里只补蓝弧。
            //
            // The raised active fill is already in the background layer; only the
            // blue arc remains here.
            // 效果图是短而圆头的蓝弧：收窄到 12px、圆角 3，比一条通宽的直条
            // 更像「选中」而不是「进度」。
            //
            // The reference shows a short, rounded blue arc: 12 px wide with a 3 px
            // radius reads as "selected" instead of "progress".
            let bw = 12.0_f32.min(r.width() - 6.0);
            let bar = Rect::from_min_size(
                Pos2::new(r.center().x - bw * 0.5, r.bottom() - 2.0),
                Vec2::new(bw, 2.0),
            );
            p.rect_filled(bar, 3.0, accent());
        }
        let clicked = resp.clicked();
        resp.on_hover_text(*tip);
        if clicked {
            flip[i] = true;
        }
    }
    // 统一改状态：`items` 是状态快照，循环里同时写 state 会让同一帧的选中态
    // 与点击结果对不上，故先收集再落地。
    //
    // Apply the flips after the loop: `items` is a snapshot, so writing state inside
    // the loop would desync a button's painted state from its click in the same frame.
    if flip[0] {
        state.grid = !state.grid;
    }
    if flip[1] {
        if state.tool == Tool::Compare {
            state.set_tool(Tool::Hand);
            state.split = false;
        } else {
            state.set_tool(Tool::Compare);
            state.split = true;
        }
    }
    if flip[2] {
        state.show_film = !state.show_film;
    }
    if flip[3] {
        state.canvas_fill = !state.canvas_fill;
    }
    if flip[4] {
        state.show_viewtools = false;
    }
}

// ---------------------------------------------------------------- 选项栏

/// 工具栏控件高度：设计图 `.chip-opt{height:28px}`，选项栏 40px 高，上下各留 6px 呼吸。
///
/// Options-bar control height: the design's `.chip-opt` is 28 px inside a 40 px bar,
/// leaving 6 px of air above and below.
const BAR_H: f32 = 28.0;
