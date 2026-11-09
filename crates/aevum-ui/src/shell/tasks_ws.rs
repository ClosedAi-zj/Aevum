//! 后台任务（v10.6 第四个工作台）：任务卡 + 任务摘要 + 通知设置。
//!
//! 设计稿 `#vTasks`：`.ws-head` 64 + `.ws-tools` 36 + `.ws-body`（`1fr 300px`，
//! gap 12）。左列是任务卡栈（进行中 62% 带取消、已完成/失败、队列顺序、最近完成），
//! 右列是「任务摘要」四宫格 + 「通知与测试」开关组。
//!
//! The tasks bench (fourth workspace): task cards plus a summary grid and notification
//! switches, per the design's `#vTasks`.

use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2};

use super::bench;
use crate::state::AppState;
use crate::theme::*;

/// 右列摘要卡宽 300：设计稿 `#vTasks .ws-body{grid-template-columns:minmax(0,1fr) 300px}`。
///
/// Right summary column, per the design grid.
const SIDE_W: f32 = 300.0;

/// 画整页后台任务。
///
/// Draw the whole tasks bench.
pub(super) fn page(state: &mut AppState, ui: &mut egui::Ui, _ctx: &egui::Context) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.rect_filled(full, 0.0, bench_bg());

    let running = state.running_jobs();
    let sub = format!(
        "{} 个任务在跑 · 队列 {} 项 · 导出{}",
        running,
        state.queue.len(),
        if state.export_busy {
            "进行中"
        } else {
            "空闲"
        }
    );
    let head = bench::page_head(ui, crate::i18n::t!("后台任务"), &sub);
    let (r1, _) = bench::head_action(ui, head.right() - 16.0, head.center().y, "全部暂停", false);
    let _ = bench::head_action(ui, r1.left() - 8.0, head.center().y, "清除已完成", false);

    let strip = bench::tools_strip(ui, head.bottom());
    let mut x = strip.left() + 16.0;
    for label in ["导入", "去闪", "对齐", "渲染"] {
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
    task_cards(state, ui, left);
    summary(state, ui, right);
}

/// 任务卡栈：进行中 → 已完成/失败 → 队列顺序 → 最近完成。
///
/// Card stack: running, finished/failed, queue order, recent.
fn task_cards(state: &AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    let mut y = rect.top();
    // 进行中卡。
    let h = 132.0;
    if state.queue_busy || state.export_busy || running_text(state).is_some() {
        let card = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), h));
        p.rect_filled(card, 12.0, panel());
        p.rect_stroke(
            card,
            12.0,
            Stroke::new(1.0, group_ring()),
            StrokeKind::Inside,
        );
        p.text(
            Pos2::new(card.left() + 14.0, card.top() + 20.0),
            Align2::LEFT_CENTER,
            running_text(state).unwrap_or_else(|| crate::i18n::t!("后台任务进行中").to_string()),
            FontId::proportional(13.0),
            text(),
        );
        let (done_n, total_n) = state
            .queue_progress()
            .map(|(d, t, _)| (d, t))
            .unwrap_or((0, 1));
        let pct = if total_n == 0 {
            0
        } else {
            (done_n * 100 / total_n).min(100)
        };
        p.text(
            Pos2::new(card.right() - 14.0, card.top() + 20.0),
            Align2::RIGHT_CENTER,
            format!("{pct}%"),
            FontId::proportional(12.0),
            accent(),
        );
        // 进度条 3px。
        let bar = Rect::from_min_size(
            Pos2::new(card.left() + 14.0, card.top() + 42.0),
            Vec2::new(card.width() - 28.0, 3.0),
        );
        p.rect_filled(bar, 2.0, slider_rail());
        let frac = (pct as f32 / 100.0).clamp(0.0, 1.0);
        p.rect_filled(
            Rect::from_min_size(bar.left_top(), Vec2::new(bar.width() * frac, 3.0)),
            2.0,
            accent(),
        );
        y += h + 12.0;
    } else {
        // 空态：一句话说明这里会有什么，不摆一张空卡。
        //
        // Empty state: one line saying what will appear here, no empty card.
        let card = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), 72.0));
        p.rect_filled(card, 12.0, panel());
        p.rect_stroke(
            card,
            12.0,
            Stroke::new(1.0, group_ring()),
            StrokeKind::Inside,
        );
        p.text(
            card.center(),
            Align2::CENTER_CENTER,
            crate::i18n::t!("没有正在跑的任务 · 导入 / 去闪 / 对齐 / 渲染共用这一页"),
            FontId::proportional(11.0),
            dim(),
        );
        y += 72.0 + 12.0;
    }

    // 最近完成：队列里已完成/失败的项。
    //
    // Recent: the finished / failed queue entries.
    let rest = Rect::from_min_max(
        Pos2::new(rect.left(), y),
        Pos2::new(rect.right(), rect.bottom()),
    );
    if rest.height() > 80.0 {
        p.rect_filled(rest, 12.0, panel());
        p.rect_stroke(
            rest,
            12.0,
            Stroke::new(1.0, group_ring()),
            StrokeKind::Inside,
        );
        p.text(
            Pos2::new(rest.left() + 14.0, rest.top() + 18.0),
            Align2::LEFT_CENTER,
            crate::i18n::t!("最近完成"),
            FontId::proportional(12.0),
            text(),
        );
        let mut ry = rest.top() + 36.0;
        for item in state
            .queue
            .iter()
            .filter(|q| !matches!(q.status, aevum_core::queue::QueueStatus::Pending))
        {
            if ry > rest.bottom() - 16.0 {
                break;
            }
            let name = std::path::Path::new(&item.root)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| item.root.clone());
            p.text(
                Pos2::new(rest.left() + 14.0, ry),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(11.0),
                muted(),
            );
            p.text(
                Pos2::new(rest.right() - 14.0, ry),
                Align2::RIGHT_CENTER,
                item.status.label(),
                FontId::proportional(10.0),
                dim(),
            );
            ry += 22.0;
        }
    }
}

