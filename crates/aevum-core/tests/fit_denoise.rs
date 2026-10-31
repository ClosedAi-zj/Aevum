//! 降噪的客观测量与现状记录。
//!
//! 判定口径取自 `docs/adobe-algorithm-atlas.md` §46.2：
//! **噪声方差下降 > 50%，同时 MTF 保持 > 85%**。
//!
//! 这两个指标天然对立——把图糊成一片，方差当然掉得更多，但细节也没了。
//! 所以任何降噪读数都必须成对给出，只报一个数的参数没有意义。
//!
//! **判定位钉死 1024×768**（`PW`/`PH`）。理由：①它是真实照片中位短边的量级
//! （4:3 的手机/相机图缩到短边 768，降噪读数对短边像素数敏感、对总分辨率不
//! 敏感）；②这个尺寸上 `rm=(768/260).max(2)=2` 已脱离地板，`rc=(768/130).max(2)=5`
//! 是另一个尺度，**rc 与 rm 真正分离**，多尺度不是「同一档滤波做两遍」；
//! ③rm=2 覆盖 512–1039 的整段短边，不是孤点。
//!
//! **现状（实测，`cargo test -p aevum-core --release --test fit_denoise --
//! --ignored --nocapture`）**：多尺度 eps 已改成 `C·σ̂²/rm`（C=0.8，见
//! `spatial.rs::NR_MS_EPS_C`），单尺度一个字节没动、读数原样：
//!
//! | 画幅 | (rc,rm) | 单尺度 方差↓/MTF | 多尺度 旧式 | 多尺度 新式(ms=100) |
//! |---|---|---|---|---|
//! | 512×256（退化） | (2,2) | 69.5% / 84.5% | 72.0% / 64.2% | 26.7% / 93.6%（降噪不足） |
//! | 1024×768（判定位） | (5,2) | 69.4% / 84.5% | 74.5% / 44.6% | **53.8% / 90.0% 达标** |
//! | 1536×1152 | (8,4) | 73.5% / 76.6% | —（本轮才测） | **62.7% / 87.3% 达标** |
//! | 2048×1536 | (11,5) | 73.8% / 75.5% | 74.9% / 48.7% | **60.3% / 89.5% 达标** |
//!
//! 三点必须一起读：
//!
//! 1. **512×256 仍然是退化画幅**：`min<390`，rc==rm==2，两层是同一个 5×5 引导
//!    滤波做两遍。新式在那里只有 26.7%——不是公式错了，是 5×5 的窗口在 512
//!    上本来就磨不动 σ=0.02 的噪声（旧式靠 eps 放大 86 倍硬磨）。所以**这一行
//!    不参与达标判读**，只当「退化画幅长什么样」的对照。
//! 2. **单尺度没跟上**：1024×768 的 84.5% 贴着 85% 目标线，2048×1536 只有
//!    75.5%。它的 `eps=0.0001+0.006k` 与噪声无关、半径 `min/260` 让大图磨得
//!    更狠，是 B1 之外的独立病灶（这一版按验收要求不动它）。
//! 3. **代价**：多尺度的方差下降比单尺度低（1024×768：53.8% vs 69.4%）。这是
//!    「eps 从噪声方差的 86 倍降到 0.8/rm 倍」的必然结果——换来了 5.5 点 MTF。
//!    用户感知是「开多尺度后比纯单尺度更保留细节、噪点也更多一点」。
//!
//! 本文件因此只做三件事：把现状按画幅量清楚、把不该退化的不变量钉住（含
//! 单尺度**逐位**不变）、把画幅依赖留在扫参表里。
//!
//! 跑法：
//! ```text
//! cargo test -p aevum-core --release --test fit_denoise -- --ignored --nocapture  # 出表
//! cargo test -p aevum-core --test fit_denoise                                     # 不变量
//! ```
//!
//! 出表用 release：2048×1536 那一段 debug 下要跑几分钟，且与读数无关。

mod support;

use aevum_core::develop::spatial::{NR_MS_EPS_C, denoise};
use aevum_core::grade::GradeParams;
use support::{mtf_through, noise_target, variance};

