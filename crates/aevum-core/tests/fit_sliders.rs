//! 十个「界面能拉、但只有金标对账」的滑杆：把每个都变成可量的数。
//!
//! `GradeParams` 有 86 个字段，其中 10 个在界面上能拉、**集成测试里一次都没出现过**：
//! `hl_rolloff` / `contrast_linear` / `black_soft` / `white_soft` /
//! `curve_hdr_tail` / `dehaze_dark` / `dehaze_contrast` / `lens_distort` /
//! `lens_vignette` / `nr_ai`。
//!
//! 它们并非完全没有验证——金标 `develop.json` 的 case 里设了这些参数，逐像素与
//! Python 参考实现对账。但金标证明的是「Rust == Python」，**不是「系数对」**：
//! 两边写的是同一个猜值，一起错就永远绿。`masking` 的死区（0–60 对纯噪声完全
//! 不生效）就是这样越过全部门槛上线的。
//!
//! 所以这里补的是**自证式**判据，不依赖任何参考实现：默认 0 是否逐位不动、随参数
//! 是否单调、效果是否落在它该管的那一段、拉满会不会炸。每条断言都对应扫参表里
//! 量到的数字，表在 `slider_sweep` / `narrow_band_sweep` 两个 `#[ignore]` 测试里。
//!
//! 跑法：
//! ```text
//! cargo test -p aevum-core --test fit_sliders -- --ignored --nocapture   # 出表
//! cargo test -p aevum-core --test fit_sliders                            # 验收
//! ```
//!
//! # 三个「单独拉没有对象可修饰」的项
//!
//! 它们不是坏滑杆，但单看数字像是没反应，测的时候必须把前置条件摆上：
//!
//! | 参数 | 前置 | 为什么 |
//! |---|---|---|
//! | `contrast_linear` | `contrast ≠ 0` | 它接管的是 `contrast` 的增量算法，`contrast=0` 时整个混合分支不进 |
//! | `black_soft` | `blacks < 0` | 它软的是 0 那一侧的硬钳，像素没被压到 0 以下就无处可软 |
//! | `dehaze_contrast` | `dehaze_dark ≠ 0` | 对比项叠在暗通道去雾之后，没去雾就没有可对比的东西 |
//!
//! # `curve_hdr_tail` 的边界：它靠曝光段产生的 > 1，不靠外部输入
//!
//! 写这份文件时我先写下过一个错结论——「曲线尾部在整条链里完全无效」，理由是
//! `spatial::lens` 在渲染起点无条件 `clamp01`（Python `_lens` 同样
//! `np.clip(x, 0, 1)`）。**这个结论是错的**：真实管线里 > 1 是**曝光段**造出来的
//! （`lin_to_srgb(lin·gain)` 可以越过 1），而镜头钳位在曝光**之前**，所以钳位碰
//! 不到它。实测 +2EV 时 `tail=0` 与 `tail=100` 的最大差 0.098，滑杆是有效的。
//!
//! 钳位真正挡住的是「从外部直接喂进 > 1 的输入」——解码器输出的编码值本来就是
//! ≤ 1，所以当时没有可见影响；但将来要做 HDR 解码 / 16-bit 线性输入时，这一句
//! 会在第一步就把动态范围削平。**这一处已经改掉**（只压负值、向上不封顶），
//! 两条各一个测试：[`curve_hdr_tail_works_on_exposure_headroom`]（靠曝光的 >1 有效）
//! 与 [`external_hdr_input_survives_the_lens_stage`]（外部 >1 能活到曲线）。

mod support;

use aevum_core::develop::{self, Ev, Opts, lut};
use aevum_core::grade::GradeParams;
use support::{noise_target, psnr};

const W: usize = 128;
const H: usize = 32;

// ---------------------------------------------------------------------------
// 靶
// ---------------------------------------------------------------------------

/// 横向亮度斜坡（三通道同值），`lo` → `hi`。
fn ramp(w: usize, h: usize, lo: f32, hi: f32) -> Vec<f32> {
    let mut img = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let t = x as f32 / (w - 1) as f32;
            let v = lo + (hi - lo) * t;
            let i = (y * w + x) * 3;
            img[i] = v;
            img[i + 1] = v;
            img[i + 2] = v;
        }
    }
    img
}

