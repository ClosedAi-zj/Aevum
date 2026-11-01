//! LCP 镜头配置：读 Adobe `.lcp`（XMP 封装的 XML），按 EXIF 的机身、镜头、焦距
//! 选出对应的标定帧，折成一份 [`Correction`]，在显影链最前面应用到像素。
//!
//! LCP lens profiles: reads Adobe `.lcp` files (XML wrapped in XMP), picks the calibration
//! frame matching the EXIF body, lens, and focal length, folds it into one [`Correction`],
//! and applies it to pixels at the very front of the develop chain.
//!
//! 数据形状来自 Adobe Camera Raw 自带的 3633 份在野样本，两种写法都认：
//! 属性式（`<rdf:Description stCamera:Model="...">`）与元素式
//! （`<stCamera:Model>...</stCamera:Model>`）；模型既可以挂在 `rdf:Description`
//! 下，也可以嵌在 `PerspectiveModel` 里。命名空间前缀一律不认，只认本地名。
//!
//! The data shapes come from 3633 in-the-wild samples shipped with Adobe Camera Raw; both
//! spellings are accepted: attribute form (`<rdf:Description stCamera:Model="...">`) and
//! element form (`<stCamera:Model>...</stCamera:Model>`); the model may hang directly under
//! `rdf:Description` or be nested inside `PerspectiveModel`. Namespace prefixes are ignored;
//! only local names are matched.
//!
//! 归一化与公式按 Adobe Camera Model 的口径（与 RawTherapee 的实现一致）：
//!
//! Normalization and formulas follow the Adobe Camera Model convention (matching the
//! RawTherapee implementation):
//!
//! - 归一化坐标 `x = (px - cx) / fx`，`fx = FocalLengthX × max(w, h)`。
//!   `FocalLengthX` 是「以 35mm 画幅 35mm 为单位的焦距」，样本里 24mm 全幅
//!   就是 0.670062（≈24/35）。缺这个字段时用
//!   `FocalLength × SensorFormatFactor ÷ 35` 推。
//! - Normalized coordinates `x = (px - cx) / fx`, with `fx = FocalLengthX × max(w, h)`.
//!   `FocalLengthX` is the focal length in units of a 35mm-wide 35mm frame; a 24mm
//!   full-frame sample reads 0.670062 (≈24/35). When the field is missing it is derived as
//!   `FocalLength × SensorFormatFactor ÷ 35`.
//! - 畸变（校正图 → 源图）：`F = 1 + k1 r² + k2 r⁴ + k3 r⁶ + 2(p2 x + p1 y)`，
//!   `x_src = x·F + p2 r²`、`y_src = y·F + p1 r²`。
//!   `RadialDistortParam1..3` 是 k1..k3，`Param4`/`Param5` 是切向 p1/p2。
//! - Distortion (corrected image → source image):
//!   `F = 1 + k1 r² + k2 r⁴ + k3 r⁶ + 2(p2 x + p1 y)`,
//!   `x_src = x·F + p2 r²`, `y_src = y·F + p1 r²`.
//!   `RadialDistortParam1..3` are k1..k3; `Param4`/`Param5` are the tangential p1/p2.
//! - 鱼眼：`θ = atan(r)`（r 已是 tan 单位），`f = (1 + k1 θ² + k2 θ⁴)·θ / r`。
//! - Fisheye: `θ = atan(r)` (r is already in tan units), `f = (1 + k1 θ² + k2 θ⁴)·θ / r`.
//! - 横向色差：红、蓝各有一份相对绿通道的模型。绿通道只做基础畸变，红/蓝在
//!   绿通道校正后的坐标上再叠 `ScaleFactor` 与自己的径向/切向偏离。
//! - Lateral chromatic aberration: red and blue each carry a model relative to the green
//!   channel. Green gets only the base distortion; red/blue stack `ScaleFactor` and their
//!   own radial/tangential offsets on top of the green-corrected coordinates.
//! - 暗角：模型给的是**衰减** `V(r) = 1 + a1 r² + a2 r⁴ + a3 r⁶`，校正就是
//!   除以它——比 RawTherapee 的泰勒展开更准，也省一次近似。
//! - Vignetting: the model gives the **falloff** `V(r) = 1 + a1 r² + a2 r⁴ + a3 r⁶`; the
//!   correction divides by it — more accurate than RawTherapee's Taylor expansion and saves
//!   an approximation.
//! - 分段暗角 `VignetteModelPiecewiseParam`（551/3633 份文件里有）：控制点是
//!   `(r, V)`，r 与上面的归一化半径同口径（3633 帧里 99.8% 的末点落在画幅角
//!   半径上，相对误差中位数 0.02%），点间按 r 线性插值，V 单调不增。表比多项式
//!   更贴厂商实测（|差| 中位 0.0045、最大 0.13），所以表覆盖到画幅角时优先用它；
//!   退化的平表（V≡1，85 帧）和只管住一小段半径的表（7 帧）在输入里就能认出来，
//!   这时退回多项式，别拿没标过的半径硬外推。
//! - Piecewise vignetting `VignetteModelPiecewiseParam` (present in 551/3633 files): control
//!   points are `(r, V)`, with r on the same convention as the normalized radius above (in
//!   the 3633 frames, 99.8% of end points land on the frame-corner radius, median relative
//!   error 0.02%); points are linearly interpolated by r, and V is monotonically
//!   non-increasing. The table tracks vendor measurements better than the polynomial (median
//!   |diff| 0.0045, max 0.13), so a table covering the frame corner is preferred; degenerate
//!   flat tables (V≡1, 85 frames) and tables covering only a small radius span (7 frames)
//!   are recognizable from the input, in which case the code falls back to the polynomial
//!   instead of extrapolating hard into uncalibrated radii.
//!
//! 选帧：同焦距常有多帧（不同对焦距离/光圈）。先按焦距在**对数**上找到包住
//! 目标的两帧，再在对焦距离（畸变/色差）或光圈（暗角）上挑更接近的一帧；
//! 两帧之间的系数按对数焦距线性插值。
//!
//! Frame selection: one focal length often has several frames (different focus distances or
//! apertures). First find the two frames bracketing the target on a **logarithmic** focal
//! scale, then pick the closer one by focus distance (distortion/CA) or aperture
//! (vignetting); coefficients between the two frames are interpolated linearly by log focal
//! length.
//!
//! 旋转/镜像不处理：像素已按 EXIF 转正，而径向项与旋转无关、切向项在 1e-4
//! 量级、中心偏移只有千分之几。
//!
//! Rotation/mirroring is not handled: pixels are already uprighted per EXIF, radial terms
//! are rotation-invariant, tangential terms are on the order of 1e-4, and center offsets
//! are only a few thousandths.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::lensdb::{Node, parse_xml};

/// 一份 LCP 里的一帧标定：某机身 + 某镜头 + 某焦距/对焦距离/光圈。
#[derive(Clone, Debug, Default)]
struct Frame {
    camera: String,
    unique_camera: String,
    lens: String,
    /// 机身的 135 画幅换算系数。
    sensor: f64,
    /// 35mm 等效焦距（mm）：`FocalLength × SensorFormatFactor`。
    focal35: f64,
    /// 模型里的归一化焦距（以 35mm 画幅宽 35mm 为单位），几何换算用它。
    focal_norm: f64,
    aperture: Option<f64>,
    focus: Option<f64>,
    base: Option<Model>,
    fisheye: Option<Model>,
    /// 红/蓝通道相对绿通道的模型。
    ca: [Option<Model>; 2],
    vig: Option<Model>,
}

impl Frame {
    /// 选帧用的焦距键。优先 `FocalLength`（EXIF 同源，24 就是 24），
    /// 模型里的 `FocalLengthX` 常被厂商截到 6 位小数，拿它当键会让
    /// 24mm 的 EXIF 落在 0.685714 的右边，选帧滑到两帧之间的插值上。
    fn focal_key(&self) -> f64 {
        if self.focal35 > 0.0 {
            self.focal35 / 35.0
        } else {
            self.focal_norm
        }
    }
}

/// 一段模型：归一化焦距、中心、`ScaleFactor` 与五个系数。
///
/// 系数含义随模型不同：畸变是 k1..k3 + p1/p2，暗角是 a1..a3（后两个空着），
/// 色差是相对绿通道的径向/切向偏离。
#[derive(Clone, Debug, Default)]
struct Model {
    foc: [Option<f64>; 2],
    center: [Option<f64>; 2],
    scale: Option<f64>,
    param: [f64; 5],
    /// 暗角分段表 `(r, V)`，按 r 升序、V 是衰减。只有 `VignetteModel` 带。
    curve: Option<Vec<(f64, f64)>>,
}

