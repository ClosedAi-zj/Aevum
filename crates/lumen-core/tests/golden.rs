//! 与 Python 实现冻结的基准比对。
//!
//! 基准由 `oracle/freeze_golden.py` 生成（见该脚本头部）。这里只读 JSON，
//! 不需要素材序列，因此克隆仓库后就能跑。

use std::path::PathBuf;

use lumen_core::deflicker;
use lumen_core::grade;
use serde_json::Value;

fn golden(name: &str) -> Value {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../oracle/golden");
    let text = std::fs::read_to_string(dir.join(name))
        .unwrap_or_else(|e| panic!("读不到基准 {name}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("基准 {name} 不是合法 JSON: {e}"))
}

fn close(actual: f64, expected: f64, tol: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tol,
        "{what}: 期望 {expected}，实得 {actual}，差 {} > {tol}",
        (actual - expected).abs()
    );
}

/// 递归比对 JSON 数值，允许容差；`expected` 里没有的键忽略。
fn assert_value_close(actual: &Value, expected: &Value, tol: f64, path: &str) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            close(a.as_f64().unwrap(), b.as_f64().unwrap(), tol, path)
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path} 长度不同");
            for (i, (av, bv)) in a.iter().zip(b).enumerate() {
                assert_value_close(av, bv, tol, &format!("{path}[{i}]"));
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            for (k, bv) in b {
                let av = a.get(k).unwrap_or_else(|| panic!("{path}.{k} 缺失"));
                assert_value_close(av, bv, tol, &format!("{path}.{k}"));
            }
        }
        (Value::Null, Value::Null) => {}
        _ => assert_eq!(actual, expected, "{path} 类型不同"),
    }
}

#[test]
fn curve_lut_matches_python() {
    let g = golden("grade.json");
    let pts: Vec<[f64; 2]> = g["curve_points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let a = p.as_array().unwrap();
            [a[0].as_f64().unwrap(), a[1].as_f64().unwrap()]
        })
        .collect();
    let lut = grade::pchip_lut(&pts, 256);
    let expected = g["curve_lut"].as_array().unwrap();
    assert_eq!(lut.len(), expected.len());
    for (i, (a, e)) in lut.iter().zip(expected).enumerate() {
        close(*a as f64, e.as_f64().unwrap(), 1e-4, &format!("lut[{i}]"));
    }
    // 单调性：单调插值不该产生回头的台阶。
    assert!(lut.windows(2).all(|w| w[1] >= w[0] - 1e-6));
}

#[test]
fn vignette_matches_python() {
    let g = golden("grade.json");
    let v = &g["vignette"];
    let size = v["size"].as_array().unwrap();
    let (w, h) = (
        size[0].as_u64().unwrap() as usize,
        size[1].as_u64().unwrap() as usize,
    );
    let p = &v["params"];
    let params = grade::VignetteParams {
        amount: p["vig_amount"].as_f64().unwrap(),
        midpoint: p["vig_midpoint"].as_f64().unwrap(),
        roundness: p["vig_roundness"].as_f64().unwrap(),
        feather: p["vig_feather"].as_f64().unwrap(),
        hl_contrast: 0.0,
    };
    let mask = grade::vignette_mask(w, h, &params, None).expect("应该生成蒙版");
    let samples = v["samples_8x5"].as_array().unwrap();
    for (row, ys) in samples.iter().enumerate() {
        let y = row * 8;
        let cols = ys.as_array().unwrap();
        for (col, xs) in cols.iter().enumerate() {
            let x = col * 8;
            close(
                mask[y * w + x] as f64,
                xs.as_f64().unwrap(),
                1e-5,
                &format!("vignette[{y}][{x}]"),
            );
        }
    }
}

#[test]
fn grade_defaults_match_python() {
    let g = golden("grade.json");
    let actual = serde_json::to_value(grade::GradeParams::default()).unwrap();
    assert_value_close(&actual, &g["defaults"], 1e-12, "defaults");
}

#[test]
fn grade_presets_match_python() {
    let g = golden("grade.json");
    let presets = g["presets"].as_object().unwrap();
    assert_eq!(presets.len(), grade::PRESET_NAMES.len());
    for name in grade::PRESET_NAMES {
        let expected = presets
            .get(*name)
            .unwrap_or_else(|| panic!("基准里没有预设 {name}"));
        let actual = serde_json::to_value(grade::preset(name)).unwrap();
        assert_value_close(&actual, expected, 1e-12, name);
    }
}

fn option_means(v: &Value) -> Vec<Option<f64>> {
    v.as_array().unwrap().iter().map(|x| x.as_f64()).collect()
}

fn option_rgb(v: &Value) -> Vec<Option<[f64; 3]>> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array().map(|c| {
                [
                    c[0].as_f64().unwrap(),
                    c[1].as_f64().unwrap(),
                    c[2].as_f64().unwrap(),
                ]
            })
        })
        .collect()
}

