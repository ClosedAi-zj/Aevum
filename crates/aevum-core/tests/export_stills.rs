//! 静帧容器：JPEG 质量、WebP 无损、输出锐化、EXIF 搬运与水印，外加断点续跑。
//!
//! 视频那条路在 `export_video.rs`；这里管「导出成一串图片」的部分，外加和它共用
//! 同一条尾巴的单帧落盘口 `write_single_still`。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::develop::Ev;
use aevum_core::export::{
    ExportError, OutputMode, OutputSharpen, RenderOptions, SharpenStrength, StillFormat,
    existing_frames, export_stills, export_video, frame_is_done, frame_path, write_single_still,
};
use aevum_core::grade::GradeParams;
use aevum_core::{exif, project, xmp};
use image::{ImageEncoder, Rgb, RgbImage};

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
            "aevum-stills-{tag}-{}-{nanos}-{}",
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
        ..RenderOptions::default()
    }
}

fn export(paths: &[PathBuf], options: &RenderOptions) -> Vec<PathBuf> {
    let params = vec![GradeParams::default(); paths.len()];
    let gains = vec![Ev::NONE; paths.len()];
    let offsets = vec![[0.0f64; 2]; paths.len()];
    let cancel = AtomicBool::new(false);
    export_stills(paths, &params, &gains, &offsets, options, &cancel, &|_| {}).unwrap()
}

/// 渐变源序列：内容随帧号变，用来对比不同质量档的产物大小。
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

/// 硬边缘源：左半 20、右半 230，用来验输出锐化只在边界附近动手。
fn edge_source(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let (w, h) = (160u32, 90u32);
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = if x < w / 2 { 20 } else { 230 };
            img.put_pixel(x, y, Rgb([v, v, v]));
        }
    }
    let path = dir.join("edge.png");
    img.save(&path).unwrap();
    path
}

/// 一个结构合法的迷你 EXIF 块（小端 TIFF：IFD0 一条 Make 指向 "LUMEN"）。
/// `set_exif_metadata` 收的就是去掉 `Exif\0\0` 头之后的那种字节，跟容器无关。
fn aevum_exif_block() -> Vec<u8> {
    let mut b: Vec<u8> = Vec::new();
    b.extend_from_slice(b"II");
    b.extend_from_slice(&42u16.to_le_bytes());
    b.extend_from_slice(&8u32.to_le_bytes()); // IFD0 紧跟头部
    b.extend_from_slice(&1u16.to_le_bytes()); // 一条条目
    b.extend_from_slice(&0x010Fu16.to_le_bytes()); // Make
    b.extend_from_slice(&2u16.to_le_bytes()); // ASCII
    b.extend_from_slice(&6u32.to_le_bytes()); // "LUMEN\0"
    b.extend_from_slice(&26u32.to_le_bytes()); // 值在末尾
    b.extend_from_slice(&0u32.to_le_bytes()); // 没有 IFD1
    b.extend_from_slice(b"LUMEN\0");
    b
}

/// 带 EXIF 的源 JPEG：让编码器自己写 APP1，测试不手工拼 JPEG 段。
fn jpeg_source_with_exif(dir: &Path, block: &[u8]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("src_exif.jpg");
    let img = RgbImage::from_pixel(32, 24, Rgb([120, 60, 200]));
    let mut file = fs::File::create(&path).unwrap();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 95);
    enc.set_exif_metadata(block.to_vec()).unwrap();
    enc.write_image(img.as_raw(), 32, 24, image::ExtendedColorType::Rgb8)
        .unwrap();
    path
}

