//! 画布区：图像绘制、裁剪/运动叠加、手柄、标尺与空态。
//!
//! Canvas area: image drawing, crop and motion overlays, handles, rulers and the
//! empty state.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::MICRO;
use super::RULER;
use super::UV;
use super::dialogs::open_dialog;
use super::dialogs::open_photos_dialog;
use super::mask::MaskGeo;
use super::mask::brush_begin;
use super::mask::brush_extend;
use super::mask::draw_mask_overlay;
use super::mask::mask_comp_is_brush;
use super::mask::mask_drag_handle;
use super::mask::mask_hit_any;
use super::mask::mask_pick_color;
use super::mask::mask_pixel_size;
use super::menu::fit_view;
use super::toolbar::doc_tab;
use crate::icons::{self, Glyph};
use crate::state::{AppState, CropDrag, Module, MotionBox, Tool};
use crate::theme::{self, *};
use aevum_core::align::CropRect;
use aevum_core::crop;
use egui::{
    Align2, Color32, CursorIcon, PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

// ---------------------------------------------------------------- 画布

pub(super) fn canvas_panel(state: &mut AppState, ui: &mut Ui) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    let ctx = ui.ctx().clone();

    // 画布顶栏（cv-toolbar）：28px header 底 + 下缘发丝线，画在 CentralPanel 内部
    // —— 不新增 egui Panel。左端文档页签，中段尺寸读数，右端蒙版数与撤销提示。
    let tb = Rect::from_min_size(full.min, Vec2::new(full.width(), CV_TOOLBAR_H));
    p.rect_filled(tb, 0, header());
    theme::rule(&p, tb.left(), tb.right(), tb.bottom(), divider());
    let tab_w = 208.0;
    doc_tab(
        state,
        &p,
        ui,
        &ctx,
        Rect::from_min_size(
            Pos2::new(tb.left() + 10.0, tb.top() + 1.0),
            Vec2::new(tab_w, TL_HD_H),
        ),
    );
    // 中段：{w} × {h} · {n} bit（位深从 RAW/DNG 解析注记里取，普通文件不显示）。
    let res = state
        .resolution
        .map(|(w, h)| format!("{w} × {h}"))
        .unwrap_or_else(|| "—".to_owned());
    let bits = state.source_note.as_ref().and_then(|s| {
        s.strip_suffix(" 位")
            .and_then(|t| t.rsplit(' ').next())
            .and_then(|t| t.parse::<u32>().ok())
    });
    let mid_text = match bits {
        Some(b) => format!("{res} · {b} bit"),
        None => res,
    };
    p.text(
        Pos2::new(tb.left() + 10.0 + tab_w + 12.0, tb.center().y),
        Align2::LEFT_CENTER,
        mid_text,
        font(SIZE_SMALL),
        dim(),
    );
    // 右端：蒙版 n 个 · 撤销 Ctrl+Z（11px dim，说明文字不上交互）。
    let undo_text = crate::i18n::t!("撤销 Ctrl+Z");
    let undo_w = p
        .layout_no_wrap(undo_text.to_owned(), font(SIZE_SMALL), dim())
        .size()
        .x;
    p.text(
        Pos2::new(tb.right() - 10.0, tb.center().y),
        Align2::RIGHT_CENTER,
        undo_text,
        font(SIZE_SMALL),
        dim(),
    );
    p.text(
        Pos2::new(tb.right() - 10.0 - undo_w - 12.0, tb.center().y),
        Align2::RIGHT_CENTER,
        crate::i18n::tr_fmt("蒙版 {} 个", &[&state.params.masks.len()]),
        font(SIZE_SMALL),
        dim(),
    );

    let body = Rect::from_min_max(Pos2::new(full.left(), full.top() + CV_TOOLBAR_H), full.max);
    p.rect_filled(body, 0, surround());

    let gutter = if state.rulers { RULER } else { 0.0 };
    let content = Rect::from_min_max(body.min + Vec2::splat(gutter), body.max);
    p.rect_filled(content, 0, canvas());

    if state.seq.is_none() {
        if state.rulers {
            draw_rulers(ui, body, content, None, 1.0, None);
        }
        empty_state(state, ui, content);
        return;
    }
    let n = state.frame_count();
    if n == 0 {
        return;
    }

    let (tex, orig, size) = if state.playing {
        match state.thumb(state.selected) {
            Some(t) => (Some(t.texture.clone()), None, t.size),
            None => (None, None, [0, 0]),
        }
    } else {
        (
            // 有插件结果就先显示它：那正是用户按下"应用"后想看到的东西。
            state
                .plugin_result
                .clone()
                .or_else(|| state.texture.clone()),
            if state.split {
                state.texture_orig.clone()
            } else {
                None
            },
            state.preview_size,
        )
    };

    let (iw, ih) = if size[0] > 0 {
        (size[0] as f32, size[1] as f32)
    } else {
        (16.0, 9.0)
    };
    let avail = content.shrink(26.0);
    let fit = (avail.width() / iw).min(avail.height() / ih).max(0.01);
    state.fit_scale = fit;
    let scale = fit * state.zoom;
    let shown = Vec2::new(iw, ih) * scale;
    let center = content.center() + state.pan;
    let img = Rect::from_center_size(center, shown);

    let resp = ui.interact(
        content,
        ui.make_persistent_id("canvas"),
        Sense::click_and_drag(),
    );
    let hover = resp.hover_pos();
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.1 {
            let old = state.zoom;
            let new = (old * (scroll * 0.0022).exp()).clamp(0.1, 24.0);
            if let Some(pos) = hover {
                state.pan += (pos - center) * (1.0 - new / old.max(1e-6));
            }
            state.zoom = new;
        }
    }
    let mask_edit =
        state.module == Module::Develop && state.tool != Tool::Crop && state.mask_sel.is_some();
    if mask_edit {
        let (gw, gh) = mask_pixel_size(state);
        if state.mask_pick {
            if resp.clicked()
                && let (Some(sel), Some(pos)) = (state.mask_sel, resp.interact_pointer_pos())
            {
                mask_pick_color(state, img, pos, sel);
            }
        } else if resp.drag_started_by(PointerButton::Primary)
            && let (Some(sel), Some(pos)) = (state.mask_sel, resp.interact_pointer_pos())
        {
            if mask_comp_is_brush(state, sel, state.mask_comp) {
                // 画笔没有手柄可抓：按下就是起笔，一次拖 = 一笔画。
                state.mask_paint = Some((sel, state.mask_comp));
                brush_begin(state, img, pos);
            } else {
                // 缓存取值：指纹命中零分配（性能重构设计 §3 第 1 层）。
                let comp_cur = state.mask_comp;
                let geos = state.mask_geos_cached(sel);
                if let Some((comp, handle)) = mask_hit_any(geos, img, gw, gh, pos, comp_cur) {
                    state.mask_comp = comp;
                    state.mask_drag = Some((sel, handle));
                }
            }
        }
        if state.mask_drag.is_some()
            && resp.dragged_by(PointerButton::Primary)
            && let Some(pos) = resp.interact_pointer_pos()
        {
            mask_drag_handle(state, img, pos);
        }
        if state.mask_drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            state.mask_drag = None;
        }
        if state.mask_paint.is_some()
            && resp.dragged_by(PointerButton::Primary)
            && let Some(pos) = resp.interact_pointer_pos()
        {
            brush_extend(state, img, pos);
        }
        if state.mask_paint.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            state.mask_paint = None;
        }
    }
    if state.mask_drag.is_none()
        && (resp.dragged_by(PointerButton::Middle) || (resp.dragged() && state.tool == Tool::Hand))
    {
        state.pan += resp.drag_delta();
    }
    if resp.double_clicked() {
        fit_view(state);
    } else if resp.clicked() && state.tool == Tool::Zoom {
        let alt = ui.input(|i| i.modifiers.alt);
        let old = state.zoom;
        let mut new = old * if alt { 1.0 / 1.6 } else { 1.6 };
        new = new.clamp(0.1, 24.0);
        if let Some(pos) = hover {
            let c = content.center() + state.pan;
            state.pan += (pos - c) * (1.0 - new / old.max(1e-6));
        }
        state.zoom = new;
    }
    if resp.hovered() {
        let icon = match state.tool {
            Tool::Hand => {
                if resp.dragged() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                }
            }
            Tool::Zoom => {
                if ui.input(|i| i.modifiers.alt) {
                    CursorIcon::ZoomOut
                } else {
                    CursorIcon::ZoomIn
                }
            }
            Tool::Crop => CursorIcon::Crosshair,
            Tool::Compare => CursorIcon::ResizeHorizontal,
            Tool::Grid => CursorIcon::Crosshair,
        };
        ui.ctx().set_cursor_icon(icon);
        if mask_edit {
            let (gw, gh) = mask_pixel_size(state);
            let comp_cur = state.mask_comp;
            let over = hover
                .and_then(|pos| {
                    // 缓存取值：同一份数据不再每帧第二次解析（E2）。
                    let geos: &[MaskGeo] = match state.mask_sel {
                        Some(sel) => state.mask_geos_cached(sel),
                        None => &[],
                    };
                    mask_hit_any(geos, img, gw, gh, pos, comp_cur)
                })
                .is_some();
            if over {
                ui.ctx().set_cursor_icon(if state.mask_drag.is_some() {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                });
            }
            if state
                .mask_sel
                .is_some_and(|sel| mask_comp_is_brush(state, sel, state.mask_comp))
            {
                // 画笔没有手柄，画布上就是一支笔：十字光标，圆圈跟着半径走。
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
            if state.mask_pick {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
        }
    }

    p.rect_filled(
        img.translate(Vec2::new(0.0, 7.0)).expand(7.0),
        0,
        Color32::from_black_alpha(70),
    );
    p.rect_stroke(
        img,
        0,
        Stroke::new(1.0, border_strong()),
        StrokeKind::Outside,
    );

    match &tex {
        Some(t) => {
            p.image(t.id(), img, UV, Color32::WHITE);
            if let Some(o) = &orig {
                // 分割线位置跟着 `split_pos`（0..1）走，按住手柄左右拉；LR 的
                // 前后对比就是这一种——不是钉死在中间的一条死线。
                let x = img.min.x + img.width() * state.split_pos.clamp(0.02, 0.98);
                let left = Rect::from_min_max(img.min, Pos2::new(x, img.max.y));
                p.clone()
                    .with_clip_rect(left)
                    .image(o.id(), img, UV, Color32::WHITE);
                p.line_segment(
                    [Pos2::new(x, img.top()), Pos2::new(x, img.bottom())],
                    Stroke::new(1.0, Color32::from_white_alpha(200)),
                );
                // 拖动手柄：分割线中段一枚圆形抓手，纵向居中；双击回到居中。
                let grip =
                    Rect::from_center_size(Pos2::new(x, img.center().y), Vec2::new(18.0, 28.0));
                let gresp = ui.interact(
                    grip,
                    ui.make_persistent_id("split-grip"),
                    Sense::click_and_drag(),
                );
                if gresp.dragged()
                    && let Some(pt) = gresp.interact_pointer_pos()
                {
                    state.split_pos = ((pt.x - img.min.x) / img.width().max(1.0)).clamp(0.02, 0.98);
                }
                if gresp.double_clicked() {
                    state.split_pos = 0.5;
                }
                if gresp.hovered() || gresp.dragged() {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                // 手柄本体：深底白描边，两枚箭头示意可拉；悬停时抬亮。
                let gcol = if gresp.dragged() {
                    accent()
                } else if gresp.hovered() {
                    elev_hover()
                } else {
                    Color32::from_rgba_unmultiplied(24, 24, 26, 235)
                };
                p.circle_filled(
                    grip.center() + Vec2::new(0.0, 1.0),
                    11.0,
                    Color32::from_black_alpha(90),
                );
                p.circle_filled(grip.center(), 11.0, gcol);
                p.circle_stroke(
                    grip.center(),
                    11.0,
                    Stroke::new(1.0, Color32::from_white_alpha(190)),
                );
                let arrow = |dx: f32| {
                    let c = grip.center();
                    [
                        Pos2::new(c.x + dx - 2.2, c.y - 4.0),
                        Pos2::new(c.x + dx - 4.0, c.y),
                        Pos2::new(c.x + dx - 2.2, c.y + 4.0),
                        Pos2::new(c.x + dx + 0.6, c.y),
                    ]
                };
                p.add(Shape::convex_polygon(
                    arrow(-3.4).to_vec(),
                    Color32::WHITE,
                    Stroke::NONE,
                ));
                p.add(Shape::convex_polygon(
                    arrow(5.2).to_vec(),
                    Color32::WHITE,
                    Stroke::NONE,
                ));
                chip_label(
                    &p,
                    Pos2::new(x - 8.0, img.top() + 8.0),
                    "前",
                    Align2::RIGHT_TOP,
                );
                chip_label(
                    &p,
                    Pos2::new(x + 8.0, img.top() + 8.0),
                    "后",
                    Align2::LEFT_TOP,
                );
            }
        }
        None => {
            p.rect_filled(img, 0, thumb_bg());
            p.text(
                img.center(),
                Align2::CENTER_CENTER,
                "载入预览…",
                font(SIZE_SMALL),
                dim(),
            );
        }
    }

    if state.crop_editing() {
        crop_overlay(state, ui, &p, img, &resp);
    } else if state.motion_editing() {
        motion_overlay(state, ui, &p, img, &resp);
    }

    // 逐帧时间戳叠印：左下角一行「帧号 · 拍摄时间 · 机型曝光」。
    // 药丸底压成半透明黑板，画面再亮也读得出来。
    //
    // Per-frame timestamp: one line in the bottom-left corner. The translucent pill keeps it
    // readable on any frame.
    if state.stamp_on
        && let Some(text) = state.stamp_text()
    {
        chip_label(
            &p,
            Pos2::new(img.left() + 10.0, img.bottom() - 10.0),
            &text,
            Align2::LEFT_BOTTOM,
        );
    }

    if state.grid {
        for k in 1..3 {
            let x = img.left() + img.width() * k as f32 / 3.0;
            let y = img.top() + img.height() * k as f32 / 3.0;
            p.line_segment(
                [Pos2::new(x, img.top()), Pos2::new(x, img.bottom())],
                Stroke::new(0.8, Color32::from_white_alpha(42)),
            );
            p.line_segment(
                [Pos2::new(img.left(), y), Pos2::new(img.right(), y)],
                Stroke::new(0.8, Color32::from_white_alpha(42)),
            );
        }
    }
    // 剪切警告（PS/LR 的黑白场检查）：红=高光裁切、蓝=阴影裁切。遮罩与预览
    // 同源同比例，LINEAR 拉伸到画布矩形就套得上；草图帧沿用旧遮罩，无妨。
    if let Some(cm) = &state.clip_mask {
        p.image(cm.id(), img, UV, Color32::WHITE);
    }
    if state.cross {
        let c = img.center();
        p.line_segment(
            [Pos2::new(c.x - 14.0, c.y), Pos2::new(c.x + 14.0, c.y)],
            Stroke::new(0.9, Color32::from_white_alpha(80)),
        );
        p.line_segment(
            [Pos2::new(c.x, c.y - 14.0), Pos2::new(c.x, c.y + 14.0)],
            Stroke::new(0.9, Color32::from_white_alpha(80)),
        );
    }
    draw_mask_overlay(&p, state, img, resp.hover_pos());

    if state.preview_inflight || state.measuring.is_some() {
        chip_label(
            &p,
            Pos2::new(content.right() - 10.0, content.top() + 10.0),
            if state.measuring.is_some() {
                "测光中"
            } else {
                "渲染中"
            },
            Align2::RIGHT_TOP,
        );
    }

    if state.rulers {
        draw_rulers(ui, body, content, Some(img), scale, hover);
    }
}

/// 裁剪态的取景框：拖拽交互 + 框外压暗 + 三分线 + 八向把手 + 尺寸标签。
///
/// 这一层全是矢量：拖动只挪框，一个像素都不重画，所以 24MP 的图也跟手。
/// 取景态下预览画的是整块可见区（`state.crop_view()`，拉直后即安全框），于是屏幕和
/// 那幅像素只剩一层等比缩放，换算就在下面两个闭包里。
fn crop_overlay(state: &mut AppState, ui: &mut Ui, p: &egui::Painter, img: Rect, resp: &Response) {
    let view = state.crop_view();
    if view.w == 0 || view.h == 0 {
        return;
    }
    let (sx, sy) = (img.width() / view.w as f32, img.height() / view.h as f32);
    if !(sx.is_finite() && sy.is_finite()) || sx <= 0.0 || sy <= 0.0 {
        return;
    }
    let to_src = |pos: Pos2| {
        [
            f64::from(view.x) + (pos.x - img.min.x) as f64 / sx as f64,
            f64::from(view.y) + (pos.y - img.min.y) as f64 / sy as f64,
        ]
    };
    let to_screen = |r: CropRect| {
        let at = |x: u32, y: u32| {
            img.min
                + Vec2::new(
                    (x as f64 - f64::from(view.x)) as f32 * sx,
                    (y as f64 - f64::from(view.y)) as f32 * sy,
                )
        };
        Rect::from_two_pos(at(r.x, r.y), at(r.x + r.w, r.y + r.h))
    };

    // 按下点决定这一拖是什么：把手=缩放，框内=平移，框外=重拉一个框。
    if resp.drag_started_by(PointerButton::Primary)
        && state.crop_drag.is_none()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let cur = to_screen(state.crop_box());
        let hit = crop::hit_test(
            crop::Edges::new(cur.min.x, cur.min.y, cur.max.x, cur.max.y),
            pos.x,
            pos.y,
            8.0,
            5.0,
        );
        state.crop_begin(to_src(pos), hit);
    }
    if state.crop_drag.is_some()
        && resp.dragged_by(PointerButton::Primary)
        && let Some(pos) = resp.interact_pointer_pos()
    {
        state.crop_drag_to(to_src(pos));
    }
    if state.crop_drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
        state.crop_end();
    }

    let sel = state.crop_box();
    let cur = to_screen(sel).intersect(img);
    let dim = Color32::from_black_alpha(150);
    for r in [
        Rect::from_min_max(img.min, Pos2::new(img.max.x, cur.top())),
        Rect::from_min_max(Pos2::new(img.min.x, cur.bottom()), img.max),
        Rect::from_min_max(
            Pos2::new(img.min.x, cur.top()),
            Pos2::new(cur.left(), cur.bottom()),
        ),
        Rect::from_min_max(
            Pos2::new(cur.right(), cur.top()),
            Pos2::new(img.max.x, cur.bottom()),
        ),
    ] {
        if r.is_positive() {
            p.rect_filled(r, 0, dim);
        }
    }
    p.rect_stroke(cur, 0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
    for k in 1..3 {
        let x = cur.left() + cur.width() * k as f32 / 3.0;
        let y = cur.top() + cur.height() * k as f32 / 3.0;
        p.line_segment(
            [Pos2::new(x, cur.top()), Pos2::new(x, cur.bottom())],
            Stroke::new(0.8, Color32::from_white_alpha(55)),
        );
        p.line_segment(
            [Pos2::new(cur.left(), y), Pos2::new(cur.right(), y)],
            Stroke::new(0.8, Color32::from_white_alpha(55)),
        );
    }
    crop_handles(p, cur);
    chip_label(
        p,
        cur.max - Vec2::splat(6.0),
        &format!("{} × {}", sel.w, sel.h),
        Align2::RIGHT_BOTTOM,
    );

    // 光标跟着命中的位置走：把手双向箭头、框内可拖、框外是十字（新建框）。
    if let Some(pos) = resp.hover_pos() {
        // 拖动中光标钉在按下那一刻的把手上：框跟着指针走，命中区早就跑偏了。
        let icon = match state.crop_drag {
            Some(CropDrag::Resize { handle, .. }) => resize_cursor(handle),
            Some(_) => CursorIcon::Grabbing,
            None => match crop::hit_test(
                crop::Edges::new(cur.min.x, cur.min.y, cur.max.x, cur.max.y),
                pos.x,
                pos.y,
                8.0,
                5.0,
            ) {
                crop::Hit::Corner(handle) | crop::Hit::Edge(handle) => resize_cursor(handle),
                crop::Hit::Inside => CursorIcon::Move,
                crop::Hit::Outside => CursorIcon::Crosshair,
            },
        };
        ui.ctx().set_cursor_icon(icon);
    }
}

