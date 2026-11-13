//! 现代暗色主题（v18 方向）：indigo 强调 + 圆角 12 + 柔和投影 + Inter 字体 + 充足留白。
//!
//! 与 [`crate::theme`]（旧壳的六皮肤三层体系）完全独立：本模块只提供
//! [`apply_modern_dark`]，接收 [`egui::Context`] 一次性安装字体 + Visuals + Style，
//! 不读 AppState、不碰旧皮肤。设计稿见 docs/aevum-v18-mockup.html。
//!
//! A modern dark theme (v18 direction): indigo accent + 12 px corner radius +
//! soft shadows + Inter type + generous whitespace. Fully independent of the old
//! six-skin [`crate::theme`] system; [`apply_modern_dark`] takes an [`egui::Context`]
//! and installs fonts + Visuals + Style in one call. Mockups live in
//! docs/aevum-v18-mockup.html.

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Style,
    TextStyle, Vec2, Visuals,
};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// 强调色 #6366F1（indigo）：唯一彩色，用于选中/主按钮/进度/焦点。
///
/// Accent #6366F1 (indigo): the single color, for selection / primary buttons /
/// progress / focus.
pub const ACCENT: Color32 = Color32::from_rgb(0x63, 0x66, 0xF1);
/// 悬停态强调色（强调色提亮一档）。
///
/// Hover accent, one step lighter than [`ACCENT`].
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x7A, 0x7D, 0xF4);
/// 按压态强调色（压暗一档）。
///
/// Pressed accent, one step darker than [`ACCENT`].
pub const ACCENT_PRESSED: Color32 = Color32::from_rgb(0x4F, 0x52, 0xD6);

/// 主背景 #1E1E2E（用户钦定）。
///
/// Base background #1E1E2E (as specified).
pub const BG: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x2E);
/// 悬浮面（卡片/弹层比主背景亮一档）。
///
/// Elevated surface (cards/popups), one step lighter than [`BG`].
pub const ELEVATED: Color32 = Color32::from_rgb(0x26, 0x26, 0x3A);
/// 输入框底（比主背景暗一档，让光标对比清晰）。
///
/// Text-edit background, one step darker than [`BG`].
pub const FIELD: Color32 = Color32::from_rgb(0x18, 0x18, 0x26);

/// 主文字 #E6E6F2。
///
/// Primary text #E6E6F2.
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE6, 0xF2);
/// 次要文字。
///
/// Secondary text.
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x9C, 0xA0, 0xB8);
/// 弱文字（占位/禁用）。
///
/// Faint text (placeholder/disabled).
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x6A, 0x6E, 0x85);

/// 细边框：白 7%（深底上读作一根发丝线）。
///
/// Thin border: white at 7% -- reads as one hairline on the dark base.
pub fn border_line() -> Color32 {
    Color32::from_white_alpha(18)
}

/// 加粗字族名（真 700 face，禁假加粗）。
///
/// Bold family name (real 700 faces; synthetic bold is off the table).
pub fn bold_family() -> FontFamily {
    FontFamily::Name("modern-bold".into())
}

/// 装配字体：Inter（可变字体，默认 400）+ Noto Sans SC 500 打头，
/// 700 子集组成独立 bold 字族；等宽族与正文同链。装配一次缓存共享。
///
/// Font assembly: Inter (variable, default 400) + Noto Sans SC 500 lead; the two
/// 700 subsets form a separate bold family; the mono family shares the same chain.
/// Built once and cached -- `apply` runs on startup and on every zoom change.
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
        // bold 字族：700 子集打头，其后整条接上常规链（缺字回落为常规体，不出豆腐）。
        //
        // Bold family: 700 subsets lead, then the whole regular chain follows (missing
        // glyphs fall back to regular instead of tofu).
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

