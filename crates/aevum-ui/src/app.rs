//! eframe 入口：把启动参数、后台任务和界面外壳接起来。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aevum_core::queue::{QueueItem, QueueStatus};
use eframe::egui;
use egui::{Context, ViewportCommand};

use crate::demo;
use crate::shell;
use crate::state::{AppState, DockTab, Module};
use crate::theme;

pub enum Startup {
    Empty,
    Open(PathBuf),
    Demo,
    /// 打开一个序列（缺省用合成演示），等画面就绪后截图并退出。
    Shot {
        out: PathBuf,
        open: Option<PathBuf>,
        module: Option<Module>,
        /// 截图前套一套演示编辑（目前是一张径向蒙版）。
        edit: bool,
        /// 截图前用演示数据填满渲染队列（不落盘）。
        queue: bool,
        /// 截图前把右侧面板滚到底，用来拍长面板的下半截。
        end: bool,
        /// 截图前把某个分组滚到面板顶部（`group_header` 的 key，如 masks）。
        group: Option<String>,
        /// 截图前打开「分通道（RGB 色闪）」去闪，用来拍彩色闪烁被压住的那张。
        channels: bool,
        /// 截图前切到某个右侧面板；拍「历史」时先造几步真实历史。
        dock: Option<String>,
        /// 截图前用 `crop.set` 拉一个内收的取景框，拍裁剪态的画布。
        crop: bool,
        /// 截图前用 `crop.set` 拉直一个角度，拍削过边的取景态画布。
        rotate: bool,
        /// 截图前用 `render.set` 把成片输出改成 JPEG + 质量 + 输出锐化。
        output: bool,
        /// 截图前用 `render.set` 盖一段右下文字水印，拍水印组那排控件。
        watermark: bool,
        /// 截图前用 `retime.set` 套一条缓入缓出曲线 + 快门，拍「变速 · 运动模糊」组。
        retime: bool,
        /// 截图前用 `motion.set` 摆两个框并开「拖取景框」，拍运镜的面板与画布。
        motion: bool,
        /// 截图前跑一遍序列体检，拍「序列体检」组（等测光落定后才起，见 `maybe_shot`）。
        qa: bool,
        /// 截图前用 `mask.add` + `mask.update` 涂一笔画笔蒙版，拍画笔组。
        brush: bool,
        /// 截图前造两个演示目录（带评分）用 library.add 加进图库，拍图库面板。
        library: bool,
        /// 配合 `library`：展开演示集合「精选」，拍集合视图。
        collection: bool,
        /// 配合 `library`：给演示目录写上说明与版权并展开信息面板。
        libmeta: bool,
        /// 配合 `library`：给演示目录造两叠、展开其中一叠，拍叠头折叠与展开两种样子。
        libstack: bool,
        /// 截图前把这个插件作用到画布上，拍「插件真的改了图」的那一张。
        plugin: Option<String>,
        /// 截图前打开插件市场窗口，拍标题栏与两栏清单。
        market: bool,
    },
}

enum Boot {
    Open(PathBuf),
    Demo,
}

struct Shot {
    out: PathBuf,
    module: Option<Module>,
    edit: bool,
    queue: bool,
    end: bool,
    group: Option<String>,
    channels: bool,
    dock: Option<String>,
    crop: bool,
    rotate: bool,
    output: bool,
    watermark: bool,
    retime: bool,
    motion: bool,
    qa: bool,
    brush: bool,
    library: bool,
    collection: bool,
    libmeta: bool,
    libstack: bool,
    /// 截图前要应用的插件 id。
    plugin: Option<String>,
    /// 截图前打开插件市场窗口。
    market: bool,
    /// 打开市场的时刻：等窗口画稳再截。
    market_at: Option<Instant>,
    /// 插件只应用一次：应用完就没必要再跑一遍。
    plugin_done: bool,
    /// 插件是后台任务：起任务的时刻记在这里，30 秒没跑完就取消兜底。
    plugin_at: Option<Instant>,
    /// 取景框要等分辨率回填以后才画得出来，画过一次就不再画。
    crop_done: bool,
    /// 体检要等测光落定才起，起过一次就不再起。
    qa_started: bool,
    started: Instant,
    requested: bool,
}

