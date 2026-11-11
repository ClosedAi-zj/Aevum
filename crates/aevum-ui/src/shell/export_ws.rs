//! 导出工作台（v10.6 第三个工作台）：左侧逐帧流水队列，右侧导出设置。
//!
//! 设计稿 `#vExport`：`.ws-head` 64 + `.ws-tools` 36 + `.ws-body`（`1fr 272px`，gap 12，
//! padding 12/16/16）。左列是队列卡：卡头一行「队列」+ 计数，每行 34px，40×28 缩略图、
//! 名称、状态点、右侧状态文字，进行中行底部 3px 进度条，行间 1px 发丝线；右列是设置
//! 卡：行高 24（标签左、值右），底部主按钮 26px 满宽。
//!
//! 队列只有一两项时卡片按内容收高，不把卡面拉到窗口底：一整片空卡面读起来比空列表更糟。
//!
//! The export bench (third workspace): per-frame queue on the left, export settings on the
//! right, per the design's `#vExport`. Queue rows are 34 px with a 40x28 thumbnail, name,
//! status dot and right-aligned status text; the running row carries a 3 px progress bar
//! along its bottom edge, and rows are separated by 1 px hairlines. The settings card uses
//! 24 px label/value rows and a full-width 26 px primary button.
//!
//! The queue card is content-high rather than stretched to the window: a huge empty card
//! reads worse than a short list.

