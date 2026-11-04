//! 命令注册表与工具实现的分发口。
//!
//! 工具名统一用 `aevum_` 前缀平铺：MCP 客户端不会替我们加命名空间，
//! 平铺后 agent 一眼能看出这是 Aevum Studio 的能力。
//!
//! 所有写操作都遵守两条底线：不覆盖源图、不删除源图；覆盖已有输出和
//! 批量写真要调用方显式给 `confirm: true`。
//!
//! 实现按功能域拆在同目录的子模块里，这个文件只剩四件东西：模块文档、
//! `to_value!` 宏、子模块声明、`commands` 的一次性构建与 `dispatch` 的路由。
//! 拆分是纯搬迁——工具名、描述、参数 schema、默认值、错误文案一个字节都不许变，
//! 判据是 `tests/suite/tools_list_snapshot.rs`：`tools/list` 的 JSON 逐字节比对。
//!
//! 域划分（每个子模块头部都写了它管什么、边界在哪）：
//!
//! - `sequence` 序列：打开 / 信息 / 测光 / 去闪
//! - `develop` 调色：参数 / 预设 / 副本 / 关键帧
//! - `export` 导出与渲染：预览 / 静帧 / 视频 / 变速 / 后台任务
//! - `merge` 合成：HDR / 全景 / 堆栈 / 时间合成 / 焦点合成
//! - `library` 素材库：扫描与筛选
//! - `queue` 批量队列
//! - `align` 对齐与任务状态
//! - `project` 工程 / XMP / .cube
//! - `shared` 跨域共用的参数解析与安全检查
//! - `spec` 工具清单与 schema（`tools/list` 的数据源）
//!
//! The command registry and the dispatch point for the tool implementations.
//!
//! Tool names are flat and all prefixed with `aevum_`: MCP clients do not add a
//! namespace for us, and a flat list lets an agent see at a glance that these are
//! Aevum Studio capabilities.
//!
//! Every write operation keeps two bottom lines: never overwrite a source image,
//! never delete one. Overwriting existing output and batch writes require the
//! caller to pass `confirm: true` explicitly.
//!
//! The implementations are split into submodules next door, and this file keeps only four
//! things: the module docs, the `to_value!` macro, the submodule declarations, and the
//! one-shot build of `commands` plus the routing in `dispatch`. The split is a pure move --
//! not one byte of a tool name, description, argument schema, default value, or error
//! message may change, and the criterion for that is
//! `tests/suite/tools_list_snapshot.rs`, a byte-for-byte comparison of the `tools/list` JSON.
//!
//! The domains (each submodule's header states what it owns and where its boundary is):
//!
//! - `sequence` sequence: open / info / measure / deflicker
//! - `develop` grading: parameters / presets / copies / keyframes
//! - `export` export and rendering: preview / stills / video / retime / background jobs
//! - `merge` fusion: HDR / panorama / stacking / time blending / focus stacking
//! - `library` library: scanning and culling
//! - `queue` batch queue
//! - `align` alignment and job state
//! - `project` project / XMP / .cube
//! - `shared` shared argument parsing and safety checks
//! - `spec` the tool catalogue and schemas (the data source for `tools/list`)

use std::sync::OnceLock;

use aevum_core::errmsg::clip_echo;
use serde_json::Value;

use crate::Command;
use crate::session::Session;

/// 核心类型都实现了 serde，这里统一把序列化错误翻成工具错误。
///
/// Core types all implement serde; this turns a serialisation error into a tool error in one
/// place.
///
/// 位置有讲究：`macro_rules!` 是**按文本顺序**生效的，子模块的 `mod` 声明必须排在它
/// 后面，各域里那句 `to_value!(...)` 才找得到它。
///
/// Position matters: `macro_rules!` takes effect in **textual order**, so the `mod`
/// declarations have to come after it for the `to_value!(...)` calls inside the domains to
/// resolve.
macro_rules! to_value {
    ($value:expr) => {
        serde_json::to_value($value).map_err(|e| format!("序列化失败：{e}"))?
    };
}

mod align;
mod develop;
mod export;
mod library;
mod merge;
mod project;
mod queue;
mod retouch;
mod sequence;
mod shared;
mod spec;

pub(crate) use self::shared::echo_text;

