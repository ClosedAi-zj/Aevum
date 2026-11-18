//! P0-0 空态可信修复的运行期判据：空态没有假预览，有照片时控件一个不少。
//!
//! 五条判据分别钉住：黑照片/三分网格、照片专属控件、假数据/假直方图、
//! 有照片态不回归、导入入口可点。画帧取 shape（与 `i18n_coverage` 同口径），
//! 而不是只扫源码——「屏幕上有没有」才是用户看到的事实。
//!
//! Runtime criteria for the P0-0 empty-state fix: no fake preview when empty and
//! no missing controls when a photo exists. All five paint real frames and inspect
//! the resulting shapes, the same way `i18n_coverage` does.

use aevum_ui::i18n::{self, Lang};
use aevum_ui::state::{AppState, Module};
use aevum_ui::{shell, theme};

use egui::epaint::ClippedShape;
use egui::{Color32, Rect, Shape};

/// 与 `--selftest` / `i18n_coverage` 同一块画布：1600×1000。
/// Same 1600×1000 canvas as the selftest and the i18n coverage sweep.
fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        max_texture_side: Some(8192),
        ..Default::default()
    }
}

/// 建模一个 Context：token 应用后画帧，避免主题差异污染颜色判据。
/// Build a themed context so colour assertions see the production palette.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    theme::apply(&ctx, 1.0);
    ctx
}

/// 三帧预热后取稳态帧：首帧还没有 galley 宽度，写字位置会漂。
/// Paint a settled frame after three warm-ups (first frame lacks galley widths).
fn paint(ctx: &egui::Context, state: &mut AppState) -> Vec<ClippedShape> {
    for _ in 0..3 {
        let mut warm = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
        warm.textures_delta.clear();
    }
    let mut out = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
    let shapes = out.shapes;
    out.textures_delta.clear();
    shapes
}

/// 空态：AppState::new() 的照片位是 None，桥接后 `has_photo() == false`。
/// Empty state: the bridge sees `texture == None`, hence `has_photo() == false`.
fn empty_shapes(ctx: &egui::Context) -> Vec<ClippedShape> {
    let mut state = AppState::new();
    // 空态判据钉的是显影井：T3 起默认模块是 Sequence，会先画序列屏。
    // These criteria pin the develop well; from T3 the default module is Sequence.
    state.module = Module::Develop;
    paint(ctx, &mut state)
}

/// 有照片态：装合成纹理，让画布/胶片条/直方图都走真实绘制路径。
/// Photo state: install synthetic textures so every photo-only path is painted.
fn photo_shapes(ctx: &egui::Context) -> Vec<ClippedShape> {
    let mut state = AppState::demo();
    state.install_demo_textures(ctx);
    state.module = Module::Develop;
    paint(ctx, &mut state)
}

/// 递归收集文本 shape（位置用于区分顶栏读数与别处的百分比）。
/// Recursively collect text shapes; positions separate top-bar readouts.
fn texts(shapes: &[ClippedShape]) -> Vec<(String, Rect)> {
    let mut out = Vec::new();
    for cs in shapes {
        walk_text(&cs.shape, &mut out);
    }
    out
}

fn walk_text(shape: &Shape, out: &mut Vec<(String, Rect)>) {
    match shape {
        Shape::Text(t) => {
            let txt = t.galley.text().to_string();
            if !txt.trim().is_empty() {
                out.push((txt, t.galley.rect.translate(t.pos.to_vec2())));
            }
        }
        Shape::Vec(v) => {
            for s in v {
                walk_text(s, out);
            }
        }
        _ => {}
    }
}

/// 递归收集矩形填充色：用来抓 #313131 假照片与 ACCENT 真按钮。
/// Recursively collect rect fills: the fake #313131 frame and the accent button.
fn rect_fills(shapes: &[ClippedShape]) -> Vec<(Rect, Color32)> {
    let mut out = Vec::new();
    for cs in shapes {
        walk_rect(&cs.shape, &mut out);
    }
    out
}

