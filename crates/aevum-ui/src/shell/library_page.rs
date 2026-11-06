//! 项目库：所有序列工程的入口页（对齐 Lightroom Classic 的 Library 模块）。
//!
//! dock 里那个「图库」页签管的是**当前序列内部**的集合与目录；这一页管的是
//! **工程之间**的浏览与切换——两者不重叠，都保留。
//!
//! Library: the entry page for every sequence project (mirroring Lightroom Classic's
//! Library module). The dock's "图库" tab manages collections and folders **inside the
//! current sequence**; this page browses and switches **between projects**. The two do
//! not overlap, and both stay.

use super::*;
use crate::state::{AppState, DockTab};
use crate::theme::{self};
use egui::{
    Align, Align2, Color32, Layout, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind,
    Ui, UiBuilder, Vec2,
};

/// 卡片尺寸：240×188（缩略区 120 + 文本区 68）。与设计稿 `.proj-card` 的
/// `minmax(240px,1fr)` 同宽，缩略区取 120 是让一屏 3 行仍能看见名字。
///
/// Card size 240x188 (120 thumbnail band + 68 text band), matching the design's
/// `minmax(240px,1fr)`; the 120 px thumbnail keeps three rows on one screen with their
/// names still readable.
const CARD_W: f32 = 240.0;
const CARD_THUMB_H: f32 = 120.0;
const CARD_TEXT_H: f32 = 68.0;
const CARD_GAP: f32 = 16.0;

/// 页面外边距：与设计稿 `.page-pad{padding:24px 32px 64px}` 同口径。
///
/// Page padding, matching the design's `.page-pad`.
const PAGE_PAD_X: f32 = 32.0;
const PAGE_PAD_Y: f32 = 24.0;

