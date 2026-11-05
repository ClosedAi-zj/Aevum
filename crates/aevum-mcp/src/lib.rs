//! M6：把 Aevum Studio 变成 MCP server。
//!
//! 不另起产品：同一个 `aevum.exe` 带 `--mcp` 时进入纯 stdio 的无头模式，
//! 由 [`run_stdio`] 接管 stdin/stdout，完全绕开 eframe 和窗口初始化。
//! 带 `--attach <control.json>` 时改走 [`run_stdio_attach`]，把 `tools/list` /
//! `tools/call` 代理到正在运行的 GUI 回环通道，界面实时可见。
//!
//! 协议层手写 JSON-RPC 2.0（换行分隔的 JSON），不引 MCP 框架；工具实现全在
//! [`tools`] 里，状态放在 [`Session`]，请求串行处理。
//!
//! M6: turn Aevum Studio into an MCP server.
//!
//! No separate product: the same `aevum.exe` enters a headless, stdio-only mode
//! when given `--mcp`, and [`run_stdio`] takes over stdin/stdout, bypassing
//! eframe and window initialisation entirely. With `--attach <control.json>` it
//! goes through [`run_stdio_attach`] instead, proxying `tools/list` /
//! `tools/call` to a running GUI over the loopback control channel, so the
//! interface shows what the agent is doing in real time.
//!
//! The protocol layer is hand-written JSON-RPC 2.0 (newline-delimited JSON) with
//! no MCP framework; the tools live in [`tools`], the state lives in
//! [`Session`], and requests are handled one at a time.

mod attach;
mod server;
mod session;
mod tools;

pub use attach::{ControlTarget, attach_path, run_stdio_attach};
pub use server::run_stdio;
pub use session::Session;

use std::sync::{Mutex, OnceLock};

/// 一个可被 agent 调用的命令，也是 MCP 的 tool 定义。
///
/// One command an agent can call; also the MCP tool definition for it.
pub struct Command {
    pub id: &'static str,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
    pub hints: Hints,
}

/// MCP `annotations` 里的四个提示位。
///
/// 为什么四个都写死、不用 `Option`：规范里四个都是可选的，「没写」和「写了 false」在
/// 宿主眼里是两种状态——前者意味着「作者没表态」，后者是「作者保证不会」。只有全部
/// 显式声明，宿主才敢在调用前给用户一句确定的提示。
///
/// 本项目的四条口径（`spec.rs` 的每个条目都按这四条判）：
///
/// - `read_only`：不写任何文件、也不改当前序列的文档状态，纯查询。
/// - `destructive`：**可能覆盖或删除磁盘上已存在的用户数据**（源图、成片、工程、XMP、
///   LUT、队列文件）。只改内存里可撤销的参数**不算**——否则「保存工程」也会弹危险提示。
/// - `idempotent`：同样的参数再调一次，不会多出东西。
/// - `open_world`：**本服务器一律 false**。不联网、不访问第三方服务，作用对象只有用户
///   显式给的本地路径。将来若加联网工具，必须在这里新增预设并逐条审。
///
/// The four hint bits of MCP `annotations`.
///
/// Why all four are plain `bool` instead of `Option`: every one of them is optional in the
/// spec, and "absent" versus "present and false" mean different things to a host -- the
/// former is "the author did not say", the latter is "the author guarantees it". Only a full
/// declaration lets a host warn the user with confidence.
///
/// The four house rules (every entry in `spec.rs` is judged by them):
///
/// - `read_only`: writes no file and does not change the open sequence; a pure query.
/// - `destructive`: **may overwrite or delete user data already on disk** (sources, exports,
///   projects, XMP, LUTs, the queue file). Reversible in-memory parameter changes do **not**
///   count, otherwise "save project" would raise a danger prompt.
/// - `idempotent`: calling again with the same arguments adds nothing.
/// - `open_world`: **always false for this server**. No network, no third-party services;
///   the only object is a local path the user supplied. A future networked tool must add a
///   preset here and be reviewed one by one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hints {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

