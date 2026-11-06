//! DCP 相机配置：读 Adobe DNG Camera Profile（`.dcp`），把相机的原始 RGB 经
//! 「白平衡 → 颜色矩阵 → 色调曲线 → sRGB」变成显示参考的像素。
//!
//! DCP camera profiles: reads an Adobe DNG Camera Profile (`.dcp`) and turns
//! raw camera RGB into display-referred pixels via
//! white balance → color matrix → tone curve → sRGB.
//!
//! 为什么不是一条普通滤镜：DCP 的矩阵定义在**相机线性 RGB** 上（白平衡之后），
//! 套在已经 gamma 编码的 JPEG/TIFF 上只会把颜色搅坏。所以 [`decode`] 自己接管
//! 解码：只有 DNG / 厂商 RAW 才接，机身内嵌预览与普通图片直接报错。
//!
//! Why this is not an ordinary filter: the DCP matrices are defined on
//! **camera linear RGB** (after white balance); applying them to already
//! gamma-encoded JPEG/TIFF data only scrambles colors. So [`decode`] takes
//! over decoding itself: only DNG / vendor RAW is accepted, while embedded
//! previews and ordinary images are rejected outright.
//!
//! 本版套用的字段：
//!
//! Fields applied by this version:
//!
//! - `UniqueCameraModel`：与 EXIF 机型比对，不一致就拒绝——配置拿错比不套更糟；
//! - `ColorMatrix1/2`、`ForwardMatrix1/2`：双光源配置按 `AsShotNeutral` 在两组
//!   标定白点之间插值（相机空间色度上的投影），单光源直接用那一组；
//! - `ProfileHueSatMap`：在 XYZ(D50) 之后、曝光之前，按 DNG 1.6 第 6 章在
//!   ProPhoto（RIMM）线性 RGB 的 HSV 空间做三线性插值；双光源时两张表按同一
//!   白平衡权重插值；
//! - `BaselineExposureOffset`：EV 偏移；
//! - `ProfileLookTable`：曝光之后、曲线之前的同格式外观表；
//! - `ProfileToneCurve`：线性光上的分段线性曲线。本版套在线性 sRGB 域；规范
//!   把它放在 ProPhoto 流水线里，逐通道曲线在两种线性域只差饱和色的通道比例，
//!   这里沿用既有口径以保持已导出的色彩不变。
//!
//! - `UniqueCameraModel`: compared against the EXIF model, and a mismatch is
//!   rejected — a wrong profile is worse than no profile;
//! - `ColorMatrix1/2`, `ForwardMatrix1/2`: dual-illuminant profiles
//!   interpolate between the two calibrated white points by `AsShotNeutral`
//!   (a projection in camera-space chromaticity); single-illuminant profiles
//!   use that one set directly;
//! - `ProfileHueSatMap`: after XYZ(D50) and before exposure, trilinear
//!   interpolation in the HSV space of ProPhoto (RIMM) linear RGB per
//!   chapter 6 of DNG 1.6; with two illuminants both tables are interpolated
//!   with the same white-balance weights;
//! - `BaselineExposureOffset`: an EV offset;
//! - `ProfileLookTable`: a look table of the same format, after exposure and
//!   before the curve;
//! - `ProfileToneCurve`: a piecewise-linear curve on linear light. This
//!   version applies it in the linear sRGB domain; the spec places it in the
//!   ProPhoto pipeline, where per-channel curves differ between the two
//!   linear domains only in channel ratios for saturated colors, so the
//!   existing convention is kept to keep already-exported colors unchanged.
//!
//! HSV 表的索引口径来自规范原文（第 6 章「Applying the Hue/Saturation/Value
//! Mapping Table」与 `ProfileHueSatMapEncoding`/`ProfileLookTableEncoding`）：
//! 色相按 360°/色相分段数环绕采样（规范例子：分 3 段即 0°/120°/240°），
//! 饱和度与明度在 [0,1] 上按 `下标 × (分段数 − 1)` 取端点；编码为 sRGB 时
//! 明度先编码再进表、出表后再解码。负的线性分量（矩阵串色）先夹到 0 再做
//! HSV——HSV 对负值没有定义。
//!
//! The HSV table indexing convention follows the spec text (chapter 6
//! "Applying the Hue/Saturation/Value Mapping Table" and
//! `ProfileHueSatMapEncoding`/`ProfileLookTableEncoding`): hue is sampled
//! circularly at 360° / hue segments (spec example: 3 segments means
//! 0°/120°/240°), while saturation and value take endpoints in [0,1] as
//! `index × (segments − 1)`. When the encoding is sRGB, value is encoded
//! before entering the table and decoded after leaving it. Negative linear
//! components (matrix cross-talk) are clamped to 0 before the HSV step —
//! HSV is undefined for negative values.
//!
//! 白平衡：DNG 的 `AsShotNeutral` 在这里按浮点乘进像素（DNG 解码器本身不做白
//! 平衡）；厂商 RAW 的解码路径已经在传感器空间做过同样的增益，不能再做第二次，
//! 因此 RAW 只套矩阵。缺 `AsShotNeutral` 的 DNG 直接报错：没有白平衡的相机
//! RGB 是绿的，套出来的颜色一定错。
//!
//! White balance: DNG's `AsShotNeutral` is multiplied into the pixels as
//! floats here (the DNG decoder itself does no white balance); the vendor RAW
//! decode path has already applied the same gains in sensor space and must
//! not apply them twice, so RAW only gets the matrices. A DNG missing
//! `AsShotNeutral` is rejected outright: camera RGB without white balance is
//! green, and the resulting colors would be wrong.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde_json::Value;

/// 一份相机配置里用得上的字段。
///
/// The fields of a camera profile that are actually used.
#[derive(Clone, Debug, Default)]
pub struct Profile {
    /// 文件路径（面板显示用）。
    ///
    /// File path (shown in the panel).
    pub path: String,
    /// `ProfileName`（例如 Adobe Standard）。
    ///
    /// `ProfileName` (for example Adobe Standard).
    pub name: String,
    /// `UniqueCameraModel`。
    ///
    /// `UniqueCameraModel`.
    pub camera: String,
    /// ColorMatrix1/2：未白平衡的相机 RGB → XYZ(D50)。
    color: [Option<Matrix>; 2],
    /// ForwardMatrix1/2：**已白平衡**的相机 RGB → XYZ(D50)。
    forward: [Option<Matrix>; 2],
    /// CalibrationIlluminant1/2（EXIF 光源码）。
    illuminant: [Option<u32>; 2],
    /// ProfileHueSatMapData1/2：XYZ(D50) 之后的 HSV 查找表。
    hue_sat: [Option<HsvTable>; 2],
    /// ProfileLookTableData：曝光之后、曲线之前的 HSV 查找表。
    look: Option<HsvTable>,
    tone: Vec<(f64, f64)>,
    baseline_ev: f64,
}

type Matrix = [[f64; 3]; 3];

/// 一张 HSV 表：每个格点给出（色相偏移角度、饱和度系数、明度系数）。
#[derive(Clone, Debug)]
struct HsvTable {
    hue: usize,
    sat: usize,
    val: usize,
    /// 索引前把明度按 sRGB 曲线编码、出表后解码（`*Encoding` = 1）。
    encoded: bool,
    /// 行序：明度最外、色相其次、饱和最内，与规范一致。
    data: Vec<[f64; 3]>,
}

impl HsvTable {
    fn cells(&self, v: usize, h: usize, s: usize) -> [f64; 3] {
        self.data[(v * self.hue + h) * self.sat + s]
    }

