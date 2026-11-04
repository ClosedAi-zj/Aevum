//! 命令注册表与工具实现。
//!
//! 工具名统一用 `aevum_` 前缀平铺：MCP 客户端不会替我们加命名空间，
//! 平铺后 agent 一眼能看出这是 Aevum Studio 的能力。
//!
//! 所有写操作都遵守两条底线：不覆盖源图、不删除源图；覆盖已有输出和
//! 批量写真要调用方显式给 `confirm: true`。
//!
//! The command registry and the tool implementations.
//!
//! Tool names are flat and all prefixed with `aevum_`: MCP clients do not add a
//! namespace for us, and a flat list lets an agent see at a glance that these are
//! Aevum Studio capabilities.
//!
//! Every write operation keeps two bottom lines: never overwrite a source image,
//! never delete one. Overwriting existing output and batch writes require the
//! caller to pass `confirm: true` explicitly.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Instant;

use aevum_core::align::{self, AlignIssue, AlignOptions, AlignReport, CropRect, Estimator};
use aevum_core::audio::{AudioCodec, AudioOptions};
use aevum_core::colorspace::ColorSpace;
use aevum_core::crop::Motion;
use aevum_core::cube;
use aevum_core::deflicker;
use aevum_core::develop::Ev;
use aevum_core::errmsg::clip_echo;
use aevum_core::exif;
use aevum_core::export::{
    self, Codec, JobProgress, OutputDynamic, OutputMode, OutputSharpen, RenderOptions,
    SharpenStrength, Stage, StillFormat,
};
use aevum_core::grade::{self, GradeParams, PRESET_NAMES};
use aevum_core::interp;
use aevum_core::metrics;
use aevum_core::preview;
use aevum_core::project;
use aevum_core::queue::{self, QueueItem, QueueStatus};
use aevum_core::retime::{self, Retime, SpeedKey};
use aevum_core::sequence::{Sequence, SequenceError, is_supported_extension};
use aevum_core::warp::{self, Framing};
use aevum_core::watermark::Watermark;
use aevum_core::xmp;
use serde_json::{Value, json};

use crate::Command;
use crate::session::{
    JobKind, JobOutcome, JobShared, MAX_LONG_SIDE, MAX_SERIES, MEASURE_LONG_SIDE, STAGE_ENCODE,
    STAGE_FRAMES, Session, measure_paths, normalize,
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
    // 再规范化一次（`lib::dispatch` 已经做过，这里是幂等的）。目的是保证
    // **`is_write_op` 看到的一定是规范名**——日志丢了没人会发现，多转一次很便宜。
    let id = aevum_core::brand::canonical_tool(id);
    let id = id.as_ref();
    let result = dispatch_inner(session, id, args);
    // 成功的写操作追加进序列目录的 op_journal.jsonl；日志失败不影响工具结果。
    if result.is_ok()
        && aevum_core::journal::is_write_op(id)
        && let Some(root) = session.seq.as_ref().map(|s| s.root.clone())
    {
        let _ = aevum_core::journal::append(&root, "mcp", id, args);
    }
    result
}

fn dispatch_inner(session: &mut Session, id: &str, args: &Value) -> Result<Value, String> {
    // 先把已经结束的后台任务收进会话，后面读进度/结果都是最新状态。
    session.poll_job();
    match id {
        "aevum_open_sequence" => open_sequence(session, args),
        "aevum_sequence_info" => sequence_info(session),
        "aevum_measure" => measure(session, args),
        "aevum_deflicker_analyze" => deflicker_analyze(session, args),
        "aevum_grade_get" => grade_get(session),
        "aevum_grade_set" => grade_set(session, args),
        "aevum_grade_preset" => grade_preset(session, args),
        "aevum_grade_reset" => grade_reset(session),
        "aevum_grade_copy" => grade_copy(session, args),
        "aevum_keyframe_add" => keyframe_add(session, args),
        "aevum_keyframe_remove" => keyframe_remove(session, args),
        "aevum_keyframe_list" => keyframe_list(session, args),
        "aevum_render_preview" => render_preview(session, args),
        "aevum_export_stills" => export_stills(session, args),
        "aevum_export_video" => export_video(session, args),
        "aevum_hdr_merge" => hdr_merge(session, args),
        "aevum_focus_merge" => focus_merge(session, args),
        "aevum_pano_merge" => pano_merge(session, args),
        "aevum_stack_merge" => stack_merge(session, args),
        "aevum_timeblend_merge" => timeblend_merge(session, args),
        "aevum_cull_evaluate" => cull_evaluate(session, args),
        "aevum_cull_extract" => cull_extract(session, args),
        "aevum_project_get" => project_get(session),
        "aevum_project_save" => project_save(session),
        "aevum_xmp_read" => xmp_read(args),
        "aevum_xmp_write" => xmp_write(session, args),
        "aevum_cube_import" => cube_import(session, args),
        "aevum_cube_export" => cube_export(session, args),
        "aevum_align" => align_run(session, args),
        "aevum_align_status" => align_status(session),
        "aevum_cancel" => cancel(session),
        "aevum_queue_list" => queue_list(session),
        "aevum_queue_add" => queue_add(session, args),
        "aevum_queue_remove" => queue_remove(session, args),
        "aevum_queue_clear" => queue_clear(session),
        "aevum_queue_run" => queue_run(session, args),
        "aevum_queue_status" => queue_status(session),
        "aevum_queue_cancel" => queue_cancel(session),
        "aevum_sequences_scan" => sequences_scan(session, args),
        "aevum_batch_run" => batch_run(session, args),
        "aevum_journal_list" => journal_list(session, args),
        other => Err(format!("未知工具：{}", clip_echo(other))),
    }
}

// ------------------------------------------------------------------ 序列

fn open_sequence(session: &mut Session, args: &Value) -> Result<Value, String> {
    // 换序列会清空会话；后台任务还握着旧帧路径，必须等它结束。
    if session.is_running() {
        return Err("有后台任务正在运行，先调用 aevum_cancel 并等它停下再换序列".to_owned());
    }
    let root = absolute(Path::new(arg_str(args, "root")?))?;
    // 路径是用户给的，可能长到几十 KB：错误串里原样回显等于把输入搬一遍，
    // agent 那边一次工具调用能收到两万字。截到 256 字符，看得见开头就够定位。
    //
    // The path comes from the caller and can be tens of KB long: echoing it verbatim
    // means shipping the input back, and an agent's tool result ends up 20 KB wide.
    // Clip at 256 characters -- the head is enough to locate the problem.
    if !root.is_dir() {
        return Err(format!("不是文件夹：{}", echo_path(&root)));
    }
    // 内层这条同样来自 IO：`read_dir` 的 `io::Error` 在 Windows 上会带上路径，
    // 一并截断，别让同一类回显从后门漏出去。
    //
    // The inner one is IO-borne too: `read_dir`'s `io::Error` carries the path on
    // Windows, so clip it as well rather than letting the same echo out the back door.
    let seq = Sequence::open(&root)
        .map_err(|e| format!("打开序列失败：{}", clip_echo(&e.to_string())))?;
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
                session.def_align_exif = p.def_align_exif;
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
            "align_exif": session.def_align_exif,
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
    // 交付预设清单：agent 要在 `aevum_export_video` 里传 `delivery`，就得先知道有哪些
    // key。放在这里而不是新开一个工具——工具目录一动，附着模式的路由表、GUI 命令、
    // 日志的写操作分类全都要跟着改，而这份清单本来就是「会话状态」的一部分。
    //
    // The delivery preset list lives here rather than in a new tool: adding a tool means
    // touching the attach-mode routing table, the GUI command catalog and the journal's
    // write-op classification, while this list is genuinely part of the session state.
    let delivery: Vec<Value> = aevum_core::delivery::DeliveryTable::load()
        .presets
        .iter()
        .map(|p| {
            json!({
                "key": p.key,
                "name_cn": p.name_cn,
                "name_en": p.name_en,
                "width": p.width,
                "height": p.height,
                "fps": p.fps,
                "codec": p.codec.key(),
                "crf": p.crf,
                "hint_cn": p.hint_cn,
                "hint_en": p.hint_en,
            })
        })
        .collect();
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
            "align_exif": session.def_align_exif,
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
        },
        "delivery": delivery
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
        return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
    }
    // 已经测过且没要求刷新时直接复用，避免每次查摘要都重读整段序列。
    if !refresh && session.metrics.is_some() {
        return measure_summary(session);
    }
    let (n, paths) = {
        let seq = session.require_seq()?;
        (seq.len(), seq.paths())
    };
    // 盘上测光缓存挂在序列根上，和无头/ GUI 共用同一个入口时要用到它。
    let root = session.require_root()?;
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    let shared = Arc::clone(&session.shared);
    shared.begin(JobKind::Measure, n);
    if background {
        let worker = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("aevum-measure".to_owned())
            .spawn(move || {
                // refresh 走强制重算，其余走盘上账（与 GUI 同一本）。
                let result = measure_paths(&root, &paths, &worker, !refresh);
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
            "note": "用 aevum_align_status 查询进度，aevum_cancel 取消。"
        }));
    }
    let result = measure_paths(&root, &paths, &shared, !refresh);
    session.complete_job(result)?;
    measure_summary(session)
}

