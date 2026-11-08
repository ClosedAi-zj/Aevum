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
use egui::{
    Align, Align2, Color32, Context, Layout, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui,
    Vec2,
};
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

/// 所有 Modal 的统一尺寸入口：宽度走主题常量并按视口夹取，高度不超过视口 62%。
/// 四个 Modal（命令面板 / 回滚 / 错误 / 导出确认）必须都过这里，否则又会各自
/// 写死一个宽度，换到窄屏就顶出去。
///
/// One sizing entry for every modal: width from a theme constant clamped to the
/// viewport, height capped at 62% of it. All four modals (palette / rollback / error /
/// export confirm) go through here; a private width would drift out on a narrow screen.
pub(super) fn modal_box(ui: &mut Ui, width: f32) {
    let vp = ui.ctx().input(|i| i.viewport_rect());
    ui.set_width(theme::modal_width(width, vp.width()));
    ui.set_max_height(theme::modal_max_h(vp.height()));
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
        // 标题带走 header()（与画布顶栏同源）：窗口本体是最亮的 bg()，
        // 标题带比它暗一档才读得出「这是标题不是内容」——全应用统一的
        // 头部带语言。panel() 与 bg() 只差两度，标题与内容糊成一片。
        //
        // The title band takes header() (same source as the canvas top bars): the
        // window body is the lightest material, so the band must sit one step darker
        // to read as *title, not content* -- the app-wide header-band language.
        // panel() and bg() differ by only two steps and blurred together.
        .fill(header())
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
pub(super) fn soft_sep(ui: &mut Ui) {
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
    // 插件菜单只列插件本身：曾经的「输入范围」选择器没有任何代码消费，
    // 还让内置动作被误置灰。
    //
    // The plugin menu lists plugins only: the old "input scope" selector had no
    // consumer and only made built-in actions look disabled.

    let mut open_market = false;
    let mut fire: Option<(String, String)> = None;
    for (title, items) in &rows {
        menu_section(ui, title);
        for r in items {
            let resp = ui.add(
                egui::Button::new(RichText::new(&r.label).size(SIZE_BODY))
                    .shortcut_text(RichText::new(&r.tag).size(SIZE_SMALL).color(dim())),
            );
            let resp = if r.desc.is_empty() {
                resp
            } else {
                resp.on_hover_text(&r.desc)
            };
            if resp.clicked() {
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
    // 上界必须按**视口**算，不能只给一个绝对值：窗口尺寸由 egui 记住，换到小屏
    // 或窗口被缩小时，记忆里那条 1080px 宽的板子会顶出视口右缘，右栏文字被屏幕
    // 边缘裁掉（用户 2026-11-07 第二轮截图：右栏插件名全被切断）。
    //
    // The upper bound must come from the **viewport**, not a fixed number: egui remembers a
    // resizable window's size, so on a smaller screen (or a smaller app window) the
    // remembered 1080 px slab runs past the right edge and the list column gets clipped by
    // the screen edge (user screenshot, second round on 2026-11-07).
    let vp = ctx.input(|i| i.viewport_rect());
    let bounds = theme::win_max(vp.size());
    let max_w = bounds.x;
    let max_h = bounds.y;
    let default_size = theme::win_default(Vec2::new(WIN_MARKET_W, WIN_MARKET_H), vp.size());
    let min_size = theme::win_min(Vec2::new(WIN_MARKET_MIN_W, WIN_MARKET_MIN_H), vp.size());
    let win = egui::Window::new(crate::i18n::t!("插件"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(default_size)
        .min_size(min_size)
        // 上界也要给：可缩放窗口的尺寸由 egui 记住，一旦被拉到整屏宽度，下次打开
        // 还是那条大板子、内容全挤在左上角（用户 2026-11-07 截图）。夹住上界，
        // 用户仍可自由调小。
        //
        // Cap the upper bound too: egui remembers a resizable window's size, so once it has
        // been stretched to the full width it reopens as that slab with the content pinned
        // to the top-left corner (user screenshot, 2026-11-07). Capping still leaves full
        // freedom to shrink.
        .max_width(max_w)
        .max_height(max_h)
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
            // 已经兜底（560×360），不再用 set_min_size 硬顶内容高度——那会
            // 把缩放手柄顶死，窗口拉不大也收不小。
            let body_h = ui.available_height() - 4.0;
            ui.horizontal_top(|ui| {
                // 左栏：类别 + 插件目录。类别行整行可点、计数贴右缘；原生
                // Button 把「名字  数字」当一整串文字左对齐，扫一列时数字对不齐。
                //
                // Left column: categories and the plug-in folder. Whole rows are clickable
                // with a right-aligned count; a native Button treats "name  count" as one
                // left-aligned string, so the numbers never line up down the column.
                ui.allocate_ui_with_layout(
                    Vec2::new(MARKET_SIDE_W, body_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.label(
                            RichText::new(crate::i18n::t!("类别"))
                                .size(SIZE_SMALL)
                                .color(dim()),
                        );
                        ui.add_space(5.0);
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
                            // 「全部」要算上清单坏掉的，否则用户会觉得少了东西。
                            if k.is_none() {
                                n += state.registry.errors().len();
                            }
                            if market_cat_row(ui, label, n, state.market_filter == k).clicked() {
                                state.market_filter = k;
                            }
                            ui.add_space(2.0);
                        }
                        // 目录与重扫钉在底部：类别是常读的，路径与维护动作是偶尔用的。
                        let spare = ui.available_height() - 76.0;
                        if spare > 0.0 {
                            ui.add_space(spare);
                        }
                        ui.label(
                            RichText::new(crate::i18n::t!("插件目录"))
                                .size(SIZE_SMALL)
                                .color(dim()),
                        );
                        ui.add_space(2.0);
                        // 显示真实路径而不是 "%APPDATA%\..."：后者是占位符，用户
                        // 照着它找不到目录。路径过长时省略，悬停给全文。
                        //
                        // Show the real path rather than "%APPDATA%\...": the placeholder
                        // cannot be followed to the folder. Long paths elide; hover shows
                        // the whole thing.
                        let dir = crate::settings::plugin_dir();
                        let full = dir.display().to_string();
                        ui.add(
                            egui::Label::new(RichText::new(&full).size(SIZE_SMALL).color(dim()))
                                .truncate(),
                        )
                        .on_hover_text(&full);
                        ui.add_space(5.0);
                        if ui
                            .add_sized(
                                [ui.available_width(), 24.0],
                                egui::Button::new(
                                    RichText::new(crate::i18n::t!("重新扫描")).size(SIZE_SMALL),
                                ),
                            )
                            .clicked()
                        {
                            rescan = true;
                        }
                    },
                );

                // 竖分栏：一条 1px 线比软分割线更直白地说明「左选类别、右看清单」。
                let (line, _) = ui.allocate_exact_size(Vec2::new(1.0, body_h), Sense::hover());
                ui.painter().line_segment(
                    [line.center_top(), line.center_bottom()],
                    Stroke::new(1.0, divider()),
                );

                // 右栏：搜索 + 列表。宽度按「剩余宽度减一点余量」算死：
                // `available_width()` 在 horizontal 里会含未消费的间距，直接拿去
                // 分配会让右栏顶出窗口右缘（列表文字被裁）。
                //
                // Right column: search + list. Its width is pinned to the remaining space
                // minus a small margin; `available_width()` inside a horizontal still counts
                // unspent spacing, and allocating that much pushes the column past the
                // window's right edge, clipping the list text.
                let right_w = (ui.available_width() - 2.0).max(180.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(right_w, body_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        // 搜索框吃满整栏，右侧留一个固定宽度的清除位：查询为空时
                        // 那一格也不塌陷，输入框宽度不会随内容跳动。
                        //
                        // The search field spans the column with a fixed-width clear slot
                        // on the right. The slot stays even when empty, so the field width
                        // never jumps with the text.
                        ui.horizontal(|ui| {
                            let field_w = (ui.available_width() - 30.0).max(120.0);
                            ui.add_sized(
                                [field_w, 26.0],
                                egui::TextEdit::singleline(&mut state.market_query)
                                    .hint_text(crate::i18n::t!("搜索名称、厂商或说明"))
                                    .margin(Vec2::new(8.0, 4.0)),
                            );
                            if !state.market_query.is_empty()
                                && ui
                                    .add_sized(
                                        [20.0, 26.0],
                                        egui::Button::new(
                                            RichText::new("×").size(SIZE_BODY).color(dim()),
                                        ),
                                    )
                                    .clicked()
                            {
                                state.market_query.clear();
                            }
                        });

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

                        // 统计行：当前筛选下有多少个、开了几个；坏清单只给一个数，
                        // 详情放在列表末尾，不让它抢在前面。
                        //
                        // Summary line: how many under the current filter and how many
                        // are on. Broken manifests get a count here; details stay at the
                        // end of the list.
                        let on = hits
                            .iter()
                            .filter(|&&i| state.registry.all()[i].enabled)
                            .count();
                        let bad = state.registry.errors().len();
                        ui.add_space(7.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(crate::i18n::tr_fmt("{} 个插件", &[&hits.len()]))
                                    .size(SIZE_SMALL)
                                    .color(dim()),
                            );
                            ui.label(RichText::new("·").size(SIZE_SMALL).color(dim()));
                            ui.label(
                                RichText::new(crate::i18n::tr_fmt("已启用 {}", &[&on]))
                                    .size(SIZE_SMALL)
                                    .color(dim()),
                            );
                            if bad > 0 {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(crate::i18n::tr_fmt(
                                            "{} 份清单无法读取",
                                            &[&bad],
                                        ))
                                        .size(SIZE_SMALL)
                                        .color(danger()),
                                    );
                                });
                            }
                        });
                        ui.add_space(2.0);
                        let (rule, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 1.0),
                            Sense::hover(),
                        );
                        theme::rule(
                            ui.painter(),
                            rule.left(),
                            rule.right(),
                            rule.center().y,
                            divider(),
                        );
                        ui.add_space(2.0);

                        ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if hits.is_empty() {
                                    ui.add_space(28.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new(crate::i18n::t!("没有匹配的插件"))
                                                .size(SIZE_BODY)
                                                .color(text()),
                                        );
                                        ui.add_space(2.0);
                                        ui.label(
                                            RichText::new(crate::i18n::t!(
                                                "换一个关键词，或重新扫描插件目录"
                                            ))
                                            .size(SIZE_SMALL)
                                            .color(dim()),
                                        );
                                    });
                                }
                                for i in hits {
                                    let p = &state.registry.all()[i];
                                    let meta = &p.manifest.plugin;
                                    let enabled = p.enabled;
                                    if market_row(
                                        ui,
                                        &meta.name,
                                        &meta.version,
                                        &meta.description,
                                        &format!("{} · {}", meta.kind.cn(), meta.source.cn()),
                                        enabled,
                                    ) {
                                        toggle = Some((meta.id.clone(), !enabled));
                                    }
                                }

                                // 坏清单单独列在最后：不藏，也不打断上面的列表。
                                // 路径与原因压成一行、超出省略，悬停给全文。
                                //
                                // Broken manifests stay last: not hidden, and they do not
                                // interrupt the list. Path and reason share one elided line;
                                // hover gives the full text.
                                let errs = state.registry.errors();
                                if !errs.is_empty() {
                                    ui.add_space(10.0);
                                    ui.label(
                                        RichText::new(crate::i18n::t!("清单无法读取"))
                                            .size(SIZE_SMALL)
                                            .color(danger()),
                                    );
                                    ui.add_space(2.0);
                                    for (path, why) in errs {
                                        let line = format!("{}：{why}", path.display());
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&line).size(SIZE_SMALL).color(dim()),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(&line);
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
        let word = crate::i18n::tr(if on { "已启用" } else { "已停用" });
        state.status = crate::i18n::tr_fmt("{}：{}", &[&id, &word]);
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

/// 插件窗口左栏宽度：够放下「修图滤镜」四个字加一个两位数计数，
/// 又不至于把右栏清单挤窄。
///
/// Sidebar width: wide enough for a four-character category plus a two-digit count,
/// without squeezing the list column.
const MARKET_SIDE_W: f32 = 168.0;

/// 插件行高：两行文字（名称/版本、说明/元信息）+ 上下留白。
///
/// Plugin row height: two text lines (name/version, description/meta) plus padding.
const MARKET_ROW_H: f32 = 56.0;

/// 左栏一条类别：整行点击、左名右数；选中是实心底，悬停只抬一档。
///
/// One category row: whole-row click, name left and count right; the selected state
/// uses the solid fill, hover lifts one step.
fn market_cat_row(ui: &mut Ui, label: &str, count: usize, active: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, radius_control(), selected());
    } else if resp.hovered() {
        p.rect_filled(rect, radius_control(), row_hover());
    }
    let (name_fg, count_fg) = if active {
        (on_selected(), on_selected())
    } else {
        (text(), dim())
    };
    p.text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        crate::i18n::tr(label),
        font(SIZE_BODY),
        name_fg,
    );
    p.text(
        Pos2::new(rect.right() - 10.0, rect.center().y),
        Align2::RIGHT_CENTER,
        count.to_string(),
        font(SIZE_SMALL),
        count_fg,
    );
    resp
}

