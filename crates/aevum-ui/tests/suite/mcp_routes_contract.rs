//! QA 独立验收：`aevum_core::mcp_routes::COMMANDS` 与 `command_catalog()` 的一一对应。
//!
//! 这里**只走公开接口**（`aevum_ui::control::command_catalog()` 的返回值），
//! 不复用 `control.rs` 里已有的 `catalog_matches_core_command_table`，
//! 免得同一个错误假设被抄两遍（比如两边都按同一种方式取集合，取漏了也看不出来）。
//!
//! 覆盖：条数、集合一致性（顺序无关）、双向差集、重复 ID、ID 合法性、
//! 机械名往返、以及 `tools/list` 要用的文案字段不为空。

use std::collections::BTreeSet;

#[test]
fn qa_catalog_and_core_table_are_the_same_set_of_ids() {
    let catalog = aevum_ui::control::command_catalog();
    let actual: Vec<&str> = catalog.iter().map(|spec| spec.id).collect();
    let core: Vec<&str> = aevum_core::mcp_routes::COMMANDS.to_vec();

    // 1) 条数：两边都必须是 77
    assert_eq!(core.len(), 77, "core 的 COMMANDS 不是 77 条");
    assert_eq!(actual.len(), 77, "command_catalog() 不是 77 条");

    // 2) 各自内部不能有重复
    let actual_set: BTreeSet<&str> = actual.iter().copied().collect();
    let core_set: BTreeSet<&str> = core.iter().copied().collect();
    assert_eq!(
        actual_set.len(),
        actual.len(),
        "command_catalog() 里有重复 ID：{:?}",
        duplicates(&actual)
    );
    assert_eq!(
        core_set.len(),
        core.len(),
        "COMMANDS 里有重复 ID：{:?}",
        duplicates(&core)
    );

    // 3) 双向差集都为空 —— 这才是「一一对应」
    let missing: Vec<&&str> = core_set.difference(&actual_set).collect();
    let extra: Vec<&&str> = actual_set.difference(&core_set).collect();
    assert!(
        missing.is_empty(),
        "COMMANDS 有、命令目录没有（MCP 会路由到一条 GUI 执行不了的命令）：{missing:?}"
    );
    assert!(
        extra.is_empty(),
        "命令目录有、COMMANDS 没有（GUI 能执行，MCP 却路由不到）：{extra:?}"
    );

    // 4) 顺序：文档声称「顺序与 command_catalog() 一致」。只作事实核对，
    //    顺序不影响路由，所以这里失败也不该拦发布——但既然写了就该是真的。
    assert_eq!(
        actual, core,
        "COMMANDS 的顺序与 command_catalog() 不一致（不影响路由，但文档的说法不成立）"
    );
}

#[test]
fn qa_every_catalog_id_survives_the_mechanical_name_round_trip() {
    for spec in aevum_ui::control::command_catalog() {
        let id = spec.id;

        // ID 本身是点分小写；带大写 / 空格 / 中文都是抄错了
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_'),
            "命令 ID 含非法字符：{id}"
        );
        assert!(id.contains('.'), "命令 ID 应当是 `组.动作`：{id}");

        // 工具名：严格 MCP 客户端只收 [A-Za-z0-9_-]
        let name = aevum_core::mcp_routes::mcp_name(id);
        assert!(name.starts_with("aevum_"), "工具名没带前缀：{name}");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'),
            "工具名含非法字符：{name}"
        );

        // 机械反查必须回到同一条命令
        assert_eq!(
            aevum_core::mcp_routes::command_for_mcp_name(&name),
            Some(id),
            "{name} 反查不到 {id}"
        );
    }
}

#[test]
fn qa_catalog_text_fields_used_by_tools_list_are_non_empty() {
    // 附着模式在线时把 summary/params/schema 直接塞进 tool description；
    // 空串会让 agent 以为这条工具没有说明。
    for spec in aevum_ui::control::command_catalog() {
        assert!(!spec.summary.trim().is_empty(), "{} 缺 summary", spec.id);
        assert!(!spec.params.trim().is_empty(), "{} 缺 params", spec.id);
        let schema = spec.schema();
        assert!(
            schema.is_object(),
            "{} 的 schema 不是 JSON 对象：{schema}",
            spec.id
        );
    }
}

#[test]
fn qa_system_commands_payload_carries_every_id() {
    // tools/list 的来源就是 catalog_json()（rpc 的 system.commands）；
    // 这里直接核对这条报文里的 id 覆盖全集，别只看命令表。
    let payload = aevum_ui::control::catalog_json();
    let ids: Vec<&str> = payload
        .iter()
        .filter_map(|entry| entry.get("id").and_then(|id| id.as_str()))
        .collect();
    assert_eq!(ids.len(), 77, "system.commands 报的条数不是 77");
    let got: BTreeSet<&str> = ids.iter().copied().collect();
    assert_eq!(got.len(), 77, "system.commands 报文里有重复 id");
    for id in aevum_core::mcp_routes::COMMANDS {
        assert!(got.contains(id), "system.commands 报文缺 {id}");
    }
}

fn duplicates(ids: &[&str]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut dup = BTreeSet::new();
    for id in ids {
        if !seen.insert(*id) {
            dup.insert((*id).to_owned());
        }
    }
    dup.into_iter().collect()
}
