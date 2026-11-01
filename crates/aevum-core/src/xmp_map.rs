//! `crs:` ↔ [`GradeParams`] 的数据驱动映射表。
//!
//! v0.3.0 的映射是 `xmp.rs` 里一条 if-let 硬编码链：每加一个键改一处逻辑，没写到的
//! 键被静默丢弃。现在只有这一张 [`SPECS`] 表——
//!
//! - **导入**：[`apply`] 按表序逐键换算写入字段；表与包元数据没认领的 `crs:` 键
//!   由调用方收进 `GradeParams::xmp_extra`，往返不丢；
//! - **导出**：[`emit_attrs`] 按同一张表反算回 `crs:` 属性，再把 `xmp_extra` 原样
//!   补写（表键与包元数据优先，防双写）。
//!
//! 表序有两层含义，新加键前先读这两条：
//! 1. **导入时后者覆盖前者**：同一字段有多条键（PV2010 兜底、SplitToning/ColorGrade
//!    别名、绝对/增量白平衡）时，优先级高的那条必须排在后面；
//! 2. **导出顺序按表序**：排完导入优先级后，表的顺序同时决定了写出属性的顺序，
//!    必须与 v0.3.0 的导出序一致（Lightroom 与本项目的回归测试都钉着它）。
//!
//! 每条映射带一行注释说明 LR 侧含义与缩放依据。数值换算一律是
//! `value = clip(raw * scale + offset, min, max)`，导出取逆。
//!
//! Data-driven mapping table between `crs:` and [`GradeParams`].
//!
//! The v0.3.0 mapping was a hard-coded if-let chain in `xmp.rs`: every added key meant a
//! logic edit, and keys nobody wrote stayed silently dropped. Now there is only this
//! [`SPECS`] table:
//!
//! - **Import**: [`apply`] walks the table in order, converting each key into a field;
//!   `crs:` keys claimed by neither the table nor the package metadata are collected by
//!   the caller into `GradeParams::xmp_extra`, so a round trip loses nothing;
//! - **Export**: [`emit_attrs`] inverts the same table back into `crs:` attributes, then
//!   appends `xmp_extra` verbatim (table keys and package metadata take precedence,
//!   which rules out double writes).
//!
//! Table order carries two meanings; read both before adding a key:
//! 1. **A later entry overrides an earlier one on import**: when one field has several
//!    keys (PV2010 fallback, SplitToning/ColorGrade aliases, absolute versus incremental
//!    white balance), the higher-priority entry must come later;
//! 2. **Export order follows table order**: once import priority is settled, the table
//!    order also fixes the order in which attributes are written, and it must match the
//!    v0.3.0 export order (the Lightroom and project regression tests both pin it).
//!
//! Each mapping carries a one-line note on the LR-side meaning and the scaling basis.
//! Every numeric conversion is `value = clip(raw * scale + offset, min, max)`, and export
//! takes the inverse.

use crate::grade::GradeParams;

/// `crs:` 命名空间 URI。
///
/// The `crs:` namespace URI.
pub const CRS_NS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";

/// XMP 的 8 个色相带，顺序与 `grade::HSL_NAMES` 一致。
///
/// The 8 hue bands of XMP, in the same order as `grade::HSL_NAMES`.
pub const BANDS: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];

/// 包元数据键：不是调色参数，导入不收进 `xmp_extra`，导出由包骨架统一写。
///
/// Package metadata keys: not grading parameters, so import does not collect them into
/// `xmp_extra`, and the package skeleton writes them as a whole on export.
pub const RESERVED_KEYS: &[&str] = &["PresetType", "Version", "ProcessVersion", "HasSettings"];

/// 不夹取（当前 v0.3.0 导入对这批键不裁剪，保持兼容；LR 的名义量程写在 note 里）。
const NO_MIN: f64 = f64::NEG_INFINITY;
const NO_MAX: f64 = f64::INFINITY;

/// 目标槽位。数组字段带下标，曲线是子元素特例、不走这张表。
///
/// Target slot. Array fields carry an index; curves are a sub-element special case and do
/// not go through this table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrsField {
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    Temperature,
    Tint,
    Vibrance,
    Saturation,
    Clarity,
    Texture,
    Dehaze,
    Vignette,
    VigMidpoint,
    VigRoundness,
    VigFeather,
    VigHlContrast,
    SharpAmount,
    SharpRadius,
    SharpDetail,
    SharpMasking,
    NrLuma,
    NrColor,
    NrLumaDetail,
    NrLumaContrast,
    NrColorSmooth,
    Grain,
    HslH(usize),
    HslS(usize),
    HslL(usize),
    Mono,
    MonoH(usize),
    ParShadows,
    ParDarks,
    ParLights,
    ParHighlights,
    ParSplit1,
    ParSplit2,
    ParSplit3,
    GradeShH,
    GradeShS,
    GradeShL,
    GradeMidH,
    GradeMidS,
    GradeMidL,
    GradeHiH,
    GradeHiS,
    GradeHiL,
    GradeGlobalH,
    GradeGlobalS,
    GradeGlobalL,
    GradeBalance,
    GradeBlending,
    LensFringe,
}

/// 应用条件。
///
/// Application condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cond {
    /// 只要键存在就应用。
    ///
    /// Applies whenever the key exists.
    Always,
    /// `crs:WhiteBalance == "Custom"` 才应用（绝对开尔文只对自定白平衡有意义）。
    ///
    /// Applies only when `crs:WhiteBalance == "Custom"`; absolute kelvin means something
    /// for a custom white balance alone.
    WbCustom,
    /// PV2010 旧键兜底：同槽位的 PV2012 主键存在时不应用。
    ///
    /// PV2010 legacy fallback: skipped when the PV2012 primary key for the same slot
    /// exists.
    Pv2010Fallback,
}

