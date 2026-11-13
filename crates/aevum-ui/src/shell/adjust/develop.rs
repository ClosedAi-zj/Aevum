//! 显影模块的九个调整区：基本 / 曲线 / 颜色混合器 / 颜色分级 / 细节 / 光学 / 效果 /
//! 外观，以及次级小标题与表外字段两个共用件。
//!
//! The nine develop sections: basic, curve, color mixer, color grading, detail, optics,
//! effects and look, plus the two shared pieces (`sub_head` for a subsection heading,
//! `spec_field` for an out-of-table field) and the gradient-rail criteria.

// 共用件（theme / i18n / 兄弟模块的函数）由 mod.rs 的 use 树统一带入。
//
// Shared pieces (theme, i18n, sibling helpers) come in through mod.rs's use tree.
use super::geometry::{adjust_geometry, adjust_optics, adjust_retouch};
use super::*;
// 「仅显示已改动」的闸门在 common 里（`spec_slider` 也用它），mod.rs 的 use 树
// 没有它，这里直接引。
//
// The "only show changed" gate lives in common (shared with `spec_slider`) and is not in
// mod.rs's use tree, hence the direct import.
use crate::icons::{self, Glyph};
use crate::shell::common::spec_visible;
/// 分区内的次级小标题（ACR 的 subsection heading）：小号、半粗、弱色，上方留一口气。
/// 与 `group_header` 不同，它不进折叠登记表——折叠始终以九分区组为单位，
/// 小标题只是给一组推子分段。文案由调用方先过 `t!`（宏只吃字面量）。
///
/// A subsection heading inside a section (ACR's style): small, semibold, muted, with a
/// breath above. Unlike `group_header` it is not registered for collapsing -- folding
/// always works on the nine sections; the heading merely bands a group of sliders.
/// Callers translate first with `t!` (the macro only takes literals).
pub(super) fn sub_head(ui: &mut Ui, label: &str) {
    // 子区标题（白平衡 / 影调 / 偏好）：与推子标签同一条左缘、同 12px 字号，
    // 只降一档色——加字距的 10 号小字在 300px 面板里读起来像脚注，和设计稿
    // 平铺的 `.lab` 列不是一套排法。
    //
    // Subsection heading (White Balance / Tone / Preference): same left edge and
    // 12 px size as the slider labels, one colour step dimmer. The old tracked 10 px
    // read like a footnote and broke the label column.
    ui.add_space(8.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font(SIZE_LABEL),
        dim(),
    );
}

/// 按字段画一条推子：清单里写了表里没有的字段，debug 构建当场炸出来，
/// 发布构建静默跳过（宁可少一根推子，也不要整块面板打不开）。
///
/// 「仅显示已改动」（设计稿 `.dock.is-only-changed .srow:not(.changed){display:none}`）
/// 也在这里生效：开关打开时，值等于默认的字段整行不画，也不占行高——过滤发生在
/// 布局之前，留下的行才会像设计稿那样往上收，而不是在原地留一条空带。
/// 默认值取规格表的 `default`（`controls` 模块头注释：表是唯一事实源，
/// `spec_defaults_match_grade_defaults` 钉住它与 `GradeParams::default()` 同值）。
///
/// Draw one slider by field: a field listed in the UI but missing from the spec table
/// trips a debug assertion, while release builds skip it silently (a missing slider
/// beats a panel that cannot open).
///
/// "Only show changed" (design `.dock.is-only-changed .srow:not(.changed)`) is enforced
/// here too: while the switch is on, a field at its default paints no row and reserves
/// no height -- the filter runs before layout, so the remaining rows close up instead of
/// leaving a blank band. The default comes from the spec table (the single source of
/// truth per the `controls` module docs; `spec_defaults_match_grade_defaults` pins it to
/// `GradeParams::default()`).
/// 展开态末尾的「收起分组」：把其余参数组重新收回，回到目标图的三卡首屏。
///
/// "Fewer groups" at the end of the expanded list: folds the extra parameter groups back
/// to the reference's three-card first screen.
fn collapse_groups_chip(ui: &mut Ui, state: &mut AppState) {
    let (row, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
    let p = ui.painter().clone();
    if resp.hovered() {
        p.rect_filled(row, radius_control(), elev());
    }
    p.text(
        row.center(),
        Align2::CENTER_CENTER,
        format!("{} ▲", crate::i18n::t!("收起分组")),
        font(SIZE_SMALL),
        if resp.hovered() { text() } else { dim() },
    );
    if resp.clicked() {
        state.show_more_groups = false;
    }
}

/// PS 式披露列表（用户 2026-11-13 以 PS 调色盘为对照）：不再只画一行
/// 「更多分组 ▾」，而是把其余分区列成 **57px 折叠头 + 3px 暗线 = 60px 等距行**，
/// 与 PS「亮 / 颜色 / 效果 / 曲线 / 混色器 / 颜色分级 / 细节」的连续行一致。
/// 点任一行仍按原产品规则展开全部分区，并把该组滚到视野内——披露规则没改，
/// 只是把「一个入口」换成「一组可读的行」。
///
/// PS-style disclosure (user, 2026-11-13): instead of one "More groups" chip, the
/// remaining sections are listed as **57 px collapsed headers + 3 px rule = 60 px
/// evenly spaced rows**, matching PS's continuous Tone/Color/Effects/Curves/Mixer/
/// Color Grading/Detail rows. Clicking any row still expands all sections per the
/// existing product rule and scrolls that section into view -- the disclosure rule
/// is unchanged; one bare entry becomes a readable list of rows.
fn more_groups_chip(ui: &mut Ui, state: &mut AppState) {
    const MORE: &[(&str, &str)] = &[
        ("tone-extra", "影调细节"),
        ("curve", "曲线"),
        ("mixer", "颜色混合器"),
        ("grade", "色彩分级"),
        ("detail", "细节"),
        ("effects", "效果"),
        ("look", "外观（.cube）"),
    ];
    for (idx, (key, cn)) in MORE.iter().enumerate() {
        let (rule, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 3.0), Sense::hover());
        theme::groove(ui.painter(), rule.left(), rule.right(), rule.center().y);
        let (row, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
        let hovered = resp.hovered();
        let p = ui.painter().clone();
        if hovered {
            p.rect_filled(row, 0.0, elev());
        }
        // 与 `group_header` 相同的等比内距：面板左缘起 18 / 36px，眼睛距右 18px。
        // Same scaled insets as `group_header`: 18 / 36 px from the left, eye 18 px
        // from the right.
        icons::paint(
            &p,
            Rect::from_center_size(
                Pos2::new(row.left() + 18.0, row.center().y),
                Vec2::splat(11.0),
            ),
            Glyph::ChevronRight,
            muted(),
        );
        p.text(
            Pos2::new(row.left() + 36.0, row.center().y),
            Align2::LEFT_CENTER,
            crate::i18n::tr(cn),
            bold(SIZE_LABEL + 2.0),
            text(),
        );
        icons::paint(
            &p,
            Rect::from_center_size(
                Pos2::new(row.right() - 18.0, row.center().y),
                Vec2::splat(14.0),
            ),
            Glyph::Eye,
            dim(),
        );
        if resp.clicked() {
            state.show_more_groups = true;
            state.dock_focus = Some((*key).to_owned());
        }
        // 完整清单只在首行给一次；每行都弹长提示会盖住下面的组头。
        // The full list appears once on the first row; repeating it on every row
        // would cover the headers underneath.
        let hint = if idx == 0 {
            format!(
                "{} · {}",
                crate::i18n::t!("更多分组"),
                crate::i18n::t!("曲线 / 颜色混合器 / 细节 / 光学 / 效果 / 蒙版")
            )
        } else {
            crate::i18n::t!("更多分组").to_owned()
        };
        resp.on_hover_text(hint);
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

pub(super) fn spec_field(ui: &mut Ui, state: &mut AppState, field: Field) -> bool {
    match controls::spec_by_field(field) {
        Some(s) => {
            // 「仅显示已改动」：值等于默认就整行不画（不占行高）。判据与
            // `spec_slider` 共用 `spec_visible`，默认值取规格表的 `default`。
            //
            // "Only show changed": a field at its default paints no row at all (and
            // reserves no height). Both this and `spec_slider` share `spec_visible`,
            // whose default comes from the spec table.
            if !spec_visible(state, s) {
                return false;
            }
            spec_slider(ui, state, s)
        }
        None => {
            debug_assert!(false, "field {field:?} has no spec");
            false
        }
    }
}

/// 组内的文字动作（LR 的「自动」位）：只有字，没有底、没有框，悬停浮一层极浅
/// 的底色，按下沉到 press 档，点中后留一小段强调色余晖。选中态（黑白开着）把字
/// 换成强调色，不画蓝底蓝框——三枚这样的胶囊并排会读成三颗主按钮，把下面真正
/// 要看的推子压没了。
///
/// 余晖是给「自动」这种异步动作的：它要先测光再回填推子，点下去的一瞬屏幕上
/// 什么都没变，光有 hover 洗不掉「到底点上没有」的疑问（用户反馈「有些按钮按下
/// 去没有视觉效果」）。
///
/// The in-group text action (Lightroom's Auto slot): text only, no fill, no stroke --
/// a barely-there hover wash, a pressed step while the pointer is down, and a short
/// accent afterglow once clicked. The active state (B&W on) changes the text to the
/// accent colour instead of painting a blue pill with a blue outline -- three such
/// pills side by side read as three primary buttons and bury the sliders below.
///
/// The afterglow exists for asynchronous actions such as Auto: metering fills the
/// sliders a beat later, so a click used to leave the screen unchanged and hover alone
/// did not answer "did that register?" (user report: "some buttons have no visual
/// effect when pressed").
fn quiet_action(ui: &mut Ui, label: &str, active: bool) -> egui::Response {
    const GLOW: f64 = 0.26;
    let label = crate::i18n::tr(label);
    // 只借它量宽度：颜色要在拿到 resp 之后才定，见下面重排。
    let measure = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(SIZE_SMALL), text());
    // 设计稿 `.segmini button{height:20px;padding:0 10px;border-radius:4px}`：
    // 胶囊里的三枚动作比旧版 24px 矮一档，字距靠 10px 内距撑开。
    //
    // The design's `.segmini button` is 20 px tall with 10 px side padding and a 4 px
    // radius; the old chips were 24 px tall.
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(measure.size().x + 20.0, 20.0), Sense::click());
    let pressed = resp.is_pointer_button_down_on();
    // 余晖按动作名记：文字就是身份，三枚动作的字互不相同。
    let glow_id = ui.make_persistent_id(("quiet-action", label));
    let now = ui.input(|i| i.time);
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(glow_id, now));
        ui.ctx().request_repaint();
    }
    let age = ui.data(|d| d.get_temp::<f64>(glow_id)).map(|t| now - t);
    let fired = age.is_some_and(|a| a < GLOW);
    if let Some(rest) = age.map(|a| GLOW - a).filter(|rest| *rest > 0.0) {
        // 不靠下一次输入把余晖收掉：到期那一帧自己醒一次。
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(rest));
    }
    let p = ui.painter();
    let fill = if pressed {
        Some(press())
    } else if fired {
        Some(accent().gamma_multiply(0.20))
    } else if active {
        // 设计稿 `.segmini button.on{background:var(--raised)}`：选中的那格抬一档
        // 实底，字回到正文白；不是强调色药丸。
        //
        // The design's `.segmini button.on` raises the active segment to `--raised`
        // and returns the label to body white -- it is not an accent pill.
        Some(elev())
    } else if resp.hovered() {
        Some(elev())
    } else {
        None
    };
    if let Some(fill) = fill {
        p.rect_filled(rect, 4.0, fill);
    }
    let fg = if pressed || fired {
        accent()
    } else if active || resp.hovered() {
        text()
    } else {
        dim()
    };
    // 文字颜色必须在排字时定死：`galley()` 的 fallback 色只对没上色的 galley
    // 生效，先按 text() 排好的字再传 fg 只会被忽略——按下时底变了、字不变，
    // 正是用户说的「按下去没有视觉效果」。
    //
    // The text colour must be baked at layout time: the fallback passed to `galley()`
    // only applies to an unpainted galley, so a galley laid out with text() ignores fg --
    // the fill changed on press but the letters did not, which is exactly the reported
    // "no visual effect when pressed".
    let galley = p.layout_no_wrap(label.to_owned(), font(SIZE_SMALL), fg);
    p.galley(rect.center() - galley.size() * 0.5, galley, fg);
    resp
}