pub(super) fn library_page(state: &mut AppState, ui: &mut Ui) {
    let full = ui.max_rect();
    let p = ui.painter().clone();

    // 整页底色用 surround（画布外那一层）：项目库不承载图像判断，但它与显影页
    // 来回切换，底色必须同一个「最暗层」，否则切模块时整屏亮度会跳。
    //
    // The page fill is `surround` -- the same darkest layer the canvas sits on. The
    // library carries no colour judgement, but switching to and from Develop must not
    // make the whole screen step brighter.
    p.rect_filled(full, 0.0, surround());

    let content = Rect::from_min_max(
        Pos2::new(full.left() + PAGE_PAD_X, full.top() + PAGE_PAD_Y),
        Pos2::new(full.right() - PAGE_PAD_X, full.bottom()),
    );

    let mut y = content.top();

    // ── 标题行 ────────────────────────────────────────────────────────
    let title = crate::i18n::t!("项目库");
    let tg = p.layout_no_wrap(title.to_owned(), bold(SIZE_HEADING), text());
    p.galley(Pos2::new(content.left(), y), tg.clone(), text());
    y += tg.size().y + 6.0;

    let desc = crate::i18n::t!("所有序列工程的总入口。点卡片切换到那个目录。");
    let dg = p.layout_no_wrap(desc.to_owned(), font(SIZE_LABEL), dim());
    p.galley(Pos2::new(content.left(), y), dg.clone(), dim());
    y += dg.size().y + 18.0;

    // ── 操作条：导入 / 重新扫描 ──────────────────────────────────────
    // 主行动唯一：只有「导入照片」是实心 accent（对齐设计稿「每视图仅 1 个主按钮」）。
    //
    // One primary action only: "导入照片" is the sole solid accent button, per the
    // design's "at most one primary button per view" rule.
    let actions = Rect::from_min_size(
        Pos2::new(content.left(), y),
        Vec2::new(content.width(), 32.0),
    );
    ui.scope_builder(
        UiBuilder::new().id_salt("lib-actions").max_rect(actions),
        |ui| {
            ui.horizontal(|ui| {
                if lib_primary(ui, crate::i18n::t!("导入照片…"), "Ctrl+I")
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_title(crate::i18n::t!("选择照片所在的文件夹"))
                        .pick_folder()
                {
                    state.open_paths(ui.ctx(), &[dir]);
                }
                if lib_secondary(ui, crate::i18n::t!("重新扫描"), "F5")
                    && let Some(i) = state.lib_sel
                {
                    state.lib_scan(i);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let n = state.lib_roots.len();
                    ui.label(
                        RichText::new(crate::i18n::tr_fmt("{} 个目录", &[&n]))
                            .size(SIZE_SMALL)
                            .color(dim()),
                    );
                });
            });
        },
    );
    y += 32.0 + 20.0;

    // ── 卡片网格 ──────────────────────────────────────────────────────
    let grid = Rect::from_min_max(
        Pos2::new(content.left(), y),
        Pos2::new(content.right(), content.bottom()),
    );
    let cols = (((grid.width() + CARD_GAP) / (CARD_W + CARD_GAP)).floor() as usize).max(1);

    if state.lib_roots.is_empty() {
        // 空态：三张「从哪开始」的引导卡，而不是一整片死黑中间一句小字。
        // 空态不画假工程卡片——那会让「还没导入」看起来像「已经有一堆工程」；
        // 但画**功能说明卡**是诚实的：每张卡说的都是真能点的事。
        //
        // Empty state: three "where to start" cards rather than one small line floating
        // in dead black. No fake project cards (that would make "nothing imported yet"
        // look like "a pile of projects"), but honest capability cards -- every one
        // names something that really works.
        empty_state(state, ui, grid);
        // 空态下半页给「工作流说明」：三张引导卡只回答了「怎么开始」，
        // 下半页回答「进来之后是什么流程」。不 early return，让版面站得住。
        //
        // The lower half of the empty state explains the workflow: the three guide cards
        // answer "how do I start", this answers "what happens next". No early return, so
        // the page is not half empty.
        workflow_section(ui, grid);
        return;
    }

    ScrollArea::vertical()
        .id_salt("lib-grid")
        .max_height(grid.height())
        .show(ui, |ui| {
            let mut cy = grid.top();
            for row_start in (0..state.lib_roots.len()).step_by(cols) {
                for (k, idx) in
                    (row_start..(row_start + cols).min(state.lib_roots.len())).enumerate()
                {
                    let x = grid.left() + k as f32 * (CARD_W + CARD_GAP);
                    project_card(
                        state,
                        ui,
                        idx,
                        Rect::from_min_size(
                            Pos2::new(x, cy),
                            Vec2::new(CARD_W, CARD_THUMB_H + CARD_TEXT_H),
                        ),
                    );
                }
                cy += CARD_THUMB_H + CARD_TEXT_H + CARD_GAP;
            }
            ui.allocate_space(Vec2::new(grid.width(), cy - grid.top()));
        });
    recent_section(state, ui, grid);
}