/// 特例规则。绝大多数键是 [`Rule::Linear`]；其余见各条注释。
///
/// Special-case rule. Nearly every key is [`Rule::Linear`]; the remaining variants are
/// described on the variants themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// `value = clip(raw * scale + offset, min, max)`，导出按同界取逆。
    ///
    /// `value = clip(raw * scale + offset, min, max)`; export takes the inverse with the
    /// same bounds.
    Linear,
    /// 只读不写：导入参与换算（或只作条件输入），导出绝不写出该键。
    /// 用于 PV2010 兜底键、绝对开尔文（导出写增量）、ColorGrade 别名（导出写 SplitToning 名）。
    ///
    /// Read only: import converts it (or uses it as a condition input only); export never
    /// writes the key. Used for PV2010 fallback keys, absolute kelvin (export writes the
    /// incremental key), and ColorGrade aliases (export writes the SplitToning name).
    ReadOnly,
    /// 布尔转单色：`True` → `mono = 100`。
    ///
    /// Boolean to monochrome: `True` → `mono = 100`.
    BoolToMono,
    /// 去边的两键取大者合成一个槽位（LR 的 Purple/Green 两个量共享一个强度）。
    ///
    /// The two defringe keys collapse into one slot by taking the larger (LR shares one
    /// strength between Purple and Green).
    DefringeMax,
    /// 纯条件输入：不映射任何字段，导出由别处统一写（WhiteBalance 随 WB 组）。
    ///
    /// Pure condition input: maps no field, and export writes it as a group elsewhere
    /// (WhiteBalance travels with the WB group).
    Skip,
}

/// 一条映射的完整规格，风格对齐 `aevum_ui::controls::ControlSpec`。
///
/// The full spec of one mapping, styled after `aevum_ui::controls::ControlSpec`.
#[derive(Clone, Copy, Debug)]
pub struct CrsSpec {
    /// `crs:` 键名，如 `"Exposure2012"`。
    ///
    /// The `crs:` key name, such as `"Exposure2012"`.
    pub key: &'static str,
    pub field: CrsField,
    /// `value = raw * scale + offset`（含方向；暗角是 -1，开尔文是 1/45 配偏移）。
    ///
    /// `value = raw * scale + offset` (sign included; vignetting is -1, kelvin is 1/45
    /// paired with an offset).
    pub scale: f64,
    pub offset: f64,
    pub min: f64,
    pub max: f64,
    pub cond: Cond,
    pub rule: Rule,
    /// 一行注释：LR 侧语义与缩放依据。
    ///
    /// One-line note: LR-side semantics and the scaling basis.
    pub note: &'static str,
}

const fn lin(
    key: &'static str,
    field: CrsField,
    scale: f64,
    min: f64,
    max: f64,
    note: &'static str,
) -> CrsSpec {
    CrsSpec {
        key,
        field,
        scale,
        offset: 0.0,
        min,
        max,
        cond: Cond::Always,
        rule: Rule::Linear,
        note,
    }
}

const fn ro(mut spec: CrsSpec) -> CrsSpec {
    spec.rule = Rule::ReadOnly;
    spec
}

const fn cond(mut spec: CrsSpec, c: Cond) -> CrsSpec {
    spec.cond = c;
    spec
}

const fn offset(mut spec: CrsSpec, offset: f64) -> CrsSpec {
    spec.offset = offset;
    spec
}

const fn rule(mut spec: CrsSpec, r: Rule) -> CrsSpec {
    spec.rule = r;
    spec
}

/// 单条色带映射：键名 = 前缀 + 色带名，全部同刻度。
macro_rules! band {
    ($prefix:literal, $band:literal, $field:expr, $lo:literal, $hi:literal, $note:literal) => {
        lin(concat!($prefix, $band), $field, 1.0, $lo, $hi, $note)
    };
}

