//! v20 界面原型（可交互）：显影屏完整体验。
//!
//! 交互：右坞 tab 切换（基本 / 色彩 / 效果）、颜色混合器两级 tab、八行频段滑块可拖可双击复位、
//! 画布滚轮缩放 + 拖拽平移 + 双击适应、左侧预设与历史可点、胶片条缩略图可点、顶栏菜单悬停反馈。
//!
//! Interaction: dock tabs, the two-level colour-mixer tabs, draggable band sliders
//! (double-click resets), canvas wheel-zoom / drag-pan / double-click fit, clickable
//! presets, history rows and filmstrip thumbnails, hover feedback on the menu row.
//!
//! 运行 / Run:
//!   cargo run --example ui_proto -p aevum-ui
//! 可选参数（用于截图不同状态）/ Optional args:
//!   prop=hue|sat|lum|all   mono   basic   effect   zoom=2.0   film=8   preset=2
//!
//! 颜色混合器与 `shell/adjust/develop.rs::adjust_mixer` 同构：
//! 一级 tab（HSL / 黑白）→ 属性 tab（色相 / 饱和度 / 明亮度 / 全部）→ 八个频段。

// 本文件是可交互视觉原型：频段/通道刻意用固定下标并行索引多个状态数组，
// `slider_row` / `band_slider` 的参数与共享几何契约一一对应。
// 这些为保持数据与绘制逐点对齐而保留的写法会触发下列 clippy 风格提示，
// 故仅在本示例文件范围内允许，避免重构改动已验证的绘制与交互行为。
#![allow(
    clippy::collapsible_if,
    clippy::manual_is_multiple_of,
    clippy::needless_range_loop,
    clippy::redundant_closure,
    clippy::too_many_arguments
)]

// ============================================================ 设计契约
//
// 这 10 条不是口号，每一条都有可执行的检查手段（`design_audit` 测试或 grep）。
// 改动界面时若违反其中任何一条，测试或验收脚本会直接报出来。
//
// 10 rules, each with an executable check (`design_audit` or a grep). Violating any
// of them fails the audit rather than being a matter of taste.
//
// C1 明度阶梯：八档表面的 L* 必须单调递增，相邻级差 ≥1.5，井→壳 ≥6.0。
// C2 中性：所有中性色 R=G=B（|R−B| ≤ 2）——Photoshop 口径。
//     暖灰/冷灰都会给照片的曝光与白平衡判读引入系统性色偏（用户实测对比 PS 后定案）。
// C3 文字：T1 ≥7.0 / T2、T3、T4_TEXT ≥4.5，且都要在**最亮的承载面**（CARD）上量。
// C4 强调色：全屏高饱和像素 ≤1.5%；紫色只用于「你在这里」（当前帧/页签/工具）。
// C5 线：全宽 1px **亮**线 0 条；只允许比底色更暗的线（暗线不制造"格子"）。
// C6 圆角：只用 3 / 4 / 5 / 6 / 8 / 10 / 12 七档，同类控件必须同档。
// C7 间距：只用 4 / 8 / 12 / 16 / 24 五档。
// C8 字号：只用 11 / 12 / 13 / 14 / 16 / 20 六档，禁止字面量 FontId。
// C9 动效：全部 90–160ms；连续输入（缩放/平移/滑块/滚动）0 缓动；`shot=` 强制终态。
// C10 分层：每个元素必须能归入「框架 / 井 / 卡 / 控件」四层之一，底色必须来自 token。
//
// C1 luminance staircase (monotonic, step ≥1.5, well→shell ≥6.0).
// C2 Neutrals: true grey (R=G=B, |R−B| ≤ 2), the Photoshop palette -- no colour
// bias for judging exposure or white balance.
// C3 Text: T1 ≥7.0, others ≥4.5, measured on the lightest surface each tier lands on.
// C4 Accent: high-chroma pixels ≤1.5%; violet means "you are here", nothing else.
// C5 Zero full-width 1 px *light* rules; only darker-than-background seams.
// C6 Radii: only 3/4/5/6/8/10/12; one class of control uses one radius.
// C7 Spacing: only 4/8/12/16/24.
// C8 Type: only 11/12/13/14/16/20 px.
// C9 Motion: 90–160 ms everywhere; zero easing on continuous input; `shot=` is static.
// C10 Layering: every element belongs to frame / well / card / control, filled by a token.

use eframe::egui;
use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2};

// ---------------------------------------------------------------- 设计令牌
//
// 色相：**真中性灰**（R=G=B，Photoshop/Adobe 口径）。
//
// 之前做过一版暖灰（R−B 恒 +5…+8，LR/ACR 口径），用户实际对比 Photoshop 后
// 明确否决——暖棕在屏幕上读成"脏"，而且给照片的曝光/白平衡判读引入系统性
// 色偏。修图软件的界面灰必须真中性：界面零色偏，判读才可信。
//
// Hue: true neutral grey (R=G=B), the Photoshop/Adobe palette. A warm-grey pass
// was tried and rejected by the user -- warm brown reads as mud and biases
// exposure/white-balance judgement. A photo editor's chrome must be neutral.
//
// 层次（L*）：井 → 壳 → 侧栏 → 胶片 → 卡 → 按钮 → 悬停，单调递增，
// 井→壳必须拉开 6.0 以上（那是"凹进去"的全部依据），控件层之间 3.0 以上。
// 具体数值以 `design_audit::c1` 的断言为准，不在此写死。

/// 井：画布底部。全场最深，照片从它里面浮起来。
const CANVAS: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1A);
/// 壳：整窗底、顶栏、状态栏。
const CHROME: Color32 = Color32::from_rgb(0x2A, 0x2A, 0x2A);
/// 卡：缩略图底、按钮底、旗标选中底。
const CARD: Color32 = Color32::from_rgb(0x37, 0x37, 0x37);
/// 内凹：滑块轨道、数值框、直方图内容底。
///
/// 必须**比壳更暗**才叫"凹"。原来取了 0x201D19（比壳 0x1A1817 更亮），于是数值框
/// 和轨道读成"贴上去的小方块"；更实际的问题是它把上面那行 11px 刻度压到 3.7:1，
/// 破了 AA。改 0x161412 后：凹下去了，刻度也回到 4.9:1。
///
/// Must be **darker** than the shell to read as recessed. The old value was lighter,
/// so inputs looked glued on -- and it pushed the 11 px ticks under AA (3.7:1).
const FIELD: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
/// 按钮面：比卡再亮一级，用于可点的实心块。
const BUTTON: Color32 = Color32::from_rgb(0x42, 0x42, 0x42);

// ---- 文字：三阶文字 + 一阶装饰 ----
//
// 暗底上再往下造第四阶必然跌破 4.5:1。旧的 T4_TEXT(0x74) 只有 3.48:1，12px 的
// 时间码在 4K 下根本读不出来——那不是"淡雅"，是缺陷。
//
// Three text tiers plus one decoration tier. A fourth text tier on a dark ground
// always drops under 4.5:1; the old T4_TEXT sat at 3.48:1.
//
// 在壳 `#2A2A2A` 上的实测对比度以 `design_audit::c3` 断言为准：T1 ≥7 / 其余 ≥4.5。
const T1: Color32 = Color32::from_rgb(0xF5, 0xF5, 0xF5);
const T2: Color32 = Color32::from_rgb(0xD2, 0xD2, 0xD2);
const T3: Color32 = Color32::from_rgb(0xAC, 0xAC, 0xAC);
const T4: Color32 = Color32::from_rgb(0x7F, 0x7F, 0x7F);
/// 最弱一层的**文字**（刻度、时间码、状态栏、历史时间）：4.7:1，过 AA。
///
/// 装饰（星标、chevron、刻线）继续用 `T4`，只做形状不做阅读。
///
/// 取 0xA59D91 而不是更暗的值：它要同时满足"壳上 5.6"和**"卡片上 4.6"**——最弱
/// 一档文字会落在缩略图底（CARD）上，那一层比壳亮，按 0x9E 算只有 4.28，会漏过 AA。
///
/// 0xA59D91 rather than darker: this tier also lands on CARD (thumbnail fill), which
/// is lighter than the shell; at 0x9E it measured 4.28 there and would slip under AA.
///
/// Faintest **text** tier at 4.7:1 (AA). Decoration keeps `T4` at 2.9:1.
const T4_TEXT: Color32 = Color32::from_rgb(0xA1, 0xA1, 0xA1);
/// 色温轨道：蓝 → 中性 → 琥珀（设计稿 `--band-temp` 六段）。
const GRAD_TEMP: [Color32; 6] = [
    Color32::from_rgb(0x19, 0x39, 0x61),
    Color32::from_rgb(0x47, 0x56, 0x6A),
    Color32::from_rgb(0x60, 0x66, 0x6D),
    Color32::from_rgb(0x70, 0x70, 0x5B),
    Color32::from_rgb(0x73, 0x73, 0x39),
    Color32::from_rgb(0x75, 0x74, 0x2A),
];
/// 色调轨道：绿 → 中性 → 品红（设计稿 `--band-tint` 六段）。
const GRAD_TINT: [Color32; 6] = [
    Color32::from_rgb(0x24, 0x61, 0x23),
    Color32::from_rgb(0x2C, 0x55, 0x2C),
    Color32::from_rgb(0x39, 0x47, 0x39),
    Color32::from_rgb(0x4A, 0x3D, 0x4C),
    Color32::from_rgb(0x6B, 0x33, 0x6B),
    Color32::from_rgb(0x87, 0x2C, 0x87),
];
/// 强调色：从 `#8B5CF6` 提到 `#A78BFA`。
///
/// 旧紫在壳底上只有 **3.91:1**，作为 12px 的页签下划线和"当前帧"标记已经破线；
/// 提亮后在壳上是 5.5:1，仍只用于**一处**语义：当前在哪儿（当前帧 / 当前页签 /
/// 当前工具）。品牌色与状态色不再共用它。
///
/// Accent lifted from #8B5CF6 to #A78BFA: the old violet was 3.91:1 on the shell,
/// below AA for the 12 px underline it was used on. Reserved for "you are here".
const ACCENT: Color32 = Color32::from_rgb(0xA7, 0x8B, 0xFA);
// ACCENT_SOFT 已删：紫色收敛到"当前帧/当前页签/当前工具"后，不再有软底用途。
// 列表选中态改用 CARD 提亮（材质表达状态，不占颜色）。
const HAIRLINE: Color32 = Color32::from_rgb(0x64, 0x64, 0x64);
const HOVER: Color32 = Color32::from_rgba_premultiplied(50, 50, 50, 48);

// ---- 光照端点：从 token 派生的两级明度，不是新的颜色 ----
//
// 现代界面的"材质感"来自一个方向性的明暗过渡：光从左上打过来，平面顶部/左侧
// 稍亮、底部/右侧稍暗。这不是装饰，是让眼睛能读出"这是一个有厚度的物体"而非
// 一张色纸。两端都从既有 token 用 from_gray 式的等比缩放派生，色相不变，
// 所以 C2（中性）和 C4（彩色占比）都不会被破坏。
//
// Light endpoints derived from the same tokens -- a directional tonal ramp gives a
// surface thickness without introducing any new hue.

/// 把颜色按系数缩放（保色相、保中性，只改明度）。
fn shade(c: Color32, k: f32) -> Color32 {
    Color32::from_rgb(
        (c[0] as f32 * k).clamp(0.0, 255.0) as u8,
        (c[1] as f32 * k).clamp(0.0, 255.0) as u8,
        (c[2] as f32 * k).clamp(0.0, 255.0) as u8,
    )
}

/// 顶栏：上亮下暗，光从上方来。
const BAR_LIT: Color32 = Color32::from_rgb(0x2F, 0x2F, 0x2F);

// 井的光照端点由 `shade(CANVAS, …)` 在绘制处直接派生（×0.86 / ×1.17），
// 不再另设常量——保证 C1 的阶梯只有一个事实源。
// The well's light ends are derived from CANVAS at the call site: one source for
// the C1 staircase.

// ---------------------------------------------------------------- 桌面与浮板
//
// 2026 桌面软件的形态语法：控件**浮**在一张暗桌上，而不是贴边塞满窗口。
// 判据是缩略图测试——整图缩到 25% 仍能看出"面板浮在背景上"。
//
// 实现上只有三件事：① 窗口先铺一层比任何面板都暗的桌面色；② 每块面板内缩
// 8px、带圆角与柔影；③ 面板之间不再用贯穿全宽的 1px 线分隔，用间隙本身分隔。
//
// 2026 desktop grammar: controls float on a dark desk instead of being flush to
// the window edge. Three things only: a darker desk fill, 8 px insets with
// rounded corners and soft shadow, and gaps instead of full-width hairlines.

/// 顶栏高：够放一行 32px 控件 + 上下 6px。
pub const TOP_H: f32 = 44.0;
const BAR_H: f32 = TOP_H;
/// 状态栏高：macOS 口径的薄信息带，不是 Win 的 24px 工具条。
pub const STATUS_H: f32 = 20.0;
/// 胶片条底：比画布亮一档——它是要看的内容，不是井。
const STRIP: Color32 = Color32::from_rgb(0x32, 0x32, 0x32);
/// 侧栏底：比画布井亮、比顶栏暗——侧栏是框架层，夹在内容与控件之间。
const PANEL_SIDE: Color32 = Color32::from_rgb(0x2E, 0x2E, 0x2E);

/// 渲染倍率。默认 1.0 = 界面尺寸与原设计稿一致（4K/100% 缩放下字会偏软，
/// 因为 13px 字只有 13 个物理像素）；需要更清晰可设 `AEVUM_PROTO_ZOOM=2`，
/// 但**整个界面会等比放大** —— 清晰度与尺寸在 egui 里是绑定的，不能只改一个。
fn ui_zoom() -> f32 {
    std::env::var("AEVUM_PROTO_ZOOM")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|z| *z >= 1.0 && *z <= 2.0)
        .unwrap_or(1.0)
}
pub const RAIL_W: f32 = 52.0;
const LEFT_W: f32 = 232.0; // 浮层宽度（展开态），不再参与画布布局
/// 浮层高度：按内容定 =「头 40 + 预设 6+26+168 + 历史 6+26+10 行 360 + 注脚 30 + 页脚 48」，
/// 让历史恰好 10 条收尾，而不是拉成一整条侧栏——按需面板要和常驻侧栏看起来不同。
///
/// Float height, sized so history lands on 10 rows: an on-demand panel should not
/// read as a full-height column.
const LEFT_H: f32 = 710.0;
pub const DOCK_W: f32 = 296.0;
/// 胶片条高：186 → 164。浮动后上下各多出 8px 间隙，再保持 186 会把画布压到 810 以下；
/// 164 仍是「filmhead 32 + 缩略图 48 + 关键帧 18 + 亮度 21 + 刻度 13 + 内距」的整数和。
///
/// Strip height 186 -> 164: the floating layout adds 16 px of gaps, and 164 keeps
/// the five lanes intact.
pub const FILM_H: f32 = 164.0;
const DOCK_TABS_H: f32 = 40.0;
/// 演示照片的像素尺寸。顶栏「画幅」读数与缩放的百分比都从这里算，
/// 两处写死两个数就会互相矛盾。
///
/// Source photo size; the frame readout and the zoom percentage both derive
/// from it, so the two can never disagree.
const PHOTO_W: f32 = 1280.0;
const PHOTO_H: f32 = 720.0;

/// 与 `widgets::BAND_NAMES` / `BAND_HUES` 同一张表。
const BAND_NAMES: [&str; 8] = ["红", "橙", "黄", "绿", "浅绿", "蓝", "紫", "洋红"];
const BAND_HUES: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 285.0, 330.0];

const PROP_NAMES: [&str; 4] = ["色相", "饱和度", "明亮度", "全部"];

// ---------------------------------------------------------------- 字号与字重

// 字号与字重**照抄源程序的排版语言**（`crates/aevum-ui/src/shell/*.rs` 的实测口径），
// 不照抄 HTML 的 12/13px 一律常规体：
//
//   正文 / 文档名 / 菜单      14px  · 菜单与品牌用 bold 13（`menu.rs` 原样）
//   参数标签 / 数值框 / 预设名 13px  · regular（`common.rs` 的 `SIZE_LABEL`）
//   分组标题 / 分区标题        13px  · **bold**（`common.rs` `bold(SIZE_LABEL)`）
//   页签（含激活态）           12px  · 激活 bold（`dock.rs` `bold(SIZE_SMALL)`）
//   时间码 / 缩略图序号 / 状态栏 12px · regular
//
// 之前原型全篇一律 12–13px 常规体，缺了「靠字重分层」这一步：同样的字号下，
// 笔画只有一像素粗时灰度 AA 会把它摊成两行各 50% 灰，整段读起来就是「虚」。
// 源程序之所以锐，一半在 14px，另一半在 **bold 标题**——粗一档，笔画就真的压在
// 一个像素上，边缘不再发灰。
//
// Sizes and weights copy the main app's typesetting (measured in
// `crates/aevum-ui/src/shell/*.rs`), not the HTML's uniform 12-13 px regular:
// body 14, parameter labels 13, section headings 13 **bold**, tabs 12 with the
// active one bold, micro text 12. Half of why the app looks sharp is the 14 px
// body; the other half is the bold headings -- one weight up puts the stem on a
// single pixel row instead of smearing it 50/50 across two.
const FONT_LABEL: f32 = crate::theme::SIZE_LABEL;
const FONT_SMALL: f32 = crate::theme::SIZE_SMALL;
const FONT_TITLE: f32 = crate::theme::SIZE_HEADING;
/// 键位徽标 / 时间码这一档（设计稿 `--fs-meta`，theme 里没有对应常量）。
const FONT_META: f32 = 11.0;

/// 参数标签、数值框、预设名、历史标题。
/// 悬停底色的 90ms 渐显。
///
/// egui 是立即模式，没有 CSS transition；`animate_bool_with_time` 是唯一的官方途径，
/// 传入稳定 `Id` 就得到一条独立时间线。90ms 是"感觉得到但不拖"的数：更快看不出是
/// 过渡，更慢会像在等。
///
/// 90 ms hover fade. Immediate mode has no CSS transition; `animate_bool_with_time`
/// with a stable id is the supported path.
fn hover_bg(ui: &egui::Ui, id: egui::Id, on: bool, rect: Rect, radius: f32, base: Color32) {
    let t = ui.ctx().animate_bool_with_time(id, on, 0.09);
    if t > 0.004 {
        ui.painter()
            .rect_filled(rect, radius, base.gamma_multiply(t));
    }
}

fn f_label() -> egui::FontId {
    egui::FontId::proportional(FONT_LABEL)
}
/// 读数专用等宽档：滑块数值、时间码、帧号、直方图计数。
///
/// egui 0.37 没有 OpenType `tnum`（等宽数字），比例字体下 `+6` 与 `+10` 的宽度不同，
/// 拖滑块时数字会左右跳 3–5px——那是最高频的交互，跳一下就"廉价"。等宽档只能用
/// `FontId::monospace`，而内置 mono 家族（Hack）没有中文字形，所以**这一档只放数字
/// 与拉丁符号，不放中文**。
///
/// Readout tier in monospace: proportional digits make slider values jump 3-5 px while
/// dragging. The built-in mono family has no CJK glyphs -- never put Chinese here.
fn f_readout() -> egui::FontId {
    egui::FontId::monospace(FONT_LABEL)
}
/// 时间码、缩略图序号、状态栏等最小一档。
fn f_small() -> egui::FontId {
    egui::FontId::proportional(FONT_SMALL)
}
/// 胶片条计数这类唯一的大字。
fn f_title() -> egui::FontId {
    egui::FontId::proportional(FONT_TITLE)
}
/// 粗体：品牌、菜单、分区标题、分组标题（源程序 `theme::bold`）。
fn f_bold(px: f32) -> egui::FontId {
    crate::theme::bold(px)
}
/// 分组标题 = 13px 粗体，与参数标签同字号、靠字重分层。
fn f_group() -> egui::FontId {
    f_bold(FONT_LABEL)
}
/// 页签：12px，激活态加粗（源程序 `dock.rs` 口径）。
fn f_tab(active: bool) -> egui::FontId {
    if active {
        f_bold(FONT_SMALL)
    } else {
        egui::FontId::proportional(FONT_SMALL)
    }
}

/// 量一段文字的实际宽度。
///
/// 顶栏那几组读数过去用 `字数 × 每字宽` 估算排布，字号或字重一变就压字——
/// 中文每字 12px、拉丁数字约 6.6px，估算式必然在某个字符组合上错位。源程序
/// `menu.rs::fit_text` 也是先 `layout_no_wrap` 再按实测宽度排。
///
/// Measure a run of text: the top bar used to lay out readouts as
/// `chars * per-char-width`, which overlaps the moment size or weight changes.
/// The app does the same thing -- lay it out first, then use the measured width.
fn text_w(p: &egui::Painter, text: &str, font: egui::FontId) -> f32 {
    p.layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x
}

// ---------------------------------------------------------------- 状态

/// demo 界面 → 真应用的动作（不捕获借用；桥接每帧 drain 后调用真 API）。
#[derive(Clone, Copy, Debug)]
pub(super) enum DemoAction {
    OpenFolder,
    OpenPhotos,
    Export,
    Undo,
    ApplyPreset(usize),
    AutoExposure,
    ResetParams,
    SetFrame(usize),
    /// 模块切换：真 API（state.module），非 Develop 模块的界面在后续构建。
    /// Module switch via the real API; non-Develop UIs land in a later build.
    SelectModule(crate::state::Module),
    /// 星级：写到 state.ratings（当前帧）。
    /// Star rating written to state.ratings for the current frame.
    SetRating(u8),
    /// 关键帧开关：当前帧有则删、无则以当前显影参数建。
    /// Keyframe toggle for the current frame: remove, or insert with the
    /// current develop params.
    ToggleKeyframe,
}

/// demo 侧的动作队列（桥接注入；demo 独立运行时为 None，保留"原型未接"提示）。
#[derive(Clone)]
struct DemoActions {
    queue: std::sync::Arc<std::sync::Mutex<Vec<DemoAction>>>,
}

impl DemoActions {
    fn send(&self, a: DemoAction) {
        if let Ok(mut q) = self.queue.lock() {
            q.push(a);
        }
    }
}

