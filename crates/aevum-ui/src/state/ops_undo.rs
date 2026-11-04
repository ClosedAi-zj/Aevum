//! 撤销与重做：历史快照的存与取、命令标签、撤销栈的步进。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Undo and redo: Storing and restoring history snapshots, command labels, and stepping the undo stack.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ 撤销 / 重做

    /// 抓一份可撤销状态。视图（缩放、面板、当前帧）与只读结果（测光、直方图）不在
    /// 里面，见 [`Snapshot`]。
    ///
    /// Capture an undoable state. View state (zoom, panels, current frame) and read-only
    /// results (metering, histogram) are not part of it; see [`Snapshot`].
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            params: self.params.clone(),
            preset: self.preset.clone(),
            def_window: self.def_window,
            def_strength: self.def_strength,
            def_multi: self.def_multi,
            def_channels: self.def_channels,
            def_align_exif: self.def_align_exif,
            aspect: self.aspect,
            crop: self.user_crop,
            rotate: self.rotate,
            grid: self.grid,
            cross: self.cross,
            split: self.split,
            keyframes: self.keyframes.clone(),
            copies: self.copies.clone(),
            ratings: self.ratings.clone(),
            flags: self.flags.clone(),
            labels: self.labels.clone(),
            keywords: self.keywords.clone(),
            output_w: self.output_w,
            output_h: self.output_h,
            output_fps: self.output_fps,
            codec: self.codec.clone(),
            render_mode: self.render_mode,
            render_format: self.render_format,
            render_quality: self.render_quality,
            render_keep_exif: self.render_keep_exif,
            render_xmp_sidecar: self.render_xmp_sidecar,
            render_sharpen_for: self.render_sharpen_for,
            render_sharpen_strength: self.render_sharpen_strength,
            render_color_space: self.render_color_space,
            render_dynamic: self.render_dynamic,
            render_watermark: self.render_watermark.clone(),
            retime: self.retime.clone(),
            motion: self.motion,
            render_crf: self.render_crf,
            render_preset: self.render_preset.clone(),
            render_keep_frames: self.render_keep_frames,
            render_resume: self.render_resume,
            render_audio: self.render_audio.clone(),
            render_audio_opts: self.render_audio_opts.clone(),
        }
    }

    /// 回放快照：收尾和人在面板上改完是同一套——该重算去闪的重算，该重新探针的探针。
    ///
    /// Replay a snapshot: the wind-down is the same as after a human edit in the panel --
    /// deflicker recomputes when it should, probes re-probe when they should.
    pub fn restore(&mut self, snap: Snapshot) {
        let def_changed = self.def_window != snap.def_window
            || self.def_strength != snap.def_strength
            || self.def_multi != snap.def_multi
            || self.def_channels != snap.def_channels
            || self.def_align_exif != snap.def_align_exif;
        let audio_changed = self.render_audio != snap.render_audio;
        self.params = snap.params;
        self.preset = snap.preset;
        self.def_window = snap.def_window;
        self.def_strength = snap.def_strength;
        self.def_multi = snap.def_multi;
        self.def_channels = snap.def_channels;
        self.def_align_exif = snap.def_align_exif;
        self.aspect = snap.aspect;
        // 裁剪框是跟着比例一起定的设置：直接落到快照里的值，不能再走 set_aspect 重铺。
        self.user_crop = snap.crop;
        self.rotate = snap.rotate;
        self.crop_drag = None;
        self.grid = snap.grid;
        self.cross = snap.cross;
        self.split = snap.split;
        self.keyframes = snap.keyframes;
        self.copies = snap.copies;
        self.ratings = snap.ratings;
        self.flags = snap.flags;
        self.labels = snap.labels;
        self.keywords = snap.keywords;
        self.output_w = snap.output_w;
        self.output_h = snap.output_h;
        self.output_fps = snap.output_fps;
        self.codec = snap.codec;
        self.render_mode = snap.render_mode;
        self.render_format = snap.render_format;
        self.render_quality = snap.render_quality;
        self.render_keep_exif = snap.render_keep_exif;
        self.render_xmp_sidecar = snap.render_xmp_sidecar;
        self.render_sharpen_for = snap.render_sharpen_for;
        self.render_sharpen_strength = snap.render_sharpen_strength;
        self.render_color_space = snap.render_color_space;
        self.render_dynamic = snap.render_dynamic;
        self.render_watermark = snap.render_watermark;
        self.retime = snap.retime;
        self.motion = snap.motion;
        self.render_crf = snap.render_crf;
        self.render_preset = snap.render_preset;
        self.render_keep_frames = snap.render_keep_frames;
        self.render_resume = snap.render_resume;
        self.render_audio_opts = snap.render_audio_opts;
        // 蒙版可能比快照里少了，选中项要跟着收，不然画布手柄指向越界的下标。
        if self.mask_sel.is_some_and(|i| i >= self.params.masks.len()) {
            self.mask_sel = None;
            self.mask_comp = 0;
            self.mask_pick = false;
        }
        if def_changed {
            self.recompute_deflicker();
        }
        if audio_changed {
            // 走同一个入口：换配乐要重新读时长，别把上一个文件的探针结果留下。
            self.set_render_audio(snap.render_audio);
        }
        self.preview_dirty = true;
    }

    /// 换序列、关序列：历史从头开始，上一步属于上一个工程。
    ///
    /// Switching or closing a sequence: the history restarts, because the previous step
    /// belongs to the previous project.
    pub fn history_reset(&mut self, label: &str) {
        history::clear_note();
        let snap = self.snapshot();
        self.history.reset(snap, label);
    }

    /// 把当前参数钉成一份命名快照，挂在左栏「快照」那一节。
    ///
    /// 名字取当前预设 + 序号：`原始 · 2`。比纯序号好认——一列「快照 1/2/3」
    /// 过两天谁也分不清哪份是满意的。
    ///
    /// Pin the current parameters as a named snapshot, listed in the left column's snapshot
    /// section.
    ///
    /// The name is the current preset plus an index. That beats a bare index -- a column of
    /// "Snapshot 1/2/3" leaves nobody able to tell which one they liked two days later.
    pub fn save_snapshot(&mut self) {
        // 预设名按当前语言翻译进 label：快照列表直接显示这串。序号与
        // 分隔符跟在后面 —— 一列「快照 1/2/3」过两天谁也分不清哪份是满意的。
        let preset = crate::i18n::tr(&self.preset);
        let label = crate::i18n::tr_fmt("{} · {}", &[&preset, &(self.snapshots.len() + 1)]);
        let snap = self.snapshot();
        self.snapshots.push((label.clone(), snap));
        self.status = crate::i18n::tr_fmt("已存快照「{}」", &[&label]);
    }

    /// 回到某一份快照。整组参数换回去，照样进历史，撤得回来。
    ///
    /// Go back to one snapshot. The whole parameter set is swapped back and still enters the
    /// history, so it can be undone.
    pub fn restore_snapshot(&mut self, i: usize) {
        let Some((label, snap)) = self.snapshots.get(i).cloned() else {
            return;
        };
        self.restore(snap);
        self.status = crate::i18n::tr_fmt("已回到快照「{}」", &[&label]);
    }

    pub fn delete_snapshot(&mut self, i: usize) {
        if i < self.snapshots.len() {
            self.snapshots.remove(i);
        }
    }

    /// 界面帧末尾把当前状态和光标处的快照比一下：不一样就记一步。
    ///
    /// 判据是状态本身，不是「有没有控件登记过」——画布手柄、色轮、以后新加的控件忘了
    /// 登记文案，改动照样进历史，只是那一行写成字段名。登记的文案优先。
    ///
    /// At the end of a UI frame, compare the current state with the snapshot at the cursor:
    /// record a step if they differ.
    ///
    /// The criterion is the state itself, not "did any control register" -- canvas handles,
    /// colour wheels, and future controls that forgot to register a label still reach the
    /// history; the row is merely worded as a field name. A registered label wins over that.
    pub fn history_commit(&mut self) {
        let snap = self.snapshot();
        if !self.history.dirty(&snap) {
            history::clear_note();
            return;
        }
        match history::take_note() {
            Some((key, label)) => self.history.push(snap, &key, label),
            // 没登记控件的路径：整步标成「字段比对」，合并按类认（见 Step::cat）。
            None => {
                let hint = self.history.diff_hint(&snap);
                self.history.push_diff(snap, hint);
            }
        }
    }

    /// 历史行里的参数名换成中文短名。面板上的长名在 shell.rs 的滑杆里，这里只要求
    /// 一眼看出改了哪一类，所以同组的细分项并成一个词。认不出的原样返回：MCP 按英文
    /// 名写入，留着英文反而对得上参数。
    fn cn_field(name: &str) -> &str {
        const GROUPS: &[(&str, &str)] = &[
            ("grain_", "颗粒"),
            ("vig_", "暗角"),
            ("sharp_", "锐化"),
            ("nr_", "降噪"),
            ("grade_", "色彩分级"),
            ("lv_", "色阶"),
            ("par_", "参数曲线"),
            ("hsl_", "HSL"),
            ("curve_", "曲线"),
            ("lens_", "镜头"),
            ("mono_", "单色"),
        ];
        match name {
            "exposure" => "曝光",
            "contrast" => "对比度",
            "highlights" => "高光",
            "shadows" => "阴影",
            "whites" => "白色",
            "blacks" => "黑色",
            "temperature" => "色温",
            "tint" => "色调",
            "vibrance" => "自然饱和度",
            "saturation" => "饱和度",
            "clarity" => "清晰度",
            "texture" => "纹理",
            "dehaze" => "去朦胧",
            "grain" => "颗粒",
            "vignette" => "暗角",
            "mono" => "单色",
            "masks" => "蒙版",
            "look_cube" => "外观",
            "grade_balance" => "分级平衡",
            "grade_blending" => "分级强度",
            "hsl_h" => "HSL 色相",
            "hsl_s" => "HSL 饱和",
            "hsl_l" => "HSL 明度",
            "curve_rgb" => "RGB 曲线",
            "curve_r" => "红曲线",
            "curve_g" => "绿曲线",
            "curve_b" => "蓝曲线",
            other => GROUPS
                .iter()
                .find(|(prefix, _)| other.starts_with(prefix))
                .map_or(other, |(_, label)| *label),
        }
    }

    /// 蒙版类型在命令行里是英文，历史行用界面那套中文说法。
    fn cn_mask_kind(kind: &str) -> &str {
        match kind {
            "radial" => "径向",
            "linear" => "线性",
            other => other,
        }
    }

    /// 命令 → 历史里的一行。`None` 是只读命令，不记步；第三个字段 false 表示效果
    /// 已经落到盘上（导出、对齐、存工程），撤销只回参数、不删产物。
    ///
    /// 这套判定和 `journal::is_write_op` 有意不同：日志要审计队列与存盘，历史只关心
    /// 能不能回到上一个状态；反过来 `deflicker.set` 进日志是漏的，但必须能撤销。
    pub(super) fn history_label(id: &str, args: &Value) -> Option<(String, String, bool)> {
        let named = |key: &str| -> String {
            args.get(key)
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned()
        };
        Some(match id {
            "grade.set" => {
                let obj = args.get("params").filter(|p| p.is_object()).unwrap_or(args);
                let fields: Vec<String> = obj
                    .as_object()
                    .map(|m| {
                        m.keys()
                            .take(3)
                            .map(|name| Self::cn_field(name).to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let more = if obj.as_object().is_some_and(|m| m.len() > 3) {
                    "…"
                } else {
                    ""
                };
                (
                    "grade.set".to_owned(),
                    crate::i18n::tr_fmt("改 {}", &[&[fields.join("、"), more.to_owned()].join("")]),
                    true,
                )
            }
            "grade.preset" => (
                "grade.preset".to_owned(),
                crate::i18n::tr_fmt("预设 {}", &[&named("name")]),
                true,
            ),
            "grade.reset" => ("grade.reset".to_owned(), "复位全部参数".to_owned(), true),
            "grade.copy" => {
                let label = match args.get("op").and_then(Value::as_str).unwrap_or("list") {
                    "save" => crate::i18n::tr_fmt("存副本 {}", &[&named("name")]),
                    "apply" => crate::i18n::tr_fmt("套用副本 {}", &[&named("name")]),
                    "delete" => crate::i18n::tr_fmt("删副本 {}", &[&named("name")]),
                    "rename" => crate::i18n::tr_fmt("副本改名 {}", &[&named("to")]),
                    _ => "调色副本".to_owned(),
                };
                ("grade.copy".to_owned(), label, true)
            }
            "mask.add" => {
                // 没带 kind 时命令按 linear 走，历史行也跟着写线性。
                let kind = if named("kind").eq_ignore_ascii_case("radial") {
                    "radial"
                } else {
                    "linear"
                };
                (
                    "mask.add".to_owned(),
                    crate::i18n::tr_fmt("加{}蒙版", &[&Self::cn_mask_kind(kind)]),
                    true,
                )
            }
            "mask.remove" => ("mask.remove".to_owned(), "删蒙版".to_owned(), true),
            "mask.update" => ("mask.update".to_owned(), "改蒙版".to_owned(), true),
            "mask.component" => ("mask.component".to_owned(), "改蒙版形状".to_owned(), true),
            "deflicker.set" => ("deflicker.set".to_owned(), "去闪烁设置".to_owned(), true),
            "lens.set" => ("lens.set".to_owned(), "镜头配置文件".to_owned(), true),
            "profile.set" => {
                let kind = if named("kind").eq_ignore_ascii_case("lcp") {
                    "镜头配置"
                } else {
                    "相机配置"
                };
                ("profile.set".to_owned(), kind.to_owned(), true)
            }
            "crop.set" => {
                // 一行说清这次改的是框、拉直还是比例；纯读回没有改动，后面 dirty 判定会吞掉这一步。
                let label = if args.get("reset").and_then(Value::as_bool).unwrap_or(false) {
                    "取消取景".to_owned()
                } else if args.get("x").is_some() {
                    crate::i18n::tr_fmt(
                        "取景框 {} × {}",
                        &[
                            &args["width"].as_u64().unwrap_or(0),
                            &args["height"].as_u64().unwrap_or(0),
                        ],
                    )
                } else if let Some(deg) = args.get("rotate").and_then(Value::as_f64) {
                    crate::i18n::tr_fmt("拉直 {}°", &[&format!("{deg:+.1}")])
                } else {
                    crate::i18n::tr_fmt("画幅 {}", &[&named("aspect")])
                };
                ("crop.set".to_owned(), label, true)
            }
            "audio.config" => ("audio.config".to_owned(), "配乐设置".to_owned(), true),
            "render.set" => {
                // 一行点出这次动的是哪个设置；纯查询没有改动，dirty 判定会吞掉这步。
                let label = if args.get("format").is_some() {
                    crate::i18n::tr_fmt("输出容器 {}", &[&named("format")])
                } else if args.get("quality").is_some() {
                    crate::i18n::tr_fmt("输出质量 {}", &[&args["quality"].as_u64().unwrap_or(0)])
                } else if args.get("sharpen_for").is_some() {
                    crate::i18n::tr_fmt("输出锐化 {}", &[&named("sharpen_for")])
                } else if args.get("keep_exif").is_some() {
                    "EXIF 搬运".to_owned()
                } else if args.get("xmp_sidecar").is_some() {
                    "XMP 旁车".to_owned()
                } else if args.get("watermark").is_some() {
                    "水印".to_owned()
                } else {
                    "输出设置".to_owned()
                };
                ("render.set".to_owned(), label, true)
            }
            "retime.set" => {
                let num = |v: Option<&Value>| -> String {
                    v.and_then(Value::as_f64)
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".to_owned())
                };
                // 变速整块只有一行：一行点出这次动的是曲线还是快门。
                let label = if args.get("reset").is_some() {
                    "清除变速".to_owned()
                } else if args.get("speed").is_some() {
                    crate::i18n::tr_fmt("倍速 {}×", &[&num(args.get("speed"))])
                } else if let Some(ease) = args.get("ease") {
                    crate::i18n::tr_fmt("缓入缓出到 {}×", &[&num(ease.get("peak"))])
                } else if args.get("keys").is_some() {
                    "速度曲线".to_owned()
                } else if args.get("shutter").is_some() {
                    crate::i18n::tr_fmt("运动模糊 {}", &[&num(args.get("shutter"))])
                } else {
                    "变速开关".to_owned()
                };
                ("retime.set".to_owned(), label, true)
            }
            "motion.set" => {
                // 运镜一行点出这次动的是哪个框、哪条坡。
                let label = if args.get("reset").is_some() {
                    "清除运镜".to_owned()
                } else if args.get("swap").is_some() {
                    "运镜反向".to_owned()
                } else if args.get("ease").is_some() {
                    crate::i18n::tr_fmt("运动曲线 {}", &[&named("ease")])
                } else if args.get("refit").is_some() {
                    "运镜换比例".to_owned()
                } else if args.get("to").is_some() {
                    "运镜终点框".to_owned()
                } else if args.get("from").is_some() {
                    "运镜起点框".to_owned()
                } else {
                    "运镜开关".to_owned()
                };
                ("motion.set".to_owned(), label, true)
            }
            "xmp.import" => ("xmp.import".to_owned(), "导入 XMP 预设".to_owned(), true),
            "cube.import" => ("cube.import".to_owned(), "导入 .cube 调色".to_owned(), true),
            "align.run" => (String::new(), "对齐序列".to_owned(), false),
            "export.video" => (String::new(), "导出成片".to_owned(), false),
            "export.stills" => (String::new(), "导出静帧".to_owned(), false),
            "hdr.merge" => (String::new(), "HDR 合并".to_owned(), false),
            "pano.merge" => (String::new(), "全景合并".to_owned(), false),
            "cull.evaluate" => (String::new(), "筛选评估".to_owned(), false),
            "cull.extract" => (String::new(), "剔除成子序列".to_owned(), false),
            "stack.merge" => (String::new(), "星空堆栈".to_owned(), false),
            "timeblend.merge" => (String::new(), "时间合成".to_owned(), false),
            "focus.merge" => (String::new(), "焦点合成".to_owned(), false),
            "queue.run" => (String::new(), "批量渲染队列".to_owned(), false),
            "queue.add" | "queue.remove" | "queue.clear" => {
                (String::new(), "改渲染队列".to_owned(), false)
            }
            "project.save" => (String::new(), "保存工程".to_owned(), false),
            "xmp.export" => (String::new(), "导出 XMP".to_owned(), false),
            "cube.export" => (String::new(), "导出 .cube".to_owned(), false),
            _ => return None,
        })
    }

    /// 命令驱动的写入：参数类的记一步，导出/对齐这类外部效果记成不可撤销。
    pub(super) fn history_note_command(&mut self, id: &str, args: &Value) {
        let step = Self::history_label(id, args);
        // 命令执行期间控件路径也会登记改动（mask.* 就是这样：命令内部走 masks_changed）。
        // 命令自己要记步时把它吞掉，一次写入只留一行；只读命令则原样退回，帧末照旧落账。
        let pending = history::take_note();
        let Some((key, label, revertible)) = step else {
            if let Some((k, l)) = pending {
                history::note(&k, l);
            }
            return;
        };
        let _ = pending;
        let snap = self.snapshot();
        if revertible {
            // 参数一点没变就不占一步（把同一个预设再套一次）。
            if self.history.dirty(&snap) {
                self.history.push(snap, &key, label);
            }
        } else {
            // 外部效果照旧记：产物已经在盘上了，这一步撤销不了，但要在历史里看得见。
            self.history.push_external(snap, label);
        }
    }

    fn history_move_json(&self, m: &history::Move, action: &str, step: &str) -> Value {
        json!({
            "action": action,
            "step": step,
            "at": m.cursor + 1,
            "total": m.total,
            "undoable": m.cursor,
            "redoable": m.redoable,
            "warning": if m.external {
                "这一步有已经写到盘上的产物：参数回退了，产物不会删除"
            } else {
                ""
            },
        })
    }

    pub(super) fn cmd_history_undo(&mut self) -> Result<Value, String> {
        let m = self
            .history
            .undo()
            .ok_or_else(|| "没有可以撤销的改动".to_owned())?;
        let out = self.history_move_json(&m, "undo", &m.label.clone());
        self.restore(m.snap);
        self.status = crate::i18n::tr_fmt("撤销 · {}", &[&m.label]);
        Ok(out)
    }

    pub(super) fn cmd_history_redo(&mut self) -> Result<Value, String> {
        let m = self
            .history
            .redo()
            .ok_or_else(|| "没有可以重做的改动".to_owned())?;
        let out = self.history_move_json(&m, "redo", &m.label.clone());
        self.restore(m.snap);
        self.status = crate::i18n::tr_fmt("重做 · {}", &[&m.label]);
        Ok(out)
    }

    pub(super) fn cmd_history_jump(&mut self, args: &Value) -> Result<Value, String> {
        let raw = args
            .get("index")
            .or_else(|| args.get("step"))
            .and_then(Value::as_u64)
            .ok_or_else(|| "缺少 index：跳到第几步（1 开始）".to_owned())?;
        if raw == 0 || raw as usize > self.history.steps().len() {
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.history.steps().len().max(1)],
            ));
        }
        let m = self
            .history
            .jump(raw as usize - 1)
            .ok_or_else(|| "历史记录已改变，请重新查询".to_owned())?;
        let label = m.label.clone();
        let out = self.history_move_json(&m, "jump", &label.clone());
        self.restore(m.snap);
        self.status = crate::i18n::tr_fmt("回到 · {}", &[&label]);
        Ok(out)
    }

    /// 历史列表 + 当前光标；面板和 MCP 共用这一份口径。
    ///
    /// The history list plus the current cursor; the panel and MCP share this one
    /// definition.
    pub fn history_json(&self) -> Value {
        let steps: Vec<Value> = self
            .history
            .steps()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({
                    "index": i + 1,
                    "step": s.label,
                    "applied": i == self.history.cursor(),
                    "revertible": s.revertible,
                })
            })
            .collect();
        json!({
            "count": steps.len(),
            "current": self.history.cursor() + 1,
            "undoable": self.history.undoable(),
            "redoable": steps.len().saturating_sub(self.history.cursor() + 1),
            "steps": steps,
        })
    }
}