fn walk_rect(shape: &Shape, out: &mut Vec<(Rect, Color32)>) {
    match shape {
        Shape::Rect(r) => out.push((r.rect, r.fill)),
        Shape::Vec(v) => {
            for s in v {
                walk_rect(s, out);
            }
        }
        _ => {}
    }
}

/// 递归收集线段颜色：三分构图线用 `from_white_alpha(22)`，只此一处。
/// Recursively collect line colours: the thirds grid alone uses white alpha 22.
fn line_colors(shapes: &[ClippedShape]) -> Vec<Color32> {
    let mut out = Vec::new();
    for cs in shapes {
        walk_line(&cs.shape, &mut out);
    }
    out
}

fn walk_line(shape: &Shape, out: &mut Vec<Color32>) {
    match shape {
        Shape::LineSegment { stroke, .. } => out.push(stroke.color),
        Shape::Vec(v) => {
            for s in v {
                walk_line(s, out);
            }
        }
        _ => {}
    }
}

/// 顶栏百分比读数的形状特征：`^\d+%$` 且 y < TOP_H。
/// Shape signature of the top-bar percent readout: digits plus % above TOP_H.
fn is_percent(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2
        && bytes[bytes.len() - 1] == b'%'
        && bytes[..bytes.len() - 1].iter().all(|b| b.is_ascii_digit())
}

/// 读源码并剥掉整行注释：判据被自己的注释绊倒过，这里照 `visual_consistency` 办。
/// Read source with whole-line comments stripped, following `visual_consistency`.
fn clean_v21() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell/v21_demo.rs");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到 v21_demo.rs：{e}"));
    raw.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 截出一个函数体（到下一个顶层 `fn` 为止），用来钉「早退在绘制之前」。
/// Slice one function body up to the next top-level `fn`, to pin early returns.
fn body<'a>(src: &'a str, name: &str) -> &'a str {
    let start = src
        .find(name)
        .unwrap_or_else(|| panic!("v21_demo.rs 找不到 {name}"));
    let rest = &src[start..];
    let end = rest
        .find("\nfn ")
        .map(|i| start + i)
        .unwrap_or_else(|| src.len());
    &src[start..end]
}

#[test]
fn empty_state_has_no_photo_like_rect() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    // 静态：假照片那一行与无条件的三分网格循环都不许回来。
    // Static: neither the fake photo fill nor the unconditional thirds loop returns.
    let src = clean_v21();
    assert!(
        !src.contains("p.rect_filled(frame, 3.0, Color32::from_rgb(0x31, 0x31, 0x31))"),
        "空态又画回 #313131 假照片块"
    );
    assert!(
        src.contains("draw_empty_well(ui, &p, rect, st)"),
        "空态引导块没有接进画布 else 分支"
    );

    let ctx = context();
    let empty = empty_shapes(&ctx);
    let fake = rect_fills(&empty)
        .iter()
        .filter(|(r, c)| {
            *c == Color32::from_rgb(0x31, 0x31, 0x31) && r.width() > 200.0 && r.height() > 120.0
        })
        .count();
    assert_eq!(fake, 0, "空态画布上出现形似照片的大矩形：{fake} 块");
    let grid = line_colors(&empty)
        .iter()
        .filter(|c| **c == Color32::from_white_alpha(22))
        .count();
    assert_eq!(grid, 0, "空态还画着三分构图辅助线：{grid} 条");

    // 双向：有照片时四条构图线必须照旧出现，别为了过空态判据误伤真功能。
    // Two-sided: a real photo must still get all four thirds lines.
    let photo = photo_shapes(&ctx);
    let photo_grid = line_colors(&photo)
        .iter()
        .filter(|c| **c == Color32::from_white_alpha(22))
        .count();
    assert!(
        photo_grid >= 4,
        "有照片时三分构图线不见了（只剩 {photo_grid} 条）"
    );
}

