//! 查表类显影运算：色彩分级、HSL、单色混合、色阶/参数曲线/曲线合成表。
//!
//! 全部是逐点或按亮度索引的查表运算，没有邻域访问，因此可以在草图与
//! `for_lut` 路径上照跑不误。语义严格照抄 Python `acceptance/reference/grade.py`：
//! `_grade_table` / `_color_grade`、`_hsl_tables` / `_core` 的 HSL 段、
//! `_mono_table` / `_mono`、`_levels_lut` / `_par_lut` / `_curve_lut`。
//!
//! 数值口径：全程 float32，与 numpy 一致；`np.rint` 的"四舍六入五成双"用
//! [`rint`] 复现。RGB↔HSV 按 OpenCV float 约定（H 0~360、S/V 0~1，扇区
//! R→G→B）。曲线类合成一张 65536 级 uint16 表、量化输入一次查完。
//!
//! Lookup-table develop operations: color grading, HSL, monochrome mixing, and the
//! levels / parametric curve / curve composition tables.
//!
//! Every operation is per-pixel or indexed by luminance, with no neighborhood access, so the
//! draft and `for_lut` paths run them unchanged. Semantics copy the Python
//! `acceptance/reference/grade.py` routines: `_grade_table` / `_color_grade`, the HSL sections
//! of `_hsl_tables` / `_core`, `_mono_table` / `_mono`,
//! `_levels_lut` / `_par_lut` / `_curve_lut`.
//!
//! Numeric convention: float32 throughout, matching numpy; the "round half to even" of
//! `np.rint` is reproduced by [`rint`]. RGB↔HSV follows the OpenCV float convention
//! (H 0~360, S/V 0~1, sector order R→G→B). Curve-class operations compose one 65536-entry
//! uint16 table and quantize the input once per lookup.

use rayon::prelude::*;

use super::{LUMA, MIN_PIXELS};
use crate::grade::{GradeParams, pchip_lut};

/// 色相分箱数（2 的幂，索引可 `& (BINS-1)` 取模）。
const HUE_BINS: usize = 65536;
/// 8 个色相带中心（度）。
const HSL_CENTERS: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 285.0, 330.0];
/// 曲线合成表的级数（16bit）。
const CURVE_N: usize = 65536;
/// 参数曲线 ±100 最多推 18% 亮度。
const PAR_SCALE: f32 = 0.18;
/// 色彩分级亮度表级数。
const GRADE_N: usize = 1024;

/// Python `_smooth`（float32 版）。
#[inline]
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Python `np.rint`：就近取整，半数取偶。这里输入恒为非负。
#[inline]
fn rint(x: f32) -> f32 {
    let f = x.floor();
    let diff = x - f;
    // 只有正好差半个单位时才看 f 的奇偶，其余按就近取整。
    if diff > 0.5 || (diff == 0.5 && f % 2.0 != 0.0) {
        f + 1.0
    } else {
        f
    }
}

/// 色相到分箱索引：`(h * (BINS/360)) as int32 & (BINS-1)`。
#[inline]
fn hue_idx(h: f32) -> usize {
    let scale = (HUE_BINS as f32) / 360.0;
    ((h * scale) as i32) as usize & (HUE_BINS - 1)
}

/// Python `_HUE_AX` 上的色相环距离：`abs((h - center + 180) % 360 - 180)`。
#[inline]
fn hue_dist(h: f32, center: f32) -> f32 {
    let m = (h - center + 180.0).rem_euclid(360.0);
    (m - 180.0).abs()
}

/// OpenCV float32 的 RGB→HSV（H 0~360、S/V 0~1），扇区判序 R→G→B。
#[inline]
fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let cmax = r.max(g).max(b);
    let cmin = r.min(g).min(b);
    let delta = cmax - cmin;
    let v = cmax;
    let s = if cmax != 0.0 { delta / cmax } else { 0.0 };
    let h = if delta == 0.0 {
        0.0
    } else if cmax == r {
        let x = 60.0 * ((g - b) / delta);
        if x < 0.0 { x + 360.0 } else { x }
    } else if cmax == g {
        120.0 + 60.0 * ((b - r) / delta)
    } else {
        240.0 + 60.0 * ((r - g) / delta)
    };
    (h, s, v)
}

