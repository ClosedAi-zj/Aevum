//! 恒晷 · Aevum Studio 界面层：外壳、状态、主题、图标、撤销历史与 eframe 入口。
//!
//! 与算法层分开：这里只做绘制和交互，所有数值口径都在 `aevum-core`。
//!
//! Aevum Studio UI layer: shell, state, theme, icons, undo history and the eframe
//! entry point.
//!
//! Kept apart from the algorithm layer: this crate only draws and handles input;
//! every numeric definition lives in `aevum-core`.

pub mod app;
#[cfg(test)]
mod bench;
pub mod bench_ui;
#[cfg(test)]
mod cache_tests;
pub mod control;
pub mod controls;
pub mod demo;
pub mod history;
pub mod i18n;
mod i18n_en;
pub mod icons;
pub mod layout;
pub mod platform;
pub mod preview_bench;
pub mod settings;
pub mod shell;
pub mod state;
pub mod theme;
pub mod theme_modern;
pub mod widgets;
pub mod win_parent;

pub use app::{AevumApp, Startup};
pub use bench_ui::{BenchRow, bench_ui, bench_ui_rows};
pub use control::ControlServer;
pub use preview_bench::{
    PreviewRow, preview_checksum, preview_first_screen, preview_first_screen_rows,
};
pub use state::AppState;

/// 无显示器自检：跑两遍布局，确认加载态与空状态都真的画出了东西。
///
/// Display-less selftest: run the layout twice and confirm both the loaded and the
/// empty state really paint something.
pub fn selftest() -> Result<String, String> {
    // 无头入口也解析一次配置路径：首启迁移与 override 诊断都挂在这条链上
    // （config_dir 的文档承诺 selftest 覆盖迁移），顺带让 AEVUM_* 指向
    // 坏路径时在 stderr 上看得见——自检本来就是干这个的。
    let _ = crate::settings::path();
    // 自检**固定中文**，不读用户的 settings.json：形状计数是回归基线，
    // 同一个提交在「用户设了英文」的机器上数出另一组数字，门槛就成了看心情。
    // 语言切换本身由 `--selftest` 之外的道路覆盖（见 i18n 的单元测试）。
    crate::i18n::set(crate::i18n::Lang::ZH);
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
        // 两种折叠态都要画：**默认态**是用户真正看见的第一屏（折叠后的排版也是
        // 界面的一部分，标题条压没压线、首屏还剩几段，都靠它回归）；**全展开态**
        // 保住展开后的布局覆盖——默认折叠了低频组，只画默认态就等于少测半屏。
        state.collapsed = state::default_collapsed();
        let closed = paint(&ctx, &mut state);
        state.collapsed.clear();
        let opened = paint(&ctx, &mut state);
        for (label, shapes) in [("默认折叠", closed), ("全展开", opened)] {
            if shapes < 80 {
                return Err(format!(
                    "{} 模块（{label}）只画出 {shapes} 个形状",
                    module.cn()
                ));
            }
        }
        per_module.push(format!("{} {closed}/{}", module.cn(), opened));
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

/// 连跑两帧取稳态形状数（首帧要吃字体与纹理的冷启动）。
fn paint(ctx: &egui::Context, state: &mut AppState) -> usize {
    let mut shapes = 0;
    for _ in 0..2 {
        let mut out = ctx.run_ui(input(), |ui| shell::ui(state, ui));
        shapes = out.shapes.len();
        out.textures_delta.clear();
    }
    shapes
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
