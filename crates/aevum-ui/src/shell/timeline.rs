//! 画布左下角的浮动胶片卡：头行、逐帧亮度曲线、缩略图行与走带。
//!
//! The floating film card at the canvas' bottom-left: header, frame curve,
//! thumbnail row and transport. 设计稿 `.filmcard`（`left:12 bottom:12 padding:6
//! radius 12 shadow`），内容自上而下：`胶片条 · N 帧` + 折叠钮 + 筛选/星级 →
//! 逐帧亮度曲线 → 缩略图行（72×56，选中 2px 蓝环）→ 走带行。

use super::MICRO;
use super::mask::DeflCurveCache;
use super::menu::menu_style;
use crate::icons::{self, Glyph};
use crate::state::AppState;
use crate::theme::{self, *};
use egui::{
    Align, Align2, Color32, Context, Layout, Pos2, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2,
};
use std::sync::Arc;

// ---------------------------------------------------------------- 卡片尺寸
//
// 全部对齐设计稿：`.filmcard{padding:6px}`、`.film-h{height:24px}`、
// `.curve`（48px 整块）、`.th{width:72px;height:56px}`、`.tp{height:26px}`。
//
// Every number below is the design's: card padding 6, header 24, curve block 48,
// thumbnail 72x56, transport 26.

/// 卡片内距（设计稿 `padding:6px`）。
const PAD: f32 = 6.0;
/// 头行高度（`.film-h{height:24px}`）。
const HEAD_H: f32 = 22.0;
/// 曲线块高度（`.curve` 标题 10 + 图 26 + 刻度 3 + 内距）。
const CURVE_H: f32 = 36.0;
/// 曲线块与缩略图行之间的间距（`.curve{margin-bottom:5px}`）。
const GAP: f32 = 5.0;
/// 缩略图格宽 / 格高 / 格距（`.th{72×56}`、`.thumbs{gap:4}`）。
const THUMB_W: f32 = 72.0;
const THUMB_H: f32 = 52.0;
const THUMB_GAP: f32 = 4.0;
/// 走带行上缘留白与行高（`.tp{margin-top:6px;height:26px}`）。
const TP_MT: f32 = 4.0;
const TP_H: f32 = 24.0;
/// 卡片最小宽度：头行（标题 + 折叠钮 + 筛选整组 + 曲线开关）放得下。
/// 设计稿 7 帧时卡宽 535（由缩略图撑开），4 帧时由头行撑到 400。
const CARD_MIN_W: f32 = 400.0;
/// 右端「曲线」开关宽度。
const CURVE_W: f32 = 34.0;
/// 筛选组里星级 / 旗标 / 色标的格距（`.star-filter button{width:14px}`）。
const DOT: f32 = 14.0;
/// 筛选组内元素间距（`.filters{gap:4px}`）。
const FGAP: f32 = 6.0;

/// 缩略图行高：由 `state.film_height()` 决定（它按首帧比例算），但卡片里的一格
/// 固定到设计稿的 56px——超出的部分不再把整张卡撑高。
///
/// `film_height()` 的语义没有变：它仍然是「缩略图该多高」的唯一来源，用户把它
/// 调矮过（`film_h`）时这里也跟着变矮。
///
/// Thumbnail row height: still derived from `state.film_height()` (the single source
/// for "how tall are the thumbnails"), folded into the design's fixed 56 px cell so
/// the card never grows past the mockup.
fn strip_h(state: &AppState) -> f32 {
    state.film_height().min(THUMB_H)
}

/// 卡片高度 = 内距 + 头行 + [曲线 + 间距] + [缩略图行] + 走带。
///
/// Card height = paddings + header + [curve + gap] + [thumb row] + transport.
pub(super) fn card_height(state: &AppState) -> f32 {
    let mut h = PAD * 2.0 + HEAD_H + TP_MT + TP_H;
    if state.show_curves {
        h += CURVE_H + GAP;
    }
    if state.show_film {
        h += strip_h(state);
    }
    h
}

fn title_text(state: &AppState) -> String {
    let n = state.frame_count();
    if n > 0 {
        crate::i18n::tr_fmt("胶片条 · {} 帧", &[&n])
    } else {
        crate::i18n::tr("胶片条 · 无序列").to_owned()
    }
}

fn filters_on(state: &AppState) -> bool {
    state.film_filter_stars > 0 || state.film_filter_flag != 0 || state.film_filter_label > 0
}

/// 筛选组各段宽度：`筛选` 字样、五颗星、旗标、色标、清除、计数。
///
/// Widths of the filter group, measured once and reused by both the card-sizing and
/// the painting path so the two can never disagree.
struct FilterW {
    label: f32,
    clear: f32,
    count: f32,
    total: f32,
}

fn filter_widths(state: &AppState, p: &egui::Painter) -> FilterW {
    let label = p
        .layout_no_wrap(
            crate::i18n::tr("筛选").to_owned(),
            font(SIZE_SMALL),
            muted(),
        )
        .size()
        .x;
    let clear = p
        .layout_no_wrap(
            crate::i18n::tr("清除筛选").to_owned(),
            font(SIZE_SMALL),
            muted(),
        )
        .size()
        .x;
    let count = if filters_on(state) {
        p.layout_no_wrap(
            crate::i18n::tr_fmt(
                "已筛选 {} / {} 帧",
                &[&state.film_visible_count(), &state.frame_count()],
            ),
            mono(MICRO),
            dim(),
        )
        .size()
        .x
    } else {
        0.0
    };
    let stars = 5.0 * DOT;
    let flags = 2.0 * DOT;
    let labels = 4.0 * DOT;
    let mut total = label + FGAP + stars + FGAP + flags + FGAP + labels + FGAP + clear;
    if count > 0.0 {
        total += FGAP + count;
    }
    FilterW {
        label,
        clear,
        count,
        total,
    }
}

/// 头行需要的最小内容宽度（不含卡片内距）。
fn head_need(state: &AppState, p: &egui::Painter) -> f32 {
    let title = p
        .layout_no_wrap(title_text(state), font(SIZE_SMALL), dim())
        .size()
        .x;
    // 2 左缩进 + 标题 + 8 + 折叠钮 22 + 10 + 筛选组 + 8 + 曲线开关
    2.0 + title + 8.0 + 22.0 + 10.0 + filter_widths(state, p).total + 8.0 + CURVE_W
}

/// 卡宽随内容：头行所需与缩略图自然宽取大，且不超过 `max_w`（窗口一半）。
///
/// Width follows the content: the header's need versus the thumbnails' natural run,
/// clamped to `max_w` (half the window) so the card can never split the canvas.
fn card_width(state: &AppState, ui: &Ui, max_w: f32) -> f32 {
    let p = ui.painter();
    let mut w = head_need(state, p).max(CARD_MIN_W - PAD * 2.0);
    if state.show_film && state.frame_count() > 0 {
        let n = state.frame_count() as f32;
        w = w.max(n * (THUMB_W + THUMB_GAP) - THUMB_GAP);
    }
    (w + PAD * 2.0).min(max_w.max(CARD_MIN_W))
}

