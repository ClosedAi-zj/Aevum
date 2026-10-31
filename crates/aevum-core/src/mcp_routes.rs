//! MCP 工具名与 GUI 控制通道命令 ID 的**唯一**对应表。
//!
//! 为什么要单独立一张表：同一个产品有两个 MCP 入口，命令清单原本各写一份——
//!
//! 1. **服务模式**（`aevum --mcp`）：自己实现 34 个工具，清单在 `aevum-mcp::tools`；
//! 2. **附着模式**（`aevum --mcp --attach control.json`）：自己不实现，只把调用转给
//!    正在运行的 GUI，命令清单在 `aevum-ui::control::command_catalog`（72 条）。
//!
//! 附着模式的工具名由命令 ID **机械加前缀**生成（`xmp.import` → `aevum_xmp_import`），
//! 而服务模式那边这条工具叫 `aevum_xmp_read`。同一个能力两个名字，只靠机械推导就断线：
//! 附着模式收到 `aevum_xmp_read` 反查不到任何命令，报「未知工具」，可它明明是一条
//! 正式发布的工具名。`aevum_xmp_write` 同理。
//!
//! 所以这里放三样东西，其余地方一律引用，不许再另写一份：
//!
//! - [`COMMANDS`]：GUI 控制通道能执行的全部命令 ID（与 `command_catalog()` 一一对应）；
//! - [`TOOL_ROUTES`]：每个 MCP 工具名落到哪条命令；`None` 表示 GUI 没有这项能力，
//!   只能直连服务模式；
//! - [`command_for_mcp_name`] / [`tool_command`]：两条查表路径，合起来覆盖
//!   「34 个工具名」与「72 个命令名」两个命名空间。
//!
//! 三份清单（这张表、`tools.rs`、`command_catalog()`）由测试逐条互锁：任何一处增删
//! 或拼错，本地测试当场就红，不会等用户来报「附着模式不认识某个工具」。
//!
//! 本模块零依赖（只用 `std` 与 [`crate::brand`] 的常量），好让 `aevum-core`、
//! `aevum-ui`、`aevum-mcp` 三方都能直接引用而不形成环。

use crate::brand::TOOL_PREFIX;

/// GUI 控制通道的全部命令 ID，顺序与 `command_catalog()` 一致。
///
/// **这是副本**：真正的执行体在 `aevum-ui::control`，描述与 JSON Schema 也在那边
/// （那些是给界面用的文案，不该塞进 core）。这里只抄 ID，并由
/// `aevum-ui` 的 `catalog_matches_core_command_table` 测试逐条比对——
/// 少一条、多一条、拼错一个字母，测试都会红。
pub const COMMANDS: &[&str] = &[
    "sequence.info",
    "sequence.open",
    "sequence.check",
    "sequence.status",
    "sequence.cancel",
    "frame.select",
    "ui.module",
    "ui.dock",
    "grade.get",
    "grade.auto",
    "grade.set",
    "grade.preset",
    "grade.reset",
    "grade.copy",
    "xmp.import",
    "xmp.export",
    "cube.import",
    "cube.export",
    "audio.config",
    "mask.list",
    "mask.add",
    "mask.remove",
    "mask.update",
    "mask.component",
    "deflicker.set",
    "crop.set",
    "lens.set",
    "profile.set",
    "library.add",
    "library.remove",
    "library.list",
    "library.open",
    "library.organize",
    "library.keyword",
    "library.metadata",
    "library.stack",
    "collection.create",
    "collection.add",
    "collection.remove",
    "collection.delete",
    "collection.list",
    "render.set",
    "retime.set",
    "retime.status",
    "motion.set",
    "motion.status",
    "align.run",
    "align.status",
    "export.video",
    "export.stills",
    "export.status",
    "export.cancel",
    "hdr.merge",
    "pano.merge",
    "merge.status",
    "merge.cancel",
    "queue.list",
    "queue.add",
    "queue.remove",
    "queue.clear",
    "queue.run",
    "queue.status",
    "queue.cancel",
    "project.save",
    "project.get",
    "project.metadata",
    "journal.list",
    "history.list",
    "history.undo",
    "history.redo",
    "history.jump",
    "ui.screenshot",
];

