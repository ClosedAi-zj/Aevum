//! 显影主链：从镜头校正到晕影的完整像素流水线。
//!
//! 这里是 GUI 预览、静帧导出与视频导出唯一的真相来源，顺序与
//! Python oracle 的 `lapse/grade.py::_core` / `apply` 严格一致，逐张图由
//! `tests/develop_golden.rs` 与 `oracle/golden/develop.json` 比对。
//!
//! 顺序（照抄 Python，不要凭直觉调整）：
//! 镜头校正 →（外部 .cube 外观）→ 降噪 → 白平衡/曝光 → 对比度 → 去朦胧
//! → 高光/阴影/白色/黑色 → 锐化 → 清晰度 → 纹理 → 自然饱和度/饱和度
//! → 色彩分级 → HSL → 单色 → 色阶+参数曲线+曲线（合成一张表）
//! → 局部蒙版 → 颗粒 → 晕影
//!
//! `STAGES_READY` 是诚实开关：只有列在这里的阶段才真的参与运算，
//! `tests/develop_golden.rs` 据此决定哪些 case 必须逐像素对上、哪些只能跳过。
//! 实现一个新阶段就把它加进来，别在没实现时说"已经一致"。

use rayon::prelude::*;

use crate::grade::{GradeParams, VignetteParams, vignette_mask};
use crate::mask::{active_masks, apply_delta, luminance, uses_luminance};

pub mod dehaze;
pub mod lut;
pub mod spatial;

pub(crate) const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// 逐像素段并行的下限：一次任务至少吃这么多像素。
///
/// 主链的每段都是「每个像素独立」，分线程不改变任何数值，所以并行只赚不亏；
/// 但 LUT 网格（33³）和单测里的 4×4 图太小，拆到 4096 像素一档自然退回单线程。
pub(crate) const MIN_PIXELS: usize = 4096;

/// 已经落地、可以逐像素比对金标的阶段。
///
/// 整条链只剩"输入位深"和外部外观查表不在这里，主链每一段都有 case 压着。
pub const STAGES_READY: &[&str] = &[
    "tone",
    "lut",
    "spatial",
    "vignette",
    "mask",
    "mask_local",
    "grain",
];

#[derive(Clone, Copy, Debug, Default)]
pub struct Opts {
    /// 拖动草图：跳过最贵的空间运算（降噪、锐化、清晰度、纹理、局部空间项）。
    pub sketch: bool,
    /// 只留逐点运算，用来生成 .cube LUT（对应 Python 的 `for_lut=True`）。
    pub for_lut: bool,
    /// 局部蒙版内部的一条链：单色与全局专属项跳过。
    pub local: bool,
}

impl Opts {
    /// 导出与松手后的精修：全量。
    pub const FULL: Opts = Opts {
        sketch: false,
        for_lut: false,
        local: false,
    };
    /// 拖动中的草图。
    pub const SKETCH: Opts = Opts {
        sketch: true,
        for_lut: false,
        local: false,
    };

    fn detail(self) -> bool {
        !self.for_lut && !self.sketch && !self.local
    }
}

/// 逐帧曝光修正：整帧 EV 加分通道残差，都由去闪烁给出。
///
/// `chan` 只装色差（三通道均值已并进 `add`），所以全零时与只补亮度的
/// 老口径逐位一致。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ev {
    pub add: f32,
    pub chan: [f32; 3],
}

impl Ev {
    pub const NONE: Ev = Ev {
        add: 0.0,
        chan: [0.0; 3],
    };

    /// 三通道相同的修正（单通道去闪、或没有修正）。
    pub fn flat(add: f32) -> Self {
        Self {
            add,
            chan: [0.0; 3],
        }
    }
}

