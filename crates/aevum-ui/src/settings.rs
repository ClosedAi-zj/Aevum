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
    /// 浮窗上次落在哪儿（键 = 窗口标识，值 = 左上角坐标，逻辑点）。
    ///
    /// 为什么不用 egui 自己的位置记忆：egui 把 `Area` 的 rect 存在**它的** store 里，
    /// 而本程序没有打开 eframe 的 storage（也不该为了三个浮窗去开——那会把整套
    /// 面板状态一起卷进持久化）。所以窗口位置原先只在进程内有效，重启就回默认位。
    /// 这里自己记一份，读写都走已有的 `settings_dirty` 单出口。
    ///
    /// `Option` 的含义与 `collapsed` 一致：`None` = 从没拖过，用各自的首开位置。
    /// 坐标会被 [`clamp_win_pos`] 夹回当前视口，多显示器拔掉后不会飘到屏幕外。
    ///
    /// Where each floating window last sat (key = window id, value = top-left in
    /// logical points).
    ///
    /// Why not egui's own position memory: egui keeps `Area` rects in *its* store, and
    /// this program never enabled eframe's storage (nor should it, just for three
    /// windows -- that would drag the whole panel state into persistence). Window
    /// positions therefore only survived one process. This keeps its own copy, reading
    /// and writing through the existing single `settings_dirty` exit.
    ///
    /// `Option` means the same as on `collapsed`: `None` = never dragged, use each
    /// window's first-open position. Coordinates are clamped back into the current
    /// viewport by [`clamp_win_pos`], so unplugging a monitor cannot strand a window
    /// off-screen.
    pub win_pos: Option<std::collections::BTreeMap<String, [f32; 2]>>,
    /// 预设栏三节的次序（键见 [`crate::state::PRESET_SECTIONS`]）。`None` / 空表 /
    /// 带未知键 / 少键，读回来都会被规整成一个完整排列——老文件与手改文件都能开。
    ///
    /// Order of the three preset-column sections (keys in
    /// [`crate::state::PRESET_SECTIONS`]). `None`, empty, unknown keys or missing keys are
    /// all normalised into a full permutation on load, so old and hand-edited files both
    /// open fine.
    pub preset_order: Option<Vec<String>>,
    /// 预览里是否打开「白以上检视」（见 `aevum_core::fold_above_white_in_place`）。
    ///
    /// 存下来是因为它是**观看方式**而不是一次性动作：用户为了看 HDR 高光打开它，关掉
    /// 软件再打开还在看同一张图，期望它还是开着。`None` = 老文件没有这一项，按关处理
    /// （默认关，否则所有人第一次打开都会看到略微压暗的白）。
    ///
    /// Whether the preview shows "above white" inspection (see
    /// `aevum_core::fold_above_white_in_place`).
    ///
    /// Persisted because it is a **way of looking** rather than a one-off action: a user who
    /// turned it on to inspect HDR highlights reopens the app on the same picture and expects
    /// it still on. `None` = older file without the field, treated as off (the default, or
    /// everyone would see slightly dimmed whites on first launch).
    pub hdr_preview: Option<bool>,
    /// 合并面板的滑杆参数（HDR / 堆栈 / 时间合成 / 焦点合成），全局偏好：换序列不清、
    /// 重启还在。枚举字段存的是 key 串（`stack_mode` = "lighten"，`tb_layout` =
    /// "sky-ground"，`focus_method` = "depthmap"），读回来解析不了就落默认值。
    /// 结果态（上次成片路径、报错、预览纹理）不在这里——那是会话的事。
    ///
    /// The merge panels' slider parameters (HDR / stack / time-blend / focus stacking),
    /// a global preference: cleared by no sequence switch, survives a restart. Enum
    /// fields store their key strings (`stack_mode` = "lighten", `tb_layout` =
    /// "sky-ground", `focus_method` = "depthmap"); anything unparsable falls back to
    /// the defaults on load. Result state (the last output path, errors, preview
    /// textures) does not live here -- that is session business.
    pub merge: MergeParams,
}

