//! 面板公共件：预设栏、侧栏行、组头、滑杆、键值行（被 adjust/mask/dock 引用）。
//!
//! Shared panel pieces: preset column, sidebar rows, group headers, sliders,
//! key-value rows (used by adjust / mask / dock).

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::INSET_RIM;
use super::MICRO;
use super::adjust::reset_group;
use crate::controls;
use crate::history;
use crate::icons::{self, Glyph};
use crate::state::AppState;
use crate::theme::{self, *};
use crate::widgets;
use aevum_core::grade::{self};
use egui::{
    Align, Align2, Color32, Layout, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, UiBuilder,
    Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

pub(super) fn preset_panel(state: &mut AppState, ui: &mut Ui) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.line_segment(
        [full.right_top(), full.right_bottom()],
        Stroke::new(1.0, border()),
    );

    // 栏头与右栏页签同高：左右两条侧栏的顶边必须落在同一条横线上。
    // 底色压成 header、字用小字档 dim —— 这一栏是"账本"，抬头不许抢条目的戏。
    let head = Rect::from_min_size(full.min, Vec2::new(full.width(), TAB_H));
    p.rect_filled(head, 0, header());
    p.line_segment(
        [head.left_bottom(), head.right_bottom()],
        Stroke::new(1.0, divider()),
    );
    p.text(
        Pos2::new(head.left() + 12.0, head.center().y),
        Align2::LEFT_CENTER,
        crate::i18n::t!("预设"),
        font(SIZE_SMALL),
        dim(),
    );
    let plus = Rect::from_center_size(
        Pos2::new(head.right() - 16.0, head.center().y),
        Vec2::splat(22.0),
    );
    let resp = ui.interact(plus, ui.make_persistent_id("snap-add"), Sense::click());
    if resp.hovered() {
        p.rect_filled(plus, radius_control(), elev());
    }
    icons::paint(
        &p,
        plus.shrink(7.0),
        Glyph::Plus,
        if resp.hovered() { text() } else { muted() },
    );
    let add = resp.clicked();
    resp.on_hover_text(crate::i18n::t!("存快照：把当前整套参数钉一版"));
    if add {
        state.save_snapshot();
    }

    let body = Rect::from_min_max(full.min + Vec2::new(0.0, TAB_H), full.max);
    let mut want_preset: Option<&'static str> = None;
    let mut want_snap: Option<usize> = None;
    let mut drop_snap: Option<usize> = None;
    let mut want_jump: Option<usize> = None;
    let n_steps = state.history.steps().len();
    let cursor = state.history.cursor();
    let snapshot_n = state.snapshots.len();
    // 每一节的节头矩形：拖拽落地时用它判「指针落在哪一节」。
    //
    // Header rect per section, used to decide which section the pointer is over when a
    // dragged section is dropped.
    let mut bands: Vec<(&'static str, Rect)> = Vec::new();

    ui.scope_builder(
        UiBuilder::new()
            .id_salt("preset-body")
            .max_rect(body.shrink2(Vec2::new(0.0, 4.0)))
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            egui::ScrollArea::vertical()
                .id_salt("preset-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;

                    // 三节按 `state.preset_order` 排（可拖）。**不在这里写死次序**：
                    // 那份次序是用户摆的，落盘在 settings.json，读回时已经规整成一个
                    // 完整排列（见 `state::normalise_preset_order`）。
                    //
                    // The three sections follow `state.preset_order` (draggable). The order
                    // is *not* hard-coded here: it belongs to the user, persisted in
                    // settings.json, and normalised into a full permutation on load.
                    for (n, key) in state.preset_order.clone().into_iter().enumerate() {
                        if n > 0 {
                            side_divider(ui);
                        }
                        let (title, count) = match key {
                            "builtin" => ("内置", grade::PRESET_NAMES.len()),
                            "snapshots" => ("快照", snapshot_n),
                            _ => ("历史", n_steps),
                        };
                        let dragging = state.drag_section == Some(key);
                        let head = side_head(ui, title, count, dragging);
                        if head.drag_started() {
                            state.drag_section = Some(key);
                        }
                        bands.push((key, head.rect));

                        match key {
                            "builtin" => {
                                for name in grade::PRESET_NAMES {
                                    if side_row(
                                        ui,
                                        name,
                                        Some(Glyph::Sliders),
                                        state.preset == *name,
                                    )
                                    .clicked()
                                    {
                                        want_preset = Some(name);
                                    }
                                }
                            }
                            "snapshots" => {
                                if snapshot_n == 0 {
                                    side_note(ui, "右上角 ＋ 存一版");
                                }
                                for (i, (label, _)) in state.snapshots.iter().enumerate() {
                                    let row = side_row(ui, label, Some(Glyph::Star), false);
                                    let x_rect = Rect::from_center_size(
                                        Pos2::new(row.rect.right() - 14.0, row.rect.center().y),
                                        Vec2::splat(18.0),
                                    );
                                    let xr = ui.interact(
                                        x_rect,
                                        ui.make_persistent_id(("snap-del", i)),
                                        Sense::click(),
                                    );
                                    let show = row.hovered() || xr.hovered();
                                    if show {
                                        icons::paint(
                                            ui.painter(),
                                            x_rect.shrink(5.0),
                                            Glyph::Close,
                                            if xr.hovered() { danger() } else { dim() },
                                        );
                                    }
                                    if row.clicked() {
                                        want_snap = Some(i);
                                    } else if xr.clicked() {
                                        drop_snap = Some(i);
                                    }
                                }
                            }
                            _ => {
                                if n_steps == 0 {
                                    side_note(ui, "还没有可撤的改动");
                                }
                                // 最新在上：翻历史是为了"回到刚才那一步"，不是从头读流水账。
                                for (i, step) in
                                    state.history.steps().iter().enumerate().rev().take(80)
                                {
                                    if side_row(ui, &step.label, None, i == cursor).clicked() {
                                        want_jump = Some(i);
                                    }
                                }
                            }
                        }
                    }
                });
        },
    );

    // 拖拽落地：指针落在哪一节的范围里，就把正在拖的那一节挪到那个位置。
    //
    // 范围取「本节节头顶 → 下一节节头顶」，比只认 32px 的节头宽容得多；最后一节没有
    // 下一节，下界给到无穷远。松手（`any_down` 变假）时收尾，无论有没有移动。
    //
    // Drop handling: whichever section's range holds the pointer becomes the target index.
    // The range spans "this header's top to the next header's top", far more forgiving than
    // the 32px header alone; the last section has no next one, so its bottom is unbounded.
    // Released (`any_down` false) ends the drag whether or not anything moved.
    if let Some(key) = state.drag_section {
        if !ui.ctx().input(|i| i.pointer.any_down()) {
            state.drag_section = None;
        } else if let Some(pos) = ui.ctx().pointer_latest_pos()
            && let Some(target) = bands.iter().enumerate().position(|(i, (k, r))| {
                if *k == key {
                    return false;
                }
                let top = r.top();
                let bottom = bands
                    .get(i + 1)
                    .map_or(f32::INFINITY, |(_, next)| next.top());
                pos.y >= top && pos.y < bottom
            })
        {
            state.move_preset_section(key, target);
        }
    }

    if let Some(name) = want_preset {
        state.apply_preset(name);
    }
    if let Some(i) = want_snap {
        state.restore_snapshot(i);
    }
    if let Some(i) = drop_snap {
        state.delete_snapshot(i);
    }
    if let Some(i) = want_jump
        && let Some(mv) = state.history.jump(i)
    {
        state.restore(mv.snap);
    }
}

