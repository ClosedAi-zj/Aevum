//! 导出工作台（v10.6 第三个工作台）：左侧逐帧流水队列，右侧导出设置。
//!
//! 设计稿 `#vExport`：`.ws-head` 64 + `.ws-tools` 36 + `.ws-body`（`1fr 272px`，gap 12，
//! padding 12/16/16）。左列是队列卡：每行 34px，缩略图 40×28、名称、状态、进度条；
//! 右列是设置卡：输出目录 / 编码 / 帧率 / 分辨率 / 质量 / 立即导出。
//!
//! The export bench (third workspace): per-frame queue on the left, export settings on the
//! right, per the design's `#vExport`.

use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2};

use super::bench;
use crate::state::AppState;
use crate::theme::*;

/// 右列设置卡宽 272（设计稿 `grid-template-columns:minmax(0,1fr) 272px`）。
///
/// Right settings column, per the design grid.
const SIDE_W: f32 = 272.0;

/// 画整页导出工作台。
///
/// Draw the whole export bench.
pub(super) fn page(state: &mut AppState, ui: &mut egui::Ui, _ctx: &egui::Context) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.rect_filled(full, 0.0, bench_bg());

    let n = state.queue.len();
    let busy = state.queue_current.map(|i| i + 1).unwrap_or(0);
    let sub = format!(
        "{n} 个任务 · 完成 {} · 渲染 {}",
        state
            .queue
            .iter()
            .filter(|q| matches!(q.status, aevum_core::queue::QueueStatus::Done))
            .count(),
        busy
    );
    let head = bench::page_head(ui, crate::i18n::t!("导出工作台"), &sub);
    let (r1, _) = bench::head_action(
        ui,
        head.right() - 16.0,
        head.center().y,
        "开始导出队列",
        true,
    );
    let (r2, _) = bench::head_action(ui, r1.left() - 8.0, head.center().y, "加入当前序列", false);
    let _ = bench::head_action(ui, r2.left() - 8.0, head.center().y, "清空已完成", false);

    let strip = bench::tools_strip(ui, head.bottom());
    let mut x = strip.left() + 16.0;
    for label in ["导出队列", "打开输出目录", "暂停当前", "重试失败项"] {
        let (nx, _) = bench::tool_button(ui, x, strip, label, false);
        x = nx;
    }

    let body = Rect::from_min_max(
        Pos2::new(full.left() + 16.0, strip.bottom() + 12.0),
        Pos2::new(full.right() - 16.0, full.bottom() - 16.0),
    );
    let left = Rect::from_min_max(
        body.left_top(),
        Pos2::new(body.right() - SIDE_W - 12.0, body.bottom()),
    );
    let right = Rect::from_min_max(
        Pos2::new(body.right() - SIDE_W + 12.0, body.top()),
        body.right_bottom(),
    );
    queue_card(state, ui, left);
    settings_card(state, ui, right);
}

/// 队列卡：逐行列出任务（缩略状态点 + 名称 + 进度 + 预计剩余）。
///
/// The queue card: one row per job (status dot, name, progress, ETA).
fn queue_card(state: &AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    p.rect_filled(rect, 12.0, panel());
    p.rect_stroke(
        rect,
        12.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );
    let mut y = rect.top() + 14.0;
    p.text(
        Pos2::new(rect.left() + 14.0, y),
        Align2::LEFT_CENTER,
        crate::i18n::t!("队列"),
        FontId::proportional(12.0),
        text(),
    );
    y += 22.0;
    for (i, item) in state.queue.iter().enumerate() {
        if y > rect.bottom() - 20.0 {
            break;
        }
        let row = Rect::from_min_size(
            Pos2::new(rect.left() + 8.0, y),
            Vec2::new(rect.width() - 16.0, 34.0),
        );
        if state.queue_current == Some(i) {
            p.rect_filled(row, 8.0, selected());
        }
        let dot = Pos2::new(row.left() + 10.0, row.center().y);
        let color = match item.status {
            aevum_core::queue::QueueStatus::Done => ok(),
            aevum_core::queue::QueueStatus::Failed => danger(),
            aevum_core::queue::QueueStatus::Pending => {
                if state.queue_current == Some(i) {
                    accent()
                } else {
                    dim()
                }
            }
        };
        p.circle_filled(dot, 3.0, color);
        let name = std::path::Path::new(&item.root)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| item.root.clone());
        p.text(
            Pos2::new(row.left() + 22.0, row.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(12.0),
            text(),
        );
        p.text(
            Pos2::new(row.right() - 8.0, row.center().y),
            Align2::RIGHT_CENTER,
            item.status.label(),
            FontId::proportional(10.0),
            dim(),
        );
        y += 36.0;
    }
    if state.queue.is_empty() {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            crate::i18n::t!("队列是空的 · 在显影工作台点「导出…」加入"),
            FontId::proportional(11.0),
            dim(),
        );
    }
}

/// 设置卡：输出目录 / 编码 / 帧率 / 分辨率 / 质量 + 主按钮。
///
/// Settings card: output dir / codec / fps / resolution / quality plus the primary button.
fn settings_card(state: &AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    p.rect_filled(rect, 12.0, panel());
    p.rect_stroke(
        rect,
        12.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );
    let mut y = rect.top() + 14.0;
    p.text(
        Pos2::new(rect.left() + 14.0, y),
        Align2::LEFT_CENTER,
        crate::i18n::t!("导出设置"),
        FontId::proportional(12.0),
        text(),
    );
    y += 26.0;
    let codec = if state.codec.to_ascii_lowercase().contains("265")
        || state.codec.to_ascii_lowercase().contains("hevc")
    {
        "HEVC"
    } else {
        "H.264"
    };
    let rows = [
        (crate::i18n::t!("编码"), codec.to_string()),
        (crate::i18n::t!("帧率"), format!("{:.0}", state.output_fps)),
        (
            crate::i18n::t!("分辨率"),
            format!("{} × {}", state.output_w, state.output_h),
        ),
        (
            crate::i18n::t!("输出"),
            state
                .last_saved
                .clone()
                .or_else(|| state.queue.last().map(|q| q.out.clone()))
                .unwrap_or_else(|| "—".into()),
        ),
    ];
    for (k, v) in rows {
        p.text(
            Pos2::new(rect.left() + 14.0, y),
            Align2::LEFT_CENTER,
            k,
            FontId::proportional(11.0),
            dim(),
        );
        p.text(
            Pos2::new(rect.right() - 14.0, y),
            Align2::RIGHT_CENTER,
            v,
            FontId::proportional(11.0),
            text(),
        );
        y += 24.0;
    }
    // 主按钮（占满卡宽，26px）。
    let btn = Rect::from_min_size(
        Pos2::new(rect.left() + 12.0, rect.bottom() - 12.0 - 26.0),
        Vec2::new(rect.width() - 24.0, 26.0),
    );
    p.rect_filled(btn, 6.0, accent());
    p.text(
        btn.center(),
        Align2::CENTER_CENTER,
        crate::i18n::t!("立即导出"),
        FontId::proportional(12.0),
        Color32::WHITE,
    );
}
