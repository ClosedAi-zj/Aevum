//! 成片导出端到端：PNG 序列、对齐位移、TIFF16、ffmpeg 编码、取消。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::align::CropRect;
use aevum_core::audio::{AudioOptions, AudioProbe};
use aevum_core::colorspace::ColorSpace;
use aevum_core::export::{
    Codec, ExportError, JobProgress, OutputMode, OutputSharpen, RenderOptions, SharpenStrength,
    Stage, StillFormat, cover_size, encode_video, export_stills, export_video, fit_size,
    pipe_command, probe_audio, video_command,
};
use aevum_core::grade::GradeParams;
use image::{ImageDecoder, Rgb, RgbImage};

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
            "aevum-export-{tag}-{}-{nanos}-{}",
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

fn default_options(out_dir: &Path) -> RenderOptions {
    RenderOptions {
        mode: OutputMode::Stills,
        format: StillFormat::Png8,
        quality: 90,
        keep_exif: false,
        sharpen_for: OutputSharpen::Off,
        sharpen_strength: SharpenStrength::Standard,
        out_dir: out_dir.to_path_buf(),
        video_out: PathBuf::new(),
        crop: None,
        rotate: 0.0,
        long_side: None,
        target_size: None,
        fps: 24.0,
        codec: Codec::H265,
        crf: 18,
        preset: "medium".to_owned(),
        audio: None,
        audio_opts: AudioOptions::default(),
        keep_frames: true,
        color_space: ColorSpace::Srgb,
        ..RenderOptions::default()
    }
}

fn make_sequence(dir: &Path, count: usize) -> Vec<PathBuf> {
    fs::create_dir_all(dir).unwrap();
    let (w, h) = (160u32, 90u32);
    (0..count)
        .map(|i| {
            let mut img = RgbImage::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let r = ((x * 255 / (w - 1) + i as u32 * 9) % 256) as u8;
                    let g = ((y * 255 / (h - 1) + i as u32 * 5) % 256) as u8;
                    let b = ((x * 3 + y * 5 + i as u32 * 11) % 256) as u8;
                    img.put_pixel(x, y, Rgb([r, g, b]));
                }
            }
            let path = dir.join(format!("src_{i:04}.png"));
            img.save(&path).unwrap();
            path
        })
        .collect()
}

fn export(paths: &[PathBuf], options: &RenderOptions) -> Vec<PathBuf> {
    let params = vec![GradeParams::default(); paths.len()];
    let gains = vec![aevum_core::develop::Ev::NONE; paths.len()];
    let offsets = vec![[0.0f64; 2]; paths.len()];
    let cancel = AtomicBool::new(false);
    export_stills(paths, &params, &gains, &offsets, options, &cancel, &|_| {}).unwrap()
}

#[test]
fn png_sequence_export() {
    let td = TempDir::new("png");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 12);

    let frames = td.path.join("frames");
    let options = default_options(&frames);
    let params = vec![GradeParams::default(); 12];
    let gains = vec![aevum_core::develop::Ev::NONE; 12];
    let offsets = vec![[0.0f64; 2]; 12];
    let cancel = AtomicBool::new(false);
    let log = Mutex::new(Vec::<JobProgress>::new());

    let outs = export_stills(&paths, &params, &gains, &offsets, &options, &cancel, &|p| {
        log.lock().unwrap().push(p)
    })
    .unwrap();

    assert_eq!(outs.len(), 12);
    for (i, path) in outs.iter().enumerate() {
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("frame_{i:06}.png")
        );
        let img = image::open(path).unwrap();
        assert_eq!((img.width(), img.height()), (160, 90));
    }

    let log = log.into_inner().unwrap();
    assert_eq!(log.first().unwrap().stage, Stage::Frames);
    assert_eq!(log.first().unwrap().done, 0);
    assert_eq!(log.first().unwrap().total, 12);
    assert!(
        log.iter()
            .all(|p| p.stage == Stage::Frames && p.total == 12)
    );
    assert!(log.windows(2).all(|w| w[1].done >= w[0].done));
    assert_eq!(log.last().unwrap().done, 12);

    // 长边缩放：160x90 → 长边 80，偶数高度取 44
    let small = td.path.join("small");
    let mut opt_small = default_options(&small);
    opt_small.long_side = Some(80);
    let outs = export(&paths[..1], &opt_small);
    let img = image::open(&outs[0]).unwrap();
    assert_eq!((img.width(), img.height()), (80, 44));

    // 奇数裁切框也要落成偶数尺寸
    let odd = td.path.join("odd");
    let mut opt_odd = default_options(&odd);
    opt_odd.crop = Some(CropRect {
        x: 1,
        y: 1,
        w: 99,
        h: 51,
    });
    let outs = export(&paths[..1], &opt_odd);
    let img = image::open(&outs[0]).unwrap();
    assert_eq!((img.width(), img.height()), (98, 50));
}

