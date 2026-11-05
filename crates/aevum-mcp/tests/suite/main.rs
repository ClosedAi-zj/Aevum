//! 全部 MCP 集成判据的一个入口。
//!
//! # 为什么合一个可执行文件
//!
//! cargo 把 `tests/` 下每个顶层 `.rs` 各链成一个完整可执行文件，每个都要静态链接
//! 一遍整个依赖树。两个文件就两遍；合成一个之后判据一条不少，名字变成
//! `模块名::判据名`（`cargo test tools::` 这种前缀过滤照样用）。理由与
//! `aevum-core/tests/suite/main.rs` 相同，见台账 C1。
//!
//! # Why one executable
//!
//! cargo links every top-level `.rs` under `tests/` into a complete executable, each
//! statically linking the whole dependency tree again. Two files meant two copies. Merged
//! into one, no assertion is lost and names become `module::test`, so prefix filters such
//! as `cargo test tools::` still work. Same reasoning as
//! `aevum-core/tests/suite/main.rs`; see ledger item C1.

mod deflicker_align;
mod mcp_stdio;
mod retouch;
mod tool_annotations;
mod tools;
mod tools_list_snapshot;
