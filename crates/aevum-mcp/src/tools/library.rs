//! 素材库域：扫描目录里的序列、按指标筛选并抽取。
//!
//! 边界：管「磁盘上有哪些序列、哪些帧该留下」，不渲染也不测光（测光读的是 `sequence`
//! 已经算好的那份账）。扫描结果 `FoundSeq` 也被批量队列复用，所以它和 `scan_depth` /
//! `scan_sequences` 一起对 `queue` 可见（`pub(super)`），但不对外。
//!
//! The library domain: scanning directories for sequences, and culling/extracting by metric.
//!
//! Boundary: which sequences exist on disk and which frames are worth keeping. It neither
//! renders nor measures (measurement is the ledger `sequence` already computed). The scan
//! result `FoundSeq` is reused by the batch queue, so it is visible to `queue`
//! (`pub(super)`) together with `scan_depth` / `scan_sequences`, but not outside.

use std::fs;
use std::path::{Path, PathBuf};

use aevum_core::errmsg::clip_echo;
use aevum_core::sequence::{Sequence, SequenceError, is_supported_extension};
use serde_json::{Value, json};

use super::shared::{
    absolute, arg_str, echo_path, opt_bool, opt_f64, opt_u32, require_batch_confirm,
};
use crate::session::Session;

/// 筛选评估：按四条判据逐帧裁决（曝光越界 / 相邻突变 / 清晰度 / 读不出来）。
///
/// 与 GUI 的 `cull.evaluate` 同判据、同测光来源：两边都走
/// `measures::measure_sequence`（先命中盘上 `<序列根>/.aevum/measures.json`，512 长边）。
/// GUI 已经测过的序列，无头这边直接读账，不再解一遍图；无头测过的序列，GUI 打开时
/// 同样命中。缓存整份读写失败会静默退化成全量现测，不影响裁决。
/// GUI 的 `apply` 是把判废帧标成「排除」的会话态，无头会话没有旗标存储，这里只评估
/// 不标记；剔除走 `aevum_cull_extract` 的 `exclude` 显式列帧下标。
///
/// Cull evaluation: judge every frame with the four rules (exposure band / adjacent
/// jumps / sharpness / unreadable).
///
/// Same rules and the same metering source as the GUI's `cull.evaluate`: both go
/// through `measures::measure_sequence` (hitting `<sequence root>/.aevum/measures.json`
/// first, 512 long side). A sequence the GUI already metered is read from that ledger
/// headless-side instead of being decoded again, and a sequence metered headless is hit
/// by the GUI when it opens. A failed cache read or write degrades silently to a full
/// re-measure and never changes a verdict. The GUI's `apply` marks rejected frames
/// "excluded" in session state, which a headless session does not store -- evaluation
/// only here; exclusion happens through `aevum_cull_extract`'s explicit `exclude` index
/// list.
pub(super) fn cull_evaluate(session: &mut Session, args: &Value) -> Result<Value, String> {
    session.ensure_metrics()?;
    let metrics = session.metrics.as_ref().expect("ensure_metrics 刚灌过测光");
    let means: Vec<Option<f64>> = metrics.iter().map(|m| m.map(|x| x.mean)).collect();
    let sharp: Vec<Option<f64>> = metrics.iter().map(|m| m.map(|x| x.sharp)).collect();
    let mut rule = aevum_core::cull::CullRule::default();
    if args
        .get("suggest")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let (lo, hi) = aevum_core::cull::suggest_band(&means);
        rule.ev_low = lo;
        rule.ev_high = hi;
    }
    if let Some(v) = opt_f64(args, "ev_low")? {
        rule.ev_low = v;
    }
    if let Some(v) = opt_f64(args, "ev_high")? {
        rule.ev_high = v;
    }
    if let Some(v) = opt_f64(args, "jump_ev")? {
        rule.jump_ev = v;
    }
    if let Some(v) = opt_f64(args, "sharp_ratio")? {
        rule.sharp_ratio = v.clamp(0.0, 1.0);
    }
    if rule.ev_low > rule.ev_high {
        return Err(format!(
            "曝光下限 {} 不该高于上限 {}",
            rule.ev_low, rule.ev_high
        ));
    }
    let verdict = aevum_core::cull::evaluate(&means, &sharp, &rule);
    let rejected = verdict.rejected_indices();
    Ok(json!({
        "band": [rule.ev_low, rule.ev_high],
        "rule": {
            "ev_low": rule.ev_low,
            "ev_high": rule.ev_high,
            "jump_ev": rule.jump_ev,
            "sharp_ratio": rule.sharp_ratio,
        },
        "rejected": rejected.len(),
        "indices": rejected,
        "jumps": verdict
            .jumps
            .iter()
            .map(|(i, step)| json!({"index": i, "step_ev": step}))
            .collect::<Vec<Value>>(),
        "frames": verdict
            .frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                json!({
                    "index": i,
                    "reject": f.reject,
                    "reason": f.reason.map(|r| r.key()),
                })
            })
            .collect::<Vec<Value>>(),
    }))
}

