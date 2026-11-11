//! 编号工作台：显影 / 帧表 / 导出 / 任务，四页共用一套外壳。
//!
//! 本模块提供工作台枚举、编号，以及整页视图共用的页头与二级工具条。工作台切换
//! 入口在菜单行「视图」菜单（用户 2026-11-11「悬浮设计不好看」），画布上不再
//! 绘制导航条或浮卡。编号 1..4 与 Ctrl+Alt+1..4 一一对应，菜单条目也带同一组编号。
//!
//! Numbered benches: develop / frame table / export / tasks, sharing one shell.
//!
//! This module owns the bench enum, its numbering, and the full-page head / secondary
//! tool strip. Switching lives in the menu-row View menu (2026-11-11 feedback: no
//! floating nav strip over the canvas). Numbers 1..4 match Ctrl+Alt+1..4 and label the
//! menu rows too.

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Vec2};

use crate::theme::*;

/// 四个工作台。顺序即编号（1 显影 / 2 帧表 / 3 导出 / 4 任务）。
///
/// The four benches; the order is the numbering (1 develop / 2 table / 3 export / 4 tasks).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Bench {
    /// 显影：媒体优先的画布 + 浮动卡片。
    ///
    /// Develop: media-first canvas with floating cards.
    #[default]
    Develop,
    /// 帧表：逐帧表格（v10.6 新页）。
    ///
    /// Frame table: the per-frame table (new in v10.6).
    Table,
    /// 导出：队列 + 设置。
    ///
    /// Export: queue plus settings.
    Export,
    /// 任务：后台任务卡。
    ///
    /// Tasks: background task cards.
    Tasks,
}

impl Bench {
    pub const ALL: [Bench; 4] = [Self::Develop, Self::Table, Self::Export, Self::Tasks];

    pub fn cn(self) -> &'static str {
        match self {
            Self::Develop => "显影",
            Self::Table => "帧表",
            Self::Export => "导出",
            Self::Tasks => "任务",
        }
    }

    /// 编号从 1 起，与 Alt+1..4 对应。
    ///
    /// One-based number, matching Alt+1..4.
    pub fn no(self) -> usize {
        match self {
            Self::Develop => 1,
            Self::Table => 2,
            Self::Export => 3,
            Self::Tasks => 4,
        }
    }
}

/// 页头高 64：设计稿 `.ws-head{height:64px;padding:0 16px;box-shadow:0 1px 0 var(--hair)}`。
///
/// Page head height 64, per `.ws-head`.
pub(super) const HEAD_H: f32 = 64.0;
/// 二级工具条高 36：`.ws-tools{height:36px}`。
///
/// Secondary tool strip height 36, per `.ws-tools`.
pub(super) const TOOLS_H: f32 = 36.0;

/// 画整页视图的页头（左侧 2px 强调闸门 + 15px 标题 + 10px 副标题），
/// 返回页头矩形；右侧动作由调用方用 [`head_action`] 从右往左摆。
///
/// Draw a full-page head (2 px accent gate + 15 px title + 10 px subtitle) and return its
/// rect; the caller lays out right-hand actions from the right edge with [`head_action`].
pub(super) fn page_head(ui: &mut egui::Ui, title: &str, sub: &str) -> Rect {
    let full = Rect::from_min_size(
        ui.max_rect().left_top(),
        Vec2::new(ui.max_rect().width(), HEAD_H),
    );
    let p = ui.painter().clone();
    p.rect_filled(full, 0.0, header());
    p.line_segment(
        [
            Pos2::new(full.left(), full.bottom() - 0.5),
            Pos2::new(full.right(), full.bottom() - 0.5),
        ],
        Stroke::new(1.0, Color32::from_white_alpha(20)),
    );
    // 闸门条：`top:3px;bottom:3px;width:2px;background:var(--blue)`，贴在标题左侧。
    //
    // The gate bar sits 3 px in from top and bottom, 2 px wide, accent-coloured.
    let gate = Rect::from_min_max(
        Pos2::new(full.left() + 16.0, full.top() + 15.0),
        Pos2::new(full.left() + 18.0, full.bottom() - 15.0),
    );
    p.rect_filled(gate, 1.0, accent());
    p.text(
        Pos2::new(gate.right() + 10.0, full.center().y - 8.0),
        Align2::LEFT_CENTER,
        title,
        font(SIZE_CHROME),
        text(),
    );
    p.text(
        Pos2::new(gate.right() + 10.0, full.center().y + 10.0),
        Align2::LEFT_CENTER,
        sub,
        font(10.0),
        dim(),
    );
    full
}

