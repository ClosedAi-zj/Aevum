//! 帧表卡：卡头 32 + 范围条 30 + 选择条 30 + 表头 26 + 行 26 + 页脚 22。
//!
//! 设计稿 `#vTable .ftablecard`：12px 圆角卡片；`.ftable-h` 卡头、`.ftable-scope` 范围条、
//! `.ftable-sel` 选择条、`.ftable th` 26px、`.ftable td` 行高 26px；选中行淡蓝底 +
//! 左侧 2px 强调条；列宽见 [`COLS`]。
//!
//! The frame-table card: 32 px head, 30 px scope bar, 30 px selection bar, 26 px column
//! header, 26 px rows and a 22 px footer, per `#vTable .ftablecard`.

use egui::{Align, Align2, Color32, Pos2, Rect, ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::state::AppState;
use crate::theme::*;

/// 列：标题、宽（px，0 = 吃剩余宽度）、对齐、是否数值列。
///
/// Columns: title, width (0 = take the remaining space), alignment and numeric flag.
const COLS: [(&str, f32, Align, bool); 12] = [
    ("#", 56.0, Align::LEFT, false),
    ("编号", 64.0, Align::RIGHT, true),
    ("亮度", 56.0, Align::RIGHT, true),
    ("间隔", 52.0, Align::RIGHT, true),
    ("快门", 62.0, Align::RIGHT, true),
    ("光圈", 50.0, Align::RIGHT, true),
    ("ISO", 46.0, Align::RIGHT, true),
    ("焦距", 62.0, Align::RIGHT, true),
    ("白平衡", 64.0, Align::RIGHT, false),
    ("文件名", 0.0, Align::LEFT, false),
    ("尺寸", 96.0, Align::RIGHT, true),
    ("日期 / 时间", 140.0, Align::RIGHT, true),
];

/// 前 `cols` 列的定宽之和；文件名列吃剩余宽度。
///
/// Sum of the fixed widths of the first `cols` columns; the file-name column takes the rest.
fn fixed_width(cols: usize) -> f32 {
    COLS.iter()
        .take(cols)
        .filter(|c| c.1 > 0.0)
        .map(|c| c.1)
        .sum()
}

/// 卡头 32（`.ftable-h{height:32px}`）。
///
/// Card head height.
const HEAD: f32 = 32.0;
/// 范围条 / 选择条各 30（`.ftable-scope` / `.ftable-sel`）。
///
/// Scope and selection bars.
const SCOPE_H: f32 = 30.0;
const SEL_H: f32 = 30.0;
/// 表头 26（`.ftable th{height:26px}`）。
///
/// Column header height.
const TH: f32 = 26.0;
/// 行高 26、页脚 22。
///
/// Row and footer heights.
const ROW: f32 = 26.0;
const FOOT: f32 = 22.0;
/// 单元格左右内距 8（设计稿 `td{padding:0 7px}`）。
///
/// Cell horizontal inset.
const PAD_X: f32 = 8.0;
/// 缩略图 40×28、圆角 4。
///
/// Thumbnail size.
const THUMB: Vec2 = Vec2::new(40.0, 28.0);

/// 范围筛选的四个口径（`data-scope`）。
///
/// The four scope filters (design's `data-scope`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    Keys,
    Offset,
    Unmeasured,
}

