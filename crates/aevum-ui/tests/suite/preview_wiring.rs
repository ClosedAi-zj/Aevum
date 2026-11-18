//! P0 回归判据：**导入后预览真的出图**——`ensure_preview` 的驱动点不许再丢。
//!
//! # 这条判据守的是什么
//!
//! 预览是拉取式任务：`state.preview_dirty` 只由上层置位，真正发渲染任务的是
//! `AppState::ensure_preview`，回包在 `poll` 里落到 `state.texture`，画布再把
//! 这张纹理画成图片 mesh。用户 2026-11-14 报「无法导入照片 / 很多接口没接入」，
//! 根因是 v21 换壳（`b963631`）把旧 `shell.rs` 末尾的 `state.ensure_preview(&ctx);`
//! 漏搬了：**生产调用点归零**，于是 `preview_dirty` 永远是 true、`preview_inflight`
//! 永远是 false、`texture` 永远是 None——序列其实加载成功了（`frames=1`），
//! 画布却一片空白，读起来就是「照片没导进来」。调色滑块同理：改参数只置脏标记，
//! 没人调度，画面一动不动。
//!
//! # 为什么判据要真跑帧，不做源码扫描
//!
//! `ensure_preview` 写在源码里、但没人调，和写在源码里、有人调，两种情况的
//! **源码文本完全一样**。只有跑起来看「纹理出没出来」才分得开。所以这里的
//! 主判据是运行期：真读一张 jpg → 循环 `poll` / `tick` / `shell::ui` → 直到
//! `state.texture` 出现 → 再画一帧，确认画布上**真有一块用这张纹理的 mesh**。
//!
//! 顺序反过来（先画帧再看）也不行：`ensure_preview` 在 `shell::ui` 帧尾，
//! 回包要下一次 `poll` 才到手，所以循环体必须是 poll → ui → 等。
//!
//! # 变异自证
//!
//! 把 `shell/mod.rs` 末尾的 `state.ensure_preview(&ctx);` 注释掉，本文件
//! `import_then_canvas_shows_photo` 必红（`texture` 恒 None）；还原即绿。
//!
//! # P0 regression: after an import, the preview really reaches the canvas
//!
//! Preview is a pull job: `state.preview_dirty` is only set by callers, the actual
//! render is posted by `AppState::ensure_preview`, the reply lands on
//! `state.texture` in `poll`, and the canvas paints that texture as an image mesh.
//! The user's 2026-11-14 report ("cannot import photos / many interfaces are not
//! wired") traces to the v21 shell swap (`b963631`) dropping the legacy
//! `state.ensure_preview(&ctx);` line: **zero production call sites**, so
//! `preview_dirty` stayed true, `preview_inflight` stayed false and `texture`
//! stayed None -- the sequence really did load (`frames=1`) while the canvas
//! stayed blank, which reads as "the photo never came in". Sliders behave the
//! same way: changing a parameter only sets the dirty flag, nobody schedules, and
//! nothing on screen moves.
//!
//! A source scan cannot tell "the call exists but nobody calls it" apart from "the
//! call exists and is called" -- the text is identical. Only running the frames
//! can. So the primary criterion is runtime: read a real jpg, loop
//! `poll` / `tick` / `shell::ui` until `state.texture` shows up, then paint one
//! more frame and require a mesh bound to that texture. Mutation proof: comment
//! out the `state.ensure_preview(&ctx);` tail in `shell/mod.rs` and this test goes
//! red; restore it and it goes green.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aevum_ui::i18n::{self, Lang};
use aevum_ui::state::{AppState, Module};
use aevum_ui::{shell, theme};

use egui::epaint::ClippedShape;
use egui::{Pos2, Rect, Shape, TextureId};

/// 与 `--selftest` / `i18n_coverage` 同一块画布：1600×1000。
/// Same 1600×1000 canvas as the selftest and the i18n coverage sweep.
fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1600.0, 1000.0))),
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

