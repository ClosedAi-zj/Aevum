//! 全部界面判据的一个入口。
//!
//! # 为什么合一个可执行文件
//!
//! cargo 把 `tests/` 下每个顶层 `.rs` 各链成一个完整可执行文件，每个都要静态链接
//! 一遍 egui / eframe / image 这一整棵树——五个文件就是五份 50 MB 级的产物，链接时间
//! 也乘五。这是 `target` 体积的三个乘数之一（台账 C1）。
//!
//! 合并之后有一个新问题：进程从五个变成一个，**全局状态开始互相看得见**。这份用例集里
//! `i18n_coverage` 与 `i18n_errors` 都要改当前语言、都要读缺译探针。所以这里多一个
//! `common` 模块专门放那把锁，两边都从它拿，见 `common.rs` 里的说明。
//!
//! # Why one executable
//!
//! cargo links every top-level `.rs` under `tests/` into a complete executable, each
//! statically linking the whole egui / eframe / image tree again: five files meant five
//! 50 MB-class artefacts and five link steps (ledger item C1).
//!
//! Merging introduces one new problem: five processes become one, so **global state is now
//! shared**. `i18n_coverage` and `i18n_errors` both switch the current language and both
//! read the missing-translation probe, so the lock lives in the `common` module and both
//! sides take it from there -- see the notes in `common.rs`.

mod common;
mod i18n_coverage;
mod i18n_errors;
mod i18n_literals;
mod mcp_routes_contract;
mod perf_budget;
mod readme_shots;
mod ui_tokens;