/// 侧栏两节之间的软分割线：线短一截、压暗一档，读作「分节」而不是「切开」。
fn side_divider(ui: &mut Ui) {
    ui.add_space(5.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    theme::soft_rule(ui.painter(), rect.left(), rect.right(), rect.center().y);
    ui.add_space(5.0);
}

/// 一节的小标题：▾ 名字 ……… 条数。32px 高、组名 12px 正文色，对齐界面稿 .sec-hd。
///
/// 节头同时是**拖拽手柄**（`Sense::click_and_drag`）：按住它把整节往上/往下拖就能
/// 换序。`dragging` 为真时铺一层 `elev()` 底，让用户看得出「正在搬的是这一节」。
///
/// The header doubles as the **drag handle** (`Sense::click_and_drag`): hold it and drag
/// the whole section up or down to reorder. When `dragging` is true an `elev()` fill marks
/// which section is being carried.
fn side_head(ui: &mut Ui, label: &str, count: usize, dragging: bool) -> Response {
    let label = crate::i18n::tr(label);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 32.0),
        Sense::click_and_drag(),
    );
    if dragging {
        ui.painter().rect_filled(rect, 0, elev());
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    let p = ui.painter();
    p.text(
        Pos2::new(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        "▼",
        font(MICRO),
        dim(),
    );
    p.text(
        Pos2::new(rect.left() + 23.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font(SIZE_LABEL),
        text(),
    );
    p.text(
        Pos2::new(rect.right() - 12.0, rect.center().y),
        Align2::RIGHT_CENTER,
        count.to_string(),
        font(SIZE_SMALL),
        dim(),
    );
    resp
}

/// 一列可点的行。`glyph` 为 None 时画一枚小圆点（历史行）。
///
/// 选中态对齐界面稿 `.li.on`：横向渐变蓝底（左深右浅）+ 左缘 2px 强调色竖条，
/// 行字与图标一起翻成选中底上的白。
fn side_row(ui: &mut Ui, label: &str, glyph: Option<Glyph>, active: bool) -> Response {
    let label = crate::i18n::tr(label);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    let p = ui.painter();
    if active {
        // egui 没有横向渐变矩形：把行拆成 8 片，从 selected 线性插到
        // selected_row_end（贴着 dock 的一档）。8 片在这条 26px 的行上看不出接缝。
        const SLICES: f32 = 8.0;
        let a = selected();
        let b = selected_row_end();
        let slice_w = rect.width() / SLICES;
        for k in 0..SLICES as usize {
            let t = k as f32 / SLICES;
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(rect.left() + slice_w * k as f32, rect.top()),
                    Pos2::new(
                        (rect.left() + slice_w * (k as f32 + 1.0) + 0.6).min(rect.right()),
                        rect.bottom(),
                    ),
                ),
                0.0,
                widgets::mix(a, b, t),
            );
        }
        // 左缘 2px 竖条：光带的"光源"压在最左边。
        p.rect_filled(
            Rect::from_min_max(rect.min, Pos2::new(rect.left() + 2.0, rect.bottom())),
            0.0,
            accent(),
        );
    } else if resp.hovered() {
        p.rect_filled(rect, 0.0, theme::row_hover());
    }
    let x = rect.left() + 32.0;
    match glyph {
        Some(g) => icons::paint(
            p,
            Rect::from_min_size(
                Pos2::new(rect.left() + 12.0, rect.center().y - 6.0),
                Vec2::splat(12.0),
            ),
            g,
            if active { on_selected() } else { dim() },
        ),
        None => {
            p.circle_filled(
                Pos2::new(rect.left() + 17.0, rect.center().y),
                1.6,
                if active { on_selected() } else { rail_fill() },
            );
        }
    }
    let ink = if active {
        on_selected()
    } else if resp.hovered() {
        text()
    } else {
        muted()
    };
    let title = theme::fit_text(p, label, font(SIZE_BODY), ink, rect.right() - x - 22.0);
    p.text(
        Pos2::new(x, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        font(SIZE_BODY),
        ink,
    );
    resp
}

/// 空节的一行说明：比正文小一档、比 dim 再灰一档，不抢条目位置。
fn side_note(ui: &mut Ui, text: &str) {
    let text = crate::i18n::tr(text);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font(SIZE_SMALL),
        dim(),
    );
}

