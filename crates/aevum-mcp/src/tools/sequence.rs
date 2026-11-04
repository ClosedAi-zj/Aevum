//! 序列域：打开帧文件夹、序列信息、逐帧测光、去闪烁分析。
//!
//! 边界：管「这条序列是什么、每帧有多亮、闪烁有多大」，不管调色参数（那是 `develop`），
//! 也不管写出去的文件（那是 `export`）。测光结果是整个工具箱共享的一份账
//! （内存 + `<序列根>/.aevum/measures.json`），所以这里的读写口径要和 GUI 一致。
//!
//! The sequence domain: opening a frame folder, sequence info, per-frame measurement, and
//! deflicker analysis.
//!
//! Boundary: how many frames there are, how bright each one is, how much it flickers.
//! Colour parameters belong to `develop`, and written files belong to `export`. The
//! measurements are one ledger shared by the whole toolbox (memory plus
//! `<sequence root>/.aevum/measures.json`), so the way this module reads and writes it has
//! to match the GUI.

use std::path::Path;
use std::sync::Arc;
use std::thread;

use aevum_core::deflicker;
use aevum_core::errmsg::clip_echo;
use aevum_core::exif;
use aevum_core::project;
use aevum_core::sequence::Sequence;
use serde_json::{Value, json};

use super::shared::{
    absolute, arg_str, echo_path, opt_bool, opt_f64, round6, stride_for, summarize,
};
use crate::session::{JobKind, MEASURE_LONG_SIDE, Session, measure_paths};

pub(super) fn open_sequence(session: &mut Session, args: &Value) -> Result<Value, String> {
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

pub(super) fn sequence_info(session: &mut Session) -> Result<Value, String> {
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

pub(super) fn measure(session: &mut Session, args: &Value) -> Result<Value, String> {
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

pub(super) fn deflicker_analyze(session: &mut Session, args: &Value) -> Result<Value, String> {
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