/// 测光结果的摘要；`series` 按步长降采样，完整数值只留在会话里。
fn measure_summary(session: &Session) -> Result<Value, String> {
    let metrics = session
        .metrics
        .as_ref()
        .ok_or_else(|| "还没有测光，先调用 aevum_measure".to_owned())?;
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
    let align_exif = opt_bool(args, "align_exif")?.unwrap_or(session.def_align_exif);
    if !window.is_finite() || !strength.is_finite() {
        return Err("window / strength 必须是有限数值".to_owned());
    }
    // EV 表只在开关打开时才去读，且**只读一次**：一条千帧序列多读一遍盘不是白给的，
    // 关着就该零成本。长度钉在输入行数上——分析是逐行对齐的，表短了校正会比帧数少。
    let evs_for = |n: usize| -> Option<Vec<Option<f64>>> {
        if !align_exif {
            return None;
        }
        let paths = session
            .seq
            .as_ref()
            .map(Sequence::paths)
            .unwrap_or_default();
        let exifs = exif::read_many(&paths);
        Some((0..n).map(|i| exifs.get(i).and_then(|e| e.ev())).collect())
    };
    // 口径同 GUI：有线性光测光就在线性域算，拿不到（比如只测过灰度）才退回 gamma 均值。
    let measured = session.def_means.iter().any(|m| m.is_some());
    let (analysis, evs) = if channels && measured {
        let rgb: Vec<Option<[f64; 3]>> =
            session.def_means.iter().map(|m| m.map(|m| m.rgb)).collect();
        let evs = evs_for(rgb.len());
        let a = match evs.as_deref() {
            Some(e) => deflicker::analyze_rgb_exif(&rgb, e, window, strength, multi),
            None => deflicker::analyze_rgb(&rgb, window, strength, multi),
        };
        (a, evs)
    } else if measured {
        let luma: Vec<Option<f64>> = session
            .def_means
            .iter()
            .map(|m| m.map(|m| m.luma))
            .collect();
        let evs = evs_for(luma.len());
        let a = match evs.as_deref() {
            Some(e) => deflicker::analyze_luma_exif(&luma, e, window, strength, multi),
            None => deflicker::analyze_luma(&luma, window, strength, multi),
        };
        (a, evs)
    } else {
        let means: Vec<Option<f64>> = session
            .metrics
            .as_ref()
            .expect("ensure_metrics 已填充")
            .iter()
            .map(|m| m.map(|m| m.mean * 255.0))
            .collect();
        let evs = evs_for(means.len());
        let a = match evs.as_deref() {
            Some(e) => deflicker::analyze_luma_exif(&means, e, window, strength, multi),
            None => deflicker::analyze_luma(&means, window, strength, multi),
        };
        (a, evs)
    };
    let ev_frames = evs.iter().flatten().count();
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
    session.def_align_exif = align_exif;
    session.analysis = Some(analysis);

    // 开关开着却一个 EV 都没读到，必须说出来：那是「看起来生效、实际空转」的假开关。
    let align_note = if align_exif && ev_frames == 0 {
        json!(
            "这些帧没有可用的曝光三要素（ISO/快门/光圈），预对齐没有生效；channels/window 等其余参数照常起作用。"
        )
    } else {
        Value::Null
    };
    Ok(json!({
        "window": window,
        "strength": strength,
        "multi": multi,
        "channels": channels,
        "align_exif": align_exif,
        "ev_frames": ev_frames,
        "align_note": align_note,
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
        "note": "ev 为正表示该帧提亮，导出与预览会自动叠加；开分通道时每项另带 rgb=[R,G,B] 三通道 EV。align_exif 为 true 时 ev 已含预对齐增益，圣杯序列上可能远超单帧 ±3 档的限幅——那是这次导出里真实的重新曝光。"
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
        return Err(format!("未知调色字段：{}", echo_text(&unknown.join(", "))));
    }
    serde_json::from_value(current).map_err(|e| format!("参数格式不对：{e}"))
}

fn grade_preset(session: &mut Session, args: &Value) -> Result<Value, String> {
    let name = arg_str(args, "name")?;
    if !PRESET_NAMES.contains(&name) {
        return Err(format!(
            "未知预设 {}；可用：{}",
            echo_text(name),
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
                    Some(_) if !overwrite => Err(format!(
                        "副本「{}」已存在；要覆盖给 overwrite: true",
                        echo_text(&name)
                    )),
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
                return Err(format!("没有叫 {} 的副本", echo_text(&name)));
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
                return Err(format!("没有叫 {} 的副本", echo_text(&name)));
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
                    return Err(format!("副本「{}」已存在", echo_text(&to)));
                }
                let Some(slot) = copies.iter_mut().find(|c| c.name == name) else {
                    return Err(format!("没有叫 {} 的副本", echo_text(&name)));
                };
                slot.name = to.clone();
                Ok(())
            })?;
            Ok(json!({"renamed": to, "copies": session.copies().len()}))
        }
        other => Err(format!(
            "未知 op {}；可用 list / save / apply / delete / rename",
            echo_text(other)
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
        return Err(format!("拒绝覆盖源图：{}", echo_path(&out)));
    }
    if out.exists() && !confirm(args) {
        return Err(format!("目标已存在，需 confirm: true：{}", echo_path(&out)));
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
        return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
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
    require_batch_confirm(args, "aevum_export_stills")?;
    aevum_core::paths::ensure_out_dir_safe(&root, &out_dir)?;

    let paths = session.require_seq()?.paths();
    // copy：这一次导出改用某套副本的调色；省略就是会话当前这套。
    let copy = opt_str(args, "copy")?.unwrap_or_default();
    let params = session.frame_params_of(n, copy)?;
    let gains = session.gains(n);
    // 有对齐结果就自动套用统一裁切 + 逐帧位移，与 GUI 导出路径一致。
    let (crop, offsets) = session.alignment(n);
    let dynamic = opt_dynamic(args)?;
    let options = RenderOptions {
        mode: OutputMode::Stills,
        format,
        dynamic,
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
        return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    require_batch_confirm(args, "aevum_hdr_merge")?;
    let indices: Vec<usize> = match args.get("frames") {
        Some(list) => list
            .as_array()
            .ok_or_else(|| "frames 要是帧下标数组，例如 [0,1,2]".to_owned())?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|i| ((i as usize) < n).then_some(i as usize))
                    .ok_or_else(|| {
                        format!("帧下标越界：{}（序列共 {n} 帧）", clip_echo(&v.to_string()))
                    })
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
    if indices.len() > aevum_core::hdr::MAX_FRAMES {
        return Err(format!(
            "HDR 合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑关键几帧",
            aevum_core::hdr::MAX_FRAMES,
            indices.len()
        ));
    }
    let root = session.require_root()?;
    let out_dir = match opt_str(args, "out_dir")? {
        Some(dir) => absolute(Path::new(dir))?,
        None => root.join("HDR"),
    };
    aevum_core::paths::ensure_out_dir_safe(&root, &out_dir)?;
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("建输出目录失败：{}", clip_echo(&e.to_string())))?;

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
    let mut fused = aevum_core::hdr::merge(&refs, w, h);

    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    let (sharpen_for, sharpen_strength) = opt_sharpen(args)?;
    let dynamic = opt_dynamic(args)?;
    let options = RenderOptions {
        quality: opt_quality(args)?,
        keep_exif: opt_bool(args, "keep_exif")?.unwrap_or(false),
        xmp_sidecar: opt_bool(args, "xmp_sidecar")?.unwrap_or(false),
        sharpen_for,
        sharpen_strength,
        color_space: color_space_for(dynamic, opt_color_space(args)?),
        dynamic,
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

/// 焦点合成：把一组不同对焦距离的帧融成一张全景深静帧。
///
/// 与 GUI 的 `focus.merge` 同口径——按帧下标读原图、套对齐的统一裁切与逐帧位移、
/// 融合后走静帧导出那条输出尾（锐化 / 水印 / 色域 / EXIF）；融合本身不改调色。
/// 无头会话没有「当前帧」，省 `start` 就取序列中位帧起的一段（与 `aevum_hdr_merge`
/// 省 `center` 时取中间帧同理）。
///
/// Focus stacking: fuse a bracket taken at different focus distances into one all-in-focus
/// still.
///
/// Same convention as the GUI's `focus.merge`: read the originals by frame index, apply the
/// alignment crop and the per-frame shifts, then go through the still-export tail (sharpen,
/// watermark, colour space, EXIF); the fusion itself does not grade. A headless session has
/// no "current frame", so an omitted `start` takes a run beginning at the middle frame (the
/// same idea as `aevum_hdr_merge` falling back to the middle frame for `center`).
fn focus_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    require_batch_confirm(args, "aevum_focus_merge")?;
    let indices: Vec<usize> = match args.get("frames") {
        Some(list) => list
            .as_array()
            .ok_or_else(|| "frames 要是帧下标数组，例如 [0,1,2]".to_owned())?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|i| ((i as usize) < n).then_some(i as usize))
                    .ok_or_else(|| {
                        format!("帧下标越界：{}（序列共 {n} 帧）", clip_echo(&v.to_string()))
                    })
            })
            .collect::<Result<_, _>>()?,
        None => {
            let around = opt_u32(args, "around")?.unwrap_or(7) as usize;
            // 无头会话没有「当前帧」，省 start 就取序列中位帧起的一段。
            let start = opt_u32(args, "start")?.map(|v| v as usize).unwrap_or(n / 2);
            if start >= n {
                return Err(format!("start 越界：{start}（序列共 {n} 帧）"));
            }
            (start..=start.saturating_add(around).min(n - 1)).collect()
        }
    };
    if indices.len() < 2 {
        return Err("焦点合成至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
    }
    if indices.len() > aevum_core::focus::MAX_FRAMES {
        return Err(format!(
            "焦点合成一次最多 {} 帧，实得 {} 帧：把 around 调小或分段合成",
            aevum_core::focus::MAX_FRAMES,
            indices.len()
        ));
    }
    let opts = focus_options(args)?;
    // 合成前自动对齐：解析口径与 UI 的 `focus.merge` 一致——缺省 false，非布尔报错。
    //
    // Pre-fusion auto alignment: the same convention as the UI's `focus.merge` -- default
    // false, a non-boolean value is an error.
    let align = opt_bool(args, "align")?.unwrap_or(false);
    let root = session.require_root()?;
    let out_dir = match opt_str(args, "out_dir")? {
        Some(dir) => absolute(Path::new(dir))?,
        None => root.join("焦点合成"),
    };
    aevum_core::paths::ensure_out_dir_safe(&root, &out_dir)?;
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("建输出目录失败：{}", clip_echo(&e.to_string())))?;

    let background = opt_bool(args, "background")?.unwrap_or(false);
    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    let (sharpen_for, sharpen_strength) = opt_sharpen(args)?;
    let dynamic = opt_dynamic(args)?;
    // 有对齐结果就套统一裁切 + 逐帧位移，与静帧导出同一条几何路。
    let (crop, offsets) = session.alignment(n);
    let options = RenderOptions {
        quality: opt_quality(args)?,
        keep_exif: opt_bool(args, "keep_exif")?.unwrap_or(false),
        xmp_sidecar: opt_bool(args, "xmp_sidecar")?.unwrap_or(false),
        sharpen_for,
        sharpen_strength,
        color_space: color_space_for(dynamic, opt_color_space(args)?),
        dynamic,
        watermark: opt_watermark(args)?,
        out_dir: out_dir.clone(),
        metadata: session.metadata(),
        ..RenderOptions::still(format)
    };
    // 执行计划在校验完成后一次性从会话取齐：后台线程只拿计划跑，不再碰会话。
    let plan = FocusMergePlan {
        indices,
        paths: session.require_seq()?.paths(),
        crop,
        offsets,
        opts,
        align,
        out_dir,
        options,
    };
    if background {
        return start_focus_merge_background(session, plan);
    }
    // 同步路径也走同一个任务槽：`begin` 顺手清掉上一轮遗留的取消旗标，
    // 否则一次取消会让下一次同步合并刚起步就被自己的旧旗标打死。
    let shared = Arc::clone(&session.shared);
    shared.begin(JobKind::Export, plan.indices.len());
    let result = run_focus_merge(plan, &shared);
    match &result {
        Ok(_) => shared.finish_ok(),
        Err(e) => shared.finish(Err(e.clone())),
    }
    result
}

/// `aevum_focus_merge` 的执行计划：校验阶段一次取齐，跑的人不再读会话。
///
/// The execution plan for `aevum_focus_merge`: gathered once during validation, so
/// the runner never touches the session again.
struct FocusMergePlan {
    indices: Vec<usize>,
    paths: Vec<PathBuf>,
    crop: Option<CropRect>,
    offsets: Vec<[f64; 2]>,
    opts: aevum_core::focus::Options,
    align: bool,
    out_dir: PathBuf,
    options: RenderOptions,
}

/// 后台跑 `aevum_focus_merge`：起线程、登记任务、立即返回。
///
/// Run `aevum_focus_merge` in the background: spawn the thread, register the job
/// and return immediately.
fn start_focus_merge_background(
    session: &mut Session,
    plan: FocusMergePlan,
) -> Result<Value, String> {
    let total = plan.indices.len();
    let shared = Arc::clone(&session.shared);
    shared.begin(JobKind::Export, total);
    let worker = Arc::clone(&shared);
    let handle = thread::Builder::new()
        .name("aevum-focus".to_owned())
        .spawn(move || {
            let result = run_focus_merge(plan, &worker);
            worker.finish(result.map(|summary| {
                let path = summary
                    .get("path")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_default();
                JobOutcome::Export { path, summary }
            }));
        })
        .map_err(|e| {
            let message = format!("启动后台焦点合成失败：{e}");
            shared.finish(Err(message.clone()));
            message
        })?;
    session.start_background(shared, handle);
    Ok(json!({
        "started": true,
        "background": true,
        "frames": total,
        "progress": {"done": 0, "total": total},
        "note": "用 aevum_align_status 查询进度，aevum_cancel 取消。"
    }))
}

/// 真正的合并执行体：解码 → 自动对齐 → 逐块 warp → 融合 → 写成片。
///
/// `shared` 既是进度出口也是取消旗标的来源；`aevum_cancel` 置位的旗标与这里
/// 读到的是同一块内存，长任务期间 status / cancel 都能应答（任务在后台线程，
/// stdio 主循环空闲）。
///
/// The actual merge runner: decode → auto-align → per-block warp → fuse → write.
///
/// `shared` is both the progress outlet and the source of the cancel flag: the flag
/// `aevum_cancel` sets is the same memory read here, and because the job runs on a
/// background thread the stdio main loop stays free to answer status / cancel.
fn run_focus_merge(plan: FocusMergePlan, shared: &JobShared) -> Result<Value, String> {
    let cancelled = || shared.cancel_requested();
    let total = plan.indices.len();
    // 有对齐结果就套统一裁切 + 逐帧位移，与静帧导出同一条几何路。
    let rect = plan
        .crop
        .map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize));
    let mut buffers: Vec<Vec<f32>> = Vec::with_capacity(total);
    let mut dims: Option<(usize, usize)> = None;
    shared.set_stage(STAGE_FRAMES, 0, total);
    for (done, i) in plan.indices.iter().enumerate() {
        // 帧与帧之间查取消：后台任务里这是真旗标，`aevum_cancel` 一到就停。
        if cancelled() {
            return Err("焦点合成已取消".to_owned());
        }
        let (rgb, w, h) =
            export::load_rgb_f32(&plan.paths[*i]).map_err(|e| format!("读帧失败：{e}"))?;
        let framing = Framing {
            angle_deg: 0.0,
            off: plan.offsets.get(*i).copied().unwrap_or([0.0, 0.0]),
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
                    plan.indices[0], prev.0, prev.1, got.0, got.1
                ));
            }
            Some(_) => {}
        }
        buffers.push(buf);
        shared.set_stage(STAGE_FRAMES, done + 1, total);
    }
    let (mut w, mut h) = dims.unwrap_or((0, 0));
    // 自动对齐：默认不动任何像素（`align: false` 这条路的成片与从前逐点一致）。
    //
    // Auto alignment: by default not a single pixel is touched (`align: false` stays
    // point-for-point identical to before).
    let mut align_status = "off";
    let mut align_gain = 0.0f32;
    if plan.align {
        let refs: Vec<&[f32]> = buffers.iter().map(|b| b.as_slice()).collect();
        // 手动序列对齐的位移不叠（拍板 ⑥）：只保留它的裁切框，位移完全由本次估计给出。
        //
        // Manual sequence-alignment offsets are never stacked on top (ruling 6): only its
        // crop rectangle survives, and the offsets come entirely from this estimate.
        let report =
            aevum_core::focus_align::estimate(&refs, w, h, plan.opts.radius, plan.crop, &cancelled)
                .ok_or_else(|| "自动对齐被要求中止".to_owned())?;
        align_status = report.status.key();
        align_gain = report.gain;
        if report.status == aevum_core::focus_align::AlignStatus::Adopted {
            // 逐块 warp + 24px 接缝淡化；输出窗口按拍板 ⑥：有手动裁切框就整幅，否则
            // 用估计位移算出的公共窗口。
            //
            // Per-block warp with 24 px seam feathering; the output window follows ruling 6:
            // the whole frame when a manual crop exists, otherwise the common window derived
            // from the estimated offsets.
            let window = match plan.crop {
                Some(_) => None,
                None => report
                    .crop
                    .map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize)),
            };
            let mut out_dims = (0usize, 0usize);
            for (i, buf) in buffers.iter_mut().enumerate() {
                if cancelled() {
                    return Err("焦点合成已取消".to_owned());
                }
                let field = report.blocks.get(i).map(Vec::as_slice).unwrap_or(&[]);
                // w/h 必须全程是**原帧**尺寸：窗口（auto_crop 的裁切）比整幅小，
                // 若在循环里把 w/h 改成第 0 帧的输出尺寸，第 1 帧起就拿裁切后的
                // 尺寸去解释整幅缓冲，几何整个错掉、尺寸对不上。输出尺寸在循环外
                // 统一取（UI 侧同一条编排的写法，见 state.rs 的 merge job）。
                //
                // w/h must stay the **source-frame** size throughout: the window (auto_crop's
                // rectangle) is smaller than the frame, and rewriting w/h to frame 0's output
                // size inside the loop would make every later frame interpret a full-size
                // buffer at cropped dimensions -- geometry scrambled, sizes inconsistent.
                // Output size is taken once, after the loop (the same orchestration the UI
                // side runs; see the merge job in state.rs).
                let (out, nw, nh) =
                    aevum_core::focus_align::warp_by_block_field(buf, w, h, field, window);
                if nw == 0 || nh == 0 {
                    return Err("自动对齐的裁切窗口是空的：检查对齐位移是否异常".to_owned());
                }
                if i == 0 {
                    out_dims = (nw, nh);
                }
                // 原地逐帧替换：新帧一落位旧帧立即 drop，两套全幅不再同时驻留
                // （24MP×8 凭空省下 ~2.3 GB 峰值）。
                //
                // In-place per-frame replacement: the source frame drops the moment the
                // warped one lands, so two full-frame sets never coexist (~2.3 GB of peak
                // saved for 24MP×8).
                *buf = out;
            }
            // 从这里起 w/h 是成片尺寸（所有帧的窗口一致，尺寸必然相同）。
            w = out_dims.0;
            h = out_dims.1;
        }
    }
    if cancelled() {
        return Err("焦点合成已取消".to_owned());
    }
    shared.set_stage(STAGE_ENCODE, total, total);
    let refs: Vec<&[f32]> = buffers.iter().map(|b| b.as_slice()).collect();
    // 报错走焦点语境的文案：`StackError::cn()` 说的是「堆栈」，对焦包围的用户看着别扭。
    let mut fused = aevum_core::focus::focus_stack(&refs, w, h, &plan.opts)
        .map_err(|e| format!("焦点合成失败：{}", aevum_core::focus::error_text(e)))?;

    let format = plan.options.format;
    let dst = plan.out_dir.join(format!(
        "focus_{:06}_{}f.{}",
        plan.indices[0],
        total,
        format.ext()
    ));
    let deep = format == StillFormat::Tiff16;
    export::write_single_still(
        &plan.paths[plan.indices[0]],
        &dst,
        &mut fused,
        w,
        h,
        deep,
        &plan.options,
    )
    .map_err(|e| format!("写焦点合成成片失败：{}", clip_echo(&e.to_string())))?;
    let count = plan.indices.len();
    Ok(json!({
        "path": dst.display().to_string(),
        "kind": "focus",
        "frames": count,
        "indices": plan.indices,
        "width": w,
        "height": h,
        // 机器键，**不发中文 reason**：MCP 客户端没有 i18n 漏斗。
        //
        // Machine keys only; the Chinese reason is **not** emitted -- MCP clients have no
        // i18n funnel.
        "align_status": align_status,
        "align_gain": align_gain,
    }))
}

