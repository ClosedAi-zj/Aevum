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
use crate::history;
use crate::state::{AppState, Aspect, MergeKind, Module, MotionBox, Tool};
use crate::theme::{self, *};
use crate::widgets;
use aevum_core::align::Estimator;
use aevum_core::audio::AudioCodec;
use aevum_core::colorspace::ColorSpace;
use aevum_core::crop;
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
    let d = GradeParams::default();
    // 推子参数组：按规格表复位——表里登记了哪些，这里就复位哪些，
    // 与面板推子、分组圆点共用同一份事实（见 controls 模块头注释）。
    for s in controls::specs_of_group(key) {
        *controls::param_slot(&mut state.params, s.field) = s.default;
    }
    // 表外的组内字段与非推子组照旧手工复位。
    match key {
        "look" => {
            state.params.look_cube = String::new();
        }
        "levels" => {
            let p = &mut state.params;
            p.lv_black = d.lv_black;
            p.lv_white = d.lv_white;
            p.lv_gamma = d.lv_gamma;
            p.lv_out_black = d.lv_out_black;
            p.lv_out_white = d.lv_out_white;
            state.levels_grab = None;
        }
        "parametric" => {
            let p = &mut state.params;
            p.par_split1 = d.par_split1;
            p.par_split2 = d.par_split2;
            p.par_split3 = d.par_split3;
        }
        "curve" => {
            let p = &mut state.params;
            p.curve_rgb = d.curve_rgb;
            p.curve_r = d.curve_r;
            p.curve_g = d.curve_g;
            p.curve_b = d.curve_b;
            state.curve_grab = None;
        }
        "hsl" => {
            state.params.hsl_h = d.hsl_h;
            state.params.hsl_s = d.hsl_s;
            state.params.hsl_l = d.hsl_l;
        }
        "mono" => {
            state.params.mono_h = d.mono_h;
        }
        "grade" => {
            let p = &mut state.params;
            p.grade_sh_h = d.grade_sh_h;
            p.grade_sh_s = d.grade_sh_s;
            p.grade_sh_l = d.grade_sh_l;
            p.grade_mid_h = d.grade_mid_h;
            p.grade_mid_s = d.grade_mid_s;
            p.grade_mid_l = d.grade_mid_l;
            p.grade_hi_h = d.grade_hi_h;
            p.grade_hi_s = d.grade_hi_s;
            p.grade_hi_l = d.grade_hi_l;
        }
        "deflicker" => {
            state.def_window = 15.0;
            state.def_strength = 100.0;
            state.def_multi = false;
            state.def_channels = false;
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
        "basic"
            | "color"
            | "effects"
            | "look"
            | "levels"
            | "parametric"
            | "curve"
            | "hsl"
            | "mono"
            | "grade"
            | "detail"
            | "grain"
            | "lens"
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
            if state.measuring { accent() } else { text() },
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
                "单张照片没有要对齐的邻居"
            } else {
                "先打开素材"
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
fn copies_group(state: &mut AppState, ui: &mut Ui) {
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
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("当前帧前后各"))
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
            RichText::new(crate::i18n::t!("帧"))
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
                    RichText::new(if busy { "拼接中…" } else { "拼接" }).size(SIZE_CHROME),
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
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("当前帧前后各"))
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
            RichText::new(crate::i18n::t!("帧"))
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
}

