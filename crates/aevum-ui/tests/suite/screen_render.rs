//! 导出屏判据：井内必须是真实三栏导出工作台，且不再提示「下一版接入」。
//!
//! Render-screen criteria: the well must paint the real three-column export
//! workspace, the coming-soon toast must be gone, and zero frames must show only
//! the honest guide.
//!
//! Render criteria for T4a.

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

fn body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(name)
        .unwrap_or_else(|| panic!("v21_demo.rs 找不到 {name}"));
    let rest = &src[start..];
    let end = rest.find("\nfn ").map(|i| start + i).unwrap_or(src.len());
    &src[start..end]
}

#[test]
fn screen_render_has_real_workspace_and_no_coming_soon_toast() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let src = clean_v21();
    let toast = body(&src, "fn toast_next_build");
    assert!(
        toast.contains("Screen::Render => {}"),
        "render screen still queues the 「下一版接入」 toast"
    );

    let ctx = context();
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    state.module = Module::Render;
    let photo = texts(&paint(&ctx, &mut state));
    for needle in ["导出", "队列", "预览", "导出设置", "立即导出"] {
        assert!(
            photo.iter().any(|t| t == needle),
            "render well is missing the real export workspace: {needle}"
        );
    }
    assert!(
        !photo.iter().any(|t| t.contains("下一版接入")),
        "render screen painted the coming-soon toast"
    );

    // 0 帧：只允许一句真实引导；不许出现队列/导出设置/立即导出。
    // Zero frames: one honest guide only; no queue/settings/export controls.
    let mut empty = AppState::new();
    empty.module = Module::Render;
    let zero = texts(&paint(&ctx, &mut empty));
    assert!(
        zero.iter()
            .any(|t| t == i18n::t!("还没有序列 · 文件 → 打开照片…")),
        "zero-frame render well is missing the import guide"
    );
    for needle in ["队列", "导出设置", "立即导出"] {
        assert!(
            !zero.iter().any(|t| t == needle),
            "zero-frame render still paints the export workspace: {needle}"
        );
    }
}
