//! 设计令牌与全局样式。
//!
//! 观感对齐 macOS 深色模式（Apple HIG Dark Mode 口径）：
//! - 材质分层取代实线分隔：窗底最暗，内容区略亮，栏/侧栏再亮一档，浮层最亮；
//!   分隔线用白色低 alpha（8–18%）的发丝线，不再像描边。
//! - 文字四级 label：白 85% / 60% / 45% / 10% alpha 压平到实际材质色值，
//!   保证正文级 ≥ 7:1、次要级 ≥ 4.5:1（4K @ 96dpi，正文不许发灰）。
//! - 强调色为 macOS 深色系统蓝 #0A84FF；选中行背景是同色约 20–25% 的压平值。
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
//! - The accent is macOS dark system blue #0A84FF; a selected row's background is the
//!   same hue flattened at roughly 20-25%.
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
    /// (hue, saturation). Across the four skins these numbers decide whether the whole
    /// interface leans cool or warm.
    pub fn hsl(self) -> (f32, f32) {
        match self {
            Self::Graphite => (240.0, 2.0),
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
            accent_h: 210.0,
            accent_s: 100.0,
            accent_l: 52.0,
            radius: 6.0,
            os: crate::platform::OsChoice::Auto,
            density: Density::Standard,
        }
    }
}

/// 强调色预设：面板上排七个色点，点一下就是这套数。
///
/// Accent presets: the seven dots across the panel, one click each.
pub const ACCENT_PRESETS: [(f32, f32, f32, &str); 7] = [
    (210.0, 100.0, 52.0, "蓝"),
    (265.0, 86.0, 63.0, "紫"),
    (188.0, 92.0, 46.0, "青"),
    (148.0, 56.0, 44.0, "绿"),
    (32.0, 94.0, 53.0, "琥珀"),
    (345.0, 84.0, 61.0, "玫红"),
    (0.0, 0.0, 60.0, "银"),
];