/// 「时间合成」分组：天地合成或时间切片，一次把不同时刻铺进一张。
///
/// Time-blending group: sky-ground or time slice, laying several moments into one frame.
fn timeblend_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    use aevum_core::timeblend::Layout;
    let busy = state.merge_kind == Some(MergeKind::TimeBlend);
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
            RichText::new(crate::i18n::t!("从当前帧起"))
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
            RichText::new(crate::i18n::t!("帧之后"))
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
                crate::i18n::t!("合成")
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
    if group_header(ui, state, "basic", "基本", true) {
        for s in controls::specs_of_group("basic") {
            changed |= spec_slider(ui, state, s);
        }
    }
    if group_header(ui, state, "color", "色彩", true) {
        for s in controls::specs_of_group("color") {
            changed |= spec_slider(ui, state, s);
        }
    }
    if group_header(ui, state, "effects", "效果", true) {
        for s in controls::specs_of_group("effects") {
            changed |= spec_slider(ui, state, s);
        }
    }
    changed |= adjust_look(state, ui);
    changed |= adjust_levels(state, ui);
    changed |= adjust_parametric(state, ui);
    changed |= adjust_curve(state, ui);
    changed |= adjust_hsl(state, ui);
    changed |= adjust_mono(state, ui);
    changed |= adjust_grade(state, ui);
    changed |= adjust_detail(state, ui);
    changed |= adjust_grain(state, ui);
    let ctx = ui.ctx().clone();
    adjust_profiles(state, &ctx, ui);
    changed |= adjust_lens(state, ui);
    adjust_masks(state, ui);
    // 文件级工具沉到底部：第一屏先给基本/色彩，和 LR 的阅读顺序一致。
    if group_header(ui, state, "copies", "调色副本", false) {
        copies_group(state, ui);
    }
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
fn adjust_levels(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "levels", "色阶", true) {
        return false;
    }
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
fn adjust_parametric(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "parametric", "参数曲线", true) {
        return false;
    }
    let d = GradeParams::default();
    let mut changed = false;
    for s in controls::specs_of_group("parametric") {
        changed |= spec_slider(ui, state, s);
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
fn adjust_curve(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "curve", "曲线", true) {
        return false;
    }
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
    // HDR 尾部：曲线 x>1 的延伸量（对齐 LR 的 LumaHDR），挂在曲线组尾。
    let mut spec_changed = false;
    for s in controls::specs_of_group("curve") {
        spec_changed |= spec_slider(ui, state, s);
    }
    moved || changed || spec_changed
}

/// HSL：8 个色相带各自的色相 / 饱和度 / 明亮度，一次调一条。
fn adjust_hsl(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "hsl", "色相 / 饱和度 / 明亮度", true) {
        return false;
    }
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
fn adjust_mono(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "mono", "黑白混合", true) {
        return false;
    }
    let mut changed = false;
    for s in controls::specs_of_group("mono") {
        changed |= spec_slider(ui, state, s);
    }
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

/// 颗粒：强度 / 大小 / 密度，噪声种子固定，同一帧每次都一样。
fn adjust_grain(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "grain", "颗粒", true) {
        return false;
    }
    let mut changed = false;
    for s in controls::specs_of_group("grain") {
        changed |= spec_slider(ui, state, s);
    }
    changed
}

/// Adobe 配置：DCP 相机配置接管 RAW 解码后的颜色，LCP 镜头配置按 EXIF 选帧。
///
/// 两行都走 `profile.set`，和 MCP 同一条校验路径：机型/镜头对不上就在面板里直说。
fn adjust_profiles(state: &mut AppState, ctx: &Context, ui: &mut Ui) {
    if !group_header(ui, state, "profiles", "相机与镜头配置", true) {
        return;
    }
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

/// 镜头校正：畸变、暗角补偿、去紫边；三项都是空间操作，导出同样生效。
/// 顶上一行是机型匹配：EXIF 对 lensfun 数据库折出的标定系数先走一步。
fn adjust_lens(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "lens", "镜头校正", true) {
        return false;
    }
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
    for s in controls::specs_of_group("lens") {
        changed |= spec_slider(ui, state, s);
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

fn adjust_render(state: &mut AppState, ui: &mut Ui) {
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
                let frac = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
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
        state.collapsed.insert("color");
        state.dock_focus = Some("color".to_string());
        {
            let mut draw = |root: &mut Ui| {
                group_header(root, &mut state, "color", "色彩", true);
            };
            let mut out = ctx.run_ui(input(vec![]), &mut draw);
            out.textures_delta.clear();
        }
        assert!(!state.collapsed.contains("color"), "滚到顶了还折着");
        assert!(state.dock_focus.is_none(), "焦点没被消费，会每帧推一次");
    }
}
