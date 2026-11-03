//! 首屏预览预算 + 草图不污染全量输出：两条判据。
//!
//! # 第一条：清缓存后，24MP RAW 的草图首屏必须落在 300ms 内
//!
//! 基线要求「大图预览首屏 ≤300ms」。首屏 = 用户打开一张 24MP RAW 时画布上第一次出现
//! 画面的那一帧，走的是**草图**路径（`sketch = true`，`aevum-ui` 的 `PREVIEW_SRC` 冷
//! 缓存）。`aevum_ui::preview_first_screen_rows()` 与 `--bench-preview` 是同一条路径：
//! 每个样本清缓存后量 5 次取中位。
//!
//! 阈值取 300ms 端到端上限，不另留余量：口径已经声明只算 `render_preview_file` 的
//! CPU 墙钟（不含 GPU 上传与呈现），比真实首屏**乐观**，所以这里再放水就等于不测。
//!
//! ## 只在 release 下卡（debug 照量、只打印）
//!
//! 这条量的是 24MP 传感器数据的解包与重采样，几百兆内存流量。dev 档（opt-level 1）的
//! 同一段代码本机实测 279.9ms（release 228.0ms），慢机器上更多——门褴在 debug 下卡
//! 300ms 会把「机器慢」报成「代码退化」。所以 `cfg!(debug_assertions)` 时照常量、照常
//! 打表，只跳过断言：数字仍在验收日志里，要它真的红就跑
//! `cargo test --release -p aevum-ui preview`。
//!
//! ## 本机实测（2026-11-03，release，Windows 11，样张 `samples/cameras/`）
//!
//! | 样本 | 档位 | 改前中位 | 改后中位 | 解码 | 调色 | 其余 |
//! |------|------|---------:|---------:|-----:|-----:|-----:|
//! | PANA0450.RW2 (24MP) | 草图 | 312.4 | 223.1 | 220.0 | 0.7 | 2.4 |
//! | PANA0450.RW2 (24MP) | 全量 | 297.3 | 289.7 | 277.4 | 1.6 | 10.7 |
//! | PANA0198.JPG | 草图 | 267.9 | 240.1 | 237.3 | 0.5 | 2.2 |
//! | PANA0198.JPG | 全量 | 289.5 | 219.9 | 211.4 | 1.5 | 7.0 |
//!
//! 改前的草图是「整幅解到 1600 再 2×2 减半」：解码成本和全量一模一样（308.5ms），
//! 减半只省下调色那 1ms。改后草图按 800 边直接解（RAW 走半尺寸去马赛克），解码
//! 220ms 上下——省掉的正是「展开成 24MP RGB 再缩回来」这一来一回。
//!
//! **同一台机上分次运行会漂**：这台机器上同时还有别的编译在跑，同一份代码的全量一列
//! 见过 282.9–446.5ms、JPEG 见过 219.9–475.2ms。所以绝对值只作参考，退化要靠下面
//! 那条相对判据抓。
//!
//! ## 变异验证（提交后做的）
//!
//! 把 `metrics::decode_preview_sketch` 的半尺寸分支短路（`if false &&`，即草图退回
//! 「整幅解码到 1600 再减半」）：
//!
//! | 变异 | 草图 | 全量 | 比值 | 300ms 那条 | 比值那条 |
//! |------|-----:|-----:|-----:|-----------|---------|
//! | 断掉半尺寸分支 | 292.9 | 282.9 | 1.04 | **绿**（还没到 300） | **红** |
//!
//! 这正是加相对判据的理由：改前代码在这台机上的绝对值本来就在 286–350ms 之间漂，
//! 300ms 的上限一个人挡不住退化。
//!
//! # 第二条：草图路径不许改到全量预览的像素
//!
//! 首屏优化最容易出事的地方不是快慢，而是「顺手把全量也换了」：全量预览、直方图/曲线
//! 分析、导出都读 `PREVIEW_SRC` 的全量槽，它一变，曲线读数和成片就跟画布对不上。
//! 两条事实钉住它：
//!
//! 1. **像素指纹**：全量预览贴图的 FNV-1a 校验和是写死的金标。金标值是在改动**之后**
//!    量的，但那次改动只动了草图槽（`git show` 里全量那条 `decode_profile_preview`
//!    调用一字未改），所以它与改动前逐字节相同。**如果这是有意改的调色/解码，
//!    重新量这两个数**；如果不是，说明草图路径串进了全量输出。
//! 2. **顺序无关**：先跑草图再跑全量，与直接跑全量，指纹必须一致——两档各占一槽，
//!    草图的写入不许让全量槽「看起来还是新鲜的」。
//!
//! # First-screen budget and "the sketch must not touch the full output"
//!
//! The baseline asks for "large-image preview first screen <= 300ms". The first screen is the
//! **sketch** path with a cold `PREVIEW_SRC`. `preview_first_screen_rows()` is the same path
//! `--bench-preview` prints: five cold runs per sample, median. The 300ms is the end-to-end
//! ceiling and gets no extra slack, because the measurement already excludes GPU upload and
//! presentation and is therefore optimistic. The check only runs in release: it measures the
//! unpack and resample of 24MP sensor data, which is an order of magnitude slower without
//! optimisations, so a debug number would measure the compiler rather than the code
//! (`cargo test --release -p aevum-ui preview`). The second check pins the full preview's pixel
//! fingerprint against a hard-coded golden and verifies that sketch-then-full equals
//! full-alone: the two tiers have separate slots, and writing the sketch must not make the
//! full slot look fresh.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_ui::{preview_checksum, preview_first_screen_rows};

