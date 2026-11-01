//! 底部两行：时间线、曲线、色标、胶片条与走带控制。

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

    let n = state.frame_count();
    p.text(
        Pos2::new(hd.left() + 10.0, hd.center().y),
        Align2::LEFT_CENTER,
        if n > 0 {
            crate::i18n::tr_fmt("胶片条 · {} 帧 · 已评级 {}", &[&n, &state.rated_count()])
        } else {
            crate::i18n::tr("胶片条 · 无序列").to_owned()
        },
        font(SIZE_SMALL),
        dim(),
    );

    // 右端两枚开关降为 ghost：透明底、dim 字，悬停才抬起 —— 头部行是说明，
    // 不是按钮排。胶片那枚带旋转箭头（Lightroom 口径）：▼ 收起 / ▸ 展开，
    // 点击收起的只是缩略图条，这行 26px 头部留着，展开的入口永远在场。
    let chip_x = hd.right() - 10.0 - 60.0;
    let film_rect = Rect::from_min_size(
        Pos2::new(chip_x, hd.center().y - 9.0),
        Vec2::new(60.0, 18.0),
    );
    let film_resp = ui.interact(film_rect, ui.make_persistent_id("tl-film"), Sense::click());
    if film_resp.hovered() {
        p.rect_filled(film_rect, radius_control(), elev());
    }
    icons::paint(
        &p,
        Rect::from_center_size(
            Pos2::new(film_rect.left() + 10.0, film_rect.center().y),
            Vec2::splat(10.0),
        ),
        if state.show_film {
            Glyph::ChevronDown
        } else {
            Glyph::ChevronRight
        },
        if film_resp.hovered() { text() } else { dim() },
    );
    p.text(
        Pos2::new(film_rect.left() + 20.0, film_rect.center().y),
        Align2::LEFT_CENTER,
        crate::i18n::tr("胶片"),
        font(SIZE_SMALL),
        if state.show_film { text() } else { dim() },
    );
    if film_resp.clicked() {
        state.show_film = !state.show_film;
    }
    film_resp.on_hover_text(crate::i18n::t!("收起 / 展开底部预览栏"));
    let curve_rect = Rect::from_min_size(
        Pos2::new(film_rect.left() - 44.0, hd.center().y - 9.0),
        Vec2::new(40.0, 18.0),
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
                        for s in 0..5u8 {
                            let sr = Rect::from_min_size(
                                Pos2::new(cell.left() + 3.0 + s as f32 * 8.0, cell.bottom() - 14.0),
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
                                if s < rating { star() } else { dim() },
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
                        // 序号：左下角，白 75% 配黑投影（对齐 .th .num）；
                        // 先画偏移 1px 的黑影再画白字，缩略图再亮也压得住。
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
                            Color32::from_white_alpha(190),
                        );

                        let cell_resp = ui.interact(
                            cell,
                            ui.make_persistent_id(("film-cell", i)),
                            Sense::click(),
                        );
                        if i == state.selected {
                            // 选中 = 2px 强调色外框（对齐 .th.on），不再用绿框：
                            // 绿色留给旗标，蓝色才是"你在哪"。
                            p.rect_stroke(
                                cell.shrink(1.0),
                                3.0,
                                Stroke::new(2.0, accent()),
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
    let mut mini = |ui: &mut Ui, glyph: Glyph, active: bool, tint: Color32| -> bool {
        let r = Rect::from_center_size(
            Pos2::new(rect.left() + 16.0 + idx as f32 * 26.0, cy),
            Vec2::splat(22.0),
        );
        let resp = ui.interact(r, ui.make_persistent_id(("transport", idx)), Sense::click());
        idx += 1;
        if resp.hovered() {
            p.rect_filled(r, radius_control(), elev());
        }
        icons::paint(
            &p,
            r.shrink(5.0),
            glyph,
            if active {
                tint
            } else if resp.hovered() {
                text()
            } else {
                muted()
            },
        );
        resp.clicked()
    };

    let has = state.frame_count() > 0;
    if mini(ui, Glyph::Prev, false, text()) && has {
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
    ) && has
    {
        state.toggle_play();
    }
    if mini(ui, Glyph::Next, false, text()) && has {
        state.step(1);
    }

    let n = state.frame_count();
    let text_x = rect.left() + 16.0 + idx as f32 * 26.0;
    p.text(
        Pos2::new(text_x + 4.0, cy),
        Align2::LEFT_CENTER,
        if n > 0 {
            crate::i18n::tr_fmt(
                "{} / {}",
                &[&format!("{:04}", state.selected + 1), &format!("{:04}", n)],
            )
        } else {
            "— / —".to_owned()
        },
        mono(SIZE_SMALL),
        if n > 0 { muted() } else { dim() },
    );
    let off = state.offset_at(state.selected);
    p.text(
        Pos2::new(text_x + 94.0, cy),
        Align2::LEFT_CENTER,
        crate::i18n::tr_fmt("去闪 {} EV", &[&format!("{off:+.2}")]),
        mono(SIZE_SMALL),
        if off.abs() > 0.02 { accent() } else { dim() },
    );

    let mut x = text_x + 190.0;
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

    p.text(
        Pos2::new(rect.right() - 8.0, cy),
        Align2::RIGHT_CENTER,
        format!("{:.0} fps", state.output_fps),
        mono(SIZE_SMALL),
        muted(),
    );
    p.text(
        Pos2::new(rect.right() - 62.0, cy),
        Align2::RIGHT_CENTER,
        crate::i18n::tr_fmt("已评级 {}", &[&state.rated_count()]),
        font(SIZE_SMALL),
        dim(),
    );
}
