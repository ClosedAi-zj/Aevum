//! 全景合并：把多张有重叠的照片拼成一张宽幅。
//!
//! 全流程 clean-room：Harris 角点 → 归一化灰度块描述子 → 最近邻比值检验匹配 →
//! RANSAC + 归一化 DLT 求单应 → 相邻帧单应连乘成「每帧到基准帧」的映射 →
//! 逆映射双线性采样、按到源图边界的距离做羽化融合。
//!
//! 不引 LibRaw/rawspeed/OpenCV，也不用 SIFT/ORB 的受版权描述子——这里的描述子是
//! 最朴素的轴对齐归一化块，配 RANSAC 足够吃掉平移为主、旋转不大的手持/云台接片。
//!
//! Panorama merge: stitch several overlapping photos into one wide image.
//!
//! The whole pipeline is clean-room: Harris corners → normalized grayscale patch descriptors →
//! nearest-neighbor ratio-test matching → RANSAC plus normalized DLT for the homography →
//! chained pairwise homographies into a per-frame mapping to the reference frame → inverse-map
//! bilinear sampling with feather blending by distance to the source image border.
//!
//! No LibRaw, rawspeed or OpenCV, and no copyright-encumbered SIFT/ORB descriptor: the
//! descriptor here is the plainest axis-aligned normalized patch, which with RANSAC is enough
//! for handheld and gimbal stitches where translation dominates and rotation is small.

use rayon::prelude::*;

/// 一次最多拼几张：全图缓冲要同时在内存里，画布还会随张数变宽。
///
/// Maximum frames per stitch: every full-resolution buffer lives in memory at once, and the
/// canvas widens with the frame count.
pub const MAX_FRAMES: usize = 12;

/// 画布单边上限：拼接失败时单应可能把角点甩到无穷远，先给个天花板。
const MAX_SIDE: usize = 20_000;

/// 每张图最多取多少个角点参与匹配（性能护栏）。
const MAX_FEATURES: usize = 800;

/// 描述子边长：8×8 网格 → 64 维。
const DESC: usize = 8;
/// 描述子取样窗口半径（源像素），特征点离边界得比它远。
const DESC_RADIUS: usize = 8;

#[derive(Debug)]
pub enum PanoError {
    TooFew(usize),
    TooMany(usize),
    NoOverlap { a: usize, b: usize },
    CanvasTooBig { w: usize, h: usize },
    Read(String),
}

