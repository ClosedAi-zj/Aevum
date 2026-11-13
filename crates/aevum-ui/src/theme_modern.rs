//! v18 统一令牌模块（方向：Arc / Linear / Raycast 系现代暗色工作台）。
//!
//! 全应用唯一色源：**任何其它模块禁止写死 `egui::Color32` 裸色**——一律经本模块
//! 的常量与访问器取色。应用入口调用 [`apply_modern_dark`] 一次性
//! `set_visuals` + `set_style`（egui 0.37 的等价入口是 `set_style_of`）套用全局。
//!
//! 层级语言：`BASE 底 → SURFACE 卡 → OVERLAY 浮层 → SUNKEN 井`，
//! 分隔只用一根白 α 发丝线；强调色唯一（indigo #6366F1），选中=实底+白字，
//! hover=白 α 极淡染；圆角三档（窗/卡 12 · 控件 8 · 徽标 6）；
//! 投影两档（窗/浮层）大 blur 低 alpha；字体 Inter + Noto Sans SC，真 700 字族。
//!
//! The single color source of the app: no other module may hardcode
//! `egui::Color32` literals -- everything goes through these constants and
//! accessors. The app entry calls [`apply_modern_dark`] once, which runs
//! `set_visuals` + `set_style` (egui 0.37's `set_style_of`) globally.
//! Layer language: BASE -> SURFACE -> OVERLAY -> SUNKEN, hairline separators,
//! one accent (indigo), three radius tiers, two soft shadow tiers.

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Style,
    TextStyle, Vec2, Visuals,
};
use std::collections::BTreeMap;
use std::sync::OnceLock;

// ============ 强调色（唯一彩色） ============

/// 强调色 #6366F1：选中/主按钮/进度/焦点，全应用唯一彩色。
///
/// Accent #6366F1: selection / primary button / progress / focus -- the only color.
pub const ACCENT: Color32 = Color32::from_rgb(0x8B, 0x5C, 0xF6);
/// 强调色 hover（提亮一档）。
///
/// Accent hover, one step lighter.
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0xA7, 0x8B, 0xFA);
/// 强调色按压（压暗一档）。
///
/// Accent pressed, one step darker.
pub const ACCENT_PRESSED: Color32 = Color32::from_rgb(0x7C, 0x3A, 0xED);
/// 强调色淡染（chip/徽标的 18% 染底）。
///
/// Accent tint for chips/badges (~18% wash).
pub fn accent_tint() -> Color32 {
    Color32::from_rgba_unmultiplied(0x8B, 0x5C, 0xF6, 46)
}

// ============ 背景层级（由深到浅） ============

/// SUNKEN：输入井/画布井——比底再深一档，靠「凹」读出可输入。
///
/// SUNKEN: input wells / canvas well, one step below BASE.
pub const SUNKEN: Color32 = Color32::from_rgb(0x10, 0x10, 0x18);
/// BASE：应用底/侧栏/面板——#1E1E2E。
///
/// BASE: app background / sidebar / panels -- #1E1E2E.
pub const BASE: Color32 = Color32::from_rgb(0x17, 0x17, 0x22);
/// SURFACE：卡片/浮出面板——比 BASE 亮一档。
///
/// SURFACE: cards / raised panels, one step above BASE.
pub const SURFACE: Color32 = Color32::from_rgb(0x1F, 0x1F, 0x2E);
/// OVERLAY：弹层/菜单/浮窗——再亮一档，配合投影读出「浮」。
///
/// OVERLAY: popovers / menus / floats, above SURFACE, paired with a shadow.
pub const OVERLAY: Color32 = Color32::from_rgb(0x26, 0x26, 0x38);

/// 旧名兼容：`ELEVATED` = [`SURFACE`]，`FIELD` = [`SUNKEN`]，`BG` = [`BASE`]。
///
/// Legacy aliases kept so earlier examples keep compiling.
pub const ELEVATED: Color32 = SURFACE;
pub const FIELD: Color32 = SUNKEN;
pub const BG: Color32 = BASE;

// ============ 发丝分隔线 ============

/// 发丝线：白 7%——区块间唯一分隔手段（禁 bevel/凹槽/条纹）。
///
/// Hairline: white at 7% -- the only separator between sections.
pub fn hairline() -> Color32 {
    Color32::from_white_alpha(18)
}
/// 强发丝线：白 12%（hover 描边/窗框）。
///
/// Strong hairline: white at 12% (hover outline / window frame).
pub const TXFILL: Color32 = Color32::from_rgb(0xC9, 0xCD, 0xD9);
pub const KNOB: Color32 = Color32::WHITE;
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xA7, 0x8B, 0xFA);
pub const ON_ACCENT: Color32 = Color32::WHITE;
pub const STAR: Color32 = Color32::from_rgb(0xF2, 0xC9, 0x4C);