/// 右列：任务摘要四宫格 + 通知与测试。
///
/// Right column: the 2x2 summary grid plus notifications.
fn summary(state: &AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    let h = 148.0;
    let card = Rect::from_min_size(rect.left_top(), Vec2::new(rect.width(), h));
    p.rect_filled(card, 12.0, panel());
    p.rect_stroke(
        card,
        12.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );
    p.text(
        Pos2::new(card.left() + 14.0, card.top() + 18.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("任务摘要"),
        FontId::proportional(12.0),
        text(),
    );
    let total = state.queue.len() as f32;
    let done = state
        .queue
        .iter()
        .filter(|q| matches!(q.status, aevum_core::queue::QueueStatus::Done))
        .count() as f32;
    let failed = state
        .queue
        .iter()
        .filter(|q| matches!(q.status, aevum_core::queue::QueueStatus::Failed))
        .count() as f32;
    let cells = [
        (total, crate::i18n::t!("全部任务")),
        (state.running_jobs() as f32, crate::i18n::t!("进行中")),
        (done, crate::i18n::t!("已完成")),
        (failed, crate::i18n::t!("部分失败")),
    ];
    for (i, (v, label)) in cells.iter().enumerate() {
        let cx = card.left() + 14.0 + (i % 2) as f32 * (card.width() - 28.0) / 2.0;
        let cy = card.top() + 60.0 + (i / 2) as f32 * 46.0;
        p.text(
            Pos2::new(cx, cy),
            Align2::LEFT_CENTER,
            format!("{v:.0}"),
            FontId::proportional(18.0),
            if i == 3 && failed > 0.0 {
                danger()
            } else {
                text()
            },
        );
        p.text(
            Pos2::new(cx, cy + 18.0),
            Align2::LEFT_CENTER,
            *label,
            FontId::proportional(10.0),
            dim(),
        );
    }

    // 通知与测试卡。
    let top = card.bottom() + 12.0;
    let rest = Rect::from_min_max(
        Pos2::new(rect.left(), top),
        Pos2::new(rect.right(), rect.bottom()),
    );
    if rest.height() > 80.0 {
        p.rect_filled(rest, 12.0, panel());
        p.rect_stroke(
            rest,
            12.0,
            Stroke::new(1.0, group_ring()),
            StrokeKind::Inside,
        );
        p.text(
            Pos2::new(rest.left() + 14.0, rest.top() + 18.0),
            Align2::LEFT_CENTER,
            crate::i18n::t!("通知与测试"),
            FontId::proportional(12.0),
            text(),
        );
        let rows = [
            (crate::i18n::t!("完成与失败时提示"), true),
            (crate::i18n::t!("任务完成时打开输出目录"), false),
        ];
        let mut ry = rest.top() + 44.0;
        for (label, on) in rows {
            let knob = Rect::from_min_size(
                Pos2::new(rest.left() + 14.0, ry - 6.0),
                Vec2::new(22.0, 12.0),
            );
            p.rect_filled(knob, 6.0, if on { accent() } else { slider_rail() });
            p.circle_filled(
                Pos2::new(
                    if on {
                        knob.right() - 6.0
                    } else {
                        knob.left() + 6.0
                    },
                    knob.center().y,
                ),
                4.5,
                Color32::WHITE,
            );
            p.text(
                Pos2::new(knob.right() + 10.0, ry),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                muted(),
            );
            ry += 30.0;
        }
    }
}

/// 进行中任务的标题：优先队列段名，其次导出。
///
/// Title of the running job: queue segment first, then export.
fn running_text(state: &AppState) -> Option<String> {
    if let Some(i) = state.queue_current {
        if let Some(item) = state.queue.get(i) {
            let name = std::path::Path::new(&item.root)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| item.root.clone());
            return Some(format!("渲染队列 · {name}"));
        }
    }
    if state.export_busy {
        return Some(crate::i18n::t!("导出进行中").to_string());
    }
    if state.merge_busy {
        return Some(crate::i18n::t!("合成进行中").to_string());
    }
    if state.align_busy {
        return Some(crate::i18n::t!("对齐进行中").to_string());
    }
    None
}