/// `rgb` 是 sRGB 编码、行优先的 `[0,1]` 浮点像素；`gain` 是去闪烁给的
/// 额外曝光（叠加在参数曝光上）。就地修改并夹到 `[0,1]`。
pub fn render(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams, gain: Ev, opts: Opts) {
    assert_eq!(rgb.len(), w * h * 3, "显影缓冲尺寸不对");

    // 镜头校正排在最前：几何与紫边先修掉，后面的局部对比才不会跟着畸变跑。
    // 配置文件校正（EXIF 匹配来的标定系数）先于手动三项：先把真实几何修直，
    // 手动滑杆才是创作量。
    if !opts.for_lut {
        if let Some(corr) = &p.lcp_profile {
            corr.apply(rgb, w, h);
        } else if let Some(corr) = &p.lens_profile {
            crate::lensdb::apply(rgb, w, h, corr);
        }
        spatial::lens(rgb, w, h, p);
    }

    // 外部 .cube 外观：进主链之前先套一层三维查表（放在这里与"外观"改的是
    // 输入而非输出的用法一致）。Python oracle 没有这一步，金标 case 的
    // look_cube 恒为空，所以不影响逐像素比对。
    if let Some(look) = crate::cube::active(p) {
        look.apply(rgb);
    }

    // 蒙版要拿"进主链之前"的那张图重跑局部链，所以先留一份。
    let masks = if opts.for_lut {
        Vec::new()
    } else {
        active_masks(p)
    };
    let original = if masks.is_empty() {
        None
    } else {
        Some(rgb.to_vec())
    };

    core(rgb, w, h, p, gain, opts);

    // 局部蒙版：拿同一张原图叠上该组的增量再算一遍，按权重混回来。
    if let Some(original) = original {
        let lum = uses_luminance(p).then(|| luminance(&original, w, h));
        for mut m in masks {
            if !m.has_point_delta() {
                continue;
            }
            m.build_field(w, h, lum.as_deref(), Some(&original));
            let q = apply_delta(p, &m.params);
            let local_opts = Opts {
                local: true,
                sketch: opts.sketch,
                for_lut: false,
            };
            let mut local = original.clone();
            core(&mut local, w, h, &q, gain, local_opts);
            rgb.par_chunks_exact_mut(3)
                .with_min_len(MIN_PIXELS)
                .zip(local.par_chunks_exact(3))
                .enumerate()
                .for_each(|(i, (px, lp))| {
                    let t = m.field[i];
                    if t <= 0.0 {
                        return;
                    }
                    for c in 0..3 {
                        px[c] += (lp[c] - px[c]) * t;
                    }
                });
        }
        rgb.par_iter_mut().for_each(|x| {
            *x = x.clamp(0.0, 1.0);
        });
        // 蒙版内的锐化/降噪/清晰度/纹理：Python 在混色之后单独补一遍。
        if !opts.sketch {
            spatial::mask_spatial(rgb, w, h, p, lum.as_deref(), &original);
        }
    }

    // 颗粒（空间噪声，不进 LUT）。
    if !opts.for_lut && p.grain != 0.0 {
        spatial::grain(rgb, w, h, p);
    }

    // 晕影在蒙版与颗粒之后：局部提亮不该把压暗的四角再拉回来。
    // 高光保护要的是这一步当前像素的亮度，只有开了参数才算。
    if !opts.for_lut && p.vignette != 0.0 {
        let params = VignetteParams::from(p);
        let lum = (params.hl_contrast != 0.0).then(|| luminance(rgb, w, h));
        if let Some(mask) = vignette_mask(w, h, &params, lum.as_deref()) {
            rgb.par_chunks_exact_mut(3)
                .with_min_len(MIN_PIXELS)
                .enumerate()
                .for_each(|(i, px)| {
                    let m = mask[i];
                    for x in px {
                        *x *= m;
                    }
                });
        }
    }

    rgb.par_iter_mut().for_each(|x| {
        *x = x.clamp(0.0, 1.0);
    });
}

