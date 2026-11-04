//! 成片队列页：交付预设、输出规格、编码、重定时、运镜、队列表与导出进度条。
//!
//! The render queue page: delivery presets, output spec, codec, retime, motion, the
//! queue table and the export progress bar.

// 共用件（theme / i18n / 兄弟模块的函数）由 mod.rs 的 use 树统一带入。
//
// Shared pieces (theme, i18n, sibling helpers) come in through mod.rs's use tree.
use super::*;
/// 交付预设表只读一次：面板每帧都画，不能每帧读一遍 TOML。改表后重启生效——预设是
/// 低频配置，为它挂一个文件监听不值。
///
/// 预设 → 界面字段的映射在 `AppState::apply_delivery`（那边有判据），这里只管画。
///
/// The delivery table is read once: the panel redraws every frame and must not re-read a TOML
/// file each time. Editing the table takes effect after a restart; presets are low-frequency
/// configuration and a watcher is not worth the complexity. The preset-to-field mapping lives
/// in `AppState::apply_delivery` (which is asserted); this only draws.
fn delivery_table() -> &'static DeliveryTable {
    static TABLE: std::sync::OnceLock<DeliveryTable> = std::sync::OnceLock::new();
    TABLE.get_or_init(DeliveryTable::load)
}

/// 在系统文件管理器里定位一个文件。只用于「预设表」这个入口：用户改的是 TOML，
/// 不给个能点开的入口，等于把「改表不改代码」写成一句空话。
///
/// Reveals a file in the system file manager. Only used by the preset-table entry: the user
/// edits a TOML file, and without a clickable way to reach it "edit the table, not the code"
/// stays an empty promise.
fn reveal_in_file_manager(path: &std::path::Path) {
    #[cfg(windows)]
    {
        // `/select,<path>` 会在资源管理器里选中该文件；explorer 对成功/失败都返回同一个
        // 退出码，所以不看返回值，也不在这里报错。
        //
        // `/select,<path>` highlights the file in Explorer. Explorer returns the same exit
        // code either way, so the result is not inspected and no error is reported here.
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-R", &path.display().to_string()])
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // 没有「选中文件」的通行做法，退到打开它所在的目录。
        //
        // There is no portable "reveal", so open the containing directory instead.
        let dir = path.parent().unwrap_or(path);
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

pub(super) fn adjust_render(state: &mut AppState, ui: &mut Ui) {
    // 平台交付：一行一个平台的完整口径，点一下把画幅/帧率/编码/质量档一次填好，之后
    // 仍能在下面逐项微调。放在「输出」之前——先定投哪儿，再调细节。
    //
    // One chip per platform fills frame, rate, encoder and quality in one click; everything
    // stays editable below. It sits before "Output": decide where it goes first, then tune.
    if group_header(ui, state, "render-delivery", "平台交付", true) {
        let table = delivery_table();
        let zh = crate::i18n::current() == crate::i18n::Lang::ZH;
        ui.horizontal_wrapped(|ui| {
            for preset in &table.presets {
                let response = bar_chip(ui, preset.name(zh), state.matches_delivery(preset));
                if response.clicked() {
                    state.apply_delivery(preset);
                }
                let hint = preset.hint(zh);
                if !hint.is_empty() {
                    response.on_hover_text(hint);
                }
            }
        });
        let active: Vec<&str> = table
            .presets
            .iter()
            .filter(|p| state.matches_delivery(p))
            .map(|p| p.name(zh))
            .collect();
        let line = if active.is_empty() {
            crate::i18n::t!("当前不是任何平台的口径（按自己的数字导出）").to_owned()
        } else {
            active.join(" · ")
        };
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(line).size(SIZE_SMALL).color(muted())).wrap());
        ui.horizontal_wrapped(|ui| {
            if ui.small_button(crate::i18n::t!("预设表…")).clicked() {
                let path = DeliveryTable::path();
                // 不存在就先写一份内置模板再打开：让人「照着一个空文件猜格式」是最坏的
                // 交付方式。
                //
                // Write the built-in template first when the file is missing: asking someone
                // to guess the format from an empty file is the worst kind of delivery.
                if !path.exists() {
                    let _ = table.write_to(&path);
                }
                reveal_in_file_manager(&path);
            }
            // 路径按**可用宽度**截断，不按字符数：34 个半角字符在 372px 面板里
            // 仍然宽过按钮剩下的位置，旧写法会让路径压到「预设表…」按钮上
            // （2026-11-04 的重叠自检抓到的实例）。`truncate()` 走 egui 的布局，
            // 拿到的是这一行真实的剩余宽度。
            //
            // Truncate by available width rather than by character count: 34 half-width
            // characters still overrun the space left of the button on a 372 px panel,
            // and the old form ran the path onto the button. `truncate()` asks egui's
            // layout for the real remaining width.
            ui.add(
                egui::Label::new(
                    RichText::new(DeliveryTable::path().display().to_string())
                        .size(SIZE_SMALL)
                        .color(dim()),
                )
                .truncate()
                .selectable(false),
            )
            .on_hover_text(crate::i18n::t!("改完重启生效：预设在启动时读一次"));
        });
    }
    if group_header(ui, state, "render-out", "输出", true) {
        ui.horizontal_wrapped(|ui| {
            for (mode, label) in [
                (OutputMode::Video, "视频"),
                (OutputMode::Stills, "静帧序列"),
            ] {
                if bar_chip(ui, label, state.render_mode == mode).clicked() {
                    state.render_mode = mode;
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for (label, w, h) in [
                ("原生", 0u32, 0u32),
                ("4K", 3840, 2160),
                ("1080p", 1920, 1080),
            ] {
                if bar_chip(ui, label, (state.output_w, state.output_h) == (w, h)).clicked() {
                    state.output_w = w;
                    state.output_h = h;
                }
            }
        });
        ui.add_space(4.0);
        let spec = if state.output_w == 0 || state.output_h == 0 {
            state
                .resolution
                .map(|(w, h)| crate::i18n::tr_fmt("原生 {} × {}", &[&w, &h]))
                .unwrap_or_else(|| "原生".to_owned())
        } else {
            format!("{} × {}", state.output_w, state.output_h)
        };
        kv(ui, "当前规格", spec, text());
        // 容器分模式：JPEG/WebP 是给最终交付用的，当不了 ffmpeg 的中间帧，所以视频
        // 模式只摆无损那两种；从静帧切回视频时把选中的容器退回 PNG，别让用户
        // 点导出才被 core 的守卫撞回来。
        if state.render_mode == OutputMode::Video && !state.render_format.frames_ok() {
            state.render_format = StillFormat::Png8;
        }
        // 成片现在也能出 HDR（x265 main10 + PQ）。以前这里把动态范围硬收回 SDR，
        // 因为那时成片只有 8bit；现在改由 `apply_dynamic` 补齐前提（HDR 成片必须
        // HEVC + Rec.2020），切换模式时不再动用户的动态范围选择。
        //
        // Video can now be HDR too (x265 main10 + PQ). This used to force the dynamic range
        // back to SDR because video was 8bit only; now `apply_dynamic` fills in the
        // prerequisites (HDR video needs HEVC + Rec.2020), so switching modes no longer
        // touches the user's choice.
        let formats: &[StillFormat] = if state.render_mode == OutputMode::Video {
            &[StillFormat::Png8, StillFormat::Tiff16]
        } else {
            &StillFormat::ALL
        };
        ui.horizontal_wrapped(|ui| {
            for &format in formats {
                if bar_chip(ui, format.cn(), state.render_format == format)
                    .on_hover_text(crate::i18n::tr(format.hint()))
                    .clicked()
                {
                    state.render_format = format;
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for space in ColorSpace::ALL {
                let picked = bar_chip(ui, space.cn(), state.render_color_space == space)
                    .on_hover_text(crate::i18n::tr(space.hint()))
                    .clicked();
                if picked {
                    state.render_color_space = space;
                }
            }
        });
        // 动态范围两种模式都开放：静帧走 AVIF 10bit，成片走 x265 main10。
        // 前提（容器 / 编码器）由 `apply_dynamic` 当场补齐，用户不会先点导出再挨错。
        //
        // Dynamic range is available in both modes: stills go AVIF 10bit, video goes x265
        // main10. The prerequisites (container / codec) are filled in by `apply_dynamic`, so
        // nobody presses export and gets an error afterwards.
        {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for dyn_range in OutputDynamic::ALL {
                    let picked = bar_chip(ui, dyn_range.cn(), state.render_dynamic == dyn_range)
                        .on_hover_text(crate::i18n::tr(dyn_range.hint()))
                        .clicked();
                    if picked {
                        state.render_dynamic = dyn_range;
                        // HDR 需要 Rec.2020 + AVIF，补齐摆在明面上（chips 会跳）。
                        state.apply_dynamic();
                    }
                }
            });
            if state.render_dynamic == OutputDynamic::HdrPq {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(if state.render_mode == OutputMode::Video {
                        crate::i18n::t!("HDR：x265 main10 + PQ，成片里带 smpte2084 标记")
                    } else {
                        crate::i18n::t!(
                            "HDR：参考白 203 nits，超过白的部分沿曲线尾部继续上升；只有 AVIF 装得下"
                        )
                    })
                    .size(SIZE_SMALL)
                    .color(theme::muted()),
                );
                // 面板只能显示到白，所以 HDR 高光在预览里始终和 SDR 一样被削平——
                // 「导出后拿别的软件看」正是这条缺口。这个开关把白以上折进最后一段
                // 码值，让高光层次在面板里可见（白以下逐位不动）。
                //
                // The panel can only reach white, so HDR highlights look clipped in the
                // preview exactly as in SDR — "export it and check elsewhere" was the gap.
                // This toggle folds above-white into the last slice of the code range so the
                // highlight layers are visible here (below white: bit-identical).
                if ui
                    .checkbox(
                        &mut state.hdr_preview,
                        RichText::new(crate::i18n::t!("预览白以上（SDR 屏上检视高光）"))
                            .size(SIZE_SMALL),
                    )
                    .on_hover_text(crate::i18n::t!(
                        "屏幕显示不到白：打开后把白以上折进最后约 25 级，高光里的层次就看得见；中间调与暗部一位不动。这不是模拟 HDR 屏亮度，只是让你确认白以上还有没有东西。"
                    ))
                    .changed()
                {
                    state.preview_dirty = true;
                    state.settings_dirty = true;
                }
            }
        }
        if state.render_format.lossy() {
            ui.add_space(4.0);
            let mut q = f64::from(state.render_quality);
            if slider(
                ui,
                "render.quality",
                "压缩质量",
                &mut q,
                1.0,
                100.0,
                90.0,
                0,
                false,
            ) {
                state.render_quality = q.round().clamp(1.0, 100.0) as u8;
            }
        }
        if state.render_format.supports_exif() {
            ui.add_space(2.0);
            ui.checkbox(
                &mut state.render_keep_exif,
                RichText::new(crate::i18n::t!("保留 EXIF 与版权")).size(SIZE_SMALL),
            )
            .on_hover_text(crate::i18n::t!("源 JPEG 的 EXIF 块原样搬进输出（机身、曝光、版权都在里面）；RAW 与 TIFF 源搬不动，容器不支持就不写"));
        }
        // 旁车只服务静帧：视频模式的中间帧是过程产物，不给它们发 .xmp，所以这一格
        // 跟着模式走，跟上面「容器分模式」一个口径。
        if state.render_mode == OutputMode::Stills {
            ui.add_space(2.0);
            ui.checkbox(
                &mut state.render_xmp_sidecar,
                RichText::new(crate::i18n::t!("写 XMP 旁车")).size(SIZE_SMALL),
            )
            .on_hover_text(crate::i18n::t!("每张静帧旁边写一个同名 .xmp：说明与版权四项（dc:）加这一张的调色参数（crs:），Lightroom / Camera Raw 直接认"));
        }
        // 续跑两种模式都要：静帧补的是交付帧，成片补的是中间帧（开了它就不走管道直连）。
        ui.add_space(2.0);
        ui.checkbox(
            &mut state.render_resume,
            RichText::new(crate::i18n::t!("断点续跑（跳过已导出的帧）")).size(SIZE_SMALL),
        )
        // 一条一行写全：Rust 的续行（反斜杠+换行）会把换行与缩进一起吃掉，
        // 而译文表按字面量原样存键，两边口径对不上就查不到译文。
        .on_hover_text(crate::i18n::t!(
            "输出目录里已经写完的帧原样留着，这一趟只补没写出来的那些。\n跑崩了再点一次就是接着导，不必从头重渲。\n它不看画面有没有改过：改了参数想重导就别勾，或者先把旧帧删掉。"
        ));
        ui.add_space(4.0);
        // 输出锐化是成片级设置：在缩放到最终尺寸之后补的那一刀，预览不看它。
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("输出锐化"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            for &s in &OutputSharpen::ALL {
                if bar_chip(ui, s.cn(), state.render_sharpen_for == s)
                    .on_hover_text(crate::i18n::tr(s.hint()))
                    .clicked()
                {
                    state.render_sharpen_for = s;
                }
            }
        });
        if state.render_sharpen_for != OutputSharpen::Off {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(crate::i18n::t!("强度"))
                        .size(SIZE_SMALL)
                        .color(dim()),
                );
                for &s in &SharpenStrength::ALL {
                    if bar_chip(ui, s.cn(), state.render_sharpen_strength == s).clicked() {
                        state.render_sharpen_strength = s;
                    }
                }
            });
        }
        if state.render_mode == OutputMode::Video {
            ui.add_space(2.0);
            ui.checkbox(
                &mut state.render_keep_frames,
                RichText::new(crate::i18n::t!("保留中间帧")).size(SIZE_SMALL),
            );
            ui.add_space(2.0);
            let dir = state.render_out_dir.display().to_string();
            ui.add(
                egui::Label::new(
                    RichText::new(crate::i18n::tr_fmt(
                        "中间帧 · {}",
                        &[&theme::ellipsis(&dir, 30)],
                    ))
                    .size(SIZE_SMALL)
                    .color(dim()),
                )
                .wrap(),
            );
            if bar_button(ui, "选择输出目录…").clicked()
                && let Some(dir) = rfd::FileDialog::new()
                    .set_directory(
                        state
                            .seq
                            .as_ref()
                            .map(|s| s.root.clone())
                            .unwrap_or_default(),
                    )
                    .pick_folder()
            {
                state.render_out_dir = dir;
            }
        }
    }
    if group_header(ui, state, "render-watermark", "水印", true) {
        let mut enabled = state.render_watermark.enabled;
        if ui
            .checkbox(
                &mut enabled,
                RichText::new(crate::i18n::t!("在成片上盖章")).size(SIZE_SMALL),
            )
            .on_hover_text(crate::i18n::t!(
                "和输出锐化同一条规矩：只在导出尾部生效，预览与缩略图都不叠这一层"
            ))
            .changed()
        {
            state.render_watermark.enabled = enabled;
            let on = crate::i18n::tr(if enabled { "开" } else { "关" });
            history::note("render.watermark", crate::i18n::tr_fmt("水印 {}", &[&on]));
        }
        if state.render_watermark.enabled {
            ui.add_space(4.0);
            // 内容二选一：切过去另一侧的输入原样留着，来回切不丢正文。
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(crate::i18n::t!("内容"))
                        .size(SIZE_SMALL)
                        .color(muted()),
                );
                for &kind in &WatermarkKind::ALL {
                    if bar_chip(ui, kind.cn(), state.render_watermark.kind == kind).clicked() {
                        state.render_watermark.kind = kind;
                        history::note(
                            "render.watermark.kind",
                            crate::i18n::tr_fmt("水印内容 {}", &[&crate::i18n::tr(kind.cn())]),
                        );
                    }
                }
            });
            ui.add_space(2.0);
            if state.render_watermark.kind == WatermarkKind::Text {
                let edited = ui.add(
                    egui::TextEdit::singleline(&mut state.render_watermark.text)
                        .hint_text(format!("© {}", aevum_core::brand::TITLE))
                        .desired_width(f32::INFINITY)
                        .font(mono(SIZE_SMALL)),
                );
                if edited.changed() {
                    history::note("render.watermark.text", "水印正文");
                }
            } else {
                let picked = state.render_watermark.image.display().to_string();
                let shown = if state.render_watermark.image.as_os_str().is_empty() {
                    "还没选图片".to_owned()
                } else {
                    theme::ellipsis(&picked, 24)
                };
                ui.horizontal(|ui| {
                    if bar_button(ui, "选择图片…").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("图片", &["png", "jpg", "jpeg", "webp", "tif", "tiff"])
                            .set_directory(
                                state
                                    .seq
                                    .as_ref()
                                    .map(|s| s.root.clone())
                                    .unwrap_or_default(),
                            )
                            .pick_file()
                    {
                        state.render_watermark.image = path;
                        history::note("render.watermark.image", "水印图片");
                    }
                    ui.label(RichText::new(shown).size(SIZE_SMALL).color(dim()));
                });
            }
            ui.add_space(4.0);
            // 九宫格按屏幕坐标读：y 向下，所以「上中」真的是上面那格。
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(crate::i18n::t!("位置"))
                        .size(SIZE_SMALL)
                        .color(muted()),
                );
                for &spot in &Spot::ALL {
                    if bar_chip(ui, spot.cn(), state.render_watermark.spot == spot).clicked() {
                        state.render_watermark.spot = spot;
                        history::note(
                            "render.watermark.spot",
                            crate::i18n::tr_fmt("水印位置 {}", &[&crate::i18n::tr(spot.cn())]),
                        );
                    }
                }
            });
            let mut opacity = f64::from(state.render_watermark.opacity);
            if slider(
                ui,
                "render.watermark.opacity",
                "不透明度",
                &mut opacity,
                0.0,
                100.0,
                60.0,
                0,
                false,
            ) {
                state.render_watermark.opacity = opacity.round().clamp(0.0, 100.0) as u8;
            }
            let mut size = f64::from(state.render_watermark.size);
            if slider(
                ui,
                "render.watermark.size",
                "尺寸",
                &mut size,
                1.0,
                40.0,
                4.0,
                0,
                false,
            ) {
                state.render_watermark.size = size.round().clamp(1.0, 40.0) as u8;
            }
            let mut margin = f64::from(state.render_watermark.margin);
            if slider(
                ui,
                "render.watermark.margin",
                "边距",
                &mut margin,
                0.0,
                40.0,
                2.0,
                0,
                false,
            ) {
                state.render_watermark.margin = margin.round().clamp(0.0, 40.0) as u8;
            }
            if state.render_watermark.kind == WatermarkKind::Text {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(crate::i18n::t!("颜色"))
                            .size(SIZE_SMALL)
                            .color(muted()),
                    );
                    for (name, rgb) in [
                        ("白", [255u8, 255, 255]),
                        ("浅灰", [200, 200, 200]),
                        ("黑", [0, 0, 0]),
                    ] {
                        if bar_chip(ui, name, state.render_watermark.color == rgb).clicked() {
                            state.render_watermark.color = rgb;
                            history::note(
                                "render.watermark.color",
                                crate::i18n::tr_fmt("水印颜色 {}", &[&crate::i18n::tr(name)]),
                            );
                        }
                    }
                });
            }
            if !state.render_watermark.is_on() {
                note(ui, "正文、图片与不透明度至少填一项。");
            }
        }
    }
    if group_header(ui, state, "render-time", "时间", true) {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
        let p = ui.painter();
        p.text(
            Pos2::new(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            crate::i18n::t!("帧率"),
            font(SIZE_SMALL),
            muted(),
        );
        in_field(ui, field(), field_hover(), |ui| {
            ui.put(
                Rect::from_min_size(
                    Pos2::new(rect.right() - 92.0, rect.center().y - 9.0),
                    Vec2::new(92.0, 18.0),
                ),
                egui::DragValue::new(&mut state.output_fps)
                    .range(1.0..=120.0)
                    .speed(1.0)
                    .fixed_decimals(0)
                    .suffix(" fps"),
            );
        });
        let dur = match state.retime_estimate() {
            // 片长说的是导出来的东西：变速开着就按变速后的秒数算。
            Ok((_, seconds)) => seconds as f32,
            Err(_) => state.frame_count() as f32 / state.output_fps.max(1.0),
        };
        kv(
            ui,
            "片长",
            crate::i18n::tr_fmt("{} 秒", &[&format!("{dur:.1}")]),
            muted(),
        );
    }
    if group_header(ui, state, "render-retime", "变速 · 运动模糊", true) {
        let mut enabled = state.retime.enabled;
        if ui
            .checkbox(
                &mut enabled,
                RichText::new(crate::i18n::t!("重映射时间（变速 + 运动模糊）")).size(SIZE_SMALL),
            )
            .changed()
        {
            state.retime.enabled = enabled;
            history::note("retime.enabled", "变速开关");
        }
        if enabled {
            let flat = flat_speed(&state.retime);
            ui.horizontal_wrapped(|ui| {
                for speed in [0.5, 1.0, 2.0, 4.0, 8.0] {
                    let label = format!("{speed}×");
                    if bar_chip(ui, &label, flat == Some(speed)).clicked() {
                        state.retime = Retime::uniform(speed, state.retime.shutter);
                        history::note("retime.speed", crate::i18n::tr_fmt("倍速 {}", &[&label]));
                    }
                }
                // 曲线不平时就是画过形状（缓入缓出）：芯片按「平不平」亮，不去猜具体几个键。
                if bar_chip(ui, "缓入缓出", flat.is_none()).clicked() {
                    let peak = retime::speed_at(&state.retime.keys, 0.5);
                    state.retime = Retime::ease(1.0, peak, 0.25, 0.75, state.retime.shutter);
                    history::note("retime.ease", "变速缓入缓出");
                }
            });
            ui.add_space(2.0);
            if let Some(speed) = flat {
                let mut v = speed;
                if slider(
                    ui,
                    "retime.speed",
                    "倍速",
                    &mut v,
                    0.05,
                    16.0,
                    1.0,
                    2,
                    false,
                ) {
                    state.retime = Retime::uniform(v, state.retime.shutter);
                }
            } else {
                // 曲线态下倍速不是一个数，推子会把它抹平成匀速——只给一行读数。
                let at = retime::speed_at(&state.retime.keys, 0.0);
                let mid = retime::speed_at(&state.retime.keys, 0.5);
                let end = retime::speed_at(&state.retime.keys, 1.0);
                kv(
                    ui,
                    "曲线",
                    format!("{}× → {}× → {}×", at, mid, end),
                    muted(),
                );
            }
            // 拖点编辑：键表空着（刚勾上开关）时先摆两个 1× 端点，第一次拖就落进真实键表。
            // 比色调曲线那格矮一档：这组上下还有芯片、快门与读数，方框吃满会把它们挤出视野。
            let box_side = ui.available_width().clamp(150.0, 208.0);
            let mut keys = if state.retime.keys.is_empty() {
                vec![
                    SpeedKey {
                        at: 0.0,
                        speed: 1.0,
                    },
                    SpeedKey {
                        at: 1.0,
                        speed: 1.0,
                    },
                ]
            } else {
                state.retime.keys.clone()
            };
            if widgets::speed_box(ui, "retime", &mut keys, box_side, &mut state.retime_grab) {
                state.retime.keys = keys;
                history::note("retime.keys", "速度曲线");
            }
            let mut pct = state.retime.shutter * 100.0;
            if slider(
                ui,
                "retime.shutter",
                "快门（模糊量）",
                &mut pct,
                0.0,
                100.0,
                0.0,
                0,
                false,
            ) {
                state.retime.shutter = (pct / 100.0).clamp(0.0, 1.0);
            }
            match state.retime_estimate() {
                Ok((frames, seconds)) => {
                    kv(
                        ui,
                        "变速后",
                        crate::i18n::tr_fmt(
                            "{} 帧 · {} 秒",
                            &[&frames, &format!("{:.1}", seconds)],
                        ),
                        accent(),
                    );
                }
                Err(e) => {
                    ui.add(
                        egui::Label::new(RichText::new(e).size(SIZE_SMALL).color(danger())).wrap(),
                    );
                }
            }
        }
    }
    if group_header(ui, state, "render-motion", "运镜 · Ken Burns", true) {
        let mut enabled = state.motion.enabled;
        if ui
            .checkbox(
                &mut enabled,
                RichText::new(crate::i18n::t!("启用运镜")).size(SIZE_SMALL),
            )
            .changed()
        {
            state.set_motion_enabled(enabled);
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            // 选中的那一端才在画布上带把手：两端同时可拖会抢同一个把手命中区。
            for w in [MotionBox::Start, MotionBox::End] {
                let on = state.motion_picking && state.motion_box == w;
                if bar_chip(ui, w.cn(), on).clicked() {
                    state.set_motion_box(w);
                    state.set_motion_picking(true);
                }
            }
            if bar_chip(ui, "拖取景框", state.motion_picking).clicked() {
                let on = !state.motion_picking;
                state.set_motion_picking(on);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for e in crop::Ease::ALL {
                if bar_chip(ui, e.cn(), state.motion.ease == e).clicked() {
                    state.set_motion_ease(e);
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.small_button(crate::i18n::t!("反向")).clicked() {
                state.motion_swap();
            }
            if ui.small_button(crate::i18n::t!("铺满")).clicked() {
                state.motion_fill_box();
            }
            if state.motion != crop::Motion::default()
                && ui.small_button(crate::i18n::t!("清零")).clicked()
            {
                state.motion_reset();
            }
        });
        ui.add_space(4.0);
        let (fw, fh) = state.motion_frame_size();
        let (ow, oh) = state.motion_output_size();
        let s = state.motion_box_rect(MotionBox::Start);
        let e = state.motion_box_rect(MotionBox::End);
        let zf = state.motion.from.zoom(ow, oh, fw, fh);
        let zt = state.motion.to.zoom(ow, oh, fw, fh);
        kv(ui, "起点", format!("{} × {} · {zf:.2}×", s.w, s.h), muted());
        kv(ui, "终点", format!("{} × {} · {zt:.2}×", e.w, e.h), muted());
        if state.motion_picking {
            kv(
                ui,
                "画布",
                crate::i18n::tr_fmt("拖 {} 那一端", &[&crate::i18n::tr(state.motion_box.cn())]),
                accent(),
            );
        }
        if zf.max(zt) > 2.0 {
            ui.add(
                egui::Label::new(
                    RichText::new(crate::i18n::t!(
                        "推近已经过 2×：再推就是拿少数像素撑大画面，会糊。"
                    ))
                    .size(SIZE_SMALL)
                    .color(warn()),
                )
                .wrap(),
            );
        }
    }
    if state.render_mode == OutputMode::Video
        && group_header(ui, state, "render-codec", "编码", true)
    {
        ui.horizontal_wrapped(|ui| {
            for codec in ["HEVC", "H.264"] {
                if bar_chip(ui, codec, state.codec == codec).clicked() {
                    state.codec = codec.to_owned();
                    // H.264 那条是 8bit，装不下 PQ。选它就把动态范围退回 SDR——可见地
                    // 退，不是偷偷留一个点导出才报错的组合。
                    //
                    // H.264 is 8bit and cannot carry PQ. Picking it drops the dynamic range
                    // back to SDR — visibly, rather than leaving a combination that only
                    // fails when export is pressed.
                    if state.codec != "HEVC" {
                        state.render_dynamic = OutputDynamic::Sdr;
                    }
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("质量 CRF"))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
            let crf = in_field(ui, field(), field_hover(), |ui| {
                ui.add(
                    egui::DragValue::new(&mut state.render_crf)
                        .range(0..=51)
                        .speed(0.2),
                )
            });
            crf.on_hover_text(crate::i18n::t!("数值越小越清晰"));
        });
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            for (preset, label) in [
                ("ultrafast", "极速"),
                ("fast", "快"),
                ("medium", "均衡"),
                ("slow", "慢"),
                ("veryslow", "很慢"),
            ] {
                if bar_chip(ui, label, state.render_preset == preset).clicked() {
                    state.render_preset = preset.to_owned();
                }
            }
        });
        let video = theme::ellipsis(&state.render_video_out.display().to_string(), 30);
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new(crate::i18n::tr_fmt("成片 · {}", &[&video]))
                    .size(SIZE_SMALL)
                    .color(dim()),
            )
            .wrap(),
        );
        if bar_button(ui, "选择成片位置…").clicked()
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("视频", &["mp4", "mov", "mkv"])
                .set_file_name(
                    state
                        .render_video_out
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("成片.mp4"),
                )
                .save_file()
        {
            state.render_video_out = path;
        }
    }
    if group_header(ui, state, "render-export", "导出", false) {
        let ctx = ui.ctx().clone();
        let w = ui.available_width();
        let busy = state.export_busy;
        let label = if busy {
            "正在导出…"
        } else if state.render_mode == OutputMode::Video {
            "导出成片"
        } else {
            "导出静帧序列"
        };
        let resp = ui.add_enabled(
            !busy,
            egui::Button::new(
                RichText::new(label)
                    .size(SIZE_SMALL)
                    .color(on_accent())
                    .strong(),
            )
            .fill(accent())
            .corner_radius(radius_control())
            .min_size(Vec2::new(w, 26.0)),
        );
        if resp.clicked() {
            state.export_copy = None;
            state.run_export(&ctx);
        }
        if busy {
            if let Some((done, total, stage)) = state.export_progress() {
                // 进度条按阶段分区间：`done` 是**阶段内**计数（写帧 0..total，编码又从 1
                // 起数），直接当比例会让进度条在阶段切换时从 100% 退回十几——用户看到
                // 条子倒退，会以为任务重启了。写帧占 0–70%、编码占 70–100%，两段各自
                // 单调，接缝只跨一次。文案仍然分阶段显示，不做「假一体化」。
                //
                // The bar maps stages to ranges: `done` is a **per-stage** count (frames
                // 0..total, then the encode restarts at 1), so using it directly makes the
                // bar fall from 100% back to the teens at the switch, which reads as "the
                // job restarted". Frames take 0-70%, encode 70-100%; each segment is
                // monotone and the seam is crossed once. The text still names the stage.
                let frac = export_bar_fraction(stage, done, total);
                let text = match stage {
                    aevum_core::export::Stage::Frames => {
                        crate::i18n::tr_fmt("写帧 {} / {}", &[&done, &total])
                    }
                    aevum_core::export::Stage::Encode => {
                        crate::i18n::tr_fmt("编码 {} / {}", &[&done, &total])
                    }
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_height(4.0)
                        .text(RichText::new(text).size(SIZE_SMALL)),
                );
            }
            if bar_button(ui, "取消导出").clicked() {
                state.stop_export();
            }
        }
        if let Some(path) = &state.last_saved {
            ui.add(
                egui::Label::new(
                    RichText::new(crate::i18n::tr_fmt(
                        "已保存 · {}",
                        &[&theme::ellipsis(path, 34)],
                    ))
                    .size(SIZE_SMALL)
                    .color(ok()),
                )
                .wrap(),
            );
        }
        ui.add_space(4.0);
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(
                    RichText::new(crate::i18n::tr_fmt(
                        "导出当前帧 · {}",
                        &[&crate::i18n::tr(state.render_format.cn())],
                    ))
                    .size(SIZE_SMALL),
                )
                .min_size(Vec2::new(w, 24.0)),
            )
            .clicked()
        {
            state.export_current(&ctx);
        }
    }
    if group_header(ui, state, "render-queue", "队列", false) {
        let busy = state.queue_busy;
        let count = state.queue.len();
        ui.horizontal_wrapped(|ui| {
            let resp = ui.add_enabled(!busy, {
                egui::Button::new(
                    RichText::new(crate::i18n::t!("加入当前序列"))
                        .size(SIZE_SMALL)
                        .color(text()),
                )
                .fill(elev())
                .stroke(Stroke::NONE)
                .corner_radius(radius_control())
                .min_size(Vec2::new(0.0, 22.0))
            });
            if resp.clicked()
                && let Err(e) = state.add_current_to_queue()
            {
                state.error = Some(e);
            }
            if busy {
                if bar_button(ui, "取消队列").clicked() {
                    state.stop_queue();
                }
            } else {
                let ctx = ui.ctx().clone();
                if bar_primary(ui, "开始渲染队列").clicked()
                    && let Err(e) = state.start_queue(&ctx)
                {
                    state.error = Some(e);
                }
            }
            if !busy && count > 0 && bar_button(ui, "清空").clicked() {
                state.clear_queue_rows();
            }
        });
        kv(
            ui,
            "队列",
            crate::i18n::tr_fmt("{} 段 · {}", &[&count, &queue_counts(state)]),
            if count > 0 { text() } else { dim() },
        );
        if count == 0 {
            note(ui, "尚无排队段。");
        } else {
            queue_table(state, ui, busy);
            if busy && let Some((done, total, stage)) = state.queue_progress() {
                let frac = if total > 0 {
                    done as f32 / total as f32
                } else {
                    0.0
                };
                let current = state.queue_current.map(|i| i + 1).unwrap_or(1);
                let phase = match stage {
                    aevum_core::export::Stage::Frames => "写帧",
                    aevum_core::export::Stage::Encode => "编码",
                };
                ui.add(
                    egui::ProgressBar::new(frac).desired_height(4.0).text(
                        RichText::new(crate::i18n::tr_fmt(
                            "第 {}/{} 段 · {} {}/{}",
                            &[&current, &count, &phase, &done, &total],
                        ))
                        .size(SIZE_SMALL),
                    ),
                );
            }
        }
        ui.add(
            egui::Label::new(
                RichText::new(crate::i18n::tr_fmt(
                    "队列文件 · {}",
                    &[&theme::ellipsis(
                        &state.queue_path.display().to_string(),
                        30,
                    )],
                ))
                .size(SIZE_SMALL)
                .color(dim()),
            )
            .wrap(),
        );
    }
}