/// 剔除成子序列：把保留下来的帧复制成一条新序列。
///
/// 与 GUI 的 `cull.extract` 同口径（路径安全检查先行、复制保留 mtime、测光缓存
/// 一并带走）；GUI 的「没被拒收的帧」读的是会话旗标，无头没有旗标，改用显式
/// `exclude`（帧下标数组，通常来自 `aevum_cull_evaluate` 的 `indices`）。
/// `dry_run: true` 只报帧数与体积，不写任何东西；非 dry_run 必须 confirm。
///
/// Extract a sub-sequence: copy the kept frames into a new folder.
///
/// Same convention as the GUI's `cull.extract` (path-safety first, mtimes preserved,
/// the metering cache carried along). The GUI's "frames not rejected" reads session
/// flags, which headless lacks, so an explicit `exclude` index list takes its place
/// (usually the `indices` reported by `aevum_cull_evaluate`). `dry_run: true` only
/// reports the frame count and size, writing nothing; anything else needs confirm.
pub(super) fn cull_extract(session: &mut Session, args: &Value) -> Result<Value, String> {
    let seq = session.require_seq()?;
    if seq.is_empty() {
        return Err("序列里没有帧".to_owned());
    }
    let target = args
        .get("target")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "target 要是目标目录的绝对路径".to_owned())?;
    let target = PathBuf::from(target);
    let dry_run = opt_bool(args, "dry_run")?.unwrap_or(false);
    if !dry_run {
        require_batch_confirm(args, "aevum_cull_extract")?;
    }
    let root = session.require_root()?;
    // 路径安全先行：源内部、盘根、非空目录都在写任何东西之前拒绝。
    //
    // Path safety first: inside the source, a drive root and a non-empty folder are
    // all refused before anything is written.
    aevum_core::cull::check_target(&root, &target).map_err(|e| e.cn().to_owned())?;
    let paths = seq.paths();
    let exclude: Vec<usize> = match args.get("exclude") {
        None => Vec::new(),
        Some(list) => list
            .as_array()
            .ok_or_else(|| "exclude 要是帧下标数组，例如 [0,17]".to_owned())?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|i| ((i as usize) < paths.len()).then_some(i as usize))
                    .ok_or_else(|| {
                        format!(
                            "exclude 帧下标越界：{}（序列共 {} 帧）",
                            clip_echo(&v.to_string()),
                            paths.len()
                        )
                    })
            })
            .collect::<Result<_, _>>()?,
    };
    let kept: Vec<PathBuf> = (0..paths.len())
        .filter(|i| !exclude.contains(i))
        .map(|i| paths[i].clone())
        .collect();
    if kept.is_empty() {
        return Err("每一帧都被剔除了：没有可保留的帧".to_owned());
    }
    if dry_run {
        let plan = aevum_core::cull::plan_extract(&kept);
        return Ok(json!({
            "dry_run": true,
            "frames": plan.kept.len(),
            "bytes": plan.bytes,
            "excluded": paths.len() - plan.kept.len(),
            "target": target.display().to_string(),
        }));
    }
    let bytes = aevum_core::cull::extract(&root, &kept, &target).map_err(|e| match e {
        aevum_core::cull::ExtractError::Target(t) => t.cn().to_owned(),
        aevum_core::cull::ExtractError::Io(e) => format!("复制失败：{e}"),
    })?;
    Ok(json!({
        "copied": kept.len(),
        "excluded": paths.len() - kept.len(),
        "bytes": bytes,
        "target": target.display().to_string(),
    }))
}