// ---------------------------------------------------------------- 卡片

/// 画整张卡。`rect` 由调用方按 `card_height()` 定好，`max_w` 是窗口宽的一半。
///
/// `docked = true` 是老式布局用的贴边形态：宽度钉死成调用方给的整条可用
/// 宽度（图片列表预览横贯底边，和旧版稿 `.film{height:120px}` 一致），
/// 且不画浮卡投影/圆角——它是一条贴边实色栏，不是飘在照片上的卡片。
/// 内容（头行、曲线、缩略图、走带）两种形态完全共用，不新增任何控件。
///
/// Paints the whole card; `rect` is pre-sized from `card_height()` and `max_w` is half
/// the window width.
///
/// `docked = true` is the Legacy, edge-docked form: the width is pinned to the
/// caller's full available width so the image-thumbnail list spans the bottom
/// edge (matching the old mockup's `.film{height:120px}`), and the floating-card
/// shadow/radius are dropped because this is a docked bar, not a card over the
/// photo. Header, curve, thumbnails and transport are shared verbatim between
/// both forms -- no new widgets.
pub(super) fn card(state: &mut AppState, ui: &mut Ui, ctx: &Context, max_w: f32, docked: bool) {
    let w = if docked {
        ui.available_width().max(CARD_MIN_W)
    } else {
        card_width(state, ui, max_w)
    };
    let h = card_height(state);
    let rect = Rect::from_min_size(ui.min_rect().min, Vec2::new(w, h));
    ui.set_min_size(Vec2::new(w, h));
    ui.set_max_size(Vec2::new(w, h));

    let p = ui.painter().clone();
    if docked {
        // 贴边栏：实色底 + 上沿一道发丝线（旧版 `.film{border-top:1px}`）。
        //
        // Docked bar: solid fill plus a hairline on its top edge (the old
        // `.film{border-top:1px}`).
        p.rect_filled(rect, 0.0, panel());
        theme::rule(&p, rect.left(), rect.right(), rect.top(), border_strong());
    } else {
        // 深投影 + 1px 内环：设计稿 `--sh: 0 8px 24px rgba(0,0,0,.45), 0 0 0 1px #fff0f`。
        //
        // Deep shadow plus a 1 px inner ring, per the design's `--sh`.
        p.add(
            egui::epaint::Shadow {
                offset: [0, 8],
                blur: 24,
                spread: 0,
                color: Color32::from_black_alpha(115),
            }
            .as_shape(rect, 12.0),
        );
        p.rect_filled(rect, 12.0, panel());
        p.rect_stroke(
            rect,
            12.0,
            Stroke::new(1.0, group_ring()),
            StrokeKind::Inside,
        );
    }

    let inner = rect.shrink(PAD);
    let mut y = inner.top();

    // 卡片自己先吃掉指针：浮在画布上，空白处的拖动不能漏给下面的画布（否则
    // 会变成平移画面）。子控件注册在它之后，命中优先级更高，互不影响。
    //
    // The card swallows pointer input first: floating over the canvas, a drag on its
    // empty area must not fall through and pan the image. Children are registered after
    // it and therefore win the hit test.
    let _ = ui.interact(
        rect,
        ui.make_persistent_id("film-card-bg"),
        Sense::click_and_drag(),
    );

    header(
        state,
        ui,
        &p,
        Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), HEAD_H)),
    );
    y += HEAD_H;

    if state.show_curves {
        // 设计稿 `.curve{margin:0 2px 5px}`：曲线块比缩略图行左右各内缩 2px。
        //
        // The design inset the curve block 2 px on each side.
        curve_block(
            state,
            ui,
            Rect::from_min_size(
                Pos2::new(inner.left() + 2.0, y),
                Vec2::new(inner.width() - 4.0, CURVE_H),
            ),
        );
        y += CURVE_H + GAP;
    }

    if state.show_film {
        let sh = strip_h(state);
        filmstrip(
            state,
            ui,
            ctx,
            Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), sh)),
        );
        y += sh;
    }

    // 走带：上缘一条发丝线（设计 `.tp{box-shadow:0 -1px 0 var(--hair)}`）。
    //
    // Transport, with the hairline on its top edge.
    y += TP_MT;
    let tp = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), TP_H));
    theme::rule(
        &p,
        inner.left(),
        inner.right(),
        tp.top() - 0.5,
        Color32::from_white_alpha(20),
    );
    transport(state, ui, tp);
}

// ---------------------------------------------------------------- 头行

/// 头行：`胶片条 · N 帧` + 折叠钮 + 筛选/星级（右对齐）+ 曲线开关。
///
/// Header: title, collapse button, filter group (right aligned) and the curve toggle.
fn header(state: &mut AppState, ui: &mut Ui, p: &egui::Painter, hd: Rect) {
    // 设计稿 `.film-h` 有 5px 下内距，文字因此比几何中线高半格。
    let cy = hd.center().y - 2.0;
    let g = p.layout_no_wrap(title_text(state), font(SIZE_SMALL), dim());
    p.galley(
        Pos2::new(hd.left() + 2.0, cy - g.size().y * 0.5),
        g.clone(),
        dim(),
    );

    // 折叠钮：真按钮、22px 圆形图标钮、名字进 tooltip（功能与旧版一致）。
    //
    // The collapse control: a real 22 px round icon button whose name lives in the
    // tooltip -- same behaviour as before, just pinned right after the title.
    let btn = 22.0;
    let film_rect = Rect::from_center_size(
        Pos2::new(hd.left() + 2.0 + g.size().x + 8.0 + btn * 0.5, cy),
        Vec2::splat(btn),
    );
    let film_resp = ui.interact(film_rect, ui.make_persistent_id("tl-film"), Sense::click());
    let hovered = film_resp.hovered();
    if hovered {
        p.rect_filled(film_rect, btn * 0.5, elev_hover());
        p.rect_stroke(
            film_rect,
            btn * 0.5,
            Stroke::new(1.0, border_strong()),
            StrokeKind::Inside,
        );
    } else {
        p.circle_filled(film_rect.center(), btn * 0.5, panel().gamma_multiply(1.35));
    }
    icons::paint(
        p,
        Rect::from_center_size(film_rect.center(), Vec2::splat(13.0)),
        if state.show_film {
            Glyph::ChevronDown
        } else {
            Glyph::ChevronRight
        },
        if hovered { text() } else { muted() },
    );
    if film_resp.clicked() {
        state.show_film = !state.show_film;
    }
    film_resp.on_hover_text(crate::i18n::t!("收起 / 展开底部预览栏"));

    // 右端「曲线」开关：设计稿把它放在筛选组之后、头行最右。
    //
    // The curve toggle sits after the filter group, at the header's right end.
    let curve_rect = Rect::from_min_size(
        Pos2::new(hd.right() - CURVE_W, cy - 10.0),
        Vec2::new(CURVE_W, 20.0),
    );
    let curve_resp = ui.interact(
        curve_rect,
        ui.make_persistent_id("tl-curve"),
        Sense::click(),
    );
    if curve_resp.hovered() {
        p.rect_filled(curve_rect, radius_control(), elev());
    }
    p.text(
        curve_rect.center(),
        Align2::CENTER_CENTER,
        crate::i18n::tr("曲线"),
        font(SIZE_SMALL),
        if state.show_curves { text() } else { dim() },
    );
    if curve_resp.clicked() {
        state.show_curves = !state.show_curves;
    }

    // 筛选组右对齐；窄窗退成「已筛选 x / y · 清除筛选」的紧凑读数——筛选**正在
    // 生效**时不能整组消失，否则胶片条会莫名其妙空着而界面上没有原因。
    //
    // The filter group is right aligned; a narrow window falls back to the compact
    // "x of y / clear" readout, because an active filter must never vanish silently.
    let fw = filter_widths(state, p);
    let gx = curve_rect.left() - 8.0 - fw.total;
    if gx >= film_rect.right() + 8.0 {
        draw_filters(state, ui, p, gx, cy, &fw);
    } else if filters_on(state) {
        compact_filters(state, ui, p, curve_rect.left() - 8.0, cy, &fw);
    }
}

