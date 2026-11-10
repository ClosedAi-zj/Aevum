//! 右坞骨架：页签容器、直方图与平滑辅助。
//!
//! Right dock skeleton: tab container, histogram and smoothing helpers.

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::adjust::adjust_tab;
use super::library::library_tab;
use super::mask::HistMeshCache;
use super::tabs::ai_tab;
use super::tabs::history_tab;
use super::tabs::info_tab;
use super::tabs::keys_tab;
use crate::icons::{self, Glyph};
use crate::state::{AppState, DockTab};
use crate::theme::{self, *};
use crate::widgets;
use egui::{
    Align2, Color32, Pos2, Rect, ScrollArea, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

// ---------------------------------------------------------------- 右侧面板

/// 面板宽度可拖：在卡的**内缘**挂一条 6px 的手柄，左右拖改 `state.dock_w`。
///
/// 手柄落在卡片内侧那条边（右坞是左缘，左坞是右缘），从页签条下沿一直到底，
/// 拖动方向按侧别取反；宽度夹在 280–560：低于 280，推子行的「标签 76 + 读数 44」
/// 会把轨道吃光；高于 560，画布被挤成一条。悬停/拖动时光标变横向量尺，
/// 用户才知道这里能拖。
///
/// 浮卡形态下手柄只能挂在卡身上：卡的高与宽都是内容算出来的，早先那种「整条
/// 面板内缘」的矩形已经不存在。因此它排在卡内所有内容**之后**绘制，6px 宽的
/// 交互区盖在同一条边上的控件之上，卡片高度再变也不会脱位。
///
/// The panel width is draggable: a 6 px handle sits on the card's **inner** edge (left
/// edge for a right dock, right edge for a left one), running from under the tab strip to
/// the card's bottom. Drag direction flips with the side; width is clamped to 280-560:
/// below 280 the slider row's "label 76 + value 44" eats the rail; above 560 the canvas is
/// squeezed into a strip. The cursor becomes a horizontal resize arrow on hover.
///
/// On a floating card the handle can only hang off the card itself -- height and width
/// both come from the content, so the old "whole panel inner edge" rectangle no longer
/// exists. It is registered after the card's content, so the 6 px interaction strip wins
/// over any control on the same edge and survives the card changing height.
fn dock_resize_handle(state: &mut AppState, ui: &mut Ui, card: Rect) {
    let side_left = state.layout.dock_side.is_left();
    const HANDLE_W: f32 = 6.0;
    const MIN_W: f32 = 280.0;
    const MAX_W: f32 = 560.0;
    let top = card.top() + TAB_H;
    let height = (card.bottom() - top).max(1.0);
    let rect = if side_left {
        Rect::from_min_size(
            Pos2::new(card.right() - HANDLE_W, top),
            Vec2::new(HANDLE_W, height),
        )
    } else {
        Rect::from_min_size(Pos2::new(card.left(), top), Vec2::new(HANDLE_W, height))
    };
    let resp = ui.interact(rect, egui::Id::unique("dock-resize"), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if resp.dragged() {
        let dx = resp.drag_delta().x;
        let delta = if side_left { dx } else { -dx };
        state.dock_w = (state.dock_w + delta).clamp(MIN_W, MAX_W);
    }
}

/// 「仅显示已改动」开关行：设计稿 `.dock-tools{height:24px;justify-content:flex-end}`
/// + `.onlychg`（20px 高、10px 字、圆角 4；开启时蓝底蓝字 + 前置 4px 圆点）。
///
/// 这一行在参数区之上、页签之下，右对齐——它是「看什么」的过滤器，不是参数，
/// 所以不占分组卡内的位置。
///
/// The "only show changed" switch row, per the design's `.dock-tools` / `.onlychg`
/// (20 px high, 10 px text, 4 px radius; when on: blue wash, blue text and a 4 px
/// leading dot).
///
/// It sits between the tabs and the parameter area, right-aligned: it filters what is
/// *shown*, so it must not sit inside any group card.
fn dock_tools(state: &mut AppState, ui: &mut Ui) {
    let (row, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), egui::Sense::hover());
    let galley = ui.painter().layout_no_wrap(
        crate::i18n::t!("仅显示已改动").to_owned(),
        font(10.0),
        if state.only_changed { accent() } else { dim() },
    );
    let dot_w = if state.only_changed { 9.0 } else { 0.0 };
    let w = galley.size().x + 12.0 + dot_w;
    let chip = Rect::from_min_max(
        Pos2::new(row.right() - w, row.center().y - 10.0),
        Pos2::new(row.right(), row.center().y + 10.0),
    );
    let resp = ui.interact(chip, ui.make_persistent_id("only-changed"), Sense::click());
    let p = ui.painter().clone();
    if state.only_changed {
        p.rect_filled(chip, 4.0, accent_dim());
        p.rect_stroke(
            chip,
            4.0,
            Stroke::new(1.0, accent().gamma_multiply(0.34)),
            StrokeKind::Inside,
        );
        p.circle_filled(Pos2::new(chip.left() + 7.0, chip.center().y), 2.0, accent());
    } else if resp.hovered() {
        p.rect_filled(chip, 4.0, elev());
    }
    p.text(
        Pos2::new(chip.left() + 6.0 + dot_w, chip.center().y),
        Align2::LEFT_CENTER,
        crate::i18n::t!("仅显示已改动"),
        font(10.0),
        if state.only_changed { text() } else { dim() },
    );
    if resp.clicked() {
        state.only_changed = !state.only_changed;
    }
    resp.on_hover_text(crate::i18n::t!("只显示已偏离默认值的参数"));
}

/// 检查器卡体：页签条贴顶，下面是可滚动的内容区。
///
/// `width` 是卡的宽度（`state.dock_w`），`max_h` 是卡的**高度上限**（窗口高减
/// 顶栏与上下两道 12px）。卡本身按内容收缩——只有内容超过上限时卡才长到上限，
/// 多出来的部分在卡**内部**滚，卡不出屏、更不会把内容顶到窗口外。
///
/// The inspector body: the tab strip hugs the top, the content below scrolls.
///
/// `width` is the card width (`state.dock_w`), `max_h` its height **cap** (window height
/// minus the top bar and the two 12 px gutters). The card shrinks to its content; only
/// when the content exceeds the cap does it grow to the cap, and the excess scrolls
/// **inside** the card.
pub(super) fn dock_panel(state: &mut AppState, ui: &mut Ui, width: f32, max_h: f32) {
    if std::env::var("AEVUM_DBG_RECT").is_ok() {
        eprintln!(
            "DOCK w={width} max_h={max_h} at {:?}",
            ui.next_widget_position()
        );
    }
    let p = ui.painter().clone();
    // 页签行底色用**面板底色**而不是 header：header 与上方那两条外部信息栏
    // （菜单行 / 选项栏 / 序列页签行）同色，三段连成一片，读起来像「页签跑到
    // 外部信息栏里去了」（用户 2026-11-07 截图）。改面板底色 + 顶缘一道发丝线，
    // 页签行就成为右栏自己的表面。
    //
    // The tab row uses the panel fill rather than `header`: `header` is the same colour as
    // the two outer bars above (menu row, options bar, sequence tab row), so the three
    // merged into one band and read as "the tabs went up into the outer info bar" (user
    // screenshot, 2026-11-07). Panel fill plus a hairline on the top edge makes the tab row
    // its own surface inside the dock.
    // 页签条自己占位：卡高由内容决定，不能再用 `ui.max_rect()` 当卡身——浮卡的
    // 画布给的是「最多这么高」，不是「就是这么高」。
    //
    // The strip allocates its own space: the card height comes from its content, so
    // `ui.max_rect()` can no longer stand in for the card body -- an Area only tells us
    // "at most this tall", not "exactly this tall".
    let (strip, _) = ui.allocate_exact_size(Vec2::new(width, TAB_H), Sense::hover());

    // 浮卡的上沿要跟着卡片圆角走，否则第一行页签是方角、下面卡片是圆角。
    //
    // The floating card's top edge follows the card radius; a square tab strip over a
    // rounded card reads as two different surfaces.
    p.rect_filled(
        strip,
        egui::CornerRadius {
            nw: 12,
            ne: 12,
            sw: 0,
            se: 0,
        },
        dock(),
    );
    theme::rule(&p, strip.left(), strip.right(), strip.top(), divider());
    theme::rule(
        &p,
        strip.left(),
        strip.right(),
        strip.bottom(),
        border_strong(),
    );

    // 面板页签用下划线而不是分段胶囊：一条 HEADER 上排六个名字，
    // 选中项靠白字 + 2px 强调线认，和 PS 的调板页签同一套读法。
    // 页签靠右缘排（用户 2026-11-09）：右缘留给 ×，页签从右往左铺。
    let seg_w = (width - 52.0) / DockTab::ALL.len() as f32;
    for (i, tab) in DockTab::ALL.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(
                strip.right() - 52.0 - (DockTab::ALL.len() - i) as f32 * seg_w + 1.0,
                strip.top() + 4.0,
            ),
            Vec2::new(seg_w - 2.0, strip.height() - 8.0),
        );
        let resp = ui.interact(r, ui.make_persistent_id(("dock-tab", i)), Sense::click());
        let active = state.dock_tab == *tab;
        if active {
            // 与顶栏步骤同一套：0.12s 从中心展开，只动宽度。
            //
            // Same rule as the top step bar: 0.12 s, grows from the centre, width only.
            let t = ui.ctx().animate_bool_with_time(
                ui.make_persistent_id(("dock-tab-underline", i as u8)),
                true,
                0.12,
            );
            let half = (r.width() * 0.5 - 8.0).max(0.0) * t;
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(r.center().x - half, r.bottom() - 1.5),
                    Pos2::new(r.center().x + half, r.bottom()),
                ),
                1.0,
                accent().gamma_multiply(t.max(0.15)),
            );
        } else if resp.is_pointer_button_down_on() {
            p.rect_filled(r, radius_control(), press());
        } else if resp.hovered() {
            p.rect_filled(r, radius_control(), elev_hover());
        }
        // 页签文字按**实测宽度**截断：窄坞（280px）下英文 "Keyframes" 会压到
        // 相邻页签上；而三个汉字实际放得下，按字符数估算会误截成「关…」。
        //
        // 选中页签对齐设计稿 `.grp-h .lb{font-weight:600}` 同一套：加粗 + 正文白
        // （下划线保留）。测量与绘制必须用同一个 FontId——bold 更宽，拿常规
        // 字体量出来的宽度去截粗体字，会截少不截多。
        //
        // Truncate by **measured width**: on a 280 px dock the English "Keyframes"
        // bleeds into the next tab, while three CJK glyphs do fit -- a char-count
        // estimate wrongly cuts "关键帧" down to "关…".
        //
        // The active tab follows the design's `.grp-h .lb{font-weight:600}` family:
        // bold + body-white text (underline stays). Measurement and painting must
        // share one FontId -- bold is wider, so measuring with the regular face and
        // clipping the bold paint truncates too late, never too early.
        let font_id = if active {
            bold(SIZE_SMALL)
        } else {
            font(SIZE_SMALL)
        };
        let full = crate::i18n::tr(tab.cn());
        let avail = (r.width() - 6.0).max(1.0);
        let measured = p
            .layout_no_wrap(full.to_owned(), font_id.clone(), text())
            .size()
            .x;
        // 截断字符数用**实测平均字宽**（measured / 字符数），不用字号常量：
        // 粗体比常规宽一档，常量会估少宽度、截少字符，粗体页签仍可能压到邻签。
        //
        // The truncation budget uses the **measured average glyph width**
        // (measured / char count), not a font-size constant: bold runs wider than
        // regular, so a constant under-estimates and the clipped bold label can
        // still bleed into the next tab.
        let label = if measured <= avail {
            full.to_owned()
        } else {
            let per_char = measured / full.chars().count() as f32;
            crate::theme::ellipsis(full, ((avail / per_char) as usize).max(1))
        };
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            font_id,
            if active || resp.hovered() {
                text()
            } else {
                muted()
            },
        );
        // 关键帧页签带计数徽标（设计稿 `关键帧 1`）：页签本身是入口，数字是
        // 「已经记了几帧」——进页面之前就能判断要不要去看。
        //
        // The keyframes tab carries a count badge (the design's `关键帧 1`): the tab is an
        // entry and the number says how many frames are already recorded, so the decision
        // "do I need that page" is made before opening it.
        if *tab == DockTab::Keys && !state.keyframes.is_empty() && r.width() > 52.0 {
            let badge = Rect::from_min_size(
                Pos2::new(r.right() - 22.0, r.center().y - 7.0),
                Vec2::new(14.0, 14.0),
            );
            p.rect_filled(badge, 4.0, elev());
            p.text(
                badge.center(),
                Align2::CENTER_CENTER,
                format!("{}", state.keyframes.len()),
                font(9.0),
                if active { text() } else { muted() },
            );
        }
        if resp.clicked() {
            state.dock_tab = *tab;
        }
    }

    let close = Rect::from_center_size(
        Pos2::new(strip.right() - 16.0, strip.center().y),
        Vec2::splat(18.0),
    );
    let cr = ui.interact(close, ui.make_persistent_id("dock-close"), Sense::click());
    if cr.hovered() {
        p.rect_filled(close, radius_control(), elev_hover());
    }
    icons::paint(
        &p,
        close.shrink(5.0),
        Glyph::Close,
        if cr.hovered() { text() } else { muted() },
    );
    if cr.clicked() {
        state.show_dock = false;
    }
    cr.on_hover_text(crate::i18n::t!("隐藏面板"));

    // 卡身的高度上限：页签条已经占掉 TAB_H，剩下的才是给内容的（含内距）。
    //
    // The body's height cap: the strip already took TAB_H, the rest is for the content
    // (inner margins included).
    let content = Rect::from_min_max(
        strip.left_top() + Vec2::new(0.0, TAB_H),
        Pos2::new(strip.right(), strip.top() + max_h.max(TAB_H + 1.0)),
    );
    ui.scope_builder(
        UiBuilder::new().id_salt("dock-body").max_rect(content),
        |ui| {
            // 滚动条压到描边色：默认那根是 ELEV，8px 宽的亮条贴在面板右缘，
            // 比面板里任何内容都抢眼，滚起来像一根白杠在爬。
            {
                let w = &mut ui.style_mut().visuals.widgets;
                w.inactive.bg_fill = border();
                w.hovered.bg_fill = border_strong();
                w.active.bg_fill = accent_dim();
            }
            egui::Frame::NONE
                .inner_margin(egui::Margin {
                    left: 10,
                    right: 10,
                    top: 10,
                    bottom: 12,
                })
                .show(ui, |ui| {
                    // 直方图已搬到画布左上浮动卡片（设计稿 .histcard）：
                    // 检查器里再画一遍等于同一份数据出现两次，且占掉右栏首屏。
                    // 画布卡片是唯一一处，这里直接进参数区。
                    //
                    // The histogram moved to the canvas's top-left floating card (the
                    // design's .histcard). Drawing it again here would show the same data
                    // twice and eat the dock's first screen; the canvas card is the one
                    // place, so this jumps straight into the parameter area.
                    ui.add_space(2.0);
                    // 纵向按内容收缩：卡高 = 内容高，只有撞到上限才长到上限并出滚动条。
                    // 横向不许缩（[false, _]）——卡宽恒等于 `dock_w`，否则短内容会把
                    // 卡收窄，右侧少一条边。
                    //
                    // Shrink vertically to the content so the card is exactly as tall as
                    // what it holds; it only reaches the cap (and starts scrolling) when the
                    // content asks for more. Never shrink horizontally ([false, _]): the card
                    // is exactly `dock_w` wide, or short content would narrow it.
                    let mut scroll = ScrollArea::vertical()
                        .id_salt("dock-scroll")
                        .auto_shrink([false, true]);
                    if state.dock_scroll_end {
                        // 只推一次：每一帧都写超大偏移会把内容画到裁剪区外。
                        // egui 在本帧末把偏移钳到底部，后面的帧就停在底部。
                        scroll = scroll.vertical_scroll_offset(1.0e6);
                        state.dock_scroll_end = false;
                    }
                    let out = scroll.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match state.dock_tab {
                            DockTab::Adjust => {
                                dock_tools(state, ui);
                                adjust_tab(state, ui)
                            }
                            DockTab::Info => info_tab(state, ui),
                            DockTab::Keys => keys_tab(state, ui),
                            DockTab::Ai => ai_tab(state, ui),
                            DockTab::History => history_tab(state, ui),
                            DockTab::Library => library_tab(state, ui),
                        }
                        ui.add_space(16.0);
                    });
                    // 滚动区底部 14px 渐隐：滚动到中间时最后一行常被硬切成半行字，
                    // 渐隐垫让内容「没入」面板而不是被裁掉。六段黑阶渐变——面板本
                    // 就近黑，黑阶等于面板色阶，肉眼连续，代价可忽略。
                    //
                    // A 14 px fade at the bottom of the scroll: mid-scroll, the last
                    // row used to be sliced mid-glyph. Six stepped black-alpha rects
                    // (the panel is near-black, so black == panel here) read as one
                    // continuous fade at negligible cost.
                    //
                    // 只在**真溢出**时才垫：卡按内容收缩，短内容下这条渐变会变成卡底
                    // 一道没来由的黑边（历史页签只有两行时最明显）。
                    //
                    // Only when the content really overflows: the card shrinks to its
                    // content, and on short tabs (two rows of history) this gradient would
                    // otherwise show up as an unexplained dark band along the card's bottom.
                    // 渐隐贴**内容**底边（`min_rect`）而不是可用区底边：卡按内容收缩，
                    // 用 `max_rect` 会在卡外多画一条黑带。
                    //
                    // The fade follows the **content's** bottom (`min_rect`), not the
                    // available area's: the card shrinks to its content, so `max_rect` would
                    // paint a black band below the card.
                    let overflowing = out.content_size.y > out.inner_rect.height() + 0.5;
                    let fade_bottom = ui.min_rect().bottom();
                    let fade_left = ui.min_rect().left();
                    let fade_right = ui.min_rect().right();
                    let pf = ui.painter();
                    if overflowing {
                        for s in 0..6u8 {
                            let y0 = fade_bottom - 14.0 + 14.0 * s as f32 / 6.0;
                            let y1 = fade_bottom - 14.0 + 14.0 * (s as f32 + 1.0) / 6.0;
                            pf.rect_filled(
                                Rect::from_min_max(
                                    Pos2::new(fade_left, y0),
                                    Pos2::new(fade_right, y1),
                                ),
                                0.0,
                                Color32::from_black_alpha(15 * (s + 1)),
                            );
                        }
                    }
                });
        },
    );
    // 拖宽：手柄挂在卡的**内缘**，卡高多少它就有多高（页签条那一段留给页签）。
    //
    // Width drag: the handle sits on the card's **inner** edge, as tall as the card
    // (the strip's height is left to the tabs).
    dock_resize_handle(state, ui, ui.min_rect());
}

