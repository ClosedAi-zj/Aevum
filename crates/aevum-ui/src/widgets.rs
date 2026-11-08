//! 显影面板里的重型控件：色相带、分级色轮、色调曲线、色阶条。
//!
//! 全部矢量自绘，不用贴图：自检会在同一个进程里开两个 `Context`，
//! 跨 Context 缓存纹理会串台；矢量图元每帧重画，代价可以忽略。
//!
//! The heavy controls inside the develop panel: hue bands, grading wheels, tone
//! curve, levels bar.
//!
//! Everything is drawn as vectors, no textures: the selftest opens two `Context`s
//! in one process, and cached textures would bleed across them; vector primitives
//! redrawn every frame cost nothing worth measuring.

use aevum_core::grade;
use aevum_core::retime::SpeedKey;
use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

use crate::theme::{self, *};

/// 8 个色相带的中心色相，与 `develop::lut::HSL_CENTERS` 同一张表。
///
/// Centre hue of each of the 8 bands, the same table as
/// `develop::lut::HSL_CENTERS`.
pub const BAND_HUES: [f64; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 285.0, 330.0];
/// 色相带名字，沿用 Python 面板那套叫法。
///
/// Band names, carrying over the naming used by the Python panel.
pub const BAND_NAMES: [&str; 8] = ["红", "橙", "黄", "绿", "浅绿", "蓝", "紫", "洋红"];

/// 色阶里灰系数的对数轴上限：0.2~5.0 在对数轴上正好对称。
const GAMMA_HI: f64 = 5.0;
/// 实心控件的那圈极淡内描边，与窗口红绿灯、滑杆推子同一材质口径。
const RIM: Color32 = Color32::from_rgba_unmultiplied_const(0, 0, 0, 46);

/// ACR 曲线编辑框材质（拍板 2026-11-07 §6）：定色不随主题走，与 RIM、
/// 色温轨停靠点同一口径——编辑框是一块独立的「深色画布」，浅色主题下
/// 也保持深底，曲线与剪影的对比才稳定。
///
/// ACR curve-editor materials (finalised 2026-11-07 §6): fixed colours outside
/// the theme ladder, same convention as RIM and the temperature stops -- the
/// editor is a self-contained dark canvas so curve/silhouette contrast holds in
/// every theme.
const CURVE_BG: Color32 = Color32::from_rgb(0x4B, 0x4B, 0x4B);
const CURVE_BORDER: Color32 = Color32::from_rgb(0x8C, 0x8C, 0x8C);
const CURVE_GRID: Color32 = Color32::from_rgb(0x6E, 0x6E, 0x6E);
const CURVE_HIST: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x3A);
const CURVE_DIAG: Color32 = Color32::from_rgb(0xD8, 0xD8, 0xD8);

