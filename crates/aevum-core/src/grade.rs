//! 调色的参数集与纯函数：曲线表、暗角场。
//!
//! 参数名与 Python 版的 `grade.DEFAULT` 一一对应，预设也照抄同一张表，
//! 逐字段由 `tests/golden.rs` 比对，防止搬运时打错一个数字。
//!
//! Grading parameter set and pure functions: curve table, vignette field.
//!
//! Parameter names correspond one-to-one to `grade.DEFAULT` in the Python version, and presets
//! copy the same table; `tests/golden.rs` compares every field, catching a mistyped number
//! during the port.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn zero8() -> [f64; 8] {
    [0.0; 8]
}

fn ident_curve() -> Vec<[f64; 2]> {
    vec![[0.0, 0.0], [255.0, 255.0]]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradeParams {
    pub exposure: f64,
    /// 曝光高光肩部（0–100）：LR rolloff 口径的有理肩部，0 = 旧线性增益逐位一致。
    ///
    /// Exposure highlight shoulder (0–100): a rational shoulder under the LR rolloff convention;
    /// 0 reproduces the old linear gain bit for bit.
    pub hl_rolloff: f64,
    pub contrast: f64,
    /// 对比度的线性光域混合系数（0–100）：0 = 旧 gamma 域 smoothstep 逐位一致。
    ///
    /// Linear-light blend factor for contrast (0–100); 0 reproduces the old gamma-domain
    /// smoothstep bit for bit.
    pub contrast_linear: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    /// 黑/白场软拐点（0–100）：h/s/w/b 段的 [0,4] 硬钳换成抛物线软拐点，
    /// 输出恒落 [0,1]。0 = 旧硬钳逐位一致。
    ///
    /// Soft knee for the black and white points (0–100): the hard [0,4] clamp of the h/s/w/b
    /// stage becomes a parabolic knee, and the output always lands in [0,1]. 0 reproduces the
    /// old hard clamp bit for bit.
    pub black_soft: f64,
    pub white_soft: f64,
    pub temperature: f64,
    pub tint: f64,
    pub vibrance: f64,
    pub saturation: f64,
    pub clarity: f64,
    pub texture: f64,
    pub dehaze: f64,
    /// 暗通道去雾量（0–100，He DCP + 大气散射）：0 = 旧逐点路径逐位一致；
    /// >0 在旧口径之后叠加空间感知的 DCP 去雾。见 `develop/dehaze.rs`。
    ///
    /// Dark-channel dehaze amount (0–100, He DCP plus atmospheric scattering).
    /// 0 reproduces the old per-pixel path bit for bit; >0 layers spatially aware DCP dehaze on
    /// top of the old convention.
    /// See `develop/dehaze.rs`.
    pub dehaze_dark: f64,
    /// 去雾对比（-50–50）：DCP 透射修正后的中间对比/黑位微调，仅在
    /// `dehaze_dark>0` 时生效。幅度 = contrast·0.15，TODO(fit)。
    ///
    /// Dehaze contrast (-50–50): a midtone contrast and black point trim applied after the DCP
    /// transmission correction, active only when `dehaze_dark>0`.
    /// Amplitude is contrast·0.15, TODO(fit).
    pub dehaze_contrast: f64,
    pub grain: f64,
    pub grain_size: f64,
    pub grain_freq: f64,
    /// 颗粒亮度调制（-100–100）：暗部增强、高光减弱，负值反转。0 = 恒定幅度逐位一致。
    /// 系数是设计选择不是拟合结果——没有参考图可对。端点是：拉满时暗部 ×2.5、高光 ×0.3
    /// （0.5 档为 ×1.75 / ×0.65）。跨实现一致性由金标 `grain_stats_lum` 保证
    /// （Python `grade.py` 同一套式子，只比统计量，容差 ±25%）。
    ///
    /// Grain luminance modulation (-100–100): stronger in the shadows, weaker in the highlights,
    /// inverted for negative values. 0 reproduces constant amplitude bit for bit. The
    /// coefficients are a design choice, not a fitted result — there is no reference image to
    /// match. The endpoints are ×2.5 in the shadows and ×0.3 in the highlights at full strength
    /// (×1.75 / ×0.65 at the half detent). Cross-implementation agreement is guarded by the
    /// `grain_stats_lum` golden case (same formula in Python `grade.py`, statistics only, ±25%).
    pub grain_lum: f64,

    pub vignette: f64,
    pub vig_midpoint: f64,
    pub vig_roundness: f64,
    pub vig_feather: f64,
    pub vig_hl_contrast: f64,

    pub sharp_amount: f64,
    pub sharp_radius: f64,
    pub sharp_detail: f64,
    pub sharp_masking: f64,
    /// 锐化的恒开边缘保护（0–100）：噪声底之上的平坦区不锐化。
    /// 0 = 旧路径（只有 masking 生效）逐位一致。对齐 LR/ACR Sharpen3。
    /// 强度已实测（`tests/fit_sharpen_protect.rs`）：全量锐化下无保护把噪声放大
    /// 1.944×，20 压到 1.773、100 压到 1.093，边缘增益全程不动（1.359）——
    /// 即 20 大致等于 `sharp_masking` 70 的强度，但只吃平坦区。
    ///
    /// Always-on edge protection for sharpening (0–100): flat regions above the noise floor are
    /// left unsharpened. 0 reproduces the old path, where only masking applied, bit for bit.
    /// Aligned with LR/ACR Sharpen3. Strength has been measured (`tests/fit_sharpen_protect.rs`):
    /// at full sharpening the unprotected path amplifies noise 1.944×; 20 brings it to 1.773 and
    /// 100 to 1.093, while the edge gain never moves (1.359) — so 20 is roughly as strong as
    /// `sharp_masking` 70, but it only touches flat regions.
    pub sharp_protect: f64,
    pub nr_luma: f64,
    pub nr_color: f64,
    pub nr_luma_detail: f64,
    pub nr_luma_contrast: f64,
    pub nr_color_smooth: f64,
    /// 多尺度降噪（0–100）：0 = 旧单尺度逐位一致；值 = 粗尺度层混合强度。
    /// 见 `develop/spatial.rs::denoise_apply` 的多尺度分支。
    ///
    /// Multiscale denoise (0–100): 0 reproduces the old single-scale path bit for bit; the value
    /// is the blend strength of the coarse scale layer.
    /// See the multiscale branch of `develop/spatial.rs::denoise_apply`.
    pub nr_multiscale: f64,
    /// 手动噪声 σ（0–100）：0 = 自动 MAD 估计（Donoho 口径）；>0 手动覆盖
    /// （映射 σ = 0.002 + (v/100)^1.6·0.030，TODO(fit)）。
    ///
    /// Manual noise σ (0–100): 0 selects the automatic MAD estimate under the Donoho convention;
    /// >0 overrides it manually (mapping σ = 0.002 + (v/100)^1.6·0.030, TODO(fit)).
    pub nr_sigma_manual: f64,
    /// AI 亮度降噪强度（0–100），0 = 关。
    ///
    /// AI luminance denoise strength (0–100); 0 disables it.
    pub nr_ai: f64,

    pub grade_global_h: f64,
    pub grade_global_s: f64,
    pub grade_global_l: f64,
    pub grade_sh_h: f64,
    pub grade_sh_s: f64,
    pub grade_sh_l: f64,
    pub grade_mid_h: f64,
    pub grade_mid_s: f64,
    pub grade_mid_l: f64,
    pub grade_hi_h: f64,
    pub grade_hi_s: f64,
    pub grade_hi_l: f64,
    pub grade_balance: f64,
    pub grade_blending: f64,

    pub lv_black: f64,
    pub lv_white: f64,
    pub lv_gamma: f64,
    pub lv_out_black: f64,
    pub lv_out_white: f64,

    pub par_shadows: f64,
    pub par_darks: f64,
    pub par_lights: f64,
    pub par_highlights: f64,
    pub par_split1: f64,
    pub par_split2: f64,
    pub par_split3: f64,

    pub mono: f64,
    pub mono_h: [f64; 8],
    pub lens_distort: f64,
    pub lens_vignette: f64,
    pub lens_fringe: f64,
    pub hsl_h: [f64; 8],
    pub hsl_s: [f64; 8],
    pub hsl_l: [f64; 8],
    pub masks: Vec<serde_json::Value>,

    pub curve_rgb: Vec<[f64; 2]>,
    pub curve_r: Vec<[f64; 2]>,
    pub curve_g: Vec<[f64; 2]>,
    pub curve_b: Vec<[f64; 2]>,
    /// 曲线 HDR 尾部（0–100）：x>1 的输入按 PCHIP 末端斜率延伸而非硬截断，
    /// 输出仍 ≤1。0 = 旧 clamp(0,1) 逐位一致。
    ///
    /// **当前在整条链里无效**：`spatial::lens` 在渲染起点无条件 `clamp01`
    /// （Python 侧 `_lens` 同样 `np.clip(x, 0, 1)`），进影调链的值永远 ≤1，
    /// 这条路径走不到。修它要改「镜头之后向上不封顶」的契约并重冻结金标，
    /// 实测见 `tests/fit_sliders.rs` 的 `curve_hdr_tail_is_inert_behind_the_lens_clamp`。
    ///
    /// Curve HDR tail (0–100): inputs with x>1 extend along the PCHIP end slope instead of being
    /// hard clipped, and the output stays ≤1. 0 reproduces the old clamp(0,1) bit for bit.
    ///
    /// **It currently does nothing in the full chain**: `spatial::lens` clamps to [0,1]
    /// unconditionally at the start of every render (Python's `_lens` does the same
    /// `np.clip(x, 0, 1)`), so nothing above 1 ever reaches the tone curve. Fixing that means
    /// changing the "no upper clamp after lens" contract and refreezing the golden data; see
    /// `curve_hdr_tail_is_inert_behind_the_lens_clamp` in `tests/fit_sliders.rs`.
    pub curve_hdr_tail: f64,

    /// 外部 .cube「外观」：主链之前先套一层三维查表，空串表示没有。
    /// Python oracle 没有这一步，金标 case 里这一项恒为空，不影响比对。
    ///
    /// External .cube look: a 3D lookup table is applied before the main chain, and an empty
    /// string means none.
    /// The Python oracle has no such step, so this field stays empty in golden cases and does
    /// not affect comparison.
    pub look_cube: String,
    pub dcp_profile: String,

    /// 机型镜头配置文件校正（畸变 / 横向色差 / 暗角），显影链最前面套。
    /// 由 EXIF 匹配 lensfun 数据库得出，见 [`crate::lensdb`]；金标 case
    /// 恒为 None，不影响逐像素比对。
    ///
    /// Camera and lens profile correction (distortion / lateral chromatic aberration /
    /// vignetting), applied at the head of the develop chain.
    /// It comes from matching EXIF against the lensfun database; see [`crate::lensdb`].
    /// Golden cases always hold None, so per-pixel comparison is unaffected.
    pub lens_profile: Option<crate::lensdb::Correction>,
    pub lcp_profile: Option<crate::lcp::Correction>,

    /// XMP `crs:` 命名空间里映射表没覆盖的键，原样字符串保留：导入时收进来、
    /// 随参数走历史与工程落盘、导出时回写。跨命名空间（dc:、photoshop: 等）不收，
    /// 包元数据（Version/PresetType 等，见 `xmp_map::RESERVED_KEYS`）也不收。
    /// 旧工程 JSON 里没有这个字段——容器级 `#[serde(default)]` 兜底，反序列化为空表。
    ///
    /// Keys in the XMP `crs:` namespace that the mapping table does not cover, kept as raw
    /// strings: collected on import, carried through history and project file persistence, and
    /// written back on export.
    /// Keys from other namespaces (dc:, photoshop: and so on) are not collected, nor is package
    /// metadata (Version/PresetType and so on; see `xmp_map::RESERVED_KEYS`).
    /// Old project JSON lacks this field, and the container-level `#[serde(default)]` covers it,
    /// so deserialization yields an empty map.
    pub xmp_extra: BTreeMap<String, String>,
}

impl Default for GradeParams {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            hl_rolloff: 0.0,
            contrast: 0.0,
            contrast_linear: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            black_soft: 0.0,
            white_soft: 0.0,
            temperature: 0.0,
            tint: 0.0,
            vibrance: 0.0,
            saturation: 0.0,
            clarity: 0.0,
            texture: 0.0,
            dehaze: 0.0,
            dehaze_dark: 0.0,
            dehaze_contrast: 0.0,
            grain: 0.0,
            grain_size: 1.0,
            grain_freq: 1.0,
            grain_lum: 0.0,
            vignette: 0.0,
            vig_midpoint: 50.0,
            vig_roundness: 0.0,
            vig_feather: 50.0,
            vig_hl_contrast: 0.0,
            sharp_amount: 0.0,
            sharp_radius: 1.0,
            sharp_detail: 25.0,
            sharp_masking: 0.0,
            sharp_protect: 0.0,
            nr_luma: 0.0,
            nr_color: 0.0,
            nr_luma_detail: 0.0,
            nr_luma_contrast: 0.0,
            nr_color_smooth: 50.0,
            nr_multiscale: 0.0,
            nr_sigma_manual: 0.0,
            nr_ai: 0.0,
            grade_global_h: 0.0,
            grade_global_s: 0.0,
            grade_global_l: 0.0,
            grade_sh_h: 0.0,
            grade_sh_s: 0.0,
            grade_sh_l: 0.0,
            grade_mid_h: 0.0,
            grade_mid_s: 0.0,
            grade_mid_l: 0.0,
            grade_hi_h: 0.0,
            grade_hi_s: 0.0,
            grade_hi_l: 0.0,
            grade_balance: 0.0,
            grade_blending: 0.0,
            lv_black: 0.0,
            lv_white: 255.0,
            lv_gamma: 1.0,
            lv_out_black: 0.0,
            lv_out_white: 255.0,
            par_shadows: 0.0,
            par_darks: 0.0,
            par_lights: 0.0,
            par_highlights: 0.0,
            par_split1: 25.0,
            par_split2: 50.0,
            par_split3: 75.0,
            mono: 0.0,
            mono_h: zero8(),
            lens_distort: 0.0,
            lens_vignette: 0.0,
            lens_fringe: 0.0,
            hsl_h: zero8(),
            hsl_s: zero8(),
            hsl_l: zero8(),
            masks: Vec::new(),
            curve_rgb: ident_curve(),
            curve_r: ident_curve(),
            curve_g: ident_curve(),
            curve_b: ident_curve(),
            curve_hdr_tail: 0.0,
            look_cube: String::new(),
            dcp_profile: String::new(),
            lens_profile: None,
            lcp_profile: None,
            xmp_extra: BTreeMap::new(),
        }
    }
}

