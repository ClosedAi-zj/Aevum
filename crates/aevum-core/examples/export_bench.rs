//! 导出分段墙钟：一条序列「解码+测光 → 渲染 → 编码」各花多久。
//!
//! # 它回答的三个问题
//!
//! 1. **1000 帧要多久**（O1）：没有量具时这个数字只能靠猜，而「要不要做流式」「要不要
//!    换编码器」都得先知道瓶颈在哪一段；
//! 2. **瓶颈在解码还是编码**（O1）：先单独跑一遍逐帧解码+测速，再跑整条导出，
//!    两段相减就是渲染与编码；
//! 3. **导出吃不吃满核**（O3）：墙钟与 CPU 时间的比值由外部采（见下），
//!    本程序只负责把墙钟与阶段边界打准。
//!
//! # 用法
//!
//! ```text
//! cargo run --release -p aevum-core --example export_bench -- --frames 60
//! cargo run --release -p aevum-core --example export_bench -- --frames 1000 --source samples/cameras/PANA0198.JPG
//! ```
//!
//! 可选：`--width 1080 --height 1920`（默认保持源尺寸）、`--fps 30`、`--crf 20`、
//! `--out <dir>`、`--clean`（跑完删掉临时序列与中间帧；默认保留，见输出里的路径）。
//!
//! CPU 利用率要外部采（本程序不引 windows-sys 只为读一个计数器）：
//!
//! ```text
//! powershell -c "1..600 | %% { $p = Get-Process aevum_export_bench -EA 0; if ($p) { $p.CPU } else { break }; Start-Sleep 1 }"
//! ```
//!
//! 首末两个 CPU 秒数之差 ÷ 墙钟 ÷ 逻辑核数 = 平均利用率。
//!
//! # 它不做什么
//!
//! 不改用户素材：源序列只读，工作文件全部写进 `--out`；不加 `--clean` 就不删任何东西
//! （在这台机器上删除会进回收站，堆起来比留着更麻烦）。
//!
//! Export wall clock by stage: decode+measure, render, encode.
//!
//! # The three questions it answers
//!
//! 1. **How long does 1000 frames take** (O1) -- a number that otherwise has to be guessed,
//!    and every "should we go streaming / swap the encoder" decision depends on knowing it;
//! 2. **Is the bottleneck decode or encode** (O1) -- decode+measure is timed separately, and
//!    subtracting it from the full export leaves render plus encode;
//! 3. **Does the export use the cores** (O3) -- wall clock and CPU time are sampled outside
//!    (see below); this program only has to report the wall clock and stage boundaries
//!    accurately.
//!
//! # Usage
//!
//! ```text
//! cargo run --release -p aevum-core --example export_bench -- --frames 60
//! ```
//!
//! Optional: `--width/--height` (default: source size), `--fps`, `--crf`, `--out`,
//! `--clean` (delete the temp sequence and intermediate frames afterwards; kept by default).
//!
//! # What it does not do
//!
//! It never touches the source: the sequence is read only and all work goes under `--out`;
//! without `--clean` nothing is deleted (on this machine deletion lands in the Recycle Bin,
//! which piles up faster than the files do).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use aevum_core::develop::Ev;
use aevum_core::export::{RenderOptions, Stage, export_video, find_ffmpeg};
use aevum_core::grade::GradeParams;

