//! 修复画笔与仿制图章：椭圆笔触内的仿制搬运与调和扩散补洞。
//!
//! 两种模式共用同一套笔触几何。像素平面是 rgb 交错、行优先的 `f32`，长度
//! `w * h * 3`，像素坐标取整数格中心（第 x 列的中心是 x），与 `warp` / `mask`
//! 两个模块一致。笔触是一个可旋转的椭圆：把 `(x - cx, y - cy)` 反向旋转
//! `angle_deg` 得到 `(u, v)`，归一化距离 `q = hypot(u / rx, v / ry)`，
//! `q <= 1` 是笔触内部；权重在 `q ∈ [1 - feather, 1]` 之间用 smoothstep 从 1
//! 降到 0，`feather = 0` 时内部权重恒为 1、外部恒为 0。
//!
//! 仿制（`source = Some(dx, dy)`）：按权重把 `(x + dx, y + dy)` 的双线性采样
//! 值 over 回原地。源区域与目标区域重叠时先把整块采样结果算完再写回，否则
//! 边写边读会让后面的像素吃到已经被改过的值。
//!
//! 修复（`source = None`）：把 `q <= 1` 的区域当作未知量，取椭圆外一圈的像素
//! 作 Dirichlet 边界，在斑内解离散拉普拉斯方程 `Δu = 0`——每个未知像素取上下
//! 左右四邻的平均，逐点 Gauss-Seidel 扫若干轮。这是 Poisson 图像编辑（Pérez
//! 等, 2003）在「边界已知、内部无引导场」时的退化情形：解出的场在斑内调和，
//! 与边界梯度连续，所以纯色不会被抹成灰、渐变也不会在斑边留下台阶。
//! 初值取水平/垂直两个方向的「跨斑线性插值」的平均：线性渐变上它已经就是解，
//! 迭代只做精修，因此迭代次数按半径给上界即可收敛，且整个求解串行、确定性。
//!
//! Healing brush and clone stamp: clone transport and harmonic inpainting inside an
//! elliptical stroke.
//!
//! Both modes share one stroke geometry. The pixel plane is interleaved rgb, row-major
//! `f32` of length `w * h * 3`, with pixel coordinates at integer cell centers (column x
//! has center x), matching the `warp` and `mask` modules. A stroke is a rotatable
//! ellipse: rotating `(x - cx, y - cy)` back by `angle_deg` gives `(u, v)`, the
//! normalized distance is `q = hypot(u / rx, v / ry)`, and `q <= 1` is the interior; the
//! weight falls from 1 to 0 with smoothstep over `q ∈ [1 - feather, 1]`, so `feather = 0`
//! makes the interior weight exactly 1 and the exterior exactly 0.
//!
//! Clone (`source = Some(dx, dy)`): the bilinear sample at `(x + dx, y + dy)` is
//! composited back over the destination by weight. When the source region overlaps the
//! destination, the whole block of samples is computed before anything is written;
//! otherwise a write-then-read smears later pixels with values it already changed.
//!
//! Heal (`source = None`): the `q <= 1` region is the unknown, the ring just outside the
//! ellipse supplies Dirichlet data, and the interior solves the discrete Laplace
//! equation `Δu = 0` — each unknown pixel takes the average of its four neighbours,
//! swept point by point with Gauss-Seidel. This is Poisson image editing (Pérez et al.,
//! 2003) degenerated to "known boundary, no guidance field": the field is harmonic
//! inside, continuous in gradient with the boundary, so flat colour is not washed grey
//! and a gradient does not leave a step at the seam. The initial guess averages the
//! horizontal and vertical linear interpolations across the hole; on a linear gradient
//! that guess is already the solution, so iterations only refine and a
//! radius-derived iteration bound suffices. The solve is serial and deterministic.