pub const PRESET_NAMES: &[&str] = &[
    "原始",
    "鲜艳风光",
    "落日暖调",
    "冷色电影",
    "复古胶片",
    "黑白对比",
    "夜景增强",
    "通透风光",
    "柔和人像",
    "青橙电影",
];

pub fn preset(name: &str) -> GradeParams {
    let mut p = GradeParams::default();
    match name {
        "原始" => {}
        "鲜艳风光" => {
            p.vibrance = 35.0;
            p.saturation = 10.0;
            p.clarity = 15.0;
            p.contrast = 12.0;
            p.texture = 8.0;
            p.dehaze = 12.0;
            p.sharp_amount = 40.0;
            p.sharp_masking = 25.0;
        }
        "落日暖调" => {
            p.temperature = 28.0;
            p.tint = 6.0;
            p.vibrance = 22.0;
            p.highlights = -20.0;
            p.shadows = 10.0;
            p.clarity = 10.0;
            p.grade_hi_h = 40.0;
            p.grade_hi_s = 18.0;
        }
        "冷色电影" => {
            p.temperature = -22.0;
            p.tint = -6.0;
            p.contrast = 16.0;
            p.saturation = -8.0;
            p.shadows = 12.0;
            p.grade_sh_h = 215.0;
            p.grade_sh_s = 28.0;
            p.grade_hi_h = 35.0;
            p.grade_hi_s = 12.0;
            p.curve_rgb = vec![[0.0, 18.0], [64.0, 58.0], [192.0, 206.0], [255.0, 254.0]];
        }
        "复古胶片" => {
            // 用户实测反馈「两个桃色预设重复了」：这里原来是 temp+12 加阴影
            // 偏橙，和「落日暖调」一水儿的暖，肉眼难分。现在往褪色冷调走：
            // 色温回中性、阴影压青绿、黑位收紧——一暖一冷两条路线。
            p.temperature = 4.0;
            p.saturation = -24.0;
            p.contrast = -8.0;
            p.blacks = -8.0;
            p.vignette = 30.0;
            p.grain = 18.0;
            p.grade_sh_h = 150.0;
            p.grade_sh_s = 22.0;
            p.grade_hi_h = 200.0;
            p.grade_hi_s = 10.0;
            p.curve_rgb = vec![[0.0, 24.0], [64.0, 70.0], [192.0, 215.0], [255.0, 248.0]];
        }
        "黑白对比" => {
            // 黑白必须走 mono（色相带混合）：只压 saturation 的话，单色混合
            // 面板的 8 条推子全程无效——灰度在进面板前就定死了。
            p.mono = 100.0;
            p.contrast = 28.0;
            p.clarity = 22.0;
            p.blacks = -15.0;
            p.sharp_amount = 35.0;
            p.sharp_masking = 40.0;
        }
        "夜景增强" => {
            p.exposure = 0.45;
            p.shadows = 32.0;
            p.highlights = -25.0;
            p.vibrance = 18.0;
            p.temperature = -10.0;
            p.clarity = 8.0;
            p.nr_luma = 30.0;
            p.nr_color = 45.0;
            p.sharp_masking = 50.0;
        }
        "通透风光" => {
            p.contrast = 10.0;
            p.highlights = -22.0;
            p.shadows = 14.0;
            p.vibrance = 28.0;
            p.dehaze = 18.0;
            p.clarity = 18.0;
            p.texture = 10.0;
            p.sharp_amount = 45.0;
            p.sharp_masking = 20.0;
        }
        "柔和人像" => {
            p.highlights = -16.0;
            p.shadows = 20.0;
            p.vibrance = 12.0;
            p.saturation = -4.0;
            p.texture = -25.0;
            p.clarity = -10.0;
            p.nr_luma = 25.0;
            p.nr_color = 30.0;
            p.sharp_amount = 30.0;
            p.sharp_masking = 60.0;
        }
        "青橙电影" => {
            p.temperature = -8.0;
            p.contrast = 14.0;
            p.shadows = 8.0;
            p.saturation = -5.0;
            p.clarity = 10.0;
            p.grade_sh_h = 210.0;
            p.grade_sh_s = 35.0;
            p.grade_hi_h = 35.0;
            p.grade_hi_s = 28.0;
        }
        _ => {}
    }
    p
}