/// 直方图：对齐 PS / Camera Raw —— 一块近黑的内凹底，三条 RGB 山形用低透明度
/// 的纯色填充、再用饱和细线勾出山脊；重叠处靠叠加自然读出灰褐，不再压一层白色
/// 亮度包络（那层白会让整块泛奶、暗部发灰）。顶角一对剪切角标，只在真裁切时
/// 着色，读数收在悬停提示里。
///
/// The histogram, in PS / Camera Raw's manner: a near-black inset well, three RGB
/// mountains filled at low alpha in pure channel colours with a saturated hairline
/// along each crest. Overlaps go grey-brown on their own, and the white luminance
/// envelope is gone -- that layer milked the whole panel and greyed the shadows.
/// A mirrored pair of clipping wedges sits in the top corners, coloured only when a
/// channel really clips; the percentages live in the hover tooltip.
/// 直方图显示层的两遍 [1,2,1]/4 卷积：三通道山形共用。
/// 抽出来是一份事实源——以后想改平滑强度只动这里。
fn smooth_twice(vals: &mut [f32], bins: usize) {
    for _ in 0..2 {
        let prev = vals.to_vec();
        for b in 0..bins {
            let l = if b == 0 { prev[0] } else { prev[b - 1] };
            let r = if b + 1 == bins {
                prev[bins - 1]
            } else {
                prev[b + 1]
            };
            vals[b] = (l + 2.0 * prev[b] + r) * 0.25;
        }
    }
}