#[test]
fn jpeg_sequence_export_honours_quality() {
    let td = TempDir::new("jpeg");
    let paths = make_sequence(&td.path.join("src"), 3);

    let mut opt = default_options(&td.path.join("hi"));
    opt.format = StillFormat::Jpeg;
    opt.quality = 92;
    let outs = export(&paths, &opt);
    assert_eq!(outs.len(), 3);
    for (i, p) in outs.iter().enumerate() {
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            format!("frame_{i:06}.jpg")
        );
        let bytes = fs::read(p).unwrap();
        assert_eq!(&bytes[..2], b"\xFF\xD8", "{p:?} 不是 JPEG");
        let img = image::open(p).unwrap();
        assert_eq!((img.width(), img.height()), (160, 90));
    }

    // 质量确实接到了编码器上：同尺寸同内容，低质量那份必须更小。
    let mut lo_opt = opt.clone();
    lo_opt.out_dir = td.path.join("lo");
    lo_opt.quality = 25;
    let lo = export(&paths, &lo_opt);
    let hi_len = fs::metadata(&outs[0]).unwrap().len();
    let lo_len = fs::metadata(&lo[0]).unwrap().len();
    assert!(lo_len < hi_len, "质量 25 反而更大：{lo_len} vs {hi_len}");
}

#[test]
fn webp_export_matches_the_png_bit_for_bit() {
    let td = TempDir::new("webp");
    let paths = make_sequence(&td.path.join("src"), 1);

    let png = export(&paths, &default_options(&td.path.join("png")));
    let mut opt = default_options(&td.path.join("webp"));
    opt.format = StillFormat::WebP;
    let webp = export(&paths, &opt);

    assert_eq!(
        webp[0].file_name().unwrap().to_string_lossy(),
        "frame_000000.webp"
    );
    // VP8L 是无损档：宣称无损就得和 PNG 那条路逐位一样。
    let a = image::open(&png[0]).unwrap().to_rgb8();
    let b = image::open(&webp[0]).unwrap().to_rgb8();
    assert_eq!(a.as_raw(), b.as_raw());
}

/// AVIF 走外部 ffmpeg：没装 ffmpeg 就跳过（CI 同款口径）。质量越高文件越大、
/// 容器头是 avif brand，临时 PNG 不能留在输出目录，色标要按输出色域写进 nclx。
#[test]
fn avif_export_goes_through_ffmpeg_and_cleans_up() {
    if aevum_core::export::find_ffmpeg().is_none() {
        return;
    }
    let td = TempDir::new("avif");
    let paths = make_sequence(&td.path.join("src"), 1);
    let mut sizes = Vec::new();
    for quality in [30u8, 90] {
        let out = td.path.join(format!("q{quality}"));
        let mut opt = default_options(&out);
        opt.format = StillFormat::Avif;
        opt.quality = quality;
        let files = export(&paths, &opt);
        assert_eq!(
            files[0].file_name().unwrap().to_string_lossy(),
            "frame_000000.avif"
        );
        let bytes = fs::read(&files[0]).unwrap();
        assert_eq!(&bytes[4..8], b"ftyp", "不是 ISOBMFF 容器");
        assert_eq!(&bytes[8..12], b"avif", "major brand 不是 avif");
        assert!(
            !out.join("frame_000000.avif-src.png").exists(),
            "临时 PNG 必须删掉"
        );
        sizes.push(bytes.len());
    }
    assert!(sizes[1] > sizes[0], "质量 90 应该比 30 大：{sizes:?}");

    // Display P3：原色/传递函数 ffmpeg 不写，由导出后打补丁补上。
    let out = td.path.join("p3");
    let mut opt = default_options(&out);
    opt.format = StillFormat::Avif;
    opt.color_space = aevum_core::colorspace::ColorSpace::DisplayP3;
    let files = export(&paths, &opt);
    let bytes = fs::read(&files[0]).unwrap();
    let at = bytes
        .windows(4)
        .position(|w| w == b"nclx")
        .expect("要有 nclx");
    let code = |i: usize| u16::from_be_bytes([bytes[at + 4 + i * 2], bytes[at + 5 + i * 2]]);
    assert_eq!(
        [code(0), code(1), code(2)],
        aevum_core::colorspace::ColorSpace::DisplayP3.nclx(),
        "nclx 该写 Display P3（12/13/1）"
    );
}

