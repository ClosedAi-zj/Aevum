//! 界面语言：中文 / English，运行时切换。
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

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// 运行时语言。只有这两种；「跟随系统」是设置里的偏好，解析后才落到这里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Zh,
    En,
}

impl Lang {
    /// 设置文件里存的值（小写、稳定，改字面量等于破坏用户的设置文件）。
    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    pub fn from_code(s: &str) -> Option<Lang> {
        match s.trim().to_ascii_lowercase().as_str() {
            "zh" | "zh-cn" | "zh-hans" | "cn" | "chinese" => Some(Lang::Zh),
            "en" | "en-us" | "en-gb" | "english" => Some(Lang::En),
            _ => None,
        }
    }

    /// 语言切换控件上显示的名字（**用该语言自己的写法**，不翻译：让只会英文的人
    /// 也能认出 "English" 那一项，是语言选择器的通行做法）。
    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "English",
        }
    }

    /// 读取系统语言。Windows 上 `LANG` 常常不存在，所以再看 `LC_ALL` / `LC_MESSAGES`；
    /// 都读不到时按英文处理——猜错成英文的用户至少能靠 "English" 那一项切回来。
    pub fn from_system() -> Lang {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Some(v) = std::env::var_os(key) {
                let v = v.to_string_lossy().to_ascii_lowercase();
                if v.starts_with("zh") {
                    return Lang::Zh;
                }
                if v.starts_with("en") {
                    return Lang::En;
                }
            }
        }
        // Windows 的语言由 GetUserDefaultUILanguage 决定，标准库不暴露；查环境变量
        // 之后退到「中文」而不是「英文」：本项目的主要用户群在中文环境里，猜错的
        // 代价比反过来小。
        Lang::Zh
    }
}

/// 设置里存的偏好：跟随系统，或钉死某一种。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LangPref {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh")]
    Zh,
    #[serde(rename = "en")]
    En,
}

impl LangPref {
    /// 落到实际语言。「跟随系统」每次启动解析一次，之后不再变——运行中改系统语言
    /// 不重启就跟着变，会让人以为界面在抽风。
    pub fn resolve(self) -> Lang {
        match self {
            LangPref::System => Lang::from_system(),
            LangPref::Zh => Lang::Zh,
            LangPref::En => Lang::En,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            LangPref::System => "system",
            LangPref::Zh => "zh",
            LangPref::En => "en",
        }
    }

    /// 按设置文件里的值还原；认不出来就跟随系统（手改坏了不该拦住启动）。
    pub fn from_code(s: &str) -> LangPref {
        match s.trim().to_ascii_lowercase().as_str() {
            "zh" => LangPref::Zh,
            "en" => LangPref::En,
            _ => LangPref::System,
        }
    }
}

/// 当前语言。用 `AtomicU8` 而不是 `OnceLock`：切换语言要能在运行中生效，
/// 又不能为了一个字体设置引入锁。
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set(lang: Lang) {
    CURRENT.store(
        match lang {
            Lang::Zh => 0,
            Lang::En => 1,
        },
        Ordering::Relaxed,
    );
}

pub fn current() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Zh,
    }
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

/// 某个键在英文表里有没有译文。数据表的完整性测试用它——不必真的切到英文
/// 再画一遍就能问「`controls::SPECS` 的每一条标签都有英文吗」。
pub fn has(key: &str) -> bool {
    table().contains_key(key)
}

/// 翻译一条界面文案。中文直接返回原文（零开销），英文查表，查不到回退中文。
///
/// 参数是 `&str` 而不是 `&'static str`：界面辅助函数（`bar_button` / `kv` / `note` …）
/// 收的是借用字符串，翻译下沉到它们内部之后，几百个调用点一行都不用改。
/// 返回值的生命周期跟着入参走，而英文值本身是 `'static`，可以安全地收窄。
pub fn tr(s: &str) -> &str {
    // 空串是最常见的一个实参（`unit: ""` / `tip: ""` 表示「不挂」），先挡掉：
    // 既省一次查表，也免得缺译探针把「空」记成一条待翻译。
    if s.is_empty() {
        return s;
    }
    match current() {
        Lang::Zh => s,
        Lang::En => match table().get(s).copied() {
            Some(en) => en,
            None => {
                #[cfg(debug_assertions)]
                if needs_translation(s) {
                    miss::record(s);
                }
                s
            }
        },
    }
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

/// 这条文案是不是「中文原文」——只有中文才需要译文。
///
/// `Windows`、`macOS`、`sRGB`、`Display P3`、`H.264`、`16:9`、`4K`、`1` 这些本来
/// 两边写法相同，不该逼着译者也造一个中文名。缺译探针靠这个谓词把「漏译」和
/// 「无需翻译」分开——否则英文界面上永远挂着几十条假警报，真漏译反而被淹掉。
///
/// 判据用中日韩统一表意文字：中文界面文案里不可能一个汉字都没有，而语言中立的
/// 名字里也不会冒出一个汉字。
pub fn needs_translation(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static T: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    T.get_or_init(|| crate::i18n_en::ENTRIES.iter().copied().collect())
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

    #[test]
    fn lang_codes_round_trip() {
        for l in [Lang::Zh, Lang::En] {
            assert_eq!(Lang::from_code(l.code()), Some(l));
        }
        assert_eq!(Lang::from_code("ZH-CN"), Some(Lang::Zh));
        assert_eq!(Lang::from_code("klingon"), None);
    }

    #[test]
    fn pref_codes_round_trip() {
        for p in [LangPref::System, LangPref::Zh, LangPref::En] {
            assert_eq!(LangPref::from_code(p.code()), p);
        }
        // 手改坏的值不能拦住启动。
        assert_eq!(LangPref::from_code("??? "), LangPref::System);
    }

    /// 英文表的占位符必须与中文一一对应：少一个 `{}`，`format!` 会 panic。
    /// 这条测试的价值在于「翻译是外部录入的数据」，不是代码——录入错了只有这里能拦住。
    #[test]
    fn placeholders_match() {
        for (zh, en) in crate::i18n_en::ENTRIES {
            let a = zh.matches("{}").count();
            let b = en.matches("{}").count();
            assert_eq!(a, b, "占位符数量不一致：{zh:?} → {en:?}");
        }
    }

    /// 英文表里不能有重复键：`HashMap` 会静默取后者，等于悄悄丢一条翻译。
    #[test]
    fn no_duplicate_keys() {
        let mut seen = HashMap::new();
        for (i, (zh, _)) in crate::i18n_en::ENTRIES.iter().enumerate() {
            if let Some(prev) = seen.insert(*zh, i) {
                panic!("英文表里 {zh:?} 出现两次（第 {prev} 与第 {i} 条）");
            }
        }
    }

    /// 英文条目不该原样等于中文（除非本来就是专有名词/符号），否则多半是漏译。
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
            "AVIF",
            "WebP",
            "GIF",
            "BMP",
        ];
        for (zh, en) in crate::i18n_en::ENTRIES {
            if zh == en {
                assert!(
                    SAME_OK.contains(zh),
                    "{zh:?} 的英文与中文完全相同，是漏译还是有意的？确认后加进 SAME_OK"
                );
            }
        }
    }
}
