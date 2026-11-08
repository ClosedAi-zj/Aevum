//! 左侧工作流面板：把「当前这一步」的 3–5 个写操作摆到手边。
//!
//! 面板只读 `flow::step_states` 派生出来的当前步，不另存进度；每个按钮都绑到
//! 既有入口（导入对话框、模块切换、dock 页签、导出确认），点下去一定有反应，
//! 需要素材的动作在无序列时明确置灰并说明原因，不做静默失败。
//!
//! Left workflow panel: the current step's 3-5 write actions within reach.
//! It reads the current step derived by `flow::step_states` and keeps no progress of its
//! own; every button binds to an existing entry point (import dialog, module switch,
//! dock tab, export confirmation). Actions that need footage are visibly disabled with a
//! reason when no sequence is loaded -- no silent no-ops.

use egui::{Context, RichText, Ui, Vec2};

use super::flow::{self, StepAction};
use crate::state::AppState;
use crate::theme::*;

/// 该不该摆这块面板。窄窗强制收起；没有序列时**不**收起，面板会给「导入照片…」。
///
/// Whether the panel should show. A narrow window forces it closed; an empty project does
/// **not**, because the panel then offers "Import photos...".
pub fn flow_panel_visible(state: &AppState, root_w: f32) -> bool {
    state.show_flow && root_w >= FLOW_MIN_W
}

/// 一颗动作按钮是否需要已导入的帧。
///
/// Whether an action needs imported frames.
fn needs_frames(a: StepAction) -> bool {
    matches!(
        a,
        StepAction::Rescan
            | StepAction::Auto
            | StepAction::Reset
            | StepAction::ExportConfirm
            | StepAction::Dock(_)
    )
}

/// 动作分发：只接既有入口，不为面板新造旁路。
///
/// Action dispatch: existing entry points only, no side channel for this panel.
fn run_action(state: &mut AppState, ctx: &Context, a: StepAction) {
    match a {
        StepAction::Import => super::dialogs::open_photos_dialog(state, ctx),
        StepAction::Rescan => super::dialogs::rescan(state, ctx),
        StepAction::Goto(m) => state.set_module(m),
        StepAction::Dock(t) => {
            state.show_dock = true;
            state.dock_tab = t;
        }
        StepAction::Presets => state.show_left = true,
        StepAction::Auto => state.auto_exposure(ctx),
        StepAction::Reset => state.reset_params(),
        StepAction::Look => state.look_open = true,
        StepAction::ExportConfirm => ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("aevum-export-pending"), 1u8);
        }),
    }
}

pub(super) fn flow_panel(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let task = state.flow_task;
    let steps = task.steps();
    let current = flow::step_states(state, task)
        .iter()
        .position(|s| *s == flow::StepState::Current)
        .unwrap_or(steps.len() - 1);
    let step = &steps[current];
    let has_frames = state.frame_count() > 0;

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::tr("工作流"))
                .size(SIZE_LABEL)
                .color(muted()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let close = ui.small_button("×");
            if close.clicked() {
                state.show_flow = false;
            }
            close.on_hover_text(crate::i18n::tr("收起工作流面板"));
        });
    });
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "{}/{}  {}",
            current + 1,
            steps.len(),
            crate::i18n::tr(step.label)
        ))
        .size(SIZE_BODY)
        .color(text()),
    );
    ui.add_space(2.0);
    ui.label(
        RichText::new(crate::i18n::tr(task.label()))
            .size(SIZE_LABEL)
            .color(dim()),
    );
    ui.add_space(8.0);
    rule(
        ui.painter(),
        ui.max_rect().left() + 8.0,
        ui.max_rect().right() - 8.0,
        ui.cursor().top() + 2.0,
        divider(),
    );
    ui.add_space(10.0);

    // 步骤说明固定占两到三行，动作列表从同一高度开始，切步骤时按钮不会跳位。
    //
    // The step description occupies a stable block so the action rows do not jump when
    // the user switches steps.
    ui.label(
        RichText::new(crate::i18n::tr(step.hint))
            .size(SIZE_LABEL)
            .color(muted()),
    );
    ui.add_space(10.0);

    if !has_frames {
        ui.label(
            RichText::new(crate::i18n::tr("还没有打开序列"))
                .size(SIZE_LABEL)
                .color(dim()),
        );
        ui.add_space(6.0);
    }

    let mut clicked = None;
    for a in step.actions {
        let enabled = has_frames || !needs_frames(*a);
        let label = a.label();
        let resp = ui.add_enabled(
            enabled,
            egui::Button::new(RichText::new(label).size(SIZE_BODY))
                .min_size(Vec2::new(ui.available_width(), 28.0))
                .corner_radius(radius_control()),
        );
        if enabled && resp.clicked() {
            clicked = Some(*a);
        }
        if !enabled {
            resp.on_hover_text(crate::i18n::tr("先导入照片，这一步才有对象可作用"));
        }
        ui.add_space(4.0);
    }

    ui.add_space(10.0);
    ui.label(
        RichText::new(crate::i18n::tr("步骤状态由当前工程派生"))
            .size(SIZE_SMALL)
            .color(dim()),
    );

    if let Some(a) = clicked {
        run_action(state, ctx, a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_windows_force_the_panel_closed_but_an_empty_project_does_not() {
        let mut s = AppState::new();
        assert!(!flow_panel_visible(&s, 1600.0), "默认收起");
        s.show_flow = true;
        assert!(flow_panel_visible(&s, 1600.0));
        assert!(flow_panel_visible(&s, FLOW_MIN_W), "下限本身要能开");
        assert!(!flow_panel_visible(&s, FLOW_MIN_W - 1.0), "窄一像素就收起");
        assert!(
            flow_panel_visible(&s, 1600.0),
            "没有序列也要开，好给导入按钮"
        );
    }

    #[test]
    fn every_step_offers_three_to_five_actions_and_import_is_always_reachable() {
        for t in crate::shell::flow::FlowTask::ALL {
            for step in t.steps() {
                assert!((3..=5).contains(&step.actions.len()));
                assert!(
                    step.actions.iter().any(|a| !needs_frames(*a)),
                    "{}",
                    step.label
                );
            }
        }
    }
}
