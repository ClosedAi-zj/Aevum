//! 渲染队列：队列的读写、行的增删、整队起停与逐行状态。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! The render queue: Loading and saving the queue, adding and removing rows, running and stopping the whole queue, and per-row status.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ 渲染队列

    /// 启动时从漫游配置读队列；测试用 `AppState::new` 不会碰这份文件。
    ///
    /// Read the queue from the roaming config at startup; `AppState::new` in tests never touches
    /// that file.
    pub fn load_queue(&mut self) {
        self.queue = queue::load(&self.queue_path);
    }

    /// 落盘队列：导出页的批量操作（排序 / 清空已完成）改完必须立刻写回，
    /// 否则下次启动读到的是旧顺序。
    ///
    /// Persist the queue: the export page's batch actions must write back at once, or
    /// the next launch reads the old order.
    pub(crate) fn save_queue(&mut self) {
        if let Err(e) = queue::save(&self.queue_path, &self.queue) {
            self.error = Some(crate::i18n::tr_fmt("队列保存失败：{}", &[&e]));
        }
    }

    pub fn queue_json(&self) -> Value {
        let items: Vec<Value> = self
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
        json!({
            "path": self.queue_path.display().to_string(),
            "count": self.queue.len(),
            "items": items,
        })
    }

    pub fn queue_status_json(&self) -> Value {
        let done = self
            .queue
            .iter()
            .filter(|i| i.status == QueueStatus::Done)
            .count();
        let failed = self
            .queue
            .iter()
            .filter(|i| i.status == QueueStatus::Failed)
            .count();
        let progress = if self.queue_busy {
            Some(json!({
                "frame_done": self.queue_done.load(Ordering::Relaxed),
                "frame_total": self.queue_total.load(Ordering::Relaxed),
                "stage": if self.queue_stage.load(Ordering::Relaxed) == 1 { "encode" } else { "frames" },
            }))
        } else {
            None
        };
        json!({
            "busy": self.queue_busy,
            "current": self.queue_current.map(|i| i + 1),
            "items": self.queue.len(),
            "done": done,
            "failed": failed,
            "pending": self.queue.len() - done - failed,
            "progress": progress,
            "path": self.queue_path.display().to_string(),
        })
    }

    pub fn queue_progress(&self) -> Option<(usize, usize, Stage)> {
        if !self.queue_busy {
            return None;
        }
        let stage = if self.queue_stage.load(Ordering::Relaxed) == 1 {
            Stage::Encode
        } else {
            Stage::Frames
        };
        Some((
            self.queue_done.load(Ordering::Relaxed),
            self.queue_total.load(Ordering::Relaxed),
            stage,
        ))
    }

    /// 把当前序列与成片设置排进队列；同一目录重复加入等于更新规格。
    ///
    /// Queue the current sequence and its render settings; adding the same folder again updates
    /// its spec.
    pub fn add_current_to_queue(&mut self) -> Result<usize, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消或等它跑完".to_owned());
        }
        let (root, name) = {
            let seq = self.require_sequence()?;
            (seq.root.clone(), seq.display_name())
        };
        let out = if self.render_video_out.as_os_str().is_empty() {
            root.join(format!("{name}_成片.mp4"))
        } else {
            self.render_video_out.clone()
        };
        let item = QueueItem {
            width: self.output_w,
            height: self.output_h,
            fps: self.output_fps as f64,
            codec: if self.codec.contains("264") {
                "h264"
            } else {
                "h265"
            }
            .to_owned(),
            crf: self.render_crf,
            preset: self.render_preset.clone(),
            keep_frames: self.render_keep_frames,
            resume: self.render_resume,
            // 加入那一刻的工程快照：排队后再改调色不影响这一段
            // （界面承诺"每段读取加入时的工程参数"）。之前渲染时读磁盘，
            // 自动保存可能已把加入之后的调色写进去，渲出来的成片会缺
            // 最后几次调色（审查确认）。
            project: Some(self.project_snapshot()),
            ..QueueItem::new(&root, out)
        };
        let index = queue::add(&mut self.queue, item)?;
        self.save_queue();
        self.status = crate::i18n::tr_fmt("已加入队列：第 {} 段", &[&(index + 1)]);
        Ok(index)
    }

    /// 图库右键「加入渲染队列」：任意序列按当前输出设置入队。当前序列用
    /// 内存快照，别的序列读它磁盘上的工程，读不到就不带（与 MCP 同口径）。
    ///
    /// Library context-menu "add to render queue": any sequence joins with the current
    /// output settings. The current sequence snapshots memory; others read their on-disk
    /// project, or none when absent (same rule as the MCP command).
    pub fn add_root_to_queue(&mut self, root: &std::path::Path) -> Result<usize, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消或等它跑完".to_owned());
        }
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "序列".to_owned());
        let out = root.join(format!("{name}_成片.mp4"));
        let is_current = self.seq.as_ref().is_some_and(|s| s.root == root);
        let project = if is_current {
            Some(self.project_snapshot())
        } else {
            project::load(&project::project_path(root)).ok()
        };
        let item = QueueItem {
            width: self.output_w,
            height: self.output_h,
            fps: self.output_fps as f64,
            codec: if self.codec.contains("264") {
                "h264"
            } else {
                "h265"
            }
            .to_owned(),
            crf: self.render_crf,
            preset: self.render_preset.clone(),
            keep_frames: self.render_keep_frames,
            resume: self.render_resume,
            project,
            ..QueueItem::new(root, out)
        };
        let index = queue::add(&mut self.queue, item)?;
        self.save_queue();
        self.status = crate::i18n::tr_fmt("已加入队列：第 {} 段", &[&(index + 1)]);
        Ok(index)
    }

    /// 开始逐段渲染；立即返回，进度看 `queue.status`。
    ///
    /// Start rendering segment by segment; returns immediately, watch `queue.status` for
    /// progress.
    pub fn start_queue(&mut self, ctx: &Context) -> Result<usize, String> {
        if self.headless {
            return Err("当前是无界面模式，不能启动队列".to_owned());
        }
        if self.queue_busy {
            return Err("队列已在进行中；用 queue.status 查询进度".to_owned());
        }
        if self.export_busy {
            return Err("正在导出当前序列，等导出结束再跑队列".to_owned());
        }
        if self.queue.is_empty() {
            return Err("队列是空的：先把当前序列加入队列".to_owned());
        }
        let ffmpeg = export::find_ffmpeg()
            .ok_or_else(|| "找不到 ffmpeg（设 AEVUM_FFMPEG 或加进 PATH）".to_owned())?;
        let n = self.queue.len();
        for item in &mut self.queue {
            item.status = QueueStatus::Pending;
            item.note.clear();
        }
        self.save_queue();
        let items = self.queue.clone();

        self.queue_busy = true;
        self.queue_current = Some(0);
        // 每次开跑都把逐段耗时清零，并把起点钉在现在：表格的「耗时 / 预计剩余」
        // 读的就是这两个。
        //
        // Reset per-segment timings on every run and pin the start to now: the table's
        // elapsed / ETA columns read exactly these two.
        self.queue_secs = vec![0.0; n];
        self.queue_row_start = Some(std::time::Instant::now());
        self.queue_selected = None;
        self.cancel_queue.store(false, Ordering::SeqCst);
        self.queue_done.store(0, Ordering::Relaxed);
        self.queue_total.store(0, Ordering::Relaxed);
        self.queue_stage.store(0, Ordering::Relaxed);
        self.status = crate::i18n::tr_fmt("队列渲染 0/{}", &[&n]);

        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let cancel = self.cancel_queue.clone();
        let done = self.queue_done.clone();
        let total = self.queue_total.clone();
        let stage = self.queue_stage.clone();
        thread::spawn(move || {
            let (mut ok, mut failed) = (0usize, 0usize);
            let mut cancelled = false;
            let progress = |p: JobProgress| {
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
            for (index, item) in items.iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    cancelled = true;
                    break;
                }
                done.store(0, Ordering::Relaxed);
                total.store(0, Ordering::Relaxed);
                stage.store(0, Ordering::Relaxed);
                match queue::render_item(item, &ffmpeg, &cancel, &progress) {
                    Ok(path) => {
                        ok += 1;
                        let _ = tx.send(Msg::QueueRow {
                            index,
                            status: QueueStatus::Done,
                            note: path.display().to_string(),
                        });
                    }
                    Err(e) => {
                        if cancel.load(Ordering::SeqCst) {
                            cancelled = true;
                            let _ = tx.send(Msg::QueueRow {
                                index,
                                status: QueueStatus::Pending,
                                note: "已取消".to_owned(),
                            });
                            break;
                        }
                        failed += 1;
                        let _ = tx.send(Msg::QueueRow {
                            index,
                            status: QueueStatus::Failed,
                            note: crate::i18n::err_text(&e),
                        });
                    }
                }
                ctx2.request_repaint();
            }
            let _ = tx.send(Msg::QueueFinished {
                ok,
                failed,
                cancelled,
            });
            ctx2.request_repaint();
        });
        Ok(n)
    }

    pub fn stop_queue(&mut self) {
        if self.queue_busy {
            self.cancel_queue.store(true, Ordering::SeqCst);
            self.status = crate::i18n::t!("正在取消队列…").to_owned();
        }
    }

    /// 面板按钮用：按 0 基序号移除一段。
    ///
    /// For the panel buttons: remove one segment by 0-based index.
    pub fn remove_queue_at(&mut self, index: usize) -> bool {
        if self.queue_busy || index >= self.queue.len() {
            return false;
        }
        self.queue.remove(index);
        // 耗时表与队列等长：删一段就要删同一位，否则后面每段的耗时会串位。
        // Keep the timing list the same length as the queue: dropping a segment must
        // drop its slot, or every later segment shows the wrong elapsed time.
        if index < self.queue_secs.len() {
            self.queue_secs.remove(index);
        }
        self.queue_selected = None;
        self.save_queue();
        true
    }

    pub fn clear_queue_rows(&mut self) -> usize {
        if self.queue_busy {
            return 0;
        }
        let cleared = self.queue.len();
        self.queue.clear();
        self.queue_secs.clear();
        self.queue_selected = None;
        self.save_queue();
        cleared
    }

    pub(super) fn cmd_queue_add(&mut self, args: &Value) -> Result<Value, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消或等它跑完".to_owned());
        }
        let root = match opt_str(args, "root")? {
            Some(text) => PathBuf::from(text),
            None => self.require_sequence()?.root.clone(),
        };
        let current = self.seq.as_ref().map(|s| s.root.clone());
        let default_out = match &current {
            Some(cur) if path_eq(&root, cur) && !self.render_video_out.as_os_str().is_empty() => {
                self.render_video_out.clone()
            }
            _ => {
                let name = root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "序列".to_owned());
                root.join(format!("{name}_成片.mp4"))
            }
        };
        let out = match opt_str(args, "out")? {
            Some(text) => PathBuf::from(text),
            None => default_out,
        };
        let width = opt_u32(args, "width")?.unwrap_or(self.output_w);
        let height = opt_u32(args, "height")?.unwrap_or(self.output_h);
        if (width == 0) != (height == 0) {
            return Err("width 与 height 需同时为 0（保持原生）或同时给出".to_owned());
        }
        if width > 0 && (width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1) {
            return Err(crate::i18n::tr_fmt(
                "width/height 必须是偶数且不小于 2：{}×{}",
                &[&width, &height],
            ));
        }
        let fps = opt_f64(args, "fps")?.unwrap_or(self.output_fps as f64);
        if !(1.0..=240.0).contains(&fps) {
            return Err("fps 必须在 1..=240".to_owned());
        }
        let crf = opt_u32(args, "crf")?.unwrap_or(self.render_crf as u32);
        if crf > 51 {
            return Err("crf 必须在 0..51".to_owned());
        }
        let codec = match opt_str(args, "codec")? {
            Some(codec) => match codec.trim().to_ascii_lowercase().as_str() {
                "h264" | "264" | "avc" => "h264",
                "h265" | "265" | "hevc" => "h265",
                other => {
                    return Err(crate::i18n::tr_fmt(
                        "未知编码 {}；可用 h264 / h265",
                        &[&other],
                    ));
                }
            },
            None if self.codec.contains("264") => "h264",
            None => "h265",
        };
        let preset = match opt_str(args, "preset")? {
            Some(preset) => {
                let preset = preset.trim();
                if !queue::PRESETS.contains(&preset) {
                    return Err(crate::i18n::tr_fmt("未知编码预设 {}", &[&preset]));
                }
                preset.to_owned()
            }
            None => self.render_preset.clone(),
        };
        let keep_frames = match args.get("keep_frames") {
            None | Some(Value::Null) => self.render_keep_frames,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "keep_frames 必须是布尔值".to_owned())?,
        };
        let resume = match args.get("resume") {
            None | Some(Value::Null) => self.render_resume,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| "resume 必须是布尔值".to_owned())?,
        };
        // 副本名只校验非空：该序列工程里有没有这套副本，渲染那一段时才晓得，
        // 队列允许先排进还没调完色的序列。
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
        // 快照"此刻"的工程：指定的序列就是当前序列时用内存参数（控制通道
        // 这头可能刚改完参数还没到自动保存的 5 秒）；指定的别的序列只能读
        // 它磁盘上的工程，读不到就不带快照，渲染照旧回落磁盘。
        let project = match &current {
            Some(cur) if path_eq(&root, cur) => Some(self.project_snapshot()),
            _ => project::load(&project::project_path(&root)).ok(),
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
            project,
            ..QueueItem::default()
        };
        let index = queue::add(&mut self.queue, item)?;
        self.save_queue();
        self.status = crate::i18n::tr_fmt("已加入队列：第 {} 段", &[&(index + 1)]);
        let mut out = self.queue_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("added".to_owned(), json!(index + 1));
        }
        Ok(out)
    }

    pub(super) fn cmd_queue_remove(&mut self, args: &Value) -> Result<Value, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消再改队列".to_owned());
        }
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少 index（从 1 开始）".to_owned())? as usize;
        if index == 0 || index > self.queue.len() {
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.queue.len()],
            ));
        }
        self.queue.remove(index - 1);
        self.save_queue();
        Ok(json!({"removed": index, "items": self.queue.len()}))
    }

    pub(super) fn cmd_queue_clear(&mut self) -> Result<Value, String> {
        if self.queue_busy {
            return Err("队列正在渲染，先取消再清空".to_owned());
        }
        let cleared = self.queue.len();
        self.queue.clear();
        self.save_queue();
        Ok(json!({"cleared": cleared, "items": 0}))
    }

    pub(super) fn cmd_queue_run(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        if !confirm(args) {
            return Err("queue.run 会批量写视频，需参数 confirm: true".to_owned());
        }
        let n = self.start_queue(ctx)?;
        Ok(json!({
            "started": true,
            "items": n,
            "note": "长任务已启动；用 queue.status 查询进度",
        }))
    }
}
