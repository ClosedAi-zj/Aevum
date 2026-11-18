//! P0 回归判据：窗口缩放热区与告警状态栏——v21 换壳漏搬的两条每帧驱动。
//!
//! # 这两条判据守的是什么
//!
//! v21 换壳（`b963631`）把旧 `shell.rs::ui()` 里的两句搬丢了，函数体都还在，
//! **生产调用点归零**，于是界面上直接没有这两件事：
//!
//! 1. `chrome::window_resize(root, &ctx, full)` —— 无边框窗口的四边四角缩放热区。
//!    窗口是 `decorations = false` 自绘的，系统不给边框，八个隐形 `interact` 区
//!    就是**唯一的**缩放入口。丢了这个调用，用户拖窗口边缘完全没反应——这是
//!    「窗口拖边缘改不了大小」的根因。`--selftest` 抓不到它（热区是隐形的，形状
//!    计数不变），只有把指针真的移动过去、真的按下去才分得开。
//! 2. `state.drain_notices()` —— 把 aevum-core 压下的诊断队列搬进 `state.notices`，
//!    再由状态栏画出来。丢了这个调用，护带目录不可写 / 设置文件只读这类事实
//!    永远进不了状态栏，**写盘失败全程静默**。
//!
//! # 为什么两条都要运行期取证
//!
//! 「函数写在源码里但没人调」与「写在源码里有人调」的源码文本一模一样，静态扫描
//! 分不开。所以这里画真帧（`shell::ui`）：
//!
//! - 缩放：指针移到右边缘 → 断言本帧输出给系统的光标是 `ResizeHorizontal`；
//!   再在同一位置按下 → 断言本帧真的发出了 `ViewportCommand::BeginResize(East)`。
//!   后者是「交给系统去拖」的那一步，没有它光标变了也拖不动。八条边角逐个走一遍。
//! - 告警：`env::note` 压一条 → 画真帧 → 断言状态栏那 20px 里出现这条文案；
//!   点掉（清空 `state.notices`）后再画一帧 → 断言文案消失。
//!
//! # 变异自证
//!
//! - 注释掉 `shell/mod.rs` 尾部 `chrome::window_resize(root, &ctx, full);` →
//!   `window_edges_are_draggable` 红（光标恒 `Default`、命令为空）；
//! - 注释掉帧首 `state.drain_notices();` → `notices_reach_the_status_bar` 红
//!   （文案压根没进状态栏）；只注释掉状态栏挂载那段同样红（进了 `notices`
//!   但没人画）。三条都逐字还原后全绿。
//!
//! # P0 regression: the resize handles and the notice bar must stay wired
//!
//! The v21 swap dropped two calls from the legacy `shell::ui()`. Both function
//! bodies still existed, so the source looked identical while the features were
//! gone: (1) `chrome::window_resize` -- the borderless window is self-drawn, so
//! these eight invisible `interact` zones are the **only** way to resize it; drop
//! the call and dragging the window edge does nothing; (2) `state.drain_notices()`
//! -- without it the diagnostics queue never reaches `state.notices`, so "the
//! sidecar folder is not writable" never shows and write failures stay silent.
//! Both are pinned at runtime: the cursor icon and the `BeginResize` viewport
//! command for the handles, and the real status-bar text for the notice.

use aevum_ui::i18n::{self, Lang};
use aevum_ui::state::{AppState, Module};
use aevum_ui::{shell, theme};

use egui::epaint::ClippedShape;
use egui::{CursorIcon, Event, PointerButton, Pos2, Rect, Shape, ViewportCommand};

/// 判据画布 1600×1000：右边缘 = x 1600，底边 = y 1000。
/// The criterion canvas: 1600x1000, so right edge = 1600 and bottom edge = 1000.
const W: f32 = 1600.0;
const H: f32 = 1000.0;

/// 与 `--selftest` / `preview_wiring` / `frame_drivers` 同一块画布。
/// Same 1600x1000 canvas as the selftest and the other P0 criteria.
fn raw_input(events: Vec<Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(W, H))),
        max_texture_side: Some(8192),
        events,
        ..Default::default()
    }
}

/// 建模一个 Context：token 应用后画帧，避免主题差异污染判据。
/// Build a themed context so colour assertions see the production palette.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::apply(&ctx, 1.0);
    ctx
}

/// 一个可画帧的状态：显影井 + 不落盘（判据不该写用户的偏好文件）。
/// A paintable state: develop well, no settings writes.
fn state() -> AppState {
    let mut state = AppState::new();
    state.module = Module::Develop;
    state.shot_mode = true;
    state
}

/// 画一帧，带上给定的事件序列，返回本帧的完整输出。
/// Paint one frame with the given events and hand back the full output.
fn run(ctx: &egui::Context, state: &mut AppState, events: Vec<Event>) -> egui::FullOutput {
    let mut out = ctx.run_ui(raw_input(events), |ui| shell::ui(state, ui));
    out.textures_delta.clear();
    out
}

