//! 设计令牌与全局样式。
//!
//! 观感对齐 macOS 深色模式（Apple HIG Dark Mode 口径）：
//! - 材质分层取代实线分隔：窗底最暗，内容区略亮，栏/侧栏再亮一档，浮层最亮；
//!   分隔线用白色低 alpha（8–18%）的发丝线，不再像描边。
//! - 文字四级 label：白 85% / 60% / 45% / 10% alpha 压平到实际材质色值，
//!   保证正文级 ≥ 7:1、次要级 ≥ 4.5:1（4K @ 96dpi，正文不许发灰）。
//! - 强调色为专业深蓝 hsl(210, 100%, 44.5) ≈ #0160D8（取色器可调）；选中行背景
//!   是同色约 15% 的压平值。
//! - 圆角同心：窗口/浮层 10，常规控件 6；控件靠底色差与圆角分层，不描边。
//! - 字号字阶：Small 11 / Body·Button 13 / Heading 15（17 档由 FONT_ZOOMS 缩放覆盖）。
//!
//! egui 没有真实背景模糊，所有“材质叠加”都预计算成不透明色值写死在常量里，
//! 只有分隔线保留 alpha，让它叠在任何材质上都是同一根发丝线。
//! 素材与图标仍然全部自绘，不引入任何系统资源。
//!
//! Design tokens and global styles.
//!
//! Look and feel follow macOS dark mode (Apple HIG Dark Mode):
//! - Material layers replace solid separators: window background darkest, content area a
//!   little lighter, bars and sidebars one step up, floating layers lightest; separators
//!   are hairline strokes of low-alpha white (8-18%) rather than outlines.
//! - Four text levels: white at 85% / 60% / 45% / 10% alpha flattened onto the actual
//!   material colour, guaranteeing body text >= 7:1 and secondary text >= 4.5:1 (4K at
//!   96 dpi; body text is not allowed to go grey).
//! - The accent is a professional deep blue, hsl(210, 100%, 44.5) ~ #0160D8 (tunable
//!   by the colour picker); a selected row's background is the same hue flattened at
//!   roughly 15%.
//! - Concentric corner radii: windows and floating layers 10, ordinary controls 6;
//!   controls are separated by fill difference and radius, never by outlines.
//! - Type scale: Small 11 / Body and Button 13 / Heading 15 (the 17 step is covered by
//!   the FONT_ZOOMS steps).
//!
//! egui has no real background blur, so every "material overlay" is precomputed into an
//! opaque colour baked into a constant; only separators keep alpha, so the same hairline
//! sits on any material. Assets and icons are still all drawn in code, with no system
//! resources pulled in.

use egui::{
    Align2, Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Pos2, RichText,
    Stroke, TextStyle, Vec2,
};

/// 把白色以 alpha/255 压平到基色上，得到 macOS“深底 + 白叠加”的实际不透明色。
///
/// Flatten white at alpha/255 over a base colour to get the opaque value macOS actually
/// renders for "dark material plus white overlay".
pub fn overlay_white(base: Color32, white_alpha: u8) -> Color32 {
    let a = white_alpha as u32;
    let blend = |b: u8| -> u8 {
        let v = (255 * a + b as u32 * (255 - a) + 127) / 255;
        if v > 255 { 255 } else { v as u8 }
    };
    Color32::from_rgb(blend(base[0]), blend(base[1]), blend(base[2]))
}

/// sRGB 相对亮度（WCAG 口径），用于对比度断言。
///
/// sRGB relative luminance (WCAG definition), used by the contrast assertions.
pub fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| -> f32 {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// 前景压平到背景上的 WCAG 对比度（先把带 alpha 的前景叠实）。
///
/// WCAG contrast of a foreground flattened onto a background (an alpha foreground is
/// composited first).
pub fn contrast(fg: Color32, bg: Color32) -> f32 {
    let flat = if fg.a() == 255 {
        fg
    } else {
        let a = fg.a() as f32 / 255.0;
        Color32::from_rgb(
            (fg[0] as f32 * a + bg[0] as f32 * (1.0 - a)).round() as u8,
            (fg[1] as f32 * a + bg[1] as f32 * (1.0 - a)).round() as u8,
            (fg[2] as f32 * a + bg[2] as f32 * (1.0 - a)).round() as u8,
        )
    };
    let (l1, l2) = (luminance(flat), luminance(bg));
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

// ============================================================================
// 三层令牌：参数 → 派生 → 组件
//
//   L1 `Appearance`：用户改的就是这几个数（明暗 / 皮肤 / 主色 / 圆角）
//   L2 `Palette`  ：39 个色值全部由 L1 算出来，任何人不许手改某一通道
//   L3 组件       ：只调访问器 `bg()` `panel()` `accent()`，不认识深色还是浅色
//
// 换皮肤 = 改两个数（色相 + 饱和）；切明暗 = 换一张亮度表；换主色 = 三个数。
// 浅色主题不是第二套界面，而是同一组公式把亮度方向（`dir`）从 +1 翻成 -1：
// 顶高光归零、向白叠加改成向黑叠加、派生偏移整体反向。
// ============================================================================

/// 明暗模式。
///
/// 「跟随系统」要等 winit 的 `ThemeChanged` 事件接进来（见开发看板 S1），
/// 现在只给三个确定值：宁可少一个开关，也不要一个点不动的开关。
///
/// Light or dark mode.
///
/// "Follow the system" has to wait for winit's `ThemeChanged` event to be wired in (see
/// board item S1); for now only three definite values are offered — better one switch
/// short than a switch that does not move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeMode {
    Dark,
    Black,
    Light,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [Self::Dark, Self::Black, Self::Light];

    pub fn cn(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Black => "纯黑",
            Self::Light => "浅色",
        }
    }
}

/// 中性色倾向：所有「灰」都是同一色相上的不同亮度，所以换皮肤只改这两个数。
///
/// Neutral tint: every "grey" is a different lightness of the same hue, so a skin swap
/// changes only these two numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Skin {
    Graphite,
    Steel,
    Sand,
    Jade,
}

impl Skin {
    pub const ALL: [Skin; 4] = [Self::Graphite, Self::Steel, Self::Sand, Self::Jade];

    pub fn cn(self) -> &'static str {
        match self {
            Self::Graphite => "石墨",
            Self::Steel => "蓝钢",
            Self::Sand => "暖砂",
            Self::Jade => "青灰",
        }
    }

    /// (色相, 饱和)。这四个数决定了整个界面偏冷还是偏暖。
    ///
    /// Graphite 取设计稿 `--card #212327 / --grp #2a2d32 / --raised #33363c`
    /// 反解出的 HSL（h≈220°, s≈8%）。旧值 (240°, 2%) 派生出的灰 R=G，
    /// 在屏幕上读作中性灰而不是设计稿的蓝灰板岩，整块面板因此发闷。
    ///
    /// (hue, saturation). Across the four skins these numbers decide whether the whole
    /// interface leans cool or warm.
    ///
    /// Graphite takes the HSL solved from the design's `--card #212327`,
    /// `--grp #2a2d32` and `--raised #33363c` (h~220 deg, s~8 %). The old (240 deg, 2 %)
    /// derived neutrals with R=G, which read as plain grey instead of the mockup's
    /// blue-slate, so the whole panel looked flat.
    pub fn hsl(self) -> (f32, f32) {
        match self {
            Self::Graphite => (220.0, 8.0),
            Self::Steel => (214.0, 14.0),
            Self::Sand => (34.0, 10.0),
            Self::Jade => (178.0, 9.0),
        }
    }
}

/// L1：外观参数。落盘就是 `settings.json` 里的 `appearance` 段。
///
/// L1: the appearance parameters. Persisted as the `appearance` section of
/// `settings.json`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: ThemeMode,
    pub skin: Skin,
    /// 强调色，HSL 三数：改这三个数，36 处半透明底与全部描边一起变。
    ///
    /// Accent colour as three HSL numbers: change these and 36 translucent fills plus every
    /// stroke change with them.
    pub accent_h: f32,
    pub accent_s: f32,
    pub accent_l: f32,
    /// 界面明暗微调：−3…+3，每档 ±2 L，只作用于背景族（面板/栏/控件底），
    /// 文字三档不动。默认 0；旧 settings.json 缺这一项时读成 0。
    ///
    /// Interface brightness trim: -3..+3, two L per step, applied to the background
    /// family only (panels, bars, control fills); the three text steps never move.
    /// Defaults to 0, and an older settings.json without the key reads back as 0.
    pub gray_offset: i8,
    /// 控件圆角。窗口与容器由它派生，不单独给。
    ///
    /// Control corner radius. Windows and containers are derived from it, never set
    /// separately.
    pub radius: f32,
    /// 外壳形态：跟随编译目标，或强制成另一家的样子（截图对比用）。
    ///
    /// Shell form: follow the build target, or force the other platform's look (for
    /// screenshot comparisons).
    pub os: crate::platform::OsChoice,
    /// 调整面板行高密度档（紧凑/标准/宽松）。
    ///
    /// Row-density step for the adjustment panel (compact / standard / roomy).
    pub density: Density,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Dark,
            skin: Skin::Graphite,
            // 默认强调色走专业口径：hsl(210,100%,44.5%) ≈ #0160d8 一档深蓝。
            // 亮蓝（l=52，#0A84FF）是消费级观感；LightCraft/LR 类校色工具普遍
            // 压低强调色亮度与面积，避免干扰色彩判断（逆向报告 R10，实测
            // LightCraft 默认 #0165dd ≈ l 43.5）。取色器仍可调回任意值。
            //
            // The default accent follows the pro-convention depth: hsl(210,100 %,44.5 %)
            // sits at the #0160d8 step. The brighter l=52 (#0A84FF) reads consumer;
            // colour-grading tools keep the accent dark and quiet so it never competes
            // with the image (reverse-report R10; LightCraft ships #0165dd at l 43.5).
            // The picker can still take it anywhere.
            accent_h: 210.0,
            accent_s: 100.0,
            accent_l: 44.5,
            gray_offset: 0,
            radius: 6.0,
            os: crate::platform::OsChoice::Auto,
            density: Density::Standard,
        }
    }
}

/// 强调色预设：面板上排七个色点，点一下就是这套数。
///
/// Accent presets: the seven dots across the panel, one click each.
pub const ACCENT_PRESETS: [(f32, f32, f32, &str); 8] = [
    (210.0, 100.0, 44.5, "蓝"),
    (265.0, 86.0, 63.0, "紫"),
    (188.0, 92.0, 46.0, "青"),
    (148.0, 56.0, 44.0, "绿"),
    (32.0, 94.0, 53.0, "琥珀"),
    (20.0, 90.0, 48.0, "橙"),
    (345.0, 84.0, 61.0, "玫红"),
    (0.0, 0.0, 60.0, "银"),
];

/// 一张亮度表。
///
/// 深色 / 纯黑 / 浅色只是这张表里的数不同，派生公式一个字都不用改；
/// `dir` 是明暗方向（深色 +1、浅色 −1），所有「比基准亮一档」的派生都乘它。
struct Ladder {
    surround: f32,
    /// 顶栏底色：设计稿 `--bar #16171b`（L9.6），是全表最暗的一档——
    /// 顶栏是「壳」，必须比 field/card 都深，画面才压得住。
    ///
    /// The top bar fill: the design's `--bar #16171b` (L9.6), the darkest step of the
    /// whole table -- the bar is the "shell" and must sit below field/card.
    bar: f32,
    canvas: f32,
    panel: f32,
    dock: f32,
    chrome: f32,
    bg: f32,
    header: f32,
    elev: f32,
    elev_h: f32,
    field: f32,
    field_hover: f32,
    thumb: f32,
    text: f32,
    muted: f32,
    dim: f32,
    dir: f32,
}

// 2026-11-07 整表上移到 ACR 暖深灰（用户钦定「颜色跟 PS 一样」，复刻稿
// aevum-acr-replica-v3）：面板系 +3 档（panel 14→17 ≈ #363638 的 L 值），
// 文字三档同步微调保住 WCAG（dim 59→62：bg 变亮后 4.5:1 的压线判据）。
// 2026-11-10（用户钦定 docs/aevum-v12-timeline.html + aevum-v10.6-complete.html
// 为重构目标）：整表按设计稿 token 重排。设计稿是「深底 + 明显分层」的语法——
// bar #16171b(L9.6) < field #1a1c20(L11.4) < card #212327(L14.1) < grp #2a2d32(L18.0)
// < raised #33363c(L21.8) < raised2 #3d4046(L25.7)，比旧表的 20/20.3/20.6/21「四档
// 几乎同灰」拉开了三倍差值。用户此前反复说「对比不足」「一块灰一块黑」，根因就是
// 旧表这几档挤在 1% 内。
//
// 2026-11-10 (the user pinned docs/aevum-v12-timeline.html and
// aevum-v10.6-complete.html as the rebuild targets): the whole table is re-laid per
// the design tokens. The design's grammar is "dark base + pronounced layering":
// bar < field < card < grp < raised < raised2, a spread roughly 3x wider than the old
// 20/20.3/20.6/21 block, where four steps lived inside 1% -- the root cause of the
// repeated "contrast too low" / "one grey patch next to one black patch" feedback.
const DARK: Ladder = Ladder {
    bar: 9.6,      // --bar #16171b
    surround: 7.5, // 设计稿画布外圈：比画布再暗一档，把视线压向图像
    canvas: 9.0,   // 画布内容区
    panel: 14.1,   // card #212327：卡片底（检查器、直方图卡）
    dock: 15.2,    // 检查器比卡片亮一档
    chrome: 17.0,  // grp #2a2d32(L18.0) 收一档留阶梯余量
    bg: 19.0,      // bar 一族的最高档（顶/底栏实际取面板色，见 shell）
    header: 19.0,  //   ↳ 判据链要求 bg 在 chrome 之上；设计稿的 bar 虽最暗，
    //     但那是「顶栏用深色」，与本表的阶梯派生是两回事，
    //     顶栏底色由 shell 直接取 panel()，不读 bg。
    elev: 21.8,   // raised #33363c：控件底
    elev_h: 25.7, // raised2 #3d4046：悬停抬升
    field: 11.4,  // field #1a1c20：数值框/轨道槽
    field_hover: 13.0,
    thumb: 6.0,
    text: 95.9, // #f2f3f7
    // 三级字的节奏向 LightCraft Charcoal 看齐（88.6/73.7/55.7）：MUTED 抬到 71.5，
    // 与 DIM 拉开 12.5 档（原 7.5 档贴在一起，标签与辅助字读不出层级）。
    // DIM 不能降——它对 bg(18.2) 的 4.5:1 判据本来就压线（约 4.6），
    // 抬 MUTED 是唯一同时满足 WCAG 与层级差的移动方向。
    //
    // The three text steps follow LightCraft's Charcoal rhythm (88.6/73.7/55.7):
    // MUTED rises to 71.5, opening a 12.5-step gap to DIM (the old 7.5-step squeeze
    // read as one level). DIM cannot go lower -- its 4.5:1 ratio against bg(18.2)
    // already sits at the edge (~4.6), so raising MUTED is the only move that
    // satisfies both WCAG and the hierarchy gap.
    muted: 79.8, // text2 #c6c9d1
    dim: 62.0,   // 设计稿 text3 #8f939c(L58.6) 在 s=2% 冷灰下对 bar 只有 4.49，
    // 抬到 62 才稳过 WCAG AA 的 4.5:1（设计稿原值差 0.01）。
    //
    // The design's text3 #8f939c (L58.6) reaches only 4.49 against
    // bar once the 2% cool tint is applied; 62 clears WCAG AA's
    // 4.5:1 (the design value was off by 0.01).
    dir: 1.0,
};