#[test]
fn empty_state_has_no_photo_only_chrome() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);
    let src = clean_v21();
    for name in ["fn draw_ai_cmdbar", "fn draw_canvas_tools"] {
        let f = body(&src, name);
        let drawn_at = f.find("let bar_h").unwrap_or(f.len());
        assert!(
            f[..drawn_at].contains("if !st.has_photo()"),
            "{name} 缺空态早退，仍会画只对照片有意义的控件"
        );
    }

    let ctx = context();
    let empty = texts(&empty_shapes(&ctx));
    let empty_zoom_chip = empty.iter().any(|(t, _)| t == i18n::t!("适应窗口"));
    let ai_text = i18n::t!("对整段做去闪 + 日转夜 ramp，保留车流鬼影");
    let empty_ai = empty.iter().any(|(t, _)| t == ai_text);
    let empty_tools = ["抓手", "整段", "单帧"]
        .iter()
        .any(|label| empty.iter().any(|(t, _)| t == i18n::tr(label)));
    let top_pct = empty
        .iter()
        .filter(|(t, r)| is_percent(t) && r.center().y < 44.0)
        .count();
    assert!(!empty_zoom_chip, "空态还有「适应窗口」缩放 chip");
    assert!(!empty_ai, "空态还有 AI 命令条");
    assert!(!empty_tools, "空态还有抓手 / 整段 / 单帧工具条");
    assert_eq!(top_pct, 0, "空态顶栏仍显示缩放百分比");

    let photo = texts(&photo_shapes(&ctx));
    assert!(
        photo.iter().any(|(t, _)| t == i18n::t!("适应窗口")),
        "有照片时缩放 chip 不见了"
    );
    assert!(
        photo.iter().any(|(t, _)| t == ai_text),
        "有照片时 AI 命令条不见了"
    );
    assert!(
        ["抓手", "整段", "单帧"]
            .iter()
            .all(|label| photo.iter().any(|(t, _)| t == i18n::tr(label))),
        "有照片时底部工具条控件不全"
    );
    assert!(
        photo
            .iter()
            .any(|(t, r)| is_percent(t) && r.center().y < 44.0),
        "有照片时顶栏缩放读数不见了"
    );
}

#[test]
fn empty_state_has_no_demo_fake_data() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);
    assert!(
        i18n::has_in(Lang::EN, "无照片时直方图无意义"),
        "新空槽提示缺英译，英文模式会露出中文"
    );
    let src = clean_v21();
    assert!(
        src.contains("无照片时直方图无意义"),
        "右坞空槽提示没有接进绘制"
    );
    let film = body(&src, "fn draw_film");
    let early = film
        .find("if !st.has_photo()")
        .unwrap_or_else(|| panic!("draw_film 没有空态分流"));
    let fake_count = film
        .find("48 张 · 已选 1")
        .unwrap_or_else(|| panic!("draw_film 的假计数写法变了"));
    assert!(
        early < fake_count,
        "draw_film 的空态早退落在假计数之后，挡不住 48/已选 1"
    );

    let ctx = context();
    let empty = texts(&empty_shapes(&ctx));
    let fake_label = empty
        .iter()
        .any(|(t, _)| t.contains("张 · 已选") || t.contains("已选 1"));
    assert!(!fake_label, "空态胶片条仍在写假帧数 / 已选 1");
    let fake_rgb = empty
        .iter()
        .filter(|(t, r)| {
            (t == "128" || t == "142" || t == "96") && r.center().x > 1200.0 && r.center().y < 260.0
        })
        .count();
    assert_eq!(fake_rgb, 0, "空态右坞还在写 RGB 假读数：{fake_rgb} 条");
    assert!(
        empty
            .iter()
            .any(|(t, _)| t == i18n::t!("无照片时直方图无意义")),
        "空态直方图空槽没有淡提示"
    );

    // 双向：有照片态 RGB 真读数与计数照旧出现（行为不变）。
    // Two-sided: photo state keeps its RGB readouts and frame count.
    let photo = texts(&photo_shapes(&ctx));
    assert!(
        photo
            .iter()
            .any(|(t, r)| t == "128" && r.center().x > 1200.0 && r.center().y < 260.0),
        "有照片时 RGB 读数不见了"
    );
    assert!(
        photo.iter().any(|(t, _)| t.contains("张 · 已选")),
        "有照片时胶片条计数不见了"
    );
}

