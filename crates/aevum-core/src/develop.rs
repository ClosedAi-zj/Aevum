//! 显影主链：从镜头校正到晕影的完整像素流水线。
//!
//! 这里是 GUI 预览、静帧导出与视频导出唯一的真相来源，顺序与
//! Python oracle 的 `acceptance/reference/grade.py::_core` / `apply` 严格一致，逐张图由
//! `tests/suite/develop_golden.rs` 与 `acceptance/oracle/golden/develop.json` 比对。
//!
//! 顺序（照抄 Python，不要凭直觉调整）：
//! 镜头校正 →（外部 .cube 外观）→ 降噪 → 白平衡/曝光 → 对比度 → 去朦胧
//! → 高光/阴影/白色/黑色 → 锐化 → 清晰度 → 纹理 → 自然饱和度/饱和度
//! → 色彩分级 → HSL → 单色 → 色阶+参数曲线+曲线（合成一张表）
//! → 局部蒙版 → 颗粒 → 晕影
//!
//! `STAGES_READY` 是诚实开关：只有列在这里的阶段才真的参与运算，
//! `tests/suite/develop_golden.rs` 据此决定哪些 case 必须逐像素对上、哪些只能跳过。
//! 实现一个新阶段就把它加进来，别在没实现时说"已经一致"。
//!
//! Develop main chain: the full pixel pipeline from lens correction to vignetting.
//!
//! This is the single source of truth for GUI preview, still export, and video export.
//! The order matches the Python oracle in `acceptance/reference/grade.py::_core` and `apply`
//! exactly, and every image is compared by `tests/suite/develop_golden.rs` against
//! `acceptance/oracle/golden/develop.json`.
//!
//! Order, copied from Python and not to be rearranged on intuition:
//! lens correction → (external .cube look) → denoise → white balance / exposure → contrast → dehaze
//! → highlights/shadows/whites/blacks → sharpening → clarity → texture → vibrance/saturation
//! → color grading → HSL → monochrome → levels + parametric curve + curve (one table)
//! → local masks → grain → vignetting
//!
//! `STAGES_READY` is an honesty switch: only the stages listed there take part in the computation,
//! and `tests/suite/develop_golden.rs` uses it to decide which cases must match pixel for pixel and which
//! may be skipped.
//! Adding a new stage means adding it there, not claiming "already consistent" before it is
//! implemented.

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
///
/// Stages that have landed and can be compared pixel for pixel against the golden.
///
/// Only input bit depth and the external look lookup remain outside the chain; every main-chain
/// stage is pinned by a case.
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
    ///
    /// Drag sketch: skips the most expensive spatial operations (denoise, sharpening, clarity,
    /// texture, and local spatial terms).
    pub sketch: bool,
    /// 只留逐点运算，用来生成 .cube LUT（对应 Python 的 `for_lut=True`）。
    ///
    /// Pointwise operations only, used to generate a .cube LUT (the Python `for_lut=True` case).
    pub for_lut: bool,
    /// 局部蒙版内部的一条链：单色与全局专属项跳过。
    ///
    /// A chain inside a local mask: monochrome and global-only terms are skipped.
    pub local: bool,
    /// HDR 输出：**不把 >1 截断**。
    ///
    /// 整条链在 sRGB 编码域上工作，各段默认把结果钳到 [0,1]——那是 SDR 的语义（显示
    /// 器只能到白）。但 HDR 导出需要白以上的部分真的存在：曝光、`hl_rolloff`、曲线
    /// 尾部都会造出 >1 的亮度，钳掉它们等于让 HDR 只剩到参考白为止，PQ 的上半段是
    /// 空的。这一位只为 HDR 打开，SDR 路径逐位不变。
    ///
    /// HDR output: **do not clamp values above 1**.
    ///
    /// The whole chain works in sRGB-encoded space where each stage clamps to [0,1] — that is
    /// SDR semantics (a display can only reach white). HDR export needs the region above white
    /// to actually exist: exposure, `hl_rolloff` and the curve tail all produce values above 1,
    /// and clamping them leaves HDR with nothing past reference white — the upper half of PQ
    /// would be empty. Only HDR sets this; the SDR path is bit-identical.
    pub hdr: bool,
}

impl Opts {
    /// 导出与松手后的精修：全量。
    ///
    /// Export and the refinement after the mouse is released: the full set.
    pub const FULL: Opts = Opts {
        sketch: false,
        for_lut: false,
        local: false,
        hdr: false,
    };
    /// 拖动中的草图。
    ///
    /// The sketch used while dragging.
    pub const SKETCH: Opts = Opts {
        sketch: true,
        for_lut: false,
        local: false,
        hdr: false,
    };
    /// HDR 导出：全量 + 不截断 >1。
    ///
    /// HDR export: the full set plus "do not clamp above 1".
    pub const HDR: Opts = Opts {
        sketch: false,
        for_lut: false,
        local: false,
        hdr: true,
    };

