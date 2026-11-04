//! 对齐与任务状态域：镜头对齐、状态查询、取消。
//!
//! 边界：对齐是「把整段序列的逐帧位移估出来并统一裁切」，属于预览/导出之前的几何
//! 准备，不写最终成片。`align_status` 与 `cancel` 是**所有**后台任务共用的状态出口，
//! 所以它们在语义上比对齐本身更宽——放在同一个文件里是因为两者读的都是会话里那一个
//! 任务槽。
//!
//! The alignment and job-state domain: lens alignment, status, cancellation.
//!
//! Boundary: alignment estimates per-frame offsets over the whole sequence and picks one
//! common crop; it is geometry preparation ahead of preview/export and writes no final
//! deliverable. `align_status` and `cancel` are the shared state outlet for **every**
//! background job, so they are semantically wider than alignment itself; they live here
//! because both read the single job slot in the session.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use aevum_core::align::{self, AlignIssue, AlignOptions, AlignReport};
use serde_json::{Value, json};

use super::shared::{
    estimator_name, opt_bool, opt_f64, opt_str, opt_u32, parse_estimator, round6, stride_for,
    summarize,
};
use crate::session::{
    JobKind, JobOutcome, JobShared, MAX_LONG_SIDE, MAX_SERIES, STAGE_ENCODE, STAGE_FRAMES, Session,
};

pub(super) fn align_run(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("已有后台任务正在运行，先看 aevum_align_status 或调用 aevum_cancel".to_owned());
    }
    let (n, paths) = {
        let seq = session.require_seq()?;
        (seq.len(), seq.paths())
    };
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    let reference = opt_u32(args, "reference")?.unwrap_or(0) as usize;
    if reference >= n {
        return Err(format!("参考帧 {reference} 超出范围 0..{}", n - 1));
    }
    let estimator = parse_estimator(opt_str(args, "estimator")?.unwrap_or("template"))?;
    let smooth = opt_bool(args, "smooth")?.unwrap_or(false);
    let smooth_window = match opt_u32(args, "smooth_window")? {
        Some(v) => v as usize,
        None if smooth => 15,
        None => 0,
    };
    if smooth_window > 999 {
        return Err("smooth_window 必须在 0..999".to_owned());
    }
    let max_shift = opt_u32(args, "max_shift")?.unwrap_or(64);
    if max_shift > 4096 {
        return Err("max_shift 必须在 0..4096".to_owned());
    }
    let jump_px = opt_f64(args, "jump_px")?.unwrap_or(24.0);
    if jump_px < 0.0 {
        return Err("jump_px 必须是非负数".to_owned());
    }
    let min_score = opt_f64(args, "min_score")?.unwrap_or(0.35);
    if !(-1.0..=1.0).contains(&min_score) {
        return Err("min_score 必须在 -1..1".to_owned());
    }
    let options = AlignOptions {
        reference,
        estimator,
        max_shift,
        smooth_window,
        jump_px,
        min_score,
    };
    let background = opt_bool(args, "background")?.unwrap_or(false);
    let shared = Arc::clone(&session.shared);
    shared.begin(JobKind::Align, n);
    session.last_align_min_score = min_score;
    session.last_align_smoothed = smooth_window > 0;

    if background {
        let worker = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("aevum-align".to_owned())
            .spawn(move || {
                let result = run_align(&paths, &options, &worker).map(|report| JobOutcome::Align {
                    report,
                    reference: options.reference,
                    estimator: estimator_name(options.estimator),
                });
                worker.finish(result);
            })
            .map_err(|e| {
                let message = format!("启动后台对齐失败：{e}");
                shared.finish(Err(message.clone()));
                message
            })?;
        session.start_background(shared, handle);
        return Ok(json!({
            "started": true,
            "background": true,
            "frames": n,
            "reference": reference,
            "estimator": estimator_name(estimator),
            "smoothed": smooth_window > 0,
            "progress": {"done": 0, "total": n},
            "note": "用 aevum_align_status 查询进度，aevum_cancel 取消；完成后报告会自动存进会话。"
        }));
    }

    let result = run_align(&paths, &options, &shared).map(|report| JobOutcome::Align {
        report,
        reference,
        estimator: estimator_name(estimator),
    });
    session.complete_job(result)?;
    align_summary(session)
}

