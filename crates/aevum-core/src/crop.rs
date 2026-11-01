//! 裁剪选区的几何：把手命中、拖框、挪框、缩放、微调。
//!
//! Geometry of the crop selection: handle hit-testing, dragging, moving, resizing, and nudging.
//!
//! 口径抄 `acceptance/reference/crop.py` 的选区一节——纯函数、不含界面。坐标一律是原图像素，
//! 界面负责把屏幕位置换算过来。左上角和宽高全取偶：4:2:0 色度抽样要求裁切边界
//! 成对，导出视频时奇数会被 ffmpeg 静默挪走一像素。
//!
//! The convention follows the selection section of `acceptance/reference/crop.py` — pure
//! functions, no UI. Coordinates are always original-image pixels; the UI is responsible for
//! converting screen positions. Top-left corners and widths/heights are all rounded to even:
//! 4:2:0 chroma subsampling requires crop boundaries to be even-paired, and odd values get
//! silently shifted by one pixel by ffmpeg on video export.

use crate::align::CropRect;
use serde::{Deserialize, Serialize};

/// 八向把手：四角 + 四边中点。
///
/// Eight-way handles: four corners plus the midpoints of the four sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

impl Handle {
    fn touches_north(self) -> bool {
        matches!(self, Handle::Nw | Handle::N | Handle::Ne)
    }

    fn touches_south(self) -> bool {
        matches!(self, Handle::Sw | Handle::S | Handle::Se)
    }

    fn touches_west(self) -> bool {
        matches!(self, Handle::Nw | Handle::W | Handle::Sw)
    }

    fn touches_east(self) -> bool {
        matches!(self, Handle::Ne | Handle::E | Handle::Se)
    }

    fn is_side(self) -> bool {
        matches!(self, Handle::N | Handle::S | Handle::E | Handle::W)
    }
}

/// 光标落在选区上的哪一类位置。
///
/// Which kind of position the cursor lands on over the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Corner(Handle),
    Edge(Handle),
    Inside,
    Outside,
}

