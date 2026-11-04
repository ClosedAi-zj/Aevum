//! 透视校正（梯形校正）与自动直立。
//!
//! 手动一路：用户圈出画面里本该是矩形的那块四边形（归一化坐标，左上→右上→右下→左下），
//! [`homography`] 用四点 DLT 把四边形映到单位方，[`rectify`] 再按**逆映射**逐像素回查原图，
//! 把四边形拉成真正的矩形。自动一路：[`vertical_lines`] 找出近垂直的主直线，
//! [`auto_upright`] 从其中两条最强线的交点（垂直消失点）反推四边形。
//!
//! Perspective correction (keystone) and auto-upright.
//!
//! Manual path: the user outlines the quadrilateral that should be a rectangle
//! (normalized coordinates, top-left → top-right → bottom-right → bottom-left);
//! [`homography`] maps it onto the unit square with the four-point DLT and [`rectify`]
//! samples the source **inverse-mapped** per output pixel to turn the quad into a real
//! rectangle. Automatic path: [`vertical_lines`] finds near-vertical dominant lines and
//! [`auto_upright`] recovers the quad from the intersection of the two strongest ones
//! (the vertical vanishing point).
//!
//! 为什么必须走逆映射：正向泼溅会把源像素撒到输出上，放大的一侧必然留空洞；逆映射对输出的
//! 每个像素求一次原图坐标，输出网格上一条缝都没有。输出像素 `(i, j)` 采的是单位方
//! `((i+0.5)/out_w, (j+0.5)/out_h)`，所以四边形与输出同为 1:1 时逐位不变。
//!
//! Why the inverse mapping is mandatory: forward splatting scatters source pixels onto the
//! output and inevitably leaves holes on the magnified side; the inverse mapping computes one
//! source coordinate per output pixel, so no gap can appear. Output pixel `(i, j)` samples the
//! unit square at `((i+0.5)/out_w, (j+0.5)/out_h)`, so a 1:1 quad and output are bitwise
//! identical.
//!
//! v1 的边界：自动直立只处理**纯梯形（keystone）**——竖线收敛到一个画面外的垂直消失点。
//! 不修旋转、不修枕形或桶形畸变，也不估画幅比例（校正结果直接铺满 `out_w × out_h`）。
//! 证据不足时一律返回 `None`：画面里没有主直线（平坦或纯噪声）、线少于两条、两条线近乎平行
//! （倾角差 < 0.3°）、消失点落在画面内、消失点离画幅超过 12 个画幅边长、或估算出的四边形与
//! 整幅几乎重合。
//!
//! v1 scope: auto-upright handles **pure keystone only** — vertical lines converging to a
//! vertical vanishing point outside the frame. It does not fix rotation, does not fix
//! pincushion or barrel distortion, and does not estimate aspect (the result simply fills
//! `out_w × out_h`). With insufficient evidence it returns `None`: no dominant line in the frame
//! (flat or pure noise), fewer than two lines, two nearly parallel lines (tilt difference
//! < 0.3°), a vanishing point inside the frame, a vanishing point farther than 12 frame spans
//! away, or an estimated quad that is almost the whole frame.

use std::f64::consts::SQRT_2;

/// 分析代理的最长边：大图先箱式缩到这个尺度再找线，结果坐标仍然是归一化的。
///
/// Longest side of the analysis proxy: large frames are box-averaged down to this size before
/// line finding; result coordinates stay normalized.
const ANALYSIS_MAX: usize = 640;

/// 一次最多返回的竖线条数。
///
/// Maximum number of vertical lines returned at once.
const MAX_LINES: usize = 32;

/// 一条线至少要有这么高（归一化）才会被 `auto_upright` 拿来定消失点。
///
/// A line must be at least this tall (normalized) before `auto_upright` uses it for the
/// vanishing point.
const MIN_PAIR_SPAN: f64 = 0.15;

/// 串线段时允许断开的行数：横线穿过的几行里竖向梯度会被压掉，不是真的断线。
///
/// Number of rows a segment may skip while chaining: a horizontal line suppresses the vertical
/// gradient for a few rows, which is not a real break.
const MAX_GAP: usize = 6;

/// 一对竖线的倾角差下限：再小就是平行，没有消失点可言。
///
/// Lower bound on the tilt difference of a pair: any smaller means parallel, so there is no
/// vanishing point to speak of.
const MIN_PAIR_TILT_DEG: f64 = 0.3;

/// 一对竖线的倾角差上限：差值太大说明不是同一组竖直边。
///
/// Upper bound on the tilt difference of a pair: any larger means they are not the same set of
/// vertical edges.
const MAX_PAIR_TILT_DEG: f64 = 20.0;

/// 最强梯度至少要是全画面平均 |gx| 的这么多倍，才算「有主直线」；否则一律当噪声。
///
/// The strongest gradient must be this many times the frame's mean `|gx|` to count as a dominant
/// line; anything less is treated as noise.
const MIN_PEAK_RATIO: f64 = 6.0;

/// 一对竖线在中段的水平间距下限（归一化）；同一条边两侧的沿离得太近，定不出消失点。
///
/// Lower bound on the horizontal gap of a pair at mid-height (normalized): the two sides of one
/// edge sit too close to pin down a vanishing point.
const MIN_PAIR_GAP: f64 = 0.15;

/// 消失点离画幅的最远距离（以画幅边长为单位）：再远就只剩数值噪声。
///
/// Farthest distance of the vanishing point from the frame (in frame spans): beyond this only
/// numerical noise remains.
const MAX_VP_DIST: f64 = 12.0;

/// 参与配对的最强线条数：再往后的线对比最宽的一对没有意义，也省掉 O(n²) 的尾巴。
///
/// Number of strongest lines that take part in pairing: weaker lines cannot beat the widest
/// pair, and this trims the tail of the O(n²) loop.
const MAX_PAIRS: usize = 12;

/// 单位方四角，顺序与 [`Quad`] 一致。
///
/// The four corners of the unit square, in the same order as [`Quad`].
const UNIT: [(f64, f64); 4] = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];

