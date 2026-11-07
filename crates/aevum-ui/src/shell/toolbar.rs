//! 选项栏/文档页签/工具轨/状态栏（界面顶部与左右竖条）。
//!
//! Options bar, document tabs, tool rail, status bar (the top strip and the left and
//! right rails).

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::BAR_H;
use super::common::combo_style;
use super::common::group_gap;
use super::common::in_field;
use super::common::opt_sep;
use super::dialogs::choose_audio;
use super::dialogs::open_dialog;
use super::menu::actual_size;
use super::menu::fit_view;
use super::menu::menu_item;
use super::menu::menu_style;
use crate::icons::{self, Glyph};
use crate::state::{AppState, Aspect, Module, Tool};
use crate::theme::{self, *};
use aevum_core::export::OutputMode;
use aevum_core::grade::{self, GradeParams};
use egui::{
    Align, Align2, Color32, Context, Layout, Pos2, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

/// 普通工具栏按钮：底色交给 theme 的 widget 层级（ELEV → 悬停 ELEV_HOVER），
/// 所以这里不写死 fill，才有悬停反馈。
pub(super) fn bar_button(ui: &mut Ui, label: &str) -> Response {
    // 翻译下沉到辅助函数：调用点照常写中文，切到英文时这里统一翻。
    let label = crate::i18n::tr(label);
    ui.add(
        egui::Button::new(RichText::new(label).size(SIZE_BODY))
            .min_size(Vec2::new(0.0, BAR_H))
            .corner_radius(radius_control()),
    )
}

/// 主按钮：强调色实心 + 深字，悬停提亮、按下压暗，圆角走 token。
pub(super) fn bar_primary(ui: &mut Ui, label: &str) -> Response {
    let label = crate::i18n::tr(label);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(SIZE_BODY), on_accent());
    let size = Vec2::new(galley.size().x + 22.0, BAR_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.dragged() {
        accent_dim()
    } else if resp.hovered() {
        accent_hover()
    } else {
        accent()
    };
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(
            rect.translate(Vec2::new(0.0, 1.0)).expand(1.0),
            radius_control(),
            Color32::from_black_alpha(60),
        );
    }
    p.rect_filled(rect, radius_control(), fill);
    p.galley(rect.center() - galley.size() * 0.5, galley, on_accent());
    resp
}

pub(super) fn bar_chip(ui: &mut Ui, label: &str, active: bool) -> Response {
    let label = crate::i18n::tr(label);
    let w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(SIZE_LABEL), text())
        .size()
        .x
        + 15.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, BAR_H - 2.0), Sense::hover());
    theme::paint_chip(ui.painter(), ui, rect, label, active, accent())
}

/// 与 [`bar_chip`] 同款，但**不走翻译**：给语言自称这类「原文即正确显示」的标签用。
///
/// Same look as [`bar_chip`] but **without translation**, for labels whose own spelling is
/// the correct rendering (language endonyms and the like).
pub(super) fn bar_chip_verbatim(ui: &mut Ui, label: &str, active: bool) -> Response {
    let w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(SIZE_LABEL), text())
        .size()
        .x
        + 15.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, BAR_H - 2.0), Sense::hover());
    theme::paint_chip(ui.painter(), ui, rect, label, active, accent())
}

/// 选项栏里的数值输入（对齐 .fld .v）：压掉 DragValue 自带的底框与字色，
/// 读数走 value 蓝 —— 数值是"读数"，不是按钮，不该长得像按钮。字族统一走
/// `mono()`：同一行里所有数值都是等宽，滑窗 / 百分比 / 帧率一眼能对齐。
///
/// Numeric inputs in the options bar (.fld .v): strip DragValue's own frame and
/// colour so readouts take the value blue -- a number is a readout, not a button. The
/// font family is pinned to `mono()` so every number on a row is monospace and aligns.
fn bar_value<R>(ui: &mut Ui, run: impl FnOnce(&mut Ui) -> R) -> R {
    let old_color = ui.style().visuals.override_text_color;
    let old_font = ui.style().override_font_id.clone();
    ui.style_mut().visuals.override_text_color = Some(value());
    ui.style_mut().override_font_id = Some(mono(SIZE_BODY));
    let out = in_field(ui, Color32::TRANSPARENT, Color32::TRANSPARENT, run);
    ui.style_mut().visuals.override_text_color = old_color;
    ui.style_mut().override_font_id = old_font;
    out
}

