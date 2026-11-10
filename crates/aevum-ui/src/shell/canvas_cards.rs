//! 画布上的浮动卡片：直方图（左上）与操作提示。
//!
//! 设计稿 `docs/aevum-v12-timeline.html` 的 `.histcard` 是**浮在画布上的卡片**
//! （`top:12px;left:12px;width:250px`），不是贴在检查器里的常驻条。把它搬到画布
//! 左上，视线落点第一件事就是数据——这是「数据先于眼睛」的像素表达。
//!
//! 卡片底必须是**实色中性灰**：浮在照片上若半透明或模糊，等于在色彩判断的参照系
//! 里掺进照片本身的颜色（技能纪律 2）。这里用 `panel()` 实底，不透明、不做
//! backdrop。
//!
//! Floating cards over the canvas: the histogram (top-left) plus action hints.
//!
//! The design's `.histcard` is a **card floating over the canvas**
//! (`top:12px;left:12px;width:250px`), not a strip pinned inside the inspector.
//! Moving it to the canvas's top-left puts data where the eye lands first -- the
//! pixel-level form of "data before the eye".
//!
//! The card fill must be an **opaque neutral grey**: a translucent or blurred card over
//! a photo injects the photo's own colour into the reference frame for colour judgement
//! (skill discipline 2). This uses an opaque `panel()` fill, no backdrop.

use egui::{Align2, Pos2, Rect, RichText, Stroke, StrokeKind, Ui, Vec2};

use super::dock;
use crate::state::AppState;
use crate::theme::*;

/// 直方图卡片宽 250：设计稿 `.histcard{width:250px}`。
///
/// Histogram card 250 wide, per the design's `.histcard{width:250px}`.
const CARD_W: f32 = 250.0;
/// 卡片距画布边 12：设计稿 `top:12px;left:12px`。
///
/// 12 px from the canvas edge, per the design's `top:12px;left:12px`.
const CARD_INSET: f32 = 12.0;
/// 卡片内边距 10：设计稿 `.histcard{padding:10px}`。
///
/// 10 px inner padding, per the design's `.histcard{padding:10px}`.
const CARD_PAD: f32 = 10.0;

/// 在画布区域 `canvas` 的左上角画浮动直方图卡片。
///
/// 返回卡片矩形，供调用方把画布内容让开（本函数只画，不改布局）。
///
/// Draw the floating histogram card at the canvas's top-left corner. Returns the card
/// rect so the caller can keep canvas content clear of it (this only paints; it does
/// not reflow anything).
pub(super) fn histogram_card(state: &mut AppState, ui: &mut Ui, canvas: Rect) -> Rect {
    // 只有真有直方图才摆卡：空态摆一张空卡只是多一块挡照片的灰。
    //
    // Only show the card when there is a histogram: an empty card is just another grey
    // block covering the photo.
    if state.hist.is_none() {
        return Rect::NOTHING;
    }

    let p = ui.painter().clone();
    let hist_h = 63.0; // 设计稿 `.histcard img{height:63px}`
    // EXIF 行由 dock::histogram 自己画（凹槽下 7px + 14px 行），卡体 =
    // 63 + 21，加上下 padding 20 正好 104 —— 与设计稿 .histcard 同高。
    //
    // The EXIF row is drawn by dock::histogram itself (7 px + 14 px under the well);
    // body = 63 + 21, plus 20 px padding = 104 -- exactly the design's card height.
    let body_h = hist_h + 21.0;
    let card = Rect::from_min_size(
        Pos2::new(canvas.left() + CARD_INSET, canvas.top() + CARD_INSET),
        Vec2::new(CARD_W, body_h + CARD_PAD * 2.0),
    );

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

    let inner = card.shrink(CARD_PAD);

    // 直方图本体：设计稿 `.histcard img{width:230;height:63}`——宽而扁的一格，
    // 下方紧跟 EXIF。编号走 dock 里那套三通道山形（唯一允许大面积彩色的区域）。
    //
    // Histogram body: the design's `.histcard img{width:230;height:63}` -- one wide,
    // short cell with the EXIF row directly below. Reuses the dock's three-channel
    // mountains (the one place large colour areas are allowed).
    let hist_rect = Rect::from_min_size(
        Pos2::new(inner.left(), inner.top()),
        Vec2::new(inner.width(), hist_h),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt("hist-card-body")
            .max_rect(hist_rect),
        |ui| {
            dock::histogram(ui, state);
        },
    );

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