/// 选区四边（屏幕坐标的 x0,y0,x1,y1），`hit_test` 的输入。
///
/// The four edges of a selection (`x0,y0,x1,y1` in screen coordinates), input to `hit_test`.
#[derive(Clone, Copy, Debug)]
pub struct Edges {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Edges {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Edges {
        Edges { x0, y0, x1, y1 }
    }
}

/// PS 风格的命中检测：角 > 边 > 内部 > 外部。
///
/// Photoshop-style hit testing: corners > edges > inside > outside.
pub fn hit_test(rect: Edges, px: f32, py: f32, corner: f32, edge: f32) -> Hit {
    for (handle, cx, cy) in [
        (Handle::Nw, rect.x0, rect.y0),
        (Handle::Ne, rect.x1, rect.y0),
        (Handle::Se, rect.x1, rect.y1),
        (Handle::Sw, rect.x0, rect.y1),
    ] {
        if (px - cx).abs() <= corner && (py - cy).abs() <= corner {
            return Hit::Corner(handle);
        }
    }
    if (py - rect.y0).abs() <= edge && px >= rect.x0 - edge && px <= rect.x1 + edge {
        return Hit::Edge(Handle::N);
    }
    if (py - rect.y1).abs() <= edge && px >= rect.x0 - edge && px <= rect.x1 + edge {
        return Hit::Edge(Handle::S);
    }
    if (px - rect.x0).abs() <= edge && py >= rect.y0 - edge && py <= rect.y1 + edge {
        return Hit::Edge(Handle::W);
    }
    if (px - rect.x1).abs() <= edge && py >= rect.y0 - edge && py <= rect.y1 + edge {
        return Hit::Edge(Handle::E);
    }
    if px > rect.x0 && px < rect.x1 && py > rect.y0 && py < rect.y1 {
        return Hit::Inside;
    }
    Hit::Outside
}

/// 整理选区：排序、夹在画面内、全部取偶。
///
/// Normalizes a selection: sorts, clamps to the frame, and rounds everything to even.
pub fn norm_rect(x0: f64, y0: f64, x1: f64, y1: f64, frame_w: u32, frame_h: u32) -> CropRect {
    let fw = frame_w as i64;
    let fh = frame_h as i64;
    let (mut x0, mut x1) = (x0.round() as i64, x1.round() as i64);
    if x1 < x0 {
        std::mem::swap(&mut x0, &mut x1);
    }
    let (mut y0, mut y1) = (y0.round() as i64, y1.round() as i64);
    if y1 < y0 {
        std::mem::swap(&mut y0, &mut y1);
    }
    let mut w = clampi(x1 - x0, 2, fw.max(2));
    let mut h = clampi(y1 - y0, 2, fh.max(2));
    w -= w & 1;
    h -= h & 1;
    let mut x = clampi(x0, 0, fw - w);
    let mut y = clampi(y0, 0, fh - h);
    x -= x & 1;
    y -= y & 1;
    if x + w > fw {
        w = fw - x;
        w -= w & 1;
    }
    if y + h > fh {
        h = fh - y;
        h -= h & 1;
    }
    CropRect {
        x: x.max(0) as u32,
        y: y.max(0) as u32,
        w: w.max(0) as u32,
        h: h.max(0) as u32,
    }
}

/// 按比例在画面里放一个居中的最大选区——切比例后先给用户看到的东西。
///
/// Places the largest centered selection for the aspect inside the frame —
/// what the user sees first after switching aspect.
pub fn center_rect(frame_w: u32, frame_h: u32, aspect: Option<(u32, u32)>) -> CropRect {
    let (fw, fh) = (frame_w as f64, frame_h as f64);
    let (w, h) = match aspect {
        Some((num, den)) => {
            let k = even_step((fw / num as f64).min(fh / den as f64)) as f64;
            (num as f64 * k, den as f64 * k)
        }
        None => (fw - fw % 2.0, fh - fh % 2.0),
    };
    norm_rect(
        (fw - w) / 2.0,
        (fh - h) / 2.0,
        (fw + w) / 2.0,
        (fh + h) / 2.0,
        frame_w,
        frame_h,
    )
}

/// 拉直之后的安全取景框：整幅绕中心转 `angle_deg`（正角=逆时针）后，
/// 四个角仍然落在原始像素里的最大居中选区——框外就是转出来的空白角。
///
/// The safe viewfinder after straightening: after the whole frame rotates about its center by
/// `angle_deg` (positive = counterclockwise), the largest centered selection whose corners
/// still land on original pixels — outside the box lies the blank corner rotated in.
///
/// 判据不用搜索：选区反旋回去要装进画面，等价于两条线性约束
/// `w·|cosθ| + h·|sinθ| ≤ 帧宽`、`w·|sinθ| + h·|cosθ| ≤ 帧高`。
/// 代 `w = num·k, h = den·k` 各解一个 `k` 上限，取小的那个；框的任何子选区角上
/// 只会更靠里，所以用户框只要落在这个框里就不会带到空白。
///
/// No search is needed for the criterion: a selection rotated back must fit the frame, which
/// equals two linear constraints `w·|cosθ| + h·|sinθ| ≤ frame width` and
/// `w·|sinθ| + h·|cosθ| ≤ frame height`. Substituting `w = num·k, h = den·k` solves a `k` cap
/// for each; take the smaller one. Corners of any sub-selection only sit further inside, so a
/// user box within this box never picks up blank space.
pub fn safe_rect(
    frame_w: u32,
    frame_h: u32,
    angle_deg: f64,
    aspect: Option<(u32, u32)>,
) -> CropRect {
    let t = angle_deg.abs().to_radians();
    let (c, s) = (t.cos(), t.sin());
    if s <= 0.0 {
        // 没转：整幅（或按比例内接）本身就是安全框。
        return center_rect(frame_w, frame_h, aspect);
    }
    let (fw, fh) = (frame_w as f64, frame_h as f64);
    let (num, den) = match aspect {
        Some((n, d)) if n > 0 && d > 0 => (n as f64, d as f64),
        _ => (fw.max(1.0), fh.max(1.0)),
    };
    // 左上角取偶最多让框心偏出 1px，反旋回去角上要多算 `(cos+sin)·1px`，两边各留一份。
    let pad = 2.0 * (c + s);
    let k = ((fw - pad) / (num * c + den * s)).min((fh - pad) / (num * s + den * c));
    let even = |v: f64| {
        let f = v.max(2.0).floor();
        f - f % 2.0
    };
    let (w, h) = (even(num * k), even(den * k));
    norm_rect(
        (fw - w) / 2.0,
        (fh - h) / 2.0,
        (fw + w) / 2.0,
        (fh + h) / 2.0,
        frame_w,
        frame_h,
    )
}

/// 已有选区换比例：绕中心缩放，不把它甩到别处去。
///
/// Refits an existing selection to a new aspect: scales around its center without
/// throwing it elsewhere.
pub fn refit_rect(
    sel: CropRect,
    frame_w: u32,
    frame_h: u32,
    aspect: Option<(u32, u32)>,
) -> CropRect {
    let (x, y) = (sel.x as f64, sel.y as f64);
    let (w, h) = (sel.w as f64, sel.h as f64);
    let Some((num, den)) = aspect else {
        return norm_rect(x, y, x + w, y + h, frame_w, frame_h);
    };
    let (num, den) = (num as f64, den as f64);
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let t = (if num >= den { w / num } else { h / den })
        .min(frame_w as f64 / num)
        .min(frame_h as f64 / den);
    let k = even_step(t) as f64;
    let (w2, h2) = (num * k, den * k);
    norm_rect(
        cx - w2 / 2.0,
        cy - h2 / 2.0,
        cx + w2 / 2.0,
        cy + h2 / 2.0,
        frame_w,
        frame_h,
    )
}

/// 空白处拖出新选区。定比例时以按下点为角，取两轴里更大的那个推进。
///
/// Drags a new selection out of empty space. With a locked aspect the press point is a corner,
/// and the larger of the two axes drives the growth.
pub fn draw_rect(
    anchor: (f64, f64),
    px: f64,
    py: f64,
    frame_w: u32,
    frame_h: u32,
    aspect: Option<(u32, u32)>,
) -> CropRect {
    let (ax, ay) = anchor;
    let Some((num, den)) = aspect else {
        return norm_rect(ax, ay, px, py, frame_w, frame_h);
    };
    let (num, den) = (num as f64, den as f64);
    let (fw, fh) = (frame_w as f64, frame_h as f64);
    let (dx, dy) = (px - ax, py - ay);
    let max_t = (fw / num)
        .min(fh / den)
        .min(if dx >= 0.0 { (fw - ax) / num } else { ax / num })
        .min(if dy >= 0.0 { (fh - ay) / den } else { ay / den });
    let t = (dx.abs() / num).max(dy.abs() / den).min(max_t).max(0.0);
    let k = even_step(t) as f64;
    let (w, h) = (num * k, den * k);
    CropRect {
        x: clampi(
            if dx >= 0.0 { ax } else { ax - w } as i64,
            0,
            (fw - w) as i64,
        ) as u32,
        y: clampi(
            if dy >= 0.0 { ay } else { ay - h } as i64,
            0,
            (fh - h) as i64,
        ) as u32,
        w: w as u32,
        h: h as u32,
    }
}

/// 框内拖动：只平移，左上角取偶。
///
/// Dragging inside the box: translates only; the top-left corner is rounded to even.
pub fn move_rect(sel: CropRect, x: f64, y: f64, frame_w: u32, frame_h: u32) -> CropRect {
    let mut x = clampi(x.round() as i64, 0, frame_w as i64 - sel.w as i64);
    let mut y = clampi(y.round() as i64, 0, frame_h as i64 - sel.h as i64);
    x -= x & 1;
    y -= y & 1;
    CropRect {
        x: x.max(0) as u32,
        y: y.max(0) as u32,
        w: sel.w,
        h: sel.h,
    }
}

/// 拖某个把手改大小。定比例时以对角为锚点，短边跟着长边走。
///
/// Resizes by dragging a handle. With a locked aspect the opposite corner is the anchor and the
/// shorter side follows the longer one.
pub fn resize_rect(
    handle: Handle,
    orig: CropRect,
    px: f64,
    py: f64,
    frame_w: u32,
    frame_h: u32,
    aspect: Option<(u32, u32)>,
) -> CropRect {
    let (ox, oy) = (orig.x as f64, orig.y as f64);
    let (ow, oh) = (orig.w as f64, orig.h as f64);
    let (fw, fh) = (frame_w as f64, frame_h as f64);
    let px = clampi(px.round() as i64, 0, frame_w as i64) as f64;
    let py = clampi(py.round() as i64, 0, frame_h as i64) as f64;
    let north = handle.touches_north();
    let west = handle.touches_west();
    let (w, h, x, y) = match aspect {
        None => {
            let raw_w = if west {
                ox + ow - px
            } else if handle.touches_east() {
                px - ox
            } else {
                ow
            };
            let raw_h = if north {
                oy + oh - py
            } else if handle.touches_south() {
                py - oy
            } else {
                oh
            };
            let w = even_down(raw_w.max(2.0).min(fw.max(2.0)));
            let h = even_down(raw_h.max(2.0).min(fh.max(2.0)));
            (
                w,
                h,
                if west { ox + ow - w } else { ox },
                if north { oy + oh - h } else { oy },
            )
        }
        Some((num, den)) => {
            let (num, den) = (num as f64, den as f64);
            if handle.is_side() && (north || handle.touches_south()) {
                // 上下边：高度驱动宽度，水平方向绕选区中心对称。
                let base = if north { oy + oh } else { oy };
                let t = if north { base - py } else { py - oy };
                let cx = ox + (orig.w / 2) as f64;
                let max_t = (2.0 * cx.min(fw - cx) / num).min(if north {
                    base / den
                } else {
                    (fh - oy) / den
                });
                let k = even_step((t / den).max(0.0).min(max_t)) as f64;
                let (w, h) = (num * k, den * k);
                (w, h, cx - w / 2.0, if north { base - h } else { oy })
            } else if handle.is_side() {
                // 左右边：宽度驱动高度，垂直方向绕选区中心对称。
                let base = if west { ox + ow } else { ox };
                let t = if west { base - px } else { px - ox };
                let cy = oy + (orig.h / 2) as f64;
                let max_t = (2.0 * cy.min(fh - cy) / den).min(if west {
                    base / num
                } else {
                    (fw - ox) / num
                });
                let k = even_step((t / num).max(0.0).min(max_t)) as f64;
                let (w, h) = (num * k, den * k);
                (w, h, if west { base - w } else { ox }, cy - h / 2.0)
            } else {
                let ax = if west { ox + ow } else { ox };
                let ay = if north { oy + oh } else { oy };
                let wd = if west { ax - px } else { px - ax };
                let hd = if north { ay - py } else { py - ay };
                let max_t = (if west { ax / num } else { (fw - ax) / num }).min(if north {
                    ay / den
                } else {
                    (fh - ay) / den
                });
                let k = even_step((wd / num).max(hd / den).min(max_t).max(0.0)) as f64;
                let (w, h) = (num * k, den * k);
                (
                    w,
                    h,
                    if west { ax - w } else { ax },
                    if north { ay - h } else { oy },
                )
            }
        }
    };

    let mut x = clampi(x as i64, 0, (fw - w) as i64).max(0);
    let mut y = clampi(y as i64, 0, (fh - h) as i64).max(0);
    x -= x & 1;
    y -= y & 1;
    CropRect {
        x: x as u32,
        y: y as u32,
        w: w.max(2.0) as u32,
        h: h.max(2.0) as u32,
    }
}

/// 方向键微调：一格 2 像素（保证仍是偶数），按住 Shift 加大格。
///
/// Arrow-key nudging: one step is 2 pixels (stays even); holding Shift uses a larger step.
pub fn nudge_rect(
    sel: CropRect,
    dx: i64,
    dy: i64,
    step: u32,
    frame_w: u32,
    frame_h: u32,
) -> CropRect {
    CropRect {
        x: clampi(
            sel.x as i64 + dx * step as i64,
            0,
            frame_w as i64 - sel.w as i64,
        )
        .max(0) as u32,
        y: clampi(
            sel.y as i64 + dy * step as i64,
            0,
            frame_h as i64 - sel.h as i64,
        )
        .max(0) as u32,
        w: sel.w,
        h: sel.h,
    }
}

/// 两个选区的交集，空了就返回 `None`。用户框要先落进对齐裁切里才谈得上画幅。
///
/// The intersection of two selections, or `None` when empty. A user box must land inside the
/// alignment crop before aspect can even be discussed.
pub fn intersect(a: CropRect, b: CropRect) -> Option<CropRect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = (a.x + a.w).min(b.x + b.w);
    let t = (a.y + a.h).min(b.y + b.h);
    if r <= x || t <= y {
        return None;
    }
    Some(CropRect {
        x,
        y,
        w: r - x,
        h: t - y,
    })
}

