//! 后台任务（v10.6 第四个工作台）：任务卡 + 任务摘要 + 通知设置。
//!
//! 设计稿 `#vTasks`：`.ws-head` 64，页面**没有**二级工具条；`.ws-body` 是
//! `1fr 300px`、gap 12、`max-width:980` 居中的两列，末尾一张通栏的「最近完成」。
//! 左列任务卡栈：进行中卡（标题 / 状态 chip / 百分比 / 取消 / 阶段链 / 3px 进度条 /
//! 数字与 ETA）→ 已完成与失败内联行 → 队列顺序；右列是「任务摘要」四宫格 +
//! 「通知与重试」开关组 + 一句共用状态源的说明。
//!
//! 开关是 egui 临时数据（`ui.ctx().data_mut`），不动 `AppState`：它只是这一页的显示
//! 偏好，没有落盘语义。卡片一律按内容收高，空数据时也不会在窗口底留一张空卡。
//!
//! The tasks bench (fourth workspace). The design has **no** secondary tool strip: the body
//! is a centred, `max-width:980` two-column grid (1fr / 300px, gap 12), with a full-width
//! "recently finished" card underneath. The left column stacks the running card (title,
//! chip, percentage, cancel, stage chain, 3 px bar, counters and ETA), inline finished /
//! failed rows, and queue order; the right column holds the 2x2 summary, the notification
//! switches and a one-line note about the shared state source.
//!
//! The switches live in egui's temp data (`ui.ctx().data_mut`) and never touch `AppState`:
//! they are a display preference for this page, with no on-disk meaning. Every card is
//! content-high, so an empty queue never leaves a blank card down to the window bottom.

use egui::{Align2, Color32, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use super::bench;
use crate::state::AppState;
use crate::theme::*;

/// 右列摘要卡宽 300：`#vTasks .ws-body{grid-template-columns:minmax(0,1fr) 300px}`。
///
/// Right summary column, per the design grid.
const SIDE_W: f32 = 300.0;
/// 页体最大宽 980 并居中：`#vTasks .ws-body{max-width:980px;margin:0 auto}`。
///
/// Body max width, centred, per the design.
const BODY_MAX_W: f32 = 980.0;
/// 页体内距：左右 16、上 12、下 16。
///
/// Body insets.
const BODY_X: f32 = 16.0;
const BODY_TOP: f32 = 12.0;
/// 卡间距 12：`.taskcol{gap:12px}` / `.tside{gap:12px}`。
///
/// Card gap.
const GAP: f32 = 12.0;
/// 卡片圆角 12：`--rc`。
///
/// Card radius.
const CARD_R: f32 = 12.0;
/// 卡内距 12：`.tcard{padding:12px}`。
///
/// Card padding.
const PAD: f32 = 12.0;

/// 卡内 10px 小字：最小 token 是 11，10 号由它减一档得来。
///
/// 10 px caption: the smallest size token is 11; 10 is that token one step down.
const SIZE_MICRO: f32 = SIZE_SMALL - 1.0;
/// chip 里 9px：`.chip{font-size:9px}`，同样是 11 减两档。
///
/// 9 px chip text: `SIZE_SMALL` two steps down.
const SIZE_CHIP: f32 = SIZE_SMALL - 2.0;
/// 摘要数字 18px（设计要求 18，token 最大 15）：标题档加三档。
///
/// Summary numbers, 18 px as specified: the title token plus three.
const SIZE_METRIC: f32 = SIZE_TITLE + 3.0;

/// 进行中卡的固定高度：上排 24 + 描述 + 阶段链 + 3px 条 + 读数行 + 上下内距。
///
/// Fixed height of the running card.
const RUN_H: f32 = 117.0;
/// 内联任务行高 44：`.trow{min-height:44px}`。
///
/// Inline task row height.
const TROW_H: f32 = 44.0;
/// 队列顺序行高 32：`.qline{min-height:32px}`。
///
/// Queue-order row height.
const QLINE_H: f32 = 32.0;
/// 历史行高 32：`.hrow{min-height:32px}`。
///
/// History row height.
const HROW_H: f32 = 32.0;

/// 阶段标记：已完成 / 当前 / 未开始。
///
/// Stage mark: done, current, not started.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Done,
    Now,
    Todo,
}

/// 行语气：完成 / 失败 / 等待。
///
/// Row tone: done, failed, waiting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Ok,
    Err,
    Wait,
}

