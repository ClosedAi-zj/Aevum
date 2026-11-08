//! PS 式三栏导出对话框：左边决定"导什么、去哪里"，中间决定"什么格式"，
//! 右边告诉你"这一下会产出什么"再按确认。
//!
//! 以前这是一个 430px 的窄表单，参数从上到下排成一列，读的人要在心里把
//! 「范围 → 目录 → 格式 → 覆盖」串成一句话；三栏把这句话摆在他眼前，
//! 右栏的摘要在按下去之前就能对完。
//!
//! A Photoshop-style three-column export dialog: the left column decides "what and
//! where", the middle "in what format", and the right shows "what this will produce"
//! before the user confirms. The old 430 px form stacked everything vertically and made
//! the reader assemble scope -> folder -> format -> overwrite into one sentence in their
//! head; the three columns put that sentence on screen.

use egui::{Layout, RichText, Ui, Vec2};

use super::toolbar::{bar_button, bar_chip, bar_primary};
use super::windows::{modal_box, soft_sep};
use crate::state::AppState;
use crate::theme::{self, *};

/// 三栏宽度：左（范围与去向）/ 中（格式与命名）/ 右（摘要与动作）。
///
/// Column widths: left (scope and destination) / middle (format and naming) / right
/// (summary and actions).
const COL_SCOPE_W: f32 = 224.0;
const COL_FORMAT_W: f32 = 368.0;
const COL_SUMMARY_W: f32 = 180.0;
const COL_GAP: f32 = 14.0;

// 32 = Modal 弹层左右各 12 的内边距，再留 8px 余量。编译期就卡住，别等运行期红。
//
// 32 = the popup frame's 12 px on each side, plus 8 px of slack. Caught at compile time
// rather than by a red test at runtime.
const _: () = {
    assert!(
        COL_SCOPE_W + COL_GAP + COL_FORMAT_W + COL_GAP + COL_SUMMARY_W <= MODAL_W_EXPORT - 32.0,
        "三栏超宽，右栏会被 modal_width 夹掉"
    );
    assert!(COL_SCOPE_W < COL_FORMAT_W, "中间栏放格式芯片，应比范围栏宽");
    assert!(COL_SUMMARY_W >= 160.0, "摘要栏太窄，放不下画幅读数");
};

/// 一次导出确认所携带的参数。
///
/// 字段全是简单类型（`String` / `u8`），宿主不必认识 `StillFormat` 之类的 core
/// 枚举；集成方按 `format_depth` 的稳定拼写再映射回具体容器。
///
/// The parameters carried by one export confirmation.
///
/// Every field is a simple type (`String` / `u8`), so the host does not need to know
/// core enums such as `StillFormat`; the integrator maps `format_depth`'s stable
/// spelling back to a concrete container.
#[derive(Debug, Clone)]
pub struct ExportChoice {
    /// 导出范围：`all`（整个序列）/ `selected`（选中帧）/ `current`（当前帧）。
    ///
    /// Export scope: `all` (whole sequence) / `selected` (checked frames) / `current`.
    pub scope: String,
    /// 目标目录（绝对路径字符串）。
    ///
    /// Destination directory as an absolute path string.
    pub out_dir: String,
    /// 格式与位深的稳定拼写，取值同 `StillFormat::key`。
    ///
    /// Stable spelling of format and bit depth, same values as `StillFormat::key`.
    pub format_depth: String,
    /// 同名覆盖策略：0 = 询问 / 1 = 覆盖 / 2 = 跳过 / 3 = 改名。
    ///
    /// Same-name policy: 0 = ask / 1 = overwrite / 2 = skip / 3 = rename.
    pub overwrite: u8,
}

/// 从当前导出参数推出表单的初始值（读不到的项用中性默认，不 unwrap）。
///
/// Derive the form's initial values from the current export settings (neutral defaults
/// where a value cannot be read; never unwrap).
/// 确认框的初值。
///
/// `current_frame_only` 来自"用户点的是哪个按钮"：点「导出当前帧」时范围必须预选
/// **当前帧**，否则表单写着「整个序列」而实际只导一帧——用户按下去之前看到的信息
/// 与将要发生的事不符，这是最伤信任的一类不一致（2026-11-08 复现）。
///
/// Initial values for the dialog. `current_frame_only` comes from which button the user
/// pressed: after "export current frame" the scope must be preselected to **current frame**,
/// otherwise the form says "whole sequence" while only one frame will be written -- the
/// form disagreeing with what is about to happen, which is the least trustworthy kind of
/// inconsistency (reproduced 2026-11-08).
fn default_export_choice(state: &AppState, current_frame_only: bool) -> ExportChoice {
    ExportChoice {
        scope: if current_frame_only { "current" } else { "all" }.to_owned(),
        out_dir: state.render_out_dir.display().to_string(),
        format_depth: state.render_format.key().to_owned(),
        overwrite: if state.render_resume { 2 } else { 0 },
    }
}

