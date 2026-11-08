//! 工作流步骤：三态派生与「显示哪一步」的口径，供左侧工作流面板使用。
//!
//! 步骤条曾挂在标题栏（2026-11-08 用户裁决撤除）：单张修图不该有步骤仪式，
//! 延时/合成的步骤列表现在住在工作流面板里。状态依旧全部从真实工程派生，
//! 不另存"进度"——另存的进度一定会和参数、序列、导出结果对不上。
//!
//! Workflow steps: the tri-state derivation and the "which step is shown" rules,
//! consumed by the left workflow panel. The title-bar step bar was removed by user
//! decision (2026-11-08): a single photo needs no step ceremony, and the timelapse /
//! merge step lists now live in the panel. States still derive from real project state
//! rather than a progress counter.

use aevum_core::grade::GradeParams;
use serde::{Deserialize, Serialize};

use crate::state::{AppState, DockTab, Module};

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
    // 单张修图（Develop）保留变体与步骤表：旧 settings 里的值能对上号，但
    // 它不再出现在 UI——单张不该有流程仪式（用户 2026-11-08）。
    pub const ALL: [FlowTask; 2] = [FlowTask::Timelapse, FlowTask::Merge];

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

static TIMELAPSE: [Step; 3] = [
    Step {
        label: "序列管理",
        module: Module::Sequence,
        check: StepCheck::Frames,
        hint: "导入与整理这一趟照片：去闪、对齐都在这一段做完，序列按文件名排序。",
        actions: &[
            A::Import,
            A::Rescan,
            A::Goto(Module::Sequence),
            A::Goto(Module::Deflicker),
            A::Goto(Module::Frame),
        ],
    },
    Step {
        label: "调色与修复",
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
        label: "导出视频",
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

/// 界面上应当高亮/展示哪一步：用户点选优先，没点选就跟随派生的当前步。
/// 步骤条与工作流面板都走这一个口径，两边永远不会各说各话。
///
/// Which step the UI highlights/shows: an explicit click wins, otherwise follow the
/// derived current step. Both the step bar and the workflow panel go through this one
/// function, so they can never disagree.
pub fn display_step(state: &AppState, task: FlowTask) -> usize {
    let n = task.steps().len();
    state
        .flow_step
        .filter(|i| *i < n)
        .unwrap_or_else(|| derived_current(state, task))
}

/// 派生的当前步：第一个输入没就绪的步骤（全就绪时停在最后一步）。
///
/// The derived current step: the first one whose input is not ready (stops at the last
/// step when everything is ready).
pub fn derived_current(state: &AppState, task: FlowTask) -> usize {
    step_states(state, task)
        .iter()
        .position(|s| *s == StepState::Current)
        .unwrap_or(task.steps().len() - 1)
}

/// 下一步要做什么。全部就绪时返回 `None`（最后一步永远不"就绪"，所以通常不会发生）。
///
/// The next action, or `None` when everything is ready (the last step never reads as
/// ready, so in practice this does not happen).
pub fn next_step(state: &AppState, task: FlowTask) -> Option<&'static Step> {
    let steps = task.steps();
    steps.iter().find(|s| !check_ready(state, s.check))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flow_in_the_picker_is_wired_and_ends_in_an_action() {
        for t in FlowTask::ALL {
            let steps = t.steps();
            assert!((3..=5).contains(&steps.len()), "{:?} 的步骤数变了", t);
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
    fn display_step_follows_the_clicked_step() {
        let mut state = AppState::new();
        // 未点选：跟随派生（空工程 = 第一步）。
        assert_eq!(display_step(&state, FlowTask::Merge), 0);
        // 点了「调色」（索引 3）：高亮与面板都必须跟着走。
        state.flow_step = Some(3);
        assert_eq!(display_step(&state, FlowTask::Merge), 3);
        // 点「去闪」（索引 1）：跟着走。
        state.flow_step = Some(1);
        assert_eq!(display_step(&state, FlowTask::Merge), 1);
        // 「下一步」清掉手选：回到派生主线。
        state.flow_step = None;
        assert_eq!(display_step(&state, FlowTask::Merge), 0);
        // 越界防御（切换工作流漏清也不至于 panic）：回落派生。
        state.flow_step = Some(99);
        assert_eq!(display_step(&state, FlowTask::Merge), 0);
    }

    #[test]
    fn states_stop_at_the_first_unmet_input() {
        let state = AppState::new();
        for t in FlowTask::ALL {
            let states = step_states(&state, t);
            assert_eq!(states.len(), t.steps().len());
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
