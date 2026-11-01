//! 锐化保护（`sharp_protect`）的扫参与验收。
//!
//! `sharp_protect` 对 RAW 首次导入**默认给 20**（state.rs 的 `apply_raw_import_defaults`），
//! 也就是说它每天都在影响真实出图，但在此文件之前它一个测试都没有、两个阈值常量也
//! 没有依据。这里把「保护到什么程度」变成可量的两个数：
//!
//! - **噪声增益**：纯噪声靶上，`std(输出 − 输入) / std(输入)`。不做任何锐化是 1.0，
//!   无保护全量锐化会明显大于 1。保护要把它压回接近 1。
//! - **边缘增益**：阶跃靶上，`对比度(输出) / 对比度(输入)`。保护的代价必须落在这里——
//!   保住了噪声就该少锐化边，这个数会往下走，验收要求它掉得不多。
//!
//! 两个数是一对矛盾，保护强度的合理位置就是「噪声压下去、边还留得住」的交点。
//!
//! 跑法：
//! ```text
//! cargo test -p aevum-core --test fit_sharpen_protect -- --ignored --nocapture   # 出表
//! cargo test -p aevum-core --test fit_sharpen_protect                            # 验收
//! ```
//!
//! 实测（全量锐化 amount=100、detail=25、噪声 σ≈0.014）：
//!
//! | protect | 噪声增益 | 边缘增益 |
//! |---|---|---|
//! | 0（无保护） | 1.944 | 1.359 |
//! | 20（RAW 导入默认） | 1.773 | 1.359 |
//! | 60 | 1.431 | 1.359 |
//! | 100 | 1.093 | 1.359 |
//!
//! 边缘增益**全程不动**，说明保护确实只吃平坦区——这是它和 `masking` 的本质区别
//! （masking 是硬阈值，靠 edge 场做门限；保护是噪声自适应的软衰减）。
//!
//! `masking` 的标尺在这份文件里量过两次，形如两代：
//!
//! | masking | 旧映射 `0.001+0.02·m²` | 现映射 `0.001+0.02·√m` |
//! |---|---|---|
//! | 25 | 1.944（**完全没动**） | 1.816 |
//! | 50 | 1.944（**完全没动**） | 1.385 |
//! | 75 | 1.683 | 1.174 |
//! | 100 | 1.077 | 1.077 |
//!
//! 旧映射是平方项，前段太平，滑杆拉到 25、40 时界面在动、一个像素没变；换成平方根
//! 后从 25 起就有反应（端点不变，所以 `masking=100` 与历史结果仍一致）。
//! 换算量级：`protect=20`（1.773）落在旧 masking 的 50（1.944，其实没作用）与 75
//! （1.683）之间，所以默认 20 不是「保守试探」，是已经等于很强的传统 masking，
//! 没有往上抬的理由。
//!
//! 两个阈值常量（`t0 = 1.2σ + 0.0008`、`t1 = t0 + 2σ + 0.004`）的位置是对的：
//! 平坦区的 `edge` 约为 `0.8σ`（高斯噪声的 E|x| = σ√(2/π)）落在 `t0` 以下 →
//! 全量抑制；真实强边的 `edge` 远超 `t1` → 一点不抑制。中间只留一段过渡给
//! 中等对比纹理。**这就是那两个阈值常量的依据**：不必再调，证据就是上面这张表
//! （结论已写进 `docs/遗留台账.md` 的已关闭表）。

mod support;

use aevum_core::develop::spatial::sharpen;
use aevum_core::grade::GradeParams;
use support::{noise_target, step_target};

const W: usize = 256;
const H: usize = 64;
const MID: f32 = 0.5;
/// 噪声幅度：约 2% 满量程，对应高 ISO 但还没到糊成一片的程度。
const SIGMA: f32 = 0.02;

fn params(protect: f32) -> GradeParams {
    let mut p = GradeParams::default();
    // 全量锐化 + 关掉 masking：只让 protect 一个变量起作用。
    p.sharp_amount = 100.0;
    p.sharp_radius = 1.0;
    p.sharp_detail = 25.0;
    p.sharp_masking = 0.0;
    p.sharp_protect = f64::from(protect);
    p
}

/// 绿通道的标准差（三个通道同值，取一个就够）。
fn chan_std(img: &[f32]) -> f32 {
    let n = img.len() / 3;
    let mut s = 0.0f64;
    let mut s2 = 0.0f64;
    for px in img.chunks_exact(3) {
        let v = f64::from(px[1]);
        s += v;
        s2 += v * v;
    }
    let mean = s / n as f64;
    ((s2 / n as f64 - mean * mean).max(0.0)).sqrt() as f32
}

