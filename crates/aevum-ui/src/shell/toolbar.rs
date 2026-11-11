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
use crate::state::{AppState, Aspect, DockTab, Module, Tool};
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
    // 立体感（用户 2026-11-11「选项要有厚度」）：自绘 bevel 渐变底 +
    // hover/按下两态，与 bar_primary 同一结构——egui Button 的平涂底
    // 没有实体感。
    let label = crate::i18n::tr(label);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(SIZE_BODY), text());
    let size = Vec2::new(galley.size().x + 20.0, BAR_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let base = if resp.is_pointer_button_down_on() {
        press()
    } else if resp.hovered() {
        elev_hover()
    } else {
        elev()
    };
    theme::surface_bevel(p, rect, base);
    p.rect_stroke(
        rect,
        radius_control(),
        Stroke::new(
            1.0,
            if resp.hovered() {
                border_strong()
            } else {
                border()
            },
        ),
        StrokeKind::Inside,
    );
    p.galley(rect.center() - galley.size() * 0.5, galley, text());
    resp
}

/// 主按钮：强调色实心 + 深字，悬停提亮、按下压暗，圆角走 token。
pub(super) fn bar_primary(ui: &mut Ui, label: &str) -> Response {
    let label = crate::i18n::tr(label);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(SIZE_BODY), on_accent());
    let size = Vec2::new(galley.size().x + 22.0, BAR_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    // `dragged()` 只在 `senses_drag()` 的控件上为真（egui interaction.rs），
    // click-only 的主按钮恒 false——按下态因此是死代码。改用真正的按下判定。
    //
    // `dragged()` is only true for drag-sensing widgets, so a click-only primary button
    // never saw it -- the pressed state was dead code.
    let fill = if resp.is_pointer_button_down_on() {
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
        ui.painter().rect_filled(rect, radius_chip(), field());
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
pub(super) fn doc_tab(state: &mut AppState, p: &egui::Painter, ui: &mut Ui, rect: Rect) {
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
            p.rect_filled(close, radius_chip(), theme::overlay_white(header(), 26));
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
        open_dialog(state);
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
    doc_tab(state, &tab_painter, ui, tab_rect);

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
                    state.request_export_confirm(ctx, true);
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

/// 轨上的一项。
///
/// 条目表与排布分开：表是静态数据（`rail_items`），排布是纯函数（`rail_layout`），
/// 绘制只消费排布结果。这样 1280×800 的溢出可以被单测直接量，不必真的开一个窗口。
///
/// One entry on the rail. The table is static data (`rail_items`), the arrangement is a pure
/// function (`rail_layout`), and drawing only consumes the result -- so the 1280x800 overflow
/// can be measured in a unit test without opening a window.
#[derive(Clone, Copy)]
pub(super) struct RailItem {
    /// 图标。
    ///
    /// Glyph drawn for this entry.
    pub glyph: Glyph,
    /// 悬停说明（i18n 键）。工具项留空，走 `Tool::hint()`。
    ///
    /// Hover text (i18n key). Tool entries leave it empty and use `Tool::hint()`.
    pub hint: &'static str,
    /// 分组：0 工作流、1 面板、2 工具、3 轨底。
    ///
    /// Group: 0 workflow, 1 panels, 2 tools, 3 rail foot.
    pub group: u8,
    pub kind: RailKind,
}

/// 一项被点击时该做什么。
///
/// What a click on the entry does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RailKind {
    Workflow,
    Presets,
    Mask,
    History,
    Library,
    Ai,
    Plugins,
    Tool(Tool),
    Look,
}

/// 轨的条目表。
///
/// 顺序即视觉顺序；`group` 决定分隔线位置。轨底的「外观与布局」也在这张表里（`group: 3`），
/// 由 `rail_layout` 单独摆到最底部——它不属于流式排布，但**必须**在同一张表里，否则
/// 「被折叠的项都能从『更多』够到」这条判据会漏掉它。
///
/// The rail's entry table. Order is visual order and `group` decides where dividers go. The
/// bottom "Appearance & layout" lives here too (`group: 3`) and `rail_layout` pins it to the
/// bottom: it is not part of the flow, but it **must** be in the table or the "every folded
/// entry stays reachable" fence would miss it.
pub(super) fn rail_items() -> &'static [RailItem] {
    const fn it(glyph: Glyph, hint: &'static str, group: u8, kind: RailKind) -> RailItem {
        RailItem {
            glyph,
            hint,
            group,
            kind,
        }
    }
    static RAIL: [RailItem; 14] = [
        it(Glyph::Steps, "工作流面板", 0, RailKind::Workflow),
        it(Glyph::Sliders, "预设与快照 (Ctrl+K)", 1, RailKind::Presets),
        it(Glyph::Eye, "蒙版", 1, RailKind::Mask),
        it(Glyph::History, "历史", 1, RailKind::History),
        it(Glyph::Folder, "图库", 1, RailKind::Library),
        it(Glyph::Star, "AI 助手", 1, RailKind::Ai),
        it(Glyph::Plug, "插件", 1, RailKind::Plugins),
        it(Glyph::Hand, "", 2, RailKind::Tool(Tool::Hand)),
        it(Glyph::Zoom, "", 2, RailKind::Tool(Tool::Zoom)),
        it(Glyph::Heal, "", 2, RailKind::Tool(Tool::Heal)),
        it(Glyph::Crop, "", 2, RailKind::Tool(Tool::Crop)),
        it(Glyph::Compare, "", 2, RailKind::Tool(Tool::Compare)),
        it(Glyph::Grid, "", 2, RailKind::Tool(Tool::Grid)),
        it(Glyph::Gear, "外观与布局", 3, RailKind::Look),
    ];
    &RAIL
}

/// 工具轨的两半：上是「工作流 / 面板入口」，下是「工具组 / 轨底设置」。
/// 用户 2026-11-09 要求两半分居窗口左右，别挤在同一侧。
///
/// The two halves of the tool rail: the upper half holds workflow / panel entries, the
/// lower half holds tools / the rail foot. The user (2026-11-09) wants them on opposite
/// window edges instead of stacked on one side.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RailHalf {
    Panels,
    Tools,
}

