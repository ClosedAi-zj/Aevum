//! 浮动窗口：菜单段/内置动作/插件菜单/市场/调色窗口/关于。
//!
//! Floating windows: menu sections, built-in actions, plugin menu, marketplace,
//! grading window, about.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::adjust::copies_group;
use super::common::{side_divider, side_head, side_note, side_row};
use super::library::group_title;
use super::menu::menu_item;
use super::menu::menu_style;
use super::toolbar::bar_button;
use super::toolbar::bar_chip;
use super::toolbar::bar_chip_verbatim;
use super::toolbar::bar_primary;
use crate::icons::{self, Glyph};
use crate::layout::StatusSlot;
use crate::platform;
use crate::state::{AppState, UpdatePhase};
use crate::theme::{self, *};
use egui::{Align, Context, Layout, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui, Vec2};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

/// 这些浮窗在 `settings.json` 里的键。改名字等于丢掉用户已经调好的位置，
/// 所以这些字符串是**对外契约**的一部分，不要顺手重命名。
///
/// Keys of these floating windows in `settings.json`. Renaming one throws away the
/// position the user already arranged, so these strings are part of the **external
/// contract** -- do not rename them casually.
pub(super) const WIN_MARKET: &str = "market";
pub(super) const WIN_LOOK: &str = "look";
pub(super) const WIN_ABOUT: &str = "about";
pub(super) const WIN_PRESETS: &str = "presets";
/// 更新窗口的位置同样落盘：它可能是用户唯一会开两次以上的浮窗（先看一眼，
/// 过几天想起来再装），位置能记住比每次居中更顺手。
///
/// The update window's position is persisted too: it may be the one floating window a
/// user opens more than once (glance now, install days later), and a remembered
/// position beats re-centring every time.
pub(super) const WIN_UPDATE: &str = "update";

/// 该浮窗上次落在哪儿；没存过就用 `fallback`（各窗的首开位置）。
///
/// `assume` 是夹取用的假定窗口尺寸（`Window` 的尺寸要 show 完才知道，而位置得
/// show 之前给）。宁可夹粗一点：真被夹歪了，用户拖一下就回到他想要的位置；不夹
/// 则可能整窗落在已拔掉的显示器上，任何操作都摸不到它。
///
/// Where the window last sat, or `fallback` (each window's first-open spot) if it has
/// never been dragged. `assume` is the window size used for clamping -- a `Window`'s
/// size is only known after `show`, but the position must be supplied before it. Better
/// to clamp roughly: if a clamp lands slightly off, one drag fixes it; without clamping
/// the whole window can end up on a monitor that is no longer plugged in, unreachable.
fn restored_pos(state: &AppState, ctx: &Context, key: &str, fallback: Pos2, assume: Vec2) -> Pos2 {
    let view = ctx.input(|i| i.viewport_rect());
    match state.win_pos.get(key) {
        Some(p) => {
            let v = crate::settings::clamp_win_pos(
                *p,
                [assume.x, assume.y],
                [view.min.x, view.min.y, view.width(), view.height()],
            );
            Pos2::new(v[0], v[1])
        }
        None => fallback,
    }
}

/// 把浮窗这一帧的位置记下来（拖动中不记，见 `AppState::note_win_pos`）。
///
/// Record where the window sits this frame (skipped while dragging, see
/// `AppState::note_win_pos`).
fn remember_pos(state: &mut AppState, ctx: &Context, key: &str, rect: Rect) {
    // 按着键不记：拖动中坐标每帧都变，那一刻写文件等于拿 `settings.json` 当草稿纸。
    // 松手后的第一帧才是稳定值。`any_down` 比「正在拖这个窗」更宽，但宽的那一侧
    // 正是我们要的——多等一帧没有代价，写爆文件有。
    //
    // Skip while a button is held: coordinates change every frame during a drag, and
    // writing then turns `settings.json` into scratch paper. The first frame after
    // release is the stable value. `any_down` is broader than "dragging this window",
    // and that broader side is what we want -- one extra frame is free, a write storm
    // is not.
    state.note_win_pos(key, rect.min, ctx.input(|i| i.pointer.any_down()));
}

/// 浮窗标题栏的统一口径：底色接管成 panel，**只圆上面两角**。
///
/// 此前这里写死 `corner_radius(0.0)`：方角标题条压在圆角窗体上，窗的上两角
/// 被削成直角、下两角还是圆的——四个角弧度不一致就是这么来的。上两角取
/// `radius_window()`（与窗体同源），下两角归零交给窗体本体，整窗一套弧度。
fn title_frame() -> egui::Frame {
    let r = radius_window() as u8;
    egui::Frame::new()
        .fill(panel())
        .stroke(Stroke::NONE)
        .corner_radius(egui::CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        })
        .inner_margin(egui::Margin::symmetric(12, 8))
}

/// 窗内分节的软分割线：`theme::soft_rule` 的 UI 包装，占 9px 高、线居中。
///
/// egui 自带的 `ui.separator()` 是通栏实线，和浮窗边框顶死，读作「切开」；
/// 这里换成缩进浅线，读作「分节」。
fn soft_sep(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 9.0), Sense::hover());
    theme::soft_rule(ui.painter(), rect.left(), rect.right(), rect.center().y);
}

/// 菜单里的小标题：比正文小一号、压暗，读作"下面这些属于这一类"。
fn menu_section(ui: &mut Ui, title: &str) {
    ui.add_space(3.0);
    ui.label(RichText::new(title).size(SIZE_SMALL).color(dim()));
}

/// 内置插件对应的真实动作。
///
/// 这层映射留在宿主而不是清单里：清单是给第三方作者写的格式，
/// 不该出现"宿主内部命令名"这种只有本程序知道的东西。
fn builtin_action(id: &str) -> Option<&'static str> {
    match id {
        "aevum.hdr-merge" => Some("hdr.merge"),
        "aevum.pano" => Some("pano.merge"),
        "aevum.lens-correct" => Some("lens.set"),
        _ => None,
    }
}