pub fn hairline_strong() -> Color32 {
    Color32::from_white_alpha(30)
}

// ============ 文字阶梯 ============

/// 主文字。
///
/// Primary text.
pub const TEXT: Color32 = Color32::from_rgb(0xE9, 0xEA, 0xF6);
/// 次要文字（标签/元信息）。
///
/// Secondary text (labels / metadata).
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0xA6, 0xAB, 0xC4);
/// 弱文字（占位/禁用）。
///
/// Faint text (placeholder / disabled).
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x71, 0x76, 0x8E);

// ============ 状态色（只出现在状态点/横幅，不做控件主色） ============

/// 完成/成功。
///
/// Done / success.
pub const OK: Color32 = Color32::from_rgb(0x5F, 0xBF, 0x7A);
/// 警示（降速/暂停/长任务）。
///
/// Warning (slowed / paused / long job).
pub const WARN: Color32 = Color32::from_rgb(0xE0, 0xB1, 0x5E);
/// 错误。
///
/// Error.
pub const DANGER: Color32 = Color32::from_rgb(0xF8, 0x71, 0x71);

// ============ 圆角三档 ============

/// 窗/卡档：12。
///
/// Window / card tier: 12.
pub const R_CARD: u8 = 12;
/// 控件档：8（按钮/输入/菜单项）。
///
/// Control tier: 8 (buttons / inputs / menu rows).
pub const R_CONTROL: u8 = 8;
/// 徽标档：6（chip/小标签）。
///
/// Badge tier: 6 (chips / small tags).
pub const R_CHIP: u8 = 6;

// ============ 间距刻度（4 基准网格） ============

/// 间距刻度：4 / 8 / 12 / 16 / 20 / 24。
///
/// Spacing scale: 4 / 8 / 12 / 16 / 20 / 24.
pub const SP1: f32 = 4.0;
pub const SP2: f32 = 8.0;
pub const SP3: f32 = 12.0;
pub const SP4: f32 = 16.0;
pub const SP5: f32 = 20.0;
pub const SP6: f32 = 24.0;
/// 列表行高。
///
/// List row height.
pub const ROW_H: f32 = 32.0;
/// 控件内距。
///
/// Control padding.
pub fn pad_control() -> Vec2 {
    Vec2::new(12.0, 7.0)
}
/// 窗口/页面边距。
///
/// Window / page margin.
pub const WIN_MARGIN: f32 = 16.0;

// ============ 投影两档 ============

/// 窗投影：大 blur 低 alpha，向下飘。
///
/// Window shadow: large blur, low alpha, drifting down.
pub fn shadow_window() -> Shadow {
    Shadow {
        offset: [0, 14],
        blur: 36,
        spread: 0,
        color: Color32::from_black_alpha(120),
    }
}
/// 浮层投影：比窗投影收一档。
///
/// Popup shadow, one tier tighter than [`shadow_window`].
pub fn shadow_popup() -> Shadow {
    Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(100),
    }
}

/// 加粗字族名（真 700 face；禁假加粗/font-synthesis）。
///
/// Bold family name (real 700 faces; synthetic bold forbidden).
pub fn bold_family() -> FontFamily {
    FontFamily::Name("v18-bold".into())
}

/// 字体装配：Inter（可变，400）+ Noto Sans SC 500 打头，700 子集独立 bold 族；
/// 等宽族与正文同链（数字与中文同脸）。装配一次缓存。
///
/// Font assembly: Inter (variable, 400) + Noto Sans SC 500 lead; the 700 subsets
/// form a separate bold family; the mono family shares the body chain. Built once.
fn font_definitions() -> FontDefinitions {
    static DEFS: OnceLock<FontDefinitions> = OnceLock::new();
    DEFS.get_or_init(|| {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "inter".to_owned(),
            FontData::from_static(aevum_core::fonts::INTER).into(),
        );
        fonts.font_data.insert(
            "noto".to_owned(),
            FontData::from_static(aevum_core::fonts::NOTO_SC).into(),
        );
        fonts.font_data.insert(
            "inter-bold".to_owned(),
            FontData::from_static(aevum_core::fonts::INTER_BOLD).into(),
        );
        fonts.font_data.insert(
            "noto-bold".to_owned(),
            FontData::from_static(aevum_core::fonts::NOTO_SC_BOLD).into(),
        );
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let list = fonts.families.entry(family).or_default();
            list.insert(0, "inter".to_owned());
            list.insert(1, "noto".to_owned());
        }
        let bold: Vec<String> = vec![
            "inter-bold".to_owned(),
            "noto-bold".to_owned(),
            "inter".to_owned(),
            "noto".to_owned(),
        ];
        fonts.families.insert(bold_family(), bold);
        fonts
    })
    .clone()
}

