//! 修复画笔的落盘口径：归一化笔触 ↔ 像素笔触。
//!
//! [`crate::heal`] 是纯像素算法，它只认「第几列第几行」。但同一份参数要在三处对上：
//! 预览（1600 边）、全尺寸导出（6000 边）、MCP 的无头调用（任意尺寸）。存像素坐标的话，
//! 用户在预览上点的那一下到了导出就飞到画面四倍远的地方。所以落盘的是归一化笔触：
//!
//! - `x` 相对画幅宽、`y` 相对画幅高，取值 `[0, 1]`；
//! - `r`（半径）与 `source`（仿制源偏移）用**同一个长度单位**：画幅长边。横向与纵向
//!   共用一个单位，圆在像素域就还是圆——若是 `x` 按宽归一化、`r` 也按宽归一化，
//!   16:9 的画幅上笔触会被横向拉成椭圆。
//!
//! 半径只支持正圆（`r` 一个数），不做长短轴：拉伸笔触是「按住并拖动」的交互产物，
//! 而第一版把「点一下修一个点」做对更有价值，且圆的落盘格式天然向前兼容——将来要加
//! 椭圆只需补 `ry` 与 `angle`，旧工程读出 `ry = None` 即按正圆处理。
//!
//! The persisted convention for the healing brush: normalized strokes ↔ pixel strokes.
//!
//! [`crate::heal`] is a pure pixel algorithm that only knows "which column, which row". But one
//! parameter set has to line up in three places: the preview (1600px edge), the full-size export
//! (6000px), and a headless MCP call (any size). Storing pixel coordinates would send the dot a
//! user placed in the preview four times too far out in the export. So what gets persisted is
//! normalized:
//!
//! - `x` is relative to the frame width and `y` to its height, both in `[0, 1]`;
//! - `r` (radius) and `source` (clone offset) share **one** length unit: the long edge. Sharing
//!   one unit for both axes keeps a circle a circle in pixel space — normalizing `r` against the
//!   width while `x` uses its own axis would stretch the stroke into an ellipse on a 16:9 frame.
//!
//! The radius is a circle (`r`, a single number) with no semi-axes: stretched strokes are the
//! product of a press-and-drag gesture, and getting "tap once, heal one spot" right is worth
//! more in a first version. The circle-only format also extends forward cleanly — adding `ry`
//! and `angle` later leaves old projects reading `ry = None` as a circle.

use crate::heal;
use serde::{Deserialize, Serialize};

/// 一个归一化的修复/仿制笔触。
///
/// One normalized healing / clone stroke.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpotSpec {
    /// 中心横坐标，相对画幅宽，`[0, 1]`（允许略微越界，采样按边界夹取）。
    ///
    /// Center x relative to the frame width, `[0, 1]`; slight overshoot is allowed and
    /// sampling clamps at the border.
    pub x: f32,
    /// 中心纵坐标，相对画幅高，`[0, 1]`。
    ///
    /// Center y relative to the frame height, `[0, 1]`.
    pub y: f32,
    /// 半径，相对画幅**长边**；`<= 0` 的笔触被跳过。
    ///
    /// Radius relative to the frame's **long edge**; strokes with `<= 0` are skipped.
    pub r: f32,
    /// 边缘羽化占半径的比例，`0..1`；0 为硬边，1 为从中心淡到边缘。
    ///
    /// Edge feather as a fraction of the radius, `0..1`; 0 is a hard edge and 1 fades from
    /// the center all the way out.
    pub feather: f32,
    /// 仿制源的偏移（相对长边）；`None` 表示修复（斑内解拉普拉斯，用周边像素当边界）。
    ///
    /// Clone source offset, relative to the long edge; `None` means heal (solve the Laplacian
    /// inside the spot with the surrounding pixels as the boundary).
    pub source: Option<(f32, f32)>,
}