/// OpenCV float32 的 HSV→RGB。与 [`rgb_to_hsv`] 互逆（同一套扇区约定）。
#[inline]
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let hi = (h / 60.0).floor();
    let f = h / 60.0 - hi;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match hi as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// 色相 -> 零和 RGB 偏移方向（Python `_hue_vec`）。
fn hue_vec(h: f64) -> [f32; 3] {
    let a = h.to_radians();
    let mut v = [
        a.cos() as f32,
        (a - 2.094395).cos() as f32,
        (a - 4.188790).cos() as f32,
    ];
    let dot = v[0] * LUMA[0] + v[1] * LUMA[1] + v[2] * LUMA[2];
    let norm = LUMA[0] * LUMA[0] + LUMA[1] * LUMA[1] + LUMA[2] * LUMA[2];
    let k = dot / norm;
    for c in 0..3 {
        v[c] -= LUMA[c] * k;
    }
    v
}

// ===================== 色彩分级 =====================

fn grade_active(p: &GradeParams) -> bool {
    p.grade_sh_s != 0.0
        || p.grade_mid_s != 0.0
        || p.grade_hi_s != 0.0
        || p.grade_global_s != 0.0
        || p.grade_sh_l != 0.0
        || p.grade_mid_l != 0.0
        || p.grade_hi_l != 0.0
        || p.grade_global_l != 0.0
}

/// Python `_grade_table`：亮度 1024 级 -> 每级 RGB 偏移（行主序展平）。
fn grade_table(p: &GradeParams) -> Vec<[f32; 3]> {
    let a = (0.46 - p.grade_balance / 100.0 * 0.12).clamp(0.18, 0.78) as f32;
    let bl = (p.grade_blending).clamp(0.0, 100.0) as f32 / 100.0;

    let sh_rgb = [p.grade_sh_h, p.grade_mid_h, p.grade_hi_h, p.grade_global_h];
    let sh_s = [p.grade_sh_s, p.grade_mid_s, p.grade_hi_s, p.grade_global_s];
    let l_off = [p.grade_sh_l, p.grade_mid_l, p.grade_hi_l, p.grade_global_l];
    // 预乘 (s/100*0.18) 的色相向量；全局轮单独处理（不分亮度）。
    let v_sh = hue_vec(sh_rgb[0]);
    let v_mid = hue_vec(sh_rgb[1]);
    let v_hi = hue_vec(sh_rgb[2]);
    let k_sh = (p.grade_sh_s / 100.0 * 0.18) as f32;
    let k_mid = (p.grade_mid_s / 100.0 * 0.18) as f32;
    let k_hi = (p.grade_hi_s / 100.0 * 0.18) as f32;
    let gl_vec = hue_vec(sh_rgb[3]);
    let gl_k = (sh_s[3] / 100.0 * 0.18) as f32;

    let lo_sh = (l_off[0] / 100.0 * 0.15) as f32;
    let lo_mid = (l_off[1] / 100.0 * 0.15) as f32;
    let lo_hi = (l_off[2] / 100.0 * 0.15) as f32;
    let lo_gl = (l_off[3] / 100.0 * 0.15) as f32;
    let any_l = l_off[0] != 0.0 || l_off[1] != 0.0 || l_off[2] != 0.0 || l_off[3] != 0.0;

    let hi_e0 = (a * 1.15).min(0.95);
    let hi_e1 = (a * 2.0).min(1.0);
    let lo_e0 = a * 0.45;

    let mut out = vec![[0.0f32; 3]; GRADE_N];
    for (row, l) in out.iter_mut().enumerate() {
        let lum = (row as f64 / (GRADE_N - 1) as f64) as f32;
        let mut w_sh = 1.0 - smooth(lo_e0, a, lum);
        let mut w_hi = smooth(hi_e0, hi_e1, lum);
        let mut w_mid = (1.0 - w_sh - w_hi).clamp(0.0, 1.0);
        if bl > 0.0 {
            w_sh = w_sh * (1.0 - bl) + bl * (1.0 - lum);
            w_hi = w_hi * (1.0 - bl) + bl * lum;
            w_mid = (1.0 - w_sh - w_hi).clamp(0.0, 1.0);
        }
        let mut acc = [0.0f32; 3];
        for c in 0..3 {
            acc[c] = w_sh * (v_sh[c] * k_sh) + w_mid * (v_mid[c] * k_mid) + w_hi * (v_hi[c] * k_hi);
            if sh_s[3] != 0.0 {
                acc[c] += gl_vec[c] * gl_k;
            }
        }
        if any_l {
            let add = w_sh * lo_sh + w_mid * lo_mid + w_hi * lo_hi + lo_gl;
            for c in 0..3 {
                acc[c] += add;
            }
        }
        *l = acc;
    }
    out
}

