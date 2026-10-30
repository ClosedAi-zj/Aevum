//! 水印：把一行文字或一张图章在导出的最终画面上。
//!
//! 只在导出尾部生效，和输出锐化同一条规矩——预览不叠这层。理由是同一件事的两面：
//! 拖滑块时每帧都要重算，多叠一层就多花一份钱；而「面板上摆的等于交付件上有的」
//! 这句话，锐化和水印都得遵守。
//!
//! 文字层不引第二个字体栈：离线依赖里只有 `skrifa`（egui 带进来的那份），它出轮廓
//! 不出像素，所以曲线展平与非零环绕扫描线填充在这里自己写。不做 GPOS 排版
//! （连字、字距、双向），水印是一行落款，不是排版引擎——这一点测试里也钉着。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::OutlinePen;
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::fonts;

/// 九宫格里的一格。屏幕坐标，y 向下。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Spot {
    #[serde(rename = "top-left")]
    TopLeft,
    #[serde(rename = "top-center")]
    TopCenter,
    #[serde(rename = "top-right")]
    TopRight,
    #[serde(rename = "middle-left")]
    MiddleLeft,
    #[default]
    #[serde(rename = "center")]
    Center,
    #[serde(rename = "middle-right")]
    MiddleRight,
    #[serde(rename = "bottom-left")]
    BottomLeft,
    #[serde(rename = "bottom-center")]
    BottomCenter,
    #[serde(rename = "bottom-right")]
    BottomRight,
}

/// 横向锚点与纵向锚点：九宫格拆成两个三选一，摆放时各算一次。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Anchor {
    Start,
    Middle,
    End,
}

impl Spot {
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::TopCenter,
        Self::TopRight,
        Self::MiddleLeft,
        Self::Center,
        Self::MiddleRight,
        Self::BottomLeft,
        Self::BottomCenter,
        Self::BottomRight,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopCenter => "top-center",
            Self::TopRight => "top-right",
            Self::MiddleLeft => "middle-left",
            Self::Center => "center",
            Self::MiddleRight => "middle-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomCenter => "bottom-center",
            Self::BottomRight => "bottom-right",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let low = raw.trim().to_lowercase();
        Self::ALL.into_iter().find(|s| s.key() == low)
    }

    pub fn cn(self) -> &'static str {
        match self {
            Self::TopLeft => "左上",
            Self::TopCenter => "上中",
            Self::TopRight => "右上",
            Self::MiddleLeft => "左中",
            Self::Center => "正中",
            Self::MiddleRight => "右中",
            Self::BottomLeft => "左下",
            Self::BottomCenter => "下中",
            Self::BottomRight => "右下",
        }
    }

    fn axes(self) -> (Anchor, Anchor) {
        match self {
            Self::TopLeft | Self::MiddleLeft | Self::BottomLeft => (Anchor::Start, self.row()),
            Self::TopCenter | Self::Center | Self::BottomCenter => (Anchor::Middle, self.row()),
            _ => (Anchor::End, self.row()),
        }
    }

    fn row(self) -> Anchor {
        match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => Anchor::Start,
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight => Anchor::End,
            _ => Anchor::Middle,
        }
    }
}

/// 水印内容种类。两种互斥，界面用一排选择器切，切过去另一侧的输入留着不动。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatermarkKind {
    #[default]
    Text,
    Image,
}

impl WatermarkKind {
    pub const ALL: [Self; 2] = [Self::Text, Self::Image];

    pub fn key(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let low = raw.trim().to_lowercase();
        Self::ALL.into_iter().find(|k| k.key() == low)
    }

    pub fn cn(self) -> &'static str {
        match self {
            Self::Text => "文字",
            Self::Image => "图片",
        }
    }
}

/// 一版水印的全部设定。数值旋钮一律用整数百分比：界面滑杆、工程文件与命令面
/// 说的是同一个数，省掉一路 `0~1` 与 `0~100` 互相换算的往返误差。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Watermark {
    pub enabled: bool,
    pub kind: WatermarkKind,
    /// 文字水印的内容，`\n` 换行（最多按行铺开，不做自动折行）。
    pub text: String,
    /// 图片水印的路径；带 alpha 就按 alpha 叠，不带就是一块实心图。
    pub image: PathBuf,
    pub spot: Spot,
    /// 不透明度 0~100。
    pub opacity: u8,
    /// 尺寸：相对输出长边的百分比。文字取大写高，图片取宽。
    pub size: u8,
    /// 边距：相对输出短边的百分比，只有靠边的八格用得到。
    pub margin: u8,
    /// 文字颜色（sRGB 0~255）；图片自带颜色，这里忽略。
    pub color: [u8; 3],
}

