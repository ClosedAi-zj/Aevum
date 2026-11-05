//! 发布说明的轻量 Markdown 解析：最小闭集，零新依赖，纯函数可单测。
//!
//! 只认二级/三级标题、无序列表、行内样式（加粗 / 行内代码 / 链接）。
//! 表格、图片、HTML 标签、代码块、四级及以上标题、嵌套列表一律降级为原样正文，
//! 不吞字、不 panic、不 unwrap——渲染层拿到的一定是「用户能看见原文」的行。
//!
//! Lightweight Markdown parsing for release notes: a minimal closed set, zero new
//! dependencies, and a pure function that is easy to unit-test.
//!
//! Only level-2/3 headings, unordered lists and inline styles (bold / inline code /
//! link) are recognised. Tables, images, HTML tags, code blocks, level-4+ headings and
//! nested lists all degrade to verbatim body text -- nothing is swallowed, nothing
//! panics, nothing unwraps, so the render layer always receives visible source text.

/// 一行发布说明的语义类别。
///
/// One semantic line of release notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoteLine {
    /// 二级或三级标题；参数是层级（2 或 3）与去掉标记的标题文字。
    ///
    /// A level-2 or level-3 heading: the level (2 or 3) plus the text with markers
    /// stripped.
    Heading(u8, String),
    /// 无序列表项（行首 `-` / `*` / `+` 已去掉），携带行内样式片段。
    ///
    /// An unordered-list item (the leading `-` / `*` / `+` is stripped), carrying inline
    /// style runs.
    Bullet(Vec<Span>),
    /// 普通正文行，携带行内样式片段。
    ///
    /// A plain body line carrying inline style runs.
    Body(Vec<Span>),
    /// 空行。
    ///
    /// A blank line.
    Blank,
}

/// 行内样式片段：正文与列表项共用。
///
/// An inline style run shared by body lines and list items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Span {
    /// 无样式文字。
    ///
    /// Unstyled text.
    Text(String),
    /// 加粗文字（`**…**`）。
    ///
    /// Bold text (`**...**`).
    Bold(String),
    /// 行内代码（反引号包裹）。
    ///
    /// Inline code (backtick-delimited).
    Code(String),
    /// 链接 `[文本](url)`：文字与目标分开保存，渲染时目标只做悬停提示。
    ///
    /// A link `[text](url)`: label and target kept apart; the target is only a hover
    /// hint when rendered.
    Link { text: String, url: String },
}

/// 把一段发布说明源码解析成可渲染的行序列。
///
/// 空输入与「解析不出任何行」都回退为单条空正文，调用方不必再判空。
///
/// Parse release-note source into a renderable sequence of lines.
///
/// Empty input and "no lines parsed at all" both fall back to one empty body line, so
/// the caller never has to special-case emptiness.
pub(crate) fn parse_release_notes(src: &str) -> Vec<NoteLine> {
    let mut out: Vec<NoteLine> = Vec::new();
    // 是否位于 ``` / ~~~ 代码块内：块内所有行都按正文原样输出。
    //
    // Whether we are inside a ``` / ~~~ fence: every line inside is emitted verbatim.
    let mut in_code = false;

    for raw in src.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();

        // 围栏行本身与块内所有行都不解析：整行原样正文。
        //
        // Fence lines and every line inside a fence are not parsed: emitted verbatim.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            out.push(NoteLine::Body(verbatim(line)));
            continue;
        }
        if in_code {
            out.push(NoteLine::Body(verbatim(line)));
            continue;
        }

        if trimmed.is_empty() {
            out.push(NoteLine::Blank);
            continue;
        }

        // 超出最小闭集的语法：整行原样正文（不解析行内样式，但一个字都不丢）。
        //
        // Syntax outside the minimal closed set: the whole line becomes verbatim body
        // text (inline styles are not parsed, but not a single character is dropped).
        if falls_back(line) {
            out.push(NoteLine::Body(verbatim(line)));
            continue;
        }

        // 三级标题先判（`### ` 也能被 `## ` 的前缀匹配误伤）。
        //
        // Check level-3 first: `### ` would otherwise be captured by the `## ` prefix.
        if let Some(rest) = line.strip_prefix("### ") {
            out.push(NoteLine::Heading(3, rest.trim().to_owned()));
            continue;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            out.push(NoteLine::Heading(2, rest.trim().to_owned()));
            continue;
        }

        if let Some(rest) = bullet_text(line) {
            out.push(NoteLine::Bullet(parse_spans(rest)));
            continue;
        }

        out.push(NoteLine::Body(parse_spans(line)));
    }

    if out.is_empty() {
        out.push(NoteLine::Body(Vec::new()));
    }
    out
}

