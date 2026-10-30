//! 命令注册表与工具实现。
//!
//! 工具名统一用 `lumen_` 前缀平铺：MCP 客户端不会替我们加命名空间，
//! 平铺后 agent 一眼能看出这是 Lumen Studio 的能力。
//!
//! 所有写操作都遵守两条底线：不覆盖源图、不删除源图；覆盖已有输出和
//! 批量写真要调用方显式给 `confirm: true`。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Instant;

use lumen_core::align::{self, AlignIssue, AlignOptions, AlignReport, Estimator};
use lumen_core::audio::{AudioCodec, AudioOptions};
use lumen_core::colorspace::ColorSpace;
use lumen_core::crop::Motion;
use lumen_core::cube;
use lumen_core::deflicker;
use lumen_core::develop::Ev;
use lumen_core::exif;
use lumen_core::export::{
    self, Codec, JobProgress, OutputMode, OutputSharpen, RenderOptions, SharpenStrength, Stage,
    StillFormat,
};
use lumen_core::grade::{self, GradeParams, PRESET_NAMES};
use lumen_core::interp;
use lumen_core::metrics;
use lumen_core::preview;
use lumen_core::project;
use lumen_core::queue::{self, QueueItem, QueueStatus};
use lumen_core::retime::{self, Retime, SpeedKey};
use lumen_core::sequence::Sequence;
use lumen_core::warp::{self, Framing};
use lumen_core::watermark::Watermark;
use lumen_core::xmp;
use serde_json::{Value, json};

use crate::Command;
use crate::session::{
    JobKind, JobOutcome, JobShared, MAX_LONG_SIDE, MAX_SERIES, MEASURE_LONG_SIDE, STAGE_ENCODE,
    STAGE_FRAMES, Session, measure_paths, normalize, path_starts_with, same_path,
};

/// 核心类型都实现了 serde，这里统一把序列化错误翻成工具错误。
macro_rules! to_value {
    ($value:expr) => {
        serde_json::to_value($value).map_err(|e| format!("序列化失败：{e}"))?
    };
}

pub(crate) fn commands() -> &'static [Command] {
    static COMMANDS: OnceLock<Vec<Command>> = OnceLock::new();
    COMMANDS.get_or_init(build_commands)
}

pub(crate) fn dispatch(session: &mut Session, id: &str, args: &Value) -> Result<Value, String> {
    let result = dispatch_inner(session, id, args);
    // 成功的写操作追加进序列目录的 op_journal.jsonl；日志失败不影响工具结果。
    if result.is_ok()
        && lumen_core::journal::is_write_op(id)
        && let Some(root) = session.seq.as_ref().map(|s| s.root.clone())
    {
        let _ = lumen_core::journal::append(&root, "mcp", id, args);
    }
    result
}

fn dispatch_inner(session: &mut Session, id: &str, args: &Value) -> Result<Value, String> {
    // 先把已经结束的后台任务收进会话，后面读进度/结果都是最新状态。
    session.poll_job();
    match id {
        "lumen_open_sequence" => open_sequence(session, args),
        "lumen_sequence_info" => sequence_info(session),
        "lumen_measure" => measure(session, args),
        "lumen_deflicker_analyze" => deflicker_analyze(session, args),
        "lumen_grade_get" => grade_get(session),
        "lumen_grade_set" => grade_set(session, args),
        "lumen_grade_preset" => grade_preset(session, args),
        "lumen_grade_reset" => grade_reset(session),
        "lumen_grade_copy" => grade_copy(session, args),
        "lumen_keyframe_add" => keyframe_add(session, args),
        "lumen_keyframe_remove" => keyframe_remove(session, args),
        "lumen_keyframe_list" => keyframe_list(session, args),
        "lumen_render_preview" => render_preview(session, args),
        "lumen_export_stills" => export_stills(session, args),
        "lumen_export_video" => export_video(session, args),
        "lumen_hdr_merge" => hdr_merge(session, args),
        "lumen_pano_merge" => pano_merge(session, args),
        "lumen_project_get" => project_get(session),
        "lumen_project_save" => project_save(session),
        "lumen_xmp_read" => xmp_read(args),
        "lumen_xmp_write" => xmp_write(session, args),
        "lumen_cube_import" => cube_import(session, args),
        "lumen_cube_export" => cube_export(session, args),
        "lumen_align" => align_run(session, args),
        "lumen_align_status" => align_status(session),
        "lumen_cancel" => cancel(session),
        "lumen_queue_list" => queue_list(session),
        "lumen_queue_add" => queue_add(session, args),
        "lumen_queue_remove" => queue_remove(session, args),
        "lumen_queue_clear" => queue_clear(session),
        "lumen_queue_run" => queue_run(session, args),
        "lumen_queue_status" => queue_status(session),
        "lumen_queue_cancel" => queue_cancel(session),
        "lumen_journal_list" => journal_list(session, args),
        other => Err(format!("未知工具：{other}")),
    }
}

// ------------------------------------------------------------------ 序列

fn open_sequence(session: &mut Session, args: &Value) -> Result<Value, String> {
    // 换序列会清空会话；后台任务还握着旧帧路径，必须等它结束。
    if session.is_running() {
        return Err("有后台任务正在运行，先调用 lumen_cancel 并等它停下再换序列".to_owned());
    }
    let root = absolute(Path::new(arg_str(args, "root")?))?;
    if !root.is_dir() {
        return Err(format!("不是文件夹：{}", root.display()));
    }
    let seq = Sequence::open(&root).map_err(|e| format!("打开序列失败：{e}"))?;
    let count = seq.len();
    let name = seq.name();
    let first = seq.frames.first().map(|f| f.path.display().to_string());
    let last = seq.frames.last().map(|f| f.path.display().to_string());

    // 换序列等于换会话，旧的分析和输出路径一律作废；队列是跨序列的，保留。
    let queue = std::mem::take(&mut session.queue);
    let queue_path = std::mem::take(&mut session.queue_path);
    let queue_loaded = session.queue_loaded;
    *session = Session::default();
    session.queue = queue;
    session.queue_path = queue_path;
    session.queue_loaded = queue_loaded;
    session.seq = Some(seq);

    let path = project::project_path(&root);
    let mut project_loaded = false;
    let mut warning = Value::Null;
    if path.is_file() {
        match project::load(&path) {
            Ok(p) => {
                session.params = p.params.clone();
                if !p.preset.is_empty() {
                    session.preset = p.preset.clone();
                }
                session.def_window = p.def_window;
                session.def_strength = p.def_strength;
                session.def_multi = p.def_multi;
                session.def_channels = p.def_channels;
                // 关键帧和上次对齐随工程恢复：导出默认会应用同一组位移与裁切。
                session.keyframes = p.keyframes.clone();
                session.last_align = p.align.clone();
                // 工程里没有记录是否平滑过，恢复后按「原始位移」展示。
                session.last_align_smoothed = false;
                session.project = Some(p);
                project_loaded = true;
            }
            // 工程坏了不该阻止打开序列，用默认参数继续，把原因带回去。
            Err(e) => {
                warning = json!(format!("工程读取失败，已用默认参数：{e}"));
            }
        }
    }

    Ok(json!({
        "root": root.display().to_string(),
        "name": name,
        "frames": count,
        "first": first,
        "last": last,
        "project_loaded": project_loaded,
        "project_warning": warning,
        "preset": session.preset,
        "keyframes": session.keyframes.len(),
        "aligned": session.last_align.is_some(),
        "deflicker": {
            "window": session.def_window,
            "strength": session.def_strength,
            "multi": session.def_multi,
            "channels": session.def_channels,
        }
    }))
}

fn sequence_info(session: &mut Session) -> Result<Value, String> {
    let seq = session.require_seq()?;
    let root = seq.root.display().to_string();
    let name = seq.name();
    let frames = seq.len();
    let first = seq.frames.first().map(|f| f.path.display().to_string());
    let last = seq.frames.last().map(|f| f.path.display().to_string());
    let path = session.project_path()?;
    let exists = path.is_file();
    let params = to_value!(&session.params);
    let stats = match &session.analysis {
        Some(a) => to_value!(&a.stats),
        None => Value::Null,
    };
    let align_brief = session.last_align.as_ref().map(|a| {
        json!({
            "reference": a.reference,
            "estimator": a.estimator,
            "crop": a.crop,
            "issues": a.issues.len(),
        })
    });
    let keyframe_frames: Vec<usize> = session.keyframes.keys().copied().collect();
    Ok(json!({
        "root": root,
        "name": name,
        "frames": frames,
        "first": first,
        "last": last,
        "preset": session.preset,
        "params": params,
        "keyframes": {"count": keyframe_frames.len(), "frames": keyframe_frames},
        "deflicker": {
            "window": session.def_window,
            "strength": session.def_strength,
            "multi": session.def_multi,
            "channels": session.def_channels,
            "analyzed": session.analysis.is_some(),
            "stats": stats,
        },
        "measured": session.metrics.is_some(),
        "aligned": session.last_align.is_some(),
        "align": align_brief,
        "capture": capture_brief(seq),
        "project": {"path": path.display().to_string(), "exists": exists},
        "outputs": {
            "last_export": session.last_export.as_ref().map(|p| p.display().to_string()),
            "last_preview": session.last_preview.as_ref().map(|p| p.display().to_string()),
        }
    }))
}

/// 拍摄参数概览：机身、方向、跨帧曝光台阶。
///
/// 抽帧读，不逐帧读：EXIF 是按块定位读的，但 4000 帧的序列为了一个摘要把
/// 每个文件都 seek 一遍不划算。24 个采样点足够看出换了机身、转了方向或者
/// 日落途中 ISO 一档档往上跳。
fn capture_brief(seq: &Sequence) -> Value {
    let paths = seq.paths();
    let n = paths.len();
    if n == 0 {
        return Value::Null;
    }
    let stride = (n / 24).max(1);
    let mut picked: Vec<usize> = (0..n).step_by(stride).collect();
    if picked.last() != Some(&(n - 1)) {
        picked.push(n - 1);
    }
    let mut camera: Option<String> = None;
    let mut orientation = 1u8;
    let mut orientation_mixed = false;
    let mut curve: Vec<Value> = Vec::new();
    let mut ev_min = f64::MAX;
    let mut ev_max = f64::MIN;
    for (k, i) in picked.iter().enumerate() {
        let e = exif::read(&paths[*i]);
        if k == 0 {
            orientation = e.orientation;
        } else if e.orientation != orientation {
            orientation_mixed = true;
        }
        if camera.is_none() {
            camera = e.camera_line();
        }
        if let Some(ev) = e.ev() {
            ev_min = ev_min.min(ev);
            ev_max = ev_max.max(ev);
            curve.push(json!({"frame": i + 1, "ev": round6(ev)}));
        }
    }
    let spread = if curve.len() < 2 {
        0.0
    } else {
        round6(ev_max - ev_min)
    };
    json!({
        "camera": camera,
        "orientation": orientation,
        "orientation_mixed": orientation_mixed,
        "sampled": picked.len(),
        "ev_read": curve.len(),
        "ev_spread": spread,
        "ev_curve": curve,
    })
}