impl Default for Watermark {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: WatermarkKind::Text,
            text: "© Aevum Studio".to_owned(),
            image: PathBuf::new(),
            spot: Spot::BottomRight,
            opacity: 60,
            size: 4,
            margin: 2,
            color: [255, 255, 255],
        }
    }
}

impl Watermark {
    /// 真的会盖章吗：开关、内容、不透明度三样缺一就不动像素。
    pub fn is_on(&self) -> bool {
        self.enabled
            && self.opacity > 0
            && self.size > 0
            && match self.kind {
                WatermarkKind::Text => !self.text.trim().is_empty(),
                WatermarkKind::Image => !self.image.as_os_str().is_empty(),
            }
    }
}

/// 章好的印章：直线 alpha（rgb 不预乘），长度 `w * h * 4`。
#[derive(Clone, Debug, PartialEq)]
struct Stamp {
    w: usize,
    h: usize,
    rgba: Vec<f32>,
}

/// 按设定把章打出来。`long_side` 是输出的长边，尺寸与边距都相对它算。
fn build_stamp(wm: &Watermark, long_side: usize) -> Option<Stamp> {
    let pct = |base: usize, p: u8| (base as f32 * f32::from(p) / 100.0).round() as usize;
    match wm.kind {
        WatermarkKind::Text => {
            text_stamp(&wm.text, pct(long_side, wm.size).max(4) as f32, wm.color)
        }
        WatermarkKind::Image => image_stamp(&wm.image, pct(long_side, wm.size).max(4)),
    }
}

/// 章比画面宽就整体缩一档（按宽度反算尺寸百分比，只重试一次）：
/// 落款被切一半比字小一点难看多了。
fn fit_stamp(wm: &Watermark, long_side: usize, canvas_w: usize) -> Option<Stamp> {
    let stamp = build_stamp(wm, long_side)?;
    let max_w = canvas_w * 9 / 10;
    if stamp.w <= max_w {
        return Some(stamp);
    }
    let shrunk = (f32::from(wm.size) * max_w as f32 / stamp.w as f32) as u8;
    if shrunk == 0 || shrunk >= wm.size {
        return Some(stamp);
    }
    let retry = build_stamp(
        &Watermark {
            size: shrunk,
            ..wm.clone()
        },
        long_side,
    );
    Some(retry.unwrap_or(stamp))
}

/// 一笔闭合轮廓展平后的多段线（填充时一律按闭合处理，末点自动接回起点）。
#[derive(Clone, Default)]
struct Poly {
    pts: Vec<[f32; 2]>,
}

/// 把字形轮廓收进多段线：skrifa 只发命令，展平曲线与坐标翻转在这里做。
struct Pen<'a> {
    polys: &'a mut Vec<Poly>,
    cur: Vec<[f32; 2]>,
    last: [f32; 2],
    ox: f32,
    oy: f32,
    /// 展平容差（像素）。四倍超采样下 0.15px 的弦误差肉眼看不见。
    tol: f32,
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        let p = self.at(x, y);
        self.last = p;
        self.cur.push(p);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.last = p;
        self.cur.push(p);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let c = self.at(cx0, cy0);
        let e = self.at(x, y);
        let s = self.last;
        flatten_quad(&mut self.cur, s, c, e, self.tol, 0);
        self.last = e;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let c0 = self.at(cx0, cy0);
        let c1 = self.at(cx1, cy1);
        let e = self.at(x, y);
        let s = self.last;
        flatten_cubic(&mut self.cur, s, c0, c1, e, self.tol, 0);
        self.last = e;
    }

    fn close(&mut self) {
        self.flush();
    }
}

impl Pen<'_> {
    fn at(&self, x: f32, y: f32) -> [f32; 2] {
        // 字体坐标 y 向上，图像坐标 y 向下：翻一次，之后所有几何都在图像空间里。
        [self.ox + x, self.oy - y]
    }

    fn flush(&mut self) {
        if self.cur.len() >= 3 {
            self.polys.push(Poly {
                pts: std::mem::take(&mut self.cur),
            });
        }
        self.cur.clear();
    }
}