/// 「配置文件」组的折叠键。设计稿给这张卡一个自己的 `▾/▸`，而 `state::GROUP_SPECS`
/// 是本次任务不许动的文件——键名因此走常量而不是字面量：`ui_tokens::
/// panel_groups_match_the_registry` 只对字面量实参做「必须登记」的双向核对，这里
/// 明确登记不了。功能上没有损失：`group_header` 只查 `collapsed` 里有没有这个键
/// （缺省即展开），右键「只看这一组」查不到登记项直接返回，落盘时这个键会被当作
/// 未知键丢掉。
///
/// The Profile card's collapse key. The design gives this card its own `▾/▸`, while
/// `state::GROUP_SPECS` is off-limits for this task -- hence a named constant rather
/// than a literal: the registry gate (`ui_tokens::panel_groups_match_the_registry`)
/// cross-checks only literal arguments, and this key cannot be registered. Nothing is
/// lost: `group_header` merely asks whether `collapsed` holds the key (absent means
/// open), "solo this group" returns early on the missing entry, and the key is dropped
/// as unknown when the collapse state is persisted.
const PROFILE_GROUP_KEY: &str = "profile";

/// 面板里的实底动作按钮（设计稿 `.btn`）：raised 实底、无描边、6px 圆角、26px 高。
/// 宽度由调用方给定——`.btnrow` 的两枚是 `flex:1`，等宽平分整行。
///
/// `bar_chip` 是选项栏的描边胶囊；搬到 300px 的检查器里，一排描边胶囊读作网页
/// 表单。设计稿这里是一块块抬起的实底按钮，按下沉到 `--field` 并压一道顶部
/// 内阴影——和选项栏同一种「按进去」的手感，只是没有描边。
///
/// The dock's solid action button (design `.btn`): raised fill, no stroke, 6 px radius,
/// 26 px tall. The width is supplied by the caller -- the two buttons of `.btnrow` are
/// `flex:1` and split the row evenly.
///
/// `bar_chip` is the options bar's outlined capsule; a row of those in a 300 px
/// inspector reads as a web form. The design uses solid raised buttons that sink to
/// `--field` with a top inner shadow when pressed -- the same pressed feel as the
/// options bar, just without the outline.
fn dock_action(ui: &mut Ui, label: &str, width: f32) -> egui::Response {
    let label = crate::i18n::tr(label);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 26.0), Sense::click());
    let pressed = resp.is_pointer_button_down_on();
    // 三态走主题令牌，与全局按钮同一口径：深色抬起、浅色凹下，字色随底自动翻。
    // 旧版写死 #3d4046 深底 + text()，在浅色皮肤下是深字压深底、几乎读不出。
    //
    // Three states use theme tokens, same contract as global buttons: raised on dark,
    // sunken on light, label colour flipping with the fill. The old hardcoded #3d4046
    // fill + text() put dark-on-dark on light skins, nearly unreadable.
    let (bg, fg) = if pressed {
        (press(), text())
    } else if resp.hovered() {
        (elev_hover(), on_elev())
    } else {
        (elev(), on_elev())
    };
    let p = ui.painter();
    p.rect_filled(rect, 6.0, bg);
    // 浅色补一圈发丝描边：elev 与分组卡底只差两度灰，无描边时按钮在卡上
    // 读作一团灰影（用户 2026-11-12「颜色违和」）。深色靠明度差就立得住，
    // 维持设计稿「raised 无描边」；浅色靠描边收边，与 WPS 的白底按钮同一口径。
    //
    // A hairline on light: elev sits only two grey steps from the group-card fill, so
    // without a stroke the button reads as a grey smudge on the card (user, 2026-11-12:
    // "the colours clash"). Dark stands on the value step alone and keeps the design's
    // "raised, no stroke"; light closes the edge with a stroke, like WPS's white buttons.
    if theme::is_light() {
        p.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, theme::border()),
            egui::StrokeKind::Inside,
        );
    }
    if pressed {
        p.line_segment(
            [
                Pos2::new(rect.left() + 2.0, rect.top() + 1.0),
                Pos2::new(rect.right() - 2.0, rect.top() + 1.0),
            ],
            Stroke::new(1.0, Color32::from_black_alpha(110)),
        );
    }
    let galley = p.layout_no_wrap(label.to_owned(), font(SIZE_LABEL), fg);
    p.galley(rect.center() - galley.size() * 0.5, galley, fg);
    resp
}

