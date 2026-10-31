//! XMP 预设读写：Python 参考实现的键映射、往返精度、命名空间/实体兼容。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::grade::GradeParams;
use aevum_core::project::Metadata;
use aevum_core::xmp::{self, XmpError};

fn temp_dir(tag: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("时钟正常")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aevum_xmp_test_{tag}_{}_{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn close(actual: f64, expected: f64, name: &str) {
    assert!(
        (actual - expected).abs() <= 1e-6,
        "{name}: 实际 {actual}，期望 {expected}"
    );
}

fn close_arr(actual: &[f64; 8], expected: &[f64; 8], name: &str) {
    for (i, (a, b)) in actual.iter().zip(expected.iter()).enumerate() {
        assert!((a - b).abs() <= 1e-6, "{name}[{i}]: 实际 {a}，期望 {b}");
    }
}

fn curve(pts: &[[i64; 2]]) -> Vec<[f64; 2]> {
    pts.iter().map(|[x, y]| [*x as f64, *y as f64]).collect()
}

fn assert_curve(actual: &[[f64; 2]], expected: &[[i64; 2]], name: &str) {
    let want = curve(expected);
    assert_eq!(actual, want.as_slice(), "{name} 不一致：{actual:?}");
}

fn golden_sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("acceptance")
        .join("oracle")
        .join("golden")
        .join("xmp_sample.xmp")
}

/// 一组接近 Python `tests/test_xmp.py::rich_params` 的非默认值。
fn rich_params() -> GradeParams {
    let mut p = GradeParams::default();
    p.exposure = 0.85;
    p.contrast = 12.0;
    p.highlights = -30.0;
    p.shadows = 25.0;
    p.whites = 8.0;
    p.blacks = -14.0;
    p.texture = 15.0;
    p.clarity = 10.0;
    p.dehaze = 7.0;
    p.vibrance = 18.0;
    p.saturation = -6.0;
    p.temperature = 22.0;
    p.tint = -11.0;

    p.vignette = -40.0;
    p.vig_midpoint = 60.0;
    p.vig_feather = 70.0;
    p.vig_roundness = 30.0;
    p.vig_hl_contrast = 45.0;

    p.grain = 22.0;
    p.sharp_amount = 55.0;
    p.sharp_radius = 1.1;
    p.sharp_detail = 30.0;
    p.sharp_masking = 12.0;
    p.nr_luma = 18.0;
    p.nr_luma_detail = 40.0;
    p.nr_luma_contrast = 5.0;
    p.nr_color = 12.0;
    p.nr_color_smooth = 35.0;

    p.par_shadows = -12.0;
    p.par_darks = 8.0;
    p.par_lights = 6.0;
    p.par_highlights = -4.0;
    p.par_split1 = 30.0;
    p.par_split2 = 55.0;
    p.par_split3 = 80.0;

    p.grade_sh_h = 220.0;
    p.grade_sh_s = 30.0;
    p.grade_sh_l = 5.0;
    p.grade_mid_h = 120.0;
    p.grade_mid_s = 10.0;
    p.grade_mid_l = -3.0;
    p.grade_hi_h = 40.0;
    p.grade_hi_s = 20.0;
    p.grade_hi_l = 2.0;
    p.grade_global_h = 300.0;
    p.grade_global_s = 7.0;
    p.grade_global_l = -2.0;
    p.grade_balance = -15.0;
    p.grade_blending = 60.0;

    p.lens_fringe = 35.0;

    p.mono = 100.0;
    p.mono_h = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
    p.hsl_h = [5.0, -3.0, 2.0, 4.0, -6.0, 7.0, 8.0, -9.0];
    p.hsl_s = [-2.0, 0.0, 3.0, 4.0, 5.0, 6.0, -7.0, -8.0];
    p.hsl_l = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];

    p.curve_rgb = vec![
        [0.0, 0.0],
        [32.0, 20.0],
        [128.0, 140.0],
        [224.0, 240.0],
        [255.0, 255.0],
    ];
    p.curve_r = vec![[0.0, 10.0], [128.0, 128.0], [255.0, 245.0]];
    p.curve_g = vec![[0.0, 0.0], [64.0, 80.0], [192.0, 210.0], [255.0, 255.0]];
    p.curve_b = vec![[0.0, 5.0], [64.0, 70.0], [192.0, 200.0], [255.0, 250.0]];
    p
}

