//! v18 主题规范单（`cargo run -p aevum-ui --example theme_spec_demo`）：
//! 配色板 / 间距刻度 / 圆角三档 / 投影 / 关键控件，同屏实渲——
//! 每个色块与控件都直接读 [`aevum_ui::theme_modern`] 的令牌，所见即所得。
//!
//! The v18 theme spec sheet: palette / spacing / radius / shadows / key controls,
//! rendered live -- every swatch and widget reads the tokens from
//! [`aevum_ui::theme_modern`], so what you see is exactly the token value.

use aevum_ui::theme_modern::*;
use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, Vec2};

fn hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

fn swatch(ui: &mut egui::Ui, name: &str, c: Color32, note: &str) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(64.0, 30.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(r, 6.0, c);
        p.rect_stroke(
            r,
            6.0,
            Stroke::new(1.0, hairline_strong()),
            egui::StrokeKind::Inside,
        );
        ui.vertical(|ui| {
            ui.label(RichText::new(name).size(12.5).color(TEXT));
            ui.label(
                RichText::new(format!("{}  {}", hex(c), note))
                    .font(FontId::new(10.5, egui::FontFamily::Monospace))
                    .color(TEXT_FAINT),
            );
        });
    });
}

fn chip(ui: &mut egui::Ui, label: &str, fill: Color32, ink: Color32, line: Option<Stroke>) {
    let pad = ui.style().spacing.button_padding;
    let size = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            FontId::new(12.0, egui::FontFamily::Proportional),
            ink,
        )
        .size()
        + pad * 2.0;
    let (r, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(r, R_CHIP as f32, fill);
    if let Some(s) = line {
        ui.painter()
            .rect_stroke(r, R_CHIP as f32, s, egui::StrokeKind::Inside);
    }
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::new(12.0, egui::FontFamily::Proportional),
        ink,
    );
}