/// 分组之间只留白，不画线框（macOS 统一工具栏口径）。
pub(super) fn group_gap(ui: &mut Ui) {
    ui.add_space(12.0);
}

/// 选项栏的分段竖线（界面稿 .opt-sep）：1 × 20 的 divider 竖线，
/// 把「工具身份 / 工具参数 / 上下文」三段隔开 —— 不用线的话，
/// 参数一多就糊成一条连续的按钮带。
pub(super) fn opt_sep(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, 20.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, divider());
    ui.add_space(4.0);
}

/// 分组的说明只在悬停 ⓘ 时出现：正文留给控件，说明按「做什么、口径、代价」三句写，
/// 不再把半截话铺在面板上。
pub(super) fn group_help(key: &str) -> Option<&'static str> {
    Some(match key {
        "copies" => {
            "保存当前调色与关键帧为独立副本，可应用、覆盖或删除；导出与队列按名称选用，互不影响。"
        }
        "hdr" => "取当前帧前后各 N 帧的不同曝光，对齐后融合为单张 32 位浮点图像。",
        "pano" => "以当前帧为起点向后取相邻帧，经特征匹配与单应变换融合为宽幅图像。",
        "basic" => {
            "整体亮度与影调范围。曝光以 EV 计，其余参数相对中间调调整。\n高光过渡（对齐 LR）：曝光推过 1 的区域走有理肩部缓收，不再硬剪。\n线性对比（对齐 LR）：对比度改在线性光域计算，暗部响应更接近 LR。\n黑场/白场软化（对齐 LR）：两端用抛物线软拐点收进 [0,1]，直方图边缘不再堆尖。"
        }
        "color" => "白平衡与色彩浓度。色温、色调的轨道直接表示冷暖与绿品方向。",
        "effects" => "去雾、暗角与暗角高光保护。",
        "look" => "载入 .cube 3D LUT 作为外观层，不改动底层调色参数。",
        "levels" => "输入黑白场、灰度系数与输出范围，在色阶卡上直接拖动。",
        "parametric" => "按亮度区间（阴影、中间调、高光）调整影调曲线。",
        "curve" => {
            "RGB 与单通道点曲线，坐标即输入输出值。\nHDR 尾部（对齐 LR）：x>1 的过曝层次按曲线末端斜率延伸进曲线，不再硬截。"
        }
        "hsl" => "按颜色范围调整色相、饱和度与明亮度。",
        "mono" => "黑白混合：各色相在灰度中的权重。",
        "grade" => "阴影、中间调、高光四向色彩分级，可分别指定色轮与亮度。",
        "detail" => {
            "锐化、降噪与蒙版。\n锐化保护（对齐 LR）：按噪声底自适应保护平坦区，与蒙版叠加生效。"
        }
        "grain" => {
            "按尺寸与粗糙度叠加胶片颗粒。\n颗粒亮度（对齐 LR）：颗粒强度随亮度分级，正值暗部更重、负值反转（高光更重）。"
        }
        "profiles" => "相机 DCP 与镜头 LCP 配置文件，可另选 lensfun 库。",
        "lens" => "暗角、色差与畸变校正。",
        "masks" => {
            "局部调整：线性、径向与画笔蒙版，参数独立于全局调色。选中后在画布上拖两端、圆心与半径，或用画笔涂抹；画笔大小按短边百分比计。"
        }
        "frame" => {
            "画幅、旋转与拉直。拉直后安全框自动内切，框外空白不进导出；裁剪里可拖框、拉把手或按方向键微调，所见即所得。"
        }
        "frame-guides" => "三分线、中心十字与前后分割，仅作取景参考，不写入导出。",
        "render-out" => "输出规格、目录、格式与色彩空间。",
        "render-watermark" => {
            "导出时叠加文字或图片水印。尺寸按输出长边百分比、边距按短边，盖在缩放与输出锐化之后、色域搬运之前；预览不叠这一层。"
        }
        "render-time" => "成片时长与帧率。",
        "render-retime" => {
            "压帧是真丢帧，拆帧是相邻两帧交叉淡化，不做光流补帧。快门决定一帧混进多少邻居；关闭时导出走 1:1 快路。曲线上点空白加点、双击删点，横轴即序列位置。"
        }
        "render-motion" => {
            "起点与终点取景框按缓动插值，即成一条镜头运动。框钉在输出画幅比例上，方向键微调、按住 Shift 加大步长；关闭或两框重合时导出走 1:1 快路。"
        }
        "render-codec" => "编码器、质量与容器。",
        "render-export" => {
            "导出成片或静帧序列；单帧与序列共用上面的容器、色域、锐化与 EXIF 设定，成片经 ffmpeg 写入 BT.709 标记。"
        }
        "render-queue" => {
            "多段导出排队执行，逐项显示进度。每段读取加入时的工程参数，排队后再改调色不必重排。"
        }
        "audio" => "配乐文件、起点与淡入淡出；延迟用 adelay 补真静音采样，淡出落在配乐收尾处。",
        "audio-mix" => "成片长度与音频编码。画面与配乐在同一趟 ffmpeg 里合成，先写帧再按容器封装。",
        "seq-qa" => {
            "查坏帧、黑帧、虚焦、重复、亮度跳变与编号缺口；判定与左右邻居比较，整体渐变不误报。体检读导入时算好的测光，不再解码。"
        }
        "seq-align" => "以中段帧为参考对齐全序列，输出统一裁切与位移。",
        "seq-spec" => "测光、去闪与预览的固定口径，数值以 Python 金标为准。",
        "deflicker" => "按窗口平滑全序列亮度，抑制曝光闪烁。",
        "def-exposure" => "在去闪结果之上再叠加统一的曝光修正，单位为 EV；画布与导出同口径。",
        "info-meta" => "说明与版权四项随工程保存；导出静帧写入 EXIF，成片写入容器标签。",
        "keys" => "记录参数快照，导出时按时间插值。",
        "ai-link" => "本机回环控制：外部 agent 通过 MCP 调用同一套命令。",
        _ => return None,
    })
}