/// 半轨的条目切片：0..7 是上半（group 0/1），7..14 是下半（group 2/3）。
/// 只切不改表，14 项一项不丢；`rail_layout` / `tool_rail` 都按下标读同一份切片。
///
/// The half-table slices: 0..7 is the upper half (groups 0/1), 7..14 the lower half
/// (groups 2/3). Slicing -- not rewriting -- keeps all 14 entries; `rail_layout` and
/// `tool_rail` read the same slice by index.
pub(super) fn rail_items_half(half: RailHalf) -> &'static [RailItem] {
    let all = rail_items();
    // 切点按 group 边界算，不写死 7：表里增删任意一项都不会静默把工具混进
    // 上半轨。表本身保证 group 单调不减（判据里也钉着这一点）。
    //
    // The split point comes from the group boundary, not a hard-coded 7: adding or
    // removing a table entry can never silently smuggle a tool into the upper rail.
    // The table itself keeps `group` non-decreasing (a test pins that too).
    let split = all.iter().position(|it| it.group >= 2).unwrap_or(all.len());
    match half {
        RailHalf::Panels => &all[..split],
        RailHalf::Tools => &all[split..],
    }
}

/// 一次排布的结果。
///
/// The result of one arrangement pass.
pub(super) struct RailSlots {
    /// 流里的可见项：`(条目下标, 矩形)`；「更多」按钮不在其中。
    ///
    /// Visible flow slots as `(entry index, rect)`; the "more" button is not among them.
    pub shown: Vec<(usize, Rect)>,
    /// 「更多」按钮的位置（有折叠时才有）。
    ///
    /// Position of the "more" button, present only when the panel group is folded.
    pub more: Option<Rect>,
    /// 被折叠进「更多」的条目下标。
    ///
    /// Indices of the entries folded into "more".
    pub folded: Vec<usize>,
    /// 组分隔线的 y。
    ///
    /// Y coordinates of the group separators.
    pub separators: Vec<f32>,
    /// 轨底那一枚（外观与布局）的位置。
    ///
    /// Position of the foot entry (appearance and layout).
    pub foot: Option<(usize, Rect)>,
    /// 本次用的按钮边长。
    ///
    /// Button edge length used for this pass.
    pub btn: f32,
}

