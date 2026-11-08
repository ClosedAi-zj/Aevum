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

/// 工作布局：按"现在在干什么"一次性把几处尺寸摆好。
///
/// **和 [`LayoutPreset`] 是两件事，别合并：**
///
/// * `LayoutPreset` 回答"版面形状是什么"——状态栏在上还是在下、胶片条有没有。它靠
///   `preset()` 反推当前配置属于哪个预设，用户动一项就熄灭，这是对的。
/// * `Workspace` 回答"这一趟活要什么尺寸"。它**只应用一次**，之后用户拖宽、开关胶片
///   条都不会让它熄灭——因为"我选的是调色工作区"这句话在用户拖了一下宽度之后依然
///   成立。这正是 Photoshop 工作区的语义，也是为什么它不参与反推。
///
/// 为什么不做"调色 = 独立界面"：参数栈、撤销历史、三级缓存、XMP、导出预设全部要
/// 分叉，收益只有"新手第一屏更简单"，而这一层已经拿到了同样的收益。
///
/// 为什么不动左工具轨与坞页签：那两处藏起来就是删功能入口。工作区只调尺寸，不动
/// 可达性——这条是硬约束。
///
/// Task workspaces: set a few sizes once, for what you are doing right now.
///
/// **Distinct from [`LayoutPreset`] on purpose:**
///
/// * `LayoutPreset` answers "what shape is the arrangement" -- status bar top or bottom,
///   filmstrip present or not. It reverse-matches through `preset()` and goes dark as soon
///   as the user changes one axis, which is correct.
/// * `Workspace` answers "what sizes does this job need". It applies **once**; dragging the
///   panel wider or toggling the filmstrip afterwards does not clear it, because "I am in the
///   Develop workspace" is still true after a drag. That is Photoshop's reading of a
///   workspace, and the reason it takes no part in reverse matching.
///
/// Why not "colours as a separate interface": the parameter stack, undo history, the three
/// cache tiers, XMP and export presets would all fork, to buy only a simpler first screen --
/// which this layer already delivers.
///
/// Why the left tool rail and the dock tabs are untouched: hiding those removes entry points,
/// which is deleting features. A workspace only adjusts sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    /// 序列：右栏收窄（这条路看的是帧列表与评级），胶片条要有，参数按紧凑排。
    #[default]
    Sequence,
    /// 调色：画面最大化（胶片条收起）、右栏给到 340，组头保持 32px 标准档。
    Develop,
    /// 成片：回到常规宽度与胶片条，交付参数要能一眼扫完。
    Render,
}

impl Workspace {
    pub const ALL: [Workspace; 3] = [Self::Sequence, Self::Develop, Self::Render];

    pub fn cn(self) -> &'static str {
        match self {
            Self::Sequence => "序列",
            Self::Develop => "调色",
            Self::Render => "成片",
        }
    }

    /// 这个工作区要的右栏宽度。
    ///
    /// 序列收窄：那一路读的是帧列表、缺帧与评级，参数只是顺手改；调色放宽到 340：
    /// 一边看曲线一边拖推子，300 在 1600 宽的窗口里紧；成片回到默认 300。
    ///
    /// The dock width this workspace wants. Sequence narrows it (that path reads frame lists,
    /// gaps and ratings, with parameters secondary); Develop widens it to 340 because 300 is
    /// cramped when reading a curve while dragging a fader; Render returns to the 300 default.
    pub fn dock_w(self) -> f32 {
        match self {
            Self::Sequence => 280.0,
            Self::Develop => 340.0,
            Self::Render => 300.0,
        }
    }

    pub fn film(self) -> FilmSlot {
        match self {
            Self::Sequence | Self::Render => FilmSlot::Shown,
            Self::Develop => FilmSlot::Hidden,
        }
    }

    /// 参数组头的密度档。
    ///
    /// 序列与成片都走紧凑：这两条路读的是列表与交付参数，一屏能多看见一行比行距
    /// 舒展更要紧。调色走标准 32px——那一路一直在读曲线与推子，行距本身是判断依据。
    ///
    /// 三档刻意都**不等于默认组合**（300 / 胶片条 / 标准档），这样"刚打开、还没选过
    /// 工作区"时三个按钮都不亮，与 [`LayoutPreset`] 的语义一致：亮 = 当前布局确实是
    /// 它。手动拖宽之后也会熄灭，那就是"自定义"，比一直假装还是某个工作区诚实。
    ///
    /// Group-header density. Sequence and Render go compact (they read lists and delivery
    /// settings, where one more visible row beats roomier spacing); Develop stays at the
    /// standard 32 px because that path reads curves and faders where row spacing is itself
    /// information. None of the three equals the default combination (300 / filmstrip shown /
    /// standard), so a fresh start has no workspace lit -- the same reading as
    /// [`LayoutPreset`]: lit means the current layout really is that one, and a manual drag
    /// turns it off, which is an honest "custom".
    pub fn dense(self) -> bool {
        match self {
            Self::Sequence | Self::Render => true,
            Self::Develop => false,
        }
    }

    /// 当前布局正好是哪个工作区；都不是就当"自定义"。
    ///
    /// 判据只用三件能被反推的量：右栏宽、胶片条、密度。模块**不**参与——工作区会把
    /// 模块切过去，但用户在模块间来回走是常态，把它算进去会让按钮每次换页都熄灭。
    ///
    /// Which workspace the current layout is, or `None` for a custom one. Only the three
    /// reverse-matchable quantities count: dock width, filmstrip, density. The module is
    /// **not** part of it -- the workspace switches the module once, but hopping between
    /// modules is normal and counting it would darken the button on every switch.
    pub fn of(layout: Layout, dock_w: f32) -> Option<Workspace> {
        Workspace::ALL.into_iter().find(|w| {
            layout.film == w.film()
                && layout.dense == w.dense()
                && (dock_w - w.dock_w()).abs() < 0.5
        })
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
    /// 信息密度：`true` = 紧凑（参数组头 28px），`false` = 标准（32px，默认）。
    ///
    /// 这个开关解决的是"照哪张参考图"的分歧：用户提供的 ACR 截图实测是 32px，
    /// 而更紧凑的 ACR 版本是 26–28px。两者都对，取决于你习惯的屏与视力，所以
    /// 交给用户选，而不是替他决定。
    ///
    /// Density: `true` = compact (28 px group headers), `false` = standard (32 px, default).
    ///
    /// This settles a genuine "which reference" disagreement: the user's ACR screenshot
    /// measures 32 px, while tighter ACR builds use 26-28 px. Both are right and it
    /// depends on screen and eyesight, so the user chooses instead of us deciding.
    pub dense: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            status: StatusSlot::Bottom,
            rail: Side::Left,
            dock_side: Side::Right,
            film: FilmSlot::Shown,
            dense: false,
        }
    }
}

impl Layout {
    /// 当前配置对应哪个预设；改了一项就返回 `None`，面板上的预设按钮自动熄灭。
    ///
    /// Which preset the current configuration matches; change one item and this returns
    /// `None`, so the preset buttons in the panel go dark on their own.
    pub fn preset(&self) -> Option<LayoutPreset> {
        // 密度不参与"是否是某个预设"的判定：它是一层与版面无关的观感偏好，
        // 选了紧凑不该让三个预设按钮一起熄灭。
        //
        // Density is excluded from the preset match: it is a look-and-feel preference
        // independent of arrangement, and picking compact must not extinguish all three
        // preset buttons.
        let probe = Self {
            dense: false,
            ..*self
        };
        LayoutPreset::ALL.into_iter().find(|p| p.layout() == probe)
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
