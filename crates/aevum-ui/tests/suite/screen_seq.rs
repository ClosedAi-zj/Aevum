//! 序列屏判据：井内必须是真帧表，且不再提示「下一版接入」。
//!
//! Sequence-screen criteria: the well must paint the real frame table, and the
//! "next build" toast must be gone. Both directions are pinned -- a 48-frame demo
//! sequence must show the table, a zero-frame state must show only the honest guide.
//!
//! Sequence criteria for T3a.

use aevum_ui::i18n::{self, Lang};
use aevum_ui::state::{AppState, Module};
use aevum_ui::{shell, theme};

use egui::epaint::ClippedShape;
use egui::{Rect, Shape};

/// 与 `--selftest` 同一块画布：1600×1000。
/// Same 1600x1000 canvas as the selftest.
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

/// 三帧预热后取稳态帧，与其余 suite 同口径。
/// Paint a settled frame after three warm-ups, matching the rest of the suite.
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

/// 读源码并剥整行注释，与 `empty_state.rs` 同一把尺。
/// Read source with whole-line comments stripped, the same ruler as `empty_state`.
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

/// 截出一个函数体（到下一个顶层 `fn` 为止）。
/// Slice one function body up to the next top-level `fn`.
#[test]
fn screen_seq_has_real_table_and_no_coming_soon_toast() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    // 静态：Seq 臂已清空，不再给「下一版接入」。把它改回去这条必红。
    // Static: the Seq arm is empty; restoring the toast turns this red.
    let src = clean_v21();
    assert!(
        !src.contains("fn toast_next_build") && !src.contains("下一版接入"),
        "sequence screen still carries the 「下一版接入」 toast"
    );

    let ctx = context();
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    state.module = Module::Sequence;
    let photo = texts(&paint(&ctx, &mut state));
    for needle in ["序列帧表", "逐帧数据", "总帧数"] {
        assert!(
            photo.iter().any(|t| t == needle),
            "sequence well is missing the real table surface: {needle}"
        );
    }
    assert!(
        !photo.iter().any(|t| t.contains("下一版接入")),
        "sequence screen painted the coming-soon toast"
    );

    // 0 帧：只允许一句真实引导；预览卡那套「0001 / 0」与空表骨架都不许出现。
    // Zero frames: one honest guide only; no "0001 / 0" preview counter or empty table.
    let mut empty = AppState::new();
    empty.module = Module::Sequence;
    let zero = texts(&paint(&ctx, &mut empty));
    assert!(
        zero.iter()
            .any(|t| t == i18n::t!("导入照片后在这里逐帧查看")),
        "zero-frame sequence well is missing the import guide"
    );
    assert!(
        !zero.iter().any(|t| t.contains("0001 / 0")),
        "zero-frame preview card painted a fake frame counter"
    );
    assert!(
        !zero.iter().any(|t| t == i18n::t!("逐帧数据")),
        "zero-frame sequence still paints the empty table skeleton"
    );
}
