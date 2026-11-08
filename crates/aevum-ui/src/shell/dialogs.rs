//! 系统对话框的入口包装：只负责把"此刻的默认值"装进请求，弹窗本身在
//! `state::ops_dialog` 的后台线程里跑。
//!
//! 这些函数全部即时返回——它们只是往队列里压一个请求。别在这里等结果：
//! 2026-11-08 实测 UI 线程上的模态对话框会把主窗口整个藏起来。
//!
//! Entry wrappers for system dialogs: each only packs "the defaults right now" into a
//! request; the dialog itself runs on the worker thread in `state::ops_dialog`.
//! Everything here returns immediately -- it only queues a request. Never wait for a
//! result here: measured on 2026-11-08, a modal dialog on the UI thread hides the main
//! window outright.

use std::path::PathBuf;

use crate::state::AppState;
use crate::state::ops_dialog::{DialogFilter, DialogMode, DialogPurpose};
use aevum_core::audio::AudioCodec;
use egui::Context;

/// 序列根目录作为对话框起点：没有就交给系统默认位置。
///
/// The sequence root as the dialog's start folder; without one, the system default.
fn start_dir(state: &AppState) -> Option<PathBuf> {
    state
        .seq
        .as_ref()
        .map(|s| s.root.clone())
        .filter(|p| !p.as_os_str().is_empty())
}

pub(super) fn open_dialog(state: &mut AppState) {
    state.request_dialog(
        DialogPurpose::OpenSequence,
        DialogMode::Folder,
        None,
        None,
        None,
    );
}

/// 按照片选素材：单张、或者从一堆帧里挑一段，都不用先整理成文件夹。
pub(super) fn open_photos_dialog(state: &mut AppState) {
    state.request_dialog(
        DialogPurpose::OpenPhotos,
        DialogMode::Files,
        Some(DialogFilter {
            name: "照片",
            exts: aevum_core::sequence::image_extensions().to_vec(),
        }),
        start_dir(state),
        None,
    );
}

pub(super) fn rescan(state: &mut AppState, ctx: &Context) {
    if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
        state.open(ctx, root);
    }
}

pub(super) fn import_xmp_dialog(state: &mut AppState) {
    state.request_dialog(
        DialogPurpose::ImportXmp,
        DialogMode::File,
        Some(DialogFilter {
            name: "XMP 预设",
            exts: vec!["xmp"],
        }),
        start_dir(state),
        None,
    );
}

pub(super) fn export_xmp_dialog(state: &mut AppState) {
    let base = state
        .seq
        .as_ref()
        .map(|s| s.display_name())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "preset".to_owned());
    state.request_dialog(
        DialogPurpose::ExportXmp,
        DialogMode::Save {
            file_name: format!("{base}.xmp"),
        },
        Some(DialogFilter {
            name: "XMP 预设",
            exts: vec!["xmp"],
        }),
        start_dir(state),
        Some("xmp"),
    );
}

pub(super) fn choose_audio(state: &mut AppState) {
    state.request_dialog(
        DialogPurpose::ChooseAudio,
        DialogMode::File,
        Some(DialogFilter {
            name: "音频",
            exts: vec![
                "mp3", "wav", "aac", "m4a", "flac", "ogg", "opus", "wma", "aif", "aiff", "m4b",
            ],
        }),
        start_dir(state),
        None,
    );
}

/// 挑一张 .cube：与 MCP 的 cube.import 走同一条命令，校验只有一套。
pub(super) fn import_look_dialog(state: &mut AppState) {
    state.request_dialog(
        DialogPurpose::ImportLook,
        DialogMode::File,
        Some(DialogFilter {
            name: "3D LUT",
            exts: vec!["cube"],
        }),
        start_dir(state),
        None,
    );
}

/// 把当前调色烘成 .cube：覆盖由保存对话框确认，命令这边直接 confirm。
pub(super) fn export_look_dialog(state: &mut AppState) {
    let base = state
        .seq
        .as_ref()
        .map(|s| s.display_name())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "look".to_owned());
    state.request_dialog(
        DialogPurpose::ExportLook,
        DialogMode::Save {
            file_name: format!("{base}.cube"),
        },
        Some(DialogFilter {
            name: "3D LUT",
            exts: vec!["cube"],
        }),
        start_dir(state),
        Some("cube"),
    );
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
