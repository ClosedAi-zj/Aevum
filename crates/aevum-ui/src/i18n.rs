//! 界面语言：可注册的多语言，运行时切换。
//!
//! # 语言不是写死的
//!
//! 已注册的语言放在 [`CATALOGS`] 里，每张表自带代码、自称、别名、译文、标签截断档位。
//! **加一种语言只做两件事**：写一张表（复制 `i18n_en.rs` 的骨架，用
//! `tools/i18n_extract.py --lang xx --write` 填），再往 [`CATALOGS`] 加一行。
//!
//! 语言选择器、`Lang::from_code` 的别名匹配、系统语言探测、缺译探针、覆盖率测试
//! 全部遍历注册表，没有任何一处列具体语言；`i18n_table.py` / `i18n_extract.py` /
//! `i18n_fill.py` 也都认 `--lang`，表文件路径按语言代码拼。
//!
//! UI language: a registry of languages, switchable at runtime.
//!
//! # Languages are not hard-wired
//!
//! Registered languages live in [`CATALOGS`]; each table carries its own code,
//! self-name, aliases, translations, and label-truncation width. **Adding a language
//! takes two steps**: write a table (copy the `i18n_en.rs` skeleton, fill it with
//! `tools/i18n_extract.py --lang xx --write`), then add one line to [`CATALOGS`].
//!
//! The language picker, `Lang::from_code` alias matching, system-language detection,
//! the missing-translation probe, and the coverage tests all iterate the registry and
//! none of them names a specific language; `i18n_table.py` / `i18n_extract.py` /
//! `i18n_fill.py` all accept `--lang` and build the table path from the code.
//!
//! # 为什么以中文原文作键
//!
//! 界面文案已经写死在两千多处调用点里。改用枚举键意味着每加一条文案要动三个地方
//! （枚举、中文表、英文表），而且两千多处调用点全要换成 `K::Xxx`；以原文作键只需在
//! 调用点外面包一层 [`t!`]，新增文案时补一条英文即可。
//!
//! 代价是查表靠字符串比较。用一次构建的 `HashMap` 把查找摊到 O(1)，每帧几千次查询
//! 的实测开销在 0.01 ms 量级（帧预算 16.7 ms）。**不为了这点开销换可维护性。**
//!
//! # 缺译时的行为
//!
//! 回退中文原文，不 panic、不显示空白、不显示键名。英文表漏一条的结果是那一处显示
//! 中文——难看，但用户读得懂，而且不会把界面撑坏。这是刻意的取舍：正式版里
//! 「某处没译」比「某处变成 ??? 或空字符串」轻得多。
//!
//! # 与 `format!` 的关系
//!
//! 模板本身是被翻译的对象，占位符必须一致：`t!("网格 {}")` 得到 `"Grid {}"` 之后照常
//! `format!`。测试 `placeholders_match` 会逐个核对中英两侧的 `{}` 数量，漏一个就红。
//!
//! UI language: Chinese / English, switched at runtime.
//!
//! # Why the Chinese original is the key
//!
//! UI copy is already hard-wired at over two thousand call sites. Enum keys would
//! mean three edits per new string (enum, Chinese table, English table) and would
//! force all two thousand call sites to become `K::Xxx`; keying on the original
//! only needs a [`t!`] wrapper, plus one English row for a new string.
//!
//! The cost is a string comparison per lookup. A `HashMap` built once makes that
//! O(1); a few thousand lookups per frame measured out at ~0.01 ms against a
//! 16.7 ms frame budget. **Not trading maintainability for that.**
//!
//! # Behaviour on a missing translation
//!
//! Fall back to the Chinese original: no panic, no blank, no key name on screen.
//! A gap in the English table shows Chinese in that one spot -- ugly, but
//! readable, and it cannot break the layout. Deliberate trade-off: in a release,
//! "one string untranslated" is far cheaper than "one string turned into ??? or an
//! empty box".
//!
//! # Relationship to `format!`
//!
//! The template itself is what gets translated, so placeholders must line up:
//! `t!("...{}...")` yields the English template with the same `{}`, and
//! `format!` then works as usual. The `placeholders_match` test counts `{}` on
//! both sides row by row; one gap turns it red.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 一种语言的全部资料：稳定代码、自称、别名、译文表、标签截断档位。
///
/// # 加一种语言只做两件事
///
/// 1. 复制 `i18n_en.rs` 的骨架成 `i18n_ja.rs`（同样的文件头 + `ENTRIES` 数组），
///    用脚本填表：`python tools/i18n_extract.py --lang ja --write`；
/// 2. 在本文件底部 [`CATALOGS`] 里加一行 `&JA_CATALOG`。
///
/// 除此之外**没有第三处**要改：语言选择器、`from_code` 别名匹配、系统语言探测、
/// 缺译探针、覆盖率测试全部遍历 [`CATALOGS`]，不认识具体有哪几种语言。
///
/// # 源语言是中文
///
/// 中文表是空表：键本身就是中文原文，查表等于原样返回。所有别的语言的表**以中文
/// 原文为键**，所以新增语言不需要改任何调用点——这也是「键用原文」的代价与收益的
/// 取舍点（见模块头）。
///
/// Everything one language needs: stable code, self-name, aliases, table, label
/// truncation width.
///
/// # Adding a language takes two steps
///
/// 1. Copy the `i18n_en.rs` skeleton to `i18n_ja.rs` (same header plus an `ENTRIES`
///    array) and fill it with `python tools/i18n_extract.py --lang ja --write`;
/// 2. Add one line `&JA_CATALOG` to [`CATALOGS`] at the bottom of this file.
///
/// There is no third place to touch: the language picker, `from_code` alias
/// matching, system-language detection, the missing-translation probe, and the
/// coverage tests all iterate [`CATALOGS`] and never know which languages exist.
///
/// # The source language is Chinese
///
/// The Chinese table is empty: the key is the Chinese original, so a lookup returns
/// it unchanged. Every other table is **keyed by that Chinese original**, which is
/// why adding a language touches no call site -- the trade-off explained in the
/// module header.
pub struct Catalog {
    /// 稳定代码，落进设置文件；改了等于破坏用户的设置。
    ///
    /// Stable code, stored in the settings file; changing it breaks user settings.
    pub code: &'static str,
    /// 该语言自己的写法（`中文` / `English` / `日本語`）。语言选择器直接用它。
    ///
    /// The language's own name, written in its own script (English, Japanese, and so
    /// on); the picker shows it verbatim.
    pub label: &'static str,
    /// `from_code` 额外认的写法：`zh-cn`、`en-us`、`pt-br` 之类。比较前统一小写。
    ///
    /// Extra spellings `from_code` accepts (`zh-cn`, `en-us`, `pt-br`), lowercased first.
    pub aliases: &'static [&'static str],
    /// 键值表。中文源语言为空表。
    ///
    /// The key/value table; empty for the Chinese source language.
    pub entries: &'static [(&'static str, &'static str)],
    /// 推子标签按多少**字符**截断。同一块 `LABEL_W` 宽度里，中文放得下 6 个汉字，
    /// 英文放得下约 12 个半角字符；写死一个数会把 `Highlights` 截成 `Highli…`。
    ///
    /// How many **characters** a slider label may keep. In the same `LABEL_W`, Chinese
    /// fits 6 Han characters and English about 12 half-width ones; a single hard-coded
    /// number would truncate `Highlights` to `Highli…`.
    pub ellipsis_chars: usize,
    /// 构建一次的表，`O(1)` 查询；`OnceLock` 让「第一次查」这一步线程安全。
    ///
    /// The table, built once for `O(1)` lookups; `OnceLock` makes the first build
    /// thread-safe.
    table: OnceLock<HashMap<&'static str, &'static str>>,
}