/// 一个修复/仿制笔触。
///
/// One healing / clone stroke.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spot {
    /// 中心横坐标（像素坐标，允许小数，可落在画面外）。
    ///
    /// Center x in pixel coordinates; fractional values are allowed and it may lie
    /// outside the frame.
    pub cx: f32,
    /// 中心纵坐标（像素坐标，允许小数，可落在画面外）。
    ///
    /// Center y in pixel coordinates; fractional values are allowed and it may lie
    /// outside the frame.
    pub cy: f32,
    /// 横向半轴（像素，必须大于 0，否则该笔触被跳过）。
    ///
    /// Horizontal semi-axis in pixels; must be greater than 0 or the stroke is skipped.
    pub rx: f32,
    /// 纵向半轴（像素，必须大于 0，否则该笔触被跳过）。
    ///
    /// Vertical semi-axis in pixels; must be greater than 0 or the stroke is skipped.
    pub ry: f32,
    /// 椭圆旋转角（度，画面坐标系，y 轴朝下）。
    ///
    /// Ellipse rotation in degrees in image coordinates, with y pointing down.
    pub angle_deg: f32,
    /// 边缘羽化占半径的比例，`0..1`；0 为硬边，1 为从中心淡到边缘。
    ///
    /// Edge feather as a fraction of the radius, `0..1`; 0 is a hard edge and 1 fades
    /// from the center all the way out.
    pub feather: f32,
    /// `Some(dx, dy)` 为仿制图章，源像素取 `(x + dx, y + dy)`；`None` 为修复（扩散填充）。
    ///
    /// `Some(dx, dy)` is the clone stamp, sampling at `(x + dx, y + dy)`; `None` is heal
    /// (diffusion fill).
    pub source: Option<(f32, f32)>,
}

/// 依次施加所有笔触，返回新缓冲区；输入只读，尺寸不变。
///
/// Applies every stroke in order and returns a new buffer; the input is read-only and the
/// size is unchanged.
///
/// 后续笔触看到的是前面笔触的结果，与用户按下的先后顺序一致。缓冲区长度不足
/// `w * h * 3`、尺寸为零、或笔触表为空时，直接返回输入的副本，不做任何写入。
///
/// Later strokes see the result of earlier ones, matching the order in which the user
/// pressed them. When the buffer is shorter than `w * h * 3`, a dimension is zero, or the
/// stroke list is empty, a plain copy of the input is returned with no writes.
pub fn heal(rgb: &[f32], w: usize, h: usize, spots: &[Spot]) -> Vec<f32> {
    let mut out = rgb.to_vec();
    let Some(len) = w.checked_mul(h).and_then(|n| n.checked_mul(3)) else {
        return out;
    };
    if len == 0 || out.len() < len || spots.is_empty() {
        return out;
    }
    for spot in spots {
        let Some(ellipse) = Ellipse::new(spot) else {
            continue;
        };
        match spot.source {
            Some((dx, dy)) => {
                let (dx, dy) = (dx as f64, dy as f64);
                if dx.is_finite() && dy.is_finite() {
                    clone_into(&mut out, w, h, &ellipse, dx, dy);
                }
            }
            None => heal_hole(&mut out, w, h, &ellipse),
        }
    }
    out
}

/// 一支笔触的几何：中心、半轴、旋转、羽化，外加椭圆外接矩形的半宽半高。
///
/// The geometry of one stroke: center, semi-axes, rotation, feather, plus the half width
/// and half height of the ellipse's axis-aligned bounding box.
struct Ellipse {
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    cos: f64,
    sin: f64,
    feather: f64,
    hx: f64,
    hy: f64,
}

