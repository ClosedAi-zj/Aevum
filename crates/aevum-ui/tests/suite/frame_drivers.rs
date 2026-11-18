//! P0 回归判据：帧尾那两条**每帧驱动**（撤销落账、错误弹窗）不许再丢。
//!
//! # 这条判据守的是什么
//!
//! `crates/aevum-ui/src/shell/mod.rs::ui()` 的帧尾挂着三条驱动，它们是「状态变了
//! 界面上却看不见」这类断线的唯一出口：
//!
//! 1. `state.history_commit();` —— 把这一帧的改动记进撤销历史。没有它，
//!    `history.steps()` 永远只有一条（或零条），`can_undo()` 恒 false，
//!    撤销/重做按钮与历史面板全哑。
//! 2. `state.sync_error_dialog();` + `windows::error_dialog(...)` —— 把
//!    `state.error` 抬成模态框并画出来。没有它，四十余处 `state.error = Some(...)`
//!    只在状态栏滑过一行，用户看不到「刚才那步失败了」。
//! 3. `state.ensure_preview(&ctx);` —— 预览调度（见 `preview_wiring.rs`，不在本文件）。
//!
//! v21 换壳（`b963631`）把前两条连同帧首的 `history.begin_frame()` 一起漏搬，
//! 生产调用点归零；源码里函数还在，界面上功能已经死了——只有跑起来看才分得开。
//!
//! # 为什么撤销判据要走真帧
//!
//! 直接在测试里调 `state.history_commit()` 只能证明**函数本身**没坏，证明不了
//! 「有人每帧在调它」——那正是这次事故的形状。所以主判据画真帧（`shell::ui`）：
//! 把 `history_commit()` 注释掉，步数停在 0，判据必红。
//!
//! 合并那条反而必须直调：它考的是「每帧一次 commit 会不会把历史灌爆」，
//! 需要精确控制「改一次 → commit 一次」的节奏，画整帧反而掺进别的改动。
//!
//! # 变异自证
//!
//! 注释掉 `shell/mod.rs` 帧尾的 `state.history_commit();` →
//! `undo_records_step_and_reverts` 红；注释掉 `windows::error_dialog(state, &ctx);`
//! → `error_surfaces_as_dialog` 红。两条都逐字还原后全绿，`git diff` 干净。
//!
//! # P0 regression: the two per-frame tail drivers must stay wired
//!
//! The tail of `shell/mod.rs::ui()` carries three drivers, the only exit for "the
//! state changed but the screen never showed it": (1) `state.history_commit();` records
//! this frame's edit into the undo history -- without it `steps()` never grows,
//! `can_undo()` is always false and the whole undo/redo + history panel surface is
//! dead; (2) `state.sync_error_dialog();` plus `windows::error_dialog(...)` promote
//! `state.error` to a modal -- without them the forty-odd `state.error = Some(...)`
//! sites only flash a status-bar line; (3) `state.ensure_preview(&ctx);` (covered by
//! `preview_wiring.rs`). The v21 shell swap (`b963631`) dropped the first two together
//! with the frame-head `history.begin_frame()`, taking the production call count to
//! zero: the functions still existed, the features were already dead. The undo
//! criterion therefore paints real frames -- commenting the call out leaves the step
//! count at zero and turns the test red. The merge criterion calls `history_commit`
//! directly instead, because it measures "does one commit per frame flood the
//! history", which needs an exact "edit once, commit once" rhythm.

use aevum_ui::i18n::{self, Lang};
use aevum_ui::state::{AppState, Module};
use aevum_ui::{shell, theme};

use egui::epaint::ClippedShape;
use egui::{Rect, Shape};

/// 与 `--selftest` / `preview_wiring` 同一块画布：1600×1000。
/// Same 1600×1000 canvas as the selftest and `preview_wiring`.
fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        max_texture_side: Some(8192),
        ..Default::default()
    }
}

/// 建模一个 Context：token 应用后画帧，避免主题差异污染颜色判据。
/// Build a themed context so colour assertions see the production palette.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::apply(&ctx, 1.0);
    ctx
}

/// 画一帧（纹理增量丢掉：这些判据只看状态与 shape）。
/// Paint exactly one frame, dropping the texture delta.
fn frame(ctx: &egui::Context, state: &mut AppState) {
    let mut out = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
    out.textures_delta.clear();
}