impl Hints {
    /// 纯查询：什么都不改。
    ///
    /// Pure query: changes nothing.
    pub const READ: Hints = Hints {
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
    };

    /// 改内存里的参数或会话状态，可撤销，同参数重复调用结果相同。
    ///
    /// Changes parameters or session state in memory; reversible, and the same arguments
    /// give the same result.
    pub const WRITE: Hints = Hints {
        read_only: false,
        destructive: false,
        idempotent: true,
        open_world: false,
    };

    /// 追加型写入：再调一次会多一条（笔触、队列项、关键帧）。
    ///
    /// Append-style write: calling again adds one more (strokes, queue items, keyframes).
    pub const WRITE_APPEND: Hints = Hints {
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: false,
    };

    /// 会覆盖或删除磁盘上已有的用户数据，但同参数重复调用结果收敛。
    ///
    /// May overwrite or delete user data on disk, yet repeating the same call converges.
    pub const DESTRUCTIVE: Hints = Hints {
        read_only: false,
        destructive: true,
        idempotent: true,
        open_world: false,
    };

    /// 会覆盖已有数据，且每调一次都真的再跑一遍（批量渲染、整批重跑）。
    ///
    /// Overwrites existing data and genuinely runs again on every call (batch renders,
    /// full-directory runs).
    pub const DESTRUCTIVE_APPEND: Hints = Hints {
        read_only: false,
        destructive: true,
        idempotent: false,
        open_world: false,
    };
}

/// 全部命令。第一次调用时构建，之后复用。
///
/// Every command. Built on first call, reused afterwards.
pub fn commands() -> &'static [Command] {
    tools::commands()
}

/// `tools/list` 响应里 `tools` 数组的内容，与无头模式发出去的逐字节一致。
///
/// 单独露出来是给快照判据用的：`tests/suite/tools_list_snapshot.rs` 比对的就是这条
/// 路径，不是测试里另写一遍的投影。拆分 `tools` 模块时必须保证它一个字节都不变。
///
/// The contents of the `tools` array in a `tools/list` response, byte-for-byte identical
/// to what the headless mode puts on the wire.
///
/// It is exposed for the snapshot criterion: `tests/suite/tools_list_snapshot.rs` compares
/// this path rather than a projection re-written in the test. Splitting the `tools` modules
/// must leave it byte-for-byte unchanged.
pub fn tool_list() -> Vec<serde_json::Value> {
    server::tool_list()
}

/// 执行一个命令。错误是给调用方看的文案，不 panic。
///
/// 入口先做一次工具名规范化：`lumen_*` 是旧名，静默别名成 `aevum_*`。
/// 这样无论调用方是从 `server`（无头）、`attach`（附着）还是测试直接进来，
/// 看到的都是同一套规范名。
///
/// Run one command. Errors are messages meant for the caller; it never panics.
///
/// The entry point canonicalises the tool name first: `lumen_*` is the old
/// spelling and is silently aliased to `aevum_*`. That way the caller sees one
/// set of canonical names whether it comes from `server` (headless), `attach`
/// (attached) or a test calling in directly.
pub fn dispatch(id: &str, args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let id = aevum_core::brand::canonical_tool(id);
    let mut session = SESSION
        .get_or_init(|| Mutex::new(Session::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    tools::dispatch(&mut session, id.as_ref(), args)
}

/// 拿一份任务槽的句柄，供 stdio 进度通知在线程里读原子量（不持有会话锁）。
///
/// Grab a handle to the job slot so the stdio progress notifier can read the
/// atomics from a thread without holding the session lock.
pub fn job_shared() -> std::sync::Arc<session::JobShared> {
    let session = SESSION
        .get_or_init(|| Mutex::new(Session::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::sync::Arc::clone(&session.shared)
}

/// 清空会话；集成测试和自检用。
///
/// Reset the session; used by integration tests and the self-check.
pub fn reset_session() {
    let mut session = SESSION
        .get_or_init(|| Mutex::new(Session::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *session = Session::default();
}

static SESSION: OnceLock<Mutex<Session>> = OnceLock::new();
