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

/// 分组之间的缝：纵向列画一道横贯全宽的凹槽（R1 的立体分割线）；横向行
/// （选项栏等）推进 12px 并画一道竖凹槽——**不许按可用宽分配**：横向布局里
/// `available_width` 是整行余宽，一口吃掉会把行内其余控件挤到面板外整段消失
/// （R1 首版回归：选项栏只剩文档页签，工具身份/参数/缩放全部不可见）。
///
/// The gap between groups: in a vertical column a full-width horizontal groove (R1's
/// recessed divider); in a horizontal row (options bar etc.) advance 12 px and paint a
/// vertical groove. The horizontal branch must NOT allocate by available width: in a
/// row that is the whole remaining width, and eating it wraps every later control out
/// of the panel -- R1's first cut made the options bar collapse to just the document
/// tab (tool identity, parameters, zoom all invisible).
pub(super) fn group_gap(ui: &mut Ui) {
    if ui.layout().is_vertical() {
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 12.0), Sense::hover());
        theme::groove(
            ui.painter(),
            rect.left() + 4.0,
            rect.right() - 4.0,
            rect.center().y,
        );
    } else {
        let h = ui.available_height();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(12.0, h), Sense::hover());
        theme::groove_v(
            ui.painter(),
            rect.center().x,
            rect.top() + 5.0,
            (rect.bottom() - 5.0).max(rect.top() + 6.0),
        );
    }
}

/// 一张分组卡（设计稿 `.grp`）：12px 内距、8px 圆角、6% 白环、卡与卡之间 8px。
/// 组头也画在卡内——这是「卡中卡」真正成立的地方；把组头留在卡外，卡只剩内容
/// 一条，标题与卡会读成两个互不相干的层。
///
/// One group card (design `.grp`): 12 px padding, 8 px radius, 6 % white ring and
/// 8 px between cards. The header lives inside as well -- that is what makes the
/// card-in-card read as one block; leaving the header outside turns the card into a
/// strip of body that is visually unrelated to its title.
pub(super) fn group_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    // ACR 连续面板口径（视觉验收 2026-11-11：卡中卡四件套=割裂感首因）：
    // 去 1px 白环、圆角收到 4、内外底色差减半（group_fill 已按验收调），
    // 组间缝 18→10——组仍可辨（靠底色差），但整列读作一张连续面板。
    //
    // ACR continuous-panel convention (visual audit 2026-11-11: the card-in-card
    // quartet was the top fragmentation source): drop the 1 px ring, radius 8→4,
    // halve the fill contrast, group gap 18→10. Groups stay legible through the fill
    // shift alone; the column reads as one continuous panel.
    // PS 连续行语法（用户 2026-11-13 以 PS「效果 / 曲线 / 混色器」为参考：
    // 「我们的间距有问题不和谐」）。实测 PS 是 `#535353` 通栏 + `#343434` 3px
    // 凹槽线，每行 57+3=60px 严格等距；我们的旧实现是「独立圆角块 + 上亮下暗
    // 双线」，块高随组内内容变，间距实测 17/22/66/81px，忽松忽紧。
    // 现在每张组卡不再自己画壳（无独立底色 / 无圆角 / 无 bevel），只在组间
    // 留固定 10px 并画一道通栏凹槽；内距 12 让折叠组落到 10+12+24+12=58px，
    // 接近 PS 的 60px，且**与展开与否无关**。
    //
    // PS continuous-row grammar (user, 2026-11-13, citing PS's Effects / Curves /
    // Color Mixer: "our spacing is off"). Measured: PS is a #535353 full-width band
    // with a #343434 3 px groove, every row exactly 57+3=60 px. Our old version used
    // independent rounded cards with a lit/dark bevel pair; card height followed the
    // group's content, so gaps measured 17/22/66/81 px -- uneven. Now a group paints
    // no shell of its own (no separate fill, radius or bevel): only a fixed 10 px gap
    // with one full-width groove. 12 px inner padding puts a collapsed group at
    // 10+12+24+12=58 px, close to PS's 60 px and independent of expanded state.
    let (gap, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), Sense::hover());
    theme::groove(ui.painter(), gap.left(), gap.right(), gap.center().y);
    let out = egui::Frame::NONE
        .inner_margin(egui::Margin::same(12))
        .show(ui, add);
    out.inner
}

/// 选项栏的分段竖线（界面稿 .opt-sep）：1 × 20 的 divider 竖线，
/// 把「工具身份 / 工具参数 / 上下文」三段隔开 —— 不用线的话，
/// 参数一多就糊成一条连续的按钮带。
pub(super) fn opt_sep(ui: &mut Ui) {
    // 竖分隔也用亮暗双线：PS 的工具选项栏靠凹槽把「工具 / 参数 / 上下文」
    // 三段分开，单条 1px 灰线在亮面板上几乎看不见，整行就糊成一条带。
    //
    // A light/dark bevel for the vertical separator too: PS grooves divide the
    // tool / parameter / context groups; one faint grey hairline disappeared on
    // the light panel and the whole row flattened into a band.
    let (rect, _) = ui.allocate_exact_size(Vec2::new(2.0, 20.0), Sense::hover());
    theme::bevel_v(ui.painter(), rect.left(), rect.top(), rect.bottom());
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
            "基本影调与色彩；眼睛只旁路预览，不删除参数。\n曝光以 EV 计，其余参数相对中间调调整。\n高光过渡（对齐 LR）：曝光推过 1 的区域走有理肩部缓收，不再硬剪。\n线性对比（对齐 LR）：对比度改在线性光域计算，暗部响应更接近 LR。\n黑场/白场软化（对齐 LR）：两端用抛物线软拐点收进 [0,1]，直方图边缘不再堆尖。"
        }
        "curve" => {
            "三种调法：参数曲线按亮度区间（阴影、中间调、高光）拉起落，另有三条分界；点曲线是 RGB 与三条单通道，坐标即输入输出值；色阶在输入黑白场、灰度系数与输出范围上直接拖动。\nHDR 尾部（对齐 LR）：x>1 的过曝层次按曲线末端斜率延伸进曲线，不再硬截。"
        }
        "mixer" => {
            "两种模式：HSL 按颜色范围调整色相、饱和度与明亮度；黑白混合决定各色相在灰度中的权重。"
        }
        "grade" => {
            "中间调大轮在上，阴影 / 高光小轮并列在下（ACR 的品字布局）；三只轮各带色相、饱和度与亮度。"
        }
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
        "profile" => "渲染基调：风格与相机配置（DCP）定颜色的起点；眼睛只旁路预览，不删除参数。",
        "tone-extra" => {
            "基本组让位后的十三根推子：高光过渡、线性对比、黑白场及其软化、纹理、清晰度、去朦胧与色彩浓度。"
        }
        "geometry" => {
            "透视校正：自动直立只修上下透视（竖线收敛），四角是手动兜底；不修旋转与畸变。"
        }
        "retouch" => {
            "修复与仿制两种模式，笔刷半径与羽化在这里调；落点在画布上（工具条选「修复」）。"
        }
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