/// 雾靶：低对比方波 + 抬黑。去雾要「压黑位」和「拉开中段对比」都在这里。
fn hazy(w: usize, h: usize) -> Vec<f32> {
    let mut img = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let s = if (x / 4).is_multiple_of(2) {
                0.42
            } else {
                0.52
            };
            let i = (y * w + x) * 3;
            img[i] = s;
            img[i + 1] = s;
            img[i + 2] = s;
        }
    }
    img
}

/// 方波靶：量暗角对四角的影响（角上本来就是暗带，比率变化只来自暗角）。
fn bars(w: usize, h: usize) -> Vec<f32> {
    let mut img = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let s = if (x / 8).is_multiple_of(2) {
                0.15
            } else {
                0.85
            };
            let i = (y * w + x) * 3;
            img[i] = s;
            img[i + 1] = s;
            img[i + 2] = s;
        }
    }
    img
}

// ---------------------------------------------------------------------------
// 量具
// ---------------------------------------------------------------------------

fn render(src: &[f32], p: &GradeParams, w: usize, h: usize) -> Vec<f32> {
    let mut img = src.to_vec();
    develop::render(&mut img, w, h, p, Ev::default(), Opts::FULL);
    img
}

fn px(img: &[f32], w: usize, x: usize, y: usize) -> f32 {
    img[(y * w + x) * 3 + 1]
}

fn mean(img: &[f32]) -> f32 {
    let n = img.len() / 3;
    img.chunks_exact(3).map(|p| p[1]).sum::<f32>() / n as f32
}

/// 全图标准差（绿通道）：对比度的粗量。
fn std_dev(img: &[f32]) -> f32 {
    let n = img.len() / 3;
    let m = mean(img);
    let s: f32 = img.chunks_exact(3).map(|p| (p[1] - m) * (p[1] - m)).sum();
    (s / n as f32).sqrt()
}

/// 最内圈（±1/16）的最大逐像素差：几何重映射在中心应当纹丝不动。
fn max_diff_inner(a: &[f32], b: &[f32], w: usize, h: usize) -> f32 {
    let mut best = 0.0f32;
    for y in h / 2 - h / 16..h / 2 + h / 16 {
        for x in w / 2 - w / 16..w / 2 + w / 16 {
            best = best.max((px(a, w, x, y) - px(b, w, x, y)).abs());
        }
    }
    best
}

/// 四角与中心的均值比（暗角口径）。
fn corner_center_ratio(img: &[f32], w: usize, h: usize) -> f32 {
    let mut corner = 0.0f32;
    let mut center = 0.0f32;
    for y in 0..h / 8 {
        for x in 0..w / 8 {
            corner += px(img, w, x, y);
            corner += px(img, w, w - 1 - x, y);
            corner += px(img, w, x, h - 1 - y);
            corner += px(img, w, w - 1 - x, h - 1 - y);
            center += px(img, w, w / 2 - 1 + x / 4, h / 2 - 1 + y / 4);
        }
    }
    let n = (w / 8 * h / 8) as f32;
    (corner / (4.0 * n)) / (center / n)
}

fn max_of(img: &[f32], w: usize, h: usize) -> f32 {
    let mut best = f32::MIN;
    for y in 0..h {
        for x in 0..w {
            best = best.max(px(img, w, x, y));
        }
    }
    best
}

/// 一条 S 曲线（0–255 坐标）：把 128 压到 102，终点可指定。
///
/// **坐标是 0–255 不是 0–1**：`curve_lut` 把 pchip 的结果除以 255 才当系数用，
/// 喂 0–1 会得到一条贴地的直线（输出恒为 1/255），什么都量不出来。
fn s_curve(end: f64) -> Vec<[f64; 2]> {
    vec![[0.0, 0.0], [128.0, 102.0], [255.0, end]]
}

// ---------------------------------------------------------------------------
// 出表
// ---------------------------------------------------------------------------