/// 完整筛选组：`筛选` + 五颗星级下限 + 两面旗 + 四个色标 + 清除 [+ 计数]。
///
/// The full filter group: label, five star levels, two flags, four colour labels,
/// clear, and the count when a filter is live.
fn draw_filters(
    state: &mut AppState,
    ui: &mut Ui,
    p: &egui::Painter,
    x0: f32,
    cy: f32,
    fw: &FilterW,
) {
    let mut x = x0;
    p.text(
        Pos2::new(x, cy),
        Align2::LEFT_CENTER,
        crate::i18n::tr("筛选"),
        font(SIZE_SMALL),
        muted(),
    );
    x += fw.label + FGAP;

    for level in 1..=5u8 {
        let r = Rect::from_center_size(Pos2::new(x + DOT * 0.5, cy), Vec2::splat(DOT));
        let resp = ui.interact(
            r,
            ui.make_persistent_id(("film-star-filter", level)),
            Sense::click(),
        );
        let on = state.film_filter_stars >= level;
        icons::paint(
            p,
            r.shrink(2.0),
            if on { Glyph::Star } else { Glyph::StarEmpty },
            if on {
                star()
            } else if resp.hovered() {
                text()
            } else {
                muted()
            },
        );
        if resp.clicked() {
            state.film_filter_stars = if state.film_filter_stars == level {
                0
            } else {
                level
            };
        }
        x += DOT;
    }
    x += FGAP;

    // 旗标：一枚勾（选用）一枚叉（排除），再点一次取消。勾/叉用线段画，
    // 不赌字体里有没有 ✓ / ✕。
    //
    // Flags: a tick (pick) and a cross (reject); clicking again clears. Both are line
    // segments so no font is trusted to carry the glyphs.
    for (flag, tip) in [(1i8, "只看选用"), (-1i8, "只看排除")] {
        let r = Rect::from_center_size(Pos2::new(x + DOT * 0.5, cy), Vec2::splat(DOT));
        let resp = ui.interact(
            r,
            ui.make_persistent_id(("film-flag-filter", flag)),
            Sense::click(),
        );
        let on = state.film_filter_flag == flag;
        let tint = if flag > 0 {
            label_color(3)
        } else {
            label_color(1)
        };
        if on {
            p.rect_filled(r, radius_control(), tint.gamma_multiply(0.22));
        }
        let col = if on {
            tint
        } else if resp.hovered() {
            text()
        } else {
            muted()
        };
        let c = r.center();
        if flag > 0 {
            p.add(egui::Shape::line(
                vec![
                    Pos2::new(c.x - 3.4, c.y + 0.2),
                    Pos2::new(c.x - 0.8, c.y + 2.8),
                    Pos2::new(c.x + 3.4, c.y - 2.8),
                ],
                Stroke::new(1.4, col),
            ));
        } else {
            for (a, b) in [
                (Vec2::new(-3.2, -3.2), Vec2::new(3.2, 3.2)),
                (Vec2::new(3.2, -3.2), Vec2::new(-3.2, 3.2)),
            ] {
                p.line_segment([c + a, c + b], Stroke::new(1.4, col));
            }
        }
        if resp.clicked() {
            state.film_filter_flag = if on { 0 } else { flag };
        }
        resp.on_hover_text(crate::i18n::tr(tip));
        x += DOT;
    }
    x += FGAP;

    // 色标：四颗圆点，选中的加一圈正文色描边（颜色本身不能当选中态，
    // 红点打不打标都是红的）。
    //
    // Colour labels: four dots; the selected one gets a body-colour ring, since the
    // dot's own colour cannot double as the selected state.
    for k in 1..=4u8 {
        let r = Rect::from_center_size(Pos2::new(x + DOT * 0.5, cy), Vec2::splat(DOT));
        let resp = ui.interact(
            r,
            ui.make_persistent_id(("film-label-filter", k)),
            Sense::click(),
        );
        let on = state.film_filter_label == k;
        p.circle_filled(
            r.center(),
            if resp.hovered() { 4.5 } else { 3.6 },
            label_color(k),
        );
        if on {
            p.circle_stroke(r.center(), 5.6, Stroke::new(1.0, text()));
        }
        if resp.clicked() {
            state.film_filter_label = if on { 0 } else { k };
        }
        resp.on_hover_text(crate::i18n::tr_fmt(
            "只看{}色标",
            &[&crate::i18n::tr(crate::state::LABEL_NAMES[k as usize])],
        ));
        x += DOT;
    }
    x += FGAP;

    let clear_rect = Rect::from_min_size(
        Pos2::new(x - 4.0, cy - 9.0),
        Vec2::new(fw.clear + 8.0, 18.0),
    );
    let clear_resp = ui.interact(
        clear_rect,
        ui.make_persistent_id("film-filter-clear"),
        Sense::click(),
    );
    p.text(
        Pos2::new(clear_rect.left() + 4.0, cy),
        Align2::LEFT_CENTER,
        crate::i18n::tr("清除筛选"),
        font(SIZE_SMALL),
        if clear_resp.hovered() {
            text()
        } else {
            muted()
        },
    );
    if clear_resp.clicked() {
        state.film_filter_stars = 0;
        state.film_filter_flag = 0;
        state.film_filter_label = 0;
    }

    if fw.count > 0.0 {
        p.text(
            Pos2::new(clear_rect.right() + FGAP, cy),
            Align2::LEFT_CENTER,
            crate::i18n::tr_fmt(
                "已筛选 {} / {} 帧",
                &[&state.film_visible_count(), &state.frame_count()],
            ),
            mono(MICRO),
            dim(),
        );
    }
}