/// 运行期造一张 64×48 的真 jpg，落在临时目录。
///
/// **不用 `samples/cameras/*`**：那批素材被 gitignore，CI 上没有，判据会假红。
/// 也不用 `demo::ensure_demo` 的产物：那是 demo 通道，绕开了 `open_paths`，
/// 正好躲开这条判据要钉的那段接线。
///
/// 图像画成横向渐变：既保证是真 jpg（不是单色块，解码器不会走退化分支），
/// 也让人肉看截图时一眼认出这是判据自己造的图。
///
/// Generate a real 64x48 jpg at runtime in the temp dir. Deliberately **not**
/// `samples/cameras/*` (gitignored: CI would go falsely red) and not
/// `demo::ensure_demo`'s output (a demo path that bypasses `open_paths`, i.e.
/// exactly the wiring this criterion pins). The gradient keeps it a genuine jpg
/// and makes it recognisable when a human looks at the shot.
fn wire_jpeg() -> PathBuf {
    let dir = std::env::temp_dir().join("aevum-preview-wiring");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        panic!("建不了临时目录 {}：{e}", dir.display());
    }
    let path = dir.join("wire-64x48.jpg");
    // 每次重写：临时目录可能被上一次的坏产物占住（半截图 / 0 字节）。
    // Rewrite every run: a stale half-written file from an earlier aborted run
    // would otherwise poison this one.
    let mut img = image::RgbImage::new(64, 48);
    for (x, _y, px) in img.enumerate_pixels_mut() {
        let t = x as f32 / 63.0;
        px.0 = [
            (24.0 + t * 200.0) as u8,
            (96.0 + t * 120.0) as u8,
            (200.0 - t * 150.0) as u8,
        ];
    }
    if let Err(e) = img.save(&path) {
        panic!("写不了测试 jpg {}：{e}", path.display());
    }
    let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    assert!(len > 0, "测试 jpg 写成空文件：{} 字节", len);
    path
}

/// 画到稳态帧并交出 shape（纹理增量丢掉：这里只关心 shape）。
///
/// 首帧还没有 galley 宽度，写字位置会漂，所以与 `empty_state` 一样先预热三帧。
///
/// Paint to a settled frame and hand back the shapes (the texture delta is
/// dropped: only shapes matter here). Three warm-up frames resolve galley widths,
/// exactly like `empty_state`.
fn paint(ctx: &egui::Context, state: &mut AppState) -> Vec<ClippedShape> {
    for _ in 0..3 {
        let mut warm = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
        warm.textures_delta.clear();
    }
    ctx.run_ui(raw_input(), |ui| shell::ui(state, ui)).shapes
}

/// 递归收集所有 mesh 的纹理 id：画布上的照片就是一块带纹理的 mesh。
/// Recursively collect every mesh's texture id: the canvas photo is one textured mesh.
fn mesh_textures(shapes: &[ClippedShape]) -> Vec<TextureId> {
    let mut out = Vec::new();
    for cs in shapes {
        walk_mesh(&cs.shape, &mut out);
    }
    out
}

fn walk_mesh(shape: &Shape, out: &mut Vec<TextureId>) {
    match shape {
        Shape::Mesh(m) => out.push(m.texture_id),
        Shape::Vec(v) => {
            for s in v {
                walk_mesh(s, out);
            }
        }
        _ => {}
    }
}

