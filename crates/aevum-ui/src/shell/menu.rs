//! 顶部菜单行与全局输入：菜单/菜单项/四个下拉、键盘与滚轮分发。
//!
//! Top menu row and global input: menus, menu items, the four dropdowns, keyboard and
//! wheel dispatch.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::chrome::window_buttons;
use super::dialogs::export_xmp_dialog;
use super::dialogs::import_xmp_dialog;
use super::dialogs::open_dialog;
use super::dialogs::open_photos_dialog;
use super::dialogs::rescan;
use super::library::lib_apply_targets;
use super::tabs::run_history_command;
use super::windows::plugin_menu;
use crate::history;
use crate::icons::{self, Glyph};
use crate::platform;
use crate::state::{AppState, DockTab, Module, Tool};
use crate::theme::{self, *};
use aevum_core::grade::{self};
use egui::{
    Align, Align2, Color32, Context, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui,
    UiBuilder, Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

// ---------------------------------------------------------------- 输入

fn digit_key(i: usize) -> Key {
    match i {
        0 => Key::Num1,
        1 => Key::Num2,
        2 => Key::Num3,
        3 => Key::Num4,
        4 => Key::Num5,
        5 => Key::Num6,
        6 => Key::Num7,
        7 => Key::Num8,
        8 => Key::Num9,
        _ => Key::Num0,
    }
}

pub(super) fn fit_view(state: &mut AppState) {
    state.zoom = 1.0;
    state.pan = Vec2::ZERO;
}

pub(super) fn actual_size(state: &mut AppState) {
    state.zoom = (1.0 / state.fit_scale.max(1e-4)).clamp(0.2, 24.0);
    state.pan = Vec2::ZERO;
}

pub(super) fn handle_input(state: &mut AppState, ctx: &Context) {
    let dropped = ctx.input(|i| i.raw.dropped_files.clone());
    let paths: Vec<std::path::PathBuf> = dropped
        .into_iter()
        .map(|f| f.path().to_path_buf())
        .collect();
    if !paths.is_empty() {
        state.open_paths(ctx, &paths);
        return;
    }

    let (ctrl, alt, shift) =
        ctx.input(|i| (i.modifiers.command, i.modifiers.alt, i.modifiers.shift));
    if ctrl && ctx.input(|i| i.key_pressed(Key::O)) {
        if shift {
            open_photos_dialog(state, ctx);
        } else {
            open_dialog(state, ctx);
        }
        return;
    }
    if ctx.egui_wants_keyboard_input() {
        return;
    }

    if ctrl {
        // Ctrl+Shift+M：插件市场。与菜单里标的快捷键一致。
        if shift && ctx.input(|i| i.key_pressed(Key::M)) {
            state.market_open = true;
            return;
        }
        // Ctrl+K：「预设与快照」浮窗（工具轨首枚图标、显影菜单同款开关）。
        if ctx.input(|i| i.key_pressed(Key::K)) {
            state.show_left = !state.show_left;
            state.settings_dirty = true;
        }
        if ctx.input(|i| i.key_pressed(Key::Z)) {
            // Shift+Ctrl+Z 与 Ctrl+Y 都走命令注册表：键盘、菜单、MCP 撤的是同一步。
            let id = if shift {
                "history.redo"
            } else {
                "history.undo"
            };
            if let Err(e) = state.run_ui_command(ctx, id, &serde_json::json!({})) {
                state.error = Some(e);
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Y))
            && let Err(e) = state.run_ui_command(ctx, "history.redo", &serde_json::json!({}))
        {
            state.error = Some(e);
        }
        if ctx.input(|i| i.key_pressed(Key::S)) {
            state.save_project();
        }
        if ctx.input(|i| i.key_pressed(Key::E)) {
            state.export_current(ctx);
        }
        if ctx.input(|i| i.key_pressed(Key::W)) {
            state.close();
        }
        if ctx.input(|i| i.key_pressed(Key::Num0)) {
            fit_view(state);
        }
        if ctx.input(|i| i.key_pressed(Key::Num1)) {
            actual_size(state);
        }
        if alt {
            for (idx, m) in Module::ALL.iter().enumerate() {
                if ctx.input(|i| i.key_pressed(digit_key(idx))) {
                    state.set_module(*m);
                }
            }
        }
    }

    if !ctrl && !alt {
        for (key, tool) in [
            (Key::H, Tool::Hand),
            (Key::Z, Tool::Zoom),
            (Key::J, Tool::Heal),
            (Key::C, Tool::Crop),
        ] {
            if ctx.input(|i| i.key_pressed(key)) {
                state.set_tool(tool);
            }
        }
        if ctx.input(|i| i.key_pressed(Key::G)) {
            state.grid = !state.grid;
            let on = crate::i18n::tr(if state.grid { "开" } else { "关" });
            history::note("grid", crate::i18n::tr_fmt("网格 {}", &[&on]));
        }
        if ctx.input(|i| i.key_pressed(Key::Backslash)) {
            state.split = !state.split;
            let on = crate::i18n::tr(if state.split { "开" } else { "关" });
            history::note("split", crate::i18n::tr_fmt("前后对比 {}", &[&on]));
        }
        // J：剪切警告循环（LR 同键同义）：关 → 高光 → 两开 → 阴影 → 关。
        if ctx.input(|i| i.key_pressed(Key::J)) {
            state.clip_cycle();
            state.status = match (state.clip_highlights, state.clip_shadows) {
                (false, false) => "剪切警告：关".to_owned(),
                (true, false) => "剪切警告：高光（红罩）".to_owned(),
                (true, true) => "剪切警告：高光 + 阴影".to_owned(),
                (false, true) => "剪切警告：阴影（蓝罩）".to_owned(),
            };
        }
        if ctx.input(|i| i.key_pressed(Key::F)) {
            fit_view(state);
        }
        // 图库有选中行时，评级键作用在全部目标行（勾选优先），当场写回工程文件。
        if let Some((_, _, row)) = (state.dock_tab == DockTab::Library)
            .then(|| state.lib_selected())
            .flatten()
        {
            let organize = |rating: Option<u8>, flag: Option<i8>, label: Option<u8>| {
                aevum_core::library::Organize {
                    rating,
                    flag,
                    label,
                }
            };
            for i in 0..5u8 {
                if ctx.input(|inp| inp.key_pressed(digit_key(i as usize))) {
                    let rating = if row.rating == i + 1 { 0 } else { i + 1 };
                    lib_apply_targets(state, organize(Some(rating), None, None));
                }
            }
            if ctx.input(|i| i.key_pressed(Key::Num0)) {
                lib_apply_targets(state, organize(Some(0), None, None));
            }
            for i in 0..4u8 {
                if ctx.input(|inp| inp.key_pressed(digit_key(i as usize + 5))) {
                    lib_apply_targets(state, organize(None, None, Some(i + 1)));
                }
            }
            if ctx.input(|i| i.key_pressed(Key::P)) {
                let flag = if row.flag == 1 { 0 } else { 1 };
                lib_apply_targets(state, organize(None, Some(flag), None));
            }
            if ctx.input(|i| i.key_pressed(Key::X)) {
                let flag = if row.flag == -1 { 0 } else { -1 };
                lib_apply_targets(state, organize(None, Some(flag), None));
            }
            let d = i8::from(ctx.input(|i| i.key_pressed(Key::ArrowDown)))
                - i8::from(ctx.input(|i| i.key_pressed(Key::ArrowUp)));
            if d != 0 {
                state.lib_step_row(d as i64);
            }
        } else {
            for i in 0..5 {
                if ctx.input(|inp| inp.key_pressed(digit_key(i))) {
                    state.rate_selected(i as u8 + 1);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::Num0)) {
                state.rate_selected(0);
            }
            for i in 0..4 {
                if ctx.input(|inp| inp.key_pressed(digit_key(i + 5))) {
                    state.set_selected_label(i as u8 + 1);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::P)) {
                let next = if state.selected_flag() == 1 { 0 } else { 1 };
                state.set_selected_flag(next);
            }
            if ctx.input(|i| i.key_pressed(Key::X)) {
                let next = if state.selected_flag() == -1 { 0 } else { -1 };
                state.set_selected_flag(next);
            }
        }
    }

    if ctx.input(|i| i.key_pressed(Key::Space)) {
        state.toggle_play();
    }
    // 裁剪态和运镜拖框态下方向键挪取景框（PS/LR 的规矩），其余时候照旧翻帧。
    let arrow = [
        i8::from(ctx.input(|i| i.key_pressed(Key::ArrowRight)))
            - i8::from(ctx.input(|i| i.key_pressed(Key::ArrowLeft))),
        i8::from(ctx.input(|i| i.key_pressed(Key::ArrowDown)))
            - i8::from(ctx.input(|i| i.key_pressed(Key::ArrowUp))),
    ];
    if arrow != [0, 0] && !(state.dock_tab == DockTab::Library && state.lib_sel_file.is_some()) {
        if state.crop_editing() {
            state.crop_nudge(arrow[0] as i64, arrow[1] as i64, if shift { 10 } else { 2 });
        } else if state.motion_editing() {
            state.motion_nudge(arrow[0] as i64, arrow[1] as i64, if shift { 10 } else { 2 });
        } else {
            let dir = if arrow[0] != 0 { arrow[0] } else { arrow[1] };
            state.step(dir as i64 * if shift { 10 } else { 1 });
        }
    }
    if ctx.input(|i| i.key_pressed(Key::Home)) {
        state.set_frame(0);
    }
    if ctx.input(|i| i.key_pressed(Key::End)) {
        state.set_frame(state.frame_count().saturating_sub(1));
    }
    if ctx.input(|i| i.key_pressed(Key::F5)) {
        rescan(state, ctx);
    }
}

// ---------------------------------------------------------------- 菜单行

pub(super) fn menu_row(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let c = platform::chrome();
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.rect_filled(full, 0, header());
    theme::rule(&p, full.left(), full.right(), full.bottom(), border());

    // 无边框窗口：菜单行空白处拖动窗口，双击最大化/还原。
    let drag = ui.interact(
        full,
        ui.make_persistent_id("titlebar"),
        Sense::click_and_drag(),
    );
    if drag.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }

    // 左段起点：macOS 的交通灯占着左上角，品牌区要整体右让。
    let mid = full.center().y;
    let mut x = if c.buttons_left {
        full.left() + c.buttons_zone_w()
    } else {
        full.left() + 12.0
    };

    // 左侧品牌：一枚圆角图标块，仅此而已。应用名搬去了窗口标题与「关于」，
    // 标题栏上文字重排只会让它更像命令面板 —— 图标认识的人自然认识。
    // mac 版连图标块都没有 —— 交通灯之后直接是菜单，品牌在 Dock 里。
    if c.brand_icon {
        let icon_tile = Rect::from_min_size(Pos2::new(x, mid - 9.0), Vec2::splat(18.0));
        p.rect_filled(icon_tile, radius_control(), accent_dim());
        icons::paint(&p, icon_tile.shrink(4.0), Glyph::Sequence, accent());
        x = icon_tile.right() + 8.0;
    }

    // 菜单：紧接品牌之后，间距与内边距按平台。
    let menus_x = x + c.brand_gap;
    let left_rect = Rect::from_min_max(
        Pos2::new(menus_x, full.top()),
        Pos2::new(menus_x + 360.0, full.bottom()),
    );
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("menus")
            .max_rect(left_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            let w = &mut ui.style_mut().visuals.widgets;
            w.inactive.weak_bg_fill = Color32::TRANSPARENT;
            w.inactive.bg_stroke = Stroke::NONE;
            w.inactive.fg_stroke = Stroke::new(1.0, text());
            w.hovered.weak_bg_fill = elev();
            w.hovered.bg_stroke = Stroke::NONE;
            w.active.weak_bg_fill = selected();
            ui.spacing_mut().button_padding = Vec2::new(c.menu_pad_x, 3.0);
            ui.spacing_mut().item_spacing.x = c.menu_gap;
            // 五项，和界面稿同口径：文件 / 编辑 / 视图 / 插件 / 帮助。
            // 原来的「显影」并进「视图 → 显影」，「模块」由标题栏的工作区胶囊接管 ——
            // 七个菜单名横排会把标题栏撑成一个命令面板，标题栏只需要"去哪找"。
            ui.menu_button(
                RichText::new(crate::i18n::t!("文件")).size(c.menu_font),
                |ui| file_menu(state, ui, ctx),
            );
            ui.menu_button(
                RichText::new(crate::i18n::t!("编辑")).size(c.menu_font),
                |ui| edit_menu(state, ui, ctx),
            );
            ui.menu_button(
                RichText::new(crate::i18n::t!("视图")).size(c.menu_font),
                |ui| view_menu(state, ui, ctx),
            );
            ui.menu_button(
                RichText::new(crate::i18n::t!("插件")).size(c.menu_font),
                |ui| plugin_menu(state, ui, ctx),
            );
            ui.menu_button(
                RichText::new(crate::i18n::t!("帮助")).size(c.menu_font),
                |ui| help_menu(state, ui),
            );
        },
    );

    // 右侧：工作区胶囊（▦ 显影 ▾）→ 主按钮「导出…」→ 窗口按钮。
    // 六个模块名不再横铺在标题栏上 —— 那排格子占掉 380px，还把"现在在哪个工作区"
    // 和"这个软件有哪几个工作区"画成了同一排。收进下拉，标题栏才回到标题栏。
    let zone_right = if c.buttons_left {
        full.right() - 16.0
    } else {
        full.right() - c.buttons_zone_w() - 10.0
    };

    // 主按钮：全窗口唯一一颗实心强调色 —— 它是唯一"会产出文件"的动作，
    // 剩下的动作都不该跟它抢同一层视觉重量。
    let export_w = 76.0;
    let export = Rect::from_min_size(
        Pos2::new(zone_right - export_w, mid - 13.0),
        Vec2::new(export_w, 26.0),
    );
    let er = ui.interact(export, ui.make_persistent_id("tb-export"), Sense::click());
    p.rect_filled(
        export,
        radius_control(),
        if er.hovered() {
            accent_hover()
        } else {
            accent()
        },
    );
    p.text(
        export.center(),
        Align2::CENTER_CENTER,
        crate::i18n::t!("导出…"),
        font(SIZE_CHROME_LABEL),
        on_accent(),
    );
    let do_export = er.clicked();
    er.on_hover_text(crate::i18n::t!("导出当前序列 / 照片（Ctrl+E）"));
    if do_export {
        state.export_current(ctx);
    }

    // 设置齿轮：外观/布局/语言的唯一常驻入口。此前只有「视图 → 外观与布局…」
    // 一条路，用户找不到（2026-11-01 反馈「界面自定义与中英切换在哪」）——
    // 全程序只此一颗齿轮，位置固定，认它就行。
    let gear = Rect::from_center_size(Pos2::new(export.left() - 16.0, mid), Vec2::splat(24.0));
    let gr = ui.interact(gear, ui.make_persistent_id("tb-look"), Sense::click());
    if gr.hovered() {
        p.rect_filled(gear, radius_control(), elev());
    }
    icons::paint(
        &p,
        gear.shrink(5.0),
        Glyph::Gear,
        if gr.hovered() { text() } else { muted() },
    );
    let open_look = gr.clicked();
    gr.on_hover_text(crate::i18n::tr_fmt(
        "{}（Ctrl+,）",
        &[&crate::i18n::tr("外观与布局")],
    ));
    if open_look {
        state.look_open = true;
    }

    // 工作区胶囊：紧跟菜单（界面稿 .menu-end + .ws-div + .ws-menu 的次序）——
    // 「现在在哪个工作区」是标题栏的第二个信息，不是右角的第三颗按钮。
    // 一条 1px 竖分隔线把它和菜单隔开，右侧只留导出与窗口钮。
    // 菜单宽度自己量：五个词的 galley 加内边距与间距，别用固定盒子空出一段假距离。
    let menu_labels = ["文件", "编辑", "视图", "插件", "帮助"];
    let menus_w: f32 = menu_labels
        .iter()
        .map(|t| {
            p.layout_no_wrap((*t).to_owned(), font(c.menu_font), text())
                .size()
                .x
                + c.menu_pad_x * 2.0
        })
        .sum::<f32>()
        + c.menu_gap * (menu_labels.len() - 1) as f32;
    let ws_w = 104.0;
    let ws_x = menus_x + menus_w + 14.0;
    let ws = Rect::from_min_size(Pos2::new(ws_x, mid - 13.0), Vec2::new(ws_w, 26.0));
    p.line_segment(
        [
            Pos2::new(ws_x - 7.0, mid - 9.0),
            Pos2::new(ws_x - 7.0, mid + 9.0),
        ],
        Stroke::new(1.0, divider()),
    );
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("ws-menu")
            .max_rect(ws)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            let w = &mut ui.style_mut().visuals.widgets;
            w.inactive.weak_bg_fill = elev();
            w.inactive.bg_stroke = Stroke::NONE;
            w.inactive.fg_stroke = Stroke::new(1.0, text());
            w.hovered.weak_bg_fill = elev_hover();
            w.hovered.bg_stroke = Stroke::NONE;
            w.hovered.fg_stroke = Stroke::new(1.0, text());
            w.active.weak_bg_fill = selected();
            w.active.bg_stroke = Stroke::NONE;
            ui.spacing_mut().button_padding = Vec2::new(8.0, 4.0);
            ui.menu_button(
                RichText::new(format!("▦ {}  ▼", crate::i18n::tr(state.module.cn())))
                    .size(SIZE_CHROME_LABEL),
                |ui| {
                    menu_style(ui);
                    for (i, m) in Module::ALL.iter().enumerate() {
                        let on = state.module == *m;
                        let label = if on {
                            format!("✓ {}", crate::i18n::tr(m.cn()))
                        } else {
                            format!("   {}", crate::i18n::tr(m.cn()))
                        };
                        if menu_item(ui, &label, &format!("Ctrl+Alt+{}", i + 1)) {
                            state.set_module(*m);
                            ui.close();
                        }
                    }
                },
            );
        },
    );

    window_buttons(ui, ctx, full);
}