impl Default for SpotSpec {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: 0.5,
            // 0.02 是「默认笔触」：1600 边预览上 32px、6000 边导出上 120px，
            // 后者正好盖住一颗灰尘——过小看不见效果，过大就抹掉结构。
            //
            // 0.02 is the default stroke: 32px on a 1600px preview and 120px on a 6000px
            // export, the latter just enough to cover a dust speck. Smaller is invisible,
            // larger erases structure.
            r: 0.02,
            feather: 0.35,
            source: None,
        }
    }
}

impl SpotSpec {
    /// 半径上限（相对长边）。超过之后单点修复要解的格点数按平方涨，而画面已被抹平，
    /// 不再有「修一个点」的意义；这一档在 6000 边导出上是 600px 半径。
    ///
    /// Upper bound on the radius (relative to the long edge). Beyond it the cell count in the
    /// Poisson solve grows quadratically while the image is already smeared flat — there is no
    /// "spot" left to heal. On a 6000px export this step is a 600px radius.
    pub const MAX_RADIUS: f32 = 0.1;

    /// 钳到合法域：坐标与半径夹进 `[0, 1]` / `(0, MAX_RADIUS]`，羽化夹进 `[0, 1]`。
    /// 非有限值一律判为不可用（返回 `None`）——NaN 混进渲染链会污染整幅图。
    ///
    /// Clamp into the legal domain: coordinates and radius into `[0, 1]` / `(0, MAX_RADIUS]`,
    /// feather into `[0, 1]`. Non-finite values are rejected outright (`None`): a NaN entering
    /// the render chain poisons the whole frame.
    pub fn sanitise(&self) -> Option<SpotSpec> {
        let finite = self.x.is_finite()
            && self.y.is_finite()
            && self.r.is_finite()
            && self.feather.is_finite()
            && self
                .source
                .is_none_or(|(dx, dy)| dx.is_finite() && dy.is_finite());
        if !finite {
            return None;
        }
        let r = self.r.clamp(0.0, Self::MAX_RADIUS);
        if r <= 0.0 {
            return None;
        }
        Some(SpotSpec {
            x: self.x.clamp(0.0, 1.0),
            y: self.y.clamp(0.0, 1.0),
            r,
            feather: self.feather.clamp(0.0, 1.0),
            source: self.source,
        })
    }

    /// 换算成像素笔触。画幅为空、半径非法或非有限时返回 `None`。
    ///
    /// Convert to a pixel stroke. Returns `None` for an empty frame, or a radius that is
    /// non-positive or non-finite.
    pub fn to_pixels(&self, w: usize, h: usize) -> Option<heal::Spot> {
        if w == 0 || h == 0 {
            return None;
        }
        let s = self.sanitise()?;
        let long = w.max(h) as f32;
        let r = s.r * long;
        if !(r > 0.0) {
            return None;
        }
        Some(heal::Spot {
            cx: s.x * w as f32,
            cy: s.y * h as f32,
            rx: r,
            ry: r,
            // 正圆：旋转角没有意义，但两个半轴相等时任何角度都等价。
            //
            // A circle: the rotation has no meaning, and with equal semi-axes every angle is
            // equivalent.
            angle_deg: 0.0,
            feather: s.feather,
            source: s.source.map(|(dx, dy)| (dx * long, dy * long)),
        })
    }
}

