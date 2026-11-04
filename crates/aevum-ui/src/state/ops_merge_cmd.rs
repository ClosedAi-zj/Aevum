//! 合并、质检与评分命令：HDR/全景/景深/堆栈/时混合五种合并的建立与进度、序列质检、批量评分，以及评分参数的写入。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Merge, QA and grading commands: Building and tracking the five merge kinds (HDR, panorama, focus, stack, time blend), sequence QA, batch rating, and writes to the grading parameters.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    /// HDR 合并：同场景多张不同曝光融成一张静帧（Mertens 曝光融合）。
    /// 帧源二选一：`frames` 显式帧号数组，或 `center` ± `around`（默认当前帧 ±1）。
    /// 对齐跑过就先套统一裁切 + 逐帧位移，与导出同一条几何路。
    ///
    /// 长任务：只校验参数、抓一份输入快照，然后起后台线程并立刻返回。
    /// 9 张 6K 帧融一遍要 6 秒，放在界面线程上就是一段死窗口。
    pub(super) fn cmd_hdr_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let n = seq.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
            ));
        }
        if !confirm(args) {
            return Err("hdr.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let indices: Vec<usize> = match args.get("frames") {
            Some(list) => list
                .as_array()
                .ok_or_else(|| "frames 要是帧号数组，例如 [0,1,2]".to_owned())?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|i| ((i as usize) < n).then_some(i as usize))
                        .ok_or_else(|| {
                            crate::i18n::tr_fmt(
                                "帧号越界：{}（序列共 {} 帧）",
                                &[&aevum_core::errmsg::clip_echo(&v.to_string()), &n],
                            )
                        })
                })
                .collect::<Result<_, _>>()?,
            None => {
                let around = opt_u32(args, "around")?.unwrap_or(1) as usize;
                let center = opt_u32(args, "center")?
                    .map(|v| v as usize)
                    .unwrap_or(self.selected.min(n - 1));
                if center >= n {
                    return Err(crate::i18n::tr_fmt(
                        "center 越界：{}（序列共 {} 帧）",
                        &[&center, &n],
                    ));
                }
                (center.saturating_sub(around)..=center.saturating_add(around).min(n - 1)).collect()
            }
        };
        let kind = MergeKind::Hdr;
        if indices.len() < 2 {
            return Err("HDR 合并至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "HDR 合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑关键几帧",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let root = seq.root.clone();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;

        // HDR / 全景不做合成前自动对齐：HDR 是同场景多曝光，全景几何交给拼接。
        //
        // HDR / panorama never run pre-fusion alignment: HDR is one scene at several
        // exposures, and panorama geometry belongs to the stitcher.
        let job = self.build_merge_job(kind, indices, &out_dir, MergeSpec::Plain, false)?;
        Ok(self.start_merge(ctx, job))
    }

    /// 合并的落盘目录：`out_dir` > 成片面板的输出目录 > 序列根下的默认子目录。
    fn merge_out_dir(&self, args: &Value, root: &Path, kind: MergeKind) -> Result<PathBuf, String> {
        match opt_str(args, "out_dir")? {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                if dir.as_os_str().is_empty() {
                    return Err("out_dir 不能为空；省略则用当前输出目录".to_owned());
                }
                Ok(dir)
            }
            None if self.render_out_dir.as_os_str().is_empty() => Ok(root.join(kind.dir_name())),
            None => Ok(self.render_out_dir.clone()),
        }
    }

    /// 抓一份合并任务的输入：帧路径、逐帧几何、成片路径与输出旋钮。
    /// 后台线程拿不到 `AppState`，所以该读的都在这里读完。
    fn build_merge_job(
        &mut self,
        kind: MergeKind,
        indices: Vec<usize>,
        out_dir: &Path,
        spec: MergeSpec,
        align: bool,
    ) -> Result<MergeJob, String> {
        // 调用方大多先过 require_sequence，但 MCP 通道可能绕过：缺序列就
        // 报错返回，而不是在后台线程里 panic（panic 只会静默吞掉任务）。
        let seq = self
            .seq
            .as_ref()
            .ok_or_else(|| "合并任务需要先打开一个序列".to_owned())?;
        let paths = seq.paths();
        let srcs: Vec<PathBuf> = indices.iter().map(|i| paths[*i].clone()).collect();
        let framing: Vec<(Option<CropRect>, [f64; 2])> = match kind {
            // 全景靠特征点自己求单应，几何全交给拼接，不套对齐位移；
            // 其余三种都是同机位叠帧，对齐位移与统一裁切必须套上，否则星点会拖成双线。
            MergeKind::Pano => indices.iter().map(|_| (None, [0.0, 0.0])).collect(),
            // 开了合成前自动对齐的焦点合成走另一套几何（拍板 ⑥「不叠加」）：手动
            // 序列对齐的位移一律不套，只保留它的裁切框；位移完全由本次估计给出。
            // 估计就在这个已裁切的坐标系里做。
            //
            // A focus merge with pre-fusion auto alignment uses a different geometry
            // (ruling 6, "no stacking"): manual sequence-alignment offsets are never
            // applied, only its crop rectangle survives; the offsets come entirely from
            // this run's estimate, which therefore works in the already-cropped frame.
            MergeKind::Focus if align => indices
                .iter()
                .map(|i| (self.alignment_at(*i).0, [0.0, 0.0]))
                .collect(),
            _ => indices.iter().map(|i| self.alignment_at(*i)).collect(),
        };
        let options = self.frame_render_options();
        let dst = out_dir.join(format!(
            "{}_{:06}_{}f.{}",
            kind.tag(),
            indices[0],
            indices.len(),
            options.format.ext()
        ));
        Ok(MergeJob {
            kind,
            indices,
            srcs,
            framing,
            dst,
            options,
            spec,
            align,
        })
    }

    /// 起后台合并任务并立刻返回一份「已启动」的 JSON；结果与进度走 `merge.status`
    /// 与面板回显（`Msg::Merged` 落账）。
    fn start_merge(&mut self, ctx: &Context, job: MergeJob) -> Value {
        let kind = job.kind;
        let frames = job.indices.len();
        let indices = job.indices.clone();
        let out = job.dst.clone();
        self.merge_busy = true;
        self.merge_kind = Some(kind);
        self.merge_result = None;
        self.merge_error = None;
        match kind {
            MergeKind::Hdr => {
                self.hdr_last = None;
                self.hdr_error = None;
            }
            MergeKind::Pano => {
                self.pano_last = None;
                self.pano_error = None;
            }
            MergeKind::Stack => {
                self.stack_last = None;
                self.stack_error = None;
            }
            MergeKind::TimeBlend => {
                self.tb_last = None;
                self.tb_error = None;
            }
            MergeKind::Focus => {
                self.focus_last = None;
                self.focus_error = None;
            }
        }
        self.merge_job += 1;
        let run = self.merge_job;
        // 新任务新开一个取消旗：上一轮的旗（可能已经立着）与这一轮无关，绝不能拿它
        // 当共同开关——旧线程还在跑时把它清零，等于让旧任务继续写盘。
        //
        // A fresh cancel flag per run: the previous one (possibly already raised) must not be
        // shared, or clearing it while the old thread is still alive revives that run.
        self.cancel_merge = Arc::new(AtomicBool::new(false));
        self.merge_done.store(0, Ordering::Relaxed);
        // 进度 = 读好的帧数 + 最后那一段（融合或拼接再写盘）。
        self.merge_total.store(frames + 1, Ordering::Relaxed);
        self.status = crate::i18n::tr_fmt("正在{}…", &[&crate::i18n::tr(kind.cn())]);

        let token = self.reload_token;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let done = self.merge_done.clone();
        let total = self.merge_total.clone();
        let cancel = self.cancel_merge.clone();
        thread::spawn(move || {
            let result = run_merge_job(&job, &done, &total, &cancel);
            let _ = tx.send(Msg::Merged {
                token,
                job: run,
                kind,
                result,
            });
            ctx2.request_repaint();
        });
        json!({
            "started": true,
            "kind": kind.tag(),
            "frames": frames,
            "indices": indices,
            "out": out.display().to_string(),
            "note": "长任务已启动；用 merge.status 查询进度与成片路径，merge.cancel 取消",
        })
    }

    /// `merge.status` 的 JSON：忙碌标记、进度计数、最近一次结果或失败原因。
    pub(super) fn merge_status_json(&self) -> Value {
        json!({
            "busy": self.merge_busy,
            "kind": self.merge_kind.map(MergeKind::tag),
            "done": self.merge_done.load(Ordering::Relaxed),
            "total": self.merge_total.load(Ordering::Relaxed),
            "result": self.merge_result,
            "error": self.merge_error,
        })
    }

    /// 请求取消正在进行的合并；核心算法是一整块，取消只在两帧之间生效。
    ///
    /// Request cancellation of the merge in progress; the core algorithm is one block, so
    /// cancellation only takes effect between two frames.
    pub fn stop_merge(&mut self) {
        if self.merge_busy {
            self.cancel_merge.store(true, Ordering::SeqCst);
            if let Some(kind) = self.merge_kind {
                self.status =
                    crate::i18n::tr_fmt("正在取消{}…（帧间生效）", &[&crate::i18n::tr(kind.cn())]);
            }
        }
    }

    /// 面板画进度用的三元组：种类 + 已完成 + 总数。没在跑就 `None`。
    ///
    /// The triple the panel draws progress from: kind + done + total. `None` when nothing is
    /// running.
    pub fn merge_progress(&self) -> Option<(MergeKind, usize, usize)> {
        if !self.merge_busy {
            return None;
        }
        Some((
            self.merge_kind.unwrap_or(MergeKind::Hdr),
            self.merge_done.load(Ordering::Relaxed),
            self.merge_total.load(Ordering::Relaxed),
        ))
    }

    /// 序列体检：`sequence.check` 起后台判定，立刻返回「已启动」。
    ///
    /// 测光随导入那趟已经算好（`self.metrics`），这里只逐帧 `stat` 一遍文件大小
    /// 再跑纯数组判定——几千帧也就几十毫秒，但仍在后台跑：界面线程不该为任何
    /// 与帧数成正比的活停下来。
    pub(super) fn cmd_sequence_check(
        &mut self,
        ctx: &Context,
        args: &Value,
    ) -> Result<Value, String> {
        let (n, paths) = {
            let seq = self.require_sequence()?;
            (seq.len(), seq.paths())
        };
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.qa_busy {
            return Err("体检正在跑；用 sequence.status 查询进度，sequence.cancel 取消".to_owned());
        }
        let sig = match args.get("sig") {
            Some(v) => {
                let s = v
                    .as_f64()
                    .ok_or_else(|| "sig 要是一个数，例如 3".to_owned())?;
                if !s.is_finite() || !(0.5..=20.0).contains(&s) {
                    return Err("sig 得在 0.5~20 之间：越小越灵敏，越大越宽松".to_owned());
                }
                s
            }
            None => self.qa_sig,
        };
        self.qa_sig = sig;
        let opts = qa::Options {
            sig,
            blur_sig: sig,
            ..qa::Options::default()
        };
        // 测光表可能比帧表短（还在导），缺的那几帧按「读不出来」判，正好是体检要报的。
        let frames: Vec<(PathBuf, Option<FrameMetrics>)> = paths
            .into_iter()
            .enumerate()
            .map(|(i, p)| (p, self.metrics.get(i).copied().flatten()))
            .collect();

        self.qa_busy = true;
        self.qa_error = None;
        self.cancel_qa.store(false, Ordering::SeqCst);
        self.qa_done.store(0, Ordering::Relaxed);
        self.qa_total.store(n + 1, Ordering::Relaxed);
        self.status = crate::i18n::t!("正在体检…").to_owned();

        let token = self.reload_token;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let done = self.qa_done.clone();
        let cancel = self.cancel_qa.clone();
        thread::spawn(move || {
            let result = run_qa_job(&frames, &opts, &done, &cancel);
            let _ = tx.send(Msg::Checked { token, result });
            ctx2.request_repaint();
        });
        Ok(json!({
            "started": true,
            "frames": n,
            "sig": sig,
            "note": "长任务已启动；用 sequence.status 查询进度与清单，sequence.cancel 取消",
        }))
    }

    /// `sequence.status` 的 JSON：忙碌标记、进度、最近一次清单与分类计数。
    pub(super) fn qa_status_json(&self) -> Value {
        json!({
            "busy": self.qa_busy,
            "done": self.qa_done.load(Ordering::Relaxed),
            "total": self.qa_total.load(Ordering::Relaxed),
            "ran": self.qa_run,
            "sig": self.qa_sig,
            "frames": self.frame_count(),
            "counts": qa::count_by_kind(&self.qa_findings),
            "findings": qa::findings_json(&self.qa_findings),
            "error": self.qa_error,
        })
    }

    /// 请求取消正在跑的体检；`stat` 是按批停的，最后一批跑完才真正退出。
    ///
    /// Request cancellation of the running health check; `stat` stops per batch, so it really
    /// exits only after the last batch finishes.
    pub fn stop_qa(&mut self) {
        if self.qa_busy {
            self.cancel_qa.store(true, Ordering::SeqCst);
            self.status = crate::i18n::t!("正在取消体检…（批间生效）").to_owned();
        }
    }

    /// 面板画进度用的二元组：已完成 / 总数。没在跑就 `None`。
    ///
    /// The pair the panel draws progress from: done / total. `None` when nothing is
    /// running.
    pub fn qa_progress(&self) -> Option<(usize, usize)> {
        if !self.qa_busy {
            return None;
        }
        Some((
            self.qa_done.load(Ordering::Relaxed),
            self.qa_total.load(Ordering::Relaxed),
        ))
    }

    /// 面板上的「开始体检」：与 `sequence.check` 同一条路径，阈值用面板当前值。
    ///
    /// The panel's "start health check": the same path as `sequence.check`, using the
    /// panel's current threshold.
    pub fn run_qa(&mut self, ctx: &Context) {
        if self.qa_busy || self.headless {
            return;
        }
        let _ = self.cmd_sequence_check(ctx, &Value::Null);
    }

    /// 全景拼接：相邻几张有重叠的照片接成一张宽幅（特征点 + RANSAC + 羽化）。
    /// 与 HDR 同一套长任务通道：校验、抓输入、起后台线程，立刻返回。
    pub(super) fn cmd_pano_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let n = seq.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
            ));
        }
        if !confirm(args) {
            return Err("pano.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let indices: Vec<usize> = match args.get("frames") {
            Some(list) => list
                .as_array()
                .ok_or_else(|| "frames 要是帧号数组，例如 [0,1,2]".to_owned())?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|i| ((i as usize) < n).then_some(i as usize))
                        .ok_or_else(|| {
                            crate::i18n::tr_fmt(
                                "帧号越界：{}（序列共 {} 帧）",
                                &[&aevum_core::errmsg::clip_echo(&v.to_string()), &n],
                            )
                        })
                })
                .collect::<Result<_, _>>()?,
            None => {
                let around = opt_u32(args, "around")?.unwrap_or(1) as usize;
                let start = opt_u32(args, "start")?
                    .map(|v| v as usize)
                    .unwrap_or(self.selected.min(n - 1));
                if start >= n {
                    return Err(crate::i18n::tr_fmt(
                        "start 越界：{}（序列共 {} 帧）",
                        &[&start, &n],
                    ));
                }
                (start..=start.saturating_add(around).min(n - 1)).collect()
            }
        };
        let kind = MergeKind::Pano;
        if indices.len() < 2 {
            return Err("全景合并至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "全景合并一次最多 {} 张，实得 {} 张：把 around 调小，或用 frames 只挑相邻几张",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let root = seq.root.clone();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;

        // HDR / 全景不做合成前自动对齐：HDR 是同场景多曝光，全景几何交给拼接。
        //
        // HDR / panorama never run pre-fusion alignment: HDR is one scene at several
        // exposures, and panorama geometry belongs to the stitcher.
        let job = self.build_merge_job(kind, indices, &out_dir, MergeSpec::Plain, false)?;
        Ok(self.start_merge(ctx, job))
    }

    /// 把 `frames`（帧号数组）或锚点 ± `around` 解析成一串合法帧号。
    ///
    /// Resolve either a `frames` array or an anchor plus an `around` radius into valid indices.
    fn pick_indices(
        &self,
        args: &Value,
        n: usize,
        anchor: IndicesAnchor,
        default_around: u32,
    ) -> Result<Vec<usize>, String> {
        if let Some(list) = args.get("frames") {
            return list
                .as_array()
                .ok_or_else(|| "frames 要是帧号数组，例如 [0,1,2]".to_owned())?
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|i| ((i as usize) < n).then_some(i as usize))
                        .ok_or_else(|| {
                            crate::i18n::tr_fmt(
                                "帧号越界：{}（序列共 {} 帧）",
                                &[&aevum_core::errmsg::clip_echo(&v.to_string()), &n],
                            )
                        })
                })
                .collect();
        }
        let around = opt_u32(args, "around")?.unwrap_or(default_around) as usize;
        let (anchor_value, name) = match anchor {
            IndicesAnchor::Center => (opt_u32(args, "center")?, "center"),
            IndicesAnchor::Start => (opt_u32(args, "start")?, "start"),
        };
        let at = anchor_value
            .map(|v| v as usize)
            .unwrap_or(self.selected.min(n - 1));
        if at >= n {
            return Err(crate::i18n::tr_fmt(
                "{} 越界：{}（序列共 {} 帧）",
                &[&name, &at, &n],
            ));
        }
        let from = match anchor {
            IndicesAnchor::Center => at.saturating_sub(around),
            IndicesAnchor::Start => at,
        };
        Ok((from..=at.saturating_add(around).min(n - 1)).collect())
    }

    /// `cull.evaluate`：按四条判据逐帧裁决，可选直接把判废的帧标成「排除」。
    ///
    /// `cull.evaluate`: judge every frame with the four rules, optionally marking the rejected
    /// ones as excluded right away.
    pub(super) fn cmd_cull_evaluate(&mut self, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        // 判据吃的是导入那趟已经算好的逐帧测光，不再解图：这条命令应当立刻返回。
        //
        // The rules live on the per-frame measurements taken during import, so nothing is
        // decoded here and the call returns at once.
        let means: Vec<Option<f64>> = self.metrics.iter().map(|m| m.map(|x| x.mean)).collect();
        let sharp: Vec<Option<f64>> = self.metrics.iter().map(|m| m.map(|x| x.sharp)).collect();
        let mut rule = aevum_core::cull::CullRule {
            ev_low: self.cull_ev_low,
            ev_high: self.cull_ev_high,
            jump_ev: self.cull_jump,
            sharp_ratio: self.cull_sharp,
        };
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
            return Err(crate::i18n::tr_fmt(
                "曝光下限 {} 不该高于上限 {}",
                &[&rule.ev_low, &rule.ev_high],
            ));
        }
        let verdict = aevum_core::cull::evaluate(&means, &sharp, &rule);
        let frames: Vec<Value> = verdict
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
            .collect();
        let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(false);
        let rejected = verdict.rejected_indices();
        if apply {
            self.flag_many(&rejected, -1);
        }
        self.cull_ev_low = rule.ev_low;
        self.cull_ev_high = rule.ev_high;
        self.cull_jump = rule.jump_ev;
        self.cull_sharp = rule.sharp_ratio;
        self.cull_note = Some(if rejected.is_empty() {
            crate::i18n::tr("这一遍没有帧越界").to_owned()
        } else {
            crate::i18n::tr_fmt("{} 帧会被判废", &[&rejected.len()])
        });
        self.status = self.cull_note.clone().unwrap_or_default();
        Ok(json!({
            "band": [rule.ev_low, rule.ev_high],
            "rule": {
                "ev_low": rule.ev_low,
                "ev_high": rule.ev_high,
                "jump_ev": rule.jump_ev,
                "sharp_ratio": rule.sharp_ratio,
            },
            "rejected": rejected.len(),
            "applied": if apply { rejected.len() } else { 0 },
            "indices": rejected,
            "jumps": verdict
                .jumps
                .iter()
                .map(|(i, step)| json!({"index": i, "step_ev": step}))
                .collect::<Vec<Value>>(),
            "frames": frames,
        }))
    }

    /// `cull.extract`：把没被拒收的帧复制成一条新序列（剔除成子序列）。
    ///
    /// `cull.extract`: copy every frame that was not rejected into a new sequence.
    pub(super) fn cmd_cull_extract(&mut self, args: &Value) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        if seq.is_empty() {
            return Err("序列里没有帧".to_owned());
        }
        let target = args
            .get("target")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| "target 要是目标目录的绝对路径".to_owned())?;
        let target = std::path::PathBuf::from(target);
        let root = seq.root.clone();
        let dry_run = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !dry_run && !confirm(args) {
            return Err(
                "cull.extract 是批量写操作，需参数 confirm: true（只看体积用 dry_run: true）"
                    .to_owned(),
            );
        }
        // 路径安全先行：源内部、盘根、非空目录都在写任何东西之前拒绝。
        //
        // Path safety first: inside the source, a drive root and a non-empty folder are all
        // refused before anything is written.
        aevum_core::cull::check_target(&root, &target)
            .map_err(|e| crate::i18n::tr(e.cn()).to_owned())?;
        let paths = seq.paths();
        let kept: Vec<PathBuf> = (0..paths.len())
            .filter(|i| self.flag_at(*i) != -1)
            .map(|i| paths[i].clone())
            .collect();
        if kept.is_empty() {
            return Err("每一帧都被判废了：没有可保留的帧".to_owned());
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
            aevum_core::cull::ExtractError::Target(t) => crate::i18n::tr(t.cn()).to_owned(),
            aevum_core::cull::ExtractError::Io(e) => crate::i18n::tr_fmt("复制失败：{}", &[&e]),
        })?;
        let note = crate::i18n::tr_fmt(
            "已导出 {} 帧到 {}",
            &[&kept.len(), &target.display().to_string()],
        );
        history::note("cull.extract", note.clone());
        self.status = note.clone();
        // 开发参数不跟过去（它们属于源序列的工程）；测光缓存已按 mtime 带走，
        // 新序列打开时直接命中，不用重测。
        //
        // Develop settings do not follow (they belong to the source project); the metering
        // cache was carried with mtimes, so the new sequence opens without a re-measure.
        Ok(json!({
            "copied": kept.len(),
            "excluded": paths.len() - kept.len(),
            "bytes": bytes,
            "target": target.display().to_string(),
            "note": note,
        }))
    }

    /// `stack.merge`：把一段同机位帧按统计量合成一张（星轨 / 降噪 / 去卫星 / σ 截尾）。
    ///
    /// `stack.merge`: combine a fixed-camera run of frames into one still using a statistical
    /// mode (star trails / noise reduction / satellite removal / sigma clipping).
    pub(super) fn cmd_stack_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
            ));
        }
        if !confirm(args) {
            return Err("stack.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let kind = MergeKind::Stack;
        let indices = self.pick_indices(args, n, IndicesAnchor::Center, self.stack_around)?;
        if indices.len() < 2 {
            return Err("堆栈至少要 2 帧：给 frames 数组，或把 around 调大".to_owned());
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "堆栈一次最多 {} 帧，实得 {} 帧：把 around 调小，或先分段再合",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let mode = match opt_str(args, "mode")? {
            Some(raw) => aevum_core::stack::Mode::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt(
                    "未知堆栈模式 {}；可用 lighten / average / median / sigma-clip",
                    &[&raw],
                )
            })?,
            None => self.stack_mode,
        };
        let clip_sigma = opt_f64(args, "clip_sigma")?
            .map(|v| v as f32)
            .unwrap_or(self.stack_sigma);
        if !clip_sigma.is_finite() || clip_sigma <= 0.0 {
            return Err("clip_sigma 要大于 0（常用 2–4：3.0 只剔真离群）".to_owned());
        }
        let fill_gaps = args
            .get("fill_gaps")
            .and_then(Value::as_bool)
            .unwrap_or(self.stack_fill);
        let root = self
            .seq
            .as_ref()
            .map(|s| s.root.clone())
            .unwrap_or_default();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;
        let spec = MergeSpec::Stack(aevum_core::stack::Options {
            mode,
            clip_sigma,
            fill_gaps,
        });
        // 堆栈不做合成前自动对齐：同机位叠帧，几何由序列对齐负责。
        //
        // Stacking never runs pre-fusion alignment: fixed-camera frames, geometry belongs
        // to sequence alignment.
        let job = self.build_merge_job(kind, indices, &out_dir, spec, false)?;
        Ok(self.start_merge(ctx, job))
    }

    /// 时间合成预览：面板每帧调用；参数或帧选择变了就在后台重算一张半分辨率小图。
    ///
    /// 派发要同时满足：序列开着、至少两帧、没在合并、没有同参数任务在飞。回包拿
    /// 参数指纹（`tb_preview_key`）对账——迟到的旧参数回包只收解码缓存、不上屏。
    ///
    /// The time-blend preview tick, called by the panel every frame; a changed parameter
    /// or frame selection re-runs a half-resolution composite in the background. Dispatch
    /// requires an open sequence with at least two frames, no running merge and no
    /// in-flight job for the same parameters. Replies reconcile against the parameter
    /// fingerprint (`tb_preview_key`): stale replies only refresh the decode cache.
    pub fn timeblend_preview_tick(&mut self, ctx: &Context) {
        if self.merge_busy || self.tb_preview_inflight {
            return;
        }
        let Some(seq) = self.seq.as_ref() else {
            return;
        };
        let n = seq.len();
        if n < 2 {
            return;
        }
        // 帧选择与 timeblend.merge 的 Start 锚点一致：当前帧起 tb_around 帧。
        let at = self.selected.min(n - 1);
        let indices: Vec<usize> =
            (at..=at.saturating_add(self.tb_around as usize).min(n - 1)).collect();
        if indices.len() < 2 {
            return;
        }
        let layout = self.tb_layout;
        let mut bands = self.tb_bands as usize;
        if layout == aevum_core::timeblend::Layout::Bands {
            bands = bands.clamp(1, indices.len());
        }
        let spec = aevum_core::timeblend::Spec {
            layout,
            bands,
            feather: self.tb_feather.clamp(0.0, 0.5),
            horizon: self.tb_horizon.clamp(0.0, 1.0),
            horizon_auto: self.tb_horizon_auto,
            deghost: self.tb_deghost,
            match_exposure: self.tb_match,
        };
        // 指纹：帧选择（序列根 + 帧号）先散一遍归解码缓存，合成参数叠在其上做对账。
        let root = seq.root.display().to_string();
        let mut hf = fnv1a(0xcbf2_9ce4_8422_2325, root.as_bytes());
        for i in &indices {
            hf = fnv1a(hf, &(*i as u64).to_le_bytes());
        }
        let key_frames = hf;
        let mut h = key_frames;
        h = fnv1a(h, layout.key().as_bytes());
        h = fnv1a(h, &(bands as u64).to_le_bytes());
        h = fnv1a(h, &spec.feather.to_bits().to_le_bytes());
        h = fnv1a(h, &spec.horizon.to_bits().to_le_bytes());
        h = fnv1a(h, &spec.deghost.to_bits().to_le_bytes());
        h = fnv1a(
            h,
            &[u8::from(spec.horizon_auto), u8::from(spec.match_exposure)],
        );
        let key = h;
        let paths = seq.paths();
        let srcs: Vec<PathBuf> = indices.iter().map(|i| paths[*i].clone()).collect();
        let framing: Vec<(Option<CropRect>, [f64; 2])> =
            indices.iter().map(|i| self.alignment_at(*i)).collect();
        let token = self.reload_token;
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let cached = self.tb_preview_data.clone();
        if self.tb_preview_key == key {
            return; // 已上屏（或已失败）的同一组参数：不重发，失败也不打转
        }
        self.tb_preview_key = key;
        self.tb_preview_inflight = true;
        self.tb_preview_error = None;
        thread::spawn(move || {
            match run_timeblend_preview(cached, srcs, indices, framing, spec, key_frames) {
                Ok((data, lin, w, h)) => {
                    let _ = tx.send(Msg::TimeblendPreview {
                        token,
                        key,
                        image: Box::new(tb_preview_image(&lin, w, h)),
                        frames: Some(data),
                    });
                }
                Err(e) => {
                    let _ = tx.send(Msg::TimeblendPreviewFail {
                        token,
                        key,
                        error: e,
                    });
                }
            }
            ctx2.request_repaint();
        });
    }

    /// `timeblend.merge`：按带或按地平线把不同时刻的帧合成为一张。
    ///
    /// `timeblend.merge`: composite frames from different moments, either as horizontal bands or
    /// split at the horizon.
    pub(super) fn cmd_timeblend_merge(
        &mut self,
        ctx: &Context,
        args: &Value,
    ) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
            ));
        }
        if !confirm(args) {
            return Err("timeblend.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let kind = MergeKind::TimeBlend;
        let indices = self.pick_indices(args, n, IndicesAnchor::Start, self.tb_around)?;
        if indices.len() < 2 {
            return Err(
                "时间合成至少要 2 帧：给 frames 数组，或把 around 调大（帧号升序 = 时间顺序）"
                    .to_owned(),
            );
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "时间合成一次最多 {} 帧，实得 {} 帧：把 around 调小",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let layout = match opt_str(args, "layout")? {
            Some(raw) => aevum_core::timeblend::Layout::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt("未知布局 {}；可用 sky-ground / bands", &[&raw])
            })?,
            None => self.tb_layout,
        };
        let bands = opt_u32(args, "bands")?.unwrap_or(self.tb_bands) as usize;
        // bands 只对时间切片有意义：天地合成不用它，不该被面板的默认值卡住。
        //
        // bands only matters for the time slice: sky-ground ignores it and must not be blocked
        // by the panel's default.
        if layout == aevum_core::timeblend::Layout::Bands && (bands == 0 || bands > indices.len()) {
            return Err(crate::i18n::tr_fmt(
                "bands 要在 1..={} 之间（等于帧数时一带一帧）",
                &[&indices.len()],
            ));
        }
        let feather = opt_f64(args, "feather")?
            .map(|v| v as f32)
            .unwrap_or(self.tb_feather)
            .clamp(0.0, 0.5);
        let horizon_auto = args
            .get("horizon_auto")
            .and_then(Value::as_bool)
            .unwrap_or(self.tb_horizon_auto);
        let horizon = opt_f64(args, "horizon")?
            .map(|v| v as f32)
            .unwrap_or(self.tb_horizon)
            .clamp(0.0, 1.0);
        let deghost = opt_f64(args, "deghost")?
            .map(|v| v as f32)
            .unwrap_or(self.tb_deghost);
        if !deghost.is_finite() || deghost < 0.0 || deghost > 4.0 {
            return Err("deghost 要在 0..=4 之间（EV；0 = 关闭去鬼影）".to_owned());
        }
        let match_exposure = args
            .get("match_exposure")
            .and_then(Value::as_bool)
            .unwrap_or(self.tb_match);
        let root = self
            .seq
            .as_ref()
            .map(|s| s.root.clone())
            .unwrap_or_default();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;
        let spec = MergeSpec::TimeBlend(aevum_core::timeblend::Spec {
            layout,
            bands,
            feather,
            horizon,
            horizon_auto,
            deghost,
            match_exposure,
        });
        // 时间合成不做合成前自动对齐：天地/星轨的位移是内容，不能被"对齐掉"。
        //
        // Time blending never runs pre-fusion alignment: motion is the content here, and
        // aligning it away would destroy the very thing the user is composing.
        let job = self.build_merge_job(kind, indices, &out_dir, spec, false)?;
        Ok(self.start_merge(ctx, job))
    }

    /// `focus.merge`：把一组不同对焦距离的帧融成一张全景深。
    ///
    /// `focus.merge`: fuse a bracket taken at different focus distances into one all-in-focus
    /// image.
    pub(super) fn cmd_focus_merge(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        if n == 0 {
            return Err("序列里没有帧".to_owned());
        }
        if self.merge_busy {
            return Err(crate::i18n::tr_fmt(
                "已有一项{}在进行中；用 merge.status 查询进度，merge.cancel 取消",
                &[&self.merge_kind.map(MergeKind::cn).unwrap_or("合并")],
            ));
        }
        if !confirm(args) {
            return Err("focus.merge 是批量写操作，需参数 confirm: true".to_owned());
        }
        let kind = MergeKind::Focus;
        let indices = self.pick_indices(args, n, IndicesAnchor::Start, self.focus_around)?;
        if indices.len() < 2 {
            return Err(
                "焦点合成至少要 2 帧：给 frames 数组，或把 around 调大（从起始帧往后铺）"
                    .to_owned(),
            );
        }
        if indices.len() > kind.max_frames() {
            return Err(crate::i18n::tr_fmt(
                "焦点合成一次最多 {} 帧，实得 {} 帧：把 around 调小或分段合成",
                &[&kind.max_frames(), &indices.len()],
            ));
        }
        let method = match opt_str(args, "method")? {
            Some(raw) => aevum_core::focus::Method::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt(
                    "未知融合方式 {}；可用 depthmap / weighted / pyramid",
                    &[&raw],
                )
            })?,
            None => self.focus_method,
        };
        let radius = opt_u32(args, "radius")?.unwrap_or(self.focus_radius) as usize;
        if radius > 32 {
            return Err("radius 要在 0..=32 之间（px；0 = 逐像素原始响应）".to_owned());
        }
        let threshold = opt_f64(args, "threshold")?
            .map(|v| v as f32)
            .unwrap_or(self.focus_threshold);
        if !threshold.is_finite() || !(0.0..=100.0).contains(&threshold) {
            return Err("threshold 要在 0..=100 之间（百分位；0 = 关闭无细节兜底）".to_owned());
        }
        let smooth_z = opt_u32(args, "smooth_z")?.unwrap_or(self.focus_smooth_z) as usize;
        if smooth_z > 16 {
            return Err("smooth_z 要在 0..=16 之间（px）".to_owned());
        }
        let feather = opt_f64(args, "feather")?
            .map(|v| v as f32)
            .unwrap_or(self.focus_feather);
        if !feather.is_finite() || !(0.0..=64.0).contains(&feather) {
            return Err("feather 要在 0..=64 之间（px；小于 0.5 视为硬切）".to_owned());
        }
        // 合成前自动对齐：缺省关；给了但不是布尔值就当场报错，别把 "true" 字符串
        // 静默当成假（那会让脚本以为开了对齐，实际没开）。
        //
        // Pre-fusion auto alignment: off by default; a non-boolean value errors out
        // immediately instead of being read as false, which would let a script believe
        // alignment ran when it did not.
        let align = match args.get("align") {
            None | Some(Value::Null) => false,
            Some(value) => value.as_bool().ok_or_else(|| {
                crate::i18n::tr_fmt("{} 要布尔值（true / false）", &[&"align"]).to_owned()
            })?,
        };
        let root = self
            .seq
            .as_ref()
            .map(|s| s.root.clone())
            .unwrap_or_default();
        let out_dir = self.merge_out_dir(args, &root, kind)?;
        ensure_out_dir_safe(&root, &out_dir)?;
        std::fs::create_dir_all(&out_dir).map_err(|e| {
            crate::i18n::tr_fmt(
                "建输出目录失败：{}",
                &[&aevum_core::errmsg::clip_echo(&e.to_string())],
            )
        })?;
        let spec = MergeSpec::Focus(aevum_core::focus::Options {
            method,
            radius,
            threshold,
            smooth_z,
            feather,
            // 界面不暴露 levels，0 = auto（见 focus::Options）。
            // The UI does not expose levels; 0 = auto (see focus::Options).
            levels: 0,
            // 界面同样不暴露测度缓存上界，用默认 8（见 focus::Options::measure_cache）。
            // The UI does not expose the measure-cache bound either; default 8
            // (see focus::Options::measure_cache).
            measure_cache: 8,
        });
        let job = self.build_merge_job(kind, indices, &out_dir, spec, align)?;
        let mut started = self.start_merge(ctx, job);
        // 这次到底套没套对齐位移，调用方（面板与脚本）都该直接读到，而不是回头去查
        // 对齐报告再猜。
        //
        // Whether this run applied the alignment offsets belongs in the reply, so both
        // the panel and a script can read it instead of consulting the align report.
        let aligned = self.align_report.is_some();
        if let Some(object) = started.as_object_mut() {
            object.insert("aligned".to_owned(), serde_json::Value::Bool(aligned));
            // 自动对齐请求也回一句：`aligned` 说的是**手动**序列对齐，两者不是一个东西。
            //
            // Also echo the auto-alignment request: `aligned` talks about **manual**
            // sequence alignment, which is a different thing.
            object.insert("align".to_owned(), serde_json::Value::Bool(align));
        }
        Ok(started)
    }

    /// 深度图预览：按下「预览深度图」时前台算一张降采样的归属图。
    ///
    /// 取帧与滑杆参数和 `focus.merge` 同一口径（当前帧起向后 `focus_around` 帧）；
    /// 帧先按长边 [`FOCUS_PREVIEW_LONG_EDGE`] 降采样再进 `focus::depth_map`，预览图
    /// 天然就小，前台同步算也不卡帧——全分辨率是合成任务自己的事，走后台线程。
    /// 不写任何文件：成片仍由「合成」按钮走命令通道。
    ///
    /// Depth-map preview: pressing "preview depth map" computes a downsampled ownership
    /// map on the spot. The frame selection and slider parameters follow the same
    /// convention as `focus.merge` (from the current frame, `focus_around` frames
    /// forward); frames are downsampled to the [`FOCUS_PREVIEW_LONG_EDGE`] long edge
    /// before entering `focus::depth_map`, so the preview bitmap is small by
    /// construction and the synchronous foreground computation stays responsive -- full
    /// resolution is the merge job's business, on the background thread. No file is
    /// written: the final still still goes through the "merge" button and the command
    /// channel.
    pub fn focus_depth_preview(&mut self, ctx: &Context) {
        if self.merge_busy {
            return;
        }
        let n = match self.seq.as_ref() {
            Some(seq) => seq.len(),
            None => return,
        };
        if n < 2 {
            return;
        }
        let indices = match self.pick_indices(
            &serde_json::json!({}),
            n,
            IndicesAnchor::Start,
            self.focus_around,
        ) {
            Ok(v) => v,
            Err(_) => return,
        };
        if indices.len() < 2 {
            self.focus_depth_tex = None;
            self.focus_depth_note =
                Some(crate::i18n::t!("预览至少要 2 帧：把取帧范围调大").to_owned());
            return;
        }
        let started = std::time::Instant::now();
        // 与 run_merge_job 同一条几何路：有对齐结果就套统一裁切与逐帧位移，
        // 否则预览选区和成片对不上，用户会以为深度图选错了帧。
        //
        // Same geometry path as run_merge_job: apply the alignment crop and per-frame
        // offsets when available, otherwise the preview selection disagrees with the
        // final still and reads as "the depth map picked the wrong frame".
        let paths = self.seq.as_ref().expect("seq checked above").paths();
        let mut buffers: Vec<Vec<f32>> = Vec::with_capacity(indices.len());
        let mut dims: Option<(usize, usize)> = None;
        for i in &indices {
            let (crop, off) = self.alignment_at(*i);
            let (rgb, w, h) = match export::load_rgb_f32(&paths[*i]) {
                Ok(v) => v,
                Err(e) => {
                    self.focus_depth_tex = None;
                    self.focus_depth_note = Some(crate::i18n::tr_fmt("预览读帧失败：{}", &[&e]));
                    return;
                }
            };
            let rect = crop.map(|c| (c.x as usize, c.y as usize, c.w as usize, c.h as usize));
            let framing = Framing {
                angle_deg: 0.0,
                off,
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
                    self.focus_depth_tex = None;
                    self.focus_depth_note = Some(crate::i18n::tr_fmt(
                        "预览跳过：第 {} 帧与第 {} 帧尺寸不一致（{}×{} vs {}×{}）",
                        &[&indices[0], i, &prev.0, &prev.1, &got.0, &got.1],
                    ));
                    return;
                }
                Some(_) => {}
            }
            buffers.push(buf);
        }
        let (w, h) = dims.unwrap_or((0, 0));
        // 同尺寸帧降一次就够了：所有帧同一 (w, h)，降完仍然同尺寸。
        //
        // Downsampling once suffices: every frame shares one (w, h) and stays shared
        // after the downscale.
        let (w, h) = {
            let first = downscale_long_edge(
                std::mem::take(&mut buffers[0]),
                w,
                h,
                FOCUS_PREVIEW_LONG_EDGE,
            );
            let (nw, nh) = (first.1, first.2);
            buffers[0] = first.0;
            for b in buffers.iter_mut().skip(1) {
                let (small, sw, sh) =
                    downscale_long_edge(std::mem::take(b), w, h, FOCUS_PREVIEW_LONG_EDGE);
                debug_assert_eq!((sw, sh), (nw, nh), "同尺寸帧降采样后尺寸不同");
                *b = small;
            }
            (nw, nh)
        };
        let opts = aevum_core::focus::Options {
            method: self.focus_method,
            radius: self.focus_radius as usize,
            threshold: self.focus_threshold,
            smooth_z: self.focus_smooth_z as usize,
            feather: self.focus_feather,
            // 深度图预览不需要塔，0 = auto。
            // The depth-map preview needs no tower; 0 = auto.
            levels: 0,
            // 深度图不读金字塔缓存，用默认 8（见 focus::Options::measure_cache）。
            // The depth map never reads the pyramid cache; default 8
            // (see focus::Options::measure_cache).
            measure_cache: 8,
        };
        let refs: Vec<&[f32]> = buffers.iter().map(|b| b.as_slice()).collect();
        let owner = match aevum_core::focus::depth_map(&refs, w, h, &opts) {
            Ok(v) => v,
            Err(e) => {
                self.focus_depth_tex = None;
                self.focus_depth_note = Some(crate::i18n::tr_fmt(
                    "预览失败：{}",
                    &[&crate::i18n::tr(aevum_core::focus::error_text(e))],
                ));
                return;
            }
        };
        let count = refs.len();
        // 参考帧口径与 focus.rs 一致：中位帧（count / 2）。
        //
        // The reference frame follows focus.rs: the middle frame (count / 2).
        let reference = (count / 2) as u16;
        let mut pixels: Vec<u8> = Vec::with_capacity(owner.len() * 3);
        for &o in &owner {
            let [r, g, b] = depth_preview_color(o, count, reference);
            pixels.extend_from_slice(&[r, g, b]);
        }
        let img = ColorImage::from_rgb([w, h], &pixels);
        // 纹理按名字复用：同参数只重传像素，不重新在 GPU 上分配（与时间合成预览同路）。
        match &mut self.focus_depth_tex {
            Some(tex) => tex.set(img, TextureOptions::LINEAR),
            None => {
                self.focus_depth_tex =
                    Some(ctx.load_texture("focus.depth", img, TextureOptions::LINEAR));
            }
        }
        // 耗时写进说明行：预览是前台同步算的，用户有权知道自己等了多久。
        let ms = started.elapsed().as_millis().to_string();
        self.focus_depth_note = Some(crate::i18n::tr_fmt(
            "预览 {}×{} · {} 帧 · {} ms · 第 {} 帧为参考帧",
            &[&w, &h, &count, &ms, &indices[reference as usize]],
        ));
    }

    pub(super) fn cmd_grade_set(&mut self, args: &Value) -> Result<Value, String> {
        // 允许 {"params": {...}}，也允许字段直接放顶层。
        let partial = match args.get("params") {
            Some(value) => value,
            None => args,
        };
        let object = partial
            .as_object()
            .ok_or_else(|| "params 必须是对象".to_owned())?;
        if object.is_empty() {
            return Err("没有要设置的调色字段（例如 {\"exposure\":0.5}）".to_owned());
        }
        let mut current = serde_json::to_value(&self.params)
            .map_err(|e| crate::i18n::tr_fmt("序列化参数失败：{}", &[&e]))?;
        let current_object = current
            .as_object_mut()
            .ok_or_else(|| "内部错误：GradeParams 不是对象".to_owned())?;
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
            return Err(crate::i18n::tr_fmt(
                "未知调色字段：{}",
                &[&unknown.join("、")],
            ));
        }
        let params: GradeParams = serde_json::from_value(current)
            .map_err(|e| crate::i18n::tr_fmt("参数格式不对：{}", &[&e]))?;
        // 换了外观路径就当场验一遍：渲染热路径读不出来时只会按"没挂"处理，
        // 报错必须在这里发生，否则界面表现为"改了没反应"。
        if object.contains_key("look_cube") && !params.look_cube.is_empty() {
            cube::refresh(&params.look_cube)
                .map_err(|e| crate::i18n::tr_fmt("外观 .cube 读不出来：{}", &[&e]))?;
        }
        self.params = params;
        self.preset = "自定义".to_owned();
        self.auto_note = None;
        self.preview_dirty = true;
        self.status = "调色参数已更新（来自命令通道）".to_owned();
        Ok(self.grade_json())
    }

    pub(super) fn cmd_grade_preset(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        if !PRESET_NAMES.contains(&name) {
            return Err(crate::i18n::tr_fmt(
                "未知预设 {}；可用：{}",
                &[&name, &PRESET_NAMES.join("、")],
            ));
        }
        self.apply_preset(name);
        self.status = crate::i18n::tr_fmt("已套用预设 {}", &[&name]);
        Ok(self.grade_json())
    }
}