impl crate::errmsg::Msg for PanoError {
    fn msg(&self) -> (&'static str, Vec<crate::errmsg::Arg<'_>>) {
        match self {
            Self::TooFew(v) => (
                "全景合并至少要 2 张，实得 {} 张",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::TooMany(v) => (
                "全景合并一次最多 {} 张，实得 {} 张",
                vec![
                    crate::errmsg::Arg::Text(MAX_FRAMES.to_string()),
                    crate::errmsg::Arg::Text(v.to_string()),
                ],
            ),
            Self::NoOverlap { a, b } => (
                "第 {} 张与第 {} 张找不到足够的重叠：确认它们相邻且有共同区域",
                vec![
                    crate::errmsg::Arg::Text(a.to_string()),
                    crate::errmsg::Arg::Text(b.to_string()),
                ],
            ),
            Self::CanvasTooBig { w, h } => (
                "拼接后尺寸过大（{}×{}）：单应发散，检查是否混进了不相干的照片",
                vec![
                    crate::errmsg::Arg::Text(w.to_string()),
                    crate::errmsg::Arg::Text(h.to_string()),
                ],
            ),
            Self::Read(v) => (
                "读图失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
        }
    }
}

crate::impl_msg_display!(PanoError);

impl std::error::Error for PanoError {}

/// 读一张图成 f32 RGB（0..1）：与导出/HDR 共用同一条解码路。
///
/// Reads one image into f32 RGB (0..1) over the same decode path as export and HDR.
pub fn load(path: &std::path::Path) -> Result<(Vec<f32>, u32, u32), PanoError> {
    crate::export::load_rgb_f32(path).map_err(|e| PanoError::Read(e.to_string()))
}

fn check_count(n: usize) -> Result<(), PanoError> {
    if n < 2 {
        return Err(PanoError::TooFew(n));
    }
    if n > MAX_FRAMES {
        return Err(PanoError::TooMany(n));
    }
    Ok(())
}

/// 文件级入口：读盘、逐对估计单应、连乘、拼进同一张画布。
///
/// File-level entry point: read from disk, estimate a homography per pair, chain them, and
/// stitch into one canvas.
pub fn stitch_paths(paths: &[&std::path::Path]) -> Result<(Vec<f32>, u32, u32), PanoError> {
    check_count(paths.len())?;
    let mut imgs = Vec::with_capacity(paths.len());
    for path in paths {
        imgs.push(load(path)?);
    }
    let refs: Vec<(&[f32], u32, u32)> = imgs
        .iter()
        .map(|(rgb, w, h)| (rgb.as_slice(), *w, *h))
        .collect();
    stitch(&refs)
}

/// 缓冲级入口：每项是 `(rgb f32 0..1, w, h)`，第 0 张当基准。
///
/// Buffer-level entry point: each item is `(rgb f32 0..1, w, h)`, and image 0 is the
/// reference.
pub fn stitch(imgs: &[(&[f32], u32, u32)]) -> Result<(Vec<f32>, u32, u32), PanoError> {
    let n = imgs.len();
    if n == 0 {
        return Err(PanoError::TooFew(0));
    }
    if n == 1 {
        return Ok((imgs[0].0.to_vec(), imgs[0].1, imgs[0].2));
    }
    check_count(n)?;

    // 每张图的灰度与角点/描述子。
    let grays: Vec<Vec<f32>> = imgs
        .par_iter()
        .map(|(rgb, w, h)| to_gray(rgb, *w as usize, *h as usize))
        .collect();
    let feats: Vec<Vec<(f64, f64, [f32; DESC * DESC])>> = grays
        .par_iter()
        .zip(imgs.par_iter())
        .map(|(g, (_, w, h))| features_with_desc(g, *w as usize, *h as usize))
        .collect();

    // 相邻帧的单应 H_{i+1 -> i}（把第 i+1 张的坐标映到第 i 张）。
    let mut pair_h: Vec<[f64; 9]> = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let h =
            match_pairs(&feats[i], &feats[i + 1]).ok_or(PanoError::NoOverlap { a: i, b: i + 1 })?;
        pair_h.push(h);
    }

    // 连乘成「每帧到基准帧（第 0 张）」的映射。
    let mut to_ref: Vec<[f64; 9]> = Vec::with_capacity(n);
    to_ref.push(IDENTITY);
    for i in 0..n - 1 {
        let prev = to_ref[i];
        // H_{i+1 -> 0} = H_{i -> 0} ∘ H_{i+1 -> i}
        to_ref.push(mat3_mul(prev, pair_h[i]));
    }

    // 画布范围：把每张图四角前向映到基准平面，取并集。
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (i, (_, w, h)) in imgs.iter().enumerate() {
        let hm = to_ref[i];
        for &(cx, cy) in &corners(*w as usize, *h as usize) {
            let (px, py) = apply_h(hm, cx, cy);
            min_x = min_x.min(px);
            min_y = min_y.min(py);
            max_x = max_x.max(px);
            max_y = max_y.max(py);
        }
    }
    if !min_x.is_finite() || !max_x.is_finite() {
        return Err(PanoError::CanvasTooBig { w: 0, h: 0 });
    }
    let ox = min_x.floor();
    let oy = min_y.floor();
    let cw = ((max_x.ceil() - ox) as usize).max(1);
    let ch = ((max_y.ceil() - oy) as usize).max(1);
    if cw > MAX_SIDE || ch > MAX_SIDE || cw as u128 * ch as u128 > 100_000_000 {
        return Err(PanoError::CanvasTooBig { w: cw, h: ch });
    }

    // 逆映射羽化融合：每张图往画布累加「权重 × 颜色」与「权重」。
    let mut acc_rgb = vec![0.0f32; cw * ch * 3];
    let mut acc_w = vec![0.0f32; cw * ch];
    for (i, (rgb, w, h)) in imgs.iter().enumerate() {
        let (ww, hh) = (*w as usize, *h as usize);
        let inv = invert3(to_ref[i]).unwrap_or(IDENTITY);
        // 画布像素 (X,Y) → 平面坐标 (X+ox, Y+oy) → 源像素：先加平移，再乘 H^{-1}。
        let sample_h = mat3_mul(inv, translate(ox, oy));
        for y in 0..ch {
            let row = y * cw;
            for x in 0..cw {
                let (u, v) = apply_h(sample_h, x as f64, y as f64);
                if !(u >= 0.0 && v >= 0.0 && u <= (ww - 1) as f64 && v <= (hh - 1) as f64) {
                    continue;
                }
                // 到源图边界的距离当羽化权重：靠边越近越透明，接缝平滑过渡。
                let edge = u.min(v).min((ww - 1) as f64 - u).min((hh - 1) as f64 - v);
                let weight = (edge.clamp(0.0, 16.0) / 16.0) as f32;
                if weight <= 0.0 {
                    continue;
                }
                let (r, g, b) = bilinear(rgb, ww, hh, u, v);
                let p = row + x;
                acc_rgb[p * 3] += weight * r;
                acc_rgb[p * 3 + 1] += weight * g;
                acc_rgb[p * 3 + 2] += weight * b;
                acc_w[p] += weight;
            }
        }
    }
    acc_rgb
        .par_chunks_mut(3)
        .zip(acc_w.par_iter())
        .for_each(|(slot, &wsum)| {
            if wsum > 1e-6 {
                for k in 0..3 {
                    slot[k] = (slot[k] / wsum).clamp(0.0, 1.0);
                }
            } else {
                for k in 0..3 {
                    slot[k] = 0.0;
                }
            }
        });
    Ok((acc_rgb, cw as u32, ch as u32))
}

const IDENTITY: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

fn corners(w: usize, h: usize) -> [(f64, f64); 4] {
    [
        (0.0, 0.0),
        (w as f64, 0.0),
        (0.0, h as f64),
        (w as f64, h as f64),
    ]
}

/// 齐次坐标下应用 3×3 单应，返回欧氏坐标。
fn apply_h(h: [f64; 9], x: f64, y: f64) -> (f64, f64) {
    let w = h[6] * x + h[7] * y + h[8];
    if w.abs() < 1e-12 {
        return (f64::NAN, f64::NAN);
    }
    (
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    )
}

/// 行主序 3×3 相乘：结果先应用 b 再应用 a。
fn mat3_mul(a: [f64; 9], b: [f64; 9]) -> [f64; 9] {
    let mut out = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            out[r * 3 + c] = a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c] + a[r * 3 + 2] * b[6 + c];
        }
    }
    out
}

