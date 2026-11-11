//! 顶部菜单行与全局输入：菜单/菜单项/四个下拉、键盘与滚轮分发。
//!
//! Top menu row and global input: menus, menu items, the four dropdowns, keyboard and
//! wheel dispatch.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::Bench;
use super::chrome::window_buttons;
use super::common::combo_style;
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
            open_photos_dialog(state);
        } else {
            open_dialog(state);
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
            state.request_export_confirm(ctx, false);
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
            // Ctrl+Alt+1..4 切工作台；Alt+1..7 仍是模块。两组必须分开：模块的
            // 0-5 序号契约写在 `ui.module` 命令与文档里，不能让工作台抢走它。
            //
            // Ctrl+Alt+1..4 switches benches; Alt+1..7 still picks modules. The two sets
            // stay apart because the module index contract is documented and shared with
            // the `ui.module` command.
            if ctrl {
                for b in Bench::ALL {
                    if ctx.input(|i| i.key_pressed(digit_key(b.no() - 1))) {
                        state.set_bench(b);
                    }
                }
            } else {
                for (idx, m) in Module::ALL.iter().enumerate() {
                    if ctx.input(|i| i.key_pressed(digit_key(idx))) {
                        state.set_module(*m);
                    }
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

/// 右侧动作区的起点：窗口按钮区宽度之外再留 10px。
///
/// Left edge of the right-hand action cluster: 10 px inside the window-button zone.
fn zone_right_pre(full: Rect, c: &platform::Chrome) -> f32 {
    if c.buttons_left {
        full.right() - 16.0
    } else {
        full.right() - c.buttons_zone_w() - 10.0
    }
}

/// 任务胶囊宽度：文字实测宽 + 内距（有角标时再留 14px 徽标位）。中英文各测各的，
/// 不能写死常量——英文 "Tasks" 比中文「任务」宽，写死就会让齿轮重新压到字上。
///
/// Tasks pill width: measured label plus padding (and a 14 px badge slot when a count is
/// present). Each language is measured; a constant would let the gear drift back onto the
/// longer English label.
fn tasks_pill_width(p: &egui::Painter, running: usize) -> f32 {
    let label = crate::i18n::t!("任务");
    p.layout_no_wrap(label.to_string(), font(SIZE_CHROME_LABEL), text())
        .size()
        .x
        + if running > 0 { 30.0 } else { 16.0 }
}

/// 右簇链式左缘：从导出按钮右缘往左，逐级减去实测宽度与间隙
/// （导出 76 → 任务胶囊 → 齿轮 24）。工具簇的锚点由这里推导，
/// 不再靠目测调一个 `-125` 魔数；语言或角标一变，两段会一起移动。
/// 菜单字排已挪到顶栏最左，链里不再有菜单段。
///
/// Chained left edge of the right cluster, walking left from the export button by measured
/// widths and gaps (export 76 -> tasks pill -> gear 24). The tool cluster anchor
/// is derived from this instead of a hand-tuned `-125`; a language or badge change moves
/// both sections together. The menu bar now lives at the far left, so the chain no longer
/// carries a menu segment.
fn right_chain_left(zone_right: f32, tasks_w: f32) -> f32 {
    const EXPORT_W: f32 = 76.0;
    const PILL_GAP: f32 = 8.0;
    const GEAR_GAP: f32 = 6.0;
    const GEAR_W: f32 = 24.0;
    zone_right - EXPORT_W - PILL_GAP - tasks_w - GEAR_GAP - GEAR_W
}

pub(super) fn menu_row(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let c = platform::chrome();
    let full = ui.max_rect();
    let p = ui.painter().clone();
    // 顶栏底色用 `bar()`（设计稿 `--bar #16171b`，全表最暗一档）：顶栏是「壳」，
    // 必须比 field/card 都深。之前取 header()（面板亮度），整条顶栏比设计稿亮
    // 一整档，就是「设计图与代码不是一套东西」观感的最大来源。
    //
    // The bar fill uses `bar()` (the design's `--bar #16171b`, the darkest step): the top
    // bar is the shell and must sit below field/card. It used to take header() (panel
    // brightness), a full step lighter than the design -- the single biggest source of
    // the "design and code are not the same set" feel.
    p.rect_filled(full, 0, bar());
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

    // 品牌块 24：设计稿 `.mk{width:24px;height:24px;border-radius:6px;background:var(--blue)}`。
    // 蓝底白标志是全窗唯一一块「品牌色」，比旧版 18px + accent_dim 更贴设计稿，
    // 也让左上角在深灰底上有个明确锚点。
    //
    // Brand tile 24 per the design's `.mk`: the one brand-coloured block in the window.
    if c.brand_icon {
        let tile = Rect::from_min_size(Pos2::new(x, mid - 12.0), Vec2::splat(24.0));
        // 圆角从 token 派生：写死 6.0 时「外观 → 圆角」调不动这颗蓝色品牌块，
        // 放大后与其它圆角件不同步（用户 2026-11-10 反馈「弧度像马赛克」）。
        //
        // Derive the corner radius from the theme token: a literal 6.0 left this
        // blue brand tile deaf to the corner-radius knob and out of step with the
        // other rounded elements when zoomed (2026-11-10 user report).
        p.rect_filled(tile, radius_chip().max(2.0), accent());
        icons::paint(&p, tile.shrink(5.0), Glyph::Sequence, Color32::WHITE);
        x = tile.right() + 10.0;
    }

    // 品牌名 + 文档动作簇（设计稿 `.toolbar` 左段：logo 恒晷 → 撤销/重做/面板 →
    // 文档名 → 关闭）。此前顶栏只有 logo 和文档名，整段比设计稿短了约 160px，
    // chips 与工具簇因此整体左移；四枚按钮全部接真实入口，不放摆设。
    //
    // Brand name + document action cluster (the design's `.toolbar` left section:
    // logo/name -> undo/redo/panels -> doc name -> close). The bar used to carry only
    // the logo and the doc name, ~160 px shorter than the design, which pulled the
    // chips and tool cluster to the left. All four buttons hit real entry points.
    let brand_label = crate::i18n::t!("恒晷");
    let brand_galley = p.layout_no_wrap(brand_label.to_string(), crate::theme::bold(13.0), text());
    p.galley(
        Pos2::new(x, mid - brand_galley.size().y * 0.5),
        brand_galley.clone(),
        text(),
    );
    x += brand_galley.size().x + 14.0;

    // 传统菜单字排（用户 2026-11-10：按传统设置选项布局，选项放最左边）：
    // 品牌 → 文件 编辑 视图 插件 帮助，与每款桌面软件一致的顺序。此前五项藏
    // 在右端 ☰ 里，找「导入」「视图开关」要先认出那枚图标；字排放在最左，
    // 第一眼就是熟悉的菜单栏。Undo/Redo 等动作簇紧随其后。
    //
    // Traditional menu bar (user, 2026-11-10: classic options layout, options at the
    // far left): brand -> File Edit View Plugins Help, the order every desktop app
    // uses. The five menus used to hide behind the right-edge ☰; spelled out at the
    // far left they read as the familiar menu bar at first glance.
    let menu_bar = ui.scope_builder(
        UiBuilder::new()
            .id_salt("menubar")
            .max_rect(Rect::from_min_size(
                Pos2::new(x, full.top()),
                Vec2::new(360.0, full.height()),
            ))
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            let w = &mut ui.style_mut().visuals.widgets;
            w.inactive.weak_bg_fill = Color32::TRANSPARENT;
            w.inactive.bg_stroke = Stroke::NONE;
            w.inactive.fg_stroke = Stroke::new(1.0, muted());
            w.hovered.weak_bg_fill = elev();
            w.hovered.bg_stroke = Stroke::NONE;
            w.hovered.fg_stroke = Stroke::new(1.0, text());
            ui.spacing_mut().button_padding = Vec2::new(7.0, 4.0);
            ui.menu_button(
                RichText::new(crate::i18n::t!("文件")).size(SIZE_CHROME_LABEL),
                |ui| {
                    menu_style(ui);
                    file_menu(state, ui, ctx);
                },
            );
            ui.menu_button(
                RichText::new(crate::i18n::t!("编辑")).size(SIZE_CHROME_LABEL),
                |ui| {
                    menu_style(ui);
                    edit_menu(state, ui, ctx);
                },
            );
            ui.menu_button(
                RichText::new(crate::i18n::t!("视图")).size(SIZE_CHROME_LABEL),
                |ui| {
                    menu_style(ui);
                    view_menu(state, ui, ctx);
                    // 工作台切换并入「视图」菜单（用户 2026-11-11「悬浮设计不好看」）：
                    // 画布上不再画第二条导航带，四项带编号，与 Ctrl+Alt+1..4 对应。
                    // 名字先取译文再拼编号，英文模式下组合串里不会残留中文。
                    //
                    // Bench switching lives in the View menu (2026-11-11 feedback): no
                    // second nav strip over the canvas. Numbered rows match Ctrl+Alt+1..4.
                    // Translate the name before joining the number, so English mode never
                    // leaves Chinese inside the composed label.
                    ui.separator();
                    for b in Bench::ALL {
                        let name = crate::i18n::tr(b.cn());
                        let label = format!("{} {}", b.no(), name);
                        let shortcut = format!("Ctrl+Alt+{}", b.no());
                        if menu_item(ui, &label, &shortcut) {
                            state.set_bench(b);
                            ui.close();
                        }
                    }
                },
            );
            ui.menu_button(
                RichText::new(crate::i18n::t!("插件")).size(SIZE_CHROME_LABEL),
                |ui| {
                    menu_style(ui);
                    plugin_menu(state, ui, ctx);
                },
            );
            ui.menu_button(
                RichText::new(crate::i18n::t!("帮助")).size(SIZE_CHROME_LABEL),
                |ui| {
                    menu_style(ui);
                    help_menu(state, ui);
                },
            );
        },
    );
    x = menu_bar.response.rect.right() + 8.0;

    let actions: [(Glyph, bool, &str, &str); 3] = [
        (
            Glyph::Undo,
            state.history.can_undo(),
            "tb-undo",
            "撤销 (Ctrl+Z)",
        ),
        (
            Glyph::Redo,
            state.history.can_redo(),
            "tb-redo",
            "重做 (Ctrl+Shift+Z)",
        ),
        (Glyph::Panels, true, "tb-panels", "切换面板左右"),
    ];
    for (glyph, enabled, id, hint) in actions {
        let r = Rect::from_center_size(Pos2::new(x + 12.0, mid), Vec2::splat(24.0));
        let resp = ui.interact(r, ui.make_persistent_id(id), Sense::click());
        // 禁用态也给一圈极淡的底框：纯 dim 图标会融进背景、看不出这里能点。
        //
        // Even the disabled state gets a faint outline: a bare dim icon melts into
        // the background and hides the fact that a button exists here.
        if !enabled {
            p.rect_stroke(
                r.shrink(1.0),
                radius_control(),
                Stroke::new(1.0, group_ring()),
                egui::StrokeKind::Inside,
            );
        }
        if resp.hovered() && enabled {
            p.rect_filled(r, radius_control(), elev());
        }
        icons::paint(
            &p,
            r.shrink(5.0),
            glyph,
            if !enabled {
                dim()
            } else if resp.hovered() {
                text()
            } else {
                muted()
            },
        );
        if resp.clicked() && enabled {
            match glyph {
                Glyph::Undo => {
                    run_history_command(state, ctx, "history.undo", &serde_json::json!({}));
                }
                Glyph::Redo => {
                    run_history_command(state, ctx, "history.redo", &serde_json::json!({}));
                }
                Glyph::Panels => {
                    state.layout.dock_side = if state.layout.dock_side.is_left() {
                        crate::layout::Side::Right
                    } else {
                        crate::layout::Side::Left
                    };
                }
                _ => {}
            }
        }
        resp.on_hover_text(crate::i18n::tr(hint));
        x += 28.0;
    }
    x += 6.0;

    // 文档块：文件名 + 类型徽标 + 帧数/帧率（设计稿 `.doc{flex-direction:column}`）。
    // 名称取序列根目录名；没有序列时写「未打开序列」，不摆假文件名。
    //
    // Doc block: file name + type badge + frame count / fps. The name comes from the
    // sequence root; with no sequence it says "no sequence", never a fake file name.
    let doc_name = state
        .seq
        .as_ref()
        .and_then(|s| s.root.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| crate::i18n::t!("未打开序列").to_string());
    let doc_w = 200.0;
    let doc = Rect::from_min_size(
        Pos2::new(x, full.top() + 6.0),
        Vec2::new(doc_w, full.height() - 12.0),
    );
    let name_galley = p.layout_no_wrap(doc_name.clone(), crate::theme::bold(13.0), text());
    let title_w = name_galley.size().x;
    p.galley(
        Pos2::new(doc.left(), doc.center().y - 7.0),
        name_galley,
        text(),
    );
    // 副标题：`1,387 帧 · 24 fps · DC-S5M2`（设计稿 `.doc span`）。机型取当前帧 EXIF，
    // 读不到就只写帧数与帧率——不编一个机型出来。
    //
    // Subtitle: `1,387 帧 · 24 fps · DC-S5M2` per the design's `.doc span`. The camera
    // comes from the current frame's EXIF; with no model only frames and fps are shown.
    let model = state
        .exif
        .get(state.selected)
        .and_then(|e| e.model().map(|m| m.trim().to_owned()))
        .filter(|m| !m.is_empty());
    let badge_label = match &model {
        Some(m) => format!(
            "{} · {:.0} fps · {m}",
            crate::i18n::tr_fmt("{} 帧", &[&state.frame_count()]),
            state.output_fps
        ),
        None => crate::i18n::tr_fmt(
            "{} 帧 · {} fps",
            &[&state.frame_count(), &format!("{:.0}", state.output_fps)],
        ),
    };
    let badge_w = p
        .layout_no_wrap(badge_label.clone(), font(9.0), muted())
        .size()
        .x
        + 8.0;
    let badge = Rect::from_min_size(
        Pos2::new(doc.left(), doc.center().y + 8.0),
        Vec2::new(badge_w, 14.0),
    );
    // 元数据是纯文本，不是禁用按钮：去掉那块 pill 底。
    //
    // Metadata is plain text, not a disabled button: drop the pill fill.
    p.text(
        Pos2::new(badge.left(), badge.center().y),
        Align2::LEFT_CENTER,
        badge_label,
        font(9.0),
        muted(),
    );
    // 关闭序列 ×：走 `state.close()`（存工程后清空，不动磁盘素材）。没有序列时
    // 不画空按钮——一个点了没东西可关的 × 就是假控件。
    //
    // Close-sequence X: calls `state.close()` (saves the project, clears in-memory
    // state, never touches footage on disk). With no sequence it is not drawn at all.
    if state.seq.is_some() {
        let close_r = Rect::from_center_size(
            Pos2::new(doc.left() + title_w.max(badge_w) + 12.0, mid),
            Vec2::splat(20.0),
        );
        let cr = ui.interact(
            close_r,
            ui.make_persistent_id("tb-close-doc"),
            Sense::click(),
        );
        if cr.hovered() {
            p.rect_filled(close_r, radius_control(), elev());
        }
        icons::paint(
            &p,
            close_r.shrink(6.0),
            Glyph::Close,
            if cr.hovered() { text() } else { muted() },
        );
        if cr.clicked() {
            state.close();
        }
        cr.on_hover_text(crate::i18n::t!("关闭当前序列"));
        x = close_r.right() + 14.0;
    } else {
        x = doc.right() + 12.0;
    }

    // 右簇链左缘：先把导出→任务→齿轮的实测宽算出来，中段（模块下拉、工具簇）都从
    // 这里推导，各段共用一份宽度，语言/角标一变一起挪，绝不会互相叠压（窄窗回归
    // 根因：左链只按递增的绝对 `x` 一路右推、没有上界，越过右簇就压到「任务」胶囊上）。
    //
    // Right cluster's chained left edge computed up front so every middle section
    // (module dropdown, tool cluster) derives from it; they share one measured width and
    // can never overlap. The narrow-window regression was the left chain pushing right by
    // an unbounded absolute `x` until it crushed the tasks pill.
    let running = state.running_jobs();
    let tasks_w = tasks_pill_width(&p, running);
    let chain_left = right_chain_left(zone_right_pre(full, &c), tasks_w);

    // 中段排布（实测定，见下）：**模块下拉 > 缩放簇 > 工具簇**，且缩放簇有「完整 / 紧凑
    // / 不画」三档。1080px 实测：左链画到文档标题右缘在 x≈559，右簇链左缘在 778，中段
    // 可用约 203px。改造前这里只能三选一，1080 下缩放读数与工具簇整段消失——用户一眼
    // 能看出的功能退步。所以下拉收一档（124→96）、缩放收一档（去掉两端 −/+，只留
    // 「轨道 + 百分比」），1080 下「下拉 96 + 间隙 6 + 紧凑缩放 96 = 198 ≤ 203」两者并存。
    // 1440 设计稿宽度下仍是「下拉 + 完整缩放 + 六枚工具」全在。
    //
    // Middle layout, set by measurement: **module dropdown > zoom cluster > tool cluster**,
    // with the zoom cluster in three tiers (full / compact / absent). Measured at 1080 px the
    // left chain reaches x ~ 559 at the doc title's right edge while the right cluster starts
    // at 778, leaving ~203 px of middle. The previous cut let only one of the three survive,
    // which dropped the zoom readout and the whole tool cluster at 1080 -- a visible
    // regression. So the dropdown loses a tier (124 -> 96) and the zoom loses a tier (no
    // minus/plus, track plus readout only): "dropdown 96 + 6 gap + compact zoom 96 = 198 <=
    // 203" keeps both at 1080. At the design's 1440 px all three remain.
    let avail = chain_left - 8.0 - (x + 8.0);
    // 下拉两档：中段宽裕用设计稿的 124，窄窗收到 96（「序列管理 / Sequence」+ 箭头都
    // 放得下），把剩下的宽度让给缩放读数——1080 下必须同时看到下拉与百分比。
    //
    // Dropdown in two widths: the design's 124 when the middle is roomy, 96 on narrow
    // windows (fits "序列管理 / Sequence" plus the caret), handing the remainder to the
    // zoom readout -- at 1080 px both the dropdown and the percentage must stay visible.
    let combo_w = if avail >= 320.0 { 124.0 } else { 96.0 };
    let combo_left = x + 8.0;
    // 只在左链与右簇链之间确实放得下时才画，更窄的窗宁可收起也不压别人。
    //
    // Painted only when it truly fits between the left chain and the right cluster;
    // a narrower window drops it rather than crushing its neighbours.
    let combo_drawn = combo_left + combo_w + 8.0 <= chain_left - 8.0;

    // 缩放：64px 轨道 + 46px 百分比（设计稿 `.zoomc .mini{width:64px} .pct{width:46px}`）。
    // 拖动轨道改 `state.zoom`；点百分比回 100%。轨道上的滑块位置按 0..=400% 映射。
    //
    // Zoom: 64 px track + 46 px readout. Dragging the track sets `state.zoom`; clicking
    // the readout resets to 100%.
    let zoom_pct = (state.zoom * state.fit_scale.max(1e-4) * 100.0).clamp(1.0, 400.0);
    // 窄窗紧凑：≤1340px 时缩放轨道 64→48、百分比 46→40，给中段（模块下拉与工具簇）
    // 多让出二十余像素——否则左段把中段挤没，两段在窄窗双双让位。宽窗仍是设计稿原尺寸。
    //
    // Narrow-window compacting: at <=1340 px the zoom track shrinks 64->48 and the
    // readout 46->40, freeing ~24 px for the middle (module dropdown and tool cluster);
    // otherwise the left section starves the middle and both give way. Wide windows keep
    // the design's original sizes.
    let narrow = full.width() < 1340.0;
    let zoom_track_w = if narrow { 48.0 } else { 64.0 };
    let zoom_pct_w = if narrow { 40.0 } else { 46.0 };
    // 缩放簇的起点：模块下拉画出来时接在它右缘（+6 间隙），否则直接接文档标题右缘。
    // 三种档位都用绝对坐标绘制，代码里的先后顺序只影响可读性，不影响最终 x 顺序。
    //
    // The zoom cluster's origin: after the dropdown's right edge (+6 gap) when it is
    // painted, otherwise straight after the doc title. All tiers use absolute coordinates,
    // so the order of the code below is about readability only, not final x placement.
    let mid_left = if combo_drawn { combo_left + combo_w } else { x } + 6.0;
    // 完整档：`[−][轨道][+] [百分比]`；紧凑档：`[轨道][百分比]`（去掉两端 −/+ 按钮，
    // 拖动轨道仍改缩放、点读数仍回 100%）。两档宽度各算一份，先试完整再退紧凑。
    //
    // Full tier is minus + track + plus + readout; the compact tier is track + readout
    // (no minus/plus; the track still drags and the readout still resets to 100%). Both
    // widths are measured up front: try full, then fall back to compact.
    let zoom_full_w = 51.0 + zoom_track_w + zoom_pct_w;
    let zoom_min_w = zoom_track_w + 8.0 + zoom_pct_w;
    let mid_right = chain_left - 8.0;
    let zoom_full = mid_left + zoom_full_w <= mid_right;
    let zoom_compact = !zoom_full && mid_left + zoom_min_w <= mid_right;
    // 三档守卫：完整 → 紧凑 → 不画。窄窗 900px 的真实现象是缩放百分比与「任务」胶囊
    // 连写成「任务%」——左链只按递增的绝对 `x` 右推、没有上界的直接后果；现在有了上界，
    // 900 下中段只剩约 21px，三档都放不下就整段不画，绝不压右簇。
    //
    // Three-tier guard: full -> compact -> absent. The 900 px symptom was the zoom readout
    // running straight into the tasks pill -- the direct result of an unbounded absolute
    // `x`. With the bound in place the middle is only ~21 px at 900, so no tier fits and
    // the whole cluster is dropped instead of crushing the right cluster.
    if zoom_full {
        // 设计稿 `.zoomc`：[−][64px 轨道][+] [百分比]。−/+ 各 18px，点一次 ×0.8 / ×1.25。
        //
        // Design `.zoomc`: minus, 64 px track, plus, then the readout. Each tap steps the
        // zoom by 0.8 / 1.25.
        let minus = Rect::from_center_size(Pos2::new(mid_left + 9.0, mid), Vec2::splat(18.0));
        let mr = ui.interact(minus, ui.make_persistent_id("tb-zoom-out"), Sense::click());
        if mr.hovered() {
            p.rect_filled(minus, radius_control(), elev());
        }
        p.text(
            minus.center(),
            Align2::CENTER_CENTER,
            "−",
            font(SIZE_CHROME_LABEL),
            if mr.hovered() { text() } else { muted() },
        );
        if mr.clicked() {
            state.zoom = (state.zoom * 0.8).clamp(0.2, 24.0);
        }
        // 簇内游标用独立名 `zx`：绝不 shadow 外层 `x`，否则被守卫跳过时外层 `x`
        // 会带着缩放宽度继续右推，右簇照样被压。
        //
        // The in-cluster cursor is a separate `zx` so it never shadows the outer `x`;
        // otherwise a skipped cluster would still push `x` rightwards by the zoom width.
        let zx = minus.right() + 4.0;
        let track = Rect::from_min_size(Pos2::new(zx, mid - 1.5), Vec2::new(zoom_track_w, 3.0));
        let zr = ui.interact(
            Rect::from_min_size(
                Pos2::new(zx - 4.0, mid - 8.0),
                Vec2::new(zoom_track_w + 8.0, 16.0),
            ),
            ui.make_persistent_id("tb-zoom"),
            Sense::click_and_drag(),
        );
        p.rect_filled(track, 2.0, slider_rail());
        let frac = (zoom_pct / 400.0).clamp(0.0, 1.0);
        let knob_x = track.left() + track.width() * frac;
        p.circle_filled(Pos2::new(knob_x, track.center().y), 4.5, knob());
        if (zr.dragged() || zr.clicked())
            && let Some(pos) = zr.interact_pointer_pos()
        {
            let f = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
            let want = (f * 4.0).max(0.01);
            state.zoom = (want / state.fit_scale.max(1e-4)).clamp(0.2, 24.0);
        }
        let pct = Rect::from_min_size(
            Pos2::new(track.right() + 8.0, mid - 13.0),
            Vec2::new(zoom_pct_w, 26.0),
        );
        let pr = ui.interact(pct, ui.make_persistent_id("tb-pct"), Sense::click());
        if pr.hovered() {
            p.rect_filled(pct, radius_control(), elev());
        }
        p.text(
            pct.center(),
            Align2::CENTER_CENTER,
            format!("{:.0}%", zoom_pct),
            font(SIZE_CHROME_LABEL),
            if pr.hovered() { text() } else { muted() },
        );
        if pr.clicked() {
            state.zoom = (1.0 / state.fit_scale.max(1e-4)).clamp(0.2, 24.0);
        }
        pr.on_hover_text(crate::i18n::t!("点击回到 100%"));
        let plus = Rect::from_center_size(Pos2::new(pct.right() + 12.0, mid), Vec2::splat(18.0));
        let xr = ui.interact(plus, ui.make_persistent_id("tb-zoom-in"), Sense::click());
        if xr.hovered() {
            p.rect_filled(plus, radius_control(), elev());
        }
        p.text(
            plus.center(),
            Align2::CENTER_CENTER,
            "+",
            font(SIZE_CHROME_LABEL),
            if xr.hovered() { text() } else { muted() },
        );
        if xr.clicked() {
            state.zoom = (state.zoom * 1.25).clamp(0.2, 24.0);
        }
        // 只有真的画了完整档，外层游标才推进到簇右缘。
        //
        // Advance the outer cursor to the cluster's right edge only when the full tier
        // was actually painted.
        x = (mid_left + zoom_full_w).max(x);
    } else if zoom_compact {
        // 紧凑档：只画「轨道 + 百分比」。−/+ 按钮在这一档不画（它们各占 20 余像素，正是
        // 1080 下读不到百分比的原因）；拖动轨道照旧改缩放，点读数照旧回 100%。
        //
        // Compact tier: track plus readout only. The minus/plus buttons are omitted here
        // (their ~20 px each is exactly what squeezed the readout out at 1080); dragging
        // the track still sets the zoom and clicking the readout still resets to 100%.
        let track =
            Rect::from_min_size(Pos2::new(mid_left, mid - 1.5), Vec2::new(zoom_track_w, 3.0));
        let zr = ui.interact(
            Rect::from_min_size(
                Pos2::new(mid_left - 4.0, mid - 8.0),
                Vec2::new(zoom_track_w + 8.0, 16.0),
            ),
            ui.make_persistent_id("tb-zoom"),
            Sense::click_and_drag(),
        );
        p.rect_filled(track, 2.0, slider_rail());
        let frac = (zoom_pct / 400.0).clamp(0.0, 1.0);
        let knob_x = track.left() + track.width() * frac;
        p.circle_filled(Pos2::new(knob_x, track.center().y), 4.5, knob());
        if (zr.dragged() || zr.clicked())
            && let Some(pos) = zr.interact_pointer_pos()
        {
            let f = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
            let want = (f * 4.0).max(0.01);
            state.zoom = (want / state.fit_scale.max(1e-4)).clamp(0.2, 24.0);
        }
        let pct = Rect::from_min_size(
            Pos2::new(track.right() + 8.0, mid - 13.0),
            Vec2::new(zoom_pct_w, 26.0),
        );
        let pr = ui.interact(pct, ui.make_persistent_id("tb-pct"), Sense::click());
        if pr.hovered() {
            p.rect_filled(pct, radius_control(), elev());
        }
        p.text(
            pct.center(),
            Align2::CENTER_CENTER,
            format!("{:.0}%", zoom_pct),
            font(SIZE_CHROME_LABEL),
            if pr.hovered() { text() } else { muted() },
        );
        if pr.clicked() {
            state.zoom = (1.0 / state.fit_scale.max(1e-4)).clamp(0.2, 24.0);
        }
        pr.on_hover_text(crate::i18n::t!("点击回到 100%"));
        x = (pct.right() + 10.0).max(x);
    }

    // 右簇链左缘已在本段之前算好（见上面 `chain_left`），模块下拉与工具簇都从这里
    // 推导，两段共用一份实测宽度。
    //
    // The right cluster's chained left edge was computed before this section (see
    // `chain_left` above); both the module dropdown and the tool cluster derive from it.

    // 模块切换统一收进单枚下拉（版式决策 2026-11-11）：菜单行不再摆 4 枚模块 chips
    // 与画布任务卡（flow_panel）构成两套并存切换器——任务卡是唯一主导航，下拉保住
    // 全部 7 个模块可达（功能零损失，Alt+1..7 的序号契约不变）。
    //
    // Module switching collapses into one dropdown (2026-11-11 layout decision): the menu
    // row no longer hosts four module chips competing with the canvas task card
    // (flow_panel) -- the card is the single primary navigator and the dropdown keeps all
    // seven modules reachable (no lost function; the Alt+1..7 index contract is unchanged).

    // 模块下拉用**绝对定位子 scope**（照抄上面 `menubar` 的写法）：`menu_row` 收到的
    // `ui` 属于 `Panel::top("menu-row").exact_size(44)`，egui 0.37 用
    // `Layout::top_down(Align::Min)` 建 Ui 并把 clip 收成 44px 高；`ui.add_space(160.0)`
    // 在 top_down 下推进的是**垂直**游标，控件落到 y≈top+160 被面板裁掉，于是这枚
    // ComboBox 从来没上过屏（截图确证）。绝对 rect 只借用 ui 做交互与绘制，坐标由
    // 自己算，垂直方向的 top_down 游标再也影响不到它。
    //
    // The module dropdown uses an **absolutely positioned sub-scope** (same shape as the
    // `menubar` scope above): `menu_row` receives the `ui` of
    // `Panel::top("menu-row").exact_size(44)`, and egui 0.37 builds that Ui with
    // `Layout::top_down(Align::Min)` clipping it to 44 px high. `ui.add_space(160.0)`
    // advances the **vertical** cursor under top_down, so the widget landed at
    // y ~ top+160, clipped away -- the ComboBox never reached the screen (screenshot
    // confirmed). An absolute rect borrows the ui only for interaction and painting,
    // so the vertical cursor can no longer affect it.
    // 几何量（`combo_w` / `combo_left` / `combo_drawn`）已在缩放簇之前算好，见上面的
    // 中段排布：这里只负责画。
    //
    // The geometry (`combo_w` / `combo_left` / `combo_drawn`) was computed before the zoom
    // cluster -- see the middle layout above; this block only paints.
    if combo_drawn {
        ui.scope_builder(
            UiBuilder::new()
                .id_salt("head-modules-scope")
                .max_rect(Rect::from_min_size(
                    Pos2::new(combo_left, full.top()),
                    Vec2::new(combo_w, full.height()),
                ))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                // 下拉按钮本体按设计稿的面式样式画（实底 + 发丝边，hover 提一档），
                // 选中项只靠 `selectable_label` 的选中态表达——不要 U+25A6 / U+2713
                // 前缀，那两个字不在 egui 默认字族里，会渲染成豆腐块。
                //
                // The button body gets the design's filled surface with a hairline (one
                // step brighter on hover); the active entry is expressed by
                // `selectable_label`'s own selected state -- no U+25A6 / U+2713 prefixes,
                // which the default font family lacks and would render as tofu boxes.
                let w = &mut ui.style_mut().visuals.widgets;
                w.inactive.weak_bg_fill = elev();
                w.inactive.bg_stroke = Stroke::new(1.0, border());
                w.hovered.weak_bg_fill = elev_hover();
                w.hovered.bg_stroke = Stroke::new(1.0, border());
                egui::ComboBox::from_id_salt("head-modules")
                    .selected_text(crate::i18n::tr(state.module.cn()))
                    .width(combo_w - 20.0)
                    .show_ui(ui, |ui| {
                        combo_style(ui);
                        for m in Module::ALL.iter() {
                            let on = state.module == *m;
                            if ui.selectable_label(on, crate::i18n::tr(m.cn())).clicked() {
                                state.set_module(*m);
                            }
                        }
                    });
            },
        );
        // 游标必须推进：下面工具簇的 `tc_after_chips` 靠它给这段预留宽度，否则工具
        // 簇会画到下拉上面去。取 `max` 是因为缩放簇可能画在下拉右侧（更靠右的一件才
        // 决定中段的真实右缘）。
        //
        // The cursor must advance: the tool cluster's `tc_after_chips` below relies on it
        // to reserve this span, otherwise the icons would paint over the dropdown. `max`
        // because the zoom cluster may sit right of the dropdown -- the rightmost piece
        // decides where the middle really ends.
        x = (combo_left + combo_w).max(x);
    }

    // 工具簇（设计稿 `.toolcluster`，html 750 行）：模块下拉右侧的六枚工具图标。
    // 每枚是 28×28 的 `.ibtn`（图标 16、圆角 `--rk` 6、簇内 gap 8），两侧各一条
    // `.vdiv`（1×20，`--hair`）细隔线；选中 = `--blue-soft`（accent 18% 罩在顶栏
    // `--bar` 上）+ 底缘 2px 强调条（`.ibtn.on` 的 `inset 0 -2px 0 var(--blue)`）。
    // 此前工具轨默认收起后这簇图标跟着消失，切工具只剩键盘——加回顶栏，工具
    // 永远有一个可见的落点，与设计稿顶栏构图逐枚对齐。
    //
    // The tool cluster (the design's `.toolcluster`, html line 750): six tool icons to
    // the right of the module dropdown. Each is a 28x28 `.ibtn` (16 px icon, `--rk` 6
    // radius, 8 px gap), flanked by two 1x20 `.vdiv` hairlines (`--hair`); the active one
    // takes `--blue-soft` (18% accent over the bar fill) plus a 2 px accent strip on the
    // bottom edge (`.ibtn.on`'s `inset 0 -2px 0 var(--blue)`). With the tool rails folded
    // away by default this cluster used to vanish and tools became keyboard-only --
    // bringing it back gives every tool a visible home in the top bar, per the design.
    // 设计稿在 ≤1400px 用媒体查询整簇隐藏。用户明确要求「图标常驻」，所以这里不
    // 隐藏，改成压缩：空间不够时 28/8 收成 24/4，宁可小一点也不让工具消失。
    // 旧实现用严格 `<` 做空间判据，右锚后边界恰好相等就把整簇静默吞掉。
    // 只有压缩后仍然放不下（中段被模块下拉与文档标题吃满）时才整簇让位，见下面的
    // `tc_fits`——那一档是「宁可收起也不压右簇」的最后手段，不是这套默认行为。
    //
    // The design hides the whole cluster under 1400 px via a media query. The user
    // explicitly wants the icons to stay, so instead of hiding we compact: 28/8 becomes
    // 24/4 when space is tight. The old strict `<` space test silently swallowed the
    // whole cluster once right-anchoring made the boundary exactly equal. It folds away
    // only when the compact size still does not fit (the middle is taken by the module
    // dropdown and the doc title) -- see `tc_fits` below, which is the "drop rather than
    // crush the right cluster" last resort, not the default behaviour.
    // 右簇留白锚点：视觉比对（顶栏右半 2×）实测工具簇落在目标左侧约 95px，根因是
    // 右簇估宽太保守、锚点被中段右缘的 max() 吃掉。这段历史值已由下面的链式推导取代
    // （`tc_right_limit`），不再有任何目测常量。
    //
    // Right-of-centre anchor: a 2x visual comparison measured the cluster landing ~95 px
    // left of the reference because the right cluster's width was over-estimated and the
    // anchor got eaten by the middle's `max()`. That hand-tuned value is gone -- the
    // chained `tc_right_limit` below replaced it, with no eyeballed constant left.
    //
    // 工具簇右锚由右簇链左缘推导，留 8px 呼吸。此前是目测调出的 `-125` 常量：
    // 它与「导出-任务-齿轮-菜单」的实际宽度无关，语言一变就可能与齿轮/任务胶囊
    // 叠压（B1 的同源隐患）。链式之后两段共用同一份实测宽度，中英都自洽。
    //
    // The tool cluster's right anchor is derived from the right cluster's chained left
    // edge with 8 px of air. It used to be a hand-tuned `-125`, unrelated to the real
    // export-tasks-gear-menu widths, so a language change could overlap the gear (the
    // same root cause as B1). Both sections now share one measured width.
    let tc_right_limit = chain_left - 8.0;
    // chips 已收进下拉：工具簇锚在文档标题右侧（x+10 是标题行右缘）。
    let tc_after_chips = x + 10.0;
    let room = tc_right_limit - tc_after_chips;
    let (tc_btn, tc_gap, tc_icon) = if room < 6.0 * 28.0 + 5.0 * 8.0 + 18.0 {
        (24.0, 4.0, 14.0)
    } else {
        (28.0, 8.0, 16.0)
    };
    let tc_w = Tool::ALL.len() as f32 * tc_btn + (Tool::ALL.len() as f32 - 1.0) * tc_gap;
    // 右锚：设计稿顶栏是「左段 / 中段 / 右段」的弹性排布，工具簇紧贴右侧动作簇，
    // 不是紧跟在 chips 后面。之前按左排画，簇整体比目标偏左约 300px——用户说的
    // 「图标全被搞丢了」有一半是这个位置问题。空间不够时才退回 chips 后面。
    //
    // Right-anchored: the design's bar is a flex layout of left / centre / right
    // sections, with the tool cluster hugging the right action group instead of
    // following the chips. Left-aligned painting put it ~300 px left of the reference
    // -- half of the "icons are gone" report was this position. It falls back to just
    // after the chips only when there is not enough room.
    let tc_left = (tc_right_limit - tc_w - 18.0).max(tc_after_chips);
    // 空间判据：右锚（`tc_right_limit - tc_w - 18`）被 `tc_after_chips` 抬起来时，说明
    // 中段已被下拉/标题吃满，簇再画就会压进右簇（任务胶囊/齿轮/导出）。此时整簇收起
    // ——这是「宁可收起也不压别人」的最后一档；`full.width() > 900` 只保证极窄窗不画，
    // 挡不住 900~1200 这段被中段挤满的情况。
    //
    // Space test: when the right anchor (`tc_right_limit - tc_w - 18`) is raised by
    // `tc_after_chips`, the middle is already full (dropdown / title), so drawing the
    // cluster would push it into the right cluster (tasks pill, gear, export). The whole
    // cluster folds away -- the last tier of "drop rather than crush". The
    // `full.width() > 900` gate alone only covers extremely narrow windows and cannot
    // stop the 900-1200 band where the middle is saturated.
    let tc_fits = tc_left + tc_w + 1.0 <= tc_right_limit;
    if full.width() > 900.0 && tc_fits {
        // 左隔线：`.vdiv{width:1px;height:20px;background:var(--hair)}`。
        p.rect_filled(
            Rect::from_min_size(Pos2::new(tc_left, mid - 10.0), Vec2::new(1.0, 20.0)),
            0.0,
            border(),
        );
        let mut tx = tc_left + 1.0 + tc_gap;
        for (ti, t) in Tool::ALL.iter().enumerate() {
            let r = Rect::from_min_size(Pos2::new(tx, mid - tc_btn * 0.5), Vec2::splat(tc_btn));
            tx += tc_btn + tc_gap;
            let resp = ui.interact(r, ui.make_persistent_id(("top-tool", ti)), Sense::click());
            let on = state.tool == *t;
            if on {
                // `--blue-soft: rgba(10,132,255,.18)`：accent 以 18% 透明罩在顶栏底上，
                // 等价于把 alpha 46 的 accent 直接叠画在已铺好的 bar() 上。
                //
                // `--blue-soft` is 18% accent over the bar fill; painting the accent at
                // alpha 46 over the already-filled bar is the same blend.
                let a = accent();
                p.rect_filled(
                    r,
                    radius_control(),
                    Color32::from_rgba_unmultiplied(a[0], a[1], a[2], 46),
                );
                // `inset 0 -2px 0 var(--blue)`：底缘 2px 强调条；圆角 6 的两端各收
                // 4px，避免直角条戳出圆角外。
                //
                // The 2 px bottom accent strip per the inset shadow; inset 4 px on both
                // ends so the square strip stays inside the 6 px rounded corners.
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(r.left() + 4.0, r.bottom() - 2.0),
                        Pos2::new(r.right() - 4.0, r.bottom()),
                    ),
                    1.0,
                    accent(),
                );
            } else if resp.hovered() {
                // `.ibtn:hover{background:var(--raised)}`。
                p.rect_filled(r, radius_control(), elev());
            }
            icons::paint(
                &p,
                Rect::from_center_size(r.center(), Vec2::splat(tc_icon)),
                t.glyph(),
                if on || resp.hovered() {
                    text()
                } else {
                    muted()
                },
            );
            if resp.clicked() {
                state.set_tool(*t);
            }
            resp.on_hover_text(crate::i18n::tr(t.hint()).to_string());
        }
        // 右隔线。
        p.rect_filled(
            Rect::from_min_size(Pos2::new(tx, mid - 10.0), Vec2::new(1.0, 20.0)),
            0.0,
            border(),
        );
    }

    // 右侧动作簇：☰ 菜单 → 齿轮 → 「导出…」→ 窗口按钮（设计稿右端图标 + 主按钮）。
    // 菜单字排收进 ☰，五项一个不少；导出仍是全窗唯一实心强调色。
    //
    // Right cluster: ☰ menu -> gear -> export -> window buttons. The menu word row lives
    // behind ☰ with every entry intact; export stays the only accent-filled button.
    let zone_right = zone_right_pre(full, &c);

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
        state.request_export_confirm(ctx, false);
    }

    // 任务胶囊（设计稿右端「任务 14」）：进行中的后台任务数，点它切到任务工作台。
    // 角标为 0 时只留一个「任务」入口，不画一个空的 0。
    //
    // The tasks pill (the design's "任务 14"): count of running background jobs, click to
    // switch to the tasks bench. With none running it stays a plain entry, no empty 0.
    // `running` 与胶囊宽度已在工具簇锚点处算过，这里复用同一份——两段必须用
    // 同一个数，否则链式推导与绘制会各按各的宽度排，又叠到一起。
    //
    // `running` and the pill width were measured at the tool-cluster anchor; reuse them.
    // Both sections must agree on one number or the chain and the painting drift apart.
    let tid = format!("{}", running);
    let tlabel = crate::i18n::t!("任务");
    let tw = tasks_w;
    let pill = Rect::from_min_size(
        Pos2::new(export.left() - 8.0 - tw, mid - 13.0),
        Vec2::new(tw, 26.0),
    );
    let tr = ui.interact(pill, ui.make_persistent_id("tb-tasks"), Sense::click());
    if tr.hovered() {
        p.rect_filled(pill, 6.0, elev());
    }
    p.text(
        Pos2::new(pill.left() + 8.0, pill.center().y),
        Align2::LEFT_CENTER,
        tlabel,
        font(SIZE_CHROME_LABEL),
        if tr.hovered() { text() } else { muted() },
    );
    if running > 0 {
        let badge = Rect::from_min_size(
            Pos2::new(pill.right() - 20.0, pill.center().y - 7.0),
            Vec2::new(14.0, 14.0),
        );
        p.rect_filled(
            badge,
            4.0,
            Color32::from_rgba_unmultiplied(245, 184, 61, 41),
        );
        p.text(
            badge.center(),
            Align2::CENTER_CENTER,
            &tid,
            font(9.0),
            warn(),
        );
    }
    if tr.clicked() {
        state.set_bench(Bench::Tasks);
    }
    tr.on_hover_text(crate::i18n::t!("后台任务"));

    // 齿轮：外观/布局/语言的唯一常驻入口。
    //
    // The gear: the single permanent entry to appearance / layout / language.
    // 齿轮钉在任务胶囊**左缘之外**（留 6px），不再用 `export.left()-22` 这个
    // 绝对锚点：后者落在胶囊文字右段上，且齿轮后画，把「任务 / Tasks」最后一
    // 两个字盖花（中英都中）。链式位置随语言实测宽度一起变。
    //
    // The gear sits outside the tasks pill's left edge (6 px of air) instead of the
    // absolute `export.left()-22`: that anchor landed on the label's right end and,
    // being painted later, smeared the last glyphs of 任务 / Tasks in both languages.
    let gear = Rect::from_center_size(Pos2::new(pill.left() - 6.0 - 12.0, mid), Vec2::splat(24.0));
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

    // ☰ 已移除：五个菜单提升为顶栏最左的传统菜单字排（2026-11-10），右簇只剩
    // 齿轮 → 任务 → 导出，链式推导同步去掉菜单段。
    //
    // The ☰ is gone: the five menus moved to the far-left traditional menu bar
    // (2026-11-10); the right cluster keeps gear -> tasks -> export and the chain
    // drops the menu segment accordingly.

    window_buttons(ui, ctx, full);
}

