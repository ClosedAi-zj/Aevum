//! 显影面板里的重型控件：色相带、分级色轮、色调曲线、色阶条。
//!
//! 全部矢量自绘，不用贴图：自检会在同一个进程里开两个 `Context`，
//! 跨 Context 缓存纹理会串台；矢量图元每帧重画，代价可以忽略。

use aevum_core::grade;
use aevum_core::retime::SpeedKey;
use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

use crate::theme::{self, *};

/// 8 个色相带的中心色相，与 `develop::lut::HSL_CENTERS` 同一张表。
pub const BAND_HUES: [f64; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 285.0, 330.0];
/// 色相带名字，沿用 Python 面板那套叫法。
pub const BAND_NAMES: [&str; 8] = ["红", "橙", "黄", "绿", "浅绿", "蓝", "紫", "洋红"];

/// 色阶里灰系数的对数轴上限：0.2~5.0 在对数轴上正好对称。
const GAMMA_HI: f64 = 5.0;
/// 实心控件的那圈极淡内描边，与窗口红绿灯、滑杆推子同一材质口径。
const RIM: Color32 = Color32::from_rgba_unmultiplied_const(0, 0, 0, 46);

/// 色相角 -> 纯色（HSV 满饱和满亮），只用于画示色。
pub fn hue_color(h: f64) -> Color32 {
    let x = h.rem_euclid(360.0) / 60.0;
    let i = x.floor() as usize % 6;
    let f = x - x.floor();
    let (r, g, b) = match i {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    };
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

pub(crate) fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    Color32::from_rgb(
        (a.r() as f32 * (1.0 - t) + b.r() as f32 * t) as u8,
        (a.g() as f32 * (1.0 - t) + b.g() as f32 * t) as u8,
        (a.b() as f32 * (1.0 - t) + b.b() as f32 * t) as u8,
    )
}

/// 灰系数 -> 把手位置（对数轴），与 Python 色阶画布同一口径。
fn gamma_pos(g: f64, span: f32) -> f32 {
    let t = 0.5 + g.max(1e-3).log2() / (2.0 * GAMMA_HI.log2());
    span * t.clamp(0.0, 1.0) as f32
}

fn gamma_at(x: f32, span: f32) -> f64 {
    let t = (x / span.max(1e-3)) as f64;
    (2.0f64)
        .powf((t - 0.5) * 2.0 * GAMMA_HI.log2())
        .clamp(1.0 / GAMMA_HI, GAMMA_HI)
}

// ---------------------------------------------------------------- 色相带选择条

/// 8 格色相带：色块本身就是那一条的色相，改过的带右上角点一颗强调点。
/// 返回是否换了选中带。
pub fn band_chips(ui: &mut Ui, sel: &mut usize, used: &[f64]) -> bool {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::hover());
    let cw = (rect.width() / BAND_NAMES.len() as f32).floor();
    let mut changed = false;
    for i in 0..BAND_NAMES.len() {
        let cell = Rect::from_min_size(
            Pos2::new(rect.left() + i as f32 * cw, rect.top() + 1.0),
            Vec2::new((cw - 2.0).max(8.0), 17.0),
        );
        let resp = ui.interact(
            cell,
            ui.make_persistent_id(("band-chip", i)),
            Sense::click(),
        );
        let p = ui.painter();
        let hue = hue_color(BAND_HUES[i]);
        let on = *sel == i;
        p.rect_filled(
            cell,
            radius_control(),
            if on {
                hue
            } else if resp.hovered() {
                hue.gamma_multiply(0.75)
            } else {
                hue.gamma_multiply(0.5)
            },
        );
        if on {
            p.rect_stroke(
                cell,
                radius_control(),
                Stroke::new(1.5, text()),
                StrokeKind::Inside,
            );
        }
        p.text(
            cell.center(),
            Align2::CENTER_CENTER,
            BAND_NAMES[i],
            font(SIZE_SMALL),
            if on {
                Color32::from_rgb(0x14, 0x14, 0x16)
            } else {
                text()
            },
        );
        if used.get(i).copied().unwrap_or(0.0) != 0.0 {
            p.circle_filled(cell.right_top() + Vec2::new(-4.0, 4.0), 2.2, accent());
        }
        if resp.clicked() {
            *sel = i;
            changed = true;
        }
    }
    changed
}

// -------------------------------------------------------------------- 分级色轮

