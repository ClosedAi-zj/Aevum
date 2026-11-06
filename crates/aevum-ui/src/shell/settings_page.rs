//! 设置页：外观、密度、语言、快捷键、更新——从浮窗升为独立页。
//!
//! 为什么是页而不是浮窗：浮窗 340px 宽装不下四组设置，滚动条一出来就得在
//! 「看得见的字段」和「滚得到的分组」之间反复横跳。Lightroom 的首选项是独立
//! 对话框，本页对齐那个信息量，但沿用应用内的中性灰与密度刻度。
//!
//! Settings page: appearance, density, language, shortcuts and updates, promoted from a
//! floating window to a page. Why a page: the 340 px window cannot hold four groups, and
//! the scrollbar forces jumping between visible fields and reachable groups. Lightroom's
//! preferences is a dialog of this size; this page matches that information load while
//! keeping the app's own neutral greys and density steps.

use super::*;
use crate::state::AppState;
use egui::{
    Align, Align2, Layout, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2,
};

const PAGE_PAD_X: f32 = 32.0;
const PAGE_PAD_Y: f32 = 24.0;
/// 设置卡片的列宽：340 起，最多两列。设计稿 `.settings-grid` 用
/// `minmax(340px,1fr)`，同一口径。
///
/// Card column width: 340 minimum, at most two columns -- the design's
/// `.settings-grid` uses `minmax(340px,1fr)`, the same rule.
const CARD_MIN_W: f32 = 340.0;
const CARD_GAP: f32 = 20.0;
/// egui 的竖直滚动条宽：内容宽度必须扣掉它，否则右列会压在滚动条外面。
///
/// egui's vertical scrollbar width: the content width must subtract it, or the right
/// column ends up underneath the scrollbar.
const SCROLLBAR_W: f32 = 8.0;

pub(super) fn settings_page(state: &mut AppState, ui: &mut Ui) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.rect_filled(full, 0.0, surround());

    let content = Rect::from_min_max(
        Pos2::new(full.left() + PAGE_PAD_X, full.top() + PAGE_PAD_Y),
        Pos2::new(
            full.right() - PAGE_PAD_X - SCROLLBAR_W,
            full.bottom() - 12.0,
        ),
    );

    let mut y = content.top();
    let title = crate::i18n::t!("设置");
    let tg = p.layout_no_wrap(title.to_owned(), bold(SIZE_HEADING), text());
    p.galley(Pos2::new(content.left(), y), tg.clone(), text());
    y += tg.size().y + 6.0;

    let desc = crate::i18n::t!("外观、密度、语言与更新。改动即时生效，落盘在 settings.json。");
    let dg = p.layout_no_wrap(desc.to_owned(), font(SIZE_LABEL), dim());
    p.galley(Pos2::new(content.left(), y), dg.clone(), dim());
    y += dg.size().y + 20.0;

    // 两列：宽度按**实际可用**算（已扣滚动条），列高由卡片返回的真实高度累加。
    // 早先按 `area.width()` 不扣滚动条，右列卡片整整压到滚动条外面，快捷键列
    // 被裁成「Ctrl-」——那是算错宽度，不是内容太长。
    //
    // Two columns: the width comes from the **actual** remainder (scrollbar already
    // subtracted) and each column accumulates the heights the cards report. The first
    // cut used `area.width()` without subtracting the scrollbar, so the right column ran
    // past it and the shortcut column got clipped to "Ctrl-".
    let cols = if content.width() >= CARD_MIN_W * 2.0 + CARD_GAP {
        2
    } else {
        1
    };
    let card_w = if cols == 2 {
        (content.width() - CARD_GAP) / 2.0
    } else {
        content.width()
    };

    let body = Rect::from_min_max(
        Pos2::new(content.left(), y),
        Pos2::new(content.right(), content.bottom()),
    );
    let mut cur = [body.top(), body.top()];

    // 左列：外观 → 语言
    let h = appearance_card(
        state,
        ui,
        Rect::from_min_size(Pos2::new(body.left(), cur[0]), Vec2::new(card_w, 0.0)),
    );
    cur[0] += h + CARD_GAP;
    let h = language_card(
        state,
        ui,
        Rect::from_min_size(Pos2::new(body.left(), cur[0]), Vec2::new(card_w, 0.0)),
    );
    cur[0] += h + CARD_GAP;

    // 右列：密度 → 快捷键 → 更新（单列时全部落到左列）
    let right_x = body.left() + card_w + CARD_GAP;
    let (rx, col_idx) = if cols == 2 {
        (right_x, 1usize)
    } else {
        (body.left(), 0usize)
    };
    let h = density_card(
        state,
        ui,
        Rect::from_min_size(Pos2::new(rx, cur[col_idx]), Vec2::new(card_w, 0.0)),
    );
    cur[col_idx] += h + CARD_GAP;
    let h = shortcut_card(
        ui,
        Rect::from_min_size(Pos2::new(rx, cur[col_idx]), Vec2::new(card_w, 0.0)),
    );
    cur[col_idx] += h + CARD_GAP;
    let h = update_card(
        state,
        ui,
        Rect::from_min_size(Pos2::new(rx, cur[col_idx]), Vec2::new(card_w, 0.0)),
    );
    cur[col_idx] += h;

    // 单列时右列内容跟着左列走
    if cols == 1 {
        cur[0] = cur[0].max(cur[col_idx]);
    }

    // 告诉 egui 这片内容有多高：绝对坐标画的东西它看不见，必须显式占位，
    // 否则滚动条按 0 高度算、内容被裁。
    //
    // Declare the extent: absolutely painted content is invisible to egui's layout, so
    // the space must be reserved explicitly or the scrollbar measures zero.
    let used = cur[0].max(cur[1]) - body.top();
    ui.scope_builder(
        UiBuilder::new().id_salt("set-scroll").max_rect(body),
        |ui| {
            ScrollArea::vertical()
                .id_salt("settings-page")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.allocate_space(Vec2::new(card_w, used));
                });
        },
    );
}