const BLACK: Ladder = Ladder {
    bar: 3.0,
    surround: 2.0,
    canvas: 3.5,
    panel: 6.0,
    dock: 7.0,
    chrome: 8.0,
    bg: 9.0,
    header: 5.0,
    elev: 12.0,
    elev_h: 18.0,
    field: 4.5,
    field_hover: 6.5,
    thumb: 1.5,
    text: 86.0,
    muted: 61.0,
    dim: 52.0,
    dir: 1.0,
};

// 浅色表：层级方向与深色一致（surround 最暗、bg 最亮），只是整体翻到亮端。
// 次要文字在浅底上必须比深底更狠地压暗 —— 白底上的 4.5:1 比黑底难得多，
// 55% 的灰在 96% 的面板上只有 4.49，差一点点就是不达标。
const LIGHT: Ladder = Ladder {
    bar: 96.5,
    surround: 86.0,
    canvas: 92.0,
    panel: 96.0,
    dock: 97.0,
    chrome: 98.0,
    bg: 100.0,
    header: 98.5,
    elev: 89.0,
    elev_h: 83.0,
    field: 100.0,
    field_hover: 96.0,
    thumb: 82.0,
    text: 18.0,
    muted: 32.0,
    dim: 38.0,
    dir: -1.0,
};

/// L2：一套完整的派生色。`Copy` 是有意的 —— 它只有 39 个 Color32，
/// 每帧拷几百次比到处借引用便宜，也不会把借用检查搅进绘制代码。
///
/// L2: one complete derived palette. `Copy` is deliberate — it is only 39 `Color32`s, so
/// copying it a few hundred times a frame is cheaper than borrowing it everywhere, and it
/// keeps the borrow checker out of drawing code.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bar: Color32,
    pub bg: Color32,
    pub chrome: Color32,
    pub panel: Color32,
    pub dock: Color32,
    pub header: Color32,
    pub surround: Color32,
    pub canvas: Color32,
    pub rail: Color32,
    pub elev: Color32,
    pub elev_hover: Color32,
    /// 按下那一档：比悬停再收一档（深色更暗、浅色也更深）。按下按钮的
    /// 反馈是「往里沉」，不是整块亮起强调色——那是塑料感的来源。
    ///
    /// The pressed step: one notch further down from hover (darker in dark themes, deeper
    /// in light ones too). Pressing a button should feel like sinking in, not lighting the
    /// whole thing up in the accent colour — that is where the plastic look comes from.
    pub press: Color32,
    pub selected: Color32,
    /// 选中行的横向渐变末端：从 `selected` 衰减到这一档，左深右浅的光带。
    ///
    /// End of the selected row's horizontal gradient: fades from `selected` to this step, a
    /// band that is dark on the left and light on the right.
    pub selected_row_end: Color32,
    pub field: Color32,
    pub field_hover: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub divider: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub dim: Color32,
    pub value: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_dim: Color32,
    pub rail_fill: Color32,
    pub track: Color32,
    pub temp_cold: Color32,
    pub temp_warm: Color32,
    pub tint_green: Color32,
    pub tint_magenta: Color32,
    pub star: Color32,
    pub warn: Color32,
    pub teal: Color32,
    pub danger: Color32,
    pub ok: Color32,
    pub thumb_bg: Color32,
    /// 强调底上的字：亮底配深字。
    ///
    /// Text on an accent fill: dark text on a light fill.
    pub on_accent: Color32,
    /// 选中底上的字（深色白字，浅色用正文色）。
    ///
    /// Text on a selected fill (white in dark themes, body colour in light ones).
    pub on_selected: Color32,
    /// 抬起控件（ELEV）上的字。
    ///
    /// Text on raised controls (ELEV).
    pub on_elev: Color32,
}

impl Palette {
    /// 由参数算出整套色值。这是唯一的派生入口。
    ///
    /// Derive the whole palette from the parameters. This is the only derivation entry
    /// point.
    pub fn of(a: &Appearance) -> Self {
        let l = match a.theme {
            ThemeMode::Dark => &DARK,
            ThemeMode::Black => &BLACK,
            ThemeMode::Light => &LIGHT,
        };
        let (h, s) = a.skin.hsl();
        let n = |lightness: f32| hsl(h, s, lightness);
        let dir = l.dir;
        // 深色向白叠加、浅色向黑叠加：所有「加亮」的派生都走这一条。
        let ink = if dir > 0.0 {
            Color32::WHITE
        } else {
            Color32::BLACK
        };
        let alpha = |dark: u8, light: u8| if dir > 0.0 { dark } else { light };

        // 界面明暗微调（自定义外观，设计稿 §2.3）：`nb` = 带偏移的 `n`，只给背景族用。
        // 每档 ±2 L，范围 ±3。文字三档（text/muted/dim）仍走 `n` —— 文字一起漂，
        // 这个滑块就变成对比度旋钮，可读性下限守不住。
        //
        // Interface brightness trim (custom appearance, design spec 2.3): `nb` is `n`
        // with the offset, applied to the background family only. Two L per step, range
        // +/-3. The three text steps keep using `n`: shifting them too would turn this
        // slider into a contrast knob and break the readability floor.
        let off = a.gray_offset as f32 * 2.0;
        let nb = |lightness: f32| n((lightness + off).clamp(0.0, 100.0));
        let panel = nb(l.panel);
        let accent = hsl(a.accent_h, a.accent_s, a.accent_l);
        let text = n(l.text);

        Self {
            bar: nb(l.bar),
            bg: nb(l.bg),
            chrome: nb(l.chrome),
            panel,
            dock: nb(l.dock),
            header: nb(l.header),
            surround: nb(l.surround),
            canvas: nb(l.canvas),
            rail: nb(l.chrome),
            elev: nb(l.elev),
            elev_hover: nb(l.elev_h),
            press: nb(l.elev - 3.0 * dir),
            // 选中底 0.15（D2「chrome 退后」）：accent 面积减半，页签下划线与
            // 描边继续承担「选中」语义，底色只作极淡的落位提示。
            //
            // Selected fill at 0.15 (D2 "chrome steps back"): accent area is halved;
            // the tab underline and strokes keep carrying the selected meaning, and
            // the fill only hints at where the selection sits.
            selected: mix(panel, accent, 0.15),
            // 渐变从 selected 衰到贴着 dock 的一档：一条式三表自动成立，
            // 换主色时两端一起动，不会出现左蓝右灰的断裂。
            selected_row_end: mix(nb(l.dock), accent, 0.05),
            field: nb(l.field),
            field_hover: nb(l.field_hover),
            border: with_alpha(ink, alpha(20, 34)),
            border_strong: with_alpha(ink, alpha(46, 60)),
            // 分隔线提亮一档：分组分隔、曲线网格主次线、标尺刻度都走这一档，
            // 原来约 10%（深）/ 13%（浅）白太淡、读不出分组，抬到约 15% / 19%。
            // 只动 divider，不动 border / border_strong，避免按钮描边与强边框跟着变亮。
            //
            // Dividers lifted one step: group separators, curve-grid lines and ruler ticks
            // all share this step; the old ~10% (dark) / ~13% (light) white was too faint
            // to read groups, now ~15% / ~19%. Only divider changes — border and
            // border_strong stay put so button strokes and strong outlines do not brighten.
            divider: with_alpha(ink, alpha(38, 48)),
            text,
            muted: n(l.muted),
            dim: n(l.dim),
            // 读数保持中性（无彩纪律）：旧版是「强调色降饱和加亮」的淡蓝，一屏十几
            // 行未改动读数铺下来像一片低饱和报错。灰一档就够读，强调色完整留给
            // 交互态（悬停 / 拖动 / 选中）。
            //
            // Readouts stay neutral (achromatic discipline): the old value was the
            // accent desaturated and lightened, and a screenful of untouched readouts
            // read as faint errors. One grey step below muted is enough; accent stays
            // reserved for interaction (hover / drag / selected).
            value: n(l.muted - 2.0),
            accent,
            accent_hover: hsl(a.accent_h, a.accent_s, a.accent_l + 10.0),
            accent_dim: mix(panel, accent, 0.17),
            rail_fill: mix(panel, ink, 0.18),
            // 滑轨对齐界面稿 `--track`：抬升面往回收两度，而不是从面板压暗。
            track: nb(l.elev - 2.0 * dir),
            // 材质色不随主题变：它们表达的是「冷/暖/绿/品红」本身，不是材质。
            temp_cold: Color32::from_rgb(0x4E, 0x86, 0xC8),
            temp_warm: Color32::from_rgb(0xD8, 0xA4, 0x55),
            tint_green: Color32::from_rgb(0x63, 0xB4, 0x6E),
            tint_magenta: Color32::from_rgb(0xB9, 0x66, 0xB0),
            star: Color32::from_rgb(0xFF, 0xD6, 0x0A),
            warn: Color32::from_rgb(0xFF, 0x9F, 0x0A),
            teal: Color32::from_rgb(0x64, 0xD2, 0xFF),
            danger: Color32::from_rgb(0xFF, 0x69, 0x61),
            ok: Color32::from_rgb(0x30, 0xD1, 0x58),
            thumb_bg: nb(l.thumb),
            on_accent: if luminance(accent) > 0.56 {
                hsl(a.accent_h, 45.0, 12.0)
            } else {
                Color32::WHITE
            },
            on_selected: if dir > 0.0 { Color32::WHITE } else { text },
            on_elev: if dir > 0.0 { Color32::WHITE } else { text },
        }
    }
}

/// HSL → sRGB。三个分量都用 0–360 / 0–100 的直觉单位，和设计稿一致。
///
/// HSL to sRGB. All three components use intuitive 0-360 / 0-100 units, matching the
/// design mockup.
pub fn hsl(h: f32, s: f32, l: f32) -> Color32 {
    let h = h.rem_euclid(360.0) / 360.0;
    let s = (s / 100.0).clamp(0.0, 1.0);
    let l = (l / 100.0).clamp(0.0, 1.0);
    if s <= f32::EPSILON {
        let v = (l * 255.0).round() as u8;
        return Color32::from_rgb(v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |t: f32| {
        let t = if t < 0.0 {
            t + 1.0
        } else if t > 1.0 {
            t - 1.0
        } else {
            t
        };
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

/// 线性混合两色（`t` = 0 取 a，1 取 b）。
///
/// Linear blend of two colours (`t` = 0 gives a, 1 gives b).
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 * (1.0 - t) + y as f32 * t).round() as u8;
    Color32::from_rgb(f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2]))
}

fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], a)
}

// ---- 全局状态：外观参数 + 当前色板 ------------------------------------------
//
// 界面只跑在 UI 线程上，但 `theme::apply` 会被自检、截图与主循环三方调用，
// 所以用一个进程级 `RwLock` 而不是 thread_local：读多写少，读者只拷贝 39 个色。

struct ThemeState {
    appearance: Appearance,
    palette: Palette,
}

static THEME: std::sync::OnceLock<std::sync::RwLock<ThemeState>> = std::sync::OnceLock::new();

fn state() -> &'static std::sync::RwLock<ThemeState> {
    THEME.get_or_init(|| {
        let appearance = Appearance::default();
        std::sync::RwLock::new(ThemeState {
            palette: Palette::of(&appearance),
            appearance,
        })
    })
}

/// 当前参数。
///
/// The current parameters.
pub fn appearance() -> Appearance {
    state().read().map(|s| s.appearance).unwrap_or_default()
}

/// 当前色板。绘制代码唯一该读的东西。
///
/// The current palette. The only thing drawing code should read.
pub fn palette() -> Palette {
    state()
        .read()
        .map(|s| s.palette)
        .unwrap_or_else(|_| Palette::of(&Appearance::default()))
}

/// 换一套参数并立刻重算色板；调用方随后要再调一次 [`apply`] 把 egui 的
/// `Visuals` 也刷一遍（那里还压着一堆派生值）。
///
/// Swap in a new set of parameters and recompute the palette at once; the caller then
/// calls [`apply`] again to refresh egui's `Visuals` (a pile of derived values sits in
/// there too).
pub fn set_appearance(a: Appearance) {
    if let Ok(mut s) = state().write() {
        s.appearance = a;
        s.palette = Palette::of(&a);
    }
}

/// 当前是不是浅色底：少数几处（阴影、发丝线权重）需要知道方向。
///
/// Whether the current background is light: a handful of places (shadows, hairline
/// weights) need to know the direction.
pub fn is_light() -> bool {
    appearance().theme == ThemeMode::Light
}

