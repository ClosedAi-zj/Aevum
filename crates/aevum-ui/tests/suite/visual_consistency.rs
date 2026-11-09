//! 视觉一致性静态守门：钉住这一轮「让已做过的东西被看见」的修复不会回退。
//!
//! Static visual-consistency guards: pin this round's "make existing features visible"
//! fixes so they cannot silently regress.

use std::path::Path;

/// 读源码并去掉整行注释：文档注释里举例说明某写法时，不该把判据自己绊倒。
/// Read a source file with whole-line comments removed, so an example in docs
/// does not trip the guard that scans for that very pattern.
fn clean(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let raw =
        std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("读不到 {rel}：{e}"));
    raw.lines()
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with("//") { "" } else { l }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 报点名：把命中行号列出来，红了要知道改哪里。
/// Name the offending lines so a red gate says where to look.
fn hits(src: &str, needle: &str) -> Vec<usize> {
    src.lines()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i + 1)
        .collect()
}

#[test]
fn slider_rows_never_consume_wheel_scroll() {
    // 用户实测：滚动调整面板时鼠标划过滑块就改值，参数被静默破坏。
    // 所有滑块汇到 common.rs 的 slider_row()，那里不许再读 smooth_scroll_delta。
    // The user found that scrolling a panel changes any slider the pointer crosses.
    // Every slider funnels through slider_row(); it must never read the wheel again.
    let common = clean("shell/common.rs");
    assert!(
        !common.contains("smooth_scroll_delta"),
        "common.rs 又出现了滚轮监听（行 {:?}）：滚轮只属于面板 ScrollArea",
        hits(&common, "smooth_scroll_delta")
    );

    // 双向钉住：画布滚轮缩放是有意功能，别为了过判据把它一起删了。
    // Two-sided: canvas wheel zoom is intentional, do not delete it to go green.
    let canvas = clean("shell/canvas.rs");
    assert!(
        canvas.contains("smooth_scroll_delta") || canvas.contains("raw_scroll_delta"),
        "canvas.rs 的滚轮缩放不见了，别误伤独立功能"
    );
}

#[test]
fn film_collapse_control_is_a_real_button() {
    // 2026-11-07 用户拍板改版：按钮**不再写字**——头行水平居中、22px 圆形图标钮、
    // hover 才亮、名字进 tooltip（「只能展示一个功能」）。判据改钉新口径：
    // 交互 ID 仍是 tl-film、点击仍翻 show_film、tooltip 文案仍在、
    // 不许回到「112px 带字大按钮」的旧形态。
    let tl = clean("shell/timeline.rs");
    assert!(
        tl.contains("make_persistent_id(\"tl-film\")"),
        "收起控件的 tl-film 交互 ID 不见了"
    );
    assert!(
        tl.contains("state.show_film = !state.show_film"),
        "收起控件不再翻转 show_film"
    );
    assert!(
        tl.contains("收起 / 展开底部预览栏"),
        "收起控件的 tooltip 文案不见了"
    );
    assert!(
        !tl.contains("btn_w = 112.0"),
        "收起控件退回带文字的宽按钮（旧形态已废，名字进 tooltip）"
    );
    // 真按钮的最低证据：悬停有自己的底色（elev_hover）。
    assert!(
        tl.contains("elev_hover()"),
        "收起控件悬停无底色，会重新变成看不见的 ghost"
    );
}

