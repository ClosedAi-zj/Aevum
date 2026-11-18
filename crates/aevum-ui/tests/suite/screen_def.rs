//! 去闪屏判据：井内必须是真实亮度曲线 + 参数组，且不再提示「下一版接入」。
//!
//! Deflicker-screen criteria: the well must paint the real luminance curve and the
//! deflicker group, the coming-soon toast must be gone, and zero frames must show
//! only the honest guide.
//!
//! Deflicker criteria for T3b.

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

/// 井内长折线数量：只看完全落在画布井里的 Path，排除轨道图标/圆角等装饰。
/// Long polylines inside the well only; rail icons and rounded chrome are excluded.
fn long_paths(shapes: &[ClippedShape]) -> usize {
    let well = Rect::from_min_max(egui::Pos2::new(80.0, 60.0), egui::Pos2::new(1290.0, 790.0));
    let mut n = 0;
    for cs in shapes {
        walk_paths(&cs.shape, well, &mut n);
    }
    n
}

fn walk_paths(shape: &Shape, well: Rect, n: &mut usize) {
    match shape {
        Shape::Path(p) if p.points.len() > 5 && p.points.iter().all(|pt| well.contains(*pt)) => {
            *n += 1;
        }
        Shape::Vec(v) => {
            for s in v {
                walk_paths(s, well, n);
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
fn screen_def_has_real_curve_and_no_coming_soon_toast() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let src = clean_v21();
    assert!(
        !src.contains("fn toast_next_build") && !src.contains("下一版接入"),
        "deflicker screen still carries the 「下一版接入」 toast"
    );

    let ctx = context();
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    state.module = Module::Deflicker;
    let shapes = paint(&ctx, &mut state);
    let photo = texts(&shapes);
    for needle in ["去闪烁", "逐帧亮度", "原始", "去闪后", "闪烁"] {
        assert!(
            photo.iter().any(|t| t == needle),
            "deflicker well is missing the real curve surface: {needle}"
        );
    }
    assert!(
        long_paths(&shapes) >= 2,
        "deflicker curve polylines are missing (grey raw + violet corrected)"
    );
    assert!(
        !photo.iter().any(|t| t.contains("下一版接入")),
        "deflicker screen painted the coming-soon toast"
    );

    // 0 帧：只允许一句真实引导；不许出现曲线标题、图例或任何折线。
    // Zero frames: one honest guide only; no curve title, legend or polylines.
    let mut empty = AppState::new();
    empty.module = Module::Deflicker;
    let zero_shapes = paint(&ctx, &mut empty);
    let zero = texts(&zero_shapes);
    assert!(
        zero.iter().any(|t| t == i18n::t!("导入照片后分析逐帧亮度")),
        "zero-frame deflicker well is missing the import guide"
    );
    assert!(
        !zero.iter().any(|t| t == i18n::t!("逐帧亮度")),
        "zero-frame deflicker still paints the curve surface"
    );
    assert_eq!(
        long_paths(&zero_shapes),
        0,
        "zero-frame deflicker painted a fake curve polyline"
    );
}
