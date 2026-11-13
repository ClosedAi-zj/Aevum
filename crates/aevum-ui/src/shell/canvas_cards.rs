//! 画布上的提示卡：无序列时提示「拖入照片开始」。
//!
//! 直方图**不在这里**：它是调色盘页签下面的常驻条（`dock::dock_panel`），现代与
//! 老式两种布局都在面板顶部，画布上不再摆浮块——此前按设计稿 `.histcard` 做成
//! 叠在照片上的悬浮卡，老式用户看到的是「老式风格却多出一块悬浮直方图」
//! （用户 2026-11-13）。直方图属于面板，画布只留空态提示。
//!
//! Hint cards over the canvas: "drop photos to start" with no sequence loaded.
//!
//! The histogram is deliberately **not** here: it is the permanent strip under the
//! palette's tab row (`dock::dock_panel`), at the top of the panel in both Modern and
//! Legacy, with no floating block over the photo. It used to follow the design's
//! `.histcard` and float over the image, which Legacy users read as "a Legacy layout
//! with a stray floating histogram" (user, 2026-11-13). The histogram belongs to the
//! panel; the canvas keeps only the empty-state hint.

use egui::{Align2, Pos2, Rect, RichText, Stroke, StrokeKind, Ui};

use crate::state::AppState;
use crate::theme::*;

/// 卡片内边距 10：设计稿 `.histcard{padding:10px}`。
///
/// 10 px inner padding, per the design's `.histcard{padding:10px}`.
const CARD_PAD: f32 = 10.0;

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