struct Proto {
    photo: Option<egui::TextureHandle>,
    dock_tab: usize,
    mixer_tab: usize,
    prop: usize,
    hsl_h: [f64; 8],
    hsl_s: [f64; 8],
    hsl_l: [f64; 8],
    mono: [f64; 8],
    wb_temp: f64,
    wb_tint: f64,
    basic: [f64; 6],
    effects: [f64; 3],
    presence: [f64; 5],
    deflicker: f64,
    smooth_win: f64,
    preset: usize,
    hist: usize,
    /// 左栏是否展开。收起后这 204px 全部还给画布 —— 照片优先时用户的第一动作。
    /// 设计稿备了 `.ltoggle`（面板头 ‹）与 `.railtab`（画布左缘竖标签）一对控件。
    ///
    /// Whether the left panel is open. Collapsing hands its 204 px back to the
    /// canvas — the first move when the photo matters more than the lists.
    left_open: bool,
    /// 预设分组是否展开（设计稿 `h4 .chev`）。
    presets_open: bool,
    /// 最近 30 帧的帧时间（ms）。状态栏显示**中位数**：瞬时 `stable_dt` 会把首帧
    /// 的初始化开销（70ms+）当成常态，装成"这软件很卡"。
    ///
    /// Last 30 frame times (ms). The status bar shows the **median**: a single
    /// `stable_dt` sample reports first-frame init cost as if it were steady state.
    dt_hist: [f32; 30],
    dt_n: usize,
    film: usize,
    zoom: f32,
    pan: Vec2,
    dock_scroll: f32,
    toast: Option<(String, f64)>,
    /// 任务队列长度（顶栏任务胶囊，桥接每帧同步）。
    /// Task-queue length for the top-bar pill, synced by the bridge every frame.
    tasks: usize,
    /// 当前帧的星级（filmhead 星标，桥接同步 + 点击回写）。
    /// Current frame's star rating: filmhead stars, synced + written back on click.
    rating: u8,
    /// 有关键帧的帧号集合（胶片条菱形车道，桥接同步）。
    /// Frames that carry keyframes: the diamond lane, synced by the bridge.
    kfs: Vec<usize>,
    /// 当前模块（顶栏 modpick 显示 + 下拉菜单高亮，桥接同步）。
    /// Current module for the top-bar picker label and dropdown highlight.
    module: crate::state::Module,
    /// `shot=<path>`：第 3 帧整窗截图后退出。
    /// 锁屏环境无法注入鼠标/键盘，像素级核对只能靠这条内置通道。
    shot: Option<String>,
    frames: u32,
    /// AI 命令条状态：false=待命（回显上一条指令），true=执行中（细进度线）。
    /// 2026 趋势（AI 信任态 / prompt bar）：AI 的"在場感"必须可见、可预期，
    /// 但紫色只许出现在那枚 6px 的在場点上（C4：紫=你在这里，Agent 是工具）。
    ///
    /// AI command bar state: false = idle (echoes the last instruction),
    /// true = running (thin progress line). The 2026 "AI trust state" trend --
    /// the agent's presence must be visible and predictable, with violet only
    /// on its 6 px presence dot (C4: violet = "you are here", the agent is a tool).
    ai_busy: bool,
    /// 真应用缩略图源（桥接注入；demo 独立运行时为 None）。
    thumb_source:
        Option<std::sync::Arc<dyn Fn(usize) -> Option<egui::TextureHandle> + Send + Sync>>,
    actions: Option<std::sync::Arc<DemoActions>>,
}

impl Proto {
    fn say(&mut self, ctx: &egui::Context, msg: &str) {
        let t = ctx.input(|i| i.time);
        self.toast = Some((msg.to_string(), t));
    }

    /// `shot=<path>` 的实现：布局稳定后请求一帧截图，落盘即关窗。
    ///
    /// Implements `shot=<path>`: once layout is settled, ask for one frame capture,
    /// write the PNG, and close the window.
    fn drive_shot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot.clone() else {
            return;
        };
        self.frames += 1;
        // 前两帧让 Panel 完成布局、字体图集完成上传；第 3 帧才请求截图。
        if self.frames != 3 {
            return;
        }
        println!(
            "shot: ppp={:.3} screen={:?} zoom_env={}",
            ctx.pixels_per_point(),
            ctx.input(|i| i.viewport().inner_rect.map(|r| r.size())),
            ui_zoom()
        );
        let ctx = ctx.clone();
        let done = ctx.clone();
        ctx.request_screenshot(move |img| {
            let (w, h) = (img.size[0] as u32, img.size[1] as u32);
            match image::RgbaImage::from_raw(w, h, img.as_raw().to_vec()) {
                Some(rgba) => match rgba.save(&path) {
                    Ok(()) => println!("shot: {path} {w}x{h}"),
                    Err(e) => eprintln!("shot: 保存失败 {path}: {e}"),
                },
                None => eprintln!("shot: 像素缓冲与尺寸不符"),
            }
            done.send_viewport_cmd(egui::ViewportCommand::Close);
        });
    }
}

/// 把文本原点对齐到整数像素：egui 用 `Align2` 算出的基线常落在 `.5` 上
/// （`LEFT_CENTER` 走 `y - galley.height()/2`，高度为奇数时正好差半像素），
/// 字形被采样在两行之间，笔画两侧都发灰 —— 这就是「字体看着模糊」。
///
/// Round the text origin to whole pixels: egui's `Align2` math routinely lands on a
/// half pixel (an odd galley height makes `y - h/2` a `.5` value), the glyph is then
/// sampled between two pixel rows and every stroke grows grey fringes.
trait TxtExt {
    fn txt(
        &self,
        pos: Pos2,
        anchor: egui::Align2,
        text: impl ToString,
        font: egui::FontId,
        color: Color32,
    );
}

impl TxtExt for egui::Painter {
    fn txt(
        &self,
        pos: Pos2,
        anchor: egui::Align2,
        text: impl ToString,
        font: egui::FontId,
        color: Color32,
    ) {
        self.text(pos.round(), anchor, text, font, color);
    }
}

// ---------------------------------------------------------------- 颜色工具

fn hsl_color(h: f32, s: f32, l: f32) -> Color32 {
    let s = s / 100.0;
    let l = l / 100.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Color32::from_rgb(
        ((r + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((g + m) * 255.0).round().clamp(0.0, 255.0) as u8,
        ((b + m) * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// 频段轨道端点色，与 `shell/common.rs::hsl_band_stops` 同口径。
fn band_stops(band: usize, prop: usize) -> (Color32, Color32) {
    let h = BAND_HUES[band];
    let c = hsl_color(h, 82.0, 55.0);
    match prop {
        0 => (c, hsl_color((h + 180.0) % 360.0, 82.0, 55.0)),
        1 => (hsl_color(h, 0.0, 55.0), c),
        _ => (hsl_color(h, 82.0, 4.0), c),
    }
}

/// 横向线性渐变，支持任意多个停靠点（色温/色调轨道是六段色，只给两个端点会
/// 变成一条突兀的蓝→黄硬过渡）。
///
/// Horizontal gradient with arbitrary stop count: the temperature/tint rails are
/// six-stop ramps; two endpoints would turn them into a hard blue-to-yellow step.
fn grad_rect(p: &egui::Painter, rect: Rect, stops: &[Color32]) {
    if stops.len() < 2 {
        if let Some(c) = stops.first() {
            p.rect_filled(rect, 0.0, *c);
        }
        return;
    }
    let segs = (stops.len() - 1) as f32;
    let mut m = egui::Mesh::default();
    for (i, pair) in stops.windows(2).enumerate() {
        let x0 = rect.left() + rect.width() * (i as f32 / segs);
        let x1 = rect.left() + rect.width() * ((i + 1) as f32 / segs);
        let idx = m.vertices.len() as u32;
        m.colored_vertex(Pos2::new(x0, rect.top()), pair[0]);
        m.colored_vertex(Pos2::new(x1, rect.top()), pair[1]);
        m.colored_vertex(Pos2::new(x1, rect.bottom()), pair[1]);
        m.colored_vertex(Pos2::new(x0, rect.bottom()), pair[0]);
        m.add_triangle(idx, idx + 1, idx + 2);
        m.add_triangle(idx, idx + 2, idx + 3);
    }
    p.add(egui::Shape::mesh(m));
}

/// 圆角矩形的**线性渐变填充**。epaint 的 `rect_filled` 只吃纯色，但现代界面的
/// 平面不该是死色：给大面一个方向性的明暗过渡，它才有"被光照着"的质感，而不
/// 是贴了一张色纸。
///
/// 实现：把圆角轮廓Sample成点，用中心扇形剖分成三角形；每个顶点颜色按它自己的 y
/// （或 x）从渐变取值。因为重心坐标对位置是线性的，三角形内部插值出来的颜色恰好
/// 等于该点 y 对应的渐变色——不是廉价的分段色带。
///
/// Rounded-rect gradient fill. A triangle fan over the rounded outline with each
/// vertex coloured by its own coordinate gives an exact linear gradient (barycentric
/// interpolation of a linear function is still linear).
fn rounded_grad(
    p: &egui::Painter,
    rect: Rect,
    radius: f32,
    c0: Color32,
    c1: Color32,
    horizontal: bool,
) {
    let lerp = |a: u8, b: u8, t: f32| -> u8 { (a as f32 * (1.0 - t) + b as f32 * t) as u8 };
    let color_at = |pt: Pos2| -> Color32 {
        let t = if horizontal {
            ((pt.x - rect.left()) / rect.width()).clamp(0.0, 1.0)
        } else {
            ((pt.y - rect.top()) / rect.height()).clamp(0.0, 1.0)
        };
        Color32::from_rgba_premultiplied(
            lerp(c0[0], c1[0], t),
            lerp(c0[1], c1[1], t),
            lerp(c0[2], c1[2], t),
            255,
        )
    };

    let r = radius.min(rect.width() * 0.5).min(rect.height() * 0.5);
    let center = rect.center();
    let mut m = egui::Mesh::default();
    let cidx = m.vertices.len() as u32;
    m.colored_vertex(center, color_at(center));

    // 圆角矩形轮廓，从右上角顺时针；每个角用 5 段折线近似圆弧。
    let steps = 5;
    let mut pts: Vec<Pos2> = Vec::with_capacity(4 * (steps + 1));
    let arcs = [
        (Pos2::new(rect.right() - r, rect.top() + r), -90.0),
        (Pos2::new(rect.right() - r, rect.bottom() - r), 0.0),
        (Pos2::new(rect.left() + r, rect.bottom() - r), 90.0),
        (Pos2::new(rect.left() + r, rect.top() + r), 180.0),
    ];
    for (center_arc, start_deg) in arcs {
        for i in 0..=steps {
            let a = (start_deg + i as f32 * 90.0 / steps as f32).to_radians();
            pts.push(Pos2::new(
                center_arc.x + r * a.cos(),
                center_arc.y + r * a.sin(),
            ));
        }
    }
    pts.push(pts[0]);
    for pair in pts.windows(2) {
        let i0 = m.vertices.len() as u32;
        m.colored_vertex(pair[0], color_at(pair[0]));
        m.colored_vertex(pair[1], color_at(pair[1]));
        m.add_triangle(cidx, i0, i0 + 1);
    }
    p.add(egui::Shape::mesh(m));
}

/// 竖向两段渐变（上 → 下）。「晷针」的光柱用它：顶端实、底端散。
///
/// Vertical two-stop gradient, used by the gnomon beam: solid at the top,
/// fading toward the base.
fn vgrad_rect(p: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(Pos2::new(rect.left(), rect.top()), top);
    m.colored_vertex(Pos2::new(rect.right(), rect.top()), top);
    m.colored_vertex(Pos2::new(rect.right(), rect.bottom()), bottom);
    m.colored_vertex(Pos2::new(rect.left(), rect.bottom()), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(m));
}

fn fmt_val(v: f64, digits: usize) -> String {
    if digits == 0 {
        if v.abs() < 0.5 {
            "0".into()
        } else if v < 0.0 {
            format!("−{}", v.abs().round() as i32)
        } else {
            format!("+{}", v.round() as i32)
        }
    } else {
        format!("{:.digits$}", v, digits = digits)
    }
}

// ---------------------------------------------------------------- 通用控件

/// 一排下划线 tab，返回被点的索引。
/// 页签的两种口径，**不能混用**（设计稿写得不一样）：
///
/// - `Dock`：`.docktab.on` —— 下划线 `left/right: 10px`（内缩 10）、激活 `font-weight:500`；
/// - `Mixer`：`.mtabs span.on` —— 下划线是整列的 `inset shadow`（不内缩）。
///
/// 字重上一律「激活加粗」：设计稿的 `.mtabs span.on` 只有 t3→t1 的色差，在 0x20 底色
/// 上约 1.4:1，12px 下几乎看不出选中；加粗才是一眼可辨的激活态。这是刻意的偏离。
///
/// Two tab flavours, deliberately different in the mockup: `Dock` insets the underline
/// by 10 px, `Mixer` spans the whole column. Both bold the active label -- the mockup's
/// mixer only shifts t3 to t1, which is about 1.4:1 on the 0x20 background and all but
/// invisible at 12 px. Bolding is a deliberate deviation.
#[derive(Clone, Copy, PartialEq)]
enum TabStyle {
    Dock,
    Mixer,
}

fn tab_row(
    p: &egui::Painter,
    ui: &mut egui::Ui,
    rect: Rect,
    labels: &[&str],
    cur: usize,
    key: &'static str,
    style: TabStyle,
    instant: bool,
) -> Option<usize> {
    let mut hit = None;
    let w = rect.width() / labels.len() as f32;
    for (i, label) in labels.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(rect.left() + i as f32 * w, rect.top()),
            Vec2::new(w, rect.height()),
        );
        let resp = ui.interact(r, ui.scope_id().with((key, i)), Sense::click());
        if resp.hovered() {
            p.rect_filled(r, 4.0, HOVER);
        }
        if resp.clicked() {
            hit = Some(i);
        }
        let font = match style {
            TabStyle::Dock => f_tab(i == cur),
            TabStyle::Mixer if i == cur => f_bold(FONT_SMALL),
            TabStyle::Mixer => egui::FontId::proportional(FONT_SMALL),
        };
        p.txt(
            r.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            font,
            if i == cur { T1 } else { T3 },
        );
    }
    // 下划线在循环外统一画，中心 x 走 120ms 插值：切换页签时它**滑过去**，不是瞬移。
    //
    // `animate_value_with_time` 会从上一个目标值平滑到新目标，正是需要的行为；
    // `instant` 用于截图模式——否则第 3 帧抓到的是一条停在半路的线。
    //
    // The underline is drawn once outside the loop and its centre is animated over
    // 120 ms, so it slides between tabs instead of teleporting.
    let uw = match style {
        TabStyle::Dock => w - 20.0,
        TabStyle::Mixer => w,
    }
    .max(10.0);
    let target = rect.left() + (cur as f32 + 0.5) * w;
    let cx = if instant {
        target
    } else {
        ui.ctx()
            .animate_value_with_time(ui.scope_id().with((key, "line")), target, 0.12)
    };
    p.rect_filled(
        Rect::from_min_size(
            Pos2::new(cx - uw / 2.0, rect.bottom() - 2.0),
            Vec2::new(uw, 2.0),
        ),
        1.0,
        ACCENT,
    );
    hit
}

/// 画一个 V 形箭头（字体里 ▾ 是缺字形，会显示成方块）。
fn chevron(p: &egui::Painter, c: Pos2, w: f32, color: Color32) {
    let h = w * 0.5;
    p.line_segment(
        [
            Pos2::new(c.x - w / 2.0, c.y - h / 2.0),
            Pos2::new(c.x, c.y + h / 2.0),
        ],
        Stroke::new(1.4, color),
    );
    p.line_segment(
        [
            Pos2::new(c.x, c.y + h / 2.0),
            Pos2::new(c.x + w / 2.0, c.y - h / 2.0),
        ],
        Stroke::new(1.4, color),
    );
}

/// 顶栏竖分隔（设计稿 `.menubar .vsep`：1×20，rgba(0,0,0,.42)）。
///
/// 顶栏有五组东西（工具 / 自动复位 / 整段单帧 / 徽标 / 读数），没有分隔线时
/// 全读成一串——这是「顶栏看着乱」的第一原因。
///
/// Vertical separator (mockup `.menubar .vsep`: 1x20, rgba(0,0,0,.42)). The bar has
/// five groups; without separators the whole row reads as one undifferentiated run.
fn vsep(p: &egui::Painter, x: f32, cy: f32) {
    p.vline(
        x,
        (cy - 10.0)..=(cy + 10.0),
        Stroke::new(1.0, Color32::from_black_alpha(107)),
    );
}

/// 键位徽标（设计稿 `.kbd`：11px、t3、--btn 底、1px 边、内距 1×5），返回宽度。
fn kbd_chip(p: &egui::Painter, x: f32, cy: f32, text: &str) -> f32 {
    let font = egui::FontId::proportional(FONT_META);
    let w = text_w(p, text, font.clone()) + 10.0;
    let r = Rect::from_min_size(Pos2::new(x, cy - 8.5), Vec2::new(w, 17.0));
    p.rect_filled(r, 3.0, BUTTON);
    p.rect_stroke(
        r,
        3.0,
        Stroke::new(1.0, Color32::from_rgb(0x56, 0x56, 0x56)),
        egui::StrokeKind::Inside,
    );
    p.txt(r.center(), egui::Align2::CENTER_CENTER, text, font, T3);
    w
}

/// 直方图：三通道叠加（确定性数据，形状接近真实夜景照片）。
fn draw_histogram(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect) {
    // 仪表底：内凹（比右坞暗）+ 竖向光照。端点从 STRIP 派生——底色保持真中性，
    // 叠上 RGB 通道色后不会出现"暖底 + 冷蓝"的脏混色；通道色相保留，浓度已压低。
    //
    // Derive the gauge fill from STRIP so the base stays neutral: channel overlays
    // blend cleanly instead of muddying against a tinted ground.
    rounded_grad(p, rect, 8.0, shade(STRIP, 0.62), shade(STRIP, 0.86), false);

    // 分区网格与 50% 中线整组删除：直方图是波形不是仪表盘，三道竖线把
    // 一条连续分布切成三格，视觉审查判定为"负效果"。波峰高低本身已足够
    // 读出曝光倾向，不需要参考线教人看图。
    //
    // The zone grid and midline are gone: a histogram is a distribution, not a
    // gauge -- three verticals sliced it into cells the review called a net
    // negative. Peak height alone already reads the exposure bias.

    let cols = 96usize;
    let cw = rect.width() / cols as f32;
    // 三通道权重差异制造"照片感"。alpha 70 → 46：70 的蓝通道把底色大片压成
    // 冷灰（审查实测合成色 #576365，R−B = −14）；46 让三通道只给波形上色，
    // 合成后仍回到底色的中性（RGB 通道名色相保留，只是浓度降）。
    //
    // Channel alpha 70 → 46: at 70 the blue channel dragged whole regions cool
    // (#576365, R−B = −14). At 46 the channels tint the waveform while the
    // composite settles back into the warm fill.
    let w = [(1.0f32, 0.92f32), (1.06f32, 1.0f32), (0.86f32, 1.14f32)];
    for ch in 0..3 {
        let col = match ch {
            0 => Color32::from_rgba_unmultiplied(0xE0, 0x6A, 0x5C, 46),
            1 => Color32::from_rgba_unmultiplied(0x6C, 0xC0, 0x6A, 46),
            _ => Color32::from_rgba_unmultiplied(0x5C, 0x8A, 0xE0, 46),
        };
        for i in 0..cols {
            let t = i as f32 / cols as f32;
            // 双峰 + 高光尾巴
            let a = (-((t - 0.28) * 9.0).powi(2)).exp();
            let b = 0.72 * (-((t - 0.62) * 7.0).powi(2)).exp();
            let tail = 0.22 * (-((t - 0.90) * 12.0).powi(2)).exp();
            let mut v = (a + b + tail) * w[ch].1;
            v = (v * (0.9 + 0.1 * ((i * 37 % 11) as f32 / 11.0))).clamp(0.0, 1.0);
            let h = v * (rect.height() - 8.0);
            p.rect_filled(
                Rect::from_min_size(
                    Pos2::new(rect.left() + i as f32 * cw, rect.bottom() - 4.0 - h),
                    Vec2::new(cw.max(1.0), h),
                ),
                0.0,
                col,
            );
        }
    }
    // 悬停读数：竖线 + 该列合成分布百分比。波形从"装饰"变成"可查询的
    // 仪表"——2026 数据可视化口径：图要能回答问题，不只给形状。
    //
    // Hover readout: a vertical hairline + the composite distribution value at
    // that column. Turns the waveform from decoration into a queryable gauge.
    let hresp = ui.interact(rect, ui.scope_id().with("hist_hover"), Sense::hover());
    if let Some(pos) = hresp.interact_pointer_pos() {
        if rect.contains(pos) {
            let t = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let v = {
                let f = |t: f32| {
                    let a = (-((t - 0.28) * 9.0).powi(2)).exp();
                    let b = 0.72 * (-((t - 0.62) * 7.0).powi(2)).exp();
                    let tail = 0.22 * (-((t - 0.90) * 12.0).powi(2)).exp();
                    (a + b + tail).clamp(0.0, 1.2)
                };
                f(t) / f(0.28).max(f(0.62))
            };
            let lx = pos.x.round();
            p.line_segment(
                [
                    Pos2::new(lx, rect.top() + 6.0),
                    Pos2::new(lx, rect.bottom() - 4.0),
                ],
                Stroke::new(1.0, HAIRLINE),
            );
            p.txt(
                Pos2::new(lx + 6.0, rect.top() + 10.0),
                egui::Align2::LEFT_CENTER,
                format!("{:.0}%", v * 100.0),
                egui::FontId::proportional(FONT_META),
                T1,
            );
        }
    }
    // 裁切指示三角（左上黑场 / 右上白场）：两个必须同向。
    // 之前左边朝上、右边朝下，看上去像两个互不相干的控件（视觉审查直接点名）。
    //
    // Clip indicators at both top corners; they must point the same way. They used to
    // point up on the left and down on the right -- they read as two unrelated widgets.
    for x in [rect.left() + 8.0, rect.right() - 8.0] {
        let y = rect.top() + 5.0;
        p.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(x - 5.0, y + 8.0),
                Pos2::new(x + 5.0, y + 8.0),
                Pos2::new(x, y),
            ],
            T4,
            Stroke::NONE,
        ));
    }
}

fn group_head(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    rect: Rect,
    y: f32,
    title: &str,
    side: &str,
    st: &mut Proto,
) -> f32 {
    let cy = y + 16.0;
    // 整行是一个可 hover 的控件：底衬在鼠标下浮现，chevron 与右操作同步提亮。
    // 之前它是"三个文字躺在空白里"，读不出"这一行可以按"。
    //
    // The whole header row is now a hoverable control: the tile appears under the
    // cursor and chevron / side action brighten with it.
    let row = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), 32.0));
    let resp = ui.interact(row, ui.scope_id().with(("grp", title)), Sense::click());
    // 折叠态存 demo 会话（桥接模式同样生效）；不再只是 toast。
    let gid = egui::Id::unique(("v21-group-fold", title));
    let folded = ui.data(|d| d.get_temp::<bool>(gid)).unwrap_or(false);
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(gid, !folded));
        // 侧按钮命中区独立于整行：自动 / 复位走真应用。
        let side_rect = Rect::from_min_size(
            Pos2::new(row.right() - 52.0, row.top() + 3.0),
            Vec2::new(48.0, row.height() - 6.0),
        );
        let side_resp = ui.interact(
            side_rect,
            ui.scope_id().with(("grp-side", title)),
            Sense::click(),
        );
        if side_resp.clicked() {
            if let Some(a) = &st.actions {
                match side {
                    "自动" => a.send(DemoAction::AutoExposure),
                    "复位" => a.send(DemoAction::ResetParams),
                    _ => {}
                }
            }
        }
    }
    hover_bg(
        ui,
        ui.scope_id().with(("h_grp", title)),
        resp.hovered(),
        row,
        6.0,
        BUTTON,
    );
    let col = if resp.hovered() { T1 } else { T3 };
    chevron(p, Pos2::new(rect.left() + 16.0, cy), 7.0, col);
    p.txt(
        Pos2::new(rect.left() + 32.0, cy),
        egui::Align2::LEFT_CENTER,
        title,
        f_group(),
        T1,
    );
    if !side.is_empty() {
        p.txt(
            Pos2::new(rect.right() - 16.0, cy), // C7
            egui::Align2::RIGHT_CENTER,
            crate::i18n::tr(side),
            f_small(),
            col,
        );
    }
    y + 34.0
}

