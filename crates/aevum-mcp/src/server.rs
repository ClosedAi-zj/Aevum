//! JSON-RPC 2.0 / MCP stdio。
//!
//! 传输就是换行分隔的 JSON：一行一条消息，stdout 只能有协议消息，日志走 stderr。
//! MCP 请求本身串行处理，不需要异步运行时。
//!
//! JSON-RPC 2.0 / MCP over stdio.
//!
//! The transport is newline-delimited JSON: one message per line, stdout carries
//! protocol messages only, and logs go to stderr. MCP requests are handled one at
//! a time, so no async runtime is needed.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::session::{JobKind, JobShared, STAGE_ENCODE};
use crate::{commands, dispatch};

/// 本机支持的最新协议版本。
///
/// 依据官方规范页 `https://modelcontextprotocol.io/specification/latest`
/// （2026-10 时显示 revision 2026-07-28）。initialize 时若客户端带了本机也
/// 认识的历史版本就原样回显，避免让老客户端降级失败；不认识才回最新版。
///
/// The newest protocol revision this build speaks.
///
/// Based on the official specification page
/// `https://modelcontextprotocol.io/specification/latest` (which showed revision
/// 2026-07-28 in 2026-10). During `initialize`, a historical version we also know
/// is echoed back verbatim so older clients do not fail to downgrade; only an
/// unknown one falls back to the latest.
pub const LATEST_PROTOCOL_VERSION: &str = "2026-07-28";

/// 本机认识的历史版本，均来自官方 specification 版本列表。
///
/// The historical revisions we recognise, all taken from the official
/// specification version list.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2026-07-28"];

/// 无头模式的入口：读到 EOF 就正常退出，退出码由调用方给 0。
///
/// Entry point of headless mode: return normally on EOF; the caller supplies exit
/// code 0.
pub fn run_stdio() -> std::process::ExitCode {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if let Err(e) = serve(stdin.lock(), stdout) {
        // 管道断了通常是宿主关掉了我们，不算错误退出。
        eprintln!("aevum-mcp: stdio 结束：{e}");
    }
    std::process::ExitCode::SUCCESS
}

/// 逐行读请求、逐行写响应。抽出来是为了能脱离进程做协议单测。
///
/// 写端放在 `Arc<Mutex<_>>` 里：进度通知由旁路线程在长任务运行期间直接写 stdout，
/// 必须和响应共用同一把锁，否则两条消息会交错。
///
/// Read requests line by line and write responses line by line. Extracted so the
/// protocol can be unit-tested without a process.
///
/// The writer lives behind `Arc<Mutex<_>>`: during a long job the progress thread
/// writes straight to stdout, and it must share that one lock with the responses,
/// otherwise two messages interleave.
/// 单行 JSON 消息的字节上限，防止一行几 GB 的输入把内存吃爆。
///
/// Byte limit for a single JSON message line, so a multi-gigabyte line cannot
/// blow up memory.
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// 一行输入的分类结果：空行、已解析的请求、或要回写的一条协议错误。
///
/// 抽成纯函数（输入只是字节切片）是为了让非法 UTF-8 和超长行都能脱离进程做单测。
///
/// Classification of one input line: empty, a parsed request, or a protocol
/// error response to write back. Extracted as a pure function over bytes so
/// invalid UTF-8 and oversized lines can be unit-tested without a process.
enum LineOutcome {
    Empty,
    Parse(Value),
    Error(Value),
}

fn classify_line(buf: &[u8]) -> LineOutcome {
    // 超长行直接丢弃并回 parse error：read_until 已经把它整个读进来了，
    // 所以这里丢弃即等于跳过该行，服务循环继续。
    //
    // Oversized lines are dropped with a parse error: `read_until` has already
    // pulled the whole line in, so dropping here is the same as skipping it and
    // the serve loop continues.
    if buf.len() > MAX_LINE_BYTES {
        return LineOutcome::Error(error_response(
            Value::Null,
            -32700,
            &format!("单行消息超过 {} 字节上限", MAX_LINE_BYTES),
        ));
    }
    // stdin 出现非法 UTF-8 时 `read_line` 会返回 Err(InvalidData) 并杀死整个
    // 服务器；改用有损解码把坏字节交给 JSON 解析器自然报 -32700，服务循环
    // 永不因编码中断。
    //
    // Invalid UTF-8 on stdin used to surface as Err(InvalidData) from
    // `read_line` and kill the whole server; lossy decoding hands the bad
    // bytes to the JSON parser which naturally answers -32700, so the serve
    // loop is never interrupted by encoding.
    let text = String::from_utf8_lossy(buf);
    let text = text.trim();
    if text.is_empty() {
        return LineOutcome::Empty;
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value) => LineOutcome::Parse(value),
        Err(e) => LineOutcome::Error(error_response(
            Value::Null,
            -32700,
            &format!("JSON 解析失败：{e}"),
        )),
    }
}

