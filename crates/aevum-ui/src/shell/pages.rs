//! 流程三页：序列管理 / 调色与修复（画布）/ 导出视频。
//!
//! 用户 2026-11-09 要求三个步骤各自是**独立界面**，不是同一个画布换页签：
//! 第 1 步看整条序列（缩略图墙 + 导入/去闪/对齐），第 2 步才是画布显影，
//! 第 3 步是导出工作台（队列 + 设置 + 进度，参考 LapseOne 的导出视频页）。
//!
//! The three flow pages: sequence manager / develop (canvas) / export video.
//!
//! The user (2026-11-09) asked for three **independent views** rather than one canvas
//! with different tabs: step 1 shows the whole sequence (thumbnail wall + import /
//! deflicker / align), step 2 is the develop canvas, step 3 is an export workbench
//! (queue + settings + progress, modelled on LapseOne's export page).

use egui::{Context, RichText, Sense, Ui, Vec2};

use super::dialogs;
use crate::state::{AppState, Module};
use crate::theme::*;

/// 序列管理页：缩略图墙 + 本段的直行动作。
///
/// Sequence manager: a thumbnail wall plus this phase's direct actions.
pub(super) fn sequence_page(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::tr("序列管理"))
                .size(SIZE_TITLE)
                .color(text()),
        );
        ui.label(
            RichText::new(crate::i18n::tr_fmt("{} 帧", &[&state.frame_count()]))
                .size(SIZE_LABEL)
                .color(muted()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if secondary_button(ui, "转到对齐").clicked() {
                state.set_module(Module::Frame);
            }
            if secondary_button(ui, "转到去闪").clicked() {
                state.set_module(Module::Deflicker);
            }
            if secondary_button(ui, "重新扫描").clicked() {
                dialogs::rescan(state, ctx);
            }
            if primary_button(ui, "导入照片…").clicked() {
                dialogs::open_photos_dialog(state);
            }
        });
    });
    ui.add_space(4.0);
    rule(
        ui.painter(),
        ui.max_rect().left() + 6.0,
        ui.max_rect().right() - 6.0,
        ui.cursor().top(),
        divider(),
    );
    ui.add_space(8.0);

    if state.frame_count() == 0 {
        ui.label(
            RichText::new(crate::i18n::tr(
                "还没有打开序列。导入一个目录，或把照片拖进窗口。",
            ))
            .size(SIZE_BODY)
            .color(dim()),
        );
        return;
    }

    // 缩略图墙：固定格子、按可用宽度算列数；只给可见区预热缩略图，
    // 千帧序列也不会一次性解码。
    //
    // Thumbnail wall: fixed cells, columns from the available width; only the visible
    // range is warmed, so a thousand-frame sequence never decodes in one go.
    const CELL_W: f32 = 104.0;
    const CELL_H: f32 = 78.0;
    const GAP: f32 = 8.0;
    let avail = ui.available_width().max(CELL_W);
    let cols = (((avail + GAP) / (CELL_W + GAP)).floor() as usize).max(1);
    let rows = state.frame_count().div_ceil(cols);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, CELL_H + GAP, rows, |ui, row_range| {
            let lo = row_range.start * cols;
            let hi = (row_range.end * cols).min(state.frame_count());
            state.ensure_thumbs(ctx, lo.saturating_sub(1), hi + 1);
            for row in row_range {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let i = row * cols + col;
                        if i >= state.frame_count() {
                            break;
                        }
                        cell(state, ui, i, CELL_W, CELL_H);
                    }
                });
                ui.add_space(GAP);
            }
        });
}