// ---- L3：组件只认这些访问器 ------------------------------------------------
macro_rules! accessors {
    ($($name:ident),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> Color32 {
                palette().$name
            }
        )*
    };
}

accessors!(
    bar,
    bg,
    chrome,
    panel,
    dock,
    header,
    surround,
    canvas,
    rail,
    elev,
    elev_hover,
    press,
    selected,
    selected_row_end,
    field,
    field_hover,
    border,
    border_strong,
    divider,
    text,
    muted,
    dim,
    value,
    accent,
    accent_hover,
    accent_dim,
    rail_fill,
    track,
    temp_cold,
    temp_warm,
    tint_green,
    tint_magenta,
    star,
    warn,
    teal,
    danger,
    ok,
    thumb_bg,
    on_accent,
    on_selected,
    on_elev,
);

/// 当前强调色的 sRGB 三元组（取色器回显用）。
///
/// The current accent as an sRGB triple (for the colour picker's readout).
pub fn accent_rgb() -> [u8; 3] {
    let c = accent();
    [c[0], c[1], c[2]]
}

/// sRGB → HSL，与 [`hsl`] 互逆。取色器给的是 RGB，而参数存的是 HSL：
/// 存 HSL 是为了让「同一主色的深浅档」能算出来，存 RGB 就只能一个个手挑。
///
/// sRGB to HSL, the inverse of [`hsl`]. The picker speaks RGB while the parameters store
/// HSL: storing HSL is what makes "the light and dark steps of one accent" computable —
/// with RGB you would have to pick each one by hand.
pub fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let l = (max + min) / 2.0;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l * 100.0);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - r).abs() < f32::EPSILON {
        ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() < f32::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (
        (h * 60.0).rem_euclid(360.0),
        (s * 100.0).clamp(0.0, 100.0),
        (l * 100.0).clamp(0.0, 100.0),
    )
}

/// 控件圆角：只由 `Appearance::radius` 派生，窗口 = 控件 + 4、容器 = 控件 + 2。
///
/// Control corner radius: derived only from `Appearance::radius`, where window = control
/// + 4 and container = control + 2.
pub fn radius_control() -> f32 {
    // 平台偏置不落进 `Appearance::radius`：用户设的是"这一档"，mac 整体再圆一格
    // 是平台口径，改外观时不该把它一起改掉（否则来回切平台会把数值越推越大）。
    (appearance().radius + crate::platform::chrome().radius_bias).clamp(0.0, 18.0)
}

/// 小徽标/芯片圆角：比控件再收 2px。全应用的字面量 2.0/3.0/8.0 都收到这一档，
/// 避免"小方块圆角肉眼可见地不一致"（审计 #37）。
///
/// Corner radius for chips and small badges: control radius minus 2 px. The literals
/// 2.0 / 3.0 / 8.0 all collapse to this one step so small blocks stop looking randomly
/// rounded.
/// M3 shape tokens：4（chip）/ 8（control）/ 12（panel）/ 16（window）。
/// 圆角只允许这四档，别再按 `radius_control ± n` 派生——派生值会随外观旋钮
/// 漂移，同一屏上的徽标与卡片就对不上。
///
/// M3 shape tokens: 4 / 8 / 12 / 16. Only these four steps are allowed; deriving from
/// `radius_control +- n` makes chips and cards drift apart as the appearance knob moves.
pub fn radius_chip() -> f32 {
    4.0
}

pub fn radius_window() -> f32 {
    16.0
}

pub fn radius_panel() -> f32 {
    12.0
}

/// 面板行的悬停底色：深色主题往上抬一档，浅色主题往下压一档 —— 两个方向都是
/// "离背景远一点"。用固定 alpha 的白叠在黑上是不行的：浅色主题里会越叠越白，
/// 高亮反而消失。
///
/// Hover fill for panel rows: one step up in dark themes, one step down in light ones —
/// both directions mean "further from the background". Flattening a fixed-alpha white over
/// black cannot work: in a light theme it gets whiter and whiter and the highlight
/// disappears.
pub fn row_hover() -> Color32 {
    let p = palette().panel;
    if is_light() {
        let f = |v: u8| -> u8 { (v as u32 * 94 / 100) as u8 };
        Color32::from_rgb(f(p[0]), f(p[1]), f(p[2]))
    } else {
        overlay_white(p, 7)
    }
}

/// 分组卡底（设计稿 `.grp{background:#2a2d32}`）：比面板抬一档，靠色差而不是
/// 双描边说「这是一块」。旧表把卡片也画成 panel，卡与底只差 1–3 级灰，整列读
/// 成一块；设计稿是 panel L14.1 → grp L18，差约 4 级。
///
/// Group-card fill (design `.grp{background:#2a2d32}`): one step above the panel, so
/// the block reads by tonal difference instead of a double outline. The old table
/// painted cards with `panel` too, leaving only 1-3 lightness steps between card and
/// background; the design moves from panel L14.1 to grp L18, about four steps.
/// 整页工作台底色：设计稿 `.wsview{background:#101114}`。
///
/// 比 `bg` 再沉一档，卡片（`panel` = #212327）浮上来才有「页 vs 卡」两层。
/// 纯黑主题不去硬凑 #101114（黑底上 #212327 卡片会显脏），跟着黑阶走一档。
///
/// Full-page bench background per the design's `.wsview{background:#101114}`.
///
/// One step below `bg`, so cards (`panel` = #212327) actually float and the page reads
/// as two layers. The pure-black theme does not force #101114 (a #212327 card on black
/// looks dirty); it steps down its own ladder instead.
pub fn bench_bg() -> Color32 {
    match appearance().theme {
        ThemeMode::Light => Color32::from_rgb(0xf2, 0xf3, 0xf5),
        ThemeMode::Black => Color32::from_rgb(0x0b, 0x0c, 0x0d),
        ThemeMode::Dark => Color32::from_rgb(0x10, 0x11, 0x14),
    }
}

/// 画布衬底（Fit 模式露出照片外的区域）：严格中性中灰，R=G=B。
/// 不同宽高比的照片在固定视窗里必然留下余量，衬底若是纯黑或带色相，会形成
/// 「黑边」并污染色彩判断；中性中灰是 Lightroom / Capture One 的同一套做法。
///
/// Canvas matte (the area exposed around a photo in Fit mode): strictly neutral
/// grey, R=G=B. Photos with different aspect ratios inevitably leave spare space
/// in a fixed viewport; a black or tinted matte becomes a "black border" and
/// biases colour judgement. Neutral mid-grey is what Lightroom/Capture One use.
pub fn matte() -> Color32 {
    if is_light() {
        Color32::from_rgb(0xE6, 0xE6, 0xE6)
    } else {
        Color32::from_rgb(0x1F, 0x1F, 0x1F)
    }
}

pub fn group_fill() -> Color32 {
    // 深色石墨直接用设计稿 `.grp{background:#2a2d32}`：从 panel 往白叠出来的
    // 灰是中性灰(40,40,41)，而设计稿的卡面带蓝(42,45,50)。这 5~9 个蓝通道差
    // 就是「卡片发灰、不像设计稿」的实测来源，靠混白永远混不出色相。
    //
    // Dark Graphite uses the design's `.grp{background:#2a2d32}` verbatim: blending
    // panel toward white gives a neutral (40,40,41), while the design card is blue-grey
    // (42,45,50). That 5-9 step blue gap is the measured "grey, not like the mockup"
    // difference -- mixing toward white can never add hue.
    if matches!(appearance().theme, ThemeMode::Dark) && appearance().skin == Skin::Graphite {
        return Color32::from_rgb(0x2A, 0x2D, 0x32);
    }
    let p = palette().panel;
    if is_light() {
        mix(p, Color32::BLACK, 0.035)
    } else {
        mix(p, Color32::WHITE, 0.045)
    }
}

/// 分组卡描边（设计稿 `--ring: rgba(255,255,255,.06)` 的 1px 环）。比 `divider`
/// 淡一档：环只负责收边，不负责分隔。
///
/// Group-card ring (the design's 1 px `--ring` at 6 % white). One step lighter than
/// `divider`: a ring closes the edge, it does not separate sections.
pub fn group_ring() -> Color32 {
    with_alpha(
        if is_light() {
            Color32::BLACK
        } else {
            Color32::WHITE
        },
        if is_light() { 30 } else { 26 },
    )
}

/// 滑块轨道（设计稿 `.track{background:#43464e}`）：卡片底上的实色灰轨。
/// 旧版把通用轨画成「左深右白」的渐变，整列白尾巴比读数还抢眼；参考图的轨道
/// 是同一根灰轨，量程只由滑块位置表达。
///
/// Slider rail (design `.track{background:#43464e}`): a flat grey rail on the card.
/// The old generic rail ramped from dark to near-white, and the bright right end
/// outweighed the readouts; the reference keeps one flat grey and lets the handle
/// state the range.
pub fn slider_rail() -> Color32 {
    let c = group_fill();
    if is_light() {
        mix(c, Color32::BLACK, 0.12)
    } else {
        mix(c, Color32::WHITE, 0.12)
    }
}

/// 滑块拇指（设计稿 `.track i{background:#c9ccd3; width:10px; height:10px}`）。
/// 用略灰的实心点而不是正文白：纯白点压在渐变轨上像一颗 LED，灰一点才读成
/// 「量程上的一个把手」。
///
/// Slider thumb (design `.track i`: 10 px, `#c9ccd3`). A slightly grey solid dot
/// instead of body white: a pure-white dot on a gradient rail reads as an LED, while
/// the grey one reads as a handle on a range.
pub fn knob() -> Color32 {
    if is_light() {
        Color32::from_rgb(0x6A, 0x6E, 0x76)
    } else {
        Color32::from_rgb(0xC9, 0xCC, 0xD3)
    }
}

// 尺寸全部对齐界面设计稿的 L3 段：
// `--opt-h:44 --rail-w:52 --left-w:240 --dock-w:372 --strip-w:28 --tl-h:120`。
// 这几个数是"整套界面读起来像不像那张图"的根：差 6px 的轨宽、差 70px 的胶片条，
// 单独看都无所谓，摆在一起就是另一种软件。
/// 顶栏高度 44：设计稿 `.toolbar{height:44px}`。
///
/// 44 px per the design's `.toolbar{height:44px}`.
pub const MENU_H: f32 = 44.0;
/// 弹层首帧宽度 240：egui 的 `Area` 在没有「上次尺寸」记忆时回落到全局
/// `Spacing::default_area_size`（egui 默认 600×400），而菜单弹层用的是
/// `Layout::top_down_justified`，于是**重启后第一次**打开菜单会铺成一条 600px 宽的空板子
/// （第二次打开已经有尺寸记忆，贴内容——这就是「之前正常、现在不对」的来源）。
///
/// 240 是菜单条目实测需要的一档：内容更宽时 `Area` 自己会长，不会截断长条目；
/// 我们自己的五个浮窗全部显式设了 `default_width`/`default_size`，下拉框走
/// `Spacing::combo_width`，tooltip 走 `Spacing::tooltip_width`，都不吃这个默认值。
///
/// Popup first-frame width, 240 px. An `Area` with no remembered size falls back to the
/// global `Spacing::default_area_size` (egui defaults to 600x400) and menu popups use a
/// `top_down_justified` layout, so the first menu opened after a restart stretched into a
/// 600 px empty slab; the second open already has a remembered size and fits its content,
/// which is exactly the "it used to be fine, now it is not" report.
///
/// 240 px covers the measured menu items; the `Area` still grows when content needs more.
/// All five of our floating windows set `default_width`/`default_size` explicitly, combo
/// boxes use `Spacing::combo_width` and tooltips use `Spacing::tooltip_width`, so none of
/// them read this default.
pub const MENU_AREA_W: f32 = 240.0;

/// 选项栏高 40：设计图 `.optionbar{height:40px}`。原 34 是把浏览器缩放当成紧凑档
/// 量出来的偏小值——选项栏里 28px 的 chip 居中后上下各只剩 3px，读作一条挤住的带子。
///
/// Options bar 40 px, per the design mockup (`.optionbar{height:40px}`). The previous 34
/// came from measuring the mockup at browser zoom; a 28 px chip then leaves only 3 px of
/// air above and below and the bar reads as cramped.
pub const OPTIONS_H: f32 = 40.0;
pub const TAB_H: f32 = 28.0;
pub const STATUS_H: f32 = 24.0;
/// 工具轨宽 56：设计图 `.toolrail{width:48px}` 是只有 7 枚工具时的宽度。
///
/// 这一版轨上要放 15 项（工作流 / 预设 / 蒙版 / 历史 / 图库 / AI / 插件 / 6 枚工具 /
/// 外观），48px 里放 36 的按钮左右只剩 6px，图标会顶到轨边；56 给到左右各 10px。
/// 依据是 Photoshop 的单列工具栏：图标条比"刚好装下按钮"略宽，留出点击余量与分组缝。
///
/// Tool rail 56 px. The mockup's `.toolrail{width:48px}` was sized for seven tools; this
/// version carries fifteen (workflow / presets / masks / history / library / AI / plugins /
/// six tools / appearance), and a 36 px button inside 48 leaves only 6 px a side, so icons
/// crowd the edge. 56 gives 10 px each side, following Photoshop's single-column tool strip:
/// slightly wider than the buttons so clicks and group gaps have room.
pub const TOOL_W: f32 = 56.0;
/// 轨上按钮的边长：32 在三项组里偏小、在 15 项里又挤，36 是"够点、又不至于让轨变粗"的档。
///
/// Rail button edge: 32 reads small in a three-item group and cramped across fifteen
/// entries; 36 is the size that stays clickable without thickening the rail.
pub const RAIL_BTN: f32 = 36.0;
/// 空间不够时的最小按钮边长（= 上一版的大小），只压不改布局。
///
/// The smallest button edge when space runs out (the previous version's size); it shrinks
/// only, never rearranges.
pub const RAIL_MIN_BTN: f32 = 32.0;
/// 同组内两项之间的缝。
///
/// Gap between two entries of the same group.
pub const RAIL_GAP: f32 = 4.0;
/// 组与组之间（含分隔线）的间距。
///
/// Spacing between groups, drawn with a divider in the middle.
pub const RAIL_GROUP_GAP: f32 = 10.0;
/// 画布与侧栏之间的呼吸带宽度。
///
/// 面板与画布直接贴边时，推子的轨道与图像边只差一根描边，眼睛会把两者连成一片；
/// 留一条与画布底色相同的中性灰带，再在面板内缘压一根发丝线，层级就分开了。
/// 这个数只在 `layout::canvas_insets` 里加一次，别处不许再 `+ 10`。
///
/// Breathing room between the canvas and a side panel.
///
/// With the panel flush against the canvas, only one stroke separates a slider rail from
/// the image edge and the two read as a single surface. A neutral band in the canvas
/// surround colour plus a hairline on the panel's inner edge restores the hierarchy.
/// This value is added exactly once, in `layout::canvas_insets`; nothing else adds it.
pub const CANVAS_GAP: f32 = 10.0;
/// 标题栏里流程步骤条的高度：比菜单行矮 6px，不与窗口拖拽区抢垂直空间。
///
/// Height of the workflow step bar in the title bar: 6 px shorter than the menu row so
/// it does not steal vertical room from the window drag zone.
pub const STEP_H: f32 = 22.0;
/// 左侧工作流面板宽度：只放当前步的 3–5 个动作，再宽就开始吃画布。
///
/// Width of the left workflow panel: just enough for the current step's 3-5 actions;
/// wider starts eating the canvas.
pub const FLOW_W: f32 = 200.0;
/// 工作流面板的最小窗口宽度：低于这个值强制收起，保住画布的可用面积。
///
/// Minimum window width for the workflow panel; below it the panel is forced closed to
/// protect the canvas area.
pub const FLOW_MIN_W: f32 = 1180.0;