#[test]
fn stills_format_keys_roundtrip_through_the_project_file() {
    for format in StillFormat::ALL {
        let mut render = project::RenderState::default();
        render.format = format.key().to_owned();
        let text = serde_json::to_string(&render).unwrap();
        let back: project::RenderState = serde_json::from_str(&text).unwrap();
        assert_eq!(StillFormat::parse(&back.format), Some(format));
    }
    // 老工程里 "tiff16"/"jpg" 这类写法继续认。
    assert_eq!(StillFormat::parse("jpg"), Some(StillFormat::Jpeg));
    assert_eq!(StillFormat::parse("TIFF"), Some(StillFormat::Tiff16));
    assert_eq!(StillFormat::parse("heic"), None);
}

#[test]
fn video_mode_rejects_the_stills_only_containers() {
    for format in StillFormat::ALL.into_iter().filter(|f| !f.frames_ok()) {
        let td = TempDir::new("video-container");
        let paths = make_sequence(&td.path.join("src"), 2);
        let mut opt = default_options(&td.path.join("frames"));
        opt.mode = OutputMode::Video;
        opt.format = format;
        opt.video_out = td.path.join("out.mp4");
        let params = vec![GradeParams::default(); 2];
        let gains = vec![Ev::NONE; 2];
        let offsets = vec![[0.0f64; 2]; 2];
        let cancel = AtomicBool::new(false);
        let err = export_video(
            &PathBuf::from("ffmpeg-根本不存在"),
            &paths,
            &params,
            &gains,
            &offsets,
            &opt,
            &cancel,
            &|_| {},
        )
        .unwrap_err();
        assert!(
            matches!(err, ExportError::Invalid(_)),
            "{format:?} 该报参数不合法：{err}"
        );
        let frames = td.path.join("frames");
        assert!(
            !frames.exists() || fs::read_dir(&frames).unwrap().next().is_none(),
            "{format:?} 被撞开之前不该已经写下中间帧"
        );
    }
}

#[test]
fn output_sharpen_defaults_off_and_only_touches_edges() {
    let td = TempDir::new("osharpen");
    let paths = vec![edge_source(&td.path.join("src"))];

    // 默认（Off）那一路：像素还是源文件的那个值，一分没动。
    let off = export(&paths, &default_options(&td.path.join("off")));
    let base = image::open(&off[0]).unwrap().to_rgb8();
    assert_eq!(base[(5, 45)], Rgb([20, 20, 20]));
    assert_eq!(base[(155, 45)], Rgb([230, 230, 230]));

    let mut opt = default_options(&td.path.join("screen"));
    opt.sharpen_for = OutputSharpen::Screen;
    let screened = export(&paths, &opt);
    let got = image::open(&screened[0]).unwrap().to_rgb8();

    // 远离边缘的平场不许变，边缘附近必须有东西发生。
    assert_eq!(got[(5, 45)], Rgb([20, 20, 20]), "平场被锐化了");
    assert_eq!(got[(155, 45)], Rgb([230, 230, 230]), "平场被锐化了");
    assert!(
        (78..82).any(|x| got[(x, 45)] != base[(x, 45)]),
        "边缘一点变化都没有，输出锐化等于没接上"
    );

    // 强度档与用途档都要真的分层：用边缘处的偏离总量比。
    let dev = |dir: &Path, for_: OutputSharpen, strength: SharpenStrength| -> u64 {
        let mut o = default_options(dir);
        o.sharpen_for = for_;
        o.sharpen_strength = strength;
        let out = export(&paths, &o);
        let img = image::open(&out[0]).unwrap().to_rgb8();
        (0..90u32)
            .map(|y| u64::from(img[(79, y)][0]).abs_diff(u64::from(base[(79, y)][0])))
            .sum()
    };
    let low = dev(
        &td.path.join("low"),
        OutputSharpen::Matte,
        SharpenStrength::Low,
    );
    let high = dev(
        &td.path.join("high"),
        OutputSharpen::Matte,
        SharpenStrength::High,
    );
    assert!(high > low, "强度档没分层：high {high} <= low {low}");
    // 哑面比屏幕更狠：染料扩散补偿就是给它准备的。
    let screen = dev(
        &td.path.join("s2"),
        OutputSharpen::Screen,
        SharpenStrength::Standard,
    );
    let matte = dev(
        &td.path.join("m2"),
        OutputSharpen::Matte,
        SharpenStrength::Standard,
    );
    assert!(
        matte > screen,
        "哑面档应该比屏幕档更用力：{matte} vs {screen}"
    );
}