fn translate(dx: f64, dy: f64) -> [f64; 9] {
    [1.0, 0.0, dx, 0.0, 1.0, dy, 0.0, 0.0, 1.0]
}

fn invert3(m: [f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, h, i] = m;
    let a00 = e * i - f * h;
    let a01 = -(d * i - f * g);
    let a02 = d * h - e * g;
    let a10 = -(b * i - c * h);
    let a11 = a * i - c * g;
    let a12 = -(a * h - b * g);
    let a20 = b * f - c * e;
    let a21 = -(a * f - c * d);
    let a22 = a * e - b * d;
    let det = a * a00 + b * a01 + c * a02;
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    // 伴随矩阵转置。
    Some([
        a00 * inv,
        a10 * inv,
        a20 * inv,
        a01 * inv,
        a11 * inv,
        a21 * inv,
        a02 * inv,
        a12 * inv,
        a22 * inv,
    ])
}

fn to_gray(rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
    (0..w * h)
        .into_par_iter()
        .map(|p| 0.299 * rgb[p * 3] + 0.587 * rgb[p * 3 + 1] + 0.114 * rgb[p * 3 + 2])
        .collect()
}

/// Harris 角点响应 → 3×3 非极大抑制 → 按响应降序取前 `MAX_FEATURES`，带描述子。
fn features_with_desc(gray: &[f32], w: usize, h: usize) -> Vec<(f64, f64, [f32; DESC * DESC])> {
    if w < 3 || h < 3 {
        return Vec::new();
    }
    let (ix, iy) = sobel(gray, w, h);
    let (sxx, syy, sxy) = structure_tensor(&ix, &iy, w, h);
    let k = 0.04f32;
    let mut resp = vec![0.0f32; w * h];
    let mut max_r = 0.0f32;
    for p in 0..w * h {
        let det = sxx[p] * syy[p] - sxy[p] * sxy[p];
        let tr = sxx[p] + syy[p];
        let r = det - k * tr * tr;
        resp[p] = r;
        if r > max_r {
            max_r = r;
        }
    }
    if max_r <= 0.0 {
        return Vec::new();
    }
    let thresh = max_r * 0.02;
    let margin = (DESC_RADIUS + 1) as isize;
    let mut cand: Vec<(f32, usize, usize)> = Vec::new();
    for y in margin..(h as isize - margin) {
        for x in margin..(w as isize - margin) {
            let (x, y) = (x as usize, y as usize);
            let p = y * w + x;
            let c = resp[p];
            if c <= thresh {
                continue;
            }
            let mut is_max = true;
            'nms: for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let q = (y as isize + dy) as usize * w + (x as isize + dx) as usize;
                    if resp[q] > c {
                        is_max = false;
                        break 'nms;
                    }
                }
            }
            if is_max {
                cand.push((c, x, y));
            }
        }
    }
    cand.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    cand.truncate(MAX_FEATURES);
    cand.iter()
        .map(|&(_, x, y)| {
            let d = descriptor(gray, w, h, x, y);
            (x as f64, y as f64, d)
        })
        .collect()
}

