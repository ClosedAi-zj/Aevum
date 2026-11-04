//! 调整组全家：显影/颜色/细节/成片队列等右侧 dock 的调整面板。
//!
//! All adjustment groups: the develop / colour / detail / render queue panels of the
//! right dock.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::BAR_H;
use super::common::group_header;
use super::common::in_field;
use super::common::kv;
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

/// 体检清单的展示顺序：硬故障在前，软嫌疑在后，缺口单独一档。
const QA_KINDS: [qa::Kind; 6] = [
    qa::Kind::Read,
    qa::Kind::Black,
    qa::Kind::Gap,
    qa::Kind::Dup,
    qa::Kind::Blur,
    qa::Kind::LumaOut,
];

/// 体检条目的颜色：确定的坏（读不出、全黑、重复、缺帧）用红，需要人眼确认的
/// 嫌疑（虚焦、亮度离群）用黄——把嫌疑染成红，整张表就没人信了。
fn qa_color(kind: qa::Kind) -> Color32 {
    match kind {
        qa::Kind::Read | qa::Kind::Black | qa::Kind::Dup | qa::Kind::Gap => danger(),
        qa::Kind::Blur | qa::Kind::LumaOut => warn(),
    }
}

fn adjust_sequence(state: &mut AppState, ui: &mut Ui) {
    if group_header(ui, state, "seq", "序列", false) {
        let n = state.frame_count();
        let valid = state.metrics.iter().filter(|m| m.is_some()).count();
        kv(
            ui,
            "帧数",
            format!("{n}"),
            if n > 0 { text() } else { dim() },
        );
        kv(
            ui,
            "测光完成",
            format!("{valid}"),
            if state.measuring.is_some() {
                accent()
            } else {
                text()
            },
        );
        let res = state
            .resolution
            .map(|(w, h)| format!("{w} × {h}"))
            .unwrap_or_else(|| "—".to_owned());
        kv(ui, "分辨率", res, text());
        kv(
            ui,
            "缺帧 / 异常",
            format!("{}", state.issues.len()),
            if state.issues.is_empty() {
                ok()
            } else {
                danger()
            },
        );
        if let Some(root) = state.seq.as_ref().map(|s| s.root.display().to_string()) {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(root)
                        .size(SIZE_SMALL)
                        .monospace()
                        .color(dim()),
                )
                .wrap()
                .selectable(false),
            );
        }
    }
    if group_header(ui, state, "seq-cull", "筛选台", false) {
        let n = state.frame_count();
        let cull_ctx = ui.ctx().clone();
        let suggest = bar_chip(ui, "按序列推建议区间", false)
            .on_hover_text(crate::i18n::t!(
                "用逐帧亮度的中位数绝对偏差推曝光区间（3×1.4826×MAD，下限 0.5 EV），渐变段落不会被误杀"
            ))
            .clicked();
        if suggest && n > 0 {
            let args = serde_json::json!({"suggest": true});
            if let Err(e) = state.run_ui_command(&cull_ctx, "cull.evaluate", &args) {
                state.cull_note = Some(e);
            }
        }
        // 阈值一改，上一遍的结论就不作数了：明说一句，别让旧读数骗人。
        //
        // Once a threshold moves, the previous verdict no longer holds; say so instead of
        // leaving a stale count on screen.
        let mut tuned = false;
        tuned |= slider(
            ui,
            "cull.ev_low",
            "曝光下限",
            &mut state.cull_ev_low,
            -6.0,
            0.0,
            -1.0,
            2,
            true,
        );
        tuned |= slider(
            ui,
            "cull.ev_high",
            "曝光上限",
            &mut state.cull_ev_high,
            0.0,
            6.0,
            1.0,
            2,
            true,
        );
        tuned |= slider(
            ui,
            "cull.jump_ev",
            "相邻突变",
            &mut state.cull_jump,
            0.0,
            6.0,
            1.0,
            2,
            false,
        );
        tuned |= slider(
            ui,
            "cull.sharp_ratio",
            "清晰度下限",
            &mut state.cull_sharp,
            0.0,
            1.0,
            0.0,
            2,
            false,
        );
        if tuned {
            state.cull_note = None;
        }
        ui.checkbox(
            &mut state.stamp_on,
            crate::i18n::t!("叠印时间戳（帧号 · 拍摄时间 · 机型曝光）"),
        );
        ui.horizontal(|ui| {
            let evaluate = bar_primary(ui, "评估")
                .on_hover_text(crate::i18n::t!(
                    "按当前阈值逐帧裁决，只报数不改标记；曝光下限/上限、突变、清晰度四条判据"
                ))
                .clicked();
            let apply = bar_button(ui, "标为排除")
                .on_hover_text(crate::i18n::t!(
                    "把判废的帧一次性标成排除（复用已有的旗标，时间轴上一眼看得见）"
                ))
                .clicked();
            if n == 0 {
                return;
            }
            let args = serde_json::json!({"apply": apply});
            if (evaluate || apply)
                && let Err(e) = state.run_ui_command(&cull_ctx, "cull.evaluate", &args)
            {
                state.cull_note = Some(e);
            }
        });
        ui.separator();
        // 剔除成子序列：目标目录 + 预估 / 导出。路径安全与体积预估都在命令层。
        //
        // Extract a subsequence: target folder plus estimate / export. Path safety and the size
        // estimate live in the command layer.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("子序列目录"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
            ui.add_enabled(
                !state.cull_target.is_empty() || n > 0,
                egui::TextEdit::singleline(&mut state.cull_target)
                    .hint_text(crate::i18n::t!("D:////精选////日落"))
                    .desired_width(220.0),
            );
        });
        ui.horizontal(|ui| {
            let estimate = bar_button(ui, "预估")
                .on_hover_text(crate::i18n::t!("只报会复制几帧、共多少字节，不写任何东西"))
                .clicked();
            let export = bar_primary(ui, "导出子序列")
                .on_hover_text(crate::i18n::t!(
                    "把没被拒收的帧复制成一条新序列：保留 mtime、测光缓存一并带走；开发参数属于源工程、不跟过去"
                ))
                .clicked();
            if (estimate || export)
                && let Err(e) = state.run_ui_command(
                    &cull_ctx,
                    "cull.extract",
                    &serde_json::json!({
                        "target": state.cull_target,
                        "dry_run": estimate,
                        "confirm": !estimate,
                    }),
                )
            {
                state.cull_note = Some(e);
            }
        });
        if let Some(note) = state.cull_note.clone() {
            ui.label(
                RichText::new(note)
                    .size(SIZE_SMALL)
                    .color(if state.cull_note.is_some() {
                        accent()
                    } else {
                        dim()
                    }),
            );
        }
    }
    if !state.issues.is_empty() && group_header(ui, state, "seq-issues", "异常帧", false) {
        let items: Vec<(usize, String)> = state.issues.iter().take(6).cloned().collect();
        for (i, msg) in items {
            ui.add(egui::Label::new(
                RichText::new(format!("{:04} · {}", i + 1, theme::ellipsis(&msg, 26)))
                    .size(SIZE_SMALL)
                    .color(danger()),
            ));
        }
        if state.issues.len() > 6 {
            ui.label(
                RichText::new(crate::i18n::tr_fmt(
                    "另有 {} 条…",
                    &[&(state.issues.len() - 6)],
                ))
                .size(SIZE_SMALL)
                .color(dim()),
            );
        }
    }
    if group_header(ui, state, "seq-qa", "序列体检", false) {
        let busy = state.qa_busy;
        let n = state.frame_count();
        if slider(
            ui,
            "qa.sig",
            "阈值松紧",
            &mut state.qa_sig,
            0.5,
            20.0,
            3.0,
            1,
            false,
        ) && state.qa_run
        {
            // 阈值一改，上一张清单就不是这个判据下的结果了；留着会让「没发现问题」骗人。
            state.qa_run = false;
        }
        ui.add_space(2.0);
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            // 忙碌时退成普通按钮：实心蓝是「现在就点」的语气，跑着的时候不该用它。
            if busy {
                ui.label(
                    RichText::new(crate::i18n::t!("正在逐帧查文件…"))
                        .size(SIZE_SMALL)
                        .color(muted()),
                );
                if bar_button(ui, "取消").clicked() {
                    state.stop_qa();
                }
            } else if n > 0 {
                if bar_primary(ui, "开始体检").clicked() {
                    state.run_qa(&ctx);
                }
            } else {
                bar_button(ui, "开始体检");
            }
        });
        if n == 0 && !busy {
            note(ui, "先打开素材。");
        }
        if let Some((done, total)) = state.qa_progress() {
            let frac = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_height(4.0)
                    .text(RichText::new(format!("{done} / {total}")).size(SIZE_SMALL)),
            );
        }
        if let Some(e) = &state.qa_error {
            ui.add(
                egui::Label::new(RichText::new(e).size(SIZE_SMALL).color(danger()))
                    .wrap()
                    .selectable(false),
            );
        }
        if state.qa_run && !busy {
            if state.qa_findings.is_empty() {
                kv(ui, "结论", "没发现问题", ok());
            } else {
                kv(
                    ui,
                    "结论",
                    crate::i18n::tr_fmt("{} 处", &[&state.qa_findings.len()]),
                    danger(),
                );
                let counts = qa::count_by_kind(&state.qa_findings);
                ui.horizontal_wrapped(|ui| {
                    for kind in QA_KINDS {
                        let c = counts.get(kind.id()).copied().unwrap_or(0);
                        if c == 0 {
                            continue;
                        }
                        ui.label(
                            RichText::new(format!("{} {}", kind.cn(), c))
                                .size(SIZE_SMALL)
                                .color(qa_color(kind)),
                        );
                    }
                });
                let items: Vec<Finding> = state.qa_findings.iter().take(60).cloned().collect();
                for f in items {
                    let resp = ui
                        .add(
                            egui::Label::new(
                                RichText::new(format!("{:04} · {}", f.frame + 1, f.kind.cn()))
                                    .size(SIZE_SMALL)
                                    .color(qa_color(f.kind)),
                            )
                            .selectable(false)
                            .sense(Sense::click()),
                        )
                        .on_hover_text(crate::i18n::tr_fmt(
                            "{}：{}",
                            &[&crate::i18n::tr(f.kind.cn()), &crate::i18n::tr(&f.note)],
                        ));
                    if resp.clicked() {
                        state.set_frame(f.frame);
                    }
                }
                if state.qa_findings.len() > 60 {
                    ui.label(
                        RichText::new(crate::i18n::tr_fmt(
                            "另有 {} 条…",
                            &[&(state.qa_findings.len() - 60)],
                        ))
                        .size(SIZE_SMALL)
                        .color(dim()),
                    );
                }
                note(ui, "点一行跳到那一帧。");
            }
        } else if !busy && n > 0 && state.qa_error.is_none() {
            note(ui, "按阈值检查缺帧与质量异常。");
        }
    }
    if group_header(ui, state, "seq-align", "序列对齐", true) {
        let n = state.frame_count();
        ui.horizontal_wrapped(|ui| {
            for (estimator, label) in [(Estimator::Template, "模板"), (Estimator::Phase, "相位")]
            {
                if bar_chip(ui, label, state.align_estimator == estimator).clicked() {
                    state.align_estimator = estimator;
                }
            }
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("参考帧"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            let mut r = state.align_ref.min(n.saturating_sub(1)) + 1;
            let resp = in_field(ui, field(), field_hover(), |ui| {
                ui.add_enabled(
                    n > 0,
                    egui::DragValue::new(&mut r).range(1..=n.max(1)).speed(0.2),
                )
            });
            if resp.changed() {
                state.align_ref = r.saturating_sub(1).min(n.saturating_sub(1));
            }
            ui.label(
                RichText::new(format!("/ {n}"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        });
        if ui
            .checkbox(
                &mut state.align_preview,
                RichText::new(crate::i18n::t!("画布显示对齐后")).size(SIZE_SMALL),
            )
            .changed()
        {
            state.preview_dirty = true;
        }
        if let Some((done, total)) = state.align_progress() {
            let frac = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_height(4.0)
                    .text(RichText::new(format!("{done} / {total}")).size(SIZE_SMALL)),
            );
        }
        let busy = state.align_busy;
        if ui
            .add_enabled(
                !busy && n > 1,
                egui::Button::new(
                    RichText::new(if busy {
                        crate::i18n::t!("正在对齐…")
                    } else {
                        crate::i18n::t!("开始对齐")
                    })
                    .size(SIZE_SMALL),
                ),
            )
            .on_disabled_hover_text(if n == 1 {
                crate::i18n::t!("单张照片没有要对齐的邻居")
            } else {
                crate::i18n::t!("先打开素材")
            })
            .clicked()
        {
            state.run_align(ui.ctx());
        }
        if let Some(msg) = &state.align_msg {
            ui.add(
                egui::Label::new(RichText::new(msg).size(SIZE_SMALL).color(
                    if state.align_report.is_some() {
                        ok()
                    } else {
                        danger()
                    },
                ))
                .wrap(),
            );
        }
    }
    if group_header(ui, state, "seq-align-report", "对齐质量", false) {
        if let Some(report) = &state.align_report {
            let crop = report.crop;
            kv(
                ui,
                "统一裁切",
                format!("{}×{} @ {},{}", crop.w, crop.h, crop.x, crop.y),
                text(),
            );
            let mut mx = 0.0f64;
            let mut my = 0.0f64;
            for o in &report.offsets {
                mx = mx.max(o[0].abs());
                my = my.max(o[1].abs());
            }
            kv(
                ui,
                "最大位移",
                format!("X {mx:.1} px · Y {my:.1} px"),
                text(),
            );
            let low = report.scores.iter().filter(|s| **s < 0.35).count();
            kv(
                ui,
                "低分帧",
                format!("{low}"),
                if low == 0 { ok() } else { danger() },
            );
            for issue in report.issues.iter().take(4) {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{:04} · {} · {}",
                            issue.frame + 1,
                            issue.kind,
                            theme::ellipsis(&issue.detail, 22)
                        ))
                        .size(SIZE_SMALL)
                        .color(danger()),
                    )
                    .wrap(),
                );
            }
            if report.issues.len() > 4 {
                ui.label(
                    RichText::new(crate::i18n::tr_fmt(
                        "另有 {} 条…",
                        &[&(report.issues.len() - 4)],
                    ))
                    .size(SIZE_SMALL)
                    .color(dim()),
                );
            }
        } else {
            note(ui, "尚未运行对齐。");
        }
    }
    if group_header(ui, state, "seq-spec", "口径", false) {
        kv(ui, "测光长边", "512 px", muted());
        kv(ui, "去闪输入", "全帧均值", muted());
        kv(ui, "预览长边", "1600 px", muted());
    }
}

fn adjust_deflicker(state: &mut AppState, ui: &mut Ui) {
    let mut changed = false;
    if group_header(ui, state, "deflicker", "去闪烁", true) {
        changed |= slider(
            ui,
            "def.window",
            "平滑窗口",
            &mut state.def_window,
            3.0,
            199.0,
            15.0,
            0,
            false,
        );
        changed |= slider(
            ui,
            "def.strength",
            "强度",
            &mut state.def_strength,
            0.0,
            150.0,
            100.0,
            0,
            false,
        );
        ui.add_space(2.0);
        if ui
            .checkbox(
                &mut state.def_multi,
                RichText::new(crate::i18n::t!("多遍窗口（长序列）")).size(SIZE_SMALL),
            )
            .changed()
        {
            state.recompute_deflicker();
        }
        if ui
            .checkbox(
                &mut state.def_channels,
                RichText::new(crate::i18n::t!("分通道（RGB 色闪）")).size(SIZE_SMALL),
            )
            .on_hover_text(crate::i18n::t!(
                "按 R/G/B 三通道分别测光与校正，治的是彩色闪烁；代价是可能带来轻微色偏漂移"
            ))
            .changed()
        {
            state.recompute_deflicker();
        }
        if ui
            .checkbox(
                &mut state.def_align_exif,
                RichText::new(crate::i18n::t!("按 EXIF 预对齐（圣杯）")).size(SIZE_SMALL),
            )
            .on_hover_text(crate::i18n::t!(
                "日转夜延时里相机一直在拉长曝光、开大光圈，去闪会把日夜过渡当成闪烁压掉；用 EXIF 的曝光三要素把相机补偿剥掉，只压真正的测光误差（只重分配每帧曝光，不改总亮度）"
            ))
            .changed()
        {
            state.recompute_deflicker();
        }
        if state.def_align_exif {
            let ev = state.exif_ev_frames();
            if ev == 0 {
                note(ui, "这批帧没有曝光三要素（ISO/快门/光圈），预对齐不会生效");
            } else {
                let total = state.metrics.len();
                note(
                    ui,
                    &crate::i18n::tr_fmt("{} / {} 帧有曝光三要素", &[&ev, &total]),
                );
            }
        }
        if bar_button(ui, "重新分析").clicked() {
            state.recompute_deflicker();
        }
        if changed {
            let snapped = ((state.def_window.round() as i64).clamp(3, 199)) | 1;
            state.def_window = snapped as f64;
            state.recompute_deflicker();
        }
    }
    if group_header(ui, state, "def-stats", "分析结果", false) {
        if let Some(a) = &state.analysis {
            let st = a.stats;
            kv(
                ui,
                "亮度抖动",
                format!("{:.3} → {:.3} EV", st.jitter_before, st.jitter_after),
                if st.jitter_after < st.jitter_before {
                    ok()
                } else {
                    text()
                },
            );
            if st.channels {
                kv(
                    ui,
                    "色闪抖动",
                    format!("{:.3} → {:.3} EV", st.color_before, st.color_after),
                    if st.color_after < st.color_before {
                        ok()
                    } else {
                        text()
                    },
                );
            }
            kv(ui, "最大修正", format!("{:+.2} EV", st.max_ev), text());
            kv(ui, "平均修正", format!("{:.3} EV", st.mean_abs_ev), text());
            kv(
                ui,
                "有效帧",
                format!("{} / {}", st.valid, st.frames),
                muted(),
            );
            let moved = state.corr.iter().filter(|v| v.abs() > 0.05).count();
            kv(ui, "受影响帧", format!("{moved}"), muted());
        } else {
            ui.label(
                RichText::new(crate::i18n::t!("等待测光…"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        }
    }
    if group_header(ui, state, "def-exposure", "曝光", false) {
        let p = &mut state.params;
        if slider(
            ui,
            "def.exp",
            "曝光",
            &mut p.exposure,
            -5.0,
            5.0,
            0.0,
            2,
            true,
        ) {
            state.preview_dirty = true;
        }
    }
}

/// 调色副本：同一序列的多套调色并存。套用取回当前，覆盖把当前写进去，
/// 导出 / 队列还能按名字指定用哪一套，互不覆盖。
/// 调色副本组：本体留在 adjust（它与参数面板的动作条同一套样式），
/// 由「预设与快照」浮窗的第三节调用。
///
/// The grade-copies group: the body stays in adjust (same styling as the parameter
/// panel action rows), called by the third section of the Presets & Snapshots window.
pub(crate) fn copies_group(state: &mut AppState, ui: &mut Ui) {
    let active = state.active_copy().map(str::to_owned);
    let mut apply: Option<String> = None;
    let mut overwrite: Option<String> = None;
    let mut delete: Option<String> = None;
    if state.copies.is_empty() {
        note(ui, "尚无副本");
    }
    for copy in &state.copies {
        let is_active = active.as_deref() == Some(copy.name.as_str());
        let name = copy.name.clone();
        let keys = copy.keyframes.len();
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::selectable(
                    is_active,
                    RichText::new(name.clone())
                        .size(SIZE_SMALL)
                        .color(if is_active { accent() } else { text() }),
                ))
                .on_hover_text(crate::i18n::t!("把这套调色与关键帧取回当前"))
                .clicked()
            {
                apply = Some(name.clone());
            }
            ui.label(
                RichText::new(crate::i18n::tr_fmt("关键帧 {}", &[&keys]))
                    .size(SIZE_CHROME_LABEL)
                    .color(dim()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if bar_button(ui, "删")
                    .on_hover_text(crate::i18n::t!("删掉这套副本，不动当前调色"))
                    .clicked()
                {
                    delete = Some(name.clone());
                }
                if bar_button(ui, "覆盖")
                    .on_hover_text(crate::i18n::t!("把当前调色与关键帧写进这套副本"))
                    .clicked()
                {
                    overwrite = Some(name.clone());
                }
            });
        });
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.copy_name)
                .hint_text(crate::i18n::t!("副本名"))
                .desired_width(108.0),
        );
        if bar_button(ui, "存当前")
            .on_hover_text(crate::i18n::t!(
                "把当前调色与关键帧存成一套新副本；同名要先点那行的「覆盖」"
            ))
            .clicked()
        {
            let name = state.copy_name.clone();
            if state.save_copy(&name, false).is_ok() {
                state.copy_name.clear();
            }
        }
    });
    if let Some(name) = apply {
        let _ = state.apply_copy(&name);
    }
    if let Some(name) = overwrite {
        let _ = state.save_copy(&name, true);
    }
    if let Some(name) = delete {
        let _ = state.delete_copy(&name);
    }
}

