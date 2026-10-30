//! 取景几何：拉直（整幅绕中心旋转）→ 对齐位移 → 裁切窗口，一趟采样做完。
//!
//! 三步合成一次逆映射是有原因的：分开做要么先把整帧旋转出一份全尺寸缓冲再裁
//! （24MP 一帧 288MB 的 f32 中转），要么两次插值把画面糊一层。逆映射只按最终
//! 留下的像素算，行与行互不干扰，按行并行。
//!
//! 边界按 reflect101，和 `cv2.warpAffine` 的默认边界、和对齐模块的口径一致。
//!
//! `view_into` 是另一件事：运镜的取景框是个**浮点**窗口，比输出小（推近），
//! 所以只能重采样，不能走上面这条 1:1 的整数快路。

use rayon::prelude::*;

/// 一帧的取景变换参数。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Framing {
    /// 拉直角度（度）。正角 = 画面逆时针转，跟 `lapse/align.py` 的 `rotation_deg` 同向。
    pub angle_deg: f64,
    /// 对齐位移（源像素）：`dst(x, y) = src(x - ox, y - oy)`。
    pub off: [f64; 2],
}

impl Framing {
    /// 没有任何几何动作：旋转角为 0 且没有位移。
    pub fn is_identity(&self) -> bool {
        self.angle_deg.abs() <= 1e-9 && self.off[0].abs() <= 1e-9 && self.off[1].abs() <= 1e-9
    }
}

/// 取景后的输出尺寸（只夹边界，不做变换）。
pub fn window(w: usize, h: usize, crop: Option<(usize, usize, usize, usize)>) -> (usize, usize) {
    let (cx, cy, cw, ch) = crop.unwrap_or((0, 0, w, h));
    let cx = cx.min(w);
    let cy = cy.min(h);
    (cw.min(w - cx), ch.min(h - cy))
}

/// 旋转 + 位移 + 裁切一次做完，写进 `out`，返回输出宽高。
///
/// `src` 是 `w × h` 的 rgb f32（0..1）。`crop` 是最终窗口，`None` 为整幅。
/// 采样坐标：窗口像素先反拉回未旋转的帧（绕画面中心转 `-angle`），再减对齐位移。
pub fn transform_into(
    src: &[f32],
    w: usize,
    h: usize,
    framing: Framing,
    crop: Option<(usize, usize, usize, usize)>,
    out: &mut Vec<f32>,
) -> (usize, usize) {
    let (cx, cy, cw, ch) = {
        let (x, y, ww, hh) = crop.unwrap_or((0, 0, w, h));
        let x = x.min(w);
        let y = y.min(h);
        (x, y, ww.min(w - x), hh.min(h - y))
    };
    if cw == 0 || ch == 0 {
        out.clear();
        return (0, 0);
    }
    if framing.is_identity() {
        out.clear();
        out.reserve(cw * ch * 3);
        if cx == 0 && cy == 0 && cw == w && ch == h {
            out.extend_from_slice(src);
        } else {
            for y in 0..ch {
                let start = ((cy + y) * w + cx) * 3;
                out.extend_from_slice(&src[start..start + cw * 3]);
            }
        }
        return (cw, ch);
    }

    out.resize(cw * ch * 3, 0.0);
    let rotated = framing.angle_deg.abs() > 1e-9;
    // 旋转中心取像素中心口径下的画面正中。
    let (mx, my) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let rad = framing.angle_deg.to_radians();
    let (cos, sin) = (rad.cos(), rad.sin());
    out.par_chunks_mut(cw * 3)
        .enumerate()
        .for_each(|(row, line)| {
            let py = (cy + row) as f64;
            for col in 0..cw {
                let px = (cx + col) as f64;
                // 逆旋转：把窗口里的点转回未旋转的帧。正角是画面上逆时针，
                // 而 y 轴朝下，所以逆变换算成矩阵就是 [[cos,-sin],[sin,cos]]。
                let (sx, sy) = if rotated {
                    let (dx, dy) = (px - mx, py - my);
                    (mx + cos * dx - sin * dy, my + sin * dx + cos * dy)
                } else {
                    (px, py)
                };
                let (sx, sy) = (sx - framing.off[0], sy - framing.off[1]);
                let (x0, x1, tx) = border_pair(sx, w);
                let (y0, y1, ty) = border_pair(sy, h);
                let o = col * 3;
                for c in 0..3 {
                    let p00 = src[(y0 * w + x0) * 3 + c];
                    let p10 = src[(y0 * w + x1) * 3 + c];
                    let p01 = src[(y1 * w + x0) * 3 + c];
                    let p11 = src[(y1 * w + x1) * 3 + c];
                    let top = p00 + (p10 - p00) * tx;
                    let bottom = p01 + (p11 - p01) * tx;
                    line[o + c] = top + (bottom - top) * ty;
                }
            }
        });
    (cw, ch)
}