/// 分级调色的一枚色轮：外环选色相，内盘按角度选色相、按半径选浓度，底下一轨选亮度。
///
/// 内盘是矢量楔形铺出来的（36 段 × 6 圈），不用贴图：满幅渐变盘要么吃一张纹理，
/// 要么靠 shader，两条路都不值得；而中性灰底盘又读不出「往哪边走是什么颜色」。
pub fn grade_wheel(
    ui: &mut Ui,
    id: &'static str,
    label: &str,
    hue: &mut f64,
    amount: &mut f64,
    lift: &mut f64,
    size: f32,
) -> bool {
    let head = 14.0;
    let rail_h = 15.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(size, head + size + rail_h), Sense::hover());
    let c = Pos2::new(rect.center().x, rect.top() + head + size * 0.5);
    let r_out = size * 0.5 - 1.0;
    let ring_w = 9.0;
    let r_in = r_out - ring_w;
    let r_disc = (r_in - 2.0).max(6.0);
    let active = *amount != 0.0 || *lift != 0.0 || *hue != 0.0;
    let p = ui.painter();
    p.text(
        Pos2::new(rect.left(), rect.top() + 6.0),
        Align2::LEFT_CENTER,
        label,
        font(SIZE_SMALL),
        if active { text() } else { muted() },
    );

    let wheel_rect = Rect::from_center_size(c, Vec2::splat(size));
    let wresp = ui.interact(
        wheel_rect,
        ui.make_persistent_id(("wheel", id)),
        Sense::click_and_drag(),
    );
    let rail_y = rect.bottom() - rail_h * 0.5;
    let rail_rect = Rect::from_min_max(
        Pos2::new(rect.left(), rail_y - 6.0),
        Pos2::new(rect.right(), rail_y + 6.0),
    );
    let rresp = ui.interact(
        rail_rect,
        ui.make_persistent_id(("wheel-rail", id)),
        Sense::click_and_drag(),
    );

    let mut changed = false;
    if let Some(pt) = wresp.interact_pointer_pos().filter(|_| wresp.dragged()) {
        let d = pt - c;
        let r = (d.x * d.x + d.y * d.y).sqrt();
        // 屏幕 y 向下，取负让色相逆时针增长。
        let ang = (d.y as f64)
            .atan2(-(d.x as f64))
            .to_degrees()
            .rem_euclid(360.0);
        if r <= r_disc + 4.0 {
            *hue = ang;
            *amount = (r / r_disc.max(1.0)).clamp(0.0, 1.0) as f64 * 100.0;
        } else if r <= r_out + 4.0 {
            *hue = ang;
        }
        changed = true;
    }
    if wresp.double_clicked() && active {
        *hue = 0.0;
        *amount = 0.0;
        *lift = 0.0;
        changed = true;
    }
    if let Some(pt) = rresp.interact_pointer_pos().filter(|_| rresp.dragged()) {
        let half = (rail_rect.width() * 0.5).max(1.0);
        let v = ((pt.x - rail_rect.center().x) as f64 / half as f64 * 100.0).clamp(-100.0, 100.0);
        if (*lift - v).abs() > 1e-9 {
            *lift = v;
            changed = true;
        }
    }
    if rresp.double_clicked() && *lift != 0.0 {
        *lift = 0.0;
        changed = true;
    }
    if wresp.hovered() || rresp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    // 外环：48 段纯色楔形。
    let tau = std::f64::consts::TAU;
    let seg = 48usize;
    let ring_point =
        |r: f32, a: f64| Pos2::new(c.x + r * (a as f32).cos(), c.y - r * (a as f32).sin());
    for k in 0..seg {
        let a0 = k as f64 * tau / seg as f64;
        let a1 = (k + 1) as f64 * tau / seg as f64;
        let deg = (a0 + a1) * 0.5 * 180.0 / std::f64::consts::PI;
        p.add(Shape::convex_polygon(
            vec![
                ring_point(r_in, a0),
                ring_point(r_out, a0),
                ring_point(r_out, a1),
                ring_point(r_in, a1),
            ],
            hue_color(deg),
            Stroke::NONE,
        ));
    }
    // 内盘：PS Camera Raw 的读法——白心柔和渐变到边缘色相，饱和度压到六成。
    // 之前满饱和的彩虹盘三枚并排互相打架，看着又挤又闹；柔和盘才衬得出行
    // 与行之间的留白。亮度轨仍然改这层底灰的明暗，提亮压暗在盘上看得见。
    let neutral = 128.0f32 * (1.0 + *lift as f32 / 100.0 * 0.45).clamp(0.45, 1.6);
    let base = Color32::from_gray(neutral.clamp(1.0, 254.0) as u8);
    let bands = 6usize;
    let disc_seg = 36usize;
    for b in 0..bands {
        let r0 = (r_disc * b as f32 / bands as f32).max(1.0);
        let r1 = r_disc * (b + 1) as f32 / bands as f32;
        let sat = (b + 1) as f32 / bands as f32 * 0.62;
        for k in 0..disc_seg {
            let a0 = k as f64 * tau / disc_seg as f64;
            let a1 = (k + 1) as f64 * tau / disc_seg as f64;
            let deg = (a0 + a1) * 0.5 * 180.0 / std::f64::consts::PI;
            p.add(Shape::convex_polygon(
                vec![
                    ring_point(r0, a0),
                    ring_point(r1, a0),
                    ring_point(r1, a1),
                    ring_point(r0, a1),
                ],
                mix(base, hue_color(deg), sat),
                Stroke::NONE,
            ));
        }
    }
    p.circle_stroke(c, r_disc, Stroke::new(1.0, RIM));
    // 环上的推子定色相，盘上的推子同时读出浓度。
    let ring_k = ring_point(r_in + ring_w * 0.5, *hue * std::f64::consts::PI / 180.0);
    p.circle_filled(
        ring_k + Vec2::new(0.0, 1.0),
        5.0,
        Color32::from_black_alpha(70),
    );
    p.circle_filled(ring_k, 5.0, Color32::WHITE);
    p.circle_stroke(ring_k, 5.0, Stroke::new(1.0, RIM));
    if *amount != 0.0 {
        let rr = r_disc * *amount as f32 / 100.0;
        let k = ring_point(rr, *hue * std::f64::consts::PI / 180.0);
        p.circle_stroke(k, 4.0, Stroke::new(1.5, text()));
    }
    // 亮度轨：中点 = 0，左右各 100。
    let rail = Rect::from_min_max(
        Pos2::new(rail_rect.left(), rail_y - 1.5),
        Pos2::new(rail_rect.right(), rail_y + 1.5),
    );
    p.rect_filled(rail, 1.5, rail_fill());
    let half = rail_rect.width() * 0.5;
    let kx = rail_rect.center().x + half * (*lift as f32 / 100.0);
    let cx = rail_rect.center().x;
    if (kx - cx).abs() > 0.5 {
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(kx.min(cx), rail_y - 1.5),
                Pos2::new(kx.max(cx), rail_y + 1.5),
            ),
            1.5,
            accent(),
        );
    }
    p.circle_filled(
        Pos2::new(kx, rail_y + 1.0),
        4.0,
        Color32::from_black_alpha(70),
    );
    p.circle_filled(Pos2::new(kx, rail_y), 4.0, Color32::WHITE);
    p.circle_stroke(Pos2::new(kx, rail_y), 4.0, Stroke::new(1.0, RIM));
    changed
}

