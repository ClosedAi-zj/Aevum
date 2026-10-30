//! 演示插件：把一个 tile 的每个通道反相，按 `strength` 线性混回原图。
//!
//! 它的用处不是"反相"这个滤镜，而是**当模板**：真正的插件（Nik、Camera Raw）
//! 与它的区别只在中间那几行像素运算，协议、参数、退出方式全都一样。
//!
//! 跑起来看一眼：
//! ```bash
//! cargo build --examples
//! cargo test -p aevum-core plugin_host
//! ```
//!
//! 手动调也行（协议是纯文本行 + 裸字节，可以直接喂）：
//! ```bash
//! printf '{"t":"hello","protocol":1,"id":"x","params":{"strength":100}}\n' \
//!   | target/debug/examples/invert.exe
//! ```

use aevum_core::plugin_host::PluginSide;

fn main() {
    let mut side = match PluginSide::handshake() {
        Ok(s) => s,
        Err(e) => {
            // 握手失败要退出码非零：宿主据此判断"这个插件起不来"，
            // 而不是"它跑完了但没输出"。
            eprintln!("反相插件：握手失败：{e}");
            std::process::exit(1);
        }
    };

    // 参数是 0–100 的百分比，缺省满强度。夹一次 —— 宿主传什么都不能信。
    let strength = (side
        .params
        .get("strength")
        .copied()
        .unwrap_or(100.0)
        .clamp(0.0, 100.0))
        / 100.0;

    loop {
        match side.recv_tile() {
            Ok(Some((seq, x, y, w, h, mut px))) => {
                for p in px.iter_mut() {
                    let v = f64::from(*p);
                    let target = 255.0 - v;
                    // 在"原值"与"反相值"之间插值。写成 `v + (255-v)*s` 是个经典陷阱：
                    // s=1 时它恒等于 255，整张图会变成一片白。
                    *p = (v + (target - v) * strength).round() as u8;
                }
                if let Err(e) = side.send_tile(seq, x, y, w, h, &px) {
                    eprintln!("反相插件：回不了像素：{e}");
                    std::process::exit(1);
                }
            }
            // 宿主说 end / cancel，或者管道关了。
            Ok(None) => break,
            Err(e) => {
                eprintln!("反相插件：{e}");
                std::process::exit(1);
            }
        }
    }
}