/// 分块调用 core 的对齐：每块都带参考帧，块之间检查取消。
///
/// core 的 `measure` 一次跑完整段且不轮询取消标志；分块后模板/相位设置只多算
/// 几次，但 `aevum_cancel` 能在下一块开始前真正停下（后台模式下）。
fn run_align(
    paths: &[PathBuf],
    options: &AlignOptions,
    shared: &JobShared,
) -> Result<AlignReport, String> {
    if shared.cancel_requested() {
        return Err("对齐已取消".to_owned());
    }
    let n = paths.len();
    if n == 0 {
        return Err("没有可对齐的帧".to_owned());
    }
    if options.reference >= n {
        return Err(format!(
            "参考帧 {} 超出范围 0..{}",
            options.reference,
            n - 1
        ));
    }
    let started = Instant::now();
    let reference_path = paths[options.reference].clone();
    // 约 4 块，单块最多 256 帧：控制参考帧与模板的重复准备成本。
    let chunk = n.div_ceil(4).clamp(1, 256);
    let mut chunk_options = options.clone();
    chunk_options.reference = 0;

    let mut offsets = vec![[0.0f64, 0.0f64]; n];
    let mut scores = vec![0.0f64; n];
    let mut issues: Vec<AlignIssue> = Vec::new();
    let mut size = [0u32; 2];
    let mut done = 0usize;
    for part in (0..n).collect::<Vec<_>>().chunks(chunk) {
        if shared.cancel_requested() {
            return Err("对齐已取消".to_owned());
        }
        // 第 0 项固定是参考帧，块成员的位移从第 1 项往回映射。
        let mut chunk_paths = Vec::with_capacity(part.len() + 1);
        chunk_paths.push(reference_path.clone());
        chunk_paths.extend(part.iter().map(|&i| paths[i].clone()));
        let report =
            align::measure(&chunk_paths, &chunk_options).map_err(|e| format!("对齐失败：{e}"))?;
        size = report.size;
        for (k, &i) in part.iter().enumerate() {
            offsets[i] = report.offsets[k + 1];
            scores[i] = report.scores[k + 1];
        }
        for issue in report.issues {
            // 跨块边界也要算跳变，块内的先丢掉，收尾时统一重算。
            if issue.kind == "jump" || issue.frame == 0 {
                continue;
            }
            let Some(&frame) = part.get(issue.frame - 1) else {
                continue;
            };
            issues.push(AlignIssue { frame, ..issue });
        }
        done += part.len();
        shared.progress(done, n);
    }
    // 取消请求可能刚好落在最后一块里，收尾前再确认一次。
    if shared.cancel_requested() {
        return Err("对齐已取消".to_owned());
    }

    if options.smooth_window > 0 {
        let kept = align::smooth_offsets(&mut offsets, &scores, options);
        // 置信度高、被判为真实抖动的帧保留原值，单独标出来。
        for frame in kept {
            issues.push(AlignIssue {
                frame,
                kind: "outlier".to_owned(),
                detail: "位移跳变且匹配置信度高，平滑时保留原值".to_owned(),
            });
        }
    }
    // 平滑后重新找一遍相邻帧跳变，供界面/agent 判断是否还有硬切。
    for i in 1..n {
        let jump = (offsets[i][0] - offsets[i - 1][0])
            .abs()
            .max((offsets[i][1] - offsets[i - 1][1]).abs());
        if jump > options.jump_px {
            issues.push(AlignIssue {
                frame: i,
                kind: "jump".to_owned(),
                detail: format!("相邻帧位移跳变 {jump:.1} px"),
            });
        }
    }
    issues.sort_by_key(|x| x.frame);
    let crop = align::common_crop(&offsets, size[0], size[1]);
    Ok(AlignReport {
        size,
        offsets,
        scores,
        issues,
        crop,
        elapsed_secs: started.elapsed().as_secs_f64(),
    })
}