/// 直方图下方那行拍摄参数，LR 的口径：ISO → 焦距 → 光圈 → 快门，四项各占
/// 一格。缺项直接省略，不补破折号——四个「—」比一行空着更吵。
///
/// The capture-parameter line under the histogram in Lightroom's order: ISO, focal
/// length, aperture, shutter, one slot each. Missing fields are omitted rather than
/// padded with dashes -- four em dashes read louder than an empty row.
pub(super) fn capture_line_pub(
    iso: Option<f64>,
    focal_mm: Option<f64>,
    fnum: Option<f64>,
    exposure_sec: Option<f64>,
) -> Vec<String> {
    let mut parts: Vec<String> = Vec::with_capacity(4);
    // 坏 EXIF 里的 0 值不显示：`f / 0.0`、`1/inf` 这类读数比缺项更糟。
    //
    // Zero values from broken EXIF are dropped: `f / 0.0` or `1/inf` reads worse than
    // a missing field.
    if let Some(v) = iso.filter(|v| *v > 0.0) {
        parts.push(format!("ISO {v:.0}"));
    }
    if let Some(v) = focal_mm.filter(|v| *v > 0.0) {
        parts.push(format!("{v:.0} mm"));
    }
    if let Some(v) = fnum.filter(|v| *v > 0.0) {
        // 光圈按 LR 的写法：`f / 8.0`，整档也带一位小数——参考图里就是 8.0。
        //
        // Aperture in Lightroom's spelling: `f / 8.0`, with one decimal even on whole
        // stops -- the reference shows 8.0.
        parts.push(format!("f / {v:.1}"));
    }
    if let Some(sec) = exposure_sec.filter(|v| *v > 0.0) {
        // Shutter in Lightroom's Chinese spelling, with the unit on both sides of one
        // second: `1/250 秒`, `4.0 秒`.
        //
        // 快门按 LR 中文写法，1 秒上下都带单位：`1/250 秒`、`4.0 秒`。
        parts.push(if sec >= 1.0 {
            crate::i18n::tr_fmt("{} 秒", &[&format!("{sec:.1}")])
        } else {
            crate::i18n::tr_fmt("1/{} 秒", &[&format!("{:.0}", 1.0 / sec)])
        });
    }
    parts
}

