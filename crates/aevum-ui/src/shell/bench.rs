//! 编号工作台：显影 / 帧表 / 导出 / 任务，四页共用一套外壳。
//!
//! 设计稿 `docs/aevum-v10.6-complete.html` 的 `.wsbar` 是一张**浮在内容上的实色卡片**
//! （`top:56px`、居中、`padding:3px`、圆角 12、带投影），不是贴在标题栏里的一排页签。
//! 切到帧表/导出/任务时，媒体让位给整页视图（`.wsview`），只留顶栏与这张导航卡。
//!
//! 为什么编号必留在按钮里：设计稿 `.sn` 是 9px 方形编号，选中态换成强调色底。
//! 编号是「第几个工作台」的唯一可见线索，键盘 Alt+1..4 与它一一对应。
//!
//! Numbered benches: develop / frame table / export / tasks, sharing one shell.
//!
//! The design's `.wsbar` is an **opaque card floating over the content** (top 56 px,
//! centred, 3 px padding, 12 px radius, drop shadow), not a row of tabs inside the title
//! bar. Switching to table / export / tasks gives the media up to a full-page view
//! (`.wsview`), leaving only the top bar and this card.
//!
//! The number badge stays in the button because the design's `.sn` is a 9 px square
//! badge that turns accent-filled when active -- it is the only visible cue for "which
//! bench is this", and it matches the Alt+1..4 shortcuts one to one.

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::state::AppState;
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

/// 导航卡内边距：设计稿 `.wsbar{padding:3px}`。
///
/// Nav card padding, per `.wsbar{padding:3px}`.
const PAD: f32 = 3.0;
/// 按钮高 28、圆角 8、内距（左 6 右 10）：`.wsbar button`。
///
/// Button: 28 high, 8 radius, 6/10 inset -- `.wsbar button`.
const BTN_H: f32 = 28.0;
const BTN_R: f32 = 8.0;
/// 编号块 16×16、圆角 4：`.wsbar .sn`。
///
/// Number badge 16x16, 4 radius -- `.wsbar .sn`.
const SN: f32 = 16.0;

