//! 全局调色参数的**唯一事实源**。
//!
//! 以前一个参数有三处手写清单：面板里的滑块调用、`group_changed` 的改动
//! 判断、`reset_group` 的复位赋值——加一个参数要同步三处，漏一处就出现
//! 「滑块动了但分组圆点不亮」这类抽屉式 bug。现在只有这张表：
//!
//! - 面板：`shell::spec_slider` 按表渲染推子，标签/范围/默认/精度全部来自这里；
//! - 分组头圆点：`state::group_changed` 按表比对是否偏离默认；
//! - 分组复位：`shell::reset_group` 按表把字段写回默认。
//!
//! 不进表的三类：绑定 `AppState` 自有字段的滑块（去闪窗口/平滑）、数组参数
//! （HSL/黑白混合按色带循环生成，键自带下标）、蒙版局部参数（键随选中组变化）。
//!
//! The **single source of truth** for global grading parameters.
//!
//! A parameter used to be hand-listed in three places: the slider call in the
//! panel, the change test in `group_changed`, and the reset assignment in
//! `reset_group` -- one new parameter meant three synchronised edits, and missing
//! one produced drawer-style bugs like "the slider moved but the group dot stays
//! dark". Now there is only this table:
//!
//! - panel: `shell::spec_slider` renders the fader from the row; label, range,
//!   default and precision all come from here;
//! - group dot: `state::group_changed` compares against the default via the table;
//! - group reset: `shell::reset_group` writes the field back to its default.
//!
//! Three kinds stay out of the table: sliders bound to `AppState`'s own fields
//! (deflicker window / smoothing), array parameters (HSL and B&W mix are generated
//! per band with the index in the key), and mask-local parameters (the key changes
//! with the selected mask group).

use aevum_core::grade::GradeParams;

/// `GradeParams` 里能上推子的标量字段。数组与复杂字段不进来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Exposure,
    /// 曝光高光有理肩部 rolloff（0–100，对齐 LR）：曝光推过 1 的区域缓收不硬剪。
    HlRolloff,
    Contrast,
    /// 对比度的线性光域混合系数（0–100，对齐 LR）：0 = 旧 gamma 域路径。
    ContrastLinear,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    /// 黑场抛物线软拐点（0–100，对齐 LR）：负值收进 [0,1] 不再硬剪。
    BlackSoft,
    /// 白场抛物线软拐点（0–100，对齐 LR）：过 1 的层次收进 [0,1] 不再硬剪。
    WhiteSoft,
    Temperature,
    Tint,
    Vibrance,
    Saturation,
    Clarity,
    Texture,
    Dehaze,
    /// 暗通道去雾量（0–100，He DCP）：>0 在旧去朦胧口径之后叠加空间感知去雾。
    DehazeDark,
    /// 去雾对比（-50..50）：DCP 透射修正后的中间对比/黑位微调。
    DehazeContrast,
    Grain,
    /// 颗粒亮度调制（-100..100，对齐 LR）：正值暗部更重，负值反转（高光更重）。
    GrainLum,
    GrainSize,
    GrainFreq,
    Vignette,
    VigMidpoint,
    VigRoundness,
    VigFeather,
    VigHlContrast,
    SharpAmount,
    SharpRadius,
    SharpDetail,
    SharpMasking,
    /// 锐化噪声自适应边缘保护（0–100，对齐 LR）：平坦噪区免锐化。
    SharpProtect,
    NrLuma,
    NrColor,
    NrLumaDetail,
    NrLumaContrast,
    NrColorSmooth,
    /// 多尺度降噪（0–100）：0 = 旧单尺度逐位一致。
    NrMultiscale,
    /// 手动噪声 σ（0–100）：0 = 自动 MAD 估计。
    NrSigmaManual,
    NrAi,
    GradeGlobalH,
    GradeGlobalS,
    GradeGlobalL,
    GradeBalance,
    GradeBlending,
    ParShadows,
    ParDarks,
    ParLights,
    ParHighlights,
    Mono,
    LensDistort,
    LensVignette,
    LensFringe,
    /// 曲线 x>1 尾部延伸（0–100，对齐 LR）：过曝层次按曲线末端斜率延伸进曲线。
    CurveHdrTail,
}