#[test]
fn offset_shifts_pixels() {
    let td = TempDir::new("offset");
    let src = td.path.join("src");
    fs::create_dir_all(&src).unwrap();
    let mut img = RgbImage::from_pixel(64, 48, Rgb([0, 0, 0]));
    for y in 0..48 {
        img.put_pixel(20, y, Rgb([255, 0, 0]));
    }
    let path = src.join("one.png");
    img.save(&path).unwrap();

    let out = td.path.join("frames");
    let options = default_options(&out);
    let cancel = AtomicBool::new(false);
    let params = [GradeParams::default()];
    let gains = [aevum_core::develop::Ev::NONE];
    // dst(x) = src(x - 10)：源图 x=20 的红线落到目标 x=30
    let offsets = [[10.0f64, 0.0]];
    let outs = export_stills(
        &[path],
        &params,
        &gains,
        &offsets,
        &options,
        &cancel,
        &|_| {},
    )
    .unwrap();

    let got = image::open(&outs[0]).unwrap().to_rgb8();
    assert_eq!(got.get_pixel(30, 24).0, [255, 0, 0]);
    assert!(got.get_pixel(20, 24).0[0] < 10, "x=20 应采样到黑底");
}

#[test]
fn tiff16_reads_back() {
    let td = TempDir::new("tiff");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 1);
    let out = td.path.join("frames");
    let mut options = default_options(&out);
    options.format = StillFormat::Tiff16;

    let outs = export(&paths, &options);
    assert_eq!(outs[0].extension().unwrap(), "tif");

    let expected = image::open(&paths[0]).unwrap().to_rgb8();
    let pixel = expected.get_pixel(32, 24).0;
    let img = image::open(&outs[0]).unwrap();
    let got = img.to_rgb16();
    assert_eq!((got.width(), got.height()), (160, 90));

    for c in 0..3 {
        let want = pixel[c] as u16 * 257;
        let have = got.get_pixel(32, 24).0[c];
        assert!(
            have.abs_diff(want) <= 1,
            "通道 {c}: 期望 {want}，实得 {have}（8bit 源 {}）",
            pixel[c]
        );
    }
    assert!(got.pixels().any(|p| p.0.iter().any(|v| *v > 255)));
}

#[test]
fn deep_source_survives_decode() {
    use aevum_core::metrics;
    use image::ImageBuffer;

    let td = TempDir::new("deep");
    let src = td.path.join("deep.png");
    // 两个像素只差 10/65535，落在同一个 8bit 桶里：解成 8bit 就并成一个，
    // 12-bit 相机的高光渐变正是死在这一步。
    let mut img: ImageBuffer<Rgb<u16>, Vec<u16>> =
        ImageBuffer::from_pixel(2, 1, Rgb([1000, 20000, 40000]));
    img.get_pixel_mut(1, 0).0[0] = 1010;
    img.save(&src).unwrap();

    let (rgb, w, h, deep) = metrics::decode_rgb_f32(&src).unwrap();
    assert_eq!((w, h), (2, 1));
    assert!(deep, "16bit 源要报出高位深，导出才知道该写 16bit");
    let gap = rgb[3] - rgb[0];
    assert!(
        (gap - 10.0 / 65535.0).abs() < 1e-6,
        "两个相邻 16bit 级被并掉了，间距只有 {gap}"
    );

    // 同一条源走老的 8bit 解码分不出来——这条是给上面那行定标尺的。
    let flat = metrics::decode_rgb(&src, 32).unwrap();
    assert_eq!(
        flat.get_pixel(0, 0).0[0],
        flat.get_pixel(1, 0).0[0],
        "8bit 口径本该并级，否则新函数就没在解决问题"
    );

    // 8bit 源仍报 shallow，不能被新路径顺手升位深、把文件白翻倍。
    let td8 = TempDir::new("shallow");
    let p8 = td8.path.join("flat.png");
    RgbImage::from_pixel(2, 1, Rgb([4, 78, 156]))
        .save(&p8)
        .unwrap();
    let (_, _, _, deep8) = metrics::decode_rgb_f32(&p8).unwrap();
    assert!(!deep8, "8bit 源不该判成高位深");
}

fn pair(args: &[String], key: &str, value: &str) -> bool {
    args.windows(2).any(|w| w[0] == key && w[1] == value)
}

