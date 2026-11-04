//! 右坞不许把文字挤出面板：每条文案都要完整落在面板的裁剪框里。
//!
//! # 这条判据是被什么逼出来的
//!
//! 2026-11-05 复核「合成」模块（英文界面）时看到：右侧面板的页签只剩
//! 「fo  Keyframes  AI  History  Library」，「HDR merge」显示成「rge」，
//! 取帧说明从中间截断，画布的照片还压在面板左缘 86px 上。根因不在文案，
//! 在**排布**：五个合成组的取帧行都是 `ui.horizontal`，而行内布局不许折行。
//! 中文勉强挤进 344px，英文
//! 「From the current frame, take 1 frames on each side (3 frames in all)」量出来
//! 418px —— 比面板宽 86px。egui 在内容溢出时会把面板 rect 整体往右推
//! （`set_rect_size` 保右边界），右坞一挪，后面声明的工具轨与中间画布跟着错位，
//! 画布底色就盖掉了面板左缘。
//!
//! 修法有两层：`shell::adjust::span_row` 按实测宽度决定行数，`shell::ui` 把
//! CentralPanel 钉在算出来的矩形里。这条判据钉的是**结果**：不管以后谁在面板里
//! 加什么、翻译成什么语言、用户把字号调到第几档，面板里的每条文本都得完整。
//!
//! # 口径
//!
//! 只认「裁剪框整个落在右坞里」的文本。顶栏那句机型串、胶片条压在图上的序号，
//! 都是刻意让整块面板裁的，它们的裁剪框是整窗宽，天然不在这条判据里。
//!
//! 为什么要把四种字号档位都扫一遍：`FONT_ZOOMS` 是 1.0 / 1.06 / 1.14 / 1.24，
//! 一档就可能把「刚好挤得下」的一行挤出去。只测默认档等于放过四分之三。
//!
//! # What forced this check
//!
//! The English Merge module (2026-11-05) showed the dock tabs reduced to
//! "fo  Keyframes  AI  History  Library", "HDR merge" clipped to "rge", the frame-span
//! caption cut mid-word, and the canvas photo lying over the panel's first 86 px. The
//! cause was layout, not wording: all five merge groups built their frame-span row with
//! `ui.horizontal`, which cannot wrap. Chinese barely fits 344 px; the English
//! "From the current frame, take 1 frames on each side (3 frames in all)" measures
//! 418 px -- 86 px wider than the panel. egui pushes the whole panel rect right when
//! content overflows (`set_rect_size` keeps the right edge); with the dock moved, the
//! tool rail and central canvas declared after it shift too, and the canvas fill paints
//! over the dock's left edge.
//!
//! Two layers fix it: `shell::adjust::span_row` picks the line count from measured
//! widths, and `shell::ui` pins CentralPanel to a computed rect. This check pins the
//! **result**: whatever gets added to the panel, whatever language it is translated to,
//! whichever font step the user picks, every text in the panel stays whole.
//!
//! # How it reads
//!
//! Only text whose clip rect lies entirely inside the dock band counts. The machine
//! string in the title bar and the frame numbers painted over filmstrip thumbnails are
//! meant to be clipped by a full-window rect, so they never enter this check.
//!
//! All four font steps are swept because `FONT_ZOOMS` runs 1.0 / 1.06 / 1.14 / 1.24:
//! one step is enough to push a row that "just fits" over the edge, and testing only the
//! default would clear three quarters of the space.

use aevum_ui::state::{AppState, Module};

/// 判据用的假窗口：与截图 harness 和重叠判据同尺寸，问题才复现得出来。
///
/// The fake window used here matches the screenshot harness and the overlap check, so
/// the defect actually reproduces.
const W: f32 = 1600.0;
const H: f32 = 1000.0;

#[test]
fn no_dock_text_is_cut_off_in_any_module() {
    let _lang_guard = crate::common::i18n_guard();
    let band = egui::Rect::from_min_max(
        egui::pos2(W - aevum_ui::theme::STRIP_W - aevum_ui::theme::DOCK_W, 0.0),
        egui::pos2(W - aevum_ui::theme::STRIP_W, H),
    );
    let mut hits: Vec<String> = Vec::new();
    for lang in [aevum_ui::i18n::Lang::ZH, aevum_ui::i18n::Lang::EN] {
        aevum_ui::i18n::set(lang);
        let ctx = egui::Context::default();
        aevum_ui::theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        for level in 0..aevum_ui::theme::FONT_ZOOMS.len() {
            state.font_level = level;
            for module in Module::ALL {
                state.module = module;
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(W, H),
                    )),
                    max_texture_side: Some(8192),
                    ..Default::default()
                };
                // 三帧热身：egui 的面板宽度按内容收敛，切模块后要几帧才稳。
                //
                // Three warm-up frames: egui converges panel widths from content, and a
                // module switch needs a few frames to settle.
                for _ in 0..3 {
                    let mut warm =
                        ctx.run_ui(input.clone(), |ui| aevum_ui::shell::ui(&mut state, ui));
                    warm.textures_delta.clear();
                }
                let mut out = ctx.run_ui(input, |ui| aevum_ui::shell::ui(&mut state, ui));
                for cs in &out.shapes {
                    if !band.contains_rect(cs.clip_rect) {
                        continue;
                    }
                    collect_overhang(&cs.shape, cs.clip_rect, &mut |text, over| {
                        if over > 1.0 {
                            hits.push(format!(
                                "{lang:?} · 字号档 {level} · {module:?} · 溢出 {over:.1}px 「{text}」"
                            ));
                        }
                    });
                }
                out.textures_delta.clear();
            }
        }
    }
    assert!(
        hits.is_empty(),
        "右坞里有 {} 条文字被面板边缘裁掉（用户看到的就是半截字）：\n{}\n\n\
         修法：别在面板里写 `ui.horizontal` 排长文案——行内布局不许折行，量出来比 \
         344px 宽就会把整个面板推歪。用 `shell::adjust::span_row` 这类「先量再排」的\
         写法，或把说明提到独立一行（垂直布局里的 `Label` 折行是可靠的）。",
        hits.len(),
        hits.join("\n")
    );
}

/// 把「文本 / 组内文本」的超宽量交给回调：正数 = 右端超出裁剪框多少像素。
///
/// Hand each text's overhang to the callback: positive means it sticks out past the
/// clip rect by that many pixels.
fn collect_overhang(shape: &egui::Shape, clip: egui::Rect, f: &mut impl FnMut(&str, f32)) {
    match shape {
        egui::Shape::Text(t) => {
            let rect = egui::Rect::from_min_size(t.pos, t.galley.size());
            f(t.galley.text(), rect.max.x - clip.max.x);
        }
        egui::Shape::Vec(items) => {
            for item in items {
                collect_overhang(item, clip, f);
            }
        }
        _ => {}
    }
}