// ---- 浮窗 / 对话框尺寸 --------------------------------------------------
// 所有子窗口与 Modal 的尺寸只有这一处口径。以前每个 Window 各写各的 340 / 248 /
// 430 / 480 / 760，换到 1280×800 或被 egui 记住一次拉大的尺寸就顶出视口，
// 用户 2026-11-07 连着两轮截图报「子窗口怎么这么大」。
//
// One source for every floating window and modal size. Each Window used to carry its
// own 340 / 248 / 430 / 480 / 760, so a remembered size on a 1280x800 screen ran off
// the viewport -- exactly what the user's two screenshot rounds reported.
pub const WIN_MAX_W_FRAC: f32 = 0.92;
pub const WIN_MAX_H_FRAC: f32 = 0.88;
pub const WIN_MAX_W: f32 = 860.0;
pub const WIN_MAX_H: f32 = 700.0;
pub const WIN_MARKET_W: f32 = 760.0;
pub const WIN_MARKET_H: f32 = 520.0;
pub const WIN_MARKET_MIN_W: f32 = 560.0;
pub const WIN_MARKET_MIN_H: f32 = 360.0;
pub const WIN_LOOK_W: f32 = 340.0;
pub const WIN_LOOK_H: f32 = 560.0;
pub const WIN_LOOK_MIN_W: f32 = 316.0;
pub const WIN_PRESETS_W: f32 = 248.0;
pub const WIN_PRESETS_H: f32 = 480.0;
pub const WIN_ABOUT_W: f32 = 430.0;
pub const WIN_ABOUT_H: f32 = 480.0;
pub const WIN_UPDATE_W: f32 = 480.0;
pub const WIN_UPDATE_H: f32 = 420.0;
pub const WIN_UPDATE_MIN_W: f32 = 420.0;
pub const WIN_UPDATE_MIN_H: f32 = 300.0;
/// Modal 最高占视口的 62%：再高就把整张照片都盖住了，用户无从判断"这个确认
/// 会不会毁掉我刚调的色"。
///
/// A modal may cover at most 62% of the viewport: any taller hides the whole photo and
/// the user cannot judge whether confirming will ruin the grade they just made.
pub const MODAL_MAX_H_FRAC: f32 = 0.62;
pub const MODAL_W_NARROW: f32 = 430.0;
/// 三栏导出对话框的宽度：范围 / 格式 / 摘要三栏并排才读得完。
///
/// Width of the three-column export dialog; only this wide can the scope, format and
/// summary columns sit side by side.
pub const MODAL_W_EXPORT: f32 = 860.0;
pub const MODAL_W_WIDE: f32 = 520.0;

/// 可缩放浮窗的上界：按视口算，再夹一层绝对值。
///
/// Upper bound of a resizable window: viewport-derived, then capped by an absolute.
pub fn win_max(vp: Vec2) -> Vec2 {
    Vec2::new(
        (vp.x * WIN_MAX_W_FRAC).min(WIN_MAX_W),
        (vp.y * WIN_MAX_H_FRAC).min(WIN_MAX_H),
    )
}

/// 默认尺寸不能比上界大，也不能比下限小。
///
/// A default size must stay inside the bounds.
pub fn win_default(size: Vec2, vp: Vec2) -> Vec2 {
    let m = win_max(vp);
    Vec2::new(size.x.min(m.x), size.y.min(m.y))
}

/// 最小尺寸同样要跟着视口收，否则 1280×800 下「最小 560×360」本身就越界。
///
/// A minimum size has to shrink with the viewport too, or "min 560x360" itself
/// overflows on a 1280x800 screen.
pub fn win_min(size: Vec2, vp: Vec2) -> Vec2 {
    let m = win_max(vp);
    Vec2::new(size.x.min(m.x), size.y.min(m.y))
}

/// Modal 宽度也按视口收一档（左右各留 24px）。
///
/// Modal width is clamped to the viewport as well (24 px on each side).
pub fn modal_width(w: f32, vp_w: f32) -> f32 {
    w.min((vp_w - 48.0).max(200.0))
}

/// Modal 高度上限。
///
/// Modal height cap.
pub fn modal_max_h(vp_h: f32) -> f32 {
    (vp_h * MODAL_MAX_H_FRAC).max(200.0)
}

/// 右侧参数面板默认宽度 300px。
///
/// 原先 372 是按「标签 76 + 读数 52 + 轨道」堆出来的上限值，实测偏宽：**
/// Photoshop 的默认面板列约 250–260px**，我们比它宽出四成，画布被挤。300px 仍能
/// 放下 76px 标签列、52px 读数框与一条可用的轨道，是三档里最接近 PS 的舒适值。
///
/// Default width of the right parameter panel, 300 px.
///
/// The old 372 was the maximum the row layout needed (76 px label + 52 px readout +
/// rail), and measured too wide: Photoshop's default panel column is about 250-260 px, so
/// we ran ~40% wider and squeezed the canvas. At 300 px the label column, the readout box
/// and a usable rail still fit, and it is the closest comfortable value to PS.
pub const DOCK_W: f32 = 300.0;
/// 最外那条 28px 竖条：网格 / 胶片 / 曲线三个开关，界面稿里是 ⌗ ◐ ◑ 三枚。
///
/// The outermost 28 px vertical strip: grid / filmstrip / curve toggles, drawn as three
/// glyphs in the mockup.
pub const STRIP_W: f32 = 28.0;

/// 面板里的两种行高：推子行要容下标签、轨道与数值，键值行只排一对字。
/// 之前都是按最小可放下的高度定的，整列读起来像密排表；现在按 macOS 侧栏的
/// 行距留一口气，一屏少放两三行，但每一行都能一眼读清。
/// 行高密度档。紧凑给高分屏小尺寸预览多看几行，宽松给触屏/演示。
/// 只影响推子行（`row_h`）与键值行（`kv_h`）；标题栏/工具栏等外壳高度
/// 是结构尺寸，不随密度变——那是界面稿钉死的骨架。
///
/// Row-density step. Compact fits more rows on a high-DPI screen with a small preview,
/// roomy suits touch screens and demos. It affects only slider rows (`row_h`) and key/value
/// rows (`kv_h`); header and toolbar heights are structural and do not move with density —
/// the mockup pinned that skeleton.
///
/// The two row heights inside panels: a slider row has to fit label, track and value,
/// while a key/value row just arranges one pair of words. These used to be the smallest
/// height that would fit, which made a whole column read like a dense table; they now keep
/// macOS sidebar spacing — two or three rows fewer per screen, but every row is legible at
/// a glance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    #[default]
    Standard,
    Loose,
}

impl Density {
    pub fn cn(self) -> &'static str {
        match self {
            Self::Compact => "紧凑",
            Self::Standard => "标准",
            Self::Loose => "宽松",
        }
    }
    pub const ALL: [Density; 3] = [Self::Compact, Self::Standard, Self::Loose];
}

/// 调整面板推子行高：紧凑 28 / 标准 32 / 宽松 38。
///
/// 2026-11-06 按设计稿（`mockups/aevum-design-review-2026-11-06.html`）重定标：
/// 原 32/38/46 比设计稿高 20–46%，一屏少放 4 行参数，「不像设计稿」的主因就是它。
/// 三档保持等差（+4/+6），Compact 仍是「装得下 12px 标签 + 20px 数值框」的下限。
///
/// Slider row height in the adjustment panel: compact 28 / standard 32 / roomy 38.
/// Re-scaled on 2026-11-06 against the design mockup (`.slider-row{height:32px}`,
/// `.density-loose .slider-row{height:40px}`): the standard step is 32 and only the roomy
/// step goes to 40, exactly as the design does it.
pub fn row_h() -> f32 {
    match appearance().density {
        Density::Compact => 28.0,
        Density::Standard => 32.0,
        Density::Loose => 40.0,
    }
}

/// 键值行 / 剪切读数行高：24 / 32 / 40。
///
/// Key/value row and crop readout row height: 24 / 32 / 40.
///
/// 设计图 `.kv-row{height:32px}` 与推子行同高：一行里只排一对字却比推子行矮，
/// 竖向节奏会断，读起来像表格里的夹行。紧凑档 24 仍留得住一对 12px 字。
///
/// The design gives `.kv-row` the same 32 px as a slider row. A row that holds only one
/// key/value pair but is shorter than the slider row breaks the vertical rhythm and reads
/// as a squeezed table row. The compact step keeps 24 px, enough for one 12 px pair.
pub fn kv_h() -> f32 {
    match appearance().density {
        Density::Compact => 24.0,
        Density::Standard => 32.0,
        Density::Loose => 40.0,
    }
}

/// 紧凑键值行高：长信息列表（序列页体检结论等）用，固定 24px 不随密度档变。
///
/// 设计图 `.kv-row` 的 32px 是「少量信息行」的规格；一屏排十余条时会多占近 90px，
/// 把用户真正要看的结果挤出可视区。这里固定 24px：一对 12px 字垂直居中且不重叠。
///
/// Compact key/value row height for long information lists (the sequence page's QA
/// summary and the like). It stays at 24 px regardless of the density step. The design's
/// 32 px `.kv-row` is the spec for a few rows; a dozen of them costs nearly 90 px more
/// and pushes the result the user came to read out of view.
pub const KV_DENSE_H: f32 = 24.0;

/// 画布内部顶栏（cv-toolbar）：文档页签 + 尺寸读数 + 右端上下文，28px。
///
/// Canvas top bar (cv-toolbar): document tabs + size readout + right-hand context, 28
/// px.
pub const CV_TOOLBAR_H: f32 = 28.0;
/// 画布内容区四周留白：图像适配窗口时的内缩量，`clamp_pan` 也以它为可视边界。
///
/// Canvas content padding: the inset used when fitting the image to the window, and the
/// viewport boundary `clamp_pan` clamps against once the view is zoomed in.
pub const CANVAS_PAD: f32 = 0.0;
/// 胶片条头部行：标题 + 右端筛选说明，26px。
///
/// Filmstrip header row: title plus a filter note on the right, 26 px.
pub const TL_HD_H: f32 = 26.0;

/// 兼容旧命名。
///
/// Kept for the old name.
pub const TOP_H: f32 = MENU_H;
pub const RAIL_W: f32 = TOOL_W;
pub const PARAMS_W: f32 = DOCK_W;
pub const INSPECTOR_W: f32 = DOCK_W;

/// 字号四档缩放（对外行为不变）：基准档对应 11/13/15/17 字阶。
///
/// Four font-size zoom steps (public behaviour unchanged): the base step maps to the
/// 11/13/15/17 scale.
pub const FONT_ZOOMS: [f32; 4] = [1.0, 1.06, 1.14, 1.24];

/// macOS 字阶基准：正文 13、小字 11、大标题 15（17 顶格由 Heading+缩放档覆盖）。
///
/// macOS type scale: body 13, small 11, large heading 15 (the 17 top step is covered by
/// Heading plus a zoom step).
pub const SIZE_SMALL: f32 = 11.0;
pub const SIZE_BODY: f32 = 13.0;
/// 页面标题：比正文大一档，全应用只有页面标题用它。
///
/// Page title: one step above body text; only page titles use it.
pub const SIZE_TITLE: f32 = 15.0;

/// 标准控件高度：按钮、输入框、页签行都取这一档；小号取 `CH_SM`。
/// 收口 5 种高度（22/24/26/28/32）是「整体像拼装」的直接来源（审计 #32）。
///
/// Standard control height: buttons, fields and tab rows all use this; `CH_SM` for the
/// compact variant. Five different heights were a direct cause of the "assembled from
/// parts" look.
pub const CH: f32 = 26.0;
/// 小号控件高度（工具栏图标按钮、芯片）。
///
/// Compact control height (toolbar icon buttons, chips).
pub const CH_SM: f32 = 22.0;
pub const SIZE_HEADING: f32 = 15.0;
/// 面板里的推子标签与键值行：比 11 号小字大一档，11 号在 372px 宽的面板上
/// 读起来像脚注，而参数名是这一页最常读的字。
///
/// Slider labels and key/value rows inside panels: one step above the 11 px small size.
/// 11 reads like a footnote on a 372 px panel, and parameter names are the most-read words
/// on that screen.
pub const SIZE_LABEL: f32 = 12.0;
/// 顶部两行（菜单行 36 / 选项栏 46）专用字阶：行高加起来了，标签仍用 11 会
/// 显得整条栏是空的，所以顶部比正文大一号，面板里的推子标签不受影响。
///
/// Type scale for the top two rows (menu row 36 / option bar 46): the rows got taller, so
/// 11 px labels would leave the whole bar looking empty; the top is one step above body
/// size, and slider labels inside panels are unaffected.
pub const SIZE_CHROME: f32 = 14.0;
/// 选项栏里的次要标签与状态文字：比小字大一号，跟得上加高后的行高。
///
/// Secondary labels and status text in the option bar: one step above small, keeping up
/// with the taller rows.
pub const SIZE_CHROME_LABEL: f32 = 12.0;

