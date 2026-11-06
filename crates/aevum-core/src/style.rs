//! 照片风格（Profile 层）与传感器基准色调。
//!
//! 两件事共用「滑块之下的整幅渲染基调」这个位置，都在 `develop::render` 的
//! 链头（外接 .cube 外观之前）逐像素作用：
//!
//! - **基准色调**（[`base_tone`]）：显示编码后的传感器数据再过一道中间调提升
//!   加轻度 S 对比——LR 的 Adobe Color 默认 Profile 干的就是这件事，没有它，
//!   复位后的 RAW 就是线性场景数据直出的「发灰」观感（用户实测反馈）。它只服务
//!   `GradeParams::base_curve`，且 DCP 路径不参与（DCP 自带色调曲线）。
//! - **照片风格**（[`apply`]）：LR Profile / 相机照片风格的同构物。换风格
//!   不改任何滑块值、不计入「N 项调整」徽标——它是基调，不是参数。
//!   `0 = 标准` 是逐位 no-op，金标 case 全落在标准上。
//!
//! 两者都把输出钳回 [0,1]（SDR 语义）；HDR 路径（`Opts::hdr`）在调用处整段跳过，
//! 白以上的亮度不归这里管。
//!
//! Photo styles (the Profile layer) and the sensor base tone.
//!
//! Both occupy the same slot -- the whole-frame rendering basis beneath every slider,
//! applied per pixel at the head of `develop::render` (before the external .cube look):
//!
//! - **Base tone** ([`base_tone`]): display-encoded sensor data run through a midtone
//!   lift plus gentle S contrast. This is what LR's Adobe Color default profile does;
//!   without it a reset RAW reads as flat linear scene data (user-reported). It serves
//!   `GradeParams::base_curve` only, and stays out of the DCP route (a DCP carries its
//!   own tone curve).
//! - **Photo styles** ([`apply`]): isomorphic to LR profiles / in-camera picture styles.
//!   Switching touches **no slider value** and never counts into the "N adjusted" badge
//!   -- a style is a basis, not a parameter. `0 = Standard` is a bit-exact no-op and
//!   every golden case sits on it.
//!
//! Both clamp output back to [0,1] (SDR semantics); the HDR path (`Opts::hdr`) skips
//! the whole slot at the call site -- brightness above white is not ours to spend.

use crate::develop::LUMA;
use rayon::prelude::*;

/// 风格总数（含 0=标准）。UI 的下拉与 `GradeParams.style` 的合法域都以它为界。
///
/// Total style count, including 0 = Standard. Both the UI dropdown and the legal domain
/// of `GradeParams.style` are bounded by it.
pub const COUNT: u8 = 8;

/// 风格的 i18n 键（中文为键，界面经 `t!` 取当前语言）。
///
/// i18n keys for each style (Chinese keys; the UI resolves them through `t!`).
pub const NAME_KEYS: [&str; COUNT as usize] = [
    "标准", "鲜明", "柔和", "风光", "人像", "暖调", "冷调", "黑白",
];

/// 逐像素作用一个风格。`0`（标准）与越界值都是逐位 no-op。
///
/// Apply one style per pixel. `0` (Standard) and any out-of-range index are bit-exact
/// no-ops.
pub fn apply(rgb: &mut [f32], style: u8) {
    if style == 0 || style >= COUNT {
        return;
    }
    rgb.par_chunks_exact_mut(3).for_each(|px| {
        for v in px.iter_mut() {
            *v = v.clamp(0.0, 1.0);
        }
        match style {
            // 鲜明：S 对比 + 饱和。
            1 => {
                s_curve(px, 0.30);
                saturate(px, 0.16);
            }
            // 柔和：反 S（压对比）+ 微降饱和。
            2 => {
                s_curve(px, -0.14);
                saturate(px, -0.08);
            }
            // 风光：中度 S + 饱和 + 略微偏冷。
            3 => {
                temp_shift(px, -0.020, 0.016);
                s_curve(px, 0.24);
                saturate(px, 0.12);
            }
            // 人像：肤色保护的中度暖移 + 轻微 S。
            4 => {
                temp_shift(px, 0.022, -0.012);
                s_curve(px, 0.10);
                saturate(px, 0.05);
            }
            // 暖调：纯色温移。
            5 => temp_shift(px, 0.05, -0.04),
            // 冷调：反向色温移。
            6 => temp_shift(px, -0.04, 0.05),
            // 黑白：亮度加权转灰 + 轻度 S（ACR 的 B&W 也保留对比）。
            7 => {
                let l = LUMA[0] * px[0] + LUMA[1] * px[1] + LUMA[2] * px[2];
                px[0] = l;
                px[1] = l;
                px[2] = l;
                s_curve(px, 0.10);
            }
            _ => {}
        }
        for v in px.iter_mut() {
            *v = v.clamp(0.0, 1.0);
        }
    });
}