fn border_pair(v: f64, n: usize) -> (usize, usize, f32) {
    let f = v.floor();
    let t = (v - f) as f32;
    (
        reflect101(f as isize, n as isize),
        reflect101(f as isize + 1, n as isize),
        t,
    )
}

/// 运镜（Ken Burns）的采样：把 `src` 里 `(x, y, w, h)` 这个**浮点**窗口铺满 `out_w × out_h`。
///
/// 源坐标是浮点，所以取景框每挪千分之一格，画面就真的动千分之一格——不像 ffmpeg
/// 的 zoompan 那样把位移抹成整像素台阶。窗口是 `crop::Viewfinder::pixels` 出来的，
/// 已经在画内；越界那一圈仍按 reflect101 收回来，与 `transform_into` 同一口径。
///
/// 只给「框比输出小」（推近）用。缩小不该走这里：双线性会漏采样点、起摩尔纹，
/// 那条路是整数裁切 + 面积平均，在 `export` 里拼。
pub fn view_into(
    src: &[f32],
    w: usize,
    h: usize,
    view: [f64; 4],
    out_w: usize,
    out_h: usize,
    out: &mut Vec<f32>,
) {
    let (bx, by, bw, bh) = (view[0], view[1], view[2], view[3]);
    if out_w == 0 || out_h == 0 || bw <= 0.0 || bh <= 0.0 || w == 0 || h == 0 {
        out.clear();
        return;
    }
    out.resize(out_w * out_h * 3, 0.0);
    let (sx, sy) = (bw / out_w as f64, bh / out_h as f64);
    out.par_chunks_mut(out_w * 3)
        .enumerate()
        .for_each(|(row, line)| {
            // 像素中心对齐：输出第 i 格采到窗口里 (i + 0.5) × 步长 的位置，
            // 1:1 时正好落在源像素中心，不会整体偏半格。
            let py = by + (row as f64 + 0.5) * sy - 0.5;
            let (y0, y1, ty) = border_pair(py, h);
            for col in 0..out_w {
                let px = bx + (col as f64 + 0.5) * sx - 0.5;
                let (x0, x1, tx) = border_pair(px, w);
                let o = col * 3;
                for c in 0..3 {
                    let p00 = src[(y0 * w + x0) * 3 + c];
                    let p10 = src[(y0 * w + x1) * 3 + c];
                    let p01 = src[(y1 * w + x0) * 3 + c];
                    let p11 = src[(y1 * w + x1) * 3 + c];
                    let top = p00 + (p10 - p00) * tx;
                    let bottom = p01 + (p11 - p01) * tx;
                    line[o + c] = top + (bottom - top) * ty;
                }
            }
        });
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// `w × h` 全黑，只有一个白点。
    fn dot(w: usize, h: usize, x: usize, y: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; w * h * 3];
        let o = (y * w + x) * 3;
        v[o] = 1.0;
        v[o + 1] = 1.0;
        v[o + 2] = 1.0;
        v
    }

    fn at(buf: &[f32], w: usize, x: usize, y: usize) -> f32 {
        buf[(y * w + x) * 3]
    }

    #[test]
    fn identity_is_a_plain_copy() {
        let src: Vec<f32> = (0..6 * 4 * 3).map(|i| i as f32 / 7.0).collect();
        let mut out = Vec::new();
        assert_eq!(
            transform_into(&src, 6, 4, Framing::default(), None, &mut out),
            (6, 4)
        );
        assert_eq!(out, src);
    }

    #[test]
    fn crop_window_takes_rows_without_interpolating() {
        let src: Vec<f32> = (0..6 * 4 * 3).map(|i| i as f32).collect();
        let mut out = Vec::new();
        let (w, h) = transform_into(&src, 6, 4, Framing::default(), Some((2, 1, 3, 2)), &mut out);
        assert_eq!((w, h), (3, 2));
        assert_eq!(out.len(), 18);
        assert_eq!(out[0], src[8 * 3], "全幅第 1 行第 2 列 → 窗口左上角");
    }

    #[test]
    fn shift_moves_the_content_by_exactly_the_offset() {
        let src = dot(9, 9, 4, 4);
        let mut out = Vec::new();
        transform_into(
            &src,
            9,
            9,
            Framing {
                angle_deg: 0.0,
                off: [2.0, -1.0],
            },
            None,
            &mut out,
        );
        // dst(x,y)=src(x-ox,y-oy)：白点从 (4,4) 挪到 (6,3)。
        assert_eq!(at(&out, 9, 6, 3), 1.0);
        assert_eq!(at(&out, 9, 4, 4), 0.0);
    }

    #[test]
    fn ninety_degrees_turns_counter_clockwise() {
        let src = dot(9, 9, 6, 4); // 中心右边一格
        let mut out = Vec::new();
        transform_into(
            &src,
            9,
            9,
            Framing {
                angle_deg: 90.0,
                off: [0.0, 0.0],
            },
            None,
            &mut out,
        );
        // 逆时针 90°：右边的点转到上方（y 轴朝下，所以是 y 变小）。
        assert_eq!(at(&out, 9, 4, 2), 1.0);
        assert_eq!(at(&out, 9, 6, 4), 0.0);
    }

    #[test]
    fn a_full_turn_comes_back_around() {
        let src: Vec<f32> = (0..16 * 10 * 3).map(|i| (i % 11) as f32 / 11.0).collect();
        let mut out = Vec::new();
        transform_into(
            &src,
            16,
            10,
            Framing {
                angle_deg: 360.0,
                off: [0.0, 0.0],
            },
            None,
            &mut out,
        );
        for (a, b) in out.iter().zip(&src) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn small_angle_stays_inside_the_source_and_loses_nothing_to_borders() {
        // 均匀灰场转多少度都还是那个灰：采样落在边界外时 reflect 回来的也是同一值。
        let src = vec![0.25f32; 32 * 24 * 3];
        let mut out = Vec::new();
        let (w, _) = transform_into(
            &src,
            32,
            24,
            Framing {
                angle_deg: 3.0,
                off: [0.0, 0.0],
            },
            Some((4, 4, 24, 16)),
            &mut out,
        );
        assert_eq!(w, 24);
        assert!(
            out.iter().all(|v| (v - 0.25).abs() < 1e-6),
            "{:?}",
            &out[..6]
        );
    }

    #[test]
    fn rotate_then_shift_matches_the_two_steps_applied_in_order() {
        // 位移是在反拉回未旋转的帧之后才减的：白点先转 90° 再按位移走。
        let src = dot(9, 9, 6, 4);
        let mut out = Vec::new();
        transform_into(
            &src,
            9,
            9,
            Framing {
                angle_deg: 90.0,
                off: [1.0, 1.0],
            },
            None,
            &mut out,
        );
        // 先反旋回未旋转的帧，再减位移：白点转到 (5,1) 而不是 (3,1)。
        assert_eq!(at(&out, 9, 5, 1), 1.0);
    }

    #[test]
    fn empty_window_yields_nothing() {
        let src = vec![0.0f32; 4 * 4 * 3];
        let mut out = Vec::new();
        assert_eq!(
            transform_into(&src, 4, 4, Framing::default(), Some((0, 0, 0, 4)), &mut out),
            (0, 0)
        );
        assert!(out.is_empty());
    }

    #[test]
    fn window_clamps_a_crop_that_runs_off_the_frame() {
        assert_eq!(window(10, 10, Some((8, 8, 6, 6))), (2, 2));
        assert_eq!(window(10, 10, None), (10, 10));
    }

    /// `h` 行 `w` 列的灰阶图：第 (x,y) 格的 R 通道 = `grid[y][x]`。
    fn gray(grid: &[&[f32]]) -> (Vec<f32>, usize, usize) {
        let h = grid.len();
        let w = grid[0].len();
        let mut v = vec![0.0f32; w * h * 3];
        for (y, line) in grid.iter().enumerate() {
            for (x, val) in line.iter().enumerate() {
                v[(y * w + x) * 3] = *val;
            }
        }
        (v, w, h)
    }

    fn g(buf: &[f32], w: usize, x: usize, y: usize) -> f32 {
        buf[(y * w + x) * 3]
    }

    #[test]
    fn a_1to1_window_is_a_plain_copy() {
        let (src, w, h) = gray(&[&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]]);
        let mut out = Vec::new();
        view_into(&src, w, h, [0.0, 0.0, 3.0, 2.0], 3, 2, &mut out);
        // 像素中心对齐：1:1 时逐位相同，运镜开关白开也不掉清晰度。
        assert_eq!(out, src);
    }

    #[test]
    fn pushing_in_keeps_a_half_pixel_move() {
        let (src, w, h) = gray(&[&[0.0, 1.0, 2.0, 3.0]]);
        let mut out = Vec::new();
        // 窗口从 0.5 起、宽 2.0，铺到 2 格：整数台阶只会给 [0, 1]。
        view_into(&src, w, h, [0.5, 0.0, 2.0, 1.0], 2, 1, &mut out);
        assert_eq!((g(&out, 2, 0, 0), g(&out, 2, 1, 0)), (0.5, 1.5));
    }

    #[test]
    fn upscaling_spreads_the_light_over_the_wider_grid() {
        // 4×4 的黑场，正中偏右上一个白点；取景框锁在它周围 2×2，铺成 4×4 = 推近 2×。
        let mut src = vec![0.0f32; 4 * 4 * 3];
        let (y, x) = (1usize, 2usize);
        src[(y * 4 + x) * 3] = 1.0;
        let mut out = Vec::new();
        view_into(&src, 4, 4, [1.0, 1.0, 2.0, 2.0], 4, 4, &mut out);
        // 双线性摊开：白点变成 2×2 的光斑，落点由像素中心对齐算死。
        assert_eq!(g(&out, 4, 2, 0), 0.5625);
        assert_eq!(g(&out, 4, 3, 0), 0.5625);
        assert_eq!(g(&out, 4, 2, 1), 0.5625);
        assert_eq!(g(&out, 4, 3, 1), 0.5625);
        assert_eq!(g(&out, 4, 1, 0), 0.1875, "爬坡的那一格");
        assert_eq!(g(&out, 4, 0, 0), 0.0);
        assert_eq!(g(&out, 4, 2, 3), 0.0);
        // 框往右挪半格（推近态下 = 输出 1 格）：光斑整体跟着左移一格，
        // 一像素都没被抹平——这就是不走整数裁切的原因。
        view_into(&src, 4, 4, [1.5, 1.0, 2.0, 2.0], 4, 4, &mut out);
        assert_eq!(g(&out, 4, 1, 0), 0.5625);
        assert_eq!(g(&out, 4, 2, 0), 0.5625);
        assert_eq!(g(&out, 4, 3, 0), 0.1875);
        assert_eq!(g(&out, 4, 0, 0), 0.1875);
    }

    #[test]
    fn the_frame_edge_mirrors_like_the_rotation_path() {
        let (src, w, h) = gray(&[&[1.0, 2.0], &[3.0, 4.0]]);
        let mut out = Vec::new();
        // 框顶满整幅（2×2）再铺成 4×4：输出四格采到 -0.25 / 0.25 / 0.75 / 1.25，
        // 两头那格越界、按 reflect101 折回画内，于是左右两半互为镜像。
        view_into(&src, w, h, [0.0, 0.0, 2.0, 2.0], 4, 4, &mut out);
        assert_eq!(g(&out, 4, 0, 0), 1.75);
        assert_eq!(g(&out, 4, 1, 0), 1.75);
        assert_eq!(g(&out, 4, 2, 0), 2.25);
        assert_eq!(g(&out, 4, 0, 2), 2.75);
        assert_eq!(g(&out, 4, 3, 3), 3.25);
    }

    #[test]
    fn a_degenerate_view_or_output_writes_nothing() {
        let (src, w, h) = gray(&[&[1.0, 2.0], &[3.0, 4.0]]);
        let mut out = vec![7.0f32; 4];
        view_into(&src, w, h, [0.0, 0.0, 0.0, 2.0], 4, 4, &mut out);
        assert!(out.is_empty(), "零宽的框不能铺满画幅");
        view_into(&src, w, h, [0.0, 0.0, 2.0, 2.0], 0, 0, &mut out);
        assert!(out.is_empty(), "零尺寸的输出没有像素可写");
    }
}