fn hdr_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let mut go = false;
    let busy = state.merge_kind == Some(MergeKind::Hdr);
    // 快照对账：参数真的变了才置落盘脏标记（见 stack_group 的说明）。
    let before_around = state.hdr_around;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("从当前帧起，前后各取"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        ui.add_enabled(
            !busy,
            egui::DragValue::new(&mut state.hdr_around)
                .range(1..=4)
                .speed(0.2),
        );
        ui.label(
            RichText::new(both_sides_caption(state.hdr_around))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        go = ui
            .add_enabled(
                !busy,
                egui::Button::new(
                    RichText::new(if busy { "合并中…" } else { "合并" }).size(SIZE_CHROME),
                )
                .min_size(Vec2::new(0.0, BAR_H))
                .corner_radius(radius_control()),
            )
            .on_hover_text(crate::i18n::t!(
                "把这一段包围曝光融成一张静帧，写进输出目录"
            ))
            .clicked();
    });
    if go {
        let args = serde_json::json!({"around": state.hdr_around, "confirm": true});
        if let Err(e) = state.run_ui_command(ctx, "hdr.merge", &args) {
            state.hdr_error = Some(e);
            state.hdr_last = None;
        }
    }
    if state.hdr_around != before_around {
        state.settings_dirty = true;
    }
    // 合并是后台任务：结果与失败原因由 `Msg::Merged` 写回 hdr_last / hdr_error。
    merge_progress_row(state, ui, MergeKind::Hdr);
    if let Some(last) = state.hdr_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    if let Some(err) = state.hdr_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
}

fn pano_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let mut go = false;
    let busy = state.merge_kind == Some(MergeKind::Pano);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("当前帧后再拼"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        ui.add_enabled(
            !busy,
            egui::DragValue::new(&mut state.pano_around)
                .range(1..=11)
                .speed(0.2),
        );
        ui.label(
            RichText::new(crate::i18n::t!("张"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        go = ui
            .add_enabled(
                !busy,
                egui::Button::new(
                    RichText::new(if busy {
                        crate::i18n::t!("拼接中…")
                    } else {
                        crate::i18n::t!("拼接")
                    })
                    .size(SIZE_CHROME),
                )
                .min_size(Vec2::new(0.0, BAR_H))
                .corner_radius(radius_control()),
            )
            .on_hover_text(crate::i18n::t!(
                "把当前帧与其后相邻几张接成一张全景，写进输出目录"
            ))
            .clicked();
    });
    if go {
        // pano_around 就是「当前帧之后再接几张」，与 pano.merge 的 around 同义。
        let args = serde_json::json!({"around": state.pano_around, "confirm": true});
        if let Err(e) = state.run_ui_command(ctx, "pano.merge", &args) {
            state.pano_error = Some(e);
            state.pano_last = None;
        }
    }
    merge_progress_row(state, ui, MergeKind::Pano);
    if let Some(last) = state.pano_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    if let Some(err) = state.pano_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
}

/// 「星空堆栈」分组：选统计量、调半径，一键合成一张。
///
/// Star-stacking group: pick the statistic, set the radius, combine into one still.
fn stack_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    use aevum_core::stack::Mode;
    let busy = state.merge_kind == Some(MergeKind::Stack);
    // 快照对账：合并参数是全局偏好（settings.json），本帧内真的变了才置落盘脏
    // 标记——帧尾的单一出口统一写文件，没变就不写，免得把 settings.json 当草稿纸。
    //
    // Snapshot reconciliation: the merge parameters are a global preference
    // (settings.json), and the dirty flag goes up only when something actually changed
    // within this frame -- the single frame-end exit writes the file, so an unchanged
    // frame never turns settings.json into scratch paper.
    let snapshot = (
        state.stack_around,
        state.stack_mode,
        state.stack_sigma,
        state.stack_fill,
    );
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("从当前帧起，前后各取"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        ui.add_enabled(
            !busy,
            egui::DragValue::new(&mut state.stack_around)
                .range(1..=32)
                .speed(0.2),
        );
        ui.label(
            RichText::new(both_sides_caption(state.stack_around))
                .size(SIZE_SMALL)
                .color(dim()),
        );
    });
    ui.horizontal_wrapped(|ui| {
        for mode in Mode::ALL {
            let picked = bar_chip(ui, mode.cn(), state.stack_mode == mode)
                .on_hover_text(crate::i18n::tr(match mode {
                    Mode::Lighten => "取最亮：星轨连成线，云与车流也连成线",
                    Mode::Average => "算术平均：随机噪声按 √N 下降，移动物变成半透明鬼影",
                    Mode::Median => "逐通道中值：卫星、飞机、单帧热噪点被整帧丢掉",
                    Mode::SigmaClip => "σ 截尾平均：先剔离群再平均，杂散光与热噪点都压得住",
                }))
                .clicked();
            if picked && !busy {
                state.stack_mode = mode;
            }
        }
    });
    if state.stack_mode == Mode::SigmaClip {
        let mut k = state.stack_sigma as f64;
        if slider(
            ui,
            "stack_clip_sigma",
            "截尾 k·σ",
            &mut k,
            1.0,
            8.0,
            3.0,
            1,
            false,
        ) {
            state.stack_sigma = k as f32;
        }
    }
    ui.checkbox(
        &mut state.stack_fill,
        crate::i18n::t!("补洞：把合成后仍全黑的像素用邻域补上"),
    );
    let go = ui
        .add_enabled(
            !busy,
            egui::Button::new(RichText::new(if busy {
                crate::i18n::t!("堆栈中…")
            } else {
                crate::i18n::t!("堆栈")
            }))
            .min_size(Vec2::new(0.0, BAR_H))
            .corner_radius(radius_control()),
        )
        .on_hover_text(crate::i18n::t!(
            "把当前帧前后若干帧按所选统计量合成一张，写进输出目录"
        ))
        .clicked();
    if go {
        let args = serde_json::json!({
            "around": state.stack_around,
            "mode": state.stack_mode.key(),
            "clip_sigma": state.stack_sigma,
            "fill_gaps": state.stack_fill,
            "confirm": true,
        });
        if let Err(e) = state.run_ui_command(ctx, "stack.merge", &args) {
            state.stack_error = Some(e);
            state.stack_last = None;
        }
    }
    merge_progress_row(state, ui, MergeKind::Stack);
    if let Some(last) = state.stack_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    if let Some(err) = state.stack_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
    if snapshot
        != (
            state.stack_around,
            state.stack_mode,
            state.stack_sigma,
            state.stack_fill,
        )
    {
        state.settings_dirty = true;
    }
}

/// 取帧行里「这个数到底代表几帧」的括号说明：前后各取。
///
/// The caption that says how many frames a span figure really means: both sides.
fn both_sides_caption(around: u32) -> String {
    // 前后各 N 帧 = 当前帧 + 两侧各 N，共 2N+1。写清楚，省得用户按 N 去算。
    crate::i18n::tr_fmt("帧（共 {} 帧）", &[&(around * 2 + 1)])
}

/// 取帧行里「这个数到底代表几帧」的括号说明：只往后取，带硬上限。
///
/// The caption that says how many frames a span figure really means: forward only, with
/// the hard cap.
fn forward_caption(around: u32, cap: usize) -> String {
    // 只往后取 N 帧 = 当前帧 + 之后 N 帧，共 N+1；超过 cap 就得分段。
    crate::i18n::tr_fmt("帧（共 {} 帧，上限 {}）", &[&(around + 1), &cap])
}

/// 焦点组的「先对齐」提示：序列开着、还没对齐、且这一组帧够长时才值得提醒。
///
/// 手持拍的对焦包围常有几像素位移：不对齐直接融，过渡带上会出现重影，而重影看起来
/// 恰恰像「深度图选错了帧」——先提一句，省得用户去拧 radius 和 threshold。
///
/// The focus group's "align first" hint: only when a sequence is open, it has not been
/// aligned yet, and the bracket is long enough for the drift to matter.
///
/// A handheld focus bracket usually drifts a few pixels. Fusing without aligning leaves
/// ghosting across the transition band, and ghosting looks exactly like "the depth map
/// picked the wrong frame" -- say it here instead of letting the user chase `radius` and
/// `threshold`.
fn focus_align_hint(state: &AppState) -> Option<String> {
    if state.seq.is_none() || state.align_report.is_some() {
        return None;
    }
    // 2 帧谈不上过渡带：不对齐的代价要 3 帧起才看得出来。
    if state.focus_around < 2 {
        return None;
    }
    // 文案在这里就翻好：调用点只负责上屏，别再各自包一层。
    //
    // The text is translated right here; call sites only put it on screen.
    Some(crate::i18n::tr_fmt(
        "这组帧还没对齐：手持拍的焦包围常有几像素位移，直接融会在过渡带留下重影。先跑一次对齐再合成。",
        &[],
    ))
}

/// 自动对齐的三态状态行：`(文案, 颜色)`，没有可交代的就返回 `None`。
///
/// 数据源是最近一次合并的结果 JSON（`kind == "focus"` 时才看）：增益与状态都是当时
/// 真实算出来的，面板只做格式转换——**不重算、不推断、不美化**。`"off"` 与字段缺失
/// 都不出行，因为「没跑」不值得占一行。
///
/// The three-state status line of auto alignment: `(text, colour)`, or `None` when there is
/// nothing to report.
///
/// The data comes from the most recent merge result JSON (only read when `kind == "focus"`):
/// both the status and the gain were computed for real back then, and the panel only formats
/// them -- it never recomputes, infers or prettifies. `"off"` and a missing field both emit
/// no row, because "it did not run" does not deserve a line.
fn focus_align_status_row(state: &AppState) -> Option<(String, egui::Color32)> {
    let result = state.merge_result.as_ref()?;
    if result.get("kind").and_then(|v| v.as_str()) != Some("focus") {
        return None;
    }
    let status = result.get("align_status").and_then(|v| v.as_str())?;
    let gain = result.get("align_gain").and_then(|v| v.as_f64());
    let percent = |g: f64| format!("{:+.1}%", g * 100.0);
    match status {
        "adopted" => Some((
            crate::i18n::tr_fmt(
                "自动对齐：已采用（增益 {}）",
                &[&gain.map(percent).unwrap_or_else(|| "—".to_owned())],
            ),
            accent(),
        )),
        "reverted_insufficient_gain" => Some((
            crate::i18n::tr_fmt(
                "自动对齐：收益不足已回退（{}），按未对齐几何出图",
                &[&gain.map(percent).unwrap_or_else(|| "—".to_owned())],
            ),
            dim(),
        )),
        "failed" => Some((
            crate::i18n::t!(
                "自动对齐：估计失败，已按未对齐几何出图；建议用三脚架重拍，或关闭自动对齐后重试"
            )
            .to_owned(),
            danger(),
        )),
        _ => None,
    }
}