/// 色彩分级：阴影 / 中间调 / 高光三段染色 + 全局轮 + 均衡 + 混合。
///
/// Color grading: three-band tint over shadows / midtones / highlights, plus the
/// global wheel, balance and blending.
pub fn color_grade(rgb: &mut [f32], p: &GradeParams) {
    if !grade_active(p) {
        return;
    }
    let tab = grade_table(p);
    rgb.par_chunks_exact_mut(3)
        .with_min_len(MIN_PIXELS)
        .for_each(|px| {
            let lum = (px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2]).clamp(0.0, 1.0);
            let idx =
                (lum * (GRADE_N - 1) as f32 + 0.5).clamp(0.0, (GRADE_N - 1) as f32) as i32 as usize;
            let row = tab[idx];
            for c in 0..3 {
                px[c] = (px[c] + row[c]).clamp(0.0, 1.0);
            }
        });
}

// ===================== HSL =====================

/// 合成三张色相表的返回形状：偏移 / 饱和系数 / 明度系数，未启用的轴为 None。
type HslTables = (Option<Vec<f32>>, Option<Vec<f32>>, Option<Vec<f32>>);

/// Python `_hsl_tables`：合成三张色相表（偏移 / 饱和系数 / 明度系数），
/// 未启用的轴为 None。
fn hsl_tables(p: &GradeParams) -> HslTables {
    let has_h = p.hsl_h.iter().any(|&v| v != 0.0);
    let has_s = p.hsl_s.iter().any(|&v| v != 0.0);
    let has_l = p.hsl_l.iter().any(|&v| v != 0.0);
    let mut shift = if has_h {
        vec![0.0f32; HUE_BINS]
    } else {
        Vec::new()
    };
    let mut sfac = if has_s {
        vec![0.0f32; HUE_BINS]
    } else {
        Vec::new()
    };
    let mut lfac = if has_l {
        vec![0.0f32; HUE_BINS]
    } else {
        Vec::new()
    };
    let scale = 360.0f32 / HUE_BINS as f32;
    for i in 0..8 {
        let hs = p.hsl_h[i];
        let ss = p.hsl_s[i];
        let ls = p.hsl_l[i];
        if hs == 0.0 && ss == 0.0 && ls == 0.0 {
            continue;
        }
        let center = HSL_CENTERS[i];
        for k in 0..HUE_BINS {
            let h = k as f32 * scale;
            let w = smooth(50.0, 18.0, hue_dist(h, center));
            if hs != 0.0 {
                shift[k] += (hs / 100.0 * 40.0) as f32 * w;
            }
            if ss != 0.0 {
                sfac[k] += (ss / 100.0) as f32 * w;
            }
            if ls != 0.0 {
                lfac[k] += (ls / 100.0 * 0.6) as f32 * w;
            }
        }
    }
    (
        has_h.then_some(shift),
        has_s.then_some(sfac),
        has_l.then_some(lfac),
    )
}