#[test]
fn roundtrip_rich_params() {
    let dir = temp_dir("rich");
    let path = dir.join("丰富预设.xmp");

    let p = rich_params();
    xmp::write(&path, &p).expect("写 XMP");
    let q = xmp::read(&path).expect("读回 XMP");

    close(q.exposure, 0.85, "exposure");
    close(q.contrast, 12.0, "contrast");
    close(q.highlights, -30.0, "highlights");
    close(q.shadows, 25.0, "shadows");
    close(q.whites, 8.0, "whites");
    close(q.blacks, -14.0, "blacks");
    close(q.texture, 15.0, "texture");
    close(q.clarity, 10.0, "clarity");
    close(q.dehaze, 7.0, "dehaze");
    close(q.vibrance, 18.0, "vibrance");
    close(q.saturation, -6.0, "saturation");

    close(q.temperature, 22.0, "temperature");
    close(q.tint, -11.0, "tint");

    close(q.vignette, -40.0, "vignette");
    close(q.vig_midpoint, 60.0, "vig_midpoint");
    close(q.vig_feather, 70.0, "vig_feather");
    close(q.vig_roundness, 30.0, "vig_roundness");
    close(q.vig_hl_contrast, 45.0, "vig_hl_contrast");

    close(q.grain, 22.0, "grain");
    close(q.sharp_amount, 55.0, "sharp_amount");
    close(q.sharp_radius, 1.1, "sharp_radius");
    close(q.sharp_detail, 30.0, "sharp_detail");
    close(q.sharp_masking, 12.0, "sharp_masking");
    close(q.nr_luma, 18.0, "nr_luma");
    close(q.nr_luma_detail, 40.0, "nr_luma_detail");
    close(q.nr_luma_contrast, 5.0, "nr_luma_contrast");
    close(q.nr_color, 12.0, "nr_color");
    close(q.nr_color_smooth, 35.0, "nr_color_smooth");

    close(q.par_shadows, -12.0, "par_shadows");
    close(q.par_darks, 8.0, "par_darks");
    close(q.par_lights, 6.0, "par_lights");
    close(q.par_highlights, -4.0, "par_highlights");
    close(q.par_split1, 30.0, "par_split1");
    close(q.par_split2, 55.0, "par_split2");
    close(q.par_split3, 80.0, "par_split3");

    close(q.grade_sh_h, 220.0, "grade_sh_h");
    close(q.grade_sh_s, 30.0, "grade_sh_s");
    close(q.grade_sh_l, 5.0, "grade_sh_l");
    close(q.grade_mid_h, 120.0, "grade_mid_h");
    close(q.grade_mid_s, 10.0, "grade_mid_s");
    close(q.grade_mid_l, -3.0, "grade_mid_l");
    close(q.grade_hi_h, 40.0, "grade_hi_h");
    close(q.grade_hi_s, 20.0, "grade_hi_s");
    close(q.grade_hi_l, 2.0, "grade_hi_l");
    close(q.grade_global_h, 300.0, "grade_global_h");
    close(q.grade_global_s, 7.0, "grade_global_s");
    close(q.grade_global_l, -2.0, "grade_global_l");
    close(q.grade_balance, -15.0, "grade_balance");
    close(q.grade_blending, 60.0, "grade_blending");

    close(q.lens_fringe, 35.0, "lens_fringe");
    close(q.mono, 100.0, "mono");
    close_arr(&q.mono_h, &p.mono_h, "mono_h");
    close_arr(&q.hsl_h, &p.hsl_h, "hsl_h");
    close_arr(&q.hsl_s, &p.hsl_s, "hsl_s");
    close_arr(&q.hsl_l, &p.hsl_l, "hsl_l");

    assert_curve(
        &q.curve_rgb,
        &[[0, 0], [32, 20], [128, 140], [224, 240], [255, 255]],
        "curve_rgb",
    );
    assert_curve(&q.curve_r, &[[0, 10], [128, 128], [255, 245]], "curve_r");
    assert_curve(
        &q.curve_g,
        &[[0, 0], [64, 80], [192, 210], [255, 255]],
        "curve_g",
    );
    assert_curve(
        &q.curve_b,
        &[[0, 5], [64, 70], [192, 200], [255, 250]],
        "curve_b",
    );

    // 未映射项保持默认：色阶、颗粒大小/密度、蒙版。
    close(q.lv_black, 0.0, "lv_black");
    close(q.lv_gamma, 1.0, "lv_gamma");
    close(q.grain_size, 1.0, "grain_size");
    close(q.grain_freq, 1.0, "grain_freq");
    assert!(q.masks.is_empty(), "蒙版不应被写入/读回");

    // 生成的文件必须是带 xpacket 的合法 XMP。
    let text = std::fs::read_to_string(&path).expect("读文件文本");
    assert!(text.starts_with("<?xpacket begin="), "缺少 xpacket begin");
    assert!(text.contains("<?xpacket end=\"w\"?>"), "缺少 xpacket end");
    assert!(text.contains("xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\""));
    assert!(text.contains("<crs:ToneCurvePV2012>"));
    assert!(text.contains("<rdf:Seq>"));
    assert!(text.contains("crs:Exposure2012=\"0.85\""));
    assert_eq!(
        text.matches("crs:GrainAmount=").count(),
        1,
        "GrainAmount 只能出现一次"
    );
}