pub(crate) fn srgb_to_lin(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

pub(crate) fn lin_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

pub(crate) fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 白场软拐点（抛物线肩部）：f(1-s)=1-s、f'(1-s)=1、f(1+s)=1、f'(1+s)=0，
/// C1 连续且恒 ≤1。宽 s = 0.08..0.25（black_soft/white_soft 参数映射，需拟合 TODO(fit)）。
pub(crate) fn soft_clip_high(x: f32, s: f32) -> f32 {
    if x <= 1.0 - s {
        x
    } else if x < 1.0 + s {
        let dd = x - (1.0 - s);
        x - dd * dd / (4.0 * s)
    } else {
        1.0
    }
}

/// 黑场软拐点：与 [`soft_clip_high`] 镜像，恒 ≥0。
pub(crate) fn soft_clip_low(x: f32, s: f32) -> f32 {
    if x >= s {
        x
    } else if x > -s {
        let dd = s - x;
        x + dd * dd / (4.0 * s)
    } else {
        0.0
    }
}

/// 影调 + 色彩主链，不含镜头校正、颗粒、晕影与蒙版混合。
fn core(rgb: &mut [f32], w: usize, h: usize, p: &GradeParams, gain: Ev, opts: Opts) {
    // 降噪在校正之前：先压掉噪点，后面拉曝光/清晰度才不会把它一起放大。
    // AI 降噪跑在经典降噪前面：它学的是原样噪点，先跑完剩下的残噪再交给
    // 引导滤波，两道叠着用比单独一道干净。0 时整段跳过，goldens 不受影响。
    if opts.detail() && p.nr_ai > 0.0 {
        crate::ai_denoise::apply(rgb, w, h, (p.nr_ai / 100.0) as f32);
    }
    if opts.detail() && (p.nr_luma != 0.0 || p.nr_color != 0.0) {
        spatial::denoise(rgb, w, h, p);
    }

    // 色温 / 色调 + 曝光：线性光里一次完成（亮度归一化，明暗不漂）。
    let t = (p.temperature / 100.0) as f32;
    let u = (p.tint / 100.0) as f32;
    let base = p.exposure as f32 + gain.add;
    let shifts = [
        base + gain.chan[0],
        base + gain.chan[1],
        base + gain.chan[2],
    ];
    // 曝光高光肩部（对齐 LR UniformsExposure 的 rolloff，E2；knee/len 需拟合
    // TODO(fit)）：有理肩部 t=0 处斜率 1 → C1 连续；按亮度比值缩放保色相
    // （atlas §9 明确警告逐通道曲线会偏色相）。k=0 且白平衡/曝光全 0 时整段
    // 跳过 = 旧路径逐位一致。
    let krl = (p.hl_rolloff / 100.0) as f32;
    let wb_active = t != 0.0 || u != 0.0 || shifts.iter().any(|s| *s != 0.0);
    if wb_active || krl != 0.0 {
        let mut gains = [1.0f32; 3];
        if wb_active {
            let mut g = [
                1.0 + 0.30 * t + 0.08 * u,
                1.0 - 0.22 * u,
                1.0 - 0.30 * t + 0.08 * u,
            ];
            let norm = g[0] * LUMA[0] + g[1] * LUMA[1] + g[2] * LUMA[2];
            for (c, gv) in g.iter_mut().enumerate() {
                *gv = *gv / norm * 2f32.powf(shifts[c]);
            }
            gains = g;
        }
        if krl != 0.0 {
            let knee = 0.75f32;
            let len = 0.40f32;
            // 这一段是全链最贵的逐像素项：每个通道两次 powf，1600 边预览上就要 140ms，
            // 所以非并行不可。像素之间互不相干，拆线程不改数值。
            rgb.par_chunks_exact_mut(3)
                .with_min_len(MIN_PIXELS)
                .for_each(|px| {
                    let lin = [
                        srgb_to_lin(px[0].clamp(0.0, 1.0)) * gains[0],
                        srgb_to_lin(px[1].clamp(0.0, 1.0)) * gains[1],
                        srgb_to_lin(px[2].clamp(0.0, 1.0)) * gains[2],
                    ];
                    let y = lin[0] * LUMA[0] + lin[1] * LUMA[1] + lin[2] * LUMA[2];
                    let mut s = 1.0f32;
                    if y > knee {
                        let tt = y - knee;
                        let yy = y + krl * tt / (1.0 + tt / len);
                        s = yy / y.max(1e-6);
                    }
                    for (c, lv) in lin.iter().enumerate() {
                        px[c] = lin_to_srgb(*lv * s);
                    }
                });
        } else {
            // 这一段是全链最贵的逐像素项：每个通道两次 powf，1600 边预览上就要 140ms，
            // 所以非并行不可。像素之间互不相干，拆线程不改数值。
            rgb.par_chunks_exact_mut(3)
                .with_min_len(MIN_PIXELS)
                .for_each(|px| {
                    for (c, g) in gains.iter().enumerate() {
                        let v = px[c].clamp(0.0, 1.0);
                        px[c] = lin_to_srgb(srgb_to_lin(v) * g);
                    }
                });
        }
    }

    // 对比度：中段变陡、两端压缩；只压 0~1，高光保留原值。
    // contrast_linear>0 时把完整映射搬进线性光域再回编码域取增量，与旧
    // gamma 域增量按 k 混合（对齐 LR PV2012 在线性光做影调运算的方向性口径，
    // E3/推断）。k=0 走旧路径逐位一致。
    let c = (p.contrast / 100.0) as f32;
    let ck = (p.contrast_linear / 100.0) as f32;
    if c != 0.0 && ck > 0.0 {
        rgb.par_iter_mut().for_each(|v| {
            let x = v.min(1.0);
            let xl = srgb_to_lin(x);
            let s_lin = xl * xl * (3.0 - 2.0 * xl);
            let delta_lin = lin_to_srgb(xl + c * (s_lin - xl)) - x;
            let delta_gam = c * (x * x * (3.0 - 2.0 * x) - x);
            *v += delta_gam * (1.0 - ck) + delta_lin * ck;
        });
    } else if c != 0.0 {
        rgb.par_iter_mut().for_each(|v| {
            let x = v.min(1.0);
            *v += c * (x * x * (3.0 - 2.0 * x) - x);
        });
    }

    // 去朦胧：正=下沉黑位，负=抬黑加雾。
    let dz = (p.dehaze / 100.0) as f32;
    if dz > 0.0 {
        let v = dz * 0.25;
        rgb.par_iter_mut().for_each(|x| {
            *x = (*x - v) / (1.0 - v);
        });
    } else if dz < 0.0 {
        let v = -dz;
        rgb.par_iter_mut().for_each(|x| {
            *x = *x * (1.0 - v * 0.20) + v * 0.07;
        });
    }

    // 暗通道先验去雾（He DCP + 大气散射）：空间感知的去雾，叠在旧逐点口径
    // 之后。sketch/local 跳过（与降噪/锐化同口径，opts.detail()）；for_lut 跳过
    // （空间运算不进 LUT）。dehaze_dark=0 整段跳过 = 旧路径逐位一致。
    if opts.detail() && p.dehaze_dark != 0.0 {
        dehaze::dehaze_dcp(
            rgb,
            w,
            h,
            (p.dehaze_dark / 100.0) as f32,
            (p.dehaze_contrast / 100.0) as f32,
        );
    }

    // 高光/阴影/白色/黑色：按亮度权重乘加，四段合成一次扫描。
    // black_soft/white_soft>0 时把段末的 [0,4] 硬钳换成抛物线软拐点（对齐
    // LR plusWhite/BlackClip 的硬软分离，E2；宽度需拟合 TODO(fit)）：直方图
    // 边缘不再堆尖，输出恒落 [0,1]、下游契约不变。两参数全 0 走原 clamp
    // 语句逐位一致。
    let s_b = if p.black_soft != 0.0 {
        (0.08 + 0.17 * (p.black_soft / 100.0)) as f32
    } else {
        0.0
    };
    let s_w = if p.white_soft != 0.0 {
        (0.08 + 0.17 * (p.white_soft / 100.0)) as f32
    } else {
        0.0
    };
    if p.highlights != 0.0
        || p.shadows != 0.0
        || p.whites != 0.0
        || p.blacks != 0.0
        || s_b != 0.0
        || s_w != 0.0
    {
        let a = (p.highlights / 100.0) as f32;
        let sh = (p.shadows / 100.0) as f32;
        let wh = (p.whites / 100.0) as f32;
        let bl = (p.blacks / 100.0) as f32;
        rgb.par_chunks_exact_mut(3)
            .with_min_len(MIN_PIXELS)
            .for_each(|px| {
                let lum = px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2];
                let mut fac = 1.0f32;
                let mut add = 0.0f32;
                if a != 0.0 {
                    fac *= 1.0 + a * 0.5 * smooth(0.45, 1.0, lum);
                }
                if sh != 0.0 {
                    let k = sh * 0.45 * smooth(0.55, 0.0, lum);
                    fac *= 1.0 - k;
                    add = add * (1.0 - k) + k * 1.2;
                }
                if wh != 0.0 {
                    let k = wh * 0.5 * smooth(0.60, 1.0, lum).powi(2);
                    fac *= 1.0 - k;
                    add = add * (1.0 - k) + k;
                }
                if bl != 0.0 {
                    add += bl * 0.35 * smooth(0.40, 0.0, lum).powi(2);
                }
                for x in px {
                    let v = *x * fac + add;
                    if s_b == 0.0 && s_w == 0.0 {
                        *x = v.clamp(0.0, 4.0);
                    } else {
                        // 单侧关闭时保持旧硬钳的对应半边（min 4 / max 0）。
                        let v = if s_w > 0.0 {
                            soft_clip_high(v, s_w)
                        } else {
                            v.min(4.0)
                        };
                        *x = if s_b > 0.0 {
                            soft_clip_low(v, s_b)
                        } else {
                            v.max(0.0)
                        };
                    }
                }
            });
    }

    // 锐化（USM + 边缘蒙版 + halo 限制）。
    if opts.detail() && p.sharp_amount != 0.0 {
        spatial::sharpen(rgb, w, h, p);
    }

    // 清晰度：大半径、边缘感知的局部对比。
    // 草图里跳过：这是全链最贵的一项之一，而拖动时看不出它的差别。
    if !opts.for_lut && !opts.sketch && p.clarity != 0.0 {
        spatial::local_contrast(
            rgb,
            w,
            h,
            (p.clarity / 100.0 * 1.6) as f32,
            clarity_radius(w, h),
            0.02,
            1.0,
        );
    }

    // 纹理：小半径局部对比。
    if !opts.for_lut && !opts.sketch && p.texture != 0.0 {
        spatial::local_contrast(
            rgb,
            w,
            h,
            (p.texture / 100.0) as f32,
            texture_radius(w, h),
            0.004,
            1.0,
        );
    }

    // 自然饱和度（低饱和像素提得多）→ 饱和度。
    let vib = (p.vibrance / 100.0) as f32;
    let sat = (p.saturation / 100.0) as f32;
    if vib != 0.0 || sat != 0.0 {
        rgb.par_chunks_exact_mut(3)
            .with_min_len(MIN_PIXELS)
            .for_each(|px| {
                for x in px.iter_mut() {
                    *x = x.clamp(0.0, 1.0);
                }
                let lum = px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2];
                let range = px.iter().copied().fold(f32::MIN, f32::max)
                    - px.iter().copied().fold(f32::MAX, f32::min);
                let mut f = 1.0 + sat;
                if vib != 0.0 {
                    f *= 1.0 + vib * (1.0 - range) * 1.3;
                }
                for x in px.iter_mut() {
                    *x = (lum + (*x - lum) * f).clamp(0.0, 1.0);
                }
            });
    }

    // 色彩分级 → HSL → 单色。
    lut::color_grade(rgb, p);
    lut::hsl(rgb, p);
    if !opts.local {
        lut::mono(rgb, p);
    }

    // 色阶 + 参数曲线 + 曲线：合成一张表，一次查完。
    lut::tone_curve(rgb, p);
}

