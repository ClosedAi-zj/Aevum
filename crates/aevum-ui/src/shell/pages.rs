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
                .font(crate::theme::bold(SIZE_TITLE))
                .color(text()),
        );
        // LRT 的规则：用数字给进度（`KF 3/8`），比一句话快。当前帧是关键帧时
        // 额外报出它是第几个，导航时不至于「不知道走到哪了」。
        //
        // LRT's rule: report progress as numbers (`KF 3/8`); digits beat sentences.
        // When the current frame is a keyframe, also say which one it is.
        let kf_n = state.keyframes.len();
        let kf_pos = state
            .keyframes
            .keys()
            .position(|k| *k == state.selected)
            .map(|i| i + 1);
        let stat = match kf_pos {
            Some(pos) => {
                crate::i18n::tr_fmt("{} 帧 · KF {}/{}", &[&state.frame_count(), &pos, &kf_n])
            }
            None => crate::i18n::tr_fmt("{} 帧 · {} 关键帧", &[&state.frame_count(), &kf_n]),
        };
        ui.label(RichText::new(stat).size(SIZE_LABEL).color(muted()));
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
            // LRT 式关键帧：只给「打点/取消」两颗，过渡由既有 keyframes 表驱动。
            // 与 LR 的评级打点同源：先打稀疏关键帧，再只在这些帧上动色。
            //
            // LRT-style keyframes: just mark / unmark; interpolation is driven by the
            // existing keyframes map. Same idea as LR rating marks: sparse checkpoints,
            // grade only those.
            // LRT 预览工具的「单箭头 = 跳上/下一个关键帧」。我们没有复制语义，
            // 所以只做跳转这一对，图标用 ‹ › 保持与 LRT 同形；无关键帧时置灰。
            //
            // LRT's transport uses single arrows to jump to the previous/next keyframe.
            // We have no "sync to next" action, so only the jump pair exists; ‹ › keeps
            // the same shape, and both are disabled when there are no keyframes.
            let prev_kf = state
                .keyframes
                .range(..state.selected)
                .next_back()
                .map(|(k, _)| *k);
            let next_kf = state
                .keyframes
                .range((
                    std::ops::Bound::Excluded(state.selected),
                    std::ops::Bound::Unbounded,
                ))
                .next()
                .map(|(k, _)| *k);
            if ui
                .add_enabled(
                    prev_kf.is_some(),
                    egui::Button::new(RichText::new("‹ KF").size(SIZE_SMALL))
                        .corner_radius(radius_control())
                        .min_size(Vec2::new(0.0, CH)),
                )
                .on_hover_text(crate::i18n::tr("上一个关键帧"))
                .clicked()
                && let Some(k) = prev_kf
            {
                state.selected = k;
            }
            if ui
                .add_enabled(
                    next_kf.is_some(),
                    egui::Button::new(RichText::new("KF ›").size(SIZE_SMALL))
                        .corner_radius(radius_control())
                        .min_size(Vec2::new(0.0, CH)),
                )
                .on_hover_text(crate::i18n::tr("下一个关键帧"))
                .clicked()
                && let Some(k) = next_kf
            {
                state.selected = k;
            }
            if state.keyframes.contains_key(&state.selected) {
                if secondary_button(ui, "取消关键帧").clicked() {
                    state.remove_keyframe(state.selected);
                }
            } else if secondary_button(ui, "标记关键帧").clicked() {
                state.add_keyframe();
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
                    // 列数是用 GAP 算的，布局必须用同一个 GAP——否则 egui 默认的
                    // item_spacing 与它不一致，右边几列会被挤出可视宽度。
                    //
                    // The column count is derived from GAP, so the layout must use that
                    // same GAP; otherwise egui's default spacing differs and the
                    // right-hand columns are pushed out of view.
                    ui.spacing_mut().item_spacing.x = GAP;
                    for col in 0..cols {
                        let i = row * cols + col;
                        if i >= state.frame_count() {
                            break;
                        }
                        cell(state, ui, ctx, i, CELL_W, CELL_H);
                    }
                });
                ui.add_space(GAP);
            }
        });
}

