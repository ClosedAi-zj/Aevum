//! 暗通道先验去雾（He DCP 2010/2011 公开数学 + E1 结构事实）。
//!
//! 设计文档：`docs/算法里程碑设计-2026-10-30.md` §2。语义以 Python
//! `acceptance/reference/grade.py::_dehaze_dcp` 为准，逐式对应；金标 case（48×64）只覆盖
//! `guided` 的全分辨率分支，下采样分支由本模块单测
//! `dehaze_downsample_matches_python` 显式覆盖（Python 对照级验证，非逐像素冻结）。
//!
//! 数值安全：大气光 A 下限 1e-3、`max(t, t0)` 防除零（对齐 E1「I+offset<1/256
//! 单独分支」的意图）。`t0 / eps_t / w(0.95)` 全部是编译期常量起步并标注
//! TODO(fit)，不做滑杆暴露（对齐 LR 单滑杆 UI）。

use rayon::prelude::*;

use super::spatial::{guided, lum_field, refl101};
use super::{lin_to_srgb, srgb_to_lin};

/// 透射下限（E1 uniform，约 0.1 量级，TODO(fit)）。
const T0: f32 = 0.1;
/// He 公开口径的去雾强度因子 w = 0.95。
const W_HAZE: f32 = 0.95;
/// 引导滤波精修的 eps（TODO(fit)）。
const EPS_T: f32 = 1e-4;
/// 大气光分量的防除零下限。
const AIRLIGHT_FLOOR: f32 = 1e-3;

/// 去雾的中间产物（接口预留，设计 §2.8）：透射图 + 大气光。对齐 LR 的
/// `dehaze_transmissionMapBuf`/`dehaze_localMaskBuf`（E2：LR 用透射图做局部
/// 蒙版约束/深度资源）。本轮不落公开 API，`dehaze_dcp_with_maps` 仅供将来
/// 局部蒙版接线时取用，字段暂无读取方。
#[allow(dead_code)]
pub(crate) struct DehazeMaps {
    /// 精修后的透射场 t''，钳在 [t0, 1]。
    pub transmission: Vec<f32>,
    /// 大气光单点估计（线性域 RGB）。
    pub airlight: [f32; 3],
}

/// 3×3 最小值滤波（BORDER_REFLECT_101，与盒滤波同一边界口径）。
fn min3x3(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let mut best = f32::MAX;
            for dy in -1isize..=1 {
                let iy = refl101(y as isize + dy, h);
                for dx in -1isize..=1 {
                    let ix = refl101(x as isize + dx, w);
                    best = best.min(src[iy * w + ix]);
                }
            }
            *v = best;
        }
    });
    out
}

/// 半径 r 的最小滤波：两遍 3×3 最小迭代（可分离思想），⌈r/2⌉ 轮。
/// **有效半径 = ⌈r/2⌉**：每轮 3×3 最小只把极值向外扩 1px，r 轮才是严格意义
/// 的 r 半径最小滤波——两边（Python `_min_filter`）同构取 ⌈r/2⌉ 轮，语义钉死
/// 在这里，TODO(fit) 时再评估改成 r 轮。
/// 与 Python `_min_filter` 逐式对应（同样的轮数与边界口径）。
fn min_filter(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let rounds = r.div_ceil(2);
    let mut cur = src.to_vec();
    for _ in 0..rounds {
        cur = min3x3(&cur, w, h);
    }
    cur
}

/// 分位数（numpy `np.percentile` 默认的 linear 插入口径）：排序后
/// `pos = q/100·(n−1)` 线性插值。
fn percentile(values: &[f32], q: f64) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    if v.len() == 1 {
        return v[0];
    }
    let pos = (q / 100.0) * (v.len() - 1) as f64;
    let i = pos.floor().max(0.0) as usize;
    let i = i.min(v.len() - 1);
    let frac = (pos - i as f64) as f32;
    let i2 = (i + 1).min(v.len() - 1);
    v[i] + (v[i2] - v[i]) * frac
}

