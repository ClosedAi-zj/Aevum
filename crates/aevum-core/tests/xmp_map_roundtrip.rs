//! crs 映射表回归：已知键的解析、未识别键的收留与往返、包元数据不混入。
//!
//! M2 的承诺：表更新即增删映射、逻辑零改动；表没覆盖的 `crs:` 键不再静默丢弃，
//! 收进 `GradeParams::xmp_extra` 随参数走历史与落盘，导出原样回写，roundtrip 不丢键。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::grade::GradeParams;
use aevum_core::xmp;

fn temp_dir(tag: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("时钟正常")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aevum_xmp_map_{tag}_{}_{stamp}",
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

/// 带已知键、未识别键与跨命名空间键的样例。属性形式与子元素形式都有，
/// 曲线是子元素特例。
const RICH: &str = r#"<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    crs:Version="15.4"
    crs:ProcessVersion="15.4"
    crs:WhiteBalance="Custom"
    crs:Temperature="6400"
    crs:Tint="12"
    crs:IncrementalTemperature="18"
    crs:IncrementalTint="-6"
    crs:Exposure2012="0.85"
    crs:Contrast2012="12"
    crs:PostCropVignetteAmount="-30"
    crs:ConvertToGrayscale="False"
    crs:GrayMixerRed="+22"
    crs:ParametricShadowSplit="30"
    crs:SplitToningShadowHue="200"
    crs:ColorGradeShadowHue="220"
    crs:SplitToningShadowSaturation="15"
    crs:ColorGradeShadowSat="35"
    crs:DefringeGreenAmount="10"
    crs:LuminanceSmoothing="25"
    crs:Sharpness="40"
    crs:FuturePanelSlider="23"
    crs:LensProfileManualScale="112"
    crs:CropTop="0.125"
    photoshop:Headline="跨命名空间不收"
    dc:title="跨命名空间不收">
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>0, 0</rdf:li>
     <rdf:li>128, 140</rdf:li>
     <rdf:li>255, 255</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

#[test]
fn known_keys_map_through_the_table() {
    let dir = temp_dir("known");
    let path = write_xmp(&dir, "rich.xmp", RICH);
    let p = xmp::read(&path).expect("读样例");

    assert!(
        (p.exposure - 0.85).abs() < 1e-9,
        "exposure = {}",
        p.exposure
    );
    assert!((p.contrast - 12.0).abs() < 1e-9);
    // 增量键优先于绝对开尔文：temperature 取 18 而不是 (6400−5500)/45。
    assert!(
        (p.temperature - 18.0).abs() < 1e-9,
        "temperature = {}",
        p.temperature
    );
    assert!((p.tint + 6.0).abs() < 1e-9, "tint = {}", p.tint);
    // LR 正数提角、本项目正数压暗：−30 → +30。
    assert!((p.vignette - 30.0).abs() < 1e-9);
    assert!((p.mono_h[0] - 22.0).abs() < 1e-9);
    // 分割点键存在才写。
    assert!((p.par_split1 - 30.0).abs() < 1e-9);
    // 分级色相 ColorGrade 优先、饱和 SplitToning 优先（键成对给出时）。
    assert!((p.grade_sh_h - 220.0).abs() < 1e-9);
    assert!((p.grade_sh_s - 15.0).abs() < 1e-9);
    // 去边：LR 20 色阶 = 100%，10 → 50。
    assert!((p.lens_fringe - 50.0).abs() < 1e-9);
    assert!((p.nr_luma - 25.0).abs() < 1e-9);
    assert!((p.sharp_amount - 40.0).abs() < 1e-9);
    assert_eq!(
        p.curve_rgb,
        vec![[0.0, 0.0], [128.0, 140.0], [255.0, 255.0]]
    );
}

#[test]
fn unknown_crs_keys_are_kept_and_roundtrip() {
    let dir = temp_dir("extra");
    let src = write_xmp(&dir, "in.xmp", RICH);
    let p = xmp::read(&src).expect("读样例");

    // 未识别的 crs 键原样保留。
    assert_eq!(
        p.xmp_extra.get("FuturePanelSlider").map(String::as_str),
        Some("23")
    );
    assert_eq!(
        p.xmp_extra
            .get("LensProfileManualScale")
            .map(String::as_str),
        Some("112")
    );
    assert_eq!(
        p.xmp_extra.get("CropTop").map(String::as_str),
        Some("0.125")
    );
    // 跨命名空间不收；包元数据（Version/ProcessVersion）也不收。
    assert!(!p.xmp_extra.keys().any(|k| k == "Headline" || k == "title"));
    assert!(!p.xmp_extra.contains_key("Version"));
    assert!(!p.xmp_extra.contains_key("ProcessVersion"));
    // 表内已知键不重复收。
    assert!(!p.xmp_extra.contains_key("Exposure2012"));

    // 导出 → 重导入：已知参数与 xmp_extra 都不丢。
    let out = write_xmp(&dir, "out.xmp", "");
    xmp::write(&out, &p).expect("写回");
    let p2 = xmp::read(&out).expect("重读");
    assert!((p2.exposure - p.exposure).abs() < 1e-9);
    assert!((p2.temperature - p.temperature).abs() < 1e-9);
    assert_eq!(p2.curve_rgb, p.curve_rgb);
    assert_eq!(p2.xmp_extra, p.xmp_extra, "往返后 xmp_extra 丢键");
    assert_eq!(
        p2.xmp_extra.get("FuturePanelSlider").map(String::as_str),
        Some("23")
    );
}

#[test]
fn pv2010_fallback_only_when_2012_absent() {
    let dir = temp_dir("pv2010");
    let with_2012 = write_xmp(
        &dir,
        "with.xmp",
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:Exposure2012="0.5"
    crs:Exposure="9.9">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#,
    );
    let p = xmp::read(&with_2012).expect("读");
    assert!(
        (p.exposure - 0.5).abs() < 1e-9,
        "PV2012 主键在场时兜底键不该生效"
    );

    let without_2012 = write_xmp(
        &dir,
        "without.xmp",
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:Exposure="0.75">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#,
    );
    let p = xmp::read(&without_2012).expect("读");
    assert!((p.exposure - 0.75).abs() < 1e-9, "主键缺席时兜底键要生效");
}

#[test]
fn absolute_kelvin_requires_custom_white_balance() {
    let dir = temp_dir("wb");
    let template = |wb: &str| {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:WhiteBalance="{wb}"
    crs:Temperature="6400">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#
        )
    };
    let p = xmp::read(&write_xmp(&dir, "asshot.xmp", &template("As Shot"))).expect("读");
    assert_eq!(p.temperature, 0.0, "非 Custom 白平衡不套绝对开尔文");

    let p = xmp::read(&write_xmp(&dir, "custom.xmp", &template("Custom"))).expect("读");
    let want = (6400.0 - 5500.0) / 45.0;
    assert!((p.temperature - want).abs() < 1e-9);
}