/// 上次对齐报告的摘要；没有结果时返回 `null`。
fn align_summary(session: &Session) -> Result<Value, String> {
    let Some(a) = &session.last_align else {
        return Ok(Value::Null);
    };
    let n = a.offsets.len();
    let stride = stride_for(n);
    let mut series = Vec::new();
    for i in (0..n).step_by(stride) {
        series.push(json!({
            "frame": i,
            "dx": round6(a.offsets[i][0]),
            "dy": round6(a.offsets[i][1]),
            "score": round6(a.scores.get(i).copied().unwrap_or(0.0)),
        }));
    }
    // 低分帧按 core 写进 issues 的 `score` 口径统计（含读图失败）。
    let low: BTreeSet<usize> = a
        .issues
        .iter()
        .filter(|i| i.kind == "score")
        .map(|i| i.frame)
        .collect();
    let edge_count = a.issues.iter().filter(|i| i.kind == "edge").count();
    let issues: Vec<Value> = a
        .issues
        .iter()
        .take(MAX_SERIES)
        .map(|i| json!({"frame": i.frame, "kind": i.kind, "detail": i.detail}))
        .collect();
    let kept = n.saturating_sub(low.len());
    let low_frames: Vec<usize> = low.iter().copied().collect();
    // 普通序列直接给逐帧数组；超长序列（>4096 帧）只给降采样 series，避免 JSON 失控。
    let full = n <= MAX_LONG_SIDE as usize;
    let offsets = if full {
        to_value!(&a.offsets)
    } else {
        Value::Null
    };
    let scores = if full {
        to_value!(&a.scores)
    } else {
        Value::Null
    };
    Ok(json!({
        "reference": a.reference,
        "estimator": a.estimator,
        "smoothed": session.last_align_smoothed,
        "frames": n,
        "size": session.last_align_size,
        "crop": a.crop,
        "offsets": offsets,
        "scores": scores,
        "score_stats": summarize(a.scores.iter().copied().map(Some)),
        "kept": kept,
        "low_score_frames": low_frames,
        "quality": {
            "min_score": session.last_align_min_score,
            "kept": kept,
            "kept_ratio": if n == 0 { 0.0 } else { round6(kept as f64 / n as f64) },
            "low_score_count": low.len(),
            "low_score_frames": low.iter().copied().collect::<Vec<_>>(),
            "edge_count": edge_count,
            "issue_count": a.issues.len(),
        },
        "issues": issues,
        "series": series,
        "stride": stride,
        "truncated": !full,
        "elapsed_secs": round6(session.last_align_elapsed),
        "stored": true,
        "note": "offsets 为 dst(x,y)=src(x-dx,y-dy) 的补偿量；导出与预览会自动套用同一份对齐结果。"
    }))
}

pub(super) fn align_status(session: &mut Session) -> Result<Value, String> {
    session.poll_job();
    let running = session.is_running();
    let shared = &session.shared;
    let job = if running {
        shared.kind().map(JobKind::name)
    } else {
        None
    };
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
    let last_align = align_summary(session)?;

    Ok(json!({
        "running": running,
        "job": job,
        "last_job": shared.kind().map(|k| k.name()),
        "progress": {
            "done": done,
            "total": total,
            "percent": percent,
            "stage": stage,
        },
        "cancel_requested": shared.cancel_requested(),
        "elapsed_secs": round6(shared.elapsed_ms() as f64 / 1000.0),
        "last_align": last_align,
        "last_export": session.last_export.as_ref().map(|p| p.display().to_string()),
        "last_error": session.last_error,
        "keyframes": {
            "count": session.keyframes.len(),
            "frames": session.keyframes.keys().copied().collect::<Vec<_>>(),
        },
        "note": "running 为 true 时可用 aevum_cancel 请求取消；同步调用期间请求会排队，取消只对后台任务即时生效。"
    }))
}

/// 取消正在跑的任务。空闲时也返回成功，重复调用安全（幂等）。
pub(super) fn cancel(session: &mut Session) -> Result<Value, String> {
    session.poll_job();
    let kind = session.request_cancel();
    let was_running = kind.is_some();
    let running = session.is_running();
    Ok(json!({
        "cancelled": was_running,
        "was_running": was_running,
        "job": kind.map(JobKind::name),
        "running": running,
        "cancel_requested": session.shared.cancel_requested(),
        "note": if was_running {
            "已请求取消；任务会在下一批帧/下一帧之间停下，结果不再写入会话。"
        } else {
            "当前没有正在运行的任务；重复调用 cancel 不会报错。"
        }
    }))
}

// ------------------------------------------------------------------ 工程与 XMP