pub fn serve<R: BufRead, W: Write + Send + 'static>(
    mut input: R,
    output: W,
) -> std::io::Result<()> {
    let out = Arc::new(Mutex::new(output));
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if input.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        let response = match classify_line(&buf) {
            LineOutcome::Empty => None,
            LineOutcome::Error(response) => Some(response),
            LineOutcome::Parse(value) => handle_value(value, &out),
        };
        if let Some(response) = response {
            write_line(&out, &response)?;
        }
    }
    Ok(())
}

fn write_line<W: Write>(out: &Arc<Mutex<W>>, value: &Value) -> std::io::Result<()> {
    let mut writer = out.lock().unwrap_or_else(|e| e.into_inner());
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn handle_value<W: Write + Send + 'static>(value: Value, out: &Arc<Mutex<W>>) -> Option<Value> {
    match value {
        // JSON-RPC 批量请求；MCP 不要求，但实现成本低，方便手测。
        Value::Array(items) => {
            let responses: Vec<Value> = items
                .into_iter()
                .filter_map(|item| handle_message(item, out))
                .collect();
            (!responses.is_empty()).then_some(Value::Array(responses))
        }
        other => handle_message(other, out),
    }
}

fn handle_message<W: Write + Send + 'static>(message: Value, out: &Arc<Mutex<W>>) -> Option<Value> {
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return id.map(|id| error_response(id, -32600, "缺少 method"));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    let Some(id) = id else {
        // 通知没有 id，也不需要响应。initialized 之后我们本来也不维护握手态。
        return None;
    };

    let response = match method {
        "initialize" => ok_response(id, initialize(&params)),
        "ping" => ok_response(id, json!({})),
        "tools/list" => ok_response(id, json!({"tools": tool_list()})),
        "tools/call" => call_tool(id, &params, out),
        _ => error_response(id, -32601, &format!("未知方法：{method}")),
    };
    Some(response)
}

fn initialize(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = match requested {
        Some(version) if SUPPORTED_PROTOCOL_VERSIONS.contains(&version) => version,
        _ => LATEST_PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {
            "name": "aevum-studio",
            "version": aevum_core::brand::version()
        },
        "instructions": format!(
            "{} 延时摄影工作台。先 aevum_open_sequence 打开帧文件夹，再测光/去闪/调色/导出；批量写与覆盖已有文件需要参数 confirm: true。",
            aevum_core::brand::TITLE
        )
    })
}

/// `tools/list` 响应里 `tools` 数组的**唯一**构造点。
///
/// `crate::tool_list` 直接把它转出去给快照判据用，所以测试冻结的就是线上真正发出去
/// 的那份字节，而不是测试里另写一遍的投影。
///
/// 四个 `annotations` 位**总是**全部输出（即使取值是 `false`）：MCP 规范把它们定义成
/// 可选，而「没写」与「写了 false」在宿主眼里是两种意思——前者代表作者没表态，后者是
/// 作者保证不会。少了这一层，宿主就没法在调用前给用户一句确定的提示。
///
/// The **only** place that builds the `tools` array of a `tools/list` response.
///
/// `crate::tool_list` hands it straight to the snapshot criterion, so what the test freezes
/// is the exact bytes that go out on the wire, not a projection re-written in the test.
///
/// All four `annotations` bits are **always** emitted, even when `false`: the MCP spec makes
/// them optional, and to a host "absent" and "present but false" are different claims -- the
/// former is "the author did not say", the latter is "the author guarantees it". Without
/// that, a host cannot warn the user with confidence before a call.
pub(crate) fn tool_list() -> Vec<Value> {
    commands()
        .iter()
        .map(|command| {
            json!({
                "name": command.id,
                "description": command.description,
                "inputSchema": command.input_schema,
                "annotations": {
                    "readOnlyHint": command.hints.read_only,
                    "destructiveHint": command.hints.destructive,
                    "idempotentHint": command.hints.idempotent,
                    "openWorldHint": command.hints.open_world,
                }
            })
        })
        .collect()
}