/// 卡片外壳：panel 底 + 1px 描边 + 18px 内边距，返回内容区。
///
/// Card shell: `panel` fill, 1px border, 18 px inset; returns the content rect.
fn card_frame(ui: &mut Ui, top_left: Pos2, width: f32, height: f32, title: &str) -> Rect {
    let p = ui.painter().clone();
    let rect = Rect::from_min_size(top_left, Vec2::new(width, height));
    p.rect_filled(rect, radius_panel(), panel());
    p.rect_stroke(
        rect,
        radius_panel(),
        Stroke::new(1.0, border()),
        StrokeKind::Inside,
    );
    let head = Rect::from_min_size(
        Pos2::new(rect.left() + 18.0, rect.top() + 16.0),
        Vec2::new(rect.width() - 36.0, 22.0),
    );
    let g = p.layout_no_wrap(title.to_owned(), bold(SIZE_BODY), text());
    p.galley(head.min, g, text());
    Rect::from_min_size(
        Pos2::new(rect.left() + 18.0, head.top() + 30.0),
        Vec2::new(
            rect.width() - 36.0,
            rect.bottom() - head.top() - 30.0 - 16.0,
        ),
    )
}

/// 外观卡：主题三档 + 皮肤 + 强调色 + 明暗微调 + 圆角。
///
/// Appearance card: theme step, skin, accent, brightness trim and radius.
fn appearance_card(state: &mut AppState, ui: &mut Ui, at: Rect) -> f32 {
    let width = at.width();
    let mut changed = false;

    // 行 1：主题三档（分段控件）
    let theme_h = 30.0;
    let skin_h = 0.0;
    let accent_h = 0.0;
    let offset_h = 0.0;
    let radius_h = 0.0;

    // 预演高度：主题 30 + 主题标签 22 + 间距……
    // 卡片总高 = 头 46 + 各组（标签 20 + 控件 30 + 间距 12）* 5 + 尾 16
    let total = 46.0
        + (20.0 + theme_h + 12.0)
        + (20.0 + 26.0 + 12.0)
        + (20.0 + 26.0 + 12.0)
        + (20.0 + 26.0 + 12.0)
        + (20.0 + 26.0 + 12.0)
        + 16.0;

    let content = card_frame(ui, at.min, width, total, crate::i18n::t!("外观"));
    let _ = (skin_h, accent_h, offset_h, radius_h);
    let mut y = content.top();

    // 主题
    label_at(ui, content.left(), y, crate::i18n::t!("界面主题"));
    y += 20.0;
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-theme")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y),
                Vec2::new(content.width(), 30.0),
            )),
        |ui| {
            ui.horizontal(|ui| {
                for mode in [
                    crate::theme::ThemeMode::Dark,
                    crate::theme::ThemeMode::Black,
                    crate::theme::ThemeMode::Light,
                ] {
                    if seg_chip(
                        ui,
                        crate::i18n::tr(mode.cn()),
                        state.appearance.theme == mode,
                    ) {
                        state.appearance.theme = mode;
                        changed = true;
                    }
                }
            });
        },
    );
    y += 30.0 + 12.0;

    // 皮肤
    label_at(ui, content.left(), y, crate::i18n::t!("皮肤"));
    y += 20.0;
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-skin")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y),
                Vec2::new(content.width(), 26.0),
            )),
        |ui| {
            ui.horizontal(|ui| {
                for skin in [
                    crate::theme::Skin::Graphite,
                    crate::theme::Skin::Steel,
                    crate::theme::Skin::Sand,
                    crate::theme::Skin::Jade,
                ] {
                    if seg_chip(
                        ui,
                        crate::i18n::tr(skin.cn()),
                        state.appearance.skin == skin,
                    ) {
                        state.appearance.skin = skin;
                        changed = true;
                    }
                }
            });
        },
    );
    y += 26.0 + 12.0;

    // 强调色：色相/饱和度/亮度三推子（与显影页同一套 slider_row 口径）
    label_at(ui, content.left(), y, crate::i18n::t!("强调色"));
    y += 20.0;
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-accent")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y),
                Vec2::new(content.width(), 26.0),
            )),
        |ui| {
            ui.horizontal(|ui| {
                let mut h = state.appearance.accent_h as f64;
                let mut s = state.appearance.accent_s as f64;
                let mut l = state.appearance.accent_l as f64;
                changed |= compact_value(ui, "H", &mut h, 0.0, 360.0, 1.0);
                changed |= compact_value(ui, "S", &mut s, 0.0, 100.0, 1.0);
                changed |= compact_value(ui, "L", &mut l, 0.0, 100.0, 0.5);
                state.appearance.accent_h = h as f32;
                state.appearance.accent_s = s as f32;
                state.appearance.accent_l = l as f32;
                // 色块预览：唯一允许出现彩色的地方（强调色本身就是它）
                let sw = Rect::from_min_size(
                    Pos2::new(ui.cursor().left() + 6.0, ui.cursor().top() + 4.0),
                    Vec2::new(22.0, 18.0),
                );
                ui.painter().rect_filled(sw, radius_control(), accent());
                ui.painter().rect_stroke(
                    sw,
                    radius_control(),
                    Stroke::new(1.0, border_strong()),
                    StrokeKind::Inside,
                );
            });
        },
    );
    y += 26.0 + 12.0;

    // 明暗微调
    label_at(ui, content.left(), y, crate::i18n::t!("界面明暗微调"));
    y += 20.0;
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-offset")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y),
                Vec2::new(content.width(), 26.0),
            )),
        |ui| {
            ui.horizontal(|ui| {
                let mut off = state.appearance.gray_offset as f64;
                if compact_value(ui, "", &mut off, -3.0, 3.0, 1.0) {
                    state.appearance.gray_offset = off as i8;
                    changed = true;
                }
                if ui.small_button(crate::i18n::t!("归零")).clicked() {
                    state.appearance.gray_offset = 0;
                    changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr_fmt(
                        "{} / ±3 档",
                        &[&(state.appearance.gray_offset as i32)],
                    ))
                    .size(SIZE_SMALL)
                    .color(dim()),
                );
            });
        },
    );
    y += 26.0 + 12.0;

    // 圆角
    label_at(ui, content.left(), y, crate::i18n::t!("控件圆角"));
    y += 20.0;
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-radius")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y),
                Vec2::new(content.width(), 26.0),
            )),
        |ui| {
            ui.horizontal(|ui| {
                let mut r = state.appearance.radius as f64;
                if compact_value(ui, "px", &mut r, 0.0, 12.0, 0.5) {
                    state.appearance.radius = r as f32;
                    changed = true;
                }
            });
        },
    );

    if changed {
        state.settings_dirty = true;
        ui.ctx().request_repaint();
    }
    total
}

