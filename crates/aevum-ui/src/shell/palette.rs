//! 命令面板：`Ctrl+K` 唤出，输入中文或快捷键即可跳到任意动作。
//!
//! 为什么值得单独做一个模块：PS 与 Lightroom 的动作都埋在多层菜单里，熟练用户靠
//! 记忆快捷键、生手靠逐个翻菜单——两者都要求"先知道它在哪"。命令面板把这件事反
//! 过来：**你只要记得要做什么，不用记得它藏在哪**。这是本产品相对它们唯一能拿到
//! 的结构性优势，所以值得做，而不是再加一层菜单。
//!
//! Command palette: `Ctrl+K` opens it; type Chinese or a shortcut to reach any action.
//!
//! Why a module of its own: Photoshop and Lightroom bury actions in nested menus, so
//! experts memorise shortcuts and beginners hunt through menus -- both require knowing
//! *where* something lives. The palette inverts that: you only need to remember *what* you
//! want. That is the one structural advantage this product can hold over them, which is
//! why it deserves real work instead of another menu level.

use super::MICRO;
use crate::state::AppState;
use crate::theme::{
    MODAL_W_WIDE, SIZE_BODY, SIZE_SMALL, accent_dim, dim, divider, elev_hover, muted,
    radius_control, text,
};
use egui::{Align2, Context, Pos2, Sense, Vec2};

/// 一条可执行动作。`hint` 是分类，`keys` 是快捷键提示（没有就留空）。
///
/// One runnable action. `hint` is its category, `keys` the shortcut hint (empty if none).
struct Action {
    id: &'static str,
    name: &'static str,
    hint: &'static str,
    keys: &'static str,
}

/// 动作表。顺序即默认排序：模块在前（最常切），视图次之，文件与窗口在后。
///
/// The action table. Order is the default ranking: modules first (most switching), views
/// next, file and window last.
///
/// 只收**真的能执行**的动作：表里多一条点不动的项，比少十条更伤信任。
///
/// Only actions that really run are listed: one dead row costs more trust than ten
/// missing ones.
const ACTIONS: &[Action] = &[
    // 工作区排在最前：它回答"接下来在什么布局里干活"，比"去哪个模块"高一层。
    //
    // Workspaces come first: "what layout am I working in" is a level above "which module".
    Action {
        id: "ws-sequence",
        name: "工作区：序列",
        hint: "视图",
        keys: "",
    },
    Action {
        id: "ws-develop",
        name: "工作区：调色",
        hint: "视图",
        keys: "",
    },
    Action {
        id: "ws-render",
        name: "工作区：成片",
        hint: "视图",
        keys: "",
    },
    Action {
        id: "mod-seq",
        name: "序列",
        hint: "模块",
        keys: "",
    },
    Action {
        id: "mod-deflicker",
        name: "去闪",
        hint: "模块",
        keys: "",
    },
    Action {
        id: "mod-develop",
        name: "显影",
        hint: "模块",
        keys: "",
    },
    Action {
        id: "mod-frame",
        name: "构图",
        hint: "模块",
        keys: "",
    },
    Action {
        id: "mod-render",
        name: "成片",
        hint: "模块",
        keys: "",
    },
    Action {
        id: "mod-audio",
        name: "声音",
        hint: "模块",
        keys: "",
    },
    Action {
        id: "mod-merge",
        name: "合成",
        hint: "模块",
        keys: "",
    },
    Action {
        id: "view-split",
        name: "前后对比",
        hint: "视图",
        keys: "",
    },
    Action {
        id: "view-grid",
        name: "网格",
        hint: "视图",
        keys: "G",
    },
    Action {
        id: "view-curves",
        name: "亮度曲线",
        hint: "视图",
        keys: "",
    },
    Action {
        id: "view-film",
        name: "胶片条",
        hint: "视图",
        keys: "",
    },
    Action {
        id: "view-rulers",
        name: "标尺",
        hint: "视图",
        keys: "",
    },
    Action {
        id: "file-export-frame",
        name: "导出当前帧",
        hint: "文件",
        keys: "",
    },
    Action {
        id: "win-look",
        name: "外观与布局",
        hint: "窗口",
        keys: "",
    },
    Action {
        id: "win-plugins",
        name: "插件",
        hint: "窗口",
        keys: "",
    },
    // 编辑与文件这几条的分发早就在 `run` 里了，表里却一直缺——等于只接线没接灯：
    // 分支能跑，面板里搜不到。补上之后 23 条动作才与 `run` 一一对应。
    //
    // The dispatch for these edit/file entries has been in `run` for a while but the table
    // never listed them -- wired but no light: the branches ran, the palette could not find
    // them. With these, all 23 entries line up with `run`.
    Action {
        id: "edit-undo",
        name: "撤销",
        hint: "编辑",
        keys: "Ctrl+Z",
    },
    Action {
        id: "edit-redo",
        name: "重做",
        hint: "编辑",
        keys: "Ctrl+Shift+Z",
    },
    Action {
        id: "edit-reset-all",
        name: "全部复位",
        hint: "编辑",
        keys: "",
    },
    Action {
        id: "file-import",
        name: "导入照片…",
        hint: "文件",
        keys: "Ctrl+I",
    },
    Action {
        id: "file-export",
        name: "导出…",
        hint: "文件",
        keys: "Ctrl+E",
    },
];

