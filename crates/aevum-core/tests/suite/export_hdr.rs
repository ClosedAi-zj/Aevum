//! HDR（PQ）输出的端到端判据：约束组合、SDR 回归、以及「PQ 真的写进了文件」。
//!
//! 为什么要一条单独的测试文件：金标（`acceptance/oracle/golden/develop.json`）只覆盖
//! **显影链的数值**，它到「渲染出 f32」为止，P 后面发生的事（色域搬运、传递函数、
//! 量化、容器编码）一件都不管。所以 HDR 这条路的判据必须自己造，不然「导出 HDR」
//! 这句话没有任何东西在看着。
//!
//! 最关键的一条是 [`pq_codes_are_actually_written_into_the_file`]：它把导出的 AVIF
//! 读回来，按 rgb48 解出码值，再跟「如果忘了做 PQ 编码会得到什么」对照。少了它，
//! 就算文件里写着 `smpte2084`，也可能只是标签对、像素是 8-bit 的 SDR 值——那种错
//! 恰恰最难看出来（播放器会把 SDR 值当 PQ 解释，画面死黑）。
//!
//! End-to-end criteria for HDR (PQ) output: constraint combinations, the SDR regression, and
//! proof that "PQ really made it into the file".
//!
//! Why a separate test file: the golden data covers only the **develop chain's numbers**, up
//! to "render to f32"; everything after that (gamut move, transfer function, quantization,
//! container coding) is unguarded. So the HDR path needs criteria of its own, otherwise
//! nothing at all is watching the sentence "export HDR".
//!
//! The one that matters most is [`pq_codes_are_actually_written_into_the_file`]: it reads the
//! exported AVIF back, decodes rgb48 codes, and compares against what "forgot to apply PQ"
//! would produce. Without it a file could carry a correct `smpte2084` tag over pixels that are
//! still 8-bit SDR values — the worst kind of wrong, since the player then interprets SDR
//! codes as PQ and the picture goes black.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::colorspace::ColorSpace;
use aevum_core::develop::Ev;
use aevum_core::export::{
    OutputDynamic, OutputMode, OutputSharpen, RenderOptions, SharpenStrength, StillFormat,
    export_stills, find_ffmpeg,
};
use aevum_core::grade::GradeParams;
use image::{Rgb, RgbImage};

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
            "aevum-hdr-{tag}-{}-{nanos}-{}",
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

fn options(out_dir: &Path, format: StillFormat) -> RenderOptions {
    RenderOptions {
        mode: OutputMode::Stills,
        format,
        quality: 90,
        sharpen_for: OutputSharpen::Off,
        sharpen_strength: SharpenStrength::Standard,
        out_dir: out_dir.to_path_buf(),
        ..RenderOptions::default()
    }
}

fn export(paths: &[PathBuf], opt: &RenderOptions) -> Vec<PathBuf> {
    let params = vec![GradeParams::default(); paths.len()];
    let gains = vec![Ev::NONE; paths.len()];
    let offsets = vec![[0.0f64; 2]; paths.len()];
    let cancel = AtomicBool::new(false);
    export_stills(paths, &params, &gains, &offsets, opt, &cancel, &|_| {}).unwrap()
}

/// 一半纯黑、一半纯白的源图：两段各自落在 PQ 曲线的两端，读回来一眼能分辨
/// 「做了 PQ」与「没做 PQ」。
///
/// A source image that is half pure black and half pure white: the two halves sit at the
/// ends of the PQ curve, so the read-back tells "PQ applied" from "PQ skipped" at a glance.
fn half_black_half_white(dir: &Path) -> Vec<PathBuf> {
    fs::create_dir_all(dir).unwrap();
    let (w, h) = (64u32, 32u32);
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = if x < w / 2 { 0u8 } else { 255u8 };
            img.put_pixel(x, y, Rgb([v, v, v]));
        }
    }
    let path = dir.join("frame_000000.png");
    img.save(&path).unwrap();
    vec![path]
}

// ---------------------------------------------------------------------------
// 约束组合：不用 ffmpeg，纯逻辑
// ---------------------------------------------------------------------------

