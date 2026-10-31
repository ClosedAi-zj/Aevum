//! 降噪的客观测量与现状记录。
//!
//! 判定口径取自 `docs/adobe-algorithm-atlas.md` §46.2：
//! **噪声方差下降 > 50%，同时 MTF 保持 > 85%**。
//!
//! 这两个指标天然对立——把图糊成一片，方差当然掉得更多，但细节也没了。
//! 所以任何降噪读数都必须成对给出，只报一个数的参数没有意义。
//!
//! **现状（实测，见 `cargo test -- --ignored --nocapture`）**：
//!
//! | 路径 | 方差下降 | MTF(0.12) 保持 | 判定 |
//! |---|---|---|---|
//! | 单尺度（默认，`nr_multiscale=0`） | 69.5% | 84.5% | 差 0.5 点，基本达标 |
//! | 多尺度（`nr_multiscale=100`） | 72.0% | 64.2% | 伤细节，低 21 点 |
//!
//! 也就是说：默认路径已经在目标线上，**多尺度分支才是问题所在**。原因在
//! `eps` 与真实噪声脱钩（固定地板 `0.0001+0.006k` 加放大系数，见
//! `spatial.rs::denoise_apply` 的诊断注释）：σ=0.02 时 eps≈0.034，是噪声
//! 方差的 85 倍，把对比度低于 0.18 的结构一并抹平。
//!
//! 修法已验证有效（`eps = gain·σ̂²` → 52% / 81%，两个方向都朝目标收），但
//! `nr_multiscale` 有 Python 金标，改公式等于改参照数据，必须先与 oracle
//! 一侧同步重冻结，因此**这一步没有落地**，记在 CHANGELOG「已知问题」。
//!
//! 本文件因此只做两件事：把现状量清楚、把不该退化的不变量钉住。
//!
//! 跑法：
//! ```text
//! cargo test -p aevum-core --test fit_denoise -- --ignored --nocapture   # 出表
//! cargo test -p aevum-core --test fit_denoise                            # 不变量
//! ```

mod support;

use aevum_core::develop::spatial::denoise;
use aevum_core::grade::GradeParams;
use support::{mtf_through, noise_target, variance};

const W: usize = 512;
const H: usize = 256;
/// 降噪作用的细节频率：偏低频，靠近「纹理」而不是「噪声」的位置。
const DETAIL_FREQ: f32 = 0.12;
const AMP: f32 = 0.10;
const MID: f32 = 0.5;

fn params(luma: f64, multiscale: f64, sigma: f64) -> GradeParams {
    GradeParams {
        nr_luma: luma,
        nr_multiscale: multiscale,
        nr_sigma_manual: sigma,
        ..GradeParams::default()
    }
}

/// 一档降噪的读数：`(方差下降比例, MTF 保持比例)`。
fn measure(p: &GradeParams, noise_sigma: f32, w: usize, h: usize) -> (f32, f32) {
    // 方差：平坦灰底 + 噪声，整幅方差就是噪声方差。
    let noisy = noise_target(w, h, MID, noise_sigma, 0x9E37_79B9);
    let before = variance(&noisy);
    let mut after_img = noisy;
    denoise(&mut after_img, w, h, p);
    let after = variance(&after_img);
    let drop = if before > 0.0 {
        1.0 - after / before
    } else {
        0.0
    };

    // MTF 保持：正弦 + 同量级噪声，比较过降噪前后的响应。
    // 噪声会污染单频测量，所以两边都用「多行平均 + 同一随机种子」，靠
    // 平均把噪声压下去，剩下的是降噪对信号本身的衰减。
    let base = mtf_through(w, h, AMP, MID, &[DETAIL_FREQ], |img| {
        add_noise(img, w, h, noise_sigma, 0x1234_5678);
    });
    let kept = mtf_through(w, h, AMP, MID, &[DETAIL_FREQ], |img| {
        add_noise(img, w, h, noise_sigma, 0x1234_5678);
        denoise(img, w, h, p);
    });
    (drop, kept[0].1 / base[0].1.max(1e-6))
}