/// 一格：序号 + 缩略图；选中格描强调色边。
///
/// One cell: index plus thumbnail; the selected cell gets an accent outline.
fn cell(state: &mut AppState, ui: &mut Ui, i: usize, w: f32, h: f32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, radius_control(), thumb_bg());
    if let Some(t) = state.thumb(i) {
        let tex = t.texture.clone();
        let size = t.size;
        let scale = (w / size[0] as f32).min(h / size[1] as f32);
        let draw = Vec2::new(size[0] as f32 * scale, size[1] as f32 * scale);
        let r = egui::Rect::from_center_size(rect.center(), draw);
        p.image(
            tex.id(),
            r,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }
    let selected = state.selected == i;
    let stroke = if selected { accent() } else { divider() };
    p.rect_stroke(
        rect,
        radius_control(),
        egui::Stroke::new(if selected { 2.0 } else { 1.0 }, stroke),
        egui::StrokeKind::Inside,
    );
    p.text(
        rect.left_bottom() + Vec2::new(4.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        format!("{}", i + 1),
        font(SIZE_SMALL),
        if selected { on_accent() } else { muted() },
    );
    if resp.clicked() {
        state.selected = i;
    }
    resp.on_hover_text(format!("第 {} 帧", i + 1));
}

/// 导出视频页：LapseOne 式「队列 + 设置 + 进度」。
///
/// Export page: LapseOne-style queue + settings + progress.
pub(super) fn export_page(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::tr("导出视频"))
                .size(SIZE_TITLE)
                .color(text()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let busy = state.queue_busy;
            if busy {
                if primary_button(ui, "取消队列").clicked() {
                    state.stop_queue();
                }
            } else {
                if primary_button(ui, "开始批量渲染").clicked()
                    && let Err(e) = state.start_queue(ctx)
                {
                    state.error = Some(e);
                }
                if secondary_button(ui, "加入当前序列").clicked()
                    && let Err(e) = state.add_current_to_queue()
                {
                    state.error = Some(e);
                }
                if !state.queue.is_empty() && secondary_button(ui, "清空队列").clicked() {
                    state.clear_queue_rows();
                }
            }
        });
    });
    ui.add_space(6.0);

    if let Some((done, total, stage)) = state.queue_progress() {
        let frac = if total > 0 {
            done as f32 / total as f32
        } else {
            0.0
        };
        let phase = match stage {
            aevum_core::export::Stage::Frames => "写帧",
            aevum_core::export::Stage::Encode => "编码",
        };
        ui.add(
            egui::ProgressBar::new(frac).desired_height(10.0).text(
                RichText::new(crate::i18n::tr_fmt("{}{}/{}", &[&phase, &done, &total]))
                    .size(SIZE_SMALL),
            ),
        );
        ui.add_space(6.0);
    }

    ui.columns(2, |cols| {
        // 左：队列
        let left = &mut cols[0];
        left.label(
            RichText::new(crate::i18n::tr("渲染队列"))
                .size(SIZE_BODY)
                .color(text()),
        );
        left.add_space(4.0);
        if state.queue.is_empty() {
            left.label(
                RichText::new(crate::i18n::tr(
                    "队列是空的。点「加入当前序列」，或到图库右键任意序列入队。",
                ))
                .size(SIZE_LABEL)
                .color(dim()),
            );
        } else {
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(left, |ui| {
                    local_queue_table(state, ui);
                });
        }

        // 右：导出设置
        let right = &mut cols[1];
        right.label(
            RichText::new(crate::i18n::tr("导出设置"))
                .size(SIZE_BODY)
                .color(text()),
        );
        right.add_space(4.0);
        egui::Grid::new("export-settings")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(right, |ui| {
                ui.label(
                    RichText::new(crate::i18n::tr("帧率"))
                        .size(SIZE_LABEL)
                        .color(muted()),
                );
                ui.add(
                    egui::DragValue::new(&mut state.output_fps)
                        .range(1.0..=60.0)
                        .speed(1.0),
                );
                ui.end_row();
                ui.label(
                    RichText::new(crate::i18n::tr("画质 CRF"))
                        .size(SIZE_LABEL)
                        .color(muted()),
                );
                ui.add(egui::DragValue::new(&mut state.render_crf).range(0..=51));
                ui.end_row();
                ui.label(
                    RichText::new(crate::i18n::tr("编码"))
                        .size(SIZE_LABEL)
                        .color(muted()),
                );
                ui.horizontal(|ui| {
                    for (name, tag) in [("H.265", "h265"), ("H.264", "h264")] {
                        let on = state.codec.contains(tag);
                        let resp = ui.add(
                            egui::Button::new(RichText::new(name).size(SIZE_SMALL).color(if on {
                                on_selected()
                            } else {
                                text()
                            }))
                            .fill(if on { accent() } else { elev() })
                            .stroke(egui::Stroke::new(1.0, if on { accent() } else { border() }))
                            .corner_radius(radius_control())
                            .min_size(Vec2::new(56.0, 24.0)),
                        );
                        if resp.clicked() {
                            state.codec = format!("libx{tag}");
                        }
                    }
                });
                ui.end_row();
                ui.label(
                    RichText::new(crate::i18n::tr("输出"))
                        .size(SIZE_LABEL)
                        .color(muted()),
                );
                ui.label(
                    RichText::new(crate::theme::ellipsis(
                        &state.render_video_out.display().to_string(),
                        42,
                    ))
                    .size(SIZE_SMALL)
                    .color(value()),
                );
                ui.end_row();
            });
    });
}

/// 常规按钮（中性底 + 1px 边）。
///
/// A regular button: neutral fill with a hairline.
fn secondary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(crate::i18n::tr(label))
                .size(SIZE_LABEL)
                .color(text()),
        )
        .fill(elev())
        .stroke(egui::Stroke::new(1.0, border()))
        .corner_radius(radius_control())
        .min_size(Vec2::new(0.0, 26.0)),
    )
}

/// 主行动按钮（强调色底）。
///
/// The primary action: accent fill.
fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            RichText::new(crate::i18n::tr(label))
                .size(SIZE_LABEL)
                .color(on_accent()),
        )
        .fill(accent())
        .stroke(egui::Stroke::new(1.0, accent_hover()))
        .corner_radius(radius_control())
        .min_size(Vec2::new(0.0, 26.0)),
    )
}

/// 紧凑队列行：序号 / 序列名 / 输出 / 移除。渲染中的那一段带进度条。
///
/// Compact queue rows: index / sequence / output / remove; the running segment carries
/// a progress bar.
fn local_queue_table(state: &mut AppState, ui: &mut Ui) {
    let mut remove: Option<usize> = None;
    let n = state.queue.len();
    for (i, item) in state.queue.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{:02}", i + 1))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            let name = std::path::Path::new(&item.root)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| item.root.clone());
            ui.label(
                RichText::new(crate::theme::ellipsis(&name, 22))
                    .size(SIZE_LABEL)
                    .color(if state.queue_current == Some(i) {
                        text()
                    } else {
                        value()
                    }),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !state.queue_busy
                    && ui
                        .small_button("×")
                        .on_hover_text(crate::i18n::tr("移除这一段"))
                        .clicked()
                {
                    remove = Some(i);
                }
                ui.label(
                    RichText::new(format!("{:.0}p · {}", item.fps, item.codec))
                        .size(SIZE_SMALL)
                        .color(muted()),
                );
            });
        });
        if state.queue_current == Some(i)
            && state.queue_busy
            && let Some((done, total, _)) = state.queue_progress()
        {
            let frac = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            ui.add(egui::ProgressBar::new(frac).desired_height(3.0));
        }
        ui.add_space(2.0);
    }
    if let Some(i) = remove {
        state.remove_queue_at(i);
    }
    let _ = n;
}
