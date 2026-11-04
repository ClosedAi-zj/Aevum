//! `aevum_deflicker_analyze` 的 `align_exif` 参数：EXIF 的台阶要真的进到逐帧 EV 里。
//!
//! 为什么单独一条：core 已经证明对齐本身有效，UI 也证明了面板开关落到校正表上。agent
//! 这条路是第三条线——MCP 自己读 EXIF、自己挑分析函数。这里写错（比如把开关存下来却
//! 不传给分析）在别的判据里看不见，因为「不开对齐」也是合法结果；只有真正带曝光台阶
//! 的素材才咬得住。
//!
//! 素材是**真 JPEG + 真 APP1**：EXIF 块按标准结构自己拼，交给 `aevum_open_sequence`
//! 之后由 `exif::read` 读回来。这样断言的是「字节 → 读数 → 分析 → 逐帧 EV」的整条路。
//!
//! Why a separate assertion: core proves alignment works and the UI proves the panel switch
//! lands in the correction table. The agent path is a third one -- MCP reads the EXIF and
//! picks the analyzer itself. A wiring bug there (storing the switch without passing it down)
//! stays invisible elsewhere, because "no alignment" is also a legal result; only material
//! with a real exposure staircase can bite.
//!
//! The material is a **real JPEG with a real APP1**: the EXIF block is assembled by hand to
//! the standard layout and read back by `exif::read` after `aevum_open_sequence`, so what is
//! asserted is the whole bytes-to-readings-to-analysis-to-per-frame-EV path.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use aevum_mcp::{dispatch, reset_session};

use crate::tools::LOCK;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aevum-mcp-grail-{tag}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 只带曝光三要素的小端 TIFF：ISO 200、f/2.8、快门由 `exposure_ms` 决定。
///
/// 标签号按 EXIF 标准写死（0x8827 / 0x829A / 0x829D），不从被测代码里取——判据的基准
/// 要站在被测物外面。
///
/// A little-endian TIFF carrying only the exposure triplet. The tag numbers are the EXIF
/// standard, hard-coded: an assertion must stand outside the thing it measures.
fn tiff_with_triplet(exposure_ms: u32) -> Vec<u8> {
    const TAG_EXPOSURE: u16 = 0x829A;
    const TAG_FNUM: u16 = 0x829D;
    const TAG_ISO: u16 = 0x8827;
    const VALUES_AT: u32 = 8 + 2 + 3 * 12 + 4;

    let mut out = b"II".to_vec();
    out.extend(42u16.to_le_bytes());
    out.extend(8u32.to_le_bytes());
    out.extend(3u16.to_le_bytes());
    let mut entry = |tag: u16, typ: u16, count: u32, value: u32| {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(value.to_le_bytes());
    };
    entry(TAG_EXPOSURE, 5, 1, VALUES_AT);
    entry(TAG_FNUM, 5, 1, VALUES_AT + 8);
    entry(TAG_ISO, 3, 1, 200);
    out.extend(0u32.to_le_bytes());
    out.extend(exposure_ms.to_le_bytes());
    out.extend(1000u32.to_le_bytes());
    out.extend(28u32.to_le_bytes());
    out.extend(10u32.to_le_bytes());
    out
}

/// 把 TIFF 包成 JPEG 的 APP1 段并插在 SOI 之后：`FFD8 | FFE1 len "Exif\0\0" TIFF | …`。
///
/// Wraps the TIFF into a JPEG APP1 segment right after SOI.
fn jpeg_with_exif(mut jpeg: Vec<u8>, tiff: &[u8]) -> Vec<u8> {
    assert_eq!(&jpeg[..2], b"\xFF\xD8", "image crate 写出来的该是 SOI 开头");
    let mut body = b"Exif\0\0".to_vec();
    body.extend_from_slice(tiff);
    let mut seg = vec![0xFF, 0xE1];
    seg.extend(((body.len() + 2) as u16).to_be_bytes());
    seg.extend(body);
    jpeg.splice(2..2, seg);
    jpeg
}