/// 画整页后台任务。
///
/// Draw the whole tasks bench.
pub(super) fn page(state: &mut AppState, ui: &mut egui::Ui, _ctx: &egui::Context) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.rect_filled(full, 0.0, bench_bg());

    let running = state.running_jobs();
    // 副行只留读数：那句「导入 / 测光 / 去闪 / 对齐 / 渲染共用一套任务卡」是功能说明，
    // 用户 2026-11-11 说「说明太多」——标题一行 + 一句读数就够。
    //
    // The subtitle keeps the numbers only: the old sentence explained what the task card
    // is for, and the user asked for less prose ("too much explaining").
    let sub = crate::i18n::tr_fmt("{} 个在跑 · 队列 {} 项", &[&running, &state.queue.len()]);
    let (head, back) = bench::page_head(ui, crate::i18n::t!("后台任务"), &sub, BODY_MAX_W, BODY_X);
    // 页头左端的返回块：任务页没有侧栏也没有画布，这里是唯一的可见出口。
    //
    // The back chip at the head's left edge: the tasks bench has neither rails nor
    // canvas, so this is the only visible way out.
    if back {
        state.set_bench(super::Bench::Develop);
        return;
    }
    let (r1, clear) = bench::head_action(
        ui,
        head.right(),
        head.center().y,
        crate::i18n::t!("清除已完成"),
        false,
    );
    if clear.clicked() {
        state.clear_queue_rows();
    }
    let (_r2, pause) = bench::head_action(
        ui,
        r1.left() - 8.0,
        head.center().y,
        crate::i18n::t!("全部暂停"),
        false,
    );
    if pause.clicked() {
        state.stop_queue();
    }

    // 页面正文：定宽 980 居中（窗口更窄时让出内距），两列 + 末尾通栏历史卡。
    //
    // Body: 980 max, centred (giving up the insets on a narrower window), two columns
    // plus the full-width history card.
    let inner_w = (BODY_MAX_W.min(full.width()) - 2.0 * BODY_X).max(320.0);
    let left_x = full.center().x - inner_w / 2.0;
    let body_top = head.bottom() + BODY_TOP;
    let available = (full.bottom() - body_top - 16.0).max(0.0);
    if available <= 0.0 {
        return;
    }
    let left = Rect::from_min_size(
        Pos2::new(left_x, body_top),
        Vec2::new((inner_w - SIDE_W - GAP).max(240.0), available),
    );
    let right = Rect::from_min_size(
        Pos2::new(left.right() + GAP, body_top),
        Vec2::new(SIDE_W, available),
    );

    // 行数据先算好：卡片高度由行数决定（设计稿的四行是「三实一空」，空数据时只留一条）。
    //
    // Rows are computed first: card heights follow the row count. The design's four rows
    // are "three real plus one empty"; with no data only the empty one remains.
    let rows = list_rows(state);
    let qrows = queue_rows(state);
    let hrows = history_rows(state);

    // 整页只留**一块面**：以前左列三段、右列两段、底部历史各画自己的圆角卡，中间还留
    // 12px 缝，读起来像一堆漂浮的方块（用户 2026-11-11「这种一块一块的好看吗」）。
    // 现在一张底板罩住全部内容，段与段之间只用发丝线分区。
    //
    // One surface for the whole page: the three left sections, the two right ones and the
    // history used to paint their own rounded cards with 12 px gaps between them, which
    // read as a pile of floating blocks (user, 2026-11-11). A single panel now backs
    // everything and sections are separated by hairlines only.
    let lh = left_height(&rows, &qrows);
    let rh = aside_height(&p, right.width());
    let hh = history_height(&hrows);
    let col_h = lh.max(rh);
    let total_h = col_h + GAP + hh;
    // 工作区贯穿到底边：内容不足时留白落在**面板内部**（像邮件客户端的列表区），
    // 而不是把面板缩在上半屏、下半屏露一大块背景。
    //
    // The work area runs to the bottom edge: spare space stays **inside** the panel
    // (like a mail client's list), instead of a short panel over a bare lower half.
    let panel_h = total_h.max(available.min(760.0));
    let surface = Rect::from_min_size(Pos2::new(left_x, body_top), Vec2::new(inner_w, panel_h));
    card_face(&p, surface);
    // 两栏之间的竖分隔线。
    //
    // The vertical rule between the two columns.
    let vx = left.right() + GAP * 0.5;
    p.line_segment(
        [
            Pos2::new(vx, body_top + PAD),
            Pos2::new(vx, body_top + panel_h - PAD),
        ],
        Stroke::new(1.0, group_ring()),
    );
    // 历史卡之上那道横分隔线。
    //
    // The horizontal rule above the history block.
    rule(
        &p,
        left_x + PAD,
        left_x + inner_w - PAD,
        body_top + col_h + GAP * 0.5,
        group_ring(),
    );

    let hist_top = body_top + col_h + GAP;
    task_cards(state, ui, left, &rows, &qrows);
    aside(state, ui, right);
    history(ui, left_x, inner_w, hist_top, &hrows);
}

/// 左列三段的总高（当前任务 + 队列行 + 队列顺序）。
///
/// Total height of the left stack: running card, queue rows, queue order.
fn left_height(rows: &[TaskRow], qrows: &[QueueRow]) -> f32 {
    RUN_H + rows.len() as f32 * TROW_H + PAD * 2.0 + 24.0 + 8.0 + qrows.len() as f32 * QLINE_H
}

/// 右列两段的总高（任务摘要 + 通知与重试 + 底部说明）。
///
/// 底部说明是**自动换行**的小字，行数随语言和列宽变（英文那句在 948px 下排三行），
/// 所以必须用同一个画笔、同一个宽度实测——写死常量会让下面的历史段正好压在这段
/// 说明上（多尺寸扫描实测重叠 40.6px）。
///
/// Total height of the right column: summary, notifications, and the trailing note.
///
/// That note wraps, and how many lines it takes depends on the language and the column
/// width (the English sentence runs to three lines at 948 px), so it must be measured
/// with the same painter and width; a constant let the history block land right on top
/// of it (the multi-size sweep measured 40.6 px of overlap).
fn aside_height(p: &Painter, width: f32) -> f32 {
    const SUM_H: f32 = PAD * 2.0 + 24.0 + 8.0 + 48.0 * 2.0 + 8.0 + 10.0 + 18.0;
    const NOTIFY_H: f32 = PAD * 2.0 + 24.0 + 8.0 + 3.0 * 32.0;
    let info =
        crate::i18n::t!("任务卡与导出队列共用同一状态源；渲染在后台继续，切换工作台不会中断。");
    let info_h = p
        .layout(
            info.to_owned(),
            font(SIZE_MICRO),
            dim(),
            (width - 2.0 * PAD).max(80.0),
        )
        .size()
        .y;
    SUM_H + GAP + NOTIFY_H + GAP + info_h + PAD
}

/// 历史段高。
///
/// Height of the history block.
fn history_height(rows: &[HistoryRow]) -> f32 {
    PAD * 2.0 + 24.0 + 8.0 + rows.len() as f32 * HROW_H
}