use self::align::{align_run, align_status, cancel};
use self::develop::{
    grade_copy, grade_get, grade_preset, grade_reset, grade_set, keyframe_add, keyframe_list,
    keyframe_remove,
};
use self::export::{export_stills, export_video, render_preview};
use self::library::{cull_evaluate, cull_extract, sequences_scan};
use self::merge::{focus_merge, hdr_merge, pano_merge, stack_merge, timeblend_merge};
use self::project::{
    cube_export, cube_import, journal_list, project_get, project_save, xmp_read, xmp_write,
};
use self::queue::{
    batch_run, queue_add, queue_cancel, queue_clear, queue_list, queue_remove, queue_run,
    queue_status,
};
use self::retouch::{retouch_edit, upright_set};
use self::sequence::{deflicker_analyze, measure, open_sequence, sequence_info};
use self::spec::build_commands;

pub(crate) fn commands() -> &'static [Command] {
    static COMMANDS: OnceLock<Vec<Command>> = OnceLock::new();
    COMMANDS.get_or_init(build_commands)
}

pub(crate) fn dispatch(session: &mut Session, id: &str, args: &Value) -> Result<Value, String> {
    // 再规范化一次（`lib::dispatch` 已经做过，这里是幂等的）。目的是保证
    // **`is_write_op` 看到的一定是规范名**——日志丢了没人会发现，多转一次很便宜。
    let id = aevum_core::brand::canonical_tool(id);
    let id = id.as_ref();
    let result = dispatch_inner(session, id, args);
    // 成功的写操作追加进序列目录的 op_journal.jsonl；日志失败不影响工具结果。
    if result.is_ok()
        && aevum_core::journal::is_write_op(id)
        && let Some(root) = session.seq.as_ref().map(|s| s.root.clone())
    {
        let _ = aevum_core::journal::append(&root, "mcp", id, args);
    }
    result
}

fn dispatch_inner(session: &mut Session, id: &str, args: &Value) -> Result<Value, String> {
    // 先把已经结束的后台任务收进会话，后面读进度/结果都是最新状态。
    session.poll_job();
    match id {
        "aevum_open_sequence" => open_sequence(session, args),
        "aevum_sequence_info" => sequence_info(session),
        "aevum_measure" => measure(session, args),
        "aevum_deflicker_analyze" => deflicker_analyze(session, args),
        "aevum_grade_get" => grade_get(session),
        "aevum_grade_set" => grade_set(session, args),
        "aevum_grade_preset" => grade_preset(session, args),
        "aevum_grade_reset" => grade_reset(session),
        "aevum_grade_copy" => grade_copy(session, args),
        "aevum_keyframe_add" => keyframe_add(session, args),
        "aevum_keyframe_remove" => keyframe_remove(session, args),
        "aevum_keyframe_list" => keyframe_list(session, args),
        "aevum_retouch" => retouch_edit(session, args),
        "aevum_upright" => upright_set(session, args),
        "aevum_render_preview" => render_preview(session, args),
        "aevum_export_stills" => export_stills(session, args),
        "aevum_export_video" => export_video(session, args),
        "aevum_hdr_merge" => hdr_merge(session, args),
        "aevum_focus_merge" => focus_merge(session, args),
        "aevum_pano_merge" => pano_merge(session, args),
        "aevum_stack_merge" => stack_merge(session, args),
        "aevum_timeblend_merge" => timeblend_merge(session, args),
        "aevum_cull_evaluate" => cull_evaluate(session, args),
        "aevum_cull_extract" => cull_extract(session, args),
        "aevum_project_get" => project_get(session),
        "aevum_project_save" => project_save(session),
        "aevum_xmp_read" => xmp_read(args),
        "aevum_xmp_write" => xmp_write(session, args),
        "aevum_cube_import" => cube_import(session, args),
        "aevum_cube_export" => cube_export(session, args),
        "aevum_align" => align_run(session, args),
        "aevum_align_status" => align_status(session),
        "aevum_cancel" => cancel(session),
        "aevum_queue_list" => queue_list(session),
        "aevum_queue_add" => queue_add(session, args),
        "aevum_queue_remove" => queue_remove(session, args),
        "aevum_queue_clear" => queue_clear(session),
        "aevum_queue_run" => queue_run(session, args),
        "aevum_queue_status" => queue_status(session),
        "aevum_queue_cancel" => queue_cancel(session),
        "aevum_sequences_scan" => sequences_scan(session, args),
        "aevum_batch_run" => batch_run(session, args),
        "aevum_journal_list" => journal_list(session, args),
        other => Err(format!("未知工具：{}", clip_echo(other))),
    }
}
