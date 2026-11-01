//! 刷屏样本：握手后狂吐不带换行的字节流。
//!
//! 协议帧是"一行 JSON"，`read_line` 会一直吃到换行 —— 没有长度上限的话，
//! 这种坏插件能把宿主吃成 OOM。宿主读到 [`MAX_LINE`](aevum_core::plugin_host::MAX_LINE)
//! 必须立刻报错断开。回归测试的靶子，不是功能示例。
//!
//! Flood sample: after the handshake, spew a byte stream with no newlines.
//!
//! A protocol frame is "one line of JSON" and `read_line` keeps reading until it sees a
//! newline -- with no length cap, a broken plugin like this can eat the host into an OOM.
//! On reaching [`MAX_LINE`](aevum_core::plugin_host::MAX_LINE) the host must fail
//! immediately and disconnect. A regression target, not a feature example.

fn main() {
    use std::io::Write;
    let out = std::io::stdout();
    let mut out = out.lock();
    let chunk = [b'x'; 8192];
    loop {
        // 宿主断开（看门狗/限长）后管道写失败，自然退出。
        if out.write_all(&chunk).is_err() {
            return;
        }
        if out.flush().is_err() {
            return;
        }
    }
}