/// 滑块行 —— **全应用唯一实现**，行高 44。
///
/// 几何（与效果图 `.srow` 一致）：
/// 标签 left12/top5/12px · 数值框 50×20 右上(right62,top4) · 轨道 left12/right12/top28/h3 ·
/// 滑钮 11px 空心环 2px 边 · 轨道在滑钮处开 14×7 孔。
///
/// `grad = Some((a, b))` 时轨道画成 a→b 的线性渐变（颜色混合器用），
/// 否则是「深灰底 + 滑钮左侧白色渐变填充」（普通滑块）。
/// 两条路径共用同一份几何，避免「两种滑块长得不一样」。
#[allow(clippy::too_many_arguments)]
fn slider_row(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    rect: Rect,
    y: f32,
    key: &str,
    label: &str,
    v: &mut f64,
    min: f64,
    max: f64,
    def: f64,
    digits: usize,
    grad: Option<&[Color32]>,
) -> f32 {
    let row = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), 44.0));
    let resp = ui.interact(
        row,
        ui.scope_id().with((key, label)),
        Sense::click_and_drag(),
    );
    hover_bg(
        ui,
        ui.scope_id().with(("h_row", key, label)),
        resp.hovered(),
        row,
        6.0,
        HOVER,
    );
    // 标签
    p.txt(
        Pos2::new(row.left() + 12.0, row.top() + 5.0),
        egui::Align2::LEFT_TOP,
        label,
        f_label(),
        T2,
    );
    // 数值框 50×20（右上、与轨道分离）。
    //
    // 之前是「深色方块 + 1px 亮描边」——描边取代了材质，这是"素"的机械来源。
    // 现在用**真实的内凹**：底比周围暗（FIELD），顶内沿压一道黑影（光从上方被
    // 唇边挡住），底内沿留一道 6% 白（光反射到唇边下沿）。不画外描边。
    //
    // Recessed value field: a dark floor plus a top inner shadow and bottom inner
    // light rim. No outline -- the bevel itself carries the depth.
    let field = Rect::from_min_size(
        Pos2::new(row.right() - 62.0, row.top() + 4.0),
        Vec2::new(50.0, 20.0),
    );
    p.rect_filled(field, 4.0, FIELD);
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(field.left() + 1.0, field.top() + 1.0),
            Pos2::new(field.right() - 1.0, field.top() + 3.0),
        ),
        0.0,
        Color32::from_black_alpha(if resp.hovered() { 130 } else { 96 }),
    );
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(field.left() + 2.0, field.bottom() - 2.0),
            Pos2::new(field.right() - 2.0, field.bottom() - 1.0),
        ),
        0.0,
        Color32::from_white_alpha(18),
    );
    p.txt(
        field.center(),
        egui::Align2::CENTER_CENTER,
        fmt_val(*v, digits),
        f_readout(),
        if resp.hovered() { T1 } else { T2 },
    );

    // 轨道
    // 轨道右端在数值框左缘前 8px 截止：之前它一路画到面板右缘、从数值框底下
    // 穿过去，放大后是"上面一个框、下面一条带"的双层边。现在两者左右分开。
    //
    // The rail stops before the value field: drawing it underneath the field made
    // a doubled edge (field on top, gradient band below all the way to the edge).
    let rail = Rect::from_min_size(
        Pos2::new(row.left() + 12.0, row.top() + 28.0),
        Vec2::new((field.left() - 8.0 - row.left() - 12.0).max(24.0), 4.0),
    );
    let t = ((*v - min) / (max - min)).clamp(0.0, 1.0) as f32;
    let cx = rail.left() + rail.width() * t;
    match grad {
        Some(stops) => grad_rect(p, rail, stops),
        None => {
            p.rect_filled(rail, 0.0, Color32::from_rgb(0x50, 0x50, 0x50)); // C6: 细轨道方角
            if cx > rail.left() + 1.0 {
                grad_rect(
                    p,
                    Rect::from_min_max(rail.min, Pos2::new(cx, rail.bottom())),
                    &[Color32::from_white_alpha(76), Color32::from_white_alpha(20)],
                );
            }
        }
    }
    // 开孔 14×7
    p.rect_filled(
        Rect::from_min_size(Pos2::new(cx - 7.0, row.top() + 26.0), Vec2::new(14.0, 7.0)),
        0.0,
        CHROME,
    );
    // 滑钮：有体积的实心旋钮，不再是空心环。
    //
    // 空心环保"轮廓"，在满是彩色的渐变轨道上读作一个小圈；实心 + 顶部高光 +
    // 底部暗影才读得出"可以抓的东西"——这是affordance，不是装饰。三层里没有
    // 一个用彩色，守 C4。
    //
    // A solid knob with a top highlight and bottom shadow: it reads as grabbable.
    let knob = Pos2::new(cx, rail.center().y);
    p.circle_filled(
        Pos2::new(knob.x, knob.y + 1.0),
        6.5,
        Color32::from_black_alpha(120),
    );
    p.circle_filled(
        knob,
        6.0,
        if resp.dragged() {
            // 拖拽中 = 这枚滑钮是"当前工具"：内圆转强调紫（C4 允许的第三处）。
            // 松手回落到 hover/常态，颜色变化本身构成"松手了"的反馈。
            //
            // While dragging this knob IS the current tool: the core turns
            // violet (the third C4-legal use). Releasing falls back to
            // hover/normal -- the colour change itself reads as "released".
            ACCENT
        } else if resp.hovered() {
            Color32::from_rgb(0xF5, 0xF5, 0xF5)
        } else {
            Color32::from_rgb(0xD2, 0xD2, 0xD2)
        },
    );
    p.circle_filled(
        Pos2::new(knob.x - 1.0, knob.y - 1.5),
        3.0,
        Color32::from_white_alpha(70),
    );

    if resp.double_clicked() {
        *v = def;
    } else if resp.dragged() || resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let tt = ((pos.x - rail.left()) / rail.width().max(1.0)).clamp(0.0, 1.0) as f64;
            *v = (min + tt * (max - min)).round();
        }
    }
    y + 44.0
}

/// 颜色混合器的频段滑块：几何完全交给 `slider_row`，只多传一个渐变端点。
/// 之前这里有一套独立实现（行高 28 / 轨道 6px / 数值框贴着轨道），
/// 与普通滑块处处不同 —— 用户连着两次报「风格不一样」「数值框一个在上一个挨着」。
fn band_slider(
    ui: &mut egui::Ui,
    p: &egui::Painter,
    rect: Rect,
    y: f32,
    band: usize,
    prop: usize,
    label: &str,
    v: &mut f64,
    gradient: bool,
) -> f32 {
    let pair = if gradient {
        let (a, b) = band_stops(band, prop);
        Some([a, b])
    } else {
        None
    };
    let key = format!("band{band}_{prop}");
    slider_row(
        ui,
        p,
        rect,
        y,
        &key,
        label,
        v,
        -100.0,
        100.0,
        0.0,
        0,
        pair.as_ref().map(|s| s.as_slice()),
    )
}

// ---------------------------------------------------------------- 顶栏

fn draw_menubar(ui: &mut egui::Ui, st: &mut Proto) {
    let rect = ui.max_rect();
    let p = ui.painter().clone();
    // 底色与分隔由 `ui()` 统一画；这里只管内容。
    // Fill and separators are drawn once in `ui()`.
    let cy = rect.center().y;

    // 无边框窗口：空白处按住可拖动（菜单/按钮的 interact 后注册，优先响应）
    let drag = ui.interact(rect, ui.scope_id().with("titlebar-drag"), Sense::drag());
    if drag.dragged() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

    // 品牌标记：日晷（晷 = 日影），不是通用方框。
    //
    // 一个圆盘 + 一根晷针 + 一段投影弧。它既是"恒晷"的字面图形，也是可被记住的
    // 产品记号——方框 logo 谁都能画，换成它之后左上角第一次有了自己的身份。
    // 全中性色、无填充块，守 C2/C4；三段线宽统一 1.4。
    //
    // A sundial mark: dial + gnomon + shadow arc. Literally 「晷」, and a memorable
    // product identity instead of a generic square.
    let lc = Pos2::new(rect.left() + 24.0, cy);
    let lr = 9.0;
    p.circle_stroke(lc, lr, Stroke::new(1.4, T2));
    // 晷针：从圆心指向右上（10:30 的光照方向，与全局光源一致）
    p.line_segment(
        [
            Pos2::new(lc.x - 1.5, lc.y + 1.5),
            Pos2::new(lc.x + 4.5, lc.y - 4.5),
        ],
        Stroke::new(1.6, T1),
    );
    // 投影弧：针尖在盘上的落点（一小段实弧，不是整圈）
    let mut arc: Vec<Pos2> = Vec::with_capacity(5);
    for i in 0..5 {
        let a = (-25.0 + i as f32 * 12.5).to_radians();
        arc.push(Pos2::new(
            lc.x + (lr - 2.0) * a.cos(),
            lc.y + (lr - 2.0) * a.sin(),
        ));
    }
    p.add(egui::Shape::line(arc, Stroke::new(1.4, T3)));
    p.circle_filled(lc, 1.6, T1);
    p.txt(
        Pos2::new(rect.left() + 44.0, cy),
        egui::Align2::LEFT_CENTER,
        crate::i18n::t!("恒晷"),
        f_bold(FONT_TITLE),
        T1,
    );

    // 46+32=78 是品牌区实际右缘（晷盘 24±9、品牌名 44 起宽约 32），留 16 呼吸。
    let mut x = rect.left() + 94.0;
    for label in [
        crate::i18n::t!("文件"),
        crate::i18n::t!("编辑"),
        crate::i18n::t!("视图"),
        crate::i18n::t!("插件"),
        crate::i18n::t!("帮助"),
    ] {
        let r = Rect::from_min_size(Pos2::new(x - 8.0, cy - 13.0), Vec2::new(40.0, 26.0));
        let resp = ui.interact(r, ui.scope_id().with(("menu", label)), Sense::click());
        hover_bg(
            ui,
            ui.scope_id().with(("h_menu", label)),
            resp.hovered(),
            r,
            8.0,
            BUTTON,
        );
        if resp.clicked() {
            st.say(
                ui.ctx(),
                &crate::i18n::tr("「{label}」菜单：原型未接（HTML 稿有完整菜单）")
                    .replace("{label}", label),
            );
        }
        p.txt(
            Pos2::new(x, cy),
            egui::Align2::LEFT_CENTER,
            label,
            f_bold(FONT_LABEL),
            if resp.hovered() { T1 } else { T2 },
        );
        x += 44.0;
    }

    // 文档名紧跟菜单（macOS 标题栏的做法），工具组居中，动作在右——
    // 三段各占一处，顶栏才不是"左边一堆、右边一堆、中间空一片"。
    //
    // Title next to the menus, tool group centred, actions on the right.
    // N4：文档名是全屏唯一的文档锚点 —— 16px semibold（C8 六档内），盒宽同步。
    // 13px 时它淹没在菜单行里；2026 消费/专业软件的文档名都至少 16。
    //
    // N4: the document name is the only document anchor on screen -- 16 px
    // semibold (within the C8 set), box widened to match. At 13 it drowned.
    let tab = Rect::from_min_size(Pos2::new(x + 16.0, cy - 14.0), Vec2::new(176.0, 28.0));
    p.rect_filled(tab, 8.0, Color32::from_rgb(0x31, 0x31, 0x31));
    p.txt(
        Pos2::new(tab.left() + 12.0, cy),
        egui::Align2::LEFT_CENTER,
        fit_text(&p, crate::i18n::t!("维港夜景-017"), f_bold(16.0), 148.0),
        f_bold(16.0),
        T1,
    );

    // 工具组（顺序照设计稿：抓手 H ┊ 自动 复位 ┊ 整段·单帧 ┊ 蒙版/调整 徽标）
    //
    // 三种形态各司其职：可点的用实心 `.btn`（12px/600、32 高、内距 16），
    // 纯读数的用 `.kv`（标签 t2 + 值 t1），键位提示用 `.kbd` 徽标。
    // 之前抓手是填充按钮、自动/复位是裸文字，正好反了 —— 整行于是读成一串等价的词。
    //
    // Three shapes, one job each: clickable = solid `.btn`; read-only = `.kv`
    // (t2 label + t1 value); key hints = `.kbd` chip. The hand tool used to be a
    // filled chip while 自动/复位 were bare text -- inverted, so the row read as one
    // undifferentiated string.
    // 画布工具（抓手 / 整段·单帧）已经搬到画布底部的浮动条（见 `draw_canvas_tools`）。
    //
    // 顶栏只留**窗口级**的东西：品牌、菜单、文档名、撤销、缩放读数、模块、导出。
    // "对照片做的事"离照片越近越好——Photos 和 iPad 版 Lightroom 都把编辑模式切换
    // 放在内容区底部，而不是窗口顶栏。
    //
    // Canvas tools moved to a floating bar at the bottom of the canvas. The bar keeps
    // only window-level things; "things done to the photo" belong next to the photo.

    // 蒙版 1 / 调整 3 两枚徽标移到右坞页签行——它们描述的是「当前照片有多少个
    // 蒙版和调整」，属于编辑状态，不属于窗口级导航。顶栏只留导航与只读读数。
    //
    // The mask/adjust counters moved to the dock tab row: they describe the photo's
    // edit state, not window-level navigation.

    // 右侧读数：从「显影」左界往左排 画幅 / 撤销 / 100%，再往左是性能 HUD。
    // 全部按实测宽度右对齐——顶栏的宽度预算就这么多，放不下时先牺牲 HUD，
    // 绝不能让 100% 压到 显影 上（旧版就是靠一个 magic limit 硬挡，字一改就露）。
    //
    // Right-hand readouts laid out right-to-left from the 显影 picker: 画幅 / 撤销 /
    // 100%, then the perf HUD. Everything uses measured widths; when the budget runs
    // out the HUD yields first and 100% never lands on 显影.
    let mut cur = rect.right() - 296.0;
    let zoom_font = f_bold(FONT_SMALL);
    cur -= text_w(&p, "100%", zoom_font.clone());
    p.txt(
        Pos2::new(cur, cy),
        egui::Align2::LEFT_CENTER,
        "100%",
        zoom_font,
        T1,
    );
    cur -= 18.0;
    {
        let font = f_small();
        let w = text_w(&p, crate::i18n::t!("撤销"), font.clone());
        let r = Rect::from_min_size(
            Pos2::new(cur - w - 6.0, cy - 14.0),
            Vec2::new(w + 12.0, 28.0),
        );
        let resp = ui.interact(r, ui.scope_id().with("undo"), Sense::click());
        hover_bg(
            ui,
            ui.scope_id().with("h_undo"),
            resp.hovered(),
            r,
            6.0,
            BUTTON,
        );
        p.txt(
            Pos2::new(cur, cy),
            egui::Align2::RIGHT_CENTER,
            crate::i18n::t!("撤销"),
            font,
            if resp.hovered() { T1 } else { T2 },
        );
        if resp.clicked() {
            if let Some(a) = &st.actions {
                a.send(DemoAction::Undo);
            } else {
                st.say(ui.ctx(), crate::i18n::t!("撤销：原型未接历史栈"));
            }
        }
    }
    // 任务胶囊（判据 ui_tokens::top_bar_icons_never_cover_the_tasks_label 的
    // 取样点）：标签常驻（灰），队列有活时数字亮起。与右侧簇的其余成员一样
    // 按实测宽度链式左移，语言切换不重叠。
    //
    // 900px 一档放不下时整颗胶囊让位（判据 text_never_overlaps 的阶梯收法：
    // 先牺牲读数，绝不跟文档名挤在一起）——1600px 及以上常驻，icon 判据的
    // 取样窗就在那一档。
    //
    // Tasks pill, the sampling point of the icon-overlap criterion: the label
    // always shows (muted), the count lights up while the queue is busy. It
    // chains left by measured widths. Below the 900 px tier it yields whole --
    // the overlap criterion's ladder takes readouts first and never squeezes
    // against the document name; the icon criterion samples at 1600 px where
    // the pill is permanent.
    {
        let label = crate::i18n::t!("任务");
        let font = f_small();
        let lw = text_w(&p, label, font.clone());
        let cwid = if st.tasks > 0 {
            8.0 + text_w(&p, &st.tasks.to_string(), f_bold(FONT_SMALL))
        } else {
            0.0
        };
        let undo_w = text_w(&p, crate::i18n::t!("撤销"), f_small());
        // 右缘 = 撤销左缘再让 18；左缘 = 右缘 - 胶囊内容宽。
        // 文档名右缘再留 16px 呼吸，放不下就不画。
        let pill_right = cur - undo_w - 18.0;
        let pill_left = pill_right - lw - cwid;
        if pill_left > tab.right() + 16.0 {
            p.txt(
                Pos2::new(pill_right, cy),
                egui::Align2::RIGHT_CENTER,
                label,
                font,
                T3,
            );
            if st.tasks > 0 {
                let cnt = st.tasks.to_string();
                let cfont = f_bold(FONT_SMALL);
                p.txt(
                    Pos2::new(pill_right - lw - 8.0, cy),
                    egui::Align2::RIGHT_CENTER,
                    cnt,
                    cfont,
                    T1,
                );
            }
        }
    }

    // 画幅 / GPU / ETA 三组读数下移状态栏：它们只在"我想知道现在的状态"时看，
    // 不是每次操作都要确认的东西。顶栏留给动作，状态栏留给读数。
    //
    // Frame size / GPU / ETA moved to the status bar: they are read when you want
    // to know, not on every action.

    // 导出是次级动作：设计稿 `.btn-export` 明确「32px ghost，白底大块会压过内容」
    // （裁决 A09）。之前是 14px 实心块，比主内容还重，一眼看去就是它最响。
    //
    // Export is the secondary action: the mockup keeps a 32 px ghost button because
    // "a big light block overwhelms the content" (decision A09). It used to be a
    // 14 px filled block that shouted louder than the photo.
    let export = Rect::from_min_size(
        Pos2::new(rect.right() - 210.0, cy - 16.0),
        Vec2::new(84.0, 32.0),
    );
    let er = ui.interact(export, ui.scope_id().with("export"), Sense::click());
    // hover 90ms 线性插值（C9）：底色从壳灰滑到钮灰。透明底画在壳上等效壳灰，
    // 插值在两个实色间进行，避免逐帧 alpha 叠加。
    //
    // Hover eases over 90 ms (C9): the fill slides from shell grey to button
    // grey. The transparent base renders as the shell, so we interpolate
    // between two solids instead of stacking alpha per frame.
    let ht =
        ui.ctx()
            .animate_bool_with_time(ui.scope_id().with("export_hover"), er.hovered(), 0.09);
    let mixc = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * ht).round() as u8;
    p.rect_filled(
        export,
        8.0,
        Color32::from_rgb(mixc(0x2A, 0x42), mixc(0x2A, 0x42), mixc(0x2A, 0x42)),
    );
    p.rect_stroke(
        export,
        8.0,
        Stroke::new(1.0, Color32::from_rgb(0x5D, 0x5D, 0x5D)),
        egui::StrokeKind::Inside,
    );
    p.txt(
        export.center(),
        egui::Align2::CENTER_CENTER,
        crate::i18n::t!("导出…"),
        f_bold(FONT_SMALL),
        T1,
    );
    if er.clicked() {
        if let Some(a) = &st.actions {
            a.send(DemoAction::Export);
        } else {
            st.say(ui.ctx(), crate::i18n::t!("导出浮层：原型未接"));
        }
    }

    let modpick = Rect::from_min_size(
        Pos2::new(rect.right() - 292.0, cy - 14.0),
        Vec2::new(68.0, 28.0),
    );
    let mr = ui.interact(modpick, ui.scope_id().with("modpick"), Sense::click());
    if mr.hovered() {
        p.rect_filled(modpick, 8.0, BUTTON);
    }
    // 当前模块名来自真状态（桥接同步）：EN 模式下过 tr 渲染英文。
    //
    // The picker label comes from real state; tr renders English in EN mode.
    p.txt(
        Pos2::new(modpick.center().x - 5.0, modpick.center().y),
        egui::Align2::CENTER_CENTER,
        crate::i18n::tr(st.module.cn()),
        f_small(),
        T1,
    );
    chevron(
        &p,
        Pos2::new(modpick.center().x + 20.0, modpick.center().y),
        7.0,
        T3,
    );
    // ---- 七模块下拉菜单 ----
    //
    // 点击 modpick 开合；开启时一块全屏点击捕手垫底（点到菜单外即收起），
    // 菜单本体画在 Foreground 层——不受顶栏 clip_rect 裁剪。行点击发
    // DemoAction::SelectModule（真 API：state.module），非显影模块 toast 说明
    // 该屏在下一版接入。仅开发屏有完整界面是 v21 的既定范围（PROGRESS-v21 §6）。
    //
    // Seven-module dropdown: a full-screen click catcher underneath closes the
    // menu on outside clicks; rows send DemoAction::SelectModule (the real
    // state.module). Non-Develop picks toast that their screens land in the next
    // build -- the documented v21 scope.
    let modmenu_id = ui.scope_id().with("modmenu");
    let open = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(modmenu_id).unwrap_or(false));
    if mr.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(modmenu_id, !open));
    }
    if open {
        let ctx = ui.ctx().clone();
        let menu_pos = Pos2::new(modpick.left(), modpick.bottom() + 4.0);
        // 点击捕手：全屏、先画（垫底）。
        egui::Area::new(modmenu_id.with("catch"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::ZERO)
            .interactable(true)
            .show(&ctx, |ui| {
                let resp = ui.allocate_rect(ctx.content_rect(), Sense::click());
                if resp.clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(modmenu_id, false));
                }
            });
        egui::Area::new(modmenu_id)
            .order(egui::Order::Foreground)
            .fixed_pos(menu_pos)
            .movable(false)
            .show(&ctx, |ui| {
                let mu = ui.painter().clone();
                let row_w = 168.0;
                let row_h = 30.0;
                let menu_rect = Rect::from_min_size(
                    menu_pos,
                    Vec2::new(row_w, row_h * crate::state::Module::ALL.len() as f32 + 12.0),
                );
                mu.rect_filled(menu_rect.expand(6.0), 12.0, Color32::from_black_alpha(58));
                mu.rect_filled(menu_rect, 10.0, FIELD);
                mu.rect_stroke(
                    menu_rect,
                    10.0,
                    Stroke::new(1.0, Color32::from_black_alpha(96)),
                    egui::StrokeKind::Inside,
                );
                let mut ry = menu_rect.top() + 6.0;
                for m in crate::state::Module::ALL {
                    let row = Rect::from_min_size(
                        Pos2::new(menu_rect.left() + 6.0, ry),
                        Vec2::new(row_w - 12.0, row_h),
                    );
                    let rr =
                        ui.interact(row, ui.scope_id().with(("modrow", m.cn())), Sense::click());
                    if rr.hovered() || m == st.module {
                        mu.rect_filled(row, 6.0, BUTTON);
                    }
                    if m == st.module {
                        mu.circle_filled(Pos2::new(row.left() + 12.0, row.center().y), 2.5, ACCENT);
                    }
                    mu.txt(
                        Pos2::new(row.left() + 24.0, row.center().y),
                        egui::Align2::LEFT_CENTER,
                        crate::i18n::tr(m.cn()),
                        if m == st.module {
                            f_bold(FONT_SMALL)
                        } else {
                            f_small()
                        },
                        if rr.hovered() || m == st.module {
                            T1
                        } else {
                            T2
                        },
                    );
                    if rr.clicked() {
                        if let Some(a) = &st.actions {
                            a.send(DemoAction::SelectModule(m));
                        }
                        if m != crate::state::Module::Develop {
                            st.say(
                                ui.ctx(),
                                &crate::i18n::tr("「{m}」屏将在下一版接入")
                                    .replace("{m}", crate::i18n::tr(m.cn())),
                            );
                        }
                        ui.ctx().data_mut(|d| d.insert_temp(modmenu_id, false));
                    }
                    ry += row_h;
                }
            });
    }

    // 窗口按钮：Windows 的位置与形状（右上、方形、hover 才出底），但压扁成 30×26
    // 并居中在 44px 顶栏里——这样既有 Windows 用户的手感，又不会把顶栏右端的圆角
    // 切成直角。
    //
    // 先试过 macOS 的三枚圆点，问题是：这是一个 Windows 程序，交通灯在这个语境里
    // 读作"仿 Mac 失败"，而且红黄绿会跟"已修改""警告"的状态色撞语义。
    //
    // Windows-shaped buttons (top-right, square, hover fill) but 30x26 and centred in
    // the 44 px bar, so they keep the bar's rounded corner. macOS traffic lights were
    // tried first and read as a failed imitation here -- and red/amber/green collide
    // with the "modified / warning" status colours.
    let bx = rect.right() - 108.0;
    for (i, kind) in ["min", "max", "close"].iter().enumerate() {
        let c = Pos2::new(bx + i as f32 * 34.0, cy);
        let r = Rect::from_center_size(c, Vec2::new(30.0, 26.0));
        let resp = ui.interact(r, ui.scope_id().with(("win", kind)), Sense::click());
        if resp.hovered() {
            p.rect_filled(
                r,
                6.0,
                if *kind == "close" {
                    Color32::from_rgb(0xC0, 0x3A, 0x3A)
                } else {
                    BUTTON
                },
            );
        }
        let col = if resp.hovered() {
            if *kind == "close" { Color32::WHITE } else { T1 }
        } else {
            T3
        };
        match *kind {
            "min" => {
                p.hline(c.x - 4.5..=c.x + 4.5, c.y, Stroke::new(1.2, col));
            }
            "max" => {
                p.rect_stroke(
                    Rect::from_center_size(c, Vec2::splat(9.0)),
                    0.0,
                    Stroke::new(1.2, col),
                    egui::StrokeKind::Inside,
                );
            }
            _ => {
                p.line_segment(
                    [
                        Pos2::new(c.x - 4.5, c.y - 4.5),
                        Pos2::new(c.x + 4.5, c.y + 4.5),
                    ],
                    Stroke::new(1.2, col),
                );
                p.line_segment(
                    [
                        Pos2::new(c.x + 4.5, c.y - 4.5),
                        Pos2::new(c.x - 4.5, c.y + 4.5),
                    ],
                    Stroke::new(1.2, col),
                );
            }
        }
        if resp.clicked() {
            match *kind {
                "min" => ui
                    .ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                "max" => {
                    let m = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Maximized(!m));
                }
                _ => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }
}