/// 一张工程卡片：缩略区（当前序列缩略图拼条）+ 名称 + 元信息。
///
/// One project card: a thumbnail band (strip of current-sequence previews), the name,
/// and metadata.
fn project_card(state: &mut AppState, ui: &mut Ui, idx: usize, rect: Rect) {
    let p = ui.painter().clone();
    let hovered = ui.rect_contains_pointer(rect);

    // 卡片底：panel 一档，描边 hover 提强（设计稿 `.proj-card:hover`）。
    //
    // Card fill is one step up (panel); the border strengthens on hover, matching the
    // design's `.proj-card:hover`.
    p.rect_filled(rect, radius_panel(), panel());
    p.rect_stroke(
        rect,
        radius_panel(),
        Stroke::new(1.0, if hovered { border_strong() } else { border() }),
        StrokeKind::Inside,
    );

    // 缩略区：三格拼条，借用当前序列的 `thumbs` 纹理。不新建纹理管线——那会多一套
    // 缓存与一次解码，帧时预算判据会当场看见。
    //
    // Thumbnail band: a three-up strip borrowing the current sequence's `thumbs`
    // textures. No second pipeline -- that would add a cache and a decode, and the frame
    // budget criterion would notice.
    let thumb = Rect::from_min_size(rect.min, Vec2::new(rect.width(), CARD_THUMB_H));
    let inner = thumb.shrink2(Vec2::new(8.0, 10.0));
    let slot_w = (inner.width() - 4.0) / 3.0;
    for k in 0..3usize {
        let slot = Rect::from_min_size(
            Pos2::new(inner.left() + k as f32 * (slot_w + 2.0), inner.top()),
            Vec2::new(slot_w, inner.height()),
        );
        p.rect_filled(slot, 2.0, thumb_bg());
        if let Some(t) = state.thumbs.get(&k) {
            let size = Vec2::new(t.size[0] as f32, t.size[1] as f32);
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            p.image(t.texture.id(), cover_rect(slot, size), uv, Color32::WHITE);
        }
    }

    // 文本区：名称（13 半粗）+ 元信息（11 dim）。
    let text_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 14.0, thumb.bottom() + 12.0),
        Vec2::new(rect.width() - 28.0, CARD_TEXT_H - 20.0),
    );
    let name = state
        .lib_roots
        .get(idx)
        .and_then(|q| q.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| crate::i18n::t!("未命名工程").to_owned());
    let ng = p.layout_no_wrap(
        theme::ellipsis(&name, crate::i18n::current().ellipsis_chars()),
        bold(SIZE_LABEL + 1.0),
        text(),
    );
    p.galley(text_rect.min, ng, text());

    // 元信息：文件数。数字走 mono，与全应用读数口径一致。
    //
    // Metadata: file count, numbers in the app-wide monospace readout face.
    let files = state.lib_entries.get(idx).map(|v| v.len()).unwrap_or(0);
    let meta = crate::i18n::tr_fmt("{} 个文件", &[&files]);
    p.text(
        Pos2::new(text_rect.left(), text_rect.top() + 22.0),
        Align2::LEFT_TOP,
        meta,
        font(SIZE_SMALL),
        dim(),
    );

    // 选中态：当前根目录亮一条 accent 左缘竖条（与工具轨激活态同一套语言）。
    //
    // Selected state: the active root gets a 2 px accent bar on its left edge, the same
    // language as the tool rail's active marker.
    if state.lib_sel == Some(idx) {
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.left() + 1.0, rect.top() + 10.0),
                Pos2::new(rect.left() + 3.0, rect.bottom() - 10.0),
            ),
            1.0,
            accent(),
        );
    }

    let resp = ui.interact(
        rect,
        ui.make_persistent_id(("proj-card", idx)),
        Sense::click(),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        state.lib_sel = Some(idx);
        state.lib_col = None;
        state.dock_tab = DockTab::Library;
    }
    resp.on_hover_text(crate::i18n::t!("点一下在右侧「图库」页签里浏览这个目录"));
}

/// 等比填充矩形（cover：短边贴满，长边居中裁切）。
///
/// Cover-fit rect: the short edge fills, the long edge is centre-cropped.
fn cover_rect(bounds: Rect, tex: Vec2) -> Rect {
    if tex.x <= 0.0 || tex.y <= 0.0 {
        return bounds;
    }
    let scale = (bounds.width() / tex.x).max(bounds.height() / tex.y);
    Rect::from_center_size(bounds.center(), Vec2::new(tex.x * scale, tex.y * scale))
}

/// 主按钮：实心 accent + 白字，悬停显示快捷键。
///
/// Primary button: solid accent with white text; the shortcut shows on hover.
fn lib_primary(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(SIZE_LABEL), on_accent());
    let w = galley.size().x + 28.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 32.0), Sense::click());
    let p = ui.painter();
    let fill = if resp.is_pointer_button_down_on() {
        accent().gamma_multiply(0.82)
    } else if resp.hovered() {
        accent_hover()
    } else {
        accent()
    };
    p.rect_filled(rect, radius_control(), fill);
    p.galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        on_accent(),
    );
    if !shortcut.is_empty() {
        resp.clone().on_hover_text(shortcut);
    }
    resp.clicked()
}