/// Sobel 梯度。
fn sobel(gray: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut ix = vec![0.0f32; w * h];
    let mut iy = vec![0.0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let p = y * w + x;
            let tl = gray[p - w - 1];
            let tc = gray[p - w];
            let tr = gray[p - w + 1];
            let ml = gray[p - 1];
            let mr = gray[p + 1];
            let bl = gray[p + w - 1];
            let bc = gray[p + w];
            let br = gray[p + w + 1];
            ix[p] = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
            iy[p] = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
        }
    }
    (ix, iy)
}

/// 3×3 盒式窗口内累加梯度乘积（结构张量分量）。
fn structure_tensor(ix: &[f32], iy: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n = w * h;
    let mut sxx = vec![0.0f32; n];
    let mut syy = vec![0.0f32; n];
    let mut sxy = vec![0.0f32; n];
    sxx.par_iter_mut()
        .zip(syy.par_iter_mut().zip(sxy.par_iter_mut()))
        .enumerate()
        .for_each(|(p, (axx, (ayy, axy)))| {
            let y = p / w;
            let x = p % w;
            let (mut xx, mut yy, mut xy) = (0.0f32, 0.0f32, 0.0f32);
            for dy in -1isize..=1 {
                let yy_i = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                for dx in -1isize..=1 {
                    let xx_i = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                    let q = yy_i * w + xx_i;
                    xx += ix[q] * ix[q];
                    yy += iy[q] * iy[q];
                    xy += ix[q] * iy[q];
                }
            }
            *axx = xx / 9.0;
            *ayy = yy / 9.0;
            *axy = xy / 9.0;
        });
    (sxx, syy, sxy)
}

/// 轴对齐归一化灰度块描述子：8×8 双线性取样、去均值、L2 归一。
fn descriptor(gray: &[f32], w: usize, h: usize, cx: usize, cy: usize) -> [f32; DESC * DESC] {
    let mut d = [0.0f32; DESC * DESC];
    let mut sum = 0.0;
    for gy in 0..DESC {
        for gx in 0..DESC {
            // 把 [-radius, radius] 的窗口映射到网格中心。
            let fx = cx as f64 + (gx as f64 + 0.5) / DESC as f64 * 2.0 * DESC_RADIUS as f64
                - DESC_RADIUS as f64;
            let fy = cy as f64 + (gy as f64 + 0.5) / DESC as f64 * 2.0 * DESC_RADIUS as f64
                - DESC_RADIUS as f64;
            let v = sample_gray(gray, w, h, fx, fy);
            d[gy * DESC + gx] = v;
            sum += v;
        }
    }
    let mean = sum / (DESC * DESC) as f32;
    let mut norm = 0.0;
    for v in d.iter_mut() {
        *v -= mean;
        norm += (*v * *v) as f64;
    }
    let norm = (norm.sqrt() + 1e-9) as f32;
    for v in d.iter_mut() {
        *v /= norm;
    }
    d
}

