//! 逐帧测光结果的磁盘缓存：`<序列根>/.aevum/measures.json`。
//!
//! 打开序列要给每一帧解一次码（512 长边指标 + 320 长边线性测光），一段千帧延时
//! 就是几分钟。这些数只由文件内容决定，所以按 `(mtime, len)` 记账：没变的帧直接
//! 读缓存，变过的帧重测并覆盖。缓存整份读写失败都退化成「全量重测」，不报错——
//! 它只是加速层，不该让序列打不开。
//!
//! 不做裁剪：单张照片打开时根目录就是它所在的文件夹，裁剪会把整册序列的账一起抹掉。
//!
//! On-disk cache of per-frame metering results: `<序列根>/.aevum/measures.json`.
//!
//! Opening a sequence decodes every frame once (512-long-side metrics plus 320-long-side
//! linear metering), which costs minutes on a thousand-frame timelapse. These numbers depend
//! only on file content, so entries are keyed by `(mtime, len)`: unchanged frames read the
//! cache directly, changed frames are measured again and overwritten. A failure to read or
//! write the whole cache degrades to a full re-measure without an error — it is only an
//! acceleration layer and must not keep a sequence from opening.
//!
//! No trimming: when a single photo is opened the root is its containing folder, and trimming
//! would wipe the ledger of the entire sequence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::deflicker::FrameMeans;
use crate::metrics::{self, FrameMetrics, MeasurePass};

/// 记账格式变了就换版本号，旧缓存整份作废。
const VERSION: u32 = 1;

/// 缓存文件名；测试与迁移脚本按这个名字找。
const FILE: &str = "measures.json";

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    /// 指标长边。换档位后旧数不能复用，整份作废。
    side: u32,
    frames: BTreeMap<String, Entry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    mtime: u64,
    len: u64,
    mean: f64,
    median: f64,
    center: f64,
    sharp: f64,
    luma: f64,
    rgb: [f64; 3],
}

impl Entry {
    fn new(stamp: (u64, u64), m: &FrameMetrics, d: &FrameMeans) -> Self {
        Self {
            mtime: stamp.0,
            len: stamp.1,
            mean: m.mean,
            median: m.median,
            center: m.center,
            sharp: m.sharp,
            luma: d.luma,
            rgb: d.rgb,
        }
    }

    fn matches(&self, stamp: (u64, u64)) -> bool {
        self.mtime == stamp.0 && self.len == stamp.1
    }

    fn metrics(&self) -> FrameMetrics {
        FrameMetrics {
            mean: self.mean,
            median: self.median,
            center: self.center,
            sharp: self.sharp,
        }
    }

    fn means(&self) -> FrameMeans {
        FrameMeans {
            luma: self.luma,
            rgb: self.rgb,
        }
    }
}

/// `<root>/.aevum/measures.json`（护带目录尚未迁移时落在旧目录里）
///
/// `<root>/.aevum/measures.json` (it lands in the legacy directory while the guard directory
/// has not been migrated yet).
pub fn cache_path(root: &Path) -> PathBuf {
    crate::project::guard_dir(root).join(FILE)
}

/// 缓存键优先用相对序列根的路径：素材整册搬走后，账还能跟着走。
fn cache_key(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => path.to_string_lossy().into_owned(),
    }
}

fn stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((since.as_nanos() as u64, meta.len()))
}

fn load(root: &Path, long_side: u32) -> BTreeMap<String, Entry> {
    let Ok(text) = std::fs::read_to_string(cache_path(root)) else {
        return BTreeMap::new();
    };
    match serde_json::from_str::<CacheFile>(&text) {
        Ok(c) if c.version == VERSION && c.side == long_side => c.frames,
        _ => BTreeMap::new(),
    }
}