/// 次按钮：elev 底 + text 字（设计稿 `.btn-secondary`）。
///
/// Secondary button: `elev` fill with body text, per the design's `.btn-secondary`.
fn lib_secondary(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(SIZE_LABEL), text());
    let w = galley.size().x + 28.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 32.0), Sense::click());
    let p = ui.painter();
    let fill = if resp.is_pointer_button_down_on() {
        palette().press
    } else if resp.hovered() {
        elev_hover()
    } else {
        elev()
    };
    p.rect_filled(rect, radius_control(), fill);
    p.galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        text(),
    );
    if !shortcut.is_empty() {
        resp.clone().on_hover_text(shortcut);
    }
    resp.clicked()
}

/// 空态引导：三张卡——导入、快捷键、当前支持格式。
///
/// Empty-state guide: three cards -- import, shortcuts, supported formats.
fn empty_state(_state: &mut AppState, ui: &mut Ui, area: Rect) {
    let p = ui.painter().clone();
    let gap = 16.0;
    let n = 3;
    let card_w = ((area.width() - gap * (n as f32 - 1.0)) / n as f32).min(300.0);
    let card_h = 132.0;
    let total_w = card_w * n as f32 + gap * (n as f32 - 1.0);
    let x0 = area.center().x - total_w * 0.5;
    // 两区（引导卡 + 工作流）合成一个整体在区域内竖直居中：整页高度 = 引导卡
    // card_h + 间距 + 工作流（26 标题 + 96 卡），余量上下对半分。
    // 早先按 8% 顶置，下半页留出一大片死黑，重心整个偏上。
    //
    // The two blocks (guide cards + workflow) are centred as one unit: total height =
    // guide cards + gap + workflow (26 heading + 96 cards), with the remainder split
    // above and below. The earlier 8% top bias left a dead black lower half and pulled
    // the whole page's weight to the top.
    let block_h = card_h + 16.0 + 26.0 + 96.0;
    let slack = (area.height() - block_h).max(0.0);
    let y0 = area.top() + (slack * 0.42).max(16.0);

    let cards: [(&str, &str); 3] = [
        (
            "① 导入照片",
            "点左上角「导入照片…」选一个文件夹。支持 RAW / DNG / JPEG / TIFF。",
        ),
        (
            "② 快捷键",
            "Ctrl+I 导入 · Ctrl+O 打开序列；F5 重新扫描 · Ctrl+K 命令面板。",
        ),
        (
            "③ 非破坏性",
            "所有调整存成参数栈，原片永不覆写；导出才生成新文件。",
        ),
    ];

    for (i, (title, body)) in cards.iter().enumerate() {
        let rect = Rect::from_min_size(
            Pos2::new(x0 + i as f32 * (card_w + gap), y0),
            Vec2::new(card_w, card_h),
        );
        p.rect_filled(rect, radius_panel(), panel());
        p.rect_stroke(
            rect,
            radius_panel(),
            Stroke::new(1.0, border()),
            StrokeKind::Inside,
        );
        p.text(
            Pos2::new(rect.left() + 16.0, rect.top() + 18.0),
            Align2::LEFT_TOP,
            crate::i18n::tr(title),
            bold(SIZE_LABEL + 1.0),
            text(),
        );
        // 正文交给 `layout()` 按卡片宽度自动折行：手拆会让译文表的键在源码里
        // 只剩片段，`no_row_is_orphaned` 会把整句译文记成「无人引用」。
        //
        // The body is wrapped by `layout()` against the card width; splitting by hand
        // leaves the translation key appearing only as a fragment in the source, and
        // `no_row_is_orphaned` records the whole sentence as unreferenced.
        let g = p.layout(
            crate::i18n::tr(body).to_owned(),
            font(SIZE_SMALL),
            muted(),
            rect.width() - 32.0,
        );
        p.galley(Pos2::new(rect.left() + 16.0, rect.top() + 44.0), g, muted());
    }

    // 底部一行：说明「这一页与右侧图库页签的分工」，免得两个入口打架。
    // 位置放在**两张卡之间**的下方留白里，不与下面的「工作流」标题争同一条基线。
    //
    // One line spelling out how this page divides work with the dock's 图库 tab. It sits
    // inside the gap under the cards, not on the same baseline as the workflow heading
    // below it.
    p.text(
        Pos2::new(area.center().x, y0 + card_h + 14.0),
        Align2::CENTER_TOP,
        crate::i18n::t!("这一页切换工程；当前序列的集合、目录与评选在右侧「图库」页签里。"),
        font(SIZE_SMALL),
        dim(),
    );
}