#[test]
fn video_command_builds() {
    let td = TempDir::new("cmd");
    let frames = td.path.join("frames");
    let video = td.path.join("out.mp4");
    let audio = td.path.join("a.wav");

    let mut options = default_options(&frames);
    options.video_out = video.clone();
    options.fps = 12.0;
    options.crf = 20;
    options.preset = "fast".to_owned();
    options.audio = Some(audio.clone());

    // 配乐是 WAV（pcm）：mp4 放不了，自动模式转 AAC；trim 打开时按画面长度 -t。
    let probe = AudioProbe {
        codec: "pcm_s16le".to_owned(),
        duration: 2.0,
    };
    let args = video_command(Path::new("ffmpeg"), &options, 12, Some(&probe));
    assert_eq!(args[0], "-y");
    assert!(pair(&args, "-framerate", "12"));
    assert!(pair(&args, "-start_number", "0"));
    assert!(pair(&args, "-progress", "pipe:1"));
    assert!(args.iter().any(|a| a == "-nostats"));
    assert!(pair(&args, "-c:v", "libx265"));
    assert!(pair(&args, "-pix_fmt", "yuv420p10le"));
    assert!(pair(&args, "-crf", "20"));
    assert!(pair(&args, "-preset", "fast"));
    assert!(pair(&args, "-movflags", "+faststart"));
    assert!(pair(&args, "-colorspace", "bt709"));
    assert!(pair(&args, "-color_primaries", "bt709"));
    assert!(pair(&args, "-color_trc", "bt709"));
    assert!(args.windows(2).any(|w| {
        w[0] == "-x265-params"
            && w[1] == "colorprim=bt709:transfer=bt709:colormatrix=bt709:range=limited"
    }));
    // 输入 pattern
    let i = args.iter().position(|a| a == "-i").unwrap();
    assert!(
        args[i + 1]
            .replace('\\', "/")
            .ends_with("frames/frame_%06d.png")
    );
    // 音频：第二路输入 + 映射 + AAC
    let audio_i = args.iter().skip(i + 1).position(|a| a == "-i").unwrap() + i + 1;
    assert_eq!(args[audio_i + 1], audio.to_string_lossy());
    assert!(pair(&args, "-map", "0:v:0"));
    assert!(pair(&args, "-map", "1:a:0"));
    assert!(pair(&args, "-c:a", "aac"));
    assert!(pair(&args, "-b:a", "192k"));
    assert!(pair(&args, "-t", "1.000"));
    assert!(!args.iter().any(|a| a == "-shortest"));
    assert!(!args.iter().any(|a| a == "-filter_complex"));
    assert_eq!(args.last().unwrap(), &video.to_string_lossy());

    // AAC 源且不滤镜：自动模式原样复制，不重编。
    let aac = AudioProbe {
        codec: "aac".to_owned(),
        duration: 2.0,
    };
    let args = video_command(Path::new("ffmpeg"), &options, 12, Some(&aac));
    assert!(pair(&args, "-c:a", "copy"));
    assert!(!args.iter().any(|a| a == "-b:a"));

    // 延迟 + 音量 + 淡入淡出：走 filter_complex，音频必重编。
    // 淡出落在配乐收尾处（min(0.5+2, 1) - 0.4 = 0.6）。
    options.audio_opts = AudioOptions {
        delay: 0.5,
        gain: 80.0,
        fade_in: 0.2,
        fade_out: 0.4,
        ..Default::default()
    };
    let args = video_command(Path::new("ffmpeg"), &options, 12, Some(&aac));
    assert!(pair(&args, "-c:a", "aac"));
    assert!(pair(&args, "-b:a", "192k"));
    let f = args
        .iter()
        .position(|a| a == "-filter_complex")
        .expect("需要滤镜");
    assert_eq!(
        args[f + 1],
        "[1:a]adelay=500:all=1,volume=0.8000,afade=t=in:st=0.500:d=0.200,afade=t=out:st=0.600:d=0.400[mu]"
    );
    assert!(pair(&args, "-map", "[mu]"));
    assert!(!pair(&args, "-map", "1:a:0"));

    // 循环：输入前加 -stream_loop，复制就能铺满，-t 收在画面末尾。
    options.audio_opts = AudioOptions {
        looped: true,
        ..Default::default()
    };
    let short = AudioProbe {
        codec: "mp3".to_owned(),
        duration: 0.5,
    };
    let args = video_command(Path::new("ffmpeg"), &options, 12, Some(&short));
    let second_i = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "-i")
        .nth(1)
        .map(|(i, _)| i)
        .expect("第二路输入");
    assert_eq!(args[second_i - 2], "-stream_loop");
    assert_eq!(args[second_i - 1], "-1");
    assert!(pair(&args, "-c:a", "copy"));
    assert!(pair(&args, "-t", "1.000"));

    // H264：8bit、不写 x265 私有参数；没有配乐就不加音频参数。
    let mut h264 = options.clone();
    h264.codec = Codec::H264;
    h264.audio = None;
    h264.video_out = td.path.join("out264.mp4");
    let args = video_command(Path::new("ffmpeg"), &h264, 12, None);
    assert!(pair(&args, "-c:v", "libx264"));
    assert!(pair(&args, "-pix_fmt", "yuv420p"));
    assert!(!args.iter().any(|a| a == "-x265-params"));
    assert!(!args.iter().any(|a| a == "-stream_loop"));
    assert!(!args.iter().any(|a| a == "-c:a"));
    assert!(args.last().unwrap().ends_with(".mp4"));
}

