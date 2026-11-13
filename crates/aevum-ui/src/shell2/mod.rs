//! v18 从零外壳（视觉定稿版）——按 docs/mockup.html 1:1 落地。
//!
//! 与旧壳（[`crate::shell`]）并存：入口 [`ShellV18::ui`]，读真 `AppState`
//! 与真 `GradeParams`（滑轨直接拖拽改参），不碰旧壳任何文件。
//! 原型范围：布局骨架 + 主题 + 程序化照片纹理 + 滑轨真绑定 + 胶片条真选帧。
//!
//! The from-scratch v18 shell beside the old one: real state/params bindings,
//! programmatic photo texture, none of the old shell files touched.

use egui::{
    Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, Vec2,
    pos2,
};

use crate::state::AppState;
use crate::theme_modern as t18;

const W_AIRAIL: f32 = 42.0;
const W_SIDEBAR: f32 = 246.0;
const W_DOCK: f32 = 300.0;
const H_MENUBAR: f32 = 38.0;
const H_OPTIONS: f32 = 36.0;
const H_FILM: f32 = 110.0;
const H_STATUS: f32 = 24.0;
const H_ROW: f32 = 26.0;

/// v18 外壳：纹理缓存（会话内）。
///
/// v18 shell with session-only texture cache.
#[derive(Default)]
pub struct ShellV18 {
    photo: Option<TextureHandle>,
    preset: usize,
}

impl ShellV18 {
    pub fn new() -> Self {
        Self::default()
    }