/// 焦点融合参数：`method` 解析，其余数值按 `control.rs` 的 schema 范围夹住。
///
/// 为什么夹而不报错：这几个数都是「越大越慢 / 越糊」的旋钮，超出范围不会让结果算错，
/// 只会让 agent 传进一份没人想要的参数；schema 已经声明了范围，这里按同一口径夹住，
/// 比让一条无头批处理任务因为 `smooth_z: 900` 多跑十分钟要友好。
///
/// Focus-fusion parameters: `method` is parsed, the numeric knobs are clamped to the ranges
/// `control.rs`'s schema declares.
///
/// Why clamp instead of erroring: each of these is a "slower/blurrier as it grows" knob that
/// never makes the result *wrong*, it only expresses parameters nobody wants; the schema
/// already declares the range, so clamping to the same bounds beats letting a headless batch
/// job run ten minutes longer because of `smooth_z: 900`.
fn focus_options(args: &Value) -> Result<aevum_core::focus::Options, String> {
    let method = match opt_str(args, "method")? {
        None => aevum_core::focus::Method::DepthMap,
        Some(raw) => aevum_core::focus::Method::parse(raw).ok_or_else(|| {
            format!(
                "未知融合方式 {}；可用 depthmap / weighted / pyramid",
                clip_echo(raw)
            )
        })?,
    };
    Ok(aevum_core::focus::Options {
        method,
        radius: (opt_u32(args, "radius")?.unwrap_or(2) as usize).min(32),
        threshold: (opt_f64(args, "threshold")?.unwrap_or(0.0) as f32).clamp(0.0, 100.0),
        smooth_z: (opt_u32(args, "smooth_z")?.unwrap_or(1) as usize).min(16),
        feather: (opt_f64(args, "feather")?.unwrap_or(2.0) as f32).clamp(0.0, 64.0),
        // 命令面不暴露 levels，0 = auto（见 focus::Options）。
        // The command surface does not expose levels; 0 = auto (see focus::Options).
        levels: 0,
        // 命令面同样不暴露测度缓存上界，用默认 8（见 focus::Options::measure_cache）。
        // The command surface does not expose the measure-cache bound either; default 8
        // (see focus::Options::measure_cache).
        measure_cache: 8,
    })
}

