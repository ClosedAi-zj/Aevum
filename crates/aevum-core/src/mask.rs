//! 局部蒙版：几何权重场与局部参数合成。
//!
//! Local masks: geometric weight fields combined with per-point parameters.
//!
//! 口径与 Python `acceptance/reference/grade.py` 的 `_comp_field` / `group_field` /
//! `_mask_field` 一致：几何用归一化坐标存盘，算场时换成像素坐标，
//! 所以同一组参数在任意预览分辨率下圈住的是画面上的同一块地方。
//!
//! The convention matches Python `acceptance/reference/grade.py` `_comp_field` /
//! `group_field` / `_mask_field`: geometry is persisted in normalized coordinates and
//! converted to pixel coordinates when the field is computed, so one set of parameters
//! circles the same region of the picture at any preview resolution.
//!
//! 参数是「相对全局的增量」：渲染时拿原图叠上增量重跑主链，再按权重混回，
//! 局部与全局共用同一套算法，不会有第二套语义。
//!
//! Parameters are deltas relative to the global grade: at render time the original image is
//! run through the main chain again with the delta applied, then blended back by weight;
//! local and global share one algorithm, with no second set of semantics.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::grade::GradeParams;

/// 蒙版能携带的逐点参数；与 Python `MASK_KEYS` 一一对应。
///
/// Per-point parameters a mask can carry; one-to-one with Python `MASK_KEYS`.
pub const MASK_KEYS: &[&str] = &[
    "exposure",
    "temperature",
    "tint",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
    "dehaze",
    "vibrance",
    "saturation",
    "clarity",
    "texture",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Linear,
    Radial,
    Brush,
}

impl ShapeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Radial => "radial",
            Self::Brush => "brush",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    Add,
    Subtract,
    Intersect,
}

impl Blend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Intersect => "intersect",
        }
    }
}

#[derive(Clone, Debug)]
pub struct LumRange {
    pub on: bool,
    pub min: f64,
    pub max: f64,
    pub feather: f64,
}

impl LumRange {
    fn weight(&self, lum: Option<&[f32]>, index: usize) -> f32 {
        let Some(src) = lum else {
            return 1.0;
        };
        if !self.on {
            return 1.0;
        }
        let Some(&value) = src.get(index) else {
            return 1.0;
        };
        let mut lo = self.min.clamp(0.0, 1.0);
        let mut hi = self.max.clamp(0.0, 1.0);
        if hi < lo {
            std::mem::swap(&mut lo, &mut hi);
        }
        let fe = self.feather.max(1e-3);
        let wl = smooth(lo - fe, lo, value as f64);
        let wh = 1.0 - smooth(hi, hi + fe, value as f64);
        wl.min(wh) as f32
    }
}

/// 颜色范围：按色相环选一段（`hue` ± `width/2` 度），带羽化与饱和度下限。
/// 饱和度低于 `sat_min` 的近灰像素自然落选，免得天空外的灰色一律被圈进来。
///
/// A color range: picks a span of the hue wheel (`hue` ± `width/2` degrees) with feathering
/// and a saturation floor. Near-gray pixels below `sat_min` naturally fall outside, keeping
/// grays beyond the sky from being swept in.
#[derive(Clone, Debug)]
pub struct HueRange {
    pub on: bool,
    /// 目标色相 0~360。
    ///
    /// Target hue, 0~360.
    pub hue: f64,
    /// 收进来的色相带宽 0~360（180 已是半环）。
    ///
    /// Hue bandwidth pulled in, 0~360 (180 is already a half wheel).
    pub width: f64,
    /// 带外再往两边淡出的角度 0~180。
    ///
    /// Angle faded out on both sides beyond the band, 0~180.
    pub feather: f64,
    /// 饱和度下限 0~1，边缘 0.05 软过渡。
    ///
    /// Saturation floor 0~1, with a 0.05 soft edge.
    pub sat_min: f64,
}

impl HueRange {
    fn weight(&self, rgb: Option<&[f32]>, index: usize) -> f32 {
        let Some(src) = rgb else {
            return 1.0;
        };
        if !self.on {
            return 1.0;
        }
        let Some(px) = src.get(index * 3..index * 3 + 3) else {
            return 1.0;
        };
        let (r, g, b) = (px[0] as f64, px[1] as f64, px[2] as f64);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let sat = if max > 1e-9 { (max - min) / max } else { 0.0 };
        let sat_w = smooth(self.sat_min - 0.05, self.sat_min + 0.05, sat);
        if sat_w <= 0.0 {
            return 0.0;
        }
        // 色相环上的最短距离；饱和度趋零时色相没意义，只信饱和度权重。
        let chroma = max - min;
        if chroma <= 1e-9 {
            return 0.0;
        }
        let (sec, frac) = if max == r {
            ((g - b) / chroma, 0.0)
        } else if max == g {
            ((b - r) / chroma, 2.0)
        } else {
            ((r - g) / chroma, 4.0)
        };
        let hue = ((sec + frac) / 6.0).rem_euclid(1.0) * 360.0;
        let d = (hue - self.hue.clamp(0.0, 360.0) + 180.0).rem_euclid(360.0) - 180.0;
        let d = d.abs();
        let hw = (self.width.clamp(0.0, 360.0) / 2.0).min(180.0);
        let fe = self.feather.clamp(0.0, 180.0);
        let hue_w = if hw + fe >= 180.0 {
            1.0
        } else {
            smooth(hw + fe, hw - fe, d)
        };
        (hue_w * sat_w) as f32
    }
}