/// 命中过滤：中文按子串，英文按动作 id 的子串，另外允许匹配分类名。
///
/// Matching: substring on the Chinese name, on the action id (Latin), and on the category.
fn matched(a: &Action, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let q = q.to_lowercase();
    crate::i18n::tr(a.name).to_lowercase().contains(&q)
        || a.id.contains(&q)
        || crate::i18n::tr(a.hint).to_lowercase().contains(&q)
        || a.keys.to_lowercase().contains(&q)
}

/// 执行一条动作。返回 `true` 表示要关闭面板。
///
/// Run one action. Returns `true` when the palette should close.
fn run(state: &mut AppState, ctx: &Context, id: &str) -> bool {
    use crate::state::Module;
    match id {
        // 工作区：一次把右栏宽、胶片条、密度摆好并切模块。排在模块之前，
        // 因为"接下来在什么布局里干活"比"去哪个页面"高一级。
        //
        // Workspaces: dock width, filmstrip and density in one go, plus the module. Listed
        // before the module switches because "what layout am I working in" is a level above
        // "which page".
        "ws-sequence" => state.apply_workspace(crate::layout::Workspace::Sequence),
        "ws-develop" => state.apply_workspace(crate::layout::Workspace::Develop),
        "ws-render" => state.apply_workspace(crate::layout::Workspace::Render),
        "mod-seq" => state.module = Module::Sequence,
        "mod-deflicker" => state.module = Module::Deflicker,
        "mod-develop" => state.module = Module::Develop,
        "mod-frame" => state.module = Module::Frame,
        "mod-render" => state.module = Module::Render,
        "mod-audio" => state.module = Module::Audio,
        "mod-merge" => state.module = Module::Merge,
        "view-split" => state.split = !state.split,
        "view-grid" => state.grid = !state.grid,
        "view-curves" => state.show_curves = !state.show_curves,
        "view-film" => state.show_film = !state.show_film,
        "view-rulers" => state.rulers = !state.rulers,
        "file-export-frame" => state.export_current(ctx),
        // 四个"缺稳定调用点"的动作，现在都接到公开入口上：
        // 撤销/重做走命令总线 `run_ui_command`（菜单与页签用的是同一条），
        // 全部复位走 `reset_params()`（显影面板那颗按钮调的就是它），
        // 导入走 `open_photos_dialog`，导出走 `run_export`。
        //
        // The four actions that "had no stable call site" now bind to public entries:
        // undo/redo go through the command bus `run_ui_command` (the same one the menu and
        // tabs use), reset-all through `reset_params()` (what the Develop button calls),
        // import through `open_photos_dialog`, export through `run_export`.
        "edit-undo" => {
            let _ = state.run_ui_command(ctx, "history.undo", &serde_json::json!({}));
        }
        "edit-redo" => {
            let _ = state.run_ui_command(ctx, "history.redo", &serde_json::json!({}));
        }
        "edit-reset-all" => state.reset_params(),
        "file-import" => super::dialogs::open_photos_dialog(state, ctx),
        "file-export" => state.run_export(ctx),
        "win-look" => state.look_open = true,
        "win-plugins" => state.market_open = true,
        _ => return false,
    }
    true
}