/// 运镜取景框编辑层：取景基准那一幅铺在画布上，起点/终点两个框叠成矢量。
///
/// 只有面板上选中的那一端能拖（框外压暗 + 八向把手），另一端只画轮廓当参照，
/// 再加一条从起点中心指向终点中心的箭头——镜头往哪儿走、走多大一眼就能看清。
/// 这层全是矢量，拖动一个像素都不重画，所以 24MP 的图也跟手。
fn motion_overlay(
    state: &mut AppState,
    ui: &mut Ui,
    p: &egui::Painter,
    img: Rect,
    resp: &Response,
) {
    let base = state.motion_base_rect();
    if base.w < 2 || base.h < 2 {
        return;
    }
    let (sx, sy) = (img.width() / base.w as f32, img.height() / base.h as f32);
    if !(sx.is_finite() && sy.is_finite()) || sx <= 0.0 || sy <= 0.0 {
        return;
    }
    let to_src = |pos: Pos2| {
        [
            f64::from(base.x) + (pos.x - img.min.x) as f64 / f64::from(sx),
            f64::from(base.y) + (pos.y - img.min.y) as f64 / f64::from(sy),
        ]
    };
    let to_screen = |r: CropRect| {
        let at = |x: u32, y: u32| {
            img.min
                + Vec2::new(
                    (f64::from(x) - f64::from(base.x)) as f32 * sx,
                    (f64::from(y) - f64::from(base.y)) as f32 * sy,
                )
        };
        Rect::from_two_pos(at(r.x, r.y), at(r.x + r.w, r.y + r.h))
    };

    let which = state.motion_box;
    let start = to_screen(state.motion_box_rect(MotionBox::Start));
    let end = to_screen(state.motion_box_rect(MotionBox::End));
    let cur = if which == MotionBox::Start {
        start
    } else {
        end
    };

    // 按下点决定这一拖是什么：把手=缩放，框内=平移，框外=重拉一个框。
    if resp.drag_started_by(PointerButton::Primary)
        && state.motion_drag.is_none()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let hit = crop::hit_test(
            crop::Edges::new(cur.min.x, cur.min.y, cur.max.x, cur.max.y),
            pos.x,
            pos.y,
            8.0,
            5.0,
        );
        state.motion_begin(to_src(pos), hit);
    }
    if state.motion_drag.is_some()
        && resp.dragged_by(PointerButton::Primary)
        && let Some(pos) = resp.interact_pointer_pos()
    {
        state.motion_drag_to(to_src(pos));
    }
    if state.motion_drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
        state.motion_end();
    }

    let cur = cur.intersect(img);
    let dim = Color32::from_black_alpha(120);
    for r in [
        Rect::from_min_max(img.min, Pos2::new(img.max.x, cur.top())),
        Rect::from_min_max(Pos2::new(img.min.x, cur.bottom()), img.max),
        Rect::from_min_max(
            Pos2::new(img.min.x, cur.top()),
            Pos2::new(cur.left(), cur.bottom()),
        ),
        Rect::from_min_max(
            Pos2::new(cur.right(), cur.top()),
            Pos2::new(img.max.x, cur.bottom()),
        ),
    ] {
        if r.is_positive() {
            p.rect_filled(r, 0, dim);
        }
    }
    p.rect_stroke(cur, 0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
    for k in 1..3 {
        let x = cur.left() + cur.width() * k as f32 / 3.0;
        let y = cur.top() + cur.height() * k as f32 / 3.0;
        p.line_segment(
            [Pos2::new(x, cur.top()), Pos2::new(x, cur.bottom())],
            Stroke::new(0.8, Color32::from_white_alpha(55)),
        );
        p.line_segment(
            [Pos2::new(cur.left(), y), Pos2::new(cur.right(), y)],
            Stroke::new(0.8, Color32::from_white_alpha(55)),
        );
    }
    crop_handles(p, cur);

    // 另一端：只描一圈，当「它往哪儿走」的参照，不参与压暗和命中。
    let other = if which == MotionBox::Start {
        end
    } else {
        start
    };
    p.rect_stroke(
        other.intersect(img),
        0,
        Stroke::new(1.0, Color32::from_white_alpha(140)),
        StrokeKind::Inside,
    );
    // 当前帧走到的那一格：橙色一圈，把「现在看到的」和两端对起来。
    if let Some(c) = state.motion_current_rect() {
        p.rect_stroke(
            to_screen(c).intersect(img),
            0,
            Stroke::new(1.2, Color32::from_rgba_unmultiplied(255, 159, 10, 220)),
            StrokeKind::Inside,
        );
    }

    // 起点 → 终点的走向箭头：两框中心差得太小就不画，免得糊成一团噪点。
    let (o, t) = (start.center(), end.center());
    let d = t - o;
    let len = d.length();
    if len > 14.0 {
        let dir = d / len;
        let nrm = Vec2::new(-dir.y, dir.x);
        let barb = (len * 0.14).min(18.0);
        let col = Color32::from_white_alpha(190);
        p.line_segment([o, t], Stroke::new(1.2, col));
        for s in [1.0, -1.0] {
            p.line_segment([t, t + (nrm * s - dir) * barb], Stroke::new(1.2, col));
        }
    }

    let px = state.motion_box_rect(which);
    let (fw, fh) = state.motion_frame_size();
    let (ow, oh) = state.motion_output_size();
    let v = match which {
        MotionBox::Start => state.motion.from,
        MotionBox::End => state.motion.to,
    };
    chip_label(
        p,
        cur.max - Vec2::splat(6.0),
        &format!(
            "{} {} × {} · {:.2}×",
            crate::i18n::tr(which.cn()),
            px.w,
            px.h,
            v.zoom(ow, oh, fw, fh)
        ),
        Align2::RIGHT_BOTTOM,
    );
    chip_label(
        p,
        other.min + Vec2::splat(6.0),
        crate::i18n::tr(which.other().cn()),
        Align2::LEFT_TOP,
    );

    if let Some(pos) = resp.hover_pos() {
        let icon = match state.motion_drag {
            Some(CropDrag::Resize { handle, .. }) => resize_cursor(handle),
            Some(_) => CursorIcon::Grabbing,
            None => match crop::hit_test(
                crop::Edges::new(cur.min.x, cur.min.y, cur.max.x, cur.max.y),
                pos.x,
                pos.y,
                8.0,
                5.0,
            ) {
                crop::Hit::Corner(handle) | crop::Hit::Edge(handle) => resize_cursor(handle),
                crop::Hit::Inside => CursorIcon::Move,
                crop::Hit::Outside => CursorIcon::Crosshair,
            },
        };
        ui.ctx().set_cursor_icon(icon);
    }
}