#[test]
fn exif_block_transfers_into_jpeg_and_png() {
    let td = TempDir::new("exif");
    let block = aevum_exif_block();
    let paths = vec![jpeg_source_with_exif(&td.path.join("src"), &block)];

    // 先确认这套写法本身能被读回来——不然后面的断言只是自我证明。
    assert_eq!(exif::read(&paths[0]).make(), Some("LUMEN"));

    let mut opt = default_options(&td.path.join("keep"));
    opt.format = StillFormat::Jpeg;
    opt.keep_exif = true;
    let out = export(&paths, &opt);
    assert_eq!(
        exif::read(&out[0]).make(),
        Some("LUMEN"),
        "JPEG 没把 EXIF 带过去"
    );
    // 整块原样：搬运不是重写，读回来的字节必须一模一样。
    assert_eq!(
        exif::transfer_block(&out[0]).unwrap(),
        block,
        "EXIF 块被改写了"
    );

    // PNG：容器不同，块照样在（eXIf 是 PNG 的正规位置）。
    let mut png_opt = default_options(&td.path.join("png"));
    png_opt.keep_exif = true;
    let png_out = export(&paths, &png_opt);
    let bytes = fs::read(&png_out[0]).unwrap();
    assert!(
        bytes.windows(4).any(|w| w == b"eXIf"),
        "PNG 输出里没有 eXIf 块"
    );

    // 关掉开关就该干净：元数据不能变成默认行为。
    let mut bare = default_options(&td.path.join("bare"));
    bare.format = StillFormat::Jpeg;
    let bare_out = export(&paths, &bare);
    assert_eq!(exif::read(&bare_out[0]).make(), None);

    // TIFF 不支持搬运，开着开关也不能报错。
    let mut tif = default_options(&td.path.join("tif"));
    tif.format = StillFormat::Tiff16;
    tif.keep_exif = true;
    let tif_out = export(&paths, &tif);
    assert!(tif_out[0].is_file());
}

/// PNG 的 `eXIf` 块体（就是那段 TIFF 结构）；没有这个块给 `None`。
fn png_exif_chunk(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    let at = bytes.windows(4).position(|w| w == b"eXIf")?;
    let len = u32::from_be_bytes(bytes.get(at - 4..at)?.try_into().ok()?) as usize;
    Some(bytes.get(at + 4..at + 4 + len)?.to_vec())
}

