//! 错误消息必须跟着界面语言走。
//!
//! # 这条判据在防什么
//!
//! core 的错误用 `Display` 产出**成句中文**，这对日志是对的，对界面是错的：英文用户
//! 会看到 `Export failed: 每像素 3 个采样不支持（只认单通道原始数据）`——一半英文一半
//! 中文。原因是 i18n 的表以「中文原文」为键，而带参数的错误每次拼出来的整串都不同，
//! 查表必然落空。
//!
//! 修法是把错误拆成「模板 + 参数」：模板是 `&'static str`，可以当键；参数是值，不翻。
//! 这条判据验证的是**整条链真的通了**——表里有数据不等于渲染路径接对了。
//!
//! # What this guards against
//!
//! Core errors produce a **finished Chinese sentence** through `Display`. That is right
//! for logs and wrong for the UI: an English user would see
//! `Export failed: 每像素 3 个采样不支持（只认单通道原始数据）` -- half English, half
//! Chinese. The cause is that the i18n table is keyed by the Chinese original, and a
//! parameterised error yields a different string every time, so the lookup always misses.
//!
//! The fix splits an error into "template plus arguments": the template is a
//! `&'static str` usable as a key, the arguments are values and are not translated. This
//! test verifies the **whole chain actually works** -- a populated table does not prove
//! the render path is wired up.

use aevum_core::dng::DngError;
use aevum_core::metrics::MetricsError;
use aevum_ui::i18n::{self, Lang};
use std::sync::{Mutex, MutexGuard};

/// 当前语言是一个全局原子（`i18n::current()`），cargo 默认并行跑同一个文件里的测试：
/// 一条刚 `set(EN)`，另一条已经 `set(ZH)`，断言就会读到别人的语言。这不是被测代码的
/// 缺陷，是判据自己没串行——所以必须先拿锁，再改语言。
///
/// 之前这里没锁，`nested_core_errors_are_translated_at_every_level` 偶发报「内层还是
/// 中文」；单独跑必过、跑整个文件必挂，就是并行争用的典型症状。
///
/// `Drop` 里复位成中文：一条判据失败时后面的判据不能继承一个半路改过的语言状态。
///
/// The current language is one global atomic (`i18n::current()`) and cargo runs tests in
/// the same file in parallel: one has just `set(EN)` while another already `set(ZH)`, so
/// an assertion can read someone else's language. That is not a defect in the code under
/// test -- the test simply is not serialised. Take the lock before touching the language.
struct LangGuard {
    _lock: MutexGuard<'static, ()>,
}

static LANG_LOCK: Mutex<()> = Mutex::new(());

impl LangGuard {
    fn new(lang: Lang) -> LangGuard {
        let lock = LANG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        i18n::set(lang);
        LangGuard { _lock: lock }
    }

    fn switch(&self, lang: Lang) {
        i18n::set(lang);
    }
}

impl Drop for LangGuard {
    fn drop(&mut self) {
        i18n::set(Lang::ZH);
    }
}

/// 英文界面下一条带参数的 core 错误必须整句英文。
///
/// 只断言「不含汉字」不够：它会放过「整句没翻译但恰好没汉字」的情况。这里**同时**
/// 断言与中文版本的差异和具体译文，三者一起才说明「模板过表、参数填空」这条路走通了。
///
/// A parameterised core error must be all English in an English UI.
///
/// Asserting "contains no Chinese" alone is not enough -- it would pass a sentence that
/// was never translated but happens to have no Han characters. Here the difference from
/// the Chinese version **and** the exact translation are asserted together; only all three
/// show that "template through the table, arguments filled in" really works.
#[test]
fn a_parameterised_core_error_renders_fully_in_english() {
    let g = LangGuard::new(Lang::ZH);
    let e = DngError::UnsupportedSamples(3);

    let zh = i18n::err_text(&e);
    assert!(
        zh.contains('3'),
        "中文渲染丢了参数：{zh}（模板与参数没接上）"
    );
    assert!(zh.contains("采样"), "中文渲染不像原文：{zh}");

    g.switch(Lang::EN);
    let en = i18n::err_text(&e);
    assert_ne!(en, zh, "英文渲染与中文完全相同：模板没走表");
    assert!(
        !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "英文界面下错误里还有汉字：{en}"
    );
    assert!(en.contains('3'), "英文渲染丢了参数：{en}");
    assert!(
        en.contains("samples per pixel"),
        "英文渲染不是表里那条译文：{en}"
    );
}

/// 不含参数的错误也要翻——它们是最容易被漏掉的一类（没有 `{}`，看不出「模板」）。
///
/// Argument-free errors must be translated too; they are the easiest to miss because
/// without a `{}` nothing looks like a template.
#[test]
fn an_argument_free_core_error_is_translated() {
    let _g = LangGuard::new(Lang::EN);
    let en = i18n::err_text(&DngError::UnsupportedCfa);
    assert!(
        !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "英文界面下错误里还有汉字：{en}"
    );
    assert!(en.contains("CFA"), "英文渲染不是表里那条译文：{en}");
}

/// 嵌套错误要**两层都翻**。
///
/// `MetricsError::Dng(..)` 的模板是「DNG 解码失败：{}」，内层是另一个可翻译错误。
/// 如果实现把内层提前拍平成字符串，外层会变英文、内层仍是中文——这种「半句翻」在
/// 界面上比整句中文更糟，因为看起来像是翻译坏了。
///
/// Nested errors must be translated at **both** levels.
///
/// `MetricsError::Dng(..)`'s template is "DNG decode failed: {}" and the inner value is
/// another translatable error. If the implementation flattens the inner one early, the
/// outer part becomes English while the inner stays Chinese -- half a sentence is worse
/// than a fully Chinese one, because it reads like the translation is broken.
#[test]
fn nested_core_errors_are_translated_at_every_level() {
    let _g = LangGuard::new(Lang::EN);
    let e = MetricsError::Dng(DngError::UnsupportedSamples(3));
    let en = i18n::err_text(&e);
    assert!(
        !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "嵌套错误的内层没被翻（外层翻了、内层还是中文）：{en}"
    );
    assert!(en.to_lowercase().contains("dng"), "外层模板没走表：{en}");
    assert!(en.contains("samples per pixel"), "内层模板没走表：{en}");
}