/// 焦点合成：一组不同对焦距离的帧融成一张全景深。
///
/// Focus stacking: fuse a bracket taken at different focus distances into one all-in-focus
/// image.
fn focus_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    use aevum_core::focus::Method;
    let busy = state.merge_kind == Some(MergeKind::Focus);
    // 快照对账：本帧内任何参数或取帧变了，旧深度图预览就地作废——宁可让用户
    // 再点一次，也不挂一张与当前参数对不上的图误导选区。
    //
    // Snapshot reconciliation: if any parameter or the frame selection changed within
    // this frame, the old depth-map preview is voided on the spot -- asking the user
    // to press again beats hanging a picture that disagrees with the current
    // parameters and misleads the selection.
    let focus_snapshot = (
        state.focus_around,
        state.focus_method,
        state.focus_radius,
        state.focus_threshold,
        state.focus_smooth_z,
        state.focus_feather,
        // 自动对齐也是「影响成片」的参数：改了就要置落盘脏标记、并作废旧深度图预览
        // （预览走的是未对齐几何，勾上对齐后它就不再代表成片了）。
        //
        // Auto alignment also changes the delivered image: flipping it must mark the
        // settings dirty and void the stale depth-map preview (that preview follows the
        // unaligned geometry and no longer represents the still once alignment is on).
        state.focus_align,
    );
    // 没对齐就先说一句，并就地给个按钮：让用户自己去别处找「开始对齐」，这句提示
    // 就只是抱怨。
    //
    // Say it up front, with the button right here: a hint that only complains would send
    // the user hunting for the align control elsewhere in the panel.
    if let Some(hint) = focus_align_hint(state) {
        ui.add(egui::Label::new(RichText::new(hint).size(SIZE_SMALL).color(accent())).wrap());
        let aligning = state.align_busy;
        if ui
            .add_enabled(
                !aligning && !busy,
                egui::Button::new(
                    RichText::new(if aligning {
                        crate::i18n::t!("正在对齐…")
                    } else {
                        crate::i18n::t!("先对齐序列")
                    })
                    .size(SIZE_SMALL),
                ),
            )
            .on_hover_text(crate::i18n::t!(
                "对齐估出每帧位移与统一裁切，焦点合成会自动套上；不对齐直接融，过渡带会出重影"
            ))
            .clicked()
        {
            state.run_align(ui.ctx());
        }
    }
    ui.horizontal_wrapped(|ui| {
        for method in Method::ALL {
            let picked = bar_chip(ui, method.cn(), state.focus_method == method)
                .on_hover_text(crate::i18n::tr(match method {
                    Method::DepthMap => {
                        "逐像素取测度最强的帧；测度低于阈值的平坦区整块用参考帧兜底，过渡带羽化——保色彩、不出斑块"
                    }
                    Method::Weighted => {
                        "测度四次方当权重直接加权；最简单，低噪栈表现好，强纹理处可能轻微发糊"
                    }
                    Method::Pyramid => {
                        "拉普拉斯塔按层融合：细层取清晰细节，最粗两层等权当底图；多尺度自带过渡，羽化在金字塔法下不适用"
                    }
                }))
                .clicked();
            if picked && !busy {
                // 切到金字塔法时深度图预览已无意义（它不产生深度图）：就地清掉纹理与
                // 说明，而不是等本帧末的快照比对——那会让旧图在同一帧里闪一下就消失。
                //
                // Switching to the pyramid method makes the depth-map preview meaningless
                // (it produces no depth map): clear the texture and caption right here
                // instead of waiting for the end-of-frame snapshot comparison, which would
                // flash the stale image for the rest of this frame.
                if method == Method::Pyramid && state.focus_method != Method::Pyramid {
                    state.focus_depth_tex = None;
                    state.focus_depth_note = None;
                }
                state.focus_method = method;
            }
        }
    });
    // 合成前自动对齐（默认关）：breathing 补偿是「多一步、贵一点、可能回退」的事，
    // 不能替用户默认打开；勾上才做，且做没做、收益多少都会在状态行如实交代。
    //
    // Pre-fusion auto alignment (off by default): breathing compensation costs an extra
    // pass and may well revert, so it is never turned on behind the user's back. Whether it
    // ran and how much it gained is reported honestly below.
    ui.add_enabled(
        !busy,
        egui::Checkbox::new(&mut state.focus_align, crate::i18n::t!("合成前自动对齐")),
    )
    .on_hover_text(crate::i18n::t!(
        "先估每帧的 8×6 网格位移，再逐块补偿：对焦时的呼吸式轻微缩放也能压住。收益不足 0.5% 会自动回退成不做（成片与关闭时逐点一致）；估计失败照样出图，只是按未对齐几何。默认关闭"
    ));
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("从当前帧起，向后多取"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        ui.add_enabled(
            !busy,
            egui::DragValue::new(&mut state.focus_around)
                .range(1..=31)
                .speed(0.2),
        );
        ui.label(
            RichText::new(forward_caption(
                state.focus_around,
                aevum_core::focus::MAX_FRAMES,
            ))
            .size(SIZE_SMALL)
            .color(dim()),
        );
    });
    // 测度半径与无细节阈值是深度图法的两根主梁，各挂一句口径说明。
    //
    // The measure radius and the featureless threshold are the two girders of the depth-map
    // method, each with its stated convention.
    let mut radius = state.focus_radius as f64;
    if slider_row(
        ui,
        "focus_radius",
        "测度半径",
        &mut radius,
        0.0,
        32.0,
        2.0,
        0,
        false,
        "px",
        None,
        false,
        "焦点测度的分析窗口半径：越大越不受噪点干扰，越小越能分辨紧挨着的前后景；0 = 逐像素原始响应",
    ) {
        state.focus_radius = radius as u32;
    }
    let mut threshold = state.focus_threshold as f64;
    if slider_row(
        ui,
        "focus_threshold",
        "无细节阈值",
        &mut threshold,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "%",
        None,
        false,
        "全图最佳测度的分位数：低于这条线的像素整块判给参考帧（序列中位帧）。表盘、天空、抛光面这类平坦区就靠它不出斑块；0 = 关闭兜底",
    ) {
        state.focus_threshold = threshold as f32;
    }
    if state.focus_method == Method::DepthMap {
        let mut smooth = state.focus_smooth_z as f64;
        if slider_row(
            ui,
            "focus_smooth_z",
            "深度去斑",
            &mut smooth,
            0.0,
            16.0,
            1.0,
            0,
            false,
            "px",
            None,
            false,
            "深度图的模值滤波半径：把孤立的深度点并进周围多数派，免得单点噪点切出一小块另一帧",
        ) {
            state.focus_smooth_z = smooth as u32;
        }
        let mut feather = state.focus_feather as f64;
        if slider_row(
            ui,
            "focus_feather",
            "过渡羽化",
            &mut feather,
            0.0,
            64.0,
            2.0,
            1,
            false,
            "px",
            None,
            false,
            "深度跳变处两张素材按高斯权重交叠的宽度；小于 0.5 px 视为硬切",
        ) {
            state.focus_feather = feather as f32;
        }
    } else if state.focus_method == Method::Pyramid {
        // 切到金字塔法时这两根滑杆会整块消失：不给一句交代，用户会以为功能丢了
        // （PRD PYR-2 要求「显式说明不适用」，不许静默）。
        //
        // Switching to the pyramid method makes both sliders disappear wholesale; leaving it
        // unsaid reads as "the feature vanished" (PRD PYR-2 demands an explicit "not
        // applicable" statement, never silence).
        ui.add(
            egui::Label::new(
                RichText::new(crate::i18n::t!(
                    "深度去斑与过渡羽化：金字塔法不适用（多尺度融合自带过渡）"
                ))
                .size(SIZE_SMALL)
                .color(dim()),
            )
            .wrap(),
        );
    }
    ui.horizontal(|ui| {
        // 预览按钮在合成按钮左边：一个只看不写（前台算降采样归属图），
        // 一个写文件（后台跑全分辨率）。
        //
        // The preview button sits left of the merge button: one looks without writing
        // (a downsampled ownership map computed in the foreground), one writes files
        // (full resolution on the background thread).
        // 金字塔法不产生深度图：按钮置灰，而不是让用户点完发现没反应（PRD PYR-2 的
        // 「禁止伪预览」）。
        //
        // The pyramid method produces no depth map: disable the button instead of letting the
        // user click and see nothing (PRD PYR-2's "no fake preview").
        let pyramid = state.focus_method == Method::Pyramid;
        let can_preview = !busy && state.seq.is_some() && !pyramid;
        if ui
            .add_enabled(
                can_preview,
                egui::Button::new(RichText::new(crate::i18n::t!("预览深度图")).size(SIZE_SMALL))
                    .min_size(Vec2::new(0.0, BAR_H))
                    .corner_radius(radius_control()),
            )
            .on_hover_text(crate::i18n::t!(
                "按当前取帧与滑杆参数算一张降采样的深度归属图：每帧一种颜色，参考帧是灰色；只算预览，不写文件"
            ))
            .clicked()
        {
            state.focus_depth_preview(ctx);
        }
        // 置灰的原因必须写在按钮旁边，不能只靠「按不下去」让人猜。
        //
        // The reason for the greyed-out state belongs right next to the button; "it does not
        // respond" is not an explanation.
        if pyramid {
            ui.label(
                RichText::new(crate::i18n::t!("金字塔法不产生深度图"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        }
        let go = ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new(if busy {
                    crate::i18n::t!("合成中…")
                } else {
                    crate::i18n::t!("开始合成")
                }))
                .min_size(Vec2::new(0.0, BAR_H))
                .corner_radius(radius_control()),
            )
            .on_hover_text(crate::i18n::t!(
                "把当前帧起若干帧按所选方法融成一张全景深，写进输出目录"
            ))
            .clicked();
        if go {
            let args = serde_json::json!({
                "around": state.focus_around,
                "method": state.focus_method.key(),
                "radius": state.focus_radius,
                "threshold": state.focus_threshold,
                "smooth_z": state.focus_smooth_z,
                "feather": state.focus_feather,
                // 面板把勾选状态**显式**传下去：命令面的缺省值仍是 false（X-C1 红线，
                // 脚本面必须确定），不靠「缺省即面板意图」这种隐式约定。
                //
                // The panel passes the checkbox state **explicitly**: the command's default
                // stays false (X-C1's red line -- scripts need determinism), so this never
                // leans on an implicit "the default is whatever the panel wants".
                "align": state.focus_align,
                "confirm": true,
            });
            if let Err(e) = state.run_ui_command(ctx, "focus.merge", &args) {
                state.focus_error = Some(e);
                state.focus_last = None;
            }
        }
    });
    merge_progress_row(state, ui, MergeKind::Focus);
    if let Some(last) = state.focus_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    // 自动对齐的三态如实上屏：面板**不重算**增益、也不把失败显示成成功。
    //
    // The three states of auto alignment go on screen as they are: the panel never recomputes
    // the gain and never dresses a failure up as success.
    if let Some((text, colour)) = focus_align_status_row(state) {
        ui.add(egui::Label::new(RichText::new(text).size(SIZE_SMALL).color(colour)).wrap());
    }
    if let Some(err) = state.focus_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
    // 深度图预览：按可用宽度缩放、保持纵横比（与时间合成预览同一条路）；
    // 说明行挂尺寸 / 耗时 / 参考帧，失败时挂原因。金字塔法下不画（它不产生深度图，
    // 残留纹理只会误导），这是「禁止伪预览」的最后一道兜底。
    //
    // Depth-map preview: scaled to the available width, aspect preserved (the same path as
    // the time-blend preview); the caption carries size / elapsed / reference frame, or the
    // failure reason. Nothing is drawn under the pyramid method (it produces no depth map,
    // and a leftover texture only misleads) -- the last guard of "no fake preview".
    if state.focus_method != Method::Pyramid
        && let Some(tex) = state.focus_depth_tex.as_ref()
    {
        let size = tex.size_vec2();
        let avail = ui.available_width();
        let scale = if size.x > 0.0 && avail > 0.0 {
            (avail / size.x).min(1.0)
        } else {
            1.0
        };
        ui.add(egui::Image::new((tex.id(), size * scale)));
    }
    if let Some(note) = state.focus_depth_note.clone() {
        ui.label(RichText::new(note).size(SIZE_SMALL).color(dim()));
    }
    // 参数或取帧动了 → 旧预览作废（见函数头的快照对账）；合并参数是全局偏好，
    // 真的变了就置落盘脏标记（与 stack_group 同一口径）。
    if focus_snapshot
        != (
            state.focus_around,
            state.focus_method,
            state.focus_radius,
            state.focus_threshold,
            state.focus_smooth_z,
            state.focus_feather,
            state.focus_align,
        )
    {
        state.focus_depth_tex = None;
        state.focus_depth_note = None;
        state.settings_dirty = true;
    }
}

/// 「时间合成」分组：天地合成或时间切片，一次把不同时刻铺进一张。
///
/// Time-blending group: sky-ground or time slice, laying several moments into one frame.
fn timeblend_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    use aevum_core::timeblend::Layout;
    let busy = state.merge_kind == Some(MergeKind::TimeBlend);
    // 快照对账：合并参数是全局偏好，本帧内真的变了才置落盘脏标记（stack_group 有口径说明）。
    let snapshot = (
        state.tb_around,
        state.tb_layout,
        state.tb_bands,
        state.tb_feather,
        state.tb_horizon,
        state.tb_horizon_auto,
        state.tb_deghost,
        state.tb_match,
    );
    ui.horizontal_wrapped(|ui| {
        for layout in Layout::ALL {
            let picked = bar_chip(ui, layout.cn(), state.tb_layout == layout)
                .on_hover_text(crate::i18n::tr(match layout {
                    Layout::SkyGround => {
                        "按地平线分天地：天取最后一帧（日落后），地取第一帧（地面还亮着）"
                    }
                    Layout::Bands => "横向分带：一带取一帧，从早到晚铺满整幅，云被拉成一段时间",
                }))
                .clicked();
            if picked && !busy {
                state.tb_layout = layout;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("从当前帧起，向后多取"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        ui.add_enabled(
            !busy,
            egui::DragValue::new(&mut state.tb_around)
                .range(1..=31)
                .speed(0.2),
        );
        ui.label(
            RichText::new(forward_caption(
                state.tb_around,
                aevum_core::timeblend::MAX_FRAMES,
            ))
            .size(SIZE_SMALL)
            .color(dim()),
        );
    });
    if state.tb_layout == Layout::Bands {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("带数"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
            ui.add_enabled(
                !busy,
                egui::DragValue::new(&mut state.tb_bands)
                    .range(1..=32)
                    .speed(0.2),
            );
        });
    } else {
        ui.checkbox(
            &mut state.tb_horizon_auto,
            crate::i18n::t!("自动地平线：按行亮度梯度找天与地的分界"),
        )
        .on_hover_text(crate::i18n::t!(
            "把所有帧逐行平均成亮度剖面，找最强的纵向台阶；找不到（阴天平光、纯天空）退回手动值"
        ));
        if !state.tb_horizon_auto {
            let mut h = state.tb_horizon as f64;
            if slider(ui, "tb_horizon", "地平线", &mut h, 0.0, 1.0, 0.5, 2, false) {
                state.tb_horizon = h as f32;
            }
        }
    }
    // 去鬼影走 slider_row：提示语挂在滑杆上（tip 参数），0 = 关闭。
    //
    // Deghosting goes through slider_row so the tip rides on the slider itself; 0 = off.
    let mut dg = state.tb_deghost as f64;
    if slider_row(
        ui,
        "tb_deghost",
        "去鬼影",
        &mut dg,
        0.0,
        2.0,
        0.5,
        2,
        false,
        "",
        None,
        false,
        "0 = 关闭。羽化区里云、树、车流这类移动物会拖成两份；某帧某块与自己邻域的偏离超过这个值（EV）就被剔掉，换干净帧顶上",
    ) {
        state.tb_deghost = dg as f32;
    }
    let mut f = state.tb_feather as f64;
    if slider(ui, "tb_feather", "羽化", &mut f, 0.0, 0.5, 0.04, 3, false) {
        state.tb_feather = f as f32;
    }
    ui.checkbox(
        &mut state.tb_match,
        crate::i18n::t!("曝光匹配：先把来源帧亮度对齐到参考帧，接缝才没有台阶"),
    );
    let go = ui
        .add_enabled(
            !busy,
            egui::Button::new(RichText::new(if busy {
                crate::i18n::t!("合成中…")
            } else {
                crate::i18n::t!("开始合成")
            }))
            .min_size(Vec2::new(0.0, BAR_H))
            .corner_radius(radius_control()),
        )
        .on_hover_text(crate::i18n::t!(
            "把当前帧起若干帧按所选布局合成为一张，写进输出目录"
        ))
        .clicked();
    if go {
        let args = serde_json::json!({
            "around": state.tb_around,
            "layout": state.tb_layout.key(),
            "bands": state.tb_bands,
            "feather": state.tb_feather,
            "horizon": state.tb_horizon,
            "horizon_auto": state.tb_horizon_auto,
            "deghost": state.tb_deghost,
            "match_exposure": state.tb_match,
            "confirm": true,
        });
        if let Err(e) = state.run_ui_command(ctx, "timeblend.merge", &args) {
            state.tb_error = Some(e);
            state.tb_last = None;
        }
    }
    merge_progress_row(state, ui, MergeKind::TimeBlend);
    if let Some(last) = state.tb_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    if let Some(err) = state.tb_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
    if snapshot
        != (
            state.tb_around,
            state.tb_layout,
            state.tb_bands,
            state.tb_feather,
            state.tb_horizon,
            state.tb_horizon_auto,
            state.tb_deghost,
            state.tb_match,
        )
    {
        state.settings_dirty = true;
    }
    // 预览：参数或帧选择变了就自动在后台重算一张半分辨率小图，按可用宽度缩放展示。
    state.timeblend_preview_tick(ctx);
    if let Some((_, tex)) = &state.tb_preview {
        let size = tex.size_vec2();
        let avail = ui.available_width();
        let scale = if size.x > 0.0 && avail > 0.0 {
            (avail / size.x).min(1.0)
        } else {
            1.0
        };
        ui.add(egui::Image::new((tex.id(), size * scale)));
    }
    if state.tb_preview_inflight {
        ui.label(
            RichText::new(crate::i18n::t!("预览计算中…"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
    }
    if let Some(err) = state.tb_preview_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
}

/// 合并任务的进度条 + 取消按钮：只在这一类任务在跑时出现。
/// 「读取 k / n+1」的最后一格留给融合或拼接加写盘，那段没法细分进度。
fn merge_progress_row(state: &mut AppState, ui: &mut Ui, kind: MergeKind) {
    let Some((running, done, total)) = state.merge_progress() else {
        return;
    };
    if running != kind {
        return;
    }
    let frac = if total > 0 {
        done as f32 / total as f32
    } else {
        0.0
    };
    ui.add(
        egui::ProgressBar::new(frac)
            .desired_height(4.0)
            .text(RichText::new(format!("{done} / {total}")).size(SIZE_SMALL)),
    );
    if bar_button(ui, "取消合并").clicked() {
        state.stop_merge();
    }
}

/// 合成模块：HDR / 全景 / 星空堆栈 / 时间合成 / 焦点合成。
///
/// 2026-11-04 从显影面板整体迁出（docs/界面布局-ACR化-2026-11-04.md §4）：
/// 「多帧进来、一张图出去」的文件级工具不该排在调色推子的尾巴上。函数体只是
/// 五个既有组的重新集合，逻辑零改动。
///
/// The Merge module: HDR / panorama / star-stack / time-blend / focus-stack. Moved
/// wholesale out of Develop on 2026-11-04 (plan §4): file-level "frames in, one image
/// out" tools do not belong at the tail of the grading sliders. This body merely
/// re-collects the five existing groups; zero logic changes.
fn adjust_merge(state: &mut AppState, ui: &mut Ui) {
    ui.add_space(4.0);
    let ctx = ui.ctx().clone();
    if group_header(ui, state, "hdr", "HDR 合并", false) {
        hdr_group(state, ui, &ctx);
    }
    if group_header(ui, state, "pano", "全景合并", false) {
        pano_group(state, ui, &ctx);
    }
    if group_header(ui, state, "stack", "星空堆栈", false) {
        stack_group(state, ui, &ctx);
    }
    if group_header(ui, state, "timeblend", "时间合成", false) {
        timeblend_group(state, ui, &ctx);
    }
    if group_header(ui, state, "focus", "焦点合成", false) {
        focus_group(state, ui, &ctx);
    }
}

/// 分区内的次级小标题（ACR 的 subsection heading）：小号、半粗、弱色，上方留一口气。
/// 与 `group_header` 不同，它不进折叠登记表——折叠始终以九分区组为单位，
/// 小标题只是给一组推子分段。文案由调用方先过 `t!`（宏只吃字面量）。
///
/// A subsection heading inside a section (ACR's style): small, semibold, muted, with a
/// breath above. Unlike `group_header` it is not registered for collapsing -- folding
/// always works on the nine sections; the heading merely bands a group of sliders.
/// Callers translate first with `t!` (the macro only takes literals).
fn sub_head(ui: &mut Ui, label: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(label).font(bold(SIZE_SMALL)).color(muted()));
}

/// 按字段画一条推子：清单里写了表里没有的字段，debug 构建当场炸出来，
/// 发布构建静默跳过（宁可少一根推子，也不要整块面板打不开）。
///
/// Draw one slider by field: a field listed in the UI but missing from the spec table
/// trips a debug assertion, while release builds skip it silently (a missing slider
/// beats a panel that cannot open).
fn spec_field(ui: &mut Ui, state: &mut AppState, field: Field) -> bool {
    match controls::spec_by_field(field) {
        Some(s) => spec_slider(ui, state, s),
        None => {
            debug_assert!(false, "field {field:?} has no spec");
            false
        }
    }
}

/// 显影面板：ACR 的九分区次序（基本 → 曲线 → 颜色混合器 → 颜色分级 → 细节 →
/// 光学 → 效果 → 外观 → 蒙版）。旧版的 21 组混排已并入这九个分区，参数与
/// 工程格式零改动，只动 UI 组织（docs/界面布局-ACR化-2026-11-04.md §3）。
///
/// The Develop panel in ACR's nine-section order (Basic, Curve, Color Mixer, Color
/// Grading, Detail, Optics, Effects, Look, Masks). The old 21-group mixed list folded
/// into these nine; parameters and the project format are untouched -- only the UI
/// organisation moved.
fn adjust_develop(state: &mut AppState, ui: &mut Ui) {
    ui.add_space(4.0);
    let mut auto = false;
    let mut reset = false;
    let mut import_xmp = false;
    let mut export_xmp = false;
    // 一次动作一条行：自动 / 复位在左，XMP 进出在右。预设名在顶栏已经有一份，
    // 这里不再重复占一行。
    // 面板里的动作条用小胶囊（26 高 / 11 号字），不用选项栏那种 28 高的
    // 大按钮：四个 14 号字的大块并排，会把 372px 的面板撑成一排砖头，
    // 抢掉下面真正要看的推子。
    ui.horizontal(|ui| {
        auto = bar_chip(ui, "自动曝光", false).clicked();
        reset = bar_chip(ui, "全部复位", false).clicked();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            export_xmp = bar_chip(ui, "导出 XMP", false)
                .on_hover_text(crate::i18n::t!("把当前调色参数写成 .xmp 预设"))
                .clicked();
            import_xmp = bar_chip(ui, "导入 XMP", false)
                .on_hover_text(crate::i18n::t!(
                    "读入 Lightroom / Camera Raw 预设，替换当前调色参数"
                ))
                .clicked();
        });
    });
    if auto {
        state.auto_exposure();
    }
    if reset {
        state.reset_params();
    }
    if import_xmp {
        import_xmp_dialog(state);
    }
    if export_xmp {
        export_xmp_dialog(state);
    }
    if let Some(note_text) = state.auto_note.clone() {
        ui.add(
            egui::Label::new(RichText::new(note_text).size(SIZE_SMALL).color(accent()))
                .wrap()
                .selectable(false),
        );
    }
    ui.add_space(6.0);

    let mut changed = false;

    // ── 基本 ────────────────────────────────────────────────────────────
    // ACR 的基本面板分三档：先定白平衡，再拉色调，最后是偏好（质感与色彩）。
    // 推子按字段清单显式排布，显示序与表内物理序解耦——规格表是「一份参数
    // 一行」的登记册，不是版面（docs/界面布局-ACR化-2026-11-04.md §3）。
    //
    // ACR's Basic panel runs in three bands: white balance, then tone, then presence.
    // Sliders come from explicit field lists, so display order is decoupled from the
    // table -- the table is the parameter registry, not the page layout.
    if group_header(ui, state, "basic", "基本", true) {
        sub_head(ui, crate::i18n::t!("白平衡"));
        for f in [Field::Temperature, Field::Tint] {
            changed |= spec_field(ui, state, f);
        }
        // 小标题用「影调」而不是「色调」：中文里 tone 与 tint 都叫「色调」，
        // 而 i18n 表以中文为键——同一个键只能有一条英文，区标题会被译成 Tint。
        //
        // The heading says 影调 (tonal range) rather than 色调: in Chinese both tone
        // and tint are called 色调, and the i18n table keys on Chinese, so one key can
        // only carry one English string -- the section title would read "Tint".
        sub_head(ui, crate::i18n::t!("影调"));
        for f in [
            Field::Exposure,
            Field::HlRolloff,
            Field::Contrast,
            Field::ContrastLinear,
            Field::Highlights,
            Field::Shadows,
            Field::Whites,
            Field::Blacks,
            Field::BlackSoft,
            Field::WhiteSoft,
        ] {
            changed |= spec_field(ui, state, f);
        }
        sub_head(ui, crate::i18n::t!("偏好"));
        for f in [
            Field::Texture,
            Field::Clarity,
            Field::Dehaze,
            Field::DehazeDark,
            Field::DehazeContrast,
            Field::Vibrance,
            Field::Saturation,
        ] {
            changed |= spec_field(ui, state, f);
        }
    }
    // 子区返回「参数动了」就顺手摘掉该组的旁路：推子路径已在 spec_slider 里
    // 自清，这里接的是曲线/色阶拖点、外观卸下这类非推子路径。
    //
    // A sub-section reporting "parameters moved" also lifts that group's bypass: slider
    // paths self-clear in spec_slider, this catches the non-slider ones -- curve and
    // levels point-dragging, dropping a LUT.
    for (group, hit) in [
        ("curve", adjust_curve(state, ui)),
        ("mixer", adjust_mixer(state, ui)),
        ("grade", adjust_grade(state, ui)),
        ("detail", adjust_detail(state, ui)),
        ("optics", adjust_optics(state, ui)),
        ("geometry", adjust_geometry(state, ui)),
        ("retouch", adjust_retouch(state, ui)),
        ("effects", adjust_effects(state, ui)),
        ("look", adjust_look(state, ui)),
    ] {
        changed |= hit;
        if hit {
            state.bypassed.remove(group);
        }
    }
    adjust_masks(state, ui);
    // 文件级工具沉到底部：第一屏先给基本/色彩，和 LR 的阅读顺序一致。
    // 2026-11-04 起 HDR/全景/堆栈/时间合成/焦点合成五组搬去「合成」模块
    // （docs/界面布局-ACR化-2026-11-04.md §4），调色副本搬去「预设与快照」
    // 浮窗——显影面板只剩调色本身。
    //
    // File-level tools used to sit at the tail so Basic/Colour came first, matching
    // LR's reading order. Since 2026-11-04 the five merge tools live in the Merge
    // module and grade copies in the Presets & Snapshots window (plan §4) -- Develop
    // holds grading and nothing else.
    if changed {
        state.preview_dirty = true;
        if state.preset != "自定义" {
            state.preset = "自定义".to_owned();
        }
    }
}

/// 外观：外挂一张 .cube 三维查表，套在主链之前；导出把当前调色烘成 .cube。
fn adjust_look(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "look", "外观（.cube）", true) {
        return false;
    }
    let mut import = false;
    let mut export = false;
    let mut drop = false;
    ui.horizontal(|ui| {
        import = bar_button(ui, "导入外观")
            .on_hover_text(crate::i18n::t!("读一张 .cube，先套在调色链之前"))
            .clicked();
        export = bar_button(ui, "导出外观")
            .on_hover_text(crate::i18n::t!(
                "把当前全部调色（含已挂外观）烘成一张 .cube"
            ))
            .clicked();
        drop = bar_button(ui, "卸下")
            .on_hover_text(crate::i18n::t!("取消外挂外观；滑杆数值不动"))
            .clicked();
    });
    let path = state.params.look_cube.clone();
    if path.is_empty() {
        kv(ui, "当前", "未挂外观", dim());
    } else {
        let name = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(path.as_str())
            .to_owned();
        kv(ui, "当前", theme::ellipsis(&name, 26), text());
    }
    if import {
        import_look_dialog(state, ui.ctx());
    }
    if export {
        export_look_dialog(state, ui.ctx());
    }
    if drop && !path.is_empty() {
        state.params.look_cube = String::new();
        return true;
    }
    false
}

