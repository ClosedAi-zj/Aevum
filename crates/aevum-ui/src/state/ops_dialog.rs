//! 系统对话框的非阻塞中枢：弹窗在后台线程跑，结果每帧回 UI 线程分发。
//!
//! 2026-11-08 实测（`GetWindowRect` + `IsWindowVisible` 逐 100ms 采样）：
//! 模态对话框跑在 UI 线程上时，**主窗口在对话框打开期间被整体隐藏**
//! （WS_VISIBLE 被清，非最小化），关掉对话框才回来；把同一个对话框挪到
//! 后台线程，主窗口全程可见（各 10 次采样，0 次 vs 10 次）。所以全部
//! 对话框都必须走这里，不许在 `App::ui` 的调用栈里直接弹。
//!
//! The non-blocking hub for system dialogs: the dialog runs on a worker thread and
//! its result is dispatched back on the UI thread each frame. Measured on 2026-11-08
//! (100 ms sampling of `GetWindowRect` + `IsWindowVisible`): with the modal dialog on
//! the UI thread the main window is hidden outright while the dialog is open
//! (WS_VISIBLE cleared, not minimised), and returns only when it closes; moving the
//! very same dialog to a worker thread keeps the window visible throughout
//! (0/10 vs 10/10 samples). Every dialog must go through here.

use std::path::PathBuf;

use egui::Context;

use super::AppState;

/// 弹完之后要做什么。配置在入队时从 state 取好，工作线程不再碰任何共享状态。
///
/// What to do with the result. The config is captured from state when the request is
/// queued; the worker thread never touches shared state.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DialogPurpose {
    OpenSequence,
    OpenPhotos,
    ImportXmp,
    ExportXmp,
    ChooseAudio,
    ImportLook,
    ExportLook,
    PickDcp,
    PickLcp,
    RenderOutDir,
    WatermarkImage,
    RenderVideoOut,
    ExportFormDir,
    LibraryAdd,
    /// 单帧「另存为」：容器在入队时定死，后缀以它为准。
    ///
    /// Single-frame "save as": the container is fixed at enqueue time and wins over
    /// whatever extension the user typed.
    SaveFrameAs,
}

/// 过滤器：名字 + 扩展名表，从各调用点的常量搬进来。
///
/// A filter: display name plus extension list, moved in from the call site's constants.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DialogFilter {
    pub name: &'static str,
    pub exts: Vec<&'static str>,
}

/// 对话框形态与预填值。
///
/// The dialog shape and pre-filled values.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DialogMode {
    Folder,
    File,
    Files,
    Save { file_name: String },
}

/// 一个待弹的对话框。
///
/// One dialog waiting to be shown.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DialogRequest {
    pub purpose: DialogPurpose,
    pub mode: DialogMode,
    pub filter: Option<DialogFilter>,
    pub start: Option<PathBuf>,
    /// 保存类对话框的后缀兜底（「另存为」手输 .png 也不能把 JPEG 说成 PNG）。
    ///
    /// Extension fix-up for save dialogs (a hand-typed `.png` must not turn a JPEG
    /// into a PNG).
    pub fix_ext: Option<&'static str>,
}

/// 工作线程带回的结果：`None` = 用户取消。
///
/// The reply from the worker thread: `None` = cancelled.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DialogPick {
    Folder(PathBuf),
    File(PathBuf),
    Files(Vec<PathBuf>),
}

type DialogReply = (DialogPurpose, Option<DialogPick>);