impl Model {
    fn norm(&self, fallback: f64) -> [f64; 2] {
        [
            self.foc[0].unwrap_or(fallback),
            self.foc[1].unwrap_or(fallback),
        ]
    }

    fn center(&self) -> [f64; 2] {
        [self.center[0].unwrap_or(0.5), self.center[1].unwrap_or(0.5)]
    }

    fn tangential(&self) -> [f64; 2] {
        [self.param[3], self.param[4]]
    }
}

/// 解析后的整份 LCP。
///
/// A fully parsed LCP file.
#[derive(Clone, Debug, Default)]
pub struct LcpFile {
    /// 文件路径（面板显示用）。
    ///
    /// The file path (for panel display).
    pub path: String,
    /// `ProfileName`，多帧时取第一帧。
    ///
    /// The `ProfileName`; the first frame when there are several.
    pub profile: String,
    /// 文件覆盖的镜头名（多帧可能是同一支）。
    ///
    /// Lens names covered by the file (several frames may be the same lens).
    pub lens: String,
    pub camera: String,
    frames: Vec<Frame>,
}

impl LcpFile {
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

/// 一次匹配需要的 EXIF 事实。
///
/// The EXIF facts one match needs.
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub make: String,
    pub model: String,
    pub lens: String,
    /// 实际焦距（mm）。
    ///
    /// Actual focal length (mm).
    pub focal_mm: f64,
    /// 35mm 等效焦距（mm），EXIF 有就直接用。
    ///
    /// 35mm-equivalent focal length (mm); used directly when EXIF provides it.
    pub focal35_mm: Option<f64>,
    pub aperture: Option<f64>,
}

/// 折好的校正系数。放进 [`crate::grade::GradeParams`]（默认 `None`），
/// 预览、导出、队列自然全通。
///
/// The folded correction coefficients. Drop it into [`crate::grade::GradeParams`] (default
/// `None`) and preview, export, and the queue all pick it up.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Correction {
    pub lens: String,
    pub profile: String,
    /// 选中的标定焦距（35mm 等效），面板显示用。
    ///
    /// The selected calibration focal length (35mm equivalent), for panel display.
    pub focal35_mm: f64,
    /// 归一化焦距 `[x, y]`：`fx = norm[0] × max(w, h)`。
    ///
    /// Normalized focal lengths `[x, y]`: `fx = norm[0] × max(w, h)`.
    pub norm: [f64; 2],
    pub center: [f64; 2],
    /// 畸变 `[k1, k2, k3, p1, p2]`。
    ///
    /// Distortion `[k1, k2, k3, p1, p2]`.
    pub dist: Option<[f64; 5]>,
    /// 鱼眼 `[k1, k2]`；与 `dist` 互斥。
    ///
    /// Fisheye `[k1, k2]`; mutually exclusive with `dist`.
    pub fisheye: Option<[f64; 2]>,
    /// 红/蓝相对绿通道的色差模型。
    ///
    /// Red/blue chromatic aberration models relative to the green channel.
    pub ca: Option<[CaModel; 2]>,
    /// 暗角衰减系数 `[a1, a2, a3]`（校正时取倒数）。
    ///
    /// Vignetting falloff coefficients `[a1, a2, a3]` (inverted during correction).
    pub vig: Option<[f64; 3]>,
    /// 暗角分段表 `(r, V)`：r 升序、V 是衰减。覆盖画幅角时优先于 `vig`。
    ///
    /// Piecewise vignetting table `(r, V)`: r ascending, V the falloff. Preferred over `vig`
    /// when it covers the frame corner.
    #[serde(default)]
    pub vig_curve: Option<Vec<(f64, f64)>>,
    pub vig_norm: [f64; 2],
    pub vig_center: [f64; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaModel {
    pub scale: f64,
    pub param: [f64; 5],
}

impl Correction {
    pub fn is_empty(&self) -> bool {
        self.dist.is_none()
            && self.fisheye.is_none()
            && self.ca.is_none()
            && self.vig.is_none()
            // 少于两点的分段表什么都描述不了，与 `Vig::new` 同口径当没有。
            && self.vig_curve.as_deref().is_none_or(|t| t.len() < 2)
    }

    /// 命令面与面板共用的 JSON 形状：查到的可以原样读回来。
    ///
    /// The JSON shape shared by the command surface and the panel: a found correction reads
    /// back verbatim.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "profile": self.profile,
            "lens": self.lens,
            "focal35_mm": self.focal35_mm,
            "dist": self.dist,
            "fisheye": self.fisheye,
            "ca": self.ca,
            "vignette": self.vig,
            "vignette_curve": self.vig_curve,
            "norm": self.norm,
        })
    }

    /// 应用到 sRGB `[0,1]` 像素。先逐像素找源坐标（畸变/色差），再按源半径补暗角。
    ///
    /// Applies to sRGB `[0,1]` pixels. First finds per-pixel source coordinates
    /// (distortion/CA), then adds vignetting by the source radius.
    pub fn apply(&self, rgb: &mut [f32], w: usize, h: usize) {
        if w == 0 || h == 0 || rgb.len() < w * h * 3 || self.is_empty() {
            return;
        }
        let maxdim = w.max(h) as f64;
        let fx = self.norm[0].max(1e-9) * maxdim;
        let fy = self.norm[1].max(1e-9) * maxdim;
        let cx = self.center[0] * w as f64;
        let cy = self.center[1] * h as f64;
        let vfx = self.vig_norm[0].max(1e-9) * maxdim;
        let vfy = self.vig_norm[1].max(1e-9) * maxdim;
        let vcx = self.vig_center[0] * w as f64;
        let vcy = self.vig_center[1] * h as f64;
        let vig = Vig::new(self, w, h, vfx, vfy, vcx, vcy);

        // 纯暗角不重采样：逐像素乘增益即可。
        if self.dist.is_none() && self.fisheye.is_none() && self.ca.is_none() {
            if matches!(vig, Vig::None) {
                return;
            }
            let vig = &vig;
            rgb.par_chunks_exact_mut(w * 3)
                .enumerate()
                .for_each(|(y, row)| {
                    let uy = (y as f64 - vcy) / vfy;
                    for (x, px) in row.chunks_exact_mut(3).enumerate() {
                        let ux = (x as f64 - vcx) / vfx;
                        let gain = vig.gain(ux * ux + uy * uy) as f32;
                        for v in px.iter_mut() {
                            *v = (*v * gain).clamp(0.0, 1.0);
                        }
                    }
                });
            return;
        }

        let src: Vec<f32> = rgb.to_vec();
        let src_ref = &src;
        let dist = self.dist;
        let fisheye = self.fisheye;
        let ca = self.ca;
        rgb.par_chunks_exact_mut(w * 3)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, px) in row.chunks_exact_mut(3).enumerate() {
                    let xd = (x as f64 - cx) / fx;
                    let yd = (y as f64 - cy) / fy;
                    // 校正图坐标 → 源图坐标。
                    let (mut xg, mut yg) = (xd, yd);
                    if let Some([k1, k2]) = fisheye {
                        let r = (xd * xd + yd * yd).sqrt();
                        if r > 1e-12 {
                            let th = r.atan();
                            let th2 = th * th;
                            let f = ((k2 * th2 + k1) * th2 + 1.0) * th / r;
                            xg = xd * f;
                            yg = yd * f;
                        }
                    } else if let Some([k1, k2, k3, p1, p2]) = dist {
                        let r2 = xd * xd + yd * yd;
                        let f = 1.0 + r2 * (k1 + r2 * (k2 + r2 * k3)) + 2.0 * (p2 * xd + p1 * yd);
                        xg = xd * f + p2 * r2;
                        yg = yd * f + p1 * r2;
                    }
                    let mut coords = [(xg, yg); 3];
                    if let Some([red, blue]) = ca {
                        let r2 = xg * xg + yg * yg;
                        coords[0] = channel_delta(&red, xg, yg, r2);
                        coords[2] = channel_delta(&blue, xg, yg, r2);
                    }
                    let gain = if matches!(vig, Vig::None) {
                        1.0
                    } else {
                        let ux = (xg * fx + cx - vcx) / vfx;
                        let uy = (yg * fy + cy - vcy) / vfy;
                        vig.gain(ux * ux + uy * uy) as f32
                    };
                    for (ch, px) in px.iter_mut().enumerate() {
                        let (sx, sy) = coords[ch];
                        let v = sample(src_ref, w, h, cx + sx * fx, cy + sy * fy, ch);
                        *px = (v * gain).clamp(0.0, 1.0);
                    }
                }
            });
    }
}

/// 暗角增益：分段表覆盖到画幅角时优先用它，否则退回多项式。
enum Vig {
    None,
    Poly([f64; 3]),
    Curve(CurveLut),
    /// 平表：`V` 处处相同（真实配置用它表示「这点光圈下没暗角」）。
    Flat(f64),
}