/// 色阶：在直方图剪影上直接拖输入黑 / 灰 / 白与输出黑 / 白。
/// 曲线区的第三个模式片（ACR 的 Levels 同构）。
///
/// Levels: drag input black/gamma/white and output black/white right on the histogram
/// silhouette. The third mode chip of the Curve section (mirrors ACR's Levels).
fn levels_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let changed = widgets::levels_strip(
        ui,
        state.hist.as_ref(),
        &mut state.params.lv_black,
        &mut state.params.lv_gamma,
        &mut state.params.lv_white,
        &mut state.params.lv_out_black,
        &mut state.params.lv_out_white,
        &mut state.levels_grab,
        78.0,
    );
    let p = &state.params;
    kv(
        ui,
        "输入 · 输出",
        format!(
            "{:.0} / {:.2} / {:.0} · {:.0} / {:.0}",
            p.lv_black, p.lv_gamma, p.lv_white, p.lv_out_black, p.lv_out_white
        ),
        dim(),
    );
    changed
}

/// 参数曲线：四个亮度区的升降 + 三条分界，与 LR 的 Parametric 同义。
/// 曲线区的第一个模式片。
///
/// Parametric curve: lift/drop four tonal zones plus three split points, same idea as
/// Lightroom's Parametric. The first mode chip of the Curve section.
fn parametric_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let d = GradeParams::default();
    let mut changed = false;
    for f in [
        Field::ParShadows,
        Field::ParDarks,
        Field::ParLights,
        Field::ParHighlights,
    ] {
        changed |= spec_field(ui, state, f);
    }
    for (i, label) in [(1, "阴影分界"), (2, "中间分界"), (3, "高光分界")] {
        let key = match i {
            1 => &mut state.params.par_split1,
            2 => &mut state.params.par_split2,
            _ => &mut state.params.par_split3,
        };
        let default = match i {
            1 => d.par_split1,
            2 => d.par_split2,
            _ => d.par_split3,
        };
        changed |= slider(
            ui,
            ("dev.par-split", i),
            label,
            key,
            0.0,
            100.0,
            default,
            0,
            false,
        );
    }
    changed
}

/// 曲线：主曲线 + 三条分通道，点空白加控制点，双击删点或整条复位。
/// 曲线区：ACR 的 Curve 面板同构——区头下三枚模式片（参数 / 点 / 色阶）
/// 切换三种调曲线的方式，共用一条历史记账与一个复位口径。
///
/// The Curve section, mirroring ACR's Curve panel: three mode chips (parametric /
/// point / levels) under the header, sharing one history path and one reset scope.
fn adjust_curve(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "curve", "曲线", true) {
        return false;
    }
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::selectable(
                state.curve_tab == 0,
                crate::i18n::t!("参数曲线"),
            ))
            .on_hover_text(crate::i18n::t!(
                "四个亮度区 + 三条分界，Lightroom 的 Parametric"
            ))
            .clicked()
        {
            state.curve_tab = 0;
        }
        if ui
            .add(egui::Button::selectable(
                state.curve_tab == 1,
                crate::i18n::t!("点曲线"),
            ))
            .on_hover_text(crate::i18n::t!("手绘任意形状，分 RGB 主通道与三条单通道"))
            .clicked()
        {
            state.curve_tab = 1;
        }
        if ui
            .add(egui::Button::selectable(
                state.curve_tab == 2,
                crate::i18n::t!("色阶"),
            ))
            .on_hover_text(crate::i18n::t!("直方图上的输入 / 输出黑白场"))
            .clicked()
        {
            state.curve_tab = 2;
        }
    });
    ui.add_space(2.0);
    match state.curve_tab {
        0 => changed |= parametric_body(state, ui),
        1 => changed |= point_curve_body(state, ui),
        _ => changed |= levels_body(state, ui),
    }
    changed
}

/// 点曲线：主曲线 + 三条分通道，点空白加控制点，双击删点或整条复位。
/// 曲线区的第二个模式片。
///
/// Point curve: the RGB master plus three per-channel curves; click empty space to add
/// a point, double-click to delete it or reset the whole curve. The second chip.
fn point_curve_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let labels = ["RGB", "红", "绿", "蓝"];
    let mut moved = false;
    ui.horizontal(|ui| {
        for (i, label) in labels.iter().enumerate() {
            if ui
                .add(egui::Button::selectable(state.curve_chan == i, *label))
                .clicked()
            {
                state.curve_chan = i;
                state.curve_grab = None;
                moved = true;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(crate::i18n::t!("点空白加点 · 双击删点"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        });
    });
    let side = ui.available_width().clamp(150.0, 248.0);
    let chan = state.curve_chan;
    let pts = match chan {
        0 => &mut state.params.curve_rgb,
        1 => &mut state.params.curve_r,
        2 => &mut state.params.curve_g,
        _ => &mut state.params.curve_b,
    };
    let tint = match chan {
        0 => Color32::from_rgb(0xDE, 0xDE, 0xDF),
        1 => Color32::from_rgb(0xFF, 0x6B, 0x63),
        2 => Color32::from_rgb(0x5F, 0xD8, 0x7A),
        _ => Color32::from_rgb(0x64, 0xA8, 0xFF),
    };
    let changed = widgets::curve_box(
        ui,
        "tone",
        "输出 ↑ / 输入 →",
        tint,
        pts,
        side,
        &mut state.curve_grab,
        &mut state.curve_lut_cache,
    );
    // HDR 尾部：曲线 x>1 的延伸量（对齐 LR 的 LumaHDR），挂在点曲线下方。
    // 参数曲线与色阶没有 x>1 的概念，所以只在点曲线模式下出现。
    //
    // HDR tail: the extension of the curve past x>1 (LR's LumaHDR), shown under the
    // point curve -- parametric and levels have no notion of x>1.
    let spec_changed = spec_field(ui, state, Field::CurveHdrTail);
    moved || changed || spec_changed
}

/// 颜色混合器：ACR 的 Color Mixer 面板同构——区头下两枚模式片（HSL / 黑白）。
/// HSL 调每个色相带的色相/饱和度/明亮度，黑白调每个色相带转灰后的亮度。
///
/// Color Mixer, mirroring ACR: two mode chips (HSL / B&W) under the header. HSL tunes
/// hue/saturation/luminance per band; B&W tunes how bright each band turns in mono.
fn adjust_mixer(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "mixer", "颜色混合器", true) {
        return false;
    }
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::selectable(
                state.mixer_tab == 0,
                crate::i18n::t!("HSL"),
            ))
            .on_hover_text(crate::i18n::t!("八段色相各自的色相 / 饱和度 / 明亮度"))
            .clicked()
        {
            state.mixer_tab = 0;
        }
        if ui
            .add(egui::Button::selectable(
                state.mixer_tab == 1,
                crate::i18n::t!("黑白"),
            ))
            .on_hover_text(crate::i18n::t!("转黑白后每个色相带变成多亮"))
            .clicked()
        {
            state.mixer_tab = 1;
        }
    });
    ui.add_space(2.0);
    match state.mixer_tab {
        0 => changed |= hsl_body(state, ui),
        _ => changed |= mono_body(state, ui),
    }
    changed
}

/// HSL：8 个色相带各自的色相 / 饱和度 / 明亮度，一次调一条。
fn hsl_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let used: Vec<f64> = (0..8)
        .map(|i| {
            state.params.hsl_h[i].abs() + state.params.hsl_s[i].abs() + state.params.hsl_l[i].abs()
        })
        .collect();
    let mut changed = widgets::band_chips(ui, &mut state.hsl_band, &used);
    let band = state.hsl_band;
    let title = format!(
        "{} · {:.0}°",
        widgets::BAND_NAMES[band],
        widgets::BAND_HUES[band]
    );
    ui.label(RichText::new(title).size(SIZE_SMALL).color(muted()));
    changed |= slider(
        ui,
        ("hsl-h", band),
        "色相",
        &mut state.params.hsl_h[band],
        -100.0,
        100.0,
        0.0,
        0,
        true,
    );
    changed |= slider(
        ui,
        ("hsl-s", band),
        "饱和度",
        &mut state.params.hsl_s[band],
        -100.0,
        100.0,
        0.0,
        0,
        true,
    );
    changed |= slider(
        ui,
        ("hsl-l", band),
        "明亮度",
        &mut state.params.hsl_l[band],
        -100.0,
        100.0,
        0.0,
        0,
        true,
    );
    changed
}

/// 黑白混合：转黑白后，8 个色相带各自变成多亮。
fn mono_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let mut changed = false;
    changed |= spec_field(ui, state, Field::Mono);
    for i in 0..8 {
        changed |= slider(
            ui,
            ("mono-h", i),
            widgets::BAND_NAMES[i],
            &mut state.params.mono_h[i],
            -100.0,
            100.0,
            0.0,
            0,
            true,
        );
    }
    changed
}

/// 色彩分级：阴影 / 中间调 / 高光三枚色轮，加全局一轮与两段过渡控制。
fn adjust_grade(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "grade", "色彩分级", true) {
        return false;
    }
    let mut changed = false;
    // PS Camera Raw 的布局口径：三枚轮给足尺寸，轮与轮之间留一口气。
    // 之前 (w-12)/3 把三轮顶到贴脸，72px 的轮盘读不出色相环。
    let gap = 8.0;
    let size = ((ui.available_width() - gap * 2.0 - 4.0) / 3.0).clamp(84.0, 116.0);
    ui.horizontal(|ui| {
        changed |= widgets::grade_wheel(
            ui,
            "grade.sh",
            "阴影",
            &mut state.params.grade_sh_h,
            &mut state.params.grade_sh_s,
            &mut state.params.grade_sh_l,
            size,
        );
        ui.add_space(gap);
        changed |= widgets::grade_wheel(
            ui,
            "grade.mid",
            "中间调",
            &mut state.params.grade_mid_h,
            &mut state.params.grade_mid_s,
            &mut state.params.grade_mid_l,
            size,
        );
        ui.add_space(gap);
        changed |= widgets::grade_wheel(
            ui,
            "grade.hi",
            "高光",
            &mut state.params.grade_hi_h,
            &mut state.params.grade_hi_s,
            &mut state.params.grade_hi_l,
            size,
        );
    });
    ui.add_space(4.0);
    for s in controls::specs_of_group("grade") {
        changed |= spec_slider(ui, state, s);
    }
    changed
}

/// 细节：中频（清晰度 / 纹理）+ 锐化四件套 + 降噪五件套。
fn adjust_detail(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "detail", "细节", true) {
        return false;
    }
    let mut changed = false;
    for s in controls::specs_of_group("detail") {
        changed |= spec_slider(ui, state, s);
    }
    changed
}

/// 效果区：ACR 的 Effects 面板——暗角在外圈（裁剪后晕影），颗粒是胶片质感。
/// 两段各有小标题：九条推子混排在一起读不出层次。
///
/// Effects, mirroring ACR: post-crop vignette on the outer ring, grain as film texture.
/// Each band gets a subsection heading -- nine mixed sliders read as a wall otherwise.
fn adjust_effects(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "effects", "效果", true) {
        return false;
    }
    let mut changed = false;
    sub_head(ui, crate::i18n::t!("暗角"));
    for f in [
        Field::Vignette,
        Field::VigMidpoint,
        Field::VigRoundness,
        Field::VigFeather,
        Field::VigHlContrast,
    ] {
        changed |= spec_field(ui, state, f);
    }
    sub_head(ui, crate::i18n::t!("颗粒"));
    for f in [
        Field::Grain,
        Field::GrainLum,
        Field::GrainSize,
        Field::GrainFreq,
    ] {
        changed |= spec_field(ui, state, f);
    }
    changed
}