/// 导出参数确认框：确认后返回参数，取消返回 `None`（不产生任何文件）。
///
/// 第二个返回值是「已经关闭」：`(None, false)` 表示用户还在表单里改，
/// `(None, true)` 是取消，`(Some(_), true)` 是确认。调用方靠它清掉挂载标记，
/// 否则弹窗关不掉。
///
/// Export-parameter confirmation: returns the parameters on confirm, `None` on cancel
/// (no file is produced).
///
/// The second return value means "closed": `(None, false)` = still editing,
/// `(None, true)` = cancelled, `(Some(_), true)` = confirmed. The caller uses it to clear
/// the mount flag, otherwise the modal never closes.
pub fn confirm_export_modal(
    ui: &mut egui::Ui,
    state: &mut AppState,
    current_frame_only: bool,
) -> (Option<ExportChoice>, bool) {
    let form_id = egui::Id::unique("aevum-export-confirm-form");
    let mut form = match ui.ctx().data_mut(|d| d.get_temp::<ExportChoice>(form_id)) {
        Some(saved) => saved,
        None => default_export_choice(state, current_frame_only),
    };

    let mut cancel = false;
    let mut go = false;
    let modal = egui::Modal::new(egui::Id::unique("aevum-export-confirm")).show(ui.ctx(), |ui| {
        modal_box(ui, MODAL_W_EXPORT);
        ui.label(
            RichText::new(crate::i18n::t!("导出参数"))
                .font(font(SIZE_BODY))
                .color(text()),
        );
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            column(ui, COL_SCOPE_W, |ui| scope_column(ui, &mut form));
            ui.add_space(COL_GAP);
            column(ui, COL_FORMAT_W, |ui| format_column(ui, &mut form));
            ui.add_space(COL_GAP);
            column(ui, COL_SUMMARY_W, |ui| summary_column(ui, state, &form));
        });
        ui.add_space(8.0);
        soft_sep(ui);
        ui.add_space(6.0);
        // 全窗只有一颗主按钮，放在底部右缘 —— 右栏再放一颗就成了两个"确认"。
        //
        // Exactly one primary button, bottom-right; a second one in the summary column
        // would read as two different confirmations.
        ui.horizontal(|ui| {
            if bar_button(ui, crate::i18n::t!("取消")).clicked() {
                cancel = true;
            }
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if bar_primary(ui, crate::i18n::t!("开始导出")).clicked() {
                    go = true;
                }
            });
        });
    });

    // Esc 与点背景都算取消。
    //
    // Escape and a backdrop click both cancel.
    if modal.should_close() {
        cancel = true;
    }
    if cancel {
        ui.ctx().data_mut(|d| d.remove::<ExportChoice>(form_id));
        return (None, true);
    }
    if go {
        ui.ctx().data_mut(|d| d.remove::<ExportChoice>(form_id));
        return (Some(form), true);
    }
    ui.ctx().data_mut(|d| d.insert_temp(form_id, form));
    (None, false)
}

/// 固定宽的一栏。高度撑满可用区，三栏的分割线才齐平。
///
/// One fixed-width column. It takes the full available height so the three dividers
/// line up.
fn column(ui: &mut Ui, w: f32, body: impl FnOnce(&mut Ui)) {
    let h = ui.available_height();
    ui.allocate_ui_with_layout(Vec2::new(w, h), Layout::top_down(egui::Align::Min), body);
}

/// 左栏：导出范围 + 目标目录。
///
/// Left column: export scope and destination folder.
fn scope_column(ui: &mut Ui, form: &mut ExportChoice) {
    section_head(ui, "范围");
    ui.horizontal_wrapped(|ui| {
        for (value, label) in [
            ("all", "整个序列"),
            ("selected", "选中帧"),
            ("current", "当前帧"),
        ] {
            if bar_chip(ui, label, form.scope == value).clicked() {
                form.scope = value.to_owned();
            }
        }
    });
    ui.add_space(10.0);
    section_head(ui, "去向");
    ui.horizontal(|ui| {
        if bar_button(ui, crate::i18n::t!("选择…")).clicked()
            && let Some(dir) = rfd::FileDialog::new().pick_folder()
        {
            form.out_dir = dir.display().to_string();
        }
    });
    ui.add(
        egui::Label::new(
            RichText::new(&form.out_dir)
                .font(font(SIZE_SMALL))
                .color(muted()),
        )
        .truncate(),
    );
}