impl Vig {
    fn new(corr: &Correction, w: usize, h: usize, vfx: f64, vfy: f64, vcx: f64, vcy: f64) -> Self {
        // 少于两个点凑不出一条曲线：畸形表（空数组来自手改的工程 JSON 或
        // `grade.set`）当没有，退回多项式，别让一帧渲染 panic 掉整个进程。
        if let Some(table) = corr.vig_curve.as_deref().filter(|t| t.len() >= 2) {
            let flat = table.iter().all(|p| (p.1 - table[0].1).abs() < 1e-6);
            if flat {
                return Self::Flat(1.0 / table[0].1.clamp(0.05, 20.0));
            }
            let end = table[table.len() - 1].0;
            // 最远像素的归一化半径；留 10% 余量给比标定画幅更宽的比例（4:3 之类）。
            let corner = (((w as f64 - vcx).max(vcx) / vfx).powi(2)
                + ((h as f64 - vcy).max(vcy) / vfy).powi(2))
            .sqrt();
            if end >= 0.9 * corner {
                return Self::Curve(CurveLut::new(table, end));
            }
        }
        match corr.vig {
            Some(p) => Self::Poly(p),
            None => Self::None,
        }
    }

    fn gain(&self, r2: f64) -> f64 {
        match self {
            Self::None => 1.0,
            Self::Poly([a1, a2, a3]) => {
                let falloff = 1.0 + r2 * (a1 + r2 * (a2 + r2 * a3));
                1.0 / falloff.clamp(0.05, 20.0)
            }
            Self::Curve(lut) => lut.gain(r2),
            Self::Flat(g) => *g,
        }
    }
}

/// 分段表采成按 r² 等距的增益表：表在 r 上分段线性，1024 个采样点之后再按
/// r² 线性内插，误差远小于 1/255，逐像素只剩一次查表。
struct CurveLut {
    end2: f64,
    inv: Vec<f32>,
}

const LUT_N: usize = 1024;

impl CurveLut {
    fn new(table: &[(f64, f64)], end: f64) -> Self {
        let mut inv = Vec::with_capacity(LUT_N);
        for i in 0..LUT_N {
            let t = i as f64 / (LUT_N - 1) as f64;
            let r = end * t.sqrt();
            inv.push((1.0 / curve_value(table, r).clamp(0.05, 20.0)) as f32);
        }
        Self {
            end2: end * end,
            inv,
        }
    }

    fn gain(&self, r2: f64) -> f64 {
        let pos = (r2 / self.end2 * (LUT_N - 1) as f64).clamp(0.0, (LUT_N - 1) as f64);
        let i = pos as usize;
        let f = pos - i as f64;
        let a = f64::from(self.inv[i]);
        let b = f64::from(self.inv[(i + 1).min(LUT_N - 1)]);
        a + (b - a) * f
    }
}

/// 表内线性插值，两端按端点值平延。
fn curve_value(table: &[(f64, f64)], r: f64) -> f64 {
    let last = table[table.len() - 1];
    if r <= table[0].0 {
        return table[0].1;
    }
    if r >= last.0 {
        return last.1;
    }
    for pair in table.windows(2) {
        let (r0, v0) = pair[0];
        let (r1, v1) = pair[1];
        if r >= r0 && r <= r1 {
            let span = r1 - r0;
            return if span > 0.0 {
                v0 + (v1 - v0) * (r - r0) / span
            } else {
                v1
            };
        }
    }
    last.1
}

/// 红/蓝通道在绿通道坐标上的偏离（`scale` 与自己的多项式）。
fn channel_delta(m: &CaModel, xg: f64, yg: f64, r2: f64) -> (f64, f64) {
    let [k1, k2, k3, p1, p2] = m.param;
    let f = 1.0 + r2 * (k1 + r2 * (k2 + r2 * k3)) + 2.0 * (p2 * xg + p1 * yg);
    (m.scale * (xg * f + p2 * r2), m.scale * (yg * f + p1 * r2))
}

/// 双线性采样单个通道。越界的四个角都按边界钳位，边缘外重复边缘像素。
fn sample(src: &[f32], w: usize, h: usize, x: f64, y: f64, ch: usize) -> f32 {
    let x0 = x.floor() as isize;
    let y0 = y.floor() as isize;
    let dx = (x - x0 as f64) as f32;
    let dy = (y - y0 as f64) as f32;
    let cx0 = x0.clamp(0, w as isize - 1) as usize;
    let cx1 = (x0 + 1).clamp(0, w as isize - 1) as usize;
    let cy0 = y0.clamp(0, h as isize - 1) as usize;
    let cy1 = (y0 + 1).clamp(0, h as isize - 1) as usize;
    let w00 = (1.0 - dx) * (1.0 - dy);
    let w10 = dx * (1.0 - dy);
    let w01 = (1.0 - dx) * dy;
    let w11 = dx * dy;
    src[(cy0 * w + cx0) * 3 + ch] * w00
        + src[(cy0 * w + cx1) * 3 + ch] * w10
        + src[(cy1 * w + cx0) * 3 + ch] * w01
        + src[(cy1 * w + cx1) * 3 + ch] * w11
}

// ---------------------------------------------------------------------------
// 读取
// ---------------------------------------------------------------------------

/// 读并解析一份 `.lcp`。坏文件把原因带出来，不静默当成「没有配置」。
///
/// Reads and parses one `.lcp`. A bad file carries the reason out instead of being silently
/// treated as "no profile".
pub fn load(path: impl AsRef<Path>) -> Result<LcpFile, String> {
    let path = path.as_ref();
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("读 LCP 失败（{}）：{e}", path.display()))?;
    parse(&src, path)
}

/// 文本入口：测试与已经握在手里的 XMP 走这条。
///
/// The text entry point: tests and XMP already held in hand go through here.
pub fn parse(src: &str, path: &Path) -> Result<LcpFile, String> {
    let root = parse_xml(src).map_err(|e| format!("LCP 不是合法 XML：{e}"))?;
    let profiles = find_local(&root, "CameraProfiles")
        .ok_or_else(|| "LCP 里没有 photoshop:CameraProfiles".to_owned())?;
    let mut file = LcpFile {
        path: path.display().to_string(),
        ..LcpFile::default()
    };
    for li in find_all_local(profiles, "li") {
        let frame_node = li.child("rdf:Description").unwrap_or(li);
        let frame = frame_from(frame_node)?;
        if frame.base.is_none()
            && frame.fisheye.is_none()
            && frame.vig.is_none()
            && frame.focal_norm <= 0.0
        {
            continue;
        }
        if file.profile.is_empty() {
            file.profile = property(frame_node, "ProfileName").unwrap_or_default();
        }
        file.frames.push(frame);
    }
    if file.frames.is_empty() {
        return Err("LCP 里没有任何标定帧（rdf:li）".to_owned());
    }
    let mut lenses: Vec<String> = Vec::new();
    for f in &file.frames {
        if !f.lens.is_empty() && !lenses.contains(&f.lens) {
            lenses.push(f.lens.clone());
        }
    }
    file.lens = lenses.join(" / ");
    file.camera = file.frames[0].camera.clone();
    if file.profile.is_empty() {
        file.profile = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("LCP")
            .to_owned();
    }
    Ok(file)
}

fn frame_from(node: &Node) -> Result<Frame, String> {
    let camera = property(node, "Model")
        .or_else(|| property(node, "UniqueCameraModel"))
        .or_else(|| property(node, "CameraPrettyName"))
        .unwrap_or_default();
    let unique_camera = property(node, "UniqueCameraModel").unwrap_or_default();
    let lens = property(node, "Lens")
        .or_else(|| property(node, "LensPrettyName"))
        .unwrap_or_default();
    let sensor = property(node, "SensorFormatFactor")
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(1.0);
    let focal_mm = property(node, "FocalLength")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0);
    let focal35 = if focal_mm > 0.0 {
        focal_mm * sensor
    } else {
        0.0
    };
    let base = model_from(node, "PerspectiveModel");
    let fisheye = model_from(node, "FisheyeModel");
    // 色差模型在两种写法里都嵌在 PerspectiveModel 内，递归找。
    let ca_red = model_from(node, "ChromaticRedGreenModel");
    let ca_blue = model_from(node, "ChromaticBlueGreenModel");
    let ca_green = model_from(node, "ChromaticGreenModel");
    let vig = model_from(node, "VignetteModel");
    let focal_norm = base
        .as_ref()
        .and_then(|m| m.foc[0])
        .or_else(|| fisheye.as_ref().and_then(|m| m.foc[0]))
        .or_else(|| vig.as_ref().and_then(|m| m.foc[0]))
        .or_else(|| (focal35 > 0.0).then_some(focal35 / 35.0))
        .unwrap_or(0.0);
    // 色差必须有绿通道的坐标基准：有绿模型或基础畸变模型才算数。
    let ca = match (ca_red, ca_blue) {
        (Some(red), Some(blue)) if ca_green.is_some() || base.is_some() => [Some(red), Some(blue)],
        _ => [None, None],
    };
    Ok(Frame {
        camera,
        unique_camera,
        lens,
        sensor,
        focal35,
        focal_norm,
        aperture: property(node, "ApertureValue").and_then(|v| v.parse().ok()),
        focus: property(node, "FocusDistance").and_then(|v| v.parse().ok()),
        base,
        fisheye,
        ca,
        vig,
    })
}

