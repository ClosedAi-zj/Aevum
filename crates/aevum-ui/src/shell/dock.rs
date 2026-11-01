//! 右坞骨架：页签容器、直方图与平滑辅助。
//!
//! Right dock skeleton: tab container, histogram and smoothing helpers.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::adjust::adjust_tab;
use super::library::library_tab;
use super::mask::HistMeshCache;
use super::tabs::ai_tab;
use super::tabs::history_tab;
use super::tabs::info_tab;
use super::tabs::keys_tab;
use crate::icons::{self, Glyph};
use crate::state::{AppState, DockTab};
use crate::theme::{self, *};
use egui::{
    Align2, Color32, Pos2, Rect, ScrollArea, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

// ---------------------------------------------------------------- 右侧面板

pub(super) fn dock_panel(state: &mut AppState, ui: &mut Ui) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    let strip = Rect::from_min_size(full.min, Vec2::new(full.width(), TAB_H));
    p.rect_filled(strip, 0, header());
    theme::rule(&p, strip.left(), strip.right(), strip.bottom(), border());

    // 面板页签用下划线而不是分段胶囊：一条 HEADER 上排六个名字，
    // 选中项靠白字 + 2px 强调线认，和 PS 的调板页签同一套读法。
    let seg_w = (full.width() - 52.0) / DockTab::ALL.len() as f32;
    for (i, tab) in DockTab::ALL.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(
                strip.left() + 6.0 + i as f32 * seg_w + 1.0,
                strip.top() + 4.0,
            ),
            Vec2::new(seg_w - 2.0, strip.height() - 8.0),
        );
        let resp = ui.interact(r, ui.make_persistent_id(("dock-tab", i)), Sense::click());
        let active = state.dock_tab == *tab;
        if active {
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(r.left() + 8.0, r.bottom() - 1.5),
                    Pos2::new(r.right() - 8.0, r.bottom()),
                ),
                1.0,
                accent(),
            );
        } else if resp.hovered() {
            p.rect_filled(r, radius_control(), elev_hover());
        }
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            crate::i18n::tr(tab.cn()),
            font(SIZE_SMALL),
            if active {
                on_selected()
            } else if resp.hovered() {
                text()
            } else {
                muted()
            },
        );
        if resp.clicked() {
            state.dock_tab = *tab;
        }
    }

    let close = Rect::from_center_size(
        Pos2::new(strip.right() - 16.0, strip.center().y),
        Vec2::splat(18.0),
    );
    let cr = ui.interact(close, ui.make_persistent_id("dock-close"), Sense::click());
    if cr.hovered() {
        p.rect_filled(close, radius_control(), elev_hover());
    }
    icons::paint(
        &p,
        close.shrink(5.0),
        Glyph::Close,
        if cr.hovered() { text() } else { muted() },
    );
    if cr.clicked() {
        state.show_dock = false;
    }
    cr.on_hover_text(crate::i18n::t!("隐藏面板"));

    let content = Rect::from_min_max(Pos2::new(full.left(), full.top() + TAB_H), full.max);
    ui.scope_builder(
        UiBuilder::new().id_salt("dock-body").max_rect(content),
        |ui| {
            // 滚动条压到描边色：默认那根是 ELEV，8px 宽的亮条贴在面板右缘，
            // 比面板里任何内容都抢眼，滚起来像一根白杠在爬。
            {
                let w = &mut ui.style_mut().visuals.widgets;
                w.inactive.bg_fill = border();
                w.hovered.bg_fill = border_strong();
                w.active.bg_fill = accent_dim();
            }
            egui::Frame::NONE
                .inner_margin(egui::Margin {
                    left: 14,
                    right: 14,
                    top: 10,
                    bottom: 12,
                })
                .show(ui, |ui| {
                    histogram(ui, state);
                    ui.add_space(6.0);
                    let mut scroll = ScrollArea::vertical()
                        .id_salt("dock-scroll")
                        .auto_shrink([false, false]);
                    if state.dock_scroll_end {
                        // 只推一次：每一帧都写超大偏移会把内容画到裁剪区外。
                        // egui 在本帧末把偏移钳到底部，后面的帧就停在底部。
                        scroll = scroll.vertical_scroll_offset(1.0e6);
                        state.dock_scroll_end = false;
                    }
                    scroll.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match state.dock_tab {
                            DockTab::Adjust => adjust_tab(state, ui),
                            DockTab::Info => info_tab(state, ui),
                            DockTab::Keys => keys_tab(state, ui),
                            DockTab::Ai => ai_tab(state, ui),
                            DockTab::History => history_tab(state, ui),
                            DockTab::Library => library_tab(state, ui),
                        }
                        ui.add_space(16.0);
                    });
                });
        },
    );
}