fn measure(session: &mut Session, args: &Value) -> Result<Value, String> {
    let refresh = opt_bool(args, "refresh")?.unwrap_or(false);
    let background = opt_bool(args, "background")?.unwrap_or(false);
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 lumen_cancel".to_owned());
    }
    // 已经测过且没要求刷新时直接复用，避免每次查摘要都重读整段序列。
    if !refresh && session.metrics.is_some() {
        return measure_summary(session);
    }
    let (n, paths) = {
        let seq = session.require_seq()?;
        (seq.len(), seq.paths())
    };
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    let shared = Arc::clone(&session.shared);
    shared.begin(JobKind::Measure, n);
    if background {
        let worker = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("lumen-measure".to_owned())
            .spawn(move || {
                let result = measure_paths(&paths, &worker);
                worker.finish(result);
            })
            .map_err(|e| {
                let message = format!("启动后台测光失败：{e}");
                shared.finish(Err(message.clone()));
                message
            })?;
        session.start_background(shared, handle);
        return Ok(json!({
            "started": true,
            "background": true,
            "frames": n,
            "long_side": MEASURE_LONG_SIDE,
            "progress": {"done": 0, "total": n},
            "note": "用 lumen_align_status 查询进度，lumen_cancel 取消。"
        }));
    }
    let result = measure_paths(&paths, &shared);
    session.complete_job(result)?;
    measure_summary(session)
}

/// 测光结果的摘要；`series` 按步长降采样，完整数值只留在会话里。
fn measure_summary(session: &Session) -> Result<Value, String> {
    let metrics = session
        .metrics
        .as_ref()
        .ok_or_else(|| "还没有测光，先调用 lumen_measure".to_owned())?;
    let stride = stride_for(metrics.len());
    let mut series = Vec::new();
    for (i, m) in metrics.iter().enumerate() {
        if i % stride != 0 && i + 1 != metrics.len() {
            continue;
        }
        match m {
            Some(m) => series.push(json!({
                "frame": i,
                "mean": round6(m.mean),
                "median": round6(m.median),
                "center": round6(m.center),
                "sharp": round6(m.sharp),
            })),
            None => series.push(json!({"frame": i, "error": "解码失败"})),
        }
    }
    let issues: Vec<Value> = session
        .issues
        .iter()
        .map(|(i, e)| json!({"frame": i, "error": e}))
        .collect();
    Ok(json!({
        "frames": metrics.len(),
        "valid": metrics.iter().filter(|m| m.is_some()).count(),
        "long_side": MEASURE_LONG_SIDE,
        "issues": issues,
        "stats": {
            "mean": summarize(metrics.iter().map(|m| m.map(|m| m.mean))),
            "median": summarize(metrics.iter().map(|m| m.map(|m| m.median))),
            "center": summarize(metrics.iter().map(|m| m.map(|m| m.center))),
            "sharp": summarize(metrics.iter().map(|m| m.map(|m| m.sharp))),
        },
        "series": series,
        "stride": stride,
        "note": "亮度为 0~1 的 gamma 编码灰度；sharp 是拉普拉斯方差，越大越锐。"
    }))
}

fn deflicker_analyze(session: &mut Session, args: &Value) -> Result<Value, String> {
    session.ensure_metrics()?;
    let window = opt_f64(args, "window")?.unwrap_or(session.def_window);
    let strength = opt_f64(args, "strength")?.unwrap_or(session.def_strength);
    let multi = opt_bool(args, "multi")?.unwrap_or(session.def_multi);
    let channels = opt_bool(args, "channels")?.unwrap_or(session.def_channels);
    if !window.is_finite() || !strength.is_finite() {
        return Err("window / strength 必须是有限数值".to_owned());
    }
    // 口径同 GUI：有线性光测光就在线性域算，拿不到（比如只测过灰度）才退回 gamma 均值。
    let measured = session.def_means.iter().any(|m| m.is_some());
    let analysis = if channels && measured {
        let rgb: Vec<Option<[f64; 3]>> =
            session.def_means.iter().map(|m| m.map(|m| m.rgb)).collect();
        deflicker::analyze_rgb(&rgb, window, strength, multi)
    } else if measured {
        let luma: Vec<Option<f64>> = session
            .def_means
            .iter()
            .map(|m| m.map(|m| m.luma))
            .collect();
        deflicker::analyze_luma(&luma, window, strength, multi)
    } else {
        let means: Vec<Option<f64>> = session
            .metrics
            .as_ref()
            .expect("ensure_metrics 已填充")
            .iter()
            .map(|m| m.map(|m| m.mean * 255.0))
            .collect();
        deflicker::analyze_luma(&means, window, strength, multi)
    };
    let ev = analysis.exposure_per_frame();
    let chan = analysis.chan_residual_per_frame();
    let frames = analysis.rows();
    let stride = stride_for(frames);
    let mut series = Vec::new();
    for (i, v) in ev.iter().enumerate() {
        if i % stride == 0 || i + 1 == frames {
            let mut point = json!({"frame": i, "ev": round6(*v)});
            if channels {
                // 开分通道时把三通道各自的 EV 也报出来：整帧曝光 + 色残差，
                // 不然 agent 只看得到均值，看不出色闪有没有被治。
                point["rgb"] = json!([
                    round6(v + chan[i][0]),
                    round6(v + chan[i][1]),
                    round6(v + chan[i][2])
                ]);
            }
            series.push(point);
        }
    }
    let min = ev.iter().copied().fold(f64::INFINITY, f64::min);
    let max = ev.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean_abs = if ev.is_empty() {
        0.0
    } else {
        ev.iter().map(|v| v.abs()).sum::<f64>() / ev.len() as f64
    };
    let stats = to_value!(&analysis.stats);

    session.def_window = window;
    session.def_strength = strength;
    session.def_multi = multi;
    session.def_channels = channels;
    session.analysis = Some(analysis);

    Ok(json!({
        "window": window,
        "strength": strength,
        "multi": multi,
        "channels": channels,
        "frames": frames,
        "valid": session.analysis.as_ref().map(|a| a.stats.valid).unwrap_or(0),
        "stats": stats,
        "offsets": {
            "min": if ev.is_empty() { 0.0 } else { round6(min) },
            "max": if ev.is_empty() { 0.0 } else { round6(max) },
            "mean_abs": round6(mean_abs),
            "first": ev.first().copied().map(round6),
            "last": ev.last().copied().map(round6),
        },
        "series": series,
        "stride": stride,
        "note": "ev 为正表示该帧提亮，导出与预览会自动叠加；开分通道时每项另带 rgb=[R,G,B] 三通道 EV。"
    }))
}

// ------------------------------------------------------------------ 调色

fn grade_get(session: &mut Session) -> Result<Value, String> {
    let copies = session.copies();
    let active = copies
        .iter()
        .find(|c| c.params == session.params && c.keyframes == session.keyframes)
        .map(|c| c.name.clone());
    Ok(json!({
        "preset": session.preset,
        "params": to_value!(&session.params),
        "copies": copies
            .iter()
            .map(|c| json!({"name": c.name, "keyframes": c.keyframes.len()}))
            .collect::<Vec<_>>(),
        "active_copy": active,
    }))
}

fn grade_set(session: &mut Session, args: &Value) -> Result<Value, String> {
    let partial = args
        .get("params")
        .ok_or_else(|| "缺少 params 对象".to_owned())?;
    session.params = merge_params(&session.params, partial)?;
    session.preset = "自定义".to_owned();
    grade_get(session)
}

/// 把部分字段合并进一组参数；未知字段直接报错，避免 agent 以为改成功了。
fn merge_params(base: &GradeParams, partial: &Value) -> Result<GradeParams, String> {
    let object = partial
        .as_object()
        .ok_or_else(|| "params 必须是对象".to_owned())?;
    let mut current = to_value!(base);
    let current_object = current
        .as_object_mut()
        .expect("GradeParams 一定序列化成对象");
    let mut unknown = Vec::new();
    for (key, value) in object {
        if current_object.contains_key(key) {
            current_object.insert(key.clone(), value.clone());
        } else {
            unknown.push(key.clone());
        }
    }
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!("未知调色字段：{}", unknown.join(", ")));
    }
    serde_json::from_value(current).map_err(|e| format!("参数格式不对：{e}"))
}

fn grade_preset(session: &mut Session, args: &Value) -> Result<Value, String> {
    let name = arg_str(args, "name")?;
    if !PRESET_NAMES.contains(&name) {
        return Err(format!(
            "未知预设 {name}；可用：{}",
            PRESET_NAMES.join("、")
        ));
    }
    session.params = grade::preset(name);
    session.preset = name.to_owned();
    grade_get(session)
}

fn grade_reset(session: &mut Session) -> Result<Value, String> {
    session.params = GradeParams::default();
    session.preset = "原始".to_owned();
    grade_get(session)
}

fn grade_copy(session: &mut Session, args: &Value) -> Result<Value, String> {
    let op = args.get("op").and_then(Value::as_str).unwrap_or("list");
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let check_name = |name: &str| -> Result<(), String> {
        if name.is_empty() {
            return Err("name 不能为空".to_owned());
        }
        if name.chars().count() > 40 {
            return Err("副本名至多 40 个字符".to_owned());
        }
        Ok(())
    };
    match op {
        "list" => grade_get(session),
        "save" => {
            check_name(&name)?;
            let overwrite = opt_bool(args, "overwrite")?.unwrap_or(false);
            let params = session.params.clone();
            let keyframes = session.keyframes.clone();
            let index =
                session.with_copies(|copies| match copies.iter().position(|c| c.name == name) {
                    Some(_) if !overwrite => {
                        Err(format!("副本「{name}」已存在；要覆盖给 overwrite: true"))
                    }
                    Some(i) => {
                        copies[i].params = params.clone();
                        copies[i].keyframes = keyframes.clone();
                        Ok(i)
                    }
                    None => {
                        copies.push(project::GradeCopy {
                            name: name.clone(),
                            params,
                            keyframes,
                        });
                        Ok(copies.len() - 1)
                    }
                })?;
            Ok(json!({
                "saved": name,
                "index": index + 1,
                "copies": session.copies().len(),
            }))
        }
        "apply" => {
            let Some(copy) = session.copies().into_iter().find(|c| c.name == name) else {
                return Err(format!("没有叫 {name} 的副本"));
            };
            session.params = copy.params;
            session.keyframes = copy.keyframes;
            session.preset = format!("副本 {}", copy.name);
            grade_get(session)
        }
        "delete" => {
            check_name(&name)?;
            let removed = session.with_copies(|copies| copies.iter().position(|c| c.name == name));
            let Some(i) = removed else {
                return Err(format!("没有叫 {name} 的副本"));
            };
            session.with_copies(|copies| {
                copies.remove(i);
            });
            Ok(json!({"deleted": name, "copies": session.copies().len()}))
        }
        "rename" => {
            check_name(&name)?;
            let to = args
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            check_name(&to)?;
            session.with_copies(|copies| {
                if copies.iter().any(|c| c.name == to) {
                    return Err(format!("副本「{to}」已存在"));
                }
                let Some(slot) = copies.iter_mut().find(|c| c.name == name) else {
                    return Err(format!("没有叫 {name} 的副本"));
                };
                slot.name = to.clone();
                Ok(())
            })?;
            Ok(json!({"renamed": to, "copies": session.copies().len()}))
        }
        other => Err(format!(
            "未知 op {other}；可用 list / save / apply / delete / rename"
        )),
    }
}