fn sample_gray(gray: &[f32], w: usize, h: usize, x: f64, y: f64) -> f32 {
    let x = x.clamp(0.0, (w - 1) as f64);
    let y = y.clamp(0.0, (h - 1) as f64);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = (x - x0 as f64) as f32;
    let ty = (y - y0 as f64) as f32;
    let v00 = gray[y0 * w + x0];
    let v10 = gray[y0 * w + x1];
    let v01 = gray[y1 * w + x0];
    let v11 = gray[y1 * w + x1];
    let top = v00 + (v10 - v00) * tx;
    let bot = v01 + (v11 - v01) * tx;
    top + (bot - top) * ty
}

fn bilinear(rgb: &[f32], w: usize, h: usize, x: f64, y: f64) -> (f32, f32, f32) {
    let x = x.clamp(0.0, (w - 1) as f64);
    let y = y.clamp(0.0, (h - 1) as f64);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = (x - x0 as f64) as f32;
    let ty = (y - y0 as f64) as f32;
    let mut out = (0.0, 0.0, 0.0);
    for k in 0..3 {
        let v00 = rgb[(y0 * w + x0) * 3 + k];
        let v10 = rgb[(y0 * w + x1) * 3 + k];
        let v01 = rgb[(y1 * w + x0) * 3 + k];
        let v11 = rgb[(y1 * w + x1) * 3 + k];
        let top = v00 + (v10 - v00) * tx;
        let bot = v01 + (v11 - v01) * tx;
        let val = top + (bot - top) * ty;
        match k {
            0 => out.0 = val,
            1 => out.1 = val,
            _ => out.2 = val,
        }
    }
    out
}

/// 比值检验匹配两组描述子，返回源→目标的下标对。
fn ratio_match(
    a: &[(f64, f64, [f32; DESC * DESC])],
    b: &[(f64, f64, [f32; DESC * DESC])],
) -> Vec<(usize, usize)> {
    if a.len() < 4 || b.len() < 4 {
        return Vec::new();
    }
    let ratio = 0.8f32;
    (0..a.len())
        .into_par_iter()
        .filter_map(|ia| {
            let da = &a[ia].2;
            let mut best = (f32::MAX, usize::MAX);
            let mut second = f32::MAX;
            for (ib, fb) in b.iter().enumerate() {
                let mut dist = 0.0;
                for k in 0..da.len() {
                    let d = da[k] - fb.2[k];
                    dist += d * d;
                }
                if dist < best.0 {
                    second = best.0;
                    best = (dist, ib);
                } else if dist < second {
                    second = dist;
                }
            }
            if best.1 != usize::MAX && second.is_finite() && best.0.sqrt() < ratio * second.sqrt() {
                Some((ia, best.1))
            } else {
                None
            }
        })
        .collect()
}