// 字体字节只在水印那个 crate 里落一次（`aevum_core::fonts`）：Noto 一份 10 MB，
// 两个 crate 各 include_bytes! 一次，产物就要多背 10 MB。
const INTER: &[u8] = aevum_core::fonts::INTER;
// Noto Sans SC 可变字体的默认字重是 100（Thin），直接嵌入会让中文细得像虚线；
// 这里用 fontTools 实例化出的 500 静态字重。
const NOTO: &[u8] = aevum_core::fonts::NOTO_SC;
// 真粗体：两份 700 子集（`tools/font_subset.py` 切出来的），只覆盖界面源码里
// 出现过的字。缺字由常规体兜底——粗体字族的列表里常规体排在后面。
//
// Real bold: two 700 subsets cut by `tools/font_subset.py`, covering only characters
// that appear in the UI source. Missing glyphs fall back to the regular family,
// which sits behind the bold entries in the family list.
const INTER_BOLD: &[u8] = aevum_core::fonts::INTER_BOLD;
const NOTO_BOLD: &[u8] = aevum_core::fonts::NOTO_SC_BOLD;

/// 粗体字族在 `FontDefinitions` 里的名字。
///
/// The bold family's name inside `FontDefinitions`.
const BOLD_FAMILY: &str = "aevum-bold";

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// 粗体字族名。egui 的 `FontId` 只有「字族 + 字号」，没有字重——真粗体的唯一
/// 做法是另注册一个字族，这里就是它的名字。
///
/// Bold family name. egui's `FontId` carries a family and a size but no weight, so
/// the only way to get real bold is to register a second family -- this is its name.
pub fn bold_family() -> FontFamily {
    FontFamily::Name(BOLD_FAMILY.into())
}

/// 粗体：分区标题、次级小标题、对话框标题用。
///
/// Bold: section headings, sub-headings and dialog titles.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, bold_family())
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn caps(text: &str) -> RichText {
    RichText::new(text).size(10.0).color(dim()).monospace()
}

pub fn apply(ctx: &Context, zoom: f32) {
    set_fonts(ctx);
    apply_style(ctx, zoom);
}

/// 只装字体。字号档位变了才需要重跑（`ctx.set_fonts` 会丢字形缓存）。
///
/// Install fonts only. Needs a rerun only when the size step changes (`ctx.set_fonts`
/// drops the glyph cache).
pub fn set_fonts(ctx: &Context) {
    ctx.set_fonts(font_definitions());
}

/// 字族装配。拆出来是为了能不带 `Context` 直接断言——`set_fonts` 只是把它交给
/// egui，装配本身没有副作用。
///
/// Family assembly, split out so it can be asserted without a `Context`: `set_fonts`
/// only hands the result to egui, and the assembly itself has no side effects.
/// 系统字体：设计稿在 Windows Chrome 里渲染，字形就是 Segoe UI + 微软雅黑；
/// 嵌入的 Inter / Noto 只在系统字体缺失时兜底。加载失败（非 Windows、精简系统）
/// 静默跳过——嵌入字体永远在链尾。
///
/// System fonts: the design renders in Windows Chrome, so its glyphs ARE Segoe UI +
/// Microsoft YaHei; the embedded Inter / Noto only back up systems without them. Load
/// failures (non-Windows, stripped systems) are silent -- embedded fonts stay at the
/// end of the chain regardless.
fn system_font_candidates() -> &'static [(&'static str, &'static str)] {
    &[
        ("Segoe UI", r"C://Windows//Fonts//segoeui.ttf"),
        ("Microsoft YaHei UI", r"C://Windows//Fonts//msyh.ttc"),
    ]
}

fn system_bold_candidates() -> &'static [(&'static str, &'static str)] {
    &[
        ("Segoe UI Bold", r"C://Windows//Fonts//segoeuib.ttf"),
        ("Microsoft YaHei UI Bold", r"C://Windows//Fonts//msyhbd.ttc"),
    ]
}

/// 系统字体合计 38.5 MB（`msyh.ttc` 19.7 MB + `msyhbd.ttc` 16.9 MB + 两份
/// Segoe）。`theme::apply` 在应用启动、字号档位切换、以及每个测试新建的 egui
/// Context 里都会装配一次字体：逐次重读磁盘并各复制一份 38.5 MB 到新的 Arc，
/// 是纯粹的浪费，也是「反复跑测试/切字号时明显停顿」的来源。
/// `FontDefinitions` 只读且 `FontData` 内部就是 Arc，装配结果可以安全共享。
///
/// System fonts total 38.5 MB (YaHei TTC 19.7 MB + bold 16.9 MB + two Segoe faces).
/// `theme::apply` assembles fonts at startup, on every font-size change, and in every
/// test's fresh egui Context: re-reading the disk and copying 38.5 MB into a new Arc
/// each time is pure waste, and the source of the pause when running tests or switching
/// font size. `FontDefinitions` is immutable and `FontData` already holds an Arc, so the
/// assembled result is safe to share.
static FONT_DEFS: std::sync::OnceLock<FontDefinitions> = std::sync::OnceLock::new();

/// 取一份字体装配：首次装配后只做便宜的 clone（Arc 只加引用计数）。
///
/// Get the font assembly: after the first build this is only a cheap clone (Arc bumps
/// a refcount, no bytes are copied).
pub fn font_definitions() -> FontDefinitions {
    FONT_DEFS.get_or_init(build_font_definitions).clone()
}

fn build_font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    // 系统字体优先：设计稿的「质感」一半在字形与 hinting 上，嵌入的
    // Inter/Noto 与 Windows Chrome 渲染的不是一张脸。
    //
    // System fonts first: half of the design's texture lives in the glyph shapes and
    // hinting; the embedded Inter/Noto are not the face Windows Chrome renders.
    let mut lead: Vec<String> = Vec::new();
    let mut lead_bold: Vec<String> = Vec::new();
    for (name, path) in system_font_candidates() {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_string(), FontData::from_owned(bytes).into());
            lead.push(name.to_string());
        }
    }
    for (name, path) in system_bold_candidates() {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_string(), FontData::from_owned(bytes).into());
            lead_bold.push(name.to_string());
        }
    }
    fonts
        .font_data
        .insert("inter".to_owned(), FontData::from_static(INTER).into());
    fonts
        .font_data
        .insert("noto".to_owned(), FontData::from_static(NOTO).into());
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        for (i, name) in lead.iter().enumerate() {
            list.insert(i, name.clone());
        }
        let off = lead.len();
        list.insert(off, "inter".to_owned());
        list.insert(off + 1, "noto".to_owned());
    }
    // 等宽族与主字体**同形**：原先 egui 内置 mono 打头，状态栏/走带/读数的
    // 数字字形与主界面明显两套（用户截图圈名「下面的字体与全局字体不同」）。
    // 改为 Inter 打头、noto 兜底——数字与正文同一张脸；数字列对齐靠同字体
    // 同字号的稳定宽度（Inter 数字近 tabular），牺牲可忽略。
    //
    // The monospace family now shares the main typeface: egui's bundled mono used to
    // lead, so digits in the status bar / transport / readouts rendered in a visibly
    // different face (user screenshot). Inter leads and noto backs up -- digits share
    // the body face; column alignment survives on Inter's near-tabular digits.
    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        list.insert(0, "inter".to_owned());
        list.insert(1, "noto".to_owned());
    }
    // 粗体字族：两份 700 子集打头，**后面整条接上常规字族的列表**（含 egui 自带的
    // 兜底字体）。这样缺字只有一种结果：那个字画成常规体。整条兜底链复制而不是
    // 手写两项，是因为 egui 自带的默认字体也在末尾兜着箭头、圈号这类符号——
    // 手抄会漏，漏了就是豆腐块。
    //
    // Bold family: the two 700 subsets first, then **the entire regular family list**
    // (including egui's bundled fallback fonts). A missing glyph then has exactly one
    // outcome: it renders regular. The chain is copied rather than hand-listed because
    // egui's own default fonts sit at the end covering arrows and circled digits --
    // hand-copying misses them, and a miss means tofu.
    fonts.font_data.insert(
        "inter-bold".to_owned(),
        FontData::from_static(INTER_BOLD).into(),
    );
    fonts.font_data.insert(
        "noto-bold".to_owned(),
        FontData::from_static(NOTO_BOLD).into(),
    );
    let mut bold_chain = lead_bold.clone();
    bold_chain.push("inter-bold".to_owned());
    bold_chain.push("noto-bold".to_owned());
    bold_chain.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts.families.insert(bold_family(), bold_chain);
    fonts
}

/// 只重刷 `Visuals`，不动字体。
///
/// 外观面板拖滑块时每帧都会走到这里：换色只是重算几十个 Color32 与一次
/// `set_style`，比重新装字体便宜两个数量级。
///
/// Refresh `Visuals` only, without touching fonts.
///
/// Dragging a slider in the appearance panel lands here every frame: recolouring means
/// recomputing a few dozen `Color32`s plus one `set_style`, two orders of magnitude cheaper
/// than reinstalling fonts.
pub fn apply_style(ctx: &Context, zoom: f32) {
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.text_styles = [
        (TextStyle::Heading, font(SIZE_HEADING)),
        (TextStyle::Body, font(SIZE_BODY)),
        (TextStyle::Button, font(SIZE_BODY)),
        (TextStyle::Small, font(SIZE_SMALL)),
        (TextStyle::Monospace, mono(11.0)),
    ]
    .into();

    style.spacing.item_spacing = Vec2::new(6.0, 4.0);
    // 按钮内衬收一格：7×4 的 padding 让按钮看起来像充了气的胶囊，收成 6×3。
    style.spacing.button_padding = Vec2::new(6.0, 3.0);
    style.spacing.interact_size = Vec2::new(22.0, 22.0);
    style.spacing.slider_width = 110.0;
    style.spacing.slider_rail_height = 4.0;
    style.spacing.scroll.bar_width = 8.0;
    // 浮动滚动条（悬浮于内容之上、不占布局宽度）：固定 false 时，展开一个长组
    // → 竖滚动条出现 → 面板内容整体横移 8px——用户实测「点击展开某个调色选项
    // 界面会动」。浮动后滚动条 hover 才显形，布局宽度恒定，展开零位移。
    //
    // Floating scrollbars (overlay, no layout width): with fixed bars, expanding a
    // long group made the vertical bar appear and shifted the whole panel 8 px --
    // the user-reported "expanding a section moves the UI". Floating bars appear on
    // hover, layout width stays constant, zero shift.
    style.spacing.scroll.floating = true;
    style.spacing.indent = 12.0;
    // 弹层首帧宽度（见 `MENU_AREA_W` 的注释）：不收这一档，重启后第一次点菜单会得到
    // 一条 egui 默认的 600px 空板子。写在这里是因为 `Area` 读的是 `ctx.global_style()`，
    // 只有全局样式生效；内容回调里改 `ui.spacing_mut()` 管不到弹层自身的宽度。
    //
    // Popup first-frame width (see `MENU_AREA_W`): without this, the first menu opened after
    // a restart becomes egui's 600 px default slab. It belongs here because `Area` reads
    // `ctx.global_style()`; changing `ui.spacing_mut()` inside the content callback cannot
    // affect the popup's own width.
    style.spacing.default_area_size = Vec2::new(MENU_AREA_W, 400.0);
    style.interaction.selectable_labels = false;

    // 浅色不是"把深色反过来"，而是 egui 自己也要换一套基线：
    // Visuals::light 里的默认值（阴影、分隔、选中）和深色是两份不同的口径。
    let light = is_light();
    let mut v = if light {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    v.panel_fill = panel();
    // 浮层最亮一档，但控制在 DIM 文字仍 ≥ 4.5:1 的亮度内。
    v.window_fill = bg();
    v.extreme_bg_color = surround();
    v.faint_bg_color = header();
    v.code_bg_color = canvas();
    // 浮窗边界要能从面板上读出来：border 的 20 透明度在深底上几乎隐形，
    // 浮窗因此「贴」在内容上而不是「浮」在内容上——成熟软件的浮层都有
    // 一圈读得到的边。描边抬到 border_strong（46），阴影自己给一套：
    // egui 默认深色阴影在深底上不可见，等于没有。
    //
    // Floating-window edges must read against the panel: border's alpha 20 is
    // nearly invisible on dark surfaces, so windows looked *stuck on* rather than
    // *floating over*. Stroke steps up to border_strong (46) and the shadow is
    // our own -- egui's default dark shadow disappears on a dark background.
    v.window_stroke = Stroke::new(1.0, border_strong());
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, if light { 5 } else { 8 }],
        blur: if light { 22 } else { 30 },
        spread: 2,
        color: Color32::from_black_alpha(if light { 64 } else { 165 }),
    };
    v.window_corner_radius = radius_window().into();
    v.menu_corner_radius = radius_window().into();
    v.hyperlink_color = accent();
    v.warn_fg_color = warn();
    v.error_fg_color = danger();
    v.weak_text_alpha = 0.8;
    // 选中底跟着主色走：换主色时这一块也要变，不能钉死在系统蓝上。
    // 强度与自绘 selected() 对齐（0.15 mix）——原生选中（下拉行、ComboBox、文本
    // 选择）曾是 accent 25% 的整行亮蓝，在暗色界面里刺眼，也和面板里的选中底
    // 对不上；统一到同一档之后，「选中」在全应用只有一个长相。
    //
    // The selection fill follows the accent, so changing the accent changes it too.
    // Its strength now matches the self-painted selected() (0.15 mix): native
    // selection (dropdown rows, combo boxes, text selection) used to be a full-row
    // bright accent at 25%, harsh on a dark UI and inconsistent with the panel's
    // selected fill; aligning them gives "selected" one single look app-wide.
    v.selection.bg_fill = selected();
    v.selection.stroke = Stroke::new(1.0, accent());
    v.striped = false;
    v.slider_trailing_fill = true;
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, if light { 4 } else { 6 }],
        blur: if light { 18 } else { 24 },
        spread: 0,
        // 浅色底的投影要收着，140 的黑在亮底上会糊成一圈脏边。
        color: Color32::from_black_alpha(if light { 56 } else { 140 }),
    };

    v.widgets.noninteractive.bg_fill = panel();
    v.widgets.noninteractive.weak_bg_fill = panel();
    v.widgets.noninteractive.bg_stroke = Stroke::NONE;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text());
    v.widgets.noninteractive.corner_radius = 4.into();

    // 全局手型光标：egui 默认 `interact_cursor = None`，导致所有原生按钮/
    // 复选框/菜单项/拖拽数值悬停时都是箭头——「能点的地方不告诉你它能点」是
    // 廉价感最普遍的一层（2026-11-09 交互审计：全仓仅 5 处手动设 PointingHand）。
    //
    // Global pointing-hand cursor: egui defaults `interact_cursor` to None, so every
    // native button / checkbox / menu item shows an arrow on hover -- "clickable things
    // that do not announce themselves" is the most widespread cheapness signal.
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);

    v.widgets.inactive.bg_fill = elev();
    v.widgets.inactive.weak_bg_fill = elev();
    // 复选框的方框就是这颗「未激活控件」的底：ELEV 底配 NONE 描边，在深色
    // 面板上只剩一块比背景亮两度的方块，勾没勾要看里面有没有细线。
    // 描边给但收着（divider 档）：普通按钮不再有一圈亮边——圈一圈的边框
    // 也是塑料感的一部分；方框照样立得住。
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, divider());
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, text());
    v.widgets.inactive.corner_radius = radius_control().into();

    // 按钮三态的「手感」：悬停只是轻轻抬起（+一档），按下是往里沉（press），
    // 不再把整块点亮成强调色。egui 默认 active=accent 描边+accent 底，按哪儿
    // 哪儿蓝，一闪一闪的就是用户说的「塑料感」；专业软件的按钮按下是变暗。
    v.widgets.hovered.bg_fill = elev_hover();
    v.widgets.hovered.weak_bg_fill = elev_hover();
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, on_elev());
    v.widgets.hovered.corner_radius = radius_control().into();

    v.widgets.active.bg_fill = press();
    v.widgets.active.weak_bg_fill = press();
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.active.fg_stroke = Stroke::new(1.0, text());
    v.widgets.active.corner_radius = radius_control().into();

    // 展开态（折叠头、组合框、置顶窗口的标题栏）只轻轻抬一档深灰。
    // 之前这里用 selected（panel 混 22% 强调蓝），egui 0.37 的置顶窗口
    // 会拿它画整条标题栏——结果就是用户截图里那条刺眼的蓝杠。
    v.widgets.open.bg_fill = header();
    v.widgets.open.weak_bg_fill = header();
    v.widgets.open.bg_stroke = Stroke::NONE;
    v.widgets.open.fg_stroke = Stroke::new(1.0, text());
    v.widgets.open.corner_radius = radius_control().into();

    style.visuals = v;
    let theme = if light {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    };
    // 两套 style 都写：切主题时 egui 会按它自己的偏好挑一份，只写一套会出现
    // "换了浅色但弹窗还是深色"的半截状态。
    ctx.set_style_of(theme, style.clone());
    ctx.set_style_of(
        if light {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        },
        style,
    );
    ctx.set_theme(if light {
        egui::ThemePreference::Light
    } else {
        egui::ThemePreference::Dark
    });
    ctx.set_zoom_factor(zoom);
}

