//! 顶部菜单行与全局输入：菜单/菜单项/四个下拉、键盘与滚轮分发。
//!
//! Top menu row and global input: menus, menu items, the four dropdowns, keyboard and
//! wheel dispatch.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::Bench;
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
        p.rect_filled(tile, 6.0, accent());
        icons::paint(&p, tile.shrink(5.0), Glyph::Sequence, Color32::WHITE);
        x = tile.right() + 10.0;
    }

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
    p.text(
        Pos2::new(doc.left(), doc.center().y - 7.0),
        Align2::LEFT_CENTER,
        doc_name,
        crate::theme::bold(13.0),
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
    p.rect_filled(badge, 3.0, elev());
    p.text(
        Pos2::new(badge.left() + 4.0, badge.center().y),
        Align2::LEFT_CENTER,
        badge_label,
        font(9.0),
        muted(),
    );
    x = doc.right() + 12.0;

    // 缩放：64px 轨道 + 46px 百分比（设计稿 `.zoomc .mini{width:64px} .pct{width:46px}`）。
    // 拖动轨道改 `state.zoom`；点百分比回 100%。轨道上的滑块位置按 0..=400% 映射。
    //
    // Zoom: 64 px track + 46 px readout. Dragging the track sets `state.zoom`; clicking
    // the readout resets to 100%.
    let zoom_pct = (state.zoom * state.fit_scale.max(1e-4) * 100.0).clamp(1.0, 400.0);
    // 设计稿 `.zoomc`：[−][64px 轨道][+] [百分比]。−/+ 各 18px，点一次 ×0.8 / ×1.25。
    //
    // Design `.zoomc`: minus, 64 px track, plus, then the readout. Each tap steps the
    // zoom by 0.8 / 1.25.
    let minus = Rect::from_center_size(Pos2::new(x + 9.0, mid), Vec2::splat(18.0));
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
    let mut x = minus.right() + 4.0;
    let track = Rect::from_min_size(Pos2::new(x, mid - 1.5), Vec2::new(64.0, 3.0));
    let zr = ui.interact(
        Rect::from_min_size(Pos2::new(x - 4.0, mid - 8.0), Vec2::new(72.0, 16.0)),
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
        Vec2::new(46.0, 26.0),
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
    x = plus.right() + 10.0;

    // 中央模块 chips（设计稿 `.seg`：`#26282c` 槽、24px 按钮、选中 `#3a3d43`）。
    // 七个模块横向铺开；「更多」不另设假入口——合成就是设计的「更多」。
    //
    // Centre module chips per `.seg`: a `#26282c` trough with 24 px buttons and a
    // `#3a3d43` active fill. All seven modules are laid out; there is no fake "more"
    // entry -- merge is what the design's "more" leads to.
    // 设计稿的 chips 是五枚：序列 / 去闪 / 显影 / 成片 / 更多（`.seg` 槽内）。
    // 构图 / 声音 / 合成三枚收进「更多」弹出层——七个名字横排会把顶栏撑长，
    // 同时把「常用四页」和「文件级工具」画成同一排。
    //
    // The design's chips are five: sequence / deflicker / develop / render / more.
    // Frame / audio / merge move under "more": seven names in a row stretch the bar and
    // paint "four common pages" and "file-level tools" as one rank.
    const HEAD_CHIPS: [Module; 4] = [
        Module::Sequence,
        Module::Deflicker,
        Module::Develop,
        Module::Render,
    ];
    let more_label = crate::i18n::t!("更多");
    let mut widths = Vec::with_capacity(HEAD_CHIPS.len() + 1);
    for m in HEAD_CHIPS {
        let g = p.layout_no_wrap(crate::i18n::tr(m.cn()).to_string(), font(12.0), text());
        widths.push(g.size().x + 24.0);
    }
    widths.push(
        p.layout_no_wrap(more_label.to_string(), font(12.0), text())
            .size()
            .x
            + 30.0,
    );
    let seg_w: f32 = widths.iter().sum::<f32>() + 2.0 * (widths.len() as f32 - 1.0) + 4.0;
    let seg_h = 28.0;
    let seg_x = x + 16.0;
    if seg_x + seg_w < zone_right_pre(full, &c) {
        let seg = Rect::from_min_size(Pos2::new(seg_x, mid - seg_h / 2.0), Vec2::new(seg_w, seg_h));
        p.rect_filled(seg, 8.0, Color32::from_rgb(0x26, 0x28, 0x2c));
        let mut cx = seg.left() + 2.0;
        for (i, m) in HEAD_CHIPS.iter().enumerate() {
            let r = Rect::from_min_size(
                Pos2::new(cx, seg.top() + 2.0),
                Vec2::new(widths[i], seg_h - 4.0),
            );
            cx += widths[i] + 2.0;
            let resp = ui.interact(r, ui.make_persistent_id(("chip", i)), Sense::click());
            let on = state.module == *m;
            if on {
                p.rect_filled(r, 6.0, Color32::from_rgb(0x3a, 0x3d, 0x43));
            } else if resp.hovered() {
                p.rect_filled(r, 6.0, elev());
            }
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                crate::i18n::tr(m.cn()),
                font(12.0),
                if on || resp.hovered() { text() } else { dim() },
            );
            if resp.clicked() && !on {
                state.set_bench(Bench::Develop);
                state.set_module(*m);
            }
            resp.on_hover_text(format!("{} (Alt+{})", crate::i18n::tr(m.cn()), i + 1));
        }
        // 更多：槽内最后一格，点开列出另外三个模块。
        let r = Rect::from_min_size(
            Pos2::new(cx, seg.top() + 2.0),
            Vec2::new(widths[HEAD_CHIPS.len()], seg_h - 4.0),
        );
        let more_on = !HEAD_CHIPS.contains(&state.module);
        if more_on {
            p.rect_filled(r, 6.0, Color32::from_rgb(0x3a, 0x3d, 0x43));
        }
        ui.scope_builder(
            UiBuilder::new()
                .id_salt("more-chips")
                .max_rect(r)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                let w = &mut ui.style_mut().visuals.widgets;
                w.inactive.weak_bg_fill = Color32::TRANSPARENT;
                w.inactive.bg_stroke = Stroke::NONE;
                w.inactive.fg_stroke = Stroke::new(1.0, if more_on { text() } else { dim() });
                w.hovered.weak_bg_fill = elev();
                w.hovered.bg_stroke = Stroke::NONE;
                w.hovered.fg_stroke = Stroke::new(1.0, text());
                ui.spacing_mut().button_padding = Vec2::new(8.0, 4.0);
                ui.menu_button(
                    RichText::new(format!("{more_label} ▾")).size(SIZE_CHROME_LABEL),
                    |ui| {
                        menu_style(ui);
                        for (i, m) in Module::ALL.iter().enumerate() {
                            if HEAD_CHIPS.contains(m) {
                                continue;
                            }
                            let on = state.module == *m;
                            let label = if on {
                                format!("✓ {}", crate::i18n::tr(m.cn()))
                            } else {
                                format!("   {}", crate::i18n::tr(m.cn()))
                            };
                            if menu_item(ui, &label, &format!("Alt+{}", i + 1)) {
                                state.set_bench(Bench::Develop);
                                state.set_module(*m);
                                ui.close();
                            }
                        }
                    },
                );
            },
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
    let running = state.running_jobs();
    let tid = format!("{}", running);
    let tlabel = crate::i18n::t!("任务");
    let tw = p
        .layout_no_wrap(tlabel.to_string(), font(SIZE_CHROME_LABEL), text())
        .size()
        .x
        + if running > 0 { 30.0 } else { 16.0 };
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
    let gear = Rect::from_center_size(Pos2::new(export.left() - 22.0, mid), Vec2::splat(24.0));
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

    // ☰：五项菜单（文件/编辑/视图/插件/帮助）全部保留在弹出层里。
    //
    // ☰: all five menus stay reachable in the popup.
    let hub = Rect::from_center_size(Pos2::new(gear.left() - 20.0, mid), Vec2::splat(28.0));
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("hub-menu")
            .max_rect(hub)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            let w = &mut ui.style_mut().visuals.widgets;
            w.inactive.weak_bg_fill = Color32::TRANSPARENT;
            w.inactive.bg_stroke = Stroke::NONE;
            w.inactive.fg_stroke = Stroke::new(1.0, muted());
            w.hovered.weak_bg_fill = elev();
            w.hovered.bg_stroke = Stroke::NONE;
            w.hovered.fg_stroke = Stroke::new(1.0, text());
            ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
            ui.menu_button(RichText::new("☰").size(SIZE_TITLE), |ui| {
                menu_style(ui);
                ui.menu_button(crate::i18n::t!("文件"), |ui| file_menu(state, ui, ctx));
                ui.menu_button(crate::i18n::t!("编辑"), |ui| edit_menu(state, ui, ctx));
                ui.menu_button(crate::i18n::t!("视图"), |ui| view_menu(state, ui, ctx));
                ui.menu_button(crate::i18n::t!("插件"), |ui| plugin_menu(state, ui, ctx));
                ui.menu_button(crate::i18n::t!("帮助"), |ui| help_menu(state, ui));
            })
            .response
            .on_hover_text(crate::i18n::t!("菜单"));
        },
    );

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
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).size(SIZE_BODY)).shortcut_text(
            RichText::new(shortcut)
                // 快捷键列走等宽（PS 编辑菜单的整齐感来源）：Ctrl+Z /
                // Shift+Ctrl+Z / Alt+Shift+Ctrl+K 右对齐后自成一条视觉锚线。
                //
                // Shortcut column uses the monospace family (where PS's Edit menu gets
                // its tidiness): right-aligned, the column becomes a visual anchor.
                .font(mono(SIZE_SMALL))
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