#[test]
fn transport_gates_playback_on_two_frames() {
    // 单帧序列点了播放：画面不动、按钮变 Pause、还切到 128px 缩略图。
    // A one-frame sequence must not be playable.
    //
    // 这条必须钉**阈值本身**：只认变量名 `can_play` 的话，把 `>= 2` 改成 `> 0`
    // 判据照样绿（变异 M3 实测过），等于没有牙。
    //
    // Pin the **threshold**, not the variable name: keying on `can_play` alone keeps the gate
    // green when `>= 2` is turned back into `> 0` (mutation M3 proved it), which is toothless.
    let tl = clean("shell/timeline.rs");
    assert!(
        tl.contains("frame_count() >= 2"),
        "timeline.rs 的播放门槛不是「至少两帧」（要找 `frame_count() >= 2`）"
    );
    assert!(
        !tl.contains("frame_count() > 0\n    && ")
            && !tl.contains("can_play = state.frame_count() > 0"),
        "timeline.rs 的 can_play 退回成 `> 0` 了，单帧又能点播放"
    );
    let session = clean("state/ops_session.rs");
    assert!(
        session.contains("frame_count() < 2"),
        "toggle_play() 没有把单帧挡在播放态之外（命令/空格键会绕过 UI）"
    );
}

#[test]
fn release_notes_are_rendered_not_verbatim() {
    // 检查更新窗口曾把 GitHub Release 的 Markdown 原文逐字塞进 label：
    // `## 下载`、`###`、反引号原样显示。现在必须由 UI 层轻量渲染。
    // The update window used to show release Markdown verbatim; it must be rendered.
    let win = clean("shell/windows.rs");
    assert!(
        !win.contains("RichText::new(&state.update_body)"),
        "检查更新窗口又把发布说明原文直接塞进 label"
    );
    assert!(
        win.contains("parse_release_notes"),
        "检查更新窗口没有调用发布说明解析器"
    );
    let md = clean("shell/markdown.rs");
    assert!(
        md.contains("Heading") && md.contains("Bullet") && md.contains("Body"),
        "发布说明解析器缺最小闭集（标题 / 列表 / 正文）"
    );
}