/// 一格：序号 + 缩略图；选中格描强调色边。
///
/// One cell: index plus thumbnail; the selected cell gets an accent outline.
/// 一格缩略图。
///
/// **画到哪格就补哪格**：此前按「行号 × 列数」估算预热区间，但每行实际能放几格
/// 受 `item_spacing` 影响，估算列数偏大时右边几列永远不在预热区间里——整片格子
/// 长期纯黑（用户 2026-11-09 截图：37–70 全黑）。改为在绘制点按需派发，与布局
/// 计算解耦。
///
/// One thumbnail cell.
///
/// **Prefetch the cell you are drawing**: the old code estimated the warm-up range
/// from "row index x column count", but how many cells actually fit per row depends on
/// `item_spacing`; an over-estimated column count left the right-hand columns outside
/// the warm-up range and they stayed black (user screenshot, 2026-11-09).
fn cell(state: &mut AppState, ui: &mut Ui, ctx: &Context, i: usize, w: f32, h: f32) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, radius_control(), thumb_bg());
    // 缺图就当场派发这一格（后台解码，不阻塞本帧），并画一个占位底：
    // 纯黑会被读成「坏格」，比「加载中」更糟。
    //
    // Missing thumbnail: dispatch this very frame (background decode, does not block
    // the frame) and draw a placeholder instead of leaving it pure black -- black reads
    // as a broken cell, which is worse than "loading".
    if state.thumb(i).is_none() {
        state.ensure_thumbs(ctx, i, i + 1);
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "…",
            font(SIZE_SMALL),
            muted(),
        );
    }
    if let Some(t) = state.thumb(i) {
        let tex = t.texture.clone();
        let size = t.size;
        // 尺寸为 0 时 `w / 0` 会得 inf/NaN，随后 `from_center_size` 拿到 NaN 矩形——
        // 轻则这一格空白，重则 egui 的几何断言失败。给分母兜一个 1。
        //
        // A zero-sized texture makes `w / 0` inf/NaN, and `from_center_size` then gets a
        // NaN rect: at best the cell is blank, at worst egui's geometry assert fires.
        // Floor the divisor at 1.
        let sw = size[0].max(1) as f32;
        let sh = size[1].max(1) as f32;
        let scale = (w / sw).min(h / sh);
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
    // 悬停：升一档底 + 描边提亮。第一屏几十个可点格子划过毫无变化，是「死界面」
    // 最直接的一层（2026-11-09 交互审计 #1）。
    //
    // Hover: one step up plus a brighter stroke. Dozens of clickable cells that do not
    // react on hover are the most direct "dead interface" signal.
    if !selected && resp.hovered() {
        p.rect_filled(rect, radius_control(), row_hover());
    }
    let stroke = if selected {
        accent()
    } else if resp.hovered() {
        border_strong()
    } else {
        divider()
    };
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
    // 关键帧角标：LRT 里关键帧就是稀疏的「锚」，必须一眼可见。
    //
    // Keyframe badge: in LRT a keyframe is a sparse anchor and must be visible at a glance.
    if state.keyframes.contains_key(&i) {
        let chip =
            egui::Rect::from_min_size(rect.left_top() + Vec2::new(4.0, 4.0), Vec2::new(14.0, 14.0));
        p.rect_filled(chip, radius_chip(), accent());
        p.text(
            chip.center(),
            egui::Align2::CENTER_CENTER,
            "K",
            font(SIZE_SMALL),
            on_accent(),
        );
    }
    if resp.clicked() {
        state.selected = i;
    }
    // 双击直接进调色（用户 2026-11-09）：序列页是「挑帧」的地方，挑完就该调。
    //
    // Double-click jumps straight to develop (user, 2026-11-09): this page is for
    // picking a frame, and once picked the next action is grading.
    if resp.double_clicked() {
        state.selected = i;
        goto_develop(state);
    }
    resp.on_hover_text(format!("第 {} 帧 · 双击进调色", i + 1));
}

