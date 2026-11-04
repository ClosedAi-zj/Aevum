//! 平台交付预设 → 界面字段的映射：点一下把五个字段写对，改任何一项高亮就熄。
//!
//! 为什么单独一条判据：core 那边已经证明「预设的每个数字都到了命令与像素上」
//! （`export_video::every_number_of_the_vertical_delivery_preset_reaches_the_render`），
//! 但那是 `RenderOptions` 的路径。面板走的是另一套字段（`output_w` / `codec` 字符串 /
//! `render_crf`…），映射写错（比如把宽写到 `output_h`）core 一个都抓不到，用户却会
//! 直接拿到错画幅。所以这里咬映射本身，以及「高亮不是装饰、是真的按数值反推」。
//!
//! Why a separate assertion: core already proves every number reaches the command line and
//! the pixels, but that is the `RenderOptions` path. The panel owns a different set of fields
//! (`output_w`, a codec *string*, `render_crf`…), and a wrong mapping there — width written
//! into `output_h`, say — is invisible to core while the user gets the wrong frame. This
//! bites the mapping itself, plus "the highlight is inferred from the numbers, not painted
//! decoration".

use aevum_core::delivery::DeliveryTable;
use aevum_core::export::OutputDynamic;
use aevum_ui::state::AppState;

/// 套用之后五个字段逐项正确，且高亮成立；改掉任何一项高亮必须熄灭。
///
/// After applying, all five fields are exact and the highlight holds; changing any one of
/// them must clear the highlight.
#[test]
fn applying_a_preset_writes_every_field_and_changing_one_clears_the_match() {
    let table = DeliveryTable::builtin();
    let douyin = table.get("douyin-vertical").expect("内置表必须有抖音竖屏");

    let mut state = AppState::default();
    // 先摆一组与预设明确不同的值：断言「被写进去」，而不是「本来就这样」。
    //
    // Start from values that clearly differ, so the assertions prove a write rather than an
    // accident of the defaults.
    state.output_w = 3840;
    state.output_h = 2160;
    state.output_fps = 24.0;
    state.codec = "HEVC".to_owned();
    state.render_crf = 16;
    state.render_preset = "slow".to_owned();
    assert!(
        !state.matches_delivery(douyin),
        "前置：4K/HEVC/CRF16 不该已经等于抖音口径"
    );

    state.apply_delivery(douyin);

    assert_eq!((state.output_w, state.output_h), (1080, 1920));
    assert_eq!(state.output_fps, 30.0, "预设给了帧率就必须生效");
    assert_eq!(state.codec, "H.264");
    assert_eq!(state.render_crf, 20);
    assert_eq!(state.render_preset, "medium");
    assert!(state.matches_delivery(douyin), "套完之后必须高亮");

    // 逐项拆掉，每拆一项都必须熄灭——高亮是按数值反推的。
    //
    // Break one field at a time; each break must clear the highlight, since it is inferred
    // from the numbers.
    for (label, break_it) in [
        (
            "画幅",
            Box::new(|s: &mut AppState| s.output_w = 1081) as Box<dyn Fn(&mut AppState)>,
        ),
        ("帧率", Box::new(|s: &mut AppState| s.output_fps = 25.0)),
        (
            "编码器",
            Box::new(|s: &mut AppState| s.codec = "HEVC".to_owned()),
        ),
        ("CRF", Box::new(|s: &mut AppState| s.render_crf = 21)),
    ] {
        let mut broken = AppState::default();
        broken.apply_delivery(douyin);
        break_it(&mut broken);
        assert!(
            !broken.matches_delivery(douyin),
            "改掉{label}之后还亮着，用户看到的是假的「当前口径」"
        );
    }
}

/// `fps: None` 的预设不许动用户定的节奏。
///
/// A preset with `fps: None` must not touch the user's pacing.
#[test]
fn a_preset_without_a_rate_keeps_the_panel_pacing() {
    let table = DeliveryTable::builtin();
    let master = table.get("master-1080p").expect("内置表必须有母版");

    let mut state = AppState::default();
    state.output_fps = 12.0;
    state.apply_delivery(master);

    assert_eq!(state.output_fps, 12.0, "fps=None 的预设不许碰面板上的帧率");
    assert_eq!((state.output_w, state.output_h), (1920, 1080));
    assert!(state.matches_delivery(master));
}

/// H.264 预设把动态范围可见地退回 SDR：不留「面板写 HDR、导出才报错」的组合。
///
/// An H.264 preset drops the dynamic range to SDR visibly, never leaving a "panel says HDR,
/// export errors out" combination behind.
#[test]
fn an_h264_preset_drops_the_dynamic_range_to_sdr() {
    let table = DeliveryTable::builtin();
    let douyin = table.get("douyin-vertical").expect("内置表必须有抖音竖屏");

    let mut state = AppState::default();
    state.render_dynamic = OutputDynamic::HdrPq;
    state.apply_delivery(douyin);

    assert_eq!(
        state.render_dynamic,
        OutputDynamic::Sdr,
        "H.264 装不下 PQ，选它就要把动态范围退回 SDR（可见地退）"
    );
}
