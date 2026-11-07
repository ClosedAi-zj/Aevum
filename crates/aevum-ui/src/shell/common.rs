//! 面板公共件：侧栏行（「预设与快照」浮窗用）、组头、滑杆、键值行（被 adjust/mask/dock/window 引用）。
//!
//! Shared panel pieces: preset column, sidebar rows, group headers, sliders,
//! key-value rows (used by adjust / mask / dock).

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::MICRO;
use super::adjust::reset_group;
use crate::controls;
use crate::history;
use crate::icons::{self, Glyph};
use crate::state::AppState;
use crate::theme::{self, *};
use crate::widgets;
use egui::{
    Align, Align2, Color32, Layout, Pos2, Rect, Response, RichText, Sense, Stroke, Ui, UiBuilder,
    Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

/// 侧栏两节之间的软分割线：线短一截、压暗一档，读作「分节」而不是「切开」。
pub(super) fn side_divider(ui: &mut Ui) {
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
pub(super) fn side_head(ui: &mut Ui, label: &str, count: usize, dragging: bool) -> Response {
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
pub(super) fn side_row(ui: &mut Ui, label: &str, glyph: Option<Glyph>, active: bool) -> Response {
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
pub(super) fn side_note(ui: &mut Ui, text: &str) {
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
///
/// Whitespace only between groups, no boxes (macOS unified-toolbar convention).
pub(super) fn group_gap(ui: &mut Ui) {
    ui.add_space(18.0);
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
        "seq-cull" => {
            "按四条判据挑废片：曝光越界（EV，相对 18% 中灰）、与相邻帧的突变、清晰度低于序列中位的比例、读不出来。判定只看导入那趟已经算好的逐帧测光，不改素材；「标为排除」写的是已有的旗标，时间轴上一眼看得见。\n建议区间用序列自身的 MAD 推（3×1.4826×MAD，下限 0.5 EV）：标准差会把日落渐变的「光在变」读成「离散大」，MAD 只看中间一半，渐变不会被误杀。\n「剔除成子序列」把没被拒收的帧复制成一条新序列：目标不能是源内部或盘根，复制保留 mtime、测光缓存一并带走，新序列打开不用重测；开发参数属于源工程、不跟过去。"
        }
        "stack" => {
            "取当前帧前后各 N 帧按所选统计量合成一张：星轨取最亮、降噪取平均、去卫星取中值、杂散光与热噪点做 σ 截尾。全部在线性域计算，gamma 域平均会把星点与暗部压暗。"
        }
        "timeblend" => {
            "把不同时刻的帧合成为一张：天地合成按地平线取天用末帧、地用首帧；时间切片横向分带，一带一帧、从早到晚铺满整幅，云被拉成一段时间。带边界用 smoothstep 羽化（两侧权重恒和为 1），来源帧先按亮度中位数对齐到参考帧，接缝才不会出现台阶。天地合成的地平线可以自动推（按行亮度梯度找最强台阶，找不到退回手动值）；羽化区里的移动物可去鬼影（某帧某块与自己邻域偏离超过阈值就被剔掉，换干净帧顶上）。"
        }

        "focus" => {
            "把一组不同对焦距离的帧融成一张全景深。深度图法：逐像素取测度最强的帧，测度低于阈值的无细节区整块判给参考帧（序列中位帧），深度跳变处按羽化宽度交叠——保色彩、平坦区不出斑块；加权平均法：测度四次方当权重直接混，最简单但对强纹理可能轻微发糊。测度在 γ2.0 亮度上算，半径是分析窗口（越大抗噪、越小分辨近邻深度）。\n阈值按全图最佳测度的分位数表达（0 = 关闭），跨序列可迁移——这就是 DMap 那根对比度滑杆的口径。"
        }
        "basic" => {
            "白平衡与影调范围：先定冷暖（色温）与绿品（色调），再拉曝光 / 对比 / 高光 / 阴影 / 黑白场，最后是纹理、清晰度、去朦胧与色彩浓度。\n曝光以 EV 计，其余参数相对中间调调整。\n高光过渡（对齐 LR）：曝光推过 1 的区域走有理肩部缓收，不再硬剪。\n线性对比（对齐 LR）：对比度改在线性光域计算，暗部响应更接近 LR。\n黑场/白场软化（对齐 LR）：两端用抛物线软拐点收进 [0,1]，直方图边缘不再堆尖。"
        }
        "curve" => {
            "三种调法：参数曲线按亮度区间（阴影、中间调、高光）拉起落，另有三条分界；点曲线是 RGB 与三条单通道，坐标即输入输出值；色阶在输入黑白场、灰度系数与输出范围上直接拖动。\nHDR 尾部（对齐 LR）：x>1 的过曝层次按曲线末端斜率延伸进曲线，不再硬截。"
        }
        "mixer" => {
            "两种模式：HSL 按颜色范围调整色相、饱和度与明亮度；黑白混合决定各色相在灰度中的权重。"
        }
        "grade" => "阴影、中间调、高光四向色彩分级，可分别指定色轮与亮度。",
        "detail" => {
            "锐化、降噪与蒙版。\n锐化保护（对齐 LR）：按噪声底自适应保护平坦区，与蒙版叠加生效。"
        }
        "optics" => {
            "手动三项（畸变、暗角、去紫边）与配置文件：相机 DCP 接管 RAW 颜色，镜头 LCP 按 EXIF 选帧，可另选 lensfun 库。"
        }
        "effects" => {
            "暗角（含中点、圆度、羽化与高光保护）与胶片颗粒。\n颗粒亮度（对齐 LR）：颗粒强度随亮度分级，正值暗部更重、负值反转（高光更重）。"
        }
        "look" => "载入 .cube 3D LUT 作为外观层，不改动底层调色参数。",
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
    // 分组头口径（设计图 `.acc-head`）：8px 顶空 + 32px 行高 + 单 hairline 下沿，
    // 内边距 `0 12px 0 20px`（标题 x=20，右侧留 12）。主次靠「粗标题 + 单线」表达，
    // 不靠留白堆。
    //
    // Group header, per the design's `.acc-head`: 8 px of top space, a 32 px row, one
    // closing hairline, and `0 12px 0 20px` padding. Hierarchy comes from the bold title
    // and the single line, not from padding.
    ui.add_space(8.0);
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
    // ACR 的分区头签名：标题条下沿一道实线，再往上 2px 一道更淡的细线——
    // 双线把「一个分区从这里开始」和表格横线区分开。旧版只在顶部画单线，
    // 十四组排下来整屏读着像一张表格。
    //
    // ACR's section-header signature: a solid rule at the bottom edge plus a fainter
    // hairline 2 px above it. The double rule reads as "a section starts here", where
    // the old single top rule read as a table row divider.
    theme::rule(p, rect.left(), rect.right(), rect.bottom() - 1.0, divider());
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
        if resp.hovered() { text() } else { muted() },
    );
    // 字号比正文大一档 + 真粗体（700 子集字族）：ACR 的分区标题是 semibold，
    // 只靠字号和颜色堆不出这层「这是标题」的体格差。
    //
    // One step above body copy plus real bold (the 700 subset family): ACR's section
    // headers are semibold, and size plus colour alone cannot carry that "this is a
    // heading" weight difference.
    let title_galley = p.layout_no_wrap(title.to_owned(), bold(SIZE_LABEL + 1.0), text());
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
    // 眼睛旁路（ACR 的面板开关）：点一下，这一组的调整退出预览渲染，再点恢复。
    // 只在显影八组出现——序列、成片那些组的参数不在调色管线里，旁路无定义。
    // 关态画一道强调色斜杠；提示语把「只影响预览」说在明处，免得有人以为
    // 导出也会跟着变。
    //
    // Eye bypass (ACR's panel switch): one click takes this group's adjustments out of
    // the preview render, another brings them back. Present on the eight Develop
    // sections only -- sequence and render groups keep their parameters outside the
    // grading pipeline, where bypass is undefined. The off state adds an accent slash;
    // the tooltip says "preview only" up front so nobody expects exports to follow.
    let bypassable = controls::is_bypassable(key);
    if bypassable {
        let off = state.bypassed.contains(key);
        let er = Rect::from_center_size(
            Pos2::new(rect.right() - 30.0, rect.center().y),
            Vec2::splat(18.0),
        );
        let eresp = ui.interact(
            er,
            ui.make_persistent_id(("group-bypass", key)),
            Sense::click(),
        );
        if eresp.hovered() {
            p.rect_filled(er, radius_control(), elev_hover());
        }
        icons::paint(
            p,
            er.shrink(4.5),
            Glyph::Eye,
            if eresp.hovered() && !off {
                text()
            } else {
                muted()
            },
        );
        if off {
            let ir = er.shrink(4.5);
            p.line_segment(
                [ir.left_top(), ir.right_bottom()],
                Stroke::new(1.2, accent()),
            );
        }
        let bypass_clicked = eresp.clicked();
        if bypass_clicked && off {
            state.bypassed.remove(key);
            state.preview_dirty = true;
        } else if bypass_clicked {
            state.bypassed.insert(key);
            state.preview_dirty = true;
        }
        eresp.on_hover_text(if off {
            crate::i18n::t!("这一组的调整已旁路，点按恢复参与渲染")
        } else {
            crate::i18n::t!(
                "旁路这一组：预览按没有这组调整的样子渲染，再点恢复（只影响预览，不进工程与导出）"
            )
        });
    }
    // 组内改动指示（界面稿 .grp-hd .dot）：组里有参数偏离默认，复位按钮左侧
    // 亮一枚 5px accent 圆点；复位之后自然熄灭。
    if state.group_changed(key) {
        let dx = match (bypassable, resettable) {
            (true, true) => 44.0,
            (true, false) => 29.0,
            (false, true) => 26.0,
            (false, false) => 12.0,
        };
        p.circle_filled(Pos2::new(rect.right() - dx, rect.center().y), 2.5, accent());
    }
    if resettable {
        let rr = Rect::from_center_size(
            Pos2::new(rect.right() - 11.0, rect.center().y),
            Vec2::splat(18.0),
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
            if rresp.hovered() { text() } else { muted() },
        );
        if rresp.clicked() {
            reset_group(state, key);
        }
        rresp.on_hover_text(crate::i18n::t!("重置这一分组"));
    }
    if resp.clicked() {
        if state.collapsed.contains(key) {
            // 单开模式（ACR 默认）：点开一组，同模块其他组自动收起——面板永远
            // 只有一区摊开，视线不用在两条长列之间跳。多开模式下各组互不干涉。
            //
            // Single-open mode (ACR's default): opening a group folds its siblings in the
            // same module, so exactly one section is unrolled at a time. In multi-open
            // mode the groups stay independent.
            if state.panel_solo {
                state.solo_group(key);
            } else {
                state.collapsed.remove(key);
                state.settings_dirty = true;
            }
        } else {
            state.collapsed.insert(key);
            state.settings_dirty = true;
        }
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
        ui.separator();
        // 第四项：ACR 的「面板模式」单选。放在菜单最后并加分隔线——它是设置，
        // 上面三条是动作，混在一起会让人以为它也只影响这一次。
        //
        // Fourth item: ACR's panel-mode choice. Last, behind a separator -- it is a
        // preference while the three above are one-off actions.
        let mut solo = state.panel_solo;
        if ui
            .checkbox(&mut solo, crate::i18n::t!("单开模式"))
            .on_hover_text(crate::i18n::t!(
                "点开一组时自动收起同模块的其他组（Camera Raw 的默认行为）"
            ))
            .changed()
        {
            state.panel_solo = solo;
            state.settings_dirty = true;
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

/// 带渐变轨道的推子：轨道两端色由调用方给定（HSL 频段这类「轨道即含义」
/// 的参数）。其余行为与 [`slider`] 完全一致。
///
/// A fader with a gradient rail: the caller supplies the two end colours for
/// parameters whose *track carries meaning* (HSL bands). Behaviour is otherwise
/// identical to [`slider`].
#[allow(clippy::too_many_arguments)]
pub(super) fn slider_grad<K: std::hash::Hash + std::fmt::Debug>(
    ui: &mut Ui,
    key: K,
    label: &str,
    v: &mut f64,
    min: f64,
    max: f64,
    default: f64,
    decimals: usize,
    signed: bool,
    stops: &'static [(f32, Color32)],
) -> bool {
    slider_row(
        ui,
        key,
        label,
        v,
        min,
        max,
        default,
        decimals,
        signed,
        "",
        Some(stops),
        false,
        "",
    )
}

/// HSL 频段的轨道渐变（对齐 LightCraft `Track::Hue/Sat/Lum` 的语义，
/// 逆向报告 §3.3：轨道样式是数据不是代码）：
/// - 色相：频段色 → 补色——往左往右各偏向哪个色相，一眼可读；
/// - 饱和度：中性灰 → 频段色（拉正 = 更艳）；
/// - 明亮度：黑 → 频段色（拉正 = 该频段提亮）。
///
/// Rail gradients for one HSL band (mirroring LightCraft's `Track::Hue/Sat/Lum`
/// semantics -- reverse report §3.3: track style is data, not code): hue runs from the
/// band colour to its complement, saturation from neutral grey to the band colour,
/// luminance from black to the band colour.
pub(super) fn hsl_band_stops(band: usize, kind: HslKind) -> &'static [(f32, Color32)] {
    let h = crate::widgets::BAND_HUES[band.clamp(0, 7)] as f32;
    let band_color = hsl(h, 82.0, 55.0);
    // 动态生成的表需要 'static：色相频段只有 8×3=24 种组合，用 Box::leak 换
    // 'static 是可接受的（进程内最多 24 次分配，之后恒等命中）。
    //
    // The generated table needs 'static: there are only 8x3 = 24 hue/kind combinations,
    // so leaking a box is acceptable (at most 24 allocations per process, thereafter
    // identical).
    let table: [(f32, Color32); 2] = match kind {
        // 补色 = 色相转半圈。
        HslKind::Hue => [
            (0.0, band_color),
            (1.0, hsl((h + 180.0) % 360.0, 82.0, 55.0)),
        ],
        HslKind::Sat => [(0.0, hsl(h, 0.0, 55.0)), (1.0, band_color)],
        HslKind::Lum => [(0.0, hsl(h, 82.0, 4.0)), (1.0, band_color)],
    };
    Box::leak(Box::new(table))
}

/// [`hsl_band_stops`] 的三条轨道种类。
///
/// The three rail kinds of an HSL band.
#[derive(Clone, Copy)]
pub(super) enum HslKind {
    Hue,
    Sat,
    Lum,
}

#[allow(clippy::too_many_arguments)]
/// 按规格表渲染一条推子。标签/范围/默认/精度/单位/渐变全部来自
/// `controls::SPECS`——加参数只登记表，不再在面板里摊九个实参。
pub(super) fn spec_slider(ui: &mut Ui, state: &mut AppState, s: &controls::ControlSpec) -> bool {
    let changed = {
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
    };
    // 在旁路中的组里拖推子＝想改它：当场恢复参与渲染。不恢复的话推子在动、
    // 画面不动，看起来就像坏了；ACR 的同位开关也是一改参数就自动点亮。
    //
    // Dragging a slider inside a bypassed group means "I want to change it": bring it
    // back into the render immediately. Otherwise the slider moves while the image does
    // not, which reads as broken; ACR's counterpart switch also re-enables on edit.
    if changed && state.bypassed.remove(s.group) {
        state.preview_dirty = true;
    }
    changed
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
    stops: Option<&'static [(f32, Color32)]>,
    track: bool,
    tip: &str,
) -> bool {
    // ── 单行横排：标签 76 · 轨道弹性 · 读数 44 ───────────────────────────
    // 设计图 `.slider-row{padding:0 12px;gap:8px}` + `.s-label{width:76px}` +
    // `.s-value{width:44px}` 的口径。读数从 52 收到 44：等宽字体下 `-100` 这类
    // 三位数带号也排得下，多出来的 8px 还给轨道——设计图里轨道明显更长。
    // 翻译下沉在这里：SPECS 静态表的 label 是 const 数据，不能在表里调 tr；
    // 所有滑块（显影/构图/成片）都汇到这一条路上，改一处全生效。
    //
    // One horizontal row: label 76 · elastic rail · readout 44, matching the design's
    // `.slider-row` padding/gap plus `.s-label` and `.s-value` widths. The readout drops
    // from 52 to 44 -- monospace fits a signed three-digit value -- and the 8 px goes to
    // the rail, which is visibly longer in the design.
    let label_w = label_col_w(ui);
    const VALUE_W: f32 = 44.0;
    // 复位点 5px：设计图 `.slider-row .s-reset{width:5px;height:5px}`。原 14px 是
    // 一个带内边距的点击靶，比设计稿宽 9px，轨道因此短了一截——实测程序轨道
    // 188px vs 设计稿约 207px。命中区改用 `ui.interact` 的矩形另行放大，
    // 不再靠占位宽度换手感。
    //
    // 5 px per the design's `.slider-row .s-reset{width:5px;height:5px}`. The old
    // 14 px padding box was 9 px wider than the design and shortened the rail
    // accordingly -- measured 188 px against the design's ~207 px. Hit area is widened
    // through the `ui.interact` rect instead of by reserving layout width.
    const RESET_W: f32 = 5.0;
    const ROW_PAD: f32 = 12.0;
    let label = crate::i18n::tr(label);
    let tip = crate::i18n::tr(tip);

    let (rect, row_resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h()), Sense::hover());
    let hot_row = ui.rect_contains_pointer(rect);
    let changed_now = (*v - default).abs() > 1e-9;

    // 数值框：右端留出复位点位置，读数右对齐 —— 一列读数的小数点才会落在同一条竖线上。
    // 右侧排布照设计图 DOM 顺序：轨道(flex:1) → 读数(44) → 复位点(5px)，
    // 复位点**在读数之后**，不占轨道宽度。
    let field = Rect::from_min_size(
        Pos2::new(
            rect.right() - ROW_PAD - RESET_W - VALUE_W,
            rect.center().y - 10.0,
        ),
        Vec2::new(VALUE_W, 20.0),
    );
    // 复位点排在读数右侧：偏离默认值才现形。旧版把它夹在轨道与读数之间，即使参数
    // 没改动也一直占着 14px，轨道被压短一截——这是并排对照里「轨道比设计稿短」的
    // 直接原因。
    //
    // The restore dot sits to the **right** of the readout, matching the design's DOM
    // order (track flex:1 → value 44 → reset 5 px). The old version wedged it between
    // the track and the readout, so it reserved 14 px even on untouched parameters and
    // the rail came out visibly shorter than the design.
    let reset = Rect::from_center_size(
        Pos2::new(rect.right() - ROW_PAD - RESET_W * 0.5, rect.center().y),
        Vec2::splat(RESET_W),
    );
    let rail_left = rect.left() + ROW_PAD + label_w + 4.0;
    let rail_right = (field.left() - 8.0).max(rail_left + 1.0);
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
        // 数值框：PS / Camera Raw 口径——实底 + 1px 描边独立成格，不是一个裸数字
        // 漂在轨道右端。`field()` 是比面板沉一档的凹槽色，`divider()` 勾出边框，
        // 视线才能把「标签│轨道│数值」三列分开，这也是 PS 调色面板最直观的特征。
        //
        // Value box, PS / Camera Raw style: a filled box with a 1px outline of its own
        // rather than a bare number floating at the rail's right end. `field()` is a
        // step below the panel, and `divider()` draws the edge, so the eye can separate
        // "label | rail | value" -- the most recognisable trait of PS's adjustment panel.
        p.rect_filled(field, radius_control(), theme::field());
        p.rect_stroke(
            field,
            radius_control(),
            Stroke::new(1.0, divider()),
            egui::StrokeKind::Inside,
        );
        if ui.rect_contains_pointer(field) {
            p.rect_filled(field, radius_control(), theme::field_hover());
            p.rect_stroke(
                field,
                radius_control(),
                Stroke::new(1.0, border_strong()),
                egui::StrokeKind::Inside,
            );
        }
        // 6 字截断：5 字中文在 76px 里刚好排满，再长就让位给轨道，
        // 宁可少两个字，也不让标签爬进推子的地盘。
        // 截断长度按语言分档：中文 6 字约 78px 恰好贴住 LABEL_W，英文同宽只放得
        // 下 ~11 个半角字符，仍按 6 截会把 "Highlights" 截成 "Highli…"。
        // 档位存在语言表里（`Catalog::ellipsis_chars`），加语言时由那张表自己声明，
        // 这里不再出现具体语言的名字。
        p.text(
            Pos2::new(rect.left() + ROW_PAD, rect.center().y),
            Align2::LEFT_CENTER,
            theme::ellipsis(label, crate::i18n::current().ellipsis_chars()),
            font(SIZE_LABEL),
            if changed_now { text() } else { muted() },
        );
    }

    let old_text = ui.style().visuals.override_text_color;
    let old_font = ui.style().override_font_id.clone();
    // 读数颜色与强调色脱钩（用户截图：满屏蓝读数像一片报错）：改动值用正文白，
    // 未动值用 value()（淡一档）。蓝色留给交互态（悬停/拖动）。
    ui.style_mut().visuals.override_text_color = Some(if changed_now { text() } else { value() });
    // 读数走等宽（设计稿 B1）：一列推子的小数点必须落在同一条竖线上。`DragValue`
    // 没有 `.font()`，只能在作用域里压 `override_font_id`，画完立刻还原——与
    // 选项栏 `bar_value` 同一套写法。
    //
    // Readouts use the monospace family (design spec B1): the decimal points of a
    // column of sliders must share one vertical line. `DragValue` has no `.font()`,
    // so the font is overridden for this scope only and restored right after --
    // the same pattern as the options bar's `bar_value`.
    ui.style_mut().override_font_id = Some(mono(SIZE_LABEL));
    let mut resp = in_field(ui, Color32::TRANSPARENT, Color32::TRANSPARENT, |ui| {
        ui.scope_builder(
            UiBuilder::new()
                .id_salt(("slider-value", &key))
                .max_rect(field.shrink2(Vec2::new(2.0, 0.0)))
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                // DragValue 内部把数字画成 Small 档的按钮，Small 映射的是 Inter——
                // 这就是「框里的数字和框外的读数不是一个字体」的根因。override_font_id
                // 在按钮内部会被其自身样式重置，所以必须改 TextStyle 表：把按钮档
                // 直接压到 mono。作用域限定在这一个框内，画完由 in_field 还原。
                //
                // DragValue paints its number as a Small-styled button, and Small maps to
                // Inter -- that is exactly why the number inside the box did not match the
                // readouts outside. `override_font_id` is reset by the button's own style,
                // so the TextStyle table is the thing to override: press the Button step
                // down to mono, scoped to this one box and restored by `in_field`.
                ui.style_mut()
                    .text_styles
                    .insert(egui::TextStyle::Button, mono(SIZE_LABEL));
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
    ui.style_mut().override_font_id = old_font;
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
    // 滑块不消费滚轮：滚轮留给面板滚动。用户在滚动面板时鼠标划过滑块，
    // 曾经会静默改掉参数——一次滚动毁掉一组调整。
    //
    // Sliders never consume wheel scroll; the wheel belongs to the panel's ScrollArea.
    // Scrolling a panel used to silently change any slider the pointer crossed.
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
        // 形状数量也只多这 48 个，几条渐变推子换来的是整组参数的可读性。
        // 取色走 `controls::sample_stops`：停靠点表支持三色以上（色温 = 蓝→灰→黄），
        // 两端直接对插会在中间拉出一条发绿的过渡，中灰位置反而不中性。
        //
        // egui has no linear-gradient rect, so 48 interpolated slices are laid down:
        // invisible seams on a 3 px rail, only 48 extra shapes, and a few gradient
        // faders buy readability across the whole group. Colour comes from
        // `controls::sample_stops`, whose stop table supports three or more stops
        // (temperature is blue -> grey -> yellow); interpolating straight between the
        // two ends drags a green cast through the middle and the neutral point stops
        // being neutral.
        Some(table) => {
            const SLICES: f32 = 48.0;
            for k in 0..SLICES as usize {
                let t0 = k as f32 / SLICES;
                let t1 = (k as f32 + 1.0) / SLICES;
                let x0 = rail_left + (rail_right - rail_left) * t0;
                let x1 = rail_left + (rail_right - rail_left) * t1;
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, rail.top()),
                        Pos2::new(x1 + 0.6, rail.bottom()),
                    ),
                    0,
                    controls::sample_stops(table, t0),
                );
            }
        }
        None => {
            // 轨道槽（设计稿 §滑块）：3px 槽 + 1px divider 环。只靠 track 色在深底
            // 上几乎看不见——用户反馈「线条太细」的真实原因是**对比度**，不是尺寸；
            // 描一圈边才把「这里可以拖」指出来。
            //
            // Track groove (design spec, sliders): a 3px groove with a 1px divider
            // ring. The track colour alone nearly vanishes on the dark panel -- the
            // "too thin" feedback was really about contrast, not size; the ring is what
            // says "this is draggable".
            p.rect_filled(rail, rail_h * 0.5, palette().track);
            p.rect_stroke(
                rail,
                rail_h * 0.5,
                Stroke::new(1.0, divider()),
                egui::StrokeKind::Inside,
            );
            let vx = tx(rail_pos(*v));
            // 填充口径按设计图 `.slider-track .fill{left:0;…}`：从轨道**左端**铺到
            // knob，读作一根进度条。旧版是「默认值 → 当前值」的差值段：参数回到默认
            // 整条填充就消失，稍改一点只剩很短一截，整列看上去全是空轨道——这是并排
            // 对照时最扎眼的一处差异（设计稿每一行都有一条明确的填充量）。
            //
            // 左端填充对「色温 / 色调」这类无默认中点的轨道同样成立：它们本来就是
            // 从左到右的量程，不是双向偏移量。
            //
            // Fill per the design's `.slider-track .fill{left:0;…}`: from the rail's
            // **left end** to the knob, reading as a progress bar. The old span ran from
            // the default value to the current one, so a parameter back at its default
            // showed no fill at all and a small change showed a stub -- the whole column
            // read as blank tracks. That was the most conspicuous difference in the
            // side-by-side comparison, where every row in the design shows a clear amount
            // of fill. Left-anchored fill also suits temperature and tint, whose range
            // runs left to right rather than around a centre.
            if vx - rail_left > 0.5 {
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(rail_left, track_y - rail_h * 0.5),
                        Pos2::new(vx, track_y + rail_h * 0.5),
                    ),
                    rail_h * 0.5,
                    // 填充色 = 读数色（refit-preview `.srow .rail .fill{background:var(--value)}`）：
                    // 轨道填充与右侧读数同一个颜色来源，扫读时「这条轨对应那个数」
                    // 靠同色就能对上；elev_hover 灰填与任何读数都无呼应。
                    //
                    // Fill colour = the readout colour (refit-preview's
                    // `.srow .rail .fill{background:var(--value)}`): rail fill and the
                    // readout on the right share one colour source, so a row's slider and
                    // its number pair up by hue alone; the old grey fill answered nothing.
                    value(),
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
    // knob 尺寸与配色严格照设计图 `.slider-track .knob`：直径 14px、底色
    // `elev_hover`、`border_strong` 描边。hover 16 / 按下 13（按下是**缩小**，
    // 光标压上去时圆点陷进轨道才读得出「按住了」）。
    //
    // 旧版是 16px **纯白**大圆点：深底上一列白点比任何参数内容都抢眼，整列读起来
    // 像一排按钮而不是推子——这是并排对照时最直接的「不像」来源，不是尺寸差 2px
    // 的问题，是白点在深色面板上的对比度远高于设计稿的灰点。
    //
    // Knob size and colour follow the design's `.slider-track .knob` exactly: 14 px
    // across, `elev_hover` fill, `border_strong` stroke. Hover is 16, pressed is 13
    // (pressed **shrinks**, so the dot sinks into the track and reads as held down).
    //
    // The old knob was a 16 px **pure white** dot: a column of white dots on a dark
    // panel out-shouted every piece of parameter content and read as a row of buttons
    // rather than a fader. That was the most direct source of "this does not look like
    // the design" -- not the 2 px size difference, but the contrast of a white dot on a
    // dark panel against the design's grey one.
    let vx = tx(rail_pos(*v));
    let knob = Pos2::new(vx, track_y);
    // 圆形手柄：用户 2026-11-07 明确要求「三角改回之前的圆形」。PS 的推子确实是
    // 三角，但本项目在 1280 宽、72% 缩放的实测下三角只有 9×10，读起来像一个小箭头
    // 而不是可抓的把手；圆点在同样尺寸下更容易命中，也和本界面其它圆形控件同一族。
    //
    // 尺寸照旧随状态变化：常态 14、悬停 16、按下 13（按下缩小＝陷进轨道）。
    //
    // Round knob: on 2026-11-07 the user asked to revert the triangle to the earlier
    // circle. Photoshop's fader really is a triangle, but measured on this UI at 1280 px
    // and 72% zoom the triangle is only 9x10 and reads as a small arrow rather than a
    // grabbable handle; a dot is easier to hit at that size and matches the other round
    // controls here.
    //
    // Size still follows state: 14 normally, 16 on hover, 13 pressed (shrinking into the
    // rail).
    let r = if t.dragged() {
        6.5
    } else if t.hovered() {
        8.0
    } else {
        7.0
    };
    // 先投影再填色：浅色渐变轨上，没有暗边的白点会糊进轨道。
    //
    // Shadow first, then fill: without a dark edge a light dot blurs into a light gradient
    // rail.
    p.circle_filled(knob + Vec2::new(0.0, 1.0), r, Color32::from_black_alpha(70));
    p.circle(
        knob,
        r,
        if t.dragged() { accent() } else { text() },
        Stroke::new(1.0, Color32::from_black_alpha(90)),
    );
    // 悬停时外圈描边，指认「可抓」；拖动中不画——按下态已用 accent 填色区分。
    //
    // A halo ring on hover says "grabbable"; omitted while dragging, where the accent fill
    // already marks the pressed state.
    if t.hovered() && !t.dragged() {
        p.circle_stroke(knob, r + 2.0, Stroke::new(1.0, border_strong()));
    }
    // 复位点画在最后：这时 *v 已经落定，不会在刚被点回默认的那一帧闪一下。
    if (*v - default).abs() > 1e-9 {
        let hot = reset_resp.hovered();
        // 5px 尺寸下圆点画 2.5/3.2：设计稿复位点本身就是 5x5 的小圆。
        p.circle_filled(reset.center(), if hot { 3.2 } else { 2.5 }, accent());
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
    kv_with_h(ui, key, value, color, kv_h());
}

/// 一行「左标签 + 右侧一组互斥选项」。
///
/// 成片页此前是五排裸 chip：`视频 / 静帧序列`、`原生 / 4K / 1080p`、
/// `PNG 8bit / TIFF 16bit`…… 一行都没有标签，用户看不出这一排是在选模式、选分辨率
/// 还是选位深。PS 与 Camera Raw 的每一行参数都带左标签，这是「像不像专业软件」
/// 最直接的一条；标签列复用滑块的 76px，换行后与第一枚选项左缘对齐（悬挂缩进）。
///
/// One row of "left label + a group of mutually exclusive options".
///
/// The render page used to be five bare chip rows with no labels at all: a user could not
/// tell whether `PNG 8bit / TIFF 16bit` picked a container or a bit depth. Every parameter
/// row in Photoshop and Camera Raw carries a left label, and that is the most direct
/// "does this read as a professional tool" signal. The label column reuses the slider's
/// 76 px, and wrapped options hang under the first option instead of under the label.
/// 下拉框弹层的统一口径（风格/编码器/交付预设全走它）：
/// egui ComboBox 默认弹层是裸灰底默认字——全站最不像成熟软件的一块（用户点名）。
/// 项 hover/选中 = 强调色实底 + 白字（与菜单同一语言），行高 26，圆角走 token。
///
/// One shared skin for every dropdown popup (style / codec / delivery presets):
/// egui's stock ComboBox popup is a bare grey list with default type -- the least
/// "mature software" block in the app (user-flagged). Items hover/selected = accent
/// fill + white text (same language as the menus), 26 px rows, token radius.
pub(super) fn combo_style(ui: &mut Ui) {
    let w = &mut ui.style_mut().visuals.widgets;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, text());
    w.hovered.weak_bg_fill = accent();
    w.hovered.bg_fill = accent();
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.0, on_accent());
    w.active.weak_bg_fill = accent_hover();
    w.active.bg_fill = accent_hover();
    w.active.bg_stroke = Stroke::NONE;
    w.active.fg_stroke = Stroke::new(1.0, on_accent());
    ui.spacing_mut().button_padding = Vec2::new(10.0, 5.0);
    ui.spacing_mut().item_spacing.y = 1.0;
}

/// 面板自适应标签列宽（Microsoft/Material 比例口径）：面板宽 24%，夹 64..100。
/// 窄面板列先让路不挤轨道，宽面板列封顶、多出的宽度全给轨道——「该长的长」。
/// slider_row 与 chip_row 共享一份，两列在任何面板宽度下都对齐。
///
/// Adaptive label column (Microsoft/Material proportional guidance): 24 % of panel
/// width, clamped 64..100. Shared by slider_row and chip_row so both columns stay
/// aligned at every panel width.
pub(super) fn label_col_w(ui: &Ui) -> f32 {
    (ui.available_width() * 0.24).clamp(64.0, 100.0)
}

pub(super) fn chip_row(ui: &mut Ui, label: &str, mut chips: impl FnMut(&mut Ui)) {
    // 与滑块同列：列宽走共享 label_col_w（比例自适应），两列任意面板宽度都对齐。
    //
    // Same column as the sliders: the width comes from the shared label_col_w, so both
    // columns stay aligned at any panel width.
    let label_w = label_col_w(ui);
    let label_text = crate::i18n::tr(label);
    ui.horizontal_top(|ui| {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(label_w, super::BAR_H - 2.0), Sense::hover());
        // 标签按列宽**单行省略号截断**：英文的 `Format and bit depth` 比 76px 列宽，
        // 旧写法直接画出去，压住第一枚选项 26px（重叠判据在 Lang("en") 下当场点名）。
        // 完整文案走悬停，中文这一列本来就放得下，看不出变化。
        //
        // The label is truncated to a single row with an ellipsis: the English
        // `Format and bit depth` is wider than the 76 px column, and painting it verbatim
        // ran 26 px into the first option (the overlap gate named it under Lang("en")).
        // The full text is on hover; the Chinese labels already fit, so nothing changes
        // there.
        let mut job = egui::text::LayoutJob::simple_singleline(
            label_text.to_owned(),
            font(SIZE_LABEL),
            muted(),
        );
        job.wrap = egui::text::TextWrapping {
            max_width: label_w - 6.0,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            Pos2::new(rect.left(), rect.center().y - galley.size().y * 0.5),
            galley,
            muted(),
        );
        // 选项在「选项列」里横向换行：竖容器定住左缘，里面的 horizontal_wrapped 负责
        // 同一行排开、满了才换行。少了里面这层，每个 chip 会各占一行（第一版就是这个
        // 毛病，截图里选项被挤成竖排）。
        //
        // Options flow horizontally inside the option column: the vertical container pins
        // the left edge and the inner horizontal_wrapped lays chips side by side, wrapping
        // only when full. Without the inner layer each chip took its own row -- that was the
        // first cut, visible in the screenshot as a vertical stack of chips.
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| chips(ui));
        });
    });
}

