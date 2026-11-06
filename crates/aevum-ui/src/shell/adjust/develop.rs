//! 显影模块的九个调整区：基本 / 曲线 / 颜色混合器 / 颜色分级 / 细节 / 光学 / 效果 /
//! 外观，以及次级小标题与表外字段两个共用件。
//!
//! The nine develop sections: basic, curve, color mixer, color grading, detail, optics,
//! effects and look, plus the two shared pieces (`sub_head` for a subsection heading,
//! `spec_field` for an out-of-table field) and the gradient-rail criteria.

// 共用件（theme / i18n / 兄弟模块的函数）由 mod.rs 的 use 树统一带入。
//
// Shared pieces (theme, i18n, sibling helpers) come in through mod.rs's use tree.
use super::geometry::{adjust_geometry, adjust_optics, adjust_retouch};
use super::*;
/// 分区内的次级小标题（ACR 的 subsection heading）：小号、半粗、弱色，上方留一口气。
/// 与 `group_header` 不同，它不进折叠登记表——折叠始终以九分区组为单位，
/// 小标题只是给一组推子分段。文案由调用方先过 `t!`（宏只吃字面量）。
///
/// A subsection heading inside a section (ACR's style): small, semibold, muted, with a
/// breath above. Unlike `group_header` it is not registered for collapsing -- folding
/// always works on the nine sections; the heading merely bands a group of sliders.
/// Callers translate first with `t!` (the macro only takes literals).
pub(super) fn sub_head(ui: &mut Ui, label: &str) {
    // 子区标题（白平衡 / 影调 / 偏好）与滑块标签拉开体格差：上方 10px 呼吸、
    // 加字距的 10 号 dim 字（ACR 的子区标头读法）——原来它与滑块标签同字号
    // 同位置，读不出「这里换了一组」。
    ui.add_space(10.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
    theme::tracked_text(
        ui.painter(),
        Pos2::new(rect.left() + 1.0, rect.center().y - 6.0),
        label,
        egui::FontId::proportional(10.0),
        dim(),
        1.6,
    );
}

/// 按字段画一条推子：清单里写了表里没有的字段，debug 构建当场炸出来，
/// 发布构建静默跳过（宁可少一根推子，也不要整块面板打不开）。
///
/// Draw one slider by field: a field listed in the UI but missing from the spec table
/// trips a debug assertion, while release builds skip it silently (a missing slider
/// beats a panel that cannot open).
pub(super) fn spec_field(ui: &mut Ui, state: &mut AppState, field: Field) -> bool {
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
pub(super) fn adjust_develop(state: &mut AppState, ui: &mut Ui) {
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
        state.auto_exposure(ui.ctx());
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
        // 自动结果说明是「读数」不是「动作」：蓝色是交互色，一整段蓝字会把
        // 眼睛钉在面板顶部（用户截图实锤三行蓝字压过直方图）。降成 muted 小字。
        ui.add(
            egui::Label::new(RichText::new(note_text).size(SIZE_SMALL).color(muted()))
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
        // 照片风格（LR 的 Profile 位）：所有滑块之下的渲染基调。换风格不动任何
        // 参数值，也不进「N 项调整」徽标——它是基调，不是调整。
        //
        // Photo style (LR's Profile slot): the rendering basis beneath every slider.
        // Switching touches no slider value and never enters the "N adjusted" badge --
        // it is a basis, not an adjustment.
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("风格"))
                    .size(SIZE_LABEL)
                    .color(muted()),
            );
            let current = state.params.style;
            egui::ComboBox::from_id_salt("dev-style")
                .selected_text(crate::i18n::tr(
                    aevum_core::style::NAME_KEYS
                        [current.min(aevum_core::style::COUNT - 1) as usize],
                ))
                .width(112.0)
                .show_ui(ui, |ui| {
                    for (i, key) in aevum_core::style::NAME_KEYS.iter().enumerate() {
                        let idx = i as u8;
                        if ui
                            .selectable_label(state.params.style == idx, crate::i18n::tr(key))
                            .clicked()
                            && state.params.style != idx
                        {
                            state.params.style = idx;
                            state.preview_dirty = true;
                            history::note(
                                "style",
                                crate::i18n::tr_fmt("风格 {}", &[&crate::i18n::tr(key).to_owned()]),
                            );
                        }
                    }
                });
        });
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
    changed |= slider_grad(
        ui,
        ("hsl-h", band),
        "色相",
        &mut state.params.hsl_h[band],
        -100.0,
        100.0,
        0.0,
        0,
        true,
        hsl_band_stops(band, HslKind::Hue),
    );
    changed |= slider_grad(
        ui,
        ("hsl-s", band),
        "饱和度",
        &mut state.params.hsl_s[band],
        -100.0,
        100.0,
        0.0,
        0,
        true,
        hsl_band_stops(band, HslKind::Sat),
    );
    changed |= slider_grad(
        ui,
        ("hsl-l", band),
        "明亮度",
        &mut state.params.hsl_l[band],
        -100.0,
        100.0,
        0.0,
        0,
        true,
        hsl_band_stops(band, HslKind::Lum),
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
pub(super) fn profiles_body(state: &mut AppState, ctx: &Context, ui: &mut Ui) {
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
