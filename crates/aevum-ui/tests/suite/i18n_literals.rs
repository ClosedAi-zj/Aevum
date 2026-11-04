//! 静态守门：界面调用位置上的中文必须过翻译。
//!
//! # 这条测试补的是 `i18n_coverage` 的盲区，不是替代它
//!
//! `tests/suite/i18n_coverage.rs` 把界面**画一遍**再扫上屏的字，那是真值；但它只能看见
//! 「这一帧画出来的东西」。实测漏掉两类：
//!
//! 1. **要图才画的面板** —— 相机/镜头配置段（`adjust_profiles`）只在装了 RAW 时
//!    才走，空状态画不到，于是 `ui.label("相机配置")`、`ui.button("自动")` 全躲过去；
//! 2. **悬停提示** —— tooltip 只在指针压上去那一帧成型，静态一帧里根本没有 Shape。
//!
//! 这两类恰恰是最容易漏的：文案写在调用点上、长得和已译的外层代码一模一样。
//!
//! # 为什么不干脆全量扫「源码里的中文字面量」
//!
//! 会把 CLI 的中文别名（`"序列" | "sequence" => …`）、日志、断言说明、以及
//! `controls::SPECS` 这类**数据表**一起抽进来；数据表翻不得（见 `history` 里落账
//! 口那条注释），译了反而出错。所以这里只认**已知会把实参原样上屏**的那几个 API，
//! 而且只认**字面量直接落在文案位置**的情形：
//!
//! * `ui.label("相机配置")` —— 中招；
//! * `ui.label(crate::i18n::t!("相机配置"))` —— 放过；
//! * `ui.label(x.cn())` / `RichText::new(format!(…))` —— 放过（值不是字面量，
//!   翻译责任在取值处，由 `i18n_coverage` 与代码评审兜）。
//!
//! 换句话说：这条测试只管「调用点上写死的中文」，宁可窄，不要制造假警报。

use std::path::{Path, PathBuf};

/// 调用名 → 文案所在的参数序号（逗号分隔，从 0 数）。
///
/// 只登记「这个位置的实参会原样上屏」的 API。`Button::new` / `RichText::new`
/// 收的是整段文字，位置 0；`checkbox` / `selectable_label` 首参是状态，位置 1。
const SITES: &[(&str, usize)] = &[
    (".label(", 0),
    (".heading(", 0),
    (".link(", 0),
    (".button(", 0),
    (".small_button(", 0),
    (".on_hover_text(", 0),
    (".hint_text(", 0),
    (".checkbox(", 1),
    (".radio_value(", 1),
    (".selectable_label(", 1),
    (".selectable_value(", 1),
    (".text_edit_singleline(", 0),
    (".text_edit_multiline(", 0),
    ("RichText::new(", 0),
    ("Button::new(", 0),
    ("Button::selectable(", 1),
];

/// 返回一个有中文的实参是否已经被翻译包住。
///
/// `t!` 宏展开成 `tr("…")`，两种写法都认；`tr_fmt` 的模板本身是中文原文，也算。
fn wrapped(arg: &str) -> bool {
    arg.contains("t!(")
        || arg.contains("tr(")
        || arg.contains("tr_fmt(")
        || arg.contains("tr_preset(")
}