/// HSL：8 个色相带的色相偏移 / 饱和度 / 明亮度。
///
/// HSL: hue shift / saturation / luminance across 8 hue bands.
pub fn hsl(rgb: &mut [f32], p: &GradeParams) {
    if p.hsl_h.iter().all(|&v| v == 0.0)
        && p.hsl_s.iter().all(|&v| v == 0.0)
        && p.hsl_l.iter().all(|&v| v == 0.0)
    {
        return;
    }
    let (ts, tf, tl) = hsl_tables(p);
    rgb.par_chunks_exact_mut(3)
        .with_min_len(MIN_PIXELS)
        .for_each(|px| {
            // Python 在进 HSL 前 clip 一次。
            let r = px[0].clamp(0.0, 1.0);
            let g = px[1].clamp(0.0, 1.0);
            let b = px[2].clamp(0.0, 1.0);
            let (h0, s0, v0) = rgb_to_hsv(r, g, b);
            // 三轴共用"原始色相"算出的分箱索引（Python 在改 h 之前算 idx）。
            let idx = hue_idx(h0);

            let mut h = h0;
            if let Some(t) = &ts {
                h += t[idx];
                if h >= 360.0 {
                    h -= 360.0;
                }
                if h < 0.0 {
                    h += 360.0;
                }
            }
            let mut s = s0;
            if let Some(t) = &tf {
                s = (s0 * (1.0 + t[idx])).clamp(0.0, 1.0);
            }
            let mut v = v0;
            if let Some(t) = &tl {
                v = (v0 * (1.0 + t[idx])).clamp(0.0, 1.0);
            }
            let (nr, ng, nb) = hsv_to_rgb(h, s, v);
            px[0] = nr;
            px[1] = ng;
            px[2] = nb;
        });
}

// ===================== 单色 =====================

/// Python `_mono_table`：色相 -> 亮度系数（中性 1.0）。
fn mono_table(p: &GradeParams) -> Vec<f32> {
    let mut tab = vec![1.0f32; HUE_BINS];
    let scale = 360.0f32 / HUE_BINS as f32;
    for i in 0..8 {
        let a = p.mono_h[i];
        if a == 0.0 {
            continue;
        }
        let center = HSL_CENTERS[i];
        let add = (a / 100.0) as f32;
        for k in 0..HUE_BINS {
            let h = k as f32 * scale;
            tab[k] += add * smooth(50.0, 18.0, hue_dist(h, center));
        }
    }
    tab
}

/// 单色 + 8 色相带亮度混合（Lightroom B&W Mixer）。
///
/// Monochrome plus 8-band luminance mixing (Lightroom B&W Mixer).
pub fn mono(rgb: &mut [f32], p: &GradeParams) {
    let k = (p.mono / 100.0) as f32;
    if k <= 0.0 {
        return;
    }
    let has_mh = p.mono_h.iter().any(|&v| v != 0.0);
    let tab = if has_mh { Some(mono_table(p)) } else { None };
    rgb.par_chunks_exact_mut(3)
        .with_min_len(MIN_PIXELS)
        .for_each(|px| {
            let r = px[0].clamp(0.0, 1.0);
            let g = px[1].clamp(0.0, 1.0);
            let b = px[2].clamp(0.0, 1.0);
            let mut lum = r * LUMA[0] + g * LUMA[1] + b * LUMA[2];
            if let Some(tab) = &tab {
                let (h, _, _) = rgb_to_hsv(r, g, b);
                lum = (lum * tab[hue_idx(h)]).clamp(0.0, 1.0);
            }
            px[0] = r + (lum - r) * k;
            px[1] = g + (lum - g) * k;
            px[2] = b + (lum - b) * k;
        });
}

// ===================== 色阶 / 参数曲线 / 曲线合成表 =====================

fn is_identity(points: &[[f64; 2]]) -> bool {
    points.len() == 2 && points[0] == [0.0, 0.0] && points[1] == [255.0, 255.0]
}

fn curves_default(p: &GradeParams) -> bool {
    is_identity(&p.curve_rgb)
        && is_identity(&p.curve_r)
        && is_identity(&p.curve_g)
        && is_identity(&p.curve_b)
}

fn levels_active(p: &GradeParams) -> bool {
    p.lv_black != 0.0
        || p.lv_gamma != 1.0
        || p.lv_white != 255.0
        || p.lv_out_black != 0.0
        || p.lv_out_white != 255.0
}

fn par_active(p: &GradeParams) -> bool {
    p.par_shadows.abs() > 1e-6
        || p.par_darks.abs() > 1e-6
        || p.par_lights.abs() > 1e-6
        || p.par_highlights.abs() > 1e-6
}

