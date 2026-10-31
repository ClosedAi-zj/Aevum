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

use egui::{
    Align2, Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Pos2, RichText,
    Stroke, TextStyle, Vec2,
};

/// 把白色以 alpha/255 压平到基色上，得到 macOS“深底 + 白叠加”的实际不透明色。
pub fn overlay_white(base: Color32, white_alpha: u8) -> Color32 {
    let a = white_alpha as u32;
    let blend = |b: u8| -> u8 {
        let v = (255 * a + b as u32 * (255 - a) + 127) / 255;
        if v > 255 { 255 } else { v as u8 }
    };
    Color32::from_rgb(blend(base[0]), blend(base[1]), blend(base[2]))
}

/// sRGB 相对亮度（WCAG 口径），用于对比度断言。
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
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: ThemeMode,
    pub skin: Skin,
    /// 强调色，HSL 三数：改这三个数，36 处半透明底与全部描边一起变。
    pub accent_h: f32,
    pub accent_s: f32,
    pub accent_l: f32,
    /// 控件圆角。窗口与容器由它派生，不单独给。
    pub radius: f32,
    /// 外壳形态：跟随编译目标，或强制成另一家的样子（截图对比用）。
    pub os: crate::platform::OsChoice,
    /// 调整面板行高密度档（紧凑/标准/宽松）。
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
    pub press: Color32,
    pub selected: Color32,
    /// 选中行的横向渐变末端：从 `selected` 衰减到这一档，左深右浅的光带。
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
    pub on_accent: Color32,
    /// 选中底上的字（深色白字，浅色用正文色）。
    pub on_selected: Color32,
    /// 抬起控件（ELEV）上的字。
    pub on_elev: Color32,
}

impl Palette {
    /// 由参数算出整套色值。这是唯一的派生入口。
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
pub fn appearance() -> Appearance {
    state().read().map(|s| s.appearance).unwrap_or_default()
}

/// 当前色板。绘制代码唯一该读的东西。
pub fn palette() -> Palette {
    state()
        .read()
        .map(|s| s.palette)
        .unwrap_or_else(|_| Palette::of(&Appearance::default()))
}

/// 换一套参数并立刻重算色板；调用方随后要再调一次 [`apply`] 把 egui 的
/// `Visuals` 也刷一遍（那里还压着一堆派生值）。
pub fn set_appearance(a: Appearance) {
    if let Ok(mut s) = state().write() {
        s.appearance = a;
        s.palette = Palette::of(&a);
    }
}

/// 当前是不是浅色底：少数几处（阴影、发丝线权重）需要知道方向。
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
pub fn accent_rgb() -> [u8; 3] {
    let c = accent();
    [c[0], c[1], c[2]]
}

/// sRGB → HSL，与 [`hsl`] 互逆。取色器给的是 RGB，而参数存的是 HSL：
/// 存 HSL 是为了让「同一主色的深浅档」能算出来，存 RGB 就只能一个个手挑。
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
/// 左侧预设栏：预设 / 快照 / 历史三张表，240 刚好放下"胶片 · 富士 400H"这种长名。
pub const LEFT_W: f32 = 240.0;
pub const DOCK_W: f32 = 372.0;
/// 最外那条 28px 竖条：网格 / 胶片 / 曲线三个开关，界面稿里是 ⌗ ◐ ◑ 三枚。
pub const STRIP_W: f32 = 28.0;
/// 胶片条总高：正文只有"标题 + 缩略图"两段（界面稿 --tl-h:120），
/// 这里多留 68 —— 这个软件是序列工具，底部还要一行播放控制。
pub const TIMELINE_H: f32 = 188.0;

/// 面板里的两种行高：推子行要容下标签、轨道与数值，键值行只排一对字。
/// 之前都是按最小可放下的高度定的，整列读起来像密排表；现在按 macOS 侧栏的
/// 行距留一口气，一屏少放两三行，但每一行都能一眼读清。
/// 行高密度档。紧凑给高分屏小尺寸预览多看几行，宽松给触屏/演示。
/// 只影响推子行（`row_h`）与键值行（`kv_h`）；标题栏/工具栏等外壳高度
/// 是结构尺寸，不随密度变——那是界面稿钉死的骨架。
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
pub fn row_h() -> f32 {
    match appearance().density {
        Density::Compact => 32.0,
        Density::Standard => 38.0,
        Density::Loose => 46.0,
    }
}

/// 键值行 / 剪切读数行高：20 / 24 / 30。
pub fn kv_h() -> f32 {
    match appearance().density {
        Density::Compact => 20.0,
        Density::Standard => 24.0,
        Density::Loose => 30.0,
    }
}

/// 画布内部顶栏（cv-toolbar）：文档页签 + 尺寸读数 + 右端上下文，28px。
pub const CV_TOOLBAR_H: f32 = 28.0;
/// 胶片条头部行：标题 + 右端筛选说明，26px。
pub const TL_HD_H: f32 = 26.0;

/// 兼容旧命名。
pub const TOP_H: f32 = MENU_H;
pub const RAIL_W: f32 = TOOL_W;
pub const PARAMS_W: f32 = DOCK_W;
pub const INSPECTOR_W: f32 = DOCK_W;

/// 字号四档缩放（对外行为不变）：基准档对应 11/13/15/17 字阶。
pub const FONT_ZOOMS: [f32; 4] = [1.0, 1.06, 1.14, 1.24];

/// macOS 字阶基准：正文 13、小字 11、大标题 15（17 顶格由 Heading+缩放档覆盖）。
pub const SIZE_SMALL: f32 = 11.0;
pub const SIZE_BODY: f32 = 13.0;
pub const SIZE_HEADING: f32 = 15.0;
/// 面板里的推子标签与键值行：比 11 号小字大一档，11 号在 372px 宽的面板上
/// 读起来像脚注，而参数名是这一页最常读的字。
pub const SIZE_LABEL: f32 = 12.0;
/// 顶部两行（菜单行 36 / 选项栏 46）专用字阶：行高加起来了，标签仍用 11 会
/// 显得整条栏是空的，所以顶部比正文大一号，面板里的推子标签不受影响。
pub const SIZE_CHROME: f32 = 14.0;
/// 选项栏里的次要标签与状态文字：比小字大一号，跟得上加高后的行高。
pub const SIZE_CHROME_LABEL: f32 = 12.0;

// 字体字节只在水印那个 crate 里落一次（`aevum_core::fonts`）：Noto 一份 10 MB，
// 两个 crate 各 include_bytes! 一次，产物就要多背 10 MB。
const INTER: &[u8] = aevum_core::fonts::INTER;
// Noto Sans SC 可变字体的默认字重是 100（Thin），直接嵌入会让中文细得像虚线；
// 这里用 fontTools 实例化出的 500 静态字重。
const NOTO: &[u8] = aevum_core::fonts::NOTO_SC;

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
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
pub fn set_fonts(ctx: &Context) {
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
    ctx.set_fonts(fonts);
}

/// 只重刷 `Visuals`，不动字体。
///
/// 外观面板拖滑块时每帧都会走到这里：换色只是重算几十个 Color32 与一次
/// `set_style`，比重新装字体便宜两个数量级。
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
pub fn rule(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    painter.line_segment(
        [
            Pos2::new(x0, y.round() + 0.5),
            Pos2::new(x1, y.round() + 0.5),
        ],
        Stroke::new(1.0, color),
    );
}

/// Lightroom 的数值写法：有符号量带正号，零显示占位。
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