fn main() {
    let args = Args::parse();
    let source = args.source.clone().unwrap_or_else(default_source);
    assert!(
        source.is_file(),
        "源样张不存在：{}\n用 --source 指一个（这条基准要的是一张真实相机产出，不是合成靶）",
        source.display()
    );
    let Some(ffmpeg) = find_ffmpeg() else {
        panic!("找不到 ffmpeg：设 AEVUM_FFMPEG，或把 ffmpeg.exe 放进 dist/aevum/tools/");
    };

    let out_root = args.out.clone().unwrap_or_else(default_out_root);
    let seq_dir = out_root.join(format!("seq-{}", args.frames));
    let frames_dir = out_root.join("frames");
    let video = out_root.join("bench.mp4");
    let _ = std::fs::create_dir_all(&seq_dir);

    // 序列：同名的 N 份。**不是为了模拟闪烁**，是为了让「每帧都要解一张真实相机文件」
    // 这件事成立——同内容让缓存失效得更彻底（mtime 与字节数相同也会被缓存的键挡住，
    // 所以这里连文件名一起变）。
    //
    // The sequence is N copies under different names. Not to simulate flicker, but to make
    // "every frame decodes a real camera file" true; distinct names keep the thumbnail cache
    // from short-circuiting the work.
    let paths = prepare_sequence(&source, &seq_dir, args.frames);
    println!(
        "\n源：{}（{} 帧）\n工作目录：{}\n",
        source.display(),
        paths.len(),
        out_root.display()
    );

    // 第一段：解码 + 测光，走**生产同一实现**（`measures::measure_sequence`：并行、
    // 长边 512、顺带读写序列目录下的测光账）。这里不能用一段手写的串行循环代替——
    // 那种数字既不代表界面打开序列的体验，也不代表导出前的准备成本，量了也白量。
    //
    // 冷/热两种口径分开跑：冷启动是第一次打开一条新序列（每帧都要解），热启动是
    // 第二遍（账上有数，只读盘）。用户感知到的是前者，重复导出吃到的是后者。
    //
    // Stage 1: decode + meter through **the same implementation production uses**
    // (`measures::measure_sequence`: parallel, long side 512, reads and writes the metering
    // ledger next to the sequence). A hand-written serial loop here would measure something
    // that is neither "opening a sequence" nor "preparing an export".
    //
    // Cold and warm are separate runs: cold is the first open (every frame decodes), warm is
    // the second pass (the ledger has the numbers and only disk is read).
    if args.cold {
        let _ = std::fs::remove_dir_all(seq_dir.join(".aevum"));
    }
    let t = Instant::now();
    let (metrics, _means, issues) = aevum_core::measures::measure_sequence(&seq_dir, &paths, 512);
    let decode = t.elapsed();
    assert!(
        issues.is_empty(),
        "有 {} 帧测光失败，基准不可信：{:?}",
        issues.len(),
        issues.iter().take(3).collect::<Vec<_>>()
    );
    assert_eq!(metrics.len(), paths.len());

    // 第二段：整条导出。阶段边界从 progress 回调里取——它是唯一知道「渲染完了、开始编码」
    // 的地方，外部拿墙钟猜不出这个分界。
    //
    // Stage 2: the full export. The boundary comes from the progress callback -- the only
    // place that knows "rendering is done, encoding starts"; an outside observer cannot guess
    // it from wall clock alone.
    let mut options = RenderOptions::default();
    if let (Some(w), Some(h)) = (args.width, args.height) {
        options.target_size = Some((w, h));
    }
    options.fps = args.fps;
    options.crf = args.crf;
    options.out_dir = frames_dir.clone();
    options.video_out = video.clone();

    let cancel = AtomicBool::new(false);
    let frames_start = Instant::now();
    // 回调是 `&(dyn Fn + Sync)`（导出线程会在多个 worker 上并发调它），所以阶段边界
    // 只能走锁：闭包不许改它捕获的外部变量，这是签名写死的。
    //
    // The callback is `&(dyn Fn + Sync)` (export workers call it concurrently), so stage
    // boundaries have to go through a lock: a closure may not mutate captured state here.
    let frames_done: Mutex<Option<Duration>> = Mutex::new(None);
    let encode_start: Mutex<Option<Instant>> = Mutex::new(None);
    let params: Vec<GradeParams> = vec![GradeParams::default(); paths.len()];
    let gains: Vec<Ev> = vec![
        Ev {
            add: 0.0,
            chan: [0.0; 3],
        };
        paths.len()
    ];
    let offsets: Vec<[f64; 2]> = vec![[0.0, 0.0]; paths.len()];

    let result = export_video(
        &ffmpeg,
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        &cancel,
        &|p| match p.stage {
            Stage::Frames if p.done == p.total => {
                let mut g = frames_done.lock().unwrap();
                if g.is_none() {
                    *g = Some(frames_start.elapsed());
                }
            }
            Stage::Encode => {
                let mut g = encode_start.lock().unwrap();
                if g.is_none() {
                    *g = Some(Instant::now());
                }
            }
            _ => {}
        },
    );
    let total = t.elapsed();
    let video = result.unwrap_or_else(|e| panic!("导出失败：{e}"));

    let render = frames_done
        .lock()
        .unwrap()
        .unwrap_or_else(|| frames_start.elapsed());
    let encode = encode_start
        .lock()
        .unwrap()
        .map(|s| s.elapsed())
        .unwrap_or_default();

    println!("分段墙钟（{} 帧）：", paths.len());
    println!(
        "  解码+测光   {:>8.2} s  ({:>6.1} ms/帧)",
        secs(decode),
        per_frame(decode, paths.len())
    );
    println!(
        "  渲染+编码   {:>8.2} s  ({:>6.1} ms/帧)",
        secs(render),
        per_frame(render, paths.len())
    );
    println!("  管道收尾    {:>8.2} s", secs(encode));
    println!("  ─────────────────────");
    println!(
        "  合计        {:>8.2} s  ({:>6.1} ms/帧)",
        secs(total),
        per_frame(total, paths.len())
    );
    // 编码与渲染**重叠**：成片走直连管道（渲染线程写 stdin，ffmpeg 同时编码），
    // `Stage::Encode` 的进度行从编码器读到第一帧就开始了。所以这里没有「编码耗时」
    // 这个独立数字，只有「冲完管道的收尾」——写一个假的独立耗时比不写更糟。
    //
    // Encoding **overlaps** rendering: the render goes through a direct pipe (the render
    // thread writes stdin while ffmpeg encodes), so `Stage::Encode` progress starts as soon as
    // the encoder sees its first frame. There is no independent "encode time" to report, only
    // "flush the pipe" -- inventing a separate number would be worse than omitting it.
    println!("（编码走直连管道、与渲染重叠；上面那行是冲完管道的收尾，不是编码总耗时）");
    println!(
        "\n成片：{}（{:.1} MB）",
        video.display(),
        std::fs::metadata(&video)
            .map(|m| m.len() as f64 / 1e6)
            .unwrap_or(0.0)
    );
    if !args.clean {
        println!(
            "工作文件保留在 {}（`--clean` 才会删；这台机器上删除会进回收站）",
            out_root.display()
        );
    } else {
        // 只在显式要求时删，且删得掉就报一句；删不掉（被占用/权限）不算失败——
        // 这是量具，不是清理工具。
        let _ = std::fs::remove_dir_all(&seq_dir);
        let _ = std::fs::remove_dir_all(&frames_dir);
    }
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn per_frame(d: Duration, n: usize) -> f64 {
    if n == 0 {
        0.0
    } else {
        d.as_secs_f64() * 1000.0 / n as f64
    }
}

/// 用源样张的副本铺一条 N 帧序列；已存在就复用（重跑基准不再复制一遍）。
///
/// Lay out an N-frame sequence from copies of the source; reuse what is already there so a
/// rerun does not copy again.
fn prepare_sequence(source: &Path, dir: &Path, n: usize) -> Vec<PathBuf> {
    let ext = source
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "jpg".to_owned());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let p = dir.join(format!("frame_{i:05}.{ext}"));
        if !p.is_file() {
            std::fs::copy(source, &p).unwrap_or_else(|e| {
                panic!(
                    "复制样张失败（{} → {}）：{e}\n磁盘满了？一条 1000 帧的 24MP 序列要 12 GB",
                    source.display(),
                    p.display()
                )
            });
        }
        out.push(p);
    }
    out
}