/// 归一化图像坐标下的四边形，顺序 左上→右上→右下→左下；`x` 向右、`y` 向下，取值 `[0, 1]`。
///
/// A quadrilateral in normalized image coordinates, ordered top-left → top-right →
/// bottom-right → bottom-left; `x` grows to the right, `y` grows downward, values in `[0, 1]`.
///
/// 归一化而不是像素：预览是缩过的、导出是全尺寸，同一份参数要在两边都对得上。角点允许落在
/// `[0, 1]` 之外（`auto_upright` 外推出来的四边形会这样），采样时按边界夹取。
///
/// Normalized rather than pixel-based: the preview is scaled down while export is full size, and
/// the same parameters must line up on both sides. Corners may fall outside `[0, 1]` (quads
/// extrapolated by `auto_upright` do), and sampling clamps at the border.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    /// 四个角，顺序 左上→右上→右下→左下。
    ///
    /// The four corners, ordered top-left → top-right → bottom-right → bottom-left.
    pub p: [(f64, f64); 4],
}

/// 把四边形映到边长为 1 的正方形（单位方）。行主序 3×3，`h[8] == 1.0`。
///
/// Maps the quadrilateral onto the unit square. Row-major 3×3, with `h[8] == 1.0`.
///
/// 标准的四点 DLT：先把源点与单位方各自归一化（平移到质心、缩放到平均距离 `√2`），解 8×8
/// 线性方程组，再反归一化 `H = Td⁻¹ · Hn · Ts`，最后整体除以 `h[8]`。解出来的矩阵会回代四个
/// 角验一遍，误差超过 `1e-6` 就判为退化——三点共线、面积为零、含 NaN 的输入因此一律得到
/// `None`，而不是一个能把画面撕碎的矩阵。
///
/// The standard four-point DLT: source and destination are each normalized (centroid to the
/// origin, mean distance to `√2`), an 8×8 linear system is solved, the result is denormalized as
/// `H = Td⁻¹ · Hn · Ts`, and everything is divided by `h[8]`. The matrix is verified by mapping
/// the four corners back; an error above `1e-6` means the quad is degenerate — three collinear
/// points, zero area, and NaN inputs therefore all yield `None` instead of a matrix that tears
/// the picture apart.
pub fn homography(q: &Quad) -> Option<[f64; 9]> {
    if q.p.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return None;
    }
    if signed_area(&q.p).abs() <= 1e-9 {
        return None;
    }
    let ts = normalize(&q.p)?;
    let td = normalize(&UNIT)?;
    let mut a = [[0.0f64; 8]; 8];
    let mut b = [0.0f64; 8];
    for k in 0..4 {
        let (nx, ny) = apply(&ts, q.p[k].0, q.p[k].1)?;
        let (mx, my) = apply(&td, UNIT[k].0, UNIT[k].1)?;
        // 行 1：[nx ny 1 0 0 0 -mx·nx -mx·ny] · h = mx
        a[2 * k] = [nx, ny, 1.0, 0.0, 0.0, 0.0, -mx * nx, -mx * ny];
        b[2 * k] = mx;
        // 行 2：[0 0 0 nx ny 1 -my·nx -my·ny] · h = my
        a[2 * k + 1] = [0.0, 0.0, 0.0, nx, ny, 1.0, -my * nx, -my * ny];
        b[2 * k + 1] = my;
    }
    let h = solve8(a, b)?;
    let hn = [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0];
    let tdi = invert3(&td)?;
    let mut m = mat3_mul(&mat3_mul(&tdi, &hn), &ts);
    if m.iter().any(|v| !v.is_finite()) || m[8].abs() < 1e-12 {
        return None;
    }
    let s = 1.0 / m[8];
    for v in m.iter_mut() {
        *v *= s;
    }
    m[8] = 1.0;
    // 回代验角：退化输入在解方程那一步未必报错，这一步必须报。
    for k in 0..4 {
        let (u, v) = apply(&m, q.p[k].0, q.p[k].1)?;
        if (u - UNIT[k].0).abs() > 1e-6 || (v - UNIT[k].1).abs() > 1e-6 {
            return None;
        }
    }
    // 视界（分母为零的直线）不能穿过四边形：四角加中心的符号必须一致，否则四边形的
    // 一部分会被翻到无穷远——自交的「蝴蝶结」就在这里被挡掉。
    let cx = q.p.iter().map(|p| p.0).sum::<f64>() / 4.0;
    let cy = q.p.iter().map(|p| p.1).sum::<f64>() / 4.0;
    let mut sign = 0.0f64;
    for (x, y) in q.p.iter().copied().chain(std::iter::once((cx, cy))) {
        let d = m[6] * x + m[7] * y + m[8];
        if !d.is_finite() || d.abs() < 1e-9 {
            return None;
        }
        if sign == 0.0 {
            sign = d.signum();
        } else if d.signum() != sign {
            return None;
        }
    }
    Some(m)
}

/// 反向映射采样：把四边形拉直成 `out_w × out_h` 的画面。
///
/// Inverse-mapped sampling: straightens the quadrilateral into an `out_w × out_h` picture.
///
/// 输出像素 `(i, j)` 对应单位方的 `((i+0.5)/out_w, (j+0.5)/out_h)`，经 `H⁻¹` 回查到归一化的
/// 原图坐标，再换算成像素坐标做双线性采样。边界按 reflect101 折回画内，与 `warp` 的口径
/// 一致。四边形退化、输出尺寸为零、输入缓冲区不够长时返回空 `Vec`，不 panic；回查落不到有限
/// 坐标的个别像素留 0（黑）。
///
/// Output pixel `(i, j)` maps to `((i+0.5)/out_w, (j+0.5)/out_h)` on the unit square, is pulled
/// back through `H⁻¹` into normalized source coordinates, and is bilinearly sampled there.
/// Borders fold back with reflect101, matching `warp`. A degenerate quad, a zero output size, or
/// a short input buffer yields an empty `Vec` and never panics; the few pixels whose pre-image is
/// not finite stay 0 (black).
pub fn rectify(rgb: &[f32], w: usize, h: usize, q: &Quad, out_w: usize, out_h: usize) -> Vec<f32> {
    if w == 0 || h == 0 || out_w == 0 || out_h == 0 || rgb.len() < w * h * 3 {
        return Vec::new();
    }
    let Some(hm) = homography(q) else {
        return Vec::new();
    };
    // 1:1 且没有任何动作：直接抄，连一次插值都不做（与 `warp::transform_into` 的快路同口径）。
    if q.p == UNIT && out_w == w && out_h == h {
        return rgb[..w * h * 3].to_vec();
    }
    let Some(hinv) = invert3(&hm) else {
        return Vec::new();
    };
    let mut out = vec![0.0f32; out_w * out_h * 3];
    let (fw, fh) = (w as f64, h as f64);
    for j in 0..out_h {
        let v = (j as f64 + 0.5) / out_h as f64;
        for i in 0..out_w {
            let u = (i as f64 + 0.5) / out_w as f64;
            let Some((x, y)) = apply(&hinv, u, v) else {
                continue;
            };
            let sx = x * fw - 0.5;
            let sy = y * fh - 0.5;
            let (x0, x1, tx) = border_pair(sx, w);
            let (y0, y1, ty) = border_pair(sy, h);
            let o = (j * out_w + i) * 3;
            for c in 0..3 {
                let p00 = rgb[(y0 * w + x0) * 3 + c];
                let p10 = rgb[(y0 * w + x1) * 3 + c];
                let p01 = rgb[(y1 * w + x0) * 3 + c];
                let p11 = rgb[(y1 * w + x1) * 3 + c];
                let top = p00 + (p10 - p00) * tx;
                let bottom = p01 + (p11 - p01) * tx;
                out[o + c] = top + (bottom - top) * ty;
            }
        }
    }
    out
}