/// 清晰度半径：Python `max(2, int(min(H, W) / 60))`。
fn clarity_radius(w: usize, h: usize) -> usize {
    ((w.min(h) / 60).max(2)).max(2)
}

/// 纹理半径：Python `max(1, int(min(H, W) / 300))`。
fn texture_radius(w: usize, h: usize) -> usize {
    (w.min(h) / 300).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grade::GradeParams;

    fn noisy_gray(w: usize, h: usize) -> Vec<f32> {
        let mut s: u32 = 9;
        let mut img = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = 0.4 + ((s as f32 / 4_294_967_296.0) - 0.5) * 0.06;
            img.extend_from_slice(&[v, v, v]);
        }
        img
    }

    fn luma_rms(a: &[f32], b: &[f32]) -> f32 {
        let d: f32 = a
            .iter()
            .zip(b)
            .step_by(3)
            .map(|(x, y)| (x - y) * (x - y))
            .sum();
        (d / (a.len() / 3) as f32).sqrt()
    }

    #[test]
    fn ai_denoise_is_opt_in_and_bites() {
        let (w, h) = (64, 48);
        let src = noisy_gray(w, h);
        let mut p = GradeParams::default();
        let mut off = src.clone();
        render(&mut off, w, h, &p, Ev::NONE, Opts::FULL);
        assert_eq!(off, src, "默认 nr_ai=0，整条链不该动像素");

        p.nr_ai = 100.0;
        let mut on = src.clone();
        render(&mut on, w, h, &p, Ev::NONE, Opts::FULL);
        let clean = vec![0.4f32; w * h * 3];
        assert!(
            luma_rms(&on, &clean) < luma_rms(&off, &clean) * 0.8,
            "AI 降噪没起作用"
        );
    }
}