/// 左列任务卡栈，返回这一列实际用到的高度。
///
/// The left task stack; returns the height it used.
fn task_cards(
    state: &mut AppState,
    ui: &mut Ui,
    rect: Rect,
    rows: &[TaskRow],
    qrows: &[QueueRow],
) -> f32 {
    let mut y = rect.top();
    let list_h = rows.len() as f32 * TROW_H;
    let q_h = PAD * 2.0 + 24.0 + 8.0 + qrows.len() as f32 * QLINE_H;
    // 三段合成**一张卡**：当前任务 / 队列 / 队列顺序本来是一条队列的三个截面，
    // 以前各画各的卡面，读起来像三个互不相干的模块（用户 2026-11-11「有些地方
    // 不需要分模块，本来就在一页的」）。卡面罩住整段，段间只留一道发丝线。
    //
    // One card for all three sections: the running job, the queue and the queue order
    // are three slices of one queue, but each used to paint its own face and read as
    // three unrelated modules (user, 2026-11-11). One face spans them all; sections are
    // separated by a hairline only.
    run_card(
        state,
        ui,
        Rect::from_min_size(rect.left_top(), Vec2::new(rect.width(), RUN_H)),
    );
    y += RUN_H;
    if !rows.is_empty() {
        rule(
            &ui.painter().clone(),
            rect.left() + PAD,
            rect.right() - PAD,
            y,
            group_ring(),
        );
    }

    task_list(
        ui,
        Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), list_h)),
        rows,
    );
    y += list_h;
    if !qrows.is_empty() {
        rule(
            &ui.painter().clone(),
            rect.left() + PAD,
            rect.right() - PAD,
            y,
            group_ring(),
        );
    }

    queue_order(
        ui,
        Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), q_h)),
        qrows,
    );
    y += q_h;
    y - rect.top()
}