/// 检测画面里「接近垂直」的主直线（给自动直立用）：返回 `(x0, y0, x1, y1)` 归一化坐标。
///
/// Detects the dominant near-vertical lines (for auto-upright): returns `(x0, y0, x1, y1)` in
/// normalized coordinates.
///
/// 先箱式缩成分析代理（最长边 [`ANALYSIS_MAX`]），求 Rec.601 亮度，再用 Sobel 梯度筛出
/// `|gx| > 2·|gy|` 的像素，在 `|gx|` 的极大值处做抛物线插值得到逐行亚像素峰位，按上下相邻
/// 关系串成线段（横线穿过的几行允许断开再接），最后用加权最小二乘拟合 `x = a·y + b`（竖线
/// 用 `x` 对 `y` 回归，不会碰上斜率无穷）。每段按 `长度 × 平均梯度` 排序，最多返回
/// [`MAX_LINES`] 条；两端 `y0 < y1`。一条线的两条沿各自算一条线——它们过同一个消失点，
/// [`auto_upright`] 会按间距把同一对沿排除。
///
/// The frame is box-averaged into an analysis proxy (longest side [`ANALYSIS_MAX`]), converted to
/// Rec.601 luma, and pixels with `|gx| > 2·|gy|` are kept from the Sobel gradients. Peaks of
/// `|gx|` are parabolically interpolated into per-row sub-pixel positions, chained into segments
/// across adjacent rows (a few rows may be skipped where a horizontal line crosses), and fitted
/// with weighted least squares as `x = a·y + b` (regressing `x` on `y` never hits an infinite
/// slope). Segments are ordered by `length × mean gradient`, at most [`MAX_LINES`] are returned,
/// and `y0 < y1` at both ends. The two sides of one line each count as a line — both pass through
/// the same vanishing point, and [`auto_upright`] rejects such a pair by its gap.
pub fn vertical_lines(rgb: &[f32], w: usize, h: usize) -> Vec<(f64, f64, f64, f64)> {
    if w < 8 || h < 8 || rgb.len() < w * h * 3 {
        return Vec::new();
    }
    let (gray, gw, gh, bs) = gray_proxy(rgb, w, h);
    let (gx, gy) = sobel_gradient(&gray, gw, gh);
    let mut peak = 0.0f32;
    let mut mean = 0.0f64;
    for p in 0..gw * gh {
        peak = peak.max(gx[p].abs());
        mean += gx[p].abs() as f64;
    }
    mean /= (gw * gh) as f64;
    // 门限跟着画面自己的最强梯度走：白墙上的一条浅缝也能留住。但「最强」必须明显强过
    // 全画面的平均水平，否则就是一片噪声——每一行都有碰巧够强的峰，串出来的线毫无意义。
    if peak <= 1e-3 || (peak as f64) < mean * MIN_PEAK_RATIO {
        return Vec::new();
    }
    let thr = (peak * 0.12).max(1e-4);
    let mut segs: Vec<Seg> = Vec::new();
    let mut active: Vec<usize> = Vec::new();
    for y in 1..gh - 1 {
        let peaks = peaks_of_row(&gx, &gy, gw, y, thr);
        active.retain(|&i| y - segs[i].y1 <= MAX_GAP);
        for (x, weight, pos) in peaks {
            let mut best: Option<(usize, f64)> = None;
            for &si in &active {
                let s = &segs[si];
                // 横线穿过的几行里竖向梯度会被压下去，允许断几行再接上；同一行只接一段，
                // 免得两条平行边被串进同一条线段。极性翻转的不是同一条边（噪声多半如此）。
                if s.y1 >= y || s.pos != pos {
                    continue;
                }
                let d = (s.predict(y) - x).abs();
                if d > 1.5 + 0.5 * (y - s.y1) as f64 {
                    continue;
                }
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((si, d));
                }
            }
            let si = match best {
                Some((i, _)) => i,
                None => {
                    segs.push(Seg {
                        y0: y,
                        y1: y,
                        cx: x,
                        pos,
                        pts: Vec::new(),
                    });
                    active.push(segs.len() - 1);
                    segs.len() - 1
                }
            };
            let s = &mut segs[si];
            s.y1 = y;
            s.cx = x;
            s.pts.push((y as f64, x, weight));
        }
    }

    let min_rows = ((gh as f64 * 0.10).round() as usize).max(6);
    let mut found: Vec<(f64, f64, f64, f64, f64)> = Vec::new();
    for s in &segs {
        if s.y1 + 1 - s.y0 < min_rows || s.pts.len() < min_rows {
            continue;
        }
        let Some((a, b)) = fit_line(&s.pts) else {
            continue;
        };
        // |dx/dy| > 1 就是 45° 以外，那已经不叫「接近垂直」了。
        if !a.is_finite() || !b.is_finite() || a.abs() > 1.0 {
            continue;
        }
        let score: f64 = s.pts.iter().map(|p| p.2).sum();
        let (y0, y1) = (s.y0 as f64, s.y1 as f64);
        let nx = |x: f64| (x + 0.5) * bs as f64 / w as f64;
        let ny = |y: f64| (y + 0.5) * bs as f64 / h as f64;
        found.push((nx(a * y0 + b), ny(y0), nx(a * y1 + b), ny(y1), score));
    }
    found.sort_by(|a, b| {
        b.4.total_cmp(&a.4)
            .then_with(|| b.3.total_cmp(&a.3))
            .then_with(|| a.0.total_cmp(&b.0))
    });
    found.truncate(MAX_LINES);
    found
        .into_iter()
        .map(|(x0, y0, x1, y1, _)| (x0, y0, x1, y1))
        .collect()
}

