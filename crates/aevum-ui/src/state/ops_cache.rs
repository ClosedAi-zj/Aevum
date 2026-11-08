//! 三层缓存与派生取值：蒙版几何的缓存与刷新、自动曝光、预览参数、预览与缩略图的按需生成。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! The three cache layers and derived reads: Caching and refreshing mask geometry, auto exposure, preview parameters, and on-demand preview and thumbnail generation.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------- 三层缓存取值

    /// 蒙版几何取值：指纹命中直接还缓存切片，零分配（E1/E2/E3 三调用点统一入口）。
    /// 未命中重解析 `params.masks[sel]`；派生量在各自取值方法里按 img 键补建。
    pub(crate) fn mask_geos_cached(&mut self, sel: usize) -> &[crate::shell::MaskGeo] {
        if sel >= self.params.masks.len() {
            self.mask_geo_cache = Default::default();
            return &[];
        }
        let fp = crate::shell::mask_geo_fingerprint(&self.params.masks[sel]);
        if self.mask_geo_cache.fp != fp {
            let geos = crate::shell::mask_geos(&self.params.masks[sel]);
            self.mask_geo_cache = crate::shell::MaskGeoCache {
                fp,
                geos,
                ..Default::default()
            };
        }
        &self.mask_geo_cache.geos
    }

    /// 画布帧准备：一帧只做一次指纹/img 双键检查，确保几何与全部派生量就绪，
    /// 返回形状个数；随后的 `mask_geo_get` / `mask_*_now` 是纯读，零哈希。
    /// （每帧多次指纹重哈希在多形状蒙版上可测，见 T03 复测记录。）
    pub(crate) fn mask_prep(&mut self, sel: usize, img: egui::Rect, w: f64, h: f64) -> usize {
        self.mask_geos_cached(sel);
        let current = self
            .mask_comp
            .min(self.mask_geo_cache.geos.len().saturating_sub(1));
        // 派生量重建键必须含「img rect + 像素尺寸 + 当前形状」三者：
        // 漏 comp 会让手柄停在旧形状上（切换形状不跟随），
        // 漏 (w,h) 会让换预览分辨率后按旧像素尺寸映射（径向/笔画错位）。
        if self.mask_geo_cache.img != Some(img)
            || self.mask_geo_cache.px != (w, h)
            || self.mask_geo_cache.comp != current
        {
            self.refresh_mask_derived(sel, current, img, w, h);
        }
        self.mask_geo_cache.geos.len()
    }

    /// 纯读：当前缓存里第 `index` 个形状的几何（需先 `mask_prep`）。
    pub(crate) fn mask_geo_get(&self, index: usize) -> Option<crate::shell::MaskGeo> {
        self.mask_geo_cache.geos.get(index).copied()
    }

    /// 纯读：当前形状的手柄（需先 `mask_prep`）。
    pub(crate) fn mask_handles_now(&self) -> &[(egui::Pos2, bool)] {
        &self.mask_geo_cache.handles
    }

    /// 纯读：第 `comp` 个形状的径向轮廓（需先 `mask_prep`）。
    pub(crate) fn mask_radial_now(&self, comp: usize) -> &[egui::Pos2] {
        self.mask_geo_cache
            .radial
            .get(comp)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 纯读：第 `comp` 个形状的笔画折线（需先 `mask_prep`）。
    pub(crate) fn mask_strokes_now(&self, comp: usize) -> &[(Vec<egui::Pos2>, bool)] {
        self.mask_geo_cache
            .strokes
            .get(comp)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// 派生量重建：只在「指纹刚换或 img 变了」时跑；几何解析层不重算。
    fn refresh_mask_derived(
        &mut self,
        sel: usize,
        current: usize,
        img: egui::Rect,
        w: f64,
        h: f64,
    ) {
        let geos: Vec<crate::shell::MaskGeo> = self.mask_geo_cache.geos.clone();
        let Some(raw) = self.params.masks.get(sel) else {
            return;
        };
        let handles = geos
            .get(current)
            .map(|g| crate::shell::mask_handles(g, img, w, h))
            .unwrap_or_default();
        let mut radial = Vec::with_capacity(geos.len());
        let mut strokes = Vec::with_capacity(geos.len());
        for (i, g) in geos.iter().enumerate() {
            radial.push(if g.radial {
                crate::shell::mask_radial_points(g, img, w, h)
            } else {
                Vec::new()
            });
            strokes.push(match crate::shell::mask_comp_object(raw, i) {
                Some(obj) if g.brush => crate::shell::mask_stroke_lines(obj, img),
                _ => Vec::new(),
            });
        }
        let cache = &mut self.mask_geo_cache;
        cache.img = Some(img);
        cache.px = (w, h);
        cache.comp = current;
        cache.handles = handles;
        cache.radial = radial;
        cache.strokes = strokes;
    }

    /// 一键自动：先按 1/50/99 百分位提一版白平衡与影调，再走真实链验收。
    ///
    /// 写的是绝对值，不是在当前值上再加一档——所以连点几次结果不变；已经曝好的
    /// 图可能只校白平衡，那不是没生效，是这张图本来就不用动。
    ///
    /// 测光代理（256 长边解码）没缓存时在后台解码（整幅 RAW 也要 0.3–1.5 秒，
    /// 放界面线程上就是一下可见的冻结），期间状态栏挂一句「测光中…」；回包经
    /// [`Msg::AutoProbed`] 落账，若是当前帧就自动再算一次曝光并刷新画面。同一帧
    /// 的在飞请求不重复发（防抖）。无显示环境（selftest）不启动读盘线程，退回
    /// 前台同步解码，判据测试才能当场断言。
    ///
    /// One-click auto: first propose a version of white balance and tone from the 1 / 50 / 99
    /// percentiles, then accept it through the real chain.
    ///
    /// It writes absolute values rather than adding a step to the current ones -- so
    /// clicking it several times gives the same result; an already well-exposed image may only
    /// get its white balance corrected, which is not a no-op, it is an image that needed
    /// nothing else.
    ///
    /// When the 256-px metering proxy is not cached it is decoded in the background (even a
    /// proxy of a whole RAW costs 0.3-1.5 s, which on the UI thread is a visible freeze); the
    /// status bar shows "metering…" meanwhile, the reply lands via [`Msg::AutoProbed`], and a
    /// reply for the frame on screen recomputes the exposure and repaints. A repeat request
    /// for the same frame is suppressed while one is in flight (debounce). A display-less
    /// environment (selftest) starts no disk threads and falls back to a synchronous decode so
    /// the tests can assert results on the spot.
    pub fn auto_exposure(&mut self, ctx: &Context) {
        let Some(path) = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.path.clone())
        else {
            self.auto_note = Some("还没有可测光的帧".to_owned());
            return;
        };
        let cached = self
            .auto_probe
            .as_ref()
            .filter(|(p, ..)| *p == path)
            .map(|(_, px, w, h)| (px.clone(), *w, *h));
        let (probe, w, h) = match cached {
            Some(v) => v,
            None => {
                if self.headless {
                    match metrics::decode_rgb(&path, 256) {
                        Ok(img) => {
                            let (w, h) = (img.width() as usize, img.height() as usize);
                            let px: Vec<f32> =
                                img.as_raw().iter().map(|v| *v as f32 / 255.0).collect();
                            self.auto_probe = Some((path.clone(), px.clone(), w, h));
                            (px, w, h)
                        }
                        Err(e) => {
                            self.auto_note = Some(crate::i18n::tr_fmt("自动校正失败：{}", &[&e]));
                            return;
                        }
                    }
                } else {
                    // 在飞的同一帧不重复发；别的帧允许换探针（回包按路径对号落缓存）。
                    //
                    // A repeat for the frame in flight is suppressed; a different frame may
                    // replace the probe (replies land in the cache keyed by path).
                    if self.auto_probe_inflight.as_deref() == Some(path.as_path()) {
                        self.auto_note = Some(crate::i18n::t!("测光中…").to_owned());
                        return;
                    }
                    self.auto_probe_inflight = Some(path.clone());
                    self.auto_note = Some(crate::i18n::t!("测光中…").to_owned());
                    let token = self.reload_token;
                    let tx = self.tx.clone();
                    let ctx2 = ctx.clone();
                    thread::spawn(move || {
                        let result = match metrics::decode_rgb(&path, 256) {
                            Ok(img) => {
                                let (w, h) = (img.width() as usize, img.height() as usize);
                                let px: Vec<f32> =
                                    img.as_raw().iter().map(|v| *v as f32 / 255.0).collect();
                                Ok((px, w, h))
                            }
                            Err(e) => Err(crate::i18n::tr_fmt("自动校正失败：{}", &[&e])),
                        };
                        let _ = tx.send(Msg::AutoProbed {
                            token,
                            path,
                            result,
                        });
                        ctx2.request_repaint();
                    });
                    return;
                }
            }
        };
        let q = aevum_core::auto::auto_params(&probe, w, h);
        self.params.exposure = q.exposure;
        self.params.contrast = q.contrast;
        self.params.highlights = q.highlights;
        self.params.shadows = q.shadows;
        self.params.whites = q.whites;
        self.params.blacks = q.blacks;
        self.params.temperature = q.temperature;
        self.params.tint = q.tint;
        self.auto_note = Some(if q.touches_tone() {
            crate::i18n::tr_fmt(
                "自动：{} EV · 对比 {} · 高光 {} · 阴影 {} · 白 {} · 黑 {} · 色温 {} · 色调 {}",
                &[
                    &format!("{:+.2}", q.exposure),
                    &format!("{:+.0}", q.contrast),
                    &format!("{:+.0}", q.highlights),
                    &format!("{:+.0}", q.shadows),
                    &format!("{:+.0}", q.whites),
                    &format!("{:+.0}", q.blacks),
                    &format!("{:+.0}", q.temperature),
                    &format!("{:+.0}", q.tint),
                ],
            )
        } else {
            crate::i18n::tr_fmt(
                "影调已在安全范围，只校白平衡（色温 {} · 色调 {}）",
                &[
                    &format!("{:+.0}", q.temperature),
                    &format!("{:+.0}", q.tint),
                ],
            )
        });
        self.preview_dirty = true;
        history::note(
            "auto-exposure",
            crate::i18n::tr_fmt(
                "自动校正 {} EV · 色温 {}",
                &[
                    &format!("{:+.2}", q.exposure),
                    &format!("{:+.0}", q.temperature),
                ],
            ),
        );
    }

    /// 预览用的参数：旁路里的分组写回默认。**导出不走这一份**——眼睛是对比
    /// 工具不是调色状态，按着眼睛看到的样子不该溜进成片；成片永远用
    /// `self.params` 原样渲染。
    ///
    /// Parameters for the preview: bypassed groups are written back to their defaults.
    /// **Export never goes through this** -- the eye is a comparison aid, not grading
    /// state, and what you saw while holding it must not leak into the deliverable;
    /// exports always render `self.params` as-is.
    pub fn preview_params(&self) -> GradeParams {
        let mut p = self.params.clone();
        for g in &self.bypassed {
            crate::controls::neutralise_group(&mut p, g);
        }
        // 拖四角时预览要显示**未校正**的画面：四边形画在原图的梯形上才拖得准。带着
        // 校正显示等于让人对着已经拉正的画面猜原来的角在哪儿，拖一格画面跳一下。
        // 与旁路同一个性质：只改预览，导出与工程文件仍用 `self.params`。
        //
        // While dragging corners the preview has to show the **uncorrected** frame: the quad
        // is drawn over the original trapezoid, which is what makes dragging accurate.
        // Showing it corrected would mean guessing where the corners used to be while the
        // picture jumps on every step. Same nature as bypass: preview only; export and the
        // project file still use `self.params`.
        if self.upright_editing {
            p.upright = None;
        }
        p
    }

    pub fn ensure_preview(&mut self, ctx: &Context) {
        // 按住鼠标就是在拖某个推子/曲线/蒙版把手：这时候先要的是跟手，不是那几项
        // 最贵的空间细节，所以出草图。松手才补全量——起手那一帧什么都不做，
        // 否则点一下任意按钮都会让画面闪一下草图。
        let dragging = ctx.input(|i| i.pointer.any_down());
        if self.preview_dragging && !dragging && self.preview_owes_full {
            self.preview_dirty = true;
        }
        // 刚开分屏：手边没有「之前」那张，得补一张带原图的。
        if self.split && !self.preview_has_orig && !self.preview_inflight {
            self.preview_dirty = true;
        }
        self.preview_dragging = dragging;
        if self.headless || !self.preview_dirty || self.preview_inflight || self.playing {
            return;
        }
        let sketch = dragging;
        // 已经有画面（贴图在手上）才敢用全分辨率出草图：那时解码缓存是热的，多出的
        // 只是调色那一遍的像素量，而画质和松手后的全量一致——拖动期间不再糊。
        // 一帧都还没出来的时候（冷首屏）仍旧减分辨率抢时间，那是唯一值得拿清晰度
        // 换延迟的时刻。
        //
        // Full-resolution sketches are only allowed once a picture is on screen: the decode
        // cache is warm then, the only extra cost is grading more pixels, and the result
        // matches the full frame in sharpness -- no more blur while dragging. With nothing on
        // screen yet (cold first screen) the resolution is still halved to win latency, which
        // is the only moment trading sharpness for time is worth it.
        let decode_half = sketch && self.texture.is_none();
        let want_original = self.split;
        let Some(path) = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.path.clone())
        else {
            return;
        };
        self.preview_dirty = false;
        self.preview_inflight = true;
        self.preview_stamp += 1;
        let stamp = self.preview_stamp;
        let params = self.preview_params();
        let gain = self.ev_at(self.selected);
        let (crop, offset) = if self.align_preview {
            self.alignment_at(self.selected)
        } else {
            (None, [0.0, 0.0])
        };
        // 裁剪态下画整块可见区：框外那圈像素得露出来才拖得动，取景框只是覆盖层。
        // 拉直之后可见区就是安全框——转出去的反射角不画出来，画布上不会看到鬼影。
        // 与画布上的落点换算共用 `preview_crop`：两处各写一份，迟早分叉。
        //
        // Shares `preview_crop` with the canvas coordinate mapping: two copies would drift.
        let _ = crop;
        let crop = self.preview_crop();
        let full_size = self.resolution;
        let framing = Framing {
            angle_deg: self.rotate,
            off: offset,
        };
        // 运镜预览按源帧序取框（开了变速时成片由输出帧号定，预览只作近似）。裁剪态
        // 画的是整块可见区，拖取景框时画的是取景基准那一幅——两种态下框都只是盖在
        // 像素上的矢量，不重画。
        let n = self.frame_count();
        let view = if self.vector_editing() {
            None
        } else {
            self.motion_view_at(self.selected.min(n.saturating_sub(1)), n)
        };
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        // 检视开关在**发任务那一刻**定下来：拖动中途切开关，这一帧仍按旧值渲染完，
        // 下一帧才换——比中途换链导致画面跳一半要稳。
        //
        // The inspection flag is fixed **when the job is posted**: toggling mid-drag lets
        // this frame finish on the old value and switches on the next one, which is steadier
        // than changing chains halfway through a frame.
        let hdr_fold = self.hdr_preview;
        thread::spawn(move || {
            match render_preview_file(
                &path,
                &params,
                gain,
                full_size,
                crop,
                framing,
                view,
                sketch,
                decode_half,
                want_original,
                hdr_fold,
            ) {
                Ok((graded, original, size, hist)) => {
                    let _ = tx.send(Msg::Preview(Box::new(PreviewMsg {
                        stamp,
                        sketch,
                        graded,
                        original,
                        size,
                        hist,
                    })));
                }
                Err(e) => {
                    let _ = tx.send(Msg::PreviewFailed(crate::i18n::tr_fmt(
                        "预览失败：{}",
                        &[&e],
                    )));
                }
            }
            ctx2.request_repaint();
        });
    }

    pub fn ensure_thumbs(&mut self, ctx: &Context, lo: usize, hi: usize) {
        if self.headless {
            return;
        }
        if self.seq.is_none() {
            return;
        }
        let hi = hi.min(self.frame_count());
        let params = self.preview_params();

        // 第一段：还没有解码源的帧，读盘解出 128px 预览，顺手按当前参数显影。
        // 相机配置换了，源也作废——`decode_thumb_source` 的解码口径跟着 profile 走，
        // 不重解就会拿旧 profile 的像素继续调色。
        //
        // Stage one: frames with no decoded source yet get the 128 px preview, developed
        // with the current parameters on the way out. A camera-profile change invalidates
        // the source too: `decode_thumb_source` decodes under the profile, so re-grading
        // old pixels would keep the previous profile's colour.
        let missing: Vec<(usize, PathBuf)> = {
            let seq = self.seq.as_ref().expect("上面已判空");
            (lo..hi)
                .filter(|i| {
                    !self.thumb_pending.contains(i)
                        && self
                            .thumb_src
                            .get(i)
                            .is_none_or(|s| s.profile != params.dcp_profile)
                })
                .take(24)
                .map(|i| (i, seq.frames[i].path.clone()))
                .collect()
        };
        if !missing.is_empty() {
            for (i, _) in &missing {
                self.thumb_pending.insert(*i);
            }
            let jobs: Vec<(usize, PathBuf, Ev)> = missing
                .into_iter()
                .map(|(i, path)| (i, path, self.ev_at(i)))
                .collect();
            let params = params.clone();
            let tx = self.tx.clone();
            let ctx2 = ctx.clone();
            thread::spawn(move || {
                let out: Vec<ThumbMsg> = thumb_pool().install(|| {
                    jobs.into_par_iter()
                        .filter_map(|(index, path, gain)| {
                            let src = decode_thumb_src(&path, &params.dcp_profile)?;
                            let (image, size) = grade_thumb(&src, &params, gain);
                            Some(ThumbMsg {
                                index,
                                image,
                                size,
                                src: Arc::new(src),
                                grade: params.clone(),
                            })
                        })
                        .collect()
                });
                let _ = tx.send(Msg::Thumbs(out));
                ctx2.request_repaint();
            });
        }

        // 第二段：缩略图的参数与画布不一致就重调色。拖推子时先不跟——手指还在
        // 动，缩略图每帧重排等于拿一批后台任务换一列噪点；松手那一帧自然补上。
        // 只处理可见区间：滚到哪补到哪，离屏格子的旧色没人看得见，等它进屏再说。
        //
        // Stage two: re-grade thumbnails whose parameters differ from the canvas. Not
        // while dragging -- the value is still moving, and re-grading every frame spends a
        // batch of jobs for a column of noise; the frame after release catches up. Only the
        // visible range is handled: offscreen cells are not looked at, so they wait.
        if ctx.input(|i| i.pointer.any_down()) || self.playing {
            return;
        }
        let stale: Vec<(usize, Arc<ThumbSrc>, Ev)> = (lo..hi)
            .filter(|i| !self.thumb_pending.contains(i) && !self.thumb_regrade.contains(i))
            .filter_map(|i| {
                let thumb = self.thumbs.get(&i)?;
                let src = self.thumb_src.get(&i)?;
                (thumb.grade.as_ref() != Some(&params)).then(|| (i, Arc::clone(src), self.ev_at(i)))
            })
            .collect();
        if stale.is_empty() {
            return;
        }
        for (i, _, _) in &stale {
            self.thumb_regrade.insert(*i);
        }
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let out: Vec<ThumbMsg> = thumb_pool().install(|| {
                stale
                    .into_par_iter()
                    .map(|(index, src, gain)| {
                        let (image, size) = grade_thumb(&src, &params, gain);
                        ThumbMsg {
                            index,
                            image,
                            size,
                            src,
                            grade: params.clone(),
                        }
                    })
                    .collect()
            });
            let _ = tx.send(Msg::Thumbs(out));
            ctx2.request_repaint();
        });
    }

    /// 播放代理的按需生成：与 [`AppState::ensure_thumbs`] 同构，但按 [`PROXY_SIDE`]
    /// 解码、走 [`Msg::Proxies`]、落进 `proxies`。代理仍走机身内嵌原片、不经显影链，
    /// 只是边长更大——播放态画布据此比 128px 缩略图清晰，暂停后回全质量。
    ///
    /// `lo..hi` 是要预取的帧区间（已按播放头夹过）；一次最多 24 张，上一批没落地
    /// 之前不重复排队。
    ///
    /// On-demand playback proxies: the same shape as [`AppState::ensure_thumbs`], but
    /// decoded at [`PROXY_SIDE`], delivered via [`Msg::Proxies`], and stored in `proxies`.
    /// The proxy still comes from the camera-embedded original and never enters the develop
    /// chain -- only its edge is larger, so the canvas looks sharper than the 128px
    /// thumbnail during playback and returns to full quality on pause.
    ///
    /// `lo..hi` is the frame range to prefetch (already clamped to the playhead); at most 24
    /// at a time, and no re-queueing until the previous batch lands.
    pub fn ensure_proxies(&mut self, ctx: &Context, lo: usize, hi: usize) {
        if self.headless {
            return;
        }
        let Some(seq) = &self.seq else { return };
        let missing: Vec<(usize, PathBuf)> = (lo..hi.min(seq.len()))
            .filter(|i| !self.proxies.contains_key(i) && !self.proxy_pending.contains(i))
            .take(24)
            .map(|i| (i, seq.frames[i].path.clone()))
            .collect();
        if missing.is_empty() {
            return;
        }
        for (i, _) in &missing {
            self.proxy_pending.insert(*i);
        }
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let out: Vec<ProxyMsg> = thumb_pool().install(|| {
                missing
                    .into_par_iter()
                    .filter_map(|(index, path)| {
                        let img = metrics::thumbnail(&path, PROXY_SIDE).ok()?;
                        let size = [img.width(), img.height()];
                        let image = ColorImage::from_rgba_unmultiplied(
                            [size[0] as usize, size[1] as usize],
                            img.as_raw(),
                        );
                        Some(ProxyMsg { index, image, size })
                    })
                    .collect()
            });
            let _ = tx.send(Msg::Proxies(out));
            ctx2.request_repaint();
        });
    }

    pub fn thumb(&self, i: usize) -> Option<&Thumb> {
        self.thumbs.get(&i)
    }
}

