//! 空间类显影运算：镜头校正、降噪、锐化、清晰度/纹理、颗粒、蒙版内细节。
//!
//! 语义以 Python `lapse/grade.py` 为准：`_lens` / `_distort_map` / `_defringe`、
//! `_denoise`、`_sharpen`、`_guided` / `_local_contrast`、`_noise`、`_mask_spatial`。
//! 这些都需要邻域访问，草图路径（`Opts::sketch`）与 `for_lut` 路径会跳过它们，
//! 所以这里的实现必须与"跳过"逻辑一致，否则预览与导出不是一条链。
//!
//! 实现要点（逐条对齐 Python）：
//! - 盒滤波 `cv2.blur` = 均值 + BORDER_REFLECT_101，这里用可分离滑窗做到 O(1)。
//! - 引导滤波 `_guided`：`r>=8→1/4`、`r>=4→1/2` 下采样算系数再上采样，
//!   但 `min(H,W)<64` 强制走全分辨率分支——**金标是 48×64，因此金标只验 s=1
//!   那条分支**；下采样分支（INTER_AREA/INTER_LINEAR）按 cv2 语义实现但未被
//!   金标覆盖，见 `guided` 里的注释。
//! - 高斯核没有 OpenCV，核尺寸按 OpenCV `getGaussianKernel` 的经验公式
//!   `radius = floor(4*sigma + 0.6)`、`ksize = 2*radius+1` 复现（与 cv2 5.0.0
//!   实测逐点一致），核值用 `exp(-d^2/2σ^2)` 归一化。
//! - 颗粒场用自实现的 PCG 之外的确定性正态发生器，随机序列与 numpy 不可能
//!   相同，只对齐统计量与空间结构；场按 (尺寸,size,freq) 缓存，与帧号无关。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;

use super::{LUMA, smooth};
use crate::grade::GradeParams;
use crate::mask::active_masks;

// ---------------------------------------------------------------------------
// 基础：边界反射、盒滤波、亮度场
// ---------------------------------------------------------------------------

/// BORDER_REFLECT_101 的下标映射：不含边缘像素重复的镜像。对任意整数（含负、
/// 超界）都成立，用 `period = 2*(n-1)` 取模后折叠。`n<=1` 时恒为 0。
#[inline]
pub(crate) fn refl101(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n <= 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let mut p = i.rem_euclid(period);
    if p >= n {
        p = period - p;
    }
    p as usize
}

#[inline]
fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// `cv2.blur(src, (2r+1, 2r+1))`：均值盒滤波，BORDER_REFLECT_101。
/// 可分离滑窗，横向按行并行、纵向按列并行。
pub(crate) fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let k = 2 * r + 1;
    // 横向：先累加窗口内的和（不除 k，最后统一缩放，贴近 cv2 的定点/浮点累加）。
    let mut hs = vec![0.0f32; w * h];
    hs.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let base = y * w;
        let mut acc = 0.0f32;
        for t in 0..k {
            acc += src[base + refl101(t as isize - r as isize, w)];
        }
        row[0] = acc;
        for x in 1..w {
            acc += src[base + refl101((x + k - 1) as isize - r as isize, w)]
                - src[base + refl101((x - 1) as isize - r as isize, w)];
            row[x] = acc;
        }
    });
    // 纵向：对每一列滑窗求和，存成列主序后缩放回行主序。
    let scale = 1.0 / (k as f32 * k as f32);
    let mut vs = vec![0.0f32; w * h];
    vs.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
        let mut acc = 0.0f32;
        for t in 0..k {
            acc += hs[refl101(t as isize - r as isize, h) * w + x];
        }
        col[0] = acc;
        for y in 1..h {
            acc += hs[refl101((y + k - 1) as isize - r as isize, h) * w + x]
                - hs[refl101((y - 1) as isize - r as isize, h) * w + x];
            col[y] = acc;
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v = vs[x * h + y] * scale;
        }
    });
    out
}

/// 行主序 RGB 亮度场（Rec.709 LUMA），与 `x @ LUMA` 一致。
pub(crate) fn lum_field(rgb: &[f32]) -> Vec<f32> {
    rgb.par_chunks_exact(3)
        .map(|px| px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2])
        .collect()
}

// ---------------------------------------------------------------------------
// 高斯核与高斯模糊（无 OpenCV，自行按 cv2 规则造核）
// ---------------------------------------------------------------------------

/// OpenCV `getGaussianKernel` 的核尺寸经验公式：`radius = floor(4*sigma+0.6)`，
/// `ksize = 2*radius+1`（与 cv2 5.0.0 在 σ∈[0.4,6] 实测逐点一致）。
fn gaussian_radius(sigma: f32) -> usize {
    let rad = (4.0 * sigma + 0.6).floor();
    if rad < 0.0 { 0 } else { rad as usize }
}

/// 归一化一维高斯核，值用 `exp(-d^2/2σ^2)` 计算后除以总和，与 cv2 一致（差 ~1e-7）。
pub(crate) fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = gaussian_radius(sigma);
    let n = 2 * radius + 1;
    let mut k = vec![0.0f32; n];
    let s2 = 2.0 * sigma * sigma;
    let mut sum = 0.0f32;
    for (i, v) in k.iter_mut().enumerate() {
        let d = i as f32 - radius as f32;
        let e = (-(d * d) / s2).exp();
        *v = e;
        sum += e;
    }
    if sum > 0.0 {
        for v in &mut k {
            *v /= sum;
        }
    }
    k
}

/// `cv2.GaussianBlur(src, (0,0), sigma)`：可分离卷积 + BORDER_REFLECT_101。
fn gaussian_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let k = gaussian_kernel(sigma);
    let n = k.len();
    let rad = n / 2;
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let base = y * w;
        for (x, v) in row.iter_mut().enumerate() {
            let mut s = 0.0f32;
            for i in 0..n {
                s += k[i] * src[base + refl101(x as isize + i as isize - rad as isize, w)];
            }
            *v = s;
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let mut s = 0.0f32;
            for i in 0..n {
                s += k[i] * tmp[refl101(y as isize + i as isize - rad as isize, h) * w + x];
            }
            *v = s;
        }
    });
    out
}

// ---------------------------------------------------------------------------
// 引导滤波（`_guided_coeff` / `_guided`）
// ---------------------------------------------------------------------------

/// `_guided_coeff`：返回系数场 (a, b)。全部盒滤波窗口 = `2r+1`，REFLECT_101。
fn guided_coeff(
    i: &[f32],
    p: &[f32],
    w: usize,
    h: usize,
    r: usize,
    eps: f32,
) -> (Vec<f32>, Vec<f32>) {
    let mi = box_mean(i, w, h, r);
    let mp = box_mean(p, w, h, r);
    let ii: Vec<f32> = i.par_iter().map(|v| v * v).collect();
    let ip: Vec<f32> = i.par_iter().zip(p.par_iter()).map(|(a, b)| a * b).collect();
    let bi2 = box_mean(&ii, w, h, r);
    let bip = box_mean(&ip, w, h, r);
    let n = w * h;
    // 系数场逐点独立，两个面各一遍并行（`var` 重算一次比串行两遍便宜）。
    let a: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|idx| {
            let var = bi2[idx] - mi[idx] * mi[idx];
            let cov = bip[idx] - mi[idx] * mp[idx];
            cov / (var + eps)
        })
        .collect();
    let b: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|idx| mp[idx] - a[idx] * mi[idx])
        .collect();
    (a, b)
}

/// `_guided`：大半径时在低分辨率上算系数再上采样。
///
/// 金标图 48×64，`min(H,W)<64` 恒成立，因此金标只走 `s==1 or min<64` 的全
/// 分辨率分支**；下采样分支按 Python 的 `cv2.resize(INTER_AREA)→系数→
/// cv2.resize(INTER_LINEAR)` 语义实现，但没有金标覆盖，正确性仅由 Python 对照
/// 保证（去雾的单测 `dehaze_downsample_matches_python` 显式覆盖了这条分支）。
pub(crate) fn guided(i: &[f32], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let s = if r >= 8 {
        4
    } else if r >= 4 {
        2
    } else {
        1
    };
    let n = w * h;
    if s == 1 || w.min(h) < 64 {
        let (a, b) = guided_coeff(i, p, w, h, r, eps);
        let ba = box_mean(&a, w, h, r);
        let bb = box_mean(&b, w, h, r);
        return (0..n)
            .into_par_iter()
            .map(|idx| ba[idx] * i[idx] + bb[idx])
            .collect();
    }
    // 下采样分支（未被金标覆盖）：INTER_AREA 缩 → 小半径系数 → INTER_LINEAR 放大。
    let sw = (w / s).max(1);
    let sh = (h / s).max(1);
    let is = resize_area(i, w, h, sw, sh);
    let ps = resize_area(p, w, h, sw, sh);
    let rs = ((r as f32 / s as f32).round() as usize).max(1);
    let (a, b) = guided_coeff(&is, &ps, sw, sh, rs, eps);
    let a = resize_linear(&a, sw, sh, w, h);
    let b = resize_linear(&b, sw, sh, w, h);
    (0..n)
        .into_par_iter()
        .map(|idx| a[idx] * i[idx] + b[idx])
        .collect()
}