/// 一条画笔笔画的硬上限：采样点数。再多的点直接截断，防炸工程文件。
///
/// Hard cap per brush stroke: sampled point count. Extra points are truncated outright to
/// keep project files from blowing up.
pub const MAX_STROKE_POINTS: usize = 4096;
/// 一张画笔组件的笔画数上限。
///
/// Stroke count cap for one brush component.
pub const MAX_STROKES: usize = 256;

/// 一条笔画：归一化点列（行优先坐标，与图像尺寸无关）+ 圆章参数。
/// `radius` 按图像短边的百分比存（0.002~0.5），`hardness` / `flow` 在 0~1。
///
/// One stroke: a normalized point list (row-major coordinates, independent of image size)
/// plus round-stamp parameters. `radius` is stored as a percentage of the image short side
/// (0.002~0.5); `hardness` / `flow` are 0~1.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub points: Vec<[f64; 2]>,
    pub radius: f64,
    pub hardness: f64,
    pub flow: f64,
    pub erase: bool,
}

#[derive(Clone, Debug)]
pub struct Component {
    pub shape: ShapeKind,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub feather: f64,
    pub midpoint: f64,
    pub angle: f64,
    pub invert: bool,
    pub blend: Blend,
    pub range: Option<LumRange>,
    pub hue: Option<HueRange>,
    /// 只在 `shape == Brush` 时有内容；其余形状恒为空。
    ///
    /// Only populated when `shape == Brush`; always empty for the other shapes.
    pub strokes: Vec<Stroke>,
}

/// 一张局部蒙版：扁平老格式视为单组件组。
///
/// One local mask: the flat legacy format counts as a single-component group.
#[derive(Clone, Debug)]
pub struct Mask {
    pub id: String,
    pub components: Vec<Component>,
    pub amount: f64,
    pub invert: bool,
    pub params: BTreeMap<String, f64>,
    pub range: Option<LumRange>,
    pub hue: Option<HueRange>,
    /// 0~1 权重场，`amount` 已折算进去。
    ///
    /// The 0~1 weight field, with `amount` already folded in.
    pub field: Vec<f32>,
}

impl Mask {
    /// 逐点参数里是否有非零增量（空间项在这里不算，主链重跑用不上）。
    ///
    /// Whether any per-point parameter has a nonzero delta (spatial terms do not count here;
    /// a main chain rerun has no use for them).
    pub fn has_point_delta(&self) -> bool {
        self.params
            .iter()
            .any(|(k, v)| MASK_KEYS.contains(&k.as_str()) && v.abs() > 1e-6)
    }

    /// 任意参数非零：决定这张蒙版是否「活着」（细节项也要算）。
    ///
    /// Whether any parameter is nonzero: decides whether this mask is "alive" (detail terms
    /// included).
    pub fn has_any_delta(&self) -> bool {
        self.params.values().any(|v| v.abs() > 1e-6)
    }
}

/// 解析一张蒙版 JSON；字段缺省按 Python `MASK_NEW` 的默认几何补齐。
///
/// Parses one mask JSON; missing fields are filled with the default geometry of Python
/// `MASK_NEW`.
pub fn parse_mask(v: &Value) -> Option<Mask> {
    let obj = v.as_object()?;
    let grouped = obj.contains_key("components");
    let (components, amount, invert, range, hue) = if grouped {
        let comps: Vec<Component> = obj
            .get("components")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(parse_component).collect())
            .unwrap_or_default();
        let (range, hue) = parse_range(obj.get("range"));
        (
            comps,
            num(obj, "amount", 100.0),
            truthy(obj.get("invert")),
            range,
            hue,
        )
    } else {
        let comp = parse_component(v)?;
        (vec![comp], num(obj, "amount", 100.0), false, None, None)
    };
    Some(Mask {
        id: obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        components,
        amount,
        invert,
        params: parse_params(obj.get("params")),
        range,
        hue,
        field: Vec::new(),
    })
}

fn parse_component(v: &Value) -> Option<Component> {
    let obj = v.as_object()?;
    let shape = match obj
        .get("kind")
        .or_else(|| obj.get("type"))
        .and_then(Value::as_str)
    {
        Some("radial") => ShapeKind::Radial,
        Some("brush") => ShapeKind::Brush,
        _ => ShapeKind::Linear,
    };
    let (dx1, dy1, dx2, dy2, dfeather) = match shape {
        ShapeKind::Linear => (0.5, 0.30, 0.5, 0.62, 45.0),
        ShapeKind::Radial => (0.5, 0.50, 0.72, 0.50, 35.0),
        ShapeKind::Brush => (0.5, 0.50, 0.5, 0.50, 0.0),
    };
    let blend = match obj.get("blend").and_then(Value::as_str) {
        Some("subtract") => Blend::Subtract,
        Some("intersect") => Blend::Intersect,
        _ => Blend::Add,
    };
    let strokes = if shape == ShapeKind::Brush {
        obj.get("strokes")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(parse_stroke)
                    .take(MAX_STROKES)
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let (range, hue) = parse_range(obj.get("range"));
    Some(Component {
        shape,
        x1: num(obj, "x1", dx1),
        y1: num(obj, "y1", dy1),
        x2: num(obj, "x2", dx2),
        y2: num(obj, "y2", dy2),
        feather: num(obj, "feather", dfeather),
        midpoint: num(obj, "midpoint", 50.0),
        angle: num(obj, "angle", 0.0),
        invert: truthy(obj.get("invert")),
        blend,
        range,
        hue,
        strokes,
    })
}

fn parse_stroke(v: &Value) -> Option<Stroke> {
    let obj = v.as_object()?;
    let points: Vec<[f64; 2]> = obj
        .get("points")?
        .as_array()?
        .iter()
        .filter_map(|p| {
            let a = p.as_array()?;
            Some([
                a.first()?.as_f64()?.clamp(0.0, 1.0),
                a.get(1)?.as_f64()?.clamp(0.0, 1.0),
            ])
        })
        .take(MAX_STROKE_POINTS)
        .collect();
    if points.is_empty() {
        return None;
    }
    Some(Stroke {
        points,
        radius: num(obj, "radius", 0.06).clamp(0.002, 0.5),
        hardness: num(obj, "hardness", 0.7).clamp(0.0, 1.0),
        flow: num(obj, "flow", 1.0).clamp(0.01, 1.0),
        erase: truthy(obj.get("erase")),
    })
}

fn parse_params(v: Option<&Value>) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    if let Some(map) = v.and_then(Value::as_object) {
        for (k, v) in map {
            if let Some(x) = v.as_f64() {
                out.insert(k.clone(), x);
            }
        }
    }
    out
}