impl AppState {
    /// 入队一个对话框；每次只弹一个，其余排队（同时弹两扇系统对话框没有意义）。
    ///
    /// Queue a dialog; only one shows at a time, the rest wait (two system dialogs at
    /// once is meaningless).
    pub(crate) fn request_dialog(
        &mut self,
        purpose: DialogPurpose,
        mode: DialogMode,
        filter: Option<DialogFilter>,
        start: Option<PathBuf>,
        fix_ext: Option<&'static str>,
    ) {
        // 无显示环境（--shot / 自检）绝不弹系统对话框。
        //
        // A display-less environment (--shot / selftest) must never open a system dialog.
        if self.headless {
            return;
        }
        self.dialog_queue.push_back(DialogRequest {
            purpose,
            mode,
            filter,
            start,
            fix_ext,
        });
        // 起飞交给 poll() 开头的 `spawn_pending_dialog`：晚一帧（约 16ms）无感，
        // 却让「入队」与「起线程」各自可测——测试可以只压队列不起真对话框。
        //
        // Take-off is left to `spawn_pending_dialog` at the top of poll(): one frame
        // (~16 ms) late and imperceptible, while keeping enqueue and thread-spawn
        // separately testable -- a test can fill the queue without a real dialog.
    }

    /// 队列非空且没有在弹的，就起一个工作线程。
    ///
    /// Spawn a worker when the queue is non-empty and nothing is in flight.
    pub(crate) fn spawn_pending_dialog(&mut self, ctx: &Context) {
        if self.dialog_in_flight {
            return;
        }
        let Some(req) = self.dialog_queue.pop_front() else {
            return;
        };
        self.dialog_in_flight = true;
        let tx = self.dialog_tx.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("system-dialog".to_owned())
            .spawn(move || {
                let mut dialog = crate::win_parent::file_dialog();
                if let Some(f) = &req.filter {
                    dialog = dialog.add_filter(f.name, &f.exts);
                }
                if let Some(start) = &req.start {
                    dialog = dialog.set_directory(start);
                }
                let picked = match &req.mode {
                    DialogMode::Folder => dialog.pick_folder().map(DialogPick::Folder),
                    DialogMode::File => dialog.pick_file().map(DialogPick::File),
                    DialogMode::Files => dialog.pick_files().map(DialogPick::Files),
                    DialogMode::Save { file_name } => dialog
                        .set_file_name(file_name.clone())
                        .save_file()
                        .map(DialogPick::File),
                };
                let _ = tx.send((req.purpose, picked));
                // 关掉对话框之后 UI 线程可能正睡在「无交互不重绘」上，推它一帧。
                //
                // After the dialog closes the UI thread may be sleeping on "no input, no
                // repaint"; nudge one frame.
                ctx.request_repaint();
            })
            .expect("起系统对话框线程");
    }

    /// 每帧收结果并分发。在 `poll()` 里调用，早于 ui()。
    ///
    /// Collect and dispatch results each frame; called from `poll()`, before ui().
    pub(crate) fn drain_dialog_replies(&mut self, ctx: &Context) {
        let mut replies: Vec<DialogReply> = Vec::new();
        while let Ok(reply) = self.dialog_rx.try_recv() {
            replies.push(reply);
        }
        if replies.is_empty() {
            return;
        }
        self.dialog_in_flight = false;
        for (purpose, pick) in replies {
            self.apply_dialog(ctx, purpose, pick);
        }
        self.spawn_pending_dialog(ctx);
    }

