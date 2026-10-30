//! 队列端到端：两段序列各用各的工程参数，中间帧按 keep_frames 清理。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::grade::GradeParams;
use aevum_core::project::{self, ProjectFile};
use aevum_core::queue::{self, QueueItem};
use image::{Rgb, RgbImage};

const FFMPEG: &str = r"D:\work\project\pycharm\p\pythonProject4\.venv\Lib\site-packages\imageio_ffmpeg\binaries\ffmpeg-win-x86_64-v7.1.exe";

fn ffmpeg() -> Option<PathBuf> {
    let p = PathBuf::from(FFMPEG);
    p.is_file().then_some(p)
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aevum-queue-{tag}-{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn make_sequence(dir: &Path, luma: u8) {
    fs::create_dir_all(dir).unwrap();
    for i in 0..3 {
        let img = RgbImage::from_pixel(160, 90, Rgb([luma, luma, luma]));
        img.save(dir.join(format!("f{i:04}.png"))).unwrap();
    }
}

fn write_project(root: &Path, exposure: f64) {
    let path = project::project_path(root);
    let file = ProjectFile {
        root: root.display().to_string(),
        params: GradeParams {
            exposure,
            ..GradeParams::default()
        },
        ..ProjectFile::default()
    };
    project::save(&path, &file).unwrap();
}

fn item(root: &Path, out: &Path, keep_frames: bool) -> QueueItem {
    QueueItem {
        width: 320,
        height: 180,
        keep_frames,
        ..QueueItem::new(root, out)
    }
}

fn mean_luma(path: &Path) -> f64 {
    let img = image::open(path).unwrap().to_rgb8();
    let sum: f64 = img.pixels().map(|p| p.0[0] as f64).sum();
    sum / img.pixels().len() as f64
}

#[test]
fn queue_renders_each_sequence_with_its_own_project() {
    let Some(ffmpeg) = ffmpeg() else {
        eprintln!("跳过：本机没有测试用 ffmpeg");
        return;
    };
    let tmp = TempDir::new("e2e");

    let bright = tmp.path.join("bright");
    let dark = tmp.path.join("dark");
    make_sequence(&bright, 128);
    make_sequence(&dark, 128);
    write_project(&bright, 1.0);
    write_project(&dark, -1.0);

    let bright_out = tmp.path.join("bright.mp4");
    let dark_out = tmp.path.join("dark.mp4");
    let cancel = AtomicBool::new(false);
    let noop = |_p: aevum_core::export::JobProgress| {};

    let made =
        queue::render_item(&item(&bright, &bright_out, true), &ffmpeg, &cancel, &noop).unwrap();
    assert_eq!(made, bright_out);
    assert!(bright_out.metadata().unwrap().len() > 0);

    queue::render_item(&item(&dark, &dark_out, true), &ffmpeg, &cancel, &noop).unwrap();

    let bright_frame = queue::frames_dir(&bright_out).join("frame_000000.png");
    let dark_frame = queue::frames_dir(&dark_out).join("frame_000000.png");
    let a = mean_luma(&bright_frame);
    let b = mean_luma(&dark_frame);
    assert!(
        a - b > 60.0,
        "两段序列应各用各的曝光：亮 {a:.1} / 暗 {b:.1}"
    );

    // keep_frames = false：编码成功后中间帧目录整个清掉。
    let plain_root = tmp.path.join("plain");
    make_sequence(&plain_root, 128);
    let plain_out = tmp.path.join("plain.mp4");
    queue::render_item(
        &item(&plain_root, &plain_out, false),
        &ffmpeg,
        &cancel,
        &noop,
    )
    .unwrap();
    assert!(plain_out.is_file());
    assert!(!queue::frames_dir(&plain_out).exists());
}

#[test]
fn queue_render_uses_the_named_copy_when_asked() {
    let Some(ffmpeg) = ffmpeg() else {
        eprintln!("跳过：本机没有测试用 ffmpeg");
        return;
    };
    let tmp = TempDir::new("copy");
    let root = tmp.path.join("seq");
    make_sequence(&root, 128);
    let mut file = ProjectFile {
        root: root.display().to_string(),
        params: GradeParams {
            exposure: -1.0,
            ..GradeParams::default()
        },
        ..ProjectFile::default()
    };
    file.copies.push(aevum_core::project::GradeCopy {
        name: "亮版".to_owned(),
        params: GradeParams {
            exposure: 1.0,
            ..GradeParams::default()
        },
        keyframes: std::collections::BTreeMap::new(),
    });
    project::save(&project::project_path(&root), &file).unwrap();

    let cancel = AtomicBool::new(false);
    let noop = |_p: aevum_core::export::JobProgress| {};

    let copy_out = tmp.path.join("copy.mp4");
    let mut with_copy = item(&root, &copy_out, true);
    with_copy.copy = "亮版".to_owned();
    queue::render_item(&with_copy, &ffmpeg, &cancel, &noop).unwrap();

    let main_out = tmp.path.join("main.mp4");
    queue::render_item(&item(&root, &main_out, true), &ffmpeg, &cancel, &noop).unwrap();

    let a = mean_luma(&queue::frames_dir(&copy_out).join("frame_000000.png"));
    let b = mean_luma(&queue::frames_dir(&main_out).join("frame_000000.png"));
    assert!(
        a - b > 60.0,
        "副本与主调色应各用各的曝光：副本 {a:.1} / 主 {b:.1}"
    );

    // 名字对不上现在就报错，不留一段渲到一半的废产物。
    let bad_out = tmp.path.join("bad.mp4");
    let mut bad = item(&root, &bad_out, false);
    bad.copy = "没有这版".to_owned();
    let err = queue::render_item(&bad, &ffmpeg, &cancel, &noop).unwrap_err();
    assert!(err.to_string().contains("没有这版"), "{err}");
    assert!(!bad_out.is_file());
}

/// 队列每一段都读**它自己的**工程：那一段开了变速，成片就该跟着变速，
/// 不该悄悄退回 1:1（调色、水印、副本都是这个口径，变速没理由例外）。
#[test]
fn queue_render_follows_the_projects_retime() {
    let Some(ffmpeg) = ffmpeg() else {
        eprintln!("跳过：本机没有测试用 ffmpeg");
        return;
    };
    let tmp = TempDir::new("retime");
    let cancel = AtomicBool::new(false);
    let noop = |_p: aevum_core::export::JobProgress| {};

    let rendered = |tag: &str, retime: aevum_core::retime::Retime| -> Vec<PathBuf> {
        let root = tmp.path.join(tag);
        make_sequence(&root, 128);
        let mut file = ProjectFile {
            root: root.display().to_string(),
            ..ProjectFile::default()
        };
        file.render.retime = retime;
        project::save(&project::project_path(&root), &file).unwrap();
        let out = tmp.path.join(format!("{tag}.mp4"));
        queue::render_item(&item(&root, &out, true), &ffmpeg, &cancel, &noop).unwrap();
        let mut frames: Vec<PathBuf> = fs::read_dir(queue::frames_dir(&out))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "png"))
            .collect();
        frames.sort();
        frames
    };

    // 3 源帧 @3× → 1 帧；@0.5× 慢放 → 6 帧；没开变速 → 3 帧原样。
    let fast = rendered("fast", aevum_core::retime::Retime::uniform(3.0, 0.0));
    assert_eq!(fast.len(), 1, "3× 应把三帧压成一帧：{fast:?}");
    let slow = rendered("slow", aevum_core::retime::Retime::uniform(0.5, 0.5));
    assert_eq!(slow.len(), 6, "0.5× 应把三帧拉成六帧：{slow:?}");
    let flat = rendered("flat", aevum_core::retime::Retime::default());
    assert_eq!(flat.len(), 3, "没开变速就是 1:1：{flat:?}");
}