impl Catalog {
    /// 源语言（中文）：表为空，查询恒落空，`tr` 直接返回入参。
    ///
    /// The source language (Chinese): empty table, lookups always miss, and `tr`
    /// returns its argument unchanged.
    pub const fn source(
        code: &'static str,
        label: &'static str,
        aliases: &'static [&'static str],
        ellipsis_chars: usize,
    ) -> Catalog {
        Catalog {
            code,
            label,
            aliases,
            entries: &[],
            ellipsis_chars,
            table: OnceLock::new(),
        }
    }

    /// 翻译语言：键是中文字面量。
    ///
    /// A translation language; keys are Chinese literals.
    pub const fn translated(
        code: &'static str,
        label: &'static str,
        aliases: &'static [&'static str],
        ellipsis_chars: usize,
        entries: &'static [(&'static str, &'static str)],
    ) -> Catalog {
        Catalog {
            code,
            label,
            aliases,
            entries,
            ellipsis_chars,
            table: OnceLock::new(),
        }
    }

    /// 这条语言是不是「原文即译文」的源语言。
    ///
    /// Whether this is the source language where the original is already the translation.
    pub fn is_source(&self) -> bool {
        self.entries.is_empty()
    }

    /// 必须是 `&'static self`：表挂在 `Catalog` 里，而所有 `Catalog` 都是静态
    /// 常量。用 `&self` 只能拿到与借用同寿的引用，`get` 就没法返回 `&'static str`
    /// ——那样 `tr` 的返回值会被绑到一次查表的生命周期上，几百个调用点全报废。
    ///
    /// `&'static self` is required: the table lives inside `Catalog`, and every
    /// `Catalog` is a static. A plain `&self` would only yield a borrow-lived
    /// reference, so `get` could not return `&'static str` -- and `tr`'s return value
    /// would be tied to one lookup, breaking hundreds of call sites.
    fn table(&'static self) -> &'static HashMap<&'static str, &'static str> {
        self.table
            .get_or_init(|| self.entries.iter().copied().collect())
    }

    /// 查一条译文；落空返回 `None`（调用方决定回退策略）。
    ///
    /// Look one entry up; `None` on a miss (the caller decides how to fall back).
    pub fn get(&'static self, key: &str) -> Option<&'static str> {
        self.table().get(key).copied()
    }

    /// 某个键在这张表里有没有译文。
    ///
    /// Whether the table has an entry for this key.
    pub fn has(&'static self, key: &str) -> bool {
        self.table().contains_key(key)
    }
}

/// 中文：源语言。表为空是有意的，不是漏填。
///
/// Chinese: the source language. The empty table is intentional, not missing data.
pub static ZH_CATALOG: Catalog = Catalog::source(
    "zh",
    "中文",
    &["zh-cn", "zh-hans", "zh-hant", "cn", "chinese"],
    6,
);

/// 英文。别名只收地区变体：`en` 与 `English` 已经由代码与自称覆盖，再列一遍就是
/// 冗余——`registry_has_no_collisions` 会把它当冲突报出来。
///
/// English. Aliases are region variants only: `en` and `English` are already covered
/// by the code and the self-name, and listing them again is redundant --
/// `registry_has_no_collisions` reports it as a collision.
pub static EN_CATALOG: Catalog = Catalog::translated(
    "en",
    "English",
    &["en-us", "en-gb"],
    12,
    crate::i18n_en::ENTRIES,
);

/// 全部已注册语言。**加语言就在这里加一行**，顺序即语言选择器里的顺序。
///
/// Every registered language. **Adding a language means adding a line here**; the
/// order is the order shown in the language picker.
pub static CATALOGS: &[&Catalog] = &[&ZH_CATALOG, &EN_CATALOG];

/// 运行时语言：一张已注册的语言资料。
///
/// 以前是个只有 `Zh | En` 两个变体的 enum——每加一种语言都要改 enum、`label`、
/// `from_code`、`from_system`、`CURRENT` 的编码、`LangPref` 的 serde 六处。现在它
/// 只是 [`CATALOGS`] 里的一个指针，编译期就知道有哪些语言。
///
/// The runtime language: one registered catalog.
///
/// It used to be an enum with just `Zh | En`, so every new language meant touching
/// six places (the enum, `label`, `from_code`, `from_system`, the `CURRENT` encoding,
/// and `LangPref`'s serde). Now it is just a pointer into [`CATALOGS`], and which
/// languages exist is known at compile time.
#[derive(Clone, Copy)]
pub struct Lang(&'static Catalog);

// 手写而不是 derive：`Catalog` 里挂着 `OnceLock` 表和一堆字符串，derive 会要求它
// 也实现 `PartialEq`/`Debug`，而语言之间「相等」的语义只有一条——**是同一张表**。
// 按指针比正是这个语义，也顺带让 `set` 的查找与 `contains` 走同一套判定。
//
// Hand-written rather than derived: `Catalog` holds a `OnceLock` table and a pile of
// strings, and deriving would demand `PartialEq`/`Debug` on it too. The only meaning
// equality has for a language is **being the same table**, which pointer equality
// expresses exactly -- and it keeps `set`'s lookup and `contains` on one rule.
impl PartialEq for Lang {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Lang {}

impl std::fmt::Debug for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Lang").field(&self.0.code).finish()
    }
}

impl Default for Lang {
    fn default() -> Self {
        Lang::ZH
    }
}

impl Lang {
    /// 中文。名字里带 `_` 也让「这是哪一个」在调用点一眼可读。
    ///
    /// Chinese; the trailing style keeps call sites readable at a glance.
    pub const ZH: Lang = Lang(&ZH_CATALOG);
    /// 英文。
    ///
    /// English.
    pub const EN: Lang = Lang(&EN_CATALOG);

    pub const fn of(catalog: &'static Catalog) -> Lang {
        Lang(catalog)
    }

    pub fn catalog(self) -> &'static Catalog {
        self.0
    }

    /// 设置文件里存的值（小写、稳定，改字面量等于破坏用户的设置文件）。
    ///
    /// The value stored in the settings file (lowercase, stable; changing the literal
    /// breaks existing settings files).
    pub fn code(self) -> &'static str {
        self.0.code
    }

    /// 语言切换控件上显示的名字（**用该语言自己的写法**，不翻译：让只会英文的人
    /// 也能认出 "English" 那一项，是语言选择器的通行做法）。
    ///
    /// The name shown in the language switcher (**written in that language's own
    /// script**, never translated: letting an English-only reader recognise the
    /// "English" entry is what every language picker does).
    pub fn label(self) -> &'static str {
        self.0.label
    }

    /// 推子标签的截断档位（见 [`Catalog::ellipsis_chars`]）。
    ///
    /// The slider-label truncation width (see [`Catalog::ellipsis_chars`]).
    pub fn ellipsis_chars(self) -> usize {
        self.0.ellipsis_chars
    }

    /// 全部已注册语言，顺序同 [`CATALOGS`]。
    ///
    /// Every registered language, in [`CATALOGS`] order.
    pub fn all() -> impl Iterator<Item = Lang> {
        CATALOGS.iter().copied().map(Lang::of)
    }

    /// 按代码或别名还原。大小写不敏感，前后空白忽略。
    ///
    /// Resolve by code or alias; case-insensitive, surrounding whitespace ignored.
    pub fn from_code(s: &str) -> Option<Lang> {
        let want = s.trim().to_ascii_lowercase();
        if want.is_empty() {
            return None;
        }
        CATALOGS
            .iter()
            .find(|c| {
                c.code == want
                    // 自称也认：用户可能把 `LANG` 直接写成 `中文` 或 `日本語`。
                    // 只降 ASCII 大小写，汉字不受影响，等于原样比较。
                    || c.label.to_ascii_lowercase() == want
                    || c.aliases.contains(&want.as_str())
            })
            .copied()
            .map(Lang::of)
    }

    /// 读取系统界面语言。Windows 上问系统 API，其余平台（以及 API 失败时）退回
    /// `LC_ALL` / `LC_MESSAGES` / `LANG` 三个环境变量。
    ///
    /// 为什么不能只读环境变量：Windows 双击启动时它们通常不存在，于是永远退回中文；
    /// 而从终端启动时 Git/MSYS 之类的 shell 会注入 `LANG=en_US.UTF-8`，同一台中文机器
    /// 就变成英文。两者都不是「跟随系统」。系统语言优先，环境变量只是回退。
    ///
    /// 三种写法都认：`ja`、`ja-JP`、`ja_JP.UTF-8`。都不认识时退到 [`Lang::default`]
    /// （中文）——本项目的主要用户群在中文环境里，猜错的代价比反过来小。
    ///
    /// Read the system UI language. On Windows it asks the OS; on other platforms
    /// (and when the API fails) it falls back to `LC_ALL` / `LC_MESSAGES` / `LANG`.
    ///
    /// Why environment variables alone do not work: a double-click launch on Windows
    /// has none of them, so it always falls back to Chinese, while a terminal launch
    /// may inject `LANG=en_US.UTF-8` and turn the same Chinese machine English.
    /// Neither follows the system. The OS answer wins; the variables are a fallback.
    ///
    /// All three shapes are accepted: `ja`, `ja-JP`, `ja_JP.UTF-8`. When nothing is
    /// recognised the answer is [`Lang::default`] (Chinese): the project's main
    /// audience is in a Chinese environment and a wrong guess costs less that way.
    pub fn from_system() -> Lang {
        #[cfg(windows)]
        let win = windows_ui_locale();
        #[cfg(not(windows))]
        let win: Option<String> = None;
        let vars = locale_vars();
        let refs: Vec<Option<&str>> = vars.iter().map(|v| v.as_deref()).collect();
        resolve_system(win.as_deref(), &refs)
    }
}