/// 从节点属性或直接子元素取标量文本。两种在野写法都走这一条。
fn property(node: &Node, name: &str) -> Option<String> {
    if let Some(v) = node
        .attrs
        .iter()
        .find(|(k, _)| local(k) == name)
        .map(|(_, v)| v.trim().to_owned())
        .filter(|v| !v.is_empty())
    {
        return Some(v);
    }
    node.children
        .iter()
        .find(|c| local(&c.name) == name)
        .map(|c| c.text.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn model_from(node: &Node, name: &str) -> Option<Model> {
    let found = find_local(node, name)?;
    // 在野写法之三：`<PerspectiveModel><rdf:Description 参数…/></PerspectiveModel>`，
    // 参数不在模型节点上，而在它包着的 Description 上。
    let model_node = match found
        .children
        .iter()
        .find(|c| local(&c.name) == "Description")
    {
        Some(inner)
            if property(found, "FocalLengthX").is_none()
                && property(found, "RadialDistortParam1").is_none()
                && property(found, "VignetteModelParam1").is_none() =>
        {
            inner
        }
        _ => found,
    };
    let mut seen = false;
    let mut num = |key: &str| {
        let v = property(model_node, key).and_then(|v| v.parse::<f64>().ok());
        seen |= v.is_some();
        v
    };
    let mut m = Model::default();
    m.foc[0] = num("FocalLengthX");
    m.foc[1] = num("FocalLengthY");
    m.center[0] = num("ImageXCenter");
    m.center[1] = num("ImageYCenter");
    m.scale = num("ScaleFactor");
    // 暗角用 `VignetteModelParamN`，畸变/色差用 `RadialDistortParamN`；两套都认。
    for key in ["VignetteModelParam1", "RadialDistortParam1"] {
        if let Some(v) = num(key) {
            m.param[0] = v;
            break;
        }
    }
    for key in ["VignetteModelParam2", "RadialDistortParam2"] {
        if let Some(v) = num(key) {
            m.param[1] = v;
            break;
        }
    }
    for key in ["VignetteModelParam3", "RadialDistortParam3"] {
        if let Some(v) = num(key) {
            m.param[2] = v;
            break;
        }
    }
    for (slot, key) in [
        (3usize, "TangentialDistortParam1"),
        (4, "TangentialDistortParam2"),
    ] {
        if let Some(v) = num(key) {
            m.param[slot] = v;
        }
    }
    for (slot, key) in [(3usize, "RadialDistortParam4"), (4, "RadialDistortParam5")] {
        if m.param[slot] == 0.0
            && let Some(v) = num(key)
        {
            m.param[slot] = v;
        }
    }
    // 分段暗角表只挂在 VignetteModel 上：从外层节点找，别让它在 PerspectiveModel
    // 的递归里串到畸变模型头上。
    if name == "VignetteModel" {
        m.curve = curve_from(found);
        seen |= m.curve.is_some();
    }
    // 真实配置里常见空的 `<PerspectiveModel/>`（该帧只标了暗角）：它没有标定，
    // 不是「畸变为零」。当成 Some(零) 会在插值里把另一帧的真实畸变盖掉。
    seen.then_some(m)
}

/// `VignetteModelPiecewiseParam`：`<rdf:Seq><rdf:li>r, V</rdf:li>…`。
/// 只收正向、有限的点；按 r 排序并去重。`r` 与多项式同口径（`FocalLengthX ×
/// max(w, h)` 归一化的半径），点间按 r 线性插值。
fn curve_from(node: &Node) -> Option<Vec<(f64, f64)>> {
    let table = find_local(node, "VignetteModelPiecewiseParam")?;
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for li in find_all_local(table, "li") {
        let text = li.text.trim();
        let Some((r, v)) = text.split_once(',') else {
            continue;
        };
        let (Ok(r), Ok(v)) = (r.trim().parse::<f64>(), v.trim().parse::<f64>()) else {
            continue;
        };
        if r.is_finite() && v.is_finite() && r >= 0.0 && v > 0.0 {
            pts.push((r, v));
        }
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    // 不去重：平表（两个控制点半径相同、V 相同）是「这档没有暗角」的写法，
    // 丢掉重复点会把它误判成空表。
    (pts.len() >= 2).then_some(pts)
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn find_local<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    if local(&node.name) == name {
        return Some(node);
    }
    node.children.iter().find_map(|c| find_local(c, name))
}

fn find_all_local<'a>(node: &'a Node, name: &str) -> Vec<&'a Node> {
    let mut out = Vec::new();
    walk(node, name, &mut out);
    out
}

fn walk<'a>(node: &'a Node, name: &str, out: &mut Vec<&'a Node>) {
    if local(&node.name) == name {
        out.push(node);
        return;
    }
    for c in &node.children {
        walk(c, name, out);
    }
}

// ---------------------------------------------------------------------------
// 匹配
// ---------------------------------------------------------------------------

/// 按 EXIF 事实从整份 LCP 里折出一份校正。
///
/// Folds one correction out of the whole LCP file from the EXIF facts.
pub fn match_correction(file: &LcpFile, req: &Request) -> Result<Correction, String> {
    let mut candidates: Vec<&Frame> = file
        .frames
        .iter()
        .filter(|f| camera_matches(f, &req.make, &req.model))
        .collect();
    if candidates.is_empty() {
        return Err(format!(
            "配置是给 {} 的，与当前机身 {} 不一致",
            if file.camera.is_empty() {
                "未知机身"
            } else {
                &file.camera
            },
            camera_line(&req.make, &req.model)
        ));
    }
    let lens_hits: Vec<&Frame> = candidates
        .iter()
        .copied()
        .filter(|f| lens_matches(&f.lens, &req.lens))
        .collect();
    if !lens_hits.is_empty() {
        candidates = lens_hits;
    } else if !req.lens.is_empty() && file.lens.contains(" / ") {
        return Err(format!(
            "配置里的镜头（{}）与 EXIF（{}）不一致",
            file.lens, req.lens
        ));
    }

    let sensor = candidates
        .iter()
        .map(|f| f.sensor)
        .find(|v| *v > 0.0)
        .unwrap_or(1.0);
    let focal35 = req
        .focal35_mm
        .filter(|v| *v > 0.0)
        .unwrap_or(req.focal_mm * sensor);
    let target = if focal35 > 0.0 { focal35 / 35.0 } else { 0.0 };
    if target <= 0.0 {
        return Err("EXIF 里没有焦距，无法从配置里选帧".to_owned());
    }

    let mut usable: Vec<&Frame> = candidates
        .into_iter()
        .filter(|f| f.focal_key() > 0.0)
        .collect();
    if usable.is_empty() {
        return Err("配置里没有可用的焦距帧".to_owned());
    }
    usable.sort_by(|a, b| a.focal_key().total_cmp(&b.focal_key()));

    // 包住目标的两档焦距；超出范围就取端点那一档。
    let below = usable
        .iter()
        .rev()
        .find(|f| f.focal_key() <= target)
        .copied();
    let above = usable.iter().find(|f| f.focal_key() >= target).copied();
    let (low, high, t) = match (below, above) {
        (Some(a), Some(b)) if a.focal_key() < b.focal_key() => {
            let weight =
                (target.ln() - a.focal_key().ln()) / (b.focal_key().ln() - a.focal_key().ln());
            (a, b, weight.clamp(0.0, 1.0))
        }
        (Some(a), Some(b)) => (a, b, 0.5),
        (Some(a), None) => (a, a, 0.0),
        (None, Some(b)) => (b, b, 0.0),
        (None, None) => return Err("配置里没有可用的焦距帧".to_owned()),
    };

    // 同一档焦距常有多个对焦距离/光圈的帧：按 EXIF 光圈挑更接近的。
    let low = pick_at(usable.iter().copied(), low, req.aperture);
    let high = pick_at(usable.iter().copied(), high, req.aperture);
    let (low_key, high_key) = (low.focal_key(), high.focal_key());
    let at = |key: f64| {
        usable
            .iter()
            .copied()
            .filter(move |f| (f.focal_key() - key).abs() < 1e-9)
    };
    let bundle = if (low_key - high_key).abs() < 1e-9 {
        Bundle::group(at(low_key), req.aperture)
    } else {
        Bundle::merge(
            &Bundle::group(at(low_key), req.aperture),
            &Bundle::group(at(high_key), req.aperture),
            t,
        )
    };
    if bundle.is_empty() {
        return Err("配置里这一帧没有任何校正模型".to_owned());
    }
    let focal_shown = if low.focal35 > 0.0 && high.focal35 > 0.0 {
        low.focal35 + (high.focal35 - low.focal35) * t
    } else {
        focal35
    };
    Ok(Correction {
        lens: if low.lens.is_empty() {
            file.lens.clone()
        } else {
            low.lens.clone()
        },
        profile: file.profile.clone(),
        focal35_mm: focal_shown,
        norm: bundle.norm,
        center: bundle.center,
        dist: bundle.dist,
        fisheye: bundle.fisheye,
        ca: bundle.ca,
        vig: bundle.vig,
        vig_curve: bundle.vig_curve,
        vig_norm: bundle.vig_norm,
        vig_center: bundle.vig_center,
    })
}

