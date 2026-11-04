use super::*;

/// 落盘的合并参数灌回 AppState：逐项到位；坏 key / 越界值夹回默认，
/// 不会把手改过的 settings.json 带进一个不存在的模式。
///
/// Persisted merge parameters pour back into AppState item by item; unparsable
/// keys / out-of-range values clamp back to the defaults, so a hand-edited
/// settings.json cannot bring up a mode that does not exist.
#[test]
fn adopt_settings_applies_merge_params_and_sanitises_bad_values() {
    let good: crate::settings::Settings = serde_json::from_str(
        json!({
            "merge": {
                "hdr_around": 3,
                "stack_around": 9,
                "stack_mode": "median",
                "stack_sigma": 2.0,
                "stack_fill": false,
                "tb_around": 15,
                "tb_layout": "bands",
                "tb_bands": 6,
                "tb_feather": 0.2,
                "tb_horizon": 0.3,
                "tb_horizon_auto": true,
                "tb_deghost": 1.5,
                "tb_match": false,
                "focus_around": 11,
                "focus_method": "weighted",
                "focus_radius": 8,
                "focus_threshold": 30.0,
                "focus_smooth_z": 4,
                "focus_feather": 12.0,
            }
        })
        .to_string()
        .as_str(),
    )
    .unwrap();
    let mut state = AppState::new();
    state.adopt_settings(good);
    assert_eq!(state.hdr_around, 3);
    assert_eq!(state.stack_around, 9);
    assert_eq!(state.stack_mode, aevum_core::stack::Mode::Median);
    assert_eq!(state.stack_sigma, 2.0);
    assert!(!state.stack_fill);
    assert_eq!(state.tb_around, 15);
    assert_eq!(state.tb_layout, aevum_core::timeblend::Layout::Bands);
    assert_eq!(state.tb_bands, 6);
    assert_eq!(state.tb_feather, 0.2);
    assert_eq!(state.tb_horizon, 0.3);
    assert!(state.tb_horizon_auto);
    assert_eq!(state.tb_deghost, 1.5);
    assert!(!state.tb_match);
    assert_eq!(state.focus_around, 11);
    assert_eq!(state.focus_method, aevum_core::focus::Method::Weighted);
    assert_eq!(state.focus_radius, 8);
    assert_eq!(state.focus_threshold, 30.0);
    assert_eq!(state.focus_smooth_z, 4);
    assert_eq!(state.focus_feather, 12.0);

    // 坏值：未知方法 key 落回深度图，越界数全部夹回合法区间。
    // 这里的未知 key 不能用 "pyramid"：它是下一版（金字塔法）的合法方法名，
    // 那时这条断言会从「未知 key 回落」变成「合法值被误判」，测试就失去了它要守的语义。
    let bad: crate::settings::Settings = serde_json::from_str(
        json!({
            "merge": {
                "focus_method": "laplacian",
                "focus_radius": 999,
                "focus_threshold": 500.0,
                "tb_feather": 9.0,
                "hdr_around": 0,
                "stack_sigma": -1.0,
            }
        })
        .to_string()
        .as_str(),
    )
    .unwrap();
    let mut state2 = AppState::new();
    state2.adopt_settings(bad);
    assert_eq!(state2.focus_method, aevum_core::focus::Method::DepthMap);
    assert_eq!(state2.focus_radius, 32);
    assert_eq!(state2.focus_threshold, 100.0);
    assert_eq!(state2.tb_feather, 0.5);
    assert_eq!(state2.hdr_around, 1);
    assert_eq!(state2.stack_sigma, 3.0);
}

/// settings() 收账：面板参数进 Settings.merge；结果态（上次成片 / 报错 / 预览）
/// 不进。另外新状态的参数必须与 MergeParams::default() 逐项一致——两处默认
/// 不同步，「没存过的用户」和「存过默认值的用户」会看到不同的首次合成。
///
/// settings() collects the bill: panel parameters land in Settings.merge; result
/// state (last output / errors / previews) does not. Also, a fresh state's
/// parameters must match MergeParams::default() item by item -- out-of-sync
/// defaults would give "never saved" and "saved the defaults" users different
/// first merges.
#[test]
fn settings_carries_merge_params_but_not_result_state() {
    // 新状态与默认表逐项一致（默认同步的不变量）。
    let fresh = AppState::new();
    assert_eq!(
        fresh.settings().merge,
        crate::settings::MergeParams::default()
    );
    // 改过的参数进收账，结果态不进。
    let mut state = AppState::new();
    state.focus_radius = 9;
    state.stack_mode = aevum_core::stack::Mode::SigmaClip;
    state.focus_last = Some("out/focus.png".to_owned());
    let s = state.settings();
    assert_eq!(s.merge.focus_radius, 9);
    assert_eq!(s.merge.stack_mode, "sigma-clip");
}

/// 落盘往返：`focus_align: true` 存进 settings、序列化、读回、灌回新状态仍是 `true`
/// ——面板上那个勾选必须活过一次重启，否则用户每次开软件都要重勾。
///
/// Round trip: `focus_align: true` is written into settings, serialised, read back and
/// poured into a fresh state as `true` -- that checkbox has to survive a restart, or the
/// user re-ticks it on every launch.
#[test]
fn focus_align_survives_the_settings_round_trip() {
    let mut state = AppState::new();
    assert!(!state.focus_align, "默认必须是关的");
    state.focus_align = true;
    let encoded = serde_json::to_string(&state.settings()).expect("序列化");
    let decoded: crate::settings::Settings = serde_json::from_str(&encoded).expect("反序列化");
    assert!(decoded.merge.focus_align, "落盘形状里必须是 true");
    let mut fresh = AppState::new();
    fresh.adopt_settings(decoded);
    assert!(fresh.focus_align, "灌回后必须是 true");
}

/// 老 settings.json 没有 `focus_align` 这个字段：读回落 `false`（`serde(default)`
/// 兜底），不报错、不会把「没存过」误当成「存过 true」。
///
/// An old settings.json without `focus_align` loads as `false` (the `serde(default)`
/// fallback): no error, and "never saved" is never mistaken for "saved as true".
#[test]
fn old_settings_without_focus_align_load_as_off() {
    let old: crate::settings::Settings = serde_json::from_str(
        r#"{"merge":{"focus_around":9,"focus_method":"weighted","focus_radius":4}}"#,
    )
    .expect("缺字段不该报错");
    assert!(!old.merge.focus_align, "缺字段必须落 false");
    assert_eq!(old.merge.focus_around, 9, "同节其余字段照常读回");
    let mut state = AppState::new();
    state.focus_align = true;
    state.adopt_settings(old);
    assert!(
        !state.focus_align,
        "灌回时也必须被覆盖成 false，而不是留着旧值"
    );
}