#[test]
fn metadata_lands_in_exported_stills() {
    let td = TempDir::new("meta");
    let meta = project::Metadata {
        title: "云海日出".to_owned(),
        description: "松下 G9 · 间隔 2 秒".to_owned(),
        creator: "张三".to_owned(),
        copyright: "© 2026 张三".to_owned(),
    };

    // 没开 keep_exif 也要带上自己写的元数据：这条路的 EXIF 块是新造的。
    // PNG 的 EXIF 住在 eXIf 块里，`exif::read` 只认 JPEG/TIFF 头，所以自己取块。
    let src = edge_source(&td.path.join("src"));
    let mut opt = default_options(&td.path.join("png"));
    opt.metadata = meta.clone();
    let out = export(std::slice::from_ref(&src), &opt);
    let block = png_exif_chunk(&out[0]).expect("PNG 里没有 eXIf 块");
    let back = exif::read_block(&block);
    assert_eq!(back.title(), Some("云海日出"));
    assert_eq!(back.description(), Some("松下 G9 · 间隔 2 秒"));
    assert_eq!(back.artist(), Some("张三"));
    assert_eq!(back.copyright(), Some("© 2026 张三"));

    // JPEG 走容器正规位置，直接 read 就该拿到。
    let mut jpg = default_options(&td.path.join("jpg"));
    jpg.format = StillFormat::Jpeg;
    jpg.metadata = meta.clone();
    let out = export(std::slice::from_ref(&src), &jpg);
    assert_eq!(exif::read(&out[0]).artist(), Some("张三"));

    // 搬源 EXIF 与写元数据同时开：源块里的 Make 要活着，新写的三项也在。
    let source_block = aevum_exif_block();
    let jpeg_src = jpeg_source_with_exif(&td.path.join("exifsrc"), &source_block);
    let mut both = default_options(&td.path.join("both"));
    both.format = StillFormat::Jpeg;
    both.keep_exif = true;
    both.metadata = meta.clone();
    let out = export(&[jpeg_src], &both);
    let back = exif::read(&out[0]);
    assert_eq!(back.make(), Some("LUMEN"), "源 EXIF 被新块顶掉了");
    assert_eq!(back.copyright(), Some("© 2026 张三"));
    let merged = exif::transfer_block(&out[0]).expect("合并后的块");
    assert!(
        merged.len() > source_block.len(),
        "块没变大，元数据大概没写进去"
    );

    // 四项全空 + 开关关着：输出里不该多出 EXIF。
    let bare = default_options(&td.path.join("bare"));
    let out = export(&[src], &bare);
    assert!(png_exif_chunk(&out[0]).is_none(), "没填元数据却写了 EXIF");

    // TIFF 装不进 EXIF 块，填了元数据也只能安静跳过。
    let mut tif = default_options(&td.path.join("tif"));
    tif.format = StillFormat::Tiff16;
    tif.metadata = meta;
    let out = export(&[edge_source(&td.path.join("src2"))], &tif);
    assert!(out[0].is_file());
}

/// XMP 旁车：开关开着时每张静帧旁边多一个同名 `.xmp`（`dc:` 四项 + 这一张的 `crs:`），
/// 关着一个字节都不写；旁车是独立文件，所以 TIFF 这种装不进 EXIF 的容器照样有。
#[test]
fn sidecar_lands_next_to_exported_stills() {
    let td = TempDir::new("sidecar");
    let srcs = make_sequence(&td.path.join("src"), 3);
    let meta = project::Metadata {
        title: "云海日出".to_owned(),
        copyright: "© 2026 张三".to_owned(),
        ..project::Metadata::default()
    };

    // 默认关着：目录里只有静帧。
    let off_dir = td.path.join("off");
    let off = export(&srcs, &default_options(&off_dir));
    assert_eq!(off.len(), 3);
    assert!(
        !off_dir.join("frame_000000.xmp").exists(),
        "没开旁车却写出来了"
    );

    let mut opt = default_options(&td.path.join("on"));
    opt.xmp_sidecar = true;
    opt.metadata = meta.clone();
    let on = export(&srcs, &opt);
    assert_eq!(on.len(), 3);
    for (i, frame) in on.iter().enumerate() {
        let sidecar = frame.with_extension("xmp");
        assert!(sidecar.exists(), "第 {i} 张没有旁车");
        let text = fs::read_to_string(&sidecar).unwrap();
        assert!(
            text.contains("云海日出") && text.contains("© 2026 张三"),
            "dc 四项没进去：{text}"
        );
        assert!(!text.contains("dc:description"), "空字段写了空标签");
        assert!(!text.contains("<crs:Name>"), "旁车不是预设，不该有预设名");
        // 调色那半用读预设的同一个读头验回来：默认参数就得读回默认参数。
        assert_eq!(
            aevum_core::xmp::read(&sidecar).expect("旁车里的 crs 读不回来"),
            GradeParams::default()
        );
    }

    // TIFF：EXIF 装不进去，旁车照样写——两条路互不牵制。
    let mut tif = default_options(&td.path.join("tif"));
    tif.format = StillFormat::Tiff16;
    tif.xmp_sidecar = true;
    tif.metadata = meta;
    let out = export(std::slice::from_ref(&srcs[0]), &tif);
    assert!(out[0].with_extension("xmp").is_file(), "TIFF 没写旁车");

    // 视频模式那一道闸：中间帧是过程产物，不给它们发旁车。
    let mut video = opt.clone();
    video.mode = OutputMode::Video;
    let frame = on[0].clone();
    assert!(
        aevum_core::export::write_sidecar(&frame, &GradeParams::default(), &video)
            .expect("旁车写失败")
            .is_none(),
        "视频模式不该发旁车"
    );
    let single = td.path.join("single.png");
    assert_eq!(
        aevum_core::export::write_sidecar(&single, &GradeParams::default(), &opt).unwrap(),
        Some(td.path.join("single.xmp")),
        "静帧模式该发旁车"
    );
}