/// 二次贝塞尔：控制点到弦的距离小于容差就当成直线，否则 de Casteljau 折两半。
fn flatten_quad(
    out: &mut Vec<[f32; 2]>,
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    tol: f32,
    d: u32,
) {
    if d >= 12 || bow(p1, p0, p2) <= tol {
        out.push(p2);
        return;
    }
    let a = lerp(p0, p1, 0.5);
    let b = lerp(p1, p2, 0.5);
    let m = lerp(a, b, 0.5);
    flatten_quad(out, p0, a, m, tol, d + 1);
    flatten_quad(out, m, b, p2, tol, d + 1);
}

fn flatten_cubic(
    out: &mut Vec<[f32; 2]>,
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
    p3: [f32; 2],
    tol: f32,
    d: u32,
) {
    if d >= 12 || bow(p1, p0, p3).max(bow(p2, p0, p3)) <= tol {
        out.push(p3);
        return;
    }
    let a = lerp(p0, p1, 0.5);
    let b = lerp(p1, p2, 0.5);
    let c = lerp(p2, p3, 0.5);
    let d1 = lerp(a, b, 0.5);
    let d2 = lerp(b, c, 0.5);
    let m = lerp(d1, d2, 0.5);
    flatten_cubic(out, p0, a, d1, m, tol, d + 1);
    flatten_cubic(out, m, d2, c, p3, tol, d + 1);
}

fn lerp(p: [f32; 2], q: [f32; 2], t: f32) -> [f32; 2] {
    [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]
}

/// 控制点到 `from`→`to` 弦的垂直距离。
fn bow(p: [f32; 2], from: [f32; 2], to: [f32; 2]) -> f32 {
    let dx = to[0] - from[0];
    let dy = to[1] - from[1];
    let len = (dx * dx + dy * dy).sqrt();
    if len <= f32::EPSILON {
        return ((p[0] - from[0]).powi(2) + (p[1] - from[1]).powi(2)).sqrt();
    }
    ((p[0] - from[0]) * dy - (p[1] - from[1]) * dx).abs() / len
}

/// 栅格化结果：从 `(0, 0)` 起算的覆盖率，取值 0~1，长度 `w * h`。
struct Raster {
    w: usize,
    h: usize,
    cov: Vec<f32>,
}

/// 非零环绕 + 每轴 `ss` 倍超采样：纵向按子行取覆盖比例，横向按跨度的小数端点
/// 精确算，所以斜边不需要更高采样率也不带锯齿。
fn rasterize(polys: &[Poly], ss: usize) -> Option<Raster> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in polys {
        for pt in &p.pts {
            min_x = min_x.min(pt[0]);
            max_x = max_x.max(pt[0]);
            min_y = min_y.min(pt[1]);
            max_y = max_y.max(pt[1]);
        }
    }
    if !(max_x > min_x && max_y > min_y) {
        return None;
    }
    let x0 = min_x.floor() as i32 - 1;
    let y0 = min_y.floor() as i32 - 1;
    let w = (max_x.ceil() as i32 + 2 - x0) as usize;
    let h = (max_y.ceil() as i32 + 2 - y0) as usize;
    if w == 0 || h == 0 || w.saturating_mul(h) > 64 * 1024 * 1024 {
        return None;
    }
    let mut cov = vec![0f32; w * h];
    let mut hits: Vec<(f32, i32)> = Vec::new();
    let sub = 1.0 / ss as f32;
    for row in 0..h {
        for s in 0..ss {
            let y = y0 as f32 + row as f32 + (s as f32 + 0.5) * sub;
            hits.clear();
            for p in polys {
                let n = p.pts.len();
                for i in 0..n {
                    let a = p.pts[i];
                    let b = p.pts[(i + 1) % n];
                    let (lo, hi) = if a[1] <= b[1] { (a, b) } else { (b, a) };
                    if y < lo[1] || y >= hi[1] {
                        continue;
                    }
                    let t = (y - a[1]) / (b[1] - a[1]);
                    hits.push((a[0] + (b[0] - a[0]) * t, if b[1] > a[1] { 1 } else { -1 }));
                }
            }
            if hits.is_empty() {
                continue;
            }
            hits.sort_by(|l, r| l.0.total_cmp(&r.0));
            let mut winding = 0i32;
            let mut span_start = 0f32;
            for k in 0..hits.len() {
                let (x, dir) = hits[k];
                if winding != 0 && x > span_start {
                    fill_span(&mut cov, w, row, span_start, x, x0, sub);
                }
                winding += dir;
                if winding != 0 {
                    span_start = x;
                }
            }
        }
    }
    for v in &mut cov {
        *v = v.clamp(0.0, 1.0);
    }
    Some(Raster { w, h, cov })
}