#[test]
fn video_command_carries_metadata() {
    let td = TempDir::new("meta");
    let mut options = default_options(&td.path.join("frames"));
    options.video_out = td.path.join("out.mp4");
    assert!(
        !args_of(&options).iter().any(|a| a == "-metadata"),
        "没填元数据不该写标签"
    );

    options.metadata = aevum_core::project::Metadata {
        title: "云海日出".to_owned(),
        description: "第一场\n日出".to_owned(),
        creator: "张三".to_owned(),
        copyright: "© 2026 张三".to_owned(),
    };
    let args = args_of(&options);
    assert!(pair(&args, "-metadata", "title=云海日出"));
    assert!(
        pair(&args, "-metadata", "comment=第一场 日出"),
        "换行该折成空格"
    );
    assert!(pair(&args, "-metadata", "artist=张三"));
    assert!(pair(&args, "-metadata", "copyright=© 2026 张三"));
    // 标签是输出文件级的参数，必须排在输出路径前面。
    let at = args.iter().position(|a| a == "-metadata").unwrap();
    let out = args.iter().position(|a| a.ends_with("out.mp4")).unwrap();
    assert!(at < out, "-metadata 跑到输出路径后面了");

    // 只填一项：其余三项不写空标签。
    let mut one = default_options(&td.path.join("frames2"));
    one.video_out = td.path.join("one.mp4");
    one.metadata.copyright = "© 2026".to_owned();
    let args = args_of(&one);
    assert_eq!(args.iter().filter(|a| *a == "-metadata").count(), 1);
    assert!(pair(&args, "-metadata", "copyright=© 2026"));
}

fn args_of(options: &RenderOptions) -> Vec<String> {
    video_command(Path::new("ffmpeg"), options, 12, None)
}

#[test]
fn video_encode_h265() {
    let Some(ffmpeg) = ffmpeg() else {
        return;
    };
    let td = TempDir::new("h265");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 12);
    let frames = td.path.join("frames");
    let options = default_options(&frames);
    export(&paths, &options);

    let video = td.path.join("out.mp4");
    let mut encode = options.clone();
    encode.mode = OutputMode::Video;
    encode.video_out = video.clone();
    encode.fps = 12.0;
    encode.crf = 20;
    encode.preset = "ultrafast".to_owned();

    let cancel = AtomicBool::new(false);
    let log = Mutex::new(Vec::<JobProgress>::new());
    let out = encode_video(&ffmpeg, &encode, &cancel, &|p| log.lock().unwrap().push(p)).unwrap();
    assert_eq!(out, video);
    let size = fs::metadata(&video).unwrap().len();
    assert!(size > 2048, "成片只有 {size} 字节");

    let log = log.into_inner().unwrap();
    assert!(
        log.iter()
            .all(|p| p.stage == Stage::Encode && p.total == 12)
    );
    assert_eq!(log.last().unwrap().done, 12);
    assert!(log.windows(2).all(|w| w[1].done >= w[0].done));

    // 解回首帧核对宽高（不依赖 ffprobe）
    let first = td.path.join("first.png");
    let mut cmd = Command::new(&ffmpeg);
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&video)
        .arg("-frames:v")
        .arg("1")
        .arg(&first);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    assert!(cmd.status().unwrap().success());
    let img = image::open(&first).unwrap();
    assert_eq!((img.width(), img.height()), (160, 90));

    // keep_frames=false 时成片成功后清掉中间帧
    let mut clean = encode.clone();
    clean.video_out = td.path.join("clean.mp4");
    clean.keep_frames = false;
    encode_video(&ffmpeg, &clean, &cancel, &|_| {}).unwrap();
    assert_eq!(fs::read_dir(&frames).unwrap().count(), 0);
}