/// 三帧预热后取稳态帧的 shape：首帧还没有 galley 宽度，写字位置会漂。
///
/// 与 `empty_state` / `preview_wiring` 同一口径：预热帧不计入断言。
///
/// Paint to a settled frame and hand back the shapes. Three warm-ups resolve the
/// galley widths, exactly like `empty_state` and `preview_wiring`; the warm-up frames
/// are never asserted on.
fn paint(ctx: &egui::Context, state: &mut AppState) -> Vec<ClippedShape> {
    for _ in 0..3 {
        frame(ctx, state);
    }
    ctx.run_ui(raw_input(), |ui| shell::ui(state, ui)).shapes
}

/// 一个可画帧的状态：显影井 + 不落盘（判据不该写用户的偏好文件）。
/// A paintable state: develop well, no settings writes (a criterion must never
/// touch the user's preference file).
fn state() -> AppState {
    let mut state = AppState::new();
    // 显影井：撤销与错误弹窗都是用户在这一屏上遇到的。
    // Develop well: both the undo chain and the error dialog are met on this screen.
    state.module = Module::Develop;
    // 与 `--shot` 通道同一口径：只拦落盘，不碰别的。
    // Same as the `--shot` path: blocks persistence and nothing else.
    state.shot_mode = true;
    state
}

/// 递归收集文本 shape 里的字。
/// Collect every text string from the shapes.
fn texts(shapes: &[ClippedShape]) -> Vec<String> {
    let mut out = Vec::new();
    for cs in shapes {
        walk_text(&cs.shape, &mut out);
    }
    out
}

fn walk_text(shape: &Shape, out: &mut Vec<String>) {
    match shape {
        Shape::Text(t) => {
            let txt = t.galley.text().to_string();
            if !txt.trim().is_empty() {
                out.push(txt);
            }
        }
        Shape::Vec(v) => {
            for s in v {
                walk_text(s, out);
            }
        }
        _ => {}
    }
}

/// **判据一（主）**：改一个参数 → 画真帧 → 历史多一步、可以撤销、撤销后值回来。
///
/// 全程走 `shell::ui`，不直调 `history_commit`：这条判据钉的是**接线**，
/// 不是函数体。注释掉帧尾那一句，`steps()` 停在 0，第一段断言当场红。
///
/// The primary criterion: change one parameter, paint a real frame, and the history
/// must gain a step, become undoable, and restore the old value on undo. Everything
/// goes through `shell::ui` instead of calling `history_commit` directly, because
/// this criterion pins the **wire**, not the function body.
#[test]
fn undo_records_step_and_reverts() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let ctx = context();
    let mut state = state();

    // 第一帧把基线记进历史：从零步长到一步「初始状态」，但还不能撤销。
    // The first frame records the baseline: zero steps grow to one "initial state",
    // which is not yet undoable.
    frame(&ctx, &mut state);
    assert_eq!(
        state.history.steps().len(),
        1,
        "帧尾 `history_commit` 没跑：画完第一帧历史应有 1 条基线，实际 {} 条",
        state.history.steps().len()
    );
    assert!(
        !state.history.can_undo(),
        "只有基线一步时 `can_undo()` 必须为 false"
    );

    let before = state.params.exposure;
    state.params.exposure = 0.5;
    frame(&ctx, &mut state);

    let steps = state.history.steps().len();
    assert!(
        steps >= 2,
        "改了曝光后画一帧，历史没有多出一步（仍 {steps} 条）——`shell::ui` 帧尾的 \
         `state.history_commit();` 又断了，撤销/重做与历史面板会全哑"
    );
    assert!(
        state.history.can_undo(),
        "历史有 {steps} 条但 `can_undo()` 为 false：光标没被推到新步上"
    );

    // 撤销走真实命令路径（与 demo 的撤销按钮同一条），不手搓 `history.undo()`。
    // Undo through the real command path, the same one the demo's undo button uses.
    state
        .run_ui_command(&ctx, "history.undo", &serde_json::json!({}))
        .unwrap_or_else(|e| panic!("history.undo 命令失败：{e}"));
    assert!(
        (state.params.exposure - before).abs() < 1e-6,
        "撤销后曝光应回到 {before}，实际 {}——历史里的快照没被回放",
        state.params.exposure
    );
    assert!(!state.history.can_undo(), "撤回到基线后不应还能再撤");
}

