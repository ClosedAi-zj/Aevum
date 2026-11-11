//! 帧表左列：预览卡 + 摘要卡 + 自动过渡卡。
//!
//! 设计稿 `#vTable .seqcol`：`.seqprev`（预览图 + 相机/亮度徽标 + 文件名/#序号 + 走带）、
//! `.seqsum`（序列摘要键值行 + 关键帧芯片）、`.seqsum`（自动过渡分段表）。
//! 三张卡同宽、圆角 12、间距 12、卡底 `panel`。
//!
//! Left column of the frame table, per `#vTable .seqcol`: the preview card (image plus
//! camera/luma badges, caption and transport), the sequence summary (key/value rows and
//! keyframe chips) and the auto-transition segment table.

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

use crate::state::AppState;
use crate::theme::*;

/// 卡间距 12（`.seqcol{gap:12px}`）。
///
/// Gap between cards.
const GAP: f32 = 12.0;
/// 卡内距 12（`.seqsum{padding:10px 12px}`，横向取 12）。
///
/// Card inner padding.
const PAD: f32 = 12.0;
/// 走带高度 34（`.seqprev .tp{height:34px}`）。
///
/// Transport row height.
const TP_H: f32 = 34.0;
/// 摘要行高 22（设计 23，这里按 22 收一档，与帧表行高同一节奏）。
///
/// Summary row height; the design uses 23, stepped to 22 to share the table's rhythm.
const KV_H: f32 = 22.0;

/// 画整列两张卡（用户 2026-11-10：除显影外分模块太多）。
///
/// 预览卡 + 一张「序列信息」卡：摘要键值与自动过渡同属「这条序列现在的状态」，
/// 拆成两张卡只是多了两层壳——合并后一条分节线足够分层，左列从三张卡减到两张。
///
/// Draw the column as two cards (user, 2026-11-10: too many module boxes outside
/// develop). The preview plus one "sequence info" card: the summary key/values and the
/// auto-transition segments both describe "where this sequence stands", so the extra
/// card shell bought nothing -- one section rule layers them just as well.
pub(super) fn column(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    if rect.width() < 40.0 || rect.height() < 80.0 {
        return;
    }
    // 设计稿里预览卡是 `flex:1`（吃剩余高度）。这里给预览一个上限：4:3 起步、
    // 最多 7:5——300px 宽的列若照抄 `flex:1`，1000px 高的窗口会把横构图裁成
    // 一条竖片，读数全错。剩下的高度归信息卡。
    //
    // The design lets the preview card flex; here it is clamped between 4:3 and 7:5.
    // Copying `flex:1` at a 300 px column and a 1000 px window would crop a landscape
    // frame into a vertical strip. The leftover height goes to the info card.
    let summary_h = summary_height(state);
    let slack = rect.height() - summary_h - GAP - TP_H;
    // 预览窗高度优先**贴合已知画幅**（分辨率 → w×h/w）：Fit 之下窗子比照片高
    // 出的部分全是灰底，贴合影后照片占满预览窗，剩余高度让给信息卡。分辨率
    // 未知才回落到 4:3–7:5 的旧夹法。
    //
    // The preview height first **hugs the known aspect** (resolution -> w*h/w): under
    // Fit every pixel above the photo is grey matte, so hugging fills the window and
    // the leftover goes to the info card. Fall back to the old 4:3-7:5 clamp only
    // when the resolution is unknown.
    let ideal = state
        .resolution
        .map(|(rw, rh)| rect.width() * rh as f32 / rw.max(1) as f32);
    let img_h = match ideal {
        Some(h) => h.min(slack).max(90.0),
        None => slack
            .clamp(rect.width() * 0.75, rect.width() * 1.4)
            .max(120.0),
    };

    let prev_h = preview_card(
        state,
        ui,
        Rect::from_min_size(rect.left_top(), Vec2::new(rect.width(), 0.0)),
        img_h,
    );
    let y = rect.top() + prev_h + GAP;
    info_card(
        state,
        ui,
        Pos2::new(rect.left(), y),
        rect.width(),
        (rect.bottom() - y).max(96.0),
    );
}