/// 组头琥珀点的判据：规格表里的组问 `state::group_changed`；表外的「配置文件」组按
/// 「换了风格、或挂了 DCP」判——这两件是渲染基调，不在 `controls::SPECS` 里，也就
/// 没有 `specs_of_group("profile")` 可问。其余表外组（蒙版 / 蒙版参数 / 序列页那些
/// 动作组）没有「偏离默认」可言，一律不点亮。
///
/// 口径写在这里而不是调用点：`group_header` 在全仓有四十多处调用，加一个形参会
/// 牵动本任务范围外的文件；判据集中一处也就不会出现「两组各写一份、迟早对不上」。
///
/// The header dot's criterion: groups in the spec table ask `state::group_changed`;
/// the out-of-table Profile card tests "style switched, or a DCP applied" -- both are a
/// rendering basis, absent from `controls::SPECS`, so there is no
/// `specs_of_group("profile")` to ask. The other out-of-table groups (masks, mask
/// params, the sequence page's action groups) have nothing to deviate from and never
/// light up.
///
/// The rule lives here rather than at the call site because `group_header` has forty-odd
/// callers and a sixth parameter would reach files outside this task; one place also
/// keeps two copies from drifting apart.
fn header_dot(state: &AppState, key: &str) -> bool {
    // 风格 0 是标准（`GradeParams::default().style`），也是复位会保留的那个值。
    // Style 0 is standard (`GradeParams::default().style`) -- the value reset keeps.
    if key == "profile" {
        return state.params.style != 0 || !state.params.dcp_profile.is_empty();
    }
    state.group_changed(key)
}