pub(super) fn menu_style(ui: &mut Ui) {
    // PS 式菜单选中态（用户点名参照）：hover/选中都是**强调色实底 + 白字**，
    // 与中性灰底的对比一步拉满——旧版 hover 只抬 6% 灰，浅色主题下几乎看不见
    // （用户截图「选择效果与底色不反差」）。Adobe 自己的文档写明：蓝底高反差
    // 是刻意设计，中性界面上最醒目、又不干扰照片色彩判断。
    let w = &mut ui.style_mut().visuals.widgets;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, text());
    w.hovered.weak_bg_fill = accent();
    w.hovered.bg_fill = accent();
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.0, on_accent());
    w.active.weak_bg_fill = accent_hover();
    w.active.bg_fill = accent_hover();
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.0, on_accent());
    // PS 编辑菜单的间距口径（用户截图 272x670 实测）：
    // - 项高 24：正文 13 的行高 18 + `button_padding` 竖 4×2 − egui 按钮自带
    //   描边 2 = 24（实测 3px 时只有 22）；
    // - 项间 0：PS 的菜单项是紧贴的，留 1px 会让整列读成「有缝的表格」；
    // - 左右 8：文字不贴边，但菜单自身也不加外边距。
    //
    // PS Edit-menu spacing (measured on the user's 272x670 screenshot): 24 px rows =
    // the 18 px line of a 13 px body plus 2x4 px padding minus egui's own 2 px button
    // stroke (measured 22 px at 3 px padding), zero gap between rows (PS rows touch; a
    // 1 px gap reads as a gapped table), 8 px horizontal.
    ui.spacing_mut().button_padding = Vec2::new(8.0, 4.0);
    ui.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
    // 弹层最小宽 232：贴屏幕右缘的菜单（缩放/视图）会被窗口边界钳宽，快捷键列
    // 被挤到弹层外面画（用户截图 7 实锤）——给足最小宽，快捷键永远在层内。
    //
    // A 232 px popup minimum: menus near the window's right edge (zoom / view) get
    // width-clamped and the shortcut column painted outside the surface (user
    // screenshot 7). The minimum keeps shortcuts inside the layer.
    ui.set_min_width(232.0);
}

