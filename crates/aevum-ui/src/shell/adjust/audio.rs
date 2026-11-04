//! 配乐页。
//!
//! The audio page.

// 共用件（theme / i18n / 兄弟模块的函数）由 mod.rs 的 use 树统一带入。
//
// Shared pieces (theme, i18n, sibling helpers) come in through mod.rs's use tree.
use super::*;
pub(super) fn adjust_audio(state: &mut AppState, ui: &mut Ui) {
    if group_header(ui, state, "audio", "配乐", true) {
        let w = ui.available_width();
        // 探针在飞（后台 ffmpeg 还在读编码与时长）时控件可看不可点：此刻换文件
        // 会让迟到回包与用户意图打架。
        //
        // While a probe is in flight (a background ffmpeg is still reading codec and
        // duration) the controls are visible but inert: swapping the file now would pit the
        // late reply against the user's intent.
        let probe_busy = state.audio_probe_busy;
        if ui
            .add_enabled(
                !probe_busy,
                egui::Button::new(RichText::new(crate::i18n::t!("选择音频…")).size(SIZE_SMALL))
                    .min_size(Vec2::new(w, 24.0)),
            )
            .clicked()
        {
            choose_audio(state);
        }
        ui.add_space(4.0);
        match &state.render_audio {
            Some(path) => {
                kv(
                    ui,
                    "当前音频",
                    theme::ellipsis(
                        path.file_name().and_then(|s| s.to_str()).unwrap_or("音频"),
                        26,
                    ),
                    text(),
                );
                if let Some(info) = &state.audio_info {
                    kv(
                        ui,
                        "编码 / 时长",
                        format!("{} · {:.1}s", info.codec, info.duration),
                        muted(),
                    );
                } else if let Some(err) = &state.audio_probe_err {
                    ui.add(
                        egui::Label::new(RichText::new(err).size(SIZE_SMALL).color(danger()))
                            .wrap(),
                    );
                }
                if ui
                    .add_enabled_ui(!probe_busy, |ui| bar_button(ui, "移除音频").clicked())
                    .inner
                {
                    state.set_render_audio(None);
                }
            }
            None => {
                kv(ui, "当前音频", "无", dim());
            }
        }
    }

    if group_header(ui, state, "audio-time", "时间与音量", true) {
        {
            let opts = &mut state.render_audio_opts;
            slider(
                ui,
                "au-delay",
                "延后进入（秒）",
                &mut opts.delay,
                0.0,
                60.0,
                0.0,
                2,
                false,
            );
            slider(
                ui,
                "au-gain",
                "音量（%）",
                &mut opts.gain,
                0.0,
                200.0,
                100.0,
                0,
                false,
            );
            slider(
                ui,
                "au-fin",
                "淡入（秒）",
                &mut opts.fade_in,
                0.0,
                30.0,
                0.0,
                2,
                false,
            );
            slider(
                ui,
                "au-fout",
                "淡出（秒）",
                &mut opts.fade_out,
                0.0,
                30.0,
                0.0,
                2,
                false,
            );
        }
    }

    if group_header(ui, state, "audio-mix", "长度与编码", true) {
        ui.checkbox(
            &mut state.render_audio_opts.looped,
            RichText::new(crate::i18n::t!("循环铺满画面")).size(SIZE_SMALL),
        )
        .on_hover_text(crate::i18n::t!(
            "配乐比画面短时反复播放；循环会限长到画面末尾，不会无限铺下去"
        ));
        ui.checkbox(
            &mut state.render_audio_opts.trim,
            RichText::new(crate::i18n::t!("截到画面结束")).size(SIZE_SMALL),
        )
        .on_hover_text(crate::i18n::t!("配乐比画面长时在画面末尾截断"));
        ui.add_space(4.0);
        egui::ComboBox::from_id_salt("audio-codec")
            .selected_text(
                RichText::new(codec_label(state.render_audio_opts.acodec))
                    .size(SIZE_SMALL)
                    .color(text()),
            )
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for codec in [
                    AudioCodec::Auto,
                    AudioCodec::Copy,
                    AudioCodec::Aac128,
                    AudioCodec::Aac192,
                    AudioCodec::Aac256,
                    AudioCodec::Aac320,
                ] {
                    if ui
                        .add(egui::Button::selectable(
                            state.render_audio_opts.acodec == codec,
                            codec_label(codec),
                        ))
                        .clicked()
                    {
                        state.render_audio_opts.acodec = codec;
                    }
                }
            });
        note(
            ui,
            "自动模式：能原样放进容器的编码直接复制，其余转 AAC 192k。",
        );
    }

    if group_header(ui, state, "audio-timeline", "成片时长", false) {
        match (state.audio_timeline(), &state.render_audio) {
            (Some((video, music, total)), _) => {
                kv(ui, "画面", format!("{video:.1} s"), muted());
                kv(ui, "配乐", format!("{music:.1} s"), muted());
                kv(ui, "成片", format!("{total:.1} s"), accent());
                if total > video + 0.05 {
                    note(ui, "配乐比画面长：片尾会只剩声音。");
                }
            }
            (None, Some(_)) => {
                let video = state.frame_count() as f64 / state.output_fps.max(1.0) as f64;
                kv(ui, "画面", format!("{video:.1} s"), muted());
                kv(ui, "配乐", "时长未知", dim());
            }
            (None, None) => {
                kv(ui, "成片时长", "选一段配乐后显示", dim());
            }
        }
    }
}