// ------------------------------------------------------------------ 运镜（Ken Burns）
// 一张静止画面（或机位锁死的素材）靠取景框随时间走动产生镜头感。框用归一化浮点
// 表示，最后交采样器重画，所以推近的过程是连续的，不会被抹成整像素台阶。
// 口径抄 `acceptance/reference/crop.py` 的运镜一节。

/// 缓动：改的就是「时间 → 进度」那条坡，四档与 Python 的 `KB_EASES` 一一对应。
///
/// Easing: changes the time-to-progress slope; the four presets map one-to-one to
/// Python `KB_EASES`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ease {
    /// 两头慢中间快（默认）：`3t² − 2t³`。
    ///
    /// Slow at both ends, fast in the middle (default): `3t² − 2t³`.
    #[default]
    Smooth,
    Linear,
    In,
    Out,
}

impl Ease {
    pub const ALL: [Ease; 4] = [Ease::Smooth, Ease::Linear, Ease::In, Ease::Out];

    /// 工程文件与命令面用的键。
    ///
    /// The key used in project files and on the command surface.
    pub fn key(self) -> &'static str {
        match self {
            Ease::Smooth => "smooth",
            Ease::Linear => "linear",
            Ease::In => "in",
            Ease::Out => "out",
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            Ease::Smooth => "缓入缓出",
            Ease::Linear => "匀速",
            Ease::In => "越推越快",
            Ease::Out => "越推越慢",
        }
    }

    pub fn parse(raw: &str) -> Option<Ease> {
        Self::ALL.into_iter().find(|e| e.key() == raw)
    }

    /// 0..1 的时间 → 0..1 的进度。
    ///
    /// Time in 0..1 mapped to progress in 0..1.
    pub fn curve(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Ease::Linear => t,
            Ease::In => t * t * t,
            Ease::Out => 1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t),
            Ease::Smooth => t * t * (3.0 - 2.0 * t),
        }
    }
}

/// 一个取景框：归一化到「参与运镜的那一幅」的左上角与宽高，0..1。
///
/// A viewfinder: top-left corner and size normalized to 0..1 over the frame that takes part
/// in the motion.
///
/// 用归一化而不是像素：预览是缩过的、导出是全尺寸，同一个框要两边都对得上。
///
/// Normalized rather than pixel-based: the preview is scaled down while export is full size,
/// and the same box must line up on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Viewfinder {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 整幅画面：运镜不开、或者只推不摇时的起点。
///
/// The full frame: the start point when motion is off, or for push-only without panning.
pub const FULL: Viewfinder = Viewfinder {
    x: 0.0,
    y: 0.0,
    w: 1.0,
    h: 1.0,
};

impl Viewfinder {
    /// 以 `(cx, cy)` 为中心、铺一个占画面 `span`（0..1 的边长比例）的框。
    /// 换比例重铺、给一个默认终点都用它。
    ///
    /// Lays out a box centered on `(cx, cy)` spanning `span` of the frame (edge lengths as
    /// 0..1 proportions). Used for refitting on an aspect change and for default end boxes.
    pub fn centered(cx: f64, cy: f64, span_w: f64, span_h: f64) -> Self {
        Self {
            x: cx - span_w / 2.0,
            y: cy - span_h / 2.0,
            w: span_w,
            h: span_h,
        }
    }

    /// 夹进画内、宽高兜到最小一档。NaN 或负宽高一律退回整幅——宁可不动，也不炸。
    ///
    /// Clamps into the frame and floors width/height at the minimum. NaN or negative sizes
    /// fall back to the full frame — better frozen than blowing up.
    pub fn normalized(self) -> Self {
        let w = if self.w.is_finite() { self.w } else { 0.0 };
        let h = if self.h.is_finite() { self.h } else { 0.0 };
        if w <= 0.0 || h <= 0.0 {
            return FULL;
        }
        let w = w.clamp(MIN_SPAN, 1.0);
        let h = h.clamp(MIN_SPAN, 1.0);
        let x = if self.x.is_finite() { self.x } else { 0.0 };
        let y = if self.y.is_finite() { self.y } else { 0.0 };
        Self {
            x: x.clamp(0.0, 1.0 - w),
            y: y.clamp(0.0, 1.0 - h),
            w,
            h,
        }
    }

    pub fn aspect(self) -> f64 {
        self.w / self.h
    }

    /// 换算成 `frame_w × frame_h` 那一幅上的浮点像素窗口 `(x, y, w, h)`，并收成
    /// `out_w × out_h` 那个画幅的比例：只往里收、中心不动。
    ///
    /// Converts to a floating-point pixel window `(x, y, w, h)` over the `frame_w × frame_h`
    /// frame, then trims it to the aspect of the `out_w × out_h` output: trimming inward only,
    /// the center stays put.
    ///
    /// 比例必须在**像素**口径上对：归一化的框横竖各按自己的边长走，源幅不是正方形时
    /// `w/h` 并不等于框的像素比例，拿它去比对输出画幅就会把画面拉扁。
    /// 收成输出比例而不是加黑边：框是用户选好的构图，宁可少留一圈，也不拉伸、也不垫底。
    ///
    /// The aspect must match in **pixel** terms: a normalized box scales each axis by its own
    /// edge length, so when the source frame is not square `w/h` is not the box's pixel aspect,
    /// and comparing it against the output aspect would squash the picture.
    /// Trim to the output aspect instead of adding black bars: the box is the composition the
    /// user picked; better to lose a rim than to stretch or letterbox.
    pub fn view_window(self, frame_w: u32, frame_h: u32, out_w: u32, out_h: u32) -> [f64; 4] {
        let s = self.normalized();
        let (fw, fh) = (frame_w as f64, frame_h as f64);
        let mut win = [s.x * fw, s.y * fh, s.w * fw, s.h * fh];
        let aspect = if out_w > 0 && out_h > 0 {
            out_w as f64 / out_h as f64
        } else {
            0.0
        };
        if aspect > 0.0 && aspect.is_finite() && win[2] > 0.0 && win[3] > 0.0 {
            if win[2] / win[3] > aspect {
                let w = win[3] * aspect;
                win[0] += (win[2] - w) / 2.0;
                win[2] = w;
            } else {
                let h = win[2] / aspect;
                win[1] += (win[3] - h) / 2.0;
                win[3] = h;
            }
        }
        win
    }

