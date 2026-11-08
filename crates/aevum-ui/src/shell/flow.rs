//! 流程步骤条：把「现在做到哪、下一步做什么」摆进标题栏，不新增一行。
//!
//! 三套步骤集按任务手选，状态全部从真实工程状态派生，不另存一份"进度"——
//! 另存的进度一定会和参数、序列、导出结果对不上，那是"假软件"最常见的破绽。
//!
//! Step bar: "where am I, what is next" lives in the title bar, not in a new row.
//! Three hand-authored step sets are chosen by task; every state is derived from real
//! project state rather than a separate progress counter, which would inevitably drift
//! away from the parameters, sequence and export results.

use aevum_core::grade::GradeParams;
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};
use serde::{Deserialize, Serialize};

use crate::state::{AppState, DockTab, Module};
use crate::theme::*;

/// 步骤条高度取自 `theme::STEP_H`，与其它视觉 token 同一出口。
///
/// The step bar height comes from `theme::STEP_H`, the single source for visual tokens.
const STEP_W: f32 = 76.0;
/// 紧凑模式下只画编号圆点的直径。
///
/// Diameter of the numbered dot in compact mode.
const STEP_DOT: f32 = 20.0;
/// 步骤之间的缝（含连接线）。
///
/// Gap between steps, connector included.
const STEP_GAP: f32 = 8.0;
/// 右端「下一步：X」在完整 / 紧凑模式下的宽度。
///
/// Width of the trailing "Next: X" label in full / compact mode.
const NEXT_W: f32 = 150.0;
const NEXT_MIN_W: f32 = 120.0;

/// 三条工作流。选择权在用户手里（左侧工作流面板里换），不跟着模块自动跳——
/// 自动跳会让用户刚切到「去闪」看一眼，整条步骤就换了套。
///
/// The three workflows. The user picks one in the left workflow panel; it does not
/// auto-switch with the module, or a one-second visit to Deflicker would swap the whole
/// step set out from under them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlowTask {
    #[default]
    Timelapse,
    Develop,
    Merge,
}

impl FlowTask {
    pub const ALL: [FlowTask; 3] = [FlowTask::Timelapse, FlowTask::Develop, FlowTask::Merge];

    pub fn key(self) -> &'static str {
        match self {
            FlowTask::Timelapse => "timelapse",
            FlowTask::Develop => "develop",
            FlowTask::Merge => "merge",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.key() == s.trim())
    }

    pub fn label(self) -> &'static str {
        match self {
            FlowTask::Timelapse => "延时序列",
            FlowTask::Develop => "单张修图",
            FlowTask::Merge => "多帧合成",
        }
    }

    /// 新工程第一次打开时猜一条：合成模块进来就给合成流程，显影给修图，其余给延时。
    /// 只在初始化调用，之后以用户选择为准。
    ///
    /// Guess one for a fresh project from the initial module; after that the user's choice
    /// wins. Only called at construction.
    pub fn for_module(m: Module) -> Self {
        match m {
            Module::Merge => FlowTask::Merge,
            Module::Develop => FlowTask::Develop,
            _ => FlowTask::Timelapse,
        }
    }

    pub fn steps(self) -> &'static [Step] {
        match self {
            FlowTask::Timelapse => &TIMELAPSE,
            FlowTask::Develop => &DEVELOP,
            FlowTask::Merge => &MERGE,
        }
    }
}

/// 一步的"输入就绪"判据。全是读现有状态，不写、不新建进度字段。
///
/// A step's "input is ready" test. Every variant reads existing state; none writes or
/// introduces a separate progress field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepCheck {
    Frames,
    Deflick,
    Aligned,
    Graded,
    Masked,
    OutDir,
    Merged,
    /// 最后一步永远是"下一步"：导出是可重复的动作，没有"已完成"这个状态。
    ///
    /// The last step stays "next" forever: exporting is a repeatable action with no
    /// permanent done state.
    Never,
}