/// `cv2.resize(..., INTER_AREA)` 降采样（面积加权，行列可分离）。仅用于上面的
/// 下采样分支，金标不覆盖。
fn resize_area(src: &[f32], w: usize, h: usize, dw: usize, dh: usize) -> Vec<f32> {
    if dw == 0 || dh == 0 {
        return Vec::new();
    }
    let xr = w as f64 / dw as f64;
    let yr = h as f64 / dh as f64;
    let mut tmp = vec![0.0f32; h * dw];
    // 横向：每个输出格只看同一行的源像素，按源行并行。
    tmp.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        for (dx, v) in row.iter_mut().enumerate() {
            let f0 = dx as f64 * xr;
            let f1 = (dx + 1) as f64 * xr;
            let i0 = f0.floor().max(0.0) as usize;
            let i1 = (f1.ceil() as usize).min(w).max(i0 + 1);
            let (mut acc, mut wsum) = (0.0f64, 0.0f64);
            for ix in i0..i1.min(w) {
                let l = f0.max(ix as f64);
                let rr = f1.min((ix + 1) as f64);
                let ww = (rr - l).max(0.0);
                if ww > 0.0 {
                    acc += src[y * w + ix] as f64 * ww;
                    wsum += ww;
                }
            }
            *v = if wsum > 0.0 {
                (acc / wsum) as f32
            } else {
                src[y * w]
            };
        }
    });
    let mut out = vec![0.0f32; dh * dw];
    // 纵向：每个输出行只看固定的几行 `tmp`，按输出行并行。
    out.par_chunks_mut(dw).enumerate().for_each(|(dy, row)| {
        let f0 = dy as f64 * yr;
        let f1 = (dy + 1) as f64 * yr;
        let i0 = f0.floor().max(0.0) as usize;
        let i1 = (f1.ceil() as usize).min(h).max(i0 + 1);
        for (dx, v) in row.iter_mut().enumerate() {
            let (mut acc, mut wsum) = (0.0f64, 0.0f64);
            for iy in i0..i1.min(h) {
                let l = f0.max(iy as f64);
                let rr = f1.min((iy + 1) as f64);
                let ww = (rr - l).max(0.0);
                if ww > 0.0 {
                    acc += tmp[iy * dw + dx] as f64 * ww;
                    wsum += ww;
                }
            }
            *v = if wsum > 0.0 {
                (acc / wsum) as f32
            } else {
                tmp[dx]
            };
        }
    });
    out
}

/// `cv2.resize(..., INTER_LINEAR)`：中心对齐的双线性重采样。仅用于下采样分支。
fn resize_linear(src: &[f32], w: usize, h: usize, dw: usize, dh: usize) -> Vec<f32> {
    if dw == 0 || dh == 0 || w == 0 || h == 0 {
        return Vec::new();
    }
    let sx = w as f32 / dw as f32;
    let sy = h as f32 / dh as f32;
    let mut out = vec![0.0f32; dh * dw];
    // 每个输出行只看两行源数据，按输出行并行。
    out.par_chunks_mut(dw).enumerate().for_each(|(dy, row)| {
        let mut fy = (dy as f32 + 0.5) * sy - 0.5;
        if fy < 0.0 {
            fy = 0.0;
        }
        let y0 = (fy.floor()) as usize;
        let y1 = (y0 + 1).min(h - 1);
        let wy = (fy - y0 as f32).clamp(0.0, 1.0);
        for (dx, v) in row.iter_mut().enumerate() {
            let mut fx = (dx as f32 + 0.5) * sx - 0.5;
            if fx < 0.0 {
                fx = 0.0;
            }
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(w - 1);
            let wx = (fx - x0 as f32).clamp(0.0, 1.0);
            let a = src[y0 * w + x0];
            let b = src[y0 * w + x1];
            let c = src[y1 * w + x0];
            let d = src[y1 * w + x1];
            *v = a * (1.0 - wx) * (1.0 - wy)
                + b * wx * (1.0 - wy)
                + c * (1.0 - wx) * wy
                + d * wx * wy;
        }
    });
    out
}

// ---------------------------------------------------------------------------
// 局部对比（`_local_contrast`：清晰度 / 纹理共用）
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn local_contrast(
    rgb: &mut [f32],
    w: usize,
    h: usize,
    amount: f32,
    radius: usize,
    eps: f32,
    scale: f32,
) {
    if w == 0 || h == 0 {
        return;
    }
    let lum = lum_field(rgb);
    let g = guided(&lum, &lum, w, h, radius, eps);
    let detail: Vec<f32> = lum
        .par_iter()
        .zip(g.par_iter())
        .map(|(l, gv)| l - gv)
        .collect();
    rgb.par_chunks_exact_mut(3).enumerate().for_each(|(i, px)| {
        let l = lum[i];
        let mid = 1.0 - (2.0 * clamp01(l) - 1.0) * (2.0 * clamp01(l) - 1.0);
        let det = detail[i] * (0.35 + 0.65 * mid);
        let mut ratio = (l + amount * scale * det) / l.max(1e-4);
        ratio = ratio.clamp(0.0, 3.0);
        px[0] *= ratio;
        px[1] *= ratio;
        px[2] *= ratio;
    });
}

// ---------------------------------------------------------------------------
// 降噪（`_denoise`）
// ---------------------------------------------------------------------------

/// 中位数：排序后奇数取中、偶数取中间两个平均，与 `np.median` 一致。
fn median(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let m = v.len() / 2;
    if v.len() % 2 == 1 {
        v[m]
    } else {
        (v[m - 1] + v[m]) / 2.0
    }
}

/// 残差的中位数绝对偏差 σ：`hf[::4,::4]` 采样后 `1.4826*median(|x-median(x)|)`。
/// （Donoho 1994 的 MAD→σ 口径；原为私有 fn，降噪多尺度 / AI 降噪共用，提权。）
pub(crate) fn mad_sigma(hf: &[f32], w: usize, h: usize) -> f32 {
    let mut samp = Vec::new();
    let mut y = 0usize;
    while y < h {
        let mut x = 0usize;
        while x < w {
            samp.push(hf[y * w + x]);
            x += 4;
        }
        y += 4;
    }
    let center = median(&samp);
    let dev: Vec<f32> = samp.iter().map(|v| (v - center).abs()).collect();
    1.4826 * median(&dev)
}

/// 亮度场的自动噪声 σ 估计：`hf = Y − box5(Y)`（与细节回还同源）后走
/// [`mad_sigma`]。多尺度降噪的 eps 与将来的 AI 降噪分档共用这一个口。
/// O(n/16) 采样，不缓存。
pub(crate) fn estimate_sigma(lum: &[f32], w: usize, h: usize) -> f32 {
    if w == 0 || h == 0 {
        return 0.0;
    }
    let blur5 = box_mean(lum, w, h, 2);
    let hf: Vec<f32> = lum
        .par_iter()
        .zip(blur5.par_iter())
        .map(|(l, b)| l - b)
        .collect();
    mad_sigma(&hf, w, h)
}

/// 3×3 结构元膨胀（最大值滤波），越界按 OpenCV dilate 默认边界（DBL_MAX 哨兵）
/// 只取图像内的邻域。
fn dilate3(m: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    // 每个输出点只看 3×3 邻域，行与行互不依赖：按行切开并行，逐点算式不变。
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let mut best = f32::MIN;
            for dy in 0..3i32 {
                let yy = y as i32 + dy - 1;
                if yy < 0 || yy as usize >= h {
                    continue;
                }
                for dx in 0..3i32 {
                    let xx = x as i32 + dx - 1;
                    if xx < 0 || xx as usize >= w {
                        continue;
                    }
                    best = best.max(m[yy as usize * w + xx as usize]);
                }
            }
            *v = best;
        }
    });
    out
}