/// 全表。顺序即应用顺序（后者覆盖）兼导出顺序——两头的约束见模块头。
///
/// The whole table. Order is application order (a later entry wins) and export order at
/// once; see the module header for both constraints.
pub const SPECS: &[CrsSpec] = &[
    // ── PV2012 六件套（LR 基本面板，同刻度）────────────────────────
    lin(
        "Exposure2012",
        CrsField::Exposure,
        1.0,
        NO_MIN,
        NO_MAX,
        "曝光，LR ±5 同刻度；导入不裁剪（兼容 v0.3.0）",
    ),
    lin(
        "Contrast2012",
        CrsField::Contrast,
        1.0,
        NO_MIN,
        NO_MAX,
        "对比度，LR ±100 同刻度",
    ),
    lin(
        "Highlights2012",
        CrsField::Highlights,
        1.0,
        NO_MIN,
        NO_MAX,
        "高光，LR ±100 同刻度",
    ),
    lin(
        "Shadows2012",
        CrsField::Shadows,
        1.0,
        NO_MIN,
        NO_MAX,
        "阴影，LR ±100 同刻度",
    ),
    lin(
        "Whites2012",
        CrsField::Whites,
        1.0,
        NO_MIN,
        NO_MAX,
        "白色，LR ±100 同刻度",
    ),
    lin(
        "Blacks2012",
        CrsField::Blacks,
        1.0,
        NO_MIN,
        NO_MAX,
        "黑色，LR ±100 同刻度",
    ),
    // ── 效果三件套 ────────────────────────────────────────────────
    lin(
        "Texture",
        CrsField::Texture,
        1.0,
        NO_MIN,
        NO_MAX,
        "纹理，LR ±100 同刻度",
    ),
    lin(
        "Clarity2012",
        CrsField::Clarity,
        1.0,
        NO_MIN,
        NO_MAX,
        "清晰度，LR ±100 同刻度",
    ),
    lin(
        "Dehaze",
        CrsField::Dehaze,
        1.0,
        NO_MIN,
        NO_MAX,
        "去朦胧，LR ±100 同刻度",
    ),
    // ── 全局饱和 ─────────────────────────────────────────────────
    lin(
        "Vibrance",
        CrsField::Vibrance,
        1.0,
        NO_MIN,
        NO_MAX,
        "自然饱和度，LR ±100 同刻度",
    ),
    lin(
        "Saturation",
        CrsField::Saturation,
        1.0,
        NO_MIN,
        NO_MAX,
        "饱和度，LR ±100 同刻度",
    ),
    // ── 锐化四件套（LR 细节面板，同刻度）──────────────────────────
    lin(
        "Sharpness",
        CrsField::SharpAmount,
        1.0,
        NO_MIN,
        NO_MAX,
        "锐化数量，LR 名义 0..150",
    ),
    lin(
        "SharpenRadius",
        CrsField::SharpRadius,
        1.0,
        NO_MIN,
        NO_MAX,
        "锐化半径，LR 名义 0.5..3",
    ),
    lin(
        "SharpenDetail",
        CrsField::SharpDetail,
        1.0,
        NO_MIN,
        NO_MAX,
        "锐化细节，LR 名义 0..100",
    ),
    lin(
        "SharpenEdgeMasking",
        CrsField::SharpMasking,
        1.0,
        NO_MIN,
        NO_MAX,
        "锐化蒙版，LR 名义 0..100",
    ),
    // ── 降噪（LR 亮度平滑 ≈ 亮度降噪，其余同刻度）─────────────────
    lin(
        "LuminanceSmoothing",
        CrsField::NrLuma,
        1.0,
        NO_MIN,
        NO_MAX,
        "亮度平滑 ≈ 亮度降噪，LR 名义 0..100",
    ),
    lin(
        "LuminanceNoiseReductionDetail",
        CrsField::NrLumaDetail,
        1.0,
        NO_MIN,
        NO_MAX,
        "降噪细节",
    ),
    lin(
        "LuminanceNoiseReductionContrast",
        CrsField::NrLumaContrast,
        1.0,
        NO_MIN,
        NO_MAX,
        "降噪对比",
    ),
    lin(
        "ColorNoiseReduction",
        CrsField::NrColor,
        1.0,
        NO_MIN,
        NO_MAX,
        "色彩降噪，LR 名义 0..100",
    ),
    lin(
        "ColorNoiseReductionSmoothness",
        CrsField::NrColorSmooth,
        1.0,
        NO_MIN,
        NO_MAX,
        "色彩降噪平滑",
    ),
    // ── 暗角（LR 正数提亮四角，本项目正数压暗，方向相反）──────────
    lin(
        "PostCropVignetteMidpoint",
        CrsField::VigMidpoint,
        1.0,
        NO_MIN,
        NO_MAX,
        "暗角中点，LR 名义 0..100",
    ),
    lin(
        "PostCropVignetteFeather",
        CrsField::VigFeather,
        1.0,
        NO_MIN,
        NO_MAX,
        "暗角羽化，LR 名义 0..100",
    ),
    lin(
        "PostCropVignetteAmount",
        CrsField::Vignette,
        -1.0,
        -100.0,
        100.0,
        "暗角量：LR 正=提角、本项目正=压暗，取负号",
    ),
    lin(
        "PostCropVignetteHighlightContrast",
        CrsField::VigHlContrast,
        1.0,
        0.0,
        100.0,
        "暗角高光保护，LR 0..100",
    ),
    lin(
        "PostCropVignetteRoundness",
        CrsField::VigRoundness,
        1.0,
        0.0,
        100.0,
        "暗角圆度，LR 0..100",
    ),
    // ── 白平衡：绝对开尔文只对 Custom 生效；增量键排在后面覆盖绝对值
    //    （与 v0.3.0 的 if/else 优先序一致，导出只写增量键）────────────
    // 绝对开尔文：value = K/45 − 5500/45 = (K−5500)/45，与 v0.3.0 的公式一致。
    ro(cond(
        offset(
            lin(
                "Temperature",
                CrsField::Temperature,
                1.0 / 45.0,
                -100.0,
                100.0,
                "绝对开尔文 ≈ (K−5500)/45，仅 Custom 白平衡；导出走增量键",
            ),
            -5500.0 / 45.0,
        ),
        Cond::WbCustom,
    )),
    ro(cond(
        lin(
            "Tint",
            CrsField::Tint,
            1.0 / 1.5,
            -100.0,
            100.0,
            "LR 绿品刻度 ≈ 1.5×，即 scale=1/1.5；仅 Custom 白平衡；导出走增量键",
        ),
        Cond::WbCustom,
    )),
    lin(
        "IncrementalTemperature",
        CrsField::Temperature,
        1.0,
        -100.0,
        100.0,
        "增量色温直取；排后覆盖绝对值",
    ),
    lin(
        "IncrementalTint",
        CrsField::Tint,
        1.0,
        -100.0,
        100.0,
        "增量色调直取；排后覆盖绝对值",
    ),
    // ── HSL 8 带（LR HSL 面板，同刻度）────────────────────────────
    band!(
        "HueAdjustment",
        "Red",
        CrsField::HslH(0),
        -100.0,
        100.0,
        "HSL 色相 8 带"
    ),
    band!(
        "HueAdjustment",
        "Orange",
        CrsField::HslH(1),
        -100.0,
        100.0,
        "HSL 色相 8 带"
    ),
    band!(
        "HueAdjustment",
        "Yellow",
        CrsField::HslH(2),
        -100.0,
        100.0,
        "HSL 色相 8 带"
    ),
    band!(
        "HueAdjustment",
        "Green",
        CrsField::HslH(3),
        -100.0,
        100.0,
        "HSL 色相 8 带"
    ),
    band!(
        "HueAdjustment",
        "Aqua",
        CrsField::HslH(4),
        -100.0,
        100.0,
        "HSL 色相 8 带"
    ),
    band!(
        "HueAdjustment",
        "Blue",
        CrsField::HslH(5),
        -100.0,
        100.0,
        "HSL 色相 8 带"
    ),
    band!(
        "HueAdjustment",
        "Purple",
        CrsField::HslH(6),
        -100.0,
        100.0,
        "HSL 色相 8 带"
    ),
    band!(
        "HueAdjustment",
        "Magenta",
        CrsField::HslH(7),
        -100.0,
        100.0,
        "HSL 色相 8 带"
    ),
    band!(
        "SaturationAdjustment",
        "Red",
        CrsField::HslS(0),
        -100.0,
        100.0,
        "HSL 饱和 8 带"
    ),
    band!(
        "SaturationAdjustment",
        "Orange",
        CrsField::HslS(1),
        -100.0,
        100.0,
        "HSL 饱和 8 带"
    ),
    band!(
        "SaturationAdjustment",
        "Yellow",
        CrsField::HslS(2),
        -100.0,
        100.0,
        "HSL 饱和 8 带"
    ),
    band!(
        "SaturationAdjustment",
        "Green",
        CrsField::HslS(3),
        -100.0,
        100.0,
        "HSL 饱和 8 带"
    ),
    band!(
        "SaturationAdjustment",
        "Aqua",
        CrsField::HslS(4),
        -100.0,
        100.0,
        "HSL 饱和 8 带"
    ),
    band!(
        "SaturationAdjustment",
        "Blue",
        CrsField::HslS(5),
        -100.0,
        100.0,
        "HSL 饱和 8 带"
    ),
    band!(
        "SaturationAdjustment",
        "Purple",
        CrsField::HslS(6),
        -100.0,
        100.0,
        "HSL 饱和 8 带"
    ),
    band!(
        "SaturationAdjustment",
        "Magenta",
        CrsField::HslS(7),
        -100.0,
        100.0,
        "HSL 饱和 8 带"
    ),
    band!(
        "LuminanceAdjustment",
        "Red",
        CrsField::HslL(0),
        -100.0,
        100.0,
        "HSL 亮度 8 带"
    ),
    band!(
        "LuminanceAdjustment",
        "Orange",
        CrsField::HslL(1),
        -100.0,
        100.0,
        "HSL 亮度 8 带"
    ),
    band!(
        "LuminanceAdjustment",
        "Yellow",
        CrsField::HslL(2),
        -100.0,
        100.0,
        "HSL 亮度 8 带"
    ),
    band!(
        "LuminanceAdjustment",
        "Green",
        CrsField::HslL(3),
        -100.0,
        100.0,
        "HSL 亮度 8 带"
    ),
    band!(
        "LuminanceAdjustment",
        "Aqua",
        CrsField::HslL(4),
        -100.0,
        100.0,
        "HSL 亮度 8 带"
    ),
    band!(
        "LuminanceAdjustment",
        "Blue",
        CrsField::HslL(5),
        -100.0,
        100.0,
        "HSL 亮度 8 带"
    ),
    band!(
        "LuminanceAdjustment",
        "Purple",
        CrsField::HslL(6),
        -100.0,
        100.0,
        "HSL 亮度 8 带"
    ),
    band!(
        "LuminanceAdjustment",
        "Magenta",
        CrsField::HslL(7),
        -100.0,
        100.0,
        "HSL 亮度 8 带"
    ),
    // ── 黑白（LR B&W 面板）───────────────────────────────────────
    rule(
        lin(
            "ConvertToGrayscale",
            CrsField::Mono,
            1.0,
            NO_MIN,
            NO_MAX,
            "转黑白：True → mono=100",
        ),
        Rule::BoolToMono,
    ),
    band!(
        "GrayMixer",
        "Red",
        CrsField::MonoH(0),
        -100.0,
        100.0,
        "黑白混合 8 带"
    ),
    band!(
        "GrayMixer",
        "Orange",
        CrsField::MonoH(1),
        -100.0,
        100.0,
        "黑白混合 8 带"
    ),
    band!(
        "GrayMixer",
        "Yellow",
        CrsField::MonoH(2),
        -100.0,
        100.0,
        "黑白混合 8 带"
    ),
    band!(
        "GrayMixer",
        "Green",
        CrsField::MonoH(3),
        -100.0,
        100.0,
        "黑白混合 8 带"
    ),
    band!(
        "GrayMixer",
        "Aqua",
        CrsField::MonoH(4),
        -100.0,
        100.0,
        "黑白混合 8 带"
    ),
    band!(
        "GrayMixer",
        "Blue",
        CrsField::MonoH(5),
        -100.0,
        100.0,
        "黑白混合 8 带"
    ),
    band!(
        "GrayMixer",
        "Purple",
        CrsField::MonoH(6),
        -100.0,
        100.0,
        "黑白混合 8 带"
    ),
    band!(
        "GrayMixer",
        "Magenta",
        CrsField::MonoH(7),
        -100.0,
        100.0,
        "黑白混合 8 带"
    ),
    // ── 参数曲线四区 + 三分割点（分割点键存在才动，缺省 25/50/75）──
    lin(
        "ParametricShadows",
        CrsField::ParShadows,
        1.0,
        -100.0,
        100.0,
        "参数曲线阴影区",
    ),
    lin(
        "ParametricDarks",
        CrsField::ParDarks,
        1.0,
        -100.0,
        100.0,
        "参数曲线暗部区",
    ),
    lin(
        "ParametricLights",
        CrsField::ParLights,
        1.0,
        -100.0,
        100.0,
        "参数曲线亮部区",
    ),
    lin(
        "ParametricHighlights",
        CrsField::ParHighlights,
        1.0,
        -100.0,
        100.0,
        "参数曲线高光区",
    ),
    lin(
        "ParametricShadowSplit",
        CrsField::ParSplit1,
        1.0,
        2.0,
        98.0,
        "分割点 1，缺省 25 不动",
    ),
    lin(
        "ParametricMidtoneSplit",
        CrsField::ParSplit2,
        1.0,
        2.0,
        98.0,
        "分割点 2，缺省 50 不动",
    ),
    lin(
        "ParametricHighlightSplit",
        CrsField::ParSplit3,
        1.0,
        2.0,
        98.0,
        "分割点 3，缺省 75 不动",
    ),
    // ── 分级（SplitToning=旧名、ColorGrade=新名）：
    //    色相 ColorGrade 优先、饱和 SplitToning 优先（与 v0.3.0 的 or_else 链一致，
    //    靠「后者覆盖」表达优先级；ColorGrade 别名导出不写）────────────
    lin(
        "SplitToningShadowHue",
        CrsField::GradeShH,
        1.0,
        0.0,
        360.0,
        "阴影色相（旧名）",
    ),
    ro(lin(
        "ColorGradeShadowHue",
        CrsField::GradeShH,
        1.0,
        0.0,
        360.0,
        "阴影色相（新名，读入优先）",
    )),
    ro(lin(
        "ColorGradeShadowSat",
        CrsField::GradeShS,
        1.0,
        0.0,
        100.0,
        "阴影饱和（新名，SplitToning 优先）",
    )),
    lin(
        "SplitToningShadowSaturation",
        CrsField::GradeShS,
        1.0,
        0.0,
        100.0,
        "阴影饱和（旧名，排后优先）",
    ),
    lin(
        "SplitToningHighlightHue",
        CrsField::GradeHiH,
        1.0,
        0.0,
        360.0,
        "高光色相（旧名）",
    ),
    ro(lin(
        "ColorGradeHighlightHue",
        CrsField::GradeHiH,
        1.0,
        0.0,
        360.0,
        "高光色相（新名，读入优先）",
    )),
    ro(lin(
        "ColorGradeHighlightSat",
        CrsField::GradeHiS,
        1.0,
        0.0,
        100.0,
        "高光饱和（新名，SplitToning 优先）",
    )),
    lin(
        "SplitToningHighlightSaturation",
        CrsField::GradeHiS,
        1.0,
        0.0,
        100.0,
        "高光饱和（旧名，排后优先）",
    ),
    lin(
        "SplitToningBalance",
        CrsField::GradeBalance,
        1.0,
        -100.0,
        100.0,
        "分区平衡",
    ),
    lin(
        "ColorGradeMidtoneHue",
        CrsField::GradeMidH,
        1.0,
        0.0,
        360.0,
        "中间调色相",
    ),
    lin(
        "ColorGradeMidtoneSat",
        CrsField::GradeMidS,
        1.0,
        0.0,
        100.0,
        "中间调饱和",
    ),
    lin(
        "ColorGradeShadowLum",
        CrsField::GradeShL,
        1.0,
        -100.0,
        100.0,
        "阴影亮度",
    ),
    lin(
        "ColorGradeMidtoneLum",
        CrsField::GradeMidL,
        1.0,
        -100.0,
        100.0,
        "中间调亮度",
    ),
    lin(
        "ColorGradeHighlightLum",
        CrsField::GradeHiL,
        1.0,
        -100.0,
        100.0,
        "高光亮度",
    ),
    lin(
        "ColorGradeBlending",
        CrsField::GradeBlending,
        1.0,
        0.0,
        100.0,
        "分区过渡",
    ),
    lin(
        "ColorGradeGlobalHue",
        CrsField::GradeGlobalH,
        1.0,
        0.0,
        360.0,
        "全局色相",
    ),
    lin(
        "ColorGradeGlobalSat",
        CrsField::GradeGlobalS,
        1.0,
        0.0,
        100.0,
        "全局饱和",
    ),
    lin(
        "ColorGradeGlobalLum",
        CrsField::GradeGlobalL,
        1.0,
        -100.0,
        100.0,
        "全局亮度",
    ),
    // ── 去边：LR 的 Purple/Green 两键取大者合成一个强度；LR 20 个色阶
    //    对应本项目 100%，scale=5 ────────────────────────────────────
    rule(
        lin(
            "DefringePurpleAmount",
            CrsField::LensFringe,
            5.0,
            0.0,
            100.0,
            "去紫边（20 色阶 = 100%）",
        ),
        Rule::DefringeMax,
    ),
    rule(
        lin(
            "DefringeGreenAmount",
            CrsField::LensFringe,
            5.0,
            0.0,
            100.0,
            "去绿边（20 色阶 = 100%）",
        ),
        Rule::DefringeMax,
    ),
    // ── 颗粒：导出按 v0.3.0 的习惯排在最后 ───────────────────────
    lin(
        "GrainAmount",
        CrsField::Grain,
        1.0,
        0.0,
        100.0,
        "颗粒数量，LR 0..100",
    ),
    // ── PV2010 兜底：PV2012 主键缺席时才生效 ─────────────────────
    ro(cond(
        lin(
            "Exposure",
            CrsField::Exposure,
            1.0,
            NO_MIN,
            NO_MAX,
            "PV2010 曝光兜底",
        ),
        Cond::Pv2010Fallback,
    )),
    ro(cond(
        lin(
            "Contrast",
            CrsField::Contrast,
            1.0,
            NO_MIN,
            NO_MAX,
            "PV2010 对比兜底",
        ),
        Cond::Pv2010Fallback,
    )),
    // ── 条件输入：只作 WbCustom 的判据，不映射字段、导出由 WB 组统一写 ──
    rule(
        lin(
            "WhiteBalance",
            CrsField::Temperature,
            0.0,
            NO_MIN,
            NO_MAX,
            "白平衡名，只作条件输入",
        ),
        Rule::Skip,
    ),
];