/// 一行插件：名称 + 版本、单行说明、类别·来源、右侧开关。整行可点。
///
/// 说明始终单行省略：插件描述常有整段文字，任它换行会把固定的 56px 节奏
/// 打乱，一屏能读的条数完全由描述长度决定。悬停整行可以看完整说明。
///
/// One plugin row: name + version, one-line description, kind · source, and the
/// switch on the right. The whole row is clickable.
///
/// The description is always elided to one line: plugin descriptions often run to a
/// paragraph, and letting them wrap destroys the fixed 56 px rhythm, making the
/// number of readable rows depend on description length. Hovering the row shows it
/// in full.
fn market_row(
    ui: &mut Ui,
    name: &str,
    version: &str,
    desc: &str,
    meta: &str,
    enabled: bool,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), MARKET_ROW_H),
        Sense::click(),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, radius_control(), row_hover());
    }

    // 开关：34×16 的胶囊，开时强调色底 + 白钮，关时输入框底 + 描边 + 正文色钮。
    // 原生 checkbox 在无标签时只剩一个孤立方框，和右侧一列元信息对不齐。
    //
    // The switch: a 34x16 pill. On = accent track with the light knob; off = field
    // fill, a hairline border, and the body-coloured knob. A label-less checkbox is a
    // lone square that cannot line up with the metadata column.
    let sw = Rect::from_min_size(
        Pos2::new(rect.right() - 12.0 - 34.0, rect.center().y - 8.0),
        Vec2::new(34.0, 16.0),
    );
    let track = if enabled {
        accent()
    } else if resp.hovered() {
        field_hover()
    } else {
        field()
    };
    p.rect_filled(sw, 8.0, track);
    if !enabled {
        p.rect_stroke(
            sw,
            8.0,
            Stroke::new(1.0, border()),
            egui::StrokeKind::Inside,
        );
    }
    let knob_x = if enabled {
        sw.right() - 8.0
    } else {
        sw.left() + 8.0
    };
    p.circle_filled(
        Pos2::new(knob_x, sw.center().y),
        6.0,
        if enabled { on_accent() } else { text() },
    );

    let left = rect.left() + 12.0;
    let text_right = sw.left() - 14.0;

    // 第一行：名称（正文）+ 版本（小字，压暗）。
    let name_y = rect.top() + 9.0;
    let ver = version.trim();
    let ver_w = if ver.is_empty() {
        0.0
    } else {
        p.layout_no_wrap(ver.to_owned(), font(SIZE_SMALL), dim())
            .size()
            .x
            + 7.0
    };
    let name_galley = elided_text(ui, name, SIZE_BODY, text(), text_right - left - ver_w);
    let name_w = name_galley.size().x;
    p.galley(Pos2::new(left, name_y), name_galley, text());
    if !ver.is_empty() {
        p.text(
            Pos2::new(left + name_w + 7.0, name_y + 1.5),
            Align2::LEFT_TOP,
            ver,
            font(SIZE_SMALL),
            dim(),
        );
    }

    // 第二行：说明（单行省略）+ 类别·来源（贴开关左缘）。
    let line_y = rect.top() + 30.0;
    let meta_w = if meta.is_empty() {
        0.0
    } else {
        p.layout_no_wrap(meta.to_owned(), font(SIZE_SMALL), dim())
            .size()
            .x
    };
    if !meta.is_empty() {
        p.text(
            Pos2::new(text_right, line_y + 1.0),
            Align2::RIGHT_TOP,
            meta,
            font(SIZE_SMALL),
            dim(),
        );
    }
    if !desc.is_empty() {
        let desc_w = (text_right - left - meta_w - 14.0).max(20.0);
        p.galley(
            Pos2::new(left, line_y),
            elided_text(ui, desc, SIZE_SMALL, dim(), desc_w),
            dim(),
        );
    }

    // 行尾 hairline：与组头的通栏线同口径，整列读作一张表，而不是卡片堆。
    theme::soft_rule(p, rect.left(), rect.right(), rect.bottom());
    let resp = if desc.is_empty() {
        resp
    } else {
        resp.on_hover_text(desc)
    };
    resp.clicked()
}

