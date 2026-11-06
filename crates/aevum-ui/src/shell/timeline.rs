//! 底部两行：时间线、曲线、色标、胶片条与走带控制。
//!
//! The two bottom rows: timeline, curve, colour scale, filmstrip and transport
//! controls.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::MICRO;
use super::UV;
use super::mask::DeflCurveCache;
use super::menu::menu_style;
use crate::icons::{self, Glyph};
use crate::state::AppState;
use crate::theme::{self, *};
use egui::{
    Align, Align2, Color32, Context, CursorIcon, Layout, Pos2, Rect, RichText, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, Vec2,
};
use std::sync::Arc;
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

// ---------------------------------------------------------------- 时间线

pub(super) fn timeline(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    // 头部行：整条 26px header 底 + 下缘发丝线，文字 11px dim —— 对齐界面稿 .tl-hd。
    let hd = Rect::from_min_size(full.min, Vec2::new(full.width(), TL_HD_H));
    p.rect_filled(hd, 0, header());
    theme::rule(&p, hd.left(), hd.right(), hd.bottom(), divider());

    let cy = hd.center().y;
    let n = state.frame_count();
    let title_text = if n > 0 {
        crate::i18n::tr_fmt("胶片条 · {} 帧 · 已评级 {}", &[&n, &state.rated_count()])
    } else {
        crate::i18n::tr("胶片条 · 无序列").to_owned()
    };
    // 标题先量宽：收起按钮要钉在它右边，跟着文字长短走。
    //
    // Measure the title first: the collapse button is pinned right after it and follows
    // whatever width the text needs.
    let title_g = p.layout_no_wrap(title_text.clone(), font(SIZE_SMALL), dim());
    p.text(
        Pos2::new(hd.left() + 10.0, cy),
        Align2::LEFT_CENTER,
        title_text,
        font(SIZE_SMALL),
        dim(),
    );

    // 收起入口做成**真按钮**，紧跟标题钉在头部左端：112 × 24，有填充底色与描边，
    // Chevron 与文案随 show_film 切换，静置走 text()、悬停抬起。
    //
    // 之前它是右端一枚「降为 ghost」的淡灰小字（透明底 + dim + 悬停才抬起），
    // 用户三轮反馈「这里应该有个按钮直接把预览图收起」——功能在、但看不见，
    // 等于没有。现在按控制件规范做成一眼能认、能点的按钮。
    //
    // The collapse entry is a **real button** pinned to the left of the header right after
    // the title: 112 x 24, filled and stroked, with the chevron and label driven by
    // show_film, text() at rest and lifting on hover.
    //
    // It used to be a dim ghost chip at the far right (transparent, dim text, only lifting
    // on hover). The user asked three times for "a button that collapses the preview" --
    // the feature existed but was invisible, which is the same as not having it.
    let curve_w = 40.0;
    let btn_w = 112.0;
    let btn_h = 24.0;
    // 右边界保护：序号到四位、或窗口变窄时，按钮不许压到右端的「曲线」开关。
    //
    // Right-edge guard: with a four-digit count or a narrow window the button must not
    // run into the "Curve" toggle at the right end.
    let max_left = hd.right() - 10.0 - curve_w - 8.0 - btn_w;
    let btn_left = (hd.left() + 10.0 + title_g.size().x + 12.0).min(max_left);
    let film_rect = Rect::from_min_size(
        Pos2::new(btn_left, cy - btn_h * 0.5),
        Vec2::new(btn_w, btn_h),
    );
    let film_resp = ui.interact(film_rect, ui.make_persistent_id("tl-film"), Sense::click());
    p.rect_filled(
        film_rect,
        radius_control(),
        if film_resp.hovered() {
            elev_hover()
        } else {
            elev()
        },
    );
    // 描边默认 border_strong、悬停才用 accent：设计文档 §5.2 要求强调色占比 ≤ 2%，
    // 常驻的 accent 描边会和「当前选中」抢语义。
    //
    // Stroke is border_strong by default and accent on hover: the design spec caps the
    // accent at 2% of pixels, and a permanent accent outline would compete with "selected".
    p.rect_stroke(
        film_rect,
        radius_control(),
        Stroke::new(
            1.0,
            if film_resp.hovered() {
                accent()
            } else {
                border_strong()
            },
        ),
        StrokeKind::Inside,
    );
    icons::paint(
        &p,
        Rect::from_center_size(Pos2::new(film_rect.left() + 16.0, cy), Vec2::splat(13.0)),
        if state.show_film {
            Glyph::ChevronDown
        } else {
            Glyph::ChevronRight
        },
        text(),
    );
    p.text(
        Pos2::new(film_rect.left() + 28.0, cy),
        Align2::LEFT_CENTER,
        if state.show_film {
            crate::i18n::t!("收起预览")
        } else {
            crate::i18n::t!("展开预览")
        },
        font(SIZE_SMALL),
        text(),
    );
    if film_resp.clicked() {
        state.show_film = !state.show_film;
    }
    film_resp.on_hover_text(crate::i18n::t!("收起 / 展开底部预览栏"));

    // 缩略图与主图曝光不同的说明：缩略图走机身内嵌原片（未显影），主画布已显影，
    // 两者亮度天然有差。写在这里是为了止住「是不是有 bug」的疑问，而不是改渲染链。
    //
    // Note on thumbnail exposure: thumbnails use the camera-embedded original (undeveloped)
    // while the canvas shows the developed preview, so their brightness differs by design.
    // This explains it instead of changing the render path.
    let note_text = crate::i18n::t!("缩略图为未显影原片，与主画布曝光不同");
    let note_left = film_rect.right() + 12.0;
    if note_left < hd.right() - curve_w - 16.0 {
        p.text(
            Pos2::new(note_left, cy),
            Align2::LEFT_CENTER,
            note_text,
            font(SIZE_SMALL),
            muted(),
        );
    }

    // 星级筛选：≥N 星才显示，N=0 表示不过滤。只在显示层收窄，不动真实帧序。
    //
    // Star filter: show frames rated at least N; N = 0 means no filter. It narrows the
    // display only and never reorders the real sequence.
    let right_cursor = hd.right() - 10.0 - curve_w - 10.0;
    {
        let clear_text = crate::i18n::t!("清除筛选");
        let label_text = crate::i18n::t!("星级筛选");
        let count_text = crate::i18n::tr_fmt(
            "已筛选 {} / {} 帧",
            &[
                &state.seq.as_ref().map_or(0usize, |q| {
                    (0..q.len())
                        .filter(|i| {
                            state.film_filter_stars == 0
                                || state.rating_at(*i) >= state.film_filter_stars
                        })
                        .count()
                }),
                &n,
            ],
        );
        let clear_g = p.layout_no_wrap(clear_text.to_owned(), font(SIZE_SMALL), muted());
        let count_g = p.layout_no_wrap(count_text.clone(), mono(SIZE_SMALL), dim());
        let label_g = p.layout_no_wrap(label_text.to_owned(), font(SIZE_SMALL), muted());
        let star_w = 5.0 * 15.0;
        let group_w =
            label_g.size().x + 8.0 + star_w + 8.0 + clear_g.size().x + 8.0 + count_g.size().x;
        let group_left = right_cursor - group_w;
        // 窄窗整组收起：宁可看不见筛选器，也不让它压住标题与说明。
        //
        // Collapse the whole group in a narrow window: better invisible than overlapping
        // the title and the note.
        if group_left > film_rect.right() + 220.0 {
            p.text(
                Pos2::new(group_left, cy),
                Align2::LEFT_CENTER,
                label_text,
                font(SIZE_SMALL),
                muted(),
            );
            let mut sx = group_left + label_g.size().x + 8.0;
            for level in 1..=5u8 {
                let r = Rect::from_center_size(Pos2::new(sx + 7.0, cy), Vec2::splat(15.0));
                let resp = ui.interact(
                    r,
                    ui.make_persistent_id(("film-star-filter", level)),
                    Sense::click(),
                );
                let on = state.film_filter_stars >= level;
                icons::paint(
                    &p,
                    r.shrink(2.0),
                    if on { Glyph::Star } else { Glyph::StarEmpty },
                    if on {
                        star()
                    } else if resp.hovered() {
                        text()
                    } else {
                        muted()
                    },
                );
                if resp.clicked() {
                    state.film_filter_stars = if state.film_filter_stars == level {
                        0
                    } else {
                        level
                    };
                }
                sx += 15.0;
            }
            let clear_rect = Rect::from_min_size(
                Pos2::new(sx + 8.0, cy - 9.0),
                Vec2::new(clear_g.size().x + 8.0, 18.0),
            );
            let clear_resp = ui.interact(
                clear_rect,
                ui.make_persistent_id("film-star-clear"),
                Sense::click(),
            );
            p.text(
                Pos2::new(clear_rect.left() + 4.0, cy),
                Align2::LEFT_CENTER,
                clear_text,
                font(SIZE_SMALL),
                if clear_resp.hovered() {
                    text()
                } else {
                    muted()
                },
            );
            if clear_resp.clicked() {
                state.film_filter_stars = 0;
            }
            p.text(
                Pos2::new(clear_rect.right() + 8.0, cy),
                Align2::LEFT_CENTER,
                count_text,
                mono(SIZE_SMALL),
                dim(),
            );
        }
    }

    // 右端「曲线」开关：锚在头部右缘，不随左端的收起按钮移动。
    //
    // The right-hand "Curve" toggle: anchored to the header's right edge, it does not
    // follow the collapse button to the left.
    let curve_rect = Rect::from_min_size(
        Pos2::new(hd.right() - 10.0 - curve_w, cy - 9.0),
        Vec2::new(curve_w, 18.0),
    );
    let curve_resp = ui.interact(
        curve_rect,
        ui.make_persistent_id("tl-curve"),
        Sense::click(),
    );
    if curve_resp.hovered() {
        p.rect_filled(curve_rect, radius_control(), elev());
    }
    p.text(
        curve_rect.center(),
        Align2::CENTER_CENTER,
        crate::i18n::tr("曲线"),
        font(SIZE_SMALL),
        if state.show_curves { text() } else { dim() },
    );
    if curve_resp.clicked() {
        state.show_curves = !state.show_curves;
    }

    let mut y = hd.bottom() + 4.0;
    let pad = 8.0;
    if state.show_curves {
        // 波形是检视工具，打开时宁可挤缩略图也不换行：一开就跳布局最难受。
        let h = 54.0;
        curves(
            state,
            ui,
            Rect::from_min_size(
                Pos2::new(full.left() + pad, y),
                Vec2::new(full.width() - pad * 2.0, h),
            ),
        );
        y += h + 6.0;
    }
    if state.show_film {
        // 高度优先级：用户拖过的高度 > 按首帧比例自动适配 > 兜底 56。
        // 之前一股脑撑满剩余空间：横图时条比图高出两三倍，下面一截空皮。
        let avail_h = (full.bottom() - y - 36.0).max(56.0);
        let auto_h = state
            .thumb(state.selected)
            .map(|t| {
                let (tw, th) = (t.size[0] as f32, t.size[1] as f32);
                (92.0 * th / tw.max(1.0) + 18.0).clamp(64.0, 220.0)
            })
            .unwrap_or(84.0);
        let h = state.film_h.unwrap_or(auto_h).clamp(48.0, avail_h);
        // 上边缘 5px 的拖动条：上下拉自定义高度，双击回到自动适配。
        let grip = Rect::from_min_max(
            Pos2::new(full.left() + pad, y - 2.0),
            Pos2::new(full.right() - pad, y + 3.0),
        );
        let gresp = ui.interact(
            grip,
            ui.make_persistent_id("film-h"),
            Sense::click_and_drag(),
        );
        if gresp.dragged()
            && let Some(pt) = gresp.interact_pointer_pos()
        {
            state.film_h = Some((pt.y - y + h).clamp(48.0, avail_h));
        }
        if gresp.double_clicked() {
            state.film_h = None;
        }
        if gresp.hovered() || gresp.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            p.rect_filled(grip, 2.0, accent().gamma_multiply(0.55));
        } else {
            p.rect_filled(grip, 2.0, divider());
        }
        filmstrip(
            state,
            ui,
            ctx,
            Rect::from_min_size(
                Pos2::new(full.left() + pad, y),
                Vec2::new(full.width() - pad * 2.0, h),
            ),
        );
        y += h + 4.0;
    }
    transport(
        state,
        ui,
        Rect::from_min_size(
            Pos2::new(full.left() + pad, y),
            Vec2::new(
                full.width() - pad * 2.0,
                (full.bottom() - y - 4.0).max(24.0),
            ),
        ),
    );
}

