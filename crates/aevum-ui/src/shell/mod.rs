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

use egui::{Pos2, Rect, Sense, Stroke, Ui, Vec2};

use self::canvas::canvas_panel;
use self::chrome::window_resize;
use self::dock::dock_panel;
use self::menu::handle_input;
use self::menu::menu_row;
use self::timeline::timeline;
use self::toolbar::options_bar;
use self::toolbar::status_bar;
use self::toolbar::status_bar_needed;
use self::toolbar::tool_rail;
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
mod canvas;
mod chrome;
mod common;
mod dialogs;
mod dock;
pub(crate) mod flow;
mod flow_panel;
mod library;
mod markdown;
mod mask;
mod menu;
mod palette;
mod tabs;
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
    debug_apply_flow(state);
    debug_apply_window(state);
    debug_export_confirm(&ctx);
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
    // 底色用 dock 而不是 chrome（B0 裁决）：选项栏服务于右侧参数面板，与面板同层
    // 才读得出「骨架（菜单行/状态栏，header）→ 内容（选项栏/dock，dock）→ 控件
    // （elev）」三级；chrome 让给工具轨，形成「外框稍亮、内容沉下去」的 ACR 结构。
    //
    // The options bar uses `dock` rather than `chrome` (B0 ruling): it serves the
    // parameter panel on the right, so sitting on the same layer makes the three-level
    // reading work -- skeleton (menu/status bar, header) -> content (options bar and
    // dock) -> controls (elev). `chrome` is left to the tool rail, so the outer frame
    // reads slightly brighter than the content, as in ACR.
    egui::Panel::top("options-bar")
        .exact_size(OPTIONS_H)
        .frame(egui::Frame::NONE.fill(dock()).inner_margin(egui::Margin {
            left: 8,
            right: 8,
            top: 0,
            bottom: 0,
        }))
        .show(root, |ui| options_bar(state, ui, &ctx));

    // 3 底部两行：**先声明状态栏**，它才拿得到窗口最底边；胶片条压在它上面。
    //   反过来声明就成了"状态栏飘在胶片条之上"，与界面稿差出整整一行。
    //   没有告警 / 任务时整条不声明——底边留白还回来（用户 2026-11-08）。
    //
    // Two bottom rows: the status bar is declared **first** so it gets the very bottom;
    // the filmstrip stacks above it. Declared the other way round it would float above
    // the strip, a whole row off the design. With nothing to report the whole bar is not
    // declared at all and the edge is given back (user, 2026-11-08).
    if layout.status == StatusSlot::Bottom && status_bar_needed(state) {
        egui::Panel::bottom("status-bar")
            .exact_size(STATUS_H)
            .frame(egui::Frame::NONE.fill(header()))
            .show(root, |ui| status_bar(state, ui));
    }
    // 整页视图（项目库 / 设置）不摆胶片条与走带：它们与「当前序列的帧」无关，
    if layout.film.allowed() && state.frame_count() > 0 {
        // 面板高度跟内容走：头行 26 + 间距 4 + [曲线 54 + 间距 6] + 胶片条实际
        // 高度 + 间距 4 + 走带 36。胶片条高度由首帧比例决定，面板就照着它收——
        // 固定 188 会把多出来的三四十像素全塞给走带，读作一条空带（用户截图）。
        //
        // The panel height follows the content: header 26 + gap 4 + [curve 54 + gap 6]
        // + the strip's actual height + gap 4 + transport 36. The strip height comes
        // from the first frame's aspect and the panel hugs it; a fixed 188 handed the
        // leftover thirty-odd pixels to the transport, reading as an empty band (user
        // screenshot).
        let tl_h = match (state.show_film, state.show_curves) {
            (true, curves) => {
                26.0 + 4.0 + if curves { 60.0 } else { 0.0 } + state.film_height() + 4.0 + 36.0
            }
            (false, true) => 26.0 + 4.0 + 60.0 + 36.0,
            (false, false) => 66.0,
        };
        egui::Panel::bottom("timeline")
            .exact_size(tl_h)
            .frame(egui::Frame::NONE.fill(surround()))
            .show(root, |ui| timeline(state, ui, &ctx));
    }

    // 4 侧栏。同一侧"先声明的在外"，所以顺序就是由外到内：
    //   右：28px 竖条 → 调整面板 → 工具轨；左：反过来。预设/快照/副本 2026-11-04
    //   起改走「预设与快照」浮窗（presets_window），左栏不再有常驻面板。
    let rail_left = layout.rail.is_left();
    let dock_left = layout.dock_side.is_left();

    // 竖条（网格 / 胶片 / 曲线）已移入画布右下浮层，这里不再占一列，
    // 右坞因此直接贴到窗口右缘。
    //
    // The rail (grid / film / curves) now floats on the canvas, so no column is
    // reserved here and the dock reaches the window edge.
    if state.show_dock && !dock_left {
        egui::Panel::right("dock")
            .exact_size(state.dock_w)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(dock()))
            .show(root, |ui| dock_panel(state, ui));
    }
    if !rail_left {
        egui::Panel::right("tool-rail")
            .exact_size(TOOL_W)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(chrome()))
            .show(root, |ui| tool_rail(state, ui));
    }
    if rail_left {
        egui::Panel::left("tool-rail")
            .exact_size(TOOL_W)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(chrome()))
            .show(root, |ui| tool_rail(state, ui));
    }
    if state.show_dock && dock_left {
        egui::Panel::left("dock")
            .exact_size(state.dock_w)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(dock()))
            .show(root, |ui| dock_panel(state, ui));
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
    let insets = crate::layout::canvas_insets(state.show_dock, dock_left, state.dock_w);
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
    let centre = crate::layout::centre_rect(full, available, insets);
    let mut centre_ui = root.new_child(egui::UiBuilder::new().max_rect(centre).id_salt("centre"));
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(surround()))
        .show(&mut centre_ui, |ui| canvas_panel(state, ui, insets.right));

    // 面板与画布之间那条呼吸带：与画布同底色的中性灰，面板内缘压一根发丝线。
    // 只画在文档区高度内，不越过菜单行 / 选项栏 / 底部胶片条。
    //
    // The gutter between dock and canvas: neutral surround grey with a hairline on the
    // dock's inner edge. Drawn only across the document area, never over the menu row,
    // options bar or bottom filmstrip.
    if let Some((band, line_x)) = crate::layout::canvas_gutter(available, centre, insets) {
        let p = root.painter().clone();
        p.rect_filled(band, 0.0, surround());
        let x = line_x.round() + 0.5;
        p.line_segment(
            [Pos2::new(x, band.top()), Pos2::new(x, band.bottom())],
            Stroke::new(1.0, border()),
        );
    }

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
    export_confirm_window(state, &ctx, full);
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