/// 窄窗下的紧凑筛选读数：右对齐，只留「已筛选 x / y」与一枚可点的清除。
///
/// Compact filter readout for narrow cards: right aligned, just the count and a
/// clickable clear.
fn compact_filters(
    state: &mut AppState,
    ui: &mut Ui,
    p: &egui::Painter,
    right: f32,
    cy: f32,
    fw: &FilterW,
) {
    let clear_left = right - 4.0 - fw.clear;
    let count_left = clear_left - 8.0 - fw.count;
    if fw.count > 0.0 {
        p.text(
            Pos2::new(count_left, cy),
            Align2::LEFT_CENTER,
            crate::i18n::tr_fmt(
                "已筛选 {} / {} 帧",
                &[&state.film_visible_count(), &state.frame_count()],
            ),
            mono(MICRO),
            dim(),
        );
    }
    let clear_rect = Rect::from_min_size(
        Pos2::new(clear_left - 4.0, cy - 9.0),
        Vec2::new(fw.clear + 8.0, 18.0),
    );
    let clear_resp = ui.interact(
        clear_rect,
        ui.make_persistent_id("film-filter-clear-compact"),
        Sense::click(),
    );
    p.text(
        Pos2::new(clear_rect.left() + 4.0, cy),
        Align2::LEFT_CENTER,
        crate::i18n::tr("清除筛选"),
        font(SIZE_SMALL),
        if clear_resp.hovered() {
            text()
        } else {
            muted()
        },
    );
    if clear_resp.clicked() {
        state.film_filter_stars = 0;
        state.film_filter_flag = 0;
        state.film_filter_label = 0;
    }
}

// ---------------------------------------------------------------- 逐帧亮度曲线

/// 曲线块：标题行（逐帧亮度 / 去闪 EV · K 数）+ 折线图 + 刻度。
///
/// 行为与旧版一致：悬停读数、点击 / 拖动跳帧、关键帧菱形、当前帧竖线、两条折线
/// （灰=原片亮度，蓝=去闪修正后）。
///
/// The curve block: title row, plot and ticks. Behaviour is unchanged from the old
/// timeline -- hover readout, click/drag to seek, keyframe diamonds, playhead, and the
/// two polylines (grey raw, blue corrected).
fn curve_block(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    p.rect_filled(rect, 6.0, field());
    p.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, group_ring()),
        StrokeKind::Inside,
    );

    let head = Rect::from_min_size(
        Pos2::new(rect.left() + 7.0, rect.top() + 4.0),
        Vec2::new(rect.width() - 14.0, 10.0),
    );
    p.text(
        head.left_center(),
        Align2::LEFT_CENTER,
        crate::i18n::tr("逐帧亮度"),
        font(MICRO),
        dim(),
    );
    p.text(
        head.right_center(),
        Align2::RIGHT_CENTER,
        crate::i18n::tr_fmt(
            "去闪 {} EV · K {}",
            &[
                &format!("{:+.2}", state.offset_at(state.selected)),
                &state.keyframes.len(),
            ],
        ),
        mono(MICRO),
        dim(),
    );

    // 刻度：底部一条 3px 高的密排发丝线（设计 `.curve-ticks`）。
    //
    // Ticks: a 3 px band of hairlines along the bottom edge.
    let ticks_top = rect.bottom() - 5.0;
    let mut tx = rect.left() + 7.0;
    while tx < rect.right() - 7.0 {
        p.line_segment(
            [Pos2::new(tx, ticks_top), Pos2::new(tx, ticks_top + 3.0)],
            Stroke::new(1.0, Color32::from_white_alpha(14)),
        );
        tx += 8.0;
    }

    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + 7.0, head.bottom() + 3.0),
        Pos2::new(rect.right() - 28.0, ticks_top - 1.0),
    );
    let resp = ui.interact(
        inner.expand2(Vec2::new(0.0, 2.0)),
        ui.make_persistent_id("curves"),
        Sense::click_and_drag(),
    );

    let n = state.frame_count();
    if n == 0 {
        p.text(
            inner.center(),
            Align2::CENTER_CENTER,
            crate::i18n::tr("打开序列后这里显示逐帧亮度与去闪修正"),
            font(MICRO),
            dim(),
        );
        return;
    }

    for k in 1..4 {
        let y = inner.top() + inner.height() * k as f32 / 4.0;
        p.line_segment(
            [Pos2::new(inner.left(), y), Pos2::new(inner.right(), y)],
            Stroke::new(1.0, Color32::from_white_alpha(10)),
        );
    }

    // 两条折线的点集走缓存（E5）：键 = (corr_version, 帧数, inner rect)。
    // 去闪参数或测光变了 corr_version 必变；静止帧直接复用点集。
    let has_any = state.metrics.iter().any(|m| m.is_some());
    if !has_any {
        p.text(
            inner.center(),
            Align2::CENTER_CENTER,
            crate::i18n::tr("测光中…"),
            font(MICRO),
            accent(),
        );
        return;
    }
    let cached = match &state.defl_curve_cache {
        Some(c) if c.version == state.corr_version && c.n == n && c.rect == inner => {
            Some((c.pts_raw.clone(), c.pts_fix.clone(), c.lo, c.hi))
        }
        _ => None,
    };
    let (pts_raw, pts_fix, lo, hi) = match cached {
        Some(hit) => hit,
        None => {
            let raw: Vec<(usize, f64)> = state
                .metrics
                .iter()
                .enumerate()
                .filter_map(|(i, m)| m.map(|m| (i, m.mean * 255.0)))
                .collect();
            let corrected: Vec<(usize, f64)> = raw
                .iter()
                .map(|(i, v)| (i, v, state.offset_at(*i) as f64))
                .map(|(i, v, off)| (*i, v * 2f64.powf(off)))
                .collect();
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for (_, v) in raw.iter().chain(corrected.iter()) {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
            let pad_v = ((hi - lo) * 0.12).max(1.0);
            let (lo, hi) = (lo - pad_v, hi + pad_v);
            let x_at = |i: usize| {
                inner.left() + inner.width() * i as f32 / n.max(2).saturating_sub(1) as f32
            };
            let y_at =
                |v: f64| inner.bottom() - inner.height() * ((v - lo) / (hi - lo).max(1e-6)) as f32;

            let stride = (raw.len() / 800).max(1);
            let mut pts_raw = Vec::new();
            let mut pts_fix = Vec::new();
            for (k, (i, v)) in raw.iter().enumerate() {
                if k % stride == 0 || k + 1 == raw.len() {
                    pts_raw.push(Pos2::new(x_at(*i), y_at(*v)));
                }
            }
            for (k, (i, v)) in corrected.iter().enumerate() {
                if k % stride == 0 || k + 1 == corrected.len() {
                    pts_fix.push(Pos2::new(x_at(*i), y_at(*v)));
                }
            }
            let pts_raw: Arc<[Pos2]> = pts_raw.into_iter().collect();
            let pts_fix: Arc<[Pos2]> = pts_fix.into_iter().collect();
            state.defl_curve_cache = Some(DeflCurveCache {
                version: state.corr_version,
                n,
                rect: inner,
                pts_raw: Arc::clone(&pts_raw),
                pts_fix: Arc::clone(&pts_fix),
                lo,
                hi,
            });
            (pts_raw, pts_fix, lo, hi)
        }
    };
    let x_at =
        |i: usize| inner.left() + inner.width() * i as f32 / n.max(2).saturating_sub(1) as f32;
    if pts_raw.len() > 1 {
        p.add(egui::Shape::line(
            pts_raw.to_vec(),
            Stroke::new(1.0, Color32::from_rgb(0x7A, 0x7A, 0x7A)),
        ));
        p.add(egui::Shape::line(
            pts_fix.to_vec(),
            Stroke::new(1.2, accent()),
        ));
    }

    for i in state.keyframes.keys() {
        let c = Pos2::new(x_at(*i), inner.top() + 3.0);
        let d = 3.0;
        p.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(c.x, c.y - d),
                Pos2::new(c.x + d, c.y),
                Pos2::new(c.x, c.y + d),
                Pos2::new(c.x - d, c.y),
            ],
            star(),
            Stroke::NONE,
        ));
    }

    let px = x_at(state.selected);
    p.line_segment(
        [Pos2::new(px, inner.top()), Pos2::new(px, inner.bottom())],
        Stroke::new(1.0, text()),
    );

    p.text(
        Pos2::new(inner.right() + 4.0, inner.top()),
        Align2::LEFT_TOP,
        format!("{:.0}", hi),
        mono(MICRO),
        dim(),
    );
    p.text(
        Pos2::new(inner.right() + 4.0, inner.bottom()),
        Align2::LEFT_BOTTOM,
        format!("{:.0}", lo),
        mono(MICRO),
        dim(),
    );

    if let Some(pos) = resp.hover_pos().filter(|pos| inner.contains(*pos)) {
        let i = (((pos.x - inner.left()) / inner.width()) * (n.saturating_sub(1)) as f32)
            .round()
            .clamp(0.0, n.saturating_sub(1) as f32) as usize;
        let x = x_at(i);
        p.line_segment(
            [Pos2::new(x, inner.top()), Pos2::new(x, inner.bottom())],
            Stroke::new(1.0, Color32::from_white_alpha(70)),
        );
        let m = state.metrics.get(i).copied().flatten();
        let off = state.offset_at(i);
        let text = if let Some(m) = m {
            crate::i18n::tr_fmt(
                "帧 {} · {} · {} EV",
                &[
                    &format!("{:04}", i + 1),
                    &format!("{:.0}", m.mean * 255.0),
                    &format!("{off:+.2}"),
                ],
            )
        } else {
            crate::i18n::tr_fmt("帧 {} · 缺帧", &[&format!("{:04}", i + 1)])
        };
        let galley = p.layout_no_wrap(text.clone(), mono(MICRO), palette().text);
        let bg = Rect::from_min_size(
            Pos2::new(inner.left() + 2.0, rect.top() + 2.0),
            galley.size() + Vec2::new(10.0, 4.0),
        );
        p.rect_filled(bg, radius_control(), Color32::from_black_alpha(170));
        p.galley(bg.min + Vec2::new(5.0, 2.0), galley, palette().text);
        if resp.clicked() || resp.dragged() {
            state.set_frame(i);
        }
    }
}