/// 同一焦距档里挑光圈最接近的一帧。没有 Exif 光圈就用质量最好的那帧。
fn pick_at<'a>(
    frames: impl Iterator<Item = &'a Frame>,
    around: &'a Frame,
    aperture: Option<f64>,
) -> &'a Frame {
    let pool: Vec<&Frame> = frames
        .filter(|f| (f.focal_key() - around.focal_key()).abs() < 1e-9)
        .collect();
    if pool.is_empty() {
        return around;
    }
    match aperture {
        Some(want) => pool
            .into_iter()
            .min_by(|a, b| {
                let da = a.aperture.map_or(f64::INFINITY, |v| (v - want).abs());
                let db = b.aperture.map_or(f64::INFINITY, |v| (v - want).abs());
                da.total_cmp(&db)
                    .then(focus_rank(a).total_cmp(&focus_rank(b)))
            })
            .unwrap_or(around),
        None => around,
    }
}

/// 同光圈时的第二排序键。EXIF 里通常没有对焦距离，延时和风景又基本在无穷远，
/// 所以对焦距离越大越优先；两帧都没写时并列，保持文件顺序。
fn focus_rank(f: &Frame) -> f64 {
    -f.focus.unwrap_or(f64::INFINITY)
}

/// 折出来的系数集合，插值在它上面做。
#[derive(Clone, Debug, Default)]
struct Bundle {
    norm: [f64; 2],
    center: [f64; 2],
    dist: Option<[f64; 5]>,
    fisheye: Option<[f64; 2]>,
    ca: Option<[CaModel; 2]>,
    vig: Option<[f64; 3]>,
    vig_curve: Option<Vec<(f64, f64)>>,
    vig_norm: [f64; 2],
    vig_center: [f64; 2],
}

impl Bundle {
    fn is_empty(&self) -> bool {
        self.dist.is_none()
            && self.fisheye.is_none()
            && self.ca.is_none()
            && self.vig.is_none()
            && self.vig_curve.is_none()
    }

    /// 同一焦距档里可能有好几帧，而且常按模型拆开标：一帧有畸变、另一帧只有
    /// 暗角（空着 `<PerspectiveModel/>`）。畸变/色差按光圈、对焦距离挑，暗角
    /// 按光圈挑，然后把两边折进同一份，别让「这帧没标」变成「这项不校正」。
    fn group<'a>(frames: impl Iterator<Item = &'a Frame>, aperture: Option<f64>) -> Bundle {
        let pool: Vec<&Frame> = frames.collect();
        let rank = |f: &Frame| {
            let ap = match (f.aperture, aperture) {
                (Some(v), Some(w)) => (v - w).abs(),
                (Some(_), None) => 0.0,
                (None, _) => f64::INFINITY,
            };
            (ap, focus_rank(f))
        };
        let best = |pred: fn(&Frame) -> bool| {
            pool.iter().copied().filter(|f| pred(f)).min_by(|a, b| {
                let (aa, af) = rank(a);
                let (ba, bf) = rank(b);
                aa.total_cmp(&ba).then(af.total_cmp(&bf))
            })
        };
        let geo = best(|f| f.base.is_some() || f.fisheye.is_some());
        let vig = best(|f| f.vig.is_some());
        match (geo, vig) {
            (Some(g), Some(v)) if !std::ptr::eq(g, v) => {
                let mut out = bundle_of(g);
                let dark = bundle_of(v);
                out.vig = dark.vig;
                out.vig_curve = dark.vig_curve;
                out.vig_norm = dark.vig_norm;
                out.vig_center = dark.vig_center;
                out
            }
            (Some(g), _) => bundle_of(g),
            (None, Some(v)) => bundle_of(v),
            (None, None) => Bundle::default(),
        }
    }

    fn merge(a: &Bundle, b: &Bundle, t: f64) -> Bundle {
        let lerp = |x: f64, y: f64| x + (y - x) * t;
        let lerp2 = |x: [f64; 2], y: [f64; 2]| [lerp(x[0], y[0]), lerp(x[1], y[1])];
        let lerp5 = |x: [f64; 5], y: [f64; 5]| {
            let mut out = [0.0; 5];
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = lerp(x[i], y[i]);
            }
            out
        };
        // 两侧都有才插值；只有一侧有就整段沿用，别把「有」稀释成「没有」。
        // 真实配置里同一镜头的两帧经常各标一半（例如只标暗角的那帧空着
        // PerspectiveModel），丢掉另一侧会让整项校正凭空消失。
        Bundle {
            norm: lerp2(a.norm, b.norm),
            center: lerp2(a.center, b.center),
            dist: match (a.dist, b.dist) {
                (Some(x), Some(y)) => Some(lerp5(x, y)),
                _ => a.dist.or(b.dist),
            },
            // 畸变与鱼眼是两类模型，混着插值没有意义；一侧缺了就沿用另一侧。
            fisheye: match (a.fisheye, b.fisheye) {
                (Some(x), Some(y)) => Some([lerp(x[0], y[0]), lerp(x[1], y[1])]),
                _ => a.fisheye.or(b.fisheye),
            },
            ca: match (a.ca, b.ca) {
                (Some(x), Some(y)) => Some([lerp_ca(x[0], y[0], t), lerp_ca(x[1], y[1], t)]),
                _ => a.ca.or(b.ca),
            },
            vig: match (a.vig, b.vig) {
                (Some(x), Some(y)) => Some([lerp(x[0], y[0]), lerp(x[1], y[1]), lerp(x[2], y[2])]),
                _ => a.vig.or(b.vig),
            },
            vig_curve: merge_curves(a.vig_curve.as_deref(), b.vig_curve.as_deref(), t),
            vig_norm: lerp2(a.vig_norm, b.vig_norm),
            vig_center: lerp2(a.vig_center, b.vig_center),
        }
    }
}

/// 两张分段表按半径并集重采样后按 `t` 混合；只有一侧有就整段沿用。
fn merge_curves(
    a: Option<&[(f64, f64)]>,
    b: Option<&[(f64, f64)]>,
    t: f64,
) -> Option<Vec<(f64, f64)>> {
    let (x, y) = match (a, b) {
        (Some(x), Some(y)) => (x, y),
        _ => return a.or(b).map(|s| s.to_vec()),
    };
    let mut radii: Vec<f64> = x.iter().chain(y).map(|p| p.0).collect();
    radii.sort_by(|p, q| p.total_cmp(q));
    radii.dedup_by(|p, q| (*p - *q).abs() < 1e-9);
    let mut out = Vec::with_capacity(radii.len());
    for r in radii {
        let va = curve_value(x, r);
        let vb = curve_value(y, r);
        out.push((r, va + (vb - va) * t));
    }
    Some(out)
}

fn lerp_ca(a: CaModel, b: CaModel, t: f64) -> CaModel {
    let mut param = [0.0; 5];
    for (i, slot) in param.iter_mut().enumerate() {
        *slot = a.param[i] + (b.param[i] - a.param[i]) * t;
    }
    CaModel {
        scale: a.scale + (b.scale - a.scale) * t,
        param,
    }
}