/// 环境里那三个语言变量，按原有优先级取回。
///
/// The three language variables from the environment, in the original priority order.
fn locale_vars() -> [Option<String>; 3] {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .map(|k| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned()))
}

/// 语言码字符串 → 已注册语言。整串先试（别名带连字符），再拆主码（`ja_JP.UTF-8` → `ja`）。
///
/// 顺序不能反：反了 `zh-cn` 会先被拆成 `zh`，虽然结果一样，别名表就白建了。
/// 判定仍然只走 [`Lang::from_code`] 那张注册表，加一种语言不必碰这里。
///
/// Locale string to registered language: try the whole string first (aliases carry
/// hyphens), then the primary subtag (`ja_JP.UTF-8` -> `ja`). Reversing the order
/// would split `zh-cn` into `zh` and make the alias table pointless. Resolution
/// still goes through [`Lang::from_code`]'s registry, so adding a language never
/// touches this function.
fn from_locale(s: &str) -> Option<Lang> {
    let v = s.trim().to_ascii_lowercase();
    if let Some(l) = Lang::from_code(&v) {
        return Some(l);
    }
    let primary = v.split(['.', '_', '-']).next().unwrap_or_default();
    Lang::from_code(primary)
}

/// 把「系统 UI 语言 + 环境变量」解析成具体语言。系统优先，环境变量是回退。
///
/// 抽成纯函数是为了能在测试里注入：本机是中文系统但 shell 注入 `LANG=en_US.UTF-8`，
/// 这种组合在真实机器上才有，写成断言才拦得住「只看环境变量」的回归。
///
/// Resolve the system UI language plus environment variables into a concrete language.
/// The OS answer wins; the variables are only a fallback. A pure function so tests can
/// inject "Chinese OS + LANG=en_US.UTF-8", a combination that only occurs on a real
/// machine and is what catches a regression to environment-only detection.
fn resolve_system(windows: Option<&str>, vars: &[Option<&str>]) -> Lang {
    if let Some(l) = windows.and_then(from_locale) {
        return l;
    }
    for v in vars.iter().flatten() {
        if let Some(l) = from_locale(v) {
            return l;
        }
    }
    Lang::default()
}

