//! 合成域：HDR、全景、堆栈、时间合成、焦点合成。
//!
//! 边界：把**多张帧**融成一张（HDR/焦点/全景）或一条（堆栈/时间合成），再交给静帧
//! 导出那条尾巴落盘。融合本身不改调色参数；这里读会话只是为了拿帧路径、对齐裁切与
//! 输出口径。焦点合成与堆栈/时间合成共用 `merge_indices` / `load_aligned_frames`，
//! 这也是它们必须待在同一个文件里的原因。
//!
//! The merge domain: HDR, panorama, stacking, time blending, focus stacking.
//!
//! Boundary: fusing **several frames** into one still (HDR / focus / panorama) or one clip
//! (stacking / time blending), then handing the result to the still-export tail. Fusion
//! never changes colour parameters; the session is read only for frame paths, the alignment
//! crop, and the output conventions. Focus stacking and stacking/time blending share
//! `merge_indices` / `load_aligned_frames`, which is why they belong in one file.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use aevum_core::align::CropRect;
use aevum_core::errmsg::clip_echo;
use aevum_core::export::{self, RenderOptions, StillFormat};
use aevum_core::warp::{self, Framing};
use serde_json::{Value, json};

use super::shared::{
    absolute, color_space_for, echo_text, opt_bool, opt_color_space, opt_dynamic, opt_f64,
    opt_quality, opt_sharpen, opt_str, opt_u32, opt_watermark, parse_format, require_batch_confirm,
};
use crate::session::{JobKind, JobOutcome, JobShared, STAGE_ENCODE, STAGE_FRAMES, Session};

/// HDR 合并：把同场景多张不同曝光融成一张静帧（Mertens 曝光融合）。
///
/// 与 GUI 的 `hdr.merge` 同口径——按帧下标读原图、套对齐的统一裁切与逐帧位移、
/// 融合后走静帧导出那条输出尾（锐化 / 水印 / 色域 / EXIF）；融合本身不改调色。
pub(super) fn hdr_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
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
pub(super) fn focus_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
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

pub(super) fn pano_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
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
pub(super) fn stack_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
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
pub(super) fn timeblend_merge(session: &mut Session, args: &Value) -> Result<Value, String> {
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