/// 十个滑杆在 0 / 25 / 50 / 75 / 100 上的效果。
#[test]
#[ignore]
fn slider_sweep() {
    let levels = [0.0f64, 25.0, 50.0, 75.0, 100.0];
    let tones = ramp(W, H, 0.0, 1.0);
    let fog = hazy(W, H);
    let grid = bars(W, H);
    let smooth_ramp = ramp(W, H, 0.1, 0.9);

    println!(
        "\n{:<22} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "参数", "值", "x=0.05", "x=0.25", "x=0.5", "x=0.95", "std"
    );
    for (name, set) in [
        (
            "hl_rolloff",
            (|p: &mut GradeParams, v| p.hl_rolloff = v) as fn(&mut GradeParams, f64),
        ),
        ("contrast_linear", |p, v| p.contrast_linear = v),
        ("black_soft", |p, v| p.black_soft = v),
        ("white_soft", |p, v| p.white_soft = v),
    ] {
        // 修饰项要把前置条件摆上，否则整段不进，看起来像坏滑杆。
        let mut base = GradeParams::default();
        match name {
            "contrast_linear" => base.contrast = 50.0,
            "black_soft" => base.blacks = -100.0,
            "white_soft" => base.whites = 100.0,
            _ => {}
        }
        for v in levels {
            let mut p = base.clone();
            set(&mut p, v);
            let out = render(&tones, &p, W, H);
            println!(
                "{name:<22} {v:>6.0} {:>8.4} {:>8.4} {:>8.4} {:>8.4} {:>8.4}",
                px(&out, W, W / 20, H / 2),
                px(&out, W, W / 4, H / 2),
                px(&out, W, W / 2, H / 2),
                px(&out, W, W - 1 - W / 20, H / 2),
                std_dev(&out)
            );
        }
    }

    println!(
        "\n{:<22} {:>6} {:>8} {:>8} {:>8}",
        "参数", "值", "雾std", "暗位", "亮位"
    );
    for (name, set) in [
        (
            "dehaze_dark",
            (|p: &mut GradeParams, v| p.dehaze_dark = v) as fn(&mut GradeParams, f64),
        ),
        ("dehaze_contrast", |p, v| p.dehaze_contrast = v),
    ] {
        for v in levels {
            let mut p = GradeParams::default();
            if name == "dehaze_contrast" {
                p.dehaze_dark = 100.0;
            }
            set(&mut p, v);
            let out = render(&fog, &p, W, H);
            println!(
                "{name:<22} {v:>6.0} {:>8.4} {:>8.4} {:>8.4}",
                std_dev(&out),
                px(&out, W, 0, H / 2),
                px(&out, W, 4, H / 2)
            );
        }
    }

    println!(
        "\n{:<22} {:>6} {:>9} {:>9} {:>9}",
        "参数", "值", "角/中", "中心位移", "PSNR"
    );
    for (name, set) in [
        (
            "lens_vignette",
            (|p: &mut GradeParams, v| p.lens_vignette = v) as fn(&mut GradeParams, f64),
        ),
        ("lens_distort", |p, v| p.lens_distort = v),
    ] {
        // 畸变是几何重映射，用连续斜坡量位移才不会像方波靶那样饱和。
        let tgt = if name == "lens_distort" {
            &smooth_ramp
        } else {
            &grid
        };
        for v in levels {
            let mut p = GradeParams::default();
            set(&mut p, v);
            let out = render(tgt, &p, W, H);
            println!(
                "{name:<22} {v:>6.0} {:>9.4} {:>9.4} {:>9.2}",
                corner_center_ratio(&out, W, H),
                max_diff_inner(&out, tgt, W, H),
                psnr(&out, tgt, 1.0)
            );
        }
    }

    println!(
        "\n{:<22} {:>6} {:>9} {:>9}",
        "参数", "值", "噪声std", "噪声增益"
    );
    let noisy = noise_target(W, H, 0.5, 0.02, 0x51D2_9E37);
    let plain = render(&noisy, &GradeParams::default(), W, H);
    for v in levels {
        let mut p = GradeParams::default();
        p.nr_ai = v;
        let out = render(&noisy, &p, W, H);
        println!(
            "{:<22} {v:>6.0} {:>9.4} {:>9.4}",
            "nr_ai",
            std_dev(&out),
            std_dev(&out) / std_dev(&plain)
        );
    }
}

