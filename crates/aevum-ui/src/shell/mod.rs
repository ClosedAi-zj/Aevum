//! 界面外壳（v21）：整个窗口由 `v21_demo` 从零绘制——顶栏 / 工具轨 / 画布井 /
//! 检查器坞 / 胶片条 / 状态栏。本模块只做三件事：
//!
//! 1. 输入收账：拖放打开 + 全局快捷键（`menu::handle_input`）；
//! 2. 布局单源：画布井矩形由 `layout::canvas_insets` + `layout::centre_rect`
//!    推导（`CANVAS_GAP` 只在 layout 里加一次），交给 `v21_demo` 绘制；
//! 3. 帧尾挂载：demo 原型没有的浮层——导出确认窗、工作流面板。
//!
//! UI shell (v21): the whole window is drawn from scratch by `v21_demo`. This
//! module only (1) collects input, (2) derives the canvas rect from the layout
//! single source, and (3) mounts the float layers the prototype lacks.
//!
//! 功能层（影像 / 参数 / 缩略图 / 菜单 / 状态）通过 `AppState` 的接口调用，
//! 不在这里排版；旧版面已删除（备份在 `backup/shell-v20-*`）。
//!
//! Functional layers are reached through `AppState` APIs; the legacy layout is
//! deleted (backup under `backup/shell-v20-*`).

use egui::{Color32, Pos2, Rect, Stroke, Ui};

use self::menu::handle_input;
use crate::state::AppState;

// ---- 模块划分（性能重构设计-2026-10-30 §7/§10）----

// v21 实时路径：mod.rs / 桥接 / 判据直接调用。
//
// The v21 live path: called from this module, the bridge, or the criteria suite.
pub(crate) mod export_dialog;
pub(crate) mod flow;
mod v21_demo;

// ---- v22 屏幕实现层（暂未接入 v21 壳，整组保留待回接）----
//
// v21 把版面壳换成了单屏显影（`v21_demo`）。下列模块是 v18–v20 的屏面实现
// ——库 / 时间线 / 帧表 / 任务 / 导出工作台 / 蒙版 / 调整面板 / 通用控件 /
// 窗口——排期在 v22 按八屏设计稿逐屏回接（PROGRESS-v21 §6）。
//
// 在回接之前整组显式豁免 `dead_code`：这不是放行新代码躺平，而是给既有实现
// 一个可 grep 的保留标记（删除前必须先迁走 tests/suite 里按文件扫描这些模块
// 的判据）；`v21_demo` 与 `mod.rs` 自身的新增死码依然会被 clippy `-D warnings`
// 拦下。
//
// The v22 screen-implementation layer, retained untouched until each screen is
// re-wired per the eight-screen design. The blanket allow is a greppable marker
// for finished work awaiting re-wiring -- new dead code in v21_demo and mod.rs
// still fails the clippy gate. Migrate the file-scanning suite criteria before
// deleting any of these.
#[allow(dead_code)]
mod adjust;
#[allow(dead_code)]
mod bench;
mod canvas;
mod canvas_cards;
#[allow(dead_code)]
mod chrome;
#[allow(dead_code)]
mod common;
#[allow(dead_code)]
mod dialogs;
#[allow(dead_code)]
mod dock;
mod export_ws;
#[allow(dead_code)]
mod flow_panel;
#[allow(dead_code)]
mod library;
#[allow(dead_code)]
mod markdown;
#[allow(dead_code)]
mod mask;
#[allow(dead_code)]
mod menu;
#[allow(dead_code)]
mod pages;
#[allow(dead_code)]
mod palette;
mod seq_table;
#[allow(dead_code)]
mod tabs;
#[allow(dead_code)]
mod tasks_ws;
#[allow(dead_code)]
mod timeline;
#[allow(dead_code)]
mod toolbar;
#[allow(dead_code)]
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
/// 保留：`canvas.rs`（v22 画布回接时）仍从 `super::` 取用。
///
/// Ruler slot width, kept for `canvas.rs` (re-wired in v22).
const RULER: f32 = 24.0;
/// 压在图像上的微标签（标尺刻度、胶片序号）：比界面最小字阶再小一档，
/// 但不许低于 9px，否则嵌进来的静态字重会发虚。
///
/// Micro labels painted over the image; never below 9 px.
const MICRO: f32 = 9.0;
/// 全幅纹理的 UV 矩形：`library.rs` / `timeline.rs` 画缩略图用。
///
/// Full-span UV rect used by the retained thumbnail painters.
const UV: Rect = Rect {
    min: Pos2::new(0.0, 0.0),
    max: Pos2::new(1.0, 1.0),
};