/// Python `_levels_lut`：色阶五项 -> n 级 [0,1] 表。
///
/// 量纲与 Python 一致：黑白场在 0~255 上给定，下界 `+2/255` 是**归一化**后的
/// 最小跨度（`lo + 2.0/255.0`），照抄不误。
fn levels_lut(p: &GradeParams, n: usize) -> Vec<f32> {
    let lo = (p.lv_black.clamp(0.0, 253.0) as f32) / 255.0;
    let hi = (p.lv_white as f32).clamp(lo + 2.0 / 255.0, 255.0) / 255.0;
    let g = (p.lv_gamma.clamp(0.1, 5.0)) as f32;
    let ob = (p.lv_out_black as f32).clamp(0.0, 255.0) / 255.0;
    let ow = (p.lv_out_white as f32).clamp(ob + 2.0 / 255.0, 255.0) / 255.0;
    let mut out = vec![0.0f32; n];
    for (k, o) in out.iter_mut().enumerate() {
        let v = (k as f64 / (n - 1) as f64) as f32;
        let mut t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
        if g != 1.0 {
            t = t.powf(1.0 / g);
        }
        *o = ob + t * (ow - ob);
    }
    out
}

/// Python `_par_lut`：参数曲线四段 + 三分界 -> 亮度映射表（严格不减）。
fn par_lut(p: &GradeParams, n: usize) -> Vec<f32> {
    let s1 = (p.par_split1.clamp(2.0, 86.0) as f32) / 100.0;
    let s2 = (p.par_split2.clamp(s1 as f64 * 100.0 + 2.0, 92.0) as f32) / 100.0;
    let s3 = (p.par_split3.clamp(s2 as f64 * 100.0 + 2.0, 98.0) as f32) / 100.0;
    let a = (p.par_shadows.clamp(-100.0, 100.0)) as f32;
    let b = (p.par_darks.clamp(-100.0, 100.0)) as f32;
    let c = (p.par_lights.clamp(-100.0, 100.0)) as f32;
    let d = (p.par_highlights.clamp(-100.0, 100.0)) as f32;
    let mut out = vec![0.0f32; n];
    let mut run_max = 0.0f32;
    for (k, o) in out.iter_mut().enumerate() {
        let v = (k as f64 / (n - 1) as f64) as f32;
        let s_sh = smooth(0.0, s1, v);
        let w_sh = 1.0 - s_sh;
        let w_dark = s_sh * (1.0 - smooth(s1, s2, v));
        let w_light = smooth(s1, s2, v) * (1.0 - smooth(s2, s3, v));
        let w_hi = smooth(s2, s3, v);
        let off = (a * w_sh + b * w_dark + c * w_light + d * w_hi) / 100.0;
        let val = (v + off * PAR_SCALE).clamp(0.0, 1.0);
        // np.maximum.accumulate
        if val > run_max {
            run_max = val;
        }
        *o = run_max;
    }
    out
}

/// 把一个 [0,1] 表量化成 [0,n-1] 的索引（Python `np.rint(t*(n-1))` clip）。
fn quant_idx(t: &[f32], n: usize) -> Vec<i32> {
    let m = (n - 1) as f32;
    t.iter()
        .map(|&v| (rint(v * m)).clamp(0.0, m) as i32)
        .collect()
}