/// 在已有图像上叠同一份可复现噪声。
fn add_noise(img: &mut [f32], w: usize, h: usize, sigma: f32, seed: u32) {
    let noise = noise_target(w, h, 0.0, sigma, seed);
    for (a, b) in img.iter_mut().zip(noise.iter()) {
        *a += *b;
    }
}

// ---------------------------------------------------------------------------
// 不变量：这几条不该随参数调整而退化
// ---------------------------------------------------------------------------

/// `nr_luma=0` 必须一个像素都不动。
#[test]
fn zero_strength_is_identity() {
    let (drop, keep) = measure(&params(0.0, 100.0, 0.0), 0.02, W, H);
    assert!(drop < 1e-4, "nr_luma=0 不该降噪，实测方差掉 {:.4}", drop);
    assert!(keep > 0.999, "nr_luma=0 不该损细节，实测保持 {:.4}", keep);
}

/// 强度滑杆必须单调：越大掉得越多。
#[test]
fn strength_slider_is_monotone() {
    let mut last = -1.0f32;
    for &luma in &[25.0f64, 50.0, 75.0, 100.0] {
        let (drop, _) = measure(&params(luma, 100.0, 0.0), 0.02, W, H);
        assert!(
            drop >= last - 1e-3,
            "nr_luma={luma} 的方差下降 {:.3} 低于更小强度的 {:.3}",
            drop,
            last
        );
        last = drop;
    }
}

/// 多尺度强度滑杆（`nr_multiscale`）不能反向。
#[test]
fn multiscale_slider_is_monotone() {
    let mut last = -1.0f32;
    for &ms in &[0.0f64, 50.0, 100.0] {
        let (drop, _) = measure(&params(50.0, ms, 0.0), 0.02, W, H);
        assert!(
            drop >= last - 0.02,
            "nr_multiscale={ms} 的方差下降 {:.3} 明显低于更小档的 {:.3}",
            drop,
            last
        );
        last = drop;
    }
}

/// 噪声方差必须真的被降下来（§46.2 的第一半）：> 50%。
#[test]
fn variance_drop_meets_target() {
    let (drop, _) = measure(&params(50.0, 100.0, 0.0), 0.02, W, H);
    assert!(drop > 0.50, "方差只降了 {:.1}%（目标 > 50%）", drop * 100.0);
}

/// 细节保持的地板：> 60%。
///
/// 这里**不是** §46.2 的 85% 目标——现状只有 ~64%，见文件头说明。设 60%
/// 是为了抓「又糊了一大截」的回归，同时不把已知缺口伪装成达标。
#[test]
fn detail_retention_does_not_regress() {
    let (_, keep) = measure(&params(50.0, 100.0, 0.0), 0.02, W, H);
    assert!(
        keep > 0.60,
        "MTF(0.12) 只剩 {:.1}%，比现状（~64%）还差，属于新回归",
        keep * 100.0
    );
}

// ---------------------------------------------------------------------------
// 扫参：把现状量成表
// ---------------------------------------------------------------------------

/// 扫强度档：看「掉多少噪声」与「留多少细节」的权衡曲线。
#[test]
#[ignore = "扫参工具，手动跑：-- --ignored --nocapture"]
fn sweep_denoise_strength() {
    println!("\n=== 降噪强度扫参（σ_noise=0.020）===");
    println!(
        "{:>8} {:>10} {:>10} {:>10}  {:>8}",
        "nr_luma", "多尺度", "方差↓%", "MTF保持%", "判断"
    );
    for &(luma, ms) in &[
        (25.0f64, 0.0f64),
        (50.0, 0.0),
        (100.0, 0.0),
        (50.0, 50.0),
        (50.0, 100.0),
        (100.0, 100.0),
    ] {
        let p = params(luma, ms, 0.0);
        let (drop, keep) = measure(&p, 0.02, W, H);
        let verdict = if drop > 0.5 && keep > 0.85 {
            "达标"
        } else if drop <= 0.5 {
            "降噪不足"
        } else {
            "伤细节"
        };
        println!(
            "{luma:>8.0} {ms:>10.0} {:>10.1} {:>10.1}  {verdict:>8}",
            drop * 100.0,
            keep * 100.0
        );
    }
    println!("目标：方差下降 > 50% 且 MTF 保持 > 85%\n");
}