/// 落盘：先写临时文件，再**原子替换**。
///
/// `std::fs::rename` 在 Windows 上走 `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)`，
/// **能覆盖已存在的目标**——本机实测：目标内容被换成源内容、源消失、返回 `Ok(())`。
/// 早先那句注释「Windows 的 rename 不能覆盖已存在的目标」是错的，于是代码先
/// `remove_file` 再 `rename`，两步之间留下一个「文件不存在」的窗口：读者一边
/// （`cull::extract` 携带测光缓存）`is_file()` 通过、`copy()` 撞上窗口就报
/// `os error 2`，整次导出失败——一个可选缓存把导出拖下水。
///
/// 现在只 rename，覆盖交给它，窗口消失。失败重试一次（10 ms 后）：真正的失败源是
/// 共享冲突——有读者正握着这个文件（Windows 不允许替换被打开的文件），等一会儿
/// 通常就过了。仍失败就放弃：缓存是加速层，丢了只意味着下一次全量重测。
///
/// Persist: write a temp file, then **replace atomically**.
///
/// `std::fs::rename` uses `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)` on Windows and
/// **does overwrite an existing target** -- measured locally: the target takes the source's
/// contents, the source disappears, and it returns `Ok(())`. The old note claiming otherwise
/// was wrong, so the code did `remove_file` and then `rename`, leaving a "file does not
/// exist" window in between: a reader (`cull::extract` carrying the metering cache) passed
/// `is_file()`, hit that window in `copy()` and reported `os error 2`, failing the whole
/// export -- an optional cache sinking the export.
///
/// Now it only renames, and the window is gone. A failure is retried once 10 ms later: the
/// real failure mode is a sharing violation -- a reader holds the file open and Windows
/// refuses to replace an open file -- which usually clears after a moment. A second failure
/// is abandoned: the cache is an acceleration layer, and losing it only costs one full
/// re-measure.
fn save(root: &Path, long_side: u32, frames: BTreeMap<String, Entry>) {
    let path = cache_path(root);
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(text) = serde_json::to_string(&CacheFile {
        version: VERSION,
        side: long_side,
        frames,
    }) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_err() {
        return;
    }
    if std::fs::rename(&tmp, &path).is_ok() {
        return;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
    if std::fs::rename(&tmp, &path).is_err() {
        // 两次都替换不了（多半是有读者一直握着它）：放弃这一份，别把 .tmp 留在护带目录里。
        //
        // Two failed replacements (likely a long-held reader): drop this copy, and do not
        // leave the .tmp behind in the guard directory.
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `metrics::measure_many` 的带缓存版本，返回的形状与它完全一致。
///
/// Cached version of `metrics::measure_many`, with exactly the same return shape.
pub fn measure_sequence(root: &Path, paths: &[PathBuf], long_side: u32) -> MeasurePass {
    measure_sequence_run(root, paths, long_side, true)
}

/// 同上，但忽略账上已有的数，全部现测（`aevum_measure --refresh` 用）。
///
/// 现测的数照旧合并写回：只覆盖这几帧的条目，别的帧的账不动。存在的理由是缓存键
/// 是 `(mtime, len)`——内容变了而这两个数没变的帧（从备份还原、工具原地改写不改
/// 时间戳）在带缓存的口径下永远读旧数，得有一条能强制重算的路。
///
/// Same as above but ignores the numbers already on the ledger and measures every frame
/// (used by `aevum_measure --refresh`).
///
/// The fresh numbers are still merged back: only the entries for these frames are
/// overwritten and other frames' entries are left alone. It exists because the key is
/// `(mtime, len)`: a frame whose content changed while both stayed the same (restored
/// from a backup, rewritten in place by a tool that preserves timestamps) would otherwise
/// read stale numbers forever, so there has to be a way to force a recompute.
pub fn measure_sequence_fresh(root: &Path, paths: &[PathBuf], long_side: u32) -> MeasurePass {
    measure_sequence_run(root, paths, long_side, false)
}

fn measure_sequence_run(
    root: &Path,
    paths: &[PathBuf],
    long_side: u32,
    read_cache: bool,
) -> MeasurePass {
    let mut cached = load(root, long_side);
    let stamps: Vec<Option<(u64, u64)>> = paths.iter().map(|p| stamp(p)).collect();
    let keys: Vec<String> = paths.iter().map(|p| cache_key(root, p)).collect();

    let mut metrics: Vec<Option<FrameMetrics>> = vec![None; paths.len()];
    let mut means: Vec<Option<FrameMeans>> = vec![None; paths.len()];
    let mut missing = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        match (stamps[i], cached.get(key)) {
            (Some(st), Some(e)) if read_cache && e.matches(st) => {
                metrics[i] = Some(e.metrics());
                means[i] = Some(e.means());
            }
            _ => missing.push(i),
        }
    }

    let fresh: Vec<_> = missing
        .par_iter()
        .map(|&i| (i, metrics::measure_file_def(&paths[i], long_side)))
        .collect();

    let mut issues = Vec::new();
    let mut touched = false;
    for (i, r) in fresh {
        match r {
            Ok((m, d)) => {
                metrics[i] = Some(m);
                means[i] = Some(d);
                if let Some(st) = stamps[i] {
                    cached.insert(keys[i].clone(), Entry::new(st, &m, &d));
                }
                touched = true;
            }
            Err(e) => issues.push((i, format!("{e}"))),
        }
    }
    issues.sort_by_key(|(i, _)| *i);

    if touched {
        save(root, long_side, cached);
    }
    (metrics, means, issues)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("aevum-meas-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_frame(path: &Path, w: u32, h: u32, rgb: [u8; 3]) {
        RgbImage::from_pixel(w, h, Rgb(rgb)).save(path).unwrap();
    }

    #[test]
    fn second_open_reads_cache_instead_of_decoding() {
        let dir = temp_dir("hit");
        let paths: Vec<PathBuf> = (0..3).map(|i| dir.join(format!("f{i:02}.png"))).collect();
        for (i, p) in paths.iter().enumerate() {
            write_frame(p, 24, 16, [i as u8 * 60, 90, 30]);
        }

        let (m1, d1, issues) = measure_sequence(&dir, &paths, 512);
        assert!(issues.is_empty());
        assert!(m1.iter().all(Option::is_some) && d1.iter().all(Option::is_some));

        // 把账上的锐度改成不可能算得出来的数，再开一次：能读到它，说明没重新解码。
        let text = std::fs::read_to_string(cache_path(&dir)).unwrap();
        let mut file: serde_json::Value = serde_json::from_str(&text).unwrap();
        for (_, e) in file["frames"].as_object_mut().unwrap() {
            e["sharp"] = serde_json::json!(12345.0);
        }
        std::fs::write(cache_path(&dir), file.to_string()).unwrap();

        let (m2, _, issues) = measure_sequence(&dir, &paths, 512);
        assert!(issues.is_empty());
        assert_eq!(m2.len(), 3);
        for m in &m2 {
            assert_eq!(m.unwrap().sharp, 12345.0);
        }
    }

    #[test]
    fn fresh_ignores_the_ledger_but_still_merges_into_it() {
        let dir = temp_dir("fresh");
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        write_frame(&a, 24, 16, [30, 30, 30]);
        write_frame(&b, 24, 16, [200, 200, 200]);
        let (_, _, issues) = measure_sequence(&dir, std::slice::from_ref(&a), 512);
        assert!(issues.is_empty());

        // 先量 b，账上有它；再把 a 的账面锐度改成算不出来的数。
        let (_, _, issues) = measure_sequence(&dir, std::slice::from_ref(&b), 512);
        assert!(issues.is_empty());
        let text = std::fs::read_to_string(cache_path(&dir)).unwrap();
        let mut file: serde_json::Value = serde_json::from_str(&text).unwrap();
        file["frames"]["a.png"]["sharp"] = serde_json::json!(12345.0);
        std::fs::write(cache_path(&dir), file.to_string()).unwrap();

        // 带缓存读：读回那笔假账（前提成立，否则这条判据测不到东西）。
        let (cached, _, _) = measure_sequence(&dir, &[a.clone(), b.clone()], 512);
        assert_eq!(cached[0].unwrap().sharp, 12345.0);

        // 强制重算：a 被现测覆盖，b 的账照旧保留。
        let (fresh, _, issues) = measure_sequence_fresh(&dir, &[a, b], 512);
        assert!(issues.is_empty());
        assert_ne!(fresh[0].unwrap().sharp, 12345.0, "refresh 没绕过账上旧数");
        let file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(cache_path(&dir)).unwrap()).unwrap();
        assert_ne!(file["frames"]["a.png"]["sharp"], 12345.0);
        assert!(
            file["frames"]["b.png"]["sharp"].is_number(),
            "refresh 把别的帧的账抹掉了"
        );
    }

    #[test]
    fn changed_frame_is_measured_again() {
        let dir = temp_dir("stale");
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        write_frame(&a, 24, 16, [10, 10, 10]);
        write_frame(&b, 24, 16, [10, 10, 10]);

        let (m1, _, _) = measure_sequence(&dir, &[a.clone(), b.clone()], 512);
        let before = m1[1].unwrap().mean;

        // 换一张不同尺寸的亮图：mtime 与长度都变，必须重测。
        write_frame(&b, 32, 32, [250, 250, 250]);
        let (m2, _, issues) = measure_sequence(&dir, &[a, b], 512);
        assert!(issues.is_empty());
        assert!(
            m2[1].unwrap().mean > before + 0.5,
            "改过的帧没重测：{} → {}",
            before,
            m2[1].unwrap().mean
        );
    }

    #[test]
    fn side_change_voids_the_cache() {
        let dir = temp_dir("side");
        let a = dir.join("a.png");
        write_frame(&a, 64, 48, [80, 80, 80]);

        let (m1, _, _) = measure_sequence(&dir, std::slice::from_ref(&a), 512);
        let file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(cache_path(&dir)).unwrap()).unwrap();
        assert_eq!(file["side"], 512);
        assert!(m1[0].is_some());

        let (m2, _, issues) = measure_sequence(&dir, std::slice::from_ref(&a), 256);
        assert!(issues.is_empty());
        assert!(m2[0].is_some());
        let file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(cache_path(&dir)).unwrap()).unwrap();
        assert_eq!(file["side"], 256);
    }

    /// 后台测光线程不停重写缓存的同时，`cull::extract` 必须一轮都不失败。
    ///
    /// 这是那条偶发红的复现：缓存落盘先 `remove_file` 再 `rename` 时，两步之间有个
    /// 「文件不存在」的窗口，读者 `is_file()` 通过、`copy()` 撞窗口就报 `os error 2`，
    /// 一个可选缓存把整次导出拖下水。现在落盘只 `rename`（Windows 上能覆盖），
    /// 提取侧再对 `NotFound` 重试，两头都不该再让导出失败。
    ///
    /// 负向验证：把 `save` 的 `remove_file` 加回去、再去掉 `carry_metering_cache` 的
    /// 重试，这条必红。
    ///
    /// While the background metering thread keeps rewriting the cache, `cull::extract` must
    /// not fail a single round.
    ///
    /// This reproduces that intermittent red: when persisting did `remove_file` and then
    /// `rename`, a "file does not exist" window sat between the two, and a reader that passed
    /// `is_file()` then hit it in `copy()` reported `os error 2`, an optional cache sinking
    /// the export. Persisting now only renames (which does overwrite on Windows) and the
    /// extraction side retries `NotFound`, so neither end should fail an export again.
    ///
    /// Negative check: put `save`'s `remove_file` back and drop `carry_metering_cache`'s
    /// retry, and this goes red.
    #[test]
    fn a_concurrent_cache_write_never_fails_an_extraction() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = temp_dir("race");
        let paths: Vec<PathBuf> = (0..3).map(|i| dir.join(format!("f{i:02}.png"))).collect();
        for (i, p) in paths.iter().enumerate() {
            write_frame(p, 16, 12, [i as u8 * 80, 60, 40]);
        }
        let (_, _, issues) = measure_sequence(&dir, &paths, 512);
        assert!(issues.is_empty(), "先量一遍写出一份缓存：{issues:?}");
        assert!(
            cache_path(&dir).is_file(),
            "缓存要真的落盘，否则这条压测没意义"
        );

        let target = std::env::temp_dir().join(format!(
            "aevum-race-out-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        // 写线程：不停重写同一份缓存，正是后台测光线程干的事（不 sleep，把窗口尽量撞满）。
        //
        // The writer: keeps rewriting the same cache, exactly what the background metering
        // thread does (no sleep, to hit the window as often as possible).
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let root = dir.clone();
            let stop = Arc::clone(&stop);
            let frames = load(&dir, 512);
            std::thread::spawn(move || {
                let mut rounds = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    save(&root, 512, frames.clone());
                    rounds += 1;
                }
                rounds
            })
        };

        // 主线程：同一根上提取 100 轮，每轮都要 Ok。
        //
        // The main thread: 100 extractions on the same root, every one of them Ok.
        let mut failures: Vec<String> = Vec::new();
        for round in 0..100 {
            if let Err(e) = crate::cull::extract(&dir, &paths[..2], &target) {
                failures.push(format!("第 {round} 轮：{e:?}"));
            }
            let _ = std::fs::remove_dir_all(&target);
        }
        stop.store(true, Ordering::Relaxed);
        let rounds = writer.join().expect("写线程不该 panic");

        assert!(rounds > 0, "写线程一轮都没跑，压测没压到东西");
        assert!(
            failures.is_empty(),
            "并发重写缓存时提取失败了 {} 轮（写线程跑了 {rounds} 轮）：\n{}",
            failures.len(),
            failures.join("\n")
        );

        // 正常路径没被「跳过」逻辑顺手跳掉：缓存确实带过去了。
        //
        // The normal path was not skipped along with the retry: the cache really came over.
        crate::cull::extract(&dir, &paths[..2], &target).expect("再提取一次");
        assert!(
            cache_path(&target).is_file(),
            "缓存没带到目标序列：跳过逻辑把正常路径也跳了"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&target);
    }
}