/// 导出视频页：LapseOne 式「队列 + 设置 + 进度」。
///
/// Export page: LapseOne-style queue + settings + progress.
pub(super) fn export_page(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::tr("导出视频"))
                .font(crate::theme::bold(SIZE_TITLE))
                .color(text()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if state.queue_busy {
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

    // 概况：帧数 / 片长 / 队列段数。LapseOne 的导出页先回答「要导什么」。
    let fps = state.output_fps.max(1.0);
    let secs = state.frame_count() as f32 / fps;
    ui.horizontal(|ui| {
        for (k, v) in [
            ("序列帧数", format!("{}", state.frame_count())),
            (
                "片长",
                format!("{:02}:{:02}", (secs / 60.0) as u32, (secs % 60.0) as u32),
            ),
            ("队列", crate::i18n::tr_fmt("{} 段", &[&state.queue.len()])),
        ] {
            card_stat(ui, k, &v);
        }
    });

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
        ui.add_space(6.0);
        ui.add(
            egui::ProgressBar::new(frac).desired_height(10.0).text(
                RichText::new(crate::i18n::tr_fmt("{}{}/{}", &[&phase, &done, &total]))
                    .size(SIZE_SMALL),
            ),
        );
    }

    ui.add_space(8.0);

    // 主体：左 62% 队列、右 38% 设置，**两张卡都撑到同一高度**并把剩余空间
    // 用实底填住——内容少时若只按内容高度画，页面会读成「没做完」
    // （用户 2026-11-09 连续两次指出）。
    //
    // Body: 62% queue / 38% settings, both cards forced to the same height and filled
    // with solid colour. Drawing them content-sized made the page read as unfinished.
    const GAP: f32 = 10.0;
    const ACTION_H: f32 = 46.0;
    let avail = ui.available_size();
    let left_w = ((avail.x - GAP) * 0.62).max(340.0);
    let right_w = (avail.x - GAP - left_w).max(280.0);
    let body_h = (avail.y - ACTION_H - 8.0).max(240.0);

    ui.horizontal_top(|ui| {
        frame_card(ui, left_w, body_h, |ui| {
            ui.label(
                RichText::new(crate::i18n::tr("渲染队列"))
                    .font(crate::theme::bold(SIZE_BODY))
                    .color(text()),
            );
            ui.add_space(8.0);
            if state.queue.is_empty() {
                // 空态：垂直居中的「图标 + 一句 + 主按钮」，不是整块空洞。
                //
                // Empty state: a centred icon + one line + the primary action, not a void.
                let h = (body_h - 56.0).max(120.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), h),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        ui.add_space(h * 0.28);
                        ui.label(
                            RichText::new(crate::i18n::tr("队列还是空的"))
                                .size(SIZE_BODY)
                                .color(dim()),
                        );
                        ui.add_space(8.0);
                        if primary_button(ui, "加入当前序列").clicked()
                            && let Err(e) = state.add_current_to_queue()
                        {
                            state.error = Some(e);
                        }
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(crate::i18n::tr("也可以到图库右键任意序列入队"))
                                .size(SIZE_SMALL)
                                .color(muted()),
                        );
                    },
                );
            } else {
                queue_head(ui);
                egui::ScrollArea::vertical()
                    .max_height(body_h - 60.0)
                    .show(ui, |ui| {
                        local_queue_table(state, ui);
                    });
            }
        });

        ui.add_space(GAP);
        frame_card(ui, right_w, body_h, |ui| {
            ui.label(
                RichText::new(crate::i18n::tr("导出设置"))
                    .font(crate::theme::bold(SIZE_BODY))
                    .color(text()),
            )
            .on_hover_text(crate::i18n::tr(
                "每段读取加入队列时的工程参数；批量渲染按顺序逐段执行。",
            ));
            ui.add_space(10.0);
            // 预设：一行一个「成品」，而不是让用户自己拼帧率+CRF+编码三件套。
            // LapseOne 的渲染对话框就是把 Codec 做成一行行预设。
            //
            // Presets: one row per finished spec instead of making the user assemble
            // fps + CRF + codec by hand (LapseOne's render dialog does the same).
            ui.label(
                RichText::new(crate::i18n::tr("预设"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (name, fps, crf, codec) in [
                    ("标准 1080p", 24.0f32, 20u8, "h265"),
                    ("4K UHD", 24.0, 18, "h265"),
                    ("高码率", 30.0, 14, "h264"),
                ] {
                    if secondary_button(ui, name).clicked() {
                        state.output_fps = fps;
                        state.render_crf = crf;
                        state.codec = format!("libx{codec}");
                    }
                }
            });

            ui.add_space(10.0);
            section_rule(ui);
            ui.label(
                RichText::new(crate::i18n::tr("输出"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            ui.add_space(4.0);
            egui::Grid::new("export-out")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
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
                        RichText::new(crate::i18n::tr("分辨率"))
                            .size(SIZE_LABEL)
                            .color(muted()),
                    );
                    ui.label(
                        RichText::new(crate::i18n::tr_fmt(
                            "{} × {}",
                            &[&state.output_w, &state.output_h],
                        ))
                        .size(SIZE_LABEL)
                        .color(value()),
                    );
                    ui.end_row();
                });

            ui.add_space(10.0);
            section_rule(ui);
            ui.label(
                RichText::new(crate::i18n::tr("编码"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (name, tag) in [("H.265", "h265"), ("H.264", "h264")] {
                    let on = state.codec.contains(tag);
                    let resp = if on {
                        primary_button(ui, name)
                    } else {
                        secondary_button(ui, name)
                    };
                    if resp.clicked() {
                        state.codec = format!("libx{tag}");
                    }
                }
            });
            ui.add_space(8.0);
            egui::Grid::new("export-enc")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(crate::i18n::tr("画质 CRF"))
                            .size(SIZE_LABEL)
                            .color(muted()),
                    );
                    ui.add(egui::DragValue::new(&mut state.render_crf).range(0..=51));
                    ui.end_row();
                });

            ui.add_space(10.0);
            section_rule(ui);
            ui.label(
                RichText::new(crate::i18n::tr("文件"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(crate::theme::ellipsis(
                    &state.render_video_out.display().to_string(),
                    34,
                ))
                .size(SIZE_SMALL)
                .color(value()),
            )
            .on_hover_text(state.render_video_out.display().to_string());
        });
    });

    // 底部行动条：跨满宽的实底条，主行动固定在右侧——LapseOne 的「开始渲染」
    // 就是这样一条独立横条，而不是挤在标题行角落。
    //
    // Bottom action bar: a full-width strip with the primary action pinned right,
    // matching LapseOne instead of tucking the button into the title row.
    ui.add_space(8.0);
    let w = ui.available_width();
    egui::Frame::NONE
        .fill(elev())
        .stroke(egui::Stroke::new(1.0, border()))
        .corner_radius(radius_control())
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(w - 20.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(crate::i18n::tr_fmt(
                        "{} 段排队 · 输出 {}",
                        &[
                            &state.queue.len(),
                            &crate::theme::ellipsis(
                                &state.render_video_out.display().to_string(),
                                28,
                            ),
                        ],
                    ))
                    .size(SIZE_SMALL)
                    .color(muted()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state.queue_busy {
                        if primary_button(ui, "取消队列").clicked() {
                            state.stop_queue();
                        }
                    } else if primary_button(ui, "开始批量渲染").clicked()
                        && let Err(e) = state.start_queue(ctx)
                    {
                        state.error = Some(e);
                    }
                });
            });
        });
}