// ---------------------------------------------------------------- 导出确认

/// 导出参数确认框的挂载点：只有点了导出按钮（`aevum-export-pending` 置位）才出现。
///
/// 用户在这里改完范围 / 目录 / 格式 / 覆盖策略并点「开始导出」，才把选择写回
/// `AppState` 并真正跑导出；取消不碰任何参数、不产生任何文件。
///
/// 为什么不在按钮里直接弹：按钮在滚动面板里，面板每帧重画，Modal 挂在帧尾才不会
/// 被面板裁剪或重复创建。
///
/// Mount point for the export confirmation: it only appears after an export button sets
/// `aevum-export-pending`.
///
/// The choice is written back into `AppState` and the export runs only after the user
/// presses "Start export"; cancel touches nothing and produces no file.
///
/// Why not show it inside the button: the button lives in a scrolling panel that repaints
/// every frame, so mounting the modal at frame end keeps it from being clipped or created
/// twice.
fn export_confirm_window(state: &mut AppState, ctx: &egui::Context, full: egui::Rect) {
    use windows::ExportChoice;

    let pending: Option<u8> = ctx.data(|d| d.get_temp(egui::Id::unique("aevum-export-pending")));
    let Some(kind) = pending else {
        return;
    };
    // 上一次点的是「整段导出」还是「导出当前帧」。
    //
    // Whether the last click meant "export the sequence" or "export the current frame".
    let current_frame_only = kind == 2;

    let (choice, closed) = {
        // Modal 需要一个 Ui；借一个零尺寸的临时 Ui，它只往 ctx 上挂浮层。
        //
        // The modal needs a Ui; a zero-sized scratch Ui only mounts the overlay on the ctx.
        let mut scratch = egui::Ui::new(
            ctx.clone(),
            egui::Id::unique("aevum-export-confirm-host"),
            egui::UiBuilder::new().max_rect(full),
        );
        windows::confirm_export_modal(&mut scratch, state, current_frame_only)
    };
    // 取消（或已确认）都要把 pending 清掉，否则弹窗再也关不掉。
    // Cancel (or a completed confirm) must clear the pending flag, or the modal never closes.
    if closed {
        ctx.data_mut(|d| d.remove::<u8>(egui::Id::unique("aevum-export-pending")));
    }

    if let Some(ExportChoice {
        out_dir,
        format_depth,
        overwrite,
        ..
    }) = choice
    {
        // 把用户的选择写回导出参数：目录、格式、覆盖策略三项是面板里已有的字段，
        // 这里只做赋值，不新增第二套导出配置。
        //
        // Write the choice back into the existing export fields: directory, format and
        // overwrite policy already exist on the panel; this only assigns them.
        if !out_dir.trim().is_empty() {
            state.render_out_dir = std::path::PathBuf::from(out_dir);
        }
        if let Some(f) = aevum_core::export::StillFormat::ALL
            .into_iter()
            .find(|f| f.key() == format_depth)
        {
            state.render_format = f;
        }
        state.render_resume = overwrite == 2;
        if current_frame_only {
            state.export_current(ctx);
        } else {
            state.run_export(ctx);
        }
    }
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
