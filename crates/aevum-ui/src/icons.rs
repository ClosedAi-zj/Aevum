//! 几何图标：全部用 painter 画，不引入图标字体或第三方素材。
//!
//! Geometric icons: all drawn with `painter`, no icon font and no third-party
//! assets.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Sequence,
    Deflicker,
    Develop,
    Frame,
    Render,
    Audio,
    Hand,
    Zoom,
    Crop,
    Compare,
    Grid,
    Play,
    Pause,
    Prev,
    Next,
    Folder,
    Plus,
    Minus,
    ChevronDown,
    ChevronRight,
    Star,
    StarEmpty,
    Flag,
    Eye,
    Histogram,
    Sliders,
    Info,
    Key,
    Reset,
    History,
    Close,
    Export,
    Help,
    Gear,
    /// 合成模块（Module::Merge）：多层进、一张出。
    ///
    /// The Merge module: layers in, one image out.
    Merge,
}

fn pt(r: Rect, x: f32, y: f32) -> Pos2 {
    Pos2::new(r.left() + x * r.width(), r.top() + y * r.height())
}

/// 图标描边宽度区间：macOS 发丝线口径（1.2pt 基准，小图标不低于 1px）。
/// 颜色不在这里定义——调用方传 theme 的 ACCENT / TEXT / MUTED / DIM，层级自动跟随。
///
/// Icon stroke-width range: macOS hairline convention (1.2pt base, never below 1px
/// for small icons).
/// Colours are not defined here -- callers pass the theme's ACCENT / TEXT / MUTED /
/// DIM, and the hierarchy follows automatically.
pub const STROKE_MIN: f32 = 1.0;
pub const STROKE_MAX: f32 = 1.55;

fn scaled(r: Rect, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::from_min_max(pt(r, x0, y0), pt(r, x1, y1))
}

fn seg(p: &Painter, a: Pos2, b: Pos2, st: Stroke) {
    p.line_segment([a, b], st);
}

fn star_points(r: Rect) -> Vec<Pos2> {
    let c = r.center();
    let outer = r.width().min(r.height()) * 0.46;
    let inner = outer * 0.44;
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        let rad = if i % 2 == 0 { outer } else { inner };
        pts.push(Pos2::new(c.x + rad * a.cos(), c.y + rad * a.sin()));
    }
    pts
}