pub(super) fn label_color(label: u8) -> Color32 {
    match label {
        1 => Color32::from_rgb(0xD9, 0x53, 0x4F),
        2 => Color32::from_rgb(0xD9, 0xBE, 0x6E),
        3 => Color32::from_rgb(0x7C, 0xB3, 0x42),
        4 => Color32::from_rgb(0x4A, 0x90, 0xD9),
        _ => Color32::TRANSPARENT,
    }
}

// ---------------------------------------------------------------- 缩略图行

/// 缩略图行：72×56、圆角 6、gap 4、选中 2px 强调环；放不下时横向滚动，
/// 绝不把卡片撑宽。
///
/// Thumbnail row: 72x56 cells, 6 px radius, 4 px gaps, a 2 px accent ring on the
/// selection. It scrolls horizontally instead of widening the card.
fn filmstrip(state: &mut AppState, ui: &mut Ui, ctx: &Context, rect: Rect) {
    let n = state.frame_count();
    let h = rect.height();
    let pitch = THUMB_W + THUMB_GAP;
    let mut visible = 0..0;

    ui.scope_builder(
        UiBuilder::new()
            .id_salt("film-scroll")
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Min)),
        |ui| {
            ScrollArea::horizontal()
                .id_salt("film")
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    // 左右各留 2px：选中环画在格子**外**（设计 `.th.on` 的
                    // `box-shadow:0 0 0 2px`），不留这点余量滚动区会把它裁掉。
                    //
                    // 2 px of air on both sides: the selection ring is painted outside
                    // the cell, so without it the scroll viewport clips the ring.
                    let (strip, _) = ui.allocate_exact_size(
                        Vec2::new(pitch * n.max(1) as f32 + 2.0, h),
                        Sense::hover(),
                    );
                    let x0 = strip.left() + 2.0;
                    let clip = ui.clip_rect();
                    let lo = (((clip.left() - x0) / pitch).floor() as isize).max(0) as usize;
                    let hi = ((((clip.right() - x0) / pitch).ceil() as usize) + 1).min(n);
                    // 虚拟化窗口退化时至少留一格：旧滚动偏移把视口推到内容之外、或窗口
                    // 极窄时，算出来的区间会是空集，整条胶片条就画成一片空白。
                    //
                    // Keep at least one cell when the virtualisation window degenerates.
                    let lo = lo.min(n.saturating_sub(1));
                    visible = lo..hi.max(lo + 1).min(n);

                    let p = ui.painter().clone();
                    for i in lo..hi {
                        if !state.film_visible(i) {
                            continue;
                        }
                        let cell = Rect::from_min_size(
                            Pos2::new(x0 + i as f32 * pitch, strip.top()),
                            Vec2::new(THUMB_W, h),
                        );
                        let cell_resp = ui.interact(
                            cell,
                            ui.make_persistent_id(("film-cell", i)),
                            Sense::click(),
                        );

                        // 底：画布色打底（缺帧时留一块空窗），圆角 6。
                        p.rect_filled(cell, 6.0, canvas());
                        if let Some(t) = state.thumb(i) {
                            let (tw, th) = (t.size[0] as f32, t.size[1] as f32);
                            // object-fit: cover —— 切片居中裁满整格，不拉变形。
                            //
                            // object-fit: cover -- centre-crop to fill the cell.
                            let uv = cover_uv(tw, th, cell.size());
                            p.add(egui::Shape::Rect(
                                egui::epaint::RectShape::filled(cell, 6.0, Color32::WHITE)
                                    .with_texture(t.texture.id(), uv),
                            ));
                        } else {
                            icons::paint(
                                &p,
                                Rect::from_center_size(cell.center(), Vec2::splat(16.0)),
                                Glyph::Sequence,
                                dim().gamma_multiply(0.7),
                            );
                        }

                        // 色标：底缘 3px 色条（缩略图上的色标读法，与帧表同形）。
                        let label = state.label_at(i);
                        if label > 0 {
                            p.rect_filled(
                                Rect::from_min_size(
                                    Pos2::new(cell.left() + 3.0, cell.bottom() - 3.0),
                                    Vec2::new(cell.width() - 6.0, 2.0),
                                ),
                                1.0,
                                label_color(label),
                            );
                        }

                        // 序号：左下角，白字 + 1px 黑投影（设计 `.th .no`）。
                        //
                        // Frame number bottom-left, white with a 1 px shadow.
                        let num = format!("{:04}", i + 1);
                        p.text(
                            Pos2::new(cell.left() + 5.0, cell.bottom() - 2.0),
                            Align2::LEFT_BOTTOM,
                            &num,
                            font(MICRO),
                            Color32::from_black_alpha(130),
                        );
                        p.text(
                            Pos2::new(cell.left() + 4.0, cell.bottom() - 3.0),
                            Align2::LEFT_BOTTOM,
                            &num,
                            font(MICRO),
                            Color32::from_white_alpha(235),
                        );

                        // 旗标：左上角一枚小图标，选用绿勾 / 排除红叉。
                        match state.flag_at(i) {
                            1 => icons::paint(
                                &p,
                                Rect::from_min_size(
                                    Pos2::new(cell.left() + 4.0, cell.top() + 4.0),
                                    Vec2::splat(10.0),
                                ),
                                Glyph::Flag,
                                ok(),
                            ),
                            -1 => icons::paint(
                                &p,
                                Rect::from_min_size(
                                    Pos2::new(cell.left() + 4.0, cell.top() + 4.0),
                                    Vec2::splat(10.0),
                                ),
                                Glyph::Close,
                                danger(),
                            ),
                            _ => {}
                        }

                        // 关键帧角标：右上角 13×13 琥珀 K（设计 `.kf`）。
                        //
                        // Keyframe badge: 13x13 amber K at the top-right (design `.kf`).
                        if state.keyframes.contains_key(&i) {
                            let chip = Rect::from_min_size(
                                Pos2::new(cell.right() - 17.0, cell.top() + 4.0),
                                Vec2::splat(13.0),
                            );
                            p.rect_filled(chip, 3.0, star());
                            p.text(
                                chip.center(),
                                Align2::CENTER_CENTER,
                                "K",
                                font(MICRO),
                                Color32::from_rgb(0x21, 0x1B, 0x0D),
                            );
                        }

                        if i == state.selected {
                            // 选中 = 2px 强调色外环（设计 `.th.on`）。
                            //
                            // Selected = the design's 2 px accent ring.
                            p.rect_stroke(
                                cell.expand(1.0),
                                7.0,
                                Stroke::new(2.0, accent()),
                                StrokeKind::Inside,
                            );
                        } else if cell_resp.hovered() {
                            p.rect_stroke(
                                cell,
                                6.0,
                                Stroke::new(1.0, border_strong()),
                                StrokeKind::Inside,
                            );
                        }

                        if cell_resp.clicked() {
                            state.set_frame(i);
                        }
                        cell_resp.context_menu(|ui| {
                            menu_style(ui);
                            ui.label(
                                RichText::new(crate::i18n::tr_fmt(
                                    "帧 {}",
                                    &[&format!("{:04}", i + 1)],
                                ))
                                .size(SIZE_SMALL)
                                .color(dim()),
                            );
                            for stars in 1..=5u8 {
                                if ui.button(crate::i18n::tr_fmt("{} 星", &[&stars])).clicked() {
                                    state.set_rating(i, stars);
                                    ui.close();
                                }
                            }
                            if ui.button(crate::i18n::t!("清除星级")).clicked() {
                                state.set_rating(i, 0);
                                ui.close();
                            }
                            ui.separator();
                            if ui.button(crate::i18n::t!("标记为选用")).clicked() {
                                state.set_flag(i, 1);
                                ui.close();
                            }
                            if ui.button(crate::i18n::t!("标记为排除")).clicked() {
                                state.set_flag(i, -1);
                                ui.close();
                            }
                            ui.separator();
                            for (label, name) in
                                [(1u8, "红标"), (2, "黄标"), (3, "绿标"), (4, "蓝标")]
                            {
                                if ui.button(name).clicked() {
                                    state.set_label(i, label);
                                    ui.close();
                                }
                            }
                            if ui.button(crate::i18n::t!("清除色标")).clicked() {
                                state.set_label(i, 0);
                                ui.close();
                            }
                        });
                    }

                    if state.scroll_to_selected {
                        let x = x0 + state.selected as f32 * pitch;
                        ui.scroll_to_rect(
                            Rect::from_min_size(Pos2::new(x, strip.top()), Vec2::new(THUMB_W, h)),
                            Some(Align::Center),
                        );
                        state.scroll_to_selected = false;
                    }
                });

            // 筛选把所有帧都藏掉时不能留一片空白：在条内居中写明原因，并给一枚
            // 一键清除。画在滚动区**外面**——滚动内容只有 n 格宽，落在内容布局之外
            // 的控件命中区会被 clip 裁空，点了没反应。
            //
            // When the filter hides every frame the strip must not sit blank: say why,
            // centred, with a one-click clear. Painted outside the ScrollArea so the
            // hit rect is not clipped away with the narrow content.
            if n > 0 && state.film_visible_count() == 0 {
                empty_hint(state, ui, rect);
            }
        },
    );

    state.ensure_thumbs(ctx, visible.start.saturating_sub(2), visible.end + 2);
}