pub(super) fn plugin_menu(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    menu_style(ui);

    // 先把要显示的东西抄出来，再画：树里存的是下标，画的时候要写 state，
    // 直接借 `state.registry` 会和 `state.status` 的可变借打架。
    struct Row {
        id: String,
        label: String,
        tag: String,
        desc: String,
    }
    let tree = state.registry.menu_tree();
    let rows: Vec<(String, Vec<Row>)> = tree
        .iter()
        .map(|g| {
            let items = g
                .items
                .iter()
                .map(|i| {
                    let p = &state.registry.all()[*i];
                    Row {
                        id: p.manifest.plugin.id.clone(),
                        label: p.manifest.menu_label().to_owned(),
                        tag: p.manifest.plugin.source.cn().to_owned(),
                        desc: p.manifest.plugin.description.clone(),
                    }
                })
                .collect();
            (group_title(&g.group).to_owned(), items)
        })
        .collect();
    let bad = state.registry.errors().len();

    if rows.is_empty() && bad == 0 {
        ui.label(
            RichText::new(crate::i18n::t!("还没有安装插件"))
                .size(SIZE_BODY)
                .color(dim()),
        );
    }
    // 输入范围：这一组插件作用在什么对象上。默认「当前窗口」，会话内保持。
    // 内置动作（HDR 合并 / 全景 / 镜头校正）只吃当前序列，选文件或文件夹时置灰并说明；
    // 第三方插件三档都放开，由 MCP 侧按范围取帧。
    //
    // Input scope: what this plugin run applies to. Defaults to the current window and is
    // kept for the session. Built-in actions (HDR merge / panorama / lens correction) only
    // take the current sequence, so they grey out with an explanation for file/folder;
    // third-party plugins allow all three scopes and let the MCP side gather frames.
    if !rows.is_empty() {
        menu_section(ui, "输入范围");
        ui.horizontal_wrapped(|ui| {
            for (value, label) in [(0u8, "当前窗口"), (1, "文件"), (2, "文件夹")] {
                if bar_chip(ui, label, state.plugin_input_scope == value).clicked() {
                    state.plugin_input_scope = value;
                }
            }
        });
    }

    let mut open_market = false;
    let mut fire: Option<(String, String)> = None;
    for (title, items) in &rows {
        menu_section(ui, title);
        for r in items {
            // 内置动作只支持「当前窗口」：换了范围就禁用，并把原因挂在悬停里，
            // 而不是让用户点下去才发现没反应（设计文档 §7 红线 1：不做假控件）。
            //
            // Built-in actions only support the current window: disable them for other
            // scopes and explain on hover, instead of letting the click silently do nothing
            // (design spec section 7, red line 1: no fake controls).
            let scope_ok = builtin_action(&r.id).is_none() || state.plugin_input_scope == 0;
            let resp = ui.add_enabled(
                scope_ok,
                egui::Button::new(RichText::new(&r.label).size(SIZE_BODY))
                    .shortcut_text(RichText::new(&r.tag).size(SIZE_SMALL).color(dim())),
            );
            let resp = if !scope_ok {
                resp.on_disabled_hover_text(crate::i18n::t!("该插件不支持此输入范围"))
            } else if r.desc.is_empty() {
                resp
            } else {
                resp.on_hover_text(&r.desc)
            };
            if resp.clicked() && scope_ok {
                fire = Some((r.id.clone(), r.label.clone()));
            }
        }
    }
    if bad > 0 {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("{} 份清单有问题", &[&bad]))
                .size(SIZE_SMALL)
                .color(danger()),
        );
    }
    soft_sep(ui);
    if state.plugin_result.is_some() && menu_item(ui, "清除插件效果", "") {
        state.plugin_result = None;
        ui.close();
    }
    if menu_item(ui, "管理插件…", "Ctrl+Shift+M") {
        open_market = true;
        ui.close();
    }

    if let Some((id, label)) = fire {
        ui.close();
        match builtin_action(&id) {
            Some(cmd) => {
                if let Err(e) =
                    state.run_ui_command(ctx, cmd, &serde_json::json!({"confirm": true}))
                {
                    state.error = Some(crate::i18n::tr_fmt("{} 没能执行：{}", &[&label, &e]));
                }
            }
            None => {
                // 「已经在跑」按状态判断，不按错误文案判断：那句话会随语言变，
                // 用 `contains` 认它，切到英文就认不出来了——本该只是一句提示，
                // 会变成弹个错误横幅。
                if state.plugin_running() {
                    state.status = crate::i18n::tr("已有插件在运行").to_owned();
                } else {
                    match state.plugin_start(ctx, &id) {
                        // 起任务就回：进度与结果由后台任务自己走状态栏那条线。
                        Ok(name) => {
                            state.status = crate::i18n::tr_fmt("{} 运行中…", &[&name]);
                        }
                        Err(e) => state.error = Some(format!("{label}：{e}")),
                    }
                }
            }
        }
    }
    if open_market {
        state.market_open = true;
    }
}

/// 插件市场。左栏类别、右栏清单，每行一个插件。
///
/// 刻意不做卡片网格：一个插件要回答的问题只有四个 —— 叫什么、谁做的、
/// 哪一版、现在开没开。卡片为这四行信息花掉两百多像素，一屏只看得到四个。
pub(super) fn market_window(state: &mut AppState, ctx: &Context) {
    if !state.market_open {
        return;
    }
    use aevum_core::plugin::Kind;
    let mut open = state.market_open;
    let mut toggle: Option<(String, bool)> = None;
    let mut rescan = false;

    // 位置走 `settings.json`：`restored_pos` 取上次拖到的地方（没拖过则居中），
    // 下面 `remember_pos` 在松手后写回。锚点（`.anchor`）会让窗口 immovable
    // ——「子窗口拖不动」就是它，所以这里只用 pos，不用锚。
    //
    // Position comes from `settings.json`: `restored_pos` returns where it was last
    // dragged (centre on first open), `remember_pos` below writes it back on release.
    // Anchoring makes a window immovable in egui -- that was the "sub-window cannot be
    // dragged" bug -- so only pos is used, never an anchor.
    let pos = restored_pos(
        state,
        ctx,
        WIN_MARKET,
        ctx.input(|i| i.viewport_rect()).center() - Vec2::new(380.0, 260.0),
        Vec2::new(760.0, 520.0),
    );
    let win = egui::Window::new(crate::i18n::t!("插件"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(Vec2::new(760.0, 520.0))
        .min_size(Vec2::new(620.0, 380.0))
        .default_pos(pos)
        // 标题栏接管成界面自己的深色：egui 默认标题栏的底与描边跟主界面
        // 不是一套口径（实测亮蓝，用户差评），这里统一成 panel 底 + 底缘
        // 一条 divider，标题居中、关闭钮走默认绘制。
        .title_frame(title_frame())
        .show(ctx, |ui| {
            // 标题栏下面压一条软分割线，把标题区和清单分开。
            let head = Rect::from_min_size(ui.min_rect().min, Vec2::new(ui.available_width(), 1.0));
            theme::soft_rule(ui.painter(), head.left(), head.right(), head.top());
            // 定住下限：`default_size` 只是建议，内容一少窗口就缩成一团，
            // 列表只剩三行可见 —— 那时候滚动条比窗口还长。窗口的 min_size
            // 已经兜底（620×380），不再用 set_min_size 硬顶内容高度——那会
            // 把缩放手柄顶死，窗口拉不大也收不小。
            let body_h = ui.available_height() - 4.0;
            ui.horizontal_top(|ui| {
                // 左栏：类别。用 selectable_label 而不是自制列表，
                // 键鼠焦点与选中态就是现成的。
                ui.allocate_ui_with_layout(
                    Vec2::new(132.0, body_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        let kinds: [(Option<Kind>, &str); 4] = [
                            (None, "全部"),
                            (Some(Kind::Filter), "修图滤镜"),
                            (Some(Kind::Decoder), "解码器"),
                            (Some(Kind::Export), "导出"),
                        ];
                        for (k, label) in kinds {
                            let mut n = state
                                .registry
                                .all()
                                .iter()
                                .filter(|p| k.is_none_or(|k| p.manifest.plugin.kind == k))
                                .count();
                            // 「全部」前面那个数要算上清单坏掉的，否则用户会觉得少了东西。
                            if k.is_none() {
                                n += state.registry.errors().len();
                            }
                            let text = format!("{label}  {n}");
                            if ui
                                .add(
                                    egui::Button::new(RichText::new(text).size(SIZE_BODY))
                                        .selected(state.market_filter == k),
                                )
                                .clicked()
                            {
                                state.market_filter = k;
                            }
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(crate::i18n::t!("来自 %APPDATA%\naevum-studio\\plugins"))
                                .size(SIZE_SMALL)
                                .color(dim()),
                        );
                        if ui
                            .add(egui::Button::new(
                                RichText::new(crate::i18n::t!("重新扫描")).size(SIZE_SMALL),
                            ))
                            .clicked()
                        {
                            rescan = true;
                        }
                    },
                );
                soft_sep(ui);

                // 右栏：搜索 + 列表
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), body_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut state.market_query)
                                    .hint_text(crate::i18n::t!("搜索插件、厂商"))
                                    .desired_width(240.0),
                            );
                            if !state.market_query.is_empty()
                                && ui
                                    .add(egui::Button::new(
                                        RichText::new(crate::i18n::t!("清除")).size(SIZE_SMALL),
                                    ))
                                    .clicked()
                            {
                                state.market_query.clear();
                            }
                        });
                        ui.add_space(4.0);

                        let q = state.market_query.to_lowercase();
                        let hits: Vec<usize> = state
                            .registry
                            .all()
                            .iter()
                            .enumerate()
                            .filter(|(_, p)| {
                                state
                                    .market_filter
                                    .is_none_or(|k| p.manifest.plugin.kind == k)
                                    && (q.is_empty()
                                        || p.manifest.plugin.name.to_lowercase().contains(&q)
                                        || p.manifest.plugin.vendor.to_lowercase().contains(&q)
                                        || p.manifest
                                            .plugin
                                            .description
                                            .to_lowercase()
                                            .contains(&q))
                            })
                            .map(|(i, _)| i)
                            .collect();

                        ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if hits.is_empty() {
                                    ui.add_space(12.0);
                                    ui.label(
                                        RichText::new(crate::i18n::t!("没有匹配的插件"))
                                            .size(SIZE_BODY)
                                            .color(dim()),
                                    );
                                }
                                for i in hits {
                                    let p = &state.registry.all()[i];
                                    let meta = &p.manifest.plugin;
                                    let enabled = p.enabled;
                                    // 行：名字 + 版本 + 厂商，右侧开关。
                                    ui.horizontal(|ui| {
                                        ui.set_min_height(26.0);
                                        ui.label(
                                            RichText::new(&meta.name).size(SIZE_BODY).color(text()),
                                        );
                                        ui.label(
                                            RichText::new(&meta.version)
                                                .size(SIZE_SMALL)
                                                .color(dim()),
                                        );
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                let mut on = enabled;
                                                if ui.checkbox(&mut on, "").changed() {
                                                    toggle = Some((meta.id.clone(), on));
                                                }
                                                ui.label(
                                                    RichText::new(meta.source.cn())
                                                        .size(SIZE_SMALL)
                                                        .color(dim()),
                                                );
                                                ui.label(
                                                    RichText::new(meta.kind.cn())
                                                        .size(SIZE_SMALL)
                                                        .color(dim()),
                                                );
                                            },
                                        );
                                    });
                                    if !meta.description.is_empty() {
                                        ui.label(
                                            RichText::new(&meta.description)
                                                .size(SIZE_SMALL)
                                                .color(dim()),
                                        );
                                    }
                                    ui.add_space(3.0);
                                    soft_sep(ui);
                                    ui.add_space(3.0);
                                }

                                // 坏清单单独列在最后：不藏，也不打断上面的列表。
                                if !state.registry.errors().is_empty() {
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new(crate::i18n::t!("清单有问题"))
                                            .size(SIZE_SMALL)
                                            .color(danger()),
                                    );
                                    for (path, why) in state.registry.errors() {
                                        ui.label(
                                            RichText::new(format!(
                                                "{}：{why}",
                                                path.parent()
                                                    .map(|d| d.display().to_string())
                                                    .unwrap_or_default()
                                            ))
                                            .size(SIZE_SMALL)
                                            .color(dim()),
                                        );
                                    }
                                }
                            });
                    },
                );
            });
        });
    if let Some(w) = win {
        remember_pos(state, ctx, WIN_MARKET, w.response.rect);
    }
    state.market_open = open;
    if let Some((id, on)) = toggle {
        state.registry.set_enabled(&id, on);
        state.status = format!("{}：{}", id, if on { "已启用" } else { "已停用" });
    }
    if rescan {
        let root = crate::settings::plugin_dir();
        state.registry = aevum_core::plugin::Registry::default()
            .with_builtin(aevum_core::plugin::builtin(), &root)
            .merge(aevum_core::plugin::Registry::scan(
                &root,
                aevum_core::plugin::Source::User,
            ));
        state.status = crate::i18n::tr_fmt("已重新扫描 {}", &[&root.display()]);
    }
}