// ------------------------------------------------------------------ 关键帧

fn keyframe_add(session: &mut Session, args: &Value) -> Result<Value, String> {
    let frame = arg_frame(session, args, "frame")?;
    // 不传 params 时把当前基准参数钉在该帧；传了就在基准上做字段合并。
    let params = match args.get("params") {
        None | Some(Value::Null) => session.params.clone(),
        Some(partial) => merge_params(&session.params, partial)?,
    };
    session.keyframes.insert(frame, params);
    keyframe_state(session)
}

fn keyframe_remove(session: &mut Session, args: &Value) -> Result<Value, String> {
    let frame = arg_frame(session, args, "frame")?;
    if session.keyframes.remove(&frame).is_none() {
        return Err(format!("第 {frame} 帧没有关键帧"));
    }
    keyframe_state(session)
}

fn keyframe_list(session: &mut Session, args: &Value) -> Result<Value, String> {
    let mut out = keyframe_state(session)?;
    // 可选 frame：顺便给出该帧插值后的参数，等价于 GUI 里跳到某帧。
    if let Some(frame) = opt_u32(args, "frame")? {
        let index = frame as usize;
        let n = session.require_seq()?.len();
        if index >= n {
            return Err(format!("frame {index} 超出范围 0..{}", n.saturating_sub(1)));
        }
        let params = interp::params_at(&session.params, &session.keyframes, index);
        out["at"] = json!({
            "frame": index,
            "is_keyframe": session.keyframes.contains_key(&index),
            "params": to_value!(&params),
        });
    }
    Ok(out)
}

fn keyframe_state(session: &Session) -> Result<Value, String> {
    let frames: Vec<usize> = session.keyframes.keys().copied().collect();
    let mut keyframes = Vec::with_capacity(frames.len());
    for frame in &frames {
        let params = session.keyframes.get(frame).expect("刚刚从同一张表取出");
        keyframes.push(json!({"frame": frame, "params": to_value!(params)}));
    }
    Ok(json!({
        "count": frames.len(),
        "frames": frames,
        "keyframes": keyframes,
        "base": to_value!(&session.params),
        "preset": session.preset,
        "note": "两帧之间线性插值，两端之外沿用最近的关键帧；导出与预览都会生效。"
    }))
}

// ------------------------------------------------------------------ 预览与导出