    /// 结果分发：每个分支都是原来调用点里「拿到路径之后」的那几行，原样搬来。
    ///
    /// Dispatch: each branch is the "after we have a path" block of the old call site,
    /// moved verbatim.
    pub(crate) fn apply_dialog(
        &mut self,
        ctx: &Context,
        purpose: DialogPurpose,
        pick: Option<DialogPick>,
    ) {
        let Some(pick) = pick else {
            return; // 取消：什么都不做、不产生文件。
        };
        match purpose {
            DialogPurpose::OpenSequence => {
                let DialogPick::Folder(dir) = pick else {
                    return;
                };
                self.open(ctx, dir);
            }
            DialogPurpose::OpenPhotos => {
                let DialogPick::Files(paths) = pick else {
                    return;
                };
                self.open_paths(ctx, &paths);
            }
            DialogPurpose::ImportXmp => {
                let DialogPick::File(path) = pick else { return };
                if let Err(e) = self.import_xmp_preset(&path) {
                    self.error = Some(crate::i18n::tr_fmt("导入 XMP 失败：{}", &[&e]));
                }
            }
            DialogPurpose::ExportXmp => {
                let DialogPick::File(path) = pick else { return };
                let path = match path.extension().and_then(|e| e.to_str()) {
                    Some(e) if e.eq_ignore_ascii_case("xmp") => path,
                    _ => path.with_extension("xmp"),
                };
                // 保存对话框自己会问是否覆盖，这里视为用户已确认。
                if let Err(e) = self.export_xmp_preset(&path, true) {
                    self.error = Some(crate::i18n::tr_fmt("导出 XMP 失败：{}", &[&e]));
                }
            }
            DialogPurpose::ChooseAudio => {
                let DialogPick::File(path) = pick else { return };
                self.set_render_audio(Some(path));
            }
            DialogPurpose::ImportLook => {
                let DialogPick::File(path) = pick else { return };
                let args = serde_json::json!({ "path": path.display().to_string() });
                if let Err(e) = self.run_ui_command(ctx, "cube.import", &args) {
                    self.error = Some(crate::i18n::tr_fmt("导入外观失败：{}", &[&e]));
                }
            }
            DialogPurpose::ExportLook => {
                let DialogPick::File(path) = pick else { return };
                let path = match path.extension().and_then(|e| e.to_str()) {
                    Some(e) if e.eq_ignore_ascii_case("cube") => path,
                    _ => path.with_extension("cube"),
                };
                let args = serde_json::json!({
                    "path": path.display().to_string(),
                    "confirm": true,
                });
                if let Err(e) = self.run_ui_command(ctx, "cube.export", &args) {
                    self.error = Some(crate::i18n::tr_fmt("导出外观失败：{}", &[&e]));
                }
            }
            DialogPurpose::PickDcp => {
                let DialogPick::File(path) = pick else { return };
                let args = serde_json::json!({"kind":"dcp","path": path.display().to_string()});
                if let Err(e) = self.run_ui_command(ctx, "profile.set", &args) {
                    self.error = Some(crate::i18n::tr_fmt("套用相机配置失败：{}", &[&e]));
                }
            }
            DialogPurpose::PickLcp => {
                let DialogPick::File(path) = pick else { return };
                let args = serde_json::json!({"kind":"lcp","path": path.display().to_string()});
                if let Err(e) = self.run_ui_command(ctx, "profile.set", &args) {
                    self.error = Some(crate::i18n::tr_fmt("套用镜头配置失败：{}", &[&e]));
                }
            }
            DialogPurpose::RenderOutDir => {
                let DialogPick::Folder(dir) = pick else {
                    return;
                };
                self.render_out_dir = dir;
            }
            DialogPurpose::WatermarkImage => {
                let DialogPick::File(path) = pick else { return };
                self.render_watermark.image = path;
                crate::history::note("render.watermark.image", "水印图片");
            }
            DialogPurpose::RenderVideoOut => {
                let DialogPick::File(path) = pick else { return };
                self.render_video_out = path;
            }
            DialogPurpose::ExportFormDir => {
                let DialogPick::Folder(dir) = pick else {
                    return;
                };
                // 表单还没建（理论上不可能：目录按钮就在表单里）就用当前参数兜底。
                //
                // If the form temp is missing (impossible in practice: the button lives in
                // the form), fall back to defaults from the current parameters.
                let fallback = crate::shell::export_dialog::default_export_choice(self, false);
                let form_id = egui::Id::unique("aevum-export-confirm-form");
                ctx.data_mut(|d| {
                    let form = d.get_temp_mut_or_insert_with(form_id, || fallback);
                    form.out_dir = dir.display().to_string();
                });
            }
            DialogPurpose::LibraryAdd => {
                let DialogPick::Folder(dir) = pick else {
                    return;
                };
                self.lib_add(&dir);
            }
            DialogPurpose::SaveFrameAs => {
                let DialogPick::File(path) = pick else { return };
                let Some(still) = self.dialog_still.take() else {
                    return;
                };
                let path = crate::state::force_container(path, still);
                self.export_current_to(ctx, path);
            }
        }
    }
}