/// Python `_curve_lut`：色阶 + 参数曲线 + 主曲线 + 分通道曲线合成一张
/// 65536 级表，返回 [r,g,b] 三张 uint16 通道表。
fn curve_lut(p: &GradeParams) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let n = CURVE_N;
    let lv = levels_active(p).then(|| levels_lut(p, n));
    let lv_idx = lv.as_ref().map(|t| quant_idx(t, n));
    let par = par_active(p).then(|| par_lut(p, n));
    let par_idx = par.as_ref().map(|t| quant_idx(t, n));

    // 色阶 + 参数曲线的合成基索引（对所有通道相同）。
    let base_idx: Option<Vec<i32>> = match (&lv_idx, &par_idx) {
        (None, None) => None,
        (Some(l), None) => Some(l.clone()),
        (None, Some(pa)) => Some(pa.clone()),
        (Some(l), Some(_pa)) => {
            let par = par.as_ref().unwrap();
            let m = (n - 1) as f32;
            Some(
                l.iter()
                    .map(|&i| (rint(par[i as usize] * m)).clamp(0.0, m) as i32)
                    .collect(),
            )
        }
    };

    let master = {
        let t = pchip_lut(&p.curve_rgb, n);
        t.iter().map(|&v| v / 255.0).collect::<Vec<f32>>()
    };

    let mut chans = Vec::with_capacity(3);
    for key in [&p.curve_r, &p.curve_g, &p.curve_b] {
        let lt = {
            let t = pchip_lut(key, n);
            t.iter().map(|&v| v / 255.0).collect::<Vec<f32>>()
        };
        let m = (n - 1) as f32;
        let mut tab = vec![0u16; n];
        for v in 0..n {
            let yi = match &base_idx {
                None => lt[v],
                Some(bi) => lt[bi[v] as usize],
            };
            let inner = (rint(yi * m)).clamp(0.0, m) as i32;
            tab[v] = (rint(master[inner as usize] * 65535.0)) as u16;
        }
        chans.push(tab);
    }
    let mut it = chans.into_iter();
    (it.next().unwrap(), it.next().unwrap(), it.next().unwrap())
}

/// 色阶 + 参数曲线 + 曲线：合成一张 16bit 表，量化输入一次查完。
///
/// Levels plus parametric curve plus curve: one composed 16-bit table, with the
/// quantized input looked up once.
pub fn tone_curve(rgb: &mut [f32], p: &GradeParams) {
    tone_curve_inner(rgb, p, false);
}

/// 与 [`tone_curve`] 相同，但**白以上的部分不截断**（HDR 用）。
///
/// 这两者的差别只有一处：尾部延伸的结果是否钳到 1。SDR 必须钳——下游按 8bit 写盘，
/// 白以上没有地方放；HDR 不能钳——那一段正是 PQ 上半段的内容，钳掉它等于「HDR 只
/// 到参考白为止」。
///
/// 尾部公式本身两边一样：`y1 + (x − 1) · slope`，其中 `y1` 是曲线端点值。
/// `slope = pchip_end_slope / 255 + k · 0.6`，k 是 `curve_hdr_tail` 滑杆（0–1）。
/// 那个 `0.6` 是「滑杆拉满时斜率额外加多少」——它决定尾部多陡。实测（`s_curve(200)`，
/// 端点 0.7843，拉满，`slope` = 0.6033）：x=1.5 → 1.0858、x=2 → 1.3873、x=4 → 2.5934。
/// 也就是 +1 stop over 参考白的输入落成 +0.47 stop，压缩半档多。
///
/// Identical to [`tone_curve`] except that **values above white are not clamped** (for HDR).
///
/// That single difference is the point: SDR must clamp (it is written as 8bit, there is nowhere
/// to put anything above white), HDR must not (that region *is* the upper half of PQ; clamping
/// it would mean "HDR stops at reference white").
///
/// The tail formula is the same on both sides: `y1 + (x − 1) · slope` with `y1` the curve
/// endpoint and `slope = pchip_end_slope / 255 + k · 0.6`, where k is the `curve_hdr_tail`
/// slider (0–1). The `0.6` is how much extra slope a maxed slider adds. Measured
/// (`s_curve(200)`, endpoint 0.7843, maxed, `slope` = 0.6033): x=1.5 → 1.0858, x=2 → 1.3873,
/// x=4 → 2.5934 — an input one stop over reference white lands half a stop and change above it.
pub fn tone_curve_keep_over(rgb: &mut [f32], p: &GradeParams) {
    tone_curve_inner(rgb, p, true);
}