/// 页头右侧的动作按钮（28px 高，主按钮走强调色）。`right` 是当前可用的最右 x，
/// 返回按钮矩形，调用方把它当下一枚的 `right`，从右往左排。
///
/// A head action button (28 high; the primary one uses the accent fill). `right` is the
/// current rightmost free x; the returned rect becomes the next call's `right`, so the
/// row grows leftwards.
pub(super) fn head_action(
    ui: &mut egui::Ui,
    right: f32,
    center_y: f32,
    label: &str,
    primary: bool,
) -> (Rect, egui::Response) {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font(12.0), text());
    let w = galley.size().x + 20.0;
    let r = Rect::from_min_size(Pos2::new(right - w, center_y - 14.0), Vec2::new(w, 28.0));
    let resp = ui.interact(
        r,
        ui.make_persistent_id(("head-action", label)),
        Sense::click(),
    );
    let p = ui.painter().clone();
    let fill = if primary {
        if resp.is_pointer_button_down_on() {
            accent_hover()
        } else {
            accent()
        }
    } else if resp.is_pointer_button_down_on() {
        press()
    } else if resp.hovered() {
        elev_hover()
    } else {
        elev()
    };
    p.rect_filled(r, 6.0, fill);
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        font(12.0),
        if primary { Color32::WHITE } else { text() },
    );
    (r, resp)
}

/// 二级工具条（36px）：返回工具条矩形，按钮由调用方从 [`tools_x`] 起往右排。
///
/// The secondary tool strip (36 px); returns its rect so the caller can place text
/// buttons rightwards starting at [`tools_left`].
pub(super) fn tools_strip(ui: &mut egui::Ui, top: f32) -> Rect {
    let full = Rect::from_min_size(
        Pos2::new(ui.max_rect().left(), top),
        Vec2::new(ui.max_rect().width(), TOOLS_H),
    );
    let p = ui.painter().clone();
    p.rect_filled(full, 0.0, header());
    p.line_segment(
        [
            Pos2::new(full.left(), full.bottom() - 0.5),
            Pos2::new(full.right(), full.bottom() - 0.5),
        ],
        Stroke::new(1.0, Color32::from_white_alpha(20)),
    );
    full
}

/// 工具条文字按钮（24px 高、11px 字）：`.ws-tools .tb`。返回响应。
///
/// A tool-strip text button (24 high, 11 px): `.ws-tools .tb`. Returns the response.
pub(super) fn tool_button(
    ui: &mut egui::Ui,
    x: f32,
    strip: Rect,
    label: &str,
    on: bool,
) -> (f32, egui::Response) {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font(11.0), dim());
    let w = galley.size().x + 16.0;
    let r = Rect::from_min_size(Pos2::new(x, strip.center().y - 12.0), Vec2::new(w, 24.0));
    let resp = ui.interact(
        r,
        ui.make_persistent_id(("tool-btn", label)),
        Sense::click(),
    );
    let p = ui.painter().clone();
    if on {
        p.rect_filled(r, 4.0, accent_dim());
    } else if resp.hovered() {
        p.rect_filled(r, 4.0, elev());
    }
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        font(11.0),
        if on { text() } else { dim() },
    );
    (r.right() + 4.0, resp)
}