/// `object-fit: cover` 的 UV：源比目标更宽就裁左右，更高就裁上下。
///
/// UV rect implementing `object-fit: cover`.
fn cover_uv(tw: f32, th: f32, dst: Vec2) -> Rect {
    let target = dst.x / dst.y.max(1.0);
    let src = tw / th.max(1.0);
    if src > target {
        let f = target / src;
        Rect::from_min_max(
            Pos2::new((1.0 - f) * 0.5, 0.0),
            Pos2::new((1.0 + f) * 0.5, 1.0),
        )
    } else {
        let f = src / target;
        Rect::from_min_max(
            Pos2::new(0.0, (1.0 - f) * 0.5),
            Pos2::new(1.0, (1.0 + f) * 0.5),
        )
    }
}

/// 空条提示：原因 + 一键清除。必须在滚动区外调用，理由见调用处。
fn empty_hint(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    let hint = crate::i18n::tr("当前筛选下没有帧");
    let hint_g = p.layout_no_wrap(hint.to_owned(), font(SIZE_SMALL), dim());
    let clear_g = p.layout_no_wrap(
        crate::i18n::tr("清除筛选").to_owned(),
        font(SIZE_SMALL),
        muted(),
    );
    let cy = rect.center().y;
    // 原因 + 清除作为一个整体水平居中，读起来是一句话加一个动作。
    //
    // Reason and clear are centred as one group: a sentence plus one action.
    let gap = 10.0;
    let total = hint_g.size().x + gap + clear_g.size().x + 16.0;
    let left = rect.center().x - total * 0.5;
    p.galley(
        Pos2::new(left, cy - hint_g.size().y * 0.5),
        hint_g.clone(),
        dim(),
    );
    let cr = Rect::from_min_size(
        Pos2::new(left + hint_g.size().x + gap, cy - 10.0),
        Vec2::new(clear_g.size().x + 16.0, 20.0),
    );
    let cresp = ui.interact(
        cr,
        ui.make_persistent_id("film-empty-clear"),
        Sense::click(),
    );
    if cresp.hovered() {
        p.rect_filled(cr, radius_control(), elev());
    }
    p.galley(
        Pos2::new(
            cr.center().x - clear_g.size().x * 0.5,
            cr.center().y - clear_g.size().y * 0.5,
        ),
        clear_g,
        if cresp.hovered() { text() } else { muted() },
    );
    if cresp.clicked() {
        state.film_filter_stars = 0;
        state.film_filter_flag = 0;
        state.film_filter_label = 0;
        state.scroll_to_selected = true;
    }
}