pub(super) fn group_header(
    ui: &mut Ui,
    state: &mut AppState,
    key: &'static str,
    title: &str,
    resettable: bool,
) -> bool {
    let changed = header_dot(state, key);
    // 设计稿 `.dot{width:5px;height:5px;background:var(--amber)}`：琥珀是「这一组
    // 动过」的标记色，和读数框里改过的数值同一个语义；旧版用强调蓝，读起来像
    // 「选中」而不是「已改动」。
    //
    // The design's `.dot` is 5 px amber (--amber, #f5b83d) -- the same "this changed"
    // colour as a moved readout. The old accent blue read as "selected", not "edited".
    const DOT_AMBER: Color32 = Color32::from_rgb(0xF5, 0xB8, 0x3D);
    let title = crate::i18n::tr(title);
    // 卡中卡（设计稿 v10.6/v12 的 `.grp`）：每个分组是一张独立圆角卡片
    // ——`background:#2a2d32`、`border-radius:8px`、`padding:12px`、
    // `box-shadow:0 0 0 1px rgba(255,255,255,.06)`。旧版是「通栏 hairline
    // 分隔」的 ACR 语法：八组排下来读成一张长表，用户点名「质感不对」的根源
    // 就在这里——分隔线只切断，不建立「这是一块」。
    //
    // Card-in-card, per the design's `.grp`: each group is its own rounded card --
    // `#2a2d32` fill, 8 px radius, 12 px padding, a 1px ring via box-shadow. The old
    // full-width hairline separators were ACR's grammar: eight groups read as one long
    // table, and that is exactly what "the texture is wrong" points at -- a rule only
    // cuts, it never says "this is one block".
    // 组头 24px（设计稿 `.grp-h{height:24px}`）。旧版 32px 是卡外组头的口径；
    // 组头搬进卡后，24px + 卡内距 12 才和参考图的呼吸一致。
    //
    // Header 24 px (design `.grp-h`). The old 32 px belonged to a header outside the
    // card; once the header moves inside, 24 px plus the card's 12 px padding matches
    // the reference's breathing room.
    let gh = 24.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), gh), Sense::click());
    if state.dock_focus.as_deref() == Some(key) {
        // 只推一次：egui 在下一帧把目标滚到位，截图时机在 900ms 之后，足够稳定。
        resp.scroll_to_me(Some(Align::Min));
        state.dock_focus = None;
        // 顺带展开：demo 编辑会把没动的分组折起来，只滚不展等于对着一张
        // 折叠的分组头截图，面板里那条渐变轨根本没上屏。
        state.collapsed.remove(key);
    }
    let p = ui.painter();
    // 组头只留「箭头 + 标题 + 眼睛」三件：箭头表示开合，标题是分区名，
    // 眼睛是这一组是否参与预览。多一枚滑杆图标只是重复「这里是参数区」，
    // 却把标题推右 9px、让整列失去共同的左缘。
    //
    // The header keeps exactly three things: the chevron (open state), the title
    // (section name), and the eye (whether this group renders into the preview). A
    // sliders glyph only repeats "parameters live here" while pushing every title
    // 9 px right and breaking the column's shared left edge.
    let open = !state.collapsed.contains(key);
    // 悬停是一整条圆角高亮，不是方条：macOS 侧栏分组头的口径。
    if resp.hovered() {
        // 通栏高亮：PS 的组头是连续面板里的一段，不是浮起的圆角按钮。
        // Full-width hover: a PS header is a segment of one continuous panel, not a
        // raised rounded button.
        let bar = Rect::from_min_max(
            Pos2::new(rect.left(), rect.top() + 1.0),
            Pos2::new(rect.right(), rect.bottom() - 1.0),
        );
        p.rect_filled(bar, 0.0, elev());
    }
    icons::paint(
        p,
        Rect::from_center_size(
            Pos2::new(rect.left() + 6.0, rect.center().y),
            Vec2::splat(11.0),
        ),
        if open {
            Glyph::ChevronDown
        } else {
            Glyph::ChevronRight
        },
        // R2：箭头 hover 变强调色——开合是这一行的首要交互，hover 时给色
        // 而不是只提亮度，ACR 的分区箭头同此。
        if resp.hovered() {
            accent_hover()
        } else {
            muted()
        },
    );
    // 组名 13px / 600（设计稿 `.grp-h .lb`）：它是卡片的标题，比下面的
    // 参数名重一档才立得住卡片层级；用真粗体字族而非描边加粗。
    //
    // Group name 13 px / 600 (the design's `.grp-h .lb`): as the card's title it
    // must outweigh the 12 px parameter labels to carry the card hierarchy; this
    // uses the real bold family, not a faux-bold stroke.
    // 16px 真粗体：PS 参考里组名比参数标签重一号、也更亮，靠排版而不是靠
    // 边框建立层级。行高仍是设计稿的 24px（判据锁着），16px 字放得下。
    //
    // 16 px real bold: in the PS reference group names outweigh the parameter labels
    // by one step, so hierarchy comes from type, not from a border. The row stays at
    // the design's 24 px (pinned by a judgement), which fits 16 px type.
    let title_galley = p.layout_no_wrap(title.to_owned(), bold(SIZE_TITLE), text());
    // 卡内左缘：设计稿 `.grp-h` 的箭头与标题相距约 6px，卡内距 12px 已经把
    // 标题推到 x≈18。这里取 18，标题与下面 `.srow` 的标签列不再差一截。
    //
    // Left edge inside the card: the design puts about 6 px between the chevron and
    // the title, and the card's 12 px padding already lands the title at x~18. Using
    // 18 keeps the title and the labels below it on the same column.
    let title_x = rect.left() + 18.0;
    p.galley(
        Pos2::new(title_x, rect.center().y - title_galley.size().y * 0.5),
        title_galley.clone(),
        text(),
    );
    // 组内改动指示（设计稿 `.grp-h .dot`）：紧跟在组名右侧的一枚 5px 琥珀点，
    // 组里没有参数偏离默认就不画。设计稿把它排在标题之后（`.lb` 与 `.help`
    // 之间），不是贴右缘——贴右缘时它和眼睛、复位挤成一团，反而看不出是谁的标记。
    //
    // In-group change dot (design `.grp-h .dot`): a 5 px amber dot right after the
    // group name, painted only when some parameter in the group deviates from its
    // default. The design puts it after `.lb` and before `.help`, not against the right
    // edge -- there it crowds the eye and reset and reads as their label.
    if changed {
        p.circle_filled(
            Pos2::new(title_x + title_galley.size().x + 8.0, rect.center().y),
            2.5,
            DOT_AMBER,
        );
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
        // 眼睛贴右缘：PS 参考图里组头眼睛在内容右缘内 6~18px，此前为了给复位
        // 让位退到 right−30，整行右侧空出一大截。现在眼睛占住最右一格，复位
        // 只在悬停时于它左侧滑出。
        //
        // Eye against the right edge: in the PS reference the header eye sits 6-18 px
        // inside the content edge; it used to retreat to right-30 to make room for the
        // reset, leaving a conspicuous gap. The eye now owns the rightmost slot and the
        // reset slides out to its left on hover only.
        let er = Rect::from_center_size(
            Pos2::new(rect.right() - 13.0, rect.center().y),
            Vec2::splat(20.0),
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
                // PS 的眼睛比正文暗一档（参考图 141 对 240）：它是开关，不该和
                // 标题抢视线。此前用 muted，和标签几乎同亮。
                //
                // PS's eye sits one step below body text (141 vs 240 in the reference):
                // it is a switch and should not compete with the title. It used muted
                // before, nearly as bright as the labels.
                dim()
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
    if resettable {
        let reset_x = if bypassable {
            rect.right() - 34.0
        } else {
            rect.right() - 12.0
        };
        let rr = Rect::from_center_size(Pos2::new(reset_x, rect.center().y), Vec2::splat(20.0));
        let rresp = ui.interact(
            rr,
            ui.make_persistent_id(("group-reset", key)),
            Sense::click(),
        );
        if rresp.hovered() {
            p.rect_filled(rr, radius_control(), elev_hover());
        }
        // ACR 极简口径（用户钦定 v3 稿）：复位图标 hover 才显形——组头默认
        // 只读作「箭头 + 名字 + 眼睛」，改动点与复位都是次级信息。
        icons::paint(
            p,
            rr.shrink(4.5),
            Glyph::Reset,
            if rresp.hovered() {
                text()
            } else {
                Color32::TRANSPARENT
            },
        );
        if rresp.clicked() {
            reset_group(state, key);
        }
        rresp.on_hover_text(crate::i18n::t!("重置这一分组"));
    }
    // ⓘ（设计稿 `.grp-h .help`）：右缘图标排的第三格——眼睛最右、复位次之、
    // ⓘ 再左一格；没有眼睛/复位的组，ⓘ 直接占最右一格。10px 字形，20px 热区。
    // 显形随组头悬停 0→1（设计稿 `.grp-h:hover .help{opacity:1}`），0.12s 与
    // 页签下划线的 --tc 同一节奏；热区常驻——帮助不因「看不见」而丢。
    //
    // The info glyph (design `.grp-h .help`): the third slot in the right-edge row --
    // eye rightmost, reset next, info one further left; a group without eye/reset lets
    // info own the rightmost slot. A 10 px glyph on a 20 px hit area. Reveal follows the
    // header hover 0->1 (the design's `.grp-h:hover .help{opacity:1}`) on a 0.12 s ease
    // shared with the tab underline's --tc; the hit area is resident, so help is never
    // lost to invisibility.
    if let Some(help) = group_help(key).map(crate::i18n::tr) {
        let help_x = if bypassable {
            rect.right() - 55.0
        } else if resettable {
            rect.right() - 34.0
        } else {
            rect.right() - 13.0
        };
        let ir = Rect::from_center_size(Pos2::new(help_x, rect.center().y), Vec2::splat(20.0));
        let iresp = ui.interact(
            ir,
            ui.make_persistent_id(("group-help", key)),
            Sense::hover(),
        );
        let t = ui.ctx().animate_bool_with_time(
            ui.make_persistent_id(("group-help-anim", key)),
            resp.hovered(),
            0.12,
        );
        if iresp.hovered() {
            p.rect_filled(ir, radius_control(), elev_hover());
        }
        let base = if iresp.hovered() {
            accent_hover()
        } else {
            dim()
        };
        icons::paint(
            p,
            ir.shrink(5.0),
            Glyph::Info,
            Color32::from_rgba_unmultiplied(
                base.r(),
                base.g(),
                base.b(),
                (base.a() as f32 * t) as u8,
            ),
        );
        iresp.on_hover_text(help);
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
    // 组头随卡走：设计稿 `.grp-h` 下没有分隔线，卡自身就是一层的边界。
    // 旧版通栏 hairline 是「卡外组头」的口径，留在这里会把卡再切一刀。
    // 展开时补 8px 底距（`.grp-h{margin-bottom:8px}`），收起时不补
    // （`.is-collapsed .grp-h{margin-bottom:0}`），否则折叠卡会多出一截下沿。
    //
    // The header travels with the card: the design's `.grp-h` has no rule beneath it
    // because the card itself is the boundary. The old full-width hairline belonged
    // to a header outside the card and would cut the card in two. Open sections add
    // 8 px below the header (`.grp-h{margin-bottom:8px}`); collapsed ones do not
    // (`.is-collapsed .grp-h{margin-bottom:0}`), which would leave a fat lower lip.
    let open_now = !state.collapsed.contains(key);
    if open_now {
        ui.add_space(8.0);
    }
    open_now
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
        ui, key, label, v, min, max, default, decimals, signed, "", None, false, "", None,
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

/// 「仅显示已改动」闸门（设计稿 `.dock.is-only-changed .srow:not(.changed){display:none}`）：
/// 开关打开时，值等于规格表默认的那一行不画，也不占行高——过滤发生在布局之前，
/// 留下的行才会像设计稿那样往上收，而不是在原地留一条空带。
///
/// 默认值取 `spec.default`（`controls` 模块头注释：表是唯一事实源，
/// `spec_defaults_match_grade_defaults` 钉住它与 `GradeParams::default()` 同值），
/// 所以「已改动」的判据和读数变琥珀、复位点出现是同一个。
///
/// The "only show changed" gate (design `.dock.is-only-changed .srow:not(.changed)`):
/// while the switch is on, a row whose value equals the spec's default paints nothing
/// and reserves no height -- the filter runs before layout, so the remaining rows close
/// up instead of leaving blank bands.
///
/// The default comes from `spec.default` (the spec table is the single source of truth per
/// the `controls` module docs; `spec_defaults_match_grade_defaults` pins it to
/// `GradeParams::default()`), so "changed" here means the same thing as the amber readout
/// and the reset dot.
pub(super) fn spec_visible(state: &AppState, s: &controls::ControlSpec) -> bool {
    !state.only_changed || (controls::param_get(&state.params, s.field) - s.default).abs() > 1e-9
}

#[allow(clippy::too_many_arguments)]
/// 按规格表渲染一条推子。标签/范围/默认/精度/单位/渐变全部来自
/// `controls::SPECS`——加参数只登记表，不再在面板里摊九个实参。
pub(super) fn spec_slider(ui: &mut Ui, state: &mut AppState, s: &controls::ControlSpec) -> bool {
    // 「仅显示已改动」也在这里挡一道：分区循环（分级 / 细节 / 光学）直接调
    // `spec_slider`，不经过 `spec_field`，闸门只放一处就会漏掉它们。
    //
    // The gate also sits here: the section loops (grade / detail / optics) call
    // `spec_slider` directly and never pass through `spec_field`, so a gate in one place
    // would miss them.
    if !spec_visible(state, s) {
        return false;
    }
    // 色温：把 -100..100 增量换算成绝对开尔文一并显示，口径与 XMP 导出
    // 的绝对温度一致（5500 K 中性点、每档 45 K）。仅影响这一行读数。
    //
    // Temperature: convert the -100..100 increment to absolute kelvin for the
    // readout, using the same scale as the XMP absolute-temperature mapping
    // (5500 K neutral, 45 K per step). Only this row's readout is affected.
    let kelvin = if s.key == "dev.temperature" {
        Some(5500.0 + 45.0 * controls::param_get(&state.params, s.field))
    } else {
        None
    };
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
            kelvin,
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
    // 色温这类参数额外显示的绝对开尔文读数（`Some(5500 + 45·v)`）。
    // 只是显示层：落盘、撤销、XMP 仍用原始 -100..100 增量值。
    //
    // An optional absolute-kelvin readout for parameters such as temperature
    // (`Some(5500 + 45·v)`). Display only: persistence, undo and XMP keep the
    // raw -100..100 increment.
    kelvin: Option<f64>,
) -> bool {
    // ── 紧凑两行：标签 + 读数在首行，通栏轨道压在第二行 ──────────────
    // 单行式把轨道塞在标签与读数框之间，只剩约 200px，右边还空出 12px 的死区；
    // 并排看 PS，轨道是**从标签左缘一直铺到读数框右缘**、并从框下方穿过的，滑块
    // 走的是整条量程。现在照参考图的版式改回两行，但节距仍是 30px（参考图 31），
    // 密度一分不丢，只是把原来浪费在中缝的空白换成了轨道长度。
    //
    // Compact two lines: label + readout on the first line, a full-width rail on the
    // second. The single-line form squeezed the rail between the label and the box
    // (~200 px) and left a 12 px dead gutter; side by side with PS the rail there runs
    // from the label's left edge all the way under the readout box to its right edge,
    // and the handle travels the whole range. This restores the reference's two-line
    // form while keeping the 30 px pitch (reference: 31), so nothing gets looser --
    // the space that used to be dead becomes rail.
    const VALUE_W: f32 = 44.0;
    const FIELD_H: f32 = 18.0;
    // 复位点 5px：设计图 `.slider-row .s-reset{width:5px;height:5px}`。命中区改用
    // `ui.interact` 的矩形另行放大，不靠占位宽度换手感。
    //
    // 5 px per the design's `.slider-row .s-reset{width:5px;height:5px}`. The hit area
    // is widened through the `ui.interact` rect instead of by reserving layout width.
    const RESET_W: f32 = 5.0;
    // 行内不留横向内距（设计稿 `.srow{padding:6px 0}`）：卡内距 12px 已经给出
    // 标签列的位置，再叠 6px 就把标签推到离卡缘 18px，同时把轨道两侧各削掉 6px
    // （实测轨道 242，设计稿 255）。去掉之后标签落在卡缘内 12px，轨道 = 卡内宽
    // 254，与设计稿的 255 对齐。
    //
    // No horizontal row padding (design `.srow{padding:6px 0}`): the card's 12 px
    // padding already places the label column; another 6 px pushed the label to 18 px
    // from the card edge and shaved 6 px off each end of the rail (measured 242 against
    // the design's 255). Without it the label sits 12 px inside the card and the rail
    // spans the card's inner width, 254, matching the design's 255.
    const ROW_PAD: f32 = 0.0;
    // 节距按设计稿 `.srow`：上下各 6px 内距 + 18px 首行 + 6px 间隔 + 3px 轨道 =
    // 39px（标签中线 15、轨道中线 31.5）。egui 在两次 allocate 之间还会补一段
    // item_spacing，所以行内只占 39 **减去**它——旧写法 `row_h() + 6` 再叠 4px
    // 间距，实测节距 42，整体比设计稿松 3px。三档密度仍随 `row_h()` 递进
    // （紧凑 / 标准 / 宽松），只把基准从 42 收到 39。
    //
    // Pitch per the design's `.srow`: a 6 px pad top and bottom + an 18 px top line +
    // a 6 px gap + a 3 px rail = 39 px (label centre 15, rail centre 31.5). egui adds
    // `item_spacing` between allocations, so the row takes 39 *minus* that -- the old
    // `row_h() + 6` plus 4 px spacing measured 42, three pixels looser than the design.
    // The three density steps still follow `row_h()`; only the base moves.
    let gap_y = ui.spacing().item_spacing.y;
    let pitch = row_h() + 3.0;
    let step = pitch + gap_y;
    // 行内两条线按**真实节距**（含行间空隙）的比例定位：0.385 / 0.81 就是设计稿的
    // 15/39 与 31.5/39，标签与轨道因此不受行间空隙变化影响。旧值 0.38/0.81 用在
    // 35px 的行内高上，标签会往上浮 1.5px、轨道贴着行底。
    //
    // The two lines are placed by proportion of the *real* pitch (including the
    // inter-row gap): 0.385 / 0.81 are the design's 15/39 and 31.5/39, so the label and
    // rail do not drift when the gap changes. The old 0.38/0.81 applied to the 35 px
    // in-row height floated the label 1.5 px up and glued the rail to the row's bottom.
    let (l1_y, track_y) = (step * 0.385, step * 0.81);
    // 标签 12 / 读数 12：v12 设计稿 `.lab` 与 `.val` 同为 12px，只有颜色与字重
    // 分主次——标签 `#c6c9d1` 常规，读数 `#f2f3f7` 中等。此前 13/11 的阶梯是 PS
    // 的口径，套在 300px 面板上让参数名偏大、读数偏小，整列灰度还不统一。
    //
    // Label 12 / readout 12: in the v12 design both `.lab` and `.val` are 12 px, and
    // only colour and weight separate them -- label `#c6c9d1` regular, value `#f2f3f7`
    // medium. The old 13/11 ladder was Photoshop's; on a 300 px panel it oversized the
    // names and shrank the numbers.
    const LABEL_SIZE: f32 = SIZE_LABEL;
    const VALUE_SIZE: f32 = SIZE_LABEL;
    let label = crate::i18n::tr(label);
    let tip = crate::i18n::tr(tip);

    let (rect, row_resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), pitch), Sense::hover());
    let hot_row = ui.rect_contains_pointer(rect);
    let changed_now = (*v - default).abs() > 1e-9;

    // 两行：首行放标签与读数框，第二行是通栏轨道。
    // First line holds the label and readout box; the second the full-width rail.
    let l1y = rect.top() + l1_y;
    let track_y = rect.top() + track_y;
    // 右侧一列：读数框贴右缘（与组头眼睛、混色器频段框同一竖线），复位点收在
    // 框**左侧**的槽里，悬停才显形。
    //
    // Right-hand column: the readout box sits on the content edge (one vertical line
    // with the header eye and the mixer band boxes), and the reset lives in the gutter
    // to its **left**, appearing on hover.
    let field = Rect::from_min_size(
        Pos2::new(rect.right() - ROW_PAD - VALUE_W, rect.top()),
        Vec2::new(VALUE_W, FIELD_H),
    );
    let reset = Rect::from_center_size(Pos2::new(field.left() - 7.0, l1y), Vec2::splat(RESET_W));
    // 轨道通栏：左端与标签同起，右端与读数框右缘同止——参考图里轨道就是从框下方
    // 穿过去的，滑块能走到数值框的正下方。
    //
    // Full-width rail: starts with the label, ends flush with the readout box's right
    // edge -- in the reference the rail passes beneath the box and the handle travels
    // all the way under it.
    let rail_left = rect.left() + ROW_PAD;
    let rail_right = rect.right() - ROW_PAD;
    {
        let p = ui.painter();
        if hot_row {
            // 通栏轻微提亮 + 左缘 2px 强调竖条（ACR 激活行读法：视线从左缘
            // 就知道「这一行是我的」——比单纯提亮多一根锚线）。
            p.rect_filled(
                rect.shrink2(Vec2::new(0.0, 1.0)),
                radius_control(),
                row_hover(),
            );
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(rect.left() + 2.0, rect.top() + 2.0),
                    Pos2::new(rect.left() + 4.0, rect.bottom() - 2.0),
                ),
                1.0,
                accent().gamma_multiply(0.75),
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
        // 静止态只留框线、不填底：ACR 的读数是「面板底色上的一个矩形轮廓」，
        // 我们原来填了比面板沉一档的凹槽色，一行行看下来像七个 Web 输入框。悬停
        // 或拖动时才填底，把「可点可拖」留给交互态而不是常驻装饰。
        //
        // At rest only the outline, no fill: ACR's readout is a rectangle drawn on the
        // panel colour, while a recessed fill made a column of rows look like seven web
        // input fields. The fill appears on hover or drag, so "clickable" is an
        // interaction cue instead of permanent decoration.
        // 读数框设计稿口径（.srow .val）：静止时**无边框**——纯文字浮在卡片底上；
        // 悬停才出现 field 底 + ring 描边（1px 低对比环，不是 border_strong）。
        // 常驻描边让一列读数读成七个输入框，是「质感不对」的来源之一。
        //
        // Readout per the design's .srow .val: no border at rest -- bare text on the card
        // fill; hover brings a field fill plus a 1px low-contrast ring. A permanent
        // outline makes a column of readouts read as seven input boxes.
        if ui.rect_contains_pointer(field) {
            p.rect_filled(field, radius_control(), theme::field());
            p.rect_stroke(
                field,
                radius_control(),
                Stroke::new(1.0, accent().gamma_multiply(0.7)),
                egui::StrokeKind::Inside,
            );
        }
        // 6 字截断：13 号字体下 6 个汉字约 78px，落在 81px 标签列里刚好排满，
        // 再长就让位给轨道，宁可少两个字，也不让标签爬进推子的地盘。
        // 截断长度按语言分档：英文同列只放得下 ~11 个半角字符，仍按 6 截会把
        // "Highlights" 截成 "Highli…"。档位存在语言表里（`Catalog::ellipsis_chars`），
        // 加语言时由那张表自己声明，这里不再出现具体语言的名字。
        let shown = theme::ellipsis(label, crate::i18n::current().ellipsis_chars());
        let label_pos = Pos2::new(rect.left() + ROW_PAD, l1y);
        p.text(
            label_pos,
            Align2::LEFT_CENTER,
            shown.clone(),
            font(LABEL_SIZE),
            // 设计稿 `.lab{color:var(--text2)}`：标签是次级文字，读数才是主角。
            // 旧版标签用正文白，和读数抢亮度，一列看下来分不出谁是要读的数。
            //
            // The design's `.lab` is secondary (`--text2`); the readout is the headline.
            // White labels competed with the numbers, so the column had no focal point.
            muted(),
        );
        // 绝对开尔文读数：RAW 调白平衡时人要看的是「多少 K」，不是 -21 这种
        // 相对增量。换算与 XMP 导出的绝对口径同源（`K = 5500 + 45·v`），
        // 但只画在这里，不改参数值、落盘、历史或撤销标注。
        // 标签按实测宽定位，避免中英文宽度不同压到文字上。
        //
        // Absolute-kelvin readout: when grading RAW white balance you need "how
        // many K", not a relative increment like -21. The conversion matches the
        // XMP absolute-kelvin convention (`K = 5500 + 45·v`) but is paint-only:
        // the parameter, persistence, history and undo labels are untouched.
        // The label is measured so CJK and Latin widths cannot overlap.
        if let Some(k) = kelvin {
            let label_w = p.layout_no_wrap(shown, font(LABEL_SIZE), muted()).size().x;
            p.text(
                Pos2::new(label_pos.x + label_w + 8.0, l1y),
                Align2::LEFT_CENTER,
                format!("{} K", k.round() as i32),
                mono(MICRO),
                dim(),
            );
        }
    }

    let old_text = ui.style().visuals.override_text_color;
    let old_font = ui.style().override_font_id.clone();
    // 读数颜色按设计稿 .srow.changed .val：**改过的值 = 琥珀色 #f5b83d**，
    // 未动值 = 正文白。琥珀是"这根推子动过"的标记色（设计稿把琥珀固定为
    // 时间/改动语义），扫一眼就知道哪几根动了——正文白做不到这一点。
    // （用户早前"满屏蓝读数"的问题不回归：蓝色仍只留给交互态。）
    //
    // Readout colour per the design's .srow.changed .val: a **changed value is amber
    // #f5b83d**, untouched stays body-white. Amber is the design's "this slider moved"
    // marker (time/change semantics), so a glance tells which sliders moved -- plain
    // white cannot. (The earlier "wall of blue readouts" does not come back: blue
    // remains interaction-only.)
    ui.style_mut().visuals.override_text_color = Some(if changed_now {
        // 设计稿 --amber #f5b83d：与 theme 的 warn(#FF9F0A) 不是一个语义，
        // 这里直接用字面量并在判据里钉住，不挪用警告色。
        //
        // The design's --amber #f5b83d is not theme::warn (#FF9F0A); the literal is used
        // here and pinned by a test, rather than borrowing the warning colour.
        Color32::from_rgb(0xF5, 0xB8, 0x3D)
    } else {
        // 设计稿 `.val{color:var(--text)}`：读数用正文白，不是次级灰。
        //
        // The design's `.val` is body white (`--text`), not the secondary grey.
        text()
    });
    // 读数与标签同族（2026 字族统一规范）：设计稿里数值就是正文字体，不是等宽
    // 体。小数点对齐靠 `Layout::right_to_left` 的右对齐，不靠换一张脸；用等宽会
    // 让「0 / 0.35 / -12」和左边的中文标签明显不是一套字。`DragValue` 没有
    // `.font()`，只能在作用域里压 `override_font_id`，画完立刻还原。
    //
    // Readouts share the body family (2026 type-system rule): in the design the
    // numbers are the same face as the labels, not a monospace one. Decimal alignment
    // comes from right-to-left layout, not a different typeface -- a mono face made
    // "0 / 0.35 / -12" visibly belong to another system than the Chinese labels.
    // `DragValue` has no `.font()`, so the id is overridden for this scope only.
    ui.style_mut().override_font_id = Some(font(VALUE_SIZE));
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
                    .insert(egui::TextStyle::Button, mono(VALUE_SIZE));
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
    // 「手在推子上」的全局标记：检查器靠它在这段时间降透明度，让画面透出来。
    // 只认真实的拖动（不是按住不动）：按住但没动时卡片保持实色，色彩判断的参照
    // 系不该因为一次误按就变。
    //
    // A global "hand is on a slider" flag: the inspector fades while it is set so the
    // frame shows through. Only a real drag counts -- a press without movement keeps the
    // card opaque, since the colour reference should not change on a stray click.
    if t.dragged() {
        ui.ctx().data_mut(|d| {
            d.insert_temp(egui::Id::unique("slider-drag"), true);
        });
    }
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
            // 通用轨道设计稿口径（`.track{background:#43464e}`）：一条平色细轨，
            // 亮度从卡底只抬一档。旧版铺了一条「暗→白」渐变，两侧推子看起来像
            // 在发光，整列轨道比标签还抢眼——这是「质感不对」的直观来源。
            // 量程由滑块位置与端点读数表示，轨道不需要用明度再讲一遍。
            //
            // Generic rail per the design (`.track` is a flat `#43464e`): one thin
            // flat bar, one step above the card. The old dark -> white ramp made every
            // rail glow and outshout its label -- a direct source of the "texture is
            // wrong" reading. Range is told by the knob and the numbers, not by a
            // brightness gradient.
            p.rect_filled(rail, rail_h * 0.5, theme::slider_rail());
        }
    }
    // 参考图的轨道上没有中缝刻线：零位由滑块本身的位置读出，未动的推子滑块就在
    // 正中。此前每根跨零推子都画一道暗刻线，整列叠起来像一排虚线。
    //
    // The reference rail carries no centre notch: the handle itself marks zero, and
    // an untouched slider sits exactly in the middle. The old per-slider tick stacked
    // into a dashed column down the whole group.
    // knob 配色：亮色实心 + 1px 暗描边，压在渐变轨上仍分得清；hover 加一圈细描边。
    //
    // Knob colour: a bright solid fill with a 1 px dark outline so it stays legible on
    // the gradient rail; hover adds a thin halo.
    let vx = tx(rail_pos(*v));
    let knob = Pos2::new(vx, track_y);
    // 圆形手柄：用户 2026-11-07 明确要求「三角改回之前的圆形」。PS 的推子确实是
    // 三角，但本项目在 1280 宽、72% 缩放的实测下三角只有 9×10，读起来像一个小箭头
    // 而不是可抓的把手；圆点在同样尺寸下更容易命中，也和本界面其它圆形控件同一族。
    //
    // 两行版式里轨道中线离标签只有 13px，12px 的大圆点会顶到标签下缘；照参考图
    // 的手柄量级收到 9px（悬停 11、按下 8），白点才退成「量程上的一个点了」。
    //
    // Round knob: on 2026-11-07 the user asked to revert the triangle to the earlier
    // circle. Photoshop's fader really is a triangle, but measured on this UI at 1280 px
    // and 72% zoom the triangle is only 9x10 and reads as a small arrow rather than a
    // grabbable handle; a dot is easier to hit at that size and matches the other round
    // controls here.
    //
    // In the two-line form the rail sits only 13 px below the label, so a 12 px dot
    // collided with the label's lower edge; matching the reference's handle scale, it is
    // now 9 px (11 hover, 8 pressed), so the white dot reads as a point on the range.
    // 设计稿 `.track i`：10×10、`#c9ccd3`、`box-shadow:0 1px 2px rgba(0,0,0,.5)`。
    // 所以静止是 10px 圆点 + 一道柔和落影，没有黑描边也没有亮白填充；悬停微胀
    // 到 11px 给指认，按下缩到 9px 并转 accent——按压反馈仍要在，但只出现在
    // 手指按下的那一瞬，不常驻。
    //
    // The design's `.track i` is 10x10 `#c9ccd3` with `0 1px 2px rgba(0,0,0,.5)`:
    // a 10 px dot with a soft drop shadow, no dark outline and no bright white fill.
    // Hover nudges it to 11 px; pressing shrinks it to 9 px and turns it accent, so
    // press feedback only exists while the finger is down.
    let r = if t.dragged() {
        4.5
    } else if t.hovered() {
        5.5
    } else {
        5.0
    };
    p.circle_filled(
        knob + Vec2::new(0.0, 1.2),
        r + 0.4,
        Color32::from_black_alpha(55),
    );
    p.circle_filled(knob, r, if t.dragged() { accent() } else { theme::knob() });
    // 复位点只在悬停这一行时画：每条推子常年挂一颗蓝点，整列看起来像一排指示灯，
    // 既挤又吵；需要时它就在读数框左边。
    //
    // The reset dot is painted only while this row is hovered: a blue dot on every
    // adjusted slider turns the column into rows of indicator lights, which is both
    // noisy and cramped. It is there, left of the readout box, when needed.
    if (*v - default).abs() > 1e-9 && hot_row {
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
    // 数值框是子 Ui：DragValue 的最小交互高只有 22，比整行 46 矮，子 Ui 的
    // min_rect 会把父游标推进到「框底 + 行距」，行与行因此叠在一起（实测节距 26，
    // 应为 50）。画完把游标按回整行底部。
    //
    // The value box is a child UI: DragValue's minimum interaction height is 22, short of
    // the 46 px row, and the child's min_rect advances the parent cursor to "field bottom
    // + spacing" -- successive rows overlapped (measured pitch 26, expected 50). Re-assert
    // the full row height on the parent cursor once the row is painted.
    if ui.cursor().top() < rect.bottom() {
        ui.advance_cursor_after_rect(rect);
    }
    changed
}