pub(super) fn group_header(
    ui: &mut Ui,
    state: &mut AppState,
    key: &'static str,
    title: &str,
    resettable: bool,
) -> bool {
    let title = crate::i18n::tr(title);
    // 分组之间留一口气：规则线紧贴上一组内容时，滚动起来像一整块密排表。
    ui.add_space(4.0);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
    if state.dock_focus.as_deref() == Some(key) {
        // 只推一次：egui 在下一帧把目标滚到位，截图时机在 900ms 之后，足够稳定。
        resp.scroll_to_me(Some(Align::Min));
        state.dock_focus = None;
        // 顺带展开：demo 编辑会把没动的分组折起来，只滚不展等于对着一张
        // 折叠的分组头截图，面板里那条渐变轨根本没上屏。
        state.collapsed.remove(key);
    }
    let p = ui.painter();
    theme::rule(p, rect.left(), rect.right(), rect.top(), border());
    let open = !state.collapsed.contains(key);
    // 悬停是一整条圆角高亮，不是方条：macOS 侧栏分组头的口径。
    if resp.hovered() {
        p.rect_filled(rect.shrink(2.0), radius_control(), elev());
    }
    icons::paint(
        p,
        Rect::from_center_size(
            Pos2::new(rect.left() + 9.0, rect.center().y),
            Vec2::splat(9.0),
        ),
        if open {
            Glyph::ChevronDown
        } else {
            Glyph::ChevronRight
        },
        if resp.hovered() { text() } else { dim() },
    );
    let title_galley = p.layout_no_wrap(title.to_owned(), font(SIZE_LABEL + 0.5), text());
    let title_x = rect.left() + 20.0;
    p.galley(
        Pos2::new(title_x, rect.center().y - title_galley.size().y * 0.5),
        title_galley.clone(),
        text(),
    );
    // 说明入口贴着标题放：右端只留复位，两个圆图标挤在一起分不清谁是谁。
    if let Some(help) = group_help(key).map(crate::i18n::tr) {
        let ir = Rect::from_center_size(
            Pos2::new(title_x + title_galley.size().x + 11.0, rect.center().y),
            Vec2::splat(14.0),
        );
        let iresp = ui.interact(
            ir,
            ui.make_persistent_id(("group-help", key)),
            Sense::hover(),
        );
        if iresp.hovered() {
            p.rect_filled(ir, radius_control(), elev_hover());
        }
        icons::paint(
            p,
            ir.shrink(4.0),
            Glyph::Info,
            if iresp.hovered() {
                accent_hover()
            } else {
                dim()
            },
        );
        iresp.on_hover_text(help);
    }
    // 组内改动指示（界面稿 .grp-hd .dot）：组里有参数偏离默认，复位按钮左侧
    // 亮一枚 5px accent 圆点；复位之后自然熄灭。
    if state.group_changed(key) {
        p.circle_filled(
            Pos2::new(
                rect.right() - if resettable { 26.0 } else { 12.0 },
                rect.center().y,
            ),
            2.5,
            accent(),
        );
    }
    if resettable {
        let rr = Rect::from_center_size(
            Pos2::new(rect.right() - 11.0, rect.center().y),
            Vec2::splat(16.0),
        );
        let rresp = ui.interact(
            rr,
            ui.make_persistent_id(("group-reset", key)),
            Sense::click(),
        );
        if rresp.hovered() {
            p.rect_filled(rr, radius_control(), elev_hover());
        }
        icons::paint(
            p,
            rr.shrink(4.0),
            Glyph::Reset,
            if rresp.hovered() { text() } else { dim() },
        );
        if rresp.clicked() {
            reset_group(state, key);
        }
    }
    if resp.clicked() {
        if state.collapsed.contains(key) {
            state.collapsed.remove(key);
        } else {
            state.collapsed.insert(key);
        }
        state.settings_dirty = true;
    }
    // 右键：Lightroom 的「Solo Mode」那套。面板摊开十来个组之后，逐个点头收起来
    // 太笨；右键标题条给三个动作，全部展开 / 全部收起 / 只看这一组。
    // 不做成常驻按钮：这三个动作一周用一次，占一条像素都是负担。
    resp.context_menu(|ui| {
        if ui.button(crate::i18n::t!("只看这一组")).clicked() {
            state.solo_group(key);
            ui.close();
        }
        if ui.button(crate::i18n::t!("全部收起")).clicked() {
            state.collapse_all_groups();
            ui.close();
        }
        if ui.button(crate::i18n::t!("全部展开")).clicked() {
            state.expand_all_groups();
            ui.close();
        }
    });
    ui.add_space(4.0);
    !state.collapsed.contains(key)
}