/// Adobe 配置：DCP 相机配置接管 RAW 解码后的颜色，LCP 镜头配置按 EXIF 选帧。
///
/// 两行都走 `profile.set`，和 MCP 同一条校验路径：机型/镜头对不上就在面板里直说。
/// 光学区的第二段（配置文件），不带组头——组头归 `adjust_optics`。
///
/// Both rows go through `profile.set`, the same validation path as MCP: a body/lens
/// mismatch is said straight in the panel. The second band of the Optics section; no
/// header of its own -- `adjust_optics` owns it.
fn profiles_body(state: &mut AppState, ctx: &Context, ui: &mut Ui) {
    let dcp_active = !state.params.dcp_profile.is_empty();
    let dcp_shown = if !state.dcp_name.is_empty() {
        state.dcp_name.clone()
    } else if dcp_active {
        std::path::Path::new(&state.params.dcp_profile)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "已套用".to_owned())
    } else {
        "未套用".to_owned()
    };
    ui.horizontal(|ui| {
        ui.label(crate::i18n::t!("相机配置"));
        ui.label(egui::RichText::new(dcp_shown).weak());
        if ui
            .button(crate::i18n::t!("自动"))
            .on_hover_text(crate::i18n::t!("按首帧 EXIF 机型在 DCP 目录里找配置（优先 Adobe Standard）。\nDCP 只接 RAW/DNG：相机原始数据才需要矩阵与曲线，\nJPEG/TIFF 已经是成品色彩。"))
            .clicked()
        {
            let args = serde_json::json!({"kind":"dcp","auto":true});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("匹配相机配置失败：{}", &[&e]));
            }
        }
        if ui.button(crate::i18n::t!("选文件")).clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("相机配置", &["dcp"])
                .pick_file()
        {
            let args = serde_json::json!({"kind":"dcp","path": path.display().to_string()});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("套用相机配置失败：{}", &[&e]));
            }
        }
        if dcp_active && ui.button(crate::i18n::t!("清除")).clicked() {
            let args = serde_json::json!({"kind":"dcp","clear":true});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("清除相机配置失败：{}", &[&e]));
            }
        }
    });
    if !state.dcp_note.is_empty() {
        ui.label(egui::RichText::new(&state.dcp_note).weak());
    }

    let lcp_shown = match state.params.lcp_profile.as_ref() {
        Some(c) => format!("{}（{} · {:.0}mm）", c.profile, c.lens, c.focal35_mm),
        None => "未套用".to_owned(),
    };
    ui.horizontal(|ui| {
        ui.label(crate::i18n::t!("镜头配置"));
        ui.label(egui::RichText::new(lcp_shown).weak());
        if ui
            .button(crate::i18n::t!("自动"))
            .on_hover_text(crate::i18n::t!("按首帧 EXIF 的机身/镜头/焦距在 LCP 目录里选帧，\n折出畸变、横向色差与暗角，先于下面的手动三项生效。"))
            .clicked()
        {
            let args = serde_json::json!({"kind":"lcp","auto":true});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("匹配镜头配置失败：{}", &[&e]));
            }
        }
        if ui.button(crate::i18n::t!("选文件")).clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("镜头配置", &["lcp"])
                .pick_file()
        {
            let args = serde_json::json!({"kind":"lcp","path": path.display().to_string()});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("套用镜头配置失败：{}", &[&e]));
            }
        }
        if state.params.lcp_profile.is_some() && ui.button(crate::i18n::t!("清除")).clicked() {
            let args = serde_json::json!({"kind":"lcp","clear":true});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("清除镜头配置失败：{}", &[&e]));
            }
        }
    });
    if !state.lcp_note.is_empty() {
        ui.label(egui::RichText::new(&state.lcp_note).weak());
    }
}

/// 光学区：ACR 的 Optics 面板——手动校正三项在前，相机/镜头配置文件在后
/// （配置文件折出的畸变/色差/暗角先于手动值生效，排在后面读起来是"标定打底"）。
///
/// 几何：透视校正。自动一路只修梯形（竖线收敛到一个画外消失点），不修旋转、
/// 不修枕形畸变——证据不足时宁可不动作，也不猜一个把画面撕歪的四边形。
///
/// Geometry: perspective correction. The automatic path fixes keystone only (vertical lines
/// converging to an off-frame vanishing point); it does not fix rotation or pincushion. With
/// insufficient evidence it does nothing rather than guess a quad that warps the frame.
fn adjust_geometry(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "geometry", "几何", false) {
        return false;
    }
    let mut auto = false;
    let mut clear = false;
    let mut corners = false;
    ui.horizontal(|ui| {
        auto = bar_button(ui, "自动直立")
            .on_hover_text(crate::i18n::t!(
                "找画面里的竖直主直线，把梯形拉正；只修上下透视，不修旋转与畸变"
            ))
            .clicked();
        // 「四角」是自动直立的兜底：自动解要求画面里真有一条像样的竖直主直线，室内
        // 广角、纯风景、对着墙拍的片子经常一条都没有。这时只能手动把四个角拖到画面的
        // 方角上——ACR 里这个入口叫 Transform 的角点手柄。开着时按钮变「完成」。
        //
        // "Corners" is the fallback for auto-upright: the automatic solve needs a decent
        // dominant vertical, and wide interiors, pure landscapes or anything shot against a
        // wall often have none. Then the four corners have to be dragged onto the square
        // object by hand -- the corner handles of Transform in ACR. While on, the chip reads
        // "Done".
        corners = bar_chip(
            ui,
            if state.upright_editing {
                "完成"
            } else {
                "四角"
            },
            state.upright_editing,
        )
        .on_hover_text(crate::i18n::t!(
            "拖四个角对齐画面里的方形物体；双击某个角放回原处"
        ))
        .clicked();
        clear = bar_button(ui, "清除")
            .on_hover_text(crate::i18n::t!("卸下透视校正，回到整幅画面"))
            .clicked();
    });
    if corners {
        let was = state.upright_editing;
        let now = state.upright_edit_toggle();
        if !was && !now {
            state.auto_note = Some(
                crate::i18n::t!("画布当前不是整幅（拉直 / 推近 / 裁剪中），四角抓不到；先复位")
                    .to_owned(),
            );
        }
    }
    if auto {
        state.auto_note = Some(match state.upright_auto() {
            Ok(()) => {
                crate::i18n::t!("已按竖直主直线拉正；四边形之外的一圈被裁掉了，这是校正的代价")
                    .to_owned()
            }
            Err(e) => e,
        });
    }
    let mut changed = false;
    if clear {
        changed |= state.upright_clear();
    }
    // 状态行报的是「上边比下边窄多少」：梯形校正的全部意义就在这个数上，比四个角点
    // 的坐标好读得多。单位方（没有动作）按「未校正」显示，不报 0.0%。
    //
    // The status line reports how much narrower the top edge is than the bottom: that ratio is
    // the whole point of keystone correction and reads far better than four corner coordinates.
    // The unit square (no action) shows as "not corrected" rather than 0.0%.
    match state.params.upright {
        Some(q) if !q.is_identity() => {
            let top = (q.p[1].0 - q.p[0].0).abs();
            let bottom = (q.p[2].0 - q.p[3].0).abs();
            let pct = if bottom > 1e-9 {
                (bottom - top) / bottom * 100.0
            } else {
                0.0
            };
            let label = if pct.abs() < 0.05 {
                crate::i18n::t!("已启用（四边形接近整幅）").to_owned()
            } else if pct > 0.0 {
                crate::i18n::tr_fmt("已拉正 · 上边比下边窄 {:.1}%", &[&pct])
            } else {
                crate::i18n::tr_fmt("已拉正 · 上边比下边宽 {:.1}%", &[&(-pct)])
            };
            kv(ui, "状态", label, text());
        }
        _ => kv(ui, "状态", crate::i18n::t!("未校正"), dim()),
    }
    if state.upright_editing {
        note(
            ui,
            crate::i18n::t!("画布上四个圆点就是四角：拖到方形物体的边上，双击某个角放回原处"),
        );
    }
    changed
}

/// 修复：画笔设定 + 已落笔触的撤销。
///
/// 落点在画布上（工具条选「修复」），不在这个组里——参数面板负责「笔有多粗」，
/// 画布负责「点在哪儿」，两者分开才不会做成一个「先输入坐标再回车」的表单。
///
/// Repair: brush settings plus undo for placed strokes.
///
/// Strokes land on the canvas (pick "Heal" in the tool bar), not in this group: the panel owns
/// "how big is the brush", the canvas owns "where it goes". Keeping them apart avoids turning
/// this into a form where you type coordinates and press enter.
fn adjust_retouch(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "retouch", "修复", false) {
        return false;
    }
    let mut changed = false;
    let mode = ui
        .horizontal(|ui| {
            let mut hit = false;
            if bar_chip(ui, "修复", !state.heal_clone).clicked() && state.heal_clone {
                state.heal_clone = false;
                state.heal_src = None;
                hit = true;
            }
            if bar_chip(ui, "仿制", state.heal_clone).clicked() && !state.heal_clone {
                state.heal_clone = true;
                state.heal_src = None;
                hit = true;
            }
            hit
        })
        .inner;
    changed |= mode;

    changed |= slider_row(
        ui,
        "heal-radius",
        "半径",
        &mut state.heal_radius,
        0.5,
        8.0,
        2.0,
        1,
        false,
        "%",
        None,
        true,
        "占画面长边的百分比：拖到 2% 大致是一颗灰尘，6% 以上是抹掉一根电线",
    );
    changed |= slider_row(
        ui,
        "heal-feather",
        "羽化",
        &mut state.heal_feather,
        0.0,
        100.0,
        35.0,
        0,
        false,
        "%",
        None,
        true,
        "边缘过渡占半径的比例：0 是硬边，100 从中心淡到边",
    );

    let n = state.params.retouch.len();
    kv(
        ui,
        "笔触",
        if n == 0 {
            crate::i18n::t!("还没有").to_owned()
        } else {
            crate::i18n::tr_fmt("{} 笔", &[&n])
        },
        if n == 0 { dim() } else { text() },
    );
    let mut undo = false;
    let mut clear = false;
    ui.horizontal(|ui| {
        undo = bar_chip(ui, "撤销一笔", false)
            .on_hover_text(crate::i18n::t!("去掉最后落下的那一笔"))
            .clicked();
        clear = bar_chip(ui, "全部清空", false)
            .on_hover_text(crate::i18n::t!("去掉这台相机这一帧上的全部修复笔触"))
            .clicked();
    });
    if undo {
        changed |= state.heal_undo();
    }
    if clear {
        changed |= state.heal_clear();
    }

    // 仿制模式下报一句「下一步是取源」：不然点了没反应，人会以为坏了。
    //
    // In clone mode, say "next tap picks the source": otherwise the first tap does nothing and
    // reads as broken.
    if state.heal_clone {
        kv(
            ui,
            "仿制源",
            match state.heal_src {
                Some(_) => crate::i18n::t!("已取；再点画面即落笔").to_owned(),
                None => crate::i18n::t!("在画面上点一下取源").to_owned(),
            },
            if state.heal_src.is_some() {
                accent()
            } else {
                dim()
            },
        );
    }
    if !state.heal_can_place() {
        note(
            ui,
            crate::i18n::t!("画布当前不是整幅（拉直 / 推近 / 裁剪中），先复位再落点"),
        );
    }
    changed
}

/// Optics, mirroring ACR: the three manual corrections first, then the camera/lens
/// profiles (what the profile derives is applied ahead of the manual sliders, so
/// reading it last says "the calibration is the base layer").
fn adjust_optics(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "optics", "光学", true) {
        return false;
    }
    let mut changed = false;
    sub_head(ui, crate::i18n::t!("镜头校正"));
    changed |= lens_body(state, ui);
    sub_head(ui, crate::i18n::t!("相机与镜头配置"));
    let ctx = ui.ctx().clone();
    profiles_body(state, &ctx, ui);
    changed
}

/// 镜头校正：畸变、暗角补偿、去紫边；三项都是空间操作，导出同样生效。
/// 顶上一行是机型匹配：EXIF 对 lensfun 数据库折出的标定系数先走一步。
/// 光学区的第一段，不带组头。
///
/// Lens correction: distortion, vignette compensation, defringe -- all spatial and
/// applied on export too. The top row is EXIF-based matching against the lensfun
/// database. The first band of the Optics section, without a header of its own.
fn lens_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let mut changed = false;
    let mut auto = state.params.lens_profile.is_some();
    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut auto, crate::i18n::t!("自动匹配"))
            .on_hover_text(crate::i18n::t!("按 EXIF 的机身/镜头/焦距在 lensfun 数据库里找标定，\n自动折出畸变、横向色差与暗角校正；\n找不到库或没命中时只剩手动三项。"))
            .changed()
        {
            if auto {
                if let Some(c) = state.lens_profile_saved.take() {
                    state.params.lens_profile = Some(c);
                } else {
                    state.match_lens_profile();
                }
            } else {
                state.lens_profile_saved = state.params.lens_profile.take();
            }
            state.preview_dirty = true;
            changed = true;
        }
        if ui
            .button(crate::i18n::t!("重配"))
            .on_hover_text(crate::i18n::t!("按当前 EXIF 重新跑一遍匹配"))
            .clicked()
        {
            state.match_lens_profile();
            state.preview_dirty = true;
            changed = true;
        }
    });
    if !state.lens_profile_note.is_empty() {
        ui.label(egui::RichText::new(&state.lens_profile_note).weak());
    }
    ui.add_space(2.0);
    for f in [Field::LensDistort, Field::LensVignette, Field::LensFringe] {
        changed |= spec_field(ui, state, f);
    }
    changed
}

fn adjust_frame(state: &mut AppState, ui: &mut Ui) {
    if group_header(ui, state, "frame", "画幅", true) {
        ui.horizontal_wrapped(|ui| {
            for a in Aspect::ALL {
                if bar_chip(ui, a.label(), state.aspect == a).clicked() {
                    state.set_aspect(a);
                }
            }
        });
        if state.resolution.is_some() {
            ui.add_space(4.0);
            // 推子只管取值，0 附近吸附和重画标记都交给 set_rotate。
            let mut deg = state.rotate;
            if slider(
                ui,
                "frame.rotate",
                "拉直",
                &mut deg,
                -45.0,
                45.0,
                0.0,
                1,
                true,
            ) {
                state.set_rotate(deg);
            }
        }
        ui.add_space(4.0);
        if state.resolution.is_some() {
            let sel = state.crop_box();
            kv(
                ui,
                "取景框",
                format!("{} × {} @ {}, {}", sel.w, sel.h, sel.x, sel.y),
                text(),
            );
            match state.aspect.ratio() {
                Some(r) => kv(ui, "画幅比例", format!("{r:.3}"), muted()),
                None => kv(ui, "画幅比例", "跟随原生", muted()),
            }
            ui.horizontal_wrapped(|ui| {
                if bar_chip(ui, "拖框裁剪", state.tool == Tool::Crop).clicked() {
                    state.set_tool(Tool::Crop);
                }
                if (state.user_crop.is_some() || state.rotate != 0.0)
                    && ui.small_button(crate::i18n::t!("取消取景")).clicked()
                {
                    state.crop_reset();
                    state.set_rotate(0.0);
                }
            });
        } else {
            kv(ui, "画幅比例", "跟随原生", muted());
        }
    }
    if group_header(ui, state, "frame-guides", "辅助线", false) {
        ui.checkbox(
            &mut state.grid,
            RichText::new(crate::i18n::t!("三分线")).size(SIZE_SMALL),
        );
        ui.checkbox(
            &mut state.cross,
            RichText::new(crate::i18n::t!("中心十字")).size(SIZE_SMALL),
        );
        ui.checkbox(
            &mut state.split,
            RichText::new(crate::i18n::t!("前后分割")).size(SIZE_SMALL),
        );
    }
}

/// 交付预设表只读一次：面板每帧都画，不能每帧读一遍 TOML。改表后重启生效——预设是
/// 低频配置，为它挂一个文件监听不值。
///
/// 预设 → 界面字段的映射在 `AppState::apply_delivery`（那边有判据），这里只管画。
///
/// The delivery table is read once: the panel redraws every frame and must not re-read a TOML
/// file each time. Editing the table takes effect after a restart; presets are low-frequency
/// configuration and a watcher is not worth the complexity. The preset-to-field mapping lives
/// in `AppState::apply_delivery` (which is asserted); this only draws.
fn delivery_table() -> &'static DeliveryTable {
    static TABLE: std::sync::OnceLock<DeliveryTable> = std::sync::OnceLock::new();
    TABLE.get_or_init(DeliveryTable::load)
}