/// 暗通道先验去雾核心（设计 §2.3 伪代码的逐式实现）：
///
/// 1. 大气光 A：归一化前暗通道最亮的 0.1% 像素里取线性亮度最高者的 RGB
///    （单点估计，下限 1e-3）；
/// 2. 归一化暗通道 `m = min(L_c/A_c)` + 最小滤波（E1：先除 A 再 min）；
/// 3. 透射初值 `t = clamp(1 − 0.95·dark, t0, 1)`；
/// 4. 引导滤波精修（E1 线性模型 a·I+b，引导图 = 线性亮度）；
/// 5. 合成 `J = (L − A)/max(t'', t0) + A`，按 amount 增量混合；
/// 6. 对比微调（`contrast ∈ [-0.5, 0.5]`）：输出亮度 [p1,p99] 的黑位/白位
///    小范围重映射，幅度 = contrast·0.15（TODO(fit)），contrast=0 时严格恒等。
///
/// `amount <= 0` 整段跳过 = 旧路径逐位一致。去雾在**线性域**做（E3 管线事实），
/// 输入钳到 [0,1] 后转线性，返回 sRGB 编码并钳到 [0,1]。
pub(crate) fn dehaze_dcp(rgb: &mut [f32], w: usize, h: usize, amount: f32, contrast: f32) {
    let _ = dehaze_dcp_with_maps(rgb, w, h, amount, contrast, false);
}