/// 把一条覆盖跨度按列摊进 `cov`：跨度的两端可以落在像素中间，所以是精确的
/// 水平抗锯齿，纵向权重由调用方按超采样份数给。
fn fill_span(cov: &mut [f32], w: usize, row: usize, from: f32, to: f32, x0: i32, weight: f32) {
    let first = (from - x0 as f32).floor().max(0.0) as usize;
    let last = (to - x0 as f32).ceil().max(0.0) as usize;
    for col in first..last.min(w) {
        let left = (x0 + col as i32) as f32;
        let overlap = to.min(left + 1.0) - from.max(left);
        if overlap > 0.0 {
            cov[row * w + col] += overlap * weight;
        }
    }
}

/// 字体字节表：先拉丁，再中文。逐字回退，所以混排一行不用挑字体。
const FONT_STACK: [&[u8]; 2] = [fonts::INTER, fonts::NOTO_SC];

/// 缓存键：文字、字号位型、颜色。Stamp 是结果本体。
type TextKey = (String, u32, [u8; 3], Stamp);

/// 把一行（或多行）文字栅格成印章。`cap_px` 是大写高度，也就是「这行字多大」。
///
/// 单条目缓存：导出几百帧落款内容不变，每帧都重新 shaping + 4 倍超采样是白干的活。
/// 键带上文字、字号与颜色，换内容自然失效；fit_stamp 缩一档重试也各存各的。
fn text_stamp(text: &str, cap_px: f32, color: [u8; 3]) -> Option<Stamp> {
    static CACHE: Mutex<Option<TextKey>> = Mutex::new(None);
    let key_px = cap_px.to_bits();
    if let Ok(cache) = CACHE.lock()
        && let Some((t, px, c, stamp)) = cache.as_ref()
        && t == text
        && *px == key_px
        && *c == color
    {
        return Some(stamp.clone());
    }
    let stamp = text_stamp_uncached(text, cap_px, color)?;
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((text.to_owned(), key_px, color, stamp.clone()));
    }
    Some(stamp)
}

fn text_stamp_uncached(text: &str, cap_px: f32, color: [u8; 3]) -> Option<Stamp> {
    let faces: Vec<FontRef<'static>> = FONT_STACK
        .iter()
        .filter_map(|bytes| FontRef::new(bytes).ok())
        .collect();
    if faces.is_empty() || cap_px <= 0.0 {
        return None;
    }
    // 字号按第一个字体的大写高反算：用户说的是「字多大」，不是「em 多大」。
    let base = faces[0].metrics(Size::new(1.0), LocationRef::default());
    let cap_em = base
        .cap_height
        .filter(|v| *v > 0.0)
        .unwrap_or_else(|| base.ascent.max(0.5) * 0.7);
    let size = Size::new(cap_px / cap_em);
    let line_metrics = faces[0].metrics(size, LocationRef::default());
    let line_h = (line_metrics.ascent - line_metrics.descent + line_metrics.leading.max(0.0))
        .max(cap_px * 1.15);

    let mut polys: Vec<Poly> = Vec::new();
    for (row, line) in text.split('\n').enumerate() {
        let oy = row as f32 * line_h;
        let mut pen_x = 0f32;
        for ch in line.chars() {
            let Some((face_idx, gid)) = find_glyph(&faces, ch) else {
                // 两个字体都没有这个码位：按平均字宽留个空，别让整行错位。
                pen_x += line_metrics.average_width.unwrap_or(cap_px * 0.5);
                continue;
            };
            let face = &faces[face_idx];
            let gm = face.glyph_metrics(size, LocationRef::default());
            let adv = gm.advance_width(gid).unwrap_or(cap_px * 0.5);
            let Some(glyph) = face.outline_glyphs().get(gid) else {
                pen_x += adv;
                continue;
            };
            let mut pen = Pen {
                polys: &mut polys,
                cur: Vec::new(),
                last: [pen_x, oy],
                ox: pen_x,
                oy,
                tol: 0.15,
            };
            // 画不出来（空轮廓、超出提示范围）就只推进，不报错：水印不该让导出失败。
            let _ = glyph.draw(size, &mut pen);
            pen.flush();
            pen_x += adv;
        }
    }
    if polys.is_empty() {
        return None;
    }
    let raster = rasterize(&polys, 4)?;
    let rgb = color.map(|v| v as f32 / 255.0);
    let mut rgba = vec![0f32; raster.w * raster.h * 4];
    for (i, c) in raster.cov.iter().enumerate() {
        rgba[i * 4] = rgb[0];
        rgba[i * 4 + 1] = rgb[1];
        rgba[i * 4 + 2] = rgb[2];
        rgba[i * 4 + 3] = *c;
    }
    Some(Stamp {
        w: raster.w,
        h: raster.h,
        rgba,
    })
}

