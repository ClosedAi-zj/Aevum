//! 序列页 / 去闪烁页 / 调色副本组，以及它们自己的单测。
//!
//! The sequence page, the deflicker page and the copy group, plus their unit criteria.

// 共用件（theme / i18n / 兄弟模块的函数）由 mod.rs 的 use 树统一带入。
//
// Shared pieces (theme, i18n, sibling helpers) come in through mod.rs's use tree.
use super::*;
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

pub(super) fn adjust_sequence(state: &mut AppState, ui: &mut Ui) {
    if group_header(ui, state, "seq", "序列", false) {
        let n = state.frame_count();
        let valid = state.metrics.iter().filter(|m| m.is_some()).count();
        kv_dense(
            ui,
            "帧数",
            format!("{n}"),
            if n > 0 { text() } else { dim() },
        );
        kv_dense(
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
        kv_dense(ui, "分辨率", res, text());
        kv_dense(
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
            // 复制在飞时按钮禁用：GB 级整批复制再点一遍等于两路线程写同一个目录。
            //
            // The button is disabled while a copy is in flight: clicking again mid-copy means
            // two threads writing the same folder.
            let export = ui
                .add_enabled_ui(!state.cull_export_busy, |ui| {
                    bar_primary(
                        ui,
                        if state.cull_export_busy {
                            "导出中…"
                        } else {
                            "导出子序列"
                        },
                    )
                    .on_hover_text(crate::i18n::t!(
                        "把没被拒收的帧复制成一条新序列：保留 mtime、测光缓存一并带走；开发参数属于源工程、不跟过去"
                    ))
                    .clicked()
                })
                .inner;
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
                kv_dense(ui, "结论", "没发现问题", ok());
            } else {
                kv_dense(
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
            kv_dense(
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
            kv_dense(
                ui,
                "最大位移",
                format!("X {mx:.1} px · Y {my:.1} px"),
                text(),
            );
            let low = report.scores.iter().filter(|s| **s < 0.35).count();
            kv_dense(
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
        kv_dense(ui, "测光长边", "512 px", muted());
        kv_dense(ui, "去闪输入", "全帧均值", muted());
        kv_dense(ui, "预览长边", "1600 px", muted());
    }
}

pub(super) fn adjust_deflicker(state: &mut AppState, ui: &mut Ui) {
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
            kv_dense(
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
                kv_dense(
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
            kv_dense(ui, "最大修正", format!("{:+.2} EV", st.max_ev), text());
            kv_dense(ui, "平均修正", format!("{:.3} EV", st.mean_abs_ev), text());
            kv_dense(
                ui,
                "有效帧",
                format!("{} / {}", st.valid, st.frames),
                muted(),
            );
            let moved = state.corr.iter().filter(|v| v.abs() > 0.05).count();
            kv_dense(ui, "受影响帧", format!("{moved}"), muted());
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

#[cfg(test)]
mod qa_panel_tests {
    use super::*;
    use crate::shell::ui;

    /// 画布高度 1400：面板行距按 ACR 口径（两行式 46 + 行距），体检组排在
    /// 序列面板末尾，1000px 视口下最后一条落在折叠线外，被裁剪后不上屏。
    /// 实际窗口里右栏有 ScrollArea，往下滚就能看到；测试要断言四条都画出来，
    /// 就得给一个能容纳整栏的视口。
    ///
    /// 1400 px canvas: panel rows use the ACR two-line pitch (46 + spacing), and the
    /// QA group sits at the tail of the Sequence panel -- at a 1000 px viewport the
    /// last finding falls past the fold and is culled. The real dock scrolls, but the
    /// test asserts all four rows are painted, so the viewport must hold the column.
    const VIEW: Vec2 = Vec2::new(1600.0, 1400.0);

    fn paint_count(ctx: &Context, state: &mut AppState) -> usize {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, VIEW)),
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
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, VIEW)),
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
