//! QA 独立验证（f5c40a6）：不信任实现方自带测试，按 PRD/设计口径自行构造用例。
//!
//! 覆盖四块：
//! 1. 色温轨道的数学性质（端点恒等 / 0 恒等 / 单调 / 双向可逆 / 对称 / 锚点表值）；
//! 2. crs 映射的边缘情况（绝对开尔文与增量并存、暗角符号、WbCustom 条件、
//!    未识别 crs 键收留、跨命名空间不收留、导出重导入往返）；
//! 3. `xmp_extra` 的 serde 兼容（新字段往返、旧 JSON 无字段可反序列化）；
//! 4. RAW 默认参数三条件已有 state.rs 测试覆盖（raw_import_tests），此处不重复。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lumen_core::grade::GradeParams;
use lumen_core::temp_track::TempTrack;
use lumen_core::xmp;
use lumen_core::xmp_map;

fn temp_dir(tag: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("时钟正常")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "lumen_qa_m2_{tag}_{}_{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn write_xmp(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("写 XMP");
    path
}

// ─────────────────────────────────────────────── 1. 色温轨道

#[test]
fn qa_temp_track_endpoints_and_zero_are_identity() {
    let t = TempTrack::COLOR_TEMPERATURE;
    assert_eq!(t.pos_to_val(-100.0), -100.0, "下端点必须恒等");
    assert_eq!(t.pos_to_val(100.0), 100.0, "上端点必须恒等");
    assert_eq!(t.pos_to_val(0.0), 0.0, "中点必须恒等");
    assert_eq!(t.val_to_pos(-100.0), -100.0);
    assert_eq!(t.val_to_pos(100.0), 100.0);
    assert_eq!(t.val_to_pos(0.0), 0.0);
}

#[test]
fn qa_temp_track_monotonic_dense_sampling() {
    let t = TempTrack::COLOR_TEMPERATURE;
    let n = 10_001;
    let mut prev = f64::NEG_INFINITY;
    for k in 0..n {
        let p = -100.0 + 200.0 * k as f64 / (n - 1) as f64;
        let v = t.pos_to_val(p);
        assert!(v >= prev, "pos={p} 处值倒挂：{v} < {prev}");
        prev = v;
    }
}

#[test]
fn qa_temp_track_roundtrip_within_1e9() {
    let t = TempTrack::COLOR_TEMPERATURE;
    let n = 20_001;
    for k in 0..n {
        let p = -100.0 + 200.0 * k as f64 / (n - 1) as f64;
        let v = t.pos_to_val(p);
        let back = t.val_to_pos(v);
        assert!(
            (back - p).abs() < 1e-9,
            "p={p} → v={v} → p'={back} 往返漂移超 1e-9"
        );
    }
}

#[test]
fn qa_temp_track_symmetric_about_zero() {
    let t = TempTrack::COLOR_TEMPERATURE;
    let n = 4_001;
    for k in 0..n {
        let p = -100.0 + 200.0 * k as f64 / (n - 1) as f64;
        let vp = t.pos_to_val(p);
        let vn = t.pos_to_val(-p);
        assert!(
            (vp + vn).abs() < 1e-12,
            "f(-p) != -f(p)：p={p}, f(p)={vp}, f(-p)={vn}"
        );
    }
}

#[test]
fn qa_temp_track_anchor_values_match_design_table() {
    // 设计附录 A 的锚点表（中段密两端疏），逐点硬编码比对——
    // 防止实现改表后没有同步设计文档。
    let expect: [(f64, f64); 13] = [
        (-100.0, -100.0),
        (-70.0, -85.0),
        (-45.0, -68.0),
        (-25.0, -48.0),
        (-12.0, -30.0),
        (-6.0, -16.0),
        (0.0, 0.0),
        (6.0, 16.0),
        (12.0, 30.0),
        (25.0, 48.0),
        (45.0, 68.0),
        (70.0, 85.0),
        (100.0, 100.0),
    ];
    for (p, v) in expect {
        assert_eq!(TempTrack::COLOR_TEMPERATURE.pos_to_val(p), v, "锚点 {p}");
    }
    // 段内插值抽查：p=3 落在 [0,0]..[6,16] 段正中 → 8。
    assert!((TempTrack::COLOR_TEMPERATURE.pos_to_val(3.0) - 8.0).abs() < 1e-12);
    // 越界夹取：超出 ±100 的输入夹到端点，不外插。
    assert_eq!(TempTrack::COLOR_TEMPERATURE.pos_to_val(150.0), 100.0);
    assert_eq!(TempTrack::COLOR_TEMPERATURE.pos_to_val(-1e4), -100.0);
}

// ─────────────────────────────────────────────── 2. crs 映射边缘情况