/// `_denoise` 的可参数化核心：色度先降（亮度作引导）、亮度后降。公开入口与
/// 蒙版内的单遍补算都调它，只是传入不同的量与副参数。
///
/// 多尺度 + σ 噪声模型（设计 2026-10-30 §1.3）：`multiscale>0 || sigma_manual>0`
/// 且 `luma_amt>0` 时走多尺度分支（粗层引导 + σ 驱动 eps）；两者全 0 时走旧
/// 单尺度路径，逐位一致。色度通道不动（色噪以低频为主）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn denoise_apply(
    img: &mut [f32],
    w: usize,
    h: usize,
    luma_amt: f32,
    color_amt: f32,
    luma_detail: f32,
    luma_contrast: f32,
    color_smooth: f32,
    multiscale: f32,
    sigma_manual: f32,
) {
    if w == 0 || h == 0 {
        return;
    }
    let min = w.min(h);
    let d = luma_detail.clamp(0.0, 100.0) / 100.0;
    let cs = color_smooth.clamp(0.0, 100.0) / 100.0;

    if color_amt != 0.0 {
        let k = color_amt / 100.0;
        let r = (min / 180).max(2);
        let eps = 0.0002 + 0.008 * k * (0.3 + 1.4 * cs);
        // cv2.COLOR_RGB2YCrCb（float 版，系数经 venv 实测）
        // 三个面各自逐点独立，各一遍并行；`yy` 重算一遍比把三个面串成一个
        // 串行循环便宜得多，算式与原来逐字相同。
        let y: Vec<f32> = img
            .par_chunks_exact(3)
            .map(|p| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])
            .collect();
        let cr: Vec<f32> = img
            .par_chunks_exact(3)
            .map(|p| 0.713 * (p[0] - (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])) + 0.5)
            .collect();
        let cb: Vec<f32> = img
            .par_chunks_exact(3)
            .map(|p| 0.564 * (p[2] - (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])) + 0.5)
            .collect();
        let scr = guided(&y, &cr, w, h, r, eps);
        let scb = guided(&y, &cb, w, h, r, eps);
        // cv2.COLOR_YCrCb2RGB（用正向系数的精确逆）
        let rows = w * 3;
        img.par_chunks_exact_mut(rows)
            .enumerate()
            .for_each(|(row, px)| {
                for (x, p) in px.chunks_exact_mut(3).enumerate() {
                    let i = row * w + x;
                    let cry = cr[i] + (scr[i] - cr[i]) * k;
                    let cby = cb[i] + (scb[i] - cb[i]) * k;
                    let yy = y[i];
                    let rr = yy + (cry - 0.5) / 0.713;
                    let bb = yy + (cby - 0.5) / 0.564;
                    let gg = (yy - 0.299 * rr - 0.114 * bb) / 0.587;
                    p[0] = rr;
                    p[1] = gg;
                    p[2] = bb;
                }
            });
    }

    if luma_amt != 0.0 {
        let lum = lum_field(img);
        let k = luma_amt / 100.0;
        let ms = multiscale.clamp(0.0, 100.0) / 100.0;
        let sv = sigma_manual.clamp(0.0, 100.0);
        if ms == 0.0 && sv == 0.0 {
            // 旧单尺度路径：与升级前逐位一致（多尺度/手动 σ 全 0 时必须原样）。
            let r = (min / 260).max(2);
            let eps = 0.0001 + 0.006 * k;
            let sm = guided(&lum, &lum, w, h, r, eps);
            // 加性回写：暗部不会把残差放大成新的色噪。
            let mut res: Vec<f32> = lum
                .par_iter()
                .zip(sm.par_iter())
                .map(|(l, s)| (s - l) * k)
                .collect();
            // 细节回还与亮度增强用的是同一张 5×5 均值图：算一次，别各算一遍全幅。
            if d != 0.0 || luma_contrast != 0.0 {
                let blur5 = box_mean(&lum, w, h, 2);
                if d != 0.0 {
                    // 把被磨掉的结构按「显著高于噪声」的门限还回去（星星、纹理边缘）。
                    let hf: Vec<f32> = lum
                        .par_iter()
                        .zip(blur5.par_iter())
                        .map(|(l, b)| l - b)
                        .collect();
                    let sig = mad_sigma(&hf, w, h);
                    let raw: Vec<f32> = hf
                        .par_iter()
                        .map(|v| ((v.abs() / sig.max(1e-4) - 3.0) / 3.0).clamp(0.0, 1.0))
                        .collect();
                    let dm = dilate3(&raw, w, h);
                    res.par_iter_mut().enumerate().for_each(|(i, out)| {
                        let m = dm[i] * raw[i].powf(0.3);
                        *out += (lum[i] - sm[i]) * (k * 3.0 * d) * m;
                    });
                }
                if luma_contrast != 0.0 {
                    let f = (luma_contrast / 100.0) * 0.25;
                    res.par_iter_mut().enumerate().for_each(|(i, out)| {
                        *out += (lum[i] - blur5[i]) * f;
                    });
                }
            }
            let rows = w * 3;
            img.par_chunks_exact_mut(rows)
                .enumerate()
                .for_each(|(row, px)| {
                    for (x, p) in px.chunks_exact_mut(3).enumerate() {
                        let v = res[row * w + x];
                        p[0] += v;
                        p[1] += v;
                        p[2] += v;
                    }
                });
        } else {
            // 多尺度 + σ 噪声模型（设计 §1.3）：σ 估计每帧一次（O(n/16)，不缓存）；
            // 手动覆盖优先（与 LR 一致）。
            let sig = if sv > 0.0 {
                // manual_map: σ = 0.002 + (v/100)^1.6·0.030，值域 [0.002,0.032]。
                // 这是「滑杆读数 → 噪声标准差」的映射，没有外部参照可对齐，
                // 取的是「v=0 时几乎不降噪、v=100 时覆盖到 ISO 25600 量级」。
                0.002 + (sv / 100.0).powf(1.6) * 0.030
            } else {
                estimate_sigma(&lum, w, h)
            };
            // eps 的 σ 平方律（引导滤波 eps 是方差量纲，噪声方差 ∝ σ²）。
            // max 门控（设计 §6）：σ 只升 eps 不降，避免高 ISO 估计偏低时降噪不足。
            //
            // ⚠ 实测诊断（`tests/fit_denoise.rs`）：这个式子在 σ=0.02 时给出
            // eps≈0.034，是噪声方差的 85 倍；σ 低于 0.006 时又有 0.0001+0.006k
            // 的固定地板，于是 eps 与真实噪声脱钩。后果是按 §46.2 量：
            // 单尺度 69.5%/84.5%（几乎达标），多尺度 72.0%/64.2%（低 21 点）。
            // 改成 `eps = gain·σ̂²` 能把读数推到 52%/81%，明显更接近目标——
            // 但 `nr_multiscale` 有 Python 金标，改公式等于改参照数据，需与
            // oracle 一侧同步重冻结后再落地。见 CHANGELOG「已知问题」。
            let sr = sig / 0.006; // σ_ref = 0.006
            let eps = (0.0001 + 0.006 * k) * (sr * sr).max(1.0);
            // 粗尺度 r_c = 2·中尺度（min/130，触发 guided 自动下采样），
            // 中尺度 r = min/260；两层 eps 同式。
            let rc = (min / 130).max(2);
            let rm = (min / 260).max(2);
            let b_coarse = guided(&lum, &lum, w, h, rc, eps);
            let b_mid = guided(&b_coarse, &b_coarse, w, h, rm, eps);
            // 粗层只回写低频差异，量被 s = multiscale 门控并入中尺度结果。
            let sm: Vec<f32> = b_mid
                .par_iter()
                .zip(b_coarse.par_iter())
                .map(|(m, c)| m + ms * (c - m))
                .collect();
            let mut res: Vec<f32> = lum
                .par_iter()
                .zip(sm.par_iter())
                .map(|(l, s)| (s - l) * k)
                .collect();
            // 细节回还与亮度增强：σ 估计与细节回还共用一张 5×5 均值图。
            if d != 0.0 || luma_contrast != 0.0 {
                let blur5 = box_mean(&lum, w, h, 2);
                if d != 0.0 {
                    // 门限还回逻辑不变，作用在 mid 层残差（lum − B_mid）上。
                    let hf: Vec<f32> = lum
                        .par_iter()
                        .zip(blur5.par_iter())
                        .map(|(l, b)| l - b)
                        .collect();
                    let sig_a = mad_sigma(&hf, w, h);
                    let raw: Vec<f32> = hf
                        .par_iter()
                        .map(|v| ((v.abs() / sig_a.max(1e-4) - 3.0) / 3.0).clamp(0.0, 1.0))
                        .collect();
                    let dm = dilate3(&raw, w, h);
                    res.par_iter_mut().enumerate().for_each(|(i, out)| {
                        let m = dm[i] * raw[i].powf(0.3);
                        *out += (lum[i] - b_mid[i]) * (k * 3.0 * d) * m;
                    });
                }
                if luma_contrast != 0.0 {
                    let f = (luma_contrast / 100.0) * 0.25;
                    res.par_iter_mut().enumerate().for_each(|(i, out)| {
                        *out += (lum[i] - blur5[i]) * f;
                    });
                }
            }
            let rows = w * 3;
            img.par_chunks_exact_mut(rows)
                .enumerate()
                .for_each(|(row, px)| {
                    for (x, p) in px.chunks_exact_mut(3).enumerate() {
                        let v = res[row * w + x];
                        p[0] += v;
                        p[1] += v;
                        p[2] += v;
                    }
                });
        }
    }

    img.par_iter_mut().for_each(|x| *x = clamp01(*x));
}