/// **判据一（合并）**：连改 10 次小步 + 每次 commit，步数必须远小于 10。
///
/// 这是「每帧落账」的代价测试：`history_commit` 设计上每帧调用，靠
/// `Step::cat` 把同帧距的同类改动并成一步。合并一坏，拖一次滑块就灌几十行，
/// 历史面板等于废掉——所以这条与上一条同样重要，只是**必须直调**：
/// 判据要精确控制「改一次 → commit 一次」的节奏，画整帧会掺进别的改动。
///
/// 顺带钉住 `history.begin_frame()`：合并判据里有 `frame > 0`，帧号不推进
/// （`begin_frame` 没被调）时会退化成「每次 commit 一步」，这条会红。
///
/// The merge criterion: ten small edits with one commit each must stay far below ten
/// steps. `history_commit` is designed to run every frame and relies on `Step::cat` to
/// fold same-class edits within the frame tolerance into one step; a broken merge
/// turns one slider drag into dozens of rows. It calls `history_commit` directly
/// because the rhythm must be exact, and it also pins `history.begin_frame()`: the
/// merge test requires `frame > 0`, so a frozen frame counter degrades to one step per
/// commit and this criterion goes red.
#[test]
fn history_merges_ten_small_edits() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let mut state = state();
    // 基线：先落一条，让后面的改动有可比的起点。
    // Baseline first, so later edits have something to compare against.
    state.history.begin_frame();
    state.history_commit();
    let base = state.history.steps().len();

    for i in 0..10 {
        // 每帧一次：帧号推进 + 改一点 + 落账，就是拖滑块时真发生的事。
        // One frame at a time: advance the frame, nudge the value, commit -- exactly
        // what a slider drag does.
        state.history.begin_frame();
        state.params.exposure = 0.01 * (i as f64 + 1.0);
        state.history_commit();
    }

    let steps = state.history.steps().len();
    assert!(
        steps <= base + 2,
        "10 次小步改动落了 {} 步（基线 {base}）：合并失效，拖一次滑块就会灌爆历史\
         ——检查 `history.begin_frame()` 是否还在帧首、`Step::cat` 合并是否被改坏",
        steps - base
    );
    assert!(
        state.history.can_undo(),
        "合并后也应至少留下一步可撤销的改动"
    );
}

/// **判据二**：`state.error` 一变，界面上必须真的出现弹窗；清掉后必须真的消失。
///
/// 三段：同步（`error_modal` 非空）→ 画出来（shapes 里有那段文案）→ 清掉后消失。
/// 第一段证明 `sync_error_dialog` 在跑，第二段证明 `error_dialog` 在跑——
/// 只补同步不补绘制，正是这次事故最容易犯的半个修复。
///
/// The error-dialog criterion, three legs: the sync (`error_modal` becomes non-empty),
/// the paint (the message really shows up in the shapes) and the dismissal (it is gone
/// once cleared). The first leg proves `sync_error_dialog` runs, the second proves
/// `error_dialog` runs -- syncing without painting is the half-fix this incident kept
/// inviting.
#[test]
fn error_surfaces_as_dialog() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let ctx = context();
    let mut state = state();
    const MSG: &str = "测试错误：导出路径不可写";

    // 先画一帧干净界面：把首帧那些一次性状态安稳下来。
    // Paint one clean frame first, settling the first-frame one-shots.
    paint(&ctx, &mut state);
    assert!(
        state.error_modal.is_none(),
        "没有错误时不该有弹窗（`error_modal` 应为 None）"
    );

    state.error = Some(MSG.to_owned());
    let shapes = paint(&ctx, &mut state);

    // 第一段：同步。
    // Leg 1: the sync.
    assert_eq!(
        state.error_modal.as_deref(),
        Some(MSG),
        "`state.error` 变了但 `error_modal` 没同步——帧尾的 `state.sync_error_dialog();` 断了"
    );
    // 第二段：真的画出来了。
    // Leg 2: it is really painted.
    let all = texts(&shapes);
    assert!(
        all.iter().any(|t| t.contains(MSG)),
        "错误弹窗没画出来：本帧 {} 条文本里找不到「{MSG}」——帧尾的 \
         `windows::error_dialog(state, &ctx);` 断了。文本样本：{all:?}",
        all.len()
    );

    // 第三段：清掉之后消失（弹窗由状态驱动，不是粘在屏上的浮层）。
    // Leg 3: gone once cleared -- the dialog is state-driven, not a stuck overlay.
    state.error_modal = None;
    state.error = None;
    let shapes = paint(&ctx, &mut state);
    let all = texts(&shapes);
    assert!(
        !all.iter().any(|t| t.contains(MSG)),
        "关掉弹窗后画帧仍在屏上找到「{MSG}」：`error_modal` 清空了但绘制没跟上"
    );
    assert!(
        state.error_modal.is_none(),
        "没有新错误时 `sync_error_dialog` 不该把旧弹窗抬回来"
    );
}
