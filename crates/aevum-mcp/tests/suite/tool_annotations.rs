//! MCP `annotations` 的判据：每个工具都要把四个提示位说清楚。
//!
//! # 为什么要有这一组
//!
//! 四个提示位在规范里都是可选的，于是「漏写」在编译器、在快照、在任何形状检查里都是
//! **静默通过**的：宿主拿到的 `annotations` 少一个键，它在调用前就不敢给用户任何提示，
//! 而这个缺失不会有任何东西报警。快照判据也帮不上忙——它只保证「与上次一致」，不保证
//! 「这一项是对的」。
//!
//! 所以这里守三层，从形状到判断：
//!
//! 1. **形状**：四个键齐全、都是布尔；
//! 2. **自洽**：只读不得同时声称破坏性，只读必须幂等；
//! 3. **判断**：破坏性集合与清单**逐条对齐**——新增一个会覆盖用户文件的工具却没标注，
//!    这里就红。这一层是硬编码的，故意的：它钉住的是「我们商量过的结论」，不是
//!    「代码里现在写着什么」。
//!
//! # Judgements for the MCP `annotations`: every tool must state all four hints.
//!
//! # Why this set exists
//!
//! All four hint bits are optional in the spec, so **omitting** one passes silently through
//! the compiler, the snapshot, and any shape check: a host that receives an `annotations`
//! object missing a key cannot warn the user before a call, and nothing raises a flag. The
//! snapshot criterion cannot help either -- it only guarantees "same as last time", not
//! "this one is right".
//!
//! So three layers are guarded here, from shape to judgement:
//!
//! 1. **Shape**: all four keys present and boolean;
//! 2. **Consistency**: read-only may not also claim destructive, and must be idempotent;
//! 3. **Judgement**: the destructive set matches the catalogue **entry by entry** -- adding a
//!    tool that overwrites user files without annotating it turns this red. This layer is
//!    hard-coded on purpose: it pins the conclusion we agreed on, not whatever the code
//!    happens to say today.

use std::collections::BTreeSet;

use serde_json::Value;

/// 读 `tools/list` 里某个工具的四位提示。
///
/// 缺键直接 panic：这正是本文件要防的事，不能用一个「默认 false」把它掩盖过去。
///
/// Read the four hint bits of one tool in `tools/list`.
///
/// A missing key panics: that is exactly what this file exists to catch, and a default of
/// `false` would hide it.
fn hints_of(tool: &Value) -> [bool; 4] {
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("<没有 name>");
    let ann = tool
        .get("annotations")
        .unwrap_or_else(|| panic!("{name} 没有 annotations"));
    let mut out = [false; 4];
    for (i, key) in [
        "readOnlyHint",
        "destructiveHint",
        "idempotentHint",
        "openWorldHint",
    ]
    .iter()
    .enumerate()
    {
        let v = ann
            .get(*key)
            .unwrap_or_else(|| panic!("{name} 的 annotations 缺 {key}"));
        out[i] = v
            .as_bool()
            .unwrap_or_else(|| panic!("{name} 的 {key} 不是布尔：{v}"));
    }
    out
}

/// 会覆盖或删除磁盘上已有用户数据的工具，**完整清单**。
///
/// 加一个导出类工具、或让某个现有工具开始写用户指定的路径，就必须在这里加一行——这正是
/// 让人停下来想一秒的地方。判定口径见 `aevum_mcp::Hints`：
/// 「覆盖的是不是本序列自己的账本」。
///
/// The **complete** list of tools that overwrite or delete user data already on disk.
///
/// Adding an export-style tool, or letting an existing one start writing a user-named path,
/// means adding a line here -- which is exactly the pause for thought this is for. The rule
/// is in `aevum_mcp::Hints`: "is the overwritten path this sequence's own bookkeeping?".
const DESTRUCTIVE: [&str; 11] = [
    "aevum_batch_run",
    "aevum_cube_export",
    "aevum_export_stills",
    "aevum_export_video",
    "aevum_focus_merge",
    "aevum_hdr_merge",
    "aevum_pano_merge",
    "aevum_queue_run",
    "aevum_stack_merge",
    "aevum_timeblend_merge",
    "aevum_xmp_write",
];

