use super::*;

fn dark_frame(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aevum-ui-auto-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join("dark.png");
    image::RgbImage::from_pixel(64, 48, image::Rgb([34, 34, 34]))
        .save(&path)
        .expect("写测试帧");
    path
}

fn state_on(path: &PathBuf) -> AppState {
    let mut state = AppState::new();
    // 无显示环境不启动读盘线程：测光代理退回前台同步解码，判据才可当场断言。
    //
    // A display-less environment starts no disk threads: the metering proxy falls back to
    // the synchronous foreground decode so the assertions can check results on the spot.
    state.headless = true;
    state.seq = Some(Sequence::open_files(std::slice::from_ref(path)).expect("开单帧"));
    state.selected = 0;
    state
}

#[test]
fn auto_writes_absolute_values_and_clicking_twice_is_stable() {
    let path = dark_frame("stable");
    let mut state = state_on(&path);
    state.params.exposure = 2.5;
    state.auto_exposure(&Context::default());
    assert!(
        state.params.exposure > 0.3,
        "暗图该提亮：{state:?}",
        state = state.params.exposure
    );
    let first = (
        state.params.exposure,
        state.params.contrast,
        state.params.highlights,
        state.params.shadows,
        state.params.whites,
        state.params.blacks,
        state.params.temperature,
        state.params.tint,
    );
    state.auto_exposure(&Context::default());
    let second = (
        state.params.exposure,
        state.params.contrast,
        state.params.highlights,
        state.params.shadows,
        state.params.whites,
        state.params.blacks,
        state.params.temperature,
        state.params.tint,
    );
    assert_eq!(first, second, "重复点自动只能在原值上写回同一组绝对值");
    assert!(state.auto_note.as_deref().is_some_and(|n| n.contains("EV")));
}

#[test]
fn auto_does_not_add_manual_exposure() {
    let path = dark_frame("absolute");
    let mut clean = state_on(&path);
    clean.auto_exposure(&Context::default());
    let mut tweaked = state_on(&path);
    tweaked.params.exposure = 3.0;
    tweaked.auto_exposure(&Context::default());
    assert_eq!(clean.params.exposure, tweaked.params.exposure);
}

/// 后台探针防抖：同一帧的在飞请求不重复发，状态栏挂「测光中」而不是空转。
///
/// Debounce of the background probe: a repeat request for the frame already in flight is
/// suppressed, and the status line reads "metering…" instead of spinning idle.
#[test]
fn auto_probe_debounces_inflight_requests() {
    let path = dark_frame("debounce");
    let mut state = AppState::new();
    state.seq = Some(Sequence::open_files(std::slice::from_ref(&path)).expect("开单帧"));
    state.selected = 0;
    assert!(state.auto_probe_inflight.is_none());
    state.auto_exposure(&Context::default());
    assert!(
        state.auto_probe_inflight.is_some(),
        "首次点击要发出后台探针"
    );
    assert!(
        state
            .auto_note
            .as_deref()
            .is_some_and(|n| n.contains("测光中")),
        "探针期间要有「测光中…」的提示，收到 {auto_note:?}",
        auto_note = state.auto_note
    );
    state.auto_exposure(&Context::default());
    assert!(
        state.auto_probe_inflight.is_some(),
        "同帧第二次点击不得再发探针"
    );
}