fn bundle_of(f: &Frame) -> Bundle {
    let fallback = if f.focal_norm > 0.0 {
        f.focal_norm
    } else {
        f.focal35 / 35.0
    };
    let geo = f.base.as_ref().or(f.fisheye.as_ref());
    let norm = geo.map(|m| m.norm(fallback)).unwrap_or([fallback; 2]);
    let center = geo.map(|m| m.center()).unwrap_or([0.5, 0.5]);
    let dist = f.base.as_ref().map(|m| {
        let [p1, p2] = m.tangential();
        [m.param[0], m.param[1], m.param[2], p1, p2]
    });
    let fisheye = f.fisheye.as_ref().map(|m| [m.param[0], m.param[1]]);
    let ca = f.ca[0].as_ref().zip(f.ca[1].as_ref()).map(|(red, blue)| {
        [
            CaModel {
                scale: red.scale.unwrap_or(1.0),
                param: red.param,
            },
            CaModel {
                scale: blue.scale.unwrap_or(1.0),
                param: blue.param,
            },
        ]
    });
    let vig = f.vig.as_ref().map(|m| [m.param[0], m.param[1], m.param[2]]);
    let vig_curve = f
        .vig
        .as_ref()
        .and_then(|m| m.curve.as_deref())
        .filter(|c| c.len() >= 2)
        .map(|c| c.to_vec());
    let vig_norm = f.vig.as_ref().map(|m| m.norm(fallback)).unwrap_or(norm);
    let vig_center = f.vig.as_ref().map(|m| m.center()).unwrap_or(center);
    Bundle {
        norm,
        center,
        dist,
        fisheye,
        ca,
        vig,
        vig_curve,
        vig_norm,
        vig_center,
    }
}