pub(super) fn histogram(ui: &mut Ui, state: &mut AppState) {
    // 94px：设计图 `.hist-svg{height:94px}`。原 84 让山形比设计图矮一档，
    // 裁切角标与峰值之间的空气也跟着少。
    // 左右各收 4px 且居中：原先只把宽度减 8 再贴左放，左边贴住、右边空 10px，
    // 整块直方图是偏的（对称是这套界面的底线）。
    //
    // 94 px per the design mockup (`.hist-svg{height:94px}`). The old 84 made the
    // mountain shapes one step shorter than the design and squeezed the air between the
    // clipping wedges and the peaks. Four px off each side, centred: the old version
    // only subtracted 8 from the width and left-aligned the well, so the left edge
    // touched while the right kept 10 px -- the whole block sat off-centre.
    let inset = 4.0;
    // 高度吃满调用方给的槽位（上限 94）：浮卡里槽位是 63（.histcard img），dock 里
    // 是 94（.hist-svg）。写死 94 会把 63 的卡撑爆——这正是卡高比设计稿多 26px 的来源。
    //
    // Height fills the caller's slot (cap 94): the floating card gives 63 (.histcard img),
    // the dock gives 94 (.hist-svg). A hard 94 blew the card up by 26 px.
    let (slot, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), ui.available_height().min(94.0)),
        Sense::hover(),
    );
    let rect = slot.shrink2(Vec2::new(inset, 0.0));
    let p = ui.painter().clone();
    let Some(h) = state.hist else {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "等待预览直方图",
            font(SIZE_SMALL),
            dim(),
        );
        return;
    };

    let bins = 128usize;
    // 分母必须和画出来的量同口径：每个点是相邻两个 bin 之和，就取「两 bin 之和」
    // 的最大值。用单 bin 最大值当分母时，尖峰摊在两个相邻 bin 上，分子能到
    // 2×max，山形直接冲出凹槽顶部（用户 2026-11-07 截图里的那条直线）。
    //
    // The divisor must match the plotted quantity: each point is the sum of two
    // adjacent bins, so normalise by the max of those sums. With the single-bin max,
    // a spike split across two neighbours reaches 2x and the mountain leaves the well.
    let max = (0..bins)
        .map(|b| {
            (0..3)
                .map(|ch| h[ch][b * 2] + h[ch][b * 2 + 1])
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let bar_w = rect.width() / bins as f32;
    // RGB 读数胶囊（ACR 同位：直方图顶部居中）：三通道均值 ×255。
    // 加权均值：Σ(档位×计数)/Σ计数 → 0..255 的真实平均亮度
    let means: Vec<f32> = h
        .iter()
        .map(|c| {
            let total: u64 = c.iter().map(|v| u64::from(*v)).sum::<u64>().max(1);
            let weighted: u64 = c
                .iter()
                .enumerate()
                .map(|(i, v)| u64::from(*v) * i as u64)
                .sum();
            weighted as f32 / total as f32
        })
        .collect();
    let cap_text = format!(
        "R {}  G {}  B {}",
        means[0].round(),
        means[1].round(),
        means[2].round()
    );
    {
        let cap_g = ui.painter().layout_no_wrap(
            cap_text,
            egui::FontId::monospace(10.0),
            Color32::from_rgb(210, 210, 210),
        );
        let cs = cap_g.size() + Vec2::new(16.0, 4.0);
        let cap_r = Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 8.0), cs);
        ui.painter()
            .rect_filled(cap_r, cs.y * 0.5, Color32::from_black_alpha(120));
        ui.painter().galley(
            cap_r.left_top() + Vec2::new(8.0, 2.0),
            cap_g,
            Color32::from_rgb(210, 210, 210),
        );
    }

    // 内凹底：比面板沉两档的近黑，加一道细描边，山形才有「装在一个凹槽里」的
    // 读法（PS 的直方图就是一块黑底）。浅色主题只压一点点，否则一块黑砖砸在
    // 白面板上。
    //
    // The inset well: near-black, two steps below the panel, with a hairline outline,
    // so the mountains read as sitting in a recess (PS's histogram is a black well).
    // The light theme darkens only slightly, or a black brick lands on a white panel.
    {
        // 设计稿 `.histcard img{background:var(--field)}`：凹槽底就是 field，
        // 不是把 field 压向黑——深 42% 的近黑让直方图像一块黑板，和目标的
        // 「沉一格的槽」差着半档。
        //
        // The design's `.histcard img{background:var(--field)}`: the well IS field, not
        // field pushed 42% toward black -- the near-black read like a chalkboard.
        let depth = if theme::is_light() { 0.12 } else { 0.0 };
        let well = widgets::mix(theme::field(), Color32::BLACK, depth);
        p.rect_filled(rect, radius_control(), well);
        p.rect_stroke(
            rect,
            radius_control(),
            Stroke::new(1.0, divider()),
            egui::StrokeKind::Inside,
        );
    }

    // 3 个填充 Mesh + 3 条山脊线走缓存（E6）：键 = (hist_version, rect)。
    // 新预览落地时 hist_version 才变；静止悬停帧直接 clone —— Shape::Mesh 持
    // Arc<Mesh>（epaint 0.37 已确认），clone 零拷贝。
    //
    // Three fill meshes plus three crest lines ride the cache: key = (hist_version,
    // rect). hist_version only changes when a new preview lands; idle hover frames
    // clone shapes, and Shape::Mesh holds an Arc<Mesh> (confirmed in epaint 0.37).
    let cached = match &state.hist_mesh_cache {
        Some(c) if c.version == state.hist_version && c.rect == rect => Some(c.shapes.clone()),
        _ => None,
    };
    let shapes = match cached {
        Some(shapes) => shapes,
        None => {
            // 每条山形是一个三角带：每个 bin 四个顶点（左峰、右峰、左底、右底），
            // 顶点色从山脊的低透明度纯色衰减到底边的更淡——egui 没有纵向渐变填充，
            // 逐顶点插值就是渐变本身。alpha 压得低，三通道重叠处才会叠出 PS 那种
            // 灰褐，而不是一层奶白。
            //
            // Each mountain is a triangle strip: four vertices per bin (left crest,
            // right crest, left base, right base), with a low-alpha pure colour at the
            // crest fading to an even fainter base -- egui has no vertical gradient fill,
            // so per-vertex interpolation is the gradient. Alpha stays low so overlapping
            // channels land on PS's grey-brown instead of a layer of milk.
            const CREST_A: u8 = 88;
            const BASE_A: u8 = 14;
            let channel_rgb: [[u8; 3]; 3] =
                [[0xC8, 0x3C, 0x38], [0x3C, 0x96, 0x4B], [0x37, 0x69, 0xD2]];
            let ridge_rgb: [[u8; 3]; 3] =
                [[0xF0, 0x5A, 0x50], [0x5E, 0xD8, 0x78], [0x5A, 0x9B, 0xFF]];
            let mut shapes: Vec<Shape> = Vec::with_capacity(6);
            for ch in 0..3 {
                let mut mesh = egui::Mesh::default();
                // 显示平滑：[1,2,1]/4 卷积两遍。原始 bin 在尖峰数据（大面积纯色天空）
                // 下会画出锯齿山，界面稿的圆滑感来自它的样例数据本身平缓 ——
                // 卷积只动显示层，不碰 state.hist 的原始数据。PS 的山脊仍带细密锯齿，
                // 所以只走一遍；原来四遍会把真实照片的直方图糊成一块色团。
                //
                // Display smoothing: two passes of the [1,2,1]/4 kernel. Raw bins spike on
                // flat subjects (a big patch of sky) and look like a saw; the mockup's
                // smoothness came from gently varying sample data. This only touches the
                // display layer. PS's ridges still carry fine saw teeth, so one pass only --
                // the old four passes smeared a real photo into a blob.
                let raw: Vec<f32> = (0..bins)
                    .map(|b| {
                        let lo = b * 2;
                        let count = h[ch][lo] + h[ch][lo + 1];
                        if count == 0 {
                            0.0
                        } else {
                            (count as f32 / max).powf(0.55).clamp(0.0, 1.0)
                        }
                    })
                    .collect();
                let mut vals = raw.clone();
                smooth_twice(&mut vals, bins);
                for (b, &v) in vals.iter().enumerate().take(bins) {
                    let x0 = rect.left() + b as f32 * bar_w;
                    let x1 = x0 + bar_w;
                    // 山形封顶在 94%：峰值贴着页签行的「超出」观感（用户截图）——
                    // 留 6% 空气，山头与 tab 行之间永远有一线呼吸。
                    let y_top = rect.bottom() - rect.height() * 0.94 * v;
                    let [r, g, bl] = channel_rgb[ch];
                    let i = mesh.vertices.len() as u32;
                    mesh.colored_vertex(
                        Pos2::new(x0, y_top),
                        Color32::from_rgba_unmultiplied(r, g, bl, CREST_A),
                    );
                    mesh.colored_vertex(
                        Pos2::new(x1, y_top),
                        Color32::from_rgba_unmultiplied(r, g, bl, CREST_A),
                    );
                    mesh.colored_vertex(
                        Pos2::new(x0, rect.bottom()),
                        Color32::from_rgba_unmultiplied(r, g, bl, BASE_A),
                    );
                    mesh.colored_vertex(
                        Pos2::new(x1, rect.bottom()),
                        Color32::from_rgba_unmultiplied(r, g, bl, BASE_A),
                    );
                    mesh.add_triangle(i, i + 1, i + 3);
                    mesh.add_triangle(i, i + 3, i + 2);
                }
                shapes.push(egui::Shape::Mesh(mesh.into()));

                // 山脊线：PS 把通道读成一根 1px 的彩色细线，填充只是线的附庸。
                // 没有这根线，低透明度的三条填充在深底上会糊成一团雾。
                //
                // The crest line: PS reads a channel as a 1 px coloured hairline with the
                // fill as its shadow. Without the line, three low-alpha fills dissolve into
                // fog on the dark well.
                let [r, g, bl] = ridge_rgb[ch];
                let mut pts: Vec<Pos2> = vals
                    .iter()
                    .enumerate()
                    .map(|(b, &v)| {
                        Pos2::new(
                            rect.left() + (b as f32 + 0.5) * bar_w,
                            rect.bottom() - rect.height() * 0.94 * v,
                        )
                    })
                    .collect();
                if let Some(last) = pts.last_mut() {
                    last.x = rect.right();
                }
                shapes.push(egui::Shape::line(
                    pts,
                    Stroke::new(1.15, Color32::from_rgb(r, g, bl)),
                ));
            }

            state.hist_mesh_cache = Some(HistMeshCache {
                version: state.hist_version,
                rect,
                shapes: shapes.clone(),
            });
            shapes
        }
    };
    for shape in &shapes {
        p.add(shape.clone());
    }

    // 剪切角标：左右一对小三角压在直方图顶角，平时只描一道细边，真裁切才着色。
    // 角标就是裁切罩的开关，读数与提示都收进悬停；原先下面那行「阴影 0.0% /
    // 高光 0.0%」被反馈太啰嗦，静态不再留文字。
    //
    // The clipping wedges: a mirrored pair in the histogram's top corners, a hairline
    // when idle and a filled colour when a channel really clips. Each wedge toggles its
    // clip mask, and the readout lives in the tooltip -- the old "Shadows 0.0% /
    // Highlights 0.0%" row was too wordy and is gone.
    let total: u64 = (0..256)
        .map(|v| h[0][v] as u64 + h[1][v] as u64 + h[2][v] as u64)
        .sum();
    let low: u64 = (0..3)
        .map(|v| h[0][v] as u64 + h[1][v] as u64 + h[2][v] as u64)
        .sum();
    let high: u64 = (253..256)
        .map(|v| h[0][v] as u64 + h[1][v] as u64 + h[2][v] as u64)
        .sum();
    let pct = |n: u64| {
        if total > 0 {
            format!("{:.1}", n as f32 * 100.0 / total as f32)
        } else {
            "0.0".to_owned()
        }
    };
    for (left, on, clip, col, salt) in [
        (
            true,
            state.clip_shadows,
            total > 0 && (low as f32 / total as f32) > 0.0003,
            Color32::from_rgb(56, 110, 255),
            "clip-wedge-shadow",
        ),
        (
            false,
            state.clip_highlights,
            total > 0 && (high as f32 / total as f32) > 0.0003,
            Color32::from_rgb(255, 46, 46),
            "clip-wedge-highlight",
        ),
    ] {
        let hit = Rect::from_min_size(
            if left {
                rect.left_top()
            } else {
                Pos2::new(rect.right() - 20.0, rect.top())
            },
            Vec2::splat(20.0),
        );
        let resp = ui.interact(hit, ui.make_persistent_id(salt), Sense::click());
        if on || resp.hovered() {
            p.rect_filled(
                hit,
                radius_control(),
                if on {
                    Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 40)
                } else {
                    elev()
                },
            );
        }
        let (x0, x1) = if left {
            (hit.left() + 4.0, hit.left() + 13.0)
        } else {
            (hit.right() - 4.0, hit.right() - 13.0)
        };
        let tri = vec![
            Pos2::new(x0, hit.top() + 4.0),
            Pos2::new(x1, hit.top() + 4.0),
            Pos2::new(x0, hit.top() + 13.0),
        ];
        if clip {
            p.add(egui::Shape::convex_polygon(tri.clone(), col, Stroke::NONE));
        } else {
            p.add(egui::Shape::closed_line(
                tri.clone(),
                Stroke::new(1.0, dim()),
            ));
        }
        if on {
            p.add(egui::Shape::closed_line(tri, Stroke::new(1.5, col)));
        }
        let readout = if left {
            crate::i18n::tr_fmt("◧ 阴影 {}%", &[&pct(low)])
        } else {
            crate::i18n::tr_fmt("高光 {}% ◨", &[&pct(high)])
        };
        let mask = crate::i18n::tr(if left {
            if on {
                "阴影裁切罩：开"
            } else {
                "阴影裁切罩：关"
            }
        } else if on {
            "高光裁切罩：开"
        } else {
            "高光裁切罩：关"
        });
        let resp = resp.on_hover_text(crate::i18n::tr_fmt(
            "{}（J 键循环开关）",
            &[&format!("{readout} · {mask}")],
        ));
        if resp.clicked() {
            if left {
                state.clip_shadows = !state.clip_shadows;
            } else {
                state.clip_highlights = !state.clip_highlights;
            }
            state.clip_mask_stamp = u64::MAX;
        }
    }

    // 拍摄参数（LR 同位同序）：紧贴直方图下方，按等间距铺满凹槽的宽度，两端
    // 留白相同——等分槽位会让短字段之间忽宽忽窄，对称的节奏只能来自等间隙。
    //
    // Capture parameters (Lightroom's slot and order) right under the histogram,
    // spread with equal gaps across the well's width and equal end margins. Equal-slot
    // layout made the gaps jump between short fields; an even rhythm needs equal gaps.
    if let Some(e) = state.exif.get(state.selected) {
        let items = capture_line_pub(e.iso(), e.focal_mm(), e.fnum(), e.exposure_sec());
        if !items.is_empty() {
            ui.add_space(7.0);
            let (row, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
            let galleys: Vec<_> = items
                .iter()
                .map(|s| {
                    ui.painter().layout_no_wrap(
                        theme::ellipsis(s, 12).to_owned(),
                        font(10.0),
                        dim(),
                    )
                })
                .collect();
            let used: f32 = galleys.iter().map(|g| g.size().x).sum();
            let margin = 6.0;
            let gap = if galleys.len() > 1 {
                ((rect.width() - margin * 2.0 - used) / (galleys.len() - 1) as f32).max(8.0)
            } else {
                0.0
            };
            let mut x = rect.left() + margin;
            for g in &galleys {
                p.galley(
                    Pos2::new(x, row.center().y - g.size().y * 0.5),
                    g.clone(),
                    dim(),
                );
                x += g.size().x + gap;
            }
        }
    }
}