fn pano_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    require_batch_confirm(args, "aevum_pano_merge")?;
    let indices: Vec<usize> = match args.get("frames") {
        Some(list) => list
            .as_array()
            .ok_or_else(|| "frames 要是帧下标数组，例如 [0,1,2]".to_owned())?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|i| ((i as usize) < n).then_some(i as usize))
                    .ok_or_else(|| {
                        format!("帧下标越界：{}（序列共 {n} 帧）", clip_echo(&v.to_string()))
                    })
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
    if indices.len() > aevum_core::pano::MAX_FRAMES {
        return Err(format!(
            "全景合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑相邻几张",
            aevum_core::pano::MAX_FRAMES,
            indices.len()
        ));
    }
    let root = session.require_root()?;
    let out_dir = match opt_str(args, "out_dir")? {
        Some(dir) => absolute(Path::new(dir))?,
        None => root.join("全景"),
    };
    aevum_core::paths::ensure_out_dir_safe(&root, &out_dir)?;
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("建输出目录失败：{}", clip_echo(&e.to_string())))?;

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
        aevum_core::pano::stitch(&refs).map_err(|e| format!("拼接失败：{e}"))?;

    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    let (sharpen_for, sharpen_strength) = opt_sharpen(args)?;
    let dynamic = opt_dynamic(args)?;
    let options = RenderOptions {
        quality: opt_quality(args)?,
        keep_exif: opt_bool(args, "keep_exif")?.unwrap_or(false),
        xmp_sidecar: opt_bool(args, "xmp_sidecar")?.unwrap_or(false),
        sharpen_for,
        sharpen_strength,
        color_space: color_space_for(dynamic, opt_color_space(args)?),
        dynamic,
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

// ------------------------------------------------------------------ 堆栈 / 时间合成 / 筛选（无头补全）

/// 合并类工具的帧选择：`frames` 数组优先，否则 `center`/`start` + `around`。
///
/// 无头会话没有「当前帧」，锚点省略时取序列中位帧（与 `aevum_hdr_merge` /
/// `aevum_focus_merge` 的先例一致）；`center_anchor` 选中心锚（堆栈）还是
/// 起始锚（时间合成），与 GUI 两条命令的口径逐字对齐。
///
/// Frame selection for the merge-family tools: a `frames` array wins, otherwise
/// `center`/`start` plus `around`.
///
/// A headless session has no "current frame", so an omitted anchor falls back to the
/// middle frame (same precedent as `aevum_hdr_merge` / `aevum_focus_merge`);
/// `center_anchor` picks the centre anchor (stacking) or the start anchor (time
/// blending), word for word aligned with the GUI commands' conventions.
fn merge_indices(
    session: &Session,
    args: &Value,
    default_around: u32,
    center_anchor: bool,
) -> Result<Vec<usize>, String> {
    let n = session.require_seq()?.len();
    match args.get("frames") {
        Some(list) => list
            .as_array()
            .ok_or_else(|| "frames 要是帧下标数组，例如 [0,1,2]".to_owned())?
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|i| ((i as usize) < n).then_some(i as usize))
                    .ok_or_else(|| {
                        format!("帧下标越界：{}（序列共 {n} 帧）", clip_echo(&v.to_string()))
                    })
            })
            .collect(),
        None => {
            let around = opt_u32(args, "around")?.unwrap_or(default_around) as usize;
            let (key, at) = if center_anchor {
                let c = opt_u32(args, "center")?
                    .map(|v| v as usize)
                    .unwrap_or(n / 2);
                ("center", c)
            } else {
                let s = opt_u32(args, "start")?.map(|v| v as usize).unwrap_or(n / 2);
                ("start", s)
            };
            if at >= n {
                return Err(format!("{key} 越界：{at}（序列共 {n} 帧）"));
            }
            let from = if center_anchor {
                at.saturating_sub(around)
            } else {
                at
            };
            Ok((from..=at.saturating_add(around).min(n - 1)).collect())
        }
    }
}

/// 把选中帧按对齐几何读成线性缓冲：统一裁切 + 逐帧位移，尺寸不齐当场拒绝。
///
/// 与 `hdr_merge` / `focus_merge` 里内联的那段读帧循环同一条路；抽出来是因为
/// 堆栈与时间合成再抄两遍就开始失守（改一处漏三处），已有的两条不动，新的两条
/// 从这里走。
///
/// Reads the selected frames into linear buffers through the alignment geometry:
/// the shared crop plus per-frame offsets, refusing mismatched sizes on the spot.
///
/// Same path as the inlined read loops inside `hdr_merge` / `focus_merge`; it is
/// extracted because copying it twice more (for stacking and time blending) starts
/// to rot -- the two existing ones stay untouched, the two new ones go through here.
fn load_aligned_frames(
    session: &Session,
    indices: &[usize],
) -> Result<(Vec<Vec<f32>>, usize, usize), String> {
    let paths = session.require_seq()?.paths();
    let n = session.require_seq()?.len();
    let (crop, offsets) = session.alignment(n);
    let rect = crop.map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize));
    let mut buffers: Vec<Vec<f32>> = Vec::with_capacity(indices.len());
    let mut dims: Option<(usize, usize)> = None;
    for i in indices {
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
    Ok((buffers, w, h))
}

/// 静帧成片尾的输出选项：与 `hdr_merge` / `focus_merge` 的构造逐字段一致。
///
/// The still-export tail's render options: field-for-field identical to what
/// `hdr_merge` / `focus_merge` build.
fn still_render_options(
    session: &Session,
    args: &Value,
    format: StillFormat,
    out_dir: &Path,
) -> Result<RenderOptions, String> {
    let (sharpen_for, sharpen_strength) = opt_sharpen(args)?;
    let dynamic = opt_dynamic(args)?;
    Ok(RenderOptions {
        quality: opt_quality(args)?,
        keep_exif: opt_bool(args, "keep_exif")?.unwrap_or(false),
        xmp_sidecar: opt_bool(args, "xmp_sidecar")?.unwrap_or(false),
        sharpen_for,
        sharpen_strength,
        color_space: color_space_for(dynamic, opt_color_space(args)?),
        dynamic,
        watermark: opt_watermark(args)?,
        out_dir: out_dir.to_path_buf(),
        metadata: session.metadata(),
        ..RenderOptions::still(format)
    })
}

