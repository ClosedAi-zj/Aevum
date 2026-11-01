//! HDR 输出编码：把渲染出来的编码值送进 PQ（SMPTE ST 2084）与 10-bit 量化。
//!
//! 这条链只干一件事——**换传递函数**，不碰调色：
//!
//! ```text
//! 渲染输出（sRGB 编码值，可 >1）
//!   → srgb 编码 → 场景线性（>1 处按幂段连续延伸）
//!   → 乘参考白得到绝对亮度 nits
//!   → PQ 反向编码（0..1 相对信号）
//!   → 量化到 10-bit 码值
//! ```
//!
//! 三件事在这里定死，改之前先看 `tests/hdr_output.rs`：
//!
//! 1. **参考白 = 203 cd/m²**（BT.2408 的 HDR reference white / graphics white）。
//!    管线里的 `1.0` 是「SDR 白」，HDR 输出时它必须落在 203 nits 而不是 100 nits：
//!    100 nits 是早期 diffuse white 的说法，按它映射会让同一张图在 HDR 屏上比旁边
//!    的 SDR 窗口暗一档。选 203 之后，SDR 与 HDR 两版并排看亮度一致，高光才往上走。
//! 2. **不做色调映射滚降**。PQ 本身能表达到 10000 nits，把 >1 的部分按曲线尾部
//!    延伸进去正是这条链存在的理由；加一层自制滚降等于替用户决定高光该压多少，而
//!    且会把「曲线尾部滑杆」的效果又抹掉一半。峰值由显示器决定，不由我们猜。
//! 3. **码值是 full range**（`-color_range pc`），不是视频那套 64–940 的 limited
//!    range。AVIF 是图像容器，PC 保留 PQ 的黑白两端；实测同一份 10-bit 源，
//!    limited 往返最大差 178/65535，full 是 128/65535。
//!
//! HDR output encoding: feed the rendered encoded values into PQ (SMPTE ST 2084) and
//! 10-bit quantization.
//!
//! This chain does exactly one thing — **swap the transfer function** — and touches no
//! grading:
//!
//! ```text
//! render output (sRGB-encoded values, may exceed 1)
//!   → sRGB encoded → scene linear (the power segment extends past 1)
//!   → multiply by reference white to get absolute nits
//!   → inverse PQ (0..1 relative signal)
//!   → quantize to 10-bit codes
//! ```
//!
//! Three things are pinned here; read `tests/hdr_output.rs` before changing them:
//!
//! 1. **Reference white = 203 cd/m²** (BT.2408 HDR reference white / graphics white).
//!    `1.0` in this pipeline means "SDR white", and on HDR output it must land on 203
//!    nits rather than 100: 100 is the older diffuse-white convention, and mapping to it
//!    makes the same picture look one stop darker than the SDR window next to it. At 203
//!    the SDR and HDR versions match in brightness and only the highlights climb.
//! 2. **No tone-mapping roll-off.** PQ reaches 10000 nits on its own, and pushing values
//!    above 1 into it along the curve tail is the whole point of this path; a homemade
//!    roll-off would decide for the user how much highlight to crush and would partly
//!    cancel the curve-tail slider. The peak belongs to the display, not to a guess here.
//! 3. **Codes are full range** (`-color_range pc`), not the 64–940 limited range from
//!    video. AVIF is an image container, and full range keeps both ends of PQ. Measured
//!    on one 10-bit source: limited round-trips with a max error of 178/65535, full with
//!    128/65535.

/// PQ 的参考白：BT.2408 的 HDR reference white（graphics white）。
///
/// 管线里的 `1.0`（SDR 白）输出 HDR 时按这个亮度落点。见模块头第 1 条。
///
/// PQ reference white: BT.2408 HDR reference white (graphics white).
///
/// The pipeline's `1.0` (SDR white) lands on this luminance when written as HDR. See
/// item 1 in the module header.
pub const REFERENCE_WHITE_NITS: f32 = 203.0;

/// PQ 能表达的最大亮度（ST 2084 定义的绝对上限，不是显示器峰值）。
///
/// The maximum luminance PQ can express (the absolute ceiling in ST 2084, not the
/// display's peak).
pub const PQ_PEAK_NITS: f32 = 10_000.0;