/// 手动画带字距的小标题（egui 的 Label 不支持 tracking）。
///
/// Draw a small, letter-spaced heading by hand (egui's `Label` has no tracking).
pub fn tracked_text(
    painter: &egui::Painter,
    pos: Pos2,
    text: &str,
    font_id: FontId,
    color: Color32,
    tracking: f32,
) {
    let mut x = pos.x;
    for ch in text.chars() {
        let galley = painter.layout_no_wrap(ch.to_string(), font_id.clone(), color);
        painter.galley(Pos2::new(x, pos.y), galley.clone(), color);
        x += galley.size().x + tracking;
    }
}

/// 居中裁切的单行文本，返回实际画出的文本。
///
/// Single-line text centred on its own crop; returns the text actually drawn.
pub fn ellipsis(text: &str, max_chars: usize) -> String {
    let n = text.chars().count();
    if n <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 按像素宽度截断单行文本。
///
/// 按字符数砍对中文不成立：一个汉字 12px、一个字母 6px，固定宽度的栏位里
/// 字符数没超而像素早就压到邻居控件上了。先量整串，超了才按平均字宽推起点、
/// 逐次收紧，最多三次排版。
///
/// Truncate a single line to a pixel width.
///
/// Cutting by character count does not hold for Chinese: one Han character is 12 px and one
/// letter 6 px, so in a fixed-width column the character count may fit while the pixels
/// already push into the neighbouring control. Measure the whole string first, and only on
/// overflow estimate a start from the average advance, tightening at most three times.
pub fn fit_text(
    painter: &egui::Painter,
    text: &str,
    font_id: FontId,
    color: Color32,
    max_w: f32,
) -> String {
    let width = |s: &str| {
        painter
            .layout_no_wrap(s.to_owned(), font_id.clone(), color)
            .size()
            .x
    };
    let full = width(text);
    if full <= max_w {
        return text.to_owned();
    }
    let n = text.chars().count();
    let mut keep = (max_w / (full / n.max(1) as f32)).floor() as usize;
    loop {
        let cut = ellipsis(text, keep.min(n.saturating_sub(1)).max(1));
        if width(&cut) <= max_w || keep <= 1 {
            return cut;
        }
        keep -= 1;
    }
}

pub fn paint_chip(
    painter: &egui::Painter,
    ui: &egui::Ui,
    rect: egui::Rect,
    label: &str,
    active: bool,
    accent: Color32,
) -> egui::Response {
    // ID 不能只由标签文本决定：`左 / 右`、`标准`、`跟随系统` 这类文案在同一屏里
    // 出现在多行（工具轨/浮动面板/胶片条都有「左·右」，字号与密度都有「标准」），
    // 同标签同作用域会撞成同一个 ID，egui 当场把「First/Second use of widget ID」
    // 画在控件上（用户 2026-11-07 截图实证）。
    //
    // 位置也进盐：自绘控件的矩形在本帧是唯一的，且同一布局下逐帧稳定，交互状态
    // 不会丢。
    //
    // The id must not depend on the label alone: `左 / 右`, `标准` and `跟随系统` all
    // appear on several rows of one screen (tool rail / floating panels / filmstrip each
    // have 左·右, font size and density each have 标准). The same label in the same scope
    // collapsed into one id, and egui painted "First/Second use of widget ID" right on the
    // control (user screenshot, 2026-11-07).
    //
    // The rect is part of the salt: a self-painted control's rect is unique within a frame
    // and stable across frames for the same layout, so interaction state survives.
    let resp = ui.interact(
        rect,
        ui.scope_id().with((
            "chip",
            label,
            rect.left().round() as i32,
            rect.top().round() as i32,
        )),
        egui::Sense::click(),
    );
    // macOS 小胶囊：靠底色差分层，只有选中态保留强调色描边。
    // 按下时底色降到 press 档并压一道顶部内阴影——「按进去」的实体感是 PS 手感
    // 的来源（用户反馈 3），自绘 chip 没有 egui 原生按钮的 active 底色，得自己给。
    //
    // macOS-style small chip: layers by fill difference, only the active state keeps
    // the accent stroke. While pressed the fill drops to the press step and a top
    // inner shadow is drawn -- the physical "pressed in" feel is what makes PS buttons
    // feel right (user feedback 3); self-painted chips get none of egui's active fill.
    let pressed = resp.is_pointer_button_down_on();
    // 选中态的字色跟主题走：深色下白字压在低饱和蓝底上；浅色下 selected() 是淡蓝，
    // 白字几乎看不见（用户截图：浅色设置窗里「中文 / 浅色 / 石墨」一片发白）。
    // on_selected() 正是为「强调色低透明叠底」准备的那一档。
    //
    // The active label colour follows the theme: white on the low-saturation blue in dark
    // themes; in light themes selected() is pale blue and white text all but disappears
    // (the user's screenshot: "中文 / 浅色 / 石墨" washed out in the light settings
    // window). on_selected() is exactly the step meant for an accent-tinted fill.
    let (fill, stroke, fg, cr) = if pressed {
        (
            press(),
            if active {
                Stroke::new(1.0, accent)
            } else {
                Stroke::NONE
            },
            if active { on_selected() } else { text() },
            radius_control(),
        )
    } else if active {
        (
            selected(),
            Stroke::new(1.0, accent),
            on_selected(),
            radius_control(),
        )
    } else if resp.hovered() {
        (elev_hover(), Stroke::NONE, text(), radius_control())
    } else {
        // 默认态也给一圈 1px border（设计稿 chip 有描边）：无框的色块在深色面板上
        // 读作「一片底色」，不像可按的控件。
        (
            elev(),
            Stroke::new(1.0, border()),
            muted(),
            radius_control(),
        )
    };
    painter.rect_filled(rect, cr, fill);
    if pressed {
        // 顶部内阴影：一条 1px 暗线读作「被按下去」的位移（对齐 CSS
        // `inset 0 1px 2px rgba(0,0,0,.45)` 的手感，不引入真实阴影层）。
        //
        // Top inner shadow: a single dark hairline reads as the button sinking in
        // (the feel of CSS `inset 0 1px 2px rgba(0,0,0,.45)` without a real shadow).
        painter.line_segment(
            [
                egui::Pos2::new(rect.left() + 2.0, rect.top() + 1.0),
                egui::Pos2::new(rect.right() - 2.0, rect.top() + 1.0),
            ],
            Stroke::new(1.0, Color32::from_black_alpha(110)),
        );
    }
    if stroke.width > 0.0 {
        painter.rect_stroke(rect, cr, stroke, egui::StrokeKind::Inside);
    }
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font(11.0), fg);
    resp
}

/// 1px 发丝分隔线：颜色由调用方给。DIVIDER 这一档已提亮一档（约 15% 白），
/// 于是走 divider 的分组分隔有一道看得见的线；线宽仍是 1px 发丝，不描边。
///
/// 1 px hairline separator: the caller supplies the colour. The `DIVIDER` step is lifted
/// one step (about 15% white), so group separators that use divider get a visible line;
/// the stroke stays a 1 px hairline, never an outline.
pub fn rule(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    painter.line_segment(
        [
            Pos2::new(x0, y.round() + 0.5),
            Pos2::new(x1, y.round() + 0.5),
        ],
        Stroke::new(1.0, color),
    );
}

/// 浮窗与侧栏分节的「软分割线」：左右各缩进 12px、沿用提亮后的 divider 且不再二次压暗。
///
/// 通栏 1px 线读作「把两块切开」，缩进的线读作「这是两节」——分割样式
/// 重设计的落点就在这个语义差上。两侧留白让线不顶到边框，视觉上短一截，
/// 恰好和分节标题的左缘缩进（12px）对齐。颜色由 divider 提亮一档而来，
/// 分节之间因此有一道看得见的分隔，不再是淡到读不出的发丝线。
///
/// The "soft divider" between floating windows and right-column sections: inset 12 px on
/// each side, using the lifted divider with no second dimming pass.
///
/// A full-width 1 px line reads as "cut these two apart", while an inset line reads as
/// "these are two sections" — that semantic difference is exactly what the divider redesign
/// hangs on. The inset keeps the line off the frames, making it visually shorter and lining
/// it up with the 12 px left inset of section headings. The colour comes from divider
/// lifted one step, so adjacent sections get a visible separator instead of a hairline too
/// faint to read.
pub fn soft_rule(painter: &egui::Painter, x0: f32, x1: f32, y: f32) {
    const INSET: f32 = 12.0;
    rule(painter, x0 + INSET, x1 - INSET, y, divider());
}

/// Lightroom 的数值写法：有符号量带正号，零显示占位。
///
/// Lightroom's number style: signed quantities carry a plus sign, and zero shows a
/// placeholder.
pub fn number(value: f64, decimals: usize, signed: bool) -> String {
    let zero = 0.5 * 10f64.powi(-(decimals as i32));
    let v = if value.abs() < zero { 0.0 } else { value };
    if signed && v > 0.0 {
        format!("{:+.*}", decimals, v)
    } else {
        format!("{:.*}", decimals, v)
    }
}

pub fn dashed_rect(painter: &egui::Painter, rect: egui::Rect, color: Color32, dash: f32) {
    let corners = [
        (rect.left_top(), rect.right_top()),
        (rect.right_top(), rect.right_bottom()),
        (rect.right_bottom(), rect.left_bottom()),
        (rect.left_bottom(), rect.left_top()),
    ];
    for (a, b) in corners {
        let len = (b - a).length();
        let dir = (b - a) / len.max(1e-4);
        let mut t = 0.0;
        while t < len {
            let t1 = (t + dash).min(len);
            painter.line_segment([a + dir * t, a + dir * t1], Stroke::new(1.0, color));
            t += dash * 2.0;
        }
    }
}

#[cfg(test)]
mod token_tests {
    use super::*;

    /// 三张表各算一遍：颜色是派生出来的，就得逐主题验，不能只验深色。
    fn modes() -> Vec<Palette> {
        ThemeMode::ALL
            .iter()
            .map(|m| {
                Palette::of(&Appearance {
                    theme: *m,
                    ..Default::default()
                })
            })
            .collect()
    }