fn render_preview(session: &mut Session, args: &Value) -> Result<Value, String> {
    let (n, source) = {
        let seq = session.require_seq()?;
        (
            seq.len(),
            seq.frames
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
        )
    };
    let frame = opt_u32(args, "frame")?.unwrap_or(0) as usize;
    if frame >= n {
        return Err(format!("frame {frame} 超出范围 0..{}", n - 1));
    }
    let side = opt_u32(args, "side")?.unwrap_or(1280);
    if side == 0 {
        return Err("side 必须大于 0".to_owned());
    }
    if side > MAX_LONG_SIDE && !confirm(args) {
        return Err(format!(
            "side {side} 超过 {MAX_LONG_SIDE}，需 confirm: true"
        ));
    }
    let out = absolute(Path::new(arg_str(args, "out")?))?;
    if !out
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
    {
        return Err("out 必须以 .png 结尾".to_owned());
    }
    if session.is_source(&out) {
        return Err(format!("拒绝覆盖源图：{}", out.display()));
    }
    if out.exists() && !confirm(args) {
        return Err(format!("目标已存在，需 confirm: true：{}", out.display()));
    }

    let rgb = metrics::decode_rgb(&source[frame], side).map_err(|e| format!("读图失败：{e}"))?;
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let mut buf: Vec<f32> = rgb.as_raw().iter().map(|v| *v as f32 / 255.0).collect();
    let gain = session.gain_at(frame);
    // 该帧自己的参数（关键帧插值），不是序列基准参数。
    let params = interp::params_at(&session.params, &session.keyframes, frame);
    preview::render(&mut buf, w, h, &params, gain);

    let mut bytes = Vec::with_capacity(w * h * 3);
    for px in buf.chunks_exact(3) {
        bytes.push((px[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        bytes.push((px[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        bytes.push((px[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    }
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let image = image::RgbImage::from_raw(w as u32, h as u32, bytes)
        .ok_or_else(|| "预览缓冲尺寸不对".to_owned())?;
    image
        .save_with_format(&out, image::ImageFormat::Png)
        .map_err(|e| format!("写 PNG 失败：{e}"))?;
    session.last_preview = Some(out.clone());

    Ok(json!({
        "path": out.display().to_string(),
        "frame": frame,
        "width": w,
        "height": h,
        "gain_ev": round6(gain.add as f64),
        "gain_rgb_ev": {
            "r": round6((gain.add + gain.chan[0]) as f64),
            "g": round6((gain.add + gain.chan[1]) as f64),
            "b": round6((gain.add + gain.chan[2]) as f64),
        },
    }))
}

fn export_stills(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 lumen_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    let root = session.require_root()?;
    let out_dir = absolute(Path::new(arg_str(args, "out_dir")?))?;
    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    let (sharpen_for, sharpen_strength) = opt_sharpen(args)?;
    let color_space = opt_color_space(args)?;
    let target_size = target_size_arg(args)?;
    let long_side = if target_size.is_some() {
        None
    } else {
        let side = opt_u32(args, "long_side")?;
        check_long_side(side, args)?;
        side
    };
    require_batch_confirm(args, "lumen_export_stills")?;
    ensure_out_dir_safe(&root, &out_dir)?;

    let paths = session.require_seq()?.paths();
    // copy：这一次导出改用某套副本的调色；省略就是会话当前这套。
    let copy = opt_str(args, "copy")?.unwrap_or_default();
    let params = session.frame_params_of(n, copy)?;
    let gains = session.gains(n);
    // 有对齐结果就自动套用统一裁切 + 逐帧位移，与 GUI 导出路径一致。
    let (crop, offsets) = session.alignment(n);
    let options = RenderOptions {
        mode: OutputMode::Stills,
        format,
        quality: opt_quality(args)?,
        keep_exif: opt_bool(args, "keep_exif")?.unwrap_or(false),
        xmp_sidecar: opt_bool(args, "xmp_sidecar")?.unwrap_or(false),
        resume: opt_bool(args, "resume")?.unwrap_or(false),
        sharpen_for,
        sharpen_strength,
        out_dir: out_dir.clone(),
        video_out: PathBuf::new(),
        crop,
        rotate: 0.0,
        long_side,
        target_size,
        fps: 24.0,
        codec: Codec::H265,
        crf: 18,
        preset: "medium".to_owned(),
        audio: None,
        audio_opts: AudioOptions::default(),
        keep_frames: false,
        color_space,
        watermark: opt_watermark(args)?,
        // 运镜跟着这次导出走：没传 motion 就沿用会话工程里那份，与队列同口径。
        motion: motion_arg(session, args)?,
        metadata: session.metadata(),
    };
    let plan = ExportPlan {
        kind: ExportKind::Stills,
        paths,
        params,
        gains,
        offsets,
        options,
        ffmpeg: None,
        deflicker: session.analysis.is_some(),
        keyframes: session.keyframes.len(),
        // 静帧没有帧率参数：fps 只影响回报的时长读数，取成片默认 24。
        retime: retime_plan(args, n, 24.0)?,
    };
    let background = opt_bool(args, "background")?.unwrap_or(false);
    if background {
        return start_export_background(session, plan);
    }
    run_export_sync(session, plan)
}

/// HDR 合并：把同场景多张不同曝光融成一张静帧（Mertens 曝光融合）。
///
/// 与 GUI 的 `hdr.merge` 同口径——按帧下标读原图、套对齐的统一裁切与逐帧位移、
/// 融合后走静帧导出那条输出尾（锐化 / 水印 / 色域 / EXIF）；融合本身不改调色。
fn hdr_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 lumen_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    require_batch_confirm(args, "lumen_hdr_merge")?;
    let indices: Vec<usize> = match args.get("frames") {
        Some(list) => list
            .as_array()
            .ok_or_else(|| "frames 要是帧下标数组，例如 [0,1,2]".to_owned())?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|i| ((i as usize) < n).then_some(i as usize))
                    .ok_or_else(|| format!("帧下标越界：{v}（序列共 {n} 帧）"))
            })
            .collect::<Result<_, _>>()?,
        None => {
            let around = opt_u32(args, "around")?.unwrap_or(1) as usize;
            // 无头会话没有「当前帧」，省 center 就取序列中间那段。
            let center = opt_u32(args, "center")?
                .map(|v| v as usize)
                .unwrap_or(n / 2);
            if center >= n {
                return Err(format!("center 越界：{center}（序列共 {n} 帧）"));
            }
            (center.saturating_sub(around)..=center.saturating_add(around).min(n - 1)).collect()
        }
    };
    if indices.len() < 2 {
        return Err("HDR 合并至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
    }
    if indices.len() > lumen_core::hdr::MAX_FRAMES {
        return Err(format!(
            "HDR 合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑关键几帧",
            lumen_core::hdr::MAX_FRAMES,
            indices.len()
        ));
    }
    let root = session.require_root()?;
    let out_dir = match opt_str(args, "out_dir")? {
        Some(dir) => absolute(Path::new(dir))?,
        None => root.join("HDR"),
    };
    ensure_out_dir_safe(&root, &out_dir)?;
    fs::create_dir_all(&out_dir).map_err(|e| format!("建输出目录失败：{e}"))?;

    let paths = session.require_seq()?.paths();
    // 有对齐结果就套统一裁切 + 逐帧位移，与静帧导出同一条几何路。
    let (crop, offsets) = session.alignment(n);
    let rect = crop.map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize));
    let mut buffers: Vec<Vec<f32>> = Vec::with_capacity(indices.len());
    let mut dims: Option<(usize, usize)> = None;
    for i in &indices {
        let (rgb, w, h) = export::load_rgb_f32(&paths[*i]).map_err(|e| format!("读帧失败：{e}"))?;
        let framing = Framing {
            angle_deg: 0.0,
            off: offsets.get(*i).copied().unwrap_or([0.0, 0.0]),
        };
        let (buf, got) = if rect.is_none() && framing.is_identity() {
            (rgb, (w as usize, h as usize))
        } else {
            let mut out = Vec::new();
            let (cw, ch) =
                warp::transform_into(&rgb, w as usize, h as usize, framing, rect, &mut out);
            (out, (cw, ch))
        };
        match dims {
            None => dims = Some(got),
            Some(prev) if prev != got => {
                return Err(format!(
                    "第 {} 帧与第 {i} 帧对齐后尺寸不一致（{}×{} vs {}×{}）：先跑对齐或裁成同尺寸",
                    indices[0], prev.0, prev.1, got.0, got.1
                ));
            }
            Some(_) => {}
        }
        buffers.push(buf);
    }
    let (w, h) = dims.unwrap_or((0, 0));
    let refs: Vec<&[f32]> = buffers.iter().map(|b| b.as_slice()).collect();
    let mut fused = lumen_core::hdr::merge(&refs, w, h);

    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    let (sharpen_for, sharpen_strength) = opt_sharpen(args)?;
    let options = RenderOptions {
        quality: opt_quality(args)?,
        keep_exif: opt_bool(args, "keep_exif")?.unwrap_or(false),
        xmp_sidecar: opt_bool(args, "xmp_sidecar")?.unwrap_or(false),
        sharpen_for,
        sharpen_strength,
        color_space: opt_color_space(args)?,
        watermark: opt_watermark(args)?,
        out_dir: out_dir.clone(),
        metadata: session.metadata(),
        ..RenderOptions::still(format)
    };
    let dst = out_dir.join(format!(
        "hdr_{:06}_{}f.{}",
        indices[0],
        indices.len(),
        format.ext()
    ));
    let deep = format == StillFormat::Tiff16;
    export::write_single_still(&paths[indices[0]], &dst, &mut fused, w, h, deep, &options)
        .map_err(|e| format!("写 HDR 成片失败：{e}"))?;
    Ok(json!({
        "path": dst.display().to_string(),
        "frames": indices.len(),
        "indices": indices,
        "width": w,
        "height": h,
    }))
}

fn pano_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 lumen_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    require_batch_confirm(args, "lumen_pano_merge")?;
    let indices: Vec<usize> = match args.get("frames") {
        Some(list) => list
            .as_array()
            .ok_or_else(|| "frames 要是帧下标数组，例如 [0,1,2]".to_owned())?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|i| ((i as usize) < n).then_some(i as usize))
                    .ok_or_else(|| format!("帧下标越界：{v}（序列共 {n} 帧）"))
            })
            .collect::<Result<_, _>>()?,
        None => {
            let around = opt_u32(args, "around")?.unwrap_or(1) as usize;
            // 无头会话没有「当前帧」，省 start 就从序列中间起。
            let start = opt_u32(args, "start")?.map(|v| v as usize).unwrap_or(n / 2);
            if start >= n {
                return Err(format!("start 越界：{start}（序列共 {n} 帧）"));
            }
            (start..=start.saturating_add(around).min(n - 1)).collect()
        }
    };
    if indices.len() < 2 {
        return Err("全景合并至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
    }
    if indices.len() > lumen_core::pano::MAX_FRAMES {
        return Err(format!(
            "全景合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑相邻几张",
            lumen_core::pano::MAX_FRAMES,
            indices.len()
        ));
    }
    let root = session.require_root()?;
    let out_dir = match opt_str(args, "out_dir")? {
        Some(dir) => absolute(Path::new(dir))?,
        None => root.join("全景"),
    };
    ensure_out_dir_safe(&root, &out_dir)?;
    fs::create_dir_all(&out_dir).map_err(|e| format!("建输出目录失败：{e}"))?;

    let paths = session.require_seq()?.paths();
    // 全景靠特征点自己求单应，不套对齐位移——原始帧直接进拼接。
    let mut frames: Vec<(Vec<f32>, u32, u32)> = Vec::with_capacity(indices.len());
    for i in &indices {
        let (rgb, w, h) = export::load_rgb_f32(&paths[*i]).map_err(|e| format!("读帧失败：{e}"))?;
        frames.push((rgb, w, h));
    }
    let refs: Vec<(&[f32], u32, u32)> = frames
        .iter()
        .map(|(rgb, w, h)| (rgb.as_slice(), *w, *h))
        .collect();
    let (mut pano, cw, ch) =
        lumen_core::pano::stitch(&refs).map_err(|e| format!("拼接失败：{e}"))?;

    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    let (sharpen_for, sharpen_strength) = opt_sharpen(args)?;
    let options = RenderOptions {
        quality: opt_quality(args)?,
        keep_exif: opt_bool(args, "keep_exif")?.unwrap_or(false),
        xmp_sidecar: opt_bool(args, "xmp_sidecar")?.unwrap_or(false),
        sharpen_for,
        sharpen_strength,
        color_space: opt_color_space(args)?,
        watermark: opt_watermark(args)?,
        out_dir: out_dir.clone(),
        metadata: session.metadata(),
        ..RenderOptions::still(format)
    };
    let dst = out_dir.join(format!(
        "pano_{:06}_{}f.{}",
        indices[0],
        indices.len(),
        format.ext()
    ));
    let deep = format == StillFormat::Tiff16;
    export::write_single_still(
        &paths[indices[0]],
        &dst,
        &mut pano,
        cw as usize,
        ch as usize,
        deep,
        &options,
    )
    .map_err(|e| format!("写全景成片失败：{e}"))?;
    Ok(json!({
        "path": dst.display().to_string(),
        "frames": indices.len(),
        "indices": indices,
        "width": cw,
        "height": ch,
    }))
}

fn export_video(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 lumen_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    let root = session.require_root()?;
    let out_dir = absolute(Path::new(arg_str(args, "out_dir")?))?;
    let video_out = absolute(Path::new(arg_str(args, "video_out")?))?;
    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    check_video_format(format)?;
    let (sharpen_for, sharpen_strength) = opt_sharpen(args)?;
    let color_space = opt_color_space(args)?;
    let codec = parse_codec(opt_str(args, "codec")?.unwrap_or("h265"))?;
    let crf = opt_u32(args, "crf")?.unwrap_or(18);
    if crf > 51 {
        return Err("crf 必须在 0..51".to_owned());
    }
    let preset = opt_str(args, "preset")?.unwrap_or("medium").to_owned();
    if !X264_PRESETS.contains(&preset.as_str()) {
        return Err(format!(
            "未知编码 preset {preset}；可用：{}",
            X264_PRESETS.join("、")
        ));
    }
    let fps = opt_f64(args, "fps")?.unwrap_or(24.0);
    if !fps.is_finite() || fps <= 0.0 || fps > 240.0 {
        return Err("fps 必须在 (0, 240]".to_owned());
    }
    let target_size = target_size_arg(args)?;
    let long_side = if target_size.is_some() {
        None
    } else {
        let side = opt_u32(args, "long_side")?;
        check_long_side(side, args)?;
        side
    };
    let audio = match opt_str(args, "audio")? {
        Some(raw) => {
            let path = absolute(Path::new(raw))?;
            if !path.is_file() {
                return Err(format!("音频文件不存在：{}", path.display()));
            }
            Some(path)
        }
        None => None,
    };
    let audio_opts = audio_options(args)?;
    let keep_frames = opt_bool(args, "keep_frames")?.unwrap_or(false);
    let resume = opt_bool(args, "resume")?.unwrap_or(false);
    require_batch_confirm(args, "lumen_export_video")?;
    ensure_out_dir_safe(&root, &out_dir)?;
    if session.is_source(&video_out) {
        return Err(format!("拒绝覆盖源图：{}", video_out.display()));
    }
    if video_out.exists() && !confirm(args) {
        return Err(format!(
            "目标已存在，需 confirm: true：{}",
            video_out.display()
        ));
    }
    // 先找 ffmpeg：缺了就别白跑一遍逐帧渲染。
    let ffmpeg = export::find_ffmpeg()
        .ok_or_else(|| "找不到 ffmpeg（设 LUMEN_FFMPEG 或加进 PATH）".to_owned())?;

    let paths = session.require_seq()?.paths();
    // copy：这一次导出改用某套副本的调色；省略就是会话当前这套。
    let copy = opt_str(args, "copy")?.unwrap_or_default();
    let params = session.frame_params_of(n, copy)?;
    let gains = session.gains(n);
    let (crop, offsets) = session.alignment(n);
    let options = RenderOptions {
        mode: OutputMode::Video,
        format,
        quality: 90,
        keep_exif: false,
        // 视频的中间帧是过程产物，不发旁车。
        xmp_sidecar: false,
        sharpen_for,
        sharpen_strength,
        out_dir: out_dir.clone(),
        video_out: video_out.clone(),
        crop,
        rotate: 0.0,
        long_side,
        target_size,
        fps,
        codec,
        crf: crf as u8,
        preset,
        audio,
        audio_opts,
        keep_frames,
        resume,
        color_space,
        watermark: opt_watermark(args)?,
        // 运镜跟着这次导出走：没传 motion 就沿用会话工程里那份，与队列同口径。
        motion: motion_arg(session, args)?,
        metadata: session.metadata(),
    };
    let plan = ExportPlan {
        kind: ExportKind::Video,
        paths,
        params,
        gains,
        offsets,
        options,
        ffmpeg: Some(ffmpeg),
        deflicker: session.analysis.is_some(),
        keyframes: session.keyframes.len(),
        retime: retime_plan(args, n, fps)?,
    };
    let background = opt_bool(args, "background")?.unwrap_or(false);
    if background {
        return start_export_background(session, plan);
    }
    run_export_sync(session, plan)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportKind {
    Stills,
    Video,
}

/// `retime` 参数 → 变速设定：`{ enabled?, shutter?, speed? | keys? | ease? }`。
/// 口径与 GUI 的 `retime.set` 一致：`at` 是源进度 0..1，三种曲线写法互斥。
fn parse_retime(raw: &Value) -> Result<Retime, String> {
    if !raw.is_object() {
        return Err("retime 要写成对象：{ enabled, shutter, speed / keys / ease }".to_owned());
    }
    let shutter = opt_f64(raw, "shutter")?.unwrap_or(0.0);
    if !(0.0..=1.0).contains(&shutter) {
        return Err(format!(
            "retime.shutter 是曝光区间占本帧源跨度的比例，要在 0..1（0 锐利、1 全平均），实得 {shutter}"
        ));
    }
    let speed = raw.get("speed").filter(|v| !v.is_null());
    let keys = raw.get("keys").filter(|v| !v.is_null());
    let ease = raw.get("ease").filter(|v| !v.is_null());
    if [speed, keys, ease].iter().filter(|v| v.is_some()).count() > 1 {
        return Err(
            "retime 的 speed / keys / ease 只能选一种写法：speed 全程一个倍速、keys 多点曲线、ease 缓入缓出"
                .to_owned(),
        );
    }
    let enabled = opt_bool(raw, "enabled")?.unwrap_or(true);
    let mut retime = Retime {
        enabled,
        keys: Vec::new(),
        shutter,
    };
    if let Some(v) = speed {
        let speed = v
            .as_f64()
            .filter(|f| f.is_finite())
            .ok_or("retime.speed 必须是有限数值")?;
        retime.keys = vec![SpeedKey { at: 0.0, speed }, SpeedKey { at: 1.0, speed }];
    } else if let Some(v) = keys {
        retime.keys = parse_speed_keys(v)?;
    } else if let Some(v) = ease {
        let edge = opt_f64(v, "edge")?.unwrap_or(1.0);
        let peak = opt_f64(v, "peak")?
            .ok_or_else(|| "retime.ease 里必须给 peak（中段倍速）".to_owned())?;
        let in_at = opt_f64(v, "in_at")?.unwrap_or(0.25);
        let out_at = opt_f64(v, "out_at")?.unwrap_or(0.75);
        retime.keys = Retime::ease(edge, peak, in_at, out_at, shutter).keys;
    }
    Ok(retime)
}

/// 速度键数组：`[{ at, speed }, …]`，`at` 夹进 0..1。
fn parse_speed_keys(raw: &Value) -> Result<Vec<SpeedKey>, String> {
    let items = raw
        .as_array()
        .ok_or("retime.keys 要写成数组：[{ at: 0, speed: 1 }, { at: 0.5, speed: 4 }]")?;
    if items.is_empty() {
        return Err("retime.keys 是空数组：要全程常速就写 speed: 1，别清空速度键".to_owned());
    }
    items
        .iter()
        .map(|item| {
            let at = opt_f64(item, "at")?
                .ok_or_else(|| "每个速度键都要有 at（源进度 0..1）".to_owned())?;
            let speed = opt_f64(item, "speed")?
                .ok_or_else(|| "每个速度键都要有 speed（倍速，大于 0）".to_owned())?;
            Ok(SpeedKey {
                at: at.clamp(0.0, 1.0),
                speed,
            })
        })
        .collect()
}

/// 没传 `retime` 就不建计划（1:1 快路）；传了但算出来是恒等也当没传。
fn retime_plan(args: &Value, n_src: usize, fps: f64) -> Result<Option<retime::Plan>, String> {
    let Some(raw) = args.get("retime").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let retime = parse_retime(raw)?;
    export::plan_for(&retime, n_src, fps).map_err(|e| e.to_string())
}

/// 导出任务的完整快照：验证阶段结束后就与 `Session` 解耦，可以安全丢进后台线程。
struct ExportPlan {
    kind: ExportKind,
    paths: Vec<PathBuf>,
    params: Vec<GradeParams>,
    gains: Vec<Ev>,
    offsets: Vec<[f64; 2]>,
    options: RenderOptions,
    ffmpeg: Option<PathBuf>,
    deflicker: bool,
    keyframes: usize,
    /// 时间重映射计划；`None` 是 1:1 原速，导出走原来的快路。
    retime: Option<retime::Plan>,
}

impl ExportPlan {
    fn target(&self) -> String {
        match self.kind {
            ExportKind::Stills => self.options.out_dir.display().to_string(),
            ExportKind::Video => self.options.video_out.display().to_string(),
        }
    }

    /// 进度按输出的帧数计：变速 2× 时源帧数翻倍出帧、压帧时减半，
    /// 拿源帧数当总数会让进度条跑到一半就"超额"。
    fn out_frames(&self) -> usize {
        self.retime
            .as_ref()
            .map_or(self.paths.len(), |plan| plan.frames())
    }

    fn run(self, shared: &JobShared) -> Result<JobOutcome, String> {
        let progress = |p: JobProgress| {
            let stage = match p.stage {
                Stage::Frames => STAGE_FRAMES,
                Stage::Encode => STAGE_ENCODE,
            };
            shared.set_stage(stage, p.done, p.total);
        };
        let ExportPlan {
            kind,
            paths,
            params,
            gains,
            offsets,
            options,
            ffmpeg,
            deflicker,
            keyframes,
            retime,
        } = self;
        let frames = paths.len();
        let out_frames = retime.as_ref().map_or(frames, |plan| plan.frames());
        // 续跑跳过几帧必须写在结果里：不开 resume 时它是 0，开着却跳过大半时
        // 一个 agent 只看「frames: 480」会把「接着导」误当成「重渲了一遍」。
        let skipped = export::existing_frames(&options, out_frames);
        let retime_json = retime
            .as_ref()
            .map(|plan| json!({ "frames": plan.frames(), "seconds": round6(plan.seconds) }));
        match kind {
            ExportKind::Stills => {
                let files = export::export_stills_planned(
                    &paths,
                    &params,
                    &gains,
                    &offsets,
                    &options,
                    retime.as_ref(),
                    shared.cancel_flag(),
                    &progress,
                )
                .map_err(export_failure)?;
                let summary = json!({
                    "out_dir": options.out_dir.display().to_string(),
                    "frames": files.len(),
                    "source_frames": frames,
                    "skipped": skipped,
                    "retime": retime_json,
                    "format": format_name(options.format),
                    "quality": options.quality,
                    "sharpen_for": options.sharpen_for.key(),
                    "watermark_applies": options.watermark.is_on(),
                    "long_side": options.long_side,
                    "target_size": options.target_size,
                    "crop": options.crop,
                    "aligned": options.crop.is_some(),
                    "first": files.first().map(|p| p.display().to_string()),
                    "last": files.last().map(|p| p.display().to_string()),
                    "deflicker_applied": deflicker,
                    "keyframes_applied": keyframes > 0,
                });
                Ok(JobOutcome::Export {
                    path: options.out_dir,
                    summary,
                })
            }
            ExportKind::Video => {
                let ffmpeg = ffmpeg
                    .ok_or_else(|| "找不到 ffmpeg（设 LUMEN_FFMPEG 或加进 PATH）".to_owned())?;
                let video = export::export_video_planned(
                    &ffmpeg,
                    &paths,
                    &params,
                    &gains,
                    &offsets,
                    &options,
                    retime.as_ref(),
                    shared.cancel_flag(),
                    &progress,
                )
                .map_err(|e| format!("导出成片失败：{e}"))?;
                let codec_name = match options.codec {
                    Codec::H264 => "h264",
                    Codec::H265 => "h265",
                };
                let summary = json!({
                    "video": video.display().to_string(),
                    "out_dir": options.out_dir.display().to_string(),
                    "frames": out_frames,
                    "source_frames": frames,
                    "skipped": skipped,
                    "retime": retime_json,
                    "codec": codec_name,
                    "crf": options.crf,
                    "fps": options.fps,
                    "long_side": options.long_side,
                    "target_size": options.target_size,
                    "crop": options.crop,
                    "aligned": options.crop.is_some(),
                    "kept_frames": options.keep_frames,
                    "deflicker_applied": deflicker,
                    "keyframes_applied": keyframes > 0,
                });
                Ok(JobOutcome::Export {
                    path: video,
                    summary,
                })
            }
        }
    }
}

/// 取消是正常结果，不要把 core 的「已取消」包成「导出失败」。
fn export_failure(e: export::ExportError) -> String {
    if matches!(e, export::ExportError::Cancelled) {
        "导出已取消".to_owned()
    } else {
        format!("导出失败：{e}")
    }
}

fn run_export_sync(session: &mut Session, plan: ExportPlan) -> Result<Value, String> {
    let shared = Arc::clone(&session.shared);
    shared.begin(JobKind::Export, plan.out_frames());
    let result = plan.run(&shared);
    let summary = match &result {
        Ok(JobOutcome::Export { summary, .. }) => summary.clone(),
        _ => Value::Null,
    };
    session.complete_job(result)?;
    Ok(summary)
}

fn start_export_background(session: &mut Session, plan: ExportPlan) -> Result<Value, String> {
    let frames = plan.out_frames();
    let target = plan.target();
    let shared = Arc::clone(&session.shared);
    shared.begin(JobKind::Export, frames);
    let worker = Arc::clone(&shared);
    let handle = thread::Builder::new()
        .name("lumen-export".to_owned())
        .spawn(move || {
            let result = plan.run(&worker);
            worker.finish(result);
        })
        .map_err(|e| {
            let message = format!("启动后台导出失败：{e}");
            shared.finish(Err(message.clone()));
            message
        })?;
    session.start_background(shared, handle);
    Ok(json!({
        "started": true,
        "background": true,
        "frames": frames,
        "target": target,
        "progress": {"done": 0, "total": frames},
        "note": "用 lumen_align_status 查询进度，lumen_cancel 取消。"
    }))
}

// ------------------------------------------------------------------ 批量队列

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

fn queue_list(session: &mut Session) -> Result<Value, String> {
    session.ensure_queue_loaded();
    Ok(queue_json(session))
}

fn queue_add(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先取消或等它结束再改队列".to_owned());
    }
    session.ensure_queue_loaded();
    let root = match opt_str(args, "root")? {
        Some(text) => absolute(Path::new(text))?,
        None => session.require_root()?,
    };
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
        other => return Err(format!("未知编码 {other}；可用 h264 / h265")),
    };
    let preset = opt_str(args, "preset")?
        .unwrap_or("medium")
        .trim()
        .to_owned();
    if !queue::PRESETS.contains(&preset.as_str()) {
        return Err(format!("未知编码预设 {preset}"));
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
    let item = QueueItem {
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
    };
    let index = queue::add(&mut session.queue, item)?;
    session.save_queue()?;
    let mut out = queue_json(session);
    if let Some(object) = out.as_object_mut() {
        object.insert("added".to_owned(), json!(index + 1));
    }
    Ok(out)
}

fn queue_remove(session: &mut Session, args: &Value) -> Result<Value, String> {
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

fn queue_clear(session: &mut Session) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先取消再清空".to_owned());
    }
    session.ensure_queue_loaded();
    let cleared = session.queue.len();
    session.queue.clear();
    session.save_queue()?;
    Ok(json!({"cleared": cleared, "count": 0}))
}

