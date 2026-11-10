//! 信息/快捷键/AI/历史四个页签。
//!
//! The four tabs: info, shortcuts, AI, history.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::common::bar_button_on;
use super::common::group_header;
use super::common::kv;
use super::common::note;
use super::toolbar::bar_button;
use super::toolbar::bar_primary;
use crate::control;
use crate::history;
use crate::icons::{self, Glyph};
use crate::state::{AiLogEntry, AppState};
use crate::theme::{self, *};
use egui::{
    Align2, Color32, Context, Key, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

// ---------------------------------------------------------------- 信息页

pub(super) fn info_tab(state: &mut AppState, ui: &mut Ui) {
    if group_header(ui, state, "info-frame", "当前帧", false) {
        let n = state.frame_count();
        if n > 0 {
            let name = state
                .seq
                .as_ref()
                .and_then(|s| s.frames.get(state.selected))
                .map(|f| f.name.clone())
                .unwrap_or_default();
            kv(
                ui,
                "帧号",
                format!("{:04} / {:04}", state.selected + 1, n),
                text(),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(name)
                        .size(SIZE_LABEL)
                        .monospace()
                        .color(muted()),
                )
                .wrap()
                .selectable(false),
            );
            ui.add_space(2.0);
            // RAW 的两条解码路保真度差一档，必须写在脸上，不能让预览冒充传感器数据。
            if let Some(note) = &state.source_note {
                kv(ui, "来源", note.clone(), muted());
            }
            if let Some(m) = state.metrics.get(state.selected).copied().flatten() {
                kv(ui, "亮度", format!("{:.1}", m.mean * 255.0), text());
                kv(ui, "中位", format!("{:.1}", m.median * 255.0), muted());
                kv(ui, "中心", format!("{:.1}", m.center * 255.0), muted());
                kv(ui, "锐度", format!("{:.0}", m.sharp), muted());
            } else if state.measuring.is_some() {
                ui.label(
                    RichText::new(crate::i18n::t!("测光中…"))
                        .size(SIZE_SMALL)
                        .color(accent()),
                );
            }
            let off = state.offset_at(state.selected);
            kv(
                ui,
                "去闪修正",
                format!("{off:+.2} EV"),
                if off.abs() > 0.02 { accent() } else { dim() },
            );
            let rating = state.selected_rating();
            kv(
                ui,
                "星级",
                if rating > 0 {
                    "★".repeat(rating as usize)
                } else {
                    "—".to_owned()
                },
                if rating > 0 { star() } else { dim() },
            );
        } else {
            ui.label(
                RichText::new(crate::i18n::t!("尚未打开序列"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        }
    }
    if group_header(ui, state, "info-capture", "拍摄参数", false) {
        match state.exif.get(state.selected) {
            Some(e) if e != &aevum_core::exif::Exif::default() => {
                if let Some(line) = e.camera_line() {
                    ui.add(
                        egui::Label::new(RichText::new(line).size(SIZE_SMALL).color(text()))
                            .wrap()
                            .selectable(false),
                    );
                }
                kv(
                    ui,
                    "ISO",
                    e.iso().map_or("—".to_owned(), |v| format!("{v:.0}")),
                    text(),
                );
                kv(
                    ui,
                    "快门",
                    e.shutter_text().unwrap_or_else(|| "—".to_owned()),
                    text(),
                );
                kv(
                    ui,
                    "光圈",
                    e.fnum().map_or("—".to_owned(), |v| format!("f/{v:.1}")),
                    text(),
                );
                kv(
                    ui,
                    "焦距",
                    e.focal_mm()
                        .map_or("—".to_owned(), |v| format!("{v:.0} mm")),
                    muted(),
                );
                match e.ev() {
                    Some(v) => kv(ui, "曝光 EV", format!("{v:+.2}"), accent()),
                    None => kv(ui, "曝光 EV", "三要素不全", dim()),
                }
                if let Some(t) = e.datetime() {
                    kv(ui, "拍摄时间", t.to_owned(), muted());
                }
                if e.orientation != 1 {
                    kv(
                        ui,
                        "原始方向",
                        crate::i18n::tr_fmt("{} · 已转正", &[&e.orientation]),
                        muted(),
                    );
                }
            }
            _ => {
                ui.label(
                    RichText::new(crate::i18n::t!(
                        "这个文件没有可读的 EXIF（PNG、处理过的图，或机身只写了预览）"
                    ))
                    .size(SIZE_SMALL)
                    .color(dim()),
                );
            }
        }
    }
    if group_header(ui, state, "info-meta", "说明与版权", false) {
        if state.seq.is_none() {
            ui.label(
                RichText::new(crate::i18n::t!("尚未打开序列"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        } else {
            meta_editor(ui, &mut state.meta);
        }
    }
    if group_header(ui, state, "info-print", "曝光指纹", false) {
        sparkline(ui, state);
    }
    if group_header(ui, state, "info-diag", "诊断", false) {
        if let Some(a) = &state.analysis {
            let st = a.stats;
            kv(
                ui,
                "抖动 P95",
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
                    "色闪",
                    format!("{:.3} → {:.3} EV", st.color_before, st.color_after),
                    text(),
                );
            }
            kv(
                ui,
                "最大 / 均值",
                format!("{:+.2} / {:.3} EV", st.max_ev, st.mean_abs_ev),
                muted(),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(if st.jitter_after > 0.02 {
                        crate::i18n::t!("仍有可见抖动：试试加大窗口或开多遍")
                    } else {
                        crate::i18n::t!("序列亮度已平整")
                    })
                    .size(SIZE_SMALL)
                    .color(if st.jitter_after > 0.02 {
                        danger()
                    } else {
                        ok()
                    }),
                )
                .wrap(),
            );
        } else {
            ui.label(
                RichText::new(crate::i18n::t!("等待测光…"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        }
    }
    if group_header(ui, state, "info-engine", "引擎", false) {
        kv(ui, "渲染", "Rust · aevum-core", muted());
        kv(ui, "金标", "15/15 · 1e-4", ok());
        // RAW 后端降级记录：rawler 解不动退内置时留一句话，支持时不用翻日志。
        if let Some(note) = aevum_core::raw::backend::last_fallback_note() {
            kv(ui, "RAW 降级", theme::ellipsis(&note, 26), danger());
        }
        // 去马赛克方法：解码侧配置，不进影调预设、不落盘；只影响传感器直解
        // （机身内嵌预览与缩略图不走去马赛克）。切换即重新解码当前帧。
        let current = aevum_core::raw::demosaic_method();
        kv(ui, "去马赛克", demosaic_cn(current), text());
        ui.horizontal(|ui| {
            for name in aevum_core::raw::DEMOSAIC_METHODS {
                let label = demosaic_cn(name);
                let resp = ui
                    .add(egui::Button::selectable(
                        current == name,
                        crate::i18n::tr(label),
                    ))
                    .on_hover_text(if name == "directional" {
                        crate::i18n::t!(
                            "Hamilton–Adams 方向自适应插值（对齐 LR 的处理口径）：边缘更干净，约为单元平均的 3-4×耗时。\n只影响传感器直解；切换后当前帧重新解码，导出走同一口径。"
                        )
                    } else {
                        crate::i18n::t!(
                            "2×2 单元平均（与 DNG 路径同口径），最快；边缘处 R/B 易混色。\n切换后当前帧重新解码。"
                        )
                    });
                if resp.clicked() && aevum_core::raw::set_demosaic_method(name) {
                    state.preview_dirty = true;
                    state.status = crate::i18n::tr_fmt("去马赛克已切换：{}", &[&label]);
                }
            }
        });
        if let Some(err) = &state.error {
            ui.add(egui::Label::new(RichText::new(err).size(SIZE_SMALL).color(danger())).wrap());
        }
        ui.add(
            egui::Label::new(
                RichText::new(state.status.clone())
                    .size(SIZE_SMALL)
                    .color(dim()),
            )
            .wrap()
            .selectable(false),
        );
    }
}

/// 去马赛克方法的中文名（面板与提示共用一份，注册表换名只改这里）。
fn demosaic_cn(name: &str) -> &'static str {
    match name {
        "directional" => "方向自适应",
        _ => "单元平均",
    }
}

/// 说明与版权的四个输入框：序列「信息」页与图库信息面板共用同一套字段、提示与
/// 归一化口径（存的时候都走 `Metadata::cleaned`，两边看到的值不会不一样）。
pub(super) fn meta_editor(ui: &mut Ui, meta: &mut aevum_core::project::Metadata) {
    meta_field(ui, "标题", "云海日出", &mut meta.title, 1);
    meta_field(
        ui,
        "说明",
        "内容、地点、间隔；换行会折成空格",
        &mut meta.description,
        3,
    );
    meta_field(ui, "创作者", "署名", &mut meta.creator, 1);
    meta_field(ui, "版权", "© 2026 张三", &mut meta.copyright, 1);
}

/// 一格元数据：小标题在上、输入框占满宽度。`rows` 为 1 时用单行框，否则多行。
fn meta_field(ui: &mut Ui, label: &str, hint: &str, value: &mut String, rows: usize) {
    let label = crate::i18n::tr(label);
    let hint = crate::i18n::tr(hint);
    ui.label(RichText::new(label).size(SIZE_LABEL).color(muted()));
    ui.add_space(2.0);
    let edit = if rows <= 1 {
        egui::TextEdit::singleline(value)
    } else {
        egui::TextEdit::multiline(value).desired_rows(rows)
    };
    ui.add(
        edit.hint_text(hint)
            .desired_width(f32::INFINITY)
            .font(mono(SIZE_LABEL)),
    );
    ui.add_space(6.0);
}

fn sparkline(ui: &mut Ui, state: &AppState) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 54.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0, surround());
    p.rect_stroke(rect, 0, Stroke::new(1.0, divider()), StrokeKind::Inside);
    let n = state.frame_count();
    if n == 0 {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "无数据",
            font(SIZE_SMALL),
            dim(),
        );
        return;
    }
    let inner = rect.shrink2(Vec2::new(5.0, 5.0));
    for k in 1..4 {
        let y = inner.top() + inner.height() * k as f32 / 4.0;
        p.line_segment(
            [Pos2::new(inner.left(), y), Pos2::new(inner.right(), y)],
            Stroke::new(1.0, divider()),
        );
    }
    let valid: Vec<(usize, f64)> = state
        .metrics
        .iter()
        .enumerate()
        .filter_map(|(i, m)| m.map(|m| (i, m.mean)))
        .collect();
    if valid.is_empty() {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "测光中…",
            font(SIZE_SMALL),
            accent(),
        );
        return;
    }
    let lo = valid
        .iter()
        .map(|(_, v)| *v)
        .fold(f64::INFINITY, f64::min)
        .clamp(0.0, 1.0);
    let hi = valid
        .iter()
        .map(|(_, v)| *v)
        .fold(f64::NEG_INFINITY, f64::max)
        .clamp(0.0, 1.0);
    let span = (hi - lo).max(0.02);
    let (lo, hi) = ((lo - span * 0.15).max(0.0), (hi + span * 0.15).min(1.0));
    let x_at =
        |i: usize| inner.left() + inner.width() * i as f32 / n.max(2).saturating_sub(1) as f32;
    let y_at = |v: f64| inner.bottom() - inner.height() * ((v - lo) / (hi - lo).max(1e-6)) as f32;
    let pts: Vec<Pos2> = valid
        .iter()
        .map(|(i, v)| Pos2::new(x_at(*i), y_at(*v)))
        .collect();
    if pts.len() > 1 {
        p.add(egui::Shape::line(
            pts,
            Stroke::new(1.1, Color32::from_rgb(0xB8, 0xB8, 0xB8)),
        ));
    }
    let x = x_at(state.selected);
    p.line_segment(
        [Pos2::new(x, inner.top()), Pos2::new(x, inner.bottom())],
        Stroke::new(1.0, accent()),
    );
}

// ---------------------------------------------------------------- 关键帧页

pub(super) fn keys_tab(state: &mut AppState, ui: &mut Ui) {
    if group_header(ui, state, "keys", "关键帧", false) {
        if bar_button(ui, "记录当前参数").clicked() {
            state.add_keyframe();
        }
        let items: Vec<usize> = state.keyframes.keys().copied().collect();
        if items.is_empty() {
            note(ui, "还没有关键帧。调好一帧后点上面的按钮记录参数。");
        }
        for i in items {
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
            let p = ui.painter();
            if i == state.selected {
                p.rect_filled(rect, 0, accent_dim());
            } else if resp.hovered() {
                p.rect_filled(rect, 0, elev());
            }
            let c = Pos2::new(rect.left() + 8.0, rect.center().y);
            let d = 3.8;
            p.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(c.x, c.y - d),
                    Pos2::new(c.x + d, c.y),
                    Pos2::new(c.x, c.y + d),
                    Pos2::new(c.x - d, c.y),
                ],
                star(),
                Stroke::NONE,
            ));
            p.text(
                Pos2::new(rect.left() + 18.0, rect.center().y),
                Align2::LEFT_CENTER,
                crate::i18n::tr_fmt("帧 {}", &[&format!("{:04}", i + 1)]),
                font(SIZE_SMALL),
                text(),
            );
            let xr = Rect::from_center_size(
                Pos2::new(rect.right() - 10.0, rect.center().y),
                Vec2::splat(16.0),
            );
            let x_resp = ui.interact(xr, ui.make_persistent_id(("kf-x", i)), Sense::click());
            icons::paint(
                p,
                xr.shrink(4.0),
                Glyph::Close,
                if x_resp.hovered() { danger() } else { muted() },
            );
            if resp.clicked() {
                state.goto_keyframe(i);
            }
            if x_resp.clicked() {
                state.remove_keyframe(i);
            }
        }
    }
}