/// 数值输入域的材质口径：`DragValue` 内部其实是一颗按钮，默认拿到的是 ELEV；
/// 这里换成 macOS 的文本域（比材质暗一档的 FIELD + 悬停亮一档），
/// 或在外面已经画好 FIELD 框时直接透明，保证全界面只有一套输入材质。
pub(super) fn in_field<R>(
    ui: &mut Ui,
    base: Color32,
    hover: Color32,
    run: impl FnOnce(&mut Ui) -> R,
) -> R {
    let old = ui.style().visuals.widgets.clone();
    {
        let w = &mut ui.style_mut().visuals.widgets;
        for s in [&mut w.inactive, &mut w.open, &mut w.active, &mut w.hovered] {
            s.bg_fill = base;
            s.weak_bg_fill = base;
            s.bg_stroke = Stroke::NONE;
        }
        w.hovered.bg_fill = hover;
        w.hovered.weak_bg_fill = hover;
    }
    let out = run(ui);
    ui.style_mut().visuals.widgets = old;
    out
}

/// 面板推子。参数一一对应界面上一条推子的各个要素，打包成结构体只会让调用点更绕，
/// 所以这里放开 `too_many_arguments`。
#[allow(clippy::too_many_arguments)]
pub(super) fn slider<K: std::hash::Hash + std::fmt::Debug>(
    ui: &mut Ui,
    key: K,
    label: &str,
    v: &mut f64,
    min: f64,
    max: f64,
    default: f64,
    decimals: usize,
    signed: bool,
) -> bool {
    slider_row(
        ui, key, label, v, min, max, default, decimals, signed, "", None, false, "",
    )
}