fn queue_run(session: &mut Session, args: &Value) -> Result<Value, String> {
    if !confirm(args) {
        return Err("lumen_queue_run 会批量写视频，需参数 confirm: true".to_owned());
    }
    if session.is_running() {
        return Err("已有后台任务正在运行，先看 lumen_queue_status 或调用 lumen_cancel".to_owned());
    }
    session.ensure_queue_loaded();
    if session.queue.is_empty() {
        return Err("队列是空的：先用 lumen_queue_add 加入序列".to_owned());
    }
    let ffmpeg = export::find_ffmpeg()
        .ok_or_else(|| "找不到 ffmpeg（设 LUMEN_FFMPEG 或加进 PATH）".to_owned())?;
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
            .name("lumen-queue".to_owned())
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
            "note": "用 lumen_queue_status 查询进度，lumen_queue_cancel 或 lumen_cancel 取消。"
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

fn queue_status(session: &mut Session) -> Result<Value, String> {
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

fn queue_cancel(session: &mut Session) -> Result<Value, String> {
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

fn align_run(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("已有后台任务正在运行，先看 lumen_align_status 或调用 lumen_cancel".to_owned());
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
            .name("lumen-align".to_owned())
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
            "note": "用 lumen_align_status 查询进度，lumen_cancel 取消；完成后报告会自动存进会话。"
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
/// 几次，但 `lumen_cancel` 能在下一块开始前真正停下（后台模式下）。
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

fn align_status(session: &mut Session) -> Result<Value, String> {
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
        "note": "running 为 true 时可用 lumen_cancel 请求取消；同步调用期间请求会排队，取消只对后台任务即时生效。"
    }))
}

/// 取消正在跑的任务。空闲时也返回成功，重复调用安全（幂等）。
fn cancel(session: &mut Session) -> Result<Value, String> {
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

fn project_get(session: &mut Session) -> Result<Value, String> {
    let snapshot = session.snapshot_project()?;
    let path = session.project_path()?;
    Ok(json!({
        "path": path.display().to_string(),
        "exists": path.is_file(),
        "project": to_value!(&snapshot),
    }))
}

fn project_save(session: &mut Session) -> Result<Value, String> {
    let snapshot = session.snapshot_project()?;
    let path = session.project_path()?;
    project::save(&path, &snapshot).map_err(|e| format!("保存工程失败：{e}"))?;
    session.project = Some(snapshot);
    Ok(json!({"path": path.display().to_string(), "saved": true}))
}

fn journal_list(session: &Session, args: &Value) -> Result<Value, String> {
    let root = session.require_root()?;
    let limit = opt_u32(args, "limit")?.unwrap_or(50).clamp(1, 500) as usize;
    let entries = lumen_core::journal::tail(&root, limit);
    Ok(json!({
        "path": lumen_core::journal::path(&root).display().to_string(),
        "count": entries.len(),
        "entries": entries,
    }))
}

fn xmp_read(args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path.is_file() {
        return Err(format!("文件不存在：{}", path.display()));
    }
    let params = xmp::read(&path).map_err(|e| format!("读 XMP 失败：{e}"))?;
    Ok(json!({
        "path": path.display().to_string(),
        "params": to_value!(&params),
    }))
}

fn xmp_write(session: &mut Session, args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
    {
        return Err("path 必须以 .xmp 结尾".to_owned());
    }
    if session.is_source(&path) {
        return Err(format!("拒绝覆盖源图：{}", path.display()));
    }
    if path.exists() && !confirm(args) {
        return Err(format!("目标已存在，需 confirm: true：{}", path.display()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    xmp::write(&path, &session.params).map_err(|e| format!("写 XMP 失败：{e}"))?;
    Ok(json!({
        "path": path.display().to_string(),
        "preset": session.preset,
    }))
}

/// 挂一张外部 .cube 作为「外观」：解析、报出格点信息，默认同时应用到会话参数。
fn cube_import(session: &mut Session, args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path.is_file() {
        return Err(format!("文件不存在：{}", path.display()));
    }
    let key = path.to_string_lossy().into_owned();
    let lut = cube::refresh(&key).map_err(|e| format!("读 .cube 失败：{e}"))?;
    let info = cube::describe(&lut);
    let apply = opt_bool(args, "apply")?.unwrap_or(true);
    if apply {
        session.params.look_cube = key.clone();
        session.preset = "自定义".to_owned();
    }
    Ok(json!({
        "path": key,
        "title": info.title,
        "size": info.size,
        "nodes": info.nodes,
        "domain_min": info.domain_min,
        "domain_max": info.domain_max,
        "neutral": info.neutral,
        "applied": apply,
    }))
}

/// 把当前参数烘成一张 .cube（只含逐点运算，与 Python 的 `write_cube` 同一条路）。
fn cube_export(session: &Session, args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cube"))
    {
        return Err("path 必须以 .cube 结尾".to_owned());
    }
    if path.exists() && !confirm(args) {
        return Err(format!("目标已存在，需 confirm: true：{}", path.display()));
    }
    let size = opt_u32(args, "size")?.unwrap_or(cube::DEFAULT_SIZE as u32) as usize;
    if !(cube::MIN_SIZE..=cube::MAX_SIZE).contains(&size) {
        return Err(format!(
            "size 只能在 {}~{} 之间",
            cube::MIN_SIZE,
            cube::MAX_SIZE
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let text =
        cube::export_text(&session.params, size).map_err(|e| format!("写 .cube 失败：{e}"))?;
    fs::write(&path, &text).map_err(|e| format!("写 .cube 失败：{e}"))?;
    Ok(json!({
        "path": path.display().to_string(),
        "size": size,
        "nodes": size * size * size,
        "bytes": text.len(),
        "look": session.params.look_cube,
    }))
}

// ------------------------------------------------------------------ 参数与安全检查
const X264_PRESETS: &[&str] = &[
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
    "placebo",
];

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("缺少字符串参数 {key}"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是字符串")),
    }
}

fn opt_u32(args: &Value, key: &str) -> Result<Option<u32>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let n = value
                .as_u64()
                .or_else(|| {
                    value.as_f64().and_then(|f| {
                        (f.is_finite() && f >= 0.0 && f.fract() == 0.0).then_some(f as u64)
                    })
                })
                .and_then(|n| u32::try_from(n).ok());
            n.map(Some).ok_or_else(|| format!("{key} 必须是非负整数"))
        }
    }
}

fn opt_f64(args: &Value, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是有限数值")),
    }
}

fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是布尔值")),
    }
}

fn confirm(args: &Value) -> bool {
    args.get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `watermark` 参数：整块按 `Watermark` 自己的 serde 形状收，键名与工程文件、
/// `render.set` 一致。这里没有「面板当前值」可合并，所以只写部分键时，没写的
/// 键取默认值——不传整个参数就等于没开水印。
fn opt_watermark(args: &Value) -> Result<Watermark, String> {
    match args.get("watermark") {
        None | Some(Value::Null) => Ok(Watermark::default()),
        Some(v) => {
            serde_json::from_value(v.clone()).map_err(|e| format!("watermark 参数不对：{e}"))
        }
    }
}

/// `motion` 参数 → 运镜设定：没传就沿用会话工程里那一份（与队列同口径），传了就在
/// 当前值上打补丁——面板上定好的取景框不该因为这次没写这个键就悄悄丢掉。
fn motion_arg(session: &Session, args: &Value) -> Result<Motion, String> {
    let base = session
        .project
        .as_ref()
        .map(|p| p.render.motion)
        .unwrap_or_default();
    match args.get("motion").filter(|v| !v.is_null()) {
        None => Ok(base),
        Some(raw) => raw
            .as_object()
            .ok_or_else(|| "motion 要写成对象：{ enabled, from, to, ease, swap, refit }".to_owned())
            .and_then(|_| base.patched(raw)),
    }
}

/// `lumen_export_video` 的配乐选项；全部可省，缺省与 GUI 默认一致。
fn audio_options(args: &Value) -> Result<AudioOptions, String> {
    let mut opts = AudioOptions::default();
    if let Some(delay) = opt_f64(args, "delay")? {
        if !(0.0..=3600.0).contains(&delay) {
            return Err("delay 必须在 0..3600 秒".to_owned());
        }
        opts.delay = delay;
    }
    if let Some(gain) = opt_f64(args, "gain")? {
        if !(0.0..=400.0).contains(&gain) {
            return Err("gain 必须在 0..400（100 为原始音量）".to_owned());
        }
        opts.gain = gain;
    }
    if let Some(fade) = opt_f64(args, "fade_in")? {
        if !(0.0..=3600.0).contains(&fade) {
            return Err("fade_in 必须在 0..3600 秒".to_owned());
        }
        opts.fade_in = fade;
    }
    if let Some(fade) = opt_f64(args, "fade_out")? {
        if !(0.0..=3600.0).contains(&fade) {
            return Err("fade_out 必须在 0..3600 秒".to_owned());
        }
        opts.fade_out = fade;
    }
    if let Some(looped) = opt_bool(args, "loop")? {
        opts.looped = looped;
    }
    if let Some(trim) = opt_bool(args, "trim")? {
        opts.trim = trim;
    }
    if let Some(codec) = opt_str(args, "acodec")? {
        opts.acodec = AudioCodec::parse(codec).ok_or_else(|| {
            format!("未知 acodec {codec}；可用 auto / copy / aac128 / aac192 / aac256 / aac320")
        })?;
    }
    Ok(opts)
}

fn require_batch_confirm(args: &Value, tool: &str) -> Result<(), String> {
    if confirm(args) {
        Ok(())
    } else {
        Err(format!("{tool} 是批量写操作，需参数 confirm: true"))
    }
}

fn check_long_side(long_side: Option<u32>, args: &Value) -> Result<(), String> {
    match long_side {
        Some(side) if side > MAX_LONG_SIDE && !confirm(args) => Err(format!(
            "long_side {side} 超过 {MAX_LONG_SIDE}，需 confirm: true"
        )),
        _ => Ok(()),
    }
}

/// 精确输出宽高（可选）：`width` 与 `height` 必须同时给，偶数且 ≥2。
///
/// 与 `long_side` 二选一，避免「按长边缩」和「精确画幅」同时生效时含义打架。
fn target_size_arg(args: &Value) -> Result<Option<(u32, u32)>, String> {
    match (opt_u32(args, "width")?, opt_u32(args, "height")?) {
        (Some(width), Some(height)) => {
            if args.get("long_side").is_some() {
                return Err("width/height 与 long_side 只能二选一".to_owned());
            }
            if width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1 {
                return Err(format!(
                    "width/height 必须是偶数且不小于 2：{width}×{height}"
                ));
            }
            Ok(Some((width, height)))
        }
        (None, None) => Ok(None),
        _ => Err("width 与 height 需同时给出（或都不给）".to_owned()),
    }
}

fn ensure_out_dir_safe(root: &Path, out: &Path) -> Result<(), String> {
    let root = normalize(root);
    let out = normalize(out);
    if same_path(&out, &root) {
        return Err("输出目录不能是序列根目录，会污染源帧".to_owned());
    }
    // 输出到根的上级更危险：批量写 + 收尾清理可能碰到源目录。
    if path_starts_with(&root, &out) {
        return Err("输出目录不能是序列根目录的上级".to_owned());
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("路径为空".to_owned());
    }
    Ok(normalize(path))
}

fn parse_format(name: &str) -> Result<StillFormat, String> {
    StillFormat::parse(name).ok_or_else(|| {
        format!(
            "未知格式 {}；可用 {}",
            name.trim(),
            StillFormat::ALL
                .iter()
                .map(|f| f.key())
                .collect::<Vec<_>>()
                .join(" / ")
        )
    })
}

/// 有损容器只当最终交付：视频那条路的中间帧必须是 PNG/TIFF。
fn check_video_format(format: StillFormat) -> Result<(), String> {
    if format.frames_ok() {
        return Ok(());
    }
    Err(format!(
        "{} 只能出静帧；视频的中间帧要 png8 或 tiff16",
        format.cn()
    ))
}

fn opt_quality(args: &Value) -> Result<u8, String> {
    match opt_u32(args, "quality")? {
        Some(q) if (1..=100).contains(&q) => Ok(q as u8),
        Some(q) => Err(format!("quality 要在 1..100：{q}")),
        None => Ok(90),
    }
}

/// 输出锐化两个档：默认 `off`，无头导出与 GUI 面板的缺省一致。
fn opt_sharpen(args: &Value) -> Result<(OutputSharpen, SharpenStrength), String> {
    let for_ = match opt_str(args, "sharpen_for")? {
        Some(raw) => OutputSharpen::parse(raw)
            .ok_or_else(|| format!("未知锐化用途 {raw}；可用 off / screen / glossy / matte"))?,
        None => OutputSharpen::Off,
    };
    let strength = match opt_str(args, "sharpen_strength")? {
        Some(raw) => SharpenStrength::parse(raw)
            .ok_or_else(|| format!("未知锐化强度 {raw}；可用 low / standard / high"))?,
        None => SharpenStrength::Standard,
    };
    Ok((for_, strength))
}

/// `color_space` 参数：省略即 sRGB（跟改造前的导出行为一致）。
fn opt_color_space(args: &Value) -> Result<ColorSpace, String> {
    match opt_str(args, "color_space")? {
        Some(raw) => ColorSpace::parse(raw)
            .ok_or_else(|| format!("未知色域 {raw}；可用 srgb / p3 / rec2020")),
        None => Ok(ColorSpace::default()),
    }
}

fn format_name(format: StillFormat) -> &'static str {
    format.key()
}

fn parse_codec(name: &str) -> Result<Codec, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "h264" | "264" | "avc" => Ok(Codec::H264),
        "h265" | "265" | "hevc" => Ok(Codec::H265),
        other => Err(format!("未知编码 {other}；可用 h264 / h265")),
    }
}