/// 预热三帧：首帧还没有上一帧的控件矩形，命中测试无从谈起。
///
/// egui 的交互命中用的是**上一帧**登记的控件矩形；指针按下那一刻要能命中热区，
/// 必须先把热区画过至少一帧。三条预热帧与 `frame_drivers` / `empty_state` 同口径。
///
/// Three warm-up frames: egui hit-tests against the **previous** frame's widget
/// rects, so the handles must have been painted once before a press can land on
/// them. Same convention as `frame_drivers` and `empty_state`.
fn warm(ctx: &egui::Context, state: &mut AppState) {
    for _ in 0..3 {
        run(ctx, state, Vec::new());
    }
}

/// 本帧发给系统的缩放命令（八个热区里，按下哪一条就发哪一条）。
/// The resize commands this frame handed to the system.
fn resize_commands(out: &egui::FullOutput) -> Vec<egui::ResizeDirection> {
    out.viewport_output
        .values()
        .flat_map(|v| v.commands.iter())
        .filter_map(|c| match c {
            ViewportCommand::BeginResize(dir) => Some(*dir),
            _ => None,
        })
        .collect()
}

/// 递归收集文本 shape 里的字与它的实心矩形。
/// Collect every text string together with its placed rect.
fn texts(shapes: &[ClippedShape]) -> Vec<(String, Rect)> {
    let mut out = Vec::new();
    for cs in shapes {
        walk_text(&cs.shape, &mut out);
    }
    out
}