/// 线性光 → sRGB 编码（IEC 61966-2-1 分段式），与 `measure_image` 反查表的走向一致。
///
/// Linear light to sRGB encoding, the piecewise curve of IEC 61966-2-1, matching the
/// direction of `measure_image`'s lookup table.
fn linear_to_srgb(c: f64) -> f64 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// 合成日转夜（手动圣杯）：场景光 8 EV 匀速变暗，相机每 10 帧拧一档补偿（共 8 档），
/// 画面被拉平 → 原始亮度只有约 1 EV 的锯齿；EXIF 留着 8 EV 的台阶（快门每档翻倍）。
///
/// 用 JPEG 是因为 EXIF 挂在 JPEG/TIFF 上：PNG 夹具读不出曝光三要素。
///
/// A synthetic day-to-night (manual holy grail): the scene drops 8 EV steadily while the
/// camera gains a stop every 10 frames (8 stops total), so the picture is held flat (~1 EV
/// sawtooth) while the EXIF keeps an 8 EV staircase. JPEG because EXIF rides on JPEG/TIFF:
/// a PNG fixture carries no exposure triplet.
fn holy_grail_jpegs(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("建序列目录");
    for i in 0..n {
        let scene = -8.0 * i as f64 / (n - 1).max(1) as f64;
        let comp = (i / 10) as u32;
        // 画面亮度用**线性光**定义（与去闪的测光口径同一个域），写像素前转成 sRGB：
        // 直接拿线性值当 8 位灰度会把 1 档锯齿放大成 2 档多（编码曲线是凹的）。
        let linear = 0.5 * 2f64.powf(scene + comp as f64);
        let level = (255.0 * linear_to_srgb(linear)).clamp(4.0, 251.0) as u8;
        let image = image::RgbImage::from_fn(96, 64, |x, y| {
            // 一点纹理，避免整帧死平（去闪的窗口统计要有东西可测）。
            let v = level.saturating_add(((x + y) % 7) as u8);
            image::Rgb([v, v, v])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .expect("编码测试帧");
        let bytes = jpeg_with_exif(bytes, &tiff_with_triplet(20 << comp));
        std::fs::write(dir.join(format!("f{i:03}.jpg")), bytes).expect("写测试帧");
    }
}

fn ok(tool: &str, args: Value) -> Value {
    dispatch(tool, &args).unwrap_or_else(|e| panic!("{tool} 失败：{e}"))
}

/// 逐帧 EV 的落差（最大 − 最小），来自工具回执里的 `offsets`。
///
/// The spread of the per-frame EV (max - min), taken from the `offsets` in the reply.
fn ev_spread(reply: &Value) -> f64 {
    let max = reply["offsets"]["max"].as_f64().expect("回执该带 max");
    let min = reply["offsets"]["min"].as_f64().expect("回执该带 min");
    max - min
}

/// `align_exif` 必须把 EXIF 的台阶折进逐帧 EV；关着时只压锯齿。参数类型写错要在分析
/// 之前就报出来，不能当成 false 悄悄跑。
///
/// `align_exif` must fold the EXIF staircase into the per-frame EV; off, it only trims the
/// sawtooth. A wrongly typed argument must be reported before the analysis runs, never
/// silently treated as false.
#[test]
fn the_agent_switch_folds_the_exif_staircase_into_the_ev_series() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    let temp = TempDir::new("analyze");
    let seq_dir = temp.path().join("frames");
    holy_grail_jpegs(&seq_dir, 81);
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );

    // 关着：相机把画面拉平了，逐帧 EV 只该反映那点锯齿。
    let plain = ok("aevum_deflicker_analyze", json!({}));
    assert_eq!(plain["align_exif"], json!(false));
    assert_eq!(plain["ev_frames"], json!(0), "没开就不该去读 EXIF");
    let flat = ev_spread(&plain);
    assert!(
        flat < 2.0,
        "夹具失效：不开对齐时逐帧 EV 不该有大落差（实得 {flat:.2}）"
    );

    // 开着：8 EV 的台阶必须进来（并回后的总校正含这次真实的重曝光）。
    let aligned = ok("aevum_deflicker_analyze", json!({"align_exif": true}));
    assert_eq!(aligned["align_exif"], json!(true));
    assert_eq!(aligned["ev_frames"], json!(81), "每帧都带曝光三要素");
    assert!(aligned["align_note"].is_null(), "读到 EV 就不该报空转");
    let wide = ev_spread(&aligned);
    assert!(
        wide > 5.0,
        "开了预对齐，逐帧 EV 也只走了 {wide:.2} 档——EXIF 的台阶没进分析"
    );

    // 类型写错：报错要指名字段，且不能留下一个「以为开了」的会话状态。
    let err = dispatch("aevum_deflicker_analyze", &json!({"align_exif": "yes"}))
        .expect_err("字符串不是布尔");
    assert!(err.contains("align_exif"), "{err}");

    // 收尾：会话指着的临时目录马上就要被删，先把会话清掉——否则下一个用例（同一把锁、
    // 同一进程）会去读一个已经不在的序列。
    //
    // Wrap up: the session still points at a temp directory about to be deleted, so reset it
    // first -- otherwise the next test in this process reads a sequence that is gone.
    reset_session();
}