/// 把一段文字排成「最多一行、超出加省略号」的 galley。
///
/// `Painter::text` 不会截断：长插件名会直接画出自己的矩形、压到开关上。
/// 这里显式给 `max_rows = 1` 与 `break_anywhere`，宽度之外一律省略。
///
/// Lay text out as "at most one row, ellipsis beyond that". `Painter::text` never
/// truncates: a long plugin name would paint past its own rect and run under the
/// switch. Setting `max_rows = 1` and `break_anywhere` elides everything past the
/// width.
fn elided_text(
    ui: &Ui,
    text: &str,
    size: f32,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::text::TextFormat {
            font_id: font(size),
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

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
        ctx.input(|i| i.viewport_rect()).right_top() - Vec2::new(WIN_LOOK_W + 14.0, -52.0),
        Vec2::new(WIN_LOOK_W, WIN_LOOK_H),
    );
    let win = egui::Window::new(crate::i18n::t!("外观与布局"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(WIN_LOOK_W)
        .default_pos(pos)
        // 与插件市场同一个标题栏口径：深色底、无高亮描边。
        .title_frame(title_frame())
        .max_height(
            theme::win_default(
                Vec2::new(WIN_LOOK_W, WIN_LOOK_H),
                ctx.input(|i| i.viewport_rect()).size(),
            )
            .y,
        )
        .show(ctx, |ui| {
            // 外观窗有十几节，内容自然高度能顶满整屏（用户 2026-11-07 截图：
            // 窗口从 y=0 一直压到底部胶片条）。把正文放进带上限的滚动区，
            // 窗口高度就固定在这一档，换小屏也跟着收。
            //
            // The appearance window has a dozen sections and its natural height can fill
            // the entire screen (user screenshot: it ran from y=0 down to the filmstrip).
            // A capped scroll area keeps the window at this height and shrinks with the
            // viewport.
            let look_h = theme::win_default(
                Vec2::new(WIN_LOOK_W, WIN_LOOK_H),
                ctx.input(|i| i.viewport_rect()).size(),
            )
            .y;
            let look_cap = (look_h - 104.0).max(200.0);
            egui::ScrollArea::vertical()
                .max_height(look_cap)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    // 定宽而不是让内容撑：锚在右上时，egui 会按内容回缩，回缩到比一行控件
                    // 还窄就会把左边的选项挤到窗口外面去。
                    ui.set_min_width(WIN_LOOK_MIN_W);
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
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
                            let c = rect.center();
                            ui.painter().circle_filled(c, 9.0, col);
                            if active {
                                ui.painter()
                                    .circle_stroke(c, 11.0, Stroke::new(1.5, text()));
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

                    // 界面明暗微调（自定义外观）：只偏移背景族，文字三档不动——
                    // 文字跟着漂，这个滑块就变成对比度旋钮（见 theme.rs Palette::of）。
                    //
                    // Interface brightness trim (custom appearance): shifts the background
                    // family only; the three text steps stay put, otherwise this slider would
                    // become a contrast knob (see Palette::of in theme.rs).
                    look_section(ui, "界面明暗微调");
                    // 七档离散量用 chip 行（-3..+3）：这行原来是全应用唯一一处 egui 原生
                    // Slider（宽 200 写死、灰轨白钮），与自绘滑块两套语言——用户截图圈名
                    // 「颜色/样式不统一」。chips 让档位一目了然，读数并入选中态，归零点档 0。
                    //
                    // The seven discrete steps become a chip row (-3..+3): this was the only
                    // stock egui Slider left in the app (hard-coded 200 px, grey rail, white
                    // knob) -- a second control language (user-flagged). Chips make the steps
                    // legible; the readout is the selected chip, tapping 0 zeroes it.
                    ui.horizontal_wrapped(|ui| {
                        for off in -3..=3 {
                            let label = format!("{off:+}");
                            if bar_chip(ui, &label, state.appearance.gray_offset == off).clicked() {
                                state.appearance.gray_offset = off;
                            }
                        }
                        if state.appearance.gray_offset != 0 {
                            ui.add_space(4.0);
                            if bar_chip(ui, "归零", false).clicked() {
                                state.appearance.gray_offset = 0;
                            }
                        }
                    });
                    if state.appearance.gray_offset.abs() > 2 {
                        ui.label(
                            RichText::new(crate::i18n::t!("对比度可能不足，部分文字可读性下降"))
                                .size(SIZE_SMALL)
                                .color(warn()),
                        );
                    }
                    // 恢复默认：强调色回专业深蓝、明暗偏移归零（皮肤与主题留在原处，
                    // 那是两套独立偏好，一键重置不该顺手改掉）。
                    //
                    // Reset: accent back to the professional blue and the trim back to zero.
                    // Skin and theme are left alone -- they are independent preferences and a
                    // one-click reset should not quietly change them.
                    if bar_chip(ui, "恢复默认强调色与明暗", false).clicked() {
                        state.appearance.accent_h = 210.0;
                        state.appearance.accent_s = 100.0;
                        state.appearance.accent_l = 44.5;
                        state.appearance.gray_offset = 0;
                    }

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
                    // 工作区放在预设之上：它调的是"这一趟活要多大地方"，比"状态栏在上还是在下"
                    // 更高一层。三个值都能反推（右栏宽 + 胶片条 + 密度），拖一下宽度就变
                    // "自定义"——不假装还停在某个工作区。
                    //
                    // The workspace sits above the presets: it answers "how much room does this job
                    // need", a level up from "is the status bar on top". All three reverse-match
                    // (dock width + filmstrip + density), so a drag turns it into "custom" rather
                    // than pretending to still be one of them.
                    look_row(
                        ui,
                        "工作区",
                        crate::layout::Workspace::of(state.layout, state.dock_w),
                        &[
                            (Some(crate::layout::Workspace::Sequence), "序列"),
                            (Some(crate::layout::Workspace::Develop), "调色"),
                            (Some(crate::layout::Workspace::Render), "成片"),
                        ],
                        |w| {
                            if let Some(w) = w {
                                state.apply_workspace(w);
                            }
                        },
                    );
                    let preset = state.layout.highlighted_preset(state.dock_w);
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
                        "信息密度",
                        state.layout.dense,
                        &[(false, "标准"), (true, "紧凑")],
                        |v| state.layout.dense = v,
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
                        "工具轨",
                        state.layout.rail,
                        &[
                            (crate::layout::Side::Right, "右"),
                            (crate::layout::Side::Left, "左"),
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
                });
            ui.add_space(6.0);
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
        Vec2::new(WIN_PRESETS_W, WIN_PRESETS_H),
    );
    let win = egui::Window::new(crate::i18n::t!("预设与快照"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(WIN_PRESETS_W)
        .default_height(WIN_PRESETS_H)
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
        ctx.input(|i| i.viewport_rect()).center() - Vec2::new(WIN_ABOUT_W * 0.5, 160.0),
        Vec2::new(WIN_ABOUT_W, WIN_ABOUT_H),
    );
    let win = egui::Window::new(crate::i18n::t!("快捷键与关于"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(WIN_ABOUT_W)
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
        ctx.input(|i| i.viewport_rect()).center() - Vec2::new(WIN_UPDATE_W * 0.5, 210.0),
        Vec2::new(WIN_UPDATE_W, WIN_UPDATE_H),
    );
    let vp = ctx.input(|i| i.viewport_rect());
    let default_size = theme::win_default(Vec2::new(WIN_UPDATE_W, WIN_UPDATE_H), vp.size());
    let min_size = theme::win_min(Vec2::new(WIN_UPDATE_MIN_W, WIN_UPDATE_MIN_H), vp.size());
    let win = egui::Window::new(crate::i18n::t!("检查更新"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(default_size)
        .min_size(min_size)
        .max_width(theme::win_max(vp.size()).x)
        .max_height(theme::win_max(vp.size()).y)
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
                let cap = (theme::win_max(vp.size()).y * 0.5).max(80.0);
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
        modal_box(ui, MODAL_W_NARROW);
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
        NoteLine::Rule => {
            ui.add_space(3.0);
            let w = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 1.0), Sense::hover());
            theme::soft_rule(ui.painter(), rect.left(), rect.right(), rect.center().y);
            ui.add_space(3.0);
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

/// 错误弹窗：任何失败都必须「被看见一次」，不能只在状态栏滑过或打进控制台。
///
/// 状态栏那行是常驻摘要（可点掉、会被下一条覆盖），弹窗是**阻断式的一次确认**：
/// 复制得出原文、按 Esc / 点背景 / 点「知道了」才消失。正文可选中，方便把原始
/// 错误贴给开发者——这一条是「别再静默失败」的落地口径。
///
/// Error dialog: every failure must be *seen once*, not merely slip through the status bar
/// or a console log.
///
/// The status-bar line is a persistent, dismissible summary that the next message can
/// overwrite; the dialog is a blocking single acknowledgement -- copy the text out, and only
/// Esc / backdrop / "Got it" dismisses it. The body is selectable so the raw error can be
/// pasted to a developer, which is what "stop failing silently" means in practice.
pub(super) fn error_dialog(state: &mut AppState, ctx: &egui::Context) {
    let Some(msg) = state.error_modal.clone() else {
        return;
    };
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::unique("aevum-error-dialog")).show(ctx, |ui| {
        modal_box(ui, MODAL_W_WIDE);
        ui.horizontal(|ui| {
            // danger 色的小圆点：不用图标字体，也不需要额外资源。
            let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 3.5, danger());
            ui.label(
                RichText::new(crate::i18n::t!("出错了"))
                    .font(font(SIZE_BODY))
                    .color(text())
                    .strong(),
            );
        });
        ui.add_space(6.0);
        // 正文可选中：用户要能把原始错误整段复制出来贴给开发者，这是「可见」之外
        // 的第二层要求。
        //
        // The body is selectable so the raw error can be copied out and pasted to a
        // developer -- that is the second requirement beyond merely being visible.
        ui.add(
            egui::Label::new(RichText::new(&msg).font(font(SIZE_LABEL)).color(muted()))
                .wrap()
                .selectable(true),
        );
        ui.add_space(10.0);
        soft_sep(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if bar_button(ui, crate::i18n::t!("复制详情")).clicked() {
                ui.ctx().copy_text(msg.clone());
            }
            if bar_primary(ui, crate::i18n::t!("知道了")).clicked() {
                close = true;
            }
        });
    });
    if modal.should_close() {
        close = true;
    }
    if close {
        state.error_modal = None;
    }
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

    /// 新错误必须弹窗，且正文与确认按钮真的上屏——这是「不许静默失败」的判据。
    ///
    /// 只断言 `error` 被赋值是不够的：那正是用户抱怨的「只写在状态栏 / 只打控制台」。
    /// 这里跑一帧真实界面，抓绘制出的文字，要求弹窗标题、错误正文、确认按钮三者都在。
    ///
    /// A new error must raise a dialog, and its body plus the acknowledge button must
    /// actually paint. This is the "no silent failures" criterion.
    ///
    /// Asserting that `error` was assigned is not enough -- that is exactly the status-line /
    /// console-only behaviour the user complained about. This runs a real frame and collects
    /// the painted text, requiring the title, the body and the button to be present.
    #[test]
    fn a_new_error_pops_a_dialog_with_its_text() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        state.error = Some("HDR 合并至少需要 2 帧：把「从当前帧起，前后各取」调大".to_owned());

        // 两帧：`Area`（Modal 的载体）第一帧只做尺寸测量，第二帧才出形状。
        // 只跑一帧会「什么都抓不到」，这正是旧回滚判据一直假绿的机制。
        //
        // Two frames: an `Area` (what Modal rides on) only measures on the first frame and
        // emits shapes on the second. A single frame collects nothing -- exactly the
        // mechanism that kept the old rollback criterion falsely green.
        let _ = painted(&mut state, &ctx);
        let text = painted(&mut state, &ctx);

        assert!(
            state.error_modal.is_some(),
            "新错误没有被抬成弹窗，只留在状态栏"
        );
        assert!(
            text.contains("HDR 合并至少需要 2 帧：把「从当前帧起，前后各取」调大"),
            "弹窗没有把错误正文画出来：\n{text}"
        );
        assert!(text.contains("知道了"), "弹窗缺少确认按钮：\n{text}");
    }

    /// 点「导出当前帧」时，确认框的范围必须预选「当前帧」。
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
        // 同上：Area 需要两帧才出形状。此前只跑两帧却漏了断言口径 —— 那四个词
        // 里「回滚到上一版」在菜单里也有一份，单帧也会「通过」，是假绿。
        //
        // Same as above: an Area needs two frames. The old version ran two frames but its
        // assertions were too loose -- "回滚到上一版" also exists in the menu, so even a
        // single frame "passed", which was a false green.
        let _ = painted(&mut state, &ctx);
        let text = painted(&mut state, &ctx);
        for want in ["回滚到上一版", ".failed", "取消", "回滚并重启"] {
            assert!(text.contains(want), "回滚确认框缺「{want}」：\n{text}");
        }
    }
}