/// 越界值按 Python 的口径裁剪：写端裁 0..100 / ±100，读端再按各自范围裁。
#[test]
fn write_clamps_out_of_range_values() {
    let dir = temp_dir("clamp");
    let path = dir.join("clamp.xmp");

    let mut p = GradeParams::default();
    p.grain = -5.0;
    p.vignette = 150.0;
    p.vig_hl_contrast = -20.0;
    p.temperature = 180.0;
    p.tint = -130.0;
    p.hsl_h[0] = 140.0;
    p.par_split1 = 140.0;

    xmp::write(&path, &p).expect("写 XMP");
    let q = xmp::read(&path).expect("读回 XMP");

    close(q.grain, 0.0, "grain 裁剪");
    close(q.vignette, 100.0, "vignette 裁剪");
    close(q.vig_hl_contrast, 0.0, "负的高光保护按 0 写");
    close(q.temperature, 100.0, "temperature 裁剪");
    close(q.tint, -100.0, "tint 裁剪");
    close(q.hsl_h[0], 100.0, "hsl 裁剪");
    close(q.par_split1, 98.0, "分割点读入裁剪");
}

/// 曲线坐标取整用 Python 的银行家舍入：.5 向偶数靠。
#[test]
fn curve_rounding_matches_python_bankers_rule() {
    let dir = temp_dir("rounding");
    let path = dir.join("rounding.xmp");

    let mut p = GradeParams::default();
    p.sharp_radius = 2.5;
    p.curve_rgb = vec![[0.5, 0.5], [2.5, 2.5], [3.5, 3.5], [255.0, 255.0]];

    xmp::write(&path, &p).expect("写 XMP");
    let q = xmp::read(&path).expect("读回 XMP");

    close(q.sharp_radius, 2.5, "非整数半径保留小数");
    assert_curve(
        &q.curve_rgb,
        &[[0, 0], [2, 2], [4, 4], [255, 255]],
        "curve_rgb 银行家舍入",
    );
}