/// 三个「效果藏在窄带里」的项：高光细节、黑场软拐点的过渡带、曲线尾部。
#[test]
#[ignore]
fn narrow_band_sweep() {
    let hi = ramp(W, H, 0.90, 0.99);
    println!(
        "\n{:<18} {:>6} {:>9} {:>9}",
        "hl_rolloff", "值", "细节std", "最亮"
    );
    for v in [0.0f64, 25.0, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.hl_rolloff = v;
        let out = render(&hi, &p, W, H);
        println!(
            "{:<18} {v:>6.0} {:>9.5} {:>9.4}",
            "hl_rolloff",
            std_dev(&out),
            px(&out, W, W - 1, H / 2)
        );
    }

    let dark = ramp(W, H, 0.0, 0.30);
    println!(
        "\n{:<18} {:>6} {:>9} {:>9} {:>9}",
        "black_soft", "值", "x=0.06", "x=0.12", "最大"
    );
    for v in [0.0f64, 25.0, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.blacks = -100.0;
        p.black_soft = v;
        let out = render(&dark, &p, W, H);
        let mx = (0..W).map(|x| px(&out, W, x, H / 2)).fold(0.0f32, f32::max);
        println!(
            "{:<18} {v:>6.0} {:>9.5} {:>9.5} {:>9.5}",
            "black_soft",
            px(&out, W, (W as f32 * 0.2) as usize, H / 2),
            px(&out, W, (W as f32 * 0.4) as usize, H / 2),
            mx
        );
    }

    let over = ramp(W, H, 0.5, 1.9);
    println!(
        "\n{:<18} {:>6} {:>9} {:>9} {:>9} {:>9}",
        "curve_hdr_tail", "值", "x=0.8", "x=1.1", "x=1.5", "x=1.8"
    );
    for v in [0.0f64, 25.0, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.curve_hdr_tail = v;
        p.curve_rgb = s_curve(230.0);
        let out = render(&over, &p, W, H);
        let at = |t: f32| px(&out, W, (W as f32 * t) as usize, H / 2);
        println!(
            "{:<18} {v:>6.0} {:>9.4} {:>9.4} {:>9.4} {:>9.4}",
            "curve_hdr_tail",
            at(0.21),
            at(0.45),
            at(0.71),
            at(0.99)
        );
    }
}

// ---------------------------------------------------------------------------
// 验收
// ---------------------------------------------------------------------------

/// 十个都是「默认 0，旧路径逐位不动」的写法。这一条把它们一起钉住。
///
/// 系数会被反复调，但「0 的语义」不能变：存量工程里这些字段多半是 0，
/// 一漂移就是几千张图跟着变。
#[test]
fn explicit_zero_is_bit_identical_to_default() {
    let src = ramp(W, H, 0.0, 1.0);
    let base = render(&src, &GradeParams::default(), W, H);
    let mut p = GradeParams::default();
    p.hl_rolloff = 0.0;
    p.contrast_linear = 0.0;
    p.black_soft = 0.0;
    p.white_soft = 0.0;
    p.curve_hdr_tail = 0.0;
    p.dehaze_dark = 0.0;
    p.dehaze_contrast = 0.0;
    p.lens_distort = 0.0;
    p.lens_vignette = 0.0;
    p.nr_ai = 0.0;
    assert_eq!(render(&src, &p, W, H), base, "显式 0 与默认必须逐位一致");
}

/// `hl_rolloff`：只抬 `y > knee` 的那一段，中低段一个像素不动。
///
/// 实测（`slider_sweep` / `narrow_band_sweep`）：x=0.05/0.25/0.5 全程 0.0472 /
/// 0.2520 / 0.5039 不变；x=0.95 从 0.9528 单调抬到 1.0300 之前的上限 1.0。
///
/// **代价要记住**：k≥25 起最亮端就被推到 1.0 了，也就是 0.90–0.99 这段高光
/// 细节会被压掉一部分。它是「抬亮」不是「压缩」——滑杆名字里的 rolloff 指
/// 曝光肩部的形状，不是「保住高光细节」。
#[test]
fn hl_rolloff_touches_only_the_top_end() {
    let src = ramp(W, H, 0.0, 1.0);
    let base = render(&src, &GradeParams::default(), W, H);
    // knee 在**线性**空间取 0.75，对应编码约 0.89。所以采样点必须贴着这个落点：
    // 只放 0.05/0.25/0.5 的话，把 knee 改到 0.30（编码 0.58）测试照样绿。
    let at = |t: f32| (W as f32 * t).min(W as f32 - 1.0) as usize;
    let untouched = [at(0.20), at(0.50), at(0.85)];
    let onset = at(0.92);
    let top = at(0.95);
    let mut prev = px(&base, W, top, H / 2);
    for k in [25.0f64, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.hl_rolloff = k;
        let out = render(&src, &p, W, H);
        for &x in &untouched {
            assert!(
                (px(&out, W, x, H / 2) - px(&base, W, x, H / 2)).abs() < 1e-6,
                "hl_rolloff={k} 动了 knee 以下的 x={x}"
            );
        }
        // knee 之上必须真的动——否则滑杆就是个装饰。
        assert!(
            px(&out, W, onset, H / 2) > px(&base, W, onset, H / 2) + 1e-6,
            "hl_rolloff={k} 在编码 0.92 处没抬起来，knee 位置可能被改了"
        );
        let hi = px(&out, W, top, H / 2);
        assert!(
            hi >= prev - 1e-6,
            "hl_rolloff={k} 的高光 {hi:.4} 比上一档 {prev:.4} 还低"
        );
        assert!(hi <= 1.0 + 1e-6, "hl_rolloff={k} 把高光推到 1 以上：{hi}");
        prev = hi;
    }
    // 拉满时最亮端顶到白——「会削掉一点高光细节」是已知代价，这里只钉现状。
    let mut p = GradeParams::default();
    p.hl_rolloff = 100.0;
    let out = render(&src, &p, W, H);
    assert!(
        (max_of(&out, W, H) - 1.0).abs() < 1e-5,
        "hl_rolloff=100 应当把最亮端抬到 1.0"
    );
}