/// 把 `open` 处的 `(` 之后按顶层逗号切成实参区间（字节偏移，右开）。
///
/// 只做「跳过字符串里的逗号与括号」这一层：界面代码的实参不会出现跨行裸宏，
/// 但会出现 `format!("{}, {}", …)` 这种嵌套，配平必须做。
fn args(src: &str, open: usize) -> Vec<(usize, usize)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut start = open + 1;
    let mut i = open;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    out.push((start, i));
                    return out;
                }
            }
            b',' if depth == 1 => {
                out.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn line_of(src: &str, at: usize) -> usize {
    src[..at].bytes().filter(|b| *b == b'\n').count() + 1
}

#[test]
fn ui_call_sites_have_no_bare_chinese() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src_dir, &mut files);
    assert!(
        files.len() > 20,
        "源码目录没扫到东西，路径口径变了：{src_dir:?}"
    );

    let mut hits: Vec<String> = Vec::new();
    for path in &files {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        // 这两份是译文表本身，键就是中文原文，扫它没有意义。
        if matches!(name, "i18n.rs" | "i18n_en.rs") {
            continue;
        }
        let src = std::fs::read_to_string(path).expect("读源码");
        for (call, idx) in SITES {
            for (pos, _) in src.match_indices(call) {
                let open = pos + call.len() - 1;
                let Some(list) = args(&src, open).into_iter().nth(*idx).into_iter().next() else {
                    continue;
                };
                let arg = src[list.0..list.1].trim();
                let Some(body) = arg.strip_prefix('"') else {
                    continue;
                };
                let Some(text) = body.split('"').next() else {
                    continue;
                };
                if aevum_ui::i18n::needs_translation(text) {
                    hits.push(format!(
                        "{}:{}  {call}{text:?}",
                        path.strip_prefix(&src_dir).unwrap_or(path).display(),
                        line_of(&src, pos),
                    ));
                }
            }
        }
    }

    assert!(
        hits.is_empty(),
        "这些调用点写死了中文，英文模式下会直接上屏 {} 处：\n{}\n\n\
         改法：字面量套 crate::i18n::t!(\"…\")，再跑 tools/i18n_extract.py --write 补译文。",
        hits.len(),
        hits.join("\n"),
    );
}

#[test]
fn tooltips_from_data_tables_are_translated() {
    // `hint()` 是 aevum-core 的数据表：`StillFormat` / `ColorSpace` / `OutputSharpen`
    // 各一份中文说明，给面板悬停与 agent 共用。它是值不是字面量，上面那条扫不到，
    // 所以单列一条：凡把 `.hint()` 直接塞进 tooltip 的，必须过 `tr`。
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src_dir, &mut files);

    let mut hits: Vec<String> = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).expect("读源码");
        for (pos, _) in src.match_indices(".on_hover_text(") {
            let open = pos + ".on_hover_text".len();
            let Some((a, b)) = args(&src, open).first().copied() else {
                continue;
            };
            let arg = &src[a..b];
            if arg.contains(".hint()") && !wrapped(arg) {
                hits.push(format!(
                    "{}:{}  {}",
                    path.strip_prefix(&src_dir).unwrap_or(path).display(),
                    line_of(&src, pos),
                    arg.trim(),
                ));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "这些 tooltip 直接拿了数据表里的中文说明，没走翻译：\n{}",
        hits.join("\n"),
    );
}

// ---------------------------------------------------------------- 表的两端

/// 从译文表里取出全部键。
///
/// 保留源码里的转义写法（`\n` 仍是两个字符），因为下面比对的是**源码里的字面量原文**
/// ——两边同一套写法才能逐字相等，不必先反转义再比。
///
/// All keys of the translation table, kept in their source spelling (`\n` stays two
/// characters): the comparison below is against **literal text in the source**, so both
/// sides must use the same spelling and no unescaping is needed.
fn table_rows() -> Vec<String> {
    let table = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/i18n_en.rs");
    let src = std::fs::read_to_string(&table).expect("读译文表");
    let mut out = Vec::new();
    for line in src.lines() {
        let Some(rest) = line.trim_start().strip_prefix("(\"") else {
            continue;
        };
        let b = rest.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == b'"' {
                out.push(rest[..i].to_owned());
                break;
            }
            i += 1;
        }
    }
    out
}

