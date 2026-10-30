//! 独立的小入口，方便不启动 GUI 时直接跑协议；正式用法仍是 `aevum-studio --mcp`。

fn main() -> std::process::ExitCode {
    aevum_mcp::run_stdio()
}