    /// 整壳入口。
    ///
    /// Shell entry.
    pub fn ui(&mut self, state: &mut AppState, ui: &mut egui::Ui) {
        self.menubar(ui);
        self.optionsbar(ui);
        self.filmstrip(state, ui);
        self.statusbar(state, ui);

        egui::Panel::left("v18-airail")
            .exact_size(W_AIRAIL)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(t18::BASE))
            .show(ui, |ui| self.airail(ui));
        egui::Panel::left("v18-sidebar")
            .exact_size(W_SIDEBAR)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(t18::BASE))
            .show(ui, |ui| self.sidebar(ui));
        egui::Panel::right("v18-dock")
            .exact_size(W_DOCK)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(t18::BASE))
            .show(ui, |ui| self.dock(state, ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(t18::SUNKEN))
            .show(ui, |ui| self.canvas(state, ui));
    }

    // ================= 菜单行 =================
    fn menubar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("v18-menubar")
            .exact_size(H_MENUBAR)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(t18::BASE))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    let (r, _) = ui.allocate_exact_size(Vec2::new(18.0, 18.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_stroke(r, 4.0, Stroke::new(1.6, t18::ACCENT), StrokeKind::Inside);
                    p.rect_filled(r.shrink(5.0), 2.0, t18::ACCENT);
                    ui.label(RichText::new("恒晷").size(13.0).strong().color(t18::TEXT));
                    ui.add_space(6.0);
                    for m in ["文件", "编辑", "视图", "插件", "帮助"] {
                        ui.label(RichText::new(m).size(12.0).color(t18::TEXT_MUTED));
                        ui.add_space(8.0);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        let _ = ui.add(
                            egui::Button::new(
                                RichText::new("导出…").size(11.5).color(t18::ON_ACCENT),
                            )
                            .fill(t18::ACCENT)
                            .min_size(Vec2::new(0.0, 26.0)),
                        );
                        ui.add_space(6.0);
                        ui.label(RichText::new("显影 ▾").size(12.0).color(t18::TEXT));
                    });
                });
            });
    }

    // ================= 选项栏 =================
    fn optionsbar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("v18-options")
            .exact_size(H_OPTIONS)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(t18::BASE))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    let (r, _) = ui.allocate_exact_size(Vec2::new(118.0, 25.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(r, 6.0, t18::SURFACE);
                    p.rect_stroke(
                        r,
                        6.0,
                        Stroke::new(1.0, t18::hairline()),
                        StrokeKind::Inside,
                    );
                    p.circle_filled(pos2(r.left() + 9.0, r.center().y), 2.5, t18::ACCENT);
                    p.text(
                        pos2(r.left() + 18.0, r.center().y),
                        Align2::LEFT_CENTER,
                        "松赞林寺-198",
                        FontId::proportional(11.5),
                        t18::TEXT,
                    );
                    ui.add_space(8.0);
                    ui.label(RichText::new("抓手").size(11.5).color(t18::TEXT));
                    ui.label(kbd("H"));
                    ui.add_space(6.0);
                    ui.label(RichText::new("原始 ▾").size(11.5).color(t18::TEXT_MUTED));
                    ui.add_space(4.0);
                    let _ = ui.add(
                        egui::Button::new(RichText::new("自动").size(11.5).color(t18::TEXT))
                            .fill(t18::SURFACE)
                            .min_size(Vec2::new(0.0, 24.0)),
                    );
                    let _ = ui.add(
                        egui::Button::new(RichText::new("复位").size(11.5).color(t18::TEXT))
                            .fill(t18::SURFACE)
                            .min_size(Vec2::new(0.0, 24.0)),
                    );
                    ui.add_space(6.0);
                    ui.label(badge("蒙版", "0 个"));
                    ui.label(badge("显影", "3 项调整"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("100% ▾")
                                .font(FontId::monospace(11.5))
                                .color(t18::TEXT),
                        );
                        ui.label(
                            RichText::new("撤销 Ctrl+Z")
                                .size(11.5)
                                .color(t18::TEXT_MUTED),
                        );
                        ui.label(
                            RichText::new("画幅 1280 × 720")
                                .size(11.5)
                                .color(t18::TEXT_MUTED),
                        );
                    });
                });
            });
    }

    // ================= AI 图标条 =================
    fn airail(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        // (图标种类, 激活态)
        let items = [
            (AiIcon::Agent, true),
            (AiIcon::Chat, false),
            (AiIcon::Wand, false),
            (AiIcon::Batch, false),
        ];
        for (icon, on) in items {
            let (r, resp) = ui.allocate_exact_size(Vec2::new(30.0, 30.0), Sense::click());
            let p = ui.painter();
            if on {
                p.rect_filled(r, 8.0, t18::accent_tint());
                p.rect_stroke(
                    r,
                    8.0,
                    Stroke::new(1.0, t18::ACCENT.gamma_multiply(0.55)),
                    StrokeKind::Inside,
                );
                p.circle_filled(pos2(r.right() - 4.0, r.top() + 4.0), 2.5, t18::ACCENT);
            } else if resp.hovered() {
                p.rect_filled(r, 8.0, Color32::from_white_alpha(14));
            }
            let c = r.center();
            let ink = if on {
                t18::ACCENT_TEXT
            } else {
                t18::TEXT_MUTED
            };
            let st = Stroke::new(1.3, ink);
            match icon {
                AiIcon::Agent => {
                    p.line_segment([pos2(c.x, c.y - 5.0), pos2(c.x, c.y + 5.0)], st);
                    p.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], st);
                    p.line_segment([pos2(c.x - 3.2, c.y - 3.2), pos2(c.x + 3.2, c.y + 3.2)], st);
                    p.line_segment([pos2(c.x + 3.2, c.y - 3.2), pos2(c.x - 3.2, c.y + 3.2)], st);
                }
                AiIcon::Chat => {
                    p.rect_stroke(
                        Rect::from_min_size(pos2(c.x - 6.0, c.y - 5.5), Vec2::new(12.0, 9.0)),
                        2.0,
                        st,
                        StrokeKind::Inside,
                    );
                    p.line_segment([pos2(c.x - 2.0, c.y + 3.5), pos2(c.x - 2.0, c.y + 6.0)], st);
                }
                AiIcon::Wand => {
                    p.line_segment([pos2(c.x - 4.0, c.y + 4.0), pos2(c.x + 4.0, c.y - 4.0)], st);
                    p.circle_filled(pos2(c.x + 4.5, c.y - 4.5), 1.6, ink);
                }
                AiIcon::Batch => {
                    p.rect_stroke(
                        Rect::from_min_size(pos2(c.x - 5.5, c.y - 5.5), Vec2::new(11.0, 11.0)),
                        2.0,
                        st,
                        StrokeKind::Inside,
                    );
                    p.line_segment([pos2(c.x - 1.5, c.y), pos2(c.x + 1.5, c.y)], st);
                    p.line_segment([pos2(c.x, c.y - 1.5), pos2(c.x, c.y + 1.5)], st);
                }
            }
        }
    }

    // ================= 左：预设与历史 =================
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new("预设与历史")
                    .size(13.0)
                    .strong()
                    .color(t18::TEXT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Presets · History")
                        .size(10.0)
                        .color(t18::TEXT_FAINT),
                );
            });
        });
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(micro("预设"));
        });
        ui.add_space(4.0);
        let names = [
            "标准",
            "鲜艳风光",
            "落日暖调",
            "冷色电影",
            "复古胶片",
            "青橙调",
        ];
        let hues = [
            (
                Color32::from_rgb(0x3a, 0x3a, 0x62),
                Color32::from_rgb(0x8a, 0x8a, 0xc0),
            ),
            (
                Color32::from_rgb(0x4a, 0x32, 0x2a),
                Color32::from_rgb(0xd8, 0x8a, 0x4a),
            ),
            (
                Color32::from_rgb(0x22, 0x30, 0x4a),
                Color32::from_rgb(0x7a, 0x9e, 0xc4),
            ),
            (
                Color32::from_rgb(0x26, 0x26, 0x30),
                Color32::from_rgb(0x8e, 0x8e, 0x99),
            ),
            (
                Color32::from_rgb(0x36, 0x24, 0x44),
                Color32::from_rgb(0xb4, 0x6e, 0x8e),
            ),
            (
                Color32::from_rgb(0x24, 0x36, 0x30),
                Color32::from_rgb(0x8a, 0xc4, 0x9e),
            ),
        ];
        let names = names;
        for (i, name) in names.iter().enumerate() {
            let on = self.preset == i;
            let (r, resp) = ui
                .scope(|ui| {
                    ui.style_mut().spacing.item_spacing = Vec2::new(8.0, 8.0);
                    let w = (ui.available_width() - 8.0) * 0.5;
                    ui.allocate_exact_size(Vec2::new(w.max(60.0), 44.0), Sense::click())
                })
                .inner;
            let p = ui.painter();
            p.rect_filled(r, 8.0, t18::SURFACE);
            if on {
                p.rect_stroke(r, 8.0, Stroke::new(1.0, t18::ACCENT), StrokeKind::Inside);
            } else {
                p.rect_stroke(
                    r,
                    8.0,
                    Stroke::new(1.0, t18::hairline()),
                    StrokeKind::Inside,
                );
            }
            let thumb = Rect::from_min_size(
                r.min + Vec2::new(1.0, 1.0),
                Vec2::new(r.width() - 2.0, 26.0),
            );
            let (a, b) = hues[i];
            let steps = 8;
            for k in 0..steps {
                let tt = k as f32 / steps as f32;
                let c = lerp_color(a, b, tt);
                p.rect_filled(
                    Rect::from_min_size(
                        pos2(thumb.left(), thumb.top() + tt * thumb.height()),
                        Vec2::new(thumb.width(), thumb.height() / steps as f32 + 1.0),
                    ),
                    0.0,
                    c,
                );
            }
            p.text(
                pos2(r.left() + 6.0, r.bottom() - 9.0),
                Align2::LEFT_CENTER,
                *name,
                FontId::proportional(10.5),
                if on { t18::TEXT } else { t18::TEXT_MUTED },
            );
            if resp.clicked() {
                self.preset = i;
            }
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(micro("快照"));
        });
        ui.add_space(4.0);
        hist_row(ui, "黄昏暖调 · 存档", "今天 21:42", "", false);
        hist_row(ui, "冷色备份", "今天 20:15", "", false);

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(micro("历史"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(12.0);
                ui.label(RichText::new("12 步").size(10.0).color(t18::TEXT_FAINT));
            });
        });
        let steps = [
            ("径向渐变蒙版 · 天空提亮", "21:47 · 当前", "蒙版", true),
            ("高光 −30 · 阴影 +18", "21:46", "基本", false),
            ("曝光 +0.35 · 对比度 +12", "21:45", "基本", false),
            ("HSL 蓝色 明度 −8", "21:43", "HSL", false),
            ("色温 5500 K", "21:41", "基本", false),
            ("自动校正（白平衡）", "21:40", "自动", false),
            ("导入 48 帧 NEF", "21:38", "序列", false),
        ];
        for (what, meta, tag, cur) in steps {
            hist_row(ui, what, meta, tag, cur);
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let _ = ui.add(
                egui::Button::new(RichText::new("撤销一步").size(11.5).color(t18::TEXT))
                    .fill(t18::SURFACE)
                    .min_size(Vec2::new(0.0, 26.0)),
            );
            let _ = ui.add(
                egui::Button::new(RichText::new("回到此处").size(11.5).color(t18::TEXT))
                    .fill(t18::SURFACE)
                    .min_size(Vec2::new(0.0, 26.0)),
            );
        });
    }

    // ================= 中：画布 =================
    fn canvas(&mut self, state: &mut AppState, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        let p = ui.painter().clone();
        p.rect_filled(
            full,
            0.0,
            Color32::from_rgba_unmultiplied(0x8B, 0x5C, 0xF6, 10),
        );
        let tex = self
            .photo
            .get_or_insert_with(|| make_photo_texture(ui.ctx()));
        let size = tex.size_vec2();
        let scale = ((full.width() - 150.0) / size.x)
            .min((full.height() - 120.0) / size.y)
            .min(1.5);
        let w = size.x * scale;
        let h = size.y * scale;
        let pr = Rect::from_center_size(full.center() - Vec2::new(0.0, 6.0), Vec2::new(w, h));
        for (i, a) in [10u8, 8, 6, 5].iter().enumerate() {
            let grow = 6.0 + i as f32 * 5.0;
            p.rect_filled(pr.expand(grow), 10.0, Color32::from_black_alpha(*a));
        }
        p.image(
            tex.id(),
            pr,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        for (fx, fy, vert) in [
            (0.333, 0.0, false),
            (0.667, 0.0, false),
            (0.0, 0.333, true),
            (0.0, 0.667, true),
        ] {
            let ln = if vert {
                [
                    pos2(pr.left() + pr.width() * fx, pr.top()),
                    pos2(pr.left() + pr.width() * fx, pr.bottom()),
                ]
            } else {
                [
                    pos2(pr.left(), pr.top() + pr.height() * fy),
                    pos2(pr.right(), pr.top() + pr.height() * fy),
                ]
            };
            p.line_segment(ln, Stroke::new(1.0, Color32::from_white_alpha(28)));
        }
        let mw = pr.width() * 0.42;
        let mh = pr.height() * 0.52;
        let mc = pos2(
            pr.center().x + pr.width() * 0.06,
            pr.center().y - pr.height() * 0.10,
        );
        let mr = Rect::from_center_size(mc, Vec2::new(mw, mh));
        p.circle_stroke(
            mc,
            mw.min(mh) * 0.62,
            Stroke::new(1.0, Color32::from_white_alpha(140)),
        );
        p.rect_stroke(
            mr,
            0.0,
            Stroke::new(1.0, t18::ACCENT.gamma_multiply(0.75)),
            StrokeKind::Middle,
        );
        for hx in [mr.left(), mr.center().x, mr.right()] {
            for hy in [mr.top(), mr.center().y, mr.bottom()] {
                if (hx == mr.center().x) != (hy == mr.center().y) {
                    p.rect_filled(
                        Rect::from_center_size(pos2(hx, hy), Vec2::splat(7.0)),
                        1.5,
                        t18::ACCENT,
                    );
                    p.rect_stroke(
                        Rect::from_center_size(pos2(hx, hy), Vec2::splat(7.0)),
                        1.5,
                        Stroke::new(1.0, t18::ON_ACCENT),
                        StrokeKind::Inside,
                    );
                }
            }
        }
        p.text(
            pos2(full.left() + 14.0, full.bottom() - 12.0),
            Align2::LEFT_CENTER,
            "松赞林寺 · NEF · 24mm · f/8 · 1/250 · ISO 100",
            FontId::proportional(10.5),
            t18::TEXT_FAINT,
        );
        p.text(
            pos2(full.right() - 44.0, full.bottom() - 22.0),
            Align2::CENTER_CENTER,
            "100%",
            FontId::monospace(11.5),
            t18::TEXT_MUTED,
        );
        let _ = state;
    }

    // ================= 右：参数坞 =================
    fn dock(&mut self, state: &mut AppState, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            for (i, tab) in ["调整", "信息", "关键帧", "AI", "历史", "图库"]
                .iter()
                .enumerate()
            {
                let on = i == 0;
                let (r, resp) = ui.allocate_exact_size(Vec2::new(38.0, 26.0), Sense::hover());
                let p = ui.painter();
                if on {
                    p.rect_filled(
                        Rect::from_min_size(
                            pos2(r.left() + 7.0, r.bottom() - 2.0),
                            Vec2::new(r.width() - 14.0, 2.0),
                        ),
                        1.0,
                        t18::ACCENT,
                    );
                } else if resp.hovered() {
                    p.rect_filled(r, 4.0, Color32::from_white_alpha(10));
                }
                p.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    *tab,
                    FontId::proportional(11.5),
                    if on { t18::TEXT } else { t18::TEXT_MUTED },
                );
                let _ = resp;
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("直方图").size(12.0).strong().color(t18::TEXT));
        });
        ui.add_space(4.0);
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 78.0), Sense::hover());
        let p = ui.painter();
        let nbars = 44;
        let bw = r.width() / nbars as f32;
        for i in 0..nbars {
            let t = i as f32 / nbars as f32;
            let v = 0.9 * (-((t - 0.10).powi(2)) / 0.004).exp()
                + 0.5 * (-((t - 0.36).powi(2)) / 0.02).exp()
                + 0.32 * (-((t - 0.62).powi(2)) / 0.016).exp()
                + 0.14 * (-((t - 0.85).powi(2)) / 0.005).exp();
            let hh = (v * r.height()).clamp(1.0, r.height());
            p.rect_filled(
                Rect::from_min_size(
                    pos2(r.left() + i as f32 * bw, r.bottom() - hh),
                    Vec2::new(bw - 1.5, hh),
                ),
                1.5,
                Color32::from_rgba_unmultiplied(0x8B, 0x5C, 0xF6, 190),
            );
        }
        p.line_segment(
            [
                pos2(r.left(), r.bottom() - 1.0),
                pos2(r.right(), r.bottom() - 1.0),
            ],
            Stroke::new(1.0, t18::hairline_strong()),
        );
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new("0")
                    .font(FontId::monospace(10.0))
                    .color(t18::TEXT_FAINT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("255")
                        .font(FontId::monospace(10.0))
                        .color(t18::TEXT_FAINT),
                );
            });
        });
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        group_head(ui, "配置文件");
        kv_row(ui, "风格", "标准 ▾");
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let _ = ui.add(
                egui::Button::new(RichText::new("导入 XMP").size(11.5).color(t18::TEXT))
                    .fill(t18::SURFACE)
                    .min_size(Vec2::new(0.0, 26.0)),
            );
            let _ = ui.add(
                egui::Button::new(RichText::new("导出 XMP").size(11.5).color(t18::TEXT))
                    .fill(t18::SURFACE)
                    .min_size(Vec2::new(0.0, 26.0)),
            );
        });
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        group_head(ui, "基本");
        ui.add_space(2.0);
        // 真绑定：滑轨直接改 GradeParams（f64）
        let pr = &mut state.params;
        slider_row(ui, "曝光", &mut pr.exposure, -3.0..=3.0, |v| {
            format!("{v:+.2} EV")
        });
        slider_row(ui, "对比度", &mut pr.contrast, -50.0..=50.0, |v| {
            format!("{v:+.0}")
        });
        slider_row(ui, "高光", &mut pr.highlights, -100.0..=100.0, |v| {
            format!("{v:+.0}")
        });
        slider_row(ui, "阴影", &mut pr.shadows, -100.0..=100.0, |v| {
            format!("{v:+.0}")
        });
        slider_row(ui, "白色色阶", &mut pr.whites, -100.0..=100.0, |v| {
            format!("{v:+.0}")
        });
        slider_row(ui, "黑色色阶", &mut pr.blacks, -100.0..=100.0, |v| {
            format!("{v:+.0}")
        });
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        group_head(ui, "HSL / 颜色");
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("2 项调整").size(10.0).color(t18::TEXT_FAINT));
        });
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        group_head(ui, "色调曲线");
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);
        group_head(ui, "锐化与降噪");
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new("更多分组 ▾")
                    .size(11.5)
                    .color(t18::TEXT_FAINT),
            );
        });
    }

    // ================= 胶片条 =================
    fn filmstrip(&mut self, state: &mut AppState, ui: &mut egui::Ui) {
        egui::Panel::bottom("v18-film")
            .exact_size(H_FILM)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(t18::SURFACE))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("胶片条 · 48 帧")
                            .size(11.5)
                            .strong()
                            .color(t18::TEXT),
                    );
                    ui.label(
                        RichText::new("筛选 ☆☆☆☆☆ ✓ ✕ ●●●●")
                            .size(11.0)
                            .color(t18::TEXT_MUTED),
                    );
                    ui.label(RichText::new("清除筛选").size(11.0).color(t18::TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("曲线").size(11.0).color(t18::TEXT_MUTED));
                    });
                });
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    let total = state
                        .seq
                        .as_ref()
                        .map(|s| s.frames.len())
                        .unwrap_or(10)
                        .max(10);
                    let sel = state.selected;
                    for i in 0..10.min(total) {
                        let on = sel == i;
                        let (r, resp) =
                            ui.allocate_exact_size(Vec2::new(112.0, 56.0), Sense::click());
                        let p = ui.painter();
                        let steps = 10;
                        for k in 0..steps {
                            let t = k as f32 / steps as f32;
                            let c = if t < 0.5 {
                                lerp_color(
                                    Color32::from_rgb(0x14, 0x30, 0x4a),
                                    Color32::from_rgb(0x4a, 0x4a, 0x63),
                                    t / 0.5,
                                )
                            } else {
                                lerp_color(
                                    Color32::from_rgb(0x4a, 0x4a, 0x63),
                                    Color32::from_rgb(0xe8, 0x9a, 0x52),
                                    (t - 0.5) / 0.5,
                                )
                            };
                            p.rect_filled(
                                Rect::from_min_size(
                                    pos2(r.left(), r.top() + t * r.height()),
                                    Vec2::new(r.width(), r.height() / steps as f32 + 1.0),
                                ),
                                0.0,
                                c,
                            );
                        }
                        if i % 4 == 0 {
                            p.rect_filled(r, 0.0, Color32::from_black_alpha(66));
                        } else if i % 4 == 1 {
                            p.rect_filled(r, 0.0, Color32::from_black_alpha(34));
                        } else if i % 4 == 3 {
                            p.rect_filled(r, 0.0, Color32::from_white_alpha(12));
                        }
                        if on {
                            p.rect_stroke(r, 0.0, Stroke::new(2.0, t18::TEXT), StrokeKind::Inside);
                            p.rect_filled(
                                Rect::from_min_size(
                                    pos2(r.left(), r.bottom() - 3.0),
                                    Vec2::new(r.width(), 3.0),
                                ),
                                0.0,
                                t18::ACCENT,
                            );
                        }
                        p.text(
                            pos2(r.left() + 5.0, r.bottom() - 7.0),
                            Align2::LEFT_CENTER,
                            format!("{:04}", i + 1),
                            FontId::new(10.0, egui::FontFamily::Proportional),
                            Color32::WHITE,
                        );
                        if i == 0 {
                            p.circle_filled(pos2(r.right() - 6.0, r.top() + 6.0), 3.0, t18::STAR);
                        }
                        if resp.clicked() {
                            state.selected = i;
                        }
                    }
                });
            });
    }

    // ================= 状态栏 =================
    fn statusbar(&mut self, state: &mut AppState, ui: &mut egui::Ui) {
        egui::Panel::bottom("v18-status")
            .exact_size(H_STATUS)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(t18::BASE))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("画布 1280 × 720")
                            .size(10.5)
                            .color(t18::TEXT_MUTED),
                    );
                    ui.label(RichText::new("蒙版 1 个").size(10.5).color(t18::TEXT_MUTED));
                    ui.label(
                        RichText::new("显影 3 项调整")
                            .size(10.5)
                            .color(t18::TEXT_MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} 帧",
                                state.seq.as_ref().map(|s| s.frames.len()).unwrap_or(0)
                            ))
                            .size(10.5)
                            .color(t18::TEXT_MUTED),
                        );
                        ui.label(RichText::new("24 fps").size(10.5).color(t18::TEXT_MUTED));
                        ui.label(
                            RichText::new("撤销栈 12 步")
                                .size(10.5)
                                .color(t18::TEXT_MUTED),
                        );
                    });
                });
            });
    }
}