/// Windows 用户默认 UI 语言的 BCP-47 名（"zh-CN" / "en-US"）。
///
/// `GetUserDefaultUILanguage` 给 LANGID，`LCIDToLocaleName` 转成 BCP-47 字符串；
/// 之后照常喂给 [`Lang::from_code`]，所以这里没有任何语言名，加语言只改注册表。
/// 两个函数都在 kernel32（系统默认链接库），不需要新依赖——`export_bench.rs` 的
/// `GetProcessTimes` 也是这么内联声明的。
///
/// The Windows user-default UI language as a BCP-47 name ("zh-CN" / "en-US").
/// `GetUserDefaultUILanguage` returns a LANGID and `LCIDToLocaleName` converts it;
/// the result goes through [`Lang::from_code`], so no language is named here.
/// Both live in kernel32 (linked by default), so there is no new dependency --
/// `export_bench.rs` declares `GetProcessTimes` the same way.
#[cfg(windows)]
fn windows_ui_locale() -> Option<String> {
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
        fn LCIDToLocaleName(lcid: u32, name: *mut u16, cch: i32, flags: u32) -> i32;
    }
    // winnt.h: LOCALE_NAME_MAX_LENGTH = 85（含结尾 NUL）。
    // winnt.h: LOCALE_NAME_MAX_LENGTH is 85, including the trailing NUL.
    const MAX: usize = 85;
    // SAFETY: 两个调用都是 kernel32 的无状态查询；buf 是长度 MAX 的 u16 数组，
    // cch 传的就是它的长度，且返回长度含 NUL，切片时减一。调用失败返回 0。
    // SAFETY: both are stateless kernel32 queries; buf is a MAX-element u16 array,
    // cch is exactly its length, and the returned length includes the NUL we drop.
    // A failed call returns 0.
    let lang_id = unsafe { GetUserDefaultUILanguage() };
    if lang_id == 0 {
        return None;
    }
    // MAKELCID(langid, SORT_DEFAULT)：SORT_DEFAULT 为 0，低 16 位原样即可。
    // MAKELCID(langid, SORT_DEFAULT): SORT_DEFAULT is 0, so the low 16 bits suffice.
    let mut buf = [0u16; MAX];
    let n = unsafe { LCIDToLocaleName(u32::from(lang_id), buf.as_mut_ptr(), MAX as i32, 0) };
    if n <= 1 || n as usize > MAX {
        return None;
    }
    let name = String::from_utf16_lossy(&buf[..n as usize - 1]);
    (!name.trim().is_empty()).then_some(name)
}

/// 设置里存的偏好：跟随系统，或钉死某一种。
///
/// 以前是 `System | Zh | En` 三个变体，serde 靠 `#[serde(rename)]` 逐个写死。现在
/// 只有「跟随系统」和「某一种已注册语言」两种状态，落盘仍是语言代码字符串——**旧
/// 设置文件照样读得回来**（`"zh"` / `"en"` / `"system"` 三个值的语义没变）。
///
/// The preference stored in settings: follow the system, or pin one language.
///
/// It used to be `System | Zh | En` with one `#[serde(rename)]` per variant. Now it
/// has just two states -- follow the system, or one registered language -- and it
/// still serialises to a language-code string, so **existing settings files keep
/// working** (`"zh"` / `"en"` / `"system"` mean exactly what they did).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LangPref {
    #[default]
    System,
    Fixed(Lang),
}

impl LangPref {
    /// 全部选项，顺序即语言选择器里的顺序：「跟随系统」打头，其余按注册表。
    ///
    /// Every option in picker order: "follow the system" first, then the registry.
    pub fn all() -> impl Iterator<Item = LangPref> {
        std::iter::once(LangPref::System).chain(Lang::all().map(LangPref::Fixed))
    }

    /// 落到实际语言。「跟随系统」每次启动解析一次，之后不再变——运行中改系统语言
    /// 不重启就跟着变，会让人以为界面在抽风。
    ///
    /// Resolve to a concrete language. "Follow the system" is resolved once per
    /// launch and then frozen: following a system-language change mid-run would make
    /// the UI look like it is glitching.
    pub fn resolve(self) -> Lang {
        match self {
            LangPref::System => Lang::from_system(),
            LangPref::Fixed(l) => l,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            LangPref::System => "system",
            LangPref::Fixed(l) => l.code(),
        }
    }

    /// 按设置文件里的值还原；认不出来就跟随系统（手改坏了不该拦住启动）。
    ///
    /// Restore from the value in the settings file; anything unrecognised means
    /// "follow the system" (a hand-edited file should not block startup).
    pub fn from_code(s: &str) -> LangPref {
        if s.trim().eq_ignore_ascii_case("system") {
            return LangPref::System;
        }
        Lang::from_code(s).map_or(LangPref::System, LangPref::Fixed)
    }
}

/// 序列化成语言代码字符串，与 `LangPref::code` / `from_code` 严格互逆。
///
/// 手写而不是 derive：`Fixed(Lang)` 里的 `Lang` 是个指针，derive 会把它序列化成
/// 一个嵌套对象，把设置文件格式改坏。这里只认字符串——**落盘格式是兼容承诺**。
///
/// Serialise to a language-code string, exactly inverse to `code` / `from_code`.
///
/// Hand-written rather than derived: `Fixed(Lang)` holds a pointer, and deriving
/// would turn it into a nested object and break the settings file format. Only a
/// string is accepted here; **the on-disk shape is a compatibility promise**.
impl serde::Serialize for LangPref {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.code())
    }
}