#[test]
fn deflicker_luma_matches_python() {
    let g = golden("deflicker.json");
    assert_luma_cases(&g, "input_means", "luma_cases");
}

/// 线性亮度口径：GUI 与 MCP 默认走这条（`def_means` 里的 `luma`），
/// gamma 均值只在演示序列/自检里兜底，所以两边都要有金标。
#[test]
fn deflicker_linear_luma_matches_python() {
    let g = golden("deflicker.json");
    if g["linear_luma_cases"].is_null() {
        eprintln!("基准里没有线性亮度用例，跳过");
        return;
    }
    assert_luma_cases(&g, "input_linear_luma", "linear_luma_cases");

    let means = option_means(&g["input_linear_luma"]);
    let a = deflicker::analyze_luma(&means, 15.0, 100.0, false);
    assert_eq!(a.cols, 1, "亮度单通道去闪只有一列");
    assert_eq!(
        a.offsets,
        a.exposure_per_frame(),
        "单通道时曝光项就是偏移本身"
    );
    assert!(
        a.chan_residual_per_frame()
            .iter()
            .all(|c| c == &[0.0, 0.0, 0.0]),
        "不开分通道不该有色闪残差"
    );
    // 线性域才是去闪该看的量级：sRGB 编码会把明暗跳压扁近一半。
    let linear = a.stats.jitter_before;
    let gamma = deflicker::analyze_luma(&option_means(&g["input_means"]), 15.0, 100.0, false)
        .stats
        .jitter_before;
    assert!(
        linear > gamma * 1.3,
        "线性抖动 {linear} 应明显大于 gamma 抖动 {gamma}"
    );
}

/// 同一套用例只是换了输入口径：逐帧偏移 + 抖动/极值统计。
fn assert_luma_cases(g: &Value, input_key: &str, cases_key: &str) {
    let means = option_means(&g[input_key]);
    for (k, case) in g[cases_key].as_array().unwrap().iter().enumerate() {
        let window = case["window"].as_f64().unwrap();
        let strength = case["strength"].as_f64().unwrap();
        let multi = case["multi"].as_bool().unwrap();
        let a = deflicker::analyze_luma(&means, window, strength, multi);
        let expected = case["offsets"].as_array().unwrap();
        assert_eq!(a.offsets.len(), expected.len());
        for (i, e) in expected.iter().enumerate() {
            close(
                a.offsets[i],
                e.as_f64().unwrap(),
                1e-4,
                &format!("{cases_key} case{k} offsets[{i}]"),
            );
        }
        let s = &case["stats"];
        close(
            a.stats.jitter_before,
            s["jitter_before"].as_f64().unwrap(),
            2e-4,
            "jitter_before",
        );
        close(
            a.stats.jitter_after,
            s["jitter_after"].as_f64().unwrap(),
            2e-4,
            "jitter_after",
        );
        close(
            a.stats.max_ev,
            s["max_ev"].as_f64().unwrap(),
            5e-4,
            "max_ev",
        );
        close(
            a.stats.mean_abs_ev,
            s["mean_abs_ev"].as_f64().unwrap(),
            5e-4,
            "mean_abs_ev",
        );
        assert_eq!(a.stats.frames, s["frames"].as_u64().unwrap() as usize);
        assert_eq!(a.stats.valid, s["valid"].as_u64().unwrap() as usize);
        assert_eq!(a.stats.channels, s["channels"].as_bool().unwrap());
    }
}

#[test]
fn deflicker_color_matches_python() {
    let g = golden("deflicker.json");
    if g["input_rgb"][0].is_null() {
        eprintln!("基准里没有 RGB 测光结果，跳过");
        return;
    }
    let means = option_rgb(&g["input_rgb"]);
    for (k, case) in g["color_cases"].as_array().unwrap().iter().enumerate() {
        let window = case["window"].as_f64().unwrap();
        let strength = case["strength"].as_f64().unwrap();
        let multi = case["multi"].as_bool().unwrap();
        let a = deflicker::analyze_rgb(&means, window, strength, multi);
        assert_eq!(a.cols, 3);
        let rows = case["offsets"].as_array().unwrap();
        for (i, row) in rows.iter().enumerate() {
            for (c, e) in row.as_array().unwrap().iter().enumerate() {
                close(
                    a.offset(i, c),
                    e.as_f64().unwrap(),
                    1e-4,
                    &format!("color case{k} [{i}][{c}]"),
                );
            }
        }
        let s = &case["stats"];
        close(
            a.stats.color_before,
            s["color_before"].as_f64().unwrap(),
            2e-4,
            "color_before",
        );
        close(
            a.stats.color_after,
            s["color_after"].as_f64().unwrap(),
            2e-4,
            "color_after",
        );
        close(
            a.stats.jitter_before,
            s["jitter_before"].as_f64().unwrap(),
            2e-4,
            "jitter_before",
        );
        close(
            a.stats.jitter_after,
            s["jitter_after"].as_f64().unwrap(),
            2e-4,
            "jitter_after",
        );
    }
}