struct Spec;
impl eframe::App for Spec {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.style_mut().spacing.item_spacing = Vec2::new(SP5, SP4);
        ui.columns(3, |cols| {
            // ============ 列 1：配色板 ============
            let ui = &mut cols[0];
            ui.heading("配色 · 背景层级");
            ui.add_space(SP2);
            swatch(ui, "BASE 底", BASE, "应用底 / 侧栏 / 面板");
            swatch(ui, "SURFACE 卡", SURFACE, "卡片 / 浮出面板");
            swatch(ui, "OVERLAY 浮层", OVERLAY, "菜单 / 弹窗");
            swatch(ui, "SUNKEN 井", SUNKEN, "输入井 / 画布井");
            ui.add_space(SP2);
            swatch(ui, "强调 ACCENT", ACCENT, "唯一彩色 · 选中/主按钮");
            swatch(ui, "强调 hover", ACCENT_HOVER, "提亮一档");
            swatch(ui, "强调按压", ACCENT_PRESSED, "压暗一档");
            ui.add_space(SP2);
            swatch(ui, "文字主", TEXT, "正文 400");
            swatch(ui, "文字次要", TEXT_MUTED, "标签 / 元信息");
            swatch(ui, "文字弱", TEXT_FAINT, "占位 / 禁用");
            ui.add_space(SP2);
            swatch(ui, "状态 OK", OK, "状态点专用");
            swatch(ui, "状态 WARN", WARN, "降速 / 暂停");
            swatch(ui, "状态 DANGER", DANGER, "错误横幅专用");
            ui.add_space(SP2);
            ui.label(
                RichText::new("发丝线（区块唯一分隔）")
                    .size(12.5)
                    .color(TEXT),
            );
            let (r, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.5), Sense::hover());
            ui.painter().rect_filled(r, 0.0, hairline());
            ui.label(
                RichText::new("白 7%  ·  hover 用白 12%")
                    .font(FontId::new(10.5, egui::FontFamily::Monospace))
                    .color(TEXT_FAINT),
            );

            // ============ 列 2：间距 / 圆角 / 投影 ============
            let ui = &mut cols[1];
            ui.heading("间距刻度（4 基准网格）");
            ui.add_space(SP2);
            for (name, v) in [
                ("SP1", SP1),
                ("SP2", SP2),
                ("SP3", SP3),
                ("SP4", SP4),
                ("SP5", SP5),
                ("SP6", SP6),
            ] {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(name)
                            .font(FontId::new(11.0, egui::FontFamily::Monospace))
                            .color(TEXT_FAINT),
                    );
                    let (r, _) = ui.allocate_exact_size(Vec2::new(v, 10.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, ACCENT.gamma_multiply(0.7));
                    ui.label(
                        RichText::new(format!("{}px", v as i32))
                            .font(FontId::new(11.0, egui::FontFamily::Monospace))
                            .color(TEXT_MUTED),
                    );
                });
            }
            ui.add_space(SP2);
            ui.label(
                RichText::new(format!(
                    "行高 ROW_H = {}px · 控件内距 12×7 · 窗边距 {}px",
                    ROW_H as i32, WIN_MARGIN as i32
                ))
                .size(12.0)
                .color(TEXT_MUTED),
            );
            ui.add_space(SP3);
            ui.heading("圆角三档");
            ui.add_space(SP2);
            ui.horizontal(|ui| {
                for (name, r, w, h) in [
                    ("徽标 6", R_CHIP, 46.0, 24.0),
                    ("控件 8", R_CONTROL, 56.0, 30.0),
                    ("窗/卡 12", R_CARD, 70.0, 40.0),
                ] {
                    let (r_, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(r_, r as f32, SURFACE);
                    p.rect_stroke(
                        r_,
                        r as f32,
                        Stroke::new(1.0, hairline_strong()),
                        egui::StrokeKind::Inside,
                    );
                    p.text(
                        r_.center(),
                        Align2::CENTER_CENTER,
                        name,
                        FontId::new(11.0, egui::FontFamily::Proportional),
                        TEXT_MUTED,
                    );
                }
            });
            ui.add_space(SP3);
            ui.heading("投影两档（柔和 · 大 blur · 低 alpha）");
            ui.add_space(SP2);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                let (r, _) = ui.allocate_exact_size(Vec2::new(150.0, 64.0), Sense::hover());
                let r2 = ui
                    .allocate_exact_size(Vec2::new(150.0, 56.0), Sense::hover())
                    .0;
                let p = ui.painter().clone();
                p.rect_filled(
                    r.translate(Vec2::new(0.0, 8.0)),
                    8.0,
                    Color32::from_black_alpha(28),
                );
                p.rect_filled(r, 8.0, SURFACE);
                p.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    "窗 36 blur",
                    FontId::new(11.0, egui::FontFamily::Proportional),
                    TEXT_MUTED,
                );
                p.rect_filled(
                    r2.translate(Vec2::new(0.0, 6.0)),
                    8.0,
                    Color32::from_black_alpha(22),
                );
                p.rect_filled(r2, 8.0, OVERLAY);
                p.text(
                    r2.center(),
                    Align2::CENTER_CENTER,
                    "浮层 28 blur",
                    FontId::new(11.0, egui::FontFamily::Proportional),
                    TEXT_MUTED,
                );
            });
            ui.add_space(SP3);
            ui.heading("字体（Inter + Noto Sans SC）");
            ui.add_space(SP2);
            ui.label(
                RichText::new("标题 15.5 / 700 真字族")
                    .heading()
                    .color(TEXT),
            );
            ui.label(
                RichText::new("正文 14 / 400 —— 界面文字皆出于此")
                    .size(14.0)
                    .color(TEXT),
            );
            ui.label(
                RichText::new("小字 11 —— 元信息与角标")
                    .small()
                    .color(TEXT_MUTED),
            );
            ui.label(
                RichText::new("等宽 13 —— 3840 × 2160 · 5500 K · +0.35 EV")
                    .font(FontId::new(13.0, egui::FontFamily::Monospace))
                    .color(TEXT),
            );

            // ============ 列 3：关键控件 ============
            let ui = &mut cols[2];
            ui.heading("关键控件");
            ui.add_space(SP2);
            ui.label(RichText::new("按钮四态").size(12.5).color(TEXT_MUTED));
            ui.add_space(SP1);
            ui.horizontal(|ui| {
                ui.add(
                    egui::Button::new(RichText::new("主按钮").color(Color32::WHITE)).fill(ACCENT),
                );
                ui.add(egui::Button::new("次按钮").fill(SURFACE));
                let _ = ui.add(
                    egui::Button::new(RichText::new("悬停态").color(Color32::WHITE))
                        .fill(ACCENT_HOVER.gamma_multiply(0.26)),
                );
                ui.add(
                    egui::Button::new(RichText::new("禁用").color(TEXT_FAINT))
                        .fill(BASE)
                        .stroke(Stroke::new(1.0, hairline())),
                );
            });
            ui.add_space(SP1);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::new(96.0, 32.0), Sense::click());
                let p = ui.painter();
                let hov = r.contains(ui.input(|i| i.pointer.latest_pos().unwrap_or_default()));
                p.rect_filled(
                    r,
                    R_CONTROL as f32,
                    if hov {
                        Color32::from_white_alpha(14)
                    } else {
                        SURFACE
                    },
                );
                p.rect_stroke(
                    r,
                    R_CONTROL as f32,
                    Stroke::new(1.0, if hov { hairline_strong() } else { hairline() }),
                    egui::StrokeKind::Inside,
                );
                p.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    "幽灵按钮 hov",
                    FontId::new(13.0, egui::FontFamily::Proportional),
                    TEXT,
                );
                ui.add(egui::Button::new(RichText::new("危险").color(Color32::WHITE)).fill(DANGER));
            });
            ui.add_space(SP3);
            ui.label(
                RichText::new("滑块（4px 轨 · 白 10% · 强调尾随填充 · 圆点手柄）")
                    .size(12.5)
                    .color(TEXT_MUTED),
            );
            ui.add_space(SP1);
            let mut exposure = 0.35f32;
            ui.add(
                egui::Slider::new(&mut exposure, -3.0..=3.0)
                    .text("曝光")
                    .show_value(true),
            );
            let mut temp = 5500f32;
            ui.add(
                egui::Slider::new(&mut temp, 2000.0..=9000.0)
                    .text("色温")
                    .show_value(true),
            );
            ui.add_space(SP2);
            ui.label(
                RichText::new("chips（徽标 6px · 选中=淡染+强调字）")
                    .size(12.5)
                    .color(TEXT_MUTED),
            );
            ui.add_space(SP1);
            ui.horizontal(|ui| {
                chip(
                    ui,
                    "整个序列",
                    accent_tint(),
                    ACCENT,
                    Some(Stroke::new(1.0, ACCENT.gamma_multiply(0.6))),
                );
                chip(
                    ui,
                    "选中帧",
                    SURFACE,
                    TEXT,
                    Some(Stroke::new(1.0, hairline())),
                );
                chip(
                    ui,
                    "当前帧",
                    SURFACE,
                    TEXT_MUTED,
                    Some(Stroke::new(1.0, hairline())),
                );
                chip(
                    ui,
                    "渲帧中",
                    SURFACE,
                    OK,
                    Some(Stroke::new(1.0, hairline())),
                );
            });
            ui.add_space(SP3);
            ui.label(
                RichText::new("侧栏行（32px 行高 · 选中=强调实底白字）")
                    .size(12.5)
                    .color(TEXT_MUTED),
            );
            ui.add_space(SP1);
            for (label, state) in [("显影", "selected"), ("帧表", "hover"), ("导出", "normal")]
            {
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
                let p = ui.painter();
                if state == "selected" {
                    p.rect_filled(r, R_CONTROL as f32, ACCENT);
                } else if state == "hover" {
                    p.rect_filled(r, R_CONTROL as f32, Color32::from_white_alpha(14));
                }
                let ink = if state == "selected" {
                    Color32::WHITE
                } else {
                    TEXT_MUTED
                };
                p.circle_filled(egui::pos2(r.left() + 14.0, r.center().y), 3.0, ink);
                p.text(
                    egui::pos2(r.left() + 28.0, r.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::new(13.0, egui::FontFamily::Proportional),
                    ink,
                );
            }
            ui.add_space(SP3);
            ui.label(
                RichText::new("输入井 / 进度 / 勾选")
                    .size(12.5)
                    .color(TEXT_MUTED),
            );
            ui.add_space(SP1);
            let mut q = String::new();
            ui.add(
                egui::TextEdit::singleline(&mut q)
                    .hint_text("搜索…")
                    .desired_width(ui.available_width()),
            );
            ui.add_space(SP1);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width() - 90.0, 6.0),
                    Sense::hover(),
                );
                let p = ui.painter();
                p.rect_filled(r, 3.0, Color32::from_white_alpha(26));
                p.rect_filled(
                    Rect::from_min_size(r.min, Vec2::new(r.width() * 0.42, r.height())),
                    3.0,
                    ACCENT,
                );
                ui.label(
                    RichText::new("42%")
                        .font(FontId::new(12.0, egui::FontFamily::Monospace))
                        .color(TEXT_MUTED),
                );
            });
            ui.checkbox(&mut true_or_false(), "仅显示已改动");
            ui.add_space(SP2);
            ui.label(
                RichText::new("面板分节（600 标题 + 发丝线）")
                    .size(12.5)
                    .color(TEXT_MUTED),
            );
            let (r, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.5), Sense::hover());
            ui.painter().rect_filled(r, 0.0, hairline());
            ui.add_space(SP1);
            ui.label(RichText::new("基本").strong().color(TEXT));
            ui.label(
                RichText::new("色温 5500 K · 曝光 +0.35 · 对比度 +12")
                    .size(12.5)
                    .color(TEXT_MUTED),
            );
        });
    }
}

fn true_or_false() -> bool {
    false
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("恒晷 v18 · 主题规范单")
            .with_inner_size([1360.0, 860.0])
            .with_position([100.0, 80.0]),
        ..Default::default()
    };
    eframe::run_native(
        "恒晷 v18 · 主题规范单",
        opts,
        Box::new(|cc| {
            apply_modern_dark(&cc.egui_ctx);
            Ok(Box::new(Spec))
        }),
    )
}