/// 密度卡：三档 + 真实行高演示（切「宽松」时推子行高真的变）。
///
/// Density card: three steps plus a live row-height demo (switching to roomy really
/// changes the fader row height).
fn density_card(state: &mut AppState, ui: &mut Ui, at: Rect) -> f32 {
    let width = at.width();
    // 演示区高度 = 三行真实行高 + 上下 12px 内边距：切到「宽松」时行高变高，
    // 卡片跟着长高，而不是把行挤出边框（早先写死 120，宽松档三行会溢出）。
    //
    // Demo height = three real rows plus 12 px of vertical padding: switching to roomy
    // makes the rows taller and the card grows with them instead of spilling outside.
    let demo_h = row_h() * 3.0 + 16.0;
    let total = 46.0 + 20.0 + 30.0 + 12.0 + demo_h + 16.0;
    let content = card_frame(ui, at.min, width, total, crate::i18n::t!("密度"));
    let mut y = content.top();
    let mut changed = false;

    label_at(ui, content.left(), y, crate::i18n::t!("参数行高"));
    y += 20.0;
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-density")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y),
                Vec2::new(content.width(), 30.0),
            )),
        |ui| {
            ui.horizontal(|ui| {
                for d in [Density::Compact, Density::Standard, Density::Loose] {
                    if seg_chip(ui, crate::i18n::tr(d.cn()), state.appearance.density == d) {
                        state.appearance.density = d;
                        changed = true;
                    }
                }
            });
        },
    );
    y += 30.0 + 12.0;

    // 真实演示：三行假推子，行高取当前档位（这就是「切宽松真的变高」的证据）。
    //
    // Live demo: three mock faders whose height comes from the current step -- the
    // evidence that switching to roomy really does make them taller.
    let demo = Rect::from_min_size(
        Pos2::new(content.left(), y),
        Vec2::new(content.width(), demo_h),
    );
    let p = ui.painter().clone();
    p.rect_filled(demo, radius_control(), field());
    let row_h_now = row_h();
    let mut ry = demo.top() + 6.0;
    for (i, label) in ["曝光", "对比度", "高光"].iter().enumerate() {
        let row = Rect::from_min_size(
            Pos2::new(demo.left() + 10.0, ry),
            Vec2::new(demo.width() - 20.0, row_h_now),
        );
        if row.bottom() > demo.bottom() - 2.0 {
            break;
        }
        p.text(
            Pos2::new(row.left(), row.center().y),
            Align2::LEFT_CENTER,
            crate::i18n::tr(label),
            font(SIZE_LABEL),
            muted(),
        );
        let rail = Rect::from_min_max(
            Pos2::new(row.left() + 76.0, row.center().y - 1.5),
            Pos2::new(row.right() - 52.0, row.center().y + 1.5),
        );
        p.rect_filled(rail, 1.5, track());
        let vx = rail.left() + rail.width() * (0.3 + 0.2 * i as f32);
        p.rect_filled(
            Rect::from_min_max(rail.left_top(), Pos2::new(vx, rail.bottom())),
            1.5,
            elev_hover(),
        );
        p.text(
            Pos2::new(row.right(), row.center().y),
            Align2::RIGHT_CENTER,
            "0",
            mono(SIZE_LABEL),
            value(),
        );
        ry += row_h_now;
    }

    if changed {
        state.settings_dirty = true;
        ui.ctx().request_repaint();
    }
    total
}