/// 紧凑键值行：给「一屏要排很多条」的信息列表用（序列页体检结论等）。
///
/// 设计图的 `.kv-row` 是 32px，那是**少量信息行**的规格；序列页有 11 行这类信息，
/// 全按 32px 排会多占约 88px，把体检清单（用户真正要看的结果）挤到可视区外。
/// 紧凑档取 24px——一对 12px 字仍在行内垂直居中，不重叠。
///
/// Compact key/value row for information lists that stack many lines on one screen
/// (the sequence page's QA summary, for instance). The design's `.kv-row` is 32 px,
/// which is the spec for a *few* information rows; the sequence page has eleven, and
/// laying all of them out at 32 px costs about 88 px extra and pushes the QA findings --
/// the thing the user actually came to read -- out of the visible area. The compact step
/// uses 24 px, still enough to centre a 12 px pair without overlap.
pub(super) fn kv_dense(ui: &mut Ui, key: &str, value: impl Into<String>, color: Color32) {
    kv_with_h(ui, key, value, color, crate::theme::KV_DENSE_H);
}

fn kv_with_h(ui: &mut Ui, key: &str, value: impl Into<String>, color: Color32, h: f32) {
    let key = crate::i18n::tr(key);
    // 值也可能是文案（`kv(ui, "当前", "未挂外观")`），一样要过一遍翻译：
    // 数字与专名查不到就原样返回，不会把读数翻坏。
    let value = value.into();
    let value = crate::i18n::tr(&value);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
    let p = ui.painter();
    let key_font = font(SIZE_LABEL);
    let key_w = p
        .layout_no_wrap(key.to_owned(), key_font.clone(), muted())
        .size()
        .x;
    p.text(
        Pos2::new(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        key,
        key_font,
        muted(),
    );
    // 值按「右侧剩余宽度」裁：路径一类长值从前是直接右对齐铺过去，超过剩余宽度
    // 就压到左边的键名上（2026-11-04 的重叠自检抓到的实例：交付面板里一条
    // `C:\Users\…` 压在「预设表」上）。留 12px 间隙，且至少给 24px——
    // 键再长也得让值露出几个字，不然这一行等于只写了键。
    //
    // The value is clipped to the width left on the right: long values (paths and the
    // like) used to be right-aligned unconditionally and would run into the key on the
    // left (caught by the 2026-11-04 overlap sweep: a `C:\Users\…` sitting on "预设表").
    // A 12 px gap, and at least 24 px so a long key cannot swallow the value entirely.
    let value_font = mono(SIZE_LABEL);
    let avail = (rect.right() - 6.0 - (rect.left() + key_w + 12.0)).max(24.0);
    let value = fit_one_line(p, value, value_font.clone(), avail);
    p.text(
        Pos2::new(rect.right() - 6.0, rect.center().y),
        Align2::RIGHT_CENTER,
        value,
        value_font,
        color,
    );
}

/// 单行宽度不够时按字符二分截断并补省略号。给读数与路径用：`Label::truncate`
/// 走的是 egui 的布局流程，而这里是在 painter 上直接落字（`kv` 两列对齐靠它）。
///
/// Truncate to one line with an ellipsis, binary-searching on the character count.
/// Used for values and paths: `Label::truncate` goes through egui's layout, while
/// these are painted directly (that is what keeps `kv`'s two columns aligned).
fn fit_one_line(p: &egui::Painter, text: &str, font_id: egui::FontId, max_w: f32) -> String {
    let width = |s: String| {
        p.layout_no_wrap(s, font_id.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width(text.to_owned()) <= max_w {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let mut probe: String = chars[..mid].iter().collect();
        probe.push('…');
        if width(probe) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut out: String = chars[..lo].iter().collect();
    out.push('…');
    out
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
