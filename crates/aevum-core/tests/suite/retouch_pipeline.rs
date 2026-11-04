//! 修复画笔与透视校正在显影链里的**位置契约**。
//!
//! `retouch` 与 `perspective` 两个模块自己的判据只管「算法对不对」，管不了「有没有接进
//! 主链、接在哪个位置」。这份文件只管后者：
//!
//! 1. 两者为空 / 为 `None` 时，渲染结果与没有这两个字段时逐位相同——接入不能改变老
//!    工程和一切未使用新功能的画面。
//! 2. 修复**先于**调色。这条靠「手算一路对照」钉住：先 heal 再 `render`（清空 retouch）
//!    的结果，必须等于直接 `render`（带 retouch）。谁把 heal 挪到 `core()` 之后，两条
//!    路立刻分岔。
//! 3. 生成 .cube LUT 的那条链跳过两者：LUT 只有逐点运算，把几何重采样与解拉普拉斯塞进
//!    一张查表在语义上就是错的。
//! 4. 梯形校正之后画面中心的内容还在——校正不是「把图抹黑」。
//!
//! Position contract for the healing brush and perspective correction inside the develop
//! chain.
//!
//! The `retouch` and `perspective` modules' own tests only cover "is the algorithm right";
//! they cannot see "did it get wired in, and where". This file covers only the latter:
//!
//! 1. With an empty list / `None`, the render matches the one without those fields bit for
//!    bit — wiring must not change old projects or any frame that does not use the features.
//! 2. Repair comes **before** grading. Pinned by a hand-computed counterpart: healing first
//!    and then `render` (with retouch cleared) must equal `render` with retouch set. Move the
//!    heal after `core()` and the two paths diverge immediately.
//! 3. The .cube LUT chain skips both: a LUT holds pointwise operations only, and putting a
//!    geometric resample or a Laplace solve into a lookup table is wrong by definition.
//! 4. After keystone correction the centre of the frame still carries content — correction is
//!    not "blacken the picture".

use aevum_core::develop::{self, Ev, Opts};
use aevum_core::grade::GradeParams;
use aevum_core::perspective::{self, Quad};
use aevum_core::retouch::{self, SpotSpec};

/// 一张 64×48 的水平渐变加一颗亮斑：修复有邻域可依，校正有结构可看。
///
/// A 64×48 horizontal gradient with a bright blob: repair has neighbours to work with and
/// correction has structure to look at.
fn scene() -> (Vec<f32>, usize, usize) {
    let (w, h) = (64usize, 48usize);
    let mut v = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let t = 0.15 + 0.5 * (x as f32 / w as f32);
            let o = (y * w + x) * 3;
            v[o] = t;
            v[o + 1] = t * 0.9;
            v[o + 2] = t * 0.8;
        }
    }
    // 中心一颗高亮小斑：修复的目标。
    //
    // A small bright blob in the centre: the repair target.
    for y in 21..27 {
        for x in 29..35 {
            let o = (y * w + x) * 3;
            v[o] = 0.97;
            v[o + 1] = 0.97;
            v[o + 2] = 0.97;
        }
    }
    (v, w, h)
}

fn spot() -> SpotSpec {
    SpotSpec {
        x: 0.5,
        y: 0.5,
        r: 0.09,
        feather: 0.4,
        source: None,
    }
}

/// 一条轻微的梯形：上边收窄 6%，模拟仰拍。
///
/// A mild keystone: the top edge narrowed by 6%, as if shot from below.
fn keystone() -> Quad {
    Quad {
        p: [(0.06, 0.0), (0.94, 0.0), (1.0, 1.0), (0.0, 1.0)],
    }
}

/// 空列表 / `None` 必须与「没有这两个字段」逐位相同：接入不能碰到老工程与未使用新功能
/// 的画面。
///
/// An empty list / `None` must match "those fields do not exist" bit for bit: wiring must not
/// touch old projects or frames that do not use the features.
#[test]
fn an_unused_repair_and_keystone_change_nothing() {
    let (src, w, h) = scene();
    let mut plain = GradeParams::default();
    plain.exposure = 0.4;
    plain.contrast = 12.0;
    let mut with_fields = plain.clone();
    with_fields.retouch = Vec::new();
    with_fields.upright = None;

    let (mut a, mut b) = (src.clone(), src.clone());
    develop::render(&mut a, w, h, &plain, Ev::NONE, Opts::FULL);
    develop::render(&mut b, w, h, &with_fields, Ev::NONE, Opts::FULL);
    assert_eq!(a, b, "空 retouch / None upright 改变了画面");
}