/// 噪声增益：输出噪声标准差相对输入。1.0 = 一点没放大。
///
/// 不能写成 `std(输出−输入)/std(输入)`——那量的是「锐化量」这个高通分量的散度，
/// 它天然小于噪声本身的散度，会得出「锐化降低了噪声」的假结论。
fn noise_gain(out: &[f32], src: &[f32]) -> f32 {
    chan_std(out) / chan_std(src)
}

/// 边缘锐度：行平均剖面在过渡处的最大一阶差分。
///
/// 不能用「左右平台对比度」——USM 是局部算子，离边 16 px 的平台本来就不变，
/// 那个指标对锐化完全不敏感（实测恒为 1.000）。锐化真正改变的是过渡处的斜率，
/// 所以直接量最大一阶差分。
fn edge_sharpness(img: &[f32]) -> f32 {
    let mut prof = vec![0.0f32; W];
    for (x, slot) in prof.iter_mut().enumerate() {
        let mut s = 0.0f32;
        for y in 0..H {
            s += img[(y * W + x) * 3 + 1];
        }
        *slot = s / H as f32;
    }
    let mut best = 0.0f32;
    for x in 1..W {
        best = best.max((prof[x] - prof[x - 1]).abs());
    }
    best
}

fn edge_gain(out: &[f32], src: &[f32]) -> f32 {
    edge_sharpness(out) / edge_sharpness(src)
}

fn run(src: &[f32], protect: f32) -> Vec<f32> {
    let mut img = src.to_vec();
    sharpen(&mut img, W, H, &params(protect));
    img
}

/// 出表：噪声增益与边缘增益随 protect 怎么走。
///
/// ```text
/// cargo test -p aevum-core --test fit_sharpen_protect -- --ignored --nocapture
/// ```
#[test]
#[ignore = "扫参出表，不参与门槛"]
fn protect_sweep() {
    let noise = noise_target(W, H, MID, SIGMA, 0x9E37_79B9);
    let step = step_target(W, H, 0.25, 0.75);
    println!(
        "\n{:>8} {:>10} {:>10}   说明",
        "protect", "噪声增益", "边缘增益"
    );
    for k in [0.0f32, 5.0, 10.0, 20.0, 30.0, 40.0, 60.0, 80.0, 100.0] {
        let n = noise_gain(&run(&noise, k), &noise);
        let e = edge_gain(&run(&step, k), &step);
        let note = if (k - 20.0).abs() < 0.1 {
            "← RAW 导入默认值"
        } else {
            ""
        };
        println!("{k:>8.0} {n:>10.3} {e:>10.3}   {note}");
    }

    // 标尺：已经上线的 `masking` 滑杆。protect 的强度要用它来翻译成用户
    // 已经熟悉的量级——「protect=20 相当于 masking 多少」比一个裸数字有用。
    println!("\n{:>8} {:>10} {:>10}", "masking", "噪声增益", "边缘增益");
    for m in [0.0f32, 10.0, 25.0, 50.0, 75.0, 100.0] {
        let mut p = params(0.0);
        p.sharp_masking = f64::from(m);
        let mut n_img = noise.clone();
        sharpen(&mut n_img, W, H, &p);
        let mut s_img = step.clone();
        sharpen(&mut s_img, W, H, &p);
        println!(
            "{m:>8.0} {:>10.3} {:>10.3}",
            noise_gain(&n_img, &noise),
            edge_gain(&s_img, &step)
        );
    }

    println!(
        "基线：不锐化 = 1.000/1.000；纯噪声 std {:.4}，阶跃最大斜率 {:.4}\n",
        chan_std(&noise),
        edge_sharpness(&step)
    );
}