/// Liquid Glass 镜面边（无模糊口径）：顶 1px 高光 + 底 1px 暗沿。
/// C 契约禁玻璃模糊材质，但 Tahoe "光学感"里可移植的部分正是这条边缘光——
/// 它只表达"这是一块有厚度的浮层"，不引入透明度混乱，也不占紫色预算。
///
/// Liquid Glass specular edge (no-blur take): a 1 px top highlight and a 1 px
/// dark bottom edge. The contract bans glass *blur*; the portable part of the
/// Tahoe look is exactly this edge light -- it reads as "a slab with thickness"
/// without touching transparency or the violet budget.
fn glass_edge(p: &egui::Painter, rect: Rect, r: f32) {
    p.rect_filled(
        Rect::from_min_size(
            Pos2::new(rect.left() + r, rect.top() + 0.5),
            Vec2::new(rect.width() - 2.0 * r, 1.0),
        ),
        0.0,
        Color32::from_white_alpha(14),
    );
    p.rect_filled(
        Rect::from_min_size(
            Pos2::new(rect.left() + r, rect.bottom() - 1.5),
            Vec2::new(rect.width() - 2.0 * r, 1.0),
        ),
        0.0,
        Color32::from_black_alpha(46),
    );
}

/// AI 命令条：画布顶部居中浮层（2026 "AI 信任态 / prompt bar" 趋势的落法）。
/// 待命态回显上一条指令；执行中（selftest）加一条细进度线。紫色只出现在
/// 6px 在場点（C4：Agent 是工具，紫=你在这里）——文案、键帽、进度线全中性。
/// 镜面边用 [`glass_edge`]，投影与底部工具条同一套（黑 70/offset 4）。
///
/// AI command bar: a centered chip at the top of the canvas (the 2026 "AI trust
/// state / prompt bar" pattern). Idle echoes the last instruction; running
/// (selftest) adds a thin progress line. Violet appears only in the 6 px
/// presence dot (C4) -- copy, key cap and progress stay neutral. Edges via
/// [`glass_edge`]; the shadow matches the bottom tool bar (black 70 / offset 4).
fn draw_ai_cmdbar(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, st: &mut Proto) {
    let bar_h = 34.0;
    let text = if st.ai_busy {
        crate::i18n::t!("正在执行：去闪 + 日转夜 ramp（28 / 48 帧）")
    } else {
        crate::i18n::t!("对整段做去闪 + 日转夜 ramp，保留车流鬼影")
    };
    let font = f_small();
    let meta = egui::FontId::proportional(FONT_META);
    let tw = text_w(p, text, font.clone());
    let esc_w = text_w(p, "Esc", meta.clone()) + 10.0; // kbd_chip 同口径
    let dot_w = 6.0;
    let bar_w = 12.0 + dot_w + 10.0 + tw + 10.0 + esc_w + 12.0; // C7
    // 左浮层展开时命令条中心右移半个面板宽，避免压住「预设与历史」（900px 实测回归）。
    let cx = if st.left_open {
        (rect.left() + 12.0 + LEFT_W + rect.right()) * 0.5
    } else {
        rect.center().x
    };
    let bar = Rect::from_center_size(
        Pos2::new(cx, rect.top() + 14.0 + bar_h * 0.5),
        Vec2::new(bar_w, bar_h),
    );
    // 投影（环境层）→ 底（框架层 CHROME）→ 镜面边 → 内容。与底部工具条同一套。
    p.rect_filled(
        bar.translate(Vec2::new(0.0, 4.0)),
        10.0,
        Color32::from_black_alpha(70),
    );
    p.rect_filled(bar, 10.0, CHROME);
    glass_edge(p, bar, 10.0);
    let cy = bar.center().y;
    let resp = ui.interact(bar, ui.scope_id().with("ai_cmdbar"), Sense::click());
    hover_bg(
        ui,
        ui.scope_id().with("ai_hover"),
        resp.hovered(),
        bar,
        10.0,
        HOVER,
    );
    if resp.clicked() && !st.ai_busy {
        // 信任态：点击给出"已排队、可取消"的明确承诺（2026 AI-trust 口径），
        // 而不是静默开始一个用户不知道去哪停的任务。
        //
        // Trust state: clicking acknowledges with "queued, cancellable"
        // instead of silently starting a task the user cannot stop.
        st.toast_now(
            ui.ctx(),
            crate::i18n::t!("已加入队列 · 整段 48 帧 · 可随时取消"),
        );
    }
    let mut tx = bar.left() + 12.0; // C7
    // 在場点：6px 紫圆。Agent 是工具，"AI 在此"是它唯一的紫色用途（C4）。
    p.circle_filled(Pos2::new(tx + 3.0, cy), 3.0, ACCENT);
    tx += dot_w + 10.0;
    p.txt(
        Pos2::new(tx, cy),
        egui::Align2::LEFT_CENTER,
        text,
        font.clone(),
        T1,
    );
    tx += tw + 10.0;
    kbd_chip(p, tx, cy, "Esc"); // C7：键帽距文案 10px，右缘 12px 已计入 bar_w
    // 执行中：细进度线贴底（中性白，不占紫色预算；阶段名在文案里）。
    if st.ai_busy {
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(bar.left() + 12.0, bar.bottom() - 3.0),
                Vec2::new((bar.width() - 24.0) * 0.58, 2.0),
            ),
            1.0,
            Color32::from_white_alpha(150),
        );
    }
}

/// 画布底部的浮动工具条：抓手 / 整段·单帧。
///
/// 顶栏只放窗口级元素之后，这几个"画布操作"下沉到这里：离照片一只手的位置，
/// 也不再占顶栏的宽度预算。底衬用 `CHROME`（框架层）而不是 `CARD`——它是控件
/// 容器，不是内容卡片；控件内部高度 26（比顶栏的 32 小一档，浮动条更紧凑）。
///
/// Floating tool bar at the bottom of the canvas: window-level chrome moved up,
/// canvas-level tools moved next to the photo.
fn draw_canvas_tools(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, st: &mut Proto) {
    let bar_h = 34.0;
    let bar = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.bottom() - 32.0 - bar_h * 0.5),
        Vec2::new(248.0, bar_h), // M8: 加了抓手图标后加宽
    );
    p.rect_filled(
        bar.translate(Vec2::new(0.0, 4.0)),
        10.0,
        Color32::from_black_alpha(70),
    );
    p.rect_filled(bar, 10.0, CHROME);
    glass_edge(p, bar, 10.0);
    let cy = bar.center().y;

    let mut tx = bar.left() + 12.0; // C7
    {
        // 抓手图标：一枚 14px 的手掌符号，坐在带光照的小方块上。
        // 之前"抓手"是裸文字 + H 键帽，读不出这是个可切换的工具模式。
        let ic = Rect::from_center_size(Pos2::new(tx + 9.0, cy), Vec2::splat(20.0));
        rounded_grad(p, ic, 6.0, shade(BUTTON, 1.1), shade(BUTTON, 0.94), false);
        let hc = ic.center();
        // 手掌：一根竖拇指 + 三指短横 + 掌根
        p.line_segment(
            [
                Pos2::new(hc.x - 4.0, hc.y + 3.0),
                Pos2::new(hc.x - 4.0, hc.y - 3.0),
            ],
            Stroke::new(1.3, T1),
        );
        for (dx, dy) in [(-1.0, -4.0), (1.5, -3.0), (3.5, -1.0)] {
            p.line_segment(
                [
                    Pos2::new(hc.x + dx - 1.0, hc.y + dy + 2.0),
                    Pos2::new(hc.x + dx, hc.y + dy - 2.0),
                ],
                Stroke::new(1.3, T1),
            );
        }
        p.line_segment(
            [
                Pos2::new(hc.x - 5.0, hc.y + 4.0),
                Pos2::new(hc.x + 4.0, hc.y + 4.0),
            ],
            Stroke::new(1.3, T1),
        );
        tx = ic.right() + 8.0;

        let font = f_small();
        p.txt(
            Pos2::new(tx, cy),
            egui::Align2::LEFT_CENTER,
            crate::i18n::t!("抓手"),
            font.clone(),
            T2,
        );
        tx += text_w(p, crate::i18n::t!("抓手"), font) + 6.0;
        tx += kbd_chip(p, tx, cy, "H") + 12.0; // C7
    }
    vsep(p, tx, cy);
    tx += 12.0;
    {
        let font = f_bold(FONT_SMALL);
        let w1 = text_w(p, crate::i18n::t!("整段"), font.clone()) + 26.0;
        let w2 = text_w(p, crate::i18n::t!("单帧"), font.clone()) + 26.0;
        let seg = Rect::from_min_size(Pos2::new(tx, cy - 13.0), Vec2::new(w1 + w2, 26.0));
        p.rect_filled(seg, 8.0, BUTTON); // C6
        let on = Rect::from_min_size(seg.min, Vec2::new(w1, seg.height()));
        p.rect_filled(on, 8.0, Color32::from_rgb(0x50, 0x50, 0x50)); // C6
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(on.left() + 12.0, on.bottom() - 3.0),
                Vec2::new(on.width() - 24.0, 2.0),
            ),
            1.0,
            ACCENT,
        );
        p.txt(
            on.center(),
            egui::Align2::CENTER_CENTER,
            crate::i18n::t!("整段"),
            font.clone(),
            T1,
        );
        p.txt(
            Pos2::new(on.right() + w2 * 0.5, cy),
            egui::Align2::CENTER_CENTER,
            crate::i18n::t!("单帧"),
            font,
            T3,
        );
        if ui
            .interact(
                Rect::from_min_size(seg.min, Vec2::new(w2, seg.height())),
                ui.scope_id().with("cadence_single"),
                Sense::click(),
            )
            .clicked()
        {
            st.say(ui.ctx(), crate::i18n::t!("单帧模式：原型只展示整段"));
        }
    }
}

// ---------------------------------------------------------------- 左栏

/// 底部状态栏：就绪 / 硬件加速 / 画幅·色彩空间 / GPU 帧时间 / 开源版本。
/// `frame_ms` 是最近 30 帧的中位数（由 `ui()` 采样后传入）。
///
/// Status line. `frame_ms` is the median of the last 30 frames.
fn draw_status(ui: &mut egui::Ui, frame_ms: f32) {
    let rect = ui.max_rect();
    let p = ui.painter();
    // 状态栏不再是一条贴底的带子：它直接浮在桌面上，无底色、无上线。
    // No strip, no rule: the status line sits straight on the desk.
    let cy = rect.center().y;
    // C7: 14/70 归到 16/16；第二项位置由第一项实测宽度 + 16 推出，不写死 70。
    let ready = crate::i18n::t!("就绪");
    let ready_font = f_small();
    p.txt(
        Pos2::new(rect.left() + 16.0, cy),
        egui::Align2::LEFT_CENTER,
        ready,
        ready_font.clone(),
        T2,
    );
    p.txt(
        Pos2::new(rect.left() + 16.0 + text_w(p, ready, ready_font) + 16.0, cy),
        egui::Align2::LEFT_CENTER,
        crate::i18n::t!("硬件加速 · 流畅"),
        f_small(),
        T3,
    );

    // 右侧只留「画幅」与帧时间。开源库链接、版本号这类信息属于「关于」对话框，
    // 常驻在状态栏等于把产品的自我宣传钉在用户眼前——它不是用户此刻需要的东西。
    //
    // Only frame size and frame time on the right. The repo link / version tag
    // belongs in About, not pinned to the status line.
    let mut cur = rect.right() - 16.0; // C7

    // GPU 帧时间取真实测量（最近 30 帧中位数），不再是写死的「8.2 ms/帧」——
    // 写死的读数永远绿，等于没有判据（性能验收必须建立在真实测量上）。
    //
    // Real frame time (median of 30); the old hard-coded "8.2 ms" could never fail.
    let gpu = crate::i18n::tr("GPU {ms} ms/帧").replace("{ms}", &format!("{frame_ms:.1}"));
    let mono = egui::FontId::monospace(FONT_META); // C8: 读数等宽
    p.txt(
        Pos2::new(cur, cy),
        egui::Align2::RIGHT_CENTER,
        &gpu,
        mono.clone(),
        T4_TEXT,
    );
    cur -= text_w(p, &gpu, mono) + 24.0; // C7

    let frame = format!("{PHOTO_W:.0}×{PHOTO_H:.0} · P3");
    p.txt(
        Pos2::new(cur, cy),
        egui::Align2::RIGHT_CENTER,
        &frame,
        f_small(),
        T3,
    );
}

/// 左侧 AI 工具轨（效果图 `.airail`，56px）：四个按钮，第一个带紫点表示激活。
fn draw_rail(ui: &mut egui::Ui, st: &mut Proto) {
    let rect = ui.max_rect();
    let p = ui.painter().clone();
    // 轨道自身不画底色与边框：它是框架的一部分，只有右缘那一条暗线属于它。
    // The rail draws no fill and no border -- just the frame's right-hand seam.

    // 工具轨：设计稿 `.ai-btn` 是 40×44，图标在上（cy-7）、11px 标签在下（cy+15），
    // 两者都在按钮内，item 间距 5，蒙版与批量之间多一条 `.ai-gap` 16。
    // 之前是 36×36 + 62 间距、标签落在按钮外 22px —— 四个图标看着像悬在一条竖线上，
    // 标签则像另一个列里的碎字；蒙版还画成空圆圈（设计稿是魔棒 + 星）。
    //
    // Rail buttons are 40x44 with the 11 px label *inside* the button, 5 px apart, plus
    // an extra 16 px gap before 批量. It used to be 36x36 boxes 62 px apart with the
    // label 22 px below the box -- icons looked like dots on a string and labels like a
    // second column of debris. 蒙版 was a bare circle instead of the wand + sparkle.
    // 工具轨五项，**底部锚定「批量」**。
    //
    // 五个按钮从上往下排只占到 y≈220，一条 800px 高的窄柱下面 700px 是空的——
    // 贴边只解决了"浮贴纸"，没解决"柱子只有上半截有东西"（架构师实测利用率 26%）。
    // 把批处理钉到轨底，柱子两头就都有内容，中间那段空正好读成"上下两组"。
    //
    // 批量 is pinned to the rail's bottom edge: five buttons stacked from the top
    // only fill the upper quarter of an 800 px column.
    let items: [(&str, u8, bool); 5] = [
        (crate::i18n::t!("预设"), 4, false),
        (crate::i18n::t!("对话"), 0, false),
        (crate::i18n::t!("本帧"), 1, false),
        (crate::i18n::t!("蒙版"), 2, false),
        (crate::i18n::t!("批量"), 3, true),
    ];
    let mut top = rect.top() + 8.0;
    for (i, (label, kind, pin_bottom)) in items.iter().enumerate() {
        // 三组：面板开关 / 单帧工作 / 批处理，组间 12px。
        if matches!(kind, 0) {
            top += 12.0;
        }
        let y = if *pin_bottom {
            rect.bottom() - 52.0
        } else {
            top
        };
        let btn = Rect::from_min_size(Pos2::new(rect.center().x - 20.0, y), Vec2::new(40.0, 44.0));
        let resp = ui.interact(btn, ui.scope_id().with(("ai", i)), Sense::click());
        // C4：琥珀竖条只能有一根。之前"预设浮层已开"和"对话工具已选"各画一根，
        // 左上角并排两根紫条——两种语义抢同一个信号，紫就什么都没说了。
        //
        // 现在：**工具选中**用竖条（"你在这里"）；**浮层开合**用底衬提亮（材质表达
        // 状态，不占颜色）。
        //
        // C4: only ONE violet bar. "Panel open" now uses a brighter plate (material),
        // leaving the amber bar exclusively for "current tool".
        let selected = *kind == 0;
        let panel_open = *kind == 4 && st.left_open;
        // 图标底座：每枚图标坐在自己的圆角方块上，方块有上亮下暗的光照。
        // 之前图标是"悬在竖线上的线稿"，因为没有承载物；有了底座，工具轨从
        // 一列符号变成一排可点的控件——这是 Figma/VS Code 活动栏的共同语法。
        //
        // Each icon sits on its own lit tile: transforms a column of symbols into
        // a row of controls.
        let tile = Rect::from_center_size(btn.center(), Vec2::splat(30.0));
        // 未选中也保留一层极淡的底（比轨道亮 6%）：图标有承载物，工具轨才读作
        // "一排控件"；完全没有底时，它永远是一列飘着的符号。
        let base = if selected {
            shade(BUTTON, 1.18)
        } else if panel_open || resp.hovered() {
            shade(PANEL_SIDE, 1.24)
        } else {
            shade(PANEL_SIDE, 1.07)
        };
        rounded_grad(&p, tile, 8.0, shade(base, 1.06), shade(base, 0.94), false);
        if selected {
            p.rect_filled(
                Rect::from_min_size(
                    Pos2::new(btn.left(), btn.top() + 10.0),
                    Vec2::new(2.0, btn.height() - 20.0),
                ),
                1.0,
                ACCENT,
            );
        }
        if !(selected || panel_open) {
            hover_bg(
                ui,
                ui.scope_id().with(("h_rail", i)),
                resp.hovered(),
                btn,
                8.0,
                BUTTON,
            );
        }
        let col = if selected || panel_open { T1 } else { T2 };
        let c = Pos2::new(btn.center().x, btn.top() + 15.0);
        match kind {
            // 侧栏（预设与历史）：外框 + 左侧分栏线，和画布面板的语义一致
            4 => {
                let ic = Rect::from_center_size(c, Vec2::new(15.0, 13.0));
                p.rect_stroke(ic, 3.0, Stroke::new(1.5, col), egui::StrokeKind::Inside); // C6
                p.vline(
                    ic.left() + ic.width() * 0.38,
                    ic.y_range(),
                    Stroke::new(1.5, col),
                );
            }
            // 星形（Agent）
            0 => {
                let mut pts = Vec::with_capacity(8);
                for k in 0..8 {
                    let ang = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 4.0;
                    let r = if k % 2 == 0 { 6.5 } else { 2.6 };
                    pts.push(Pos2::new(c.x + r * ang.cos(), c.y + r * ang.sin()));
                }
                p.add(egui::Shape::closed_line(pts, Stroke::new(1.5, col)));
            }
            // 气泡（本帧对话）
            1 => {
                p.rect_stroke(
                    Rect::from_center_size(Pos2::new(c.x, c.y - 1.0), Vec2::new(15.0, 12.0)),
                    4.0,
                    Stroke::new(1.5, col),
                    egui::StrokeKind::Inside,
                );
                p.line_segment(
                    [
                        Pos2::new(c.x - 4.0, c.y + 5.0),
                        Pos2::new(c.x - 2.0, c.y + 9.0),
                    ],
                    Stroke::new(1.5, col),
                );
            }
            // 魔棒 + 星（蒙版）：设计稿 path M4 16 L13 7 + 四角星
            2 => {
                p.line_segment(
                    [
                        Pos2::new(c.x - 6.0, c.y + 6.0),
                        Pos2::new(c.x + 1.5, c.y - 1.5),
                    ],
                    Stroke::new(1.5, col),
                );
                let sp = Pos2::new(c.x + 4.5, c.y - 4.5);
                let mut pts = Vec::with_capacity(8);
                for k in 0..8 {
                    let ang = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 4.0;
                    let r = if k % 2 == 0 { 4.2 } else { 1.3 };
                    pts.push(Pos2::new(sp.x + r * ang.cos(), sp.y + r * ang.sin()));
                }
                p.add(egui::Shape::closed_line(pts, Stroke::new(1.3, col)));
            }
            // 方框 + 加号（批量）
            _ => {
                p.rect_stroke(
                    Rect::from_center_size(c, Vec2::splat(13.0)),
                    3.0,
                    Stroke::new(1.5, col),
                    egui::StrokeKind::Inside,
                );
                p.line_segment(
                    [Pos2::new(c.x - 3.0, c.y), Pos2::new(c.x + 3.0, c.y)],
                    Stroke::new(1.5, col),
                );
                p.line_segment(
                    [Pos2::new(c.x, c.y - 3.0), Pos2::new(c.x, c.y + 3.0)],
                    Stroke::new(1.5, col),
                );
            }
        }
        // 那个「有新内容」的紫点已删：选中态已经有琥珀竖条，再加一个点就是
        // 同一个小方块上两个强调信号。
        //
        // The violet "has new content" dot is gone: the amber bar already marks the
        // active item, and two accents on one 40 px button is one too many.
        p.txt(
            Pos2::new(btn.center().x, btn.top() + 33.0),
            egui::Align2::CENTER_CENTER,
            *label,
            egui::FontId::proportional(FONT_META),
            if selected || panel_open { T2 } else { T3 },
        );
        if resp.clicked() {
            if *kind == 4 {
                st.left_open = !st.left_open;
            } else {
                st.say(
                    ui.ctx(),
                    crate::i18n::t!("AI 面板：原型未接（HTML 稿有极光浮层）"),
                );
            }
        }
        if !*pin_bottom {
            top += 44.0 + 5.0;
        }
    }
}

