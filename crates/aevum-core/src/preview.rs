//! 预览渲染的入口：把调用方转发到 `develop` 主链。
//!
//! 保留这个模块只为兼容既有调用点（GUI 预览、静帧导出、视频导出、MCP）。
//! 真正的像素顺序、阶段开关与金标比对都在 [crate::develop]。
//!
//! Entry point for preview rendering: forwards the caller to the `develop` main chain.
//!
//! The module is kept only for compatibility with existing call sites (GUI preview, still
//! export, video export, MCP). The real pixel order, stage switches and golden comparisons all
//! live in [crate::develop].

use crate::develop::{self, Ev, Opts};
use crate::grade::GradeParams;

/// 全量渲染：松手后的精修预览、静帧导出与视频导出都走这里。
///
/// Full render: the refined preview after release, still export and video export all come
/// through here.
pub fn render(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams, gain: Ev) {
    develop::render(rgb, w, h, p, gain, Opts::FULL);
}

/// HDR 导出的渲染：全量，且**不把白以上的部分截断**。
///
/// 与 [`render`] 的唯一差别就是这一条：`render` 在终点把结果钳到 [0,1]（SDR 的语义，
/// 下游按 8bit 写盘），这里只压负值，好让 PQ 编码器接住参考白以上的亮度。预览默认走
/// [`render`]（面板显示不到白以上，看到的就是 SDR 交付的样子）；想要在面板里检视白以上
/// 有没有细节时，走这里再接 [`fold_above_white_in_place`]。
///
/// HDR export render: full set, and **values above white are not clamped**.
///
/// The only difference from [`render`]: that one pins the result to [0,1] at the end (SDR
/// semantics — it is written as 8bit), while this one only floors negatives so the PQ encoder
/// can receive everything above reference white.
///
/// 预览也可以走这里，但**必须**紧跟 [`fold_above_white_in_place`]：面板只能显示 0..1，
/// 不折的话白以上的部分在成图时会被那个环节钳掉，看起来就和 SDR 一样。
///
/// Preview may use this too, but **must** follow it with [`fold_above_white_in_place`]: the
/// panel can only show 0..1, and without folding the above-white part gets clamped by whatever
/// converts to the texture, looking exactly like SDR.
pub fn render_hdr(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams, gain: Ev) {
    develop::render(rgb, w, h, p, gain, Opts::HDR);
}

/// 拖动中的草图：跳过最贵的空间运算，保证跟手。
/// 无空间参数时与 [render] 逐点一致，有差异的那几项正是草图省掉的。
///
/// Sketch while dragging: skips the most expensive spatial operations to stay responsive.
/// With no spatial parameters it matches [render] per pixel; the few differing terms are
/// exactly what the sketch omits.
pub fn render_sketch(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams, gain: Ev) {
    develop::render(rgb, w, h, p, gain, Opts::SKETCH);
}

/// 草图的 HDR 版：拖动中也保留白以上（否则一松手 HDR 检视会突然变样）。
///
/// The sketch version with HDR: above-white survives mid-drag too, otherwise releasing the
/// slider makes the HDR inspection view jump.
pub fn render_hdr_sketch(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams, gain: Ev) {
    develop::render(
        rgb,
        w,
        h,
        p,
        gain,
        Opts {
            hdr: true,
            ..Opts::SKETCH
        },
    );
}

/// 预览里「白以上」的折点：这一段码值留给超过参考白的部分。
///
/// 为什么是 0.90：留 10% 的码值给白以上，8bit 下约 25 级——少于 25 级时这个渐变在面板上
/// 看不出来（相邻两级的差别已到底噪），多于 30 级则会让纯白明显发灰、用户以为调色变了。
///
/// The fold knee for "above white" in the preview: this slice of the code range is reserved
/// for content past reference white.
///
/// Why 0.90: 10% of the code range is about 25 levels at 8bit. Fewer than 25 and the gradient
/// is invisible on screen (neighbouring levels are already at the noise floor); more than 30
/// and pure white visibly greys out, which reads as "the grade changed".
pub const FOLD_KNEE: f32 = 0.90;

