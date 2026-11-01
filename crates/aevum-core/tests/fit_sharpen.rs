//! 锐化参数的扫参与验收。
//!
//! 两件事放在同一个文件里，因为它们是同一个闭环的两半：
//!
//! - `#[ignore]` 的扫参测试：打印参数 → 指标的表，人看表定值；
//! - 默认跑的验收测试：把定下来的值钉在 `docs/adobe-algorithm-atlas.md`
//!   §46.2 的阈值上（MTF50 提升 1.2–1.6×，halo < 3%）。
//!
//! 跑法：
//! ```text
//! cargo test -p aevum-core --test fit_sharpen -- --ignored --nocapture   # 出表
//! cargo test -p aevum-core --test fit_sharpen                            # 验收
//! ```
//!
//! **已试过、确认无效的方向**（省得以后重跑）：软膝限幅（膝内线性、膝外 `tanh`
//! 饱和）。设想是让低对比度纹理躲过限幅、只压硬边，但实测这两个指标量到的
//! Δ 都远超任何合理膝点（正弦靶约 0.04，阶跃边约 0.25），全部落在「饱和到 cap」
//! 那一段，因此 MTF50 与 halo 都只由 `delta_cap` 决定，换 knee 数值一格不动。
//! 结论：这个参数化下软膝买不到东西，不做。

mod support;

use aevum_core::develop::spatial::{
    UsmSettings, UsmTuning, output_sharpen_usm_tuned, usm_size_gain,
};
use aevum_core::export::{OutputSharpen, RenderOptions, SharpenStrength};
use support::{
    gaussian_blur as blur, halo_ratio, mtf_freqs, mtf_through, mtf50, mtf50_gain, sine_target,
    step_target,
};

const W: usize = 256;
const H: usize = 24;
/// 合成靶的振幅与中灰：低对比度，让锐化/降噪留在近似线性区。
const AMP: f32 = 0.10;
const MID: f32 = 0.5;
/// 模拟镜头/抗锯齿造成的软化。真实照片 MTF50 落在 0.2–0.35 cyc/px，
/// 理想数字边则接近 Nyquist——不给这层模糊，锐化就「没有可提升的空间」。
const LENS_SIGMA: f32 = 0.7;

// ---------------------------------------------------------------------------
// 指标自检：指标错了，后面所有拟合都是假的
// ---------------------------------------------------------------------------

/// 理想正弦的测出幅度必须等于生成幅度，误差 < 2%。
#[test]
fn metrics_amplitude_is_calibrated() {
    for &f in &[0.03f32, 0.10, 0.20, 0.35] {
        let img = sine_target(W, H, f, AMP, MID);
        let got = support::amp_at_mean(&img, W, H, f);
        let err = (got - AMP).abs() / AMP;
        assert!(
            err < 0.02,
            "f={f} 测出 {got:.4}，期望 {AMP}，误差 {:.1}%",
            err * 100.0
        );
    }
}

/// 未处理的阶跃边不该有任何过冲。
#[test]
fn metrics_halo_is_zero_on_clean_step() {
    let img = step_target(W, H, 0.25, 0.75);
    let h = halo_ratio(&img, W, H, 0.75, 0.25);
    assert!(h < 1e-4, "干净阶跃的 halo 应为 0，实测 {h:.4}");
}

/// 高斯模糊必须把 MTF50 压下来，且 σ 越大压得越低（单调）。
///
/// σ=0 那档不作「必须穿过 0.5」的要求：理想数字正弦到 Nyquist 都不衰减，
/// 它的 MTF50 本来就在扫参范围之外——这恰恰说明**扫参必须先加一层模糊**，
/// 否则「锐化提升 MTF50」无从谈起。
#[test]
fn metrics_blur_lowers_mtf50_monotonically() {
    let freqs = mtf_freqs();
    let mut last = f32::INFINITY;
    for &sigma in &[0.0f32, 0.6, 1.0, 1.6] {
        let curve = mtf_through(W, H, AMP, MID, &freqs, |img| {
            *img = blur(img, W, H, sigma);
        });
        let (f50, exact) = mtf50(&curve);
        if sigma > 0.0 {
            assert!(exact, "σ={sigma} 的 MTF 曲线没穿过 0.5，扫参频点范围不够");
        }
        assert!(f50 < last, "σ={sigma} 的 MTF50={f50:.3} 未随模糊下降");
        last = f50;
    }
}