use egui::{
    Align2, Color32, CornerRadius, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use super::bench;
use crate::state::AppState;
use crate::theme::*;

/// 右列设置卡宽 272（设计稿 `grid-template-columns:minmax(0,1fr) 272px`）。
///
/// Right settings column, per the design grid.
const SIDE_W: f32 = 272.0;

/// 页体左右内距 16、上 12、下 16：`.ws-body{padding:12px 16px 16px}`。
///
/// Body insets, per `.ws-body`.
const BODY_X: f32 = 16.0;
const BODY_TOP: f32 = 12.0;
const BODY_BOTTOM: f32 = 16.0;
/// 两列间距 12：`.ws-body{gap:12px}`。
///
/// Column gap.
const GAP: f32 = 12.0;

/// 队列卡头高 30（设计稿表头 28，加一点呼吸给「队列」+ 计数）。
///
/// Queue-card header height.
const QHEAD_H: f32 = 30.0;
/// 队列行高 34：`.qtable tr.frow td{height:34px}`。
///
/// Queue row height.
const ROW_H: f32 = 34.0;
/// 缩略图 40×28：小到不抢行，又能认出是哪一段。
///
/// Thumbnail size.
const THUMB_W: f32 = 40.0;
const THUMB_H: f32 = 28.0;
/// 空队列卡高：一句话居中，不铺满整页。
///
/// Empty-queue card height; one centred line, never the whole page.
const EMPTY_H: f32 = 92.0;
/// 队列卡脚注高：说明这一队有多少项还等着编码。
///
/// Queue-card footer height.
const FOOT_H: f32 = 26.0;

/// 设置卡内距 14：值靠右时至少离开卡边 14，长路径不许被裁在卡边上。
///
/// Settings-card inset. 14 keeps the right-aligned value off the card edge so a long path
/// can never be clipped by the card border.
const SIDE_PAD: f32 = 14.0;
/// 设置行高 24。
///
/// Settings row height.
const SROW_H: f32 = 24.0;
/// 卡面小字 10：字号 token 最小一档是 11，10 号由它减一档得来。
///
/// 10 px card caption: the smallest token is 11, so 10 is that token one step down.
const SIZE_MICRO: f32 = SIZE_SMALL - 1.0;

/// 画整页导出工作台。
///
/// Draw the whole export bench.
pub(super) fn page(state: &mut AppState, ui: &mut egui::Ui, ctx: &egui::Context) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.rect_filled(full, 0.0, bench_bg());

    let n = state.queue.len();
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
    let busy = state.queue_current.map(|i| i + 1).unwrap_or(0);
    let sub = crate::i18n::tr_fmt(
        "{} 项 · 完成 {} · 渲染 {} · 失败 {}",
        &[&n, &done, &busy, &failed],
    );
    let (head, back) = bench::page_head(ui, crate::i18n::t!("导出工作台"), &sub, f32::MAX, BODY_X);
    // 页头左端的返回块：整页工作台没有侧栏也没有画布，这里是唯一的可见出口。
    //
    // The back chip at the head's left edge: a full-page bench has neither rails nor
    // canvas, so this is the only visible way out.
    if back {
        state.set_bench(super::Bench::Develop);
        return;
    }
    // 页头只放次级动作：「开始批量渲染」是这一步唯一的主动作，钉在右列底部（设置读完
    // 就按），页头再放一颗同色主按钮会变成两个「终点」，用户不知道按哪个。
    //
    // The head carries secondary actions only: "start rendering" is this step's single
    // primary and lives at the bottom of the settings column (read the settings, then
    // commit). A second accent button up here gives the page two finish lines.
    let (r1, join) = bench::head_action(
        ui,
        head.right(),
        head.center().y,
        crate::i18n::t!("加入当前序列"),
        false,
    );
    if join.clicked()
        && let Err(e) = state.add_current_to_queue()
    {
        state.error = Some(e);
    }
    let (_r2, clear) = bench::head_action(
        ui,
        r1.left() - 8.0,
        head.center().y,
        crate::i18n::t!("清空已完成"),
        false,
    );
    if clear.clicked() {
        state.clear_queue_rows();
    }

    let strip = bench::tools_strip(ui, head.bottom());
    let mut x = strip.left() + BODY_X;
    // 只保留**有实现**的按钮。「打开输出目录」「重试失败帧」此前画得出来、点了没反应
    // （`let (nx, _) =` 把 Response 丢了），属于假控件，删掉比留着诚实。
    //
    // Only buttons with a real handler stay. "Open output folder" and "Retry failed
    // frames" were painted but had no handler (the Response was dropped), so they are
    // removed rather than left as decoys.
    let (nx, _) = bench::tool_button(ui, x, strip, crate::i18n::t!("导出队列"), true);
    x = nx;
    let (nx, cur) = bench::tool_button(ui, x, strip, crate::i18n::t!("导出当前帧"), false);
    if cur.clicked() {
        state.request_export_confirm(ctx, true);
    }
    // 竖分隔：`--hair` 1px、高 14、左右各留 8。
    //
    // Vertical separator: 1 px hairline, 14 high, 8 on each side.
    let sep = (nx + 4.0).round() + 0.5;
    p.line_segment(
        [
            Pos2::new(sep, strip.center().y - 7.0),
            Pos2::new(sep, strip.center().y + 7.0),
        ],
        Stroke::new(1.0, group_ring()),
    );
    let (last, pause) =
        bench::tool_button(ui, sep + 8.0, strip, crate::i18n::t!("暂停当前"), false);
    if pause.clicked() {
        state.stop_queue();
    }

    // 工具条右端的流水说明：`每帧 渲染 → 去闪 → 参数过渡 → 编码 · 24 fps · H.265 CRF 18`。
    // 按钮排到它跟前就整条省略，不叠字。
    //
    // Pipeline caption on the right of the tool strip; it is dropped entirely rather than
    // overlapping the buttons when the window is too narrow.
    let fps = format!("{:.0}", state.output_fps);
    let codec = if state.codec.to_ascii_lowercase().contains("264") {
        "H.264"
    } else {
        "H.265"
    };
    let crf = format!("{} CRF {}", codec, state.render_crf);
    let cap = crate::i18n::tr_fmt(
        "每帧 渲染 → 去闪 → 参数过渡 → 编码 · {} fps · {}",
        &[&fps, &crf],
    );
    let cap_w = p
        .layout_no_wrap(cap.clone(), font(SIZE_MICRO), dim())
        .size()
        .x;
    if strip.right() - BODY_X - cap_w > last + 16.0 {
        p.text(
            Pos2::new(strip.right() - BODY_X, strip.center().y),
            Align2::RIGHT_CENTER,
            cap,
            font(SIZE_MICRO),
            dim(),
        );
    }

    // 内容驱动高度：两栏按「行数」长，不再拉满整窗。以前队列里只有一段任务，
    // 却挂在一张近 800px 高的空框里——整页最像「半成品」的地方。
    //
    // Content-driven height: the two columns grow with the row count instead of
    // stretching to the window. A single queue entry inside an ~800 px empty frame
    // was the most half-finished-looking part of the whole app.
    let body_top = strip.bottom() + BODY_TOP;
    let avail_h = (full.bottom() - BODY_BOTTOM - body_top).max(0.0);
    // 一整块工作区贯穿到窗口底边：内容驱动高度试过，画面变成「上半屏一块板、
    // 下半屏一片黑」，比铺满更头重脚轻。留 16px 底边距，上限 760 防止超高屏拉空。
    //
    // The single work area runs to the window's bottom edge: a content-driven height
    // left the lower half of the screen empty and read as worse-balanced. Cap at 760
    // so very tall windows do not stretch it into a void.
    let wanted_h = avail_h.min(760.0);
    let body = Rect::from_min_max(
        Pos2::new(full.left() + BODY_X, body_top),
        Pos2::new(full.right() - BODY_X, body_top + wanted_h),
    );
    if body.width() <= SIDE_W + GAP || body.height() <= 0.0 {
        return;
    }
    let side_left = body.right() - SIDE_W;
    let left = Rect::from_min_max(body.left_top(), Pos2::new(side_left - GAP, body.bottom()));
    let right = Rect::from_min_max(Pos2::new(side_left, body.top()), body.right_bottom());
    // 两栏合成**一块工作区**：以前各画各的圆角卡面，中间留 12px 缝，读起来是
    // 两块并排的积木。现在一张直角底板 + 一道竖分隔线，是 Photoshop / Capture One
    // 的面板分栏语法。
    //
    // One work area instead of two rounded cards with a 12 px gutter: a square backing
    // panel plus a vertical hairline is how pro apps divide docked columns.
    p.rect_filled(body, 0.0, panel());
    p.rect_stroke(
        body,
        0.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );
    let divider_x = (side_left - GAP * 0.5).round() + 0.5;
    p.line_segment(
        [
            Pos2::new(divider_x, body.top() + 8.0),
            Pos2::new(divider_x, body.bottom() - 8.0),
        ],
        Stroke::new(1.0, group_ring()),
    );
    queue_card(state, ui, left);
    settings_card(state, ui, ctx, right);
}