#[test]
fn no_bare_font_size_literals_in_shell() {
    // 设计文档 §5.3：字号必须走 theme.rs 的 SIZE_* token。
    // 裸字面量会让「字体不统一」在每个新面板里复发。
    // Design spec 5.3: every font size comes from a theme token.
    use std::path::PathBuf;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell");
    let mut files = Vec::new();
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        let mut es: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        es.sort();
        for p in es {
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    walk(&root, &mut files);
    assert!(files.len() > 10, "shell/ 没扫到源码，路径口径变了");

    let mut offenders = Vec::new();
    for f in &files {
        let rel = f
            .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let src = clean(&rel);
        for (i, line) in src.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") || t.starts_with("///") || t.starts_with("//!") {
                continue;
            }
            // `.size(11.0)` / `.size(10.0)` 这类裸字面量；排除注释掉的示例。
            // Bare `.size(11.0)`-style literals; commented examples are excluded.
            let Some(at) = t.find(".size(") else { continue };
            let rest = &t[at + 6..];
            let arg: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !arg.is_empty() && arg.parse::<f32>().is_ok() {
                offenders.push(format!("{}:{} .size({})", rel, i + 1, arg));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "组件层出现裸字号字面量，改用 theme.rs 的 SIZE_* token：\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn destructive_controls_are_not_dim_by_default() {
    // 同一轮排查出的同类问题：删除/关闭按钮默认 dim()，悬停才显形。
    // 这些是破坏性动作，用户看不见却点得到，代价最大。默认态至少 muted()。
    // Sibling issue: delete/close controls were dim() by default. These are
    // destructive yet invisible; default state must be at least muted().
    //
    // 颜色与 Glyph 常常分行写，所以用「命中行 ±3 行」的窗口判断，而不是要求同一行。
    // Colour and glyph often sit on different lines, so use a 3-line window.
    for rel in [
        "shell/tabs.rs",
        "shell/mask.rs",
        "shell/adjust/render.rs",
        "shell/windows.rs",
        "shell/common.rs",
    ] {
        let src = clean(rel);
        let lines: Vec<&str> = src.lines().collect();
        let mut offenders = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let is_destructive = line.contains("Glyph::Close")
                || line.contains("Glyph::Reset")
                || line.contains("\"×\"")
                || line.contains("\"✕\"");
            if !is_destructive {
                continue;
            }
            let lo = i.saturating_sub(3);
            let hi = (i + 4).min(lines.len());
            let window = lines[lo..hi].join("\n");
            if window.contains("else { dim() }") {
                offenders.push(format!("{}:{}", rel, i + 1));
            }
        }
        assert!(
            offenders.is_empty(),
            "{rel} 的删除/关闭/复位控件默认态仍是 dim()，会重新变成看不见的 ghost：{:?}",
            offenders
        );
    }
}

#[test]
fn presets_window_defaults_to_closed() {
    // 「预设与快照」浮窗悬浮在画布上，默认 true 意味着每次启动都压住主图
    // （2026-11-06 用户模块截图里它盖着画布左上）。默认态必须关，入口保留。
    // The Presets & Snapshots window floats over the canvas; defaulting to true meant
    // every launch started with it covering the image (user module screenshots of
    // 2026-11-06). The default must be closed; the entries stay.
    let ops = clean("state/ops_session.rs");
    assert!(
        ops.contains("show_left: false"),
        "ops_session.rs 的 show_left 默认值必须是 false（浮窗不该开屏压住画布）"
    );
    // 双向钉住：别为了过判据把入口一起删了——工具轨的取反开关必须还在。
    // Two-sided: do not delete the entry to go green -- the rail toggle must remain.
    let toolbar = clean("shell/toolbar.rs");
    assert!(
        toolbar.contains("state.show_left = !state.show_left"),
        "工具轨的「预设与快照」开关不见了，别误伤入口"
    );
}

#[test]
fn canvas_top_bar_frame_size_is_labelled() {
    // 裸数字曾引发「× 是关闭叉」的误读（评审 E3 / 用户反馈 #7）：顶栏中段的
    // W × H 必须经 frame_size_text() 加「画幅」标签，不许退回裸 format!。
    // The bare numbers invited the "× is a close button" misreading (review E3 /
    // feedback #7): the top-bar W × H must go through frame_size_text() and carry the
    // Frame-size label; a bare format! is a regression.
    let canvas = clean("shell/canvas.rs");
    assert!(
        canvas.contains("fn frame_size_text"),
        "canvas.rs 的 frame_size_text() 不见了，顶栏画幅又变回裸数字"
    );
    // 2026-11-08 顶部压层：画幅读数从画布顶栏搬进选项行右段，所以调用点在
    // toolbar.rs。要求不变——尺寸仍是带「画幅」标签的成句，不是裸数字。
    //
    // 2026-11-08 chrome fold: the frame-size readout moved from the canvas's own top row
    // into the options bar, so the call site now lives in toolbar.rs. The requirement is
    // unchanged -- a labelled sentence, never bare numbers.
    let toolbar = clean("shell/toolbar.rs");
    assert!(
        toolbar.contains("frame_size_text(state.resolution, bits)"),
        "选项行右段没有走 frame_size_text()（行 {:?}）",
        hits(&toolbar, "× {h}")
    );
    // 状态栏的常驻「画幅」读数已随整条底部读数撤掉（用户 2026-11-08「底部其实
    // 没有必要显示」）：画幅只留画布顶栏这一处，判据到此为止，不再反向钉状态栏。
    //
    // The status bar's permanent Frame-size readout went with the rest of the bottom
    // chrome (user, 2026-11-08: "the bottom does not need to show anything"). The label
    // lives on the canvas top bar only; the check stops there and no longer pins the
    // status bar.
}

#[test]
fn thumbnails_are_developed_like_the_canvas() {
    // 用户 2026-11-07：「预览图应该跟主图一致」。缩略图必须走主画布同一份显影参数，
    // 而不是机身内嵌的未显影原片；胶片条上原来那枚「原片」角标随之取消。
    //
    // User, 2026-11-07: "the previews should match the main image". Thumbnails must
    // go through the canvas's develop parameters instead of the undeveloped embedded
    // original, and the filmstrip's "ORIG" chip goes with it.
    let cache = clean("state/ops_cache.rs");
    assert!(
        cache.contains("render_sketch"),
        "缩略图没有走显影链（render_sketch），会和主画布不一致"
    );
    assert!(
        cache.contains("preview_params()"),
        "缩略图没用主画布那份参数（preview_params），旁路/取景态会和画布分叉"
    );
    assert!(
        cache.contains("thumb_regrade"),
        "缩略图没有失效重调色的通路，改参数后胶片条会停在旧画面"
    );
    // 同一条报障的后半：胶片条原来抓的是机身内嵌 JPEG（自带相机色调曲线），比画布
    // 解出的 RAW 亮近一档（2026-11-07 实测 78.9 对 62.0）。显影参数一致还不够，源也
    // 得同源；`decode_thumb_source` 是那条与画布同口径的解码口。
    //
    // The other half of the same report: the filmstrip used the camera-embedded JPEG,
    // which carries the camera's tone curve and measured nearly a stop brighter than the
    // canvas RAW (78.9 vs 62.0, 2026-11-07). Matching parameters is not enough -- the
    // source has to match too; `decode_thumb_source` is the canvas-convention decoder.
    assert!(
        cache.contains("decode_thumb_source"),
        "胶片条又去读机身内嵌 JPEG 了（metrics::thumbnail），会和画布差近一档曝光"
    );
    assert!(
        !cache.contains("thumbnail(path, THUMB_SIDE)"),
        "胶片条仍在用 metrics::thumbnail 解 128px：那是相机按 As Shot 出的 JPEG"
    );
    let tl = clean("shell/timeline.rs");
    assert!(
        !tl.contains("t!(\"原片\")"),
        "缩略图已经显影，胶片条不应再贴「原片」角标"
    );
}

#[test]
fn floating_windows_read_as_floating() {
    // 「很假很廉价」的主因之一：浮窗描边 alpha 20 在深底上隐形、没有自己的
    // 阴影、标题带与内容只差两度——浮窗贴在内容上而不是浮在内容上。
    // 三处必须同时成立：border_strong 描边、自有 window_shadow、header 标题带。
    // One cause of the "cheap" look: invisible window stroke (alpha 20), no
    // window-specific shadow, title band two steps from the body -- windows stuck
    // on instead of floating over. All three must hold together.
    let theme = clean("theme.rs");
    assert!(
        theme.contains("v.window_stroke = Stroke::new(1.0, border_strong())"),
        "浮窗描边退回 border()（深底上隐形）"
    );
    assert!(
        theme.contains("v.window_shadow"),
        "theme.rs 的 window_shadow 不见了（egui 默认深色阴影在深底上不可见）"
    );
    let windows = clean("shell/windows.rs");
    assert!(
        windows.contains("fn title_frame()") && {
            let i = windows.find("fn title_frame()").unwrap();
            let block = &windows[i..(i + 700).min(windows.len())];
            block.contains(".fill(header())")
        },
        "title_frame 不再用 header()，标题带与窗口主体糊回一片"
    );
}

#[test]
fn hsl_band_rails_carry_their_band_colour() {
    // D1（LightCraft Track::Hue/Sat/Lum 语义）：HSL 轨道必须走频段渐变——
    // 色相=频段色→补色、饱和=灰→频段色、明亮=黑→频段色。三个种类不许退回
    // 同一个无含义的灰轨，红(0°)与蓝(240°)的频段色必须分得开。
    // 2026-11-07 起混色器走紧凑行 band_row（参考图实测 31px 行距），HSL 渐变
    // 由它统一绘制——门禁盯「频段行确实把停靠点交给 band_row」。
    // Since 2026-11-07 the mixer uses the compact band_row (31 px pitch measured on
    // the reference) and it paints the HSL gradients, so the gate pins that the band
    // rows hand it per-band stop tables.
    let common = clean("shell/common.rs");
    for needle in [
        "fn hsl_band_stops",
        "HslKind::Hue",
        "HslKind::Sat",
        "HslKind::Lum",
        "pub(super) fn band_row",
        "controls::sample_stops(table, t0)",
    ] {
        assert!(common.contains(needle), "common.rs 缺 {needle}");
    }
    let dev = clean("shell/adjust/develop.rs");
    let n = dev.matches("Some(hsl_band_stops(band, kind))").count();
    assert!(
        n >= 2,
        "HSL 频段行没有把渐变停靠点交给 band_row（现 {n} 处），频段渐变退回灰轨"
    );
    assert!(
        dev.contains("hsl_band_stops(band, kind)"),
        "HSL 行没有按「频段 + 属性」取渐变停靠点"
    );
}

#[test]
fn group_headers_match_the_acr_panel_column() {
    // 用户 2026-11-10 钦定的 v12 设计稿：组头是一行「折叠箭头 + 常规/粗体标题 +
    // 眼睛」，下面是卡片边界本身，不再压一道通栏 hairline。判据保留箭头和标题，
    // 只把 hairline 的要求翻成「不许有」。
    //
    // The user's pinned v12 design (2026-11-10): the header is a row of chevron +
    // title + eye, closed by the card edge itself rather than a full-width hairline.
    // The gate keeps the chevron/title checks and flips the hairline requirement.
    let common = clean("shell/common.rs");
    let i = common
        .find("pub(super) fn group_header")
        .expect("group_header 不见了");
    let body = &common[i..common[i..]
        .find("\npub(super) fn ")
        .map(|j| i + j)
        .unwrap_or(common.len())];
    // v12 设计稿改为「卡中卡」语法：`.grp` 自己就是边界，`.grp-h` 下面不再压线。
    // 旧 ACR 参考图（卡外组头 + 通栏 hairline）已不是当前目标，判据跟着翻面。
    //
    // v12 uses the "card inside card" grammar: `.grp` is the boundary and `.grp-h`
    // carries no rule beneath it. The older ACR reference (out-of-card headers with a
    // full-width hairline) is no longer the target, so the gate flips.
    assert_eq!(
        body.matches("theme::rule(").count(),
        0,
        "组头下不该再压 hairline：v12 设计稿用卡片边界分组"
    );
    assert!(
        !body.contains("Glyph::Sliders"),
        "组头又加回了滑杆图标（参考图里箭头之后直接是标题）"
    );
    // 组头高度按 v12 设计稿 `.grp-h{height:24px}`：搬进卡片后 24px 才对得上
    // 卡内距 12px 的呼吸。旧口径的 28/32 密度开关属于「卡外组头」时代。
    //
    // The header height follows v12's `.grp-h{height:24px}`: inside the card, 24 px is
    // what matches the 12 px card padding. The old 28/32 density switch belonged to the
    // out-of-card header era.
    assert!(
        body.contains("let gh = 24.0;"),
        "组头行高应为设计稿 `.grp-h` 的 24px"
    );
}

#[test]
fn options_bar_sits_on_the_dock_layer() {
    // B0 裁决（2026-11-06）：选项栏底色 = dock，与它服务的右侧参数面板同层；
    // 菜单行独占 header 骨架层，chrome 留给工具轨。旧版是「header → chrome →
    // dock」三档交错，选项栏比它服务的面板还亮半档，视觉重心漂。
    //
    // B0 ruling (2026-11-06): the options bar sits on `dock`, the same layer as the
    // parameter panel it serves; the menu row owns the `header` skeleton layer and
    // `chrome` is left to the tool rail. The old stack was header -> chrome -> dock,
    // with the options bar half a step brighter than the panel it serves.
    let shell = clean("shell/mod.rs");
    let i = shell
        .find("\"options-bar\"")
        .expect("options-bar 面板不见了");
    let body = &shell[i..(i + 700).min(shell.len())];
    assert!(
        body.contains("fill(dock())"),
        "选项栏底色不是 dock（B0 裁决：应与它服务的面板同层）"
    );
    assert!(
        !body.contains("fill(chrome())"),
        "选项栏又用回 chrome（三档交错回归）"
    );
}
