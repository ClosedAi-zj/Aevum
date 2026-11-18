//! 音频屏判据：真实音轨信息 + 参数组，无假波形、无 toast。
//!
//! Audio-screen criteria: real track metadata plus the audio group, no fake
//! waveform and no toast.
//!
//! Audio criteria for T4b.

use aevum_core::audio::AudioProbe;
use aevum_ui::i18n::{self, Lang};
use aevum_ui::state::{AppState, Module};
use aevum_ui::{shell, theme};

use egui::epaint::ClippedShape;
use egui::{Rect, Shape};
use std::path::PathBuf;

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

/// 井内长折线数量：无真实采样就不该有任何波形折线。
/// Long polylines inside the well: without samples there must be no waveform.
fn long_paths(shapes: &[ClippedShape]) -> usize {
    let well = Rect::from_min_max(egui::Pos2::new(80.0, 60.0), egui::Pos2::new(960.0, 790.0));
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
fn screen_audio_has_real_track_and_no_fake_waveform() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let src = clean_v21();
    assert!(
        !src.contains("fn toast_next_build") && !src.contains("下一版接入"),
        "audio screen still carries the 「下一版接入」 toast"
    );

    let ctx = context();
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    state.module = Module::Audio;
    state.render_audio = Some(PathBuf::from("score.wav"));
    state.audio_info = Some(AudioProbe {
        codec: "aac".to_owned(),
        duration: 12.5,
    });
    let shapes = paint(&ctx, &mut state);
    let photo = texts(&shapes);
    for needle in ["音频", "音轨", "编码", "时长", "aac", "00:12.50"] {
        assert!(
            photo.iter().any(|t| t == needle),
            "audio well is missing the real track surface: {needle}"
        );
    }
    assert!(
        !photo.iter().any(|t| t.contains("下一版接入")),
        "audio screen painted the coming-soon toast"
    );
    assert_eq!(
        long_paths(&shapes),
        0,
        "audio screen painted a fake waveform without samples"
    );

    // 有帧但没有配乐：诚实写「未选择配乐」，同样不画波形。
    // Frames but no music: say "未选择配乐" and still paint no waveform.
    let mut no_music = AppState::demo();
    no_music.install_demo_textures(&ctx);
    no_music.module = Module::Audio;
    no_music.render_audio = None;
    no_music.audio_info = None;
    let no_music_shapes = paint(&ctx, &mut no_music);
    let no_music_text = texts(&no_music_shapes);
    assert!(
        no_music_text.iter().any(|t| t == i18n::t!("未选择配乐")),
        "audio well is missing the honest 「未选择配乐」 state"
    );
    assert_eq!(
        long_paths(&no_music_shapes),
        0,
        "audio screen painted a waveform with no music selected"
    );

    // 0 帧：只允许一句真实引导；不许出现音轨卡。
    // Zero frames: one honest guide only; no track card.
    let mut empty = AppState::new();
    empty.module = Module::Audio;
    let zero_shapes = paint(&ctx, &mut empty);
    let zero = texts(&zero_shapes);
    assert!(
        zero.iter()
            .any(|t| t == i18n::t!("导入照片后在这里配乐与调音")),
        "zero-frame audio well is missing the import guide"
    );
    assert!(
        !zero.iter().any(|t| t == i18n::t!("音轨")),
        "zero-frame audio still paints the track card"
    );
    assert_eq!(long_paths(&zero_shapes), 0);
}