/// 整行作为一个无样式片段（超出闭集时的降级出口）。
///
/// The whole line as one unstyled run (the fallback for out-of-scope syntax).
fn verbatim(line: &str) -> Vec<Span> {
    vec![Span::Text(line.to_owned())]
}

/// 该行是否属于「不解析、整行原样输出」的语法。
///
/// Whether the line belongs to syntax that is left verbatim and not parsed.
fn falls_back(line: &str) -> bool {
    let trimmed = line.trim_start();

    // 1 级标题与 4 级及以上标题：闭集只有 2/3 级。
    //
    // Level-1 and level-4+ headings: the closed set only has levels 2 and 3.
    if trimmed.starts_with('#') {
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if !(2..=3).contains(&hashes) && trimmed.chars().nth(hashes) == Some(' ') {
            return true;
        }
    }

    // 缩进的嵌套列表：只认行首零缩进的列表。
    //
    // Indented nested lists: only zero-indent list markers are recognised.
    let indented = line.trim_start_matches([' ', '\t']);
    if indented.len() != line.len()
        && (indented.starts_with("- ") || indented.starts_with("* ") || indented.starts_with("+ "))
    {
        return true;
    }

    // 表格：以竖线开头的行。
    //
    // Tables: lines that start with a pipe.
    if trimmed.starts_with('|') || trimmed.ends_with('|') {
        return true;
    }

    // 图片：`![alt](url)`。
    //
    // Images: `![alt](url)`.
    if line.contains("![") {
        return true;
    }

    // HTML 标签：`<tag …>` / `</tag>`。
    //
    // HTML tags: `<tag ...>` / `</tag>`.
    if has_html_tag(line) {
        return true;
    }

    false
}

/// 行内是否出现像 HTML 标签的一对尖括号（`<` 后紧跟字母或 `/`，后面有 `>`）。
///
/// Whether the line contains something shaped like an HTML tag (`<` followed by a
/// letter or `/`, with a `>` later on).
fn has_html_tag(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            if j < bytes.len() && bytes[j] == b'/' {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_alphabetic() && line[j..].contains('>') {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// 行首零缩进的 `- ` / `* ` / `+ `：返回标记之后的文本。
///
/// A zero-indent `- ` / `* ` / `+ ` at the start of the line: returns the text after
/// the marker.
fn bullet_text(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    if bytes.len() < 2 || bytes[0].is_ascii_whitespace() {
        return None;
    }
    if !matches!(bytes[0], b'-' | b'*' | b'+') || bytes[1] != b' ' {
        return None;
    }
    Some(&line[2..])
}

/// 把一行文本切成 `Text` / `Bold` / `Code` / `Link` 片段。
///
/// 任何未闭合的标记都原样退回 `Text`，保证不吞字。
///
/// Split one line into `Text` / `Bold` / `Code` / `Link` runs.
///
/// Any marker that never closes falls back to `Text` verbatim, so no character is
/// swallowed.
fn parse_spans(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // 行内代码：`…`
        //
        // Inline code: `...`
        if c == '`'
            && let Some(end) = find_char(&chars, i + 1, '`')
        {
            flush_text(&mut out, &mut buf);
            out.push(Span::Code(chars[i + 1..end].iter().collect()));
            i = end + 1;
            continue;
        }

        // 加粗：`**…**`
        //
        // Bold: `**...**`
        if c == '*'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
            && let Some(end) = find_pair(&chars, i + 2, '*', '*')
        {
            flush_text(&mut out, &mut buf);
            out.push(Span::Bold(chars[i + 2..end].iter().collect()));
            i = end + 2;
            continue;
        }

        // 链接：`[文本](url)`
        //
        // Link: `[text](url)`
        if c == '['
            && let Some((text_end, url_end)) = find_link(&chars, i)
        {
            flush_text(&mut out, &mut buf);
            out.push(Span::Link {
                text: chars[i + 1..text_end].iter().collect(),
                url: chars[text_end + 2..url_end].iter().collect(),
            });
            i = url_end + 1;
            continue;
        }

        buf.push(c);
        i += 1;
    }

    flush_text(&mut out, &mut buf);
    if out.is_empty() {
        out.push(Span::Text(String::new()));
    }
    out
}

/// 把累积的普通文字压入结果，并清空缓冲。
///
/// Push accumulated plain text into the result and clear the buffer.
fn flush_text(out: &mut Vec<Span>, buf: &mut String) {
    if !buf.is_empty() {
        out.push(Span::Text(std::mem::take(buf)));
    }
}

/// 从 `from` 起找第一个 `target` 的下标。
///
/// Find the index of the first `target` at or after `from`.
fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&k| chars[k] == target)
}