/// HDR（PQ）只能配 AVIF：选 PNG/TIFF/JPEG/WebP 时必须报错，不能悄悄退回 SDR。
///
/// 悄悄退回是这里最坏的失败方式——界面显示「HDR」，文件里是 8bit SDR，用户拿去
/// HDR 屏上看是一片灰，而且没有任何地方会提示。
///
/// HDR (PQ) only fits AVIF: PNG/TIFF/JPEG/WebP must error out rather than quietly fall
/// back to SDR.
///
/// The quiet fallback is the worst failure mode here — the UI says "HDR", the file is 8-bit
/// SDR, and on an HDR display it looks flat grey with nothing anywhere to explain why.
#[test]
fn hdr_pq_is_refused_by_eight_bit_containers() {
    for format in [
        StillFormat::Png8,
        StillFormat::Tiff16,
        StillFormat::Jpeg,
        StillFormat::WebP,
    ] {
        let mut opt = options(Path::new("."), format);
        opt.dynamic = OutputDynamic::HdrPq;
        opt.color_space = ColorSpace::Rec2020;
        let err = opt
            .check_dynamic()
            .expect_err("8bit 容器必须拒绝 HDR（PQ）");
        let msg = err.to_string();
        assert!(
            msg.contains("AVIF"),
            "报错要说清只有 AVIF 装得下，实测：{msg}"
        );
    }
    // AVIF + Rec.2020 是唯一成立的组合。
    let mut ok = options(Path::new("."), StillFormat::Avif);
    ok.dynamic = OutputDynamic::HdrPq;
    ok.color_space = ColorSpace::Rec2020;
    assert!(ok.check_dynamic().is_ok(), "AVIF + Rec.2020 应当放行");
}

/// HDR（PQ）必须配 Rec.2020：sRGB / P3 原色上的 PQ 没有播放器认。
///
/// HDR (PQ) requires Rec.2020: PQ on sRGB or P3 primaries is recognized by no player.
#[test]
fn hdr_pq_is_refused_by_narrow_gamuts() {
    for space in [ColorSpace::Srgb, ColorSpace::DisplayP3] {
        let mut opt = options(Path::new("."), StillFormat::Avif);
        opt.dynamic = OutputDynamic::HdrPq;
        opt.color_space = space;
        let err = opt.check_dynamic().expect_err("窄色域必须拒绝 HDR（PQ）");
        assert!(
            err.to_string().contains("Rec.2020"),
            "报错要点名 Rec.2020，实测：{err}"
        );
        assert!(space.nclx_pq().is_none(), "{} 不该有 PQ 色标", space.cn());
    }
    assert_eq!(
        ColorSpace::Rec2020.nclx_pq(),
        Some([9, 16, 9]),
        "Rec.2020 的 PQ 色标应当是 primaries 9 / transfer 16（ST 2084）/ matrix 9"
    );
    assert_eq!(
        ColorSpace::Rec2020.vui_pq(),
        Some(("bt2020", "smpte2084", "bt2020nc"))
    );
}

/// SDR 是所有容器的默认，且默认值就是 SDR。
///
/// SDR is allowed in every container, and the default is SDR.
#[test]
fn sdr_is_the_default_and_fits_everywhere() {
    assert_eq!(OutputDynamic::default(), OutputDynamic::Sdr);
    for format in StillFormat::ALL {
        assert!(
            OutputDynamic::Sdr.fits(format),
            "{} 装得下 SDR",
            format.cn()
        );
        let mut opt = options(Path::new("."), format);
        opt.dynamic = OutputDynamic::Sdr;
        opt.color_space = ColorSpace::Rec2020;
        assert!(opt.check_dynamic().is_ok());
    }
}

/// 工程/命令行里的拼写要稳，而且 `hdr` 这种随手写法也要认。
///
/// The spellings used by the project file and the command line must be stable, and loose
/// forms like `hdr` must be accepted.
#[test]
fn dynamic_keys_round_trip() {
    for d in OutputDynamic::ALL {
        assert_eq!(OutputDynamic::parse(d.key()), Some(d), "{}", d.key());
    }
    assert_eq!(OutputDynamic::parse("hdr"), Some(OutputDynamic::HdrPq));
    assert_eq!(OutputDynamic::parse("PQ"), Some(OutputDynamic::HdrPq));
    assert_eq!(OutputDynamic::parse("SDR "), Some(OutputDynamic::Sdr));
    assert_eq!(OutputDynamic::parse("hdr10"), None);
}