/// 混色器的紧凑频段行（参考图实测：行距 31、标签与数值框同一行、渐变轨下一行）。
/// 与普通滑块同一版式，只是行高再压 2px——八条频段同屏，整组高度才是参考图的量级。
///
/// Compact band row for the mixer (measured on the reference: 31 px pitch, label and
/// value field on one line, gradient rail on the next). Same two-line form as the
/// normal sliders, only 2 px shorter, so eight bands fit the reference's group height.
#[allow(clippy::too_many_arguments)]
pub(super) fn band_row<K: std::hash::Hash + std::fmt::Debug>(
    ui: &mut Ui,
    key: K,
    label: &str,
    v: &mut f64,
    min: f64,
    max: f64,
    default: f64,
    stops: Option<&'static [(f32, Color32)]>,
) -> bool {
    const H: f32 = 28.0;
    const VALUE_W: f32 = 44.0;
    const FIELD_H: f32 = 18.0;
    let label = crate::i18n::tr(label);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), H), Sense::hover());
    let hot_row = ui.rect_contains_pointer(rect);
    let changed_now = (*v - default).abs() > 1e-9;
    let l1y = rect.top() + 9.0;
    let track_y = rect.top() + 22.0;

    // 读数框与普通滑块同列：右侧留 6px，宽度同为 44——混色器八行与上面的
    // 滑块共用一条竖直的数值列，PS 参考里所有框都落在同一条线上。
    //
    // Same value column as the normal sliders: 6 px inset, 44 wide. The eight mixer
    // rows and the sliders above share one vertical number column, as every box does
    // in the PS reference.
    let field = Rect::from_min_size(
        Pos2::new(rect.right() - 6.0 - VALUE_W, rect.top()),
        Vec2::new(VALUE_W, FIELD_H),
    );
    {
        let p = ui.painter();
        if hot_row {
            p.rect_filled(
                rect.shrink2(Vec2::new(0.0, 1.0)),
                radius_control(),
                row_hover(),
            );
        }
        p.rect_filled(field, radius_control(), theme::field());
        p.rect_stroke(
            field,
            radius_control(),
            Stroke::new(
                1.0,
                if ui.rect_contains_pointer(field) {
                    accent().gamma_multiply(0.7)
                } else {
                    border()
                },
            ),
            egui::StrokeKind::Inside,
        );
        p.text(
            Pos2::new(rect.left() + 6.0, l1y),
            Align2::LEFT_CENTER,
            theme::ellipsis(label, crate::i18n::current().ellipsis_chars()),
            font(SIZE_LABEL + 1.0),
            text(),
        );
    }

    let old_text = ui.style().visuals.override_text_color;
    let old_font = ui.style().override_font_id.clone();
    ui.style_mut().visuals.override_text_color = Some(if changed_now { text() } else { value() });
    ui.style_mut().override_font_id = Some(font(SIZE_SMALL));
    let mut resp = in_field(ui, Color32::TRANSPARENT, Color32::TRANSPARENT, |ui| {
        ui.scope_builder(
            UiBuilder::new()
                .id_salt(("band-value", &key))
                .max_rect(field.shrink2(Vec2::new(3.0, 0.0)))
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                ui.style_mut()
                    .text_styles
                    .insert(egui::TextStyle::Button, mono(SIZE_SMALL));
                ui.add(
                    egui::DragValue::new(v)
                        .range(min..=max)
                        .speed((max - min) / 300.0)
                        .fixed_decimals(0)
                        .custom_formatter(move |n, _| theme::number(n, 0, true)),
                )
            },
        )
        .inner
    });
    ui.style_mut().visuals.override_text_color = old_text;
    ui.style_mut().override_font_id = old_font;
    let mut changed = resp.changed();

    let rail = Rect::from_min_max(
        Pos2::new(rect.left() + 6.0, track_y - 1.5),
        Pos2::new(rect.right() - 6.0, track_y + 1.5),
    );
    let span = rail.width().max(1.0);
    let tx = |val: f64| {
        rail.left() + span * (((val - min) / (max - min).max(1e-9)).clamp(0.0, 1.0)) as f32
    };
    let hit = Rect::from_min_max(
        Pos2::new(rail.left(), track_y - 8.0),
        Pos2::new(rail.right(), track_y + 8.0),
    );
    let t = ui.interact(
        hit,
        ui.make_persistent_id(("band", &key)),
        Sense::click_and_drag(),
    );
    if (t.clicked() || t.dragged())
        && let Some(pos) = t.interact_pointer_pos()
    {
        let frac = ((pos.x - rail.left()) / span).clamp(0.0, 1.0) as f64;
        let nv = min + frac * (max - min);
        if (*v - nv).abs() > 1e-9 {
            *v = nv;
            changed = true;
        }
    }
    if t.double_clicked() {
        *v = default;
        changed = true;
    }
    if changed {
        resp.mark_changed();
        history::note(label, format!("{label} {}", theme::number(*v, 0, true)));
    }

    let p = ui.painter();
    match stops {
        Some(table) => {
            const SLICES: f32 = 48.0;
            for k in 0..SLICES as usize {
                let t0 = k as f32 / SLICES;
                let t1 = (k as f32 + 1.0) / SLICES;
                let x0 = rail.left() + rail.width() * t0;
                let x1 = rail.left() + rail.width() * t1;
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
            // 与普通推子同一条通栏渐变（参考图混色器：彩轨与灰轨都是左暗右亮），
            // 混色器八行才不会在视觉上和上面的滑块分家。
            //
            // The same full-width ramp as the normal sliders (the reference mixer shows
            // both hue and neutral rails running dark -> light), so the eight bands do
            // not split visually from the sliders above.
            const SLICES: f32 = 48.0;
            let lo = theme::track();
            let hi = theme::text();
            for k in 0..SLICES as usize {
                let t0 = k as f32 / SLICES;
                let t1 = (k as f32 + 1.0) / SLICES;
                let x0 = rail.left() + rail.width() * t0;
                let x1 = rail.left() + rail.width() * t1;
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(x0, rail.top()),
                        Pos2::new(x1 + 0.6, rail.bottom()),
                    ),
                    0,
                    crate::widgets::mix(lo, hi, t0),
                );
            }
        }
    }
    let knob = Pos2::new(tx(*v), track_y);
    let r = if t.dragged() {
        4.0
    } else if t.hovered() {
        5.0
    } else {
        4.0
    };
    p.circle_filled(knob + Vec2::new(0.0, 1.0), r, Color32::from_black_alpha(70));
    p.circle(
        knob,
        r,
        if t.dragged() { accent() } else { text() },
        Stroke::new(1.0, Color32::from_black_alpha(90)),
    );
    if t.hovered() && !t.dragged() {
        p.circle_stroke(knob, r + 2.0, Stroke::new(1.0, border_strong()));
    }
    // 同 slider_row：数值框子 Ui 只推进 22px，紧凑行 28px 会被下一行压上来。
    // Same as slider_row: the value-box child UI only advances 22 px, so the next row
    // rode up over this 28 px compact row.
    if ui.cursor().top() < rect.bottom() {
        ui.advance_cursor_after_rect(rect);
    }
    changed
}

