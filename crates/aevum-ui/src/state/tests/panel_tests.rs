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
    // 每个**带参数分组**的模块首屏都要有一组展开，否则面板只剩一排标题。
    // （整页视图已随项目库一并移除，不再需要例外分支。）
    //
    // Every module with parameter groups needs one open on the first screen, otherwise the
    // panel is nothing but headings. The full-page views were removed along with the
    // Library module, so no exception branch is needed any more.
    for module in Module::ALL {
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

/// 三个面板开关与工作流选择要能过一遍 settings.json：此前 `show_left` /
/// `show_dock` 的注释声称落盘、实际没落，重启就回默认（用户「变不回去了」的同类）。
///
/// The three panel switches and the workflow choice must survive a settings.json round
/// trip. Their comments claimed persistence that did not exist, so a restart reset them.
#[test]
fn panel_switches_and_workflow_survive_a_settings_round_trip() {
    let mut a = AppState::new();
    a.show_dock = false;
    a.show_flow = true;
    a.show_left = true;
    a.flow_task = crate::shell::flow::FlowTask::Merge;

    let saved = a.settings();
    assert_eq!(saved.show_dock, Some(false));
    assert_eq!(saved.show_flow, Some(true));
    assert_eq!(saved.show_left, Some(true));
    assert_eq!(saved.flow_task.as_deref(), Some("merge"));

    let json = serde_json::to_string(&saved).expect("序列化设置");
    let back: crate::settings::Settings = serde_json::from_str(&json).expect("解析设置");
    let mut b = AppState::new();
    b.adopt_settings(back);
    assert!(!b.show_dock);
    assert!(b.show_flow);
    assert!(b.show_left);
    assert_eq!(b.flow_task, crate::shell::flow::FlowTask::Merge);

    // 老文件缺这一节：三项回首启默认，坏 key 回落默认工作流。
    //
    // An older file without the section: defaults on all three, and a bad key falls back
    // to the default workflow.
    let old: crate::settings::Settings =
        serde_json::from_str(r#"{"font_level":1,"flow_task":"no-such-workflow"}"#).unwrap();
    let mut c = AppState::new();
    c.adopt_settings(old);
    assert!(c.show_dock);
    assert!(!c.show_flow);
    assert!(!c.show_left);
    assert_eq!(c.flow_task, crate::shell::flow::FlowTask::default());
}

/// 帧首比对会自动置脏：新增赋值点不必记得手工标 `settings_dirty`。
///
/// The frame-start diff marks settings dirty automatically, so a new assignment site does
/// not have to remember to flag it by hand.
#[test]
fn changing_a_layout_switch_marks_settings_dirty_on_the_next_poll() {
    let ctx = egui::Context::default();
    let mut a = AppState::new();
    a.settings_dirty = false;
    a.poll(&ctx);
    assert!(!a.settings_dirty, "没改任何开关就不该写盘");

    a.show_flow = true;
    a.poll(&ctx);
    assert!(a.settings_dirty, "改了工作流面板开关却没置脏");

    a.settings_dirty = false;
    a.poll(&ctx);
    assert!(!a.settings_dirty, "同一状态不该每帧反复写盘");
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