/// 队列表格：LapseOne 的导出队列口径——列头 + 每行一条自己的进度。
/// 之前整屏只有一行「第 x/y 段」，排上七八段就看不出哪段在跑、哪段已经好了。
fn queue_table(state: &mut AppState, ui: &mut Ui, busy: bool) {
    const ROW_H: f32 = 22.0;
    const COL_STATUS: f32 = 42.0;
    const COL_SPEC: f32 = 76.0;
    const COL_BAR: f32 = 88.0;
    const GAP: f32 = 6.0;
    let width = ui.available_width();
    let name_w = (width - COL_STATUS - COL_SPEC - COL_BAR - GAP * 3.0 - 16.0).max(48.0);
    let x0 = ui.cursor().left();
    let x_name = x0 + COL_STATUS + GAP;
    let x_spec = x_name + name_w + GAP;
    let x_bar = x_spec + COL_SPEC + GAP;
    let p = ui.painter().clone();

    let (head, _) = ui.allocate_exact_size(Vec2::new(width, 16.0), Sense::hover());
    for (x, text) in [
        (x0, "状态"),
        (x_name, "序列"),
        (x_spec, "规格"),
        (x_bar, "进度"),
    ] {
        p.text(
            Pos2::new(x, head.center().y),
            Align2::LEFT_CENTER,
            text,
            font(SIZE_SMALL),
            dim(),
        );
    }
    theme::rule(&p, head.left(), head.right(), head.bottom(), divider());

    let mut remove = None;
    egui::ScrollArea::vertical()
        .max_height(150.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (index, item) in state.queue.iter().enumerate() {
                let (rr, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::hover());
                let current = busy && state.queue_current == Some(index);
                if current {
                    p.rect_filled(rr.shrink(1.0), radius_control(), selected());
                }
                let (frac, status) = if current {
                    let (done, total, _) =
                        state
                            .queue_progress()
                            .unwrap_or((0, 0, aevum_core::export::Stage::Frames));
                    (
                        if total > 0 {
                            (done as f32 / total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        },
                        "渲染中",
                    )
                } else {
                    (
                        if item.status == QueueStatus::Done {
                            1.0
                        } else {
                            0.0
                        },
                        item.status.label(),
                    )
                };
                let status_color = match item.status {
                    QueueStatus::Done => ok(),
                    QueueStatus::Failed => danger(),
                    QueueStatus::Pending => {
                        if current {
                            accent()
                        } else {
                            dim()
                        }
                    }
                };
                p.text(
                    Pos2::new(x0, rr.center().y),
                    Align2::LEFT_CENTER,
                    status,
                    font(SIZE_SMALL),
                    status_color,
                );
                let name = std::path::Path::new(&item.root)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| item.root.clone());
                p.text(
                    Pos2::new(x_name, rr.center().y),
                    Align2::LEFT_CENTER,
                    theme::fit_text(&p, &name, font(SIZE_SMALL), text(), name_w),
                    font(SIZE_SMALL),
                    if current { Color32::WHITE } else { text() },
                );
                let spec = format!("{}×{} {}", item.width, item.height, item.codec);
                p.text(
                    Pos2::new(x_spec, rr.center().y),
                    Align2::LEFT_CENTER,
                    theme::fit_text(&p, &spec, mono(10.0), muted(), COL_SPEC),
                    mono(10.0),
                    muted(),
                );
                // 进度：绿填充 + 居中百分比，排队中是空心轨道——和 LapseOne 同一读法。
                let bar = Rect::from_min_max(
                    Pos2::new(x_bar, rr.center().y - 5.5),
                    Pos2::new(x_bar + COL_BAR - 18.0, rr.center().y + 5.5),
                );
                p.rect_stroke(
                    bar,
                    3.0,
                    Stroke::new(1.0, if frac > 0.0 { ok() } else { rail_fill() }),
                    StrokeKind::Inside,
                );
                if frac > 0.0 {
                    p.rect_filled(
                        Rect::from_min_max(
                            bar.min,
                            Pos2::new(bar.left() + bar.width() * frac, bar.bottom()),
                        ),
                        3.0,
                        ok().gamma_multiply(0.85),
                    );
                }
                p.text(
                    bar.center(),
                    Align2::CENTER_CENTER,
                    if item.status == QueueStatus::Failed {
                        "失败".to_owned()
                    } else if frac > 0.0 {
                        format!("{:.0}%", frac * 100.0)
                    } else {
                        "排队".to_owned()
                    },
                    mono(10.0),
                    if frac > 0.0 { Color32::WHITE } else { dim() },
                );
                if !busy {
                    let xrect = Rect::from_min_size(
                        Pos2::new(rr.right() - 16.0, rr.center().y - 8.0),
                        Vec2::splat(16.0),
                    );
                    let del = ui.interact(
                        xrect,
                        ui.make_persistent_id(("queue-del", index)),
                        Sense::click(),
                    );
                    p.text(
                        xrect.center(),
                        Align2::CENTER_CENTER,
                        "×",
                        font(SIZE_BODY),
                        if del.hovered() { danger() } else { dim() },
                    );
                    if del.clicked() {
                        remove = Some(index);
                    }
                }
                resp.on_hover_text(crate::i18n::tr_fmt(
                    "{}\n{}",
                    &[&item.out, &crate::i18n::tr(&item.note)],
                ));
            }
        });
    if let Some(index) = remove {
        state.remove_queue_at(index);
    }
}