/// 读 Python 参考实现生成的样例，关键值与 Python 端读出的结果逐项比对。
#[test]
fn reads_python_generated_sample() {
    let path = golden_sample();
    assert!(path.exists(), "样例不存在：{}", path.display());
    let p = xmp::read(&path).expect("读 Python 样例");

    close(p.exposure, 0.85, "exposure");
    close(p.contrast, 12.0, "contrast");
    close(p.highlights, -30.0, "highlights");
    close(p.shadows, 25.0, "shadows");
    close(p.whites, 8.0, "whites");
    close(p.blacks, -14.0, "blacks");
    close(p.texture, 15.0, "texture");
    close(p.clarity, 10.0, "clarity");
    close(p.dehaze, 7.0, "dehaze");
    close(p.vibrance, 18.0, "vibrance");
    close(p.saturation, -6.0, "saturation");

    close(p.temperature, 22.0, "temperature");
    close(p.tint, -11.0, "tint");
    close(p.vignette, -40.0, "vignette");
    close(p.vig_midpoint, 60.0, "vig_midpoint");
    close(p.vig_feather, 70.0, "vig_feather");
    close(p.vig_roundness, 30.0, "vig_roundness");
    close(p.vig_hl_contrast, 45.0, "vig_hl_contrast");

    close(p.grain, 22.0, "grain");
    close(p.sharp_amount, 55.0, "sharp_amount");
    close(p.sharp_radius, 1.1, "sharp_radius");
    close(p.sharp_detail, 30.0, "sharp_detail");
    close(p.sharp_masking, 12.0, "sharp_masking");
    close(p.nr_luma, 18.0, "nr_luma");
    close(p.nr_luma_detail, 40.0, "nr_luma_detail");
    close(p.nr_luma_contrast, 5.0, "nr_luma_contrast");
    close(p.nr_color, 12.0, "nr_color");
    close(p.nr_color_smooth, 35.0, "nr_color_smooth");
    close(p.lens_fringe, 35.0, "lens_fringe");

    close(p.par_shadows, -12.0, "par_shadows");
    close(p.par_darks, 8.0, "par_darks");
    close(p.par_lights, 6.0, "par_lights");
    close(p.par_highlights, -4.0, "par_highlights");
    close(p.par_split1, 30.0, "par_split1");
    close(p.par_split2, 55.0, "par_split2");
    close(p.par_split3, 80.0, "par_split3");

    close(p.grade_sh_h, 220.0, "grade_sh_h");
    close(p.grade_sh_s, 30.0, "grade_sh_s");
    close(p.grade_hi_h, 40.0, "grade_hi_h");
    close(p.grade_hi_s, 20.0, "grade_hi_s");
    close(p.grade_mid_h, 120.0, "grade_mid_h");
    close(p.grade_mid_s, 10.0, "grade_mid_s");
    close(p.grade_balance, -15.0, "grade_balance");
    close(p.grade_sh_l, 5.0, "grade_sh_l");
    close(p.grade_mid_l, -3.0, "grade_mid_l");
    close(p.grade_hi_l, 2.0, "grade_hi_l");
    close(p.grade_global_h, 300.0, "grade_global_h");
    close(p.grade_global_s, 7.0, "grade_global_s");
    close(p.grade_global_l, -2.0, "grade_global_l");
    close(p.grade_blending, 60.0, "grade_blending");

    close_arr(
        &p.hsl_h,
        &[5.0, -3.0, 2.0, 4.0, -6.0, 7.0, 8.0, -9.0],
        "hsl_h",
    );
    close_arr(
        &p.hsl_s,
        &[-2.0, 0.0, 3.0, 4.0, 5.0, 6.0, -7.0, -8.0],
        "hsl_s",
    );
    close_arr(&p.hsl_l, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], "hsl_l");
    close_arr(
        &p.mono_h,
        &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
        "mono_h",
    );
    close(p.mono, 0.0, "mono");

    assert_curve(
        &p.curve_rgb,
        &[[0, 0], [32, 20], [128, 140], [224, 240], [255, 255]],
        "curve_rgb",
    );
    assert_curve(&p.curve_r, &[[0, 10], [128, 128], [255, 245]], "curve_r");
    assert_curve(&p.curve_g, &[[0, 0], [128, 128], [255, 255]], "curve_g");
    assert_curve(
        &p.curve_b,
        &[[0, 5], [64, 70], [192, 200], [255, 250]],
        "curve_b",
    );
}

