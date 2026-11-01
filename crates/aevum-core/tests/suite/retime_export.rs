//! 时间重映射接进导出：输出帧数、来源选择、混帧像素与陈旧计划的拒绝。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::develop::Ev;
use aevum_core::export::{
    OutputMode, RenderOptions, StillFormat, export_stills, export_stills_planned,
    export_video_planned, plan_for,
};
use aevum_core::grade::GradeParams;
use aevum_core::retime::{Retime, SpeedKey};
use image::{Rgb, RgbImage};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const NO_WINDOW: u32 = 0x0800_0000;

/// 找 ffmpeg。**找不到就让这条用例红**，除非显式声明可以跳过。
///
/// 以前这里是一个写死的绝对路径（某个人的 `pythonProject4\.venv` 里那份 imageio-ffmpeg）：
/// 那台机器上有，别处没有，而且它连 `AEVUM_FFMPEG` 都不读。于是这些用例在 CI 上
/// 永远是「静默通过」的空壳 —— 一个什么都没验的用例比没有用例更糟，因为它给出的是
/// 「绿」。AVIF 导出那个从上线起就坏的 bug 就是这么藏了一整轮的。
///
/// 现在与生产代码共用 `find_ffmpeg()`：`AEVUM_FFMPEG` → exe 旁的 `tools/` → `PATH`。
/// 真没有 ffmpeg 又想跑别的用例，就显式设 `AEVUM_ALLOW_NO_FFMPEG=1`，跳过时会喊一声。
///
/// Locates ffmpeg, and **fails the test when it is missing** unless skipping is declared.
///
/// This used to be a hard-coded absolute path into one developer's virtualenv: present there,
/// absent everywhere else, and it did not even consult `AEVUM_FFMPEG`. So these tests were
/// silently green shells in CI — a test that verifies nothing is worse than no test, because
/// it returns the illusion of green. The AVIF export bug that shipped broken hid behind
/// exactly that for a whole release cycle.
fn ffmpeg() -> Option<PathBuf> {
    if let Some(p) = aevum_core::export::find_ffmpeg() {
        return Some(p);
    }
    if std::env::var_os("AEVUM_ALLOW_NO_FFMPEG").is_some() {
        eprintln!(
            "⚠ 跳过：没有 ffmpeg（已显式声明 AEVUM_ALLOW_NO_FFMPEG）—— 这条用例这次什么都没验。"
        );
        eprintln!("⚠ SKIPPED: no ffmpeg (AEVUM_ALLOW_NO_FFMPEG set) — this test verified nothing.");
        return None;
    }
    panic!(
        "找不到 ffmpeg，这条用例无法执行。
\
         指一个：AEVUM_FFMPEG=/path/to/ffmpeg cargo test ...
\
         或确认这次可以跳过：AEVUM_ALLOW_NO_FFMPEG=1
\
         No ffmpeg found; point at one with AEVUM_FFMPEG, or declare the skip."
    );
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
            "aevum-retime-{tag}-{}-{nanos}-{}",
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

fn stills_options(out_dir: &Path) -> RenderOptions {
    RenderOptions {
        mode: OutputMode::Stills,
        format: StillFormat::Png8,
        out_dir: out_dir.to_path_buf(),
        ..RenderOptions::default()
    }
}

/// 每帧一整片纯色，颜色按帧号给出：混帧结果能直接对着两个源值算。
fn flat_sequence(dir: &Path, colors: &[(u8, u8, u8)]) -> Vec<PathBuf> {
    fs::create_dir_all(dir).unwrap();
    colors
        .iter()
        .enumerate()
        .map(|(i, (r, g, b))| {
            let img = RgbImage::from_pixel(24, 16, Rgb([*r, *g, *b]));
            let path = dir.join(format!("src_{i:04}.png"));
            img.save(&path).unwrap();
            path
        })
        .collect()
}

fn plain_args(n: usize) -> (Vec<GradeParams>, Vec<Ev>, Vec<[f64; 2]>) {
    (
        vec![GradeParams::default(); n],
        vec![Ev::NONE; n],
        vec![[0.0f64; 2]; n],
    )
}

fn pixel(path: &Path) -> (u8, u8, u8) {
    let img = image::open(path).unwrap().to_rgb8();
    let p = img.get_pixel(3, 5);
    (p[0], p[1], p[2])
}

/// 两帧等权混合后的 8bit 取值（round-half-up，和导出末尾那道量化一致）。
fn half(a: u8, b: u8) -> u8 {
    (a as u16 + b as u16).div_ceil(2) as u8
}

fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

#[test]
fn two_x_without_shutter_writes_half_the_frames_with_unchanged_pixels() {
    let src = TempDir::new("2x-src");
    let out = TempDir::new("2x-out");
    let flat = TempDir::new("2x-flat");
    let paths = flat_sequence(
        &src.path,
        &[
            (10, 20, 30),
            (40, 50, 60),
            (70, 80, 90),
            (100, 110, 120),
            (130, 140, 150),
            (160, 170, 180),
            (190, 200, 210),
            (220, 230, 240),
        ],
    );
    let (params, gains, offsets) = plain_args(paths.len());
    let cancel = AtomicBool::new(false);
    let options = stills_options(&out.path);

    let plan = plan_for(&Retime::uniform(2.0, 0.0), paths.len(), 24.0)
        .unwrap()
        .expect("2× 该有计划");
    assert_eq!(plan.frames(), 4);

    let outs = export_stills_planned(
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        Some(&plan),
        &cancel,
        &|_| {},
    )
    .unwrap();
    assert_eq!(outs.len(), 4);

    // 与 1:1 导出偶数帧逐字节相同：变速不重采样就不该动任何一个像素。
    // 两次导出落各自的目录——共用一个目录时，后写的那批会把先写出来的盖掉。
    let one_to_one = export_stills(
        &paths,
        &params,
        &gains,
        &offsets,
        &stills_options(&flat.path),
        &cancel,
        &|_| {},
    )
    .unwrap();
    for (k, src_index) in [0usize, 2, 4, 6].iter().enumerate() {
        assert_eq!(
            pixel(&outs[k]),
            pixel(&one_to_one[*src_index]),
            "输出第 {k} 帧应当等于源第 {src_index} 帧"
        );
        assert_eq!(bytes(&outs[k]), bytes(&one_to_one[*src_index]));
    }
}

#[test]
fn full_shutter_averages_the_crossed_frames_in_the_stills_path() {
    let src = TempDir::new("blur-src");
    let out = TempDir::new("blur-out");
    let paths = flat_sequence(
        &src.path,
        &[(10, 200, 40), (210, 40, 150), (60, 60, 60), (160, 160, 160)],
    );
    let (params, gains, offsets) = plain_args(paths.len());
    let cancel = AtomicBool::new(false);
    let options = stills_options(&out.path);

    let plan = plan_for(&Retime::uniform(2.0, 1.0), paths.len(), 24.0)
        .unwrap()
        .unwrap();
    let outs = export_stills_planned(
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        Some(&plan),
        &cancel,
        &|_| {},
    )
    .unwrap();
    assert_eq!(outs.len(), 2);
    // 2× + 满快门：输出 0 = 源 0、1 各一半；输出 1 = 源 2、3 各一半。
    for (k, a, b) in [(0usize, 0usize, 1usize), (1usize, 2usize, 3usize)] {
        let ea = pixel(&paths[a]);
        let eb = pixel(&paths[b]);
        let got = pixel(&outs[k]);
        let want = (half(ea.0, eb.0), half(ea.1, eb.1), half(ea.2, eb.2));
        for (name, g, w) in [
            ("r", got.0, want.0),
            ("g", got.1, want.1),
            ("b", got.2, want.2),
        ] {
            assert!(
                g.abs_diff(w) <= 1,
                "第 {k} 帧混出的 {name} 是 {g}，期望 {w}（{ea:?} + {eb:?}）"
            );
        }
    }
}

#[test]
fn slow_motion_dissolves_between_neighbours() {
    let src = TempDir::new("slo-mo-src");
    let out = TempDir::new("slo-mo-out");
    let paths = flat_sequence(&src.path, &[(0, 0, 0), (200, 100, 50)]);
    let (params, gains, offsets) = plain_args(paths.len());
    let cancel = AtomicBool::new(false);
    let options = stills_options(&out.path);

    let plan = plan_for(&Retime::uniform(0.5, 0.0), paths.len(), 24.0)
        .unwrap()
        .unwrap();
    assert_eq!(plan.frames(), 4);
    let outs = export_stills_planned(
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        Some(&plan),
        &cancel,
        &|_| {},
    )
    .unwrap();
    assert_eq!(outs.len(), 4);
    assert_eq!(pixel(&outs[0]), (0, 0, 0));
    assert_eq!(pixel(&outs[3]), (200, 100, 50));
    let mid = pixel(&outs[1]);
    for (g, w) in [(mid.0, 100), (mid.1, 50), (mid.2, 25)] {
        assert!(g.abs_diff(w) <= 1, "淡化中间帧 {g} 偏离期望 {w}");
    }
}

#[test]
fn identity_settings_take_the_one_to_one_path() {
    assert!(plan_for(&Retime::default(), 10, 24.0).unwrap().is_none());
    let on = Retime {
        enabled: true,
        keys: vec![
            SpeedKey {
                at: 0.0,
                speed: 1.0,
            },
            SpeedKey {
                at: 1.0,
                speed: 1.0,
            },
        ],
        shutter: 0.0,
    };
    assert!(plan_for(&on, 10, 24.0).unwrap().is_none());
}

#[test]
fn a_stale_plan_is_rejected_before_any_frame_is_written() {
    let src = TempDir::new("stale-src");
    let out = TempDir::new("stale-out");
    let paths = flat_sequence(&src.path, &[(10, 10, 10), (20, 20, 20), (30, 30, 30)]);
    let (params, gains, offsets) = plain_args(paths.len());
    let cancel = AtomicBool::new(false);
    let options = stills_options(&out.path);

    // 计划按 20 帧的序列算，实际只给了 3 帧：必须当场拒绝，不能越界取帧。
    let plan = plan_for(&Retime::uniform(0.5, 0.0), 20, 24.0)
        .unwrap()
        .unwrap();
    let err = export_stills_planned(
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        Some(&plan),
        &cancel,
        &|_| {},
    )
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("重新算一次计划"), "{text}");
    assert!(
        fs::read_dir(&out.path).unwrap().next().is_none(),
        "拒绝得在读帧之前，输出目录该是空的"
    );
}