/// 一个 MCP 工具在附着模式下能落到哪条 GUI 命令。
#[derive(Debug, Clone, Copy)]
pub struct ToolRoute {
    /// 对外工具名：`aevum-mcp` 的 `tools/list` 里出现的那个名字。
    pub tool: &'static str,
    /// 附着模式下要 POST 给 GUI 控制通道的命令 ID。
    ///
    /// `None` 的含义是「GUI 侧没有对应的可执行命令」，**不是**「这条工具不存在」。
    /// 这些能力只在服务模式可用，报错时要说清楚，别让用户以为是名字拼错了。
    pub command: Option<&'static str>,
}

/// 全部 34 个 MCP 工具，以及它们在附着模式下的去向。
///
/// 顺序与 `aevum-mcp::tools::build_commands` 一致（`tools.rs` 有测试逐条比对）。
///
/// 工具名**一律沿用已发布的拼法**，哪怕它和命令 ID 对不齐——
/// `aevum_xmp_read` 就是 `xmp.import`，改名会让已经配好的客户端全部失效。
/// 这张表存在的意义之一，正是把这种历史错位集中记在一处，而不是散在代码里当隐性知识。
pub const TOOL_ROUTES: &[ToolRoute] = &[
    route("aevum_open_sequence", "sequence.open"),
    route("aevum_sequence_info", "sequence.info"),
    // 测光：GUI 打开序列时自动跑一遍，没有单条命令可按需触发。
    route_only_server("aevum_measure"),
    // 去闪分析：GUI 侧只有 `deflicker.set`（改参数），分析与曲线在面板里联动。
    route_only_server("aevum_deflicker_analyze"),
    route("aevum_grade_get", "grade.get"),
    route("aevum_grade_set", "grade.set"),
    route("aevum_grade_preset", "grade.preset"),
    route("aevum_grade_reset", "grade.reset"),
    route("aevum_grade_copy", "grade.copy"),
    // 关键帧：GUI 的关键帧面板直接改工程，没有独立命令。
    route_only_server("aevum_keyframe_add"),
    route_only_server("aevum_keyframe_remove"),
    route_only_server("aevum_keyframe_list"),
    // 预览渲染：GUI 有 `render.set`（改输出设置），没有「渲染这一帧」命令。
    route_only_server("aevum_render_preview"),
    route("aevum_export_stills", "export.stills"),
    route("aevum_export_video", "export.video"),
    route("aevum_hdr_merge", "hdr.merge"),
    route("aevum_pano_merge", "pano.merge"),
    route("aevum_project_get", "project.get"),
    route("aevum_project_save", "project.save"),
    route("aevum_journal_list", "journal.list"),
    // 历史错位：工具叫 read/write，命令叫 import/export。两者指同一件事。
    route("aevum_xmp_read", "xmp.import"),
    route("aevum_xmp_write", "xmp.export"),
    route("aevum_cube_import", "cube.import"),
    route("aevum_cube_export", "cube.export"),
    route("aevum_align", "align.run"),
    route("aevum_align_status", "align.status"),
    // 统一取消：GUI 把取消拆成了 sequence/export/merge/queue 四条，
    // 没有「取消现在跑着的那个」这种命令，所以不能瞎映射到其中一条。
    route_only_server("aevum_cancel"),
    route("aevum_queue_list", "queue.list"),
    route("aevum_queue_add", "queue.add"),
    route("aevum_queue_remove", "queue.remove"),
    route("aevum_queue_clear", "queue.clear"),
    route("aevum_queue_run", "queue.run"),
    route("aevum_queue_status", "queue.status"),
    route("aevum_queue_cancel", "queue.cancel"),
];

/// 工具名 → 附着模式的命令 ID。
///
/// 返回 `None` 有两种可能，调用方要用 [`is_known_tool`] 区分：
/// 「这条工具存在但 GUI 干不了」与「根本没这个工具」。
pub fn tool_command(tool: &str) -> Option<&'static str> {
    TOOL_ROUTES
        .iter()
        .find(|route| route.tool == tool)
        .and_then(|route| route.command)
}

/// 这个名字是不是已知的 MCP 工具（不区分它在附着模式下能不能跑）。
pub fn is_known_tool(tool: &str) -> bool {
    TOOL_ROUTES.iter().any(|route| route.tool == tool)
}