/// 画命令面板。放在帧尾、错误弹窗之前：它比普通浮窗更靠上，但让错误提示压住它。
///
/// Draws the palette. Called near the frame's end, before the error dialog: it sits above
/// ordinary windows, while an error still covers it.
pub(super) fn command_palette(state: &mut AppState, ctx: &Context) {
    // Ctrl/Cmd+K 开合。`consume_key` 保证这一帧不会被别的控件重复消费。
    //
    // Ctrl/Cmd+K toggles. `consume_key` stops any other widget from consuming it this frame.
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
        state.palette_open = !state.palette_open;
        state.palette_query.clear();
        state.palette_sel = 0;
        state.palette_fresh = true;
    }
    // 调试钩子：`AEVUM_PALETTE=1` 启动即展开一次，用来给面板出证据图
    // （`--shot` 没法按键，和 `AEVUM_DBG_RECT` 同类用途）。
    //
    // Debug hook: `AEVUM_PALETTE=1` opens it once at startup so the panel can be captured
    // (`--shot` cannot press keys; same purpose as `AEVUM_DBG_RECT`).
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static POKED: AtomicBool = AtomicBool::new(false);
        // `swap` 一次就够：既标记"已处理过"，又拿到"这是不是第一帧"。
        //
        // A single `swap` both marks it handled and tells us whether this is the first frame.
        let first_frame = !POKED.swap(true, Ordering::Relaxed);
        if first_frame && std::env::var_os("AEVUM_PALETTE").is_some() && !state.palette_open {
            state.palette_open = true;
            state.palette_fresh = true;
        }
    }
    if !state.palette_open {
        return;
    }

    // 命中列表每帧现算：15 条的规模不值得缓存，缓存反而要维护失效条件。
    //
    // The hit list is computed every frame: fifteen rows do not justify a cache, and a
    // cache would need invalidation rules.
    let hits: Vec<&Action> = ACTIONS
        .iter()
        .filter(|a| matched(a, &state.palette_query))
        .collect();
    let n = hits.len();
    if n == 0 {
        state.palette_sel = 0;
    } else if state.palette_sel >= n {
        state.palette_sel = n - 1;
    }

    // 键盘导航在这里消费，避免"输入框吃掉方向键"与"列表跟着滚"打架。
    //
    // Keyboard navigation is consumed here so the text field and the list do not fight
    // over the arrow keys.
    if n > 0 {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
            state.palette_sel = (state.palette_sel + 1) % n;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
            state.palette_sel = (state.palette_sel + n - 1) % n;
        }
    }

    let mut close = false;
    let mut fire: Option<&'static str> = None;

    let modal = egui::Modal::new(egui::Id::unique("aevum-cmd-palette")).show(ctx, |ui| {
        super::windows::modal_box(ui, MODAL_W_WIDE);
        ui.horizontal(|ui| {
            // 放大镜用自绘线段，不赌字体里有没有图标。
            //
            // The magnifier is drawn from line segments so no icon font is assumed.
            let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            let p = ui.painter();
            let c = r.center() + Vec2::new(-1.0, -1.0);
            p.circle_stroke(c, 4.2, egui::Stroke::new(1.3, muted()));
            p.line_segment(
                [c + Vec2::new(3.2, 3.2), c + Vec2::new(6.4, 6.4)],
                egui::Stroke::new(1.3, muted()),
            );
            let edit = egui::TextEdit::singleline(&mut state.palette_query)
                .hint_text(crate::i18n::t!("搜索动作、模块或快捷键…"))
                .desired_width(f32::INFINITY)
                .font(egui::FontId::proportional(SIZE_BODY));
            let resp = ui.add(edit);
            if state.palette_fresh {
                resp.request_focus();
                state.palette_fresh = false;
            }
            // 回车执行：输入框有焦点时回车也会走到这里，所以只认一次。
            //
            // Enter runs the selection; the text field reports it too, so consume once.
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && n > 0 {
                fire = Some(hits[state.palette_sel].id);
            }
        });
        ui.add_space(8.0);
        // 一条通栏发丝线：面板内部只靠明度分层会显得糊，一条 1px 线就够。
        // A full-width hairline: brightness alone reads muddy inside a floating panel.
        let (line, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(
            line.x_range(),
            line.center().y,
            egui::Stroke::new(1.0, divider()),
        );
        ui.add_space(4.0);

        if n == 0 {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(crate::i18n::t!("没有匹配的动作"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
            ui.add_space(10.0);
        }

        // 行高 30：单手能扫、又不会让十条撑成两屏。可见十条，其余靠方向键滚。
        //
        // 30 px rows: scannable, and ten of them still fit; the rest are reached by
        // arrow keys.
        const ROW_H: f32 = 30.0;
        const VISIBLE: usize = 10;
        // 可见窗口跟着选中项滚：只取前十条时，方向键走到第 11 条高亮会"消失"，
        // 看起来像按坏了。窗口始终包含选中项，且不越过列表尾部。
        //
        // The visible window follows the selection: taking always the first ten rows made
        // the highlight vanish once the arrow keys passed row ten, reading as a broken key.
        // The window always contains the selection and never overruns the list.
        let start = state
            .palette_sel
            .saturating_sub(VISIBLE - 1)
            .min(n.saturating_sub(VISIBLE));
        for (i, a) in hits.iter().enumerate().skip(start).take(VISIBLE) {
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
            let on = i == state.palette_sel;
            let p = ui.painter();
            if on {
                p.rect_filled(
                    rect.shrink2(Vec2::new(0.0, 1.0)),
                    radius_control(),
                    accent_dim(),
                );
            } else if resp.hovered() {
                p.rect_filled(
                    rect.shrink2(Vec2::new(0.0, 1.0)),
                    radius_control(),
                    elev_hover(),
                );
            }
            p.text(
                Pos2::new(rect.left() + 10.0, rect.center().y),
                Align2::LEFT_CENTER,
                crate::i18n::tr(a.name),
                egui::FontId::proportional(SIZE_BODY),
                if on { text() } else { muted() },
            );
            // 分类靠右、快捷键最右：三级信息都用同一行，扫读时右缘对齐。
            //
            // Category right-aligned, shortcut at the far right: three levels of detail on
            // one line, flush to the right edge.
            let mut x = rect.right() - 10.0;
            if !a.keys.is_empty() {
                let g = p.layout_no_wrap(a.keys.to_owned(), egui::FontId::monospace(MICRO), dim());
                x -= g.size().x;
                p.galley(Pos2::new(x, rect.center().y - g.size().y * 0.5), g, dim());
                x -= 12.0;
            }
            let g = p.layout_no_wrap(
                crate::i18n::tr(a.hint).to_owned(),
                egui::FontId::proportional(MICRO),
                dim(),
            );
            x -= g.size().x;
            p.galley(Pos2::new(x, rect.center().y - g.size().y * 0.5), g, dim());

            if resp.hovered() {
                state.palette_sel = i;
            }
            if resp.clicked() {
                fire = Some(a.id);
            }
        }

        ui.add_space(6.0);
        let (line, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(
            line.x_range(),
            line.center().y,
            egui::Stroke::new(1.0, divider()),
        );
        ui.add_space(4.0);
        // 底部提示：把三个键写出来。面板是"给不知道的人用的"，不能假设他们会按 F1。
        //
        // Footer hint spells out the three keys: the palette is for people who do not know
        // their way around, so nothing may be assumed.
        ui.label(
            egui::RichText::new(crate::i18n::t!("↑↓ 选择 · 回车执行 · Esc 关闭"))
                .size(MICRO)
                .color(dim()),
        );
    });

    if modal.should_close() {
        close = true;
    }
    // 合并成 let-chain：动作只在真的被选中时执行一次，读起来也少一层缩进。
    //
    // Collapsed into a let-chain: the action runs exactly once, only when one was chosen.
    if let Some(id) = fire
        && run(state, ctx, id)
    {
        close = true;
    }
    if close {
        state.palette_open = false;
        state.palette_query.clear();
        state.palette_sel = 0;
    }
}