pub(super) fn menu_item(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
    menu_item_enabled(ui, label, shortcut, true)
}

/// 菜单项统一走这里：**字号恒 SIZE_BODY**、快捷键恒 65% 白——hover 蓝底与
/// 暗底两种状态下都可读（dim() 在蓝底上不可读）。旧版禁用项走 add_enabled
/// 裸按钮、没设字号，落到 egui 默认——用户截图里菜单字号/字重不一的根因。
///
/// Every menu item funnels through here: label size is fixed at SIZE_BODY and the
/// shortcut at 65% white, readable on both the hover-blue fill and the dark rest
/// state (dim() disappears on blue). Disabled items used to go through bare
/// add_enabled buttons with no size, falling to egui's default -- the root cause of
/// the mixed type sizes in the user's screenshot.
pub(super) fn menu_item_enabled(ui: &mut Ui, label: &str, shortcut: &str, enabled: bool) -> bool {
    let label = crate::i18n::tr(label);
    let shortcut_color = Color32::from_white_alpha(166);
    // 菜单行是 chrome：W4 字号 +1 不波及——BODY/SMALL 各回退一档，保住
    // PS 口径的 24px 行高（ps_menu_specs 判据 24±1 钉着）。
    //
    // Menu rows are chrome: the W4 +1 type bump does not reach here -- BODY/SMALL
    // step back one so the PS-spec 24 px row height survives (pinned by
    // ps_menu_specs at 24±1).
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).size(SIZE_BODY - 1.0)).shortcut_text(
            RichText::new(shortcut)
                // 快捷键列走等宽（PS 编辑菜单的整齐感来源）：Ctrl+Z /
                // Shift+Ctrl+Z / Alt+Shift+Ctrl+K 右对齐后自成一条视觉锚线。
                //
                // Shortcut column uses the monospace family (where PS's Edit menu gets
                // its tidiness): right-aligned, the column becomes a visual anchor.
                .font(mono(SIZE_SMALL - 1.0))
                .color(shortcut_color),
        ),
    )
    .clicked()
}