/// 首屏端到端上限（毫秒）。
///
/// End-to-end first-screen ceiling in milliseconds.
const BUDGET_MS: f64 = 300.0;

/// 草图相对全量最多只能花这么点（同一台机、同一次运行里量）。
///
/// 上限本身挡不住退化：本机同一段改前代码在不同时刻量到 286.6–349.5ms（机器状态与
/// 内存压力的差别就有这么大），300ms 的绝对值会时红时绿。所以再加一条**相对的**：
/// 草图既然按 800 边解，就**必须**比全量便宜——退化回「整幅解码再减半」时两者相等，
/// 比值约 1.0，这条立刻红。本机实测改后 224.8/293.3 = 0.77，阈值 0.90 两边都留着
/// 一成以上的余量。
///
/// The absolute ceiling cannot catch the regression on its own: the same pre-change code
/// measured 286.6-349.5ms at different moments on this machine, so a 300ms gate flips colour
/// with machine state. Hence a **relative** check: a sketch decoded at 800 must be cheaper
/// than the full preview; falling back to "decode fully, then halve" makes the two equal
/// (ratio ~1.0) and turns this red immediately. Measured 224.8/293.3 = 0.77 here, so 0.90
/// leaves more than ten percent of headroom on both sides.
const SKETCH_VS_FULL_RATIO: f64 = 0.90;

/// 24MP RAW 样本：本机 `samples/cameras/` 里那张 6000×4000 的 RW2。
///
/// The 24MP RAW sample: the 6000x4000 RW2 under `samples/cameras/` on this machine.
const RAW_SAMPLE: &str = "PANA0450.RW2";

/// 同门 JPEG 样本。
///
/// The JPEG sample from the same camera line.
const JPEG_SAMPLE: &str = "PANA0198.JPG";

/// 全量预览像素指纹金标：24MP RAW。
///
/// Golden fingerprint of the full preview for the 24MP RAW.
const GOLDEN_FULL_RAW: u64 = 7566983699783529374;

/// 全量预览像素指纹金标：JPEG。
///
/// Golden fingerprint of the full preview for the JPEG.
const GOLDEN_FULL_JPEG: u64 = 9138833060390763379;

/// 全量预览像素指纹金标：合成图（入库，任何机器上都一样）。
///
/// Golden fingerprint of the full preview for the synthetic picture (committed, identical on
/// every machine).
const GOLDEN_FULL_SYNTH: u64 = 17066742548242236717;