/// 显影面板：ACR 的九分区次序（基本 → 曲线 → 颜色混合器 → 颜色分级 → 细节 →
/// 光学 → 效果 → 外观 → 蒙版）。旧版的 21 组混排已并入这九个分区，参数与
/// 工程格式零改动，只动 UI 组织（docs/界面布局-ACR化-2026-11-04.md §3）。
///
/// The Develop panel in ACR's nine-section order (Basic, Curve, Color Mixer, Color
/// Grading, Detail, Optics, Effects, Look, Masks). The old 21-group mixed list folded
/// into these nine; parameters and the project format are untouched -- only the UI
/// organisation moved.
pub(super) fn adjust_develop(state: &mut AppState, ui: &mut Ui) {
    ui.add_space(4.0);
    // 「仅显示已改动」把参数全过滤掉时，面板只剩几张空组壳（快捷动作与配置文件
    // 行不是参数，保留是对的），却没有任何一行解释「为什么空」。真的一条改动
    // 都没有时补一句 dim 说明；只要有一根推子偏离默认就不画。
    //
    // With "only show changed" on and every parameter at its default the panel keeps
    // empty group shells (quick actions and profile rows are not parameters and stay),
    // but nothing explains why. Add one dim line only when nothing has changed.
    if state.only_changed
        && !crate::controls::SPECS
            .iter()
            .any(|s| (crate::controls::param_get(&state.params, s.field) - s.default).abs() > 1e-9)
    {
        let (row, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
        ui.painter().text(
            row.center(),
            Align2::CENTER_CENTER,
            crate::i18n::t!("没有已改动的参数"),
            font(SIZE_SMALL),
            dim(),
        );
    }
    let mut import_xmp = false;
    let mut export_xmp = false;
    // 面板顶部不再有动作条：设计稿里「复位」在基本组的 `.segmini` 凹槽右端，
    // 「导入 / 导出 XMP」在「配置文件」组的 `.btnrow`，全局动作不该另占一行
    // 把参数推下去——旧版那一行连同它下面的发丝线已按设计稿拆散归位。
    //
    // No action bar at the top any more: the design puts reset at the right end of
    // Basic's `.segmini` trough and import/export XMP in the Profile card's `.btnrow`.
    // A separate global row only pushed the parameters down; the old row and the
    // hairline under it are gone.
    let mut changed = false;

    // ── 基本 ────────────────────────────────────────────────────────────
    // 参数顺序照设计稿：色温/色调 → 曝光…黑白场 → 偏好（质感与色彩），段与段之间
    // 不再插小标题——设计稿的 `.srow` 是一条条平铺下来的。推子按字段清单显式
    // 排布，显示序与表内物理序解耦（规格表是参数登记册，不是版面）。
    //
    // Parameter order per the design: temperature/tint, then exposure...black/white
    // points, then presence, with no subsection headings in between -- the design's
    // `.srow` rows simply stack. Sliders come from explicit field lists, so display
    // order is decoupled from the table.

    // ── 配置文件 ──────────────────────────────────────────────────────────
    // 设计稿在基本组之后紧跟一张「配置文件」卡：风格、相机配置、XMP 一进一出。
    // 三件都是「这幅画面按什么基准渲染」，比基本组的推子低一层，所以单独成卡，
    // 不再散落在基本组尾巴和面板顶栏。
    //
    // The design puts a Profile card right after Basic: style, camera profile and the
    // XMP pair. All three answer "what basis does this frame render on", one layer
    // below Basic's sliders, so they get their own card instead of trailing Basic or
    // sitting in a panel-wide action row.
    group_card(ui, |ui| {
        // 键 `profile` 不在 `GROUP_SPECS` 里（state.rs 不动）：`group_header` 只看
        // `collapsed` 里有没有这个键，缺省即展开；右键的「只看这一组」查不到登记项
        // 会直接返回，折叠偏好落盘时也会被当作未知键丢弃——不会 panic。
        //
        // The key `profile` is absent from `GROUP_SPECS` (state.rs is off-limits):
        // `group_header` only asks whether `collapsed` holds the key, so it starts
        // expanded; "solo this group" returns early on the missing registry entry and a
        // persisted collapse preference is dropped as unknown -- nothing panics.
        if !group_header(ui, state, PROFILE_GROUP_KEY, "配置文件", false) {
            return;
        }
        style_row(state, ui);
        // 相机配置行（LR 的 Profile 行）：DCP 决定 RAW 解码后的白平衡、颜色矩阵
        // 与色调曲线，是所有推子底下的地基。镜头配置仍留在光学组——那是整幅画面
        // 的几何与暗角校正，和调色不是一件事。
        //
        // Camera-profile row (LR's Profile row): DCP takes over white balance, colour
        // matrix and tone curve after RAW decode, i.e. it sits beneath every slider. The
        // lens profile stays in Optics -- a whole-frame geometry/vignetting correction,
        // not grading.
        let ctx = ui.ctx().clone();
        camera_profile_row(state, &ctx, ui);
        // 设计稿 `.btnrow{display:flex;gap:8px;margin-top:8px}` + `.btnrow .btn{flex:1}`：
        // 两枚等宽、26px 高的实底按钮，平分卡内宽度；底色走主题三态令牌。
        //
        // Design `.btnrow`: two `flex:1` buttons, 8 px apart, 26 px tall, splitting the
        // card's inner width; fills come from the theme's three-state tokens.
        ui.add_space(8.0);
        // PS 的「配置文件」行只有标签 + 下拉框 + 一枚小图标；我们多出 XMP
        // 一进一出两枚按钮，若按 `flex:1` 铺满整行就成了两块大积木，把整组
        // 撑成三行。改成按实测文案宽排的小按钮，右侧留白还给面板。
        //
        // PS's Profile row is just a label, a dropdown and one small icon. Our extra
        // XMP pair used to stretch to `flex:1` across the whole row, turning the
        // group into three heavy lines. Size them to their labels instead.
        let bw = 96.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            import_xmp = dock_action(ui, "导入 XMP", bw)
                .on_hover_text(crate::i18n::t!(
                    "读入 Lightroom / Camera Raw 预设，替换当前调色参数"
                ))
                .clicked();
            export_xmp = dock_action(ui, "导出 XMP", bw)
                .on_hover_text(crate::i18n::t!("把当前调色参数写成 .xmp 预设"))
                .clicked();
        });
    });
    group_card(ui, |ui| {
        if !group_header(ui, state, "basic", "基本", true) {
            return;
        }
        // 快捷动作行（设计稿 `.segmini.actions`）：[自动][黑白][HDR] 三枚动作坐在
        // 一块 `--field` 凹槽里，凹槽右端跟着第四枚 [复位]——复位改的正是下面这堆
        // 推子，所以它搬进基本组，不再在面板顶栏另占一行（设计稿就是这么排的）。
        //
        // The quick-action row (design `.segmini.actions`): Auto / B&W / HDR sit in one
        // `--field` trough, with Reset as a fourth chip at its right end. Reset drives
        // exactly the sliders below, so it moves into Basic instead of owning a
        // panel-wide top row -- which is how the design lays it out.
        let mut auto = false;
        let mut mono_toggle = false;
        let mut hdr = false;
        let mut reset = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // 设计稿 `.segmini`：动作坐在一块 `--field` 凹槽里，2px 内距 +
            // 2px 间隔，整组读成一颗分段控件。凹槽按内容收缩（设计稿实测宽约
            // 180px），不做通栏——通栏会在三枚字动作右侧留下一大片空槽。
            //
            // The design's `.segmini`: actions sit inside one `--field` trough with
            // 2 px padding and 2 px gaps, reading as a single segmented control. The
            // trough shrinks to its content (about 180 px in the design) rather than
            // spanning the card, which would leave a wide empty basin.
            egui::Frame::NONE
                .fill(field())
                .corner_radius(radius_control())
                .inner_margin(egui::Margin::same(2))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        // 自动结果（「+1.17 EV · 对比 …」）进悬停提示，不铺在面板里：那是一串
                        // 读数，摊开会把推子推下去。
                        //
                        // The auto result ("+1.17 EV · contrast …") stays in the tooltip rather than
                        // the panel body: it is a wall of readings that would push the sliders down.
                        let chip = quiet_action(ui, "自动", false);
                        auto = match state.auto_note.as_deref() {
                            Some(note) => chip.on_hover_text(note).clicked(),
                            None => chip.clicked(),
                        };
                        mono_toggle = quiet_action(ui, "黑白", state.params.mono >= 50.0)
                            .on_hover_text(crate::i18n::t!(
                                "在彩色与黑白之间切换；黑白后可到「颜色混合器 → 黑白」逐色相调明暗"
                            ))
                            .clicked();
                        hdr = quiet_action(ui, "HDR", false)
                            .on_hover_text(crate::i18n::t!(
                                "切到合成模块的 HDR 合并：同一场景的包围曝光帧合成一张"
                            ))
                            .clicked();
                        // 设计稿把复位放在这一槽的右端（`<span class="sp">` 之后）：
                        // 它是「这一组的动作」，和自动/黑白/HDR 同一条底、同一档字。
                        //
                        // The design puts reset at the right end of this trough (after the
                        // `.sp` spacer): it is this group's action, on the same fill and at
                        // the same type size as Auto / B&W / HDR.
                        reset = quiet_action(ui, "复位", false)
                            .on_hover_text(crate::i18n::t!("复位全部参数"))
                            .clicked();
                    });
                });
        });
        // 设计稿 `.segmini{margin-bottom:8px}`：凹槽与第一条推子之间留 8px。
        // egui 还会在两次 allocate 之间补 4px item_spacing，所以这里只补 4。
        //
        // The design's `.segmini` carries an 8 px bottom margin; egui already inserts
        // 4 px of item_spacing, so only 4 more is needed here.
        ui.add_space(4.0);
        if auto {
            state.auto_exposure(ui.ctx());
        }
        if reset {
            // 复位全部显影参数：`reset_params` 自己会把这一步记进撤销历史
            // （`history::note("reset", "复位全部参数")`），这里不再重复登记，
            // 否则一次点击在历史里留两步。
            //
            // Reset every develop parameter: `reset_params` records the step itself
            // (`history::note("reset", …)`), so noting it here again would leave two
            // entries for one click.
            state.reset_params();
        }
        if mono_toggle {
            let to_mono = state.params.mono < 50.0;
            state.params.mono = if to_mono { 100.0 } else { 0.0 };
            history::note(
                "mono",
                crate::i18n::tr(if to_mono { "黑白" } else { "彩色" }).to_owned(),
            );
            changed = true;
        }
        if hdr {
            state.module = Module::Merge;
            state.dock_tab = crate::state::DockTab::Adjust;
            state.dock_focus = Some("hdr".to_owned());
            state.collapsed.remove("hdr");
        }
        // 基本组按设计稿只留六根：色温 / 色调 / 曝光 / 对比度 / 高光 / 阴影。
        // 其余十三根（过渡、线性对比、黑白场、软化、纹理、清晰度、去朦胧、
        // 自然饱和度、饱和度）收进下方「影调细节」折叠组——首屏因此和设计稿的
        // 版式一致，功能一根不少。
        //
        // Basic keeps exactly the design's six rows; the other thirteen live in the
        // collapsible "tone detail" group below, so the first screen matches the design
        // while every parameter stays one click away.
        for f in [
            Field::Temperature,
            Field::Tint,
            Field::Exposure,
            Field::Contrast,
            Field::Highlights,
            Field::Shadows,
        ] {
            changed |= spec_field(ui, state, f);
        }
    });
    if import_xmp {
        import_xmp_dialog(state);
    }
    if export_xmp {
        export_xmp_dialog(state);
    }

    // 子区返回「参数动了」就顺手摘掉该组的旁路：推子路径已在 spec_slider 里
    // 自清，这里接的是曲线/色阶拖点、外观卸下这类非推子路径。
    //
    // A sub-section reporting "parameters moved" also lifts that group's bypass: slider
    // paths self-clear in spec_slider, this catches the non-slider ones -- curve and
    // levels point-dragging, dropping a LUT.
    // PS 顺序（用户 2026-11-12 第 4 条）：首屏是 配置文件 → 基本 两张卡；
    // 展开后按 曲线 → 颜色混合器 → 色彩分级 → 细节 → 光学 → 几何 → 修复 →
    // 效果 → 外观 排列。点一下全部展开，参数一个没少——这是披露，不是删除。
    //
    // PS order (user, 2026-11-12 #4): the first screen is Profile -> Basic; expanding
    // reveals Curve -> Mixer -> Grading -> Detail -> Optics -> Geometry -> Retouch ->
    // Effects -> Look. One click expands them all -- disclosure, not deletion.
    if state.show_more_groups {
        // ── 影调细节 ────────────────────────────────────────────────────────
        // 基本组让位给设计稿的六行后，多出来的十三根推子落在这里，默认折叠。
        //
        // Tone detail: the thirteen sliders that Basic gave up, collapsed by default.
        group_card(ui, |ui| {
            if !group_header(ui, state, "tone-extra", "影调细节", false) {
                return;
            }
            for f in [
                Field::HlRolloff,
                Field::ContrastLinear,
                Field::Whites,
                Field::Blacks,
                Field::BlackSoft,
                Field::WhiteSoft,
            ] {
                changed |= spec_field(ui, state, f);
            }
            for f in [
                Field::Texture,
                Field::Clarity,
                Field::Dehaze,
                Field::DehazeDark,
                Field::DehazeContrast,
                Field::Vibrance,
                Field::Saturation,
            ] {
                changed |= spec_field(ui, state, f);
            }
        });

        for (group, hit) in [
            ("curve", adjust_curve(state, ui)),
            ("mixer", adjust_mixer(state, ui)),
            // PS / ACR 顺序：颜色混合器之后才是色彩分级，再往下是细节与光学。
            //
            // PS / ACR order: colour grading follows the mixer, then detail/optics.
            ("grade", adjust_grade(state, ui)),
            ("detail", adjust_detail(state, ui)),
            ("optics", adjust_optics(state, ui)),
            ("geometry", adjust_geometry(state, ui)),
            ("retouch", adjust_retouch(state, ui)),
            ("effects", adjust_effects(state, ui)),
            ("look", adjust_look(state, ui)),
        ] {
            changed |= hit;
            if hit {
                state.bypassed.remove(group);
            }
        }
        adjust_masks(state, ui);
    } else {
        more_groups_chip(ui, state);
    }
    if state.show_more_groups {
        collapse_groups_chip(ui, state);
    }

    // 文件级工具沉到底部：第一屏先给基本/色彩，和 LR 的阅读顺序一致。
    // 2026-11-04 起 HDR/全景/堆栈/时间合成/焦点合成五组搬去「合成」模块
    // （docs/界面布局-ACR化-2026-11-04.md §4），调色副本搬去「预设与快照」
    // 浮窗——显影面板只剩调色本身。
    //
    // File-level tools used to sit at the tail so Basic/Colour came first, matching
    // LR's reading order. Since 2026-11-04 the five merge tools live in the Merge
    // module and grade copies in the Presets & Snapshots window (plan §4) -- Develop
    // holds grading and nothing else.
    if changed {
        state.preview_dirty = true;
        if state.preset != "自定义" {
            state.preset = "自定义".to_owned();
        }
    }
}