/// 默认源：`samples/cameras/` 里第一张 JPG（不入库，本机没有就让用户指一个）。
fn default_source() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/cameras");
    let mut jpgs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().is_some_and(|e| {
                        matches!(e.to_string_lossy().to_lowercase().as_str(), "jpg" | "jpeg")
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    jpgs.sort();
    jpgs.into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("samples/cameras/PANA0198.JPG"))
}

/// 工作目录默认放 `E:/tmp`（本仓一贯的临时区），没有 E 盘就用系统临时目录。
fn default_out_root() -> PathBuf {
    let e = Path::new("E:/tmp");
    if e.is_dir() {
        e.join("aevum-export-bench")
    } else {
        std::env::temp_dir().join("aevum-export-bench")
    }
}

struct Args {
    frames: usize,
    source: Option<PathBuf>,
    width: Option<u32>,
    height: Option<u32>,
    fps: f64,
    crf: u8,
    out: Option<PathBuf>,
    clean: bool,
    /// 冷启动：跑之前把序列目录下的测光账删掉，保证每帧都真的解一遍。
    ///
    /// Cold start: drop the metering ledger first so every frame really decodes.
    cold: bool,
}

impl Args {
    fn parse() -> Self {
        let mut a = Self {
            frames: 60,
            source: None,
            width: None,
            height: None,
            fps: 30.0,
            crf: 20,
            out: None,
            clean: false,
            cold: true,
        };
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let mut i = 0;
        while i < argv.len() {
            let key = argv[i].as_str();
            let next = |i: &mut usize| -> String {
                *i += 1;
                argv.get(*i)
                    .unwrap_or_else(|| panic!("{key} 后面缺一个值"))
                    .clone()
            };
            match key {
                "--frames" => a.frames = next(&mut i).parse().expect("--frames 要整数"),
                "--source" => a.source = Some(PathBuf::from(next(&mut i))),
                "--width" => a.width = Some(next(&mut i).parse().expect("--width 要整数")),
                "--height" => a.height = Some(next(&mut i).parse().expect("--height 要整数")),
                "--fps" => a.fps = next(&mut i).parse().expect("--fps 要数字"),
                "--crf" => a.crf = next(&mut i).parse().expect("--crf 要整数"),
                "--out" => a.out = Some(PathBuf::from(next(&mut i))),
                "--clean" => a.clean = true,
                "--warm" => a.cold = false,
                other => panic!(
                    "不认识的参数 {other}（可用：--frames --source --width --height --fps --crf --out --clean --warm）"
                ),
            }
            i += 1;
        }
        assert!(a.frames > 0, "--frames 至少 1");
        a
    }
}
