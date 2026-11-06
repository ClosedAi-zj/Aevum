use super::*;

/// 首屏只留高频组：显影这一屏按 ACR 分成九区，默认若全摊开就是一条长滚。
#[test]
fn first_screen_keeps_only_the_frequent_groups_open() {
    let dev_open = GROUP_SPECS
        .iter()
        .filter(|s| s.open && s.module == Some(Module::Develop))
        .count();
    assert!(
        dev_open <= 6,
        "显影首屏展开了 {dev_open} 组——「减少信息负担」就靠这个数往下压"
    );
    let closed = default_collapsed();
    // 典型的高频/低频各取一个：开基本、折光学。
    assert!(!closed.contains("basic"));
    assert!(closed.contains("curve"));
    assert!(closed.contains("optics"));
    // 每个**有参数分组**的模块都得有展开的组，否则整屏只剩标题条，等于误伤。
    // 项目库与设置是整页视图，本来就没有参数分组——把它们算进来，这条判据会
    // 要求「设置页首屏展开一个推子组」，那是无意义的要求。
    //
    // Every module that **has parameter groups** needs one open on the first screen,
    // otherwise the panel is nothing but headings. Library and Settings are full-page
    // views with no parameter groups; including them would demand "the settings page
    // opens a fader group on first screen", which means nothing.
    for module in Module::ALL {
        if matches!(module, Module::Library | Module::Settings) {
            continue;
        }
        assert!(
            GROUP_SPECS
                .iter()
                .any(|s| s.open && s.module == Some(module)),
            "{} 模块首屏一个展开的组都没有",
            module.cn()
        );
    }
}

/// 折叠态要能从 JSON 走一圈回来：键名 ↔ `&'static str` 的互转全靠登记表。
#[test]
fn collapse_state_survives_a_settings_round_trip() {
    let mut a = AppState::new();
    assert_eq!(a.collapsed, default_collapsed(), "首屏该是默认折叠态");
    a.collapsed.insert("basic"); // 用户连「基本」也折了
    a.collapsed.remove("curve"); // 又把曲线点开

    let json = serde_json::to_string(&a.settings()).expect("序列化设置");
    let back: crate::settings::Settings = serde_json::from_str(&json).expect("解析设置");
    let mut b = AppState::new();
    b.adopt_settings(back);
    assert_eq!(b.collapsed, a.collapsed, "折叠态没过一圈就丢了");
}

/// 老设置文件里没有这一项 → 回到首屏默认；**不是**「全展开」。这条如果不成立，
/// 升级上来的用户第一眼看到的就是那屏十八段长滚，等于这次优化对老用户失效。
#[test]
fn missing_collapsed_field_falls_back_to_defaults() {
    let old: crate::settings::Settings = serde_json::from_str(r#"{"font_level":1}"#).unwrap();
    let mut s = AppState::new();
    s.adopt_settings(old);
    assert_eq!(s.collapsed, default_collapsed());
}

/// 反过来：用户亲手把每一组都点开（空列表）是明确表态，默认态不许覆盖他。
#[test]
fn explicit_all_open_is_respected() {
    let all_open: crate::settings::Settings = serde_json::from_str(r#"{"collapsed":[]}"#).unwrap();
    let mut s = AppState::new();
    s.adopt_settings(all_open);
    assert!(s.collapsed.is_empty(), "用户全展开的表态被默认态覆盖了");
}

/// 「只看这一组」只收同模块的组：折到别的模块去，切过去就看见一屏全折着的
/// 面板——用户会以为功能坏了。dock 页的组（`module == None`）自成一组。
#[test]
fn solo_only_folds_its_own_scope() {
    let mut s = AppState::new();
    s.solo_group("curve");
    // 同模块（显影）的兄弟组全收起来了，自己不动。
    assert!(!s.collapsed.contains("curve"), "被 solo 的那组得是展开的");
    assert!(s.collapsed.contains("basic"));
    assert!(s.collapsed.contains("optics"));
    // 别的模块不受影响。
    assert!(!s.collapsed.contains("frame"));
    assert!(!s.collapsed.contains("render-out"));
    assert!(s.settings_dirty, "折叠态变了必须触发落盘");

    // dock 页同理：solo 信息页的一组，快捷键页与代理页都不受牵连。
    s.expand_all_groups();
    s.solo_group("info-meta");
    assert!(s.collapsed.contains("info-capture"));
    assert!(!s.collapsed.contains("keys"), "dock 另一页的组被连坐了");
    assert!(!s.collapsed.contains("ai-log"), "dock 另一页的组被连坐了");
}

/// 全部展开 / 全部收起两个极端都得真的到头。
#[test]
fn expand_all_and_collapse_all_reach_the_ends() {
    let mut s = AppState::new();
    s.expand_all_groups();
    assert!(s.collapsed.is_empty());
    s.collapse_all_groups();
    assert_eq!(s.collapsed.len(), GROUP_SPECS.len());
    s.expand_all_groups();
    assert!(s.collapsed.is_empty());
}

/// 旧版本留下的键（这一版已经没有那个组）读回来要丢掉：留着的话既进不了
/// 界面的任何一次绘制，又会一直往配置文件里写。
#[test]
fn unknown_keys_are_dropped_on_load() {
    let stale: crate::settings::Settings =
        serde_json::from_str(r#"{"collapsed":["curve","some-removed-group"]}"#).unwrap();
    let mut s = AppState::new();
    s.adopt_settings(stale);
    assert!(s.collapsed.contains("curve"));
    assert_eq!(
        s.collapsed.len(),
        1,
        "没登记的键被收进来了：{:?}",
        s.collapsed
    );
}