/// 扫噪声水平：同一个强度设置在不同噪声下是否稳定。
#[test]
#[ignore = "扫参工具，手动跑：-- --ignored --nocapture"]
fn sweep_noise_levels() {
    println!("\n=== 不同噪声水平下的行为（nr_luma=50, 多尺度=100）===");
    println!(
        "{:>8} {:>10} {:>10}  {:>8}",
        "σ_noise", "方差↓%", "MTF保持%", "判断"
    );
    for &sigma in &[0.004f32, 0.008, 0.02, 0.05] {
        let (drop, keep) = measure(&params(50.0, 100.0, 0.0), sigma, W, H);
        let verdict = if drop > 0.5 && keep > 0.85 {
            "达标"
        } else if drop <= 0.5 {
            "降噪不足"
        } else {
            "伤细节"
        };
        println!(
            "{sigma:>8.3} {:>10.1} {:>10.1}  {verdict:>8}",
            drop * 100.0,
            keep * 100.0
        );
    }
    println!("目标：方差下降 > 50% 且 MTF 保持 > 85%\n");
}

/// 扫画幅：验证「半径 = min/260」对降噪能力的影响。
///
/// 半径是画幅的函数，所以「降噪够不够」离了画幅没有答案。
#[test]
#[ignore = "扫参工具，release 下跑：--release -- --ignored --nocapture"]
fn sweep_frame_size() {
    println!("\n=== 画幅对降噪能力的影响（σ=0.020, nr_luma=50, 多尺度=100）===");
    println!(
        "{:>14} {:>8} {:>10} {:>10}  {:>8}",
        "画幅", "半径", "方差↓%", "MTF保持%", "判断"
    );
    for &(w, h) in &[
        (256usize, 128usize),
        (1040, 1040),
        (1560, 1040),
        (2080, 1560),
    ] {
        let (drop, keep) = measure(&params(50.0, 100.0, 0.0), 0.02, w, h);
        let r = (w.min(h) / 260).max(2);
        let verdict = if drop > 0.5 && keep > 0.85 {
            "达标"
        } else if drop <= 0.5 {
            "降噪不足"
        } else {
            "伤细节"
        };
        println!(
            "{w:>6}×{h:<7} {r:>8} {:>10.1} {:>10.1}  {verdict:>8}",
            drop * 100.0,
            keep * 100.0
        );
    }
    println!("目标：方差下降 > 50% 且 MTF 保持 > 85%\n");
}

/// 扫细节回还：`nr_luma_detail` 对双目标有没有帮助。
#[test]
#[ignore = "扫参工具，手动跑：-- --ignored --nocapture"]
fn sweep_detail_return() {
    println!("\n=== 细节回还（σ=0.020, nr_luma=50, 多尺度=100）===");
    println!(
        "{:>8} {:>10} {:>10}  {:>8}",
        "细节", "方差↓%", "MTF保持%", "判断"
    );
    for &detail in &[0.0f64, 25.0, 50.0, 75.0] {
        let mut p = params(50.0, 100.0, 0.0);
        p.nr_luma_detail = detail;
        let (drop, keep) = measure(&p, 0.02, W, H);
        let verdict = if drop > 0.5 && keep > 0.85 {
            "达标"
        } else if drop <= 0.5 {
            "降噪不足"
        } else {
            "伤细节"
        };
        println!(
            "{detail:>8.0} {:>10.1} {:>10.1}  {verdict:>8}",
            drop * 100.0,
            keep * 100.0
        );
    }
    println!("目标：方差下降 > 50% 且 MTF 保持 > 85%\n");
}