/// 绝对开尔文与增量色温同时存在的 XMP 模板（WB 名可替换）。
fn wb_conflict_xmp(wb: &str) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:WhiteBalance="{wb}"
    crs:Temperature="6400"
    crs:Tint="15"
    crs:IncrementalTemperature="18"
    crs:IncrementalTint="-9">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
    )
}

#[test]
fn qa_incremental_temperature_wins_over_absolute_kelvin() {
    let dir = temp_dir("wbconflict");
    // Custom：绝对开尔文先算 (6400−5500)/45 ≈ 20，增量 18 后写覆盖 → 18。
    let p = xmp::read(&write_xmp(&dir, "custom.xmp", &wb_conflict_xmp("Custom"))).expect("读");
    assert!(
        (p.temperature - 18.0).abs() < 1e-9,
        "增量键必须覆盖绝对开尔文，got {}",
        p.temperature
    );
    assert!((p.tint - (-9.0)).abs() < 1e-9, "增量色调覆盖 15/1.5=10");
    // 非 Custom：绝对开尔文不生效，但增量键无条件生效 → 仍是 18。
    let p = xmp::read(&write_xmp(&dir, "asshot.xmp", &wb_conflict_xmp("As Shot"))).expect("读");
    assert!((p.temperature - 18.0).abs() < 1e-9, "增量键与 WB 名无关");
    assert!((p.tint + 9.0).abs() < 1e-9);
}

#[test]
fn qa_absolute_kelvin_only_under_custom_wb() {
    let dir = temp_dir("wbonly");
    let template = |wb: &str, tint: &str| {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:WhiteBalance="{wb}"
    crs:Temperature="6400"
    crs:Tint="{tint}">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
        )
    };
    // Custom：K=6400 → (6400−5500)/45；Tint=15 → 15/1.5=10。
    let p = xmp::read(&write_xmp(&dir, "c.xmp", &template("Custom", "15"))).expect("读");
    assert!((p.temperature - (6400.0 - 5500.0) / 45.0).abs() < 1e-9);
    assert!((p.tint - 10.0).abs() < 1e-9);
    // 非 Custom：两者都不生效。
    for wb in ["As Shot", "Daylight", "Auto", "Shade"] {
        let p = xmp::read(&write_xmp(&dir, "a.xmp", &template(wb, "15"))).expect("读");
        assert_eq!(p.temperature, 0.0, "WB={wb} 时绝对开尔文不得生效");
        assert_eq!(p.tint, 0.0, "WB={wb} 时绝对色调不得生效");
    }
}

/// 带暗角、未识别 crs 键与跨命名空间键的全家桶样例。
const EDGE: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    crs:Version="15.4"
    crs:ProcessVersion="15.4"
    crs:WhiteBalance="Custom"
    crs:PostCropVignetteAmount="-30"
    crs:PostCropVignetteMidpoint="55"
    crs:FuturePanelSlider="23"
    crs:LensProfileManualScale="112"
    crs:CropTop="0.125"
    crs:SomeLegacyKey="hello world"
    exif:ExposureTime="1/125"
    xmp:Rating="4">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

#[test]
fn qa_vignette_sign_flip_and_roundtrip() {
    let dir = temp_dir("vig");
    let src = write_xmp(&dir, "in.xmp", EDGE);
    let p = xmp::read(&src).expect("读");
    // LR 的 -30（提亮四角）→ 本项目 +30（压暗）。
    assert!((p.vignette - 30.0).abs() < 1e-9, "vignette = {}", p.vignette);
    assert!((p.vig_midpoint - 55.0).abs() < 1e-9);

    let out = write_xmp(&dir, "out.xmp", "");
    xmp::write(&out, &p).expect("写回");
    let text = std::fs::read_to_string(&out).expect("读导出原文");
    // 导出要翻回 LR 的符号：+30 → "-30"。
    assert!(
        text.contains("crs:PostCropVignetteAmount=\"-30\""),
        "导出暗角符号没翻回去：{}",
        text
    );
    let p2 = xmp::read(&out).expect("重读");
    assert!((p2.vignette - 30.0).abs() < 1e-9);
    assert!((p2.vig_midpoint - 55.0).abs() < 1e-9);
}