/// 表里有没有这个键（未知键收集与导出去重的判据）。
///
/// Whether the table holds this key; the test for collecting unknown keys and for
/// de-duplicating on export.
pub fn has_spec(key: &str) -> bool {
    SPECS.iter().any(|s| s.key == key)
}

/// 是不是包元数据键（导入不收、导出由包骨架写）。
///
/// Whether this is a package metadata key: import skips it, and the package skeleton
/// writes it on export.
pub fn is_reserved(key: &str) -> bool {
    RESERVED_KEYS.contains(&key)
}

/// PV2010 兜底键对应槽位的 PV2012 主键。
fn pv2012_primary(field: CrsField) -> &'static str {
    match field {
        CrsField::Exposure => "Exposure2012",
        CrsField::Contrast => "Contrast2012",
        _ => "",
    }
}

/// 字段槽位。与 `xmp.rs` 的读/写共用，映射与 `CrsField` 一一对应。
///
/// Field slot. Shared with the read and write paths in `xmp.rs`; mappings line up with
/// `CrsField` one to one.
pub fn field_slot(p: &mut GradeParams, f: CrsField) -> &mut f64 {
    match f {
        CrsField::Exposure => &mut p.exposure,
        CrsField::Contrast => &mut p.contrast,
        CrsField::Highlights => &mut p.highlights,
        CrsField::Shadows => &mut p.shadows,
        CrsField::Whites => &mut p.whites,
        CrsField::Blacks => &mut p.blacks,
        CrsField::Temperature => &mut p.temperature,
        CrsField::Tint => &mut p.tint,
        CrsField::Vibrance => &mut p.vibrance,
        CrsField::Saturation => &mut p.saturation,
        CrsField::Clarity => &mut p.clarity,
        CrsField::Texture => &mut p.texture,
        CrsField::Dehaze => &mut p.dehaze,
        CrsField::Vignette => &mut p.vignette,
        CrsField::VigMidpoint => &mut p.vig_midpoint,
        CrsField::VigRoundness => &mut p.vig_roundness,
        CrsField::VigFeather => &mut p.vig_feather,
        CrsField::VigHlContrast => &mut p.vig_hl_contrast,
        CrsField::SharpAmount => &mut p.sharp_amount,
        CrsField::SharpRadius => &mut p.sharp_radius,
        CrsField::SharpDetail => &mut p.sharp_detail,
        CrsField::SharpMasking => &mut p.sharp_masking,
        CrsField::NrLuma => &mut p.nr_luma,
        CrsField::NrColor => &mut p.nr_color,
        CrsField::NrLumaDetail => &mut p.nr_luma_detail,
        CrsField::NrLumaContrast => &mut p.nr_luma_contrast,
        CrsField::NrColorSmooth => &mut p.nr_color_smooth,
        CrsField::Grain => &mut p.grain,
        CrsField::HslH(i) => &mut p.hsl_h[i],
        CrsField::HslS(i) => &mut p.hsl_s[i],
        CrsField::HslL(i) => &mut p.hsl_l[i],
        CrsField::Mono => &mut p.mono,
        CrsField::MonoH(i) => &mut p.mono_h[i],
        CrsField::ParShadows => &mut p.par_shadows,
        CrsField::ParDarks => &mut p.par_darks,
        CrsField::ParLights => &mut p.par_lights,
        CrsField::ParHighlights => &mut p.par_highlights,
        CrsField::ParSplit1 => &mut p.par_split1,
        CrsField::ParSplit2 => &mut p.par_split2,
        CrsField::ParSplit3 => &mut p.par_split3,
        CrsField::GradeShH => &mut p.grade_sh_h,
        CrsField::GradeShS => &mut p.grade_sh_s,
        CrsField::GradeShL => &mut p.grade_sh_l,
        CrsField::GradeMidH => &mut p.grade_mid_h,
        CrsField::GradeMidS => &mut p.grade_mid_s,
        CrsField::GradeMidL => &mut p.grade_mid_l,
        CrsField::GradeHiH => &mut p.grade_hi_h,
        CrsField::GradeHiS => &mut p.grade_hi_s,
        CrsField::GradeHiL => &mut p.grade_hi_l,
        CrsField::GradeGlobalH => &mut p.grade_global_h,
        CrsField::GradeGlobalS => &mut p.grade_global_s,
        CrsField::GradeGlobalL => &mut p.grade_global_l,
        CrsField::GradeBalance => &mut p.grade_balance,
        CrsField::GradeBlending => &mut p.grade_blending,
        CrsField::LensFringe => &mut p.lens_fringe,
    }
}

