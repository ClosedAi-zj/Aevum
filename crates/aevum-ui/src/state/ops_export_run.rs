//! 导出与对齐的启停与进度：对齐与导出的后台任务起停、进度与剩余时间、跳过标记。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Starting, stopping and tracking export and alignment: Starting and stopping the alignment and export workers, their progress and ETA, and the skip flag.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    pub fn run_align(&mut self, ctx: &Context) {
        if self.align_busy || self.headless {
            return;
        }
        let Some(seq) = &self.seq else { return };
        if seq.is_empty() {
            return;
        }
        let paths = seq.paths();
        let options = align::AlignOptions {
            reference: self.align_ref.min(seq.len() - 1),
            estimator: self.align_estimator,
            ..align::AlignOptions::default()
        };
        self.align_busy = true;
        self.align_msg = None;
        self.align_done.store(0, Ordering::Relaxed);
        self.status = crate::i18n::tr_fmt("正在对齐 {} 帧…", &[&paths.len()]);

        let token = self.reload_token;
        let done = self.align_done.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let progress = |n: usize, _total: usize| {
                done.store(n, Ordering::Relaxed);
            };
            match align::measure_with_progress(&paths, &options, &progress) {
                Ok(report) => {
                    let _ = tx.send(Msg::Aligned { token, report });
                }
                Err(e) => {
                    let _ = tx.send(Msg::AlignFailed(crate::i18n::err_text(&e)));
                }
            }
            ctx2.request_repaint();
        });
    }

    pub fn run_export(&mut self, ctx: &Context) {
        if self.export_busy || self.headless {
            return;
        }
        let Some(seq) = &self.seq else { return };
        let paths = seq.paths();
        if paths.is_empty() {
            return;
        }
        let n = paths.len();
        let mode = self.render_mode;
        let out_root = if self.render_out_dir.as_os_str().is_empty() {
            seq.root.join("成片")
        } else {
            self.render_out_dir.clone()
        };
        // 视频导出的中间帧单独放一层，收尾时只清理这一层里我们自己写的文件。
        let out_dir = if mode == OutputMode::Video {
            out_root.join("_序列帧")
        } else {
            out_root.clone()
        };
        let video_out = if self.render_video_out.as_os_str().is_empty() {
            out_root.join("成片.mp4")
        } else {
            self.render_video_out.clone()
        };
        // 命令通道指定过副本时，这一次导出用那套副本的调色；取走即清，
        // 界面上再点导出回到当前调色，不会有人忘了它还挂着。
        let grade = self.export_copy.take().and_then(|name| {
            self.copies
                .iter()
                .find(|c| c.name == name)
                .map(|c| (c.params.clone(), c.keyframes.clone()))
        });
        let (base_params, base_keys) = match &grade {
            Some((params, keys)) => (params, keys),
            None => (&self.params, &self.keyframes),
        };
        let frame_params: Vec<GradeParams> = (0..n)
            .map(|i| interp::params_at(base_params, base_keys, i))
            .collect();
        let gains: Vec<Ev> = (0..n).map(|i| self.ev_at(i)).collect();
        let mut offsets = vec![[0.0, 0.0]; n];
        if let Some(report) = &self.align_report {
            for (i, off) in report.offsets.iter().take(n).enumerate() {
                offsets[i] = *off;
            }
        }
        // 导出取景和画布上那个框是同一份几何：对齐安全框 ∘ 用户框 ∘ 画幅比例。
        let crop = self.effective_crop();
        let options = RenderOptions {
            mode,
            format: self.render_format,
            quality: self.render_quality,
            keep_exif: self.render_keep_exif,
            xmp_sidecar: self.render_xmp_sidecar,
            metadata: self.meta.clone(),
            sharpen_for: self.render_sharpen_for,
            sharpen_strength: self.render_sharpen_strength,
            out_dir,
            video_out,
            crop,
            rotate: self.rotate,
            long_side: None,
            // 成片面板里的「4K / 1080p」是精确画幅：cover 放大后居中裁切，
            // 不再只限长边（裁切后的 1278×718 也要铺满 3840×2160）。
            target_size: if self.output_w == 0 || self.output_h == 0 {
                None
            } else {
                Some((self.output_w, self.output_h))
            },
            fps: self.output_fps.max(1.0) as f64,
            codec: if self.codec.contains("264") {
                Codec::H264
            } else {
                Codec::H265
            },
            crf: self.render_crf,
            preset: self.render_preset.clone(),
            audio: self.render_audio.clone(),
            audio_opts: self.render_audio_opts.clone(),
            keep_frames: self.render_keep_frames,
            resume: self.render_resume,
            color_space: self.render_color_space,
            dynamic: self.render_dynamic,
            watermark: self.render_watermark.clone(),
            // 运镜跟着这一次导出走：面板上定好的取景框，成片里就该照着走。
            motion: self.motion,
        };

        // 变速计划跟这一次导出绑定：线程起来之后就算面板上又动了倍速，
        // 跑的还是启动那一刻算出来的帧序，进度与产物都不会自相矛盾。
        let plan = match export::plan_for(&self.retime, n, self.output_fps.max(1.0) as f64) {
            Ok(plan) => plan,
            Err(e) => {
                self.status = crate::i18n::tr_fmt("变速参数不对：{}", &[&e]);
                return;
            }
        };

        self.cancel_export.store(false, Ordering::SeqCst);
        self.export_busy = true;
        self.export_stage = Some(Stage::Frames);
        self.export_done.store(0, Ordering::Relaxed);
        let out_frames = plan.as_ref().map_or(n, |p| p.frames());
        self.export_total.store(out_frames, Ordering::Relaxed);
        self.export_stage_shared.store(0, Ordering::Relaxed);
        // 「跳过 N」是数出来的，而数帧与帧数成正比——这份活不放界面线程，
        // 交给下面那条导出线程，结果写进原子量由状态栏的 100ms 心跳去读。
        self.export_skipped_shared.store(0, Ordering::Relaxed);
        self.status = if self.render_resume {
            crate::i18n::t!("正在导出…（续跑：先数已导出的帧）").to_owned()
        } else {
            crate::i18n::t!("正在导出…").to_owned()
        };

        let token = self.reload_token;
        let cancel = self.cancel_export.clone();
        let done = self.export_done.clone();
        let skipped = self.export_skipped_shared.clone();
        let total = self.export_total.clone();
        let stage = self.export_stage_shared.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            // 续跑先数一遍已经写完的帧，好把「跳过 N」显示出来：只报「已导出」
            // 看不出这一趟几乎没干活，续跑和重跑得区分得开。没开续跑时它是 0。
            skipped.store(
                export::existing_frames(&options, out_frames),
                Ordering::Relaxed,
            );
            let progress = move |p: JobProgress| {
                done.store(p.done, Ordering::Relaxed);
                total.store(p.total, Ordering::Relaxed);
                stage.store(
                    match p.stage {
                        Stage::Frames => 0,
                        Stage::Encode => 1,
                    },
                    Ordering::Relaxed,
                );
            };
            let result = (|| -> Result<PathBuf, String> {
                if options.mode == OutputMode::Video {
                    let ffmpeg = export::find_ffmpeg()
                        .ok_or_else(|| "找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned())?;
                    let video = export::export_video_planned(
                        &ffmpeg,
                        &paths,
                        &frame_params,
                        &gains,
                        &offsets,
                        &options,
                        plan.as_ref(),
                        &cancel,
                        &progress,
                    )
                    .map_err(|e| crate::i18n::err_text(&e))?;
                    if !options.keep_frames {
                        cleanup_frames(&options.out_dir, options.format);
                    }
                    Ok(video)
                } else {
                    export::export_stills_planned(
                        &paths,
                        &frame_params,
                        &gains,
                        &offsets,
                        &options,
                        plan.as_ref(),
                        &cancel,
                        &progress,
                    )
                    .map_err(|e| crate::i18n::err_text(&e))?;
                    Ok(options.out_dir.clone())
                }
            })();
            match result {
                Ok(path) => {
                    let _ = tx.send(Msg::Exported { token, path });
                }
                Err(e) => {
                    let _ = tx.send(Msg::ExportFailed(e));
                }
            }
            ctx2.request_repaint();
        });
    }

    pub fn stop_export(&mut self) {
        if self.export_busy {
            self.cancel_export.store(true, Ordering::SeqCst);
            self.status = crate::i18n::t!("正在取消导出…").to_owned();
        }
    }

    /// 状态栏「约剩 Xs」：按最近 8 秒的推进速度估。
    ///
    /// 只吃「当前显示的任务」的采样：任务换了、或者进度回退（新任务从 0 起），就把
    /// 窗口清空重开，免得上一轮的速率借给下一轮。同一进度值重复画帧不记样本，否则
    /// 界面帧率会被误当成任务的推进速度；`done` 为 0 或已经到终点时直接不给数字。
    ///
    /// ETA for the status bar, from the last eight seconds of progress. Only the task
    /// currently on screen contributes samples: switching tasks, or a progress reset,
    /// starts a fresh window so one run's rate never leaks into the next. Repeated frames
    /// at the same progress value are not sampled -- otherwise the UI frame rate would be
    /// mistaken for the task's throughput. No number at `done == 0` or at completion.
    pub fn eta_secs(&mut self, task: &str, done: usize, total: usize) -> Option<u64> {
        if done == 0 || total == 0 || done >= total {
            self.eta = EtaWindow::default();
            return None;
        }
        let now = Instant::now();
        if self.eta.key != task || done < self.eta.last_done {
            self.eta = EtaWindow {
                key: task.to_owned(),
                ..EtaWindow::default()
            };
        }
        self.eta.last_done = done;
        if self.eta.samples.last().is_none_or(|(_, d)| *d != done) {
            self.eta.samples.push((now, done));
        }
        // 只留最近 8 秒、最多 16 个点；至少留一个，否则永远算不出速率。
        while self.eta.samples.len() > 16
            || (self.eta.samples.len() > 1
                && now.duration_since(self.eta.samples[0].0) > Duration::from_secs(8))
        {
            self.eta.samples.remove(0);
        }
        let (t0, d0) = *self.eta.samples.first()?;
        let secs = now.duration_since(t0).as_secs_f32();
        if secs <= 0.0 || done <= d0 {
            return None;
        }
        let rate = (done - d0) as f32 / secs;
        if rate <= 0.0 {
            return None;
        }
        Some(((total - done) as f32 / rate).ceil() as u64)
    }

    pub fn export_progress(&self) -> Option<(usize, usize, Stage)> {
        if !self.export_busy {
            return None;
        }
        let stage = if self.export_stage_shared.load(Ordering::Relaxed) == 1 {
            Stage::Encode
        } else {
            Stage::Frames
        };
        Some((
            self.export_done.load(Ordering::Relaxed),
            self.export_total.load(Ordering::Relaxed),
            stage,
        ))
    }

    /// 这一趟续跑跳过了几帧：没开续跑、或者还没数完，都是 0。
    ///
    /// How many frames this run skipped through resume: 0 when resume is off or the count is not
    /// in yet.
    pub fn export_skipped(&self) -> usize {
        self.export_skipped_shared.load(Ordering::Relaxed)
    }

    /// 只有测试能摆这个数：真机上它只由导出线程写，界面这边没有别的入口。
    #[cfg(test)]
    pub(crate) fn set_export_skipped_for_test(&self, n: usize) {
        self.export_skipped_shared.store(n, Ordering::Relaxed);
    }

    pub fn align_progress(&self) -> Option<(usize, usize)> {
        if !self.align_busy {
            return None;
        }
        Some((self.align_done.load(Ordering::Relaxed), self.frame_count()))
    }
}