/// 「最近打开」区块：右栏式的时间线列表，把下半页填住。
///
/// 数据来源是 `lib_roots` 的修改时间——那是这一页真实握有的东西。没有历史记录
/// 就不编一条假的「最近打开」出来。
///
/// Recent-opens block: a right-hand-column timeline that fills the lower half. The data
/// is each root's modification time, which is what this page actually holds. No invented
/// history.
fn recent_section(state: &mut AppState, ui: &mut Ui, area: Rect) {
    if state.lib_roots.is_empty() {
        return;
    }
    let p = ui.painter().clone();
    let top = area.top() + 240.0;
    if top > area.bottom() - 60.0 {
        return;
    }
    let head = crate::i18n::t!("最近打开");
    p.text(
        Pos2::new(area.left(), top),
        Align2::LEFT_TOP,
        head,
        bold(SIZE_LABEL + 1.0),
        text(),
    );

    // 取前 5 个根目录，按修改时间倒序（读不到时间就按原序）。
    //
    // Up to five roots, newest modification first; unreadable times keep their order.
    let mut items: Vec<(usize, Option<std::time::SystemTime>)> = state
        .lib_roots
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let t = std::fs::metadata(q).and_then(|m| m.modified()).ok();
            (i, t)
        })
        .collect();
    items.sort_by_key(|a| std::cmp::Reverse(a.1));

    let row_h = 30.0;
    let mut y = top + 24.0;
    for (idx, mtime) in items.iter().take(5) {
        if y + row_h > area.bottom() {
            break;
        }
        let row = Rect::from_min_size(Pos2::new(area.left(), y), Vec2::new(area.width(), row_h));
        let hovered = ui.rect_contains_pointer(row);
        if hovered {
            p.rect_filled(row, radius_control(), elev());
        }
        let name = state
            .lib_roots
            .get(*idx)
            .and_then(|q| q.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| crate::i18n::t!("未命名").to_owned());
        p.text(
            Pos2::new(row.left() + 10.0, row.center().y),
            Align2::LEFT_CENTER,
            theme::ellipsis(&name, crate::i18n::current().ellipsis_chars()),
            font(SIZE_LABEL),
            if hovered { text() } else { muted() },
        );
        if let Some(t) = mtime {
            let secs = t
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            p.text(
                Pos2::new(row.right() - 10.0, row.center().y),
                Align2::RIGHT_CENTER,
                short_stamp(secs),
                mono(SIZE_SMALL),
                dim(),
            );
        }
        let ly = row.bottom() - 1.0;
        p.line_segment(
            [Pos2::new(row.left(), ly), Pos2::new(row.right(), ly)],
            Stroke::new(1.0, divider()),
        );
        let resp = ui.interact(
            row,
            ui.make_persistent_id(("recent-row", idx)),
            Sense::click(),
        );
        if resp.clicked() {
            state.lib_sel = Some(*idx);
            state.lib_col = None;
            state.dock_tab = DockTab::Library;
        }
        y += row_h;
    }
}