// ================= 小工具 =================

enum AiIcon {
    Agent,
    Chat,
    Wand,
    Batch,
}

fn kbd(s: &str) -> RichText {
    RichText::new(format!(" {s} "))
        .size(10.0)
        .color(t18::TEXT_MUTED)
        .background_color(t18::SURFACE)
}

fn badge(k: &str, v: &str) -> RichText {
    RichText::new(format!("{k} {v}"))
        .size(11.5)
        .color(t18::TEXT)
}

fn micro(s: &str) -> RichText {
    RichText::new(s).size(10.0).color(t18::TEXT_FAINT)
}

fn kv_row(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(RichText::new(k).size(11.5).color(t18::TEXT_MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(12.0);
            ui.label(RichText::new(v).size(11.5).color(t18::TEXT));
        });
    });
}

fn group_head(ui: &mut egui::Ui, name: &str) {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(RichText::new(name).size(12.0).strong().color(t18::TEXT));
    });
}

fn hist_row(ui: &mut egui::Ui, what: &str, meta: &str, tag: &str, cur: bool) {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
    let p = ui.painter();
    if cur {
        p.rect_filled(r, 6.0, t18::accent_tint());
        p.rect_filled(
            Rect::from_min_size(
                pos2(r.left(), r.top() + 4.0),
                Vec2::new(2.0, r.height() - 8.0),
            ),
            1.0,
            t18::ACCENT,
        );
    } else if resp.hovered() {
        p.rect_filled(r, 6.0, Color32::from_white_alpha(12));
    }
    p.circle_filled(
        pos2(r.left() + 12.0, r.top() + 11.0),
        2.8,
        if cur { t18::ACCENT } else { t18::TEXT_FAINT },
    );
    p.text(
        pos2(r.left() + 24.0, r.top() + 9.0),
        Align2::LEFT_CENTER,
        what,
        FontId::proportional(11.5),
        if cur || resp.hovered() {
            t18::TEXT
        } else {
            t18::TEXT_MUTED
        },
    );
    p.text(
        pos2(r.left() + 24.0, r.bottom() - 8.0),
        Align2::LEFT_CENTER,
        meta,
        FontId::proportional(10.0),
        t18::TEXT_FAINT,
    );
    if !tag.is_empty() {
        let w = p
            .layout_no_wrap(tag.to_owned(), FontId::proportional(9.5), t18::TEXT_FAINT)
            .size()
            .x
            + 12.0;
        let tr = Rect::from_min_size(
            pos2(r.right() - w - 8.0, r.center().y - 8.0),
            Vec2::new(w, 16.0),
        );
        p.rect_stroke(
            tr,
            4.0,
            Stroke::new(1.0, t18::hairline()),
            StrokeKind::Inside,
        );
        p.text(
            tr.center(),
            Align2::CENTER_CENTER,
            tag,
            FontId::proportional(9.5),
            t18::TEXT_FAINT,
        );
    }
    let _ = resp;
}

