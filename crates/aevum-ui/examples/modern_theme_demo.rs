//! 现代暗色主题预览（`cargo run -p aevum-ui --example modern_theme_demo`）：
//! 一屏摆齐常用控件，验证 [`aevum_ui::theme_modern::apply_modern_dark`] 的观感。
//!
//! Modern dark theme preview: one screen of common widgets exercising
//! [`aevum_ui::theme_modern::apply_modern_dark`].

use aevum_ui::theme_modern::{ACCENT, BG, ELEVATED, FIELD, apply_modern_dark};
use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, Vec2};

struct Demo {
    exposure: f32,
    contrast: f32,
    temperature: f32,
    checked: bool,
    pick: usize,
    query: String,
}

impl Default for Demo {
    fn default() -> Self {
        Self {
            exposure: 0.35,
            contrast: 12.0,
            temperature: 5500.0,
            checked: false,
            pick: 0,
            query: String::new(),
        }
    }
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("dock")
            .resizable(false)
            .exact_size(300.0)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.heading("直方图");
                ui.add_space(6.0);
                // 直方图占位：accent 色随机峰（演示圆角与材质）。
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 88.0), Sense::hover());
                let p = ui.painter();
                let seed = [
                    0.62, 0.85, 0.4, 0.7, 0.5, 0.9, 0.34, 0.62, 0.78, 0.46, 0.68, 0.3,
                ];
                let n = seed.len();
                let bw = rect.width() / n as f32;
                for (i, &v) in seed.iter().enumerate() {
                    let h = v * rect.height();
                    p.rect_filled(
                        Rect::from_min_size(
                            egui::pos2(rect.left() + i as f32 * bw + 1.5, rect.bottom() - h),
                            Vec2::new(bw - 3.0, h),
                        ),
                        3.0,
                        Color32::from_rgba_unmultiplied(0x63, 0x66, 0xF1, 140),
                    );
                }
                ui.add_space(8.0);
                ui.separator();
                ui.heading("基本");
                ui.add_space(4.0);
                ui.scope(|ui| {
                    ui.style_mut().spacing.slider_width = 150.0;
                    egui::Grid::new("sliders")
                        .num_columns(2)
                        .spacing([10.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("曝光");
                            ui.add(
                                egui::Slider::new(&mut self.exposure, -3.0..=3.0)
                                    .text("")
                                    .show_value(true),
                            );
                            ui.end_row();
                            ui.label("对比度");
                            ui.add(
                                egui::Slider::new(&mut self.contrast, -50.0..=50.0)
                                    .text("")
                                    .show_value(true),
                            );
                            ui.end_row();
                            ui.label("色温");
                            ui.add(
                                egui::Slider::new(&mut self.temperature, 2000.0..=9000.0)
                                    .text("")
                                    .show_value(true),
                            );
                            ui.end_row();
                        });
                });
                ui.checkbox(&mut self.checked, "仅显示已改动");
                ui.add_space(4.0);
                ui.label("输出容器");
                egui::ComboBox::from_id_salt("codec")
                    .selected_text(["H.265 / HEVC", "H.264 / AVC", "ProRes"][self.pick])
                    .width(250.0)
                    .show_ui(ui, |ui| {
                        for (i, t) in ["H.265 / HEVC", "H.264 / AVC", "ProRes"].iter().enumerate() {
                            ui.selectable_value(&mut self.pick, i, *t);
                        }
                    });
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("搜索预设…")
                        .desired_width(250.0),
                );
                ui.add_space(8.0);
                if ui
                    .add(
                        egui::Button::new(RichText::new("立即导出").strong())
                            .min_size(Vec2::new(ui.available_width(), 36.0)),
                    )
                    .clicked()
                {
                    // 演示无动作
                }
            });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("显影工作台");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("松赞林寺-198 · 48 帧 · NEF").size(12.0).color(Color32::from_rgb(0x9C, 0xA0, 0xB8)));
                });
            });
            ui.add_space(10.0);

            // 照片卡：渐变 + 太阳（圆角 12 的画布材质样张）。
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 300.0), Sense::hover());
            let p = ui.painter();
            p.rect_filled(rect, 12.0, FIELD);
            let n = 40;
            let top = Color32::from_rgb(0x1A, 0x1A, 0x3E);
            let mid = Color32::from_rgb(0xB3, 0x5C, 0x3E);
            let low = Color32::from_rgb(0xE0, 0x8C, 0x4A);
            for i in 0..n {
                let t = i as f32 / n as f32;
                let c = if t < 0.55 {
                    lerp(top, mid, t / 0.55)
                } else {
                    lerp(mid, low, (t - 0.55) / 0.45)
                };
                let h = rect.height() / n as f32;
                p.rect_filled(
                    Rect::from_min_size(
                        egui::pos2(rect.left() + 1.0, rect.top() + i as f32 * h),
                        Vec2::new(rect.width() - 2.0, h + 1.0),
                    ),
                    0.0,
                    c,
                );
            }
            let cx = rect.center().x;
            let sun_y = rect.top() + rect.height() * 0.66;
            p.circle_filled(egui::pos2(cx, sun_y), 26.0, Color32::from_rgb(0xFF, 0xE3, 0xA8));
            p.circle_filled(egui::pos2(cx, sun_y), 52.0, Color32::from_rgba_unmultiplied(0xFF, 0xC4, 0x6A, 70));
            let ridge = rect.bottom() - rect.height() * 0.16;
            p.rect_filled(
                Rect::from_min_size(
                    egui::pos2(rect.left() + 1.0, ridge),
                    Vec2::new(rect.width() - 2.0, rect.bottom() - ridge - 1.0),
                ),
                0.0,
                Color32::from_rgb(0x14, 0x10, 0x1C),
            );

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let _ = ui.button("导入 XMP");
                let _ = ui.button("导出 XMP");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(160.0, 8.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(r, 4.0, BG);
                    p.rect_filled(Rect::from_min_size(r.min, Vec2::new(r.width() * 0.42, r.height())), 4.0, ACCENT);
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let chips = ["整个序列", "选中帧", "当前帧"];
                for (i, c) in chips.iter().enumerate() {
                    let sel = i == 0;
                    let txt = RichText::new(*c).size(13.0);
                    let btn = if sel {
                        egui::Button::new(RichText::new(*c).color(Color32::WHITE).size(13.0))
                            .fill(ACCENT)
                            .stroke(Stroke::NONE)
                    } else {
                        egui::Button::new(txt.color(Color32::from_rgb(0xC9, 0xCB, 0xDD)))
                            .fill(ELEVATED)
                            .stroke(Stroke::new(1.0, Color32::from_white_alpha(18)))
                    };
                    let _ = ui.add(btn);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Button::new(RichText::new("禁用按钮").size(13.0)).sense(Sense::hover()).fill(BG).stroke(Stroke::new(1.0, Color32::from_white_alpha(6))));
                });
            });
            ui.add_space(8.0);
            ui.label(RichText::new("圆角 12 · 强调色 #6366F1 · 底 #1E1E2E · 柔和投影 · 细边框 · Inter · 留白 12/8 —— 以上每个控件都在这套令牌上").size(12.0).color(Color32::from_rgb(0x9C, 0xA0, 0xB8)));
            ui.label(RichText::new("等宽数字：3840 × 2160 · 5500 K · +0.35 EV · CRF 18").size(12.0).color(Color32::from_rgb(0x9C, 0xA0, 0xB8)));
        });
    }
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("恒晷 · Modern Dark")
            .with_inner_size([1120.0, 720.0])
            .with_position([160.0, 120.0]),
        ..Default::default()
    };
    eframe::run_native(
        "恒晷 · Modern Dark",
        opts,
        Box::new(|cc| {
            apply_modern_dark(&cc.egui_ctx);
            // 主题底色透到窗口清除色（避免 resize 露白）。
            cc.egui_ctx.style_mut_of(egui::Theme::Dark, |s| {
                s.visuals.panel_fill = BG;
                s.visuals.extreme_bg_color = FIELD;
                let _ = CornerRadius::same(12);
                let _ = Stroke::NONE;
            });
            Ok(Box::new(Demo::default()))
        }),
    )
}