/// 队列卡：卡头「队列」+ 计数，逐行任务（缩略图 + 名称 + 状态点 + 状态文字）。
///
/// The queue card: a "queue" header with counts, then one row per job (thumbnail, name,
/// status dot, status text).
fn queue_card(state: &AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    let n = state.queue.len();
    // 空队列时卡片只留一条说明的高度：一张铺到窗口底的空卡面读起来像坏了。有内容时
    // 卡片铺满列高（设计稿的表格卡就是一整列，脚注钉在卡底），行放不下就截断并在脚注
    // 里交代还剩几项。
    //
    // Empty: the card hugs one line of copy -- a blank card stretched to the window bottom
    // reads as broken. With content it fills the column (the design's table card is a full
    // column with its footer pinned to the bottom); overflowing rows are cut and reported in
    // the footer.
    let content_h = if n == 0 { EMPTY_H } else { rect.height() };
    let card = Rect::from_min_size(
        rect.left_top(),
        Vec2::new(rect.width(), content_h.min(rect.height())),
    );

    // 卡头：比卡面亮一档的窄带，只圆上面两只角。
    //
    // Header band: one step above the card, only the top corners rounded.
    let band = Rect::from_min_size(card.min, Vec2::new(card.width(), QHEAD_H));
    p.rect_filled(
        band,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: 0,
            se: 0,
        },
        group_fill(),
    );
    p.text(
        Pos2::new(card.left() + SIDE_PAD, band.center().y),
        Align2::LEFT_CENTER,
        crate::i18n::t!("队列"),
        bold(SIZE_LABEL),
        text(),
    );
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
    let count = crate::i18n::tr_fmt("{} 项 · 完成 {} · 失败 {}", &[&n, &done, &failed]);
    p.text(
        Pos2::new(card.right() - SIDE_PAD, band.center().y),
        Align2::RIGHT_CENTER,
        count,
        font(SIZE_MICRO),
        dim(),
    );
    rule(&p, card.left(), card.right(), band.bottom(), group_ring());

    if n == 0 {
        // 说明居中在**表头以下**的卡身里，不是整张卡的中心：表头占掉 30px，按整卡居中
        // 会读成偏上。
        //
        // The sentence centres in the card body below the header, not in the whole card:
        // the 30 px header would otherwise push it visually high.
        let body = Rect::from_min_max(Pos2::new(card.left(), band.bottom()), card.right_bottom());
        p.text(
            body.center(),
            Align2::CENTER_CENTER,
            fit_text(
                &p,
                crate::i18n::t!("队列是空的 · 在显影工作台点「导出…」加入"),
                font(SIZE_SMALL),
                dim(),
                body.width() - 2.0 * SIDE_PAD,
            ),
            font(SIZE_SMALL),
            dim(),
        );
        return;
    }

    // 行区域：脚注之上，放不下的行直接不画（列表卡不滚，也不许溢出卡边）。
    //
    // Row area above the footer; rows that do not fit are skipped rather than painted
    // past the card edge.
    let rows_bottom = card.bottom() - FOOT_H;
    let mut drawn = 0usize;
    for (i, item) in state.queue.iter().enumerate() {
        let y = band.bottom() + i as f32 * ROW_H;
        if y + ROW_H > rows_bottom + 0.5 {
            break;
        }
        drawn += 1;
        let row = Rect::from_min_size(Pos2::new(card.left(), y), Vec2::new(card.width(), ROW_H));
        let running = state.queue_current == Some(i);
        if running {
            p.rect_filled(row, 0.0, warn().gamma_multiply(0.07));
            // 当前行左侧 2px 琥珀条：`.qtable tr.frow.run td:first-child{inset 2px 0 0 amber}`。
            p.rect_filled(
                Rect::from_min_size(
                    Pos2::new(row.left() + 1.0, row.top() + 7.0),
                    Vec2::new(2.0, ROW_H - 14.0),
                ),
                1.0,
                warn(),
            );
        }

        // 缩略图：队列项没有自己的帧图，用当前选中帧的缩略图占位；没解出来就一块灰底。
        //
        // Thumbnail: a queue entry carries no frame image of its own, so the selected
        // frame's thumbnail stands in; a flat block when it is not decoded yet.
        let thumb = Rect::from_min_size(
            Pos2::new(row.left() + SIDE_PAD, row.center().y - THUMB_H / 2.0),
            Vec2::new(THUMB_W, THUMB_H),
        );
        p.rect_filled(thumb, 3.0, thumb_bg());
        if let Some(t) = state.thumb(state.selected) {
            let (tw, th) = (t.size[0].max(1) as f32, t.size[1].max(1) as f32);
            let scale = (THUMB_W / tw).min(THUMB_H / th);
            p.image(
                t.texture.id(),
                Rect::from_center_size(thumb.center(), Vec2::new(tw * scale, th * scale)),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        p.rect_stroke(
            thumb,
            3.0,
            Stroke::new(1.0, group_ring()),
            StrokeKind::Inside,
        );

        let status = crate::i18n::tr(item.status.label());
        let status_w = p
            .layout_no_wrap(status.to_owned(), font(SIZE_MICRO), dim())
            .size()
            .x;
        let status_right = row.right() - SIDE_PAD;
        let status_x = status_right - status_w;
        let dot = Pos2::new(status_x - 10.0, row.center().y);
        let (dot_color, status_color) = match item.status {
            aevum_core::queue::QueueStatus::Done => (ok(), dim()),
            aevum_core::queue::QueueStatus::Failed => (danger(), danger()),
            aevum_core::queue::QueueStatus::Pending => {
                if running {
                    (warn(), warn())
                } else {
                    (dim(), dim())
                }
            }
        };
        p.circle_filled(dot, 3.0, dot_color);

        let name_x = thumb.right() + 10.0;
        let name = item_name(&item.root);
        let name = fit_text(
            &p,
            &name,
            font(SIZE_LABEL),
            text(),
            (dot.x - 12.0 - name_x).max(24.0),
        );
        p.text(
            Pos2::new(name_x, row.center().y),
            Align2::LEFT_CENTER,
            name,
            font(SIZE_LABEL),
            if running { text() } else { muted() },
        );
        p.text(
            Pos2::new(status_right, row.center().y),
            Align2::RIGHT_CENTER,
            status,
            font(SIZE_MICRO),
            status_color,
        );

        // 进行中行底部 3px 进度条。
        //
        // The running row's 3 px progress bar along its bottom edge.
        if running {
            let bar = Rect::from_min_size(
                Pos2::new(name_x, row.bottom() - 4.0),
                Vec2::new((row.right() - SIDE_PAD - name_x).max(8.0), 3.0),
            );
            p.rect_filled(bar, 2.0, slider_rail());
            let frac = row_fraction(state, i);
            if frac > 0.0 {
                p.rect_filled(
                    Rect::from_min_size(bar.left_top(), Vec2::new(bar.width() * frac, 3.0)),
                    1.5,
                    warn(),
                );
            }
        }
        if i + 1 < state.queue.len() {
            rule(
                &p,
                row.left() + SIDE_PAD,
                row.right() - SIDE_PAD,
                row.bottom(),
                group_ring(),
            );
        }
    }

    // 脚注：这一队还剩多少项。放不下的行数也在这里交代，卡面不外溢。
    //
    // Footer: how many entries are still waiting; the count of rows that did not fit is
    // reported here too, so nothing spills out of the card.
    let foot_y = rows_bottom;
    rule(&p, card.left(), card.right(), foot_y, group_ring());
    let pending = state
        .queue
        .iter()
        .filter(|q| matches!(q.status, aevum_core::queue::QueueStatus::Pending))
        .count();
    let hidden = n.saturating_sub(drawn);
    let left_note = if hidden > 0 {
        crate::i18n::tr_fmt("另有 {} 项未显示", &[&hidden])
    } else {
        crate::i18n::t!("完成后按序自动编码").to_owned()
    };
    p.text(
        Pos2::new(card.left() + SIDE_PAD, (foot_y + card.bottom()) / 2.0),
        Align2::LEFT_CENTER,
        left_note,
        font(SIZE_MICRO),
        dim(),
    );
    p.text(
        Pos2::new(card.right() - SIDE_PAD, (foot_y + card.bottom()) / 2.0),
        Align2::RIGHT_CENTER,
        crate::i18n::tr_fmt("待渲 {} 项", &[&pending]),
        font(SIZE_MICRO),
        dim(),
    );
}

/// 右侧设置卡：行高 24 的键值行 + 底部 26px 满宽主按钮。
///
/// The settings card: 24 px label/value rows plus a full-width 26 px primary button
/// pinned to the card's bottom.
fn settings_card(state: &mut AppState, ui: &mut Ui, ctx: &egui::Context, rect: Rect) {
    let p = ui.painter().clone();

    let x0 = rect.left() + SIDE_PAD;
    let x1 = rect.right() - SIDE_PAD;
    // 值的锚点比标题再进 2px：长路径顶着卡边读起来像被裁掉，多这 2px 才收得住。
    //
    // Values anchor 2 px further in than the heading: a long path flush with the card edge
    // reads as clipped, and those 2 px keep it clear.
    let vx1 = rect.right() - 16.0;
    let mut y = rect.top() + SIDE_PAD;

    // 卡头：标题 + 右端「保存到预设」。
    //
    // Header: the title plus a "save as preset" link on the right.
    p.text(
        Pos2::new(x0, y + 10.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("导出设置"),
        bold(SIZE_LABEL),
        text(),
    );
    p.text(
        Pos2::new(x1, y + 10.0),
        Align2::RIGHT_CENTER,
        crate::i18n::t!("保存到预设"),
        font(SIZE_MICRO),
        accent(),
    );
    y += SROW_H + 4.0;

    let out_path = state
        .queue
        .iter()
        .rev()
        .find(|q| !q.out.is_empty())
        .map(|q| q.out.clone())
        .or_else(|| state.last_saved.clone())
        .unwrap_or_default();
    let (dir, file) = split_out(&out_path);
    let codec = if state.codec.to_ascii_lowercase().contains("264")
        || state.codec.to_ascii_lowercase().contains("avc")
    {
        "H.264 / AVC (MP4)"
    } else {
        "H.265 / HEVC (MP4)"
    };
    let rows: [(&str, String, bool); 6] = [
        (crate::i18n::t!("编码"), codec.to_owned(), false),
        (
            crate::i18n::t!("帧率"),
            format!("{:.0}", state.output_fps),
            false,
        ),
        (
            crate::i18n::t!("分辨率"),
            format!("{} × {}", state.output_w, state.output_h),
            false,
        ),
        (
            crate::i18n::t!("画质 CRF"),
            format!("{}", state.render_crf),
            false,
        ),
        (crate::i18n::t!("输出文件"), file, false),
        (crate::i18n::t!("输出目录"), dir, true),
    ];
    for (label, value, is_path) in rows {
        p.text(
            Pos2::new(x0, y + SROW_H / 2.0),
            Align2::LEFT_CENTER,
            label,
            font(SIZE_MICRO),
            dim(),
        );
        let label_w = p
            .layout_no_wrap(label.to_owned(), font(SIZE_MICRO), dim())
            .size()
            .x;
        // 值右对齐，宽度 = 卡宽 − 两侧内距 − 标签 − 12 的呼吸；超了从前面截，
        // 路径的尾段（文件名）才是要紧的那一截。
        //
        // Values right-align; their width is the card minus both insets, the label and a
        // 12 px breather. Overflowing paths lose their head, because the tail (the file
        // name) is the part that matters.
        let max_w = (vx1 - x0 - label_w - 12.0).max(40.0);
        let text_value = if is_path {
            fit_tail(&p, &value, font(SIZE_LABEL), text(), max_w)
        } else {
            fit_text(&p, &value, font(SIZE_LABEL), text(), max_w)
        };
        p.text(
            Pos2::new(vx1, y + SROW_H / 2.0),
            Align2::RIGHT_CENTER,
            text_value,
            font(SIZE_LABEL),
            text(),
        );
        y += SROW_H;
    }

    // 底部块：一条发丝线 + 队列小结 + 26px 满宽主按钮。
    //
    // Bottom block: a hairline, a queue summary, then the 26 px full-width primary.
    let btn = Rect::from_min_size(
        Pos2::new(rect.left() + 12.0, rect.bottom() - 12.0 - 26.0),
        Vec2::new(rect.width() - 24.0, 26.0),
    );
    let sum_y = btn.top() - 20.0;
    if sum_y > y {
        // 发丝线在读数上方（设计稿 `qsum{box-shadow:0 -1px 0}`）：它分开的是设置行与小节脚注。
        //
        // The hairline sits above the readout, per the design's `qsum` shadow: it separates
        // the setting rows from the section footnote.
        rule(&p, x0, x1, sum_y - 8.0, group_ring());
        p.text(
            Pos2::new(x0, sum_y),
            Align2::LEFT_CENTER,
            crate::i18n::t!("队列"),
            font(SIZE_MICRO),
            dim(),
        );
        let pending = state
            .queue
            .iter()
            .filter(|q| matches!(q.status, aevum_core::queue::QueueStatus::Pending))
            .count();
        p.text(
            Pos2::new(x1, sum_y),
            Align2::RIGHT_CENTER,
            crate::i18n::tr_fmt("{} 项 · 待渲 {}", &[&state.queue.len(), &pending]),
            font(SIZE_MICRO),
            muted(),
        );
    }

    let resp = ui.interact(
        btn,
        ui.make_persistent_id("export-ws-primary"),
        Sense::click(),
    );
    // 空队列时这颗按钮没有可渲染的段：画成禁用态并吞掉点击，比「点了没反应」诚实。
    // 有队列时它是本页**唯一**的主动作（页头那颗同义按钮已删，见 `page`）。
    //
    // With an empty queue there is nothing to render: draw it disabled and swallow the
    // click, which beats a button that silently does nothing. When there is work it is this
    // page's **only** primary action (the duplicate in the head was removed, see `page`).
    let ready = !state.queue.is_empty();
    let fill = if !ready {
        field()
    } else if resp.is_pointer_button_down_on() {
        accent_hover()
    } else {
        accent()
    };
    p.rect_filled(btn, 6.0, fill);
    p.text(
        btn.center(),
        Align2::CENTER_CENTER,
        crate::i18n::t!("立即导出"),
        font(SIZE_LABEL),
        if ready { on_accent() } else { dim() },
    );
    if ready {
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked()
            && let Err(e) = state.start_queue(ctx)
        {
            state.error = Some(e);
        }
    }
}

/// 队列项名：取 root 的最后一段，取不到就用原串。
///
/// Queue entry name: the last segment of `root`, falling back to the raw string.
fn item_name(root: &str) -> String {
    std::path::Path::new(root)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root.to_owned())
}

/// 当前行的进度比例：只有跑着的那一行有真实进度，完成行满格，其余为 0。
///
/// The row's progress fraction: only the running row has real progress, finished rows are
/// full, everything else is zero.
fn row_fraction(state: &AppState, i: usize) -> f32 {
    if state.queue_current != Some(i) {
        return match state.queue.get(i).map(|q| q.status) {
            Some(aevum_core::queue::QueueStatus::Done) => 1.0,
            _ => 0.0,
        };
    }
    match state.queue_progress() {
        Some((done, total, _)) if total > 0 => (done as f32 / total as f32).clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// 输出路径拆成「目录 / 文件名」；空路径两段都给占位破折号。
///
/// Split an output path into directory and file name; an empty path yields placeholders
/// for both.
fn split_out(out: &str) -> (String, String) {
    if out.is_empty() {
        return ("—".to_owned(), "—".to_owned());
    }
    let path = std::path::Path::new(out);
    let dir = path
        .parent()
        .map(|d| d.display().to_string())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "—".to_owned());
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "—".to_owned());
    (dir, file)
}

/// 从尾部截断的单行文本：路径超宽时留尾段（文件名），前面用省略号。
///
/// 与 [`fit_text`] 相反——它砍尾。路径砍掉尾段就只剩一串上级目录，读不出是哪一段。
///
/// Tail-preserving single-line truncation for paths: the head goes, the file name stays.
/// [`fit_text`] cuts the tail, which for a path leaves a chain of parent folders and no
/// way to tell which segment it is.
fn fit_tail(
    painter: &Painter,
    text: &str,
    font_id: egui::FontId,
    color: Color32,
    max_w: f32,
) -> String {
    let width = |s: &str| {
        painter
            .layout_no_wrap(s.to_owned(), font_id.clone(), color)
            .size()
            .x
    };
    let full = width(text);
    if full <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = (max_w / (full / chars.len().max(1) as f32)).floor() as usize;
    loop {
        let start = chars.len().saturating_sub(keep.max(1));
        let mut out = String::from("…");
        out.extend(&chars[start..]);
        if width(&out) <= max_w || keep <= 1 {
            return out;
        }
        keep -= 1;
    }
}
