//! 对齐、变速、运镜、导出与工程命令：这些子系统的命令出口：起停、状态查询与参数写入。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Commands for alignment, retiming, motion, export and the project file: The command surfaces of those subsystems: start/stop, status queries and parameter writes.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    pub(super) fn cmd_align_run(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.headless {
            return Err("当前是无界面模式，不能启动对齐".to_owned());
        }
        if self.align_busy {
            return Err("对齐已在进行中；用 align.status 查询进度".to_owned());
        }
        let reference = opt_u32(args, "reference")?;
        if let Some(reference) = reference
            && (reference == 0 || reference as usize > n)
        {
            return Err(crate::i18n::tr_fmt("reference 超出范围：1..={}", &[&n]));
        }
        let estimator = match opt_str(args, "estimator")? {
            Some(name) => Some(match name.trim().to_ascii_lowercase().as_str() {
                "template" | "模板" => Estimator::Template,
                "phase" | "相位" => Estimator::Phase,
                other => {
                    return Err(crate::i18n::tr_fmt(
                        "未知估计器 {}；可用 template / phase",
                        &[&other],
                    ));
                }
            }),
            None => None,
        };
        // 参数全部校验通过后再改状态，避免失败的命令留下半套设置。
        if let Some(reference) = reference {
            self.align_ref = reference as usize - 1;
        }
        if let Some(estimator) = estimator {
            self.align_estimator = estimator;
        }
        self.run_align(ctx);
        Ok(json!({
            "started": true,
            "frames": n,
            "reference": self.align_ref + 1,
            "estimator": estimator_name(self.align_estimator),
            "note": "长任务已启动；用 align.status 查询进度",
        }))
    }

    pub(super) fn align_status_json(&self) -> Value {
        let report = self.align_report.as_ref().map(|report| {
            json!({
                "frames": report.offsets.len(),
                "crop": rect_json(report.crop),
                "issues": report.issues.iter().take(8).map(|issue| json!({
                    "frame": issue.frame + 1,
                    "kind": issue.kind,
                    "detail": issue.detail,
                })).collect::<Vec<_>>(),
                "elapsed_secs": round6(report.elapsed_secs),
            })
        });
        let progress = self
            .align_progress()
            .map(|(done, total)| json!({"done": done, "total": total}));
        json!({
            "busy": self.align_busy,
            "progress": progress,
            "message": self.align_msg,
            "report": report,
        })
    }

    /// 面板读数：按速度曲线估输出帧数与时长。只积分曲线，不建整段取样计划——
    /// 几十万帧的计划每帧 UI 算一遍会把界面拖住。
    ///
    /// Panel readout: estimate the output frame count and duration from the speed curve.
    /// Integrates the curve only and never builds a whole sampling plan -- a plan over
    /// hundreds of thousands of frames, recomputed every UI frame, would stall the UI.
    pub fn retime_estimate(&self) -> Result<(usize, f64), String> {
        let n = self.frame_count();
        let fps = self.output_fps.max(1.0) as f64;
        if n == 0 {
            return Ok((0, 0.0));
        }
        if self.retime.is_identity() {
            return Ok((n, n as f64 / fps));
        }
        let retime = self.retime.normalized()?;
        let frames = retime::output_frames(&retime.keys, n)?;
        Ok((frames, frames as f64 / fps))
    }

    /// 变速面板与命令的当前值，外加按序列帧数换算的帧数、时长。
    ///
    /// `frames`/`seconds` 是曲线积分出来的真实输出量：常速或没开变速就等于源帧数，
    /// 面板上那行「N 源帧 → M 帧 · X 秒」直接念这两个数，不用自己估。
    pub(super) fn retime_status_json(&self) -> Value {
        let n = self.frame_count();
        let fps = self.output_fps.max(1.0) as f64;
        let (frames, seconds, error) = match self.retime_estimate() {
            Ok((frames, seconds)) => (Some(frames), Some(round6(seconds)), None),
            Err(e) => (None, None, Some(e)),
        };
        json!({
            "enabled": self.retime.enabled,
            "shutter": round6(self.retime.shutter),
            "keys": self
                .retime
                .keys
                .iter()
                .map(|k| json!({"at": round6(k.at), "speed": round6(k.speed)}))
                .collect::<Vec<_>>(),
            "identity": self.retime.is_identity(),
            "source_frames": n,
            "fps": round6(fps),
            "frames": frames,
            "seconds": seconds,
            "max_output_frames": retime::MAX_OUTPUT_FRAMES,
            "error": error,
        })
    }

    /// 写变速设定。`speed` / `keys` / `ease` 三种写法互斥：都收会让「传了两个」
    /// 的调用悄悄只用其中一个，不如当场拒绝。
    pub(super) fn cmd_retime_set(&mut self, args: &Value) -> Result<Value, String> {
        let mut next = self.retime.clone();
        if let Some(v) = args.get("enabled") {
            next.enabled = v.as_bool().ok_or("enabled 必须是布尔")?;
        }
        if let Some(v) = args.get("reset") {
            if !v.as_bool().ok_or("reset 必须是布尔")? {
                return Err(
                    "reset 只接受 true：要改回去就逐项写 enabled / speed / shutter".to_owned(),
                );
            }
            next = Retime::default();
        }
        let curve_given = ["speed", "keys", "ease"]
            .iter()
            .filter(|key| args.get(*key).is_some_and(|v| v != &Value::Null))
            .copied()
            .collect::<Vec<_>>();
        if curve_given.len() > 1 {
            return Err(crate::i18n::tr_fmt(
                "{} 同时给了：变速曲线只能选一种写法（speed 全程一个倍速 / keys 多点曲线 / ease 缓入缓出）",
                &[&curve_given.join("、")],
            ));
        }
        if !curve_given.is_empty() {
            // 给了曲线就是当真要变速，省掉「先 enabled 再 speed」两步。
            next.enabled = true;
        }
        if let Some(speed) = opt_f64(args, "speed")? {
            next = Retime::uniform(speed, next.shutter);
        }
        if let Some(raw) = args.get("keys") {
            next.keys = parse_speed_keys(raw)?;
        }
        if let Some(ease) = args.get("ease") {
            if !ease.is_object() {
                return Err("ease 要写成对象：{ peak, edge?, in_at?, out_at? }".to_owned());
            }
            let edge = opt_f64(ease, "edge")?.unwrap_or(1.0);
            let peak = opt_f64(ease, "peak")?
                .ok_or_else(|| "ease 里必须给 peak（中段倍速）".to_owned())?;
            let in_at = opt_f64(ease, "in_at")?.unwrap_or(0.25);
            let out_at = opt_f64(ease, "out_at")?.unwrap_or(0.75);
            next = Retime::ease(edge, peak, in_at, out_at, next.shutter);
        }
        if let Some(shutter) = opt_f64(args, "shutter")? {
            if !(0.0..=1.0).contains(&shutter) {
                return Err(crate::i18n::tr_fmt(
                    "shutter 是曝光区间占本帧源跨度的比例，要在 0..1（0 锐利、1 全平均），实得 {}",
                    &[&shutter],
                ));
            }
            next.shutter = shutter;
        }
        // 关着的时候不做规整：`normalized()` 会把整块摊平，面板关一下再开曲线就没了。
        // 数字写错了也留到开变速那天再报，反正那时导出前一定会校验。
        if next.enabled {
            next = next.normalized()?;
        }
        self.retime = next;
        Ok(self.retime_status_json())
    }

    /// 运镜作用的那一幅像素尺寸：拉直 + 导出取景之后的宽高，与导出里 `bw × bh` 同口径。
    /// 取景框是相对这一幅归一化的，所以画布换算与推近读数都认它。
    ///
    /// The pixel size the motion acts on: width and height after straightening and export
    /// cropping, the same definition as the export's frame size.
    /// The viewfinder is normalised against this frame, so the canvas conversion and the
    /// push-in readout both go by it.
    pub fn motion_frame_size(&self) -> (u32, u32) {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        match self.effective_crop() {
            Some(c) => (c.w.max(1), c.h.max(1)),
            None => (w, h),
        }
    }

    /// 运镜的输出画幅：成片面板给了精确宽高就认它，否则就是那一幅原样铺出去。
    ///
    /// The motion output frame: the exact width and height from the render panel when given,
    /// otherwise that frame laid out as is.
    pub fn motion_output_size(&self) -> (u32, u32) {
        if self.output_w >= 2 && self.output_h >= 2 {
            (self.output_w, self.output_h)
        } else {
            self.motion_frame_size()
        }
    }

    /// 按输出帧号取当前运镜的取景框；没开就返回 `None`，预览照原来的路走。
    ///
    /// The viewfinder of the current motion at the given output frame; `None` when it is
    /// off, and the preview takes the original path.
    pub fn motion_view_at(&self, i: usize, total: usize) -> Option<Viewfinder> {
        self.motion
            .is_active()
            .then(|| self.motion.view_of(i, total.max(1)))
    }

    pub(super) fn motion_status_json(&self) -> Value {
        let (fw, fh) = self.motion_frame_size();
        let (ow, oh) = self.motion_output_size();
        let box_json = |v: &Viewfinder| {
            let s = v.normalized();
            [round6(s.x), round6(s.y), round6(s.w), round6(s.h)].to_vec()
        };
        json!({
            "enabled": self.motion.enabled,
            "active": self.motion.is_active(),
            "ease": self.motion.ease.key(),
            "ease_cn": self.motion.ease.cn(),
            "from": box_json(&self.motion.from),
            "to": box_json(&self.motion.to),
            "frame": [fw, fh],
            "output": [ow, oh],
            // 推近倍数：>1 就是拿少数像素撑大画面，面板按它给一句「再推就糊了」。
            "zoom_from": round6(self.motion.from.zoom(ow, oh, fw, fh)),
            "zoom_to": round6(self.motion.to.zoom(ow, oh, fw, fh)),
            "source_frames": self.frame_count(),
        })
    }

    /// 写运镜设定：解析交给 `Motion::patched`（核心里那份，MCP 与控制通道同一条路），
    /// 这里只负责落状态与回报。
    pub(super) fn cmd_motion_set(&mut self, args: &Value) -> Result<Value, String> {
        self.motion = self.motion.patched(args)?;
        // 取景框一动，预览就得重出：画布上那块像素跟着框走。
        self.preview_dirty = true;
        Ok(self.motion_status_json())
    }

    pub(super) fn cmd_export(
        &mut self,
        ctx: &Context,
        id: &str,
        args: &Value,
    ) -> Result<Value, String> {
        let video = id == "export.video";
        let seq = self.require_sequence()?;
        let n = seq.len();
        let root = seq.root.clone();
        let seq_name = seq.display_name();
        let sources = seq.paths();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.headless {
            return Err("当前是无界面模式，不能从命令通道启动导出".to_owned());
        }
        if self.export_busy {
            return Err("导出已在进行中；用 export.status 查询，或 export.cancel 取消".to_owned());
        }
        // 批量写操作的硬要求：覆盖已有输出、整段导出都必须显式确认。
        if !confirm(args) {
            return Err(crate::i18n::tr_fmt(
                "{} 是批量写操作，需参数 confirm: true",
                &[&id],
            ));
        }
        // 先把所有参数校验完，再一次性落到状态里，失败时不留半套设置。
        let size = match (opt_u32(args, "width")?, opt_u32(args, "height")?) {
            (Some(width), Some(height)) => {
                if width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1 {
                    return Err(crate::i18n::tr_fmt(
                        "width/height 必须是偶数且不小于 2：{}×{}",
                        &[&width, &height],
                    ));
                }
                Some((width, height))
            }
            (None, None) => None,
            _ => {
                return Err("width 与 height 需同时给出（或都不给，保持当前规格）".to_owned());
            }
        };
        let out_dir = match opt_str(args, "out_dir")? {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                if dir.as_os_str().is_empty() {
                    return Err("out_dir 不能为空；省略则用当前输出目录".to_owned());
                }
                dir
            }
            None if self.render_out_dir.as_os_str().is_empty() => root.join("成片"),
            None => self.render_out_dir.clone(),
        };
        ensure_out_dir_safe(&root, &out_dir)?;
        let fps = opt_f64(args, "fps")?;
        if let Some(fps) = fps
            && !(fps > 0.0 && fps <= 240.0)
        {
            return Err("fps 必须在 (0, 240]".to_owned());
        }
        let crf = opt_u32(args, "crf")?;
        if let Some(crf) = crf
            && crf > 51
        {
            return Err("crf 必须在 0..51".to_owned());
        }
        let codec = match opt_str(args, "codec")? {
            Some(codec) => Some(match codec.trim().to_ascii_lowercase().as_str() {
                "h264" | "264" | "avc" => "H.264".to_owned(),
                "h265" | "265" | "hevc" => "HEVC".to_owned(),
                other => {
                    return Err(crate::i18n::tr_fmt(
                        "未知编码 {}；可用 h264 / h265",
                        &[&other],
                    ));
                }
            }),
            None => None,
        };
        // 色域静帧和成片都用同一条参数：省略就保持面板当前选择。
        let color_space = opt_str(args, "color_space")?
            .map(|raw| {
                ColorSpace::parse(raw).ok_or_else(|| {
                    crate::i18n::tr_fmt("未知色域 {}；可用 srgb / p3 / rec2020", &[&raw])
                })
            })
            .transpose()?;
        // 动态范围与色域同理：省略就保持面板当前选择。
        let dynamic = opt_str(args, "dynamic")?
            .map(|raw| {
                OutputDynamic::parse(raw).ok_or_else(|| {
                    crate::i18n::tr_fmt("未知动态范围 {}；可用 sdr / hdr-pq", &[&raw])
                })
            })
            .transpose()?;
        // 容器与质量只在静帧那一路有意义：视频模式撞上有损档现在就拦，别等 core。
        let format = opt_str(args, "format")?
            .map(|raw| {
                StillFormat::parse(raw).ok_or_else(|| {
                    crate::i18n::tr_fmt(
                        "未知容器 {}；可用 png8 / tiff16 / jpeg / webp / avif",
                        &[&raw],
                    )
                })
            })
            .transpose()?;
        let quality = opt_u32(args, "quality")?;
        if let Some(q) = quality
            && !(1..=100).contains(&q)
        {
            return Err("quality 要在 1..100".to_owned());
        }
        // copy：这一次导出改用某套副本的调色；省略就是面板上当前这套。
        let copy = match opt_str(args, "copy")? {
            Some(raw) => {
                let name = raw.trim().to_owned();
                if self.copy_index(&name).is_none() {
                    let have: Vec<&str> = self.copies.iter().map(|c| c.name.as_str()).collect();
                    let have_text = if have.is_empty() {
                        crate::i18n::tr("（还没有副本）").to_owned()
                    } else {
                        have.join(crate::i18n::tr("、"))
                    };
                    return Err(crate::i18n::tr_fmt(
                        "没有叫 {} 的副本；现有：{}",
                        &[&name, &have_text],
                    ));
                }
                Some(name)
            }
            None => None,
        };
        let video_out = if video {
            if args.get("out_dir").is_some() || self.render_video_out.as_os_str().is_empty() {
                out_dir.join(format!("{seq_name}_成片.mp4"))
            } else {
                self.render_video_out.clone()
            }
        } else {
            self.render_video_out.clone()
        };
        // 工程文件里的 video_out 可能被手改过；绝不允许它指向源帧。
        if video && sources.iter().any(|source| path_eq(source, &video_out)) {
            return Err(crate::i18n::tr_fmt(
                "拒绝覆盖素材：{}",
                &[&video_out.display()],
            ));
        }
        if video {
            // 缺 ffmpeg 就别先渲染一整段帧；提前失败，消息更直接。
            if export::find_ffmpeg().is_none() {
                return Err("找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned());
            }
        }

        // 校验通过，开始落设置。
        if let Some((width, height)) = size {
            self.output_w = width;
            self.output_h = height;
        }
        self.render_out_dir = out_dir;
        if let Some(space) = color_space {
            self.render_color_space = space;
        }
        if let Some(dyn_range) = dynamic {
            self.render_dynamic = dyn_range;
            self.apply_dynamic();
        }
        // 模式先落，容器才知道自己是不是要当 ffmpeg 的中间帧。
        self.render_mode = if video {
            OutputMode::Video
        } else {
            OutputMode::Stills
        };
        if let Some(format) = format {
            self.set_render_format(format)?;
        }
        if let Some(q) = quality {
            self.render_quality = q as u8;
        }
        if video {
            self.render_video_out = video_out;
            if let Some(fps) = fps {
                self.output_fps = fps as f32;
            }
            if let Some(crf) = crf {
                self.render_crf = crf as u8;
            }
            if let Some(codec) = codec {
                self.codec = codec;
            }
        }

        self.export_copy = copy.clone();
        self.run_export(ctx);
        if !self.export_busy {
            return Err("导出没有启动：检查是否已在导出或无界面模式".to_owned());
        }
        Ok(json!({
            "started": true,
            "mode": mode_name(self.render_mode),
            "format": self.render_format.key(),
            "quality": self.render_quality,
            "color_space": self.render_color_space.key(),
            "dynamic": self.render_dynamic.key(),
            // HDR 只对 AVIF 成立，容器装不下时面板该让人看出「这个组合不成立」。
            "dynamic_fits": self.render_dynamic.fits(self.render_format),
            "copy": copy,
            "frames": n,
            "out_dir": self.render_out_dir.display().to_string(),
            "video_out": if video { Some(self.render_video_out.display().to_string()) } else { None },
            "note": "长任务已启动；用 export.status 查询进度，export.cancel 取消",
        }))
    }

    pub(super) fn export_status_json(&self) -> Value {
        let progress = self.export_progress().map(|(done, total, stage)| {
            json!({
                "done": done,
                "total": total,
                "stage": match stage {
                    Stage::Frames => "frames",
                    Stage::Encode => "encode",
                },
            })
        });
        json!({
            "busy": self.export_busy,
            "stage": self.export_stage.map(|stage| match stage {
                Stage::Frames => "frames",
                Stage::Encode => "encode",
            }),
            "progress": progress,
            "mode": mode_name(self.render_mode),
            "out_dir": self.render_out_dir.display().to_string(),
            "video_out": self.render_video_out.display().to_string(),
            "last_saved": self.last_saved,
            "last_error": self.error,
            "status": self.status,
        })
    }

    pub(super) fn cmd_project_save(&mut self) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let path = project::project_path(&seq.root);
        if self.headless {
            return Err("当前是无界面模式，不写工程文件".to_owned());
        }
        // 直接写一份快照，失败就把原因带回去；不走 quiet 自动保存，
        // 免得被更早的无关错误干扰判断。
        let snapshot = self.project_snapshot();
        project::save(&path, &snapshot)
            .map_err(|e| crate::i18n::tr_fmt("保存工程失败：{}", &[&e]))?;
        self.last_auto_save = Instant::now();
        self.status = crate::i18n::tr_fmt("工程已保存 {}", &[&path.display()]);
        Ok(json!({"path": path.display().to_string(), "saved": true}))
    }

    pub(super) fn cmd_project_get(&self) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let path = project::project_path(&seq.root);
        let snapshot = self.project_snapshot();
        Ok(json!({
            "path": path.display().to_string(),
            "exists": path.is_file(),
            "project": serde_json::to_value(&snapshot).unwrap_or(Value::Null),
        }))
    }

    /// 当前序列所属目录的说明与版权：不带参数是查询，带了就改那几项。
    /// 改完立刻写一次工程文件，MCP 那头不用等 5 秒的自动保存。
    pub(super) fn cmd_project_metadata(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.require_sequence()?.root.clone();
        let changed = apply_metadata_args(&mut self.meta, args)?;
        if changed {
            self.meta = self.meta.cleaned();
            self.write_project(true);
            self.status = if self.meta.is_empty() {
                "已清空说明与版权信息".to_owned()
            } else {
                "已更新说明与版权信息".to_owned()
            };
        }
        let mut out = metadata_json(&self.meta);
        out["path"] = json!(project::project_path(&root).display().to_string());
        out["saved"] = json!(changed);
        Ok(out)
    }

    pub(super) fn cmd_journal_list(&self, args: &Value) -> Result<Value, String> {
        let root = self.require_sequence()?.root.clone();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|v| v as usize)
            .unwrap_or(50)
            .clamp(1, 500);
        let entries = journal::tail(&root, limit);
        Ok(json!({
            "path": journal::path(&root).display().to_string(),
            "count": entries.len(),
            "entries": entries,
        }))
    }
}
