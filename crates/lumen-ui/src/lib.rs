//! 光时 · Lumen Studio 界面层：外壳、状态、主题、图标、撤销历史与 eframe 入口。
//!
//! 与算法层分开：这里只做绘制和交互，所有数值口径都在 `lumen-core`。

pub mod app;
#[cfg(test)]
mod bench;
#[cfg(test)]
mod cache_tests;
pub mod control;
pub mod controls;
pub mod demo;
pub mod history;
pub mod icons;
pub mod layout;
pub mod platform;
pub mod settings;
pub mod shell;
pub mod state;
pub mod theme;
pub mod widgets;

pub use app::{LumenApp, Startup};
pub use control::ControlServer;
pub use state::AppState;

/// 无显示器自检：跑两遍布局，确认加载态与空状态都真的画出了东西。
pub fn selftest() -> Result<String, String> {
    let ctx = egui::Context::default();
    theme::apply(&ctx, 1.0);

    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    let mut loaded = 0;
    for _ in 0..2 {
        let mut out = ctx.run_ui(input(), |ui| shell::ui(&mut state, ui));
        loaded = out.shapes.len();
        out.textures_delta.clear();
    }
    if loaded < 200 {
        return Err(format!("加载态只画出 {loaded} 个形状"));
    }

    let mut per_module = Vec::new();
    for module in state::Module::ALL {
        state.module = module;
        let mut shapes = 0;
        for _ in 0..2 {
            let mut out = ctx.run_ui(input(), |ui| shell::ui(&mut state, ui));
            shapes = out.shapes.len();
            out.textures_delta.clear();
        }
        if shapes < 80 {
            return Err(format!("{} 模块只画出 {shapes} 个形状", module.cn()));
        }
        per_module.push(format!("{} {shapes}", module.cn()));
    }

    let ctx2 = egui::Context::default();
    theme::apply(&ctx2, 1.0);
    let mut empty = AppState::new();
    let mut empty_shapes = 0;
    for _ in 0..2 {
        let mut out = ctx2.run_ui(input(), |ui| shell::ui(&mut empty, ui));
        empty_shapes = out.shapes.len();
        out.textures_delta.clear();
    }
    if empty_shapes < 20 {
        return Err(format!("空状态只画出 {empty_shapes} 个形状"));
    }

    Ok(format!(
        "加载态 {loaded} 形状 · 空状态 {empty_shapes} 形状 · 模块 {}",
        per_module.join(" / ")
    ))
}

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        max_texture_side: Some(8192),
        ..Default::default()
    }
}
