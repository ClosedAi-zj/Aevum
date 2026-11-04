//! 批量队列域：入队、出队、跑队列、看进度、取消。
//!
//! 边界：队列本身就是「一串待渲染的活」——这里只调度，不实现渲染（那是 `export` 的
//! `ExportPlan`），也不实现扫描（那是 `library`）。`batch_run` 是唯一的例外：它把
//! 扫描 + 入队 + 立刻跑串成一次调用，因为无人值守场景要的就是这个。
//!
//! The batch queue domain: enqueue, dequeue, run, watch progress, cancel.
//!
//! Boundary: the queue is a list of pending renders -- this module schedules them. It does
//! not implement rendering (that is `export`'s `ExportPlan`) and does not implement scanning
//! (that is `library`). `batch_run` is the one exception: it chains scan, enqueue and run
//! into a single call, because that is exactly what unattended use needs.

use std::path::Path;
use std::sync::Arc;
use std::thread;

use aevum_core::export::{self, JobProgress, Stage};
use aevum_core::queue::{self, QueueItem, QueueStatus};
use serde_json::{Value, json};

use super::library::{scan_depth, scan_sequences};
use super::shared::{
    absolute, arg_str, confirm, echo_path, echo_text, opt_bool, opt_f64, opt_str, opt_u32, round6,
};
use crate::session::{JobKind, JobOutcome, JobShared, STAGE_ENCODE, STAGE_FRAMES, Session};

fn queue_json(session: &Session) -> Value {
    let items: Vec<Value> = session
        .queue
        .iter()
        .enumerate()
        .map(|(index, item)| {
            json!({
                "index": index + 1,
                "root": item.root,
                "out": item.out,
                "width": item.width,
                "height": item.height,
                "fps": round6(item.fps),
                "codec": item.codec,
                "crf": item.crf,
                "preset": item.preset,
                "keep_frames": item.keep_frames,
                "resume": item.resume,
                "copy": item.copy,
                "status": item.status.label(),
                "note": item.note,
            })
        })
        .collect();
    let done = session
        .queue
        .iter()
        .filter(|i| i.status == QueueStatus::Done)
        .count();
    let failed = session
        .queue
        .iter()
        .filter(|i| i.status == QueueStatus::Failed)
        .count();
    json!({
        "path": session.queue_path.display().to_string(),
        "count": session.queue.len(),
        "done": done,
        "failed": failed,
        "pending": session.queue.len() - done - failed,
        "items": items,
    })
}

pub(super) fn queue_list(session: &mut Session) -> Result<Value, String> {
    session.ensure_queue_loaded();
    Ok(queue_json(session))
}

pub(super) fn queue_add(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先取消或等它结束再改队列".to_owned());
    }
    session.ensure_queue_loaded();
    let root = match opt_str(args, "root")? {
        Some(text) => absolute(Path::new(text))?,
        None => session.require_root()?,
    };
    let item = queue_item_from_args(&root, args)?;
    let index = queue::add(&mut session.queue, item)?;
    session.save_queue()?;
    let mut out = queue_json(session);
    if let Some(object) = out.as_object_mut() {
        object.insert("added".to_owned(), json!(index + 1));
    }
    Ok(out)
}