/// 堆栈：把同机位一段帧按统计量合成一张（星轨 / 降噪 / 去卫星 / σ 截尾）。
///
/// 与 GUI 的 `stack.merge` 同口径——按帧下标读原图、套对齐的统一裁切与逐帧位移；
/// 无头会话没有「当前帧」，省 `center` 就取序列中间那段（`aevum_hdr_merge` 同理）。
///
/// Star-field stacking: combine a fixed-camera run into one still with a statistical
/// mode (star trails / noise reduction / satellite removal / sigma clipping).
///
/// Same convention as the GUI's `stack.merge`: read the originals by frame index and
/// apply the alignment crop and per-frame shifts. A headless session has no "current
/// frame", so an omitted `center` takes the run around the middle frame (as in
/// `aevum_hdr_merge`).
fn stack_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    require_batch_confirm(args, "aevum_stack_merge")?;
    let indices = merge_indices(session, args, 4, true)?;
    if indices.len() < 2 {
        return Err("堆栈至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
    }
    if indices.len() > aevum_core::stack::MAX_FRAMES {
        return Err(format!(
            "堆栈一次最多 {} 帧，实得 {} 帧：把 around 调小，或用 frames 只挑关键几帧",
            aevum_core::stack::MAX_FRAMES,
            indices.len()
        ));
    }
    let mode = match opt_str(args, "mode")? {
        None => aevum_core::stack::Mode::Lighten,
        Some(raw) => aevum_core::stack::Mode::parse(raw).ok_or_else(|| {
            format!(
                "未知堆栈模式 {}；可用 lighten / average / median / sigma-clip",
                echo_text(raw)
            )
        })?,
    };
    let clip_sigma = opt_f64(args, "clip_sigma")?.unwrap_or(3.0) as f32;
    if !clip_sigma.is_finite() || clip_sigma <= 0.0 {
        return Err("clip_sigma 要大于 0（常用 2–4：3.0 只剔真离群）".to_owned());
    }
    let fill_gaps = opt_bool(args, "fill_gaps")?.unwrap_or(true);
    let opts = aevum_core::stack::Options {
        mode,
        clip_sigma,
        fill_gaps,
    };
    let root = session.require_root()?;
    let out_dir = match opt_str(args, "out_dir")? {
        Some(dir) => absolute(Path::new(dir))?,
        None => root.join("堆栈"),
    };
    aevum_core::paths::ensure_out_dir_safe(&root, &out_dir)?;
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("建输出目录失败：{}", clip_echo(&e.to_string())))?;
    let (buffers, w, h) = load_aligned_frames(session, &indices)?;
    let refs: Vec<&[f32]> = buffers.iter().map(|b| b.as_slice()).collect();
    let mut out = aevum_core::stack::combine(&refs, w, h, &opts)
        .map_err(|e| format!("堆栈失败：{}", e.cn()))?;
    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    let options = still_render_options(session, args, format, &out_dir)?;
    let dst = out_dir.join(format!(
        "stack_{:06}_{}f.{}",
        indices[0],
        indices.len(),
        format.ext()
    ));
    let deep = format == StillFormat::Tiff16;
    export::write_single_still(
        &session.require_seq()?.paths()[indices[0]],
        &dst,
        &mut out,
        w,
        h,
        deep,
        &options,
    )
    .map_err(|e| format!("写堆栈成片失败：{}", clip_echo(&e.to_string())))?;
    Ok(json!({
        "path": dst.display().to_string(),
        "kind": "stack",
        "frames": indices.len(),
        "indices": indices,
        "width": w,
        "height": h,
    }))
}

