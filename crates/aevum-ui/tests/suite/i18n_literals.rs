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