    fn detail(self) -> bool {
        !self.for_lut && !self.sketch && !self.local
    }
}

/// 逐帧曝光修正：整帧 EV 加分通道残差，都由去闪烁给出。
///
/// `chan` 只装色差（三通道均值已并进 `add`），所以全零时与只补亮度的
/// 老口径逐位一致。
///
/// Per-frame exposure correction: a frame-wide EV offset plus per-channel residuals, both supplied
/// by deflicker.
///
/// `chan` holds only the color difference, since the three-channel mean is already merged into
/// `add`, so when all channels are zero the result is bit-identical to the old convention that
/// corrected brightness only.
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
    ///
    /// The same correction for all three channels, as used by single-channel deflicker or by no
    /// correction at all.
    pub fn flat(add: f32) -> Self {
        Self {
            add,
            chan: [0.0; 3],
        }
    }
}

/// `rgb` 是 sRGB 编码、行优先的 `[0,1]` 浮点像素；`gain` 是去闪烁给的
/// 额外曝光（叠加在参数曝光上）。就地修改并夹到 `[0,1]`。
///
/// `rgb` holds sRGB-encoded, row-major `[0,1]` float pixels; `gain` is the extra exposure supplied
/// by deflicker, added on top of the parameter exposure.
/// The buffer is modified in place and clamped to `[0,1]`.
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

    // 几何与像素修补：两者都必须在调色之前。调色链是逐点的，局部对比、锐化、去噪都拿
    // 「邻域」当依据——梯形没拉正、灰尘还在，后面每一级都会把它们当结构强化一遍，
    // 那时候再修就得跟已经强化过的痕迹打架。.cube 外观排在它们之后，因为外观是创作层，
    // 用户想看到的是「已经修好的画面」被风格化，而不是「被风格化过的缺陷」。
    //
    // Geometry and pixel repair, both before grading. The grade chain is per-pixel but its
    // neighbourhood terms (clarity, sharpening, denoise) treat whatever is there as structure:
    // an unstraightened keystone or a dust speck gets reinforced by every stage below, and
    // fixing it afterwards means fighting the reinforced trace. The .cube look comes after them
    // because it is a creative layer — users want a repaired image stylized, not a stylized
    // defect.
    if !opts.for_lut {
        if let Some(q) = &p.upright {
            crate::perspective::rectify_in_place(rgb, w, h, q);
        }
        if !p.retouch.is_empty() {
            crate::retouch::apply(rgb, w, h, &p.retouch);
        }
    }

    // 渲染基调层（基准色调 + 照片风格，均在所有滑块之下，见 style 模块头）：
    // 基准色调只服务传感器路径（base_curve 且未挂 DCP——DCP 自带色调，叠上是
    // 双份）；风格是 LR Profile 的同构物，0=标准逐位不动。金标 case 两者皆关，
    // Python oracle 没有这一步，逐像素比对不受影响。HDR 路径整段跳过：
    // 两者都会钳到 [0,1]，会吃掉白以上。
    //
    // The rendering-basis layer (base tone + photo style, both beneath every slider;
    // see the style module header): the base tone serves the sensor route only
    // (base_curve and no DCP -- a DCP carries its own tone and stacking would double
    // it); a style is isomorphic to an LR Profile with 0 = Standard a bit-exact no-op.
    // Golden cases run with both off and the Python oracle has no such step, so the
    // per-pixel comparison is unaffected. The HDR path skips the whole slot: both
    // clamp to [0,1] and would eat the brightness above white.
    if !opts.hdr {
        if p.base_curve && p.dcp_profile.is_empty() {
            crate::style::base_tone(rgb);
        }
        crate::style::apply(rgb, p.style);
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
                hdr: opts.hdr,
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
        // 蒙版混色后按模式收口：SDR 钳到白，HDR 只压负值。
        let hdr = opts.hdr;
        rgb.par_iter_mut().for_each(|x| {
            *x = if hdr { x.max(0.0) } else { x.clamp(0.0, 1.0) };
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

    // 终点收口：SDR 钳到白（下游按 8bit 写盘），HDR 只压负值 —— 白以上的部分要
    // 留给 PQ 编码，那是 HDR 与 SDR 唯一的实质差别。
    //
    // Final clamp: SDR pins to white (it gets written as 8bit), HDR only floors negatives —
    // whatever sits above white has to reach the PQ encoder, and that is the only real
    // difference between HDR and SDR output.
    let hdr = opts.hdr;
    rgb.par_iter_mut().for_each(|x| {
        *x = if hdr { x.max(0.0) } else { x.clamp(0.0, 1.0) };
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

/// 线性光域的对比度 S 曲线：**绕中灰转**，不是绕线性 0.5。
///
/// 直接对 `xl` 做 smoothstep 会在线性 0.5 拐弯，而线性 0.5 对应编码 0.735——
/// 等于把对比度的支点抬到 73% 灰。实测 contrast=50 时中灰 0.5 被压暗到 0.4487，
/// 也就是拉满 `contrast_linear` 顺手吃掉 14 级（8 位）的中灰。
///
/// 这里用共轭：先把 `xl` 重标定成 `u = xl^γ`，使中灰（线性 ≈0.214）落到 u=0.5，
/// 做 smoothstep 再用 `u^(1/γ)` 拉回。于是 S(0)=0、S(中灰)=中灰、S(1)=1，
/// 中灰处斜率仍是 smoothstep 的 1.5——支点不动，阴影与高光是反方向拉开。
///
/// The contrast S-curve in linear light, pivoting **on mid-gray** rather than on linear 0.5.
///
/// A plain smoothstep on `xl` turns at linear 0.5, which is encoded 0.735 — that puts the
/// contrast pivot at 73% gray. Measured with contrast=50, mid-gray 0.5 was pushed down to
/// 0.4487, so turning `contrast_linear` up silently ate 14 levels (8-bit) of mid-gray.
///
/// This conjugates instead: re-scale `xl` to `u = xl^γ` so mid-gray (linear ≈0.214) lands on
/// u=0.5, run the smoothstep, then pull back with `u^(1/γ)`. S(0)=0, S(mid)=mid, S(1)=1, and
/// the slope at mid-gray is still smoothstep's 1.5 — the pivot stays put while shadows and
/// highlights move in opposite directions.
pub(crate) fn contrast_s_lin(xl: f32) -> f32 {
    // γ = ln(0.5) / ln(srgb_to_lin(0.5))，其中 srgb_to_lin(0.5) ≈ 0.2140。
    const GAMMA: f32 = 0.4496;
    const INV_GAMMA: f32 = 2.2242;
    let u = xl.max(0.0).powf(GAMMA);
    let s = u * u * (3.0 - 2.0 * u);
    s.powf(INV_GAMMA)
}

pub(crate) fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 白场软拐点（抛物线肩部）：f(1-s)=1-s、f'(1-s)=1、f(1+s)=1、f'(1+s)=0，
/// C1 连续且恒 ≤1、且单调不减（这三条是可证的，不是近似）。
///
/// 宽度 s 的取值范围 0.08…0.25 由两条边界定死：
/// - **上界 0.25**：`f(x)=x` 在 `x ≤ 1-s` 成立，所以 s=0.25 时编码 0.75（75% 灰）
///   以下**逐位不动**。再大就会开始压中间调，那就不是「软拐点」而是改对比度了。
/// - **下界 0.08**：最小档也得有可见效果。s=0.08 时编码 0.95 处被压 0.0028
///   （约 0.7 级 / 255）——再小就等于没开。
///
/// 见 `fit_sliders.rs::soft_clip_is_c1_and_width_bounds_are_meaningful`。
///
/// White soft knee (parabolic shoulder): f(1-s)=1-s, f'(1-s)=1, f(1+s)=1, f'(1+s)=0,
/// C1-continuous, bounded by 1 and monotonically non-decreasing — all three are provable, not
/// approximate.
///
/// The width range 0.08…0.25 is fixed by two boundaries: **the upper 0.25** because `f(x)=x`
/// holds for `x ≤ 1-s`, so s=0.25 leaves encoded 0.75 (75% gray) and below **bit-identical** —
/// any larger starts compressing midtones, which is a contrast change rather than a soft knee;
/// **the lower 0.08** because the smallest setting must still do something — at s=0.08, encoded
/// 0.95 is pulled down by 0.0028 (about 0.7 of a 255 level), and anything smaller is
/// indistinguishable from off.
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
    // 曝光高光肩部（对齐 LR UniformsExposure 的 rolloff，E2）：按亮度比值缩放保色相
    // （atlas §9 明确警告逐通道曲线会偏色相）。k=0 且白平衡/曝光全 0 时整段跳过 =
    // 旧路径逐位一致。
    //
    // 形状是 `yy = y + krl·t/(1+t/len)`，t = max(0, y−knee)。
    // **它不是 C1 连续**（这里以前写错了）：y ≤ knee 时斜率为 1，y 略高于 knee 时
    // 斜率跳到 1+krl，再随 t 增大回落到 1。所以 knee 处是一个折点，不是平滑衔接。
    // 保留这个折点是因为它符合「只把最亮的一小撮往上抬，中间调一根手指都不碰」的
    // 意图 —— 折点正好落在 knee 上，而 knee 选在 0.75 线性（编码 0.881），也就是
    // SDR 里最后一档高光的起点。
    //
    // knee / len 的取值依据：
    // - **knee = 0.75 线性 = 编码 0.881**：低于它的像素逐位不动（实测编码 0.875 不动、
    //   0.885 开始动，窗口只有 0.01 编码）。
    // - **len = 0.40**：饱和抬升量就是 `krl·len`。拉满（krl=1）时最多抬 0.40 线性，
    //   于是编码 1.0 的像素被推到线性 1.4（编码 1.06）—— 正好给 `curve_hdr_tail` 与
    //   HDR 编码提供白以上的亮度。len 太小（0.05）几乎无感，太大（10）等于整体过曝。
    //   实测窗口见 `fit_sliders.rs::hl_rolloff_knee_and_len_bounds`。
    //
    // Exposure highlight shoulder (matching LR's UniformsExposure rolloff, E2), scaled by the
    // luminance ratio to preserve hue (atlas §9 warns explicitly that per-channel curves shift
    // hue). With k=0 and no white balance or exposure this whole block is skipped, so old paths
    // stay bit-identical.
    //
    // The shape is `yy = y + krl·t/(1+t/len)` with t = max(0, y−knee). **It is not
    // C1-continuous** (this comment used to say it was): the slope is 1 below the knee, jumps to
    // 1+krl just above it, then decays back to 1. So the knee is a corner, not a smooth join.
    // That corner is kept because it matches the intent — lift only the brightest sliver and
    // leave midtones alone — and it sits exactly at a knee chosen as linear 0.75 (encoded
    // 0.881), the start of SDR's last highlight stop.
    //
    // Why those values: **knee = 0.75 linear = encoded 0.881** because everything below is
    // bit-identical (measured: encoded 0.875 unmoved, 0.885 moved — a 0.01 window);
    // **len = 0.40** because the saturated lift is `krl·len`, so a maxed slider lifts at most
    // 0.40 in linear — encoded 1.0 goes to linear 1.4 (encoded 1.06), which is exactly the
    // brightness above white that `curve_hdr_tail` and HDR encoding need. A tiny len (0.05) is
    // imperceptible, a huge one (10) is global overexposure.
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
            let s_lin = contrast_s_lin(xl);
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
    // LR plusWhite/BlackClip 的硬软分离，E2）：直方图边缘不再堆尖，输出恒落
    // [0,1]、下游契约不变。两参数全 0 走原 clamp 语句逐位一致。
    //
    // 宽度映射 `s = 0.08 + 0.17·(v/100)` 的两端与 [`soft_clip_high`] 那条注释同一
    // 依据：上端 0.25 保住 75% 灰以下不动，下端 0.08 保证最小档有约 0.7 级的可见
    // 作用。中间线性插值只是因为滑杆手感要线性，没有别的含义。
    //
    // The width mapping `s = 0.08 + 0.17·(v/100)` shares its bounds with [`soft_clip_high`]:
    // 0.25 keeps everything below 75% gray untouched, 0.08 keeps the smallest setting visible
    // at about 0.7 of a level. The linear interpolation in between is only there so the slider
    // feels linear; it carries no other meaning.
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
                // 饱和度按「相对亮度放大」算，超白的部分也要参与 —— 否则 HDR 的
                // 高光会被这一站先拍平，后面再怎么编码都是白的。
                //
                // Saturation scales around luminance, so the region above white must take part
                // too; otherwise this stage flattens HDR highlights before they are ever encoded.
                if opts.hdr {
                    for x in px.iter_mut() {
                        *x = x.max(0.0);
                    }
                } else {
                    for x in px.iter_mut() {
                        *x = x.clamp(0.0, 1.0);
                    }
                }
                let lum = px[0] * LUMA[0] + px[1] * LUMA[1] + px[2] * LUMA[2];
                let range = px.iter().copied().fold(f32::MIN, f32::max)
                    - px.iter().copied().fold(f32::MAX, f32::min);
                let mut f = 1.0 + sat;
                if vib != 0.0 {
                    f *= 1.0 + vib * (1.0 - range) * 1.3;
                }
                for x in px.iter_mut() {
                    let v = lum + (*x - lum) * f;
                    *x = if opts.hdr {
                        v.max(0.0)
                    } else {
                        v.clamp(0.0, 1.0)
                    };
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
    if opts.hdr {
        lut::tone_curve_keep_over(rgb, p);
    } else {
        lut::tone_curve(rgb, p);
    }
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