fn parse_estimator(name: &str) -> Result<Estimator, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "template" | "tpl" => Ok(Estimator::Template),
        "phase" => Ok(Estimator::Phase),
        other => Err(format!("未知 estimator {other}；可用 template / phase")),
    }
}

fn estimator_name(estimator: Estimator) -> &'static str {
    match estimator {
        Estimator::Template => "template",
        Estimator::Phase => "phase",
    }
}

/// 必填帧号：缺参、非整数、越界都返回结构化中文错误。
fn arg_frame(session: &Session, args: &Value, key: &str) -> Result<usize, String> {
    let frame = opt_u32(args, key)?.ok_or_else(|| format!("缺少整数参数 {key}"))? as usize;
    let n = session.require_seq()?.len();
    if frame >= n {
        return Err(format!("{key} {frame} 超出范围 0..{}", n.saturating_sub(1)));
    }
    Ok(frame)
}

fn stride_for(n: usize) -> usize {
    n.div_ceil(MAX_SERIES).max(1)
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

fn summarize(values: impl Iterator<Item = Option<f64>>) -> Value {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut count = 0usize;
    for value in values.flatten().filter(|v| v.is_finite()) {
        min = min.min(value);
        max = max.max(value);
        sum += value;
        count += 1;
    }
    if count == 0 {
        return json!({"count": 0});
    }
    json!({
        "count": count,
        "min": round6(min),
        "max": round6(max),
        "avg": round6(sum / count as f64),
    })
}

// ------------------------------------------------------------------ 注册表

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// `retime` 参数的 schema：变速 + 运动模糊，两种导出共用同一套口径。
fn retime_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "description": "时间重映射（变速 + 运动模糊）；省略就是 1:1 原速，导出像素与不开时逐位一致",
        "properties": {
            "enabled": {"type": "boolean", "default": true, "description": "关掉等于不做变速"},
            "speed": {"type": "number", "exclusiveMinimum": 0, "description": "全程一个倍速：2 是两帧压一帧，0.5 是一帧拆两帧；与 keys/ease 互斥"},
            "keys": {"type": "array", "minItems": 1, "description": "多点速度曲线，键之间线性", "items": {"type": "object", "additionalProperties": false, "required": ["at", "speed"], "properties": {"at": {"type": "number", "minimum": 0, "maximum": 1, "description": "源进度（0=序列开头，1=结尾），不是输出秒"}, "speed": {"type": "number", "exclusiveMinimum": 0, "description": "该处倍速；要停住写得很小，别写 0"}}}},
            "ease": {"type": "object", "additionalProperties": false, "required": ["peak"], "description": "缓入缓出：两头 edge 倍速，中段 peak 倍速", "properties": {"edge": {"type": "number", "exclusiveMinimum": 0, "default": 1}, "peak": {"type": "number", "exclusiveMinimum": 0}, "in_at": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.25}, "out_at": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.75}}},
            "shutter": {"type": "number", "minimum": 0, "maximum": 1, "default": 0, "description": "曝光区间占本帧源跨度的比例：0 只交叉淡化相邻两帧，1 把跨过的源帧按重叠时长全平均（车流变光带）；不做光流补帧"}
        }
    })
}