/// 折线在折点之后的软度（线性光单位）。
///
/// 1.0 这个值由**输出端的分辨力**定：白以上要能分出档。实测 `x=2` 落在 0.9667、
/// `x=4` 落在 0.9955（8bit 差 7 级）；软度压到 0.25 时 `x=2` 已经 0.9988，`x=4` 贴住
/// 1.0，等于白以上又变成一片白。
///
/// Softness of the curve past the knee, in linear light.
///
/// 1.0 is set by **what the output can resolve**: content above white must come out in
/// distinguishable steps. Measured: `x=2` lands on 0.9667 and `x=4` on 0.9955 (7 levels apart
/// at 8bit); at a softness of 0.25, `x=2` is already 0.9988 and `x=4` hugs 1.0 — above white
/// collapses into flat white again.
pub const FOLD_SOFTNESS: f32 = 1.0;

/// 预览映射：把「白以上」折进可见的最后一小段码值，让 SDR 面板能看见高光里有没有细节。
///
/// **这不是 HDR 屏模拟，也不假装是。** 一块 100 nit 的 SDR 面板显示不了 203 nit 参考白
/// 与 1000 nit 峰值之间的差别——那是显示器亮度的事，软件改不了。预览能回答的只有一个
/// 问题：白以上到底还有没有东西（HDR 保住了高光层次，还是和 SDR 一样被削平）。
///
/// 所以这条映射的语义是：
///
/// - `x ≤ FOLD_KNEE`：**逐位不动**。也就是说中间调、阴影、以及大多数亮部与关闭开关时
///   完全一致，开关只影响白以上——用户不会因为打开检视就怀疑自己调色变了。
/// - `x > FOLD_KNEE`：`K + (1−K)·(1 − e^{−(x−K)/S})`，连续、单调、恒小于 1。折点之后
///   斜率是 `(1−K)/S`，越往右越平——这正是「白以上还有层次但挤在一起」的样子。
///
/// 逐通道做，不做色度保持：过曝区去饱和本来就是胶片与传感器在高光处的行为。
///
/// Preview mapping: fold "above white" into the last visible slice of the code range so an
/// SDR panel can show whether the highlights still carry detail.
///
/// **This is not an HDR display simulation and does not pretend to be one.** A 100-nit SDR
/// panel cannot show the difference between a 203-nit reference white and a 1000-nit peak —
/// that is display hardware, and software cannot fix it. The one question a preview *can*
/// answer is whether anything exists above white at all (did HDR keep the highlight layers, or
/// was it flattened the same way SDR would be).
///
/// So the semantics are:
///
/// - `x ≤ FOLD_KNEE`: **bit-identical**. Midtones, shadows and most of the bright range are
///   exactly what they are with the toggle off; only above-white is affected, so nobody
///   suspects their grade changed when they turn the inspection on.
/// - `x > FOLD_KNEE`: `K + (1−K)·(1 − e^{−(x−K)/S})`, continuous, monotonic, always below 1.
///   The slope past the knee is `(1−K)/S` and flattens to the right — which is exactly what
///   "detail still there but squeezed" looks like.
///
/// Per channel, with no chroma preservation: overexposed regions desaturating is what film and
/// sensors do at the top end anyway.
pub fn fold_above_white_in_place(rgb: &mut [f32]) {
    let k = FOLD_KNEE;
    let room = 1.0 - k;
    for v in rgb.iter_mut() {
        let x = *v;
        if x > k {
            *v = k + room * (1.0 - (-(x - k) / FOLD_SOFTNESS).exp());
        } else if x < 0.0 {
            *v = 0.0;
        }
    }
}