// ---------------------------------------------------------------- 数值

/// 解 `crs:` 数值：与 Python 参考实现一致——容忍 BOM 与空白，解析失败当 0。
///
/// Parses a `crs:` number exactly as the Python reference does: tolerates a BOM and
/// surrounding whitespace, and treats a parse failure as 0.
pub fn num(raw: &str) -> f64 {
    raw.trim()
        .trim_start_matches('\u{feff}')
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
}

fn clip(v: f64, lo: f64, hi: f64) -> f64 {
    v.max(lo).min(hi)
}

/// Python 的 `round()` 是银行家舍入（.5 取偶），导出格式化依赖它。
///
/// Python's `round()` uses banker's rounding (a .5 goes to the even neighbour), and the
/// export formatting depends on it.
pub fn round_half_even(v: f64) -> f64 {
    let floor = v.floor();
    if (v - floor).abs() == 0.5 {
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        v.round()
    }
}

/// 与 Python `_fmt` 一致：整数不带小数点，其余最多 6 位小数并去掉尾零。
///
/// Matches Python `_fmt`: an integer carries no decimal point, and any other value keeps
/// at most 6 decimals with trailing zeros stripped.
pub fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_owned();
    }
    let rounded = round_half_even(v);
    if (v - rounded).abs() < 1e-9 {
        return format!("{}", rounded as i64);
    }
    let text = format!("{v:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

/// LR 的布尔字面量（`True`/`False`，容忍大小写与 1/0）。
fn truthy(raw: Option<&str>) -> bool {
    match raw {
        Some(text) => matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        ),
        None => false,
    }
}