// ---------------------------------------------------------------------------
// SDR 回归：这批改动不许碰默认出图
// ---------------------------------------------------------------------------

/// 默认（SDR）导出的 PNG 必须逐字节不变。
///
/// 判据不是「跟上次的字节比」（那要往仓库里塞一张图），而是「显式写 SDR 与不写
/// 完全一致」——加上 HDR 那条路之后，默认路径一个字节都不该动。
///
/// A default (SDR) PNG export must be byte-identical.
///
/// The criterion is not "compare against last time's bytes" (that would mean committing an
/// image), but "explicit SDR equals not specifying it" — adding the HDR path must not move
/// the default path by a single byte.
#[test]
fn sdr_output_is_unchanged_by_the_new_switch() {
    let td = TempDir::new("sdr");
    let paths = half_black_half_white(&td.path.join("src"));
    let a = export(&paths, &options(&td.path.join("a"), StillFormat::Png8));
    let mut explicit = options(&td.path.join("b"), StillFormat::Png8);
    explicit.dynamic = OutputDynamic::Sdr;
    let b = export(&paths, &explicit);
    let ba = fs::read(&a[0]).unwrap();
    let bb = fs::read(&b[0]).unwrap();
    assert_eq!(ba, bb, "显式 SDR 与默认必须逐字节一致");
}

// ---------------------------------------------------------------------------
// 端到端：需要 ffmpeg
// ---------------------------------------------------------------------------

/// 在 ISOBMFF 里找 `colr` 的 `nclx` 三元组。测试自备，不复用实现里的私有函数——
/// 判据不该和被测代码共用同一份解析逻辑。
///
/// Finds the `colr`/`nclx` triple inside the ISOBMFF. Local to the test and not reusing the
/// private helper in the implementation: a criterion must not share its parsing logic with
/// the code under test.
fn nclx_of(bytes: &[u8]) -> Option<[u16; 3]> {
    let at = bytes.windows(4).position(|w| w == b"colr")?;
    let payload = at + 4;
    if bytes.get(payload..payload + 4)? != b"nclx" {
        return None;
    }
    let mut out = [0u16; 3];
    for (i, slot) in out.iter_mut().enumerate() {
        let off = payload + 4 + i * 2;
        *slot = u16::from_be_bytes(bytes.get(off..off + 2)?.try_into().ok()?);
    }
    Some(out)
}