#[test]
fn planned_video_export_encodes_the_retimed_frame_count() {
    let Some(ffmpeg) = ffmpeg() else { return };
    let src = TempDir::new("vid-src");
    let work = TempDir::new("vid-out");
    let paths = flat_sequence(
        &src.path,
        &[
            (10, 20, 30),
            (40, 50, 60),
            (70, 80, 90),
            (100, 110, 120),
            (130, 140, 150),
            (160, 170, 180),
            (190, 200, 210),
            (220, 230, 240),
        ],
    );
    let (params, gains, offsets) = plain_args(paths.len());
    let cancel = AtomicBool::new(false);
    let options = RenderOptions {
        mode: OutputMode::Video,
        video_out: work.path.join("retime.mp4"),
        fps: 24.0,
        ..RenderOptions::default()
    };
    let plan = plan_for(&Retime::uniform(2.0, 1.0), paths.len(), 24.0)
        .unwrap()
        .unwrap();
    assert_eq!(plan.frames(), 4);

    export_video_planned(
        &ffmpeg,
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        Some(&plan),
        &cancel,
        &|_| {},
    )
    .unwrap();

    let mut cmd = Command::new(&ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        options.video_out.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "解码成片失败");
    let per_frame = 24 * 16 * 3; // 源图 24×16
    assert_eq!(
        out.stdout.len() / per_frame,
        plan.frames(),
        "成片帧数该等于重映射后的输出帧数"
    );
}