/// 语言卡：跟随系统 + 各语言自称（不走 tr()）。
///
/// Language card: follow-system plus each language's endonym (never through `tr()`).
fn language_card(state: &mut AppState, ui: &mut Ui, at: Rect) -> f32 {
    let width = at.width();
    let total = 46.0 + 20.0 + 30.0 + 12.0 + 40.0 + 16.0;
    let content = card_frame(ui, at.min, width, total, crate::i18n::t!("语言 / Language"));
    let mut y = content.top();
    let mut changed = false;

    label_at(ui, content.left(), y, crate::i18n::t!("界面语言"));
    y += 20.0;
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-lang")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y),
                Vec2::new(content.width(), 30.0),
            )),
        |ui| {
            ui.horizontal(|ui| {
                let system_label = crate::i18n::t!("跟随系统");
                for p in crate::i18n::LangPref::all() {
                    // 「跟随系统」走翻译；其余语言用自己的自称——`label()` 本就
                    // 是那门语言自己的写法，再翻一次会把「中文」译成 "Chinese"。
                    //
                    // "Follow system" is translated; the rest keep their endonym --
                    // `label()` already is that language's own spelling, and translating
                    // it again would render 中文 as "Chinese".
                    let label = match p {
                        crate::i18n::LangPref::System => system_label.to_owned(),
                        crate::i18n::LangPref::Fixed(l) => l.label().to_owned(),
                    };
                    if seg_chip(ui, &label, state.lang == p) {
                        state.lang = p;
                        changed = true;
                    }
                }
            });
        },
    );
    y += 30.0 + 12.0;
    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-lang-note")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y),
                Vec2::new(content.width(), 32.0),
            )),
        |ui| {
            ui.label(
                RichText::new(crate::i18n::t!(
                    "语言自称不翻译：中文在任何界面里都写作「中文」。"
                ))
                .size(SIZE_SMALL)
                .color(dim()),
            );
        },
    );

    if changed {
        state.settings_dirty = true;
        ui.ctx().request_repaint();
    }
    total
}