fn parse_range(v: Option<&Value>) -> (Option<LumRange>, Option<HueRange>) {
    let Some(obj) = v.and_then(Value::as_object) else {
        return (None, None);
    };
    let lum = Some(LumRange {
        on: truthy(obj.get("lum_on")),
        min: num(obj, "lum_min", 0.0),
        max: num(obj, "lum_max", 1.0),
        feather: num(obj, "lum_feather", 0.1),
    });
    let hue = Some(HueRange {
        on: truthy(obj.get("hue_on")),
        hue: num(obj, "hue", 0.0),
        width: num(obj, "hue_width", 40.0),
        feather: num(obj, "hue_feather", 20.0),
        sat_min: num(obj, "hue_sat", 0.05),
    });
    (lum, hue)
}

fn num(obj: &Map<String, Value>, key: &str, default: f64) -> f64 {
    obj.get(key).and_then(Value::as_f64).unwrap_or(default)
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x.abs() > 1e-12),
        Some(Value::String(s)) => !s.is_empty() && s != "0" && s != "false",
        _ => false,
    }
}

fn smooth(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn pixel_line(c: &Component, w: usize, h: usize) -> (f64, f64, f64, f64) {
    let (mut ax, mut ay) = (c.x1 * (w as f64 - 1.0), c.y1 * (h as f64 - 1.0));
    let (mut bx, mut by) = (c.x2 * (w as f64 - 1.0), c.y2 * (h as f64 - 1.0));
    if c.angle != 0.0 {
        let (sin, cos) = c.angle.to_radians().sin_cos();
        // linear 绕两端中点旋转整条渐变线
        let (mx, my) = ((ax + bx) * 0.5, (ay + by) * 0.5);
        let (dx, dy) = (ax - mx, ay - my);
        ax = mx + dx * cos - dy * sin;
        ay = my + dx * sin + dy * cos;
        let (dx, dy) = (bx - mx, by - my);
        bx = mx + dx * cos - dy * sin;
        by = my + dx * sin + dy * cos;
    }
    (ax, ay, bx, by)
}

fn component_field(c: &Component, w: usize, h: usize) -> Vec<f32> {
    if c.shape == ShapeKind::Brush {
        return brush_field(c, w, h);
    }
    let fe = c.feather.clamp(0.0, 100.0) / 100.0;
    let mp = c.midpoint.clamp(0.0, 100.0) / 100.0;
    let mut out = vec![0.0f32; w * h];

    match c.shape {
        ShapeKind::Radial => {
            let cx = c.x1 * (w as f64 - 1.0);
            let cy = c.y1 * (h as f64 - 1.0);
            let dx = (c.x2 - c.x1) * (w as f64 - 1.0);
            let dy = (c.y2 - c.y1) * (h as f64 - 1.0);
            let mut rx = dx.abs();
            let mut ry = dy.abs();
            if ry < (h as f64 * 0.01).max(1.0) {
                // 只横向拖 = 正圆
                ry = rx.max(1.0);
            } else if rx < (w as f64 * 0.01).max(1.0) {
                rx = ry.max(1.0);
            }
            let (sin, cos) = c.angle.to_radians().sin_cos();
            let shift = (mp - 0.5) * (0.5 + fe);
            for y in 0..h {
                let py = y as f64 - cy;
                for x in 0..w {
                    let px = x as f64 - cx;
                    let (u, v) = if c.angle != 0.0 {
                        (px * cos + py * sin, -px * sin + py * cos)
                    } else {
                        (px, py)
                    };
                    let q = (u / rx).hypot(v / ry) - shift;
                    out[y * w + x] = if fe <= 0.0 {
                        (q <= 1.0) as u8 as f32
                    } else {
                        smooth(1.0 + fe, 1.0 - fe, q) as f32
                    };
                }
            }
        }
        ShapeKind::Linear => {
            let (ax, ay, bx, by) = pixel_line(c, w, h);
            let (dx, dy) = (bx - ax, by - ay);
            let l2 = (dx * dx + dy * dy).max(1e-6);
            for y in 0..h {
                let py = y as f64 - ay;
                for x in 0..w {
                    let px = x as f64 - ax;
                    let t = (px * dx + py * dy) / l2 - (mp - 0.5);
                    out[y * w + x] = smooth(1.0 + fe, -fe, t) as f32;
                }
            }
        }
        // 画笔在函数开头已经提前返回，这里到不了。
        ShapeKind::Brush => {}
    }

    if c.invert {
        for v in &mut out {
            *v = 1.0 - *v;
        }
    }
    out
}

/// 画笔场：每条笔画沿折线按弧长重采（步长 ≈ 圆章半径的 30%，保证不断线），
/// 逐章盖软边圆，章内用 over 合成让同一笔的反复覆盖不再加深；
/// 笔画之间 paint 用 over、erase 用削。
fn brush_field(c: &Component, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    for stroke in &c.strokes {
        let s = stroke_field(stroke, w, h);
        for (acc, v) in out.iter_mut().zip(s) {
            *acc = if stroke.erase {
                *acc * (1.0 - v)
            } else {
                *acc + v * (1.0 - *acc)
            };
        }
    }
    if c.invert {
        for v in &mut out {
            *v = 1.0 - *v;
        }
    }
    out
}

fn stroke_field(stroke: &Stroke, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    let wf = (w as f64 - 1.0).max(0.0);
    let hf = (h as f64 - 1.0).max(0.0);
    let r_px = (stroke.radius * w.min(h) as f64).max(0.5);
    let hard_r = stroke.hardness.clamp(0.0, 1.0) * r_px;
    let soft = ((1.0 - stroke.hardness.clamp(0.0, 1.0)) * r_px).max(1e-6);
    let step = (r_px * 0.3).max(1.0);

    let stamp = |out: &mut [f32], cx: f64, cy: f64| {
        let x0 = ((cx - r_px).floor().max(0.0)) as usize;
        let x1 = (((cx + r_px).ceil()) as usize).min(w.max(1) - 1);
        let y0 = ((cy - r_px).floor().max(0.0)) as usize;
        let y1 = (((cy + r_px).ceil()) as usize).min(h.max(1) - 1);
        for y in y0..=y1 {
            let dy = y as f64 - cy;
            for x in x0..=x1 {
                let dx = x as f64 - cx;
                let d = dx.hypot(dy);
                let t = ((d - hard_r) / soft).clamp(0.0, 1.0);
                let wgt = (1.0 - t * t * (3.0 - 2.0 * t)) as f32;
                if wgt <= 0.0 {
                    continue;
                }
                let a = stroke.flow as f32 * wgt;
                let i = y * w + x;
                out[i] += a * (1.0 - out[i]);
            }
        }
    };

    let mut prev = stroke.points[0];
    stamp(&mut out, prev[0] * wf, prev[1] * hf);
    for p in &stroke.points[1..] {
        let (ax, ay) = (prev[0] * wf, prev[1] * hf);
        let (bx, by) = (p[0] * wf, p[1] * hf);
        let len = (bx - ax).hypot(by - ay);
        let n = ((len / step).ceil() as usize).max(1);
        for i in 1..=n {
            let t = i as f64 / n as f64;
            stamp(&mut out, ax + (bx - ax) * t, ay + (by - ay) * t);
        }
        prev = *p;
    }
    out
}

fn combine(
    field: &mut [f32],
    mut w: Vec<f32>,
    blend: Blend,
    lum: Option<&[f32]>,
    range: Option<&LumRange>,
    rgb: Option<&[f32]>,
    hue: Option<&HueRange>,
) {
    if let Some(range) = range
        && range.on
    {
        for (i, v) in w.iter_mut().enumerate() {
            *v *= range.weight(lum, i);
        }
    }
    if let Some(hue) = hue
        && hue.on
    {
        for (i, v) in w.iter_mut().enumerate() {
            *v *= hue.weight(rgb, i);
        }
    }
    for (acc, v) in field.iter_mut().zip(w) {
        *acc = match blend {
            Blend::Add => (1.0f32).min(*acc + v),
            Blend::Subtract => (0.0f32).max(*acc - v),
            Blend::Intersect => *acc * v,
        };
    }
}

/// 按 `amount` / `invert` / 组级亮度与颜色范围收尾并夹到 `[0,1]`。
fn finish(
    field: &mut [f32],
    amount: f64,
    invert: bool,
    range: Option<&LumRange>,
    lum: Option<&[f32]>,
    hue: Option<&HueRange>,
    rgb: Option<&[f32]>,
) {
    let amt = amount.clamp(0.0, 100.0) / 100.0;
    if amt < 1.0 {
        for v in field.iter_mut() {
            *v *= amt as f32;
        }
    }
    if invert {
        for v in field.iter_mut() {
            *v = 1.0 - *v;
        }
    }
    if let Some(range) = range
        && range.on
    {
        for (i, v) in field.iter_mut().enumerate() {
            *v *= range.weight(lum, i);
        }
    }
    if let Some(hue) = hue
        && hue.on
    {
        for (i, v) in field.iter_mut().enumerate() {
            *v *= hue.weight(rgb, i);
        }
    }
    for v in field.iter_mut() {
        *v = v.clamp(0.0, 1.0);
    }
}

impl Mask {
    /// 算权重场；一个组的组件按 add / subtract / intersect 合成一张图。
    /// `lum` 是原图亮度图（亮度范围用），`rgb` 是原图本身（颜色范围用），不缺就不传。
    ///
    /// Computes the weight field; a group's components are combined into one image via
    /// add / subtract / intersect. `lum` is the original luminance map (for luminance ranges)
    /// and `rgb` is the original image itself (for color ranges); pass neither when
    /// unavailable.
    pub fn build_field(&mut self, w: usize, h: usize, lum: Option<&[f32]>, rgb: Option<&[f32]>) {
        let mut field = vec![0.0f32; w * h];
        let mut first = true;
        for comp in &self.components {
            let w_field = component_field(comp, w, h);
            let blend = if first { Blend::Add } else { comp.blend };
            combine(
                &mut field,
                w_field,
                blend,
                lum,
                comp.range.as_ref(),
                rgb,
                comp.hue.as_ref(),
            );
            first = false;
        }
        finish(
            &mut field,
            self.amount,
            self.invert,
            self.range.as_ref(),
            lum,
            self.hue.as_ref(),
            rgb,
        );
        self.field = field;
    }
}

/// 会被渲染的蒙版：强度为 0、参数全 0、或组件为空的只占位。
///
/// The masks that will render: zero strength, all-zero parameters, or empty components only
/// take up space.
pub fn active_masks(p: &GradeParams) -> Vec<Mask> {
    let mut out = Vec::new();
    for raw in &p.masks {
        let Some(mask) = parse_mask(raw) else {
            continue;
        };
        if mask.amount <= 1e-6 || mask.components.is_empty() || !mask.has_any_delta() {
            continue;
        }
        out.push(mask);
    }
    out
}

/// 是否有蒙版启用亮度范围（调用方要先算一张原图亮度图）。
///
/// Whether any mask enables a luminance range (the caller must first compute an
/// original-image luminance map).
pub fn uses_luminance(p: &GradeParams) -> bool {
    for raw in &p.masks {
        let Some(mask) = parse_mask(raw) else {
            continue;
        };
        if mask.range.as_ref().is_some_and(|r| r.on) {
            return true;
        }
        if mask
            .components
            .iter()
            .any(|c| c.range.as_ref().is_some_and(|r| r.on))
        {
            return true;
        }
    }
    false
}

/// 原图亮度图（行优先，`[0,1]` 夹紧），给亮度范围蒙版用。
///
/// The original-image luminance map (row-major, clamped to `[0,1]`), for luminance-range
/// masks.
pub fn luminance(rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(w * h);
    for px in rgb.chunks_exact(3) {
        let lum = px[0] * 0.2126 + px[1] * 0.7152 + px[2] * 0.0722;
        out.push(lum.clamp(0.0, 1.0));
    }
    out
}

/// 把一张蒙版的增量叠到全局参数上，得到「只在这块区域生效」的参数副本。
///
/// Folds a mask's delta into the global parameters, producing a copy that applies only
/// inside this region.
pub fn apply_delta(p: &GradeParams, d: &BTreeMap<String, f64>) -> GradeParams {
    let mut q = p.clone();
    macro_rules! add {
        ($($field:ident),+ $(,)?) => {
            $(
                if let Some(v) = d.get(stringify!($field)) {
                    q.$field += *v;
                }
            )+
        };
    }
    add!(
        exposure,
        temperature,
        tint,
        contrast,
        highlights,
        shadows,
        whites,
        blacks,
        dehaze,
        vibrance,
        saturation,
        clarity,
        texture,
    );
    q
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!(
        "{:012x}",
        now ^ (SEQ
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9))
    )
}