/// `contrast_linear`：线性光域的 S 曲线**绕中灰转**，不再顺手压暗中灰。
///
/// 修之前，`s_lin` 是直接对线性值做的 smoothstep，拐点落在线性 0.5，也就是编码
/// 0.735——等于把对比度的支点抬到 73% 灰。实测 contrast=50 时中灰 0.5049 被压到
/// 0.4487，即拉满 `contrast_linear` 白吃 14 级（8 位）。
///
/// 现在先把线性值重标定成 `u = xl^γ`（使中灰落到 u=0.5），做完 smoothstep 再
/// 拉回，于是 S(中灰)=中灰。实测中灰全档 0.5049 不动，阴影/高光各自挪 0.001–0.003。
///
/// **顺带说清一件事**：修好支点之后这个滑杆的可见差异只剩 0.005 量级——它原来的
/// 「效果」绝大部分来自那个错位的支点，而不是线性光域本身。
#[test]
fn contrast_linear_keeps_mid_gray_fixed() {
    let src = ramp(W, H, 0.0, 1.0);
    let mut gamma = GradeParams::default();
    gamma.contrast = 50.0;
    let g = render(&src, &gamma, W, H);
    let mid = px(&g, W, W / 2, H / 2);
    let mut moved = 0.0f32;
    for k in [25.0f64, 50.0, 75.0, 100.0] {
        let mut p = gamma.clone();
        p.contrast_linear = k;
        let out = render(&src, &p, W, H);
        assert!(
            (px(&out, W, W / 2, H / 2) - mid).abs() < 1e-4,
            "contrast_linear={k} 动了中灰：{:.4} vs {mid:.4}",
            px(&out, W, W / 2, H / 2)
        );
        // 整条剖面取最大差异，别只看两个点。
        let d = (0..W)
            .map(|x| (px(&out, W, x, H / 2) - px(&g, W, x, H / 2)).abs())
            .fold(0.0f32, f32::max);
        moved = moved.max(d);
    }
    // 实测最大差异 0.0041（编码 0.18 附近，contrast=50、ck=100）——约 1 级。
    // 上界 0.01 是给支点回归用的：谁把 `s_lin` 改回绕线性 0.5 转，差异立刻回到
    // 0.05 量级（中灰被压暗 14 级），这里就先红。真要在线性光域做更强的对比度，
    // 该改的是 `contrast` 的强度曲线，不是这个支点。
    assert!(
        moved > 0.001 && moved < 0.01,
        "contrast_linear 的可见差异应当只有 1 级上下，实测 {moved:.4}"
    );
}

