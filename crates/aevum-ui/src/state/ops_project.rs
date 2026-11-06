//! 工程存取与旁车：工程快照与写盘、变更判定、旁车识别与原始导入默认值的套用。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Project persistence and sidecars: Project snapshots and writes, change detection, sidecar detection and applying RAW import defaults.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    pub fn project_snapshot(&self) -> ProjectFile {
        let root = self
            .seq
            .as_ref()
            .map(|s| s.root.display().to_string())
            .unwrap_or_default();
        let long_side = if self.output_w == 0 || self.output_h == 0 {
            None
        } else {
            Some(self.output_w.max(self.output_h))
        };
        let align = self.align_report.as_ref().map(|r| AlignState {
            reference: self.align_ref,
            estimator: match self.align_estimator {
                Estimator::Template => "template",
                Estimator::Phase => "phase",
            }
            .to_owned(),
            offsets: r.offsets.clone(),
            scores: r.scores.clone(),
            issues: r.issues.clone(),
            crop: Some(r.crop),
        });
        ProjectFile {
            version: project::VERSION,
            root,
            reference: self.align_ref,
            params: self.params.clone(),
            preset: self.preset.clone(),
            ratings: self.ratings.iter().map(|(k, v)| (*k, *v)).collect(),
            flags: self.flags.iter().map(|(k, v)| (*k, *v)).collect(),
            labels: self.labels.iter().map(|(k, v)| (*k, *v)).collect(),
            keywords: self.keywords.iter().map(|(k, v)| (*k, v.clone())).collect(),
            stacks: self.stacks.clone(),
            // 落盘的是归一化后的那一份：换行折成空格、超长截断，读回来才和写进去一致。
            metadata: self.meta.cleaned(),
            keyframes: self.keyframes.clone(),
            copies: self.copies.clone(),
            def_window: self.def_window,
            def_strength: self.def_strength,
            def_multi: self.def_multi,
            def_channels: self.def_channels,
            def_align_exif: self.def_align_exif,
            def_offsets: self.corr.clone(),
            def_chan: self.corr_rgb.clone(),
            crop: self.effective_crop(),
            user_crop: self.user_crop,
            rotate: self.rotate,
            aspect: self.aspect.label().to_owned(),
            align,
            render: RenderState {
                mode: match self.render_mode {
                    OutputMode::Stills => "stills",
                    OutputMode::Video => "video",
                }
                .to_owned(),
                format: self.render_format.key().to_owned(),
                quality: self.render_quality,
                keep_exif: self.render_keep_exif,
                xmp_sidecar: self.render_xmp_sidecar,
                sharpen_for: self.render_sharpen_for.key().to_owned(),
                sharpen_strength: self.render_sharpen_strength.key().to_owned(),
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
                out_dir: self.render_out_dir.display().to_string(),
                video_out: self.render_video_out.display().to_string(),
                audio: self
                    .render_audio
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                audio_opts: self.render_audio_opts.clone(),
                long_side,
                keep_frames: self.render_keep_frames,
                color_space: self.render_color_space.key().to_owned(),
                dynamic: self.render_dynamic.key().to_owned(),
                watermark: self.render_watermark.clone(),
                retime: self.retime.clone(),
                motion: self.motion,
            },
            font_level: self.font_level,
        }
    }

    fn apply_project(&mut self, p: ProjectFile) {
        self.params = p.params;
        self.preset = p.preset;
        self.ratings = p.ratings.into_iter().collect();
        self.flags = p.flags.into_iter().collect();
        self.labels = p.labels.into_iter().collect();
        self.keywords = p.keywords.into_iter().collect();
        self.stacks = aevum_core::library::normalize_stacks(&p.stacks);
        self.meta = p.metadata;
        self.keyframes = p.keyframes;
        self.copies = p.copies;
        self.def_window = p.def_window;
        self.def_strength = p.def_strength;
        self.def_multi = p.def_multi;
        self.def_channels = p.def_channels;
        self.def_align_exif = p.def_align_exif;
        self.font_level = p.font_level.min(3);
        self.align_ref = p.reference;

        if let Some(a) = p.align {
            self.align_estimator = if a.estimator == "phase" {
                Estimator::Phase
            } else {
                Estimator::Template
            };
            self.align_ref = a.reference;
            if !a.offsets.is_empty() {
                let crop = a.crop.unwrap_or_else(|| {
                    let (w, h) = self.resolution.unwrap_or((0, 0));
                    CropRect { x: 0, y: 0, w, h }
                });
                let size = self.resolution.unwrap_or((crop.w, crop.h));
                self.align_report = Some(AlignReport {
                    size: [size.0, size.1],
                    offsets: a.offsets,
                    scores: a.scores,
                    issues: a.issues,
                    crop,
                    elapsed_secs: 0.0,
                });
            }
        }

        // 取景：比例、角度和用户框一起回，直接落值——重铺是 set_aspect 的事，这里不能再改形状。
        // `p.crop` 是给渲染队列用的最终取景，界面上要接着拖得用 `user_crop`。
        self.aspect = Aspect::from_label(&p.aspect);
        self.user_crop = p.user_crop;
        self.rotate = p.rotate;
        self.crop_drag = None;

        self.render_mode = if p.render.mode == "stills" {
            OutputMode::Stills
        } else {
            OutputMode::Video
        };
        // 认不出来的容器当 PNG8：旧工程只有那两种，手改错了也不能让面板显示空值。
        self.render_format = StillFormat::parse(&p.render.format).unwrap_or(StillFormat::Png8);
        self.render_quality = p.render.quality.clamp(1, 100);
        self.render_keep_exif = p.render.keep_exif;
        self.render_xmp_sidecar = p.render.xmp_sidecar;
        self.render_sharpen_for = OutputSharpen::parse(&p.render.sharpen_for).unwrap_or_default();
        self.render_sharpen_strength =
            SharpenStrength::parse(&p.render.sharpen_strength).unwrap_or_default();
        // 工程里的色域认不出来就当 sRGB：旧工程没这个字段，手改错了也不能让面板显示空值。
        self.render_color_space = ColorSpace::parse(&p.render.color_space).unwrap_or_default();
        // 动态范围同理：旧工程没这个字段就是 SDR，认不出来的手改值也回落到 SDR。
        self.render_dynamic = OutputDynamic::parse(&p.render.dynamic).unwrap_or_default();
        // 水印是一整块：旧工程没这一段，读进来就是 `Watermark::default()`，也就是没开。
        self.render_watermark = p.render.watermark.clone();
        // 变速同理：旧工程读到的是关着的重映射，导出与以前逐字节一致。
        self.retime = p.render.retime.clone();
        // 运镜同理：旧工程没这一段，读回来就是关着的取景框。
        self.motion = p.render.motion;
        if p.render.width > 0 && p.render.height > 0 {
            self.output_w = p.render.width;
            self.output_h = p.render.height;
        }
        if p.render.fps.is_finite() && p.render.fps > 0.0 {
            self.output_fps = p.render.fps as f32;
        }
        self.codec = if p.render.codec == "h264" {
            "H.264".to_owned()
        } else {
            "HEVC".to_owned()
        };
        self.render_crf = p.render.crf;
        if !p.render.preset.is_empty() {
            self.render_preset = p.render.preset;
        }
        if !p.render.out_dir.is_empty() {
            self.render_out_dir = PathBuf::from(p.render.out_dir);
        }
        if !p.render.video_out.is_empty() {
            self.render_video_out = PathBuf::from(p.render.video_out);
        }
        self.render_audio = if p.render.audio.is_empty() {
            None
        } else {
            Some(PathBuf::from(p.render.audio))
        };
        self.render_audio_opts = p.render.audio_opts;
        self.audio_info = None;
        self.audio_probe_err = None;
        self.render_keep_frames = p.render.keep_frames;

        // 先把落盘的逐帧补偿放回去；有测光数据时下面会按当前参数重算覆盖。
        self.corr = p.def_offsets;
        self.corr_rgb = p.def_chan;
        if !self.metrics.is_empty() {
            self.recompute_deflicker();
        }
        self.preview_dirty = true;
    }

    pub(super) fn write_project(&mut self, quiet: bool) {
        if self.headless {
            return;
        }
        let Some(seq) = &self.seq else { return };
        let path = project::project_path(&seq.root);
        let snap = self.project_snapshot();
        match project::save(&path, &snap) {
            Ok(()) => {
                self.saved_snap = Some(self.snapshot());
                self.saved_stamp = Some(self.project_stamp());
                if !quiet {
                    self.status = crate::i18n::tr_fmt("工程已保存 {}", &[&path.display()]);
                }
            }
            Err(e) => {
                if !quiet {
                    self.error = Some(crate::i18n::tr_fmt("工程保存失败：{}", &[&e]));
                }
            }
        }
    }

    /// 当前状态是否和最后一次落盘的一致。`Snapshot` 是逐字段比较，几微秒的事，
    /// 比序列化加写盘便宜三个数量级。
    pub(super) fn project_unchanged(&self) -> bool {
        self.saved_snap.as_ref() == Some(&self.snapshot())
            && self.saved_stamp.as_ref() == Some(&self.project_stamp())
    }

    fn project_stamp(&self) -> ProjectStamp {
        let report = self.align_report.as_ref();
        ProjectStamp {
            align_ref: self.align_ref,
            estimator: match self.align_estimator {
                Estimator::Template => "template",
                Estimator::Phase => "phase",
            },
            offsets: report.map_or(0, |r| r.offsets.len()),
            issues: report.map_or(0, |r| r.issues.len()),
            crop: self.effective_crop().map(|c| (c.x, c.y, c.w, c.h)),
        }
    }

    pub fn save_project(&mut self) {
        self.write_project(false);
        self.last_auto_save = Instant::now();
    }

    pub fn load_project(&mut self, root: &Path) {
        let path = project::project_path(root);
        if !path.is_file() {
            return;
        }
        match project::load(&path) {
            Ok(p) => self.apply_project(p),
            Err(e) => {
                self.error = Some(crate::i18n::tr_fmt("读取工程失败：{}", &[&e]));
            }
        }
    }

    /// 打开序列时检测同目录 `.xmp` 边车：只记下路径给状态栏提示，
    /// **不做静默自动套用**——LR 用户对「打开即所见」有预期，但静默改参数
    /// 也会让「我从哪张图开始调的」说不清，应用与否交给一键按钮。
    ///
    /// 只查**前 3 帧**：Lightroom / Camera Raw 的边车按帧名与图同名，而这类序列
    /// 几乎总是从首帧开始成套带边车（批量改图也是从第一张起按序写盘）；跳帧
    /// 存在但极罕见。三千帧逐帧 `is_file()` 就是三千次 `stat`，换来的只是那一例
    /// 罕见情形——不值得让每次开序列都付这笔 I/O。
    ///
    /// Detects a sibling `.xmp` sidecar when a sequence opens: only the path is remembered
    /// for the status-bar hint, **never applied silently** -- Lightroom users expect "open
    /// and see", but silently changing parameters also muddies "which image did I start
    /// from"; applying it is a one-click decision.
    ///
    /// Only the **first 3 frames** are checked: Lightroom / Camera Raw sidecars are named
    /// after their frames, and such sequences almost always carry sidecars as a set from
    /// the first frame (batch edits are written in order starting at frame one); a gap does
    /// occur but is rare. `is_file()` over three thousand frames is three thousand `stat`s
    /// bought for that one rare case -- not worth paying on every sequence open.
    pub(super) fn detect_sidecar_hint(&mut self) {
        self.sidecar_hint = None;
        let Some(seq) = &self.seq else { return };
        for frame in seq.paths().into_iter().take(3) {
            let sidecar = xmp::sidecar_for(frame.as_path());
            if sidecar.is_file() {
                self.sidecar_hint = Some(sidecar);
                return;
            }
        }
    }

    /// RAW 首次导入的默认参数：锐化 40 / 锐化保护 20 / 亮度降噪 25。
    ///
    /// 挂在 `open_paths` 的 `load_project` 之后、`history_reset` 之前：
    /// 「只一次」由工程文件保证——首次导入后工程落盘（含这些默认值），之后打开
    /// 读工程、条件不再成立；普通图片序列不含 RAW 帧，零影响。
    /// 默认值进初始历史，撤销一挡就能回到干净状态。
    ///
    /// 注意 `GradeParams::default()` 的锐化保护仍是 0（旧工程 JSON 缺字段也反序列化
    /// 成 0、渲染走旧路径）：20 只在这里给新建工程，不进引擎默认、不进预设。
    ///
    /// Default parameters for a first RAW import: sharpen 40 / sharpen guard 20 / luminance
    /// denoise 25.
    ///
    /// Hooked in after `load_project` and before `history_reset` in `open_paths`: "only once" is
    /// guaranteed by the project file -- the first import writes the project (including these
    /// defaults), and later opens read it, so the condition no longer holds; an ordinary image
    /// sequence contains no RAW frames and is unaffected.
    /// The defaults enter the initial history, so one undo returns to a clean state.
    ///
    /// Note that `GradeParams::default()` still has a sharpen guard of 0 (project JSON missing
    /// the field also deserialises to 0 and renders the old path): 20 is given only to newly
    /// created projects, never to the engine defaults or the presets.
    pub fn apply_raw_import_defaults(&mut self, root: &Path) {
        if project::project_path(root).is_file() {
            return;
        }
        let Some(seq) = &self.seq else { return };
        // 传感器路径 = 厂商 RAW 后缀 **或** 命名 DNG：RAW_EXTENSIONS 表故意不含
        // dng（DNG 走内置专用解析），只查那张表会让 iPhone/相机的 DNG 序列
        // 拿不到导入默认——用户实测 base_curve 永远没置位的根因。
        //
        // The sensor route = vendor RAW suffixes **or** named DNGs: RAW_EXTENSIONS
        // deliberately omits dng (DNGs go through the built-in parser), so keying on
        // that table alone meant iPhone/camera DNG sequences never received the import
        // defaults -- the root cause of base_curve never being set (user-tested).
        let sensor_any = seq
            .paths()
            .iter()
            .any(|p| aevum_core::raw::is_raw_name(p) || aevum_core::dng::is_named_dng(p));
        if !sensor_any {
            return;
        }
        // 基准色调每次打开都置位（幂等）：它是 RAW 的渲染基调而非「参数」，
        // 旧工程（本字段引入前创建的）也该拿到；复位与预设都保留它。
        // base_curve 在 dcp_profile 非空时自动不参与渲染（style::base_tone 的
        // 调用条件），无需在这里判断。
        self.params.base_curve = true;
        if project::project_path(root).is_file() {
            // 旧工程：只补基调，锐化/降噪这些「导入默认」仍然只给新工程。
            self.status = "RAW 序列已套用基准色调（传感器默认渲染基调）".to_owned();
            return;
        }
        self.params.sharp_amount = 40.0;
        self.params.sharp_protect = 20.0;
        self.params.nr_luma = 25.0;
        self.status =
            "RAW 序列已套用默认：基准色调 · 锐化 40 · 锐化保护 20 · 亮度降噪 25".to_owned();
    }

    /// 状态栏「应用边车」的一键动作：把检测到的边车当预设导入（整体替换
    /// 全局参数，与 `xmp.import` 同口径），失败落 error、成功记历史。
    ///
    /// The status bar's one-click "apply sidecar": import the detected sidecar as a preset
    /// (replacing the global parameters wholesale, the same definition as `xmp.import`), raising
    /// `error` on failure and recording history on success.
    pub fn apply_sidecar_hint(&mut self) {
        let Some(path) = self.sidecar_hint.take() else {
            return;
        };
        match self.import_xmp_preset(&path) {
            Ok(_) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.status = crate::i18n::tr_fmt("已应用 XMP 边车 {}", &[&name]);
                history::note(
                    "xmp-sidecar",
                    crate::i18n::tr_fmt("应用 XMP 边车 {}", &[&path.display()]),
                );
            }
            Err(e) => {
                self.error = Some(e);
            }
        }
    }
}