/// 画整张帧表卡。
///
/// Draw the frame-table card.
pub(super) fn card(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    if rect.width() < 200.0 || rect.height() < 150.0 {
        return;
    }
    let p = ui.painter().clone();
    p.rect_filled(rect, radius_panel(), panel());
    p.rect_stroke(
        rect,
        radius_panel(),
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );

    let n = state.frame_count();
    let sel = if n == 0 { 0 } else { state.selected.min(n - 1) };
    let keys = state.keyframes.len();
    // 窄窗口跟着设计稿的媒体查询收列：卡片放不下时先舍「尺寸 / 日期」，
    // 不让最后两列压到卡片外面。
    //
    // Narrow cards drop the last two columns, as the mockup's media query does, instead of
    // painting them outside the card.
    let cols = if rect.width() >= 980.0 {
        COLS.len()
    } else {
        10
    };
    let name_w = (rect.width() - fixed_width(cols)).max(96.0);

    // ── 卡头 32：标题 + 行数 + 图例 ───────────────────────────────────
    //
    // Card head: title, row count and the K legend.
    let head = Rect::from_min_size(rect.left_top(), Vec2::new(rect.width(), HEAD));
    p.text(
        Pos2::new(head.left() + 10.0, head.center().y),
        Align2::LEFT_CENTER,
        "逐帧数据",
        bold(12.0),
        text(),
    );
    p.text(
        Pos2::new(head.left() + 66.0, head.center().y),
        Align2::LEFT_CENTER,
        format!("{} 行 · 关键帧 {}", thousands(n), thousands(keys)),
        font(10.0),
        dim(),
    );
    let legend = "K = 关键帧";
    let lg = p.layout_no_wrap(legend.to_owned(), font(9.0), dim());
    p.text(
        Pos2::new(head.right() - 10.0, head.center().y),
        Align2::RIGHT_CENTER,
        legend,
        font(9.0),
        dim(),
    );
    p.rect_filled(
        Rect::from_min_size(
            Pos2::new(
                head.right() - 10.0 - lg.size().x - 12.0,
                head.center().y - 3.0,
            ),
            Vec2::new(6.0, 6.0),
        ),
        1.0,
        warn(),
    );
    p.line_segment(
        [head.left_bottom(), head.right_bottom()],
        Stroke::new(1.0, divider()),
    );

    // ── 范围条 30 ────────────────────────────────────────────────────
    //
    // Scope bar.
    let scope_rect = Rect::from_min_size(
        Pos2::new(rect.left(), head.bottom()),
        Vec2::new(rect.width(), SCOPE_H),
    );
    let scope = scope_bar(state, ui, scope_rect);
    p.line_segment(
        [scope_rect.left_bottom(), scope_rect.right_bottom()],
        Stroke::new(1.0, divider()),
    );

    // ── 选择条 30 ────────────────────────────────────────────────────
    //
    // Selection bar.
    let sel_rect = Rect::from_min_size(
        Pos2::new(rect.left(), scope_rect.bottom()),
        Vec2::new(rect.width(), SEL_H),
    );
    selection_bar(state, ui, sel_rect, sel, n);
    p.line_segment(
        [sel_rect.left_bottom(), sel_rect.right_bottom()],
        Stroke::new(1.0, divider()),
    );

    // ── 表头 26 ──────────────────────────────────────────────────────
    //
    // Column header.
    let th = Rect::from_min_size(
        Pos2::new(rect.left(), sel_rect.bottom()),
        Vec2::new(rect.width(), TH),
    );
    p.rect_filled(th, 0.0, field());
    let mut x = th.left();
    for (title, w, align, _) in COLS.iter().take(cols) {
        let cw = if *w <= 0.0 { name_w } else { *w };
        let (at, a2) = if *align == Align::RIGHT {
            (
                Pos2::new(x + cw - PAD_X, th.center().y),
                Align2::RIGHT_CENTER,
            )
        } else {
            (Pos2::new(x + PAD_X, th.center().y), Align2::LEFT_CENTER)
        };
        p.text(at, a2, *title, font(10.0), dim());
        x += cw;
    }
    p.line_segment(
        [th.left_bottom(), th.right_bottom()],
        Stroke::new(1.0, divider()),
    );

    // ── 行区（虚拟化）────────────────────────────────────────────────
    //
    // Virtualised rows.
    let rows_rect = Rect::from_min_max(
        Pos2::new(rect.left(), th.bottom()),
        Pos2::new(rect.right(), rect.bottom() - FOOT),
    );
    let rows: Vec<usize> = (0..n)
        .filter(|i| match scope {
            Scope::All => true,
            Scope::Keys => state.keyframes.contains_key(i),
            Scope::Offset => state.corr.get(*i).is_some_and(|v| v.abs() > 1e-3),
            Scope::Unmeasured => state.metrics.get(*i).is_none_or(|m| m.is_none()),
        })
        .collect();

    let mut click = None;
    let mut open = None;
    if rows.is_empty() {
        let msg = if n == 0 {
            "还没有序列 · 文件 → 打开照片…"
        } else {
            "当前范围没有帧"
        };
        p.text(
            rows_rect.center(),
            Align2::CENTER_CENTER,
            msg,
            font(11.0),
            dim(),
        );
    } else {
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rows_rect)
                .layout(egui::Layout::top_down(Align::Min)),
        );
        ScrollArea::vertical()
            .id_salt("seq-table-rows")
            .auto_shrink([false, false])
            .show_rows(&mut child, ROW, rows.len(), |ui, range| {
                // 缩略图按本批可见行一次派发：逐行调用会把 `preview_params()` 和距离
                // 排序重复二十遍。
                //
                // Dispatch thumbnails once per visible batch: calling per row repeats
                // `preview_params()` and the distance sort twenty times.
                let lo = rows.get(range.start).copied().unwrap_or(0);
                let hi = rows
                    .get(range.end.saturating_sub(1))
                    .copied()
                    .map(|v| v + 1)
                    .unwrap_or(lo);
                state.ensure_thumbs(ui.ctx(), lo, hi);

                for k in range {
                    let Some(&i) = rows.get(k) else { continue };
                    let (row, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), ROW), Sense::click());
                    let rp = ui.painter();
                    if i == sel {
                        rp.rect_filled(row, 0.0, selected());
                        rp.rect_filled(
                            Rect::from_min_size(row.left_top(), Vec2::new(2.0, row.height())),
                            0.0,
                            accent(),
                        );
                    } else if resp.hovered() {
                        rp.rect_filled(row, 0.0, row_hover());
                    }
                    // 隔行一条 1px 分隔（设计稿 `td` 的 hairline）。
                    //
                    // A 1 px hairline on every other row.
                    if i % 2 == 1 && i != sel {
                        rp.line_segment(
                            [
                                Pos2::new(row.left(), row.bottom() - 0.5),
                                Pos2::new(row.right(), row.bottom() - 0.5),
                            ],
                            Stroke::new(1.0, divider()),
                        );
                    }

                    let ex = state.exif.get(i);
                    let m = state.metrics.get(i).and_then(|m| m.as_ref());
                    let fps = state.output_fps;
                    let interval = if fps > 0.1 {
                        let sec = 1.0 / fps;
                        if sec >= 1.0 {
                            format!("{sec:.1} s")
                        } else {
                            format!("{:.0} ms", sec * 1000.0)
                        }
                    } else {
                        "—".into()
                    };
                    let name = state
                        .seq
                        .as_ref()
                        .and_then(|s| s.frames.get(i))
                        .map(|f| f.name.clone())
                        .unwrap_or_default();
                    let cells: [String; 12] = [
                        String::new(),
                        format!("{:04}", i + 1),
                        m.map(|m| format!("{:.3}", m.mean))
                            .unwrap_or_else(|| "—".into()),
                        interval,
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
                        // EXIF 里没有白平衡访问器（`Exif` 只暴露曝光三要素与机型），
                        // 没有数据就写 `—`，不猜。
                        //
                        // `Exif` exposes no white-balance accessor, so the cell stays at `—`
                        // rather than guessing.
                        "—".into(),
                        name,
                        state
                            .resolution
                            .map(|(rw, rh)| format!("{rw} × {rh}"))
                            .unwrap_or_else(|| "—".into()),
                        ex.and_then(|e| e.datetime())
                            .map(datetime_text)
                            .unwrap_or_else(|| "—".into()),
                    ];

                    let mut x = row.left();
                    for (c, (_, w, align, num)) in COLS.iter().take(cols).enumerate() {
                        let cw = if *w <= 0.0 { name_w } else { *w };
                        if c == 0 {
                            draw_thumb(&state_texture(state, i), rp, row, x);
                        } else if let Some(value) = cells.get(c) {
                            // 关键帧在编号列带一枚 10px 的 K 徽章（设计稿 `.ftable .k`），
                            // 表头那句「K = 关键帧」才有落点。
                            //
                            // Keyframes carry a 10 px K badge in the index cell, which is
                            // what the "K = keyframe" legend refers to.
                            if c == 1 && state.keyframes.contains_key(&i) {
                                let kr = Rect::from_min_size(
                                    Pos2::new(x + PAD_X, row.center().y - 5.0),
                                    Vec2::splat(10.0),
                                );
                                rp.rect_filled(kr, 2.0, warn());
                                rp.text(
                                    kr.center(),
                                    Align2::CENTER_CENTER,
                                    "K",
                                    font(9.0),
                                    bench_bg(),
                                );
                            }
                            let font = if *num || c == 1 {
                                mono(11.0)
                            } else {
                                font(11.0)
                            };
                            let ink = if i == sel {
                                text()
                            } else if *num || c == 1 {
                                muted()
                            } else {
                                dim()
                            };
                            let max_w = cw - PAD_X * 2.0;
                            let shown = fit_text(rp, value, font.clone(), ink, max_w);
                            let (at, a2) = if *align == Align::RIGHT {
                                (
                                    Pos2::new(x + cw - PAD_X, row.center().y),
                                    Align2::RIGHT_CENTER,
                                )
                            } else {
                                (Pos2::new(x + PAD_X, row.center().y), Align2::LEFT_CENTER)
                            };
                            rp.text(at, a2, shown, font, ink);
                        }
                        x += cw;
                    }

                    if resp.clicked() {
                        click = Some(i);
                    }
                    if resp.double_clicked() {
                        open = Some(i);
                    }
                }
            });
    }

    // ── 页脚 22 ──────────────────────────────────────────────────────
    //
    // Footer.
    let foot = Rect::from_min_size(
        Pos2::new(rect.left(), rect.bottom() - FOOT),
        Vec2::new(rect.width(), FOOT),
    );
    p.line_segment(
        [foot.left_top(), foot.right_top()],
        Stroke::new(1.0, divider()),
    );
    p.text(
        Pos2::new(foot.left() + 10.0, foot.center().y),
        Align2::LEFT_CENTER,
        format!(
            "第 {} / {} 帧 · 关键帧 {}",
            if n == 0 { 0 } else { sel + 1 },
            n,
            keys
        ),
        font(9.0),
        dim(),
    );
    p.text(
        Pos2::new(foot.right() - 10.0, foot.center().y),
        Align2::RIGHT_CENTER,
        format!("显示 {} / {} 行", thousands(rows.len()), thousands(n)),
        font(9.0),
        dim(),
    );

    if let Some(i) = click {
        state.selected = i;
    }
    if let Some(i) = open {
        state.selected = i;
        state.set_bench(crate::shell::Bench::Develop);
    }
}