/// 自动直立：从两组主方向的直线估计校正四边形；证据不足返回 `None`。
///
/// Auto-upright: estimates the correcting quadrilateral from two dominant line directions;
/// returns `None` when the evidence is insufficient.
///
/// v1 只处理**纯梯形**：取两条足够长、倾角相近（差 0.3°～20°）且在中段有明显间距的竖直线，
/// 求它们的交点作为垂直消失点；消失点必须在画面外、且离画幅不超过 [`MAX_VP_DIST`]，再由两条线
/// 外推到画面上下边界得到四边形。所有可行的线对里取中段间距最宽的一对：覆盖最全，消失点也最
/// 稳。线少于两条、两条线近乎平行、消失点落在画面内、消失点远得只剩噪声、或估出来的四边形
/// 几乎就是整幅时返回 `None`——宁可不动作，也不瞎猜。旋转、枕形畸变、画幅比例都不在 v1 的范围。
///
/// v1 handles **pure keystone** only: two long enough, similarly tilted (0.3°–20° apart)
/// vertical lines with a clear mid-height gap are intersected to get the vertical vanishing point;
/// it must lie outside the frame and within [`MAX_VP_DIST`] of it, and the two lines are
/// extrapolated to the top and bottom borders to form the quad. Among the valid pairs the one with
/// the widest mid-height gap wins: it covers the most and pins the vanishing point most stably. It
/// returns `None` with fewer than two lines, two nearly parallel lines, a vanishing point inside
/// the frame, one so far away that only noise is left, or a quad that is nearly the whole frame —
/// better to do nothing than to guess. Rotation, pincushion distortion, and aspect are out of
/// scope for v1.
pub fn auto_upright(rgb: &[f32], w: usize, h: usize) -> Option<Quad> {
    let lines = vertical_lines(rgb, w, h);
    if lines.len() < 2 {
        return None;
    }
    let cand: Vec<Line> = lines
        .iter()
        .map(|&(x0, y0, x1, y1)| Line { x0, y0, x1, y1 })
        .filter(|l| l.span() >= MIN_PAIR_SPAN)
        .take(MAX_PAIRS)
        .collect();
    if cand.len() < 2 {
        return None;
    }
    let mut best: Option<(f64, Quad)> = None;
    for i in 0..cand.len() {
        for j in (i + 1)..cand.len() {
            let Some((q, gap)) = quad_from_pair(cand[i], cand[j]) else {
                continue;
            };
            if best.is_none_or(|(g, _)| gap > g) {
                best = Some((gap, q));
            }
        }
    }
    best.map(|(_, q)| q)
}

// ------------------------------------------------------------------ 几何小工具

/// 一条检测出来的线段，保证 `y0 < y1`。
///
/// A detected line segment, guaranteed to have `y0 < y1`.
#[derive(Clone, Copy)]
struct Line {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Line {
    /// 线在给定 `y` 处的 `x`（`y0 != y1` 由构造保证）。
    ///
    /// The `x` of the line at a given `y` (`y0 != y1` is guaranteed by construction).
    fn x_at(self, y: f64) -> f64 {
        self.x0 + (y - self.y0) * (self.x1 - self.x0) / (self.y1 - self.y0)
    }