// ---------------------------------------------------------------- 外观与布局

/// 小标题：10px 等宽、压暗一档，与面板里的分组标题同一套语气。
fn look_section(ui: &mut Ui, title: &str) {
    let title = crate::i18n::tr(title);
    ui.add_space(6.0);
    // 分组之间画一道看得见的分隔：此前只有一行小灰字、没有线，用户反馈「分割不明显」。
    // soft_sep 走 theme::soft_rule（divider 已提亮一档），左右各缩进 12px。
    //
    // Draw a visible separator between groups: there used to be only a small grey label and
    // no line, which the user reported as "the division is not obvious". soft_sep routes
    // through theme::soft_rule (divider lifted one step) with a 12 px inset on both sides.
    soft_sep(ui);
    ui.add_space(4.0);
    // 标题用 SIZE_LABEL + text()：分组名是这一屏的结构信息，不该是最淡的一档。
    //
    // Titles use SIZE_LABEL + text(): a group name is structural information on this screen
    // and should not be the faintest step on it.
    ui.label(RichText::new(title).size(SIZE_LABEL).color(text()));
}

/// 一行「标签 + 若干互斥选项」。
///
/// 七个控件用手写 `selectable_label` 会把这一屏写成 200 行；统一成这个形状之后，
/// 加一个布局轴只是加一行数据。
fn look_row<T: PartialEq + Copy>(
    ui: &mut Ui,
    label: &str,
    current: T,
    items: &[(T, &str)],
    mut set: impl FnMut(T),
) {
    let label = crate::i18n::tr(label);
    ui.horizontal(|ui| {
        ui.add_sized(
            [52.0, 20.0],
            egui::Label::new(RichText::new(label).size(SIZE_LABEL).color(dim())),
        );
        for (value, text) in items {
            // selectable_label 的选中态是整块实心强调色，一屏七个控件会铺出一片蓝。
            // 改用 bar_chip：选中走「描边 + 低饱和底」，强调色只描一圈边。
            //
            // selectable_label fills the whole chip with the accent, so seven controls paint
            // a slab of blue. bar_chip keeps the accent to a stroke plus a low-saturation
            // fill.
            if bar_chip(ui, text, current == *value).clicked() {
                set(*value);
            }
        }
    });
}

