//! 无头帧时基准：`--bench-ui` 的口径与实现。
//!
//! # 口径（写死在这里，数字才有意义）
//!
//! - **预热 5 帧丢弃**：字体栅格、纹理上传、各面板首建缓存都不上账，测的是稳态。
//! - **每个场景连续测 30 帧**，报**中位**与 **p95**（最近秩，`ceil(0.95·n)`）。
//! - **计时区间只有 `egui::Context::run_ui` 的墙钟**：即 CPU 侧的布局 + 绘制提交。
//!   **不含** GPU 上传与呈现，**不是端到端帧时**——这里量的是 CPU 预算：面板与滑块
//!   每帧交给 egui 的那部分开销。端到端帧时还要加上上传、呈现与垂直同步，本夹具看不到。
//! - 另记**切换首帧**（第 0 帧，冷缓存）：面板切过去的头一帧是用户真正看见卡顿的地方，
//!   它的数字并进报告，但不进中位/p95 的判据（判据只卡稳态，见 `perf_budget` 判据）。
//! - **墙钟对机器负载敏感**：测之前把编译、导出这类重活停掉。带负载测出来的数字偏高，
//!   那是机器在忙，不是界面变慢——两台机器差出两倍也正常，只比同机同负载下的前后。
//!
//! # 场景
//!
//! - **A 面板切换**：六个模块各跑「默认折叠 / 全展开」两种，共 12 组。整轮共用一个
//!   `Context`（真实会话里字体与样式是热的），每组换一份新的 `AppState::demo()`。
//! - **B 滑块拖动**：去闪模块的「曝光」（`def-exposure` 组），每帧把 `params.exposure`
//!   从 −2 推到 +2 共 30 步，模拟按住滑块拖动时界面每帧的重绘。
//!
//! # Measurement basis
//!
//! - Five warm-up frames are discarded; each scenario then measures 30 consecutive frames
//!   and reports the median and the p95 (nearest rank).
//! - The timed region is only the wall clock of `egui::Context::run_ui`: CPU-side layout and
//!   draw submission. It excludes GPU upload and presentation -- this is a **CPU budget**,
//!   not an end-to-end frame time.
//! - The first frame after a switch (cold caches) is reported separately as an indicator but
//!   is not part of the median/p95 budget.
//! - Wall clock is load sensitive: stop compiles and exports before measuring. A number taken
//!   under load is the machine being busy, not the UI getting slower; two machines differing
//!   by 2x is normal, so only compare before/after on the same box under the same load.
//! - Scenario A walks the six modules in both the default-collapsed and fully-expanded
//!   states (12 groups). One `Context` is shared by the whole run (fonts and styles are
//!   warm in a real session); each group gets a fresh demo state. Scenario B drives the
//!   flicker module's exposure slider from -2 to +2 over 30 steps.

use std::time::Instant;

use crate::AppState;
use crate::shell;
use crate::state::{self, Module};
use crate::theme;

/// 预热帧数：首建字体、纹理与各组缓存都在这几帧里发生。
///
/// Warm-up frames: font/atlas and first-build caches happen here.
const WARMUP: usize = 5;

/// 每场景计时帧数。30 帧足够给出稳定的中位与 p95，又不会让本机 build 变慢。
///
/// Timed frames per scenario: enough for a stable median and p95.
const MEASURE: usize = 30;

/// 一组实测结果。
///
/// One measured scenario.
#[derive(Clone, Debug)]
pub struct BenchRow {
    /// 场景名（中文，带模块/折叠态）。
    ///
    /// Scenario name (Chinese, with module and fold state).
    pub name: String,
    /// 30 帧的中位耗时（毫秒）。
    ///
    /// Median frame time over 30 frames, in milliseconds.
    pub median_ms: f64,
    /// 30 帧的 p95 耗时（毫秒，最近秩）。
    ///
    /// p95 frame time over 30 frames, in milliseconds (nearest rank).
    pub p95_ms: f64,
    /// 切换后的第一帧（冷缓存），只作参考，不进判据。
    ///
    /// First frame after the switch (cold caches); reference only, not part of the budget.
    pub first_ms: f64,
}