fn motion_schema() -> Value {
    json!({
        "type": "object",
        "description": "运镜（Ken Burns）：取景框从起点走到终点。省略就沿用会话工程里那一份；工程里也没有就是不开，导出像素与不开时逐位一致。",
        "additionalProperties": false,
        "properties": {
            "enabled": {"type": "boolean", "description": "总开关；关着、或两个框重合，导出一像素都不重采样"},
            "from": {"type": "array", "minItems": 4, "maxItems": 4, "items": {"type": "number"}, "description": "起点框 [x, y, 宽, 高]，相对拉直+裁切后那一幅归一化 0..1；也接受 {x,y,w,h}。越界由内核夹回画面内"},
            "to": {"type": "array", "minItems": 4, "maxItems": 4, "items": {"type": "number"}, "description": "终点框，写法同 from"},
            "ease": {"type": "string", "enum": ["smooth", "linear", "in", "out"], "description": "走动节奏：smooth 缓入缓出、linear 匀速、in 越推越快、out 越推越慢"},
            "swap": {"type": "boolean", "description": "true = 起点与终点互换"},
            "refit": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "exclusiveMinimum": 0}, "description": "[宽, 高] 新画幅跨度：按起点框的变化把终点框等比例带过去"},
            "reset": {"type": "boolean", "description": "true = 整块清回默认（关运镜、整幅起、内收止、缓入缓出）"}
        }
    })
}