/// 前缀、属性顺序、命名空间声明位置都可变；实体要能还原。
#[test]
fn reads_varying_prefix_order_and_entities() {
    let dir = temp_dir("prefix");
    let path = dir.join("weird.xmp");
    let xml = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <w:RDF xmlns:w="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <w:Description xmlns:acr="http://ns.adobe.com/camera-raw-settings/1.0/" w:about=""
    acr:Contrast2012="&#49;2" acr:Exposure2012="1.25" acr:Tint="-15"
    acr:WhiteBalance="Custom" acr:Temperature="6050" acr:ProcessVersion="15.4">
   <acr:ToneCurvePV2012>
    <w:Seq>
     <w:li>255, 255</w:li>
     <w:li>128, 140</w:li>
     <w:li>0, 0</w:li>
    </w:Seq>
   </acr:ToneCurvePV2012>
   <acr:Name><w:Alt><w:li xml:lang="x-default">A &amp; B</w:li></w:Alt></acr:Name>
  </w:Description>
 </w:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;
    std::fs::write(&path, xml).expect("写测试文件");

    let p = xmp::read(&path).expect("读变体 XMP");
    close(p.exposure, 1.25, "exposure");
    close(p.contrast, 12.0, "contrast（实体）");
    close(p.temperature, (6050.0 - 5500.0) / 45.0, "绝对色温");
    close(p.tint, -15.0 / 1.5, "绝对色调");
    assert_curve(
        &p.curve_rgb,
        &[[0, 0], [128, 140], [255, 255]],
        "curve_rgb 排序",
    );
}

/// 相机旁车常见多段 rdf:Description：第一段没有 crs 键，参数在第二段。
#[test]
fn reads_sidecar_style_multiple_descriptions() {
    let dir = temp_dir("sidecar");
    let path = dir.join("IMG_0001.xmp");
    let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/">
   <xmp:CreatorTool>Camera Raw 15.4</xmp:CreatorTool>
  </rdf:Description>
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
   <crs:Exposure2012>-0.50</crs:Exposure2012>
   <crs:ToneCurvePV2012Blue>
    <rdf:Seq><rdf:li>0, 12</rdf:li><rdf:li>255, 243</rdf:li></rdf:Seq>
   </crs:ToneCurvePV2012Blue>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    std::fs::write(&path, xml).expect("写测试文件");

    let p = xmp::read(&path).expect("读旁车");
    close(p.exposure, -0.5, "exposure");
    assert_curve(&p.curve_b, &[[0, 12], [255, 243]], "curve_b");
    assert_curve(&p.curve_rgb, &[[0, 0], [255, 255]], "curve_rgb 默认");
}