pub fn denoise(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams) {
    denoise_apply(
        rgb,
        w,
        h,
        p.nr_luma as f32,
        p.nr_color as f32,
        p.nr_luma_detail as f32,
        p.nr_luma_contrast as f32,
        p.nr_color_smooth as f32,
        p.nr_multiscale as f32,
        p.nr_sigma_manual as f32,
    );
}

// ---------------------------------------------------------------------------
// 锐化（`_sharpen`）
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(crate) fn sharpen_apply(
    rgb: &mut [f32],
    w: usize,
    h: usize,
    amount: f32,
    radius: f32,
    detail: f32,
    masking: f32,
    protect: f32,
) {
    if w == 0 || h == 0 {
        return;
    }
    let lum = lum_field(rgb);
    let r = radius.clamp(0.5, 3.0);
    let g = gaussian_blur(&lum, w, h, r);
    let mut base: Vec<f32> = lum
        .par_iter()
        .zip(g.par_iter())
        .map(|(l, gv)| l - gv)
        .collect();
    let d = detail.clamp(0.0, 100.0) / 100.0;
    if d != 0.0 {
        let fine_sigma = (r * 0.5).max(0.6);
        let gf = gaussian_blur(&lum, w, h, fine_sigma);
        base.par_iter_mut().enumerate().for_each(|(i, v)| {
            let fine = lum[i] - gf[i];
            *v = *v * (1.0 - d) + fine * (1.1 * d);
        });
    }
    // tanh 软限幅压 halo。
    let halo = 0.08f32;
    base.par_iter_mut()
        .for_each(|v| *v = halo * (*v / halo).tanh());
    let m = masking.clamp(0.0, 100.0) / 100.0;
    // 恒开边缘保护（对齐 LR/ACR Sharpen3 的保护掩码基础设施，E2；阈值需拟合
    // TODO(fit)）：噪声底用 MAD σ 现场估计，edge 场复用 masking 的 absbase
    // 高斯模糊。protect=0 且 masking=0 时整段跳过 = 旧路径逐位一致。
    let pw = protect.clamp(0.0, 100.0) / 100.0 * 0.9;
    if m != 0.0 || pw > 0.0 {
        let absbase: Vec<f32> = base.par_iter().map(|v| v.abs()).collect();
        let edge_sigma = (r * 2.0).max(1.0);
        let edge = gaussian_blur(&absbase, w, h, edge_sigma);
        if pw > 0.0 {
            let sigma = mad_sigma(&base, w, h);
            let t0 = sigma * 1.2 + 0.0008;
            let t1 = t0 + sigma * 2.0 + 0.004;
            base.par_iter_mut().enumerate().for_each(|(i, v)| {
                *v *= 1.0 - pw * (1.0 - smooth(t0, t1, edge[i]));
            });
        }
        if m != 0.0 {
            let thr = 0.001 + 0.02 * m * m;
            base.par_iter_mut().enumerate().for_each(|(i, v)| {
                *v *= smooth(thr * 0.35, thr, edge[i]);
            });
        }
    }
    let gain = amount.max(0.0) / 100.0 * 1.2;
    rgb.par_chunks_exact_mut(3).enumerate().for_each(|(i, p)| {
        let mut ratio = (lum[i] + gain * base[i]) / lum[i].max(1e-4);
        ratio = ratio.clamp(0.0, 3.0);
        p[0] *= ratio;
        p[1] *= ratio;
        p[2] *= ratio;
    });
}

pub fn sharpen(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams) {
    sharpen_apply(
        rgb,
        w,
        h,
        p.sharp_amount as f32,
        p.sharp_radius as f32,
        p.sharp_detail as f32,
        p.sharp_masking as f32,
        p.sharp_protect as f32,
    );
}

// ---------------------------------------------------------------------------
// 输出锐化（专用 USM + 阈值，atlas §21 DoUSMWithThreshold 口径）
// ---------------------------------------------------------------------------

/// 输出编码预留（E2：`UniformsOutputSharpenEncodeSDR/HDR` 单 pass 合并结构）。
/// 当前导出管线上限是 sRGB f32 [0,1]，本轮只有 SDR 落地；HDR（PQ）留给
/// atlas §36 里程碑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputEncoding {
    Sdr,
    /// HDR（PQ）输出：decode_pq → USM → encode_pq，当前不可达占位。
    #[allow(dead_code)]
    HdrPq,
}

/// 一次输出锐化的三个用户可见参数（用途档 × 强度档合成后的结果）。
///
/// 打包成结构体是为了让函数签名留在可读范围内；数值本身没有语义变化。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UsmSettings {
    pub amount: f32,
    pub radius: f32,
    pub threshold: f32,
}

impl UsmSettings {
    pub fn new(amount: f32, radius: f32, threshold: f32) -> Self {
        Self {
            amount,
            radius,
            threshold,
        }
    }
}

/// 输出锐化专用 USM：高通 = 原图 − 高斯模糊，线性软阈值（E2 threshold 口径，
/// 非 masking），Δ 限幅压振铃，按亮度比值回写（色度不漂）。
///
/// 输出尺寸感知：LR 按输出分辨率隐式放大半径（4K 导出比 1080p 需要更大的
/// 半径才等效），但公开资料没有给出确切式，也没有可对齐的参照图，所以这里
/// 是**自定**的：`size_gain = clamp(max(w,h)/1600, 1, 3).sqrt()`。
/// 它保留在 [`UsmTuning`] 里可调，等将来有多分辨率参照再拟合。
/// 只在 `apply_output_finish`（缩放后、水印前）调用，预览不经过这里。
pub fn output_sharpen_usm(
    rgb: &mut [f32],
    w: usize,
    h: usize,
    s: UsmSettings,
    enc: OutputEncoding,
) {
    output_sharpen_usm_tuned(rgb, w, h, s, enc, UsmTuning::default())
}

/// 同 [`output_sharpen_usm`]，但接受一组显式的拟合参数。
///
/// 生产路径不该用它——默认值才是拟合出来的结论。它存在的意义是让
/// `tests/fit_sharpen.rs` 能在同一份实现上扫参，而不是复制一份算法来试数
/// （那样试出来的值跟真正跑的值不是一回事）。
pub fn output_sharpen_usm_tuned(
    rgb: &mut [f32],
    w: usize,
    h: usize,
    s: UsmSettings,
    enc: OutputEncoding,
    tuning: UsmTuning,
) {
    match enc {
        OutputEncoding::Sdr => output_sharpen_usm_sdr(rgb, w, h, s, tuning),
        OutputEncoding::HdrPq => {
            todo!("HDR（PQ）输出编码接入后实现：decode_pq → USM → encode_pq（atlas §36）")
        }
    }
}

/// 输出尺寸感知增益：`size_gain = clamp(max(w,h)/1600, 1, 3).sqrt()`，
/// 半径乘 gain、阈值乘 √gain。见 [`output_sharpen_usm`] 的说明——自定值，
/// 无参照可拟合。
pub fn usm_size_gain(w: usize, h: usize) -> f32 {
    usm_size_gain_with(w, h, UsmTuning::default())
}