/// v20 版面保留模块（toolbar/adjust/common…）仍从 `super::` 取用的菜单行高。
/// v21 顶栏是 `v21_demo::TOP_H`；这两个值当前同高，v22 回接各屏时随屏迁移。
///
/// Menu-row height still imported via `super::` by the retained modules.
/// The v21 top bar is `v21_demo::TOP_H`; the two currently agree.
pub(crate) const BAR_H: f32 = 44.0;

/// 井相对框架的统一内缩：四周留 8px，井才"凹"得出来。
/// 坞侧的呼吸带不在这算——`layout::canvas_insets` 是它的唯一出口（v21_demo
/// 旧版把 8px 同时当呼吸带，右侧因此比这里少 2px；接回单源后井右侧由
/// `CANVAS_GAP` 决定）。
///
/// The well's uniform inset on the frame. The dock-side gutter is NOT computed
/// here: `layout::canvas_insets` owns it, so the well's right edge follows
/// `CANVAS_GAP` instead of this constant.
const WELL_INSET: f32 = 8.0;

/// 首帧应用一次 `AEVUM_EXPORT_CONFIRM`（见函数内说明）。
///
/// Apply `AEVUM_EXPORT_CONFIRM` once, on the first frame.
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

pub fn ui(state: &mut AppState, root: &mut Ui) {
    let ctx = root.ctx().clone();

    // ---- 首帧调试钩子（--shot 截图回归的入口）----
    debug_export_confirm(&ctx);
    debug_apply_flow(state);

    // ---- 输入收账：拖放打开 + 全局快捷键（与旧壳同一入口）----
    handle_input(state, &ctx);

    // ---- 布局单源 ----
    //
    // 画布井 = 身体区经 `layout::canvas_insets` / `centre_rect` 让位后的中央区。
    // 呼吸带只在 layout 里加一次（`CANVAS_GAP` 的唯一使用点），这边只补齐井
    // 上 / 下 / 左的统一内缩；右侧呼吸带随坞由 centre_rect 让出。
    //
    // The canvas well = the body region yielded through the layout single source.
    // The breathing band is added exactly once inside layout; this side only adds
    // the uniform top/bottom/left inset -- the dock side comes from centre_rect.
    let full = root.max_rect();
    let body = Rect::from_min_max(
        Pos2::new(full.left() + v21_demo::RAIL_W, full.top() + v21_demo::TOP_H),
        Pos2::new(
            full.right(),
            full.bottom() - v21_demo::FILM_H - v21_demo::STATUS_H,
        ),
    );
    let insets = crate::layout::canvas_insets(true, false, v21_demo::DOCK_W);
    let centre = crate::layout::centre_rect(full, body, insets);
    let canvas = Rect::from_min_max(
        Pos2::new(centre.min.x + WELL_INSET, centre.min.y + WELL_INSET),
        Pos2::new(centre.max.x, centre.max.y - WELL_INSET),
    );

    v21_demo::ui(state, root, &canvas);

    // ---- 帧尾挂载：demo 原型没有的浮层 ----
    //
    // demo 是单屏原型，没有导出确认窗与工作流面板；真应用有。在 demo 画完
    // 之后叠画它们，既能用又不改 demo 一行绘制代码。
    //
    // Float layers the prototype lacks, painted after the demo so the prototype's
    // drawing code stays untouched.
    export_dialog::export_confirm_window(state, &ctx, full);
    if flow_panel::flow_panel_visible(state, full.width()) {
        // 工作流面板：画布**右上**的浮卡（宽 FLOW_W），右缘让过检查器坞、顶缘
        // 让过顶栏。demo 的浮层语法本来就是"卡浮在照片上"——预设与历史卡在
        // 左上，工作流卡在右上，同一套语法互不叠。标题栏步骤条已随
        // 2026-11-08 裁决撤除（步骤住进面板），所以这里不挂 flow_top_bar。
        //
        // The workflow panel as a floating card at the canvas's top-right: its
        // right edge clears the inspector dock and its top edge clears the bar.
        // The demo's float grammar is "cards hovering over the photo" -- presets
        // at top-left, workflow at top-right, same grammar without overlap. The
        // title-bar step strip is gone per the 2026-11-08 ruling, so
        // flow_top_bar is not mounted.
        // 落点就地算：右缘让过检查器坞 + 井内距，顶缘让过顶栏。不用
        // `.anchor(…)`——判据 no_floating_window_is_anchored（egui 源码注释
        // 原话：一 anchor 浮窗就 immovable）。卡宽 = FLOW_W + Frame 内距 8×2。
        //
        // The slot is computed here: right edge clears the dock plus the well
        // inset, top edge clears the bar. No `.anchor(…)` -- the no-anchor
        // criterion; card width = FLOW_W plus the Frame's 8 px margins.
        let flow_card_w = crate::theme::FLOW_W + 16.0;
        let flow_pos = Pos2::new(
            full.right() - v21_demo::DOCK_W - WELL_INSET - flow_card_w,
            full.top() + v21_demo::TOP_H + WELL_INSET,
        );
        egui::Area::new(egui::Id::unique("v21-flow-panel"))
            .order(egui::Order::Middle)
            .fixed_pos(flow_pos)
            .movable(false)
            .show(&ctx, |ui| {
                ui.set_max_width(crate::theme::FLOW_W);
                // 浮卡要有"卡身"：elev 底 + 10 圆角 + 8 内距 + 1px 暗描边——
                // 老壳里这层底由面板容器给，浮到照片上后由 Frame 自己给，
                // 否则白字直接压在照片上读不清。
                //
                // The card needs a body: elevated fill, 10 px radius, 8 px margin
                // and a 1 px dark stroke -- the old shell's panel container
                // provided this; over the photo the Frame provides it, or the
                // white text sinks into the picture.
                egui::Frame::new()
                    .fill(crate::theme::elev())
                    // 圆角走面板档：跟着「外观 → 圆角」旋钮走，不写死。
                    //
                    // Panel-tier radius, following the corner-radius knob.
                    .corner_radius(crate::theme::radius_panel())
                    .inner_margin(8.0)
                    .stroke(Stroke::new(1.0, Color32::from_black_alpha(96)))
                    .show(ui, |ui| {
                        flow_panel::flow_panel(state, ui, &ctx);
                    });
            });
    }

    // ---- 帧尾驱动：预览任务的唯一调度点 ----
    //
    // 预览是「有脏标记就排一次后台渲染」的拉取式任务：`ensure_preview` 读
    // `preview_dirty` / `preview_inflight`，脏了就把渲染任务扔进线程池，回包在
    // `poll` 里落到 `state.texture`。**没有这一行，标记永远是脏的、任务永远不发**，
    // 表现就是用户报告的「导入照片后画布一片空白、滑块怎么拖都不刷新」。
    //
    // 位置与旧壳一致：帧尾、所有绘制之后。因为函数内部要读
    // `ctx.input(|i| i.pointer.any_down())` 判断「这一帧是不是在拖草图」——必须在
    // egui 收完本帧输入、画完本帧之后调用，才能在松手的那一帧补全量。
    //
    // v21 换壳（b963631）时从旧 `shell.rs` 末尾漏搬了这一句，生产调用点归零，
    // P0 回归判据 `preview_wiring.rs` 是这条线的看门人。
    //
    // The tail-end driver: the single scheduling point for preview jobs. Preview is a
    // pull-based job -- `ensure_preview` reads `preview_dirty` / `preview_inflight`,
    // posts a render when dirty, and the reply lands on `state.texture` in `poll`.
    // **Without this line the flag stays dirty and no job is ever posted**, which is
    // exactly the user's report: "import a photo and the canvas stays blank, sliders
    // never refresh it". Same position as the legacy shell -- at the tail, after every
    // paint -- because it reads `ctx.input(|i| i.pointer.any_down())` to decide whether
    // this frame is a drag sketch; it must run after egui has collected input and
    // painted, so the release frame can top up with a full render. The v21 shell swap
    // (b963631) dropped this line, taking the production call count to zero;
    // `preview_wiring.rs` guards the wire.
    state.ensure_preview(&ctx);
}