/// 上下文徽标（界面稿 .ctx）：粗体名字 + field 底的小圆角徽标。
/// 徽标是"发生了什么"的旁注，SIZE_LABEL dim；warn 不为空时追加一枚 danger 色的
/// 计数（缺帧这类既该看见又不该抢戏的信号）。
fn ctx_seg(ui: &mut Ui, name: &str, note: &str, warn: Option<String>) {
    let name = crate::i18n::tr(name);
    let note = crate::i18n::tr(note);
    ui.label(RichText::new(name).size(SIZE_LABEL).color(text()).strong());
    ui.add_space(2.0);
    let badge = |ui: &mut Ui, text: String, ink: Color32| {
        let galley = ui.painter().layout_no_wrap(text, font(SIZE_LABEL), ink);
        let size = galley.size() + Vec2::new(10.0, 4.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        ui.painter().rect_filled(rect, 3.0, field());
        ui.painter().galley(
            Pos2::new(rect.left() + 5.0, rect.center().y - galley.size().y * 0.5),
            galley,
            ink,
        );
    };
    badge(ui, note.to_owned(), dim());
    if let Some(w) = warn {
        ui.add_space(4.0);
        badge(ui, w, danger());
    }
}

/// 文档页签：文件名 + 修改圆点 + 关闭叉，画在画布顶栏左端（对齐 .doctab）。
///
/// 上圆角、贴着顶栏底线 —— PS 的做法：文件名从标题栏搬回画布正上方，
/// 标题栏才腾得出来只放菜单。
pub(super) fn doc_tab(
    state: &mut AppState,
    p: &egui::Painter,
    ui: &mut Ui,
    ctx: &Context,
    rect: Rect,
) {
    let has_seq = state.seq.is_some();
    let resp = ui.interact(rect, ui.make_persistent_id("doc-tab"), Sense::click());
    let fill = theme::overlay_white(header(), if has_seq { 20 } else { 14 });
    p.rect_filled(
        rect,
        egui::CornerRadius {
            nw: 4,
            ne: 4,
            sw: 0,
            se: 0,
        },
        fill,
    );
    let name = state
        .seq
        .as_ref()
        .map(|s| s.display_name())
        .unwrap_or_else(|| "未打开序列".to_owned());
    let ink = if has_seq { text() } else { muted() };
    // 名字按可用像素宽度截：右边留着修改点与关闭叉的位置，中文名不再压到叉上。
    let title = theme::fit_text(p, &name, font(SIZE_LABEL), ink, rect.width() - 46.0);
    let name_g = p.layout_no_wrap(title.clone(), font(SIZE_LABEL), ink);
    p.text(
        Pos2::new(rect.left() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        font(SIZE_LABEL),
        ink,
    );
    if has_seq {
        // 修改圆点：参数偏离默认（有未保存的调整）才亮。
        if state.params != GradeParams::default() {
            p.circle_filled(
                Pos2::new(rect.left() + 9.0 + name_g.size().x + 7.0, rect.center().y),
                2.5,
                muted(),
            );
        }
        let close = Rect::from_center_size(
            Pos2::new(rect.right() - 12.0, rect.center().y),
            Vec2::splat(15.0),
        );
        let cr = ui.interact(close, ui.make_persistent_id("doc-close"), Sense::click());
        if cr.hovered() {
            p.rect_filled(close, 3.0, theme::overlay_white(header(), 26));
        }
        // 裸「✕」+ dim() 曾经让用户看不出这是什么、点了会怎样（「x 也看不懂」）。
        // 换成有语义的关闭图标、默认态抬到 muted()、悬停说明动作。
        //
        // A bare "✕" in dim() left the user unable to tell what it was or what it would do.
        // Use a semantic close glyph, lift the default state to muted(), and explain on hover.
        icons::paint(
            p,
            Rect::from_center_size(close.center(), Vec2::splat(11.0)),
            Glyph::Close,
            if cr.hovered() { text() } else { muted() },
        );
        if cr.clicked() {
            state.close();
        }
        cr.on_hover_text(crate::i18n::t!("关闭当前文档"));
    } else if resp.clicked() {
        open_dialog(state, ctx);
    }
}

pub(super) fn options_bar(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let full = ui.max_rect();
    theme::rule(
        ui.painter(),
        full.left(),
        full.right(),
        full.bottom(),
        border(),
    );

    // 文档页签贴在选项行左端、压着下缘线：文件名回答的是"这一行在编辑哪个文档"，
    // 属于选项行，不属于图像；搬下来之后画布整块还给画面。
    //
    // The document tab sits at the options bar's left end on its bottom rule: the file
    // name answers "which document is this row editing", so it belongs here, not to the
    // image -- and the canvas gets the whole block back.
    let tab_rect = Rect::from_min_size(
        Pos2::new(full.left() + 8.0, full.bottom() - theme::TL_HD_H),
        Vec2::new(208.0, theme::TL_HD_H),
    );
    let tab_painter = ui.painter().clone();
    doc_tab(state, &tab_painter, ui, ctx, tab_rect);

    ui.horizontal_centered(|ui| {
        ui.add_space(208.0 + 10.0);
        group_gap(ui);
        // 工具身份：界面稿 .tool-id 是一颗**下拉**（icon + 名字 + ▼），点了换工具；
        // 之前只画静态名字，工具身份和工具栏读起来像两个东西。
        // 图标是自绘字形，egui 的 menu_button 只收文本 —— 图标画在按钮外沿，
        // 视觉上仍是一个整体（间距 4px，同 .tool-id 的 gap 7 折算）。
        let (icon, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
        icons::paint(ui.painter(), icon, state.tool.glyph(), text());
        ui.add_space(4.0);
        let w = &mut ui.style_mut().visuals.widgets;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, text());
        w.hovered.weak_bg_fill = elev_hover();
        w.hovered.bg_stroke = Stroke::NONE;
        w.active.weak_bg_fill = selected();
        ui.spacing_mut().button_padding = Vec2::new(6.0, 3.0);
        ui.menu_button(
            RichText::new(format!("{}  ▼", crate::i18n::tr(state.tool.cn())))
                .size(SIZE_LABEL)
                .color(text()),
            |ui| {
                menu_style(ui);
                for t in Tool::ALL {
                    let on = state.tool == t;
                    let label = if on {
                        format!("✓ {}", crate::i18n::tr(t.cn()))
                    } else {
                        format!("   {}", crate::i18n::tr(t.cn()))
                    };
                    let hotkey = match t {
                        Tool::Hand => "H",
                        Tool::Zoom => "Z",
                        Tool::Heal => "J",
                        Tool::Crop => "C",
                        Tool::Compare => "\\",
                        Tool::Grid => "G",
                    };
                    let clicked = menu_item(ui, &label, hotkey);
                    if clicked {
                        state.set_tool(t);
                        if t == Tool::Compare {
                            state.split = true;
                        }
                        if t == Tool::Grid {
                            state.grid = true;
                        }
                        ui.close();
                    }
                }
            },
        );
        opt_sep(ui);

        match state.module {
            Module::Sequence => {
                // 打开文件夹 / 重新扫描已是「文件」菜单里的常规项（Ctrl+O / F5），
                // 摆在工具栏上等于给全局动作开第二个入口 —— 界面稿的工具栏
                // 只回答"当前工具 + 它的参数 + 上下文"，文件操作归文件菜单。
                let n = state.frame_count();
                let valid = state.metrics.iter().filter(|m| m.is_some()).count();
                let count_label = if n == 1 {
                    crate::i18n::t!("单张照片").to_owned()
                } else {
                    crate::i18n::tr_fmt("{} 帧 · 可读 {}", &[&n, &valid])
                };
                ctx_seg(
                    ui,
                    "序列",
                    &count_label,
                    if state.issues.is_empty() {
                        None
                    } else {
                        Some(crate::i18n::tr_fmt("缺帧 {}", &[&state.issues.len()]))
                    },
                );
            }
            Module::Deflicker => {
                ui.label(
                    RichText::new(crate::i18n::t!("平滑窗口"))
                        .size(SIZE_LABEL)
                        .color(muted()),
                );
                let mut changed = bar_value(ui, |ui| {
                    ui.add(
                        egui::DragValue::new(&mut state.def_window)
                            .range(3.0..=199.0)
                            .speed(1.5)
                            .fixed_decimals(0),
                    )
                    .changed()
                });
                ui.label(
                    RichText::new(crate::i18n::t!("强度"))
                        .size(SIZE_LABEL)
                        .color(muted()),
                );
                changed |= bar_value(ui, |ui| {
                    ui.add(
                        egui::DragValue::new(&mut state.def_strength)
                            .range(0.0..=150.0)
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(" %"),
                    )
                    .changed()
                });
                if ui
                    .checkbox(
                        &mut state.def_multi,
                        RichText::new(crate::i18n::t!("多遍")).size(SIZE_LABEL),
                    )
                    .changed()
                {
                    state.recompute_deflicker();
                }
                if ui
                    .checkbox(
                        &mut state.def_channels,
                        RichText::new(crate::i18n::t!("分通道")).size(SIZE_LABEL),
                    )
                    .on_hover_text(crate::i18n::t!(
                        "按 R/G/B 分别校正，治彩色闪烁；不勾只补亮度"
                    ))
                    .changed()
                {
                    state.recompute_deflicker();
                }
                if state.is_single() {
                    ui.label(
                        RichText::new(crate::i18n::t!("单张照片没有闪烁可去"))
                            .size(SIZE_LABEL)
                            .color(dim()),
                    );
                } else if bar_button(ui, "重新分析").clicked() {
                    state.recompute_deflicker();
                }
                if changed {
                    let snapped = ((state.def_window.round() as i64).clamp(3, 199)) | 1;
                    state.def_window = snapped as f64;
                    state.recompute_deflicker();
                }
            }
            Module::Develop => {
                let current = state.preset.clone();
                let mut chosen: Option<&'static str> = None;
                egui::ComboBox::from_id_salt("preset-opt")
                    // 预设名可能是「自定义」或者「副本 xxx」——后者是存下来的标识，
                    // 交给 `tr_preset` 认前缀再翻。
                    .selected_text(
                        RichText::new(crate::i18n::tr_preset(&current))
                            .size(SIZE_LABEL)
                            .color(text()),
                    )
                    .width(118.0)
                    .show_ui(ui, |ui| {
                        combo_style(ui);
                        for name in grade::PRESET_NAMES {
                            if ui
                                .add(egui::Button::selectable(
                                    *name == state.preset,
                                    crate::i18n::tr(name),
                                ))
                                .clicked()
                            {
                                chosen = Some(name);
                            }
                        }
                    });
                if let Some(name) = chosen {
                    state.apply_preset(name);
                }
                if bar_button(ui, "自动").clicked() {
                    state.auto_exposure(ui.ctx());
                }
                if bar_button(ui, "复位").clicked() {
                    state.reset_params();
                }
                if let Some(note) = &state.auto_note {
                    ui.label(
                        RichText::new(theme::ellipsis(note, 30))
                            .size(SIZE_LABEL)
                            .color(accent()),
                    );
                }
                opt_sep(ui);
                // 上下文段（界面稿 .ctx）：粗体模块名 + 「N 项调整」小徽标。
                // 徽标数的是偏离默认值的参数组数 —— 蒙版数已经画在画布顶栏，
                // 这里只回答"调色动了多大片"。
                let changed = state.changed_groups();
                ctx_seg(
                    ui,
                    "显影",
                    &crate::i18n::tr_fmt("{} 项调整", &[&changed]),
                    None,
                );
            }
            Module::Frame => {
                for a in Aspect::ALL {
                    if bar_chip(ui, a.label(), state.aspect == a).clicked() {
                        state.set_aspect(a);
                    }
                }
                group_gap(ui);
                ui.checkbox(
                    &mut state.grid,
                    RichText::new(crate::i18n::t!("网格")).size(SIZE_LABEL),
                );
                ui.checkbox(
                    &mut state.cross,
                    RichText::new(crate::i18n::t!("十字")).size(SIZE_LABEL),
                );
                ui.checkbox(
                    &mut state.split,
                    RichText::new(crate::i18n::t!("对比")).size(SIZE_LABEL),
                );
            }
            Module::Render => {
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
                group_gap(ui);
                ui.label(
                    RichText::new(crate::i18n::t!("帧率"))
                        .size(SIZE_LABEL)
                        .color(muted()),
                );
                bar_value(ui, |ui| {
                    ui.add(
                        egui::DragValue::new(&mut state.output_fps)
                            .range(1.0..=120.0)
                            .speed(1.0)
                            .fixed_decimals(0)
                            .suffix(" fps"),
                    );
                });
                group_gap(ui);
                egui::ComboBox::from_id_salt("codec-opt")
                    .selected_text(
                        RichText::new(state.codec.clone())
                            .size(SIZE_LABEL)
                            .color(text()),
                    )
                    .width(84.0)
                    .show_ui(ui, |ui| {
                        combo_style(ui);
                        for codec in ["HEVC", "H.264", "ProRes"] {
                            if ui
                                .add(egui::Button::selectable(state.codec == codec, codec))
                                .clicked()
                            {
                                state.codec = codec.to_owned();
                            }
                        }
                    });
                if bar_primary(ui, "导出当前帧").clicked() {
                    state.export_current(ctx);
                }
            }
            Module::Audio => {
                if bar_button(ui, "选择音频…").clicked() {
                    choose_audio(state);
                }
                match &state.render_audio {
                    Some(path) => {
                        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("音频");
                        ui.label(
                            RichText::new(theme::ellipsis(name, 24))
                                .size(SIZE_LABEL)
                                .color(text()),
                        )
                        .on_hover_text(path.display().to_string());
                        if bar_button(ui, "移除").clicked() {
                            state.set_render_audio(None);
                        }
                        if bar_primary(ui, "带配乐导出").clicked() {
                            state.render_mode = OutputMode::Video;
                            state.export_copy = None;
                            state.run_export(ctx);
                        }
                    }
                    None => {
                        ui.label(
                            RichText::new(crate::i18n::t!("未选择配乐"))
                                .size(SIZE_LABEL)
                                .color(dim()),
                        );
                    }
                }
            }
            Module::Merge => {
                // 合成模块的上下文条只讲「哪张成片」：五组的进度都走状态栏长任务格，
                // 这里再放一份就成了第二处真相。
                //
                // The Merge context strip only names the current output: the five tools
                // report progress through the status bar's long-task slot, and a second
                // copy here would be a second source of truth.
                let n = state.frame_count();
                ui.label(
                    RichText::new(crate::i18n::tr_fmt("{} 帧待合成", &[&n]))
                        .size(SIZE_LABEL)
                        .color(dim()),
                );
            }
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // 右端只留一枚 ghost 胶囊（界面稿 .opt-chip 的静置态）：透明底、dim 字，
            // 悬停才抬起。缩放、适应、网格、对比这些都是"怎么看这张图"，
            // 和左边"当前工具的参数"不是一类事 —— 摊成七颗并排的控件，
            // 工具栏就成了两段互不相干的按钮条。
            let w = &mut ui.style_mut().visuals.widgets;
            w.inactive.weak_bg_fill = Color32::TRANSPARENT;
            w.inactive.bg_stroke = Stroke::NONE;
            w.inactive.fg_stroke = Stroke::new(1.0, dim());
            w.hovered.weak_bg_fill = elev_hover();
            w.hovered.bg_stroke = Stroke::NONE;
            w.hovered.fg_stroke = Stroke::new(1.0, text());
            w.active.weak_bg_fill = selected();
            w.active.bg_stroke = Stroke::NONE;
            ui.spacing_mut().button_padding = Vec2::new(9.0, 4.0);
            ui.menu_button(
                RichText::new(format!("{:.0}%  ▼", state.zoom * state.fit_scale * 100.0))
                    .font(mono(SIZE_LABEL)),
                |ui| {
                    menu_style(ui);
                    if menu_item(ui, "适应窗口", "Ctrl+0") {
                        fit_view(state);
                        ui.close();
                    }
                    if menu_item(ui, "实际像素", "Ctrl+1") {
                        actual_size(state);
                        ui.close();
                    }
                    if menu_item(ui, "放大", "+") {
                        state.zoom = (state.zoom * 1.25).min(24.0);
                        ui.close();
                    }
                    if menu_item(ui, "缩小", "−") {
                        state.zoom = (state.zoom / 1.25).max(0.1);
                        ui.close();
                    }
                    ui.separator();
                    ui.checkbox(
                        &mut state.grid,
                        RichText::new(crate::i18n::t!("网格")).size(SIZE_BODY),
                    );
                    ui.checkbox(
                        &mut state.split,
                        RichText::new(crate::i18n::t!("前后对比")).size(SIZE_BODY),
                    );
                    ui.separator();
                    ui.menu_button(
                        RichText::new(crate::i18n::t!("界面字号")).size(SIZE_BODY),
                        |ui| {
                            menu_style(ui);
                            for (i, z) in FONT_ZOOMS.iter().enumerate() {
                                let on = state.font_level == i;
                                let label = if on {
                                    format!("✓ {}%", (z * 100.0).round())
                                } else {
                                    format!("   {}%", (z * 100.0).round())
                                };
                                if menu_item(ui, &label, "") {
                                    state.font_level = i;
                                    ui.close();
                                }
                            }
                        },
                    );
                },
            );
        });
    });
    // 右段读数（原画布顶行）：画幅·位深 / 蒙版 n 个 / 撤销 Ctrl+Z。用 painter 排在
    // 缩放控件左侧，避免挤进那个 right_to_left 布局；三处都是 11px dim，不参与交互。
    //
    // Right-hand readouts (moved from the canvas's own top row): frame size · bit depth,
    // mask count, undo hint. Painted left of the zoom control so they do not have to be
    // threaded through the right-to-left layout; all 11 px dim and non-interactive.
    let bits = state.source_note.as_ref().and_then(|s| {
        s.strip_suffix(" 位")
            .and_then(|t| t.rsplit(' ').next())
            .and_then(|t| t.parse::<u32>().ok())
    });
    let p = ui.painter();
    let zoom_w =
        p.layout_no_wrap(
            format!("{:.0}%  ▼", state.zoom * state.fit_scale * 100.0),
            mono(SIZE_LABEL),
            text(),
        )
        .size()
        .x + 26.0;
    let mut x = full.right() - 10.0 - zoom_w - 14.0;
    let readings = [
        crate::i18n::t!("撤销 Ctrl+Z").to_owned(),
        crate::i18n::tr_fmt("蒙版 {} 个", &[&state.params.masks.len()]),
        super::canvas::frame_size_text(state.resolution, bits),
    ];
    for text_line in readings {
        let g = p.layout_no_wrap(text_line, font(SIZE_SMALL), dim());
        x -= g.size().x;
        p.galley(Pos2::new(x, full.center().y - g.size().y * 0.5), g, dim());
        x -= 14.0;
    }
}

// ---------------------------------------------------------------- 工具轨

pub(super) fn tool_rail(state: &mut AppState, ui: &mut Ui) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.line_segment(
        [full.right_top(), full.right_bottom()],
        Stroke::new(1.0, border()),
    );
    // 轨首一枚「预设与快照」开关：2026-11-04 起预设/快照/副本住浮窗
    // （docs/界面布局-ACR化-2026-11-04.md §2），ACR 也把它们挂在工具条上而不是
    // 编辑面板里。它是面板开关不是工具，所以与下面的动手类工具用一道缝隔开。
    //
    // The rail starts with a Presets & Snapshots toggle: since 2026-11-04 presets /
    // snapshots / copies live in a floating window (plan §2), and ACR likewise keeps
    // them on the tool strip rather than in the edit panel. It is a panel toggle, not a
    // tool, so a divider separates it from the hands-on tools below.
    // 工具按钮 32×32：设计图 `.tool-btn{width:32px;height:32px}`，48px 轨宽里
    // 左右各留 8px。原 36 配的是 52px 轨；轨收窄后按钮必须同步，否则会顶到轨边、
    // 左右不再对称。
    //
    // Tool buttons are 32×32 per the design's `.tool-btn`, centred in the 48 px rail
    // with 8 px on each side. The old 36 paired with the 52 px rail; once the rail
    // narrowed the button has to follow or it touches the edge.
    let size = 32.0;
    let x = full.left() + (TOOL_W - size) * 0.5;
    let mut y = full.top() + 8.0;
    {
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(size));
        let resp = ui.interact(rect, ui.make_persistent_id("rail-presets"), Sense::click());
        if state.show_left {
            p.rect_filled(rect, radius_control(), selected());
            p.rect_stroke(
                rect,
                radius_control(),
                Stroke::new(1.0, accent()),
                StrokeKind::Inside,
            );
        } else if resp.hovered() {
            p.rect_filled(rect, radius_control(), elev_hover());
        }
        icons::paint(
            &p,
            rect.shrink(8.0),
            Glyph::Sliders,
            if state.show_left {
                on_selected()
            } else if resp.hovered() {
                text()
            } else {
                muted()
            },
        );
        if resp.clicked() {
            state.show_left = !state.show_left;
            state.settings_dirty = true;
        }
        resp.on_hover_text(crate::i18n::t!("预设与快照 (Ctrl+K)"));
        y += size + 4.0;
    }
    p.line_segment(
        [
            Pos2::new(full.left() + 14.0, y - 3.0),
            Pos2::new(full.right() - 14.0, y - 3.0),
        ],
        Stroke::new(1.0, divider()),
    );
    y += 6.0;
    for (i, t) in Tool::ALL.iter().enumerate() {
        // 查看类（抓手/缩放）与动手类（裁剪/对比/网格）之间留一道缝。
        if i == 2 {
            p.line_segment(
                [
                    Pos2::new(full.left() + 14.0, y - 3.0),
                    Pos2::new(full.right() - 14.0, y - 3.0),
                ],
                Stroke::new(1.0, divider()),
            );
            y += 6.0;
        }
        let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(size));
        let resp = ui.interact(rect, ui.make_persistent_id(("tool", i)), Sense::click());
        let active = state.tool == *t;
        if active {
            p.rect_filled(rect, radius_control(), selected());
            // 激活指示改左缘 2px 竖条（设计稿态）：整圈 accent 描边在 14 枚工具上
            // 面积过大，竖条只占 2px 宽、把「哪个工具开着」指得同样清楚，也让
            // accent 的面积留给焦点环与主行动。
            //
            // The active marker is a 2 px accent bar on the left edge (design spec):
            // a full accent outline across 14 tools spends far more area; the bar is
            // 2 px wide, points just as clearly at the active tool, and leaves accent
            // for the focus ring and primary actions.
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(rect.left() + 1.0, rect.top() + 7.0),
                    Pos2::new(rect.left() + 3.0, rect.bottom() - 7.0),
                ),
                1.0,
                accent(),
            );
        } else if resp.hovered() {
            p.rect_filled(rect, radius_control(), elev_hover());
        }
        let color = if active {
            on_selected()
        } else if resp.hovered() {
            text()
        } else {
            muted()
        };
        icons::paint(&p, rect.shrink(8.0), t.glyph(), color);
        if resp.clicked() {
            state.set_tool(*t);
            if *t == Tool::Compare {
                state.split = true;
            }
            if *t == Tool::Grid {
                state.grid = true;
            }
        }
        resp.on_hover_text(crate::i18n::tr(t.hint()));
        y += size + 4.0;
    }

    // 轨底只留一枚「前后」：界面稿 .rail-foot 就这一个动作 —— 按住/切换看原图。
    // 文件夹在「文件」菜单（Ctrl+O），导出在标题栏（导出…），帮助在「帮助」菜单；
    // 三个入口在这里各开一份，轨就变成了第二排菜单栏。
    let foot_hint = if state.split {
        "前后对比：开（\\ 关闭）"
    } else {
        "前后对比：关（\\ 开启）"
    };
    let foot_rect =
        Rect::from_min_size(Pos2::new(x, full.bottom() - 8.0 - size), Vec2::splat(size));
    let resp = ui.interact(
        foot_rect,
        ui.make_persistent_id("rail-foot"),
        Sense::click(),
    );
    if state.split {
        p.rect_filled(foot_rect, radius_control(), selected());
        p.rect_stroke(
            foot_rect,
            radius_control(),
            Stroke::new(1.0, accent()),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_filled(foot_rect, radius_control(), elev());
    }
    icons::paint(
        &p,
        foot_rect.shrink(8.0),
        Glyph::Compare,
        if state.split {
            on_selected()
        } else if resp.hovered() {
            text()
        } else {
            muted()
        },
    );
    if resp.clicked() {
        state.split = !state.split;
    }
    resp.on_hover_text(foot_hint);
}