fn curves(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    let resp = ui.interact(
        rect,
        ui.make_persistent_id("curves"),
        Sense::click_and_drag(),
    );
    let p = ui.painter();
    p.rect_filled(rect, 0, canvas());
    p.rect_stroke(rect, 0, Stroke::new(1.0, divider()), StrokeKind::Inside);

    let n = state.frame_count();
    if n == 0 {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "打开序列后这里显示逐帧亮度与去闪修正",
            font(SIZE_SMALL),
            dim(),
        );
        return;
    }

    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, rect.top() + 5.0),
        Pos2::new(rect.right() - 36.0, rect.bottom() - 13.0),
    );
    for k in 1..4 {
        let y = inner.top() + inner.height() * k as f32 / 4.0;
        p.line_segment(
            [Pos2::new(inner.left(), y), Pos2::new(inner.right(), y)],
            Stroke::new(1.0, divider()),
        );
    }

    // 两条折线的点集走缓存（E5）：键 = (corr_version, 帧数, inner rect)。
    // 去闪参数或测光变了 corr_version 必变；静止帧直接复用点集。
    let has_any = state.metrics.iter().any(|m| m.is_some());
    if !has_any {
        p.text(
            inner.center(),
            Align2::CENTER_CENTER,
            "测光中…",
            font(SIZE_SMALL),
            accent(),
        );
        return;
    }
    let cached = match &state.defl_curve_cache {
        Some(c) if c.version == state.corr_version && c.n == n && c.rect == inner => {
            Some((c.pts_raw.clone(), c.pts_fix.clone(), c.lo, c.hi))
        }
        _ => None,
    };
    let (pts_raw, pts_fix, lo, hi) = match cached {
        Some(hit) => hit,
        None => {
            let raw: Vec<(usize, f64)> = state
                .metrics
                .iter()
                .enumerate()
                .filter_map(|(i, m)| m.map(|m| (i, m.mean * 255.0)))
                .collect();
            let corrected: Vec<(usize, f64)> = raw
                .iter()
                .map(|(i, v)| (i, v, state.offset_at(*i) as f64))
                .map(|(i, v, off)| (*i, v * 2f64.powf(off)))
                .collect();
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for (_, v) in raw.iter().chain(corrected.iter()) {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
            let pad_v = ((hi - lo) * 0.12).max(1.0);
            let (lo, hi) = (lo - pad_v, hi + pad_v);
            let x_at = |i: usize| {
                inner.left() + inner.width() * i as f32 / n.max(2).saturating_sub(1) as f32
            };
            let y_at =
                |v: f64| inner.bottom() - inner.height() * ((v - lo) / (hi - lo).max(1e-6)) as f32;

            let stride = (raw.len() / 800).max(1);
            let mut pts_raw = Vec::new();
            let mut pts_fix = Vec::new();
            for (k, (i, v)) in raw.iter().enumerate() {
                if k % stride == 0 || k + 1 == raw.len() {
                    pts_raw.push(Pos2::new(x_at(*i), y_at(*v)));
                }
            }
            for (k, (i, v)) in corrected.iter().enumerate() {
                if k % stride == 0 || k + 1 == corrected.len() {
                    pts_fix.push(Pos2::new(x_at(*i), y_at(*v)));
                }
            }
            let pts_raw: Arc<[Pos2]> = pts_raw.into_iter().collect();
            let pts_fix: Arc<[Pos2]> = pts_fix.into_iter().collect();
            state.defl_curve_cache = Some(DeflCurveCache {
                version: state.corr_version,
                n,
                rect: inner,
                pts_raw: Arc::clone(&pts_raw),
                pts_fix: Arc::clone(&pts_fix),
                lo,
                hi,
            });
            (pts_raw, pts_fix, lo, hi)
        }
    };
    let x_at =
        |i: usize| inner.left() + inner.width() * i as f32 / n.max(2).saturating_sub(1) as f32;
    if pts_raw.len() > 1 {
        p.add(egui::Shape::line(
            pts_raw.to_vec(),
            Stroke::new(1.0, Color32::from_rgb(0x7A, 0x7A, 0x7A)),
        ));
        p.add(egui::Shape::line(
            pts_fix.to_vec(),
            Stroke::new(1.2, accent()),
        ));
    }

    for i in state.keyframes.keys() {
        let c = Pos2::new(x_at(*i), inner.top() + 4.0);
        let d = 3.2;
        p.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(c.x, c.y - d),
                Pos2::new(c.x + d, c.y),
                Pos2::new(c.x, c.y + d),
                Pos2::new(c.x - d, c.y),
            ],
            star(),
            Stroke::NONE,
        ));
    }

    let px = x_at(state.selected);
    p.line_segment(
        [Pos2::new(px, inner.top()), Pos2::new(px, inner.bottom())],
        Stroke::new(1.0, text()),
    );

    p.text(
        Pos2::new(inner.right() + 4.0, inner.top()),
        Align2::LEFT_TOP,
        format!("{:.0}", hi),
        mono(MICRO),
        dim(),
    );
    p.text(
        Pos2::new(inner.right() + 4.0, inner.bottom()),
        Align2::LEFT_BOTTOM,
        format!("{:.0}", lo),
        mono(MICRO),
        dim(),
    );

    if let Some(pos) = resp.hover_pos().filter(|pos| inner.contains(*pos)) {
        let i = (((pos.x - inner.left()) / inner.width()) * (n.saturating_sub(1)) as f32)
            .round()
            .clamp(0.0, n.saturating_sub(1) as f32) as usize;
        let x = x_at(i);
        p.line_segment(
            [Pos2::new(x, inner.top()), Pos2::new(x, inner.bottom())],
            Stroke::new(1.0, Color32::from_white_alpha(70)),
        );
        let m = state.metrics.get(i).copied().flatten();
        let off = state.offset_at(i);
        let text = if let Some(m) = m {
            crate::i18n::tr_fmt(
                "帧 {} · {} · {} EV",
                &[
                    &format!("{:04}", i + 1),
                    &format!("{:.0}", m.mean * 255.0),
                    &format!("{off:+.2}"),
                ],
            )
        } else {
            crate::i18n::tr_fmt("帧 {} · 缺帧", &[&format!("{:04}", i + 1)])
        };
        let galley = p.layout_no_wrap(text.clone(), mono(SIZE_SMALL), palette().text);
        let bg = Rect::from_min_size(
            Pos2::new(inner.left() + 2.0, rect.top() + 2.0),
            galley.size() + Vec2::new(10.0, 4.0),
        );
        p.rect_filled(bg, radius_control(), Color32::from_black_alpha(170));
        p.galley(bg.min + Vec2::new(5.0, 2.0), galley, palette().text);
        if resp.clicked() || resp.dragged() {
            state.set_frame(i);
        }
    }
}