/// 一份渲染规格：`queue_add` 与 `aevum_batch_run` **共用这一份校验**。
///
/// 差别只有一处：`out` 省略时都默认落在序列目录下的 `<名字>_成片.mp4`。共用不是为了
/// 省代码，是为了不让「加一条」和「加一批」对同一个参数给出两套口径——R4 的教训
/// （一条文件只许一套判断）搬到参数上就是：一个参数只许一套判断。
///
/// One render spec: `queue_add` and `aevum_batch_run` **share this single validation pass**.
///
/// The only difference is that both default `out` to `<name>_成片.mp4` inside the sequence
/// directory when it is omitted. Sharing is not about saving code: it stops "add one" and
/// "add a batch" from reaching two verdicts about the same argument -- the R4 lesson (one
/// file, one verdict) applied to arguments: one argument, one verdict.
fn queue_item_from_args(root: &Path, args: &Value) -> Result<QueueItem, String> {
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "序列".to_owned());
    let out = match opt_str(args, "out")? {
        Some(text) => absolute(Path::new(text))?,
        None => root.join(format!("{name}_成片.mp4")),
    };
    let width = opt_u32(args, "width")?.unwrap_or(0);
    let height = opt_u32(args, "height")?.unwrap_or(0);
    if (width == 0) != (height == 0) {
        return Err("width 与 height 需同时为 0（保持原生）或同时给出".to_owned());
    }
    if width > 0 && (width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1) {
        return Err(format!(
            "width/height 必须是偶数且不小于 2：{width}×{height}"
        ));
    }
    let fps = opt_f64(args, "fps")?.unwrap_or(24.0);
    if !(1.0..=240.0).contains(&fps) {
        return Err("fps 必须在 1..=240".to_owned());
    }
    let crf = opt_u32(args, "crf")?.unwrap_or(18);
    if crf > 51 {
        return Err("crf 必须在 0..51".to_owned());
    }
    let codec = match opt_str(args, "codec")?
        .unwrap_or("h265")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "h264" | "264" | "avc" => "h264",
        "h265" | "265" | "hevc" => "h265",
        other => return Err(format!("未知编码 {}；可用 h264 / h265", echo_text(other))),
    };
    let preset = opt_str(args, "preset")?
        .unwrap_or("medium")
        .trim()
        .to_owned();
    if !queue::PRESETS.contains(&preset.as_str()) {
        return Err(format!("未知编码预设 {}", echo_text(&preset)));
    }
    let keep_frames = opt_bool(args, "keep_frames")?.unwrap_or(false);
    let resume = opt_bool(args, "resume")?.unwrap_or(false);
    let copy = match opt_str(args, "copy")? {
        Some(raw) => {
            let name = raw.trim().to_owned();
            if name.is_empty() {
                return Err("copy 不能为空串；省略才表示用主调色".to_owned());
            }
            name
        }
        None => String::new(),
    };
    Ok(QueueItem {
        root: root.display().to_string(),
        out: out.display().to_string(),
        width,
        height,
        fps,
        codec: codec.to_owned(),
        crf: crf as u8,
        preset,
        keep_frames,
        resume,
        copy,
        ..QueueItem::default()
    })
}

pub(super) fn queue_remove(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先取消再改队列".to_owned());
    }
    session.ensure_queue_loaded();
    let index =
        opt_u32(args, "index")?.ok_or_else(|| "缺少 index（从 1 开始）".to_owned())? as usize;
    let len = session.queue.len();
    if index == 0 || index > len {
        return Err(format!("index 超出范围：1..={len}"));
    }
    session.queue.remove(index - 1);
    session.save_queue()?;
    Ok(json!({"removed": index, "count": session.queue.len()}))
}

pub(super) fn queue_clear(session: &mut Session) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先取消再清空".to_owned());
    }
    session.ensure_queue_loaded();
    let cleared = session.queue.len();
    session.queue.clear();
    session.save_queue()?;
    Ok(json!({"cleared": cleared, "count": 0}))
}

pub(super) fn queue_run(session: &mut Session, args: &Value) -> Result<Value, String> {
    if !confirm(args) {
        return Err("aevum_queue_run 会批量写视频，需参数 confirm: true".to_owned());
    }
    if session.is_running() {
        return Err("已有后台任务正在运行，先看 aevum_queue_status 或调用 aevum_cancel".to_owned());
    }
    session.ensure_queue_loaded();
    if session.queue.is_empty() {
        return Err("队列是空的：先用 aevum_queue_add 加入序列".to_owned());
    }
    run_queue_now(session, args)
}

