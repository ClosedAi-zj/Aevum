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

use egui::{Color32, Context, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

use super::flow::{self, StepAction};

use crate::state::AppState;
use crate::state::Module;
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
pub(super) fn run_action(state: &mut AppState, ctx: &Context, a: StepAction) {
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
    // 步骤导航只在顶栏（横向页签），面板不再重复一列竖排步骤——两处列同一套
    // 步骤是用户 2026-11-09 明确指出的重复。面板这里只留「当前段说明 + 动作」。
    //
    // Step navigation lives in the top bar only (horizontal tabs); the panel no longer
    // repeats the same steps as a vertical list -- exactly the duplication the user
    // called out. The panel keeps the current phase's description and actions.
    // 单张模式不再用一行文字解释「界面为什么长这样」：状态由标题与动作本身
    // 表达，解释收进标题的 tooltip（2026-11-09 审计 #41）。
    //
    // Single mode no longer explains itself in a standing line; the title and the
    // actions already say it, and the explanation moves into a tooltip.
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
        // 说明只留一行（约 18 字），全文进 tooltip：每步一大段散文会把动作按钮
        // 顶到折线以下，而用户此刻需要的是按钮，不是导读（审计 #43）。
        //
        // The hint keeps one line (~18 chars) and the full text goes to a tooltip: a
        // paragraph per step pushes the action buttons below the fold, while buttons are
        // what the user needs right now.
        let full = crate::i18n::tr(step.hint);
        ui.label(
            RichText::new(crate::theme::ellipsis(full, 18))
                .size(SIZE_LABEL)
                .color(muted()),
        )
        .on_hover_text(full);
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
                .min_size(Vec2::new(ui.available_width(), CH))
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
                    .min_size(Vec2::new(ui.available_width(), CH))
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
/// 步骤按钮的视觉状态（纯样式，不含任何交互判定）。
///
/// Visual state of a step button: style only, no interaction decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StepVisual {
    pub fill: Color32,
    pub stroke: Color32,
    pub text: Color32,
    pub underline: bool,
}

/// 表驱动的五态样式：默认 / 悬停 / 按下 / 选中 / 禁用，全部走 theme token，
/// 不裸写颜色。禁用态只定义样式契约——**步骤恒可点**，交互逻辑不因此改变。
/// 选中态带底部强调线；按下态对选中段同样生效（按下时换 press 底，松手回
/// selected 底），这样四态在每一段上都能被看见。
///
/// Table-driven five-state style: default / hover / pressed / selected / disabled,
/// all through theme tokens. The disabled entry defines a style contract only --
/// **every step stays clickable**, so no interaction logic changes. The selected
/// state carries the bottom accent line; pressing also applies on the selected
/// segment (press fill while held, back to selected on release), so all four
/// states are visible on every segment.
pub(super) fn step_visual(
    done: bool,
    active: bool,
    hovered: bool,
    pressed: bool,
    enabled: bool,
) -> StepVisual {
    if !enabled {
        return StepVisual {
            fill: elev(),
            stroke: divider(),
            text: muted(),
            underline: false,
        };
    }
    let underline = active;
    if pressed {
        StepVisual {
            fill: press(),
            stroke: accent_hover(),
            text: on_elev(),
            underline,
        }
    } else if hovered {
        StepVisual {
            fill: elev_hover(),
            stroke: border_strong(),
            text: text(),
            underline,
        }
    } else if active {
        StepVisual {
            fill: selected(),
            stroke: accent(),
            text: on_elev(),
            underline,
        }
    } else {
        StepVisual {
            fill: elev(),
            stroke: divider(),
            text: if done { text() } else { dim() },
            underline,
        }
    }
}

/// 步骤栏的横向尺寸：段间距、段高、点击区下限、理想段宽、右侧计数占位。
///
/// Step-bar metrics: gap, segment height, minimum tap target, ideal segment width and
/// the counter slot on the right.
const STEP_GAP: f32 = 6.0;
const STEP_BAR_H: f32 = 26.0;
const STEP_MIN_TAP: f32 = 44.0;
const STEP_IDEAL_W: f32 = 116.0;
const STEP_COUNTER_W: f32 = 34.0;

/// 按可用宽度算每段宽与是否进紧凑模式。空间不足时先放弃「理想宽度」，
/// 再放弃「44px 点击区下限」，最后才允许段宽继续缩——宁可点击区变小，
/// 也不许把最后一段挤出屏幕。
///
/// 物理下限 `step_min_width(n)`：计数槽 + 全部段间距 + 每段至少 1px。
/// 容器比它还窄时，任何排布都放不下（egui 会裁切），这不是溢出，是空间不够。
///
/// Segment width and compact flag from the available width. When space runs out we give
/// up the ideal width first, then the 44 px tap floor, and only then shrink further -- a
/// smaller tap target beats pushing the last segment off screen.
///
/// Physical floor `step_min_width(n)`: counter slot + all gaps + 1 px per segment. Below
/// that nothing fits (egui clips); that is a lack of space, not an overflow bug.
pub(super) fn step_metrics(avail_w: f32, n: usize) -> (f32, bool) {
    if n == 0 {
        return (STEP_IDEAL_W, false);
    }
    let raw = (avail_w - STEP_COUNTER_W - STEP_GAP * (n as f32 + 1.0)) / n as f32;
    let per = if raw >= STEP_MIN_TAP {
        raw.min(STEP_IDEAL_W)
    } else {
        raw.max(1.0)
    };
    (per, per < 96.0)
}

/// 一排步骤放得下的最小容器宽度：计数槽 + 全部段间距 + 每段 1px。
///
/// The narrowest container that can hold a step row: counter slot + all gaps + 1 px per
/// segment.
pub(super) fn step_min_width(n: usize) -> f32 {
    STEP_COUNTER_W + STEP_GAP * (n as f32 + 1.0) + n as f32
}

/// 中央区当前该画哪一页：三步各自一个独立界面（用户 2026-11-09）。
///
/// Which page the centre area should paint: three steps, three independent views
/// (user, 2026-11-09).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FlowPage {
    Sequence,
    Develop,
    Export,
}

