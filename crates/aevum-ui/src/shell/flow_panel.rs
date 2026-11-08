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
        StepAction::Import => super::dialogs::open_photos_dialog(state),
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
        StepAction::ExportConfirm => state.request_export_confirm(ctx, false),
    }
}

/// 单张模式的一排直通动作：没有流程，只有去处（用户 2026-11-08：单张与延时分开）。
///
/// Flat actions for single-photo mode: no ceremony, just destinations.
static SINGLE_PHOTO_ACTIONS: &[StepAction] = &[
    StepAction::Import,
    StepAction::Goto(crate::state::Module::Develop),
    StepAction::Presets,
    StepAction::Look,
    StepAction::ExportConfirm,
];

pub(super) fn flow_panel(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let task = state.flow_task;
    let steps = task.steps();
    let current = flow::display_step(state, task);
    let step = &steps[current];
    let has_frames = state.frame_count() > 0;
    // 单张（≤1 帧）不走流程：步骤列表与任务切换都藏起来，只给一排直通动作。
    // 导入第二张后自动回到流程模式，不需要任何开关。
    //
    // A single photo (<=1 frame) skips the flow: the step list and the task picker hide,
    // leaving one flat row of actions. Loading a second frame returns to flow mode
    // automatically -- no switch to flip.
    let single = state.frame_count() <= 1;

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
            // 任务切换住在这里：标题栏不再画步骤条（用户 2026-11-08——单张
            // 不该有步骤仪式，延时的步骤也不该占标题栏）。单张时只给静态标签。
            //
            // The task picker lives here: no step bar in the title bar any more. Single
            // photos get a plain label instead.
            if single {
                ui.label(
                    RichText::new(crate::i18n::tr("单张修图"))
                        .size(SIZE_LABEL)
                        .color(muted()),
                );
            } else {
                let pick = ui
                    .small_button(crate::i18n::tr(task.label()))
                    .on_hover_text(crate::i18n::tr("切换工作流"));
                if let Some(t) = flow_task_menu(&pick) {
                    state.flow_task = t;
                    state.flow_step = None;
                }
            }
        });
    });
    ui.add_space(4.0);
    // 五步全程摆出来，点哪步看哪步：选中态走 display_step（点选优先、未选
    // 回落派生当前步），✓ 是派生进度，不因点选撒谎。单张没有这一段。
    //
    // All steps stay visible; clicking one shows it. The highlight follows
    // display_step (an explicit click wins, otherwise the derived current step), while
    // ✓ remains the derived progress and never lies because of a click. Single photos
    // get none of this.
    let states = flow::step_states(state, task);
    if !single {
        ui.add_space(2.0);
    }
    for (i, s) in steps
        .iter()
        .enumerate()
        .take(if single { 0 } else { steps.len() })
    {
        let marker = match states[i] {
            flow::StepState::Ready => "✓",
            flow::StepState::Current => "●",
            flow::StepState::Todo => "○",
        };
        let label = format!("{marker}  {}", crate::i18n::tr(s.label));
        let resp = ui.selectable_label(i == current, RichText::new(label).size(SIZE_BODY));
        let info = match states[i] {
            flow::StepState::Ready => crate::i18n::tr("输入已就绪"),
            flow::StepState::Current => crate::i18n::tr("下一步"),
            flow::StepState::Todo => crate::i18n::tr("尚未开始"),
        };
        let resp = resp.on_hover_text(format!("{} · {}", crate::i18n::tr(s.label), info));
        if resp.clicked() {
            state.flow_step = Some(i);
            state.set_module(s.module);
        }
    }
    if !single {
        ui.add_space(6.0);
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
    } else {
        ui.label(
            RichText::new(crate::i18n::tr("单张照片不需要流程，下面的动作直接用"))
                .size(SIZE_LABEL)
                .color(dim()),
        );
        ui.add_space(2.0);
    }
    ui.add_space(2.0);
    rule(
        ui.painter(),
        ui.max_rect().left() + 8.0,
        ui.max_rect().right() - 8.0,
        ui.cursor().top() + 2.0,
        divider(),
    );
    ui.add_space(10.0);

    // 步骤说明固定占两到三行，动作列表从同一高度开始，切步骤时按钮不会跳位。
    // 单张没有步骤，也就没有说明。
    //
    // The step description occupies a stable block so the action rows do not jump when
    // the user switches steps. Single photos have no steps, hence no description.
    if !single {
        ui.label(
            RichText::new(crate::i18n::tr(step.hint))
                .size(SIZE_LABEL)
                .color(muted()),
        );
        ui.add_space(10.0);
    } else {
        ui.add_space(6.0);
    }

    if !has_frames {
        ui.label(
            RichText::new(crate::i18n::tr("还没有打开序列"))
                .size(SIZE_LABEL)
                .color(dim()),
        );
        ui.add_space(6.0);
    }

    let mut clicked = None;
    for a in if single {
        SINGLE_PHOTO_ACTIONS
    } else {
        step.actions
    } {
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

    // 回到主线：「下一步」清掉手选、跳到派生的当前步。步骤列表允许用户
    // 随便看，这里永远提供一条回主线的路。单张没有流程，也就没有这一颗。
    //
    // Back to the main line: "next" drops the manual pick and jumps to the derived
    // current step. The list is free browsing; this is always the way home. Single
    // photos have no flow, hence no button.
    if !single {
        ui.add_space(6.0);
    }
    if !single && let Some(next) = flow::next_step(state, task) {
        let back = format!(
            "{}  →",
            crate::i18n::tr_fmt("下一步：{}", &[&crate::i18n::tr(next.label)])
        );
        if ui
            .add_enabled(
                current != flow::derived_current(state, task),
                egui::Button::new(RichText::new(back).size(SIZE_BODY))
                    .min_size(Vec2::new(ui.available_width(), 26.0))
                    .corner_radius(radius_control()),
            )
            .clicked()
        {
            state.flow_step = None;
            state.set_module(next.module);
        }
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(crate::i18n::tr("步骤状态由当前工程派生"))
            .size(SIZE_SMALL)
            .color(dim()),
    );

    if let Some(a) = clicked {
        run_action(state, ctx, a);
    }
}