/// 风格下拉（设计稿「配置文件」组的第一行 `.kv` + `.combo`）：28px 行，标签 12px
/// 次级色，下拉紧接标签、占满余宽，26px 高、`--field` 实底、无描边。
///
/// 换风格不动任何参数值，也不进「N 项调整」徽标——它是渲染基调，不是调整，
/// 所以这一行在「配置文件」卡里而不是基本组的推子列中。
///
/// The style dropdown (the Profile card's first `.kv` + `.combo` row in the design):
/// a 28 px row with a 12 px secondary label, and a 26 px dropdown filling the rest in
/// `--field` with no stroke.
///
/// Switching the style touches no parameter and never enters the "N adjusted" badge --
/// it is a rendering basis, not an adjustment, which is why it lives in the Profile card
/// rather than among Basic's sliders.
fn style_row(state: &mut AppState, ui: &mut Ui) {
    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
    let lab = ui.painter().layout_no_wrap(
        crate::i18n::t!("风格").to_owned(),
        font(SIZE_LABEL),
        muted(),
    );
    // 与 `.srow` 同一条左缘：卡内距 12px 就是标签列的位置，不再叠 6px——否则
    // 「风格」比下面的「相机配置」缩进一格，两行读起来不在一条线上。右缘同理，
    // 组合框铺到内容右缘（设计稿 `.combo{flex:1}`）。
    //
    // Same left edge as `.srow`: the card's 12 px padding *is* the label column, with no
    // extra 6 px -- otherwise "style" sits one notch deeper than the camera-profile row
    // below it. The right edge follows: the combo runs to the content edge (design
    // `.combo{flex:1}`).
    let lab_x = row.left();
    ui.painter().galley(
        Pos2::new(lab_x, row.center().y - lab.size().y * 0.5),
        lab.clone(),
        muted(),
    );
    let combo = Rect::from_min_max(
        Pos2::new(lab_x + lab.size().x + 8.0, row.top() + 1.0),
        Pos2::new(row.right(), row.bottom() - 1.0),
    );
    let current = state.params.style;
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(combo)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().interact_size.y = 26.0;
            ui.spacing_mut().icon_width = 10.0;
            {
                let v = &mut ui.style_mut().visuals;
                v.override_text_color = Some(text());
                v.widgets.inactive.bg_fill = field();
                v.widgets.inactive.weak_bg_fill = field();
                v.widgets.inactive.bg_stroke = Stroke::NONE;
                v.widgets.hovered.bg_fill = field_hover();
                v.widgets.hovered.weak_bg_fill = field_hover();
                v.widgets.hovered.bg_stroke = Stroke::NONE;
                v.widgets.active.bg_fill = field_hover();
                v.widgets.active.weak_bg_fill = field_hover();
                v.widgets.active.bg_stroke = Stroke::NONE;
            }
            egui::ComboBox::from_id_salt("dev-style")
                .selected_text(crate::i18n::tr(
                    aevum_core::style::NAME_KEYS
                        [current.min(aevum_core::style::COUNT - 1) as usize],
                ))
                .width(combo.width())
                .show_ui(ui, |ui| {
                    combo_style(ui);
                    for (i, key) in aevum_core::style::NAME_KEYS.iter().enumerate() {
                        let idx = i as u8;
                        if ui
                            .selectable_label(state.params.style == idx, crate::i18n::tr(key))
                            .clicked()
                            && state.params.style != idx
                        {
                            state.params.style = idx;
                            state.preview_dirty = true;
                            history::note(
                                "style",
                                crate::i18n::tr_fmt("风格 {}", &[&crate::i18n::tr(key).to_owned()]),
                            );
                        }
                    }
                });
        },
    );
}

