//! 会话、帧与标记：打开/关闭序列、取帧与步进、评分旗标标签、控制文件轮询与通知排空。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Session, frames and marks: Opening and closing sequences, frame access and stepping, rating/flag/label marks, control-file polling and notice draining.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    /// 应用一个工作区：一次把右栏宽、胶片条、密度摆好，并切到对应模块。
    ///
    /// 这是**一次性动作**，不建立持续状态：之后拖宽、开关胶片条都只是"自定义"，
    /// 不需要回头改工作区的选择，也不该弹回去。模块映射放在这里而不是
    /// `layout`：布局层不认识模块，让它认识就等于把两层搅在一起。
    ///
    /// Applies a workspace: dock width, filmstrip and density in one go, plus the module.
    ///
    /// A **one-shot action**, not a mode: dragging the width or toggling the filmstrip
    /// afterwards is simply "custom" and nothing springs back. The module mapping lives here
    /// rather than in `layout` because the layout layer does not know about modules, and
    /// teaching it would tangle the two layers.
    pub fn apply_workspace(&mut self, w: crate::layout::Workspace) {
        self.dock_w = w.dock_w();
        self.layout.film = w.film();
        self.layout.dense = w.dense();
        self.module = match w {
            crate::layout::Workspace::Sequence => Module::Sequence,
            crate::layout::Workspace::Develop => Module::Develop,
            crate::layout::Workspace::Render => Module::Render,
        };
    }
    /// 把「这一帧新出现的错误」抬成弹窗。
    ///
    /// 全仓有四十余处 `state.error = Some(...)`（导入、导出、相机/镜头配置、插件、
    /// 更新器、命令分发……），逐个改成弹窗既易漏又容易回归。这里按内容比对：
    /// `error` 一旦变化（新错误或换了一条），就抬成模态框，关掉前不会被状态栏的
    /// 短摘要盖过去。清空弹窗不动 `error`，状态栏那行仍由用户点掉，两条互不干扰。
    ///
    /// Promote "an error that appeared this frame" to a dialog.
    ///
    /// There are forty-odd `state.error = Some(...)` sites across the crate (import, export,
    /// camera/lens profiles, plugins, updater, command dispatch...). Editing each into a
    /// dialog is both easy to miss and easy to regress, so compare by content instead: as
    /// soon as `error` changes (a new failure, or a different one), it is promoted to a
    /// modal and stays until dismissed. Clearing the dialog leaves `error` alone -- the
    /// status-bar line is still dismissed by the user, and the two never fight.
    pub fn sync_error_dialog(&mut self) {
        if self.error == self.error_seen {
            return;
        }
        self.error_seen = self.error.clone();
        if let Some(msg) = self.error.clone() {
            self.error_modal = Some(msg);
        }
    }
    /// 重算剪切遮罩：从最近一次全量预览的 CPU 像素里挑出高光/阴影裁切的
    /// 像素，生成一张与预览同尺寸的 RGBA 遮罩（红=高光、蓝=阴影、其余透明）。
    ///
    /// 只在全量帧到达（!sketch）后调用；草图帧用旧遮罩顶着——遮罩是示意
    /// 罩层，与草图帧的宽高比一致，拉伸显示对得上，精度无关紧要。
    /// 阈值取 LR 同款口径：r、g、b 全部 ≥250 视为高光裁切，全部 ≤5 视为阴影。
    ///
    /// Recompute the clipping mask: pick the highlight / shadow clipped pixels out of the
    /// most recent full preview's CPU pixels and build an RGBA mask of the same size as the
    /// preview (red = highlights, blue = shadows, everything else transparent).
    ///
    /// Called only after a full frame arrives (!sketch); sketch frames keep the old mask --
    /// the mask is an indicative overlay with the same aspect ratio as the sketch, so
    /// stretching it lines up and precision does not matter.
    /// Thresholds follow Lightroom's: r, g and b all >= 250 counts as highlight clipping,
    /// all <= 5 as shadow.
    pub fn rebuild_clip_mask(&mut self, ctx: &Context, stamp: u64) {
        if !self.clip_highlights && !self.clip_shadows {
            if self.clip_mask.take().is_some() {
                self.clip_mask_stamp = stamp;
            }
            return;
        }
        if self.clip_mask_stamp == stamp && self.clip_mask.is_some() {
            return;
        }
        let Some((rgb, size)) = &self.preview_rgb else {
            return;
        };
        let Some(color) = clip_mask_image(rgb, *size, self.clip_highlights, self.clip_shadows)
        else {
            return;
        };
        match &mut self.clip_mask {
            Some(tex) => tex.set(color, TextureOptions::LINEAR),
            None => {
                self.clip_mask =
                    Some(ctx.load_texture("preview.clip", color, TextureOptions::LINEAR))
            }
        }
        self.clip_mask_stamp = stamp;
    }

    /// J 键循环剪切警告：关 → 高光 → 两开 → 阴影 → 关（LR 的口径）。
    /// 置 stamp 为 MAX 强制下一帧重算。
    ///
    /// The J key cycles the clipping warning: off -> highlights -> both -> shadows -> off
    /// (Lightroom's order).
    /// Sets the stamp to MAX to force a recompute on the next frame.
    pub fn clip_cycle(&mut self) {
        match (self.clip_highlights, self.clip_shadows) {
            (false, false) => self.clip_highlights = true,
            (true, false) => self.clip_shadows = true,
            (true, true) => self.clip_highlights = false,
            (false, true) => {
                self.clip_shadows = false;
            }
        }
        self.clip_mask_stamp = u64::MAX;
    }

    /// 把一个滤镜插件作用到当前预览上（看板 B06：后台任务版）。
    ///
    /// 起一条工作线程：真进程 → 握手 → 按 512 的块往返。每块之间向界面报进度，
    /// 用户随时可以取消。结果由 [`AppState::plugin_poll`] 每帧收：成功换一张纹理
    /// 盖在画布上（并等着下一次全量预览把它冲掉），失败进错误横幅并按 id 记一笔。
    ///
    /// Apply a filter plugin to the current preview (kanban B06: background-job version).
    ///
    /// Starts a worker thread: real process -> handshake -> 512-pixel tile round trips.
    /// Progress is reported to the UI between tiles and the user can cancel at any time.
    /// Results are collected each frame by [`AppState::plugin_poll`]: success swaps in a
    /// texture over the canvas (and waits for the next full preview to wash it away),
    /// failure raises the error banner and records one against the id.
    pub fn plugin_start(&mut self, ctx: &Context, id: &str) -> Result<String, String> {
        use aevum_core::plugin_host::Host;
        // 一次只跑一个：画布上盖着的插件结果只有一个，两个任务抢同一张纹理
        // 谁后到谁赢，用户根本看不懂。
        if self.plugin_job.is_some() {
            return Err("已有插件在运行".to_owned());
        }
        // 先把要用的东西抄出来，免得同时持有 `registry` 的不可变借与自己的可变借。
        let (name, plugin_id, entry, params) = {
            let p = self
                .registry
                .get(id)
                .ok_or_else(|| crate::i18n::tr_fmt("找不到插件：{}", &[&id]))?;
            let params: BTreeMap<String, f64> = p
                .manifest
                .params
                .item
                .iter()
                .map(|s| (s.key.clone(), s.default))
                .collect();
            (
                p.manifest.plugin.name.clone(),
                p.manifest.plugin.id.clone(),
                p.entry_path(),
                params,
            )
        };
        let entry =
            entry.ok_or_else(|| crate::i18n::tr_fmt("{} 由宿主内部实现，还没接上", &[&name]))?;
        // 512 分块只读，整份克隆最省心：别让工作线程借用 self，
        // 也不跟预览缓冲复用那条路抢同一块内存。
        let (rgb, size) = self
            .preview_rgb
            .as_ref()
            .ok_or_else(|| "还没有可用的预览图".to_owned())?
            .clone();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cols = size[0].div_ceil(aevum_core::plugin_host::TILE);
        let rows = size[1].div_ceil(aevum_core::plugin_host::TILE);
        self.plugin_progress = (0, cols * rows);
        self.status = crate::i18n::tr_fmt("{} 运行中…", &[&name]);
        self.plugin_job = Some(PluginJob {
            id: plugin_id.clone(),
            name: name.clone(),
            started: Instant::now(),
            rx,
            cancel: Arc::clone(&cancel),
        });
        thread::spawn(move || {
            match Host::start(&entry, &plugin_id, &params) {
                Ok(mut host) => {
                    // 取消旗桥进进度回调：宿主在每块收发之间问一次"还要不要继续"。
                    let result = host.apply_progress(size[0], size[1], &rgb, |done, total| {
                        let _ = tx.send(PluginMsg::Progress(done, total));
                        !cancel.load(Ordering::Relaxed)
                    });
                    // 无论成败都要收尾：失败时留下的进程会一直占着管道。
                    host.shutdown();
                    let _ = tx.send(PluginMsg::Done(result));
                }
                Err(e) => {
                    let _ = tx.send(PluginMsg::Done(Err(e)));
                }
            }
        });
        // 进度条要动起来：空闲时 egui 不会自己重绘。
        ctx.request_repaint_after(Duration::from_millis(50));
        Ok(name)
    }

    /// 每帧收一次插件任务的进度与结果。挂在 [`AppState::poll`] 里，不用单独调。
    ///
    /// Collect plugin job progress and results once per frame. Called from
    /// [`AppState::poll`]; no separate call needed.
    pub fn plugin_poll(&mut self, ctx: &Context) {
        let Some(job) = self.plugin_job.as_mut() else {
            return;
        };
        let mut finished = None;
        loop {
            match job.rx.try_recv() {
                Ok(msg) => match msg {
                    PluginMsg::Progress(done, total) => {
                        // 只进不退：迟到的旧心跳直接丢。
                        if done >= self.plugin_progress.0 {
                            self.plugin_progress = (done, total);
                        }
                    }
                    PluginMsg::Done(result) => {
                        finished = Some(result);
                        break;
                    }
                },
                // 通道空了但没挂：还在跑，等下一帧。
                Err(TryRecvError::Empty) => break,
                // 发送端没了且队列空：插件线程死透了也没人发 Done——之前
                // 这里直接当"还在跑"，状态栏的进度与取消永远挂着收不了场。
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("插件进程意外退出（没有返回结果）".to_owned()));
                    break;
                }
            }
        }
        let Some(result) = finished else {
            // 还在跑：保一个低频心跳，进度条与取消按钮才动得起来。
            ctx.request_repaint_after(Duration::from_millis(60));
            return;
        };
        let job = self.plugin_job.take().expect("上面刚确认过它在飞");
        self.plugin_progress = (0, 0);
        match result {
            Ok(out) => {
                let image =
                    ColorImage::from_rgb([out.width as usize, out.height as usize], &out.pixels);
                self.plugin_result =
                    Some(ctx.load_texture("plugin.result", image, TextureOptions::LINEAR));
                self.plugin_stamp = self.preview_stamp;
                self.status = format!(
                    "{} · {:.0} ms",
                    job.name,
                    out.elapsed.as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                if e.contains(aevum_core::plugin_host::ERR_CANCELLED) {
                    // 用户取消不是失败：只清状态，不进错误横幅、不计崩溃。
                    self.status = crate::i18n::tr_fmt("{} 已取消", &[&job.name]);
                } else {
                    let crashes = self.plugin_crashes.entry(job.id).or_insert(0);
                    *crashes += 1;
                    let hint = if *crashes >= 3 {
                        "（连续失败多次，可在插件市场里停用它）"
                    } else {
                        ""
                    };
                    self.error = Some(format!("{}：{e}{hint}", job.name));
                }
            }
        }
    }

    /// 用户点了取消：立旗就回。剩下的交给工作线程 —— 它在下一块边界退出、
    /// 把子进程收尸干净，[`AppState::plugin_poll`] 收到回包后清掉在飞状态。
    ///
    /// The user pressed cancel: raise the flag and return. The rest is up to the worker
    /// thread -- it exits at the next tile boundary, reaps the child process cleanly, and
    /// [`AppState::plugin_poll`] clears the in-flight state once the reply arrives.
    pub fn plugin_cancel(&mut self) {
        if let Some(job) = &self.plugin_job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = crate::i18n::tr_fmt("正在取消 {}…", &[&job.name]);
        }
    }

    /// 有没有插件正在后台跑。在飞期间再点别的插件只会得到一句提示。
    ///
    /// Whether a plugin is running in the background. Clicking another plugin while one is
    /// in flight only yields a message.
    pub fn plugin_running(&self) -> bool {
        self.plugin_job.is_some()
    }

    /// 当前偏好的快照（外观 + 布局 + 字号 + 语言 + 折叠态），落盘与回填都用它。
    ///
    /// A snapshot of the current preferences (appearance + layout + font step + language +
    /// folding state), used for both persisting and restoring.
    pub fn settings(&self) -> crate::settings::Settings {
        // 折叠态按登记表次序输出：`HashSet` 的遍历顺序每次进程都不同，原样写文件
        // 会让「什么都没改」也产生 diff，也让 golden/回归比对没法做。排序之后
        // 同一份界面状态永远写出一份字节相同的 JSON。
        let mut collapsed: Vec<String> = GROUP_SPECS
            .iter()
            .filter(|s| self.collapsed.contains(s.key))
            .map(|s| s.key.to_owned())
            .collect();
        // `profile` 是表外组（不在 GROUP_SPECS），但同样是合法的折叠键；
        // 不写出来，用户折上「配置文件」重启就又展开了。
        //
        // `profile` is outside GROUP_SPECS but is a valid collapse key; without
        // this, folding the Profile row would not survive a restart.
        if self.collapsed.contains("profile") {
            collapsed.push("profile".to_owned());
        }
        crate::settings::Settings {
            appearance: self.appearance,
            layout: self.layout,
            font_level: self.font_level,
            lang: self.lang,
            collapsed: Some(collapsed),
            panel_solo: Some(self.panel_solo),
            hdr_preview: Some(self.hdr_preview),
            preset_order: Some(self.preset_order.iter().map(|k| (*k).to_owned()).collect()),
            win_pos: (!self.win_pos.is_empty()).then(|| self.win_pos.clone()),
            // 上次检查更新的时间：静默检查的 24 小时节流就挂在这一项上。
            //
            // The timestamp of the last update check; the silent check's 24-hour
            // throttle hangs off exactly this field.
            last_update_check: self.last_update_check,
            // 面板开关与工作流选择：帧首的布局快照负责置脏，这里只管收账。
            //
            // Panel visibility and the workflow choice: the frame-start layout snapshot
            // marks these dirty, this only collects them.
            show_dock: Some(self.show_dock),
            show_flow: Some(self.show_flow),
            show_left: Some(self.show_left),
            show_rails: Some(self.show_rails),
            show_options: Some(self.show_options),
            flow_task: Some(self.flow_task.key().to_owned()),
            // 合并参数是全局偏好：结果态（*_last / *_error / 预览纹理）不进来。
            merge: crate::settings::MergeParams {
                hdr_around: self.hdr_around,
                stack_around: self.stack_around,
                stack_mode: self.stack_mode.key().to_owned(),
                stack_sigma: self.stack_sigma,
                stack_fill: self.stack_fill,
                tb_around: self.tb_around,
                tb_layout: self.tb_layout.key().to_owned(),
                tb_bands: self.tb_bands,
                tb_feather: self.tb_feather,
                tb_horizon: self.tb_horizon,
                tb_horizon_auto: self.tb_horizon_auto,
                tb_deghost: self.tb_deghost,
                tb_match: self.tb_match,
                focus_around: self.focus_around,
                focus_method: self.focus_method.key().to_owned(),
                focus_radius: self.focus_radius,
                focus_threshold: self.focus_threshold,
                focus_smooth_z: self.focus_smooth_z,
                focus_feather: self.focus_feather,
                focus_align: self.focus_align,
            },
        }
    }

    /// 启动时把落盘的偏好灌回来。不动任何图像状态，只改外观、布局与合并面板参数。
    ///
    /// Pour the persisted preferences back in at startup. Touches no image state, only
    /// appearance, layout and the merge panels' parameters.
    pub fn adopt_settings(&mut self, s: crate::settings::Settings) {
        self.appearance = s.appearance;
        self.layout = s.layout;
        self.font_level = s.font_level.min(3);
        self.lang = s.lang;
        // `None` = 老设置文件里没有这一项（或者从没打开过外观窗）→ 用首屏默认态；
        // `Some(vec![])` = 用户把每一组都展开了，这是明确表态，必须尊重。
        self.collapsed = match s.collapsed {
            Some(list) => list.iter().filter_map(|k| legacy_group_key(k)).collect(),
            None => default_collapsed(),
        };
        // `None` = 老设置文件里没有这一项 → 多开（默认）。单开是 ACR 的选项，
        // 已存盘的明确选择照样尊重。
        //
        // `None` = the setting predates this field -> multi-open (the default). Single-open
        // is an ACR-style option; an explicit saved choice is still honoured.
        self.panel_solo = s.panel_solo.unwrap_or(false);
        self.win_pos = s.win_pos.unwrap_or_default();
        // 上次检查更新的时刻：`None`（老设置文件没有这一项）等于「从没查过」，
        // 于是首启会静默查一次，正是想要的行为。
        //
        // The last update-check stamp: `None` (an older settings file without the field)
        // means "never checked", so the first launch runs one silent check -- exactly what
        // is wanted.
        self.last_update_check = s.last_update_check;
        self.hdr_preview = s.hdr_preview.unwrap_or(false);
        // 面板开关：`None`（老文件）落首启默认；与 `new()` 的初值必须一致，
        // 否则"老用户升级后界面突然多/少一块"。
        //
        // Panel switches: `None` (older file) lands on the fresh-launch defaults, which
        // must match `new()` or an upgrade would silently add/remove a panel.
        self.show_dock = s.show_dock.unwrap_or(true);
        self.show_flow = s.show_flow.unwrap_or(false);
        self.show_left = s.show_left.unwrap_or(false);
        // 工具轨 / 选项栏：`None`（老文件）落回首启默认（都收起），与该文件其它开关
        // 同一口径——老用户升级后界面不会突然多出一根轨。
        //
        // Tool rails / options bar: `None` (older file) lands on the fresh-launch default
        // (both folded), the same rule as the other switches here, so an upgrade never
        // makes a rail appear out of nowhere.
        self.show_rails = s.show_rails.unwrap_or(true);
        self.show_options = s.show_options.unwrap_or(true);
        self.flow_task = s
            .flow_task
            .as_deref()
            .and_then(crate::shell::flow::FlowTask::parse)
            .unwrap_or_default();
        self.layout_switches = (
            self.show_dock,
            self.show_flow,
            self.show_left,
            self.show_rails,
            self.show_options,
            self.flow_task.key(),
        );
        self.preset_order = normalise_preset_order(&s.preset_order.unwrap_or_default());
        // 合并参数是全局偏好：坏 key / 越界值当场夹回默认，不让手改过的
        // settings.json 把首轮合成带进一个不存在的模式。
        //
        // Merge parameters are a global preference: unparsable keys / out-of-range
        // values are clamped back to the defaults here, so a hand-edited settings.json
        // cannot drag the first merge into a mode that does not exist.
        let m = &s.merge;
        self.hdr_around = m.hdr_around.clamp(1, 4);
        self.stack_around = m.stack_around.clamp(1, 32);
        self.stack_mode = aevum_core::stack::Mode::parse(&m.stack_mode)
            .unwrap_or(aevum_core::stack::Mode::Lighten);
        self.stack_sigma = if m.stack_sigma.is_finite() && m.stack_sigma > 0.0 {
            m.stack_sigma.clamp(1.0, 8.0)
        } else {
            3.0
        };
        self.stack_fill = m.stack_fill;
        self.tb_around = m.tb_around.clamp(1, 31);
        self.tb_layout = aevum_core::timeblend::Layout::parse(&m.tb_layout)
            .unwrap_or(aevum_core::timeblend::Layout::SkyGround);
        self.tb_bands = m.tb_bands.clamp(1, 32);
        self.tb_feather = m.tb_feather.clamp(0.0, 0.5);
        self.tb_horizon = m.tb_horizon.clamp(0.0, 1.0);
        self.tb_horizon_auto = m.tb_horizon_auto;
        self.tb_deghost = if m.tb_deghost.is_finite() {
            m.tb_deghost.clamp(0.0, 4.0)
        } else {
            0.5
        };
        self.tb_match = m.tb_match;
        self.focus_around = m.focus_around.clamp(1, 31);
        self.focus_method = aevum_core::focus::Method::parse(&m.focus_method)
            .unwrap_or(aevum_core::focus::Method::DepthMap);
        self.focus_radius = m.focus_radius.min(32);
        self.focus_threshold = if m.focus_threshold.is_finite() {
            m.focus_threshold.clamp(0.0, 100.0)
        } else {
            0.0
        };
        self.focus_smooth_z = m.focus_smooth_z.min(16);
        self.focus_feather = if m.focus_feather.is_finite() {
            m.focus_feather.clamp(0.0, 64.0)
        } else {
            2.0
        };
        // 布尔无需清洗：serde 的 `#[serde(default)]` 已保证缺字段落 `false`。
        //
        // Nothing to sanitise for a boolean: `#[serde(default)]` already lands a missing
        // field on `false`.
        self.focus_align = m.focus_align;
    }

    /// 把某一节拖到第 `to` 位。越界夹住、原地不动就不置脏。
    ///
    /// Move a section to position `to`. Out-of-range values are clamped, and dropping it
    /// where it already is does not set the dirty flag.
    pub fn move_preset_section(&mut self, key: &'static str, to: usize) {
        let Some(from) = self.preset_order.iter().position(|k| *k == key) else {
            return;
        };
        let to = to.min(self.preset_order.len().saturating_sub(1));
        if from == to {
            return;
        }
        let k = self.preset_order.remove(from);
        self.preset_order.insert(to, k);
        self.settings_dirty = true;
    }

    /// 记一个浮窗的新位置，等帧尾统一落盘。
    ///
    /// `pointer_busy` 传 `ctx.input(|i| i.pointer.any_down())`：拖动过程中每帧坐标都在变，逐帧
    /// 落盘等于拿 `settings.json` 当草稿纸。松手那一刻值才稳定，也只在那时记一次。
    /// 若窗口被拖回原位，`get != Some(&v)` 不成立，连脏标记都不会置。
    ///
    /// Record a floating window's new position for the end-of-frame write.
    ///
    /// `pointer_busy` is `ctx.input(|i| i.pointer.any_down())`: while dragging, the coordinates change
    /// every frame and persisting each one turns `settings.json` into scratch paper. Only
    /// when the button is released is the value stable, and only then is it recorded. If the
    /// window was dragged back where it started, `get != Some(&v)` fails and not even the
    /// dirty flag is set.
    pub fn note_win_pos(&mut self, key: &str, pos: egui::Pos2, pointer_busy: bool) {
        if pointer_busy {
            return;
        }
        let v = [pos.x, pos.y];
        if self.win_pos.get(key) != Some(&v) {
            self.win_pos.insert(key.to_owned(), v);
            self.settings_dirty = true;
        }
    }

    pub fn new() -> Self {
        let (tx, rx) = channel();
        let (control_tx, control_rx) = channel();
        let (dialog_tx, dialog_rx) = channel();
        let mut s = Self {
            seq: None,
            metrics: Vec::new(),
            issues: Vec::new(),
            analysis: None,
            corr: Vec::new(),
            corr_rgb: Vec::new(),
            def_means: Vec::new(),
            exif: Vec::new(),
            selected: 0,
            bench: crate::shell::Bench::Develop,
            module: Module::Sequence,
            tool: Tool::Hand,
            dock_tab: DockTab::Adjust,
            only_changed: false,
            show_more_groups: false,
            canvas_fill: false,
            // 固定骨架默认开：选项栏（PS 的工具选项行）+ 左右工具轨。此前两者默认
            // 收起，默认画面只剩「一条菜单行 + 黑底 + 几张浮卡」，读起来像个玩具工具
            // 而不是一套软件（用户 2026-11-11 拿 Photoshop 作对照）。
            //
            // The fixed frame is on by default: the options bar (PS's tool-options row)
            // and the two tool rails. With both folded the default view was one menu row,
            // a black void and a few floating cards -- a gadget, not an application.
            show_options: true,
            show_rails: true,
            slider_drag: false,
            collapsed: default_collapsed(),
            panel_solo: false,
            hdr_preview: false,
            preset_order: default_preset_order(),
            drag_section: None,
            win_pos: std::collections::BTreeMap::new(),
            settings_dirty: false,
            status: "打开一个帧文件夹，或把文件夹拖进窗口".to_owned(),
            error: None,
            notices: Vec::new(),
            sidecar_hint: None,
            measuring: None,
            cancel_measure: Arc::new(AtomicBool::new(false)),
            params: GradeParams::default(),
            grade_baseline: None,
            preset: "原始".to_owned(),
            auto_note: None,
            auto_probe: None,
            auto_probe_inflight: None,
            def_window: 15.0,
            def_strength: 100.0,
            def_multi: false,
            def_channels: false,
            def_align_exif: false,
            aspect: Aspect::Native,
            grid: false,
            cross: false,
            split: false,
            split_pos: 0.5,
            film_h: None,
            zoom: 1.0,
            fit_scale: 1.0,
            pan: Vec2::ZERO,
            space_pending: false,
            playing: false,
            last_play: Instant::now(),
            font_level: 0,
            lang: crate::i18n::LangPref::default(),
            texture: None,
            texture_orig: None,
            preview_size: [0, 0],
            preview_stamp: 0,
            preview_dirty: false,
            preview_inflight: false,
            preview_dragging: false,
            preview_owes_full: false,
            preview_has_orig: false,
            bypassed: BTreeSet::new(),
            thumbs: HashMap::new(),
            thumb_trim_at: None,
            thumb_pending: HashMap::new(),
            thumb_src: HashMap::new(),
            thumb_regrade: HashMap::new(),
            proxies: HashMap::new(),
            proxy_pending: HashMap::new(),
            drag_epoch: 0,
            film_filter_stars: 0,
            film_filter_flag: 0,
            film_filter_label: 0,
            scroll_to_selected: false,
            ratings: HashMap::new(),
            flags: HashMap::new(),
            labels: HashMap::new(),
            keywords: HashMap::new(),
            stacks: Vec::new(),
            meta: project::Metadata::default(),
            lib_roots: Vec::new(),
            lib_sel: None,
            lib_sel_file: None,
            lib_filter: aevum_core::library::Filter::default(),
            lib_checked: std::collections::BTreeSet::new(),
            lib_anchor: None,
            lib_stack_open: HashSet::new(),
            lib_entries: Vec::new(),
            lib_cols: Vec::new(),
            lib_col: None,
            lib_col_name: String::new(),
            lib_keyword: String::new(),
            lib_meta: project::Metadata::default(),
            lib_meta_root: None,
            lib_meta_open: false,
            lib_thumbs: HashMap::new(),
            lib_thumb_pending: HashMap::new(),
            history: History::new(),
            hist: None,
            show_dock: true,
            show_flow: false,
            flow_step: None,
            flow_task: crate::shell::flow::FlowTask::default(),
            layout_switches: (true, false, false, true, true, "timelapse"),
            error_modal: None,
            palette_open: false,
            palette_query: String::new(),
            palette_sel: 0,
            palette_fresh: false,
            async_errors: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            error_seen: None,
            dock_w: crate::theme::DOCK_W,
            // 浮卡几何：`None` / `0.0` 表示「还没拖过，跟随默认位置与内容高度」；
            // 直方图默认 290×92（更宽扁），视图工具条默认显示。
            //
            // Floating-card geometry: `None` / `0.0` mean "not dragged yet, follow the
            // default position and content height"; the histogram starts 290x92 (wider
            // and flatter) and the view toolbar is shown by default.
            dock_pos: None,
            dock_h: 0.0,
            hist_pos: None,
            hist_w: 290.0,
            hist_h: 92.0,
            show_viewtools: true,
            dialog_queue: Default::default(),
            dialog_in_flight: false,
            dialog_tx: dialog_tx.clone(),
            dialog_rx,
            dialog_still: None,
            // 「预设与快照」浮窗默认关：它悬浮在画布上，开屏即开正好压住主图
            // （用户 2026-11-06 的模块截图里它都盖着画布左上）。入口仍在工具轨
            // 图标与「视图」菜单；开关随 settings.json 落盘，首启/重置才用默认值。
            //
            // The Presets & Snapshots window defaults to closed: it floats over the
            // canvas and opened right on top of the image at startup (the user's
            // 2026-11-06 module screenshots show it covering the canvas). Entries stay
            // on the rail icon and the View menu; the toggle is persisted in
            // settings.json, so this default only applies to a fresh/reset state.
            show_left: false,
            show_strip: true,
            snapshots: Vec::new(),
            appearance: crate::theme::Appearance::default(),
            layout: crate::layout::Layout::default(),
            look_open: false,
            registry: {
                // 内置清单先立住，再叠目录里扫到的：同 id 以文件为准，
                // 用户从市场更新过的版本不该被编译进二进制的那份盖回去。
                let root = crate::settings::plugin_dir();
                aevum_core::plugin::Registry::default()
                    .with_builtin(aevum_core::plugin::builtin(), &root)
                    .merge(aevum_core::plugin::Registry::scan(
                        &root,
                        aevum_core::plugin::Source::User,
                    ))
            },
            market_open: false,
            preview_rgb: None,
            clip_highlights: false,
            clip_shadows: false,
            clip_mask: None,
            clip_mask_stamp: u64::MAX,
            plugin_result: None,
            plugin_stamp: 0,
            plugin_job: None,
            plugin_progress: (0, 0),
            plugin_crashes: HashMap::new(),
            market_filter: None,
            market_query: String::new(),
            dock_scroll_end: false,
            dock_focus: None,
            show_film: true,
            // 默认关：胶片条那一行要留给缩略图。波形是去闪时的检视工具，
            // 常年摊在底部会把 176px 的胶片条挤成 44px 的窄缝。
            show_curves: false,
            rulers: false,
            about_open: false,
            keyframes: BTreeMap::new(),
            copies: Vec::new(),
            export_copy: None,
            copy_name: String::new(),
            hdr_around: 1,
            hdr_last: None,
            hdr_error: None,
            pano_around: 1,
            pano_last: None,
            pano_error: None,
            // 星轨默认给 9 帧（±4）：再少星轨连不成线，再多读盘就成了等待。
            stack_around: 4,
            stack_mode: aevum_core::stack::Mode::Lighten,
            stack_sigma: 3.0,
            stack_fill: true,
            stack_last: None,
            stack_error: None,
            // 时间切片默认 8 帧、4% 羽化，正好铺满一屏八带。
            tb_around: 7,
            tb_layout: aevum_core::timeblend::Layout::SkyGround,
            tb_bands: 8,
            tb_feather: 0.04,
            tb_horizon: 0.5,
            tb_horizon_auto: false,
            tb_deghost: 0.5,
            tb_match: true,
            tb_last: None,
            tb_error: None,
            tb_preview: None,
            tb_preview_img: None,
            tb_preview_key: 0,
            tb_preview_inflight: false,
            tb_preview_data: None,
            tb_preview_error: None,
            // 焦点合成默认 8 帧（±7 从起始帧往后铺）、深度图法、半径 2、兜底关、3×3 去斑、2 px 羽化。
            // 合成前自动对齐默认关（与 `MergeParams::default()` 同步）。
            focus_around: 7,
            focus_method: aevum_core::focus::Method::DepthMap,
            focus_radius: 2,
            focus_threshold: 0.0,
            focus_smooth_z: 1,
            focus_feather: 2.0,
            focus_align: false,
            focus_last: None,
            focus_error: None,
            focus_depth_tex: None,
            focus_depth_note: None,
            // 判据默认值与核心一致：±1 EV 之外判废、相邻突变 1 EV、清晰度不设限。
            cull_ev_low: -1.0,
            cull_ev_high: 1.0,
            cull_jump: 1.0,
            cull_sharp: 0.0,
            cull_note: None,
            cull_target: String::new(),
            cull_export_busy: false,
            stamp_on: false,
            resolution: None,
            source_note: None,
            mask_sel: None,
            mask_comp: 0,
            mask_drag: None,
            mask_move_anchor: None,
            mask_paint: None,
            heal_radius: 2.0,
            heal_feather: 35.0,
            heal_clone: false,
            heal_src: None,
            heal_last: None,
            upright_editing: false,
            brush_radius: 6.0,
            brush_hardness: 70.0,
            brush_flow: 100.0,
            brush_erase: false,
            mask_pick: false,
            mask_pick_img: None,
            mask_geo_cache: Default::default(),
            curve_lut_cache: None,
            hist_version: 0,
            hist_mesh_cache: None,
            corr_version: 0,
            defl_curve_cache: None,
            lens_profile_note: String::new(),
            lens_profile_saved: None,
            dcp_name: String::new(),
            dcp_note: String::new(),
            lcp_note: String::new(),
            user_crop: None,
            rotate: 0.0,
            crop_drag: None,
            motion_drag: None,
            motion_picking: false,
            motion_box: MotionBox::End,
            hsl_band: 0,
            curve_chan: 0,
            curve_tab: 0,
            mixer_tab: 0,
            hsl_all_prop: 0,
            curve_grab: None,
            levels_grab: None,
            retime_grab: None,
            output_w: 3840,
            output_h: 2160,
            output_fps: 24.0,
            codec: "HEVC".to_owned(),
            export_busy: false,
            last_saved: None,
            headless: false,
            control_enabled: true,
            shot_mode: false,
            align_report: None,
            align_busy: false,
            align_estimator: Estimator::Template,
            align_ref: 0,
            align_preview: true,
            align_msg: None,
            align_done: Arc::new(AtomicUsize::new(0)),
            render_mode: OutputMode::Video,
            render_format: StillFormat::Png8,
            render_quality: 90,
            render_keep_exif: false,
            render_xmp_sidecar: false,
            render_sharpen_for: OutputSharpen::default(),
            render_sharpen_strength: SharpenStrength::default(),
            render_color_space: ColorSpace::default(),
            render_dynamic: OutputDynamic::default(),
            render_watermark: Watermark::default(),
            retime: Retime::default(),
            motion: Motion::default(),
            render_crf: 18,
            render_preset: "medium".to_owned(),
            render_audio: None,
            render_audio_opts: AudioOptions::default(),
            audio_info: None,
            audio_probe_err: None,
            audio_probe_busy: false,
            render_out_dir: PathBuf::new(),
            render_video_out: PathBuf::new(),
            render_keep_frames: false,
            render_resume: false,
            export_skipped_shared: Arc::new(AtomicUsize::new(0)),
            export_stage: None,
            export_done: Arc::new(AtomicUsize::new(0)),
            export_total: Arc::new(AtomicUsize::new(0)),
            export_stage_shared: Arc::new(AtomicU8::new(0)),
            cancel_export: Arc::new(AtomicBool::new(false)),
            merge_busy: false,
            merge_kind: None,
            merge_result: None,
            merge_error: None,
            merge_done: Arc::new(AtomicUsize::new(0)),
            merge_total: Arc::new(AtomicUsize::new(0)),
            cancel_merge: Arc::new(AtomicBool::new(false)),
            merge_job: 0,
            qa_busy: false,
            qa_findings: Vec::new(),
            qa_run: false,
            qa_error: None,
            qa_sig: 3.0,
            qa_done: Arc::new(AtomicUsize::new(0)),
            qa_total: Arc::new(AtomicUsize::new(0)),
            cancel_qa: Arc::new(AtomicBool::new(false)),
            queue: Vec::new(),
            queue_path: queue::default_path(),
            queue_busy: false,
            queue_current: None,
            queue_done: Arc::new(AtomicUsize::new(0)),
            queue_total: Arc::new(AtomicUsize::new(0)),
            queue_stage: Arc::new(AtomicU8::new(0)),
            cancel_queue: Arc::new(AtomicBool::new(false)),
            last_auto_save: Instant::now(),
            eta: EtaWindow::default(),
            saved_snap: None,
            saved_stamp: None,
            reload_token: 0,
            update_open: false,
            update_check: None,
            update_body: String::new(),
            update_sums_url: None,
            update_phase: UpdatePhase::Idle,
            update_error: None,
            update_progress: Arc::new(Mutex::new((0, 0))),
            update_ready: None,
            update_job: 0,
            last_update_check: None,
            rollback_confirm: false,
            tx,
            rx,
            control: None,
            ai_input: String::new(),
            ai_log: Vec::new(),
            control_tx,
            control_rx,
        };
        // 历史先落下第 0 步：不这么做，会话里第一次改动就没有「之前」可以回。
        s.history_reset("空状态");
        // 图库清单跨会话常驻：启动就载入，编目等展开目录时再扫。
        let store = aevum_core::library::load(&aevum_core::library::default_path());
        s.lib_roots = store.roots;
        s.lib_cols = store.collections;
        s.lib_entries = vec![Vec::new(); s.lib_roots.len()];
        s
    }

    pub fn frame_count(&self) -> usize {
        self.seq.as_ref().map(|s| s.len()).unwrap_or(0)
    }

    /// 切工作台（显影 / 帧表 / 导出 / 任务）。
    ///
    /// 只改路由，不动任何数据：四个工作台读的是同一份序列与参数，切回来时
    /// 画布、选中帧、滚动位置都还在原处。
    ///
    /// Switch benches (develop / table / export / tasks).
    ///
    /// Routing only: all four read the same sequence and parameters, so the canvas,
    /// selected frame and scroll positions are still where they were when you switch back.
    pub fn set_bench(&mut self, bench: crate::shell::Bench) {
        if bench != crate::shell::Bench::Develop && self.bench == crate::shell::Bench::Develop {
            // 离开显影台收起预设与快照浮窗（R7 巡检）：预设是调色工具，在帧表 /
            // 任务台没有对象可作用，auto-open 只会盖住那两页自己的左列。设置里的
            // show_left 不回写——回到显影台，它还按上次的开关展开。
            //
            // Leaving Develop collapses the presets window (R7 sweep): presets are a
            // grading tool with no subject on the frame table / tasks benches, and
            // auto-open only covers those pages' own left columns. The settings key
            // is untouched -- back on Develop it expands per the saved toggle.
            self.show_left = false;
        }
        self.bench = bench;
    }

    /// 正在跑的后台任务数：工作台导航的角标用。
    ///
    /// 只数真在跑的那几个（队列渲染 / 导出 / 合成 / 剔除复制 / 对齐），已完成的
    /// 任务不占角标——角标读作「还有几件事没完」。
    ///
    /// Number of background jobs in flight, for the bench nav badge.
    ///
    /// Only the ones actually running are counted (queue render / export / merge / cull
    /// copy / align); finished jobs do not hold the badge, which reads as "how many things
    /// are still going".
    pub fn running_jobs(&self) -> usize {
        [
            self.queue_busy,
            self.export_busy,
            self.merge_busy,
            self.cull_export_busy,
            self.align_busy,
        ]
        .into_iter()
        .filter(|busy| *busy)
        .count()
    }

    /// 素材是不是只有一张照片。序列级的功能（对齐、去闪）靠邻居帧才有意义。
    ///
    /// Whether the material is a single photo. Sequence-level features (alignment,
    /// deflicker) only make sense with neighbouring frames.
    pub fn is_single(&self) -> bool {
        self.frame_count() == 1
    }

    pub fn offset_at(&self, i: usize) -> f32 {
        self.corr.get(i).copied().unwrap_or(0.0)
    }

    /// 这一帧的去闪补偿，直接喂给显影链。
    ///
    /// 单通道时 `chan` 全零，与只补亮度的老口径逐位一致；开分通道后 `add` 吃三
    /// 通道均值、`chan` 只留色差，两者相加正好还原该通道原本的偏移。
    ///
    /// This frame's deflicker compensation, fed straight into the develop chain.
    ///
    /// In single-channel mode `chan` is all zeros, bit-identical to the old brightness-only
    /// definition; with per-channel on, `add` carries the three-channel mean and `chan` keeps
    /// only the colour difference, and the two together restore each channel's original
    /// offset exactly.
    pub fn ev_at(&self, i: usize) -> Ev {
        Ev {
            add: self.offset_at(i),
            chan: self.corr_rgb.get(i).copied().unwrap_or([0.0; 3]),
        }
    }

    pub fn open(&mut self, ctx: &Context, path: PathBuf) {
        self.open_paths(ctx, std::slice::from_ref(&path));
    }

    /// 拖进来或选进来的一堆路径：单个可以是目录也可以是照片，多个按照片列表当素材。
    ///
    /// 单张照片不能退化成「打开它所在的整个文件夹」——那样用户挑的那张会被邻居淹没，
    /// 护带和成片还会写进一个他根本没选的目录。
    ///
    /// A batch of paths dropped or picked: a single one may be a directory or a photo,
    /// several are taken as a photo list.
    ///
    /// A single photo must not degrade into "open the whole folder it sits in" -- the one the
    /// user picked would drown among its neighbours, and the guard directory and the exports
    /// would land in a directory he never chose.
    pub fn open_paths(&mut self, ctx: &Context, paths: &[PathBuf]) {
        let seq = match paths {
            [] => return,
            [only] => Sequence::open_any(only),
            many => Sequence::open_files(many).or_else(|e| match e {
                SequenceError::NoUsableFile => match many.iter().find(|p| p.is_dir()) {
                    Some(dir) => Sequence::open(dir),
                    None => Err(SequenceError::NoUsableFile),
                },
                other => Err(other),
            }),
        };
        let seq = match seq {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(format!("{e}"));
                return;
            }
        };
        self.reload_token += 1;
        let token = self.reload_token;
        let n = seq.len();
        let single = seq.is_single();

        let root = seq.root.clone();
        let seq_name = seq.display_name();
        self.seq = Some(seq);
        // 导入基线不在这里取：下面还会重置 / 恢复 / 套 RAW 默认参数，
        // 此刻的 `params` 并不是这盘序列真正落地的值。取值点见
        // `apply_raw_import_defaults` 之后（用户 2026-11-10 实测：导入序列
        // 直接跳到导出页，就是因为基线过早导致「已调色」误判）。
        //
        // The import baseline is NOT taken here: the code below resets params,
        // restores the project and applies RAW defaults, so `params` at this
        // instant is not what this sequence actually ends up with. It is taken
        // after `apply_raw_import_defaults` (2026-11-10 user report: importing
        // a sequence jumped straight to the export page because an early
        // baseline made "already graded" read true).
        self.unload_jobs();
        self.align_report = None;
        self.align_busy = false;
        self.align_msg = None;
        self.align_ref = 0;
        self.align_preview = true;
        self.align_done.store(0, Ordering::Relaxed);
        self.render_mode = OutputMode::Video;
        self.render_out_dir = root.join("成片");
        self.render_video_out = root.join(format!("{seq_name}_成片.mp4"));
        self.render_audio = None;
        self.audio_info = None;
        self.audio_probe_err = None;
        self.metrics = vec![None; n];
        // 导入在后台线程收尾时 UI 可能停在反应式空转：到点必须叫醒重绘，
        // 否则 ✓ 与胶片条要等用户动一下鼠标才出现（用户实测「勾选不及时」）。
        //
        // The import lands on a background thread and a reactive UI may be idling: wake
        // the repaint, or the ✓ and the strip wait for the next mouse move (user-reported
        // "checkmarks update late").
        ctx.request_repaint();
        self.exif.clear();
        self.issues.clear();
        self.analysis = None;
        self.corr.clear();
        self.corr_rgb.clear();
        // 与 `metrics` 同长：测光分块回包按序号切片写入，长度不对就拼错位。
        //
        // Same length as `metrics`: metering chunks are spliced in by index, and a
        // mismatched length would write into the wrong slots.
        self.def_means = vec![None; n];
        self.selected = 0;
        // 筛选只对上一份序列成立：星级下限 / 旗标 / 色标一路带到新序列，整组新帧会被
        // 静默筛掉，胶片条看起来「突然没有预览图」（用户截图）。换序列一律归零。
        //
        // A filter belongs to the sequence it was set on: carrying a minimum star rating,
        // flag or colour label into a new sequence silently hides every frame there and the
        // strip reads as "the previews suddenly disappeared" (user screenshot). Opening a
        // sequence clears all three.
        self.film_filter_stars = 0;
        self.film_filter_flag = 0;
        self.film_filter_label = 0;
        self.texture = None;
        self.texture_orig = None;
        self.reset_session();
        // 换序列等于换画幅：上一个序列的取景框和拉直角度对新尺寸没有意义。
        self.user_crop = None;
        self.rotate = 0.0;
        self.crop_drag = None;
        self.saved_snap = None;
        self.saved_stamp = None;
        self.ratings.clear();
        self.flags.clear();
        self.labels.clear();
        // 关键词、堆叠与元数据都跟着工程走：新目录没有工程文件时也得清空，否则上一个
        // 序列的词和版权信息会赖在这儿，等自动保存写进新目录的工程里。
        self.keywords.clear();
        self.stacks.clear();
        self.meta = project::Metadata::default();
        self.hist = None;
        self.keyframes.clear();
        self.copies.clear();
        self.export_copy = None;
        self.hdr_last = None;
        self.hdr_error = None;
        self.pano_last = None;
        self.pano_error = None;
        self.stack_last = None;
        self.stack_error = None;
        self.tb_last = None;
        self.tb_error = None;
        self.tb_preview = None;
        self.tb_preview_img = None;
        self.tb_preview_key = 0;
        self.tb_preview_inflight = false;
        self.tb_preview_data = None;
        self.tb_preview_error = None;
        // 深度图预览只对「这一个序列的这一组参数」成立：换序列 / 关闭一律作废。
        //
        // The depth-map preview is only valid for one selection in one sequence: switching
        // or closing voids it.
        self.focus_depth_tex = None;
        self.focus_depth_note = None;
        self.merge_kind = None;
        self.merge_result = None;
        self.merge_error = None;
        if self.merge_busy {
            // 换序列时正在跑的合并作废：只转取消旗，忙碌标记留给旧 job 自己的回包清。
            // 这里若抢清 `merge_busy`，用户就能在新序列上再起一个合并，而旧线程还在
            // 往上一份成片里写——同一个出口两条线程。
            //
            // A sequence switch only raises the cancel flag; the busy flag waits for the old
            // run's own reply. Clearing it here would let a second merge start while the old
            // thread is still writing the previous output.
            self.cancel_merge.store(true, Ordering::SeqCst);
        }
        // 体检清单是这一段的诊断结论，换序列必须清空，否则上一段的坏帧还挂在表里。
        self.qa_findings.clear();
        self.qa_run = false;
        self.qa_error = None;
        if self.qa_busy {
            self.cancel_qa.store(true, Ordering::SeqCst);
            self.qa_busy = false;
        }
        self.params = GradeParams::default();
        self.preset = "原始".to_owned();
        self.auto_note = None;
        self.sidecar_hint = None;
        self.reset_mask_state();
        self.zoom = 1.0;
        self.fit_scale = 1.0;
        self.pan = Vec2::ZERO;
        self.playing = false;
        self.scroll_to_selected = true;
        self.error = None;
        self.measuring = Some(n);
        self.resolution = None;
        self.source_note = None;
        self.status = if single {
            "正在读取这张照片…".to_owned()
        } else {
            crate::i18n::tr_fmt("正在测量 {} 帧…", &[&n])
        };
        self.module = Module::Develop;
        self.load_project(&root);
        // 边车只提示不套用：检测到同目录 .xmp 时状态栏给一键应用（架构师建议、本轮采纳）。
        self.detect_sidecar_hint();
        // RAW 首次导入（工程文件不存在）时套默认锐化/降噪；有工程文件的用户参数不动。
        self.apply_raw_import_defaults(&root);
        // 基线必须取**最终落地**的参数：重置 / 工程恢复 / RAW 默认都在上面跑完了，
        // 此刻才等于「这盘序列没调过色」。早取会让 `StepCheck::Graded` 立刻为真，
        // 派生步跳过前两步、落到最后一步导出页（用户 2026-11-10 实测）。
        //
        // The baseline MUST snapshot the params as they finally landed: reset,
        // project restore and RAW defaults all ran above, so only now does it mean
        // "this sequence is ungraded". Taking it earlier makes `StepCheck::Graded`
        // true immediately, skipping the first steps and landing on the export
        // page (2026-11-10 user report).
        self.grade_baseline = Some(self.params.clone());
        // 打开新序列是「从头开始」：清掉上一盘手选的步骤页，并把工作台归回显影，
        // 否则工作台选中显示显影、内容区却停在旧的导出页（两套状态打架）。
        //
        // Opening a new sequence starts over: clear the previous hand-picked step
        // and return the bench to Develop, or the bench selection says Develop
        // while the content still sits on the old export page (two states at odds).
        self.flow_step = None;
        self.set_bench(crate::shell::Bench::Develop);
        // 工程恢复完才是这盘棋的起点：历史从头记，撤销不会把用户带回上一个序列。
        // 默认参数挂在 history_reset 之前，进初始历史、可撤销。
        self.history_reset("打开序列");

        let paths = self.seq.as_ref().map(|s| s.paths()).unwrap_or_default();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        let cache_root = root.clone();
        // 换序列先停掉上一趟测光：千帧的旧活不能和新的一起抢核。
        //
        // Stop the previous metering pass first: a thousand old frames must not compete
        // with the new sequence for cores.
        self.cancel_measure.store(true, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_measure = Arc::clone(&cancel);
        // 画面不等测光：首帧参数已经就位，预览现在就能解；先把画面放出来，测光
        // 在后面一块一块补。此前 `preview_dirty` 只在整条测光跑完时置位，千帧序列
        // 就变成「载入预览…」一直挂着（用户 2026-11-07 报告）。
        //
        // The picture does not wait for metering: the first frame's parameters are in place,
        // so the preview can decode right now. Show it first; metering catches up chunk by
        // chunk. Previously `preview_dirty` was only set once the whole pass finished, which
        // left "loading preview…" on screen for a thousand-frame sequence (user report,
        // 2026-11-07).
        self.preview_dirty = true;
        thread::spawn(move || {
            // 第一包只做元数据：EXIF 是块寻址的，千帧也就几百毫秒，不该陪整条测光
            // 一起堵在后面——镜头匹配与直方图下那行参数只等它。
            //
            // The first package is metadata only: EXIF reads are block-addressed, a few
            // hundred milliseconds even for a thousand frames, and lens matching plus the
            // capture line under the histogram only wait for this.
            let exif = aevum_core::exif::read_many(&paths);
            // 竖拍帧的显示尺寸是旋转之后的：EXIF 方向 5~8 交换长短边。
            // `metrics::decode` 对 RAW 同样会转正，所以这条交换三条分支都要吃，
            // 否则竖拍 RAW 的状态栏尺寸和画面差一个 90°。
            let orientation = exif.first().map_or(1, |e| e.orientation);
            let rotated = |(w, h): (u32, u32)| {
                if (5..=8).contains(&orientation) {
                    (h, w)
                } else {
                    (w, h)
                }
            };
            // `image` 库不认 CFA 容器，RAW/DNG 的宽高只能问我们自己的解析器；
            // 顺带把「传感器还是机身预览」这条口径带出来，信息面板用它标注保真度。
            let (resolution, source_note) = match paths.first() {
                None => (None, None),
                Some(p) if aevum_core::raw::is_raw_name(p) => match aevum_core::raw::info(p) {
                    Ok(i) => (
                        Some(rotated((i.width, i.height))),
                        Some(crate::i18n::tr_fmt(
                            "{} · {} 位",
                            &[&i.path.short(), &i.bits],
                        )),
                    ),
                    Err(_) => (None, None),
                },
                Some(p) if aevum_core::dng::is_named_dng(p) => match aevum_core::dng::info(p) {
                    Ok(i) => (
                        Some(rotated((i.width, i.height))),
                        Some(crate::i18n::tr_fmt("DNG · 传感器直解 · {} 位", &[&i.bits])),
                    ),
                    // 内置解析只认单通道原始数据，三通道 LinearRaw（iPhone 导出的 DNG）
                    // 会回落后端解码——尺寸和保真度就得问后端。不回落的话，面板会对着
                    // 一张能正常显示的图说「尺寸不明」，两处说法自相矛盾。
                    Err(e) if e.soft_fallback() => match aevum_core::raw::info(p) {
                        Ok(i) => (
                            Some(rotated((i.width, i.height))),
                            Some(crate::i18n::tr_fmt(
                                "{} · {} 位",
                                &[&i.path.short(), &i.bits],
                            )),
                        ),
                        Err(_) => (None, None),
                    },
                    Err(_) => (None, None),
                },
                Some(p) => (image::image_dimensions(p).ok().map(rotated), None),
            };
            let _ = tx.send(Msg::Loaded(Box::new(Loaded {
                token,
                resolution,
                source_note,
                exif,
            })));
            ctx2.request_repaint();

            // 测光按块回包：每块落一条 `Msg::Measured`，状态栏进度与去闪曲线一路
            // 跟着长。块大小取 1/16 且落在 8..=192：千帧序列约 16 次回包，几十帧
            // 的短序列一块就完。
            //
            // Metering replies chunk by chunk: each chunk lands one `Msg::Measured`, so the
            // status-line progress and the deflicker curve grow along the way. Chunk size is
            // 1/16 of the sequence clamped to 8..=192: about 16 replies for a thousand frames,
            // and a few dozen frames finish in a single chunk.
            let n = paths.len();
            let chunk = n.div_ceil(16).clamp(8, 192).max(1);
            let mut lo = 0;
            while lo < n {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let hi = (lo + chunk).min(n);
                let (metrics, def_means, issues) = measure_pool().install(|| {
                    aevum_core::measures::measure_sequence(&cache_root, &paths[lo..hi], 512)
                });
                let done = hi == n;
                let _ = tx.send(Msg::Measured(Box::new(Measured {
                    token,
                    lo,
                    metrics,
                    def_means,
                    issues,
                    done,
                })));
                lo = hi;
                ctx2.request_repaint();
            }
        });
        // 状态栏留在「正在测量 N 帧…」：这时候测光才刚起跑，先把帧数写在前面
        // 等于告诉用户已经读完了。真正的汇总由 `Msg::Loaded` 落账时替换。
        //
        // Keep the "measuring N frames…" status: the metering pass has only just started, and
        // writing the summary here would claim it is already done. `Msg::Loaded` replaces it.
    }

    /// 关闭当前序列，回到空状态；不碰磁盘上的素材。
    ///
    /// Close the current sequence and return to the empty state; never touches footage on
    /// disk.
    pub fn close(&mut self) {
        self.save_project();
        self.unload_jobs();
        self.reload_token += 1;
        // 关图时把还在跑的测光停掉：千帧的旧活不该继续吃核。
        self.cancel_measure.store(true, Ordering::SeqCst);
        self.align_report = None;
        self.align_msg = None;
        self.align_ref = 0;
        self.render_out_dir.clear();
        self.render_video_out.clear();
        self.seq = None;
        self.metrics.clear();
        self.exif.clear();
        self.issues.clear();
        self.analysis = None;
        self.corr.clear();
        self.corr_rgb.clear();
        self.def_means.clear();
        self.selected = 0;
        // 关闭后筛选归零，和打开新序列同一口径。
        //
        // Closing clears the filter too, matching the open path.
        self.film_filter_stars = 0;
        self.film_filter_flag = 0;
        self.film_filter_label = 0;
        self.texture = None;
        self.texture_orig = None;
        self.reset_session();
        // 换序列等于换画幅：上一个序列的取景框和拉直角度对新尺寸没有意义。
        self.user_crop = None;
        self.rotate = 0.0;
        self.crop_drag = None;
        self.saved_snap = None;
        self.saved_stamp = None;
        self.ratings.clear();
        self.flags.clear();
        self.labels.clear();
        self.keywords.clear();
        self.stacks.clear();
        self.meta = project::Metadata::default();
        self.hist = None;
        self.keyframes.clear();
        self.copies.clear();
        self.export_copy = None;
        self.hdr_last = None;
        self.hdr_error = None;
        self.pano_last = None;
        self.pano_error = None;
        self.stack_last = None;
        self.stack_error = None;
        self.tb_last = None;
        self.tb_error = None;
        self.tb_preview = None;
        self.tb_preview_img = None;
        self.tb_preview_key = 0;
        self.tb_preview_inflight = false;
        self.tb_preview_data = None;
        self.tb_preview_error = None;
        // 深度图预览只对「这一个序列的这一组参数」成立：换序列 / 关闭一律作废。
        //
        // The depth-map preview is only valid for one selection in one sequence: switching
        // or closing voids it.
        self.focus_depth_tex = None;
        self.focus_depth_note = None;
        self.merge_kind = None;
        self.merge_result = None;
        self.merge_error = None;
        if self.merge_busy {
            // 换序列时正在跑的合并作废：只转取消旗，忙碌标记留给旧 job 自己的回包清。
            // 这里若抢清 `merge_busy`，用户就能在新序列上再起一个合并，而旧线程还在
            // 往上一份成片里写——同一个出口两条线程。
            //
            // A sequence switch only raises the cancel flag; the busy flag waits for the old
            // run's own reply. Clearing it here would let a second merge start while the old
            // thread is still writing the previous output.
            self.cancel_merge.store(true, Ordering::SeqCst);
        }
        // 体检清单是这一段的诊断结论，换序列必须清空，否则上一段的坏帧还挂在表里。
        self.qa_findings.clear();
        self.qa_run = false;
        self.qa_error = None;
        if self.qa_busy {
            self.cancel_qa.store(true, Ordering::SeqCst);
            self.qa_busy = false;
        }
        self.params = GradeParams::default();
        self.preset = "原始".to_owned();
        self.auto_note = None;
        self.sidecar_hint = None;
        self.reset_mask_state();
        self.resolution = None;
        self.source_note = None;
        self.measuring = None;
        self.playing = false;
        self.zoom = 1.0;
        self.fit_scale = 1.0;
        self.pan = Vec2::ZERO;
        self.module = Module::Sequence;
        self.last_saved = None;
        self.error = None;
        self.status = crate::i18n::t!("打开一个帧文件夹，或把文件夹拖进窗口").to_owned();
        self.history_reset("空状态");
    }

    /// 会话级清理：close 与换序列两条路的公共段，一处维护（性能重构设计 E7）。
    /// 字段清单收编自原 open 与 close 里两段**逐字相同**的块（收编前 diff 为零，
    /// 见 docs/基准-改前-2026-10-30.md 的核对记录）。
    pub(super) fn reset_session(&mut self) {
        // 插件结果与 CPU 像素都是"这一张图"的衍生物，换序列一并作废；
        // 剪切遮罩跟着 preview 走，也在这里清掉。（这段清单在 close 与
        // 换序列两条路上重复出现——后续应抽成 reset_session 一处维护。）
        self.plugin_result = None;
        self.plugin_stamp = 0;
        self.plugin_progress = (0, 0);
        self.preview_rgb = None;
        self.clip_mask = None;
        self.clip_mask_stamp = u64::MAX;
        self.mask_paint = None;
        // 仿制源与上一落点也是「这一张图」的会话态：撤销栈不含它们，换序列
        // 不清场就会把 A 图的仿制源坐标带到 B 图上，落下去就是错位的一笔。
        //
        // The clone source and the last stroke point are per-image session state too: the
        // undo stack does not carry them, so without this sweep photo A's clone source
        // coordinates would leak into photo B and land as a misplaced stroke.
        self.heal_src = None;
        self.heal_last = None;
        self.preview_size = [0, 0];
        self.preview_dirty = false;
        self.preview_inflight = false;
        self.thumbs.clear();
        self.thumb_pending.clear();
        self.thumb_src.clear();
        self.thumb_regrade.clear();
        // 播放代理与缩略图同属「这一份序列的衍生物」，换序列一并作废；
        // 拖动世代号也归零，免得上一次拖动的 epoch 漏进新序列的历史合并。
        //
        // The playback proxies are derivatives of this sequence just like the thumbnails,
        // so clear them on switch; the drag epoch resets too, so the previous drag's epoch
        // cannot leak into the new sequence's history merge.
        self.proxies.clear();
        self.proxy_pending.clear();
        self.drag_epoch = 0;
        // 四类缓存跟着会话走：换序列/关图整体清空（指纹自校验之外的保守兜底）。
        self.mask_geo_cache = Default::default();
        self.curve_lut_cache = None;
        self.hist_mesh_cache = None;
        self.defl_curve_cache = None;
    }

    /// 蒙版五连复位（性能重构设计 E8）：选中/形状/拖拽/取色态/取色像素
    /// 是一组共进退的界面状态，收成一处，避免手抄漏字段造成跨会话泄漏。
    pub(super) fn reset_mask_state(&mut self) {
        self.mask_sel = None;
        self.mask_comp = 0;
        self.mask_drag = None;
        self.mask_pick = false;
        self.mask_pick_img = None;
    }

    pub fn rating_at(&self, i: usize) -> u8 {
        self.ratings.get(&i).copied().unwrap_or(0)
    }

    pub fn set_rating(&mut self, i: usize, stars: u8) {
        let stars = stars.min(5);
        if stars == 0 {
            self.ratings.remove(&i);
        } else {
            self.ratings.insert(i, stars);
        }
        history::note(
            "rating",
            crate::i18n::tr_fmt("帧 {} 评 {} 星", &[&format!("{:04}", i + 1), &stars]),
        );
    }

    pub fn selected_rating(&self) -> u8 {
        self.rating_at(self.selected)
    }

    pub fn rate_selected(&mut self, stars: u8) {
        self.set_rating(self.selected, stars);
    }

    pub fn flag_at(&self, i: usize) -> i8 {
        self.flags.get(&i).copied().unwrap_or(0)
    }

    pub fn set_flag(&mut self, i: usize, flag: i8) {
        if flag == 0 {
            self.flags.remove(&i);
        } else {
            self.flags.insert(i, flag.signum());
        }
        let mark = match flag {
            1 => "选用",
            -1 => "排除",
            _ => "取消标记",
        };
        history::note(
            "flag",
            crate::i18n::tr_fmt(
                "帧 {} {}",
                &[&format!("{:04}", i + 1), &crate::i18n::tr(mark)],
            ),
        );
    }

    pub fn selected_flag(&self) -> i8 {
        self.flag_at(self.selected)
    }

    /// 一次标一批帧（筛选台用）：整批只记一行历史，不逐帧刷。
    ///
    /// Flag a batch of frames at once (the cull station): one history line for the whole batch
    /// instead of one per frame.
    pub fn flag_many(&mut self, indices: &[usize], flag: i8) {
        for i in indices {
            if flag == 0 {
                self.flags.remove(i);
            } else {
                self.flags.insert(*i, flag.signum());
            }
        }
        if !indices.is_empty() {
            let verb = if flag < 0 { "排除" } else { "选用" };
            history::note(
                "flag",
                crate::i18n::tr_fmt("{} 帧{}", &[&indices.len(), &crate::i18n::tr(verb)]),
            );
        }
    }

    /// 画布叠印的那一行字：帧号 · 拍摄时间 · 机型与曝光。缺什么省什么。
    ///
    /// The line burned onto the canvas: frame number, capture time, camera and exposure.
    /// Anything missing is simply left out.
    pub fn stamp_text(&self) -> Option<String> {
        let total = self.frame_count();
        if total == 0 {
            return None;
        }
        let index = self.selected.min(total - 1);
        Some(stamp_line(index, total, self.exif.get(index)))
    }

    pub fn set_selected_flag(&mut self, flag: i8) {
        let i = self.selected;
        self.set_flag(i, flag);
    }

    pub fn label_at(&self, i: usize) -> u8 {
        self.labels.get(&i).copied().unwrap_or(0)
    }

    pub fn set_label(&mut self, i: usize, label: u8) {
        if label == 0 {
            self.labels.remove(&i);
        } else {
            self.labels.insert(i, label.min(4));
        }
        history::note(
            "label",
            crate::i18n::tr_fmt(
                "帧 {} 色标「{}」",
                &[
                    &format!("{:04}", i + 1),
                    &crate::i18n::tr(LABEL_NAMES[label.min(4) as usize]),
                ],
            ),
        );
    }

    pub fn selected_label(&self) -> u8 {
        self.label_at(self.selected)
    }

    pub fn set_selected_label(&mut self, label: u8) {
        let i = self.selected;
        self.set_label(i, label);
    }

    pub fn rated_count(&self) -> usize {
        self.ratings.values().filter(|v| **v > 0).count()
    }

    /// 胶片条这一帧是否显示：星级 / 旗标 / 色标三条判据同时成立才画。
    /// 只决定「画不画」，不动真实帧序——播放、导出与缩略图预热仍按真帧号走。
    ///
    /// Whether the filmstrip draws this frame: all three criteria (stars, flag, colour
    /// label) must hold. It only decides visibility and never reorders the sequence --
    /// playback, export and thumbnail warm-up keep the real frame index.
    pub fn film_visible(&self, i: usize) -> bool {
        (self.film_filter_stars == 0 || self.rating_at(i) >= self.film_filter_stars)
            && (self.film_filter_flag == 0 || self.flag_at(i) == self.film_filter_flag)
            && (self.film_filter_label == 0 || self.label_at(i) == self.film_filter_label)
    }

    /// 通过三层筛选的帧数，给头行「已筛选 n / m 帧」用。
    ///
    /// Frames surviving all three filters, for the header's "n / m shown" readout.
    pub fn film_visible_count(&self) -> usize {
        (0..self.frame_count())
            .filter(|i| self.film_visible(*i))
            .count()
    }

    /// 胶片条当前高度：用户拖过的优先，其次按首帧比例自动适配，兜底 84。
    /// 底部面板与胶片条共用这一个数，面板才不会为「剩下的空间」留出一条空带。
    ///
    /// The filmstrip's current height: a user-dragged value first, else fitted to the
    /// first frame's aspect, else 84. The bottom panel and the strip share this number so
    /// the panel cannot keep a blank band for "whatever space is left".
    pub fn film_height(&self) -> f32 {
        let auto = self
            .thumb(self.selected)
            .map(|t| {
                let (tw, th) = (t.size[0] as f32, t.size[1] as f32);
                (92.0 * th / tw.max(1.0) + 18.0).clamp(64.0, 220.0)
            })
            .unwrap_or(84.0);
        self.film_h.unwrap_or(auto).clamp(48.0, 220.0)
    }

    pub fn set_frame(&mut self, i: usize) {
        let n = self.frame_count();
        if n == 0 {
            return;
        }
        let i = i.min(n - 1);
        if i != self.selected {
            self.selected = i;
            self.preview_dirty = true;
            self.scroll_to_selected = true;
        }
    }

    pub fn step(&mut self, delta: i64) {
        let n = self.frame_count() as i64;
        if n == 0 {
            return;
        }
        let i = (self.selected as i64 + delta).clamp(0, n - 1);
        self.set_frame(i as usize);
    }

    pub fn toggle_play(&mut self) {
        // 少于两帧不播：单帧「播放」只会原地刷新同一张图，纯属费电；`< 2` 同时挡住
        // 命令与空格键绕过界面禁用态的那条路。
        //
        // Never play with fewer than two frames: playing a single frame only refreshes the
        // same image in place and wastes power; `< 2` also blocks the command and the
        // space bar from bypassing the UI's disabled state.
        if self.frame_count() < 2 {
            return;
        }
        self.playing = !self.playing;
        self.last_play = Instant::now();
        if !self.playing {
            self.preview_dirty = true;
        }
    }

    /// 把 core 压下的诊断提示搬进状态栏。
    ///
    /// 三条口径：
    /// - **保序**：core 那边先说的先显示，同一件事的因果不会被倒过来读；
    /// - **去重**：同一句话只留一条（用户点掉之后再来的同一句才重新出现）；
    /// - **上限 8 条，旧的先走**：新的问题必须留得住，否则刚触发的那条会被老账顶掉。
    ///
    /// 每帧开头调一次就够：`env::drain_notes` 是取走并清空，重复调用不会丢东西。
    ///
    /// Move the diagnostics core has queued into the status bar.
    ///
    /// Three rules:
    /// - **order preserved**: whatever core said first is shown first, so cause and effect
    ///   are never read backwards;
    /// - **deduplicated**: one copy per sentence (the same sentence reappears only after the
    ///   user dismissed it);
    /// - **capped at 8, oldest leaves first**: new problems must survive, otherwise the one
    ///   just triggered would be pushed out by old debts.
    ///
    /// Once at the start of each frame is enough: `env::drain_notes` takes and clears, so
    /// repeated calls lose nothing.
    pub fn drain_notices(&mut self) {
        for msg in aevum_core::env::drain_notes() {
            if self.notices.iter().any(|m| m == &msg) {
                continue;
            }
            self.notices.push(msg);
            if self.notices.len() > NOTICE_LIMIT {
                self.notices.remove(0);
            }
        }
    }

    pub fn tick(&mut self, ctx: &Context) {
        if self.playing {
            let n = self.frame_count();
            if n == 0 {
                self.playing = false;
            } else {
                let dt = 1.0 / self.output_fps.max(1.0);
                let elapsed = self.last_play.elapsed().as_secs_f32();
                if elapsed >= dt {
                    let steps = (elapsed / dt).floor().max(1.0) as usize;
                    self.last_play = Instant::now();
                    let next = (self.selected + steps) % n;
                    self.selected = next;
                    self.scroll_to_selected = true;
                    self.preview_dirty = true;
                    // 播放时画布吃 640px 代理（[`PROXY_SIDE`]，仍走机身内嵌原片、未
                    // 显影）：提前把前面 24 帧取回来，否则指针跑到哪儿才现解到哪儿，
                    // 画面就是一串空位闪。暂停后 `toggle_play` 会置 `preview_dirty`，
                    // 画布自动回全质量。只在上一批落地之后再要下一批，一次一批而不是
                    // 一帧一个线程。
                    //
                    // During playback the canvas uses the 640px proxy ([`PROXY_SIDE`],
                    // still the camera-embedded original, not developed): prefetch the next
                    // 24 frames, or the picture shows blank slots wherever the playhead has
                    // not decoded yet. On pause `toggle_play` sets `preview_dirty`, so the
                    // canvas returns to full quality. Only the next batch is requested after
                    // the previous one lands -- one batch at a time, not one thread per frame.
                    if self.proxy_pending.is_empty() {
                        self.ensure_proxies(ctx, next, next + PLAY_PROXY_AHEAD);
                    }
                }
                ctx.request_repaint_after(Duration::from_secs_f32(dt * 0.5));
            }
        }
        // 偏好（折叠态）落盘的唯一出口。界面那边只置 `settings_dirty`，帧尾在这里
        // 写一次：右键「全部收起」会一口气改十几个键，逐处写会把文件反复搓。
        // 写失败不打断用户（`settings::save` 自己会出声），置回 false 免得每帧重试。
        // 一次性截图（shot_mode）不落盘：演示用的临时外观/浮窗位置不该写进用户偏好。
        if self.settings_dirty {
            self.settings_dirty = false;
            if !self.shot_mode {
                crate::settings::save(&self.settings());
            }
        }
    }

    pub fn poll(&mut self, ctx: &Context) {
        // 对话框中枢先于一切输入处理：上一帧入队的弹窗现在就要起飞，
        // 后台线程带回的结果要在这一帧落到 state 上。
        //
        // The dialog hub runs before any input handling: a dialog queued last frame
        // takes off now, and replies from the worker thread land on state this frame.
        self.spawn_pending_dialog(ctx);
        self.drain_dialog_replies(ctx);
        // 布局开关的自动收账：`show_dock` / `show_flow` / `show_left` / `show_rails` /
        // `show_options` / `flow_task` 散在菜单、工具轨、浮窗等十来处赋值，逐个记得置
        // `settings_dirty` 早晚会漏。帧首比对一次就够——改动发生在上一帧的 ui()，下一帧
        // tick() 写盘，用户无感。
        //
        // Automatic persistence of layout switches: those six fields are assigned from a
        // dozen places (menus, rail, floating windows), and remembering to flag
        // `settings_dirty` at each one will eventually be missed. One diff at frame start
        // is enough -- the change happened in last frame's ui(), and tick() writes it on
        // the next frame without the user noticing.
        let switches = (
            self.show_dock,
            self.show_flow,
            self.show_left,
            self.show_rails,
            self.show_options,
            self.flow_task.key(),
        );
        if switches != self.layout_switches {
            self.layout_switches = switches;
            self.settings_dirty = true;
        }
        // 先把异步回调攒下的失败抬成 `error`（截图保存这类「命令先 OK、结果晚到」的
        // 路径），下一帧 `sync_error_dialog` 会把它变成弹窗。
        //
        // First promote failures collected by async callbacks (screenshot saving and other
        // "return early, finish later" paths) into `error`; `sync_error_dialog` turns it into
        // a dialog on the next frame.
        if let Ok(mut slot) = self.async_errors.lock()
            && !slot.is_empty()
        {
            let msgs: Vec<String> = slot.drain(..).collect();
            drop(slot);
            self.error = Some(msgs.join("\n"));
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Loaded(loaded) => {
                    if loaded.token != self.reload_token {
                        continue;
                    }
                    let Loaded {
                        resolution,
                        source_note,
                        exif,
                        ..
                    } = *loaded;
                    self.resolution = resolution;
                    self.source_note = source_note;
                    self.exif = exif;
                    // 首帧 EXIF 到手就能匹配镜头配置文件：命中折进参数，
                    // 预览标脏，没命中留一行说明。
                    self.match_lens_profile();
                    // 空序列不会再有测光块，别把「正在测量 0 帧…」挂在状态栏上。
                    if self.frame_count() == 0 {
                        self.measuring = None;
                    }
                }
                Msg::Measured(chunk) => {
                    if chunk.token != self.reload_token {
                        continue;
                    }
                    let Measured {
                        lo,
                        metrics,
                        def_means,
                        issues,
                        done,
                        ..
                    } = *chunk;
                    let hi = lo + metrics.len();
                    if hi > self.metrics.len()
                        || hi > self.def_means.len()
                        || metrics.len() != def_means.len()
                    {
                        // 长度对不上说明这一包属于另一份帧表（换过序列又换回来）：
                        // 宁可不落账，也不把数拼错位。
                        //
                        // A length mismatch means this package belongs to another frame
                        // table (switched away and back): skip it rather than splice numbers
                        // into the wrong slots.
                        continue;
                    }
                    self.metrics[lo..hi].copy_from_slice(&metrics);
                    self.def_means[lo..hi].copy_from_slice(&def_means);
                    self.issues
                        .extend(issues.into_iter().map(|(i, e)| (lo + i, e)));
                    // 去闪曲线跟着已测的部分走：千帧序列不用等最后一块才看见曲线。
                    // `recompute_deflicker` 会把预览标脏；但选中的那一帧还没测到时
                    // 没必要重渲染，先把旗子收回，避免每块都白解一帧整幅画面。
                    //
                    // The deflicker curve follows what has been measured so far, so a
                    // thousand-frame sequence shows it before the last chunk.
                    // `recompute_deflicker` marks the preview dirty; if the selected frame is
                    // not in this chunk there is nothing new to show, so put the flag back and
                    // avoid decoding a full frame per chunk for nothing.
                    let selected_measured = (lo..hi).contains(&self.selected);
                    let was_dirty = self.preview_dirty;
                    self.recompute_deflicker();
                    self.preview_dirty = was_dirty || selected_measured || done;
                    if done {
                        self.measuring = None;
                        let valid = self.metrics.iter().filter(|m| m.is_some()).count();
                        self.status = crate::i18n::tr_fmt(
                            "{} 帧 · 可读 {} · 缺帧 {}",
                            &[&self.frame_count(), &valid, &self.issues.len()],
                        );
                    } else {
                        self.status = crate::i18n::tr_fmt(
                            "正在测量 {} 帧… 已测 {}",
                            &[&self.frame_count(), &hi],
                        );
                    }
                }
                Msg::Preview(p) => {
                    self.preview_inflight = false;
                    let PreviewMsg {
                        stamp,
                        sketch,
                        graded,
                        original,
                        size,
                        hist,
                    } = *p;
                    if stamp == self.preview_stamp {
                        self.hist = Some(hist);
                        // 新预览落地：直方图 Mesh 缓存换版（键之一，见 HistMeshCache）。
                        self.hist_version += 1;
                        // 吸管取色要读像素：取色期间留一份，退出取色就清。
                        if self.mask_pick {
                            self.mask_pick_img = Some(graded.clone());
                        } else {
                            self.mask_pick_img = None;
                        }
                        // 插件要作用在"用户看到的这一张"上，所以留一份 CPU 像素。
                        // 底图一变，上一次的插件结果就作废 —— 留着会让用户对着
                        // 一张旧结果的图继续调参数。
                        if !sketch {
                            // 复用同一块缓冲：预览一帧就是 8 MB，每帧重新分配会让
                            // 分配器在拖动松手那一串全量渲染里一直忙。
                            let mut rgb = self
                                .preview_rgb
                                .take()
                                .map(|(mut v, _)| {
                                    v.clear();
                                    v
                                })
                                .unwrap_or_default();
                            rgb.reserve(graded.pixels.len() * 3);
                            for c in &graded.pixels {
                                rgb.extend_from_slice(&[c.r(), c.g(), c.b()]);
                            }
                            self.preview_rgb = Some((rgb, size));
                            // 底图换了一张，旧结果就没意义了。同一张重传不算。
                            if self.plugin_result.is_some() && stamp != self.plugin_stamp {
                                self.plugin_result = None;
                            }
                        }
                        // 纹理按名字复用：每帧新建一次 TextureHandle 就要在 GPU 上
                        // 分配/释放一次，`set` 只重传像素。
                        match &mut self.texture {
                            Some(tex) => tex.set(graded, TextureOptions::LINEAR),
                            None => {
                                self.texture = Some(ctx.load_texture(
                                    "preview.graded",
                                    graded,
                                    TextureOptions::LINEAR,
                                ));
                            }
                        }
                        self.texture_orig = if self.split {
                            original.map(|img| {
                                ctx.load_texture("preview.original", img, TextureOptions::LINEAR)
                            })
                        } else {
                            None
                        };
                        self.preview_has_orig = self.texture_orig.is_some();
                        self.preview_size = size;
                        // 剪切警告跟着全量帧走：草图帧不重算（见 rebuild 注释）。
                        if !sketch {
                            self.rebuild_clip_mask(ctx, stamp);
                        }
                        // 草图欠一张全量：还按着就等松手那一帧补，已经松手就当场补。
                        self.preview_owes_full = sketch;
                        if sketch && !self.preview_dragging {
                            self.preview_dirty = true;
                        }
                    }
                }
                Msg::ThumbsFailed(indices) => {
                    // 失败帧放行重试：标记清掉，下一帧 ensure_thumbs 会再试；
                    // 状态栏给一句总数，用户知道有几张读不了而不是「永远转圈」。
                    for i in indices {
                        self.thumb_pending.remove(&i);
                    }
                }
                Msg::Thumbs(items) => {
                    for t in items {
                        self.thumb_pending.remove(&t.index);
                        self.thumb_regrade.remove(&t.index);
                        self.thumb_src.insert(t.index, t.src);
                        let tex = ctx.load_texture(
                            format!("thumb.{}", t.index),
                            t.image,
                            TextureOptions::LINEAR,
                        );
                        self.thumbs.insert(
                            t.index,
                            Thumb {
                                texture: tex,
                                size: t.size,
                                grade: Some(t.grade),
                            },
                        );
                    }
                }
                Msg::Proxies(items) => {
                    for t in items {
                        self.proxy_pending.remove(&t.index);
                        // 纹理名必须与 128px 缩略图分开：`proxy.{i}` ≠ `thumb.{i}`，
                        // 否则两种尺寸会互相 load_texture 覆盖同一个槽。
                        //
                        // The texture name must differ from the 128px thumbnail:
                        // `proxy.{i}` ≠ `thumb.{i}`, otherwise the two sizes would
                        // overwrite the same slot through `load_texture`.
                        let tex = ctx.load_texture(
                            format!("proxy.{}", t.index),
                            t.image,
                            TextureOptions::LINEAR,
                        );
                        self.proxies.insert(
                            t.index,
                            Thumb {
                                texture: tex,
                                size: t.size,
                                grade: None,
                            },
                        );
                    }
                }
                Msg::LibThumbs(items) => {
                    for t in items {
                        self.lib_thumb_pending.remove(&t.path);
                        let tex = ctx.load_texture(
                            format!("lib-thumb.{}", t.path.display()),
                            t.image,
                            TextureOptions::LINEAR,
                        );
                        self.lib_thumbs.insert(
                            t.path,
                            Thumb {
                                texture: tex,
                                size: t.size,
                                grade: None,
                            },
                        );
                    }
                }
                Msg::Saved(path) => {
                    self.export_busy = false;
                    self.last_saved = Some(path.clone());
                    self.status = crate::i18n::tr_fmt("已保存 {}", &[&path]);
                }
                Msg::PreviewFailed(e) => {
                    self.preview_inflight = false;
                    self.error = Some(e);
                }
                Msg::Aligned { token, report } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.align_busy = false;
                    self.align_msg = Some(crate::i18n::tr_fmt(
                        "对齐完成 · {} 帧 · 裁切 {}×{} · {}s · 异常 {}",
                        &[
                            &report.offsets.len(),
                            &report.crop.w,
                            &report.crop.h,
                            &format!("{:.1}", report.elapsed_secs),
                            &report.issues.len(),
                        ],
                    ));
                    self.status = self.align_msg.clone().unwrap_or_default();
                    self.align_report = Some(report);
                    self.preview_dirty = true;
                }
                Msg::AlignFailed(e) => {
                    self.align_busy = false;
                    self.align_msg = Some(crate::i18n::tr_fmt("对齐失败：{}", &[&e]));
                    self.error = Some(crate::i18n::tr_fmt("对齐失败：{}", &[&e]));
                }
                Msg::Exported { token, path } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.export_busy = false;
                    self.export_stage = None;
                    self.last_saved = Some(path.display().to_string());
                    let skipped = self.export_skipped();
                    self.status = if skipped > 0 {
                        crate::i18n::tr_fmt(
                            "已导出 {}（续跑跳过 {} 帧）",
                            &[&path.display(), &skipped],
                        )
                    } else {
                        crate::i18n::tr_fmt("已导出 {}", &[&path.display()])
                    };
                }
                Msg::ExportFailed(e) => {
                    self.export_busy = false;
                    self.export_stage = None;
                    if e.contains("已取消") {
                        self.status = crate::i18n::t!("导出已取消").to_owned();
                    } else {
                        self.error = Some(crate::i18n::tr_fmt("导出失败：{}", &[&e]));
                    }
                }
                Msg::Merged {
                    token,
                    job,
                    kind,
                    result,
                } => {
                    // 只有当前这个 job 的回包能动忙碌标记：换序列时旧的取消旗已经立起来，
                    // 但旧线程可能还在两帧之间，`merge_busy` 要等它自己收尾。
                    //
                    // Only the current run's reply may touch the busy flag: on a sequence
                    // switch the old cancel flag is raised, but the old thread may still be
                    // between frames, so `merge_busy` waits for it to finish on its own.
                    if job != self.merge_job {
                        continue;
                    }
                    self.merge_busy = false;
                    self.merge_kind = None;
                    if token != self.reload_token {
                        continue;
                    }
                    // 面板那两行回显按任务种类分头落账：HDR 的失败不该顶掉全景的结果。
                    let (last, err) = match kind {
                        MergeKind::Hdr => (&mut self.hdr_last, &mut self.hdr_error),
                        MergeKind::Pano => (&mut self.pano_last, &mut self.pano_error),
                        MergeKind::Stack => (&mut self.stack_last, &mut self.stack_error),
                        MergeKind::TimeBlend => (&mut self.tb_last, &mut self.tb_error),
                        MergeKind::Focus => (&mut self.focus_last, &mut self.focus_error),
                    };
                    match result {
                        Ok(v) => {
                            let path = v["path"].as_str().unwrap_or_default().to_owned();
                            *last = Some(path.clone());
                            *err = None;
                            self.merge_result = Some(v);
                            self.merge_error = None;
                            self.status = crate::i18n::tr_fmt(
                                "{}完成：{}",
                                &[&crate::i18n::tr(kind.cn()), &path],
                            );
                        }
                        Err(e) => {
                            self.merge_result = None;
                            self.merge_error = Some(e.clone());
                            *last = None;
                            *err = Some(e.clone());
                            if e.contains("已取消") {
                                self.status =
                                    crate::i18n::tr_fmt("{}已取消", &[&crate::i18n::tr(kind.cn())]);
                            } else {
                                self.status =
                                    crate::i18n::tr_fmt("{}失败", &[&crate::i18n::tr(kind.cn())]);
                                self.error = Some(crate::i18n::tr_fmt(
                                    "{}失败：{}",
                                    &[&crate::i18n::tr(kind.cn()), &e],
                                ));
                            }
                        }
                    }
                }
                Msg::Checked { token, result } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.qa_busy = false;
                    match result {
                        Ok(v) => {
                            let bad = v.len();
                            self.qa_findings = v;
                            self.qa_run = true;
                            self.qa_error = None;
                            self.status = if bad == 0 {
                                "体检完成：没发现问题".to_owned()
                            } else {
                                crate::i18n::tr_fmt("体检完成：发现 {} 处", &[&bad])
                            };
                        }
                        Err(e) => {
                            self.qa_error = Some(e.clone());
                            if e.contains("已取消") {
                                self.status = "体检已取消".to_owned();
                            } else {
                                self.error = Some(crate::i18n::tr_fmt("体检失败：{}", &[&e]));
                            }
                        }
                    }
                }
                Msg::TimeblendPreview {
                    token,
                    key,
                    image,
                    frames,
                } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.tb_preview_inflight = false;
                    if let Some(data) = frames {
                        // 解码缓存照收：哪怕参数又变了，帧没变就还能复用。
                        self.tb_preview_data = Some(data);
                    }
                    if key != self.tb_preview_key {
                        continue; // 迟到的旧参数回包：不上屏
                    }
                    let img = *image;
                    self.tb_preview_img = Some(Arc::new(img.clone()));
                    // 纹理按名字复用：同参数只重传像素，不重新在 GPU 上分配。
                    match &mut self.tb_preview {
                        Some((k, tex)) if *k == key => tex.set(img, TextureOptions::LINEAR),
                        _ => {
                            self.tb_preview = Some((
                                key,
                                ctx.load_texture("timeblend.preview", img, TextureOptions::LINEAR),
                            ));
                        }
                    }
                }
                Msg::TimeblendPreviewFail { token, key, error } => {
                    if token != self.reload_token {
                        continue;
                    }
                    self.tb_preview_inflight = false;
                    if key == self.tb_preview_key {
                        self.tb_preview_error = Some(error);
                    }
                }
                Msg::QueueRow {
                    index,
                    status,
                    note,
                } => {
                    if let Some(item) = self.queue.get_mut(index) {
                        item.status = status;
                        item.note = note;
                        self.queue_current = Some(index);
                        self.status = crate::i18n::tr_fmt(
                            "队列第 {}/{} 段：{}",
                            &[
                                &(index + 1),
                                &self.queue.len(),
                                &crate::i18n::tr(status.label()),
                            ],
                        );
                    }
                    self.save_queue();
                }
                Msg::QueueFinished {
                    ok,
                    failed,
                    cancelled,
                } => {
                    self.queue_busy = false;
                    self.queue_current = None;
                    self.queue_stage.store(0, Ordering::Relaxed);
                    self.status = if cancelled {
                        crate::i18n::tr_fmt("队列已取消：完成 {} 段，失败 {} 段", &[&ok, &failed])
                    } else if failed > 0 {
                        crate::i18n::tr_fmt("队列完成：成功 {} 段，失败 {} 段", &[&ok, &failed])
                    } else {
                        crate::i18n::tr_fmt("队列完成：{} 段", &[&ok])
                    };
                    if failed > 0 && !cancelled {
                        self.error = Some(crate::i18n::tr_fmt(
                            "队列有 {} 段没渲成，逐行查看原因",
                            &[&failed],
                        ));
                    }
                    self.save_queue();
                }
                Msg::CullExtracted { token, result } => {
                    // 回包无论新旧都先把忙碌标记放下来：换过序列的迟到回包不许挡住
                    // 新序列再发一单复制。
                    //
                    // Every reply, stale or not, lowers the busy flag first: a late reply
                    // from a switched-away sequence must not block a fresh copy on the new one.
                    self.cull_export_busy = false;
                    if token != self.reload_token {
                        continue;
                    }
                    match result {
                        Ok(note) => {
                            history::note("cull.extract", note.clone());
                            self.status = note;
                            self.cull_note = None;
                        }
                        Err(e) => {
                            self.cull_note = Some(e);
                        }
                    }
                }
                Msg::AutoProbed {
                    token,
                    path,
                    result,
                } => {
                    // 迟到回包的解码结果照样进缓存（按路径对号，无害），忙碌标记
                    // 也照清；只有仍是当前帧的回包才当场重算一次曝光并刷新画面，
                    // 失败的回包把原因写进提示，别让「测光中…」挂到天荒地老。
                    //
                    // A late reply's decode still lands in the cache (keyed by path,
                    // harmless) and the busy flag still clears; only a reply for the frame
                    // still on screen recomputes the exposure and repaints, and a failed
                    // reply writes the reason into the note instead of leaving "metering…"
                    // hanging forever.
                    self.auto_probe_inflight = None;
                    if let Ok((px, w, h)) = &result {
                        self.auto_probe = Some((path.clone(), px.clone(), *w, *h));
                    }
                    if token != self.reload_token {
                        continue;
                    }
                    let current = self
                        .seq
                        .as_ref()
                        .and_then(|s| s.frames.get(self.selected))
                        .map(|f| f.path.clone());
                    if current.as_deref() == Some(path.as_path()) {
                        match result {
                            Err(e) => self.auto_note = Some(e),
                            Ok(_) => {
                                self.auto_exposure(ctx);
                                ctx.request_repaint();
                            }
                        }
                    }
                }
                Msg::AudioProbed { path, result } => {
                    self.audio_probe_busy = false;
                    // 只有仍在用这份配乐的回包才落账：换过配乐的迟到回包不覆盖
                    // 新文件的探针结果。
                    //
                    // Only a reply for the soundtrack still in use lands: a late reply for a
                    // switched file must not overwrite the new file's probe.
                    if self.render_audio.as_deref() != Some(path.as_path()) {
                        continue;
                    }
                    match result {
                        Ok(info) => self.audio_info = Some(info),
                        Err(e) => self.audio_probe_err = Some(e),
                    }
                }
                Msg::UpdateChecked { token, result } => {
                    // 不是最新那次任务的回包直接丢：连点两次「检查更新」时，先发的那次
                    // 可能后到，让它的结论盖掉新的那一份就等于「点第二次没反应」。
                    //
                    // A reply from anything but the newest job is dropped: when "check for
                    // updates" is clicked twice the first request may land last, and
                    // letting its conclusion overwrite the newer one looks like the second
                    // click did nothing.
                    if token != self.update_job {
                        continue;
                    }
                    let UpdateCheckOut {
                        check,
                        body,
                        sums_url,
                    } = *result;
                    self.update_phase = UpdatePhase::Idle;
                    // 失败也落账：窗口要显示那一行原因，菜单则因为 `installable()` 为假
                    // 而回到「检查更新…」。覆盖旧结论是有意的——「上次查到了新版本、
                    // 这次没查通」不该继续挂着一条装不了的提示。
                    //
                    // A failure is recorded too: the window shows the reason while the menu
                    // falls back to "check for updates" because `installable()` is false.
                    // Overwriting the old conclusion is deliberate -- "found an update last
                    // time, could not reach the server this time" must not keep advertising
                    // something that cannot be installed.
                    self.update_check = Some(check);
                    self.update_body = body;
                    self.update_sums_url = sums_url;
                    // 时间戳只在**完成**时写，失败也写：断网的机器每次启动都去敲一遍
                    // 网络，用户看不见，却要白等一次 DNS 超时。
                    //
                    // The stamp is written on **completion**, failures included: an offline
                    // machine knocking on the network every launch is invisible to the user
                    // and still wastes one DNS timeout.
                    self.last_update_check = Some(super::ops_update::unix_now());
                    self.settings_dirty = true;
                }
                Msg::UpdatePrepared { job, result } => {
                    if job != self.update_job {
                        continue;
                    }
                    match result {
                        Ok(prepared) => {
                            self.update_phase = UpdatePhase::Ready;
                            self.update_ready = Some(prepared);
                            self.update_error = None;
                        }
                        Err(e) => {
                            // 回落到 Idle 而不是留一个「下载中」：失败之后那颗按钮要变回
                            // 「下载并安装」（重试），否则进度条会停在一个永远不动的
                            // 百分比上。
                            //
                            // Fall back to Idle rather than staying "downloading": after a
                            // failure the button must become "download and install" again,
                            // or the bar freezes at a percentage that never moves.
                            self.update_phase = UpdatePhase::Idle;
                            self.update_ready = None;
                            self.update_error = Some(e);
                        }
                    }
                }
            }
        }

        // 插件任务：收进度、收结果。空闲时是空操作。
        self.plugin_poll(ctx);

        if self.align_busy
            || self.export_busy
            || self.queue_busy
            || self.merge_busy
            || self.qa_busy
            || self.cull_export_busy
            || self.audio_probe_busy
            || self.auto_probe_inflight.is_some()
            // 更新检查 / 下载期间要低频重绘：进度条的值在共享元组里，没有消息进来时
            // 只有这个心跳能让它动起来。`Ready` / `Idle` 不再重绘——那时画面是静的。
            //
            // A low-frequency repaint during check / download: the bar's value lives in the
            // shared tuple, and with no message arriving this heartbeat is what makes it
            // move. `Ready` / `Idle` do not repaint -- the picture is static then.
            || matches!(
                self.update_phase,
                UpdatePhase::Checking | UpdatePhase::Downloading
            )
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self.seq.is_some() {
            // egui 空闲时不会自己重绘；自动保存与后台进度都需要一个低频心跳。
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        if self.seq.is_some() && self.last_auto_save.elapsed() >= Duration::from_secs(5) {
            self.last_auto_save = Instant::now();
            // 只在内容真的变了、而且用户没在动手的时候写盘：序列化加写替换要几毫秒，
            // 正好落在拖滑块或播放的那一帧上就是一下可见的卡顿；空闲时每小时写一次
            // 和每 5 秒写一次，对盘上结果没有任何区别。
            let busy = self.playing || ctx.input(|i| i.pointer.any_down());
            if !busy && !self.project_unchanged() {
                self.write_project(true);
            }
        }

        // 回环控制：先把在线程里排队的请求在本帧里执行完，再让服务跟随当前序列。
        self.drain_control_events(ctx);
        self.sync_control(ctx);
    }

    /// 收尾控制通道：把在线程里排队的请求在本帧里执行掉。
    ///
    /// 迟到的命令不执行：客户端最多等 [`control::UI_TIMEOUT`] 就报超时，
    /// 一条排队超过这个上限才轮到的事件，说明客户端多半已经超时走人、
    /// 甚至可能重试了一遍。照常执行的话 export.video / queue.run /
    /// hdr.merge 这类批量写命令会被跑两遍（审查确认的重复执行路径）。
    /// 回复照发（对端通常已断开，发送失败无妨），至少语义是完整的。
    /// 独立成方法是为了让"迟到不执行"这条规则能脱离整帧 UI 直接测试。
    pub(super) fn drain_control_events(&mut self, ctx: &Context) {
        loop {
            let event = match self.control_rx.try_recv() {
                Ok(event) => event,
                Err(_) => break,
            };
            let waited = event.queued_at.elapsed();
            if waited > control::UI_TIMEOUT {
                let _ = event.reply.send(Err(crate::i18n::tr_fmt(
                    "命令 {} 排队 {} 秒才轮到执行，超过 {} 秒等待上限：为避免与客户端超时重试撞车，已放弃执行",
                    &[
                        &event.id,
                        &format!("{:.0}", waited.as_secs_f64()),
                        &format!("{:.0}", control::UI_TIMEOUT.as_secs_f64()),
                    ],
                )));
                continue;
            }
            let result = self.run_ui_command(ctx, &event.id, &event.args);
            let _ = event.reply.send(result);
        }
    }

    /// 保证控制服务与「当前有没有序列」一致：没序列就不写 control.json，
    /// 换了序列就重启服务（端口与令牌一起换新，旧文件由 Drop 清掉）。
    fn sync_control(&mut self, ctx: &Context) {
        if self.headless || !self.control_enabled {
            self.control = None;
            return;
        }
        // 每帧都要问一遍「还是这个序列吗」，所以这里不留分配：路径先只借来比一下，
        // 真要重启服务时才拷一份。
        let open = self
            .seq
            .as_ref()
            .map(|s| s.root.as_path())
            .filter(|path| path.is_dir());
        let same = match (&self.control, open) {
            (Some(server), Some(root)) => server.root() == root,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let root = open.map(|path| path.to_path_buf());
        // 先停旧的（Drop 会删自己写的那份 control.json）。
        self.control = None;
        let Some(root) = root else { return };

        let tx = Arc::new(Mutex::new(self.control_tx.clone()));
        let ctx = ctx.clone();
        let executor = move |id: &str, args: &Value| -> Result<Value, String> {
            let (reply, wait) = channel();
            {
                let sender = tx.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                sender
                    .send(ControlEvent {
                        id: id.to_owned(),
                        args: args.clone(),
                        queued_at: Instant::now(),
                        reply,
                    })
                    .map_err(|_| "界面线程已关闭，控制通道不可用".to_owned())?;
            }
            // 立刻唤醒界面：空闲的 egui 不会自己重绘，请求会一直排队。
            ctx.request_repaint();
            wait.recv_timeout(control::UI_TIMEOUT).map_err(|_| {
                crate::i18n::tr_fmt(
                    "命令 {} 超时：界面 {} 秒内没有响应",
                    &[&id, &format!("{:.0}", control::UI_TIMEOUT.as_secs_f64())],
                )
            })?
        };
        match ControlServer::start(&root, Arc::new(executor)) {
            Ok(server) => self.control = Some(server),
            Err(e) => self.error = Some(crate::i18n::tr_fmt("控制服务启动失败：{}", &[&e])),
        }
    }

    /// 给界面展示用的控制文件信息；没开服务时为 None。
    ///
    /// The control file info for display; None when the server is not running.
    pub fn control_file(&self) -> Option<&ControlFile> {
        self.control.as_ref().map(ControlServer::file)
    }

    pub fn control_path(&self) -> Option<PathBuf> {
        self.control.as_ref().map(|s| s.path().to_path_buf())
    }
}
