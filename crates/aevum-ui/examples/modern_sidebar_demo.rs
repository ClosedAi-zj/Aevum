//! 侧边栏 + 内容卡片列表预览（`cargo run -p aevum-ui --example modern_sidebar_demo`）：
//! [`aevum_ui::theme_modern::apply_modern_dark`] 之上的两层结构——
//! 左侧导航（分组/搜索/选中态）+ 右侧响应式卡片网格（渐变缩略图、状态 chips、
//! 进度条、评分点）。真实种子数据：恒晷的六段延时序列。
//!
//! Sidebar + card-list preview on top of the modern dark theme: left navigation
//! (groups / search / selection states) and a responsive card grid (gradient
//! thumbnails, status chips, progress, rating dots) with six real timelapse
//! sequences as seed data.

use aevum_ui::theme_modern::{
    ACCENT, BG, ELEVATED, FIELD, TEXT, TEXT_FAINT, TEXT_MUTED, apply_modern_dark,
};
use eframe::egui::{self, Color32, Rect, RichText, Sense, Stroke, Vec2};

/// 卡片状态：与恒晷的队列语义一致。
///
/// Card status, mirroring Aevum's queue semantics.
enum Status {
    /// 渲染中（带 0..1 进度）。
    Running(f32),
    Pending,
    Done,
}

struct Card {
    title: String,
    frames: usize,
    duration: &'static str,
    fps: u32,
    codec: &'static str,
    status: Status,
    rating: usize,
    /// 缩略图三段色（天顶 / 中段 / 地平线）。
    hues: (Color32, Color32, Color32),
    night: bool,
}