impl Ellipse {
    /// 校验并构造；半轴非正、任何字段非有限值时返回 `None`（该笔触直接跳过）。
    ///
    /// Validates and builds the geometry; returns `None` for non-positive semi-axes or any
    /// non-finite field, in which case the stroke is skipped.
    fn new(s: &Spot) -> Option<Self> {
        let cx = s.cx as f64;
        let cy = s.cy as f64;
        let rx = s.rx as f64;
        let ry = s.ry as f64;
        let angle = s.angle_deg as f64;
        if !cx.is_finite()
            || !cy.is_finite()
            || !rx.is_finite()
            || !ry.is_finite()
            || !angle.is_finite()
        {
            return None;
        }
        if rx <= 0.0 || ry <= 0.0 {
            return None;
        }
        // 非有限的羽化按硬边处理：宁可算成 0，也不让 NaN 顺着权重污染整块缓冲区。
        let feather = if s.feather.is_finite() {
            (s.feather as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (sin, cos) = angle.to_radians().sin_cos();
        // 旋转椭圆的 AABB 半轴，见任意解析几何教材的二次型对角化。
        let hx = ((rx * cos) * (rx * cos) + (ry * sin) * (ry * sin)).sqrt();
        let hy = ((rx * sin) * (rx * sin) + (ry * cos) * (ry * cos)).sqrt();
        Some(Self {
            cx,
            cy,
            rx,
            ry,
            cos,
            sin,
            feather,
            hx,
            hy,
        })
    }

    /// 像素 `(x, y)` 到椭圆的归一化距离：椭圆上为 1。
    ///
    /// Normalized distance from pixel `(x, y)` to the ellipse; it is 1 on the outline.
    fn q(&self, x: usize, y: usize) -> f64 {
        let px = x as f64 - self.cx;
        let py = y as f64 - self.cy;
        let u = px * self.cos + py * self.sin;
        let v = -px * self.sin + py * self.cos;
        let a = u / self.rx;
        let b = v / self.ry;
        (a * a + b * b).sqrt()
    }

    /// 权重：`q <= 1` 内为 1 衰减到边缘的 0；`feather = 0` 退化成硬边掩码。
    ///
    /// Weight: 1 inside `q <= 1`, fading to 0 at the outline; `feather = 0` degenerates to
    /// a hard mask.
    fn weight(&self, q: f64) -> f64 {
        if self.feather <= 0.0 {
            return if q <= 1.0 { 1.0 } else { 0.0 };
        }
        let t = ((1.0 - q) / self.feather).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

/// 把 `[center - half, center + half]` 外扩 `margin` 后夹到 `0..n-1`，得到整数闭区间。
///
/// Clamps `[center - half, center + half]` grown by `margin` to `0..n-1`, yielding an
/// inclusive integer range.
fn span(center: f64, half: f64, n: usize, margin: f64) -> Option<(usize, usize)> {
    if n == 0 {
        return None;
    }
    let lo = (center - half - margin).floor().max(0.0);
    let hi = (center + half + margin).ceil().min(n as f64 - 1.0);
    if !lo.is_finite() || !hi.is_finite() || hi < lo {
        return None;
    }
    Some((lo as usize, hi as usize))
}

/// 双线性采样，越界按边界夹取（夹到最近的边像素），与 `warp` 的整数格中心口径一致。
///
/// Bilinear sampling with clamp-to-edge borders, matching the integer-cell-center
/// convention of `warp`.
fn sample(buf: &[f32], w: usize, h: usize, sx: f64, sy: f64) -> [f32; 3] {
    let px = sx.clamp(0.0, w as f64 - 1.0);
    let py = sy.clamp(0.0, h as f64 - 1.0);
    let fx = px.floor();
    let fy = py.floor();
    let tx = (px - fx) as f32;
    let ty = (py - fy) as f32;
    let x0 = fx as usize;
    let y0 = fy as usize;
    let o00 = (y0 * w + x0) * 3;
    // 正好落在格中心时不做插值：仿制取整像素偏移必须逐位等于源像素，不能被
    // `p00 + (p10 - p00) * 0.0` 这种运算引入一个 ulp 的漂移。
    if tx == 0.0 && ty == 0.0 {
        return [buf[o00], buf[o00 + 1], buf[o00 + 2]];
    }
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let p00 = buf[o00 + c];
        let p10 = buf[(y0 * w + x1) * 3 + c];
        let p01 = buf[(y1 * w + x0) * 3 + c];
        let p11 = buf[(y1 * w + x1) * 3 + c];
        let top = p00 + (p10 - p00) * tx;
        let bottom = p01 + (p11 - p01) * tx;
        out[c] = top + (bottom - top) * ty;
    }
    out
}

/// 仿制：按羽化权重把源偏移处的采样值混回原地。
///
/// Clone: blends the sample at the source offset back over the destination by feather
/// weight.
fn clone_into(buf: &mut [f32], w: usize, h: usize, e: &Ellipse, dx: f64, dy: f64) {
    let (Some((x0, x1)), Some((y0, y1))) = (span(e.cx, e.hx, w, 1.0), span(e.cy, e.hy, h, 1.0))
    else {
        return;
    };
    let bw = x1 - x0 + 1;
    let bh = y1 - y0 + 1;
    // 先把整块的源采样与权重算完再写回：dx、dy 小于斑径时源区域与目标区域重叠，
    // 边写边读会把前面的写入当成源像素，仿制就变成了自我涂抹。
    let mut src = vec![0.0f32; bw * bh * 3];
    let mut weights = vec![0.0f32; bw * bh];
    for y in y0..=y1 {
        for x in x0..=x1 {
            let wgt = e.weight(e.q(x, y));
            if wgt <= 0.0 {
                continue;
            }
            let i = (y - y0) * bw + (x - x0);
            let px = sample(buf, w, h, x as f64 + dx, y as f64 + dy);
            src[i * 3] = px[0];
            src[i * 3 + 1] = px[1];
            src[i * 3 + 2] = px[2];
            weights[i] = wgt as f32;
        }
    }
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = (y - y0) * bw + (x - x0);
            let wgt = weights[i];
            if wgt <= 0.0 {
                continue;
            }
            let o = (y * w + x) * 3;
            for c in 0..3 {
                let s = src[i * 3 + c];
                // 权重正好为 1 时直接赋值：内部逐位等于源，不经过 lerp 的舍入。
                buf[o + c] = if wgt >= 1.0 {
                    s
                } else {
                    buf[o + c] + (s - buf[o + c]) * wgt
                };
            }
        }
    }
}

/// 修复：以斑外一圈为边界，在斑内解离散拉普拉斯方程，再按羽化权重混回。
///
/// Heal: solves the discrete Laplace equation inside the stroke with the surrounding ring
/// as boundary, then blends the result back by feather weight.
fn heal_hole(buf: &mut [f32], w: usize, h: usize, e: &Ellipse) {
    let (Some((x0, x1)), Some((y0, y1))) = (span(e.cx, e.hx, w, 1.0), span(e.cy, e.hy, h, 1.0))
    else {
        return;
    };
    let bw = x1 - x0 + 1;
    let bh = y1 - y0 + 1;
    let mut inside = vec![false; bw * bh];
    let mut n_in = 0usize;
    for y in y0..=y1 {
        for x in x0..=x1 {
            if e.q(x, y) <= 1.0 {
                inside[(y - y0) * bw + (x - x0)] = true;
                n_in += 1;
            }
        }
    }
    // 一个内部像素都没有 = 笔触落在画面外；全部都是内部 = 画面里没有边界可用，
    // 无解可求。两种情况都原样返回，绝不拿假想的边界去填。
    if n_in == 0 || n_in == bw * bh {
        return;
    }

    // 初值：每个内部像素沿水平方向跨斑线性插值（左右两侧最近的斑外像素），
    // 竖直方向同理，两者取平均。线性渐变上这个初值已经精确，迭代只做精修。
    let mut solved = vec![0.0f32; bw * bh * 3];
    for y in y0..=y1 {
        for x in x0..=x1 {
            if !inside[(y - y0) * bw + (x - x0)] {
                continue;
            }
            let mut left: Option<([f32; 3], f64)> = None;
            let mut xx = x;
            while xx > x0 {
                xx -= 1;
                if !inside[(y - y0) * bw + (xx - x0)] {
                    let o = (y * w + xx) * 3;
                    left = Some(([buf[o], buf[o + 1], buf[o + 2]], (x - xx) as f64));
                    break;
                }
            }
            let mut right: Option<([f32; 3], f64)> = None;
            let mut xx = x;
            while xx < x1 {
                xx += 1;
                if !inside[(y - y0) * bw + (xx - x0)] {
                    let o = (y * w + xx) * 3;
                    right = Some(([buf[o], buf[o + 1], buf[o + 2]], (xx - x) as f64));
                    break;
                }
            }
            let mut up: Option<([f32; 3], f64)> = None;
            let mut yy = y;
            while yy > y0 {
                yy -= 1;
                if !inside[(yy - y0) * bw + (x - x0)] {
                    let o = (yy * w + x) * 3;
                    up = Some(([buf[o], buf[o + 1], buf[o + 2]], (y - yy) as f64));
                    break;
                }
            }
            let mut down: Option<([f32; 3], f64)> = None;
            let mut yy = y;
            while yy < y1 {
                yy += 1;
                if !inside[(yy - y0) * bw + (x - x0)] {
                    let o = (yy * w + x) * 3;
                    down = Some(([buf[o], buf[o + 1], buf[o + 2]], (yy - y) as f64));
                    break;
                }
            }

            let mut sum = [0.0f64; 3];
            let mut terms = 0.0f64;
            if let (Some((lv, ld)), Some((rv, rd))) = (left, right) {
                let d = ld + rd;
                for c in 0..3 {
                    sum[c] += (lv[c] as f64 * rd + rv[c] as f64 * ld) / d;
                }
                terms += 1.0;
            }
            if let (Some((uv, ud)), Some((dv, dd))) = (up, down) {
                let d = ud + dd;
                for c in 0..3 {
                    sum[c] += (uv[c] as f64 * dd + dv[c] as f64 * ud) / d;
                }
                terms += 1.0;
            }
            let o = (y * w + x) * 3;
            let s = ((y - y0) * bw + (x - x0)) * 3;
            for c in 0..3 {
                solved[s + c] = if terms > 0.0 {
                    (sum[c] / terms) as f32
                } else {
                    buf[o + c]
                };
            }
        }
    }

    // Gauss-Seidel：内部像素取上下左右四邻平均，斑外像素保持原值（Dirichlet 边界），
    // 贴着画面边、四邻不全时只对存在的邻居求平均（自然边界）。迭代次数按半径给上界。
    let sweeps = ((e.rx.max(e.ry) * 4.0).ceil() as usize).clamp(16, 256);
    for _ in 0..sweeps {
        for yi in 0..bh {
            for xi in 0..bw {
                let here = yi * bw + xi;
                if !inside[here] {
                    continue;
                }
                let o = here * 3;
                for c in 0..3 {
                    let mut sum = 0.0f32;
                    let mut cnt = 0.0f32;
                    if xi > 0 {
                        let j = here - 1;
                        sum += if inside[j] {
                            solved[j * 3 + c]
                        } else {
                            buf[((y0 + yi) * w + (x0 + xi - 1)) * 3 + c]
                        };
                        cnt += 1.0;
                    }
                    if xi + 1 < bw {
                        let j = here + 1;
                        sum += if inside[j] {
                            solved[j * 3 + c]
                        } else {
                            buf[((y0 + yi) * w + (x0 + xi + 1)) * 3 + c]
                        };
                        cnt += 1.0;
                    }
                    if yi > 0 {
                        let j = here - bw;
                        sum += if inside[j] {
                            solved[j * 3 + c]
                        } else {
                            buf[((y0 + yi - 1) * w + (x0 + xi)) * 3 + c]
                        };
                        cnt += 1.0;
                    }
                    if yi + 1 < bh {
                        let j = here + bw;
                        sum += if inside[j] {
                            solved[j * 3 + c]
                        } else {
                            buf[((y0 + yi + 1) * w + (x0 + xi)) * 3 + c]
                        };
                        cnt += 1.0;
                    }
                    if cnt > 0.0 {
                        solved[o + c] = sum / cnt;
                    }
                }
            }
        }
    }

    for yi in 0..bh {
        for xi in 0..bw {
            let here = yi * bw + xi;
            if !inside[here] {
                continue;
            }
            let wgt = e.weight(e.q(x0 + xi, y0 + yi)) as f32;
            if wgt <= 0.0 {
                continue;
            }
            let o = ((y0 + yi) * w + (x0 + xi)) * 3;
            for c in 0..3 {
                let v = solved[here * 3 + c];
                buf[o + c] = if wgt >= 1.0 {
                    v
                } else {
                    buf[o + c] + (v - buf[o + c]) * wgt
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 横向 0 → 1 的灰度渐变（三通道相同），理想值就是 `x / (w - 1)`。
    fn gradient(w: usize, h: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; w * h * 3];
        let denom = (w as f32 - 1.0).max(1.0);
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / denom;
                let o = (y * w + x) * 3;
                v[o] = t;
                v[o + 1] = t;
                v[o + 2] = t;
            }
        }
        v
    }

    fn gray(v: f32, w: usize, h: usize) -> Vec<f32> {
        vec![v; w * h * 3]
    }

    fn at(buf: &[f32], w: usize, x: usize, y: usize) -> f32 {
        buf[(y * w + x) * 3]
    }

    /// 与实现同口径的归一化距离，供测试挑「斑内 / 斑外」的像素。
    fn q_of(x: usize, y: usize, cx: f64, cy: f64, rx: f64, ry: f64, angle: f64) -> f64 {
        let (sin, cos) = angle.to_radians().sin_cos();
        let px = x as f64 - cx;
        let py = y as f64 - cy;
        let u = px * cos + py * sin;
        let v = -px * sin + py * cos;
        ((u / rx) * (u / rx) + (v / ry) * (v / ry)).sqrt()
    }

    /// 一块矩形区域内相邻像素的最大一阶差分，横向纵向都算。
    fn max_step(buf: &[f32], w: usize, x0: usize, x1: usize, y0: usize, y1: usize) -> f32 {
        let mut worst = 0.0f32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                if x < x1 {
                    worst = worst.max((at(buf, w, x + 1, y) - at(buf, w, x, y)).abs());
                }
                if y < y1 {
                    worst = worst.max((at(buf, w, x, y + 1) - at(buf, w, x, y)).abs());
                }
            }
        }
        worst
    }