/// 进行中卡：标题 + 状态 chip + 百分比 + 取消，阶段链，3px 进度条，读数行。
///
/// The running card: title, chip, percentage and cancel; stage chain; 3 px bar; readouts.
fn run_card(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    let busy = state.queue_busy || state.export_busy || state.merge_busy || state.align_busy;
    let (done_n, total_n, encoding) = match state.queue_progress() {
        Some((d, t, stage)) => (d, t, matches!(stage, aevum_core::export::Stage::Encode)),
        None => (0, 0, false),
    };
    let pct = done_n
        .checked_mul(100)
        .and_then(|v| v.checked_div(total_n))
        .unwrap_or(0)
        .min(100);
    let x0 = rect.left() + PAD;
    let x1 = rect.right() - PAD;
    let cy = rect.top() + PAD + 12.0;

    // 跑着时左侧 2px 琥珀闸门：`.tcard.run::before`。
    //
    // The amber 2 px gate on the left while a job runs.
    if busy {
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(rect.left() + 1.0, rect.top() + PAD),
                Vec2::new(2.0, rect.height() - 2.0 * PAD),
            ),
            1.0,
            warn(),
        );
    }

    let title = if busy {
        running_text(state).unwrap_or_else(|| crate::i18n::t!("后台任务进行中").to_owned())
    } else {
        crate::i18n::t!("没有正在跑的任务").to_owned()
    };
    let title_max = (rect.width() - 250.0).max(80.0);
    let title_draw = fit_text(&p, &title, bold(SIZE_BODY), text(), title_max);
    let title_w = p
        .layout_no_wrap(title_draw.clone(), bold(SIZE_BODY), text())
        .size()
        .x;
    p.text(
        Pos2::new(x0, cy),
        Align2::LEFT_CENTER,
        title_draw,
        bold(SIZE_BODY),
        if busy { text() } else { muted() },
    );

    // 取消按钮（可用时是实心小按钮，否则压成一块禁用底）：`.trow .btn{height:24px}`。
    //
    // Cancel button: a real small button while running, a disabled block otherwise.
    let cancel = Rect::from_min_size(Pos2::new(x1 - 48.0, cy - 12.0), Vec2::new(48.0, 24.0));
    let cancel_resp = ui.interact(
        cancel,
        ui.make_persistent_id("task-ws-cancel"),
        if busy { Sense::click() } else { Sense::hover() },
    );
    p.rect_filled(
        cancel,
        6.0,
        if !busy {
            field()
        } else if cancel_resp.hovered() {
            elev_hover()
        } else {
            elev()
        },
    );
    p.text(
        cancel.center(),
        Align2::CENTER_CENTER,
        crate::i18n::t!("取消"),
        font(SIZE_SMALL),
        if busy {
            text()
        } else {
            dim().gamma_multiply(0.72)
        },
    );
    if cancel_resp.clicked() {
        state.stop_queue();
    }
    if cancel_resp.hovered() && busy {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    // 百分比：`.pct{font-size:12px;font-weight:500;color:var(--text)}`。
    //
    // Percentage.
    p.text(
        Pos2::new(cancel.left() - 10.0, cy),
        Align2::RIGHT_CENTER,
        format!("{pct}%"),
        font(SIZE_LABEL),
        text(),
    );

    // 状态 chip：进行中（琥珀）/ 空闲（灰）。
    //
    // Status chip.
    let (chip_label, chip_on) = if busy {
        (crate::i18n::t!("进行中"), true)
    } else {
        (crate::i18n::t!("空闲"), false)
    };
    let chip_w = p
        .layout_no_wrap(chip_label.to_owned(), bold(SIZE_CHIP), dim())
        .size()
        .x
        + 12.0;
    let chip = Rect::from_min_size(
        Pos2::new(x0 + title_w + 8.0, cy - 9.0),
        Vec2::new(chip_w, 18.0),
    );
    p.rect_filled(
        chip,
        4.0,
        if chip_on {
            warn().gamma_multiply(0.14)
        } else {
            field()
        },
    );
    p.text(
        chip.center(),
        Align2::CENTER_CENTER,
        chip_label,
        bold(SIZE_CHIP),
        if chip_on { warn() } else { dim() },
    );

    // 阶段链：导入 → 渲染 → 编码 → 完成（空闲时全是待办）。
    //
    // Stage chain: import -> render -> encode -> done (all pending while idle).
    let stages = stage_chain(state, busy, encoding);
    let stage_y = cy + 26.0;
    let mut fixed = 0.0;
    for (label, _) in &stages {
        fixed += 16.0
            + 6.0
            + p.layout_no_wrap((*label).to_owned(), font(SIZE_MICRO), dim())
                .size()
                .x;
    }
    let line_w = ((x1 - x0 - fixed - (stages.len() as f32 - 1.0) * 16.0)
        / (stages.len() as f32 - 1.0))
        .max(12.0);
    let mut sx = x0;
    for (i, (label, mark)) in stages.iter().enumerate() {
        let d = Rect::from_min_size(Pos2::new(sx, stage_y - 8.0), Vec2::splat(16.0));
        let (fill, ink, ring) = match mark {
            Mark::Done => (ok().gamma_multiply(0.16), ok(), None),
            Mark::Now => (warn(), Color32::from_rgb(0x1b, 0x13, 0x05), None),
            Mark::Todo => (field(), dim(), Some(group_ring())),
        };
        p.rect_filled(d, 4.0, fill);
        if let Some(c) = ring {
            p.rect_stroke(d, 4.0, Stroke::new(1.0, c), StrokeKind::Inside);
        }
        p.text(
            d.center(),
            Align2::CENTER_CENTER,
            if *mark == Mark::Done {
                "✓".to_owned()
            } else {
                format!("{}", i + 1)
            },
            bold(SIZE_CHIP),
            ink,
        );
        p.text(
            Pos2::new(d.right() + 6.0, d.center().y),
            Align2::LEFT_CENTER,
            *label,
            font(SIZE_MICRO),
            match mark {
                Mark::Done => muted(),
                Mark::Now => text(),
                Mark::Todo => dim(),
            },
        );
        let label_w = p
            .layout_no_wrap((*label).to_owned(), font(SIZE_MICRO), dim())
            .size()
            .x;
        sx = d.right() + 6.0 + label_w;
        if i + 1 < stages.len() {
            let ly = d.center().y;
            p.line_segment(
                [Pos2::new(sx + 8.0, ly), Pos2::new(sx + 8.0 + line_w, ly)],
                Stroke::new(1.0, group_ring()),
            );
            sx += line_w + 16.0;
        }
    }

    // 3px 进度条。
    //
    // The 3 px progress bar.
    let bar = Rect::from_min_size(Pos2::new(x0, stage_y + 12.0), Vec2::new(x1 - x0, 3.0));
    p.rect_filled(bar, 2.0, slider_rail());
    let frac = if total_n == 0 {
        0.0
    } else {
        (done_n as f32 / total_n as f32).clamp(0.0, 1.0)
    };
    if frac > 0.0 {
        p.rect_filled(
            Rect::from_min_size(bar.left_top(), Vec2::new(bar.width() * frac, 3.0)),
            1.5,
            warn(),
        );
    }

    // 读数行：当前阶段 / 帧数 / 预计剩余 / 右端常驻说明。
    //
    // Readouts: current stage, frames, ETA, and the standing note on the right.
    let meta_y = bar.bottom() + 17.0;
    let stage_now = stages
        .iter()
        .find(|(_, m)| *m == Mark::Now)
        .map(|(l, _)| *l)
        .unwrap_or("—");
    let mut mx = x0;
    mx = run(
        &p,
        mx,
        meta_y,
        &[(crate::i18n::t!("当前阶段"), dim(), false)],
    );
    mx += 4.0;
    mx = run(&p, mx, meta_y, &[(stage_now, muted(), true)]);
    mx += 16.0;
    mx = run(
        &p,
        mx,
        meta_y,
        &[(&format!("{done_n} / {total_n}"), muted(), true)],
    );
    mx += 4.0;
    mx = run(&p, mx, meta_y, &[(crate::i18n::t!("帧"), dim(), false)]);
    mx += 16.0;
    mx = run(
        &p,
        mx,
        meta_y,
        &[(crate::i18n::t!("预计剩余"), dim(), false)],
    );
    mx += 4.0;
    let _ = run(&p, mx, meta_y, &[("—", muted(), false)]);
    p.text(
        Pos2::new(x1, meta_y),
        Align2::RIGHT_CENTER,
        crate::i18n::t!("后台运行 · 关闭面板不中断"),
        font(SIZE_MICRO),
        dim(),
    );
}

