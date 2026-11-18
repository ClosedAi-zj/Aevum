//! 合成屏判据：左列真实类型卡 + 右列参数组，无步骤号、无假结果、无 toast。
//!
//! Merge-screen criteria: real type cards on the left, parameter groups on the
//! right, no step numbers, no fake result and no coming-soon toast.
//!
//! Merge criteria for T4a.

use aevum_ui::i18n::{self, Lang};
use aevum_ui::state::{AppState, Module};
use aevum_ui::{shell, theme};

use egui::epaint::ClippedShape;
use egui::{Rect, Shape};

fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
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

fn click(ctx: &egui::Context, state: &mut AppState, pos: egui::Pos2) {
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
fn screen_merge_has_real_type_cards_and_no_coming_soon_toast() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let src = clean_v21();
    assert!(
        !src.contains("fn toast_next_build") && !src.contains("下一版接入"),
        "merge screen still carries the 「下一版接入」 toast"
    );

    let ctx = context();
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    state.module = Module::Merge;
    let shapes = paint(&ctx, &mut state);
    let rows = text_rects(&shapes);
    let texts: Vec<&str> = rows.iter().map(|(t, _)| t.as_str()).collect();
    for needle in [
        "合成",
        "合成类型",
        "HDR 合并",
        "全景拼接",
        "星空堆栈",
        "时间合成",
        "焦点合成",
    ] {
        assert!(
            texts.contains(&needle),
            "merge well is missing the real type card: {needle}"
        );
    }
    for step in ["①", "②", "③"] {
        assert!(
            !texts.iter().any(|t| t.contains(step)),
            "merge screen painted a forbidden step number: {step}"
        );
    }
    assert!(
        !texts.iter().any(|t| t.contains("下一版接入")),
        "merge screen painted the coming-soon toast"
    );

    // 类型卡是真实行为：点 HDR 行后，其余四组折起、HDR 组展开（真实 collapsed 状态）。
    // The cards are real controls: clicking HDR collapses the other four groups.
    let hdr = rows
        .iter()
        .filter(|(t, _)| t == "HDR 合并")
        .min_by(|a, b| a.1.left().total_cmp(&b.1.left()))
        .map(|(_, r)| r.center())
        .expect("merge type card 里找不到 HDR 行");
    click(&ctx, &mut state, hdr);
    assert!(!state.collapsed.contains("hdr"), "HDR 组应展开");
    for other in ["pano", "stack", "timeblend", "focus"] {
        assert!(state.collapsed.contains(other), "{other} 组应折起");
    }

    // 0 帧：只允许一句真实引导；不许出现类型卡或任何假合成结果。
    // Zero frames: one honest guide only; no type cards or fake merge result.
    let mut empty = AppState::new();
    empty.module = Module::Merge;
    let zero = text_rects(&paint(&ctx, &mut empty));
    let zero_texts: Vec<&str> = zero.iter().map(|(t, _)| t.as_str()).collect();
    assert!(
        zero_texts
            .iter()
            .any(|t| *t == i18n::t!("还没有序列 · 文件 → 打开照片…")),
        "zero-frame merge well is missing the import guide"
    );
    for needle in ["合成类型", "HDR 合并", "全景拼接", "焦点合成"] {
        assert!(
            !zero_texts.contains(&needle),
            "zero-frame merge still paints the type card: {needle}"
        );
    }
}