/// 外观与布局：把 L1 参数直接摊在用户面前。
///
/// 面板自己不存样式，所有控件写回 `state.appearance` / `state.layout`，
/// 帧末比一次快照，变了才重算色板并落盘 —— 全程序只有这一条写路径。
pub(super) fn look_window(state: &mut AppState, ctx: &Context) {
    if !state.look_open {
        return;
    }
    let before = (state.appearance, state.layout);
    let before_lang = state.lang;
    let mut open = state.look_open;
    // 首开贴右上角（菜单行下 52px），之后按上次拖到的位置。
    //
    // First open hugs the top-right corner (52px below the menu row); afterwards it
    // returns to wherever it was dragged.
    let pos = restored_pos(
        state,
        ctx,
        WIN_LOOK,
        ctx.input(|i| i.viewport_rect()).right_top() - Vec2::new(354.0, -52.0),
        Vec2::new(340.0, 560.0),
    );
    let win = egui::Window::new(crate::i18n::t!("外观与布局"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .default_pos(pos)
        // 与插件市场同一个标题栏口径：深色底、无高亮描边。
        .title_frame(title_frame())
        .show(ctx, |ui| {
            // 定宽而不是让内容撑：锚在右上时，egui 会按内容回缩，回缩到比一行控件
            // 还窄就会把左边的选项挤到窗口外面去。
            ui.set_min_width(316.0);
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 5.0);

            look_section(ui, "语言 / Language");
            ui.horizontal_wrapped(|ui| {
                // 选项来自注册表，不逐个列语言：「加一种语言只改 CATALOGS 一行」这条
                // 承诺在这里兑现——下面是它唯一的使用者，写法对语言数量一无所知。
                //
                // Options come from the registry rather than an explicit list, so the
                // promise "adding a language means one line in CATALOGS" holds here:
                // this is its only consumer, and it knows nothing about the count.
                for p in crate::i18n::LangPref::all() {
                    // 「跟随系统」是唯一需要翻译的一项；其余用各自语言自己的写法，
                    // 不翻 —— 只会英文的人要能认出那一项。
                    //
                    // 语言自称必须绕开 tr()：那是「这个语言怎么称呼自己」，不是界面文案。
                    // 走 tr() 会被 i18n 覆盖率判据记成缺译（英文界面上「中文」本就该是汉字）。
                    //
                    // "跟随系统" is the only translated entry; the rest use each language's own
                    // endonym so an English-only reader can still find it.
                    //
                    // Endonyms must bypass tr(): they name the language itself, not UI copy.
                    // Routing them through tr() is recorded as a missing translation, because
                    // "中文" is meant to stay in Han characters even in English mode.
                    let system_label = crate::i18n::t!("跟随系统");
                    let (name, translate): (&str, bool) = match p {
                        crate::i18n::LangPref::System => (system_label, true),
                        crate::i18n::LangPref::Fixed(l) => (l.label(), false),
                    };
                    let hit = if translate {
                        bar_chip(ui, name, state.lang == p).clicked()
                    } else {
                        bar_chip_verbatim(ui, name, state.lang == p).clicked()
                    };
                    if hit && state.lang != p {
                        state.lang = p;
                        // 立即生效：之后的绘制就走新语言。落盘交给帧末那条统一路径
                        // （见函数末尾），这里不自己写文件。
                        crate::i18n::set(p.resolve());
                    }
                }
            });

            look_section(ui, "明暗");
            ui.horizontal_wrapped(|ui| {
                for m in crate::settings::THEME_ORDER {
                    if ui
                        .selectable_label(state.appearance.theme == m, m.cn())
                        .clicked()
                    {
                        state.appearance.theme = m;
                    }
                }
            });

            look_section(ui, "皮肤 · 中性色倾向");
            ui.horizontal_wrapped(|ui| {
                for skin in Skin::ALL {
                    if ui
                        .selectable_label(state.appearance.skin == skin, skin.cn())
                        .clicked()
                    {
                        state.appearance.skin = skin;
                    }
                }
            });

            look_section(ui, "主色");
            ui.horizontal_wrapped(|ui| {
                for (h, s, l, name) in theme::ACCENT_PRESETS {
                    let col = theme::hsl(h, s, l);
                    let active = (state.appearance.accent_h - h).abs() < 1.0
                        && (state.appearance.accent_s - s).abs() < 2.0
                        && (state.appearance.accent_l - l).abs() < 2.0;
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                    let c = rect.center();
                    ui.painter().circle_filled(c, 8.0, col);
                    if active {
                        ui.painter()
                            .circle_stroke(c, 10.0, Stroke::new(1.5, text()));
                    }
                    if resp.clicked() {
                        state.appearance.accent_h = h;
                        state.appearance.accent_s = s;
                        state.appearance.accent_l = l;
                    }
                    resp.on_hover_text(name);
                }
                // 任意取色：给的是 RGB，换算成 HSL 再存，深浅档仍能由参数派生。
                let mut rgb = theme::accent_rgb();
                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    let (h, s, l) = theme::rgb_to_hsl(rgb[0], rgb[1], rgb[2]);
                    state.appearance.accent_h = h;
                    state.appearance.accent_s = s;
                    state.appearance.accent_l = l;
                }
            });

            look_section(ui, "外壳");
            look_row(
                ui,
                "平台",
                state.appearance.os,
                &[
                    (platform::OsChoice::Auto, "跟随系统"),
                    (platform::OsChoice::Win, "Windows"),
                    (platform::OsChoice::Mac, "macOS"),
                ],
                |v| state.appearance.os = v,
            );

            look_section(ui, "形状与字号");
            ui.horizontal(|ui| {
                ui.add_sized(
                    [52.0, 20.0],
                    egui::Label::new(
                        RichText::new(crate::i18n::t!("圆角"))
                            .size(SIZE_LABEL)
                            .color(dim()),
                    ),
                );
                ui.add(
                    egui::Slider::new(&mut state.appearance.radius, 0.0..=16.0)
                        .max_decimals(0)
                        .suffix(" px"),
                );
            });
            // 密度：只动调整面板的行高，外壳骨架高度不跟——那是界面稿钉死的。
            let mut density = state.appearance.density;
            look_row(
                ui,
                "密度",
                density,
                &[
                    (theme::Density::Compact, "紧凑"),
                    (theme::Density::Standard, "标准"),
                    (theme::Density::Loose, "宽松"),
                ],
                |v| density = v,
            );
            state.appearance.density = density;
            let mut level = state.font_level;
            look_row(
                ui,
                "字号",
                level,
                &[(0, "小"), (1, "标准"), (2, "大"), (3, "特大")],
                |v| level = v,
            );
            if level != state.font_level {
                state.font_level = level;
                theme::set_fonts(ctx); // 字号档位变了才重装字体
            }

            look_section(ui, "布局");
            let preset = state.layout.preset();
            look_row(
                ui,
                "预设",
                preset,
                &[
                    (Some(crate::layout::LayoutPreset::Classic), "经典"),
                    (Some(crate::layout::LayoutPreset::Focus), "专注"),
                    (Some(crate::layout::LayoutPreset::Min), "极简"),
                ],
                |p| {
                    if let Some(p) = p {
                        state.layout = state.layout.with_preset(p);
                    }
                },
            );
            look_row(
                ui,
                "状态栏",
                state.layout.status,
                &[
                    (StatusSlot::Bottom, "底部"),
                    (StatusSlot::Top, "顶部"),
                    (StatusSlot::Hidden, "隐藏"),
                ],
                |v| state.layout.status = v,
            );
            look_row(
                ui,
                "工具轨",
                state.layout.rail,
                &[
                    (crate::layout::Side::Left, "左"),
                    (crate::layout::Side::Right, "右"),
                ],
                |v| state.layout.rail = v,
            );
            look_row(
                ui,
                "调整面板",
                state.layout.dock_side,
                &[
                    (crate::layout::Side::Right, "右"),
                    (crate::layout::Side::Left, "左"),
                ],
                |v| state.layout.dock_side = v,
            );
            look_row(
                ui,
                "胶片条",
                state.layout.film,
                &[
                    (crate::layout::FilmSlot::Shown, "显示"),
                    (crate::layout::FilmSlot::Hidden, "隐藏"),
                ],
                |v| state.layout.film = v,
            );

            ui.add_space(8.0);
            soft_sep(ui);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new(crate::i18n::t!("重置")).size(SIZE_BODY))
                    .clicked()
                {
                    state.appearance = Appearance::default();
                    state.layout = crate::layout::Layout::default();
                }
                if ui
                    .button(RichText::new(crate::i18n::t!("完成")).size(SIZE_BODY))
                    .clicked()
                {
                    state.look_open = false;
                }
                ui.label(
                    RichText::new(crate::i18n::t!("改动即时生效并记住"))
                        .size(SIZE_SMALL)
                        .color(dim()),
                );
            });
        });
    if let Some(w) = win {
        remember_pos(state, ctx, WIN_LOOK, w.response.rect);
    }
    state.look_open = open;

    if (state.appearance, state.layout) != before {
        theme::set_appearance(state.appearance);
        theme::apply_style(ctx, FONT_ZOOMS[state.font_level.min(FONT_ZOOMS.len() - 1)]);
        crate::settings::save(&state.settings());
    } else if state.lang != before_lang {
        // 只换了语言：色板与样式不用重算，但设置文件必须落盘 —— 否则下次启动
        // 又回到旧语言，用户会以为切换没生效。仍然只有这一条写路径。
        crate::settings::save(&state.settings());
    }
}