pub(super) fn label_color(label: u8) -> Color32 {
    match label {
        1 => Color32::from_rgb(0xD9, 0x53, 0x4F),
        2 => Color32::from_rgb(0xD9, 0xBE, 0x6E),
        3 => Color32::from_rgb(0x7C, 0xB3, 0x42),
        4 => Color32::from_rgb(0x4A, 0x90, 0xD9),
        _ => Color32::TRANSPARENT,
    }
}

fn filmstrip(state: &mut AppState, ui: &mut Ui, ctx: &Context, rect: Rect) {
    let n = state.frame_count();
    let cell_w = 92.0;
    let h = rect.height();
    let mut visible = 0..0;

    ui.scope_builder(
        UiBuilder::new()
            .id_salt("film-scroll")
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Min)),
        |ui| {
            ScrollArea::horizontal()
                .id_salt("film")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let (strip, _) = ui.allocate_exact_size(
                        Vec2::new(cell_w * n.max(1) as f32, h),
                        Sense::hover(),
                    );
                    let clip = ui.clip_rect();
                    let lo =
                        (((clip.left() - strip.left()) / cell_w).floor() as isize).max(0) as usize;
                    let hi =
                        ((((clip.right() - strip.left()) / cell_w).ceil() as usize) + 1).min(n);
                    visible = lo..hi.max(lo);

                    let p = ui.painter();
                    for i in lo..hi {
                        // 星级筛选只决定「画不画」，不改真实帧序：i 仍是序列里的真帧号，
                        // 所以播放范围、导出范围、缩略图预热都还是原来的语义。
                        //
                        // The star filter only decides whether a cell is drawn; `i` stays the
                        // real sequence index, so playback, export and thumbnail warm-up keep
                        // their original meaning.
                        if state.film_filter_stars > 0
                            && state.rating_at(i) < state.film_filter_stars
                        {
                            continue;
                        }
                        let cell = Rect::from_min_size(
                            Pos2::new(strip.left() + i as f32 * cell_w + 1.0, strip.top()),
                            Vec2::new(cell_w - 2.0, h),
                        );
                        // 单元对齐界面稿 .th：gap 2、圆角 3、canvas 底、
                        // 外圈 1px 白 5% 的极淡环，没有内衬黑卡。
                        p.rect_filled(cell, 3.0, canvas());
                        p.rect_stroke(
                            cell,
                            3.0,
                            Stroke::new(1.0, Color32::from_white_alpha(13)),
                            StrokeKind::Inside,
                        );
                        let inner = Rect::from_min_max(
                            cell.min + Vec2::new(3.0, 3.0),
                            cell.max - Vec2::new(3.0, 15.0),
                        );
                        if let Some(t) = state.thumb(i) {
                            let (tw, th) = (t.size[0] as f32, t.size[1] as f32);
                            let scale = (inner.width() / tw).min(inner.height() / th);
                            let size = Vec2::new(tw * scale, th * scale);
                            let img = Rect::from_center_size(inner.center(), size);
                            p.image(t.texture.id(), img, UV, Color32::WHITE);
                        } else {
                            icons::paint(
                                p,
                                Rect::from_center_size(inner.center(), Vec2::splat(16.0)),
                                Glyph::Sequence,
                                dim().gamma_multiply(0.7),
                            );
                        }
                        // 「原片」角标（评审 E2）：缩略图恒为机身内嵌未显影原片，
                        // 与主画布的显影效果不同口径——不标出来，用户会以为参数
                        // 没生效或被偷偷丢掉。半透明黑底压右上角，随 cell 可见。
                        //
                        // The "original" chip (review E2): a thumbnail is always the
                        // in-camera undeveloped original, a different convention from the
                        // developed canvas -- unmarked, it reads as "my edits vanished".
                        // Translucent black chip pinned top-right, visible with the cell.
                        let chip_g = p.layout_no_wrap(
                            crate::i18n::t!("原片").to_owned(),
                            font(SIZE_SMALL),
                            text(),
                        );
                        let chip_rect = Rect::from_min_size(
                            Pos2::new(cell.right() - 7.0 - chip_g.size().x, cell.top() + 3.0),
                            Vec2::new(chip_g.size().x + 4.0, chip_g.size().y + 1.0),
                        );
                        p.rect_filled(chip_rect, 2.0, Color32::from_black_alpha(160));
                        p.galley(chip_rect.left_top() + Vec2::new(2.0, 0.5), chip_g, text());
                        let label = state.label_at(i);
                        if label > 0 {
                            p.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(cell.left() + 3.0, cell.bottom() - 16.0),
                                    Vec2::new(cell.width() - 6.0, 2.0),
                                ),
                                0,
                                label_color(label),
                            );
                        }
                        let rating = state.rating_at(i);
                        // 星级右对齐、序号左对齐：两者同在底缘带里，早先都从左起画，
                        // 星级（bottom-14..-7）压着序号字顶（MICRO 10px，顶到
                        // bottom-11）——每格都叠。水平分道后互不相交。
                        //
                        // Stars hug the right edge, the number the left: both live in the
                        // bottom band, and painting both from the left made the star row
                        // (bottom-14..-7) overlap the number's cap height (MICRO, up to
                        // bottom-11) in every cell. Splitting horizontally fixes it.
                        for s in 0..5u8 {
                            let sr = Rect::from_min_size(
                                Pos2::new(
                                    cell.right() - 3.0 - (5 - s) as f32 * 8.0,
                                    cell.bottom() - 14.0,
                                ),
                                Vec2::splat(7.0),
                            );
                            icons::paint(
                                p,
                                sr,
                                if s < rating {
                                    Glyph::Star
                                } else {
                                    Glyph::StarEmpty
                                },
                                if s < rating {
                                    // 星级升作主信息（B4）：48 格 × 5 枚黄点曾是整条
                                    // 胶片条最吵的一层彩色。改用正文白后，「评级」与
                                    // 「未评级（dim 空星）」的对比反而更清楚，彩色只
                                    // 留给色标与旗标。
                                    //
                                    // Stars become the primary readout (B4): 48 cells x 5
                                    // yellow fills were the noisiest colour band on the
                                    // strip. Body white keeps "rated vs not" (dim hollow
                                    // stars) legible and leaves colour to flags and labels.
                                    if i == state.selected {
                                        on_selected()
                                    } else {
                                        text()
                                    }
                                } else {
                                    dim()
                                },
                            );
                        }
                        match state.flag_at(i) {
                            1 => icons::paint(
                                p,
                                Rect::from_min_size(
                                    Pos2::new(cell.left() + 4.0, cell.top() + 4.0),
                                    Vec2::splat(10.0),
                                ),
                                Glyph::Flag,
                                ok(),
                            ),
                            -1 => icons::paint(
                                p,
                                Rect::from_min_size(
                                    Pos2::new(cell.left() + 4.0, cell.top() + 4.0),
                                    Vec2::splat(10.0),
                                ),
                                Glyph::Close,
                                danger(),
                            ),
                            _ => {}
                        }
                        // 序号：左下角，dim 配黑投影（B4 让位给星级）；
                        // 先画偏移 1px 的黑影再画字，缩略图再亮也压得住。
                        //
                        // The frame number sits bottom-left in dim (B4 hands the
                        // primary readout to the stars); a 1px offset shadow keeps it
                        // legible over bright thumbnails.
                        let num = format!("{:04}", i + 1);
                        p.text(
                            Pos2::new(cell.left() + 4.0, cell.bottom() - 1.0),
                            Align2::LEFT_BOTTOM,
                            num.clone(),
                            font(MICRO),
                            Color32::from_black_alpha(120),
                        );
                        p.text(
                            Pos2::new(cell.left() + 3.0, cell.bottom() - 2.0),
                            Align2::LEFT_BOTTOM,
                            num,
                            font(MICRO),
                            if i == state.selected {
                                on_selected()
                            } else {
                                dim()
                            },
                        );

                        let cell_resp = ui.interact(
                            cell,
                            ui.make_persistent_id(("film-cell", i)),
                            Sense::click(),
                        );
                        if i == state.selected {
                            // 选中 = 1px 强调色外框 + 内圈 surround 压边（设计稿 B4）：
                            // 2px 外框在 48 格上一排读作「一圈蓝条」，1px 描边配一圈
                            // 压暗既清楚又不抢；绿色留给旗标，蓝色才是"你在哪"。
                            //
                            // Selected = 1px accent outline plus an inner surround ring
                            // (design spec B4): a 2px outline read as a blue band across
                            // the whole strip; 1px with an inner darkening stays legible
                            // without shouting. Green is for flags, blue is "where you are".
                            p.rect_stroke(
                                cell.shrink(1.0),
                                3.0,
                                Stroke::new(1.0, accent()),
                                StrokeKind::Inside,
                            );
                            p.rect_stroke(
                                cell.shrink(2.0),
                                2.0,
                                Stroke::new(1.0, surround()),
                                StrokeKind::Inside,
                            );
                        } else if cell_resp.hovered() {
                            p.rect_stroke(
                                cell,
                                0,
                                Stroke::new(1.0, border_strong()),
                                StrokeKind::Inside,
                            );
                        }
                        if cell_resp.clicked() {
                            state.set_frame(i);
                        }
                        cell_resp.context_menu(|ui| {
                            menu_style(ui);
                            ui.label(
                                RichText::new(crate::i18n::tr_fmt(
                                    "帧 {}",
                                    &[&format!("{:04}", i + 1)],
                                ))
                                .size(SIZE_SMALL)
                                .color(dim()),
                            );
                            for stars in 1..=5u8 {
                                if ui.button(crate::i18n::tr_fmt("{} 星", &[&stars])).clicked() {
                                    state.set_rating(i, stars);
                                    ui.close();
                                }
                            }
                            if ui.button(crate::i18n::t!("清除星级")).clicked() {
                                state.set_rating(i, 0);
                                ui.close();
                            }
                            ui.separator();
                            if ui.button(crate::i18n::t!("标记为选用")).clicked() {
                                state.set_flag(i, 1);
                                ui.close();
                            }
                            if ui.button(crate::i18n::t!("标记为排除")).clicked() {
                                state.set_flag(i, -1);
                                ui.close();
                            }
                            ui.separator();
                            for (label, name) in
                                [(1u8, "红标"), (2, "黄标"), (3, "绿标"), (4, "蓝标")]
                            {
                                if ui.button(name).clicked() {
                                    state.set_label(i, label);
                                    ui.close();
                                }
                            }
                            if ui.button(crate::i18n::t!("清除色标")).clicked() {
                                state.set_label(i, 0);
                                ui.close();
                            }
                        });
                    }

                    if state.scroll_to_selected {
                        let x = strip.left() + state.selected as f32 * cell_w;
                        ui.scroll_to_rect(
                            Rect::from_min_size(Pos2::new(x, strip.top()), Vec2::new(cell_w, h)),
                            Some(Align::Center),
                        );
                        state.scroll_to_selected = false;
                    }
                });
        },
    );

    state.ensure_thumbs(ctx, visible.start.saturating_sub(2), visible.end + 2);
}

