//! v18 PoC：验证「Rust 渲染帧 → Tauri IPC → webview canvas」的流式传输帧率。
//!
//! 判据：640×360 RGBA 帧经 Channel 推送，前端 putImageData + FPS 计数，
//! ≥24 fps 即判定画布流式可行（mockup.html 可升级为真 UI）。
//!
//! v18 PoC: validates the frame-streaming pipeline — Rust renders 640×360 RGBA
//! frames, pushes them over a Tauri Channel; the webview draws ImageData and
//! counts FPS. Pass ≥24 fps and the web-UI route is viable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::ipc::Channel;
use tauri::State;

/// 共享渲染参数（前端经 command 修改，渲染线程逐帧读取）。
///
/// Shared render params: mutated from the frontend via commands, read per frame.
#[derive(Clone)]
struct PocState {
    params: Arc<Mutex<Params>>,
    frame_counter: Arc<AtomicU64>,
}

#[derive(Clone, Copy)]
struct Params {
    exposure: f32,
    contrast: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self { exposure: 0.0, contrast: 0.0 }
    }
}

/// 渲染一帧 640×360 RGBA：teal-orange 电影调色的程序化日落，
/// 曝光/对比度参数真实作用于像素（证明命令桥端到端生效）。
///
/// Renders one 640×360 RGBA frame: procedural teal-orange sunset with the
/// exposure/contrast params actually applied to pixels (proves the command
/// bridge end to end).
fn render_frame(params: Params, t_secs: f32) -> Vec<u8> {
    let w = 640usize;
    let h = 360usize;
    let mut buf = vec![0u8; w * h * 4];
    let ev = 2f32.powf(params.exposure);
    let contrast = 1.0 + params.contrast / 100.0;
    for y in 0..h {
        let ty = y as f32 / h as f32;
        let (r, g, b) = if ty < 0.42 {
            let k = ty / 0.42;
            (
                20.0 + (58.0 - 20.0) * k,
                36.0 + (92.0 - 36.0) * k,
                52.0 + (128.0 - 52.0) * k,
            )
        } else if ty < 0.62 {
            let k = (ty - 0.42) / 0.20;
            (
                58.0 + (176.0 - 58.0) * k,
                92.0 + (128.0 - 92.0) * k,
                128.0 + (110.0 - 128.0) * k,
            )
        } else {
            let k = (ty - 0.62) / 0.38;
            (
                176.0 + (255.0 - 176.0) * k,
                128.0 + (214.0 - 128.0) * k,
                110.0 + (168.0 - 110.0) * k,
            )
        };
        for x in 0..w {
            let tx = x as f32 / w as f32;
            let sun_x = 0.5 + 0.02 * (t_secs * 0.8).sin();
            let dx = (tx - sun_x) * (w as f32 / h as f32);
            let dy = (ty - 0.58) * 3.0;
            let d = (dx * dx + dy * dy).sqrt();
            let (mut r, mut g, mut b) = (r, g, b);
            if d < 0.10 {
                let k = (1.0 - d / 0.10).clamp(0.0, 1.0);
                r = r + (255.0 - r) * k;
                g = g + (240.0 - g) * k;
                b = b + (200.0 - b) * k;
            } else if d < 0.30 {
                let k = (1.0 - (d - 0.10) / 0.20) * 0.5;
                r = r + (255.0 - r) * k;
                g = g + (230.0 - g) * k;
                b = b + (190.0 - b) * k;
            }
            let ridge = 0.84 + 0.03 * (tx * 9.0 + t_secs * 0.05).sin();
            if ty > ridge {
                r = 14.0;
                g = 26.0;
                b = 38.0;
            }
            r = ((r / 255.0 * ev - 0.5) * contrast + 0.5).clamp(0.0, 1.0) * 255.0;
            g = ((g / 255.0 * ev - 0.5) * contrast + 0.5).clamp(0.0, 1.0) * 255.0;
            b = ((b / 255.0 * ev - 0.5) * contrast + 0.5).clamp(0.0, 1.0) * 255.0;
            let i = (y * w + x) * 4;
            buf[i] = r as u8;
            buf[i + 1] = g as u8;
            buf[i + 2] = b as u8;
            buf[i + 3] = 255;
        }
    }
    buf
}

#[tauri::command]
fn start_stream(on_frame: Channel<Vec<u8>>, state: State<'_, PocState>) -> Result<(), String> {
    let params = state.params.clone();
    let counter = state.frame_counter.clone();
    std::thread::spawn(move || {
        let mut t = 0.0f32;
        loop {
            let p = params.lock().unwrap().clone();
            let frame = render_frame(p, t);
            t += 1.0 / 30.0;
            if on_frame.send(frame).is_err() {
                break; // webview 已关闭
            }
            counter.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    });
    Ok(())
}

#[tauri::command]
fn set_params(exposure: f32, contrast: f32, state: State<'_, PocState>) {
    let mut p = state.params.lock().unwrap();
    p.exposure = exposure;
    p.contrast = contrast;
}

#[tauri::command]
fn fps_report(fps: f32, state: State<'_, PocState>) -> String {
    let frames = state.frame_counter.load(Ordering::Relaxed);
    format!("webview FPS {fps:.1} · Rust 已渲 {frames} 帧")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WS 流式线程：localhost:8932 二进制推帧（绕开 IPC JSON 序列化）。
    let params = Arc::new(Mutex::new(Params::default()));
    let params_ws = params.clone();
    std::thread::spawn(move || {
        let listener = std::net::TcpListener::bind("127.0.0.1:8932").unwrap();
        for stream in listener.incoming() {
            let Ok(sock) = stream else { continue };
            let params = params_ws.clone();
            std::thread::spawn(move || {
                let mut ws = tungstenite::accept(sock).unwrap();
                let mut t = 0.0f32;
                loop {
                    let p = params.lock().unwrap().clone();
                    let frame = render_frame(p, t);
                    t += 1.0 / 30.0;
                    use tungstenite::Message;
                    if ws.send(Message::Binary(frame.into())).is_err() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(33));
                }
            });
        }
    });

    tauri::Builder::default()
        .manage(PocState {
            params: params.clone(),
            frame_counter: Arc::new(AtomicU64::new(0)),
        })
        .invoke_handler(tauri::generate_handler![start_stream, set_params, fps_report])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
