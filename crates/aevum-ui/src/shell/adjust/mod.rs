//! 调整组全家：显影/颜色/细节/成片队列等右侧 dock 的调整面板。
//!
//! All adjustment groups: the develop / colour / detail / render queue panels of the
//! right dock.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::BAR_H;
use super::common::HslKind;
use super::common::acr_tabs;
use super::common::band_row;
use super::common::chip_row;
use super::common::combo_style;
use super::common::group_card;
use super::common::group_header;
use super::common::hsl_band_stops;
use super::common::in_field;
use super::common::kv;
use super::common::kv_dense;
use super::common::note;
use super::common::slider;
use super::common::slider_row;
use super::common::spec_slider;
use super::dialogs::choose_audio;
use super::dialogs::codec_label;
use super::dialogs::export_look_dialog;
use super::dialogs::export_xmp_dialog;
use super::dialogs::import_look_dialog;
use super::dialogs::import_xmp_dialog;
use super::mask::adjust_masks;
use super::toolbar::bar_button;
use super::toolbar::bar_chip;
use super::toolbar::bar_primary;
use crate::controls;
use crate::controls::Field;
use crate::history;
use crate::state::{AppState, Aspect, MergeKind, Module, MotionBox, Tool};
use crate::theme::{self, *};
use crate::widgets;
use aevum_core::align::Estimator;
use aevum_core::audio::AudioCodec;
use aevum_core::colorspace::ColorSpace;
use aevum_core::crop;
use aevum_core::delivery::DeliveryTable;
use aevum_core::export::{OutputDynamic, OutputMode, OutputSharpen, SharpenStrength, StillFormat};
use aevum_core::grade::GradeParams;
use aevum_core::qa::{self, Finding};
use aevum_core::queue::QueueStatus;
use aevum_core::retime::{self, Retime, SpeedKey};
use aevum_core::watermark::{Spot, WatermarkKind};
use egui::{
    Align, Align2, Color32, Context, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
    Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

fn flat_speed(retime: &Retime) -> Option<f64> {
    let want = retime.keys.first().map(|k| k.speed).unwrap_or(1.0);
    retime
        .keys
        .iter()
        .all(|k| (k.speed - want).abs() <= 1e-6)
        .then_some(want)
}

pub(crate) fn reset_group(state: &mut AppState, key: &str) {
    // 推子参数组与表外字段：口径在 controls::neutralise_group——复位按钮和
    // 「眼睛」旁路必须是同一个动作，两边各写一份迟早对不上。
    //
    // Slider groups and out-of-table fields: the convention lives in
    // controls::neutralise_group -- the reset button and the "eye" bypass must be the
    // same action, and two handwritten copies eventually disagree.
    controls::neutralise_group(&mut state.params, key);
    // 参数之外的状态按组手工复位。
    //
    // Non-parameter state resets by group, by hand.
    match key {
        // 曲线区的拖拽态跟着清：参数回默认了，手上还抓着一条旧曲线就怪了。
        //
        // Curve drag state clears with the parameters: with everything back at defaults,
        // still holding an old curve would be bizarre.
        "curve" => {
            state.curve_grab = None;
            state.levels_grab = None;
        }
        "deflicker" => {
            state.def_window = 15.0;
            state.def_strength = 100.0;
            state.def_multi = false;
            state.def_channels = false;
            state.def_align_exif = false;
            state.recompute_deflicker();
        }
        "frame" => {
            state.set_aspect(Aspect::Native);
            state.crop_reset();
            state.grid = false;
            state.cross = false;
            state.split = false;
        }
        "render-out" | "render-time" | "render-codec" => {
            state.output_w = 3840;
            state.output_h = 2160;
            state.output_fps = 24.0;
            state.codec = "HEVC".to_owned();
        }
        "render-retime" => state.retime = Retime::default(),
        "render-motion" => state.motion_reset(),
        "audio" => state.set_render_audio(None),
        "audio-time" => {
            let opts = &mut state.render_audio_opts;
            opts.delay = 0.0;
            opts.gain = 100.0;
            opts.fade_in = 0.0;
            opts.fade_out = 0.0;
        }
        "audio-mix" => {
            let opts = &mut state.render_audio_opts;
            opts.looped = false;
            opts.trim = true;
            opts.acodec = AudioCodec::Auto;
        }
        _ => {}
    }
    if matches!(
        key,
        "basic" | "curve" | "mixer" | "grade" | "detail" | "optics" | "effects" | "look"
    ) {
        state.preset = "自定义".to_owned();
    }
    state.preview_dirty = true;
}
// ---------------------------------------------------------------- 调整页

pub(super) fn adjust_tab(state: &mut AppState, ui: &mut Ui) {
    match state.module {
        Module::Sequence => adjust_sequence(state, ui),
        Module::Deflicker => adjust_deflicker(state, ui),
        Module::Develop => adjust_develop(state, ui),
        Module::Merge => adjust_merge(state, ui),
        Module::Frame => adjust_frame(state, ui),
        Module::Render => adjust_render(state, ui),
        Module::Audio => adjust_audio(state, ui),
    }
}

// 各页拆成独立文件：切点落在函数与它自己的 doc 注释之间，一块是连续原文。
// 外部只认 `reset_group` / `adjust_tab` / `copies_group` 三个名字。
//
// Each page lives in its own file, cut at a function boundary together with its own doc
// comment, so every block is contiguous original text. Only `reset_group`, `adjust_tab`
// and `copies_group` leave this module.
mod audio;
mod develop;
mod geometry;
mod merge;
mod render;
mod sequence;

use self::audio::adjust_audio;
use self::develop::adjust_develop;
use self::geometry::adjust_frame;
use self::merge::adjust_merge;
use self::render::adjust_render;
pub(crate) use self::sequence::copies_group;
use self::sequence::{adjust_deflicker, adjust_sequence};