/// 把手对应的双向箭头：横边竖向、竖边横向、斜角走对角线。
fn resize_cursor(handle: crop::Handle) -> CursorIcon {
    match handle {
        crop::Handle::Nw | crop::Handle::Se => CursorIcon::ResizeNwSe,
        crop::Handle::Ne | crop::Handle::Sw => CursorIcon::ResizeNeSw,
        crop::Handle::N | crop::Handle::S => CursorIcon::ResizeVertical,
        crop::Handle::E | crop::Handle::W => CursorIcon::ResizeHorizontal,
    }
}

fn crop_handles(p: &egui::Painter, r: Rect) {
    let pts = [
        r.left_top(),
        r.center_top(),
        r.right_top(),
        r.right_center(),
        r.right_bottom(),
        r.center_bottom(),
        r.left_bottom(),
        r.left_center(),
    ];
    for c in pts {
        p.rect_filled(
            Rect::from_center_size(c, Vec2::splat(2.0)),
            0,
            Color32::WHITE,
        );
    }
}

fn chip_label(p: &egui::Painter, pos: Pos2, text: &str, anchor: Align2) {
    let galley = p.layout_no_wrap(text.to_owned(), mono(SIZE_SMALL), palette().text);
    let pad = Vec2::new(5.0, 2.5);
    let rect = anchor.anchor_size(pos, galley.size() + pad * 2.0);
    p.rect_filled(rect, radius_control(), Color32::from_black_alpha(180));
    p.galley(rect.min + pad, galley, palette().text);
}