/// 新形状组件的默认几何（归一化坐标，与图像尺寸无关）。
///
/// Default geometry for a new shape component (normalized coordinates, independent of image
/// size).
pub fn new_component(kind: ShapeKind) -> Value {
    let (x1, y1, x2, y2, feather) = match kind {
        ShapeKind::Linear => (0.5, 0.30, 0.5, 0.62, 45.0),
        ShapeKind::Radial => (0.5, 0.50, 0.72, 0.50, 35.0),
        // 画笔没有渐变几何；空笔画场全 0，等第一笔落下才生效。
        ShapeKind::Brush => (0.5, 0.50, 0.5, 0.50, 0.0),
    };
    let mut v = json!({
        "kind": kind.as_str(),
        "id": new_id(),
        "x1": x1, "y1": y1, "x2": x2, "y2": y2,
        "feather": feather, "invert": 0,
        "angle": 0.0, "midpoint": 50.0,
        "blend": "add",
    });
    if kind == ShapeKind::Brush {
        v["strokes"] = json!([]);
    }
    v
}

/// 新建一张单组件蒙版，几何与 Python `new_mask` 一致。
///
/// Creates a single-component mask whose geometry matches Python `new_mask`.
pub fn new_mask(kind: ShapeKind) -> Value {
    json!({
        "id": new_id(),
        "components": [new_component(kind)],
        "params": {},
        "amount": 100.0,
        "invert": 0,
    })
}