/// 一条推子的全部规格。渲染层只认这个结构，不再各自传八个实参。
#[derive(Clone, Copy, Debug)]
pub struct ControlSpec {
    /// 持久 id（egui 控件记忆、撤销标注用），同以前的滑块 key。
    ///
    /// Stable id (egui widget memory, undo labelling) -- the same key the slider used
    /// before.
    pub key: &'static str,
    pub label: &'static str,
    pub field: Field,
    pub min: f64,
    pub max: f64,
    /// 默认值。必须等于 `GradeParams::default()` 的同名字段——有测试钉住。
    ///
    /// Default value. Must equal the same-named field of `GradeParams::default()` -- a
    /// test pins this.
    pub default: f64,
    pub decimals: usize,
    /// 允许负值。只影响读数格式（带不带 + 号），范围照 min/max 走。
    ///
    /// Allow negative values. Affects only the readout format (whether a `+` is
    /// shown); the range still follows min/max.
    pub signed: bool,
    /// 读数后缀，如 "px"。空串 = 纯数字。
    ///
    /// Readout suffix, e.g. "px". Empty string = plain number.
    pub unit: &'static str,
    /// 悬停 tooltip。空串 = 不挂（大多数推子标签自释）。
    ///
    /// Hover tooltip. Empty string = none (most fader labels are self-explanatory).
    pub tip: &'static str,
    /// 分组键，同 `group_header` 的 key。
    ///
    /// Group key, matching the `group_header` key.
    pub group: &'static str,
    /// 色温/色调这类「轨道即含义」的渐变轨。
    ///
    /// Gradient track for parameters where the track itself carries the meaning, such
    /// as temperature / tint.
    pub grad: bool,
    /// 非线性轨道标记：拖动时「位置↔值」经 `aevum_core::temp_track` 换算，
    /// 数字输入框/历史/落盘仍用原始值。当前只有色温一条；换回线性
    /// 即删掉这个标记（core 侧还有一张 LINEAR 退表可整表替换）。
    ///
    /// Marks a non-linear track: while dragging, "position <-> value" goes through
    /// `aevum_core::temp_track`; the number box, undo history and persistence keep
    /// using the raw value. Only temperature uses this today; going back to linear
    /// means deleting this flag (core also has a LINEAR retirement table that can be
    /// swapped in wholesale).
    pub track: bool,
}

/// 渐变轨的停靠点色标：`(位置 0..1, 颜色)` 的有序表，长度 ≥2。
///
/// 三色以上才像 PS：色温在 PS / Camera Raw 里是「蓝 → 中性灰 → 黄」，两个端点直接
/// 对插会在中间拉出一条发绿的过渡，中灰位置反而不中性；色调同理（绿 → 灰 → 品红）。
/// 所以这里用停靠点表而不是两端色，中间的灰显式写出来。
///
/// Gradient track stops: an ordered `(position 0..1, colour)` table of length >= 2.
///
/// Three or more stops are what makes it read like PS: temperature there runs
/// "blue -> neutral grey -> yellow", and interpolating straight between the two ends
/// drags a green cast through the middle, so the neutral point is not actually neutral.
/// Tint behaves the same way (green -> grey -> magenta). Hence a stop table with the
/// grey written out explicitly rather than two end colours.
pub fn grad_stops(spec: &ControlSpec) -> Option<&'static [(f32, egui::Color32)]> {
    match spec.key {
        "dev.temperature" => Some(&TEMPERATURE_STOPS),
        "dev.tint" => Some(&TINT_STOPS),
        _ => None,
    }
}