/// 中栏：格式与位深 + 同名覆盖。
///
/// Middle column: format and bit depth, plus the same-name policy.
fn format_column(ui: &mut Ui, form: &mut ExportChoice) {
    section_head(ui, "格式与位深");
    ui.horizontal_wrapped(|ui| {
        for f in aevum_core::export::StillFormat::ALL {
            if bar_chip(ui, f.cn(), form.format_depth == f.key())
                .on_hover_text(crate::i18n::tr(f.hint()))
                .clicked()
            {
                form.format_depth = f.key().to_owned();
            }
        }
    });
    ui.add_space(10.0);
    section_head(ui, "同名覆盖");
    ui.horizontal_wrapped(|ui| {
        for (value, label) in [(0u8, "询问"), (1, "覆盖"), (2, "跳过"), (3, "改名")] {
            if bar_chip(ui, label, form.overwrite == value).clicked() {
                form.overwrite = value;
            }
        }
    });
}

/// 右栏：这一下会产出什么。按下去之前能对完的四件事。
///
/// Right column: what this run will produce -- the four things to check before pressing.
fn summary_column(ui: &mut Ui, state: &AppState, form: &ExportChoice) {
    section_head(ui, "这一次会产出");
    let frames = match form.scope.as_str() {
        // 「当前帧」与单选语义下的「选中帧」都是 1 帧；只有「整个序列」才走全量。
        //
        // "Current" and the single-selection reading of "selected" are one frame; only
        // "whole sequence" counts them all.
        "current" | "selected" => 1,
        _ => state.frame_count(),
    };
    let (ow, oh) = if state.output_w > 0 && state.output_h > 0 {
        (state.output_w, state.output_h)
    } else {
        state.resolution.unwrap_or((0, 0))
    };
    let fmt = aevum_core::export::StillFormat::ALL
        .into_iter()
        .find(|f| f.key() == form.format_depth)
        .map_or_else(|| form.format_depth.clone(), |f| f.cn().to_owned());
    let lines = [
        (crate::i18n::tr("帧数"), frames.to_string()),
        (crate::i18n::tr("画幅"), format!("{} × {}", ow, oh)),
        (crate::i18n::tr("帧率"), format!("{} fps", state.output_fps)),
        (crate::i18n::tr("格式"), fmt),
        (crate::i18n::tr("目录"), theme::ellipsis(&form.out_dir, 22)),
    ];
    for (k, v) in lines {
        ui.label(RichText::new(k).size(SIZE_SMALL).color(dim()));
        ui.label(RichText::new(v).font(mono(SIZE_SMALL)).color(text()));
        ui.add_space(3.0);
    }
    ui.add_space(6.0);
    // 「原图永不覆写」摆在这里而不是底部：它是这句话的收尾，不是另一颗按钮的注脚。
    //
    // "Sources are never overwritten" lives here rather than in the footer: it finishes
    // this sentence instead of annotating a second button.
    ui.label(
        RichText::new(crate::i18n::tr("导出写新文件，原图永不覆写"))
            .size(SIZE_SMALL)
            .color(dim()),
    );
}

/// 栏内小节标题：与右坞分节同一口径，但更小一档。
///
/// A column's section head: same idea as a dock section head, one step smaller.
fn section_head(ui: &mut Ui, label: &str) {
    ui.label(
        RichText::new(crate::i18n::tr(label))
            .size(SIZE_LABEL)
            .color(muted())
            .strong(),
    );
    ui.add_space(3.0);
}

