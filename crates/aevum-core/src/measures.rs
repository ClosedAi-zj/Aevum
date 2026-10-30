//! 逐帧测光结果的磁盘缓存：`<序列根>/.aevum/measures.json`。
//!
//! 打开序列要给每一帧解一次码（512 长边指标 + 320 长边线性测光），一段千帧延时
//! 就是几分钟。这些数只由文件内容决定，所以按 `(mtime, len)` 记账：没变的帧直接
//! 读缓存，变过的帧重测并覆盖。缓存整份读写失败都退化成「全量重测」，不报错——
//! 它只是加速层，不该让序列打不开。
//!
//! 不做裁剪：单张照片打开时根目录就是它所在的文件夹，裁剪会把整册序列的账一起抹掉。

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

/// 先写临时文件再替换；Windows 的 rename 不能覆盖已存在的目标。
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
    if path.exists() && std::fs::remove_file(&path).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, &path);
}

/// `metrics::measure_many` 的带缓存版本，返回的形状与它完全一致。
pub fn measure_sequence(root: &Path, paths: &[PathBuf], long_side: u32) -> MeasurePass {
    let mut cached = load(root, long_side);
    let stamps: Vec<Option<(u64, u64)>> = paths.iter().map(|p| stamp(p)).collect();
    let keys: Vec<String> = paths.iter().map(|p| cache_key(root, p)).collect();

    let mut metrics: Vec<Option<FrameMetrics>> = vec![None; paths.len()];
    let mut means: Vec<Option<FrameMeans>> = vec![None; paths.len()];
    let mut missing = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        match (stamps[i], cached.get(key)) {
            (Some(st), Some(e)) if e.matches(st) => {
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
}