    #[test]
    fn empty_spots_is_bitwise_identity() {
        let src = gradient(17, 11);
        let out = heal(&src, 17, 11, &[]);
        assert_eq!(out.len(), src.len());
        for (a, b) in out.iter().zip(&src) {
            assert_eq!(a.to_bits(), b.to_bits(), "空笔触表必须原样返回");
        }
    }

    #[test]
    fn clone_of_a_uniform_patch_changes_nothing() {
        // 纯色图上从哪儿取都是同一个值。源偏移方向写反、符号写错，只要还在纯色区
        // 就看不出来——所以这一条咬的是「取到了画面外或取错了缓冲区」那类实现。
        let src = gray(0.4, 24, 24);
        let spot = Spot {
            cx: 12.0,
            cy: 12.0,
            rx: 6.0,
            ry: 6.0,
            angle_deg: 0.0,
            feather: 0.3,
            source: Some((5.0, -3.0)),
        };
        let out = heal(&src, 24, 24, &[spot]);
        for (a, b) in out.iter().zip(&src) {
            assert_eq!(a.to_bits(), b.to_bits(), "纯色区仿制后必须逐位不变");
        }
    }

    #[test]
    fn heal_reproduces_a_linear_gradient() {
        let (w, h) = (64usize, 32usize);
        let mut src = gradient(w, h);
        let (cx, cy, r) = (32.0f64, 16.0f64, 10.0f64);
        // 斑内先打成 0（模拟污点）：只读原样的实现会被这条判据咬住。
        for y in 0..h {
            for x in 0..w {
                if q_of(x, y, cx, cy, r, r, 0.0) <= 1.0 {
                    let o = (y * w + x) * 3;
                    src[o] = 0.0;
                    src[o + 1] = 0.0;
                    src[o + 2] = 0.0;
                }
            }
        }
        let spot = Spot {
            cx: cx as f32,
            cy: cy as f32,
            rx: r as f32,
            ry: r as f32,
            angle_deg: 0.0,
            feather: 0.0,
            source: None,
        };
        let out = heal(&src, w, h, &[spot]);
        let denom = w as f32 - 1.0;
        let mut worst = 0.0f32;
        for y in 0..h {
            for x in 0..w {
                let q = q_of(x, y, cx, cy, r, r, 0.0);
                if q < 0.95 {
                    let ideal = x as f32 / denom;
                    for c in 0..3 {
                        let e = (out[(y * w + x) * 3 + c] - ideal).abs();
                        worst = worst.max(e);
                    }
                } else if q > 1.05 {
                    for c in 0..3 {
                        assert_eq!(
                            out[(y * w + x) * 3 + c].to_bits(),
                            src[(y * w + x) * 3 + c].to_bits(),
                            "斑外必须逐位不变"
                        );
                    }
                }
            }
        }
        assert!(worst < 1e-3, "斑内与理想梯度的最大误差 {worst}");
    }