/// 色温轨：冷蓝 → 中性灰 → 暖黄。中灰取主题的 dim 档，保证中点是真正的「无彩」。
///
/// Temperature rail: cold blue -> neutral grey -> warm yellow. The middle grey comes
/// from the theme's `dim` step so the centre point is genuinely achromatic.
/// 色值直接写字面量：`theme::temp_cold()` 这类访问器不是 const fn，static 里调不了；
/// 这四个「材质色不随主题变」的值在 theme.rs 里就是固定 RGB（见那里的注释），
/// 这里抄同一组数，改动时两处一起改。
///
/// The values are written literally: accessors such as `theme::temp_cold()` are not
/// `const fn` and cannot be called from a `static`. Those four "material colours do not
/// follow the theme" values are fixed RGB in theme.rs (see the note there), so the same
/// numbers are repeated here and both places change together.
/// 色温轨：冷蓝 → 中性灰 → 暖黄。中灰取主题的 dim 档，保证中点是真正的「无彩」。
///
/// Temperature rail: cold blue -> neutral grey -> warm yellow. The middle grey comes
/// from the theme's `dim` step so the centre point is genuinely achromatic.
/// 色值直接写字面量：`theme::temp_cold()` 这类访问器不是 const fn，static 里调不了。
///
/// Values are literals: accessors such as `theme::temp_cold()` are not `const fn`.
static TEMPERATURE_STOPS: [(f32, egui::Color32); 3] = [
    (0.0, egui::Color32::from_rgb(0x4E, 0x86, 0xC8)),
    (0.5, egui::Color32::from_rgb(0x8A, 0x8A, 0x8C)),
    (1.0, egui::Color32::from_rgb(0xD8, 0xA4, 0x55)),
];

/// 色调轨：绿 → 中性灰 → 品红。
///
/// Tint rail: green -> neutral grey -> magenta.
static TINT_STOPS: [(f32, egui::Color32); 3] = [
    (0.0, egui::Color32::from_rgb(0x63, 0xB4, 0x6E)),
    (0.5, egui::Color32::from_rgb(0x8A, 0x8A, 0x8C)),
    (1.0, egui::Color32::from_rgb(0xB9, 0x66, 0xB0)),
];

/// 在停靠点表上按位置取色（分段线性插值）。
///
/// Sample the stop table at a position (piecewise-linear interpolation).
pub fn sample_stops(stops: &[(f32, egui::Color32)], t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    if stops.len() < 2 {
        return stops.first().map(|s| s.1).unwrap_or(egui::Color32::GRAY);
    }
    for w in stops.windows(2) {
        let (p0, c0) = w[0];
        let (p1, c1) = w[1];
        if t <= p1 {
            let d = (p1 - p0).max(1e-6);
            return crate::widgets::mix(c0, c1, (t - p0) / d);
        }
    }
    stops[stops.len() - 1].1
}

macro_rules! spec {
    ($key:literal, $label:literal, $field:ident, $min:literal, $max:literal, $default:literal, $dec:literal, $signed:literal, $group:literal) => {
        ControlSpec {
            key: $key,
            label: $label,
            field: Field::$field,
            min: $min,
            max: $max,
            default: $default,
            decimals: $dec,
            signed: $signed,
            unit: "",
            tip: "",
            group: $group,
            grad: false,
            track: false,
        }
    };
}