// ST 2084 的四个常数，分母都写成 2 的幂（规范原样），不是随意的小数。
// m1 = 2610/16384，m2 = 2523/4096 × 128，c1 = 3424/4096，c2 = 2413/4096 × 32，
// c3 = 2392/4096 × 32。
const M1: f64 = 2610.0 / 16384.0;
const M2: f64 = 2523.0 / 4096.0 * 128.0;
const C1: f64 = 3424.0 / 4096.0;
const C2: f64 = 2413.0 / 4096.0 * 32.0;
const C3: f64 = 2392.0 / 4096.0 * 32.0;

/// 绝对亮度（nits）→ PQ 相对信号（0..1）。ST 2084 正向式。
///
/// 内部走 f64：10-bit 只有 1024 级，f32 在 `Y^m1` 那一段的舍入会在端点附近
/// 抖出 1 级，而这条式子要跟规范常数逐位对得上。
///
/// Absolute luminance (nits) → PQ relative signal (0..1), the ST 2084 forward form.
///
/// The body runs in f64: 10-bit has only 1024 levels, and f32 rounding in the `Y^m1`
/// segment wobbles by one level near the ends, while this formula must line up with the
/// published constants bit for bit.
pub fn nits_to_pq(nits: f32) -> f32 {
    let y = (f64::from(nits) / 10_000.0).clamp(0.0, 1.0);
    let ym = y.powf(M1);
    (((C1 + C2 * ym) / (1.0 + C3 * ym)).powf(M2)) as f32
}

/// PQ 相对信号（0..1）→ 绝对亮度（nits）。ST 2084 反向式。
///
/// PQ relative signal (0..1) → absolute luminance (nits), the ST 2084 inverse form.
pub fn pq_to_nits(code: f32) -> f32 {
    let e = f64::from(code).clamp(0.0, 1.0);
    let em = (e.powf(1.0 / M2) - C1).max(0.0);
    let den = C2 - C3 * e.powf(1.0 / M2);
    let y = if den > 0.0 { em / den } else { 0.0 };
    (y.powf(1.0 / M1) * 10_000.0) as f32
}