/// 输出锐化里那几个「只能拟合、无法取证」的常量，集中放在一处。
///
/// 之所以做成结构体而不是散在函数体里的字面量：这些值要按
/// `atlas §46.2` 的验收口径（MTF50 提升 1.2–1.6×、halo < 3%）扫参确定，
/// 扫参时需要在同一份代码上试不同取值；定下来以后仍然只有一个默认值，
/// 生产路径行为不变。
///
/// `delta_cap` 已按上述口径拟合（见 `tests/fit_sharpen.rs`，扫参表可复现）；
/// `size_scale` / `size_cap` 仍是无参照的自定值——**不要**把它们标成已拟合。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UsmTuning {
    /// 高通 Δ 的**绝对上限**（作用在乘 `amount` 之前，单位是亮度）。
    ///
    /// 两个坑都踩过，记在这里免得再踩：
    ///
    /// - 早期写 `delta_limit = 0.12·amount`：bound 随力度伸缩（真咬住的话输出会
    ///   正比于 amount²），而 0.12 比典型高通幅度（阶跃边约 0.042）还大近 3 倍，
    ///   于是它**一次都没咬住**。实测 amount 1.0→2.0 的 halo 是 8.35%→17.66%，
    ///   就是纯线性放大——限幅等于不存在。
    /// - 改成本值上限但放在乘 amount **之后**，则不同力度在同一硬边上被压到同一个
    ///   绝对量，强度滑杆失效（`export_stills` 的强度分层测试当场红）。
    pub delta_cap: f32,
    /// 尺寸补偿的参考长边（px）：`size_gain = (max/size_scale).clamp(1,..)^0.5`。
    pub size_scale: f32,
    /// 尺寸补偿上限（增益封顶）。
    pub size_cap: f32,
}

impl Default for UsmTuning {
    fn default() -> Self {
        Self {
            delta_cap: 0.0105,
            size_scale: 1600.0,
            size_cap: 3.0,
        }
    }
}

/// 带调参的尺寸增益，供扫参用。
pub fn usm_size_gain_with(w: usize, h: usize, t: UsmTuning) -> f32 {
    ((w.max(h) as f32 / t.size_scale).clamp(1.0, t.size_cap)).sqrt()
}

/// SDR 单路：sRGB 编码 f32 [0,1] 上的 USM。
fn output_sharpen_usm_sdr(rgb: &mut [f32], w: usize, h: usize, s: UsmSettings, tuning: UsmTuning) {
    let UsmSettings {
        amount,
        radius,
        threshold,
    } = s;
    if w == 0 || h == 0 || amount <= 0.0 {
        return;
    }
    let size_gain = usm_size_gain_with(w, h, tuning);
    let radius_eff = (radius * size_gain).max(0.3);
    let threshold_eff = threshold * size_gain.sqrt();
    let lum = lum_field(rgb);
    let blur = gaussian_blur(&lum, w, h, radius_eff);
    // 限幅作用在**高通信号本身**（乘 amount 之前），于是：
    // 1) 它真的会咬住——高通幅度一旦超过 `delta_cap` 就不再放大；
    // 2) 输出仍随 amount 线性变化，强度档不会在高对比边上失效。
    // 若把限幅放到乘 amount 之后（或让 bound 随 amount 伸缩），前者会变成
    // 「成正比于 amount 的闸门」，后者会让滑杆在硬边上失灵，两者都试过、都错。
    let cap = tuning.delta_cap;
    let hs: Vec<f32> = lum
        .par_iter()
        .zip(blur.par_iter())
        .map(|(y, b)| {
            let hp = y - b;
            let mag = (hp.abs() - threshold_eff).max(0.0);
            let v = if hp >= 0.0 { mag } else { -mag };
            v.clamp(-cap, cap)
        })
        .collect();
    rgb.par_chunks_exact_mut(3).enumerate().for_each(|(i, p)| {
        let mut ratio = (lum[i] + amount * hs[i]) / lum[i].max(1e-4);
        ratio = ratio.clamp(0.0, 3.0);
        p[0] *= ratio;
        p[1] *= ratio;
        p[2] *= ratio;
    });
}

// ---------------------------------------------------------------------------
// 镜头校正（`_lens` / `_distort_map` / `_defringe`）
// ---------------------------------------------------------------------------

/// `_radius`：到画面中心的椭圆归一化半径（到角落为 1），0~1。与
/// `crate::grade::vignette_mask` 里画的 `r_ell` 同一套公式（roundness=0）。
fn falloff(w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    let wf = w as f32;
    let hf = h as f32;
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = (y as f32 - (hf - 1.0) / 2.0) / (hf / 2.0);
        for (x, v) in row.iter_mut().enumerate() {
            let sx = (x as f32 - (wf - 1.0) / 2.0) / (wf / 2.0);
            *v = ((sx * sx + sy * sy) / 2.0).sqrt();
        }
    });
    out
}

/// `cv2.remap(INTER_LINEAR, BORDER_REPLICATE)`：逐目标像素按 map 双线性采样，
/// 越界索引夹到边缘（replicate）。已对拍 cv2 到 1e-7。
fn remap_replicate(src: &[f32], w: usize, h: usize, mapx: &[f32], mapy: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h * 3];
    // 逐目标像素独立采样，按行并行；每个点的四项加权顺序不变。
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for (x, px) in row.chunks_exact_mut(3).enumerate() {
            let u = mapx[y * w + x];
            let v = mapy[y * w + x];
            let x0 = u.floor() as isize;
            let dx = u - x0 as f32;
            let y0 = v.floor() as isize;
            let dy = v - y0 as f32;
            let cx0 = x0.clamp(0, w as isize - 1) as usize;
            let cx1 = (x0 + 1).clamp(0, w as isize - 1) as usize;
            let cy0 = y0.clamp(0, h as isize - 1) as usize;
            let cy1 = (y0 + 1).clamp(0, h as isize - 1) as usize;
            let w00 = (1.0 - dx) * (1.0 - dy);
            let w10 = dx * (1.0 - dy);
            let w01 = (1.0 - dx) * dy;
            let w11 = dx * dy;
            for c in 0..3 {
                px[c] = src[(cy0 * w + cx0) * 3 + c] * w00
                    + src[(cy0 * w + cx1) * 3 + c] * w10
                    + src[(cy1 * w + cx0) * 3 + c] * w01
                    + src[(cy1 * w + cx1) * 3 + c] * w11;
            }
        }
    });
    out
}

/// 径向畸变 remap 表（`_distort_map`）：正=桶形、负=枕形。
fn distort_maps(w: usize, h: usize, k: f32) -> (Vec<f32>, Vec<f32>) {
    let r = falloff(w, h);
    let cx = (w as f32 - 1.0) * 0.5;
    let cy = (h as f32 - 1.0) * 0.5;
    let mut mx = vec![0.0f32; w * h];
    let mut my = vec![0.0f32; w * h];
    // 两张表逐点独立，各自按行并行；`s` 与两个坐标的算式与原来逐字相同。
    mx.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let rv = r[y * w + x];
            *v = (x as f32 - cx) * (1.0 + k * rv * rv) + cx;
        }
    });
    my.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let rv = r[y * w + x];
            *v = (y as f32 - cy) * (1.0 + k * rv * rv) + cy;
        }
    });
    (mx, my)
}

/// Sobel 3×3（BORDER_REFLECT_101）。`horizontal=true` → d/dx，kernel
/// `[[-1,0,1],[-2,0,2],[-1,0,1]]`；否则 d/dy（转置）。与 cv2.Sobel 实测一致。
fn sobel3(src: &[f32], w: usize, h: usize, horizontal: bool) -> Vec<f32> {
    let smooth_axis = |t: i32| match t {
        -1 | 1 => 1,
        0 => 2,
        _ => 0,
    };
    // 核先摊平：每个输出点按 dy→dx 的固定顺序累加，行与行互不依赖。
    let mut k = [[0.0f32; 3]; 3];
    for (dy, row) in k.iter_mut().enumerate() {
        for (dx, v) in row.iter_mut().enumerate() {
            let (row, col) = (dy as i32 - 1, dx as i32 - 1);
            *v = if horizontal {
                col * smooth_axis(row)
            } else {
                row * smooth_axis(col)
            } as f32;
        }
    }
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let y = y as i32;
        for (x, v) in row.iter_mut().enumerate() {
            let x = x as i32;
            let mut acc = 0.0f32;
            // cv2.Sobel ksize=3 默认用 BORDER_REFLECT_101 取越界值，核权重不变：
            //   horizontal → [[-1,0,1],[-2,0,2],[-1,0,1]]
            //   vertical   → [[-1,-2,-1],[0,0,0],[1,2,1]]
            for dy in -1..=1 {
                let iy = refl101((y + dy) as isize, h);
                for dx in -1..=1 {
                    let ix = refl101((x + dx) as isize, w);
                    acc += k[(dy + 1) as usize][(dx + 1) as usize] * src[iy * w + ix];
                }
            }
            *v = acc;
        }
    });
    out
}