/// 全表。顺序即面板绘制顺序（同组内连续）。
///
/// The whole table. Order is panel draw order (consecutive within a group).
pub const SPECS: &[ControlSpec] = &[
    // ── 基本 ────────────────────────────────────────────────
    // 新增影调参数按归属挨着主参数放（LR 的口径：rolloff 属于曝光、
    // 软拐点属于黑白场、线性混合属于对比度），不单开分组。
    spec!(
        "dev.exposure",
        "曝光",
        Exposure,
        -5.0,
        5.0,
        0.0,
        2,
        true,
        "basic"
    ),
    spec!(
        "dev.hl-rolloff",
        "高光过渡",
        HlRolloff,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "basic"
    ),
    spec!(
        "dev.contrast",
        "对比度",
        Contrast,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.contrast-linear",
        "线性对比",
        ContrastLinear,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "basic"
    ),
    spec!(
        "dev.highlights",
        "高光",
        Highlights,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.shadows",
        "阴影",
        Shadows,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.whites",
        "白色",
        Whites,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.blacks",
        "黑色",
        Blacks,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.black-soft",
        "黑场软化",
        BlackSoft,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "basic"
    ),
    spec!(
        "dev.white-soft",
        "白场软化",
        WhiteSoft,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "basic"
    ),
    // ── 色彩 ────────────────────────────────────────────────
    // 色温走非线性轨道：中段密两端疏，锚点表在 core 的 temp_track。
    // 色调本轮不动（需求只点名色温；表机制可复用，加一张表即可）。
    ControlSpec {
        grad: true,
        track: true,
        ..spec!(
            "dev.temperature",
            "色温",
            Temperature,
            -100.0,
            100.0,
            0.0,
            0,
            true,
            "basic"
        )
    },
    ControlSpec {
        grad: true,
        ..spec!(
            "dev.tint", "色调", Tint, -100.0, 100.0, 0.0, 0, true, "basic"
        )
    },
    spec!(
        "dev.vibrance",
        "自然饱和度",
        Vibrance,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.saturation",
        "饱和度",
        Saturation,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    // ── 效果 ────────────────────────────────────────────────
    spec!(
        "dev.dehaze",
        "去朦胧",
        Dehaze,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    // 暗通道去雾两枚滑杆紧挨「去朦胧」之下（LR 口径在 Basic 面板单滑杆，
    // 本仓库分组沿现状，不为单参数迁组）。
    spec!(
        "dev.dehaze-dark",
        "暗通道去雾",
        DehazeDark,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "basic"
    ),
    spec!(
        "dev.dehaze-contrast",
        "去雾对比",
        DehazeContrast,
        -50.0,
        50.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.vignette",
        "暗角",
        Vignette,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "effects"
    ),
    spec!(
        "dev.vig-mid",
        "暗角中点",
        VigMidpoint,
        0.0,
        100.0,
        50.0,
        0,
        false,
        "effects"
    ),
    spec!(
        "dev.vig-round",
        "圆度",
        VigRoundness,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "effects"
    ),
    spec!(
        "dev.vig-feather",
        "羽化",
        VigFeather,
        1.0,
        100.0,
        50.0,
        0,
        false,
        "effects"
    ),
    spec!(
        "dev.vig-hl",
        "高光保护",
        VigHlContrast,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "effects"
    ),
    // ── 细节 ────────────────────────────────────────────────
    spec!(
        "dev.texture",
        "纹理",
        Texture,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.clarity",
        "清晰度",
        Clarity,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "basic"
    ),
    spec!(
        "dev.sharp-amount",
        "锐化",
        SharpAmount,
        0.0,
        150.0,
        0.0,
        0,
        false,
        "detail"
    ),
    ControlSpec {
        unit: "px",
        ..spec!(
            "dev.sharp-radius",
            "锐化半径",
            SharpRadius,
            0.5,
            3.0,
            1.0,
            1,
            false,
            "detail"
        )
    },
    spec!(
        "dev.sharp-detail",
        "锐化细节",
        SharpDetail,
        0.0,
        100.0,
        25.0,
        0,
        false,
        "detail"
    ),
    spec!(
        "dev.sharp-mask",
        "锐化蒙版",
        SharpMasking,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "detail"
    ),
    // 锐化保护挨着蒙版放：两者都在回答"哪里不锐化"，保护的口径是噪声自适应恒开。
    spec!(
        "dev.sharp-protect",
        "锐化保护",
        SharpProtect,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "detail"
    ),
    spec!(
        "dev.nr-luma",
        "亮度降噪",
        NrLuma,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "detail"
    ),
    spec!(
        "dev.nr-color",
        "色彩降噪",
        NrColor,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "detail"
    ),
    spec!(
        "dev.nr-ai",
        "AI 降噪",
        NrAi,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "detail"
    ),
    spec!(
        "dev.nr-luma-detail",
        "降噪细节",
        NrLumaDetail,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "detail"
    ),
    spec!(
        "dev.nr-luma-contrast",
        "降噪对比",
        NrLumaContrast,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "detail"
    ),
    spec!(
        "dev.nr-color-smooth",
        "降噪平滑",
        NrColorSmooth,
        0.0,
        100.0,
        50.0,
        0,
        false,
        "detail"
    ),
    // 多尺度与手动 σ 紧跟降噪平滑：都是亮度降噪的升级量（设计 2026-10-30 §1）。
    spec!(
        "dev.nr-multiscale",
        "多尺度降噪",
        NrMultiscale,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "detail"
    ),
    ControlSpec {
        tip: "手动噪声 σ：0 = 自动估计（MAD）。滑杆低段（约 ≤28）因自动 σ 兜底门控暂不生效，待拟合标定。",
        ..spec!(
            "dev.nr-sigma-manual",
            "手动噪声σ",
            NrSigmaManual,
            0.0,
            100.0,
            0.0,
            0,
            false,
            "detail"
        )
    },
    // ── 颗粒 ────────────────────────────────────────────────
    spec!(
        "dev.grain",
        "颗粒",
        Grain,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "effects"
    ),
    // 亮度调制紧跟强度：正值暗部更重，负值反转（高光更重），双向。
    spec!(
        "dev.grain-lum",
        "颗粒亮度",
        GrainLum,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "effects"
    ),
    ControlSpec {
        unit: "px",
        ..spec!(
            "dev.grain-size",
            "颗粒大小",
            GrainSize,
            1.0,
            8.0,
            1.0,
            1,
            false,
            "effects"
        )
    },
    spec!(
        "dev.grain-freq",
        "颗粒密度",
        GrainFreq,
        0.2,
        1.0,
        1.0,
        2,
        false,
        "effects"
    ),
    // ── 镜头校正 ────────────────────────────────────────────
    spec!(
        "dev.lens-distort",
        "畸变",
        LensDistort,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "optics"
    ),
    spec!(
        "dev.lens-vignette",
        "暗角补偿",
        LensVignette,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "optics"
    ),
    spec!(
        "dev.lens-fringe",
        "去紫边",
        LensFringe,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "optics"
    ),
    // ── 色彩分级 ────────────────────────────────────────────
    spec!(
        "dev.grade-gl-h",
        "全局色相",
        GradeGlobalH,
        0.0,
        360.0,
        0.0,
        0,
        false,
        "grade"
    ),
    spec!(
        "dev.grade-gl-s",
        "全局浓度",
        GradeGlobalS,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "grade"
    ),
    spec!(
        "dev.grade-gl-l",
        "全局亮度",
        GradeGlobalL,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "grade"
    ),
    spec!(
        "dev.grade-bal",
        "分区平衡",
        GradeBalance,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "grade"
    ),
    spec!(
        "dev.grade-blend",
        "分区过渡",
        GradeBlending,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "grade"
    ),
    // ── 参数曲线 ────────────────────────────────────────────
    spec!(
        "dev.par-sh",
        "阴影区",
        ParShadows,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "curve"
    ),
    spec!(
        "dev.par-dark",
        "暗部区",
        ParDarks,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "curve"
    ),
    spec!(
        "dev.par-light",
        "亮部区",
        ParLights,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "curve"
    ),
    spec!(
        "dev.par-hi",
        "高光区",
        ParHighlights,
        -100.0,
        100.0,
        0.0,
        0,
        true,
        "curve"
    ),
    // ── 黑白 ────────────────────────────────────────────────
    spec!("dev.mono", "单色", Mono, 0.0, 100.0, 0.0, 0, false, "mixer"),
    // ── 曲线 ────────────────────────────────────────────────
    // 曲线组只有这一条推子（点曲线本体走 curve_box），挂组尾。
    spec!(
        "dev.curve-hdr-tail",
        "HDR 尾部",
        CurveHdrTail,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "curve"
    ),
];

/// 取一个字段的可变槽位。`group_changed` / `reset_group` / 推子渲染共用。
///
/// Mutable slot for one field. Shared by `group_changed`, `reset_group` and the
/// fader rendering.
pub fn param_slot(p: &mut GradeParams, f: Field) -> &mut f64 {
    match f {
        Field::Exposure => &mut p.exposure,
        Field::HlRolloff => &mut p.hl_rolloff,
        Field::Contrast => &mut p.contrast,
        Field::ContrastLinear => &mut p.contrast_linear,
        Field::Highlights => &mut p.highlights,
        Field::Shadows => &mut p.shadows,
        Field::Whites => &mut p.whites,
        Field::Blacks => &mut p.blacks,
        Field::BlackSoft => &mut p.black_soft,
        Field::WhiteSoft => &mut p.white_soft,
        Field::Temperature => &mut p.temperature,
        Field::Tint => &mut p.tint,
        Field::Vibrance => &mut p.vibrance,
        Field::Saturation => &mut p.saturation,
        Field::Clarity => &mut p.clarity,
        Field::Texture => &mut p.texture,
        Field::Dehaze => &mut p.dehaze,
        Field::DehazeDark => &mut p.dehaze_dark,
        Field::DehazeContrast => &mut p.dehaze_contrast,
        Field::Grain => &mut p.grain,
        Field::GrainLum => &mut p.grain_lum,
        Field::GrainSize => &mut p.grain_size,
        Field::GrainFreq => &mut p.grain_freq,
        Field::Vignette => &mut p.vignette,
        Field::VigMidpoint => &mut p.vig_midpoint,
        Field::VigRoundness => &mut p.vig_roundness,
        Field::VigFeather => &mut p.vig_feather,
        Field::VigHlContrast => &mut p.vig_hl_contrast,
        Field::SharpAmount => &mut p.sharp_amount,
        Field::SharpRadius => &mut p.sharp_radius,
        Field::SharpDetail => &mut p.sharp_detail,
        Field::SharpMasking => &mut p.sharp_masking,
        Field::SharpProtect => &mut p.sharp_protect,
        Field::NrLuma => &mut p.nr_luma,
        Field::NrColor => &mut p.nr_color,
        Field::NrLumaDetail => &mut p.nr_luma_detail,
        Field::NrLumaContrast => &mut p.nr_luma_contrast,
        Field::NrColorSmooth => &mut p.nr_color_smooth,
        Field::NrMultiscale => &mut p.nr_multiscale,
        Field::NrSigmaManual => &mut p.nr_sigma_manual,
        Field::NrAi => &mut p.nr_ai,
        Field::GradeGlobalH => &mut p.grade_global_h,
        Field::GradeGlobalS => &mut p.grade_global_s,
        Field::GradeGlobalL => &mut p.grade_global_l,
        Field::GradeBalance => &mut p.grade_balance,
        Field::GradeBlending => &mut p.grade_blending,
        Field::ParShadows => &mut p.par_shadows,
        Field::ParDarks => &mut p.par_darks,
        Field::ParLights => &mut p.par_lights,
        Field::ParHighlights => &mut p.par_highlights,
        Field::Mono => &mut p.mono,
        Field::LensDistort => &mut p.lens_distort,
        Field::LensVignette => &mut p.lens_vignette,
        Field::LensFringe => &mut p.lens_fringe,
        Field::CurveHdrTail => &mut p.curve_hdr_tail,
    }
}

/// 只读取数。分组圆点每帧要读几十个字段，不想要可变借用。
/// 与 `param_slot` 的映射必须一致——下方测试逐字段钉住。
///
/// Read-only accessor. The group dot reads dozens of fields per frame and does not
/// want a mutable borrow.
/// Must map identically to `param_slot` -- the test below pins it field by field.
pub fn param_get(p: &GradeParams, f: Field) -> f64 {
    *match f {
        Field::Exposure => &p.exposure,
        Field::HlRolloff => &p.hl_rolloff,
        Field::Contrast => &p.contrast,
        Field::ContrastLinear => &p.contrast_linear,
        Field::Highlights => &p.highlights,
        Field::Shadows => &p.shadows,
        Field::Whites => &p.whites,
        Field::Blacks => &p.blacks,
        Field::BlackSoft => &p.black_soft,
        Field::WhiteSoft => &p.white_soft,
        Field::Temperature => &p.temperature,
        Field::Tint => &p.tint,
        Field::Vibrance => &p.vibrance,
        Field::Saturation => &p.saturation,
        Field::Clarity => &p.clarity,
        Field::Texture => &p.texture,
        Field::Dehaze => &p.dehaze,
        Field::DehazeDark => &p.dehaze_dark,
        Field::DehazeContrast => &p.dehaze_contrast,
        Field::Grain => &p.grain,
        Field::GrainLum => &p.grain_lum,
        Field::GrainSize => &p.grain_size,
        Field::GrainFreq => &p.grain_freq,
        Field::Vignette => &p.vignette,
        Field::VigMidpoint => &p.vig_midpoint,
        Field::VigRoundness => &p.vig_roundness,
        Field::VigFeather => &p.vig_feather,
        Field::VigHlContrast => &p.vig_hl_contrast,
        Field::SharpAmount => &p.sharp_amount,
        Field::SharpRadius => &p.sharp_radius,
        Field::SharpDetail => &p.sharp_detail,
        Field::SharpMasking => &p.sharp_masking,
        Field::SharpProtect => &p.sharp_protect,
        Field::NrLuma => &p.nr_luma,
        Field::NrColor => &p.nr_color,
        Field::NrLumaDetail => &p.nr_luma_detail,
        Field::NrLumaContrast => &p.nr_luma_contrast,
        Field::NrColorSmooth => &p.nr_color_smooth,
        Field::NrMultiscale => &p.nr_multiscale,
        Field::NrSigmaManual => &p.nr_sigma_manual,
        Field::NrAi => &p.nr_ai,
        Field::GradeGlobalH => &p.grade_global_h,
        Field::GradeGlobalS => &p.grade_global_s,
        Field::GradeGlobalL => &p.grade_global_l,
        Field::GradeBalance => &p.grade_balance,
        Field::GradeBlending => &p.grade_blending,
        Field::ParShadows => &p.par_shadows,
        Field::ParDarks => &p.par_darks,
        Field::ParLights => &p.par_lights,
        Field::ParHighlights => &p.par_highlights,
        Field::Mono => &p.mono,
        Field::LensDistort => &p.lens_distort,
        Field::LensVignette => &p.lens_vignette,
        Field::LensFringe => &p.lens_fringe,
        Field::CurveHdrTail => &p.curve_hdr_tail,
    }
}

/// 某个分组下的全部规格，保持表序（即面板绘制序）。
///
/// Every spec in one group, preserving table order (that is, panel draw order).
pub fn specs_of_group(group: &str) -> impl Iterator<Item = &'static ControlSpec> {
    SPECS.iter().filter(move |s| s.group == group)
}

/// 按字段取规格：显影面板的分区按 ACR 口径用字段清单显式排布
/// （docs/界面布局-ACR化-2026-11-04.md §3），显示顺序与表内物理顺序解耦。
/// 入参用 `Field` 枚举而非字符串，清单写错字段编译期即报。
///
/// Look up a spec by field: the Develop panel lays its sub-sections out by explicit
/// field lists following ACR (plan §3), decoupling display order from table order.
/// Takes the `Field` enum instead of a string so a wrong entry fails at compile time.
pub fn spec_by_field(field: Field) -> Option<&'static ControlSpec> {
    SPECS.iter().find(|s| s.field == field)
}

