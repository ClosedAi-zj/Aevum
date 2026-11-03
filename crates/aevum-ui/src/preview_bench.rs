//! 首屏预览基准：清缓存后量「打开一张大图」的端到端墙钟。
//!
//! # 为什么单独一条基准
//!
//! `bench_ui`（`--bench-ui`）量的是**帧时**：布局与绘制提交。它看不见打开一张 24MP
//! RAW 的那一屏——那部分在渲染线程里，一次解码就够把一帧的时间预算打穿。基线要求
//! 「大图预览首屏 ≤300ms」，所以这里量的是另一件事：**从空缓存到调色贴图出会来**。
//!
//! # 口径（写死在这里，数字才有意义）
//!
//! - **每个样本量 5 次取中位**，每次之前清掉预览源缓存（`PREVIEW_SRC` / `PREVIEW_ORIG`），
//!   所以量的是冷路径。缓存命中之后草图只要几毫秒，那个数字跟首屏无关。
//! - **只计 `render_preview_file` 的墙钟**：解码 → 调色（DCP/白平衡/曲线）→ 取景 →
//!   成 `ColorImage` → 直方图。**不含** GPU 上传与呈现，也不含文件首次从磁盘读入
//!   （量之前先把文件读一遍，让 OS 页缓存热起来——否则第一次量的是这块盘）。
//! - **分解列**：`decode` 与 `grade` 由 `render_preview_file` 自己记（`PreviewPhases`），
//!   不是在外面另跑一遍估算的，所以三段一定落在同一次渲染上；`rest` = 总中位 − 两段，
//!   含取景、成图与直方图。
//! - 样张在 `samples/cameras/`，不入库；没有样张时返回空表，判据打一行跳过。
//!
//! # Measurement basis
//!
//! Five runs per sample, median; the preview caches are cleared before each run, so this is
//! the cold path. The timed region is `render_preview_file` only: decode, grade, framing,
//! `ColorImage` build and histogram. GPU upload, presentation and the first read from disk are
//! excluded (the file is read once beforehand so the page cache is warm). The `decode` and
//! `grade` columns are recorded by `render_preview_file` itself (`PreviewPhases`), so all three
//! segments belong to the same render rather than to a separate estimate; `rest` is the total
//! minus both. Samples live in `samples/cameras/` and are not committed, so a missing directory
//! yields an empty table and the fence prints a skip line.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::state;

/// 每个样本量几次。5 次取中位：本机解一次 24MP RAW 要 200ms 上下，再多只是让验收变慢。
///
/// Runs per sample; the median of five keeps this under a few seconds while still smoothing
/// out machine noise.
const REPS: usize = 5;

/// 一个样本的一行结果。
///
/// One measured sample.
#[derive(Clone, Debug)]
pub struct PreviewRow {
    /// 样本文件名。
    ///
    /// Sample file name.
    pub file: String,
    /// `true` = 拖动草图（首屏那条路），`false` = 全量预览。
    ///
    /// `true` for the drag sketch (the first-screen path), `false` for the full preview.
    pub sketch: bool,
    /// 端到端中位，毫秒。
    ///
    /// End-to-end median in milliseconds.
    pub median_ms: f64,
    /// 端到端最小值 / 最大值，毫秒。
    ///
    /// End-to-end min / max in milliseconds.
    pub min_ms: f64,
    pub max_ms: f64,
    /// 只解码的中位，毫秒。
    ///
    /// Decode-only median in milliseconds.
    pub decode_ms: f64,
    /// 只调色的中位，毫秒。
    ///
    /// Grade-only median in milliseconds.
    pub grade_ms: f64,
    /// 其余各步（取景、成图、直方图）。
    ///
    /// Everything else (framing, image build, histogram).
    pub rest_ms: f64,
    /// 贴图尺寸。
    ///
    /// Texture size.
    pub size: [u32; 2],
    /// 送进调色的缓冲尺寸（草图与全量在这列上就能看出差别）。
    ///
    /// Size of the buffer handed to grading; this column is where the sketch and the full
    /// preview visibly differ.
    pub src: [usize; 2],
    /// 草图解码走的路：`Some(true)` = 半尺寸快路，`Some(false)` = 整幅解码再减半，
    /// `None` = 全量行（不适用）。
    ///
    /// 这是**代码走没走快路**的事实，不受机器快慢影响；首屏判据断言的就是它。
    ///
    /// Which route the sketch decode took: `Some(true)` for the half-size fast route,
    /// `Some(false)` for a full decode plus halving, `None` for a full-preview row. A fact
    /// about the code path rather than about machine speed; the first-screen fence asserts it.
    pub half_route: Option<bool>,
}

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

/// 首屏预算用的两个样本：一台 24MP 机身的 RAW 与一张同门 JPEG。
///
/// 固定文件名而不是「目录里的第一个」：数字要能跟上一轮逐条对比，换成别的机身就
/// 没有可比性。文件不在就跳过。
///
/// The two samples the first-screen budget uses: a 24MP body's RAW and a JPEG from the same
/// line. Fixed names rather than "first in the directory", so numbers stay comparable across
/// runs; a missing file is skipped.
const SAMPLES: [&str; 2] = ["PANA0450.RW2", "PANA0198.JPG"];