/// `_defringe`：去紫边 / 绿边，只压边缘附近、单侧通道明显高出的部分。就地改。
fn defringe(img: &mut [f32], w: usize, h: usize, a: f32) {
    if a <= 0.0 || w == 0 || h == 0 {
        return;
    }
    let clipped: Vec<f32> = img.par_iter().map(|v| clamp01(*v)).collect();
    let lum = lum_field(&clipped);
    let gx = sobel3(&lum, w, h, true);
    let gy = sobel3(&lum, w, h, false);
    img.par_chunks_exact_mut(3).enumerate().for_each(|(i, p)| {
        let mag = (gx[i] * gx[i] + gy[i] * gy[i]).sqrt();
        let edge = smooth(0.010, 0.075, mag);
        let (rv, gv, bv) = (clipped[i * 3], clipped[i * 3 + 1], clipped[i * 3 + 2]);
        let mx = rv.max(bv);
        let pur = (rv.min(bv) - gv).max(0.0);
        let grn = (gv - mx).max(0.0);
        let wp = a * edge * clamp01(pur / 0.10);
        let wg = a * edge * clamp01(grn / 0.10);
        p[0] = rv - (rv - gv) * wp;
        p[2] = bv - (bv - gv) * wp;
        p[1] = gv - (gv - mx) * wg;
    });
}

pub fn lens(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams) {
    if w == 0 || h == 0 {
        return;
    }
    if p.lens_distort != 0.0 {
        let k = (p.lens_distort / 100.0 * 0.22) as f32;
        let (mx, my) = distort_maps(w, h, k);
        let src = rgb.to_vec();
        let remapped = remap_replicate(&src, w, h, &mx, &my);
        rgb.copy_from_slice(&remapped);
    }
    if p.lens_vignette != 0.0 {
        let lv = (p.lens_vignette / 100.0 * 0.9) as f32;
        let r = falloff(w, h);
        rgb.par_chunks_exact_mut(3).enumerate().for_each(|(i, px)| {
            let m = (1.0 + lv * r[i]).clamp(0.0, 2.0);
            px[0] *= m;
            px[1] *= m;
            px[2] *= m;
        });
    }
    if p.lens_fringe != 0.0 {
        let a = (p.lens_fringe / 100.0) as f32;
        defringe(rgb, w, h, a);
    }
    rgb.par_iter_mut().for_each(|x| *x = clamp01(*x));
}

// ---------------------------------------------------------------------------
// 颗粒（`_noise`）
// ---------------------------------------------------------------------------

/// splitmix64 → 均匀 (0,1) → Box–Muller 正态。种子固定、与帧号无关，
/// 保证同一序列每帧是同一张颗粒场（否则视频里颗粒会"沸腾"）。
struct NormalGen {
    state: u64,
    spare: Option<f32>,
}

impl NormalGen {
    fn new(seed: u64) -> Self {
        Self {
            state: seed,
            spare: None,
        }
    }
    #[inline]
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    #[inline]
    fn uniform(&mut self) -> f64 {
        // 53 位精度、开区间 (0,1)，避免 log(0)。
        let u = (self.next_u64() >> 11) as f64;
        (u + 1.0) / ((1u64 << 53) as f64)
    }
    fn normal(&mut self) -> f32 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let u1 = self.uniform();
        let u2 = self.uniform();
        let r = (-2.0 * u1.ln()).sqrt();
        let (s, c) = (2.0 * std::f64::consts::PI * u2).sin_cos();
        self.spare = Some((r * s) as f32);
        (r * c) as f32
    }
}

/// 生成一张颗粒场（结构与 Python `_noise` 一致：粗网格取随机→放大→模糊），
/// 数值只对齐统计量。场零均值化，保证不引入整体亮度偏移。
fn build_noise_field(w: usize, h: usize, size: f32, freq: f32) -> Vec<f32> {
    let sz = size.clamp(1.0, 12.0);
    let fq = freq.clamp(0.2, 1.0);
    let mut rng = NormalGen::new(2024_0517);
    let field = if fq >= 0.999 && sz <= 1.0 {
        // 逐像素取随机（与旧版颗粒场结构一致）。
        (0..w * h).map(|_| rng.normal()).collect::<Vec<f32>>()
    } else {
        let sh = ((h as f32 * fq).round() as usize).max(1);
        let sw = ((w as f32 * fq).round() as usize).max(1);
        let small: Vec<f32> = (0..sh * sw).map(|_| rng.normal()).collect();
        let mut n = resize_linear(&small, sw, sh, w, h);
        let k = sz.round() as usize;
        if k > 1 {
            n = box_mean(&n, w, h, k);
        }
        n
    };
    // 零均值化（对 std 影响可忽略，但保证 diff_mean 落在容差内）。
    let n = w * h;
    let mean = if n > 0 {
        field.iter().sum::<f32>() / n as f32
    } else {
        0.0
    };
    field.into_iter().map(|v| v - mean).collect()
}

/// 颗粒噪声场的缓存键：宽高，加 size 与 freq 的定点量化值。
type NoiseKey = (usize, usize, i32, i32);
type NoiseCache = Mutex<HashMap<NoiseKey, Arc<Vec<f32>>>>;

fn noise_field(w: usize, h: usize, size: f32, freq: f32) -> Arc<Vec<f32>> {
    // 与 Python `_noise` 一致：按 (尺寸, size 取整到 2 位, freq 取整到 3 位) 缓存，
    // 同一序列每帧复用同一个场（种子固定、与帧号无关）。命中路径返回 Arc 克隆
    // ——grain 每帧必经这里，整场 clone（24MP 约 96MB 拷贝）就是这个热点的来路。
    let qkey = (
        w,
        h,
        (size.clamp(1.0, 12.0) * 100.0).round() as i32,
        (freq.clamp(0.2, 1.0) * 1000.0).round() as i32,
    );
    static CACHE: OnceLock<NoiseCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = map.get(&qkey) {
        return Arc::clone(v);
    }
    let field = Arc::new(build_noise_field(w, h, size, freq));
    map.insert(qkey, Arc::clone(&field));
    field
}

pub fn grain(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams) {
    if w == 0 || h == 0 {
        return;
    }
    let field = noise_field(w, h, p.grain_size as f32, p.grain_freq as f32);
    let amp = (p.grain / 100.0 * 0.075) as f32;
    // 亮度调制（对齐 LR UniformsOverlayGrain 的 shadowScale/highlightScale，E2；
    // 系数需拟合 TODO(fit)）：暗部增强、高光减弱，C1 平滑、零调制时严格恒等。
    // k=0 走旧路径逐位一致；负 k 自然反转（高光重、暗部轻）。
    let k = (p.grain_lum / 100.0) as f32;
    if k != 0.0 {
        rgb.par_chunks_exact_mut(3).enumerate().for_each(|(i, px)| {
            let y = (px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2]).clamp(0.0, 1.0);
            let dark = 1.0 + k * 1.5 * smooth(0.55, 0.0, y);
            let light = 1.0 - k * 0.7 * smooth(0.45, 1.0, y);
            let d = amp * dark * light * field[i];
            px[0] += d;
            px[1] += d;
            px[2] += d;
        });
    } else {
        rgb.par_chunks_exact_mut(3).enumerate().for_each(|(i, px)| {
            let d = amp * field[i];
            px[0] += d;
            px[1] += d;
            px[2] += d;
        });
    }
}

// ---------------------------------------------------------------------------
// 蒙版内的空间细节（`_mask_spatial`）
// ---------------------------------------------------------------------------