#[allow(clippy::too_many_arguments)]
/// 按规格表渲染一条推子。标签/范围/默认/精度/单位/渐变全部来自
/// `controls::SPECS`——加参数只登记表，不再在面板里摊九个实参。
pub(super) fn spec_slider(ui: &mut Ui, state: &mut AppState, s: &controls::ControlSpec) -> bool {
    let slot = controls::param_slot(&mut state.params, s.field);
    slider_row(
        ui,
        s.key,
        s.label,
        slot,
        s.min,
        s.max,
        s.default,
        s.decimals,
        s.signed,
        s.unit,
        controls::grad_stops(s),
        s.track,
        s.tip,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn slider_row<K: std::hash::Hash + std::fmt::Debug>(
    ui: &mut Ui,
    key: K,
    label: &str,
    v: &mut f64,
    min: f64,
    max: f64,
    default: f64,
    decimals: usize,
    signed: bool,
    unit: &str,
    stops: Option<[Color32; 2]>,
    track: bool,
    tip: &str,
) -> bool {
    // ── 单行横排：标签 76 · 轨道弹性 · 读数 52 ───────────────────────────
    // 设计稿 `.row` 的口径。旧版把标签压在轨道上方，36px 的行高里排两行，
    // 一屏八行看下来标签和推子互相咬住，读起来像一张密排表格而不是调整面板。
    // 翻译下沉在这里：SPECS 静态表的 label 是 const 数据，不能在表里调 tr；
    // 所有滑块（显影/构图/成片）都汇到这一条路上，改一处全生效。
    const LABEL_W: f32 = 76.0;
    const VALUE_W: f32 = 52.0;
    const RESET_W: f32 = 14.0;
    let label = crate::i18n::tr(label);
    let tip = crate::i18n::tr(tip);

    let (rect, row_resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h()), Sense::hover());
    let hot_row = ui.rect_contains_pointer(rect);
    let changed_now = (*v - default).abs() > 1e-9;

    // 数值框：右端固定 52，右对齐 —— 一列读数的小数点才会落在同一条竖线上。
    let field = Rect::from_min_size(
        Pos2::new(rect.right() - VALUE_W, rect.center().y - 10.0),
        Vec2::new(VALUE_W, 20.0),
    );
    // 复位点夹在轨道与读数之间：偏离默认值才现形，平时把位置让给轨道。
    let reset = Rect::from_center_size(
        Pos2::new(field.left() - RESET_W * 0.5 - 2.0, rect.center().y),
        Vec2::splat(RESET_W),
    );
    let rail_left = rect.left() + LABEL_W + 4.0;
    let rail_right = (reset.left() - 3.0).max(rail_left + 1.0);
    let track_y = rect.center().y;
    {
        let p = ui.painter();
        if hot_row {
            // 通栏轻微提亮：一行 36px，只提 7/255，够指认、不打断纵向扫读。
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(rect.left() - 6.0, rect.top() + 1.0),
                    Pos2::new(rect.right() + 6.0, rect.bottom() - 1.0),
                ),
                radius_control(),
                row_hover(),
            );
        }
        if ui.rect_contains_pointer(field) {
            p.rect_filled(field, radius_control(), field_hover());
        }
        // 6 字截断：5 字中文在 76px 里刚好排满，再长就让位给轨道，
        // 宁可少两个字，也不让标签爬进推子的地盘。
        // 截断长度按语言分档：中文 6 字约 78px 恰好贴住 LABEL_W，英文同宽只放得
        // 下 ~11 个半角字符，仍按 6 截会把 "Highlights" 截成 "Highli…"。
        // 档位存在语言表里（`Catalog::ellipsis_chars`），加语言时由那张表自己声明，
        // 这里不再出现具体语言的名字。
        p.text(
            Pos2::new(rect.left() + 2.0, rect.center().y),
            Align2::LEFT_CENTER,
            theme::ellipsis(label, crate::i18n::current().ellipsis_chars()),
            font(SIZE_LABEL),
            if changed_now { text() } else { muted() },
        );
    }

    let old_text = ui.style().visuals.override_text_color;
    ui.style_mut().visuals.override_text_color =
        Some(if changed_now { accent_hover() } else { value() });
    let mut resp = in_field(ui, Color32::TRANSPARENT, Color32::TRANSPARENT, |ui| {
        ui.scope_builder(
            UiBuilder::new()
                .id_salt(("slider-value", &key))
                .max_rect(field.shrink2(Vec2::new(2.0, 0.0)))
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                ui.add(
                    egui::DragValue::new(v)
                        .range(min..=max)
                        .speed((max - min) / 300.0)
                        .fixed_decimals(decimals)
                        .custom_formatter(move |n, _| theme::number(n, decimals, signed))
                        .suffix(unit.to_owned()),
                )
            },
        )
        .inner
    });
    ui.style_mut().visuals.override_text_color = old_text;
    let mut changed = resp.changed();

    let reset_resp = ui.interact(
        reset,
        ui.make_persistent_id(("slider-reset", &key)),
        Sense::click(),
    );
    if reset_resp.clicked() && changed_now {
        *v = default;
        changed = true;
    }

    let span = (rail_right - rail_left).max(1.0);
    let tx = |val: f64| {
        rail_left + span * (((val - min) / (max - min).max(1e-9)).clamp(0.0, 1.0)) as f32
    };
    // 非线性轨道（aevum_core::temp_track）：轨道几何上呈现的是「位置 p」，
    // 参数值 v 经锚点表双向换算——中段密两端疏，拖动手感对齐 LR。
    // 只影响滑块拖动与画点；数字输入框、滚动、历史、落盘全部仍用原始值 v。
    let rail_pos = |val: f64| -> f64 {
        if track {
            aevum_core::temp_track::TempTrack::COLOR_TEMPERATURE.val_to_pos(val)
        } else {
            val
        }
    };
    let drag_val = |p: f64| -> f64 {
        if track {
            aevum_core::temp_track::TempTrack::COLOR_TEMPERATURE.pos_to_val(p)
        } else {
            p
        }
    };
    // 命中区给 16px 高：轨道只有 3px，按 3px 去点会点空。
    let hit = Rect::from_min_max(
        Pos2::new(rail_left, track_y - 8.0),
        Pos2::new(rail_right, track_y + 8.0),
    );
    let t = ui.interact(
        hit,
        ui.make_persistent_id(("slider", &key)),
        Sense::click_and_drag(),
    );
    if (t.clicked() || t.dragged())
        && span > 1.0
        && let Some(pos) = t.interact_pointer_pos()
    {
        let frac = ((pos.x - rail_left) / span).clamp(0.0, 1.0) as f64;
        let p = min + frac * (max - min);
        let nv = drag_val(p);
        if (*v - nv).abs() > 1e-9 {
            *v = nv;
            changed = true;
        }
    }
    if t.double_clicked() {
        *v = default;
        changed = true;
    }
    if t.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y) as f64;
        if scroll.abs() > 0.1 {
            *v = (*v + scroll * (max - min) / 2000.0).clamp(min, max);
            changed = true;
        }
    }
    if changed {
        resp.mark_changed();
        // 进撤销历史：拖动中每帧都会登记一次，帧号连续的同一控件会被并成一步。
        history::note(
            label,
            format!("{label} {}", theme::number(*v, decimals, signed)),
        );
    }

    let p = ui.painter();
    // 轨道：3px 胶囊，用 theme 的滑块轨道色；已改量从默认点向数值填强调色。
    let rail_h = 3.0;
    let rail = Rect::from_min_max(
        Pos2::new(rail_left, track_y - rail_h * 0.5),
        Pos2::new(rail_right, track_y + rail_h * 0.5),
    );
    match stops {
        // egui 没有线性渐变矩形，48 片插值铺过去：3px 高的轨道上看不出接缝，
        // 形状数量也只多这 48 个，两条色温推子换来的是整组参数的可读性。
        Some([a, b]) => {
            const SLICES: f32 = 48.0;
            for k in 0..SLICES as usize {
                let t0 = k as f32 / SLICES;
                let x0 = rail_left + (rail_right - rail_left) * t0;
                let x1 = rail_left + (rail_right - rail_left) * (t0 + 1.0 / SLICES);
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, rail.top()),
                        Pos2::new(x1 + 0.6, rail.bottom()),
                    ),
                    0,
                    widgets::mix(a, b, t0),
                );
            }
        }
        None => {
            p.rect_filled(rail, rail_h * 0.5, palette().track);
            let ax = tx(rail_pos(default));
            let vx = tx(rail_pos(*v));
            if (vx - ax).abs() > 0.5 {
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(ax.min(vx), track_y - rail_h * 0.5),
                        Pos2::new(ax.max(vx), track_y + rail_h * 0.5),
                    ),
                    rail_h * 0.5,
                    accent(),
                );
            }
        }
    }
    // 中心刻线：跨零的推子给一条 1px 竖标，0 在哪一眼就看得见。
    // 不跨零的参数不画 —— 每一行都画一根，整列会多出一道虚线。
    if min < 0.0 && max > 0.0 {
        let cx = tx(rail_pos(0.0));
        p.line_segment(
            [Pos2::new(cx, track_y - 3.5), Pos2::new(cx, track_y + 3.5)],
            Stroke::new(1.0, divider()),
        );
    }
    // 推子：macOS 的实心小白圆 + 极淡投影；悬停、拖动各有一档明确的尺寸状态。
    let vx = tx(rail_pos(*v));
    let knob = Pos2::new(vx, track_y);
    let r = if t.dragged() {
        6.5
    } else if t.hovered() {
        6.0
    } else {
        5.5
    };
    p.circle_filled(knob + Vec2::new(0.0, 1.0), r, Color32::from_black_alpha(70));
    p.circle_filled(knob, r, Color32::WHITE);
    p.circle_stroke(knob, r, Stroke::new(1.0, INSET_RIM));
    if t.dragged() {
        p.circle_stroke(
            knob,
            r + 3.0,
            Stroke::new(1.5, accent().gamma_multiply(0.7)),
        );
    } else if t.hovered() {
        p.circle_stroke(knob, r + 2.5, Stroke::new(1.0, border_strong()));
    }
    // 复位点画在最后：这时 *v 已经落定，不会在刚被点回默认的那一帧闪一下。
    if (*v - default).abs() > 1e-9 {
        let hot = reset_resp.hovered();
        p.circle_filled(reset.center(), if hot { 3.6 } else { 2.6 }, accent());
        if hot {
            p.circle_stroke(
                reset.center(),
                5.5,
                Stroke::new(1.0, accent().gamma_multiply(0.6)),
            );
        }
        let _ = reset_resp.clone().on_hover_text(crate::i18n::tr_fmt(
            "点一下复位到 {}",
            &[&theme::number(default, decimals, signed)],
        ));
    }
    // 规格表自带的悬停说明（如手动噪声σ的低段死区提示）。挂在整行响应上，
    // 不与轨道/复位点的 tooltip 抢：非空才挂，绝大多数推子没有。
    if !tip.is_empty() {
        row_resp.on_hover_text(tip);
    }
    changed
}

