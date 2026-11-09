//! 帧表卡：表头 32 + 范围条 30 + 虚拟化行 + 页脚 28。
//!
//! 设计稿 `#vTable .ftablecard`：12px 圆角卡片，表头 `th` 26px，行 26px，
//! 选中行淡蓝底 + 左侧强调条；列宽见 [`COLS`]。
//!
//! The frame-table card: 32 px header + 30 px scope bar + virtualised rows + 28 px footer.
//! Design `#vTable .ftablecard`: 12 px radius, 26 px th, 26 px rows, selected row in a pale
//! blue wash with an accent bar on the left; column widths in [`COLS`].

use egui::{
    Align, Align2, Color32, FontId, Layout, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};

use crate::state::AppState;
use crate::theme::*;

/// 列：标题、宽（px）、对齐。
///
/// Columns: title, width and alignment.
const COLS: [(&str, f32, Align); 10] = [
    ("编号", 64.0, Align::LEFT),
    ("亮度", 56.0, Align::RIGHT),
    ("间隔", 52.0, Align::RIGHT),
    ("快门", 64.0, Align::RIGHT),
    ("光圈", 52.0, Align::RIGHT),
    ("ISO", 48.0, Align::RIGHT),
    ("焦距", 52.0, Align::RIGHT),
    ("文件名", 168.0, Align::LEFT),
    ("尺寸", 88.0, Align::RIGHT),
    ("日期 / 时间", 132.0, Align::RIGHT),
];

/// 表头高 32、行高 26、页脚 28。
///
/// Header 32, row 26, footer 28.
const HEAD: f32 = 32.0;
const ROW: f32 = 26.0;
const FOOT: f32 = 28.0;

/// 画整张帧表卡。
///
/// Draw the frame-table card.
pub(super) fn card(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    if rect.width() < 120.0 || rect.height() < 80.0 {
        return;
    }
    let p = ui.painter().clone();
    p.rect_filled(rect, 12.0, panel());
    p.rect_stroke(
        rect,
        12.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );

    // 表头：列名 + 右侧汇总（设计稿 `th` 右对齐的计数）。
    //
    // Header: column names plus the right-aligned count.
    let head = Rect::from_min_size(rect.left_top(), Vec2::new(rect.width(), HEAD));
    p.line_segment(
        [head.left_bottom(), head.right_bottom()],
        Stroke::new(1.0, divider()),
    );
    let mut x = head.left() + 12.0;
    for (title, w, align) in COLS {
        let at = match align {
            Align::RIGHT => Pos2::new(x + w - 4.0, head.center().y),
            _ => Pos2::new(x + 4.0, head.center().y),
        };
        let a2 = if matches!(align, Align::RIGHT) {
            Align2::RIGHT_CENTER
        } else {
            Align2::LEFT_CENTER
        };
        p.text(at, a2, title, FontId::proportional(10.0), dim());
        x += w;
    }

    // 行区：虚拟化，只画可见行。
    //
    // Rows: virtualised, only the visible ones are painted.
    let rows_rect = Rect::from_min_max(
        Pos2::new(rect.left(), head.bottom()),
        Pos2::new(rect.right(), rect.bottom() - FOOT),
    );
    let n = state.frame_count();
    let mut click = None;
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rows_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(&mut child, ROW, n, |ui, range| {
            for i in range {
                let (row, resp) =
                    ui.allocate_exact_size(Vec2::new(rows_rect.width(), ROW), Sense::click());
                let p = ui.painter();
                if state.selected == i {
                    p.rect_filled(row, 0.0, selected());
                    p.rect_filled(
                        Rect::from_min_size(row.left_top(), Vec2::new(2.0, row.height())),
                        0.0,
                        accent(),
                    );
                } else if resp.hovered() {
                    p.rect_filled(row, 0.0, row_hover());
                }
                if i % 2 == 1 && state.selected != i {
                    p.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(row.left(), row.top()),
                            Vec2::new(row.width(), 1.0),
                        ),
                        0.0,
                        Color32::from_white_alpha(5),
                    );
                }
                let cy = row.center().y;
                let mut x = row.left() + 12.0;
                let ex = state.exif.get(i);
                let m = state.metrics.get(i).and_then(|m| m.as_ref());
                let name = state
                    .seq
                    .as_ref()
                    .and_then(|s| s.frames.get(i))
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                let cells = [
                    format!("{:04}", i + 1),
                    m.map(|m| format!("{:.3}", m.mean))
                        .unwrap_or_else(|| "—".into()),
                    "—".to_string(),
                    ex.and_then(|e| e.shutter_text())
                        .unwrap_or_else(|| "—".into()),
                    ex.and_then(|e| e.fnum())
                        .map(|v| format!("f/{v:.1}"))
                        .unwrap_or_else(|| "—".into()),
                    ex.and_then(|e| e.iso())
                        .map(|v| format!("{v:.0}"))
                        .unwrap_or_else(|| "—".into()),
                    ex.and_then(|e| e.focal_mm())
                        .map(|v| format!("{v:.0} mm"))
                        .unwrap_or_else(|| "—".into()),
                    name,
                    "—".to_string(),
                    ex.and_then(|e| e.datetime()).unwrap_or("—").to_string(),
                ];
                for (k, (_, w, align)) in COLS.iter().enumerate() {
                    let value = &cells[k];
                    let (at, a2, color) = match *align {
                        Align::RIGHT => (Pos2::new(x + w - 4.0, cy), Align2::RIGHT_CENTER, text()),
                        _ => (Pos2::new(x + 4.0, cy), Align2::LEFT_CENTER, muted()),
                    };
                    p.text(at, a2, value, FontId::proportional(11.0), color);
                    x += w;
                }
                if resp.clicked() {
                    click = Some(i);
                }
            }
        });
    if let Some(i) = click {
        state.selected = i;
    }

    // 页脚：已选 / 关键帧 / 占位提示（设计稿底部一条 10px 说明）。
    //
    // Footer: selection, keyframes and a hint, as in the design's 10 px caption.
    let foot = Rect::from_min_size(
        Pos2::new(rect.left(), rect.bottom() - FOOT),
        Vec2::new(rect.width(), FOOT),
    );
    p.line_segment(
        [foot.left_top(), foot.right_top()],
        Stroke::new(1.0, divider()),
    );
    p.text(
        Pos2::new(foot.left() + 12.0, foot.center().y),
        Align2::LEFT_CENTER,
        format!(
            "第 {} / {} 帧 · 关键帧 {}",
            state.selected + 1,
            n.max(1),
            state.keyframes.len()
        ),
        FontId::proportional(10.0),
        dim(),
    );
}