/// 手绘滑轨行：label / value / 2px 轨 / 中性填充 / 白手柄；拖拽改真值（f64）。
///
/// Hand-painted slider row (label / value / 2 px rail / neutral fill / white
/// knob); dragging writes the real f64 value.
fn slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    fmt: impl Fn(f64) -> String,
) {
    let lo = *range.start();
    let hi = *range.end();
    let v = *value as f32;
    let frac = ((v - lo as f32) / (hi as f32 - lo as f32)).clamp(0.0, 1.0);
    let full = ui.available_width();
    let rail_w = full * 0.62;
    let (row, resp) = ui.allocate_exact_size(Vec2::new(full, H_ROW), Sense::click_and_drag());
    let p = ui.painter();
    p.text(
        pos2(row.left(), row.top() + 2.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(11.5),
        t18::TEXT_MUTED,
    );
    p.text(
        pos2(row.right(), row.top() + 2.0),
        Align2::RIGHT_CENTER,
        fmt(*value),
        FontId::monospace(11.5),
        t18::TEXT,
    );
    let rail = Rect::from_min_size(pos2(row.left(), row.top() + 20.0), Vec2::new(rail_w, 2.0));
    p.rect_filled(rail, 1.0, t18::hairline_strong());
    p.rect_filled(
        Rect::from_min_size(rail.min, Vec2::new(rail.width() * frac, 2.0)),
        1.0,
        t18::TXFILL,
    );
    let kx = rail.left() + rail.width() * frac;
    p.circle_filled(pos2(kx, rail.center().y), 6.0, t18::KNOB);
    p.circle_stroke(
        pos2(kx, rail.center().y),
        6.0,
        Stroke::new(1.0, t18::SUNKEN),
    );
    if resp.dragged()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let f = ((pos.x - rail.left()) / rail.width()).clamp(0.0, 1.0);
        *value = lo + (hi - lo) * f as f64;
    }
    let _ = resp;
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

/// 画布照片纹理：程序化 teal-orange 日落（320×213，一次生成会话复用）。
///
/// Programmatic teal-orange sunset texture (320x213), generated once.
fn make_photo_texture(ctx: &egui::Context) -> TextureHandle {
    let w = 320usize;
    let h = 213usize;
    let mut img = egui::ColorImage::new([w, h], vec![Color32::BLACK; w * h]);
    for y in 0..h {
        let t = y as f32 / h as f32;
        // 三段电影调色：青阴影 → 紫灰过渡 → 暖橙地平线
        let (r, g, b) = if t < 0.42 {
            let k = t / 0.42;
            (
                20.0 + (58.0 - 20.0) * k,
                36.0 + (92.0 - 36.0) * k,
                52.0 + (128.0 - 52.0) * k,
            )
        } else if t < 0.62 {
            let k = (t - 0.42) / 0.20;
            (
                58.0 + (176.0 - 58.0) * k,
                92.0 + (128.0 - 92.0) * k,
                128.0 + (110.0 - 128.0) * k,
            )
        } else {
            let k = (t - 0.62) / 0.38;
            (
                176.0 + (255.0 - 176.0) * k,
                128.0 + (214.0 - 128.0) * k,
                110.0 + (168.0 - 110.0) * k,
            )
        };
        for x in 0..w {
            img.pixels[y * w + x] = Color32::from_rgb(r as u8, g as u8, b as u8);
        }
    }
    let sc = pos2(w as f32 * 0.5, h as f32 * 0.58);
    for dy in -30i32..=30 {
        for dx in -30i32..=30 {
            let x = sc.x as i32 + dx;
            let y = sc.y as i32 + dy;
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                continue;
            }
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            if d < 12.0 {
                img.pixels[y as usize * w + x as usize] = Color32::from_rgb(0xFF, 0xE3, 0xA8);
            } else if d < 34.0 {
                let k = 1.0 - (d - 12.0) / 22.0;
                let px = &mut img.pixels[y as usize * w + x as usize];
                *px = Color32::from_rgb(
                    (0xFF as f32 * k + px.r() as f32 * (1.0 - k)) as u8,
                    (0xC4 as f32 * k + px.g() as f32 * (1.0 - k)) as u8,
                    (0x6A as f32 * k + px.b() as f32 * (1.0 - k)) as u8,
                );
            }
        }
    }
    for y in (h as f32 * 0.82) as usize..h {
        for x in 0..w {
            let t = x as f32 / w as f32;
            let ridge = (t * 9.0).sin() * 8.0 + (t * 21.0).sin() * 3.0;
            if y as f32 > h as f32 * 0.82 + ridge {
                img.pixels[y * w + x] = Color32::from_rgb(0x16, 0x30, 0x3e);
            }
        }
    }
    for y in (h as f32 * 0.90) as usize..h {
        for x in 0..w {
            let t = x as f32 / w as f32;
            let ridge = (t * 13.0).sin() * 5.0;
            if y as f32 > h as f32 * 0.90 + ridge {
                img.pixels[y * w + x] = Color32::from_rgb(0x0e, 0x1a, 0x26);
            }
        }
    }
    let mut seed = 7u32;
    for px in img.pixels.iter_mut() {
        seed = (seed * 9301 + 49297) % 233280;
        let n = (seed % 7) as i32 - 3;
        *px = Color32::from_rgb(
            (px.r() as i32 + n).clamp(0, 255) as u8,
            (px.g() as i32 + n).clamp(0, 255) as u8,
            (px.b() as i32 + n).clamp(0, 255) as u8,
        );
    }
    ctx.load_texture("v18-photo", img, egui::TextureOptions::LINEAR)
}