/// 已完成 / 失败 / 排队内联行；最后一行是空态行，把卡面收住。
///
/// Inline finished / failed / queued rows; the last one is the empty-state row that closes
/// the card.
fn task_list(ui: &mut Ui, rect: Rect, rows: &[TaskRow]) {
    let p = ui.painter().clone();
    for (i, row) in rows.iter().enumerate() {
        let y = rect.top() + i as f32 * TROW_H;
        let r = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), TROW_H));
        if i + 1 < rows.len() {
            rule(&p, r.left(), r.right(), r.bottom(), group_ring());
        }
        let cy = r.center().y;
        // 状态：72px 一列，圆点 + 文字。
        //
        // Status: a 72 px column with a dot and a label.
        let st_x = r.left() + PAD;
        let color = match row.tone {
            Tone::Ok => ok(),
            Tone::Err => danger(),
            Tone::Wait => dim(),
        };
        p.circle_filled(Pos2::new(st_x + 3.0, cy), 3.0, color);
        p.text(
            Pos2::new(st_x + 12.0, cy),
            Align2::LEFT_CENTER,
            row.label,
            font(SIZE_SMALL),
            match row.tone {
                Tone::Ok => muted(),
                Tone::Err => danger(),
                Tone::Wait => dim(),
            },
        );
        // 名称 190px：`.trow .nm{width:190px}`。
        //
        // Name column, 190 px wide.
        let nm_x = st_x + 72.0 + GAP;
        let name_color = if row.empty { dim() } else { text() };
        p.text(
            Pos2::new(nm_x, cy),
            Align2::LEFT_CENTER,
            fit_text(&p, &row.name, font(SIZE_LABEL), name_color, 190.0),
            font(SIZE_LABEL),
            name_color,
        );
        // 右端：时间列 + 失败行的重试按钮。按钮宽按文案量，右端再让 10px 给时间列，
        // 两者永不叠字。
        //
        // Right end: the when column plus a retry button on a failed row. The button is
        // sized to its label and leaves 10 px for the when column, so they never overlap.
        let retry_label = crate::i18n::t!("重试失败帧");
        let retry_w = if row.retry {
            (p.layout_no_wrap(retry_label.to_owned(), font(SIZE_SMALL), text())
                .size()
                .x
                + 16.0)
                .max(56.0)
        } else {
            0.0
        };
        let when_x = r.right() - PAD - retry_w;
        if row.retry {
            let btn = Rect::from_min_size(
                Pos2::new(when_x + 10.0, cy - 12.0),
                Vec2::new(retry_w - 10.0, 24.0),
            );
            p.rect_filled(btn, 6.0, elev());
            p.text(
                btn.center(),
                Align2::CENTER_CENTER,
                retry_label,
                font(SIZE_SMALL),
                text(),
            );
        }
        p.text(
            Pos2::new(when_x, cy),
            Align2::RIGHT_CENTER,
            &row.when,
            font(SIZE_MICRO),
            dim(),
        );
        // 中段说明：占剩余宽度，超了截断。
        //
        // Middle detail: takes the leftover width and truncates on overflow.
        let meta_x = nm_x + 190.0 + GAP;
        let meta_max = when_x - 8.0 - meta_x;
        if meta_max > 24.0 {
            p.text(
                Pos2::new(meta_x, cy),
                Align2::LEFT_CENTER,
                fit_text(&p, &row.meta, font(SIZE_MICRO), dim(), meta_max),
                font(SIZE_MICRO),
                dim(),
            );
        }
    }
}

/// 队列顺序卡：待渲项按队列次序列出（最多 3 行）。
///
/// Queue order: the waiting entries in queue order, at most three rows.
fn queue_order(ui: &mut Ui, rect: Rect, rows: &[QueueRow]) {
    let p = ui.painter().clone();
    let x0 = rect.left() + PAD;
    let x1 = rect.right() - PAD;
    p.text(
        Pos2::new(x0, rect.top() + PAD + 12.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("队列顺序"),
        bold(SIZE_BODY),
        text(),
    );
    p.text(
        Pos2::new(x1, rect.top() + PAD + 12.0),
        Align2::RIGHT_CENTER,
        crate::i18n::t!("当前段完成后自动开始"),
        font(SIZE_MICRO),
        dim(),
    );
    let mut y = rect.top() + PAD + 24.0 + 8.0;
    for (i, row) in rows.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(x0, y),
            Vec2::new(rect.width() - 2.0 * PAD, QLINE_H),
        );
        if i + 1 < rows.len() {
            rule(&p, r.left(), r.right(), r.bottom(), group_ring());
        }
        // 序号块 16×16：`.qn`。
        //
        // Number badge.
        let qn = Rect::from_min_size(Pos2::new(r.left(), r.center().y - 8.0), Vec2::splat(16.0));
        p.rect_filled(qn, 4.0, field());
        p.text(
            qn.center(),
            Align2::CENTER_CENTER,
            if row.no == 0 {
                "—".to_owned()
            } else {
                format!("{}", row.no)
            },
            bold(SIZE_CHIP),
            dim(),
        );
        let nm_x = qn.right() + 10.0;
        p.text(
            Pos2::new(nm_x, r.center().y),
            Align2::LEFT_CENTER,
            fit_text(&p, &row.name, font(SIZE_LABEL), text(), 150.0),
            font(SIZE_LABEL),
            if row.no == 0 { dim() } else { text() },
        );
        // 右端「约 xx 位」。
        //
        // Right end: the approximate slot.
        let when_w = p
            .layout_no_wrap(row.when.clone(), font(SIZE_MICRO), dim())
            .size()
            .x;
        p.text(
            Pos2::new(r.right(), r.center().y),
            Align2::RIGHT_CENTER,
            &row.when,
            font(SIZE_MICRO),
            dim(),
        );
        let meta_x = nm_x + 160.0;
        let meta_max = r.right() - when_w - 12.0 - meta_x;
        if meta_max > 24.0 {
            p.text(
                Pos2::new(meta_x, r.center().y),
                Align2::LEFT_CENTER,
                fit_text(&p, &row.meta, font(SIZE_MICRO), dim(), meta_max),
                font(SIZE_MICRO),
                dim(),
            );
        }
        y += QLINE_H;
    }
}

