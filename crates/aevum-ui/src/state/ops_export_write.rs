//! 逐帧渲染与写盘：逐帧渲染选项的拼装、交付预设的套用与匹配、动态参数，以及单帧导出的落盘。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Rendering and writing frames: Assembling per-frame render options, applying and matching delivery presets, dynamic parameters, and writing a single-frame export.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    /// 单帧导出读的成片档旋钮：跟批量导出同一组，面板上选了什么这里就是什么。
    pub(super) fn frame_render_options(&self) -> RenderOptions {
        RenderOptions {
            quality: self.render_quality,
            keep_exif: self.render_keep_exif,
            xmp_sidecar: self.render_xmp_sidecar,
            metadata: self.meta.clone(),
            sharpen_for: self.render_sharpen_for,
            sharpen_strength: self.render_sharpen_strength,
            color_space: self.render_color_space,
            dynamic: self.render_dynamic,
            watermark: self.render_watermark.clone(),
            ..RenderOptions::still(self.render_format)
        }
    }

    /// 动态范围与环境不匹配时把组合补齐：HDR 强制 Rec.2020 + AVIF。
    ///
    /// 三个开关互相约束，与其让人点完导出才被 core 的守卫撞回来，不如让选择本身一致。
    /// 补齐是**可见的**（色域与容器的选中态会跳过去），不是静默改值。
    ///
    /// Fills in a legal combination when the dynamic range does not fit: HDR forces
    /// Rec.2020 + AVIF.
    ///
    /// The three switches constrain each other; better to keep the selection consistent than
    /// to let the user hit core's guard after pressing export. The adjustment is **visible**
    /// (the colour space and container selections move), not a silent rewrite.
    /// 套用一条平台交付预设：画幅、帧率（预设给了才改）、编码器、质量档、速度档一次写进去。
    ///
    /// 只动这五项，裁切/拉直/运镜/水印/色域/配乐一概不碰——预设是**交付口径**，不是
    /// 「重置导出设置」。字段口径与 `aevum_core::delivery::DeliveryPreset::apply_to`
    /// 一一对应，那边有判据（core 端到端会把每个数字验到命令与像素上）。
    ///
    /// Applies one platform delivery preset: frame size, frame rate (only when the preset
    /// states one), encoder, quality target and encoder speed. Nothing else is touched --
    /// crop, straighten, camera move, watermark, colour space and audio stay as they are. A
    /// preset is a delivery convention, not a reset button. The mapping mirrors
    /// `DeliveryPreset::apply_to` in core, where the end-to-end assertion checks every number
    /// against the command line and the pixels.
    pub fn apply_delivery(&mut self, preset: &DeliveryPreset) {
        self.output_w = preset.width;
        self.output_h = preset.height;
        if let Some(fps) = preset.fps {
            self.output_fps = fps as f32;
        }
        self.codec = codec_label_for(preset.codec).to_owned();
        self.render_crf = preset.crf;
        self.render_preset = preset.encoder_preset.clone();
        // H.264 是 8bit，装不下 PQ：可见地把动态范围退回 SDR，而不是留一个按下导出才
        // 报错的组合。
        //
        // H.264 is 8bit and cannot carry PQ: drop the dynamic range to SDR visibly instead of
        // leaving a combination that only fails when export is pressed.
        if preset.codec == Codec::H264 {
            self.render_dynamic = OutputDynamic::Sdr;
        }
    }

    /// 当前设置是否**正好等于**这条平台口径。高亮由数值反推：改掉任何一项就自动熄灭，
    /// 不会出现「按钮亮着、设置其实已经不是它」的假象。
    ///
    /// 编码速度档不参与比较——那是性能偏好，不是平台口径。
    ///
    /// Whether the current settings **exactly match** this convention. The highlight is
    /// inferred from the numbers, so changing any one turns it off rather than leaving a lit
    /// chip that no longer matches. Encoder speed is excluded: a performance preference, not
    /// part of the convention.
    pub fn matches_delivery(&self, preset: &DeliveryPreset) -> bool {
        self.output_w == preset.width
            && self.output_h == preset.height
            && self.codec == codec_label_for(preset.codec)
            && self.render_crf == preset.crf
            && preset
                .fps
                .is_none_or(|f| (self.output_fps - f as f32).abs() < 0.5)
    }

    pub fn apply_dynamic(&mut self) {
        if self.render_dynamic != OutputDynamic::HdrPq {
            return;
        }
        // 两个前提：色域必须 Rec.2020（PQ 配窄色域没人认），容器 / 编码器必须装得下
        // 10bit。静帧是 AVIF，成片是 HEVC——两条都当场补齐，别让用户点导出才发现。
        //
        // Two prerequisites: Rec.2020 primaries (PQ in a narrow gamut is recognised by
        // nobody) and a container/codec that holds 10bit. AVIF for stills, HEVC for video —
        // both fixed here rather than at export time.
        self.render_color_space = ColorSpace::Rec2020;
        match self.render_mode {
            OutputMode::Stills => {
                if !self.render_dynamic.fits(self.render_format) {
                    self.render_format = StillFormat::Avif;
                }
            }
            OutputMode::Video => self.codec = "HEVC".to_owned(),
        }
    }

    /// 导出当前帧：**写到默认输出目录，全程不弹系统对话框**。
    ///
    /// 此前这里直接开 `rfd::FileDialog::save_file()`，而它既不带 `set_directory`、也不读
    /// 用户在导出对话框里刚选过的 `render_out_dir` —— 于是"先选目录、再选文件名"，前一次
    /// 的选择被整个丢掉（用户 2026-11-08 反馈"点导出弹两下文件框"，根因就在这里）。
    /// 批量导出 `run_export` 从来就不问路径，单帧没有理由问：目录即输出，文件名按
    /// `{帧名}_graded.{扩展名}` 生成，遵守"同名跳过"。
    ///
    /// 想放到别处走 [`Self::export_current_as`]——那是唯一保留的保存对话框。
    ///
    /// Exports the current frame **into the output directory, with no system dialog**.
    ///
    /// It used to open `rfd::FileDialog::save_file()` without `set_directory` and without
    /// reading `render_out_dir`, which the user had just picked in the export dialog -- so the
    /// user chose a folder, then had to choose a file, and the first choice was thrown away
    /// (user report, 2026-11-08: "clicking export pops two file dialogs"). Batch export never
    /// asked for a path; a single frame has no reason to. The directory is the output and the
    /// name is `{frame stem}_graded.{ext}`, honouring "skip existing".
    ///
    /// Use [`Self::export_current_as`] to put it elsewhere -- the one remaining save dialog.
    pub fn export_current(&mut self, ctx: &Context) {
        let Some(dst) = self.auto_frame_path() else {
            return;
        };
        self.export_current_to(ctx, dst);
    }

    /// 「另存为…」：唯一会弹系统保存框的单帧导出。
    ///
    /// 起始目录就是面板上的输出目录——这一步是本轮修掉的另一半：旧代码不设起始目录，
    /// 对话框每次都从系统默认位置打开，用户得重新找一遍刚才选过的文件夹。
    ///
    /// Save-as: the only single-frame path that opens a system save dialog. It starts in the
    /// panel's output directory -- the other half of this fix, since the old code set no
    /// starting directory and every dialog opened somewhere the user had to navigate back from.
    pub fn export_current_as(&mut self, ctx: &Context) {
        let Some(dst) = self.pick_frame_path() else {
            return;
        };
        self.export_current_to(ctx, dst);
    }

    /// 默认落点：`render_out_dir`（空则 `<序列根>/成片`）+ `{帧名}_graded.{ext}`。
    ///
    /// `None` = 这一下不该导出（已在导出中 / 没有帧 / 同名文件存在且开着"跳过同名"）。
    ///
    /// The default destination: `render_out_dir` (else `<sequence root>/成片`) plus
    /// `{stem}_graded.{ext}`. `None` means there is nothing to do (already exporting, no frame,
    /// or the file exists while "skip existing" is on).
    pub(crate) fn auto_frame_path(&mut self) -> Option<std::path::PathBuf> {
        if self.export_busy {
            return None;
        }
        let seq = self.seq.as_ref()?;
        let frame = seq.frames.get(self.selected)?;
        let stem = std::path::Path::new(&frame.name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "frame".to_owned());
        let dst = crate::state::auto_frame_dst(
            &self.render_out_dir,
            &seq.root,
            &stem,
            self.render_format,
        );
        if self.render_resume && dst.exists() {
            self.status = crate::i18n::tr_fmt("{} 已存在，按「跳过同名」略过", &[&dst.display()]);
            return None;
        }
        Some(dst)
    }

    /// 让用户挑一个落点；起始目录跟着面板走。
    ///
    /// Ask the user for a destination, starting in the panel's output directory.
    fn pick_frame_path(&mut self) -> Option<std::path::PathBuf> {
        let seq = self.seq.as_ref()?;
        let frame = seq.frames.get(self.selected)?;
        let stem = std::path::Path::new(&frame.name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "frame".to_owned());
        let dir = if self.render_out_dir.as_os_str().is_empty() {
            seq.root.join("成片")
        } else {
            self.render_out_dir.clone()
        };
        // 局部变量不能叫 `format`：它会把 `format!` 宏遮住，报"expected value, found macro"。
        //
        // The local must not be named `format`: it shadows the `format!` macro.
        let still = self.render_format;
        let picked = rfd::FileDialog::new()
            .set_directory(&dir)
            .set_file_name(format!("{stem}_graded.{}", still.ext()))
            .add_filter(still.cn(), &[still.ext()])
            .save_file()?;
        Some(force_container(picked, still))
    }

    /// 真正落盘：两条入口共用这一段，避免"另存为"和"默认导出"各写一套。
    ///
    /// The actual write, shared by both entries so "save as" and "default export" cannot drift.
    fn export_current_to(&mut self, ctx: &Context, path: std::path::PathBuf) {
        if self.export_busy {
            return;
        }
        let Some(seq) = &self.seq else { return };
        let Some(frame) = seq.frames.get(self.selected) else {
            return;
        };
        // 目录可能还不存在（用户第一次导出到某个新文件夹）：先建出来，否则编码器报的是
        // 一个和"目录不存在"无关的错，用户看不出该做什么。
        //
        // The directory may not exist yet (first export into a new folder): create it first,
        // otherwise the encoder reports something unrelated to "no such directory".
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.error = Some(crate::i18n::tr_fmt("建不了输出目录：{}", &[&e]));
            return;
        }
        self.export_busy = true;
        self.status = crate::i18n::tr_fmt(
            "正在导出当前帧（{}）…",
            &[&crate::i18n::tr(self.render_format.cn())],
        );
        let params = self.params.clone();
        let gain = self.ev_at(self.selected);
        // 导出按画幅比例裁：面板上的取景框就是最终画面，对齐裁切照旧先叠一层。
        let crop = self.effective_crop();
        let (_, offset) = self.alignment_at(self.selected);
        let framing = Framing {
            angle_deg: self.rotate,
            off: offset,
        };
        let full_size = self.resolution;
        let render = self.frame_render_options();
        let src = frame.path.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            match render_full_file(FrameExport {
                src: &src,
                dst: &path,
                params: &params,
                gain,
                full_size,
                crop,
                framing,
                render,
            }) {
                Ok(()) => {
                    let _ = tx.send(Msg::Saved(path.display().to_string()));
                }
                Err(e) => {
                    let _ = tx.send(Msg::ExportFailed(e));
                }
            }
            ctx2.request_repaint();
        });
    }
}
