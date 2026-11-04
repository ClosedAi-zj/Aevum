//! 导出与渲染域：预览、静帧导出、视频导出、变速、后台任务的启停。
//!
//! 边界：所有「真的生成文件」的路径都在这里（合成类除外，见 `merge`）。前台/后台
//! 两条走的是同一份 `ExportPlan`：校验一次、跑的人不再读会话，切换同步/异步不改像素。
//! 参数翻译（`parse_format` / `opt_quality` / `parse_retime`）留在 `shared`，这里只负责
//! 把它们拼成计划并执行。
//!
//! The export and rendering domain: preview, still export, video export, retiming, and
//! starting/stopping background jobs.
//!
//! Boundary: every path that actually produces a file lives here (the merge family is the
//! exception, see `merge`). Foreground and background share one `ExportPlan`: validation
//! happens once and the runner never reads the session again, so switching between
//! synchronous and asynchronous changes no pixel. Argument translation (`parse_format`,
//! `opt_quality`, `parse_retime`) stays in `shared`; this module only assembles the plan and
//! executes it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use aevum_core::audio::AudioOptions;
use aevum_core::develop::Ev;
use aevum_core::export::{self, Codec, JobProgress, OutputMode, RenderOptions, Stage};
use aevum_core::grade::GradeParams;
use aevum_core::interp;
use aevum_core::metrics;
use aevum_core::preview;
use aevum_core::retime::{self, Retime, SpeedKey};
use serde_json::{Value, json};

use super::shared::{
    X264_PRESETS, absolute, arg_str, audio_options, check_long_side, check_video_format, confirm,
    echo_path, echo_text, format_name, motion_arg, opt_bool, opt_color_space, opt_dynamic, opt_f64,
    opt_quality, opt_sharpen, opt_str, opt_u32, opt_watermark, parse_codec, parse_format,
    require_batch_confirm, round6, target_size_arg,
};
use crate::session::{
    JobKind, JobOutcome, JobShared, MAX_LONG_SIDE, STAGE_ENCODE, STAGE_FRAMES, Session,
};

pub(super) fn render_preview(session: &mut Session, args: &Value) -> Result<Value, String> {
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

pub(super) fn export_stills(session: &mut Session, args: &Value) -> Result<Value, String> {
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

pub(super) fn export_video(session: &mut Session, args: &Value) -> Result<Value, String> {
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
