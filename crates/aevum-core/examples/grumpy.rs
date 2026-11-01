//! 握手拒绝样本：回一句 error，然后赖着不退。
//!
//! 宿主的握手失败路径必须杀进程收尸，不能留一个僵尸占着管道。
//! 这也是回归测试的靶子 —— 一个"正常"的插件报错后应当自己退出。
//!
//! Handshake-refusal sample: answer with an error, then refuse to exit.
//!
//! The host's handshake-failure path must kill and reap the process; leaving a zombie
//! holding the pipe is not acceptable.
//! This is a regression target too -- a "normal" plugin should exit by itself after
//! reporting an error.

use std::io::{BufRead, Write};
use std::time::Duration;

fn main() {
    // 手写协议帧而不是走 PluginSide：PluginSide::handshake 会替宿主把
    // ready 发出去，"拒绝握手"就演不成了。
    let stdin = std::io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let out = std::io::stdout();
    let mut out = out.lock();
    let _ = writeln!(out, "{{\"t\":\"error\",\"msg\":\"我拒绝\"}}");
    let _ = out.flush();
    // 赖着不退：宿主握手失败路径应当 abort（kill + wait）把它收干净。
    std::thread::sleep(Duration::from_secs(600));
}