fn draw_rulers(
    ui: &Ui,
    full: Rect,
    content: Rect,
    img: Option<Rect>,
    scale: f32,
    hover: Option<Pos2>,
) {
    let p = ui.painter();
    let top = Rect::from_min_max(full.min, Pos2::new(full.right(), full.top() + RULER));
    let left = Rect::from_min_max(full.min, Pos2::new(full.left() + RULER, full.bottom()));
    let corner = Rect::from_min_max(full.min, Pos2::new(full.left() + RULER, full.top() + RULER));
    p.rect_filled(top, 0, header());
    p.rect_filled(left, 0, header());
    p.rect_filled(corner, 0, header());
    p.line_segment(
        [
            Pos2::new(full.left(), top.bottom()),
            Pos2::new(full.right(), top.bottom()),
        ],
        Stroke::new(1.0, border()),
    );
    p.line_segment(
        [
            Pos2::new(full.left(), top.bottom()),
            Pos2::new(left.right(), top.bottom()),
        ],
        Stroke::new(1.0, border()),
    );
    p.line_segment(
        [
            Pos2::new(left.right(), full.top()),
            Pos2::new(left.right(), full.bottom()),
        ],
        Stroke::new(1.0, border()),
    );

    let Some(img) = img else {
        return;
    };
    let mut unit: i64 = 10;
    for cand in [10i64, 20, 50, 100, 200, 500, 1000, 2000, 5000] {
        unit = cand;
        if cand as f32 * scale >= 56.0 {
            break;
        }
    }
    let origin_x = img.left();
    let origin_y = img.top();
    let first_x = (((content.left() - origin_x) / scale) / unit as f32).floor() as i64 * unit;
    let mut v = first_x;
    while origin_x + v as f32 * scale < content.right() {
        let x = origin_x + v as f32 * scale;
        if x >= content.left() + 1.0 {
            p.line_segment(
                [Pos2::new(x, top.bottom() - 5.0), Pos2::new(x, top.bottom())],
                Stroke::new(1.0, divider()),
            );
            if x + 28.0 < content.right() {
                p.text(
                    Pos2::new(x + 3.0, top.top() + 2.0),
                    Align2::LEFT_TOP,
                    v.to_string(),
                    mono(MICRO),
                    dim(),
                );
            }
        }
        v += unit;
    }
    let first_y = (((content.top() - origin_y) / scale) / unit as f32).floor() as i64 * unit;
    let mut v = first_y;
    while origin_y + v as f32 * scale < content.bottom() {
        let y = origin_y + v as f32 * scale;
        if y >= content.top() + 1.0 {
            p.line_segment(
                [Pos2::new(left.right() - 5.0, y), Pos2::new(left.right(), y)],
                Stroke::new(1.0, divider()),
            );
            if y + 12.0 < content.bottom() {
                p.text(
                    Pos2::new(left.left() + 2.0, y + 1.0),
                    Align2::LEFT_TOP,
                    v.to_string(),
                    mono(MICRO),
                    dim(),
                );
            }
        }
        v += unit;
    }
    if let Some(pos) = hover {
        if pos.x >= content.left() && pos.x <= content.right() {
            p.line_segment(
                [
                    Pos2::new(pos.x, top.top() + 2.0),
                    Pos2::new(pos.x, top.bottom()),
                ],
                Stroke::new(1.0, accent().gamma_multiply(0.8)),
            );
        }
        if pos.y >= content.top() && pos.y <= content.bottom() {
            p.line_segment(
                [
                    Pos2::new(left.left() + 2.0, pos.y),
                    Pos2::new(left.right(), pos.y),
                ],
                Stroke::new(1.0, accent().gamma_multiply(0.8)),
            );
        }
    }
}