/// 未处理的正弦在任意频点都应接近满响应——这条钉住「逐频点生成靶」的正确性。
#[test]
fn metrics_passthrough_is_flat() {
    let freqs = mtf_freqs();
    let curve = mtf_through(W, H, AMP, MID, &freqs, |_| {});
    for &(f, m) in &curve {
        assert!(
            (m - 1.0).abs() < 0.05,
            "f={f:.3} 的直通响应 {m:.3} 偏离 1.0 过多（频点与靶频率没对齐？）"
        );
    }
}

// ---------------------------------------------------------------------------
// 验收：把拟合出来的默认值钉在 atlas §46.2 的阈值上
// ---------------------------------------------------------------------------

/// 生产路径真正会用的三元组：直接问 `RenderOptions::sharpen_profile()`，不抄字面量。
///
/// 这里以前是 `const REAL_CORNERS: &[(f32, f32)] = &[(0.385,0.6), (0.8,0.9), (1.0,1.4),
/// (1.4,1.4)]` —— 手抄的。抄出来的数等于第二个事实源：`OutputSharpen::profile()`
/// 改了它不知道，测试继续对着空气绿。现在只有一条路取数。
///
/// The triples the production path actually uses: asked of
/// `RenderOptions::sharpen_profile()`, never copied by hand. The old `const REAL_CORNERS`
/// was a hand-copied second source of truth — change `OutputSharpen::profile()` and it would
/// not know, leaving the test green against nothing.
fn real_triples() -> Vec<(&'static str, &'static str, (f32, f32, f32))> {
    let mut out = Vec::new();
    for p in OutputSharpen::ALL {
        for s in SharpenStrength::ALL {
            let mut o = RenderOptions::default();
            o.sharpen_for = p;
            o.sharpen_strength = s;
            if let Some(t) = o.sharpen_profile() {
                out.push((p.key(), s.key(), t));
            }
        }
    }
    out
}

/// 全部真实设置都不许出过冲红线：`halo < 3%`。
///
/// 这条与用途档无关——哪怕是「哑面 + 高」这种最狠的组合，过冲也得压住。
/// 量的是**生产三元组**（含各自的软阈值），不是把阈值拆成 0 的理想路径：
/// 软阈值会把小 Δ 吃掉，拆掉它量出来的数偏大，等于给了一个生产上不存在的余量。
///
/// Every real setting must stay under the overshoot red line of 3%, including the harshest
/// "matte + high" pairing. Measured on the **production triples** (each with its own soft
/// threshold), not on the idealised path with the threshold zeroed: the soft threshold eats
/// small deltas, so dropping it measures a margin that production never has.
#[test]
fn no_real_setting_exceeds_halo_budget() {
    for (p, s, (amount, radius, threshold)) in real_triples() {
        let (_, halo) = measure_output_sharpen(amount, radius, threshold);
        assert!(
            halo < 0.03,
            "{p}/{s}（amount={amount:.4} radius={radius} thr={threshold}）的 halo {:.2}% 超 3%",
            halo * 100.0
        );
    }
}

/// 主用途档（Matte）在 `Standard` / `High` 上必须拿到 `MTF50× ∈ [1.2, 1.6]`。
///
/// 只对 Matte 提这条要求：Screen / Glossy 的设计意图就是「只回补缩放损耗」，
/// 拿 Matte 的强度指标去卡它们等于把三档做成同一档。
///
/// `Low` 例外，而且必须是例外：实测 1.144×，故意压在目标下面。强度档存在的意义就是
/// 「同一用途下调力度」，若 `Matte/Low` 也达标，那强度滑杆就只有往上两个方向有用。
/// 所以这里反过来钉住它：**必须低于 1.2，且仍 > 1.05**（不是没锐化）。
///
/// The Matte profile must hit `MTF50× ∈ [1.2, 1.6]` at `Standard` / `High`. `Low` is the
/// exception by design: measured 1.144×, deliberately under target — the strength mode exists
/// to scale force within one purpose, so a `Matte/Low` that also hit the target would mean the
/// slider only works upward. Hence it is pinned the other way: **below 1.2 but still above
/// 1.05** (it does sharpen).
#[test]
fn matte_profile_reaches_the_mtf_target() {
    for (p, s, (amount, radius, threshold)) in real_triples() {
        if p != "matte" {
            continue;
        }
        let (gain, _) = measure_output_sharpen(amount, radius, threshold);
        if s == "low" {
            assert!(
                (1.05..1.20).contains(&gain),
                "Matte/low 的 MTF50 提升 {gain:.3}× 不在 [1.05, 1.20)——它该低于打印目标但仍在锐化"
            );
        } else {
            assert!(
                (1.2..=1.6).contains(&gain),
                "Matte/{s} 的 MTF50 提升 {gain:.3}× 出界（目标 1.2–1.6）"
            );
        }
    }
}