/// 取某帧缩略图的纹理与尺寸；缺图时返回 `None`，由 [`draw_thumb`] 画 `…`。
///
/// The thumbnail texture and size for one frame; `None` makes [`draw_thumb`] paint `…`.
fn state_texture(state: &AppState, i: usize) -> Option<(egui::TextureId, [u32; 2])> {
    state.thumb(i).map(|t| (t.texture.id(), t.size))
}

/// 画 40×28、圆角 4 的缩略图；缺图时画 `…`。
///
/// Draw the 40×28 thumbnail with a 4 px radius; paint `…` when the texture is missing.
fn draw_thumb(tex: &Option<(egui::TextureId, [u32; 2])>, p: &egui::Painter, row: Rect, x: f32) {
    let r = Rect::from_min_size(Pos2::new(x + PAD_X, row.center().y - THUMB.y * 0.5), THUMB);
    // 40×28 的图比 26px 的行高多 2px，整块裁到行内，否则会压到上下两行。
    //
    // The 40×28 thumb is 2 px taller than the 26 px row; clip the whole thing to the row so
    // it cannot bleed into its neighbours.
    let p = p.with_clip_rect(row.intersect(p.clip_rect()));
    p.rect_filled(r, radius_chip(), thumb_bg());
    match tex {
        Some((id, size)) => {
            let sw = size[0].max(1) as f32;
            let sh = size[1].max(1) as f32;
            let scale = (r.width() / sw).max(r.height() / sh);
            let draw = Rect::from_center_size(r.center(), Vec2::new(sw * scale, sh * scale));
            p.image(
                *id,
                draw,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            p.text(r.center(), Align2::CENTER_CENTER, "…", font(10.0), dim());
        }
    }
}

/// 范围条：全部 / 已打点 / 有去闪偏移 / 未测光。返回当前筛选口径。
///
/// Scope bar: all / keyed / with deflicker offset / unmeasured. Returns the active filter.
fn scope_bar(state: &AppState, ui: &mut Ui, rect: Rect) -> Scope {
    let p = ui.painter().clone();
    let id = egui::Id::unique("aevum-seq-table-scope");
    let cur = ui
        .ctx()
        .data(|d| d.get_temp::<Scope>(id))
        .unwrap_or(Scope::All);

    let n = state.frame_count();
    let keys = state.keyframes.len();
    let offset = state.corr.iter().filter(|v| v.abs() > 1e-3).count();
    let unmeasured = (0..n)
        .filter(|i| state.metrics.get(*i).is_none_or(|m| m.is_none()))
        .count();
    let items = [
        (Scope::All, "全部", n),
        (Scope::Keys, "已打点", keys),
        (Scope::Offset, "有去闪偏移", offset),
        (Scope::Unmeasured, "未测光", unmeasured),
    ];

    let mut pick = None;
    let mut x = rect.left() + 10.0;
    for (s, label, count) in items {
        let on = s == cur;
        let fg = if on { text() } else { dim() };
        let lg = p.layout_no_wrap(label.to_owned(), font(10.0), fg);
        let cg = p.layout_no_wrap(thousands(count), font(10.0), dim());
        let bw = lg.size().x + cg.size().x + 20.0;
        let r = Rect::from_min_size(Pos2::new(x, rect.center().y - 10.0), Vec2::new(bw, 20.0));
        let resp = ui.interact(r, ui.make_persistent_id(("scope", label)), Sense::click());
        if on {
            p.rect_filled(r, radius_chip(), accent_dim());
        } else if resp.hovered() {
            p.rect_filled(r, radius_chip(), elev());
        }
        p.galley(
            Pos2::new(r.left() + 7.0, r.center().y - lg.size().y * 0.5),
            lg,
            fg,
        );
        p.galley(
            Pos2::new(
                r.right() - 7.0 - cg.size().x,
                r.center().y - cg.size().y * 0.5,
            ),
            cg,
            dim(),
        );
        if resp.clicked() && !on {
            pick = Some(s);
        }
        x = r.right() + 2.0;
    }

    if let Some(s) = pick {
        ui.ctx().data_mut(|d| d.insert_temp(id, s));
        return s;
    }
    cur
}

/// 选择条：已选读数 + 在显影中打开（可用）/ 导出所选、取消全选（置灰）。
///
/// Selection bar: the selected-frame readout plus the actions that this bench can actually
/// perform; unavailable ones are dimmed.
fn selection_bar(state: &mut AppState, ui: &mut Ui, rect: Rect, sel: usize, n: usize) {
    let p = ui.painter().clone();
    let a = accent();
    p.rect_filled(
        rect,
        0.0,
        Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 13),
    );

    let info = if n == 0 {
        "未选行".to_owned()
    } else {
        let name = state
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(sel))
            .map(|f| f.name.clone())
            .unwrap_or_default();
        format!("已选 #{:04} · {name}", sel + 1)
    };
    let info = fit_text(
        &p,
        &info,
        font(10.0),
        muted(),
        (rect.width() * 0.5 - 60.0).max(80.0),
    );
    let iw = p.layout_no_wrap(info.clone(), font(10.0), muted()).size().x;
    p.text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        info,
        font(10.0),
        muted(),
    );

    let sep_x = rect.left() + 10.0 + iw + 10.0;
    p.line_segment(
        [
            Pos2::new(sep_x, rect.center().y - 7.0),
            Pos2::new(sep_x, rect.center().y + 7.0),
        ],
        Stroke::new(1.0, divider()),
    );

    let (r, resp) = bar_button(ui, &p, sep_x + 6.0, rect, "在显影中打开", n > 0);
    if resp.clicked() && n > 0 {
        state.selected = sel;
        state.set_bench(crate::shell::Bench::Develop);
    }
    // 「导出所选」「取消全选」还没有落点：本工作台只有单帧选中（`selected: usize`），
    // 没有多选集合，也没有导出所选帧的入口，所以置灰而不是假装能点。
    //
    // "Export selected" and "clear selection" have no target yet: this bench has a single
    // `selected: usize` and no export-selected entry point, so both stay dimmed rather
    // than pretending to work.
    let _ = bar_button(ui, &p, r.right() + 2.0, rect, "导出所选", false);
    let cw = p
        .layout_no_wrap("取消全选".to_owned(), font(10.0), dim())
        .size()
        .x
        + 14.0;
    let _ = bar_button(ui, &p, rect.right() - 10.0 - cw, rect, "取消全选", false);
}