/// 色相角 -> 纯色（HSV 满饱和满亮），只用于画示色。
///
/// Hue angle -> pure colour (HSV at full saturation and value); only used to paint
/// the swatch itself.
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
///
/// The 8 hue-band chips: the swatch is that band's own hue, and an edited band gets
/// an accent dot in its top-right corner.
/// Returns whether the selected band changed.
pub fn band_chips(ui: &mut Ui, sel: &mut usize, used: &[f64]) -> bool {
    // PS / Camera Raw 的调色混合器：一排**纯色小方块**，块里不写字；选中的那块套
    // 一圈白描边，当前色相的名字与角度写在下一行（本函数下面的调用点负责）。
    // 旧版把色名直接印在整条彩色胶囊里，八个词挤成一条彩带，既不像 PS，也让
    // 「选中」只能靠描边勉强认出来（用户 2026-11-07 反馈「这里没改，改成跟 PS 一样」）。
    //
    // Photoshop / Camera Raw's colour mixer is a row of plain colour swatches with no text
    // inside; the selected one gets a white ring, and the active band's name and angle live
    // on the next line (drawn by the caller). The old version printed the band name inside a
    // full-width coloured pill, so eight words merged into one ribbon, unlike PS, and
    // "selected" was hard to read (user feedback, 2026-11-07).
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
    const GAP: f32 = 4.0;
    let cw =
        ((rect.width() - GAP * (BAND_NAMES.len() as f32 - 1.0)) / BAND_NAMES.len() as f32).max(8.0);
    let mut changed = false;
    for (i, hue_deg) in BAND_HUES.iter().enumerate() {
        let cell = Rect::from_min_size(
            Pos2::new(rect.left() + i as f32 * (cw + GAP), rect.top() + 1.0),
            Vec2::new(cw, 15.0),
        );
        let resp = ui.interact(
            cell,
            ui.make_persistent_id(("band-chip", i)),
            Sense::click(),
        );
        let p = ui.painter();
        let hue = hue_color(*hue_deg);
        let on = *sel == i;
        // 非选中态不压暗太多：色块本身就是识别依据，压到 0.5 会让「绿」和「浅绿」
        // 分不出来。改成只降一点亮度，选中靠白环与提亮区分。
        //
        // Inactive swatches are barely darkened: the colour itself is the identifier, and
        // dropping to 0.5 made "green" and "aqua" indistinguishable. A slight dim plus a
        // white ring on the selected one is enough.
        p.rect_filled(
            cell,
            radius_control(),
            if on {
                hue
            } else if resp.hovered() {
                hue.gamma_multiply(0.9)
            } else {
                hue.gamma_multiply(0.78)
            },
        );
        if on {
            p.rect_stroke(
                cell,
                radius_control(),
                Stroke::new(1.5, Color32::WHITE),
                StrokeKind::Inside,
            );
        }
        // 用量提示（某频段有无改动）仍保留：在色块底缘点一个小点，不写字。
        //
        // The "band is in use" hint stays: a small dot on the swatch's bottom edge, no text.
        if used.get(i).is_some_and(|v| v.abs() > 1e-9) {
            p.circle_filled(
                Pos2::new(cell.center().x, cell.bottom() - 2.5),
                1.6,
                if on {
                    Color32::from_rgb(0x14, 0x14, 0x16)
                } else {
                    Color32::WHITE
                },
            );
        }
        if resp.clicked() && *sel != i {
            *sel = i;
            changed = true;
        }
    }
    changed
}

// -------------------------------------------------------------------- 分级色轮

/// 色轮盘面的顶点色（拍板 2026-11-07 §6）：中心灰 128 → 边缘全饱和，
/// 逐通道 `128 + (c - 128) * sat`。与 egui-proof 验证 crate 同一套数学。
///
/// Wheel-disc vertex colour (finalised 2026-11-07 §6): neutral grey 128 at the
/// centre to full saturation at the rim, per channel `128 + (c - 128) * sat` --
/// the same maths verified in the egui-proof crate.
fn wheel_vertex_color(deg: f64, sat: f32) -> Color32 {
    let hc = hue_color(deg);
    let ch = |c: u8| (128.0 + (c as f32 - 128.0) * sat).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(ch(hc.r()), ch(hc.g()), ch(hc.b()))
}

/// 整张色轮盘面一个网格画完（ rings+1 圈顶点 × segments+1 列）：
/// 旧版 5×36 楔形是 180 个 Shape，三轮 540 个；网格把颜色插值交给
/// 光栅化，每轮只剩 1 个 Shape，渐变还更平滑。
///
/// The whole disc in one mesh ((rings+1) vertex rows x segments+1 columns):
/// the old 5x36 wedges cost 180 shapes per wheel; the mesh hands colour
/// interpolation to the rasteriser, leaving one shape and a smoother gradient.
fn wheel_mesh(c: Pos2, radius: f32, rings: usize, segments: usize) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    let stride = segments + 1;
    for b in 0..=rings {
        let r = radius * b as f32 / rings as f32;
        let sat = b as f32 / rings as f32;
        for k in 0..=segments {
            let a = k as f64 / segments as f64 * std::f64::consts::TAU;
            mesh.vertices.push(egui::epaint::Vertex {
                pos: Pos2::new(c.x + r * (a as f32).cos(), c.y - r * (a as f32).sin()),
                uv: Pos2::ZERO,
                color: wheel_vertex_color(a.to_degrees(), sat),
            });
        }
    }
    for b in 0..rings {
        for k in 0..segments {
            let i0 = (b * stride + k) as u32;
            let i1 = i0 + 1;
            let i2 = i0 + stride as u32;
            let i3 = i2 + 1;
            mesh.indices.extend_from_slice(&[i0, i2, i1, i1, i2, i3]);
        }
    }
    mesh
}