    /// 竖直方向的跨度。
    ///
    /// The vertical span.
    fn span(self) -> f64 {
        self.y1 - self.y0
    }
}

/// 两条竖线 → 四边形：交点即垂直消失点，两条线直接当四边形的左右边；同时回报中段间距。
///
/// Two vertical lines → a quad: the intersection is the vertical vanishing point and the lines
/// themselves become the left and right sides; the mid-height gap is reported as well.
fn quad_from_pair(a: Line, b: Line) -> Option<(Quad, f64)> {
    let (adx, ady) = (a.x1 - a.x0, a.y1 - a.y0);
    let (bdx, bdy) = (b.x1 - b.x0, b.y1 - b.y0);
    if ady <= 1e-6 || bdy <= 1e-6 {
        return None;
    }
    let (ta, tb) = ((adx / ady).atan(), (bdx / bdy).atan());
    let diff = (ta - tb).abs().to_degrees();
    if !(MIN_PAIR_TILT_DEG..=MAX_PAIR_TILT_DEG).contains(&diff) {
        return None;
    }
    let (am, bm) = (a.x_at(0.5), b.x_at(0.5));
    let gap = (am - bm).abs();
    if gap < MIN_PAIR_GAP {
        return None;
    }
    // a: P + t·d，b: Q + s·e；交点就是消失点。
    let den = adx * bdy - ady * bdx;
    if den.abs() < 1e-9 {
        return None;
    }
    let t = ((b.x0 - a.x0) * bdy - (b.y0 - a.y0) * bdx) / den;
    let (vx, vy) = (a.x0 + t * adx, a.y0 + t * ady);
    if !vx.is_finite() || !vy.is_finite() {
        return None;
    }
    // 消失点落在画面里就不是梯形：那两条线是在画内真的相交了。
    if (-0.02..=1.02).contains(&vx) && (-0.02..=1.02).contains(&vy) {
        return None;
    }
    if vx.abs() > MAX_VP_DIST || vy.abs() > MAX_VP_DIST {
        return None;
    }
    let (l, r) = if am < bm { (a, b) } else { (b, a) };
    let (l0, l1) = (l.x_at(0.0), l.x_at(1.0));
    let (r0, r1) = (r.x_at(0.0), r.x_at(1.0));
    if !(l0 < r0 && l1 < r1) {
        return None;
    }
    // 外推到画幅外的部分要有边：超过两倍画幅宽就是线段噪声放大出来的假消失点。
    for v in [l0, r0, l1, r1] {
        if !(-1.0..=2.0).contains(&v) {
            return None;
        }
    }
    let (top, bottom) = (r0 - l0, r1 - l1);
    if top < 0.02 || bottom < 0.02 {
        return None;
    }
    let ratio = top / bottom;
    if !(0.1..=10.0).contains(&ratio) {
        return None;
    }
    let q = Quad {
        p: [(l0, 0.0), (r0, 0.0), (r1, 1.0), (l1, 1.0)],
    };
    // 几乎就是整幅：本来就没斜，别让一次重采样白白掉一档清晰度。
    if q.p
        .iter()
        .zip(UNIT)
        .all(|(p, u)| (p.0 - u.0).abs() < 2e-3 && (p.1 - u.1).abs() < 2e-3)
    {
        return None;
    }
    Some((q, gap))
}

/// 带符号面积（鞋带公式），正负只反映绕向。
///
/// Signed area (shoelace); the sign only reflects the winding.
fn signed_area(p: &[(f64, f64); 4]) -> f64 {
    let mut s = 0.0;
    for k in 0..4 {
        let (x0, y0) = p[k];
        let (x1, y1) = p[(k + 1) % 4];
        s += x0 * y1 - x1 * y0;
    }
    s / 2.0
}

/// 归一化变换：质心移到原点、平均距离缩到 `√2`。
///
/// Normalizing transform: centroid to the origin, mean distance to `√2`.
fn normalize(pts: &[(f64, f64); 4]) -> Option<[f64; 9]> {
    let (mut cx, mut cy) = (0.0, 0.0);
    for p in pts {
        cx += p.0;
        cy += p.1;
    }
    cx /= 4.0;
    cy /= 4.0;
    let mut mean = 0.0;
    for p in pts {
        mean += ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt();
    }
    mean /= 4.0;
    if mean.is_nan() || mean <= 1e-9 {
        return None;
    }
    let s = SQRT_2 / mean;
    Some([s, 0.0, -s * cx, 0.0, s, -s * cy, 0.0, 0.0, 1.0])
}

/// 3×3 矩阵作用在点上；分母非有限或贴近零时返回 `None`（该点映到无穷远）。
///
/// Applies a 3×3 matrix to a point; returns `None` when the denominator is not finite or is
/// near zero (the point maps to infinity).
fn apply(m: &[f64; 9], x: f64, y: f64) -> Option<(f64, f64)> {
    let d = m[6] * x + m[7] * y + m[8];
    if !d.is_finite() || d.abs() < 1e-12 {
        return None;
    }
    let u = (m[0] * x + m[1] * y + m[2]) / d;
    let v = (m[3] * x + m[4] * y + m[5]) / d;
    if u.is_finite() && v.is_finite() {
        Some((u, v))
    } else {
        None
    }
}

/// 3×3 矩阵乘法，行主序。
///
/// 3×3 matrix multiplication, row-major.
fn mat3_mul(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut m = [0.0f64; 9];
    for r in 0..3 {
        for c in 0..3 {
            let mut s = 0.0;
            for k in 0..3 {
                s += a[r * 3 + k] * b[k * 3 + c];
            }
            m[r * 3 + c] = s;
        }
    }
    m
}

/// 3×3 求逆（伴随矩阵法）；行列式为零或非有限时返回 `None`。
///
/// 3×3 inverse (adjugate); returns `None` when the determinant is zero or not finite.
fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let (a, b, c) = (m[0], m[1], m[2]);
    let (d, e, f) = (m[3], m[4], m[5]);
    let (g, h, i) = (m[6], m[7], m[8]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let s = 1.0 / det;
    Some([
        (e * i - f * h) * s,
        (c * h - b * i) * s,
        (b * f - c * e) * s,
        (f * g - d * i) * s,
        (a * i - c * g) * s,
        (c * d - a * f) * s,
        (d * h - e * g) * s,
        (b * g - a * h) * s,
        (a * e - b * d) * s,
    ])
}

/// 8×8 高斯消元（列主元），解 `A x = b`；主元相对过小即判退化。
///
/// 8×8 Gaussian elimination with partial pivoting, solving `A x = b`; a relatively tiny pivot
/// means the system is degenerate.
fn solve8(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    let mut scale = 0.0f64;
    for row in &a {
        for v in row {
            scale = scale.max(v.abs());
        }
    }
    if scale <= 0.0 {
        return None;
    }
    let tiny = scale * 1e-14;
    for col in 0..8 {
        let mut piv = col;
        for r in (col + 1)..8 {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() <= tiny {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        let d = a[col][col];
        for r in (col + 1)..8 {
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
        for c in (r + 1)..8 {
            s -= a[r][c] * x[c];
        }
        let d = a[r][r];
        if d == 0.0 {
            return None;
        }
        x[r] = s / d;
    }
    if x.iter().all(|v| v.is_finite()) {
        Some(x)
    } else {
        None
    }
}

// ------------------------------------------------------------------ 采样

/// 双线性采样的下标与权重；越界按 reflect101 折回画内，与 `warp` 同口径。
///
/// Indices and weight for bilinear sampling; out-of-range values fold back with reflect101, the
/// same convention as `warp`.
fn border_pair(v: f64, n: usize) -> (usize, usize, f32) {
    let ni = n as isize;
    // 逆映射在外推得很远时会给出天文数字：先夹到几倍画幅以内，反射循环才不会跑上亿次。
    let lim = (ni * 4 + 8) as f64;
    let v = if v.is_finite() {
        v.clamp(-lim, lim)
    } else {
        0.0
    };
    let f = v.floor();
    let t = (v - f) as f32;
    (
        reflect101(f as isize, ni),
        reflect101(f as isize + 1, ni),
        t,
    )
}

/// 边界反射（不含边界本身的那一格）：`... 2 1 0 1 2 ...`。
///
/// Border reflection (excluding the edge pixel itself): `... 2 1 0 1 2 ...`.
fn reflect101(mut i: isize, n: isize) -> usize {
    if n <= 1 {
        return 0;
    }
    while i < 0 || i >= n {
        if i < 0 {
            i = -i;
        } else {
            i = 2 * (n - 1) - i;
        }
    }
    i as usize
}

// ------------------------------------------------------------------ 找线

/// 一个分析代理里的连续段：逐行串起来的一条竖直边。
///
/// A run chained across rows inside the analysis proxy: one vertical edge.
struct Seg {
    y0: usize,
    y1: usize,
    /// 上一行的亚像素中心，串线时用来判「是不是同一条」。
    ///
    /// Sub-pixel center on the last row, used to decide whether the next row continues the line.
    cx: f64,
    /// 梯度方向（`gx > 0` 为真）。同一条边的极性不会变，噪声会——串线时拿它挡噪声。
    ///
    /// Gradient polarity (`gx > 0`). One edge keeps its polarity while noise does not; chaining
    /// uses it to reject noise.
    pos: bool,
    /// `(行号, 亚像素中心, 权重)`。
    ///
    /// `(row, sub-pixel center, weight)`.
    pts: Vec<(f64, f64, f64)>,
}

impl Seg {
    /// 按最后两行外推本行的中心：串线时用它当参照，免得被旁边那条平行边拽走。
    ///
    /// Extrapolates this row's center from the last two points: used as the chaining reference so
    /// a parallel line next door cannot pull the track away.
    fn predict(&self, y: usize) -> f64 {
        let n = self.pts.len();
        if n >= 2 {
            let (y1, x1, _) = self.pts[n - 1];
            let (y0, x0, _) = self.pts[n - 2];
            if y1 > y0 {
                return x1 + (x1 - x0) / (y1 - y0) * (y as f64 - y1);
            }
        }
        self.cx
    }
}

/// 亮度代理：按整数块做箱式平均，把最长边压到 [`ANALYSIS_MAX`] 以内。
///
/// Luma proxy: integer-size box averaging that shrinks the longest side to [`ANALYSIS_MAX`].
fn gray_proxy(rgb: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize, usize) {
    let bs = w.max(h).div_ceil(ANALYSIS_MAX).max(1);
    let gw = w.div_ceil(bs);
    let gh = h.div_ceil(bs);
    let mut g = vec![0.0f32; gw * gh];
    for py in 0..gh {
        let y0 = py * bs;
        let y1 = (y0 + bs).min(h);
        for px in 0..gw {
            let x0 = px * bs;
            let x1 = (x0 + bs).min(w);
            let mut sum = 0.0f64;
            let mut n = 0usize;
            for y in y0..y1 {
                let row = y * w;
                for x in x0..x1 {
                    let o = (row + x) * 3;
                    sum += 0.299 * rgb[o] as f64
                        + 0.587 * rgb[o + 1] as f64
                        + 0.114 * rgb[o + 2] as f64;
                    n += 1;
                }
            }
            g[py * gw + px] = if n > 0 { (sum / n as f64) as f32 } else { 0.0 };
        }
    }
    (g, gw, gh, bs)
}

/// Sobel 3×3 梯度（跳过最外一圈，越界值不参与）。
///
/// Sobel 3×3 gradients (the outer ring is skipped; out-of-range values never enter).
fn sobel_gradient(g: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut gx = vec![0.0f32; w * h];
    let mut gy = vec![0.0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let p = y * w + x;
            let tl = g[p - w - 1];
            let tc = g[p - w];
            let tr = g[p - w + 1];
            let ml = g[p - 1];
            let mr = g[p + 1];
            let bl = g[p + w - 1];
            let bc = g[p + w];
            let br = g[p + w + 1];
            gx[p] = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
            gy[p] = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
        }
    }
    (gx, gy)
}

/// 一行的近垂直边：`(亚像素中心, 峰强, 梯度方向)`。
///
/// Near-vertical edges of one row: `(sub-pixel center, peak strength, gradient polarity)`.
///
/// 逐像素的 `|gx|` 在极大值处用抛物线插值出亚像素峰位。一条线的两侧沿各自是一条线——它们
/// 都过同一个消失点，`auto_upright` 会按间距把「同一条线的两条沿」排除在线对之外，所以这里
/// 不做合并：合并需要一个宽度门限，而门限正好卡在线的宽度上时，逐行的中心会在两条沿之间跳。
///
/// The per-pixel `|gx|` is parabolically interpolated at its maxima for a sub-pixel peak. The two
/// sides of one line are two lines of their own — both pass through the same vanishing point, and
/// `auto_upright` rejects "the two sides of one line" as a pair by their gap, so nothing is merged
/// here: merging needs a width threshold, and when a line's width straddles that threshold the
/// per-row center jumps between the two sides.
fn peaks_of_row(gx: &[f32], gy: &[f32], w: usize, y: usize, thr: f32) -> Vec<(f64, f64, bool)> {
    let row = y * w;
    let mut out: Vec<(f64, f64, bool)> = Vec::new();
    for x in 2..w - 2 {
        let p = row + x;
        let c = gx[p].abs();
        if c <= thr || c <= 2.0 * gy[p].abs() {
            continue;
        }
        let a = gx[p - 1].abs() as f64;
        let b = gx[p + 1].abs() as f64;
        let c = c as f64;
        // 平台只取最右一格（`>=` 在左、`>` 在右），一行不会给同一个峰发两张票。
        if c < a || c <= b {
            continue;
        }
        let den = a - 2.0 * c + b;
        let delta = if den < -1e-9 {
            (0.5 * (a - b) / den).clamp(-0.75, 0.75)
        } else {
            0.0
        };
        out.push((x as f64 + delta, c, gx[p] > 0.0));
    }
    out
}

/// 加权最小二乘拟合 `x = a·y + b`，剔掉一轮 >2 px 的离群点后重拟合。
///
/// Weighted least squares fit of `x = a·y + b`, with one pass that drops outliers beyond 2 px
/// before refitting.
fn fit_line(pts: &[(f64, f64, f64)]) -> Option<(f64, f64)> {
    let fit = |ps: &[(f64, f64, f64)]| -> Option<(f64, f64)> {
        let (mut sw, mut sy, mut sx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &(y, x, w) in ps {
            sw += w;
            sy += w * y;
            sx += w * x;
            syy += w * y * y;
            sxy += w * x * y;
        }
        let den = sw * syy - sy * sy;
        if !den.is_finite() || den.abs() <= 1e-12 || sw <= 0.0 {
            return None;
        }
        let a = (sw * sxy - sy * sx) / den;
        let b = (sx - a * sy) / sw;
        if a.is_finite() && b.is_finite() {
            Some((a, b))
        } else {
            None
        }
    };
    let (a, b) = fit(pts)?;
    let keep: Vec<(f64, f64, f64)> = pts
        .iter()
        .copied()
        .filter(|&(y, x, _)| (x - (a * y + b)).abs() <= 2.0)
        .collect();
    if keep.len() < 6 || keep.len() * 2 < pts.len() {
        return None;
    }
    fit(&keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(p: [(f64, f64); 4]) -> Quad {
        Quad { p }
    }

    fn unit() -> Quad {
        quad(UNIT)
    }

    fn gray_image(w: usize, h: usize, f: impl Fn(f64, f64) -> f32) -> Vec<f32> {
        let mut v = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let g = f(x as f64 + 0.5, y as f64 + 0.5);
                let o = (y * w + x) * 3;
                v[o] = g;
                v[o + 1] = g;
                v[o + 2] = g;
            }
        }
        v
    }

    /// 点到线段的距离，画抗锯齿细线用。
    fn dist_to_segment(px: f64, py: f64, x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (cx, cy) = (x0 + t * dx, y0 + t * dy);
        (px - cx).hypot(py - cy)
    }

    #[test]
    fn identity_quad_is_bitwise_identity() {
        let (w, h) = (37usize, 23usize);
        let src: Vec<f32> = (0..w * h * 3)
            .map(|i| ((i * 37 % 101) as f32) / 101.0 - 0.5)
            .collect();
        let out = rectify(&src, w, h, &unit(), w, h);
        assert_eq!(out.len(), src.len());
        assert_eq!(out, src, "1:1 的单位四边形不能动一个比特");
        // 输出尺寸不同也照样能跑，而且不 panic。
        let small = rectify(&src, w, h, &unit(), 11, 7);
        assert_eq!(small.len(), 11 * 7 * 3);
    }

    #[test]
    fn homography_maps_the_four_corners_exactly() {
        let q = quad([(0.11, 0.07), (0.93, 0.04), (0.97, 0.92), (0.03, 0.95)]);
        let m = homography(&q).expect("正常梯形可解");
        assert_eq!(m[8], 1.0);
        for k in 0..4 {
            let (u, v) = apply(&m, q.p[k].0, q.p[k].1).expect("分母非零");
            assert!((u - UNIT[k].0).abs() < 1e-9, "第 {k} 角 u={u}");
            assert!((v - UNIT[k].1).abs() < 1e-9, "第 {k} 角 v={v}");
        }
    }

    #[test]
    fn homography_round_trip_is_identity() {
        for q in [
            unit(),
            quad([(0.2, 0.1), (0.8, 0.05), (0.95, 0.9), (0.05, 0.85)]),
            quad([(0.3, 0.3), (0.7, 0.25), (0.75, 0.7), (0.25, 0.8)]),
        ] {
            let m = homography(&q).expect("可解");
            let inv = invert3(&m).expect("可逆");
            let p = mat3_mul(&m, &inv);
            for r in 0..3 {
                for c in 0..3 {
                    let want = if r == c { 1.0 } else { 0.0 };
                    assert!(
                        (p[r * 3 + c] - want).abs() < 1e-9,
                        "H·H⁻¹ 的 ({r},{c}) = {}",
                        p[r * 3 + c]
                    );
                }
            }
        }
    }

    #[test]
    fn straight_lines_stay_straight() {
        let (w, h) = (200usize, 200usize);
        let q = quad([(0.10, 0.05), (0.92, 0.03), (0.98, 0.95), (0.02, 0.93)]);
        // 源图：白底上一条 1 px 宽、带抗锯齿的斜线（连续像素坐标，格心在 i+0.5）。
        let seg = (40.0f64, 24.0, 160.0, 176.0);
        let src = gray_image(w, h, |x, y| {
            let d = dist_to_segment(x, y, seg.0, seg.1, seg.2, seg.3);
            (1.0 - 0.9 * (1.0 - d).clamp(0.0, 1.0)) as f32
        });
        let out = rectify(&src, w, h, &q, w, h);
        assert_eq!(out.len(), w * h * 3);

        // 这条线在校正图上的解析位置：源端点经 H 投过去（连续坐标）。
        let m = homography(&q).expect("可解");
        let map = |x: f64, y: f64| -> (f64, f64) {
            let (u, v) = apply(&m, x / w as f64, y / h as f64).expect("在四边形里");
            (u * w as f64, v * h as f64)
        };
        let (ax, ay) = map(seg.0, seg.1);
        let (bx, by) = map(seg.2, seg.3);
        assert!(
            (0.35..=2.8).contains(&((by - ay) / (bx - ax))),
            "测试前提：校正后这条线仍然是斜的"
        );

        let mut pts = Vec::new();
        let x_from = ax.max(4.0) + 6.0;
        let x_to = (bx - 6.0).min(w as f64 - 4.0);
        let mut x = x_from;
        while x < x_to {
            let t = (x - ax) / (bx - ax);
            let expected = ay + t * (by - ay);
            let (mut num, mut den) = (0.0f64, 0.0f64);
            let col = (x - 0.5).round() as usize;
            let y0 = ((expected - 8.0).max(1.0)) as usize;
            let y1 = ((expected + 8.0).min(h as f64 - 1.0)) as usize;
            for row in y0..=y1 {
                let g = out[(row * w + col) * 3] as f64;
                let dark = 1.0 - g;
                if dark > 0.05 {
                    num += dark * (row as f64 + 0.5);
                    den += dark;
                }
            }
            if den > 0.2 {
                pts.push((x, num / den));
            }
            x += 3.0;
        }
        assert!(pts.len() > 30, "取样点太少：{}", pts.len());

        // 最小二乘 y = a x + b，然后算竖向残差；线大约 45°，竖向残差是垂直残差的 √2 倍。
        let n = pts.len() as f64;
        let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
        for &(x, y) in &pts {
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let den = n * sxx - sx * sx;
        let a = (n * sxy - sx * sy) / den;
        let b = (sy - a * sx) / n;
        let mut ss = 0.0;
        for &(x, y) in &pts {
            let r = y - (a * x + b);
            ss += r * r;
        }
        let rms = (ss / n).sqrt();
        assert!(rms < 0.5, "校正后直线不直：残差 {rms} px");
    }

    #[test]
    fn rectify_is_deterministic() {
        let (w, h) = (64usize, 48usize);
        let src = gray_image(w, h, |x, y| ((x + y) * 0.01).sin() as f32 * 0.5 + 0.5);
        let q = quad([(0.15, 0.1), (0.9, 0.06), (0.94, 0.9), (0.06, 0.94)]);
        let a = rectify(&src, w, h, &q, 53, 41);
        let b = rectify(&src, w, h, &q, 53, 41);
        assert_eq!(a.len(), 53 * 41 * 3);
        assert_eq!(a, b);
    }

    #[test]
    fn degenerate_quads_are_refused() {
        // 三点共线：单位方三只角也不共线，无解。
        let collinear = quad([(0.2, 0.2), (0.4, 0.4), (0.6, 0.6), (0.8, 0.1)]);
        // 四点全在一条横线上：面积为零。
        let flat = quad([(0.2, 0.4), (0.3, 0.4), (0.4, 0.4), (0.5, 0.4)]);
        // 一个 NaN。
        let nan = quad([(0.0, 0.0), (f64::NAN, 0.0), (1.0, 1.0), (0.0, 1.0)]);
        // 蝴蝶结：四个角分别落在两条对角线上，视界穿过内部。
        let bowtie = quad([(0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 1.0)]);

        for q in [collinear, flat, nan, bowtie] {
            assert!(homography(&q).is_none(), "退化四边形不该有解：{q:?}");
        }

        let src = vec![0.5f32; 32 * 32 * 3];
        for q in [collinear, flat, nan, bowtie] {
            let out = rectify(&src, 32, 32, &q, 16, 16);
            assert!(out.is_empty(), "退化四边形不该写出像素");
        }
        // 输出尺寸为零、缓冲区不够长：空结果，不 panic。
        assert!(rectify(&src, 32, 32, &unit(), 0, 16).is_empty());
        assert!(rectify(&src, 32, 32, &unit(), 16, 0).is_empty());
        assert!(rectify(&src[..7], 32, 32, &unit(), 16, 16).is_empty());
        assert!(rectify(&src, 0, 0, &unit(), 16, 16).is_empty());
    }

    #[test]
    fn vertical_lines_finds_the_stripes() {
        let (w, h) = (320usize, 240usize);
        let stripes = [40.0f64, 120.0, 200.0, 280.0];
        let src = gray_image(w, h, |x, _| {
            let d = stripes.iter().fold(1.0f64, |acc, s| acc.min((x - s).abs()));
            (1.0 - (1.0 - d).clamp(0.0, 1.0)) as f32
        });
        let lines = vertical_lines(&src, w, h);
        assert!(lines.len() >= 4, "只找到 {} 条线", lines.len());
        assert!(lines.len() <= MAX_LINES);
        for &(x0, y0, x1, y1) in &lines {
            assert!(y0 < y1, "端点顺序反了");
            assert!((0.0..=1.0).contains(&x0) && (0.0..=1.0).contains(&x1));
            assert!((y1 - y0) > 0.5, "竖线该贯穿画面");
            assert!(
                ((x1 - x0) / (y1 - y0)).abs() < 0.05,
                "竖线被拟合歪了：dx={} dy={}",
                x1 - x0,
                y1 - y0
            );
        }
    }

    #[test]
    fn auto_upright_refuses_parallel_verticals() {
        // 只是把画面转了个角度：所有竖线依然彼此平行（倾角差 ≈ 0），没有消失点可估。
        // v1 不修旋转，宁可返回 None，也不拿一条歪掉的矩形硬套。
        let (w, h) = (240usize, 240usize);
        let upright = gray_image(w, h, |x, _| {
            let d = [40.0f64, 100.0, 160.0, 220.0]
                .iter()
                .fold(1.0f64, |acc, s| acc.min((x - s).abs()));
            (1.0 - (1.0 - d).clamp(0.0, 1.0)) as f32
        });
        let angle = 6.0f64.to_radians();
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let rotated = gray_image(w, h, |x, y| {
            let (dx, dy) = (x - cx, y - cy);
            let sx = cx + angle.cos() * dx - angle.sin() * dy;
            let sy = cy + angle.sin() * dx + angle.cos() * dy;
            let xi = sx.floor().clamp(0.0, w as f64 - 1.0) as usize;
            let yi = sy.floor().clamp(0.0, h as f64 - 1.0) as usize;
            upright[(yi * w + xi) * 3]
        });
        assert!(
            vertical_lines(&rotated, w, h).len() >= 2,
            "斜线本身仍该找得到"
        );
        assert!(
            auto_upright(&rotated, w, h).is_none(),
            "平行竖线不许硬估出四边形"
        );
    }

    #[test]
    fn auto_upright_recovers_a_known_keystone() {
        let (w, h) = (240usize, 240usize);
        // 已知的梯形：上窄下宽，这就是相机拍到的那块「本该是矩形」的画面。
        let trap = quad([(0.16, 0.03), (0.84, 0.03), (0.97, 0.97), (0.03, 0.97)]);
        let m = homography(&trap).expect("梯形可解");
        // 直立场景：竖线 + 两条横线；用对称的斜坡剖面，梯度质心没有系统偏移。
        let scene = |u: f64, v: f64| -> f32 {
            let mut g = 0.82f64;
            for k in 1..6 {
                let uk = k as f64 / 6.0;
                let d = (u - uk).abs();
                if d < 0.006 {
                    g = g.min(0.05 + 0.77 * (d / 0.006));
                }
            }
            for vk in [0.25f64, 0.75] {
                let d = (v - vk).abs();
                if d < 0.006 {
                    g = g.min(0.35 + 0.4 * (d / 0.006));
                }
            }
            g as f32
        };
        // 正向 warp：图像 (x,y) 看到的是场景在 H(x,y) 处的样子，于是场景的竖线被压成梯形。
        let warped = gray_image(w, h, |x, y| match apply(&m, x / w as f64, y / h as f64) {
            Some((u, v)) => scene(u, v),
            None => 0.5,
        });

        let q = auto_upright(&warped, w, h).expect("强梯形应该估得出来");
        let fixed = rectify(&warped, w, h, &q, w, h);
        let lines = vertical_lines(&fixed, w, h);
        let tall: Vec<_> = lines.iter().filter(|l| l.3 - l.1 >= 0.5).collect();
        assert!(tall.len() >= 3, "校正后只剩 {} 条长竖线", tall.len());
        let mut worst = 0.0f64;
        for &&(x0, y0, x1, y1) in &tall {
            let ang = ((x1 - x0) / (y1 - y0)).atan().to_degrees().abs();
            worst = worst.max(ang);
        }
        assert!(worst < 1.0, "校正后竖线还歪 {worst}°");

        // 反面判据：没有任何直线的平坦图上不许凭空给出依据。
        let flat = vec![0.5f32; w * h * 3];
        assert!(auto_upright(&flat, w, h).is_none());
        // 只有一条竖线的一张图也不够。
        let single = gray_image(w, h, |x, _| if (x - 120.0).abs() < 1.0 { 0.0 } else { 1.0 });
        assert!(auto_upright(&single, w, h).is_none());
    }
}
