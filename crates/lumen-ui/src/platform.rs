//! 平台分派：同一套界面代码，在 Windows 上长成 Windows 程序，在 macOS 上长成
//! macOS 程序。
//!
//! 只分派**外壳**（chrome）——标题栏高度、窗口按钮在哪一侧、菜单的呼吸感、
//! 分段控件的形态。区域内部的版式（面板、胶片条、画布标尺）两个平台是同一套：
//! 那是产品的样子，不该跟着操作系统漂移。
//!
//! 刻意不做的事：不按平台换图标、不按平台换字体族（字体栈本来就有平台序）、
//! 不按平台换配色。这三件在跨平台软件里最容易变成两套东西，维护成本翻倍，
//! 而用户能感知到的差异恰恰只在标题栏那几十个像素上。

use egui::Context;

/// 目标操作系统。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Os {
    Win,
    Mac,
}

impl Os {
    /// 编译期就能定的那个平台。运行时不需要探测：同一份二进制不会跨平台跑。
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            // Linux 在 M5 之前不进发布矩阵，先按 Windows 口径走：
            // 两者都是"窗口按钮在右上、菜单在左"的形状。
            Os::Win
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            Os::Win => "Windows",
            Os::Mac => "macOS",
        }
    }
}

/// 用户的显式选择。默认跟随编译目标，允许在外观面板里强制成另一套，
/// 这样一台机器上就能拍两种形态的截图做基线比对。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OsChoice {
    #[default]
    Auto,
    Win,
    Mac,
}

impl OsChoice {
    pub const ALL: [OsChoice; 3] = [OsChoice::Auto, OsChoice::Win, OsChoice::Mac];

    pub fn resolve(self) -> Os {
        match self {
            OsChoice::Auto => Os::current(),
            OsChoice::Win => Os::Win,
            OsChoice::Mac => Os::Mac,
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            OsChoice::Auto => "跟随系统",
            OsChoice::Win => "Windows",
            OsChoice::Mac => "macOS",
        }
    }
}

/// 外壳的可变部分。十二个数，全部是长度或布尔，没有颜色 ——
/// 颜色属于 `theme::Palette`，跟平台无关。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    /// 窗口控件在哪一侧：Win 右上，Mac 左上。
    pub buttons_left: bool,
    /// 画圆点（红/黄/绿）还是画字形（横线/方框/叉）。
    pub traffic_lights: bool,
    /// 标题栏行高。Mac 的标题栏比 Win 窄，菜单也压得更扁。
    pub titlebar_h: f32,
    /// 左侧品牌图标块。Mac 版没有这块，标题就是窗口名。
    pub brand_icon: bool,
    /// 品牌名的字距（px）。两端都拉开，Mac 拉得更开。
    pub title_tracking: f32,
    /// 菜单项左右内边距。
    pub menu_pad_x: f32,
    /// 菜单项之间的间距。Win 几乎贴齐，Mac 留一点气口。
    pub menu_gap: f32,
    /// 品牌区到菜单的距离。
    pub brand_gap: f32,
    /// 菜单字号。
    pub menu_font: f32,
    /// 模块切换的格宽。
    pub seg_width: f32,
    /// 模块切换是否画那层胶囊底。Mac 版只留文字与选中下划线。
    pub seg_track: bool,
    /// 控件圆角的偏置（叠加在 `Appearance::radius` 上）。
    pub radius_bias: f32,
}

impl Chrome {
    /// 每个数都有来历：高度、内边距、格宽按两家的 HIG 取值，
    /// 不是"看着差不多"。改成别的值之前先想清楚要跟哪家的规范对齐。
    pub fn of(os: Os) -> Self {
        match os {
            // Windows 11：标题栏 36（含 8px 上下气口），菜单 12px、项左右 12px 内边距，
            // 右上三枚 46×32 的窗口按钮，模块切换是带轨道的分段控件。
            Os::Win => Self {
                buttons_left: false,
                traffic_lights: false,
                titlebar_h: 36.0,
                brand_icon: true,
                title_tracking: 1.2,
                menu_pad_x: 12.0,
                menu_gap: 1.0,
                brand_gap: 16.0,
                menu_font: 12.0,
                seg_width: 58.0,
                seg_track: true,
                radius_bias: 0.0,
            },
            // macOS：统一工具栏 30，交通灯直径 12、间距 8，菜单 13px、项距 2，
            // 模块切换是纯文字页签（macOS 没有"分段控件贴标题栏"的习惯）。
            // 圆角偏置 +2：同一套控件在 mac 上略圆一档，才不像 Windows 的方角。
            Os::Mac => Self {
                buttons_left: true,
                traffic_lights: true,
                titlebar_h: 30.0,
                brand_icon: false,
                title_tracking: 1.6,
                menu_pad_x: 10.0,
                menu_gap: 2.0,
                brand_gap: 22.0,
                menu_font: 13.0,
                seg_width: 54.0,
                seg_track: false,
                radius_bias: 2.0,
            },
        }
    }