/// 修复先于调色：手算「先 heal、再渲染」必须等于「带 retouch 一次渲染」。
/// 顺序一改（heal 挪到调色之后），两条路立刻分岔。
///
/// Repair runs before grading: hand-computed "heal, then render" must equal one render with
/// retouch set. Change the order and the two paths diverge at once.
#[test]
fn repair_runs_before_grading() {
    let (src, w, h) = scene();
    let mut p = GradeParams::default();
    // 曝光 + 对比：只要有一步顺序颠倒，这两个逐点项就会把差异放大到肉眼可查。
    //
    // Exposure plus contrast: if any ordering flips, these two pointwise stages amplify the
    // difference beyond any threshold.
    p.exposure = 0.8;
    p.contrast = 35.0;
    let mut once = p.clone();
    once.retouch = vec![spot()];

    // 路径 A：一次渲染。
    //
    // Path A: one render.
    let mut a = src.clone();
    develop::render(&mut a, w, h, &once, Ev::NONE, Opts::FULL);

    // 路径 B：手工先把笔触落到缓冲上，再渲染（参数里没有 retouch）。
    //
    // Path B: apply the stroke to the buffer by hand, then render with no retouch.
    let mut b = src.clone();
    assert_eq!(retouch::apply(&mut b, w, h, &[spot()]), 1);
    develop::render(&mut b, w, h, &p, Ev::NONE, Opts::FULL);

    let worst = a
        .iter()
        .zip(&b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1e-6, "修复没有落在调色之前（最大差 {worst:.6}）");

    // 反向对照：不修的那一路必须与修过的明显不同，否则上一条会因为「两者都是原图」而
    // 空过。
    //
    // Counter-check: the unhealed path must differ visibly from the healed one, otherwise the
    // assertion above could pass because both are the untouched image.
    let mut untouched = src.clone();
    develop::render(&mut untouched, w, h, &p, Ev::NONE, Opts::FULL);
    let gap = a
        .iter()
        .zip(&untouched)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(gap > 0.05, "修复没生效（最大差 {gap:.4}）");
}

/// 生成 .cube LUT 的链必须跳过几何与修补：LUT 是逐点查表，几何重采样与解拉普拉斯都
/// 不是逐点运算，塞进去等于给每个像素算一个错的值。
///
/// The .cube LUT chain must skip geometry and repair: a LUT is a pointwise lookup, while a
/// geometric resample and a Laplace solve are not pointwise — baking them in would give every
/// pixel a wrong value.
#[test]
fn the_lut_chain_skips_geometry_and_repair() {
    let (src, w, h) = scene();
    let mut p = GradeParams::default();
    p.exposure = 0.3;
    let mut with = p.clone();
    with.retouch = vec![spot()];
    with.upright = Some(keystone());

    let mut a = src.clone();
    develop::render(
        &mut a,
        w,
        h,
        &p,
        Ev::NONE,
        Opts {
            for_lut: true,
            ..Opts::FULL
        },
    );
    let mut b = src.clone();
    develop::render(
        &mut b,
        w,
        h,
        &with,
        Ev::NONE,
        Opts {
            for_lut: true,
            ..Opts::FULL
        },
    );
    assert_eq!(a, b, "for_lut 链被几何或修复污染了");
}

/// 梯形校正之后画面中心还有内容，且四角确实被重新采样（不是逐位不变）。
///
/// After keystone correction the centre still carries content, and the corners really were
/// resampled (not bit-identical).
#[test]
fn a_keystone_keeps_the_centre_and_moves_the_frame() {
    let (src, w, h) = scene();
    let mut p = GradeParams::default();
    p.upright = Some(keystone());

    let mut got = src.clone();
    develop::render(&mut got, w, h, &p, Ev::NONE, Opts::FULL);

    let c = ((h / 2) * w + w / 2) * 3;
    assert!(got[c] > 0.2, "校正后中心变成了黑块：{}", got[c]);
    assert!(got != src, "四边形没有生效（画面逐位不变）");
    // 左上角在四边形之外，采样会落回边界，颜色必然与原图不同。
    //
    // The top-left corner sits outside the quad; sampling falls back to the border, so its
    // colour must differ from the original.
    assert!(
        (got[0] - src[0]).abs() > 1e-4,
        "左上角没有被重采样（校正像是没生效）"
    );
}

/// 归一化四边形可以被钳进画幅；含 NaN 的四边形直接判废——不许把 NaN 送进采样器。
///
/// A normalized quad clamps into the frame; one containing NaN is rejected outright — no NaN
/// may reach the sampler.
#[test]
fn quad_clamping_is_total() {
    let over = Quad {
        p: [(-0.2, 0.1), (1.3, -0.05), (1.1, 1.2), (0.02, 0.98)],
    };
    let c = over.clamped().expect("可钳");
    assert_eq!(c.p[0], (0.0, 0.1));
    assert_eq!(c.p[1], (1.0, 0.0));
    assert_eq!(c.p[2], (1.0, 1.0));

    let bad = Quad {
        p: [(f64::NAN, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
    };
    assert!(bad.clamped().is_none(), "NaN 角必须判废");
    assert!(Quad::default().is_identity(), "缺省四边形必须是单位方");
    assert!(!keystone().is_identity());
}

/// `perspective::rectify_in_place` 与 `rectify` 的输出一致：就地版不许偷偷换一套采样。
///
/// `perspective::rectify_in_place` matches `rectify`'s output: the in-place variant must not
/// quietly use a different sampler.
#[test]
fn the_in_place_variant_matches_the_allocating_one() {
    let (src, w, h) = scene();
    let q = keystone();
    let want = perspective::rectify(&src, w, h, &q, w, h);
    let mut got = src.clone();
    perspective::rectify_in_place(&mut got, w, h, &q);
    assert_eq!(got, want);
}
