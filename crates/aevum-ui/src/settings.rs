//! 用户设置的落盘与读取。
//!
//! 只管"看起来怎样、东西放哪"这一类偏好，不碰图库与渲染队列（那两个各有自己的
//! 文件，历史上就是这么分的）。文件坏掉不能拦住启动：所有字段都有缺省值，
//! 解析失败就当没存过 —— 用户最多丢一次皮肤，不该连程序都打不开。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::Layout;
use crate::theme::{Appearance, ThemeMode};

/// 设置文件名。放漫游配置目录，与图库清单、渲染队列同级。
const FILE: &str = "settings.json";

/// 落盘的全部内容。新增字段必须给缺省值（`#[serde(default)]`），
/// 否则老版本写的文件会在新版本里解析失败。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
    pub layout: Layout,
    /// 字号档位（0–3，对应 `FONT_ZOOMS`）。
    pub font_level: usize,
}

/// `%APPDATA%/aevum-studio/settings.json`；`AEVUM_SETTINGS`（旧名 `LUMEN_SETTINGS` 回退）
/// 可指到别处（便携盘与测试用）。
///
/// 首行调用 `ensure_adopted()`：配置目录的首启迁移挂在这里，于是 GUI、MCP 无头、
/// `--selftest` 与集成测试四个入口都自动覆盖，不必在各处写启动代码。
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