/// 右列：任务摘要四宫格 + 通知与重试开关 + 共用状态源说明。返回用掉的高度。
///
/// The right column: 2x2 summary, notification switches, and the shared-state note.
/// Returns the height used.
fn aside(state: &AppState, ui: &mut Ui, rect: Rect) -> f32 {
    let p = ui.painter().clone();
    let sum_h = PAD * 2.0 + 24.0 + 8.0 + 48.0 * 2.0 + 8.0 + 10.0 + 18.0;
    let summary = Rect::from_min_size(rect.left_top(), Vec2::new(rect.width(), sum_h));
    let x0 = summary.left() + PAD;
    let x1 = summary.right() - PAD;
    p.text(
        Pos2::new(x0, summary.top() + PAD + 12.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("任务摘要"),
        bold(SIZE_BODY),
        text(),
    );
    p.text(
        Pos2::new(x1, summary.top() + PAD + 12.0),
        Align2::RIGHT_CENTER,
        crate::i18n::t!("最近 24 小时"),
        font(SIZE_MICRO),
        dim(),
    );

    let total = state.queue.len();
    let done = state
        .queue
        .iter()
        .filter(|q| matches!(q.status, aevum_core::queue::QueueStatus::Done))
        .count();
    let failed = state
        .queue
        .iter()
        .filter(|q| matches!(q.status, aevum_core::queue::QueueStatus::Failed))
        .count();
    let running = state.running_jobs();
    let cells: [(usize, &str, Color32); 4] = [
        (total, crate::i18n::t!("全部任务"), text()),
        (
            running,
            crate::i18n::t!("进行中"),
            if running > 0 { warn() } else { text() },
        ),
        (done, crate::i18n::t!("已完成"), text()),
        (
            failed,
            crate::i18n::t!("部分失败"),
            if failed > 0 { danger() } else { text() },
        ),
    ];
    let cell_w = (x1 - x0 - 8.0) / 2.0;
    for (i, (v, label, color)) in cells.iter().enumerate() {
        let cell = Rect::from_min_size(
            Pos2::new(
                x0 + (i % 2) as f32 * (cell_w + 8.0),
                summary.top() + PAD + 24.0 + 8.0 + (i / 2) as f32 * 56.0,
            ),
            Vec2::new(cell_w, 48.0),
        );
        p.rect_filled(cell, 8.0, group_fill());
        p.text(
            Pos2::new(cell.left() + 10.0, cell.top() + 7.0),
            Align2::LEFT_TOP,
            format!("{v}"),
            bold(SIZE_METRIC),
            *color,
        );
        p.text(
            Pos2::new(cell.left() + 10.0, cell.bottom() - 10.0),
            Align2::LEFT_CENTER,
            *label,
            font(SIZE_MICRO),
            dim(),
        );
    }
    // 队列吞吐：`qsum` 行（发丝线 + 左右各一条读数）。
    //
    // Queue line: a hairline with one readout on each end.
    let qy = summary.top() + PAD + 24.0 + 8.0 + 48.0 * 2.0 + 8.0 + 10.0;
    rule(&p, x0, x1, qy, group_ring());
    p.text(
        Pos2::new(x0, qy + 14.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("队列"),
        font(SIZE_MICRO),
        dim(),
    );
    p.text(
        Pos2::new(x1, qy + 14.0),
        Align2::RIGHT_CENTER,
        crate::i18n::tr_fmt("{} 项 · 完成 {}", &[&total, &done]),
        font(SIZE_MICRO),
        muted(),
    );

    // 通知与重试：三条规则，开关是真控件，状态存 egui 临时数据。
    //
    // Notifications: three rules whose switches are real controls; their state lives in
    // egui's temp data.
    let top = summary.bottom() + GAP;
    let notify_h = PAD * 2.0 + 24.0 + 8.0 + 3.0 * 32.0;
    let notify = Rect::from_min_size(
        Pos2::new(rect.left(), top),
        Vec2::new(rect.width(), notify_h),
    );
    p.text(
        Pos2::new(x0, notify.top() + PAD + 12.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("通知与重试"),
        bold(SIZE_BODY),
        text(),
    );
    let rules: [(&str, bool, Option<&str>); 3] = [
        (
            crate::i18n::t!("完成与失败时提示"),
            true,
            Some(crate::i18n::t!("默认静默")),
        ),
        (crate::i18n::t!("失败段自动重试 1 次"), true, None),
        (crate::i18n::t!("任务完成时打开输出目录"), false, None),
    ];
    let mut ry = notify.top() + PAD + 24.0 + 8.0 + 16.0;
    for (i, (label, default, note)) in rules.iter().enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(notify.left() + PAD, ry - 16.0),
            Vec2::new(notify.width() - 2.0 * PAD, 32.0),
        );
        let id = ui.make_persistent_id(("task-ws-rule", i));
        let mut on = ui
            .ctx()
            .data(|d| d.get_temp::<bool>(id))
            .unwrap_or(*default);
        let resp = ui.interact(row, id.with("hit"), Sense::click());
        if resp.clicked() {
            on = !on;
            ui.ctx().data_mut(|d| d.insert_temp(id, on));
            ui.ctx().request_repaint();
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        // 开关 26×15，圆角 7.5，白点 11：`.sw` 的放大版，按真实控件画。
        //
        // Switch: 26x15 track, 7.5 radius, 11 px knob.
        let track = Rect::from_min_size(
            Pos2::new(row.left(), row.center().y - 7.5),
            Vec2::new(26.0, 15.0),
        );
        p.rect_filled(track, 8.0, if on { accent() } else { slider_rail() });
        p.circle_filled(
            Pos2::new(
                if on {
                    track.right() - 8.5
                } else {
                    track.left() + 8.5
                },
                track.center().y,
            ),
            5.5,
            Color32::WHITE,
        );
        p.text(
            Pos2::new(track.right() + 8.0, row.center().y),
            Align2::LEFT_CENTER,
            *label,
            font(SIZE_SMALL),
            muted(),
        );
        if let Some(note) = note {
            p.text(
                Pos2::new(row.right(), row.center().y),
                Align2::RIGHT_CENTER,
                *note,
                font(SIZE_MICRO),
                dim(),
            );
        }
        if i + 1 < rules.len() {
            rule(&p, row.left(), row.right(), row.bottom(), group_ring());
        }
        ry += 32.0;
    }

    // 共用状态源说明：一段 10px 小字，不占卡。
    //
    // The shared-state note: one 10 px paragraph, no card of its own.
    let info_y = notify.bottom() + GAP;
    let info =
        crate::i18n::t!("任务卡与导出队列共用同一状态源；渲染在后台继续，切换工作台不会中断。");
    let galley = p.layout(
        info.to_owned(),
        font(SIZE_MICRO),
        dim(),
        rect.width() - 2.0 * PAD,
    );
    let info_h = galley.size().y;
    p.galley(Pos2::new(rect.left() + PAD, info_y), galley, dim());
    info_y + info_h + PAD - rect.top()
}

/// 通栏「最近完成」卡：已完成 / 失败的结果行，结果保留。
///
/// The full-width "recently finished" card; finished and failed results stay put.
fn history(ui: &mut Ui, left: f32, width: f32, top: f32, rows: &[HistoryRow]) {
    let p = ui.painter().clone();
    let h = PAD * 2.0 + 24.0 + 8.0 + rows.len() as f32 * HROW_H;
    let rect = Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, h));
    let x0 = rect.left() + PAD;
    let x1 = rect.right() - PAD;
    p.text(
        Pos2::new(x0, rect.top() + PAD + 12.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("最近完成"),
        bold(SIZE_BODY),
        text(),
    );
    p.text(
        Pos2::new(x1, rect.top() + PAD + 12.0),
        Align2::RIGHT_CENTER,
        crate::i18n::t!("结果保留"),
        font(SIZE_MICRO),
        dim(),
    );
    let mut y = rect.top() + PAD + 24.0 + 8.0;
    for (i, row) in rows.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(rect.left() + PAD, y),
            Vec2::new(rect.width() - 2.0 * PAD, HROW_H),
        );
        if i + 1 < rows.len() {
            rule(&p, r.left(), r.right(), r.bottom(), group_ring());
        }
        p.text(
            Pos2::new(r.left(), r.center().y),
            Align2::LEFT_CENTER,
            &row.when,
            font(SIZE_MICRO),
            dim(),
        );
        let name_x = r.left() + 88.0 + GAP;
        let name_max = (r.width() - 88.0 - GAP - 200.0 - GAP - 72.0 - GAP).max(80.0);
        p.text(
            Pos2::new(name_x, r.center().y),
            Align2::LEFT_CENTER,
            fit_text(
                &p,
                &row.name,
                font(SIZE_LABEL),
                if row.empty { dim() } else { text() },
                name_max,
            ),
            font(SIZE_LABEL),
            if row.empty { dim() } else { text() },
        );
        p.text(
            Pos2::new(r.right() - 72.0 - GAP, r.center().y),
            Align2::RIGHT_CENTER,
            fit_text(&p, &row.meta, font(SIZE_MICRO), dim(), 200.0),
            font(SIZE_MICRO),
            dim(),
        );
        if !row.empty {
            p.text(
                Pos2::new(r.right(), r.center().y),
                Align2::RIGHT_CENTER,
                crate::i18n::t!("打开输出"),
                font(SIZE_MICRO),
                accent(),
            );
        }
        y += HROW_H;
    }
}

