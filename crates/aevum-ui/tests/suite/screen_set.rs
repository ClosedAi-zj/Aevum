//! 设置屏判据：五页签真实内容 + 静态无「下一版接入」+ 返回回路畅通。
//!
//! Settings-screen criteria: five tabs with real content, no coming-soon toast,
//! and a working back route to dev.
//!
//! Set criteria for T5a.

use aevum_ui::i18n::{self, Lang};
use aevum_ui::state::{AppState, Module};
use aevum_ui::{shell, theme};

use egui::epaint::ClippedShape;
use egui::{Pos2, Rect, Shape};

fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
        max_texture_side: Some(8192),
        ..Default::default()
    }
}

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::apply(&ctx, 1.0);
    ctx
}

fn paint(ctx: &egui::Context, state: &mut AppState) -> Vec<ClippedShape> {
    for _ in 0..3 {
        let mut warm = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
        warm.textures_delta.clear();
    }
    let mut out = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
    let shapes = out.shapes;
    out.textures_delta.clear();
    shapes
}

fn text_rects(shapes: &[ClippedShape]) -> Vec<(String, Rect)> {
    let mut out = Vec::new();
    for cs in shapes {
        walk_text(&cs.shape, &mut out);
    }
    out
}

fn walk_text(shape: &Shape, out: &mut Vec<(String, Rect)>) {
    match shape {
        Shape::Text(t) => out.push((
            t.galley.text().to_owned(),
            t.galley.rect.translate(t.pos.to_vec2()),
        )),
        Shape::Vec(v) => {
            for s in v {
                walk_text(s, out);
            }
        }
        _ => {}
    }
}

fn click(ctx: &egui::Context, state: &mut AppState, pos: Pos2) {
    let mut input = raw_input();
    input.events.push(egui::Event::PointerMoved(pos));
    input.events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    input.events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    let mut out = ctx.run_ui(input, |ui| shell::ui(state, ui));
    out.textures_delta.clear();
    let mut settled = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
    settled.textures_delta.clear();
}

/// 点某一行文字的正中；只在给定 x 区间找，避开右侧坞同名页签。
fn click_label(
    ctx: &egui::Context,
    state: &mut AppState,
    rows: &[(String, Rect)],
    label: &str,
    x_max: f32,
) {
    let pos = rows
        .iter()
        .filter(|(text, r)| text == label && r.center().x < x_max)
        .min_by(|a, b| a.1.left().total_cmp(&b.1.left()))
        .map(|(_, r)| r.center())
        .unwrap_or_else(|| panic!("找不到可点的「{label}」标签"));
    click(ctx, state, pos);
}

fn clean_v21() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell/v21_demo.rs");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到 v21_demo.rs：{e}"));
    raw.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn screen_set_has_real_tabs_and_no_fake_controls() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    // 静态：回加占位 toast 会直接变红。
    // Static: re-adding the placeholder toast turns this red.
    let src = clean_v21();
    assert!(
        !src.contains("fn toast_next_build") && !src.contains("下一版接入"),
        "set screen still carries the coming-soon toast"
    );

    let ctx = context();
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    state.module = Module::Develop;

    // 左轨齿轮（x<120 的「设置」标签）是入口，点开后 0 帧也该完整显示。
    // The rail gear (the "设置" label at x<120) is the entry; zero frames still
    // render the whole page.
    let dev = text_rects(&paint(&ctx, &mut state));
    let dev_texts: Vec<&str> = dev.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        !dev_texts.contains(&"快捷键"),
        "dev screen should not paint the settings tab list"
    );
    click_label(&ctx, &mut state, &dev, i18n::t!("设置"), 120.0);

    let rows = text_rects(&paint(&ctx, &mut state));
    let texts: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
    for tab in [
        i18n::t!("外观"),
        i18n::t!("快捷键"),
        i18n::t!("信息"),
        i18n::t!("关于"),
        i18n::t!("发布说明"),
    ] {
        assert!(texts.contains(&tab), "settings well is missing tab: {tab}");
    }
    assert!(
        !texts.contains(&"即将接入"),
        "settings must not paint the placeholder copy"
    );

    // 右列内容随页签切换；默认值必须是真实档位，不是假开关。
    // The right column changes with the tab; the default shows real values.
    //
    // 这里只按列取窗：左栏页签文字落在 x≈92–116，右栏文字从左栏右缘 + 24 内距
    // 起排（面板 8px 贴井后是 x≈324），所以 300 是干净的分栏缝。此前写 330 是
    // 按 24px 贴井那版的 340 卡出来的；面板贴边（2026-11-14）后右列只左移 16px
    // 就掉出了这个窗，判据红的是自己的魔数、不是界面。
    //
    // The window here only separates the columns: left-column tab labels sit at
    // x≈92-116, right-column copy starts at the left column's right edge plus 24
    // (x≈324 once the panel hugs the well by 8), so 300 is the clean seam. The old
    // 330 was cut against the 24 px-inset version's 340; once the panel moved to the
    // edge the right column shifted 16 px left and fell out of the window -- the
    // criterion was red on its own magic number, not on the UI.
    fn right_has(ctx: &egui::Context, state: &mut AppState, label: &str) -> bool {
        text_rects(&paint(ctx, state))
            .into_iter()
            .filter(|(_, r)| {
                r.left() > 300.0 && r.right() < 960.0 && r.top() > 60.0 && r.bottom() < 790.0
            })
            .any(|(t, _)| t == label)
    }
    assert!(
        right_has(&ctx, &mut state, i18n::t!("主题")),
        "appearance tab must show the real theme row"
    );

    click_label(&ctx, &mut state, &rows, i18n::t!("信息"), 330.0);
    assert!(
        right_has(&ctx, &mut state, i18n::t!("当前帧")),
        "info tab must forward to info_tab"
    );

    click_label(&ctx, &mut state, &rows, i18n::t!("关于"), 330.0);
    let about = text_rects(&paint(&ctx, &mut state));
    let about_texts: Vec<&str> = about.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        about_texts.contains(&aevum_core::brand::version()),
        "about tab must report brand::version()"
    );

    click_label(&ctx, &mut state, &rows, i18n::t!("发布说明"), 330.0);
    let notes = text_rects(&paint(&ctx, &mut state));
    assert!(
        notes.iter().any(|(t, _)| t.starts_with("v1.")),
        "release-notes tab must render the newest CHANGELOG section"
    );

    click_label(&ctx, &mut state, &rows, i18n::t!("快捷键"), 330.0);
    assert!(
        right_has(&ctx, &mut state, i18n::t!("关键帧")),
        "shortcuts tab must forward to keys_tab"
    );

    // 返回入口走 CloseSettings，回到 dev 后页签列表必须消失。
    // Back sends CloseSettings; after returning the tab list must disappear.
    let before_back = text_rects(&paint(&ctx, &mut state));
    click_label(&ctx, &mut state, &before_back, i18n::t!("返回"), 330.0);
    let after = text_rects(&paint(&ctx, &mut state));
    let after_texts: Vec<&str> = after.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        !after_texts.contains(&i18n::t!("快捷键")),
        "back did not return to dev"
    );
}