// ---------------------------------------------------------------- 状态栏

/// 状态栏只在「有话要说」时存在：告警、长任务进度、边车提示。
///
/// 常驻的文件名 / 画幅 / 帧号 / 缩放 / 色彩管线 / 引擎状态整条撤掉（用户 2026-11-08
/// 「底部其实没有必要显示」）——这些读数在画布面包屑、走带与选项栏里都已经有一份，
/// 底边再抄一遍只是白占 24px。顶层错误改由画布顶部的错误条承担，状态栏不再重复。
///
/// The status bar exists only when it has something to say: a notice, a long-task
/// progress row, or a sidecar hint. The permanent filename / frame size / frame counter
/// / zoom / pipeline / engine readouts are gone (user, 2026-11-08: "the bottom does not
/// need to show anything") -- the canvas breadcrumb, transport and options bar already
/// carry them, and a second copy only eats 24 px. Top-level errors live in the canvas
/// banner; the status bar no longer repeats them.
pub(super) fn status_bar_needed(state: &AppState) -> bool {
    !state.notices.is_empty()
        || state.sidecar_hint.is_some()
        || state.plugin_running()
        || state.export_progress().is_some()
        || state.queue_progress().is_some()
        || state.merge_progress().is_some()
        || state.align_progress().is_some()
        || state.qa_progress().is_some()
}