fn call_tool<W: Write + Send + 'static>(id: Value, params: &Value, out: &Arc<Mutex<W>>) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error_response(id, -32602, "tools/call 缺少 name");
    };
    // 旧工具名（`lumen_*`）静默别名成规范名。放在**存在性校验之前**，
    // 于是旧名走的是与新名完全相同的一条路，响应体因此逐字节相同。
    let name = aevum_core::brand::canonical_tool(name);
    if !commands().iter().any(|command| command.id == name.as_ref()) {
        return error_response(
            id,
            -32602,
            &format!("未知工具：{}", crate::tools::echo_text(&name)),
        );
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return error_response(id, -32602, "arguments 必须是对象");
    }

    // MCP 规定进度令牌放在 params._meta.progressToken；带了才发通知。
    let token = params
        .get("_meta")
        .and_then(|meta| meta.get("progressToken"))
        .cloned();
    let monitor =
        token.map(|token| ProgressMonitor::start(token, out.clone(), crate::job_shared()));
    let result = dispatch(name.as_ref(), &arguments);
    if let Some(monitor) = monitor {
        monitor.finish();
    }

    match result {
        Ok(value) => {
            let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
            ok_response(
                id,
                json!({
                    "content": [{"type": "text", "text": text}],
                    "isError": false
                }),
            )
        }
        // 工具执行失败是业务结果，不是协议错误：进程继续服务后续请求。
        Err(message) => ok_response(
            id,
            json!({
                "content": [{"type": "text", "text": message}],
                "isError": true
            }),
        ),
    }
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

/// 长任务的进度旁路：轮询 [`JobShared`]，把变化写成 `notifications/progress`。
///
/// 同步任务会阻塞 stdio 主循环，所以通知必须由这个线程直接写 stdout；
/// 后台任务在响应返回后继续跑，通知也跟着跑到任务结束。
struct ProgressMonitor {
    stop: Arc<AtomicBool>,
    shared: Arc<JobShared>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ProgressMonitor {
    fn start<W: Write + Send + 'static>(
        token: Value,
        out: Arc<Mutex<W>>,
        shared: Arc<JobShared>,
    ) -> Self {
        // 序号必须在 dispatch 之前取：任务可能比旁路线程先启动。
        let serial0 = shared.serial();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let worker = Arc::clone(&shared);
        let handle = thread::spawn(move || monitor_loop(token, out, worker, serial0, flag));
        Self {
            stop,
            shared,
            handle: Some(handle),
        }
    }

    /// 调用返回后收尾：同步任务是等旁路线程写完最后一条再回包；
    /// 后台任务还在跑就让它继续发通知，不阻塞响应。
    fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if self.shared.is_running() {
            return;
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn monitor_loop<W: Write>(
    token: Value,
    out: Arc<Mutex<W>>,
    shared: Arc<JobShared>,
    serial0: u64,
    stop: Arc<AtomicBool>,
) {
    let mut last_percent = -1i64;
    let mut last_message = String::new();
    loop {
        let running = shared.is_running();
        let started = shared.serial() > serial0;
        if started {
            let percent =
                progress_percent(shared.kind(), shared.stage(), shared.done(), shared.total())
                    .max(last_percent);
            let message =
                progress_message(shared.kind(), shared.stage(), shared.done(), shared.total());
            if percent > last_percent || (percent >= 0 && message != last_message) {
                let note = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {
                        "progressToken": token,
                        "progress": percent,
                        "total": 100,
                        "message": message,
                    }
                });
                if write_line(&out, &note).is_err() {
                    return;
                }
                last_percent = percent;
                last_message = message;
            }
        }
        // 调用已返回且任务也停了：这条路径覆盖"任务在第一次轮询前就跑完"。
        if stop.load(Ordering::SeqCst) && !running {
            return;
        }
        thread::sleep(Duration::from_millis(120));
    }
}