/// 真正跑队列：`queue_run` 与 `aevum_batch_run` 共用，前置检查由调用方做。
///
/// Actually run the queue: shared by `queue_run` and `aevum_batch_run`, with the pre-checks
/// left to the caller.
fn run_queue_now(session: &mut Session, args: &Value) -> Result<Value, String> {
    let ffmpeg = export::find_ffmpeg()
        .ok_or_else(|| "找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned())?;
    let background = opt_bool(args, "background")?.unwrap_or(false);
    let n = session.queue.len();
    for item in &mut session.queue {
        item.status = QueueStatus::Pending;
        item.note.clear();
    }
    session.save_queue()?;
    let items = session.queue.clone();
    let shared = Arc::clone(&session.shared);
    shared.begin(JobKind::Queue, n);

    if background {
        let worker = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("aevum-queue".to_owned())
            .spawn(move || {
                let result = run_queue_job(&items, &ffmpeg, &worker);
                worker.finish(result);
            })
            .map_err(|e| {
                let message = format!("启动后台队列失败：{e}");
                shared.finish(Err(message.clone()));
                message
            })?;
        session.start_background(shared, handle);
        return Ok(json!({
            "started": true,
            "background": true,
            "items": n,
            "progress": {"done": 0, "total": n},
            "note": "用 aevum_queue_status 查询进度，aevum_queue_cancel 或 aevum_cancel 取消。"
        }));
    }

    let result = run_queue_job(&items, &ffmpeg, &shared);
    let (ok, failed, cancelled) = match &result {
        Ok(JobOutcome::Queue {
            ok,
            failed,
            cancelled,
            ..
        }) => (*ok, *failed, *cancelled),
        _ => (0, 0, false),
    };
    session.complete_job(result)?;
    let mut out = queue_json(session);
    if let Some(object) = out.as_object_mut() {
        object.insert("ok".to_owned(), json!(ok));
        object.insert("failed".to_owned(), json!(failed));
        object.insert("cancelled".to_owned(), json!(cancelled));
    }
    Ok(out)
}

pub(super) fn queue_status(session: &mut Session) -> Result<Value, String> {
    session.poll_job();
    session.ensure_queue_loaded();
    let running = session.is_running();
    let shared = &session.shared;
    let done = shared.done();
    let total = shared.total();
    let percent = if total == 0 {
        0.0
    } else {
        round6((done.min(total) as f64 / total as f64) * 100.0)
    };
    let stage = match shared.stage() {
        STAGE_FRAMES => "frames",
        STAGE_ENCODE => "encode",
        _ => "idle",
    };
    let queue = queue_json(session);
    Ok(json!({
        "running": running,
        "job": if running { shared.kind().map(JobKind::name) } else { None },
        "last_job": shared.kind().map(|k| k.name()),
        "progress": {"done": done, "total": total, "percent": percent, "stage": stage},
        "cancel_requested": shared.cancel_requested(),
        "elapsed_secs": round6(shared.elapsed_ms() as f64 / 1000.0),
        "path": session.queue_path.display().to_string(),
        "queue": queue,
        "last_error": session.last_error,
        "note": "progress.done/total 是已完成的段数；queue.items 里是每一段的最近结果。"
    }))
}

pub(super) fn queue_cancel(session: &mut Session) -> Result<Value, String> {
    session.poll_job();
    let kind = session.request_cancel();
    let was_running = kind.is_some();
    Ok(json!({
        "cancelled": was_running,
        "was_running": was_running,
        "job": kind.map(JobKind::name),
        "is_queue": kind == Some(JobKind::Queue),
        "running": session.is_running(),
        "cancel_requested": session.shared.cancel_requested(),
        "note": "取消只停住还没写完的段；已完成的段保留结果。"
    }))
}

/// `aevum_batch_run`：扫一批 → 全部入队 → 直接跑，一次调用给出逐段结果。
///
/// 无人值守场景要的就是这个：调用方给一个父目录，拿回「几段成功、几段失败、每段为什么」。
/// 默认**先清空队列**（`append: true` 才往后追加）——「跑这个目录下所有序列」如果混进
/// 上一批的残留，结果就没法看。规格先整批校验再动队列：半批入队比一批不进去更难收拾。
///
/// `aevum_batch_run`: scan a batch, enqueue all of it, run it, and return per-item results in
/// one call. That is exactly what unattended use needs: the caller passes a parent directory
/// and gets back how many succeeded, how many failed and why. The queue is **cleared first**
/// by default (`append: true` appends instead), because "render every sequence under this
/// directory" contaminated by leftovers from the previous batch is unreadable. All specs are
/// validated before the queue is touched: half a batch enqueued is harder to clean up than
/// none.
pub(super) fn batch_run(session: &mut Session, args: &Value) -> Result<Value, String> {
    if !confirm(args) {
        return Err("aevum_batch_run 会批量入队并写视频，需参数 confirm: true".to_owned());
    }
    if session.is_running() {
        return Err("已有后台任务正在运行，先看 aevum_queue_status 或调用 aevum_cancel".to_owned());
    }
    let root = absolute(Path::new(&arg_str(args, "root")?))?;
    if !root.is_dir() {
        return Err(format!("不是文件夹：{}", echo_path(&root)));
    }
    let depth = scan_depth(args)?;
    let (found, skipped) = scan_sequences(&root, depth);
    if found.is_empty() {
        return Err(format!("这个目录下没扫到序列：{}", echo_path(&root)));
    }
    // 一条 out 对一批序列只会互相覆盖，直接拒绝比「最后一段赢」好收拾。
    //
    // One `out` across a batch would overwrite itself, so refuse rather than let the last
    // item win.
    if found.len() > 1 && opt_str(args, "out")?.is_some() {
        return Err("批量跑时不要给 out：每段默认写 <序列目录>/<名字>_成片.mp4".to_owned());
    }
    let mut items = Vec::with_capacity(found.len());
    for s in &found {
        items.push(queue_item_from_args(&s.root, args)?);
    }
    session.ensure_queue_loaded();
    let append = opt_bool(args, "append")?.unwrap_or(false);
    let cleared = if append {
        0
    } else {
        let n = session.queue.len();
        session.queue.clear();
        n
    };
    let mut queued = 0usize;
    for item in items {
        queue::add(&mut session.queue, item)?;
        queued += 1;
    }
    session.save_queue()?;
    let mut out = run_queue_now(session, args)?;
    if let Some(object) = out.as_object_mut() {
        object.insert("root".to_owned(), json!(root.display().to_string()));
        object.insert("depth".to_owned(), json!(depth));
        object.insert("scanned".to_owned(), json!(found.len()));
        object.insert("queued".to_owned(), json!(queued));
        object.insert("cleared".to_owned(), json!(cleared));
        object.insert(
            "skipped".to_owned(),
            json!(
                skipped
                    .iter()
                    .map(|(dir, why)| json!({"root": dir.display().to_string(), "reason": why}))
                    .collect::<Vec<_>>()
            ),
        );
        object.insert(
            "note".to_owned(),
            json!(
                "items 里每段带状态与失败原因；ok/failed 是段数。skipped 是目录里有图却读不出来的条目。"
            ),
        );
    }
    Ok(out)
}

/// 逐段渲染；段与段之间检查取消，段内由导出层逐帧检查。
fn run_queue_job(
    items: &[QueueItem],
    ffmpeg: &Path,
    shared: &JobShared,
) -> Result<JobOutcome, String> {
    let n = items.len();
    let mut rows = Vec::new();
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut cancelled = false;
    for (index, item) in items.iter().enumerate() {
        if shared.cancel_requested() {
            cancelled = true;
            break;
        }
        shared.set_stage(STAGE_FRAMES, index, n);
        let progress = |p: JobProgress| {
            shared.set_stage(
                match p.stage {
                    Stage::Frames => STAGE_FRAMES,
                    Stage::Encode => STAGE_ENCODE,
                },
                index,
                n,
            );
        };
        match queue::render_item(item, ffmpeg, shared.cancel_flag(), &progress) {
            Ok(path) => {
                ok += 1;
                rows.push((index, QueueStatus::Done, path.display().to_string()));
            }
            Err(e) => {
                if shared.cancel_requested() {
                    cancelled = true;
                    rows.push((index, QueueStatus::Pending, "已取消".to_owned()));
                    break;
                }
                failed += 1;
                rows.push((index, QueueStatus::Failed, e.to_string()));
            }
        }
        shared.progress(index + 1, n);
    }
    Ok(JobOutcome::Queue {
        rows,
        ok,
        failed,
        cancelled,
    })
}

// ------------------------------------------------------------------ 对齐与任务状态