/// 两张同尺寸图的差异坐标：水印那种「只动一小块」的效果，看被动过的位置比看数值直观。
fn changed(a: &RgbImage, b: &RgbImage) -> Vec<(u32, u32)> {
    (0..a.height())
        .flat_map(|y| (0..a.width()).map(move |x| (x, y)))
        .filter(|&(x, y)| a[(x, y)] != b[(x, y)])
        .collect()
}

#[test]
fn watermark_lands_on_every_export_route() {
    let td = TempDir::new("watermark");
    let src = edge_source(&td.path.join("src"));
    let paths = vec![src.clone()];
    let read = |p: &Path| image::open(p).unwrap().to_rgb8();
    let stamped = |dir: &Path| {
        let mut o = default_options(dir);
        o.watermark.enabled = true;
        o.watermark.text = "LUMEN".to_owned();
        o.watermark.size = 8;
        o.watermark.opacity = 100;
        o
    };

    // 默认（关）那一路一个像素都不许动——金标逐位不变靠的就是这条。
    let plain = export(&paths, &default_options(&td.path.join("plain")));
    let bare = read(&plain[0]);

    // 批量静帧：右下落款只准动右下角那一块。
    let stills = export(&paths, &stamped(&td.path.join("stills")));
    let still_diff = changed(&bare, &read(&stills[0]));
    assert!(!still_diff.is_empty(), "开了水印，静帧却一个像素都没动");
    assert!(
        still_diff.iter().all(|&(x, y)| x >= 80 && y >= 45),
        "右下落款盖到别处去了： {:?}",
        &still_diff[..still_diff.len().min(4)]
    );

    // 视频中间帧：喂 ffmpeg 的那批像素和静帧吃的是同一刀，坐标要一字不差。
    let mut frames_opt = stamped(&td.path.join("frames"));
    frames_opt.mode = OutputMode::Video;
    frames_opt.video_out = td.path.join("out.mp4");
    let frames = export(&paths, &frames_opt);
    assert_eq!(
        changed(&bare, &read(&frames[0])),
        still_diff,
        "中间帧和静帧吃的不是同一刀"
    );

    // GUI 单帧：像素已经是最终尺寸，从 write_single_still 进去，位置也得对得上。
    let (rgb, sw, sh, deep) = aevum_core::metrics::decode_rgb_f32(&src).unwrap();
    let single = |name: &str, options: &RenderOptions| -> RgbImage {
        let dst = td.path.join(name);
        let mut buf = rgb.clone();
        write_single_still(
            &src,
            &dst,
            &mut buf,
            sw as usize,
            sh as usize,
            deep,
            options,
        )
        .unwrap();
        read(&dst)
    };
    let off_single = single("single-off.png", &RenderOptions::default());
    let on_single = single("single-on.png", &stamped(&td.path.join("single")));
    assert_eq!(
        changed(&off_single, &on_single),
        still_diff,
        "单帧的印章和批量的对不上"
    );
}

#[test]
fn output_sharpen_keys_parse_and_default_off() {
    for for_ in OutputSharpen::ALL {
        assert_eq!(OutputSharpen::parse(for_.key()), Some(for_));
    }
    assert_eq!(OutputSharpen::default(), OutputSharpen::Off);
    assert_eq!(OutputSharpen::parse("none"), Some(OutputSharpen::Off));
    assert_eq!(OutputSharpen::parse("poster"), None);
    for s in SharpenStrength::ALL {
        assert_eq!(SharpenStrength::parse(s.key()), Some(s));
    }
    assert_eq!(SharpenStrength::default(), SharpenStrength::Standard);
    // Off 之外才有 profile；强度档只乘强度，半径跟用途走。
    let mut o = RenderOptions::default();
    assert_eq!(o.sharpen_profile(), None);
    o.sharpen_for = OutputSharpen::Screen;
    let (low_a, low_r, _) = o.sharpen_profile().unwrap();
    o.sharpen_strength = SharpenStrength::High;
    let (high_a, high_r, _) = o.sharpen_profile().unwrap();
    assert!(high_a > low_a && low_r == high_r);
}

