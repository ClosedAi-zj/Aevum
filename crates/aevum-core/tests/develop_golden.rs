//! 显影主链与 Python 实现的逐像素比对。
//!
//! 基准由 `acceptance/oracle/freeze_golden.py --develop-only` 生成：三张 48×64 的小图
//! （色相渐变 / 硬边结构 / 带噪点）过一遍 `grade.apply`，存 36 个 case 的
//! 完整输出（小端 float32 的 hex）。
//!
//! 每个 case 标了它压到哪些阶段（`tone`/`lut`/`spatial`/`vignette`/`grain`/
//! `mask`/`mask_local`）。只有当全部阶段都进了 `develop::STAGES_READY` 才逐像素
//! 比对，否则跳过——这样没搬完的运算不会假装通过，搬完一条就能立刻看见。

use std::path::PathBuf;

use aevum_core::develop::{self, Ev, Opts, STAGES_READY};
use aevum_core::grade::GradeParams;
use serde_json::Value;

const TOL: f32 = 2.0 / 255.0;

fn golden() -> Value {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../acceptance/oracle/golden");
    let text = std::fs::read_to_string(dir.join("develop.json"))
        .unwrap_or_else(|e| panic!("读不到 develop.json（先跑 freeze_golden.py）: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("develop.json 不合法: {e}"))
}

/// 小端 f32 的 hex 串 → 像素缓冲。
fn decode_hex(s: &str) -> Vec<f32> {
    let b = s.as_bytes();
    assert_eq!(b.len() % 8, 0, "hex 长度不对");
    let mut out = Vec::with_capacity(b.len() / 8);
    for chunk in b.chunks_exact(8) {
        let hex = std::str::from_utf8(chunk).expect("hex 里有非 ASCII");
        let n = u32::from_str_radix(hex, 16).expect("不是合法 hex");
        // hex 是按字节顺序写的：先出来的字节是最低位，所以要还原成小端读法。
        out.push(f32::from_bits(n.swap_bytes()));
    }
    out
}

fn images(g: &Value) -> serde_json::Map<String, Value> {
    g["images"]
        .as_object()
        .expect("develop.json 没有 images")
        .clone()
}

fn stages(case: &Value) -> Vec<String> {
    case["stage"]
        .as_array()
        .expect("case 缺 stage 字段（重跑 freeze_golden.py）")
        .iter()
        .map(|s| s.as_str().expect("阶段名不是字符串").to_string())
        .collect()
}

fn ready(case: &Value) -> bool {
    stages(case)
        .iter()
        .all(|s| STAGES_READY.contains(&s.as_str()))
}

/// 渲染一个 case：解出的原图进主链，返回与 Python 输出的最大逐点差。
///
/// 基准里的 `chan_ev`（分通道去闪）在 Python 侧是 `exposure + chan[c]`；
/// 我们把它拆成「曝光吃三通道均值 + 残差补色差」，所以这里按同样的拆法喂，
/// 对上就说明拆分无损。
fn run_case(img: &Value, case: &Value) -> (f32, usize) {
    let w = img["w"].as_u64().unwrap() as usize;
    let h = img["h"].as_u64().unwrap() as usize;
    let mut rgb = decode_hex(img["hex"].as_str().unwrap());
    let p: GradeParams = serde_json::from_value(case["params"].clone())
        .unwrap_or_else(|e| panic!("{} 的参数反序列化失败: {e}", case["name"]));
    let gain = match case["params"]["chan_ev"].as_array() {
        Some(c) => {
            assert_eq!(c.len(), 3, "{} 的 chan_ev 要三个通道", case["name"]);
            let v: Vec<f32> = c.iter().map(|x| x.as_f64().unwrap() as f32).collect();
            let mean = (v[0] + v[1] + v[2]) / 3.0;
            Ev {
                add: mean,
                chan: [v[0] - mean, v[1] - mean, v[2] - mean],
            }
        }
        None => Ev::NONE,
    };
    develop::render(&mut rgb, w, h, &p, gain, Opts::FULL);

    let want = decode_hex(case["out_hex"].as_str().unwrap());
    assert_eq!(rgb.len(), want.len(), "{} 输出尺寸不同", case["name"]);
    let mut max = 0.0f32;
    let mut at = 0usize;
    for (i, (a, b)) in rgb.iter().zip(&want).enumerate() {
        let d = (a - b).abs();
        if d > max {
            max = d;
            at = i;
        }
    }
    (max, at)
}

#[test]
fn golden_decodes_and_stats_line_up() {
    // 先证明解码与原图基准自洽，否则逐像素差到底是搬运错还是算法错都分不清。
    let g = golden();
    for (name, img) in images(&g) {
        let w = img["w"].as_u64().unwrap() as usize;
        let h = img["h"].as_u64().unwrap() as usize;
        let px = decode_hex(img["hex"].as_str().unwrap());
        assert_eq!(px.len(), w * h * 3, "{name} 长度");
        for (c, stat) in img["stats"].as_array().unwrap().iter().enumerate() {
            let vals: Vec<f64> = px.iter().skip(c).step_by(3).map(|v| *v as f64).collect();
            let n = vals.len() as f64;
            let mean = vals.iter().sum::<f64>() / n;
            let std = (vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n).sqrt();
            let s = stat.as_object().unwrap();
            assert!(
                (mean - s["mean"].as_f64().unwrap()).abs() < 1e-4,
                "{name} 通道{c} mean: {mean} vs {}",
                s["mean"].as_f64().unwrap()
            );
            assert!(
                (std - s["std"].as_f64().unwrap()).abs() < 1e-4,
                "{name} 通道{c} std: {std} vs {}",
                s["std"].as_f64().unwrap()
            );
        }
    }
}

/// 逐点类的阶段对得上 1/255 量级，空间类刚搬完时允许更松（重采样与卷积核
/// 的实现差异），所以按阶段给容差。
fn tolerance(case: &Value) -> f32 {
    let pointwise = |s: &str| matches!(s, "tone" | "vignette" | "mask");
    if stages(case).iter().all(|s| pointwise(s)) {
        1.0e-3
    } else {
        TOL
    }
}

/// 并行开发用的后门：`AEVUM_DEV_CASES=tone_only,vignette_only` 强制跑指定 case，
/// 哪怕它所属阶段还没进 `STAGES_READY`。这样两条工作线能各自验收，不用等主线。
fn forced() -> Vec<String> {
    aevum_core::env::var_str("DEV_CASES")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn develop_cases_match_python() {
    let g = golden();
    let imgs = images(&g);
    let force = forced();
    let mut ran = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut covered: Vec<String> = Vec::new();

    for case in g["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if case.get("grain").and_then(Value::as_bool).unwrap_or(false) {
            continue; // 颗粒只比统计量，见 grain_stats_matches_python
        }
        let forced = force.iter().any(|f| f == name);
        if !forced && !ready(case) {
            skipped.push(format!("{name}:{}", stages(case).join("+")));
            continue;
        }
        let img = &imgs[case["image"].as_str().unwrap()];
        let (max, at) = run_case(img, case);
        for s in stages(case) {
            if !covered.contains(&s) {
                covered.push(s);
            }
        }
        ran += 1;
        let tol = tolerance(case);
        assert!(
            max <= tol,
            "{name} 与 Python 最大差 {max:.5}（第 {} 个像素，通道 {}），容差 {tol:.5}",
            at / 3,
            at % 3
        );
        println!("{name} 最大差 {max:.5}（容差 {tol:.5}）");
    }

    assert!(ran > 0, "一个 case 都没跑，STAGES_READY 是不是写错了？");
    for stage in STAGES_READY {
        // 颗粒 case 在这里被跳过（随机数对不上），它的覆盖由
        // grain_stats_matches_python 负责，不能也算"没覆盖"。
        if stage == &"grain" {
            continue;
        }
        assert!(
            covered.iter().any(|c| c == stage),
            "声称 {stage} 阶段已就绪，但没有任何 case 覆盖它"
        );
    }
    if !skipped.is_empty() {
        println!("未就绪、跳过的 case：\n  {}", skipped.join("\n  "));
    }
}

/// 颗粒对不了随机数，只能对统计量：幅度、均值与越界比例。
/// `STAGES_READY` 里没有 `grain` 时跳过。所有标 `"grain": true` 的 case
/// （含 grain_lum 调制变体）都走同一套统计口径。
#[test]
fn grain_stats_matches_python() {
    if !STAGES_READY.contains(&"grain") {
        let any_forced = g_cases_with_grain()
            .iter()
            .any(|name| forced().iter().any(|f| f == name));
        if !any_forced {
            println!("grain 未就绪，跳过统计量比对");
            return;
        }
    }
    let g = golden();
    let imgs = images(&g);
    let mut checked = 0usize;
    for case in g["cases"].as_array().unwrap() {
        if !case.get("grain").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let name = case["name"].as_str().unwrap().to_string();
        let w = imgs[case["image"].as_str().unwrap()]["w"].as_u64().unwrap() as usize;
        let h = imgs[case["image"].as_str().unwrap()]["h"].as_u64().unwrap() as usize;
        let src = decode_hex(
            imgs[case["image"].as_str().unwrap()]["hex"]
                .as_str()
                .unwrap(),
        );
        let mut rgb = src.clone();
        let p: GradeParams = serde_json::from_value(case["params"].clone()).unwrap();
        develop::render(&mut rgb, w, h, &p, Ev::NONE, Opts::FULL);

        let scale = case["diff_scale"].as_f64().unwrap() as f32;
        let n = (w * h) as f32;
        for c in 0..3 {
            let mut sum = 0.0f32;
            let mut sum2 = 0.0f32;
            for i in (c..rgb.len()).step_by(3) {
                let d = rgb[i] - src[i];
                sum += d;
                sum2 += d * d;
            }
            let mean = sum / n;
            let var = (sum2 / n - mean * mean).max(0.0);
            let std = var.sqrt();
            let want = case["diff_std"][c].as_f64().unwrap() as f32;
            assert!(
                (std - want).abs() <= want * 0.25,
                "{name} 通道{c} 颗粒 std {std:.5}，Python {want:.5}（±25%）"
            );
            let want_mean = case["diff_mean"][c].as_f64().unwrap() as f32;
            assert!(
                (mean - want_mean).abs() <= 0.03 * scale,
                "{name} 通道{c} 颗粒均值 {mean:.5} 偏移过大（scale {scale:.5}）"
            );
            assert!(
                rgb.iter()
                    .skip(c)
                    .step_by(3)
                    .all(|v| (0.0..=1.0).contains(v)),
                "{name} 通道{c} 颗粒后越界"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "基准里没有任何 grain 统计 case");
}

/// 基准里所有 grain 统计 case 的名字（供 forced() 判断用）。
fn g_cases_with_grain() -> Vec<String> {
    let g = golden();
    g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c.get("grain").and_then(Value::as_bool).unwrap_or(false))
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}