#[test]
fn photo_state_keeps_all_controls() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);
    let ctx = context();
    let shapes = photo_shapes(&ctx);
    let photo = texts(&shapes);

    // 控制条三条线：缩放 chip / AI 条 / 底部工具条，有照片时全部在位。
    // All three chrome lines stay in place once a photo exists.
    assert!(
        photo.iter().any(|(t, _)| t == i18n::t!("适应窗口")),
        "有照片时「适应窗口」缺失"
    );
    assert!(
        photo
            .iter()
            .any(|(t, _)| t == i18n::t!("对整段做去闪 + 日转夜 ramp，保留车流鬼影")),
        "有照片时 AI 命令条缺失"
    );
    assert!(
        ["抓手", "整段", "单帧"]
            .iter()
            .all(|label| photo.iter().any(|(t, _)| t == i18n::tr(label))),
        "有照片时工具条三控件不全"
    );

    // 直方图真波形：红通道柱以 from_rgba_unmultiplied(...,46) 画满 96 列。
    // Real waveform: the red channel paints 96 columns at alpha 46.
    let bars = rect_fills(&shapes)
        .iter()
        .filter(|(_, c)| *c == Color32::from_rgba_unmultiplied(0xE0, 0x6A, 0x5C, 46))
        .count();
    assert!(bars >= 24, "有照片时直方图真波形不见了（只剩 {bars} 柱）");

    // 空态同样几项必须全无（双源漂移时这里也会红）。
    // The same items must all be absent on the empty side.
    let empty = texts(&empty_shapes(&ctx));
    assert!(
        !empty.iter().any(|(t, _)| t == i18n::t!("适应窗口")),
        "空态混进了有照片态的控制条"
    );
}

#[test]
fn empty_state_shows_import_entry() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);

    // 静态：两枚按钮仍走真 DemoAction，且真的被 send 出去（可点，不是装饰）。
    // Static: both buttons still send real DemoActions -- clickable, not decoration.
    let src = clean_v21();
    // 两个按钮现在画在全局统一引导页里，dev 与五个 0 帧屏共用同一段真动作。
    // Both buttons now live in the one shared welcome page used by every empty screen.
    let page = body(&src, "fn draw_welcome_page");
    assert!(
        page.contains("DemoAction::OpenFolder") && page.contains("DemoAction::OpenPhotos"),
        "空态导入入口不再是两个真动作"
    );
    assert!(
        page.contains("actions.send(action)"),
        "空态导入按钮没有把动作送进队列"
    );

    let ctx = context();
    let shapes = empty_shapes(&ctx);
    let empty = texts(&shapes);
    assert!(
        empty.iter().any(|(t, _)| t == i18n::t!("打开文件夹")),
        "空态没有「打开文件夹」入口"
    );
    assert!(
        empty.iter().any(|(t, _)| t == i18n::t!("选照片…")),
        "空态没有「选照片…」入口"
    );
    // 品牌必须落在画布井里，不能靠顶栏那枚 logo 混过判据。
    // Brand must sit inside the well; the top-bar logo must not satisfy this.
    assert!(
        empty.iter().any(|(t, r)| {
            t == i18n::t!("恒晷")
                && r.center().y > 200.0
                && r.center().y < 600.0
                && r.center().x > 200.0
                && r.center().x < 1100.0
        }),
        "空态画布井里没有品牌欢迎块"
    );
    assert!(
        empty
            .iter()
            .any(|(t, _)| t == i18n::t!("把照片或文件夹拖进来开始")),
        "空态丢了引导文案"
    );
    let primary = rect_fills(&shapes)
        .iter()
        .filter(|(r, c)| {
            *c == Color32::from_rgb(0xA7, 0x8B, 0xFA)
                && (r.width() - 128.0).abs() < 1.0
                && (r.height() - 32.0).abs() < 1.0
        })
        .count();
    assert!(primary >= 1, "空态主导入按钮没有画出来（或尺寸/配色漂移）");
}