fn queue_counts(state: &AppState) -> String {
    let done = state
        .queue
        .iter()
        .filter(|i| i.status == QueueStatus::Done)
        .count();
    let failed = state
        .queue
        .iter()
        .filter(|i| i.status == QueueStatus::Failed)
        .count();
    if done == 0 && failed == 0 {
        crate::i18n::t!("待渲").to_owned()
    } else {
        crate::i18n::tr_fmt("完成 {} · 失败 {}", &[&done, &failed])
    }
}

/// 导出进度条的显示比例：把阶段内完成度映射到该阶段占的区间（写帧 0–70%、编码
/// 70–100%）。`done` 是**阶段内**计数（写帧从 0 数到 total，编码又按 ffmpeg 回报
/// 从 1 起数），直接当全局比例会让条子在阶段切换时后退——用户会以为任务重启。
///
/// Export bar fraction: maps a per-stage done/total onto the range that stage owns
/// (frames 0-70%, encode 70-100%). Feeding the per-stage count straight into the ratio
/// makes the bar step backwards at the switch, which reads as "the job restarted".
fn export_bar_fraction(stage: aevum_core::export::Stage, done: usize, total: usize) -> f32 {
    use aevum_core::export::Stage;
    let inner = if total > 0 {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (lo, hi) = match stage {
        Stage::Frames => (0.0f32, 0.7f32),
        Stage::Encode => (0.7f32, 1.0f32),
    };
    lo + (hi - lo) * inner
}

#[cfg(test)]
/// 导出进度条的阶段区间映射：全程单调、接缝不回退、两段都走到满格。
mod export_bar_tests {
    use super::*;
    use aevum_core::export::Stage;

    #[test]
    fn the_bar_never_goes_backwards_across_the_stage_seam() {
        // 写帧走完是 70%，编码第一帧就已经越过它——旧算法在这一步从 100% 退回 17%。
        let mut last = 0.0f32;
        let mut path = Vec::new();
        for done in 0..=6 {
            let f = export_bar_fraction(Stage::Frames, done, 6);
            assert!(f >= last, "写帧阶段回退：{done} → {f} < {last}");
            last = f;
            path.push(f);
        }
        assert!((last - 0.7).abs() < 1e-6, "写帧阶段最高只到 70%：{last}");
        // 编码阶段从 ffmpeg 的 1 起数（首块通常是 frame=1），必须 ≥ 70%。
        for done in 1..=6 {
            let f = export_bar_fraction(Stage::Encode, done, 6);
            assert!(f >= last, "接缝回退：编码 {done}/6 → {f} < {last}");
            assert!(f <= 1.0, "比例越过 100%：{f}");
            last = f;
            path.push(f);
        }
        assert!((last - 1.0).abs() < 1e-6, "编码走完必须是 100%：{last}");
        // 首尾都要在 [0,1] 内，且空 total 不产生 NaN。
        assert_eq!(export_bar_fraction(Stage::Frames, 0, 0), 0.0);
        assert!(export_bar_fraction(Stage::Encode, 0, 0) >= 0.7);
        assert!(path.iter().all(|f| (0.0..=1.0).contains(f)));
    }
}