    fn surfaces(p: &Palette) -> [Color32; 7] {
        [
            p.surround, p.canvas, p.header, p.panel, p.dock, p.chrome, p.bg,
        ]
    }

    /// 弹层首帧宽度必须落在菜单档，不能是 egui 默认的 600。
    ///
    /// 这条钉的是 `apply_style` 有没有把 `default_area_size` 写进**全局**样式：
    /// `Area` 读 `ctx.global_style()`，所以只在内容回调里改 `ui.spacing_mut()` 是无效的
    /// （这正是前三轮菜单样式改了却「没变化」的同类错误）。删掉那行赋值，egui 的
    /// 600×400 默认值立刻回来，本测试红。
    ///
    /// 诚实的边界：弹层在测试环境不渲染（`Popup::open(true)` 在无头帧里只产生空 shape），
    /// 所以这条只证明「全局样式被正确设置」，**端到端仍需用户重启后首帧点开菜单确认**。
    ///
    /// The popup's first-frame width must land in the menu range, not egui's 600 default.
    ///
    /// This pins whether `apply_style` writes `default_area_size` into the **global** style:
    /// `Area` reads `ctx.global_style()`, so touching `ui.spacing_mut()` inside the content
    /// callback does nothing -- the same class of mistake that made three rounds of menu
    /// styling changes invisible. Remove that assignment and egui's 600x400 default returns,
    /// turning this test red.
    ///
    /// Honest limit: popups do not render in the test harness (`Popup::open(true)` yields
    /// only an empty shape in a headless frame), so this proves the global style is set
    /// correctly; end-to-end still needs the user to restart and open the menu once.
    #[test]
    fn popup_first_frame_width_is_menu_sized_not_egui_600() {
        let ctx = Context::default();
        apply_style(&ctx, 1.0);

        // 两套主题都要带这一档：apply 写两份，漏一份会在切主题后复发。
        // Both theme styles must carry it: apply writes both, and a missed one comes back
        // after a theme switch.
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let w = ctx.style_of(theme).spacing.default_area_size.x;
            assert!(
                (w - MENU_AREA_W).abs() < f32::EPSILON,
                "{theme:?} 的弹层默认宽是 {w}，不是菜单档 {MENU_AREA_W}"
            );
            assert!(
                w < 600.0,
                "{theme:?} 的弹层默认宽退回了 egui 的 600，重启后首个菜单会铺成空板子"
            );
        }

        // 全局样式（Area 真正读的那份）也得是这个值。
        // The global style -- the one `Area` actually reads -- must carry it too.
        let global = ctx.global_style().spacing.default_area_size.x;
        assert!(
            (global - MENU_AREA_W).abs() < f32::EPSILON,
            "Area 读的全局样式宽是 {global}，不是 {MENU_AREA_W}"
        );
    }

    /// 同标签、同作用域的两枚 chip 必须拿到不同 ID。
    ///
    /// 这是个真实发生过的缺陷：`paint_chip` 的 ID 原本只由标签文本决定，而
    /// `左 / 右`、`标准`、`跟随系统` 这些文案在同一屏里会出现多行（工具轨 / 浮动面板 /
    /// 胶片条都有「左·右」；字号与密度都有「标准」）。撞 ID 后 egui 直接把
    /// 「First use / Second use of widget ID」画在控件上（用户 2026-11-07 截图）。
    ///
    /// 改法是把控件矩形也纳入盐；这条判据保证「按标签算 ID」不会再回来。
    ///
    /// Two chips with the same label in the same scope must get different ids.
    ///
    /// This really happened: `paint_chip` derived its id from the label alone, while
    /// `左 / 右`, `标准` and `跟随系统` appear on several rows of one screen (tool rail /
    /// floating panels / filmstrip each have 左·右; font size and density each have 标准).
    /// Colliding ids made egui paint "First use / Second use of widget ID" onto the
    /// controls (user screenshot, 2026-11-07).
    ///
    /// The fix folds the control rect into the salt; this test keeps label-only ids from
    /// coming back.
    #[test]
    fn two_chips_with_the_same_label_get_different_ids() {
        let ctx = Context::default();
        apply_style(&ctx, 1.0);

        let mut ids = [egui::Id::NULL; 2];
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1000.0),
            )),
            max_texture_side: Some(8192),
            ..Default::default()
        };
        let mut out = ctx.run_pass(input, |ctx| {
            ctx.root_ui(|ui| {
                for (i, slot) in ids.iter_mut().enumerate() {
                    let rect = egui::Rect::from_min_size(
                        egui::Pos2::new(0.0, i as f32 * 32.0),
                        egui::vec2(64.0, 26.0),
                    );
                    let resp = paint_chip(ui.painter(), ui, rect, "左", false, accent());
                    *slot = resp.id;
                }
            });
        });
        out.textures_delta.clear();

        assert_ne!(
            ids[0], ids[1],
            "同标签 chip 撞了同一个 ID —— egui 会在控件上画 First/Second use of widget ID"
        );
    }

    /// `Area` 不设 `default_size` 时的实际布局宽度必须跟着 `MENU_AREA_W` 走。
    ///
    /// 上一条只证明「样式里存了对的值」；这条跑一帧真实布局，量 `Area` 自己算出来的宽度，
    /// 证明它真的采纳了这个默认值——菜单弹层用的就是「`Area` 不设尺寸 +
    /// `top_down_justified` 布局」这同一个组合，只是它换了个 `Popup` 外壳。
    ///
    /// The width an `Area` actually lays out to, when it sets no `default_size`, must follow
    /// `MENU_AREA_W`. The previous test only proves the style holds the right value; this one
    /// runs a real frame and measures the width the `Area` itself computes, proving the
    /// default is honoured. A menu popup is exactly this combination -- an `Area` with no
    /// size plus a `top_down_justified` layout -- just wrapped in a `Popup`.
    #[test]
    fn an_area_without_default_size_lays_out_at_the_menu_width() {
        let ctx = Context::default();
        apply_style(&ctx, 1.0);

        let mut laid_out = 0.0f32;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1000.0),
            )),
            max_texture_side: Some(8192),
            ..Default::default()
        };
        let mut out = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::unique("area-width-probe"))
                .layout(egui::Layout::top_down_justified(egui::Align::Min))
                .show(ctx, |ui| {
                    // 探针只用 ASCII：这条判据量的是宽度上限，与文案语言无关，
                    // 而中文裸字面量会被 `ui_call_sites_have_no_bare_chinese` 抓红
                    // （英文模式下那两行会直接上屏）。
                    //
                    // The probe stays ASCII: this test measures an upper bound on width and
                    // does not care about language, while bare Chinese literals are flagged
                    // by `ui_call_sites_have_no_bare_chinese` (they would paint verbatim in
                    // English mode).
                    ui.label("File");
                    ui.label("Export frames...");
                    laid_out = ui.min_rect().width();
                });
        });
        // 无头帧要把纹理增量消费掉，否则 epaint 在 Drop 时 panic（与本判据无关）。
        //
        // A headless frame must consume its texture deltas, or epaint panics on Drop
        // (unrelated to what this test asserts).
        out.textures_delta.clear();

        assert!(
            laid_out <= MENU_AREA_W + 1.0,
            "无尺寸 Area 排出了 {laid_out}px，超过菜单档 {MENU_AREA_W}（600 默认值回来了？）"
        );
    }

    #[test]
    fn body_text_keeps_7_to_1_on_every_material() {
        for p in modes() {
            for s in surfaces(&p) {
                assert!(
                    contrast(p.text, s) >= 7.0,
                    "{:?} 上正文只有 {:.2}:1",
                    s,
                    contrast(p.text, s)
                );
            }
        }
    }

    #[test]
    fn secondary_labels_keep_wcag_aa_on_every_material() {
        for p in modes() {
            for s in surfaces(&p) {
                assert!(
                    contrast(p.muted, s) >= 4.5,
                    "MUTED {:.2}",
                    contrast(p.muted, s)
                );
                assert!(contrast(p.dim, s) >= 4.5, "DIM {:.2}", contrast(p.dim, s));
            }
        }
    }

    #[test]
    fn control_fills_hold_their_labels() {
        for p in modes() {
            assert!(
                contrast(p.text, p.elev) >= 4.5,
                "TEXT/ELEV {:.2}",
                contrast(p.text, p.elev)
            );
            assert!(
                contrast(p.muted, p.elev) >= 4.5,
                "MUTED/ELEV {:.2}",
                contrast(p.muted, p.elev)
            );
            // 选中底与按下底都是强调色的低透明叠底，字要用 on_selected（浅色下是深字）。
            assert!(contrast(p.on_selected, p.selected) >= 4.5);
            assert!(contrast(p.on_selected, p.accent_dim) >= 4.5);
            assert!(contrast(p.text, p.field) >= 7.0);
            // 实心强调色上的字按大字号口径 ≥ 3:1（与 macOS 主按钮一致）。
            assert!(contrast(p.on_accent, p.accent) >= 3.0);
        }
    }

    #[test]
    fn material_ramp_is_monotonic() {
        for p in modes() {
            let l = luminance;
            let (panel, dock, chrome, bg) = (l(p.panel), l(p.dock), l(p.chrome), l(p.bg));
            // 深色往亮里走、浅色往暗里走，两条都必须是单调的层级。
            // 三张表的层级方向一致：画布外围最暗 → 画布 → 面板 → 侧栏 → 栏 → 浮层。
            assert!(l(p.surround) < l(p.canvas));
            assert!(l(p.canvas) < panel);
            assert!(panel < dock);
            assert!(dock < chrome);
            assert!(chrome < bg);
            // 控件底：深色主题要比栏更亮（抬起），浅色主题要比面板更暗（凹下去）。
            if p.text[0] > 128 {
                assert!(l(p.elev) > chrome);
            } else {
                assert!(l(p.elev) < panel);
            }
        }
    }

    /// 明暗微调只动背景族，文字三档必须原地不动（自定义外观的语义契约）。
    ///
    /// The brightness trim moves the background family only; the three text steps
    /// must stay put (the semantic contract of the custom-appearance trim).
    #[test]
    fn gray_offset_moves_backgrounds_but_never_text() {
        let mut a = Appearance::default();
        let base = Palette::of(&a);
        a.gray_offset = 2;
        let up = Palette::of(&a);
        assert!(
            luminance(up.panel) > luminance(base.panel),
            "+2 档必须让面板底变亮（背景族没接上 nb）"
        );
        assert!(
            luminance(up.surround) > luminance(base.surround),
            "+2 档必须让画布外围变亮（背景族没接上 nb）"
        );
        assert_eq!(up.text, base.text, "文字档不随明暗微调变化");
        assert_eq!(up.muted, base.muted, "次要文字不随明暗微调变化");
        assert_eq!(up.dim, base.dim, "三级文字不随明暗微调变化");
        a.gray_offset = -2;
        let down = Palette::of(&a);
        assert!(
            luminance(down.panel) < luminance(base.panel),
            "-2 档必须让面板底变暗"
        );
    }

    /// 默认参数下必须逐通道还原旧的手调令牌（±3）。
    ///
    /// 这条测试是这次重构的安全网：色值一旦公式漂移，界面会"说不上哪里不对"，
    /// 靠人眼比不出来，只能靠数字钉住。
    #[test]
    fn dark_default_matches_legacy_tokens() {
        let p = Palette::of(&Appearance::default());
        // 2026-11-10：整表按 docs/aevum-v12-timeline.html 的设计 token 重排后，
        // 这组快照同步更新（旧值是上一轮 ACR 暖深灰的产物，与目标设计稿的
        // 「深底 + 明显分层」不是同一个语法）。偏离设计稿的只有两处，理由跟在
        // 对应项后面；其余严格等于 hsl(220,8%,L) 的输出。
        //
        // 2026-11-10: the whole table was re-laid per the design tokens in
        // docs/aevum-v12-timeline.html, so this snapshot follows. The old values were
        // the previous warm-ACR pass, a different grammar from the target design's
        // "dark base + pronounced layering". Only two entries depart from the design,
        // each with its reason inline; the rest equal hsl(220,8%,L) exactly.
        let cases: [(Color32, u32, &str); 12] = [
            (p.bg, 0x2D2F34, "BG"),
            (p.chrome, 0x282A2F, "CHROME"),
            (p.panel, 0x212327, "PANEL"),
            (p.dock, 0x24262A, "DOCK"),
            (p.header, 0x2D2F34, "HEADER"),
            (p.surround, 0x121315, "SURROUND"),
            (p.canvas, 0x151619, "CANVAS"),
            (p.elev, 0x33363C, "ELEV"),
            (p.elev_hover, 0x3C4047, "ELEV_HOVER"),
            (p.field, 0x1B1C1F, "FIELD"),
            (p.text, 0xF4F4F5, "TEXT"),
            // 设计稿 text3 = #8f939c(L58.6)：对 bar 只有 4.49:1，差 0.01 不过
            // WCAG AA。抬到 L62 是唯一保底做法，比设计稿亮约 8 级。
            //
            // The design's text3 #8f939c (L58.6) reaches only 4.49:1 against bar --
            // 0.01 short of WCAG AA. Raising it to L62 is the only way to hold the
            // floor; it sits about 8 levels above the mockup.
            (p.muted, 0xC7CAD0, "MUTED"),
        ];
        for (got, want, name) in cases {
            for i in 0..3 {
                let w = ((want >> (16 - i * 8)) & 0xFF) as i32;
                let g = got[i] as i32;
                assert!((g - w).abs() <= 3, "{name} 通道 {i}: {g} vs {w}");
            }
        }
    }

    /// 换主色只该动强调色一族，中性材质一个通道都不许动。
    #[test]
    fn accent_does_not_leak_into_materials() {
        let a = Appearance::default();
        let b = Appearance {
            accent_h: 32.0,
            accent_s: 94.0,
            accent_l: 53.0,
            ..a
        };
        let (pa, pb) = (Palette::of(&a), Palette::of(&b));
        assert_eq!(pa.panel, pb.panel);
        assert_eq!(pa.text, pb.text);
        assert_ne!(pa.accent, pb.accent);
        assert_ne!(pa.selected, pb.selected);
    }

    /// 浅色主题必须真的翻面：材质比文字亮，强调底上的字要变深。
    #[test]
    fn light_theme_inverts_the_stack() {
        let p = Palette::of(&Appearance {
            theme: ThemeMode::Light,
            ..Default::default()
        });
        assert!(luminance(p.panel) > luminance(p.text));
        assert_eq!(p.on_selected, p.text);
        assert!(p.border[3] > 0);
    }

    #[test]
    fn overlay_white_flattens_labels() {
        let panel = panel();
        assert_eq!(overlay_white(panel, 255), Color32::WHITE);
        assert_eq!(overlay_white(panel, 0), panel);
        let c = overlay_white(panel, 217); // 白 85%
        assert!(c[0] >= 0xD8 && c[0] <= 0xE2, "{c:?}");
    }
}

