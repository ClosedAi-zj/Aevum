//! 系统对话框：打开照片/序列、XMP 导入导出、.cube 导入导出、音轨选择。
//!
//! System dialogs: open photo / sequence, XMP import and export, .cube import and
//! export, audio track picking.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use crate::state::AppState;
use aevum_core::audio::AudioCodec;
use egui::Context;
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

pub(super) fn open_dialog(state: &mut AppState, ctx: &Context) {
    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
        state.open(ctx, dir);
    }
}

/// 按照片选素材：单张、或者从一堆帧里挑一段，都不用先整理成文件夹。
pub(super) fn open_photos_dialog(state: &mut AppState, ctx: &Context) {
    let mut dialog =
        rfd::FileDialog::new().add_filter("照片", aevum_core::sequence::image_extensions());
    if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
        dialog = dialog.set_directory(root);
    }
    if let Some(paths) = dialog.pick_files() {
        state.open_paths(ctx, &paths);
    }
}

pub(super) fn rescan(state: &mut AppState, ctx: &Context) {
    if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
        state.open(ctx, root);
    }
}

pub(super) fn import_xmp_dialog(state: &mut AppState) {
    let mut dialog = rfd::FileDialog::new().add_filter("XMP 预设", &["xmp"]);
    if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
        dialog = dialog.set_directory(root);
    }
    let Some(path) = dialog.pick_file() else {
        return;
    };
    if let Err(e) = state.import_xmp_preset(&path) {
        state.error = Some(crate::i18n::tr_fmt("导入 XMP 失败：{}", &[&e]));
    }
}

pub(super) fn export_xmp_dialog(state: &mut AppState) {
    let base = state
        .seq
        .as_ref()
        .map(|s| s.display_name())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "preset".to_owned());
    let mut dialog = rfd::FileDialog::new()
        .add_filter("XMP 预设", &["xmp"])
        .set_file_name(format!("{base}.xmp"));
    if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
        dialog = dialog.set_directory(root);
    }
    let Some(path) = dialog.save_file() else {
        return;
    };
    let path = if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
    {
        path
    } else {
        path.with_extension("xmp")
    };
    // 保存对话框自己会问是否覆盖，这里视为用户已确认。
    if let Err(e) = state.export_xmp_preset(&path, true) {
        state.error = Some(crate::i18n::tr_fmt("导出 XMP 失败：{}", &[&e]));
    }
}

pub(super) fn choose_audio(state: &mut AppState) {
    let mut dialog = rfd::FileDialog::new().add_filter(
        "音频",
        &[
            "mp3", "wav", "aac", "m4a", "flac", "ogg", "opus", "wma", "aif", "aiff", "m4b",
        ],
    );
    if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
        dialog = dialog.set_directory(root);
    }
    if let Some(path) = dialog.pick_file() {
        state.set_render_audio(Some(path));
    }
}

pub(super) fn codec_label(codec: AudioCodec) -> &'static str {
    // 只有 Auto 与 Copy 带中文，其余是编解码器名（语言中立）。
    match codec {
        AudioCodec::Auto => crate::i18n::t!("自动（兼容优先）"),
        AudioCodec::Copy => crate::i18n::t!("不转码（直接复制）"),
        AudioCodec::Aac128 => "AAC 128k",
        AudioCodec::Aac192 => "AAC 192k",
        AudioCodec::Aac256 => "AAC 256k",
        AudioCodec::Aac320 => "AAC 320k",
    }
}

/// 挑一张 .cube：与 MCP 的 cube.import 走同一条命令，校验只有一套。
pub(super) fn import_look_dialog(state: &mut AppState, ctx: &Context) {
    let mut dialog = rfd::FileDialog::new().add_filter("3D LUT", &["cube"]);
    if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
        dialog = dialog.set_directory(root);
    }
    let Some(path) = dialog.pick_file() else {
        return;
    };
    let args = serde_json::json!({ "path": path.display().to_string() });
    if let Err(e) = state.run_ui_command(ctx, "cube.import", &args) {
        state.error = Some(crate::i18n::tr_fmt("导入外观失败：{}", &[&e]));
    }
}

/// 把当前调色烘成 .cube：覆盖由保存对话框确认，命令这边直接 confirm。
pub(super) fn export_look_dialog(state: &mut AppState, ctx: &Context) {
    let base = state
        .seq
        .as_ref()
        .map(|s| s.display_name())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "look".to_owned());
    let mut dialog = rfd::FileDialog::new()
        .add_filter("3D LUT", &["cube"])
        .set_file_name(format!("{base}.cube"));
    if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
        dialog = dialog.set_directory(root);
    }
    let Some(path) = dialog.save_file() else {
        return;
    };
    let path = if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cube"))
    {
        path
    } else {
        path.with_extension("cube")
    };
    let args = serde_json::json!({
        "path": path.display().to_string(),
        "confirm": true,
    });
    if let Err(e) = state.run_ui_command(ctx, "cube.export", &args) {
        state.error = Some(crate::i18n::tr_fmt("导出外观失败：{}", &[&e]));
    }
}
