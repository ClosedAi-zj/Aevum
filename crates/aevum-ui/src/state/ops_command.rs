//! 命令执行器与片段参数：把 UI 命令派发到具体动作，以及去闪、镜头、配置文件、裁剪、渲染、EXIF 与序列信息这些片段参数的读写。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! The command executor and sequence-level parameters: Dispatching UI commands to concrete actions, plus reading and writing sequence parameters: deflicker, lens, profiles, crop, render, EXIF and sequence info.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ 命令执行器

    pub fn require_sequence(&self) -> Result<&Sequence, String> {
        self.seq
            .as_ref()
            .ok_or_else(|| "还没有打开序列：先用 sequence.open 指定帧文件夹".to_owned())
    }

    /// 唯一的命令执行入口：控制服务（回环）与右侧 AI 面板都走这里。
    ///
    /// 返回 JSON 给调用方；错误是可直接展示的中文文案。长任务
    /// （`align.run` / `export.video` / `export.stills` / `hdr.merge` / `pano.merge` /
    /// `ui.screenshot`）只负责启动后台线程并立即返回，进度用对应的 status 命令查询
    /// （合并是 `merge.status`，可用 `merge.cancel` 取消）。
    pub fn run_ui_command(
        &mut self,
        ctx: &Context,
        id: &str,
        args: &Value,
    ) -> Result<Value, String> {
        let result = self.dispatch_ui_command(ctx, id, args);
        // 成功的写操作追加进序列目录的 op_journal.jsonl；日志失败不影响命令本身。
        if result.is_ok()
            && journal::is_write_op(id)
            && let Some(root) = self.seq.as_ref().map(|s| s.root.clone())
        {
            let _ = journal::append(&root, "gui", id, args);
        }
        // 同时记进撤销历史：无头模式不会走界面帧，所以在这里就地落账。
        if result.is_ok() && id != "sequence.open" {
            self.history_note_command(id, args);
        }
        result
    }

    fn dispatch_ui_command(
        &mut self,
        ctx: &Context,
        id: &str,
        args: &Value,
    ) -> Result<Value, String> {
        match id {
            "sequence.info" => self.cmd_sequence_info(),
            "sequence.open" => self.cmd_sequence_open(ctx, args),
            "sequence.check" => self.cmd_sequence_check(ctx, args),
            "sequence.status" => Ok(self.qa_status_json()),
            "sequence.cancel" => {
                let busy = self.qa_busy;
                self.stop_qa();
                Ok(json!({
                    "cancelling": busy,
                    "note": if busy {
                        "已请求取消体检；在一批文件之间生效，清单保持上一次的结果".to_owned()
                    } else {
                        "当前没有进行中的体检".to_owned()
                    },
                }))
            }
            "library.add" => self.cmd_library_add(args),
            "library.remove" => self.cmd_library_remove(args),
            "library.list" => self.cmd_library_list(args),
            "library.open" => self.cmd_library_open(ctx, args),
            "library.organize" => self.cmd_library_organize(args),
            "library.keyword" => self.cmd_library_keyword(args),
            "library.metadata" => self.cmd_library_metadata(args),
            "library.stack" => self.cmd_library_stack(args),
            "collection.create" => self.cmd_collection_create(args),
            "collection.add" => self.cmd_collection_add(args),
            "collection.remove" => self.cmd_collection_remove(args),
            "collection.delete" => self.cmd_collection_delete(args),
            "collection.list" => self.cmd_collection_list(args),
            "frame.select" => self.cmd_frame_select(args),
            "ui.module" => self.cmd_ui_module(args),
            "ui.dock" => self.cmd_ui_dock(args),
            "grade.get" => Ok(self.grade_json()),
            "grade.auto" => {
                self.auto_exposure(ctx);
                let mut out = self.grade_json();
                if let Some(note) = &self.auto_note
                    && let Some(obj) = out.as_object_mut()
                {
                    obj.insert("note".to_owned(), json!(note));
                }
                Ok(out)
            }
            "grade.set" => self.cmd_grade_set(args),
            "grade.preset" => self.cmd_grade_preset(args),
            "grade.reset" => {
                self.reset_params();
                Ok(self.grade_json())
            }
            "grade.copy" => self.cmd_grade_copy(args),
            "hdr.merge" => self.cmd_hdr_merge(ctx, args),
            "pano.merge" => self.cmd_pano_merge(ctx, args),
            "cull.evaluate" => self.cmd_cull_evaluate(args),
            "cull.extract" => self.cmd_cull_extract(ctx, args),
            "stack.merge" => self.cmd_stack_merge(ctx, args),
            "timeblend.merge" => self.cmd_timeblend_merge(ctx, args),
            "focus.merge" => self.cmd_focus_merge(ctx, args),
            "merge.status" => Ok(self.merge_status_json()),
            "merge.cancel" => {
                let busy = self.merge_busy;
                let kind = self.merge_kind.map(MergeKind::cn);
                self.stop_merge();
                Ok(json!({
                    "cancelling": busy,
                    "note": match (busy, kind) {
                        (true, Some(k)) => crate::i18n::tr_fmt("已请求取消{}；在两帧之间生效，已写下的帧不会删除", &[&k]),
                        (true, None) => "已请求取消；在两帧之间生效".to_owned(),
                        (false, _) => "当前没有进行中的合并".to_owned(),
                    },
                }))
            }
            "mask.list" => Ok(self.mask_list_json()),
            "mask.add" => self.cmd_mask_add(args),
            "mask.remove" => self.cmd_mask_remove(args),
            "mask.update" => self.cmd_mask_update(args),
            "mask.component" => self.cmd_mask_component(args),
            "align.run" => self.cmd_align_run(ctx, args),
            "align.status" => Ok(self.align_status_json()),
            "deflicker.set" => self.cmd_deflicker_set(args),
            "lens.set" => self.cmd_lens_set(args),
            "profile.set" => self.cmd_profile_set(args),
            "crop.set" => self.cmd_crop_set(args),
            "render.set" => self.cmd_render_set(args),
            "retime.set" => self.cmd_retime_set(args),
            "retime.status" => Ok(self.retime_status_json()),
            "motion.set" => self.cmd_motion_set(args),
            "motion.status" => Ok(self.motion_status_json()),
            "export.video" | "export.stills" => self.cmd_export(ctx, id, args),
            "export.status" => Ok(self.export_status_json()),
            "export.cancel" => {
                let busy = self.export_busy;
                self.stop_export();
                Ok(json!({
                    "cancelling": busy,
                    "note": if busy { "已请求取消；已写下的素材不会删除" } else { "当前没有进行中的导出" },
                }))
            }
            "project.save" => self.cmd_project_save(),
            "project.get" => self.cmd_project_get(),
            "project.metadata" => self.cmd_project_metadata(args),
            "journal.list" => self.cmd_journal_list(args),
            "history.list" => Ok(self.history_json()),
            "history.undo" => self.cmd_history_undo(),
            "history.redo" => self.cmd_history_redo(),
            "history.jump" => self.cmd_history_jump(args),
            "xmp.import" => self.cmd_xmp_import(args),
            "xmp.export" => self.cmd_xmp_export(args),
            "cube.import" => self.cmd_cube_import(args),
            "cube.export" => self.cmd_cube_export(args),
            "audio.config" => self.configure_audio(args),
            "queue.list" => Ok(self.queue_json()),
            "queue.add" => self.cmd_queue_add(args),
            "queue.remove" => self.cmd_queue_remove(args),
            "queue.clear" => self.cmd_queue_clear(),
            "queue.run" => self.cmd_queue_run(ctx, args),
            "queue.status" => Ok(self.queue_status_json()),
            "queue.cancel" => {
                let busy = self.queue_busy;
                self.stop_queue();
                Ok(json!({
                    "cancelling": busy,
                    "note": if busy { "已请求取消；已渲完的段保留，未开始的仍待渲" } else { "队列没有在跑" },
                }))
            }
            "ui.screenshot" => self.cmd_screenshot(ctx, args),
            other => Err(crate::i18n::tr_fmt(
                "未知命令：{}（用 system.commands 或 AI 面板的命令清单查看可用命令）",
                &[&other],
            )),
        }
    }

    /// 去闪设置 + 最近一次分析摘要；`sequence.info` 与 `deflicker.set` 共用一份口径。
    fn deflicker_json(&self) -> Value {
        let analysis = match &self.analysis {
            Some(analysis) => json!({
                "analyzed": true,
                "frames": analysis.stats.frames,
                "valid": analysis.stats.valid,
                "jitter_before": round6(analysis.stats.jitter_before),
                "jitter_after": round6(analysis.stats.jitter_after),
                "channels": analysis.stats.channels,
                "color_before": round6(analysis.stats.color_before),
                "color_after": round6(analysis.stats.color_after),
                "max_ev": round6(analysis.stats.max_ev),
            }),
            None => json!({"analyzed": false}),
        };
        json!({
            "window": self.def_window,
            "strength": self.def_strength,
            "multi": self.def_multi,
            "channels": self.def_channels,
            "align_exif": self.def_align_exif,
            "ev_frames": self.exif_ev_frames(),
            "measured": self.def_means.iter().filter(|m| m.is_some()).count(),
            "applied": self.corr.iter().filter(|v| v.abs() > 0.001).count(),
            "analysis": analysis,
        })
    }

    /// 读或改去闪设置；任何改动都立刻重算，回一条与 `sequence.info` 同口径的摘要。
    fn cmd_deflicker_set(&mut self, args: &Value) -> Result<Value, String> {
        if let Some(v) = args.get("window") {
            let x = v.as_f64().ok_or("window 必须是数字（奇数窗口）")?;
            if !x.is_finite() || !(3.0..=199.0).contains(&x) {
                return Err("window 必须在 3..199".to_owned());
            }
            self.def_window = ((x.round() as i64).clamp(3, 199) | 1) as f64;
        }
        if let Some(v) = args.get("strength") {
            let x = v.as_f64().ok_or("strength 必须是数字（百分比）")?;
            if !x.is_finite() || !(0.0..=150.0).contains(&x) {
                return Err("strength 必须在 0..150".to_owned());
            }
            self.def_strength = x;
        }
        if let Some(v) = args.get("multi") {
            self.def_multi = v.as_bool().ok_or("multi 必须是布尔")?;
        }
        if let Some(v) = args.get("channels") {
            self.def_channels = v.as_bool().ok_or("channels 必须是布尔")?;
        }
        if let Some(v) = args.get("align_exif") {
            self.def_align_exif = v.as_bool().ok_or("align_exif 必须是布尔")?;
        }
        self.recompute_deflicker();
        Ok(self.deflicker_json())
    }

    /// 镜头配置文件摘要：`lens.set` 的回执与面板显示同一份口径。
    fn lens_profile_json(&self) -> Value {
        json!({
            "profile": self.params.lens_profile.as_ref().map(|c| c.to_json()),
            "lcp": self.params.lcp_profile,
            "note": self.lens_profile_note,
        })
    }

    /// 打开序列后按首帧 EXIF 匹配镜头配置文件。lensfun 数据库在磁盘上找，
    /// 没有库、没接镜头、EXIF 缺信息都静默降级——手动三项照旧，只留一行说明。
    ///
    /// After opening a sequence, match a lens profile from the first frame's EXIF. The
    /// lensfun database is looked up on disk; a missing database, an unattached lens, or
    /// missing EXIF all degrade silently -- the three manual values stay as they are, with one
    /// explanatory line.
    pub fn match_lens_profile(&mut self) {
        self.lens_profile_note.clear();
        let Some(ex) = self.exif.first() else {
            self.lens_profile_note = "没有 EXIF，无从匹配镜头配置".to_owned();
            return;
        };
        let Some(make) = ex.make().map(str::to_owned) else {
            self.lens_profile_note = "EXIF 里没有机身厂商，无从匹配镜头配置".to_owned();
            return;
        };
        let model = ex.model().unwrap_or("").to_owned();
        let lens = ex.lens().unwrap_or("").to_owned();
        let focal = ex.focal_mm().unwrap_or(0.0);
        let aperture = ex.fnum();
        self.lens_match_apply(&make, &model, &lens, focal, aperture);
    }

    /// 用给定的 EXIF 三件套跑匹配并落进参数。`match_lens_profile` 的主体，
    /// 独立出来供测试直呼。
    ///
    /// Run the match with the given EXIF triple and write it into the parameters. The body of
    /// `match_lens_profile`, split out so tests can call it directly.
    pub fn lens_match_apply(
        &mut self,
        make: &str,
        model: &str,
        lens: &str,
        focal: f64,
        aperture: Option<f64>,
    ) {
        let db = aevum_core::lensdb::load_auto();
        if db.lenses.is_empty() {
            self.lens_profile_note =
                "没找到 lensfun 数据库（设 AEVUM_LENSFUN，或把库放 exe 旁的 lensfun/ 目录）"
                    .to_owned();
            return;
        }
        match aevum_core::lensdb::match_correction(&db, make, model, lens, focal, aperture) {
            Some(corr) => {
                self.lens_profile_note = crate::i18n::tr_fmt(
                    "已匹配 {}（{}mm · 裁切 {}）",
                    &[&corr.lens, &corr.focal_mm, &format!("{:.1}", corr.crop)],
                );
                self.lens_profile_saved = Some(corr.clone());
                self.params.lens_profile = Some(corr);
                self.preview_dirty = true;
            }
            None => {
                self.lens_profile_note = crate::i18n::tr_fmt(
                    "数据库里没有 {} {} 的标定，畸变与暗角仍走手动三项",
                    &[&make, &lens],
                );
            }
        }
    }

    /// 读或开关镜头配置文件校正：`enabled` 开关（关掉暂存、再开恢复）、
    /// `rematch: true` 强制按当前 EXIF 重新匹配；不带参数即查询。
    pub(super) fn cmd_lens_set(&mut self, args: &Value) -> Result<Value, String> {
        if args
            .get("rematch")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.match_lens_profile();
        }
        if let Some(v) = args.get("enabled") {
            let on = v.as_bool().ok_or("enabled 必须是布尔")?;
            if on {
                if self.params.lens_profile.is_none()
                    && let Some(c) = self.lens_profile_saved.take()
                {
                    self.params.lens_profile = Some(c);
                    self.preview_dirty = true;
                }
            } else {
                self.lens_profile_saved = self.params.lens_profile.take();
                self.preview_dirty = true;
            }
        }
        Ok(self.lens_profile_json())
    }

    /// 相机/镜头配置摘要：面板与 `profile.set` 共用一份口径。
    fn profile_json(&self) -> Value {
        json!({
            "dcp": {
                "path": self.params.dcp_profile,
                "name": self.dcp_name,
                "note": self.dcp_note,
            },
            "lcp": {
                "correction": self.params.lcp_profile.as_ref().map(|c| c.to_json()),
                "note": self.lcp_note,
            },
        })
    }

    /// 从首帧 EXIF 折出 LCP 的匹配请求。
    fn exif_lens_request(&self) -> Result<aevum_core::lcp::Request, String> {
        let ex = self
            .exif
            .first()
            .ok_or_else(|| "没有 EXIF，无从匹配镜头配置".to_owned())?;
        Ok(aevum_core::lcp::Request {
            make: ex.make().unwrap_or("").to_owned(),
            model: ex.model().unwrap_or("").to_owned(),
            lens: ex.lens().unwrap_or("").to_owned(),
            focal_mm: ex.focal_mm().unwrap_or(0.0),
            focal35_mm: None,
            aperture: ex.fnum(),
        })
    }

    /// 按 EXIF 机身找 DCP；找不到只留一行说明，不改动当前参数。
    ///
    /// Find a DCP by the EXIF camera body; not finding one only leaves an explanatory line
    /// and changes no current parameter.
    pub fn match_dcp_profile(&mut self) {
        self.dcp_note.clear();
        let Some(ex) = self.exif.first() else {
            self.dcp_note = "没有 EXIF，无从匹配相机配置".to_owned();
            return;
        };
        let make = ex.make().unwrap_or("").to_owned();
        let model = ex.model().unwrap_or("").to_owned();
        match aevum_core::dcp::auto_match(&make, &model) {
            Ok(p) => {
                self.dcp_note = crate::i18n::tr_fmt("已套用「{}」（{}）", &[&p.name, &p.camera]);
                self.dcp_name = p.name.clone();
                self.params.dcp_profile = p.path;
                self.preview_dirty = true;
            }
            Err(e) => self.dcp_note = e,
        }
    }

    /// 指定一份 `.dcp`。机型对不上就拒绝——配置拿错比不套更糟。
    ///
    /// Specify a `.dcp`. Rejected when the camera model does not match -- the wrong profile
    /// is worse than none.
    pub fn set_dcp_profile(&mut self, path: &str) -> Result<(), String> {
        let p = aevum_core::dcp::Profile::load(path)?;
        if let Some(ex) = self.exif.first() {
            let make = ex.make().unwrap_or("");
            let model = ex.model().unwrap_or("");
            if !model.is_empty() && !p.matches(make, model) {
                return Err(crate::i18n::tr_fmt(
                    "配置是给「{}」的，当前机型是「{} {}」",
                    &[&p.camera, &make, &model],
                ));
            }
        }
        self.dcp_name = p.name.clone();
        self.dcp_note = crate::i18n::tr_fmt("已套用「{}」（{}）", &[&p.name, &p.camera]);
        self.params.dcp_profile = p.path.clone();
        self.preview_dirty = true;
        Ok(())
    }

    pub fn clear_dcp_profile(&mut self) {
        self.params.dcp_profile.clear();
        self.dcp_name.clear();
        self.dcp_note.clear();
        self.preview_dirty = true;
    }

    /// 按 EXIF 在默认目录里找 LCP 并折出校正。
    ///
    /// Find an LCP in the default directories by EXIF and fold its correction in.
    pub fn match_lcp_profile(&mut self) {
        self.lcp_note.clear();
        let req = match self.exif_lens_request() {
            Ok(req) => req,
            Err(e) => {
                self.lcp_note = e;
                return;
            }
        };
        match aevum_core::lcp::auto_match(&req) {
            Ok((path, corr)) => {
                let file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.lcp_note = crate::i18n::tr_fmt(
                    "已套用 {}（{} · {}mm）",
                    &[&file, &corr.lens, &format!("{:.0}", corr.focal35_mm)],
                );
                self.params.lcp_profile = Some(corr);
                self.preview_dirty = true;
            }
            Err(e) => self.lcp_note = e,
        }
    }

    /// 指定一份 `.lcp`，按当前 EXIF 选帧。
    ///
    /// Specify a `.lcp`, picking the frame by the current EXIF.
    pub fn set_lcp_profile(&mut self, path: &str) -> Result<(), String> {
        let file = aevum_core::lcp::load(path)?;
        let req = self.exif_lens_request()?;
        let corr = aevum_core::lcp::match_correction(&file, &req)?;
        self.lcp_note = crate::i18n::tr_fmt(
            "已套用 {}（{} · {}mm）",
            &[
                &file.profile,
                &corr.lens,
                &format!("{:.0}", corr.focal35_mm),
            ],
        );
        self.params.lcp_profile = Some(corr);
        self.preview_dirty = true;
        Ok(())
    }

    pub fn clear_lcp_profile(&mut self) {
        self.params.lcp_profile = None;
        self.lcp_note.clear();
        self.preview_dirty = true;
    }

    /// 读取或套用相机/镜头配置。`kind` 为 `dcp` 或 `lcp`：
    /// `path` 指定文件、`auto` 按 EXIF 自动找、`clear` 清掉；都不带即查询。
    fn cmd_profile_set(&mut self, args: &Value) -> Result<Value, String> {
        let kind = match args.get("kind").and_then(Value::as_str) {
            Some("dcp") => "dcp",
            Some("lcp") => "lcp",
            Some(other) => {
                return Err(crate::i18n::tr_fmt(
                    "未知的配置类型：{}（dcp 或 lcp）",
                    &[&other],
                ));
            }
            None => return Err("缺少 kind（dcp 或 lcp）".to_owned()),
        };
        if args.get("clear").and_then(Value::as_bool).unwrap_or(false) {
            if kind == "dcp" {
                self.clear_dcp_profile();
            } else {
                self.clear_lcp_profile();
            }
            return Ok(self.profile_json());
        }
        if let Some(path) = args.get("path").and_then(Value::as_str) {
            let path = path.trim();
            if path.is_empty() {
                return Err("path 不能为空".to_owned());
            }
            if kind == "dcp" {
                self.set_dcp_profile(path)?;
            } else {
                self.set_lcp_profile(path)?;
            }
            return Ok(self.profile_json());
        }
        if args.get("auto").and_then(Value::as_bool).unwrap_or(false) {
            if kind == "dcp" {
                self.match_dcp_profile();
            } else {
                self.match_lcp_profile();
            }
        }
        Ok(self.profile_json())
    }

    /// 取景框摘要：`crop.set` 与 `sequence.info` 共用一份口径。
    /// `manual` 是用户拖出来的框（没拖过为 null），`effective` 是叠上拉直安全框、
    /// 对齐安全框与画幅比例后、真正会导出的那个。
    fn crop_json(&self) -> Value {
        let (w, h) = self.resolution.unwrap_or((0, 0));
        json!({
            "frame": {"width": w, "height": h},
            "aspect": self.aspect.label(),
            "rotate": self.rotate,
            "manual": self.user_crop.map(rect_json),
            "effective": self.effective_crop().map(rect_json),
            "editing": self.crop_editing(),
        })
    }

    /// 读或改写取景框。`x/y/width/height` 一起给就整体替换（源像素，几何规则交给
    /// `aevum_core::crop`：取偶、夹进画面）；`reset` 取消手动框和拉直；`aspect` 换画幅比例；
    /// `rotate` 拉直（度，正角=画面逆时针，±45 内）；什么都不给就是读回当前取景。
    fn cmd_crop_set(&mut self, args: &Value) -> Result<Value, String> {
        if let Some(v) = args.get("aspect") {
            let text = v.as_str().ok_or("aspect 必须是字符串，例如 \"16:9\"")?;
            self.set_aspect(aspect_arg(text)?);
        }
        if let Some(v) = args.get("rotate") {
            let deg = v.as_f64().ok_or("rotate 必须是数字（度）")?;
            if !deg.is_finite() || deg.abs() > 45.0 {
                return Err("拉直角度要在 -45..45 之间".to_owned());
            }
            self.set_rotate(deg);
        }
        if args.get("reset").and_then(Value::as_bool).unwrap_or(false) {
            self.crop_reset();
            self.set_rotate(0.0);
            return Ok(self.crop_json());
        }
        let keys = ["x", "y", "width", "height"];
        if keys.iter().any(|k| args.get(*k).is_some()) {
            if !keys.iter().all(|k| args.get(*k).is_some()) {
                return Err(
                    "取景框要一次性给全 x / y / width / height，缺一个只会画出半套框".to_owned(),
                );
            }
            let (fw, fh) = self.resolution.ok_or("还没有打开序列，无从取景")?;
            let mut v = [0u32; 4];
            for (i, k) in keys.iter().enumerate() {
                let x = args[*k]
                    .as_f64()
                    .ok_or_else(|| crate::i18n::tr_fmt("{} 必须是数字（源像素）", &[&k]))?;
                if !x.is_finite() || x < 0.0 || x > f64::from(fw.max(fh)) {
                    return Err(crate::i18n::tr_fmt(
                        "{} 超出画面范围（0..{}）",
                        &[&k, &fw.max(fh)],
                    ));
                }
                v[i] = x.round() as u32;
            }
            let next = crop::norm_rect(
                f64::from(v[0]),
                f64::from(v[1]),
                f64::from(v[0] + v[2]),
                f64::from(v[1] + v[3]),
                fw,
                fh,
            );
            self.user_crop = Some(self.clamp_to_view(next));
            self.crop_drag = None;
            self.preview_dirty = true;
        }
        Ok(self.crop_json())
    }

    /// 成片输出摘要：`render.set` 的回执与查询用同一份口径。
    /// `*_applies` 那几项是诚实开关——容器不吃的参数摆出来就要说清它现在不起作用。
    fn render_json(&self) -> Value {
        json!({
            "mode": mode_name(self.render_mode),
            "format": self.render_format.key(),
            "format_cn": self.render_format.cn(),
            "quality": self.render_quality,
            "quality_applies": self.render_format.lossy(),
            "keep_exif": self.render_keep_exif,
            "exif_applies": self.render_format.supports_exif(),
            "xmp_sidecar": self.render_xmp_sidecar,
            // 旁车是静帧的事：视频模式导出的中间帧不发旁车。
            "sidecar_applies": self.render_mode == OutputMode::Stills,
            "color_space": self.render_color_space.key(),
            "dynamic": self.render_dynamic.key(),
            // HDR 只对 AVIF 成立，容器装不下时面板该让人看出「这个组合不成立」。
            "dynamic_fits": self.render_dynamic.fits(self.render_format),
            "width": self.output_w,
            "height": self.output_h,
            "sharpen_for": self.render_sharpen_for.key(),
            "sharpen_strength": self.render_sharpen_strength.key(),
            // 水印整块照 `Watermark` 自己的形状吐出去：查到的那份可以原样写回来。
            "watermark": self.render_watermark,
            "watermark_applies": self.render_watermark.is_on(),
            "watermark_spots": Spot::ALL.iter().map(|s| s.key()).collect::<Vec<_>>(),
            "watermark_kinds": WatermarkKind::ALL.iter().map(|k| k.key()).collect::<Vec<_>>(),
            "out_dir": self.render_out_dir.display().to_string(),
            "formats": StillFormat::ALL.iter().map(|f| f.key()).collect::<Vec<_>>(),
            "sharpen_uses": OutputSharpen::ALL.iter().map(|s| s.key()).collect::<Vec<_>>(),
        })
    }

    /// 落一个容器：视频模式的中间帧只能无损，撞上有损档直接报错，别让 core 的
    /// 守卫等到导出那一步才说话。
    pub(super) fn set_render_format(&mut self, format: StillFormat) -> Result<(), String> {
        if self.render_mode == OutputMode::Video && !format.frames_ok() {
            return Err(crate::i18n::tr_fmt(
                "{} 只能出静帧；视频的中间帧要 PNG 或 TIFF（先把 mode 改成 stills）",
                &[&crate::i18n::tr(format.cn())],
            ));
        }
        self.render_format = format;
        Ok(())
    }

    /// 改水印：只写要改的那几键，其余保持面板当前值。
    ///
    /// 合并是把当前设定先摊平成 map 再覆盖——`Watermark` 上的 `serde(default)` 只补
    /// 结构里根本没有的键，所以「改个不透明度把正文清空」这种惊喜不会出现，写进来的
    /// 键名也和工程文件、`render.set` 查询回执用同一套词。
    fn patch_watermark(&mut self, value: &Value) -> Result<(), String> {
        let incoming = value
            .as_object()
            .ok_or("watermark 必须是对象（enabled / kind / text / image / spot / opacity / size / margin / color）")?;
        let base = serde_json::to_value(&self.render_watermark)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let mut merged = base.clone();
        for (key, v) in incoming {
            if !base.contains_key(key) {
                let keys = base
                    .keys()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ");
                return Err(crate::i18n::tr_fmt(
                    "未知的水印参数 {}；可用 {}",
                    &[&key, &keys],
                ));
            }
            merged.insert(key.clone(), v.clone());
        }
        let next: Watermark = serde_json::from_value(Value::Object(merged))
            .map_err(|e| crate::i18n::tr_fmt("水印参数不对：{}", &[&e]))?;
        if incoming.contains_key("image")
            && !next.image.as_os_str().is_empty()
            && !next.image.is_file()
        {
            let shown = next.image.display();
            return Err(crate::i18n::tr_fmt(
                "水印图片不存在：{}（传空串表示不换/清空）",
                &[&shown],
            ));
        }
        for (key, slot, max) in [
            ("opacity", i32::from(next.opacity), 100),
            ("size", i32::from(next.size), 40),
            ("margin", i32::from(next.margin), 40),
        ] {
            if incoming.contains_key(key) && slot > max {
                return Err(crate::i18n::tr_fmt(
                    "{} 要在 0..{}（相对输出尺寸的百分比）",
                    &[&key, &max],
                ));
            }
        }
        self.render_watermark = next;
        Ok(())
    }

    /// 读或改成片输出设置：容器、质量、EXIF 搬运、色域、规格、输出锐化、水印、目录。
    fn cmd_render_set(&mut self, args: &Value) -> Result<Value, String> {
        // 模式与容器是一对：先各自解析，凑不成合法组合就整条拒绝，
        // 别改一半留下「视频模式 + 有损中间帧」这种导不出、又没记进历史的态。
        let mode = match args.get("mode") {
            Some(v) => {
                let raw = v.as_str().ok_or("mode 必须是字符串（stills / video）")?;
                match raw.trim().to_ascii_lowercase().as_str() {
                    "stills" | "still" | "images" => OutputMode::Stills,
                    "video" | "mp4" => OutputMode::Video,
                    other => {
                        return Err(crate::i18n::tr_fmt(
                            "未知输出模式 {}；可用 stills / video",
                            &[&other],
                        ));
                    }
                }
            }
            None => self.render_mode,
        };
        let format = match args.get("format") {
            Some(v) => {
                let raw = v.as_str().ok_or("format 必须是字符串")?;
                StillFormat::parse(raw).ok_or_else(|| {
                    crate::i18n::tr_fmt(
                        "未知容器 {}；可用 png8 / tiff16 / jpeg / webp / avif",
                        &[&raw],
                    )
                })?
            }
            None => self.render_format,
        };
        if mode == OutputMode::Video && !format.frames_ok() {
            return Err(crate::i18n::tr_fmt(
                "{} 只能出静帧；视频的中间帧要 PNG 或 TIFF（同一条命令里把 format 一起改掉）",
                &[&crate::i18n::tr(format.cn())],
            ));
        }
        self.render_mode = mode;
        self.render_format = format;
        if let Some(q) = opt_u32(args, "quality")? {
            if !(1..=100).contains(&q) {
                return Err("quality 要在 1..100".to_owned());
            }
            self.render_quality = q as u8;
        }
        if let Some(v) = args.get("keep_exif") {
            self.render_keep_exif = v.as_bool().ok_or("keep_exif 必须是布尔")?;
        }
        if let Some(v) = args.get("xmp_sidecar") {
            self.render_xmp_sidecar = v.as_bool().ok_or("xmp_sidecar 必须是布尔")?;
        }
        if let Some(v) = args.get("resume") {
            self.render_resume = v.as_bool().ok_or("resume 必须是布尔")?;
        }
        if let Some(v) = args.get("sharpen_for") {
            let raw = v.as_str().ok_or("sharpen_for 必须是字符串")?;
            self.render_sharpen_for = OutputSharpen::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt(
                    "未知锐化用途 {}；可用 off / screen / glossy / matte",
                    &[&raw],
                )
            })?;
        }
        if let Some(v) = args.get("sharpen_strength") {
            let raw = v.as_str().ok_or("sharpen_strength 必须是字符串")?;
            self.render_sharpen_strength = SharpenStrength::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt("未知锐化强度 {}；可用 low / standard / high", &[&raw])
            })?;
        }
        if let Some(raw) = opt_str(args, "color_space")? {
            self.render_color_space = ColorSpace::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt("未知色域 {}；可用 srgb / p3 / rec2020", &[&raw])
            })?;
        }
        if let Some(raw) = opt_str(args, "dynamic")? {
            self.render_dynamic = OutputDynamic::parse(raw).ok_or_else(|| {
                crate::i18n::tr_fmt("未知动态范围 {}；可用 sdr / hdr-pq", &[&raw])
            })?;
            // 命令行给 HDR 时同样补齐：色域自动 rec2020、容器自动 avif。
            self.apply_dynamic();
        }
        if let Some(v) = args.get("watermark") {
            self.patch_watermark(v)?;
        }
        match (opt_u32(args, "width")?, opt_u32(args, "height")?) {
            (Some(width), Some(height)) => {
                if width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1 {
                    return Err(crate::i18n::tr_fmt(
                        "width/height 必须是偶数且不小于 2：{}×{}",
                        &[&width, &height],
                    ));
                }
                self.output_w = width;
                self.output_h = height;
            }
            (None, None) => {}
            _ => return Err("width 与 height 需同时给出（或都不给，保持当前规格）".to_owned()),
        }
        if let Some(raw) = opt_str(args, "out_dir")? {
            if raw.trim().is_empty() {
                return Err("out_dir 不能为空；省略则保持当前目录".to_owned());
            }
            let dir = PathBuf::from(raw);
            if let Some(seq) = &self.seq {
                ensure_out_dir_safe(&seq.root, &dir)?;
            }
            self.render_out_dir = dir;
        }
        Ok(self.render_json())
    }

    /// 当前帧的拍摄参数。整块读不到（PNG、无 EXIF 的处理过的图）返回 `null`，
    /// 面板和 agent 都按「这条没有」处理，不摆一排空值。
    ///
    /// The current frame's shooting parameters. Returns `null` when the whole block is
    /// unreadable (PNG, or a processed image with no EXIF); the panel and the agent both treat
    /// that as "this one has none" rather than showing a row of empty values.
    pub fn exif_json(&self) -> Value {
        let Some(e) = self.exif.get(self.selected) else {
            return Value::Null;
        };
        if e == &Exif::default() {
            return Value::Null;
        }
        json!({
            "make": e.make(),
            "model": e.model(),
            "lens": e.lens(),
            "datetime": e.datetime(),
            "iso": e.iso(),
            "shutter": e.shutter_text(),
            "shutter_sec": e.exposure_sec().map(round6),
            "aperture": e.fnum().map(|v| (v * 10.0).round() / 10.0),
            "focal_mm": e.focal_mm().map(|v| (v * 10.0).round() / 10.0),
            "ev": e.ev().map(round6),
            "orientation": e.orientation,
        })
    }

    pub(super) fn cmd_sequence_info(&self) -> Result<Value, String> {
        let seq = self.require_sequence()?;
        let resolution = match self.resolution {
            Some((w, h)) => json!({"width": w, "height": h}),
            None => Value::Null,
        };
        let frame_name = seq
            .frames
            .get(self.selected)
            .map(|f| f.name.clone())
            .unwrap_or_default();
        let issues: Vec<Value> = self
            .issues
            .iter()
            .take(50)
            .map(|(i, msg)| json!({"frame": i + 1, "error": msg}))
            .collect();
        let align = match &self.align_report {
            Some(report) => json!({
                "available": true,
                "reference": self.align_ref + 1,
                "estimator": estimator_name(self.align_estimator),
                "frames": report.offsets.len(),
                "crop": rect_json(report.crop),
                "issues": report.issues.len(),
                "elapsed_secs": round6(report.elapsed_secs),
            }),
            None => json!({
                "available": false,
                "reference": self.align_ref + 1,
                "estimator": estimator_name(self.align_estimator),
            }),
        };
        let project_path = project::project_path(&seq.root);
        Ok(json!({
            "root": seq.root.display().to_string(),
            "name": seq.display_name(),
            "frames": seq.len(),
            "resolution": resolution,
            "missing_frames": issues,
            "missing_count": self.issues.len(),
            "measured": self.metrics.iter().filter(|m| m.is_some()).count(),
            "measuring": self.measuring.is_some(),
            "selected": self.selected + 1,
            "frame_name": frame_name,
            "exif": self.exif_json(),
            "module": self.module.cn(),
            "module_index": self.module.index(),
            "preset": self.preset,
            "deflicker": self.deflicker_json(),
            "align": align,
            "crop": self.crop_json(),
            "export": {
                "busy": self.export_busy,
                "mode": mode_name(self.render_mode),
                "last_saved": self.last_saved,
            },
            "project": {
                "path": project_path.display().to_string(),
                "exists": project_path.is_file(),
            },
        }))
    }

    fn cmd_sequence_open(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let raw = arg_str(args, "root")?;
        let path = PathBuf::from(raw);
        if !path.is_dir() && !path.is_file() {
            return Err(crate::i18n::tr_fmt("路径不存在：{}", &[&path.display()]));
        }
        // open 只在真正换成新序列时才递增 reload_token；用它判断是否打开成功，
        // 否则失败时会误把旧序列当成新打开的序列返回给 agent。
        let token = self.reload_token;
        self.open(ctx, path);
        if self.reload_token == token {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "打开失败：这里没有可用的图片".to_owned()));
        }
        let mut info = self.cmd_sequence_info()?;
        if let Some(object) = info.as_object_mut() {
            object.insert("opened".to_owned(), json!(true));
        }
        Ok(info)
    }
}
