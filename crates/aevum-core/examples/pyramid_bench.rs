//! 金字塔融合的内存量具：8 帧 24 MP 融合的**峰值堆增量**、墙钟与逐位指纹。
//!
//! 为什么要有它：`pyramid.rs` 模块头的内存数字（1.54 GB 融合增量）来自 2026-11-03 的
//! 一次性探针，探针没进仓库。没有量具，任何「省内存」的改动都只能拿进程工作集猜——
//! 而工作集混着解码器余量、分配器碎片和操作系统的说法，看不出「本法自己用了多少」。
//!
//! 这里换一个口径：进程内装一个**计数分配器**，直接量 Rust 堆的 `current` / `peak`。
//! 融合开始前记一次基线，峰值减去基线就是**融合增量**（模块头那张表的第三列）。
//! 分配器看不见 `mmap`、栈和驱动侧内存，所以它比工作集**小**；两个数字不通用，
//! 引用时必须说清是哪一个。
//!
//! 用法：
//!
//! ```text
//! cargo run --release -p aevum-core --example pyramid_bench
//! cargo run --release -p aevum-core --example pyramid_bench -- --width 6000 --height 4000 \
//!     --frames 8 --radius 2 --threshold 0 --levels 0 --repeat 3
//! ```
//!
//! 输出里的 `fnv` 是逐位指纹（把每个 f32 的 `to_bits()` 按 FNV-1a 滚一遍），
//! 改实现前后必须**完全相同**——`T10_DUMP` 那 288 MB 的逐字节 `cmp` 在仓库里没有夹具，
//! 这个指纹就是它的轻量替身。`sum` 是同一批像素的 f64 和，用来在指纹不同时判断
//! 「差多少」，不是判据。
//!
//! # Pyramid fusion memory gauge: peak heap delta, wall clock and a bitwise fingerprint
//!
//! Why it exists: the memory numbers in `pyramid.rs`'s module header (1.54 GB fusion delta)
//! came from a one-off probe on 2026-11-03 that never entered the repository. Without a
//! gauge, any "save memory" change can only be guessed from the process working set -- which
//! mixes in decoder slack, allocator fragmentation and whatever the OS feels like counting,
//! so it cannot separate "what this algorithm uses" from everything else.
//!
//! Different instrument here: a **counting allocator** inside the process measures the Rust
//! heap's `current` and `peak` directly. A baseline is taken right before fusion, and peak
//! minus baseline is the **fusion delta** (third column of the module header's table). The
//! allocator cannot see `mmap`, stack or driver-side memory, so its number is **smaller**
//! than the working set; the two are not interchangeable and any citation must say which.
//!
//! `fnv` in the output is a bitwise fingerprint (FNV-1a over every f32's `to_bits()`).
//! It must be **identical** before and after an implementation change -- there is no fixture
//! in the repository for the 288 MB byte-for-byte `cmp` that `T10_DUMP` did, so this
//! fingerprint stands in for it. `sum` is a f64 sum of the same pixels, useful for judging
//! "how far off" when the fingerprint differs, and is not a criterion.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use aevum_core::focus::Options;
use aevum_core::pyramid;

/// 计数分配器：只统计「现在用了多少」与「最高用过多少」。
///
/// A counting allocator: tracks only "how much is live now" and "how much was ever live".
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

impl Counting {
    /// 记账：`delta > 0` 是分配，`delta < 0` 是释放。
    ///
    /// Bookkeeping: `delta > 0` allocates, `delta < 0` releases.
    fn add(delta: isize) {
        let now = if delta >= 0 {
            LIVE.fetch_add(delta as usize, Ordering::Relaxed) + delta as usize
        } else {
            LIVE.fetch_sub((-delta) as usize, Ordering::Relaxed) - (-delta) as usize
        };
        // 只升不降：峰值一旦被超过就抬高。
        //
        // Monotone: the peak only rises when exceeded.
        PEAK.fetch_max(now, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            Counting::add(layout.size() as isize);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        Counting::add(-(layout.size() as isize));
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            Counting::add(new_size as isize - layout.size() as isize);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// 命令行参数。
///
/// Command-line arguments.
struct Args {
    width: usize,
    height: usize,
    frames: usize,
    levels: usize,
    radius: usize,
    threshold: f32,
    repeat: usize,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            width: 6000,
            height: 4000,
            frames: 8,
            levels: 0,
            radius: 2,
            threshold: 0.0,
            repeat: 3,
        }
    }
}

/// 解析 `--key value` 形式的参数；不认识的键直接报错，别让人以为生效了。
///
/// Parses `--key value` pairs; an unknown key is an error so nobody assumes it took effect.
fn parse_args() -> Args {
    let mut a = Args::default();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let key = argv[i].as_str();
        let val = || {
            argv.get(i + 1)
                .unwrap_or_else(|| panic!("{key} 缺一个值 / {key} needs a value"))
                .clone()
        };
        match key {
            "--width" => a.width = val().parse().expect("--width 要整数"),
            "--height" => a.height = val().parse().expect("--height 要整数"),
            "--frames" => a.frames = val().parse().expect("--frames 要整数"),
            "--levels" => a.levels = val().parse().expect("--levels 要整数"),
            "--radius" => a.radius = val().parse().expect("--radius 要整数"),
            "--threshold" => a.threshold = val().parse().expect("--threshold 要小数"),
            "--repeat" => a.repeat = val().parse().expect("--repeat 要整数"),
            other => panic!("不认识的参数 / unknown argument: {other}"),
        }
        i += 2;
    }
    assert!(a.frames >= 1, "--frames 至少 1 / needs at least 1");
    assert!(a.repeat >= 1, "--repeat 至少 1 / needs at least 1");
    a
}

