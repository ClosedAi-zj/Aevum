//! 工程文件往返与向后兼容。

use std::collections::BTreeMap;

use lumen_core::align::CropRect;
use lumen_core::grade::GradeParams;
use lumen_core::project::{self, AlignState, ProjectFile, RenderState};
use lumen_core::watermark::{Spot, Watermark, WatermarkKind};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen_project_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn roundtrip_keeps_everything() {
    let dir = temp_dir("roundtrip");
    let path = project::project_path(&dir);
    let mut p = ProjectFile::default();
    p.root = "D:/frames".to_owned();
    p.reference = 3;
    p.params.exposure = 0.75;
    p.params.contrast = -12.0;
    p.params.curve_rgb = vec![[0.0, 0.0], [64.0, 70.0], [255.0, 250.0]];
    p.params.hsl_h[2] = 8.0;
    p.preset = "暖调".to_owned();
    p.ratings.insert(1, 5);
    p.flags.insert(2, -1);
    p.labels.insert(2, 3);
    let mut kf = BTreeMap::new();
    let mut k = GradeParams::default();
    k.exposure = 1.25;
    kf.insert(10usize, k.clone());
    p.keyframes = kf;
    p.def_window = 21.0;
    p.def_strength = 80.0;
    p.def_multi = true;
    p.crop = Some(CropRect {
        x: 4,
        y: 6,
        w: 1900,
        h: 1060,
    });
    p.align = Some(AlignState {
        reference: 0,
        estimator: "template".to_owned(),
        offsets: vec![[0.0, 0.0], [1.5, -2.25]],
        scores: vec![1.0, 0.98],
        issues: Vec::new(),
        crop: Some(CropRect {
            x: 3,
            y: 3,
            w: 1910,
            h: 1070,
        }),
    });
    p.render = RenderState {
        codec: "h264".to_owned(),
        crf: 23,
        fps: 30.0,
        format: "jpeg".to_owned(),
        quality: 72,
        keep_exif: true,
        xmp_sidecar: true,
        sharpen_for: "matte".to_owned(),
        sharpen_strength: "high".to_owned(),
        watermark: Watermark {
            enabled: true,
            kind: WatermarkKind::Text,
            text: "© 光时 · Lumen Studio".to_owned(),
            spot: Spot::TopLeft,
            opacity: 85,
            size: 6,
            ..Default::default()
        },
        ..Default::default()
    };
    p.font_level = 2;
    p.metadata = project::Metadata {
        title: "云海日出".to_owned(),
        description: "松下 G9 · 间隔 2 秒".to_owned(),
        creator: "张三".to_owned(),
        copyright: "© 2026 张三".to_owned(),
    };
    p.stacks = vec![vec![0, 1, 2], vec![5, 6]];
    let mut copy = project::GradeCopy {
        name: "亮版".to_owned(),
        ..Default::default()
    };
    copy.params.exposure = 1.5;
    copy.keyframes.insert(4usize, k.clone());
    p.copies = vec![copy];

    project::save(&path, &p).unwrap();
    assert!(path.is_file());
    let got = project::load(&path).unwrap();

    assert_eq!(got.root, p.root);
    assert_eq!(got.reference, 3);
    assert_eq!(got.params.exposure, 0.75);
    assert_eq!(got.params.curve_rgb, p.params.curve_rgb);
    assert_eq!(got.ratings.get(&1), Some(&5));
    assert_eq!(got.flags.get(&2), Some(&-1));
    assert_eq!(got.keyframes.get(&10).unwrap().exposure, 1.25);
    assert_eq!(got.align.as_ref().unwrap().offsets.len(), 2);
    assert_eq!(got.render.codec, "h264");
    // 成片档（容器、质量、EXIF 搬运、输出锐化）也要活过工程文件。
    assert_eq!(got.render.format, "jpeg");
    assert_eq!(got.render.quality, 72);
    assert!(got.render.keep_exif);
    assert!(got.render.xmp_sidecar, "旁车开关没活过工程文件");
    assert_eq!(got.render.sharpen_for, "matte");
    assert_eq!(got.render.sharpen_strength, "high");
    // 水印是一整块设定：开关、正文、九宫格、不透明度一个都不许在工程文件里丢。
    assert_eq!(got.render.watermark, p.render.watermark);
    assert!(got.render.watermark.enabled);
    assert_eq!(got.render.watermark.spot, Spot::TopLeft);
    assert_eq!(got.font_level, 2);
    // 说明与版权整块也要活过工程文件：导出与图库都读这一份。
    assert_eq!(got.metadata, p.metadata);
    assert_eq!(got.metadata.creator, "张三");
    // 堆叠也要活过工程文件：图库折叠显示靠它。
    assert_eq!(got.stacks, p.stacks);
    // 调色副本连关键帧一起往返：多版调色并存全靠工程文件记着。
    assert_eq!(got.copies.len(), 1);
    assert_eq!(got.copies[0].name, "亮版");
    assert_eq!(got.copies[0].params.exposure, 1.5);
    assert_eq!(got.copies[0].keyframes.get(&4).unwrap().exposure, 1.25);

    // 再存一次，确保替换已存在的文件也能成功。
    project::save(&path, &got).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn old_file_with_missing_fields_uses_defaults() {
    let dir = temp_dir("legacy");
    let path = project::project_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"version":1,"root":"D:/old","params":{"exposure":0.5},
            "render":{"format":"png8"}}"#,
    )
    .unwrap();

    let got = project::load(&path).unwrap();
    assert_eq!(got.root, "D:/old");
    assert_eq!(got.params.exposure, 0.5);
    assert_eq!(got.params.contrast, 0.0);
    assert_eq!(got.render.codec, "h265");
    // 旧工程的 render 里没有成片档那几个键：按默认走，不能读失败。
    assert_eq!(got.render.quality, 90);
    assert!(!got.render.keep_exif);
    assert!(!got.render.xmp_sidecar, "旧工程不该凭空多出旁车");
    assert_eq!(got.render.sharpen_for, "off");
    // 水印整块缺字段：没开就是没开，旧工程不许因为多了这一段读不进来。
    assert_eq!(got.render.watermark, Watermark::default());
    assert!(got.align.is_none());
    // 旧工程没有 metadata 这一段：四项全空，不许因此读失败。
    assert_eq!(got.metadata, project::Metadata::default());
    assert!(got.metadata.is_empty());
    // 旧工程没有 stacks 这一段：就是没叠。
    assert!(got.stacks.is_empty());
    // 旧工程没有 copies 这一段：就是没有副本。
    assert!(got.copies.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn newer_version_is_rejected() {
    let dir = temp_dir("version");
    let path = project::project_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"version":99}"#).unwrap();
    match project::load(&path) {
        Err(project::ProjectError::Version(99)) => {}
        other => panic!("应当拒绝新版本，实得 {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 改名前的工程把参数存在 `<序列>/.lapse/`：打开序列时搬进 `.lumen/`，接着用。
#[test]
fn legacy_guard_dir_moves_aside_on_load() {
    let dir = temp_dir("legacy_guard");
    std::fs::write(dir.join("frame_0001.jpg"), b"not really a jpeg").unwrap();
    let guard = dir.join(".lapse");
    std::fs::create_dir_all(&guard).unwrap();
    std::fs::write(guard.join("project.json"), r#"{"params":{"exposure":0.4}}"#).unwrap();

    let seq = lumen_core::Sequence::open(&dir).expect("打开序列");
    assert!(!dir.join(".lapse").exists(), "旧护带目录该搬走");
    assert!(
        dir.join(".lumen/project.json").is_file(),
        "工程文件要在名字对的那边"
    );
    let loaded = project::load(&project::project_path(&seq.root)).unwrap();
    assert_eq!(loaded.params.exposure, 0.4, "搬完参数要还在");

    assert!(
        lumen_core::Sequence::open(&dir).is_ok(),
        "再开一次不该被搬不动卡住"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 两个目录同时存在说明用户自己动过，猜哪边是对的会覆盖数据，所以原地不动。
#[test]
fn both_guard_dirs_are_left_alone() {
    let dir = temp_dir("both_guard");
    std::fs::create_dir_all(dir.join(".lapse")).unwrap();
    std::fs::write(dir.join(".lapse/project.json"), r#"{"preset":"旧"}"#).unwrap();
    std::fs::create_dir_all(dir.join(".lumen")).unwrap();
    std::fs::write(dir.join(".lumen/project.json"), r#"{"preset":"新"}"#).unwrap();

    assert!(!project::migrate_guard_dir(&dir), "两边都在时不能搬");
    assert!(
        dir.join(".lapse/project.json").is_file(),
        "旧目录要原样留着"
    );
    let loaded = project::load(&project::project_path(&dir)).unwrap();
    assert_eq!(loaded.preset, "新", "读的还是 .lumen 那份");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 根上那些 `.lapse_ratings.json` 是 Python 版的同名前缀文件，不是目录，别碰。
#[test]
fn legacy_files_with_same_prefix_are_untouched() {
    let dir = temp_dir("prefix_guard");
    std::fs::write(dir.join(".lapse_ratings.json"), "{}").unwrap();

    assert!(!project::migrate_guard_dir(&dir));
    assert!(
        dir.join(".lapse_ratings.json").is_file(),
        "同名前缀文件不该被动"
    );
    assert!(!dir.join(".lumen").exists(), "不该凭空造出护带目录");
    let _ = std::fs::remove_dir_all(&dir);
}