    /// 三线性插值；分度数为 1 的维度只取第 0 层。
    fn lookup(&self, h: f64, s: f64, v: f64) -> [f64; 3] {
        let hd = self.hue as f64;
        let hp = (h / 360.0 * hd).clamp(0.0, hd);
        let h0 = (hp.floor() as usize).min(self.hue - 1);
        let h1 = if self.hue > 1 { (h0 + 1) % self.hue } else { 0 };
        let th = if h1 == h0 { 0.0 } else { hp - h0 as f64 };

        let sp = s.clamp(0.0, 1.0) * (self.sat - 1) as f64;
        let s0 = (sp.floor() as usize).min(self.sat - 1);
        let s1 = (s0 + 1).min(self.sat - 1);
        let ts = if s1 == s0 { 0.0 } else { sp - s0 as f64 };

        let vp = if self.val > 1 {
            v.clamp(0.0, 1.0) * (self.val - 1) as f64
        } else {
            0.0
        };
        let v0 = (vp.floor() as usize).min(self.val - 1);
        let v1 = (v0 + 1).min(self.val - 1);
        let tv = if v1 == v0 { 0.0 } else { vp - v0 as f64 };

        let mut out = [0.0; 3];
        for (vi, wv) in [(v0, 1.0 - tv), (v1, tv)] {
            if wv == 0.0 {
                continue;
            }
            for (hi, wh) in [(h0, 1.0 - th), (h1, th)] {
                let wh = wh * wv;
                if wh == 0.0 {
                    continue;
                }
                for (si, ws) in [(s0, 1.0 - ts), (s1, ts)] {
                    let w = ws * wh;
                    if w == 0.0 {
                        continue;
                    }
                    let c = self.cells(vi, hi, si);
                    for k in 0..3 {
                        out[k] += c[k] * w;
                    }
                }
            }
        }
        out
    }

    /// 对线性 ProPhoto 像素原地套表。规范第 5–7 步：色相加偏移、饱和度与
    /// 明度乘系数。明度只在原本不超过 1 时夹到 1——高光不该被表吃掉。
    fn apply(&self, rgb: &mut [f64; 3]) {
        if rgb.iter().any(|v| !v.is_finite()) {
            return;
        }
        let (h, s, v) = rgb_to_hsv(rgb[0].max(0.0), rgb[1].max(0.0), rgb[2].max(0.0));
        let vin = if self.encoded {
            lin_to_srgb(v.min(1.0))
        } else {
            v
        };
        let d = self.lookup(h, s, vin);
        let h = (h + d[0]).rem_euclid(360.0);
        let s = (s * d[1]).clamp(0.0, 1.0);
        let vt = (vin * d[2]).min(v.max(1.0));
        // 编码表的明度轴只到 1，超白光没有格点可对：拿表顶的系数当纯增益乘回
        // 线性值。系数 1.0 时原样穿过，v=1 处与下面那条分支连续。
        let vout = if self.encoded {
            if v > 1.0 {
                v * srgb_decode(d[2])
            } else {
                srgb_decode(vt)
            }
        } else {
            vt
        };
        *rgb = hsv_to_rgb(h, s, vout);
    }

    /// 两张表分度相同就逐格插值；不同就按 `t` 取近的那张（规范要求同光源
    /// 下格式一致，真遇到畸形配置也不至于读出两层表之外的东西）。
    fn blend(a: &HsvTable, b: &HsvTable, t: f64) -> HsvTable {
        if (a.hue, a.sat, a.val, a.encoded) != (b.hue, b.sat, b.val, b.encoded) {
            return if t < 0.5 { a.clone() } else { b.clone() };
        }
        let data = a
            .data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| {
                [
                    x[0] + (y[0] - x[0]) * t,
                    x[1] + (y[1] - x[1]) * t,
                    x[2] + (y[2] - x[2]) * t,
                ]
            })
            .collect();
        HsvTable { data, ..a.clone() }
    }
}

/// ProPhoto（RIMM）原色 + D50 白的 RGB→XYZ(D50)，Y 归一。
const PP_TO_XYZ: Matrix = [
    [0.797_674_9, 0.135_191_7, 0.031_353_4],
    [0.288_040_2, 0.711_874_1, 0.000_085_7],
    [0.0, 0.0, 0.825_210_0],
];

impl Profile {
    /// 读并解析一份 `.dcp`。
    ///
    /// Reads and parses a `.dcp` file.
    pub fn load(path: impl AsRef<Path>) -> Result<Profile, String> {
        let path = path.as_ref();
        let data =
            std::fs::read(path).map_err(|e| format!("读 DCP 失败（{}）：{e}", path.display()))?;
        let mut profile = parse_bytes(&data)?;
        profile.path = path.display().to_string();
        Ok(profile)
    }

    pub fn to_json(&self) -> Value {
        let first = self.hue_sat[0].as_ref().or(self.hue_sat[1].as_ref());
        serde_json::json!({
            "profile": self.name,
            "camera": self.camera,
            "path": self.path,
            "matrices": self.matrix_count(),
            "tone_curve_points": self.tone.len(),
            "baseline_ev": self.baseline_ev,
            "hue_sat_map": status(&self.hue_sat[0], &self.hue_sat[1]),
            "hue_sat_dims": first.map(|t| vec![t.hue, t.sat, t.val]),
            "look_table": if self.look.is_some() { "applied" } else { "absent" },
            "look_dims": self.look.as_ref().map(|t| vec![t.hue, t.sat, t.val]),
        })
    }

    fn matrix_count(&self) -> usize {
        self.color.iter().filter(|m| m.is_some()).count()
    }

    /// 这份配置是否给这台机身。面板在套用前先问一句，错配当场就能看到原因。
    ///
    /// Whether this profile belongs to this camera body. The panel asks
    /// before applying, so a mismatch shows its cause on the spot.
    pub fn matches(&self, make: &str, model: &str) -> bool {
        camera_matches(&self.camera, make, model)
    }

    /// 有 ForwardMatrix 就用它（Adobe 的推荐路径），否则退回 ColorMatrix。
    fn matrices(&self) -> ([Option<Matrix>; 2], bool) {
        if self.forward.iter().any(Option::is_some) {
            (self.forward, true)
        } else {
            (self.color, false)
        }
    }

    /// 双光源的 HueSatMap 按与矩阵相同的白平衡权重插值。
    fn hue_sat_table(&self, weight: f64) -> Option<HsvTable> {
        match (&self.hue_sat[0], &self.hue_sat[1]) {
            (Some(a), Some(b)) => Some(HsvTable::blend(a, b, weight)),
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        }
    }
}

fn status(a: &Option<HsvTable>, b: &Option<HsvTable>) -> &'static str {
    if a.is_some() || b.is_some() {
        "applied"
    } else {
        "absent"
    }
}