/// 直方图：对齐界面稿 `.histo` —— 没有卡片、没有描边、没有网格线，
/// 三条 RGB 渐变填充山形直接画在 dock 底上，白色亮度山形压在最上面；
/// 下方一行 10px 的剪切读数（左阴影、右高光，两端对齐）。
/// 直方图显示层的两遍 [1,2,1]/4 卷积：三通道山形与白色亮度山形共用，
/// 抽出来是一份事实源——以后想改平滑强度只动这里。
fn smooth_twice(vals: &mut [f32], bins: usize) {
    for _ in 0..2 {
        let prev = vals.to_vec();
        for b in 0..bins {
            let l = if b == 0 { prev[0] } else { prev[b - 1] };
            let r = if b + 1 == bins {
                prev[bins - 1]
            } else {
                prev[b + 1]
            };
            vals[b] = (l + 2.0 * prev[b] + r) * 0.25;
        }
    }
}

fn histogram(ui: &mut Ui, state: &mut AppState) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 84.0), Sense::hover());
    let p = ui.painter().clone();
    let Some(h) = state.hist else {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "等待预览直方图",
            font(SIZE_SMALL),
            dim(),
        );
        return;
    };

    let bins = 128usize;
    let max = h.iter().flatten().copied().max().unwrap_or(0).max(1) as f32;
    let bar_w = rect.width() / bins as f32;

    // 4 个 Mesh 形状（3 通道 + 亮度包络）走缓存（E6）：键 = (hist_version, rect)。
    // 新预览落地时 hist_version 才变；静止悬停帧直接 clone —— Shape::Mesh 持
    // Arc<Mesh>（epaint 0.37 已确认），clone 零拷贝。
    let cached = match &state.hist_mesh_cache {
        Some(c) if c.version == state.hist_version && c.rect == rect => Some(c.shapes.clone()),
        _ => None,
    };
    let shapes = match cached {
        Some(shapes) => shapes,
        None => {
            // 每条山形是一个三角带：每个 bin 四个顶点（左峰、右峰、左底、右底），
            // 顶点色从山脊的半透明衰减到底边的全透明 —— egui 没有纵向渐变填充，
            // 逐顶点插值就是渐变本身。三通道重合处 alpha 叠加自然泛白，和 LR 的读法一致。
            const CREST_A: u8 = 115; // 界面稿 stop-opacity .45 ≈ 115/255
            let channel_rgb: [[u8; 3]; 3] =
                [[0xFF, 0x5C, 0x5C], [0x5C, 0xFF, 0x8C], [0x5C, 0x8C, 0xFF]];
            let mut shapes: Vec<Shape> = Vec::with_capacity(4);
            for ch in 0..3 {
                let mut mesh = egui::Mesh::default();
                // 显示平滑：[1,2,1]/4 卷积两遍。原始 bin 在尖峰数据（大面积纯色天空）
                // 下会画出锯齿山，界面稿的圆滑感来自它的样例数据本身平缓 ——
                // 卷积只动显示层，不碰 state.hist 的原始数据。
                let raw: Vec<f32> = (0..bins)
                    .map(|b| {
                        let lo = b * 2;
                        let count = h[ch][lo] + h[ch][lo + 1];
                        if count == 0 {
                            0.0
                        } else {
                            (count as f32 / max).powf(0.55)
                        }
                    })
                    .collect();
                let mut vals = raw.clone();
                smooth_twice(&mut vals, bins);
                for (b, &v) in vals.iter().enumerate().take(bins) {
                    let x0 = rect.left() + b as f32 * bar_w;
                    let x1 = x0 + bar_w;
                    let y_top = rect.bottom() - rect.height() * v;
                    let [r, g, bl] = channel_rgb[ch];
                    let i = mesh.vertices.len() as u32;
                    mesh.colored_vertex(
                        Pos2::new(x0, y_top),
                        Color32::from_rgba_unmultiplied(r, g, bl, CREST_A),
                    );
                    mesh.colored_vertex(
                        Pos2::new(x1, y_top),
                        Color32::from_rgba_unmultiplied(r, g, bl, CREST_A),
                    );
                    mesh.colored_vertex(
                        Pos2::new(x0, rect.bottom()),
                        Color32::from_rgba_unmultiplied(r, g, bl, 0),
                    );
                    mesh.colored_vertex(
                        Pos2::new(x1, rect.bottom()),
                        Color32::from_rgba_unmultiplied(r, g, bl, 0),
                    );
                    mesh.add_triangle(i, i + 1, i + 3);
                    mesh.add_triangle(i, i + 3, i + 2);
                }
                shapes.push(egui::Shape::Mesh(mesh.into()));
            }

            // 亮度包络：三通道按同一缩放相加后的轮廓，一条白色山形把「整体曝光」
            // 从彩色山里拎出来。顶点 alpha 140 → 底 13（界面稿 .55 → .05）。
            let mut lum = egui::Mesh::default();
            // 亮度包络与三通道同一套平滑，山形才叠得齐。
            let lum_raw: Vec<f32> = (0..bins)
                .map(|b| {
                    let lo = b * 2;
                    let sum = [0usize, 1, 2]
                        .iter()
                        .map(|&ch| h[ch][lo] as f32 + h[ch][lo + 1] as f32)
                        .sum::<f32>();
                    (sum / (max * 2.2)).powf(0.55).min(1.0)
                })
                .collect();
            let mut lum_vals = lum_raw.clone();
            smooth_twice(&mut lum_vals, bins);
            for (b, &v) in lum_vals.iter().enumerate().take(bins) {
                let x0 = rect.left() + b as f32 * bar_w;
                let x1 = x0 + bar_w;
                let y_top = rect.bottom() - rect.height() * v;
                let i = lum.vertices.len() as u32;
                lum.colored_vertex(Pos2::new(x0, y_top), Color32::from_white_alpha(140));
                lum.colored_vertex(Pos2::new(x1, y_top), Color32::from_white_alpha(140));
                lum.colored_vertex(Pos2::new(x0, rect.bottom()), Color32::from_white_alpha(13));
                lum.colored_vertex(Pos2::new(x1, rect.bottom()), Color32::from_white_alpha(13));
                lum.add_triangle(i, i + 1, i + 3);
                lum.add_triangle(i, i + 3, i + 2);
            }
            shapes.push(egui::Shape::Mesh(lum.into()));

            state.hist_mesh_cache = Some(HistMeshCache {
                version: state.hist_version,
                rect,
                shapes: shapes.clone(),
            });
            shapes
        }
    };
    for shape in &shapes {
        p.add(shape.clone());
    }

    // 剪切角标：真的剪切才出现，两枚小三角压在直方图区顶角。
    let total: u64 = (0..256)
        .map(|v| h[0][v] as u64 + h[1][v] as u64 + h[2][v] as u64)
        .sum();
    let low: u64 = (0..3)
        .map(|v| h[0][v] as u64 + h[1][v] as u64 + h[2][v] as u64)
        .sum();
    let high: u64 = (253..256)
        .map(|v| h[0][v] as u64 + h[1][v] as u64 + h[2][v] as u64)
        .sum();
    let clip_low = total > 0 && (low as f32 / total as f32) > 0.0003;
    let clip_high = total > 0 && (high as f32 / total as f32) > 0.0003;
    let s = 4.0;
    if clip_low {
        p.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(rect.left() + 1.0, rect.top() + 1.0),
                Pos2::new(rect.left() + 1.0 + s * 2.0, rect.top() + 1.0),
                Pos2::new(rect.left() + 1.0, rect.top() + 1.0 + s * 2.0),
            ],
            text(),
            Stroke::NONE,
        ));
    }
    if clip_high {
        p.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(rect.right() - 1.0, rect.top() + 1.0),
                Pos2::new(rect.right() - 1.0 - s * 2.0, rect.top() + 1.0),
                Pos2::new(rect.right() - 1.0, rect.top() + 1.0 + s * 2.0),
            ],
            text(),
            Stroke::NONE,
        ));
    }

    // 剪切读数行：左「◧ 阴影 n%」右「高光 n% ◨」，两端对齐；两枚读数本身就是
    // 开关——点一下让画布盖对应的裁切罩（LR 直方图角标的做法），悬停提示 J 键。
    let (clip_row, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), kv_h()), Sense::hover());
    let pct = |n: u64| {
        if total > 0 {
            format!("{:.1}", n as f32 * 100.0 / total as f32)
        } else {
            "0.0".to_owned()
        }
    };
    let micro = font(10.0);
    let shadow_chip = Rect::from_min_size(
        Pos2::new(clip_row.left(), clip_row.center().y - 9.0),
        Vec2::new(64.0, 18.0),
    );
    let high_chip = Rect::from_min_size(
        Pos2::new(clip_row.right() - 64.0, clip_row.center().y - 9.0),
        Vec2::new(64.0, 18.0),
    );
    for (chip, on, label, col) in [
        (
            shadow_chip,
            state.clip_shadows,
            crate::i18n::tr_fmt("◧ 阴影 {}%", &[&pct(low)]),
            Color32::from_rgb(56, 110, 255),
        ),
        (
            high_chip,
            state.clip_highlights,
            crate::i18n::tr_fmt("高光 {}% ◨", &[&pct(high)]),
            Color32::from_rgb(255, 46, 46),
        ),
    ] {
        let resp = ui.interact(
            chip,
            ui.make_persistent_id(("clip-chip", chip == shadow_chip)),
            Sense::click(),
        );
        if on || resp.hovered() {
            let fill = if on {
                Color32::from_rgba_unmultiplied(col[0], col[1], col[2], 46)
            } else {
                elev()
            };
            p.rect_filled(chip, radius_control(), fill);
        }
        if on {
            p.rect_stroke(
                chip,
                radius_control(),
                Stroke::new(1.0, col),
                StrokeKind::Inside,
            );
        }
        p.text(
            chip.center(),
            Align2::CENTER_CENTER,
            label,
            micro.clone(),
            if on { text() } else { dim() },
        );
        if resp.clicked() {
            match chip == shadow_chip {
                true => state.clip_shadows = !state.clip_shadows,
                false => state.clip_highlights = !state.clip_highlights,
            }
            state.clip_mask_stamp = u64::MAX;
        }
    }
    let shadow_tip = if state.clip_shadows {
        "阴影裁切罩：开"
    } else {
        "阴影裁切罩：关"
    };
    let high_tip = if state.clip_highlights {
        "高光裁切罩：开"
    } else {
        "高光裁切罩：关"
    };
    ui.interact(shadow_chip, ui.scope_id().with("st"), Sense::hover())
        .on_hover_text(crate::i18n::tr_fmt(
            "{}（J 键循环开关）",
            &[&crate::i18n::tr(shadow_tip)],
        ));
    ui.interact(high_chip, ui.scope_id().with("ht"), Sense::hover())
        .on_hover_text(crate::i18n::tr_fmt(
            "{}（J 键循环开关）",
            &[&crate::i18n::tr(high_tip)],
        ));
}

