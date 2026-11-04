//! `tools/list` 快照判据：工具清单在 `src/tools.rs` 拆分前后必须逐字节不变。
//!
//! # 为什么要有这条
//!
//! 拆 `tools.rs` 是纯搬迁作业，但「纯搬迁」这句话本身不可验证：删掉一条工具、
//! 改一个字段名、把默认值从 `false` 写成 `true`、复制粘贴出一个重名工具，
//! 编译器一句话都不会说。而 agent 的世界里工具名就是 API——改名等于把历史会话里
//! 的调用全部作废，改 schema 等于悄悄换了参数口径。
//!
//! 所以先把清单冻住，再动刀。冻结的对象是 [`aevum_mcp::tool_list`]，也就是
//! `src/server.rs` 里 `tools/list` 的**唯一**构造点，因此比对的是线上真正发出去的
//! 那份字节，不是测试里另抄一遍的投影。
//!
//! # 换行
//!
//! 仓库 `.gitattributes` 里 `*.json text`、`core.autocrlf=true`，同一份快照在
//! Windows 检出成 CRLF、在 Linux 检出成 LF。两边都归一成 LF 再比：判据不能因为
//! 检出平台而红，那是信息量为零的红。
//!
//! # 更新快照
//!
//! `AEVUM_WRITE_SNAPSHOT=1 cargo test -p aevum-mcp --test suite tools_list_snapshot`
//!
//! 只在**确认改动是有意的**之后才敲这条；写完必须重编一次再跑一遍，因为
//! `include_str!` 是在编译期把快照嵌进二进制的。`tests/` 目录不进 doc_lint
//! （它是回归靶子，不是交付的 API 面），但注释照仓库习惯写中英双语。
//!
//! A snapshot criterion for `tools/list`: the tool catalogue must stay byte-for-byte
//! identical while `src/tools.rs` is split into modules.
//!
//! # Why this exists
//!
//! Splitting `tools.rs` is a pure move, but "pure move" is not something the compiler can
//! verify: drop a tool, rename a field, flip a default from `false` to `true`, or copy-paste
//! a duplicate tool name, and nothing complains. To an agent the tool names *are* the API --
//! renaming one voids every call in past sessions, and reshaping a schema silently changes
//! what the arguments mean.
//!
//! So freeze the catalogue first, then cut. What gets frozen is [`aevum_mcp::tool_list`],
//! the single construction point of `tools/list` in `src/server.rs`, so the bytes compared
//! here are the bytes that go out on the wire, not a projection re-written in the test.
//!
//! # Line endings
//!
//! `.gitattributes` marks `*.json` as text and `core.autocrlf=true`, so the same snapshot
//! checks out as CRLF on Windows and LF on Linux. Both sides are normalised to LF before
//! comparing: a criterion must not go red just because of the checkout platform.
//!
//! # Regenerating
//!
//! `AEVUM_WRITE_SNAPSHOT=1 cargo test -p aevum-mcp --test suite tools_list_snapshot`
//!
//! Only after confirming the change is intentional; then rebuild and run again, because
//! `include_str!` embeds the snapshot at compile time.

use std::fmt::Write as _;

use serde_json::Value;

/// 入库的快照：`{"tools": [...]}`，与 `tools/list` 的响应体同形。
///
/// The committed snapshot: `{"tools": [...]}`, shaped like a `tools/list` response body.
const SNAPSHOT: &str = include_str!("../snapshots/tools_list.json");

/// 快照在磁盘上的绝对路径，只给 `AEVUM_WRITE_SNAPSHOT` 那条重生成路径用。
///
/// Absolute on-disk path of the snapshot, used only by the `AEVUM_WRITE_SNAPSHOT` path.
const SNAPSHOT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/snapshots/tools_list.json"
);

/// 线上 `tools/list` 的响应体，缩进美化后成文。
///
/// The live `tools/list` response body, pretty-printed as text.
fn live() -> String {
    let payload = serde_json::json!({ "tools": aevum_mcp::tool_list() });
    serde_json::to_string_pretty(&payload).expect("工具清单必须能序列化")
}