impl Card {
    fn visible_in(&self, view: View) -> bool {
        match view {
            View::All => true,
            View::Recent => self.rating >= 2,
            View::Starred => self.rating >= 3,
        }
    }
    fn status_label(&self) -> (&'static str, Color32) {
        match &self.status {
            Status::Running(_) => ("渲帧中", ACCENT),
            Status::Pending => ("待渲", TEXT_MUTED),
            Status::Done => ("已完成", Color32::from_rgb(0x5F, 0xBF, 0x7A)),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    All,
    Recent,
    Starred,
}

/// 侧边栏入口：图标种类 + 文字。
///
/// A sidebar entry: icon kind + label.
enum Entry {
    Work(&'static str, Icon),
    Group(&'static str),
    Seq(&'static str, Icon, usize),
}

enum Icon {
    Sliders,
    Table,
    Export,
    Tasks,
    Grid,
    Clock,
    Star,
}

fn cards() -> Vec<Card> {
    vec![
        Card {
            title: "松赞林寺-198".into(),
            frames: 48,
            duration: "00:02",
            fps: 24,
            codec: "H.265",
            status: Status::Running(0.42),
            rating: 3,
            hues: (
                Color32::from_rgb(0x2A, 0x2A, 0x52),
                Color32::from_rgb(0xB3, 0x5C, 0x3E),
                Color32::from_rgb(0xE8, 0xA2, 0x5A),
            ),
            night: false,
        },
        Card {
            title: "纳木错星轨-012".into(),
            frames: 240,
            duration: "00:10",
            fps: 30,
            codec: "H.265",
            status: Status::Done,
            rating: 5,
            hues: (
                Color32::from_rgb(0x08, 0x0C, 0x1E),
                Color32::from_rgb(0x14, 0x22, 0x4C),
                Color32::from_rgb(0x2A, 0x3E, 0x74),
            ),
            night: true,
        },
        Card {
            title: "城市灯火-004".into(),
            frames: 36,
            duration: "00:01",
            fps: 25,
            codec: "H.264",
            status: Status::Done,
            rating: 4,
            hues: (
                Color32::from_rgb(0x14, 0x14, 0x2A),
                Color32::from_rgb(0x3A, 0x2C, 0x5E),
                Color32::from_rgb(0x7A, 0x4E, 0x9E),
            ),
            night: true,
        },
        Card {
            title: "青海湖日出-045".into(),
            frames: 60,
            duration: "00:02",
            fps: 24,
            codec: "H.265",
            status: Status::Done,
            rating: 0,
            hues: (
                Color32::from_rgb(0x2C, 0x3E, 0x62),
                Color32::from_rgb(0x9E, 0x5C, 0x6E),
                Color32::from_rgb(0xF2, 0xA2, 0x8A),
            ),
            night: false,
        },
        Card {
            title: "公路云海-071".into(),
            frames: 72,
            duration: "00:03",
            fps: 30,
            codec: "H.265",
            status: Status::Pending,
            rating: 0,
            hues: (
                Color32::from_rgb(0x1C, 0x2C, 0x42),
                Color32::from_rgb(0x3E, 0x5C, 0x82),
                Color32::from_rgb(0x8A, 0xB4, 0xC9),
            ),
            night: false,
        },
        Card {
            title: "黄河九曲-033".into(),
            frames: 96,
            duration: "00:04",
            fps: 24,
            codec: "ProRes",
            status: Status::Pending,
            rating: 2,
            hues: (
                Color32::from_rgb(0x1E, 0x32, 0x38),
                Color32::from_rgb(0x3C, 0x6E, 0x6E),
                Color32::from_rgb(0xC9, 0xA2, 0x5E),
            ),
            night: false,
        },
    ]
}

/// 1.5px 线性小图标（24 网格语义缩到 16px 内绘）。
///
/// 1.5 px line icons (24-grid semantics drawn inside 16 px).
fn paint_icon(p: &egui::Painter, c: egui::Pos2, kind: &Icon, color: Color32) {
    let s = Stroke::new(1.5, color);
    match kind {
        Icon::Sliders => {
            for (y, x) in [(0.3, 0.6), (0.5, 0.3), (0.7, 0.7)] {
                p.line_segment(
                    [
                        egui::pos2(c.x - 7.0, c.y + y * 14.0 - 7.0),
                        egui::pos2(c.x + 7.0, c.y + y * 14.0 - 7.0),
                    ],
                    s,
                );
                p.circle_filled(
                    egui::pos2(c.x + (x - 0.5) * 14.0, c.y + y * 14.0 - 7.0),
                    2.4,
                    color,
                );
            }
        }
        Icon::Table => {
            p.rect_stroke(
                Rect::from_min_size(egui::pos2(c.x - 6.5, c.y - 5.5), Vec2::new(13.0, 11.0)),
                2.0,
                s,
                egui::StrokeKind::Inside,
            );
            p.line_segment(
                [
                    egui::pos2(c.x - 6.5, c.y - 1.0),
                    egui::pos2(c.x + 6.5, c.y - 1.0),
                ],
                s,
            );
            p.line_segment(
                [
                    egui::pos2(c.x - 1.5, c.y - 5.5),
                    egui::pos2(c.x - 1.5, c.y + 5.5),
                ],
                s,
            );
        }
        Icon::Export => {
            p.line_segment([egui::pos2(c.x, c.y - 6.5), egui::pos2(c.x, c.y + 1.5)], s);
            p.add(egui::Shape::line(
                vec![
                    egui::pos2(c.x - 3.5, c.y - 3.0),
                    egui::pos2(c.x, c.y - 6.5),
                    egui::pos2(c.x + 3.5, c.y - 3.0),
                ],
                s,
            ));
            p.add(egui::Shape::line(
                vec![
                    egui::pos2(c.x - 6.5, c.y + 3.5),
                    egui::pos2(c.x - 6.5, c.y + 6.5),
                    egui::pos2(c.x + 6.5, c.y + 6.5),
                    egui::pos2(c.x + 6.5, c.y + 3.5),
                ],
                s,
            ));
        }
        Icon::Tasks => {
            p.rect_stroke(
                Rect::from_min_size(egui::pos2(c.x - 6.0, c.y - 6.0), Vec2::new(12.0, 12.0)),
                3.0,
                s,
                egui::StrokeKind::Inside,
            );
            p.add(egui::Shape::line(
                vec![
                    egui::pos2(c.x - 3.0, c.y + 0.5),
                    egui::pos2(c.x - 0.8, c.y + 3.0),
                    egui::pos2(c.x + 3.2, c.y - 2.6),
                ],
                s,
            ));
        }
        Icon::Grid => {
            for k in [-2.4f32, 2.4] {
                p.line_segment(
                    [
                        egui::pos2(c.x + k, c.y - 6.5),
                        egui::pos2(c.x + k, c.y + 6.5),
                    ],
                    s,
                );
                p.line_segment(
                    [
                        egui::pos2(c.x - 6.5, c.y + k),
                        egui::pos2(c.x + 6.5, c.y + k),
                    ],
                    s,
                );
            }
        }
        Icon::Clock => {
            p.circle_stroke(c, 6.2, s);
            p.line_segment([egui::pos2(c.x, c.y - 3.4), egui::pos2(c.x, c.y + 0.4)], s);
            p.line_segment([egui::pos2(c.x, c.y), egui::pos2(c.x + 3.0, c.y + 1.6)], s);
        }
        Icon::Star => {
            p.circle_filled(c, 2.6, color);
            p.line_segment(
                [
                    egui::pos2(c.x - 6.0, c.y + 4.5),
                    egui::pos2(c.x + 6.0, c.y + 4.5),
                ],
                s,
            );
        }
    }
}

struct Demo {
    view: View,
    bench: usize,
    selected: usize,
    query: String,
}

impl eframe::App for Demo {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // ---------- 侧边栏 ----------
        egui::Panel::left("sidebar")
            .exact_size(232.0)
            .resizable(false)
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| {
                ui.add_space(12.0);
                // 品牌
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new("恒晷").size(15.0).strong().color(TEXT));
                    ui.label(RichText::new("Aevum Studio").size(11.5).color(TEXT_FAINT));
                });
                ui.add_space(10.0);
                ui.scope(|ui| {
                    ui.style_mut().spacing.button_padding = Vec2::new(10.0, 6.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("搜索序列…")
                            .desired_width(ui.available_width() - 24.0),
                    );
                });
                ui.add_space(14.0);

                let entries: Vec<Entry> = vec![
                    Entry::Group("工作台"),
                    Entry::Work("显影", Icon::Sliders),
                    Entry::Work("帧表", Icon::Table),
                    Entry::Work("导出", Icon::Export),
                    Entry::Work("任务", Icon::Tasks),
                    Entry::Group("序列"),
                    Entry::Seq("全部序列", Icon::Grid, 6),
                    Entry::Seq("最近打开", Icon::Clock, 2),
                    Entry::Seq("已收藏", Icon::Star, 1),
                ];
                for e in &entries {
                    match e {
                        Entry::Group(g) => {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add_space(10.0);
                                ui.label(RichText::new(*g).size(11.0).color(TEXT_FAINT));
                            });
                            ui.add_space(2.0);
                        }
                        Entry::Work(label, icon) | Entry::Seq(label, icon, _) => {
                            let on = match e {
                                Entry::Work(_, _) => self.bench == entries.iter().position(|x| matches!(x, Entry::Work(l, _) if std::ptr::eq(l, label))).unwrap_or(usize::MAX),
                                Entry::Seq(_, _, _) => self.view == match *label {
                                    "全部序列" => View::All,
                                    "最近打开" => View::Recent,
                                    _ => View::Starred,
                                },
                                _ => false,
                            };
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
                            let p = ui.painter();
                            if on {
                                p.rect_filled(rect, 8.0, ACCENT);
                            } else if resp.hovered() {
                                p.rect_filled(rect, 8.0, Color32::from_white_alpha(14));
                            }
                            let ink = if on { Color32::WHITE } else if resp.hovered() { TEXT } else { TEXT_MUTED };
                            let ic = if on { Color32::WHITE } else { TEXT_MUTED };
                            paint_icon(p, egui::pos2(rect.left() + 22.0, rect.center().y), icon, ic);
                            p.text(
                                egui::pos2(rect.left() + 38.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                *label,
                                egui::FontId::new(13.0, egui::FontFamily::Proportional),
                                ink,
                            );
                            if let Entry::Seq(_, _, n) = e {
                                p.text(
                                    egui::pos2(rect.right() - 12.0, rect.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    n.to_string(),
                                    egui::FontId::new(11.5, egui::FontFamily::Proportional),
                                    if on { Color32::WHITE } else { TEXT_FAINT },
                                );
                            }
                            if resp.clicked() {
                                match e {
                                    Entry::Work(_, _) => self.bench = entries.iter().position(|x| matches!(x, Entry::Work(l, _) if std::ptr::eq(l, label))).unwrap_or(0),
                                    Entry::Seq(_, _, _) => {
                                        self.view = match *label {
                                            "全部序列" => View::All,
                                            "最近打开" => View::Recent,
                                            _ => View::Starred,
                                        };
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    paint_icon(ui.painter(), egui::pos2(ui.cursor().left() + 12.0, ui.cursor().center().y), &Icon::Tasks, TEXT_MUTED);
                    ui.label(RichText::new("设置").size(13.0).color(TEXT_MUTED));
                });
            });

        // ---------- 内容区 ----------
        egui::CentralPanel::default_margins()
            .frame(egui::Frame::NONE.fill(FIELD).inner_margin(egui::Margin::same(18)))
            .show(ui, |ui| {
                let view_name = match self.view { View::All => "全部序列", View::Recent => "最近打开", View::Starred => "已收藏" };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(view_name).size(17.0).strong().color(TEXT));
                    let n = cards().into_iter().filter(|c| c.visible_in(self.view)).count();
                    ui.label(RichText::new(format!("{n} 段")).size(12.5).color(TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let _ = ui.add(
                            egui::Button::new(RichText::new("＋ 新建序列").size(13.0).color(Color32::WHITE))
                                .fill(ACCENT)
                                .min_size(Vec2::new(0.0, 32.0)),
                        );
                    });
                });
                ui.add_space(14.0);

                let all = cards();
                let shown: Vec<usize> = (0..all.len()).filter(|i| all[*i].visible_in(self.view)).collect();
                // 响应式网格：horizontal_wrapped 流式换行，卡片定宽 320，列数由窗口宽决定。
                //
                // Responsive grid: horizontal_wrapped flows fixed-width 320 px cards;
                // the column count follows the window width.
                ui.horizontal_wrapped(|ui| {
                    ui.style_mut().spacing.item_spacing = Vec2::new(16.0, 16.0);
                    for idx in shown {
                        let card = &all[idx];
                        ui.allocate_ui_with_layout(
                            Vec2::new(320.0, 236.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(320.0);
                                draw_card(ui, card, idx, self.selected == idx, &mut self.selected);
                            },
                        );
                    }
                });
                ui.add_space(10.0);
                ui.label(
                    RichText::new("卡片 = ELEVATED #26263A · 圆角 12 · 1px 白 7% 边 · hover 提亮 · 选中=强调描边 —— 全部来自 apply_modern_dark")
                        .size(11.5)
                        .color(TEXT_FAINT),
                );
            });
    }
}

fn draw_card(ui: &mut egui::Ui, card: &Card, idx: usize, selected: bool, selected_out: &mut usize) {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 232.0), Sense::click());
    let p = ui.painter();
    // 卡底：elevated + 细边框；hover 提亮；选中=强调描边。
    p.rect_filled(rect, 12.0, ELEVATED);
    let line = if selected {
        Stroke::new(2.0, ACCENT)
    } else if resp.hovered() {
        Stroke::new(1.0, Color32::from_white_alpha(45))
    } else {
        Stroke::new(1.0, Color32::from_white_alpha(18))
    };
    p.rect_stroke(rect, 12.0, line, egui::StrokeKind::Inside);

    // 缩略图：三段天色 + 太阳/星轨 + 地平线，顶部圆角 12。
    let thumb = Rect::from_min_size(
        rect.min + Vec2::new(1.0, 1.0),
        Vec2::new(width - 2.0, 118.0),
    );
    p.rect_filled(thumb, 10.0, card.hues.1);
    let n = 24;
    for i in 0..n {
        let t = i as f32 / n as f32;
        let c = if t < 0.55 {
            lerp(card.hues.0, card.hues.1, t / 0.55)
        } else {
            lerp(card.hues.1, card.hues.2, (t - 0.55) / 0.45)
        };
        let h = thumb.height() / n as f32;
        p.rect_filled(
            Rect::from_min_size(
                egui::pos2(thumb.left(), thumb.top() + i as f32 * h),
                Vec2::new(thumb.width(), h + 0.8),
            ),
            0.0,
            c,
        );
    }
    let (c1, c2) = (thumb.center(), thumb.center());
    if card.night {
        // 星轨：几段圆弧。
        for (r, a) in [(26.0, 0.6), (38.0, 2.2), (50.0, 4.1)] {
            p.circle_stroke(c1, r, Stroke::new(1.0, Color32::from_white_alpha(46)));
            let _ = (a, c2);
        }
        p.circle_filled(egui::pos2(c1.x, c1.y), 3.0, Color32::WHITE);
    } else {
        p.circle_filled(
            egui::pos2(c1.x, thumb.top() + 74.0),
            16.0,
            Color32::from_rgb(0xFF, 0xE3, 0xA8),
        );
        p.circle_filled(
            egui::pos2(c1.x, thumb.top() + 74.0),
            30.0,
            Color32::from_rgba_unmultiplied(0xFF, 0xC4, 0x6A, 60),
        );
        let ground = thumb.bottom() - 26.0;
        p.rect_filled(
            Rect::from_min_size(
                egui::pos2(thumb.left(), ground),
                Vec2::new(thumb.width(), 26.0),
            ),
            0.0,
            Color32::from_rgb(0x12, 0x0E, 0x18),
        );
    }
    // 时长角标
    p.text(
        egui::pos2(thumb.right() - 8.0, thumb.bottom() - 10.0),
        egui::Align2::RIGHT_CENTER,
        card.duration,
        egui::FontId::new(11.0, egui::FontFamily::Proportional),
        Color32::from_white_alpha(230),
    );

    // 状态 chip（右上）
    let (st, stc) = card.status_label();
    let stw =
        p.layout_no_wrap(
            st.to_owned(),
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
            stc,
        )
        .size()
        .x + 14.0;
    let chip = Rect::from_min_size(
        egui::pos2(thumb.left() + 8.0, thumb.top() + 8.0),
        Vec2::new(stw, 20.0),
    );
    p.rect_filled(chip, 10.0, Color32::from_black_alpha(120));
    p.text(
        chip.center(),
        egui::Align2::CENTER_CENTER,
        st,
        egui::FontId::new(11.0, egui::FontFamily::Proportional),
        stc,
    );

    // 主体：标题 / 元信息 / 评分点
    let body = Rect::from_min_size(
        egui::pos2(rect.left() + 12.0, thumb.bottom() + 10.0),
        Vec2::new(width - 24.0, rect.bottom() - thumb.bottom() - 18.0),
    );
    p.text(
        egui::pos2(body.left(), body.top() + 8.0),
        egui::Align2::LEFT_CENTER,
        &card.title,
        egui::FontId::new(14.5, egui::FontFamily::Proportional),
        TEXT,
    );
    p.text(
        egui::pos2(body.left(), body.top() + 30.0),
        egui::Align2::LEFT_CENTER,
        format!(
            "{} 帧 · {} · {} fps · {}",
            card.frames, card.duration, card.fps, card.codec
        ),
        egui::FontId::new(12.0, egui::FontFamily::Proportional),
        TEXT_MUTED,
    );
    // 评分点：filled=accent，空=faint
    for k in 0..5 {
        let c = egui::pos2(body.left() + 5.0 + k as f32 * 12.0, body.top() + 52.0);
        p.circle_filled(c, 2.8, if k < card.rating { ACCENT } else { TEXT_FAINT });
    }
    // 进度（渲帧中）
    if let Status::Running(f) = &card.status {
        let bar = Rect::from_min_size(
            egui::pos2(body.left(), body.bottom() - 10.0),
            Vec2::new(body.width(), 5.0),
        );
        p.rect_filled(bar, 2.5, Color32::from_white_alpha(26));
        p.rect_filled(
            Rect::from_min_size(bar.min, Vec2::new(bar.width() * f, bar.height())),
            2.5,
            ACCENT,
        );
        p.text(
            egui::pos2(bar.right(), bar.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{}%", (f * 100.0) as i32),
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
            TEXT_MUTED,
        );
    }

    if resp.clicked() {
        *selected_out = if selected { usize::MAX } else { idx };
    }
    let _ = idx;
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("恒晷 · 侧边栏 + 卡片列表")
            .with_inner_size([1280.0, 800.0])
            .with_position([120.0, 90.0]),
        ..Default::default()
    };
    eframe::run_native(
        "恒晷 · 侧边栏 + 卡片列表",
        opts,
        Box::new(|cc| {
            apply_modern_dark(&cc.egui_ctx);
            Ok(Box::new(Demo {
                view: View::All,
                bench: 0,
                selected: usize::MAX,
                query: String::new(),
            }))
        }),
    )
}
