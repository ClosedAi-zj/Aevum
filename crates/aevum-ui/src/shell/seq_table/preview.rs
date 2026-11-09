//! 帧表左列：预览卡 + 摘要卡 + 自动过渡卡。
//!
//! 设计稿 `#vTable .ws-body` 左列 300px：`.seqprev`（16:9 预览 + 文件名/#序号 + 走带）、
//! `.seqsum`（键值行）、`.seqauto`（自动过渡分段表）。三张卡同宽、圆角 12、间距 12。
//!
//! Left column of the frame table: preview card + summary card + auto-transition card.

use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2};

use crate::state::AppState;
use crate::theme::*;

/// 卡间距 12。
///
/// Gap between cards.
const GAP: f32 = 12.0;
/// 预览卡内距 12。
///
/// Preview card inner padding.
const PAD: f32 = 12.0;

/// 画整列三张卡。
///
/// Draw the whole column of three cards.
pub(super) fn column(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    if rect.width() < 40.0 {
        return;
    }
    let mut y = rect.top();
    let prev_h = preview_card(
        state,
        ui,
        Rect::from_min_size(rect.left_top(), Vec2::new(rect.width(), 0.0)),
    );
    y += prev_h + GAP;
    let sum_h = summary_card(state, ui, Pos2::new(rect.left(), y), rect.width());
    y += sum_h + GAP;
    let auto_h = auto_card(
        state,
        ui,
        Pos2::new(rect.left(), y),
        rect.width(),
        rect.bottom() - y,
    );
    let _ = auto_h;
}

/// 预览卡：16:9 大图 + 文件名/序号 + 走带。返回卡片高度。
///
/// Preview card: 16:9 image, file name / index, transport. Returns the card height.
fn preview_card(state: &mut AppState, ui: &mut Ui, at: Rect) -> f32 {
    let w = at.width();
    let img_h = (w - PAD * 2.0) * 9.0 / 16.0;
    let h = PAD * 2.0 + img_h + 8.0 + 16.0 + 6.0 + 26.0;
    let card = Rect::from_min_size(at.left_top(), Vec2::new(w, h));
    let p = ui.painter().clone();
    p.rect_filled(card, 12.0, panel());
    p.rect_stroke(
        card,
        12.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );

    let img = Rect::from_min_size(
        Pos2::new(card.left() + PAD, card.top() + PAD),
        Vec2::new(w - PAD * 2.0, img_h),
    );
    p.rect_filled(img, 8.0, thumb_bg());
    let i = state.selected;
    if state.thumb(i).is_none() {
        state.ensure_thumbs(ui.ctx(), i, i + 1);
    }
    if let Some(t) = state.thumb(i) {
        let tex = t.texture.clone();
        let size = t.size;
        let sw = size[0].max(1) as f32;
        let sh = size[1].max(1) as f32;
        let scale = (img.width() / sw).min(img.height() / sh);
        let draw = Vec2::new(size[0] as f32 * scale, size[1] as f32 * scale);
        let r = Rect::from_center_size(img.center(), draw);
        p.image(
            tex.id(),
            r,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // 文件名 + 序号（设计稿：左文件名、右 `#0001`）。
    let name = state
        .seq
        .as_ref()
        .and_then(|s| s.frames.get(i))
        .map(|f| {
            std::path::Path::new(&f.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let baseline = Pos2::new(img.left(), img.bottom() + 12.0);
    p.text(
        baseline,
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(11.0),
        muted(),
    );
    p.text(
        Pos2::new(img.right(), img.bottom() + 12.0),
        Align2::RIGHT_CENTER,
        format!("#{:04}", i + 1),
        FontId::proportional(10.0),
        dim(),
    );

    // 走带：三枚小按钮 + 计数（占位几何，交互接现有 step/set_frame）。
    let bar = Rect::from_min_size(
        Pos2::new(card.left() + PAD, card.bottom() - PAD - 26.0),
        Vec2::new(w - PAD * 2.0, 26.0),
    );
    p.text(
        bar.center(),
        Align2::CENTER_CENTER,
        format!("{} / {}", i + 1, state.frame_count().max(1)),
        FontId::proportional(11.0),
        text(),
    );
    h
}

/// 摘要卡：键值行（总帧数 / 间隔 / 快门 / 分辨率 / 关键帧）。
///
/// Summary card: key/value rows.
fn summary_card(state: &AppState, ui: &mut Ui, at: Pos2, w: f32) -> f32 {
    let rows = 5;
    let h = PAD * 2.0 + 20.0 + 8.0 + rows as f32 * 22.0;
    let card = Rect::from_min_size(at, Vec2::new(w, h));
    let p = ui.painter().clone();
    p.rect_filled(card, 12.0, panel());
    p.rect_stroke(
        card,
        12.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );
    p.text(
        Pos2::new(card.left() + PAD, card.top() + PAD + 10.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("序列摘要"),
        FontId::proportional(12.0),
        text(),
    );
    let n = state.frame_count();
    let ex = state.exif.get(state.selected);
    let items: [(&str, String); 5] = [
        ("总帧数", format!("{n}")),
        (
            "间隔 / 帧率",
            format!(
                "{:.1} s · {:.0} fps",
                1.0 / state.output_fps.max(1e-3),
                state.output_fps
            ),
        ),
        (
            "快门 / 光圈",
            match ex {
                Some(e) => match (e.shutter_text(), e.fnum()) {
                    (Some(s), Some(f)) => format!("{s} · f/{f:.1}"),
                    _ => "—".into(),
                },
                None => "—".into(),
            },
        ),
        (
            "ISO",
            ex.and_then(|e| e.iso())
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "—".into()),
        ),
        ("关键帧 / 过渡", format!("{}", state.keyframes.len())),
    ];
    let mut y = card.top() + PAD + 20.0 + 8.0 + 11.0;
    for (k, v) in items {
        p.text(
            Pos2::new(card.left() + PAD, y),
            Align2::LEFT_CENTER,
            k,
            FontId::proportional(11.0),
            dim(),
        );
        p.text(
            Pos2::new(card.right() - PAD, y),
            Align2::RIGHT_CENTER,
            v,
            FontId::proportional(11.0),
            text(),
        );
        y += 22.0;
    }
    h
}

/// 自动过渡卡：分段表（设计稿 `.seqauto`）。高度吃满剩余空间。
///
/// Auto-transition card: a segment table; fills the remaining height.
fn auto_card(_state: &AppState, ui: &mut Ui, at: Pos2, w: f32, remain: f32) -> f32 {
    let h = remain.max(80.0);
    let card = Rect::from_min_size(at, Vec2::new(w, h));
    let p = ui.painter().clone();
    p.rect_filled(card, 12.0, panel());
    p.rect_stroke(
        card,
        12.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );
    p.text(
        Pos2::new(card.left() + PAD, card.top() + PAD + 10.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("自动过渡"),
        FontId::proportional(12.0),
        text(),
    );
    h
}