/// 分级调色的一枚色轮（ACR 同构，拍板 2026-11-07）：HSV 盘面角度选色相、
/// 半径选浓度，底下一条明度轨。盘面中心灰 128、边缘全饱和；把手 10px
/// `#9A9A9A` 套 2px `#4B4B4B` 环。双击盘面整轮复位，双击明度轨归零。
///
/// One grading wheel (mirroring ACR, finalised 2026-11-07): the HSV disc picks
/// hue by angle and concentration by radius, with a luminance rail below. The
/// disc runs from neutral grey 128 at the centre to full saturation at the rim;
/// the handle is 10 px `#9A9A9A` inside a 2 px `#4B4B4B` ring. Double-click the
/// disc to reset the wheel, double-click the rail to zero the lift.
/// 盘面坐标 → (色相, 浓度)。半径超出盘面时贴着盘缘取方向——拖出盘外不丢点。
/// 纯函数：单击与拖动共用，单测直接量。
///
/// Disc offset to (hue, amount). Past the rim the direction is kept with the radius
/// clamped, so dragging outside never loses the point. Pure: shared by click and drag,
/// measured directly in unit tests.
fn wheel_point(d: Vec2, r_disc: f32) -> (f64, f64) {
    // 盘缘内取实长，出盘贴缘；盘径下限 1 防零除。守卫只有这一道，
    // 判据的变异验证才打得穿。
    //
    // Real length inside the rim, clamped to it outside; the 1 px floor avoids division
    // by zero. This is the single guard, so the acceptance mutation can bite.
    let r_disc = r_disc.max(1.0);
    let clamped = d.length().min(r_disc);
    // 屏幕 y 向下，取负让色相逆时针增长；与绘制用的 (cos a, -sin a) 同一口径。
    let hue = (-(d.y as f64))
        .atan2(d.x as f64)
        .to_degrees()
        .rem_euclid(360.0);
    let amount = (clamped / r_disc) as f64 * 100.0;
    (hue, amount)
}