/// 内联任务行。
///
/// One inline task row.
struct TaskRow {
    label: &'static str,
    tone: Tone,
    name: String,
    meta: String,
    when: String,
    retry: bool,
    empty: bool,
}

/// 队列顺序行。
///
/// One queue-order row.
struct QueueRow {
    no: usize,
    name: String,
    meta: String,
    when: String,
}

/// 历史行。
///
/// One history row.
struct HistoryRow {
    when: String,
    name: String,
    meta: String,
    empty: bool,
}

/// 内联任务行：队列里每一项一行，末尾补一条空态行（设计稿的「三实一空」）。
///
/// Inline rows: one per queue entry plus the empty-state row at the end (the design's
/// "three real rows plus one empty").
fn list_rows(state: &AppState) -> Vec<TaskRow> {
    let mut rows: Vec<TaskRow> = state
        .queue
        .iter()
        .take(3)
        .map(|item| {
            let (label, tone) = match item.status {
                aevum_core::queue::QueueStatus::Done => (crate::i18n::t!("已完成"), Tone::Ok),
                aevum_core::queue::QueueStatus::Failed => (crate::i18n::t!("部分失败"), Tone::Err),
                aevum_core::queue::QueueStatus::Pending => (crate::i18n::t!("排队"), Tone::Wait),
            };
            TaskRow {
                label,
                tone,
                name: crate::i18n::tr_fmt("导出 · {}", &[&item_name(&item.root)]),
                meta: item_meta(item),
                when: match item.status {
                    aevum_core::queue::QueueStatus::Pending => {
                        crate::i18n::t!("等待前序完成").to_owned()
                    }
                    _ => "—".to_owned(),
                },
                retry: matches!(item.status, aevum_core::queue::QueueStatus::Failed),
                empty: false,
            }
        })
        .collect();
    rows.push(TaskRow {
        label: crate::i18n::t!("空闲"),
        tone: Tone::Wait,
        name: crate::i18n::t!("暂无更多后台任务").to_owned(),
        // 空闲行只留一句「暂无更多后台任务」：那句「导入、去闪、对齐、渲染共用这一套
        // 任务卡；完成后结果保留」原是页头副标题的重复（用户 2026-11-11「说明太多」）。
        //
        // The idle row keeps one line; the old explanation merely repeated the page
        // subtitle the user asked to trim.
        meta: String::new(),
        when: String::new(),
        retry: false,
        empty: true,
    });
    rows
}