impl<'de> serde::Deserialize<'de> for LangPref {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(LangPref::from_code(&raw))
    }
}

/// 当前语言在 [`CATALOGS`] 里的下标。用 `AtomicUsize` 而不是 `OnceLock`：切换语言
/// 要能在运行中生效，又不能为了一个语言设置引入锁。
///
/// 存下标而不是存指针：原子类型存不了 `&'static Catalog`，而下标正好能反查回同
/// 一张表——注册表是编译期常量，下标永远有效。
///
/// The index of the current language in [`CATALOGS`]. An `AtomicUsize` rather than a
/// `OnceLock`: switching language must take effect mid-run, and a lock is too much
/// machinery for one setting.
///
/// The index, not the pointer: atomics cannot hold a `&'static Catalog`, and an index
/// maps back to the same table -- the registry is a compile-time constant, so the
/// index is always valid.
static CURRENT: AtomicUsize = AtomicUsize::new(0);

pub fn set(lang: Lang) {
    // 找不到说明传进来的是「没注册的表」，那是编程错误；退回源语言比 panic 好——
    // 语言设置出错不该让整个程序起不来。
    let idx = CATALOGS
        .iter()
        .position(|c| std::ptr::eq(*c, lang.0))
        .unwrap_or(0);
    CURRENT.store(idx, Ordering::Relaxed);
}

pub fn current() -> Lang {
    let i = CURRENT.load(Ordering::Relaxed);
    Lang::of(CATALOGS.get(i).copied().unwrap_or(&ZH_CATALOG))
}

/// 缺译探针：英文模式下查表落空的键在这里排一份。
///
/// 只在 debug 构建里存在（release 整段编译掉，`tr` 里连一个分支都不多）。
/// 有了它，「界面还有哪几条没翻译」就不再靠人眼看截图——`i18n_coverage` 测试把
/// 全部模块、全部 dock 标签、全部窗口挨个画一遍，画完读这里，空了才算过。
///
/// 用 `Mutex` 而不是 thread_local：egui 的绘制可能被测试放在别的线程上跑，
/// 而这条路径只在**落空**时才走一次，真正的运行期成本是零。
#[cfg(debug_assertions)]
mod miss {
    use std::sync::Mutex;

    static MISSES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    pub(super) fn record(key: &str) {
        let mut guard = match MISSES.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // 同一帧里同一个键会来几百次，去重后再存：失败信息要能直接照着补表。
        if !guard.iter().any(|k| k == key) {
            guard.push(key.to_owned());
        }
    }

    pub(super) fn take() -> Vec<String> {
        let mut guard = match MISSES.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *guard)
    }
}

/// 取走并清空缺译记录（debug 构建专供，release 里恒为空）。
#[cfg(debug_assertions)]
pub fn take_untranslated() -> Vec<String> {
    miss::take()
}

/// 同上，release 构建下的空实现：调用点不必写 `cfg`。
#[cfg(not(debug_assertions))]
pub fn take_untranslated() -> Vec<String> {
    Vec::new()
}

/// 某个键在**当前语言**里有没有译文。数据表的完整性测试用它——不必真的切到英文
/// 再画一遍就能问「`controls::SPECS` 的每一条标签都有译文吗」。
///
/// Whether the key has a translation in the **current** language. Used by the data
/// table completeness tests -- it answers "does every label in `controls::SPECS` have
/// a translation" without actually switching language and drawing a frame.
pub fn has(key: &str) -> bool {
    current().0.has(key)
}

/// 某个键在**指定语言**里有没有译文。
///
/// 和 [`has`] 分开是因为覆盖率测试要一次问遍所有语言，而不是只问当前这种。
///
/// Whether the key has a translation in a **given** language.
///
/// Separate from [`has`] because the coverage tests ask about every language at once,
/// not just the current one.
pub fn has_in(lang: Lang, key: &str) -> bool {
    lang.0.has(key)
}

/// 翻译一条界面文案。源语言（中文）直接返回原文（零开销），其余查表，查不到回退原文。
///
/// 参数是 `&str` 而不是 `&'static str`：界面辅助函数（`bar_button` / `kv` / `note` …）
/// 收的是借用字符串，翻译下沉到它们内部之后，几百个调用点一行都不用改。
/// 返回值的生命周期跟着入参走，而译文本身是 `'static`，可以安全地收窄。
///
/// Translate one piece of UI copy. The source language (Chinese) returns the input
/// unchanged (zero cost); every other language looks it up and falls back to the
/// original on a miss.
///
/// Takes `&str` rather than `&'static str` on purpose: the UI helpers
/// (`bar_button` / `kv` / `note` ...) take borrowed strings, and with the
/// translation pushed down inside them, hundreds of call sites need no edit. The
/// return lifetime follows the argument; the translations themselves are
/// `'static`, so narrowing is safe.
pub fn tr(s: &str) -> &str {
    // 空串是最常见的一个实参（`unit: ""` / `tip: ""` 表示「不挂」），先挡掉：
    // 既省一次查表，也免得缺译探针把「空」记成一条待翻译。
    if s.is_empty() {
        return s;
    }
    let cat = current().0;
    // 源语言没有表可查，直接走这条路：省一次哈希，也免得把每个键都记成缺译。
    if cat.is_source() {
        return s;
    }
    match cat.get(s) {
        Some(hit) => hit,
        None => {
            #[cfg(debug_assertions)]
            if needs_translation(s) {
                miss::record(s);
            }
            s
        }
    }
}