/// 确定性合成一帧：低频渐变 + 高频格子 + 每帧位移，保证各帧权重面真的不一样。
///
/// Deterministic synthetic frame: low-frequency gradient + high-frequency grid + a per-frame
/// shift, so the weight planes really differ between frames.
fn synth_frame(w: usize, h: usize, seed: u32, shift: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let sx = (x + shift) % w;
            // 整数格子在位移下移动，低频渐变不动：权重面既不是常数也不是纯噪声。
            //
            // The integer grid moves with the shift while the gradient stays put, so the
            // weight plane is neither constant nor pure noise.
            let checker = ((sx / 7) + (y / 7) + seed as usize) % 2;
            let base = 0.25 + 0.5 * (y as f32 / h as f32) + 0.2 * (x as f32 / w as f32);
            let detail = if checker == 0 { -0.18 } else { 0.18 };
            let p = (y * w + x) * 3;
            v[p] = base + detail;
            v[p + 1] = base * 0.9 + detail * 0.5;
            v[p + 2] = base * 0.8 - detail * 0.5;
        }
    }
    v
}

/// 逐位指纹：每个 f32 的位模式按 FNV-1a 滚一遍。
///
/// Bitwise fingerprint: FNV-1a over each f32's bit pattern.
fn fingerprint(v: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for f in v {
        for b in f.to_bits().to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() {
    let a = parse_args();
    let px = a.width * a.height;
    println!(
        "frames={} size={}x{} ({:.1} MP) radius={} threshold={} levels={}(0=auto) repeat={}",
        a.frames,
        a.width,
        a.height,
        px as f64 / 1e6,
        a.radius,
        a.threshold,
        a.levels,
        a.repeat
    );

    let frames: Vec<Vec<f32>> = (0..a.frames)
        .map(|f| synth_frame(a.width, a.height, f as u32, f * 5))
        .collect();
    let inputs: usize = frames.iter().map(|f| f.len() * 4).sum();
    println!(
        "输入驻留 / input resident: {:.0} MiB ({} 帧 × {:.0} MiB)",
        mib(inputs),
        a.frames,
        mib(inputs / a.frames)
    );
    let refs: Vec<&[f32]> = frames.iter().map(|f| f.as_slice()).collect();

    let mut opts = Options::default();
    opts.method = aevum_core::focus::Method::Pyramid;
    opts.radius = a.radius;
    opts.threshold = a.threshold;
    opts.levels = a.levels;

    let mut times = Vec::with_capacity(a.repeat);
    let mut peaks = Vec::with_capacity(a.repeat);
    let mut prints = std::collections::BTreeSet::new();
    let mut last_sum = 0.0f64;
    for r in 0..a.repeat {
        let base = LIVE.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let t = Instant::now();
        let out = pyramid::fuse_pyramid(&refs, a.width, a.height, &opts);
        let dt = t.elapsed().as_secs_f64();
        let peak = PEAK.load(Ordering::Relaxed);
        times.push(dt);
        peaks.push(peak.saturating_sub(base));
        prints.insert(fingerprint(&out));
        last_sum = out.iter().map(|&v| v as f64).sum();
        println!(
            "  run {}: {:.2} s · 峰值堆 {:.0} MiB · 融合增量 {:.0} MiB · 指纹 {:016x}",
            r + 1,
            dt,
            mib(peak),
            mib(peak.saturating_sub(base)),
            prints.iter().next().copied().unwrap_or(0)
        );
        drop(out);
    }

    times.sort_by(|x, y| x.partial_cmp(y).unwrap());
    peaks.sort_unstable();
    let med_t = times[times.len() / 2];
    let med_p = peaks[peaks.len() / 2];
    println!(
        "中位 / median: {:.2} s · {:.0} ms/帧 · 融合增量 {:.0} MiB · {:.2} MiB/MP",
        med_t,
        med_t * 1000.0 / a.frames as f64,
        mib(med_p),
        mib(med_p) / (px as f64 / 1e6)
    );
    println!("指纹 / fingerprint: {:?}", prints);
    println!("像素和 / sum: {last_sum:.6}");
    if prints.len() != 1 {
        // 同一个输入跑多轮结果不同，说明实现里有不确定性——这比数字本身更值得先看。
        //
        // Different results across identical runs mean the implementation is
        // non-deterministic, which matters more than the numbers themselves.
        println!("⚠ 多轮输出不一致：实现存在不确定性 / runs disagree: non-deterministic");
    }
}