/// PV2010 同义键兜底；新键存在时优先新键；缺键保持默认。
#[test]
fn reads_legacy_keys_and_keeps_defaults() {
    let dir = temp_dir("legacy");
    let path = dir.join("old.xmp");
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   crs:ProcessVersion="5.7" crs:Exposure="0.5" crs:Exposure2012="0.9"
   crs:Contrast="10" crs:Vibrance="21" crs:Saturation="-16"/>
 </rdf:RDF>
</x:xmpmeta>"#;
    std::fs::write(&path, xml).expect("写测试文件");

    let p = xmp::read(&path).expect("读旧预设");
    close(p.exposure, 0.9, "Exposure2012 优先");
    close(p.contrast, 10.0, "Contrast 兜底");
    close(p.vibrance, 21.0, "Vibrance");
    close(p.saturation, -16.0, "Saturation");

    // 缺失键保持 GradeParams 默认值。
    close(p.clarity, 0.0, "clarity");
    close(p.vig_midpoint, 50.0, "vig_midpoint");
    close(p.sharp_radius, 1.0, "sharp_radius");
    close(p.sharp_detail, 25.0, "sharp_detail");
    close(p.nr_color_smooth, 50.0, "nr_color_smooth");
    close(p.par_split1, 25.0, "par_split1");
    assert!(p.masks.is_empty());
    assert_curve(&p.curve_rgb, &[[0, 0], [255, 255]], "curve_rgb 默认");
    assert_curve(&p.curve_r, &[[0, 0], [255, 255]], "curve_r 默认");
}

/// 曲线点：越界裁剪、重复横坐标取后一个、没有逗号的点忽略。
#[test]
fn reads_curve_clips_sorts_and_dedupes() {
    let dir = temp_dir("curve");
    let path = dir.join("curve.xmp");
    let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0">
   <crs:ToneCurvePV2012>
    <rdf:Seq>
     <rdf:li>300, -10</rdf:li>
     <rdf:li>128, 100</rdf:li>
     <rdf:li>128, 120</rdf:li>
     <rdf:li>junk</rdf:li>
     <rdf:li>0, 0</rdf:li>
    </rdf:Seq>
   </crs:ToneCurvePV2012>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
    std::fs::write(&path, xml).expect("写测试文件");

    let p = xmp::read(&path).expect("读曲线");
    assert_curve(
        &p.curve_rgb,
        &[[0, 0], [128, 120], [255, 0]],
        "curve_rgb 规整",
    );
}

/// 没有任何 crs 键就是空预设；坏 XML 报解析错误而不是崩。
#[test]
fn reports_empty_and_malformed_files() {
    let dir = temp_dir("errors");

    let empty = dir.join("empty.xmp");
    std::fs::write(
        &empty,
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><xmp:CreatorTool>Lapse</xmp:CreatorTool></rdf:Description></rdf:RDF></x:xmpmeta>"#,
    )
    .expect("写测试文件");
    assert!(matches!(xmp::read(&empty), Err(XmpError::Empty)));

    let missing = dir.join("missing.xmp");
    assert!(matches!(xmp::read(&missing), Err(XmpError::Io(_))));

    let broken = dir.join("broken.xmp");
    std::fs::write(&broken, "<not-xmp").expect("写测试文件");
    assert!(matches!(xmp::read(&broken), Err(XmpError::Parse(_))));
}

#[test]
fn reads_color_grade_precedence_like_python() {
    let dir = temp_dir("grade");
    let path = dir.join("grade.xmp");
    let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
   crs:SplitToningShadowHue="10" crs:ColorGradeShadowHue="20"
   crs:SplitToningHighlightHue="30" crs:ColorGradeHighlightHue="40"
   crs:SplitToningShadowSaturation="50" crs:ColorGradeShadowSat="60"
   crs:SplitToningHighlightSaturation="70" crs:ColorGradeHighlightSat="80"/>
 </rdf:RDF>
</x:xmpmeta>"#;
    std::fs::write(&path, xml).expect("写测试文件");

    let p = xmp::read(&path).expect("读色彩分级");
    close(p.grade_sh_h, 20.0, "色相 ColorGrade 优先");
    close(p.grade_hi_h, 40.0, "高光色相 ColorGrade 优先");
    close(p.grade_sh_s, 50.0, "饱和度 SplitToning 优先");
    close(p.grade_hi_s, 70.0, "高光饱和度 SplitToning 优先");
}