// ---------------------------------------------------------------- AI 页

pub(super) fn ai_tab(state: &mut AppState, ui: &mut Ui) {
    let ctx = ui.ctx().clone();

    if group_header(ui, state, "ai-console", "本机命令控制台", false) {
        ui.add(
            egui::Label::new(
                RichText::new(crate::i18n::t!(
                    "只控制本机正在运行的 Aevum Studio：不联网、不调用任何模型；模型接入放在后续里程碑。"
                ))
                .size(SIZE_SMALL)
                .color(dim()),
            )
            .wrap(),
        );
        ui.add_space(6.0);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut state.ai_input)
                .hint_text(crate::i18n::t!(
                    "sequence.info　或　grade.set {\"exposure\":0.5}"
                ))
                .desired_width(f32::INFINITY)
                .font(mono(SIZE_SMALL)),
        );
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let mut run = enter;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if bar_primary(ui, "执行").clicked() {
                run = true;
            }
            if bar_button(ui, "填一条示例").clicked() {
                state.ai_input = "grade.set {\"exposure\": 0.5}".to_owned();
            }
            if bar_button(ui, "清空记录").clicked() {
                state.ai_log.clear();
            }
        });
        if run {
            let line = state.ai_input.clone();
            let id = line.split_whitespace().next().unwrap_or("").to_owned();
            match state.run_ai_line(&ctx, &line) {
                Ok(_) => state.status = crate::i18n::tr_fmt("命令已执行：{}", &[&id]),
                Err(e) => state.status = crate::i18n::tr_fmt("命令失败：{}", &[&e]),
            }
        }
        note(ui, "格式：命令 ID + 可选 JSON 参数。");
    }

    if group_header(ui, state, "ai-log", "最近执行", false) {
        if state.ai_log.is_empty() {
            ui.label(
                RichText::new(crate::i18n::t!("还没有执行过命令。"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        }
        let entries: Vec<AiLogEntry> = state.ai_log.iter().rev().cloned().collect();
        for entry in entries {
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
            let p = ui.painter().clone();
            p.text(
                Pos2::new(rect.left(), rect.center().y),
                Align2::LEFT_CENTER,
                if entry.ok { "✓" } else { "✗" },
                font(SIZE_SMALL),
                if entry.ok { ok() } else { danger() },
            );
            p.text(
                Pos2::new(rect.left() + 15.0, rect.center().y),
                Align2::LEFT_CENTER,
                theme::ellipsis(&entry.input, 34),
                mono(SIZE_SMALL),
                if entry.ok { text() } else { danger() },
            );
            ui.add(
                egui::Label::new(
                    RichText::new(entry.detail)
                        .size(SIZE_SMALL)
                        .monospace()
                        .color(if entry.ok { muted() } else { danger() }),
                )
                .wrap()
                .selectable(false),
            );
            ui.add_space(4.0);
        }
    }

    if group_header(ui, state, "ai-link", "回环控制", false) {
        // 先把展示数据拷出来，后面改 status 时不再借着 state 的不可变借用。
        let link = state.control_file().cloned().zip(state.control_path());
        match link {
            Some((file, path)) => {
                kv(ui, "地址", format!("127.0.0.1:{}", file.port), ok());
                kv(ui, "进程号", format!("{}", file.pid), muted());
                if !file.root.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(crate::i18n::tr_fmt(
                                "序列 {}",
                                &[&theme::ellipsis(&file.root, 38)],
                            ))
                            .size(SIZE_SMALL)
                            .color(dim()),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(2.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(path.display().to_string())
                            .size(SIZE_SMALL)
                            .monospace()
                            .color(muted()),
                    )
                    .wrap()
                    .selectable(false),
                );
                let cmd = control::attach_command(&path);
                let config = control::attach_config(&path);
                let path_text = path.display().to_string();
                ui.add(
                    egui::Label::new(
                        RichText::new(cmd.clone())
                            .size(SIZE_SMALL)
                            .monospace()
                            .color(dim()),
                    )
                    .wrap()
                    .selectable(false),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(config.clone())
                            .size(SIZE_SMALL)
                            .monospace()
                            .color(muted()),
                    )
                    .wrap()
                    .selectable(false),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if bar_button(ui, "复制 MCP 配置").clicked() {
                        ctx.copy_text(config.clone());
                        state.status = "已复制 MCP 配置片段".to_owned();
                    }
                    if bar_button(ui, "复制附着命令").clicked() {
                        ctx.copy_text(cmd.clone());
                        state.status = "已复制 --mcp --attach 命令".to_owned();
                    }
                    if bar_button(ui, "复制路径").clicked() {
                        ctx.copy_text(path_text.clone());
                        state.status = "已复制 control.json 路径".to_owned();
                    }
                });
                note(ui, "令牌只写入 .aevum/control.json，界面不显示。");
            }
            None => {
                note(ui, "控制服务未启动，先打开序列。");
            }
        }
    }

    if group_header(ui, state, "ai-cmds", "命令清单", false) {
        for spec in control::command_catalog() {
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::click());
            let p = ui.painter().clone();
            if resp.hovered() {
                p.rect_filled(rect, 0, elev());
            }
            p.text(
                Pos2::new(rect.left() + 2.0, rect.center().y),
                Align2::LEFT_CENTER,
                spec.id,
                mono(SIZE_SMALL),
                text(),
            );
            p.text(
                Pos2::new(rect.left() + 104.0, rect.center().y),
                Align2::LEFT_CENTER,
                theme::ellipsis(crate::i18n::tr(spec.summary), 20),
                font(SIZE_SMALL),
                dim(),
            );
            if resp.clicked() {
                state.ai_input = format!("{} ", spec.id);
            }
            // 截断要落在翻译之后：反过来会把中文按字数截断再翻译，对不上键。
            resp.on_hover_text(crate::i18n::tr_fmt(
                "{}\n参数：{}",
                &[
                    &crate::i18n::tr(spec.summary),
                    &crate::i18n::tr(spec.params),
                ],
            ));
        }
        note(ui, "点击命令名可填进输入框；参数省略即 {}。");
    }
}