fn transport(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    p.rect_filled(rect, 0, header());
    p.rect_stroke(rect, 0, Stroke::new(1.0, border()), StrokeKind::Inside);
    let cy = rect.center().y;
    let mut idx = 0usize;
    // `enabled` 为假时只挂 hover 感：不可点、图标压到 dim、并说明为什么。
    // 播放按钮在单帧序列上就是这种状态——点了画面不动，用户会以为坏了。
    //
    // With `enabled` false the control only senses hover: not clickable, icon dimmed, and
    // a hover hint explains why. That is the play button on a one-frame sequence -- pressing
    // it used to do nothing and read as a bug.
    let mut mini =
        |ui: &mut Ui, glyph: Glyph, active: bool, tint: Color32, enabled: bool| -> bool {
            let r = Rect::from_center_size(
                Pos2::new(rect.left() + 16.0 + idx as f32 * 26.0, cy),
                Vec2::splat(22.0),
            );
            let resp = ui.interact(
                r,
                ui.make_persistent_id(("transport", idx)),
                if enabled {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            idx += 1;
            if enabled && resp.hovered() {
                p.rect_filled(r, radius_control(), elev());
            }
            icons::paint(
                &p,
                r.shrink(5.0),
                glyph,
                if !enabled {
                    dim()
                } else if active {
                    tint
                } else if resp.hovered() {
                    text()
                } else {
                    muted()
                },
            );
            let clicked = resp.clicked();
            if !enabled {
                resp.on_hover_text(crate::i18n::t!("至少需要两帧才能播放"));
            }
            clicked && enabled
        };

    let has = state.frame_count() > 0;
    // 播放至少要两帧：一帧的「播放」既不动、又会把画面切成代理，纯属误导。
    //
    // Playback needs at least two frames: "playing" a single frame neither moves nor means
    // anything, and it used to swap the canvas to a proxy for no reason.
    let can_play = state.frame_count() >= 2;
    if mini(ui, Glyph::Prev, false, text(), has) {
        state.step(-1);
    }
    if mini(
        ui,
        if state.playing {
            Glyph::Pause
        } else {
            Glyph::Play
        },
        state.playing,
        accent(),
        can_play,
    ) {
        state.toggle_play();
    }
    if mini(ui, Glyph::Next, false, text(), has) {
        state.step(1);
    }

    let n = state.frame_count();
    // 左段按实测文本宽依次排：早先是 `+94` / `+190` 两枚魔法数字——中文下刚好，
    // 英文 "Deflicker +0.00 EV" 一出就压到帧号。宽度用 galley 量，两种语言都成立。
    //
    // The left run is laid out by measured text width: fixed offsets of +94/+190 fit
    // Chinese but let English "Deflicker +0.00 EV" collide with the frame counter.
    // Measuring galleys works for both languages.
    let text_x = rect.left() + 16.0 + idx as f32 * 26.0;
    let frame_text = if n > 0 {
        crate::i18n::tr_fmt(
            "{} / {}",
            &[&format!("{:04}", state.selected + 1), &format!("{:04}", n)],
        )
    } else {
        "— / —".to_owned()
    };
    let frame_g = p.layout_no_wrap(
        frame_text,
        mono(SIZE_SMALL),
        if n > 0 { muted() } else { dim() },
    );
    let frame_pos = Pos2::new(text_x + 4.0, cy - frame_g.size().y * 0.5);
    p.galley(frame_pos, frame_g.clone(), muted());

    let off = state.offset_at(state.selected);
    let defl_text = crate::i18n::tr_fmt("去闪 {} EV", &[&format!("{off:+.2}")]);
    let defl_color = if off.abs() > 0.02 { accent() } else { dim() };
    let defl_g = p.layout_no_wrap(defl_text, mono(SIZE_SMALL), defl_color);
    let defl_x = frame_pos.x + frame_g.size().x + 14.0;
    let defl_pos = Pos2::new(defl_x, cy - defl_g.size().y * 0.5);
    p.galley(defl_pos, defl_g.clone(), defl_color);

    let mut x = defl_x + defl_g.size().x + 14.0;
    let rating = state.selected_rating();
    for s in 1..=5u8 {
        let r = Rect::from_center_size(Pos2::new(x, cy), Vec2::splat(15.0));
        let resp = ui.interact(r, ui.make_persistent_id(("rate", s)), Sense::click());
        if resp.hovered() {
            p.rect_filled(r, radius_control(), elev());
        }
        icons::paint(
            &p,
            r.shrink(2.0),
            if s <= rating {
                Glyph::Star
            } else {
                Glyph::StarEmpty
            },
            if s <= rating {
                star()
            } else {
                if resp.hovered() { text() } else { dim() }
            },
        );
        if resp.clicked() {
            state.rate_selected(if rating == s { 0 } else { s });
        }
        x += 17.0;
    }

    x += 10.0;
    let flag = state.selected_flag();
    let flags = [
        (1i8, Glyph::Flag, ok(), "选用 (P)"),
        (-1, Glyph::Close, danger(), "排除 (X)"),
    ];
    for (f, glyph, tint, tip) in flags {
        let r = Rect::from_center_size(Pos2::new(x + 11.0, cy), Vec2::splat(22.0));
        let resp = ui.interact(r, ui.make_persistent_id(("flag", f)), Sense::click());
        if resp.hovered() {
            p.rect_filled(r, radius_control(), elev());
        }
        icons::paint(
            &p,
            r.shrink(5.0),
            glyph,
            if flag == f {
                tint
            } else if resp.hovered() {
                text()
            } else {
                dim()
            },
        );
        if resp.clicked() {
            state.set_selected_flag(if flag == f { 0 } else { f });
        }
        resp.on_hover_text(tip);
        x += 24.0;
    }

    x += 8.0;
    let label = state.selected_label();
    for l in 1..=4u8 {
        let r = Rect::from_center_size(Pos2::new(x + 7.0, cy), Vec2::splat(16.0));
        let resp = ui.interact(r, ui.make_persistent_id(("label", l)), Sense::click());
        if resp.hovered() || label == l {
            p.rect_filled(r, radius_control(), elev());
        }
        p.circle_filled(r.center(), 4.0, label_color(l));
        if label == l {
            p.circle_stroke(r.center(), 6.5, Stroke::new(1.0, text()));
        }
        if resp.clicked() {
            state.set_selected_label(if label == l { 0 } else { l });
        }
        x += 18.0;
    }

    // 右端两格从右往左排，空间不足逐格让位：窄窗下「已评级 N」「30 fps」原是
    // `right-62` / `right-8` 两枚固定偏移，会压到色标圆点。先保 fps（数值常看），
    // 再保已评级；再不够就都让位——它们是读数，不是入口。
    //
    // The two right-side readouts chain from the right edge and yield one by one when
    // space runs out: the old fixed offsets (right-62 / right-8) collided with the
    // colour-label dots in narrow windows. fps yields last (it is watched live);
    // both are readouts, never entrances, so dropping them is safe.
    let fps_text = format!("{:.0} fps", state.output_fps);
    let fps_g = p.layout_no_wrap(fps_text, mono(SIZE_SMALL), muted());
    let rated_text = crate::i18n::tr_fmt("已评级 {}", &[&state.rated_count()]);
    let rated_g = p.layout_no_wrap(rated_text, font(SIZE_SMALL), dim());
    let mut rx = rect.right() - 8.0;
    let fps_left = rx - fps_g.size().x;
    if fps_left > x + 8.0 {
        let fps_pos = Pos2::new(fps_left, cy - fps_g.size().y * 0.5);
        p.galley(fps_pos, fps_g, muted());
        rx = fps_left - 12.0;
        let rated_left = rx - rated_g.size().x;
        if rated_left > x + 8.0 {
            p.galley(
                Pos2::new(rated_left, cy - rated_g.size().y * 0.5),
                rated_g,
                dim(),
            );
        }
    }
}