pub fn mask_spatial(
    rgb: &mut [f32],
    w: usize,
    h: usize,
    p: &GradeParams,
    lum: Option<&[f32]>,
    original: &[f32],
) {
    if w == 0 || h == 0 {
        return;
    }
    let mut w_sharp: Option<Vec<f32>> = None;
    let mut w_nr: Option<Vec<f32>> = None;
    let mut s_max = 0.0f32;
    let mut l_max = 0.0f32;
    let mut c_max = 0.0f32;
    // s_tune = (radius, detail, masking)，n_tune = (luma_detail, luma_contrast, color_smooth)
    let mut s_tune = (1.0f32, 25.0f32, 0.0f32);
    let mut n_tune = (0.0f32, 0.0f32, 50.0f32);
    let mut n_best = -1.0f32;

    let mut masks = active_masks(p);
    for m in masks.iter_mut() {
        // 先 clone params，避免与 build_field 的可变借用冲突。
        let d = m.params.clone();
        let get = |k: &str, def: f32| d.get(k).map(|v| *v as f32).unwrap_or(def);
        let sa = get("sharp_amount", 0.0).max(0.0);
        let nl = get("nr_luma", 0.0).max(0.0);
        let nc = get("nr_color", 0.0).max(0.0);
        let raw_r = get("sharp_radius", 1.0);
        let s_detail = get("sharp_detail", 25.0);
        let s_masking = get("sharp_masking", 0.0);
        let n_ld = get("nr_luma_detail", 0.0);
        let n_lc = get("nr_luma_contrast", 0.0);
        let n_cs = d.get("nr_color_smooth").map(|v| *v as f32).unwrap_or(50.0);
        if sa <= 1e-6 && nl <= 1e-6 && nc <= 1e-6 {
            continue;
        }
        m.build_field(w, h, lum, Some(original));
        let field = &m.field;
        if sa > 1e-6 {
            let wmap: Vec<f32> = field
                .par_iter()
                .map(|v| (v * (sa / 100.0)).clamp(0.0, 1.0))
                .collect();
            w_sharp = Some(match w_sharp.take() {
                Some(prev) => prev
                    .into_par_iter()
                    .zip(wmap.into_par_iter())
                    .map(|(a, b)| a.max(b))
                    .collect(),
                None => wmap,
            });
            if sa > s_max {
                s_max = sa;
                let radius = if raw_r == 0.0 { 1.0 } else { raw_r };
                s_tune = (radius, s_detail, s_masking);
            }
        }
        if nl > 1e-6 || nc > 1e-6 {
            let top = nl.max(nc);
            let wmap: Vec<f32> = field
                .par_iter()
                .map(|v| (v * (top / 100.0)).clamp(0.0, 1.0))
                .collect();
            w_nr = Some(match w_nr.take() {
                Some(prev) => prev
                    .into_par_iter()
                    .zip(wmap.into_par_iter())
                    .map(|(a, b)| a.max(b))
                    .collect(),
                None => wmap,
            });
            l_max = l_max.max(nl);
            c_max = c_max.max(nc);
            if top > n_best {
                n_best = top;
                n_tune = (n_ld, n_lc, n_cs);
            }
        }
    }

    if w_sharp.is_none() && w_nr.is_none() {
        return;
    }
    let blend = |rgb: &mut [f32], xs: &[f32], wt: &[f32]| {
        rgb.par_chunks_exact_mut(3).enumerate().for_each(|(i, px)| {
            let t = wt[i];
            for c in 0..3 {
                let k = i * 3 + c;
                px[c] += (xs[k] - px[c]) * t;
            }
        });
    };
    if let Some(w_nr) = w_nr {
        let mut xs = rgb.to_vec();
        // 蒙版内面积小、收益低：单遍补算不接多尺度（设计 §1.3），保持旧调用。
        denoise_apply(
            &mut xs, w, h, l_max, c_max, n_tune.0, n_tune.1, n_tune.2, 0.0, 0.0,
        );
        blend(rgb, &xs, &w_nr);
    }
    if let Some(w_sharp) = w_sharp {
        let mut xs = rgb.to_vec();
        // 蒙版内锐化不走全局的恒开保护：组强度已经按权重混回，protect 是
        // 全局面板量，这里保持旧口径。
        sharpen_apply(&mut xs, w, h, s_max, s_tune.0, s_tune.1, s_tune.2, 0.0);
        blend(rgb, &xs, &w_sharp);
    }
    rgb.par_iter_mut().for_each(|x| *x = clamp01(*x));
}

