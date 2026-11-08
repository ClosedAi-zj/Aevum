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
use crate::icons::{self, Glyph};
use crate::state::{AppState, CropDrag, Module, MotionBox, Tool};
use crate::theme::{self, *};
use aevum_core::align::CropRect;
use aevum_core::crop;
use egui::{
    Align2, Color32, CursorIcon, PointerButton, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, TextureHandle, Ui, Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

// ---------------------------------------------------------------- 画布

pub(super) fn canvas_panel(state: &mut AppState, ui: &mut Ui, right_inset: f32) {
    // 右边界以「整窗右缘减去右侧面板」为准，再与 egui 给的剩余区取小：
    // 正常帧两者相等；egui 的 Panel 用记忆宽度缩剩余区的那几帧，取小值把画布
    // 拉回真实边界（见 `mod.rs` 里的说明）。
    //
    // The right edge comes from "window right minus the right-hand panels", min'd with
    // the remainder egui reports: equal on normal frames, and on the frames where egui
    // shrinks by its remembered width, the min pulls the canvas back to the real edge.
    let raw = ui.max_rect();
    let right = raw
        .right()
        .min(ui.ctx().content_rect().right() - right_inset);
    let full = Rect::from_min_max(
        raw.min,
        Pos2::new(right.max(raw.left() + 1.0), raw.bottom()),
    );
    let p = ui.painter().clone();

    // 顶层错误横幅：画布顶部正中央一条 danger 底横幅——状态栏 24px 里塞长文本
    // 必然截断，用户截屏证明他根本没看到（"错误信息放在下面我根本看不到"）。
    // 放画布顶：视线第一落点，红底白字想忽略都难；点 × 或「清除」关掉，
    // 状态栏仍保留短摘要（两者读 state.error 同一来源）。
    //
    // Top-level error banner, centred at the top of the canvas: a 24 px status bar
    // truncates long text, and the user's screenshot proves he never saw it. The canvas
    // top is the first place the eye lands, on a danger fill with white text; clicking
    // × or "清除" dismisses it. The status bar keeps its short summary -- both read the
    // same `state.error`.
    if let Some(err) = state.error.clone() {
        let g = p.layout_no_wrap(err.to_owned(), font(SIZE_LABEL), on_accent());
        let pad_x = 12.0;
        let close_w = 22.0;
        let w = (g.size().x + pad_x * 2.0 + close_w).min(full.width() * 0.86);
        let h = 30.0;
        let bx = full.center().x - w * 0.5;
        let by = full.top() + 6.0;
        let bar = Rect::from_min_size(Pos2::new(bx, by), Vec2::new(w, h));
        p.rect_filled(bar, radius_control(), danger());
        p.galley(
            Pos2::new(bar.left() + pad_x, bar.center().y - g.size().y * 0.5),
            g,
            on_accent(),
        );
        // 关闭按钮：× 字形（悬停提亮），命中区 22px。
        let cx = Rect::from_center_size(
            Pos2::new(bar.right() - close_w * 0.5 - 4.0, bar.center().y),
            Vec2::splat(close_w),
        );
        let xresp = ui.interact(cx, ui.scope_id().with("err-banner-close"), Sense::click());
        p.text(
            cx.center(),
            Align2::CENTER_CENTER,
            "✕",
            font(SIZE_SMALL),
            on_accent(),
        );
        if xresp.hovered() {
            p.rect_filled(cx, radius_control(), danger().gamma_multiply(0.78));
        }
        if xresp.clicked() {
            state.error = None;
        }
        xresp.on_hover_text(crate::i18n::t!("关闭这条错误提示"));
    }

    // 文档页签与读数已搬进选项行（顶部三层 chrome 压成两层）：画布从 CentralPanel
    // 顶边整块开始，图像多得到 28px 高。
    //
    // The document tab and readouts moved into the options bar, folding three rows of top
    // chrome into two; the canvas now starts at the CentralPanel's top edge and the image
    // gains 28 px of height.
    let body = full;

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

    let (tex, orig, size, fast_preview) = texture_choice(state);

    let (iw, ih) = if size[0] > 0 {
        (size[0] as f32, size[1] as f32)
    } else {
        (16.0, 9.0)
    };
    let avail = content.shrink(CANVAS_PAD);
    let fit = (avail.width() / iw).min(avail.height() / ih).max(0.01);
    state.fit_scale = fit;
    let scale = fit * state.zoom;
    let shown = Vec2::new(iw, ih) * scale;
    // 收口 pan：滚轮锚点 / 中键拖动 / Zoom 点击 / 此处使用四个写入点，统一在下一帧归位。
    //
    // Collect pan: the wheel anchor, middle drag, zoom tap and this use are the four write
    // sites; all of them are normalised here on the next frame.
    let pan = clamp_pan(shown, avail, state.pan);
    state.pan = pan;
    let center = content.center() + pan;
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
                } else if let Some(snap) = super::mask::mask_body_geo(state, img, pos) {
                    // 抓在形状主体上：整体移动，不平移图片（LR 同款手感）。
                    state.mask_drag = Some((sel, super::mask::HANDLE_MOVE));
                    state.mask_move_anchor = Some((pos, snap));
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
    // 双击空白处回到「适应窗口」。四角编辑态里双击要留给「把这个角放回原位」，
    // 所以这一下让给下面的把手；否则拖歪一个角想复位，画面反而缩走了。
    //
    // Double-clicking empty space fits the view to the window. In corner-edit mode the
    // double-click belongs to "put this corner back", so the handles below claim it; otherwise
    // resetting a corner would also shrink the view away.
    if resp.double_clicked() && !state.upright_editing {
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
            Tool::Heal => {
                // 不能落点时给「禁止」，比给一个看起来能画的光标要好——后者会让人
                // 在错的位置点半天才发现什么都没发生。
                //
                // A "not allowed" cursor when a tap cannot land beats one that looks ready:
                // the latter has people tapping the wrong place before noticing nothing happens.
                if state.heal_can_place() {
                    CursorIcon::Crosshair
                } else {
                    CursorIcon::NotAllowed
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
                        .map(|_| ())
                        .or_else(|| super::mask::mask_body_geo(state, img, pos).map(|_| ()))
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

    // 投影不外扩（用户截图：四周露出一圈黑板读作「黑边」）——只向下方偏移 5px
    // 露出一条柔影，左右上三面贴住图像；画面与背景的区分交给 1px 细描边。
    //
    // 放大后 `img` 会越出内容区：投影 / 描边 / 分屏原图 / 分割线一律裁到 `content`，
    // 否则会在画布工具条与标尺上盖出一条黑带（用户截图「放大后上面一边有黑边」）。
    //
    // The drop shadow no longer expands (user screenshot: the exposed slab read as a
    // "black border" on all four sides) -- it only offsets downward so a soft edge
    // shows beneath; the 1 px hairline separates frame from background.
    //
    // Once zoomed, `img` overflows the content rect: the shadow, border, split original,
    // divider and chips are all clipped to `content`, or they paint a dark band over the
    // canvas toolbar and rulers (user screenshot: "one side has a black band at the top").
    let cp = p.clone().with_clip_rect(content);
    cp.rect_filled(
        img.translate(Vec2::new(0.0, 5.0)),
        0,
        Color32::from_black_alpha(55),
    );
    cp.rect_stroke(
        img,
        0,
        Stroke::new(1.0, border_strong()),
        StrokeKind::Outside,
    );

    match &tex {
        Some(t) => {
            // 图像裁到内容区：放大后画布外的像素不画，避免上边缘顶到工具条 / 标尺。
            //
            // Clip the image to the content rect: zoomed-in pixels outside the canvas are
            // not drawn, so the image never pushes up into the toolbar / rulers.
            cp.image(t.id(), img, UV, Color32::WHITE);
            if let Some(o) = &orig {
                // 分割线位置跟着 `split_pos`（0..1）走，按住手柄左右拉；LR 的
                // 前后对比就是这一种——不是钉死在中间的一条死线。
                let x = img.min.x + img.width() * state.split_pos.clamp(0.02, 0.98);
                let left = Rect::from_min_max(img.min, Pos2::new(x, img.max.y)).intersect(content);
                cp.clone()
                    .with_clip_rect(left)
                    .image(o.id(), img, UV, Color32::WHITE);
                // 分割线只在画面还没被裁出去时才画；放大到线落在可视区外就整条收掉，
                // 免得它贴着工具条边缘画成一条白边。
                //
                // The divider is drawn only while it still falls inside the canvas; once
                // zoomed past it, the whole overlay is dropped rather than pinned to the edge.
                let line_visible = x > content.left() && x < content.right();
                if line_visible {
                    cp.line_segment(
                        [
                            Pos2::new(x, img.top().max(content.top())),
                            Pos2::new(x, img.bottom().min(content.bottom())),
                        ],
                        Stroke::new(1.0, Color32::from_white_alpha(200)),
                    );
                }
                if line_visible {
                    // 拖动手柄：分割线中段一枚圆形抓手；纵向位置钉在**可见区域**的
                    // 中段——放大后整幅图的中线可能已经在画布外，用图心的 y 会让手柄
                    // 消失。双击回到居中。
                    //
                    // Drag handle: a round grip on the divider's middle, pinned to the
                    // middle of the *visible* area; once zoomed the image's own center may
                    // sit outside the canvas and a handle anchored to it disappears.
                    let gy = img
                        .center()
                        .y
                        .clamp(content.top() + 14.0, content.bottom() - 14.0);
                    let grip = Rect::from_center_size(Pos2::new(x, gy), Vec2::new(18.0, 28.0));
                    let gresp = ui.interact(
                        grip,
                        ui.make_persistent_id("split-grip"),
                        Sense::click_and_drag(),
                    );
                    if gresp.dragged()
                        && let Some(pt) = gresp.interact_pointer_pos()
                    {
                        state.split_pos =
                            ((pt.x - img.min.x) / img.width().max(1.0)).clamp(0.02, 0.98);
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
                    cp.circle_filled(
                        grip.center() + Vec2::new(0.0, 1.0),
                        11.0,
                        Color32::from_black_alpha(90),
                    );
                    cp.circle_filled(grip.center(), 11.0, gcol);
                    cp.circle_stroke(
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
                    cp.add(Shape::convex_polygon(
                        arrow(-3.4).to_vec(),
                        Color32::WHITE,
                        Stroke::NONE,
                    ));
                    cp.add(Shape::convex_polygon(
                        arrow(5.2).to_vec(),
                        Color32::WHITE,
                        Stroke::NONE,
                    ));
                    // 角标也钉在可见区的顶缘，而不是图顶：放大后图顶在工具条上方。
                    //
                    // The "before / after" chips pin to the visible area's top edge, not
                    // the image's: zoomed in, the image top sits above the toolbar.
                    let chip_y = content.top() + 8.0;
                    chip_label(&cp, Pos2::new(x - 8.0, chip_y), "前", Align2::RIGHT_TOP);
                    chip_label(&cp, Pos2::new(x + 8.0, chip_y), "后", Align2::LEFT_TOP);
                }
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

    // 快速预览标识：播放中标出当前是较大尺寸代理，非全质量显影预览。
    //
    // Fast-preview badge: during playback, mark that this is the larger proxy rather than
    // the full-quality developed preview.
    if fast_preview {
        // 徽标也裁到内容区：放大后 `img` 可能越界，徽标不该压到工具条上。
        //
        // Clip the badge to the content rect too: once zoomed in `img` can overflow, and the
        // badge must not sit on top of the toolbar.
        chip_label(
            &p.clone().with_clip_rect(content),
            Pos2::new(img.left() + 10.0, img.top() + 10.0),
            crate::i18n::t!("快速预览"),
            Align2::LEFT_TOP,
        );
    }

    if state.crop_editing() {
        crop_overlay(state, ui, &p, img, &resp);
    } else if state.motion_editing() {
        motion_overlay(state, ui, &p, img, &resp);
    } else if state.module == Module::Develop && state.upright_editing {
        // 四角排在修复画笔前面：它是面板上明确的开关，开着的时候画布归它。
        // 选画笔时 `set_tool` 会自动把它关掉，所以两者不会同时想要这一只手。
        //
        // Corners come before the repair brush: this is an explicit toggle on the panel, and
        // while it is on the canvas belongs to it. Picking the brush switches it off in
        // `set_tool`, so the two never compete for the same hand.
        upright_overlay(state, ui, &p, img);
    } else if state.module == Module::Develop && state.tool == Tool::Heal {
        heal_overlay(state, &p, img, &resp);
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

    // 辅看开关浮在画布右下角（原先是窗口右缘的 28px 竖条）。
    //
    // The overlay toggles float at the canvas's bottom-right corner (they used to be a
    // 28 px rail on the window's right edge).
    super::overlay_toggles(state, ui, content);

    if state.rulers {
        draw_rulers(ui, body, content, Some(img), scale, hover);
    }
}

/// 把平移量收口到「图像不越出内容区」的范围：图像比可视区小则强制居中（偏差 0px），
/// 否则每轴 clamp 到 ±(图像 - 可视区)/2。四个写 pan 的入口统一在下一帧被这里收口。
///
/// Clamp the pan so the image never slides out of the content rect: when the image is
/// smaller than the viewport it is pinned to the centre (zero geometric offset), otherwise
/// each axis is clamped to ±(shown - avail)/2. All four pan writes are collected here on
/// the next frame.
fn clamp_pan(shown: Vec2, avail: Rect, pan: Vec2) -> Vec2 {
    let axis = |s: f32, a: f32, p: f32| {
        if s <= a {
            0.0
        } else {
            p.clamp(-(s - a) / 2.0, (s - a) / 2.0)
        }
    };
    Vec2::new(
        axis(shown.x, avail.width(), pan.x),
        axis(shown.y, avail.height(), pan.y),
    )
}

/// 选择本帧要画的纹理：仅在播放中用较大尺寸代理，其余一律用全质量已显影预览（旧纹理
/// 一直留着，新预览渲染完成才替换，天然无空窗）。返回 (主纹理, 分屏原图, 尺寸, 是否快速预览)。
///
/// Pick the texture for this frame: use the larger proxy only during playback, otherwise
/// always the full-quality developed preview (the old texture stays until the new preview
/// finishes, so there is no blank frame). Returns (main, split original, size, is_fast_preview).
fn texture_choice(
    state: &AppState,
) -> (Option<TextureHandle>, Option<TextureHandle>, [u32; 2], bool) {
    if state.playing {
        // 播放中：代理命中就用代理，冷启动没代理则退回缩略图，保证不 blank。
        //
        // During playback: use the proxy when present, else fall back to the thumbnail on a
        // cold start so the frame never blanks.
        let (tex, size) = match state.proxies.get(&state.selected) {
            Some(t) => (Some(t.texture.clone()), t.size),
            None => match state.thumb(state.selected) {
                Some(t) => (Some(t.texture.clone()), t.size),
                None => (None, [0, 0]),
            },
        };
        return (tex, None, size, true);
    }
    // 非播放：一律全质量（保持原行为）。不做 preview_dirty 判断——那字段平时就为真，
    // 一改参数就切代理反而更糊；旧的全质量纹理留到新预览渲染完成，天然没有空窗。
    //
    // Not playing: always full quality (unchanged behaviour). The preview_dirty flag is true
    // in normal use, so switching to a proxy on it would only blur the canvas; the old
    // full-quality texture stays until the new preview finishes, so there is no blank frame.
    (
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
        false,
    )
}

/// 透视四角：画出四边形与四个把手，拖动即改参数。
///
/// 每秒都在变的东西只有矢量：四个角的屏幕位置由参数算出，拖动只改参数、由预览那边重画，
/// 所以和裁剪框一样跟手。把手各用一份 `ui.interact`，于是每个角能独立拖、独立双击复位，
/// 也不必和画布整体的「双击适应窗口」抢——那一处在编辑态里已经让开了。
///
/// The perspective quad: draws the quadrilateral and its four handles; dragging edits the
/// parameters.
///
/// Only vector work happens per frame: the four screen positions derive from the parameters,
/// and a drag only writes parameters for the preview side to redraw, so it keeps up just like
/// the crop box. Each handle owns a `ui.interact`, which lets corners drag and reset
/// independently and keeps them clear of the canvas-wide "double-click to fit" -- that one
/// steps aside while editing.
fn upright_overlay(state: &mut AppState, ui: &mut Ui, p: &egui::Painter, img: Rect) {
    let screen: Vec<Pos2> = state
        .upright_corners()
        .iter()
        .map(|(cu, cv)| Pos2::new(img.left() + cu * img.width(), img.top() + cv * img.height()))
        .collect();

    // 四边形：先压一道半透明黑，再压强调色。亮画面（天空、白墙）与暗画面上都得看得见，
    // 只画一种颜色的话总有一半照片里它消失了。
    //
    // The quadrilateral: a translucent black pass under the accent pass, so it stays visible
    // on both bright frames (sky, white walls) and dark ones -- one colour alone disappears in
    // half of all photos.
    for (a, b) in [
        (screen[0], screen[1]),
        (screen[1], screen[2]),
        (screen[2], screen[3]),
        (screen[3], screen[0]),
    ] {
        p.line_segment([a, b], Stroke::new(3.2, Color32::from_black_alpha(90)));
        p.line_segment([a, b], Stroke::new(1.4, accent()));
    }

    for (i, c) in screen.iter().copied().enumerate() {
        let rect = Rect::from_center_size(c, Vec2::splat(24.0));
        let g = ui.interact(
            rect,
            ui.make_persistent_id(("upright-corner", i)),
            Sense::click_and_drag(),
        );
        if g.dragged()
            && let Some(pos) = g.interact_pointer_pos()
        {
            state.upright_drag_corner(
                i,
                f64::from((pos.x - img.left()) / img.width().max(1.0)),
                f64::from((pos.y - img.top()) / img.height().max(1.0)),
            );
        }
        if g.drag_stopped() {
            state.upright_commit();
        }
        if g.double_clicked() {
            state.upright_reset_corner(i);
        }
        let hot = g.hovered() || g.dragged();
        if hot {
            ui.ctx().set_cursor_icon(if g.dragged() {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            });
        }
        let fill = if g.dragged() {
            accent()
        } else if hot {
            elev_hover()
        } else {
            Color32::from_rgba_unmultiplied(24, 24, 26, 230)
        };
        p.circle_filled(c + Vec2::new(0.0, 1.0), 7.5, Color32::from_black_alpha(90));
        p.circle_filled(c, 6.0, fill);
        p.circle_stroke(c, 6.0, Stroke::new(1.0, Color32::from_white_alpha(200)));
    }
}

/// 修复画笔层：落点、拖动补点，并把已落的笔触与仿制源画在画面上。
///
/// Healing-brush layer: lay strokes, top them up while dragging, and draw the placed strokes
/// and the clone source over the image.
///
/// 为什么不在拖动中实时预览「这一笔会变成什么样」：修复要解泊松方程，一个点几毫秒，
/// 拖动时每帧重算是拿不准的买卖；而修复本身是**立即生效**的（松手前预览已经带着
/// 这一笔重画过了），所以画一圈轮廓提示位置就够，用户看到的结果就是真的结果。
///
/// Why not preview "what this stroke will look like" mid-drag: healing solves a Poisson
/// equation, a few milliseconds per spot, and recomputing every frame is a bad trade. The
/// repair already takes effect immediately (the preview redraws with the stroke before the
/// mouse is released), so an outline marking the position is enough — what the user sees is
/// the real result.
fn heal_overlay(state: &mut AppState, p: &egui::Painter, img: Rect, resp: &Response) {
    let long_px = img.width().max(img.height()).max(1.0);
    let to_screen = |state: &AppState, u: f64, v: f64| {
        let (cu, cv) = state.frame_to_canvas(u, v);
        Pos2::new(
            img.left() + cu as f32 * img.width(),
            img.top() + cv as f32 * img.height(),
        )
    };

    // 已落的笔触：最后一笔用强调色——撤销撤的就是它，得让人看见撤掉哪一个。
    //
    // Placed strokes: the last one takes the accent colour, because that is the one an undo
    // removes and it should be visible which one that is.
    let n = state.params.retouch.len();
    for (i, s) in state.params.retouch.iter().enumerate() {
        let c = to_screen(state, f64::from(s.x), f64::from(s.y));
        let r = (f64::from(s.r) * f64::from(long_px)) as f32;
        let col = if i + 1 == n {
            accent()
        } else {
            Color32::from_white_alpha(110)
        };
        p.circle_stroke(c, r.max(2.0), Stroke::new(1.0, col));
    }

    // 仿制源标记：十字。取源态（还没取）不画，因为此时画的是「下一次点击要取源」。
    //
    // Clone source marker: a cross. Nothing is drawn before a source is picked, because at
    // that point the next tap is the one that picks it.
    if state.heal_clone
        && let Some((su, sv)) = state.heal_src
    {
        let c = to_screen(state, f64::from(su), f64::from(sv));
        let st = Stroke::new(1.2, accent());
        p.line_segment([c - Vec2::new(7.0, 0.0), c + Vec2::new(7.0, 0.0)], st);
        p.line_segment([c - Vec2::new(0.0, 7.0), c + Vec2::new(0.0, 7.0)], st);
    }

    // 悬停圈：半径就是下一次落笔的半径（改推子立刻看得到）。
    //
    // The hover ring: its radius is what the next stroke will use, so moving the slider shows
    // up at once.
    if let Some(pos) = resp.hover_pos().filter(|q| img.contains(*q)) {
        let r = (state.heal_radius / 100.0 * f64::from(long_px)) as f32;
        p.circle_stroke(
            pos,
            r.max(2.0),
            Stroke::new(1.0, Color32::from_white_alpha(80)),
        );
    }

    if resp.drag_stopped() {
        state.heal_last = None;
    }
    if !state.heal_can_place() {
        return;
    }
    let Some(pos) = resp.interact_pointer_pos().filter(|q| img.contains(*q)) else {
        return;
    };
    // 一次按下会同时报 drag_started 与 clicked 两件事，按下那一刻只许落一笔。
    //
    // One press reports both drag_started and clicked; only one stroke may land from it.
    if let Some(last) = state.heal_last
        && last.distance(pos) < 2.0
    {
        return;
    }
    if resp.drag_started_by(PointerButton::Primary) || resp.clicked_by(PointerButton::Primary) {
        heal_place(state, img, pos);
    } else if resp.dragged_by(PointerButton::Primary) {
        // 拖动补点的间距取半径的 0.6 倍（下限 4 屏幕像素）：太密会让每一帧都要解几次
        // 泊松方程，拖起来像在泥里走。
        //
        // The drag spacing is 0.6 of the radius (floor of 4 screen pixels): denser would solve
        // the Poisson equation several times per frame and drag like walking through mud.
        let step = ((state.heal_radius / 100.0 * f64::from(long_px)) as f32 * 0.6).max(4.0);
        if state
            .heal_last
            .is_none_or(|last| last.distance(pos) >= step)
        {
            heal_place(state, img, pos);
        }
    }
}

/// 把一个画布点落成笔触：换算到整幅归一化坐标再交给状态层。
///
/// Turn a canvas point into a stroke: convert to whole-frame normalized coordinates and hand
/// it to the state layer.
fn heal_place(state: &mut AppState, img: Rect, pos: Pos2) {
    let u = f64::from((pos.x - img.left()) / img.width().max(1.0));
    let v = f64::from((pos.y - img.top()) / img.height().max(1.0));
    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
        return;
    }
    if state.heal_add(state.canvas_to_frame(u, v)) {
        ui_note(state);
    }
    state.heal_last = Some(pos);
}

/// 落笔后的界面提示：第一笔顺带说一次「这是修复，不是橡皮」，之后不再打扰。
///
/// A UI hint after a stroke: the first one also says "this is repair, not an eraser"; later
/// strokes stay quiet.
fn ui_note(state: &mut AppState) {
    if state.params.retouch.len() == 1 {
        state.auto_note =
            Some(crate::i18n::t!("修复会按周边像素重新生成，不会把斑点抹成一块灰").to_owned());
    }
}

/// 裁剪态的取景框：拖拽交互 + 框外压暗 + 三分线 + 八向把手 + 尺寸标签。
///
/// 这一层全是矢量：拖动只挪框，一个像素都不重画，所以 24MP 的图也跟手。
/// 取景态下预览画的是整块可见区（`state.crop_view()`，拉直后即安全框），于是屏幕和
/// 那幅像素只剩一层等比缩放，换算就在下面两个闭包里。
///
/// The crop box: drag interaction, dimming outside the box, thirds, eight handles and a size
/// label.
///
/// The whole layer is vector: dragging only moves the box and repaints not a single pixel, so
/// a 24MP frame keeps up. In crop-edit mode the preview draws the entire visible area
/// (`state.crop_view()`, which after straightening is the safety frame), so screen and pixels
/// differ by one uniform scale -- the conversion lives in the two closures below.
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
        open_dialog(state);
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
        open_photos_dialog(state);
    }
}

/// 画布顶栏中段的画幅读数（纯函数以便判据直测）：带「画幅」标签把 W × H 框成
/// 尺寸——× 是乘号，不是关闭叉（评审 E3 / 用户反馈 #7）；位深来自 RAW/DNG
/// 注记，普通文件不显示。未打开序列时退成一段占位横线。
///
/// The canvas top-bar frame-size readout (pure so the gate can test it directly):
/// the label frames W × H as a dimension -- × is a multiplication sign, not a close
/// cross (review E3 / user feedback #7). Bit depth comes from the RAW/DNG note and is
/// hidden for plain files. With no sequence open it degrades to a placeholder dash.
pub(super) fn frame_size_text(res: Option<(u32, u32)>, bits: Option<u32>) -> String {
    let Some((w, h)) = res else {
        return "—".to_owned();
    };
    match bits {
        Some(b) => format!("{} {w} × {h} · {b} bit", crate::i18n::t!("画幅")),
        None => format!("{} {w} × {h}", crate::i18n::t!("画幅")),
    }
}

#[cfg(test)]
mod frame_size_tests {
    use super::*;

    #[test]
    fn frame_size_readout_carries_the_label_and_bit_depth() {
        // 有位深：标签 + 尺寸 + 位深三段全在。
        let s = frame_size_text(Some((6000, 4000)), Some(16));
        assert!(
            s.starts_with(crate::i18n::t!("画幅")),
            "顶栏画幅读数缺「画幅」标签：{s}"
        );
        assert!(s.contains("6000 × 4000") && s.contains("16 bit"), "{s}");

        // 普通文件没有位深段，也不该冒出 bit。
        let s = frame_size_text(Some((1280, 720)), None);
        assert!(
            s.starts_with(crate::i18n::t!("画幅")) && s.ends_with("1280 × 720"),
            "{s}"
        );
        assert!(!s.contains("bit"), "{s}");

        // 未打开序列：占位横线，不画标签。
        assert_eq!(frame_size_text(None, Some(16)), "—");
    }
}