/// 跑完全部样本，返回结构化结果（`--bench-preview` 与首屏预算判据共用这条路径）。
///
/// Run every sample and return structured rows; both the report and the fence go through this
/// single path, so the printed table and the assertion cannot disagree.
pub fn preview_first_screen_rows() -> Result<Vec<PreviewRow>, String> {
    let dir = sample_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let files: Vec<PathBuf> = SAMPLES
        .iter()
        .map(|name| dir.join(name))
        .filter(|p| p.is_file())
        .collect();
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let mut rows = Vec::with_capacity(files.len() * 2);
    for path in &files {
        // 先把文件读一遍：让 OS 页缓存热起来，别把「这块盘第一次读」算进首屏。
        //
        // Touch the file first so the page cache is warm; otherwise the first run measures
        // the disk rather than the pipeline.
        let _ = std::fs::read(path);
        for sketch in [true, false] {
            rows.push(measure_sample(path, sketch)?);
        }
    }
    Ok(rows)
}

/// 预览贴图的像素指纹（FNV-1a 64），判据用它钉住「草图路径没有改到全量输出」。
///
/// 指纹覆盖贴图尺寸与每个像素的四个通道——只要全量预览的像素挪了一个码值，这里就变。
///
/// FNV-1a 64 fingerprint of the preview texture's pixels, used by the fence to pin "the sketch
/// path does not touch the full output". The size and all four channels of every pixel are
/// covered, so a single moved code value changes the number.
pub fn preview_checksum(path: &Path, sketch: bool) -> Result<u64, String> {
    // 每次都从冷缓存开始：判据要问的是「这一次渲染出来的是什么」，不是「缓存里还有
    // 什么」。清完再跑，顺序无关（先草图后全量）才是一条有判别力的事实。
    //
    // Every call starts cold: the fence asks what this render produces, not what happens to
    // be cached. Only then does "sketch first, full after" become a meaningful claim.
    state::clear_preview_caches_for_bench();
    let probe = state::render_preview_for_bench(path, sketch)?;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&(probe.graded.size[0] as u32).to_le_bytes());
    eat(&(probe.graded.size[1] as u32).to_le_bytes());
    for p in &probe.graded.pixels {
        eat(&[p.r(), p.g(), p.b(), p.a()]);
    }
    Ok(h)
}

/// 量一个样本的一行。
///
/// Measure one row for one sample.
fn measure_sample(path: &Path, sketch: bool) -> Result<PreviewRow, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut total = Vec::with_capacity(REPS);
    let mut decode = Vec::with_capacity(REPS);
    let mut grade = Vec::with_capacity(REPS);
    let mut size = [0u32; 2];
    let mut src = [0usize; 2];
    let mut half_route = None;
    for _ in 0..REPS {
        state::clear_preview_caches_for_bench();
        let t0 = Instant::now();
        let probe = state::render_preview_for_bench(path, sketch)?;
        total.push(t0.elapsed());
        decode.push(probe.phases.decode);
        grade.push(probe.phases.grade);
        size = probe.size;
        src = probe.phases.src;
        half_route = probe.sketch_half;
    }

    let row = PreviewRow {
        file: name,
        sketch,
        median_ms: median_ms(&total),
        min_ms: min_of(&total),
        max_ms: max_of(&total),
        decode_ms: median_ms(&decode),
        grade_ms: median_ms(&grade),
        rest_ms: 0.0,
        size,
        src,
        half_route,
    };
    Ok(PreviewRow {
        rest_ms: (row.median_ms - row.decode_ms - row.grade_ms).max(0.0),
        ..row
    })
}

fn median_ms(v: &[Duration]) -> f64 {
    let mut ms: Vec<f64> = v.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.total_cmp(b));
    if ms.is_empty() {
        return 0.0;
    }
    let mid = ms.len() / 2;
    if ms.len().is_multiple_of(2) {
        (ms[mid - 1] + ms[mid]) / 2.0
    } else {
        ms[mid]
    }
}

fn min_of(v: &[Duration]) -> f64 {
    v.iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .fold(f64::INFINITY, f64::min)
}

fn max_of(v: &[Duration]) -> f64 {
    v.iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .fold(0.0f64, f64::max)
}

/// 把结构化结果排成给人看的表（`--bench-preview` 打印它）。
///
/// Format the rows into the table `--bench-preview` prints.
pub fn preview_first_screen() -> Result<String, String> {
    let rows = preview_first_screen_rows()?;
    if rows.is_empty() {
        return Ok(
            "首屏预览基准：没有样张（samples/cameras/ 不入库），跳过。\n\
             First-screen preview bench: no samples (samples/cameras/ is not committed), skipped.\n"
                .to_owned(),
        );
    }
    let mut out = String::from(
        "首屏预览（清缓存后 5 次取中位；单位 ms，端到端 = 解码 + 调色 + 取景/成图/直方图）\n\
         口径：仅 render_preview_file 的 CPU 墙钟，不含 GPU 上传与呈现，文件页缓存已预热。\n\
         路：草图的解码走了半尺寸快路（快路）还是整幅解码再减半（兜底）——与机器快慢无关。\n",
    );
    for row in &rows {
        out.push_str(&format!(
            "  {:<14} {:<4} {:<6} 端到端 {:7.1}（{:6.1}…{:6.1}）  解码 {:6.1}  调色 {:5.1}  其余 {:5.1}  源 {}×{}  贴图 {}×{}\n",
            row.file,
            if row.sketch { "草图" } else { "全量" },
            match row.half_route {
                Some(true) => "快路",
                Some(false) => "兜底",
                None => "—",
            },
            row.median_ms,
            row.min_ms,
            row.max_ms,
            row.decode_ms,
            row.grade_ms,
            row.rest_ms,
            row.src[0],
            row.src[1],
            row.size[0],
            row.size[1]
        ));
    }
    Ok(out)
}