/// 全局统一饱满引导页：空态有卡、有真主按钮、有淡提示，且没有黑画布矩形。
///
/// 静态部分钉住「共用一套绘制」「阴影构造在照片分支内」，运行期双向检查
/// 空态无影 / 有照片保留影。把 `let frame = if st.photo.is_some()` 改回无条件
/// 构造，或把某个空态绕开 `draw_welcome_page`，这条都会红。
///
/// Shared full welcome page: an empty well shows the card, a real primary button
/// and a quiet footer, with no black photo surrogate. Static checks pin the shared
/// painter and the photo-only shadow construction; runtime checks look at both
/// sides of the photo branch. Restoring the unconditional frame turns this red.
#[test]
fn empty_state_welcome_page_is_full_and_shared() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);
    let src = clean_v21();

    let page = body(&src, "fn draw_welcome_page");
    assert!(
        page.contains("PANEL_SIDE") && page.contains("f_bold(20.0)") && page.contains("f_title()"),
        "统一引导页丢了卡片底 / 20px 标题 / 说明档位"
    );
    assert!(
        body(&src, "fn draw_empty_well").contains("draw_welcome_page(")
            && body(&src, "fn draw_well_guide").contains("draw_welcome_page("),
        "dev 或 0 帧屏绕开了统一引导页"
    );
    assert!(
        !src.contains("还没有序列 · 文件 → 打开照片…")
            && !body(&src, "fn draw_well_guide").contains("520.0"),
        "引导页又退回了旧的 seq 专用文案 / 520px 小卡"
    );

    // 假画布根因：frame 构造与三层黑影必须都在 `photo.is_some()` 分支里。
    // Root cause: frame construction and all three shadows must sit in the photo branch.
    let canvas = body(&src, "fn draw_canvas_photo");
    let guard = canvas
        .find("let frame = if st.photo.is_some()")
        .unwrap_or_else(|| panic!("画布 frame 又变成无条件构造"));
    let shadow = canvas
        .find("Color32::from_black_alpha(46)")
        .unwrap_or_else(|| panic!("画布环境影不见了"));
    assert!(guard < shadow, "环境影画在照片分支之前，空态又会出现黑方块");

    let ctx = context();
    let empty = empty_shapes(&ctx);
    let empty_texts = texts(&empty);
    let footer = i18n::t!("支持 JPG / PNG / TIFF · 也可拖入文件夹");
    assert!(
        empty_texts.iter().any(|(t, _)| t.as_str() == footer),
        "统一引导页缺底部格式 / 拖拽提示"
    );
    let cards = rect_fills(&empty)
        .iter()
        .filter(|(r, c)| {
            *c == crate_theme_panel_side()
                && r.width() > 600.0
                && r.width() < 660.0
                && r.height() > 280.0
        })
        .count();
    assert!(
        cards >= 1,
        "空态没有 640x320 的饱满引导卡（实际 {cards} 块）"
    );
    let fake_canvas = rect_fills(&empty)
        .iter()
        .filter(|(r, c)| {
            *c == Color32::from_black_alpha(46) && r.width() > 300.0 && r.height() > 100.0
        })
        .count();
    assert_eq!(fake_canvas, 0, "空态又画出大块黑影相纸：{fake_canvas} 块");
    let primary = rect_fills(&empty)
        .iter()
        .filter(|(r, c)| {
            *c == Color32::from_rgb(0xA7, 0x8B, 0xFA)
                && (r.width() - 128.0).abs() < 1.0
                && (r.height() - 32.0).abs() < 1.0
        })
        .count();
    assert!(primary >= 1, "统一引导页没有可点的主按钮");

    // 双向：有照片时环境影照旧，不能为了过空态判据把真照片的影删掉。
    // Two-sided: real photos keep the ambient shadow; the empty-side fix must not
    // strip the genuine photo treatment.
    let photo_shadow = rect_fills(&photo_shapes(&ctx))
        .iter()
        .filter(|(r, c)| {
            *c == Color32::from_black_alpha(46) && r.width() > 300.0 && r.height() > 100.0
        })
        .count();
    assert!(photo_shadow >= 1, "有照片时环境影不见了");
}

