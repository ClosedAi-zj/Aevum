//! .cube 导出与 Python oracle 的比对。
//!
//! 基准由 `oracle/freeze_golden.py --cube-only` 生成：17³ 格点过一遍
//! `grade.apply(..., for_lut=True)`，存下输出（小端 float32 的 hex）以及
//! 其中一个 case 用 `grade.write_cube` 的格式写出的完整文本。
//!
//! 两条线分开验：**文本格式**用 Python 自己的数值喂给我们的格式化函数，
//! 逐字节必须一致；**数值**按 `develop::STAGES_READY` 逐格点比。这样对不上时
//! 一眼能看出是搬运格式错了还是算法错了。

use std::path::PathBuf;

use lumen_core::cube::{self, CubeLut};
use lumen_core::develop::STAGES_READY;
use lumen_core::grade::GradeParams;
use serde_json::Value;

const TOL: f32 = 2.0 / 255.0;

fn golden() -> Value {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../oracle/golden");
    let text = std::fs::read_to_string(dir.join("cube.json"))
        .unwrap_or_else(|e| panic!("读不到 cube.json（先跑 freeze_golden.py --cube-only）: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("cube.json 不合法: {e}"))
}

/// 小端 f32 的 hex 串 → RGB 交织缓冲。
fn decode_hex(s: &str) -> Vec<f32> {
    let b = s.as_bytes();
    assert_eq!(b.len() % 8, 0, "hex 长度不对");
    let mut out = Vec::with_capacity(b.len() / 8);
    for chunk in b.chunks_exact(8) {
        let hex = std::str::from_utf8(chunk).expect("hex 里有非 ASCII");
        let n = u32::from_str_radix(hex, 16).expect("不是合法 hex");
        out.push(f32::from_bits(n.swap_bytes()));
    }
    out
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

/// 与 develop_golden 同一个后门：`LUMEN_DEV_CASES=tone,clip` 强制跑未就绪的 case。
fn forced() -> Vec<String> {
    std::env::var("LUMEN_DEV_CASES")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn tolerance(case: &Value) -> f32 {
    let pointwise = |s: &str| matches!(s, "tone" | "vignette" | "mask");
    if stages(case).iter().all(|s| pointwise(s)) {
        1.0e-3
    } else {
        TOL
    }
}

#[test]
fn lattice_layout_is_documented_and_decodes() {
    // 先确认基准本身说的是"红最快"，否则后面所有比对都可能对着一张转置的表。
    // 只有 identity case 的输出等于输入，拿它验格点顺序。
    let g = golden();
    assert!(
        g["layout"].as_str().unwrap().contains("红分量变化最快"),
        "基准的格点顺序说明变了，Rust 侧要跟着核对"
    );
    let size = g["size"].as_u64().unwrap() as usize;
    let identity = g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"].as_str() == Some("identity"))
        .expect("基准没有 identity case");
    let want = decode_hex(identity["out_hex"].as_str().unwrap());
    assert_eq!(want.len(), size * size * size * 3);
    assert_eq!(&want[0..3], &[0.0, 0.0, 0.0], "起点应是纯黑");
    assert_eq!(&want[3..6], &[1.0 / 16.0, 0.0, 0.0], "第二格只动红");
    assert_eq!(
        &want[17 * 3..17 * 3 + 3],
        &[0.0, 1.0 / 16.0, 0.0],
        "第 17 格才动绿"
    );
    assert_eq!(
        &want[17 * 17 * 3..17 * 17 * 3 + 3],
        &[0.0, 0.0, 1.0 / 16.0],
        "第 17² 格才动蓝"
    );
    assert_eq!(&want[want.len() - 3..], &[1.0, 1.0, 1.0], "终点应是纯白");

    // 我方生成的输入格点必须与基准的完全一样。
    let mine = cube::lattice(size);
    assert_eq!(mine, want, "格点定义域生成不一致");
}

#[test]
fn text_format_matches_python_byte_for_byte() {
    // 用 Python 自己的数值走我们的格式化函数：文本必须逐字节相同，
    // 这样六位小数、行序、头部、结尾换行都不会悄悄跑偏。
    let g = golden();
    let name = g["cube_text_case"].as_str().unwrap();
    let case = g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"].as_str() == Some(name))
        .expect("基准没给出文本对应的 case");
    let size = case["size"].as_u64().unwrap() as usize;
    let flat = decode_hex(case["out_hex"].as_str().unwrap());
    let data: Vec<[f32; 3]> = flat.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    let mine = cube::text("grade", size, &data);
    let want = g["cube_text"].as_str().unwrap();
    assert_eq!(mine.len(), want.len(), "文本长度不同");
    if mine != want {
        let off = mine
            .bytes()
            .zip(want.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(0);
        let line = want[..off].lines().count();
        panic!(
            "第 {line} 行起与 Python 写出的 .cube 不同\n我方：{}\n基准：{}",
            &mine[off..off + 80],
            &want[off..off + 80]
        );
    }
}

#[test]
fn cube_cases_match_python() {
    let g = golden();
    let force = forced();
    let mut ran = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut worst = 0.0f32;

    for case in g["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let forced = force.iter().any(|f| f == name);
        if !forced && !ready(case) {
            skipped.push(format!("{name}:{}", stages(case).join("+")));
            continue;
        }
        let size = case["size"].as_u64().unwrap() as usize;
        let p: GradeParams = serde_json::from_value(case["params"].clone())
            .unwrap_or_else(|e| panic!("{name} 的参数反序列化失败: {e}"));
        let mine = cube::build(&p, size);
        let want = decode_hex(case["out_hex"].as_str().unwrap());
        assert_eq!(mine.len() * 3, want.len(), "{name} 格点数不同");

        let mut max = 0.0f32;
        let mut at = 0usize;
        for (i, (a, b)) in mine.iter().flat_map(|v| v.iter()).zip(&want).enumerate() {
            let d = (a - b).abs();
            if d > max {
                max = d;
                at = i;
            }
        }
        ran += 1;
        worst = worst.max(max);
        let tol = tolerance(case);
        assert!(
            max <= tol,
            "{name} 格点最大差 {max:.5}（第 {} 格，通道 {}），容差 {tol:.5}",
            at / 3,
            at % 3
        );
        println!("{name} 格点最大差 {max:.5}（容差 {tol:.5}）");
    }

    assert!(
        ran > 0,
        "一个 .cube case 都没跑，STAGES_READY 是不是写错了？"
    );
    if !skipped.is_empty() {
        println!("未就绪、跳过的 case：\n  {}", skipped.join("\n  "));
    }
    println!(".cube 已比对 {ran} 个 case，最大差 {worst:.5}");
}

#[test]
fn exported_file_reads_back_as_the_same_lut() {
    // 端到端：写盘 → 读回 → 与内存里的表一致。用 17³ 免得测试等太久。
    let mut p = GradeParams::default();
    p.exposure = 0.35;
    p.contrast = 20.0;
    let dir = std::env::temp_dir().join(format!("lumen-cube-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let path = dir.join("grade.cube");
    cube::export(&path, &p, 17).expect("写 .cube 失败");
    let back = cube::read(&path).expect("读 .cube 失败");
    let want = CubeLut::from_params(&p, 17);
    assert_eq!(back.size, 17);
    assert_eq!(back.data.len(), want.data.len());
    for (a, b) in back.data.iter().zip(&want.data) {
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() <= 6e-7, "通道 {c}: {a:?} vs {b:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