/// 样张目录。不入库，所以可能不存在。
///
/// The sample directory. Not committed, so it may not exist.
fn sample_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("cameras")
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aevum-ui-preview-budget-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 首屏预算：24MP RAW 清缓存后草图首次 ≤300ms。
///
/// First-screen budget: a 24MP RAW's cold sketch must come in at or under 300ms.
#[test]
fn preview_sketch_first_screen_within_budget() {
    let rows = preview_first_screen_rows().expect("首屏基准跑不起来");
    if rows.is_empty() {
        println!("跳过：没有样张（samples/cameras/ 不入库）");
        return;
    }

    let mut table = String::from("首屏预览（清缓存后 5 次取中位，ms）：\n");
    for row in &rows {
        table.push_str(&format!(
            "  {:<14} {:<4} 端到端 {:7.1}  解码 {:6.1}  调色 {:5.1}  其余 {:5.1}\n",
            row.file,
            if row.sketch { "草图" } else { "全量" },
            row.median_ms,
            row.decode_ms,
            row.grade_ms,
            row.rest_ms
        ));
    }
    println!("{table}");

    // debug 下只打印不判：这条量的是 24MP 数据的解包与重采样，dev 档（opt-level 1）
    // 的同一段代码是本机 release 的 1.2 倍上下、慢机器上更多——门槛跑 `cargo test`
    // 时不卡它，数字照样留在日志里；要它真的红，跑
    // `cargo test --release -p aevum-ui preview`。
    //
    // In debug this only prints: the measurement is 24MP unpack plus resampling, and the dev
    // profile (opt-level 1) runs 1.2x the release time here and more on a slow box. The
    // gate (`cargo test`) therefore does not assert it, but the numbers still land in the
    // log; to make it bite, run `cargo test --release -p aevum-ui preview`.
    if cfg!(debug_assertions) {
        println!("debug 档：只打印不判（预算按 release 口径）。");
        return;
    }

    let sketch = rows
        .iter()
        .find(|r| r.sketch && r.file == RAW_SAMPLE)
        .unwrap_or_else(|| panic!("没有 {RAW_SAMPLE} 的草图行：\n{table}"));
    let full = rows
        .iter()
        .find(|r| !r.sketch && r.file == RAW_SAMPLE)
        .unwrap_or_else(|| panic!("没有 {RAW_SAMPLE} 的全量行：\n{table}"));

    assert!(
        sketch.median_ms <= BUDGET_MS,
        "24MP RAW 草图首屏中位 {:.1}ms 越过 {BUDGET_MS:.0}ms：\n{table}",
        sketch.median_ms
    );
    // 相对判据：草图必须真的比全量便宜（否则它又在按 1600 整幅解码）。
    //
    // The relative check: the sketch has to be genuinely cheaper than the full preview,
    // otherwise it is decoding 1600-wide again.
    let ratio = sketch.median_ms / full.median_ms.max(f64::MIN_POSITIVE);
    assert!(
        ratio <= SKETCH_VS_FULL_RATIO,
        "草图 {:.1}ms 是全量 {:.1}ms 的 {ratio:.2} 倍（上限 {SKETCH_VS_FULL_RATIO:.2}）——\
         草图又在整幅解码：\n{table}",
        sketch.median_ms,
        full.median_ms
    );
}

/// 草图路径不得改变全量预览输出。
///
/// The sketch path must not change the full preview output.
#[test]
fn sketch_path_does_not_change_the_full_preview() {
    let dir = temp_dir("golden");
    let synth = dir.join("frame.png");
    let mut img = image::RgbImage::new(120, 80);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgb([(x * 2) as u8, (y * 3) as u8, ((x + y) * 3) as u8]);
    }
    img.save(&synth).expect("写测试图");

    // 1. 金标：全量预览的像素指纹。
    let full = preview_checksum(&synth, false).expect("渲染全量预览");
    assert_eq!(
        full, GOLDEN_FULL_SYNTH,
        "合成图的全量预览输出变了。若这是有意的调色/解码改动，重新量这个金标；\
         否则是草图路径串进了全量输出"
    );

    // 2. 顺序无关：先草图再全量，与直接全量，指纹必须一致。
    let sketch = preview_checksum(&synth, true).expect("渲染草图");
    let after_sketch = preview_checksum(&synth, false).expect("草图之后再渲染全量");
    assert_eq!(
        full, after_sketch,
        "草图（指纹 {sketch:#x}）跑过之后全量预览的像素变了：两档缓存串槽了"
    );

    let _ = std::fs::remove_dir_all(&dir);

    // 3. 真样张上的同一对事实（样张不入库，缺了跳过）。
    let dir = sample_dir();
    for (name, golden) in [
        (RAW_SAMPLE, GOLDEN_FULL_RAW),
        (JPEG_SAMPLE, GOLDEN_FULL_JPEG),
    ] {
        let path = dir.join(name);
        if !path.is_file() {
            println!("跳过 {name}：样张不在（samples/cameras/ 不入库）");
            continue;
        }
        let full = preview_checksum(&path, false).expect("渲染全量预览");
        assert_eq!(
            full, golden,
            "{name} 的全量预览输出变了。若这是有意的调色/解码改动，重新量这个金标；\
             否则是草图路径串进了全量输出"
        );
        let _ = preview_checksum(&path, true).expect("渲染草图");
        let after_sketch = preview_checksum(&path, false).expect("草图之后的全量预览");
        assert_eq!(
            full, after_sketch,
            "{name}：草图跑过之后全量预览的像素变了——两档缓存串槽了"
        );
    }
}