/// 合并面板滑杆参数的落盘形状。字段与 `AppState` 的 `hdr_` / `stack_` / `tb_` /
/// `focus_` 可调参数一一对应；老 settings.json 没有这一节时整块落默认值
/// （容器级 `#[serde(default)]`），单个字段缺失也一样。
///
/// The persisted shape of the merge panels' slider parameters. Fields map one-to-one
/// onto `AppState`'s adjustable `hdr_` / `stack_` / `tb_` / `focus_` parameters; an
/// old settings.json without this section lands on the defaults wholesale (the
/// container-level `#[serde(default)]`), and so does any single missing field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MergeParams {
    /// HDR 包围：当前帧前后各取几帧。
    ///
    /// HDR bracket: frames taken on each side of the current one.
    pub hdr_around: u32,
    /// 堆栈：中心前后各取几帧 / 统计量 key / σ 倍数 / 补洞。
    ///
    /// Stacking: frames on each side of the centre / the statistic's key / the sigma
    /// multiplier / gap fill.
    pub stack_around: u32,
    pub stack_mode: String,
    pub stack_sigma: f32,
    pub stack_fill: bool,
    /// 时间合成：起始帧后取几帧 / 布局 key / 带数 / 羽化比例 / 地平线 / 自动地平线 /
    /// 去鬼影阈值 / 曝光匹配。
    ///
    /// Time blending: frames after the start / layout key / band count / feather
    /// fraction / horizon / auto horizon / deghost threshold / exposure matching.
    pub tb_around: u32,
    pub tb_layout: String,
    pub tb_bands: u32,
    pub tb_feather: f32,
    pub tb_horizon: f32,
    pub tb_horizon_auto: bool,
    pub tb_deghost: f32,
    pub tb_match: bool,
    /// 焦点合成：起始帧后取几帧 / 方法 key / 测度半径 / 无细节阈值 / 去斑 / 羽化。
    ///
    /// Focus stacking: frames after the start / method key / measure radius /
    /// featureless threshold / despeckle / feather.
    pub focus_around: u32,
    pub focus_method: String,
    pub focus_radius: u32,
    pub focus_threshold: f32,
    pub focus_smooth_z: u32,
    pub focus_feather: f32,
}

impl Default for MergeParams {
    /// 默认值与 `AppState::new()` 的初始参数逐项一致：两处必须同步改，
    /// 否则「没存过的用户」与「存过默认值的用户」看到的首次合成不同。
    ///
    /// Defaults match `AppState::new()`'s initial parameters item by item; the two
    /// sites must change together, or users who never saved and users who saved the
    /// defaults would see different first merges.
    fn default() -> Self {
        Self {
            hdr_around: 1,
            stack_around: 4,
            stack_mode: "lighten".to_owned(),
            stack_sigma: 3.0,
            stack_fill: true,
            tb_around: 7,
            tb_layout: "sky-ground".to_owned(),
            tb_bands: 8,
            tb_feather: 0.04,
            tb_horizon: 0.5,
            tb_horizon_auto: false,
            tb_deghost: 0.5,
            tb_match: true,
            focus_around: 7,
            focus_method: "depthmap".to_owned(),
            focus_radius: 2,
            focus_threshold: 0.0,
            focus_smooth_z: 1,
            focus_feather: 2.0,
        }
    }
}