    #[test]
    fn clone_takes_pixels_from_the_source_offset() {
        let (w, h) = (48usize, 24usize);
        let src = gradient(w, h);
        let (cx, cy, r, dx) = (24.0f64, 12.0f64, 8.0f64, 6usize);
        let spot = Spot {
            cx: cx as f32,
            cy: cy as f32,
            rx: r as f32,
            ry: r as f32,
            angle_deg: 0.0,
            feather: 0.0,
            source: Some((dx as f32, 0.0)),
        };
        let out = heal(&src, w, h, &[spot]);
        for y in 0..h {
            for x in 0..w {
                if q_of(x, y, cx, cy, r, r, 0.0) < 0.9 {
                    let want = src[(y * w + x + dx) * 3];
                    let got = at(&out, w, x, y);
                    assert_eq!(got.to_bits(), want.to_bits(), "({x},{y}) 应逐位等于源像素");
                }
            }
        }
    }

    #[test]
    fn feather_makes_the_seam_monotone() {
        // 黑底 + 右侧一块纯白，笔触从白块里取材盖到左边：硬边会在斑的轮廓上留下
        // 一整格 0→1 的台阶，羽化把这一格摊到整个环带上，相邻差分随之变小。
        let (w, h) = (64usize, 64usize);
        let mut src = gray(0.0, w, h);
        for y in 0..h {
            for x in 40..w {
                let o = (y * w + x) * 3;
                src[o] = 1.0;
                src[o + 1] = 1.0;
                src[o + 2] = 1.0;
            }
        }
        let spot = |feather: f32| Spot {
            cx: 20.0,
            cy: 32.0,
            rx: 8.0,
            ry: 8.0,
            angle_deg: 0.0,
            feather,
            source: Some((32.0, 0.0)),
        };
        let hard = heal(&src, w, h, &[spot(0.0)]);
        let soft = heal(&src, w, h, &[spot(0.6)]);
        // 只看笔触周围那圈，白块自己的边界（x = 39/40）不算在内。
        let hard_step = max_step(&hard, w, 10, 32, 22, 44);
        let soft_step = max_step(&soft, w, 10, 32, 22, 44);
        assert!(hard_step > 0.9, "硬边应当留下一格整跳变，实测 {hard_step}");
        assert!(
            soft_step < 0.45,
            "羽化后相邻差分应显著变小，实测 {soft_step}"
        );
        // 斑心仍然是纯白，羽化只软化边缘，没有把中间也冲淡。
        assert!((at(&soft, w, 20, 32) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn heal_is_deterministic() {
        let (w, h) = (40usize, 30usize);
        let mut src = gradient(w, h);
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 3;
                src[o + 1] = ((x * 7 + y * 3) % 11) as f32 / 10.0;
            }
        }
        let spots = [
            Spot {
                cx: 10.0,
                cy: 10.0,
                rx: 6.0,
                ry: 4.0,
                angle_deg: 20.0,
                feather: 0.4,
                source: None,
            },
            Spot {
                cx: 28.0,
                cy: 18.0,
                rx: 5.0,
                ry: 5.0,
                angle_deg: -35.0,
                feather: 0.0,
                source: Some((-7.0, 2.0)),
            },
        ];
        let a = heal(&src, w, h, &spots);
        let b = heal(&src, w, h, &spots);
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.to_bits(), y.to_bits(), "同输入两次调用必须逐位相同");
        }
    }

    #[test]
    fn spots_outside_the_frame_are_harmless() {
        let (w, h) = (32usize, 32usize);
        let src = gradient(w, h);
        let spots = [
            // 中心在画面外五个半径处（左右上下各一次）。
            Spot {
                cx: -40.0,
                cy: 16.0,
                rx: 8.0,
                ry: 8.0,
                angle_deg: 0.0,
                feather: 0.2,
                source: None,
            },
            Spot {
                cx: 72.0,
                cy: 16.0,
                rx: 8.0,
                ry: 8.0,
                angle_deg: 0.0,
                feather: 0.2,
                source: Some((3.0, 3.0)),
            },
            Spot {
                cx: 16.0,
                cy: -40.0,
                rx: 8.0,
                ry: 8.0,
                angle_deg: 45.0,
                feather: 0.0,
                source: None,
            },
            Spot {
                cx: 16.0,
                cy: 72.0,
                rx: 8.0,
                ry: 8.0,
                angle_deg: 45.0,
                feather: 0.0,
                source: Some((0.0, -3.0)),
            },
            // 贴边 0.5 px：半个斑在画内，边界像素不全。
            Spot {
                cx: 0.5,
                cy: 0.5,
                rx: 6.0,
                ry: 6.0,
                angle_deg: 0.0,
                feather: 0.5,
                source: None,
            },
            Spot {
                cx: 31.5,
                cy: 31.5,
                rx: 5.0,
                ry: 5.0,
                angle_deg: 30.0,
                feather: 0.0,
                source: Some((-4.0, -4.0)),
            },
        ];
        let out = heal(&src, w, h, &spots);
        assert_eq!(out.len(), w * h * 3);
        assert!(
            out.iter().all(|v| v.is_finite()),
            "越界与贴边的笔触不能产生 NaN/Inf"
        );
    }

    #[test]
    fn later_spots_see_earlier_results() {
        // 第一个笔触把白块搬到左边，第二个笔触的源正好落在第一个笔触的成果上：
        // 若实现每次都从输入副本取源，第二个笔触只能拿到黑底。
        let (w, h) = (48usize, 32usize);
        let mut src = gray(0.0, w, h);
        for y in 12..20 {
            for x in 36..44 {
                let o = (y * w + x) * 3;
                src[o] = 1.0;
                src[o + 1] = 1.0;
                src[o + 2] = 1.0;
            }
        }
        let first = Spot {
            cx: 10.0,
            cy: 16.0,
            rx: 4.0,
            ry: 4.0,
            angle_deg: 0.0,
            feather: 0.0,
            source: Some((30.0, 0.0)),
        };
        let second = Spot {
            cx: 20.0,
            cy: 16.0,
            rx: 3.0,
            ry: 3.0,
            angle_deg: 0.0,
            feather: 0.0,
            source: Some((-10.0, 0.0)),
        };
        let out = heal(&src, w, h, &[first, second]);
        assert!(
            (at(&out, w, 20, 16) - 1.0).abs() < 1e-6,
            "后一个笔触应当看到前一个笔触写入的白色，实测 {}",
            at(&out, w, 20, 16)
        );
    }

    #[test]
    fn healed_field_satisfies_the_laplace_equation() {
        // 背景是径向斜坡（不是调和场），斑内先打成 0 再修：若实现只做「跨斑线性插值」
        // 或「用边界均值填平」，斑心的四邻平均会明显不等于自身；真正解了拉普拉斯方程
        // 的场，残差 `|4u - Σ四邻|` 只剩舍入量级。
        let (w, h) = (64usize, 64usize);
        let (cx, cy, r) = (32.0f64, 32.0f64, 12.0f64);
        let mut src = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let d = q_of(x, y, cx, cy, 1.0, 1.0, 0.0);
                let v = (0.2 + 0.6 * (d / 32.0)).clamp(0.0, 1.0) as f32;
                let o = (y * w + x) * 3;
                src[o] = v;
                src[o + 1] = v;
                src[o + 2] = v;
            }
        }
        for y in 0..h {
            for x in 0..w {
                if q_of(x, y, cx, cy, r, r, 0.0) <= 1.0 {
                    let o = (y * w + x) * 3;
                    src[o] = 0.0;
                    src[o + 1] = 0.0;
                    src[o + 2] = 0.0;
                }
            }
        }
        let spot = Spot {
            cx: cx as f32,
            cy: cy as f32,
            rx: r as f32,
            ry: r as f32,
            angle_deg: 0.0,
            feather: 0.0,
            source: None,
        };
        let out = heal(&src, w, h, &[spot]);
        let mut worst = 0.0f32;
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                // 只查离轮廓足够远的内部像素：四邻全在斑内，残差纯由求解质量决定。
                if q_of(x, y, cx, cy, r, r, 0.0) > 0.75 {
                    continue;
                }
                for c in 0..3 {
                    let u = out[(y * w + x) * 3 + c];
                    let sum = out[(y * w + x - 1) * 3 + c]
                        + out[(y * w + x + 1) * 3 + c]
                        + out[((y - 1) * w + x) * 3 + c]
                        + out[((y + 1) * w + x) * 3 + c];
                    worst = worst.max((4.0 * u - sum).abs());
                }
            }
        }
        assert!(
            worst < 1e-3,
            "斑内拉普拉斯残差过大（只做插值或均值填充会到 1e-2 量级）：{worst}"
        );
    }
}