/// 从 `from` 起找第一对相邻的 `a` `b`，返回 `a` 的下标。
///
/// Find the first adjacent `a` `b` pair at or after `from`, returning the index of `a`.
fn find_pair(chars: &[char], from: usize, a: char, b: char) -> Option<usize> {
    let mut k = from;
    while k + 1 < chars.len() {
        if chars[k] == a && chars[k + 1] == b {
            return Some(k);
        }
        k += 1;
    }
    None
}

/// 解析 `[文本](url)`：返回 `]` 与 `)` 的下标。
///
/// Parse `[text](url)`: returns the indices of `]` and `)`.
fn find_link(chars: &[char], open: usize) -> Option<(usize, usize)> {
    let text_end = find_char(chars, open + 1, ']')?;
    if text_end + 1 >= chars.len() || chars[text_end + 1] != '(' {
        return None;
    }
    let url_end = find_char(chars, text_end + 2, ')')?;
    Some((text_end, url_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[Span]) -> String {
        spans
            .iter()
            .map(|s| match s {
                Span::Text(t) | Span::Bold(t) | Span::Code(t) => t.clone(),
                Span::Link { text, .. } => text.clone(),
            })
            .collect()
    }

    #[test]
    fn headings_bullets_and_body_are_classified() {
        let lines = parse_release_notes("## 下载\n### 修复\n- 第一条\n普通正文");
        assert!(matches!(&lines[0], NoteLine::Heading(2, _)));
        assert!(matches!(&lines[1], NoteLine::Heading(3, _)));
        assert!(matches!(&lines[2], NoteLine::Bullet(_)));
        assert!(matches!(&lines[3], NoteLine::Body(_)));
    }

    #[test]
    fn inline_styles_are_split() {
        let lines = parse_release_notes("前 **粗** 中 `码` 后 [链](https://e.x)");
        if let NoteLine::Body(spans) = &lines[0] {
            assert!(spans.iter().any(|s| matches!(s, Span::Bold(_))));
            assert!(spans.iter().any(|s| matches!(s, Span::Code(_))));
            assert!(spans.iter().any(|s| matches!(s, Span::Link { .. })));
        } else {
            panic!("应为正文行");
        }
    }

    #[test]
    fn out_of_scope_syntax_is_kept_verbatim() {
        for src in [
            "#### 四级标题",
            "| a | b |",
            "![图](x.png)",
            "<div>原文</div>",
            "  - 嵌套列表",
            "```rust",
            "~~~",
        ] {
            let lines = parse_release_notes(src);
            let joined = match &lines[0] {
                NoteLine::Body(spans) => plain(spans),
                other => format!("{other:?}"),
            };
            assert_eq!(joined, src, "超出语法的行必须逐字保留：{src:?}");
        }
    }

    #[test]
    fn empty_input_falls_back_to_one_body() {
        let lines = parse_release_notes("");
        assert_eq!(lines.len(), 1);
        assert!(matches!(&lines[0], NoteLine::Body(_)));
    }

    #[test]
    fn unclosed_markers_do_not_eat_text() {
        let lines = parse_release_notes("未闭合 `代码 与 **粗体");
        if let NoteLine::Body(spans) = &lines[0] {
            assert_eq!(plain(spans), "未闭合 `代码 与 **粗体");
        } else {
            panic!("应为正文行");
        }
    }
}