/// 解出胶片条用的 128px 显影源（sRGB 编码 f32 [0,1]），与画布同一条解码口径。
///
/// 不再像 1.9.7 那样读机身内嵌 JPEG：那张图是相机按 As Shot 出片、自带相机色调曲线，
/// 同一帧比画布解出的 RAW 亮近一档（128px 均值 78.9 对 62.0，用户 2026-11-07 的
/// 「小图与大图曝光不同」）。见 [`aevum_core::metrics::decode_thumb_source`]。
///
/// Decode the 128 px filmstrip source (sRGB-encoded f32 in [0,1]) through the canvas's
/// decoding convention. It no longer reads the camera-embedded JPEG, which is the camera's
/// As Shot rendering with its own tone curve and measured nearly a stop brighter than the
/// canvas RAW decode (78.9 vs 62.0 mean over 128 px -- the user's 2026-11-07 "small and
/// large preview differ in exposure"). See
/// [`aevum_core::metrics::decode_thumb_source`].
fn decode_thumb_src(path: &std::path::Path, profile: &str) -> Option<ThumbSrc> {
    let (rgb, w, h) = metrics::decode_thumb_source(path, profile, THUMB_SIDE).ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some(ThumbSrc {
        rgb,
        w,
        h,
        profile: profile.to_owned(),
    })
}

/// 按主画布同一份参数把缩略源显影成贴图。走草图链：128px 上看不出草图省掉的
/// 空间项，换来的是拖完一根推子后整条胶片条能在一帧内翻新。
///
/// Develop the thumbnail source with the same parameters as the canvas. The sketch chain
/// is used: the spatial terms it skips are invisible at 128 px, and the whole filmstrip
/// refreshes within a frame after a slider is released.
fn grade_thumb(src: &ThumbSrc, params: &GradeParams, gain: Ev) -> (ColorImage, [u32; 2]) {
    let mut rgb = src.rgb.clone();
    aevum_core::render_sketch(&mut rgb, src.w, src.h, params, gain);
    let pixels: Vec<egui::Color32> = rgb
        .par_chunks_exact(3)
        .map(|c| {
            egui::Color32::from_rgb(
                (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            )
        })
        .collect();
    (
        ColorImage::new([src.w, src.h], pixels),
        [src.w as u32, src.h as u32],
    )
}