/// [`dehaze_dcp`] 的完整形态：`want_maps` 时一并交出透射图与大气光（接口预留）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn dehaze_dcp_with_maps(
    rgb: &mut [f32],
    w: usize,
    h: usize,
    amount: f32,
    contrast: f32,
    want_maps: bool,
) -> Option<DehazeMaps> {
    if amount <= 0.0 || w == 0 || h == 0 {
        return None;
    }
    let n = w * h;

    // 线性域（E3 管线事实：WB/曝光/去雾同段，避免 gamma 域放大噪声）。
    let lin: Vec<f32> = rgb
        .par_chunks_exact(3)
        .flat_map_iter(|p| {
            [
                srgb_to_lin(p[0].clamp(0.0, 1.0)),
                srgb_to_lin(p[1].clamp(0.0, 1.0)),
                srgb_to_lin(p[2].clamp(0.0, 1.0)),
            ]
        })
        .collect();
    let lum = lum_field(&lin);
    let dark0: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|i| lin[i * 3].min(lin[i * 3 + 1]).min(lin[i * 3 + 2]))
        .collect();

    // 1) 大气光 A：暗通道最亮的 0.1% 里取亮度最高者（并列取首个，与 numpy
    //    argmax 一致）；分量下限 1e-3。
    let k = (((n as f64) * 0.001).ceil() as usize).max(1);
    let mut sorted = dark0.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let thr = sorted[n - k.min(n)];
    let mut best = usize::MAX;
    let mut best_lum = f32::MIN;
    for i in 0..n {
        if dark0[i] >= thr && lum[i] > best_lum {
            best_lum = lum[i];
            best = i;
        }
    }
    let best = best.min(n - 1);
    let a = [
        lin[best * 3].max(AIRLIGHT_FLOOR),
        lin[best * 3 + 1].max(AIRLIGHT_FLOOR),
        lin[best * 3 + 2].max(AIRLIGHT_FLOOR),
    ];

    // 2) 归一化暗通道 + 最小滤波；3) 透射初值。
    let r_d = (w.min(h) / 200).max(2);
    let m: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|i| {
            (lin[i * 3] / a[0])
                .min(lin[i * 3 + 1] / a[1])
                .min(lin[i * 3 + 2] / a[2])
        })
        .collect();
    let dark = min_filter(&m, w, h, r_d);
    let t_init: Vec<f32> = dark
        .par_iter()
        .map(|d| (1.0 - W_HAZE * d).clamp(T0, 1.0))
        .collect();

    // 4) 引导滤波精修（E1 结构：a·I+b，引导图 = 灰度即线性亮度）。
    let r_t = 4 * r_d;
    let t_ref = guided(&lum, &t_init, w, h, r_t, EPS_T);
    let t: Vec<f32> = t_ref.par_iter().map(|v| v.clamp(T0, 1.0)).collect();

    // 5) 合成 + 按 amount 增量混合。
    let mut l_out: Vec<f32> = (0..n * 3)
        .into_par_iter()
        .map(|j| {
            let i = j / 3;
            let c = j % 3;
            let j_v = (lin[j] - a[c]) / t[i].max(T0) + a[c];
            lin[j] + amount * (j_v - lin[j])
        })
        .collect();

    // 6) 对比微调：输出亮度 [p1,p99] 的黑位/白位重映射（contrast=0 时整段
    //    跳过 = 严格恒等）。幅度 = contrast·0.15，TODO(fit)。
    if contrast != 0.0 {
        let mm = contrast * 0.15;
        let lum_out = lum_field(&l_out);
        let p1 = percentile(&lum_out, 1.0);
        let p99 = percentile(&lum_out, 99.0);
        let lo = mm * p1;
        let hi = 1.0 - mm * (1.0 - p99);
        let span = (hi - lo).max(1e-4);
        l_out.par_iter_mut().for_each(|v| {
            *v = ((*v - lo) / span).clamp(0.0, 1.0);
        });
    }

    // 回写 sRGB 并钳到 [0,1]（主链契约）。
    rgb.par_chunks_exact_mut(3).enumerate().for_each(|(i, px)| {
        px[0] = lin_to_srgb(l_out[i * 3]).clamp(0.0, 1.0);
        px[1] = lin_to_srgb(l_out[i * 3 + 1]).clamp(0.0, 1.0);
        px[2] = lin_to_srgb(l_out[i * 3 + 2]).clamp(0.0, 1.0);
    });

    if want_maps {
        Some(DehazeMaps {
            transmission: t,
            airlight: a,
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 单测：无雾稳定性、amount=0 短路、去雾有效性、下采样分支的 Python 对照
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪随机 [0,1)（与 spatial.rs 测试同一 LCG）。
    fn lcg_next(state: &mut u64) -> f32 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((*state >> 33) as f32) / (1u64 << 31) as f32
    }

    /// 带雾合成图：场景（阶梯 + 渐变）与大气光按透射场混合，物理口径
    /// `I = J·t + A·(1−t)`（线性域），转回 sRGB。
    fn hazy_scene(w: usize, h: usize, airlight: [f32; 3]) -> Vec<f32> {
        let mut s = 0xC0FF_EE12_3456_789Au64;
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let u = x as f32 / (w - 1) as f32;
                let v = y as f32 / (h - 1) as f32;
                // 场景：横向阶梯 + 纵向渐变 + 少量纹理，恒在 [0.05, 0.9]。
                let scene = [
                    0.15 + 0.6 * (u > 0.4) as i32 as f32 + 0.1 * v,
                    0.20 + 0.5 * v,
                    0.25 + 0.4 * (u > 0.7) as i32 as f32 + 0.1 * (1.0 - v),
                ];
                // 透射：远处（上方）雾重。
                let t = 0.25 + 0.6 * v + 0.05 * (lcg_next(&mut s) - 0.5);
                for c in 0..3 {
                    let lin_j = srgb_to_lin(scene[c].clamp(0.0, 1.0));
                    let lin_i = lin_j * t + airlight[c] * (1.0 - t);
                    rgb.push(lin_to_srgb(lin_i.clamp(0.0, 1.0)));
                }
            }
        }
        rgb
    }

    #[test]
    fn dehaze_noop_at_zero_amount() {
        let (w, h) = (64, 48);
        let src = hazy_scene(w, h, [0.8, 0.82, 0.85]);
        let mut a = src.clone();
        dehaze_dcp(&mut a, w, h, 0.0, 0.3);
        for (i, (x, y)) in a.iter().zip(src.iter()).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "amount=0 时第 {i} 位被改动");
        }
    }

    #[test]
    fn dehaze_constant_image_is_stable() {
        // 无雾平坦图：A 恰好等于像素色 → J = L，输出与输入一致（sRGB 往返的
        // 1ulp 内）。这是「A 估计在无雾图上误触发」风险的兜底验证。
        let (w, h) = (64, 48);
        let mut src = vec![0.0f32; w * h * 3];
        for i in 0..w * h {
            src[i * 3] = 0.4;
            src[i * 3 + 1] = 0.45;
            src[i * 3 + 2] = 0.5;
        }
        let mut a = src.clone();
        dehaze_dcp(&mut a, w, h, 1.0, 0.0);
        for (i, (x, y)) in a.iter().zip(src.iter()).enumerate() {
            assert!((x - y).abs() < 1e-5, "平坦图第 {i} 像素漂移 {x} vs {y}");
        }
    }

    #[test]
    fn dehaze_reduces_haze_on_hazy_scene() {
        // 带雾合成图去雾后：暗通道整体下降（雾被抽走）、对比度上升。
        let (w, h) = (96, 64);
        let src = hazy_scene(w, h, [0.75, 0.78, 0.82]);
        let mut a = src.clone();
        dehaze_dcp(&mut a, w, h, 0.8, 0.0);
        let dark = |img: &[f32]| -> (f32, f32) {
            let mut mn = 0.0f32;
            let mut mx = 0.0f32;
            for i in 0..w * h {
                let d = img[i * 3].min(img[i * 3 + 1]).min(img[i * 3 + 2]);
                mn += d;
                mx = mx.max(d);
            }
            (mn / (w * h) as f32, mx)
        };
        let (m0, x0) = dark(&src);
        let (m1, x1) = dark(&a);
        assert!(m1 < m0 * 0.9, "去雾后暗通道均值没降：{m0} → {m1}");
        assert!(x1 < x0, "去雾后暗通道峰值没降：{x0} → {x1}");
        for v in &a {
            assert!((0.0..=1.0).contains(v), "去雾输出越界 {v}");
        }
        // 对比微调：contrast>0 进一步拉开（黑位下拉），contrast<0 反向。
        let mut c_pos = src.clone();
        dehaze_dcp(&mut c_pos, w, h, 0.8, 0.5);
        let mut c_neg = src.clone();
        dehaze_dcp(&mut c_neg, w, h, 0.8, -0.5);
        let mean = |img: &[f32]| -> f32 { img.iter().step_by(3).sum::<f32>() / (w * h) as f32 };
        assert!(
            mean(&c_pos) < mean(&a) && mean(&c_neg) > mean(&a),
            "对比微调方向反了：{} / {} / {}",
            mean(&c_pos),
            mean(&a),
            mean(&c_neg)
        );
    }

    /// 下采样分支的 Python 对照（设计 §6 风险处置）：金标 48×64 不触发
    /// `guided` 的 r>=8 下采样路径，这里用 96×128（min=96 ≥ 64，r_t = 8 → s=4）
    /// 的合成图，与冻结时同解释器跑出的 Python 参考统计量对拍（容差 2e-3，
    /// Python 对照级验证，非逐像素冻结——与 guided_downsample 惯例一致）。
    #[test]
    fn dehaze_downsample_matches_python() {
        let (w, h) = (128, 96);
        let src = hazy_scene(w, h, [0.75, 0.78, 0.82]);
        let mut a = src.clone();
        dehaze_dcp(&mut a, w, h, 0.6, 0.2);
        // Python 参考（acceptance/oracle/dehaze_downsample_ref.py，与本测试 hazy_scene
        // 同款确定性图 + reference.grade._dehaze_dcp）。噪声源是 LCG（与
        // spatial.rs 测试同一个混合同余）：
        //   s = s·6364136223846793005 + 1442695040888963407（u64 回绕，
        //   种子 0xC0FFEE1234567890），每像素抽一次 r = (s>>33)/2^31。
        //   场景/透射/大气光与 hazy_scene 逐式相同。
        // 期望统计量（acceptance/oracle/dehaze_downsample_ref.py，f64 累计）：
        //   mean = [0.6817661, 0.6422953, 0.5528787]
        //   std  = [0.1958477, 0.0611306, 0.1444934]
        let want_mean = [0.6817661f32, 0.6422953, 0.5528787];
        let want_std = [0.1958477f32, 0.0611306, 0.1444934];
        for c in 0..3 {
            let vals: Vec<f64> = a.iter().skip(c).step_by(3).map(|v| *v as f64).collect();
            let n = vals.len() as f64;
            let mean = (vals.iter().sum::<f64>() / n) as f32;
            let var = vals
                .iter()
                .map(|v| (v - mean as f64) * (v - mean as f64))
                .sum::<f64>()
                / n;
            let std = (var as f32).sqrt();
            assert!(
                (mean - want_mean[c]).abs() < 2e-3,
                "通道{c} mean {mean} vs Python {}",
                want_mean[c]
            );
            assert!(
                (std - want_std[c]).abs() < 2e-3,
                "通道{c} std {std} vs Python {}",
                want_std[c]
            );
        }
    }
}