// ---------------------------------------------------------------- 走带

/// 走带行：◀ ▶ 上一帧 / 下一帧、`0001 / 0004`、`−0.01 EV`、本帧星级、fps。
///
/// 播放 / 暂停、帧号与 EV 读数沿用旧版行为；右端读数额满则逐格让位。
///
/// Transport: prev/next, play/pause, frame counter, EV readout, the selected frame's
/// stars and fps. Play/pause, the counter and the EV readout keep the old behaviour;
/// right-hand readouts yield one by one when the row runs out of room.
fn transport(state: &mut AppState, ui: &mut Ui, rect: Rect) {
    let p = ui.painter().clone();
    let cy = rect.center().y;
    let mut idx = 0usize;
    // `enabled` 为假时只挂 hover 感：不可点、图标压到 dim、并说明为什么。
    // 播放按钮在单帧序列上就是这种状态——点了画面不动，用户会以为坏了。
    //
    // With `enabled` false the control only senses hover: not clickable, icon dimmed,
    // with a hint. That is the play button on a one-frame sequence.
    //
    // `hint` 是键盘提示（设计稿 title 的口径）：←/→ 翻帧、空格播放暂停，
    // 两者都是真实接线（menu.rs 的全局键处理），不是装饰文案。
    //
    // `hint` is the keyboard hint (the design's title wording): arrows step frames,
    // space toggles playback -- both really wired (menu.rs's global key handling),
    // not decorative copy.
    let mut mini = |ui: &mut Ui,
                    glyph: Glyph,
                    active: bool,
                    tint: Color32,
                    enabled: bool,
                    hint: &str|
     -> bool {
        // 走带按钮 22×22（设计 `.tp .ibtn`），按 26px 一格排。
        //
        // Transport buttons are 22x22 per the design's `.tp .ibtn`, on a 26 px pitch.
        let r = Rect::from_center_size(
            Pos2::new(rect.left() + 11.0 + idx as f32 * 26.0, cy),
            Vec2::splat(22.0),
        );
        let resp = ui.interact(
            r,
            ui.make_persistent_id(("transport", idx)),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        idx += 1;
        if enabled && resp.hovered() {
            p.rect_filled(r, radius_control(), elev());
        }
        icons::paint(
            &p,
            r.shrink(5.0),
            glyph,
            if !enabled {
                dim()
            } else if active {
                tint
            } else if resp.hovered() {
                text()
            } else {
                muted()
            },
        );
        let clicked = resp.clicked();
        // hint 是中文字面量，这里统一过 tr()：英文模式才有译文。
        if !enabled {
            resp.on_hover_text(crate::i18n::t!("至少需要两帧才能播放"));
        } else {
            resp.on_hover_text(crate::i18n::tr(hint));
        }
        clicked && enabled
    };

    let has = state.frame_count() > 0;
    // 播放至少要两帧：一帧的「播放」既不动、又会把画面切成代理，纯属误导。
    //
    // Playback needs at least two frames: "playing" a single frame neither moves nor
    // means anything.
    let can_play = state.frame_count() >= 2;
    if mini(
        ui,
        Glyph::Prev,
        false,
        text(),
        has,
        "上一帧（←；Shift 一次十帧）",
    ) {
        state.step(-1);
    }
    if mini(
        ui,
        if state.playing {
            Glyph::Pause
        } else {
            Glyph::Play
        },
        state.playing,
        accent(),
        can_play,
        "播放 / 暂停（空格）",
    ) {
        state.toggle_play();
    }
    if mini(
        ui,
        Glyph::Next,
        false,
        text(),
        has,
        "下一帧（→；Shift 一次十帧）",
    ) {
        state.step(1);
    }

    let n = state.frame_count();
    let mut x = rect.left() + 11.0 + idx as f32 * 26.0;
    // 帧号：`0001 / 0004`（设计 `.tp .display`，等宽数字）。
    let frame_text = if n > 0 {
        format!("{:04} / {:04}", state.selected + 1, n)
    } else {
        "— / —".to_owned()
    };
    let frame_g = p.layout_no_wrap(
        frame_text,
        mono(SIZE_SMALL),
        if n > 0 { text() } else { dim() },
    );
    p.galley(
        Pos2::new(x, cy - frame_g.size().y * 0.5),
        frame_g.clone(),
        text(),
    );
    x += frame_g.size().x + 8.0;

    // 记录关键帧（设计 `.tp .record`）：◇ 记录，贴在帧号右侧。当前帧已有关键帧
    // 时整颗转琥珀底、◇ 升 ◆（`.record.recorded`）——「这帧记过了」在走路带时
    // 一眼可见，不用翻关键帧页。点击走 add_keyframe 同一条路径：同帧再点即覆写。
    //
    // Record keyframe (the design's `.tp .record`): ◇ + label, right of the counter.
    // When the current frame already carries a keyframe the chip turns amber and the
    // glyph rises to ◆ (`.record.recorded`) -- "this frame is already recorded" is
    // visible while scrubbing, no need to open the Keys tab. Clicking shares the
    // add_keyframe path: clicking again on the same frame overwrites.
    if n > 0 {
        const REC_AMBER: Color32 = Color32::from_rgb(0xF5, 0xB8, 0x3D); // 设计 --amber
        let recorded = state.keyframes.contains_key(&state.selected);
        let rec_lab = crate::i18n::t!("记录").to_owned();
        let rec_g = p.layout_no_wrap(rec_lab, font(MICRO), Color32::WHITE);
        let rec_w = 12.0 + 4.0 + rec_g.size().x + 10.0;
        let rr = Rect::from_min_size(Pos2::new(x, cy - 10.0), Vec2::new(rec_w, 20.0));
        let rresp = ui.interact(
            rr,
            ui.make_persistent_id("transport-record"),
            Sense::click(),
        );
        if rresp.hovered() {
            p.rect_filled(rr, 4.0, elev());
        }
        if recorded {
            // 12% 琥珀底（设计 rgba(245,184,61,.12)），label 与 glyph 同升琥珀。
            p.rect_filled(
                rr,
                4.0,
                Color32::from_rgba_unmultiplied(0xF5, 0xB8, 0x3D, 31),
            );
        }
        icons::paint(
            &p,
            Rect::from_center_size(Pos2::new(rr.left() + 7.0, cy), Vec2::splat(11.0)),
            if recorded {
                Glyph::KeyframeSet
            } else {
                Glyph::Keyframe
            },
            REC_AMBER,
        );
        p.galley(
            Pos2::new(rr.left() + 14.0, cy - rec_g.size().y * 0.5),
            rec_g.clone(),
            if recorded { REC_AMBER } else { muted() },
        );
        if rresp.clicked() {
            state.add_keyframe();
        }
        rresp.on_hover_text(if recorded {
            crate::i18n::t!("这一帧已有关键帧，再点以当前参数覆写")
        } else {
            crate::i18n::t!("把当前帧的全部显影参数记录为关键帧")
        });
        x += rec_w + 8.0;
    }

    // 去闪读数：`去闪 −0.01 EV`，有实际修正时抬成强调色。
    //
    // Deflicker readout; it lifts to accent when there is an actual correction.
    let off = state.offset_at(state.selected);
    let ev_text = crate::i18n::tr_fmt("去闪 {} EV", &[&format!("{off:+.2}")]);
    let ev_color = if off.abs() > 0.02 { accent() } else { dim() };
    let ev_g = p.layout_no_wrap(ev_text, mono(SIZE_SMALL), ev_color);
    p.galley(
        Pos2::new(x, cy - ev_g.size().y * 0.5),
        ev_g.clone(),
        ev_color,
    );
    x += ev_g.size().x + 8.0;

    // 本帧星级：与上方「星级筛选」是两件事——这里改的是当前帧的评级。
    //
    // This frame's rating: distinct from the header's star filter -- this writes the
    // rating of the selected frame.
    let rating = state.selected_rating();
    let cap = p.layout_no_wrap(crate::i18n::t!("本帧").to_owned(), font(SIZE_SMALL), dim());
    let star_w = 13.0;
    let stars_w = 5.0 * star_w;
    let fps_g = p.layout_no_wrap(format!("{:.0} fps", state.output_fps), mono(MICRO), muted());
    // 先给右端 fps 留出位置；「本帧」这枚说明字在窄卡里是唯一能整段让掉的东西
    // （英文 "This frame" 比中文长一倍），星级本身是控件，不许让。
    //
    // Reserve room for the fps readout first. The "this frame" caption is the only
    // thing that may drop whole in a narrow card (English is twice as wide as Chinese);
    // the stars themselves are a control and never yield.
    let cap_ok = x + cap.size().x + 4.0 + stars_w <= rect.right() - fps_g.size().x - 12.0;
    if cap_ok {
        p.galley(Pos2::new(x, cy - cap.size().y * 0.5), cap.clone(), dim());
        x += cap.size().x + 4.0;
    }
    let stars_left = x;
    for s in 1..=5u8 {
        let r = Rect::from_center_size(
            Pos2::new(stars_left + (s as f32 - 0.5) * star_w, cy),
            Vec2::new(star_w, 20.0),
        );
        let resp = ui.interact(r, ui.make_persistent_id(("rate", s)), Sense::click());
        if resp.hovered() {
            p.rect_filled(r, 3.0, elev());
        }
        icons::paint(
            &p,
            r.shrink2(Vec2::new(1.0, 4.0)),
            if s <= rating {
                Glyph::Star
            } else {
                Glyph::StarEmpty
            },
            if s <= rating {
                star()
            } else if resp.hovered() {
                text()
            } else {
                dim()
            },
        );
        if resp.clicked() {
            state.rate_selected(if rating == s { 0 } else { s });
        }
    }
    x = stars_left + stars_w;

    // 右端两格从右往左排，空间不足逐格让位：先保 fps（数值常看），再保已评级。
    //
    // The two right-side readouts chain from the right edge and yield one by one when
    // space runs out: fps yields last, both are readouts rather than entrances.
    let rated_text = crate::i18n::tr_fmt("已评级 {}", &[&state.rated_count()]);
    let rated_g = p.layout_no_wrap(rated_text, font(MICRO), dim());
    let fps_left = rect.right() - fps_g.size().x;
    if fps_left > x + 8.0 {
        p.galley(
            Pos2::new(fps_left, cy - fps_g.size().y * 0.5),
            fps_g,
            muted(),
        );
        let rated_left = fps_left - 10.0 - rated_g.size().x;
        if rated_left > x + 8.0 {
            p.galley(
                Pos2::new(rated_left, cy - rated_g.size().y * 0.5),
                rated_g,
                dim(),
            );
        }
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use egui::Context;

    fn input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            max_texture_side: Some(8192),
            ..Default::default()
        }
    }

    /// 记录钮的琥珀底只在当前帧已有关键帧时出现（设计 `.record.recorded` 的
    /// rgba(245,184,61,.12)）：没记录就不该亮，亮了就必须是记录过的帧——
    /// 「这帧记过了」是走带时扫一眼的判据，亮错比不亮更害人。
    ///
    /// The record chip's amber fill appears only when the selected frame carries a
    /// keyframe (the design's `.record.recorded`): no record, no glow -- and the glow
    /// must never light up on an unrecorded frame, which would be worse than never
    /// lighting at all.
    #[test]
    fn record_chip_lights_up_only_when_the_frame_is_recorded() {
        let ctx = Context::default();
        crate::theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        assert!(state.frame_count() > 0, "demo 状态应有帧可走带");
        let rect = Rect::from_min_size(Pos2::new(20.0, 20.0), Vec2::new(680.0, 26.0));
        let amber_rect = |out: &egui::FullOutput| {
            out.shapes.iter().any(|c| {
                matches!(
                    &c.shape,
                    egui::Shape::Rect(rs) if rs.fill
                        == Color32::from_rgba_unmultiplied(0xF5, 0xB8, 0x3D, 31)
                )
            })
        };
        let mut out = ctx.run_ui(input(), |ui| transport(&mut state, ui, rect));
        assert!(!amber_rect(&out), "没记录关键帧，记录钮不该亮琥珀底");
        out.textures_delta.clear();
        state.add_keyframe();
        let mut out = ctx.run_ui(input(), |ui| transport(&mut state, ui, rect));
        assert!(amber_rect(&out), "已记录关键帧，记录钮没有琥珀底");
        out.textures_delta.clear();
    }
}
