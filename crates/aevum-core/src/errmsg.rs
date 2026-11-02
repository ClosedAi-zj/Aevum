//! 错误消息：把「怎么说」和「用什么语言说」分开。
//!
//! # 为什么不能让错误自己拼字符串
//!
//! 错误枚举以前用 `#[error("每像素 {0} 个采样不支持")]` 直接产出成句中文。这对日志
//! 没问题，但**界面上就翻不了**：i18n 的表以「中文原文」为键，而带参数的错误每次拼
//! 出来的整串都不一样（`每像素 3 个采样不支持` / `每像素 4 个…`），查表必然落空，
//! 于是英文界面下所有来自 core 的报错都是中文。
//!
//! 这里把一条消息拆成两部分：
//!
//! * **模板**：`&'static str`，带 `{}` 占位，是可以翻译的键；
//! * **参数**：已经格式化好的值，不翻译（数字、路径、内层错误）。
//!
//! 于是「英文怎么说」只是 i18n 表里的一行，加第三种语言不用碰 core 一行代码。
//!
//! # 单一事实源
//!
//! 每个错误类型的 [`Display`] 实现都由 [`Msg::msg`] 派生（见 [`impl_msg_display`]），
//! **不另写一份**。这样「日志里看到的那句话」和「界面上翻译的那句话」是同一个模板，
//! 不存在「改了 Display 忘了改 msg」这种漂移。
//!
//! # Why an error must not assemble its own sentence
//!
//! Error enums used to produce a finished Chinese sentence straight from
//! `#[error("...")]`. That is fine for logs but **untranslatable on screen**: the i18n
//! table is keyed by the Chinese original, and a parameterised error yields a different
//! string every time (`3 samples per pixel ...` / `4 samples ...`), so the lookup always
//! misses and every core error shows up in Chinese inside an English UI.
//!
//! A message is therefore split in two:
//!
//! * **template**: a `&'static str` with `{}` placeholders, usable as a translation key;
//! * **arguments**: already-formatted values that are not translated (numbers, paths,
//!   nested errors).
//!
//! "How do you say this in English" then becomes one row in the i18n table, and adding a
//! third language touches no core code at all.
//!
//! # Single source of truth
//!
//! Each error type's [`Display`] is derived from [`Msg::msg`] (see [`impl_msg_display`]),
//! never written twice. The sentence in the log and the sentence translated on screen come
//! from the same template, so "changed Display but forgot msg" cannot happen.

use std::fmt;

/// 模板里的一个实参。
///
/// 分两种是因为**嵌套错误要跟着翻译**：`没有可解码的原始影像：{内层}` 的内层如果直接
/// 拍平成字符串，英文界面下内层那半句还是中文。留着 `Msg` 引用，渲染时才能递归。
///
/// One argument of a template.
///
/// Two kinds, because **nested errors must be translated too**: if the inner error of
/// "no decodable raw data: {inner}" were flattened to a string, that half would stay
/// Chinese in an English UI. Keeping a `Msg` reference makes recursion possible.
pub enum Arg<'a> {
    /// 已格式化的值：数字、路径、外部库的错误文本。
    ///
    /// An already-formatted value: numbers, paths, text from an external library.
    Text(String),
    /// 嵌套的、同样可翻译的错误。
    ///
    /// A nested error that is itself translatable.
    Nested(&'a dyn Msg),
}

impl Arg<'_> {
    /// 按**给定的翻译函数**渲染。`tr` 是 `&str -> &str`，源语言下原样返回。
    ///
    /// Render through the **given translator**. `tr` maps `&str -> &str` and returns the
    /// input unchanged for the source language.
    fn render(&self, tr: &dyn Fn(&str) -> &str) -> String {
        match self {
            Arg::Text(s) => s.clone(),
            Arg::Nested(e) => e.render(tr),
        }
    }
}

/// 一条错误消息的模板与参数。
///
/// `msg` 只返回**中文模板**——那是这个项目的源语言，也是 i18n 表的键。渲染成别的语言
/// 是 [`Msg::render`] 的事，由界面层传入自己的翻译函数。
///
/// The template and arguments of one error message.
///
/// `msg` returns only the **Chinese template** -- this project's source language and the
/// key of the i18n tables. Rendering into another language is [`Msg::render`]'s job, with
/// the translator supplied by the UI layer.
pub trait Msg {
    fn msg(&self) -> (&'static str, Vec<Arg<'_>>);

