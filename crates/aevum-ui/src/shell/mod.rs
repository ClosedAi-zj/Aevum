//! 界面外壳（v21）：整个窗口由 `v21_demo` 从零绘制——顶栏 / 工具轨 / 画布井 /
//! 检查器坞 / 胶片条 / 状态栏。本模块只做三件事：
//!
//! 1. 输入收账：拖放打开 + 全局快捷键（`menu::handle_input`）；
//! 2. 布局单源：画布井矩形由 `layout::canvas_insets` + `layout::centre_rect`
//!    推导（`CANVAS_GAP` 只在 layout 里加一次），交给 `v21_demo` 绘制；
//! 3. 帧尾挂载：demo 原型没有的浮层——导出确认窗、工作流面板、告警状态栏、
//!    子窗口关闭钮皮肤、无边框窗口的缩放热区。
//!
//! UI shell (v21): the whole window is drawn from scratch by `v21_demo`. This
//! module only (1) collects input, (2) derives the canvas rect from the layout
//! single source, and (3) mounts the layers the prototype lacks (export dialog,
//! workflow card, notice bar, close-button skin, resize handles).
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

/// 状态栏高度：v21 底边那条带子的高度，直接取 `v21_demo` 的口径。
///
/// 两处各写一遍必然会漂：告警状态栏的挂载槽位按它算，判据「这条文案是不是落在
/// 状态栏里」也按它算——所以对外只留这一个数。
///
/// Status-bar height: the v21 bottom strip, taken straight from `v21_demo` so the
/// number exists once. The notice-bar slot is laid out with it, and the criterion
/// "is this text inside the status row" reads it as well.
pub const STATUS_BAR_H: f32 = v21_demo::STATUS_H;

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
    // ---- 每帧第一件事：把 core 压下的告警搬进状态栏 ----
    //
    // 护带目录写不进去、设置文件只读、路径被同名文件占住这类事实，用户必须
    // 在**点保存之前**就看见——晚一帧，那一下可能已经点下去了。所以这条排在
    // 所有绘制与帧号记账之前，与旧壳（`b963631` 之前的 `shell.rs`）同一位置；
    // v21 换壳时漏搬，生产调用点归零，后果是写盘失败全程静默。
    //
    // First thing every frame: move the notices core has queued into the status
    // bar. "The sidecar folder is not writable / the settings file is read-only /
    // the path is taken by a file" must be visible **before** the user hits save;
    // one frame late and that click has already happened. Same slot as the legacy
    // shell; the v21 swap dropped it, so write failures went silent.
    state.drain_notices();
    let ctx = root.ctx().clone();

    // ---- 首帧调试钩子（--shot 截图回归的入口）----
    debug_export_confirm(&ctx);
    debug_apply_flow(state);

    // ---- 帧首记帧号：撤销历史按帧记账的起点 ----
    //
    // `History::push_with` 的合并判据里有一条 `frame > 0` 与一条
    // `frame - tip.frame <= MERGE_FRAME_TOLERANCE`：没有这一行，`frame` 恒为 0，
    // 合并条件永远不成立，帧尾的 `history_commit` 就会**每帧记一步**——拖一次滑块
    // 灌进几十行历史，面板随即被刷爆。旧壳（`b963631` 之前的 `shell.rs`）在 `ui()`
    // 开头就调它，v21 换壳时与 `history_commit` 一起漏搬，本轮成对补回。
    //
    // Numbers the frame for undo-history bookkeeping. `History::push_with` merges on
    // `frame > 0` plus `frame - tip.frame <= MERGE_FRAME_TOLERANCE`; without this call
    // `frame` stays 0, the merge test can never hold, and the tail `history_commit`
    // records **one step per frame** -- a single slider drag would flood the panel.
    // The legacy shell called it at the top of `ui()`, the v21 swap dropped it together
    // with `history_commit`, and this round restores the pair.
    state.history.begin_frame();

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

    // ---- 帧尾驱动一：撤销历史落账 ----
    //
    // `history_commit` 拿当前状态和光标处的快照比，不一样就记一步（控件登记过
    // 文案就用文案，没登记就按字段比对认类）。它**设计上就是每帧调用**：合并
    // 交给 `Step::cat` / 拖动序号，不靠调用方攒批量。v21 换壳时这一句也漏搬了，
    // 生产调用点归零，后果是撤销/重做与历史面板全哑——`steps()` 永远只有一条
    // 初始步、`can_undo()` 恒 false。
    //
    // 顺序照旧壳：先记账，再把「这一帧新出现的错误」抬成弹窗，最后才调度预览。
    //
    // Tail driver #1: record the undo history. `history_commit` compares the current
    // state against the snapshot at the cursor and records a step when they differ (a
    // registered label wins, otherwise the field-comparison class is used). It is
    // **designed to be called every frame**: merging is `Step::cat` / drag-serial
    // work, not the caller's batch job. The v21 swap dropped this call too, taking the
    // production call count to zero -- undo/redo and the history panel went silent
    // (`steps()` held only the initial step and `can_undo()` was always false). Order
    // follows the legacy shell: book-keep first, then promote new errors to a dialog,
    // and only then schedule the preview.
    state.history_commit();

    // ---- 帧尾驱动二：新错误抬成弹窗 ----
    //
    // 全仓四十余处 `state.error = Some(...)` 都靠这一处统一兜住：`error` 一变就
    // 同步进 `error_modal`，再由下面的 `windows::error_dialog` 画出来。**两条必须
    // 成对**：只同步不画等于白补，只画不同步则弹窗永远不出现。
    //
    // 顺序也有讲究：`sync_error_dialog` 必须排在 `error_dialog` 之前，否则这一帧
    // 新产生的错误要等到下一帧才可见。
    //
    // Tail driver #2: promote new errors to a dialog. The forty-odd
    // `state.error = Some(...)` sites are all covered here: when `error` changes it is
    // copied into `error_modal`, and `windows::error_dialog` below paints it. The two
    // **must be paired** -- syncing without painting is a no-op, painting without
    // syncing never fires. Order matters as well: `sync_error_dialog` has to run before
    // `error_dialog`, or an error raised this frame only shows up on the next one.
    state.sync_error_dialog();
    // 弹窗画在最后：它是对「刚才那次操作失败」的确认，压住所有浮层。
    //
    // The dialog paints last: it acknowledges a failed action and sits above every
    // other layer.
    windows::error_dialog(state, &ctx);

    // ---- 帧尾挂载三：告警状态栏 ----
    //
    // demo 的底边只是一行装饰读数（「就绪 · 硬件加速 · 画幅 · GPU 时间」），没有
    // 告警的位置。`drain_notices` 把告警搬进 `state.notices` 之后必须**有人画**，
    // 否则状态栏是空的，写盘失败依旧静默——这正是「补一半」最容易犯的错。
    //
    // 这里复用 v20 的 `toolbar::status_bar`（旧壳在 `StatusSlot` 处挂的那一个）：
    // 它自带告警文案 +「+N」+ 点一下消一条，还有长任务进度与边车提示三格，
    // 语义与旧壳完全一致。占位就是 demo 状态栏那 20px：整条底色先盖掉装饰读数，
    // 再画状态栏内容——「状态栏只在有话要说时才存在」这条口径照旧，没话要说时
    // 一个形状都不建，`--selftest` 的形状计数与截图回归都不受影响。
    //
    // Tail mount #3: the notice bar. The prototype's bottom edge is a decorative
    // readout row with no slot for notices; `drain_notices` fills `state.notices`,
    // so **something has to paint it** or the bar stays empty and the write failure
    // stays silent. The v20 `toolbar::status_bar` is reused as-is (the same widget
    // the legacy shell mounted at `StatusSlot`): it already carries the notice text
    // plus "+N" plus click-to-dismiss, along with the long-task and sidecar slots.
    // The slot is the prototype's 20 px status row: cover the decorative readout
    // first, then paint the bar, so "the status bar exists only when it has
    // something to say" still holds -- no shapes at all when it is quiet, which is
    // what `--selftest` shape counts and the screenshot regression rely on.
    if toolbar::status_bar_needed(state) {
        let status = Rect::from_min_max(
            Pos2::new(full.left(), full.bottom() - STATUS_BAR_H),
            full.max,
        );
        root.scope_builder(
            egui::UiBuilder::new()
                .max_rect(status)
                .id_salt("status-bar"),
            |ui| {
                ui.set_clip_rect(status);
                ui.painter().rect_filled(status, 0.0, crate::theme::bar());
                toolbar::status_bar(state, ui);
            },
        );
    }

    // ---- 帧尾挂载四：子窗口关闭钮的 hover 皮肤（纯观感）----
    //
    // egui 内建的 ✕ 只有 ~20px、hover 只换笔画色，与标题栏按钮不是一套规格。
    // `style_close_buttons` 在 Tooltip 层叠画 28×28 的圆角按钮（静止态接管外观，
    // hover 换 Windows 红底白叉）；点击行为仍归 egui 自己的关闭钮，所以挂在所有
    // 浮窗之后、缩放热区之前即可。v21 换壳时漏搬，这一句归零 = 皮肤全哑。
    //
    // Tail mount #4: the sub-window close button skin (cosmetic only). It paints a
    // 28x28 rounded button on the Tooltip layer -- click handling stays with egui's
    // own close button -- so it goes after every floating window and before the
    // resize handles. The v21 swap dropped the call, leaving the skin dead.
    windows::style_close_buttons(&ctx);

    // ---- 帧尾挂载五：无边框窗口的拖拽缩放热区 ----
    //
    // 窗口是无边框自绘的（`decorations = false`），系统不再给边框，四边四角的
    // 缩放全靠 `chrome::window_resize` 用隐形 `interact` 区自己做：鼠标压上去换
    // 缩放光标，按下就把 `ViewportCommand::BeginResize(方向)` 交给系统去拖。
    // **没有这一句，窗口边缘完全拖不动**——这是用户报「窗口拖边缘改不了大小」
    // 的根因，`--selftest` 测不到（它只数形状，热区是隐形的）。
    //
    // 位置与旧壳一致，且必须在**最后**：`ui.interact` 的命中优先级按注册顺序排，
    // 最后注册的热区压在所有面板之上，否则坞 / 胶片条 / 状态栏会先吃掉指针，
    // 边缘 5px 变成死区。传入 `full`（整窗 = `root.max_rect()`），热区不吃 clip
    // ——它画在窗口最外圈，本来就在所有面板之外。
    //
    // 参数口径：`window_resize(ui, ctx, full)`，`ui` 要一个能注册交互的 Ui（根 Ui
    // 即可，它覆盖整窗、clip 就是窗口），`full` 必须是**整窗矩形**而不是某个
    // 子区，函数内部按 `full` 的四边四角算八个热区。
    //
    // Tail mount #5: the borderless window's drag-to-resize zones. The window draws
    // its own chrome (`decorations = false`), so the system ships no border and
    // `chrome::window_resize` builds the four edges and four corners out of
    // invisible `interact` rects: hovering swaps the cursor, pressing hands
    // `ViewportCommand::BeginResize(direction)` to the system. **Without this line
    // the window edge cannot be dragged at all** -- the root cause of "I cannot
    // resize the window by its edge", which `--selftest` cannot see (it counts
    // shapes; these zones are invisible). It must come **last**: `ui.interact` hit
    // priority follows registration order, so the handles registered last sit above
    // every panel; otherwise the dock / filmstrip / status bar eat the pointer and
    // the outer 5 px become dead. `full` is the whole window (`root.max_rect()`);
    // the zones live on the outermost ring, outside every panel, so no clip is
    // needed. Signature: `window_resize(ui, ctx, full)` -- any Ui that can register
    // interactions (the root Ui, which covers and clips to the whole window) plus
    // the **whole-window** rect the function slices into eight handles.
    chrome::window_resize(root, &ctx, full);

    // ---- 帧尾驱动三：预览任务的唯一调度点 ----
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
