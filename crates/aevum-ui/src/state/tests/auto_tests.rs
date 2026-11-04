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
    state.seq = Some(Sequence::open_files(std::slice::from_ref(path)).expect("开单帧"));
    state.selected = 0;
    state
}

#[test]
fn auto_writes_absolute_values_and_clicking_twice_is_stable() {
    let path = dark_frame("stable");
    let mut state = state_on(&path);
    state.params.exposure = 2.5;
    state.auto_exposure();
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
    state.auto_exposure();
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
    clean.auto_exposure();
    let mut tweaked = state_on(&path);
    tweaked.params.exposure = 3.0;
    tweaked.auto_exposure();
    assert_eq!(clean.params.exposure, tweaked.params.exposure);
}