/// 直方图与推子读数：三色柱、白色亮度折线、改动后才变蓝的数值。
/// 这些全是「不上屏不会报错」的画法，只能照色认形状。
#[cfg(test)]
mod dock_paint_tests {
    use super::*;
    use crate::shell::common::group_help;
    use crate::shell::common::slider;
    use crate::shell::ui;
    use crate::state::Module;
    use egui::Context;

    /// 拍摄参数行的拼法与省略规则：齐了按 LR 的顺序 ISO → 焦距 → 光圈 → 快门
    /// 排，光圈写 `f / 8.0`，快门一律带「秒」，缺项不补占位、全缺就是空表。
    ///
    /// The capture-line composition and omission rules: Lightroom's order ISO, focal,
    /// aperture, shutter; aperture as `f / 8.0`; the shutter always carries the unit;
    /// missing fields leave no placeholder, and an all-missing frame yields no items.
    #[test]
    fn capture_line_formats_and_omits_missing_fields() {
        assert_eq!(
            capture_line_pub(Some(200.0), Some(35.0), Some(8.0), Some(1.0 / 250.0)),
            vec!["ISO 200", "35 mm", "f / 8.0", "1/250 秒"]
        );
        assert_eq!(
            capture_line_pub(Some(1600.0), Some(50.0), Some(1.8), Some(1.0 / 60.0)),
            vec!["ISO 1600", "50 mm", "f / 1.8", "1/60 秒"]
        );
        // 1 秒以上：LR 中文口径写「4.0 秒」。
        assert_eq!(
            capture_line_pub(Some(100.0), Some(249.0), Some(8.0), Some(4.0)),
            vec!["ISO 100", "249 mm", "f / 8.0", "4.0 秒"]
        );
        // 缺项：直接跳过，不补「—」。
        assert_eq!(
            capture_line_pub(Some(100.0), None, Some(4.0), None),
            vec!["ISO 100", "f / 4.0"]
        );
        assert!(capture_line_pub(None, None, None, None).is_empty());
        // 坏 EXIF 的 0 值当缺项：不出 `f / 0.0` 或 `1/inf`。
        assert!(capture_line_pub(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).is_empty());
    }