    /// 中心点，画箭头与读数用。
    ///
    /// The center point, used for drawing the arrow and the readout.
    pub fn center(self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// 两个框之间取第 `t` 个（`t` 已经是缓动后的进度）。
    /// 两端同比例时结果也同比例：分子分母走的是同一条坡。
    ///
    /// The box at fraction `t` between two boxes (`t` is already the eased progress).
    /// When both ends share the same aspect the result keeps that aspect: numerator and
    /// denominator ride the same slope.
    pub fn lerp(self, other: Self, t: f64) -> Self {
        let mix = |a: f64, b: f64| (1.0 - t) * a + t * b;
        Self {
            x: mix(self.x, other.x),
            y: mix(self.y, other.y),
            w: mix(self.w, other.w),
            h: mix(self.h, other.h),
        }
    }

    /// 换算成 `w × h` 那一幅上的浮点像素窗口 `(x, y, w, h)`。
    ///
    /// Converts to a floating-point pixel window `(x, y, w, h)` over the `w × h` frame.
    pub fn pixels(self, w: u32, h: u32) -> [f64; 4] {
        let s = self.normalized();
        [
            s.x * w as f64,
            s.y * h as f64,
            s.w * w as f64,
            s.h * h as f64,
        ]
    }

    /// 这个框被放大几倍才填满输出画幅：**整幅按输出比例裁进画面**（`view_window`）是 1×，
    /// 框收小到一半就是 2×。>1 说明已经在拿少数像素撑画面——界面按它提醒「再推就糊」。
    ///
    /// How many times the box must be blown up to fill the output aspect: the **full frame
    /// cropped into the picture at output aspect** (`view_window`) is 1×, and a box shrunk to
    /// half is 2×. A value >1 means a few pixels are already stretching the picture — the UI
    /// uses it to warn that "one more push goes blurry".
    ///
    /// 以整幅为 1× 而不是「输出像素 ÷ 框像素」：720p 预览导出 4K 本来就放大 3×，
    /// 那一步与运镜无关，算到推近头上会让人一开面板就看到一条假警告。
    ///
    /// The full frame counts as 1× rather than "output pixels ÷ box pixels": exporting 4K from
    /// a 720p preview is already a 3× upscale unrelated to motion; charging it to the push
    /// would show a false warning the moment the panel opens.
    pub fn zoom(self, out_w: u32, out_h: u32, frame_w: u32, frame_h: u32) -> f64 {
        if frame_w == 0 || frame_h == 0 {
            return 1.0;
        }
        let full = FULL.view_window(frame_w, frame_h, out_w, out_h);
        let win = self.view_window(frame_w, frame_h, out_w, out_h);
        if win[2] <= 0.0 || full[2] <= 0.0 {
            return 1.0;
        }
        full[2] / win[2]
    }
}

/// 宽高下限：再小就只剩插值噪声，而且读数会抖得没法看。
///
/// The width/height floor: any smaller leaves only interpolation noise, and the readout
/// shakes unreadably.
const MIN_SPAN: f64 = 0.02;

/// 运镜设定：取景框从 `from` 走到 `to`，走的是 `ease` 那条坡。
///
/// A motion setup: the viewfinder travels from `from` to `to` along the `ease` slope.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Motion {
    pub enabled: bool,
    pub from: Viewfinder,
    pub to: Viewfinder,
    pub ease: Ease,
}

impl Default for Motion {
    fn default() -> Self {
        // 默认终点往里收一档：一打开就该看到镜头在动，而不是两个重合的框。
        Self {
            enabled: false,
            from: FULL,
            to: Viewfinder::centered(0.5, 0.5, 0.85, 0.85),
            ease: Ease::Smooth,
        }
    }
}

impl Motion {
    /// 真的会在动：开关开着，而且两个框没重合。重合时导出走原来的快路，
    /// 一像素都不重采样——开着运镜但还没调框，不该白白掉一档清晰度。
    ///
    /// Whether the motion actually moves: the switch is on and the two boxes do not coincide.
    /// When they coincide, export takes the original fast path without resampling a single
    /// pixel — motion enabled but not yet tuned should not cost a step of sharpness for
    /// nothing.
    pub fn is_active(self) -> bool {
        self.enabled && (self.from.normalized() != self.to.normalized())
    }

    /// 第 `t` 个取景框（`t` 是 0..1 的时间进度）。
    ///
    /// The viewfinder at time fraction `t` (0..1).
    pub fn view_at(self, t: f64) -> Viewfinder {
        self.from
            .normalized()
            .lerp(self.to.normalized(), Ease::curve(self.ease, t))
    }

    /// 第 `i` 帧（共 `n` 帧）的取景框。只有一帧就停在起点，
    /// 别让单帧片子跑出一条算错的坡。
    ///
    /// The viewfinder for frame `i` of `n` frames. With a single frame it stays at the start
    /// instead of running a wrongly computed slope.
    pub fn view_of(self, i: usize, n: usize) -> Viewfinder {
        if n <= 1 {
            return self.from.normalized();
        }
        self.view_at(i as f64 / (n - 1) as f64)
    }

    /// 换比例时两个框一起重铺：按起点框的变化把终点框等比例带过去。
    /// 各框各自重铺会把它们夹成同一个最大框，运镜幅度就这样悄悄归零了。
    ///
    /// Refits both boxes together on an aspect change: the end box is carried over
    /// proportionally by the start box's change. Refitting each box on its own would clamp
    /// them into the same largest box, silently zeroing the motion amplitude.
    pub fn refit(self, span_w: f64, span_h: f64) -> Self {
        let from = self.from.normalized();
        let (fcx, fcy) = from.center();
        let next_from = Viewfinder::centered(fcx, fcy, span_w, span_h).normalized();
        let fx = (self.to.x - from.x) / from.w;
        let fy = (self.to.y - from.y) / from.h;
        let fw = self.to.w / from.w;
        let fh = self.to.h / from.h;
        let next_to = Viewfinder {
            x: next_from.x + fx * next_from.w,
            y: next_from.y + fy * next_from.h,
            w: fw * next_from.w,
            h: fh * next_from.h,
        }
        .normalized();
        Self {
            from: next_from,
            to: next_to,
            ..self
        }
    }
}

/// 命令与 MCP 共用的补丁写法：只改传来的那几键，其余保持当前值。
///
/// The patch spelling shared by the command surface and MCP: only the given keys change, the
/// rest keep their current values.
///
/// 放核心而不是各端各写一份：界面面板、控制通道、MCP 三条路必须报同一句错、
/// 认同一种框的写法，否则同一份参数在三边得到三个结果。
///
/// Lives in the core instead of one copy per front end: the UI panel, the control channel,
/// and MCP must report the same error and agree on one box spelling, or the same parameters
/// yield three results on three sides.
impl Motion {
    pub fn patched(&self, raw: &serde_json::Value) -> Result<Self, String> {
        if raw.get("reset").is_some_and(|v| v.as_bool() == Some(true)) {
            return Ok(Motion::default());
        }
        if let Some(v) = raw.get("reset")
            && !v.is_null()
            && v.as_bool() != Some(false)
        {
            return Err(
                "reset 只认布尔：true 清空整块，false 什么都不清（要改就逐项写 enabled / from / to / ease）".to_owned(),
            );
        }
        let mut next = *self;
        if let Some(v) = raw.get("enabled").filter(|v| !v.is_null()) {
            next.enabled = v.as_bool().ok_or("enabled 必须是布尔")?;
        }
        let mut box_given = false;
        for key in ["from", "to"] {
            if let Some(v) = raw.get(key).filter(|v| !v.is_null()) {
                let box_ = viewfinder_arg(v, key)?;
                if key == "from" {
                    next.from = box_;
                } else {
                    next.to = box_;
                }
                box_given = true;
            }
        }
        if let Some(v) = raw.get("ease").filter(|v| !v.is_null()) {
            let name = v
                .as_str()
                .ok_or("ease 要写成字符串：smooth / linear / in / out")?;
            next.ease = Ease::parse(name)
                .ok_or_else(|| format!("不认识的运动曲线 {name}：要 smooth / linear / in / out"))?;
        }
        if raw
            .get("swap")
            .is_some_and(|v| v.as_bool().unwrap_or(false))
        {
            std::mem::swap(&mut next.from, &mut next.to);
        }
        if let Some(v) = raw.get("refit").filter(|v| !v.is_null()) {
            let (w, h) = pair_arg(v, "refit")?;
            if w <= 0.0 || h <= 0.0 {
                return Err(format!("refit 的宽高要是正数（新画幅的跨度）：[{w}, {h}]"));
            }
            next = next.refit(w, h);
        }
        // 给了框就是当真要运镜，省掉「先 enabled 再 from/to」两步。
        if box_given {
            next.enabled = true;
        }
        Ok(next)
    }
}