/// 只读工具，同样是一份完整清单。
///
/// The read-only tools, likewise a complete list.
const READ_ONLY: [&str; 11] = [
    "aevum_align_status",
    "aevum_cull_evaluate",
    "aevum_grade_get",
    "aevum_journal_list",
    "aevum_keyframe_list",
    "aevum_project_get",
    "aevum_queue_list",
    "aevum_queue_status",
    "aevum_sequence_info",
    "aevum_sequences_scan",
    "aevum_xmp_read",
];

/// 形状层：每个工具都有四项、都是布尔，且只读与破坏性不冲突。
///
/// Shape layer: every tool has four booleans, and read-only never conflicts with
/// destructive.
#[test]
fn every_tool_declares_four_boolean_hints() {
    let tools = aevum_mcp::tool_list();
    assert!(!tools.is_empty(), "工具清单是空的");
    for tool in &tools {
        let [read_only, destructive, idempotent, open_world] = hints_of(tool);
        let name = tool["name"].as_str().unwrap_or("<没有 name>");
        assert!(
            !(read_only && destructive),
            "{name} 同时声称只读与破坏性：宿主会拿到互相矛盾的提示"
        );
        assert!(
            !read_only || idempotent,
            "{name} 声称只读却不幂等：只读的重复调用不可能产生额外效果"
        );
        assert!(
            !open_world,
            "{name} 声称 openWorld：本服务器不联网，作用对象只有用户给的本地路径。\
             真要开，先在 Hints 里补预设并逐条审"
        );
    }
}

/// 判断层：破坏性集合与清单逐条对齐，双向都比。
///
/// 单向比对（只查「标注了的都在清单里」）会漏掉「新工具没标注」这种最危险的情况。
///
/// Judgement layer: the destructive set matches the list in both directions.
///
/// A one-way check ("everything annotated is in the list") would miss the dangerous case:
/// a new tool that was never annotated at all.
#[test]
fn the_destructive_set_matches_the_catalogue() {
    let expected: BTreeSet<&str> = DESTRUCTIVE.iter().copied().collect();
    let actual: BTreeSet<String> = aevum_mcp::tool_list()
        .iter()
        .filter(|t| hints_of(t)[1])
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();

    let missing: Vec<&str> = expected
        .iter()
        .copied()
        .filter(|n| !actual.contains(*n))
        .collect();
    let extra: Vec<&String> = actual
        .iter()
        .filter(|n| !expected.contains(n.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "这些工具应该标 destructive 却没标（宿主不会在覆盖文件前提醒用户）：{missing:?}"
    );
    assert!(
        extra.is_empty(),
        "这些工具标了 destructive 但不在清单里：要么是判断变了（改清单），\
         要么是标错了（改 hints）：{extra:?}"
    );
}

/// 判断层：只读集合同样逐条对齐。
///
/// 只读标错的代价是双向的——把写操作标成只读，宿主不会提示；把纯查询标成写操作，用户
/// 每查一次都被警告一次，久了就学会无视所有警告。
///
/// Judgement layer: the read-only set is aligned entry by entry as well.
///
/// Mis-annotating read-only costs in both directions: a write marked read-only gets no
/// warning, while a pure query marked as a write trains the user to ignore every warning.
#[test]
fn the_read_only_set_matches_the_catalogue() {
    let expected: BTreeSet<&str> = READ_ONLY.iter().copied().collect();
    let actual: BTreeSet<String> = aevum_mcp::tool_list()
        .iter()
        .filter(|t| hints_of(t)[0])
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();

    let missing: Vec<&str> = expected
        .iter()
        .copied()
        .filter(|n| !actual.contains(*n))
        .collect();
    let extra: Vec<&String> = actual
        .iter()
        .filter(|n| !expected.contains(n.as_str()))
        .collect();
    assert!(missing.is_empty(), "这些纯查询工具没标只读：{missing:?}");
    assert!(
        extra.is_empty(),
        "这些工具被标成只读，但它们会改东西：{extra:?}"
    );
}