/// 逐字回退：先拉丁（Inter），再中文（Noto Sans SC）。返回字体下标而不是 `FontRef`
/// 本身——它不是 `Copy`，从共享引用里搬出来编译就过不去。
fn find_glyph(faces: &[FontRef<'static>], ch: char) -> Option<(usize, GlyphId)> {
    faces
        .iter()
        .enumerate()
        .find_map(|(i, f)| f.charmap().map(ch).map(|gid| (i, gid)))
}

/// 图片水印：解码 → 按目标宽度等比缩放 → 摊成 f32 直线 alpha。
///
/// 单条目缓存：一次导出几百帧，每张都重解一张 PNG 是白干的活。键里带上目标宽度，
/// 换画幅时自然失效。
fn image_stamp(path: &Path, target_w: usize) -> Option<Stamp> {
    static CACHE: Mutex<Option<(PathBuf, usize, Stamp)>> = Mutex::new(None);
    let mut cache = CACHE.lock().ok()?;
    if let Some((p, w, stamp)) = cache.as_ref()
        && *p == path
        && *w == target_w
    {
        return Some(stamp.clone());
    }
    let stamp = decode_stamp(path, target_w)?;
    *cache = Some((path.to_path_buf(), target_w, stamp.clone()));
    Some(stamp)
}

fn decode_stamp(path: &Path, target_w: usize) -> Option<Stamp> {
    let img = image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.decode().ok())?
        .to_rgba8();
    let (ow, oh) = (img.width() as usize, img.height() as usize);
    if ow == 0 || oh == 0 || target_w == 0 {
        return None;
    }
    let tw = target_w.clamp(1, 8192) as u32;
    let th = ((oh as u64 * tw as u64 / ow as u64).max(1)) as u32;
    let scaled = if tw as usize == ow && th as usize == oh {
        img
    } else {
        image::imageops::resize(&img, tw, th, image::imageops::FilterType::Lanczos3)
    };
    let (w, h) = (scaled.width() as usize, scaled.height() as usize);
    let mut rgba = vec![0f32; w * h * 4];
    for (i, px) in scaled.pixels().enumerate() {
        for c in 0..4 {
            rgba[i * 4 + c] = px.0[c] as f32 / 255.0;
        }
    }
    Some(Stamp { w, h, rgba })
}

/// 印章左上角落在画面哪里：靠边的格子按 `margin` 让开，居中的按剩余空间取中。
fn origin(w: usize, h: usize, stamp: &Stamp, spot: Spot, margin: usize) -> (usize, usize) {
    let (ax, ay) = spot.axes();
    let x = match ax {
        Anchor::Start => margin,
        Anchor::Middle => w.saturating_sub(stamp.w) / 2,
        Anchor::End => w.saturating_sub(stamp.w + margin),
    };
    let y = match ay {
        Anchor::Start => margin,
        Anchor::Middle => h.saturating_sub(stamp.h) / 2,
        Anchor::End => h.saturating_sub(stamp.h + margin),
    };
    (x.min(w), y.min(h))
}