/// 由当前步派生页面：序列管理步 → 缩略图墙；导出步 → 导出工作台；
/// 其余（去闪/对齐/调色）都回到画布。单张与空工程恒为画布。
///
/// Page derived from the current step: sequence step -> thumbnail wall; export step ->
/// export workbench; everything else (deflicker / align / develop) -> canvas. Single
/// photos and an empty project always get the canvas.
pub(super) fn current_page(state: &AppState) -> FlowPage {
    // 防御性守卫：`shell::mod` 的 `!develop` 分支已经把非显影工作台拦下，
    // 正常调用时 bench 必为 Develop。这里再挡一次，是为了将来有人从别处调用
    // `current_page` 时，不会仅凭流程派生步把内容区切成导出页——那种组合下
    // 导航卡高亮「1 显影」而内容区是「导出视频」（用户 2026-11-10 实测）。
    // 主修复在 `ops_session::open_paths`（基线时机 + 清手选步），此处是兜底。
    //
    // Defensive guard: `!develop` in `shell::mod` already routes non-develop
    // benches to their full-page view, so bench is Develop on the normal path.
    // This keeps a future caller of `current_page` from turning the content into
    // the export page purely on a derived flow step -- the combination where the
    // nav card highlights "1 Develop" while the content shows "Export video"
    // (2026-11-10 user report). The primary fix is in `ops_session::open_paths`
    // (baseline timing + clearing the hand-picked step); this is the backstop.
    if state.bench != crate::shell::Bench::Develop || state.frame_count() <= 1 {
        return FlowPage::Develop;
    }
    let task = state.flow_task;
    let steps = task.steps();
    let step = &steps[flow::display_step(state, task).min(steps.len().saturating_sub(1))];
    match step.module {
        Module::Sequence => FlowPage::Sequence,
        Module::Render => FlowPage::Export,
        _ => FlowPage::Develop,
    }
}