/// 画工作台导航卡并处理点击。返回本帧是否有切换。
///
/// Draw the bench nav card and handle clicks; returns whether the bench changed.
pub(super) fn nav(state: &mut AppState, ui: &mut egui::Ui, top_center: Pos2) {
    let p = ui.painter().clone();
    // 先量宽：卡片要按内容居中，而不是先摆一个固定宽再塞文字。
    // `.cnt` 徽标占位也要算进按钮宽（设计稿里徽标在 flow 里：gap 8 + 徽标自身宽），
    // 否则角标会压在名称或右内距上——任务数是一位数时徽标 min-width 14，
    // 两位数起按字宽 + 左右各 3px 放宽。
    //
    // Measure first: the card centres on its content instead of a fixed width. The `.cnt`
    // badge takes part in the button width (in the design it sits in flow: 8 px gap plus
    // the badge itself); otherwise the badge lands on the label or the right padding. The
    // badge is min-width 14 with 3 px side padding, widening with digits from two up.
    let running = state.running_jobs();
    let mut widths = Vec::with_capacity(Bench::ALL.len());
    for b in Bench::ALL {
        let label = crate::i18n::tr(b.cn());
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_string(), font(12.0), text());
        let mut w = 6.0 + SN + 8.0 + galley.size().x + 10.0;
        if b == Bench::Tasks && running > 0 {
            let cnt = ui
                .painter()
                .layout_no_wrap(format!("{running}"), font(9.0), warn());
            w += 8.0 + (cnt.size().x + 6.0).max(14.0);
        }
        widths.push(w);
    }
    let total: f32 = widths.iter().sum::<f32>() + 2.0 * (Bench::ALL.len() as f32 - 1.0);
    let card_w = total + PAD * 2.0;
    let card_h = BTN_H + PAD * 2.0;
    let card = Rect::from_min_size(
        Pos2::new(top_center.x - card_w / 2.0, top_center.y),
        Vec2::new(card_w, card_h),
    );
    // 投影：设计稿 `--sh: 0 8px 24px rgba(0,0,0,.45), 0 0 0 1px rgba(255,255,255,.06)`。
    // egui 的 Shadow 会把 blur 摊到四边；卡片落在媒体上，实色底 + 深投影才不糊照片。
    //
    // Shadow per `--sh`; egui's Shadow spreads the blur all round. The card floats over
    // media, so an opaque fill plus a deep shadow keeps the photo from bleeding through.
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

    let mut x = card.left() + PAD;
    let mut pick = None;
    for (i, b) in Bench::ALL.iter().enumerate() {
        let r = Rect::from_min_size(Pos2::new(x, card.top() + PAD), Vec2::new(widths[i], BTN_H));
        x += widths[i] + 2.0;
        let resp = ui.interact(r, ui.make_persistent_id(("bench", b.no())), Sense::click());
        let on = state.bench == *b;
        if on {
            // `.wsbar button.on{background:var(--raised)}`：选中与 hover 同一档抬升，
            // 选中语义由 `.sn` 的强调底承担。
            //
            // `.wsbar button.on` takes `--raised`: selection reads through the accent
            // number badge, so the fill stays on the same raised step as hover.
            p.rect_filled(r, BTN_R, elev());
        } else if resp.hovered() {
            // `.wsbar button:hover{background:var(--raised)}`（旧版用了更亮一档的
            // raised2，和选中态脱节）。
            //
            // `.wsbar button:hover` is also `--raised`; the old code took the brighter
            // raised2 step and drifted apart from the selected state.
            p.rect_filled(r, BTN_R, elev());
        }
        // 编号块。
        let sn = Rect::from_min_size(
            Pos2::new(r.left() + 6.0, r.center().y - SN / 2.0),
            Vec2::splat(SN),
        );
        p.rect_filled(sn, 4.0, if on { accent() } else { field() });
        p.text(
            sn.center(),
            Align2::CENTER_CENTER,
            format!("{}", b.no()),
            font(9.0),
            if on { Color32::WHITE } else { dim() },
        );
        // 名称。
        p.text(
            Pos2::new(sn.right() + 8.0, r.center().y),
            Align2::LEFT_CENTER,
            crate::i18n::tr(b.cn()),
            font(12.0),
            if on || resp.hovered() { text() } else { dim() },
        );
        // 任务数角标（设计稿 `.cnt`）：只在任务工作台有进行中的任务时出现。
        // 徽标贴着右内距（10px）收边，宽度量字定：min-width 14、左右各 3px，
        // 两位数不再挤在 14px 的方框里。
        //
        // Badge per `.cnt`: only when the tasks bench has something running. It closes
        // against the 10 px right padding with a measured width -- min-width 14 plus 3 px
        // per side -- so multi-digit counts no longer squeeze into a fixed 14 px box.
        if *b == Bench::Tasks && running > 0 {
            let cnt_gal = p.layout_no_wrap(format!("{running}"), font(9.0), warn());
            let badge_w = (cnt_gal.size().x + 6.0).max(14.0);
            let badge = Rect::from_min_size(
                Pos2::new(r.right() - 10.0 - badge_w, r.center().y - 7.0),
                Vec2::new(badge_w, 14.0),
            );
            p.rect_filled(
                badge,
                4.0,
                Color32::from_rgba_unmultiplied(245, 184, 61, 41),
            );
            p.text(
                badge.center(),
                Align2::CENTER_CENTER,
                format!("{running}"),
                font(9.0),
                warn(),
            );
        }
        if resp.clicked() {
            pick = Some(*b);
        }
        let tip = format!("{} (Ctrl+Alt+{})", crate::i18n::tr(b.cn()), b.no());
        resp.on_hover_text(tip);
    }
    if let Some(b) = pick {
        state.set_bench(b);
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
        font(15.0),
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