    /// 交通灯按钮的直径与间距（只在 `traffic_lights` 为真时用）。
    pub const LIGHT_D: f32 = 12.0;
    pub const LIGHT_GAP: f32 = 8.0;
    /// 交通灯那一组的左侧起点。
    pub const LIGHT_X: f32 = 14.0;

    /// 窗口控件占用的横向宽度：右上三枚要避让，左上交通灯也要避让。
    pub fn buttons_zone_w(&self) -> f32 {
        if self.traffic_lights {
            Self::LIGHT_X * 2.0 + Self::LIGHT_D * 3.0 + Self::LIGHT_GAP * 2.0
        } else {
            46.0 * 3.0
        }
    }
}

/// 预览模式：截图时窗口拿不到焦点，但截图要拍聚焦态。
///
/// 无头启动（`--shot`）的窗口在系统看来是后台窗口，`focused` 一路为假，
/// 标题栏就画成未聚焦的样子（交通灯褪灰、窗口按钮压暗）。那是**真实状态
/// 但不代表用户看到的画面**，用它做验收基线会一直是错的。这个开关只影响
/// 聚焦态的渲染，不碰任何逻辑。
static PREVIEW_FOCUS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 打开/关闭预览聚焦。只该由截图流程调用。
pub fn preview_focus(on: bool) {
    PREVIEW_FOCUS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// 窗口此刻是否该按"聚焦"来画。
pub fn focused(ctx: &Context) -> bool {
    if PREVIEW_FOCUS.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    ctx.input(|i| i.viewport().focused.unwrap_or(true))
}

/// 当前生效的外壳。跟 `palette()` 一样是全局读，绘制代码直接调。
pub fn chrome() -> Chrome {
    Chrome::of(crate::theme::appearance().os.resolve())
}

/// 当前生效的操作系统（截图与诊断用）。
pub fn os() -> Os {
    crate::theme::appearance().os.resolve()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_gets_a_distinct_shell() {
        let win = Chrome::of(Os::Win);
        let mac = Chrome::of(Os::Mac);
        // 四件必须不同的事：按钮侧、字形、行高、分段形态。
        assert!(!win.buttons_left && mac.buttons_left);
        assert!(!win.traffic_lights && mac.traffic_lights);
        assert!(win.titlebar_h > mac.titlebar_h);
        assert!(win.seg_track && !mac.seg_track);
        // 菜单字号按行高压得住为准：win 36px 行配 12px 字、mac 30px 行配 13px 字，
        // 两端刻意不同档，谁也不许悄悄改成同一个数。
        assert_ne!(mac.menu_font, win.menu_font);
        assert!(mac.menu_font + 8.0 <= mac.titlebar_h);
    }

    #[test]
    fn auto_follows_the_build_target() {
        assert_eq!(OsChoice::Auto.resolve(), Os::current());
        // 显式选择必须压过自动，否则截图拍不出另一套形态。
        assert_eq!(OsChoice::Mac.resolve(), Os::Mac);
        assert_eq!(OsChoice::Win.resolve(), Os::Win);
    }

    #[test]
    fn button_zone_covers_the_three_buttons() {
        for os in [Os::Win, Os::Mac] {
            let c = Chrome::of(os);
            // 避让区必须真的装得下三枚，否则模块标签会压到按钮上。
            let needed = if c.traffic_lights {
                Chrome::LIGHT_X + Chrome::LIGHT_D * 3.0 + Chrome::LIGHT_GAP * 2.0
            } else {
                46.0 * 3.0
            };
            assert!(c.buttons_zone_w() >= needed);
        }
    }
}