fn walk_text(shape: &Shape, out: &mut Vec<(String, Rect)>) {
    match shape {
        Shape::Text(t) => {
            let txt = t.galley.text().to_string();
            if !txt.trim().is_empty() {
                out.push((txt, t.galley.rect.translate(t.pos.to_vec2())));
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

/// 无边框窗口的八个热区：位置（判据画布坐标）、期望光标、按下后应发出的方向。
///
/// 取值就是 `chrome::window_resize` 自己的几何：角 12px 见方、边 5px 宽。
///
/// The eight handles: probe position, expected cursor, expected resize direction.
/// The geometry is `chrome::window_resize`'s own -- 12 px corners, 5 px edges.
fn handles() -> [(&'static str, Pos2, CursorIcon, egui::ResizeDirection); 8] {
    [
        (
            "上边 North",
            Pos2::new(W / 2.0, 2.0),
            CursorIcon::ResizeVertical,
            egui::ResizeDirection::North,
        ),
        (
            "下边 South",
            Pos2::new(W / 2.0, H - 2.0),
            CursorIcon::ResizeVertical,
            egui::ResizeDirection::South,
        ),
        (
            "左边 West",
            Pos2::new(2.0, H / 2.0),
            CursorIcon::ResizeHorizontal,
            egui::ResizeDirection::West,
        ),
        (
            "右边 East",
            Pos2::new(W - 2.0, H / 2.0),
            CursorIcon::ResizeHorizontal,
            egui::ResizeDirection::East,
        ),
        (
            "左上角 NorthWest",
            Pos2::new(2.0, 2.0),
            CursorIcon::ResizeNwSe,
            egui::ResizeDirection::NorthWest,
        ),
        (
            "右上角 NorthEast",
            Pos2::new(W - 2.0, 2.0),
            CursorIcon::ResizeNeSw,
            egui::ResizeDirection::NorthEast,
        ),
        (
            "左下角 SouthWest",
            Pos2::new(2.0, H - 2.0),
            CursorIcon::ResizeNeSw,
            egui::ResizeDirection::SouthWest,
        ),
        (
            "右下角 SouthEast",
            Pos2::new(W - 2.0, H - 2.0),
            CursorIcon::ResizeNwSe,
            egui::ResizeDirection::SouthEast,
        ),
    ]
}

/// **判据一**：窗口四边四角都能拖——悬停换缩放光标，按下就发 `BeginResize`。
///
/// 两段缺一不可：只有光标没有命令 = 看得见拖不动；只有命令没有光标 = 用户根本
/// 不知道该往哪儿按。八个热区逐个走一遍，任何一个没被 `shell::ui` 挂上都会红。
///
/// The handles criterion, two legs that must both hold: hovering swaps the cursor,
/// and pressing emits `BeginResize` (cursor without the command looks draggable but
/// is not; the command without the cursor is undiscoverable). All eight handles are
/// probed, so losing the mount turns this red on the first one.
#[test]
fn window_edges_are_draggable() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    let ctx = context();
    let mut state = state();
    // 指针先停在画布中央：别让上一帧的悬停状态污染第一条断言。
    // Park the pointer mid-canvas so no earlier hover leaks into the first probe.
    warm(&ctx, &mut state);
    run(
        &ctx,
        &mut state,
        vec![Event::PointerMoved(Pos2::new(W / 2.0, H / 2.0))],
    );

    for (name, pos, cursor, dir) in handles() {
        // 第一段：悬停 → 光标换成对应的缩放箭头。
        // Leg 1: hovering swaps to the matching resize cursor.
        let out = run(&ctx, &mut state, vec![Event::PointerMoved(pos)]);
        assert_eq!(
            out.platform_output.cursor_icon, cursor,
            "把指针挪到{name}（{pos:?}）时，光标应是 {cursor:?}，实际 {:?}\
             ——`shell::ui` 没挂 `chrome::window_resize`，窗口边缘是死的",
            out.platform_output.cursor_icon
        );

        // 第二段：按下 → 本帧必须把 `BeginResize` 交给系统。
        // Leg 2: pressing must hand `BeginResize` to the system this frame.
        let press = Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let out = run(&ctx, &mut state, vec![Event::PointerMoved(pos), press]);
        let cmds = resize_commands(&out);
        assert!(
            cmds.contains(&dir),
            "在{name}（{pos:?}）按下指针后本帧应发出 BeginResize({dir:?})，实际命令 {cmds:?}\
             ——热区只换了光标却没收账，窗口照样拖不动"
        );

        // 松手复位：下一个热区要从「没按着」的干净状态开始。
        // Release so the next handle starts from a clean, unpressed state.
        let release = Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        };
        run(&ctx, &mut state, vec![Event::PointerMoved(pos), release]);
    }
}

/// **判据二**：core 压下的告警必须真的出现在状态栏那一条里。
///
/// 三段：注入 `env::note` → 画真帧 → shapes 里能找到这段文案，且它落在状态栏那
/// 20px 高度内（不是浮在别处的 toast）→ 清空 `state.notices` 后再画一帧，文案消失
/// （状态栏由状态驱动，不是粘在屏上的字）。
///
/// The notice criterion, three legs: inject through `env::note`, paint a real frame
/// and find the text **inside the 20 px status row** (not a floating toast), then
/// clear `state.notices` and require it to be gone -- the bar is state-driven.
#[test]
fn notices_reach_the_status_bar() {
    // 告警队列是全进程共享的：与 `state/tests/notice_tests.rs` 同一把锁。
    // The queue is process-global: same lock as `state/tests/notice_tests.rs`.
    let _notes_guard = aevum_core::env::lock_notes_for_test();
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    // 起点干净：别人（或前一次跑）压下的句子不许混进这次断言。
    // Clean slate: nothing left over from another producer or an earlier run.
    let _ = aevum_core::env::drain_notes();
    // 文案压在 56 字以内：状态栏会按 `theme::ellipsis(…, 56)` 截断，太长就断在前面。
    // Keep it under 56 chars: the bar ellipsizes at 56.
    let msg = format!("测试告警：护带目录不可写 pid{}", std::process::id());
    aevum_core::env::note(msg.clone());

    let ctx = context();
    let mut state = state();
    warm(&ctx, &mut state);

    // 第一段：告警被搬到状态里（`drain_notices` 在跑）。
    // Leg 1: the notice landed in the state -- `drain_notices` is running.
    assert!(
        state.notices.iter().any(|m| m == &msg),
        "画了三帧真帧，`env::note` 压下的告警没进 `state.notices`（现为 {:?}）\
         ——帧首的 `state.drain_notices();` 断了",
        state.notices
    );

    // 第二段：状态栏真的把它画出来了，而且画在那 20px 里。
    // Leg 2: the bar paints it, inside the 20 px status row.
    let out = run(&ctx, &mut state, Vec::new());
    let rows = texts(&out.shapes);
    let hit = rows
        .iter()
        .find(|(t, _)| t.contains(&msg))
        .unwrap_or_else(|| {
            panic!(
                "告警进了状态但状态栏没画：本帧 {} 条文本里找不到「{msg}」\
             ——`shell::ui` 帧尾没有挂 `toolbar::status_bar`。文本样本：{:?}",
                rows.len(),
                rows.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>()
            )
        });
    assert!(
        hit.1.center().y > H - aevum_ui::shell::STATUS_BAR_H,
        "告警文案画在了 y={}，不在状态栏那 {}px 里（底边 y={H}）——状态栏被别的浮层顶掉了",
        hit.1.center().y,
        aevum_ui::shell::STATUS_BAR_H
    );

    // 第三段：点掉（清空）之后必须消失。
    // Leg 3: dismissed means gone.
    state.notices.clear();
    let out = run(&ctx, &mut state, Vec::new());
    let rows = texts(&out.shapes);
    assert!(
        !rows.iter().any(|(t, _)| t.contains(&msg)),
        "`state.notices` 清空了但状态栏还在画「{msg}」"
    );
}