#[test]
fn audio_is_probed_looped_and_muxed() {
    let Some(ffmpeg) = ffmpeg() else {
        return;
    };
    let td = TempDir::new("audio");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 12);
    let frames = td.path.join("frames");
    let options = default_options(&frames);
    export(&paths, &options);

    // 半秒正弦波：比 1 秒画面短，正好验证循环铺满与淡出落点。
    let music = td.path.join("music.wav");
    let mut cmd = Command::new(&ffmpeg);
    cmd.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=0.5",
        "-c:a",
        "pcm_s16le",
    ])
    .arg(&music);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    assert!(cmd.status().unwrap().success());

    let probe = probe_audio(&ffmpeg, &music).unwrap();
    assert!(probe.codec.starts_with("pcm"), "编码 {}", probe.codec);
    assert!(
        (probe.duration - 0.5).abs() < 0.1,
        "探针时长 {}",
        probe.duration
    );

    let video = td.path.join("out.mp4");
    let mut encode = options.clone();
    encode.mode = OutputMode::Video;
    encode.codec = Codec::H264;
    encode.preset = "ultrafast".to_owned();
    encode.crf = 28;
    encode.fps = 12.0;
    encode.video_out = video.clone();
    encode.audio = Some(music);
    encode.audio_opts = AudioOptions {
        looped: true,
        gain: 50.0,
        fade_in: 0.2,
        fade_out: 0.2,
        ..Default::default()
    };

    let cancel = AtomicBool::new(false);
    encode_video(&ffmpeg, &encode, &cancel, &|_| {}).unwrap();

    // 成片里必须有音频流，且长度收在画面末尾（1.0s）附近。
    let out = probe_audio(&ffmpeg, &video).unwrap();
    assert!(out.codec.starts_with("aac"), "成片音频编码 {}", out.codec);
    assert!(
        (out.duration - 1.0).abs() < 0.15,
        "成片时长 {}",
        out.duration
    );
}

#[test]
fn probe_audio_rejects_missing_and_silent_files() {
    let Some(ffmpeg) = ffmpeg() else {
        return;
    };
    let td = TempDir::new("probe");
    assert!(matches!(
        probe_audio(&ffmpeg, &td.path.join("missing.wav")),
        Err(ExportError::Invalid(_))
    ));

    // PNG 没有音频流：要在渲染前就报清楚，而不是交给 ffmpeg 编码时才失败。
    let img = td.path.join("frame.png");
    RgbImage::from_pixel(4, 4, Rgb([1, 2, 3]))
        .save(&img)
        .unwrap();
    assert!(matches!(
        probe_audio(&ffmpeg, &img),
        Err(ExportError::Invalid(_))
    ));
}

#[test]
fn cancel_before_start() {
    let td = TempDir::new("cancel");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 3);
    let options = default_options(&td.path.join("frames"));
    let params = vec![GradeParams::default(); 3];
    let gains = vec![aevum_core::develop::Ev::NONE; 3];
    let offsets = vec![[0.0f64; 2]; 3];
    let cancel = AtomicBool::new(true);
    let res = export_stills(
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        &cancel,
        &|_| {},
    );
    assert!(matches!(res, Err(ExportError::Cancelled)));

    let encode = RenderOptions {
        video_out: td.path.join("out.mp4"),
        ..options.clone()
    };
    let res = encode_video(Path::new("ffmpeg"), &encode, &cancel, &|_| {});
    assert!(matches!(res, Err(ExportError::Cancelled)));
}

#[test]
fn mismatched_arrays_are_invalid() {
    let td = TempDir::new("invalid");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 2);
    let options = default_options(&td.path.join("frames"));
    let params = vec![GradeParams::default(); 1];
    let gains = vec![aevum_core::develop::Ev::NONE; 2];
    let offsets = vec![[0.0f64; 2]; 2];
    let cancel = AtomicBool::new(false);
    let res = export_stills(
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        &cancel,
        &|_| {},
    );
    assert!(matches!(res, Err(ExportError::Invalid(_))));
}

#[test]
fn fit_size_is_even_and_limits_long_side() {
    assert_eq!(fit_size(1920, 1080, Some(3840)), (1920, 1080));
    assert_eq!(fit_size(1919, 1081, None), (1918, 1080));
    assert_eq!(fit_size(4000, 3000, Some(1000)), (1000, 750));
    assert_eq!(fit_size(160, 90, Some(80)), (80, 44));
    assert_eq!(fit_size(2, 3, None), (2, 2));
    assert_eq!(fit_size(1, 1, None), (2, 2));
}