fn empty_state(state: &mut AppState, ui: &mut Ui, content: Rect) {
    let ctx = ui.ctx().clone();
    let p = ui.painter().clone();
    let area = Rect::from_center_size(content.center(), Vec2::new(420.0, 236.0));
    theme::dashed_rect(&p, area, border_strong(), 6.0);
    icons::paint(
        &p,
        Rect::from_center_size(
            Pos2::new(area.center().x, area.top() + 52.0),
            Vec2::splat(36.0),
        ),
        Glyph::Folder,
        accent(),
    );
    p.text(
        Pos2::new(area.center().x, area.top() + 98.0),
        Align2::CENTER_CENTER,
        "打开一叠帧或一张照片",
        font(SIZE_HEADING),
        text(),
    );
    p.text(
        Pos2::new(area.center().x, area.top() + 124.0),
        Align2::CENTER_CENTER,
        "文件夹和照片都能直接拖进来",
        font(SIZE_SMALL),
        muted(),
    );
    let button = Rect::from_center_size(
        Pos2::new(area.center().x - 73.0, area.bottom() - 44.0),
        Vec2::new(138.0, 28.0),
    );
    let resp = ui.interact(button, ui.make_persistent_id("empty-open"), Sense::click());
    let fill = if resp.dragged() {
        accent_dim()
    } else if resp.hovered() {
        accent_hover()
    } else {
        accent()
    };
    p.rect_filled(button, radius_control(), fill);
    p.text(
        button.center(),
        Align2::CENTER_CENTER,
        "打开帧文件夹",
        font(SIZE_BODY),
        on_accent(),
    );
    if resp.clicked() {
        open_dialog(state, &ctx);
    }
    // 单张照片是一等公民，不该藏在二级菜单里：空状态直接给一个入口。
    let photo_btn = Rect::from_center_size(
        Pos2::new(area.center().x + 73.0, area.bottom() - 44.0),
        Vec2::new(138.0, 28.0),
    );
    let presp = ui.interact(
        photo_btn,
        ui.make_persistent_id("empty-photo"),
        Sense::click(),
    );
    let pfill = if presp.dragged() {
        elev()
    } else if presp.hovered() {
        elev_hover()
    } else {
        bg()
    };
    p.rect_filled(photo_btn, radius_control(), pfill);
    p.rect_stroke(
        photo_btn,
        radius_control(),
        Stroke::new(1.0, border_strong()),
        StrokeKind::Inside,
    );
    p.text(
        photo_btn.center(),
        Align2::CENTER_CENTER,
        "选照片…",
        font(SIZE_BODY),
        if presp.hovered() { text() } else { muted() },
    );
    if presp.clicked() {
        open_photos_dialog(state, &ctx);
    }
}