/// 三个用途档必须是一条**量出来的阶梯**，不是三个名字。
///
/// 判据分三层，全部实测（`Standard` 强度，生产三元组）：
///
/// | 档 | MTF50× | halo |
/// |---|---|---|
/// | screen | 1.096 | 0.19% |
/// | glossy | 1.161 | 0.71% |
/// | matte | 1.209 | 2.09% |
///
/// **① 目的与代价同向**：锐度递增、过冲也递增，且档间差 ≥ 0.02×——三档若拉不开就
/// 等于一个档。**② 档不塌陷**：screen 的**最强**档也必须弱于 matte 的**标准**档
/// （实测 1.136 < 1.209）——否则「屏幕」这个用途只是名字，用户选它等于选打印档。
/// 这条不能写成「screen ≤ 1.15」那种绝对阈值：那个界在档序约束下永远碰不到，
/// 是个不会炸的判据。**③ 三元组同向**：`amount` 递增、`radius` 递增、`threshold`
/// **递减**。任何一格反向都会让相邻两档互相抵消（比如 radius 涨、threshold 也涨 =
/// 半径更钝却更不响应），是设计上不该出现的形状。
///
/// The three purpose profiles must be a **measured ladder**, not three names. Three layers,
/// all measured at `Standard` strength on the production triples: (1) sharpening and its cost
/// rise together, at least a 0.02× gap between rungs; (2) **rungs do not collapse** — the
/// strongest `screen` setting must still be weaker than the standard `matte` one
/// (measured 1.136 < 1.209), otherwise "screen" is just a label; (3) the triples move in the
/// same direction — `amount` up, `radius` up, `threshold` **down** — any field reversed makes
/// neighbouring rungs cancel out.
#[test]
fn purpose_profiles_form_a_measured_ladder() {
    let at = |p: OutputSharpen, s: SharpenStrength| {
        let mut o = RenderOptions::default();
        o.sharpen_for = p;
        o.sharpen_strength = s;
        o.sharpen_profile().expect("真实用途档必须有 profile")
    };
    let gain_of = |t: (f32, f32, f32)| measure_output_sharpen(t.0, t.1, t.2).0;

    let rungs = [
        (
            "screen",
            at(OutputSharpen::Screen, SharpenStrength::Standard),
        ),
        (
            "glossy",
            at(OutputSharpen::Glossy, SharpenStrength::Standard),
        ),
        ("matte", at(OutputSharpen::Matte, SharpenStrength::Standard)),
    ];

    // ③ 三元组同向。
    for pair in rungs.windows(2) {
        let (lo_name, (a0, r0, t0)) = pair[0];
        let (hi_name, (a1, r1, t1)) = pair[1];
        assert!(
            a1 > a0,
            "{lo_name}→{hi_name} 的 amount 没递增：{a0:.4} → {a1:.4}"
        );
        assert!(r1 > r0, "{lo_name}→{hi_name} 的 radius 没递增：{r0} → {r1}");
        assert!(
            t1 < t0,
            "{lo_name}→{hi_name} 的 threshold 没递减：{t0} → {t1}（半径更大却更不响应，两档会互相抵消）"
        );
    }

    // ① 锐度与过冲同向递增。
    let mut prev_gain = 0.0f32;
    let mut prev_halo = 0.0f32;
    for (name, (a, r, t)) in rungs {
        let (gain, halo) = measure_output_sharpen(a, r, t);
        assert!(
            gain > prev_gain + 0.02,
            "{name} 的 MTF50 提升 {gain:.3}× 没比下一档 {prev_gain:.3}× 高出 0.02×——两档分不开"
        );
        assert!(
            halo > prev_halo,
            "{name} 的 halo {:.2}% 反而低于下一档 {:.2}%——「更用力」没体现成代价",
            halo * 100.0,
            prev_halo * 100.0
        );
        prev_gain = gain;
        prev_halo = halo;
    }

    // ② 档不塌陷：screen 最强 < matte 标准。
    let screen_top = gain_of(at(OutputSharpen::Screen, SharpenStrength::High));
    let matte_std = gain_of(at(OutputSharpen::Matte, SharpenStrength::Standard));
    assert!(
        screen_top < matte_std,
        "screen/high 的 MTF50 提升 {screen_top:.3}× 追平甚至超过 matte/standard 的 {matte_std:.3}× ——「屏幕」档塌进打印档了"
    );
}