/// 单调三次插值（Fritsch–Carlson）：控制点 → n 级 0..255 查找表。
///
/// Monotone cubic interpolation (Fritsch–Carlson): control points to an n-step 0..255 lookup
/// table.
pub fn pchip_lut(points: &[[f64; 2]], n: usize) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    let mut pts: Vec<[f64; 2]> = points.to_vec();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let xs: Vec<f64> = pts.iter().map(|p| p[0]).collect();
    let ys: Vec<f64> = pts.iter().map(|p| p[1]).collect();
    if xs.len() < 2 {
        return linspace_f32(0.0, 255.0, n);
    }
    let m = pchip_slopes(&xs, &ys);

    (0..n)
        .map(|k| {
            let g = if n == 1 {
                0.0
            } else {
                k as f64 * 255.0 / (n - 1) as f64
            };
            let mut i = xs.partition_point(|x| *x <= g) as isize - 1;
            i = i.clamp(0, xs.len() as isize - 2);
            let i = i as usize;
            let h = (xs[i + 1] - xs[i]).max(1e-9);
            let t = ((g - xs[i]) / h).clamp(0.0, 1.0);
            let (t2, t3) = (t * t, t * t * t);
            let v = (2.0 * t3 - 3.0 * t2 + 1.0) * ys[i]
                + (t3 - 2.0 * t2 + t) * h * m[i]
                + (-2.0 * t3 + 3.0 * t2) * ys[i + 1]
                + (t3 - t2) * h * m[i + 1];
            v.clamp(0.0, 255.0) as f32
        })
        .collect()
}

