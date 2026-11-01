//! 全部集成判据的一个入口。
//!
//! # 为什么不再一个文件一个可执行文件
//!
//! cargo 会把 `tests/` 下每个顶层 `.rs` 各链成一个**完整可执行文件**，每个都要
//! 静态链接一遍 image / rawler / skrifa 这些依赖。28 个文件就是 28 份 50 MB 级的
//! 产物，链接时间也跟着乘 28——这是 `target` 体积的三个乘数之一（台账 C1）。
//!
//! 改成 `tests/suite/main.rs` 一个入口 + 子目录里的模块文件：cargo 只把
//! `tests/suite/main.rs` 认成一个目标，子目录里的 `.rs` 只是模块。判据**一条不少**，
//! 名字变成 `模块名::判据名`，`cargo test xmp_roundtrip::` 这种前缀过滤照样能用。
//!
//! # Why not one executable per file any more
//!
//! cargo links every top-level `.rs` under `tests/` into a **complete executable**, each
//! statically linking image / rawler / skrifa again. Twenty-eight files meant
//! twenty-eight 50 MB-class artefacts and twenty-eight link steps -- one of the three
//! multipliers on `target` size (ledger item C1).
//!
//! Now `tests/suite/main.rs` is the single entry point and the files beside it are just
//! modules. No assertion is lost; names become `module::test`, and prefix filters such
//! as `cargo test xmp_roundtrip::` still work.

// 只在入口引入一次：见下面各文件里那段说明。
// Declared once at the entry point; see the note in the files that use it.
#[path = "../support/mod.rs"]
mod support;

mod align_golden;
mod align_synthetic;
mod crop_golden;
mod cube_golden;
mod dcp_real;
mod develop_golden;
mod dng_reads;
mod export_hdr;
mod export_stills;
mod export_video;
mod fit_denoise;
mod fit_sharpen;
mod fit_sharpen_protect;
mod fit_sliders;
mod golden;
mod interp_basic;
mod lcp_real;
mod lensfun_real;
mod motion_export;
mod orientation_real;
mod project_roundtrip;
mod qa_m2_edge;
mod queue_export;
mod raw_reads;
mod raw_real_sample;
mod retime_export;
mod xmp_map_roundtrip;
mod xmp_roundtrip;