#[test]
fn qa_unknown_crs_kept_cross_ns_not_and_full_roundtrip() {
    let dir = temp_dir("extra");
    let src = write_xmp(&dir, "in.xmp", EDGE);
    let p = xmp::read(&src).expect("读");

    // 未识别 crs 键收留（含带空格的字符串值）。
    assert_eq!(p.xmp_extra.get("FuturePanelSlider").map(String::as_str), Some("23"));
    assert_eq!(p.xmp_extra.get("LensProfileManualScale").map(String::as_str), Some("112"));
    assert_eq!(p.xmp_extra.get("CropTop").map(String::as_str), Some("0.125"));
    assert_eq!(p.xmp_extra.get("SomeLegacyKey").map(String::as_str), Some("hello world"));
    // 跨命名空间（exif: / xmp:）绝不收留；包元数据不收留。
    assert!(!p.xmp_extra.contains_key("ExposureTime"));
    assert!(!p.xmp_extra.contains_key("Rating"));
    assert!(!p.xmp_extra.contains_key("Version"));
    assert!(!p.xmp_extra.contains_key("ProcessVersion"));
    // 表内已知键不重复收留。
    assert!(!p.xmp_extra.contains_key("PostCropVignetteAmount"));
    assert!(!p.xmp_extra.contains_key("WhiteBalance"));

    // 导出 → 重导入：参数与 xmp_extra 全量往返一致。
    let out = write_xmp(&dir, "out.xmp", "");
    xmp::write(&out, &p).expect("写回");
    let p2 = xmp::read(&out).expect("重读");
    assert_eq!(p2, p, "边缘样例导出重导入后应逐字段一致");
    assert_eq!(p2.xmp_extra, p.xmp_extra);
}

#[test]
fn qa_wb_custom_keys_do_not_leak_into_extra() {
    // WbCustom 条件键（Temperature/Tint）与条件输入键 WhiteBalance
    // 都是表内键，任何条件下都不该落进 xmp_extra。
    let dir = temp_dir("wbleak");
    let p = xmp::read(&write_xmp(&dir, "a.xmp", &wb_conflict_xmp("As Shot"))).expect("读");
    assert!(!p.xmp_extra.contains_key("Temperature"));
    assert!(!p.xmp_extra.contains_key("Tint"));
    assert!(!p.xmp_extra.contains_key("WhiteBalance"));
    assert!(!p.xmp_extra.contains_key("IncrementalTemperature"));
}

// ─────────────────────────────────────────────── 3. xmp_extra serde

#[test]
fn qa_xmp_extra_serde_roundtrip_key_by_key() {
    let mut p = GradeParams::default();
    p.exposure = 0.4;
    p.temperature = -12.0;
    p.xmp_extra.insert("FuturePanelSlider".to_owned(), "23".to_owned());
    p.xmp_extra.insert("CropTop".to_owned(), "0.125".to_owned());
    p.xmp_extra.insert("Note".to_owned(), "带 空格 & 符号".to_owned());

    let json = serde_json::to_string(&p).expect("序列化");
    let back: GradeParams = serde_json::from_str(&json).expect("反序列化");
    assert_eq!(back, p, "serde 往返应逐字段一致");
    for (k, v) in &p.xmp_extra {
        assert_eq!(back.xmp_extra.get(k).map(String::as_str), Some(v.as_str()), "键 {k}");
    }
}

#[test]
fn qa_xmp_extra_missing_in_old_json_defaults_to_empty() {
    // 旧工程 JSON 没有 xmp_extra 字段：serde(default) 兜底，不得报错。
    let old = r#"{"exposure":0.5,"contrast":10,"temperature":18,"tint":-6}"#;
    let p: GradeParams = serde_json::from_str(old).expect("旧 JSON 必须能反序列化");
    assert!((p.exposure - 0.5).abs() < 1e-9);
    assert!((p.temperature - 18.0).abs() < 1e-9);
    assert!(p.xmp_extra.is_empty(), "缺字段应反序列化为空表");
}

// ─────────────────────────────────────────────── 4. 映射表自检

#[test]
fn qa_spec_table_scales_match_v030_semantics() {
    // 与 f5c40a6^ 的 if-let 链逐条对照的关键刻度：
    // 暗角 -1、绝对开尔文 1/45 配 -5500/45、色调 1/1.5、去边 5（20 色阶=100%）。
    let find = |key: &str| xmp_map::SPECS.iter().find(|s| s.key == key).expect("表内键");
    let vig = find("PostCropVignetteAmount");
    assert_eq!(vig.scale, -1.0, "暗角方向相反");
    assert_eq!((vig.min, vig.max), (-100.0, 100.0));
    let kelvin = find("Temperature");
    assert_eq!(kelvin.scale, 1.0 / 45.0);
    assert!((kelvin.offset - -5500.0 / 45.0).abs() < 1e-12);
    let tint = find("Tint");
    assert_eq!(tint.scale, 1.0 / 1.5);
    let fringe = find("DefringePurpleAmount");
    assert_eq!(fringe.scale, 5.0, "LR 20 色阶 = 100%");
    // 导出顺序锚点：表首是 Exposure2012，表尾是 WhiteBalance（条件输入）。
    assert_eq!(xmp_map::SPECS.first().unwrap().key, "Exposure2012");
    assert_eq!(xmp_map::SPECS.last().unwrap().key, "WhiteBalance");
    assert_eq!(find("GrainAmount").key, "GrainAmount");
}
