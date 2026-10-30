//! JSON-RPC 2.0 / MCP stdio。
//!
//! 传输就是换行分隔的 JSON：一行一条消息，stdout 只能有协议消息，日志走 stderr。
//! MCP 请求本身串行处理，不需要异步运行时。

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
pub const LATEST_PROTOCOL_VERSION: &str = "2026-07-28";

/// 本机认识的历史版本，均来自官方 specification 版本列表。
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2026-07-28"];

/// 无头模式的入口：读到 EOF 就正常退出，退出码由调用方给 0。
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
pub fn serve<R: BufRead, W: Write + Send + 'static>(
    mut input: R,
    output: W,
) -> std::io::Result<()> {
    let out = Arc::new(Mutex::new(output));
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break;
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(text) {
            Ok(value) => handle_value(value, &out),
            Err(e) => Some(error_response(
                Value::Null,
                -32700,
                &format!("JSON 解析失败：{e}"),
            )),
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
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": format!(
            "{} 延时摄影工作台。先 aevum_open_sequence 打开帧文件夹，再测光/去闪/调色/导出；批量写与覆盖已有文件需要参数 confirm: true。",
            aevum_core::brand::TITLE
        )
    })
}

fn tool_list() -> Vec<Value> {
    commands()
        .iter()
        .map(|command| {
            json!({
                "name": command.id,
                "description": command.description,
                "inputSchema": command.input_schema
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
        return error_response(id, -32602, &format!("未知工具：{name}"));
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
}
