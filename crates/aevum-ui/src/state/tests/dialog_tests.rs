//! 对话框中枢的队列与分发判据。真对话框绝不在这里弹——测试只压队列、
//! 直接注入结果。

use crate::state::AppState;
use crate::state::ops_dialog::{DialogMode, DialogPick, DialogPurpose};
use egui::Context;
use std::path::PathBuf;

#[test]
fn requests_queue_up_and_one_flies_at_a_time() {
    let mut state = AppState::new();
    state.headless = false;
    let ctx = Context::default();

    state.request_dialog(
        DialogPurpose::RenderOutDir,
        DialogMode::Folder,
        None,
        None,
        None,
    );
    state.request_dialog(
        DialogPurpose::RenderOutDir,
        DialogMode::Folder,
        None,
        None,
        None,
    );
    assert_eq!(state.dialog_queue.len(), 2, "两个请求都该排队");

    state.spawn_pending_dialog(&ctx);
    assert_eq!(state.dialog_queue.len(), 1, "只放行一个");
    assert!(state.dialog_in_flight, "在弹标记必须立起");
    // 在弹期间再来一个请求：继续排队，不并发弹两扇。
    state.request_dialog(
        DialogPurpose::RenderOutDir,
        DialogMode::Folder,
        None,
        None,
        None,
    );
    assert_eq!(state.dialog_queue.len(), 2);
}

#[test]
fn replies_land_on_state_and_release_the_slot() {
    let mut state = AppState::new();
    state.headless = false;
    let ctx = Context::default();
    state.request_dialog(
        DialogPurpose::RenderOutDir,
        DialogMode::Folder,
        None,
        None,
        None,
    );
    state.spawn_pending_dialog(&ctx);
    assert!(state.dialog_in_flight);

    state
        .dialog_tx
        .send((
            DialogPurpose::RenderOutDir,
            Some(DialogPick::Folder(PathBuf::from("E:/out"))),
        ))
        .expect("发结果");
    state.drain_dialog_replies(&ctx);
    assert_eq!(
        state.render_out_dir,
        PathBuf::from("E:/out"),
        "选目录的结果没落到 state"
    );
    assert!(!state.dialog_in_flight, "收完账要放行下一个");
    // 取消（None）：什么都不做，但同样放行。
    state
        .dialog_tx
        .send((DialogPurpose::RenderOutDir, None))
        .expect("发取消");
    state.drain_dialog_replies(&ctx);
    assert_eq!(state.render_out_dir, PathBuf::from("E:/out"));
    assert!(!state.dialog_in_flight);
}

#[test]
fn headless_never_queues_a_dialog() {
    // --shot / 自检把这一档打开；这里照抄它们的口径。
    //
    // --shot / selftest turn this on; the test mirrors exactly that mode.
    let mut state = AppState::new();
    state.headless = true;
    state.request_dialog(
        DialogPurpose::RenderOutDir,
        DialogMode::Folder,
        None,
        None,
        None,
    );
    assert!(
        state.dialog_queue.is_empty() && !state.dialog_in_flight,
        "--shot / 自检环境不许弹系统对话框"
    );
}