#[test]
fn cover_size_scales_up_to_exact_target() {
    // 同比例：正好命中，不需要裁。
    assert_eq!(cover_size(1280, 720, 3840, 2160), (3840, 2160));
    // 对齐裁切后的 1278×718 略有偏差：宽度多出 5 像素，高度正好。
    assert_eq!(cover_size(1278, 718, 3840, 2160), (3845, 2160));
    // 小图放大到 1080p。
    assert_eq!(cover_size(160, 90, 1920, 1080), (1920, 1080));
}

#[test]
fn exact_target_size_exports_cover_and_crop() {
    let td = TempDir::new("target");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 1);

    // 4:3 画幅塞进 1:1 目标：等比放大到 178×100 后居中裁掉左右。
    let square = td.path.join("square");
    let mut opt = default_options(&square);
    opt.target_size = Some((100, 100));
    let outs = export(&paths, &opt);
    let img = image::open(&outs[0]).unwrap();
    assert_eq!((img.width(), img.height()), (100, 100));

    // 目标小于源图也要精确命中，而不是按长边留原比例。
    let tight = td.path.join("tight");
    let mut opt = default_options(&tight);
    opt.target_size = Some((80, 44));
    let outs = export(&paths, &opt);
    let img = image::open(&outs[0]).unwrap();
    assert_eq!((img.width(), img.height()), (80, 44));

    // 奇数或过小的目标尺寸在写任何文件前就被拒绝。
    let bad = td.path.join("bad");
    let mut opt = default_options(&bad);
    opt.target_size = Some((99, 100));
    let params = [GradeParams::default()];
    let gains = [aevum_core::develop::Ev::NONE];
    let offsets = [[0.0f64, 0.0]];
    let cancel = AtomicBool::new(false);
    let res = export_stills(&paths, &params, &gains, &offsets, &opt, &cancel, &|_| {});
    assert!(matches!(res, Err(ExportError::Invalid(_))));
    assert!(!bad.exists(), "校验失败时不应创建输出目录");
}

/// 读回静态图里嵌的 ICC；没嵌返回 None。
fn embedded_icc(path: &Path) -> Option<Vec<u8>> {
    let mut decoder = image::ImageReader::open(path)
        .unwrap()
        .into_decoder()
        .unwrap();
    decoder.icc_profile().unwrap()
}

fn pure_red_source(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("src_0000.png");
    RgbImage::from_pixel(8, 8, Rgb([255, 0, 0]))
        .save(&path)
        .unwrap();
    path
}

#[test]
fn wide_gamut_still_embeds_profile_and_moves_pixels() {
    // 标记与像素必须一起走：单测里核过的矩阵，到这一步要能在真文件上读回来。
    let td = TempDir::new("icc");
    let src = td.path.join("src");
    let paths = vec![pure_red_source(&src)];

    for space in ColorSpace::ALL {
        let out = td.path.join(format!("out_{}", space.key()));
        let mut opt = default_options(&out);
        opt.color_space = space;
        let outs = export(&paths, &opt);

        let want = if space == ColorSpace::Srgb {
            None
        } else {
            Some(space.icc())
        };
        assert_eq!(
            embedded_icc(&outs[0]).as_deref(),
            want.as_deref(),
            "{space:?}"
        );

        // littleCMS 实测的 sRGB 纯红 → 目标色域编码值；sRGB 那条是不变。
        let px = image::open(&outs[0]).unwrap().to_rgb8().get_pixel(0, 0).0;
        match space {
            ColorSpace::Srgb => assert_eq!(px, [255, 0, 0]),
            ColorSpace::DisplayP3 => {
                assert!(
                    px.iter()
                        .zip([234u8, 51, 35])
                        .all(|(g, w)| g.abs_diff(w) <= 1),
                    "P3 红不对：{px:?}"
                );
            }
            ColorSpace::Rec2020 => {
                assert!(
                    px[0] > 200 && px[1] < 90 && px[2] < 60,
                    "2020 红跑偏：{px:?}"
                );
            }
        }
    }
}

#[test]
fn neutral_stays_neutral_through_the_still_path() {
    let td = TempDir::new("neutral");
    let src = td.path.join("src");
    fs::create_dir_all(&src).unwrap();
    let path = src.join("src_0000.png");
    RgbImage::from_pixel(8, 8, Rgb([120, 120, 120]))
        .save(&path)
        .unwrap();
    let paths = vec![path];

    for space in [ColorSpace::DisplayP3, ColorSpace::Rec2020] {
        let mut opt = default_options(&td.path.join(format!("out_{}", space.key())));
        opt.color_space = space;
        let outs = export(&paths, &opt);
        let px = image::open(&outs[0]).unwrap().to_rgb8().get_pixel(0, 0).0;
        assert!(
            px[0].abs_diff(px[1]) <= 1 && px[1].abs_diff(px[2]) <= 1,
            "{space:?} 把灰推成彩色了：{px:?}"
        );
    }
}