/// 在系统文件管理器里定位一个文件。只用于「预设表」这个入口：用户改的是 TOML，
/// 不给个能点开的入口，等于把「改表不改代码」写成一句空话。
///
/// Reveals a file in the system file manager. Only used by the preset-table entry: the user
/// edits a TOML file, and without a clickable way to reach it "edit the table, not the code"
/// stays an empty promise.
fn reveal_in_file_manager(path: &std::path::Path) {
    #[cfg(windows)]
    {
        // `/select,<path>` 会在资源管理器里选中该文件；explorer 对成功/失败都返回同一个
        // 退出码，所以不看返回值，也不在这里报错。
        //
        // `/select,<path>` highlights the file in Explorer. Explorer returns the same exit
        // code either way, so the result is not inspected and no error is reported here.
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-R", &path.display().to_string()])
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // 没有「选中文件」的通行做法，退到打开它所在的目录。
        //
        // There is no portable "reveal", so open the containing directory instead.
        let dir = path.parent().unwrap_or(path);
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

fn adjust_render(state: &mut AppState, ui: &mut Ui) {
    // 平台交付：一行一个平台的完整口径，点一下把画幅/帧率/编码/质量档一次填好，之后
    // 仍能在下面逐项微调。放在「输出」之前——先定投哪儿，再调细节。
    //
    // One chip per platform fills frame, rate, encoder and quality in one click; everything
    // stays editable below. It sits before "Output": decide where it goes first, then tune.
    if group_header(ui, state, "render-delivery", "平台交付", true) {
        let table = delivery_table();
        let zh = crate::i18n::current() == crate::i18n::Lang::ZH;
        ui.horizontal_wrapped(|ui| {
            for preset in &table.presets {
                let response = bar_chip(ui, preset.name(zh), state.matches_delivery(preset));
                if response.clicked() {
                    state.apply_delivery(preset);
                }
                let hint = preset.hint(zh);
                if !hint.is_empty() {
                    response.on_hover_text(hint);
                }
            }
        });
        let active: Vec<&str> = table
            .presets
            .iter()
            .filter(|p| state.matches_delivery(p))
            .map(|p| p.name(zh))
            .collect();
        let line = if active.is_empty() {
            crate::i18n::t!("当前不是任何平台的口径（按自己的数字导出）").to_owned()
        } else {
            active.join(" · ")
        };
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(line).size(SIZE_SMALL).color(muted())).wrap());
        ui.horizontal_wrapped(|ui| {
            if ui.small_button(crate::i18n::t!("预设表…")).clicked() {
                let path = DeliveryTable::path();
                // 不存在就先写一份内置模板再打开：让人「照着一个空文件猜格式」是最坏的
                // 交付方式。
                //
                // Write the built-in template first when the file is missing: asking someone
                // to guess the format from an empty file is the worst kind of delivery.
                if !path.exists() {
                    let _ = table.write_to(&path);
                }
                reveal_in_file_manager(&path);
            }
            // 路径按**可用宽度**截断，不按字符数：34 个半角字符在 372px 面板里
            // 仍然宽过按钮剩下的位置，旧写法会让路径压到「预设表…」按钮上
            // （2026-11-04 的重叠自检抓到的实例）。`truncate()` 走 egui 的布局，
            // 拿到的是这一行真实的剩余宽度。
            //
            // Truncate by available width rather than by character count: 34 half-width
            // characters still overrun the space left of the button on a 372 px panel,
            // and the old form ran the path onto the button. `truncate()` asks egui's
            // layout for the real remaining width.
            ui.add(
                egui::Label::new(
                    RichText::new(DeliveryTable::path().display().to_string())
                        .size(SIZE_SMALL)
                        .color(dim()),
                )
                .truncate()
                .selectable(false),
            )
            .on_hover_text(crate::i18n::t!("改完重启生效：预设在启动时读一次"));
        });
    }
    if group_header(ui, state, "render-out", "输出", true) {
        ui.horizontal_wrapped(|ui| {
            for (mode, label) in [
                (OutputMode::Video, "视频"),
                (OutputMode::Stills, "静帧序列"),
            ] {
                if bar_chip(ui, label, state.render_mode == mode).clicked() {
                    state.render_mode = mode;
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for (label, w, h) in [
                ("原生", 0u32, 0u32),
                ("4K", 3840, 2160),
                ("1080p", 1920, 1080),
            ] {
                if bar_chip(ui, label, (state.output_w, state.output_h) == (w, h)).clicked() {
                    state.output_w = w;
                    state.output_h = h;
                }
            }
        });
        ui.add_space(4.0);
        let spec = if state.output_w == 0 || state.output_h == 0 {
            state
                .resolution
                .map(|(w, h)| crate::i18n::tr_fmt("原生 {} × {}", &[&w, &h]))
                .unwrap_or_else(|| "原生".to_owned())
        } else {
            format!("{} × {}", state.output_w, state.output_h)
        };
        kv(ui, "当前规格", spec, text());
        // 容器分模式：JPEG/WebP 是给最终交付用的，当不了 ffmpeg 的中间帧，所以视频
        // 模式只摆无损那两种；从静帧切回视频时把选中的容器退回 PNG，别让用户
        // 点导出才被 core 的守卫撞回来。
        if state.render_mode == OutputMode::Video && !state.render_format.frames_ok() {
            state.render_format = StillFormat::Png8;
        }
        // 成片现在也能出 HDR（x265 main10 + PQ）。以前这里把动态范围硬收回 SDR，
        // 因为那时成片只有 8bit；现在改由 `apply_dynamic` 补齐前提（HDR 成片必须
        // HEVC + Rec.2020），切换模式时不再动用户的动态范围选择。
        //
        // Video can now be HDR too (x265 main10 + PQ). This used to force the dynamic range
        // back to SDR because video was 8bit only; now `apply_dynamic` fills in the
        // prerequisites (HDR video needs HEVC + Rec.2020), so switching modes no longer
        // touches the user's choice.
        let formats: &[StillFormat] = if state.render_mode == OutputMode::Video {
            &[StillFormat::Png8, StillFormat::Tiff16]
        } else {
            &StillFormat::ALL
        };
        ui.horizontal_wrapped(|ui| {
            for &format in formats {
                if bar_chip(ui, format.cn(), state.render_format == format)
                    .on_hover_text(crate::i18n::tr(format.hint()))
                    .clicked()
                {
                    state.render_format = format;
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for space in ColorSpace::ALL {
                let picked = bar_chip(ui, space.cn(), state.render_color_space == space)
                    .on_hover_text(crate::i18n::tr(space.hint()))
                    .clicked();
                if picked {
                    state.render_color_space = space;
                }
            }
        });
        // 动态范围两种模式都开放：静帧走 AVIF 10bit，成片走 x265 main10。
        // 前提（容器 / 编码器）由 `apply_dynamic` 当场补齐，用户不会先点导出再挨错。
        //
        // Dynamic range is available in both modes: stills go AVIF 10bit, video goes x265
        // main10. The prerequisites (container / codec) are filled in by `apply_dynamic`, so
        // nobody presses export and gets an error afterwards.
        {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for dyn_range in OutputDynamic::ALL {
                    let picked = bar_chip(ui, dyn_range.cn(), state.render_dynamic == dyn_range)
                        .on_hover_text(crate::i18n::tr(dyn_range.hint()))
                        .clicked();
                    if picked {
                        state.render_dynamic = dyn_range;
                        // HDR 需要 Rec.2020 + AVIF，补齐摆在明面上（chips 会跳）。
                        state.apply_dynamic();
                    }
                }
            });
            if state.render_dynamic == OutputDynamic::HdrPq {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(if state.render_mode == OutputMode::Video {
                        crate::i18n::t!("HDR：x265 main10 + PQ，成片里带 smpte2084 标记")
                    } else {
                        crate::i18n::t!(
                            "HDR：参考白 203 nits，超过白的部分沿曲线尾部继续上升；只有 AVIF 装得下"
                        )
                    })
                    .size(SIZE_SMALL)
                    .color(theme::muted()),
                );
                // 面板只能显示到白，所以 HDR 高光在预览里始终和 SDR 一样被削平——
                // 「导出后拿别的软件看」正是这条缺口。这个开关把白以上折进最后一段
                // 码值，让高光层次在面板里可见（白以下逐位不动）。
                //
                // The panel can only reach white, so HDR highlights look clipped in the
                // preview exactly as in SDR — "export it and check elsewhere" was the gap.
                // This toggle folds above-white into the last slice of the code range so the
                // highlight layers are visible here (below white: bit-identical).
                if ui
                    .checkbox(
                        &mut state.hdr_preview,
                        RichText::new(crate::i18n::t!("预览白以上（SDR 屏上检视高光）"))
                            .size(SIZE_SMALL),
                    )
                    .on_hover_text(crate::i18n::t!(
                        "屏幕显示不到白：打开后把白以上折进最后约 25 级，高光里的层次就看得见；中间调与暗部一位不动。这不是模拟 HDR 屏亮度，只是让你确认白以上还有没有东西。"
                    ))
                    .changed()
                {
                    state.preview_dirty = true;
                    state.settings_dirty = true;
                }
            }
        }
        if state.render_format.lossy() {
            ui.add_space(4.0);
            let mut q = f64::from(state.render_quality);
            if slider(
                ui,
                "render.quality",
                "压缩质量",
                &mut q,
                1.0,
                100.0,
                90.0,
                0,
                false,
            ) {
                state.render_quality = q.round().clamp(1.0, 100.0) as u8;
            }
        }
        if state.render_format.supports_exif() {
            ui.add_space(2.0);
            ui.checkbox(
                &mut state.render_keep_exif,
                RichText::new(crate::i18n::t!("保留 EXIF 与版权")).size(SIZE_SMALL),
            )
            .on_hover_text(crate::i18n::t!("源 JPEG 的 EXIF 块原样搬进输出（机身、曝光、版权都在里面）；RAW 与 TIFF 源搬不动，容器不支持就不写"));
        }
        // 旁车只服务静帧：视频模式的中间帧是过程产物，不给它们发 .xmp，所以这一格
        // 跟着模式走，跟上面「容器分模式」一个口径。
        if state.render_mode == OutputMode::Stills {
            ui.add_space(2.0);
            ui.checkbox(
                &mut state.render_xmp_sidecar,
                RichText::new(crate::i18n::t!("写 XMP 旁车")).size(SIZE_SMALL),
            )
            .on_hover_text(crate::i18n::t!("每张静帧旁边写一个同名 .xmp：说明与版权四项（dc:）加这一张的调色参数（crs:），Lightroom / Camera Raw 直接认"));
        }
        // 续跑两种模式都要：静帧补的是交付帧，成片补的是中间帧（开了它就不走管道直连）。
        ui.add_space(2.0);
        ui.checkbox(
            &mut state.render_resume,
            RichText::new(crate::i18n::t!("断点续跑（跳过已导出的帧）")).size(SIZE_SMALL),
        )
        // 一条一行写全：Rust 的续行（反斜杠+换行）会把换行与缩进一起吃掉，
        // 而译文表按字面量原样存键，两边口径对不上就查不到译文。
        .on_hover_text(crate::i18n::t!(
            "输出目录里已经写完的帧原样留着，这一趟只补没写出来的那些。\n跑崩了再点一次就是接着导，不必从头重渲。\n它不看画面有没有改过：改了参数想重导就别勾，或者先把旧帧删掉。"
        ));
        ui.add_space(4.0);
        // 输出锐化是成片级设置：在缩放到最终尺寸之后补的那一刀，预览不看它。
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("输出锐化"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            for &s in &OutputSharpen::ALL {
                if bar_chip(ui, s.cn(), state.render_sharpen_for == s)
                    .on_hover_text(crate::i18n::tr(s.hint()))
                    .clicked()
                {
                    state.render_sharpen_for = s;
                }
            }
        });
        if state.render_sharpen_for != OutputSharpen::Off {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(crate::i18n::t!("强度"))
                        .size(SIZE_SMALL)
                        .color(dim()),
                );
                for &s in &SharpenStrength::ALL {
                    if bar_chip(ui, s.cn(), state.render_sharpen_strength == s).clicked() {
                        state.render_sharpen_strength = s;
                    }
                }
            });
        }
        if state.render_mode == OutputMode::Video {
            ui.add_space(2.0);
            ui.checkbox(
                &mut state.render_keep_frames,
                RichText::new(crate::i18n::t!("保留中间帧")).size(SIZE_SMALL),
            );
            ui.add_space(2.0);
            let dir = state.render_out_dir.display().to_string();
            ui.add(
                egui::Label::new(
                    RichText::new(crate::i18n::tr_fmt(
                        "中间帧 · {}",
                        &[&theme::ellipsis(&dir, 30)],
                    ))
                    .size(SIZE_SMALL)
                    .color(dim()),
                )
                .wrap(),
            );
            if bar_button(ui, "选择输出目录…").clicked()
                && let Some(dir) = rfd::FileDialog::new()
                    .set_directory(
                        state
                            .seq
                            .as_ref()
                            .map(|s| s.root.clone())
                            .unwrap_or_default(),
                    )
                    .pick_folder()
            {
                state.render_out_dir = dir;
            }
        }
    }
    if group_header(ui, state, "render-watermark", "水印", true) {
        let mut enabled = state.render_watermark.enabled;
        if ui
            .checkbox(
                &mut enabled,
                RichText::new(crate::i18n::t!("在成片上盖章")).size(SIZE_SMALL),
            )
            .on_hover_text(crate::i18n::t!(
                "和输出锐化同一条规矩：只在导出尾部生效，预览与缩略图都不叠这一层"
            ))
            .changed()
        {
            state.render_watermark.enabled = enabled;
            let on = crate::i18n::tr(if enabled { "开" } else { "关" });
            history::note("render.watermark", crate::i18n::tr_fmt("水印 {}", &[&on]));
        }
        if state.render_watermark.enabled {
            ui.add_space(4.0);
            // 内容二选一：切过去另一侧的输入原样留着，来回切不丢正文。
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(crate::i18n::t!("内容"))
                        .size(SIZE_SMALL)
                        .color(muted()),
                );
                for &kind in &WatermarkKind::ALL {
                    if bar_chip(ui, kind.cn(), state.render_watermark.kind == kind).clicked() {
                        state.render_watermark.kind = kind;
                        history::note(
                            "render.watermark.kind",
                            crate::i18n::tr_fmt("水印内容 {}", &[&crate::i18n::tr(kind.cn())]),
                        );
                    }
                }
            });
            ui.add_space(2.0);
            if state.render_watermark.kind == WatermarkKind::Text {
                let edited = ui.add(
                    egui::TextEdit::singleline(&mut state.render_watermark.text)
                        .hint_text(format!("© {}", aevum_core::brand::TITLE))
                        .desired_width(f32::INFINITY)
                        .font(mono(SIZE_SMALL)),
                );
                if edited.changed() {
                    history::note("render.watermark.text", "水印正文");
                }
            } else {
                let picked = state.render_watermark.image.display().to_string();
                let shown = if state.render_watermark.image.as_os_str().is_empty() {
                    "还没选图片".to_owned()
                } else {
                    theme::ellipsis(&picked, 24)
                };
                ui.horizontal(|ui| {
                    if bar_button(ui, "选择图片…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("图片", &["png", "jpg", "jpeg", "webp", "tif", "tiff"])
                            .set_directory(
                                state
                                    .seq
                                    .as_ref()
                                    .map(|s| s.root.clone())
                                    .unwrap_or_default(),
                            )
                            .pick_file()
                    {
                        state.render_watermark.image = path;
                        history::note("render.watermark.image", "水印图片");
                    }
                    ui.label(RichText::new(shown).size(SIZE_SMALL).color(dim()));
                });
            }
            ui.add_space(4.0);
            // 九宫格按屏幕坐标读：y 向下，所以「上中」真的是上面那格。
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(crate::i18n::t!("位置"))
                        .size(SIZE_SMALL)
                        .color(muted()),
                );
                for &spot in &Spot::ALL {
                    if bar_chip(ui, spot.cn(), state.render_watermark.spot == spot).clicked() {
                        state.render_watermark.spot = spot;
                        history::note(
                            "render.watermark.spot",
                            crate::i18n::tr_fmt("水印位置 {}", &[&crate::i18n::tr(spot.cn())]),
                        );
                    }
                }
            });
            let mut opacity = f64::from(state.render_watermark.opacity);
            if slider(
                ui,
                "render.watermark.opacity",
                "不透明度",
                &mut opacity,
                0.0,
                100.0,
                60.0,
                0,
                false,
            ) {
                state.render_watermark.opacity = opacity.round().clamp(0.0, 100.0) as u8;
            }
            let mut size = f64::from(state.render_watermark.size);
            if slider(
                ui,
                "render.watermark.size",
                "尺寸",
                &mut size,
                1.0,
                40.0,
                4.0,
                0,
                false,
            ) {
                state.render_watermark.size = size.round().clamp(1.0, 40.0) as u8;
            }
            let mut margin = f64::from(state.render_watermark.margin);
            if slider(
                ui,
                "render.watermark.margin",
                "边距",
                &mut margin,
                0.0,
                40.0,
                2.0,
                0,
                false,
            ) {
                state.render_watermark.margin = margin.round().clamp(0.0, 40.0) as u8;
            }
            if state.render_watermark.kind == WatermarkKind::Text {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(crate::i18n::t!("颜色"))
                            .size(SIZE_SMALL)
                            .color(muted()),
                    );
                    for (name, rgb) in [
                        ("白", [255u8, 255, 255]),
                        ("浅灰", [200, 200, 200]),
                        ("黑", [0, 0, 0]),
                    ] {
                        if bar_chip(ui, name, state.render_watermark.color == rgb).clicked() {
                            state.render_watermark.color = rgb;
                            history::note(
                                "render.watermark.color",
                                crate::i18n::tr_fmt("水印颜色 {}", &[&crate::i18n::tr(name)]),
                            );
                        }
                    }
                });
            }
            if !state.render_watermark.is_on() {
                note(ui, "正文、图片与不透明度至少填一项。");
            }
        }
    }
    if group_header(ui, state, "render-time", "时间", true) {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
        let p = ui.painter();
        p.text(
            Pos2::new(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            crate::i18n::t!("帧率"),
            font(SIZE_SMALL),
            muted(),
        );
        in_field(ui, field(), field_hover(), |ui| {
            ui.put(
                Rect::from_min_size(
                    Pos2::new(rect.right() - 92.0, rect.center().y - 9.0),
                    Vec2::new(92.0, 18.0),
                ),
                egui::DragValue::new(&mut state.output_fps)
                    .range(1.0..=120.0)
                    .speed(1.0)
                    .fixed_decimals(0)
                    .suffix(" fps"),
            );
        });
        let dur = match state.retime_estimate() {
            // 片长说的是导出来的东西：变速开着就按变速后的秒数算。
            Ok((_, seconds)) => seconds as f32,
            Err(_) => state.frame_count() as f32 / state.output_fps.max(1.0),
        };
        kv(
            ui,
            "片长",
            crate::i18n::tr_fmt("{} 秒", &[&format!("{dur:.1}")]),
            muted(),
        );
    }
    if group_header(ui, state, "render-retime", "变速 · 运动模糊", true) {
        let mut enabled = state.retime.enabled;
        if ui
            .checkbox(
                &mut enabled,
                RichText::new(crate::i18n::t!("重映射时间（变速 + 运动模糊）")).size(SIZE_SMALL),
            )
            .changed()
        {
            state.retime.enabled = enabled;
            history::note("retime.enabled", "变速开关");
        }
        if enabled {
            let flat = flat_speed(&state.retime);
            ui.horizontal_wrapped(|ui| {
                for speed in [0.5, 1.0, 2.0, 4.0, 8.0] {
                    let label = format!("{speed}×");
                    if bar_chip(ui, &label, flat == Some(speed)).clicked() {
                        state.retime = Retime::uniform(speed, state.retime.shutter);
                        history::note("retime.speed", crate::i18n::tr_fmt("倍速 {}", &[&label]));
                    }
                }
                // 曲线不平时就是画过形状（缓入缓出）：芯片按「平不平」亮，不去猜具体几个键。
                if bar_chip(ui, "缓入缓出", flat.is_none()).clicked() {
                    let peak = retime::speed_at(&state.retime.keys, 0.5);
                    state.retime = Retime::ease(1.0, peak, 0.25, 0.75, state.retime.shutter);
                    history::note("retime.ease", "变速缓入缓出");
                }
            });
            ui.add_space(2.0);
            if let Some(speed) = flat {
                let mut v = speed;
                if slider(
                    ui,
                    "retime.speed",
                    "倍速",
                    &mut v,
                    0.05,
                    16.0,
                    1.0,
                    2,
                    false,
                ) {
                    state.retime = Retime::uniform(v, state.retime.shutter);
                }
            } else {
                // 曲线态下倍速不是一个数，推子会把它抹平成匀速——只给一行读数。
                let at = retime::speed_at(&state.retime.keys, 0.0);
                let mid = retime::speed_at(&state.retime.keys, 0.5);
                let end = retime::speed_at(&state.retime.keys, 1.0);
                kv(
                    ui,
                    "曲线",
                    format!("{}× → {}× → {}×", at, mid, end),
                    muted(),
                );
            }
            // 拖点编辑：键表空着（刚勾上开关）时先摆两个 1× 端点，第一次拖就落进真实键表。
            // 比色调曲线那格矮一档：这组上下还有芯片、快门与读数，方框吃满会把它们挤出视野。
            let box_side = ui.available_width().clamp(150.0, 208.0);
            let mut keys = if state.retime.keys.is_empty() {
                vec![
                    SpeedKey {
                        at: 0.0,
                        speed: 1.0,
                    },
                    SpeedKey {
                        at: 1.0,
                        speed: 1.0,
                    },
                ]
            } else {
                state.retime.keys.clone()
            };
            if widgets::speed_box(ui, "retime", &mut keys, box_side, &mut state.retime_grab) {
                state.retime.keys = keys;
                history::note("retime.keys", "速度曲线");
            }
            let mut pct = state.retime.shutter * 100.0;
            if slider(
                ui,
                "retime.shutter",
                "快门（模糊量）",
                &mut pct,
                0.0,
                100.0,
                0.0,
                0,
                false,
            ) {
                state.retime.shutter = (pct / 100.0).clamp(0.0, 1.0);
            }
            match state.retime_estimate() {
                Ok((frames, seconds)) => {
                    kv(
                        ui,
                        "变速后",
                        crate::i18n::tr_fmt(
                            "{} 帧 · {} 秒",
                            &[&frames, &format!("{:.1}", seconds)],
                        ),
                        accent(),
                    );
                }
                Err(e) => {
                    ui.add(
                        egui::Label::new(RichText::new(e).size(SIZE_SMALL).color(danger())).wrap(),
                    );
                }
            }
        }
    }
    if group_header(ui, state, "render-motion", "运镜 · Ken Burns", true) {
        let mut enabled = state.motion.enabled;
        if ui
            .checkbox(
                &mut enabled,
                RichText::new(crate::i18n::t!("启用运镜")).size(SIZE_SMALL),
            )
            .changed()
        {
            state.set_motion_enabled(enabled);
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            // 选中的那一端才在画布上带把手：两端同时可拖会抢同一个把手命中区。
            for w in [MotionBox::Start, MotionBox::End] {
                let on = state.motion_picking && state.motion_box == w;
                if bar_chip(ui, w.cn(), on).clicked() {
                    state.set_motion_box(w);
                    state.set_motion_picking(true);
                }
            }
            if bar_chip(ui, "拖取景框", state.motion_picking).clicked() {
                let on = !state.motion_picking;
                state.set_motion_picking(on);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for e in crop::Ease::ALL {
                if bar_chip(ui, e.cn(), state.motion.ease == e).clicked() {
                    state.set_motion_ease(e);
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.small_button(crate::i18n::t!("反向")).clicked() {
                state.motion_swap();
            }
            if ui.small_button(crate::i18n::t!("铺满")).clicked() {
                state.motion_fill_box();
            }
            if state.motion != crop::Motion::default()
                && ui.small_button(crate::i18n::t!("清零")).clicked()
            {
                state.motion_reset();
            }
        });
        ui.add_space(4.0);
        let (fw, fh) = state.motion_frame_size();
        let (ow, oh) = state.motion_output_size();
        let s = state.motion_box_rect(MotionBox::Start);
        let e = state.motion_box_rect(MotionBox::End);
        let zf = state.motion.from.zoom(ow, oh, fw, fh);
        let zt = state.motion.to.zoom(ow, oh, fw, fh);
        kv(ui, "起点", format!("{} × {} · {zf:.2}×", s.w, s.h), muted());
        kv(ui, "终点", format!("{} × {} · {zt:.2}×", e.w, e.h), muted());
        if state.motion_picking {
            kv(
                ui,
                "画布",
                crate::i18n::tr_fmt("拖 {} 那一端", &[&crate::i18n::tr(state.motion_box.cn())]),
                accent(),
            );
        }
        if zf.max(zt) > 2.0 {
            ui.add(
                egui::Label::new(
                    RichText::new(crate::i18n::t!(
                        "推近已经过 2×：再推就是拿少数像素撑大画面，会糊。"
                    ))
                    .size(SIZE_SMALL)
                    .color(warn()),
                )
                .wrap(),
            );
        }
    }
    if state.render_mode == OutputMode::Video
        && group_header(ui, state, "render-codec", "编码", true)
    {
        ui.horizontal_wrapped(|ui| {
            for codec in ["HEVC", "H.264"] {
                if bar_chip(ui, codec, state.codec == codec).clicked() {
                    state.codec = codec.to_owned();
                    // H.264 那条是 8bit，装不下 PQ。选它就把动态范围退回 SDR——可见地
                    // 退，不是偷偷留一个点导出才报错的组合。
                    //
                    // H.264 is 8bit and cannot carry PQ. Picking it drops the dynamic range
                    // back to SDR — visibly, rather than leaving a combination that only
                    // fails when export is pressed.
                    if state.codec != "HEVC" {
                        state.render_dynamic = OutputDynamic::Sdr;
                    }
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("质量 CRF"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            let crf = in_field(ui, field(), field_hover(), |ui| {
                ui.add(
                    egui::DragValue::new(&mut state.render_crf)
                        .range(0..=51)
                        .speed(0.2),
                )
            });
            crf.on_hover_text(crate::i18n::t!("数值越小越清晰"));
        });
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            for (preset, label) in [
                ("ultrafast", "极速"),
                ("fast", "快"),
                ("medium", "均衡"),
                ("slow", "慢"),
                ("veryslow", "很慢"),
            ] {
                if bar_chip(ui, label, state.render_preset == preset).clicked() {
                    state.render_preset = preset.to_owned();
                }
            }
        });
        let video = theme::ellipsis(&state.render_video_out.display().to_string(), 30);
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new(crate::i18n::tr_fmt("成片 · {}", &[&video]))
                    .size(SIZE_SMALL)
                    .color(dim()),
            )
            .wrap(),
        );
        if bar_button(ui, "选择成片位置…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("视频", &["mp4", "mov", "mkv"])
                .set_file_name(
                    state
                        .render_video_out
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("成片.mp4"),
                )
                .save_file()
        {
            state.render_video_out = path;
        }
    }
    if group_header(ui, state, "render-export", "导出", false) {
        let ctx = ui.ctx().clone();
        let w = ui.available_width();
        let busy = state.export_busy;
        let label = if busy {
            "正在导出…"
        } else if state.render_mode == OutputMode::Video {
            "导出成片"
        } else {
            "导出静帧序列"
        };
        let resp = ui.add_enabled(
            !busy,
            egui::Button::new(
                RichText::new(label)
                    .size(SIZE_SMALL)
                    .color(on_accent())
                    .strong(),
            )
            .fill(accent())
            .corner_radius(radius_control())
            .min_size(Vec2::new(w, 26.0)),
        );
        if resp.clicked() {
            state.export_copy = None;
            state.run_export(&ctx);
        }
        if busy {
            if let Some((done, total, stage)) = state.export_progress() {
                // 进度条按阶段分区间：`done` 是**阶段内**计数（写帧 0..total，编码又从 1
                // 起数），直接当比例会让进度条在阶段切换时从 100% 退回十几——用户看到
                // 条子倒退，会以为任务重启了。写帧占 0–70%、编码占 70–100%，两段各自
                // 单调，接缝只跨一次。文案仍然分阶段显示，不做「假一体化」。
                //
                // The bar maps stages to ranges: `done` is a **per-stage** count (frames
                // 0..total, then the encode restarts at 1), so using it directly makes the
                // bar fall from 100% back to the teens at the switch, which reads as "the
                // job restarted". Frames take 0-70%, encode 70-100%; each segment is
                // monotone and the seam is crossed once. The text still names the stage.
                let frac = export_bar_fraction(stage, done, total);
                let text = match stage {
                    aevum_core::export::Stage::Frames => {
                        crate::i18n::tr_fmt("写帧 {} / {}", &[&done, &total])
                    }
                    aevum_core::export::Stage::Encode => {
                        crate::i18n::tr_fmt("编码 {} / {}", &[&done, &total])
                    }
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_height(4.0)
                        .text(RichText::new(text).size(SIZE_SMALL)),
                );
            }
            if bar_button(ui, "取消导出").clicked() {
                state.stop_export();
            }
        }
        if let Some(path) = &state.last_saved {
            ui.add(
                egui::Label::new(
                    RichText::new(crate::i18n::tr_fmt(
                        "已保存 · {}",
                        &[&theme::ellipsis(path, 34)],
                    ))
                    .size(SIZE_SMALL)
                    .color(ok()),
                )
                .wrap(),
            );
        }
        ui.add_space(4.0);
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(
                    RichText::new(crate::i18n::tr_fmt(
                        "导出当前帧 · {}",
                        &[&crate::i18n::tr(state.render_format.cn())],
                    ))
                    .size(SIZE_SMALL),
                )
                .min_size(Vec2::new(w, 24.0)),
            )
            .clicked()
        {
            state.export_current(&ctx);
        }
    }
    if group_header(ui, state, "render-queue", "队列", false) {
        let busy = state.queue_busy;
        let count = state.queue.len();
        ui.horizontal_wrapped(|ui| {
            let resp = ui.add_enabled(!busy, {
                egui::Button::new(
                    RichText::new(crate::i18n::t!("加入当前序列"))
                        .size(SIZE_SMALL)
                        .color(text()),
                )
                .fill(elev())
                .stroke(Stroke::NONE)
                .corner_radius(radius_control())
                .min_size(Vec2::new(0.0, 22.0))
            });
            if resp.clicked()
                && let Err(e) = state.add_current_to_queue()
            {
                state.error = Some(e);
            }
            if busy {
                if bar_button(ui, "取消队列").clicked() {
                    state.stop_queue();
                }
            } else {
                let ctx = ui.ctx().clone();
                if bar_primary(ui, "开始渲染队列").clicked()
                    && let Err(e) = state.start_queue(&ctx)
                {
                    state.error = Some(e);
                }
            }
            if !busy && count > 0 && bar_button(ui, "清空").clicked() {
                state.clear_queue_rows();
            }
        });
        kv(
            ui,
            "队列",
            crate::i18n::tr_fmt("{} 段 · {}", &[&count, &queue_counts(state)]),
            if count > 0 { text() } else { dim() },
        );
        if count == 0 {
            note(ui, "尚无排队段。");
        } else {
            queue_table(state, ui, busy);
            if busy && let Some((done, total, stage)) = state.queue_progress() {
                let frac = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                let current = state.queue_current.map(|i| i + 1).unwrap_or(1);
                let phase = match stage {
                    aevum_core::export::Stage::Frames => "写帧",
                    aevum_core::export::Stage::Encode => "编码",
                };
                ui.add(
                    egui::ProgressBar::new(frac).desired_height(4.0).text(
                        RichText::new(crate::i18n::tr_fmt(
                            "第 {}/{} 段 · {} {}/{}",
                            &[&current, &count, &phase, &done, &total],
                        ))
                        .size(SIZE_SMALL),
                    ),
                );
            }
        }
        ui.add(
            egui::Label::new(
                RichText::new(crate::i18n::tr_fmt(
                    "队列文件 · {}",
                    &[&theme::ellipsis(
                        &state.queue_path.display().to_string(),
                        30,
                    )],
                ))
                .size(SIZE_SMALL)
                .color(dim()),
            )
            .wrap(),
        );
    }
}

/// 队列表格：LapseOne 的导出队列口径——列头 + 每行一条自己的进度。
/// 之前整屏只有一行「第 x/y 段」，排上七八段就看不出哪段在跑、哪段已经好了。
fn queue_table(state: &mut AppState, ui: &mut Ui, busy: bool) {
    const ROW_H: f32 = 22.0;
    const COL_STATUS: f32 = 42.0;
    const COL_SPEC: f32 = 76.0;
    const COL_BAR: f32 = 88.0;
    const GAP: f32 = 6.0;
    let width = ui.available_width();
    let name_w = (width - COL_STATUS - COL_SPEC - COL_BAR - GAP * 3.0 - 16.0).max(48.0);
    let x0 = ui.cursor().left();
    let x_name = x0 + COL_STATUS + GAP;
    let x_spec = x_name + name_w + GAP;
    let x_bar = x_spec + COL_SPEC + GAP;
    let p = ui.painter().clone();

    let (head, _) = ui.allocate_exact_size(Vec2::new(width, 16.0), Sense::hover());
    for (x, text) in [
        (x0, "状态"),
        (x_name, "序列"),
        (x_spec, "规格"),
        (x_bar, "进度"),
    ] {
        p.text(
            Pos2::new(x, head.center().y),
            Align2::LEFT_CENTER,
            text,
            font(SIZE_SMALL),
            dim(),
        );
    }
    theme::rule(&p, head.left(), head.right(), head.bottom(), divider());

    let mut remove = None;
    egui::ScrollArea::vertical()
        .max_height(150.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (index, item) in state.queue.iter().enumerate() {
                let (rr, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::hover());
                let current = busy && state.queue_current == Some(index);
                if current {
                    p.rect_filled(rr.shrink(1.0), radius_control(), selected());
                }
                let (frac, status) = if current {
                    let (done, total, _) =
                        state
                            .queue_progress()
                            .unwrap_or((0, 0, aevum_core::export::Stage::Frames));
                    (
                        if total > 0 {
                            (done as f32 / total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        },
                        "渲染中",
                    )
                } else {
                    (
                        if item.status == QueueStatus::Done {
                            1.0
                        } else {
                            0.0
                        },
                        item.status.label(),
                    )
                };
                let status_color = match item.status {
                    QueueStatus::Done => ok(),
                    QueueStatus::Failed => danger(),
                    QueueStatus::Pending => {
                        if current {
                            accent()
                        } else {
                            dim()
                        }
                    }
                };
                p.text(
                    Pos2::new(x0, rr.center().y),
                    Align2::LEFT_CENTER,
                    status,
                    font(SIZE_SMALL),
                    status_color,
                );
                let name = std::path::Path::new(&item.root)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| item.root.clone());
                p.text(
                    Pos2::new(x_name, rr.center().y),
                    Align2::LEFT_CENTER,
                    theme::fit_text(&p, &name, font(SIZE_SMALL), text(), name_w),
                    font(SIZE_SMALL),
                    if current { Color32::WHITE } else { text() },
                );
                let spec = format!("{}×{} {}", item.width, item.height, item.codec);
                p.text(
                    Pos2::new(x_spec, rr.center().y),
                    Align2::LEFT_CENTER,
                    theme::fit_text(&p, &spec, mono(10.0), muted(), COL_SPEC),
                    mono(10.0),
                    muted(),
                );
                // 进度：绿填充 + 居中百分比，排队中是空心轨道——和 LapseOne 同一读法。
                let bar = Rect::from_min_max(
                    Pos2::new(x_bar, rr.center().y - 5.5),
                    Pos2::new(x_bar + COL_BAR - 18.0, rr.center().y + 5.5),
                );
                p.rect_stroke(
                    bar,
                    3.0,
                    Stroke::new(1.0, if frac > 0.0 { ok() } else { rail_fill() }),
                    StrokeKind::Inside,
                );
                if frac > 0.0 {
                    p.rect_filled(
                        Rect::from_min_max(
                            bar.min,
                            Pos2::new(bar.left() + bar.width() * frac, bar.bottom()),
                        ),
                        3.0,
                        ok().gamma_multiply(0.85),
                    );
                }
                p.text(
                    bar.center(),
                    Align2::CENTER_CENTER,
                    if item.status == QueueStatus::Failed {
                        "失败".to_owned()
                    } else if frac > 0.0 {
                        format!("{:.0}%", frac * 100.0)
                    } else {
                        "排队".to_owned()
                    },
                    mono(10.0),
                    if frac > 0.0 { Color32::WHITE } else { dim() },
                );
                if !busy {
                    let xrect = Rect::from_min_size(
                        Pos2::new(rr.right() - 16.0, rr.center().y - 8.0),
                        Vec2::splat(16.0),
                    );
                    let del = ui.interact(
                        xrect,
                        ui.make_persistent_id(("queue-del", index)),
                        Sense::click(),
                    );
                    p.text(
                        xrect.center(),
                        Align2::CENTER_CENTER,
                        "×",
                        font(SIZE_BODY),
                        if del.hovered() { danger() } else { dim() },
                    );
                    if del.clicked() {
                        remove = Some(index);
                    }
                }
                resp.on_hover_text(crate::i18n::tr_fmt(
                    "{}\n{}",
                    &[&item.out, &crate::i18n::tr(&item.note)],
                ));
            }
        });
    if let Some(index) = remove {
        state.remove_queue_at(index);
    }
}