/// 字体装配的判据。这里要拦的两类事故都是「肉眼要过很久才发现」的：
/// 字族名打错（整族静默变豆腐块）、以及子集字体没跟着源码更新。
///
/// Judges for font assembly. Both accidents guarded here take a long time to notice by
/// eye: a mistyped family name (the whole family silently becomes tofu) and a subset
/// font that fell behind the source.
#[cfg(test)]
mod font_tests {
    use super::*;

    fn weight(bytes: &[u8]) -> f32 {
        use skrifa::MetadataProvider;
        let font = skrifa::FontRef::new(bytes).expect("字体解析失败");
        font.attributes().weight.value()
    }

    fn cmap(bytes: &[u8]) -> std::collections::HashSet<char> {
        use skrifa::MetadataProvider;
        let font = skrifa::FontRef::new(bytes).expect("字体解析失败");
        let map = font.charmap();
        let mut out = std::collections::HashSet::new();
        for cp in 0x20u32..=0xFFFF {
            if let Some(ch) = char::from_u32(cp)
                && map.map(ch).is_some()
            {
                out.insert(ch);
            }
        }
        out
    }

    /// 每个字族引用的名字都必须在 `font_data` 里真有那一份。名字打错时 egui 不报警，
    /// 只是那个字族整片画不出来。
    ///
    /// Every family entry must name a face that actually exists in `font_data`. A
    /// typo raises no error in egui -- the family simply fails to draw.
    #[test]
    fn every_family_entry_points_at_installed_font_data() {
        let f = font_definitions();
        for (family, list) in &f.families {
            assert!(!list.is_empty(), "{family:?} 字族是空的");
            for name in list {
                assert!(
                    f.font_data.contains_key(name),
                    "{family:?} 引用了不存在的字体 {name}"
                );
            }
        }
    }

    /// 粗体字族的兜底链必须**逐项等于**常规字族那条。少一项就意味着有一类字在
    /// 粗体下没有兜底（egui 自带的符号字体也在末尾），画出来是豆腐块；顺序错了
    /// 则意味着某些字会先撞上一个画不出它的字体。
    ///
    /// The bold family's fallback chain must **match the regular chain item for item**.
    /// A missing entry means some class of character has no fallback under bold (egui's
    /// bundled symbol fonts sit at the end too) and renders as tofu; a wrong order means
    /// some characters hit a face that cannot draw them first.
    #[test]
    fn the_bold_family_falls_back_through_the_regular_chain() {
        let f = font_definitions();
        let bold = &f.families[&bold_family()];
        let regular = &f.families[&FontFamily::Proportional];
        // 系统粗体（Windows 上是 Segoe UI Bold / 雅黑 Bold）排在最前，两份嵌入
        // 700 子集紧随其后，再往后必须与常规字族完全一致——缺字的唯一出路是
        // 画成常规体。判据按位置断言而不是按下标：没有系统字体的机器上，
        // inter-bold 就是第 0 项，性质不变。
        //
        // System bolds (Segoe UI Bold / YaHei Bold on Windows) lead, the two embedded
        // 700 subsets follow, and everything after must equal the regular family exactly
        // -- a missing glyph's only escape is rendering regular. Assert by position, not
        // index: on machines without the system fonts inter-bold IS item 0, same rule.
        let at = bold
            .iter()
            .position(|f| f == "inter-bold")
            .expect("粗体链里没有 inter-bold");
        assert_eq!(
            &bold[at..at + 2],
            ["inter-bold", "noto-bold"],
            "嵌入粗体子集不连续"
        );
        assert_eq!(
            &bold[at + 2..],
            regular.as_slice(),
            "粗体字族的兜底链与常规字族不一致"
        );
        for (bytes, name) in [(INTER, "inter"), (NOTO, "noto")] {
            assert!(
                weight(bytes) < 600.0,
                "{name} 不是常规字重（{}），兜不住缺字",
                weight(bytes)
            );
        }
    }

    /// 粗体那一份必须真的比常规那一份重。把常规体改名成 bold 也能骗过前两条，
    /// 这一条骗不过——它直接读 `OS/2.usWeightClass`。
    ///
    /// The bold faces must genuinely be heavier than the regular ones. Renaming a
    /// regular file to "bold" fools the two tests above but not this one: it reads
    /// `OS/2.usWeightClass` directly.
    #[test]
    fn bold_faces_are_heavier_than_their_regular_twins() {
        for (bold, regular, label) in [
            (INTER_BOLD, INTER, "Inter"),
            (NOTO_BOLD, NOTO, "Noto Sans SC"),
        ] {
            let (b, r) = (weight(bold), weight(regular));
            assert!(
                b >= 600.0 && b > r + 100.0,
                "{label}: 粗体 {b} vs 常规 {r}，字重差不够"
            );
        }
    }

    /// 去掉注释、只留代码。注释里的字不进分母：判据关心的是「界面会画出哪些字」，
    /// 一句注释里写个生僻字不该逼着别人去重切子集。
    ///
    /// 只做行注释与块注释两种粗判，不做真解析：字符串里出现 `//`（比如 URL）会把
    /// 后半行截掉，方向是**少收**——漏掉几个字不会造成误报，宁可这样。
    ///
    /// Strip comments, keep code. Characters in comments stay out of the denominator: the
    /// judge cares about "which characters the UI draws", and a rare character in a
    /// comment should not force anyone to re-cut the subset.
    ///
    /// Only crude line- and block-comment handling, no real parsing: a `//` inside a
    /// string (a URL, say) truncates the rest of that line, and the direction is to
    /// **under-collect** -- missing a few characters cannot produce a false alarm.
    fn code_only(src: &str) -> String {
        let chars: Vec<char> = src.chars().collect();
        let mut out = String::with_capacity(src.len());
        let (mut in_block, mut in_str) = (false, false);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied().unwrap_or('\0');
            if in_block {
                if c == '*' && next == '/' {
                    in_block = false;
                    i += 1;
                }
            } else if c == '/' && next == '/' {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            } else if c == '/' && next == '*' {
                in_block = true;
                i += 1;
            } else {
                if c == '"' {
                    in_str = !in_str;
                }
                out.push(c);
            }
            i += 1;
        }
        out
    }

    /// 子集必须盖住「我们自己的字体本来画得出」的每一个字。判据和
    /// `tools/font_subset.py` 的收字区段是同一套：那边切子集、这边验证，忘了
    /// 重跑脚本就红在这里。
    ///
    /// 分母取「常规那两份的 cmap」而不是「源码里所有字」：箭头、圈号这类符号
    /// 两份源字体本来就没有，一直由 egui 自带的字体画，粗体下也照旧由它画——
    /// 把它们算进分母只会逼着子集去收不属于自己的字。
    ///
    /// The subset must cover every character **our own fonts can already draw**. The
    /// ranges match `tools/font_subset.py`: that script cuts the subset, this test
    /// checks it, and forgetting to re-run turns red here.
    ///
    /// The denominator is the cmap of the two regular faces rather than "all source
    /// characters": arrows and circled digits are absent from both source fonts and
    /// have always been drawn by egui's bundled fonts, bold or not -- counting them
    /// would only force the subset to carry glyphs that are not its own.
    #[test]
    fn bold_faces_cover_every_character_the_ui_uses() {
        // 与 tools/font_subset.py 的 RANGES 一一对应。
        const RANGES: [(u32, u32); 11] = [
            (0x0020, 0x007E),
            (0x00A0, 0x00FF),
            (0x2000, 0x206F),
            (0x2100, 0x214F),
            (0x2190, 0x21FF),
            (0x2200, 0x22FF),
            (0x2460, 0x24FF),
            (0x25A0, 0x25FF),
            (0x3000, 0x303F),
            (0x4E00, 0x9FFF),
            (0xFF00, 0xFFEF),
        ];
        let covered: std::collections::HashSet<char> = cmap(INTER_BOLD)
            .into_iter()
            .chain(cmap(NOTO_BOLD))
            .collect();
        let drawable: std::collections::HashSet<char> =
            cmap(INTER).into_iter().chain(cmap(NOTO)).collect();

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missing: Vec<(char, String)> = Vec::new();
        let mut files = vec![root.clone()];
        while let Some(dir) = files.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    files.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    for ch in code_only(&text).chars() {
                        let cp = ch as u32;
                        if RANGES.iter().any(|(lo, hi)| *lo <= cp && cp <= *hi)
                            && drawable.contains(&ch)
                            && !covered.contains(&ch)
                            && !missing.iter().any(|(c, _)| *c == ch)
                        {
                            missing.push((ch, p.display().to_string()));
                        }
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "子集缺 {} 个字（重跑 python tools/font_subset.py）：{:?}",
            missing.len(),
            &missing[..missing.len().min(8)]
        );
    }
}

/// 浮窗尺寸的契约：比例、视口夹取、Modal 高度上限。
///
/// Floating-window size contracts: aspect ratios, viewport clamping and the modal cap.
#[cfg(test)]
mod window_size_tests {
    use super::*;

    /// 两类窗口的比例：横向对话框接近 4:3~3:2，竖向窄面板是 1:2 左右的柱形。
    /// 混着来就会出现「同一排窗口有的像纸条、有的像横幅」。
    ///
    /// Two classes of window: landscape dialogs near 4:3-3:2, and tall narrow panels
    /// around 1:2. Mixing them freely is what produced "some look like strips, some
    /// like banners" on one screen.
    #[test]
    fn window_aspect_ratios_stay_in_their_class() {
        for (w, h) in [
            (WIN_MARKET_W, WIN_MARKET_H),
            (WIN_ABOUT_W, WIN_ABOUT_H),
            (WIN_UPDATE_W, WIN_UPDATE_H),
        ] {
            let r = w / h;
            assert!((0.85..=1.75).contains(&r), "横向窗比例 {r} 跑偏：{w}×{h}");
        }
        for (w, h) in [(WIN_LOOK_W, WIN_LOOK_H), (WIN_PRESETS_W, WIN_PRESETS_H)] {
            let r = w / h;
            assert!((0.40..=0.70).contains(&r), "窄面板比例 {r} 跑偏：{w}×{h}");
        }
    }

    /// 五扇窗在 1280×800 与 1600×1000 下都放得下，且默认值不比上界大。
    ///
    /// All five windows fit at both 1280x800 and 1600x1000, and no default exceeds
    /// the viewport-derived upper bound.
    #[test]
    fn every_window_fits_1280x800_and_1600x1000() {
        let sizes = [
            (WIN_MARKET_W, WIN_MARKET_H),
            (WIN_LOOK_W, WIN_LOOK_H),
            (WIN_PRESETS_W, WIN_PRESETS_H),
            (WIN_ABOUT_W, WIN_ABOUT_H),
            (WIN_UPDATE_W, WIN_UPDATE_H),
        ];
        for vp in [Vec2::new(1280.0, 800.0), Vec2::new(1600.0, 1000.0)] {
            let m = win_max(vp);
            assert!(m.x <= vp.x * WIN_MAX_W_FRAC + 1e-3);
            assert!(m.y <= vp.y * WIN_MAX_H_FRAC + 1e-3);
            for (w, h) in sizes {
                let d = win_default(Vec2::new(w, h), vp);
                assert!(d.x <= m.x && d.y <= m.y, "{w}×{h} 在 {vp:?} 越界：{d:?}");
                assert!(d.x >= 1.0 && d.y >= 1.0);
            }
        }
        // 极窄视口：上界先跟着收，默认值不得反超。
        //
        // A tiny viewport: the bound shrinks first and defaults must not overshoot it.
        let small = Vec2::new(700.0, 500.0);
        let m = win_max(small);
        assert_eq!(m.x, 700.0 * WIN_MAX_W_FRAC);
        assert!(win_default(Vec2::new(WIN_MARKET_W, WIN_MARKET_H), small).x <= m.x);
    }

    /// Modal 永远不超过视口的 62%，宽度也留出 24px 边距。
    ///
    /// A modal never exceeds 62% of the viewport, and leaves a 24 px side margin.
    #[test]
    fn modals_cap_height_and_width() {
        for h in [600.0, 800.0, 1000.0, 2000.0] {
            let cap = modal_max_h(h);
            assert!(cap <= h * MODAL_MAX_H_FRAC + 1e-3, "高 {h}：上限 {cap}");
        }
        // 小视口的 200px 地板是为了不让内容被压成一条缝，但仍不得超过视口。
        //
        // The 200 px floor on tiny viewports keeps content readable without exceeding it.
        assert_eq!(modal_max_h(800.0), 800.0 * MODAL_MAX_H_FRAC);
        // 0.62 是产品口径（用户要看得到照片再确认），常量被顺手改大也要红。
        //
        // 0.62 is the product rule (the user must still see the photo before confirming);
        // raising the constant casually must turn this red.
        assert!(modal_max_h(800.0) <= 800.0 * 0.62 + 1e-3);
        assert_eq!(modal_width(MODAL_W_WIDE, 1280.0), MODAL_W_WIDE);
        assert!(modal_width(MODAL_W_WIDE, 500.0) <= 500.0 - 48.0 + 1e-3);
    }
}