/// ACR 式下划线选项卡（拍板 2026-11-07 §6）：文字即按钮，选中态一条 2px
/// 亮色下划线，不画胶囊底。曲线模式/通道、混色器模式/属性都汇到这一条路，
/// 改一处全生效。返回被点中的序号；没点返回 None。
///
/// ACR-style underline tabs (finalised 2026-11-07 §6): the text is the button and
/// the selected one carries a 2 px bright underline, no pill background. Curve
/// modes/channels and mixer modes/properties all route through here. Returns the
/// clicked index, or None.
pub(super) fn acr_tabs(ui: &mut Ui, labels: &[&str], tips: &[&str], sel: usize) -> Option<usize> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (i, raw) in labels.iter().enumerate() {
            let label = crate::i18n::tr(raw);
            let on = i == sel;
            let galley = ui.painter().layout_no_wrap(
                label.to_owned(),
                font(SIZE_LABEL),
                if on { text() } else { muted() },
            );
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(galley.size().x, 24.0), Sense::click());
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            let resp = match tips.get(i).filter(|t| !t.is_empty()) {
                Some(tip) => resp.on_hover_text(crate::i18n::tr(tip)),
                None => resp,
            };
            let p = ui.painter();
            let col = if on || resp.hovered() {
                text()
            } else {
                muted()
            };
            p.galley(
                Pos2::new(rect.left(), rect.center().y - galley.size().y * 0.5),
                galley,
                col,
            );
            if on {
                p.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(rect.left(), rect.bottom() - 2.0),
                        Vec2::new(rect.width(), 2.0),
                    ),
                    0.0,
                    text(),
                );
            }
            if resp.clicked() {
                picked = Some(i);
            }
        }
    });
    picked
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
            // 复位点只在悬停这一行时出现，所以先让指针落在行内再找它。
            //
            // The dot exists only while the row is hovered, so park the pointer in the
            // row before looking for it.
            let hover = vec![egui::Event::PointerMoved(Pos2::new(200.0, 17.0))];
            let mut out = ctx.run_ui(input(hover), &mut draw);
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