/// 把水印叠到最终像素上（sRGB 计算空间，色域搬运之前）。
///
/// 关掉、不透明度为 0、内容空、图打不开——一律原像素不动，也不报错：
/// 水印是交付层的一层皮，不该让一张导出好的片子失败。
pub fn apply_in_place(rgb: &mut [f32], w: usize, h: usize, wm: &Watermark) {
    if !wm.is_on() || w == 0 || h == 0 {
        return;
    }
    let Some(stamp) = fit_stamp(wm, w.max(h), w) else {
        return;
    };
    if stamp.w == 0 || stamp.h == 0 {
        return;
    }
    let margin = (w.min(h) as f32 * f32::from(wm.margin.min(50)) / 100.0).round() as usize;
    let (ox, oy) = origin(w, h, &stamp, wm.spot, margin);
    let opacity = f32::from(wm.opacity.min(100)) / 100.0;
    let sw = stamp.w.min(w.saturating_sub(ox));
    let sh = stamp.h.min(h.saturating_sub(oy));
    for sy in 0..sh {
        let dy = oy + sy;
        for sx in 0..sw {
            let dx = ox + sx;
            let s = (sy * stamp.w + sx) * 4;
            let a = stamp.rgba[s + 3] * opacity;
            if a <= 0.0 {
                continue;
            }
            let d = (dy * w + dx) * 3;
            for c in 0..3 {
                rgb[d + c] = rgb[d + c] * (1.0 - a) + stamp.rgba[s + c] * a;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("aevum-wm-{tag}-{}-{nanos}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wm(kind: WatermarkKind) -> Watermark {
        Watermark {
            enabled: true,
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn spot_keys_roundtrip_and_cover_the_nine_grid() {
        assert_eq!(Spot::ALL.len(), 9);
        for s in Spot::ALL {
            assert_eq!(Spot::parse(s.key()), Some(s));
            assert!(!s.cn().is_empty());
        }
        assert_eq!(Spot::parse("middle-center"), None);
        assert_eq!(Spot::parse(" BOTTOM-RIGHT "), Some(Spot::BottomRight));
        // 九宫格的横纵锚点必须凑齐三种，不然某一侧会永远贴边。
        for want in [Anchor::Start, Anchor::Middle, Anchor::End] {
            assert!(Spot::ALL.iter().filter(|s| s.axes().0 == want).count() == 3);
            assert!(Spot::ALL.iter().filter(|s| s.axes().1 == want).count() == 3);
        }
    }

    #[test]
    fn watermark_json_uses_the_command_surface_keys() {
        let mut set = wm(WatermarkKind::Text);
        set.spot = Spot::BottomRight;
        let json = serde_json::to_string(&set).unwrap();
        assert!(json.contains("\"spot\":\"bottom-right\""), "{json}");
        assert!(json.contains("\"kind\":\"text\""), "{json}");
        let back: Watermark = serde_json::from_str(&json).unwrap();
        assert_eq!(back, set);
        // 旧工程文件里没有这一段：整块缺字段要走 `Watermark::default()`，
        // 也就是「没开 + 那行默认落款」，不许解析失败。
        let legacy: Watermark = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy, Watermark::default());
        assert!(!legacy.enabled);
    }

    #[test]
    fn off_switches_and_empty_content_leave_pixels_alone() {
        let mut px = vec![0.25f32; 64 * 64 * 3];
        let before = px.clone();
        // 没开
        apply_in_place(&mut px, 64, 64, &Watermark::default());
        assert_eq!(px, before);
        // 开了但内容空
        let mut empty = wm(WatermarkKind::Text);
        empty.text = "   ".to_owned();
        apply_in_place(&mut px, 64, 64, &empty);
        assert_eq!(px, before);
        // 不透明度 0
        let mut zero = wm(WatermarkKind::Text);
        zero.opacity = 0;
        apply_in_place(&mut px, 64, 64, &zero);
        assert_eq!(px, before);
        // 图片不存在：静悄悄什么都不做
        let mut missing = wm(WatermarkKind::Image);
        missing.image = PathBuf::from("Z:/没有这个文件.png");
        apply_in_place(&mut px, 64, 64, &missing);
        assert_eq!(px, before, "水印读不到图不该影响导出");
    }

    #[test]
    fn text_stamp_draws_holes_and_survives_a_font_switch() {
        // 大写高 32px 的一行 A：遮罩的包围盒应该被填满字宽，中间那个三角洞是空的。
        let stamp = text_stamp("A", 32.0, [255, 255, 255]).expect("A 要有轮廓");
        assert!(stamp.w > 20 && stamp.w < 80, "A 的宽度离谱：{}", stamp.w);
        assert!(stamp.h >= 28 && stamp.h <= 48, "A 的高度离谱：{}", stamp.h);
        let max_cov = stamp
            .rgba
            .chunks_exact(4)
            .map(|p| p[3])
            .fold(0f32, f32::max);
        assert!(max_cov > 0.9, "实心处覆盖率才 {max_cov}，填充八成漏了");
        // 洞：横过 A 的中线上必须有一段几乎为零的区间。
        let mid = stamp.h / 2;
        let row: Vec<f32> = (0..stamp.w)
            .map(|x| stamp.rgba[(mid * stamp.w + x) * 4 + 3])
            .collect();
        assert!(
            row.windows(3)
                .any(|w3| w3.iter().all(|v| *v < 0.02) && row.iter().any(|v| *v > 0.5)),
            "A 的中线没有洞：{row:?}"
        );

        // 中文走回退字体：Noto 里没有就该是空的，有就该是个接近方形的全角字面。
        let cjk = text_stamp("光", 32.0, [255, 255, 255]).expect("中文要有轮廓");
        assert!(
            cjk.w > 24 && cjk.h > 24,
            "中文字面比拉丁还小，回退字体没生效：{}×{}",
            cjk.w,
            cjk.h
        );
        assert!(
            (cjk.w as f32 / cjk.h as f32 - 1.0).abs() < 0.5,
            "全角字面该接近方形：{}×{}",
            cjk.w,
            cjk.h
        );
        let ink = |s: &Stamp| -> f32 { s.rgba.chunks_exact(4).map(|p| p[3]).sum() };
        assert!(ink(&cjk) > ink(&stamp), "中文比 A 还淡，轮廓八成没填上");

        // 同样的设定两次栅格必须一模一样：不许有随机抖动或浮点累加顺序差。
        let again = text_stamp("A", 32.0, [255, 255, 255]).unwrap();
        assert_eq!(again, stamp, "栅格不确定");
    }

    #[test]
    fn text_scales_and_wraps_lines() {
        let small = text_stamp("Aevum", 16.0, [255, 255, 255]).unwrap();
        let big = text_stamp("Aevum", 32.0, [255, 255, 255]).unwrap();
        assert!(
            big.w as f32 > small.w as f32 * 1.5 && big.h > small.h,
            "尺寸没按比例走"
        );
        let two = text_stamp("A\nA", 32.0, [255, 255, 255]).unwrap();
        let one = text_stamp("A", 32.0, [255, 255, 255]).unwrap();
        assert!(
            two.h > one.h + 20,
            "换行没把行高叠上：{} vs {}",
            two.h,
            one.h
        );
        assert!(
            (two.w as f32 - one.w as f32).abs() < one.w as f32 * 0.2,
            "两行的宽度不该差太多"
        );
    }

    #[test]
    fn stamp_lands_where_the_nine_grid_says() {
        let (w, h) = (100usize, 60usize);
        let stamp = Stamp {
            w: 10,
            h: 6,
            rgba: vec![1.0; 10 * 6 * 4],
        };
        assert_eq!(origin(w, h, &stamp, Spot::TopLeft, 4), (4, 4));
        assert_eq!(origin(w, h, &stamp, Spot::BottomRight, 4), (86, 50));
        assert_eq!(origin(w, h, &stamp, Spot::Center, 4), (45, 27));
        assert_eq!(origin(w, h, &stamp, Spot::TopCenter, 0), (45, 0));
        // 章比画面还大时不许跑到负坐标：夹回画面里，宁可被裁。
        let huge = Stamp {
            w: 300,
            h: 300,
            rgba: vec![1.0; 300 * 300 * 4],
        };
        let (x, y) = origin(w, h, &huge, Spot::BottomRight, 4);
        assert!(x <= w && y <= h, "越界了：{x},{y}");
    }

    #[test]
    fn apply_blends_towards_the_stamp_colour() {
        let (w, h) = (200usize, 120usize);
        let mut px = vec![0.0f32; w * h * 3];
        let mut set = wm(WatermarkKind::Text);
        set.text = "I".to_owned();
        set.opacity = 100;
        set.color = [255, 255, 255];
        set.size = 20;
        set.spot = Spot::Center;
        apply_in_place(&mut px, w, h, &set);
        let lit = px.iter().copied().fold(0f32, f32::max);
        assert!(lit > 0.9, "黑底上盖白字却盖不出亮来：{lit}");
        // 半透明：同一处设定砍一半不透明度，峰值也该跟着砍。
        let mut half = px.clone();
        half.fill(0.0);
        set.opacity = 50;
        apply_in_place(&mut half, w, h, &set);
        let dim = half.iter().copied().fold(0f32, f32::max);
        assert!(dim > 0.4 && dim < lit, "不透明度没起作用：{dim} vs {lit}");
        // 正中：四角必须还是黑的。
        assert_eq!(&px[0..3], &[0.0, 0.0, 0.0]);
        let corner = (w * (h - 1) - 1) * 3;
        assert_eq!(&px[corner..corner + 3], &[0.0, 0.0, 0.0]);
    }

    #[test]
    fn image_stamp_keeps_alpha_and_aspect() {
        let dir = temp_dir("wmimg");
        let path = dir.join("logo.png");
        // 4×2 的图：左半实心红、右半全透明。缩到 40px 宽就该是 20px 高。
        let mut img = image::RgbaImage::new(4, 2);
        for (i, px) in img.pixels_mut().enumerate() {
            // 每行 4 个像素：左半实心红，右半全透明蓝。
            *px = if i % 4 < 2 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 0])
            };
        }
        img.save(&path).unwrap();

        let stamp = decode_stamp(&path, 40).expect("解码 logo");
        assert_eq!((stamp.w, stamp.h), (40, 20), "等比缩放没保住 2:1");
        assert!(stamp.rgba[3] > 0.9, "实心处 alpha 丢了");
        assert!(
            stamp.rgba[(stamp.h / 2 * stamp.w + stamp.w - 1) * 4 + 3] < 0.05,
            "透明处不该有 alpha"
        );

        // 走一遍公开口：黑底上盖出红来，说明图片水印真的接得上。
        let mut px = vec![0f32; 200 * 120 * 3];
        let mut set = wm(WatermarkKind::Image);
        set.image = path.clone();
        set.size = 20;
        set.opacity = 100;
        set.spot = Spot::TopLeft;
        set.margin = 0;
        apply_in_place(&mut px, 200, 120, &set);
        assert!(
            px[0] > 0.9 && px[1] < 0.1,
            "左上角该是红的：{:?}",
            &px[0..3]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_overwide_stamp_shrinks_instead_of_getting_cut() {
        // 一行长落款按 20% 字高排开远超画面宽：不许切一半，要整体缩进画框。
        let (w, h) = (300usize, 120usize);
        let mut px = vec![0.5f32; w * h * 3];
        let mut set = wm(WatermarkKind::Text);
        set.text = "恒晷 Aevum Studio 一长串落款落款落款".to_owned();
        set.size = 20;
        set.opacity = 100;
        set.spot = Spot::BottomRight;
        set.margin = 0;
        apply_in_place(&mut px, w, h, &set);
        let lit = px.iter().copied().fold(0f32, f32::max);
        assert!(lit > 0.9, "缩完什么都没盖上：{lit}");
        // 最右一列与最下一行必须还是原色：章没挂到边外，也没被切。
        let right = (0..h).map(|y| px[(y * w + w - 1) * 3]).collect::<Vec<_>>();
        assert!(
            right.iter().all(|v| (*v - 0.5).abs() < 1e-6),
            "右边被盖到了：{right:?}"
        );
        let bottom = (0..w)
            .map(|x| px[((h - 1) * w + x) * 3])
            .collect::<Vec<_>>();
        assert!(
            bottom.iter().all(|v| (*v - 0.5).abs() < 1e-6),
            "底边被盖到了"
        );
    }

    #[test]
    fn curved_outlines_stay_smooth_enough() {
        // 圆是纯曲线：展平不够细的话，中线会出现锯齿状的台阶。
        let stamp = text_stamp("O", 48.0, [255, 255, 255]).unwrap();
        let mid = stamp.h / 2;
        let mut edge = None;
        for x in 0..stamp.w {
            let c = stamp.rgba[(mid * stamp.w + x) * 4 + 3];
            if c > 0.02 && c < 0.98 {
                edge = Some(x);
                break;
            }
        }
        assert!(edge.is_some(), "轮廓边缘全是硬跳变，曲线没展平");
    }
}