/// 解码并按 DCP 渲染。返回（像素、宽、高、位深是否高于 8bit）。
///
/// Decodes and renders through the DCP. Returns (pixels, width, height,
/// whether the bit depth is above 8bit).
pub fn decode(path: &Path, profile_path: &Path) -> Result<(Vec<f32>, u32, u32, bool), String> {
    let profile = Profile::load(profile_path)?;
    let (make, model, neutral) = source_facts(path)?;
    if !camera_matches(&profile.camera, &make, &model) {
        return Err(format!(
            "相机配置是给「{}」的，当前机型是「{}」",
            profile.camera,
            camera_line(&make, &model)
        ));
    }
    let (mut rgb, w, h, deep, origin) =
        crate::metrics::decode_rgb_f32_with_origin(path).map_err(|e| e.to_string())?;
    if w == 0 || h == 0 {
        return Err("解码结果为空".to_owned());
    }
    // 白平衡只补在「谁没做」的那条路上：内置 DNG 解析不解白平衡，这里补；RAW 后端
    // 已经按 AsShotNeutral 做过定点增益，再乘一次就是双份。判据用解码路由，不用
    // 文件后缀——同一张 `.dng` 既可能被内置接下来（单通道 CFA），也可能落到后端
    // （三通道 LinearRaw），后缀说不出这件事。
    //
    // White balance is added only on the route that did not do it: the built-in DNG parser does
    // not apply any, so it is added here, while the RAW backend has already applied the fixed-point
    // gains from AsShotNeutral, and a second application would double it. The test is the decode
    // route, not the file suffix: the same `.dng` may be handled by the built-in parser
    // (single-channel CFA) or fall through to the backend (three-channel LinearRaw), and the suffix
    // cannot tell them apart.
    //
    // 2026-11-07：`decode_rgb_f32_with_origin` 的传感器出口现在做 sRGB 显示编码
    // （普通无配置路径的「复位变黑」修复）。DCP 的矩阵/色调曲线数学吃**线性**输入、
    // 自己在出口 `lin_to_srgb`——入口先把那层编码撤销（srgb_decode），数学原样，
    // 输出域不变。
    //
    // 2026-11-07: the sensor exit of `decode_rgb_f32_with_origin` now display-encodes
    // to sRGB (the "reset goes black" fix for the plain, profile-less path). DCP's
    // matrix / tone-curve math takes **linear** input and encodes at its own exit via
    // `lin_to_srgb` -- undo the encoding here (srgb_decode) so the math and the output
    // domain stay exactly as before.
    if origin != crate::metrics::PixelOrigin::Image {
        rgb.par_chunks_exact_mut(3).for_each(|px| {
            for v in px.iter_mut() {
                let c = f64::from(*v);
                *v = if c <= 0.04045 {
                    (c / 12.92) as f32
                } else {
                    (f64::powf((c + 0.055) / 1.055, 2.4)) as f32
                };
            }
        });
    }
    if origin == crate::metrics::PixelOrigin::DngBuiltin {
        let neutral = neutral
            .ok_or_else(|| "这个 DNG 没有 AsShotNeutral，无法做相机配置白平衡".to_owned())?;
        apply_neutral(&mut rgb, &neutral);
    }

    let (matrices, _forward) = profile.matrices();
    let weight = blend_weight(&profile, neutral);
    let matrix =
        pick_matrix(&matrices, weight).ok_or_else(|| "配置里没有可用的颜色矩阵".to_owned())?;
    let ev = profile.baseline_ev;
    let tone = profile.tone.clone();
    let gain = 2f64.powf(ev);
    let hue_sat = profile.hue_sat_table(weight);
    let look = profile.look.clone();
    if hue_sat.is_some() || look.is_some() {
        // 两张表都定义在 ProPhoto（RIMM）线性 RGB 上。曝光是纯缩放，放在
        // ProPhoto 里乘等于在 sRGB 里乘，所以整段只在进出各换一次基。
        let xyz_to_pp = inv3(&PP_TO_XYZ).ok_or_else(|| "ProPhoto 矩阵不可逆".to_owned())?;
        rgb.par_chunks_exact_mut(3).for_each(|px| {
            let xyz = mul3(&matrix, &[px[0] as f64, px[1] as f64, px[2] as f64]);
            let mut pp = mul3(&xyz_to_pp, &xyz);
            // HueSatMap 在 XYZ(D50) 之后、曝光之前。
            if let Some(t) = &hue_sat {
                t.apply(&mut pp);
            }
            for v in &mut pp {
                *v *= gain;
            }
            // LookTable 在曝光之后、色调曲线之前。
            if let Some(t) = &look {
                t.apply(&mut pp);
            }
            let xyz = mul3(&PP_TO_XYZ, &pp);
            let srgb = xyz_d50_to_srgb(xyz[0], xyz[1], xyz[2]);
            for (ch, v) in px.iter_mut().enumerate() {
                let lin = srgb[ch].max(0.0);
                let curved = tone_value(&tone, lin);
                *v = lin_to_srgb(curved) as f32;
            }
        });
        return Ok((rgb, w, h, deep));
    }
    rgb.par_chunks_exact_mut(3).for_each(|px| {
        let r = px[0] as f64;
        let g = px[1] as f64;
        let b = px[2] as f64;
        // 相机线性 RGB → XYZ(D50) → 线性 sRGB(D65)。
        let x = matrix[0][0] * r + matrix[0][1] * g + matrix[0][2] * b;
        let y = matrix[1][0] * r + matrix[1][1] * g + matrix[1][2] * b;
        let z = matrix[2][0] * r + matrix[2][1] * g + matrix[2][2] * b;
        let srgb = xyz_d50_to_srgb(x, y, z);
        for (ch, v) in px.iter_mut().enumerate() {
            let lin = (srgb[ch] * gain).max(0.0);
            let curved = tone_value(&tone, lin);
            *v = lin_to_srgb(curved) as f32;
        }
    });
    Ok((rgb, w, h, deep))
}