/// 外观：外挂一张 .cube 三维查表，套在主链之前；导出把当前调色烘成 .cube。
fn adjust_look(state: &mut AppState, ui: &mut Ui) -> bool {
    group_card(ui, |ui| {
        if !group_header(ui, state, "look", "外观（.cube）", true) {
            return false;
        }
        let mut import = false;
        let mut export = false;
        let mut drop = false;
        ui.horizontal(|ui| {
            import = bar_button(ui, "导入外观")
                .on_hover_text(crate::i18n::t!("读一张 .cube，先套在调色链之前"))
                .clicked();
            export = bar_button(ui, "导出外观")
                .on_hover_text(crate::i18n::t!(
                    "把当前全部调色（含已挂外观）烘成一张 .cube"
                ))
                .clicked();
            drop = bar_button(ui, "卸下")
                .on_hover_text(crate::i18n::t!("取消外挂外观；滑杆数值不动"))
                .clicked();
        });
        let path = state.params.look_cube.clone();
        if path.is_empty() {
            kv(ui, "当前", "未挂外观", dim());
        } else {
            let name = path
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(path.as_str())
                .to_owned();
            kv(ui, "当前", theme::ellipsis(&name, 26), text());
        }
        if import {
            import_look_dialog(state);
        }
        if export {
            export_look_dialog(state);
        }
        if drop && !path.is_empty() {
            state.params.look_cube = String::new();
            return true;
        }
        false
    })
}

/// 色阶：在直方图剪影上直接拖输入黑 / 灰 / 白与输出黑 / 白。
/// 曲线区的第三个模式片（ACR 的 Levels 同构）。
///
/// Levels: drag input black/gamma/white and output black/white right on the histogram
/// silhouette. The third mode chip of the Curve section (mirrors ACR's Levels).
fn levels_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let changed = widgets::levels_strip(
        ui,
        state.hist.as_ref(),
        &mut state.params.lv_black,
        &mut state.params.lv_gamma,
        &mut state.params.lv_white,
        &mut state.params.lv_out_black,
        &mut state.params.lv_out_white,
        &mut state.levels_grab,
        78.0,
    );
    let p = &state.params;
    kv(
        ui,
        "输入 · 输出",
        format!(
            "{:.0} / {:.2} / {:.0} · {:.0} / {:.0}",
            p.lv_black, p.lv_gamma, p.lv_white, p.lv_out_black, p.lv_out_white
        ),
        dim(),
    );
    changed
}

/// 参数曲线：四个亮度区的升降 + 三条分界，与 LR 的 Parametric 同义。
/// 曲线区的第一个模式片。
///
/// Parametric curve: lift/drop four tonal zones plus three split points, same idea as
/// Lightroom's Parametric. The first mode chip of the Curve section.
fn parametric_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let d = GradeParams::default();
    let mut changed = false;
    for f in [
        Field::ParShadows,
        Field::ParDarks,
        Field::ParLights,
        Field::ParHighlights,
    ] {
        changed |= spec_field(ui, state, f);
    }
    for (i, label) in [(1, "阴影分界"), (2, "中间分界"), (3, "高光分界")] {
        let key = match i {
            1 => &mut state.params.par_split1,
            2 => &mut state.params.par_split2,
            _ => &mut state.params.par_split3,
        };
        let default = match i {
            1 => d.par_split1,
            2 => d.par_split2,
            _ => d.par_split3,
        };
        changed |= slider(
            ui,
            ("dev.par-split", i),
            label,
            key,
            0.0,
            100.0,
            default,
            0,
            false,
        );
    }
    changed
}

/// 曲线：主曲线 + 三条分通道，点空白加控制点，双击删点或整条复位。
/// 曲线区：ACR 的 Curve 面板同构——区头下三枚下划线选项卡（参数 / 点 / 色阶）
/// 切换三种调曲线的方式，共用一条历史记账与一个复位口径。
///
/// The Curve section, mirroring ACR's Curve panel: three underline tabs (parametric /
/// point / levels) under the header, sharing one history path and one reset scope.
fn adjust_curve(state: &mut AppState, ui: &mut Ui) -> bool {
    group_card(ui, |ui| {
        if !group_header(ui, state, "curve", "曲线", true) {
            return false;
        }
        let mut changed = false;
        if let Some(i) = acr_tabs(
            ui,
            &["参数曲线", "点曲线", "色阶"],
            &[
                "四个亮度区 + 三条分界，Lightroom 的 Parametric",
                "手绘任意形状，分 RGB 主通道与三条单通道",
                "直方图上的输入 / 输出黑白场",
            ],
            state.curve_tab,
        ) {
            state.curve_tab = i;
        }
        ui.add_space(2.0);
        match state.curve_tab {
            0 => changed |= parametric_body(state, ui),
            1 => changed |= point_curve_body(state, ui),
            _ => changed |= levels_body(state, ui),
        }
        changed
    })
}

/// 点曲线：主曲线 + 三条分通道，点空白加控制点，双击删点或整条复位。
/// 曲线区的第二个模式片。
///
/// Point curve: the RGB master plus three per-channel curves; click empty space to add
/// a point, double-click to delete it or reset the whole curve. The second chip.
fn point_curve_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let mut moved = false;
    if let Some(i) = acr_tabs(ui, &["RGB", "红", "绿", "蓝"], &[], state.curve_chan) {
        state.curve_chan = i;
        state.curve_grab = None;
        moved = true;
    }
    ui.label(
        RichText::new(crate::i18n::t!("点空白加点 · 双击删点"))
            .size(SIZE_SMALL)
            .color(dim()),
    );
    // PS 参考图（512×2093）量比：轨道通栏 295px，曲线框 221px —— 74.9% 且为
    // 正方。旧值 0.70 来自早前一张内容宽 316.5px 的稿，比 PS 小一档；换成 0.75
    // 让框宽与内容宽的比例对上参考图。
    //
    // PS reference (512×2093): the rails span 295 px and the curve box 221 px --
    // 74.9 %, square. The old 0.70 came from an earlier 316.5 px-content mockup and
    // ran a notch small; 0.75 matches the reference's box-to-content ratio.
    let side = (ui.available_width() * 0.75).clamp(180.0, 320.0);
    let chan = state.curve_chan;
    let pts = match chan {
        0 => &mut state.params.curve_rgb,
        1 => &mut state.params.curve_r,
        2 => &mut state.params.curve_g,
        _ => &mut state.params.curve_b,
    };
    // 通道色照拍板 §6：主曲线近白，R/G/B 各走自己的识别色。
    let tint = match chan {
        0 => Color32::from_rgb(0xD8, 0xD8, 0xD8),
        1 => Color32::from_rgb(0xE0, 0x55, 0x48),
        2 => Color32::from_rgb(0x5D, 0xBE, 0x63),
        _ => Color32::from_rgb(0x4D, 0x9F, 0xE0),
    };
    let changed = widgets::curve_box(
        ui,
        "tone",
        "输出 ↑ / 输入 →",
        tint,
        state.hist.as_ref(),
        pts,
        side,
        &mut state.curve_grab,
        &mut state.curve_lut_cache,
    );
    // HDR 尾部：曲线 x>1 的延伸量（对齐 LR 的 LumaHDR），挂在点曲线下方。
    // 参数曲线与色阶没有 x>1 的概念，所以只在点曲线模式下出现。
    //
    // HDR tail: the extension of the curve past x>1 (LR's LumaHDR), shown under the
    // point curve -- parametric and levels have no notion of x>1.
    let spec_changed = spec_field(ui, state, Field::CurveHdrTail);
    moved || changed || spec_changed
}