/// 截图用的队列示例：一行完成、一行待渲、一行失败，覆盖三种状态配色。
fn demo_queue(root: &std::path::Path) -> Vec<QueueItem> {
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "序列".to_owned());
    vec![
        QueueItem {
            root: root.display().to_string(),
            out: root
                .join(format!("{name}_成片_4K.mp4"))
                .display()
                .to_string(),
            width: 3840,
            height: 2160,
            fps: 24.0,
            preset: "slow".to_owned(),
            status: QueueStatus::Done,
            note: "渲染完成（4K · H.265）".to_owned(),
            ..QueueItem::default()
        },
        QueueItem {
            root: root.display().to_string(),
            out: root
                .join(format!("{name}_预览_1080p.mp4"))
                .display()
                .to_string(),
            width: 1920,
            height: 1080,
            fps: 24.0,
            codec: "h264".to_owned(),
            crf: 20,
            keep_frames: true,
            ..QueueItem::default()
        },
        QueueItem {
            root: root
                .with_file_name(format!("{name}_夜"))
                .display()
                .to_string(),
            out: root
                .with_file_name(format!("{name}_夜_成片.mp4"))
                .display()
                .to_string(),
            status: QueueStatus::Failed,
            note: "序列目录不存在：加入后被移动或删除".to_owned(),
            ..QueueItem::default()
        },
    ]
}

/// 给历史面板的截图造几步：命令写入、一次拖动（连续帧并成一步）、外部效果。
fn seed_history(state: &mut AppState, ctx: &Context) {
    let _ = state.run_ui_command(ctx, "grade.set", &serde_json::json!({"exposure": 0.35}));
    let _ = state.run_ui_command(
        ctx,
        "grade.preset",
        &serde_json::json!({"name": "冷色电影"}),
    );
    for v in [12.0, 20.0, 26.0] {
        state.history.begin_frame();
        state.params.contrast = v;
        crate::history::note("对比度", format!("对比度 {v:+.0}"));
        state.history_commit();
    }
    let _ = state.run_ui_command(ctx, "mask.add", &serde_json::json!({"kind": "radial"}));
    let _ = state.run_ui_command(ctx, "queue.clear", &serde_json::json!({}));
}

pub struct AevumApp {
    state: AppState,
    boot: Option<Boot>,
    shot: Option<Shot>,
}