/// 扫到的一条序列。
///
/// One sequence found by the scan.
pub(super) struct FoundSeq {
    pub(super) root: PathBuf,
    name: String,
    frames: usize,
    first: Option<String>,
    last: Option<String>,
}

/// 枚举目录下的序列：根目录自己 + 往下 `depth` 层子目录，`Sequence::open` 认得的就是。
///
/// 隐藏目录（`.` 开头——序列自己的 `.aevum` 就在里面）直接跳过；一旦某个目录能开成
/// 序列就不再往里钻（序列的父目录里是帧，不是序列）。第二项返回「看着像序列却读不出来」
/// 的目录与原因：有图但解不开值得说出来，纯空文件夹不值得。
///
/// Enumerate sequences under a directory: the root itself plus subdirectories down to
/// `depth` levels; whatever `Sequence::open` accepts counts as one.
///
/// Hidden directories (leading `.` -- where a sequence keeps its own `.aevum`) are skipped
/// outright, and once a directory opens as a sequence the walk does not descend into it (the
/// children are frames, not sequences). The second value lists directories that *looked* like
/// sequences but would not read, with the reason: "there are images but they will not open"
/// deserves saying, while an empty folder does not.
pub(super) fn scan_sequences(root: &Path, depth: usize) -> (Vec<FoundSeq>, Vec<(PathBuf, String)>) {
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    scan_into(root, depth, &mut found, &mut skipped);
    found.sort_by(|a, b| a.root.cmp(&b.root));
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    (found, skipped)
}

fn scan_into(
    dir: &Path,
    depth: usize,
    found: &mut Vec<FoundSeq>,
    skipped: &mut Vec<(PathBuf, String)>,
) {
    match Sequence::open(dir) {
        Ok(seq) => {
            found.push(FoundSeq {
                root: dir.to_path_buf(),
                name: seq.name(),
                frames: seq.len(),
                first: seq.frames.first().map(|f| f.path.display().to_string()),
                last: seq.frames.last().map(|f| f.path.display().to_string()),
            });
            return;
        }
        Err(SequenceError::Io(e)) => {
            skipped.push((dir.to_path_buf(), format!("读取目录失败：{e}")));
            return;
        }
        Err(e) => {
            // 只有「确实有图」才记一笔：普通文件夹的 `Empty` 不是缺陷。
            //
            // Record only when images are actually present: `Empty` on an ordinary folder
            // is not a defect.
            if has_frame_file(dir) {
                skipped.push((dir.to_path_buf(), e.to_string()));
            }
        }
    }
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            !p.file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(false)
        })
        .collect();
    children.sort();
    for child in children {
        scan_into(&child, depth - 1, found, skipped);
    }
}

/// 目录里有没有一张「我们认的」图（不看它能不能解码）。
///
/// Whether the directory holds at least one file we recognise as an image, decodable or not.
fn has_frame_file(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.path()
                    .extension()
                    .map(|ext| is_supported_extension(&ext.to_string_lossy()))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

pub(super) fn scan_depth(args: &Value) -> Result<usize, String> {
    let depth = opt_u32(args, "depth")?.unwrap_or(1) as usize;
    if depth > 4 {
        return Err("depth 必须在 0..=4（0 = 只看这个目录自己）".to_owned());
    }
    Ok(depth)
}

/// `aevum_sequences_scan`：只看不写，不需要 confirm。
///
/// `aevum_sequences_scan`: read-only, no `confirm` needed.
pub(super) fn sequences_scan(_session: &mut Session, args: &Value) -> Result<Value, String> {
    let root = absolute(Path::new(&arg_str(args, "root")?))?;
    if !root.is_dir() {
        return Err(format!("不是文件夹：{}", echo_path(&root)));
    }
    let depth = scan_depth(args)?;
    let (found, skipped) = scan_sequences(&root, depth);
    Ok(json!({
        "root": root.display().to_string(),
        "depth": depth,
        "count": found.len(),
        "sequences": found
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "root": s.root.display().to_string(),
                    "frames": s.frames,
                    "first": s.first,
                    "last": s.last,
                })
            })
            .collect::<Vec<_>>(),
        "skipped": skipped
            .iter()
            .map(|(dir, why)| json!({"root": dir.display().to_string(), "reason": why}))
            .collect::<Vec<_>>(),
        "note": "skipped 是「目录里有图但读不出来」的条目；空文件夹不算。depth=0 只看这个目录自己。",
    }))
}