/// 命令 ID → MCP 工具名：`sequence.info` → `aevum_sequence_info`。
///
/// 严格客户端只接受 `[A-Za-z0-9_-]`，所以工具名不能带点；命令 ID 仍保留点号。
pub fn mcp_name(command: &str) -> String {
    format!("{TOOL_PREFIX}{}", command.replace('.', "_"))
}

/// 工具名 → 命令 ID，只走「机械加前缀」这一条路（覆盖 [`COMMANDS`] 全部 72 条）。
///
/// 用于附着模式：GUI 在线的工具清单是从命令目录现生成的，名字就是 `mcp_name(命令 ID)`。
/// 这批名字里有 `aevum_grade_auto`、`aevum_library_add`、`aevum_history_undo` 等等，
/// 它们不在 [`TOOL_ROUTES`] 里（那 34 条是服务模式的工具），必须靠这里才能路由。
pub fn command_for_mcp_name(name: &str) -> Option<&'static str> {
    COMMANDS.iter().copied().find(|id| mcp_name(id) == name)
}

/// 造 [`ToolRoute`] 的辅助函数，只为让上表一行一条、宽度可控。
const fn route(tool: &'static str, command: &'static str) -> ToolRoute {
    ToolRoute {
        tool,
        command: Some(command),
    }
}

/// 同上，但标记「GUI 没有这项能力」。
const fn route_only_server(tool: &'static str) -> ToolRoute {
    ToolRoute {
        tool,
        command: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn command_ids_are_unique_and_non_empty() {
        assert_eq!(COMMANDS.len(), 72, "命令目录共 72 条，数量变了要同步改这里");
        let mut seen = HashSet::new();
        for id in COMMANDS {
            assert!(!id.is_empty());
            assert!(seen.insert(*id), "命令 ID 重复：{id}");
            // 命令 ID 是点分小写；带大写或空格说明抄错了。
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_'),
                "命令 ID 含非法字符：{id}"
            );
        }
    }

    #[test]
    fn every_tool_appears_once_and_points_at_a_real_command() {
        assert_eq!(TOOL_ROUTES.len(), 34, "服务模式共 34 个工具");
        let mut seen = HashSet::new();
        for route in TOOL_ROUTES {
            assert!(route.tool.starts_with(TOOL_PREFIX), "{}", route.tool);
            assert!(seen.insert(route.tool), "工具名重复：{}", route.tool);
            if let Some(command) = route.command {
                assert!(
                    COMMANDS.contains(&command),
                    "{} 指向了不存在的命令 {command}",
                    route.tool
                );
            }
        }
    }

    #[test]
    fn mcp_name_round_trips_through_the_command_table() {
        for id in COMMANDS {
            let name = mcp_name(id);
            assert_eq!(
                command_for_mcp_name(&name),
                Some(*id),
                "{name} 应当能机械反查回 {id}"
            );
        }
        // 不在表里的名字不能瞎命中
        assert_eq!(command_for_mcp_name("aevum_nope"), None);
        assert_eq!(command_for_mcp_name("grade.set"), None);
    }

    /// 历史上唯一的两处「工具名与命令 ID 对不齐」，必须显式登记。
    /// 这两条要是哪天自己消失了，说明有人又去改名了，得回头读一遍 CHANGELOG。
    #[test]
    fn historically_mismatched_names_are_recorded() {
        assert_eq!(tool_command("aevum_xmp_read"), Some("xmp.import"));
        assert_eq!(tool_command("aevum_xmp_write"), Some("xmp.export"));
        // 机械推导出来的名字是另一套，两条路都得通
        assert_eq!(command_for_mcp_name("aevum_xmp_import"), Some("xmp.import"));
        assert_eq!(command_for_mcp_name("aevum_xmp_export"), Some("xmp.export"));
    }

    #[test]
    fn server_only_tools_are_known_but_unroutable() {
        for tool in [
            "aevum_measure",
            "aevum_deflicker_analyze",
            "aevum_keyframe_add",
            "aevum_render_preview",
            "aevum_cancel",
        ] {
            assert!(is_known_tool(tool), "{tool} 应当是已知工具");
            assert_eq!(tool_command(tool), None, "{tool} 不应当有附着路由");
        }
        assert!(!is_known_tool("aevum_nope"));
    }
}