// -------------------------------------------------------------------- 色调曲线

/// 曲线 PCHIP 插值表的缓存（性能重构设计 §3 第 2 层）：
/// 指纹命中就跳过 `grade::pchip_lut` 的 129 点重算。自校验——拖动改了
/// `pts` 指纹必变，天然失效，不依赖手动清理。
pub struct CurveLutCache {
    /// 参数指纹（逐点 x/y 的 f64 位型哈希）。
    pub(crate) fp: u64,
    /// 129 点插值表，与 `grade::pchip_lut(pts, CURVE_LUT_N)` 逐位一致。
    pub(crate) lut: Vec<f32>,
}

/// LUT 采样数：与渲染同一张表，历史口径就是 129。
const CURVE_LUT_N: usize = 129;

/// 当前通道控制点的指纹：f64 一律 `to_bits()` 进 hasher（禁 `to_string()`）。
pub(crate) fn curve_lut_fingerprint(pts: &[[f64; 2]]) -> u64 {
    // 与蒙版指纹同一套 fx 风格 hasher（见 shell::FpHasher 的理由）。
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mix = |h: &mut u64, v: u64| {
        *h = (h.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    };
    mix(&mut h, pts.len() as u64);
    for p in pts {
        mix(&mut h, p[0].to_bits());
        mix(&mut h, p[1].to_bits());
    }
    h
}

/// 色调曲线编辑框：横轴输入、纵轴输出，都在 0..255。
///
/// 拖动只改纵坐标，点空白加一个点，双击中间点删掉，双击空白整条复位。
/// `grab` 是外存的抓取序号：拖动中点在指针下走，靠它才不会跳。
/// `cache` 挂在 AppState 上跨帧存活：指纹命中跳过插值表重算。
#[allow(clippy::too_many_arguments)]
pub fn curve_box(
    ui: &mut Ui,
    id: &'static str,
    label: &str,
    tint: Color32,
    pts: &mut Vec<[f64; 2]>,
    side: f32,
    grab: &mut Option<usize>,
    cache: &mut Option<CurveLutCache>,
) -> bool {
    let head = 15.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(side, head + side), Sense::hover());
    let inner = Rect::from_min_size(Pos2::new(rect.left(), rect.top() + head), Vec2::splat(side));
    let p = ui.painter();
    p.text(
        Pos2::new(inner.left(), rect.top() + 6.0),
        Align2::LEFT_CENTER,
        label,
        font(SIZE_SMALL),
        text(),
    );
    p.rect_filled(inner, radius_control(), field());
    p.rect_stroke(
        inner,
        radius_control(),
        Stroke::new(1.0, RIM),
        StrokeKind::Inside,
    );
    let sx = inner.width() / 255.0;
    let sy = inner.height() / 255.0;
    let to_px =
        |x: f64, y: f64| Pos2::new(inner.left() + x as f32 * sx, inner.bottom() - y as f32 * sy);

    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment(
            [
                Pos2::new(inner.left() + inner.width() * f, inner.top() + 1.0),
                Pos2::new(inner.left() + inner.width() * f, inner.bottom() - 1.0),
            ],
            Stroke::new(1.0, divider()),
        );
        p.line_segment(
            [
                Pos2::new(inner.left() + 1.0, inner.top() + inner.height() * f),
                Pos2::new(inner.right() - 1.0, inner.top() + inner.height() * f),
            ],
            Stroke::new(1.0, divider()),
        );
    }
    p.line_segment(
        [to_px(0.0, 0.0), to_px(255.0, 255.0)],
        Stroke::new(1.0, border_strong()),
    );

    let resp = ui.interact(
        inner,
        ui.make_persistent_id(("curve", id)),
        Sense::click_and_drag(),
    );
    let mut changed = false;
    // 命中检测单独收一个 pts 参数：闭包若直接抓住 pts，拖动时就再也改不动它。
    let nearest = |q: &[[f64; 2]], pos: Pos2| -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, p) in q.iter().enumerate() {
            let d = (to_px(p[0], p[1]) - pos).length();
            if d < 9.0 && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    };

    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.dragged() {
            if grab.is_none() {
                *grab = nearest(pts, pos);
            }
            if let Some(i) = *grab
                && let Some(q) = pts.get_mut(i)
            {
                let y = ((inner.bottom() - pos.y) as f64 / sy as f64).clamp(0.0, 255.0);
                if (q[1] - y).abs() > 1e-9 {
                    q[1] = y;
                    changed = true;
                }
            }
        } else if resp.clicked_by(egui::PointerButton::Primary) && nearest(pts, pos).is_none() {
            let x = ((pos.x - inner.left()) as f64 / sx as f64)
                .clamp(1.0, 254.0)
                .round();
            if !pts.iter().any(|q| (q[0] - x).abs() < 0.5) {
                let y = ((inner.bottom() - pos.y) as f64 / sy as f64)
                    .clamp(0.0, 255.0)
                    .round();
                pts.push([x, y]);
                pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
                *grab = pts.iter().position(|q| q[0] == x);
                changed = true;
            }
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        *grab = None;
    }
    if resp.double_clicked() {
        let pos = ui.input(|i| i.pointer.hover_pos());
        let hit = pos.and_then(|p| nearest(pts, p));
        match hit {
            // 首尾两点不许删，否则曲线没有端点。
            Some(i) if i != 0 && i + 1 != pts.len() && pts.len() > 2 => {
                pts.remove(i);
                changed = true;
            }
            None if pts.len() > 2 => {
                pts.retain(|q| q[0] == 0.0 || q[0] == 255.0);
                changed = true;
            }
            _ => {}
        }
    }
    if resp.hovered() {
        let hover = ui.input(|i| i.pointer.hover_pos().unwrap_or(Pos2::ZERO));
        if nearest(pts, hover).is_some() || grab.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
    }

    // 曲线本体：与渲染同一张 pchip 插值表，画的就是实际生效的那条。
    // LUT 缓存命中直接复用；指纹没变就不重算（E4）。
    let fp = curve_lut_fingerprint(pts);
    let cache_hit = cache.as_ref().is_some_and(|c| c.fp == fp);
    if !cache_hit {
        *cache = Some(CurveLutCache {
            fp,
            lut: grade::pchip_lut(pts, CURVE_LUT_N),
        });
    }
    let lut = &cache.as_ref().expect("刚写入必有值").lut;
    let line: Vec<Pos2> = lut
        .iter()
        .enumerate()
        .map(|(k, v)| to_px(k as f64 * 2.0, *v as f64))
        .collect();
    p.add(Shape::line(line, Stroke::new(1.4, tint)));
    for (i, q) in pts.iter().enumerate() {
        let c = to_px(q[0], q[1]);
        let editable = i != 0 && i + 1 != pts.len();
        let held = *grab == Some(i);
        p.circle_filled(
            c,
            if held { 4.5 } else { 3.6 },
            if editable { accent() } else { field() },
        );
        p.circle_stroke(
            c,
            if held { 4.5 } else { 3.6 },
            Stroke::new(1.2, if editable { text() } else { accent() }),
        );
    }
    changed
}