/// sRGB 编码值 → 线性光，**超过 1 的部分也连续**。
///
/// 标准式在 `x > 0.04045` 段是 `((x + 0.055) / 1.055)^2.4`，这一段对任意大的 `x`
/// 都有定义且光滑，所以不需要给 >1 单开一条规则——直接算下去就是它的自然延伸。
/// 负值压到 0：下游要进幂运算，负数会出 NaN。
///
/// sRGB-encoded value → linear light, **continuous above 1 as well**.
///
/// The standard branch for `x > 0.04045` is `((x + 0.055) / 1.055)^2.4`, defined and
/// smooth for any larger `x`, so values above 1 need no special rule — the same
/// expression is their natural extension. Negatives clamp to 0: what follows is a power
/// and a negative would produce NaN.
pub fn srgb_to_linear_ext(x: f32) -> f32 {
    if x <= 0.0 {
        0.0
    } else if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// 线性光 → sRGB 编码值，**超过 1 的部分也连续**（[`srgb_to_linear_ext`] 的反向）。
///
/// Linear light → sRGB-encoded value, **continuous above 1 as well** (the inverse of
/// [`srgb_to_linear_ext`]).
pub fn linear_to_srgb_ext(x: f32) -> f32 {
    if x <= 0.0 {
        0.0
    } else if x <= 0.003_130_8 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// 管线里的编码值 → PQ 信号，按 [`REFERENCE_WHITE_NITS`] 落地。
///
/// 这是导出路径唯一该调的函数：进来是渲染出来的 sRGB 编码值（可 >1），出去是
/// 0..1 的 PQ 相对信号，乘 1023 就是 10-bit 码值。
///
/// The pipeline's encoded value → PQ signal, anchored at [`REFERENCE_WHITE_NITS`].
///
/// This is the only function the export path should call: sRGB-encoded render output
/// (may exceed 1) comes in, a 0..1 PQ relative signal comes out, and times 1023 that is
/// the 10-bit code.
pub fn encoded_to_pq(x: f32) -> f32 {
    nits_to_pq(srgb_to_linear_ext(x) * REFERENCE_WHITE_NITS)
}

/// 三通道版 [`encoded_to_pq`]：原地换成 PQ 信号。
///
/// 逐通道做，没有色度处理——PQ 作用在 R'G'B' 上，色度由 Y'CbCr 那一步交给 ffmpeg
/// 的 bt2020nc 矩阵（我们喂 rgb48，它按 full range 转 limited-vs-full 自己定）。
///
/// Three-channel [`encoded_to_pq`]: rewrites in place.
///
/// Per channel, with no chromaticity work — PQ applies to R'G'B', and chroma is handed
/// to ffmpeg's bt2020nc matrix at the Y'CbCr step (we feed rgb48 and it handles the
/// range conversion).
pub fn encode_pq_in_place(rgb: &mut [f32]) {
    rgb.iter_mut().for_each(|v| *v = encoded_to_pq(*v));
}

/// 10-bit 量化：0..1 → 0..1023，四舍五入。
///
/// 不夹到 1023 之外：上游 [`nits_to_pq`] 已经按 10000 nits 封过顶。
///
/// 10-bit quantization: 0..1 → 0..1023, rounding.
///
/// Nothing clamps beyond 1023: [`nits_to_pq`] already caps at 10000 nits upstream.
pub fn to_u10(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 1023.0 + 0.5) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BT.2408 / ST 2084 的公开信号值：100 nits → 0.5081、203 → 0.5807、
    /// 1000 → 0.7518、10000 → 1.0。这几对是这条链的对外锚点。
    ///
    /// 没写 10 nits 那档：它的值（0.2983）不是规范里的公开锚点，是我自己算的，
    /// 拿来当断言只会把「我记得的数」钉进测试。
    ///
    /// The published signal values (BT.2408 / ST 2084): 100 nits → 0.5081, 203 → 0.5807,
    /// 1000 → 0.7518, 10000 → 1.0. These pairs anchor the whole chain.
    ///
    /// The 10-nit tier is left out: its value (0.2983) is not a published anchor but my
    /// own arithmetic, and asserting it would pin down "the number I remember".
    #[test]
    fn pq_matches_the_published_anchors() {
        for (nits, want) in [
            (100.0f32, 0.5081f32),
            (203.0, 0.5807),
            (1000.0, 0.7518),
            (10_000.0, 1.0),
        ] {
            let got = nits_to_pq(nits);
            assert!(
                (got - want).abs() < 5e-4,
                "{nits} nits 应当是 {want:.4}，实测 {got:.4}"
            );
        }
    }

    /// 往返：`pq_to_nits(nits_to_pq(x)) == x`，相对误差 < 1e-4。
    ///
    /// Round trip: `pq_to_nits(nits_to_pq(x)) == x` within 1e-4 relative.
    #[test]
    fn pq_round_trips() {
        for nits in [
            0.0f32, 0.01, 0.1, 1.0, 100.0, 203.0, 1000.0, 4000.0, 10_000.0,
        ] {
            let back = pq_to_nits(nits_to_pq(nits));
            let scale = nits.max(1.0);
            assert!(
                (back - nits).abs() < 1e-4 * scale,
                "{nits} nits 往返成 {back}"
            );
        }
    }

    /// 参考白：编码值 1.0 必须落在 203 nits，也就是 PQ 0.5807（10-bit 594）。
    ///
    /// 这条是整个 HDR 路径的亮度原点。改成 100 nits 的话同一张图会暗一档，
    /// 而 SDR 与 HDR 两版并排看就会明显不一样。
    ///
    /// Reference white: encoded 1.0 must land on 203 nits, i.e. PQ 0.5807 (10-bit 594).
    ///
    /// This is the brightness origin of the whole HDR path. At 100 nits the same picture
    /// goes one stop darker, and the SDR and HDR versions no longer match side by side.
    #[test]
    fn sdr_white_lands_on_reference_white() {
        let pq = encoded_to_pq(1.0);
        assert!(
            (pq - 0.5807).abs() < 5e-4,
            "编码 1.0 应当落在 PQ 0.5807，实测 {pq:.4}"
        );
        assert_eq!(to_u10(pq), 594, "10-bit 码值应当是 594");
        let nits = pq_to_nits(pq);
        assert!(
            (nits - REFERENCE_WHITE_NITS).abs() < 0.5,
            "解码回亮度应当是 203 nits，实测 {nits:.1}"
        );
    }

    /// 黑是黑、负值不出 NaN、单调不回头。
    ///
    /// 负值出现在蒙版边缘与锐化过冲处，直接进幂运算就是 NaN，而 NaN 会让
    /// ffmpeg 整帧失败。
    ///
    /// 注意 PQ 的数学零点不是 0：0 nits 代进正向式得到 7.3e-7（`c1^m2`），因为
    /// 规范把绝对零留在了曲线外。这是 ST 2084 本来的行为，不是我们的偏差——量化到
    /// 10-bit 后它就是 0 码值。
    ///
    /// Black stays black, negatives do not produce NaN, and the curve never turns back.
    ///
    /// Negative values show up at mask edges and sharpening overshoot; fed straight into
    /// a power they become NaN, and one NaN fails the whole frame in ffmpeg.
    ///
    /// Note that PQ's mathematical zero is not 0: 0 nits through the forward form gives
    /// 7.3e-7 (`c1^m2`) because the spec leaves absolute zero off the curve. That is ST
    /// 2084's own behaviour, not our drift — quantized to 10-bit it is code 0 anyway.
    #[test]
    fn black_and_negatives_are_safe() {
        assert!(
            encoded_to_pq(0.0) < 1e-6,
            "0 nits 的信号应当在 1e-6 以下，实测 {}",
            encoded_to_pq(0.0)
        );
        assert_eq!(to_u10(encoded_to_pq(0.0)), 0, "0 nits 必须量化成 0 码值");
        assert_eq!(encoded_to_pq(-0.5), encoded_to_pq(0.0), "负值应当压成 0");
        assert_eq!(to_u10(encoded_to_pq(-3.0)), 0);
        let mut prev = -1.0f32;
        for step in 0..=1000 {
            let x = step as f32 / 200.0; // 0..5，覆盖 +2.3EV 的高光
            let pq = encoded_to_pq(x);
            assert!(pq.is_finite(), "x={x} 出了非有限值");
            assert!(pq >= prev - 1e-7, "x={x} 让 PQ 回头了：{pq} < {prev}");
            prev = pq;
        }
    }

    /// 高光真的往上走：+2EV 的编码值必须显著高于参考白，而不是被压平。
    ///
    /// 这条是「不做滚降」那个决定的判据。如果哪天有人加了一层自制的软膝，
    /// 这里会立刻报出来。
    ///
    /// Highlights really climb: a +2EV encoded value must sit clearly above reference
    /// white instead of being flattened.
    ///
    /// This pins the "no roll-off" decision. If someone adds a homemade soft knee, this
    /// test reports it immediately.
    #[test]
    fn highlights_climb_above_reference_white() {
        // 编码 2.0 → 线性 4.95 → 1006 nits → PQ 0.752；编码 8.0 → 线性 131 →
        // 26700 nits，撞上 PQ 的 10000 nits 上限顶到 1.0。
        let two = encoded_to_pq(2.0);
        assert!(
            two > encoded_to_pq(1.0) + 0.10,
            "+2 档编码值应当明显高于参考白，实测 {two:.4}"
        );
        assert!(
            (two - 0.752).abs() < 0.02,
            "编码 2.0 应当落在 PQ 0.752 附近，实测 {two:.4}"
        );
        assert_eq!(
            encoded_to_pq(20.0),
            1.0,
            "远超 10000 nits 的输入应当顶到 PQ 上限"
        );
    }

    /// sRGB 扩展式的往返与连续：>1 段不许有跳变。
    ///
    /// Round trip and continuity of the extended sRGB form: no jump above 1.
    #[test]
    fn extended_srgb_stays_continuous() {
        for x in [0.0f32, 0.003, 0.04045, 0.5, 1.0, 1.5, 2.0, 8.0] {
            let back = linear_to_srgb_ext(srgb_to_linear_ext(x));
            assert!((back - x).abs() < 1e-5, "编码 {x} 往返成 {back}");
        }
        // 越过 1 的地方逐点检查相邻差没有突变（线性段与幂段衔接处最容易出坑）。
        let mut prev = srgb_to_linear_ext(0.98);
        for step in 0..=200 {
            let x = 0.99 + step as f32 * 0.01;
            let y = srgb_to_linear_ext(x);
            assert!(y > prev, "x={x} 处不单调");
            assert!(y - prev < 0.6, "x={x} 处出现了跳变：{prev} → {y}");
            prev = y;
        }
    }
}