/// 流里的一项：要么是某个条目，要么是「更多」。
///
/// One flow slot: either a table entry or the "more" button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Item(usize),
    More(u8),
}

/// 排一次轨。纯函数，不碰 `Ui`、不读状态。
///
/// 规则按顺序尝试，取第一个放得下的方案：
/// 1. 全展开、按钮 36；
/// 2. 把面板组（group 1）折成一颗「更多」，按钮保持 36；
/// 3. 按钮压到 32（上一版的大小）——只缩不重排；
/// 4. 再压 2px 兜底。
///
/// **工具组（group 2）永不折叠**：它是每天要点的东西，折进去等于把主要动作藏起来。
/// 面板组可以折——它们本来就是"一类入口"，折成一颗「更多」仍然一次点开。
///
/// Lays the rail out. A pure function: no `Ui`, no state. It tries, in order: everything at
/// 36 px; fold the panel group into one "more" entry at 36 px; shrink to 32 px (the previous
/// size) without rearranging; then 2 px tighter as a last resort. The tool group is **never**
/// folded -- those are the things clicked every day, and folding them hides the main actions.
/// The panel group may fold: it is a family of entries anyway, and one "more" click still
/// reaches them.
pub(super) fn rail_layout(rect: Rect, items: &[RailItem]) -> RailSlots {
    const PAD: f32 = 8.0;
    const FOOT_BTN_MIN: f32 = 32.0;

    // 轨底那一枚占掉底部一条，流可用高要减掉它。
    //
    // The foot entry owns a strip at the bottom, so the flow loses that height.
    let has_foot = items.iter().any(|it| it.group == 3);
    let foot_reserve = if has_foot {
        FOOT_BTN_MIN + RAIL_GAP
    } else {
        0.0
    };
    let avail = (rect.height() - PAD * 2.0 - foot_reserve).max(0.0);

    let flow_of = |fold: bool| -> Vec<Flow> {
        let mut out = Vec::new();
        let mut folded_group: Option<u8> = None;
        for (i, it) in items.iter().enumerate() {
            if it.group == 3 {
                continue;
            }
            if fold && it.group == 1 {
                if folded_group != Some(1) {
                    out.push(Flow::More(1));
                    folded_group = Some(1);
                }
                continue;
            }
            out.push(Flow::Item(i));
        }
        out
    };

    let height_of = |flow: &[Flow], btn: f32| -> f32 {
        let mut h = 0.0;
        let mut prev_group: Option<u8> = None;
        for f in flow {
            let g = match f {
                Flow::Item(i) => items[*i].group,
                Flow::More(g) => *g,
            };
            if let Some(pg) = prev_group {
                h += if pg == g { RAIL_GAP } else { RAIL_GROUP_GAP };
            }
            h += btn;
            prev_group = Some(g);
        }
        h
    };

    let tries = [
        (RAIL_BTN, false),
        (RAIL_BTN, true),
        (RAIL_MIN_BTN, true),
        (RAIL_MIN_BTN - 2.0, true),
    ];
    let mut picked = (RAIL_MIN_BTN - 2.0, true);
    let mut flow = flow_of(true);
    for (btn, fold) in tries {
        let f = flow_of(fold);
        if height_of(&f, btn) <= avail {
            picked = (btn, fold);
            flow = f;
            break;
        }
        if (btn, fold) == tries[tries.len() - 1] {
            flow = f;
        }
    }
    let (btn, _) = picked;

    let mut shown = Vec::new();
    let mut separators = Vec::new();
    let mut more = None;
    let mut folded = Vec::new();
    let mut y = rect.top() + PAD;
    let mut prev_group: Option<u8> = None;
    for f in &flow {
        let g = match f {
            Flow::Item(i) => items[*i].group,
            Flow::More(g) => *g,
        };
        if let Some(pg) = prev_group {
            if pg != g {
                separators.push(y + RAIL_GROUP_GAP * 0.5);
                y += RAIL_GROUP_GAP;
            } else {
                y += RAIL_GAP;
            }
        }
        let slot = Rect::from_min_size(Pos2::new(rect.center().x - btn * 0.5, y), Vec2::splat(btn));
        match f {
            Flow::Item(i) => shown.push((*i, slot)),
            Flow::More(_) => {
                more = Some(slot);
                folded = items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| it.group == 1)
                    .map(|(i, _)| i)
                    .collect();
            }
        }
        y += btn;
        prev_group = Some(g);
    }

    // 轨底：外观与布局。它不在流里，因此永远可见——这是"工具永不折叠"之外的第二条
    // 硬约束：设置入口不能因为窗口变矮就消失。
    //
    // The foot: Appearance & layout. Not in the flow, so it is always visible -- the second
    // hard rule besides "tools never fold": the settings entry must not vanish when the window
    // gets short.
    let foot_idx = items.iter().position(|it| it.group == 3);
    let foot = foot_idx.map(|i| {
        let r = Rect::from_min_size(
            Pos2::new(
                rect.center().x - RAIL_MIN_BTN * 0.5,
                rect.bottom() - PAD - RAIL_MIN_BTN,
            ),
            Vec2::splat(RAIL_MIN_BTN),
        );
        (i, r)
    });

    RailSlots {
        shown,
        more,
        folded,
        separators,
        foot,
        btn,
    }
}