fn flag_pair<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str())
}

#[test]
fn color_space_drives_the_vui_flags() {
    let td = TempDir::new("vui");
    let mut opt = default_options(&td.path.join("frames"));
    opt.mode = OutputMode::Video;
    opt.video_out = td.path.join("out.mp4");

    opt.color_space = ColorSpace::DisplayP3;
    opt.codec = Codec::H264;
    let args = video_command(Path::new("ffmpeg"), &opt, 4, None);
    assert_eq!(
        flag_pair(&args, "-x264-params"),
        Some("colorprim=smpte432:transfer=iec61966-2-1:colormatrix=bt709:range=limited")
    );
    assert_eq!(flag_pair(&args, "-color_primaries"), Some("smpte432"));
    assert_eq!(flag_pair(&args, "-color_trc"), Some("iec61966-2-1"));
    assert_eq!(flag_pair(&args, "-colorspace"), Some("bt709"));
    assert!(flag_pair(&args, "-x265-params").is_none());

    // H.264 那条以前完全不写colour参数：现在两条编码路都得带。
    opt.codec = Codec::H265;
    opt.color_space = ColorSpace::Rec2020;
    let args = video_command(Path::new("ffmpeg"), &opt, 4, None);
    assert_eq!(
        flag_pair(&args, "-x265-params"),
        Some("colorprim=bt2020:transfer=bt709:colormatrix=bt2020nc:range=limited")
    );
    assert_eq!(flag_pair(&args, "-color_primaries"), Some("bt2020"));
    assert!(flag_pair(&args, "-x264-params").is_none());
}

#[test]
fn wide_gamut_video_vui_survives_the_encode() {
    // 只有真编码器说了算：全局 -color_* 参数会被 x26? 忽略，必须靠 params 串落进 VUI。
    let Some(ffmpeg) = ffmpeg() else {
        return;
    };
    let td = TempDir::new("vui-live");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 4);
    let frames = td.path.join("frames");
    let mut opt = default_options(&frames);
    opt.color_space = ColorSpace::DisplayP3;
    export(&paths, &opt);

    opt.mode = OutputMode::Video;
    opt.codec = Codec::H264;
    opt.fps = 12.0;
    opt.crf = 28;
    opt.preset = "ultrafast".to_owned();
    opt.keep_frames = true;
    opt.video_out = td.path.join("p3.mp4");
    let cancel = AtomicBool::new(false);
    encode_video(&ffmpeg, &opt, &cancel, &|_| {}).unwrap();

    let mut cmd = Command::new(&ffmpeg);
    cmd.arg("-hide_banner").arg("-i").arg(&opt.video_out);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    let info = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&info.stderr).to_lowercase();
    assert!(
        text.contains("smpte432") && text.contains("iec61966-2-1"),
        "成片 VUI 里没有 P3 标记：{text}"
    );
}

fn decode_frames(ffmpeg: &Path, video: &Path) -> Vec<u8> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(video)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"]);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "解帧失败：{}", video.display());
    out.stdout
}

#[test]
fn pipe_command_reads_raw_frames_from_stdin() {
    let td = TempDir::new("pipe-args");
    let mut opt = default_options(&td.path.join("frames"));
    opt.mode = OutputMode::Video;
    opt.fps = 12.0;
    opt.video_out = td.path.join("out.mp4");
    let args = pipe_command(&opt, 6, None, (160, 90));
    assert!(pair(&args, "-f", "rawvideo"));
    assert!(pair(&args, "-pix_fmt", "rgb24"));
    assert!(pair(&args, "-s", "160x90"));
    assert!(pair(&args, "-framerate", "12"));
    // 输入就是标准输入，且不再有 image2 的起始序号
    let i = args.iter().position(|a| a == "-i").unwrap();
    assert_eq!(args[i + 1], "-");
    assert!(!args.iter().any(|a| a == "-start_number"));
    assert!(pair(&args, "-frames:v", "6"));
}