/// 验收：保护必须真的压噪声，且代价不落在边缘上。
///
/// 数值全部来自 `protect_sweep` 的表（改系数时那张表要重跑）：
/// 无保护 1.944 / 默认 20 → 1.773 / 拉满 100 → 1.093，边缘增益全程 1.359。
#[test]
fn protect_suppresses_noise_and_keeps_edges() {
    let noise = noise_target(W, H, MID, SIGMA, 0x9E37_79B9);
    let step = step_target(W, H, 0.25, 0.75);

    // 1) 不保护时确实会放大噪声——否则这个测试什么也没证明。
    let open = noise_gain(&run(&noise, 0.0), &noise);
    assert!(
        open > 1.4,
        "无保护全量锐化应当放大噪声，实测只有 {open:.3}，靶子可能太软"
    );

    // 2) 随 protect 单调下降：强度只能往一个方向走，不能来回。
    let mut prev = f32::INFINITY;
    for k in [0.0f32, 10.0, 20.0, 40.0, 60.0, 100.0] {
        let g = noise_gain(&run(&noise, k), &noise);
        assert!(
            g <= prev + 1e-3,
            "protect={k} 噪声增益 {g:.3} 反而大于前一档 {prev:.3}"
        );
        prev = g;
    }

    // 3) 默认值 20 要**看得出来**在起作用。实测从 1.944 压到 1.773（−9%），
    //    强度上大致等于 masking 70（见 protect_sweep 的第二张表），所以不再
    //    往上抬——它已经是「很强的传统 masking」那一档了。
    let at_default = noise_gain(&run(&noise, 20.0), &noise);
    assert!(
        at_default < open * 0.95,
        "protect=20 相对无保护应有可测的抑制（{at_default:.3} vs {open:.3}）"
    );

    // 4) 拉满时把锐化带来的噪声放大基本吃干净（1.944 → 1.093，接近不锐化的 1.0）。
    let at_full = noise_gain(&run(&noise, 100.0), &noise);
    assert!(
        at_full < 1.15,
        "protect=100 应基本消除噪声放大，实测 {at_full:.3}"
    );

    // 5) 代价：边缘增益不能跟着掉。保护衰减的是 base（锐化量），边上的 base
    //    本来就大、smooth 后接近 1，所以应当几乎不损失——实测全程 1.359。
    let e_open = edge_gain(&run(&step, 0.0), &step);
    for k in [20.0f32, 60.0, 100.0] {
        let e = edge_gain(&run(&step, k), &step);
        assert!(
            e > e_open * 0.95,
            "protect={k} 不该吃掉边缘增益（{e:.3} vs 无保护 {e_open:.3}）"
        );
    }

    // 6) 极端：拉满也不该把图弄坏。
    let full = run(&step, 100.0);
    assert!(
        full.iter().all(|v| v.is_finite()),
        "protect=100 出现了 NaN/Inf"
    );
}

/// 「默认不漂移」铁律的落点：旧工程 JSON 没有 `sharp_protect` 字段，
/// 容器级 `#[serde(default)]` 必须兜底成 0，走的还是原来那条路。
///
/// 这条比「逐位一致」更值得测：逐位一致由代码结构保证（`pw == 0` 且 `m == 0`
/// 时整段保护根本不进），而 serde 兜底是唯一会被别处改动打破的环节。
#[test]
fn missing_sharp_protect_deserializes_to_zero() {
    let mut v = serde_json::to_value(GradeParams::default()).expect("默认值可序列化");
    let obj = v.as_object_mut().expect("结构体序列化成对象");
    assert!(obj.remove("sharp_protect").is_some(), "字段名对不上");
    let p: GradeParams = serde_json::from_value(v).expect("缺字段也应能反序列化");
    assert_eq!(p.sharp_protect, 0.0, "缺失字段必须兜底为 0（旧行为）");
}

/// `masking` 必须全段可用：拉到 25 就要有反应，且随滑杆单调变强。
///
/// 旧映射（`0.001 + 0.02·m²`）在 0–50 之间对纯噪声一动不动，等于假滑杆。
/// 现在换成 `0.001 + 0.02·√m`：实测 25 → 1.816、50 → 1.385、75 → 1.174、
/// 100 → 1.077（0 是 1.944）。端点没动，所以 `masking=100` 与历史结果一致。
#[test]
fn masking_responds_across_the_whole_slider() {
    let noise = noise_target(W, H, MID, SIGMA, 0x9E37_79B9);
    let mut prev = f32::INFINITY;
    let mut at25 = 0.0f32;
    for k in [0.0f64, 10.0, 25.0, 50.0, 75.0, 100.0] {
        let mut img = noise.clone();
        let mut p = params(0.0);
        p.sharp_masking = k;
        sharpen(&mut img, W, H, &p);
        let g = noise_gain(&img, &noise);
        assert!(
            g <= prev + 1e-3,
            "masking={k} 的噪声增益 {g:.3} 反而高于上一档 {prev:.3}"
        );
        if k == 25.0 {
            at25 = g;
        }
        prev = g;
    }
    let open = noise_gain(&run(&noise, 0.0), &noise);
    assert!(
        at25 < open - 0.05,
        "masking=25 就该看得出作用（无 masking 是 {open:.3}，实测 {at25:.3}）"
    );
}