/// 用 ffmpeg 把 AVIF 解成 rgb48 并读回第一条扫描线。
///
/// 显式 `-nostdin` 并把 stdin 接空：`status()` 会把测试进程的 stdin 继承给子进程，
/// 而 ffmpeg 在非交互管道上仍会去读它（等一个永远不会来的键）。2026-11-05 门槛就是
/// 卡在这一步：`cargo` 与 `ffmpeg` 的 CPU 累计都停在 0.0x 秒，进程在、一动不动。
/// 生产代码走 `Command::output()`（stdin 本来就是关的），这里补的是测试侧那条路。
///
/// Decodes the AVIF back to rgb48 with ffmpeg and reads the first scanline.
///
/// `-nostdin` plus a null stdin on purpose: `status()` hands the test process's stdin to
/// the child, and ffmpeg still reads it on a non-interactive pipe (waiting for a keypress
/// that never comes). That is where the gate stalled on 2026-11-05 -- both `cargo` and
/// `ffmpeg` sat at 0.0x seconds of CPU, alive but unmoving. Production code goes through
/// `Command::output()`, whose stdin is already closed; this fixes the test-side path.
fn decode_rgb48(ffmpeg: &Path, avif: &Path, w: u32, h: u32, out: &Path) -> Vec<u16> {
    let status = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(avif)
        .args(["-pix_fmt", "rgb48le", "-f", "rawvideo"])
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("起不来 ffmpeg");
    assert!(status.success(), "解码失败");
    let bytes = fs::read(out).unwrap();
    assert_eq!(
        bytes.len(),
        (w as usize) * (h as usize) * 3 * 2,
        "解码出来的字节数与 WxHx3x2 不符"
    );
    // 取中间那一行，避开上下边界的压缩伪影。
    let row = (h as usize / 2) * (w as usize) * 3;
    bytes[row * 2..row * 2 + (w as usize) * 3 * 2]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// 端到端：文件里真的是 10bit + PQ 码值，不是「标签对、像素还是 SDR」。
///
/// 三件事一起验：
/// 1. 容器：`ftyp/avif`，`colr/nclx` 是 (9, 16, 9)；
/// 2. 位深：解回来的白块码值在 PQ(203 nits)·65535 ≈ 38059 附近——**如果忘了做
///    PQ 编码**，白块会是 65535，两者差着 2.7 万级，不可能测混；
/// 3. 暗部：黑块接近 0（PQ 的零点在数学上不是 0，量化后就是 0 码值）。
///
/// 为什么用 quality=100：crf 4 时往返误差只有十几级，能把「编码错」与「压缩损失」
/// 分开；crf 20 的话两者混在一起，红了你也不知道该看哪儿。
///
/// End to end: the file really carries 10bit PQ codes, not "right tag, SDR pixels".
///
/// Three things at once:
/// 1. container: `ftyp/avif` with `colr/nclx` = (9, 16, 9);
/// 2. bit depth: the white block decodes near PQ(203 nits)·65535 ≈ 38059 — **had PQ been
///    skipped** it would be 65535, a gap of 27000 levels, impossible to confuse;
/// 3. shadows: the black block is near 0 (PQ's mathematical zero is not 0; quantized it is
///    code 0).
///
/// Why quality=100: at crf 4 the round-trip error is a dozen levels, which separates
/// "encoding is wrong" from "compression loss"; at crf 20 the two mix and a red test tells
/// you nothing about where to look.
#[test]
fn pq_codes_are_actually_written_into_the_file() {
    let Some(ffmpeg) = find_ffmpeg() else {
        eprintln!("跳过：本机没有 ffmpeg（口径同 export_stills.rs 的 AVIF 测试）");
        return;
    };
    let td = TempDir::new("e2e");
    let paths = half_black_half_white(&td.path.join("src"));
    let mut opt = options(&td.path.join("out"), StillFormat::Avif);
    opt.quality = 100;
    opt.dynamic = OutputDynamic::HdrPq;
    opt.color_space = ColorSpace::Rec2020;
    let files = export(&paths, &opt);
    let bytes = fs::read(&files[0]).unwrap();
    assert_eq!(&bytes[4..8], b"ftyp", "不是 ISOBMFF 容器");
    assert_eq!(&bytes[8..12], b"avif", "major brand 不是 avif");
    assert_eq!(
        nclx_of(&bytes),
        Some([9, 16, 9]),
        "colr/nclx 应当是 bt2020 原色 + ST 2084 传递 + bt2020nc 矩阵"
    );

    let raw = td.path.join("back.rgb");
    let row = decode_rgb48(&ffmpeg, &files[0], 64, 32, &raw);
    // 左半黑（x<32）、右半白，取各自靠里的位置避开跨界与边缘。
    let black = row[8 * 3] as f32 / 65535.0;
    let white = row[56 * 3] as f32 / 65535.0;
    let want_white = aevum_core::hdr_output::encoded_to_pq(1.0);
    assert!(
        (white - want_white).abs() < 0.02,
        "白块应当落在 PQ {want_white:.4}（≈38059/65535），实测 {white:.4}；\
         若接近 1.0 说明忘了做 PQ 编码"
    );
    assert!(white > 0.5, "白块不该被压到 PQ 中段以下：{white:.4}");
    assert!(black < 0.01, "黑块应当接近 0 码值，实测 {black:.4}");
    // 顺带把「没做 PQ 会怎样」写清楚，免得将来看不懂这条断言的边界。
    assert!(
        white < 0.99,
        "白块顶到 1.0 等于 65535，那是没做 PQ 编码的直接特征"
    );

    // 临时文件（SDR 那条路才有）不该出现在输出目录里。
    assert!(
        !td.path.join("out/frame_000000.avif-src.png").exists(),
        "HDR 路径不该落临时 PNG"
    );
}
