//! 画布上的浮动卡片：直方图（左上）与操作提示。
//!
//! 设计稿 `docs/aevum-v12-timeline.html` 的 `.histcard` 是**浮在画布上的卡片**
//! （`top:12px;left:12px`），不是贴在检查器里的常驻条。把它搬到画布
//! 左上，视线落点第一件事就是数据——这是「数据先于眼睛」的像素表达。
//! 用户要求「不要固定、可以改大小、可以移动、有最小尺寸」，又嫌它「像正方形」：
//! 默认改成更宽扁的 290×92，顶部把手可拖、右下角手柄可缩放（最小 220×80，
//! 最大不超过画布的 40%）。
//!
//! 卡片底必须是**实色中性灰**：浮在照片上若半透明或模糊，等于在色彩判断的参照系
//! 里掺进照片本身的颜色（技能纪律 2）。这里用 `panel()` 实底，不透明、不做
//! backdrop。
//!
//! Floating cards over the canvas: the histogram (top-left) plus action hints.
//!
//! The design's `.histcard` is a **card floating over the canvas**
//! (`top:12px;left:12px`), not a strip pinned inside the inspector.
//! Moving it to the canvas's top-left puts data where the eye lands first -- the
//! pixel-level form of "data before the eye". The user asked for a card that is
//! "not fixed, resizable, movable, with a minimum size" and disliked its "square"
//! look: it now starts wider and flatter at 290x92, with a top drag strip and a
//! bottom-right resize handle (min 220x80, never beyond 40% of the canvas).
//!
//! The card fill must be an **opaque neutral grey**: a translucent or blurred card over
//! a photo injects the photo's own colour into the reference frame for colour judgement
//! (skill discipline 2). This uses an opaque `panel()` fill, no backdrop.

use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use super::dock;
use crate::state::AppState;
use crate::theme::*;

/// 直方图卡片最小宽 220、最小高 80（用户要求「最小弄成这个样子」）。
///
/// Histogram card minimum: 220 wide, 80 tall (the user asked for a floor).
const HIST_MIN_W: f32 = 220.0;
const HIST_MIN_H: f32 = 80.0;
/// 卡片最大不超过画布宽/高的 40%：留足画布给照片。
///
/// The card never exceeds 40% of the canvas width / height: leave the photo room.
const HIST_MAX_FRAC: f32 = 0.4;
/// 顶部拖动条高度（落在卡片上内距里，不与绘图区抢位置）。
///
/// Height of the top drag strip (it lives in the top padding, clear of the plot).
const HIST_GRIP_H: f32 = 10.0;
/// 右下角缩放手柄边长。
///
/// Edge length of the bottom-right resize handle.
const HIST_RESIZE: f32 = 14.0;
/// EXIF 行高度：`dock::histogram` 在槽内画 7px 间距 + 14px 行。
///
/// Height of the EXIF row: `dock::histogram` paints a 7 px gap plus a 14 px row.
const HIST_EXIF_H: f32 = 21.0;
/// 卡片距画布边 12：设计稿 `top:12px;left:12px`。
///
/// 12 px from the canvas edge, per the design's `top:12px;left:12px`.
const CARD_INSET: f32 = 12.0;
/// 卡片内边距 10：设计稿 `.histcard{padding:10px}`。
///
/// 10 px inner padding, per the design's `.histcard{padding:10px}`.
const CARD_PAD: f32 = 10.0;