/// 队列顺序：当前段之后的待渲项，最多三行；没有就一条说明。
///
/// Queue order: the pending entries after the current one, at most three rows; a single
/// note when there are none.
fn queue_rows(state: &AppState) -> Vec<QueueRow> {
    let mut rows: Vec<QueueRow> = state
        .queue
        .iter()
        .enumerate()
        .filter(|(i, item)| {
            matches!(item.status, aevum_core::queue::QueueStatus::Pending)
                && Some(*i) != state.queue_current
        })
        .take(3)
        .map(|(i, item)| QueueRow {
            no: i + 1,
            name: item_name(&item.root),
            meta: item_meta(item),
            when: crate::i18n::t!("等待前序完成").to_owned(),
        })
        .collect();
    if rows.is_empty() {
        rows.push(QueueRow {
            no: 0,
            name: crate::i18n::t!("队列里没有待渲项").to_owned(),
            meta: String::new(),
            when: String::new(),
        });
    }
    rows
}

/// 历史：已完成 / 失败项，最多四行；空时一条说明。
///
/// History: finished and failed entries, at most four rows; one note when empty.
fn history_rows(state: &AppState) -> Vec<HistoryRow> {
    let mut rows: Vec<HistoryRow> = state
        .queue
        .iter()
        .enumerate()
        .filter(|(_, item)| !matches!(item.status, aevum_core::queue::QueueStatus::Pending))
        .take(4)
        .map(|(i, item)| HistoryRow {
            when: crate::i18n::tr_fmt("队列 #{}", &[&(i + 1)]),
            name: crate::i18n::tr_fmt("导出 · {}", &[&item_name(&item.root)]),
            meta: item_meta(item),
            empty: false,
        })
        .collect();
    if rows.is_empty() {
        rows.push(HistoryRow {
            when: crate::i18n::t!("暂无").to_owned(),
            name: crate::i18n::t!("还没有完成的任务 · 结果会在这里保留").to_owned(),
            meta: String::new(),
            empty: true,
        });
    }
    rows
}

/// 队列项的规格串：编码 · 画幅（0 表示跟源尺寸）· 帧率。
///
/// A queue entry's spec line: codec, size (0 means "follow the source") and frame rate.
fn item_meta(item: &aevum_core::queue::QueueItem) -> String {
    let codec = if item.codec.to_ascii_lowercase().contains("264") {
        "H.264"
    } else {
        "H.265"
    };
    let size = if item.width == 0 || item.height == 0 {
        crate::i18n::t!("跟随源尺寸").to_owned()
    } else {
        format!("{} × {}", item.width, item.height)
    };
    format!("{} · {} · {:.0} fps", codec, size, item.fps)
}

/// 阶段链：跑队列时是导入 → 渲染 → 编码 → 完成；测光时是导入 → 测光 → 缩略图 → 完成。
///
/// The stage chain: import -> render -> encode -> done while the queue runs; import ->
/// meter -> thumbnails -> done while metering.
fn stage_chain(state: &AppState, busy: bool, encoding: bool) -> Vec<(&'static str, Mark)> {
    if state.queue_busy {
        return vec![
            (crate::i18n::t!("导入"), Mark::Done),
            (
                crate::i18n::t!("渲染"),
                if encoding {
                    Mark::Done
                } else if busy {
                    Mark::Now
                } else {
                    Mark::Todo
                },
            ),
            (
                crate::i18n::t!("编码"),
                if encoding { Mark::Now } else { Mark::Todo },
            ),
            (crate::i18n::t!("完成"), Mark::Todo),
        ];
    }
    if state.measuring.is_some() {
        return vec![
            (crate::i18n::t!("导入"), Mark::Done),
            (crate::i18n::t!("测光"), Mark::Now),
            (crate::i18n::t!("缩略图"), Mark::Todo),
            (crate::i18n::t!("完成"), Mark::Todo),
        ];
    }
    vec![
        (crate::i18n::t!("导入"), Mark::Todo),
        (crate::i18n::t!("测光"), Mark::Todo),
        (crate::i18n::t!("缩略图"), Mark::Todo),
        (crate::i18n::t!("完成"), Mark::Todo),
    ]
}

/// 画卡片底与 1px 环。
///
/// Paint a card face with its 1 px ring.
fn card_face(p: &Painter, rect: Rect) {
    p.rect_filled(rect, CARD_R, panel());
    p.rect_stroke(
        rect,
        CARD_R,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );
}

/// 在一行里依次画若干段文字，返回右端 x。每段是 `(文本, 颜色, 是否粗体)`，字号 10。
///
/// Paint several text runs left to right on one line; returns the right edge. Each run is
/// `(text, colour, bold)`, all at 10 px.
fn run(p: &Painter, x: f32, cy: f32, items: &[(&str, Color32, bool)]) -> f32 {
    let mut x = x;
    for (text, color, heavy) in items {
        let f = if *heavy {
            bold(SIZE_MICRO)
        } else {
            font(SIZE_MICRO)
        };
        let w = p
            .layout_no_wrap((*text).to_owned(), f.clone(), *color)
            .size()
            .x;
        p.text(Pos2::new(x, cy), Align2::LEFT_CENTER, *text, f, *color);
        x += w;
    }
    x
}

/// 队列项名：root 的最后一段。
///
/// Queue entry name: the last segment of `root`.
fn item_name(root: &str) -> String {
    std::path::Path::new(root)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_owned())
}

/// 进行中任务的标题：优先队列段名，其次导出 / 合成 / 对齐。
///
/// Title of the running job: queue segment first, then export / merge / align.
fn running_text(state: &AppState) -> Option<String> {
    if let Some(i) = state.queue_current
        && let Some(item) = state.queue.get(i)
    {
        return Some(crate::i18n::tr_fmt(
            "渲染队列 · {}",
            &[&item_name(&item.root)],
        ));
    }
    if state.export_busy {
        return Some(crate::i18n::t!("导出进行中").to_owned());
    }
    if state.merge_busy {
        return Some(crate::i18n::t!("合成进行中").to_owned());
    }
    if state.align_busy {
        return Some(crate::i18n::t!("对齐进行中").to_owned());
    }
    None
}
