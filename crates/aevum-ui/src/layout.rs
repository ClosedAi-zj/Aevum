//! 布局接口：区域在哪个方向、显不显示。
//!
//! 这一层刻意和主题分开：**主题管"看起来怎样"，布局管"东西放哪"**。
//! 混在一起的话，4 套皮肤 × 3 种明暗 × 6 种布局就是 72 个测试面；
//! 分开之后主题是纯函数（参数进、色板出），布局只有 4 个枚举。
//!
//! 实现上只做一件事：决定 `egui::Panel` 的声明顺序与方向。
//! 各个区域自己的绘制函数（`menu_row` / `dock_panel` / `status_bar`…）
//! **不认识布局**，也不知道自己在左边还是上面 —— 谁读布局，谁就再也挪不动了。
//!
//! 为什么要枚举而不是自由坐标：自由拖拽在 DPI 变化、窗口缩放、多显示器
//! 之间切换时必然错位，而且截图回归没法比对。枚举换来的是"每种组合都能拍一张图"。
//!
//! Layout interface: which side each region sits on, and whether it shows.
//!
//! Deliberately separate from the theme: **the theme owns "how it looks", the
//! layout owns "where things go"**. Mixed together, 4 skins x 3 light/dark x 6
//! layouts would mean 72 test surfaces; separated, the theme is a pure function
//! (parameters in, palette out) and the layout is four enums.
//!
//! The implementation does exactly one thing: decide the declaration order and
//! direction of the `egui::Panel`s. Each region's own draw function
//! (`menu_row` / `dock_panel` / `status_bar` ...) **does not know about layout**,
//! and does not know whether it sits left or top -- whoever starts reading the
//! layout can never be moved again.
//!
//! Why enums instead of free coordinates: free dragging inevitably misaligns
//! across DPI changes, window resizes and multiple monitors, and screenshot
//! regression has nothing to compare against. Enums buy "one screenshot per
//! combination".

use serde::{Deserialize, Serialize};

/// 状态栏位置。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusSlot {
    /// 底部（默认，Windows/macOS 桌面软件的惯例位置）。
    #[default]
    Bottom,
    /// 标题栏之下、选项栏之上：适合把"当前文件/色彩空间/占用"当抬头读的用法。
    Top,
    /// 隐藏：画布优先，信息移到信息面板。
    Hidden,
}

impl StatusSlot {
    pub const ALL: [StatusSlot; 3] = [Self::Bottom, Self::Top, Self::Hidden];

    pub fn cn(self) -> &'static str {
        match self {
            Self::Bottom => "底部",
            Self::Top => "顶部",
            Self::Hidden => "隐藏",
        }
    }
}

/// 侧边方位：工具轨与调整面板都用它。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    #[default]
    Left,
    Right,
}

impl Side {
    pub fn is_left(self) -> bool {
        matches!(self, Side::Left)
    }

    pub fn cn(self) -> &'static str {
        match self {
            Self::Left => "左",
            Self::Right => "右",
        }
    }
}

/// 胶片条：显示 / 隐藏（高度仍在主题里，跟着密度走）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilmSlot {
    #[default]
    Shown,
    Hidden,
}

impl FilmSlot {
    pub fn allowed(self) -> bool {
        matches!(self, FilmSlot::Shown)
    }
}

/// 布局预设：只是把下面几个枚举配成一组，不是另一套模板。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutPreset {
    /// 经典：底状态栏 + 底胶片条 + 左工具轨 + 右调整面板。
    #[default]
    Classic,
    /// 专注：去掉胶片条，画布变大；挑图仍靠图库。
    Focus,
    /// 极简：再去掉状态栏，只留标题、选项栏、画布与调整面板。
    Min,
}

impl LayoutPreset {
    pub const ALL: [LayoutPreset; 3] = [Self::Classic, Self::Focus, Self::Min];

    pub fn cn(self) -> &'static str {
        match self {
            Self::Classic => "经典",
            Self::Focus => "专注",
            Self::Min => "极简",
        }
    }

    pub fn layout(self) -> Layout {
        match self {
            Self::Classic => Layout::default(),
            Self::Focus => Layout {
                film: FilmSlot::Hidden,
                ..Layout::default()
            },
            Self::Min => Layout {
                status: StatusSlot::Hidden,
                film: FilmSlot::Hidden,
                ..Layout::default()
            },
        }
    }
}

/// 布局参数：落盘就是 `settings.json` 里的 `layout` 段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub status: StatusSlot,
    pub rail: Side,
    pub dock_side: Side,
    pub film: FilmSlot,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            status: StatusSlot::Bottom,
            rail: Side::Left,
            dock_side: Side::Right,
            film: FilmSlot::Shown,
        }
    }
}

impl Layout {
    /// 当前配置对应哪个预设；改了一项就返回 `None`，面板上的预设按钮自动熄灭。
    ///
    /// Which preset the current configuration matches; change one item and this returns
    /// `None`, so the preset buttons in the panel go dark on their own.
    pub fn preset(&self) -> Option<LayoutPreset> {
        LayoutPreset::ALL.into_iter().find(|p| p.layout() == *self)
    }

    pub fn with_preset(self, p: LayoutPreset) -> Self {
        // 预设只覆盖它自己声明的那几项，其余保持用户当前选择。
        let base = p.layout();
        Self {
            status: base.status,
            film: base.film,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_classic() {
        assert_eq!(Layout::default().preset(), Some(LayoutPreset::Classic));
    }

    #[test]
    fn changing_one_axis_drops_the_preset() {
        let l = Layout {
            rail: Side::Right,
            ..Layout::default()
        };
        assert_eq!(l.preset(), None);
    }

    #[test]
    fn preset_keeps_user_sides() {
        let l = Layout {
            rail: Side::Right,
            dock_side: Side::Left,
            ..Layout::default()
        }
        .with_preset(LayoutPreset::Min);
        assert_eq!(l.status, StatusSlot::Hidden);
        assert_eq!(l.film, FilmSlot::Hidden);
        assert_eq!(l.rail, Side::Right);
        assert_eq!(l.dock_side, Side::Left);
    }
}