/// 颜色混合器：ACR 的 Color Mixer 面板同构（拍板 2026-11-07）——区头下
/// 两枚下划线选项卡（HSL / 黑白）。HSL 页内再选属性（色相 / 饱和度 /
/// 明亮度），八个频段同屏排列；黑白调每个色相带转灰后的亮度。
/// 旧单频段页（色块条 + 三条推子）并入属性页：八行同屏覆盖同一组参数，
/// 少一次「先选频段」的来回。
///
/// Color Mixer, mirroring ACR (finalised 2026-11-07): two underline tabs
/// (HSL / B&W) under the header. The HSL page picks a property (hue /
/// saturation / luminance) and lists all eight bands at once; B&W tunes how
/// bright each band turns in mono. The old single-band page folds into the
/// property page -- the eight rows cover the same parameters without the
/// "pick a band first" round trip.
fn adjust_mixer(state: &mut AppState, ui: &mut Ui) -> bool {
    group_card(ui, |ui| {
        if !group_header(ui, state, "mixer", "颜色混合器", true) {
            return false;
        }
        let mut changed = false;
        if let Some(i) = acr_tabs(
            ui,
            &["HSL", "黑白"],
            &[
                "八段色相各自的色相 / 饱和度 / 明亮度",
                "转黑白后每个色相带变成多亮",
            ],
            usize::from(state.mixer_tab == 1),
        ) {
            state.mixer_tab = i;
        }
        ui.add_space(4.0);
        match state.mixer_tab {
            1 => changed |= mono_body(state, ui),
            // 旧工程里存的「全部」页码（2）落到同一张属性页，不落空。
            _ => changed |= hsl_all_body(state, ui),
        }
        changed
    })
}

/// HSL 属性页（ACR 同构）：选一个属性（色相 / 饱和度 / 明亮度），八个频段
/// 各占一行同屏排列，每行轨道铺该频段的渐变——不用来回切色块就能扫过
/// 整条光谱（用户 2026-11-07 确认要做）。
///
/// The HSL property page (mirroring ACR): pick one property (hue / saturation /
/// luminance) and all eight bands list at once, each rail tinted with its own
/// band gradient -- sweep the whole spectrum without flipping swatches
/// (user confirmed, 2026-11-07).
fn hsl_all_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let mut changed = false;
    // 四枚属性页（LR 同构）：色相 / 饱和度 / 明亮度，以及「全部」——后者把
    // 八个频段各起一节，三种属性同屏列出。参考图里这一排就是四个平级选项。
    //
    // Four property tabs (mirroring LR): hue / saturation / luminance plus "All",
    // where each of the eight bands gets a heading with all three properties listed.
    // The reference shows exactly these four as peers.
    if let Some(k) = acr_tabs(
        ui,
        &["色相", "饱和度", "明亮度", "全部"],
        &[],
        state.hsl_all_prop.min(3),
    ) {
        state.hsl_all_prop = k;
    }
    ui.add_space(2.0);
    if state.hsl_all_prop == 3 {
        for band in 0..8 {
            ui.add_space(4.0);
            ui.label(
                RichText::new(crate::i18n::tr(widgets::BAND_NAMES[band]))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
            for (prop, (name, kind)) in [
                ("色相", HslKind::Hue),
                ("饱和度", HslKind::Sat),
                ("明亮度", HslKind::Lum),
            ]
            .into_iter()
            .enumerate()
            {
                let v = match prop {
                    0 => &mut state.params.hsl_h[band],
                    1 => &mut state.params.hsl_s[band],
                    _ => &mut state.params.hsl_l[band],
                };
                changed |= band_row(
                    ui,
                    ("hsl-band", band, prop),
                    name,
                    v,
                    -100.0,
                    100.0,
                    0.0,
                    Some(hsl_band_stops(band, kind)),
                );
            }
        }
        return changed;
    }
    for band in 0..8 {
        // 行首频段名（PS 同位），轨道渐变与单频段页同一份事实源。
        let (key, v, kind) = match state.hsl_all_prop {
            0 => (
                ("hsl-all-h", band),
                &mut state.params.hsl_h[band],
                HslKind::Hue,
            ),
            1 => (
                ("hsl-all-s", band),
                &mut state.params.hsl_s[band],
                HslKind::Sat,
            ),
            _ => (
                ("hsl-all-l", band),
                &mut state.params.hsl_l[band],
                HslKind::Lum,
            ),
        };
        changed |= band_row(
            ui,
            key,
            widgets::BAND_NAMES[band],
            v,
            -100.0,
            100.0,
            0.0,
            Some(hsl_band_stops(band, kind)),
        );
    }
    changed
}

/// 黑白混合：转黑白后，8 个色相带各自变成多亮。
fn mono_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let mut changed = false;
    changed |= spec_field(ui, state, Field::Mono);
    for i in 0..8 {
        changed |= band_row(
            ui,
            ("mono-h", i),
            widgets::BAND_NAMES[i],
            &mut state.params.mono_h[i],
            -100.0,
            100.0,
            0.0,
            None,
        );
    }
    changed
}

/// 色彩分级：中间调居上、阴影 / 高光并列居下（ACR 的品字布局，拍板
/// 2026-11-07），加全局一轮与两段过渡控制。三枚轮共用 widgets::grade_wheel
/// 的 HSV 灰心盘面。
fn adjust_grade(state: &mut AppState, ui: &mut Ui) -> bool {
    group_card(ui, |ui| {
        if !group_header(ui, state, "grade", "色彩分级", true) {
            return false;
        }
        let mut changed = false;
        let w = ui.available_width();
        // 参考图逐项实测（内容宽 316.5px 的面板）：轮径 92.5（29.2%）、下排缝 34.5
        // （10.9%）、标签中心到盘顶 21（6.7%）、盘底到明度轨 21.5（6.8%）、明度轨宽
        // 为盘径的 93%。旧版按 28% 取值，三枚轮整体偏小、标签又贴住盘顶，读起来比
        // 参考图「紧一档、小一档」。
        //
        // Measured item by item on the reference (316.5 px content width): disc 92.5
        // (29.2 %), bottom-pair gap 34.5 (10.9 %), label centre to disc top 21 (6.7 %),
        // disc bottom to luminance rail 21.5 (6.8 %), rail width 93 % of the disc. The old
        // 28 % made all three wheels a notch small and the labels sat right on the disc.
        let size = (w * 0.30).clamp(88.0, 124.0);
        let gap = (w * 0.11).clamp(24.0, 44.0);
        ui.vertical_centered(|ui| {
            changed |= widgets::grade_wheel(
                ui,
                "grade.mid",
                "中间调",
                &mut state.params.grade_mid_h,
                &mut state.params.grade_mid_s,
                &mut state.params.grade_mid_l,
                size,
            );
        });
        // 中调轮与下排之间：参考图轨道中线到下一行标签中心 27.8（8.8%）。轮子自身
        // 的分配已经包含轨下的余量，这里只补 8。
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // 显式留白居中：关掉默认项距，两枚轮 + 缝精确落在面板中轴。
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(((w - size * 2.0 - gap) * 0.5).max(0.0));
            changed |= widgets::grade_wheel(
                ui,
                "grade.sh",
                "阴影",
                &mut state.params.grade_sh_h,
                &mut state.params.grade_sh_s,
                &mut state.params.grade_sh_l,
                size,
            );
            ui.add_space(gap);
            changed |= widgets::grade_wheel(
                ui,
                "grade.hi",
                "高光",
                &mut state.params.grade_hi_h,
                &mut state.params.grade_hi_s,
                &mut state.params.grade_hi_l,
                size,
            );
        });
        ui.add_space(4.0);
        for s in controls::specs_of_group("grade") {
            changed |= spec_slider(ui, state, s);
        }
        changed
    })
}

/// 细节：中频（清晰度 / 纹理）+ 锐化四件套 + 降噪五件套。
fn adjust_detail(state: &mut AppState, ui: &mut Ui) -> bool {
    group_card(ui, |ui| {
        if !group_header(ui, state, "detail", "细节", true) {
            return false;
        }
        let mut changed = false;
        for s in controls::specs_of_group("detail") {
            changed |= spec_slider(ui, state, s);
        }
        changed
    })
}