fn norm_text(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn camera_matches(f: &Frame, make: &str, model: &str) -> bool {
    let cam = norm_text(&f.camera);
    let uniq = norm_text(&f.unique_camera);
    let model = norm_text(model);
    let make = norm_text(make);
    if model.is_empty() {
        return cam.is_empty() && uniq.is_empty();
    }
    if cam.is_empty() && uniq.is_empty() {
        return true;
    }
    [cam, uniq].iter().any(|p| {
        !p.is_empty()
            && (p == &model
                || p.contains(&model)
                || model.contains(p.as_str())
                || (!make.is_empty() && model.contains(make.as_str()) && p.contains(make.as_str())))
    })
}

fn lens_matches(profile: &str, lens: &str) -> bool {
    if profile.is_empty() || lens.is_empty() {
        return true;
    }
    let p = norm_text(profile);
    let l = norm_text(lens);
    if p.is_empty() || l.is_empty() {
        return true;
    }
    p == l || p.contains(&l) || l.contains(&p)
}

fn camera_line(make: &str, model: &str) -> String {
    if model.starts_with(make) || make.is_empty() {
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

/// 找 LCP 根目录：`AEVUM_LCP`（旧名 `LUMEN_LCP` 回退；目录或单个 `.lcp`）
/// → Adobe Camera Raw 的默认位置。
///
/// Finds the LCP root directory: `AEVUM_LCP` (legacy name `LUMEN_LCP` falls back; a
/// directory or a single `.lcp`), then the default Adobe Camera Raw location.
pub fn default_directory() -> Option<PathBuf> {
    if let Some(dir) = crate::env::var_os("LCP") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Some(path);
        }
        if path.is_file() {
            return path.parent().map(Path::to_path_buf);
        }
    }
    let adobe = Path::new("C:/ProgramData/Adobe/CameraRaw/LensProfiles/1.0");
    adobe.is_dir().then(|| adobe.to_path_buf())
}

/// 在默认目录里按 EXIF 自动找一份配置并匹配。
///
/// Automatically finds and matches a profile in the default directory from the EXIF facts.
///
/// 先按文件名粗筛（3633 份文件逐个解析要几秒），只解析镜头名能对上的前几份，
/// 再走 [`match_correction`] 的完整校验。
///
/// First a coarse filter by file name (parsing all 3633 files takes seconds), then only the
/// first few whose lens names match get parsed, followed by the full validation of
/// [`match_correction`].
pub fn auto_match(req: &Request) -> Result<(PathBuf, Correction), String> {
    let dir = default_directory().ok_or_else(|| {
        "没找到 LCP 目录（设 AEVUM_LCP，或安装 Adobe Camera Raw 的镜头配置）".to_owned()
    })?;
    let files = collect_lcp(&dir);
    if files.is_empty() {
        return Err(format!("{} 下没有 .lcp 文件", dir.display()));
    }
    let lens = norm_text(&req.lens);
    let mut ranked: Vec<(usize, PathBuf)> = files
        .into_iter()
        .filter_map(|path| {
            let name = norm_text(&path.file_name()?.to_string_lossy());
            let hit = !lens.is_empty() && name.contains(&lens);
            hit.then_some((name.len(), path))
        })
        .collect();
    ranked.sort_by_key(|(len, _)| *len);
    if ranked.is_empty() {
        return Err(if req.lens.is_empty() {
            "EXIF 里没有镜头型号，无法自动挑配置".to_owned()
        } else {
            format!("{} 里没有与 {} 对得上的镜头配置", dir.display(), req.lens)
        });
    }
    let mut last = String::new();
    for (_, path) in ranked.into_iter().take(8) {
        let Ok(file) = load(&path) else {
            continue;
        };
        match match_correction(&file, req) {
            Ok(corr) => return Ok((path, corr)),
            Err(e) => last = e,
        }
    }
    Err(if last.is_empty() {
        "候选配置都解析失败".to_owned()
    } else {
        last
    })
}

fn collect_lcp(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("lcp"))
            {
                out.push(path);
            }
            if out.len() > 50_000 {
                return out;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 测试：两种在野写法、选帧插值、暗角与采样回归
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ATTR_FIXTURE: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">
   <photoshop:CameraProfiles>
    <rdf:Seq>
     <rdf:li>
      <rdf:Description
       stCamera:Make="Test" stCamera:Model="Test Camera" stCamera:UniqueCameraModel="Test Camera"
       stCamera:Lens="Test 24-70mm F2.8" stCamera:ProfileName="Test Profile"
       stCamera:SensorFormatFactor="1" stCamera:FocalLength="24" stCamera:ApertureValue="8">
       <stCamera:PerspectiveModel
        stCamera:FocalLengthX="0.685714" stCamera:FocalLengthY="0.685714"
        stCamera:ImageXCenter="0.5" stCamera:ImageYCenter="0.5"
        stCamera:RadialDistortParam1="-0.1" stCamera:RadialDistortParam2="0.05"
        stCamera:RadialDistortParam3="-0.01"/>
       <stCamera:VignetteModel
        stCamera:FocalLengthX="0.685714" stCamera:FocalLengthY="0.685714"
        stCamera:VignetteModelParam1="-0.3" stCamera:VignetteModelParam2="0.1"
        stCamera:VignetteModelParam3="0.01"/>
      </rdf:Description>
     </rdf:li>
     <rdf:li>
      <rdf:Description
       stCamera:Make="Test" stCamera:Model="Test Camera" stCamera:Lens="Test 24-70mm F2.8"
       stCamera:ProfileName="Test Profile" stCamera:SensorFormatFactor="1"
       stCamera:FocalLength="70" stCamera:ApertureValue="8">
       <stCamera:PerspectiveModel
        stCamera:FocalLengthX="2" stCamera:FocalLengthY="2"
        stCamera:ImageXCenter="0.5" stCamera:ImageYCenter="0.5"
        stCamera:RadialDistortParam1="-0.2" stCamera:RadialDistortParam2="0.08"
        stCamera:RadialDistortParam3="-0.02"/>
      </rdf:Description>
     </rdf:li>
    </rdf:Seq>
   </photoshop:CameraProfiles>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    const ELEMENT_FIXTURE: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="">
   <photoshop:CameraProfiles xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">
    <rdf:Seq>
     <rdf:li rdf:parseType="Resource">
      <stCamera:Model xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">Phone 3G</stCamera:Model>
      <stCamera:Lens xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">Phone lens</stCamera:Lens>
      <stCamera:ProfileName xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">Phone profile</stCamera:ProfileName>
      <stCamera:FocusDistance xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">1.27</stCamera:FocusDistance>
      <stCamera:PerspectiveModel xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile" rdf:parseType="Resource">
       <stCamera:Version>2</stCamera:Version>
       <stCamera:FocalLengthX>0.715735</stCamera:FocalLengthX>
       <stCamera:FocalLengthY>0.715735</stCamera:FocalLengthY>
       <stCamera:ImageXCenter>0.493115</stCamera:ImageXCenter>
       <stCamera:ImageYCenter>0.466536</stCamera:ImageYCenter>
       <stCamera:RadialDistortParam1>-0.016126</stCamera:RadialDistortParam1>
       <stCamera:ChromaticGreenModel rdf:parseType="Resource">
        <stCamera:FocalLengthX>0.708083</stCamera:FocalLengthX>
        <stCamera:RadialDistortParam1>-0.015988</stCamera:RadialDistortParam1>
       </stCamera:ChromaticGreenModel>
       <stCamera:ChromaticRedGreenModel rdf:parseType="Resource">
        <stCamera:ScaleFactor>1.000373</stCamera:ScaleFactor>
        <stCamera:RadialDistortParam1>-0.000144</stCamera:RadialDistortParam1>
       </stCamera:ChromaticRedGreenModel>
       <stCamera:ChromaticBlueGreenModel rdf:parseType="Resource">
        <stCamera:ScaleFactor>1.000183</stCamera:ScaleFactor>
        <stCamera:RadialDistortParam1>0.000403</stCamera:RadialDistortParam1>
       </stCamera:ChromaticBlueGreenModel>
      </stCamera:PerspectiveModel>
      <stCamera:VignetteModel rdf:parseType="Resource">
       <stCamera:FocalLengthX>0.715735</stCamera:FocalLengthX>
       <stCamera:ImageXCenter>0.493115</stCamera:ImageXCenter>
       <stCamera:VignetteModelParam1>-0.168303</stCamera:VignetteModelParam1>
      </stCamera:VignetteModel>
     </rdf:li>
    </rdf:Seq>
   </photoshop:CameraProfiles>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    fn parse_fixture(src: &str) -> LcpFile {
        parse(src, Path::new("fixture.lcp")).expect("fixture 应能解析")
    }

    fn request(focal35: f64) -> Request {
        Request {
            make: "Test".to_owned(),
            model: "Test Camera".to_owned(),
            lens: "Test 24-70mm F2.8".to_owned(),
            focal_mm: focal35,
            focal35_mm: Some(focal35),
            aperture: Some(8.0),
        }
    }

    #[test]
    fn parses_attribute_form_with_two_focal_frames() {
        let file = parse_fixture(ATTR_FIXTURE);
        assert_eq!(file.frame_count(), 2);
        assert_eq!(file.camera, "Test Camera");
        assert_eq!(file.lens, "Test 24-70mm F2.8");
        assert_eq!(file.profile, "Test Profile");
        assert!((file.frames[0].focal_norm - 24.0 / 35.0).abs() < 1e-6);
    }

    #[test]
    fn matches_exact_focal_and_interpolates_between_frames() {
        let file = parse_fixture(ATTR_FIXTURE);
        let corr = match_correction(&file, &request(24.0)).unwrap();
        let dist = corr.dist.expect("24mm 有畸变");
        assert!((dist[0] + 0.1).abs() < 1e-9);
        assert!((corr.vig.unwrap()[0] + 0.3).abs() < 1e-9);

        let mid = match_correction(&file, &request(47.0)).unwrap();
        let k1 = mid.dist.expect("插值后仍有畸变")[0];
        assert!(
            (-0.2..-0.1).contains(&k1),
            "47mm 的 k1 应落在两帧之间，得到 {k1}"
        );
    }

    #[test]
    fn parses_element_form_and_nested_ca_models() {
        let file = parse_fixture(ELEMENT_FIXTURE);
        assert_eq!(file.frame_count(), 1);
        let frame = &file.frames[0];
        assert_eq!(frame.camera, "Phone 3G");
        assert_eq!(frame.lens, "Phone lens");
        assert!((frame.focal_norm - 0.715735).abs() < 1e-9);
        assert!((frame.focus.unwrap() - 1.27).abs() < 1e-9);
        assert!(frame.vig.is_some(), "暗角模型应在帧下找到");
        let red = frame.ca[0]
            .as_ref()
            .expect("红色差模型在 PerspectiveModel 里");
        assert!((red.scale.unwrap() - 1.000373).abs() < 1e-9);

        let req = Request {
            make: "Apple".to_owned(),
            model: "Phone 3G".to_owned(),
            lens: "Phone lens".to_owned(),
            focal_mm: 3.85,
            focal35_mm: None,
            aperture: None,
        };
        let corr = match_correction(&file, &req).unwrap();
        let ca = corr.ca.expect("折出来的校正要带色差");
        assert!((ca[0].scale - 1.000373).abs() < 1e-9);
        assert!((ca[1].param[0] - 0.000403).abs() < 1e-9);
        assert!((corr.dist.unwrap()[0] + 0.016126).abs() < 1e-9);
    }

    #[test]
    fn rejects_other_camera() {
        let file = parse_fixture(ATTR_FIXTURE);
        let mut req = request(24.0);
        req.model = "Some Other Body".to_owned();
        let err = match_correction(&file, &req).unwrap_err();
        assert!(err.contains("不一致"), "{err}");
    }

    #[test]
    fn vignette_only_brightens_corners() {
        let mut rgb = vec![0.5f32; 4 * 4 * 3];
        let corr = Correction {
            vig: Some([-0.5, 0.0, 0.0]),
            vig_norm: [0.5, 0.5],
            vig_center: [0.5, 0.5],
            ..Correction::default()
        };
        corr.apply(&mut rgb, 4, 4);
        let center = &rgb[(2 * 4 + 2) * 3..][..3];
        let corner = &rgb[0..3];
        assert!((center[0] - 0.5).abs() < 1e-6, "中心不该被改：{center:?}");
        assert!(corner[0] > 0.5, "角落要提亮：{corner:?}");
    }

    #[test]
    fn resampling_keeps_each_channel_separate() {
        // 采样曾把每像素第 0 个分量当三个通道用，整幅图会褪成暖灰。
        let mut rgb = vec![0.0f32; 8 * 8 * 3];
        for px in rgb.chunks_exact_mut(3) {
            px.copy_from_slice(&[0.8, 0.4, 0.2]);
        }
        let corr = Correction {
            dist: Some([-0.05, 0.0, 0.0, 0.0, 0.0]),
            norm: [0.5, 0.5],
            center: [0.5, 0.5],
            vig_norm: [0.5, 0.5],
            vig_center: [0.5, 0.5],
            ..Correction::default()
        };
        corr.apply(&mut rgb, 8, 8);
        for px in rgb.chunks_exact(3) {
            assert!((px[0] - 0.8).abs() < 1e-5, "{px:?}");
            assert!((px[1] - 0.4).abs() < 1e-5, "{px:?}");
            assert!((px[2] - 0.2).abs() < 1e-5, "{px:?}");
        }
    }

    #[test]
    fn model_inside_wrapped_description() {
        // 第三种在野写法：参数挂在内层 rdf:Description 上。
        let src = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="">
   <photoshop:CameraProfiles xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">
    <rdf:Seq>
     <rdf:li>
      <rdf:Description
       stCamera:Make="Leica Camera AG" stCamera:Model="Leica M9"
       stCamera:Lens="28mm f/1.9" stCamera:LensPrettyName="Voigtlander 28mm"
       stCamera:ProfileName="Wrapped" stCamera:SensorFormatFactor="1"
       stCamera:FocalLength="28"
       xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">
       <stCamera:PerspectiveModel>
        <rdf:Description
         stCamera:FocalLengthX="0.8" stCamera:FocalLengthY="0.8"
         stCamera:ImageXCenter="0.502" stCamera:ImageYCenter="0.501"
         stCamera:RadialDistortParam1="-0.017111"
         stCamera:RadialDistortParam2="0.014556"/>
       </stCamera:PerspectiveModel>
      </rdf:Description>
     </rdf:li>
    </rdf:Seq>
   </photoshop:CameraProfiles>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        let file = parse_fixture(src);
        assert_eq!(file.frame_count(), 1);
        let req = Request {
            make: "Leica Camera AG".to_owned(),
            model: "Leica M9".to_owned(),
            lens: "28mm f/1.9".to_owned(),
            focal_mm: 28.0,
            focal35_mm: None,
            aperture: None,
        };
        let corr = match_correction(&file, &req).unwrap();
        let dist = corr.dist.expect("内层 Description 的系数要读到");
        assert!((dist[0] + 0.017111).abs() < 1e-9, "{dist:?}");
        assert!((corr.norm[0] - 0.8).abs() < 1e-9);
    }

    #[test]
    fn split_frames_keep_distortion_and_vignette() {
        // 真实配置的常见形态：同焦距两帧，一帧只标畸变，另一帧是空的
        // `<PerspectiveModel/>` 加暗角。空模型不能当成「零畸变」，
        // 合并后两项都得在。
        let src = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="">
   <photoshop:CameraProfiles xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">
    <rdf:Seq>
     <rdf:li>
      <rdf:Description
       stCamera:Make="Test" stCamera:Model="Test Camera"
       stCamera:Lens="Test 24-70mm F2.8" stCamera:ProfileName="Split"
       stCamera:SensorFormatFactor="1" stCamera:FocalLength="24" stCamera:FocusDistance="0.5"
       xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">
       <stCamera:PerspectiveModel
        stCamera:FocalLengthX="0.685714" stCamera:FocalLengthY="0.685714"
        stCamera:ImageXCenter="0.5" stCamera:ImageYCenter="0.5"
        stCamera:RadialDistortParam1="-0.12"/>
      </rdf:Description>
     </rdf:li>
     <rdf:li>
      <rdf:Description
       stCamera:Make="Test" stCamera:Model="Test Camera"
       stCamera:Lens="Test 24-70mm F2.8" stCamera:ProfileName="Split"
       stCamera:SensorFormatFactor="1" stCamera:FocalLength="24"
       stCamera:FocusDistance="5" stCamera:ApertureValue="8"
       xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">
       <stCamera:PerspectiveModel/>
       <stCamera:VignetteModel
        stCamera:FocalLengthX="0.685714" stCamera:FocalLengthY="0.685714"
        stCamera:VignetteModelParam1="-0.4"
        stCamera:VignetteModelParam2="0.1"/>
      </rdf:Description>
     </rdf:li>
    </rdf:Seq>
   </photoshop:CameraProfiles>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        let file = parse_fixture(src);
        assert_eq!(file.frame_count(), 2);
        let mut req = request(24.0);
        req.aperture = Some(8.0);
        let corr = match_correction(&file, &req).unwrap();
        let dist = corr.dist.expect("第一帧的畸变不能被空模型盖掉");
        assert!((dist[0] + 0.12).abs() < 1e-9, "{dist:?}");
        let vig = corr.vig.expect("第二帧的暗角要带上");
        assert!((vig[0] + 0.4).abs() < 1e-9, "{vig:?}");
    }

    const PIECEWISE_FIXTURE: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="">
   <photoshop:CameraProfiles xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">
    <rdf:Seq>
     <rdf:li>
      <rdf:Description
       stCamera:Make="Test" stCamera:Model="Test Camera" stCamera:Lens="Test 24-70mm F2.8"
       stCamera:ProfileName="Piecewise" stCamera:SensorFormatFactor="1"
       stCamera:FocalLength="24"
       xmlns:stCamera="http://ns.adobe.com/photoshop/1.0/camera-profile">
       <stCamera:PerspectiveModel>
        <rdf:Description
         stCamera:FocalLengthX="0.5" stCamera:FocalLengthY="0.5"
         stCamera:RadialDistortParam1="0">
        <stCamera:VignetteModel>
         <rdf:Description
          stCamera:FocalLengthX="0.5" stCamera:FocalLengthY="0.5"
          stCamera:VignetteModelParam1="-0.1">
          <stCamera:VignetteModelPiecewiseParam>
           <rdf:Seq>
            <rdf:li>0.000000, 1.000000</rdf:li>
            <rdf:li>1.000000, 0.800000</rdf:li>
            <rdf:li>1.500000, 0.500000</rdf:li>
           </rdf:Seq>
          </stCamera:VignetteModelPiecewiseParam>
         </rdf:Description>
        </stCamera:VignetteModel>
        </rdf:Description>
       </stCamera:PerspectiveModel>
      </rdf:Description>
     </rdf:li>
    </rdf:Seq>
   </photoshop:CameraProfiles>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn piecewise_vignette_table_is_parsed_and_preferred() {
        let file = parse_fixture(PIECEWISE_FIXTURE);
        let corr = match_correction(&file, &request(24.0)).unwrap();
        let curve = corr.vig_curve.as_deref().expect("分段表要读进来");
        assert_eq!(curve.len(), 3);
        assert!((curve[2].1 - 0.5).abs() < 1e-9, "{curve:?}");
        let json = corr.to_json();
        assert_eq!(json["vignette_curve"][2][1], 0.5);

        // 8×8、vig_norm 0.5 → 角上 r = √2，落在 1.0→1.5 段。
        let mut rgb = vec![0.5f32; 8 * 8 * 3];
        corr.apply(&mut rgb, 8, 8);
        let v = 0.8 - 0.3 * ((2f64).sqrt() - 1.0) / 0.5;
        let want = 0.5 / v;
        assert!(
            (f64::from(rgb[0]) - want).abs() < 0.01,
            "角上 {}, 期望 {want}（多项式只给 0.625）",
            rgb[0]
        );
        let center = rgb[(4 * 8 + 4) * 3];
        assert!((center - 0.5).abs() < 1e-6, "中心不该被改：{center}");
    }

    #[test]
    fn piecewise_table_that_misses_the_corner_falls_back_to_polynomial() {
        // 表只管到 r=0.2，画幅角 r=√2：别拿没标过的半径硬外推。
        let corr = Correction {
            vig: Some([-0.2, 0.0, 0.0]),
            vig_curve: Some(vec![(0.0, 1.0), (0.2, 0.5)]),
            vig_norm: [0.5, 0.5],
            vig_center: [0.5, 0.5],
            ..Correction::default()
        };
        let mut rgb = vec![0.5f32; 8 * 8 * 3];
        corr.apply(&mut rgb, 8, 8);
        // 多项式在 r²=2 处衰减 0.6，增益 1/0.6。
        let want = 0.5f32 / 0.6;
        assert!((rgb[0] - want).abs() < 1e-3, "{} != {want}", rgb[0]);
    }

    #[test]
    fn flat_piecewise_table_means_no_vignetting() {
        // 真实配置用平表表示「这档光圈没测到暗角」，这时不该退回多项式。
        let corr = Correction {
            vig: Some([-0.5, 0.0, 0.0]),
            vig_curve: Some(vec![(0.000694, 1.0), (0.000694, 1.0)]),
            vig_norm: [0.5, 0.5],
            vig_center: [0.5, 0.5],
            ..Correction::default()
        };
        let mut rgb = vec![0.5f32; 8 * 8 * 3];
        corr.apply(&mut rgb, 8, 8);
        assert!(rgb.iter().all(|v| (v - 0.5).abs() < 1e-6), "{rgb:?}");
    }

    /// 畸形分段表（空数组、单点）能从工程 JSON 或 `grade.set` 原样反序列化进来。
    /// 以前 `Vig::new` 直接摸 `table[0]` 与末点，一帧渲染就能 panic 掉整个进程。
    #[test]
    fn malformed_curve_tables_degrade_instead_of_panicking() {
        let corr: Correction = serde_json::from_value(serde_json::json!({
            "lens": ".SIGMA 30mm F1.4",
            "profile": "sample",
            "focal35_mm": 30.0,
            "norm": [0.5, 0.5],
            "center": [0.5, 0.5],
            "dist": null,
            "fisheye": null,
            "ca": null,
            "vig": [-0.2, 0.0, 0.0],
            "vig_curve": [],
            "vig_norm": [0.5, 0.5],
            "vig_center": [0.5, 0.5],
        }))
        .expect("空表也要读得进来");
        assert!(!corr.is_empty(), "同份配置还有多项式暗角，不算空");
        let mut rgb = vec![0.5f32; 8 * 8 * 3];
        corr.apply(&mut rgb, 8, 8);
        // 空表当没有：继续走多项式，r²=2 处衰减 0.6。
        let want = 0.5f32 / 0.6;
        assert!(
            (rgb[0] - want).abs() < 1e-3,
            "空表要退回多项式：{} != {want}",
            rgb[0]
        );

        // 只有一个点描述不出曲线：整份配置就当没校正。
        let one = Correction {
            vig: None,
            vig_curve: Some(vec![(0.3, 0.7)]),
            ..Correction::default()
        };
        assert!(one.is_empty(), "单点表不是有效的暗角曲线");
        let mut rgb2 = vec![0.5f32; 8 * 8 * 3];
        one.apply(&mut rgb2, 8, 8);
        assert!(rgb2.iter().all(|v| (v - 0.5).abs() < 1e-6), "{rgb2:?}");
    }

    #[test]
    fn merged_focal_frames_blend_their_tables() {
        let a = Bundle {
            vig_curve: Some(vec![(0.0, 1.0), (1.0, 0.8)]),
            ..Bundle::default()
        };
        let b = Bundle {
            vig_curve: Some(vec![(0.0, 1.0), (0.5, 0.9), (1.0, 0.4)]),
            ..Bundle::default()
        };
        let m = Bundle::merge(&a, &b, 0.5).vig_curve.expect("两侧都有表");
        assert_eq!(m.len(), 3, "半径并集要保留两侧的控制点：{m:?}");
        assert!((m[1].1 - 0.9).abs() < 1e-12, "{m:?}"); // a 在 0.5 处是 0.9
        assert!((m[2].1 - 0.6).abs() < 1e-12, "{m:?}");
        // 只有一侧有表时整段沿用。
        let one = Bundle::merge(&a, &Bundle::default(), 0.5);
        assert_eq!(one.vig_curve.unwrap(), vec![(0.0, 1.0), (1.0, 0.8)]);
    }
}