// ---------------------------------------------------------------- 速度曲线框

/// 倍速的编辑域：对数轴 0.0625×~16×。取互为倒数两端，几何中点正好是 1×，
/// 那条「原样」基准线才落在框的正中（0.05 会让 1× 偏到下边）。
const SPEED_LO: f64 = 0.0625;
const SPEED_HI: f64 = 16.0;

/// 倍速 -> 框内纵向像素（0 在顶、`side` 在底）。对数轴，1× 落在正中。
fn speed_pos(speed: f64, side: f32) -> f32 {
    let t = (speed.clamp(SPEED_LO, SPEED_HI).ln() - SPEED_LO.ln()) / (SPEED_HI / SPEED_LO).ln();
    (1.0 - t as f32) * side
}

/// `speed_pos` 的逆：像素读数夹回编辑域，所以拖出框外不会算出 0 或负倍速。
fn speed_at_pos(y: f32, side: f32) -> f64 {
    let t = ((side - y) as f64 / side as f64).clamp(0.0, 1.0);
    (SPEED_LO * (SPEED_HI / SPEED_LO).powf(t)).clamp(SPEED_LO, SPEED_HI)
}

/// 速度曲线框：横轴是**源进度**（0..1），纵轴是倍速（对数）。
///
/// 画的是 `retime::speed_at` 那条折线本身——键之间线性，所以线也直，
/// 不像色调曲线那样要套 pchip；面板上看到的就是导出实际算的那条。
/// 拖动：首尾两点只动倍速（它们钉住曲线两端），中间两点横竖都能拖；
/// 空白处单击加一个点，双击点删掉它，双击空白收回到只剩首尾。
pub fn speed_box(
    ui: &mut Ui,
    id: &'static str,
    keys: &mut Vec<SpeedKey>,
    side: f32,
    grab: &mut Option<usize>,
) -> bool {
    let head = 15.0;
    let foot = 14.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(side, head + side + foot), Sense::hover());
    let inner = Rect::from_min_size(Pos2::new(rect.left(), rect.top() + head), Vec2::splat(side));
    let p = ui.painter();
    p.text(
        Pos2::new(inner.left(), rect.top() + 6.0),
        Align2::LEFT_CENTER,
        "速度曲线（横轴＝序列进度）",
        font(SIZE_SMALL),
        text(),
    );
    p.rect_filled(inner, radius_control(), field());
    p.rect_stroke(
        inner,
        radius_control(),
        Stroke::new(1.0, RIM),
        StrokeKind::Inside,
    );
    let to_px = |at: f64, speed: f64| {
        Pos2::new(
            inner.left() + at as f32 * inner.width(),
            inner.top() + speed_pos(speed, inner.height()),
        )
    };
    let speed_at_y = |y: f32| speed_at_pos(y - inner.top(), inner.height());

    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment(
            [
                Pos2::new(inner.left() + inner.width() * f, inner.top() + 1.0),
                Pos2::new(inner.left() + inner.width() * f, inner.bottom() - 1.0),
            ],
            Stroke::new(1.0, divider()),
        );
    }
    for speed in [0.125, 0.25, 0.5, 2.0, 4.0, 8.0] {
        let y = to_px(0.0, speed).y;
        p.line_segment(
            [
                Pos2::new(inner.left() + 1.0, y),
                Pos2::new(inner.right() - 1.0, y),
            ],
            Stroke::new(1.0, divider()),
        );
    }
    // 1× 是「原样」那条基准，画得比网格重一档。
    let flat = to_px(0.0, 1.0).y;
    p.line_segment(
        [
            Pos2::new(inner.left() + 1.0, flat),
            Pos2::new(inner.right() - 1.0, flat),
        ],
        Stroke::new(1.0, border_strong()),
    );
    for (at, text) in [(0.0, "头"), (0.5, "中"), (1.0, "尾")] {
        p.text(
            Pos2::new(
                inner.left() + (at as f32) * inner.width(),
                inner.bottom() + 3.0,
            ),
            Align2::CENTER_TOP,
            text,
            font(SIZE_SMALL),
            muted(),
        );
    }
    p.text(
        Pos2::new(inner.right() - 2.0, to_px(0.0, 8.0).y),
        Align2::RIGHT_TOP,
        "8×",
        font(SIZE_SMALL),
        muted(),
    );
    p.text(
        Pos2::new(inner.right() - 2.0, to_px(0.0, 0.125).y),
        Align2::RIGHT_BOTTOM,
        "0.125×",
        font(SIZE_SMALL),
        muted(),
    );

    let resp = ui.interact(
        inner,
        ui.make_persistent_id(("speed", id)),
        Sense::click_and_drag(),
    );
    let mut changed = false;
    let nearest = |q: &[SpeedKey], pos: Pos2| -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, k) in q.iter().enumerate() {
            let d = (to_px(k.at, k.speed) - pos).length();
            if d < 9.0 && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    };

    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.dragged() {
            if grab.is_none() {
                *grab = nearest(keys, pos);
            }
            if let Some(i) = *grab
                && i < keys.len()
            {
                // 首尾只动倍速：它们的横坐标钉住曲线两端，中间点才允许挪时间。
                // 邻居的横坐标要在拿 &mut 之前读出来，否则同一段切片 borrow 两次。
                let ends = i == 0 || i + 1 == keys.len();
                let lo = if ends { 0.0 } else { keys[i - 1].at + 0.01 };
                let hi = if ends { 1.0 } else { keys[i + 1].at - 0.01 };
                let k = &mut keys[i];
                let speed = speed_at_y(pos.y);
                let at = if ends {
                    k.at
                } else {
                    let want = (pos.x - inner.left()) as f64 / inner.width() as f64;
                    want.clamp(lo.max(0.0), hi.min(1.0))
                };
                if (k.speed - speed).abs() > 1e-9 || (k.at - at).abs() > 1e-9 {
                    k.speed = speed;
                    k.at = at;
                    changed = true;
                }
            }
        } else if resp.clicked_by(egui::PointerButton::Primary) && nearest(keys, pos).is_none() {
            let at = ((pos.x - inner.left()) as f64 / inner.width() as f64).clamp(0.02, 0.98);
            if !keys.iter().any(|k| (k.at - at).abs() < 0.02) {
                keys.push(SpeedKey {
                    at,
                    speed: speed_at_y(pos.y),
                });
                keys.sort_by(|a, b| a.at.total_cmp(&b.at));
                *grab = keys.iter().position(|k| (k.at - at).abs() < 1e-9);
                changed = true;
            }
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        *grab = None;
    }
    if resp.double_clicked() {
        let pos = ui.input(|i| i.pointer.hover_pos());
        let hit = pos.and_then(|q| nearest(keys, q));
        match hit {
            Some(i) if i != 0 && i + 1 != keys.len() && keys.len() > 2 => {
                keys.remove(i);
                changed = true;
            }
            None if keys.len() > 2 => {
                let first = keys[0];
                let last = keys[keys.len() - 1];
                *keys = vec![first, last];
                changed = true;
            }
            _ => {}
        }
    }
    if resp.hovered() {
        let hover = ui.input(|i| i.pointer.hover_pos().unwrap_or(Pos2::ZERO));
        if nearest(keys, hover).is_some() || grab.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
    }

    let line: Vec<Pos2> = keys.iter().map(|k| to_px(k.at, k.speed)).collect();
    if line.len() > 1 {
        p.add(Shape::line(line, Stroke::new(1.4, accent())));
    }
    for (i, k) in keys.iter().enumerate() {
        let c = to_px(k.at, k.speed);
        let ends = i == 0 || i + 1 == keys.len();
        let held = *grab == Some(i);
        p.circle_filled(
            c,
            if held { 4.5 } else { 3.6 },
            if ends { field() } else { accent() },
        );
        p.circle_stroke(
            c,
            if held { 4.5 } else { 3.6 },
            Stroke::new(1.2, if ends { accent() } else { text() }),
        );
    }
    changed
}