#[test]
fn direct_pipeline_matches_frame_route() {
    let Some(ffmpeg) = ffmpeg() else {
        return;
    };
    let td = TempDir::new("direct");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 6);
    let params = vec![GradeParams::default(); 6];
    let gains = vec![aevum_core::develop::Ev::NONE; 6];
    let offsets = vec![[0.0f64; 2]; 6];

    let mut direct = default_options(&td.path.join("frames"));
    direct.mode = OutputMode::Video;
    direct.codec = Codec::H264;
    direct.fps = 12.0;
    direct.crf = 18;
    direct.preset = "ultrafast".to_owned();
    direct.long_side = Some(80);
    direct.keep_frames = false;
    direct.video_out = td.path.join("direct.mp4");

    let mut legacy = direct.clone();
    legacy.out_dir = td.path.join("legacy_frames");
    legacy.video_out = td.path.join("legacy.mp4");
    legacy.keep_frames = true;

    let cancel = AtomicBool::new(false);
    let log = Mutex::new(Vec::<JobProgress>::new());
    let out = export_video(
        &ffmpeg,
        &paths,
        &params,
        &gains,
        &offsets,
        &direct,
        &cancel,
        &|p| log.lock().unwrap().push(p),
    )
    .unwrap();
    assert_eq!(out, direct.video_out);
    export_video(
        &ffmpeg,
        &paths,
        &params,
        &gains,
        &offsets,
        &legacy,
        &cancel,
        &|_| {},
    )
    .unwrap();

    // 两条路喂给编码器的像素相同，解回来逐字节只该有编码本身的抖动。
    let a = decode_frames(&ffmpeg, &direct.video_out);
    let b = decode_frames(&ffmpeg, &legacy.video_out);
    let (fw, fh) = (80usize, 44usize);
    assert_eq!(a.len(), 6 * fw * fh * 3, "直连成片帧数/尺寸不对");
    assert_eq!(a.len(), b.len());
    let max = a
        .iter()
        .zip(&b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0);
    assert!(max <= 2, "直连与留帧两条路差到 {max}");

    // 直连不留中间帧：目录根本没建。
    assert!(!direct.out_dir.exists(), "直连不该写 PNG 序列");
    let log = log.into_inner().unwrap();
    assert_eq!(log.first().unwrap().stage, Stage::Frames);
    assert_eq!(log.last().unwrap().stage, Stage::Encode);
    assert_eq!(log.last().unwrap().done, 6);
    assert!(log.windows(2).all(|w| w[1].done >= w[0].done));
}

#[test]
fn direct_pipeline_muxes_audio() {
    let Some(ffmpeg) = ffmpeg() else {
        return;
    };
    let td = TempDir::new("direct-audio");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 12);

    let music = td.path.join("music.wav");
    let mut cmd = Command::new(&ffmpeg);
    cmd.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=0.5",
        "-c:a",
        "pcm_s16le",
    ])
    .arg(&music);
    #[cfg(windows)]
    cmd.creation_flags(NO_WINDOW);
    assert!(cmd.status().unwrap().success());

    let mut opt = default_options(&td.path.join("frames"));
    opt.mode = OutputMode::Video;
    opt.codec = Codec::H264;
    opt.fps = 12.0;
    opt.crf = 28;
    opt.preset = "ultrafast".to_owned();
    opt.audio = Some(music);
    opt.audio_opts = AudioOptions {
        looped: true,
        ..Default::default()
    };
    opt.video_out = td.path.join("out.mp4");

    let params = vec![GradeParams::default(); 12];
    let gains = vec![aevum_core::develop::Ev::NONE; 12];
    let offsets = vec![[0.0f64; 2]; 12];
    let cancel = AtomicBool::new(false);
    export_video(
        &ffmpeg,
        &paths,
        &params,
        &gains,
        &offsets,
        &opt,
        &cancel,
        &|_| {},
    )
    .unwrap();

    let probe = probe_audio(&ffmpeg, &opt.video_out).unwrap();
    assert!(
        probe.codec.starts_with("aac"),
        "成片音频编码 {}",
        probe.codec
    );
    assert!(
        (probe.duration - 1.0).abs() < 0.15,
        "成片时长 {}",
        probe.duration
    );
}

#[test]
fn direct_pipeline_cancels_before_start() {
    let Some(ffmpeg) = ffmpeg() else {
        return;
    };
    let td = TempDir::new("direct-cancel");
    let src = td.path.join("src");
    let paths = make_sequence(&src, 3);
    let mut opt = default_options(&td.path.join("frames"));
    opt.mode = OutputMode::Video;
    opt.video_out = td.path.join("out.mp4");
    let params = vec![GradeParams::default(); 3];
    let gains = vec![aevum_core::develop::Ev::NONE; 3];
    let offsets = vec![[0.0f64; 2]; 3];
    let cancel = AtomicBool::new(true);
    let res = export_video(
        &ffmpeg,
        &paths,
        &params,
        &gains,
        &offsets,
        &opt,
        &cancel,
        &|_| {},
    );
    assert!(matches!(res, Err(ExportError::Cancelled)));
}