// ---------------------------------------------------------------- 导入

/// 按表把 `crs:` 键应用到参数。`get` 由调用方给（XMP 解析出的键值对，
/// 属性与无子节点的子元素已合并）；键不存在返回 `None`，该条跳过。
///
/// 返回值的生命周期与映射表无关（值属于调用方的键值表），所以单独标 `'map`，
/// 闭包不必为了签名把借用拉成 `'static`。
///
/// Applies the `crs:` keys to the parameters according to the table. `get` is supplied by
/// the caller (the key-value pairs parsed from the XMP, with attributes and child
/// elements that have no children already merged); a missing key yields `None` and that
/// entry is skipped.
///
/// The return lifetime is independent of the mapping table, since the values belong to
/// the caller's key-value table. That is why `'map` is declared separately, and why the
/// closure need not widen the borrow to `'static` just for the signature.
pub fn apply<'map>(p: &mut GradeParams, get: &dyn Fn(&str) -> Option<&'map str>) {
    for spec in SPECS {
        let Some(raw) = get(spec.key) else {
            continue;
        };
        match spec.cond {
            // 绝对开尔文只对自定白平衡有意义（与 v0.3.0 的判据一字不差）。
            Cond::WbCustom if get("WhiteBalance") != Some("Custom") => continue,
            // PV2010 兜底：主键在就不动。
            Cond::Pv2010Fallback if get(pv2012_primary(spec.field)).is_some() => continue,
            _ => {}
        }
        apply_one(p, spec, raw);
    }
}