    fn input(w: f32, h: f32) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(w, h))),
            max_texture_side: Some(8192),
            ..Default::default()
        }
    }

    /// 一块有峰有谷的直方图：每个通道都铺满 128 档，山脊线才画得出折线的凹处。
    fn textured_hist() -> [[u32; 256]; 3] {
        let shapes = [(7u32, 40u32, 4u32), (11, 30, 3), (13, 36, 5)];
        shapes.map(|(mul, span, base)| std::array::from_fn(|v| base + (v as u32 * mul) % span))
    }

    /// 尖峰直方图：一个高台横跨相邻两档——正是「分子是两 bin 之和、分母却取
    /// 单 bin 最大值」会翻倍的那种数据。旧实现下它必然冲出凹槽顶部。
    ///
    /// A spiky histogram: one plateau straddling two adjacent bins -- exactly the case
    /// where plotting a two-bin sum against a single-bin max doubles the ratio and sent
    /// the mountain out of the well.
    fn spiky_hist() -> [[u32; 256]; 3] {
        let mut h = [[1u32; 256]; 3];
        h[0][100] = 4000;
        h[0][101] = 4000;
        h[2][100] = 3500;
        h[2][101] = 3500;
        h
    }

    #[test]
    fn histogram_peaks_stay_inside_the_well() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::new();
        state.hist = Some(spiky_hist());
        let mut draw = |ui: &mut Ui| histogram(ui, &mut state);
        let mut out = ctx.run_ui(input(320.0, 200.0), &mut draw);
        let well = if theme::is_light() {
            widgets::mix(theme::field(), Color32::BLACK, 0.12)
        } else {
            theme::field()
        };
        let mut well_rect = None;
        for clipped in &out.shapes {
            if let egui::Shape::Rect(r) = &clipped.shape
                && r.fill == well
                && r.rect.height() > 80.0
            {
                well_rect = Some(r.rect);
            }
        }
        let well_rect = well_rect.expect("直方图的凹槽没上屏");
        let mut topmost = f32::INFINITY;
        for clipped in &out.shapes {
            match &clipped.shape {
                egui::Shape::Mesh(m) => {
                    for v in &m.vertices {
                        topmost = topmost.min(v.pos.y);
                    }
                }
                egui::Shape::Path(path) => {
                    // 只看山脊线：剪切角标是无描边的填充三角，压着凹槽顶角。
                    if let egui::epaint::ColorMode::Solid(col) = path.stroke.color
                        && col.a() > 0
                    {
                        for pt in &path.points {
                            topmost = topmost.min(pt.y);
                        }
                    }
                }
                _ => {}
            }
        }
        out.textures_delta.clear();
        // 不只要求「不越界」，还要保住 94% 封顶留下的那一线空气。
        assert!(
            topmost >= well_rect.top() + 4.0,
            "直方图冲出凹槽顶部：最高点 y={topmost}，凹槽顶 y={}",
            well_rect.top()
        );
    }

    #[test]
    fn histogram_paints_three_channel_fills_and_crest_lines() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::new();
        state.hist = Some(textured_hist());
        let mut draw = |ui: &mut Ui| histogram(ui, &mut state);
        let mut out = ctx.run_ui(input(320.0, 200.0), &mut draw);
        // 新规格（PS 口径）：三条低透明度纯色填充山形 + 三条饱和山脊细线。
        // 顶点色经 from_rgba_unmultiplied 落进顶点时已按 alpha 压平，
        // 所以期望值也用同一构造算出来，逐色完整比较。
        let channel_rgb: [[u8; 3]; 3] =
            [[0xC8, 0x3C, 0x38], [0x3C, 0x96, 0x4B], [0x37, 0x69, 0xD2]];
        let ridge_rgb: [[u8; 3]; 3] = [[0xF0, 0x5A, 0x50], [0x5E, 0xD8, 0x78], [0x5A, 0x9B, 0xFF]];
        let crest: Vec<Color32> = channel_rgb
            .iter()
            .map(|rgb| Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], 88))
            .collect();
        let base: Vec<Color32> = channel_rgb
            .iter()
            .map(|rgb| Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], 14))
            .collect();
        let ridges: Vec<Color32> = ridge_rgb
            .iter()
            .map(|rgb| Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
            .collect();
        let mut channel_meshes = [0usize; 3];
        let mut ridge_lines = [0usize; 3];
        let mut white_wash = 0usize;
        let mut solid_bars = 0usize;
        for clipped in &out.shapes {
            match &clipped.shape {
                egui::Shape::Mesh(m) => {
                    let mut touched = [false; 3];
                    for v in &m.vertices {
                        for k in 0..3 {
                            if v.color == crest[k] || v.color == base[k] {
                                touched[k] = true;
                            }
                        }
                        if v.color == Color32::from_white_alpha(22) {
                            white_wash += 1;
                        }
                    }
                    for k in 0..3 {
                        if touched[k] {
                            channel_meshes[k] += 1;
                        }
                    }
                }
                egui::Shape::Path(path) => {
                    if let egui::epaint::ColorMode::Solid(color) = path.stroke.color
                        && let Some(k) = ridges.iter().position(|c| *c == color)
                    {
                        ridge_lines[k] += 1;
                    }
                }
                egui::Shape::Rect(r) => {
                    // 旧实现是逐 bin 实心矩形柱：新实现不许再出现这类色块。
                    // 直方图自己的内凹底不算——它是一整块带一点点色偏的近黑，
                    // 按「不透明彩色矩形」的字面判据会被误抓。
                    //
                    // The old implementation drew one solid rectangle per bin; that must
                    // not come back. The histogram's own inset well is exempt: it is one
                    // near-black slab with a faint tint that the literal "opaque coloured
                    // rect" criterion would otherwise catch.
                    // 豁免色必须跟实现同步：深色主题凹槽底现在就是 field 本身
                    // （见 histogram 内注释），浅色主题仍压 12%。
                    //
                    // The exemption must track the implementation: the dark-theme well is
                    // field itself now (see histogram), light still darkens 12%.
                    let well = if theme::is_light() {
                        widgets::mix(theme::field(), Color32::BLACK, 0.12)
                    } else {
                        theme::field()
                    };
                    let opaque_colored = r.fill.a() > 100
                        && (r.fill[0] != r.fill[1] || r.fill[1] != r.fill[2])
                        && r.fill != well;
                    if opaque_colored {
                        solid_bars += 1;
                    }
                }
                _ => {}
            }
        }
        out.textures_delta.clear();
        for (ch, n) in channel_meshes.iter().enumerate() {
            assert!(*n >= 1, "{} 通道的填充山形没上屏", ["红", "绿", "蓝"][ch]);
        }
        for (ch, n) in ridge_lines.iter().enumerate() {
            assert!(*n >= 1, "{} 通道的山脊线没上屏", ["红", "绿", "蓝"][ch]);
        }
        assert_eq!(white_wash, 0, "直方图又压上了白色亮度包络");
        assert_eq!(solid_bars, 0, "直方图又退回了实心矩形柱");
    }

    fn value_color(v: f64) -> Color32 {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut value = v;
        let mut draw = |ui: &mut Ui| {
            slider(ui, "exposure", "曝光", &mut value, -5.0, 5.0, 0.0, 2, true);
        };
        let mut out = ctx.run_ui(input(320.0, 120.0), &mut draw);
        let mut found = None;
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape {
                let text = t.galley.text().trim().to_owned();
                if text == "0.00" || text == "+0.75" {
                    found = t.galley.job.sections.first().map(|s| s.format.color);
                }
            }
        }
        out.textures_delta.clear();
        found.expect("推子的读数没上屏")
    }

    #[test]
    fn slider_value_is_flat_grey_at_default_and_text_once_changed() {
        // 2026-11-10 第三次换口径（设计稿 v10.6/v12 的 .srow.changed .val）：
        // **改动值 = 琥珀 #f5B83D**，未动值 = 正文白 text()（设计稿 `.val{color:var(--text)}`）。
        // 琥珀是设计稿固定给"改动/时间"的语义色；蓝色仍只留给交互态（accent）。
        //
        // Third colour ruling (2026-11-10, per the design's .srow.changed .val): a
        // **changed value is amber #f5B83D**, untouched stays body white `text()` (the
        // design's `.val{color:var(--text)}`). Amber owns the "changed/time" semantics;
        // blue stays interaction-only.
        assert_eq!(value_color(0.0), text(), "默认值的读数应走正文白");
        assert_eq!(
            value_color(0.75),
            Color32::from_rgb(0xF5, 0xB8, 0x3D),
            "改过的读数应是设计稿琥珀 #f5b83d"
        );
        assert_ne!(value_color(0.0), value_color(0.75), "两种状态必须分得开");
    }

        /// 显影十二组每组必须有一枚 ⓘ 与一段真实说明（设计稿 `.grp-h .help`）：
    /// 漏一组，那组的「这是什么、动了对吗」就只剩参数名可以猜。
    ///
    /// Every one of the twelve develop groups must carry an info glyph with a real
    /// note (the design's `.grp-h .help`): miss one and that group's "what is this,
    /// did I break it" is left to guessing from parameter names alone.
    #[test]
    fn every_develop_group_has_a_real_help_note() {
        for key in [
            "basic",
            "profile",
            "grade",
            "tone-extra",
            "curve",
            "mixer",
            "detail",
            "optics",
            "geometry",
            "retouch",
            "effects",
            "look",
        ] {
            let help = group_help(key).unwrap_or_else(|| panic!("{key} 缺 ⓘ 说明"));
            assert!(help.chars().count() >= 16, "{key} 的说明太短，等于没写");
        }
    }

    /// 整段说明只许待在 ⓘ 的悬停提示里：正文铺回去，面板又变成一列散文。
    #[test]
    fn explanations_live_in_group_help_not_on_the_panel() {
        for key in [
            "copies",
            "hdr",
            "pano",
            "stack",
            "timeblend",
            "basic",
            "curve",
            "mixer",
            "optics",
        ] {
            let help = group_help(key).expect("分组没有说明入口");
            assert!(help.chars().count() >= 12, "{key} 的说明太短，等于没写");
        }
        // 界面帧每帧 drain 全进程共享的提示队列：持 env 的测试锁（见 env.rs）。
        let _serial = aevum_core::env::lock_notes_for_test();
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        state.module = Module::Develop;
        let mut out = ctx.run_ui(input(1600.0, 1000.0), |root| ui(&mut state, root));
        let mut text = String::new();
        for clipped in &out.shapes {
            if let egui::Shape::Text(t) = &clipped.shape {
                text.push_str(t.galley.text());
            }
        }
        out.textures_delta.clear();
        for old in [
            "同场景的多张不同曝光",
            "多张有重叠的照片",
            "还没有副本：调好一版",
        ] {
            assert!(!text.contains(old), "长句又铺回面板了：{old}");
        }
    }
}