/// PCHIP 末端斜率（0..255 空间）：Fritsch–Carlson 的 `m[n-1] = 最后一段割线`，
/// 与 [`pchip_lut`]/`pchip_slopes` 的端点处理一致。曲线 HDR 尾部（B3）用它
/// 保证延伸段与查表末端 C1 连续。
///
/// PCHIP end slope in 0..255 space: `m[n-1] = last segment secant` per Fritsch–Carlson,
/// consistent with the endpoint handling of [`pchip_lut`] and `pchip_slopes`.
/// The curve HDR tail (B3) uses it to keep the extended segment C1-continuous with the end of the
/// lookup table.
pub fn pchip_end_slope(points: &[[f64; 2]]) -> f64 {
    let mut pts: Vec<[f64; 2]> = points.to_vec();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let xs: Vec<f64> = pts.iter().map(|p| p[0]).collect();
    let ys: Vec<f64> = pts.iter().map(|p| p[1]).collect();
    if xs.len() < 2 {
        return 0.0;
    }
    (ys[ys.len() - 1] - ys[ys.len() - 2]) / (xs[xs.len() - 1] - xs[xs.len() - 2]).max(1e-9)
}

fn pchip_slopes(xs: &[f64], ys: &[f64]) -> Vec<f64> {
    let n = xs.len();
    let h: Vec<f64> = (0..n - 1).map(|i| xs[i + 1] - xs[i]).collect();
    let d: Vec<f64> = (0..n - 1)
        .map(|i| (ys[i + 1] - ys[i]) / h[i].max(1e-9))
        .collect();
    let mut m = vec![0.0; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        if d[i - 1] * d[i] <= 0.0 {
            m[i] = 0.0;
        } else {
            let w1 = 2.0 * h[i] + h[i - 1];
            let w2 = h[i] + 2.0 * h[i - 1];
            m[i] = (w1 + w2) / (w1 / d[i - 1] + w2 / d[i]);
        }
    }
    m
}