// ---------------------------------------------------------------------- 色阶条
/// 色阶条：直方图剪影在上，上排三个把手夹输入（黑 / 灰 / 白），下排两个夹输出。
///
/// `grab` 是抓中的把手序号（0 输入黑、1 输入灰、2 输入白、3 输出黑、4 输出白）；
/// 黑白两两互留 1 的间距，防止查表除零。
/// 五个把手直接借 `GradeParams` 的色阶字段，拼成结构体反而要在面板和控件之间来回搬，
/// 所以这里放开 `too_many_arguments`。
#[allow(clippy::too_many_arguments)]
pub fn levels_strip(
    ui: &mut Ui,
    hist: Option<&[[u32; 256]; 3]>,
    lb: &mut f64,
    lg: &mut f64,
    lw: &mut f64,
    ob: &mut f64,
    ow: &mut f64,
    grab: &mut Option<usize>,
    height: f32,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let pad = 8.0f32;
    let span = (rect.width() - pad * 2.0).max(1.0);
    let hist_h = (height - 36.0).max(20.0);
    let hist_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), hist_h));
    let y_in = hist_rect.bottom() + 10.0;
    let y_out = rect.bottom() - 5.0;
    let p = ui.painter();
    p.rect_filled(hist_rect, radius_control(), field());
    p.rect_stroke(
        hist_rect,
        radius_control(),
        Stroke::new(1.0, RIM),
        StrokeKind::Inside,
    );

    let at = |v: f64| rect.left() + pad + (v as f32 / 255.0) * span;
    let val = |x: f32| ((x - rect.left() - pad) as f64 / span as f64 * 255.0).clamp(0.0, 255.0);
    let gx = |g: f64| rect.left() + pad + gamma_pos(g, span);
    let g_at = |x: f32| gamma_at(x - rect.left() - pad, span);

    if let Some(h) = hist {
        for (c, band) in h.iter().enumerate() {
            let peak = band.iter().max().copied().unwrap_or(0).max(1) as f32;
            let col = match c {
                0 => Color32::from_rgba_unmultiplied(0xFF, 0x6B, 0x63, 150),
                1 => Color32::from_rgba_unmultiplied(0x5F, 0xD8, 0x7A, 150),
                _ => Color32::from_rgba_unmultiplied(0x64, 0xA8, 0xFF, 150),
            };
            let mut pts = Vec::with_capacity(64);
            for k in 0..64 {
                let i0 = k * 4;
                let mx = band[i0..i0 + 4].iter().max().copied().unwrap_or(0);
                pts.push(Pos2::new(
                    hist_rect.left() + (k as f32 / 63.0) * hist_rect.width(),
                    hist_rect.bottom() - (mx as f32 / peak) * (hist_h - 4.0),
                ));
            }
            p.add(Shape::closed_line(pts, Stroke::new(1.0, col)));
        }
    }

    let resp = ui.interact(
        rect,
        ui.make_persistent_id("levels"),
        Sense::click_and_drag(),
    );
    let mut changed = false;
    let hands = [
        Pos2::new(at(*lb), y_in),
        Pos2::new(gx(*lg), y_in),
        Pos2::new(at(*lw), y_in),
        Pos2::new(at(*ob), y_out),
        Pos2::new(at(*ow), y_out),
    ];
    if let Some(pos) = resp.interact_pointer_pos()
        && resp.dragged()
    {
        if grab.is_none() {
            let mut best: Option<(usize, f32)> = None;
            for (i, hc) in hands.iter().enumerate() {
                let d = (*hc - pos).length();
                if d < 12.0 && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
            *grab = best.map(|(i, _)| i);
        }
        let v = match *grab {
            Some(0) => Some(val(pos.x).min(*lw - 1.0)),
            Some(1) => Some(g_at(pos.x)),
            Some(2) => Some(val(pos.x).max(*lb + 1.0)),
            Some(3) => Some(val(pos.x).min(*ow - 1.0)),
            Some(4) => Some(val(pos.x).max(*ob + 1.0)),
            _ => None,
        };
        if let Some(v) = v {
            let slot = match *grab {
                Some(0) => Some(&mut *lb),
                Some(1) => Some(&mut *lg),
                Some(2) => Some(&mut *lw),
                Some(3) => Some(&mut *ob),
                Some(4) => Some(&mut *ow),
                _ => None,
            };
            if let Some(slot) = slot
                && (*slot - v).abs() > 1e-9
            {
                *slot = v;
                changed = true;
            }
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        *grab = None;
    }
    if resp.hovered() {
        let hover = ui.input(|i| i.pointer.hover_pos().unwrap_or(Pos2::ZERO));
        if grab.is_some() || hands.iter().any(|hc| (*hc - hover).length() < 12.0) {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
    }

    theme::rule(
        p,
        rect.left() + pad,
        rect.right() - pad,
        y_in,
        border_strong(),
    );
    theme::rule(
        p,
        rect.left() + pad,
        rect.right() - pad,
        y_out,
        border_strong(),
    );
    for (i, hc) in hands.iter().enumerate() {
        let hot = *grab == Some(i);
        if hot {
            p.line_segment(
                [
                    Pos2::new(hc.x, hist_rect.top()),
                    Pos2::new(hc.x, hist_rect.bottom()),
                ],
                Stroke::new(1.0, accent().gamma_multiply(0.7)),
            );
        }
        let d = 5.0f32;
        let tri = if i < 3 {
            vec![
                Pos2::new(hc.x - d, hc.y - d),
                Pos2::new(hc.x + d, hc.y - d),
                *hc,
            ]
        } else {
            vec![
                Pos2::new(hc.x - d, hc.y + d),
                Pos2::new(hc.x + d, hc.y + d),
                *hc,
            ]
        };
        p.add(Shape::convex_polygon(
            tri,
            if hot {
                accent()
            } else if i == 1 {
                teal()
            } else {
                text()
            },
            Stroke::NONE,
        ));
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_endpoints_land_on_expected_primaries() {
        assert_eq!(hue_color(0.0), Color32::from_rgb(255, 0, 0));
        assert_eq!(hue_color(120.0), Color32::from_rgb(0, 255, 0));
        assert_eq!(hue_color(240.0), Color32::from_rgb(0, 0, 255));
        // 负角与超过一圈都要绕回来：-120 与 240 同相。
        assert_eq!(hue_color(-120.0), hue_color(240.0));
        assert_eq!(hue_color(480.0), hue_color(120.0));
    }

    #[test]
    fn gamma_axis_is_symmetric_and_round_trips() {
        let span = 300.0f32;
        assert!((gamma_pos(1.0, span) - span * 0.5).abs() < 1e-6);
        assert!((gamma_pos(GAMMA_HI, span) - span).abs() < 1e-3);
        assert!((gamma_at(0.0, span) - 1.0 / GAMMA_HI).abs() < 1e-9);
        assert!((gamma_at(span, span) - GAMMA_HI).abs() < 1e-9);
        for &g in &[0.34, 1.0, 2.7] {
            let x = gamma_pos(g, span);
            // 把手位置是 f32 像素，往返只能夹到像素精度；再紧就是在测四舍五入噪声。
            assert!((gamma_at(x, span) - g).abs() < 1e-5, "{g} 往返不相等");
        }
        // 越界读数被夹住，不会算出负数或无穷。
        assert!(gamma_at(-50.0, span) >= 1.0 / GAMMA_HI - 1e-9);
        assert!(gamma_at(span + 80.0, span) <= GAMMA_HI + 1e-9);
    }

    /// 速度曲线的对数纵轴：1× 必须正好落在框正中，慢放与快进上下对称。
    /// 早先按 0.05~16 取域时几何中点是 0.894×，基准线肉眼看着偏下才发现。
    #[test]
    fn speed_axis_puts_unity_dead_centre() {
        let side = 240.0f32;
        assert!(
            (speed_pos(1.0, side) - side * 0.5).abs() < 1e-4,
            "1× 不在正中：{}",
            speed_pos(1.0, side)
        );
        assert!((speed_pos(SPEED_HI, side) - 0.0).abs() < 1e-4);
        assert!((speed_pos(SPEED_LO, side) - side).abs() < 1e-4);
        for &s in &[0.125, 0.5, 1.0, 3.7, 8.0] {
            let y = speed_pos(s, side);
            assert!((speed_at_pos(y, side) - s).abs() < 1e-6, "{s}× 往返不相等");
            // 互为倒数的两档关于中线对称。
            assert!((speed_pos(s, side) + speed_pos(1.0 / s, side) - side).abs() < 1e-3);
        }
        // 拖出框外夹回编辑域，不会算出 0 或负倍速。
        assert!(speed_at_pos(-80.0, side) <= SPEED_HI + 1e-9);
        assert!(speed_at_pos(side + 80.0, side) >= SPEED_LO - 1e-9);
    }

    #[test]
    fn mix_blends_towards_the_second_color() {
        let a = Color32::from_rgb(0, 0, 0);
        let b = Color32::from_rgb(200, 100, 50);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 2.0), b);
        assert_eq!(mix(a, b, 0.5), Color32::from_rgb(100, 50, 25));
    }

    /// 无显示器把四个重控件各画一遍：形状要够多，坐标要有限，且不能画到框外。
    /// 这几个控件是纯几何（三角函数 + 对数轴 + 插值表），算错就会飘出可视区，
    /// 光看模块级自检的形状总数看不出问题。
    #[test]
    fn heavy_widgets_paint_inside_their_box() {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 1420.0));
        let raw = || egui::RawInput {
            screen_rect: Some(screen),
            max_texture_side: Some(8192),
            ..Default::default()
        };

        let mut hist = [[0u32; 256]; 3];
        for band in hist.iter_mut() {
            for (v, bin) in band.iter_mut().enumerate() {
                *bin = (v as u32 % 37) + 1;
            }
        }

        let mut out = ctx.run_ui(raw(), |ui| {
            let mut sel = 3usize;
            band_chips(ui, &mut sel, &[0.0, 12.0, 0.0, -8.0, 0.0, 20.0, 0.0, 0.0]);

            let (mut hue, mut amount, mut lift) = (210.0, 45.0, -30.0);
            grade_wheel(
                ui,
                "wheel-shadow",
                "阴影",
                &mut hue,
                &mut amount,
                &mut lift,
                92.0,
            );

            let mut pts = vec![[0.0, 0.0], [85.0, 110.0], [180.0, 150.0], [255.0, 255.0]];
            let mut grab = None;
            let mut lut_cache: Option<CurveLutCache> = None;
            curve_box(
                ui,
                "curve-tone",
                "输出 ↑ / 输入 →",
                Color32::WHITE,
                &mut pts,
                240.0,
                &mut grab,
                &mut lut_cache,
            );

            let (mut lb, mut lg, mut lw, mut ob, mut ow) = (12.0, 0.62, 240.0, 0.0, 255.0);
            levels_strip(
                ui,
                Some(&hist),
                &mut lb,
                &mut lg,
                &mut lw,
                &mut ob,
                &mut ow,
                &mut grab,
                78.0,
            );

            // 速度曲线：对数纵轴要跨 0.5×~4×，端点与中间点各画一种材质。
            let mut keys = vec![
                SpeedKey {
                    at: 0.0,
                    speed: 1.0,
                },
                SpeedKey {
                    at: 0.4,
                    speed: 4.0,
                },
                SpeedKey {
                    at: 1.0,
                    speed: 0.5,
                },
            ];
            speed_box(ui, "speed", &mut keys, 240.0, &mut grab);
        });

        // 无头跑完要把纹理增量清掉，否则 epaint 在 Drop 里断言「有未应用的 delta」，
        // 会把测试进程直接 abort 掉。
        out.textures_delta.clear();
        assert!(
            out.shapes.len() > 90,
            "重控件只画出 {} 个形状",
            out.shapes.len()
        );
        for clipped in &out.shapes {
            let b = clipped.shape.visual_bounding_rect();
            assert!(b.is_finite(), "画出了非有限坐标：{b:?}");
            // 夹到 0/255 的曲线把手、贴着边缘的色阶三角本来就压在控件边上，
            // 圆头会外溢半个把手；留 8px 余量，超出这个量才是坐标真的算飞了。
            assert!(screen.expand(8.0).contains_rect(b), "画到了可视框外：{b:?}");
        }
    }
}
