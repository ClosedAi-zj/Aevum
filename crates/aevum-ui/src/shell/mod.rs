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

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use self::canvas::canvas_panel;
use self::chrome::window_resize;
use self::dock::dock_panel;
use self::menu::handle_input;
use self::menu::menu_row;
use self::timeline::timeline;
use self::toolbar::options_bar;
use self::toolbar::status_bar;
use self::toolbar::tool_rail;
use self::windows::about_window;
use self::windows::look_window;
use self::windows::market_window;
use self::windows::presets_window;
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
mod library;
mod mask;
mod menu;
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
/// 实心控件的那圈极淡内描边（红绿灯圆点、滑杆推子共用）：
/// 只给边界，不给 Adobe 式硬边框。
const INSET_RIM: Color32 = Color32::from_rgba_unmultiplied_const(0, 0, 0, 46);

const UV: Rect = Rect {
    min: Pos2::new(0.0, 0.0),
    max: Pos2::new(1.0, 1.0),
};

pub fn ui(state: &mut AppState, root: &mut Ui) {
    // 每帧第一件事：把 core 压下的告警搬进状态栏。护带目录写不进去这类事
    // 必须在用户点保存之前就看见——晚一帧，可能已经点下去了。
    state.drain_notices();
    let ctx = root.ctx().clone();
    let full = root.max_rect();
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
    if layout.status == StatusSlot::Top {
        egui::Panel::top("status-bar")
            .exact_size(STATUS_H)
            .frame(egui::Frame::NONE.fill(header()))
            .show(root, |ui| status_bar(state, ui));
    }

    // 2 工具栏：当前工具 │ 工具参数 │ 上下文 ……… 右侧缩放。
    egui::Panel::top("options-bar")
        .exact_size(OPTIONS_H)
        .frame(egui::Frame::NONE.fill(chrome()).inner_margin(egui::Margin {
            left: 8,
            right: 8,
            top: 0,
            bottom: 0,
        }))
        .show(root, |ui| options_bar(state, ui, &ctx));

    // 3 底部两行：**先声明状态栏**，它才拿得到窗口最底边；胶片条压在它上面。
    //   反过来声明就成了"状态栏飘在胶片条之上"，与界面稿差出整整一行。
    if layout.status == StatusSlot::Bottom {
        egui::Panel::bottom("status-bar")
            .exact_size(STATUS_H)
            .frame(egui::Frame::NONE.fill(header()))
            .show(root, |ui| status_bar(state, ui));
    }
    if layout.film.allowed() {
        egui::Panel::bottom("timeline")
            .exact_size(TIMELINE_H)
            .frame(egui::Frame::NONE.fill(surround()))
            .show(root, |ui| timeline(state, ui, &ctx));
    }

    // 4 侧栏。同一侧"先声明的在外"，所以顺序就是由外到内：
    //   右：28px 竖条 → 调整面板 → 工具轨；左：反过来。预设/快照/副本 2026-11-04
    //   起改走「预设与快照」浮窗（presets_window），左栏不再有常驻面板。
    let rail_left = layout.rail.is_left();
    let dock_left = layout.dock_side.is_left();

    if state.show_strip {
        egui::Panel::right("dock-strip")
            .exact_size(STRIP_W)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(header()))
            .show(root, |ui| dock_strip(state, ui));
    }
    if state.show_dock && !dock_left {
        egui::Panel::right("dock")
            .exact_size(DOCK_W)
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
            .exact_size(DOCK_W)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(dock()))
            .show(root, |ui| dock_panel(state, ui));
    }

    // 画布右边界按已知的右侧面板宽度夹一次：切模块时 dock 内容一换，egui 的
    // Panel 会用它记着的「上次宽度」去缩剩余区，而面板自己的 rect 已经是
    // `exact_size`，两者会连着几十帧对不上（画布因此压在 dock 页签上，见判据
    // `ui_tokens::text_never_overlaps_in_any_module`）。布局常量就摆在这里，
    // 用它算边界比依赖第三方内部状态稳。
    //
    // Clamp the canvas right edge from the known right-hand panel widths: switching
    // modules swaps the dock content, and egui's Panel shrinks the remainder using the
    // width it remembers while its own rect already honours `exact_size`, so the two
    // disagree for tens of frames (the canvas ends up sitting on the dock tabs -- see
    // `text_never_overlaps_in_any_module`). The layout constants are right here, and
    // deriving the edge from them beats depending on a third party's internal state.
    let right_inset =
        if state.show_strip { STRIP_W } else { 0.0 } + if state.show_dock { DOCK_W } else { 0.0 };
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(surround()))
        .show(root, |ui| canvas_panel(state, ui, right_inset));

    state.history_commit();
    presets_window(state, &ctx);
    about_window(state, &ctx);
    look_window(state, &ctx);
    market_window(state, &ctx);
    window_resize(root, &ctx, full);

    state.ensure_preview(&ctx);
}

// ---------------------------------------------------------------- 右侧竖条

/// 最外那条 28px 竖条：网格 / 胶片 / 曲线三枚开关（界面稿里是 ⌗ ◐ ◑）。
///
/// 它挂在调整面板外侧、贴着窗口右沿 —— 是"画布上加什么辅看"这一层，
/// 和面板里"改什么参数"不是一类东西，所以宁可窄成一条也不并进页签。
fn dock_strip(state: &mut AppState, ui: &mut Ui) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.line_segment(
        [full.left_top(), full.left_bottom()],
        Stroke::new(1.0, border()),
    );
    let items: [(Glyph, &str, bool); 3] = [
        (Glyph::Grid, "网格 (G) · 三分线", state.grid),
        (Glyph::Frame, "胶片条", state.show_film),
        (Glyph::Sliders, "亮度曲线", state.show_curves),
    ];
    let mut flip = [false; 3];
    let mut y = full.top() + 6.0;
    for (i, (glyph, tip, on)) in items.iter().enumerate() {
        let r = Rect::from_min_size(Pos2::new(full.left() + 3.0, y), Vec2::splat(22.0));
        let resp = ui.interact(r, ui.make_persistent_id(("strip", i)), Sense::click());
        if *on {
            // 选中只给一层极淡的强调色底 + 强调色图标：整块实心蓝在这条 28px
            // 的窄条里会变成一颗抢眼的按钮，而它其实只是"辅看开着"。
            p.rect_filled(r, radius_control(), accent_dim());
        } else if resp.hovered() {
            p.rect_filled(r, radius_control(), elev());
        }
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
        let clicked = resp.clicked();
        resp.on_hover_text(*tip);
        if clicked {
            flip[i] = true;
        }
        y += 26.0;
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

/// 工具栏控件高度：跟加高后的选项栏行高配套，按钮不再显得飘在中间。
const BAR_H: f32 = 28.0;