/// 取景框参数：`[x, y, 宽, 高]` 或 `{x, y, w, h}`，相对那一幅归一化。
/// 只认数字；越界与退化交给 `Viewfinder::normalized()`，不在这里判。
///
/// A viewfinder argument: `[x, y, w, h]` or `{x, y, w, h}`, normalized to the frame.
/// Only numbers are accepted; out-of-range and degenerate values are left to
/// `Viewfinder::normalized()`, not checked here.
pub fn viewfinder_arg(raw: &serde_json::Value, field: &str) -> Result<Viewfinder, String> {
    let num = |v: &serde_json::Value| v.as_f64().filter(|n| n.is_finite());
    let (x, y, w, h) = match raw {
        serde_json::Value::Array(items) => {
            if items.len() != 4 {
                return Err(format!(
                    "{field} 要写成四个数 [x, y, w, h]（归一化 0..1），实得 {} 个",
                    items.len()
                ));
            }
            (
                num(&items[0]).ok_or(format!("{field} 的 x 不是有限数值"))?,
                num(&items[1]).ok_or(format!("{field} 的 y 不是有限数值"))?,
                num(&items[2]).ok_or(format!("{field} 的宽不是有限数值"))?,
                num(&items[3]).ok_or(format!("{field} 的高不是有限数值"))?,
            )
        }
        serde_json::Value::Object(map) => {
            let take = |key: &str| {
                map.get(key)
                    .and_then(num)
                    .ok_or_else(|| format!("{field}.{key} 要写成有限数值"))
            };
            (take("x")?, take("y")?, take("w")?, take("h")?)
        }
        _ => return Err(format!("{field} 要写成 {{x, y, w, h}} 或 [x, y, w, h]")),
    };
    Ok(Viewfinder { x, y, w, h })
}

/// 成对参数：`[宽, 高]` 或 `{w, h}` / `{width, height}`；换画幅跨度（refit）用它。
///
/// A pair argument: `[w, h]` or `{w, h}` / `{width, height}`; used for the refit span on an
/// aspect change.
pub fn pair_arg(raw: &serde_json::Value, field: &str) -> Result<(f64, f64), String> {
    let num = |v: &serde_json::Value| v.as_f64().filter(|n| n.is_finite());
    match raw {
        serde_json::Value::Array(items) if items.len() == 2 => Ok((
            num(&items[0]).ok_or(format!("{field} 的宽不是有限数值"))?,
            num(&items[1]).ok_or(format!("{field} 的高不是有限数值"))?,
        )),
        serde_json::Value::Object(map) => {
            let w = map
                .get("w")
                .and_then(num)
                .or_else(|| map.get("width").and_then(num));
            let h = map
                .get("h")
                .and_then(num)
                .or_else(|| map.get("height").and_then(num));
            match (w, h) {
                (Some(w), Some(h)) => Ok((w, h)),
                _ => Err(format!("{field} 要写成 [宽, 高] 或 {{w, h}}")),
            }
        }
        _ => Err(format!("{field} 要写成 [宽, 高] 或 {{w, h}}")),
    }
}

/// `int(t / 2) * 2` 再兜到最小 2：定比例的推进步长只能成对走。
fn even_step(t: f64) -> u32 {
    ((t / 2.0).floor() * 2.0).max(2.0) as u32
}

fn even_down(v: f64) -> f64 {
    let mut i = v as i64;
    i -= i & 1;
    i.max(2) as f64
}