fn build_commands() -> Vec<Command> {
    vec![
        Command {
            id: "lumen_open_sequence",
            description: "打开一个帧文件夹作为当前序列；已有 .lumen/project.json 时自动恢复参数。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "帧文件夹路径（绝对或相对）"}
                }),
                &["root"],
            ),
        },
        Command {
            id: "lumen_sequence_info",
            description: "返回当前序列的根目录、帧数、调色参数、去闪烁设置与最近输出。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_measure",
            description: "逐帧测光（均值/中值/中心/锐度），返回统计与降采样序列；已测过时复用缓存，refresh 可强制重测。",
            input_schema: object(
                json!({
                    "refresh": {"type": "boolean", "default": false, "description": "true 时忽略已缓存的测光结果，重新逐帧测量"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台测光并立即返回，用 lumen_align_status 查进度"}
                }),
                &[],
            ),
        },
        Command {
            id: "lumen_deflicker_analyze",
            description: "分析曝光闪烁并保存逐帧 EV 补偿；之后预览与导出会自动叠加。channels 为 true 时按 R/G/B 三通道分别校正，压的是彩色闪烁。",
            input_schema: object(
                json!({
                    "window": {"type": "number", "minimum": 3, "maximum": 199, "description": "平滑窗口（奇数，默认 15）"},
                    "strength": {"type": "number", "minimum": 0, "maximum": 100, "description": "校正强度百分比（默认 100）"},
                    "multi": {"type": "boolean", "description": "长序列多遍窗口（默认 false）"},
                    "channels": {"type": "boolean", "description": "分通道（RGB）去闪烁，治色闪；默认 false 只补亮度"}
                }),
                &[],
            ),
        },
        Command {
            id: "lumen_grade_get",
            description: "读取当前调色参数与预设名。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_grade_set",
            description: "按字段合并调色参数，例如 {\"exposure\":0.5,\"contrast\":12}；未知字段会报错。",
            input_schema: object(
                json!({
                    "params": {
                        "type": "object",
                        "description": "GradeParams 的部分字段：exposure/contrast/highlights/shadows/whites/blacks/temperature/tint/vibrance/saturation/clarity/texture/dehaze/grain/vignette/曲线与 HSL 等",
                        "additionalProperties": true
                    }
                }),
                &["params"],
            ),
        },
        Command {
            id: "lumen_grade_preset",
            description: "应用一个内置调色预设（原始、鲜艳风光、落日暖调、冷色电影等）。",
            input_schema: object(
                json!({
                    "name": {"type": "string", "description": "预设名：原始/鲜艳风光/落日暖调/冷色电影/复古胶片/黑白对比/夜景增强/通透风光/柔和人像/青橙电影"}
                }),
                &["name"],
            ),
        },
        Command {
            id: "lumen_grade_reset",
            description: "把调色参数恢复为默认值（预设「原始」）。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_grade_copy",
            description: "命名调色副本：同一序列多套调色并存。op=list/save/apply/delete/rename（省=list）；save 把当前调色+关键帧存成 name（同名需 overwrite:true），apply 取回当前；导出与队列可按副本名指定用哪一套。",
            input_schema: object(
                json!({
                    "op": {"type": "string", "enum": ["list", "save", "apply", "delete", "rename"], "description": "省略即 list"},
                    "name": {"type": "string", "description": "副本名（save/apply/delete 必填，rename 填旧名）"},
                    "overwrite": {"type": "boolean", "description": "save 同名覆盖；默认报错"},
                    "to": {"type": "string", "description": "rename 的新名字"}
                }),
                &[],
            ),
        },
        Command {
            id: "lumen_keyframe_add",
            description: "在第 frame 帧钉一个关键帧；不传 params 时用当前基准参数，传了则在其上做字段合并。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "帧号"},
                    "params": {
                        "type": "object",
                        "description": "可选的部分调色字段，与 lumen_grade_set 的 params 同口径",
                        "additionalProperties": true
                    }
                }),
                &["frame"],
            ),
        },
        Command {
            id: "lumen_keyframe_remove",
            description: "删除第 frame 帧的关键帧；该帧没有关键帧时返回结构化错误。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "帧号"}
                }),
                &["frame"],
            ),
        },
        Command {
            id: "lumen_keyframe_list",
            description: "列出全部关键帧（帧号与参数）；可选 frame 顺便返回该帧插值后的参数。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "可选：查询该帧插值后的参数"}
                }),
                &[],
            ),
        },
        Command {
            id: "lumen_render_preview",
            description: "渲染一帧带当前调色、关键帧插值与去闪烁补偿的预览 PNG，返回落盘路径与尺寸。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "帧号，默认 0"},
                    "side": {"type": "integer", "minimum": 1, "description": "预览长边像素，默认 1280；超过 4096 需 confirm"},
                    "out": {"type": "string", "description": "输出 PNG 路径；已有文件需 confirm"},
                    "confirm": {"type": "boolean", "description": "覆盖已有文件或 side > 4096 时必需"}
                }),
                &["out"],
            ),
        },
        Command {
            id: "lumen_export_stills",
            description: "把整个序列按当前调色、关键帧插值、去闪烁补偿与对齐结果导出为静帧序列（PNG/TIFF/JPEG/WebP/AVIF，批量写，需 confirm）；可选 retime 变速、运动模糊与运镜（Ken Burns）。",
            input_schema: object(
                json!({
                    "out_dir": {"type": "string", "description": "输出帧目录（不能是序列根或其上级）"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8", "description": "AVIF 走随包 ffmpeg 的 libaom-av1，色域写 nclx 标记、不带 ICC/EXIF，逐帧起进程会比较慢"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "每张静帧旁边写一个同名 .xmp 旁车：dc: 的说明与版权四项（取自会话工程的元数据）+ crs: 的这一张调色参数"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：输出目录里已经写完的帧（连带该带的 .xmp）原样留着，这一趟只补没写出来的那些；上次跑到一半断了、这次接着导就用它，改了调色想重导就别开"},
                    "copy": {"type": "string", "description": "改用某套命名调色副本的调色导出；省略用会话当前调色"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时像素真转换并嵌对应 ICC（p3=Display P3，rec2020=BT.2020 SDR）"},
                    "long_side": {"type": "integer", "minimum": 1, "description": "按长边缩放；与 width/height 二选一"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；cover 放大后居中裁切）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给）"},
                    "retime": retime_schema(),
                    "motion": motion_schema(),
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台导出并立即返回，用 lumen_align_status 查进度"}
                }),
                &["out_dir"],
            ),
        },
        Command {
            id: "lumen_export_video",
            description: "导出帧序列并用 ffmpeg 编码成 H.264/H.265 视频；自动套用关键帧插值、去闪烁与对齐裁切，可选 retime 变速、运动模糊与运镜（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "out_dir": {"type": "string", "description": "中间帧目录（不能是序列根或其上级）"},
                    "video_out": {"type": "string", "description": "成片文件路径，如 D:/out/clip.mp4"},
                    "codec": {"type": "string", "enum": ["h265", "h264"], "default": "h265"},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 51, "default": 18},
                    "preset": {"type": "string", "default": "medium", "description": "x264/x265 速度档：ultrafast…veryslow"},
                    "fps": {"type": "number", "exclusiveMinimum": 0, "maximum": 240, "default": 24},
                    "long_side": {"type": "integer", "minimum": 1, "description": "按长边缩放；与 width/height 二选一"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；cover 放大后居中裁切）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给）"},
                    "audio": {"type": "string", "description": "可选配乐文件；序列本身没有原声，因此是替换式混入"},
                    "delay": {"type": "number", "minimum": 0, "maximum": 3600, "description": "配乐延后进入（秒）"},
                    "gain": {"type": "number", "minimum": 0, "maximum": 400, "description": "配乐音量百分比，100 为原始"},
                    "fade_in": {"type": "number", "minimum": 0, "maximum": 3600, "description": "淡入时长（秒）"},
                    "fade_out": {"type": "number", "minimum": 0, "maximum": 3600, "description": "淡出时长（秒）"},
                    "loop": {"type": "boolean", "default": false, "description": "配乐短于画面时循环铺满"},
                    "trim": {"type": "boolean", "default": true, "description": "配乐长于画面时截到画面结束"},
                    "acodec": {"type": "string", "enum": ["auto", "copy", "aac128", "aac192", "aac256", "aac320"], "default": "auto", "description": "配乐编码；自动模式能复制就复制，否则 AAC 192k"},
                    "keep_frames": {"type": "boolean", "default": false, "description": "编码后保留中间帧"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：输出目录里已经写完的帧原样留着，这一趟只补没写出来的那些（改了参数想重导就别开）"},
                    "copy": {"type": "string", "description": "改用某套命名调色副本的调色导出；省略用会话当前调色"},
                    "format": {"type": "string", "enum": ["png8", "tiff16"], "default": "png8"},
                    "retime": retime_schema(),
                    "motion": motion_schema(),
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；决定 VUI 的 primaries/transfer/matrix 与中间帧的 ICC，rec2020 的传递函数在 VUI 里只能标 bt709"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台导出并立即返回，用 lumen_align_status 查进度"}
                }),
                &["out_dir", "video_out"],
            ),
        },
        Command {
            id: "lumen_hdr_merge",
            description: "把同场景多张不同曝光融成一张静帧（Mertens 曝光融合）；自动套用对齐的统一裁切与逐帧位移，尺寸对不齐会拒绝（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要融合的帧下标（0 开始）；给了就忽略 center/around"},
                    "center": {"type": "integer", "minimum": 0, "description": "包围中心帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 4, "default": 1, "description": "中心前后各取几帧（1..4，最多凑成 9 张）"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 HDR/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "lumen_pano_merge",
            description: "把相邻几张有重叠的照片拼成一张宽幅全景（特征点匹配 + RANSAC 单应 + 羽化融合）；不套对齐位移，靠特征点自己求几何（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要拼接的帧下标（0 开始，按接片顺序）；给了就忽略 start/around"},
                    "start": {"type": "integer", "minimum": 0, "description": "起始帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 11, "default": 1, "description": "起始帧之后再接几张（1..11，最多凑成 12 张）"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 全景/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "lumen_project_get",
            description: "返回当前会话对应的工程内容（含参数、去闪烁设置）与磁盘上的工程路径。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_project_save",
            description: "把当前参数与去闪烁设置写入 <序列>/.lumen/project.json，保留评级与关键帧。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_journal_list",
            description: "读取当前序列的操作日志末尾（只记成功的写操作，一行一条 JSON）。",
            input_schema: object(
                json!({
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50, "description": "最多返回条数"}
                }),
                &[],
            ),
        },
        Command {
            id: "lumen_xmp_read",
            description: "读取一个 XMP 预设/旁车文件，返回可用的调色参数（不自动应用）。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": "XMP 文件路径"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "lumen_xmp_write",
            description: "把当前调色参数写成一个 XMP 文件；覆盖已有文件需 confirm。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": "输出 .xmp 路径"},
                    "confirm": {"type": "boolean", "description": "覆盖已有文件时必需"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "lumen_cube_import",
            description: "把一张外部 .cube 三维 LUT 挂成「外观」（主链之前先套一层查表）；返回格点信息，apply=false 只解析不生效。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": ".cube 文件路径"},
                    "apply": {"type": "boolean", "default": true, "description": "是否立刻应用到当前参数"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "lumen_cube_export",
            description: "把当前调色参数导出成 .cube LUT（只含逐点运算，覆盖已有文件需 confirm）。size 默认 65。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": "输出 .cube 路径"},
                    "size": {"type": "integer", "minimum": 2, "maximum": 256, "default": 65, "description": "立方体边长"},
                    "confirm": {"type": "boolean", "description": "覆盖已有文件时必需"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "lumen_align",
            description: "估计每帧相对参考帧的位移并计算统一裁切框；结果存入会话，导出时自动套用。",
            input_schema: object(
                json!({
                    "reference": {"type": "integer", "minimum": 0, "default": 0, "description": "参考帧下标"},
                    "estimator": {"type": "string", "enum": ["template", "phase"], "default": "template", "description": "template=底部模板匹配，phase=相位相关"},
                    "smooth": {"type": "boolean", "default": false, "description": "是否平滑位移；默认不平滑，与 GUI 对齐结果一致"},
                    "smooth_window": {"type": "integer", "minimum": 0, "maximum": 999, "description": "平滑窗口；0 关闭，smooth=true 且不填时用 15"},
                    "max_shift": {"type": "integer", "minimum": 0, "maximum": 4096, "default": 64, "description": "搜索半径（像素）"},
                    "jump_px": {"type": "number", "minimum": 0, "default": 24, "description": "相邻帧跳变阈值（像素）"},
                    "min_score": {"type": "number", "minimum": -1, "maximum": 1, "default": 0.35, "description": "低于该匹配分记为低分帧"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台对齐并立即返回，用 lumen_align_status 查进度"}
                }),
                &[],
            ),
        },
        Command {
            id: "lumen_align_status",
            description: "查询后台任务（对齐/测光/导出）的进度、是否运行中、上次对齐报告摘要与最近错误。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_cancel",
            description: "请求取消正在运行的后台对齐/测光/导出/队列；空闲时也返回成功，可重复调用（幂等）。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_queue_list",
            description: "列出批量渲染队列（与 GUI 共用 render_queue.json）；工程参数在渲染时从各自的 .lumen/project.json 读取。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_queue_add",
            description: "把一条序列加入渲染队列；同一目录重复加入等于更新规格并挪到队尾。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "帧文件夹；省略时用当前序列"},
                    "out": {"type": "string", "description": "成片路径（.mp4/.mkv/.mov）；省略时放在序列目录下"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；省略=原生）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给；省略=原生）"},
                    "fps": {"type": "number", "exclusiveMinimum": 0, "maximum": 240, "default": 24},
                    "codec": {"type": "string", "enum": ["h265", "h264"], "default": "h265"},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 51, "default": 18},
                    "preset": {"type": "string", "default": "medium", "description": "x264/x265 速度档：ultrafast…veryslow"},
                    "keep_frames": {"type": "boolean", "default": false, "description": "编码后保留中间帧"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：输出目录里已经写完的帧原样留着，这一趟只补没写出来的那些（改了参数想重导就别开）"},
                    "copy": {"type": "string", "description": "该段改用序列工程里某套命名调色副本渲染；省略用工程主调色"}
                }),
                &[],
            ),
        },
        Command {
            id: "lumen_queue_remove",
            description: "从渲染队列移除一段（只改队列，不动素材）。",
            input_schema: object(
                json!({
                    "index": {"type": "integer", "minimum": 1, "description": "队列序号，从 1 开始"}
                }),
                &["index"],
            ),
        },
        Command {
            id: "lumen_queue_clear",
            description: "清空渲染队列（不删除已渲染的成片）；正在渲染时拒绝。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_queue_run",
            description: "逐段渲染整个队列（批量写，必须 confirm）；默认同步跑完，background=true 时立即返回。",
            input_schema: object(
                json!({
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台渲染并立即返回，用 lumen_queue_status 查进度"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "lumen_queue_status",
            description: "查询队列进度（段数、当前阶段、每段最近结果）与队列文件路径。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "lumen_queue_cancel",
            description: "请求取消正在渲染的队列；已完成的段保留结果，未开始的段保持待渲。",
            input_schema: object(json!({}), &[]),
        },
    ]
}