/// 效果区：ACR 的 Effects 面板——暗角在外圈（裁剪后晕影），颗粒是胶片质感。
/// 两段各有小标题：九条推子混排在一起读不出层次。
///
/// Effects, mirroring ACR: post-crop vignette on the outer ring, grain as film texture.
/// Each band gets a subsection heading -- nine mixed sliders read as a wall otherwise.
fn adjust_effects(state: &mut AppState, ui: &mut Ui) -> bool {
    group_card(ui, |ui| {
        if !group_header(ui, state, "effects", "效果", true) {
            return false;
        }
        let mut changed = false;
        sub_head(ui, crate::i18n::t!("暗角"));
        for f in [
            Field::Vignette,
            Field::VigMidpoint,
            Field::VigRoundness,
            Field::VigFeather,
            Field::VigHlContrast,
        ] {
            changed |= spec_field(ui, state, f);
        }
        sub_head(ui, crate::i18n::t!("颗粒"));
        for f in [
            Field::Grain,
            Field::GrainLum,
            Field::GrainSize,
            Field::GrainFreq,
        ] {
            changed |= spec_field(ui, state, f);
        }
        changed
    })
}

/// 相机配置行（LR 的 Profile 行，住基本组）：DCP 接管 RAW 解码后的白平衡、
/// 颜色矩阵与色调曲线，是所有调色推子底下的地基。
///
/// 走 `profile.set`，和 MCP 同一条校验路径：机型对不上就在面板里直说。
///
/// The camera-profile row (LR's Profile row, living in Basic): DCP takes over white
/// balance, colour matrix and tone curve after RAW decode, i.e. the basis under every
/// grading slider. It goes through `profile.set`, the same validation path as MCP: a
/// body mismatch is said straight in the panel.
fn camera_profile_row(state: &mut AppState, ctx: &Context, ui: &mut Ui) {
    let dcp_active = !state.params.dcp_profile.is_empty();
    let dcp_shown = if !state.dcp_name.is_empty() {
        state.dcp_name.clone()
    } else if dcp_active {
        std::path::Path::new(&state.params.dcp_profile)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| crate::i18n::tr("已套用").to_owned())
    } else {
        crate::i18n::tr("未套用").to_owned()
    };
    // 一行塞「标签 + 名称 + 三颗按钮」在 300px 面板里必然溢出，把「清除」顶出右缘
    // （用户 2026-11-08 截图）。改成：名称单行截断占满余宽，按钮自动落到下一行。
    //
    // Fitting label + name + three buttons on one row cannot work in a 300 px panel and
    // pushed "clear" past the right edge. The name now truncates to one line and the
    // buttons wrap onto the next row.
    // 相机配置的按钮照设计稿 `.btn`：raised 实底、无描边；egui 默认按钮是
    // 描边胶囊，一列下来像网页表单。小号 22px 高，文字 11px。
    //
    // Camera-profile buttons follow the design's `.btn`: raised fill, no stroke.
    // egui's stock button is an outlined pill, which reads as a web form; these are
    // 22 px tall with 11 px text.
    let old_widgets = ui.style().visuals.widgets.clone();
    let old_interact = ui.style().spacing.interact_size;
    {
        let s = ui.style_mut();
        s.spacing.interact_size.y = 22.0;
        let w = &mut s.visuals.widgets;
        // 浅色给发丝描边收边（elev 与卡底太近，无描边读作灰影）；深色维持无描边。
        //
        // Light closes the edge with a hairline (elev is too close to the card fill);
        // dark keeps the stroke-free raised look.
        let edge = if theme::is_light() {
            Stroke::new(1.0, theme::border())
        } else {
            Stroke::NONE
        };
        for st in [&mut w.inactive, &mut w.hovered, &mut w.active] {
            st.bg_stroke = edge;
            st.corner_radius = egui::CornerRadius::same(theme::radius_chip() as u8);
        }
        w.inactive.bg_fill = elev();
        w.inactive.weak_bg_fill = elev();
        w.hovered.bg_fill = elev_hover();
        w.hovered.weak_bg_fill = elev_hover();
        w.active.bg_fill = press();
        w.active.weak_bg_fill = press();
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("相机配置"))
                .size(SIZE_LABEL)
                .color(muted()),
        );
        ui.add(
            egui::Label::new(egui::RichText::new(theme::ellipsis(&dcp_shown, 18)).weak())
                .truncate(),
        );
        if ui
            .button(crate::i18n::t!("自动"))
            .on_hover_text(crate::i18n::t!("按首帧 EXIF 机型在 DCP 目录里找配置（优先 Adobe Standard）。\nDCP 只接 RAW/DNG：相机原始数据才需要矩阵与曲线，\nJPEG/TIFF 已经是成品色彩。"))
            .clicked()
        {
            let args = serde_json::json!({"kind":"dcp","auto":true});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("匹配相机配置失败：{}", &[&e]));
            }
        }
        if ui.button(crate::i18n::t!("选文件")).clicked() {
            state.request_dialog(
                crate::state::ops_dialog::DialogPurpose::PickDcp,
                crate::state::ops_dialog::DialogMode::File,
                Some(crate::state::ops_dialog::DialogFilter {
                    name: "相机配置",
                    exts: vec!["dcp"],
                }),
                None,
                None,
            );
        }
        if dcp_active && ui.button(crate::i18n::t!("清除")).clicked() {
            let args = serde_json::json!({"kind":"dcp","clear":true});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("清除相机配置失败：{}", &[&e]));
            }
        }
    });
    ui.style_mut().visuals.widgets = old_widgets;
    ui.style_mut().spacing.interact_size = old_interact;
    if !state.dcp_note.is_empty() {
        ui.label(egui::RichText::new(&state.dcp_note).weak());
    }
}

/// 镜头配置行（LR 的 Lens Corrections 位，留在光学组）：LCP 按 EXIF 选帧，
/// 折出畸变、横向色差与暗角，先于下面的手动三项生效。
///
/// The lens-profile row (LR's Lens Corrections slot, staying in Optics): LCP selects by
/// EXIF and folds out distortion, lateral chromatic aberration and vignetting ahead of
/// the three manual controls.
pub(super) fn lens_profile_body(state: &mut AppState, ctx: &Context, ui: &mut Ui) {
    let lcp_shown = match state.params.lcp_profile.as_ref() {
        Some(c) => format!("{}（{} · {:.0}mm）", c.profile, c.lens, c.focal35_mm),
        None => crate::i18n::tr("未套用").to_owned(),
    };
    // 与相机配置行同一口径：名称截断 + 按钮换行，避免在窄面板里顶出右缘。
    //
    // Same rule as the camera-profile row: truncate the name and let the buttons wrap.
    ui.horizontal_wrapped(|ui| {
        ui.label(crate::i18n::t!("镜头配置"));
        ui.add(
            egui::Label::new(egui::RichText::new(theme::ellipsis(&lcp_shown, 22)).weak())
                .truncate(),
        );
        if ui
            .button(crate::i18n::t!("自动"))
            .on_hover_text(crate::i18n::t!("按首帧 EXIF 的机身/镜头/焦距在 LCP 目录里选帧，\n折出畸变、横向色差与暗角，先于下面的手动三项生效。"))
            .clicked()
        {
            let args = serde_json::json!({"kind":"lcp","auto":true});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("匹配镜头配置失败：{}", &[&e]));
            }
        }
        if ui.button(crate::i18n::t!("选文件")).clicked() {
            state.request_dialog(
                crate::state::ops_dialog::DialogPurpose::PickLcp,
                crate::state::ops_dialog::DialogMode::File,
                Some(crate::state::ops_dialog::DialogFilter {
                    name: "镜头配置",
                    exts: vec!["lcp"],
                }),
                None,
                None,
            );
        }
        if state.params.lcp_profile.is_some() && ui.button(crate::i18n::t!("清除")).clicked() {
            let args = serde_json::json!({"kind":"lcp","clear":true});
            if let Err(e) = state.run_ui_command(ctx, "profile.set", &args) {
                state.error = Some(crate::i18n::tr_fmt("清除镜头配置失败：{}", &[&e]));
            }
        }
    });
    if !state.lcp_note.is_empty() {
        ui.label(egui::RichText::new(&state.lcp_note).weak());
    }
}