/// 带子菜单的菜单项：与普通项同高同字，右端多一枚 `▶`。
///
/// PS 的编辑菜单里，能展开的项右端有箭头，不能展开的留空——两者左端起齐、
/// 右侧都由快捷键列或箭头收边，整列因此读作一张表而不是一列按钮。
///
/// A menu item that opens a submenu: same height and type as a plain item, with a `▶`
/// at the right end. In PS's Edit menu the expandable rows carry an arrow and the rest
/// stay blank; both start flush left and are closed off by the shortcut column or the
/// arrow, so the column reads as a table rather than a stack of buttons.
pub(super) fn menu_submenu<R>(ui: &mut Ui, label: &str, add_contents: impl FnOnce(&mut Ui) -> R) {
    let label = crate::i18n::tr(label);
    let _ = ui.menu_button(RichText::new(label).size(SIZE_BODY), add_contents);
}

/// 分组分隔线：PS 在项与项之间留一段空白再画一根细线，而不是紧贴上一项。
///
/// Group separator: PS leaves a band of air around the hairline instead of butting it
/// against the previous row.
pub(super) fn menu_sep(ui: &mut Ui) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, divider());
    ui.add_space(4.0);
}

fn file_menu(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    menu_style(ui);
    if menu_item(ui, "打开序列…", "Ctrl+O") {
        open_dialog(state);
        ui.close();
    }
    if menu_item(ui, "导入照片…", "Ctrl+⇧+O") {
        open_photos_dialog(state);
        ui.close();
    }
    if menu_item(ui, "重新扫描", "F5") {
        rescan(state, ctx);
        ui.close();
    }
    menu_sep(ui);
    let has_seq = state.seq.is_some();
    if menu_item_enabled(ui, "保存工程", "Ctrl+S", has_seq) {
        state.save_project();
        ui.close();
    }
    if menu_item_enabled(ui, "重新载入工程", "", has_seq) {
        if let Some(root) = state.seq.as_ref().map(|s| s.root.clone()) {
            state.load_project(&root);
        }
        ui.close();
    }
    menu_sep(ui);
    if menu_item(ui, "导入 XMP 预设…", "") {
        import_xmp_dialog(state);
        ui.close();
    }
    if menu_item(ui, "导出 XMP 预设…", "") {
        export_xmp_dialog(state);
        ui.close();
    }
    menu_sep(ui);
    if menu_item_enabled(ui, "导出当前帧…", "Ctrl+E", has_seq) {
        state.request_export_confirm(ctx, true);
        ui.close();
    }
    menu_sep(ui);
    if menu_item_enabled(ui, "关闭序列", "", has_seq) {
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
    if menu_item_enabled(ui, "撤销", "Ctrl+Z", undo) {
        run_history_command(state, ctx, "history.undo", &serde_json::json!({}));
        ui.close();
    }
    if menu_item_enabled(ui, "重做", "Ctrl+Shift+Z", redo) {
        run_history_command(state, ctx, "history.redo", &serde_json::json!({}));
        ui.close();
    }
    menu_sep(ui);
    if menu_item(ui, "自动曝光", "") {
        state.auto_exposure(ctx);
        ui.close();
    }
    if menu_item(ui, "复位全部参数", "") {
        state.reset_params();
        ui.close();
    }
    menu_sep(ui);
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
    menu_sep(ui);
    let has_seq = state.seq.is_some();
    if menu_item_enabled(ui, "导出当前帧…", "Ctrl+E", has_seq) {
        state.request_export_confirm(ctx, true);
        ui.close();
    }
}

fn develop_menu(state: &mut AppState, ui: &mut Ui) {
    menu_style(ui);
    menu_submenu(ui, "预设", |ui| {
        menu_style(ui);
        for name in grade::PRESET_NAMES {
            if ui.button(RichText::new(*name).size(SIZE_BODY)).clicked() {
                state.apply_preset(name);
                ui.close();
            }
        }
        menu_sep(ui);
        if menu_item(ui, "导入 XMP 预设…", "") {
            import_xmp_dialog(state);
            ui.close();
        }
        if menu_item(ui, "导出 XMP 预设…", "") {
            export_xmp_dialog(state);
            ui.close();
        }
    });
    menu_sep(ui);
    if menu_item(ui, "自动曝光", "") {
        state.auto_exposure(ui.ctx());
        ui.close();
    }
    if menu_item(ui, "复位", "") {
        state.reset_params();
        ui.close();
    }
    menu_sep(ui);
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
    menu_submenu(ui, "显影调节", |ui| develop_menu(state, ui));
    menu_sep(ui);
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
    menu_sep(ui);
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
    // 画布铺满是画布这一族的观看方式，跟标尺/网格放一起，不和面板开关混排。
    //
    // Canvas fill is a way of *viewing the canvas*, same family as rulers/grid --
    // keep it with those, not with the panel toggles below.
    ui.checkbox(
        &mut state.canvas_fill,
        RichText::new(crate::i18n::t!("画布铺满窗口")).size(SIZE_BODY),
    );
    menu_sep(ui);
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
    // 工具轨 / 选项栏 / 流程条三个状态早就存在（show_rails / show_options /
    // show_flow），只是菜单里没有入口，用户开不了——断头路。这里把它们接回：
    // 勾选即开，取消即收，与 state 里的默认值（设计稿不画这三条带子）互补。
    //
    // The tool rails / options bar / flow strip have existed as state all along
    // (show_rails / show_options / show_flow) but had no menu entry, so nobody
    // could turn them on -- a dead end. Wire them back here: check to show,
    // uncheck to hide; the state defaults (strips hidden) still match the design.
    ui.checkbox(
        &mut state.show_rails,
        RichText::new(crate::i18n::t!("显示工具轨")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.show_options,
        RichText::new(crate::i18n::t!("显示工具选项栏")).size(SIZE_BODY),
    );
    ui.checkbox(
        &mut state.show_flow,
        RichText::new(crate::i18n::t!("显示流程步骤条")).size(SIZE_BODY),
    );
    // 视图工具条：工具条上的减号会把它收起，这里是唯一的重开入口
    //（与上面那组「显示…」开关同组）。
    //
    // View toolbar: its minus button hides it, so this is the only way back;
    // grouped with the "Show …" switches above.
    ui.checkbox(
        &mut state.show_viewtools,
        RichText::new(crate::i18n::t!("显示视图工具条")).size(SIZE_BODY),
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
    menu_sep(ui);
    // 菜单项文案随「有没有可装的新版本」变：查到了就把结论摆在菜单上
    // （「有新版本 vX.Y.Z…」），否则维持「检查更新…」。
    //
    // 为什么不在这里显示远端版本号以外的东西：菜单是"去哪找"，一行结论已经够；
    // 版本号是唯一能让用户立刻判断"要不要现在装"的信息。
    //
    // The menu item's label follows "is there something installable": a found update puts
    // the conclusion on the menu itself ("new version vX.Y.Z ..."), otherwise it stays
    // "check for updates ...".
    //
    // Why nothing beyond the version: the menu answers "where do I go", and one line of
    // conclusion is enough -- the version number is the only fact a user needs in order to
    // decide "install now or later".
    let label = match state.update_available() {
        Some(v) => crate::i18n::tr_fmt("有新版本 {}…", &[&v]),
        None => crate::i18n::t!("检查更新…").to_owned(),
    };
    // 不走 `menu_item`：那个帮手会再翻一次 `tr(label)`，而这里的 label 已经是译文。
    //
    // Not `menu_item`: that helper runs `tr(label)` again, and this label is already
    // translated.
    let clicked = ui
        .add(egui::Button::new(RichText::new(label).size(SIZE_BODY)))
        .clicked();
    if clicked {
        state.update_open = true;
        // 从这个进程还没查过就顺手查一次：用户点「检查更新」的意思就是要结论，
        // 打开一扇写着「还没有检查过」的空窗是没把他当回事。
        //
        // Start a check here if this process has not checked yet: clicking "check for
        // updates" means "give me a conclusion", and opening an empty window that says
        // "never checked" is not taking the click seriously.
        if state.update_check.is_none() && state.update_phase == crate::state::UpdatePhase::Idle {
            state.manual_update_check(ui.ctx());
        }
        ui.close();
    }
    // 程序旁边有 `.old`（安装脚本留下的上一版）才给回滚：点了必然有效的项才配出现在
    // 菜单里，否则用户点完得到一句「找不到上一版」。
    //
    // Only offer a rollback when `<exe>.old` sits next to the program (the previous build
    // the install script kept): a menu item that always works is the only kind that
    // belongs there, or the click ends in "no previous version found".
    if state.has_rollback_backup() && menu_item(ui, "回滚到上一版…", "") {
        state.rollback_confirm = true;
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
        // 选项栏现在默认收起（设计稿没有这条带子），手型工具下不出现。本判据问的是
        // 「选项栏画出来时有没有当前工具名」，所以显式把它打开再取证——不能因为
        // 默认收起就断言一段根本不画的东西。
        //
        // The options bar is folded away by default now (the design has no such strip) and
        // does not appear under the hand tool. This assertion asks whether the bar *names
        // the current tool when it is drawn*, so the bar is switched on explicitly -- an
        // assertion must not demand text from something that is not painted.
        state.show_options = true;
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

    /// 视图菜单必须把四个早已有状态、却没有入口的布局开关接回来（show_rails /
    /// show_options / show_flow / canvas_fill）：状态在而入口无，用户永远开不了，
    /// 是断头路。菜单画得出这四行字，就是入口存在性的判据；勾选态由 egui
    /// checkbox 自带，这里不重复断言勾选图形。
    ///
    /// The view menu must wire back the four layout switches that existed as state but
    /// had no entry (show_rails / show_options / show_flow / canvas_fill): state without
    /// an entry is a dead end nobody can open. The menu painting these four labels is
    /// the evidence that the entries exist; the checkmark itself is egui's, not ours to
    /// assert again.
    #[test]
    fn view_menu_carries_the_four_layout_switches() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        let mut out = ctx.run_ui(input(vec![]), |ui| view_menu(&mut state, ui, &ctx));
        let mut text = String::new();
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape {
                text.push_str(t.galley.text());
                text.push('\n');
            }
        }
        for want in [
            "显示工具轨",
            "显示工具选项栏",
            "显示流程步骤条",
            "画布铺满窗口",
        ] {
            assert!(text.contains(want), "视图菜单缺「{want}」：\n{text}");
        }
        // 与 painted_text 同一条口径：textures_delta 不清会在 Drop 时炸断言。
        out.textures_delta.clear();
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

/// PS 口径菜单的绘制级判据：项高 24、行间 0、快捷键列等宽、分隔线上下各 4px 空白。
///
/// 菜单是弹出层，`--shot` 拍不到打开态；这组测试在测试 UI 里真画一次菜单内容，
/// 用几何断言钉住四项规格，比人工目检可靠，也进六关门禁。
///
/// Painting-level criteria for the PS-style menu: 24 px rows, zero row gap, a
/// monospaced shortcut column, and 4 px of air above and below each separator. Menus
/// are popups that `--shot` cannot capture open, so these tests paint real menu content
/// in a test UI and pin the four spec points geometrically -- they run in the six-gate
/// check and beat eyeballing.
#[cfg(test)]
mod ps_menu_specs {
    use super::*;

    /// 画一批菜单项并量行几何：每行高 24，行间 0（下一行顶端 == 上一行底端）。
    ///
    /// Paint a batch of items and measure row geometry: every row is 24 px tall and rows
    /// touch (next top == previous bottom).
    #[test]
    fn menu_rows_are_24_tall_and_touch() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        // 预热一帧：字体图集在首帧建账，不预热的话首帧画的 galley 会带着
        // 未应用的 TexturesDelta 被 drop（与 bench_ui 的 WARMUP 同一理由）。
        //
        // One warm-up frame: the font atlas is built on the first pass, and a galley
        // painted before that carries an unapplied TexturesDelta (same reason as
        // bench_ui's WARMUP).
        let mut warm = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 400.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |ui| {
                menu_style(ui);
                let _ = menu_item(ui, "打开序列…", "Ctrl+O");
            },
        );
        warm.textures_delta.clear();
        let mut rows: Vec<Rect> = Vec::new();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 400.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |ui| {
                menu_style(ui);
                for i in 0..3usize {
                    let before = ui.cursor().top();
                    let _ = menu_item(ui, "打开序列…", "Ctrl+O");
                    let after = ui.cursor().top();
                    rows.push(Rect::from_min_max(
                        Pos2::new(0.0, before),
                        Pos2::new(320.0, after),
                    ));
                    let _ = i;
                }
            },
        );
        out.textures_delta.clear();
        assert_eq!(rows.len(), 3);
        for r in &rows {
            let h = (r.height() * 100.0).round() / 100.0;
            assert!((23.0..=26.0).contains(&h), "菜单行高 {h}px，PS 口径是 24±1");
        }
        // 行间 0：第二行顶端贴着第一行底端（±0.5px 容差）。
        let gap01 = rows[1].top() - rows[0].bottom();
        let gap12 = rows[2].top() - rows[1].bottom();
        assert!(
            gap01.abs() <= 0.5,
            "第 1/2 行之间有 {gap01}px 空隙，PS 是 0"
        );
        assert!(
            gap12.abs() <= 0.5,
            "第 2/3 行之间有 {gap12}px 空隙，PS 是 0"
        );
    }

    /// 分隔线上下各留约 4px 空白：PS 的分组线不是紧贴上一项的。
    ///
    /// Separators keep ~4 px of air above and below: PS group lines do not butt against
    /// the previous row.
    #[test]
    fn menu_separator_carries_air() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        // 与上一条同一理由：先预热一帧建账字体图集。
        //
        // Same reason as the row test: warm the font atlas first.
        let mut warm = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 400.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |ui| {
                menu_style(ui);
                let _ = menu_item(ui, "打开序列…", "Ctrl+O");
            },
        );
        warm.textures_delta.clear();
        let mut gap_before = 0.0f32;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 400.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |ui| {
                menu_style(ui);
                let _ = menu_item(ui, "打开序列…", "Ctrl+O");
                let before = ui.cursor().top();
                menu_sep(ui);
                let after = ui.cursor().top();
                // menu_sep 的真实总占用：上 4 + 线 1 + 下 4 = 9。
                // （cursor.top 推进量就是它占的竖向空间，不掺菜单项自身高度。）
                gap_before = after - before;
            },
        );
        out.textures_delta.clear();
        // 总占 9px = 上留白 4 + 线 1 + 下留白 4：PS 的分组线在项与项之间
        // 是「一段空白 + 细线 + 一段空白」，不是紧贴上一项的一条缝。
        //
        // 9 px total = 4 above + 1 line + 4 below: PS group separators are
        // "air + hairline + air", not a slit butted against the previous row.
        assert_eq!(
            gap_before, 9.0,
            "分隔线总占 {gap_before}px，PS 口径是 9（上 4 + 线 1 + 下 4）"
        );
    }
}
