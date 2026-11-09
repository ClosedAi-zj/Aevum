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
    // 收起/展开按钮（用户 2026-11-07 拍板）：**只此一个功能**、头行水平居中、
    // 22px 圆形图标钮、不写字——名字进悬停提示。静置是低调的灰圆（不与标题/星级
    // 抢注意），悬停抬起并亮起，tooltip 说明收起/展开。
    //
    // The collapse control (user decision): one function, horizontally centred on the
    // header, a 22 px round icon button with no text -- the name lives in the tooltip.
    // At rest it is a quiet grey disc; hover lifts and lights it.
    let btn = 22.0;
    let film_rect = Rect::from_center_size(Pos2::new(full.center().x, cy), Vec2::splat(btn));
    let film_resp = ui.interact(film_rect, ui.make_persistent_id("tl-film"), Sense::click());
    let hovered = film_resp.hovered();
    if hovered {
        p.rect_filled(film_rect, btn * 0.5, elev_hover());
        p.rect_stroke(
            film_rect,
            btn * 0.5,
            Stroke::new(1.0, border_strong()),
            StrokeKind::Inside,
        );
    } else {
        p.circle_filled(film_rect.center(), btn * 0.5, panel().gamma_multiply(1.35));
    }
    icons::paint(
        &p,
        Rect::from_center_size(film_rect.center(), Vec2::splat(13.0)),
        if state.show_film {
            Glyph::ChevronDown
        } else {
            Glyph::ChevronRight
        },
        if hovered { text() } else { muted() },
    );
    if film_resp.clicked() {
        state.show_film = !state.show_film;
    }
    film_resp.on_hover_text(crate::i18n::t!("收起 / 展开底部预览栏"));
    // 右端「曲线」开关的宽度常量（星级筛选/说明行的右边界都要用它）。
    let curve_w = 40.0;

    // 缩略图与主画布同一口径（用户 2026-11-07：「预览图应该跟主图一致」）：胶片条
    // 的每一格都由同一份显影参数渲染，所以这里不再需要「缩略图是未显影原片」的
    // 说明字，也不再给每格贴「原片」角标。
    //
    // Thumbnails share the canvas's convention (user, 2026-11-07): every filmstrip cell
    // is developed with the same parameters, so the "undeveloped original" caption and
    // the per-cell "ORIG" chip are gone.

    // 筛选（LR 胶片条筛选栏同构）：星级下限、旗标、色标三组判据并排放在头行
    // 右段，三组同时成立才画那一格。只在显示层收窄，不动真实帧序。
    //
    // Filters (Lightroom's filmstrip filter bar): minimum stars, flag and colour label
    // share the header's right band, and a cell is drawn only when all three hold. They
    // narrow the display only and never reorder the sequence.
    let right_cursor = hd.right() - 10.0 - curve_w - 10.0;
    {
        let clear_text = crate::i18n::t!("清除筛选");
        let label_text = crate::i18n::t!("筛选");
        let count_text =
            crate::i18n::tr_fmt("已筛选 {} / {} 帧", &[&state.film_visible_count(), &n]);
        let clear_g = p.layout_no_wrap(clear_text.to_owned(), font(SIZE_SMALL), muted());
        let count_g = p.layout_no_wrap(count_text.clone(), mono(SIZE_SMALL), dim());
        let label_g = p.layout_no_wrap(label_text.to_owned(), font(SIZE_SMALL), muted());
        const DOT: f32 = 15.0;
        let star_w = 5.0 * DOT;
        let flag_w = 2.0 * DOT;
        let color_w = 4.0 * DOT;
        // 筛选是否真的在生效：决定「清除筛选」与「已筛选 n / N 帧」要不要占位。
        // 没筛选时那句读数和左上角的「12 帧」是同一件事，白占 80px，还把
        // 「清除筛选」淹在一串无关文字里（用户 2026-11-07 的视觉走查）。
        //
        // Whether a filter is actually narrowing the strip; it decides whether the
        // "clear" and "x of N" readouts take any space at all. With no filter the count
        // repeats the "12 frames" on the left, costs 80 px, and drowns "clear filters".
        let filters_on = state.film_filter_stars > 0
            || state.film_filter_flag != 0
            || state.film_filter_label > 0;
        let group_w = label_g.size().x
            + 8.0
            + star_w
            + 6.0
            + flag_w
            + 6.0
            + color_w
            + 8.0
            + clear_g.size().x
            + if filters_on {
                8.0 + count_g.size().x
            } else {
                0.0
            };
        let group_left = right_cursor - group_w;
        // 窄窗整组收起：宁可看不见筛选器，也不让它压住标题与收起按钮。但筛选**正在
        // 生效**时不能一起收掉——那样整条胶片条会莫名其妙地空着，界面上却找不到
        // 原因（用户截图「突然底部没有预览图了」）。这时退成一枚「已筛选 x / y ·
        // 清除筛选」的窄读数，让状态始终可见、可一键恢复。
        //
        // Collapse the whole group in a narrow window: better invisible than overlapping
        // the title and the collapse button. An **active** filter must not vanish with it,
        // or the strip goes inexplicably empty with no cause on screen (user screenshot:
        // "the filmstrip previews suddenly disappeared"). It falls back to a compact
        // "x of y · clear filters" readout so the state stays visible and reversible.
        if group_left > film_rect.right() + 12.0 {
            p.text(
                Pos2::new(group_left, cy),
                Align2::LEFT_CENTER,
                label_text,
                font(SIZE_SMALL),
                muted(),
            );
            let mut sx = group_left + label_g.size().x + 8.0;
            for level in 1..=5u8 {
                let r = Rect::from_center_size(Pos2::new(sx + 7.0, cy), Vec2::splat(DOT));
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
                sx += DOT;
            }
            // 旗标：一枚勾（选用）一枚叉（排除），再点一次取消。勾/叉用线段画，
            // 不赌字体里有没有 ✓ / ✕。
            //
            // Flags: a tick (pick) and a cross (reject); clicking again clears. Both are
            // line segments so no font is trusted to carry ✓ / ✕.
            sx += 6.0;
            for (flag, tip) in [(1i8, "只看选用"), (-1i8, "只看排除")] {
                let r = Rect::from_center_size(Pos2::new(sx + 7.0, cy), Vec2::splat(DOT));
                let resp = ui.interact(
                    r,
                    ui.make_persistent_id(("film-flag-filter", flag)),
                    Sense::click(),
                );
                let on = state.film_filter_flag == flag;
                let tint = if flag > 0 {
                    label_color(3)
                } else {
                    label_color(1)
                };
                if on {
                    p.rect_filled(r, radius_control(), tint.gamma_multiply(0.22));
                }
                let col = if on {
                    tint
                } else if resp.hovered() {
                    text()
                } else {
                    muted()
                };
                let c = r.center();
                if flag > 0 {
                    p.add(egui::Shape::line(
                        vec![
                            Pos2::new(c.x - 3.4, c.y + 0.2),
                            Pos2::new(c.x - 0.8, c.y + 2.8),
                            Pos2::new(c.x + 3.4, c.y - 2.8),
                        ],
                        Stroke::new(1.4, col),
                    ));
                } else {
                    for (a, b) in [
                        (Vec2::new(-3.2, -3.2), Vec2::new(3.2, 3.2)),
                        (Vec2::new(3.2, -3.2), Vec2::new(-3.2, 3.2)),
                    ] {
                        p.line_segment([c + a, c + b], Stroke::new(1.4, col));
                    }
                }
                if resp.clicked() {
                    state.film_filter_flag = if on { 0 } else { flag };
                }
                resp.on_hover_text(crate::i18n::tr(tip));
                sx += DOT;
            }
            // 色标：四颗圆点，选中的加一圈正文色描边（颜色本身不能当选中态，
            // 红点打不打标都是红的）。
            //
            // Colour labels: four dots; the selected one gets a body-colour ring, since
            // the dot's own colour cannot double as the selected state.
            sx += 6.0;
            for k in 1..=4u8 {
                let r = Rect::from_center_size(Pos2::new(sx + 7.0, cy), Vec2::splat(DOT));
                let resp = ui.interact(
                    r,
                    ui.make_persistent_id(("film-label-filter", k)),
                    Sense::click(),
                );
                let on = state.film_filter_label == k;
                p.circle_filled(
                    r.center(),
                    if resp.hovered() { 5.5 } else { 4.5 },
                    label_color(k),
                );
                if on {
                    p.circle_stroke(r.center(), 6.5, Stroke::new(1.0, text()));
                }
                if resp.clicked() {
                    state.film_filter_label = if on { 0 } else { k };
                }
                resp.on_hover_text(crate::i18n::tr_fmt(
                    "只看{}色标",
                    &[&crate::i18n::tr(crate::state::LABEL_NAMES[k as usize])],
                ));
                sx += DOT;
            }
            let clear_rect = Rect::from_min_size(
                Pos2::new(sx + 8.0, cy - 9.0),
                Vec2::new(clear_g.size().x + 8.0, 18.0),
            );
            let clear_resp = ui.interact(
                clear_rect,
                ui.make_persistent_id("film-filter-clear"),
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
                state.film_filter_flag = 0;
                state.film_filter_label = 0;
            }
            if filters_on {
                p.text(
                    Pos2::new(clear_rect.right() + 8.0, cy),
                    Align2::LEFT_CENTER,
                    count_text,
                    mono(SIZE_SMALL),
                    dim(),
                );
            }
        } else if filters_on {
            // 窄窗下的紧凑筛选读数：右对齐，只留「已筛选 x / y」与一枚可点的清除。
            //
            // Compact filter readout for narrow windows: right-aligned, just the
            // "x of y" count and a clickable clear.
            let clear_left = right_cursor - 6.0 - clear_g.size().x;
            let count_left = clear_left - 10.0 - count_g.size().x;
            let clear_rect = Rect::from_min_size(
                Pos2::new(clear_left - 4.0, cy - 9.0),
                Vec2::new(clear_g.size().x + 8.0, 18.0),
            );
            let clear_resp = ui.interact(
                clear_rect,
                ui.make_persistent_id("film-filter-clear-compact"),
                Sense::click(),
            );
            p.text(
                Pos2::new(count_left, cy),
                Align2::LEFT_CENTER,
                count_text,
                mono(SIZE_SMALL),
                dim(),
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
                state.film_filter_flag = 0;
                state.film_filter_label = 0;
            }
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
        // 走带是固定 36 高的一行，胶片条只吃自己需要的高度——以前剩余高度全被
        // 走带吞掉，单帧序列底下会留出半条空带（用户截图：走带里只有两撮字的
        // 大空白）。
        //
        // Height priority: user drag > first-frame aspect > 56 fallback. The transport is
        // a fixed 36 px row and the strip takes only what it needs; the leftover used to
        // be swallowed by the transport, leaving half a band of empty space under a
        // one-frame sequence (user screenshot: two clumps of text in a wide blank row).
        let avail_h = (full.bottom() - y - 40.0).max(56.0);
        let h = state.film_height().clamp(48.0, avail_h);
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
            p.rect_filled(grip, radius_chip(), accent().gamma_multiply(0.55));
        } else {
            p.rect_filled(grip, radius_chip(), divider());
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
            Vec2::new(full.width() - pad * 2.0, 36.0),
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
                    // 虚拟化窗口退化时至少留一格：旧滚动偏移把视口推到内容之外、或窗口
                    // 极窄时，算出来的区间会是空集，整条胶片条就画成一片空白。
                    //
                    // Keep at least one cell when the virtualisation window degenerates: a
                    // stale scroll offset or a very narrow window can yield an empty range,
                    // and the whole strip would paint as a blank band.
                    let lo = lo.min(n.saturating_sub(1));
                    visible = lo..hi.max(lo + 1).min(n);

                    let p = ui.painter();
                    for i in lo..hi {
                        // 筛选只决定「画不画」，不改真实帧序：i 仍是序列里的真帧号，
                        // 所以播放范围、导出范围、缩略图预热都还是原来的语义。
                        //
                        // The filter only decides whether a cell is drawn; `i` stays the
                        // real sequence index, so playback, export and thumbnail warm-up
                        // keep their original meaning.
                        if !state.film_visible(i) {
                            continue;
                        }
                        let cell = Rect::from_min_size(
                            Pos2::new(strip.left() + i as f32 * cell_w + 1.0, strip.top()),
                            Vec2::new(cell_w - 2.0, h),
                        );
                        // 单元对齐界面稿 .th：gap 2、圆角 3、canvas 底、
                        // 外圈 1px 白 5% 的极淡环，没有内衬黑卡。
                        p.rect_filled(cell, radius_chip(), canvas());
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

            // 筛选把所有帧都藏掉时不能留一片纯白（用户截图「突然底部没有
            // 预览图了」）：在条内居中写明原因，并给一枚一键清除。
            // 这枚提示画在滚动区**外面**：横向 ScrollArea 的内容只有 n 格宽
            // （单帧时 92px），落在内容布局之外的控件虽然画得出来，命中区却被
            // 内容 UI 的 clip 裁空，点了没反应。画在条自己的 Ui 上，命中区就是
            // 整个可见条。
            //
            // When the filter hides every frame the strip must not sit blank
            // (user screenshot: "the filmstrip previews suddenly disappeared"):
            // say why, centred, and offer a one-click clear. It is drawn *outside*
            // the horizontal ScrollArea: the content is only n cells wide (92 px for
            // a single frame), and a widget laid out beyond that content gets its
            // interact rect clipped away while still painting — so clicks went
            // nowhere. Painting on the strip's own Ui keeps the whole visible
            // strip hittable.
            if n > 0 && state.film_visible_count() == 0 {
                empty_hint(state, ui, rect);
            }
        },
    );

    state.ensure_thumbs(ctx, visible.start.saturating_sub(2), visible.end + 2);
}