pub fn paint(painter: &Painter, rect: Rect, glyph: Glyph, color: Color32) {
    // 对齐到物理像素，避免 1px 线落在像素中间发灰、像虚线。
    let ppp = painter.ctx().pixels_per_point().max(1.0);
    let snap = |v: f32| (v * ppp).round() / ppp;
    let rect = Rect::from_min_max(
        Pos2::new(snap(rect.min.x), snap(rect.min.y)),
        Pos2::new(snap(rect.max.x), snap(rect.max.y)),
    );
    let s = rect.width().min(rect.height());
    let sw = (s * 0.075).clamp(STROKE_MIN, STROKE_MAX);
    let st = Stroke::new(sw, color);
    let r = rect.shrink(s * 0.1);
    match glyph {
        Glyph::Sequence => {
            let body = scaled(r, 0.08, 0.18, 0.92, 0.82);
            painter.rect_stroke(body, 1, st, StrokeKind::Inside);
            for k in 0..3 {
                let y = 0.27 + k as f32 * 0.19;
                let d = s * 0.05;
                painter.rect_filled(
                    Rect::from_center_size(pt(r, 0.15, y), Vec2::splat(d)),
                    0,
                    color,
                );
                painter.rect_filled(
                    Rect::from_center_size(pt(r, 0.85, y), Vec2::splat(d)),
                    0,
                    color,
                );
            }
            seg(painter, pt(r, 0.27, 0.18), pt(r, 0.27, 0.82), st);
            seg(painter, pt(r, 0.73, 0.18), pt(r, 0.73, 0.82), st);
        }
        Glyph::Deflicker => {
            let pts: Vec<Pos2> = (0..=8)
                .map(|i| {
                    let t = i as f32 / 8.0;
                    let a = if i % 2 == 0 { -1.0 } else { 1.0 };
                    let fade = 1.0 - t * 0.55;
                    pt(r, 0.06 + t * 0.88, 0.46 + a * 0.28 * fade)
                })
                .collect();
            painter.add(Shape::line(pts, st));
            seg(
                painter,
                pt(r, 0.06, 0.8),
                pt(r, 0.94, 0.8),
                Stroke::new(sw * 0.8, color),
            );
        }
        Glyph::Develop => {
            for (y, x) in [(0.28, 0.32), (0.5, 0.68), (0.72, 0.45)] {
                seg(
                    painter,
                    pt(r, 0.12, y),
                    pt(r, 0.88, y),
                    Stroke::new(sw * 0.85, color),
                );
                painter.circle_filled(pt(r, x, y), sw * 1.6, color);
            }
        }
        Glyph::Frame => {
            let a = 0.24;
            let b = 0.76;
            let l = 0.22;
            for (p, hx, hy) in [
                ((a, a), 1.0, 1.0),
                ((b, a), -1.0, 1.0),
                ((b, b), -1.0, -1.0),
                ((a, b), 1.0, -1.0),
            ] {
                seg(painter, pt(r, p.0, p.1), pt(r, p.0 + l * hx, p.1), st);
                seg(painter, pt(r, p.0, p.1), pt(r, p.0, p.1 + l * hy), st);
            }
        }
        Glyph::Render => {
            let body = scaled(r, 0.1, 0.16, 0.9, 0.84);
            painter.rect_stroke(body, 1, st, StrokeKind::Inside);
            let tri = vec![pt(r, 0.4, 0.34), pt(r, 0.4, 0.66), pt(r, 0.7, 0.5)];
            painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
        }
        Glyph::Audio => {
            for (i, h) in [0.24f32, 0.46, 0.7, 0.5, 0.3].iter().enumerate() {
                let x = 0.2 + i as f32 * 0.15;
                let bar = Rect::from_center_size(pt(r, x, 0.5), Vec2::new(s * 0.07, s * h * 0.8));
                painter.rect_filled(bar, 0, color);
            }
        }
        Glyph::Hand => {
            painter.rect_filled(scaled(r, 0.26, 0.44, 0.76, 0.9), s * 0.1, color);
            for (x, top) in [(0.3, 0.2), (0.46, 0.1), (0.62, 0.22)] {
                painter.rect_filled(scaled(r, x, top, x + 0.11, 0.6), s * 0.05, color);
            }
            painter.rect_filled(scaled(r, 0.13, 0.44, 0.26, 0.68), s * 0.06, color);
        }
        Glyph::Zoom => {
            painter.circle_stroke(pt(r, 0.44, 0.42), s * 0.26, st);
            seg(
                painter,
                pt(r, 0.63, 0.61),
                pt(r, 0.9, 0.88),
                Stroke::new(sw * 1.15, color),
            );
            seg(painter, pt(r, 0.3, 0.42), pt(r, 0.58, 0.42), st);
            seg(painter, pt(r, 0.44, 0.28), pt(r, 0.44, 0.56), st);
        }
        Glyph::Crop => {
            seg(painter, pt(r, 0.22, 0.0), pt(r, 0.22, 0.78), st);
            seg(painter, pt(r, 0.22, 0.78), pt(r, 1.0, 0.78), st);
            seg(painter, pt(r, 0.0, 0.22), pt(r, 0.78, 0.22), st);
            seg(painter, pt(r, 0.78, 0.22), pt(r, 0.78, 1.0), st);
        }
        Glyph::Compare => {
            let body = scaled(r, 0.08, 0.18, 0.92, 0.82);
            painter.rect_stroke(body, 0, st, StrokeKind::Inside);
            painter.rect_filled(scaled(r, 0.08, 0.18, 0.5, 0.82), 0, color);
            seg(painter, pt(r, 0.5, 0.1), pt(r, 0.5, 0.9), st);
        }
        Glyph::Grid => {
            let body = scaled(r, 0.08, 0.08, 0.92, 0.92);
            painter.rect_stroke(body, 0, st, StrokeKind::Inside);
            for k in 1..3 {
                let t = k as f32 / 3.0;
                seg(
                    painter,
                    pt(r, t, 0.08),
                    pt(r, t, 0.92),
                    Stroke::new(sw * 0.7, color),
                );
                seg(
                    painter,
                    pt(r, 0.08, t),
                    pt(r, 0.92, t),
                    Stroke::new(sw * 0.7, color),
                );
            }
        }
        Glyph::Play => {
            let tri = vec![pt(r, 0.28, 0.14), pt(r, 0.28, 0.86), pt(r, 0.84, 0.5)];
            painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
        }
        Glyph::Pause => {
            painter.rect_filled(scaled(r, 0.28, 0.16, 0.43, 0.84), 0, color);
            painter.rect_filled(scaled(r, 0.57, 0.16, 0.72, 0.84), 0, color);
        }
        Glyph::Prev => {
            painter.rect_filled(scaled(r, 0.18, 0.18, 0.26, 0.82), 0, color);
            let tri = vec![pt(r, 0.82, 0.14), pt(r, 0.82, 0.86), pt(r, 0.3, 0.5)];
            painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
        }
        Glyph::Next => {
            painter.rect_filled(scaled(r, 0.74, 0.18, 0.82, 0.82), 0, color);
            let tri = vec![pt(r, 0.18, 0.14), pt(r, 0.18, 0.86), pt(r, 0.7, 0.5)];
            painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
        }
        Glyph::Folder => {
            painter.rect_stroke(scaled(r, 0.1, 0.26, 0.9, 0.82), 1, st, StrokeKind::Inside);
            seg(painter, pt(r, 0.1, 0.26), pt(r, 0.1, 0.16), st);
            seg(painter, pt(r, 0.1, 0.16), pt(r, 0.42, 0.16), st);
            seg(painter, pt(r, 0.42, 0.16), pt(r, 0.5, 0.26), st);
        }
        Glyph::Plus => {
            seg(painter, pt(r, 0.5, 0.18), pt(r, 0.5, 0.82), st);
            seg(painter, pt(r, 0.18, 0.5), pt(r, 0.82, 0.5), st);
        }
        Glyph::Minus => {
            seg(painter, pt(r, 0.18, 0.5), pt(r, 0.82, 0.5), st);
        }
        Glyph::ChevronDown => {
            seg(painter, pt(r, 0.22, 0.38), pt(r, 0.5, 0.66), st);
            seg(painter, pt(r, 0.5, 0.66), pt(r, 0.78, 0.38), st);
        }
        Glyph::ChevronRight => {
            seg(painter, pt(r, 0.38, 0.2), pt(r, 0.66, 0.5), st);
            seg(painter, pt(r, 0.66, 0.5), pt(r, 0.38, 0.8), st);
        }
        Glyph::Star | Glyph::StarEmpty => {
            let filled = glyph == Glyph::Star;
            let mut pts = star_points(r);
            if filled {
                // 五角星是凹多边形，convex_polygon 会画坏；用中心扇形网格填。
                let mut mesh = egui::Mesh::default();
                mesh.colored_vertex(r.center(), color);
                for p in &pts {
                    mesh.colored_vertex(*p, color);
                }
                let n = pts.len() as u32;
                for i in 0..n {
                    mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
                }
                painter.add(Shape::mesh(mesh));
            } else {
                pts.push(pts[0]);
                painter.add(Shape::line(pts, Stroke::new(sw * 0.9, color)));
            }
        }
        Glyph::Flag => {
            seg(painter, pt(r, 0.26, 0.08), pt(r, 0.26, 0.94), st);
            let flag = vec![pt(r, 0.26, 0.12), pt(r, 0.84, 0.3), pt(r, 0.26, 0.5)];
            painter.add(Shape::convex_polygon(flag, color, Stroke::NONE));
        }
        Glyph::Eye => {
            let mut pts = Vec::new();
            for i in 0..=16 {
                let t = -1.0 + i as f32 / 8.0;
                pts.push(pt(r, 0.5 + t * 0.4, 0.5 - 0.24 * (1.0 - t * t)));
            }
            for i in (0..=16).rev() {
                let t = -1.0 + i as f32 / 8.0;
                pts.push(pt(r, 0.5 + t * 0.4, 0.5 + 0.24 * (1.0 - t * t)));
            }
            pts.push(pts[0]);
            painter.add(Shape::line(pts, st));
            painter.circle_filled(pt(r, 0.5, 0.5), s * 0.1, color);
        }
        Glyph::Merge => {
            // 三张错位叠放的层片 + 汇流箭头：合成模块（HDR/全景/堆栈/时间合成/
            // 焦点合成）的语义就是「多层进、一张出」。
            //
            // Three offset stacked sheets funneling into one: the Merge module's
            // semantics (HDR/panorama/stack/time-blend/focus) are "layers in, one out".
            for (i, top) in [0.16f32, 0.34].iter().enumerate() {
                let inset = 0.10 + i as f32 * 0.10;
                painter.rect_stroke(
                    scaled(r, 0.18 + inset * 0.6, *top, 0.82 - inset * 0.2, *top + 0.22),
                    1,
                    Stroke::new(
                        sw * 0.8,
                        color.gamma_multiply(if i == 0 { 0.55 } else { 0.8 }),
                    ),
                    StrokeKind::Inside,
                );
            }
            painter.rect_stroke(scaled(r, 0.18, 0.54, 0.82, 0.78), 1, st, StrokeKind::Inside);
            let tri = vec![pt(r, 0.44, 0.82), pt(r, 0.56, 0.82), pt(r, 0.5, 0.94)];
            painter.add(Shape::convex_polygon(tri, color, Stroke::NONE));
        }
        Glyph::Histogram => {
            let bars = [0.34f32, 0.62, 0.9, 0.55, 0.28];
            for (i, h) in bars.iter().enumerate() {
                let x = 0.14 + i as f32 * 0.16;
                let bar = Rect::from_min_max(pt(r, x, 0.88 - h * 0.76), pt(r, x + 0.09, 0.88));
                painter.rect_filled(bar, 0, color);
            }
        }
        Glyph::Sliders => {
            for (y, x) in [(0.24, 0.34), (0.5, 0.66), (0.76, 0.44)] {
                seg(
                    painter,
                    pt(r, 0.1, y),
                    pt(r, 0.9, y),
                    Stroke::new(sw * 0.85, color),
                );
                painter.circle_filled(pt(r, x, y), sw * 1.5, color);
            }
        }
        Glyph::Gear => {
            // 齿圈：8 枚短齿（径向线段）+ 中孔圆。小尺寸下比真齿轮轮廓可读，
            // 悬停放大到 24px 时齿距仍然分得开。
            let c = pt(r, 0.5, 0.5);
            let r_out = s * 0.40;
            let r_in = s * 0.28;
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::TAU / 8.0 + std::f32::consts::PI / 8.0;
                let (dx, dy) = (a.sin(), -a.cos());
                seg(
                    painter,
                    Pos2::new(c.x + dx * r_in, c.y + dy * r_in),
                    Pos2::new(c.x + dx * r_out, c.y + dy * r_out),
                    Stroke::new(sw * 0.9, color),
                );
            }
            painter.circle_stroke(c, s * 0.17, st);
        }
        Glyph::Info => {
            painter.circle_stroke(pt(r, 0.5, 0.5), s * 0.4, st);
            painter.circle_filled(pt(r, 0.5, 0.31), sw * 0.9, color);
            seg(painter, pt(r, 0.5, 0.45), pt(r, 0.5, 0.72), st);
        }
        Glyph::Key => {
            for (x, y) in [(0.3, 0.28), (0.7, 0.72)] {
                let c = pt(r, x, y);
                let d = s * 0.15;
                painter.add(Shape::convex_polygon(
                    vec![
                        Pos2::new(c.x, c.y - d),
                        Pos2::new(c.x + d, c.y),
                        Pos2::new(c.x, c.y + d),
                        Pos2::new(c.x - d, c.y),
                    ],
                    color,
                    Stroke::NONE,
                ));
            }
            seg(
                painter,
                pt(r, 0.4, 0.38),
                pt(r, 0.6, 0.62),
                Stroke::new(sw * 0.8, color),
            );
        }
        Glyph::Reset => {
            let c = pt(r, 0.5, 0.52);
            let rad = s * 0.32;
            let mut pts = Vec::new();
            for i in 0..=20 {
                let a = -0.35 + i as f32 / 20.0 * std::f32::consts::TAU * 0.82;
                pts.push(Pos2::new(c.x + rad * a.cos(), c.y + rad * a.sin()));
            }
            let head = pts[0];
            painter.add(Shape::line(pts, Stroke::new(sw, color)));
            painter.add(Shape::convex_polygon(
                vec![
                    Pos2::new(head.x - sw * 1.6, head.y - sw * 2.2),
                    Pos2::new(head.x + sw * 1.6, head.y - sw * 0.4),
                    Pos2::new(head.x - sw * 1.2, head.y + sw * 0.9),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Glyph::History => {
            // macOS 的「时间 + 回头」：几乎一整个表盘，缺口在左上，箭头逆时针指回去，
            // 里面两根指针。和 Reset 的区别就是这两根指针——那是历史，不是归零。
            let c = pt(r, 0.5, 0.52);
            let rad = s * 0.32;
            let head_a = -2.4f32;
            let mut pts = Vec::new();
            for i in 0..=20 {
                let a = head_a + i as f32 / 20.0 * std::f32::consts::TAU * 0.85;
                pts.push(Pos2::new(c.x + rad * a.cos(), c.y + rad * a.sin()));
            }
            let head = pts[0];
            painter.add(Shape::line(pts, Stroke::new(sw, color)));
            // 逆时针方向（屏幕坐标 y 向下）在 head_a 处的切线，和它的法线。
            let dir = Vec2::new(head_a.sin(), -head_a.cos());
            let nrm = Vec2::new(-dir.y, dir.x);
            let tip = head + dir * sw * 2.6;
            painter.add(Shape::convex_polygon(
                vec![tip, head + nrm * sw * 1.7, head - nrm * sw * 1.7],
                color,
                Stroke::NONE,
            ));
            seg(
                painter,
                c,
                Pos2::new(c.x, c.y - rad * 0.56),
                Stroke::new(sw, color),
            );
            seg(
                painter,
                c,
                Pos2::new(c.x + rad * 0.44, c.y + rad * 0.16),
                Stroke::new(sw, color),
            );
        }
        Glyph::Close => {
            seg(painter, pt(r, 0.24, 0.24), pt(r, 0.76, 0.76), st);
            seg(painter, pt(r, 0.76, 0.24), pt(r, 0.24, 0.76), st);
        }
        Glyph::Export => {
            seg(painter, pt(r, 0.5, 0.12), pt(r, 0.5, 0.62), st);
            seg(painter, pt(r, 0.28, 0.36), pt(r, 0.5, 0.14), st);
            seg(painter, pt(r, 0.72, 0.36), pt(r, 0.5, 0.14), st);
            let tray = vec![
                pt(r, 0.12, 0.58),
                pt(r, 0.12, 0.88),
                pt(r, 0.88, 0.88),
                pt(r, 0.88, 0.58),
            ];
            painter.add(Shape::line(tray, st));
        }
        Glyph::Help => {
            painter.circle_stroke(pt(r, 0.5, 0.5), s * 0.4, st);
            painter.add(Shape::line(
                vec![
                    pt(r, 0.36, 0.36),
                    pt(r, 0.42, 0.24),
                    pt(r, 0.58, 0.22),
                    pt(r, 0.66, 0.34),
                    pt(r, 0.6, 0.48),
                    pt(r, 0.5, 0.56),
                ],
                st,
            ));
            painter.circle_filled(pt(r, 0.5, 0.74), sw * 0.9, color);
        }
    }
}