/// 时间合成：按带或按地平线把不同时刻的帧合成一张。
///
/// 与 GUI 的 `timeblend.merge` 同口径；帧号升序即时间顺序（frames[0] 是最早一帧）。
/// 无头会话没有「当前帧」，省 `start` 就从序列中位帧起取一段（`aevum_focus_merge`
/// 同理）；锚点不参与合成几何，只决定取哪一段。
///
/// Time blending: composite frames from different moments as horizontal bands or
/// split at the horizon.
///
/// Same convention as the GUI's `timeblend.merge`; ascending frame numbers are the
/// time order (frames[0] is the earliest). A headless session has no "current frame",
/// so an omitted `start` takes a run beginning at the middle frame (as in
/// `aevum_focus_merge`); the anchor shapes the selection only, never the geometry.
fn timeblend_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
    }
    let n = session.require_seq()?.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    require_batch_confirm(args, "aevum_timeblend_merge")?;
    let indices = merge_indices(session, args, 7, false)?;
    if indices.len() < 2 {
        return Err(
            "时间合成至少要 2 帧：给 frames 数组，或把 around 调大（帧号升序 = 时间顺序）"
                .to_owned(),
        );
    }
    if indices.len() > aevum_core::timeblend::MAX_FRAMES {
        return Err(format!(
            "时间合成一次最多 {} 帧，实得 {} 帧：把 around 调小",
            aevum_core::timeblend::MAX_FRAMES,
            indices.len()
        ));
    }
    let layout = match opt_str(args, "layout")? {
        None => aevum_core::timeblend::Layout::SkyGround,
        Some(raw) => aevum_core::timeblend::Layout::parse(raw)
            .ok_or_else(|| format!("未知布局 {}；可用 sky-ground / bands", echo_text(raw)))?,
    };
    let bands = opt_u32(args, "bands")?.unwrap_or(8) as usize;
    // bands 只对时间切片有意义：天地合成不用它，不该被默认值卡住。
    //
    // bands only matters for the time slice: sky-ground ignores it and must not be
    // blocked by the default.
    if layout == aevum_core::timeblend::Layout::Bands && (bands == 0 || bands > indices.len()) {
        return Err(format!(
            "bands 要在 1..={} 之间（等于帧数时一带一帧）",
            indices.len()
        ));
    }
    let feather = (opt_f64(args, "feather")?.unwrap_or(0.04) as f32).clamp(0.0, 0.5);
    let horizon_auto = opt_bool(args, "horizon_auto")?.unwrap_or(false);
    let horizon = (opt_f64(args, "horizon")?.unwrap_or(0.5) as f32).clamp(0.0, 1.0);
    let deghost = opt_f64(args, "deghost")?.unwrap_or(0.5) as f32;
    if !deghost.is_finite() || !(0.0..=4.0).contains(&deghost) {
        return Err("deghost 要在 0..=4 之间（EV；0 = 关闭去鬼影）".to_owned());
    }
    let match_exposure = opt_bool(args, "match_exposure")?.unwrap_or(true);
    let spec = aevum_core::timeblend::Spec {
        layout,
        bands,
        feather,
        horizon,
        horizon_auto,
        deghost,
        match_exposure,
    };
    let root = session.require_root()?;
    let out_dir = match opt_str(args, "out_dir")? {
        Some(dir) => absolute(Path::new(dir))?,
        None => root.join("时间合成"),
    };
    aevum_core::paths::ensure_out_dir_safe(&root, &out_dir)?;
    fs::create_dir_all(&out_dir)
        .map_err(|e| format!("建输出目录失败：{}", clip_echo(&e.to_string())))?;
    let (buffers, w, h) = load_aligned_frames(session, &indices)?;
    let refs: Vec<&[f32]> = buffers.iter().map(|b| b.as_slice()).collect();
    let mut out = aevum_core::timeblend::time_slice(&refs, w, h, &spec)
        .map_err(|e| format!("时间合成失败：{}", e.cn()))?;
    let format = parse_format(opt_str(args, "format")?.unwrap_or("png8"))?;
    let options = still_render_options(session, args, format, &out_dir)?;
    let dst = out_dir.join(format!(
        "timeblend_{:06}_{}f.{}",
        indices[0],
        indices.len(),
        format.ext()
    ));
    let deep = format == StillFormat::Tiff16;
    export::write_single_still(
        &session.require_seq()?.paths()[indices[0]],
        &dst,
        &mut out,
        w,
        h,
        deep,
        &options,
    )
    .map_err(|e| format!("写时间合成成片失败：{}", clip_echo(&e.to_string())))?;
    Ok(json!({
        "path": dst.display().to_string(),
        "kind": "timeblend",
        "frames": indices.len(),
        "indices": indices,
        "width": w,
        "height": h,
    }))
}

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
fn cull_evaluate(session: &mut Session, args: &Value) -> Result<Value, String> {
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
fn cull_extract(session: &mut Session, args: &Value) -> Result<Value, String> {
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

fn export_video(session: &mut Session, args: &Value) -> Result<Value, String> {
    if session.is_running() {
        return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
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
            "未知编码 preset {}；可用：{}",
            echo_text(&preset),
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
                return Err(format!("音频文件不存在：{}", echo_path(&path)));
            }
            Some(path)
        }
        None => None,
    };
    let audio_opts = audio_options(args)?;
    let keep_frames = opt_bool(args, "keep_frames")?.unwrap_or(false);
    let resume = opt_bool(args, "resume")?.unwrap_or(false);
    // 交付预设的 key 先解析掉，且**在找 ffmpeg 之前**：参数写错就该立刻说，别让人等
    // 「找不到 ffmpeg」回来才发现 key 拼错了。真正写进选项在后面（选项还没构造）。
    //
    // Resolve the delivery key here, **before locating ffmpeg**: a typo should be reported
    // immediately instead of after an unrelated "ffmpeg not found". Writing it into the
    // options happens later, once they exist.
    let delivery_preset = match opt_str(args, "delivery")? {
        Some(key) => {
            let table = aevum_core::delivery::DeliveryTable::load();
            match table.get(key) {
                Some(found) => Some(found.clone()),
                None => {
                    return Err(format!(
                        "未知交付预设 {}；可用：{}（表在 {}，改表不用改代码）",
                        echo_text(key),
                        table.keys().join("、"),
                        aevum_core::delivery::DeliveryTable::path().display()
                    ));
                }
            }
        }
        None => None,
    };
    require_batch_confirm(args, "aevum_export_video")?;
    aevum_core::paths::ensure_out_dir_safe(&root, &out_dir)?;
    if session.is_source(&video_out) {
        return Err(format!("拒绝覆盖源图：{}", echo_path(&video_out)));
    }
    if video_out.exists() && !confirm(args) {
        return Err(format!(
            "目标已存在，需 confirm: true：{}",
            echo_path(&video_out)
        ));
    }
    // 先找 ffmpeg：缺了就别白跑一遍逐帧渲染。
    let ffmpeg = export::find_ffmpeg()
        .ok_or_else(|| "找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned())?;

    let paths = session.require_seq()?.paths();
    // copy：这一次导出改用某套副本的调色；省略就是会话当前这套。
    let copy = opt_str(args, "copy")?.unwrap_or_default();
    let params = session.frame_params_of(n, copy)?;
    let gains = session.gains(n);
    let (crop, offsets) = session.alignment(n);
    let dynamic = opt_dynamic(args)?;
    let mut options = RenderOptions {
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
        dynamic,
        watermark: opt_watermark(args)?,
        // 运镜跟着这次导出走：没传 motion 就沿用会话工程里那份，与队列同口径。
        motion: motion_arg(session, args)?,
        metadata: session.metadata(),
    };
    // 平台交付预设：一次填好一组口径，但**显式参数优先**——预设是省掉逐项填，不是
    // 「不许改其中一项」。key 的解析与报错在找 ffmpeg 之前就做完了，这里只负责写值。
    //
    // Delivery preset: fills in a whole convention at once, but **explicit arguments win**;
    // a preset saves typing, it does not forbid changing one number. The key was resolved
    // (and reported on a typo) before ffmpeg was located; this only writes the values.
    if let Some(found) = &delivery_preset {
        found.apply_to(&mut options)?;
        if let Some(fps) = opt_f64(args, "fps")? {
            options.fps = fps;
        }
        if let Some(crf) = opt_u32(args, "crf")? {
            options.crf = crf as u8;
        }
        if let Some(codec) = opt_str(args, "codec")? {
            options.codec = parse_codec(codec)?;
        }
        if let Some(preset) = opt_str(args, "preset")? {
            options.preset = preset.to_owned();
        }
        if let Some(size) = target_size_arg(args)? {
            options.target_size = Some(size);
            options.long_side = None;
        } else if let Some(side) = opt_u32(args, "long_side")? {
            options.long_side = Some(side);
            options.target_size = None;
        }
    }
    // 变速的输出帧数按**最终**帧率算：预设可能把它从 24 改到 30，用旧值算会让
    // 变速计划与成片帧率对不上。
    let output_fps = options.fps;
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
        retime: retime_plan(args, n, output_fps)?,
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
                    .ok_or_else(|| "找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned())?;
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
        .name("aevum-export".to_owned())
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
        "note": "用 aevum_align_status 查询进度，aevum_cancel 取消。"
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

/// 扫到的一条序列。
///
/// One sequence found by the scan.
struct FoundSeq {
    root: PathBuf,
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
fn scan_sequences(root: &Path, depth: usize) -> (Vec<FoundSeq>, Vec<(PathBuf, String)>) {
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

fn scan_depth(args: &Value) -> Result<usize, String> {
    let depth = opt_u32(args, "depth")?.unwrap_or(1) as usize;
    if depth > 4 {
        return Err("depth 必须在 0..=4（0 = 只看这个目录自己）".to_owned());
    }
    Ok(depth)
}

/// `aevum_sequences_scan`：只看不写，不需要 confirm。
///
/// `aevum_sequences_scan`: read-only, no `confirm` needed.
fn sequences_scan(_session: &mut Session, args: &Value) -> Result<Value, String> {
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
fn batch_run(session: &mut Session, args: &Value) -> Result<Value, String> {
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

fn align_run(session: &mut Session, args: &Value) -> Result<Value, String> {
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
        "note": "running 为 true 时可用 aevum_cancel 请求取消；同步调用期间请求会排队，取消只对后台任务即时生效。"
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
    let entries = aevum_core::journal::tail(&root, limit);
    Ok(json!({
        "path": aevum_core::journal::path(&root).display().to_string(),
        "count": entries.len(),
        "entries": entries,
    }))
}

fn xmp_read(args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path.is_file() {
        return Err(format!("文件不存在：{}", echo_path(&path)));
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
        return Err(format!("拒绝覆盖源图：{}", echo_path(&path)));
    }
    if path.exists() && !confirm(args) {
        return Err(format!(
            "目标已存在，需 confirm: true：{}",
            echo_path(&path)
        ));
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
        return Err(format!("文件不存在：{}", echo_path(&path)));
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
        return Err(format!(
            "目标已存在，需 confirm: true：{}",
            echo_path(&path)
        ));
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

/// `aevum_export_video` 的配乐选项；全部可省，缺省与 GUI 默认一致。
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
            format!(
                "未知 acodec {}；可用 auto / copy / aac128 / aac192 / aac256 / aac320",
                echo_text(codec)
            )
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

/// 把用户给的字符串塞进错误串的唯一写法：截到 `MAX_ECHO_CHARS`。
///
/// The one way to put a caller-supplied string into an error string: clip it to
/// `MAX_ECHO_CHARS`.
pub(crate) fn echo_text(text: &str) -> String {
    clip_echo(text)
}

/// 把路径塞进错误串的唯一写法：先显示成字符串，再截到 `MAX_ECHO_CHARS`。
///
/// 为什么要有这个漏斗：路径来自调用方，可以长到几十 KB。`format!("…：{}", path.display())`
/// 这种写法在 1.4.0 之前散在十来个地方，改一处漏一处；收成一个函数之后，新代码要么用它，
/// 要么一眼就能在看 diff 时被发现。
///
/// The one way to put a path into an error string: render it, then clip it to
/// `MAX_ECHO_CHARS`.
///
/// Why a funnel: paths come from the caller and can be tens of KB long. Before 1.4.0 the
/// `format!("...: {}", path.display())` shape was scattered over a dozen sites and every fix
/// missed one; with a single function, new code either calls it or stands out in review.
fn echo_path(path: &Path) -> String {
    echo_text(&path.display().to_string())
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
            echo_text(name.trim()),
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
        Some(raw) => OutputSharpen::parse(raw).ok_or_else(|| {
            format!(
                "未知锐化用途 {}；可用 off / screen / glossy / matte",
                echo_text(raw)
            )
        })?,
        None => OutputSharpen::Off,
    };
    let strength = match opt_str(args, "sharpen_strength")? {
        Some(raw) => SharpenStrength::parse(raw).ok_or_else(|| {
            format!(
                "未知锐化强度 {}；可用 low / standard / high",
                echo_text(raw)
            )
        })?,
        None => SharpenStrength::Standard,
    };
    Ok((for_, strength))
}

/// `color_space` 参数：省略即 sRGB（跟改造前的导出行为一致）。
fn opt_color_space(args: &Value) -> Result<ColorSpace, String> {
    match opt_str(args, "color_space")? {
        Some(raw) => ColorSpace::parse(raw)
            .ok_or_else(|| format!("未知色域 {}；可用 srgb / p3 / rec2020", echo_text(raw))),
        None => Ok(ColorSpace::default()),
    }
}

/// `dynamic` 参数：省略即 SDR。给了 `hdr-pq` 就顺手把色域补成 Rec.2020——那是
/// 唯一的合法组合，让调用方为这件事多写一个参数只是制造一次必然的报错。
///
/// 补完之后仍然走 `check_dynamic`：容器装不下（PNG/TIFF/JPEG/WebP）照样报错，
/// 别在这里悄悄退回 SDR。
///
/// The `dynamic` argument: SDR when omitted. Passing `hdr-pq` also fills in Rec.2020 — that
/// is the only legal combination, and making callers spell it out only creates a guaranteed
/// error. `check_dynamic` still runs afterwards: a container that cannot hold it
/// (PNG/TIFF/JPEG/WebP) errors out rather than quietly falling back to SDR.
fn opt_dynamic(args: &Value) -> Result<OutputDynamic, String> {
    match opt_str(args, "dynamic")? {
        Some(raw) => OutputDynamic::parse(raw)
            .ok_or_else(|| format!("未知动态范围 {}；可用 sdr / hdr-pq", echo_text(raw))),
        None => Ok(OutputDynamic::default()),
    }
}

/// 按动态范围把色域补成合法组合。
///
/// Fills in a legal color space for the given dynamic range.
fn color_space_for(dynamic: OutputDynamic, chosen: ColorSpace) -> ColorSpace {
    match dynamic {
        OutputDynamic::Sdr => chosen,
        OutputDynamic::HdrPq => ColorSpace::Rec2020,
    }
}

fn format_name(format: StillFormat) -> &'static str {
    format.key()
}

fn parse_codec(name: &str) -> Result<Codec, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "h264" | "264" | "avc" => Ok(Codec::H264),
        "h265" | "265" | "hevc" => Ok(Codec::H265),
        other => Err(format!("未知编码 {}；可用 h264 / h265", echo_text(other))),
    }
}

fn parse_estimator(name: &str) -> Result<Estimator, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "template" | "tpl" => Ok(Estimator::Template),
        "phase" => Ok(Estimator::Phase),
        other => Err(format!(
            "未知 estimator {}；可用 template / phase",
            echo_text(other)
        )),
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
            id: "aevum_open_sequence",
            description: "打开一个帧文件夹作为当前序列；已有 .aevum/project.json 时自动恢复参数。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "帧文件夹路径（绝对或相对）"}
                }),
                &["root"],
            ),
        },
        Command {
            id: "aevum_sequence_info",
            description: "返回当前序列的根目录、帧数、调色参数、去闪烁设置与最近输出。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_measure",
            description: "逐帧测光（均值/中值/中心/锐度），返回统计与降采样序列；先复用会话内存与盘上 `<序列根>/.aevum/measures.json`（与 GUI 同一本账），refresh 可强制重算。",
            input_schema: object(
                json!({
                    "refresh": {"type": "boolean", "default": false, "description": "true 时忽略会话内存与盘上账上的测光结果，重新逐帧测量（测出来的数仍写回账）"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台测光并立即返回，用 aevum_align_status 查进度"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_deflicker_analyze",
            description: "分析曝光闪烁并保存逐帧 EV 补偿；之后预览与导出会自动叠加。channels 为 true 时按 R/G/B 三通道分别校正，压的是彩色闪烁。align_exif 为 true 时先用 EXIF 的 ISO/快门/光圈把相机补偿剥掉（日转夜/圣杯延时），只压真正的测光误差。",
            input_schema: object(
                json!({
                    "window": {"type": "number", "minimum": 3, "maximum": 199, "description": "平滑窗口（奇数，默认 15）"},
                    "strength": {"type": "number", "minimum": 0, "maximum": 100, "description": "校正强度百分比（默认 100）"},
                    "multi": {"type": "boolean", "description": "长序列多遍窗口（默认 false）"},
                    "channels": {"type": "boolean", "description": "分通道（RGB）去闪烁，治色闪；默认 false 只补亮度"},
                    "align_exif": {"type": "boolean", "description": "按 EXIF 曝光三要素预对齐：日转夜延时里相机在不停补偿，不对齐的话去闪会把日夜过渡当闪烁压掉。缺三要素的帧不对齐也不猜；一个都没有时这一项等于没开"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_grade_get",
            description: "读取当前调色参数与预设名。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_grade_set",
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
            id: "aevum_grade_preset",
            description: "应用一个内置调色预设（原始、鲜艳风光、落日暖调、冷色电影等）。",
            input_schema: object(
                json!({
                    "name": {"type": "string", "description": "预设名：原始/鲜艳风光/落日暖调/冷色电影/复古胶片/黑白对比/夜景增强/通透风光/柔和人像/青橙电影"}
                }),
                &["name"],
            ),
        },
        Command {
            id: "aevum_grade_reset",
            description: "把调色参数恢复为默认值（预设「原始」）。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_grade_copy",
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
            id: "aevum_keyframe_add",
            description: "在第 frame 帧钉一个关键帧；不传 params 时用当前基准参数，传了则在其上做字段合并。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "帧号"},
                    "params": {
                        "type": "object",
                        "description": "可选的部分调色字段，与 aevum_grade_set 的 params 同口径",
                        "additionalProperties": true
                    }
                }),
                &["frame"],
            ),
        },
        Command {
            id: "aevum_keyframe_remove",
            description: "删除第 frame 帧的关键帧；该帧没有关键帧时返回结构化错误。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "帧号"}
                }),
                &["frame"],
            ),
        },
        Command {
            id: "aevum_keyframe_list",
            description: "列出全部关键帧（帧号与参数）；可选 frame 顺便返回该帧插值后的参数。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "可选：查询该帧插值后的参数"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_render_preview",
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
            id: "aevum_export_stills",
            description: "把整个序列按当前调色、关键帧插值、去闪烁补偿与对齐结果导出为静帧序列（PNG/TIFF/JPEG/WebP/AVIF，批量写，需 confirm）；可选 retime 变速、运动模糊与运镜（Ken Burns）。",
            input_schema: object(
                json!({
                    "out_dir": {"type": "string", "description": "输出帧目录（不能是序列根或其上级）"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8", "description": "AVIF 走随包 ffmpeg 的 libaom-av1，色域写 nclx 标记、不带 ICC/EXIF，逐帧起进程会比较慢"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "每张静帧旁边写一个同名 .xmp 旁车：dc: 的说明与版权四项（取自会话工程的元数据）+ crs: 的这一张调色参数"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：输出目录里已经写完的帧（连带该带的 .xmp）原样留着，这一趟只补没写出来的那些；上次跑到一半断了、这次接着导就用它，改了调色想重导就别开"},
                    "copy": {"type": "string", "description": "改用某套命名调色副本的调色导出；省略用会话当前调色"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时像素真转换并嵌对应 ICC（p3=Display P3，rec2020=BT.2020 SDR）"},
                    "long_side": {"type": "integer", "minimum": 1, "description": "按长边缩放；与 width/height 二选一"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；cover 放大后居中裁切）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给）"},
                    "retime": retime_schema(),
                    "motion": motion_schema(),
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台导出并立即返回，用 aevum_align_status 查进度"}
                }),
                &["out_dir"],
            ),
        },
        Command {
            id: "aevum_export_video",
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
                    "delivery": {"type": "string", "description": "平台交付预设 key（如 douyin-vertical / bilibili-1080p）：一次填好画幅、帧率、编码器与质量档。显式给的 fps/crf/codec/preset/width/height/long_side 仍会覆盖它。可用 key 见 aevum_sequence_info 的 delivery 列表；预设表在配置目录的 delivery-presets.toml，改表不用改代码"},
                    "retime": retime_schema(),
                    "motion": motion_schema(),
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围；成片目前只出 SDR，传 hdr-pq 会直接报错（HDR 视频编码还没做），静帧的 HDR 走 export_stills + avif"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；决定 VUI 的 primaries/transfer/matrix 与中间帧的 ICC，rec2020 的传递函数在 VUI 里只能标 bt709"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台导出并立即返回，用 aevum_align_status 查进度"}
                }),
                &["out_dir", "video_out"],
            ),
        },
        Command {
            id: "aevum_hdr_merge",
            description: "把同场景多张不同曝光融成一张静帧（Mertens 曝光融合）；自动套用对齐的统一裁切与逐帧位移，尺寸对不齐会拒绝（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要融合的帧下标（0 开始）；给了就忽略 center/around"},
                    "center": {"type": "integer", "minimum": 0, "description": "包围中心帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 4, "default": 1, "description": "中心前后各取几帧（1..4，最多凑成 9 张）"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 HDR/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_focus_merge",
            description: "把一组不同对焦距离的帧融成一张全景深（焦点合成：深度图法逐像素取测度最强的帧、无细节区兜底参考帧、过渡带羽化；加权平均法按测度四次方混合）；自动套用对齐的统一裁切与逐帧位移，尺寸对不齐会拒绝（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "参与合成的帧下标（0 开始）；给了就忽略 start/around"},
                    "start": {"type": "integer", "minimum": 0, "description": "起始帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": (aevum_core::focus::MAX_FRAMES - 1), "default": 7, "description": "起始帧之后再取几帧（1..31）；省略=7（共 8 帧）"},
                    "method": {"type": "string", "enum": ["depthmap", "weighted", "pyramid"], "default": "depthmap", "description": "depthmap 逐像素选测度最强的帧（无细节区兜底参考帧）；weighted 按测度四次方加权混合；pyramid 拉普拉斯塔按层融合（多尺度自带过渡，feather 与 depth-preview 不适用）"},
                    "align": {"type": "boolean", "default": false, "description": "合成前自动对齐：先估每帧 8×6 网格位移再逐块补偿，压住对焦呼吸的轻微缩放。默认关闭；收益不足 0.5% 自动回退成不做（成片与关闭时逐点一致）；估计失败照样出图，只是按未对齐几何"},
                    "radius": {"type": "integer", "minimum": 0, "maximum": 32, "default": 2, "description": "焦点测度的分析半径（px），0 = 逐像素原始响应"},
                    "threshold": {"type": "number", "minimum": 0, "maximum": 100, "default": 0, "description": "无细节阈值（全图最佳测度的百分位，0..100）：低于这条线的像素整块判给参考帧；0 = 关闭兜底"},
                    "smooth_z": {"type": "integer", "minimum": 0, "maximum": 16, "default": 1, "description": "深度图去斑半径（px，模值滤波），只对 depthmap 生效"},
                    "feather": {"type": "number", "minimum": 0, "maximum": 64, "default": 2, "description": "过渡羽化半径（px），小于 0.5 视为硬切，只对 depthmap 生效"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 焦点合成/"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台合成并立即返回，用 aevum_align_status 查进度，aevum_cancel 取消；长任务期间 status / cancel 都能应答"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_pano_merge",
            description: "把相邻几张有重叠的照片拼成一张宽幅全景（特征点匹配 + RANSAC 单应 + 羽化融合）；不套对齐位移，靠特征点自己求几何（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要拼接的帧下标（0 开始，按接片顺序）；给了就忽略 start/around"},
                    "start": {"type": "integer", "minimum": 0, "description": "起始帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 11, "default": 1, "description": "起始帧之后再接几张（1..11，最多凑成 12 张）"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 全景/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_stack_merge",
            description: "把同机位的一段帧按统计量合成一张：星轨取最亮、降噪取平均、去卫星取中值、杂散光与热噪点做 σ 截尾（批量写，需 confirm）；自动套用对齐的统一裁切与逐帧位移，尺寸对不齐会拒绝。无头会话没有「当前帧」，省 center 就取序列中间那段。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要堆栈的帧下标（0 开始）；给了就忽略 center/around"},
                    "center": {"type": "integer", "minimum": 0, "description": "包围中心帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 32, "default": 4, "description": "中心前后各取几帧（1..32）；省略=4（共 9 帧）"},
                    "mode": {"type": "string", "enum": ["lighten", "average", "median", "sigma-clip"], "default": "lighten", "description": "统计量：星轨用 lighten、降噪用 average、去卫星用 median、杂散光用 sigma-clip"},
                    "clip_sigma": {"type": "number", "exclusiveMinimum": 0, "maximum": 10, "default": 3, "description": "σ 截尾的倍数，常用 2–4；只对 sigma-clip 生效"},
                    "fill_gaps": {"type": "boolean", "default": true, "description": "合成后仍全黑的像素用 3×3 邻域补上，闭合星轨之间的空隙"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 堆栈/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_timeblend_merge",
            description: "把不同时刻的帧合成为一张：天地合成（天取末帧、地取首帧，可自动推地平线）或时间切片（横向分带，一带一帧、从早到晚铺满整幅），羽化区里的移动物可自动去鬼影（批量写，需 confirm）；自动套用对齐的统一裁切与逐帧位移，帧号升序即时间顺序。无头会话没有「当前帧」，省 start 就从序列中位帧起取一段。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "参与合成的帧下标（0 开始，升序即时间顺序）；给了就忽略 start/around"},
                    "start": {"type": "integer", "minimum": 0, "description": "起始帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 31, "default": 7, "description": "起始帧之后再取几帧（1..31）；省略=7（共 8 帧）"},
                    "layout": {"type": "string", "enum": ["sky-ground", "bands"], "default": "sky-ground", "description": "sky-ground 按地平线分天地；bands 横向分带、一带一帧"},
                    "bands": {"type": "integer", "minimum": 1, "maximum": 32, "default": 8, "description": "带数，只对 bands 生效；等于帧数时一带一帧"},
                    "feather": {"type": "number", "minimum": 0, "maximum": 0.5, "default": 0.04, "description": "羽化宽度占画面高度的比例，0.04 约等于 4%"},
                    "horizon": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.5, "description": "地平线位置（从顶部算起的比例），只对 sky-ground 生效；horizon_auto 为 true 时忽略"},
                    "horizon_auto": {"type": "boolean", "default": false, "description": "按行亮度梯度自动推地平线（找不到退回 horizon）"},
                    "deghost": {"type": "number", "minimum": 0, "maximum": 4, "default": 0.5, "description": "去鬼影阈值（EV）：某帧某块与自己 3×3 空间中位数偏离超过它就按标签衰减权重；0 = 关闭"},
                    "match_exposure": {"type": "boolean", "default": true, "description": "混之前把来源帧亮度对齐到参考帧（钳 ±2 EV），否则接缝处是一道台阶"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 时间合成/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_cull_evaluate",
            description: "按四条判据逐帧裁决并给出理由：曝光越界（EV）、与相邻帧的突变、清晰度低于序列中位的比例、读不出来；立即返回。测光与 GUI 同一条路径（512 长边）：先读盘上 `<序列根>/.aevum/measures.json`，没有的帧才解码，测完按同一格式写回。所以 GUI 已经测过的序列，无头这边直接出裁决、不再解一遍图，反之亦然；缓存读写失败会静默退化成现测，不影响裁决。账的键是 `(mtime, 字节数)`：文件内容变了但这两项没变（复制工具保留时间戳覆盖同名文件）时，用 `aevum_measure --refresh` 强制重算再评估。只评估不标记：GUI 侧的 apply（把判废帧标成「排除」）是会话态，无头剔除走 aevum_cull_extract 的 exclude 参数。",
            input_schema: object(
                json!({
                    "suggest": {"type": "boolean", "description": "用序列自身的亮度中位数绝对偏差推曝光区间（3×1.4826×MAD，下限 0.5 EV）；给了则覆盖 ev_low/ev_high"},
                    "ev_low": {"type": "number", "minimum": -10, "maximum": 0, "description": "曝光下限（EV，相对 18% 中灰）；省略=-1"},
                    "ev_high": {"type": "number", "minimum": 0, "maximum": 10, "description": "曝光上限（EV）；省略=1"},
                    "jump_ev": {"type": "number", "minimum": 0, "maximum": 10, "description": "与相邻帧的突变阈值（EV）；0 关闭这条判据；省略=1"},
                    "sharp_ratio": {"type": "number", "minimum": 0, "maximum": 1, "description": "清晰度低于序列中位的这个比例就判废；0 关闭这条判据；省略=0"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_cull_extract",
            description: "把保留下来的帧复制成一条新序列（剔除成子序列）：路径安全检查先行（不能是源内部、盘根或非空目录），复制保留 mtime、测光缓存一并带走（新序列不用重测）。exclude 给要剔除的帧下标（通常来自 aevum_cull_evaluate 的 indices）；dry_run: true 只报帧数与体积，不写任何东西；非 dry_run 需 confirm。",
            input_schema: object(
                json!({
                    "target": {"type": "string", "description": "目标目录（绝对路径）；不能是源序列根或其内部、盘符根、已存在且非空的目录"},
                    "exclude": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要剔除的帧下标（0 开始）；省略=全部保留"},
                    "dry_run": {"type": "boolean", "description": "true 只报会复制几帧、共多少字节，不写任何东西"},
                    "confirm": {"type": "boolean", "description": "非 dry_run 时必须为 true"}
                }),
                &["target"],
            ),
        },
        Command {
            id: "aevum_project_get",
            description: "返回当前会话对应的工程内容（含参数、去闪烁设置）与磁盘上的工程路径。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_project_save",
            description: "把当前参数与去闪烁设置写入 <序列>/.aevum/project.json，保留评级与关键帧。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_journal_list",
            description: "读取当前序列的操作日志末尾（只记成功的写操作，一行一条 JSON）。",
            input_schema: object(
                json!({
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50, "description": "最多返回条数"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_xmp_read",
            description: "读取一个 XMP 预设/旁车文件，返回可用的调色参数（不自动应用）。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": "XMP 文件路径"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "aevum_xmp_write",
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
            id: "aevum_cube_import",
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
            id: "aevum_cube_export",
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
            id: "aevum_align",
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
                    "background": {"type": "boolean", "default": false, "description": "true 时后台对齐并立即返回，用 aevum_align_status 查进度"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_align_status",
            description: "查询后台任务（对齐/测光/导出）的进度、是否运行中、上次对齐报告摘要与最近错误。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_cancel",
            description: "请求取消正在运行的后台对齐/测光/导出/队列；空闲时也返回成功，可重复调用（幂等）。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_queue_list",
            description: "列出批量渲染队列（与 GUI 共用 render_queue.json）；工程参数在渲染时从各自的 .aevum/project.json 读取。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_queue_add",
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
            id: "aevum_queue_remove",
            description: "从渲染队列移除一段（只改队列，不动素材）。",
            input_schema: object(
                json!({
                    "index": {"type": "integer", "minimum": 1, "description": "队列序号，从 1 开始"}
                }),
                &["index"],
            ),
        },
        Command {
            id: "aevum_queue_clear",
            description: "清空渲染队列（不删除已渲染的成片）；正在渲染时拒绝。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_queue_run",
            description: "逐段渲染整个队列（批量写，必须 confirm）；默认同步跑完，background=true 时立即返回。",
            input_schema: object(
                json!({
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台渲染并立即返回，用 aevum_queue_status 查进度"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_queue_status",
            description: "查询队列进度（段数、当前阶段、每段最近结果）与队列文件路径。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_queue_cancel",
            description: "请求取消正在渲染的队列；已完成的段保留结果，未开始的段保持待渲。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_sequences_scan",
            description: "枚举目录下的序列（只看不写）：根目录自己 + 往下 depth 层子目录；同时列出「有图却读不出来」的目录与原因。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "父目录；序列通常是它的子目录"},
                    "depth": {"type": "integer", "minimum": 0, "maximum": 4, "default": 1, "description": "往下找几层；0 = 只看这个目录自己"}
                }),
                &["root"],
            ),
        },
        Command {
            id: "aevum_batch_run",
            description: "无人值守批量：扫目录下所有序列 → 入队 → 直接渲染，一次调用返回逐段结果（成功数/失败数/每段原因）。默认先清空队列，append=true 才追加。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "父目录；下面的序列全部参与"},
                    "depth": {"type": "integer", "minimum": 0, "maximum": 4, "default": 1, "description": "往下找几层；0 = 只看这个目录自己"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "append": {"type": "boolean", "default": false, "description": "true 时保留队列里已有的段并追加；默认先清空"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台渲染并立即返回，用 aevum_queue_status 查进度"},
                    "out": {"type": "string", "description": "只在扫到单条序列时可用；多条时省略（每段默认写 <序列目录>/<名字>_成片.mp4）"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；省略=原生）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给；省略=原生）"},
                    "fps": {"type": "number", "exclusiveMinimum": 0, "maximum": 240, "default": 24},
                    "codec": {"type": "string", "enum": ["h265", "h264"], "default": "h265"},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 51, "default": 18},
                    "preset": {"type": "string", "default": "medium", "description": "x264/x265 速度档：ultrafast…veryslow"},
                    "keep_frames": {"type": "boolean", "default": false, "description": "编码后保留中间帧"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：已经写完的帧原样留着，这一趟只补没写出来的那些"},
                    "copy": {"type": "string", "description": "各段统一改用序列工程里某套命名调色副本渲染；省略用工程主调色"}
                }),
                &["root", "confirm"],
            ),
        },
    ]
}