/// Python 侧的 `clamp(v, lo, hi)`：`hi < lo` 时取 `lo`，而不是炸。
fn clampi(v: i64, lo: i64, hi: i64) -> i64 {
    if hi < lo { lo } else { v.clamp(lo, hi) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, w: u32, h: u32) -> CropRect {
        CropRect { x, y, w, h }
    }

    #[test]
    fn normalized_rects_are_clamped_and_even() {
        assert_eq!(
            norm_rect(10.0, 20.0, 110.0, 260.0, 400, 300),
            rect(10, 20, 100, 240)
        );
        // 拖反了照样是同一个框。
        assert_eq!(
            norm_rect(110.0, 260.0, 10.0, 20.0, 400, 300),
            rect(10, 20, 100, 240)
        );
        // 奇数的边长和左上角都被抹平成偶数。
        let r = norm_rect(11.0, 21.0, 112.0, 262.0, 400, 300);
        assert_eq!((r.x % 2, r.y % 2, r.w % 2, r.h % 2), (0, 0, 0, 0));
        assert_eq!(r, rect(10, 20, 100, 240));
    }

    #[test]
    fn normalized_rect_stays_inside_a_small_frame() {
        assert_eq!(
            norm_rect(-50.0, -50.0, 5000.0, 5000.0, 640, 480),
            rect(0, 0, 640, 480)
        );
        // 一像素宽的画幅：取偶之后剩 0 宽，不 panic 也不给出越界框。
        let r = norm_rect(0.0, 0.0, 9.0, 9.0, 1, 8);
        assert_eq!((r.x, r.y), (0, 0));
        assert!(r.w <= 1 && r.h <= 8);
    }

    #[test]
    fn center_rect_fits_the_largest_aspect_box() {
        assert_eq!(center_rect(1000, 600, None), rect(0, 0, 1000, 600));
        assert_eq!(center_rect(1000, 600, Some((1, 1))), rect(200, 0, 600, 600));
        // 16:9 的步长取 62 → 992×558，居中后左上角再被抹平成偶数（21 → 20）。
        assert_eq!(center_rect(1000, 600, Some((16, 9))), rect(4, 20, 992, 558));
    }

    #[test]
    fn refit_keeps_the_center_still() {
        let start = rect(100, 100, 400, 400);
        let r = refit_rect(start, 1200, 800, Some((16, 9)));
        assert_eq!((r.x + r.w / 2, r.y + r.h / 2), (300, 300));
        assert_eq!((r.w % 2, r.h % 2), (0, 0));
        // 换回自由：原框一个像素不动。
        assert_eq!(refit_rect(start, 1200, 800, None), start);
    }

    #[test]
    fn drag_from_a_corner_with_aspect_locked() {
        let r = draw_rect((100.0, 100.0), 300.0, 280.0, 1000, 800, Some((1, 1)));
        assert_eq!(r, rect(100, 100, 200, 200));
        // 往左上拖，框跟着翻到锚点的另一侧。
        let r = draw_rect((500.0, 500.0), 300.0, 300.0, 1000, 800, Some((1, 1)));
        assert_eq!(r, rect(300, 300, 200, 200));
        // 拖到画外：步长被 max_t 夹住，框顶到画面边为止。
        let r = draw_rect((900.0, 700.0), 1000.0, 800.0, 1000, 800, Some((1, 1)));
        assert_eq!((r.x + r.w, r.y + r.h), (1000, 800));
    }

    #[test]
    fn free_drag_is_just_the_two_corners() {
        assert_eq!(
            draw_rect((10.0, 20.0), 210.0, 220.0, 1000, 800, None),
            rect(10, 20, 200, 200)
        );
    }

    #[test]
    fn move_only_translates() {
        assert_eq!(
            move_rect(rect(100, 100, 200, 150), 33.7, 45.2, 1000, 800),
            rect(34, 44, 200, 150)
        );
        assert_eq!(
            move_rect(rect(100, 100, 200, 150), -9999.0, 9999.0, 1000, 800),
            rect(0, 650, 200, 150)
        );
    }

    #[test]
    fn resize_free_handle_follows_the_pointer() {
        let orig = rect(100, 100, 200, 200);
        assert_eq!(
            resize_rect(Handle::Se, orig, 350.0, 350.0, 1000, 800, None),
            rect(100, 100, 250, 250)
        );
        // 拖过对角：宽高各自兜到最小 2，锚点不动。
        let r = resize_rect(Handle::Nw, orig, 400.0, 400.0, 1000, 800, None);
        assert_eq!((r.w, r.h), (2, 2));
        assert_eq!((r.x + r.w, r.y + r.h), (300, 300));
    }

    #[test]
    fn resize_corner_with_aspect_keeps_the_opposite_corner() {
        let orig = rect(100, 100, 200, 200);
        let r = resize_rect(Handle::Se, orig, 500.0, 260.0, 1000, 800, Some((1, 1)));
        assert_eq!(r, rect(100, 100, 400, 400));
        // 顶到画面边：步长被 max_t 夹住，不许把框推出画外。
        let r = resize_rect(Handle::Se, orig, 9999.0, 9999.0, 1000, 800, Some((1, 1)));
        assert_eq!(r, rect(100, 100, 700, 700));
    }

    #[test]
    fn resize_side_handle_grows_around_the_center() {
        let orig = rect(100, 100, 400, 300);
        let r = resize_rect(Handle::N, orig, 300.0, 40.0, 1000, 800, Some((16, 9)));
        assert_eq!(r, rect(12, 76, 576, 324));
        // 左上角取偶会把中心挪半像素——这是 Python 侧同样的规则。
        let r = resize_rect(Handle::E, orig, 900.0, 250.0, 1000, 800, Some((16, 9)));
        assert_eq!(r, rect(100, 24, 800, 450));
    }

    #[test]
    fn nudge_moves_by_whole_steps_and_stops_at_the_edge() {
        assert_eq!(
            nudge_rect(rect(100, 100, 200, 150), 1, -1, 2, 1000, 800),
            rect(102, 98, 200, 150)
        );
        assert_eq!(
            nudge_rect(rect(0, 0, 200, 150), -1, -1, 10, 1000, 800),
            rect(0, 0, 200, 150)
        );
        assert_eq!(
            nudge_rect(rect(800, 650, 200, 150), 1, 1, 10, 1000, 800),
            rect(800, 650, 200, 150)
        );
    }

    #[test]
    fn hit_test_prefers_corners_then_edges() {
        let e = Edges::new(100.0, 100.0, 300.0, 200.0);
        assert_eq!(hit_test(e, 103.0, 97.0, 8.0, 5.0), Hit::Corner(Handle::Nw));
        assert_eq!(hit_test(e, 150.0, 103.0, 8.0, 5.0), Hit::Edge(Handle::N));
        assert_eq!(hit_test(e, 298.0, 150.0, 8.0, 5.0), Hit::Edge(Handle::E));
        assert_eq!(hit_test(e, 200.0, 150.0, 8.0, 5.0), Hit::Inside);
        assert_eq!(hit_test(e, 20.0, 150.0, 8.0, 5.0), Hit::Outside);
    }

    #[test]
    fn intersect_never_leaves_the_frame() {
        assert_eq!(
            intersect(rect(0, 0, 100, 100), rect(50, 50, 100, 100)),
            Some(rect(50, 50, 50, 50))
        );
        assert_eq!(intersect(rect(0, 0, 10, 10), rect(20, 20, 10, 10)), None);
    }

    /// 反旋回去四个角还在画面里——安全框的唯一硬指标。
    /// 用连续口径（框占 `x..x+w`，画面占 `0..fw`），跟 `safe_rect` 的推导一致。
    fn corners_fit(sel: CropRect, angle_deg: f64, fw: u32, fh: u32) -> bool {
        let t = angle_deg.to_radians();
        let (cx, cy) = (fw as f64 / 2.0, fh as f64 / 2.0);
        for (dx, dy) in [
            (sel.x as f64 - cx, sel.y as f64 - cy),
            (sel.x as f64 + sel.w as f64 - cx, sel.y as f64 - cy),
            (sel.x as f64 - cx, sel.y as f64 + sel.h as f64 - cy),
            (
                sel.x as f64 + sel.w as f64 - cx,
                sel.y as f64 + sel.h as f64 - cy,
            ),
        ] {
            let x = t.cos() * dx - t.sin() * dy + cx;
            let y = t.sin() * dx + t.cos() * dy + cy;
            if x < -1e-6 || y < -1e-6 || x > fw as f64 + 1e-6 || y > fh as f64 + 1e-6 {
                return false;
            }
        }
        true
    }

    #[test]
    fn safe_rect_is_the_whole_frame_at_zero() {
        assert_eq!(safe_rect(1280, 720, 0.0, None), rect(0, 0, 1280, 720));
        assert_eq!(
            safe_rect(1280, 720, 0.0, Some((16, 9))),
            rect(0, 0, 1280, 720)
        );
        assert_eq!(safe_rect(1280, 720, -0.0, None), rect(0, 0, 1280, 720));
    }

    #[test]
    fn safe_rect_matches_the_closed_form() {
        // 1280×720 转 5°、锁 16:9：pad = 2(c+s) = 2.167，
        // k = min(1277.83/(16c+9s), 717.83/(16s+9c)) = 69.287…，
        // 代回来 1108.6 × 623.6，往下取偶 1108 × 622，居中 86, 48。
        assert_eq!(
            safe_rect(1280, 720, 5.0, Some((16, 9))),
            rect(86, 48, 1108, 622)
        );
        assert_eq!(
            safe_rect(1280, 720, -5.0, Some((16, 9))),
            rect(86, 48, 1108, 622)
        );
        // 正方形转 45°：内接方框 = 边长 ÷ 2 再取偶。
        assert_eq!(safe_rect(1000, 1000, 45.0, None), rect(148, 148, 704, 704));
    }

    #[test]
    fn safe_rect_shrinks_and_keeps_the_corners_covered() {
        let mut last = u64::MAX;
        for deg in 0..=40 {
            for aspect in [
                None,
                Some((16, 9)),
                Some((4, 3)),
                Some((1, 1)),
                Some((9, 16)),
            ] {
                let sel = safe_rect(4000, 3000, deg as f64, aspect);
                assert!(
                    sel.w.is_multiple_of(2) && sel.h.is_multiple_of(2),
                    "边要成对：{sel:?}"
                );
                assert!(
                    sel.x + sel.w <= 4000 && sel.y + sel.h <= 3000,
                    "不能出画面：{sel:?}"
                );
                assert!(
                    corners_fit(sel, deg as f64, 4000, 3000),
                    "{deg}° {aspect:?} 的角转回去出了画面：{sel:?}"
                );
                if aspect.is_none() {
                    let area = u64::from(sel.w) * u64::from(sel.h);
                    assert!(
                        area <= last,
                        "角度越大可留的画面只会更少：{deg}° {area} > {last}"
                    );
                    last = area;
                }
            }
        }
    }

    #[test]
    fn safe_rect_survives_a_frame_smaller_than_the_rotation() {
        // 转过了头，安全框被夹到最小的一对像素——不能 panic，也不能返回 0 尺寸。
        let sel = safe_rect(8, 8, 45.0, None);
        assert!(sel.w >= 2 && sel.h >= 2, "{sel:?}");
        assert!(sel.w.is_multiple_of(2) && sel.h.is_multiple_of(2));
    }

    // ------------------------------------------------------------------ 运镜

    fn vf(x: f64, y: f64, w: f64, h: f64) -> Viewfinder {
        Viewfinder { x, y, w, h }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    fn vf_eq(a: Viewfinder, b: Viewfinder) -> bool {
        near(a.x, b.x) && near(a.y, b.y) && near(a.w, b.w) && near(a.h, b.h)
    }

    fn win_eq(a: [f64; 4], b: [f64; 4]) -> bool {
        a.iter().zip(b).all(|(x, y)| near(*x, y))
    }

    #[test]
    fn ease_curves_match_the_python_formulas() {
        // smooth = 3t²−2t³、in = t³、out = 1−(1−t)³、linear = t。
        assert!(near(Ease::Smooth.curve(0.25), 0.15625));
        assert!(near(Ease::Smooth.curve(0.5), 0.5));
        assert!(near(Ease::In.curve(0.5), 0.125));
        assert!(near(Ease::Out.curve(0.5), 0.875));
        assert!(near(Ease::Linear.curve(0.375), 0.375));
        for ease in Ease::ALL {
            assert!(near(ease.curve(-1.0), 0.0), "{ease:?} 起点没夹住");
            assert!(near(ease.curve(2.0), 1.0), "{ease:?} 终点没夹住");
            let mut last = -1.0;
            for i in 0..=200 {
                let t = i as f64 / 200.0;
                let v = ease.curve(t);
                assert!(v >= last, "{ease:?} 在 {t} 处倒退：{v} < {last}");
                last = v;
            }
            assert!(near(last, 1.0));
        }
        // 键名与工程文件、命令面共用同一套字面量。
        assert_eq!(Ease::Smooth.key(), "smooth");
        assert_eq!(serde_json::to_string(&Ease::In).unwrap(), "\"in\"");
        for ease in Ease::ALL {
            assert_eq!(Ease::parse(ease.key()), Some(ease));
        }
        assert_eq!(Ease::parse("bounce"), None);
    }

    #[test]
    fn view_of_walks_the_box_from_start_to_end() {
        let m = Motion {
            enabled: true,
            from: FULL,
            to: vf(0.25, 0.1, 0.5, 0.5),
            ease: Ease::Linear,
        };
        // 只有一帧：停在起点，别跑一条算错的坡。
        assert_eq!(m.view_of(0, 1), FULL);
        assert_eq!(m.view_of(0, 5), FULL);
        assert!(vf_eq(m.view_of(4, 5), vf(0.25, 0.1, 0.5, 0.5)));
        // 正中间：匀速就是四元组的算术平均。
        assert!(vf_eq(m.view_of(2, 5), vf(0.125, 0.05, 0.75, 0.75)));
        // 缓入缓出在 t=0.5 也落在同一格，两头才分快慢。
        let smooth = Motion {
            ease: Ease::Smooth,
            ..m
        };
        assert!(vf_eq(smooth.view_of(2, 5), vf(0.125, 0.05, 0.75, 0.75)));
        // 第 1 帧（共 5 帧）时间走到 25%：匀速就推进 25% 段，缓动只走 15.625% 段。
        assert!(vf_eq(m.view_of(1, 5), vf(0.0625, 0.025, 0.875, 0.875)));
        assert!(vf_eq(
            smooth.view_of(1, 5),
            vf(0.0390625, 0.015625, 0.921875, 0.921875)
        ));
    }

    #[test]
    fn normalized_viewfinder_stays_inside_the_frame() {
        assert!(vf_eq(
            vf(-0.5, 0.2, 1.6, 0.5).normalized(),
            vf(0.0, 0.2, 1.0, 0.5)
        ));
        // 左上角被宽高夹过之后剩下的活动区间也跟着收紧。
        assert!(vf_eq(
            vf(0.9, 0.9, 0.5, 0.5).normalized(),
            vf(0.5, 0.5, 0.5, 0.5)
        ));
        // 零宽高、负宽高、NaN：宁可退回整幅，也不炸出一个空框。
        assert_eq!(vf(0.2, 0.2, 0.0, 0.5).normalized(), FULL);
        assert_eq!(vf(0.2, 0.2, -1.0, 0.5).normalized(), FULL);
        assert_eq!(vf(f64::NAN, 0.2, f64::NAN, 0.5).normalized(), FULL);
        assert!(near(vf(0.1, f64::NAN, 0.5, 0.5).normalized().y, 0.0));
        // 小过一档的框兜到 MIN_SPAN，免得读数抖到插值噪声里。
        let tiny = vf(0.5, 0.5, 0.001, 0.001).normalized();
        assert!(vf_eq(tiny, vf(0.5, 0.5, MIN_SPAN, MIN_SPAN)));
        assert!(near(vf(0.99, 0.99, 0.02, 0.02).normalized().x, 0.98));
    }

    #[test]
    fn refit_carries_the_end_box_with_the_start_box() {
        let m = Motion {
            enabled: true,
            from: FULL,
            to: vf(0.25, 0.25, 0.5, 0.5),
            ease: Ease::In,
        };
        // 铺成 4:3 的框：起点重铺后，终点按「相对起点的偏移与倍率」一起搬过去。
        let r = m.refit(0.8, 0.6);
        assert!(vf_eq(r.from, vf(0.1, 0.2, 0.8, 0.6)), "{:?}", r.from);
        assert!(vf_eq(r.to, vf(0.3, 0.35, 0.4, 0.3)), "{:?}", r.to);
        // 幅度没被夹成同一个框——各框各自重铺就会栽在这儿。
        assert!(r.to != r.from);
        assert!(near(r.to.w / r.from.w, 0.5) && near(r.to.h / r.from.h, 0.5));
        // 终点中心仍然落在画面正中（起点也是），缓动档位不许被重铺吃掉。
        assert!(near(r.to.center().0, 0.5) && near(r.to.center().1, 0.5));
        assert_eq!(r.ease, Ease::In);
        assert!(r.enabled);
        // 铺满整幅 = 什么都不动。
        let same = m.refit(1.0, 1.0);
        assert!(vf_eq(same.from, FULL) && vf_eq(same.to, m.to));
    }

    #[test]
    fn view_window_is_the_pixel_box_trimmed_to_the_output_aspect() {
        // 160×90 的源铺 320×180 的成片：比例本来就对得上，一个数都不动。
        assert!(win_eq(
            FULL.view_window(160, 90, 320, 180),
            [0.0, 0.0, 160.0, 90.0]
        ));
        assert!(win_eq(
            vf(0.5, 0.5, 0.5, 0.5).view_window(160, 90, 320, 180),
            [80.0, 45.0, 80.0, 45.0]
        ));
        // 方源铺 16:9：方框在像素口径上也是方的，得从上下各收掉 (50 − 28.125)/2。
        let sq = vf(0.25, 0.25, 0.5, 0.5).view_window(100, 100, 160, 90);
        assert!(near(sq[0], 25.0) && near(sq[2], 50.0), "{sq:?}");
        assert!(near(sq[3], 50.0 * 90.0 / 160.0), "{sq:?}");
        assert!(near(sq[1], 25.0 + (50.0 - sq[3]) / 2.0), "{sq:?}");
        // 扁框铺 16:9：改成收左右，中心与上下都不动。
        let wide = vf(0.0, 0.0, 1.0, 0.5).view_window(200, 100, 160, 90);
        assert!(
            near(wide[0] + wide[2] / 2.0, 100.0),
            "中心被挪走了：{wide:?}"
        );
        assert!(
            near(wide[2] / wide[3], 160.0 / 90.0),
            "比例没对上：{wide:?}"
        );
        assert!(near(wide[1], 0.0) && near(wide[3], 50.0), "{wide:?}");
        // 输出尺寸非法（还没探测到画幅）：原样给回像素窗口，不裁也不炸。
        assert_eq!(
            vf(0.25, 0.25, 0.5, 0.5).view_window(100, 100, 0, 0),
            [25.0, 25.0, 50.0, 50.0]
        );
        // 越界与 NaN 先过 normalized()，出来的窗口照旧要收成输出比例。
        assert!(win_eq(
            vf(-1.0, -1.0, 4.0, 4.0).view_window(640, 480, 1280, 720),
            [0.0, 60.0, 640.0, 360.0]
        ));
        assert!(win_eq(
            vf(f64::NAN, 0.0, f64::NAN, 1.0).view_window(640, 480, 640, 480),
            [0.0, 0.0, 640.0, 480.0]
        ));
        // 收完之后宽高都还在 (0, 画幅] 里：采样窗口不会退化成零。
        for (frame, out) in [
            ((160u32, 90u32), (320u32, 180u32)),
            ((100, 100), (160, 90)),
            ((4000, 3000), (1920, 1080)),
            ((90, 160), (3840, 2160)),
        ] {
            for seed in [
                FULL,
                vf(0.5, 0.5, 0.5, 0.5),
                vf(0.0, 0.0, 0.2, 0.9),
                vf(0.45, 0.45, 0.02, 0.02),
            ] {
                let w = seed.view_window(frame.0, frame.1, out.0, out.1);
                assert!(w[2] > 0.0 && w[3] > 0.0, "{seed:?} 收空了：{w:?}");
                assert!(
                    w[0] >= -1e-9
                        && w[1] >= -1e-9
                        && w[0] + w[2] <= frame.0 as f64 + 1e-9
                        && w[1] + w[3] <= frame.1 as f64 + 1e-9,
                    "{seed:?} 出了画面：{w:?}"
                );
                assert!(
                    near(w[2] / w[3], out.0 as f64 / out.1 as f64),
                    "{seed:?} 的比例没对上：{w:?}"
                );
            }
        }
    }

    #[test]
    fn viewfinder_pixels_are_the_source_window() {
        let v = vf(0.25, 0.25, 0.5, 0.5);
        assert_eq!(v.pixels(1200, 800), [300.0, 200.0, 600.0, 400.0]);
        // 越界与退化先过 normalized()，换算出来的窗口永远在画内。
        let p = vf(-1.0, -1.0, 4.0, 4.0).pixels(640, 480);
        assert_eq!(p, [0.0, 0.0, 640.0, 480.0]);
    }

    #[test]
    fn zoom_reads_how_hard_the_box_is_being_blown_up() {
        let frame = (2000, 1000);
        // 基准是「整幅铺满输出」：整幅 = 1×，框缩到 1/4 边长刚好填满 = 4×（推）。
        // 输出像素多少与它无关——720p 预览导 4K 的那 3× 是缩放，不是运镜。
        assert!(near(FULL.zoom(1000, 500, frame.0, frame.1), 1.0));
        assert!(near(
            vf(0.25, 0.25, 0.25, 0.25).zoom(1000, 500, frame.0, frame.1),
            4.0
        ));
        // 框与输出比例对不上时按「盖住」那一档收窗，两个框都先收成输出比例再比。
        let z = vf(0.5, 0.5, 0.5, 0.5).zoom(1920, 1080, frame.0, frame.1);
        assert!(near(z, 2.0), "{z}");
        // 源幅宽为 0（序列还没探测到尺寸）：不给除零，报 1× 让界面先别读数。
        assert!(near(vf(0.25, 0.25, 0.5, 0.5).zoom(1000, 500, 0, 0), 1.0));
    }

    #[test]
    fn motion_needs_both_a_switch_and_a_real_move() {
        let to = vf(0.25, 0.25, 0.5, 0.5);
        assert!(
            !Motion {
                enabled: false,
                from: FULL,
                to,
                ease: Ease::Smooth,
            }
            .is_active()
        );
        assert!(
            !Motion {
                enabled: true,
                from: FULL,
                to: FULL,
                ease: Ease::Smooth,
            }
            .is_active()
        );
        // 起点拖出画外、夹回来跟终点重合：也不算在动，别白白重采样一次。
        assert!(
            !Motion {
                enabled: true,
                from: vf(-0.2, -0.2, 1.4, 1.4),
                to: FULL,
                ease: Ease::Smooth,
            }
            .is_active()
        );
        assert!(
            Motion {
                enabled: true,
                from: FULL,
                to,
                ease: Ease::Smooth,
            }
            .is_active()
        );
        // 默认关着，但一打开就有可见的幅度（终点往里收一档）。
        let d = Motion::default();
        assert!(!d.is_active());
        assert!(d.to.w < 1.0 && d.to.h < 1.0);
        assert_eq!(d.ease, Ease::Smooth);
    }

    #[test]
    fn motion_round_trips_through_json() {
        let m = Motion {
            enabled: true,
            from: FULL,
            to: vf(0.25, 0.25, 0.5, 0.5),
            ease: Ease::Out,
        };
        let raw = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Motion>(&raw).unwrap(), m);
        assert!(raw.contains("\"out\""), "{raw}");
    }

    #[test]
    fn patched_only_moves_the_keys_it_was_given() {
        let base = Motion {
            enabled: false,
            from: FULL,
            to: vf(0.25, 0.25, 0.5, 0.5),
            ease: Ease::Smooth,
        };
        // 只写 enabled：两个框和曲线都照旧。
        let one = base
            .patched(&serde_json::json!({ "enabled": true }))
            .unwrap();
        assert!(one.enabled);
        assert_eq!((one.from, one.to, one.ease), (FULL, base.to, Ease::Smooth));
        // 只写终点：起点与曲线照旧，且「给了框」就等于要运镜，不用先推开关。
        let two = base
            .patched(&serde_json::json!({ "to": [0.1, 0.1, 0.6, 0.6] }))
            .unwrap();
        assert!(two.enabled);
        assert_eq!(two.from, FULL);
        assert!(near(two.to.x, 0.1) && near(two.to.w, 0.6));
        // 对象写法与数组写法得到同一个框。
        let three = base
            .patched(&serde_json::json!({ "from": { "x": 0.2, "y": 0.0, "w": 0.8, "h": 1.0 } }))
            .unwrap();
        assert!(near(three.from.x, 0.2) && near(three.from.w, 0.8));
        // 空值等于没写：`{"from": null}` 不该把起点抹成 0 宽。
        let four = base.patched(&serde_json::json!({ "from": null })).unwrap();
        assert_eq!(four, base);
    }

    #[test]
    fn patched_ease_swap_and_refit() {
        let base = Motion {
            enabled: true,
            from: FULL,
            to: vf(0.25, 0.25, 0.5, 0.5),
            ease: Ease::Smooth,
        };
        let e = base
            .patched(&serde_json::json!({ "ease": "linear" }))
            .unwrap();
        assert_eq!(e.ease, Ease::Linear);
        assert!(
            base.patched(&serde_json::json!({ "ease": "bouncy" }))
                .unwrap_err()
                .contains("smooth / linear / in / out")
        );
        let s = base.patched(&serde_json::json!({ "swap": true })).unwrap();
        assert_eq!(s.from, base.to);
        assert_eq!(s.to, base.from);
        // 换画幅：终点跟着起点一起缩，运镜幅度不会被「各自铺满」抹平。
        let r = base
            .patched(&serde_json::json!({ "refit": [1.0, 0.5625] }))
            .unwrap();
        assert!(r.from.h < 1.0 && r.to.h < 1.0);
        assert!(r.from != r.to);
        assert!(
            base.patched(&serde_json::json!({ "refit": [0.0, 1.0] }))
                .unwrap_err()
                .contains("正数")
        );
    }

    #[test]
    fn patched_reset_and_bad_input_report_the_rule() {
        let base = Motion {
            enabled: true,
            from: FULL,
            to: vf(0.25, 0.25, 0.5, 0.5),
            ease: Ease::In,
        };
        assert_eq!(
            base.patched(&serde_json::json!({ "reset": true })).unwrap(),
            Motion::default()
        );
        // reset 只负责「一键清空」：写 false 就是别清，其余键照常生效。
        assert_eq!(
            base.patched(&serde_json::json!({ "reset": false }))
                .unwrap(),
            base
        );
        assert_eq!(
            base.patched(&serde_json::json!({ "reset": false, "ease": "out" }))
                .unwrap()
                .ease,
            Ease::Out
        );
        assert_eq!(
            base.patched(&serde_json::json!({ "reset": null })).unwrap(),
            base
        );
        assert!(
            base.patched(&serde_json::json!({ "enabled": "yes" }))
                .unwrap_err()
                .contains("布尔")
        );
        // 框少给一个数：报错点名是哪一项、该怎么写。
        let err = base
            .patched(&serde_json::json!({ "to": [0.1, 0.2, 0.3] }))
            .unwrap_err();
        assert!(err.contains("to 要写成四个数"), "{err}");
        assert!(
            base.patched(&serde_json::json!({ "from": "0,0,1,1" }))
                .unwrap_err()
                .contains("from 要写成")
        );
        assert!(
            base.patched(&serde_json::json!({ "to": [0.1, 0.2, "wide", 0.4] }))
                .unwrap_err()
                .contains("宽不是有限数值")
        );
    }

    #[test]
    fn pair_arg_takes_both_spellings() {
        let p = |raw: serde_json::Value| pair_arg(&raw, "refit");
        assert_eq!(p(serde_json::json!([1.0, 0.5625])).unwrap(), (1.0, 0.5625));
        assert_eq!(
            p(serde_json::json!({ "w": 1.0, "h": 0.5 })).unwrap(),
            (1.0, 0.5)
        );
        assert_eq!(
            p(serde_json::json!({ "width": 1.0, "height": 0.5 })).unwrap(),
            (1.0, 0.5)
        );
        assert!(p(serde_json::json!([1.0])).unwrap_err().contains("refit"));
        assert!(p(serde_json::json!("16:9")).is_err());
    }
}