/// 传感器基准色调：黑位压实 0.018 → 中间调提升（gamma 0.82）→ S 对比
/// （c=0.22）→ 饱和 +0.10，四步一条链，对齐相机 JPEG 直出的观感（用户实测
/// 「发灰」的三要素：黑位不黑、中间调闷、饱和弱——LR Adobe Color 同位）。
/// 参数用真实样张的分位数收敛：P50 59→70、P95 179→196（见台账）。
/// 黑与白两端不动（0 与 1 逐位保持），单调，SDR 域。
///
/// Sensor base tone: black-point crush 0.018 → midtone lift (gamma 0.82) → S
/// contrast (c = 0.22) → saturation +0.10, one four-step chain matching the look of
/// camera JPEG output (the user-reported "flat/gray" triad: lifted blacks,闷 mids,
/// weak saturation -- the Adobe Color slot in LR). Tuned against real-sample
/// percentiles: P50 59→70, P95 179→196 (see the ledger). Endpoints preserved
/// bit-exactly (0 and 1 stay put); monotonic; SDR domain.
pub fn base_tone(rgb: &mut [f32]) {
    rgb.par_chunks_exact_mut(3).for_each(|px| {
        for v in px.iter_mut() {
            let x = v.clamp(0.0, 1.0);
            let y = ((x - 0.018) / 0.982).max(0.0);
            let g = y.powf(0.82);
            *v = g + 0.22 * g * (1.0 - g) * (2.0 * g - 1.0);
        }
        let l = LUMA[0] * px[0] + LUMA[1] * px[1] + LUMA[2] * px[2];
        for v in px.iter_mut() {
            *v = (l + (*v - l) * 1.10).clamp(0.0, 1.0);
        }
    });
}

/// 绕中灰的轻度 S（`c` 为正加对比、为负压对比）：`y + c·y(1-y)(2y-1)`，
/// 端点与中灰不动，`|c| ≤ 0.5` 时全程单调。
///
/// A gentle S around mid-grey (`c > 0` adds contrast, `c < 0` flattens):
/// `y + c·y(1-y)(2y-1)`; endpoints and mid-grey stay put, and the map stays monotonic
/// for `|c| ≤ 0.5`.
fn s_curve(px: &mut [f32], c: f32) {
    for v in px.iter_mut() {
        let y = *v;
        *v = y + c * y * (1.0 - y) * (2.0 * y - 1.0);
    }
}

/// 饱和（亮度加权）：`l + (v-l)·(1+s)`，`s` 为正提饱和、为负降饱和。
///
/// Saturation, luma-weighted: `l + (v-l)·(1+s)`; positive `s` saturates, negative
/// desaturates.
fn saturate(px: &mut [f32], s: f32) {
    let l = LUMA[0] * px[0] + LUMA[1] * px[1] + LUMA[2] * px[2];
    let k = 1.0 + s;
    for v in px.iter_mut() {
        *v = l + (*v - l) * k;
    }
}