/// 过冲必须**不随力度平方增长**——这是那条真缺陷的回归测试。
///
/// 旧实现把高通信号钳到 `±0.12·amount` 再乘 amount，于是输出幅度正比于
/// amount²，且 0.12 比典型高通幅度（阶跃边约 0.042）还大，闸门一次都没咬住：
/// 实测 amount 1.0→2.0 的 halo 是 8.35%→17.66%（2.12 倍）。
/// 现在钳的是高通信号本身、与 amount 无关，输出随力度线性变化。
#[test]
fn halo_does_not_grow_superlinearly() {
    let (_, halo_lo) = measure_output_sharpen(0.7, 1.4, 0.0);
    let (_, halo_hi) = measure_output_sharpen(1.4, 1.4, 0.0);
    let ratio = halo_hi / halo_lo.max(1e-6);
    assert!(
        ratio < 2.2,
        "力度翻倍后 halo 涨了 {ratio:.2} 倍（{:.2}% → {:.2}%），限幅又失效了",
        halo_lo * 100.0,
        halo_hi * 100.0
    );
}

/// 力度越大锐化越强（单调），且 amount=0 原样输出。
#[test]
fn mtf_gain_is_monotone_in_amount() {
    let mut last = 0.0f32;
    for &amount in &[0.0f32, 0.385, 0.55, 0.8, 1.0, 1.4] {
        let (gain, _) = measure_output_sharpen(amount, 1.0, 0.0);
        assert!(
            gain >= last - 1e-3,
            "amount={amount} 的 MTF50 提升 {gain:.3}× 低于更小力度的 {last:.3}×"
        );
        last = gain;
    }
    let (gain, halo) = measure_output_sharpen(0.0, 1.0, 0.0);
    assert!(
        (gain - 1.0).abs() < 0.02,
        "amount=0 应原样输出，实测 {gain:.3}×"
    );
    assert!(halo < 1e-4, "amount=0 不该产生过冲，实测 {halo:.4}");
}

// ---------------------------------------------------------------------------
// 扫参：输出参数 → 指标表
// ---------------------------------------------------------------------------

fn measure_output_sharpen(amount: f32, radius: f32, threshold: f32) -> (f32, f32) {
    measure_tuned(amount, radius, threshold, UsmTuning::default())
}

fn measure_tuned(amount: f32, radius: f32, threshold: f32, tuning: UsmTuning) -> (f32, f32) {
    let freqs = mtf_freqs();
    // 基线：只有镜头模糊、不锐化。
    let base_curve = mtf_through(W, H, AMP, MID, &freqs, |img| {
        *img = blur(img, W, H, LENS_SIGMA);
    });
    // 锐化后：同样的模糊，再过一遍输出锐化。
    let sharp_curve = mtf_through(W, H, AMP, MID, &freqs, |img| {
        *img = blur(img, W, H, LENS_SIGMA);
        output_sharpen_usm_tuned(
            img,
            W,
            H,
            UsmSettings::new(amount, radius, threshold),
            tuning,
        );
    });
    let gain = mtf50_gain(&base_curve, &sharp_curve);

    let mut step = blur(&step_target(W, H, 0.25, 0.75), W, H, LENS_SIGMA);
    output_sharpen_usm_tuned(
        &mut step,
        W,
        H,
        UsmSettings::new(amount, radius, threshold),
        tuning,
    );
    let halo = halo_ratio(&step, W, H, 0.75, 0.25);
    (gain, halo)
}