fn queue_counts(state: &AppState) -> String {
    let done = state
        .queue
        .iter()
        .filter(|i| i.status == QueueStatus::Done)
        .count();
    let failed = state
        .queue
        .iter()
        .filter(|i| i.status == QueueStatus::Failed)
        .count();
    if done == 0 && failed == 0 {
        crate::i18n::t!("待渲").to_owned()
    } else {
        crate::i18n::tr_fmt("完成 {} · 失败 {}", &[&done, &failed])
    }
}

fn adjust_audio(state: &mut AppState, ui: &mut Ui) {
    if group_header(ui, state, "audio", "配乐", true) {
        let w = ui.available_width();
        if ui
            .add(
                egui::Button::new(RichText::new(crate::i18n::t!("选择音频…")).size(SIZE_SMALL))
                    .min_size(Vec2::new(w, 24.0)),
            )
            .clicked()
        {
            choose_audio(state);
        }
        ui.add_space(4.0);
        match &state.render_audio {
            Some(path) => {
                kv(
                    ui,
                    "当前音频",
                    theme::ellipsis(
                        path.file_name().and_then(|s| s.to_str()).unwrap_or("音频"),
                        26,
                    ),
                    text(),
                );
                if let Some(info) = &state.audio_info {
                    kv(
                        ui,
                        "编码 / 时长",
                        format!("{} · {:.1}s", info.codec, info.duration),
                        muted(),
                    );
                } else if let Some(err) = &state.audio_probe_err {
                    ui.add(
                        egui::Label::new(RichText::new(err).size(SIZE_SMALL).color(danger()))
                            .wrap(),
                    );
                }
                if bar_button(ui, "移除音频").clicked() {
                    state.set_render_audio(None);
                }
            }
            None => {
                kv(ui, "当前音频", "无", dim());
            }
        }
    }

    if group_header(ui, state, "audio-time", "时间与音量", true) {
        {
            let opts = &mut state.render_audio_opts;
            slider(
                ui,
                "au-delay",
                "延后进入（秒）",
                &mut opts.delay,
                0.0,
                60.0,
                0.0,
                2,
                false,
            );
            slider(
                ui,
                "au-gain",
                "音量（%）",
                &mut opts.gain,
                0.0,
                200.0,
                100.0,
                0,
                false,
            );
            slider(
                ui,
                "au-fin",
                "淡入（秒）",
                &mut opts.fade_in,
                0.0,
                30.0,
                0.0,
                2,
                false,
            );
            slider(
                ui,
                "au-fout",
                "淡出（秒）",
                &mut opts.fade_out,
                0.0,
                30.0,
                0.0,
                2,
                false,
            );
        }
    }

    if group_header(ui, state, "audio-mix", "长度与编码", true) {
        ui.checkbox(
            &mut state.render_audio_opts.looped,
            RichText::new(crate::i18n::t!("循环铺满画面")).size(SIZE_SMALL),
        )
        .on_hover_text(crate::i18n::t!(
            "配乐比画面短时反复播放；循环会限长到画面末尾，不会无限铺下去"
        ));
        ui.checkbox(
            &mut state.render_audio_opts.trim,
            RichText::new(crate::i18n::t!("截到画面结束")).size(SIZE_SMALL),
        )
        .on_hover_text(crate::i18n::t!("配乐比画面长时在画面末尾截断"));
        ui.add_space(4.0);
        egui::ComboBox::from_id_salt("audio-codec")
            .selected_text(
                RichText::new(codec_label(state.render_audio_opts.acodec))
                    .size(SIZE_SMALL)
                    .color(text()),
            )
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for codec in [
                    AudioCodec::Auto,
                    AudioCodec::Copy,
                    AudioCodec::Aac128,
                    AudioCodec::Aac192,
                    AudioCodec::Aac256,
                    AudioCodec::Aac320,
                ] {
                    if ui
                        .add(egui::Button::selectable(
                            state.render_audio_opts.acodec == codec,
                            codec_label(codec),
                        ))
                        .clicked()
                    {
                        state.render_audio_opts.acodec = codec;
                    }
                }
            });
        note(
            ui,
            "自动模式：能原样放进容器的编码直接复制，其余转 AAC 192k。",
        );
    }

    if group_header(ui, state, "audio-timeline", "成片时长", false) {
        match (state.audio_timeline(), &state.render_audio) {
            (Some((video, music, total)), _) => {
                kv(ui, "画面", format!("{video:.1} s"), muted());
                kv(ui, "配乐", format!("{music:.1} s"), muted());
                kv(ui, "成片", format!("{total:.1} s"), accent());
                if total > video + 0.05 {
                    note(ui, "配乐比画面长：片尾会只剩声音。");
                }
            }
            (None, Some(_)) => {
                let video = state.frame_count() as f64 / state.output_fps.max(1.0) as f64;
                kv(ui, "画面", format!("{video:.1} s"), muted());
                kv(ui, "配乐", "时长未知", dim());
            }
            (None, None) => {
                kv(ui, "成片时长", "选一段配乐后显示", dim());
            }
        }
    }
}