/// `black_soft`：把 0 那一侧的硬钳换成抛物线拐点。
///
/// 必须先把像素压到 0 以下才看得见，所以配 `blacks=-100` + 0–0.30 的暗部斜坡。
/// 实测：x=0.12 处从 0.0000 单调抬到 0.0244，最深处（x=0.06）与亮部都不动。
#[test]
fn black_soft_rounds_the_zero_clip() {
    let src = ramp(W, H, 0.0, 0.30);
    let probe = (W as f32 * 0.4) as usize;
    let deep = (W as f32 * 0.2) as usize;
    let mut base = GradeParams::default();
    base.blacks = -100.0;
    let hard = render(&src, &base, W, H);
    assert!(
        px(&hard, W, probe, H / 2) == 0.0,
        "前提不成立：blacks=-100 应当把 x=0.12 压到 0"
    );
    let mut prev = 0.0f32;
    for k in [25.0f64, 50.0, 75.0, 100.0] {
        let mut p = base.clone();
        p.black_soft = k;
        let out = render(&src, &p, W, H);
        let v = px(&out, W, probe, H / 2);
        assert!(
            v >= prev - 1e-6,
            "black_soft={k} 的软拐点 {v:.5} 反而低于上一档 {prev:.5}"
        );
        assert!(
            px(&out, W, deep, H / 2) == 0.0,
            "black_soft={k} 不该把最深处从 0 拉起来"
        );
        assert!(
            (max_of(&out, W, H) - max_of(&hard, W, H)).abs() < 1e-6,
            "black_soft={k} 不该动亮部"
        );
        prev = v;
    }
    assert!(prev > 0.02, "black_soft=100 应当明显抬起过渡带：{prev:.5}");
}

/// `white_soft`：把 1 那一侧（这里是 `whites` 推出来的过冲）的硬钳换成软肩。
///
/// 实测（whites=100）：x=0.95 从 0.9746 单调压回 0.9242，且全程不越 1。
#[test]
fn white_soft_pulls_highlights_back_into_range() {
    let src = ramp(W, H, 0.0, 1.0);
    let probe = W - 1 - W / 20;
    let mut base = GradeParams::default();
    base.whites = 100.0;
    let mut prev = f32::INFINITY;
    for k in [0.0f64, 25.0, 50.0, 75.0, 100.0] {
        let mut p = base.clone();
        p.white_soft = k;
        let out = render(&src, &p, W, H);
        let v = px(&out, W, probe, H / 2);
        assert!(
            v <= prev + 1e-6,
            "white_soft={k} 的高光 {v:.4} 比上一档 {prev:.4} 还高"
        );
        assert!(v <= 1.0 + 1e-6, "white_soft={k} 越界：{v}");
        prev = v;
    }
    assert!(prev < 0.95, "white_soft=100 应当把高光明显压回：{prev:.4}");
}

/// `curve_hdr_tail` 的数学本身是对的——直接喂 `tone_curve` 能看到延伸。
///
/// 这半条保护的是「算法」；下半条
/// [`curve_hdr_tail_is_inert_behind_the_lens_clamp`] 保护的是「现状」。
#[test]
fn curve_hdr_tail_math_extends_above_one() {
    let mut p = GradeParams::default();
    p.curve_rgb = s_curve(230.0);
    let mut off = vec![1.5f32; 3];
    lut::tone_curve(&mut off, &p);
    let mut p_on = p.clone();
    p_on.curve_hdr_tail = 100.0;
    let mut on = vec![1.5f32; 3];
    lut::tone_curve(&mut on, &p_on);
    assert!(
        (off[0] - 230.0 / 255.0).abs() < 1e-3,
        "tail=0 时 x>1 应当停在曲线终点值，实测 {:.4}",
        off[0]
    );
    assert!(
        on[0] > off[0] + 0.05,
        "tail=100 应当向上延伸，实测 {:.4}",
        on[0]
    );
    assert!(on[0] <= 1.0 + 1e-6);
    // x ≤ 1 的主路径一个像素都不能动。
    let mut a = vec![0.4f32; 3];
    let mut b = vec![0.4f32; 3];
    lut::tone_curve(&mut a, &p);
    lut::tone_curve(&mut b, &p_on);
    assert_eq!(a, b, "tail 不该影响 x ≤ 1 的主路径");
}

/// 真实路径：`curve_hdr_tail` 对**曝光段造出来的** > 1 有效。
///
/// +2EV 把高光推到 1 以上，尾部按 PCHIP 末端斜率继续延伸，所以 `tail` 越大、
/// 出来的高光越亮。这条是「滑杆确实有用」的正证。
#[test]
fn curve_hdr_tail_works_on_exposure_headroom() {
    let src = ramp(W, H, 0.55, 0.95);
    let mut p = GradeParams::default();
    p.exposure = 2.0;
    p.curve_rgb = s_curve(230.0);
    let mut prev = 0.0f32;
    for k in [0.0f64, 25.0, 50.0, 75.0, 100.0] {
        let mut q = p.clone();
        q.curve_hdr_tail = k;
        let out = render(&src, &q, W, H);
        let mx = max_of(&out, W, H);
        assert!(
            mx >= prev - 1e-6,
            "curve_hdr_tail={k} 的高光 {mx:.4} 比上一档 {prev:.4} 还低"
        );
        assert!(mx <= 1.0 + 1e-6);
        prev = mx;
    }
    assert!(
        prev > 0.93,
        "tail=100 应当把过曝区继续推高，实测最高 {prev:.4}"
    );
}