pub fn grade_wheel(
    ui: &mut Ui,
    id: &'static str,
    label: &str,
    hue: &mut f64,
    amount: &mut f64,
    lift: &mut f64,
    size: f32,
) -> bool {
    // 翻译下沉：色轮标题（阴影 / 中间调 / 高光）由这里统一处理。
    let label = crate::i18n::tr(label);
    // 参考图逐项实测（轮径 92.5）：标签中心在块顶下方 7px，标签中心到盘顶 21px
    // （= 轮径的 22.7%），盘底到明度轨中线 21.5px（23.2%），轨宽 = 轮径 93%。
    // 旧版 head 固定 16、轨下留 18，结果是标签贴住盘顶、轨后空出一大段——竖向
    // 节奏和参考图正好相反。
    //
    // Measured on the reference (disc 92.5): the label centre sits 7 px below the
    // block top, 21 px above the disc top (22.7 % of the disc), the rail centre is
    // 21.5 px below the disc (23.2 %), and the rail is 93 % of the disc wide. The old
    // fixed head of 16 and 18 px below the rail put the label on the disc and left a
    // big hole after the rail -- the vertical rhythm was inverted.
    let head = 7.0 + size * 0.227;
    let rail_gap = size * 0.232;
    let rail_h = 10.0;
    let rail_w = size * 0.93;
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(size, head + size + rail_gap + rail_h),
        Sense::hover(),
    );
    let c = Pos2::new(rect.center().x, rect.top() + head + size * 0.5);
    let r_disc = size * 0.5 - 1.0;
    let active = *amount != 0.0 || *lift != 0.0 || *hue != 0.0;
    let p = ui.painter();
    // 标题居中压在轮子上方：三轮摆成品字时各自认领各自的轮。
    p.text(
        Pos2::new(rect.center().x, rect.top() + 7.0),
        Align2::CENTER_CENTER,
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
    let rail_y = rect.top() + head + size + rail_gap;
    let rail_x = rect.center().x - rail_w * 0.5;
    let rail_rect = Rect::from_min_max(
        Pos2::new(rail_x, rail_y - 7.0),
        Pos2::new(rail_x + rail_w, rail_y + 7.0),
    );
    let rresp = ui.interact(
        rail_rect,
        ui.make_persistent_id(("wheel-rail", id)),
        Sense::click_and_drag(),
    );

    let mut changed = false;
    // PS 手感两条：单击盘面直接把点放到那里；拖出盘外点贴着盘缘继续跟随方向，
    // 不会「拖丢」。拖动与单击共用 wheel_point 的同一套换算。
    //
    // Two PS-feel rules: a click places the point right there; dragging past the rim
    // keeps the point riding the rim in the drag direction instead of getting lost.
    if let Some(pt) = wresp
        .interact_pointer_pos()
        .filter(|_| wresp.dragged() || wresp.clicked())
    {
        let d = pt - c;
        let (h, a) = wheel_point(d, r_disc);
        *hue = h;
        *amount = a;
        changed = true;
    }
    if wresp.double_clicked() && active {
        *hue = 0.0;
        *amount = 0.0;
        *lift = 0.0;
        changed = true;
    }
    if let Some(pt) = rresp
        .interact_pointer_pos()
        .filter(|_| rresp.dragged() || rresp.clicked())
    {
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

    // 盘面：一张网格，中心灰 → 边缘全饱和（拍板规格）。
    p.add(Shape::mesh(wheel_mesh(c, r_disc, 8, 64)));
    p.circle_stroke(
        c,
        r_disc,
        Stroke::new(1.0, Color32::from_rgb(0x2C, 0x2C, 0x2C)),
    );
    // 把手：浓度 0 时落在盘心，读作「中性」；有浓度才往外走。
    let a = *hue * std::f64::consts::PI / 180.0;
    let rr = r_disc * (*amount as f32 / 100.0).clamp(0.0, 1.0);
    let k = Pos2::new(c.x + rr * (a as f32).cos(), c.y - rr * (a as f32).sin());
    p.circle_filled(k + Vec2::new(0.0, 1.0), 5.0, Color32::from_black_alpha(70));
    p.circle_filled(k, 5.0, Color32::from_rgb(0x9A, 0x9A, 0x9A));
    p.circle_stroke(
        k,
        5.0,
        Stroke::new(2.0, Color32::from_rgb(0x4B, 0x4B, 0x4B)),
    );
    // 明度轨：暗 → 中灰 → 亮 三段渐变（拍板 #2E2E2E / #8C8C8C / #E8E8E8），
    // 中点 = 0。24 片插值铺 4px 高的轨，接缝不可见。
    let rail = Rect::from_min_max(
        Pos2::new(rail_rect.left(), rail_y - 2.0),
        Pos2::new(rail_rect.right(), rail_y + 2.0),
    );
    const LUM_STOPS: [(f32, Color32); 3] = [
        (0.0, Color32::from_rgb(0x2E, 0x2E, 0x2E)),
        (0.5, Color32::from_rgb(0x8C, 0x8C, 0x8C)),
        (1.0, Color32::from_rgb(0xE8, 0xE8, 0xE8)),
    ];
    for s in 0..24 {
        let t0 = s as f32 / 24.0;
        let x0 = rail.left() + rail.width() * t0;
        let x1 = rail.left() + rail.width() * (s as f32 + 1.0) / 24.0;
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(x0, rail.top()),
                Pos2::new(x1 + 0.6, rail.bottom()),
            ),
            0,
            crate::controls::sample_stops(&LUM_STOPS, t0),
        );
    }
    let half = rail_rect.width() * 0.5;
    let kx = rail_rect.center().x + half * (*lift as f32 / 100.0);
    p.circle_filled(
        Pos2::new(kx, rail_y + 1.0),
        4.5,
        Color32::from_black_alpha(70),
    );
    p.circle_filled(
        Pos2::new(kx, rail_y),
        4.5,
        Color32::from_rgb(0xD8, 0xD8, 0xD8),
    );
    p.circle_stroke(
        Pos2::new(kx, rail_y),
        4.5,
        Stroke::new(1.0, Color32::from_rgb(0x2C, 0x2C, 0x2C)),
    );
    changed
}

// -------------------------------------------------------------------- 色调曲线

/// 曲线 PCHIP 插值表的缓存（性能重构设计 §3 第 2 层）：
/// 指纹命中就跳过 `grade::pchip_lut` 的 129 点重算。自校验——拖动改了
/// `pts` 指纹必变，天然失效，不依赖手动清理。
///
/// Cache for the curve's PCHIP lookup table (performance refactor design section 3,
/// layer 2): a fingerprint hit skips recomputing the 129-point
/// `grade::pchip_lut`. Self-invalidating -- dragging changes `pts`, which changes
/// the fingerprint, with no manual cleanup to remember.
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