/// 一项的悬停说明：工具项走 `Tool::hint()`，其余读表。
///
/// An entry's tooltip: tools come from `Tool::hint()`, everything else from the table.
fn item_hint(it: &RailItem) -> &'static str {
    match it.kind {
        RailKind::Tool(t) => t.hint(),
        _ => it.hint,
    }
}

/// 这一项是否处于"开"的状态。
///
/// Whether the entry reads as "on".
fn item_active(state: &AppState, kind: RailKind) -> bool {
    match kind {
        RailKind::Workflow => state.show_flow,
        RailKind::Presets => state.show_left,
        RailKind::Tool(t) => state.tool == t,
        // 面板类与设置类是"按一下就过去"的动作，没有常驻开态。
        //
        // Panel and settings entries are one-shot actions, with no persistent on-state.
        RailKind::Mask
        | RailKind::History
        | RailKind::Library
        | RailKind::Ai
        | RailKind::Plugins
        | RailKind::Look => false,
    }
}

pub(super) fn tool_rail(state: &mut AppState, ui: &mut Ui, half: RailHalf, on_left: bool) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    // 边框与激活条永远朝「内侧」：左轨取右缘，右轨取左缘。左轨右邻是
    // 画布（或工作流面板），右轨左邻是参数坞——两种情形都在分界处画线，
    // 窗口外缘不留线。
    //
    // The border and the active bar always face inward: a left rail uses its right edge,
    // a right rail its left edge. The left rail's neighbour is the canvas (or the flow
    // panel); the right rail's is the dock -- both get the divider at the seam, and the
    // outer window edge stays clean.
    let (edge_a, edge_b) = if on_left {
        (full.right_top(), full.right_bottom())
    } else {
        (full.left_top(), full.left_bottom())
    };
    p.line_segment([edge_a, edge_b], Stroke::new(1.0, border()));

    let items = rail_items_half(half);
    let slots = rail_layout(full, items);
    let mut click: Option<RailKind> = None;
    let mut more_resp: Option<egui::Response> = None;

    for (idx, rect) in &slots.shown {
        let it = &items[*idx];
        let resp = ui.interact(*rect, ui.make_persistent_id(("rail", *idx)), Sense::click());
        let active = item_active(state, it.kind);
        if active {
            p.rect_filled(*rect, radius_control(), selected());
            // 左缘 2px 竖条：整圈 accent 描边在十几枚图标上面积过大，竖条把"哪个开着"
            // 指得同样清楚，把 accent 的面积留给焦点环与主行动。
            //
            // A 2 px accent bar on the left edge: a full outline across a dozen icons spends
            // far more area; the bar points just as clearly and leaves accent for the focus
            // ring and primary actions.
            let (bx0, bx1) = if on_left {
                (rect.right() - 3.0, rect.right() - 1.0)
            } else {
                (rect.left() + 1.0, rect.left() + 3.0)
            };
            // 竖条从中间长出来（0.12s）：与顶栏/坞下划线同一套参数，只动长度。
            //
            // The bar grows from the middle over 0.12 s, matching the top bar and dock
            // underline, length only.
            let t = ui.ctx().animate_bool_with_time(
                ui.make_persistent_id(("rail-active", *idx)),
                true,
                0.12,
            );
            let mid = rect.center().y;
            let half = (rect.height() * 0.5 - 8.0).max(0.0) * t;
            p.rect_filled(
                Rect::from_min_max(Pos2::new(bx0, mid - half), Pos2::new(bx1, mid + half)),
                1.0,
                accent().gamma_multiply(t.max(0.15)),
            );
        } else if resp.is_pointer_button_down_on() {
            // 按下比悬停再沉一档：15 个图标点起来才有「按」而不是「滑」。
            //
            // Pressed sinks one step below hover, so the fifteen icons feel pressed,
            // not swiped.
            p.rect_filled(*rect, radius_control(), press());
        } else if resp.hovered() {
            p.rect_filled(*rect, radius_control(), elev_hover());
        }
        let color = if active {
            on_selected()
        } else if resp.hovered() {
            text()
        } else {
            muted()
        };
        icons::paint(&p, rect.shrink(slots.btn * 0.28), it.glyph, color);
        if resp.clicked() {
            click = Some(it.kind);
        }
        resp.on_hover_text(crate::i18n::tr(item_hint(it)));
    }

    // 「更多」：面板组折进来时才有。点开是一列带图标的菜单项，与轨上的图标同一套。
    //
    // "More": only when the panel group is folded. It opens a column of icon + label rows
    // using the same glyphs as the rail.
    if let Some(rect) = slots.more {
        let resp = ui.interact(rect, ui.make_persistent_id("rail-more"), Sense::click());
        if resp.hovered() {
            p.rect_filled(rect, radius_control(), elev_hover());
        }
        icons::paint(
            &p,
            rect.shrink(slots.btn * 0.28),
            Glyph::ChevronDown,
            if resp.hovered() { text() } else { muted() },
        );
        more_resp = Some(resp.clone());
        resp.on_hover_text(crate::i18n::tr("更多面板"));
    }

    if let (Some(resp), true) = (more_resp.as_ref(), !slots.folded.is_empty()) {
        let folded = slots.folded.clone();
        egui::Popup::menu(resp).width(TOOL_W * 2.6).show(|ui| {
            for idx in &folded {
                let it = &items[*idx];
                let row = ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    icons::paint(&p, r, it.glyph, muted());
                    ui.label(crate::i18n::tr(item_hint(it)));
                });
                if row.response.interact(Sense::click()).clicked() {
                    click = Some(it.kind);
                    ui.close();
                }
            }
        });
    }

    if let Some((idx, rect)) = slots.foot {
        let it = &items[idx];
        let resp = ui.interact(rect, ui.make_persistent_id("rail-foot"), Sense::click());
        if resp.hovered() {
            p.rect_filled(rect, radius_control(), elev_hover());
        }
        icons::paint(
            &p,
            rect.shrink(RAIL_MIN_BTN * 0.28),
            it.glyph,
            if resp.hovered() { text() } else { muted() },
        );
        if resp.clicked() {
            click = Some(it.kind);
        }
        resp.on_hover_text(crate::i18n::tr(item_hint(it)));
    }

    for y in &slots.separators {
        p.line_segment(
            [
                Pos2::new(full.left() + 14.0, *y),
                Pos2::new(full.right() - 14.0, *y),
            ],
            Stroke::new(1.0, divider()),
        );
    }

    if let Some(kind) = click {
        activate_rail(state, kind);
    }
}