#[cfg(test)]
mod qa_panel_tests {
    use super::*;
    use crate::shell::ui;

    fn paint_count(ctx: &Context, state: &mut AppState) -> usize {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |panel| ui(state, panel),
        );
        let count = out.shapes.len();
        out.textures_delta.clear();
        count
    }

    /// 把所有画出来的文字拼成一坨：清单有没有上屏，看字比数形状可靠。
    fn painted_text(ctx: &Context, state: &mut AppState) -> String {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |panel| ui(state, panel),
        );
        let mut text = String::new();
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape {
                text.push_str(t.galley.text());
                text.push('\n');
            }
        }
        out.textures_delta.clear();
        text
    }

    fn finding(frame: usize, kind: qa::Kind, note: &str) -> Finding {
        Finding {
            frame,
            kind,
            value: 0.42,
            note: note.to_owned(),
        }
    }

    /// 体检组画在序列面板里：清单一旦落账，分类计数与逐条帧号都要真的上屏。
    /// 面板画漏了不会报错，只会静悄悄显示一张空表——所以这里认的是字。
    #[test]
    fn findings_paint_inside_the_sequence_panel() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        state.module = Module::Sequence;
        state.qa_run = false;
        state.qa_findings.clear();
        // 两遍：跟 --selftest 一样把字体与纹理的冷启动排除掉。
        let _ = paint_count(&ctx, &mut state);
        let cold = painted_text(&ctx, &mut state);
        assert!(cold.contains("序列体检"), "序列面板里没有体检组");
        assert!(!cold.contains("疑似虚焦"), "没跑过体检却列了条目");

        state.qa_run = true;
        state.qa_findings = vec![
            finding(4, qa::Kind::Black, "亮度只有 0.0020，可能是误拍或镜头盖"),
            finding(9, qa::Kind::Gap, "缺 1 帧（编号 10）：成片里会按上一帧停留"),
            finding(
                12,
                qa::Kind::Blur,
                "清晰度只有左右邻居的 18%，大概虚焦或抖糊了",
            ),
            finding(
                19,
                qa::Kind::Dup,
                "与上一帧的测量和文件大小完全一致，可能是快门没动作",
            ),
        ];
        let hot = painted_text(&ctx, &mut state);
        for want in [
            "几乎全黑 1",
            "序列缺口 1",
            "疑似虚焦 1",
            "疑似重复 1",
            "0005 · 几乎全黑",
            "0010 · 序列缺口",
            "0013 · 疑似虚焦",
            "0020 · 疑似重复",
            "结论",
        ] {
            assert!(hot.contains(want), "清单缺「{want}」：\n{hot}");
        }

        // 跑任务时换成进度条，旧清单先收起：画一张还没算完的表比不画更误导。
        state.qa_busy = true;
        let running = painted_text(&ctx, &mut state);
        assert!(
            !running.contains("0013 · 疑似虚焦"),
            "忙碌时还把旧清单一起画着"
        );
        assert!(running.contains("取消"), "忙碌时没有取消入口");
        state.qa_busy = false;

        // 折叠这一组之后，清单要退回去。
        state.collapsed.insert("seq-qa");
        let folded = painted_text(&ctx, &mut state);
        assert!(!folded.contains("0013 · 疑似虚焦"), "折叠了还在画清单");
        assert!(folded.contains("序列体检"), "折叠后连组标题都没了");
    }
}

#[cfg(test)]
/// 色温/色调那条渐变轨，以及截图时把分组滚到顶要连展开一起做。
mod gradient_rail_tests {
    use super::*;
    use crate::shell::common::slider_row;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 120.0))),
            max_texture_side: Some(8192),
            events,
            ..Default::default()
        }
    }

    /// 数的是「轨道上出现了多少片渐变色」，不是形状总数：
    /// 渐变只有 `slider_grad` 会画，调用点哪天退回普通 `slider`，这一组色就整体消失，
    /// 而界面照常能拖——静悄悄的降级只有照色认形状才拦得住。
    fn painted_gradient_slices(grad: bool) -> Vec<Color32> {
        let stops = [theme::temp_cold(), theme::temp_warm()];
        let band: Vec<Color32> = (0..48)
            .map(|k| widgets::mix(stops[0], stops[1], k as f32 / 48.0))
            .collect();
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut value = 0.0f64;
        let mut draw = |root: &mut Ui| {
            // 渐变与否现在由规格表决定（controls::grad_stops），这里直接走
            // slider_row 复刻两条路径，钉住「渐变只有表驱动入口会画」。
            slider_row(
                root,
                "temp",
                "色温",
                &mut value,
                -100.0,
                100.0,
                0.0,
                0,
                true,
                "",
                if grad { Some(stops) } else { None },
                false,
                "",
            );
        };
        let mut out = ctx.run_ui(input(vec![]), &mut draw);
        let fills: Vec<Color32> = out
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Rect(r) if band.contains(&r.fill) => Some(r.fill),
                _ => None,
            })
            .collect();
        out.textures_delta.clear();
        fills
    }

    #[test]
    fn gradient_rail_spans_both_ends_and_flat_rail_has_none() {
        let grad = painted_gradient_slices(true);
        assert!(grad.len() >= 40, "渐变轨只铺出 {} 片，轨道断了", grad.len());
        assert_eq!(grad[0], theme::temp_cold(), "渐变轨的冷端起错色");
        assert!(
            grad[grad.len() - 1] != grad[0],
            "渐变轨两头同色，等于没渐变"
        );
        assert_eq!(
            painted_gradient_slices(false).len(),
            0,
            "普通推子的轨道上出现了渐变色"
        );
    }

    /// `--shot-group` 命中分组头时顺手展开它。
    #[test]
    fn focusing_a_group_for_a_shot_also_opens_it() {
        // 演示编辑会把没动的分组折起来，只滚不展的话拍到的是折叠着的一条头，
        // 面板里那些推子（包括渐变轨）根本没上屏。
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::new();
        state.collapsed.insert("optics");
        state.dock_focus = Some("optics".to_string());
        {
            let mut draw = |root: &mut Ui| {
                group_header(root, &mut state, "optics", "光学", true);
            };
            let mut out = ctx.run_ui(input(vec![]), &mut draw);
            out.textures_delta.clear();
        }
        assert!(!state.collapsed.contains("optics"), "滚到顶了还折着");
        assert!(state.dock_focus.is_none(), "焦点没被消费，会每帧推一次");
    }
}

#[cfg(test)]
/// 焦点组的「先对齐」提示与取帧说明的算术。
mod focus_hint_tests {
    use super::*;
    use aevum_core::align::{AlignReport, CropRect};

    /// 一张 48×48 的临时序列，够焦点组合成用。
    fn open_three_frames(state: &mut AppState, ctx: &Context, tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aevum-hint-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        for i in 0..3u32 {
            let img = image::RgbImage::from_fn(48, 48, |x, _y| {
                let v = if (x / 2).is_multiple_of(2) { 200u8 } else { 60 };
                image::Rgb([v, v, v.wrapping_add(i as u8)])
            });
            img.save(dir.join(format!("f{i:03}.png"))).expect("写帧");
        }
        state
            .run_ui_command(
                ctx,
                "sequence.open",
                &serde_json::json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        dir
    }

    /// 该提示的时候提示，不该提示的时候闭嘴：没序列、只取 2 帧、已经对齐过。
    ///
    /// The hint shows exactly when it should: no sequence, a two-frame bracket and an
    /// already-aligned sequence all stay quiet.
    #[test]
    fn the_align_hint_shows_only_for_a_real_unaligned_bracket() {
        let ctx = Context::default();
        let mut state = AppState::new();
        // 新状态：没有序列 → 什么都不说。
        assert!(state.seq.is_none(), "新状态本来就没有序列");
        assert!(focus_align_hint(&state).is_none(), "没有序列时不该提对齐");
        // 挂上序列、还没对齐、默认 around = 7（共 8 帧）→ 该提示。
        let dir = open_three_frames(&mut state, &ctx, "hint");
        let hint = focus_align_hint(&state).expect("没对齐就该提示");
        assert!(hint.contains("对齐"), "提示里得说清该干什么：{hint}");
        // 只取 2 帧：谈不上过渡带。
        state.focus_around = 1;
        assert!(focus_align_hint(&state).is_none(), "2 帧也提示，是噪音");
        // 对齐过了就别再提。
        state.focus_around = 7;
        state.align_report = Some(AlignReport {
            size: [48, 48],
            offsets: vec![[0.0, 0.0]; 3],
            scores: vec![1.0; 3],
            issues: Vec::new(),
            crop: CropRect {
                x: 0,
                y: 0,
                w: 48,
                h: 48,
            },
            elapsed_secs: 0.0,
        });
        assert!(focus_align_hint(&state).is_none(), "对齐过了还在提示");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 取帧说明必须把这个数换算成真正的帧数：前后各 N 是 2N+1，只往后 N 是 N+1。
    ///
    /// The caption must turn the knob value into the real frame count: both sides is
    /// 2N+1, forward only is N+1.
    #[test]
    fn the_span_caption_states_the_real_frame_count() {
        let both = both_sides_caption(4);
        assert!(both.contains('9'), "前后各 4 帧共 9 帧：{both}");
        assert!(!both.contains("上限"), "前后各取没有硬上限：{both}");
        let forward = forward_caption(7, aevum_core::focus::MAX_FRAMES);
        assert!(forward.contains('8'), "往后 7 帧共 8 帧：{forward}");
        assert!(
            forward.contains(&aevum_core::focus::MAX_FRAMES.to_string()),
            "说明里要带上硬上限：{forward}"
        );
        // 负向对照：把常见误读（把旋钮值当帧数）写进去就会露馅。
        //
        // Negative control: the usual misreading -- taking the knob value for the frame
        // count -- would show up right here.
        assert_ne!(
            forward_caption(7, aevum_core::focus::MAX_FRAMES),
            forward_caption(8, aevum_core::focus::MAX_FRAMES),
            "不同帧数必须给出不同说明"
        );
    }

    /// 三态状态行：只说真话、只说一次。
    ///
    /// `off` 与缺字段一律不出行（没跑不值得占一行）；`adopted` 用强调色并给出带符号的
    /// 百分比；回退用次要色；**失败必须是危险色**——把失败画成成功是这条判据守的东西。
    ///
    /// The three-state row tells the truth, once: `off` and a missing field emit nothing
    /// ("it did not run" is not worth a line); `adopted` uses the accent colour and a signed
    /// percentage; a revert uses the secondary colour; **a failure must be the danger
    /// colour** -- dressing a failure as success is exactly what this assertion guards.
    #[test]
    fn the_status_row_reports_three_states_and_stays_silent_when_off() {
        let mut state = AppState::new();
        // 没有结果 → 不出行。
        assert!(focus_align_status_row(&state).is_none(), "没结果不该出行");
        // 非焦点合并的结果 → 也不出行（同一个 `merge_result` 字段被五种合并共用）。
        //
        // A non-focus result emits nothing either: all five merge kinds share one
        // `merge_result` field.
        state.merge_result = Some(serde_json::json!({"kind": "hdr", "align_status": "adopted"}));
        assert!(
            focus_align_status_row(&state).is_none(),
            "HDR 的结果不该借用这一行"
        );

        // 夹具里**不再**放一个独立的 reason 字段：`FocusAlignReport` 早就不带它了，
        // 结果 JSON 也不发它，面板是从 `align_status` 选整句、从 `align_gain` 取数字。
        // 夹具里留着那个字段会让后来的人以为面板读它——QA 做过实验，把字段全删掉这几条
        // 判据仍然全绿，说明它一直是摆设。
        //
        // The fixtures deliberately carry **no** separate reason field: the report has not
        // carried one for a while, the result JSON never emits one, and the panel picks a
        // whole sentence from `align_status` plus a number from `align_gain`. Keeping that
        // field in the fixture would teach the next reader that the panel reads it; QA
        // deleted them and every assertion here still passed, so it was decoration.
        state.merge_result = Some(serde_json::json!({
            "kind": "focus", "align_status": "off", "align_gain": 0.0,
        }));
        assert!(focus_align_status_row(&state).is_none(), "off 不该出行");

        state.merge_result = Some(serde_json::json!({
            "kind": "focus", "align_status": "adopted", "align_gain": 1.110231,
        }));
        let (text, colour) = focus_align_status_row(&state).expect("adopted 必须出行");
        assert!(text.contains("已采用"), "文案要说清采用了：{text}");
        assert!(text.contains("+111.0%"), "增益要带符号保留一位小数：{text}");
        assert_eq!(colour, accent(), "采用是好事，用强调色");

        state.merge_result = Some(serde_json::json!({
            "kind": "focus", "align_status": "reverted_insufficient_gain",
            "align_gain": -0.018796,
        }));
        let (text, colour) = focus_align_status_row(&state).expect("回退必须出行");
        assert!(text.contains("回退"), "文案要说清回退了：{text}");
        assert!(text.contains("-1.9%"), "负增益也要带符号：{text}");
        assert_eq!(colour, dim(), "回退是中性结果，不用危险色");

        state.merge_result = Some(serde_json::json!({
            "kind": "focus", "align_status": "failed", "align_gain": 0.0,
        }));
        let (text, colour) = focus_align_status_row(&state).expect("失败必须出行");
        assert!(text.contains("失败"), "失败要直说：{text}");
        assert_eq!(colour, danger(), "失败必须是危险色，不许画成成功");
        assert_ne!(colour, accent(), "危险色不能与强调色相同");
    }
}

/// 导出进度条的显示比例：把阶段内完成度映射到该阶段占的区间（写帧 0–70%、编码
/// 70–100%）。`done` 是**阶段内**计数（写帧从 0 数到 total，编码又按 ffmpeg 回报
/// 从 1 起数），直接当全局比例会让条子在阶段切换时后退——用户会以为任务重启。
///
/// Export bar fraction: maps a per-stage done/total onto the range that stage owns
/// (frames 0-70%, encode 70-100%). Feeding the per-stage count straight into the ratio
/// makes the bar step backwards at the switch, which reads as "the job restarted".
fn export_bar_fraction(stage: aevum_core::export::Stage, done: usize, total: usize) -> f32 {
    use aevum_core::export::Stage;
    let inner = if total > 0 {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (lo, hi) = match stage {
        Stage::Frames => (0.0f32, 0.7f32),
        Stage::Encode => (0.7f32, 1.0f32),
    };
    lo + (hi - lo) * inner
}

#[cfg(test)]
/// 导出进度条的阶段区间映射：全程单调、接缝不回退、两段都走到满格。
mod export_bar_tests {
    use super::*;
    use aevum_core::export::Stage;

    #[test]
    fn the_bar_never_goes_backwards_across_the_stage_seam() {
        // 写帧走完是 70%，编码第一帧就已经越过它——旧算法在这一步从 100% 退回 17%。
        let mut last = 0.0f32;
        let mut path = Vec::new();
        for done in 0..=6 {
            let f = export_bar_fraction(Stage::Frames, done, 6);
            assert!(f >= last, "写帧阶段回退：{done} → {f} < {last}");
            last = f;
            path.push(f);
        }
        assert!((last - 0.7).abs() < 1e-6, "写帧阶段最高只到 70%：{last}");
        // 编码阶段从 ffmpeg 的 1 起数（首块通常是 frame=1），必须 ≥ 70%。
        for done in 1..=6 {
            let f = export_bar_fraction(Stage::Encode, done, 6);
            assert!(f >= last, "接缝回退：编码 {done}/6 → {f} < {last}");
            assert!(f <= 1.0, "比例越过 100%：{f}");
            last = f;
            path.push(f);
        }
        assert!((last - 1.0).abs() < 1e-6, "编码走完必须是 100%：{last}");
        // 首尾都要在 [0,1] 内，且空 total 不产生 NaN。
        assert_eq!(export_bar_fraction(Stage::Frames, 0, 0), 0.0);
        assert!(export_bar_fraction(Stage::Encode, 0, 0) >= 0.7);
        assert!(path.iter().all(|f| (0.0..=1.0).contains(f)));
    }
}