/// 从匹配点里 RANSAC 出单应 `dst = H · src`。src/dst 分别是两张图里的坐标。
fn estimate_homography(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<[f64; 9]> {
    let n = src.len().min(dst.len());
    if n < 4 {
        return None;
    }
    let thresh = 3.0f64;
    let iters = 600;
    let mut best_inliers: Vec<usize> = Vec::new();
    let mut best_h = IDENTITY;
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = move || -> usize {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for _ in 0..iters {
        let mut idx = [0usize; 4];
        for slot in idx.iter_mut() {
            *slot = rnd();
        }
        // 四点必须不共线、且互不重复，否则 DLT 退化。
        if idx.iter().collect::<std::collections::HashSet<_>>().len() < 4 {
            continue;
        }
        let s4: Vec<(f64, f64)> = idx.iter().map(|&i| src[i]).collect();
        let d4: Vec<(f64, f64)> = idx.iter().map(|&i| dst[i]).collect();
        let Some(h) = dlt(&s4, &d4) else { continue };
        let mut inliers = Vec::new();
        for i in 0..n {
            let (px, py) = apply_h(h, src[i].0, src[i].1);
            if (px - dst[i].0).hypot(py - dst[i].1) <= thresh {
                inliers.push(i);
            }
        }
        if inliers.len() > best_inliers.len() {
            best_inliers = inliers;
            best_h = h;
        }
    }
    if best_inliers.len() < 4 {
        return None;
    }
    let s: Vec<(f64, f64)> = best_inliers.iter().map(|&i| src[i]).collect();
    let d: Vec<(f64, f64)> = best_inliers.iter().map(|&i| dst[i]).collect();
    dlt(&s, &d).or(Some(best_h))
}

/// 归一化 DLT（h9=1 的非齐次最小二乘），求解 `dst = H · src`。
fn dlt(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<[f64; 9]> {
    let n = src.len().min(dst.len());
    if n < 4 {
        return None;
    }
    // 归一化：平移质心到原点、缩放到平均距离 √2。
    let ts = normalize_t(src, n);
    let td = normalize_t(dst, n);
    // 最小二乘 A^T A x = A^T b，8×8。
    let mut ata = [[0.0f64; 8]; 8];
    let mut atb = [0.0f64; 8];
    for i in 0..n {
        let (nx, ny) = apply_h(ts, src[i].0, src[i].1);
        let (mx, my) = apply_h(td, dst[i].0, dst[i].1);
        // 行 1：[x y 1 0 0 0 -m·x -m·y] · h = m
        let r1 = [nx, ny, 1.0, 0.0, 0.0, 0.0, -mx * nx, -mx * ny];
        let b1 = mx;
        // 行 2：[0 0 0 x y 1 -n·x -n·y] · h = n
        let r2 = [0.0, 0.0, 0.0, nx, ny, 1.0, -my * nx, -my * ny];
        let b2 = my;
        accumulate_row(&mut ata, &mut atb, &r1, b1);
        accumulate_row(&mut ata, &mut atb, &r2, b2);
    }
    let h8 = solve8(ata, atb)?;
    let mut hn = [0.0f64; 9];
    hn[..8].copy_from_slice(&h8);
    hn[8] = 1.0;
    // 反归一化：H = Td^{-1} · Hnorm · Ts。
    let tdi = invert3(td)?;
    let h = mat3_mul(mat3_mul(tdi, hn), ts);
    // 归一化尺度让 h9 更接近 1，数值更稳。
    if h[8].abs() < 1e-12 {
        return None;
    }
    let s = 1.0 / h[8];
    Some([
        h[0] * s,
        h[1] * s,
        h[2] * s,
        h[3] * s,
        h[4] * s,
        h[5] * s,
        h[6] * s,
        h[7] * s,
        1.0,
    ])
}

fn accumulate_row(ata: &mut [[f64; 8]; 8], atb: &mut [f64; 8], row: &[f64; 8], b: f64) {
    for i in 0..8 {
        atb[i] += row[i] * b;
        for j in 0..8 {
            ata[i][j] += row[i] * row[j];
        }
    }
}

/// 归一化变换矩阵：平移质心到原点、缩放到平均距离 √2。
fn normalize_t(pts: &[(f64, f64)], n: usize) -> [f64; 9] {
    let mut cx = 0.0;
    let mut cy = 0.0;
    for i in 0..n {
        cx += pts[i].0;
        cy += pts[i].1;
    }
    cx /= n as f64;
    cy /= n as f64;
    let mut mean = 0.0;
    for i in 0..n {
        mean += ((pts[i].0 - cx).powi(2) + (pts[i].1 - cy).powi(2)).sqrt();
    }
    mean /= n as f64;
    let s = if mean > 1e-9 {
        std::f64::consts::SQRT_2 / mean
    } else {
        1.0
    };
    [s, 0.0, -s * cx, 0.0, s, -s * cy, 0.0, 0.0, 1.0]
}

/// 8×8 高斯消元（部分主元），解 A x = b。
fn solve8(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    // 轻微正则，防纯退化时主元为 0。
    for i in 0..8 {
        a[i][i] += 1e-12;
    }
    for col in 0..8 {
        let mut piv = col;
        for r in col + 1..8 {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-14 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        let d = a[col][col];
        for r in col + 1..8 {
            let f = a[r][col] / d;
            if f == 0.0 {
                continue;
            }
            for c in col..8 {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0.0f64; 8];
    for r in (0..8).rev() {
        let mut s = b[r];
        for c in r + 1..8 {
            s -= a[r][c] * x[c];
        }
        x[r] = s / a[r][r];
    }
    Some(x)
}

/// 匹配 + RANSAC：把第二张（b）的坐标映到第一张（a）。返回单应，重叠不足返回 None。
fn match_pairs(
    a: &[(f64, f64, [f32; DESC * DESC])],
    b: &[(f64, f64, [f32; DESC * DESC])],
) -> Option<[f64; 9]> {
    let matches = ratio_match(a, b);
    if matches.len() < 4 {
        return None;
    }
    // src = 第二张（b）坐标，dst = 第一张（a）坐标，解出的 H 把 b 映到 a。
    let src: Vec<(f64, f64)> = matches.iter().map(|&(_, ib)| (b[ib].0, b[ib].1)).collect();
    let dst: Vec<(f64, f64)> = matches.iter().map(|&(ia, _)| (a[ia].0, a[ia].1)).collect();
    estimate_homography(&src, &dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性合成场景：中灰背景上放 40 个「灰度各不相同」的方块。
    /// 每个方块角点是全图唯一的局部结构，描述子不会张冠李戴，平移可稳定复原。
    fn blob(k: usize) -> (i64, i64, i64, f32) {
        let bx = ((k * 37) % 100) as i64;
        let by = (((k * 23) % 40) + 4) as i64;
        let size = 4 + (k % 3) as i64;
        let gray = 0.1 + 0.8 * (k as f32) / 40.0;
        (bx, by, size, gray)
    }

    fn synth(w: usize, h: usize, shift_x: i64) -> Vec<f32> {
        (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let wx = (x as i64) + shift_x;
                    let wy = y as i64;
                    let mut v = 0.5f32;
                    for k in 0..40 {
                        let (bx, by, size, gray) = blob(k);
                        if wx >= bx && wx < bx + size && wy >= by && wy < by + size {
                            v = gray;
                            break;
                        }
                    }
                    [v, v, v]
                })
            })
            .collect()
    }

    #[test]
    fn linear_solve_recovers_known_vector() {
        // A = I, b = 常数向量。
        let mut a = [[0.0f64; 8]; 8];
        for i in 0..8 {
            a[i][i] = 1.0;
        }
        let b = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let x = solve8(a, b).unwrap();
        for i in 0..8 {
            assert!((x[i] - b[i]).abs() < 1e-9, "分量 {i}: {}", x[i]);
        }
    }

    #[test]
    fn homography_recovers_translation() {
        // 非共线点阵：4×4 网格，DLT 才有唯一解。
        let src: Vec<(f64, f64)> = (0..16)
            .map(|i| (((i % 4) * 23 + 7) as f64, ((i / 4) * 19 + 11) as f64))
            .collect();
        let dst: Vec<(f64, f64)> = src.iter().map(|&(x, y)| (x + 20.0, y - 5.0)).collect();
        let h = estimate_homography(&src, &dst).expect("平移可解");
        let (px, py) = apply_h(h, 50.0, 40.0);
        assert!((px - 70.0).abs() < 0.5, "x: {px}");
        assert!((py - 35.0).abs() < 0.5, "y: {py}");
    }

    #[test]
    fn homography_identity_when_same_points() {
        let pts: Vec<(f64, f64)> = (0..16)
            .map(|i| (((i % 4) * 29 + 3) as f64, ((i / 4) * 31 + 5) as f64))
            .collect();
        let h = estimate_homography(&pts, &pts).unwrap();
        let (px, py) = apply_h(h, 33.0, 77.0);
        assert!(
            (px - 33.0).abs() < 0.5 && (py - 77.0).abs() < 0.5,
            "{px},{py}"
        );
    }

    #[test]
    fn mat3_mul_and_invert_roundtrip() {
        let m = translate(13.0, -7.0);
        let inv = invert3(m).unwrap();
        let id = mat3_mul(m, inv);
        for i in 0..9 {
            assert!((id[i] - IDENTITY[i]).abs() < 1e-9, "分量 {i}: {}", id[i]);
        }
    }

    #[test]
    fn translation_pair_stitches_wider() {
        let w = 64;
        let h = 48;
        // B 是同一世界左移 24 像素取样：与 A 有 40 像素重叠。
        let a = synth(w, h, 0);
        let b = synth(w, h, 24);
        let out = stitch(&[(&a, w as u32, h as u32), (&b, w as u32, h as u32)]).unwrap();
        let (rgb, ow, oh) = out;
        assert_eq!(oh as usize, h, "纯平移不该改变高度");
        // 宽度应接近 w + 24（并集），至少明显大于单张。
        assert!((ow as usize) > w + 8, "拼接后应更宽：ow={ow}, w={w}");
        assert!((ow as usize) <= w + 24 + 4, "宽度不该发散：ow={ow}");
        assert_eq!(rgb.len(), ow as usize * oh as usize * 3);
        for v in &rgb {
            assert!((0.0..=1.0).contains(v), "出界：{v}");
        }
    }

    #[test]
    fn single_frame_returns_itself() {
        let a = synth(32, 24, 0);
        let (rgb, w, h) = stitch(&[(&a, 32, 24)]).unwrap();
        assert_eq!((w, h), (32, 24));
        assert_eq!(rgb.len(), 32 * 24 * 3);
    }

    #[test]
    fn count_limits_speak_chinese() {
        let err = stitch(&[]).unwrap_err();
        assert!(err.to_string().contains("至少要 2 张"), "{err}");
        let blank = synth(16, 16, 0);
        let many: Vec<(&[f32], u32, u32)> = (0..MAX_FRAMES + 1)
            .map(|_| (blank.as_slice(), 16u32, 16u32))
            .collect();
        let err = stitch(&many).unwrap_err();
        assert!(err.to_string().contains("最多"), "{err}");
    }

    #[test]
    fn disjoint_frames_report_no_overlap() {
        // 两张完全不相干的图（不同 shift 且无共同世界区）应报无重叠或尺寸发散。
        let a = synth(48, 32, 0);
        let b = synth(48, 32, 5000);
        let r = stitch(&[(&a, 48, 32), (&b, 48, 32)]);
        match r {
            Err(PanoError::NoOverlap { .. }) | Err(PanoError::CanvasTooBig { .. }) => {}
            other => panic!("不相干图应报错，实得 {other:?}"),
        }
    }

    #[test]
    fn three_frames_chain_wider_than_two() {
        let w = 64;
        let h = 48;
        // 三张依次左移 24：相邻单应连乘成「到基准帧」的映射。
        let a = synth(w, h, 0);
        let b = synth(w, h, 24);
        let c = synth(w, h, 48);
        let two = stitch(&[(&a, w as u32, h as u32), (&b, w as u32, h as u32)]).unwrap();
        let three = stitch(&[
            (&a, w as u32, h as u32),
            (&b, w as u32, h as u32),
            (&c, w as u32, h as u32),
        ])
        .unwrap();
        assert_eq!(three.2 as usize, h, "纯平移高度不变");
        assert!(
            three.1 > two.1,
            "三张应比两张更宽：{} vs {}",
            three.1,
            two.1
        );
        assert!(three.1 as usize <= w + 48 + 6, "宽度不该发散：{}", three.1);
    }

    #[test]
    fn stitch_paths_reads_from_disk() {
        let dir = std::env::temp_dir().join(format!("aevum-pano-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = 64u32;
        let h = 48u32;
        let pa = dir.join("a.png");
        let pb = dir.join("b.png");
        let a = synth(w as usize, h as usize, 0);
        let b = synth(w as usize, h as usize, 24);
        save_rgb(&a, w, h, &pa);
        save_rgb(&b, w, h, &pb);
        let (rgb, ow, oh) = stitch_paths(&[&pa, &pb]).unwrap();
        assert_eq!(oh, h);
        assert!(ow > w, "应更宽：{ow}");
        assert_eq!(rgb.len(), ow as usize * oh as usize * 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn save_rgb(rgb: &[f32], w: u32, h: u32, path: &std::path::Path) {
        let buf: Vec<u8> = rgb
            .iter()
            .map(|v| (v.clamp(0.0, 1.0) * 255.0) as u8)
            .collect();
        image::RgbImage::from_raw(w, h, buf)
            .unwrap()
            .save(path)
            .unwrap();
    }
}