/// 应用入口处一次性套用：先装字体，再 `set_visuals()` + `set_style_of()`
/// （egui 0.37 的 set_style 等价入口），并把主题偏好钉在 Dark。幂等。
///
/// Apply once at the app entry: fonts first, then `set_visuals()` +
/// `set_style_of()` (egui 0.37's renamed `set_style`), pinning the theme
/// preference to Dark. Idempotent.
#[allow(clippy::field_reassign_with_default)] // 有意在 Default 上逐项覆盖：Visuals 三十余字段只有十来项需要改
pub fn apply_modern_dark(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());

    let mut visuals = Visuals::default();
    visuals.dark_mode = true;
    // 圆角三档：窗/卡 12，控件 8；菜单/弹层按窗档。
    let r_win = CornerRadius::same(R_CARD);
    let r_ctl = CornerRadius::same(8);
    visuals.window_corner_radius = r_win;
    visuals.menu_corner_radius = r_win;
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = r_ctl;
    }
    // 背景层级
    visuals.panel_fill = BASE;
    visuals.window_fill = OVERLAY;
    visuals.extreme_bg_color = SUNKEN;
    visuals.text_edit_bg_color = Some(SUNKEN);
    visuals.code_bg_color = SUNKEN;
    visuals.faint_bg_color = Color32::from_white_alpha(10);
    // 细边框
    visuals.window_stroke = egui::Stroke::new(1.0, hairline_strong());
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, hairline());
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, hairline());
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, hairline_strong());
    visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, hairline_strong());
    visuals.widgets.noninteractive.bg_fill = BASE;
    visuals.widgets.noninteractive.weak_bg_fill = BASE;
    visuals.widgets.inactive.bg_fill = SURFACE;
    visuals.widgets.inactive.weak_bg_fill = SURFACE;
    visuals.widgets.hovered.bg_fill = ACCENT_HOVER.gamma_multiply(0.26);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_white_alpha(14);
    visuals.widgets.active.bg_fill = ACCENT_PRESSED;
    visuals.widgets.active.weak_bg_fill = ACCENT_PRESSED;
    visuals.widgets.open.bg_fill = Color32::from_white_alpha(14);
    visuals.widgets.open.weak_bg_fill = Color32::from_white_alpha(14);
    // 文字
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_MUTED);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, TEXT);
    // 选中：强调色实底 + 白字
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0, Color32::from_white_alpha(90));
    // 柔和投影两档
    visuals.window_shadow = shadow_window();
    visuals.popup_shadow = shadow_popup();
    visuals.slider_trailing_fill = true;
    visuals.striped = false;
    visuals.handle_shape = egui::style::HandleShape::Circle;
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    visuals.window_highlight_topmost = false;

    let mut style = Style::default();
    style.visuals = visuals.clone();
    // 留白刻度：行距 12×10、控件内距 12×7、窗边距 16、菜单边距 12。
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.window_margin = Margin::same(WIN_MARGIN as i8);
    style.spacing.menu_margin = Margin::same(12);
    style.spacing.slider_width = 200.0;
    style.spacing.slider_rail_height = 4.0;
    style.spacing.interact_size = Vec2::new(40.0, 26.0);
    style.spacing.indent = 22.0;
    style.animation_time = 0.15;
    style.interaction.interact_radius = 8.0;
    style.interaction.show_tooltips_only_when_still = true;
    // 字阶（12px 密度体系）：10.5 元信息 / 12 正文与按钮 / 13 标题（700）/ 11.5 等宽。
    style.text_styles = BTreeMap::from([
        (
            TextStyle::Small,
            FontId::new(10.5, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(12.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(11.5, FontFamily::Proportional),
        ),
        (TextStyle::Heading, FontId::new(13.0, bold_family())),
        (
            TextStyle::Monospace,
            FontId::new(11.5, FontFamily::Monospace),
        ),
    ]);
    style.drag_value_text_style = TextStyle::Monospace;

    // 两次显式调用：set_visuals() + set_style_of()（0.37 的 set_style）。
    ctx.set_visuals(visuals);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    ctx.set_theme(egui::ThemePreference::Dark);
}