/// 镜头阶段**不再向上封顶**：外部喂进的 >1 输入能一路活到曲线。
///
/// 旧口径是 `clamp01`（Python `_lens` 同样 `np.clip(x, 0, 1)`）。解码器输出本来
/// ≤1，所以放开上界今天看不出变化；放开是为了 HDR 解码与 16-bit 线性输入，
/// 否则动态范围在第一步就被削平。负值仍然要压掉——下游 `lin_to_srgb` 对负数
/// 做 powf 会出 NaN。
#[test]
fn external_hdr_input_survives_the_lens_stage() {
    let src = ramp(W, H, 0.5, 1.9);
    let mut off = GradeParams::default();
    off.curve_rgb = s_curve(230.0);
    let mut on = off.clone();
    on.curve_hdr_tail = 100.0;
    let a = render(&src, &off, W, H);
    let b = render(&src, &on, W, H);
    let d = a
        .iter()
        .zip(&b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(
        d > 0.05,
        "tail 对外部 >1 输入应当有效（说明 >1 活到了曲线），实测最大差 {d:.4}"
    );
    // tail=0 时 x>1 停在曲线终点值，这是「不延伸」的定义。
    assert!(
        (max_of(&a, W, H) - 230.0 / 255.0).abs() < 1e-3,
        "tail=0 时应当停在曲线终点值"
    );
    // 负值必须被压掉，不能把 NaN 传下去。
    let mut neg = ramp(W, H, 0.0, 1.0);
    for (i, v) in neg.iter_mut().enumerate() {
        if i % 4 == 0 {
            *v = -0.2;
        }
    }
    let out = render(&neg, &GradeParams::default(), W, H);
    assert!(out.iter().all(|v| v.is_finite()), "负输入产生了 NaN/Inf");
    assert!(out.iter().all(|v| *v >= 0.0), "负输入没被压到 0");
}

/// `dehaze_dark`：压黑位、拉开中段对比，亮部不动。
///
/// 实测：雾靶 std 0.0500 → 0.1926 单调上升；暗位 0.4200 → 0.1365 单调下降；
/// 亮位全程 0.5200 不动（大气光被当成基准，正是暗通道先验的做法）。
#[test]
fn dehaze_dark_lowers_the_haze_floor() {
    let src = hazy(W, H);
    let plain = render(&src, &GradeParams::default(), W, H);
    let mut prev_std = std_dev(&plain);
    let mut prev_dark = px(&plain, W, 0, H / 2);
    for k in [25.0f64, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.dehaze_dark = k;
        let out = render(&src, &p, W, H);
        let s = std_dev(&out);
        let d = px(&out, W, 0, H / 2);
        assert!(
            s >= prev_std - 1e-6,
            "dehaze_dark={k} 对比没继续拉开：{s:.4}"
        );
        assert!(
            d <= prev_dark + 1e-6,
            "dehaze_dark={k} 暗位没继续压下去：{d:.4}"
        );
        assert!(
            (px(&out, W, 4, H / 2) - 0.52).abs() < 1e-5,
            "dehaze_dark={k} 动了亮部（大气光应当保持）"
        );
        prev_std = s;
        prev_dark = d;
    }
    assert!(
        prev_dark < 0.2,
        "dehaze_dark=100 应当把雾底压到 0.2 以下：{prev_dark:.4}"
    );
}

/// `dehaze_contrast`：只有在去雾真的跑了之后才有对象可调。
///
/// 实测：单独拉它（`dehaze_dark=0`）逐位不变；配 `dehaze_dark=100` 时 std
/// 0.1926 → 0.2078 单调上升，亮位 0.5200 → 0.5482 一起抬。
///
/// 单独无效不是缺陷，但界面上两个滑杆并排、这个拉起来没反应，容易被当成坏的。
#[test]
fn dehaze_contrast_only_works_after_dehaze() {
    let src = hazy(W, H);
    let base = render(&src, &GradeParams::default(), W, H);
    for k in [25.0f64, 50.0, 100.0] {
        let mut p = GradeParams::default();
        p.dehaze_contrast = k;
        assert_eq!(
            render(&src, &p, W, H),
            base,
            "dehaze_dark=0 时 dehaze_contrast={k} 应当完全无效"
        );
    }
    let mut prev = 0.0f32;
    for k in [0.0f64, 25.0, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.dehaze_dark = 100.0;
        p.dehaze_contrast = k;
        let s = std_dev(&render(&src, &p, W, H));
        assert!(
            s >= prev - 1e-6,
            "dehaze_contrast={k} 对比没继续拉开：{s:.4}"
        );
        prev = s;
    }
    assert!(
        prev > 0.20,
        "dehaze_contrast=100 应当继续拉开对比：{prev:.4}"
    );
}

/// `lens_vignette`：正值抬四角（校正暗角），负值压四角。只动径向权重，不动几何。
///
/// 实测：角/中比 1.5385 → 1.9087 单调上升；中心位移 ≤0.0812（只是同一片
/// 亮度被乘大了，不是几何被挪了）。
#[test]
fn lens_vignette_brightens_corners_monotonically() {
    let src = bars(W, H);
    let mut prev = corner_center_ratio(&render(&src, &GradeParams::default(), W, H), W, H);
    let mut positive = Vec::new();
    for k in [25.0f64, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.lens_vignette = k;
        let out = render(&src, &p, W, H);
        let r = corner_center_ratio(&out, W, H);
        assert!(
            r >= prev - 1e-6,
            "lens_vignette={k} 角/中比没继续上升：{r:.4}"
        );
        positive.push(r);
        prev = r;
    }
    assert!(prev > 1.85, "lens_vignette=100 应当明显抬四角：{prev:.4}");
    // 负值必须往反方向走，否则「压暗角」这个方向不存在。
    let mut p = GradeParams::default();
    p.lens_vignette = -100.0;
    let dark = corner_center_ratio(&render(&src, &p, W, H), W, H);
    assert!(dark < positive[0] - 0.5, "负暗角应当压角，实测 {dark:.4}");
}

/// `lens_distort`：几何重映射，中心钉死、越靠边挪得越多。
///
/// 实测：最内圈（±1/16）位移 0.0001，PSNR 44.53 → 33.91 dB 单调下滑。
#[test]
fn lens_distort_pins_the_center() {
    let src = ramp(W, H, 0.1, 0.9);
    let mut prev = f32::INFINITY;
    for k in [25.0f64, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.lens_distort = k;
        let out = render(&src, &p, W, H);
        assert!(
            max_diff_inner(&out, &src, W, H) < 1e-3,
            "lens_distort={k} 动了画面中心（几何畸变应当钉住中心）"
        );
        let q = psnr(&out, &src, 1.0);
        assert!(q <= prev + 1e-3, "lens_distort={k} 的 PSNR {q:.2} 反而上升");
        prev = q;
    }
    assert!(
        prev < 40.0,
        "lens_distort=100 应当明显改变画面：{prev:.2} dB"
    );
}

/// `nr_ai`：真的压噪声，且干净图上不生造细节。
///
/// 实测：噪声 std 0.0141 → 0.0053（增益 0.378）单调下降；干净图 PSNR 66.44 dB
/// ——降噪网络在无噪输入上基本是恒等，不会「脑补」纹理。
#[test]
fn nr_ai_denoises_without_inventing_detail() {
    let noisy = noise_target(W, H, 0.5, 0.02, 0x51D2_9E37);
    let plain = render(&noisy, &GradeParams::default(), W, H);
    let mut prev = f32::INFINITY;
    for k in [25.0f64, 50.0, 75.0, 100.0] {
        let mut p = GradeParams::default();
        p.nr_ai = k;
        let out = render(&noisy, &p, W, H);
        let g = std_dev(&out) / std_dev(&plain);
        assert!(g <= prev + 1e-6, "nr_ai={k} 的噪声增益 {g:.4} 反而变大");
        assert!(g > 0.0, "nr_ai={k} 把图抹成一片");
        prev = g;
    }
    assert!(prev < 0.45, "nr_ai=100 应当把噪声压到一半以下：{prev:.4}");

    let clean = noise_target(W, H, 0.5, 0.0, 0x51D2_9E37);
    let mut p = GradeParams::default();
    p.nr_ai = 100.0;
    let out = render(&clean, &p, W, H);
    let q = psnr(&out, &clean, 1.0);
    assert!(q > 40.0, "干净图上 nr_ai 不该改变画面：{q:.2} dB");
}
