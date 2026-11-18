//! 构图屏判据：井内真实画布 + 画幅参数组，无 toast、无假图。
//!
//! Frame-screen criteria: real canvas body plus the frame group, no toast and no
//! fake image.
//!
//! Frame criteria for T4b.

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

fn texts(shapes: &[ClippedShape]) -> Vec<String> {
    let mut out = Vec::new();
    for cs in shapes {
        walk_text(&cs.shape, &mut out);
    }
    out
}

fn walk_text(shape: &Shape, out: &mut Vec<String>) {
    match shape {
        Shape::Text(t) => out.push(t.galley.text().to_owned()),
        Shape::Vec(v) => {
            for s in v {
                walk_text(s, out);
            }
        }
        _ => {}
    }
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
fn screen_frame_has_real_canvas_and_no_coming_soon_toast() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let src = clean_v21();
    assert!(
        !src.contains("fn toast_next_build") && !src.contains("下一版接入"),
        "frame screen still carries the 「下一版接入」 toast"
    );

    let ctx = context();
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    state.module = Module::Frame;
    let photo = texts(&paint(&ctx, &mut state));
    for needle in ["构图", "画幅"] {
        assert!(
            photo.iter().any(|t| t == needle),
            "frame well is missing the real canvas surface: {needle}"
        );
    }
    assert!(
        !photo.iter().any(|t| t.contains("下一版接入")),
        "frame screen painted the coming-soon toast"
    );

    // 0 帧：只允许一句真实引导；不许出现画幅参数或裁剪读数。
    // Zero frames: one honest guide only; no frame group or crop readout.
    let mut empty = AppState::new();
    empty.module = Module::Frame;
    let zero = texts(&paint(&ctx, &mut empty));
    assert!(
        zero.iter()
            .any(|t| t == i18n::t!("导入照片后调整画幅与取景")),
        "zero-frame frame well is missing the import guide"
    );
    assert!(
        !zero.iter().any(|t| t == i18n::t!("画幅")),
        "zero-frame frame still paints the frame group"
    );
}