/// 把一串归一化笔触按顺序作用到缓冲区上，返回真正生效（换算成功）的笔触数。
/// 空表、空画幅、尺寸不符时逐位不动。
///
/// Apply a list of normalized strokes to the buffer in order, returning how many actually
/// took effect (converted successfully). An empty list, an empty frame or a size mismatch
/// leaves the buffer bit-identical.
pub fn apply(rgb: &mut [f32], w: usize, h: usize, spots: &[SpotSpec]) -> usize {
    if w == 0 || h == 0 || spots.is_empty() || rgb.len() < w * h * 3 {
        return 0;
    }
    let px: Vec<heal::Spot> = spots.iter().filter_map(|s| s.to_pixels(w, h)).collect();
    if px.is_empty() {
        return 0;
    }
    let out = heal::heal(&rgb[..w * h * 3], w, h, &px);
    if out.len() != w * h * 3 {
        return 0;
    }
    rgb[..w * h * 3].copy_from_slice(&out);
    px.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 横竖各一条渐变，保证「修复」有梯度可依、不会因为纯色而看不出差别。
    ///
    /// A horizontal and a vertical gradient, so healing has a gradient to work with instead of
    /// a flat field where every result looks alike.
    fn gradient(w: usize, h: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let t = (x as f32 / w as f32 + y as f32 / h as f32) * 0.5;
                let o = (y * w + x) * 3;
                v[o] = t;
                v[o + 1] = t;
                v[o + 2] = t;
            }
        }
        v
    }

    /// 空表必须逐位不动——判据的底座，后面几条都拿它当对照。
    ///
    /// An empty list must leave the buffer bit-identical: the base case every other test
    /// compares against.
    #[test]
    fn an_empty_list_leaves_the_buffer_alone() {
        let src = gradient(64, 48);
        let mut got = src.clone();
        assert_eq!(apply(&mut got, 64, 48, &[]), 0);
        assert_eq!(got, src);
    }

    /// 半径非法（0 / 负 / NaN）的笔触被跳过，且不改一个像素：一个坏参数不该拖垮整批。
    ///
    /// Strokes with an illegal radius (0, negative, NaN) are skipped and change nothing:
    /// one bad parameter must not take the whole batch down.
    #[test]
    fn strokes_with_an_illegal_radius_are_skipped() {
        let src = gradient(32, 32);
        for bad in [0.0, -0.1, f32::NAN] {
            let mut got = src.clone();
            let s = SpotSpec {
                r: bad,
                ..SpotSpec::default()
            };
            assert_eq!(apply(&mut got, 32, 32, &[s]), 0, "r={bad}");
            assert_eq!(got, src, "r={bad} 改动了像素");
        }
    }

    /// 同一份归一化笔触在两种分辨率下命中**同一块相对区域**：这是归一化的全部意义，
    /// 也是「预览上点一下、导出对得上」的判据。
    ///
    /// One normalized stroke hits the **same relative area** at two resolutions: that is the
    /// whole point of normalizing, and the test behind "taps in the preview line up in the
    /// export".
    #[test]
    fn the_same_spec_hits_the_same_relative_area_at_two_sizes() {
        // 用仿制而不是修复做这条判据：修复在**线性渐变**上解出来的就是原值，改动只剩舍入
        // 噪声，改动像素的集合是随机子集，质心会飘。仿制改的是有确定色差的源区域，改动
        // 覆盖整个笔触，质心与占比才都是确定量。
        //
        // Cloning rather than healing: healing a **linear gradient** returns the original
        // values, so the changed pixels are only rounding noise and their centroid wanders.
        // Cloning pulls from a source with a definite colour difference, so the change covers
        // the whole stroke and both centroid and share are deterministic.
        let spec = SpotSpec {
            x: 0.5,
            y: 0.5,
            r: 0.06,
            feather: 0.0,
            source: Some((0.2, 0.0)),
        };
        // 改动像素的质心与占比：前者验「位置对不对」，后者验「大小对不对」。只比「圆心
        // 那个像素变了没」会放过「笔触整体偏了半格」这种错。
        //
        // Centroid and share of the changed pixels: the former checks position, the latter
        // size. Comparing only "did the center pixel change" would let a half-cell offset pass.
        fn changed(buf: &[f32], before: &[f32], n: usize) -> (f64, f64, f64) {
            let (mut sx, mut sy, mut c) = (0.0f64, 0.0f64, 0usize);
            for y in 0..n {
                for x in 0..n {
                    let o = (y * n + x) * 3;
                    if buf[o..o + 3] != before[o..o + 3] {
                        sx += x as f64;
                        sy += y as f64;
                        c += 1;
                    }
                }
            }
            assert!(c > 0, "{n}: 一个像素都没改");
            (sx / c as f64, sy / c as f64, c as f64 / (n * n) as f64)
        }

        let mut small = gradient(100, 100);
        let mut big = gradient(200, 200);
        assert_eq!(apply(&mut small, 100, 100, &[spec]), 1);
        assert_eq!(apply(&mut big, 200, 200, &[spec]), 1);
        let (sx, sy, share_small) = changed(&small, &gradient(100, 100), 100);
        let (bx, by, share_big) = changed(&big, &gradient(200, 200), 200);
        assert!(
            (sx - 50.0).abs() < 1.5 && (sy - 50.0).abs() < 1.5,
            "小图质心 ({sx},{sy})"
        );
        assert!(
            (bx - 100.0).abs() < 1.5 && (by - 100.0).abs() < 1.5,
            "大图质心 ({bx},{by})"
        );
        // 半径 0.06 的圆占画面 π·0.06² ≈ 1.13%；两档尺寸的实测占比必须都在这个量级。
        //
        // A circle of r=0.06 covers π·0.06² ≈ 1.13% of the frame; both sizes must land there.
        for (n, share) in [(100, share_small), (200, share_big)] {
            assert!(
                (share - 0.0113).abs() < 0.002,
                "{n}: 改动占比 {share:.4} 偏离圆的面积"
            );
        }
    }

    /// 仿制源的偏移也按长边归一化：小图上取到的相对位置，大图上必须还是那个相对位置。
    ///
    /// The clone offset is normalized against the long edge too: the relative source position
    /// picked on the small frame must still be that position on the large frame.
    #[test]
    fn the_clone_offset_scales_with_the_frame() {
        // 左半黑、右半白：把中心（0.5）从右侧 (0.75) 仿制过来，结果必须接近白。
        //
        // Left half black, right half white: cloning the center (0.5) from the right (0.75)
        // must come out near white.
        let mut small = vec![0.0f32; 100 * 100 * 3];
        for y in 0..100 {
            for x in 50..100 {
                let o = (y * 100 + x) * 3;
                small[o] = 1.0;
                small[o + 1] = 1.0;
                small[o + 2] = 1.0;
            }
        }
        let mut big = vec![0.0f32; 200 * 200 * 3];
        for y in 0..200 {
            for x in 100..200 {
                let o = (y * 200 + x) * 3;
                big[o] = 1.0;
                big[o + 1] = 1.0;
                big[o + 2] = 1.0;
            }
        }
        let spec = SpotSpec {
            x: 0.5,
            y: 0.5,
            r: 0.05,
            feather: 0.0,
            source: Some((0.25, 0.0)),
        };
        assert_eq!(apply(&mut small, 100, 100, &[spec]), 1);
        assert_eq!(apply(&mut big, 200, 200, &[spec]), 1);
        for (n, buf) in [(100usize, &small), (200usize, &big)] {
            let o = ((n / 2) * n + n / 2) * 3;
            assert!(buf[o] > 0.99, "{n}: 仿制取错了源（{}）", buf[o]);
        }
    }

    /// 半径被 `MAX_RADIUS` 钳住：落盘值再大也不会让单点修复变成整幅求解。
    ///
    /// The radius is clamped by `MAX_RADIUS`: no persisted value can turn one spot into a
    /// whole-frame solve.
    #[test]
    fn the_radius_is_clamped_before_it_reaches_the_solver() {
        let huge = SpotSpec {
            r: 8.0,
            ..SpotSpec::default()
        };
        let s = huge.sanitise().expect("钳位后应当仍可用");
        assert_eq!(s.r, SpotSpec::MAX_RADIUS);
        let spot = s.to_pixels(1000, 800).expect("换算失败");
        assert_eq!(spot.rx, SpotSpec::MAX_RADIUS * 1000.0);
        assert_eq!(spot.rx, spot.ry, "正圆的两个半轴必须相等");
    }

    /// 尺寸不符（缓冲区短于 `w*h*3`）时一个字都不改：宁可不动，也不越界。
    ///
    /// When the buffer is shorter than `w*h*3` nothing is touched: better to do nothing than to
    /// run off the end.
    #[test]
    fn a_short_buffer_is_left_alone() {
        let src = gradient(16, 16);
        let mut got = src[..16 * 8 * 3].to_vec();
        let before = got.clone();
        assert_eq!(apply(&mut got, 16, 16, &[SpotSpec::default()]), 0);
        assert_eq!(got, before);
    }
}
