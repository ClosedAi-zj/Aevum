//! 配乐：配乐的挂载、时间轴读取与音频编码设置。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Soundtrack: Attaching the soundtrack, reading its timeline, and the audio encoding settings.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ 配乐

    /// 换配乐并立刻读一次编码与时长；没有 ffmpeg 只记原因，不影响导出前的报错。
    ///
    /// Swap the soundtrack and immediately probe its codec and duration once; without ffmpeg
    /// only the reason is recorded, which does not change the pre-export error.
    pub fn set_render_audio(&mut self, path: Option<PathBuf>) {
        self.render_audio = path;
        self.audio_info = None;
        self.audio_probe_err = None;
        let Some(path) = &self.render_audio else {
            return;
        };
        if self.headless {
            return;
        }
        let Some(ffmpeg) = export::find_ffmpeg() else {
            self.audio_probe_err = Some("找不到 ffmpeg，无法读取配乐时长".to_owned());
            return;
        };
        match export::probe_audio(&ffmpeg, path) {
            Ok(info) => self.audio_info = Some(info),
            Err(e) => self.audio_probe_err = Some(crate::i18n::err_text(&e)),
        }
    }

    /// 画面 / 配乐 / 成片三段时长；没有探针结果时返回 `None`。
    ///
    /// The three durations: picture / soundtrack / result. `None` without a probe result.
    pub fn audio_timeline(&self) -> Option<(f64, f64, f64)> {
        let info = self.audio_info.as_ref()?;
        let vdur = self.frame_count() as f64 / self.output_fps.max(1.0) as f64;
        Some((
            vdur,
            info.duration,
            self.render_audio_opts.total_dur(vdur, info.duration),
        ))
    }

    fn audio_status_json(&self) -> Value {
        let vdur = self.frame_count() as f64 / self.output_fps.max(1.0) as f64;
        let adur = self.audio_info.as_ref().map(|i| i.duration).unwrap_or(0.0);
        let filter = self
            .audio_info
            .as_ref()
            .and_then(|_| self.render_audio_opts.filter(vdur, adur))
            .map(|f| f.complex);
        let out_ext = self
            .render_video_out
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp4");
        let copy = self
            .audio_info
            .as_ref()
            .is_some_and(|i| self.render_audio_opts.copy_ok(out_ext, &i.codec));
        json!({
            "path": self.render_audio.as_ref().map(|p| p.display().to_string()),
            "probe": self.audio_info,
            "probe_error": self.audio_probe_err,
            "options": self.render_audio_opts,
            "video_seconds": round6(vdur),
            "total_seconds": if self.audio_info.is_some() {
                Some(round6(self.render_audio_opts.total_dur(vdur, adur)))
            } else {
                None
            },
            "filter": filter,
            "copy": copy,
        })
    }

    /// 一次改多项配乐设置；`path` 传空串或 null 表示清空。
    ///
    /// Change several soundtrack settings at once; an empty string or null for `path` clears
    /// it.
    pub fn configure_audio(&mut self, args: &Value) -> Result<Value, String> {
        let mut opts = self.render_audio_opts.clone();
        if let Some(v) = args.get("delay") {
            let d = v.as_f64().ok_or("delay 必须是数字（秒）")?;
            if !(d.is_finite() && (0.0..=3600.0).contains(&d)) {
                return Err("delay 必须在 0..3600 秒".to_owned());
            }
            opts.delay = d;
        }
        if let Some(v) = args.get("gain") {
            let g = v.as_f64().ok_or("gain 必须是数字（百分比）")?;
            if !(g.is_finite() && (0.0..=400.0).contains(&g)) {
                return Err("gain 必须在 0..400（100 为原始音量）".to_owned());
            }
            opts.gain = g;
        }
        for (key, slot) in [
            ("fade_in", &mut opts.fade_in),
            ("fade_out", &mut opts.fade_out),
        ] {
            if let Some(v) = args.get(key) {
                let f = v
                    .as_f64()
                    .ok_or(crate::i18n::tr_fmt("{} 必须是数字（秒）", &[&key]))?;
                if !(f.is_finite() && (0.0..=3600.0).contains(&f)) {
                    return Err(crate::i18n::tr_fmt("{} 必须在 0..3600 秒", &[&key]));
                }
                *slot = f;
            }
        }
        if let Some(v) = args.get("loop") {
            opts.looped = v.as_bool().ok_or("loop 必须是布尔值")?;
        }
        if let Some(v) = args.get("trim") {
            opts.trim = v.as_bool().ok_or("trim 必须是布尔值")?;
        }
        if let Some(v) = args.get("acodec") {
            let text = v.as_str().ok_or("acodec 必须是字符串")?;
            opts.acodec = AudioCodec::parse(text).ok_or_else(|| {
                crate::i18n::tr_fmt(
                    "未知 acodec {}；可用 auto / copy / aac128 / aac192 / aac256 / aac320",
                    &[&text],
                )
            })?;
        }
        let mut audio = self.render_audio.clone();
        let mut changed_file = false;
        if let Some(v) = args.get("path") {
            changed_file = true;
            match v {
                Value::Null => audio = None,
                Value::String(s) if s.trim().is_empty() => audio = None,
                Value::String(s) => {
                    let path = PathBuf::from(s);
                    if !path.is_file() {
                        return Err(crate::i18n::tr_fmt(
                            "配乐文件不存在：{}",
                            &[&path.display()],
                        ));
                    }
                    if !aevum_core::audio::is_audio_path(&path) {
                        return Err(crate::i18n::tr_fmt(
                            "不支持这个扩展名：{}；可用 mp3 / wav / aac / m4a / flac / ogg / opus / wma / aif(m4b)",
                            &[&path.display()],
                        ));
                    }
                    audio = Some(path);
                }
                _ => return Err("path 必须是字符串或 null".to_owned()),
            }
        }
        self.render_audio_opts = opts;
        if changed_file || (self.render_audio.is_some() && self.audio_info.is_none()) {
            self.set_render_audio(audio);
        } else {
            self.render_audio = audio;
        }
        self.status = "配乐设置已更新".to_owned();
        Ok(self.audio_status_json())
    }
}