fn tone_curve_inner(rgb: &mut [f32], p: &GradeParams, keep_over: bool) {
    if curves_default(p) && !levels_active(p) && !par_active(p) {
        return;
    }
    let (lr, lg, lb) = curve_lut(p);
    // 曲线 HDR 尾部（对齐 LR fExtension_LumaHDR 的「延伸而非截断」，E2）：x>1 的
    // 输入按「曲线端点值 + 末端斜率 × (x−1)」延伸。斜率的两个来源分开：曲线自身的
    // PCHIP 末端斜率（换算到 0–1 值域，除以 255）给出「曲线往哪儿走」，滑杆
    // `curve_hdr_tail` 乘 0.6 给「额外多陡」。
    //
    // 那个 0.6 的判据是**接得住但既不放大也不甩出去**（详见 `fit_sliders.rs::
    // curve_hdr_tail_slope_stays_in_a_usable_window` 的四条）：斜率恒 < 1 保证
    // 「输出 ≤ 输入」（亮部滚降的定义），下界 0.40 保证 +1EV 之后还越得过参考白；
    // 实测 0.6033，落在可用窗口 [0.40, 0.85] 的 44% 处，偏保守那一侧。
    //
    // 早期注释在这里写过「实测最大输出 0.90」——那个数是在 SDR 出口量的，被末尾钳位
    // 掩盖了，跟斜率没关系，已删。
    //
    // x∈[0,1] 主路径逐位不动——这是「默认不漂移」最干净的 else 分支实现。
    //
    // Curve HDR tail (matching LR's fExtension_LumaHDR "extend, do not truncate", E2): inputs
    // above 1 extend as `endpoint + slope · (x−1)`. The slope comes from two places: the curve's
    // own PCHIP end slope (scaled into 0–1 by dividing by 255) says where the curve is heading,
    // and the `curve_hdr_tail` slider times 0.6 adds extra steepness.
    //
    // The 0.6 is judged by **carrying the input without amplifying or flinging it**: a slope
    // always below 1 keeps "output ≤ input" (the very definition of a highlight roll-off), and
    // the 0.40 lower bound keeps +1 EV above reference white; measured 0.6033, at 44% of the
    // usable window [0.40, 0.85] — the conservative side. See
    // `fit_sliders.rs::curve_hdr_tail_slope_stays_in_a_usable_window` for all four criteria.
    //
    // An earlier comment here claimed "measured max output 0.90". That number was taken through
    // the *SDR* exit, where the final clamp hides it; it says nothing about the slope. Removed.
    let k = (p.curve_hdr_tail / 100.0) as f32;
    let tail = if k > 0.0 {
        Some((
            lr[CURVE_N - 1] as f32 / 65535.0,
            lg[CURVE_N - 1] as f32 / 65535.0,
            lb[CURVE_N - 1] as f32 / 65535.0,
            (crate::grade::pchip_end_slope(&p.curve_rgb) as f32 / 255.0 + k * 0.6).max(0.0),
        ))
    } else {
        None
    };
    rgb.par_chunks_exact_mut(3)
        .with_min_len(MIN_PIXELS)
        .for_each(|px| {
            if let Some((y1r, y1g, y1b, slope)) =
                tail.filter(|_| px[0] > 1.0 || px[1] > 1.0 || px[2] > 1.0)
            {
                let y1 = [y1r, y1g, y1b];
                let tabs = [&lr, &lg, &lb];
                for c in 0..3 {
                    if px[c] > 1.0 {
                        let ext = y1[c] + (px[c] - 1.0) * slope;
                        px[c] = if keep_over {
                            ext.max(0.0)
                        } else {
                            ext.clamp(0.0, 1.0)
                        };
                    } else {
                        // ≤1 的通道仍走原查表口径。
                        let q = rint(px[c].clamp(0.0, 1.0) * 65535.0) as usize;
                        px[c] = tabs[c][q.min(CURVE_N - 1)] as f32 / 65535.0;
                    }
                }
                return;
            }
            // 三个通道都在 [0,1] 内：走量化查表（Python：clip -> rint(x*65535) -> 查表
            // -> /65535）。查表值天然 ≤1，所以 HDR 与 SDR 在这一支没有差别。
            //
            // All three channels are within [0,1], so this is the quantised lookup; table
            // values are inherently ≤1, which is why HDR and SDR do not differ on this branch.
            let q0 = rint(px[0].clamp(0.0, 1.0) * 65535.0) as usize;
            let q1 = rint(px[1].clamp(0.0, 1.0) * 65535.0) as usize;
            let q2 = rint(px[2].clamp(0.0, 1.0) * 65535.0) as usize;
            px[0] = lr[q0.min(CURVE_N - 1)] as f32 / 65535.0;
            px[1] = lg[q1.min(CURVE_N - 1)] as f32 / 65535.0;
            px[2] = lb[q2.min(CURVE_N - 1)] as f32 / 65535.0;
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn hsv_round_trip_is_stable() {
        // RGB→HSV→RGB 在参数全 0 时必须逐点几乎不变（OpenCV 约定自洽）。
        let vals = [0.0f32, 0.004, 0.13, 0.25, 0.5, 0.66, 0.77, 0.9, 1.0];
        for &r in &vals {
            for &g in &vals {
                for &b in &vals {
                    let (h, s, v) = rgb_to_hsv(r, g, b);
                    let (nr, ng, nb) = hsv_to_rgb(h, s, v);
                    assert!(approx(nr, r, 1e-5), "r {r}->{nr}");
                    assert!(approx(ng, g, 1e-5), "g {g}->{ng}");
                    assert!(approx(nb, b, 1e-5), "b {b}->{nb}");
                }
            }
        }
    }

    #[test]
    fn hsl_zero_params_is_identity() {
        let p = GradeParams::default();
        let mut rgb = vec![0.2f32, 0.6, 0.9, 0.1, 0.1, 0.1];
        let before = rgb.clone();
        hsl(&mut rgb, &p);
        assert_eq!(rgb, before);
    }

    #[test]
    fn mono_full_greyscales() {
        let mut p = GradeParams::default();
        p.mono = 100.0;
        let mut rgb = vec![0.2f32, 0.6, 0.9];
        mono(&mut rgb, &p);
        assert!(approx(rgb[0], rgb[1], 1e-6) && approx(rgb[1], rgb[2], 1e-6));
        let want = 0.2 * LUMA[0] + 0.6 * LUMA[1] + 0.9 * LUMA[2];
        assert!(approx(rgb[0], want, 1e-5));
    }

    #[test]
    fn mono_zero_is_noop() {
        let p = GradeParams::default();
        let mut rgb = vec![0.3f32, 0.4, 0.5];
        let before = rgb.clone();
        mono(&mut rgb, &p);
        assert_eq!(rgb, before);
    }

    #[test]
    fn hue_distance_wraps_symmetrically() {
        // 色相环上 350 与 10 只相隔 20 度。
        assert!(approx(hue_dist(350.0, 10.0), 20.0, 1e-4));
        assert!(approx(hue_dist(0.0, 0.0), 0.0, 1e-4));
        assert!(approx(hue_dist(180.0, 0.0), 180.0, 1e-4));
    }

    #[test]
    fn tone_curve_identity_is_noop() {
        let p = GradeParams::default();
        let mut rgb = vec![0.4f32, 0.5, 0.6];
        let before = rgb.clone();
        tone_curve(&mut rgb, &p);
        assert_eq!(rgb, before);
    }

    #[test]
    fn tone_curve_master_rounds_monotonic() {
        let mut p = GradeParams::default();
        p.curve_rgb = vec![[0.0, 10.0], [128.0, 150.0], [255.0, 245.0]];
        let mut rgb: Vec<f32> = (0..=255).flat_map(|i| [i as f32 / 255.0; 3]).collect();
        tone_curve(&mut rgb, &p);
        // 同一张表作用在三通道，输出应逐通道单调不减。
        for c in 0..3 {
            let mut prev = -1.0f32;
            for i in 0..=255 {
                let v = rgb[i * 3 + c];
                assert!(v >= prev - 1e-6, "通道 {c} 在 {i} 处非单调");
                prev = v;
            }
        }
    }

    #[test]
    fn par_lut_is_non_decreasing() {
        let mut p = GradeParams::default();
        p.par_shadows = 40.0;
        p.par_darks = -30.0;
        p.par_lights = 25.0;
        p.par_highlights = -50.0;
        let t = par_lut(&p, 256);
        for w in t.windows(2) {
            assert!(w[1] >= w[0] - 1e-6);
        }
        assert!(t.iter().all(|&v| (0.0..=1.0).contains(&v)));
    }

    #[test]
    fn color_grade_inactive_is_noop() {
        let p = GradeParams::default();
        let mut rgb = vec![0.3f32, 0.5, 0.7];
        let before = rgb.clone();
        color_grade(&mut rgb, &p);
        assert_eq!(rgb, before);
    }
}