fn apply_one(p: &mut GradeParams, spec: &CrsSpec, raw: &str) {
    match spec.rule {
        // 纯条件输入不落字段。
        Rule::Skip => {}
        Rule::BoolToMono => {
            if truthy(Some(raw)) {
                p.mono = 100.0;
            }
        }
        // 两键取大：谁大写谁，都不大保持默认（与 v0.3.0 的 max + 阈值口径等价）。
        Rule::DefringeMax => {
            let v = num(raw) * spec.scale;
            let slot = field_slot(p, spec.field);
            if v > *slot {
                *slot = clip(v, spec.min, spec.max);
            }
        }
        Rule::Linear | Rule::ReadOnly => {
            let v = clip(num(raw) * spec.scale + spec.offset, spec.min, spec.max);
            *field_slot(p, spec.field) = v;
        }
    }
}

// ---------------------------------------------------------------- 导出

/// 按表反算出全部 `crs:` 参数属性（不含包元数据与曲线子元素）。
/// 顺序即表序；`xmp_extra` 的回写由调用方追加在后面。
///
/// Inverts the table into every `crs:` parameter attribute, package metadata and curve
/// sub-elements excluded. The order is the table order; the caller appends the
/// `xmp_extra` write-back afterwards.
pub fn emit_attrs(p: &GradeParams) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for spec in SPECS {
        match spec.rule {
            // 只读键与条件输入不写；它们的输出由别的键代表（增量写绝对值、
            // SplitToning 名写别名值、WhiteBalance 随 WB 组）。
            Rule::ReadOnly | Rule::Skip => {}
            Rule::BoolToMono => {
                push(
                    &mut out,
                    spec.key,
                    if p.mono > 0.0 { "True" } else { "False" },
                );
            }
            Rule::DefringeMax => {
                // 两键同源：都写当前强度（LR 侧 Purple/Green 一直是同一个值）。
                let v = clip(*field_slot_ref(p, spec.field), spec.min, spec.max);
                push(&mut out, spec.key, &fmt_num(v / spec.scale));
            }
            Rule::Linear => match spec.key {
                // WB 组：温度或色调动过才写三个键（与 v0.3.0 的判据一致），
                // WhiteBalance 在组头，Tint 随组写、自身不单独出。
                "IncrementalTemperature" => {
                    if p.temperature.abs() > 1e-6 || p.tint.abs() > 1e-6 {
                        push(&mut out, "WhiteBalance", "Custom");
                        push(
                            &mut out,
                            "IncrementalTemperature",
                            &fmt_num(clip(p.temperature, -100.0, 100.0)),
                        );
                        push(
                            &mut out,
                            "IncrementalTint",
                            &fmt_num(clip(p.tint, -100.0, 100.0)),
                        );
                    }
                }
                "IncrementalTint" | "WhiteBalance" => {}
                _ => {
                    let v = clip(*field_slot_ref(p, spec.field), spec.min, spec.max);
                    push(&mut out, spec.key, &fmt_num((v - spec.offset) / spec.scale));
                }
            },
        }
    }
    out
}

fn push(out: &mut Vec<(String, String)>, key: &str, value: &str) {
    out.push((format!("crs:{key}"), value.to_owned()));
}