/// 统一换行，去掉平台差异（见模块注释）。
///
/// Normalise line endings so the checkout platform cannot decide the result.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[test]
fn tools_list_matches_snapshot() {
    let live = normalize(&live());

    // 显式开关才写盘：平时缺少快照、或快照过期，都必须红，不能悄悄自愈。
    if std::env::var("AEVUM_WRITE_SNAPSHOT").is_ok_and(|value| value == "1") {
        if let Some(parent) = std::path::Path::new(SNAPSHOT_PATH).parent() {
            std::fs::create_dir_all(parent).expect("建快照目录");
        }
        std::fs::write(SNAPSHOT_PATH, format!("{live}\n")).expect("写快照");
        return;
    }

    let snapshot = normalize(SNAPSHOT);
    // 文件末尾那一个换行是仓库习惯，不算进「字节」；剩下的全部逐字节比。
    let expected = snapshot.strip_suffix('\n').unwrap_or(&snapshot);

    assert!(
        live == expected,
        "\n{}",
        describe(&live, expected, SNAPSHOT_PATH)
    );
}

#[test]
fn tool_names_are_unique() {
    // JSON 快照能过、重名却能溜过去：顺序一致时逐字节比对看不出来，
    // 但客户端按名字路由，两个同名工具里只有一个能被执行。拆文件时最容易
    // 复制粘贴出一个重名条目，所以单独一条判据。
    //
    // A byte-identical snapshot still passes when a name is duplicated -- if the order
    // matches, the bytes match. But clients route by name, so only one of the two twins can
    // ever run. Copy-pasting a duplicate is the likeliest slip while splitting files, hence
    // a dedicated criterion.
    let tools = aevum_mcp::tool_list();
    assert!(!tools.is_empty(), "tools/list 不能是空的");

    let mut first_seen: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let mut duplicates: Vec<String> = Vec::new();
    for (index, tool) in tools.iter().enumerate() {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .expect("每条工具都要有字符串 name");
        if let Some(first) = first_seen.insert(name.to_owned(), index + 1) {
            duplicates.push(format!("{name}（第 {first} 条与第 {} 条）", index + 1));
        }
    }
    assert!(
        duplicates.is_empty(),
        "工具名重复：{}",
        duplicates.join("、")
    );
}

/// 把「哪一条工具、哪个字段」变了说清楚，别只丢一句 `assertion failed`。
///
/// 顺序：能解析就先做结构化比对（增删 / 改名 / 换序 / 逐条字段），
/// 解析不了再退回按行比——那时候报的是「第几行、两边原文」，
/// 也足够定位。
///
/// Say **which tool and which field** changed instead of leaving a bare
/// `assertion failed`. Structured comparison first (added / removed / renamed / reordered /
/// per-field); if either side does not parse, fall back to a line diff, which still points
/// at the exact line and shows both texts.
fn describe(live: &str, expected: &str, path: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "tools/list 与快照不一致（快照：{path}）。\n\
         若改动是有意的，跑 `AEVUM_WRITE_SNAPSHOT=1 cargo test -p aevum-mcp --test suite tools_list_snapshot` \
         重生成后重编再跑。"
    );
    if structured(live, expected, &mut out) {
        return out;
    }
    let _ = writeln!(out, "（两边无法解析成 JSON，退回按行比对）");
    line_diff("整个清单", live, expected, &mut out);
    out
}

