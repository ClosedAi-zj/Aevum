use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("aevum-ui-raw-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// RAW 首次导入：锐化 40 / 亮度降噪 25；普通图片序列零影响；
/// 工程文件在场（二次打开）不再触发——「只一次」的保证。
#[test]
fn raw_defaults_apply_only_for_first_raw_import() {
    // 只看扩展名认领，序列构建不解码像素，占位字节即可。
    let raw_dir = temp_dir("raw");
    std::fs::write(raw_dir.join("f001.nef"), b"placeholder").expect("写占位 RAW");
    let mut st = AppState::new();
    st.seq = Some(aevum_core::sequence::Sequence::open(&raw_dir).expect("开 RAW 序列"));
    st.apply_raw_import_defaults(&raw_dir);
    assert_eq!(st.params.sharp_amount, 40.0, "RAW 首次导入要套锐化 40");
    assert_eq!(st.params.sharp_protect, 20.0, "RAW 首次导入要套锐化保护 20");
    assert_eq!(st.params.nr_luma, 25.0, "RAW 首次导入要套亮度降噪 25");
    assert!(st.status.contains("40"), "状态栏要说明默认参数");

    // 普通图片序列：条件不满足，参数不动。
    let png_dir = temp_dir("png");
    let img = image::RgbImage::from_pixel(8, 6, image::Rgb([20, 40, 60]));
    img.save(png_dir.join("f001.png")).expect("写 PNG");
    st.params = GradeParams::default();
    st.seq = Some(aevum_core::sequence::Sequence::open(&png_dir).expect("开 PNG 序列"));
    st.apply_raw_import_defaults(&png_dir);
    assert_eq!(st.params.sharp_amount, 0.0, "普通序列不该套 RAW 默认");

    // 工程文件在场：用户已保存过参数，不得覆盖。
    let proj = ProjectFile::default();
    project::save(&project::project_path(&raw_dir), &proj).expect("存工程");
    st.seq = Some(aevum_core::sequence::Sequence::open(&raw_dir).expect("再开 RAW 序列"));
    st.apply_raw_import_defaults(&raw_dir);
    assert_eq!(st.params.sharp_amount, 0.0, "有工程文件时不得覆盖");
    let _ = std::fs::remove_dir_all(&raw_dir);
    let _ = std::fs::remove_dir_all(&png_dir);
}

/// 边车检测：同目录有 .xmp 就记录提示，只提示不套用。
#[test]
fn sidecar_hint_detects_but_never_applies() {
    let dir = temp_dir("sidecar");
    let img = image::RgbImage::from_pixel(8, 6, image::Rgb([20, 40, 60]));
    img.save(dir.join("f001.png")).expect("写 PNG");
    std::fs::write(dir.join("f001.xmp"), b"<x/>").expect("写边车");

    let mut st = AppState::new();
    st.seq = Some(aevum_core::sequence::Sequence::open(&dir).expect("开序列"));
    st.detect_sidecar_hint();
    let hint = st.sidecar_hint.clone().expect("要检测到边车");
    assert!(hint.file_name().unwrap() == "f001.xmp");

    // 只提示：参数保持打开时的样子，不因检测而改变。
    assert_eq!(st.params.exposure, 0.0);

    // 无边车的序列：提示为空。
    let plain = temp_dir("plain");
    img.save(plain.join("f001.png")).expect("写 PNG");
    st.seq = Some(aevum_core::sequence::Sequence::open(&plain).expect("开普通序列"));
    st.detect_sidecar_hint();
    assert!(st.sidecar_hint.is_none(), "没有边车不该有提示");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&plain);
}