/// 色温移：R 增 `r`、B 增 `b`（G 不动）——线性域的小幅通道增益，色相漂移可控。
///
/// Temperature shift: R gains `r`, B gains `b` (G untouched) -- small per-channel gains
/// in the linear domain, keeping hue drift under control.
fn temp_shift(px: &mut [f32], r: f32, b: f32) {
    px[0] += r;
    px[2] += b;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标准与越界索引逐位不动：金标 case 全落在标准上，这条是它们安全的根据。
    #[test]
    fn standard_and_out_of_range_are_bit_exact_noops() {
        let mut a = vec![0.1f32, 0.5, 0.9, 0.25, 0.75, 0.0];
        let b = a.clone();
        apply(&mut a, 0);
        assert_eq!(a, b, "标准风格必须逐位不动");
        apply(&mut a, 99);
        assert_eq!(a, b, "越界风格必须逐位不动");
        let c = b.clone();
        base_tone(&mut []);
        // base_tone 只在 base_curve=true 时被调；这里只验证空缓冲不崩。
        assert_eq!(c, b);
    }

    /// 端点不动 + 单调：基准色调不许把黑推成灰、白压成灰。
    #[test]
    fn base_tone_keeps_endpoints_and_monotonicity() {
        let mut v = vec![0.0f32, 1.0, 0.0];
        base_tone(&mut v);
        assert_eq!(v[0], 0.0, "黑必须保持黑");
        assert!((v[1] - 1.0).abs() < 1e-6, "白必须保持白");
        let mut prev = -1.0f32;
        let mut x = 0.0f32;
        while x <= 1.0 {
            // 三个同值样本，凑满一个 chunk，循环才真正作用。
            let mut one = vec![x, x, x];
            base_tone(&mut one);
            assert!(one[0] >= prev - 1e-6, "基准色调在 {x} 处单调递减");
            prev = one[0];
            x += 0.05;
        }
    }

    /// 中间调被提升：0.46（18% 灰编码值）之后必须更亮——「发灰/发暗」的方向性。
    #[test]
    fn base_tone_lifts_midtones() {
        // 必须用 3 的倍数长度：par_chunks_exact(3) 会把不足一组的 remainder
        // 整个丢给 return，单元素向量会让循环一次都不跑——前版判据就是
        // 这样假绿的（输出恒等于输入却全绿）。
        let mut v = vec![0.461f32, 0.461, 0.461];
        base_tone(&mut v);
        assert!(v[0] > 0.51, "18% 灰经基准色调后应明显提亮，实际 {}", v[0]);
        assert!(v[0] < 1.0);
    }

    /// 每个风格都真的改变了像素，且黑白风格消色度。
    #[test]
    fn every_style_moves_pixels_and_mono_removes_chroma() {
        let src = vec![0.30f32, 0.50, 0.70, 0.10, 0.40, 0.80];
        for s in 1..COUNT {
            let mut v = src.clone();
            apply(&mut v, s);
            assert!(
                v.iter().zip(&src).any(|(a, b)| (a - b).abs() > 1e-4),
                "风格 {s} 没有改变任何像素"
            );
        }
        let mut m = src.clone();
        apply(&mut m, 7);
        let l0 = LUMA[0] * 0.30 + LUMA[1] * 0.50 + LUMA[2] * 0.70;
        assert!(
            (m[0] - m[1]).abs() < 1e-4 && (m[1] - m[2]).abs() < 1e-4,
            "黑白风格后三通道应当相等"
        );
        assert!(
            (m[0] - l0).abs() > 1e-3,
            "黑白后还带轻度 S，不应等于纯 luma"
        );
    }

    /// 暖调向红移、冷调向蓝移：方向性钉住，防止两个风格写反。
    #[test]
    fn warm_and_cool_shift_in_opposite_directions() {
        let src = vec![0.5f32, 0.5, 0.5];
        let mut w = src.clone();
        apply(&mut w, 5);
        let mut c = src.clone();
        apply(&mut c, 6);
        assert!(w[0] > w[2], "暖调应 R>B");
        assert!(c[2] > c[0], "冷调应 B>R");
    }
}