/// 一张亮度表。
///
/// 深色 / 纯黑 / 浅色只是这张表里的数不同，派生公式一个字都不用改；
/// `dir` 是明暗方向（深色 +1、浅色 −1），所有「比基准亮一档」的派生都乘它。
struct Ladder {
    surround: f32,
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

const DARK: Ladder = Ladder {
    surround: 7.2,
    canvas: 11.0,
    panel: 14.0,
    dock: 15.5,
    chrome: 16.5,
    bg: 18.2,
    header: 12.5,
    elev: 23.0,
    elev_h: 29.0,
    field: 12.0,
    field_hover: 14.6,
    thumb: 5.5,
    text: 87.5,
    muted: 66.5,
    dim: 59.0,
    dir: 1.0,
};

const BLACK: Ladder = Ladder {
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

        let panel = n(l.panel);
        let accent = hsl(a.accent_h, a.accent_s, a.accent_l);
        let text = n(l.text);

        Self {
            bg: n(l.bg),
            chrome: n(l.chrome),
            panel,
            dock: n(l.dock),
            header: n(l.header),
            surround: n(l.surround),
            canvas: n(l.canvas),
            rail: n(l.chrome),
            elev: n(l.elev),
            elev_hover: n(l.elev_h),
            press: n(l.elev - 3.0 * dir),
            selected: mix(panel, accent, 0.22),
            // 渐变从 selected 衰到贴着 dock 的一档：一条式三表自动成立，
            // 换主色时两端一起动，不会出现左蓝右灰的断裂。
            selected_row_end: mix(n(l.dock), accent, 0.05),
            field: n(l.field),
            field_hover: n(l.field_hover),
            border: with_alpha(ink, alpha(20, 34)),
            border_strong: with_alpha(ink, alpha(46, 60)),
            divider: with_alpha(ink, alpha(26, 34)),
            text,
            muted: n(l.muted),
            dim: n(l.dim),
            // 读数：强调色降饱和 26、加亮 24（浅色主题反向），比纯白少抢一点。
            value: hsl(
                a.accent_h,
                (a.accent_s - 26.0).max(0.0),
                a.accent_l + 24.0 * dir,
            ),
            accent,
            accent_hover: hsl(a.accent_h, a.accent_s, a.accent_l + 10.0),
            accent_dim: mix(panel, accent, 0.17),
            rail_fill: mix(panel, ink, 0.18),
            // 滑轨对齐界面稿 `--track`：抬升面往回收两度，而不是从面板压暗。
            track: n(l.elev - 2.0 * dir),
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
            thumb_bg: n(l.thumb),
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

pub fn radius_window() -> f32 {
    radius_control() + 4.0
}

pub fn radius_panel() -> f32 {
    radius_control() + 2.0
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

// 尺寸全部对齐界面设计稿的 L3 段：
// `--opt-h:44 --rail-w:52 --left-w:240 --dock-w:372 --strip-w:28 --tl-h:120`。
// 这几个数是"整套界面读起来像不像那张图"的根：差 6px 的轨宽、差 70px 的胶片条，
// 单独看都无所谓，摆在一起就是另一种软件。
pub const MENU_H: f32 = 36.0;
pub const OPTIONS_H: f32 = 44.0;
pub const TAB_H: f32 = 28.0;
pub const STATUS_H: f32 = 24.0;
pub const TOOL_W: f32 = 52.0;
/// 右侧调整面板宽 372：界面稿 `--dock-w`。推子行「标签 76 · 轨道弹性 · 读数 52」
/// 的排布就是照这个宽度定的，窄一档标签就开始抢轨道的地盘。
///
/// Right-hand adjustment panel, 372 px (mockup `--dock-w`). The slider row
/// "label 76 · elastic rail · value 52" is dimensioned for exactly this width; one step
/// narrower and the label starts eating the rail.
pub const DOCK_W: f32 = 372.0;
/// 最外那条 28px 竖条：网格 / 胶片 / 曲线三个开关，界面稿里是 ⌗ ◐ ◑ 三枚。
///
/// The outermost 28 px vertical strip: grid / filmstrip / curve toggles, drawn as three
/// glyphs in the mockup.
pub const STRIP_W: f32 = 28.0;
/// 胶片条总高：正文只有"标题 + 缩略图"两段（界面稿 --tl-h:120），
/// 这里多留 68 —— 这个软件是序列工具，底部还要一行播放控制。
///
/// Total filmstrip height: the body is only "title + thumbnails" (mockup `--tl-h:120`),
/// and the extra 68 is because this is a sequence tool — a playback row lives at the
/// bottom as well.
pub const TIMELINE_H: f32 = 188.0;

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

/// 调整面板推子行高：紧凑 32 / 标准 38 / 宽松 46。
///
/// Slider row height in the adjustment panel: compact 32 / standard 38 / roomy 46.
pub fn row_h() -> f32 {
    match appearance().density {
        Density::Compact => 32.0,
        Density::Standard => 38.0,
        Density::Loose => 46.0,
    }
}

/// 键值行 / 剪切读数行高：20 / 24 / 30。
///
/// Key/value row and crop readout row height: 20 / 24 / 30.
pub fn kv_h() -> f32 {
    match appearance().density {
        Density::Compact => 20.0,
        Density::Standard => 24.0,
        Density::Loose => 30.0,
    }
}

/// 画布内部顶栏（cv-toolbar）：文档页签 + 尺寸读数 + 右端上下文，28px。
///
/// Canvas top bar (cv-toolbar): document tabs + size readout + right-hand context, 28
/// px.
pub const CV_TOOLBAR_H: f32 = 28.0;
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
pub fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("inter".to_owned(), FontData::from_static(INTER).into());
    fonts
        .font_data
        .insert("noto".to_owned(), FontData::from_static(NOTO).into());
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        list.insert(0, "inter".to_owned());
        list.insert(1, "noto".to_owned());
    }
    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        list.push("noto".to_owned());
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
    let mut bold_chain = vec!["inter-bold".to_owned(), "noto-bold".to_owned()];
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
    style.spacing.scroll.floating = false;
    style.spacing.indent = 12.0;
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
    v.window_stroke = Stroke::new(1.0, border());
    v.window_corner_radius = radius_window().into();
    v.menu_corner_radius = radius_window().into();
    v.hyperlink_color = accent();
    v.warn_fg_color = warn();
    v.error_fg_color = danger();
    v.weak_text_alpha = 0.8;
    // 选中底跟着主色走：换主色时这一块也要变，不能钉死在系统蓝上。
    v.selection.bg_fill =
        Color32::from_rgba_unmultiplied(accent()[0], accent()[1], accent()[2], 64);
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
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("chip", label)),
        egui::Sense::click(),
    );
    // macOS 小胶囊：靠底色差分层，只有选中态保留强调色描边。
    let (fill, stroke, fg, cr) = if active {
        (
            selected(),
            Stroke::new(1.0, accent),
            Color32::WHITE,
            radius_control(),
        )
    } else if resp.hovered() {
        (elev_hover(), Stroke::NONE, text(), radius_control())
    } else {
        (elev(), Stroke::NONE, muted(), radius_control())
    };
    painter.rect_filled(rect, cr, fill);
    if stroke.width > 0.0 {
        painter.rect_stroke(rect, cr, stroke, egui::StrokeKind::Inside);
    }
    painter.text(rect.center(), Align2::CENTER_CENTER, label, font(11.0), fg);
    resp
}