#[test]
fn default_params_write_and_read_back_clean() {
    // 默认参数写出后重读应回到默认：增量键不出（温度/色调为零）、
    // xmp_extra 为空、已知键取默认值。
    let dir = temp_dir("default");
    let p = GradeParams::default();
    let out = write_xmp(&dir, "default.xmp", "");
    xmp::write(&out, &p).expect("写");
    let text = std::fs::read_to_string(&out).expect("读原文");
    assert!(!text.contains("crs:WhiteBalance="), "零温色调不该写 WB 组");
    let p2 = xmp::read(&out).expect("重读");
    assert_eq!(p2, p, "默认参数往返应逐字段一致");
    assert!(p2.xmp_extra.is_empty());
}

#[test]
fn self_written_file_reimport_loses_nothing() {
    // 自写自读的闭环：我们自己导出的每一类键都要能原样回来，
    // 这是「表更新即增删映射」不破坏往返的护栏。
    let dir = temp_dir("selfloop");
    let mut p = GradeParams::default();
    p.exposure = -1.25;
    p.temperature = -33.0;
    p.tint = 21.0;
    p.vignette = 44.0;
    p.mono = 100.0;
    p.mono_h = [5.0, -5.0, 10.0, -10.0, 15.0, -15.0, 20.0, -20.0];
    p.hsl_h[3] = -42.0;
    p.par_highlights = 17.0;
    p.par_split2 = 55.0;
    p.grade_global_h = 300.0;
    p.grade_blending = 66.0;
    p.grain = 12.0;
    p.xmp_extra
        .insert("StudioCustomKey".to_owned(), "42".to_owned());

    let out = write_xmp(&dir, "self.xmp", "");
    xmp::write(&out, &p).expect("写");
    let p2 = xmp::read(&out).expect("重读");
    assert!((p2.exposure + 1.25).abs() < 1e-9);
    assert!((p2.temperature + 33.0).abs() < 1e-9);
    assert!((p2.tint - 21.0).abs() < 1e-9);
    assert!((p2.vignette - 44.0).abs() < 1e-9);
    assert!((p2.mono - 100.0).abs() < 1e-9);
    assert!((p2.hsl_h[3] + 42.0).abs() < 1e-9);
    assert!((p2.par_highlights - 17.0).abs() < 1e-9);
    assert!((p2.par_split2 - 55.0).abs() < 1e-9);
    assert!((p2.grade_global_h - 300.0).abs() < 1e-9);
    assert!((p2.grade_blending - 66.0).abs() < 1e-9);
    assert!((p2.grain - 12.0).abs() < 1e-9);
    assert_eq!(
        p2.xmp_extra.get("StudioCustomKey").map(String::as_str),
        Some("42")
    );
}