/// 顶部步骤栏（LapseOne 式）：多帧序列的三段横排在画布上方，点标签即跳段。
/// 单张与未开序列都不摆——那两种状态没有流程。
///
/// The top step bar (LapseOne-style): the flow's phases sit horizontally above the
/// canvas; clicking a tab jumps there. Neither single photos nor an empty project get
/// one -- neither has a flow.
pub fn flow_top_bar(state: &mut AppState, ui: &mut Ui) {
    let task = state.flow_task;
    let steps = task.steps();
    let states = flow::step_states(state, task);
    let current = flow::display_step(state, task);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // LapseOne 分段式：每段「编号 + 名称」的胶囊，激活段强调色填充、
        // 完成段带 ✓、未到段灰字——三种状态一眼可分，不再是三枚裸文字。
        for (i, s) in steps.iter().enumerate() {
            let done = states[i] == flow::StepState::Ready;
            let label = if done {
                format!("✓ {}  {}", i + 1, crate::i18n::tr(s.label))
            } else {
                format!("{}  {}", i + 1, crate::i18n::tr(s.label))
            };
            let active = i == current;
            let text_c = if active {
                on_selected()
            } else if done {
                text()
            } else {
                muted()
            };
            let resp = ui.add(
                egui::Button::new(RichText::new(label).size(SIZE_BODY).color(text_c))
                    .fill(if active { accent() } else { elev() })
                    .corner_radius(radius_control())
                    .min_size(Vec2::new(0.0, 24.0)),
            );
            let info = match states[i] {
                flow::StepState::Ready => crate::i18n::tr("输入已就绪"),
                flow::StepState::Current => crate::i18n::tr("下一步"),
                flow::StepState::Todo => crate::i18n::tr("尚未开始"),
            };
            let resp = resp.on_hover_text(format!("{} · {}", crate::i18n::tr(s.label), info));
            if resp.clicked() {
                state.flow_step = Some(i);
                state.set_module(s.module);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{}/{}", current + 1, steps.len()))
                    .size(SIZE_SMALL)
                    .color(muted()),
            );
        });
    });
    ui.add_space(2.0);
}

/// 任务切换菜单（原标题栏的 flow_task_menu，随步骤条一起搬进来）。
///
/// The task picker menu (formerly the title bar's, moved with the step list).
fn flow_task_menu(resp: &egui::Response) -> Option<flow::FlowTask> {
    let mut picked = None;
    egui::Popup::menu(resp).width(160.0).show(|ui| {
        for t in flow::FlowTask::ALL {
            if ui
                .button(crate::i18n::tr(t.label()))
                .on_hover_text(crate::i18n::tr("切换工作流"))
                .clicked()
            {
                picked = Some(t);
                ui.close();
            }
        }
    });
    picked
}

#[cfg(test)]
mod tests {
    /// 手选态只有一条写入（点步骤行）与一条清零（切任务）：多一条就会出现
    /// 「点了没反应」或「高亮钉死」的某一种。flow.rs 产品段不许再写 flow_step——
    /// 步骤条撤除后标题栏已经没有这个职责。
    ///
    /// Exactly one write (step-row click) and one clear (task switch): any extra makes
    /// either a dead click or a stuck highlight. flow.rs no longer touches flow_step --
    /// the title bar is out of this business.
    #[test]
    fn the_manual_pick_has_one_write_and_one_clear() {
        let code = include_str!("flow_panel.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or("");
        // 两条写入都是合法点击路径：面板里的步骤行 + 顶部的 LapseOne 式标签。
        assert_eq!(
            code.matches("state.flow_step = Some(i);").count(),
            2,
            "手选写入（面板行 + 顶部标签）数量变了"
        );
        // 两条清零都是合法语义：切任务换一套步骤、底部「下一步」回到派生主线。
        assert_eq!(
            code.matches("state.flow_step = None;").count(),
            2,
            "清零路径（切任务 + 回到主线）数量变了"
        );
        let flow = include_str!("flow.rs");
        let flow_code = flow.split("#[cfg(test)]").next().unwrap_or("");
        assert_eq!(
            flow_code.matches("state.flow_step").count(),
            0,
            "flow.rs 不该再写选中态"
        );
    }

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