/// 点一项之后做什么。
///
/// 全部接既有入口：模块切换、dock 页签、浮窗开关——不为轨新造旁路。
///
/// What a click does. Everything binds to existing entries (module switch, dock tab, window
/// toggles); the rail gets no side channel of its own.
fn activate_rail(state: &mut AppState, kind: RailKind) {
    match kind {
        RailKind::Workflow => {
            state.show_flow = !state.show_flow;
            state.settings_dirty = true;
        }
        RailKind::Presets => {
            state.show_left = !state.show_left;
            state.settings_dirty = true;
        }
        RailKind::Mask => {
            // 蒙版住在显影页的调整面板里：跳过去、把那一组展开，有蒙版就选第一枚。
            //
            // Masks live in the Develop panel: jump there, open the group, and select the first
            // mask when one exists.
            state.module = Module::Develop;
            state.dock_tab = DockTab::Adjust;
            state.collapsed.remove("masks");
            if state.mask_sel.is_none() && !state.params.masks.is_empty() {
                state.mask_sel = Some(0);
            }
        }
        RailKind::History => {
            state.show_dock = true;
            state.dock_tab = DockTab::History;
        }
        RailKind::Library => {
            state.show_dock = true;
            state.dock_tab = DockTab::Library;
        }
        RailKind::Ai => {
            state.show_dock = true;
            state.dock_tab = DockTab::Ai;
        }
        RailKind::Plugins => state.market_open = true,
        RailKind::Look => state.look_open = true,
        RailKind::Tool(t) => {
            state.set_tool(t);
            if t == Tool::Compare {
                state.split = true;
            }
            if t == Tool::Grid {
                state.grid = true;
            }
        }
    }
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

#[cfg(test)]
mod rail_tests {
    use super::*;

    /// 两半分居左右：各 7 枚、合计 14 枚不丢，轨底只在下半，工具组永不折叠。
    ///
    /// The two halves live on opposite edges: seven each, all fourteen accounted for,
    /// the foot only on the lower half, and the tool group never folds.
    #[test]
    fn rail_halves_split_every_entry_once() {
        let all = rail_items();
        assert!(
            all.windows(2).all(|w| w[0].group <= w[1].group),
            "轨条目表的 group 必须单调不减，否则切点会把工具混进上半轨"
        );
        let panels = rail_items_half(RailHalf::Panels);
        let tools = rail_items_half(RailHalf::Tools);
        assert!(panels.iter().all(|i| i.group <= 1));
        assert!(tools.iter().all(|i| i.group >= 2));
        assert_eq!(panels.len(), 7, "上半应有 7 枚");
        assert_eq!(tools.len(), 7, "下半应有 7 枚");
        assert_eq!(panels.len() + tools.len(), rail_items().len());
        assert!(panels.iter().all(|i| i.group <= 1), "上半只放工作流/面板组");
        assert!(tools.iter().all(|i| i.group >= 2), "下半只放工具组/轨底");
        assert!(!panels.iter().any(|i| i.group == 3), "轨底不在上半");
        assert!(tools.iter().any(|i| i.group == 3), "轨底必须在下半");
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(TOOL_W, 800.0));
        let slots = rail_layout(rect, tools);
        assert_eq!(slots.shown.len(), 6, "6 枚工具全部常显");
        assert!(slots.more.is_none(), "工具组不许折进「更多」");
        assert!(slots.foot.is_some(), "外观与布局仍在下半轨底");
    }

    /// 三档窗口下的轨高：36 菜单行 + 40 选项行 + 136 胶片条 + 24 状态栏之后剩下的。
    ///
    /// Rail heights at three window sizes: what is left after the 36 px menu row, 40 px
    /// options bar, 136 px filmstrip and 24 px status bar.
    const HEIGHTS: [f32; 3] = [764.0, 564.0, 404.0];

    fn rect(h: f32) -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(TOOL_W, h))
    }

    /// 严重叠：相切（边贴边）不算。
    ///
    /// Strict overlap: touching edges do not count.
    fn overlap(a: Rect, b: Rect) -> bool {
        a.min.x < b.max.x - 0.5
            && b.min.x < a.max.x - 0.5
            && a.min.y < b.max.y - 0.5
            && b.min.y < a.max.y - 0.5
    }

    /// 三档窗口下：任何两个槽位都不重叠，且都在轨内。
    ///
    /// At all three sizes no two slots overlap and every slot stays inside the rail.
    ///
    /// 这条挡的是"功能加了、一半点不到"：14 项 × 40 + 组间在 1280×800 下会溢出，
    /// 溢出时后面的图标会跑到轨外——开发机 1600×1000 永远够，所以必须在这里量。
    ///
    /// This blocks "added the features, half of them unreachable": fourteen entries at 40 px
    /// plus group gaps overflow at 1280x800, and the overflow runs past the rail. A 1600x1000
    /// dev machine always fits, so it has to be measured here.
    #[test]
    fn rail_slots_never_overlap_at_three_sizes() {
        let items = rail_items();
        for h in HEIGHTS {
            let r = rect(h);
            let slots = rail_layout(r, items);

            let mut all: Vec<Rect> = slots.shown.iter().map(|(_, s)| *s).collect();
            if let Some(m) = slots.more {
                all.push(m);
            }
            if let Some((_, f)) = slots.foot {
                all.push(f);
            }
            assert!(!all.is_empty(), "高 {h} 时轨上一个槽位都没有");

            for (i, a) in all.iter().enumerate() {
                assert!(
                    a.top() >= r.top() - 0.5 && a.bottom() <= r.bottom() + 0.5,
                    "高 {h}：槽位 {i}（{a:?}）跑出轨外 {r:?}"
                );
                for b in all.iter().skip(i + 1) {
                    assert!(
                        !overlap(*a, *b),
                        "高 {h}：槽位 {a:?} 与 {b:?} 重叠——两个图标会叠在一起、有一个点不到"
                    );
                }
            }
        }
    }

    /// 折叠时，被折掉的项必须恰好是面板组全集；不折叠时不许留下「更多」。
    ///
    /// When folded, exactly the panel group is folded away; when not folded there must be no
    /// "more" button left behind.
    #[test]
    fn rail_hidden_items_are_all_reachable_via_more() {
        let items = rail_items();
        let panel: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.group == 1)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(panel.len(), 6, "面板组条目数变了，判据要跟着改");

        for h in HEIGHTS {
            let slots = rail_layout(rect(h), items);
            match slots.more {
                Some(_) => {
                    assert_eq!(
                        slots.folded, panel,
                        "高 {h}：折叠集合不等于面板组——有项被折掉却够不到"
                    );
                    // 折掉的项不能在流里同时可见，否则等于没折还多占一颗。
                    //
                    // A folded entry must not also be visible in the flow.
                    for (idx, _) in &slots.shown {
                        assert!(!panel.contains(idx), "高 {h}：条目 {idx} 既被折叠又可见");
                    }
                }
                None => assert!(
                    slots.folded.is_empty(),
                    "高 {h}：没有「更多」按钮却记了折叠项"
                ),
            }
        }
    }

    /// 轨宽与按钮的关系：左右各留 10px，图标不顶到轨边。
    ///
    /// Rail width versus button: 10 px on each side, so icons never touch the rail edge.
    #[test]
    fn rail_button_keeps_ten_px_on_each_side() {
        let side = (TOOL_W - RAIL_BTN) * 0.5;
        assert!(
            (side - 10.0).abs() < f32::EPSILON,
            "轨宽 {TOOL_W} 配按钮 {RAIL_BTN} 时左右只剩 {side}px，图标会顶到轨边"
        );
        // 最紧的一档也要留出至少 8px，否则图标与分隔线撞在一起。
        //
        // Even the tightest tier keeps at least 8 px, or icons collide with the dividers.
        let tight = (TOOL_W - RAIL_MIN_BTN) * 0.5;
        assert!(tight >= 8.0, "最紧一档左右只有 {tight}px，太挤");
    }

    /// 每项都有图标与说明，图标不重复，说明可读。
    ///
    /// Every entry has a glyph and a tooltip; glyphs do not repeat and tooltips are non-empty.
    #[test]
    fn every_rail_item_has_a_distinct_glyph_and_tooltip() {
        let items = rail_items();
        let mut glyphs = Vec::new();
        let mut hints = Vec::new();
        for it in items {
            assert!(
                !glyphs.contains(&it.glyph),
                "图标 {:?} 重复出现——两个不同功能会长得一样",
                it.glyph
            );
            glyphs.push(it.glyph);

            let hint = item_hint(it);
            assert!(!hint.trim().is_empty(), "条目 {:?} 没有悬停说明", it.glyph);
            assert!(
                !hints.contains(&hint),
                "说明「{hint}」重复——悬停时分不清是哪一项"
            );
            hints.push(hint);
        }
        // 工具组 6 项 + 面板组 6 项 + 工作流 1 + 外观 1。
        //
        // Six tools, six panel entries, one workflow, one appearance.
        assert_eq!(items.len(), 14, "条目数变了，判据要跟着改");
    }
}
