//! 命令面板的动作表与分发必须一一对应。
//!
//! # 这条判据为什么必须存在
//!
//! 2026-11-08 一次改动里，「撤销 / 重做 / 全部复位 / 导入 / 导出」的**分发分支**先写进了
//! `run()`，动作表 `ACTIONS` 却漏了同样五条。结果：分支能跑、代码能编译、编译期零告警，
//! 但用户在面板里搜不到它们——**只接线没接灯**。这类缺陷编译器看不见，人眼也容易漏，
//! 只有把两张表放在一起比才抓得住。
//!
//! 同一个下午又发生了一次反向的：工作区三条先写进了表，分发漏了。两边都说明同一件事——
//! 表与分支是两份必须同步的清单，缺一条就是"点了没反应"或"搜得到点不动"。
//!
//! # 为什么用文本扫描而不是把表暴露给测试
//!
//! 把 `ACTIONS` 与 `run` 的 id 做成 `pub` 供测试比对，等于为了测试放宽生产 API；而且
//! 那样只能证明"运行期表里有"，证明不了"分发里也有"（`match` 的分支不是数据）。文本扫描
//! 丑，但它检查的正是**这两份真实源码**。
//!
//! The palette's action table and its dispatch must match one-to-one.
//!
//! # Why this gate has to exist
//!
//! During one 2026-11-08 change the dispatch branches for undo / redo / reset-all / import /
//! export went into `run()` first while `ACTIONS` was missing the same five entries. The
//! result compiled, ran and produced zero warnings, yet the palette could not find them -- all
//! wired up, no light in the room. A compiler cannot see that, and neither does a careful read;
//! only putting the two lists side by side catches it.
//!
//! A reverse case happened the same afternoon: the three workspace entries reached the table
//! while their dispatch was missing. Both directions say the same thing -- the table and the
//! branches are two lists that must stay in sync, and a gap means either "clicking does
//! nothing" or "findable but dead".
//!
//! # Why a text scan instead of exporting the table
//!
//! Making `ACTIONS` and the `run` ids `pub` just for a test would widen the production API for
//! the test's sake, and it could only prove "the table has it" -- `match` arms are not data.
//! The scan is ugly, but it inspects **both real sources**.

use std::path::Path;

/// 读源码并去掉整行注释：注释里举的 `"xxx" =>` 例子不该被当成真分支。
///
/// Read a source file with whole-line comments removed, so an illustrative `"xxx" =>` in a
/// comment is not mistaken for a real arm.
fn clean(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let raw =
        std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("读不到 {rel}：{e}"));
    raw.lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 动作表里的 id，按出现顺序。
///
/// The ids in the action table, in order.
fn table_ids(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("const ACTIONS") {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        if t.starts_with("];") {
            break;
        }
        if let Some(rest) = t.strip_prefix("id: \"")
            && let Some(id) = rest.split('"').next()
        {
            out.push(id.to_owned());
        }
    }
    out
}

/// `run()` 里所有 `"xxx" =>` 分支的 id。
///
/// Every `"xxx" =>` arm inside `run()`.
fn dispatch_ids(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let start = src.find("fn run(").expect("找不到 run()");
    for line in src[start..].lines() {
        let t = line.trim_start();
        // 取第一对引号之间的 id，再看紧跟其后（跳过闭合引号）是不是 `=>`。
        // 用 `split('"').next()` 会漏掉闭合引号，`rest[id.len()..]` 于是从 `"` 开始，
        // 永远匹配不上——第一版就是这样，把 23 条全判成"缺分支"。
        //
        // Take the id between the first pair of quotes, then check that `=>` follows after
        // the closing quote. `split('"').next()` drops the closing quote, so `rest[id.len()..]`
        // starts at `"` and never matches -- the first cut of this did exactly that and
        // reported all 23 as missing.
        if let Some(rest) = t.strip_prefix('"')
            && let Some(end) = rest.find('"')
            && rest[end + 1..].trim_start().starts_with("=>")
        {
            out.push(rest[..end].to_owned());
        }
    }
    out
}

/// 表里每条动作都必须有分发分支，否则"搜得到、点了没反应"。
///
/// Every table entry needs a dispatch arm, or the row can be found and does nothing.
#[test]
fn every_palette_action_has_a_dispatch_arm() {
    let src = clean("shell/palette.rs");
    let table = table_ids(&src);
    let arms = dispatch_ids(&src);

    assert!(
        table.len() >= 20,
        "动作表只解析出 {} 条，解析器可能失效了：{table:?}",
        table.len()
    );

    let missing: Vec<&String> = table.iter().filter(|id| !arms.contains(id)).collect();
    assert!(
        missing.is_empty(),
        "这些动作在表里但 run() 没有分支——用户点下去什么都不会发生：{missing:?}"
    );
}

/// 分发里每条分支也必须出现在表里，否则那段代码是死路（用户永远触发不到）。
///
/// Every dispatch arm must also be listed, or that code is dead (nothing can trigger it).
#[test]
fn every_dispatch_arm_is_listed_in_the_table() {
    let src = clean("shell/palette.rs");
    let table = table_ids(&src);
    let arms = dispatch_ids(&src);

    let unlisted: Vec<&String> = arms.iter().filter(|id| !table.contains(id)).collect();
    assert!(
        unlisted.is_empty(),
        "这些分支在 run() 里但动作表没有——用户永远搜不到、点不到：{unlisted:?}"
    );
}