/// 只留逐点运算的一条链，用来生成 .cube LUT。
///
/// A chain with only per-pixel operations, used to generate .cube LUTs.
pub fn render_for_lut(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams, gain: Ev) {
    develop::render(
        rgb,
        w,
        h,
        p,
        gain,
        Opts {
            for_lut: true,
            ..Opts::FULL
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(v: f32, n: usize) -> Vec<f32> {
        vec![v; n]
    }

    #[test]
    fn exposure_raises_flat_gray() {
        let mut px = flat(0.5, 10 * 8 * 3);
        let p = GradeParams {
            exposure: 1.0,
            ..Default::default()
        };
        render(&mut px, 10, 8, &p, Ev::NONE);
        assert!((px[0] - 0.69).abs() < 0.03, "got {}", px[0]);
    }

    #[test]
    fn neutral_params_are_identity() {
        let mut px = flat(0.337, 4 * 4 * 3);
        let p = GradeParams::default();
        render(&mut px, 4, 4, &p, Ev::NONE);
        assert!((px[0] - 0.337).abs() < 1e-6);
    }

    #[test]
    fn vignette_darkens_corners_not_center() {
        let mut px = flat(0.6, 32 * 18 * 3);
        let p = GradeParams {
            vignette: 60.0,
            ..Default::default()
        };
        render(&mut px, 32, 18, &p, Ev::NONE);
        let center = (9 * 32 + 16) * 3;
        assert!(px[center] > 0.55);
        assert!(px[0] < 0.5);
    }

    #[test]
    fn mask_exposure_only_changes_its_region() {
        let mut px = flat(0.3, 16 * 16 * 3);
        let mut p = GradeParams::default();
        p.masks = vec![serde_json::json!({
            "id": "m", "type": "linear",
            "x1": 0.5, "y1": 0.0, "x2": 0.5, "y2": 0.5,
            "feather": 0.0, "midpoint": 50.0, "amount": 100.0,
            "params": {"exposure": 1.0},
        })];
        render(&mut px, 16, 16, &p, Ev::NONE);
        assert!(px[0] > 0.4, "top {}", px[0]);
        let bottom = 15 * 16 * 3;
        assert!((px[bottom] - 0.3).abs() < 1e-6, "bottom {}", px[bottom]);
    }

    /// 草图只在空间运算上和精修不同：没碰空间参数时两条链必须逐点相同。
    #[test]
    fn sketch_and_full_agree_without_spatial_params() {
        let mut a = flat(0.42, 24 * 16 * 3);
        let mut b = a.clone();
        let p = GradeParams {
            exposure: 0.4,
            contrast: 30.0,
            highlights: -25.0,
            shadows: 20.0,
            temperature: 22.0,
            vibrance: 18.0,
            saturation: -9.0,
            vignette: 40.0,
            ..Default::default()
        };
        render(&mut a, 24, 16, &p, Ev::NONE);
        render_sketch(&mut b, 24, 16, &p, Ev::NONE);
        let diff = a
            .iter()
            .zip(&b)
            .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()));
        assert!(diff < 1e-6, "草图与精修差 {diff}");
    }

    /// LUT 用的那条链不该碰任何逐点之外的东西：几何、噪声、晕影都得跳过。
    #[test]
    fn for_lut_skips_every_spatial_stage() {
        let mut px = flat(0.42, 24 * 16 * 3);
        let mut p = GradeParams {
            exposure: 0.4,
            contrast: 30.0,
            dehaze: 20.0,
            vibrance: 18.0,
            temperature: 22.0,
            grain: 60.0,
            vignette: 60.0,
            lens_distort: 20.0,
            sharp_amount: 80.0,
            clarity: 50.0,
            texture: 40.0,
            nr_luma: 40.0,
            nr_color: 30.0,
            lv_gamma: 1.2,
            par_shadows: 20.0,
            ..Default::default()
        };
        p.masks = vec![serde_json::json!({
            "id": "m", "type": "linear",
            "x1": 0.0, "y1": 0.0, "x2": 1.0, "y2": 0.0,
            "feather": 0.0, "midpoint": 50.0, "amount": 100.0,
            "params": {"exposure": 1.0},
        })];
        let before = px.clone();
        render_for_lut(&mut px, 24, 16, &p, Ev::NONE);

        // 全平的一幅图：逐点链跑完，每个通道内部仍然得是常数——通道之间可以不等
        // （色温本来就会给 R/G/B 不同的增益），但绝不允许出现位置相关的差异。
        for c in 0..3 {
            let ch = px.iter().skip(c).step_by(3).copied();
            let spread = ch.clone().fold(f32::MIN, f32::max) - ch.fold(f32::MAX, f32::min);
            assert!(
                spread < 1e-6,
                "LUT 链里混进了空间项，通道 {c} 极差 {spread}"
            );
        }
        assert!(px[..3] != before[..3], "至少逐点项要起作用");
    }

    /// 草图必须真的省掉那四项最贵的空间运算。
    ///
    /// 这里不能用全平图：清晰度/纹理/锐化对一张无细节的图本来就不起作用，
    /// 跳没跳过都看不出差别，所以喂一份伪随机噪声。
    #[test]
    fn sketch_skips_each_priciest_stage() {
        let n = 32 * 24 * 3;
        // 固定种子的 LCG：不引 rand，且失败时可复现。
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut noise = Vec::with_capacity(n);
        for _ in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            noise.push(((s >> 33) as f32 / (1u64 << 31) as f32) * 0.8 + 0.1);
        }
        let stages = [
            ("清晰度", "clarity", 60.0),
            ("纹理", "texture", 50.0),
            ("锐化", "sharp_amount", 90.0),
            ("降噪", "nr_luma", 50.0),
        ];
        for (name, key, value) in stages {
            let mut p: GradeParams =
                serde_json::from_value(serde_json::json!({ key: value })).expect("单参数工程");
            if key == "nr_luma" {
                p.nr_color = 40.0;
            }
            let mut full = noise.clone();
            let mut sketch = noise.clone();
            render(&mut full, 32, 24, &p, Ev::NONE);
            render_sketch(&mut sketch, 32, 24, &p, Ev::NONE);
            let diff = full
                .iter()
                .zip(&sketch)
                .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(
                diff > 1e-3,
                "草图没跳过{name}：两条链只差 {diff}，等于什么都没省"
            );
        }
    }

    // -----------------------------------------------------------------------
    // B1：预览里「白以上」的折线
    // -----------------------------------------------------------------------

    /// 折线在折点以下必须**逐位不动**——这是「打开检视不等于改了我的调色」的唯一保证。
    ///
    /// 判据取折点本身与它左边几个典型值：0.05（暗部）、0.45（中灰）、0.895（刚好在折点
    /// 左侧）。`assert_eq!` 而不是容差比较：任何一位不同都说明折线漏到了白以下。
    ///
    /// Below the knee the fold must be **bit-identical** — that is the only guarantee behind
    /// "turning the inspection on did not change my grade". The values are the knee itself and
    /// three typical points to its left; `assert_eq!` rather than a tolerance, because a single
    /// differing bit means the fold leaked below white.
    #[test]
    fn fold_leaves_everything_below_the_knee_alone() {
        let src = [0.0f32, 0.05, 0.45, 0.895, FOLD_KNEE];
        let mut px: Vec<f32> = src.to_vec();
        fold_above_white_in_place(&mut px);
        assert_eq!(px, src, "折线动了白以下的像素");
    }

    /// 白以上必须**看得出层次**：连续、单调、恒小于 1，且 x=2 与 x=4 分得开。
    ///
    /// 四条缺一不可，因为这条曲线唯一的用途就是「代替硬削平」：
    /// - 恒小于 1：面板没有白以上，映射一旦到 1.0 就等于又在硬削；
    /// - 单调：不单调会让高光里出现反转的灰带，比削平更假；
    /// - x=1 落进 (0.90, 0.96)：纯白要**离开**顶端才显得出后面还有东西，但不能离开太多
    ///   （超过 0.96 用户会觉得画面发灰）；
    /// - x=2 与 x=4 差 ≥ 5/255：两级之内分不开的话，这条映射在 8bit 上等于没做。
    ///
    /// Above white must show **layering**: continuous, monotonic, always below 1, and x=2 must
    /// separate from x=4. All four matter because replacing a hard clip is the curve's only
    /// job: always below 1 (the panel has no above-white, so reaching 1.0 is just clipping
    /// again); monotonic (otherwise highlights grow a reversed grey band, faker than a clip);
    /// x=1 inside (0.90, 0.96) (pure white must leave the top end to leave room, but not by
    /// so much that the picture looks washed out); x=2 vs x=4 at least 5/255 apart (anything
    /// tighter is invisible in 8bit and the mapping does nothing).
    #[test]
    fn fold_keeps_layering_above_white() {
        let map = |x: f32| {
            let mut v = [x];
            fold_above_white_in_place(&mut v);
            v[0]
        };
        let white = map(1.0);
        assert!(
            (FOLD_KNEE..0.96).contains(&white),
            "纯白折到了 {white:.4}，不在 ({FOLD_KNEE}, 0.96) 里"
        );

        let mut prev = FOLD_KNEE;
        for i in 0..=400 {
            let x = FOLD_KNEE + i as f32 * 0.02;
            let y = map(x);
            assert!(
                y >= prev - 1e-7,
                "折线在 x={x:.2} 处回退：{y:.6} < {prev:.6}"
            );
            assert!(y < 1.0, "折线在 x={x:.2} 处到了 {y:.6}，白以上又被削平了");
            prev = y;
        }

        let gap = (map(4.0) - map(2.0)) * 255.0;
        assert!(gap >= 5.0, "x=2 与 x=4 只差 {gap:.1} 级，8bit 上看不出层次");
    }

    /// 打开检视之后的整幅预览：亮部真的留在白以上（而不是被成图那一步钳掉）。
    ///
    /// 这条和上面两条不是一回事：上面量的是折线本身，这里量的是**接到渲染之后**的
    /// 结果——`render_hdr` 造出 >1 的亮度、折线把它折进 (0.9, 1.0)，全程没有一步把
    /// 它压成 1.0。
    ///
    /// The full preview with the inspection on: the highlights really do sit above white
    /// (instead of being clamped when the texture is made). Not the same as the two tests
    /// above — those measure the curve itself, this measures it **after a render**: `render_hdr`
    /// produces >1 and the fold squeezes it into (0.9, 1.0) without any step pinning it to 1.0.
    #[test]
    fn hdr_preview_shows_highlights_that_sdr_pins_to_white() {
        let p = GradeParams {
            exposure: 3.0,
            ..Default::default()
        };
        let mut sdr = flat(0.55, 16 * 12 * 3);
        let mut hdr = sdr.clone();
        render(&mut sdr, 16, 12, &p, Ev::NONE);
        render_hdr(&mut hdr, 16, 12, &p, Ev::NONE);

        let sdr_max = sdr.iter().fold(0.0f32, |m, v| m.max(*v));
        let hdr_max = hdr.iter().fold(0.0f32, |m, v| m.max(*v));
        assert!(
            (sdr_max - 1.0).abs() < 1e-6,
            "SDR 预览的峰值是 {sdr_max:.4}，还没到白，这条靶子没有判别力"
        );
        assert!(
            hdr_max > 1.2,
            "HDR 渲染的峰值只有 {hdr_max:.4}，白以上没东西可折"
        );

        fold_above_white_in_place(&mut hdr);
        let folded = hdr.iter().fold(0.0f32, |m, v| m.max(*v));
        assert!(
            folded < 1.0 && folded > FOLD_KNEE,
            "折完峰值 {folded:.4}，不在 ({FOLD_KNEE}, 1.0) 里——要么没折，要么折没了"
        );
    }
}