/// 结构化比对；两边都能解析时返回 `true`。
///
/// Structured comparison; returns `true` when both sides parse.
fn structured(live: &str, expected: &str, out: &mut String) -> bool {
    let (Ok(live_doc), Ok(expected_doc)) = (
        serde_json::from_str::<Value>(live),
        serde_json::from_str::<Value>(expected),
    ) else {
        return false;
    };
    let (Some(live_tools), Some(expected_tools)) = (
        live_doc.get("tools").and_then(Value::as_array),
        expected_doc.get("tools").and_then(Value::as_array),
    ) else {
        let _ = writeln!(out, "两边都缺 `tools` 数组，或它不是数组");
        return true;
    };

    let _ = writeln!(
        out,
        "快照 {} 条工具，实际 {} 条。",
        expected_tools.len(),
        live_tools.len()
    );

    // 1. 顺序与数量。名字序列不同就先说清楚增、删、换位，逐条再比下去只会刷屏。
    let live_names: Vec<&str> = live_tools.iter().map(tool_name).collect();
    let expected_names: Vec<&str> = expected_tools.iter().map(tool_name).collect();
    if live_names != expected_names {
        let live_set: std::collections::BTreeSet<&str> = live_names.iter().copied().collect();
        let expected_set: std::collections::BTreeSet<&str> =
            expected_names.iter().copied().collect();
        let removed: Vec<&str> = expected_set.difference(&live_set).copied().collect();
        let added: Vec<&str> = live_set.difference(&expected_set).copied().collect();
        if !removed.is_empty() {
            let _ = writeln!(out, "少了这些工具：{}", removed.join("、"));
        }
        if !added.is_empty() {
            let _ = writeln!(out, "多了这些工具：{}", added.join("、"));
        }
        if removed.is_empty() && added.is_empty() {
            let _ = writeln!(out, "工具集合一样，但顺序变了：");
            for (index, (live_name, expected_name)) in
                live_names.iter().zip(&expected_names).enumerate()
            {
                if live_name != expected_name {
                    let _ = writeln!(
                        out,
                        "  第 {} 条：快照 {expected_name}，实际 {live_name}",
                        index + 1
                    );
                    break;
                }
            }
        }
    }

    // 2. 同名同序的逐条比字段。只比 schema 的三件套，多出来的键也算「变了」。
    for (index, (live_tool, expected_tool)) in live_tools.iter().zip(expected_tools).enumerate() {
        let name = tool_name(live_tool);
        if name != tool_name(expected_tool) {
            continue; // 位置已经错开了，上面报过增删，这里不再逐条误报。
        }
        for field in ["description", "inputSchema"] {
            if live_tool.get(field) == expected_tool.get(field) {
                continue;
            }
            let _ = writeln!(out, "工具 {name}（第 {} 条）的 `{field}` 变了：", index + 1);
            line_diff(
                field,
                &pretty(live_tool.get(field)),
                &pretty(expected_tool.get(field)),
                out,
            );
        }
        // 键集合变了（比如漏了 `description`）也报出来。
        let keys = |value: &Value| -> Vec<String> {
            value
                .as_object()
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default()
        };
        if keys(live_tool) != keys(expected_tool) {
            let _ = writeln!(
                out,
                "工具 {name}（第 {} 条）的字段名变了：快照 {:?}，实际 {:?}",
                index + 1,
                keys(expected_tool),
                keys(live_tool)
            );
        }
    }
    true
}

/// 取工具名；缺名字时给一个占位符，让报错本身别 panic。
///
/// Read the tool name; a placeholder instead of a panic when it is missing.
fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("<无名>")
}

/// 一个字段的美化文本；不存在时用 `null` 占位，避免把「缺字段」和「字段为 null」混起来。
///
/// Pretty text of one field, `null` when absent -- so "missing" and "null" stay apart.
fn pretty(field: Option<&Value>) -> String {
    match field {
        Some(value) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "<无法序列化>".to_owned())
        }
        None => "<缺这个字段>".to_owned(),
    }
}

/// 按行比对：先跳过公共前缀，报第一处之后的三行，再报总差异行数。
///
/// 单行超长的（schema 常见）截到 240 字符，否则一条报错能刷满一屏。
///
/// Line diff: skip the common prefix, show three lines from the first difference, then the
/// total count. Very long single lines (schemas) are clipped at 240 characters so one failure
/// does not flood the terminal.
fn line_diff(label: &str, live: &str, expected: &str, out: &mut String) {
    let live_lines: Vec<&str> = live.lines().collect();
    let expected_lines: Vec<&str> = expected.lines().collect();
    let prefix = live_lines
        .iter()
        .zip(&expected_lines)
        .take_while(|(live_line, expected_line)| live_line == expected_line)
        .count();
    let differing = live_lines
        .iter()
        .zip(&expected_lines)
        .filter(|(live_line, expected_line)| live_line != expected_line)
        .count();
    let total = differing + live_lines.len().abs_diff(expected_lines.len());
    let _ = writeln!(
        out,
        "  {label}：{total} 行不同，第一处是第 {} 行",
        prefix + 1
    );
    for offset in 0..3 {
        let index = prefix + offset;
        let live_line = live_lines.get(index);
        let expected_line = expected_lines.get(index);
        if live_line == expected_line {
            continue;
        }
        if let Some(line) = expected_line {
            let _ = writeln!(out, "    快照：{}", clip(line));
        }
        match live_line {
            Some(line) => {
                let _ = writeln!(out, "    实际：{}", clip(line));
            }
            None => {
                let _ = writeln!(out, "    实际：（这一行没了）");
            }
        }
    }
}

/// 截断过长的单行，保留可辨认的开头。
///
/// Clip an over-long line while keeping a recognisable head.
fn clip(line: &str) -> String {
    const LIMIT: usize = 240;
    if line.chars().count() <= LIMIT {
        return line.to_owned();
    }
    let head: String = line.chars().take(LIMIT).collect();
    format!("{head}…（共 {} 字符）", line.chars().count())
}
