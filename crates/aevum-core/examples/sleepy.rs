//! 握手挂死样本：收到 hello 后装死不回。
//!
//! 宿主的握手看门狗必须在超时后杀掉它，否则 `Host::start` 会永远阻塞。
//! 这是回归测试的靶子，不是功能示例 —— 一个"正常"的插件不该学它。
//!
//! 故意不复用 [`aevum_core::plugin_host::PluginSide::handshake`]：
//! 那会正常回 ready，挂死就测不到了。

use std::io::BufRead;
use std::time::Duration;

fn main() {
    // 只把 hello 那行读走（不读的话宿主的 write 也会被管道背压卡住，
    // 场景就不纯粹了），然后什么都不做，模拟"启动即挂死"的坏插件。
    let stdin = std::io::stdin();
    let mut line = String::new();
    let _ = stdin.lock().read_line(&mut line);
    std::thread::sleep(Duration::from_secs(600));
}