/// 退化靶：`min<390`，两层半径双双落到地板 2。只用来做形状不变量与历史读数
/// 对照，**不拿来判达标**（见文件头）。
const W: usize = 512;
const H: usize = 256;

/// 判定位：1.0.0 验收 B1 的测量画幅，钉死 1024×768。
///
/// 三条理由：①真实照片的中位短边量级（手机 4:3 的 4032×3024 缩到 1/4 就是
/// 3840 的短边 2880，但降噪的读数对「短边像素数」敏感而不是对分辨率敏感，
/// 1024 短边落在「rm=2 已经脱离地板、又不必开大图」的区间）；②`rm=(min/260)
/// .max(2)` 在 768 上刚好给出 2、`rc=(min/130)` 给出 5——两层真正分离
/// （rc≠rm），多尺度不是「同一档滤波做两遍」；③864×576/1280×960 这类中间画幅
/// 的 rm 仍是 2，所以 1024×768 与更大画幅的 rm 是连续谱上的同一段，不是孤点。
const PW: usize = 1024;
const PH: usize = 768;

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

/// 多尺度权重（`nr_multiscale`）的作用方向：**ms 越大，磨得越少**。
///
/// 这条原来断言的是「ms 越大降噪越多」，靠旧 eps 的饱和蒙过去——旧式两层都给
/// eps≈0.034（噪声方差的几十倍），两层输出几乎一样，差值被抹平，升降都看不出。
/// 换成 `eps=C·σ̂²/rm` 后真实方向露出来了，而它是**结构性**的，不是这次改坏的：
///
/// ```text
/// sm = b_mid + ms·(b_coarse − b_mid)
/// ```
///
/// `b_mid = guided(b_coarse, rm)`，即在粗层结果上又过了一遍 rm 半径，比粗层本身
/// 更平滑。ms=1 时 sm 退化成 b_coarse，也就是「少磨一遍」。所以 ms 上涨是往
/// 细节多、噪声也多的方向走。
///
/// 这里钉的是「方向没翻」（有人顺手改符号时当场红），不代表这个方向是对的：
/// 滑杆名叫「多尺度强度」，越大越不磨是反直觉的。修它要让 ms=0 取 b_mid、
/// ms=1 取更粗的一层（或干脆改成两层加权和），属于 B1 之外的单独议题，
/// 已列入 CHANGELOG 的已知问题。跨分支（ms=0 的单尺度）不参与比较：那是另一套
/// eps、另一个算法，见文件头「代价」。
#[test]
fn multiscale_weight_trades_smoothing_for_detail() {
    let low = measure(&params(50.0, 50.0, 0.0), 0.02, W, H).0;
    let high = measure(&params(50.0, 100.0, 0.0), 0.02, W, H).0;
    assert!(
        high <= low + 1e-3,
        "ms=100 的方差下降 {:.3} 反而高于 ms=50 的 {:.3}，权重方向变了",
        high,
        low
    );
}

/// 噪声方差必须真的被降下来（§46.2 的第一半）：> 50%。
///
/// 判定位 1024×768（见 `PW`/`PH`）。实测 53.8%，余量 3.8 点——这个余量是
/// C=0.8 的取舍结果：C 再往 0.7 压只剩 1.7 点，MTF 换来的余量不划算。
#[test]
fn variance_drop_meets_target() {
    let (drop, _) = measure(&params(50.0, 100.0, 0.0), 0.02, PW, PH);
    assert!(drop > 0.50, "方差只降了 {:.1}%（目标 > 50%）", drop * 100.0);
}

/// 多尺度细节保持：MTF ≥ 85%（§46.2 的目标线）。
///
/// 判定位 1024×768，实测 90.0%。旧 eps 在这里只有 44.6%，差的 45 点就是
/// 「eps 与噪声脱钩」的全部代价。
#[test]
fn detail_retention_meets_target() {
    let (_, keep) = measure(&params(50.0, 100.0, 0.0), 0.02, PW, PH);
    assert!(
        keep >= 0.85,
        "多尺度 MTF(0.12) 只剩 {:.1}%，低于 §46.2 的 85%",
        keep * 100.0
    );
}