/// 六个预设的「影调指纹」：暗部色 → 亮部色、快捷键。
///
/// 原来画的是 96×34 的照片缩略图 —— 同一张照片复制六份再压到 34px 高，看不清
/// 内容、更读不出预设差别，用户的原话是「不希望压缩照片显示，界面要服务于照片」。
/// 两条色带的走向本身就是预设的调性（暗部偏青、亮部偏琥珀…），看色带比看糊掉
/// 的照片准，也不再假装「这里有一张照片」。真正判断预设效果只能看画布大图，
/// 这是 ACR / Lightroom 的做法。
///
/// Six presets as a tonal fingerprint (shadow colour -> highlight colour) instead of
/// a 96x34 photo thumbnail: six copies of one frame squashed to 34 px read as neither
/// a photo nor a preview. A two-stop band states the grade itself; judging a preset
/// belongs on the full-size canvas, as ACR and Lightroom do it.
const PRESETS: [(&str, u32, u32, &str); 6] = [
    ("标准", 0x4A4A4C, 0xD8D8D8, "⇧1"),
    ("鲜艳风光", 0x123A4A, 0xE8B96A, "⇧2"),
    ("冷色电影", 0x101A2C, 0xC8D6E8, "⇧3"),
    ("黑白对比", 0x0A0A0A, 0xF2F2F2, "⇧4"),
    ("复古胶片", 0x2C2216, 0xDCC49A, "⇧5"),
    ("青橙调", 0x0E2A2E, 0xE0762E, "⇧6"),
];

/// 历史条目：改了什么、什么时候、属于哪一组（设计稿 `.histrow` 的三段）。
/// 条数取 14：12 条时列表底到注脚还空 72px（面板的 8.5%），14 条 × 36 正好铺到
/// 注脚线上。标题里的步数由表长算出，不写死 —— 表比计数长就是骗人。
///
/// History rows: what changed, when, which group. 14 rows is what fits exactly;
/// the step counter in the section head is derived from the table length.
const HISTORY: [(&str, &str, &str); 18] = [
    ("天空提亮 · 径向", "21:47 · 当前", "蒙版"),
    ("高光 −30 · 阴影 +18", "21:46", "基本"),
    ("白平衡 5500K +6M", "21:45", "颜色"),
    ("蓝色饱和 +12", "21:44", "颜色"),
    ("曲线 中间调 +4", "21:43", "曲线"),
    ("降噪 明亮度 18", "21:41", "细节"),
    ("曝光 +0.35 · 对比度 +12", "21:41", "基本"),
    ("HSL 蓝色 明度 −8", "21:40", "HSL"),
    ("色温 5500 K", "21:40", "基本"),
    ("裁剪 16:9 · 3840×2160", "21:39", "几何"),
    ("镜头校正 自动", "21:39", "镜头"),
    ("去闪 · 窗口 5 帧 强度 62", "21:39", "去闪"),
    ("色彩空间 显示 P3", "21:38", "色彩"),
    ("锐化 数量 40 半径 0.8", "21:38", "细节"),
    ("暗角 −12", "21:37", "效果"),
    ("颗粒 20", "21:37", "效果"),
    ("评级 4 星", "21:36", "序列"),
    ("导入 48 张照片", "21:35", "序列"),
];

/// 历史总步数（源程序 `history::HISTORY_LIMIT` 是 200 步的内存上限）。
/// 面板一屏只放得下十来条，剩下的用底部「更早 N 步」交代——总数与可见条数
/// 必须对得上，否则计数就是假的。
///
/// Total history length (the app keeps 200 steps in memory). Only a dozen rows
/// fit; the footer states how many are older, and the two numbers must add up.
const HISTORY_TOTAL: usize = 202;

/// `0xRRGGBB` → `Color32`。
fn hex_rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// 按实测宽度截断并补省略号（源程序 `theme::ellipsis` 的等价物）。
///
/// Truncate to a measured width and append an ellipsis (the main app's
/// `theme::ellipsis`).
fn fit_text(p: &egui::Painter, text: &str, font: egui::FontId, max_w: f32) -> String {
    if text_w(p, text, font.clone()) <= max_w {
        return text.to_owned();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut probe = out.clone();
        probe.push(ch);
        probe.push('…');
        if text_w(p, &probe, font.clone()) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// 左栏：面板头 + 预设（色带）+ 历史（紧凑行）+ 页脚。
///
/// 三处改动都在回答同一个问题「界面怎么服务于照片」：
/// 1. 预设不再用压缩照片占位，改成暗部→亮部的影调色带；
/// 2. 历史行高 42 → 36，12 条铺满而不是留 200px 死白；
/// 3. 面板头可收起，204px 直接还给画布（设计稿 `.ltoggle` / `.railtab` 原本就有）。
///
/// The left panel. Presets use tonal bands instead of squashed photos, history rows
/// are 36 px tall and fill the panel, and the whole column collapses to give its
/// 204 px back to the canvas.
fn draw_left(ui: &mut egui::Ui, rect: Rect, st: &mut Proto) {
    let p = ui.painter().clone();
    // 浮层容器：外阴影 + 10 圆角 + 1px 16% 白描边。
    //
    // 这是「如何放这些界面」的答案：预设与历史是**次要工具**，不该常年吃走
    // 204px 画布宽度。主程序 2026-11-04 已经做过同一个判断（`shell/windows.rs`
    // `presets_window`：「这三样都是次要功能，不该占一整栏画布宽度……ACR 同样把
    // 预设/快照做成工具条上随叫随到的面板」）。这里沿用它的结论：默认收起，
    // 叫出来时压在画布左上，照片的宽高比永远不变。
    //
    // Floating container: shadow, 10 px radius, 16% white hairline. Presets and
    // history are secondary tools and must not cost a permanent 204 px column --
    // the main app already settled this in `shell/windows.rs::presets_window`.
    // 浮层容器：三层柔影 + 不透明底，**不描边**。
    //
    // 原来还有一圈 1px 白 16% 的描边。在深色界面上它是"边界"，可一旦压在照片上
    // （尤其夜景的亮部），那圈亮线就成了照片的"重影"——面板底部与照片之间多出
    // 一条来路不明的亮带。浮层不透明，边界由柔影负责就够了。
    //
    // Three-layer soft shadow, opaque fill, **no hairline**. The old 1px 16% white
    // stroke read as a ghost edge once the panel sat on top of a bright photo.
    for (dy, grow, a) in [(4.0, 0.0, 64u8), (10.0, 3.0, 38), (20.0, 8.0, 20)] {
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.left() - grow, rect.top() + dy - grow),
                Pos2::new(rect.right() + grow, rect.bottom() + dy + grow),
            ),
            12.0 + grow,
            Color32::from_black_alpha(a),
        );
    }
    p.rect_filled(rect, 12.0, CHROME);
    // 吃掉浮层内的按下/拖拽，否则拖列表会顺带把画布平移走。
    ui.interact(
        rect,
        ui.scope_id().with("leftpanel"),
        Sense::click_and_drag(),
    );

    // ---- 面板头（设计稿 `.lhead`：40 高、底部 42% 黑分隔、右侧关闭）----
    let head = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 40.0));
    p.hline(
        head.x_range(),
        head.bottom() - 0.5,
        Stroke::new(1.0, Color32::from_black_alpha(107)),
    );
    p.txt(
        Pos2::new(rect.left() + 14.0, head.center().y),
        egui::Align2::LEFT_CENTER,
        crate::i18n::t!("预设与历史"),
        f_bold(FONT_TITLE),
        T1,
    );
    let hide_r = Rect::from_center_size(
        Pos2::new(rect.right() - 17.0, head.center().y),
        Vec2::splat(24.0),
    );
    let hide = ui.interact(hide_r, ui.scope_id().with("lhide"), Sense::click());
    if hide.hovered() {
        p.rect_filled(hide_r, 4.0, BUTTON);
    }
    let hc = hide_r.center();
    let hcol = if hide.hovered() { T1 } else { T3 };
    for (dx, dy) in [(-1.0f32, -1.0f32), (-1.0, 1.0)] {
        p.line_segment(
            [
                Pos2::new(hc.x + dx * 3.5, hc.y + dy * 3.5),
                Pos2::new(hc.x - dx * 3.5, hc.y - dy * 3.5),
            ],
            Stroke::new(1.5, hcol),
        );
    }
    if hide.clicked() {
        st.left_open = false;
        st.say(
            ui.ctx(),
            crate::i18n::t!("已收起：画布恢复全宽，点左缘「预设」再打开"),
        );
    }

    let mut y = head.bottom();

    // ---- 预设（设计稿 `.lsec h4` + `.presets`）----
    y += 6.0; // `.lsec` 的上内距 8
    let h4p = Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), 26.0));
    let h4p_r = ui.interact(h4p, ui.scope_id().with("h4preset"), Sense::click());
    p.txt(
        Pos2::new(rect.left() + 14.0, h4p.center().y),
        egui::Align2::LEFT_CENTER,
        crate::i18n::t!("预设"),
        f_group(),
        T3,
    );
    let chc = Pos2::new(rect.right() - 16.0, h4p.center().y); // C7
    if st.presets_open {
        chevron(&p, chc, 9.0, T3);
    } else {
        p.line_segment(
            [
                Pos2::new(chc.x - 2.0, chc.y - 4.5),
                Pos2::new(chc.x + 2.5, chc.y),
            ],
            Stroke::new(1.4, T3),
        );
        p.line_segment(
            [
                Pos2::new(chc.x + 2.5, chc.y),
                Pos2::new(chc.x - 2.0, chc.y + 4.5),
            ],
            Stroke::new(1.4, T3),
        );
    }
    if h4p_r.clicked() {
        st.presets_open = !st.presets_open;
    }
    y += 26.0;

    if st.presets_open {
        for (i, (name, dark, light, key)) in PRESETS.iter().enumerate() {
            let name = crate::i18n::tr(name);
            let row = Rect::from_min_size(
                Pos2::new(rect.left() + 8.0, y + i as f32 * 28.0 + 1.0),
                Vec2::new(rect.width() - 16.0, 26.0),
            );
            let resp = ui.interact(row, ui.scope_id().with(("preset", i)), Sense::click());
            let on = st.preset == i;
            // 每张卡片都有底（比面板亮一档），选中再提亮。之前只有选中行有底，
            // 其余行"躺在空白里"，读不出这是一列可选单元。
            if !on {
                p.rect_filled(row, 6.0, shade(PANEL_SIDE, 1.06));
            }
            if on {
                // C4：紫色归"当前帧/当前工具"。浮层里的选中是**列表状态**，
                // 用提亮一档的表面 + 中性细条表达，不再铺紫底。
                p.rect_filled(row, 6.0, CARD);
                p.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(row.left(), row.top() + 5.0),
                        Vec2::new(2.0, row.height() - 10.0),
                    ),
                    1.0,
                    T1,
                );
            } else if resp.hovered() {
                p.rect_filled(row, 6.0, HOVER);
            }
            // 色带：左端暗部、右端亮部。32×20 是「读得出调性」的下限——再小就
            // 只剩一个色点；描边用 8% 白而不是黑色，深色预设才不会糊进面板底。
            // 影调指纹：色带 + 叠在其上的 S 曲线。
            //
            // 纯色带只说"从暗到亮"，六枚预设看着都差不多；加一条随预设变化的
            // 影调曲线（提亮型上凸、压暗型下凹、对比型陡峭），一眼能读出调性走向。
            // 曲线用白描在色带上，不是新的彩色（守 C4）。
            //
            // A tonal curve drawn over the band: lift / sink / steep S tells the
            // preset's character at a glance, instead of six near-identical ramps.
            let band = Rect::from_min_size(
                Pos2::new(row.left() + 8.0, row.center().y - 12.0),
                Vec2::new(44.0, 24.0),
            );
            grad_rect(&p, band, &[hex_rgb(*dark), hex_rgb(*light)]);
            // 影调强度：由预设序号派生的不同曲线族（i 已在作用域内）
            let lift = match i {
                0 => 0.0,   // 原版：直线
                1 => 0.28,  // 提亮：上凸
                2 => -0.22, // 压暗：下凹
                3 => 0.42,  // 高对比：陡 S
                _ => 0.12 * ((i * 3 % 5) as f32 - 2.0),
            };
            let mut curve = Vec::with_capacity(9);
            for k in 0..9 {
                let t = k as f32 / 8.0;
                // S 曲线：t + lift·sin(π·t) 的变体，陡峭度随 lift 增大
                let y = t - lift * (std::f32::consts::PI * t).sin() * (1.0 - t * 0.3);
                curve.push(Pos2::new(
                    band.left() + 3.0 + t * (band.width() - 6.0),
                    band.bottom() - 3.0 - y.clamp(0.0, 1.0) * (band.height() - 6.0),
                ));
            }
            p.add(egui::Shape::line(
                curve,
                Stroke::new(1.6, Color32::from_white_alpha(210)),
            ));
            // 名称 + 快捷键
            let key_font = egui::FontId::proportional(FONT_META);
            let name_x = band.right() + 12.0; // C7: 色带加宽到 44 后留 12 呼吸
            let key_w = text_w(&p, key, key_font.clone());
            let max_w = row.right() - 8.0 - key_w - 12.0 - name_x; // C7
            p.txt(
                Pos2::new(name_x, row.center().y),
                egui::Align2::LEFT_CENTER,
                fit_text(&p, name, f_label(), max_w),
                f_label(),
                if on { T1 } else { T2 },
            );
            p.txt(
                Pos2::new(row.right() - 8.0, row.center().y),
                egui::Align2::RIGHT_CENTER,
                *key,
                key_font,
                if on { T3 } else { T4_TEXT },
            );
            if resp.clicked() {
                st.preset = i;
                if let Some(a) = &st.actions {
                    a.send(DemoAction::ApplyPreset(i));
                }
                st.toast_now(
                    ui.ctx(),
                    &crate::i18n::tr("已套用预设「{name}」").replace("{name}", name),
                );
            }
        }
        y += PRESETS.len() as f32 * 28.0;
    }

    // ---- 历史（设计稿 `.histrow`）----
    y += 6.0;
    p.txt(
        Pos2::new(rect.left() + 14.0, y + 13.0),
        egui::Align2::LEFT_CENTER,
        crate::i18n::t!("历史"),
        f_group(),
        T3,
    );
    p.txt(
        Pos2::new(rect.right() - 16.0, y + 13.0), // C7
        egui::Align2::RIGHT_CENTER,
        crate::i18n::tr("{n} 步").replace("{n}", &HISTORY_TOTAL.to_string()),
        f_small(),
        T4_TEXT,
    );
    y += 26.0;

    // 行数按剩余高度决定：页脚上方留 30px 给「更早」提示，不把最后一条切成半行。
    let foot_h = 48.0;
    let rows_y = y;
    let avail = (rect.bottom() - foot_h - 30.0 - rows_y).max(0.0);
    let n = ((avail / 36.0).floor() as usize).clamp(1, HISTORY.len());

    for (i, (label, when, tag)) in HISTORY.iter().enumerate().take(n) {
        let label = crate::i18n::tr(label);
        let when = crate::i18n::tr(when);
        let tag = crate::i18n::tr(tag);
        let row = Rect::from_min_size(
            Pos2::new(rect.left() + 8.0, rows_y + i as f32 * 36.0),
            Vec2::new(rect.width() - 16.0, 36.0),
        );
        let resp = ui.interact(row, ui.scope_id().with(("hist", i)), Sense::click());
        let on = st.hist == i;
        if on {
            // C4：同预设行——历史选中也是列表状态，不占紫色。
            p.rect_filled(row, 6.0, CARD);
            p.rect_filled(
                Rect::from_min_size(
                    Pos2::new(row.left(), row.top() + 6.0),
                    Vec2::new(2.0, row.height() - 12.0),
                ),
                1.0,
                T1,
            );
        } else if resp.hovered() {
            p.rect_filled(row, 6.0, HOVER);
        }
        // 圆点：当前步紫、其余灰（设计稿 `.histrow .dot`）
        p.circle_filled(
            Pos2::new(row.left() + 10.0, row.top() + 13.0),
            3.0,
            if on { T1 } else { T4 },
        );
        // 分类标签：右对齐、与标题同一水平线（原来贴在时间行，比设计稿低一行）
        let tag_font = f_small();
        let tag_w = text_w(&p, tag, tag_font.clone()) + 12.0;
        let tag_rect = Rect::from_min_size(
            Pos2::new(row.right() - 8.0 - tag_w, row.top() + 6.0),
            Vec2::new(tag_w, 16.0),
        );
        let title_x = row.left() + 24.0; // C7
        p.txt(
            Pos2::new(title_x, row.top() + 7.0),
            egui::Align2::LEFT_TOP,
            fit_text(&p, label, f_small(), tag_rect.left() - 8.0 - title_x),
            f_small(),
            if on { T1 } else { T2 },
        );
        p.txt(
            Pos2::new(title_x, row.top() + 22.0),
            egui::Align2::LEFT_TOP,
            when,
            f_small(),
            T4_TEXT,
        );
        p.rect_filled(
            tag_rect,
            4.0, // C6: 3 不在圆角档位
            // C4: 分类标签是列表状态，不是"当前帧"——用底衬明度区分，不用紫。
            if on { CARD } else { BUTTON },
        );
        p.txt(
            tag_rect.center(),
            egui::Align2::CENTER_CENTER,
            tag,
            tag_font,
            if on { T1 } else { T3 },
        );
        if resp.clicked() {
            st.hist = i;
        }
        resp.on_hover_text(label);
    }

    // 列表底部：还有更早的步骤 —— 把「下面空着」变成一条有意义的边界，
    // 而不是留在面板里的一整块死白（源程序 `tabs.rs` 也是用 note() 收尾）。
    let note_y = rect.bottom() - foot_h - 15.0;
    p.hline(
        (rect.left() + 8.0)..=(rect.right() - 8.0),
        note_y - 12.0,
        // C5: 内缩短线也会读数，改成暗线（比底更暗）。
        Stroke::new(1.0, Color32::from_black_alpha(72)),
    );
    p.txt(
        Pos2::new(rect.center().x, note_y),
        egui::Align2::CENTER_CENTER,
        crate::i18n::tr("更早 {n} 步 · 滚轮查看")
            .replace("{n}", &HISTORY_TOTAL.saturating_sub(n).to_string()),
        f_small(),
        T4_TEXT,
    );

    // 左栏页脚（设计稿 `.lfoot`）：撤销一步 / 回到此处，两个等宽 32px 按钮。
    // 没有它时历史列表下面空着 340px（面板高度的 40%），是整屏最大的一块死白。
    // 设计稿把这块交给两个高频动作——空着不是「留白」，是没画完。
    //
    // Left footer from the mockup: two equal 32 px buttons. Without it the history
    // list left 340 px of dead space (40% of the panel height) -- the largest blank
    // area on screen. The mockup puts two high-frequency actions there.
    {
        let fh = 48.0;
        let fy = rect.bottom() - fh;
        p.hline(
            rect.x_range(),
            fy,
            Stroke::new(1.0, Color32::from_black_alpha(107)),
        );
        // 按钮四周留 12（= 浮层圆角）：留 10 时按钮的直角边会切进圆角弧里，
        // 底部两角看起来像"面板没包住按钮"。
        //
        // 12 px inset matches the panel radius; at 10 the button's square corners
        // cut into the arc and read as a leak at the bottom edge.
        let bw = (rect.width() - 24.0 - 6.0) / 2.0;
        for (i, label) in [crate::i18n::t!("撤销一步"), crate::i18n::t!("回到此处")]
            .iter()
            .enumerate()
        {
            let r = Rect::from_min_size(
                Pos2::new(rect.left() + 12.0 + i as f32 * (bw + 6.0), fy + 7.0),
                Vec2::new(bw, 32.0),
            );
            let resp = ui.interact(r, ui.scope_id().with(("lfoot", i)), Sense::click());
            p.rect_filled(
                r,
                8.0,
                if resp.hovered() {
                    Color32::from_rgb(0x4C, 0x4C, 0x4C)
                } else {
                    BUTTON
                },
            );
            p.txt(
                r.center(),
                egui::Align2::CENTER_CENTER,
                *label,
                f_bold(FONT_SMALL),
                T2,
            );
            if resp.clicked() {
                st.say(
                    ui.ctx(),
                    &crate::i18n::tr("「{label}」：原型未接历史栈").replace("{label}", label),
                );
            }
        }
    }
}

// ---------------------------------------------------------------- 画布