/// 把各阶段折算成 0..100：导出写帧占前一半、编码占后一半；队列按已完成段数。
fn progress_percent(kind: Option<JobKind>, stage: u8, done: usize, total: usize) -> i64 {
    let frac = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    };
    let value = match kind {
        Some(JobKind::Export) => {
            if stage >= STAGE_ENCODE {
                0.5 + 0.5 * frac
            } else {
                0.5 * frac
            }
        }
        _ => frac,
    };
    (value * 100.0).round() as i64
}

fn progress_message(kind: Option<JobKind>, stage: u8, done: usize, total: usize) -> String {
    match kind {
        Some(JobKind::Export) => {
            if stage >= STAGE_ENCODE {
                format!("导出 · 编码 {done}/{total}")
            } else {
                format!("导出 · 写帧 {done}/{total}")
            }
        }
        Some(JobKind::Queue) => format!("队列 {done}/{total} 段"),
        Some(JobKind::Align) => format!("对齐 {done}/{total} 帧"),
        Some(JobKind::Measure) => format!("测光 {done}/{total} 帧"),
        None => "完成".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_version_falls_back_to_latest() {
        let result = initialize(&json!({"protocolVersion": "9999-01-01"}));
        assert_eq!(result["protocolVersion"], LATEST_PROTOCOL_VERSION);
    }

    #[test]
    fn known_version_is_echoed() {
        let result = initialize(&json!({"protocolVersion": "2025-06-18"}));
        assert_eq!(result["protocolVersion"], "2025-06-18");
    }

    /// 测试用落点：把响应收进共享缓冲，满足 serve 对 `W: 'static` 的要求。
    ///
    /// Test sink: collects responses into a shared buffer to satisfy serve's `W: 'static`
    /// bound.
    #[derive(Clone)]
    struct SharedSink(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn drain(sink: &SharedSink) -> String {
        String::from_utf8(sink.0.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .expect("输出应为 UTF-8")
    }

    /// 喂非法 UTF-8 字节序列：服务器必须回 -32700 并继续服务下一条合法请求。
    ///
    /// Feed an invalid UTF-8 byte sequence: the server must answer -32700 and
    /// keep serving the next valid request.
    #[test]
    fn invalid_utf8_line_answers_parse_error_and_keeps_serving() {
        let mut input: Vec<u8> = vec![b'{', 0xff, 0xfe, b'\n'];
        input.extend_from_slice(b"{\"method\":\"ping\",\"id\":1}\n");
        let sink = SharedSink(Arc::new(Mutex::new(Vec::new())));
        serve(&mut input.as_slice(), sink.clone()).expect("serve 不应因编码错误退出");
        let text = drain(&sink);
        let mut lines = text.lines();
        let first: Value = serde_json::from_str(lines.next().expect("应有第一条响应"))
            .expect("第一条响应应为合法 JSON");
        assert_eq!(first["error"]["code"], -32700);
        let second: Value = serde_json::from_str(lines.next().expect("应有第二条响应"))
            .expect("第二条响应应为合法 JSON");
        assert_eq!(second["id"], 1);
        assert_eq!(second["result"], json!({}));
    }

    /// 喂一条超长行：必须回 -32700 而不是 OOM/中断，且后续请求照常处理。
    ///
    /// Feed an oversized line: the server must answer -32700 instead of OOM or
    /// aborting, and keep handling later requests.
    #[test]
    fn oversized_line_answers_parse_error_and_keeps_serving() {
        let oversized = vec![b'a'; MAX_LINE_BYTES + 1];
        let mut input: Vec<u8> = oversized;
        input.push(b'\n');
        input.extend_from_slice(b"{\"method\":\"ping\",\"id\":7}\n");
        let sink = SharedSink(Arc::new(Mutex::new(Vec::new())));
        serve(&mut input.as_slice(), sink.clone()).expect("serve 不应因超长行退出");
        let text = drain(&sink);
        let mut lines = text.lines();
        let first: Value = serde_json::from_str(lines.next().expect("应有第一条响应"))
            .expect("第一条响应应为合法 JSON");
        assert_eq!(first["error"]["code"], -32700);
        let second: Value = serde_json::from_str(lines.next().expect("应有第二条响应"))
            .expect("第二条响应应为合法 JSON");
        assert_eq!(second["id"], 7);
    }
}
