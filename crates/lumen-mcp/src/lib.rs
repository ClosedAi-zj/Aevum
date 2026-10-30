//! M6：把 Lumen Studio 变成 MCP server。
//!
//! 不另起产品：同一个 `lumen.exe` 带 `--mcp` 时进入纯 stdio 的无头模式，
//! 由 [`run_stdio`] 接管 stdin/stdout，完全绕开 eframe 和窗口初始化。
//! 带 `--attach <control.json>` 时改走 [`run_stdio_attach`]，把 `tools/list` /
//! `tools/call` 代理到正在运行的 GUI 回环通道，界面实时可见。
//!
//! 协议层手写 JSON-RPC 2.0（换行分隔的 JSON），不引 MCP 框架；工具实现全在
//! [`tools`] 里，状态放在 [`Session`]，请求串行处理。

mod attach;
mod server;
mod session;
mod tools;

pub use attach::{ControlTarget, attach_path, run_stdio_attach};
pub use server::run_stdio;
pub use session::Session;

use std::sync::{Mutex, OnceLock};

/// 一个可被 agent 调用的命令，也是 MCP 的 tool 定义。
pub struct Command {
    pub id: &'static str,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
}

/// 全部命令。第一次调用时构建，之后复用。
pub fn commands() -> &'static [Command] {
    tools::commands()
}

/// 执行一个命令。错误是给调用方看的文案，不 panic。
pub fn dispatch(id: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let mut session = SESSION
        .get_or_init(|| Mutex::new(Session::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    tools::dispatch(&mut session, id, args)
}

/// 拿一份任务槽的句柄，供 stdio 进度通知在线程里读原子量（不持有会话锁）。
pub fn job_shared() -> std::sync::Arc<session::JobShared> {
    let session = SESSION
        .get_or_init(|| Mutex::new(Session::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::sync::Arc::clone(&session.shared)
}

/// 清空会话；集成测试和自检用。
pub fn reset_session() {
    let mut session = SESSION
        .get_or_init(|| Mutex::new(Session::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *session = Session::default();
}

static SESSION: OnceLock<Mutex<Session>> = OnceLock::new();