/// 快捷键卡：一张只读表，键位走 mono 右对齐（对齐 PS 菜单的快捷键列口径）。
///
/// Shortcut card: a read-only table with monospace right-aligned keys, matching the
/// shortcut-column convention of PS menus.
fn shortcut_card(ui: &mut Ui, at: Rect) -> f32 {
    let width = at.width();
    let rows: [(&str, &str); 6] = [
        ("打开序列", "Ctrl+O"),
        ("导入照片", "Ctrl+I"),
        ("导出当前帧", "Ctrl+E"),
        ("命令面板", "Ctrl+K"),
        ("撤销", "Ctrl+Z"),
        ("重新扫描", "F5"),
    ];
    let row_h = 26.0;
    let total = 46.0 + 20.0 + rows.len() as f32 * row_h + 16.0;
    let content = card_frame(ui, at.min, width, total, crate::i18n::t!("快捷键"));
    let p = ui.painter().clone();
    let mut y = content.top() + 20.0;
    for (label, key) in rows {
        let row = Rect::from_min_size(
            Pos2::new(content.left(), y),
            Vec2::new(content.width(), row_h),
        );
        if row.center().x > 0.0 {
            p.text(
                Pos2::new(row.left(), row.center().y),
                Align2::LEFT_CENTER,
                crate::i18n::tr(label),
                font(SIZE_LABEL),
                muted(),
            );
            p.text(
                Pos2::new(row.right(), row.center().y),
                Align2::RIGHT_CENTER,
                key,
                mono(SIZE_LABEL),
                dim(),
            );
            let ly = row.bottom() - 1.0;
            p.line_segment(
                [Pos2::new(row.left(), ly), Pos2::new(row.right(), ly)],
                Stroke::new(1.0, divider()),
            );
        }
        y += row_h;
    }
    total
}