/// 空条提示：原因 + 一键清除。必须在滚动区外调用，理由见调用处。
fn empty_hint(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    let hint = crate::i18n::tr("当前筛选下没有帧");
    let clear_g = p.layout_no_wrap(
        crate::i18n::tr("清除筛选").to_owned(),
        font(SIZE_SMALL),
        muted(),
    );
    let cy = rect.center().y;
    p.text(
        Pos2::new(rect.center().x, cy - 9.0),
        Align2::CENTER_CENTER,
        hint,
        font(SIZE_SMALL),
        dim(),
    );
    let cr = Rect::from_center_size(
        Pos2::new(rect.center().x, cy + 11.0),
        Vec2::new(clear_g.size().x + 16.0, 20.0),
    );
    let cresp = ui.interact(
        cr,
        ui.make_persistent_id("film-empty-clear"),
        Sense::click(),
    );
    if cresp.hovered() {
        p.rect_filled(cr, radius_control(), elev());
    }
    p.galley(
        Pos2::new(
            cr.center().x - clear_g.size().x * 0.5,
            cr.center().y - clear_g.size().y * 0.5,
        ),
        clear_g,
        if cresp.hovered() { text() } else { muted() },
    );
    if cresp.clicked() {
        state.film_filter_stars = 0;
        state.film_filter_flag = 0;
        state.film_filter_label = 0;
        state.scroll_to_selected = true;
    }
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
            // 走带按钮 26×26：设计图 `.tp-btn{width:26px;height:26px}`，按 26px 一格排。
            // 原 22 的命中区在触摸屏上偏小，也与设计图不符。
            //
            // Transport buttons are 26×26 per the design's `.tp-btn`, laid out on a
            // 26 px pitch. The old 22 hit area was small for touch and did not match
            // the design.
            let r = Rect::from_center_size(
                Pos2::new(rect.left() + 16.0 + idx as f32 * 26.0, cy),
                Vec2::splat(26.0),
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