/// 把一个分组的参数就地写回默认，返回这一组是否真有参数可复位——
/// 也就是「眼睛」旁路对这一组是否有意义。
///
/// 复位按钮（`shell::reset_group`）与预览旁路**必须**共用这一份口径：
/// 否则「眼睛关掉看到的样子」和「按复位之后的样子」会对不上，对比就在撒谎。
/// 返回 `true` 的分组恰好是显影八组（基本/曲线/混合器/分级/细节/光学/效果/外观）；
/// 序列、成片、音频那些组的参数不在 `GradeParams` 里，旁路对它们没有定义。
///
/// Write one group's parameters back to their defaults in place, returning whether the
/// group has parameters to reset at all -- that is, whether the "eye" bypass means
/// anything for it.
///
/// The reset button (`shell::reset_group`) and the preview bypass **must** share this
/// one implementation: otherwise what the eye shows and what a reset produces disagree,
/// and the comparison lies. The groups returning `true` are exactly the eight Develop
/// sections; sequence, render and audio groups keep their parameters outside
/// `GradeParams`, so bypass is undefined for them.
pub fn neutralise_group(p: &mut GradeParams, key: &str) -> bool {
    let d = GradeParams::default();
    let mut touched = false;
    // 推子参数组：按规格表写回——表里登记了哪些，这里就写回哪些，
    // 与面板推子、分组圆点、复位按钮共用同一份事实。
    for s in specs_of_group(key) {
        *param_slot(p, s.field) = s.default;
        touched = true;
    }
    // 表外的组内字段照旧手工写回。
    match key {
        "look" => {
            p.look_cube = String::new();
            touched = true;
        }
        // 曲线区三种模式同一个口径：参数曲线的三条分界、色阶的五档、
        // 点曲线的四通道一起回默认（与复位按钮一致）。
        //
        // One convention for all three Curve modes: parametric splits, the five levels
        // handles and the four point curves go back together (same as the reset button).
        "curve" => {
            p.par_split1 = d.par_split1;
            p.par_split2 = d.par_split2;
            p.par_split3 = d.par_split3;
            p.lv_black = d.lv_black;
            p.lv_white = d.lv_white;
            p.lv_gamma = d.lv_gamma;
            p.lv_out_black = d.lv_out_black;
            p.lv_out_white = d.lv_out_white;
            p.curve_rgb = d.curve_rgb;
            p.curve_r = d.curve_r;
            p.curve_g = d.curve_g;
            p.curve_b = d.curve_b;
            touched = true;
        }
        // 颜色混合器：HSL 八段与黑白八段一起回默认（黑白强度那条在表里已写回）。
        //
        // Color Mixer: the HSL and B&W band arrays go back together (the B&W strength
        // slider is in the spec table and already reset above).
        "mixer" => {
            p.hsl_h = d.hsl_h;
            p.hsl_s = d.hsl_s;
            p.hsl_l = d.hsl_l;
            p.mono_h = d.mono_h;
            touched = true;
        }
        "grade" => {
            p.grade_sh_h = d.grade_sh_h;
            p.grade_sh_s = d.grade_sh_s;
            p.grade_sh_l = d.grade_sh_l;
            p.grade_mid_h = d.grade_mid_h;
            p.grade_mid_s = d.grade_mid_s;
            p.grade_mid_l = d.grade_mid_l;
            p.grade_hi_h = d.grade_hi_h;
            p.grade_hi_s = d.grade_hi_s;
            p.grade_hi_l = d.grade_hi_l;
            touched = true;
        }
        _ => {}
    }
    touched
}

