//! v18 外壳演示（`cargo run -p aevum-ui --example shell_v18`）：
//! 从零外壳 [`aevum_ui::shell2::ShellV18`] + 真 `AppState::demo()` 数据 +
//! [`aevum_ui::theme_modern::apply_modern_dark`] 主题。
//!
//! v18 shell demo: the from-scratch shell driving a real demo AppState under
//! the final theme. Old shell untouched.

use aevum_ui::shell2::ShellV18;
use aevum_ui::state::AppState;
use aevum_ui::theme_modern::apply_modern_dark;

struct App {
    state: AppState,
    shell: ShellV18,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.shell.ui(&mut self.state, ui);
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("恒晷 v18 · 新外壳预览")
            .with_inner_size([1600.0, 1000.0])
            .with_position([60.0, 40.0]),
        ..Default::default()
    };
    eframe::run_native(
        "恒晷 v18 · 新外壳预览",
        opts,
        Box::new(|cc| {
            apply_modern_dark(&cc.egui_ctx);
            Ok(Box::new(App {
                state: AppState::demo(),
                shell: ShellV18::new(),
            }))
        }),
    )
}
