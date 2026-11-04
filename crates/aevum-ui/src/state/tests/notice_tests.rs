use super::*;

/// 提示队列是全进程共享的：两个用例并行跑会互相抢名额，串起来跑。
/// 锁放在 `env`（队列旁边），跑完整界面帧的测试也用同一把——见 env.rs。
fn lock() -> std::sync::MutexGuard<'static, ()> {
    aevum_core::env::lock_notes_for_test()
}

/// 每条用例一个唯一前缀，从共享队列里按前缀捞回自己的那几条。
fn unique(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("aevum-ui-notice-{tag}-{}-{nanos}", std::process::id())
}

/// `env::note` 压下的提示要按原顺序搬进 `state.notices`，同一句只留一条。
#[test]
fn drain_notices_moves_notes_in_order_and_dedupes() {
    let _serial = lock();
    let tag = unique("order");
    let _ = aevum_core::env::drain_notes();

    aevum_core::env::note(format!("{tag}-一"));
    aevum_core::env::note(format!("{tag}-二"));
    aevum_core::env::note(format!("{tag}-一"));

    let mut state = AppState::new();
    state.drain_notices();

    let got: Vec<&String> = state.notices.iter().filter(|m| m.contains(&tag)).collect();
    assert_eq!(got.len(), 2, "同一句只该留一条：{got:?}");
    assert!(got[0].ends_with("一"), "顺序必须保住：{got:?}");
    assert!(got[1].ends_with("二"), "顺序必须保住：{got:?}");
}

/// 上限 8 条，旧的挤掉：刚触发的问题必须留得住，否则用户看不到自己这件事。
#[test]
fn drain_notices_caps_at_eight_keeping_the_newest() {
    let _serial = lock();
    let tag = unique("cap");
    let _ = aevum_core::env::drain_notes();

    for i in 0..10 {
        aevum_core::env::note(format!("{tag}-{i}"));
    }

    let mut state = AppState::new();
    state.drain_notices();

    let got: Vec<&String> = state.notices.iter().filter(|m| m.contains(&tag)).collect();
    assert_eq!(got.len(), NOTICE_LIMIT, "上限是 8 条：{got:?}");
    assert!(got[0].ends_with("-2"), "最旧的两条该被挤掉：{got:?}");
    assert!(
        got[NOTICE_LIMIT - 1].ends_with("-9"),
        "最新的一条必须留着：{got:?}"
    );
}

/// 无告警时什么也不加：`--selftest` 的形状计数靠这条（状态栏那格不许建形状）。
#[test]
fn drain_notices_adds_nothing_when_queue_is_empty() {
    let _serial = lock();
    let _ = aevum_core::env::drain_notes();
    let mut state = AppState::new();
    state.drain_notices();
    assert!(state.notices.is_empty(), "没有告警时状态栏那格必须不存在");
}