/// 定宽定高的卡片：先铺实底与描边，再让内容在固定区域内排版。
///
/// A fixed-size card: solid fill and stroke first, then content inside that box.
fn frame_card<R>(ui: &mut Ui, w: f32, h: f32, add: impl FnOnce(&mut Ui) -> R) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), Sense::hover());
    // 关键：Frame::show 的高度由内容决定，allocate 出来的 h 会被无视——这正是
    // 前几轮「卡片改成定高却依然只有一小截」的原因。直接用 painter 铺满占位
    // 矩形，骨架高度才真正等于 h。
    //
    // Frame::show sizes itself from its content, ignoring the height we allocated;
    // that is why "fixed-height cards" stayed a stub. Paint the allocated rect directly
    // so the skeleton really is h tall.
    let p = ui.painter();
    let r = radius_panel();
    p.rect_filled(rect, r, elev());
    p.rect_stroke(
        rect,
        r,
        egui::Stroke::new(1.0, border()),
        egui::StrokeKind::Inside,
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(12.0))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    add(&mut child);
}
/// 设置区的小节分隔线。
///
/// Section divider inside the settings card.
fn section_rule(ui: &mut Ui) {
    let w = ui.available_width();
    let y = ui.cursor().top();
    rule(
        ui.painter(),
        ui.cursor().left(),
        ui.cursor().left() + w,
        y,
        divider(),
    );
}

/// 概况格：小标签 + 值。
///
/// A summary cell: small label plus value.
fn card_stat(ui: &mut Ui, k: &str, v: &str) {
    let frame = egui::Frame::NONE
        .fill(elev())
        .stroke(egui::Stroke::new(1.0, border()))
        .corner_radius(radius_control())
        .inner_margin(egui::Margin::symmetric(12, 6));
    frame.show(ui, |ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr(k))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            ui.label(RichText::new(v).size(SIZE_BODY).color(text()));
        });
    });
}