/// 把 core 的错误按**当前语言**渲染成一句话。
///
/// 错误的 [`Display`] 恒是中文（日志、`anyhow` 上下文、测试断言都靠它），界面不能直接
/// 用它——那样英文用户看到的是中文。core 的错误类型都实现了
/// `aevum_core::errmsg::Msg`，给出「模板 + 参数」；这里把模板过一遍当前语言的表，
/// 参数原样填回去。嵌套错误（例如 `MetricsError::Dng(DngError)`）递归处理。
///
/// 模板就是中文原文，与界面文案同一套键，所以**加一种语言不需要改 core 一行代码**。
///
/// Render a core error into one sentence in the **current** language.
///
/// An error's [`Display`] is always Chinese (logs, `anyhow` contexts and test assertions
/// rely on it), so the UI must not print it directly -- an English user would see Chinese.
/// Core error types implement `aevum_core::errmsg::Msg`, yielding a template plus
/// arguments; this function runs the template through the current language's table and
/// fills the arguments back in. Nested errors (for example `MetricsError::Dng(DngError)`)
/// are handled recursively.
///
/// The template is the Chinese original, the same kind of key as any UI string, so
/// **adding a language touches no core code**.
pub fn err_text(e: &dyn aevum_core::errmsg::Msg) -> String {
    e.render(&tr)
}

/// 翻译一个带 `{}` 占位的模板，再按顺序填空。
///
/// 为什么需要它：`format!` 的首参**必须是字面量**，编译器不接受函数返回值，所以
/// `format!(tr("{} 项调整"), n)` 根本编译不过。而 `format!("{} 项调整", n)` 又把中文
/// 写死在里面。中间这条路就是 `tr_fmt`——先查表拿到整条模板的译文，再自己替换 `{}`。
///
/// 顺带解决了「译文里占位符顺序可能不同」：中文「{} 帧 · 可读 {}」到英文可以是
/// `"{} frames · {} readable"`，也可以是 `"Frames: {} · readable: {}"`，由译者决定，
/// 代码不用跟着改。占位符个数由 `placeholders_match` 测试逐条核对。
///
/// 只认 `{}`。要保留小数位这类格式说明，先在调用点把数值格式化成字符串再传进来：
/// `tr_fmt("拉直 {}°", &[&format!("{deg:+.1}")])`。
///
/// Translate a template containing `{}` placeholders, then fill them in order.
///
/// Why this exists: `format!` requires a **literal** as its first argument -- the
/// compiler rejects a function's return value, so `format!(tr("...{}..."), n)`
/// does not compile. But writing the template inline hard-wires Chinese into the
/// call site. `tr_fmt` is the middle path: look up the whole template, then
/// substitute `{}` yourself.
///
/// It also solves "the translation may reorder the placeholders": a Chinese
/// sentence with two `{}` can become either `"{} frames ... {} readable"` or
/// `"Frames: {} ... readable: {}"` in English, at the translator's discretion,
/// with no code change. The `placeholders_match` test checks the count per row.
///
/// Only `{}` is supported. Format specs such as fixed decimals must be formatted
/// at the call site first: `tr_fmt("...{}...", &[&format!("{deg:+.1}")])`.
pub fn tr_fmt(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let tmpl = tr(template);
    debug_assert_eq!(
        tmpl.matches("{}").count(),
        args.len(),
        "模板 {template:?} 的占位符个数与实参对不上"
    );
    let mut out = String::with_capacity(tmpl.len() + 8 * args.len());
    let mut rest = tmpl;
    for arg in args {
        match rest.find("{}") {
            Some(at) => {
                out.push_str(&rest[..at]);
                out.push_str(&arg.to_string());
                rest = &rest[at + 2..];
            }
            // 译文里占位符比实参少（译者删了），多出来的实参丢掉比 panic 体面：
            // 界面上一句话少个数字，总好过整窗崩掉。debug 下由上面的断言拦住。
            None => return out + rest,
        }
    }
    out.push_str(rest);
    out
}

/// 预设名的**显示**文案。
///
/// `AppState::preset` 存的是标识而不是文案：`原始` / `胶片 · 富士` / `自定义` /
/// `副本 我的调色`。它会写进 `.aevum/project.json`，还要和 `PRESET_NAMES` 逐字
/// 比较——存译文的话，中文会话在读过一次英文工程之后，下拉里会冒出 "Copy x"。
///
/// 所以翻译放在这里：整串命中表就直翻（`原始`、各个胶片名），命中不了再认
/// 「副本 」这个已知前缀。副本名是用户起的，原样带过来不翻。
///
/// The **display** text for a preset name.
///
/// `AppState::preset` stores an identifier, not a label: the neutral preset, each
/// film name, the custom marker, and the copy preset built from a user name. That
/// value goes into `.aevum/project.json` and is compared verbatim against
/// `PRESET_NAMES` -- if it held a translation, a Chinese session that once opened
/// an English project would see an English copy name appear in the dropdown.
///
/// So translation happens here: a whole-string table hit is translated directly
/// (the neutral preset and the film names), otherwise the known copy prefix is
/// recognised and the rest is carried through. A copy name is user-authored and
/// never translated.
pub fn tr_preset(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("副本 ") {
        return format!("{} {}", tr("副本"), rest);
    }
    tr(s).to_owned()
}