fn linspace_f32(a: f64, b: f64, n: usize) -> Vec<f32> {
    (0..n)
        .map(|k| {
            if n == 1 {
                a as f32
            } else {
                (a + (b - a) * k as f64 / (n - 1) as f64) as f32
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VignetteParams {
    pub amount: f64,
    pub midpoint: f64,
    pub roundness: f64,
    pub feather: f64,
    pub hl_contrast: f64,
}

impl From<&GradeParams> for VignetteParams {
    fn from(p: &GradeParams) -> Self {
        Self {
            amount: p.vignette,
            midpoint: p.vig_midpoint,
            roundness: p.vig_roundness,
            feather: p.vig_feather,
            hl_contrast: p.vig_hl_contrast,
        }
    }
}

/// 裁剪后暗角的乘法蒙版。正 amount 压暗四角，负 amount 提亮。
///
/// `lum` 是**当前像素**的亮度场（Python 在晕影这一步现算，不是原图亮度）：
/// 只有开了高光保护 `hl_contrast` 才需要，其余调用传 `None`。
///
/// Multiplicative mask for the post-crop vignette.
/// A positive amount darkens the corners, and a negative amount brightens them.
///
/// `lum` is the luminance field of the **current pixel**, computed by Python at the vignette
/// step rather than taken from the source image: it is needed only when highlight protection
/// `hl_contrast` is on, and other calls pass `None`.
pub fn vignette_mask(
    w: usize,
    h: usize,
    p: &VignetteParams,
    lum: Option<&[f32]>,
) -> Option<Vec<f32>> {
    let a = p.amount / 100.0;
    if a == 0.0 || w == 0 || h == 0 {
        return None;
    }
    let mp = (p.midpoint.clamp(0.0, 100.0)) / 100.0;
    let fe = (p.feather.clamp(0.0, 100.0)) / 100.0;
    let hc = (p.hl_contrast.clamp(-100.0, 100.0)) / 100.0;
    let hl_on = hc != 0.0 && lum.is_some_and(|l| l.len() == w * h);

    let xs: Vec<f32> = (0..w)
        .map(|x| (x as f32 - (w as f32 - 1.0) / 2.0) / (w as f32 / 2.0))
        .collect();
    let ys: Vec<f32> = (0..h)
        .map(|y| (y as f32 - (h as f32 - 1.0) / 2.0) / (h as f32 / 2.0))
        .collect();
    let half = ((w as f32 / 2.0).hypot(h as f32 / 2.0)).max(1e-6);
    let rd = (p.roundness.clamp(0.0, 100.0) / 100.0) as f32;

    let start = (mp * 0.9) as f32;
    let width = ((fe * 1.2) as f32).max(0.05);
    let mut out = vec![0.0f32; w * h];
    for (y, sy) in ys.iter().enumerate() {
        for (x, sx) in xs.iter().enumerate() {
            // 画幅椭圆：两轴各自归一化后的半径 / sqrt(2)
            let r_ell = ((sx * sx + sy * sy) / 2.0).sqrt();
            let r_circ = {
                let dx = sx * (w as f32 / 2.0);
                let dy = sy * (h as f32 / 2.0);
                (dx * dx + dy * dy).sqrt() / half
            };
            let r = r_ell * (1.0 - rd) + r_circ * rd;
            let t = ((r - start) / width).clamp(0.0, 1.0);
            let s = t * t * (3.0 - 2.0 * t);
            let mut m = 1.0 - a as f32 * s;
            // 高光保护：按像素亮度把亮部被压掉的量还回去（亮于 0.5 才起作用）。
            if hl_on {
                let bright = (lum.unwrap()[y * w + x] * 2.0 - 1.0).clamp(0.0, 1.0);
                m += hc as f32 * bright * (1.0 - m);
            }
            out[y * w + x] = m.clamp(0.0, 2.0);
        }
    }
    Some(out)
}
