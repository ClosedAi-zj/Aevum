//! 用户设置的落盘与读取。
//!
//! 只管"看起来怎样、东西放哪"这一类偏好，不碰图库与渲染队列（那两个各有自己的
//! 文件，历史上就是这么分的）。文件坏掉不能拦住启动：所有字段都有缺省值，
//! 解析失败就当没存过 —— 用户最多丢一次皮肤，不该连程序都打不开。
//!
//! User settings: persistence and loading.
//!
//! Only preferences of the "how does it look, where do things go" kind. The
//! library and the render queue are not here -- each has its own file, and that
//! split is historical. A broken file must never block startup: every field has
//! a default, and an unparsable file is treated as "never saved". The worst a
//! user loses is one skin choice; the program itself should still open.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::i18n::LangPref;
use crate::layout::Layout;
use crate::theme::{Appearance, ThemeMode};

/// 设置文件名。放漫游配置目录，与图库清单、渲染队列同级。
const FILE: &str = "settings.json";

/// 落盘的全部内容。新增字段必须给缺省值（`#[serde(default)]`），
/// 否则老版本写的文件会在新版本里解析失败。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub layout: Layout,
    /// 字号档位（0–3，对应 `FONT_ZOOMS`）。
    ///
    /// Font size step (0-3, indexes `FONT_ZOOMS`).
    pub font_level: usize,
    /// 界面语言：跟随系统 / 中文 / English。缺省「跟随系统」，老设置文件读进来也落在这一档。
    ///
    /// UI language: follow the system / Chinese / English. Defaults to "follow the
    /// system", which is also where an older settings file lands.
    pub lang: LangPref,
    /// 折叠中的面板分组键（键表见 [`crate::state::GROUP_SPECS`]）。
    ///
    /// 三层含义必须分清，否则「首屏该展开什么」会来回打摆：
    ///
    /// * `None` —— 还没表过态（首次启动，或老版本写的文件）。用
    ///   [`crate::state::default_collapsed`]，低频组折起来；
    /// * `Some(vec![])` —— 用户把每一组都点开了。这是**明确表态**，不许再拿默认态
    ///   覆盖他；
    /// * `Some([...])` —— 用户自己折起来的那些。
    ///
    /// 所以这里是 `Option` 而不是裸 `Vec`：`#[serde(default)]` 让缺字段落成 `None`，
    /// 恰好等于「没表过态」，不用再写版本号判别。
    ///
    /// Keys of the panel groups that are currently collapsed (see
    /// [`crate::state::GROUP_SPECS`] for the key table).
    ///
    /// Three meanings must stay distinct, or "what opens on the first screen" will
    /// keep flip-flopping:
    ///
    /// * `None` -- no opinion expressed yet (first launch, or a file written by an
    ///   older version). Fall back to [`crate::state::default_collapsed`], which
    ///   folds the low-frequency groups;
    /// * `Some(vec![])` -- the user opened every single group. That is an **explicit
    ///   statement** and must not be overridden by the defaults;
    /// * `Some([...])` -- the groups the user folded himself.
    ///
    /// Hence `Option` rather than a bare `Vec`: `#[serde(default)]` turns a missing
    /// field into `None`, which is exactly "no opinion expressed", so no version tag
    /// is needed to tell the two apart.
    pub collapsed: Option<Vec<String>>,
}

/// `%APPDATA%/aevum-studio/settings.json`；`AEVUM_SETTINGS`（旧名 `LUMEN_SETTINGS` 回退）
/// 可指到别处（便携盘与测试用）。
///
/// 首行调用 `ensure_adopted()`：配置目录的首启迁移挂在这里，于是 GUI、MCP 无头、
/// `--selftest` 与集成测试四个入口都自动覆盖，不必在各处写启动代码。
///
/// `%APPDATA%/aevum-studio/settings.json`; `AEVUM_SETTINGS` (with the legacy
/// `LUMEN_SETTINGS` as fallback) can point elsewhere (portable drives, tests).
///
/// The first line calls `ensure_adopted()`: the first-launch migration of the
/// config directory hangs off this, so the GUI, the headless MCP server,
/// `--selftest` and the integration tests all get it for free instead of each
/// wiring up its own startup code.
pub fn path() -> PathBuf {
    aevum_core::config_dir::ensure_adopted();
    if let Some(p) = aevum_core::env::var_os("SETTINGS")
        && !p.is_empty()
    {
        let p = PathBuf::from(p);
        // override 是产品承诺：设了就必须用它，绝不偷偷回退 `%APPDATA%`。
        // 这里只把「用不了」喊出来（带缓存，每进程只探一次、只告一次），
        // 路径照原样返回——静默失败才是当初的 bug。
        aevum_core::env::check_override_cached("SETTINGS", &p, aevum_core::env::Target::File);
        return p;
    }
    aevum_core::config_dir::dir().join(FILE)
}