impl AevumApp {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> Self {
        // 偏好先于界面：先在全局装好参数，再按它装字体与 Visuals，
        // 否则首帧会闪一下默认深色再跳成用户选的那套。
        let saved = crate::settings::load();
        // 语言必须早于任何一次绘制：晚一帧设，首帧就会用错语言画一遍再跳回来。
        // 这条也在 `theme::apply` 之前——字体回退链与语言无关，但文案宽度会影响
        // 第一次布局，先定语言再量字。
        crate::i18n::set(saved.lang.resolve());
        theme::set_appearance(saved.appearance);
        let mut state = AppState::new();
        state.adopt_settings(saved);
        theme::apply(
            &cc.egui_ctx,
            theme::FONT_ZOOMS[state.font_level.min(theme::FONT_ZOOMS.len() - 1)],
        );
        state.load_queue();
        let (boot, shot) = match startup {
            Startup::Empty => (None, None),
            Startup::Open(path) => (Some(Boot::Open(path)), None),
            Startup::Demo => (Some(Boot::Demo), None),
            Startup::Shot {
                out,
                open,
                module,
                edit,
                queue,
                end,
                group,
                channels,
                dock,
                crop,
                rotate,
                output,
                watermark,
                retime,
                motion,
                qa,
                brush,
                library,
                collection,
                libmeta,
                libstack,
                plugin,
                market,
            } => {
                // 一次性截图不参与回环控制：避免短暂占用序列的 control.json，
                // 和已经在跑的 GUI 抢同一份文件。
                state.control_enabled = false;
                let boot = match open {
                    Some(path) => Boot::Open(path),
                    None => match demo::ensure_demo() {
                        Ok(dir) => Boot::Open(dir),
                        Err(e) => {
                            state.error = Some(e);
                            Boot::Demo
                        }
                    },
                };
                (
                    Some(boot),
                    Some(Shot {
                        out,
                        module,
                        edit,
                        queue,
                        end,
                        group,
                        channels,
                        dock,
                        crop,
                        rotate,
                        output,
                        watermark,
                        retime,
                        motion,
                        qa,
                        brush,
                        library,
                        collection,
                        libmeta,
                        libstack,
                        plugin,
                        market,
                        market_at: None,
                        plugin_done: false,
                        plugin_at: None,
                        crop_done: false,
                        qa_started: false,
                        started: Instant::now(),
                        requested: false,
                    }),
                )
            }
        };
        // 截图要拍的是"用户看到的样子"，而用户面前的窗口一定是聚焦的。
        // 无头启动的窗口常常拿不到焦点，标题栏会一路画成未聚焦态（交通灯褪灰、
        // 窗口按钮压暗），那不是要验收的那个画面。这里主动要一次焦点。
        if shot.is_some() {
            crate::platform::preview_focus(true);
        }
        Self { state, boot, shot }
    }

