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
    assert!(
        canvas.contains("frame_size_text(state.resolution, bits)"),
        "顶栏中段没有走 frame_size_text()（行 {:?}）",
        hits(&canvas, "× {h}")
    );
    // 双向钉住：底部状态栏自己的「画幅」标签别被顺手删掉。
    // Two-sided: the bottom status bar keeps its own Frame-size label.
    let toolbar = clean("shell/toolbar.rs");
    assert!(
        toolbar.contains("t!(\"画幅\")"),
        "底部状态栏的「画幅」标签不见了"
    );
}

#[test]
fn thumbnails_carry_the_original_chip() {
    // 评审 E2 的可见标记：行内说明字已在（d701cca），但验收口径是「每个缩略图
    // 有可见标记」——胶片条每格右上角的「原片」角标不许回退，英文表要有键。
    // The visible marker from review E2: the inline caption already exists (d701cca),
    // but the acceptance wording is "every thumbnail carries a visible marker" -- the
    // per-cell chip must not regress, and the English table must have the key.
    let tl = clean("shell/timeline.rs");
    assert!(
        tl.contains("t!(\"原片\")"),
        "胶片条缩略图的「原片」角标不见了（评审 E2 回退）"
    );
    assert!(
        tl.contains("rect_filled(chip_rect"),
        "「原片」角标没有自己的底色，会重新糊进缩略图里"
    );
    let en = clean("i18n_en.rs");
    assert!(
        en.contains("(\"原片\", \"ORIG\")"),
        "i18n_en.rs 缺「原片」的英文键（i18n_literals 会红，这里提前指路）"
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
    // D1（LightCraft Track::Hue/Sat/Lum 语义）：HSL 三条轨道必须走频段渐变——
    // 色相=频段色→补色、饱和=灰→频段色、明亮=黑→频段色。三个种类不许退回
    // 同一个无含义的灰轨，红(0°)与蓝(240°)的频段色必须分得开。
    // D1 (LightCraft Track semantics): the three HSL rails must take band
    // gradients; the three kinds must not collapse into one grey rail, and the
    // red (0 deg) and blue (240 deg) band colours must differ.
    let common = clean("shell/common.rs");
    for needle in [
        "fn hsl_band_stops",
        "HslKind::Hue",
        "HslKind::Sat",
        "HslKind::Lum",
    ] {
        assert!(common.contains(needle), "common.rs 缺 {needle}");
    }
    let dev = clean("shell/adjust/develop.rs");
    let n = dev.matches("slider_grad").count();
    assert!(
        n >= 3,
        "HSL 三条滑块没有走 slider_grad（现 {n} 处），频段渐变退回灰轨"
    );
}

#[test]
fn group_headers_end_in_a_single_hairline() {
    // 用户点名「选项间的分割」不喜欢：旧版组头是「实线+2px 上一道淡线」的
    // 双线签名，十四组排下来读作表格。现在一条 hairline 收尾——组头函数体
    // 内 theme::rule 恰好一次，多一次即红。
    let common = clean("shell/common.rs");
    let i = common
        .find("pub(super) fn group_header")
        .expect("group_header 不见了");
    let body = &common[i..common[i..]
        .find("\npub(super) fn ")
        .map(|j| i + j)
        .unwrap_or(common.len())];
    let rules = body.matches("theme::rule(").count();
    assert_eq!(
        rules, 1,
        "group_header 里出现了 {rules} 道分隔线，双线口径已废"
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
