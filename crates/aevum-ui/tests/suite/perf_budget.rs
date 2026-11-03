//! 帧时预算判据：面板切换与滑块拖动的 CPU 帧时不许越过 16ms。
//!
//! # 判的是什么
//!
//! `aevum_ui::bench_ui_rows()` 走的是和 `--bench-ui` 同一条路径：无头 `run_ui`
//! 墙钟、预热 5 帧、每场景 30 帧。判据只看**中位**——单帧毛刺在 CI 上太多，
//! 拿 p95 卡会把机器抖动当成代码退化（数字照样打印出来，看得见）。
//!
//! # 阈值怎么来的
//!
//! 端到端 60fps 预算是 16ms，而本夹具只量 CPU 侧（布局 + 绘制提交），
//! 所以 16ms 是**上限**而不是目标。本机实测（2026-11-03，release 与 debug 各跑
//! 三轮）：12 组面板 + 滑块的中位最大 1.4ms、p95 最大 3.0ms——取 16ms 意味着
//! 还有 10 倍以上的余量，CI 上慢三五倍的机器也不会误报。
//!
//! 变异验证（2026-11-03 提交后，两台机器各做一遍）：在 `shell::ui` 每帧克隆一个
//! 128MB 的 `Vec<f32>`，**两台都变红**（开发机中位 35–41ms，QA 机 31–46ms，报错
//! 点名最差场景）。换成 64MB 时两台落在 16ms 的两侧：开发机 12–15ms（绿）、
//! QA 机 18–20ms（三次连跑全红）——**别把 64MB 当安全余量**，这个量级本来就在
//! 预算线上。这条判据卡的是 16ms 端到端预算本身，不是灵敏度计；想知道「哪一组
//! 离预算还有多远」，看打印出来的那张表，不看红绿。
//!
//! # What this fences
//!
//! Same path as `--bench-ui`: headless `run_ui` wall clock, 5 warm-up frames, 30
//! measured frames per scenario. Only the **median** is asserted: single-frame spikes are
//! too common on CI, and a p95 fence would report machine noise as a regression (the p95
//! is still printed).
//!
//! The 16ms figure is the end-to-end 60fps budget and therefore a ceiling, not a target;
//! this harness only measures the CPU side. Locally (2026-11-03, release and debug, three
//! runs each) the worst median was 1.4ms and the worst p95 3.0ms, so 16ms leaves more than
//! ten times of headroom for a slower CI box. Mutation check (two machines): cloning a 128MB
//! `Vec<f32>` per frame in `shell::ui` turned this test red on both (median 35-41ms on the
//! dev box, 31-46ms on the QA box). At 64MB the two machines landed on opposite sides of the
//! line (12-15ms green here, 18-20ms red on QA, three runs in a row), so 64MB is not a safe
//! margin -- that size sits right on the budget.

use aevum_ui::bench_ui_rows;

/// 端到端 60fps 预算：CPU 侧只有 16ms 的全部额度，越过它无论 GPU 多快都会掉帧。
///
/// End-to-end 60fps budget: the CPU side may not spend more than this.
const BUDGET_MS: f64 = 16.0;

#[test]
fn frame_time_stays_inside_the_cpu_budget() {
    // 和画界面那几条判据共用一把锁：同一个可执行文件里它们也在画 egui 帧，
    // 并行跑会把彼此的 CPU 时间算进墙钟。
    //
    // Share the lock with the painting tests: in this merged executable they paint egui
    // frames too, and running in parallel would charge their CPU time to our wall clock.
    let _lock = crate::common::i18n_guard();

    let rows = bench_ui_rows().expect("基准跑不起来");
    assert!(!rows.is_empty(), "基准没有产出任何场景");

    let mut table = String::from("帧时预算（中位 / p95 / 首帧，ms）：\n");
    let mut worst = 0.0f64;
    let mut worst_name = String::new();
    for row in &rows {
        table.push_str(&format!(
            "  {:<24} 中位 {:6.2}  p95 {:6.2}  首帧 {:6.2}\n",
            row.name, row.median_ms, row.p95_ms, row.first_ms
        ));
        if row.median_ms > worst {
            worst = row.median_ms;
            worst_name = row.name.clone();
        }
    }
    println!("{table}");

    assert!(
        worst <= BUDGET_MS,
        "「{worst_name}」中位 {worst:.2}ms 越过 {BUDGET_MS:.0}ms 的 CPU 预算：\n{table}"
    );
}