pub fn flow_top_bar(state: &mut AppState, ui: &mut Ui) {
    let task = state.flow_task;
    let steps = task.steps();
    let states = flow::step_states(state, task);
    let current = flow::display_step(state, task);
    let p = ui.painter().clone();

    // 统一尺寸：每段等宽等高，右侧计数固定占位；窄屏按可用宽收窄到最小
    // 44px 点击区，并隐藏编号／截字，hover 仍给全名。步骤顺序与点击行为不变。
    //
    // Uniform sizing: equal width and height per segment, fixed counter slot on the
    // right. Narrow windows shrink to a 44 px minimum tap target, hiding the index and
    // truncating the label while hover still shows the full name. Step order and click
    // behaviour are unchanged.
    let n = steps.len();
    let full_w = ui.available_width();
    let (per, compact) = step_metrics(full_w, n);
    // 连「计数槽 + 间距」都放不下时，把右下角的 n/N 让出来给步骤本身：
    // 少一个读数，好过把最后一段挤到屏幕外。
    //
    // When even the counter slot plus gaps do not fit, give the n/N readout up so the
    // steps themselves keep their room -- one missing readout beats an off-screen segment.
    let show_counter = full_w >= step_min_width(n);

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = STEP_GAP;
        for (i, s) in steps.iter().enumerate() {
            let done = states[i] == flow::StepState::Ready;
            let name = crate::i18n::tr(s.label);
            // 序号与完成标记改由左侧圆承担，文字只留名称——「✓ 1 序列管理」
            // 这种三重前缀挤在一起，读起来像注释不像导航。
            //
            // The index and the done mark move into the leading circle; the text keeps
            // only the name. "check 1 sequence" reads like a comment, not navigation.
            // 圆占掉 27px，文字只能用剩下的宽度：超出就截断，否则窄屏会压到
            // 相邻段（ui_tokens 的文本重叠判据会红）。
            //
            // The circle eats 27 px; the label gets the rest and is truncated, or narrow
            // layouts would bleed into the neighbouring segment.
            let text_w = (per - 27.0 - 10.0).max(12.0);
            let max_chars = (text_w / SIZE_BODY).max(1.0) as usize;
            let label = if crate::i18n::tr(s.label).chars().count() > max_chars {
                crate::theme::ellipsis(name, max_chars)
            } else {
                name.to_owned()
            };
            // 极窄屏：按段宽截字，避免文字压到边框外。
            let label = if compact {
                let max_chars = ((per - 12.0) / SIZE_BODY).max(2.0) as usize;
                crate::theme::ellipsis(&label, max_chars)
            } else {
                label
            };
            let rect = Rect::from_min_size(ui.cursor().left_top(), Vec2::new(per, STEP_BAR_H));
            ui.allocate_space(Vec2::new(per, STEP_BAR_H));
            let resp = ui.interact(rect, ui.make_persistent_id(("flow-tab", i)), Sense::click());
            let v = step_visual(
                done,
                i == current,
                resp.hovered(),
                resp.is_pointer_button_down_on(),
                true,
            );
            // 去段级胶囊（用户 2026-11-11 二次反馈「悬浮设计不好看」）：
            // 非激活段**无底**——平底条上只留 编号圆+文字，hover 才给 4% 白；
            // 激活段保留 bevel 实底+强调描边（唯一的实心段=当前步，视线锚点）。
            if v.fill == selected() {
                crate::theme::surface_bevel(&p, rect, v.fill);
            } else if resp.hovered() {
                p.rect_filled(rect, radius_control(), Color32::from_white_alpha(10));
            }
            p.rect_stroke(
                rect,
                radius_control(),
                Stroke::new(1.0, v.stroke),
                egui::StrokeKind::Outside,
            );
            if v.underline {
                // 强调线贴在按钮**内**底边：旧实现用 left_bottom + bar_h 把线画到
                // 了按钮下方 26px 处，等于看不见——这就是「没有点击效果」的根因。
                //
                // The accent line sits on the button's **inner** bottom edge. The old
                // implementation added bar_h to left_bottom, drawing it 26 px below the
                // button -- effectively invisible, hence "no click effect".
                // 下划线从中心展开（0.12s）：状态切换「跳」一下是数字感的来源；
                // 一次只动一个属性（宽度），不做位移/缩放/发光。
                //
                // The underline grows from the centre over 0.12 s: a state change that
                // snaps is where the digital feel comes from. Only one property moves
                // (width) -- no shift, scale or glow.
                let t = ui.ctx().animate_bool_with_time(
                    ui.make_persistent_id(("flow-tab-underline", i as u8)),
                    true,
                    0.12,
                );
                let half = (per - 12.0) * 0.5 * t;
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(rect.center().x - half, rect.bottom() - 2.0),
                        Pos2::new(rect.center().x + half, rect.bottom()),
                    ),
                    1.0,
                    accent().gamma_multiply(t.max(0.15)),
                );
            }
            // 序号圆：导航的主锚点。完成=强调色暗底✓，当前=强调色实底序号，
            // 未到=升一档底 + 次级字。形状 + 位置 + 颜色三重编码，不只靠颜色。
            //
            // Leading number circle: the navigation anchor. Done = accent-dim with a
            // check, current = solid accent, todo = elevated fill with secondary text.
            // Shape + position + colour, not colour alone.
            let d = 18.0;
            let circle = Rect::from_center_size(
                Pos2::new(rect.left() + 9.0 + d * 0.5, rect.center().y),
                Vec2::splat(d),
            );
            let (cfill, ctext) = if i == current {
                (accent(), on_accent())
            } else if done {
                (accent_dim(), on_elev())
            } else {
                (elev_hover(), muted())
            };
            p.circle_filled(circle.center(), d * 0.5, cfill);
            p.text(
                circle.center(),
                egui::Align2::CENTER_CENTER,
                // 三步是导航，不是进度条：完成态只用底色区分，不再画 ✓。
                // 派生完成态与手选步会互相矛盾（导入后显示「✓调色与修复」，
                // 但高亮仍停在第一段），用户 2026-11-09 反复指出。
                //
                // Three steps are navigation, not a progress bar: done is shown by the
                // fill only. A check contradicts the hand-picked highlight.
                format!("{}", i + 1),
                font(SIZE_SMALL),
                ctext,
            );
            let galley = ui
                .painter()
                .layout_no_wrap(label.clone(), font(SIZE_BODY), v.text);
            p.galley(
                Pos2::new(
                    circle.right() + 7.0,
                    rect.center().y - galley.size().y * 0.5,
                ),
                galley,
                v.text,
            );
            let info = match states[i] {
                flow::StepState::Ready => crate::i18n::tr("输入已就绪"),
                flow::StepState::Current => crate::i18n::tr("下一步"),
                flow::StepState::Todo => crate::i18n::tr("尚未开始"),
            };
            let resp = resp.on_hover_text(format!("{} · {}", name, info));
            if resp.clicked() {
                state.flow_step = Some(i);
                state.set_module(s.module);
            }
        }
        if show_counter {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{}/{}", current + 1, steps.len()))
                        .size(SIZE_SMALL)
                        .color(muted()),
                );
            });
        }
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
    use super::*;

    /// 五态表：默认 / 悬停 / 按下 / 选中 / 禁用两两可分，且都走 token。
    /// 这是「仅调整样式」的量化护栏——任何一个态被合并，视觉反馈就丢一个。
    ///
    /// Five-state table: default / hover / pressed / selected / disabled are pairwise
    /// distinguishable. This is the measurable guard for "style-only changes" -- merging
    /// any two states removes one piece of visual feedback.
    #[test]
    fn step_visual_keeps_five_distinct_states() {
        let cases: [(&str, StepVisual); 5] = [
            ("default", step_visual(false, false, false, false, true)),
            ("hover", step_visual(false, false, true, false, true)),
            ("pressed", step_visual(false, false, false, true, true)),
            ("active", step_visual(false, true, false, false, true)),
            ("disabled", step_visual(false, false, false, false, false)),
        ];
        for i in 0..cases.len() {
            for j in (i + 1)..cases.len() {
                let (na, a) = cases[i];
                let (nb, b) = cases[j];
                assert_ne!(a, b, "{na} 与 {nb} 视觉状态撞车");
            }
        }
        // 完成态与未完成态文字层级不同（✓ 段更亮）。
        assert_eq!(
            step_visual(true, false, false, false, true).text,
            text(),
            "完成段应用正文色"
        );
        assert_eq!(
            step_visual(false, false, false, false, true).text,
            dim(),
            "未到段应用次级色"
        );
        // 强调线只在选中段出现；禁用段永远不带强调线。
        assert!(step_visual(false, true, false, false, true).underline);
        assert!(!step_visual(false, false, false, false, true).underline);
        assert!(!step_visual(false, true, false, false, false).underline);
        // 按下态对选中段同样生效（不能只在非选中段可见）。
        assert_eq!(
            step_visual(false, true, false, true, true).fill,
            press(),
            "选中段按下时应换成 press 底"
        );
    }

    /// 步骤栏尺寸：任何可用宽下总宽都不超出可用宽；宽窗取理想宽，
    /// 极窄时先丢理想宽、再丢 44px 点击区下限，绝不把最后一段挤出屏幕。
    ///
    /// Step-bar metrics: the total never exceeds the available width at any size; wide
    /// windows use the ideal width, and very narrow ones give up the ideal width first,
    /// then the 44 px tap floor -- never pushing the last segment off screen.
    #[test]
    fn step_metrics_never_overflow_the_available_width() {
        for n in 1..=6usize {
            for avail in [0.0f32, 40.0, 120.0, 300.0, 600.0, 900.0, 1600.0] {
                let (per, compact) = step_metrics(avail, n);
                assert!(per > 0.0, "n={n} avail={avail} 段宽必须为正");
                assert_eq!(compact, per < 96.0, "紧凑阈值与段宽不一致");
                // 低于物理下限时放不下是空间问题，不是溢出 bug；只要求不 panic。
                if avail < step_min_width(n) {
                    continue;
                }
                let total = per * n as f32 + STEP_GAP * (n as f32 + 1.0) + STEP_COUNTER_W;
                assert!(
                    total <= avail + 0.01,
                    "n={n} avail={avail} 总宽 {total} 溢出可用宽"
                );
            }
        }
        // 宽窗：理想宽封顶，不会无限拉长。
        let (wide, _) = step_metrics(3000.0, 3);
        assert!((wide - STEP_IDEAL_W).abs() < 0.01, "宽窗应封顶在理想宽");
        // 空步骤：不 panic，给理想宽。
        assert_eq!(step_metrics(800.0, 0).0, STEP_IDEAL_W);
    }

    /// 只调样式的契约：步骤顺序、点击写入、状态派生、模块跳转四件事原样保留。
    ///
    /// The style-only contract: step order, click write, state derivation and module
    /// jump all stay byte-for-byte in the product half of this file.
    #[test]
    fn top_bar_keeps_the_original_interaction_contract() {
        let code = include_str!("flow_panel.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or("");
        assert!(
            code.contains("for (i, s) in steps.iter().enumerate()"),
            "步骤顺序被改了"
        );
        assert_eq!(
            code.matches("state.flow_step = Some(i);").count(),
            1,
            "点击写入手选态应只剩顶栏一条"
        );
        assert!(
            code.contains("state.set_module(s.module);"),
            "点击后的模块跳转被改了"
        );
        assert!(
            code.contains("flow::step_states(state, task)"),
            "三态派生被改了"
        );
        assert!(
            code.contains("flow::display_step(state, task)"),
            "当前步派生被改了"
        );
        assert!(
            code.contains("resp.is_pointer_button_down_on()"),
            "按下态没有接上"
        );
        assert!(
            code.contains("rect.bottom() - 2.0"),
            "强调线必须贴在按钮内底边"
        );
    }

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
        // 唯一写入口是顶部页签：左侧面板的竖排步骤列表已删（与顶栏重复，
        // 用户 2026-11-09 明确指认）。写入数回到 1 就是去重生效的证据。
        assert_eq!(
            code.matches("state.flow_step = Some(i);").count(),
            1,
            "手选写入应只剩顶部页签一条"
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