// ---- 断点续跑（resume） ----------------------------------------------------

#[test]
fn resume_keeps_finished_frames_and_fills_only_the_gaps() {
    let src = TempDir::new("resume-src");
    let out = TempDir::new("resume-out");
    let paths = make_sequence(&src.path, 6);
    let mut options = default_options(&out.path);
    assert_eq!(export(&paths, &options).len(), 6);

    // 假装上一次跑到一半断在这儿：第 3 帧留一个可辨认的哨兵内容，第 5 帧整个没写出来。
    let third = frame_path(&out.path, StillFormat::Png8, 2);
    fs::write(&third, b"sentinel-not-a-png").unwrap();
    let fifth = frame_path(&out.path, StillFormat::Png8, 4);
    fs::remove_file(&fifth).unwrap();

    options.resume = true;
    let again = export(&paths, &options);
    assert_eq!(again.len(), 6, "续跑也要交出全部 6 帧的路径");
    assert_eq!(
        fs::read(&third).unwrap(),
        b"sentinel-not-a-png",
        "已经写完的那一帧被重渲了"
    );
    assert!(image::open(&fifth).is_ok(), "缺的那一帧没补上");
    assert!(
        !frame_is_done(&fifth, &options) || image::open(&fifth).is_ok(),
        "补出来的帧读不开"
    );

    // 临时名不许留在盘上：留着就是下一次续跑眼里「一张写完的图」。
    let leftovers: Vec<String> = fs::read_dir(&out.path)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".part"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "输出目录里留下了临时名：{leftovers:?}"
    );
}

#[test]
fn resume_redoes_zero_byte_frames_and_missing_sidecars() {
    let src = TempDir::new("resume2-src");
    let out = TempDir::new("resume2-out");
    let paths = make_sequence(&src.path, 3);
    let mut options = default_options(&out.path);
    options.xmp_sidecar = true;
    export(&paths, &options);

    // 空文件：别的程序占位留的、或者手工截出来的，都不能算导完。
    let first = frame_path(&out.path, StillFormat::Png8, 0);
    fs::write(&first, []).unwrap();
    assert!(!frame_is_done(&first, &options), "零字节的静帧被当成导完");
    // 旁车缺一张：静帧本身好着，但这一帧的交付没完。
    let second = frame_path(&out.path, StillFormat::Png8, 1);
    let second_xmp = xmp::sidecar_for(&second);
    assert!(second_xmp.is_file(), "上一趟没写旁车");
    fs::remove_file(&second_xmp).unwrap();
    assert!(!frame_is_done(&second, &options), "缺旁车的帧被当成导完");

    options.resume = true;
    export(&paths, &options);
    assert!(image::open(&first).is_ok(), "空文件没被重渲");
    assert!(second_xmp.is_file(), "缺的旁车没补上");
    assert!(frame_is_done(&first, &options), "补完之后这一帧该算导完");
}

#[test]
fn existing_frames_counts_only_what_resume_will_skip() {
    let src = TempDir::new("resume3-src");
    let out = TempDir::new("resume3-out");
    let paths = make_sequence(&src.path, 4);
    let mut options = default_options(&out.path);
    export(&paths, &options);

    // 没开续跑就报 0：状态栏那句「跳过 N」不能凭空冒出来。
    assert_eq!(existing_frames(&options, 4), 0, "续跑关着还数出已导出的帧");
    options.resume = true;
    assert_eq!(existing_frames(&options, 4), 4);
    fs::remove_file(frame_path(&out.path, StillFormat::Png8, 2)).unwrap();
    assert_eq!(existing_frames(&options, 4), 3, "少一张就该只报三张");
}