/// 扫出源码里所有 `t!("…")` 的字面量原文及其偏移。
///
/// 只认**词边界上的** `t!(`：`format!("…")` 的尾巴长着同样的四个字符，前一个字符是
/// 标识符字符就说明它不是这个宏（这条曾让第一次扫描多出四百多条假警报）。
///
/// Every `t!("…")` literal in the source, with its offset. Only a `t!(` on a word
/// boundary counts: the tail of `format!("…")` looks identical, and a preceding identifier
/// character means it is not this macro (that mistake once produced 400+ false alarms).
fn t_literals(src: &str) -> Vec<(usize, String)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= b.len() {
        let boundary = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if boundary && &b[i..i + 4] == b"t!(\"" {
            let mut j = i + 4;
            let mut buf = String::new();
            while j < b.len() {
                if b[j] == b'\\' {
                    buf.push_str(&src[j..j + 2]);
                    j += 2;
                    continue;
                }
                if b[j] == b'"' {
                    break;
                }
                let ch = src[j..].chars().next().expect("字符边界");
                buf.push(ch);
                j += ch.len_utf8();
            }
            out.push((i, buf));
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// 每个 `t!` 字面量都必须在译文表里有一行。
///
/// # 这条补的是「已经包了 t!、但表里没有」这一半
///
/// `ui_call_sites_have_no_bare_chinese` 管的是反方向（写死了中文没包 `t!`）；
/// `i18n_coverage` 管的是「上屏的字有没有译」。两条都发现不了本文件尾部这类情况：
/// 文案已经正确地包了 `t!`，表里却漏了一行，于是英文界面回退成中文。
/// 实测它抓到过 `t!("HSL")`——一个看起来「不用译」的缩写，仍然得有一行，
/// 否则回退逻辑走的是「键不存在」，与「译文恰好等于原文」在探针里是两种状态。
///
/// # What this covers
///
/// `ui_call_sites_have_no_bare_chinese` covers the opposite direction (Chinese left
/// unwrapped) and `i18n_coverage` covers what actually reaches the screen. Neither sees the
/// case this test exists for: the call site is wrapped correctly but the table has no row,
/// so the English UI falls back to Chinese. It caught `t!("HSL")` — an acronym that looks
/// like it needs no translation but still needs a row, because "key missing" and
/// "translation equals the original" are different states to the missing-translation probe.
#[test]
fn every_t_literal_has_a_row() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let rows = table_rows();
    assert!(rows.len() > 1200, "译文表没读到东西：{} 条", rows.len());

    let mut files = Vec::new();
    rs_files(&src_dir, &mut files);
    let mut hits: Vec<String> = Vec::new();
    for path in &files {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(name, "i18n.rs" | "i18n_en.rs") {
            continue;
        }
        let src = std::fs::read_to_string(path).expect("读源码");
        for (at, key) in t_literals(&src) {
            if !rows.iter().any(|r| r == &key) {
                hits.push(format!(
                    "{}:{}  t!({key:?})",
                    path.strip_prefix(&src_dir).unwrap_or(path).display(),
                    line_of(&src, at),
                ));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "这些 t! 字面量在译文表里没有行，英文界面会回退成中文 {} 处：\n{}\n\n\
         改法：给 src/i18n_en.rs 补一行（键就是中文原文），缩写也要补。",
        hits.len(),
        hits.join("\n"),
    );
}

/// 译文表里不许有「源码里再也找不到」的孤儿行。
///
/// # 为什么孤儿也算缺陷
///
/// 面板里的长文案、MCP 工具说明是**数据表里的字符串**，翻译走 `i18n::tr(spec.summary)`
/// 这类动态查表。改文案时只改数据表、忘了改译文表，删除的那一行就变成孤儿，
/// 而新文案没有行——英文界面静默回退中文。孤儿行本身不显示，但它是「漂移发生过」的
/// 唯一痕迹，所以直接当红处理。
///
/// # Why orphans are a defect
///
/// Long panel copy and MCP tool descriptions are **strings in data tables**, translated by
/// dynamic lookups such as `i18n::tr(spec.summary)`. Edit the data table and forget the
/// translation table and the dropped row becomes an orphan while the new wording has no row
/// at all — the English UI silently falls back to Chinese. An orphan never shows on screen,
/// but it is the only trace that the drift happened, so it fails the build.
#[test]
fn no_row_is_orphaned() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in [
        "crates/aevum-ui/src",
        "crates/aevum-core/src",
        "crates/aevum-mcp/src",
        "apps/aevum-studio/src",
    ] {
        rs_files(&root.join(dir), &mut files);
    }
    assert!(files.len() > 100, "工作区没扫到源码：{:?}", root);

    let mut hay = String::new();
    for path in &files {
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n == "i18n_en.rs")
        {
            continue;
        }
        hay.push_str(&std::fs::read_to_string(path).expect("读源码"));
    }

    let dead: Vec<String> = table_rows()
        .into_iter()
        .filter(|k| !hay.contains(&format!("\"{k}\"")))
        .collect();
    assert!(
        dead.is_empty(),
        "这些译文条目在源码里找不到任何引用（文案改了没同步，英文会回退中文）{} 条：\n{}\n\n\
         改法：删掉孤儿行，并给**现在的**文案补一行。",
        dead.len(),
        dead.join("\n"),
    );
}