/// 「眼睛」旁路对这一组是否有意义：恰为显影八组。
///
/// Whether the "eye" bypass means anything for this group: exactly the eight Develop
/// sections.
pub fn is_bypassable(group: &str) -> bool {
    neutralise_group(&mut GradeParams::default(), group)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表里的 default 必须等于 `GradeParams::default()` 的同名字段。
    /// 面板推子的复位点、分组圆点的判断基准都用 spec.default，
    /// 这两处一旦和引擎默认漂移，「没动过的参数显示已改动」。
    #[test]
    fn spec_defaults_match_grade_defaults() {
        let d = GradeParams::default();
        for s in SPECS {
            let actual = *param_slot(&mut d.clone(), s.field);
            assert_eq!(
                s.default, actual,
                "spec {} 的 default 与 GradeParams::default 不一致",
                s.key
            );
        }
    }

    /// key 是持久 id，不允许重复；范围合法、默认在界内。
    #[test]
    fn spec_keys_are_unique_and_groups_known() {
        let mut seen = std::collections::HashSet::new();
        for s in SPECS {
            assert!(seen.insert(s.key), "重复的 spec key：{}", s.key);
            assert!(s.min < s.max, "spec {} 范围倒挂", s.key);
            assert!(
                s.default >= s.min && s.default <= s.max,
                "spec {} 默认值出界",
                s.key
            );
        }
    }

    /// param_get 与 param_slot 必须指向同一格：写入后读回应相等。
    #[test]
    fn param_get_and_slot_agree() {
        let mut p = GradeParams::default();
        for (i, s) in SPECS.iter().enumerate() {
            *param_slot(&mut p, s.field) = 100.0 + i as f64;
        }
        for (i, s) in SPECS.iter().enumerate() {
            assert_eq!(param_get(&p, s.field), 100.0 + i as f64, "{} 读错格", s.key);
        }
    }

    /// 复位按表走之后，「复位 == 全部回到默认」必须严格成立。
    #[test]
    fn resetting_every_group_restores_defaults() {
        use crate::state::AppState;
        let mut st = AppState::new();
        for s in SPECS {
            *param_slot(&mut st.params, s.field) = s.max * 0.8;
        }
        let groups: std::collections::HashSet<&str> = SPECS.iter().map(|s| s.group).collect();
        for g in groups {
            crate::shell::reset_group(&mut st, g);
        }
        for s in SPECS {
            assert_eq!(
                *param_slot(&mut st.params, s.field),
                s.default,
                "{} 未复位",
                s.key
            );
        }
    }
}