pub(super) fn status_bar(state: &mut AppState, ui: &mut Ui) {
    let full = ui.max_rect();
    theme::rule(
        ui.painter(),
        full.left(),
        full.right(),
        full.top(),
        border(),
    );
    // LapseOne 把「导出中 (1/19, 4%)」常驻在状态栏最左：长任务跑到哪一步，
    // 不该只有点开渲染面板才看得到。导出和队列共用这一格，谁在跑显示谁。
    // 插件在飞时这一格归插件：名字 + 进度 + 一枚取消，毕竟它是用户刚刚
    // 亲手点下去的那个活。同一时刻只讲一个长任务的故事。
    let plugin_run = state.plugin_running();
    // 全局长任务一格：导出 / 队列带阶段，合并 / 对齐 / 体检只有 n/m。同一时刻只显示
    // 最先命中的那一个，和「状态栏只讲一个故事」的口径一致。
    //
    // One global long-task slot: export/queue carry a stage, merge/align/QA only n/m. The
    // first match wins, matching the "one story in the status bar" rule.
    let prog = state
        .export_progress()
        .map(|(done, total, stage)| ("导出", done, total, Some(stage)))
        .or_else(|| {
            state
                .queue_progress()
                .map(|(done, total, stage)| ("队列", done, total, Some(stage)))
        })
        .or_else(|| {
            state
                .merge_progress()
                .map(|(kind, done, total)| (kind.cn(), done, total, None))
        })
        .or_else(|| {
            state
                .align_progress()
                .map(|(done, total)| ("对齐", done, total, None))
        })
        .or_else(|| {
            state
                .qa_progress()
                .map(|(done, total)| ("体检", done, total, None))
        });
    ui.horizontal_centered(|ui| {
        ui.add_space(8.0);
        // 这一行现在只讲「正在发生的事」：告警 / 长任务 / 边车。段间中点只在前面
        // 真画过一段时才点，否则状态栏会以一枚孤零零的「·」开头或结尾。
        //
        // This row now only carries what is happening: notice / long task / sidecar. The
        // inter-segment dot is emitted only when something was drawn before it, or the bar
        // would open or close on a lone "·".
        let mut spoke = false;
        // 常驻告警：护带目录搬不动 / 只读 / 被同名文件占住这类「用户点保存之前
        // 就该知道」的事实。空的时候一个形状都不建——`--selftest` 的形状计数
        // 靠的就是这条路径在无告警时完全不留痕。
        if !state.notices.is_empty() {
            let first = state.notices[0].clone();
            let rest = state.notices.len() - 1;
            let mut text = format!("⚠ {}", theme::ellipsis(&first, 56));
            if rest > 0 {
                text.push_str(&format!("  +{rest}"));
            }
            let resp = ui
                .add(
                    egui::Label::new(RichText::new(text).size(SIZE_LABEL).color(warn()))
                        .selectable(false)
                        .sense(Sense::click()),
                )
                .on_hover_text(crate::i18n::tr_fmt("{}\n（点击消除这条提示）", &[&first]));
            // 点一下消一条：还有下一条就顶上来，全消完这一格（连同分隔点）一起消失。
            if resp.clicked() {
                state.notices.remove(0);
            }
            ui.label(RichText::new("·").size(SIZE_LABEL).color(dim()));
            spoke = true;
        }
        if plugin_run {
            let (done, total) = state.plugin_progress;
            let name = state
                .plugin_job
                .as_ref()
                .map(|j| j.name.clone())
                .unwrap_or_default();
            ui.label(
                RichText::new(format!("{name} ({done}/{total})"))
                    .font(mono(SIZE_LABEL))
                    .color(accent()),
            );
            if ui
                .add(egui::Button::new(
                    RichText::new(crate::i18n::t!("✕ 取消")).size(SIZE_LABEL),
                ))
                .clicked()
            {
                state.plugin_cancel();
            }
            ui.label(RichText::new("·").size(SIZE_LABEL).color(dim()));
            spoke = true;
        } else if let Some((what, done, total, stage)) = prog {
            let pct = if total > 0 {
                done as f32 * 100.0 / total as f32
            } else {
                0.0
            };
            let line = match stage {
                Some(stage) => {
                    let stage = match stage {
                        aevum_core::export::Stage::Frames => "写帧",
                        aevum_core::export::Stage::Encode => "编码",
                    };
                    crate::i18n::tr_fmt(
                        "{}中·{} ({}/{} {}%)",
                        &[
                            &crate::i18n::tr(what),
                            &crate::i18n::tr(stage),
                            &done,
                            &total,
                            &format!("{pct:.0}"),
                        ],
                    )
                }
                // 合并 / 对齐 / 体检没有编码阶段，别硬凑一个空的阶段名。
                None => {
                    crate::i18n::tr_fmt("正在{}… ({}/{})", &[&crate::i18n::tr(what), &done, &total])
                }
            };
            ui.label(RichText::new(line).font(mono(SIZE_LABEL)).color(accent()));
            // 剩余时间只用「当前显示的这个任务」的历史采样，换了任务或进度回退
            // （新任务从 0 起）就重新开表，免得把上一轮的速率借给下一轮。
            //
            // ETA uses only this task's samples; a task change or a reset to 0 starts a
            // fresh table so one run's rate never leaks into the next.
            if let Some(secs) = state.eta_secs(what, done, total) {
                ui.label(RichText::new("·").size(SIZE_LABEL).color(dim()));
                ui.label(
                    RichText::new(crate::i18n::tr_fmt("约剩 {}s", &[&secs]))
                        .font(mono(SIZE_LABEL))
                        .color(muted()),
                );
            }
            // 续跑跳过的那几帧由导出线程数完写进原子量，所以这一格是晚一拍才出现的。
            let skipped = state.export_skipped();
            if skipped > 0 {
                ui.label(RichText::new("·").size(SIZE_LABEL).color(dim()));
                ui.label(
                    RichText::new(crate::i18n::tr_fmt("续跑跳过 {}", &[&skipped]))
                        .font(mono(SIZE_LABEL))
                        .color(muted()),
                );
            }
            spoke = true;
        }
        // 文件名 / 画幅 / 帧号 / 星级这些常驻读数已撤：画布面包屑、走带和选项栏各有一份，
        // 底边不再抄第二遍（用户 2026-11-08「底部其实没有必要显示」）。
        //
        // The permanent filename / frame-size / frame-counter / rating readouts are gone:
        // the canvas breadcrumb, transport and options bar each carry their own copy, so the
        // bottom edge no longer repeats them (user, 2026-11-08).
        // 边车提示：检测到同目录 .xmp 时给一枚一键应用。不做静默套用——
        // 改不改参数要让用户亲手按下这一下（策略：提示 + 一键应用）。
        if let Some(sidecar) = state.sidecar_hint.clone() {
            if spoke {
                ui.label(RichText::new("·").size(SIZE_LABEL).color(dim()));
            }
            let name = sidecar
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| sidecar.display().to_string());
            let apply = ui.add(egui::Button::new(
                RichText::new(crate::i18n::tr_fmt(
                    "应用边车 {}",
                    &[&theme::ellipsis(&name, 28)],
                ))
                .size(SIZE_LABEL),
            ));
            if apply.clicked() {
                state.apply_sidecar_hint();
            }
            apply.on_hover_text(crate::i18n::tr_fmt(
                "检测到 Lightroom 边车，点击套用：{}",
                &[&sidecar.display()],
            ));
        }
    });
    // 窗口底边再压一条随进度变长的细线：瞥一眼右下角就知道活儿干完没有。
    // 插件在飞时这条线归插件（done/total），导出/队列照旧。
    let thin = if plugin_run {
        let (done, total) = state.plugin_progress;
        (total > 0).then_some(done as f32 / total as f32)
    } else {
        prog.and_then(|(_, done, total, _)| (total > 0).then_some(done as f32 / total as f32))
    };
    if let Some(f) = thin {
        let f = f.clamp(0.0, 1.0);
        ui.painter().rect_filled(
            Rect::from_min_max(
                full.left_bottom(),
                Pos2::new(full.left() + full.width() * f, full.bottom()),
            ),
            0,
            accent(),
        );
    }
}