/// 左侧工作流面板上的一颗动作按钮能做什么。全部绑到既有入口，不新造旁路。
///
/// What a button in the left workflow panel does. Everything binds to an existing
/// entry point; the panel gets no side channel of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepAction {
    Import,
    Rescan,
    Goto(Module),
    Dock(DockTab),
    Presets,
    Auto,
    Reset,
    Look,
    ExportConfirm,
}

impl StepAction {
    pub fn label(self) -> String {
        match self {
            StepAction::Import => crate::i18n::tr("导入照片…").to_owned(),
            StepAction::Rescan => crate::i18n::tr("重新扫描").to_owned(),
            StepAction::Goto(m) => crate::i18n::tr_fmt("转到{}", &[&crate::i18n::tr(m.cn())]),
            StepAction::Dock(t) => crate::i18n::tr_fmt("打开{}页签", &[&crate::i18n::tr(t.cn())]),
            StepAction::Presets => crate::i18n::tr("预设与快照").to_owned(),
            StepAction::Auto => crate::i18n::tr("自动曝光").to_owned(),
            StepAction::Reset => crate::i18n::tr("全部复位").to_owned(),
            StepAction::Look => crate::i18n::tr("外观与布局").to_owned(),
            StepAction::ExportConfirm => crate::i18n::tr("导出设置…").to_owned(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Step {
    pub label: &'static str,
    pub module: Module,
    pub check: StepCheck,
    /// 这一步要干什么，写给左侧工作流面板看。
    ///
    /// What this step is for, shown in the left workflow panel.
    pub hint: &'static str,
    pub actions: &'static [StepAction],
}

use StepAction as A;

static TIMELAPSE: [Step; 5] = [
    Step {
        label: "导入序列",
        module: Module::Sequence,
        check: StepCheck::Frames,
        hint: "把同一趟拍的照片放进一个目录，序列按文件名排序。",
        actions: &[
            A::Import,
            A::Rescan,
            A::Goto(Module::Sequence),
            A::Dock(DockTab::Info),
        ],
    },
    Step {
        label: "去闪",
        module: Module::Deflicker,
        check: StepCheck::Deflick,
        hint: "读每帧亮度，压掉光圈/快门跳变带来的整体明暗闪烁。",
        actions: &[
            A::Goto(Module::Deflicker),
            A::Dock(DockTab::Keys),
            A::Rescan,
        ],
    },
    Step {
        label: "对齐",
        module: Module::Frame,
        check: StepCheck::Aligned,
        hint: "估计帧间位移；三脚架被碰过、或是手持延时，先对齐再谈裁剪。",
        actions: &[A::Goto(Module::Frame), A::Dock(DockTab::Info), A::Reset],
    },
    Step {
        label: "调色",
        module: Module::Develop,
        check: StepCheck::Graded,
        hint: "影调、白平衡、曲线与局部蒙版都作用在整个序列上。",
        actions: &[
            A::Goto(Module::Develop),
            A::Auto,
            A::Presets,
            A::Dock(DockTab::Adjust),
        ],
    },
    Step {
        label: "成片",
        module: Module::Render,
        check: StepCheck::Never,
        hint: "选范围、格式与目录，然后导出；运镜/变速/水印仍在右坞。",
        actions: &[
            A::Goto(Module::Render),
            A::ExportConfirm,
            A::Dock(DockTab::Info),
        ],
    },
];

static DEVELOP: [Step; 5] = [
    Step {
        label: "导入照片",
        module: Module::Sequence,
        check: StepCheck::Frames,
        hint: "单张也走序列容器：以后想补成延时，不用换工程。",
        actions: &[A::Import, A::Rescan, A::Goto(Module::Sequence)],
    },
    Step {
        label: "影调",
        module: Module::Develop,
        check: StepCheck::Graded,
        hint: "先把曝光、对比与白平衡定下来，再谈风格。",
        actions: &[A::Goto(Module::Develop), A::Auto, A::Presets, A::Reset],
    },
    Step {
        label: "局部",
        module: Module::Develop,
        check: StepCheck::Masked,
        hint: "线性/径向/画笔蒙版只改选区，参数独立于全局调色。",
        actions: &[
            A::Goto(Module::Develop),
            A::Dock(DockTab::Adjust),
            A::Presets,
        ],
    },
    Step {
        label: "输出设置",
        module: Module::Render,
        check: StepCheck::OutDir,
        hint: "确认输出目录、格式、位深与锐化，导出前都能改。",
        actions: &[A::Goto(Module::Render), A::Dock(DockTab::Info), A::Look],
    },
    Step {
        label: "导出",
        module: Module::Render,
        check: StepCheck::Never,
        hint: "导出写新文件，原图永不覆写。",
        actions: &[
            A::ExportConfirm,
            A::Goto(Module::Render),
            A::Dock(DockTab::Info),
        ],
    },
];

static MERGE: [Step; 5] = [
    Step {
        label: "选片",
        module: Module::Sequence,
        check: StepCheck::Frames,
        hint: "HDR 包围、全景、堆栈都要一组曝光/位置不同的源片。",
        actions: &[A::Import, A::Rescan, A::Goto(Module::Sequence)],
    },
    Step {
        label: "对齐",
        module: Module::Frame,
        check: StepCheck::Aligned,
        hint: "手持包围先对齐，否则合成边缘会出重影。",
        actions: &[A::Goto(Module::Frame), A::Dock(DockTab::Info), A::Reset],
    },
    Step {
        label: "合成",
        module: Module::Merge,
        check: StepCheck::Merged,
        hint: "选一种合成方式，跑完会得到一张新的静帧。",
        actions: &[A::Goto(Module::Merge), A::Dock(DockTab::Adjust), A::Reset],
    },
    Step {
        label: "精修",
        module: Module::Develop,
        check: StepCheck::Graded,
        hint: "合成结果是普通照片，照常走影调与局部。",
        actions: &[A::Goto(Module::Develop), A::Auto, A::Presets],
    },
    Step {
        label: "导出",
        module: Module::Render,
        check: StepCheck::Never,
        hint: "导出写新文件，原图与合成中间结果都保留。",
        actions: &[
            A::ExportConfirm,
            A::Goto(Module::Render),
            A::Dock(DockTab::Info),
        ],
    },
];

/// 一步的三种显示状态。
///
/// The three visible states of a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    /// 输入已就绪（不是"用户点过完成"）。
    ///
    /// Its input is ready (not "the user ticked a box").
    Ready,
    Current,
    Todo,
}

/// 从真实状态派生每一步是否就绪。
///
/// Derive each step's readiness from real state.
pub fn check_ready(state: &AppState, check: StepCheck) -> bool {
    match check {
        StepCheck::Frames => state.frame_count() > 0,
        StepCheck::Deflick => !state.corr.is_empty(),
        StepCheck::Aligned => state.align_report.is_some(),
        StepCheck::Graded => state.params != GradeParams::default(),
        StepCheck::Masked => !state.params.masks.is_empty(),
        StepCheck::OutDir => !state.render_out_dir.as_os_str().is_empty(),
        StepCheck::Merged => state.merge_result.is_some(),
        StepCheck::Never => false,
    }
}

/// 状态全派生：第一个没就绪的就是"当前步"，它之前的都算输入已就绪。
///
/// All states derived: the first step whose input is not ready is "current", and the
/// ones before it read as ready.
pub fn step_states(state: &AppState, task: FlowTask) -> Vec<StepState> {
    let steps = task.steps();
    let current = steps
        .iter()
        .position(|s| !check_ready(state, s.check))
        .unwrap_or(steps.len());
    (0..steps.len())
        .map(|i| {
            if i < current {
                StepState::Ready
            } else if i == current {
                StepState::Current
            } else {
                StepState::Todo
            }
        })
        .collect()
}

/// 下一步要做什么。全部就绪时返回 `None`（最后一步永远不"就绪"，所以通常不会发生）。
///
/// The next action, or `None` when everything is ready (the last step never reads as
/// ready, so in practice this does not happen).
pub fn next_step(state: &AppState, task: FlowTask) -> Option<&'static Step> {
    let steps = task.steps();
    steps.iter().find(|s| !check_ready(state, s.check))
}

/// 给定可用宽度，算出能画几步、是否降级成圆点、还放不放得下「下一步」。
/// 纯函数：1280×800 与 900×600 的降级都能在单测里直接量。
///
/// How many steps fit, whether to fall back to dots, and whether "Next" still fits, for
/// a given width. A pure function: both the 1280x800 and 900x600 degradations can be
/// measured in a unit test without opening a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepFit {
    pub shown: usize,
    pub compact: bool,
    pub next: bool,
}

pub fn step_fit(avail_w: f32, n: usize) -> StepFit {
    if n == 0 || avail_w <= 0.0 {
        return StepFit {
            shown: 0,
            compact: true,
            next: false,
        };
    }
    let full = n as f32 * (STEP_W + STEP_GAP) + NEXT_W;
    if avail_w >= full {
        return StepFit {
            shown: n,
            compact: false,
            next: true,
        };
    }
    let dots = n as f32 * (STEP_DOT + STEP_GAP) + NEXT_MIN_W;
    if avail_w >= dots {
        return StepFit {
            shown: n,
            compact: true,
            next: true,
        };
    }
    let per = STEP_DOT + STEP_GAP;
    let with_next = ((avail_w - NEXT_MIN_W) / per).floor().max(0.0) as usize;
    if with_next > 0 {
        return StepFit {
            shown: with_next.min(n),
            compact: true,
            next: true,
        };
    }
    StepFit {
        shown: ((avail_w / per).floor().max(0.0) as usize).min(n),
        compact: true,
        next: false,
    }
}

/// 画一行按可用宽度截断的文字。步骤芯片是固定宽的，英文标签（"Import sequence"）
/// 原样画会越出芯片、压住相邻两枚——重叠判据在 `Lang("en")` 下当场点名。
///
/// Paint one line truncated to the available width. Step chips have a fixed width, so
/// painting an English label like "Import sequence" verbatim spills out and covers both
/// neighbours -- the overlap gate catches it under `Lang("en")`.
fn paint_truncated(
    p: &egui::Painter,
    center_x: Option<f32>,
    left_x: f32,
    cy: f32,
    text: &str,
    max_w: f32,
    color: Color32,
) {
    let mut job =
        egui::text::LayoutJob::simple_singleline(text.to_owned(), font(SIZE_LABEL), color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = p.layout_job(job);
    let x = center_x.map_or(left_x, |c| c - galley.size().x * 0.5);
    p.galley(Pos2::new(x, cy - galley.size().y * 0.5), galley, color);
}

/// 画步骤条。`rect` 是菜单行里那段空白，画不下就整体不画（不挤菜单）。
///
/// Draw the step bar into the given slice of the menu row. If it does not fit, draw
/// nothing rather than squeezing the menus.
pub fn step_bar(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    if rect.width() < 120.0 {
        return;
    }
    let task = state.flow_task;
    let steps = task.steps();
    let states = step_states(state, task);
    let fit = step_fit(rect.width(), steps.len());
    if fit.shown == 0 {
        return;
    }
    let p = ui.painter().clone();
    let cy = rect.center().y;
    let dot = STEP_DOT.min(STEP_H);

    // 任务名：点开换流程。步骤集不跟着模块自动跳，切换必须是一个明确的动作。
    let task_label = crate::i18n::tr(task.label());
    let task_w = p
        .layout_no_wrap(task_label.to_owned(), font(SIZE_LABEL), muted())
        .size()
        .x
        + 14.0;
    let task_rect = Rect::from_min_size(
        Pos2::new(rect.left(), cy - STEP_H * 0.5),
        Vec2::new(task_w, STEP_H),
    );
    let tr = ui.interact(
        task_rect,
        ui.make_persistent_id("flow-task"),
        Sense::click(),
    );
    if tr.hovered() {
        p.rect_filled(task_rect, radius_control(), elev_hover());
    }
    p.text(
        task_rect.center(),
        Align2::CENTER_CENTER,
        task_label,
        font(SIZE_LABEL),
        if tr.hovered() { text() } else { muted() },
    );
    tr.clone().on_hover_text(crate::i18n::tr("切换工作流"));
    if let Some(t) = flow_task_menu(&tr) {
        state.flow_task = t;
    }

    let mut x = task_rect.right() + 8.0;
    let per = if fit.compact {
        STEP_DOT + STEP_GAP
    } else {
        STEP_W + STEP_GAP
    };
    let mut jump: Option<Module> = None;
    for (i, step) in steps.iter().enumerate().take(fit.shown) {
        let w = if fit.compact { dot } else { STEP_W };
        let chip = Rect::from_min_size(Pos2::new(x, cy - w * 0.5), Vec2::new(w, w));
        let resp = ui.interact(
            chip,
            ui.make_persistent_id(("flow-step", i)),
            Sense::click(),
        );
        let st = states[i];
        if st == StepState::Current {
            p.rect_filled(chip, radius_control(), selected());
        } else if resp.hovered() {
            p.rect_filled(chip, radius_control(), elev_hover());
        }
        if fit.compact {
            p.text(
                chip.center(),
                Align2::CENTER_CENTER,
                (i + 1).to_string(),
                font(SIZE_LABEL),
                if st == StepState::Current {
                    on_selected()
                } else if st == StepState::Ready {
                    text()
                } else {
                    muted()
                },
            );
        } else {
            let label = crate::i18n::tr(step.label);
            let caption = if st == StepState::Ready {
                format!("✓ {label}")
            } else {
                label.to_owned()
            };
            let color = if st == StepState::Current {
                on_selected()
            } else if st == StepState::Ready {
                text()
            } else {
                muted()
            };
            paint_truncated(
                &p,
                Some(chip.center().x),
                chip.left(),
                cy,
                &caption,
                w - 8.0,
                color,
            );
        }
        let info = match st {
            StepState::Ready => crate::i18n::tr("输入已就绪"),
            StepState::Current => crate::i18n::tr("下一步"),
            StepState::Todo => crate::i18n::tr("尚未开始"),
        };
        resp.clone()
            .on_hover_text(format!("{} · {}", crate::i18n::tr(step.label), info));
        if resp.clicked() {
            jump = Some(step.module);
        }
        x += per;
        if i + 1 < fit.shown {
            p.line_segment(
                [Pos2::new(x - STEP_GAP + 1.0, cy), Pos2::new(x - 1.0, cy)],
                Stroke::new(1.0, divider()),
            );
        }
    }

    if fit.next
        && let Some(step) = next_step(state, task)
    {
        let label = crate::i18n::tr_fmt("下一步：{}", &[&crate::i18n::tr(step.label)]);
        let w = (NEXT_W).min(rect.right() - x);
        if w > 40.0 {
            let r = Rect::from_min_size(Pos2::new(x, cy - STEP_H * 0.5), Vec2::new(w, STEP_H));
            let resp = ui.interact(r, ui.make_persistent_id("flow-next"), Sense::click());
            paint_truncated(
                &p,
                None,
                r.left(),
                cy,
                &label,
                w - 6.0,
                if resp.hovered() { text() } else { muted() },
            );
            resp.clone().on_hover_text(crate::i18n::tr("跳到这一步"));
            if resp.clicked() {
                jump = Some(step.module);
            }
        }
    }
    if let Some(m) = jump {
        state.set_module(m);
        state.show_flow = true;
    }
}

/// 任务切换菜单。抽出来是为了让 `step_bar` 只管排布与命中。
///
/// Task picker. Split out so `step_bar` only handles layout and hit testing.
fn flow_task_menu(resp: &egui::Response) -> Option<FlowTask> {
    let mut picked = None;
    egui::Popup::menu(resp).width(160.0).show(|ui| {
        for t in FlowTask::ALL {
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
    use super::*;

    #[test]
    fn three_step_sets_are_wired_and_end_in_an_action() {
        for t in FlowTask::ALL {
            let steps = t.steps();
            assert_eq!(steps.len(), 5, "{:?} 的步骤数变了", t);
            assert_eq!(steps.last().map(|s| s.check), Some(StepCheck::Never));
            for s in steps {
                assert!(!s.label.is_empty());
                assert!(!s.hint.is_empty());
                assert!(
                    (1..=5).contains(&s.actions.len()),
                    "{:?} 的「{}」动作数 {}",
                    t,
                    s.label,
                    s.actions.len()
                );
            }
        }
        assert_eq!(FlowTask::parse("merge"), Some(FlowTask::Merge));
        assert_eq!(FlowTask::parse("nope"), None);
    }

    #[test]
    fn step_fit_degrades_without_losing_the_next_label() {
        // 1280×800 的菜单行空白约 650px：五步全展开 + 下一步。
        assert_eq!(
            step_fit(660.0, 5),
            StepFit {
                shown: 5,
                compact: false,
                next: true
            }
        );
        // 中等宽度：圆点 + 下一步。
        assert_eq!(
            step_fit(320.0, 5),
            StepFit {
                shown: 5,
                compact: true,
                next: true
            }
        );
        // 很窄：少画几步也要保住下一步。
        let narrow = step_fit(180.0, 5);
        assert!(narrow.compact && narrow.next && narrow.shown >= 1 && narrow.shown < 5);
        // 连下一步都放不下时宁可只画点，也不把标签压成两截。
        let tiny = step_fit(50.0, 5);
        assert!(tiny.compact && !tiny.next);
        assert_eq!(step_fit(660.0, 0).shown, 0);
    }

    #[test]
    fn step_fit_never_draws_outside_the_given_width() {
        for w in [0.0, 40.0, 120.0, 180.0, 320.0, 500.0, 660.0, 900.0] {
            let f = step_fit(w, 5);
            let per = if f.compact {
                STEP_DOT + STEP_GAP
            } else {
                STEP_W + STEP_GAP
            };
            let used = f.shown as f32 * per + if f.next { NEXT_MIN_W } else { 0.0 };
            assert!(used <= w + per, "宽 {w}：排布用了 {used}，会溢出（{f:?}）");
        }
    }

    #[test]
    fn states_stop_at_the_first_unmet_input() {
        let state = AppState::new();
        for t in FlowTask::ALL {
            let states = step_states(&state, t);
            assert_eq!(states.len(), 5);
            // 空工程：第一步就没就绪，后面全是"尚未开始"。
            assert_eq!(states[0], StepState::Current);
            assert!(states[1..].iter().all(|s| *s == StepState::Todo));
            assert_eq!(
                next_step(&state, t).map(|s| s.label),
                Some(t.steps()[0].label)
            );
        }
    }

    /// 数据表里的中文键也要有英文译文：`i18n_extract.py` 只扫 `t!` / `tr_fmt`
    /// 字面量，扫不到「在取值处才过 tr」的步骤名、说明与动作名。
    ///
    /// Chinese keys in data tables need English entries too: `i18n_extract.py` only
    /// scans `t!` / `tr_fmt` literals and cannot see step names, hints and action names
    /// that pass through `tr` at the point of use.
    #[test]
    fn every_flow_label_and_hint_has_an_english_entry() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/i18n_en.rs");
        let en = std::fs::read_to_string(&path).expect("读英文表");
        let mut keys: Vec<&str> = Vec::new();
        for t in FlowTask::ALL {
            keys.push(t.label());
            for s in t.steps() {
                keys.push(s.label);
                keys.push(s.hint);
            }
        }
        for k in [
            "导入照片…",
            "重新扫描",
            "转到{}",
            "打开{}页签",
            "预设与快照",
            "自动曝光",
            "全部复位",
            "外观与布局",
            "导出设置…",
            "切换工作流",
            "输入已就绪",
            "下一步",
            "尚未开始",
            "下一步：{}",
            "跳到这一步",
            "工作流",
            "收起工作流面板",
            "步骤状态由当前工程派生",
            "还没有打开序列",
            "先导入照片，这一步才有对象可作用",
        ] {
            keys.push(k);
        }
        for key in keys {
            assert!(
                en.contains(&format!("(\"{key}\",")),
                "英文表缺键「{key}」，切到 English 会漏出中文"
            );
        }
    }
}