/// Unix 秒 → 简短时间戳（`2026-11-02 21:14`）。
///
/// 纯函数：不读时钟、不依赖本地时区库——只把给定的秒数按 UTC 展开。
/// 这样它可被单测钉住；界面在同一帧里不会因为跨秒而抖动。
///
/// Unix seconds to a short stamp. Pure: it reads no clock and depends on no timezone
/// library, only expanding the given seconds in UTC. That makes it testable, and the UI
/// cannot jitter mid-frame as the clock ticks over.
fn short_stamp(unix_secs: u64) -> String {
    // 民用历换算（Howard Hinnant 的 days_from_civil 逆运算），1970 起算。
    //
    // Civil-calendar conversion (the inverse of Hinnant's days_from_civil), epoch 1970.
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let (h, mi) = (rem / 3600, (rem % 3600) / 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// 空态下半页：四步工作流说明（导入 → 筛选 → 显影 → 导出）。
///
/// 为什么画这个：一个只有三张卡的空页会在下半页留一大片死黑，读起来像加载失败。
/// 这一条把「这个软件怎么用」按顺序讲清楚，且每一步都是真的模块名。
///
/// The lower half of the empty state: a four-step workflow (import -> cull -> develop ->
/// export). Why: three cards alone leave a dead black lower half that reads like a failed
/// load. This spells out the order, and every step names a module that really exists.
fn workflow_section(ui: &mut Ui, area: Rect) {
    let p = ui.painter().clone();
    // 位置由 empty_state 的居中算式给出：引导卡区高度 + 一行分工说明 + 间距。
    // 与 empty_state 用同一套数字，两块不会互相压。
    //
    // The position comes from the same centring arithmetic as `empty_state`: guide-card
    // band + the分工 line + a gap. Sharing the numbers keeps the two blocks off each
    // other.
    let block_h = 132.0 + 32.0 + 26.0 + 96.0;
    let slack = (area.height() - block_h).max(0.0);
    let top = area.top() + (slack * 0.42).max(16.0) + 132.0 + 32.0 + 26.0;
    if top > area.bottom() - 80.0 {
        return;
    }
    let steps: [(&str, &str); 4] = [
        ("1 · 序列", "导入后先看帧数与缺帧，评星、旗标把可用的挑出来"),
        (
            "2 · 显影",
            "非破坏性调色：基本 / 曲线 / 混色器 / 分级 / 细节",
        ),
        ("3 · 构图 · 成片", "裁切拉直、选输出规格与色彩空间"),
        ("4 · 导出", "生成新文件，原片与工程参数都不被改写"),
    ];
    let gap = 12.0;
    let n = steps.len() as f32;
    let card_w = ((area.width() - gap * (n - 1.0)) / n).min(280.0);
    let total_w = card_w * n + gap * (n - 1.0);
    let x0 = area.center().x - total_w * 0.5;
    let card_h = 96.0;

    p.text(
        Pos2::new(area.center().x, top),
        Align2::CENTER_TOP,
        crate::i18n::t!("工作流"),
        bold(SIZE_LABEL + 1.0),
        text(),
    );

    for (i, (title, body)) in steps.iter().enumerate() {
        let rect = Rect::from_min_size(
            Pos2::new(x0 + i as f32 * (card_w + gap), top + 26.0),
            Vec2::new(card_w, card_h),
        );
        p.rect_filled(rect, radius_panel(), panel());
        p.rect_stroke(
            rect,
            radius_panel(),
            Stroke::new(1.0, border()),
            StrokeKind::Inside,
        );
        p.text(
            Pos2::new(rect.left() + 14.0, rect.top() + 14.0),
            Align2::LEFT_TOP,
            crate::i18n::tr(title),
            bold(SIZE_LABEL),
            text(),
        );
        let g = p.layout(
            crate::i18n::tr(body).to_owned(),
            font(SIZE_SMALL),
            muted(),
            rect.width() - 28.0,
        );
        p.galley(Pos2::new(rect.left() + 14.0, rect.top() + 38.0), g, muted());
    }
    let _ = ui;
}

#[cfg(test)]
mod stamp_tests {
    use super::short_stamp;

    /// 已知时刻的换算：钉住「纯函数」这个承诺，也让时区/闰年改错时当场红。
    ///
    /// Known instants: this pins the "pure function" promise and goes red immediately if
    /// the leap-year or month arithmetic is mistyped.
    #[test]
    fn civil_conversion_matches_known_instants() {
        assert_eq!(short_stamp(0), "1970-01-01 00:00");
        assert_eq!(short_stamp(86_340), "1970-01-01 23:59");
        assert_eq!(short_stamp(999_999_960), "2001-09-09 01:46");
        // 2024-02-29（闰日）
        assert_eq!(short_stamp(1_709_164_800), "2024-02-29 00:00");
        // 2026-11-02 21:14 UTC
        assert_eq!(short_stamp(1_793_654_040), "2026-11-02 21:14");
    }
}