/// 更新卡：当前版本 + 检查按钮 + 上次检查时间。
///
/// Update card: current version, a check button and the last check time.
fn update_card(state: &mut AppState, ui: &mut Ui, at: Rect) -> f32 {
    let width = at.width();
    let total = 46.0 + 20.0 + 30.0 + 12.0 + 34.0 + 16.0;
    let content = card_frame(ui, at.min, width, total, crate::i18n::t!("更新"));
    let p = ui.painter().clone();
    let mut y = content.top();

    label_at(ui, content.left(), y, crate::i18n::t!("当前版本"));
    y += 20.0;
    p.text(
        Pos2::new(content.left(), y + 4.0),
        Align2::LEFT_TOP,
        aevum_core::update::current_version(),
        mono(SIZE_LABEL),
        text(),
    );

    ui.scope_builder(
        UiBuilder::new()
            .id_salt("set-update")
            .max_rect(Rect::from_min_size(
                Pos2::new(content.left(), y + 26.0),
                Vec2::new(content.width(), 32.0),
            )),
        |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(
                        RichText::new(crate::i18n::t!("检查更新…")).size(SIZE_LABEL),
                    ))
                    .clicked()
                {
                    state.look_open = false;
                    state.update_open = true;
                }
                ui.label(
                    RichText::new(crate::i18n::t!("更新只替换程序本体，工程与设置不动。"))
                        .size(SIZE_SMALL)
                        .color(dim()),
                );
            });
        },
    );
    total
}

/// 在指定位置画一个小标签（卡片内分组标题）。
///
/// Draw a small label at a position (in-card group heading).
fn label_at(ui: &Ui, x: f32, y: f32, label: &str) {
    ui.painter().text(
        Pos2::new(x, y + 9.0),
        Align2::LEFT_CENTER,
        label,
        font(SIZE_LABEL),
        muted(),
    );
}

/// 分段控件里的一枚：选中 = elev 底 + 白字，未选 = 透明 + muted。
///
/// One segment chip: selected is `elev` fill with white text, otherwise transparent
/// with `muted`.
fn seg_chip(ui: &mut Ui, label: &str, selected: bool) -> bool {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        font(SIZE_LABEL),
        if selected { text() } else { muted() },
    );
    let w = galley.size().x + 20.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, radius_control(), elev());
        p.rect_stroke(
            rect,
            radius_control(),
            Stroke::new(1.0, border_strong()),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, radius_control(), elev_hover());
    }
    p.galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        if selected { text() } else { muted() },
    );
    resp.clicked()
}

/// 紧凑数值输入：标签 + 宽度 64 的框，返回是否改动。
///
/// Compact numeric field: a label plus a 64 px box; reports whether it changed.
fn compact_value(ui: &mut Ui, label: &str, v: &mut f64, min: f64, max: f64, speed: f64) -> bool {
    let mut changed = false;
    if !label.is_empty() {
        ui.label(RichText::new(label).size(SIZE_SMALL).color(dim()));
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::new(64.0, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, radius_control(), field());
    ui.painter().rect_stroke(
        rect,
        radius_control(),
        Stroke::new(1.0, divider()),
        StrokeKind::Inside,
    );
    let inner = rect.shrink2(Vec2::new(6.0, 0.0));
    ui.scope_builder(
        UiBuilder::new()
            .id_salt(format!("cv-{label}"))
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            changed = ui
                .add(
                    egui::DragValue::new(v)
                        .range(min..=max)
                        .speed(speed)
                        .fixed_decimals(if speed < 1.0 { 1 } else { 0 }),
                )
                .changed();
        },
    );
    changed
}