/// 「预设与快照」浮窗：内置预设 / 快照 / 调色副本三节，节次可拖。
///
/// 2026-11-04 起它取代常驻左栏（docs/界面布局-ACR化-2026-11-04.md §2）：这三样
/// 都是次要功能，不该占一整栏画布宽度；ACR 同样把预设/快照做成工具条上随叫随到
/// 的面板而不是编辑面板的一部分。历史一节删除——dock「历史」页有完整撤销/重做。
/// 开关是 `state.show_left`（settings.json 键沿用旧左栏），入口：工具轨顶部图标、
/// 「显影」菜单、Ctrl+K。
///
/// The Presets & Snapshots window: built-in presets / snapshots / grade copies, with
/// draggable sections. Since 2026-11-04 it replaces the resident left column (plan §2):
/// all three are secondary tools that should not cost a full column of canvas; ACR
/// likewise keeps presets/snapshots as on-demand panels on the tool strip rather than
/// part of the edit panel. The history section is gone -- the dock's History tab has
/// the full undo/redo. Toggled by `state.show_left` (settings key carried over from the
/// old column); entrances: the tool-rail icon, the Develop menu, Ctrl+K.
pub(super) fn presets_window(state: &mut AppState, ctx: &Context) {
    if !state.show_left {
        return;
    }
    let mut open = true;
    // 首开贴左上（旧左栏的位置），之后按上次拖到的位置。
    //
    // First open near the top-left (where the old column sat), then back to where it
    // was dragged.
    let pos = restored_pos(
        state,
        ctx,
        WIN_PRESETS,
        Pos2::new(60.0, 96.0),
        Vec2::new(248.0, 480.0),
    );
    let win = egui::Window::new(crate::i18n::t!("预设与快照"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(248.0)
        .default_height(480.0)
        .default_pos(pos)
        .title_frame(title_frame())
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if bar_button(ui, "＋ 存快照")
                    .on_hover_text(crate::i18n::t!("存快照：把当前整套参数钉一版"))
                    .clicked()
                {
                    state.save_snapshot();
                }
            });
            ui.add_space(2.0);

            let mut want_preset: Option<&'static str> = None;
            let mut want_snap: Option<usize> = None;
            let mut drop_snap: Option<usize> = None;
            let snapshot_n = state.snapshots.len();
            let copies_n = state.copies.len();
            // 每一节的节头矩形：拖拽落地时用它判「指针落在哪一节」。
            //
            // Header rect per section, used to decide which section the pointer is over
            // when a dragged section is dropped.
            let mut bands: Vec<(&'static str, egui::Rect)> = Vec::new();

            egui::ScrollArea::vertical()
                .id_salt("presets-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    // 节次按 `state.preset_order` 排（可拖）。**不在这里写死次序**：
                    // 那份次序是用户摆的，落盘在 settings.json，读回时已经规整成一个
                    // 完整排列（见 `state::normalise_preset_order`）。
                    //
                    // Sections follow `state.preset_order` (draggable). The order is *not*
                    // hard-coded here: it belongs to the user, persisted in settings.json,
                    // and normalised into a full permutation on load.
                    for (n, key) in state.preset_order.clone().into_iter().enumerate() {
                        if n > 0 {
                            side_divider(ui);
                        }
                        let (title, count) = match key {
                            "builtin" => ("内置", aevum_core::grade::PRESET_NAMES.len()),
                            "snapshots" => ("快照", snapshot_n),
                            _ => ("调色副本", copies_n),
                        };
                        let dragging = state.drag_section == Some(key);
                        let head = side_head(ui, title, count, dragging);
                        if head.drag_started() {
                            state.drag_section = Some(key);
                        }
                        bands.push((key, head.rect));

                        match key {
                            "builtin" => {
                                for name in aevum_core::grade::PRESET_NAMES {
                                    if side_row(
                                        ui,
                                        name,
                                        Some(Glyph::Sliders),
                                        state.preset == *name,
                                    )
                                    .clicked()
                                    {
                                        want_preset = Some(name);
                                    }
                                }
                            }
                            "snapshots" => {
                                if snapshot_n == 0 {
                                    side_note(ui, "上方 ＋ 存一版");
                                }
                                for (i, (label, _)) in state.snapshots.iter().enumerate() {
                                    let row = side_row(ui, label, Some(Glyph::Star), false);
                                    let x_rect = egui::Rect::from_center_size(
                                        egui::Pos2::new(
                                            row.rect.right() - 14.0,
                                            row.rect.center().y,
                                        ),
                                        egui::Vec2::splat(18.0),
                                    );
                                    let xr = ui.interact(
                                        x_rect,
                                        ui.make_persistent_id(("snap-del", i)),
                                        Sense::click(),
                                    );
                                    let show = row.hovered() || xr.hovered();
                                    if show {
                                        icons::paint(
                                            ui.painter(),
                                            x_rect.shrink(5.0),
                                            Glyph::Close,
                                            if xr.hovered() { danger() } else { muted() },
                                        );
                                    }
                                    let xr_hint =
                                        xr.clone().on_hover_text(crate::i18n::t!("删除快照"));
                                    let _ = &xr_hint;
                                    if row.clicked() {
                                        want_snap = Some(i);
                                    } else if xr.clicked() {
                                        drop_snap = Some(i);
                                    }
                                }
                            }
                            _ => {
                                if copies_n == 0 {
                                    side_note(ui, "下方存一套当前调色");
                                }
                                copies_group(state, ui);
                            }
                        }
                    }
                });

            // 拖拽落地：指针落在哪一节的范围里，就把正在拖的那一节挪到那个位置
            // （范围取「本节节头顶 → 下一节节头顶」，最后一节到无穷远）。
            //
            // Drop handling: whichever section's range holds the pointer becomes the
            // target index (this header's top to the next header's top; the last
            // section is unbounded below).
            if let Some(key) = state.drag_section {
                if !ui.ctx().input(|i| i.pointer.any_down()) {
                    state.drag_section = None;
                } else if let Some(pos) = ui.ctx().pointer_latest_pos()
                    && let Some(target) = bands.iter().enumerate().position(|(i, (k, r))| {
                        if *k == key {
                            return false;
                        }
                        let top = r.top();
                        let bottom = bands
                            .get(i + 1)
                            .map_or(f32::INFINITY, |(_, next)| next.top());
                        pos.y >= top && pos.y < bottom
                    })
                {
                    state.move_preset_section(key, target);
                }
            }

            if let Some(name) = want_preset {
                state.apply_preset(name);
            }
            if let Some(i) = want_snap {
                state.restore_snapshot(i);
            }
            if let Some(i) = drop_snap {
                state.delete_snapshot(i);
            }
        });
    if !open {
        state.show_left = false;
        state.settings_dirty = true;
    }
    if let Some(w) = win {
        remember_pos(state, ctx, WIN_PRESETS, w.response.rect);
    }
}