pub(super) fn kv(ui: &mut Ui, key: &str, value: impl Into<String>, color: Color32) {
    let key = crate::i18n::tr(key);
    // 值也可能是文案（`kv(ui, "当前", "未挂外观")`），一样要过一遍翻译：
    // 数字与专名查不到就原样返回，不会把读数翻坏。
    let value = value.into();
    let value = crate::i18n::tr(&value);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), kv_h()), Sense::hover());
    let p = ui.painter();
    p.text(
        Pos2::new(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        key,
        font(SIZE_LABEL),
        muted(),
    );
    // 右端留 6px：非浮动的滚动条虽然不压在内容上，但读数紧贴那根竖条时
    // 会读成"和滚动条撞在一起"，一列数字右缘齐平反而更脏。
    p.text(
        Pos2::new(rect.right() - 6.0, rect.center().y),
        Align2::RIGHT_CENTER,
        value,
        mono(SIZE_LABEL),
        color,
    );
}

pub(super) fn note(ui: &mut Ui, text: &str) {
    let text = crate::i18n::tr(text);
    ui.add_space(6.0);
    ui.add(
        egui::Label::new(RichText::new(text).size(SIZE_SMALL).color(dim()))
            .wrap()
            .selectable(false),
    );
}

/// 工具栏按钮的灰态版：能不能点取决于历史光标，不能点就按禁用画。
pub(super) fn bar_button_on(ui: &mut Ui, label: &str, enabled: bool, shortcut: &str) -> Response {
    let label = crate::i18n::tr(label);
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).size(SIZE_BODY)).shortcut_text(shortcut),
    )
}