/// 选择条上的文字按钮（高 22，圆角 4）。返回矩形与响应。
///
/// A text button on the selection bar (22 high, 4 px radius).
fn bar_button(
    ui: &mut Ui,
    p: &egui::Painter,
    x: f32,
    bar: Rect,
    label: &str,
    enabled: bool,
) -> (Rect, egui::Response) {
    let fg = if enabled { muted() } else { dim() };
    let g = p.layout_no_wrap(label.to_owned(), font(10.0), fg);
    let w = g.size().x + 14.0;
    let r = Rect::from_min_size(Pos2::new(x, bar.center().y - 11.0), Vec2::new(w, 22.0));
    let resp = ui.interact(
        r,
        ui.make_persistent_id(("seq-selbar", label)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if enabled && resp.hovered() {
        p.rect_filled(r, radius_chip(), elev());
    }
    p.galley(
        Pos2::new(
            r.center().x - g.size().x * 0.5,
            r.center().y - g.size().y * 0.5,
        ),
        g,
        fg,
    );
    (r, resp)
}

/// EXIF 的 `2026:04:25 10:50:23` 写成设计稿的 `2026-04-25 10:50:23`。
///
/// Rewrite EXIF's colon-separated date into the design's dashed form.
fn datetime_text(s: &str) -> String {
    let mut cs: Vec<char> = s.chars().collect();
    if cs.len() > 7 && cs.get(4) == Some(&':') && cs.get(7) == Some(&':') {
        cs[4] = '-';
        cs[7] = '-';
    }
    cs.into_iter().collect()
}

/// 千分位（与左列同一口径）。
///
/// Thousands separator, shared with the left column.
fn thousands(n: usize) -> String {
    super::preview::thousands(n)
}