fn draw_canvas(ui: &mut egui::Ui, st: &mut Proto) {
    let rect = ui.max_rect();
    let p = ui.painter().clone();
    // 井的底色、圆角与内阴影由 `ui()` 画。
    // The well's fill, radius and inner shadow are drawn in `ui()`.

    let resp = ui.interact(rect, ui.scope_id().with("canvas"), Sense::click_and_drag());

    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.1 {
            let before = st.zoom;
            st.zoom = (st.zoom * (1.0 + scroll * 0.0015)).clamp(0.25, 4.0);
            if let Some(c) = resp.hover_pos() {
                let center = rect.center() + st.pan;
                st.pan += (c - center) * (1.0 - st.zoom / before);
            }
        }
    }
    if resp.dragged() && resp.drag_delta() != Vec2::ZERO {
        st.pan += resp.drag_delta();
    }
    if resp.double_clicked() {
        st.zoom = 1.0;
        st.pan = Vec2::ZERO;
        st.say(ui.ctx(), crate::i18n::t!("已适应窗口"));
    }

    let avail = rect.shrink(24.0);
    // 上限从 960 提到 1180：左栏收起后画布有 1244px，仍按 960 封顶的话
    // 「收起左栏」只换来两边更多黑边——照片一点没大，等于白收。
    //
    // Raised from 960 to 1180: with the left column gone the canvas holds
    // 1244 px, and keeping the old cap would buy nothing but wider black
    // margins -- the photo itself would not grow a single pixel.
    let base_w = avail.width().min(1180.0);
    let mut w = base_w * st.zoom;
    let mut h = w / 1.5;
    if h > avail.height() * st.zoom {
        h = avail.height() * st.zoom;
        w = h * 1.5;
    }
    let frame = Rect::from_center_size(rect.center() + st.pan, Vec2::new(w, h));

    // 旧版是「光桌」：八层暖白（0xFFF4E2）从照片边缘向外递减，想让照片像
    // 唯一光源一样发光。实测它在暖黑底上叠出一条**粉橙色脏晕**——暖白加暖底
    // 还是暖白，紧贴照片彩色内容时读成"画面颜色渗出来了"（用户点名：颜色太丑）。
    //
    // 改为深色界面对媒体对象的标准处理：三层**中性黑影**，照片靠影浮起、
    // 边缘干净利落，画布上没有任何彩色溢出：
    //   ① 环境影 —— 大而淡，把照片从井底托起一档；
    //   ② 投影 —— 下移 10px，读出"照片悬浮在井上方"；
    //   ③ 接触影 —— 紧贴边缘的一圈深影，给照片厚度。
    // 黑色只会压暗底色、不引入色相（混黑保中性），脏色问题从根上消失。
    //
    // The old "light table" spilled eight layers of warm white from the photo
    // edge; over the warm-black well they compounded into a pinkish-brown halo
    // that read as the image bleeding colour. Replaced with the standard dark-UI
    // treatment for media: three neutral black shadows (ambient / cast / contact)
    // lift the photo with a clean edge -- black darkens the warm ground without
    // introducing any hue, so the dirty tint is gone at the root.
    p.rect_filled(frame.expand(14.0), 8.0, Color32::from_black_alpha(46));
    p.rect_filled(
        frame.translate(Vec2::new(0.0, 10.0)).expand(4.0),
        6.0,
        Color32::from_black_alpha(64),
    );
    p.rect_filled(
        frame.translate(Vec2::new(0.0, 4.0)).expand(1.0),
        2.0,
        Color32::from_black_alpha(90),
    );
    if let Some(t) = &st.photo {
        p.image(
            t.id(),
            frame,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        p.rect_filled(frame, 3.0, Color32::from_rgb(0x31, 0x31, 0x31)); // C6
        // 空态接真应用：打开文件夹 / 选照片（demo 独立运行时保留静态占位）。
        //
        // Empty state wired to the real app: open folder / pick photos.
        if st.actions.is_some() {
            let bw = 128.0;
            let by = frame.center().y + 18.0;
            for (i, (label, action)) in [
                (crate::i18n::t!("打开文件夹"), DemoAction::OpenFolder),
                (crate::i18n::t!("选照片…"), DemoAction::OpenPhotos),
            ]
            .into_iter()
            .enumerate()
            {
                let r = Rect::from_min_size(
                    Pos2::new(
                        frame.center().x + (i as f32 * 2.0 - 1.0) * (bw / 2.0 + 5.0),
                        by,
                    ),
                    Vec2::new(bw, 32.0),
                );
                let resp = ui.interact(r, ui.scope_id().with(("empty-open", i)), Sense::click());
                p.rect_filled(
                    r,
                    6.0,
                    if i == 0 {
                        if resp.hovered() {
                            Color32::from_rgb(0xB8, 0x9E, 0xFB)
                        } else {
                            ACCENT
                        }
                    } else if resp.hovered() {
                        Color32::from_rgb(0x4A, 0x4A, 0x4A)
                    } else {
                        BUTTON
                    },
                );
                p.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    f_bold(FONT_SMALL),
                    if i == 0 {
                        Color32::from_rgb(0x18, 0x18, 0x18)
                    } else {
                        T1
                    },
                );
                if resp.clicked() {
                    if let Some(a) = &st.actions {
                        a.send(action);
                    }
                }
            }
        }
    }
    // 顶部光边：1px 白，只在照片存在时画（空态的占位块不是光源）。
    // 它是"相纸边缘反射顶光"的信号——很细，但在 25% 缩略图下仍给照片一条清晰上沿。
    if st.photo.is_some() {
        p.hline(
            (frame.left() + 1.0)..=(frame.right() - 1.0),
            frame.top() + 0.5,
            Stroke::new(1.0, Color32::from_white_alpha(26)), // M2: 46 太亮成相框，压到 26
        );
    }
    for i in 1..3 {
        let x = frame.left() + frame.width() * i as f32 / 3.0;
        let y = frame.top() + frame.height() * i as f32 / 3.0;
        p.vline(
            x,
            frame.y_range(),
            Stroke::new(1.0, Color32::from_white_alpha(22)),
        );
        p.hline(
            frame.x_range(),
            y,
            Stroke::new(1.0, Color32::from_white_alpha(22)),
        );
    }

    // 空态没有照片：不画蒙版圆和控制点（空画布上一枚孤零零的圆读成 bug）。
    // No photo: skip the mask circle and handles -- a lone circle on an empty
    // well reads as a rendering bug.
    if st.photo.is_some() {
        let mc = Pos2::new(
            frame.left() + frame.width() * 0.56,
            frame.top() + frame.height() * 0.30,
        );
        let mr = 125.0 * st.zoom.min(1.6);
        // 蒙版圆：从"一条白线 + 四个白方块"改成真正的可操作对象。
        //   ① 十字引导线（虚线，很淡）——标出圆心，让手知道这个圆可以被移动；
        //   ② 圆本身——白 55% 细线，不抢照片；
        //   ③ 四个手柄——投影 + 实心圆 + 高光，读作能拖的点，而不是四个色块。
        //
        // A mask circle with crosshair guides and volume handles: it reads as something
        // you can grab, not a white line with four squares.
        // 引导线：只在圆内的一段，虚线由小段组成（egui 无原生 dash）。
        // 槽位居中：把直径分成 8 槽，每槽画中间 50%——首段离左端和末段离右端
        // 等距，虚线图案对圆心严格对称（旧写法末段差 1/16，读成中心偏移）。
        // alpha 38 → 26：引导线只该被"感觉到"，不该和中心十字抢亮度。
        //
        // Slot-centred dashes: split the diameter into 8 slots, draw the middle 50%
        // of each -- the pattern is symmetric about the centre (the old version ran
        // 1/16 short on the last dash, which read as an off-centre crosshair).
        // Alpha 38 → 26: guides should be felt, not compete with the centre cross.
        {
            let slots = 8.0f32;
            let s = 2.0 * mr / slots;
            for k in 0..8 {
                let a = mc.x - mr + s * (k as f32 + 0.25);
                let b = mc.x - mr + s * (k as f32 + 0.75);
                p.line_segment(
                    [Pos2::new(a, mc.y), Pos2::new(b, mc.y)],
                    Stroke::new(1.0, Color32::from_white_alpha(26)),
                );
                let c0 = mc.y - mr + s * (k as f32 + 0.25);
                let c1 = mc.y - mr + s * (k as f32 + 0.75);
                p.line_segment(
                    [Pos2::new(mc.x, c0), Pos2::new(mc.x, c1)],
                    Stroke::new(1.0, Color32::from_white_alpha(26)),
                );
            }
        }
        p.circle_stroke(mc, mr, Stroke::new(1.0, Color32::from_white_alpha(140)));
        // 中心十字标记：180 → 120，十字是定位记号不是焦点。
        p.line_segment(
            [Pos2::new(mc.x - 6.0, mc.y), Pos2::new(mc.x + 6.0, mc.y)],
            Stroke::new(1.2, Color32::from_white_alpha(120)),
        );
        p.line_segment(
            [Pos2::new(mc.x, mc.y - 6.0), Pos2::new(mc.x, mc.y + 6.0)],
            Stroke::new(1.2, Color32::from_white_alpha(120)),
        );
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            let c = Pos2::new(mc.x + dx * mr, mc.y + dy * mr);
            p.circle_filled(
                Pos2::new(c.x, c.y + 1.0),
                5.5,
                Color32::from_black_alpha(110),
            );
            // 手柄 235 → 214、高光 WHITE → 190：手柄仍要浮在照片上，但四个纯白点
            // 在夜景照片上是画面里最亮的物体，比照片本身的高光还抢眼。
            //
            // Handles 235 → 214, highlight 255 → 190: on a night photo four pure
            // white dots out-shone the image's own highlights.
            p.circle_filled(c, 5.0, Color32::from_white_alpha(214));
            p.circle_filled(
                Pos2::new(c.x - 1.0, c.y - 1.0),
                2.0,
                Color32::from_white_alpha(190),
            );
        }
    }

    // 缩放控件拆成两段：左边是**读数**（当前百分比，点一下回到 100%），
    // 右边是**动作**（适应窗口）。原来把两者塞进一句话「100% · 适应窗口」并让整块
    // 点击都执行「适应窗口」——点数字却触发别的动作，是最典型的一类误操作面。
    //
    // Zoom split in two: the left half is the readout (clicking returns to 100%), the
    // right half is the action (fit to window). It used to be one sentence with the
    // whole pill wired to "fit", so clicking the number did something else entirely.
    // C7: 14/34/6 归到 16/32/8。C10: 底衬用 CHROME token，不硬编码。
    let chip = Rect::from_min_size(
        Pos2::new(rect.left() + 16.0, rect.bottom() - 32.0),
        Vec2::new(158.0, 24.0),
    );
    p.rect_filled(
        chip.translate(Vec2::new(0.0, 3.0)),
        8.0,
        Color32::from_black_alpha(60),
    );
    p.rect_filled(chip, 8.0, CHROME);
    glass_edge(&p, chip, 8.0);
    let split = chip.left() + 56.0; // C7: 54 → 56
    p.vline(
        split,
        (chip.top() + 8.0)..=(chip.bottom() - 8.0),
        Stroke::new(1.0, Color32::from_black_alpha(120)),
    );
    // 百分比必须是**真实比例** frame.width / 源图宽，不是 `zoom * 100`：
    // zoom=1 是「适应窗口」，fit 之后照片往往只有 92%（1180 / 1280），
    // 写死 100% 就是骗读数，而且会让「100%」和「适应窗口」两个按钮看着一样。
    //
    // The percentage is the real frame-to-source ratio, not `zoom * 100`:
    // zoom 1 means "fit", which lands at 92% for a 1180-wide frame in a 1280-wide
    // photo. Hard-coding 100% fakes the reading and makes both buttons identical.
    let fit_pct = frame.width() / PHOTO_W * 100.0;
    let zoom_lbl = format!("{}%", fit_pct.round() as i32);
    let left = Rect::from_min_max(chip.min, Pos2::new(split, chip.bottom()));
    let right = Rect::from_min_max(Pos2::new(split, chip.top()), chip.max);
    let lr = ui.interact(left, ui.scope_id().with("zoom-100"), Sense::click());
    let rr = ui.interact(right, ui.scope_id().with("zoom-fit"), Sense::click());
    if lr.hovered() {
        p.rect_filled(left, 8.0, Color32::from_rgb(0x42, 0x42, 0x42));
    }
    if rr.hovered() {
        p.rect_filled(right, 8.0, Color32::from_rgb(0x42, 0x42, 0x42));
    }
    p.txt(
        left.center(),
        egui::Align2::CENTER_CENTER,
        zoom_lbl,
        f_bold(FONT_SMALL),
        if lr.hovered() { T1 } else { T2 },
    );
    p.txt(
        right.center(),
        egui::Align2::CENTER_CENTER,
        crate::i18n::t!("适应窗口"),
        f_small(),
        if rr.hovered() { T1 } else { T3 },
    );
    if lr.clicked() {
        st.zoom = 1.0;
        st.pan = Vec2::ZERO;
        st.say(ui.ctx(), crate::i18n::t!("缩放 100%"));
    }
    if rr.clicked() {
        st.zoom = 1.0;
        st.pan = Vec2::ZERO;
        st.say(ui.ctx(), crate::i18n::t!("已适应窗口"));
    }

    // 收起态的入口在工具轨第一项（「预设」），画布内不再放第二枚按钮：
    // 原来左上角并排着「对话」和一枚方形「打开侧栏」，两个来路不明的方块。
    //
    // The collapsed entry lives in the rail's first item; no second button inside
    // the canvas.

    // AI 命令条：画布顶部浮层（2026 "AI 信任态 / prompt bar" 趋势的 egui 落法）。
    // 必须在浮层列表之前画，层级与底部工具条对称：一上一下，中间留给照片。
    //
    // AI command bar: floating chip at the top of the canvas (the egui take on
    // the 2026 "AI trust state / prompt bar" trend). Drawn before the left
    // overlay, mirroring the bottom tool bar -- one above, one below, photo in
    // between.
    draw_ai_cmdbar(ui, &p, rect, st);

    draw_canvas_tools(ui, &p, rect, st);

    // 浮层最后画：盖在照片与蒙版控制点之上。
    if st.left_open {
        let w = LEFT_W;
        let h = (rect.height() - 24.0).min(LEFT_H);
        // 160ms 位移入场：浮层从上方 10px 落到位（透明度不动，只动位置——
        // egui 没有全局 alpha，逐色相乘会把 300 处颜色全改一遍）。
        //
        // `shot=` 强制终态：截图若抓到过渡中间帧，"浮层位置"这条验收就永远对不上。
        //
        // 160 ms positional entry. `shot=` forces the end state -- a screenshot that
        // catches a mid-transition frame makes the layout unverifiable.
        let t = if st.shot.is_some() {
            1.0
        } else {
            ui.ctx()
                .animate_bool_with_time(ui.scope_id().with("leftanim"), true, 0.16)
        };
        let dy = (1.0 - t) * -10.0;
        let panel = Rect::from_min_size(
            Pos2::new(rect.left() + 12.0, rect.top() + 12.0 + dy),
            Vec2::new(w, h),
        );
        draw_left(ui, panel, st);
    }
}

// ---------------------------------------------------------------- 右坞

fn draw_dock(ui: &mut egui::Ui, st: &mut Proto) {
    let rect = ui.max_rect();
    let p = ui.painter().clone();
    // 右坞同样是一块浮板：左缘竖线删掉，靠间隙与桌面色分界。
    // Same for the dock: the left rule is replaced by a gap and the desk colour.

    let tabs_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), DOCK_TABS_H));
    if let Some(i) = tab_row(
        &p,
        ui,
        tabs_rect,
        &[
            crate::i18n::t!("基本"),
            crate::i18n::t!("色彩"),
            crate::i18n::t!("效果"),
        ],
        st.dock_tab,
        "docktab",
        TabStyle::Dock,
        st.shot.is_some(),
    ) {
        st.dock_tab = i;
        st.dock_scroll = 0.0;
    }
    // C5：页签下方那条贯穿全宽的 HAIRLINE 删了。页签自己有 2px 下划线在说
    // "当前是哪个"，再来一条等重的线只会把右坞切成上下两格；直方图容器本身的
    // 边界 + 12px 间距已经足够分隔。
    //
    // The full-width rule under the tabs is gone: the 2 px tab underline already
    // says "which one", and a second full-width line just makes a box.
    //
    // C7: 10/14/28 are off-scale; 12/16/32 are on it.
    let hist = Rect::from_min_size(
        Pos2::new(rect.left() + 16.0, tabs_rect.bottom() + 12.0),
        Vec2::new(rect.width() - 32.0, 84.0),
    );
    draw_histogram(ui, &p, hist);
    // 刻度 + RGB 读数
    let ty = hist.bottom() + 4.0;
    for (i, m) in ["0", "50", "100"].iter().enumerate() {
        let x = hist.left() + hist.width() * i as f32 / 2.0;
        p.txt(
            Pos2::new(x, ty),
            if i == 0 {
                egui::Align2::LEFT_TOP
            } else if i == 2 {
                egui::Align2::RIGHT_TOP
            } else {
                egui::Align2::CENTER_TOP
            },
            *m,
            f_small(),
            T4_TEXT,
        );
    }
    // RGB 读数：按直方图宽度三等分，色块与数字共用一条水平中线。
    // 之前用固定 62px 步长 —— 右半空出一大片（两个读数宽），左侧三个数字挤在一起，
    // 色块又比数字低 2px，整块看着像没对齐的表格。
    //
    // RGB readouts split the histogram width into three equal columns, swatch and
    // number sharing one centre line. A fixed 62 px stride left the right half empty,
    // crammed the three numbers together and dropped the swatches 2 px below the text.
    let ry = ty + 16.0;
    let colw = hist.width() / 3.0;
    for (i, (c, v)) in [
        (Color32::from_rgb(0xE0, 0x6A, 0x5C), "128"),
        (Color32::from_rgb(0x6C, 0xC0, 0x6A), "142"),
        (Color32::from_rgb(0x5C, 0x8A, 0xE0), "96"),
    ]
    .iter()
    .enumerate()
    {
        // 每组（色块 + 数字）在各自三等分里**居中**：左对齐会让第三组把右侧三分之一
        // 空着，而 100 刻度还在最右边，读起来像少了两个读数。
        // Each swatch+number pair is centred inside its third: left-aligning left the
        // last third empty while the 100 tick still sat at the far right.
        let font = f_small();
        let tw = text_w(&p, v, font.clone());
        let x0 = hist.left() + (i as f32 + 0.5) * colw - (12.0 + tw) / 2.0;
        let cy_r = ry + 7.0;
        p.rect_filled(
            Rect::from_center_size(Pos2::new(x0 + 4.0, cy_r), Vec2::splat(8.0)),
            3.0,
            *c,
        );
        p.txt(
            Pos2::new(x0 + 12.0, cy_r),
            egui::Align2::LEFT_CENTER,
            *v,
            font,
            T2,
        );
    }

    let body = Rect::from_min_max(
        Pos2::new(rect.left(), ry + 16.0),
        Pos2::new(rect.right(), rect.bottom()),
    );
    let body_resp = ui.interact(body, ui.scope_id().with("dockbody"), Sense::hover());
    if body_resp.hovered() {
        let sc = ui.input(|i| i.smooth_scroll_delta.y);
        if sc.abs() > 0.1 {
            st.dock_scroll = (st.dock_scroll - sc).max(0.0);
        }
    }

    let content_h = match st.dock_tab {
        0 => 1100.0,
        1 => {
            if st.mixer_tab == 1 {
                360.0
            } else if st.prop == 3 {
                8.0 * (20.0 + 3.0 * 28.0) + 190.0
            } else {
                420.0
            }
        }
        _ => 280.0,
    };
    st.dock_scroll = st.dock_scroll.min((content_h - body.height()).max(0.0));

    let y = body.top() + 12.0 - st.dock_scroll;
    match st.dock_tab {
        0 => {
            dock_basic(ui, &p, rect, y, st);
        }
        1 => {
            dock_color(ui, &p, rect, y, st);
        }
        _ => {
            dock_effect(ui, &p, rect, y, st);
        }
    }

    if content_h > body.height() {
        let frac = body.height() / content_h;
        let bar_h = (body.height() * frac).max(30.0);
        let max_scroll = (content_h - body.height()).max(1.0);
        let t = (st.dock_scroll / max_scroll).clamp(0.0, 1.0);
        let bx = rect.right() - 4.0;
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(bx, body.top() + t * (body.height() - bar_h)),
                Vec2::new(2.0, bar_h),
            ),
            1.0,
            Color32::from_rgb(0x50, 0x50, 0x50),
        );
    }
}

fn dock_basic(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, y0: f32, st: &mut Proto) {
    let mut y = group_head(
        ui,
        p,
        rect,
        y0,
        crate::i18n::t!("光"),
        crate::i18n::t!("复位"),
        st,
    );
    let labels = [
        crate::i18n::t!("曝光"),
        crate::i18n::t!("对比度"),
        crate::i18n::t!("高光"),
        crate::i18n::t!("阴影"),
        crate::i18n::t!("白色色阶"),
        crate::i18n::t!("黑色色阶"),
    ];
    for (i, label) in labels.iter().enumerate() {
        let digits = usize::from(i == 0);
        let v = &mut st.basic[i];
        y = slider_row(
            ui,
            p,
            rect,
            y,
            "basic",
            label,
            v,
            -100.0,
            100.0,
            0.0,
            digits * 2,
            None,
        );
    }
    y = group_head(
        ui,
        p,
        rect,
        y + 4.0,
        crate::i18n::t!("白平衡"),
        crate::i18n::t!("自动"),
        st,
    );
    y = slider_row(
        ui,
        p,
        rect,
        y,
        "wb",
        crate::i18n::t!("色温"),
        &mut st.wb_temp,
        -100.0,
        100.0,
        0.0,
        0,
        Some(&GRAD_TEMP),
    );
    y = slider_row(
        ui,
        p,
        rect,
        y,
        "wb",
        crate::i18n::t!("色调"),
        &mut st.wb_tint,
        -100.0,
        100.0,
        0.0,
        0,
        Some(&GRAD_TINT),
    );

    // 存在感
    y = group_head(
        ui,
        p,
        rect,
        y + 4.0,
        crate::i18n::t!("存在感"),
        crate::i18n::t!("复位"),
        st,
    );
    let presence = [
        crate::i18n::t!("纹理"),
        crate::i18n::t!("清晰度"),
        crate::i18n::t!("去朦胧"),
        crate::i18n::t!("鲜艳度"),
        crate::i18n::t!("饱和度"),
    ];
    for (i, label) in presence.iter().enumerate() {
        let v = &mut st.presence[i];
        y = slider_row(
            ui, p, rect, y, "presence", label, v, -100.0, 100.0, 0.0, 0, None,
        );
    }

    // 延时序列
    y = group_head(
        ui,
        p,
        rect,
        y + 4.0,
        crate::i18n::t!("延时序列"),
        crate::i18n::t!("锚帧 第 12 帧"),
        st,
    );
    y = slider_row(
        ui,
        p,
        rect,
        y,
        "tl",
        crate::i18n::t!("去闪强度"),
        &mut st.deflicker,
        -100.0,
        100.0,
        0.0,
        0,
        None,
    );
    y = slider_row(
        ui,
        p,
        rect,
        y,
        "tl",
        crate::i18n::t!("平滑窗口"),
        &mut st.smooth_win,
        0.0,
        100.0,
        50.0,
        0,
        None,
    );

    // 渲染输出
    y = group_head(ui, p, rect, y + 4.0, crate::i18n::t!("渲染输出"), "", st);
    for (k, v) in [
        (crate::i18n::t!("分辨率"), "4K · 3840 × 2160"),
        (crate::i18n::t!("帧率"), crate::i18n::t!("24 帧/秒")),
        (crate::i18n::t!("格式"), crate::i18n::t!("H.264 · 高画质")),
    ] {
        p.txt(
            Pos2::new(rect.left() + 28.0, y + 8.0),
            egui::Align2::LEFT_TOP,
            k,
            f_small(),
            T3,
        );
        p.txt(
            Pos2::new(rect.right() - 16.0, y + 8.0), // C7
            egui::Align2::RIGHT_TOP,
            v,
            f_small(),
            T1,
        );
        y += 26.0;
    }
}