/// 现代暗色主题：调用 `ctx.set_visuals()` 与 `ctx.set_style()` 各一次，
/// 一次性安装字体、圆角 12、强调色 #6366F1、深底 #1E1E2E、柔和投影、细边框、
/// 充足留白。幂等（重复调用只是覆盖同一套值）。
///
/// The modern dark theme: calls `ctx.set_visuals()` and `ctx.set_style()` once each,
/// installing fonts, 12 px corner radius, accent #6366F1, dark base #1E1E2E, soft
/// shadows, thin borders and generous whitespace. Idempotent (re-calls overwrite
/// with the same values).
pub fn apply_modern_dark(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());

    let mut visuals = Visuals::default();
    visuals.dark_mode = true;
    // 圆角 12px：窗口/菜单/弹层/控件统一一档，可勾选小控件同样 12（16px 的
    // checkbox 会读成圆片，正是「柔和现代」要的形状）。
    //
    // 12 px radius across windows/menus/popups/widgets alike; small checkable
    // controls share it too -- a 16 px checkbox reads as a soft pill, which is the
    // intended modern shape.
    let r12 = CornerRadius::same(12);
    visuals.window_corner_radius = r12;
    visuals.menu_corner_radius = r12;
    visuals.widgets.noninteractive.corner_radius = r12;
    visuals.widgets.inactive.corner_radius = r12;
    visuals.widgets.hovered.corner_radius = r12;
    visuals.widgets.active.corner_radius = r12;
    visuals.widgets.open.corner_radius = r12;
    // 面
    visuals.panel_fill = BG;
    visuals.window_fill = ELEVATED;
    visuals.extreme_bg_color = FIELD;
    visuals.text_edit_bg_color = Some(FIELD);
    visuals.code_bg_color = FIELD;
    visuals.faint_bg_color = Color32::from_white_alpha(10);
    // 细边框：一律 1px 低 alpha；窗框白 9%。
    let hairline = egui::Stroke::new(1.0, border_line());
    visuals.window_stroke = egui::Stroke::new(1.0, Color32::from_white_alpha(24));
    visuals.widgets.noninteractive.bg_stroke = hairline;
    visuals.widgets.inactive.bg_stroke = hairline;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, Color32::from_white_alpha(40));
    visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, Color32::from_white_alpha(40));
    visuals.widgets.noninteractive.bg_fill = BG;
    visuals.widgets.noninteractive.weak_bg_fill = BG;
    visuals.widgets.inactive.bg_fill = ELEVATED;
    visuals.widgets.inactive.weak_bg_fill = ELEVATED;
    visuals.widgets.hovered.bg_fill = ACCENT_HOVER.gamma_multiply(0.28);
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
    // 选中：强调色实底 + 白字（用户恒定项）。
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0, Color32::from_white_alpha(90));
    // 柔和投影：大 blur、低 alpha、向下偏移——飘起来但不脏边。
    //
    // Soft shadows: large blur, low alpha, downward offset -- lifted, not dirty.
    visuals.window_shadow = Shadow {
        offset: [0, 12],
        blur: 32,
        spread: 0,
        color: Color32::from_black_alpha(130),
    };
    visuals.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    visuals.slider_trailing_fill = true;
    visuals.striped = false;
    visuals.handle_shape = egui::style::HandleShape::Circle;
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    visuals.window_highlight_topmost = false;

    let mut style = Style::default();
    style.visuals = visuals.clone();
    // 留白：控件内距 12×8、行距 12×10、窗口边距 14、菜单边距 12。
    //
    // Whitespace: control padding 12x8, row spacing 12x10, window margin 14,
    // menu margin 12.
    style.spacing.item_spacing = Vec2::new(12.0, 10.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style.spacing.window_margin = Margin::same(14);
    style.spacing.menu_margin = Margin::same(12);
    style.spacing.slider_width = 220.0;
    style.spacing.slider_rail_height = 4.0;
    style.spacing.interact_size = Vec2::new(40.0, 34.0);
    style.spacing.indent = 22.0;
    style.animation_time = 0.15;
    style.interaction.interact_radius = 8.0;
    style.interaction.show_tooltips_only_when_still = true;
    // 字阶：12 小字 / 14 正文与按钮 / 16 标题（700 真字族）/ 13 等宽数字。
    //
    // Type ladder: 12 small / 14 body & button / 16 heading (real 700 family) /
    // 13 mono digits.
    style.text_styles = BTreeMap::from([
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        ),
        (TextStyle::Heading, FontId::new(16.0, bold_family())),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]);
    style.drag_value_text_style = TextStyle::Monospace;

    // 两次显式调用（用户要求）：先 set_visuals()，再等价于 set_style() 的
    // set_style_of()（egui 0.37 把 set_style 改名为 set_style_of(theme, style)；
    // 深/浅两份都写同一套，主题偏好切到 Dark——本函数就是暗色主题）。
    //
    // The two explicit calls (as requested): set_visuals() first, then egui 0.37's
    // set_style() equivalent set_style_of(theme, style); both themes get the same
    // style and the preference is pinned to Dark -- this function IS the dark theme.
    ctx.set_visuals(visuals);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    ctx.set_theme(egui::ThemePreference::Dark);
}
