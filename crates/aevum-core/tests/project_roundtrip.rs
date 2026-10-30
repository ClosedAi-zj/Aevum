//! 工程文件往返与向后兼容。

use std::collections::BTreeMap;

use aevum_core::align::CropRect;
use aevum_core::grade::GradeParams;
use aevum_core::project::{self, AlignState, ProjectFile, RenderState};
use aevum_core::watermark::{Spot, Watermark, WatermarkKind};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aevum_project_{tag}_{}", std::process::id()));
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
            text: "© 恒晷 · Aevum Studio".to_owned(),
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

// ---------------------------------------------------------------- 护带目录三级链
// C1–C5：`.lapse`（更早）→ `.lumen`（v0.4）→ `.aevum`（本版）。
// 这几条守的是「用户升级后工程打不开/参数看起来丢了」这一类最高严重度的故障。

/// 统计根目录下所有「护带风格」的目录名（以点开头、且是旧名或新名之一）。
fn guard_like_dirs(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .map(|it| {
            it.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n == ".aevum" || n == ".lumen" || n == ".lapse")
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// C1：只有 `.lumen/`（v0.4 形态）→ 打开后 `.aevum/` 出现、`.lumen` 消失、参数读到。
#[test]
fn c1_v04_guard_dir_is_adopted() {
    let dir = temp_dir("c1_lumen");
    std::fs::write(dir.join("frame_0001.jpg"), b"not really a jpeg").unwrap();
    std::fs::create_dir_all(dir.join(".lumen")).unwrap();
    std::fs::write(
        dir.join(".lumen/project.json"),
        r#"{"params":{"exposure":0.4}}"#,
    )
    .unwrap();

    let seq = aevum_core::Sequence::open(&dir).expect("打开序列");

    assert_eq!(
        guard_like_dirs(&dir),
        vec![".aevum".to_owned()],
        ".lumen 应当已被搬成 .aevum，且不留残迹"
    );
    let loaded = project::load(&project::project_path(&seq.root)).unwrap();
    assert_eq!(loaded.params.exposure, 0.4, "搬完参数要还在");
    let _ = std::fs::remove_dir_all(&dir);
}

/// C2：只有 `.lapse/`（Python 版形态）→ **直接**搬成 `.aevum/`，不经过 `.lumen`。
#[test]
fn c2_oldest_guard_dir_is_adopted_directly() {
    let dir = temp_dir("c2_lapse");
    std::fs::write(dir.join("frame_0001.jpg"), b"not really a jpeg").unwrap();
    let guard = dir.join(".lapse");
    std::fs::create_dir_all(&guard).unwrap();
    std::fs::write(guard.join("project.json"), r#"{"params":{"exposure":0.4}}"#).unwrap();

    let seq = aevum_core::Sequence::open(&dir).expect("打开序列");

    assert_eq!(
        guard_like_dirs(&dir),
        vec![".aevum".to_owned()],
        ".lapse 应当直接搬成 .aevum"
    );
    let loaded = project::load(&project::project_path(&seq.root)).unwrap();
    assert_eq!(loaded.params.exposure, 0.4, "搬完参数要还在");
    let _ = std::fs::remove_dir_all(&dir);
}

/// C3：`.aevum` 与 `.lumen` 都在 → `.aevum` 为准，`.lumen` **原封不动**。
///
/// 猜哪边是对的会覆盖数据，所以两边都在时一律不动旧目录（旧目录不删是硬约束：
/// 用户随时能回去翻）。
#[test]
fn c3_both_dirs_present_new_one_wins_and_old_is_untouched() {
    let dir = temp_dir("c3_both");
    std::fs::create_dir_all(dir.join(".lumen")).unwrap();
    std::fs::write(dir.join(".lumen/project.json"), r#"{"preset":"旧"}"#).unwrap();
    std::fs::create_dir_all(dir.join(".aevum")).unwrap();
    std::fs::write(dir.join(".aevum/project.json"), r#"{"preset":"新"}"#).unwrap();

    assert_eq!(
        project::adopt_guard_dir(&dir),
        dir.join(".aevum"),
        "两边都在时必须返回 .aevum"
    );
    assert_eq!(
        guard_like_dirs(&dir),
        vec![".aevum".to_owned(), ".lumen".to_owned()],
        ".lumen 必须原封不动留着"
    );
    let loaded = project::load(&project::project_path(&dir)).unwrap();
    assert_eq!(loaded.preset, "新", "读的必须是 .aevum 那份");
    let _ = std::fs::remove_dir_all(&dir);
}

/// C4：同一目录连续打开 3 次 → 不反复搬、无中间目录残留。
#[test]
fn c4_repeated_open_does_not_thrash() {
    let dir = temp_dir("c4_repeat");
    std::fs::write(dir.join("frame_0001.jpg"), b"not really a jpeg").unwrap();
    std::fs::create_dir_all(dir.join(".lumen")).unwrap();
    std::fs::write(dir.join(".lumen/project.json"), r#"{"preset":"x"}"#).unwrap();

    for i in 0..3 {
        aevum_core::Sequence::open(&dir)
            .unwrap_or_else(|e| panic!("第 {} 次打开失败：{e:?}", i + 1));
        assert_eq!(
            guard_like_dirs(&dir),
            vec![".aevum".to_owned()],
            "第 {} 次打开后不该多出目录",
            i + 1
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// C5：`rename` 失败（只读卷 / 文件被占）时**不能降级成只读**——
/// 要继续用旧目录读写，否则「保存」会静默失败、用户以为存上了。
///
/// 这里走的是 `adopt_guard_dir_with` 的注入缝，而不是去真实文件系统上造失败：
///
/// - 早先的版本用 `chmod 0o555` 让 rename 失败，但 Windows 不是 POSIX 权限模型，
///   当前用户对自己建的临时目录始终有写权 → 用例在**唯一发货平台**上运行时探测后
///   直接 `return`，等于空转。最该保护的一条路径反而没有自动化保护，而且日志里看不出
///   它没跑。
/// - 现在把 `rename` 变成参数，测试传一个必然报 `PermissionDenied` 的假 renamer，
///   任何平台都会真正执行到「搬不动」那条分支。
///
/// 三条断言对应这条路径的三个失效后果：① 返回值必须指向旧目录（否则调用方会去新建
/// 一个空 `.aevum`）② 旧目录必须仍可读写（否则保存静默失败）③ `.aevum` 不能被创建
/// （否则下次打开会优先读到空护带，用户以为参数全丢）。
#[test]
fn c5_rename_failure_falls_back_to_legacy_dir() {
    let dir = temp_dir("c5_fallback");
    std::fs::create_dir_all(dir.join(".lumen")).unwrap();
    std::fs::write(dir.join(".lumen/project.json"), r#"{"preset":"旧"}"#).unwrap();

    let used = project::adopt_guard_dir_with(&dir, |_, _| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "qa stub：模拟只读卷 / 文件被占",
        ))
    });

    // ① 继续用旧目录
    assert_eq!(used, dir.join(".lumen"), "搬不动时必须继续用旧目录");
    // ② 旧目录仍可读写：写一个新文件再读回来，证明不是只读降级
    let probe = used.join("write-probe.json");
    std::fs::write(&probe, br#"{"ok":1}"#).expect("旧目录必须仍可写，否则保存会静默失败");
    assert_eq!(std::fs::read_to_string(&probe).unwrap(), r#"{"ok":1}"#);
    assert_eq!(
        std::fs::read_to_string(used.join("project.json")).unwrap(),
        r#"{"preset":"旧"}"#,
        "旧数据不能被弄丢"
    );
    // ③ 不许凭空造出新护带目录
    assert!(
        !dir.join(".aevum").exists(),
        "rename 失败时绝不能建出空 .aevum（下次打开会优先读到它）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// F8 回归：`.aevum` 万一是**同名文件**（用户手滑、别的工具写出来的），
/// 不能把它当护带目录用 —— 否则后续 `create_dir_all` 失败、保存直接报错。
///
/// 判据必须和旧目录一致（都是「是不是目录」），读路径与写路径才不会各说各话。
#[test]
fn f8_guard_path_that_is_a_regular_file_is_not_used() {
    let dir = temp_dir("f8_file_guard");
    std::fs::create_dir_all(dir.join(".lumen")).unwrap();
    std::fs::write(dir.join(".lumen/project.json"), r#"{"preset":"旧"}"#).unwrap();
    // `.aevum` 是个文件，不是目录
    std::fs::write(dir.join(".aevum"), b"i am not a directory").unwrap();

    assert_eq!(
        project::guard_dir(&dir),
        dir.join(".lumen"),
        "同名文件不能当护带目录，必须回落到旧目录"
    );
    // 搬也搬不过去（目标是文件），此时仍要退回旧目录可用
    let used = project::adopt_guard_dir(&dir);
    assert_eq!(used, dir.join(".lumen"), "搬不过去时仍用旧目录");
    assert!(used.join("project.json").is_file());
    assert!(dir.join(".aevum").is_file(), "那个文件不该被删或改名");
    // 内容也必须一字不动：Windows 上往已存在的文件上 rename 会把它直接覆盖掉
    assert_eq!(
        std::fs::read(dir.join(".aevum")).unwrap(),
        b"i am not a directory",
        "同名文件的内容被覆盖了 —— 这是静默数据丢失"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 根上那些 `.lapse_ratings.json` 是 Python 版的同名前缀**文件**，不是目录，别碰。
#[test]
fn legacy_files_with_same_prefix_are_untouched() {
    let dir = temp_dir("prefix_guard");
    std::fs::write(dir.join(".lapse_ratings.json"), "{}").unwrap();

    let used = project::adopt_guard_dir(&dir);
    assert_eq!(used, dir.join(".aevum"), "没有旧护带目录时返回新名路径");
    assert!(
        dir.join(".lapse_ratings.json").is_file(),
        "同名前缀文件不该被动"
    );
    assert!(!dir.join(".aevum").exists(), "不该凭空造出护带目录");
    let _ = std::fs::remove_dir_all(&dir);
}