fn dock_color(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, y0: f32, st: &mut Proto) {
    let mut y = group_head(
        ui,
        p,
        rect,
        y0,
        crate::i18n::t!("白平衡"),
        crate::i18n::t!("自动"),
        st,
    );
    y = slider_row(
        ui,
        p,
        rect,
        y,
        "wb",
        crate::i18n::t!("色温"),
        &mut st.wb_temp,
        -100.0,
        100.0,
        0.0,
        0,
        Some(&GRAD_TEMP),
    );
    y = slider_row(
        ui,
        p,
        rect,
        y,
        "wb",
        crate::i18n::t!("色调"),
        &mut st.wb_tint,
        -100.0,
        100.0,
        0.0,
        0,
        Some(&GRAD_TINT),
    );
    mixer(ui, p, rect, y + 2.0, st);
}

/// 颜色混合器：一级 tab（HSL / 黑白）→ 属性 tab（色相/饱和度/明亮度/全部）→ 八行。
fn mixer(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, y0: f32, st: &mut Proto) {
    let mut y = group_head(
        ui,
        p,
        rect,
        y0,
        crate::i18n::t!("颜色混合器"),
        crate::i18n::t!("复位"),
        st,
    );

    let h1 = Rect::from_min_size(
        Pos2::new(rect.left() + 14.0, y),
        Vec2::new(rect.width() - 28.0, 26.0),
    );
    if let Some(i) = tab_row(
        p,
        ui,
        h1,
        &["HSL", crate::i18n::t!("黑白")],
        st.mixer_tab,
        "mix1",
        TabStyle::Mixer,
        st.shot.is_some(),
    ) {
        st.mixer_tab = i;
    }
    y = h1.bottom() + 4.0;

    if st.mixer_tab == 0 {
        let h2 = Rect::from_min_size(
            Pos2::new(rect.left() + 14.0, y),
            Vec2::new(rect.width() - 28.0, 24.0),
        );
        if let Some(i) = tab_row(
            p,
            ui,
            h2,
            &PROP_NAMES,
            st.prop,
            "mix2",
            TabStyle::Mixer,
            st.shot.is_some(),
        ) {
            st.prop = i;
        }
        y = h2.bottom() + 6.0;

        if st.prop == 3 {
            for band in 0..8 {
                p.txt(
                    Pos2::new(rect.left() + 20.0, y + 9.0),
                    egui::Align2::LEFT_CENTER,
                    BAND_NAMES[band],
                    f_small(),
                    T1,
                );
                y += 20.0;
                for prop in 0..3 {
                    let label = PROP_NAMES[prop];
                    let v = match prop {
                        0 => &mut st.hsl_h[band],
                        1 => &mut st.hsl_s[band],
                        _ => &mut st.hsl_l[band],
                    };
                    y = band_slider(ui, p, rect, y, band, prop, label, v, true);
                }
            }
            return;
        }

        for band in 0..8 {
            let prop = st.prop;
            let v = match prop {
                0 => &mut st.hsl_h[band],
                1 => &mut st.hsl_s[band],
                _ => &mut st.hsl_l[band],
            };
            y = band_slider(ui, p, rect, y, band, prop, BAND_NAMES[band], v, true);
        }
    } else {
        for band in 0..8 {
            let v = &mut st.mono[band];
            y = band_slider(ui, p, rect, y, band, 0, BAND_NAMES[band], v, false);
        }
    }
}

fn dock_effect(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, y0: f32, st: &mut Proto) {
    let mut y = group_head(
        ui,
        p,
        rect,
        y0,
        crate::i18n::t!("效果"),
        crate::i18n::t!("复位"),
        st,
    );
    let labels = [
        crate::i18n::t!("去朦胧"),
        crate::i18n::t!("颗粒"),
        crate::i18n::t!("暗角"),
    ];
    for (i, label) in labels.iter().enumerate() {
        let v = &mut st.effects[i];
        y = slider_row(ui, p, rect, y, "fx", label, v, -100.0, 100.0, 0.0, 0, None);
    }
}

// ---------------------------------------------------------------- 胶片条

/// 底部胶片条 + 时间轴三车道（与效果图同规格）：
/// filmhead 36 → 缩略图 50 → 关键帧车道 18 → 逐帧亮度 21 → 刻度 13。
fn draw_film(ui: &mut egui::Ui, st: &mut Proto) {
    let rect = ui.max_rect();
    let p = ui.painter().clone();
    let photo = st.photo.clone();
    // 底色 `STRIP` 与顶部分界线由 `ui()` 画；这里只画内容。
    // The strip's fill and top seam are drawn in `ui()`.

    // ---- filmhead 36 ----
    let head = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 36.0));
    let cy = head.center().y;
    // C5：胶片头下方的全宽亮线删了。缩略图行本身有 4px 间隙在分隔，这条线
    // 只是在标题和内容之间再画一道"格子"。
    //
    // C5: no full-width rule under the film header -- the thumbnail gaps separate
    // the two regions already.
    p.txt(
        Pos2::new(head.left() + 14.0, cy),
        egui::Align2::LEFT_CENTER,
        crate::i18n::t!("48 张 · 已选 1"),
        f_title(),
        T1,
    );
    // 星级
    let mut sx = head.left() + 138.0;
    for i in 0..5 {
        let c = Pos2::new(sx, cy);
        let zone = Rect::from_center_size(c, Vec2::splat(18.0));
        let resp = ui.interact(zone, ui.scope_id().with(("rate", i)), Sense::click());
        let lit = i < st.rating as usize;
        let hot = resp.hovered();
        star(&p, c, 6.0, if lit || hot { T2 } else { T4 }, lit);
        if resp.clicked() {
            if let Some(a) = &st.actions {
                a.send(DemoAction::SetRating(if st.rating == (i + 1) as u8 {
                    0
                } else {
                    (i + 1) as u8
                }));
            }
        }
        sx += 15.0;
    }
    // 「选用 / 已选 / 排除」三个旗标删除：它们和左边的「48 张 · 已选 1」、
    // 以及星级说的是同一件事（这一帧要不要进导出），一行里出现三遍。
    // 导出筛选在 demo 里也没有真实行为，留着只是噪点。
    //
    // The three flag chips are gone: they repeat what 「48 张 · 已选 1」 and the
    // star rating already say, three times in one row.

    // 右侧：当前帧读数
    p.txt(
        Pos2::new(head.right() - 14.0, cy),
        egui::Align2::RIGHT_CENTER,
        crate::i18n::t!("第 {n} 帧 / 48 · 24 帧/秒").replace("{n}", &(st.film + 1).to_string()),
        f_small(),
        T3,
    );

    // ---- 时间轴三车道 ----
    // C7: 2/6 are off-scale; 4/8 are on it.
    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + 16.0, head.bottom() + 4.0),
        Pos2::new(rect.right() - 16.0, rect.bottom() - 8.0),
    );
    let mut y = inner.top();

    // 缩略图行 50
    // 固定高度 56：宽度按照片比例算，比例 1.29 的素材得到 ~72px（50 太矮会显窄条）
    let thumbs = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), 56.0));
    let gap = 4.0; // C7: 缩略图间隙从 6 归到 4px 档
    // 缩略图**固定高度**、宽度按照片真实宽高比算：
    // 原来写死 96x50(1.92:1) 把 3:2 的照片压扁了（用户报「不应该是长方形」）。
    let ratio = st
        .photo
        .as_ref()
        .map(|t| {
            let s = t.size_vec2();
            if s.y > 1.0 { s.x / s.y } else { 1.5 }
        })
        .unwrap_or(1.5);
    let tw = (thumbs.height() * ratio).round();
    let bw = tw + gap;
    // 帧数：demo 独立运行时用 48 张假数据；桥接进真应用后用真工程帧数。
    //
    // Frame count: 48 fake frames in the standalone demo, the real sequence
    // length once bridged into the app.
    let frames_count = if st.actions.is_some() {
        st.frames.max(1) as usize
    } else {
        48
    };
    // ---- 缩略图与时间轴共用一套 x 映射（这是整个胶片条唯一的事实源）----
    //
    // 视觉审查抓到的硬伤：缩略图一屏只排得下 ~19 张，却按「第 i 张 = 第 i 帧」
    // 排；时间轴按 48 帧铺满整条宽度。于是当前帧竖线（按 48 帧定位）落在第 4 张
    // 缩略图的左边，两根线互相矛盾，谁都不在选中的那张图上。
    //
    // 现在只有一个映射：第 f 帧的 x = 左边界 - 滚动量 + f × (缩略图宽 + 间隙)。
    // 缩略图、关键帧、亮度带、刻度、游标全部由它定位，滚动量让当前帧居中；
    // 超出可视区的部分用裁剪矩形切掉（而不是「只画能放下的那一屏」）。
    //
    // One x mapping for the whole strip. Vision review caught the real defect: the
    // row only fits ~19 thumbs but indexed them 1:1 as frames, while the timeline
    // spread all 48 frames across the full width -- so the playhead (48-frame
    // mapping) landed left of the 4th thumb and the two disagreed. Now every lane
    // derives from a single mapping plus a scroll offset that centres the current
    // frame; anything outside the viewport is clipped instead of silently dropped.
    let total = bw * frames_count as f32;
    let max_off = (total - inner.width()).max(0.0);
    let off = (st.film as f32 * bw - (inner.width() - tw) * 0.5).clamp(0.0, max_off);
    let fx = |f: f32| inner.left() - off + f * bw;
    let clip = Rect::from_min_max(
        Pos2::new(inner.left(), thumbs.top() - 2.0),
        Pos2::new(inner.right(), rect.bottom() - 4.0),
    );
    let tp = p.with_clip_rect(clip);

    for i in 0..frames_count {
        let x = fx(i as f32);
        if x + tw < inner.left() - 1.0 || x > inner.right() + 1.0 {
            continue;
        }
        let cell = Rect::from_min_size(Pos2::new(x, thumbs.top()), Vec2::new(tw, thumbs.height()));
        let resp = ui.interact(cell, ui.scope_id().with(("thumb", i)), Sense::click());
        // 缩略图圆角 8 → 4：48×56 的小块上，8 已经读成「药丸」，而它只是张缩略图。
        // 缩略图卡片：照片缩在卡片内 2px，露出一圈底衬——从"连续照片带"变成
        // "一张张卡片"，每张有边界、有间隙，读作可独立选择的序列单元。
        //
        // Each thumb is a card with the image inset 2 px: a sequence of selectable
        // units rather than a continuous film strip.
        tp.rect_filled(cell, 5.0, FIELD);
        let photo_cell = cell.shrink(2.0);
        let real_thumb = st.thumb_source.as_ref().and_then(|f| f(i));
        let draw_tex = real_thumb.as_ref().or(photo.as_ref());
        if let Some(t) = draw_tex {
            tp.image(
                t.id(),
                photo_cell,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                if i == st.film {
                    Color32::WHITE
                } else {
                    // 未选中帧的压暗 tint：与界面同口径的真中性灰，压暗不偏色。
                    //
                    // Neutral dim tint, consistent with the C2 contract: darken
                    // without any hue cast.
                    Color32::from_rgb(0x88, 0x88, 0x88)
                },
            );
        }
        if i == st.film {
            // 当前帧只保留**一条** 2px 紫框：原来的底部 3px 指示条和描边下沿
            // 叠在同一条边上，读成双线（审查点名）。框已经是唯一的选中语义。
            //
            // One border only: the old 3 px bottom bar stacked onto the stroke's
            // lower edge and read as a double line. The frame is the sole
            // selection cue now.
            tp.rect_stroke(
                cell,
                5.0,
                Stroke::new(2.0, ACCENT),
                egui::StrokeKind::Inside,
            );
        } else if resp.hovered() {
            tp.rect_stroke(
                cell,
                5.0,
                Stroke::new(1.0, Color32::from_rgb(0x64, 0x64, 0x64)),
                egui::StrokeKind::Inside,
            );
        }
        // 序号直接压在画面上：亮天空/白楼时白字完全糊掉（实测 09/10 两张）。
        // 给一个半透明黑底，宽度按实测文字宽加 8px 内距——LR 的胶片条也是这么做的。
        //
        // The index sits on the image itself: white text disappears on bright sky
        // (frames 09/10). A translucent dark chip behind it -- width from the measured
        // text plus 8 px -- is what Lightroom does too.
        // 序号只在**当前帧**与**悬停帧**上出现。
        //
        // 20 张缩略图各贴一块黑底白字的编号，整条胶片就成了一排标签贴纸——序号是
        // 需要时才查的信息，不是常驻信息（Lightroom 也只在选中/悬停时给编号）。
        //
        // Index chip only on the current frame and the hovered one: 20 permanent black
        // chips turn the strip into a row of stickers.
        let show_idx = i == st.film || resp.hovered();
        let idx = format!("{:02}", i + 1);
        let idx_font = egui::FontId::monospace(FONT_SMALL);
        let idx_w = text_w(&p, &idx, idx_font.clone());
        // chip 放左上角：光柱三角从缩略图下沿起笔，原来贴在左下角的序号块
        // 正好压在三角正上方，两者挤成一团；移到对角后互不接触。
        //
        // The chip moves to the top-left: the gnomon triangle starts at the
        // thumb's bottom edge, and the bottom-left chip sat right above it --
        // diagonal placement keeps them apart.
        let chip = Rect::from_min_size(
            Pos2::new(cell.left() + 4.0, cell.top() + 4.0),
            Vec2::new(idx_w + 8.0, 15.0),
        );
        if show_idx {
            tp.rect_filled(chip, 3.0, Color32::from_black_alpha(150));
            tp.txt(
                chip.center(),
                egui::Align2::CENTER_CENTER,
                idx,
                idx_font,
                Color32::from_white_alpha(235),
            );
        }
        if resp.clicked() {
            st.film = i;
            if let Some(a) = &st.actions {
                a.send(DemoAction::SetFrame(i));
            }
        }
    }
    y += 56.0 + 2.0;

    // 关键帧车道 18
    let kf = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), 18.0));
    p.hline(
        kf.x_range(),
        kf.center().y,
        // C5: 数据车道基线也比底暗，不画亮线。
        Stroke::new(1.0, Color32::from_black_alpha(48)),
    );
    // 桥接后用真关键帧集合（st.kfs）；独立演示时保留演示菱形。
    // 点击车道在当前帧开关关键帧（DemoAction::ToggleKeyframe）。
    //
    // Real keyframe set once bridged; demo diamonds standalone. Clicking the lane
    // toggles the keyframe on the current frame.
    let kf_list: Vec<usize> = if st.actions.is_some() {
        st.kfs.clone()
    } else {
        vec![3, 15, 28, 40]
    };
    let kresp = ui.interact(kf, ui.scope_id().with("kf-toggle"), Sense::click());
    if kresp.clicked() {
        if let Some(a) = &st.actions {
            let has_kf = st.kfs.contains(&st.film);
            a.send(DemoAction::ToggleKeyframe);
            if has_kf {
                st.say(ui.ctx(), crate::i18n::t!("已移除此帧关键帧"));
            } else {
                st.say(ui.ctx(), crate::i18n::t!("已在此帧建关键帧"));
            }
        }
    }
    for f in &kf_list {
        // 当前帧的关键帧点不画：晷针已经表达了"我在这里"，再叠一个实心菱形，
        // 三角 + 菱形 + 光柱会挤成一朵紫色的花。
        //
        // Skip the keyframe dot on the current frame: the gnomon already marks it,
        // and triangle + diamond + beam merged into a violet blob.
        if *f == st.film {
            continue;
        }
        let dc = Pos2::new(fx(*f as f32 + 0.5), kf.center().y);
        // 悬停：菱形放大 1px + 转实心紫。关键帧是"序列的锚点在哪"，悬停即
        // 把这个锚点点亮——查询反馈，不是装饰（C4：紫=位置语义）。
        //
        // Hover: grow 1 px and fill violet. A keyframe says "where the anchors
        // are"; hovering lights that anchor up -- query feedback, not
        // decoration (C4: violet = position semantics).
        let hov = ui
            .interact(
                Rect::from_center_size(dc, Vec2::splat(16.0)),
                ui.scope_id().with(("kfh", *f)),
                Sense::hover(),
            )
            .hovered();
        diamond(&tp, dc, if hov { 8.0 } else { 7.0 }, hov);
    }
    y += 18.0 + 2.0;

    // 逐帧亮度 21：底槽铺满可视区，柱状图按同一映射铺开再裁掉超出部分
    let ramp = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), 21.0));
    p.rect_filled(ramp, 3.0, FIELD);
    let bars = Rect::from_min_size(Pos2::new(fx(0.0), y), Vec2::new(total, 21.0));
    draw_ramp(&tp, bars, st.film, frames_count);
    // 标签压在最右几帧的柱子上：柱色 (0x6A) 与 T4_TEXT(0x74) 只差 10 级，
    // 文字直接埋进柱里。先铺一块与轨道同色的底，再写字——轨道底本来就是 FIELD，
    // 铺底看不出接缝，但文字不再被柱穿过。
    //
    // The label sits on the bars; bar grey (0x6A) and T4_TEXT (0x74) differ by only
    // 10 levels. Lay a FIELD patch first -- the ramp's own background is FIELD, so the
    // patch is invisible, but the text no longer has bars running through it.
    //
    // 橙色柱 = 检测到闪烁的帧（draw_ramp 的 is_flick 分支），图例必须写出来 ——
    // 没有说明的彩色方块，用户只会当成脏点或选中标记。
    //
    // Orange bars mark frames with detected flicker; the legend has to say so, or the
    // colour reads as dirt or a selection mark.
    // 图例：色块 + 短标签。长句「灰=原始 紫=去闪 橙=闪烁帧」会横跨近三分之一
    // 轨道，把整段数据压在文字下面；改成 6px 色块后只要 ~120px，且色块与柱/线
    // 同色，读的是颜色本身而不是解释颜色的话。
    //
    // Legend as swatches plus one-word labels. The long form spanned a third of the
    // ramp and buried the data under text; the swatches use the very colours of the
    // bars and line, so the reader matches colour, not prose.
    {
        let lf = egui::FontId::proportional(FONT_META);
        let keys: [(&str, Color32); 3] = [
            (crate::i18n::t!("原始"), Color32::from_rgb(0x7F, 0x7F, 0x7F)),
            (crate::i18n::t!("去闪"), ACCENT),
            (crate::i18n::t!("闪烁"), Color32::from_rgb(0xE3, 0xA8, 0x3C)),
        ];
        let mut lw = 0.0;
        for (t, _) in keys {
            lw += 6.0 + 3.0 + text_w(&p, t, lf.clone()) + 8.0;
        }
        let right = ramp.right() - 6.0;
        let x0 = right - lw + 8.0;
        p.rect_filled(
            Rect::from_min_size(
                Pos2::new(x0 - 5.0, ramp.top() + 2.0),
                Vec2::new(lw + 8.0, 17.0),
            ),
            3.0,
            FIELD,
        );
        let mut kx = x0;
        for (t, c) in keys {
            p.rect_filled(
                Rect::from_center_size(Pos2::new(kx + 3.0, ramp.center().y), Vec2::splat(6.0)),
                1.0,
                c,
            );
            kx += 9.0;
            p.txt(
                Pos2::new(kx, ramp.center().y),
                egui::Align2::LEFT_CENTER,
                t,
                lf.clone(),
                T4_TEXT,
            );
            kx += text_w(&p, t, lf.clone()) + 8.0;
        }
    }
    y += 21.0 + 2.0;

    // 刻度 13：跟着同一映射走（0:00 / 0:12 / 0:24 / 0:36 / 0:48 = 第 0/12/24/36/48 帧）
    let ruler = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), 13.0));
    let marks = ["0:00", "0:12", "0:24", "0:36", "0:48"];
    for (i, m) in marks.iter().enumerate() {
        let frame = i as f32 * 12.0;
        let x = fx(frame);
        if x < inner.left() - 10.0 || x > inner.right() + 10.0 {
            continue;
        }
        let al = if i == 0 {
            egui::Align2::LEFT_CENTER
        } else if i == marks.len() - 1 {
            egui::Align2::RIGHT_CENTER
        } else {
            egui::Align2::CENTER_CENTER
        };
        p.txt(
            Pos2::new(x, ruler.center().y),
            al,
            *m,
            egui::FontId::monospace(FONT_SMALL),
            T4_TEXT,
        );
    }

    // 当前帧竖线：只贯穿三车道（关键帧 / 亮度 / 刻度），**不从缩略图上切过去**。
    // 之前它从上往下穿过被选中的那张图，与 2px 选中框、序号块叠在一起，
    // 看起来像一条裁剪辅助线。当前帧在缩略图上已有选中框表达。
    //
    // The playhead spans the three lanes only and no longer cuts through the
    // thumbnail: crossing the selected frame on top of its 2 px border and index chip
    // made it look like a crop guide.
    // 晷针（视觉签名的第二件）：当前帧位置一道 2px 光柱，顶端一枚小三角。
    //
    // 名字里的「晷」就是日影：一道自上而下衰减的光，顶端有个指针。它同时干三件事
    // ——标当前帧、串起三条车道、给这块区域一个只属于这个产品的记号（通用播放头
    // 是 1px 等宽线，谁都能画；有顶端指针和衰减的才是"晷"）。
    //
    // The gnomon: a 2 px beam that fades toward its base, capped by a triangle.
    // 「恒晷」means sundial — this is the one mark no other app would draw.
    let px = fx(st.film as f32 + 0.5);
    let beam_top = thumbs.bottom() + 2.0;
    let beam_bot = ruler.bottom() - 4.0;
    // 三角紧贴缩略图下沿（1..6），光柱从 6 起——关键帧菱形中心在 +11、半径 3.5，
    // 两者留出 1.5px 空隙，不会糊成一团紫色。
    //
    // Triangle hugs the thumb's bottom edge (1..6), beam starts at 6; the keyframe
    // diamond sits at +11 with r=3.5, so the two no longer merge.
    tp.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(px, beam_top + 1.0),
            Pos2::new(px + 4.0, beam_top + 6.0),
            Pos2::new(px - 4.0, beam_top + 6.0),
        ],
        T2,
        Stroke::NONE,
    ));
    vgrad_rect(
        &tp,
        Rect::from_min_max(
            Pos2::new(px - 1.0, beam_top + 6.0),
            Pos2::new(px + 1.0, beam_bot),
        ),
        T2, // C4: 当前帧已由紫框表达，晷针只是读数，用中性灰
        Color32::from_rgba_unmultiplied(0xAC, 0xAC, 0xAC, 26),
    );
}

/// 五角星（字体里没有 ★ 的实心/空心差异，直接画）
fn star(p: &egui::Painter, c: Pos2, r: f32, color: Color32, filled: bool) {
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let ang = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        let rad = if i % 2 == 0 { r } else { r * 0.42 };
        pts.push(Pos2::new(c.x + rad * ang.cos(), c.y + rad * ang.sin()));
    }
    if filled {
        p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    } else {
        for i in 0..10 {
            p.line_segment([pts[i], pts[(i + 1) % 10]], Stroke::new(1.0, color));
        }
    }
}