/// 直方图与推子读数：三色柱、白色亮度折线、改动后才变蓝的数值。
/// 这些全是「不上屏不会报错」的画法，只能照色认形状。
#[cfg(test)]
mod dock_paint_tests {
    use super::*;
    use crate::shell::common::group_help;
    use crate::shell::common::slider;
    use crate::shell::ui;
    use crate::state::Module;
    use egui::Context;

    fn input(w: f32, h: f32) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(w, h))),
            max_texture_side: Some(8192),
            ..Default::default()
        }
    }

    /// 一块有峰有谷的直方图：每个通道都铺满 128 档，亮度包络才画得出折线的凹处。
    fn textured_hist() -> [[u32; 256]; 3] {
        let shapes = [(7u32, 40u32, 4u32), (11, 30, 3), (13, 36, 5)];
        shapes.map(|(mul, span, base)| std::array::from_fn(|v| base + (v as u32 * mul) % span))
    }

    #[test]
    fn histogram_paints_three_channel_meshes_and_a_luminance_mesh() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::new();
        state.hist = Some(textured_hist());
        let mut draw = |ui: &mut Ui| histogram(ui, &mut state);
        let mut out = ctx.run_ui(input(320.0, 200.0), &mut draw);
        // 新规格：山形是逐顶点渐变的三角带（Mesh），不再有逐 bin 的实心矩形柱。
        // 顶点色经 from_rgba_unmultiplied 落进顶点时已按 alpha 压平，
        // 所以期望值也用同一构造算出来，逐色完整比较。
        let channel_rgb: [[u8; 3]; 3] =
            [[0xFF, 0x5C, 0x5C], [0x5C, 0xFF, 0x8C], [0x5C, 0x8C, 0xFF]];
        let crest: Vec<Color32> = channel_rgb
            .iter()
            .map(|rgb| Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], 115))
            .collect();
        let base: Vec<Color32> = channel_rgb
            .iter()
            .map(|rgb| Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], 0))
            .collect();
        let (lum_top, lum_base) = (
            Color32::from_white_alpha(140),
            Color32::from_white_alpha(13),
        );
        let mut channel_meshes = [0usize; 3];
        let mut lum_mesh = 0usize;
        let mut solid_bars = 0usize;
        for clipped in &out.shapes {
            match &clipped.shape {
                egui::Shape::Mesh(m) => {
                    let mut touched = [false; 3];
                    let mut white = false;
                    for v in &m.vertices {
                        for k in 0..3 {
                            if v.color == crest[k] || v.color == base[k] {
                                touched[k] = true;
                            }
                        }
                        if v.color == lum_top || v.color == lum_base {
                            white = true;
                        }
                    }
                    for k in 0..3 {
                        if touched[k] {
                            channel_meshes[k] += 1;
                        }
                    }
                    if white {
                        lum_mesh += 1;
                    }
                }
                egui::Shape::Rect(r) => {
                    // 旧实现是逐 bin 实心矩形柱：新实现不许再出现这类色块。
                    let opaque_colored =
                        r.fill.a() > 100 && (r.fill[0] != r.fill[1] || r.fill[1] != r.fill[2]);
                    if opaque_colored {
                        solid_bars += 1;
                    }
                }
                _ => {}
            }
        }
        out.textures_delta.clear();
        for (ch, n) in channel_meshes.iter().enumerate() {
            assert!(*n >= 1, "{} 通道的渐变山形没上屏", ["红", "绿", "蓝"][ch]);
        }
        assert!(lum_mesh >= 1, "亮度山形没上屏，整体曝光读不出来");
        assert_eq!(solid_bars, 0, "直方图又退回了实心矩形柱");
    }

    fn value_color(v: f64) -> Color32 {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut value = v;
        let mut draw = |ui: &mut Ui| {
            slider(ui, "exposure", "曝光", &mut value, -5.0, 5.0, 0.0, 2, true);
        };
        let mut out = ctx.run_ui(input(320.0, 120.0), &mut draw);
        let mut found = None;
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape {
                let text = t.galley.text().trim().to_owned();
                if text == "0.00" || text == "+0.75" {
                    found = t.galley.job.sections.first().map(|s| s.format.color);
                }
            }
        }
        out.textures_delta.clear();
        found.expect("推子的读数没上屏")
    }

    #[test]
    fn slider_value_is_flat_grey_at_default_and_accent_once_changed() {
        assert_eq!(value_color(0.0), value(), "默认值的读数不该抢眼");
        assert_eq!(
            value_color(0.75),
            accent_hover(),
            "改过的读数没变蓝，一排推子里看不出哪条动过"
        );
    }

    /// 整段说明只许待在 ⓘ 的悬停提示里：正文铺回去，面板又变成一列散文。
    #[test]
    fn explanations_live_in_group_help_not_on_the_panel() {
        for key in [
            "copies",
            "hdr",
            "pano",
            "stack",
            "timeblend",
            "basic",
            "color",
        ] {
            let help = group_help(key).expect("分组没有说明入口");
            assert!(help.chars().count() >= 12, "{key} 的说明太短，等于没写");
        }
        // 界面帧每帧 drain 全进程共享的提示队列：持 env 的测试锁（见 env.rs）。
        let _serial = aevum_core::env::lock_notes_for_test();
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        state.module = Module::Develop;
        let mut out = ctx.run_ui(input(1600.0, 1000.0), |root| ui(&mut state, root));
        let mut text = String::new();
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape {
                text.push_str(t.galley.text());
            }
        }
        out.textures_delta.clear();
        for old in [
            "同场景的多张不同曝光",
            "多张有重叠的照片",
            "还没有副本：调好一版",
        ] {
            assert!(!text.contains(old), "长句又铺回面板了：{old}");
        }
    }
}
