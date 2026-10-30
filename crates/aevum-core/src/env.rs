//! 环境变量的唯一取用口：`AEVUM_<NAME>` 优先，`LUMEN_<NAME>` 回退。
//!
//! 为什么要有这层：同一套 if-else 会被抄到 13 个变量、9 个文件里，抄漏一个就变成
//! 「这个变量用老名字不管用」的诡异 bug，而且这种 bug 只在便携盘用户身上出现。
//!
//! 口径说明：`var_str` 把**空串按未设置处理**。这与仓库原有的
//! `if let Some(p) = std::env::var_os(X) && !p.is_empty()` 写法一致，
//! 所以把调用点换成 `var_str` 时可以放心删掉那个 `!p.is_empty()`。

use std::ffi::OsString;

use crate::brand;

/// 取一个带前缀的环境变量（`OsString` 版，给路径用）。
pub fn var_os(name: &str) -> Option<OsString> {
    var_os_with(name, |k| std::env::var_os(k))
}

/// 字符串版；**空串按未设置处理**。
pub fn var_str(name: &str) -> Option<String> {
    var_os(name)
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

/// 测试缝：把 getter 抽成参数，单测可以喂一张假表，
/// 不必 `unsafe { env::set_var }`（edition 2024 下它是 unsafe，而且进程级变量
/// 在并行测试里会互相踩）。
///
/// `pub` 而不是 `pub(crate)`：`aevum-ui` 与 `aevum-mcp` 的测试也要用它，
/// 但**产品代码只用 `var_os` / `var_str`**，别直接调这个。
pub fn var_os_with(name: &str, get: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    let fresh = format!("{}{name}", brand::ENV_PREFIX);
    if let Some(v) = get(&fresh) {
        // 新名设了就用新名，即使旧名也设着——新名优先是这层的唯一承诺。
        return Some(v);
    }
    let legacy = format!("{}{name}", brand::LEGACY_ENV_PREFIX);
    let v = get(&legacy)?;
    note_legacy(&legacy);
    Some(v)
}

/// 弃用提示：整个进程只写一行，stderr。
///
/// GUI 子系统下 stderr 不可见（无害）；MCP stdio 模式下 stdout 是协议通道、
/// stderr 是日志，安全。绝不写进 MCP 响应体——多一行「你用了旧名」只烧 token。
fn note_legacy(legacy: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let stripped = legacy
            .strip_prefix(brand::LEGACY_ENV_PREFIX)
            .unwrap_or(legacy);
        let fresh = format!("{}{stripped}", brand::ENV_PREFIX);
        eprintln!(
            "{}: 检出旧环境变量 {legacy}（已生效）；请改用 {fresh}。",
            brand::BIN
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 把一张假表包成 getter。`var_os_with` 的测试缝就是为这个存在的：
    /// 不碰真实进程环境（edition 2024 的 `set_var` 是 unsafe，且并行测试会互踩）。
    fn fake(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key: &str| map.get(key).map(|v| OsString::from(v.clone()))
    }

    /// C6：只设旧名 → 取到值。
    #[test]
    fn legacy_only_is_read() {
        let get = fake(&[("LUMEN_FFMPEG", "D:/old/ffmpeg.exe")]);
        assert_eq!(
            var_os_with("FFMPEG", &get),
            Some(OsString::from("D:/old/ffmpeg.exe"))
        );
    }

    /// C7：新旧都设 → 取新名。
    #[test]
    fn fresh_wins_over_legacy() {
        let get = fake(&[
            ("AEVUM_FFMPEG", "D:/new/ffmpeg.exe"),
            ("LUMEN_FFMPEG", "D:/old/ffmpeg.exe"),
        ]);
        assert_eq!(
            var_os_with("FFMPEG", &get),
            Some(OsString::from("D:/new/ffmpeg.exe")),
            "新名必须优先，否则用户改了变量却不生效"
        );
    }

    /// C8：都没设 → None。
    #[test]
    fn neither_set_yields_none() {
        let get = fake(&[]);
        assert_eq!(var_os_with("FFMPEG", &get), None);
    }

    /// 前缀必须是**整体**前缀：`AEVUM_X` 不能被 `X` 之外的名字蹭到。
    #[test]
    fn only_exact_prefixed_names_match() {
        let get = fake(&[("AEVUM_FFMPEG_SRC", "D:/src"), ("LUMENLENS", "x")]);
        assert_eq!(var_os_with("FFMPEG", &get), None);
        assert_eq!(var_os_with("LENS", &get), None);
    }

    /// `var_str` 的空串口径：空串 == 未设置。这条与仓库原有写法保持一致，
    /// 换调用点时才能安全删掉 `!p.is_empty()`。
    #[test]
    fn empty_string_means_unset() {
        let get = fake(&[("AEVUM_SETTINGS", "")]);
        // var_os_with 本身不吞空串（它只管取名），空串过滤在 var_str 那一层
        assert_eq!(var_os_with("SETTINGS", &get), Some(OsString::from("")));
    }
}