/// 这条文案是不是「中文原文」——只有中文才需要译文。
///
/// `Windows`、`macOS`、`sRGB`、`Display P3`、`H.264`、`16:9`、`4K`、`1` 这些本来
/// 两边写法相同，不该逼着译者也造一个中文名。缺译探针靠这个谓词把「漏译」和
/// 「无需翻译」分开——否则英文界面上永远挂着几十条假警报，真漏译反而被淹掉。
///
/// 判据用中日韩统一表意文字：中文界面文案里不可能一个汉字都没有，而语言中立的
/// 名字里也不会冒出一个汉字。
///
/// Whether this string is "Chinese source text" -- only those need a translation.
///
/// `Windows`, `macOS`, `sRGB`, `Display P3`, `H.264`, `16:9`, `4K`, `1` all read
/// the same in both languages and should not force translators to invent a
/// Chinese name. The missing-translation probe uses this predicate to separate
/// "not translated yet" from "needs no translation" -- otherwise the English UI
/// would carry dozens of permanent false alarms and drown the real gaps.
///
/// The test is CJK Unified Ideographs: Chinese UI copy can never be free of Han
/// characters, and a language-neutral name never contains one.
pub fn needs_translation(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 包一层翻译。只接受字面量——动态拼出来的字符串没有稳定的键，翻不了，
/// 那种情况应该拆成「模板 + 参数」再翻译模板。
macro_rules! t {
    ($s:literal) => {
        $crate::i18n::tr($s)
    };
}
pub(crate) use t;

#[cfg(test)]
mod tests {
    use super::*;

    /// 每张非源语言的表，逐条展开成 `(语言, 键, 译文)`。
    ///
    /// 所有「整表」判据都从这里取数据，没有一条写死 `i18n_en`——加一种语言时它们
    /// 自动把新表也管上，不需要改测试。这是「不写死」的验收入口。
    ///
    /// Every non-source table, flattened to `(language, key, translation)`.
    ///
    /// All whole-table assertions take their data from here and none names `i18n_en`,
    /// so a new language is covered automatically without touching the tests. This is
    /// the acceptance hook for "nothing is hard-wired".
    fn all_entries() -> impl Iterator<Item = (&'static Catalog, &'static str, &'static str)> {
        CATALOGS
            .iter()
            .filter(|c| !c.is_source())
            .flat_map(|c| c.entries.iter().map(move |(k, v)| (*c, *k, *v)))
    }

    #[test]
    fn lang_codes_round_trip() {
        for l in Lang::all() {
            assert_eq!(
                Lang::from_code(l.code()),
                Some(l),
                "{} 的代码认不回来",
                l.code()
            );
            assert_eq!(
                Lang::from_code(l.label()),
                Some(l),
                "{} 的自称认不回来",
                l.label()
            );
        }
        assert_eq!(Lang::from_code("ZH-CN"), Some(Lang::ZH));
        assert_eq!(Lang::from_code("  EN-us  "), Some(Lang::EN));
        assert_eq!(Lang::from_code("klingon"), None);
        // 空串与只有空白：不能当成任何语言。
        assert_eq!(Lang::from_code(""), None);
        assert_eq!(Lang::from_code("   "), None);
    }

    /// 注册表自身的完整性：代码唯一、自称唯一、别名不与任何代码或别的别名撞车。
    ///
    /// 这是加语言时最容易踩的坑，而且症状很隐蔽——`from_code` 会静默返回先匹配
    /// 的那一种，用户选了日语却得到英文，界面上看不出哪里错了。所以在数据层先拦。
    ///
    /// Registry integrity: unique codes, unique labels, no alias colliding with any
    /// code or another alias.
    ///
    /// This is the easy mistake when adding a language and its symptom is subtle --
    /// `from_code` silently returns whichever matched first, so picking Japanese
    /// yields English with nothing on screen looking wrong. Caught here at the data
    /// layer instead.
    #[test]
    fn registry_has_no_collisions() {
        let mut seen: HashMap<String, &'static str> = HashMap::new();
        for c in CATALOGS {
            for name in std::iter::once(c.code)
                .chain(std::iter::once(c.label))
                .chain(c.aliases.iter().copied())
            {
                let key = name.to_ascii_lowercase();
                if let Some(prev) = seen.insert(key, c.code) {
                    panic!(
                        "{name:?} 被 {prev} 和 {} 同时占用，from_code 会静默取前者",
                        c.code
                    );
                }
            }
        }
    }

    #[test]
    fn pref_codes_round_trip() {
        for p in LangPref::all() {
            assert_eq!(LangPref::from_code(p.code()), p);
        }
        // 手改坏的值不能拦住启动。
        assert_eq!(LangPref::from_code("??? "), LangPref::System);
    }

    /// 设置文件的三个历史值语义不变：老用户的 `settings.json` 不能因为这次改造
    /// 变成「跟随系统」。这是落盘兼容承诺，不是内部细节。
    ///
    /// The three historical settings values keep their meaning: an existing
    /// `settings.json` must not silently become "follow the system". This is an
    /// on-disk compatibility promise, not an internal detail.
    #[test]
    fn settings_codes_keep_their_meaning() {
        assert_eq!(LangPref::from_code("system"), LangPref::System);
        assert_eq!(LangPref::from_code("zh"), LangPref::Fixed(Lang::ZH));
        assert_eq!(LangPref::from_code("en"), LangPref::Fixed(Lang::EN));
        assert_eq!(LangPref::Fixed(Lang::ZH).code(), "zh");
        assert_eq!(LangPref::Fixed(Lang::EN).code(), "en");
        assert_eq!(LangPref::System.code(), "system");
        // serde 走的是同一条路：存字符串，读字符串。
        let json = serde_json::to_string(&LangPref::Fixed(Lang::EN)).unwrap();
        assert_eq!(json, "\"en\"");
        assert_eq!(
            serde_json::from_str::<LangPref>("\"zh\"").unwrap(),
            LangPref::Fixed(Lang::ZH)
        );
    }

    /// 语言选择器的选项必须能落回每一种注册语言，且「跟随系统」排在第一个。
    ///
    /// The picker's options must map back to every registered language, with
    /// "follow the system" first.
    #[test]
    fn every_language_is_selectable() {
        let opts: Vec<LangPref> = LangPref::all().collect();
        assert_eq!(
            opts.first(),
            Some(&LangPref::System),
            "第一项必须是跟随系统"
        );
        assert_eq!(opts.len(), CATALOGS.len() + 1);
        for l in Lang::all() {
            assert!(
                opts.contains(&LangPref::Fixed(l)),
                "{} 在语言选择器里没有对应项",
                l.code()
            );
        }
    }

    /// 占位符必须与源语言一一对应：少一个 `{}`，`format!` 会 panic。
    /// 这条测试的价值在于「翻译是外部录入的数据」，不是代码——录入错了只有这里能拦住。
    #[test]
    fn placeholders_match() {
        for (cat, zh, tr) in all_entries() {
            let a = zh.matches("{}").count();
            let b = tr.matches("{}").count();
            assert_eq!(a, b, "{}：占位符数量不一致：{zh:?} → {tr:?}", cat.code);
        }
    }

    /// 译文里不能残留汉字。
    ///
    /// 这是「译了一半」最典型的样子：`("队列第 {}/{} 段：{}", "Queue item {}/{}: 段：{}")`，
    /// 或者把一句中文照抄进去只换了个数字占位。`entries_are_translated` 只看
    /// 译文与源文是否**完全相同**，挡不住这种「半中半英」；而界面上真出现汉字时，
    /// 只有 `i18n_coverage` 那条要画遍全界面才会红——那句文案如果只在某条命令
    /// 走完才上屏，就永远画不到。所以在数据这一层先拦一道。
    ///
    /// 例外是语言自称：日语表里 `("中文", "中文 (中国語)")` 这类是故意保留的原文形式，
    /// 只允许出现在语言选择器用到的那几条上——这里不开口子，需要时改 `SAME_OK` 的口径
    /// 而不是放宽整条判据。
    #[test]
    fn translations_have_no_chinese() {
        for (cat, zh, tr) in all_entries() {
            assert!(
                !needs_translation(tr),
                "{}：{zh:?} 的译文里还有汉字：{tr:?}",
                cat.code
            );
        }
    }

    /// 每张表里不能有重复键：`HashMap` 会静默取后者，等于悄悄丢一条翻译。
    #[test]
    fn no_duplicate_keys() {
        for cat in CATALOGS.iter().filter(|c| !c.is_source()) {
            let mut seen = HashMap::new();
            for (i, (zh, _)) in cat.entries.iter().enumerate() {
                if let Some(prev) = seen.insert(*zh, i) {
                    panic!("{} 表里 {zh:?} 出现两次（第 {prev} 与第 {i} 条）", cat.code);
                }
            }
        }
    }

    /// 表里每条键都得是非空的：空键会把「不挂 tip」这类合法用法当成一条待翻译。
    ///
    /// Every key must be non-empty: an empty key would turn legitimate "no tip"
    /// usages into translation entries.
    #[test]
    fn keys_are_not_empty() {
        for (cat, zh, _) in all_entries() {
            assert!(!zh.is_empty(), "{} 表里有一条空键", cat.code);
        }
    }

    /// 译文不该原样等于源文（除非本来就是专有名词/符号），否则多半是漏译。
    /// 白名单很小，只放确实两边写法相同的条目。
    #[test]
    fn entries_are_translated() {
        const SAME_OK: &[&str] = &[
            "Aevum",
            "English",
            "中文",
            "×",
            "—",
            "…",
            "%",
            "dB",
            "EV",
            "ISO",
            "RGB",
            "HSL",
            "LUT",
            "MCP",
            "PNG",
            "JPEG",
            "TIFF",
            "RAW",
            "DNG",
            "XMP",
            "H.264",
            "H.265",
            "sRGB",
            "Adobe RGB",
            "ProPhoto",
            "Rec.709",
            "V-Log",
            "S-Log3",
            "PQ",
            "HDR",
            "AVIF",
            "WebP",
            "GIF",
            "BMP",
        ];
        for (cat, zh, tr) in all_entries() {
            if zh == tr {
                assert!(
                    SAME_OK.contains(&zh),
                    "{}：{zh:?} 的译文与源文完全相同，是漏译还是有意的？确认后加进 SAME_OK",
                    cat.code
                );
            }
        }
    }

    /// 「跟随系统」必须真的跟随系统：系统 UI 语言优先于 shell 注入的环境变量。
    ///
    /// 本机实测过这个组合：Windows 是 zh-CN，终端里却有 `LC_ALL=C.UTF-8` 与
    /// `LANG=en_US.UTF-8`。只看环境变量的旧实现会得 EN（界面变英文），从资源管理器
    /// 双击又因变量不存在退回 ZH——同一台机器两种语言，都不等于系统语言。
    ///
    /// "Follow the system" must really follow the system: the OS UI language wins
    /// over shell-injected environment variables. Measured on this box: Windows is
    /// zh-CN while the terminal has `LC_ALL=C.UTF-8` and `LANG=en_US.UTF-8`. The old
    /// environment-only implementation returned EN there, and fell back to ZH on a
    /// double-click launch with no variables at all -- two languages on one machine.
    #[test]
    fn system_language_wins_over_shell_environment() {
        let none = [None, None, None];
        assert_eq!(resolve_system(Some("en-US"), &none), Lang::EN);
        assert_eq!(resolve_system(Some("zh-CN"), &none), Lang::ZH);
        assert_eq!(resolve_system(Some("  ZH-hans-CN  "), &none), Lang::ZH);
        // 关键回归：中文系统 + 英文 shell 变量，必须仍是中文。
        // The regression that matters: Chinese OS plus an English shell stays Chinese.
        assert_eq!(
            resolve_system(Some("zh-CN"), &[Some("C.UTF-8"), None, Some("en_US.UTF-8")]),
            Lang::ZH
        );
        // 未注册的语言不猜，回退源语言。
        // An unregistered language is not guessed: fall back to the source language.
        assert_eq!(resolve_system(Some("ja-JP"), &none), Lang::ZH);
        // 非 Windows / 系统 API 失败时，环境变量仍是有效回退。
        // Without an OS answer the environment variables remain a valid fallback.
        assert_eq!(
            resolve_system(None, &[None, None, Some("en_US.UTF-8")]),
            Lang::EN
        );
        assert_eq!(resolve_system(None, &none), Lang::ZH);
    }

    /// `from_system` 真的接了 Windows 分支，而不是只留着纯函数好看。
    ///
    /// 把调用删掉（等价于回退到旧实现）时，本机「系统 zh-CN + LANG=en_US.UTF-8」
    /// 会让左边得 EN、右边得 ZH，这条当场红。判据不断言系统具体是哪种语言，
    /// 所以在英文 runner 上也成立。
    ///
    /// `from_system` really calls the Windows branch rather than leaving the pure
    /// function unused. Delete the call (i.e. restore the old implementation) and
    /// on this box the left side becomes EN while the right stays ZH, so this fails.
    /// Nothing here assumes which language the OS uses, so it holds on an English
    /// runner too.
    #[test]
    fn from_system_really_reads_the_windows_ui_language() {
        #[cfg(windows)]
        let win = windows_ui_locale();
        #[cfg(not(windows))]
        let win: Option<String> = None;
        let vars = locale_vars();
        let refs: Vec<Option<&str>> = vars.iter().map(|v| v.as_deref()).collect();
        assert_eq!(Lang::from_system(), resolve_system(win.as_deref(), &refs));
        #[cfg(windows)]
        {
            assert!(
                win.as_deref().is_some_and(|s| !s.trim().is_empty()),
                "Windows UI 语言查询不该返回空"
            );
        }
    }
}
