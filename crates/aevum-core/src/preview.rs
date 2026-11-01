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

/// 拖动中的草图：跳过最贵的空间运算，保证跟手。
/// 无空间参数时与 [render] 逐点一致，有差异的那几项正是草图省掉的。
///
/// Sketch while dragging: skips the most expensive spatial operations to stay responsive.
/// With no spatial parameters it matches [render] per pixel; the few differing terms are
/// exactly what the sketch omits.
pub fn render_sketch(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams, gain: Ev) {
    develop::render(rgb, w, h, p, gain, Opts::SKETCH);
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
}