pub(super) fn about_window(state: &mut AppState, ctx: &Context) {
    if !state.about_open {
        return;
    }
    let mut open = true;
    // 首开居中，之后按上次拖到的位置。
    //
    // Centred on first open, then back to where it was dragged.
    let pos = restored_pos(
        state,
        ctx,
        WIN_ABOUT,
        ctx.input(|i| i.viewport_rect()).center() - Vec2::new(215.0, 160.0),
        Vec2::new(430.0, 480.0),
    );
    let win = egui::Window::new(crate::i18n::t!("快捷键与关于"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(430.0)
        .default_pos(pos)
        .title_frame(title_frame())
        .show(ctx, |ui| {
            ui.label(
                RichText::new(aevum_core::brand::TITLE)
                    .size(SIZE_HEADING)
                    .color(text())
                    .strong(),
            );
            ui.label(
                RichText::new(crate::i18n::t!("Rust + egui · 核心数值以 Python 金标为准"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
            ui.add_space(8.0);
            egui::Grid::new("shortcut-grid")
                .num_columns(2)
                .spacing([20.0, 3.0])
                .show(ui, |ui| {
                    for (key, desc) in [
                        ("Ctrl+O", crate::i18n::t!("打开序列")),
                        ("Ctrl+Shift+O", crate::i18n::t!("选照片（可多张）")),
                        ("Ctrl+E", crate::i18n::t!("导出当前帧")),
                        ("Ctrl+Z", crate::i18n::t!("撤销")),
                        ("Ctrl+Shift+Z / Ctrl+Y", crate::i18n::t!("重做")),
                        ("Ctrl+0 / Ctrl+1", crate::i18n::t!("适应窗口 / 实际像素")),
                        ("Ctrl+Alt+1…7", crate::i18n::t!("切换模块")),
                        ("Ctrl+K", crate::i18n::t!("预设与快照")),
                        ("H / Z / C", crate::i18n::t!("抓手 / 缩放 / 裁剪")),
                        ("G / \\", crate::i18n::t!("网格 / 前后对比")),
                        ("空格", crate::i18n::t!("播放 / 暂停")),
                        ("← →", crate::i18n::t!("逐帧（Shift 一次十帧）")),
                        ("1–5 / 0", crate::i18n::t!("星级 / 清除星级")),
                        ("6–9", crate::i18n::t!("红黄绿蓝色标")),
                        ("P / X", crate::i18n::t!("选用 / 排除")),
                        ("F", crate::i18n::t!("适应窗口")),
                    ] {
                        ui.label(
                            RichText::new(key)
                                .size(SIZE_SMALL)
                                .monospace()
                                .color(accent()),
                        );
                        ui.label(RichText::new(desc).size(SIZE_SMALL).color(muted()));
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            soft_sep(ui);
            ui.horizontal(|ui| {
                ui.label(theme::caps("GOLDEN 15/15 · 1e-4"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(RichText::new(crate::i18n::t!("知道了")).size(SIZE_SMALL))
                        .clicked()
                    {
                        state.about_open = false;
                    }
                });
            });
        });
    if let Some(w) = win {
        remember_pos(state, ctx, WIN_ABOUT, w.response.rect);
    }
    if !open {
        state.about_open = false;
    }
}

/// 「检查更新」窗口：当前/远端版本、一行状态、发布说明、以及随阶段变化的那颗按钮。
///
/// 四段结构对应四种「用户此刻该知道什么」：
/// 1. **两枚版本号** —— 我在哪、对面在哪，比一句「有新版本」有用；
/// 2. **一行状态** —— 直接用 `UpdateCheck::note`（core 已经把它写成一句完整的话，
///    成功失败都是）；界面不再自己拼，否则四处措辞会漂；
/// 3. **发布说明** —— 可滚动、最多占窗口一半高度：说明可以很长，但不能把按钮挤出屏幕；
/// 4. **动作区** —— 空闲时「下载并安装」或「检查更新」，下载中进度条 + 禁用按钮，
///    失败红字一行，备好后「立即重启并安装」。
///
/// 所有耗时动作都只是**置一个意图**，真正的后台任务在窗口关掉之后再起——窗口里调
/// `&mut self` 的方法会和 `show` 闭包对 `state` 的借用打架，而且先收集意图再执行的
/// 写法与「插件市场」那扇窗完全一致。
///
/// The "check for updates" window: current and remote versions, one status line, the
/// release notes, and the single button that changes with the phase.
///
/// The four sections answer four "what does the user need to know now" questions:
/// 1. **two version numbers** -- where I am and where the other side is beats "an update
///    exists";
/// 2. **one status line** -- `UpdateCheck::note` verbatim (core already writes it as a
///    complete sentence, success or failure); the window does not re-derive it, or the
///    wording drifts between the four outcomes;
/// 3. **release notes** -- scrollable, at most half the window height: notes can be long,
///    but they must not push the buttons off screen;
/// 4. **the action area** -- idle gives "download and install" or "check", downloading shows
///    a bar with a disabled button, a failure shows one red line, and a prepared install
///    shows "restart and install".
///
/// Every slow action here only **records an intent**; the real background job starts after
/// the window closes -- calling an `&mut self` method inside `show` would fight the closure's
/// borrow of `state`, and collect-then-act is exactly how the plugin market window works.
pub(super) fn update_window(state: &mut AppState, ctx: &Context) {
    if !state.update_open {
        return;
    }
    let mut open = state.update_open;
    let mut want_check = false;
    let mut want_download = false;
    let mut want_launch = false;

    let pos = restored_pos(
        state,
        ctx,
        WIN_UPDATE,
        ctx.input(|i| i.viewport_rect()).center() - Vec2::new(240.0, 210.0),
        Vec2::new(480.0, 420.0),
    );
    let win = egui::Window::new(crate::i18n::t!("检查更新"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(Vec2::new(480.0, 420.0))
        .min_size(Vec2::new(420.0, 300.0))
        .default_pos(pos)
        .title_frame(title_frame())
        .show(ctx, |ui| {
            // 标题区下面压一条软线，与「关于 / 插件」两扇窗同一套结构。
            //
            // A soft rule under the title area, the same structure as the about and
            // plugin windows.
            let head = Rect::from_min_size(ui.min_rect().min, Vec2::new(ui.available_width(), 1.0));
            theme::soft_rule(ui.painter(), head.left(), head.right(), head.top());
            ui.add_space(4.0);

            // 1 两枚版本号。
            let latest = state.update_check.as_ref().and_then(|c| c.latest.clone());
            let remote = latest.unwrap_or_else(|| crate::i18n::t!("未知").to_owned());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(crate::i18n::tr_fmt(
                        "当前版本 {}",
                        &[&aevum_core::update::current_version()],
                    ))
                    .size(SIZE_SMALL)
                    .color(muted()),
                );
                ui.add_space(14.0);
                ui.label(
                    RichText::new(crate::i18n::tr_fmt("远端版本 {}", &[&remote]))
                        .size(SIZE_SMALL)
                        .color(muted()),
                );
            });

            // 2 一行状态。core 的 `note` 恒非空，直接用；这个进程还没查过时给一句话，
            // 免得窗口上是一片空白。
            let note = match state.update_check.as_ref() {
                Some(c) => c.note.clone(),
                None => crate::i18n::t!("还没有检查过").to_owned(),
            };
            ui.add_space(5.0);
            ui.label(RichText::new(note).size(SIZE_BODY).color(text()));

            // 3 发布说明：可滚动，最多占窗口一半高度。
            if !state.update_body.is_empty() {
                ui.add_space(7.0);
                ui.label(
                    RichText::new(crate::i18n::t!("发布说明"))
                        .size(SIZE_SMALL)
                        .color(dim()),
                );
                let cap = (ctx.input(|i| i.viewport_rect()).height() * 0.5).max(80.0);
                egui::ScrollArea::vertical()
                    .max_height(cap)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        // Release 正文是 GitHub 上的 Markdown：`## 下载`、`###`、反引号、
                        // 加粗原本被逐字塞进一个 label，用户看到的是源码。这里做轻量渲染，
                        // 只认标题 / 列表 / 加粗 / 行内代码 / 链接；超出闭集的行原样保留。
                        //
                        // The release body is GitHub Markdown: `## Download`, `###`, backticks
                        // and bold used to show up verbatim in a single label. Render it
                        // lightly here -- headings, lists, bold, inline code, links -- and keep
                        // anything outside that closed set verbatim.
                        for line in crate::shell::markdown::parse_release_notes(&state.update_body)
                        {
                            render_note_line(ui, &line);
                        }
                    });
            }

            // 4 动作区。失败那行红字放在按钮之前：先读到「为什么没成」，再看到那颗
            // 可以重试的按钮，顺序才顺。
            //
            // The action area. The red failure line goes above the button: read "why it
            // failed" first, then see the button that can retry.
            ui.add_space(9.0);
            soft_sep(ui);
            ui.add_space(6.0);
            if let Some(e) = &state.update_error {
                ui.label(RichText::new(e.as_str()).size(SIZE_SMALL).color(danger()));
                ui.add_space(5.0);
            }

            match state.update_phase {
                UpdatePhase::Checking => {
                    ui.label(
                        RichText::new(crate::i18n::t!("正在检查…"))
                            .size(SIZE_SMALL)
                            .color(dim()),
                    );
                    ui.add_space(4.0);
                    // 禁用而不是隐藏：按钮留在原位，用户看得见「这个动作正在跑」。
                    //
                    // Disabled rather than hidden: the button stays put so the user can see
                    // that this action is the one running.
                    ui.add_enabled(
                        false,
                        egui::Button::new(crate::i18n::t!("检查更新"))
                            .min_size(Vec2::new(0.0, 26.0)),
                    );
                }
                UpdatePhase::Downloading => {
                    let (done, total) = state.update_progress_snapshot();
                    let frac = if total > 0 {
                        (done as f32 / total as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    // 1024 进制、一位小数：与资源管理器显示的「MB」同口径，用户对得上。
                    //
                    // Base 1024 with one decimal: same reading as Explorer's "MB", so the
                    // numbers line up for the user.
                    let mb = |v: u64| format!("{:.1}", v as f64 / (1024.0 * 1024.0));
                    let text = if total > 0 {
                        crate::i18n::tr_fmt("{} / {} MB", &[&mb(done), &mb(total)])
                    } else {
                        crate::i18n::tr_fmt("{} MB", &[&mb(done)])
                    };
                    let mut bar = egui::ProgressBar::new(frac)
                        .desired_height(6.0)
                        .text(RichText::new(text).size(SIZE_SMALL));
                    // 资产没声明大小时（`total == 0`）用不确定进度条：一根停在 0% 的
                    // 实心条会被读成「卡住了」。
                    //
                    // When the asset declares no size (`total == 0`) the bar goes
                    // indeterminate: a solid bar parked at 0% reads as "stuck".
                    if total == 0 {
                        bar = bar.animate(true);
                    }
                    ui.add(bar);
                    ui.add_space(4.0);
                    ui.add_enabled(
                        false,
                        egui::Button::new(crate::i18n::t!("下载并安装"))
                            .min_size(Vec2::new(0.0, 26.0)),
                    );
                }
                UpdatePhase::Ready => {
                    let version = state
                        .update_ready
                        .as_ref()
                        .map(|p| p.version.clone())
                        .unwrap_or_default();
                    ui.label(
                        RichText::new(crate::i18n::tr_fmt(
                            "已准备好 {}，重启后完成安装",
                            &[&version],
                        ))
                        .size(SIZE_SMALL)
                        .color(ok()),
                    );
                    ui.add_space(6.0);
                    if bar_primary(ui, "立即重启并安装").clicked() {
                        want_launch = true;
                    }
                }
                UpdatePhase::Idle => {
                    let installable = state.update_check.as_ref().is_some_and(|c| c.installable());
                    if installable {
                        if bar_primary(ui, "下载并安装").clicked() {
                            want_download = true;
                        }
                    } else if bar_button(ui, "检查更新").clicked() {
                        want_check = true;
                    }
                }
            }
        });
    if let Some(w) = win {
        remember_pos(state, ctx, WIN_UPDATE, w.response.rect);
    }
    state.update_open = open;
    if want_check {
        state.manual_update_check(ctx);
    }
    if want_download && let Err(e) = state.start_install(ctx) {
        state.update_error = Some(e);
    }
    if want_launch && let Err(e) = state.launch_installer(ctx) {
        state.update_error = Some(e);
    }
}

/// 「回滚到上一版」的二次确认窗。
///
/// 为什么必须有这一步：回滚会**替换正在运行的 exe**，而且用户点它的动机通常是「新版
/// 起不来了」——那一刻他心情最差、最容易误点，所以不给一次「确定吗」是不负责任的。
/// 窗口里把两件后果写清楚：当前版改名成 `.failed`（留着排查，不删）、上一版随即启动。
///
/// 为什么用 `Modal` 而不是 `Window`：确认框要在用户回答之前**挡住**后面的面板，否则
/// 用户可以在回答「要不要换掉当前版本」的同时继续调色，那就等于两个状态同时在动。也正
/// 因为它是模态框而不是工具窗，它不参与浮窗位置记账（没有「摆到角落」这回事）。
///
/// The second confirmation for "roll back to the previous version".
///
/// Why this step is mandatory: a rollback **replaces the running exe**, and the motive for
/// clicking it is usually "the new build will not start" -- the worst moment for a
/// misclick, so not asking once would be irresponsible. The window spells out both
/// consequences: this build is renamed to `.failed` (kept for diagnosis, not deleted) and
/// the previous one starts right away.
///
/// Why `Modal` and not `Window`: a confirmation has to **block** the panels behind it until
/// it is answered, otherwise the user can keep grading while deciding whether to swap the
/// running build -- two states moving at once. That is also why it takes no part in the
/// floating-window position bookkeeping: it is not a tool window anyone parks in a corner.
pub(super) fn rollback_window(state: &mut AppState, ctx: &Context) {
    if !state.rollback_confirm {
        return;
    }
    let mut cancel = false;
    let mut go = false;
    let modal = egui::Modal::new(egui::Id::unique("aevum-rollback-confirm")).show(ctx, |ui| {
        ui.set_width(430.0);
        ui.label(
            RichText::new(crate::i18n::t!("回滚到上一版"))
                .size(SIZE_BODY)
                .color(text()),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(crate::i18n::t!(
                "回滚到上一版？当前这一版会被改名成 .failed 留着排查，上一版随即启动。"
            ))
            .size(SIZE_SMALL)
            .color(text()),
        );
        ui.add_space(9.0);
        ui.horizontal(|ui| {
            if bar_button(ui, "取消").clicked() {
                cancel = true;
            }
            if bar_primary(ui, "回滚并重启").clicked() {
                go = true;
            }
        });
    });
    // Esc 与点背景都算取消：模态框必须留一条不改任何东西的出口，否则它就成了陷阱。
    //
    // Escape and a backdrop click both cancel: a modal must keep an exit that changes
    // nothing, or it becomes a trap.
    if modal.should_close() {
        cancel = true;
    }
    if cancel {
        state.rollback_confirm = false;
        return;
    }
    if go {
        state.rollback_confirm = false;
        if let Err(e) = state.launch_rollback(ctx) {
            // 起不来就把原因挂到状态栏那条既有通道上：确认窗已经关了，这时候再弹一个
            // 只会是第二层惊吓。
            //
            // A failure goes out through the existing status/error channel: the
            // confirmation is already gone, and popping another window now would only be a
            // second scare.
            state.error = Some(e);
        }
    }
}

/// 画一行发布说明：标题、列表、正文三种形态，全部走 theme token。
///
/// Draw one release-note line: heading, list item or body, all through theme tokens.
fn render_note_line(ui: &mut Ui, line: &crate::shell::markdown::NoteLine) {
    use crate::shell::markdown::NoteLine;
    match line {
        NoteLine::Heading(2, text) => {
            ui.add_space(4.0);
            ui.label(RichText::new(text).font(font(SIZE_HEADING)).color(accent()));
        }
        NoteLine::Heading(_, heading) => {
            ui.add_space(2.0);
            ui.label(RichText::new(heading).font(font(SIZE_LABEL)).color(text()));
        }
        NoteLine::Bullet(spans) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("·").size(SIZE_SMALL).color(muted()));
                for span in spans {
                    render_note_span(ui, span);
                }
            });
        }
        NoteLine::Body(spans) => {
            ui.horizontal_wrapped(|ui| {
                for span in spans {
                    render_note_span(ui, span);
                }
            });
        }
        NoteLine::Blank => {
            ui.add_space(4.0);
        }
    }
}

/// 画一个行内片段：普通文字、加粗、行内代码、链接。
///
/// Draw one inline run: plain text, bold, inline code or link.
fn render_note_span(ui: &mut Ui, span: &crate::shell::markdown::Span) {
    use crate::shell::markdown::Span;
    match span {
        Span::Text(s) => {
            ui.label(RichText::new(s).size(SIZE_SMALL).color(muted()));
        }
        Span::Bold(s) => {
            ui.label(RichText::new(s).size(SIZE_SMALL).color(text()).strong());
        }
        Span::Code(s) => {
            ui.label(
                RichText::new(s)
                    .font(mono(SIZE_SMALL))
                    .color(value())
                    .background_color(field_hover()),
            );
        }
        Span::Link { text: label, url } => {
            ui.label(
                RichText::new(label)
                    .size(SIZE_SMALL)
                    .color(accent())
                    .underline(),
            )
            .on_hover_text(url.as_str());
        }
    }
}

/// 一次导出确认所携带的参数。
///
/// 字段全是简单类型（`String` / `u8`），宿主不必认识 `StillFormat` 之类的 core
/// 枚举；集成方按 `format_depth` 的稳定拼写再映射回具体容器。
///
/// The parameters carried by one export confirmation.
///
/// Every field is a simple type (`String` / `u8`), so the host does not need to know
/// core enums such as `StillFormat`; the integrator maps `format_depth`'s stable
/// spelling back to a concrete container.
#[derive(Debug, Clone)]
pub struct ExportChoice {
    /// 导出范围：`all`（整个序列）/ `selected`（选中帧）/ `current`（当前帧）。
    ///
    /// Export scope: `all` (whole sequence) / `selected` (checked frames) / `current`.
    pub scope: String,
    /// 目标目录（绝对路径字符串）。
    ///
    /// Destination directory as an absolute path string.
    pub out_dir: String,
    /// 格式与位深的稳定拼写，取值同 `StillFormat::key`。
    ///
    /// Stable spelling of format and bit depth, same values as `StillFormat::key`.
    pub format_depth: String,
    /// 同名覆盖策略：0 = 询问 / 1 = 覆盖 / 2 = 跳过 / 3 = 改名。
    ///
    /// Same-name policy: 0 = ask / 1 = overwrite / 2 = skip / 3 = rename.
    pub overwrite: u8,
}

/// 从当前导出参数推出表单的初始值（读不到的项用中性默认，不 unwrap）。
///
/// Derive the form's initial values from the current export settings (neutral defaults
/// where a value cannot be read; never unwrap).
fn default_export_choice(state: &AppState) -> ExportChoice {
    ExportChoice {
        scope: "all".to_owned(),
        out_dir: state.render_out_dir.display().to_string(),
        format_depth: state.render_format.key().to_owned(),
        overwrite: if state.render_resume { 2 } else { 0 },
    }
}

/// 导出参数确认框：确认后返回参数，取消返回 `None`（不产生任何文件）。
///
/// 第二个返回值是「已经关闭」：`(None, false)` 表示用户还在表单里改，
/// `(None, true)` 是取消，`(Some(_), true)` 是确认。调用方靠它清掉挂载标记，
/// 否则弹窗关不掉。
///
/// Export-parameter confirmation: returns the parameters on confirm, `None` on cancel
/// (no file is produced).
///
/// The second return value means "closed": `(None, false)` = still editing,
/// `(None, true)` = cancelled, `(Some(_), true)` = confirmed. The caller uses it to clear
/// the mount flag, otherwise the modal never closes.
pub fn confirm_export_modal(
    ui: &mut egui::Ui,
    state: &mut AppState,
) -> (Option<ExportChoice>, bool) {
    let form_id = egui::Id::unique("aevum-export-confirm-form");
    let mut form = match ui.ctx().data_mut(|d| d.get_temp::<ExportChoice>(form_id)) {
        Some(saved) => saved,
        None => default_export_choice(state),
    };

    let mut cancel = false;
    let mut go = false;
    let modal = egui::Modal::new(egui::Id::unique("aevum-export-confirm")).show(ui.ctx(), |ui| {
        ui.set_width(430.0);
        ui.label(
            RichText::new(crate::i18n::t!("导出参数"))
                .font(font(SIZE_BODY))
                .color(text()),
        );
        ui.add_space(8.0);

        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("导出范围"))
                    .font(font(SIZE_LABEL))
                    .color(text()),
            );
            for (value, label) in [
                ("all", "整个序列"),
                ("selected", "选中帧"),
                ("current", "当前帧"),
            ] {
                if bar_chip(ui, label, form.scope == value).clicked() {
                    form.scope = value.to_owned();
                }
            }
        });

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("目标目录"))
                    .font(font(SIZE_LABEL))
                    .color(text()),
            );
            if bar_button(ui, crate::i18n::t!("选择…")).clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                form.out_dir = dir.display().to_string();
            }
        });
        ui.add(
            egui::Label::new(
                RichText::new(&form.out_dir)
                    .font(font(SIZE_SMALL))
                    .color(muted()),
            )
            .truncate(),
        );

        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("格式与位深"))
                    .font(font(SIZE_LABEL))
                    .color(text()),
            );
            for f in aevum_core::export::StillFormat::ALL {
                if bar_chip(ui, f.cn(), form.format_depth == f.key()).clicked() {
                    form.format_depth = f.key().to_owned();
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("同名覆盖"))
                    .font(font(SIZE_LABEL))
                    .color(text()),
            );
            for (value, label) in [(0u8, "询问"), (1, "覆盖"), (2, "跳过"), (3, "改名")] {
                if bar_chip(ui, label, form.overwrite == value).clicked() {
                    form.overwrite = value;
                }
            }
        });

        ui.add_space(10.0);
        soft_sep(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if bar_button(ui, crate::i18n::t!("取消")).clicked() {
                cancel = true;
            }
            if bar_primary(ui, crate::i18n::t!("开始导出")).clicked() {
                go = true;
            }
        });
    });

    // Esc 与点背景都算取消。
    //
    // Escape and a backdrop click both cancel.
    if modal.should_close() {
        cancel = true;
    }
    if cancel {
        ui.ctx().data_mut(|d| d.remove::<ExportChoice>(form_id));
        return (None, true);
    }
    if go {
        ui.ctx().data_mut(|d| d.remove::<ExportChoice>(form_id));
        return (Some(form), true);
    }
    ui.ctx().data_mut(|d| d.insert_temp(form_id, form));
    (None, false)
}