/// 在画布区域 `canvas` 上画浮动直方图卡片（默认左上，可拖动、可缩放）。
///
/// 返回卡片矩形，供调用方把画布内容让开（本函数只画，不改布局）；空态返回
/// `Rect::NOTHING`。
///
/// Draw the floating histogram card over `canvas` (top-left by default, movable and
/// resizable). Returns the card rect so the caller can keep canvas content clear of it
/// (this only paints; it does not reflow anything); `Rect::NOTHING` when empty.
pub(super) fn histogram_card(state: &mut AppState, ui: &mut Ui, canvas: Rect) -> Rect {
    // 只有真有直方图才摆卡：空态摆一张空卡只是多一块挡照片的灰。
    //
    // Only show the card when there is a histogram: an empty card is just another grey
    // block covering the photo.
    if state.hist.is_none() {
        return Rect::NOTHING;
    }

    // 宽高夹在 [最小, 画布 40%]。窗口极小时上限可能低于下限，用 max 保底，
    // 免得 clamp 的上下界倒挂（egui 的 clamp 遇到 min>max 会 panic）。
    //
    // Clamp width / height to [min, 40% of canvas]. In a tiny window the cap can fall
    // below the floor, so lift it with max -- `clamp` panics when min > max.
    let max_w = (canvas.width() * HIST_MAX_FRAC).max(HIST_MIN_W);
    let max_h = (canvas.height() * HIST_MAX_FRAC).max(HIST_MIN_H);
    let w = state.hist_w.clamp(HIST_MIN_W, max_w);
    let h = state.hist_h.clamp(HIST_MIN_H, max_h);

    // 默认贴画布左上 12px；拖过用 hist_pos，逐帧夹在画布内（留 12px）。
    //
    // Default top-left with a 12 px inset; after a drag use hist_pos, clamped inside
    // the canvas with a 12 px gutter every frame.
    let min_pos = Pos2::new(canvas.left() + CARD_INSET, canvas.top() + CARD_INSET);
    let max_pos = Pos2::new(
        (canvas.right() - CARD_INSET - w).max(min_pos.x),
        (canvas.bottom() - CARD_INSET - h).max(min_pos.y),
    );
    let pos = state
        .hist_pos
        .map(|q| {
            Pos2::new(
                q.x.clamp(min_pos.x, max_pos.x),
                q.y.clamp(min_pos.y, max_pos.y),
            )
        })
        .unwrap_or(min_pos);
    let card = Rect::from_min_size(pos, Vec2::new(w, h));

    let p = ui.painter().clone();
    // 卡片底：实色 panel，圆角 radius_panel，外描边 border_strong——
    // 浮在照片上要读得出「这是一张浮起来的卡」，靠明度差与描边，不靠模糊。
    //
    // Card fill: opaque `panel`, `radius_panel` corners, `border_strong` outline. On a
    // photo it must read as "a card floating above", via value contrast and outline,
    // never blur.
    p.rect_filled(card, radius_panel(), panel());
    p.rect_stroke(
        card,
        radius_panel(),
        Stroke::new(1.0, border_strong()),
        StrokeKind::Inside,
    );
    // 与检查器同一套上沿光：两张浮卡共享材质，不能一张有边缘光一张没有。
    //
    // Same top edge light as the inspector: both floating cards share one material.
    p.line_segment(
        [
            Pos2::new(card.left() + 12.0, card.top() + 0.5),
            Pos2::new(card.right() - 12.0, card.top() + 0.5),
        ],
        Stroke::new(1.0, Color32::from_white_alpha(20)),
    );

    let inner = card.shrink(CARD_PAD);
    // 绘图区高度随卡高变化：卡高 - EXIF 行 - 上下内距。dock::histogram 会
    // 用 available_height().min(94) 吃满这个槽位，再在自己下方画 21px EXIF，
    // 正好落在 inner 底边；槽位超过 94 时它只画 94，卡内留白、不溢出卡外。
    //
    // Plot height follows the card height: card height - EXIF row - paddings.
    // `dock::histogram` fills this slot with `available_height().min(94)` and paints
    // its 21 px EXIF row below, landing exactly on the inner bottom edge; when the
    // slot exceeds 94 it paints only 94, leaving the rest blank inside the card,
    // never spilling out.
    let plot_h = h - HIST_EXIF_H - CARD_PAD * 2.0;
    let hist_rect = Rect::from_min_size(inner.left_top(), Vec2::new(inner.width(), plot_h));
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt("hist-card-body")
            .max_rect(hist_rect),
        |ui| {
            dock::histogram(ui, state);
        },
    );

    // ---- 顶部拖动条：落在卡片上内距 10px 里，不与绘图区/EXIF/角标重叠 ----
    // 它是独立小矩形、独立 id；dock::histogram 的剪切角标从 inner.top() 起，
    // 这条从 card.top()+1 起、到 +9 止，压根不碰卡内任何控件。
    //
    // Top drag strip: it lives in the card's top 10 px padding, clear of the plot,
    // EXIF row and clipping wedges. Independent small rect and id; `dock::histogram`'s
    // wedges start at inner.top(), while this strip runs from card.top()+1 to +9 and
    // never touches any widget inside the card.
    let grip = Rect::from_min_size(
        Pos2::new(card.left() + CARD_PAD, card.top() + 1.0),
        Vec2::new((w - CARD_PAD * 2.0).max(1.0), HIST_GRIP_H - 2.0),
    );
    let drag = ui.interact(grip, ui.make_persistent_id("hist-drag"), Sense::drag());
    let drag_delta = drag.drag_delta();
    let drag_on = drag.dragged();
    let drag_hover = drag.hovered();
    if drag_hover || drag_on {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    // 中间一排小点：一眼认得出「这里能抓」。
    //
    // A row of dots in the middle: reads as "grab here" at a glance.
    for k in 0..5 {
        p.circle_filled(
            Pos2::new(grip.center().x + (k as f32 - 2.0) * 5.0, grip.center().y),
            1.0,
            if drag_on { accent() } else { dim() },
        );
    }
    if drag_on {
        state.hist_pos = Some(Pos2::new(
            (pos.x + drag_delta.x).clamp(min_pos.x, max_pos.x),
            (pos.y + drag_delta.y).clamp(min_pos.y, max_pos.y),
        ));
    }
    drag.on_hover_text(crate::i18n::t!("拖动直方图卡片"));

    // ---- 右下角 resize 手柄：写回 hist_w / hist_h ----
    //
    // Bottom-right resize handle: writes back hist_w / hist_h.
    let corner = Rect::from_min_size(
        Pos2::new(card.right() - HIST_RESIZE, card.bottom() - HIST_RESIZE),
        Vec2::splat(HIST_RESIZE),
    );
    let rz = ui.interact(corner, ui.make_persistent_id("hist-resize"), Sense::drag());
    let rz_delta = rz.drag_delta();
    let rz_on = rz.dragged();
    let rz_hover = rz.hovered();
    if rz_hover || rz_on {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    // 三道斜线角标。
    //
    // Three diagonal ticks.
    for k in 0..3 {
        let t = 3.0 + k as f32 * 4.0;
        p.line_segment(
            [
                Pos2::new(corner.right() - 2.0 - t, corner.bottom() - 2.0),
                Pos2::new(corner.right() - 2.0, corner.bottom() - 2.0 - t),
            ],
            Stroke::new(1.0, if rz_on { accent() } else { dim() }),
        );
    }
    if rz_on {
        state.hist_w = (state.hist_w + rz_delta.x).clamp(HIST_MIN_W, max_w);
        state.hist_h = (state.hist_h + rz_delta.y).clamp(HIST_MIN_H, max_h);
    }
    rz.on_hover_text(crate::i18n::t!("拖动调整直方图大小"));

    card
}

/// 画布左上角之外的小提示卡：仅在无序列时显示「拖入照片开始」。
///
/// A small hint card beside the histogram: "drop photos to start" when no sequence is
/// loaded.
pub(super) fn empty_hint(state: &AppState, ui: &mut Ui, canvas: Rect) -> Rect {
    if state.frame_count() > 0 {
        return Rect::NOTHING;
    }
    let p = ui.painter().clone();
    let text = crate::i18n::t!("把照片或文件夹拖进来开始");
    let g = p.layout_no_wrap(text.to_owned(), font(SIZE_LABEL), muted());
    let w = g.size().x + CARD_PAD * 2.0;
    let h = 32.0;
    let rect = Rect::from_min_size(
        Pos2::new(canvas.center().x - w * 0.5, canvas.center().y - h * 0.5),
        egui::vec2(w, h),
    );
    p.rect_filled(rect, radius_control(), panel());
    p.rect_stroke(
        rect,
        radius_control(),
        Stroke::new(1.0, border()),
        StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        font(SIZE_LABEL),
        muted(),
    );
    let _ = RichText::new("");
    rect
}