/// 跑全部场景，返回结构化结果（`--bench-ui` 与时间预算判据共用同一条路径）。
///
/// Run every scenario and return structured rows; both `--bench-ui` and the time-budget
/// test go through this single path so the numbers in the report and the fence agree.
pub fn bench_ui_rows() -> Result<Vec<BenchRow>, String> {
    // 与 selftest 同一条道理：形状与排版要可复现，基准固定中文，不跟随用户 settings。
    //
    // Same reasoning as selftest: the layout must be reproducible, so the benchmark pins
    // Chinese instead of following the user's settings.
    crate::i18n::set(crate::i18n::Lang::ZH);

    let mut rows = Vec::with_capacity(Module::ALL.len() * 2 + 1);

    // 整个基准共用一个 `Context`：真实会话里字体图集、主题样式是热的，切面板
    // 不会重建它们。每个场景仍换一份新的 `AppState::demo()`，不带着上一屏的状态跑。
    // 开头先空跑 [`WARMUP`] 帧，把「新开 Context」的一次性建账（字体栅格、样式表）
    // 摘出去——否则每组测到的首帧都在量新建 Context，而不是量切面板。
    //
    // One `Context` for the whole run: in a real session the font atlas and theme are
    // warm and switching panels does not rebuild them. Each scenario still gets a fresh
    // demo state. A leading warm-up pass takes the one-time "new context" cost (font
    // raster, style tables) out of the picture, so the first-frame column measures the
    // switch rather than context creation.
    let ctx = egui::Context::default();
    theme::apply(&ctx, 1.0);
    {
        let mut warm = AppState::demo();
        warm.install_demo_textures(&ctx);
        for _ in 0..WARMUP {
            let mut out = ctx.run_ui(frame_input(), |ui| shell::ui(&mut warm, ui));
            out.shapes.clear();
            out.textures_delta.clear();
        }
    }

    for module in Module::ALL {
        let states = [
            ("默认折叠", state::default_collapsed()),
            ("全展开", std::collections::HashSet::new()),
        ];
        for (label, collapsed) in states {
            let mut state = AppState::demo();
            state.install_demo_textures(&ctx);
            state.module = module;
            state.collapsed = collapsed;
            let (first, median, p95) = sample(&ctx, &mut state, |_, _| {});
            rows.push(BenchRow {
                name: format!("A {}/{label}", module.cn()),
                median_ms: median,
                p95_ms: p95,
                first_ms: first,
            });
        }
    }

    // 场景 B：拖滑块。曝光在「去闪」模块的 `def-exposure` 组里（`basic` 组另有同名
    // 读数，但只有这一组是本模块独占的滑块路径）。每帧推进一小步，模拟拖动。
    //
    // Scenario B: slider drag. Exposure lives in the flicker module's `def-exposure`
    // group; advancing one small step per frame mimics holding the slider.
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    state.module = Module::Deflicker;
    state.collapsed.remove("def-exposure");
    let (first, median, p95) = sample(&ctx, &mut state, |state, i| {
        // 预热帧也走同一条推进，保证进入计时窗口时滑块已经在动。
        //
        // Warm-up frames advance too, so the slider is already moving when timing starts.
        let t = (i as f64 / (MEASURE - 1) as f64).clamp(0.0, 1.0);
        state.params.exposure = -2.0 + 4.0 * t;
        state.preview_dirty = true;
    });
    rows.push(BenchRow {
        name: "B 去闪/曝光 −2→+2".to_owned(),
        median_ms: median,
        p95_ms: p95,
        first_ms: first,
    });

    Ok(rows)
}

/// 把结构化结果排成给人看的表（`--bench-ui` 打印它）。
///
/// Format the rows into the table `--bench-ui` prints.
pub fn bench_ui() -> Result<String, String> {
    let rows = bench_ui_rows()?;
    let mut out = String::new();
    out.push_str(
        "无头 CPU 帧时（预热 5 帧丢弃 → 每场景 30 帧；中位/p95/首帧 单位 ms）\n\
         口径：仅 egui 布局 + 绘制提交的 CPU 墙钟，不含 GPU 上传与呈现——是 CPU 预算，不是端到端帧时。\n",
    );
    let mut last_tag = '\0';
    for row in &rows {
        let tag = row.name.chars().next().unwrap_or(' ');
        if tag != last_tag {
            out.push_str(if tag == 'A' {
                "场景 A · 面板切换（切换后的稳态；首帧为冷缓存参考）\n"
            } else {
                "场景 B · 滑块拖动（每帧推进 1/29 行程）\n"
            });
            last_tag = tag;
        }
        out.push_str(&format!(
            "  {:<22} 中位 {:6.2}  p95 {:6.2}  首帧 {:6.2}\n",
            row.name.trim_start_matches("A ").trim_start_matches("B "),
            row.median_ms,
            row.p95_ms,
            row.first_ms
        ));
    }
    Ok(out)
}

/// 跑一个场景：预热 [`WARMUP`] 帧后计时 [`MEASURE`] 帧，返回（首帧, 中位, p95）毫秒。
///
/// `tick` 在每帧绘制前调用，用来改状态（场景 B 推滑块）；场景 A 传空闭包。
///
/// Run one scenario: [`WARMUP`] frames discarded, then [`MEASURE`] timed, returning
/// (first frame, median, p95) in milliseconds. `tick` mutates state before each frame.
fn sample(
    ctx: &egui::Context,
    state: &mut AppState,
    mut tick: impl FnMut(&mut AppState, usize),
) -> (f64, f64, f64) {
    let mut samples = Vec::with_capacity(MEASURE);
    let mut first = 0.0;
    for i in 0..WARMUP + MEASURE {
        tick(state, i);
        let t0 = Instant::now();
        let mut out = ctx.run_ui(frame_input(), |ui| shell::ui(state, ui));
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        // 形状与纹理增量只在本夹具里存在；不清掉会让下一帧的量级失真。
        //
        // The shape list and texture delta exist only for this harness; clearing them keeps
        // the next frame's cost honest.
        out.shapes.clear();
        out.textures_delta.clear();
        if i == 0 {
            first = dt;
        }
        if i >= WARMUP {
            samples.push(dt);
        }
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    let median = if samples.len() % 2 == 0 {
        let mid = samples.len() / 2;
        (samples[mid - 1] + samples[mid]) / 2.0
    } else {
        samples[samples.len() / 2]
    };
    // 最近秩：第 ceil(0.95·n) 个样本（1 起算）。
    //
    // Nearest rank: the ceil(0.95*n)-th sample, one-based.
    let rank = (samples.len() as f64 * 0.95).ceil().max(1.0) as usize;
    let p95 = samples[rank.min(samples.len()) - 1];
    (first, median, p95)
}

/// 与 `selftest` 同尺寸的假屏幕（1600×1000）。
///
/// Same fake screen as `selftest` (1600x1000).
fn frame_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        max_texture_side: Some(8192),
        ..Default::default()
    }
}