/// 切回中文时不能残留英文：`err_text` 必须每次按当前语言重算，不能缓存成第一次的结果。
///
/// Switching back to Chinese must leave no English behind: `err_text` has to recompute per
/// call rather than caching the first result.
#[test]
fn switching_language_back_restores_chinese() {
    let g = LangGuard::new(Lang::EN);
    let e = DngError::UnsupportedSamples(3);
    let en = i18n::err_text(&e);
    assert!(
        !en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "前置条件不成立（英文没生效）：{en}"
    );
    g.switch(Lang::ZH);
    let zh = i18n::err_text(&e);
    assert!(
        zh.contains("采样"),
        "切回中文后渲染的不是中文：{zh}（可能缓存了英文）"
    );
}

/// core 里**每一条**错误模板在**每一张**语言表里都得有译文。
///
/// 前面四条只验了两三个错误，证明的是「这条路能通」；这一条证明「所有错误都在路上」。
///
/// 为什么必须扫源码而不是枚举实例：错误变体有九十个，一半要靠构造特定的坏文件才会
/// 触发，写不出实例也写不全。上面注释里那个 `{MAX_FRAMES}` 就是这么漏的——编译通过、
/// 测试全绿，界面上原样打印占位符。
///
/// 为什么放在 `aevum-ui` 而不是 core：模板是 core 的事，语言表是 ui 的事，只有这一层
/// 同时看得见两边。代价是要按相对路径去读 core 的源码目录，所以目录不存在时**直接
/// 失败**而不是静默跳过——一条「什么都没检查」的判据比没有更坏。
///
/// Every **single** core error template must have a translation in **every** language
/// table.
///
/// The four tests above cover two or three errors and prove the path works; this one
/// proves *all* errors are on that path.
///
/// Scanning the source instead of enumerating instances is necessary because there are
/// about ninety variants and half of them only trigger on a specifically malformed file.
/// That is exactly how `{MAX_FRAMES}` slipped through: it compiled, every test was green,
/// and the UI printed the placeholder verbatim.
#[test]
fn every_core_error_template_is_translated_in_every_catalog() {
    let templates = core_error_templates();
    assert!(
        templates.len() > 60,
        "只扫到 {} 条模板，扫描器坏了（不是模板变少了）",
        templates.len()
    );
    // 扫描器自检：得先证明它真能抓到一条**已知的**模板，否则「一条都没缺」可能只是
    // 「一条都没抓到」。上面那个条数下限防的是同一件事，这条防的是「抓到的全是别的」。
    assert!(
        templates
            .iter()
            .any(|(_, t)| t == "每像素 {} 个采样不支持（只认单通道原始数据）"),
        "扫描器没抓到已知模板，抓到的 {} 条都不作数",
        templates.len()
    );

    let mut missing: Vec<String> = Vec::new();
    for (file, tmpl) in &templates {
        // `{}` 这种纯占位模板没有可翻的字，`needs_translation` 会放过。
        if !i18n::needs_translation(tmpl) {
            continue;
        }
        for l in Lang::all() {
            if l.catalog().is_source() {
                continue;
            }
            if !i18n::has_in(l, tmpl) {
                missing.push(format!("    ({tmpl:?}, \"\"),  // {file}"));
            }
        }
    }
    missing.sort_unstable();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "core 还有 {} 条错误模板没译文，界面上一抛错就是中文：\n{}",
        missing.len(),
        missing.join("\n")
    );
}

/// 把 core 里 `fn msg` 函数体中的所有模板字面量抓出来，返回 `(文件, 模板)`。
///
/// 小状态机而不是按行匹配：模板可能写成
/// ```ignore
/// Self::X(v) => (
///     "模板 {}",
/// ```
/// 即 `=> (` 之后换行才出现字面量，按行找 `=> ("` 会静默漏掉。也只在 `fn msg` 体内扫：
/// `lcp.rs` / `mask.rs` 里还有 `format!("{make} {model}")` 这类非错误模板的写法。
///
/// Collects every template literal inside `fn msg` bodies in core as `(file, template)`.
///
/// A small state machine rather than per-line matching, because a template may be written
/// as `Self::X(v) => (` with the literal on the following line; and only `fn msg` bodies
/// are scanned, since `lcp.rs` / `mask.rs` contain `format!("{make} {model}")` and other
/// non-error templates.
fn core_error_templates() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("aevum-core")
        .join("src");
    assert!(
        root.is_dir(),
        "找不到 core 源码目录：{}（这条判据要读它，不能跳过）",
        root.display()
    );

    let mut out: Vec<(String, String)> = Vec::new();
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
            let file = path
                .strip_prefix(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("..")
                        .join("aevum-core"),
                )
                .unwrap_or(&path)
                .display()
                .to_string();
            let mut in_msg = false;
            let mut want = false;
            for line in text.lines() {
                if line.contains("fn msg(&self)") {
                    in_msg = true;
                    want = false;
                    continue;
                }
                // `fn msg` 在 4 空格缩进，函数体结束是 4 空格的 `}`。
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
                want = false;
                out.push((file.clone(), rest[..close].to_string()));
            }
        }
    }
    out
}