/// 等一张纹理上门：poll → tick → 画一帧 → 小睡，最多 `wait`。
///
/// 每次画帧都用**新的独立分支**而不是复用 `paint`：`ensure_preview` 在帧尾，
/// 回包要下一次 `poll` 才落地，所以这里必须交替跑，不能连画四帧再看。
///
/// Wait for a texture: poll, tick, paint one frame, nap -- up to `wait`. The loop
/// must alternate (paint → next poll) because `ensure_preview` runs at the frame
/// tail and its reply only lands on the following `poll`; painting four frames
/// back to back would never see it.
fn pump_until_texture(
    ctx: &egui::Context,
    state: &mut AppState,
    wait: Duration,
) -> Option<TextureId> {
    let deadline = Instant::now() + wait;
    loop {
        state.poll(ctx);
        state.tick(ctx);
        // 画帧本身就是驱动：`shell::ui` 帧尾会跑 `ensure_preview`。
        // Painting is itself the driver: `shell::ui` runs `ensure_preview` at its tail.
        let mut out = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
        out.textures_delta.clear();
        let tex = state.texture.as_ref().map(|t| t.id());
        if tex.is_some() {
            return tex;
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// **P0 主判据**：导入一张真 jpg，画布必须真的出现用它的图片 mesh。
///
/// 三段断言，缺一不可：
/// 1. 序列真的加载了（`seq` 有帧）——否则后面的失败与预览无关，是导入断了；
/// 2. `state.texture` 出现——这是 `ensure_preview` 被调度过的**直接证据**；
/// 3. 画布上有一块 `texture_id` 等于这张纹理的 mesh——这是用户「看见照片」的
///    直接证据，比只看 `texture` 更硬：纹理在手上但没人画，屏幕上照样是空的。
///
/// **The P0 criterion**: import a real jpg and the canvas must paint a mesh bound
/// to it. Three assertions, none optional: (1) the sequence really loaded, or the
/// failure is the import not the preview; (2) `state.texture` appeared -- direct
/// proof `ensure_preview` was scheduled; (3) a canvas mesh carries that texture id
/// -- direct proof the user sees the photo, which is stronger than (2): a texture
/// nobody paints still leaves a blank screen.
#[test]
fn import_then_canvas_shows_photo() {
    let _guard = crate::common::i18n_guard();
    i18n::set(Lang::ZH);
    let photo = wire_jpeg();
    assert!(photo.exists(), "测试素材没造出来：{}", photo.display());

    let ctx = context();
    let mut state = AppState::new();
    // 显影井：默认模块是 Sequence，预览链在显影屏才是用户报告的那一段。
    // Develop well: the default module is Sequence; the reported break is here.
    state.module = Module::Develop;
    // 判据不出门：tick 的偏好落盘出口会写用户真实配置，跑测试不该动那个文件。
    // 与 --shot 通道同一口径（`shot_mode` 只拦落盘，不碰预览链）。
    //
    // Keep the criterion indoors: `tick`'s settings writer would touch the user's
    // real config, which a test must never do. Same as the --shot path -- `shot_mode`
    // blocks the write and nothing else.
    state.shot_mode = true;
    assert!(
        !state.headless,
        "AppState::new() 变成了 headless，ensure_preview 会直接早退，判据失去意义"
    );

    state.open_paths(&ctx, std::slice::from_ref(&photo));

    let tex = pump_until_texture(&ctx, &mut state, Duration::from_secs(20));
    let frames = state.seq.as_ref().map(|s| s.frames.len()).unwrap_or(0);
    assert!(frames >= 1, "导入没成功：序列 0 帧（{}）", photo.display());
    let Some(tex) = tex else {
        panic!(
            "导入后 20 秒内没有预览纹理：preview_dirty={} preview_inflight={} texture.is_none={} \
             —— `ensure_preview` 的帧尾驱动点又丢了（见 shell/mod.rs 末尾）",
            state.preview_dirty,
            state.preview_inflight,
            state.texture.is_none(),
        );
    };

    // 第三段：纹理在手上，还得有人画它。再画一帧看画布上的 mesh。
    // Third leg: a texture in hand still needs a painter. Paint again and look
    // for the mesh on the canvas.
    let shapes = paint(&ctx, &mut state);
    let ids = mesh_textures(&shapes);
    assert!(
        ids.contains(&tex),
        "画布上没有用预览纹理的图片 mesh（纹理 {:?}，本帧 mesh 纹理 {:?}）——\
         纹理有了但没画出来，用户看到的仍是空井",
        tex,
        ids,
    );
}

/// 接线静态自证：生产代码里必须真有 `ensure_preview` 的调用点。
///
/// 运行期判据已经能抓到断线，这条是**便宜的旁证**：它能在 0.1ms 内指出
/// 「调用点又没了」，不必等到 20 秒超时。两条一起才既快又准——静态这条
/// 单看会被「调用了但走不到」骗过，运行期那条单看则超时后只说结论不说原因。
///
/// Cheap corroboration: production code must really contain the call. The runtime
/// criterion catches the break, but only after the 20 s timeout; this one points at
/// "the call site is gone again" instantly. Together they are fast and precise --
/// static alone can be fooled by a call that is never reached, runtime alone only
/// reports the conclusion, not the cause.
#[test]
fn shell_ui_drives_ensure_preview() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell/mod.rs");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到 mod.rs：{e}"));
    // 剥掉整行注释：判据被自己的注释绊倒过（`empty_state` 同口径）。
    // Strip whole-line comments: a comment tripped an earlier criterion.
    let live: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        live.contains("state.ensure_preview(&ctx);"),
        "`shell::ui` 里没有 `state.ensure_preview(&ctx);`——预览驱动点又断了，\
         导入后画布会一直空白"
    );
    // 位置：必须在 `v21_demo::ui` 之后（帧尾），否则拖动草图判定读到的是上一帧
    // 的输入，松手那一帧也不会补全量。
    // Position: after `v21_demo::ui`, or the drag-sketch check reads last frame's
    // input and the release frame never tops up.
    let draw = live
        .find("v21_demo::ui(state, root, &canvas);")
        .unwrap_or_else(|| panic!("mod.rs 里找不到 demo 绘制调用"));
    let drive = live
        .find("state.ensure_preview(&ctx);")
        .unwrap_or_else(|| panic!("mod.rs 里找不到预览驱动"));
    assert!(
        drive > draw,
        "预览驱动排在 demo 绘制之前，拖动草图判定会读到上一帧的指针状态"
    );
}