/// 与 v21_demo 的 PANEL_SIDE 同值；判据里写一次，免得把 token 名暴露成 pub。
/// Same value as v21_demo::PANEL_SIDE without making the token public.
fn crate_theme_panel_side() -> Color32 {
    Color32::from_rgb(0x2E, 0x2E, 0x2E)
}

/// 引导页两枚按钮必须与卡片同轴：按钮组中心 == 卡片中心。
///
/// 用户 2026-11-14 报「对齐」：原式 `cx + (2i-1)*(bw/2+6)` 让 i=0 左缘落在
/// `cx-70`、i=1 左缘落在 `cx+70`，组跨度 `cx-70..cx+198`，整体中心 `cx+64`——
/// 整组右偏 64px。判据从**画出来的矩形**取几何（不是读源码常量），所以改回
/// 任何偏心写法都会红。
///
/// The two welcome buttons must share the card's axis: the pair's centre equals the
/// card's centre. The old `cx + (2i-1)*(bw/2+6)` put i=0 at `cx-70` and i=1 at
/// `cx+70`, spanning `cx-70..cx+198` with its centre at `cx+64` -- 64 px right.
/// Geometry is read from the **painted rects**, not from source constants, so any
/// return of an off-centre formula goes red.
#[test]
fn empty_state_welcome_buttons_are_centred() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);
    let ctx = context();
    let shapes = empty_shapes(&ctx);
    let fills = rect_fills(&shapes);

    // 主按钮 = ACCENT 128×32；次按钮 = BUTTON 128×32。两枚都要，缺一枚说明
    // 引导页退化，居中也就无从谈起。
    // Primary = ACCENT 128x32, secondary = BUTTON 128x32. Both required; a missing
    // one means the welcome page regressed and "centred" is meaningless.
    let pick = |c: Color32| -> Option<Rect> {
        fills
            .iter()
            .find(|(r, f)| {
                *f == c && (r.width() - 128.0).abs() < 1.0 && (r.height() - 32.0).abs() < 1.0
            })
            .map(|(r, _)| *r)
    };
    let primary =
        pick(Color32::from_rgb(0xA7, 0x8B, 0xFA)).unwrap_or_else(|| panic!("引导页主按钮没画出来"));
    let secondary =
        pick(Color32::from_rgb(0x42, 0x42, 0x42)).unwrap_or_else(|| panic!("引导页次按钮没画出来"));

    // 卡片：640×320 的 PANEL_SIDE 底，中心就是那根竖轴。
    // Card: the 640x320 PANEL_SIDE fill; its centre is the axis.
    let card = fills
        .iter()
        .find(|(r, c)| {
            *c == crate_theme_panel_side()
                && r.width() > 600.0
                && r.width() < 660.0
                && r.height() > 280.0
        })
        .map(|(r, _)| *r)
        .unwrap_or_else(|| panic!("引导卡没画出来，无法验证居中"));

    let group = primary.union(secondary);
    let total_w = primary.width() + secondary.width() + 12.0;
    assert!(
        (group.width() - total_w).abs() < 0.5,
        "两枚按钮间距不是 12px（实际 {}px）",
        group.width() - primary.width() - secondary.width()
    );
    assert!(
        (group.center().x - card.center().x).abs() < 1.0,
        "按钮组没有与卡片同轴：组中心 x={:.1}，卡片中心 x={:.1}，偏 {:.1}px",
        group.center().x,
        card.center().x,
        group.center().x - card.center().x,
    );
    // 同一行：两枚按钮顶边齐平，否则「居中」只对了横向还是歪的。
    // Same row: equal top edges, or "centred" only holds horizontally.
    assert!(
        (primary.top() - secondary.top()).abs() < 0.5,
        "两枚按钮不在同一行（顶边差 {:.1}px）",
        primary.top() - secondary.top()
    );
}