    fn boot(&mut self, ctx: &Context) {
        let Some(boot) = self.boot.take() else {
            return;
        };
        match boot {
            Boot::Open(path) => self.state.open(ctx, path),
            Boot::Demo => match demo::ensure_demo() {
                Ok(dir) => self.state.open(ctx, dir),
                Err(e) => self.state.error = Some(e),
            },
        }
        if let Some(module) = self.shot.as_ref().and_then(|s| s.module) {
            self.state.module = module;
            if module == Module::Audio
                && let Ok(wav) = demo::ensure_demo_audio()
            {
                self.state.set_render_audio(Some(wav));
                self.state.render_audio_opts.gain = 85.0;
                self.state.render_audio_opts.fade_in = 0.4;
                self.state.render_audio_opts.fade_out = 0.8;
                self.state.render_audio_opts.looped = true;
            }
        }
        if self.shot.as_ref().is_some_and(|s| s.edit) {
            self.state.apply_demo_edit();
        }
        if self.shot.as_ref().is_some_and(|s| s.end) {
            self.state.dock_scroll_end = true;
        }
        if let Some(group) = self.shot.as_ref().and_then(|s| s.group.clone()) {
            self.state.dock_focus = Some(group);
        }
        if self.shot.as_ref().is_some_and(|s| s.queue)
            && let Some(seq) = &self.state.seq
        {
            self.state.queue = demo_queue(&seq.root);
            self.state.dock_scroll_end = true;
        }
        // 走命令注册表而不是直接改字段：截图要反映「用户勾了一下」的同一条路径，
        // 顺带把去闪重算跑出来，面板上的色闪抖动数字才是真的。
        if self.shot.as_ref().is_some_and(|s| s.channels) {
            let _ = self.state.run_ui_command(
                ctx,
                "deflicker.set",
                &serde_json::json!({"channels": true}),
            );
        }
        // 输出面板那张也要走真实命令：静帧 + JPEG + 质量 + 输出锐化一次摆全，
        // 截图里看到的芯片状态和点出来的一样。
        if self.shot.as_ref().is_some_and(|s| s.output) {
            let _ = self.state.run_ui_command(
                ctx,
                "render.set",
                &serde_json::json!({
                    "mode": "stills",
                    "format": "jpeg",
                    "quality": 80,
                    "keep_exif": true,
                    "xmp_sidecar": true,
                    "sharpen_for": "screen",
                    "color_space": "p3",
                }),
            );
        }
        // 水印组那张同样走真实命令：开关、正文、九宫格、不透明度都是活的面板状态。
        if self.shot.as_ref().is_some_and(|s| s.watermark) {
            let _ = self.state.run_ui_command(
                ctx,
                "render.set",
                &serde_json::json!({
                    "watermark": {
                        "enabled": true,
                        "text": format!("© {}", aevum_core::brand::TITLE),
                        "spot": "bottom-right",
                        "opacity": 70,
                    },
                }),
            );
        }
        // 变速那张也走真实命令：一条缓入缓出曲线 + 快门，面板上的帧数读数与
        // 「曲线摘要」芯片状态都是命令通道落盘的结果。
        if self.shot.as_ref().is_some_and(|s| s.retime) {
            let _ = self.state.run_ui_command(
                ctx,
                "retime.set",
                &serde_json::json!({ "ease": { "peak": 4.0 }, "shutter": 0.6 }),
            );
        }
        // 运镜那张也走真实命令：起点铺满、终点往右下推一档再走一段（演示序列本身就是
        // 16:9，归一化的方框正好是 16:9 的像素框），面板上两行读数、画布上那两个框和
        // 那根方向箭头都是命令通道落盘的结果；「拖取景框」是视图态，直接开——覆盖层只在开着时画。
        if self.shot.as_ref().is_some_and(|s| s.motion) {
            let _ = self.state.run_ui_command(
                ctx,
                "motion.set",
                &serde_json::json!({
                    "from": [0.0, 0.0, 1.0, 1.0],
                    "to": { "x": 0.28, "y": 0.3, "w": 0.6, "h": 0.6 },
                    "ease": "smooth",
                }),
            );
            self.state.set_motion_picking(true);
        }
        // 画笔蒙版那张也走真实命令：mask.add 画笔 + mask.update 一组笔画（涂一块天空），
        // 面板与画布上看到的都是命令通道落盘的状态。
        if self.shot.as_ref().is_some_and(|s| s.brush) {
            let _ =
                self.state
                    .run_ui_command(ctx, "mask.add", &serde_json::json!({"kind": "brush"}));
            let _ = self.state.run_ui_command(
                ctx,
                "mask.update",
                &serde_json::json!({
                    "index": 1,
                    "params": {"exposure": 0.6},
                    "geometry": {"strokes": [
                        {"points": [[0.15, 0.22], [0.3, 0.18], [0.45, 0.2], [0.6, 0.15], [0.75, 0.18], [0.88, 0.22]],
                         "radius": 0.09, "hardness": 0.5, "flow": 0.9}
                    ]},
                    "range": {"hue_on": true, "hue": 210, "hue_width": 60, "hue_feather": 20}
                }),
            );
        }
        // 图库那张：造两个演示目录（一个带评分）用 library.add 加进图库并展开编目。
        // 清单指到临时处——截图目录不该记进用户真的图库。
        if self.shot.as_ref().is_some_and(|s| s.library) {
            let base = std::env::temp_dir().join("aevum-library-shot");
            let _ = std::fs::remove_dir_all(&base);
            unsafe { std::env::set_var("AEVUM_LIBRARY", base.join("galleries.json").as_os_str()) };
            // 截图要可复现：启动时从用户真图库读进来的根与集合一律不带进演示图，
            // 否则演示目录的序号会随用户图库变，下面写死的 root 0 就对不上了。
            self.state.lib_roots.clear();
            self.state.lib_cols.clear();
            for (name, count) in [("日出前景", 4usize), ("星轨收尾", 3)] {
                let dir = base.join(name);
                std::fs::create_dir_all(&dir).expect("建演示目录");
                for i in 0..count {
                    let mut img = image::RgbImage::new(96, 64);
                    for (x, y, p) in img.enumerate_pixels_mut() {
                        *p = image::Rgb([
                            (x * 2 + i as u32 * 30) as u8,
                            (y * 3 + i as u32 * 15) as u8,
                            220u32.saturating_sub(x) as u8,
                        ]);
                    }
                    img.save(dir.join(format!("f{i:03}.png")))
                        .expect("写演示帧");
                }
                if name == "日出前景" {
                    let mut proj = aevum_core::project::ProjectFile::default();
                    proj.ratings.insert(0, 5);
                    proj.ratings.insert(1, 4);
                    proj.flags.insert(0, 1);
                    proj.labels.insert(2, 2);
                    proj.keywords
                        .insert(0, vec!["日出".to_owned(), "前景".to_owned()]);
                    proj.keywords.insert(2, vec!["云海".to_owned()]);
                    if self.shot.as_ref().is_some_and(|s| s.libmeta) {
                        proj.metadata = aevum_core::project::Metadata {
                            title: "日出前景".to_owned(),
                            description: "牛背山东平台，间隔 2 秒".to_owned(),
                            creator: "张三".to_owned(),
                            copyright: "© 2026 张三".to_owned(),
                        };
                    }
                    if self.shot.as_ref().is_some_and(|s| s.libstack) {
                        // 两叠各两张：截图里既有折叠的叠头，也有展开的一叠。
                        proj.stacks = vec![vec![0, 1], vec![2, 3]];
                    }
                    std::fs::create_dir_all(aevum_core::project::guard_dir(&dir)).unwrap();
                    let _ =
                        aevum_core::project::save(&aevum_core::project::project_path(&dir), &proj);
                }
                self.state.lib_add(&dir);
            }
            // 集合：把评分最高的两张收进「精选」，截图里带上集合区与选中行。
            let col = self.state.lib_collection_create("精选");
            for name in ["f000.png", "f001.png"] {
                let _ = self
                    .state
                    .lib_collection_put(col, &base.join("日出前景").join(name));
            }
            if self.shot.as_ref().is_some_and(|s| s.collection) {
                self.state.lib_scan(0);
                self.state.lib_col = Some(0);
                self.state.lib_sel = None;
            } else {
                self.state.lib_scan(0);
                // 勾两行演示批量挑片：筛选条下会出现「已选 N 张」。
                self.state.lib_click(1, false, false);
                self.state.lib_click(2, true, false);
            }
            if self.shot.as_ref().is_some_and(|s| s.libmeta) {
                self.state.lib_meta_open = true;
                self.state.lib_load_meta(0);
            }
            if self.shot.as_ref().is_some_and(|s| s.libstack) {
                // 展开第二叠，选中它的叠头：折叠与展开两种角标同框。
                self.state.lib_stack_open.insert((0, 1));
                self.state.lib_click(2, false, false);
            }
        }
        if let Some(name) = self.shot.as_ref().and_then(|s| s.dock.clone()) {
            match DockTab::from_name(&name) {
                Some(tab) => {
                    if tab == DockTab::History {
                        seed_history(&mut self.state, ctx);
                    }
                    self.state.dock_tab = tab;
                    self.state.show_dock = true;
                }
                None => self.state.error = Some(format!("不认识的面板：{name}")),
            }
        }
    }