/// 插件目录：`%APPDATA%/aevum-studio/plugins`。`AEVUM_PLUGINS`（旧名 `LUMEN_PLUGINS`
/// 回退）可指到别处（便携盘、开发时指到仓库里的样例插件）。
/// 目录不存在不算错 —— 只是没装插件。
///
/// Plugin directory: `%APPDATA%/aevum-studio/plugins`. `AEVUM_PLUGINS` (with the
/// legacy `LUMEN_PLUGINS` as fallback) can point elsewhere (portable drives, or
/// the repo's sample plugins during development).
/// A missing directory is not an error -- it just means no plugins are installed.
pub fn plugin_dir() -> PathBuf {
    if let Some(p) = aevum_core::env::var_os("PLUGINS")
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir)
        .join("plugins")
}

/// 读设置。缺文件、坏 JSON、字段越界都回落到缺省值，不报错、不中断启动。
///
/// Read settings. Missing file, broken JSON, out-of-range fields all fall back to
/// the defaults -- no error, no interrupted startup.
pub fn load() -> Settings {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return Settings::default();
    };
    let mut s: Settings = serde_json::from_str(&text).unwrap_or_default();
    // 手改过的文件可能带上越界字号，读进来就夹住，免得后面索引越界。
    s.font_level = s.font_level.min(3);
    s.appearance.radius = s.appearance.radius.clamp(0.0, 16.0);
    s
}

/// 写设置。失败不打断用户（调色中的崩溃比丢一次设置贵得多），但必须出声：
/// 便携盘路径失效时用户以为设置存上了，实际每次都在丢——静默吞掉只会让
/// 「为什么每次打开主题都变回去」变成无头案。
///
/// Write settings. A failure must not interrupt the user (a crash mid-grade costs
/// far more than losing one settings write), but it must be audible: when a
/// portable-drive path goes stale the user believes the settings were saved while
/// every write is dropped. Swallowing that silently turns "why does the theme
/// revert every time I open this" into an unsolvable mystery.
pub fn save(s: &Settings) {
    let p = path();
    if let Some(dir) = p.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        aevum_core::env::note(format!(
            "设置保存失败：目录 {} 建不出来（{e}）；本次修改只在内存里，退出即丢。\
             把该环境变量去掉可回到默认配置目录，或改指一个可写的位置。",
            dir.display()
        ));
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(s)
        && let Err(e) = std::fs::write(&p, text)
    {
        aevum_core::env::note(format!(
            "设置保存失败：写不进 {}（{e}）；本次修改只在内存里，退出即丢。\
             把该环境变量去掉可回到默认配置目录，或改指一个可写的位置。",
            p.display()
        ));
    }
}

/// 明暗模式的展示顺序（面板上按这个排）。
///
/// Display order of the light/dark modes (the panel lists them in this order).
pub const THEME_ORDER: [ThemeMode; 3] = [ThemeMode::Dark, ThemeMode::Black, ThemeMode::Light];

#[cfg(test)]
mod tests {
    use super::*;

    /// 老版本只写了 appearance，新版本读出来 layout/font_level 必须是缺省值而不是报错。
    #[test]
    fn partial_file_keeps_defaults() {
        let s: Settings = serde_json::from_str(r#"{"appearance":{"theme":"light"}}"#).unwrap();
        assert_eq!(s.appearance.theme, ThemeMode::Light);
        assert_eq!(s.appearance.skin, Appearance::default().skin);
        assert_eq!(s.layout, Layout::default());
        assert_eq!(s.font_level, 0);
        // 老文件里没有 collapsed 这一项：必须是 `None`（没表过态），不能是空 Vec——
        // 空 Vec 的意思是「用户把每一组都点开了」，两者混一起，首屏默认态就会
        // 在升级后第一次启动时被跳过。
        assert_eq!(s.collapsed, None);
    }

    /// 「全展开」和「没表过态」在 JSON 里必须能区分开。
    #[test]
    fn empty_collapsed_is_not_the_same_as_absent() {
        let all_open: Settings = serde_json::from_str(r#"{"collapsed":[]}"#).unwrap();
        assert_eq!(all_open.collapsed, Some(Vec::new()));
        let absent: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.collapsed, None);
    }

    #[test]
    fn round_trip() {
        let s = Settings {
            font_level: 2,
            ..Settings::default()
        };
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(s, back);
    }
}