/// 1px 发丝分隔线：颜色由调用方给（BORDER/DIVIDER 已是白 8–10% alpha）。
///
/// 1 px hairline separator: the caller supplies the colour (`BORDER`/`DIVIDER` are
/// already white at 8-10% alpha).
pub fn rule(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    painter.line_segment(
        [
            Pos2::new(x0, y.round() + 0.5),
            Pos2::new(x1, y.round() + 0.5),
        ],
        Stroke::new(1.0, color),
    );
}

/// 浮窗与侧栏分节的「软分割线」：左右各缩进 12px、颜色再压暗一档。
///
/// 通栏 1px 线读作「把两块切开」，缩进的浅线读作「这是两节」——分割样式
/// 重设计的落点就在这个语义差上。两侧留白让线不顶到边框，视觉上短一截，
/// 恰好和分节标题的左缘缩进（12px）对齐。
///
/// The "soft divider" between floating windows and right-column sections: inset 12 px on
/// each side, one step darker.
///
/// A full-width 1 px line reads as "cut these two apart", while an inset faint line reads as
/// "these are two sections" — that semantic difference is exactly what the divider redesign
/// hangs on. The inset keeps the line off the frames, making it visually shorter and lining
/// it up with the 12 px left inset of section headings.
pub fn soft_rule(painter: &egui::Painter, x0: f32, x1: f32, y: f32) {
    const INSET: f32 = 12.0;
    rule(
        painter,
        x0 + INSET,
        x1 - INSET,
        y,
        divider().gamma_multiply(0.7),
    );
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

    /// 默认参数下必须逐通道还原旧的手调令牌（±3）。
    ///
    /// 这条测试是这次重构的安全网：色值一旦公式漂移，界面会"说不上哪里不对"，
    /// 靠人眼比不出来，只能靠数字钉住。
    #[test]
    fn dark_default_matches_legacy_tokens() {
        let p = Palette::of(&Appearance::default());
        let cases: [(Color32, u32, &str); 12] = [
            (p.bg, 0x2E2E30, "BG"),
            (p.chrome, 0x29292B, "CHROME"),
            (p.panel, 0x232325, "PANEL"),
            (p.dock, 0x262628, "DOCK"),
            (p.header, 0x1F1F21, "HEADER"),
            (p.surround, 0x121213, "SURROUND"),
            (p.canvas, 0x1A1A1C, "CANVAS"),
            (p.elev, 0x3A3A3C, "ELEV"),
            (p.elev_hover, 0x48484A, "ELEV_HOVER"),
            (p.field, 0x1E1E20, "FIELD"),
            (p.text, 0xDEDEDE, "TEXT"),
            (p.muted, 0xA8A8AA, "MUTED"),
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
        assert_eq!(&bold[..2], ["inter-bold", "noto-bold"], "粗体项位置变了");
        assert_eq!(
            &bold[2..],
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