#[cfg(test)]
mod slider_reset_tests {
    use super::*;
    use egui::Context;
    use egui::PointerButton;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 120.0))),
            max_texture_side: Some(8192),
            events,
            ..Default::default()
        }
    }

    /// 复位点画在哪儿、点哪儿能中，两边必须对得上：
    /// 只亮不点等于骗人，只点不亮等于没有。所以先从画出来的形状里找出那颗点，
    /// 再照着它的坐标去点，而不是把几何抄一遍进测试。
    #[test]
    fn reset_dot_appears_only_when_changed_and_clicking_it_restores_default() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut value = 1.5f64;
        let mut draw = |root: &mut Ui| {
            slider(root, "exp", "曝光", &mut value, -5.0, 5.0, 0.0, 2, true);
        };
        let dot = {
            let mut out = ctx.run_ui(input(vec![]), &mut draw);
            let found = out.shapes.iter().find_map(|cs| match &cs.shape {
                egui::Shape::Circle(circle)
                    if circle.fill == accent() && (circle.radius - 2.6).abs() < 0.2 =>
                {
                    Some(circle.center)
                }
                _ => None,
            });
            out.textures_delta.clear();
            found.expect("改过的推子没画出复位点")
        };

        let events = [
            egui::Event::PointerMoved(dot),
            egui::Event::PointerButton {
                pos: dot,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos: dot,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ];
        let still_has_dot = {
            for ev in events {
                let mut out = ctx.run_ui(input(vec![ev]), &mut draw);
                out.textures_delta.clear();
            }
            // 回到默认之后点要消失，否则每条推子都挂着一颗按了没反应的圆。
            let mut out = ctx.run_ui(input(vec![]), &mut draw);
            let painted = out.shapes.iter().any(|cs| match &cs.shape {
                egui::Shape::Circle(circle) => circle.fill == accent() && circle.radius < 4.0,
                _ => false,
            });
            out.textures_delta.clear();
            painted
        };
        assert_eq!(value, 0.0, "点了复位点没回到默认值");
        assert!(!still_has_dot, "默认值上还挂着复位点");
    }
}