    /// 用给定的翻译函数渲染整条消息（含嵌套错误，递归）。
    ///
    /// 默认实现够用；类型不需要自己覆盖。
    ///
    /// Render the whole message through the given translator, recursing into nested
    /// errors. The default implementation is enough; types do not override it.
    fn render(&self, tr: &dyn Fn(&str) -> &str) -> String {
        let (tmpl, args) = self.msg();
        fill(tr(tmpl), &args, tr)
    }
}

/// 按顺序把 `{}` 换成参数。
///
/// 参数比占位符少时保留剩余模板，比占位符多时丢掉多余的——两种都是译者或调用方的错，
/// 但界面上少一个数字远好过整窗崩掉。
///
/// Substitutes `{}` in order. Fewer arguments than placeholders keeps the remaining
/// template; more arguments are dropped. Both are mistakes by a translator or a caller, but
/// a missing number on screen is far better than a crashed window.
pub fn fill(tmpl: &str, args: &[Arg<'_>], tr: &dyn Fn(&str) -> &str) -> String {
    let mut out = String::with_capacity(tmpl.len() + 8 * args.len());
    let mut rest = tmpl;
    for arg in args {
        match rest.find("{}") {
            Some(at) => {
                out.push_str(&rest[..at]);
                out.push_str(&arg.render(tr));
                rest = &rest[at + 2..];
            }
            None => return out + rest,
        }
    }
    out.push_str(rest);
    out
}

/// `Display` 恒用源语言（中文）：日志、`anyhow` 上下文、测试断言都走这里。
///
/// 界面**不用** `Display`，用 [`Msg::render`] 配上当前语言的翻译函数——这是两者唯一
/// 的区别，也是这套设计存在的理由。
///
/// `Display` always renders in the source language (Chinese): logs, `anyhow` contexts and
/// test assertions all go through it.
///
/// The UI does **not** use `Display`; it uses [`Msg::render`] with the current language's
/// translator. That is the only difference, and the reason this design exists.
#[macro_export]
macro_rules! impl_msg_display {
    ($t:ty) => {
        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                // 源语言的翻译函数就是恒等：中文模板已经是成品。
                // 用具名 fn 而不是闭包：`&dyn Fn(&str) -> &str` 是 HRTB，
                // 闭包在这种位置上生命周期推导容易失败。
                fn identity(s: &str) -> &str {
                    s
                }
                f.write_str(&$crate::errmsg::Msg::render(self, &identity))
            }
        }
    };
}

/// 一个只打印不翻译的 `Display` 适配器，给「内层不是 `Msg`」的场合用。
///
/// 例如 `std::io::Error`：它的文本由操作系统给，本来就没有可翻译的模板。
///
/// A `Display` adapter that prints without translating, for the case where the inner value
/// is not a `Msg` -- for example `std::io::Error`, whose text comes from the OS and has no
/// translatable template to begin with.
pub struct Plain<'a>(pub &'a dyn fmt::Display);

impl Msg for Plain<'_> {
    fn msg(&self) -> (&'static str, Vec<Arg<'_>>) {
        ("{}", vec![Arg::Text(self.0.to_string())])
    }
}

/// 错误串里回显用户输入的上限（**字符**数，不是字节）。
///
/// The cap on user input echoed into an error message, in **characters**, not bytes.
pub const MAX_ECHO_CHARS: usize = 256;

/// 把要拼进错误串的用户输入截断到 [`MAX_ECHO_CHARS`] 个字符。
///
/// 为什么必须截：命令面（MCP）与面板会把参数原样拼进错误串，一条 20 KB 的路径足以把
/// agent 的上下文整段灌满（实测）。被截断时补一个 `…`，让人看得出这里少了东西。
///
/// 按 **`char`** 切而不是按 byte 切：中文与 emoji 一个字占多个字节，按字节切会切出
/// 半个 UTF-8 序列——要么 panic（`&str` 的边界检查），要么产出乱码。
///
/// Truncate user input echoed into an error message to [`MAX_ECHO_CHARS`] characters.
///
/// Why truncation is mandatory: the command surface (MCP) and the panel splice arguments
/// straight into error strings, and a single 20 KB path is enough to fill an agent's whole
/// context (measured). A trailing `…` marks that something was cut.
///
/// It cuts on **`char`** boundaries, not bytes: a Chinese character or an emoji is several
/// bytes wide, so cutting by byte would slice a UTF-8 sequence in half -- which either
/// panics (the `&str` boundary check) or yields mojibake.
pub fn clip_echo(s: &str) -> String {
    clip_echo_to(s, MAX_ECHO_CHARS)
}