/// 单尺度细节保持：≥ 84%。
///
/// 单尺度这条路这一版**一个字节没动**（`single_scale_stays_bit_exact` 守着），
/// 所以 84% 是在钉「已知缺口不许变大」，不是达标线：实测 84.5%，贴着
/// §46.2 的 85% 目标线差 0.5 点。它自己的病灶是 `eps=0.0001+0.006k` 与噪声
/// 无关、且半径 `min/260` 让大图磨得更狠（2048×1536 上只有 75.5%），要单独议。
#[test]
fn single_scale_detail_retention() {
    let (_, keep) = measure(&params(50.0, 0.0, 0.0), 0.02, PW, PH);
    assert!(
        keep >= 0.84,
        "单尺度 MTF(0.12) 掉到 {:.1}%，比现状（84.5%）还差",
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
/// 半径是画幅的函数，所以「降噪够不够」离了画幅没有答案。`rc==rm` 的行是
/// 退化尺寸（min<390），多尺度在那里等于同一档滤波做两遍。
#[test]
#[ignore = "扫参工具，release 下跑：--release -- --ignored --nocapture"]
fn sweep_frame_size() {
    println!("\n=== 画幅对降噪能力的影响（σ=0.020, nr_luma=50, 多尺度=100）===");
    println!(
        "{:>14} {:>4} {:>4} {:>10} {:>10}  {:>8}",
        "画幅", "rc", "rm", "方差↓%", "MTF保持%", "判断"
    );
    for &(w, h) in &[
        (256usize, 128usize),
        (1040, 1040),
        (1560, 1040),
        (2080, 1560),
    ] {
        let (drop, keep) = measure(&params(50.0, 100.0, 0.0), 0.02, w, h);
        let (rc, rm) = radii(w, h);
        let verdict = if drop > 0.5 && keep >= 0.85 {
            "达标"
        } else if drop <= 0.5 {
            "降噪不足"
        } else {
            "伤细节"
        };
        println!(
            "{w:>6}×{h:<7} {rc:>4} {rm:>4} {:>10.1} {:>10.1}  {verdict:>8}",
            drop * 100.0,
            keep * 100.0
        );
    }
    println!("目标：方差下降 > 50% 且 MTF 保持 > 85%\n");
}

/// FNV-1a 64：把 f32 缓冲的**原始字节**折成一个指纹。
///
/// 只用来钉「逐位一致」——浮点求和/均值对舍入顺序敏感，比「最大差 < ε」
/// 弱得多，所以这里比字节。
fn bits_digest(img: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in img {
        for b in v.to_bits().to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// 单尺度路径必须逐位不变：`nr_multiscale==0` 且 `nr_sigma_manual==0` 时走的是
/// 升级前那条老路，这条不变量在 `spatial.rs::denoise_apply` 里有注释承诺。
///
/// 比的是**原始字节**的 FNV-1a 指纹，不是「最大差 < ε」：改多尺度分支时很容易
/// 顺手挪动上面的共享量（`lum`、`k`、`res`），差一点点在肉眼验收里看不出来，
/// 但会让所有单尺度金标悄悄漂移。指纹取自 1.0.0 之前（commit 71316ac）的实现，
/// 改动前后跑同一个输入，值必须一模一样。
#[test]
fn single_scale_stays_bit_exact() {
    let noisy = noise_target(W, H, MID, 0.02, 0x9E37_79B9);
    let mut plain = noisy.clone();
    denoise(&mut plain, W, H, &params(50.0, 0.0, 0.0));
    assert_eq!(
        bits_digest(&plain),
        0x61ec_ddbb_8d5e_8d80,
        "单尺度（仅 nr_luma）的输出字节变了"
    );

    let mut with_detail = noisy;
    let mut p = params(50.0, 0.0, 0.0);
    p.nr_luma_detail = 15.0;
    p.nr_luma_contrast = 20.0;
    denoise(&mut with_detail, W, H, &p);
    assert_eq!(
        bits_digest(&with_detail),
        0x0dbd_b63a_cd6f_28e9,
        "单尺度（带细节回还与亮度对比）的输出字节变了"
    );
}

/// 画幅对应的两层半径，镜像 `spatial.rs::denoise_apply` 里的
/// `rc = (min/130).max(2)`、`rm = (min/260).max(2)`。
///
/// 在测试里重算一份而不是从产品代码导出：这是要量的**自变量**，表里必须与
/// 产品实际用的值对上；公式若改，这里也要跟着改，否则表会撒谎。
fn radii(w: usize, h: usize) -> (usize, usize) {
    let min = w.min(h);
    ((min / 130).max(2), (min / 260).max(2))
}

/// §46.2 的双目标判读：方差下降 > 50% 且 MTF 保持 ≥ 85%。
fn verdict(drop: f32, keep: f32) -> &'static str {
    if drop > 0.5 && keep >= 0.85 {
        "达标"
    } else if drop <= 0.5 {
        "降噪不足"
    } else {
        "伤细节"
    }
}

/// 扫画幅：单尺度 vs 多尺度（`eps = C·σ̂²/rm`），把「半径依赖」量成表。
///
/// 四行覆盖两件事：
/// - 512×256 是**退化画幅**：`min=256<390`，`rc==rm==2`，两层是同一个 5×5 引导
///   滤波做两遍。上一轮把它当结构结论，得出「多尺度天生吃细节」——错的。
///   这一行留着，是因为它是本轮退化与真实画幅的对照物，也是金标 48×64 的邻居。
/// - 1024×768 / 1536×1152 / 2048×1536：rm = 2 / 4 / 5，真正分离的两层。判定位
///   取 1024×768（见 `PW`）。
///
/// C 的扫参（C∈0.6–1.5 × 这三个真实画幅，外加 0.70–1.30 步长 0.05 的加密）是
/// 用临时的 `Some(C)` 钩子跑的，表与结论记在 `spatial.rs::NR_MS_EPS_C` 的文档
/// 注释里；钩子随定案删除，要复现得把那一行临时换回 `C·σ̂²/rm`。
/// 3072×2304（rm=8）没测：`measure` 内部同时挂 3–4 张 3072×2304×3 f32
/// （≈85MB/张），只为换一个更远的外推点不值得；它的 rm 落在 5 之外，属报告里
/// 明说的外推风险。
///
/// 内存：2048×1536×3 f32 ≈ 37MB/张，所以**串行**逐画幅跑，别把画幅并行铺开。
#[test]
#[ignore = "扫参工具，release 下跑：--release -- --ignored --nocapture"]
fn sweep_multiscale_frames() {
    println!("\n=== 画幅 × 路径（σ_noise=0.020, nr_luma=50, 多尺度=100, C={NR_MS_EPS_C}）===");
    println!(
        "{:>14} {:>4} {:>4} {:>10} {:>10}  {:>8}",
        "画幅", "rc", "rm", "方差↓%", "MTF保持%", "判断"
    );
    for &(w, h) in &[
        (512usize, 256usize),
        (1024, 768),
        (1536, 1152),
        (2048, 1536),
    ] {
        let (rc, rm) = radii(w, h);
        for &ms in &[0.0f64, 100.0] {
            let (drop, keep) = measure(&params(50.0, ms, 0.0), 0.02, w, h);
            // 单尺度只有一层，rc 一栏留空；多尺度才写粗层半径。
            let rc_cell = if ms == 0.0 {
                "—".to_string()
            } else {
                rc.to_string()
            };
            println!(
                "{:>14} {:>4} {:>4} {:>10.1} {:>10.1}  {:>8}",
                format!("{w}×{h}"),
                rc_cell,
                rm,
                drop * 100.0,
                keep * 100.0,
                verdict(drop, keep)
            );
        }
    }
    println!("目标：方差下降 > 50% 且 MTF 保持 ≥ 85%\n");
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