/// 曲线网格的主次线（纯函数以便判据直测）：50% 中线是主线——divider 全宽；
/// 25/75% 四分线是次线——同色半宽。主次可分，不数格子也找得到中间（评审 E8）。
///
/// Major/minor grid strokes for the curve editor (pure so the gate can test it):
/// the 50 % midline is major -- full-width divider; the 25/75 % quarters are minor --
/// same colour at half width. The hierarchy is what makes the middle findable
/// without counting cells (review E8).
fn curve_grid_stroke(major: bool) -> Stroke {
    if major {
        Stroke::new(1.0, CURVE_GRID)
    } else {
        Stroke::new(0.5, CURVE_GRID)
    }
}

/// 色调曲线编辑框：横轴输入、纵轴输出，都在 0..255。
/// ACR 同构（拍板 2026-11-07）：深灰画布 + 直方图剪影垫底，曲线画在
/// 剪影之上——拉点时看的是「这张照片的影调分布」，不是一块空网格。
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
    hist: Option<&[[u32; 256]; 3]>,
    pts: &mut Vec<[f64; 2]>,
    side: f32,
    grab: &mut Option<usize>,
    cache: &mut Option<CurveLutCache>,
) -> bool {
    // 同上：曲线框的坐标说明（输出 ↑ / 输入 →）也在这里翻。
    let label = crate::i18n::tr(label);
    let head = 15.0;
    // 参考图实测：编辑框在组内**居中**，且近似正方（222×220）。旧版左贴边、
    // 高宽比 1.12，并排对照里是最大的比例差。
    //
    // Measured on the reference: the editor is **centred** in the group and close to
    // square (222x220). The old one hugged the left edge at a 1.12 height ratio --
    // the biggest proportion mismatch in the side-by-side.
    let outer_w = ui.available_width();
    let w = side.min(outer_w).max(120.0);
    let h = w.round();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(outer_w, head + h), Sense::hover());
    let inner = Rect::from_min_size(
        Pos2::new(rect.left() + (outer_w - w) * 0.5, rect.top() + head),
        Vec2::new(w, h),
    );
    let p = ui.painter();
    p.text(
        Pos2::new(inner.left(), rect.top() + 6.0),
        Align2::LEFT_CENTER,
        label,
        font(SIZE_SMALL),
        text(),
    );
    p.rect_filled(inner, radius_control(), CURVE_BG);
    p.rect_stroke(
        inner,
        radius_control(),
        Stroke::new(1.0, CURVE_BORDER),
        StrokeKind::Inside,
    );
    let sx = inner.width() / 255.0;
    let sy = inner.height() / 255.0;
    let to_px =
        |x: f64, y: f64| Pos2::new(inner.left() + x as f32 * sx, inner.bottom() - y as f32 * sy);

    // 直方图剪影：三通道逐 bin 取最大值合成一条影调分布，64 段采样，
    // 峰值归一。**逐列四边形**而不是一条闭合多边形——剪影是非凸的，
    // PathShape 的填充三角化会在凹处漏出大块楔形（实机截图里右下角那块
    // 灰色三角形），逐列三角带没有这个问题。
    //
    // Histogram silhouette: per-bin max over the three channels, 64 samples,
    // normalised to the peak. Drawn as **one quad per column** rather than a closed
    // polygon: the silhouette is concave, and PathShape's fill triangulation leaked a
    // large wedge at the concave side (the grey triangle in the lower right of the live
    // screenshot). A column strip has no such failure mode.
    if let Some(h) = hist {
        let peak = h
            .iter()
            .flat_map(|b| b.iter())
            .max()
            .copied()
            .unwrap_or(0)
            .max(1) as f32;
        let mut mesh = egui::Mesh::default();
        let n = 64usize;
        let mut prev: Option<(f32, f32)> = None;
        for k in 0..n {
            let i0 = k * 4;
            let mx = h
                .iter()
                .map(|b| b[i0..i0 + 4].iter().max().copied().unwrap_or(0))
                .max()
                .unwrap_or(0);
            let x = inner.left() + (k as f32 / (n - 1) as f32) * inner.width();
            let y = inner.bottom() - (mx as f32 / peak) * (inner.height() - 2.0);
            if let Some((px, py)) = prev {
                let i = mesh.vertices.len() as u32;
                mesh.colored_vertex(Pos2::new(px, py), CURVE_HIST);
                mesh.colored_vertex(Pos2::new(x, y), CURVE_HIST);
                mesh.colored_vertex(Pos2::new(px, inner.bottom()), CURVE_HIST);
                mesh.colored_vertex(Pos2::new(x, inner.bottom()), CURVE_HIST);
                mesh.add_triangle(i, i + 1, i + 2);
                mesh.add_triangle(i + 1, i + 3, i + 2);
            }
            prev = Some((x, y));
        }
        p.add(Shape::Mesh(mesh.into()));
    }

    for i in 1..4 {
        let f = i as f32 / 4.0;
        // 主次线（评审 E8）：50% 中线是主线，25/75% 四分线是次线——同色半宽，
        // 不数格子也找得到中间。次要线不许比主线粗，判据钉在 curve_grid_stroke。
        let grid = curve_grid_stroke(i == 2);
        p.line_segment(
            [
                Pos2::new(inner.left() + inner.width() * f, inner.top() + 1.0),
                Pos2::new(inner.left() + inner.width() * f, inner.bottom() - 1.0),
            ],
            grid,
        );
        p.line_segment(
            [
                Pos2::new(inner.left() + 1.0, inner.top() + inner.height() * f),
                Pos2::new(inner.right() - 1.0, inner.top() + inner.height() * f),
            ],
            grid,
        );
    }
    p.line_segment(
        [to_px(0.0, 0.0), to_px(255.0, 255.0)],
        Stroke::new(1.0, CURVE_DIAG),
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
    // 曲线与圆点一律裁进画布：线宽 1.2 压在 1px 描边上也会溢出去，端点圆点
    // 更是有一半落在框外（用户截图说的「曲线顶出显示区域」）。
    //
    // Line and handles are clipped to the plot: a 1.2 px line still spills over the
    // 1 px border, and the endpoint dots sit half outside the frame -- the "curve
    // pokes out of the display area" complaint.
    let cp = p.with_clip_rect(inner);
    cp.add(Shape::line(line, Stroke::new(1.2, tint)));
    let hover_pos = ui.input(|i| i.pointer.hover_pos());
    let hover_idx = hover_pos.and_then(|q| nearest(pts, q));
    for (i, q) in pts.iter().enumerate() {
        let c = to_px(q[0], q[1]);
        let editable = i != 0 && i + 1 != pts.len();
        let held = *grab == Some(i);
        // 端点默认不画圆点：PS / ACR 参考图里两端就是一条线收到框角，没有
        // 半出框的圆点；只有悬停在端点上或正在拖它时才补一枚（仍裁在框内）。
        //
        // Endpoints draw no dot by default -- in the PS / ACR reference the line
        // simply reaches the corner. A dot appears only while that endpoint is hovered
        // or dragged, and it is clipped to the plot.
        if !editable && !held && hover_idx != Some(i) {
            continue;
        }
        let r = if held { 4.0 } else { 3.2 };
        // 控制点随通道着色（拍板 §6）：红通道一条红点，绿通道一条绿点，
        // 与曲线本体同色——三条单通道来回切时不会认错自己刚拉的那条。
        cp.circle_filled(c, r, if editable { tint } else { CURVE_BG });
        cp.circle_stroke(
            c,
            r,
            Stroke::new(
                1.0,
                if editable {
                    Color32::from_rgb(0x2C, 0x2C, 0x2C)
                } else {
                    CURVE_BORDER
                },
            ),
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
///
/// The speed-curve box: x is **source progress** (0..1), y is speed (log).
///
/// It draws the very polyline `retime::speed_at` computes -- linear between keys,
/// so the line is straight and needs no pchip like the tone curve does; what the
/// panel shows is exactly what the export computes.
/// Dragging: the two end points move speed only (they pin the curve ends), the
/// inner points move both ways; clicking empty space adds a point, double-clicking
/// a point removes it, double-clicking empty space reduces to the two ends.
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

    /// 色轮换算的 PS 手感：盘内按方向定位；盘外贴缘不丢点；盘心即中性。
    /// 回归锚：用户要求「学 PS，移动一个点进行调节」。
    ///
    /// The wheel mapping's PS feel: inside, position follows the direction; outside,
    /// the point rides the rim; the centre is neutral. Regression anchor: the user asked
    /// for "learn from PS -- move one point to adjust".
    #[test]
    fn wheel_point_places_clamps_and_neutralises() {
        let r = 46.0;
        // 盘心：浓度 0（中性），色相任意但为 0。
        let (h, a) = wheel_point(Vec2::ZERO, r);
        assert!(a.abs() < 1e-9, "盘心必须是中性浓度");
        assert!(h.abs() < 1e-9);
        // 正右方：色相 0、浓度满。
        let (h, a) = wheel_point(Vec2::new(r, 0.0), r);
        assert!(h.abs() < 1e-6 && (a - 100.0).abs() < 1e-6);
        // 正上方（屏幕 y 向下 → 色相 90）。
        let (h, a) = wheel_point(Vec2::new(0.0, -r), r);
        assert!((h - 90.0).abs() < 1e-6 && (a - 100.0).abs() < 1e-6);
        // 拖出盘外：贴缘、方向保留（这是「不丢点」的关键）。
        let (h, a) = wheel_point(Vec2::new(0.0, -3.0 * r), r);
        assert!((h - 90.0).abs() < 1e-6, "出盘后方向不许翻转");
        assert!((a - 100.0).abs() < 1e-6, "出盘后浓度贴缘");
        // 半径方向 45°：色相 45。
        let (h, _) = wheel_point(Vec2::new(r, -r) * (r / (r * r * 2.0).sqrt()), r);
        assert!((h - 45.0).abs() < 1e-6, "对角线方向色相 {h}");
    }

    #[test]
    fn curve_grid_midline_is_the_major_stroke() {
        // 评审 E8：中线主、四分线次，只许宽度有别、不许换色。
        let (major, minor) = (curve_grid_stroke(true), curve_grid_stroke(false));
        assert!(
            major.width > minor.width,
            "曲线网格中线必须比四分线粗：{:?} vs {:?}",
            major,
            minor
        );
        assert_eq!(major.color, minor.color, "主次线同色，宽度是唯一的层级");
    }

    #[test]
    fn hue_endpoints_land_on_expected_primaries() {
        assert_eq!(hue_color(0.0), Color32::from_rgb(255, 0, 0));
        assert_eq!(hue_color(120.0), Color32::from_rgb(0, 255, 0));
        assert_eq!(hue_color(240.0), Color32::from_rgb(0, 0, 255));
        // 负角与超过一圈都要绕回来：-120 与 240 同相。
        assert_eq!(hue_color(-120.0), hue_color(240.0));
        assert_eq!(hue_color(480.0), hue_color(120.0));
    }

    /// 拍板 §6 的色轮数学：盘心必须是真中性灰 128，边缘必须是满饱和，
    /// 半半径是两者的逐通道中点。这三条钉住「灰心 HSV 盘」不退化成
    /// 旧的白心盘或深底盘。
    #[test]
    fn wheel_disc_is_neutral_at_center_and_saturated_at_rim() {
        assert_eq!(
            wheel_vertex_color(0.0, 0.0),
            Color32::from_rgb(128, 128, 128),
            "盘心不是中性灰 128"
        );
        assert_eq!(wheel_vertex_color(0.0, 1.0), hue_color(0.0));
        assert_eq!(wheel_vertex_color(240.0, 1.0), hue_color(240.0));
        assert_eq!(
            wheel_vertex_color(120.0, 0.5),
            Color32::from_rgb(64, 192, 64),
            "半半径不是逐通道中点"
        );
    }

    /// 网格拓扑：顶点数 = (rings+1)×(segments+1)，三角形索引不越界。
    #[test]
    fn wheel_mesh_topology_is_closed_and_in_bounds() {
        let mesh = wheel_mesh(Pos2::new(50.0, 60.0), 40.0, 8, 64);
        assert_eq!(mesh.vertices.len(), 9 * 65);
        assert_eq!(mesh.indices.len(), 8 * 64 * 6);
        assert!(
            mesh.indices
                .iter()
                .all(|&i| (i as usize) < mesh.vertices.len()),
            "索引越界"
        );
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
                Some(&hist),
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