// ---------------------------------------------------------------------------
// 单测：与 numpy/cv2 手算的几个期望值对拍（venv python + numpy 2.5 + cv2 5.0）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[f32], b: &[f32], tol: f32, msg: &str) {
        assert_eq!(a.len(), b.len(), "{msg} 长度");
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            assert!((x - y).abs() <= tol, "{msg}: idx {i}: {x} vs {y}");
        }
    }

    #[test]
    fn box_mean_3x3_matches_numpy() {
        // cv2.blur(np.arange(16).reshape(4,4), (3,3)) 的期望（BORDER_REFLECT_101）
        let a: Vec<f32> = (0..16).map(|v| v as f32).collect();
        let want = [
            3.3333333_f32,
            3.6666667,
            4.6666665,
            5.0,
            4.6666665,
            5.0,
            6.0,
            6.3333335,
            8.666667,
            9.0,
            10.0,
            10.333333,
            10.0,
            10.333333,
            11.333333,
            11.666667,
        ];
        let got = box_mean(&a, 4, 4, 1);
        close(&got, &want, 1e-5, "box_mean 3x3");
    }

    #[test]
    fn gaussian_kernel_matches_opencv() {
        // cv2.getGaussianKernel(11, 1.2).ravel()
        let want = [
            0.000056469_f32,
            0.0012852357,
            0.014606954,
            0.082_897_82,
            0.23492715,
            0.33245273,
            0.23492715,
            0.082_897_82,
            0.014606954,
            0.0012852357,
            0.000056469,
        ];
        let got = gaussian_kernel(1.2);
        assert_eq!(got.len(), want.len(), "ksize sigma=1.2");
        close(&got, &want, 1e-5, "gaussian 11/1.2");
    }

    #[test]
    fn guided_full_res_matches_numpy() {
        // I=p=arange(16)/16, r=1, eps=0.01 走全分辨率分支（4x4<64 强制）。
        let i: Vec<f32> = (0..16).map(|v| v as f32 / 16.0).collect();
        let want = [
            0.0665019_f32,
            0.1183726,
            0.1741444,
            0.2260151,
            0.2687876,
            0.3206584,
            0.3764301,
            0.4283009,
            0.5091991,
            0.5610699,
            0.6168417,
            0.6687124,
            0.7114849,
            0.7633556,
            0.8191273,
            0.8709981,
        ];
        let got = guided(&i, &i, 4, 4, 1, 0.01);
        close(&got, &want, 1e-5, "guided r1 eps0.01");
    }

    #[test]
    fn noise_field_is_deterministic_and_zero_mean() {
        let a = noise_field(64, 48, 2.0, 0.7);
        let b = noise_field(64, 48, 2.0, 0.7);
        assert_eq!(a.len(), 64 * 48);
        // 同一序列每帧同一场：直接比较字节内容。
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.to_bits(), y.to_bits());
        }
        let mean: f32 = a.iter().sum::<f32>() / a.len() as f32;
        assert!(mean.abs() < 1e-5, "field mean {mean}");
        // 结构上 std 应显著小于 1（网格粗 + 模糊），接近 0.26。
        let var: f32 = a.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / a.len() as f32;
        let std = var.sqrt();
        let amp = 0.6 * 0.075; // grain=60 → (60/100)*0.075
        println!(
            "grain field std={std:.5} mean={mean:.5}; 实测 diff std≈{:.5} mean≈{:.5}",
            std * amp,
            mean * amp
        );
        assert!((std - 0.26).abs() < 0.10, "field std {std}");
    }

    /// 确定性伪随机 [0,1)（LR 混合同余，跨平台稳定）。
    fn lcg_next(state: &mut u64) -> f32 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((*state >> 33) as f32) / (1u64 << 31) as f32
    }

    #[test]
    fn estimate_sigma_math_properties() {
        let (w, h) = (96, 64);
        // 平坦图：hf 全零 → σ = 0。
        let flat = vec![0.5f32; w * h];
        assert_eq!(estimate_sigma(&flat, w, h), 0.0, "平坦图的 σ 必须是 0");
        // 加性噪声：σ 估计应落在真实 σ 的 ±35% 内（O(n/16) 采样 + MAD 的稳健量级）。
        for true_sigma in [0.01f32, 0.03, 0.08] {
            let mut s = 0x2545_F491_4F6C_DD1Du64;
            let noisy: Vec<f32> = (0..w * h)
                .map(|_| 0.5 + (lcg_next(&mut s) - 0.5) * 2.0 * true_sigma * 1.732_05)
                .collect();
            // uniform(-σ,σ) 的 std = σ；MAD 是高斯口径的稳健估计，对均匀噪声
            // 有 ~1.28× 的系统性抬升（median|x| = a/2 → 1.4826·a/2），放界到 ±50%。
            let est = estimate_sigma(&noisy, w, h);
            assert!(
                est > true_sigma * 0.5 && est < true_sigma * 1.5,
                "σ={true_sigma} 估计 {est} 出界"
            );
            // 单调性：噪声更大 → σ 估计更大。
            let mut s2 = 0x2545_F491_4F6C_DD1Du64;
            let noisier: Vec<f32> = (0..w * h)
                .map(|_| 0.5 + (lcg_next(&mut s2) - 0.5) * 2.0 * true_sigma * 3.464_10)
                .collect();
            assert!(
                estimate_sigma(&noisier, w, h) > est,
                "噪声加倍后 σ 估计必须上升"
            );
        }
    }

    /// 多尺度关（ms=0 且 manual=0）必须与升级前的旧单尺度路径逐位一致。
    /// 这里在测试里独立复刻旧算法（升级前 spatial.rs 的原实现），按位比对。
    #[test]
    fn denoise_multiscale_off_matches_legacy_bitwise() {
        let (w, h) = (64, 48);
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut img: Vec<f32> = Vec::new();
        for _ in 0..w * h {
            let v = 0.35 + lcg_next(&mut s) * 0.3;
            img.extend_from_slice(&[v, v * 0.98, v * 1.02]);
        }
        // 旧路径参数（与金标 detail_spatial / nr_luma_only 的量级一致）。
        let args = (42.0f32, 8.0f32, 20.0f32, 25.0f32, 60.0f32);
        let mut a = img.clone();
        denoise_apply(
            &mut a, w, h, args.0, args.1, args.2, args.3, args.4, 0.0, 0.0,
        );

        // —— 复刻升级前的旧实现 ——
        let mut b = img.clone();
        let min = w.min(h);
        let d = args.2.clamp(0.0, 100.0) / 100.0;
        let cs = args.4.clamp(0.0, 100.0) / 100.0;
        if args.1 != 0.0 {
            let k = args.1 / 100.0;
            let r = (min / 180).max(2);
            let eps = 0.0002 + 0.008 * k * (0.3 + 1.4 * cs);
            let y: Vec<f32> = b
                .chunks_exact(3)
                .map(|p| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])
                .collect();
            let cr: Vec<f32> = b
                .chunks_exact(3)
                .map(|p| 0.713 * (p[0] - (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])) + 0.5)
                .collect();
            let cb: Vec<f32> = b
                .chunks_exact(3)
                .map(|p| 0.564 * (p[2] - (0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])) + 0.5)
                .collect();
            let scr = guided(&y, &cr, w, h, r, eps);
            let scb = guided(&y, &cb, w, h, r, eps);
            for i in 0..w * h {
                let cry = cr[i] + (scr[i] - cr[i]) * k;
                let cby = cb[i] + (scb[i] - cb[i]) * k;
                let yy = y[i];
                let rr = yy + (cry - 0.5) / 0.713;
                let bb = yy + (cby - 0.5) / 0.564;
                let gg = (yy - 0.299 * rr - 0.114 * bb) / 0.587;
                b[i * 3] = rr;
                b[i * 3 + 1] = gg;
                b[i * 3 + 2] = bb;
            }
        }
        if args.0 != 0.0 {
            let lum = lum_field(&b);
            let k = args.0 / 100.0;
            let r = (min / 260).max(2);
            let eps = 0.0001 + 0.006 * k;
            let sm = guided(&lum, &lum, w, h, r, eps);
            let mut res: Vec<f32> = lum
                .iter()
                .zip(sm.iter())
                .map(|(l, s)| (s - l) * k)
                .collect();
            if d != 0.0 || args.3 != 0.0 {
                let blur5 = box_mean(&lum, w, h, 2);
                if d != 0.0 {
                    let hf: Vec<f32> = lum.iter().zip(blur5.iter()).map(|(l, bb)| l - bb).collect();
                    let sig = mad_sigma(&hf, w, h);
                    let raw: Vec<f32> = hf
                        .iter()
                        .map(|v| ((v.abs() / sig.max(1e-4) - 3.0) / 3.0).clamp(0.0, 1.0))
                        .collect();
                    let dm = dilate3(&raw, w, h);
                    for i in 0..w * h {
                        let m = dm[i] * raw[i].powf(0.3);
                        res[i] += (lum[i] - sm[i]) * (k * 3.0 * d) * m;
                    }
                }
                if args.3 != 0.0 {
                    let f = (args.3 / 100.0) * 0.25;
                    for i in 0..w * h {
                        res[i] += (lum[i] - blur5[i]) * f;
                    }
                }
            }
            for i in 0..w * h {
                for c in 0..3 {
                    b[i * 3 + c] += res[i];
                }
            }
        }
        for v in b.iter_mut() {
            *v = v.clamp(0.0, 1.0);
        }
        assert_eq!(a.len(), b.len());
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "多尺度关闭时第 {i} 位不一致");
        }
    }

    #[test]
    fn output_sharpen_usm_threshold_and_ramp() {
        let (w, h) = (64, 48);
        // 1) 平坦图：高通恒 0 → 任何 amount/threshold 下逐位不动。
        let flat = vec![0.5f32; w * h * 3];
        let mut a = flat.clone();
        output_sharpen_usm(
            &mut a,
            w,
            h,
            UsmSettings::new(0.9, 0.8, 0.004),
            OutputEncoding::Sdr,
        );
        for (i, (x, y)) in a.iter().zip(flat.iter()).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "平坦图第 {i} 位被 USM 改动");
        }
        // 2) 阈值行为：小振幅正弦高频被阈值吃掉 → 输出与原图逐位一致。
        let mut small = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = 0.5 + 0.001 * ((x as f32) * 0.9).sin();
                let i = (y * w + x) * 3;
                small[i] = v;
                small[i + 1] = v;
                small[i + 2] = v;
            }
        }
        let mut b = small.clone();
        output_sharpen_usm(
            &mut b,
            w,
            h,
            UsmSettings::new(0.8, 0.8, 0.05),
            OutputEncoding::Sdr,
        );
        for (i, (x, y)) in b.iter().zip(small.iter()).enumerate() {
            assert_eq!(
                x.to_bits(),
                y.to_bits(),
                "阈值 0.05 > 振幅 0.001，第 {i} 位不该被锐化"
            );
        }
        // 3) 斜坡响应：大振幅阶跃被增强（边缘对比度上升），且 Δ 限幅兜住不炸。
        let mut step = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.3 } else { 0.7 };
                let i = (y * w + x) * 3;
                step[i] = v;
                step[i + 1] = v;
                step[i + 2] = v;
            }
        }
        let mut c = step.clone();
        output_sharpen_usm(
            &mut c,
            w,
            h,
            UsmSettings::new(1.4, 1.0, 0.0),
            OutputEncoding::Sdr,
        );
        // 阶跃两侧的过冲/下冲存在（锐化真的发生了）。
        let overshoot = (c[(h / 2) * w * 3 + (w / 2) * 3] - 0.7).abs();
        assert!(overshoot > 1e-4, "阶跃边缘没有过冲，USM 没起作用");
        for v in &c {
            assert!((0.0..=1.0).contains(v), "USM 输出越界 {v}");
        }
        // 4) size_gain 公式：1600 边 = 1，3200 边 = √2，14400 边起钳在 √3。
        assert_eq!(usm_size_gain(800, 600).to_bits(), 1.0f32.to_bits());
        assert_eq!(usm_size_gain(1600, 100).to_bits(), 1.0f32.to_bits());
        assert!((usm_size_gain(3200, 100) - 2.0f32.sqrt()).abs() < 1e-6);
        assert!((usm_size_gain(14400, 100) - 3.0f32.sqrt()).abs() < 1e-6);
        // 半径与阈值随 gain 同抬：radius_eff/threshold_eff 直接由 gain 驱动，
        // 上面三段已覆盖算子行为，这里只钉公式锚点。
    }
}