/// 扫 `delta_cap`：它是 halo 的主闸门。
///
/// 固定用户参数在「典型值」而不是扫满二维：`amount`/`radius` 是滑杆，用户会
/// 自己推；要拟的是**内部常量**，就得看它在典型设置下把指标压在哪。这里跑
/// 真实 UI 能到的四个角：Screen+低(0.385,0.6)、Glossy+标准(0.8,0.9)、
/// Matte+标准(1.0,1.4)、Matte+高(1.4,1.4)。
#[test]
#[ignore = "扫参工具，手动跑：-- --ignored --nocapture"]
fn sweep_delta_cap() {
    println!("\n=== Δ 限幅绝对上限扫参 ===");
    println!(
        "{:>10} {:>7} {:>7} {:>10} {:>10}  {:>8}",
        "delta_cap", "amount", "radius", "MTF50×", "halo%", "判断"
    );
    for &cap in &[0.008f32, 0.009, 0.010, 0.0105, 0.011, 0.012] {
        let tuning = UsmTuning {
            delta_cap: cap,
            ..UsmTuning::default()
        };
        for &(amount, radius) in &[(0.385f32, 0.6f32), (0.8, 0.9), (1.0, 1.4), (1.4, 1.4)] {
            let (gain, halo) = measure_tuned(amount, radius, 0.0, tuning);
            let ok_gain = (1.2..=1.6).contains(&gain);
            let ok_halo = halo < 0.03;
            let verdict = match (ok_gain, ok_halo) {
                (true, true) => "达标",
                (false, true) => "MTF 不足",
                (true, false) => "halo 超",
                (false, false) => "双不达",
            };
            println!(
                "{cap:>10.4} {amount:>7.3} {radius:>7.2} {gain:>10.3} {:>10.2}  {verdict:>8}",
                halo * 100.0
            );
        }
    }
    println!("目标：MTF50× ∈ [1.2, 1.6] 且 halo < 3.00%\n");
}

/// 扫 amount × radius，看 MTF50 提升与 halo 怎么走。
///
/// amount 取真实 UI 能到达的区间：用途档基准（Screen 0.55 / Glossy 0.80 /
/// Matte 1.00）× 强度档（低 0.7 / 标准 1.0 / 高 1.4），即 **0.385 – 1.40**。
/// 扫到 2.0 之类范围外的值没有意义——那是产品里不存在的设置。
#[test]
#[ignore = "扫参工具，手动跑：-- --ignored --nocapture"]
fn sweep_output_sharpen() {
    println!("\n=== 输出锐化扫参（threshold=0，σ_lens={LENS_SIGMA}）===");
    println!(
        "{:>7} {:>7} {:>10} {:>10}",
        "amount", "radius", "MTF50×", "halo%"
    );
    for &amount in &[0.385f32, 0.55, 0.8, 1.0, 1.4] {
        for &radius in &[0.6f32, 0.9, 1.4, 2.0] {
            let (gain, halo) = measure_output_sharpen(amount, radius, 0.0);
            println!(
                "{amount:>7.3} {radius:>7.2} {gain:>10.3} {:>10.2}",
                halo * 100.0
            );
        }
    }
    println!("目标：MTF50× 落在 1.2–1.6，halo < 3.00\n");
}

/// 扫 size_gain 的刻度：1200 / 1600 / 2400 三档下增益各是多少。
#[test]
#[ignore = "扫参工具，手动跑：-- --ignored --nocapture"]
fn sweep_size_gain() {
    println!("\n=== 输出尺寸感知增益 ===");
    for &side in &[800usize, 1200, 1600, 2400, 3200, 4000] {
        println!("{side:>5}px → size_gain {:.3}", usm_size_gain(side, side));
    }
    println!("（当前实现 clamp(max/1600,1,3).sqrt()，2.4K 起封顶 1.732）\n");
}

/// 扫阈值：threshold 应该只吃掉小振幅细节（噪声），不伤大边。
#[test]
#[ignore = "扫参工具，手动跑：-- --ignored --nocapture"]
fn sweep_threshold() {
    println!("\n=== 阈值对细节/噪声的取舍（amount=1.0, radius=1.0）===");
    println!("{:>9} {:>12} {:>12}", "threshold", "MTF50×", "halo%");
    for &t in &[0.0f32, 0.005, 0.01, 0.02, 0.04] {
        let (gain, halo) = measure_output_sharpen(1.0, 1.0, t);
        println!("{t:>9.3} {gain:>12.3} {:>12.2}", halo * 100.0);
    }
}