/// 摘要块高度：10 上距 + 22 标题 + 2 + 五行 × 22 + 芯片行 + 10 下距。
///
/// Summary block height: padding, title, five rows and the chip row.
fn summary_height(state: &AppState) -> f32 {
    let chips = if state.keyframes.is_empty() {
        0.0
    } else {
        25.0
    };
    10.0 + 22.0 + 2.0 + 5.0 * KV_H + chips + 10.0
}

/// 预览卡：预览图 + 叠印徽标/文件名 + 走带。返回卡片高度。
///
/// Preview card: the image with overlaid badges and caption, plus the transport row.
fn preview_card(state: &mut AppState, ui: &mut Ui, at: Rect, img_h: f32) -> f32 {
    let w = at.width();
    let card = Rect::from_min_size(at.left_top(), Vec2::new(w, img_h + TP_H));
    let p = ui.painter().clone();
    p.rect_filled(card, radius_panel(), panel());
    p.rect_stroke(
        card,
        radius_panel(),
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );

    let n = state.frame_count();
    let sel = if n == 0 { 0 } else { state.selected.min(n - 1) };
    let img = Rect::from_min_size(card.left_top(), Vec2::new(w, img_h));
    // Fit 的衬底用中性灰 matte（与主画布同一口径），不用纯黑：黑边会被读成
    // 「视频坏了」，灰边读作「画幅比预览窗宽」。
    //
    // The Fit letterbox uses the neutral matte (same rule as the main canvas), not
    // pure black: black reads as a broken stream, grey reads as a wider frame.
    p.rect_filled(
        img,
        CornerRadius {
            nw: 12,
            ne: 12,
            sw: 0,
            se: 0,
        },
        matte(),
    );

    // 预览优先用 640px 播放代理，回落到 128px 缩略图：卡宽 276，128px 已经糊成一片。
    //
    // The preview prefers the 640 px playback proxy and falls back to the 128 px thumbnail:
    // at a 276 px card width the small one is visibly soft.
    if n > 0 {
        state.ensure_proxies(ui.ctx(), sel, sel + 1);
        state.ensure_thumbs(ui.ctx(), sel, sel + 1);
    }
    let tex = state
        .proxies
        .get(&sel)
        .or_else(|| state.thumb(sel))
        .map(|t| (t.texture.id(), t.size));
    if let Some((id, size)) = tex {
        let sw = size[0].max(1) as f32;
        let sh = size[1].max(1) as f32;
        // object-fit:contain（Fit，与主画布 `.min(1.0)` 同一条规矩）：整幅可见、
        // 等比、不裁切。此前的 cover 把 3:2 横幅塞进竖长预览窗，两侧裁掉大半，
        // 用户读成「预览的视频画面比例不对」（2026-11-10）。
        //
        // object-fit: contain (Fit, the same rule as the main canvas's `.min(1.0)`):
        // the whole frame stays visible, proportional, uncropped. cover used to jam
        // a 3:2 landscape into the tall preview window, cropping most of both sides
        // -- read as "the preview video's aspect ratio is wrong" (2026-11-10).
        let scale = (img.width() / sw).min(img.height() / sh);
        let draw = Rect::from_center_size(img.center(), Vec2::new(sw * scale, sh * scale));
        let ip = p.with_clip_rect(img.intersect(p.clip_rect()));
        ip.image(
            id,
            draw,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    // 上下压两层深影，徽标与文件名才读得出来（设计稿 `.pv::after` 的渐变）。
    //
    // Two dark bands top and bottom, so the badges and the caption stay readable.
    for k in 0..3 {
        let band = 12.0;
        let top = img.bottom() - (k + 1) as f32 * band;
        if top < img.top() {
            break;
        }
        p.rect_filled(
            Rect::from_min_size(Pos2::new(img.left(), top), Vec2::new(img.width(), band)),
            0.0,
            Color32::from_black_alpha(34 - k as u8 * 9),
        );
        let ty = img.top() + k as f32 * band;
        p.rect_filled(
            Rect::from_min_size(Pos2::new(img.left(), ty), Vec2::new(img.width(), band)),
            0.0,
            Color32::from_black_alpha(16 - k as u8 * 5),
        );
    }

    // 左上「相机预览」、右上「亮度 0.213」（设计稿 `.badge` / `.luma`）。
    //
    // Camera badge top-left, luma readout top-right.
    let bfont = font(11.0);
    let gl = p.layout_no_wrap(
        crate::i18n::t!("相机预览").to_owned(),
        bfont.clone(),
        muted(),
    );
    let badge = Rect::from_min_size(
        Pos2::new(img.left() + 10.0, img.top() + 10.0),
        Vec2::new(gl.size().x + 28.0, 20.0),
    );
    p.rect_filled(badge, radius_chip(), badge_fill());
    p.circle_filled(
        Pos2::new(badge.left() + 10.0, badge.center().y),
        2.5,
        accent(),
    );
    p.galley(
        Pos2::new(badge.left() + 18.0, badge.center().y - gl.size().y * 0.5),
        gl,
        muted(),
    );

    let luma = state
        .metrics
        .get(sel)
        .and_then(|m| m.as_ref())
        .map(|m| format!("{:.3}", m.mean));
    if let Some(v) = luma {
        let lw = p.layout_no_wrap(crate::i18n::t!("亮度").to_owned(), bfont.clone(), dim());
        let vw = p.layout_no_wrap(v, bfont.clone(), text());
        let lw_w = lw.size().x;
        let bw = lw_w + vw.size().x + 20.0;
        let b = Rect::from_min_size(
            Pos2::new(img.right() - 10.0 - bw, img.top() + 10.0),
            Vec2::new(bw, 20.0),
        );
        p.rect_filled(b, radius_chip(), badge_fill());
        p.galley(
            Pos2::new(b.left() + 7.0, b.center().y - lw.size().y * 0.5),
            lw,
            dim(),
        );
        p.galley(
            Pos2::new(b.left() + 11.0 + lw_w, b.center().y - vw.size().y * 0.5),
            vw,
            text(),
        );
    }

    // 文件名（左）+ `#0001`（右），贴在图片底部 10px 内（设计稿 `.cap`）。
    //
    // File name left, `#0001` right, inset 10 px from the image bottom.
    let name = state
        .seq
        .as_ref()
        .and_then(|s| s.frames.get(sel))
        .map(|f| f.name.clone())
        .unwrap_or_default();
    let nfont = bold(12.0);
    let name = fit_text(&p, &name, nfont.clone(), text(), img.width() - 80.0);
    p.text(
        Pos2::new(img.left() + 10.0, img.bottom() - 10.0),
        Align2::LEFT_BOTTOM,
        name,
        nfont,
        text(),
    );
    p.text(
        Pos2::new(img.right() - 10.0, img.bottom() - 10.0),
        Align2::RIGHT_BOTTOM,
        format!("#{:04}", sel + 1),
        font(11.0),
        muted(),
    );

    // 走带：◀ ▶ + `0001 / 1387` + 右侧帧率/时间码（设计稿 `.tp`）。
    //
    // Transport: prev/next, frame counter and the rate/timecode readout.
    let bar = Rect::from_min_size(Pos2::new(card.left(), img.bottom()), Vec2::new(w, TP_H));
    p.line_segment(
        [
            Pos2::new(bar.left(), bar.top() + 0.5),
            Pos2::new(bar.right(), bar.top() + 0.5),
        ],
        Stroke::new(1.0, divider()),
    );
    let mut x = bar.left() + 7.0;
    let prev = transport_button(ui, &p, Pos2::new(x, bar.center().y), true, sel > 0);
    x = prev.rect.left() + 26.0;
    let next = transport_button(
        ui,
        &p,
        Pos2::new(x, bar.center().y),
        false,
        n > 0 && sel + 1 < n,
    );
    if prev.clicked() && sel > 0 {
        state.selected = sel - 1;
    }
    if next.clicked() && sel + 1 < n {
        state.selected = sel + 1;
    }

    let fps = state.output_fps.max(0.0);
    p.text(
        Pos2::new(next.rect.left() + 32.0, bar.center().y),
        Align2::LEFT_CENTER,
        format!("{:04} / {}", sel + 1, n),
        font(11.0),
        dim(),
    );
    let secs = if fps > 0.1 { sel as f32 / fps } else { 0.0 };
    let mm = (secs / 60.0).floor() as u32;
    let ss = secs - mm as f32 * 60.0;
    p.text(
        Pos2::new(bar.right() - 7.0, bar.center().y),
        Align2::RIGHT_CENTER,
        format!("{fps:.0} fps · {mm:02}:{ss:05.2}"),
        font(11.0),
        dim(),
    );

    card.height()
}

/// 走带上的单枚箭头按钮（24×24，悬停抬一档）。返回响应。
///
/// One transport arrow button (24×24, hover raises one step).
fn transport_button(
    ui: &mut Ui,
    p: &egui::Painter,
    center_left: Pos2,
    left: bool,
    enabled: bool,
) -> egui::Response {
    let r = Rect::from_min_size(
        Pos2::new(center_left.x, center_left.y - 12.0),
        Vec2::new(24.0, 24.0),
    );
    let resp = ui.interact(
        r,
        ui.make_persistent_id(("seq-tp", left)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if enabled && resp.hovered() {
        p.rect_filled(r, radius_chip(), elev());
    }
    let c = r.center();
    let pts = if left {
        vec![
            Pos2::new(c.x + 4.0, c.y - 5.5),
            Pos2::new(c.x + 4.0, c.y + 5.5),
            Pos2::new(c.x - 4.5, c.y),
        ]
    } else {
        vec![
            Pos2::new(c.x - 4.0, c.y - 5.5),
            Pos2::new(c.x - 4.0, c.y + 5.5),
            Pos2::new(c.x + 4.5, c.y),
        ]
    };
    let ink = if !enabled {
        dim()
    } else if resp.hovered() {
        text()
    } else {
        muted()
    };
    p.add(Shape::convex_polygon(pts, ink, Stroke::NONE));
    resp
}

/// 序列信息卡：摘要键值 + 关键帧芯片 + 分节线 + 自动过渡分段。
/// 两块信息合用一张卡（2026-11-10 减模块），高度吃满剩余。
///
/// Sequence info card: summary key/values, keyframe chips, a section rule, then the
/// auto-transition segments. Two blocks share one card (fewer module boxes,
/// 2026-11-10); the height fills what is left.
fn info_card(state: &mut AppState, ui: &mut Ui, at: Pos2, w: f32, h: f32) -> f32 {
    let n = state.frame_count();
    let sel = if n == 0 { 0 } else { state.selected.min(n - 1) };
    let fps = state.output_fps;

    let camera = state
        .exif
        .get(sel)
        .and_then(|e| e.model())
        .unwrap_or("")
        .to_owned();
    let (shutter, fnum) = state
        .exif
        .get(sel)
        .map(|e| (e.shutter_text(), e.fnum()))
        .unwrap_or((None, None));
    let shutter_ap = match (shutter, fnum) {
        (Some(s), Some(f)) => format!("{s} · f/{f:.1}"),
        (Some(s), None) => s,
        (None, Some(f)) => format!("f/{f:.1}"),
        (None, None) => "—".to_owned(),
    };
    let interval = if fps > 0.1 {
        let sec = 1.0 / fps;
        if sec >= 1.0 {
            format!("{sec:.1} s")
        } else {
            format!("{:.0} ms", sec * 1000.0)
        }
    } else {
        "—".to_owned()
    };
    let res = state
        .resolution
        .map(|(rw, rh)| format!("{rw} × {rh}"))
        .unwrap_or_else(|| "—".to_owned());
    let keys = state.keyframes.len();
    let segs_n = keys.saturating_sub(1);
    let chips: Vec<(usize, String)> = state
        .keyframes
        .keys()
        .copied()
        .take(8)
        .map(|i| (i, format!("K {:04}", i + 1)))
        .collect();

    let rows: [(&str, String); 5] = [
        ("总帧数", thousands(n)),
        ("间隔 / 帧率", format!("{interval} · {fps:.0} fps")),
        ("快门 / 光圈", shutter_ap),
        ("分辨率", res),
        (
            "关键帧 / 过渡",
            crate::i18n::tr_fmt("{} · {} 段", &[&keys, &segs_n]),
        ),
    ];

    let card = Rect::from_min_size(at, Vec2::new(w, h));
    let p = ui.painter().clone();
    p.rect_filled(card, radius_panel(), panel());
    p.rect_stroke(
        card,
        radius_panel(),
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );

    // 标题 12 + 右侧机型（设计稿 `.seqsum h3 span`）。
    //
    // 12 px title with the camera model on the right.
    p.text(
        Pos2::new(card.left() + PAD, card.top() + 10.0 + 11.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("序列信息"),
        bold(12.0),
        text(),
    );
    if !camera.is_empty() {
        p.text(
            Pos2::new(card.right() - PAD, card.top() + 10.0 + 11.0),
            Align2::RIGHT_CENTER,
            camera,
            font(11.0),
            dim(),
        );
    }

    let mut y = card.top() + 10.0 + 22.0 + 2.0;
    let last = rows.len().saturating_sub(1);
    for (idx, (k, v)) in rows.into_iter().enumerate() {
        let mid = y + KV_H * 0.5;
        p.text(
            Pos2::new(card.left() + PAD, mid),
            Align2::LEFT_CENTER,
            crate::i18n::tr(k),
            font(11.0),
            dim(),
        );
        let vfont = font(11.0);
        let v = fit_text(
            &p,
            &v,
            vfont.clone(),
            muted(),
            (w - PAD * 2.0 - 96.0).max(60.0),
        );
        p.text(
            Pos2::new(card.right() - PAD, mid),
            Align2::RIGHT_CENTER,
            v,
            vfont,
            muted(),
        );
        y += KV_H;
        // 设计稿 `.kv2:last-of-type{box-shadow:none}`：最后一行不收边。
        //
        // The design drops the shadow on the last row.
        if idx < last {
            p.line_segment(
                [
                    Pos2::new(card.left() + PAD, y - 0.5),
                    Pos2::new(card.right() - PAD, y - 0.5),
                ],
                Stroke::new(1.0, divider()),
            );
        }
    }

    // 关键帧芯片：点一下跳到那一帧（已有能力：只写 `selected`）。
    //
    // Keyframe chips jump to that frame — the only state they touch is `selected`.
    if !chips.is_empty() {
        let mut x = card.left() + PAD;
        let right = card.right() - PAD;
        let cy = y + 7.0 + 9.0;
        let mut drawn = 0usize;
        for (i, label) in chips {
            let g = p.layout_no_wrap(label, font(11.0), warn());
            let cw = g.size().x + 10.0;
            // 一行放不下就停：300px 宽的列最多容 5–6 枚，硬排会压到卡片外。
            //
            // Stop when the row is full: a 300 px column holds five or six chips at most.
            if x + cw > right {
                break;
            }
            let r = Rect::from_min_size(Pos2::new(x, cy - 9.0), Vec2::new(cw, 18.0));
            let resp = ui.interact(r, ui.make_persistent_id(("seq-kchip", i)), Sense::click());
            let a = warn();
            p.rect_filled(
                r,
                radius_chip(),
                Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 36),
            );
            p.galley(
                Pos2::new(r.left() + 5.0, r.center().y - g.size().y * 0.5),
                g,
                warn(),
            );
            if resp.clicked() {
                state.selected = i;
            }
            x = r.right() + 4.0;
            drawn += 1;
        }
        if drawn < keys {
            p.text(
                Pos2::new(x, cy),
                Align2::LEFT_CENTER,
                format!("+{}", keys - drawn),
                font(11.0),
                dim(),
            );
        }
        y += 25.0;
    }

    // 分节线 + 「自动过渡」小节：同一张卡内的第二层，不再单独包壳。
    //
    // Section rule + the auto-transition block: a second layer inside the same card,
    // no extra shell.
    y += 4.0;
    p.line_segment(
        [
            Pos2::new(card.left() + PAD, y - 0.5),
            Pos2::new(card.right() - PAD, y - 0.5),
        ],
        Stroke::new(1.0, divider()),
    );
    y += 6.0;
    p.text(
        Pos2::new(card.left() + PAD, y + 9.0),
        Align2::LEFT_CENTER,
        crate::i18n::t!("自动过渡"),
        font(11.0),
        text(),
    );
    p.text(
        Pos2::new(card.right() - PAD, y + 9.0),
        Align2::RIGHT_CENTER,
        crate::i18n::tr_fmt("{} 个关键帧 · {} 段", &[&keys, &segs_n]),
        font(11.0),
        dim(),
    );
    y += 22.0;

    let keys_list: Vec<usize> = state.keyframes.keys().copied().collect();
    let mut segs: Vec<(usize, usize, f64, f32)> = Vec::new();
    for pair in keys_list.windows(2) {
        let (Some(&a), Some(&b)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let ev = state.keyframes.get(&a).map(|k| k.exposure).unwrap_or(0.0);
        let secs = if fps > 0.1 {
            (b.saturating_sub(a)) as f32 / fps
        } else {
            0.0
        };
        segs.push((a, b, ev, secs));
    }

    let clip = p.with_clip_rect(card.shrink(1.0).intersect(p.clip_rect()));
    if segs.is_empty() {
        clip.text(
            Pos2::new(card.left() + PAD, y + 12.0),
            Align2::LEFT_CENTER,
            crate::i18n::t!("还没有关键帧"),
            font(11.0),
            dim(),
        );
    }
    for (a, b, ev, secs) in segs {
        if y + 24.0 > card.bottom() {
            break;
        }
        let mid = y + 12.0;
        clip.text(
            Pos2::new(card.left() + PAD, mid),
            Align2::LEFT_CENTER,
            format!("{:04} → {:04}", a + 1, b + 1),
            font(11.0),
            muted(),
        );
        clip.text(
            Pos2::new(card.right() - PAD - 42.0, mid),
            Align2::RIGHT_CENTER,
            format!("{ev:+.3} EV"),
            font(11.0),
            warn(),
        );
        clip.text(
            Pos2::new(card.right() - PAD, mid),
            Align2::RIGHT_CENTER,
            format!("{secs:.1} s"),
            font(11.0),
            dim(),
        );
        y += 24.0;
        clip.line_segment(
            [
                Pos2::new(card.left() + PAD, y - 0.5),
                Pos2::new(card.right() - PAD, y - 0.5),
            ],
            Stroke::new(1.0, divider()),
        );
    }

    h
}

/// 叠在预览图上的徽标底：`panel` 压到 85% 不透明度，跟着主题走（设计稿是
/// `rgba(22,23,27,.78)`，浅色主题下硬写深色会脏）。
///
/// Badge fill over the preview: `panel` at 85 % alpha, so it follows the theme.
fn badge_fill() -> Color32 {
    let c = panel();
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 216)
}

/// 千分位：设计稿里 1,387 这种读数，四位以上才加分隔。
///
/// Thousands separator for readouts such as 1,387.
pub(super) fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