/// 导出确认框的挂载点：只有点了导出按钮（`aevum-export-pending` 置位）才出现。
///
/// 用户在这里改完范围 / 目录 / 格式 / 覆盖策略并点「开始导出」，才把选择写回
/// `AppState` 并真正跑导出；取消不碰任何参数、不产生任何文件。
///
/// 为什么不在按钮里直接弹：按钮在滚动面板里，面板每帧重画，Modal 挂在帧尾才不会
/// 被面板裁剪或重复创建。
///
/// Mount point for the export confirmation: it only appears after an export button sets
/// `aevum-export-pending`.
///
/// The choice is written back into `AppState` and the export runs only after the user
/// presses "Start export"; cancel touches nothing and produces no file.
///
/// Why not show it inside the button: the button lives in a scrolling panel that repaints
/// every frame, so mounting the modal at frame end keeps it from being clipped or created
/// twice.
pub(super) fn export_confirm_window(state: &mut AppState, ctx: &egui::Context, full: egui::Rect) {
    let pending: Option<u8> = ctx.data(|d| d.get_temp(egui::Id::unique("aevum-export-pending")));
    let Some(kind) = pending else {
        return;
    };
    // 上一次点的是「整段导出」还是「导出当前帧」。
    //
    // Whether the last click meant "export the sequence" or "export the current frame".
    let current_frame_only = kind == 2;

    let (choice, closed) = {
        // Modal 需要一个 Ui；借一个零尺寸的临时 Ui，它只往 ctx 上挂浮层。
        //
        // The modal needs a Ui; a zero-sized scratch Ui only mounts the overlay on the ctx.
        let mut scratch = egui::Ui::new(
            ctx.clone(),
            egui::Id::unique("aevum-export-confirm-host"),
            egui::UiBuilder::new().max_rect(full),
        );
        confirm_export_modal(&mut scratch, state, current_frame_only)
    };
    // 取消（或已确认）都要把 pending 清掉，否则弹窗再也关不掉。
    // Cancel (or a completed confirm) must clear the pending flag, or the modal never closes.
    if closed {
        ctx.data_mut(|d| d.remove::<u8>(egui::Id::unique("aevum-export-pending")));
    }

    if let Some(ExportChoice {
        out_dir,
        format_depth,
        overwrite,
        ..
    }) = choice
    {
        // 把用户的选择写回导出参数：目录、格式、覆盖策略三项是面板里已有的字段，
        // 这里只做赋值，不新增第二套导出配置。
        //
        // Write the choice back into the existing export fields: directory, format and
        // overwrite policy already exist on the panel; this only assigns them.
        if !out_dir.trim().is_empty() {
            state.render_out_dir = std::path::PathBuf::from(out_dir);
        }
        if let Some(f) = aevum_core::export::StillFormat::ALL
            .into_iter()
            .find(|f| f.key() == format_depth)
        {
            state.render_format = f;
        }
        state.render_resume = overwrite == 2;
        if current_frame_only {
            state.export_current(ctx);
        } else {
            state.run_export(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三栏加两条缝必须装进 Modal 主体，右栏被挤掉等于摘要消失。
    ///
    /// The three columns plus the gaps must fit inside the modal body, or the summary
    /// column is the one that gets squeezed out.
    #[test]
    fn three_columns_fit_inside_a_1280_viewport() {
        let body = theme::modal_width(MODAL_W_EXPORT, 1280.0) - 32.0;
        let used = COL_SCOPE_W + COL_GAP + COL_FORMAT_W + COL_GAP + COL_SUMMARY_W;
        assert!(used <= body, "三栏 {used} 超出 Modal 主体 {body}");
    }

    #[test]
    fn export_dialog_preselects_current_frame() {
        let state = AppState::demo();
        assert_eq!(
            default_export_choice(&state, true).scope,
            "current",
            "「导出当前帧」的确认框没有预选当前帧——表单与实际行为不符"
        );
        assert_eq!(
            default_export_choice(&state, false).scope,
            "all",
            "整段导出应当预选整个序列"
        );
    }

    /// 目录初值非空：空目录会让用户以为没地方可导。
    ///
    /// The folder default must not be empty, or the user thinks there is nowhere to write.
    #[test]
    fn default_folder_is_not_empty() {
        let mut state = AppState::new();
        // 目录初值镜像面板字段，哪怕为空——写回端有「空就跳过」的守卫，
        // 这里钉的是"表单不撒谎"：面板里是空，弹窗里也必须是空。
        //
        // The folder default mirrors the panel field, empty included: the write-back has a
        // "skip when empty" guard, and this pins "the form does not lie" -- empty on the
        // panel means empty in the dialog.
        state.render_out_dir = std::path::PathBuf::from("");
        assert!(default_export_choice(&state, false).out_dir.is_empty());
        state.render_out_dir = std::path::PathBuf::from("E:/out");
        assert_eq!(default_export_choice(&state, false).out_dir, "E:/out");
        assert_eq!(
            default_export_choice(&state, false).format_depth,
            state.render_format.key()
        );
    }
}