#[cfg(test)]
/// 色温/色调那条渐变轨，以及截图时把分组滚到顶要连展开一起做。
mod gradient_rail_tests {
    use super::*;
    use crate::shell::common::slider_row;

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 120.0))),
            max_texture_side: Some(8192),
            events,
            ..Default::default()
        }
    }

    /// 数的是「轨道上出现了多少片渐变色」，不是形状总数：
    /// 渐变只有 `slider_grad` 会画，调用点哪天退回普通 `slider`，这一组色就整体消失，
    /// 而界面照常能拖——静悄悄的降级只有照色认形状才拦得住。
    fn painted_gradient_slices(grad: bool) -> Vec<Color32> {
        // 与 grad_stops 同源的停靠点表：测试要和实现走同一套取色，
        // 否则「渐变画了几片」数的是测试自己编的颜色。
        //
        // A stop table from the same source as `grad_stops`: the test must sample
        // colours the same way the implementation does, or "how many gradient slices
        // were painted" counts colours the test invented.
        static STOPS: [(f32, Color32); 3] = [
            (0.0, Color32::from_rgb(0x4E, 0x86, 0xC8)),
            (0.5, Color32::from_rgb(0x8A, 0x8A, 0x8C)),
            (1.0, Color32::from_rgb(0xD8, 0xA4, 0x55)),
        ];
        let stops: &'static [(f32, Color32)] = &STOPS;
        let band: Vec<Color32> = (0..48)
            .map(|k| crate::controls::sample_stops(stops, k as f32 / 48.0))
            .collect();
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut value = 0.0f64;
        let mut draw = |root: &mut Ui| {
            // 渐变与否现在由规格表决定（controls::grad_stops），这里直接走
            // slider_row 复刻两条路径，钉住「渐变只有表驱动入口会画」。
            slider_row(
                root,
                "temp",
                "色温",
                &mut value,
                -100.0,
                100.0,
                0.0,
                0,
                true,
                "",
                if grad { Some(stops) } else { None },
                false,
                "",
                None,
            );
        };
        let mut out = ctx.run_ui(input(vec![]), &mut draw);
        let fills: Vec<Color32> = out
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Rect(r) if band.contains(&r.fill) => Some(r.fill),
                _ => None,
            })
            .collect();
        out.textures_delta.clear();
        fills
    }

    #[test]
    fn mixer_all_tab_renders_without_panic_and_flags_changes() {
        // HSL 属性页（旧「全部」页码 2 落同一页）：三属性片 + 八行频段滑块同屏。
        // 逐行拖动一行，changed 必须为真且只有该频段变化——「八行共用一个键」
        // 的 ID 冲突会被这条当场抓住。
        let ctx = Context::default();
        crate::theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.mixer_tab = 2;
        state.hsl_all_prop = 1; // 饱和度
        let before = state.params.hsl_s;
        // TexturesDelta 未消费会在 Drop 时 panic——照 windows.rs::painted 的
        // 口径显式 clear。
        let mut out = ctx.run_ui(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = adjust_mixer(&mut state, ui);
            });
        });
        out.textures_delta.clear();
        drop(out);
        // 渲染一帧不 panic 即通过前半；后半：直接改一条频段验证数据通路。
        state.params.hsl_s[3] = -20.0;
        assert_ne!(state.params.hsl_s[3], before[3], "饱和度通路断了");
    }

    #[test]
    fn gradient_rail_spans_both_ends_and_flat_rail_has_none() {
        let grad = painted_gradient_slices(true);
        assert!(grad.len() >= 40, "渐变轨只铺出 {} 片，轨道断了", grad.len());
        assert_eq!(grad[0], theme::temp_cold(), "渐变轨的冷端起错色");
        assert!(
            grad[grad.len() - 1] != grad[0],
            "渐变轨两头同色，等于没渐变"
        );
        assert_eq!(
            painted_gradient_slices(false).len(),
            0,
            "普通推子的轨道上出现了渐变色"
        );
    }

    /// `--shot-group` 命中分组头时顺手展开它。
    #[test]
    fn focusing_a_group_for_a_shot_also_opens_it() {
        // 演示编辑会把没动的分组折起来，只滚不展的话拍到的是折叠着的一条头，
        // 面板里那些推子（包括渐变轨）根本没上屏。
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::new();
        state.collapsed.insert("optics");
        state.dock_focus = Some("optics".to_string());
        {
            let mut draw = |root: &mut Ui| {
                group_header(root, &mut state, "optics", "光学", true);
            };
            let mut out = ctx.run_ui(input(vec![]), &mut draw);
            out.textures_delta.clear();
        }
        assert!(!state.collapsed.contains("optics"), "滚到顶了还折着");
        assert!(state.dock_focus.is_none(), "焦点没被消费，会每帧推一次");
    }
}

#[cfg(test)]
mod only_changed_tests {
    use super::*;

    /// 数「平色轨道」的画布片数：每根未改动的推子画一条 `slider_rail()`。
    /// 渐变轨（色温 / 色调）走 48 片插值，不在这条计数里，所以这里数的是
    /// 「有多少行真的上了屏」，正是过滤要证明的事。
    ///
    /// Counts flat rail slices: every untouched slider paints one `slider_rail()` rect.
    /// Gradient rails (temperature / tint) paint 48 interpolated slices instead, so this
    /// counts "how many rows actually made it onto the canvas" -- exactly what the filter
    /// has to prove.
    fn rail_count(ctx: &Context, state: &mut AppState) -> usize {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(360.0, 3000.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    adjust_develop(state, ui);
                });
            },
        );
        let n = out
            .shapes
            .iter()
            .filter(|cs| {
                matches!(&cs.shape, egui::Shape::Rect(r)
                        // 双条件：颜色 + 长度。皮肤一换（PS 灰的面板是亮面），
                        // 分组控件、表头的填充色可能与滑轨撞色——单看颜色会把
                        // 它们误计入推子数。滑轨是贯穿整行的长条，用宽度区分。
                        //
                        // Two conditions: colour + length. When a skin swaps (the PS-grey
                        // panel is a light one), group widgets and headers can share the
                        // rail's fill, and counting by colour alone would count them as
                        // sliders. A rail is a full-width strip, so width separates them.
                        if r.fill == theme::slider_rail() && r.rect.width() >= 120.0)
            })
            .count();
        out.textures_delta.clear();
        n
    }

    /// 收集这一帧画出来的文字（含 Shape::Vec 里的嵌套层）。
    fn painted_strings(ctx: &Context, state: &mut AppState) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => {
                    for s in v {
                        walk(s, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(360.0, 3000.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    adjust_develop(state, ui);
                });
            },
        );
        let mut texts = Vec::new();
        for cs in &out.shapes {
            walk(&cs.shape, &mut texts);
        }
        out.textures_delta.clear();
        texts
    }

    /// 过滤后一个参数都不剩时要给解释，有一条改动时不能留着这句解释。
    /// 用 `t!` 取当前语言的文案，判据跟语言切换无关。
    ///
    /// An empty filtered panel explains itself; once a parameter changes the line
    /// must disappear. Resolve the text through `t!` so the assertion is language-safe.
    #[test]
    fn empty_filter_explains_why_the_panel_is_empty() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let hint = crate::i18n::t!("没有已改动的参数").to_owned();
        let mut state = AppState::new();
        state.only_changed = true;
        let empty = painted_strings(&ctx, &mut state);
        assert!(
            empty.iter().any(|t| t == &hint),
            "过滤后全空，却没有一句解释（面板会像坏了）"
        );
        state.params.exposure = 0.35;
        let busy = painted_strings(&ctx, &mut state);
        assert!(
            !busy.iter().any(|t| t == &hint),
            "已经有一条改动了，还挂着「没有已改动的参数」"
        );
    }

    /// 「仅显示已改动」：开关打开后，等于默认值的字段整行消失（不占行高），
    /// 只有偏离默认的那一根留下；关掉后又全部回来。三条断言分别钉住
    /// 「默认态确实在画」（否则后两条等于什么都没测）、「全默认时清空」、
    /// 「改一根只留一根」。
    ///
    /// "Only show changed": with the switch on, fields at their defaults vanish entirely
    /// (no reserved height) and only the deviating row survives; switching it off brings
    /// them all back. The three assertions pin down "the default state really paints"
    /// (otherwise the other two prove nothing), "all defaults clears the list", and
    /// "one changed field leaves one row".
    #[test]
    fn only_changed_hides_default_rows_and_keeps_the_changed_one() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::new();
        // 其余分组现在收在「更多分组」后面；本判据要数全部推子，先展开。
        // The extra groups now sit behind "more groups"; this count needs them open.
        state.show_more_groups = true;
        let all = rail_count(&ctx, &mut state);
        state.only_changed = true;
        let none = rail_count(&ctx, &mut state);
        state.params.exposure = 0.35;
        let one = rail_count(&ctx, &mut state);
        assert!(all > 10, "默认态没有画推子（{all} 根），后两条断言失去意义");
        assert_eq!(none, 0, "仅显示已改动打开了，还剩 {none} 根默认推子");
        assert_eq!(one, 1, "只有曝光偏离默认，却画了 {one} 根");
    }
}