/// 只读槽位（导出侧不需要可变借用）。
fn field_slot_ref(p: &GradeParams, f: CrsField) -> &f64 {
    match f {
        CrsField::Exposure => &p.exposure,
        CrsField::Contrast => &p.contrast,
        CrsField::Highlights => &p.highlights,
        CrsField::Shadows => &p.shadows,
        CrsField::Whites => &p.whites,
        CrsField::Blacks => &p.blacks,
        CrsField::Temperature => &p.temperature,
        CrsField::Tint => &p.tint,
        CrsField::Vibrance => &p.vibrance,
        CrsField::Saturation => &p.saturation,
        CrsField::Clarity => &p.clarity,
        CrsField::Texture => &p.texture,
        CrsField::Dehaze => &p.dehaze,
        CrsField::Vignette => &p.vignette,
        CrsField::VigMidpoint => &p.vig_midpoint,
        CrsField::VigRoundness => &p.vig_roundness,
        CrsField::VigFeather => &p.vig_feather,
        CrsField::VigHlContrast => &p.vig_hl_contrast,
        CrsField::SharpAmount => &p.sharp_amount,
        CrsField::SharpRadius => &p.sharp_radius,
        CrsField::SharpDetail => &p.sharp_detail,
        CrsField::SharpMasking => &p.sharp_masking,
        CrsField::NrLuma => &p.nr_luma,
        CrsField::NrColor => &p.nr_color,
        CrsField::NrLumaDetail => &p.nr_luma_detail,
        CrsField::NrLumaContrast => &p.nr_luma_contrast,
        CrsField::NrColorSmooth => &p.nr_color_smooth,
        CrsField::Grain => &p.grain,
        CrsField::HslH(i) => &p.hsl_h[i],
        CrsField::HslS(i) => &p.hsl_s[i],
        CrsField::HslL(i) => &p.hsl_l[i],
        CrsField::Mono => &p.mono,
        CrsField::MonoH(i) => &p.mono_h[i],
        CrsField::ParShadows => &p.par_shadows,
        CrsField::ParDarks => &p.par_darks,
        CrsField::ParLights => &p.par_lights,
        CrsField::ParHighlights => &p.par_highlights,
        CrsField::ParSplit1 => &p.par_split1,
        CrsField::ParSplit2 => &p.par_split2,
        CrsField::ParSplit3 => &p.par_split3,
        CrsField::GradeShH => &p.grade_sh_h,
        CrsField::GradeShS => &p.grade_sh_s,
        CrsField::GradeShL => &p.grade_sh_l,
        CrsField::GradeMidH => &p.grade_mid_h,
        CrsField::GradeMidS => &p.grade_mid_s,
        CrsField::GradeMidL => &p.grade_mid_l,
        CrsField::GradeHiH => &p.grade_hi_h,
        CrsField::GradeHiS => &p.grade_hi_s,
        CrsField::GradeHiL => &p.grade_hi_l,
        CrsField::GradeGlobalH => &p.grade_global_h,
        CrsField::GradeGlobalS => &p.grade_global_s,
        CrsField::GradeGlobalL => &p.grade_global_l,
        CrsField::GradeBalance => &p.grade_balance,
        CrsField::GradeBlending => &p.grade_blending,
        CrsField::LensFringe => &p.lens_fringe,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_special_rules_consistent() {
        let mut seen = std::collections::HashSet::new();
        for s in SPECS {
            assert!(seen.insert(s.key), "重复的 crs 键：{}", s.key);
            assert!(!s.note.is_empty(), "{} 缺 note", s.key);
        }
    }

    /// 只读键绝不写出：PV2010 兜底、绝对开尔文、ColorGrade 别名。
    #[test]
    fn read_only_keys_never_emitted() {
        let mut p = GradeParams::default();
        p.exposure = 1.0;
        p.contrast = 5.0;
        p.temperature = 10.0;
        p.tint = 5.0;
        p.grade_sh_h = 200.0;
        p.grade_sh_s = 20.0;
        p.grade_hi_h = 40.0;
        p.grade_hi_s = 10.0;
        let attrs = emit_attrs(&p);
        let keys: Vec<&str> = attrs.iter().map(|(k, _)| k.as_str()).collect();
        for bad in [
            "crs:Exposure",
            "crs:Contrast",
            "crs:Temperature",
            "crs:Tint",
            "crs:ColorGradeShadowHue",
            "crs:ColorGradeShadowSat",
            "crs:ColorGradeHighlightHue",
            "crs:ColorGradeHighlightSat",
        ] {
            assert!(!keys.contains(&bad), "只读键 {bad} 不该被导出");
        }
        // 增量键在（温度/色调非零时）应随 WB 组写出。
        assert!(keys.contains(&"crs:WhiteBalance"));
        assert!(keys.contains(&"crs:IncrementalTemperature"));
        assert!(keys.contains(&"crs:IncrementalTint"));
    }

    /// 暗角方向相反：正 vignette（压暗）导出为负 PostCropVignetteAmount。
    #[test]
    fn vignette_negated_on_export() {
        let mut p = GradeParams::default();
        p.vignette = 30.0;
        let attrs = emit_attrs(&p);
        let amount = attrs
            .iter()
            .find(|(k, _)| k == "crs:PostCropVignetteAmount")
            .map(|(_, v)| num(v))
            .unwrap();
        assert!((amount + 30.0).abs() < 1e-9);
    }

    /// 去边两键同源同值，20 色阶 = 100%。
    #[test]
    fn defringe_scales_symmetrically() {
        let mut p = GradeParams::default();
        p.lens_fringe = 50.0;
        let attrs = emit_attrs(&p);
        for key in ["crs:DefringePurpleAmount", "crs:DefringeGreenAmount"] {
            let v = attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| num(v))
                .unwrap();
            assert!((v - 10.0).abs() < 1e-9, "{key} = {v}");
        }
    }

    /// WbCustom 条件：WhiteBalance 不是 Custom 时绝对开尔文不生效。
    #[test]
    fn absolute_kelvin_requires_custom_wb() {
        let mut p = GradeParams::default();
        let get = |k: &str| match k {
            "Temperature" => Some("6400"),
            "WhiteBalance" => Some("As Shot"),
            _ => None,
        };
        apply(&mut p, &get);
        assert_eq!(p.temperature, 0.0);
        let get = |k: &str| match k {
            "Temperature" => Some("6400"),
            "WhiteBalance" => Some("Custom"),
            _ => None,
        };
        apply(&mut p, &get);
        assert!((p.temperature - (6400.0 - 5500.0) / 45.0).abs() < 1e-9);
    }
}