/// [`clip_echo`] 的可调版本：上限由调用方给（判据要能探边界）。
///
/// [`clip_echo`] with a caller-supplied limit (the tests need to probe the boundary).
pub fn clip_echo_to(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 源语言的翻译函数：恒等。
    ///
    /// The source-language translator: identity.
    fn id(s: &str) -> &str {
        s
    }

    #[test]
    fn fill_replaces_placeholders_in_order() {
        let args = [Arg::Text("a".into()), Arg::Text("b".into())];
        assert_eq!(fill("{} 和 {}", &args, &id), "a 和 b");
        assert_eq!(fill("没有占位", &args, &id), "没有占位");
        assert_eq!(fill("{}", &args[..1], &id), "a");
    }

    /// 参数不够时保留剩余模板，多余参数丢弃——两种都是调用方或译者的错，
    /// 但界面上少一个数字远好过整窗崩掉。
    ///
    /// Too few arguments keeps the remaining template; surplus arguments are dropped.
    /// Both are caller or translator mistakes, but a missing number beats a crash.
    #[test]
    fn fill_is_forgiving_about_arity() {
        assert_eq!(fill("{} 与 {}", &[Arg::Text("a".into())], &id), "a 与 {}");
        assert_eq!(
            fill(
                "只有 {}",
                &[Arg::Text("a".into()), Arg::Text("b".into())],
                &id
            ),
            "只有 a"
        );
    }

    /// 嵌套错误要跟着翻译函数走，而不是提前拍平。
    ///
    /// Nested errors must follow the translator instead of being flattened early.
    #[test]
    fn nested_errors_are_translated_recursively() {
        struct Inner;
        impl Msg for Inner {
            fn msg(&self) -> (&'static str, Vec<Arg<'_>>) {
                ("内层", vec![])
            }
        }
        struct Outer(Inner);
        impl Msg for Outer {
            fn msg(&self) -> (&'static str, Vec<Arg<'_>>) {
                ("外层：{}", vec![Arg::Nested(&self.0)])
            }
        }
        let o = Outer(Inner);
        // 源语言：拼接原文。
        assert_eq!(o.render(&id), "外层：内层");
        // 换成「把中文换成 X」的假翻译函数：两层都要被翻。
        // 用具名 fn 而不是闭包：`&dyn Fn(&str) -> &str` 是 HRTB，闭包在这里
        // 推导不出 `for<'a>`，只能写成普通函数。
        fn fake(s: &str) -> &str {
            match s {
                "外层：{}" => "outer: {}",
                "内层" => "inner",
                other => other,
            }
        }
        assert_eq!(o.render(&fake), "outer: inner");
    }

    /// **模板里只允许 `{}`**：`{0}`、`{name}`、`{:#06x}` 这些是 `format!` 的写法，
    /// 而 [`fill`] 只认 `{}`——漏转一个，界面上就会原样打印 `{MAX_FRAMES}`。
    ///
    /// 这条判据**扫源码**而不是枚举实例：错误变体有九十个，构造实例既写不完，也会
    /// 漏掉「从未被触发」的那些。上一次就是这么漏的：`{MAX_FRAMES}` 在 thiserror 里
    /// 是隐式 format 参数，批量转换时没转成 `{}`，编译通过、测试全绿，界面上会露出来。
    ///
    /// **Only `{}` is allowed in a template**: `{0}`, `{name}` and `{:#06x}` are
    /// `format!` syntax, while [`fill`] only knows `{}`. Miss one and the UI prints
    /// `{MAX_FRAMES}` verbatim.
    ///
    /// This scans the **source** rather than enumerating instances: there are ninety
    /// variants, constructing them all is not feasible, and it would miss the ones never
    /// triggered. That is exactly how `{MAX_FRAMES}` slipped through: in thiserror it was
    /// an implicit format argument, the bulk conversion left it alone, it compiled, every
    /// test was green, and the UI would have shown it.
    #[test]
    fn clip_echo_cuts_on_char_boundaries_not_bytes() {
        // 20 KB 的路径灌进错误串会把 agent 的上下文填满；截到 256 个字符 + 省略号。
        //
        // A 20 KB path in an error string fills an agent's context; cut it at 256 characters
        // plus an ellipsis.
        let long = "路径".repeat(6000);
        let clipped = clip_echo(&long);
        assert_eq!(clipped.chars().count(), MAX_ECHO_CHARS + 1);
        assert!(clipped.ends_with('…'), "截断后要有省略号");
        assert!(
            long.starts_with(clipped.trim_end_matches('…')),
            "截断只砍尾巴"
        );
        // 短串原样返回，多字节字符一个都不许切坏。
        //
        // Short strings come back untouched, with no multi-byte character sliced.
        assert_eq!(clip_echo("成片"), "成片");
        assert_eq!(clip_echo_to("abcdef", 3), "abc…");
        for n in 0..40 {
            for unit in ["帧", "🎞", "a"] {
                let s = unit.repeat(n);
                let out = clip_echo_to(&s, 5);
                assert!(out.chars().count() <= 6);
                assert!(out.ends_with('…') == (n > 5), "n={n} 的省略号位置不对");
            }
        }
    }

    #[test]
    fn every_error_template_uses_only_brace_braces() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0usize;
        let mut bad: Vec<String> = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("读不了源码目录") {
                let path = entry.expect("目录项读不了").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("源文件读不了");
                // 小状态机而不是按行匹配：模板可能写成
                //     Self::X(v) => (
                //         "模板 {}",
                // 即 `=> (` 之后换行才出现字面量。按行找 `=> ("` 会静默漏掉它们，
                // 而「漏掉」在一条「不许出现非法占位符」的判据里就等于永远通过。
                //
                // Small state machine rather than per-line matching: a template may be
                // written as `Self::X(v) => (` followed by the literal on the next line.
                // Matching `=> ("` per line would silently skip those, and in a "no illegal
                // placeholder" assertion a skip means a permanent pass.
                // 只在 `fn msg` 的函数体里扫：`lcp.rs` / `mask.rs` 里也有
                // `format!("{make} {model}")` 这类写法，它们不是错误模板，
                // 放宽到全文件会报出一堆假的。
                //
                // Scan only inside `fn msg`: `lcp.rs` / `mask.rs` contain things like
                // `format!("{make} {model}")` that are not error templates, and widening
                // the scan to the whole file would raise a pile of false positives.
                let mut in_msg = false;
                let mut want = false;
                for line in text.lines() {
                    if line.contains("fn msg(&self)") {
                        in_msg = true;
                        want = false;
                        continue;
                    }
                    // `fn msg` 在 4 空格缩进，函数体结束是 4 空格的 `}`。
                    // `fn msg` sits at four spaces; the body ends at a four-space `}`.
                    if in_msg && line.starts_with("    }") {
                        in_msg = false;
                        want = false;
                        continue;
                    }
                    if !in_msg {
                        continue;
                    }
                    if line.contains("=> (") {
                        want = true;
                    }
                    if !want {
                        continue;
                    }
                    let Some(open) = line.find('"') else {
                        continue;
                    };
                    let rest = &line[open + 1..];
                    let Some(close) = rest.find('"') else {
                        continue;
                    };
                    let tmpl = &rest[..close];
                    want = false;
                    checked += 1;
                    let opens = tmpl.matches('{').count();
                    let pairs = tmpl.matches("{}").count();
                    if opens != pairs {
                        bad.push(format!("{}: {tmpl}", path.display()));
                    }
                }
            }
        }
        assert!(checked > 60, "只扫到 {checked} 条模板，扫描器坏了");
        assert!(
            bad.is_empty(),
            "这些模板里有不是 `{{}}` 的占位符，界面上会原样打印出来：\n{}",
            bad.join("\n")
        );
    }
}