/// 机型/白平衡事实：DNG 走 `dng::`，厂商 RAW 走 `raw::`，其它一律拒绝。
///
/// DNG 内置解析器只认单通道原始数据，三通道 LinearRaw（iPhone 导出的 DNG 就是）会被
/// 它拒掉；那不是文件坏了而是能力边界，机型事实得问后端——**解码走哪条路，机型事实
/// 就得跟哪条路**，否则「内置说解不了、后端说解得了」这两句话会同时出现在界面上。
fn source_facts(path: &Path) -> Result<(String, String, Option<[f64; 3]>), String> {
    if crate::dng::is_named_dng(path) {
        match crate::dng::info(path) {
            Ok(info) => {
                let neutral = crate::dng::as_shot_neutral(path).map_err(|e| e.to_string())?;
                return Ok((
                    info.make.unwrap_or_default(),
                    info.model.unwrap_or_default(),
                    neutral,
                ));
            }
            // 与 `metrics::decode` 同一个判据：软错才回落，硬错照内置的具体错误串报。
            Err(e) if e.soft_fallback() => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if crate::raw::is_raw_name(path) || crate::dng::is_named_dng(path) {
        let info = crate::raw::info(path).map_err(|e| e.to_string())?;
        if info.path == crate::raw::RawPath::Preview {
            return Err(
                "这一帧走的是机身内嵌 JPEG 预览，没有相机原始数据，套不了相机配置".to_owned(),
            );
        }
        return Ok((
            info.make.unwrap_or_default(),
            info.model.unwrap_or_default(),
            None,
        ));
    }
    Err("相机配置只用于 RAW/DNG 原始数据，JPEG/TIFF 已经是成品色彩".to_owned())
}

/// 按 `AsShotNeutral` 乘白平衡增益（绿通道归一）。和 `raw::cfa` 的固定点算法
/// 同一个口径，只是这里在 f32 像素上做，不额外丢位深。
fn apply_neutral(rgb: &mut [f32], neutral: &[f64; 3]) {
    let g = neutral[1];
    if !(g.is_finite() && g > 0.0) {
        return;
    }
    let mut gains = [1.0f32; 3];
    for (i, gain) in gains.iter_mut().enumerate() {
        let n = neutral[i];
        if n.is_finite() && n > 0.0 {
            *gain = (g / n) as f32;
        }
    }
    rgb.par_chunks_exact_mut(3).for_each(|px| {
        for (ch, v) in px.iter_mut().enumerate() {
            *v *= gains[ch];
        }
    });
}

fn pick_matrix(matrices: &[Option<Matrix>; 2], weight: f64) -> Option<Matrix> {
    match (matrices[0], matrices[1]) {
        (Some(a), Some(b)) => {
            let mut out = [[0.0; 3]; 3];
            for r in 0..3 {
                for c in 0..3 {
                    out[r][c] = a[r][c] + (b[r][c] - a[r][c]) * weight;
                }
            }
            Some(out)
        }
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// 双光源插值权重：把拍摄白点（AsShotNeutral）投影到两条标定白点连线上。
fn blend_weight(p: &Profile, neutral: Option<[f64; 3]>) -> f64 {
    let (Some(m1), Some(m2)) = (p.color[0], p.color[1]) else {
        return 1.0;
    };
    let Some(shot) = neutral else {
        return 1.0;
    };
    let ill1 = illuminant_xyz(p.illuminant[0].unwrap_or(21));
    let ill2 = illuminant_xyz(p.illuminant[1].unwrap_or(21));
    let (Some(n1), Some(n2)) = (
        inv3(&m1).map(|inv| mul3(&inv, &ill1)),
        inv3(&m2).map(|inv| mul3(&inv, &ill2)),
    ) else {
        return 1.0;
    };
    let a = chroma(n1);
    let b = chroma(n2);
    let s = chroma(shot);
    let d = (b[0] - a[0], b[1] - a[1]);
    let denom = d.0 * d.0 + d.1 * d.1;
    if denom < 1e-12 {
        return 0.5;
    }
    let t = ((s[0] - a[0]) * d.0 + (s[1] - a[1]) * d.1) / denom;
    t.clamp(0.0, 1.0)
}

fn chroma(v: [f64; 3]) -> [f64; 2] {
    let y = if v[1].abs() > 1e-9 { v[1] } else { 1.0 };
    [v[0] / y, v[2] / y]
}

fn mul3(m: &Matrix, v: &[f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn inv3(m: &Matrix) -> Option<Matrix> {
    let a = m[0][0];
    let b = m[0][1];
    let c = m[0][2];
    let d = m[1][0];
    let e = m[1][1];
    let f = m[1][2];
    let g = m[2][0];
    let h = m[2][1];
    let i = m[2][2];
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (e * i - f * h) * inv,
            (c * h - b * i) * inv,
            (b * f - c * e) * inv,
        ],
        [
            (f * g - d * i) * inv,
            (a * i - c * g) * inv,
            (c * d - a * f) * inv,
        ],
        [
            (d * h - e * g) * inv,
            (b * g - a * h) * inv,
            (a * e - b * d) * inv,
        ],
    ])
}

/// DNG 光源码 → XYZ（Y=1）。只认常用的三个，其它按 D65。
fn illuminant_xyz(code: u32) -> [f64; 3] {
    match code {
        17 => [1.09850, 1.0, 0.35585], // Standard Light A
        23 => [0.96422, 1.0, 0.82521], // D50
        _ => [0.95047, 1.0, 1.08883],  // D65
    }
}

/// XYZ(D50) → 线性 sRGB：Bradford 色适应到 D65 后再换基。
fn xyz_d50_to_srgb(x: f64, y: f64, z: f64) -> [f64; 3] {
    // Bradford D50→D65 与 XYZ(D65)→sRGB 乘在一起的常数矩阵（SMPTE RP 177 口径）。
    const M: Matrix = [
        [3.1338561, -1.6168667, -0.4906146],
        [-0.9787684, 1.9161415, 0.0334540],
        [0.0719453, -0.2289914, 1.4052427],
    ];
    [
        M[0][0] * x + M[0][1] * y + M[0][2] * z,
        M[1][0] * x + M[1][1] * y + M[1][2] * z,
        M[2][0] * x + M[2][1] * y + M[2][2] * z,
    ]
}

fn lin_to_srgb(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_decode(v: f64) -> f64 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// 归一化 RGB（0..1、最大分量即 V）→ HSV，H ∈ [0,360)。
fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= 1e-12 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max <= 1e-12 { 0.0 } else { d / max };
    (h.rem_euclid(360.0), s, max)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - (h / 60.0 % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// 分段线性曲线；空表就是恒等映射。曲线两端之外按端点值取。
fn tone_value(tone: &[(f64, f64)], v: f64) -> f64 {
    if tone.is_empty() {
        return v;
    }
    if v <= tone[0].0 {
        return tone[0].1;
    }
    for pair in tone.windows(2) {
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        if v <= x1 {
            let d = x1 - x0;
            return if d.abs() < 1e-12 {
                y1
            } else {
                y0 + (y1 - y0) * (v - x0) / d
            };
        }
    }
    tone[tone.len() - 1].1
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

const TAG_UNIQUE_CAMERA_MODEL: u16 = 50708;
const TAG_COLOR_MATRIX_1: u16 = 50721;
const TAG_COLOR_MATRIX_2: u16 = 50722;
const TAG_CALIBRATION_ILLUMINANT_1: u16 = 50778;
const TAG_CALIBRATION_ILLUMINANT_2: u16 = 50779;
const TAG_PROFILE_NAME: u16 = 50936;
const TAG_HUE_SAT_DIMS: u16 = 50937;
const TAG_HUE_SAT_DATA_1: u16 = 50938;
const TAG_HUE_SAT_DATA_2: u16 = 50939;
const TAG_PROFILE_TONE_CURVE: u16 = 50940;
const TAG_FORWARD_MATRIX_1: u16 = 50964;
const TAG_FORWARD_MATRIX_2: u16 = 50965;
const TAG_LOOK_TABLE_DIMS: u16 = 50981;
const TAG_LOOK_TABLE_DATA: u16 = 50982;
const TAG_HUE_SAT_ENCODING: u16 = 51107;
const TAG_LOOK_TABLE_ENCODING: u16 = 51108;
const TAG_BASELINE_EXPOSURE_OFFSET: u16 = 51109;
/// DCP 的 TIFF magic：字面量 "CR"（0x4352），不是普通 TIFF 的 42。
const MAGIC_DCP: u16 = 0x4352;
const MAGIC_TIFF: u16 = 42;
const MAX_BYTES: u64 = 64 << 20;

struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

#[derive(Clone, Copy)]
struct Entry<'a> {
    typ: u16,
    count: u32,
    data: &'a [u8],
}

fn unit_bytes(typ: u16) -> Option<usize> {
    match typ {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

impl<'a> Tiff<'a> {
    fn parse(d: &'a [u8]) -> Result<Self, String> {
        let head = d.get(0..8).ok_or("不是 TIFF/DCP：文件太短")?;
        let le = match &head[0..2] {
            b"II" => true,
            b"MM" => false,
            _ => return Err("不是 TIFF/DCP：字节序标记不对".to_owned()),
        };
        let t = Self { d, le };
        let magic = t.u16(2)?;
        if magic != MAGIC_DCP && magic != MAGIC_TIFF {
            return Err(format!("不是 DCP：magic {magic:#06x}"));
        }
        Ok(t)
    }

    fn u16(&self, at: usize) -> Result<u16, String> {
        let b = self.d.get(at..at + 2).ok_or("DCP 被截断")?;
        Ok(if self.le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, at: usize) -> Result<u32, String> {
        let b = self.d.get(at..at + 4).ok_or("DCP 被截断")?;
        Ok(if self.le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }

    fn entry(&self, tag: u16) -> Result<Option<Entry<'a>>, String> {
        let ifd = self.u32(4)? as usize;
        let count = self.u16(ifd)? as usize;
        for i in 0..count {
            let at = ifd + 2 + i * 12;
            let rec = self.d.get(at..at + 12).ok_or("DCP 被截断")?;
            let t = if self.le {
                u16::from_le_bytes([rec[0], rec[1]])
            } else {
                u16::from_be_bytes([rec[0], rec[1]])
            };
            if t != tag {
                continue;
            }
            let typ = if self.le {
                u16::from_le_bytes([rec[2], rec[3]])
            } else {
                u16::from_be_bytes([rec[2], rec[3]])
            };
            let n = if self.le {
                u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]])
            } else {
                u32::from_be_bytes([rec[4], rec[5], rec[6], rec[7]])
            };
            let unit = unit_bytes(typ).ok_or_else(|| format!("标记 {tag} 的类型 {typ} 不支持"))?;
            let total = unit
                .checked_mul(n as usize)
                .ok_or_else(|| format!("标记 {tag} 的长度溢出"))?;
            let data = if total <= 4 {
                rec.get(8..8 + total)
                    .ok_or_else(|| format!("标记 {tag} 的内联值越界"))?
            } else {
                let off = if self.le {
                    u32::from_le_bytes([rec[8], rec[9], rec[10], rec[11]])
                } else {
                    u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]])
                } as usize;
                self.d
                    .get(off..off + total)
                    .ok_or_else(|| format!("标记 {tag} 的数据越界"))?
            };
            return Ok(Some(Entry {
                typ,
                count: n,
                data,
            }));
        }
        Ok(None)
    }

    fn text(&self, tag: u16) -> Option<String> {
        let e = self.entry(tag).ok()??;
        if e.typ != 2 {
            return None;
        }
        let end = e.data.iter().position(|b| *b == 0).unwrap_or(e.data.len());
        let s = String::from_utf8_lossy(&e.data[..end]).trim().to_owned();
        (!s.is_empty()).then_some(s)
    }

    fn numbers(&self, tag: u16) -> Result<Option<Vec<f64>>, String> {
        let Some(e) = self.entry(tag)? else {
            return Ok(None);
        };
        let read_u32 = |b: &[u8]| {
            if self.le {
                u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64
            } else {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64
            }
        };
        let read_i32 = |b: &[u8]| read_u32(b) as i32 as i64;
        let vals: Result<Vec<f64>, String> = match e.typ {
            3 => e
                .data
                .chunks_exact(2)
                .map(|b| {
                    Ok(if self.le {
                        u16::from_le_bytes([b[0], b[1]]) as f64
                    } else {
                        u16::from_be_bytes([b[0], b[1]]) as f64
                    })
                })
                .collect(),
            4 => e
                .data
                .chunks_exact(4)
                .map(|b| Ok(read_u32(b) as f64))
                .collect(),
            5 => e
                .data
                .chunks_exact(8)
                .map(|b| {
                    let den = read_u32(&b[4..8]);
                    if den == 0 {
                        return Err(format!("标记 {tag} 的分数分母为 0"));
                    }
                    Ok(read_u32(&b[0..4]) as f64 / den as f64)
                })
                .collect(),
            10 => e
                .data
                .chunks_exact(8)
                .map(|b| {
                    let den = read_i32(&b[4..8]);
                    if den == 0 {
                        return Err(format!("标记 {tag} 的分数分母为 0"));
                    }
                    Ok(read_i32(&b[0..4]) as f64 / den as f64)
                })
                .collect(),
            11 => e
                .data
                .chunks_exact(4)
                .map(|b| {
                    Ok(if self.le {
                        f32::from_le_bytes([b[0], b[1], b[2], b[3]])
                    } else {
                        f32::from_be_bytes([b[0], b[1], b[2], b[3]])
                    } as f64)
                })
                .collect(),
            12 => e
                .data
                .chunks_exact(8)
                .map(|b| {
                    let mut a = [0u8; 8];
                    a.copy_from_slice(b);
                    Ok(if self.le {
                        f64::from_le_bytes(a)
                    } else {
                        f64::from_be_bytes(a)
                    })
                })
                .collect(),
            other => Err(format!("标记 {tag} 的类型 {other} 不是数值")),
        };
        let vals = vals?;
        if vals.len() != e.count as usize {
            return Err(format!("标记 {tag} 的元素数量与声明不符"));
        }
        Ok(Some(vals))
    }
}

/// 解析字节流。`path` 留空，`load` 会补上。
///
/// Parses a byte stream. `path` is left empty; `load` fills it in.
pub fn parse_bytes(data: &[u8]) -> Result<Profile, String> {
    if data.len() as u64 > MAX_BYTES {
        return Err(format!("DCP 超过 {MAX_BYTES} 字节上限"));
    }
    let t = Tiff::parse(data)?;
    let camera = t
        .text(TAG_UNIQUE_CAMERA_MODEL)
        .ok_or_else(|| "DCP 缺 UniqueCameraModel".to_owned())?;
    let name = t.text(TAG_PROFILE_NAME).unwrap_or_else(|| camera.clone());
    let color = [
        matrix_of(t.numbers(TAG_COLOR_MATRIX_1)?)?,
        matrix_of(t.numbers(TAG_COLOR_MATRIX_2)?)?,
    ];
    let forward = [
        matrix_of(t.numbers(TAG_FORWARD_MATRIX_1)?)?,
        matrix_of(t.numbers(TAG_FORWARD_MATRIX_2)?)?,
    ];
    if color.iter().all(Option::is_none) && forward.iter().all(Option::is_none) {
        return Err("DCP 里没有 ColorMatrix 或 ForwardMatrix".to_owned());
    }
    let illum = [
        t.numbers(TAG_CALIBRATION_ILLUMINANT_1)?
            .and_then(|v| v.first().map(|x| *x as u32)),
        t.numbers(TAG_CALIBRATION_ILLUMINANT_2)?
            .and_then(|v| v.first().map(|x| *x as u32)),
    ];
    let mut tone = Vec::new();
    if let Some(vals) = t.numbers(TAG_PROFILE_TONE_CURVE)? {
        for pair in vals.chunks_exact(2) {
            tone.push((pair[0], pair[1]));
        }
        tone.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    let baseline_ev = t
        .numbers(TAG_BASELINE_EXPOSURE_OFFSET)?
        .and_then(|v| v.first().copied())
        .unwrap_or(0.0);
    let hue_sat = [
        hsv_table(
            &t,
            TAG_HUE_SAT_DIMS,
            TAG_HUE_SAT_DATA_1,
            TAG_HUE_SAT_ENCODING,
        )?,
        hsv_table(
            &t,
            TAG_HUE_SAT_DIMS,
            TAG_HUE_SAT_DATA_2,
            TAG_HUE_SAT_ENCODING,
        )?,
    ];
    let look = hsv_table(
        &t,
        TAG_LOOK_TABLE_DIMS,
        TAG_LOOK_TABLE_DATA,
        TAG_LOOK_TABLE_ENCODING,
    )?;
    if t.entry(TAG_HUE_SAT_DIMS)?.is_some() && hue_sat[0].is_none() && hue_sat[1].is_none() {
        return Err("有 HueSatMap 分度数却没有任何一张表数据".to_owned());
    }
    if t.entry(TAG_LOOK_TABLE_DIMS)?.is_some() && look.is_none() {
        return Err("有 LookTable 分度数却没有表数据".to_owned());
    }
    Ok(Profile {
        path: String::new(),
        name,
        camera,
        color,
        forward,
        illuminant: illum,
        hue_sat,
        look,
        tone,
        baseline_ev,
    })
}

/// 读一张 HSV 表。没有分度标签、或没有这张表的数据就是没有；单光源配置
/// 常见「有分度、没有 Data2」，这是合法的。有数据但长度或数值不对才算坏配置。
fn hsv_table(
    t: &Tiff,
    dims_tag: u16,
    data_tag: u16,
    enc_tag: u16,
) -> Result<Option<HsvTable>, String> {
    let Some(dims) = t.numbers(dims_tag)? else {
        return Ok(None);
    };
    let Some(raw) = t.numbers(data_tag)? else {
        return Ok(None);
    };
    if dims.len() != 3 {
        return Err(format!("标记 {dims_tag} 不是 3 个分度数"));
    }
    let (hue, sat, val) = (dims[0] as usize, dims[1] as usize, dims[2] as usize);
    if hue < 1 || sat < 2 || val < 1 {
        return Err(format!("HSV 表分度数不合法：{hue}×{sat}×{val}"));
    }
    let cells = hue
        .checked_mul(sat)
        .and_then(|x| x.checked_mul(val))
        .and_then(|x| x.checked_mul(3))
        .ok_or_else(|| format!("标记 {dims_tag} 的分度数溢出"))?;
    if raw.len() != cells {
        return Err(format!(
            "HSV 表数据 {len} 个，与 {hue}×{sat}×{val}×3 不符",
            len = raw.len()
        ));
    }
    let data: Vec<[f64; 3]> = raw.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    if data.iter().flatten().any(|v| !v.is_finite()) {
        return Err("HSV 表里有非有限数".to_owned());
    }
    let enc = t
        .numbers(enc_tag)?
        .and_then(|v| v.first().copied())
        .unwrap_or(0.0);
    Ok(Some(HsvTable {
        hue,
        sat,
        val,
        // 规范写明编码标签只对三维表有意义；二维表用不到明度索引。
        encoded: val > 1 && enc != 0.0,
        data,
    }))
}

fn matrix_of(vals: Option<Vec<f64>>) -> Result<Option<Matrix>, String> {
    let Some(vals) = vals else {
        return Ok(None);
    };
    if vals.len() == 12 {
        // 4 色平面（RGB + 第四通道）的矩阵是 4×3，第三行与第四通道的贡献
        // 不能靠截前 9 个数丢掉；本项目的解码只有 RGB 三通道。
        return Err("配置是 4 色平面（12 元矩阵），当前只支持 RGB 三通道相机".to_owned());
    }
    if vals.len() != 9 || vals.iter().any(|v| !v.is_finite()) {
        return Err(format!("颜色矩阵不是 9 个有限数（实际 {} 个）", vals.len()));
    }
    Ok(Some([
        [vals[0], vals[1], vals[2]],
        [vals[3], vals[4], vals[5]],
        [vals[6], vals[7], vals[8]],
    ]))
}

fn norm_text(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn camera_matches(profile: &str, make: &str, model: &str) -> bool {
    let p = norm_text(profile);
    let mut m = norm_text(model);
    if m.is_empty() {
        return false;
    }
    let mk = norm_text(make);
    if !mk.is_empty() && !m.starts_with(&mk) {
        m = format!("{mk}{m}");
    }
    p == m || p.contains(&m) || m.contains(&p)
}

fn camera_line(make: &str, model: &str) -> String {
    if make.is_empty() || model.starts_with(make) {
        model.to_owned()
    } else if model.is_empty() {
        make.to_owned()
    } else {
        format!("{make} {model}")
    }
}

// ---------------------------------------------------------------------------
// 自动查找
// ---------------------------------------------------------------------------

/// DCP 根目录：`AEVUM_DCP`（旧名 `LUMEN_DCP` 回退）→ Adobe Camera Raw 的默认位置。
///
/// DCP root directory: `AEVUM_DCP` (falls back to the old name `LUMEN_DCP`)
/// → the default Adobe Camera Raw location.
pub fn default_directory() -> Option<PathBuf> {
    if let Some(dir) = crate::env::var_os("DCP") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Some(path);
        }
        if path.is_file() {
            return path.parent().map(Path::to_path_buf);
        }
    }
    let adobe = Path::new("C:/ProgramData/Adobe/CameraRaw/CameraProfiles");
    adobe.is_dir().then(|| adobe.to_path_buf())
}

/// 按机型在默认目录里自动挑一份配置（优先 Adobe Standard）。
///
/// Automatically picks a profile for the model in the default directory
/// (Adobe Standard preferred).
pub fn auto_match(make: &str, model: &str) -> Result<Profile, String> {
    let dir = default_directory().ok_or_else(|| {
        "没找到 DCP 目录（设 AEVUM_DCP，或安装 Adobe Camera Raw 的相机配置）".to_owned()
    })?;
    let model_norm = norm_text(&camera_line(make, model));
    if model_norm.is_empty() {
        return Err("EXIF 里没有机型，无法自动找相机配置".to_owned());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(parent) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&parent) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("dcp"))
            {
                continue;
            }
            let name = norm_text(&path.file_name().unwrap_or_default().to_string_lossy());
            if name.contains(&model_norm) || model_norm.contains(&name) {
                candidates.push(path);
            }
        }
    }
    if candidates.is_empty() {
        return Err(format!(
            "{} 里没有 {} 的相机配置",
            dir.display(),
            camera_line(make, model)
        ));
    }
    // Adobe Standard 是通用外观，优先；其次路径越短越可能是主配置。
    candidates.sort_by_key(|p| {
        let standard = !norm_text(&p.to_string_lossy()).contains("adobestandard");
        (standard, p.to_string_lossy().len())
    });
    let mut last = String::new();
    for path in candidates.into_iter().take(6) {
        match Profile::load(&path) {
            Ok(profile) if camera_matches(&profile.camera, make, model) => return Ok(profile),
            Ok(_) => last = format!("{} 的机型对不上", path.display()),
            Err(e) => last = e,
        }
    }
    Err(if last.is_empty() {
        "候选配置都不匹配".to_owned()
    } else {
        last
    })
}