#[cfg(test)]
mod update_window_tests {
    use super::*;
    use aevum_core::update::{ReleaseAsset, UpdateCheck};

    /// 与 `--selftest` 同一块画布：1600×1000，最大纹理 8192。
    ///
    /// The same canvas as `--selftest`: 1600x1000, max texture side 8192.
    fn raw_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1000.0),
            )),
            max_texture_side: Some(8192),
            ..Default::default()
        }
    }

    /// 画一帧，把这一帧上屏的每个文字形状拼成一段返回。
    ///
    /// Paint one frame and concatenate every text shape that reached the screen.
    fn painted(state: &mut AppState, ctx: &Context) -> String {
        let mut out = ctx.run_ui(raw_input(), |ui| crate::shell::ui(state, ui));
        let mut text = String::new();
        for clipped in &out.shapes {
            collect(&clipped.shape, &mut text);
        }
        out.textures_delta.clear();
        text
    }

    /// 递归收字：分组形状（`Shape::Vec`）里也压着文字，只扫顶层会漏掉半个窗口。
    ///
    /// Collect text recursively: grouped shapes (`Shape::Vec`) carry text too, and a
    /// top-level-only scan misses half a window.
    fn collect(shape: &egui::Shape, out: &mut String) {
        match shape {
            egui::Shape::Text(t) => {
                out.push_str(t.galley.text());
                out.push('\n');
            }
            egui::Shape::Vec(shapes) => {
                for s in shapes {
                    collect(s, out);
                }
            }
            _ => {}
        }
    }

    /// 一条「有新版本」的结论，资产与清单地址都齐（安装按钮要的就是这两样）。
    ///
    /// An "update available" conclusion with both the asset and the manifest URL, which
    /// is exactly what the install button needs.
    fn found_update() -> UpdateCheck {
        UpdateCheck {
            current: aevum_core::update::current_version().to_owned(),
            latest: Some("v9.9.9".to_owned()),
            asset: Some(ReleaseAsset {
                name: "Aevum-Studio-v9.9.9-windows-x64.zip".to_owned(),
                url: "https://example.invalid/pkg.zip".to_owned(),
                size: 4 * 1024 * 1024,
            }),
            note: "发现新版本 v9.9.9".to_owned(),
        }
    }

    /// 更新窗口的四种阶段每种都要真的画出来。
    ///
    /// 这条挡的是「阶段分支写错但编译通过」：漏一个分支、把进度读数拼错成空串、
    /// 或者 `match` 里某一臂忘了摆按钮，都不会报错，只会让用户在某个时刻面对一扇
    /// 空窗。断言里只认**那一刻该看见的那几个字**。
    ///
    /// Every one of the four phases must really paint.
    ///
    /// This catches "the phase branch is wrong but it compiles": a missing arm, a progress
    /// reading that formats to an empty string, or a `match` arm that forgets its button
    /// all fail silently and leave the user facing an empty window at some moment. The
    /// assertions only require the few words that must be visible at that moment.
    #[test]
    fn every_update_phase_paints_its_own_copy() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        state.update_open = true;

        // 首开还没查过：给一句话，不画一片空白。
        //
        // Never checked when first opened: one line rather than a blank area.
        let _ = painted(&mut state, &ctx);
        let cold = painted(&mut state, &ctx);
        assert!(cold.contains("还没有检查过"), "首开状态没有说明：\n{cold}");

        // 空闲 + 可安装 → 主按钮「下载并安装」。
        //
        // Idle and installable -> the primary "download and install" button.
        state.update_check = Some(found_update());
        state.update_body = "改动 1\n改动 2".to_owned();
        let idle = painted(&mut state, &ctx);
        for want in ["当前版本", "远端版本", "发布说明", "下载并安装"] {
            assert!(idle.contains(want), "空闲态缺「{want}」：\n{idle}");
        }

        // 下载中 → 进度读数 + 禁用的按钮（禁用态照样画字，所以这里认字不认灰）。
        //
        // Downloading -> the progress reading plus a disabled button (a disabled widget
        // still paints its label, so the text is what gets asserted).
        state.update_phase = UpdatePhase::Downloading;
        if let Ok(mut slot) = state.update_progress.lock() {
            *slot = (1024 * 1024, 4 * 1024 * 1024);
        }
        let mid = painted(&mut state, &ctx);
        assert!(
            mid.contains("1.0 / 4.0 MB"),
            "下载中没有真实字节读数：\n{mid}"
        );

        // 备好 → 「已准备好 …，重启后完成安装」+「立即重启并安装」。
        //
        // Ready -> "… is ready, restart to finish" plus "restart and install".
        state.update_phase = UpdatePhase::Ready;
        state.update_ready = Some(crate::state::PreparedInstall {
            version: "v9.9.9".to_owned(),
            script: std::path::PathBuf::from("t.cmd"),
            new_exe: std::path::PathBuf::from("new.exe"),
            target: std::path::PathBuf::from("aevum.exe"),
        });
        let ready = painted(&mut state, &ctx);
        for want in ["已准备好 v9.9.9", "立即重启并安装"] {
            assert!(ready.contains(want), "就绪态缺「{want}」：\n{ready}");
        }

        // 失败 → 红字一行 + 可以重试的按钮。
        //
        // Failed -> one red line plus a retry button.
        state.update_phase = UpdatePhase::Idle;
        state.update_ready = None;
        state.update_error = Some("下载失败（退出码 6）：网络不可达".to_owned());
        let failed = painted(&mut state, &ctx);
        assert!(
            failed.contains("下载失败（退出码 6）"),
            "失败态没有显示原因：\n{failed}"
        );
    }

    /// 回滚确认框要真的上屏，并且按钮文案在。
    ///
    /// The rollback confirmation must paint, with its buttons' copy present.
    #[test]
    fn the_rollback_confirmation_paints() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        state.rollback_confirm = true;
        let _ = painted(&mut state, &ctx);
        let text = painted(&mut state, &ctx);
        for want in ["回滚到上一版", ".failed", "取消", "回滚并重启"] {
            assert!(text.contains(want), "回滚确认框缺「{want}」：\n{text}");
        }
    }
}