pub(super) fn menu_style(ui: &mut Ui) {
    let w = &mut ui.style_mut().visuals.widgets;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, text());
    w.hovered.weak_bg_fill = elev_hover();
    w.hovered.bg_stroke = Stroke::NONE;
    w.active.weak_bg_fill = selected();
    ui.spacing_mut().button_padding = Vec2::new(8.0, 4.0);
    ui.spacing_mut().item_spacing.y = 1.0;
}

pub(super) fn menu_item(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
    let label = crate::i18n::tr(label);
    ui.add(
        egui::Button::new(RichText::new(label).size(SIZE_BODY))
            .shortcut_text(RichText::new(shortcut).size(SIZE_SMALL).color(dim())),
    )
    .clicked()
}

fn file_menu(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    menu_style(ui);
    if menu_item(ui, "打开序列…", "Ctrl+O") {
        open_dialog(state, ctx);
        ui.close();
    }
    if menu_item(ui, "导入照片…", "Ctrl+⇧+O") {
        open_photos_dialog(state, ctx);
        ui.close();
    }
    if menu_item(ui, "重新扫描", "F5") {
        rescan(state, ctx);
        ui.close();
    }
    ui.separator();
    let has_seq = state.seq.is_some();
    if ui
        .add_enabled(
            has_seq,
            egui::Button::new(crate::i18n::t!("保存工程")).shortcut_text("Ctrl+S"),
        )
        .clicked()
    {
        state.save_project();
        ui.close();
    }
    if ui
        .add_enabled(has_seq, egui::Button::new(crate::i18n::t!("重新载入工程")))
        .clicked()
    {
        if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
            state.load_project(&root);
        }
        ui.close();
    }
    ui.separator();
    if menu_item(ui, "导入 XMP 预设…", "") {
        import_xmp_dialog(state);
        ui.close();
    }
    if menu_item(ui, "导出 XMP 预设…", "") {
        export_xmp_dialog(state);
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(
            has_seq,
            egui::Button::new(crate::i18n::t!("导出当前帧…")).shortcut_text("Ctrl+E"),
        )
        .clicked()
    {
        state.export_current(ctx);
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(has_seq, egui::Button::new(crate::i18n::t!("关闭序列")))
        .clicked()
    {
        state.close();
        ui.close();
    }
    if menu_item(ui, "退出", "") {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        ui.close();
    }
}

fn edit_menu(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    menu_style(ui);
    // 撤销/重做走命令注册表，和快捷键、历史面板、MCP 同一个入口。
    let undo = state.history.can_undo();
    let redo = state.history.can_redo();
    if ui
        .add_enabled(
            undo,
            egui::Button::new(crate::i18n::t!("撤销")).shortcut_text("Ctrl+Z"),
        )
        .clicked()
    {
        run_history_command(state, ctx, "history.undo", &serde_json::json!({}));
        ui.close();
    }
    if ui
        .add_enabled(
            redo,
            egui::Button::new(crate::i18n::t!("重做")).shortcut_text("Ctrl+Shift+Z"),
        )
        .clicked()
    {
        run_history_command(state, ctx, "history.redo", &serde_json::json!({}));
        ui.close();
    }
    ui.separator();
    if menu_item(ui, "自动曝光", "") {
        state.auto_exposure();
        ui.close();
    }
    if menu_item(ui, "复位全部参数", "") {
        state.reset_params();
        ui.close();
    }
    ui.separator();
    if menu_item(ui, "记录当前关键帧", "") {
        state.add_keyframe();
        ui.close();
    }
    if menu_item(ui, "删除最近关键帧", "") {
        if let Some(i) = state.keyframe_near(usize::MAX / 4) {
            state.remove_keyframe(i);
        }
        ui.close();
    }
    ui.separator();
    let has_seq = state.seq.is_some();
    if ui
        .add_enabled(
            has_seq,
            egui::Button::new(crate::i18n::t!("导出当前帧…")).shortcut_text("Ctrl+E"),
        )
        .clicked()
    {
        state.export_current(ctx);
        ui.close();
    }
}

fn develop_menu(state: &mut AppState, ui: &mut Ui) {
    menu_style(ui);
    ui.menu_button(
        RichText::new(crate::i18n::t!("预设")).size(SIZE_BODY),
        |ui| {
            menu_style(ui);
            for name in grade::PRESET_NAMES {
                if ui.button(RichText::new(*name).size(SIZE_BODY)).clicked() {
                    state.apply_preset(name);
                    ui.close();
                }
            }
            ui.separator();
            if menu_item(ui, "导入 XMP 预设…", "") {
                import_xmp_dialog(state);
                ui.close();
            }
            if menu_item(ui, "导出 XMP 预设…", "") {
                export_xmp_dialog(state);
                ui.close();
            }
        },
    );
    ui.separator();
    if menu_item(ui, "自动曝光", "") {
        state.auto_exposure();
        ui.close();
    }
    if menu_item(ui, "复位", "") {
        state.reset_params();
        ui.close();
    }
    ui.separator();
    if menu_item(ui, "记录关键帧", "") {
        state.add_keyframe();
        ui.close();
    }
}

fn view_menu(state: &mut AppState, ui: &mut Ui, _ctx: &Context) {
    menu_style(ui);
    if menu_item(ui, "外观与布局…", "Ctrl+,") {
        state.look_open = true;
        ui.close();
    }
    // 「显影」原来是标题栏上独立的一项，收进这里：调色命令是"对当前图做什么"，
    // 本来就属于视图菜单这一族，不该和文件/编辑并列。
    ui.menu_button(
        RichText::new(crate::i18n::t!("显影调节")).size(SIZE_BODY),
        |ui| develop_menu(state, ui),
    );
    ui.separator();
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
        &mut state.rulers,
        RichText::new(crate::i18n::t!("标尺")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.grid,
        RichText::new(crate::i18n::t!("网格")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.cross,
        RichText::new(crate::i18n::t!("中心十字")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.split,
        RichText::new(crate::i18n::t!("前后对比")).size(SIZE_BODY),
    );
    ui.separator();
    ui.checkbox(
        &mut state.show_left,
        RichText::new(crate::i18n::t!("预设与快照 (Ctrl+K)")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.show_dock,
        RichText::new(crate::i18n::t!("右侧面板")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.show_strip,
        RichText::new(crate::i18n::t!("右侧竖条")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.show_film,
        RichText::new(crate::i18n::t!("胶片条")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.show_curves,
        RichText::new(crate::i18n::t!("曲线")).size(SIZE_BODY),
    );
}

fn help_menu(state: &mut AppState, ui: &mut Ui) {
    menu_style(ui);
    if menu_item(ui, "快捷键与关于…", "") {
        state.about_open = true;
        ui.close();
    }
}

#[cfg(test)]
mod chrome_labels_tests {
    use super::*;
    use crate::shell::ui;
    use aevum_core::export::OutputMode;
    use aevum_core::queue::QueueStatus;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0))),
            max_texture_side: Some(8192),
            events,
            ..Default::default()
        }
    }

    fn painted_text(ctx: &Context, state: &mut AppState) -> String {
        let mut out = ctx.run_ui(input(vec![]), |panel| ui(state, panel));
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

    /// 图标轨不带字的口径（对齐界面稿：工具轨 / 侧竖条都是纯图标）下，
    /// "这是哪颗"由三处文字兜底：选项栏左端画当前工具名、每颗工具的悬停提示
    /// 非空、dock 六页签全带中文名。
    #[test]
    fn both_icon_rails_carry_their_labels() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        let text = painted_text(&ctx, &mut state);
        assert!(
            text.contains(state.tool.cn()),
            "选项栏没有当前工具名「{}」：\n{text}",
            state.tool.cn()
        );
        for tool in Tool::ALL {
            assert!(!tool.hint().is_empty(), "工具 {} 缺悬停提示", tool.cn());
        }
        for tab in DockTab::ALL {
            assert!(
                text.contains(tab.cn()),
                "dock 缺页签「{}」：\n{text}",
                tab.cn()
            );
        }
    }

    /// 队列表格：列头 + 每行自己的进度。整屏只有一行总进度时，
    /// 排了七八段就看不出哪段在跑、哪段已经好了。
    #[test]
    fn queue_rows_carry_headers_and_their_own_progress() {
        use aevum_core::queue::QueueItem;
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        state.module = Module::Render;
        state.collapsed.remove("render-queue");
        state.queue = vec![
            QueueItem {
                root: "D:/seq/白天".to_owned(),
                out: "D:/out/day.mp4".to_owned(),
                width: 3840,
                height: 2160,
                codec: "h265".to_owned(),
                status: QueueStatus::Done,
                ..Default::default()
            },
            QueueItem {
                root: "D:/seq/夜".to_owned(),
                out: "D:/out/night.mp4".to_owned(),
                width: 1920,
                height: 1080,
                codec: "h264".to_owned(),
                status: QueueStatus::Failed,
                note: "ffmpeg 退出码 1".to_owned(),
                ..Default::default()
            },
            QueueItem {
                root: "D:/seq/等待".to_owned(),
                ..Default::default()
            },
        ];
        let _ = painted_text(&ctx, &mut state);
        let text = painted_text(&ctx, &mut state);
        for want in [
            "状态",
            "序列",
            "规格",
            "进度",
            "完成",
            "100%",
            "失败",
            "排队",
            "3840×2160",
        ] {
            assert!(text.contains(want), "队列表格缺「{want}」：\n{text}");
        }
    }

    /// 续跑这条复选框两种模式都要上屏：静帧补的是交付帧，成片补的是中间帧，
    /// 少摆一边就等于有一半用户不知道跑崩了可以直接接着导。
    /// XMP 旁车反过来，只在静帧模式出现——视频中间帧是过程产物，不给它们发旁车。
    #[test]
    fn resume_checkbox_shows_in_both_modes_and_xmp_only_for_stills() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let label = "断点续跑（跳过已导出的帧）";
        for (mode, wants_xmp) in [(OutputMode::Stills, true), (OutputMode::Video, false)] {
            let mut state = AppState::demo();
            state.install_demo_textures(&ctx);
            state.module = Module::Render;
            state.collapsed.remove("render-out");
            state.render_mode = mode;
            let _ = painted_text(&ctx, &mut state);
            let text = painted_text(&ctx, &mut state);
            assert!(
                text.contains(label),
                "{mode:?} 模式的输出面板没有「{label}」：\n{text}"
            );
            assert_eq!(
                text.contains("写 XMP 旁车"),
                wants_xmp,
                "{mode:?} 模式的旁车复选框不该{}：\n{text}",
                if wants_xmp { "缺席" } else { "出现" }
            );
        }
    }

    /// 续跑的读数走的是「导出线程数完 → 原子量 → 状态栏」这条链，界面线程不干数帧的活。
    /// 这条测试认的是链子两端：没数到东西时不许凭空报数，数到了就得真上屏。
    #[test]
    fn status_bar_reports_the_resume_skip_count() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        state.export_busy = true;
        let _ = painted_text(&ctx, &mut state);
        let plain = painted_text(&ctx, &mut state);
        assert!(
            plain.contains("导出中·写帧"),
            "进度读数本身就没上屏：\n{plain}"
        );
        assert!(!plain.contains("续跑跳过"), "一帧都没跳过却报了数");
        state.set_export_skipped_for_test(312);
        let text = painted_text(&ctx, &mut state);
        assert!(text.contains("续跑跳过 312"), "续跑读数没上屏：\n{text}");
    }
}