// ---------------------------------------------------------------------------
// 测试：合成 DCP 字节，不依赖本机 Adobe 目录
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一份最小 DCP：小端、magic "CR"、IFD0 依次放给定字段。
    fn build(fields: &[(u16, u16, u32, Vec<u8>)]) -> Vec<u8> {
        let ifd = 8usize;
        let data_start = ifd + 2 + fields.len() * 12 + 4;
        let mut data: Vec<u8> = Vec::new();
        let mut records: Vec<[u8; 12]> = Vec::new();
        let mut at = data_start as u32;
        for (tag, typ, count, payload) in fields {
            let mut rec = [0u8; 12];
            rec[0..2].copy_from_slice(&tag.to_le_bytes());
            rec[2..4].copy_from_slice(&typ.to_le_bytes());
            rec[4..8].copy_from_slice(&count.to_le_bytes());
            if payload.len() <= 4 {
                rec[8..8 + payload.len()].copy_from_slice(payload);
            } else {
                rec[8..12].copy_from_slice(&at.to_le_bytes());
                data.extend_from_slice(payload);
                at += payload.len() as u32;
            }
            records.push(rec);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&MAGIC_DCP.to_le_bytes());
        out.extend_from_slice(&(ifd as u32).to_le_bytes());
        out.extend_from_slice(&(records.len() as u16).to_le_bytes());
        for r in records {
            out.extend_from_slice(&r);
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn ascii(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn short(v: u16) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }

    fn srational(vals: &[(i32, i32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (n, d) in vals {
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&d.to_le_bytes());
        }
        out
    }

    fn rational(vals: &[(u32, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (n, d) in vals {
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&d.to_le_bytes());
        }
        out
    }

    fn long(vals: &[u32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn float(vals: &[f32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn identity() -> Vec<(i32, i32)> {
        vec![
            (1, 1),
            (0, 1),
            (0, 1),
            (0, 1),
            (1, 1),
            (0, 1),
            (0, 1),
            (0, 1),
            (1, 1),
        ]
    }

    fn sample() -> Vec<u8> {
        build(&[
            (TAG_UNIQUE_CAMERA_MODEL, 2, 10, ascii("Test Cam")),
            (TAG_PROFILE_NAME, 2, 14, ascii("Test Standard")),
            (TAG_COLOR_MATRIX_1, 10, 9, srational(&identity())),
            (TAG_CALIBRATION_ILLUMINANT_1, 3, 1, short(21)),
            (
                TAG_PROFILE_TONE_CURVE,
                5,
                4,
                rational(&[(0, 1), (0, 1), (1, 1), (1, 1)]),
            ),
            (TAG_BASELINE_EXPOSURE_OFFSET, 10, 1, srational(&[(-1, 2)])),
        ])
    }

    #[test]
    fn parses_magic_cr_fields() {
        let p = parse_bytes(&sample()).expect("合成 DCP 应能解析");
        assert_eq!(p.camera, "Test Cam");
        assert_eq!(p.name, "Test Standard");
        assert_eq!(p.matrix_count(), 1);
        assert_eq!(p.tone.len(), 2);
        assert!((p.baseline_ev + 0.5).abs() < 1e-12);
        assert!(p.hue_sat[0].is_none() && p.hue_sat[1].is_none() && p.look.is_none());
        let json = p.to_json();
        assert_eq!(json["hue_sat_map"].as_str(), Some("absent"));
        assert_eq!(json["matrices"].as_u64(), Some(1));
    }

    /// 三张表都齐的最小 DCP：2 段色相、2 段饱和、1 段明度。
    fn with_tables(data1: &[f32], data2: Option<&[f32]>) -> Vec<u8> {
        let mut fields = vec![
            (TAG_UNIQUE_CAMERA_MODEL, 2, 10, ascii("Test Cam")),
            (TAG_COLOR_MATRIX_1, 10, 9, srational(&identity())),
            (TAG_HUE_SAT_DIMS, 4, 3, long(&[2, 2, 1])),
            (TAG_HUE_SAT_DATA_1, 11, data1.len() as u32, float(data1)),
        ];
        if let Some(d2) = data2 {
            fields.push((TAG_HUE_SAT_DATA_2, 11, d2.len() as u32, float(d2)));
        }
        build(&fields)
    }

    #[test]
    fn parses_hue_sat_table_and_reports_applied() {
        // 2×2×1：8 格 × 3。索引 (h, s) = h*2+s。
        let data = [
            120.0, 1.0, 1.0, // h=0° s=0
            120.0, 1.0, 1.0, // h=0° s=1
            0.0, 1.0, 1.0, // h=180° s=0
            0.0, 0.5, 1.0, // h=180° s=1
        ];
        let p = parse_bytes(&with_tables(&data, None)).expect("带表的 DCP 应能解析");
        let t = p.hue_sat[0].as_ref().expect("Data1 要读进来");
        assert_eq!((t.hue, t.sat, t.val), (2, 2, 1));
        assert!(!t.encoded);
        assert_eq!(p.to_json()["hue_sat_map"].as_str(), Some("applied"));
        // 红（h=0°, s=1）拿到 +120° 偏移，变成绿。
        let mut red = [1.0, 0.0, 0.0];
        t.apply(&mut red);
        assert!(
            red[0].abs() < 1e-9 && (red[1] - 1.0).abs() < 1e-9,
            "得到 {red:?}"
        );
        // 蓝（240°）落在 180°–360°(≡0°) 段内 1/3 处：偏移在 0° 与 120° 之间
        // 插出 40°，饱和度在 1.0 与 0.5 之间插出 2/3。
        let d = t.lookup(240.0, 1.0, 1.0);
        assert!((d[0] - 40.0).abs() < 1e-9, "得到 {d:?}");
        assert!((d[1] - 2.0 / 3.0).abs() < 1e-9, "得到 {d:?}");
        let mut blue = [0.0, 0.0, 1.0];
        t.apply(&mut blue);
        assert!((blue[2] - 1.0).abs() < 1e-9, "得到 {blue:?}");
        assert!((blue[1] - 1.0 / 3.0).abs() < 1e-9, "得到 {blue:?}");
    }

    #[test]
    fn parses_look_table_and_encoding_flag() {
        let data = [0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0, 0.0, 0.7, 1.0];
        let bytes = build(&[
            (TAG_UNIQUE_CAMERA_MODEL, 2, 10, ascii("Test Cam")),
            (TAG_COLOR_MATRIX_1, 10, 9, srational(&identity())),
            (TAG_LOOK_TABLE_DIMS, 4, 3, long(&[2, 2, 1])),
            (TAG_LOOK_TABLE_DATA, 11, 12, float(&data)),
            (TAG_LOOK_TABLE_ENCODING, 4, 1, long(&[1])),
        ]);
        let p = parse_bytes(&bytes).expect("带 LookTable 的 DCP 应能解析");
        let t = p.look.as_ref().expect("LookTable 要读进来");
        assert_eq!((t.hue, t.sat, t.val), (2, 2, 1));
        // 2.5D 表不按编码索引（规范：编码标签不适用于明度只有 1 段的表）。
        assert!(!t.encoded);
        assert_eq!(p.to_json()["look_table"].as_str(), Some("applied"));
    }

    #[test]
    fn rejects_table_with_wrong_size() {
        let bytes = build(&[
            (TAG_UNIQUE_CAMERA_MODEL, 2, 10, ascii("Test Cam")),
            (TAG_COLOR_MATRIX_1, 10, 9, srational(&identity())),
            (TAG_HUE_SAT_DIMS, 4, 3, long(&[3, 2, 1])),
            (TAG_HUE_SAT_DATA_1, 11, 6, float(&[0.0; 6])),
        ]);
        assert!(parse_bytes(&bytes).is_err(), "表长与分度数不符要报错");
    }

    fn table(hue: usize, sat: usize, val: usize, data: &[[f64; 3]]) -> HsvTable {
        HsvTable {
            hue,
            sat,
            val,
            encoded: false,
            data: data.to_vec(),
        }
    }

    #[test]
    fn hsv_conversion_round_trips() {
        for rgb in [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.25, 0.5, 0.75],
            [0.5, 0.5, 0.5],
            [0.0, 0.0, 0.0],
        ] {
            let (h, s, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
            let back = hsv_to_rgb(h, s, v);
            for k in 0..3 {
                assert!((back[k] - rgb[k]).abs() < 1e-9, "{rgb:?} 往返得到 {back:?}");
            }
        }
        // 绕回：359.9° 与 -0.1° 是同一色相。
        let a = hsv_to_rgb(359.9, 1.0, 1.0);
        let b = hsv_to_rgb(-0.1, 1.0, 1.0);
        assert!((a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3);
    }

    #[test]
    fn table_indexing_uses_degrees_and_endpoint_saturation() {
        // 2 段色相：0° 与 180°；偏移 0° 与 100°。
        let t = table(
            2,
            2,
            1,
            &[
                [0.0, 1.0, 1.0],
                [0.0, 1.0, 1.0],
                [100.0, 1.0, 1.0],
                [100.0, 1.0, 1.0],
            ],
        );
        // 90° 正好在两段之间，取一半。
        let d = t.lookup(90.0, 1.0, 1.0);
        assert!((d[0] - 50.0).abs() < 1e-9, "得到 {d:?}");
        // 色相环绕：300° 落在 180°–360° 段内 2/3 处 → 100° 的 1/3。
        let d = t.lookup(300.0, 1.0, 1.0);
        assert!((d[0] - 100.0 / 3.0).abs() < 1e-9, "得到 {d:?}");
        // 饱和 1.0 必须落在最后一格（端点包含），不能落到表外。
        let d = t.lookup(180.0, 1.0, 1.0);
        assert!((d[0] - 100.0).abs() < 1e-9, "得到 {d:?}");
    }

    #[test]
    fn apply_shifts_hue_scales_saturation_and_value() {
        // 单段色相、两段饱和：饱和 1 时乘 0.5。
        let t = table(1, 2, 1, &[[0.0, 1.0, 1.0], [0.0, 0.5, 0.8]]);
        let mut red = [1.0, 0.0, 0.0];
        t.apply(&mut red);
        // 饱和度 ×0.5、明度 ×0.8：c = 0.8·0.5，白底 0.4。
        assert!((red[0] - 0.8).abs() < 1e-9, "得到 {red:?}");
        assert!((red[1] - 0.4).abs() < 1e-9, "得到 {red:?}");
        assert!((red[2] - 0.4).abs() < 1e-9, "得到 {red:?}");
        // 明度系数 0.8：灰 0.5 → 0.4。
        let mut grey = [0.5, 0.5, 0.5];
        t.apply(&mut grey);
        // 灰的饱和是 0，走第一格（系数 1）。
        assert!((grey[0] - 0.5).abs() < 1e-9, "得到 {grey:?}");
        // 高光不被表压到 1 以下：线性值 2.0 原样保留（规范字面会夹到 1，
        // 这里对超过 1 的输入只保证不放大）。
        let mut hot = [2.0, 0.0, 0.0];
        let t1 = table(1, 2, 1, &[[0.0, 1.0, 1.0], [0.0, 1.0, 1.0]]);
        t1.apply(&mut hot);
        assert!((hot[0] - 2.0).abs() < 1e-9, "高光被压到 {hot:?}");
    }

    #[test]
    fn encoded_table_round_trips_neutral() {
        let data = vec![[0.0, 1.0, 1.0]; 8];
        let mut t = table(2, 2, 2, &data);
        t.encoded = true;
        let mut grey = [0.25, 0.25, 0.25];
        t.apply(&mut grey);
        for v in grey {
            assert!((v - 0.25).abs() < 1e-6, "编码往返漂了：{grey:?}");
        }
        // 编码后索引的是 sRGB 值：暗部的网格更密。这里直接核对查表位置。
        let d = t.lookup(0.0, 0.0, lin_to_srgb(0.25));
        assert!(d.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn blending_two_tables_mixes_cells() {
        let a = table(1, 2, 1, &[[0.0, 1.0, 1.0], [0.0, 1.0, 1.0]]);
        let b = table(1, 2, 1, &[[60.0, 2.0, 1.0], [60.0, 2.0, 1.0]]);
        let m = HsvTable::blend(&a, &b, 0.25);
        assert!((m.data[0][0] - 15.0).abs() < 1e-9);
        assert!((m.data[0][1] - 1.25).abs() < 1e-9);
        // 分度不同的畸形组合：按权重取近的一张。
        let c = table(2, 2, 1, &[[0.0; 3]; 4]);
        let m = HsvTable::blend(&a, &c, 0.6);
        assert_eq!(m.hue, 2);
    }

    #[test]
    fn prophoto_matrix_maps_white_to_d50() {
        let white = mul3(&PP_TO_XYZ, &[1.0, 1.0, 1.0]);
        let d50 = illuminant_xyz(23);
        for k in 0..3 {
            assert!(
                (white[k] - d50[k]).abs() < 2e-3,
                "ProPhoto 白点应落在 D50，得到 {white:?}"
            );
        }
        let back = mul3(&inv3(&PP_TO_XYZ).unwrap(), &white);
        for k in 0..3 {
            assert!((back[k] - 1.0).abs() < 2e-3, "逆矩阵往返得到 {back:?}");
        }
    }

    #[test]
    fn rejects_bad_magic_and_missing_camera() {
        let mut bad = sample();
        bad[2..4].copy_from_slice(&0x1234u16.to_le_bytes());
        assert!(parse_bytes(&bad).is_err());

        let no_camera = build(&[(TAG_COLOR_MATRIX_1, 10, 9, srational(&identity()))]);
        assert!(parse_bytes(&no_camera).is_err());

        let no_matrix = build(&[(TAG_UNIQUE_CAMERA_MODEL, 2, 9, ascii("Test Cam"))]);
        assert!(parse_bytes(&no_matrix).is_err());

        assert!(parse_bytes(b"II*\0").is_err(), "截断文件要报错");
    }

    #[test]
    fn camera_match_handles_make_prefix_and_mismatch() {
        assert!(camera_matches(
            "Canon EOS 5D Mark II",
            "Canon",
            "EOS 5D Mark II"
        ));
        assert!(camera_matches(
            "Canon EOS 5D Mark II",
            "Canon",
            "Canon EOS 5D Mark II"
        ));
        assert!(camera_matches("Canon EOS 5D Mark II", "", "EOS 5D Mark II"));
        assert!(!camera_matches("Canon EOS 5D Mark II", "Nikon", "D850"));
        assert!(!camera_matches("Canon EOS 5D Mark II", "", ""));
    }

    fn dual(illum1: u32, illum2: u32) -> Profile {
        let m = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut p = Profile::default();
        p.color = [Some(m), Some(m)];
        p.illuminant = [Some(illum1), Some(illum2)];
        p
    }

    #[test]
    fn dual_illuminant_weight_follows_neutral() {
        // 两组都是单位矩阵：被摄白点等于哪组光源，权重就贴哪一端。
        let p = dual(21, 23);
        let d65 = illuminant_xyz(21);
        let d50 = illuminant_xyz(23);
        assert!(blend_weight(&p, Some(d65)) < 1e-9);
        assert!((blend_weight(&p, Some(d50)) - 1.0).abs() < 1e-9);
        let mid = [(d65[0] + d50[0]) / 2.0, 1.0, (d65[2] + d50[2]) / 2.0];
        let w = blend_weight(&p, Some(mid));
        assert!((0.3..0.7).contains(&w), "中点权重应在中间，得到 {w}");
        // 单光源或没白平衡：直接用第二组，不猜。
        assert!((blend_weight(&p, None) - 1.0).abs() < 1e-12);
        let mut single = p.clone();
        single.color[0] = None;
        assert!((blend_weight(&single, Some(d65)) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn matrix_pick_interpolates_and_falls_back() {
        let a = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let b = [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0]];
        let m = pick_matrix(&[Some(a), Some(b)], 0.25).unwrap();
        assert!((m[0][0] - 1.25).abs() < 1e-12);
        assert_eq!(pick_matrix(&[None, None], 0.5), None);
        assert_eq!(pick_matrix(&[None, Some(b)], 0.9).unwrap()[0][0], 2.0);
    }

    #[test]
    fn tone_curve_interpolates_and_clamps() {
        let tone = [(0.0, 0.0), (0.5, 0.4), (1.0, 1.0)];
        assert!((tone_value(&tone, 0.25) - 0.2).abs() < 1e-12);
        assert!((tone_value(&tone, -1.0) - 0.0).abs() < 1e-12);
        assert!((tone_value(&tone, 2.0) - 1.0).abs() < 1e-12);
        assert!((tone_value(&[], 0.3) - 0.3).abs() < 1e-12);
        // 重复 x 不能除零。
        let flat = [(0.5, 0.1), (0.5, 0.9)];
        assert!(tone_value(&flat, 0.7).is_finite());
    }

    #[test]
    fn d50_white_maps_to_neutral_srgb() {
        let d50 = illuminant_xyz(23);
        let rgb = xyz_d50_to_srgb(d50[0], d50[1], d50[2]);
        for v in rgb {
            assert!((v - 1.0).abs() < 2e-3, "D50 白点应回到 1,1,1，得到 {rgb:?}");
        }
        assert!((lin_to_srgb(0.0)).abs() < 1e-12);
        assert!((lin_to_srgb(1.0) - 1.0).abs() < 1e-9);
    }

    /// 编码表（`*Encoding`=1，真机语料里 2516 份 LookTable 是这种）的明度轴只到 1。
    /// 超白光必须按顶格系数等比抬走：系数 1 原样穿过，系数在半程时 2 倍输入给 2 倍输出。
    /// 旧实现先 `min(1)` 再乘，把一切 v>1 的像素夹回系数本身，高光直接塌成白色。
    #[test]
    fn encoded_table_scales_super_white_instead_of_collapsing_it() {
        let table = |mult: f64| HsvTable {
            hue: 2,
            sat: 2,
            val: 5,
            encoded: true,
            data: vec![[0.0, 1.0, mult]; 2 * 2 * 5],
        };
        let mut ident = [1.6, 0.4, 0.0];
        table(1.0).apply(&mut ident);
        assert!(
            (ident[0] - 1.6).abs() < 1e-9 && (ident[1] - 0.4).abs() < 1e-9,
            "恒等表不该动超白光：{ident:?}"
        );

        let half = srgb_decode(0.5);
        let mut at_one = [1.0, 1.0, 1.0];
        table(0.5).apply(&mut at_one);
        assert!(
            (at_one[0] - half).abs() < 1e-9,
            "v=1 走编码域乘法，应得 decode(0.5)={half}，实际 {:?}",
            at_one[0]
        );
        let mut at_two = [2.0, 2.0, 2.0];
        table(0.5).apply(&mut at_two);
        assert!(
            (at_two[0] - 2.0 * half).abs() < 1e-9,
            "v=2 要给两倍的 v=1 结果，实际 {:?}",
            at_two[0]
        );
    }

    /// 编码表在 v≤1 的整段行为一个比特都不许变：真机校正结果与金标都压在这条上。
    #[test]
    fn encoded_table_below_one_is_unchanged() {
        let table = HsvTable {
            hue: 3,
            sat: 2,
            val: 5,
            encoded: true,
            data: (0..3 * 2 * 5)
                .map(|i| [((i % 7) as f64) - 3.0, 0.5 + (i % 3) as f64 * 0.2, 0.9])
                .collect(),
        };
        for v in [0.0, 0.013, 0.25, 0.5, 0.9, 1.0] {
            for s in [0.0, 0.4, 1.0] {
                let mut a = hsv_to_rgb(37.0, s, v);
                // 灰与黑没有色相可回读，按 apply 的口径从像素本身再算一遍。
                let (h2, s2, v2) = rgb_to_hsv(a[0].max(0.0), a[1].max(0.0), a[2].max(0.0));
                table.apply(&mut a);
                let d = table.lookup(h2, s2, lin_to_srgb(v2));
                let want = hsv_to_rgb(
                    (h2 + d[0]).rem_euclid(360.0),
                    (s2 * d[1]).clamp(0.0, 1.0),
                    srgb_decode((lin_to_srgb(v2) * d[2]).min(v2.max(1.0))),
                );
                for c in 0..3 {
                    assert!(
                        (a[c] - want[c]).abs() < 1e-12,
                        "v={v} s={s} 通道 {c}：{a:?} vs {want:?}"
                    );
                }
            }
        }
    }
}