/// 队列列头。
///
/// Queue table header.
fn queue_head(ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::tr("序号"))
                .size(SIZE_SMALL)
                .color(muted()),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(crate::i18n::tr("序列"))
                .size(SIZE_SMALL)
                .color(muted()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(crate::i18n::tr("规格"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
        });
    });
    rule(
        ui.painter(),
        ui.max_rect().left() + 4.0,
        ui.max_rect().right() - 4.0,
        ui.cursor().top(),
        divider(),
    );
    ui.add_space(4.0);
}

/// 常规按钮（中性底 + 1px 边）。
///
/// A regular button: neutral fill with a hairline.
/// 常规按钮。**不写 `.fill()/.stroke()`**：egui 文档明写这两个会覆盖悬停与
/// 按下效果（`button.rs` "this will override any on-hover effects"），
/// 之前 11 颗按钮因此悬停/按下毫无变化——这是「点了像图片」的最大来源。
/// 底色与三态交给 `theme::apply_style` 里统一配好的 widgets。
///
/// A regular button. **No `.fill()` / `.stroke()`**: egui documents that they override
/// the hover and pressed effects. Eleven buttons had no hover/press change because of
/// them -- the biggest source of "clicking feels like clicking an image".
fn secondary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(crate::i18n::tr(label)).size(SIZE_LABEL))
            .corner_radius(radius_control())
            .min_size(Vec2::new(0.0, CH)),
    )
}

/// 主行动按钮（强调色底）。
///
/// The primary action: accent fill.
/// 主行动按钮：强调色底，但**三态仍由 style 驱动**（悬停更亮、按下更深），
/// 而不是用 `.fill()` 把三种状态钉成同一个颜色。
///
/// The primary action: accent fill, but its three states still come from the style
/// (brighter on hover, deeper on press) instead of `.fill()` pinning all three to one
/// colour.
fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.style_mut().visuals.widgets;
        w.inactive.bg_fill = accent();
        w.inactive.weak_bg_fill = accent();
        w.inactive.fg_stroke = egui::Stroke::new(1.0, on_accent());
        w.inactive.bg_stroke = egui::Stroke::new(1.0, accent());
        w.hovered.bg_fill = accent_hover();
        w.hovered.weak_bg_fill = accent_hover();
        w.hovered.fg_stroke = egui::Stroke::new(1.0, on_accent());
        w.hovered.bg_stroke = egui::Stroke::new(1.0, accent_hover());
        w.active.bg_fill = accent_dim();
        w.active.weak_bg_fill = accent_dim();
        w.active.fg_stroke = egui::Stroke::new(1.0, on_accent());
        w.active.bg_stroke = egui::Stroke::new(1.0, accent_dim());
        ui.add(
            egui::Button::new(RichText::new(crate::i18n::tr(label)).size(SIZE_LABEL))
                .corner_radius(radius_control())
                .min_size(Vec2::new(0.0, CH)),
        )
    })
    .inner
}

/// 紧凑队列行：序号 / 序列名 / 输出 / 移除。渲染中的那一段带进度条。
///
/// Compact queue rows: index / sequence / output / remove; the running segment carries
/// a progress bar.
fn local_queue_table(state: &mut AppState, ui: &mut Ui) {
    let mut remove: Option<usize> = None;
    // 进度只问一次：放在循环里就是每行一次 `queue_progress()`，队列一长变成
    // O(n²)/帧，而它的结果对整张表是同一个值。
    //
    // Ask for the progress once: inside the loop it ran per row, turning the table
    // into O(n^2) per frame even though the answer is the same for every row.
    let progress = if state.queue_busy {
        state.queue_progress()
    } else {
        None
    };
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
            && let Some((done, total, _)) = progress
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
}

/// 跳到调色页：把当前工作流的「调色与修复」段设为手选步，并切到显影模块。
/// 放在本文件而不是 `flow_panel`，是为了让「手选步唯一写入口」那条判据仍然只数
/// 顶栏那一处。
///
/// Jump to the develop page: pin the flow's develop phase as the hand-picked step and
/// switch to the Develop module. Kept here (not in `flow_panel`) so the "single write
/// path for the hand-picked step" fence still counts only the top bar.
fn goto_develop(state: &mut AppState) {
    let task = state.flow_task;
    if let Some(i) = task
        .steps()
        .iter()
        .position(|step| step.module == Module::Develop)
    {
        state.flow_step = Some(i);
    }
    state.set_module(Module::Develop);
}