// ---------------------------------------------------------------- 历史页

/// 撤销 / 重做 / 跳步的统一入口：错误落进状态栏，成功由命令自己写 status。
pub(super) fn run_history_command(
    state: &mut AppState,
    ctx: &Context,
    id: &str,
    args: &serde_json::Value,
) {
    if let Err(e) = state.run_ui_command(ctx, id, args) {
        state.error = Some(e);
    }
}

pub(super) fn history_tab(state: &mut AppState, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let p = ui.painter().clone();
    theme::rule(
        &p,
        ui.cursor().left(),
        ui.cursor().right(),
        ui.cursor().top(),
        border(),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if bar_button_on(ui, "撤销", state.history.can_undo(), "Ctrl+Z").clicked() {
            run_history_command(state, &ctx, "history.undo", &serde_json::json!({}));
        }
        if bar_button_on(ui, "重做", state.history.can_redo(), "Ctrl+Shift+Z").clicked() {
            run_history_command(state, &ctx, "history.redo", &serde_json::json!({}));
        }
    });
    ui.add_space(8.0);

    // 先把行拷出来：下面点行要 &mut state，不能一边借着 steps() 一边改。
    let rows: Vec<(usize, String, bool)> = state
        .history
        .steps()
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.label.clone(), s.revertible))
        .collect();
    let cursor = state.history.cursor();
    if rows.len() <= 1 {
        note(ui, "尚无改动记录。");
        return;
    }
    for (i, label, revertible) in rows {
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::click());
        let p = ui.painter();
        let applied = i == cursor;
        if applied {
            p.rect_filled(rect, radius_control(), selected());
        } else if resp.hovered() {
            p.rect_filled(rect, radius_control(), elev());
        }
        p.text(
            Pos2::new(rect.left() + 6.0, rect.center().y),
            Align2::LEFT_CENTER,
            format!("{}", i + 1),
            mono(SIZE_SMALL),
            if applied { muted() } else { dim() },
        );
        p.text(
            Pos2::new(rect.left() + 24.0, rect.center().y),
            Align2::LEFT_CENTER,
            theme::ellipsis(&label, 26),
            font(SIZE_SMALL),
            if applied { Color32::WHITE } else { text() },
        );
        if !revertible {
            p.text(
                Pos2::new(rect.right() - 6.0, rect.center().y),
                Align2::RIGHT_CENTER,
                "外部",
                font(SIZE_SMALL),
                if applied { muted() } else { dim() },
            );
        }
        if resp.clicked() {
            let args = serde_json::json!({ "index": i + 1 });
            run_history_command(state, &ctx, "history.jump", &args);
        }
        resp.on_hover_text(if revertible {
            crate::i18n::t!("点这一步就回到它做完之后的状态")
        } else {
            crate::i18n::t!(
                "这一步有已经落到盘上的产物（导出、对齐、存工程）：跳回去只回参数，产物不会删除"
            )
        });
    }
    note(
        ui,
        &crate::i18n::tr_fmt(
            "内存中保留最近 {} 步，换序列后重新开始。",
            &[&history::HISTORY_LIMIT],
        ),
    );
}