/// 关键帧菱形
fn diamond(p: &egui::Painter, c: Pos2, s: f32, on: bool) {
    let h = s / 2.0;
    let pts = vec![
        Pos2::new(c.x, c.y - h),
        Pos2::new(c.x + h, c.y),
        Pos2::new(c.x, c.y + h),
        Pos2::new(c.x - h, c.y),
    ];
    if on {
        p.add(egui::Shape::convex_polygon(
            pts,
            ACCENT,
            Stroke::new(1.0, ACCENT),
        ));
    } else {
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.2, T3)));
    }
}

/// 逐帧亮度柱 + 去闪平滑线（与 def 屏同一套数据口径）
fn draw_ramp(p: &egui::Painter, rect: Rect, cur: usize, n: usize) {
    // 演示波形：一条低频明暗节奏 + 一处渐变抬升，读起来才像「一段片子的亮度」。
    // 只用一个 sigmoid 时整条带前后差不到 3px，看起来就是一条灰线（设计稿那条
    // canvas 带子有 2~3 个起伏峰）。这是**演示数据**，真实数据来自 core 的逐帧测光。
    //
    // Demo waveform: one low-frequency swing plus a gradual lift, so the strip reads
    // as "the brightness of a clip". A lone sigmoid differs by <3 px end to end and
    // looks like a flat grey line; the mockup's canvas strip has 2-3 peaks. This is
    // **demo data** -- real values come from core's per-frame metering.
    let base = |i: usize| {
        let t = i as f32 / n as f32;
        let wave = 0.5 + 0.5 * (t * std::f32::consts::TAU * 1.15 - 1.1).sin();
        (0.24 + 0.46 * wave + 0.12 * (t * std::f32::consts::TAU * 3.0).sin()).clamp(0.06, 1.0)
    };
    let flick = |i: usize| {
        if i == 8 || i == 17 || i == 26 || i == 39 {
            0.10
        } else if i % 9 == 0 {
            0.035
        } else {
            0.0
        }
    };
    let raw: Vec<f32> = (0..n).map(|i| (base(i) + flick(i)).min(1.0)).collect();
    let smooth: Vec<f32> = (0..n)
        .map(|i| {
            let lo = i.saturating_sub(2);
            let hi = (i + 2).min(n - 1);
            let s: f32 = raw[lo..=hi].iter().sum();
            s / (hi - lo + 1) as f32
        })
        .collect();
    let bw = rect.width() / n as f32;
    let lo = 0.22f32;
    let hi = 0.85f32;
    for i in 0..n {
        let v = ((raw[i] - lo) / (hi - lo)).clamp(0.02, 1.0);
        let h = (rect.height() - 4.0) * v;
        let x = rect.left() + i as f32 * bw;
        let is_flick = flick(i) > 0.05;
        // 柱宽 = 帧宽减 1px：原来只占 0.5 档，48 帧在 1500px 里读成一串虚线，
        // 看不出「逐帧亮度」是一条连续带（设计稿 .ramp 是贴柱的柱状图）。
        // Bar width = frame width minus 1 px: the old 0.5 ratio read as a dashed
        // line instead of the continuous per-frame ramp in the mockup.
        p.rect_filled(
            Rect::from_min_size(
                // 半像素 x+0.5 在 150% 缩放下会跨两个物理像素，放大读成虚边；
                // 用整数坐标 + 满宽 bw，48 帧读成一条连续带（设计稿 .ramp 即如此）。
                Pos2::new(x.round(), rect.bottom() - 2.0 - h),
                Vec2::new(bw.max(1.0), h),
            ),
            0.0, // C6: 数据柱方角
            if is_flick {
                Color32::from_rgb(0xE3, 0xA8, 0x3C)
            } else {
                Color32::from_rgb(0x7F, 0x7F, 0x7F)
            },
        );
    }
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let v = ((smooth[i] - lo) / (hi - lo)).clamp(0.02, 1.0);
        pts.push(Pos2::new(
            rect.left() + (i as f32 + 0.5) * bw,
            rect.bottom() - 2.0 - (rect.height() - 4.0) * v,
        ));
    }
    // 曲线用中性灰：它是一条**读数**，不是状态。原来用强调色，整条横贯的紫线
    // 与时间轴上的晷针同色，屏幕上就出现了两个"紫色主角"——只有当前帧配得上彩色。
    //
    // Neutral grey: the curve is a readout, not a state. In accent colour it competed
    // with the gnomon for the "one violet thing on screen" slot.
    p.add(egui::Shape::line(pts, Stroke::new(1.4, T3)));

    // 当前帧游标**不在这里画**：时间轴三车道已经有一条贯穿的紫线（draw_film），
    // 再叠一条白线，同一个 x 上出现两条不同颜色的竖线，读起来像两个控件在抢同一件事。
    //
    // No cursor here: the timeline already draws one accent playhead across the lane.
    // A second white line at the same x reads as two widgets fighting over one thing.
    let _ = cur;
}

// ---------------------------------------------------------------- toast

impl Proto {
    fn toast_now(&mut self, ctx: &egui::Context, msg: &str) {
        let t = ctx.input(|i| i.time);
        self.toast = Some((msg.to_string(), t));
    }
}

fn draw_toast(ctx: &egui::Context, st: &mut Proto, film_h: f32) {
    let Some((msg, t0)) = st.toast.clone() else {
        return;
    };
    let now = ctx.input(|i| i.time);
    let age = now - t0;
    if age > 2.2 {
        st.toast = None;
        return;
    }
    let alpha = ((2.2 - age) / 0.6).clamp(0.0, 1.0);
    // 入场 160ms（C9）：从下方 8px 滑入，透明度与位移同一条曲线——
    // 退场淡出早已存在，入场却是"凭空出现"，反馈的来路不明。
    //
    // Entrance 160 ms (C9): slide up 8 px with alpha on the same curve. The
    // exit fade always existed; the entrance used to pop out of nowhere.
    let ent = (age / 0.16).clamp(0.0, 1.0);
    let alpha = alpha.min(ent);
    let a = (alpha * 235.0) as u8;
    let screen = ctx.content_rect();
    let w = 380.0_f32.min(screen.width() - 40.0);
    let r = Rect::from_min_size(
        // C7: 20/36 归到 16/32。
        Pos2::new(
            screen.right() - w - 16.0,
            screen.bottom() - 32.0 - film_h + (1.0 - ent as f32) * 8.0,
        ),
        Vec2::new(w, 32.0),
    );
    let p = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("toast"),
    ));
    p.rect_filled(
        r,
        10.0,
        Color32::from_rgba_unmultiplied(CHROME[0], CHROME[1], CHROME[2], a),
    ); // C10/C2
    p.rect_stroke(
        r,
        8.0,
        Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(HAIRLINE[0], HAIRLINE[1], HAIRLINE[2], a),
        ), // C10/C2
        egui::StrokeKind::Inside,
    );
    p.txt(
        Pos2::new(r.left() + 12.0, r.center().y),
        egui::Align2::LEFT_CENTER,
        msg,
        f_small(),
        Color32::from_rgba_unmultiplied(T1[0], T1[1], T1[2], a), // C10/C2
    );
    ctx.request_repaint();
}

// ---------------------------------------------------------------- App

impl Proto {
    /// demo 原样的绘制入口（从 `eframe::App::ui` 搬出来，壳里直接调用）。
    pub fn ui(&mut self, root: &mut egui::Ui, canvas_in: &egui::Rect) {
        // eframe 会在窗口创建后用自己的 ppp 覆盖 `new()` 里的设置，所以这里持续校正。
        // 只在偏离时才写，避免每帧触发重排。
        {
            let ctx = root.ctx().clone();
            let zoom = ui_zoom();
            if (ctx.zoom_factor() - zoom).abs() > 0.005 {
                ctx.set_zoom_factor(zoom);
                ctx.request_repaint();
            }
        }
        let no_frame = egui::Frame::NONE;
        let _ = no_frame;

        // ---- 布局：一个框架 + 一口井 ----
        //
        // 上一版是「六块浮板」：每块都缩进 5px、各自带圆角与柔影。结果是屏幕被切成
        // 六张卡片，相邻处叠出「缝 + 边 + 缝」三道边界——顶栏与画布之间那道最明显。
        // macOS 和 Lightroom 都不是这么分的：**整窗是一个框架，只有内容井是凹下去的
        // 一块**。分割越少，照片越突出。
        //
        // 现在：顶栏 / 工具轨 / 右坞 / 胶片条 / 状态栏 全部贴边同底，只用 1px 暗线
        // 分界；画布是唯一内缩 8px、带圆角与内阴影的"井"。
        //
        // One frame plus one well. The previous version inset six sheets, which stacked
        // gap + edge + gap at every junction. macOS and Lightroom use one frame and a
        // single recessed well instead.
        let full = root.max_rect();
        {
            let p = root.painter().clone();
            p.rect_filled(full, 0.0, CHROME);
        }

        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), BAR_H));
        // 顶栏：竖向光照，上亮下暗。
        rounded_grad(&root.painter().clone(), bar, 0.0, BAR_LIT, CHROME, false);
        let status = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - STATUS_H), full.max);
        let film = Rect::from_min_max(
            Pos2::new(full.left(), status.top() - FILM_H),
            Pos2::new(full.right(), status.top()),
        );
        let body_top = bar.bottom();
        let body_bottom = film.top();
        let rail = Rect::from_min_max(
            Pos2::new(full.left(), body_top),
            Pos2::new(full.left() + RAIL_W, body_bottom),
        );
        let dock = Rect::from_min_max(
            Pos2::new(full.right() - DOCK_W, body_top),
            Pos2::new(full.right(), body_bottom),
        );
        // 画布井矩形由 mod.rs 从 layout 单源推入（canvas_insets + centre_rect）：
        // 呼吸带只在 layout 加一次，井的上/下/左内缩也在那边统一补齐。
        //
        // The well rect arrives from mod.rs, derived through the layout single
        // source; this side only consumes it.
        let canvas = *canvas_in;

        {
            let p = root.painter().clone();
            // 框架：横向光照。工具轨的光从右侧（井）来——右缘略亮；右坞的光从左
            // 侧来——左缘略亮。两条侧栏因此看起来是在"包围"画布，而不是贴在墙上
            // 的色块。胶片条保持竖向：上暗下亮，与井的底部呼应。
            rounded_grad(&p, rail, 0.0, PANEL_SIDE, shade(PANEL_SIDE, 1.14), true);
            rounded_grad(&p, dock, 0.0, shade(PANEL_SIDE, 1.14), PANEL_SIDE, true);
            // 胶片条：顶到底只差 12% 的渐变几乎不可感知，整块读成一块平涂的
            // 泥灰（用户点名）。顶端从 ×0.80 起步——靠近井底的明度，接缝处
            // 不再是"黑井突然接一块亮灰"，同时上暗下亮的光形也更明显。
            //
            // The strip's 12% gradient was imperceptible -- it read as one flat
            // slab of mud grey. Starting the top at ×0.80 (near the well's bottom
            // luminance) softens the seam and makes the light shape visible.
            rounded_grad(&p, film, 0.0, shade(STRIP, 0.80), shade(STRIP, 1.12), false);
            // 井：圆角 + **纵向纵深渐变**（顶部暗、底部亮）——这是"光桌"的物理依据：
            // 眼睛读到底部更亮，会把它理解成一个向光倾斜的凹面，而不是一块平板。
            rounded_grad(
                &p,
                canvas,
                10.0,
                shade(CANVAS, 0.86),
                shade(CANVAS, 1.17),
                false,
            );
            // 井的内阴影删了：它是三条**矩形**渐变带，而井是圆角——在左上/右上两个
            // 圆角处，暗带的直角会从弧里戳出来，放大 3 倍能看到一个折角。
            // 井与壳的 ΔL* 已有 7.0，"凹"由明度差自己成立，不需要再压一层暗边。
            //
            // The well's inner shadow was three *rectangular* gradient bands on a
            // rounded rect: at the top corners the band's square end poked out of the
            // arc. ΔL* 7.0 against the shell already reads as recessed.
            // 井的 1px 描边也删了。它是 black 70，理论上该"界定边界"，但圆角半径
            // 只有 10、描边宽度 1，在四个弧上的抗锯齿会挤出半像素台阶；而 ΔL* 7.0
            // 的明度差本身就把边界说清楚了。深色 UI 里"少一道线"几乎总是对的。
            //
            // The well's 1 px stroke is gone too: at r=10 it produced half-pixel steps
            // on the arcs, and ΔL* 7.0 already states the boundary.
            // 唯一的四处分隔，全部是「比底色更暗」的线，不是亮线。
            let line = Stroke::new(1.0, Color32::from_black_alpha(96));
            p.hline(bar.x_range(), bar.bottom() - 0.5, line);
            p.vline(rail.right() - 0.5, rail.y_range(), line);
            p.vline(dock.left() + 0.5, dock.y_range(), line);
            p.hline(film.x_range(), film.top() + 0.5, line);
            p.hline(
                status.x_range(),
                status.top() + 0.5,
                Stroke::new(1.0, Color32::from_black_alpha(56)),
            );
        }

        // 帧时间采样：环形缓冲 30 帧，取中位数。首帧包含窗口初始化（70ms+），
        // 瞬时值会把它当成常态显示出来。
        let dt = root.ctx().input(|i| i.stable_dt) * 1000.0;
        self.dt_hist[self.dt_n % 30] = dt;
        self.dt_n += 1;
        let n_dt = self.dt_n.min(30);
        let mut samples = self.dt_hist;
        samples[..n_dt]
            .sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let frame_ms = samples[n_dt / 2];

        root.scope_builder(egui::UiBuilder::new().max_rect(bar).id_salt("bar"), |ui| {
            ui.set_clip_rect(bar);
            draw_menubar(ui, self);
        });
        root.scope_builder(
            egui::UiBuilder::new().max_rect(rail).id_salt("rail"),
            |ui| {
                ui.set_clip_rect(rail);
                draw_rail(ui, self);
            },
        );
        root.scope_builder(
            egui::UiBuilder::new().max_rect(dock).id_salt("dock"),
            |ui| {
                ui.set_clip_rect(dock);
                draw_dock(ui, self);
            },
        );
        root.scope_builder(
            egui::UiBuilder::new().max_rect(canvas).id_salt("canvas"),
            |ui| {
                ui.set_clip_rect(canvas);
                draw_canvas(ui, self);
            },
        );
        root.scope_builder(
            egui::UiBuilder::new().max_rect(film).id_salt("film"),
            |ui| {
                ui.set_clip_rect(film);
                draw_film(ui, self);
            },
        );
        root.scope_builder(
            egui::UiBuilder::new().max_rect(status).id_salt("status"),
            |ui| {
                ui.set_clip_rect(status);
                draw_status(ui, frame_ms);
            },
        );

        let ctx = root.ctx().clone();
        draw_toast(&ctx, self, film.height());
        self.drive_shot(&ctx);
    }
}

// ---------------------------------------------------------------- 真应用桥接
//
// 这一段是**唯一**为接入真应用写的代码；上面所有绘制函数都是 demo 原文。
// 每帧把 AppState 的字段同步进 Proto，然后调用 demo 原样的 `Proto::ui`。

use crate::state::AppState as RealAppState;
use std::cell::RefCell;

thread_local! {
    /// demo 的界面状态（与真应用参数分开存放；参数每帧从真应用同步进来）。
    static DEMO_STATE: RefCell<Option<Proto>> = const { RefCell::new(None) };
}

/// 壳入口：同步真状态 → 调 demo 原样绘制。
pub(super) fn ui(state: &mut RealAppState, root: &mut egui::Ui, canvas: &egui::Rect) {
    // 字体：demo 独立版在 Proto::new() 里装字体，桥接后 new() 不再被调用；
    // 真壳只在「字号档位变了」时才装（windows.rs），首帧用的是 egui 内置字体，
    // 于是同一个词里有的字走系统字体、有的走内置兜底——观感就是"字体不统一"。
    // 这里首帧补装一次，之后 theme 的 OnceLock 让重复调用几乎零成本。
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FONTS_SET: AtomicBool = AtomicBool::new(false);
        if !FONTS_SET.swap(true, Ordering::Relaxed) {
            crate::theme::set_fonts(root.ctx());
        }
    }
    // 首帧调试钩子（与旧壳同一口径）：AEVUM_OPEN_PHOTOS=1 自动打开选图对话框，
    // 让 --shot 通道能验证"真照片 → demo 画布/胶片"这条链。
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static OPENED: AtomicBool = AtomicBool::new(false);
        if std::env::var_os("AEVUM_OPEN_PHOTOS").is_some() && !OPENED.swap(true, Ordering::Relaxed)
        {
            super::dialogs::open_photos_dialog(state);
        }
        if let Some(path) = std::env::var_os("AEVUM_LOAD") {
            use std::path::PathBuf;
            let p = PathBuf::from(path);
            if OPENED
                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                state.open_paths(root.ctx(), std::slice::from_ref(&p));
            }
        }
    }
    DEMO_STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let st = slot.get_or_insert_with(|| Proto {
            photo: None,
            tasks: 0,
            rating: 0,
            kfs: Vec::new(),
            module: crate::state::Module::Develop,
            dock_tab: 0,
            mixer_tab: 0,
            prop: 0,
            hsl_h: [0.0; 8],
            hsl_s: [0.0; 8],
            hsl_l: [0.0; 8],
            mono: [0.0; 8],
            wb_temp: 0.0,
            wb_tint: 0.0,
            basic: [0.0; 6],
            effects: [0.0; 3],
            presence: [0.0; 5],
            deflicker: 0.0,
            smooth_win: 0.0,
            preset: 0,
            hist: 0,
            left_open: true,
            presets_open: true,
            dt_hist: [0.0; 30],
            dt_n: 0,
            film: 0,
            zoom: 1.0,
            pan: Vec2::ZERO,
            dock_scroll: 0.0,
            toast: None,
            shot: None,
            frames: 0,
            ai_busy: false,
            thumb_source: None,
            actions: None,
        });

        // ---- 注入动作队列 ----
        let action_queue = std::sync::Arc::new(std::sync::Mutex::new(Vec::<DemoAction>::new()));
        st.actions = Some(std::sync::Arc::new(DemoActions {
            queue: action_queue.clone(),
        }));

        // ---- 状态同步（真 → demo）----
        st.photo = state.texture.clone();
        // 截图验证通道：AEVUM_DEMO_PHOTO=<path> 直接同步解码注入 demo 的照片位，
        // 绕开真应用的异步预览链（--shot 第 3 帧就退出，异步预览来不及完成）。
        // 正常运行时不设置该变量，照片完全来自 state.texture。
        if st.photo.is_none() {
            if let Some(path) = std::env::var_os("AEVUM_DEMO_PHOTO") {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(img) = image::load_from_memory(&bytes) {
                        let img = img.to_rgba8();
                        let (w, h) = img.dimensions();
                        let color = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            img.as_raw(),
                        );
                        st.photo = Some(root.ctx().load_texture(
                            "demo-verify-photo",
                            color,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                }
            }
        }
        // 真应用缩略图：每帧从 AppState 取当前帧的 TextureHandle 列表快照。
        let thumbs: Vec<Option<egui::TextureHandle>> = (0..st.frames as usize)
            .map(|i| state.thumb(i).map(|t| t.texture.clone()))
            .collect();
        st.thumb_source = Some(std::sync::Arc::new(move |i: usize| {
            thumbs.get(i).cloned().flatten()
        }));
        st.film = state.selected.min(state.frame_count().saturating_sub(1));
        st.frames = state.frame_count() as u32;
        st.wb_temp = state.params.temperature;
        st.wb_tint = state.params.tint;
        st.hsl_h = state.params.hsl_h;
        st.hsl_s = state.params.hsl_s;
        st.hsl_l = state.params.hsl_l;
        st.basic[0] = state.params.exposure;
        st.basic[1] = state.params.contrast;
        st.basic[2] = state.params.highlights;
        st.basic[3] = state.params.shadows;
        st.basic[4] = state.params.whites;
        st.basic[5] = state.params.blacks;
        st.effects[0] = state.params.clarity;
        st.effects[1] = state.params.texture;
        st.effects[2] = state.params.vibrance;

        // ---- demo 原样绘制（画布井矩形由 mod.rs 单源推入）----
        st.ui(root, canvas);

        // ---- 执行 demo 触发的真应用动作 ----
        let actions: Vec<DemoAction> = action_queue
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default();
        for action in actions {
            match action {
                DemoAction::OpenFolder => super::dialogs::open_dialog(state),
                DemoAction::OpenPhotos => super::dialogs::open_photos_dialog(state),
                DemoAction::Export => {
                    let _ = super::export_dialog::default_export_choice(state, false);
                }
                DemoAction::Undo => {
                    let ctx = root.ctx();
                    super::tabs::run_history_command(
                        state,
                        ctx,
                        "history.undo",
                        &serde_json::json!({}),
                    );
                }
                DemoAction::ApplyPreset(i) => {
                    if let Some(name) = aevum_core::grade::PRESET_NAMES.get(i) {
                        state.apply_preset(name);
                    }
                }
                DemoAction::AutoExposure => state.auto_exposure(root.ctx()),
                DemoAction::ResetParams => state.reset_params(),
                DemoAction::SetFrame(i) => state.set_frame(i),
                DemoAction::SelectModule(m) => state.module = m,
                DemoAction::SetRating(n) => {
                    state.ratings.insert(state.selected, n);
                }
                DemoAction::ToggleKeyframe => {
                    if state.keyframes.contains_key(&state.selected) {
                        state.keyframes.remove(&state.selected);
                    } else {
                        state.keyframes.insert(state.selected, state.params.clone());
                    }
                }
            }
        }

        // ---- 控件写回（demo → 真）----
        let params_before = state.params.clone();
        state.params.temperature = st.wb_temp;
        state.params.tint = st.wb_tint;
        state.params.hsl_h = st.hsl_h;
        state.params.hsl_s = st.hsl_s;
        state.params.hsl_l = st.hsl_l;
        state.params.exposure = st.basic[0];
        state.params.contrast = st.basic[1];
        state.params.highlights = st.basic[2];
        state.params.shadows = st.basic[3];
        state.params.whites = st.basic[4];
        state.params.blacks = st.basic[5];
        state.params.clarity = st.effects[0];
        state.params.texture = st.effects[1];
        state.params.vibrance = st.effects[2];
        // 参数变了就触发真应用的重算链（demo 独立运行时没有这一步）。
        if params_before.temperature != state.params.temperature
            || params_before.tint != state.params.tint
            || params_before.exposure != state.params.exposure
            || params_before.contrast != state.params.contrast
            || params_before.highlights != state.params.highlights
            || params_before.shadows != state.params.shadows
            || params_before.whites != state.params.whites
            || params_before.blacks != state.params.blacks
            || params_before.clarity != state.params.clarity
            || params_before.texture != state.params.texture
            || params_before.vibrance != state.params.vibrance
            || params_before.hsl_h != state.params.hsl_h
            || params_before.hsl_s != state.params.hsl_s
            || params_before.hsl_l != state.params.hsl_l
        {
            state.preview_dirty = true;
            state.recompute_deflicker();
        }

        // ---- 真应用状态 → demo 显示（任务胶囊 / 星标 / 关键帧车道）----
        st.tasks = state.queue.len();
        st.rating = state.ratings.get(&state.selected).copied().unwrap_or(0);
        st.kfs = state.keyframes.keys().copied().collect();
        st.module = state.module;
    });
}