#[test]
fn sidecar_path_replaces_extension() {
    assert_eq!(
        xmp::sidecar_for(Path::new("/photos/IMG_0001.CR3")),
        Path::new("/photos/IMG_0001.xmp")
    );
    assert_eq!(
        xmp::sidecar_for(Path::new("/photos/DSC0001")),
        Path::new("/photos/DSC0001.xmp")
    );
    assert_eq!(
        xmp::sidecar_for(Path::new("/photos/already.xmp")),
        Path::new("/photos/already.xmp")
    );
}

/// 照片旁车：`dc:` 四项与 `crs:` 调色写在同一个 `Description` 里。
/// 调色那半用读预设的同一个读头验回来，文档信息那半看转义与折叠口径。
#[test]
fn sidecar_carries_dc_and_crs() {
    let dir = temp_dir("sidecar");
    let image = dir.join("IMG_0001.png");
    std::fs::write(&image, b"placeholder").expect("写占位图");

    let mut params = GradeParams {
        exposure: 0.75,
        contrast: -20.0,
        temperature: 12.0,
        vignette: -35.0,
        ..GradeParams::default()
    };
    params.curve_rgb = vec![[0.0, 8.0], [128.0, 120.0], [255.0, 250.0]];
    let meta = Metadata {
        title: "云海 <日出> & 前景".to_owned(),
        description: "  松下 G9   间隔 2 秒 ".to_owned(),
        creator: "张三".to_owned(),
        copyright: "© 2026 张三".to_owned(),
    };

    let path = xmp::write_sidecar(&image, &params, &meta).expect("写旁车");
    assert_eq!(path, xmp::sidecar_for(&image), "旁车要落在图片旁边");

    let back = xmp::read(&path).expect("旁车里的 crs 读不回来");
    close(back.exposure, params.exposure, "曝光");
    close(back.contrast, params.contrast, "对比度");
    close(back.temperature, params.temperature, "色温");
    // LR 的暗角正数提亮，这边正数压暗：写出去再读回来必须还原成同一个方向。
    close(back.vignette, params.vignette, "暗角");
    assert_eq!(
        back.curve_rgb,
        vec![[0.0, 8.0], [128.0, 120.0], [255.0, 250.0]],
        "曲线没往返"
    );

    let text = std::fs::read_to_string(&path).expect("读旁车");
    assert!(
        text.contains("云海 &lt;日出&gt; &amp; 前景"),
        "标题没做 XML 转义：{text}"
    );
    assert!(
        text.contains("松下 G9 间隔 2 秒"),
        "说明没按 cleaned() 折叠空白：{text}"
    );
    assert!(
        text.contains("<dc:creator>\n    <rdf:Seq>\n     <rdf:li>张三</rdf:li>"),
        "创作者不是有序列表：{text}"
    );
    assert!(text.contains("© 2026 张三"), "版权丢了");
    assert!(text.contains("xmlns:dc="), "没声明都柏林核心命名空间");
    assert!(!text.contains("<crs:Name>"), "旁车不是预设，不该有预设名");
}

/// 元数据全空（或只剩空白）时旁车只有调色那一半：不往文件里塞空标签。
#[test]
fn sidecar_skips_blank_metadata() {
    let dir = temp_dir("sidecar-blank");
    let image = dir.join("f000.png");
    std::fs::write(&image, b"placeholder").expect("写占位图");

    let meta = Metadata {
        description: "   ".to_owned(),
        ..Metadata::default()
    };
    let path = xmp::write_sidecar(&image, &GradeParams::default(), &meta).expect("写旁车");
    let text = std::fs::read_to_string(&path).expect("读旁车");
    assert!(!text.contains("<dc:"), "空白元数据不该写 dc 标签：{text}");
    assert!(
        text.contains("crs:HasSettings=\"True\""),
        "调色那一半不能丢"
    );
    assert!(
        xmp::read(&path).is_ok(),
        "旁车里的 crs 键读不出来会被判成空预设"
    );
}