/// 素材不在仓库里，设 `LUMEN_FRAMES=<帧目录>` 才跑。用来验证解码 + 缩放的
/// 测光结果与 Python 一致（缩略图算法差异按 3e-3 容忍）。
#[test]
fn luma_matches_python_when_frames_available() {
    let Ok(dir) = std::env::var("LUMEN_FRAMES") else {
        eprintln!("未设置 LUMEN_FRAMES，跳过实测比对");
        return;
    };
    let g = golden("luma.json");
    let expected = g["luma"].as_array().unwrap();
    for (i, row) in expected.iter().enumerate() {
        if row.is_null() {
            continue;
        }
        let name = g["frames"][i].as_str().unwrap();
        let path = std::path::Path::new(&dir).join(name);
        let m = lumen_core::metrics::measure_file(&path, 512).expect("读帧失败");
        let e = row.as_array().unwrap();
        close(
            m.mean,
            e[0].as_f64().unwrap(),
            3e-3,
            &format!("{name} mean"),
        );
        close(
            m.median,
            e[1].as_f64().unwrap(),
            3e-3,
            &format!("{name} median"),
        );
        close(
            m.center,
            e[2].as_f64().unwrap(),
            3e-3,
            &format!("{name} center"),
        );
        let want_sharp = e[3].as_f64().unwrap();
        assert!(
            (m.sharp - want_sharp).abs() <= want_sharp * 0.10 + 1.0,
            "{name} sharp: 期望 {want_sharp}，实得 {}",
            m.sharp
        );
    }
}

/// 去闪测光实测比对：解码 + 320 缩放 + 截尾均值整条链对 Python。
/// 同样只在设了 `LUMEN_FRAMES` 时跑。
///
/// 亮度按 1e-4、分通道按 1e-3 容忍：本机实测亮度最大差 4.2e-5，分通道 4.8e-4，
/// 全部来自 JPEG 解码器差异（Rust 缩略图与 cv2 逐像素最多差 1 LSB、约 12% 像素不同）。
/// 把 Rust 的像素喂回 Python 算法能复现 Rust 的数字，说明口径本身没差。
/// 暗部饱和帧里 1 LSB 正好落在 sRGB 反解最陡的一段，所以分通道比亮度敏感一个量级；
/// 1e-3 折算约 0.03 EV，远小于可见闪烁阈值。
#[test]
fn deflicker_measure_matches_python_when_frames_available() {
    let Ok(dir) = std::env::var("LUMEN_FRAMES") else {
        eprintln!("未设置 LUMEN_FRAMES，跳过测光比对");
        return;
    };
    let g = golden("deflicker.json");
    let names = g["frames"].as_array().unwrap();
    let want_luma = g["input_linear_luma"].as_array().unwrap();
    let want_rgb = g["input_rgb"].as_array().unwrap();
    for i in 0..names.len() {
        if want_luma[i].is_null() {
            continue;
        }
        let name = names[i].as_str().unwrap();
        let path = std::path::Path::new(&dir).join(name);
        let m = deflicker::measure_path(&path).unwrap_or_else(|| panic!("测光失败 {path:?}"));
        close(
            m.luma,
            want_luma[i].as_f64().unwrap(),
            1e-4,
            &format!("{name} 线性亮度"),
        );
        let row = want_rgb[i].as_array().unwrap();
        for c in 0..3 {
            close(
                m.rgb[c],
                row[c].as_f64().unwrap(),
                1e-3,
                &format!("{name} 通道{c}"),
            );
        }
    }
}

/// 蒙版权重场逐像素比对 Python；亮度范围用同一根 0→1 渐变。
#[test]
fn mask_fields_match_python() {
    let g = golden("mask.json");
    for case in g["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let w = case["w"].as_u64().unwrap() as usize;
        let h = case["h"].as_u64().unwrap() as usize;
        let lum = if case["lum"].as_str() == Some("ramp") {
            let n = w * h;
            Some(
                (0..n)
                    .map(|i| i as f32 / (n - 1) as f32)
                    .collect::<Vec<f32>>(),
            )
        } else {
            None
        };
        let mut mask = lumen_core::mask::parse_mask(&case["mask"]).expect("蒙版 JSON 解析失败");
        mask.build_field(w, h, lum.as_deref(), None);
        let expected = case["field"].as_array().unwrap();
        assert_eq!(mask.field.len(), expected.len(), "{name} 场长度不同");
        for (i, (actual, want)) in mask.field.iter().zip(expected).enumerate() {
            close(
                *actual as f64,
                want.as_f64().unwrap(),
                2e-5,
                &format!("{name}[{i}]"),
            );
        }
    }
}