    fn maybe_shot(&mut self, ctx: &Context) {
        let Some(shot) = &mut self.shot else {
            return;
        };
        if shot.requested {
            return;
        }
        // 取景走 `crop.set` 那条真实路径：分辨率是解码回来才回填的，所以这里等它出现，
        // 套完框再多等一帧，让裁剪叠层真正上屏后才按下截图。
        if (shot.crop || shot.rotate) && !shot.crop_done {
            if let Some((w, h)) = self.state.resolution {
                shot.crop_done = true;
                let mut args = serde_json::Map::new();
                let inset = |v: u32| (f64::from(v) * 0.08).round() as u64;
                let (dx, dy) = (inset(w), inset(h));
                args.insert("x".into(), serde_json::json!(dx));
                args.insert("y".into(), serde_json::json!(dy));
                args.insert("width".into(), serde_json::json!(u64::from(w) - 2 * dx));
                args.insert("height".into(), serde_json::json!(u64::from(h) - 2 * dy));
                if shot.rotate {
                    args.insert("rotate".into(), serde_json::json!(6.0));
                }
                let _ =
                    self.state
                        .run_ui_command(ctx, "crop.set", &serde_json::Value::Object(args));
                shot.started = Instant::now();
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(Duration::from_millis(80));
            }
            return;
        }
        // 体检那张必须等测光落定：没有测光表，每一帧都会被判成「读不出来」，
        // 拍到的是一屏假红灯。起完任务把计时器归零，让清单先渲染完再按快门。
        if shot.qa && !shot.qa_started {
            if self.state.measuring || self.state.seq.is_none() {
                ctx.request_repaint_after(Duration::from_millis(80));
                return;
            }
            shot.qa_started = true;
            let _ = self.state.run_ui_command(
                ctx,
                "sequence.check",
                &serde_json::json!({ "sig": 1.2 }),
            );
            shot.started = Instant::now();
            ctx.request_repaint();
            return;
        }
        let elapsed = shot.started.elapsed();
        let ready = elapsed >= Duration::from_millis(900)
            && !self.state.measuring
            && !self.state.qa_busy
            && self.state.texture.is_some();
        if !ready && elapsed < Duration::from_secs(30) {
            ctx.request_repaint_after(Duration::from_millis(80));
            return;
        }
        // 插件那张要等画面稳定再跑：插件拿的是 CPU 像素，跑早了会因为演示编辑
        // 还没套完而作用在上一版底图上，结果立刻被下一次预览冲掉。
        // B06 之后插件是后台任务：这里起一次，之后每帧等 `plugin_poll`
        // （挂在 state.poll 里）把它收尾，收完再截图；30 秒兜底取消。
        if let Some(id) = shot.plugin.clone() {
            if !shot.plugin_done {
                shot.plugin_done = true;
                shot.plugin_at = Some(Instant::now());
                match self.state.plugin_start(ctx, &id) {
                    Ok(name) => println!("插件已应用：{name}"),
                    Err(e) => eprintln!("插件没跑起来：{e}"),
                }
                ctx.request_repaint();
                return;
            }
            if self.state.plugin_running() {
                if shot
                    .plugin_at
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(30))
                {
                    eprintln!("插件 30 秒没跑完，取消并放弃");
                    self.state.plugin_cancel();
                }
                ctx.request_repaint_after(Duration::from_millis(60));
                return;
            }
        }
        // 插件市场那张：开窗等一拍，窗口动画落定再截。
        if shot.market {
            self.state.market_open = true;
            if shot
                .market_at
                .is_none_or(|t| t.elapsed() < Duration::from_millis(400))
            {
                if shot.market_at.is_none() {
                    shot.market_at = Some(Instant::now());
                }
                ctx.request_repaint_after(Duration::from_millis(80));
                return;
            }
        }
        shot.requested = true;
        let out = shot.out.clone();
        let ctx2 = ctx.clone();
        ctx.request_screenshot(move |image| {
            match crate::control::save_png(&out, &image) {
                Ok(()) => println!("截图已保存：{}", out.display()),
                Err(e) => eprintln!("截图保存失败：{e}"),
            }
            ctx2.send_viewport_cmd(ViewportCommand::Close);
        });
        ctx.request_repaint();
    }
}

impl eframe::App for AevumApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.boot(&ctx);
        self.state.poll(&ctx);
        self.state.tick(&ctx);
        shell::ui(&mut self.state, ui);
        self.maybe_shot(&ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = theme::canvas();
        [
            c.r() as f32 / 255.0,
            c.g() as f32 / 255.0,
            c.b() as f32 / 255.0,
            1.0,
        ]
    }
}