#[cfg(test)]
mod combo_popup_material_tests {
    use super::*;
    use egui::{Context, PointerButton, Pos2, Vec2};

    /// R7 判据：ComboBox 弹层的背板必须是暗色主题材质（window_fill）——
    /// combo 弹层与 menu 弹层、tooltip 在同一页上必须是一套材质。
    ///
    /// R7 criterion: a ComboBox popup's backdrop must use the dark theme material
    /// (window_fill) -- combo popups, menu popups and tooltips share one material set.
    #[test]
    fn combo_popup_backdrop_is_dark_theme_material() {
        let ctx = Context::default();
        crate::theme::apply(&ctx, 1.0);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 120.0));
        let at = Pos2::new(60.0, 13.0); // combo 按钮中心（探针顶行）
        let press = egui::Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        };
        let release = egui::Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        };
        let mut frames: Vec<Vec<egui::Event>> = vec![
            vec![],
            vec![egui::Event::PointerMoved(at)],
            vec![press],
            vec![release],
        ];
        // 弹层有淡入动画：release 之后空跑 40 帧让它落定，再取背板——否则取到
        // 的是半透明中间态，会把「正在淡入」误判成材质。
        //
        // Popups fade in: after release, run 40 empty frames to let it settle before
        // sampling the backdrop -- otherwise a mid-fade frame reads as the material.
        for _ in 0..40 {
            frames.push(vec![]);
        }
        let mut backdrop: Option<Color32> = None;
        let mut dbg_all = String::new();
        for (fi, events) in frames.iter().enumerate() {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(rect),
                    max_texture_side: Some(8192),
                    events: events.clone(),
                    ..Default::default()
                },
                |panel| {
                    let mut probe =
                        panel.new_child(egui::UiBuilder::new().max_rect(rect).id_salt("cp-probe"));
                    egui::ComboBox::from_id_salt("probe-combo")
                        .selected_text("X")
                        .show_ui(&mut probe, |ui| {
                            combo_style(ui);
                            ui.label(crate::i18n::t!("自动"));
                            ui.label(crate::i18n::t!("黑白"));
                        });
                },
            );
            // 展开帧里找高度 >=40 的不透明实底矩形（= 弹层背板；combo 按钮只有
            // 26px 高，弹层装两行必然更高）。
            // 背板藏在嵌套 Shape::Vec 里（popup 的 Frame 整组绘制），要递归展开。
            fn collect(shapes: &[egui::Shape], dbg: &mut String, backdrop: &mut Option<Color32>) {
                for sh in shapes {
                    match sh {
                        egui::Shape::Vec(v) => collect(v, dbg, backdrop),
                        egui::Shape::Rect(r) => {
                            dbg.push_str(&format!(
                                "rect {:?} fill {:?}
",
                                r.rect, r.fill
                            ));
                            let a = r.rect.area();
                            if a > 800.0 && r.rect.height() >= 40.0 && r.fill.a() > 200 {
                                *backdrop = Some(r.fill);
                            }
                        }
                        _ => {}
                    }
                }
            }
            let mut dbg = String::new();
            let flat: Vec<egui::Shape> = out.shapes.iter().map(|c| c.shape.clone()).collect();
            collect(&flat, &mut dbg, &mut backdrop);
            dbg_all.push_str(&format!(
                "-- frame {fi} --
{dbg}"
            ));
            out.textures_delta.clear();
        }
        let fill = backdrop.unwrap_or_else(|| {
            panic!(
                "点击 ComboBox 后应有弹层背板。逐帧：
{dbg_all}"
            )
        });
        // 落定态必须逐位等于 window_fill（bg()）——半透明背板会让照片透过下拉
        // 菜单显形，是「违和」的源头。
        //
        // The settled backdrop must equal window_fill (bg()) bit-for-bit -- a
        // translucent backdrop lets the photo show through the dropdown.
        assert_eq!(
            fill,
            crate::theme::bg(),
            "ComboBox 弹层背板落定后应为 window_fill，实际 {fill:?}"
        );
    }
}