/// 扁平老蒙版包成单组件组；几何与强度语义原样保留。
///
/// Wraps a flat legacy mask into a single-component group; geometry and strength semantics
/// are kept as-is.
pub fn as_group(v: &Value) -> Value {
    let Some(obj) = v.as_object() else {
        return v.clone();
    };
    if obj.contains_key("components") {
        return v.clone();
    }
    let mut comp = Map::new();
    for (k, val) in obj {
        if !matches!(k.as_str(), "params" | "amount" | "type" | "range") {
            comp.insert(k.clone(), val.clone());
        }
    }
    comp.insert(
        "kind".to_owned(),
        Value::String(
            obj.get("type")
                .and_then(Value::as_str)
                .unwrap_or("linear")
                .to_owned(),
        ),
    );
    comp.entry("id".to_owned())
        .or_insert_with(|| Value::String(new_id()));
    comp.insert("blend".to_owned(), Value::String("add".to_owned()));
    json!({
        "id": obj.get("id").cloned().unwrap_or_else(|| Value::String(new_id())),
        "components": [Value::Object(comp)],
        "params": obj.get("params").cloned().unwrap_or_else(|| json!({})),
        "amount": obj.get("amount").cloned().unwrap_or_else(|| json!(100.0)),
        "invert": 0,
        "range": obj.get("range").cloned().unwrap_or_else(|| json!({})),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_json(kind: &str) -> Value {
        json!({
            "id": "m1", "type": kind,
            "x1": 0.5, "y1": 0.30, "x2": 0.5, "y2": 0.62,
            "feather": 0.0, "invert": 0, "angle": 0.0,
            "midpoint": 50.0, "amount": 100.0,
            "params": {"exposure": 1.0},
        })
    }

    #[test]
    fn linear_gradient_runs_from_start_to_end() {
        let mut m = parse_mask(&mask_json("linear")).unwrap();
        m.build_field(9, 9, None, None);
        let at = |y: usize| m.field[y * 9 + 4];
        assert!((at(0) - 1.0).abs() < 1e-6, "top {}", at(0));
        assert!((at(3) - 0.861).abs() < 0.01, "upper {}", at(3));
        assert!((at(4) - 0.316).abs() < 0.03, "mid {}", at(4));
        assert!(at(5) < 1e-6, "past end {}", at(5));
        assert!(at(8) < 1e-6, "bottom {}", at(8));
        // 水平方向是均匀的
        assert!((m.field[4 * 9] - m.field[4 * 9 + 8]).abs() < 1e-6);
    }

    #[test]
    fn invert_flips_field() {
        let mut raw = mask_json("linear");
        raw["invert"] = json!(1);
        let mut m = parse_mask(&raw).unwrap();
        m.build_field(9, 9, None, None);
        assert!(m.field[0] < 1e-6);
        assert!(m.field[80] > 0.99);
    }

    #[test]
    fn radial_is_one_inside_and_zero_far_away() {
        let mut raw = mask_json("radial");
        raw["x1"] = json!(0.5);
        raw["y1"] = json!(0.5);
        raw["x2"] = json!(0.75);
        raw["y2"] = json!(0.5);
        raw["feather"] = json!(0.0);
        let mut m = parse_mask(&raw).unwrap();
        m.build_field(32, 18, None, None);
        assert!(m.field[9 * 32 + 16] > 0.99, "center");
        assert!(m.field[0] < 1e-6, "corner");
        // 只横向拖 = 正圆：圆内的点无论上下左右都该是 1
        assert!(m.field[9 * 32 + 22] > 0.99);
        assert!(m.field[3 * 32 + 16] > 0.99);
    }

    #[test]
    fn amount_scales_and_zero_amount_is_inactive() {
        let mut raw = mask_json("linear");
        raw["amount"] = json!(50.0);
        let mut m = parse_mask(&raw).unwrap();
        m.build_field(9, 9, None, None);
        assert!((m.field[0] - 0.5).abs() < 1e-6);

        let mut p = GradeParams::default();
        raw["amount"] = json!(0.0);
        p.masks = vec![raw];
        assert!(active_masks(&p).is_empty());
    }

    #[test]
    fn zero_params_do_not_activate() {
        let mut raw = mask_json("linear");
        raw["params"] = json!({"exposure": 0.0});
        let mut p = GradeParams::default();
        p.masks = vec![raw];
        assert!(active_masks(&p).is_empty());
    }

    #[test]
    fn group_components_add_and_subtract() {
        let raw = json!({
            "id": "g", "amount": 100.0, "invert": 0,
            "params": {"exposure": 0.5},
            "components": [
                {"kind": "linear", "x1": 0.5, "y1": 0.0, "x2": 0.5,
                 "y2": 1.0, "feather": 0.0, "midpoint": 50.0, "blend": "add"},
                {"kind": "radial", "x1": 0.5, "y1": 0.5, "x2": 0.8,
                 "y2": 0.5, "feather": 0.0, "midpoint": 50.0,
                 "blend": "subtract"}
            ],
        });
        let mut m = parse_mask(&raw).unwrap();
        m.build_field(40, 40, None, None);
        // 竖线渐变在水平中线处约 0.5；中心再减掉径向 = 0
        assert!(
            m.field[20 * 40 + 20] < 0.02,
            "center {}",
            m.field[20 * 40 + 20]
        );
        assert!(m.field[0] > 0.9, "top {}", m.field[0]);
    }

    #[test]
    fn luminance_range_selects_bright_pixels() {
        let raw = json!({
            "id": "r", "amount": 100.0, "invert": 0,
            "params": {"exposure": 0.5},
            "range": {"lum_on": 1, "lum_min": 0.5, "lum_max": 1.0,
                      "lum_feather": 0.05},
            "components": [
                {"kind": "radial", "x1": 0.5, "y1": 0.5, "x2": 4.5,
                 "y2": 0.5, "feather": 0.0, "midpoint": 50.0}
            ],
        });
        let mut m = parse_mask(&raw).unwrap();
        let lum = vec![0.0f32, 1.0];
        m.build_field(2, 1, Some(&lum), None);
        assert!(m.field[0] < 1e-6);
        assert!(m.field[1] > 0.99);
    }

    #[test]
    fn hue_range_selects_matching_hue_and_rejects_gray() {
        // 目标色相 120°（绿）、带宽 60°、羽化 10°。两个像素：纯绿圈进来，纯红排除，
        // 灰色不管色相一律落选（饱和度太低）。
        let raw = json!({
            "id": "h", "amount": 100.0, "invert": 0,
            "params": {"exposure": 0.5},
            "range": {"hue_on": 1, "hue": 120.0, "hue_width": 60.0,
                      "hue_feather": 10.0, "hue_sat": 0.05},
            "components": [
                {"kind": "radial", "x1": 0.5, "y1": 0.5, "x2": 4.5,
                 "y2": 0.5, "feather": 0.0, "midpoint": 50.0}
            ],
        });
        let mut m = parse_mask(&raw).unwrap();
        let rgb = vec![0f32, 1.0, 0.0, 1.0, 0.0, 0.0, 0.4, 0.4, 0.4];
        m.build_field(3, 1, None, Some(&rgb));
        assert!(m.field[0] > 0.99, "绿 {}", m.field[0]);
        assert!(m.field[1] < 1e-6, "红 {}", m.field[1]);
        assert!(m.field[2] < 1e-6, "灰 {}", m.field[2]);
        // 色相环最短距离：目标 5°（红），红橙 hue≈6° 该进来，蓝 240° 排除。
        let mut near = parse_mask(&raw).unwrap();
        let near_rgb = vec![1.0f32, 0.1, 0.0, 0.0, 0.0, 1.0];
        near.range = None;
        near.hue.as_mut().unwrap().hue = 5.0;
        near.hue.as_mut().unwrap().width = 60.0;
        near.hue.as_mut().unwrap().feather = 10.0;
        near.build_field(2, 1, None, Some(&near_rgb));
        assert!(near.field[0] > 0.99, "5° 黄该进 5°±30° {}", near.field[0]);
        assert!(near.field[1] < 1e-6, "红不该进 {}", near.field[1]);
    }

    #[test]
    fn hue_range_off_or_missing_rgb_is_everywhere() {
        let raw = json!({
            "id": "h", "amount": 100.0, "invert": 0,
            "params": {"exposure": 0.5},
            "range": {"hue_on": 1, "hue": 120.0, "hue_width": 60.0},
            "components": [
                {"kind": "linear", "x1": 0.5, "y1": 0.0, "x2": 0.5,
                 "y2": 1.0, "feather": 0.0, "midpoint": 50.0}
            ],
        });
        let mut m = parse_mask(&raw).unwrap();
        m.build_field(9, 9, None, None);
        // 没图可查 = 全过；权重纯看几何。
        assert!((m.field[0] - 1.0).abs() < 1e-6);
        let mut off = parse_mask(&json!({
            "id": "h2", "amount": 100.0, "invert": 0,
            "params": {"exposure": 0.5},
            "range": {"hue_on": 0, "hue": 120.0, "hue_width": 60.0},
            "components": [
                {"kind": "linear", "x1": 0.5, "y1": 0.0, "x2": 0.5,
                 "y2": 1.0, "feather": 0.0, "midpoint": 50.0}
            ],
        }))
        .unwrap();
        let gray = vec![0.5f32; 9 * 9 * 3];
        off.build_field(9, 9, None, Some(&gray));
        assert!((off.field[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn delta_only_touches_mask_keys() {
        let p = GradeParams {
            exposure: 1.0,
            contrast: 10.0,
            ..Default::default()
        };
        let mut d = BTreeMap::new();
        d.insert("exposure".to_owned(), 0.5);
        d.insert("contrast".to_owned(), -4.0);
        d.insert("sharp_amount".to_owned(), 30.0);
        let q = apply_delta(&p, &d);
        assert!((q.exposure - 1.5).abs() < 1e-9);
        assert!((q.contrast - 6.0).abs() < 1e-9);
        assert_eq!(q.sharp_amount, p.sharp_amount);
    }

    #[test]
    fn as_group_keeps_flat_geometry_and_params() {
        let flat = json!({
            "id": "f", "type": "radial",
            "x1": 0.4, "y1": 0.4, "x2": 0.6, "y2": 0.4,
            "feather": 20.0, "amount": 80.0,
            "params": {"exposure": 0.25},
        });
        let g = as_group(&flat);
        assert_eq!(g["components"][0]["kind"], "radial");
        assert_eq!(g["components"][0]["x1"], 0.4);
        assert_eq!(g["amount"], 80.0);
        assert_eq!(g["params"]["exposure"], 0.25);
        assert_eq!(g["invert"], 0);
    }

    fn brush_json(strokes: Value) -> Value {
        json!({
            "id": "b", "amount": 100.0, "invert": 0,
            "params": {"exposure": 0.5},
            "components": [{"kind": "brush", "strokes": strokes}],
        })
    }

    fn dot(x: f64, y: f64) -> Value {
        json!({"points": [[x, y]], "radius": 0.2, "hardness": 1.0, "flow": 1.0})
    }

    #[test]
    fn brush_dot_covers_center_only() {
        // 41px 短边 × 半径 0.2 = 8.2px 圆章；硬度 1.0 → 整个圆都是硬核。
        let mut m = parse_mask(&brush_json(json!([dot(0.5, 0.5)]))).unwrap();
        m.build_field(41, 41, None, None);
        assert!(m.field[20 * 41 + 20] > 0.99, "center");
        assert!(m.field[20 * 41 + 21] > 0.99, "d=1 还在圆内");
        assert!(m.field[20 * 41 + 28] > 0.99, "d=8 还在 8.2 半径里");
        assert!(m.field[20 * 41 + 29] < 1e-6, "d=9 出圆");
        assert!(m.field[0] < 1e-6, "corner");
    }

    #[test]
    fn brush_hardness_softens_edge() {
        let mut raw = brush_json(json!([{
            "points": [[0.5, 0.5]], "radius": 0.2, "hardness": 0.5, "flow": 1.0,
        }]));
        let mut m = parse_mask(&raw).unwrap();
        m.build_field(41, 41, None, None);
        assert!((m.field[20 * 41 + 20] - 1.0).abs() < 1e-6, "center");
        assert!((m.field[22 * 41 + 20] - 1.0).abs() < 1e-6, "inner core");
        // d=7px：软硬度 0.5 已在过渡带里衰减到约 0.207
        assert!(
            (m.field[27 * 41 + 20] - 0.207).abs() < 0.02,
            "edge {}",
            m.field[27 * 41 + 20]
        );
        assert!(m.field[30 * 41 + 20] < 1e-6, "outside");
        raw["components"][0]["strokes"][0]["hardness"] = json!(1.0);
        let mut hard = parse_mask(&raw).unwrap();
        hard.build_field(41, 41, None, None);
        assert!(hard.field[27 * 41 + 20] > 0.99, "硬边 d=7 还在实心里");
        assert!(hard.field[30 * 41 + 20] < 1e-6, "硬边 d=9 出圆");
    }

    #[test]
    fn brush_flow_builds_across_strokes() {
        let strokes = json!([
            {"points": [[0.5, 0.5]], "radius": 0.2, "hardness": 1.0, "flow": 0.5},
            {"points": [[0.5, 0.5]], "radius": 0.2, "hardness": 1.0, "flow": 0.5},
        ]);
        let mut m = parse_mask(&brush_json(strokes)).unwrap();
        m.build_field(41, 41, None, None);
        // over 合成：1 - (1-0.5)² = 0.75
        assert!(
            (m.field[20 * 41 + 20] - 0.75).abs() < 1e-6,
            "center {}",
            m.field[20 * 41 + 20]
        );
    }

    #[test]
    fn brush_erase_removes_paint() {
        let strokes = json!([
            dot(0.5, 0.5),
            {"points": [[0.5, 0.5]], "radius": 0.2, "hardness": 1.0,
             "flow": 1.0, "erase": 1},
            dot(0.1, 0.1),
        ]);
        let mut m = parse_mask(&brush_json(strokes)).unwrap();
        m.build_field(41, 41, None, None);
        assert!(m.field[20 * 41 + 20] < 1e-6, "被擦掉的中心");
        assert!(m.field[4 * 41 + 4] > 0.99, "别处的笔画还在");
    }

    #[test]
    fn brush_polyline_is_continuous() {
        let strokes = json!([
            {"points": [[0.3, 0.5], [0.7, 0.5]], "radius": 0.1,
             "hardness": 0.6, "flow": 1.0},
        ]);
        let mut m = parse_mask(&brush_json(strokes)).unwrap();
        m.build_field(41, 41, None, None);
        // 线上的点（含中点）都是实心；离线 8px 的点够不着
        assert!(m.field[20 * 41 + 12] > 0.99, "start");
        assert!(m.field[20 * 41 + 20] > 0.99, "middle");
        assert!(m.field[20 * 41 + 28] > 0.99, "end");
        assert!(m.field[12 * 41 + 20] < 1e-6, "off line");
    }

    #[test]
    fn brush_caps_truncate() {
        let many: Vec<Value> = (0..300).map(|i| dot(i as f64 / 300.0, 0.5)).collect();
        let m = parse_mask(&brush_json(json!(many))).unwrap();
        assert_eq!(m.components[0].strokes.len(), MAX_STROKES);

        let long: Vec<Value> = (0..5000).map(|i| json!([i as f64 / 5000.0, 0.5])).collect();
        let one = json!({"points": long, "radius": 0.05});
        let m = parse_mask(&brush_json(json!([one]))).unwrap();
        assert_eq!(m.components[0].strokes[0].points.len(), MAX_STROKE_POINTS);
    }

    #[test]
    fn brush_stroke_defaults_and_clamps() {
        let raw = brush_json(json!([
            {"points": [[1.5, -0.5]]},
            {"points": [[0.5, 0.5]], "radius": 5.0, "hardness": 2.0, "flow": 0.0},
        ]));
        let m = parse_mask(&raw).unwrap();
        let strokes = &m.components[0].strokes;
        assert_eq!(strokes.len(), 2, "两条笔画都该活下来");
        let (a, b) = (&strokes[0], &strokes[1]);
        assert_eq!(a.radius, 0.06);
        assert_eq!(a.hardness, 0.7);
        assert_eq!(a.flow, 1.0);
        assert!(!a.erase);
        assert_eq!(a.points[0], [1.0, 0.0]);
        assert_eq!(b.radius, 0.5);
        assert_eq!(b.hardness, 1.0);
        assert_eq!(b.flow, 0.01);
    }

    #[test]
    fn new_brush_component_carries_empty_strokes() {
        let v = new_component(ShapeKind::Brush);
        assert_eq!(v["kind"], "brush");
        assert_eq!(v["strokes"], json!([]));
        // 空笔画的画笔组件解析不炸，场全 0
        let mut m = parse_mask(&brush_json(json!([]))).unwrap();
        m.build_field(8, 8, None, None);
        assert!(m.field.iter().all(|v| *v < 1e-6));
    }
}