/// 把落盘的窗口坐标夹回当前视口，保证整窗可见。
///
/// 三条边界都要管：①视口左上不能为负（窗口标题栏拖到屏幕外就再也抓不回来）；
/// ②右下要让出窗口自身尺寸（否则只剩一条边在屏内）；③视口比窗口还小时，退回
/// 视口左上角落——宁可错位，也不能算出负的可用宽度。
///
/// Clamp a persisted window position back into the current viewport so the whole
/// window is reachable. Three edges matter: (1) the top-left cannot go negative (a
/// title bar dragged past the edge can never be grabbed again); (2) the bottom-right
/// must leave room for the window itself; (3) when the viewport is smaller than the
/// window, fall back to the viewport's top-left -- misplacement beats a negative
/// usable width.
pub fn clamp_win_pos(pos: [f32; 2], win: [f32; 2], view: [f32; 4]) -> [f32; 2] {
    let [vx, vy, vw, vh] = view;
    let max_x = (vx + vw - win[0]).max(vx);
    let max_y = (vy + vh - win[1]).max(vy);
    [pos[0].clamp(vx, max_x), pos[1].clamp(vy, max_y)]
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

    /// 浮窗位置要能真的过一遍 JSON：存、读、逐位相等。
    #[test]
    fn window_positions_survive_the_round_trip() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("market".to_owned(), [120.0f32, -8.5]);
        m.insert("look".to_owned(), [1536.0f32, 52.0]);
        let s = Settings {
            win_pos: Some(m),
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
        // 老文件没有这一项 → `None`（从没拖过），不是空表。
        let absent: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.win_pos, None);
    }

    /// 夹取的三条边界：负坐标、越右下、视口比窗口还小。
    #[test]
    fn window_positions_are_clamped_into_the_viewport() {
        let view = [0.0f32, 0.0, 1920.0, 1080.0];
        let win = [400.0f32, 300.0];
        // 跑到屏幕左上外面（标题栏摸不到了）→ 拉回 0。
        assert_eq!(clamp_win_pos([-320.0, -80.0], win, view), [0.0, 0.0]);
        // 跑到右下外面 → 右下要让出窗口自身尺寸。
        assert_eq!(clamp_win_pos([1800.0, 1000.0], win, view), [1520.0, 780.0]);
        // 本来就在里面 → 一动都不动（不能因为夹取把位置改出 1px 差）。
        assert_eq!(clamp_win_pos([640.0, 360.0], win, view), [640.0, 360.0]);
        // 视口比窗口还小 → 退回视口左上，不是负的可用宽度。
        let small = [0.0f32, 0.0, 300.0, 200.0];
        assert_eq!(clamp_win_pos([100.0, 100.0], win, small), [0.0, 0.0]);
    }

    /// 合并参数的往返：存、读、逐项相等；老格式 JSON（没有 merge 节）整块落默认值。
    ///
    /// Merge-parameter round trip: save, load, compare item by item; an old-format JSON
    /// (no merge section) lands on the defaults wholesale.
    #[test]
    fn merge_params_survive_the_round_trip_and_old_files_load_with_defaults() {
        let merge = MergeParams {
            hdr_around: 3,
            stack_mode: "sigma-clip".to_owned(),
            stack_sigma: 2.5,
            stack_fill: false,
            tb_layout: "bands".to_owned(),
            tb_bands: 12,
            tb_deghost: 0.0,
            focus_method: "weighted".to_owned(),
            focus_threshold: 12.5,
            focus_feather: 8.0,
            ..MergeParams::default()
        };
        let s = Settings {
            merge,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
        // 老版本文件没有 merge 节：读进来必须是整块默认值，不能报错。
        let old: Settings = serde_json::from_str(r#"{"appearance":{"theme":"light"}}"#).unwrap();
        assert_eq!(old.merge, MergeParams::default());
        // 单个字段缺失也一样：serde default 逐字段兜底，解析不了的枚举 key 回默认。
        let partial: Settings = serde_json::from_str(
            r#"{"merge":{"hdr_around":2,"focus_radius":9,"stack_mode":"nope"}}"#,
        )
        .unwrap();
        assert_eq!(partial.merge.hdr_around, 2);
        assert_eq!(partial.merge.focus_radius, 9);
        // stack_mode 是裸 String，坏值原样存进来；坏 key 的清洗在 adopt_settings。
        assert_eq!(partial.merge.stack_mode, "nope");
        assert_eq!(
            partial.merge.stack_around,
            MergeParams::default().stack_around
        );
    }
}
