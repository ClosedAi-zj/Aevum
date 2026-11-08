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

use egui::{Pos2, Rect};
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

    /// 面板上该高亮哪个预设：**工作区命中时不亮任何预设**。
    ///
    /// 原因是两个控件会互相矛盾：调色工作区的版面就是「无胶片条 + 底部状态栏」，
    /// 那正好也是「专注」预设在版面维度上的样子，于是同一屏里「工作区 = 调色」和
    /// 「预设 = 专注」同时亮着，用户不知道该信哪个。工作区一旦命中，它已经把当前
    /// 布局说清楚了，预设行就该熄灭（等同"自定义"）；工作区不命中时预设照常反推。
    ///
    /// Which preset the panel should light up: **none while a workspace matches**.
    ///
    /// The two controls would otherwise contradict each other: the Develop workspace's
    /// arrangement is exactly "no filmstrip + bottom status bar", which is also what the Focus
    /// preset looks like on the arrangement axis, so one screen would show both "workspace =
    /// Develop" and "preset = Focus" lit. Once a workspace matches it already describes the
    /// layout, so the preset row goes dark (the same reading as "custom"); otherwise presets
    /// reverse-match as before.
    pub fn highlighted_preset(&self, dock_w: f32) -> Option<LayoutPreset> {
        if Workspace::of(*self, dock_w).is_some() {
            None
        } else {
            self.preset()
        }
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

// ---------------------------------------------------------------- 画布让位

/// 画布左右各要让出多少：侧栏宽度 + 一条固定呼吸带。
///
/// 这是 `CANVAS_GAP` 唯一被加进去的地方。中央面板、画布浮层、面板内缘发丝线都从
/// 这组数字反推，谁再自己 `+ 10` 就会在拖宽面板时多出一条缝——判据
/// `ui_tokens::canvas_gap_has_one_source` 盯的就是这件事。
///
/// How much the canvas yields on the left and right: side panel width plus one fixed
/// breathing band. This is the only place `CANVAS_GAP` is added. The centre panel, canvas
/// overlays and the hairline on the panel's inner edge all derive from these numbers;
/// a second `+ 10` anywhere would open an extra seam when the panel is dragged wider
/// (`ui_tokens::canvas_gap_has_one_source` watches for exactly that).
pub fn canvas_insets(show_dock: bool, dock_left: bool, dock_w: f32) -> (f32, f32) {
    if !show_dock {
        return (0.0, 0.0);
    }
    let inset = dock_w.max(0.0) + crate::theme::CANVAS_GAP;
    if dock_left {
        (inset, 0.0)
    } else {
        (0.0, inset)
    }
}

/// 中央区矩形：从 egui 报的剩余区出发，按已知让位量钉死左右边界。
///
/// egui 的 Panel 会用它记住的「上次宽度」缩剩余区，而面板自己的 rect 已经按
/// `exact_size` 走，两者会连着几十帧对不上。以整窗边界和布局常量为准，画布就
/// 永远压不到面板上。
///
/// The centre rectangle: start from the remainder egui reports, then pin the left and
/// right edges from the known insets. egui's Panel shrinks the remainder using a
/// remembered width while its own rect already honours `exact_size`, so the two disagree
/// for dozens of frames. Deriving from the window edges and layout constants keeps the
/// canvas off the panel permanently.
pub fn centre_rect(full: Rect, available: Rect, insets: (f32, f32)) -> Rect {
    let mut r = available;
    r.min.x = r.min.x.max(full.left() + insets.0);
    r.max.x = r.max.x.min(full.right() - insets.1).max(r.min.x + 1.0);
    r
}

/// 呼吸带：`(带矩形, 发丝线所在的 x)`；没有侧栏时返回 `None`。
///
/// 带宽不写死——由让位量减去侧栏宽度得到，所以面板拖宽时带宽恒为常量、位置跟着走。
/// `vertical` 只取上下边界：呼吸带只存在于文档区，不越过菜单行与底部胶片条。
///
/// The gutter: `(band rect, x of the hairline)`, or `None` when no dock is shown.
/// The width is not hardcoded -- it is the inset minus the dock width, so dragging the
/// panel keeps the band a constant width and moves it along. `vertical` supplies only
/// the top and bottom: the gutter belongs to the document area, not the menu row or the
/// bottom filmstrip.
pub fn canvas_gutter(
    full: Rect,
    vertical: Rect,
    insets: (f32, f32),
    dock_w: f32,
) -> Option<(Rect, f32)> {
    let (x0, x1, line_x) = if insets.0 > 0.0 {
        let edge = full.left() + dock_w;
        (edge, full.left() + insets.0, edge)
    } else if insets.1 > 0.0 {
        let edge = full.right() - dock_w;
        (full.right() - insets.1, edge, edge)
    } else {
        return None;
    };
    if x1 <= x0 {
        return None;
    }
    let band = Rect::from_min_max(
        Pos2::new(x0, vertical.top()),
        Pos2::new(x1, vertical.bottom()),
    );
    Some((band, line_x))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_classic() {
        assert_eq!(Layout::default().preset(), Some(LayoutPreset::Classic));
    }

    /// 工作区命中时预设行必须熄灭，否则同一屏两个控件都说"当前是我"。
    ///
    /// A matched workspace must darken the preset row, or one screen has two controls both
    /// claiming to be current.
    #[test]
    fn a_matched_workspace_suppresses_the_preset_highlight() {
        // 调色工作区的版面（无胶片条 + 底部状态栏）在版面维度上恰好等于「专注」。
        //
        // The Develop workspace's arrangement is exactly what Focus looks like.
        let layout = Layout {
            film: FilmSlot::Hidden,
            ..Layout::default()
        };
        assert_eq!(layout.preset(), Some(LayoutPreset::Focus));
        assert_eq!(
            Workspace::of(layout, 340.0),
            Some(Workspace::Develop),
            "调色工作区没有反推出来"
        );
        assert_eq!(
            layout.highlighted_preset(340.0),
            None,
            "工作区命中时预设行仍亮着——同一屏两个控件会互相矛盾"
        );
        // 拖一下宽度就离开工作区，此时预设应当照常反推。
        //
        // Nudge the width out of the workspace and presets match again.
        assert_eq!(
            layout.highlighted_preset(300.0),
            Some(LayoutPreset::Focus),
            "离开工作区后预设应当恢复反推"
        );
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

    fn win() -> Rect {
        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1280.0, 800.0))
    }

    /// 四种 dock 组合：让位量只出现在有面板的那一侧，且恰好是「面板宽 + 呼吸带」。
    ///
    /// Four dock combinations: the inset appears only on the side with a panel, and is
    /// exactly "panel width + gutter".
    #[test]
    fn canvas_insets_four_combinations() {
        let gap = crate::theme::CANVAS_GAP;
        let cases = [
            (false, false, (0.0, 0.0)),
            (false, true, (0.0, 0.0)),
            (true, false, (0.0, 300.0 + gap)),
            (true, true, (300.0 + gap, 0.0)),
        ];
        for (show, left, want) in cases {
            assert_eq!(
                canvas_insets(show, left, 300.0),
                want,
                "show={show} left={left}"
            );
        }
        // 拖宽面板：让位量跟着涨，呼吸带本身不涨。
        //
        // Drag the panel wider: the inset grows with it, the gutter itself does not.
        assert_eq!(canvas_insets(true, false, 420.0).1, 420.0 + gap);
    }

    /// 呼吸带恒为 10px，贴着面板内缘，且不越过文档区上下边界。
    ///
    /// The gutter is always 10 px, sits against the panel's inner edge, and never crosses
    /// the document area's top or bottom.
    #[test]
    fn canvas_gutter_tracks_the_dock_on_both_sides() {
        let full = win();
        let vertical = Rect::from_min_max(Pos2::new(0.0, 80.0), Pos2::new(1280.0, 620.0));
        for (left, want_edge) in [(false, 980.0), (true, 300.0)] {
            let insets = canvas_insets(true, left, 300.0);
            let (band, line_x) =
                canvas_gutter(full, vertical, insets, 300.0).expect("有面板就该有呼吸带");
            assert!(
                (band.width() - crate::theme::CANVAS_GAP).abs() < 1e-4,
                "{band:?}"
            );
            assert!(
                (line_x - want_edge).abs() < 1e-4,
                "left={left} line_x={line_x}"
            );
            assert_eq!(band.top(), vertical.top());
            assert_eq!(band.bottom(), vertical.bottom());
            if left {
                assert_eq!(band.left(), want_edge);
            } else {
                assert_eq!(band.right(), want_edge);
            }
            // 面板拖宽后带宽不变，只是整体平移。
            //
            // After widening the panel the band keeps its width and just slides along.
            let wide = canvas_gutter(full, vertical, canvas_insets(true, left, 420.0), 420.0)
                .expect("宽面板也该有呼吸带");
            assert!((wide.0.width() - crate::theme::CANVAS_GAP).abs() < 1e-4);
        }
        assert!(canvas_gutter(full, vertical, (0.0, 0.0), 300.0).is_none());
    }

    /// 中央区在四种组合下都不越过面板内缘，且至少留 1px 可用宽。
    ///
    /// Under all four combinations the centre rect stays off the panel and keeps at least
    /// 1 px of usable width.
    #[test]
    fn centre_rect_stays_clear_of_the_dock() {
        let full = win();
        for (show, left, dock_w) in [
            (false, false, 300.0),
            (false, true, 300.0),
            (true, false, 300.0),
            (true, true, 300.0),
            (true, false, 560.0),
        ] {
            let insets = canvas_insets(show, left, dock_w);
            let available = Rect::from_min_max(
                Pos2::new(full.left() + if show && left { dock_w } else { 0.0 }, 80.0),
                Pos2::new(
                    full.right() - if show && !left { dock_w } else { 0.0 },
                    620.0,
                ),
            );
            let r = centre_rect(full, available, insets);
            assert!(
                r.width() >= 1.0,
                "show={show} left={left} w={dock_w}: {r:?}"
            );
            if show && left {
                assert!(
                    (r.left() - (full.left() + dock_w + crate::theme::CANVAS_GAP)).abs() < 1e-4
                );
            }
            if show && !left {
                assert!(
                    (r.right() - (full.right() - dock_w - crate::theme::CANVAS_GAP)).abs() < 1e-4
                );
            }
        }
    }
}
