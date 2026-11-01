//! 浮动窗口：菜单段/内置动作/插件菜单/市场/调色窗口/关于。

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::library::group_title;
use super::menu::menu_item;
use super::menu::menu_style;
use crate::layout::StatusSlot;
use crate::platform;
use crate::state::AppState;
use crate::theme::{self, *};
use egui::{Align, Align2, Context, Layout, Rect, RichText, ScrollArea, Sense, Stroke, Ui, Vec2};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

/// 浮窗标题栏的统一口径：底色接管成 panel，**只圆上面两角**。
///
/// 此前这里写死 `corner_radius(0.0)`：方角标题条压在圆角窗体上，窗的上两角
/// 被削成直角、下两角还是圆的——四个角弧度不一致就是这么来的。上两角取
/// `radius_window()`（与窗体同源），下两角归零交给窗体本体，整窗一套弧度。
fn title_frame() -> egui::Frame {
    let r = radius_window() as u8;
    egui::Frame::new()
        .fill(panel())
        .stroke(Stroke::NONE)
        .corner_radius(egui::CornerRadius {
            nw: r,
            ne: r,
            sw: 0,
            se: 0,
        })
        .inner_margin(egui::Margin::symmetric(12, 8))
}

/// 窗内分节的软分割线：`theme::soft_rule` 的 UI 包装，占 9px 高、线居中。
///
/// egui 自带的 `ui.separator()` 是通栏实线，和浮窗边框顶死，读作「切开」；
/// 这里换成缩进浅线，读作「分节」。
fn soft_sep(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 9.0), Sense::hover());
    theme::soft_rule(ui.painter(), rect.left(), rect.right(), rect.center().y);
}

/// 菜单里的小标题：比正文小一号、压暗，读作"下面这些属于这一类"。
fn menu_section(ui: &mut Ui, title: &str) {
    ui.add_space(3.0);
    ui.label(RichText::new(title).size(SIZE_SMALL).color(dim()));
}

/// 内置插件对应的真实动作。
///
/// 这层映射留在宿主而不是清单里：清单是给第三方作者写的格式，
/// 不该出现"宿主内部命令名"这种只有本程序知道的东西。
fn builtin_action(id: &str) -> Option<&'static str> {
    match id {
        "aevum.hdr-merge" => Some("hdr.merge"),
        "aevum.pano" => Some("pano.merge"),
        "aevum.lens-correct" => Some("lens.set"),
        _ => None,
    }
}

pub(super) fn plugin_menu(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    menu_style(ui);

    // 先把要显示的东西抄出来，再画：树里存的是下标，画的时候要写 state，
    // 直接借 `state.registry` 会和 `state.status` 的可变借打架。
    struct Row {
        id: String,
        label: String,
        tag: String,
        desc: String,
    }
    let tree = state.registry.menu_tree();
    let rows: Vec<(String, Vec<Row>)> = tree
        .iter()
        .map(|g| {
            let items = g
                .items
                .iter()
                .map(|i| {
                    let p = &state.registry.all()[*i];
                    Row {
                        id: p.manifest.plugin.id.clone(),
                        label: p.manifest.menu_label().to_owned(),
                        tag: p.manifest.plugin.source.cn().to_owned(),
                        desc: p.manifest.plugin.description.clone(),
                    }
                })
                .collect();
            (group_title(&g.group).to_owned(), items)
        })
        .collect();
    let bad = state.registry.errors().len();

    if rows.is_empty() && bad == 0 {
        ui.label(
            RichText::new(crate::i18n::t!("还没有安装插件"))
                .size(SIZE_BODY)
                .color(dim()),
        );
    }
    let mut open_market = false;
    let mut fire: Option<(String, String)> = None;
    for (title, items) in &rows {
        menu_section(ui, title);
        for r in items {
            let resp = ui.add(
                egui::Button::new(RichText::new(&r.label).size(SIZE_BODY))
                    .shortcut_text(RichText::new(&r.tag).size(SIZE_SMALL).color(dim())),
            );
            let resp = if r.desc.is_empty() {
                resp
            } else {
                resp.on_hover_text(&r.desc)
            };
            if resp.clicked() {
                fire = Some((r.id.clone(), r.label.clone()));
            }
        }
    }
    if bad > 0 {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("{} 份清单有问题", &[&bad]))
                .size(SIZE_SMALL)
                .color(danger()),
        );
    }
    soft_sep(ui);
    if state.plugin_result.is_some() && menu_item(ui, "清除插件效果", "") {
        state.plugin_result = None;
        ui.close();
    }
    if menu_item(ui, "管理插件…", "Ctrl+Shift+M") {
        open_market = true;
        ui.close();
    }

    if let Some((id, label)) = fire {
        ui.close();
        match builtin_action(&id) {
            Some(cmd) => {
                if let Err(e) =
                    state.run_ui_command(ctx, cmd, &serde_json::json!({"confirm": true}))
                {
                    state.error = Some(crate::i18n::tr_fmt("{} 没能执行：{}", &[&label, &e]));
                }
            }
            None => {
                // 「已经在跑」按状态判断，不按错误文案判断：那句话会随语言变，
                // 用 `contains` 认它，切到英文就认不出来了——本该只是一句提示，
                // 会变成弹个错误横幅。
                if state.plugin_running() {
                    state.status = crate::i18n::tr("已有插件在运行").to_owned();
                } else {
                    match state.plugin_start(ctx, &id) {
                        // 起任务就回：进度与结果由后台任务自己走状态栏那条线。
                        Ok(name) => {
                            state.status = crate::i18n::tr_fmt("{} 运行中…", &[&name]);
                        }
                        Err(e) => state.error = Some(format!("{label}：{e}")),
                    }
                }
            }
        }
    }
    if open_market {
        state.market_open = true;
    }
}

/// 插件市场。左栏类别、右栏清单，每行一个插件。
///
/// 刻意不做卡片网格：一个插件要回答的问题只有四个 —— 叫什么、谁做的、
/// 哪一版、现在开没开。卡片为这四行信息花掉两百多像素，一屏只看得到四个。
pub(super) fn market_window(state: &mut AppState, ctx: &Context) {
    if !state.market_open {
        return;
    }
    use aevum_core::plugin::Kind;
    let mut open = state.market_open;
    let mut toggle: Option<(String, bool)> = None;
    let mut rescan = false;

    egui::Window::new(crate::i18n::t!("插件"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(Vec2::new(760.0, 520.0))
        .min_size(Vec2::new(620.0, 380.0))
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        // 标题栏接管成界面自己的深色：egui 默认标题栏的底与描边跟主界面
        // 不是一套口径（实测亮蓝，用户差评），这里统一成 panel 底 + 底缘
        // 一条 divider，标题居中、关闭钮走默认绘制。
        .title_frame(title_frame())
        .show(ctx, |ui| {
            // 标题栏下面压一条软分割线，把标题区和清单分开。
            let head = Rect::from_min_size(ui.min_rect().min, Vec2::new(ui.available_width(), 1.0));
            theme::soft_rule(ui.painter(), head.left(), head.right(), head.top());
            // 定住下限：`default_size` 只是建议，内容一少窗口就缩成一团，
            // 列表只剩三行可见 —— 那时候滚动条比窗口还长。窗口的 min_size
            // 已经兜底（620×380），不再用 set_min_size 硬顶内容高度——那会
            // 把缩放手柄顶死，窗口拉不大也收不小。
            let body_h = ui.available_height() - 4.0;
            ui.horizontal_top(|ui| {
                // 左栏：类别。用 selectable_label 而不是自制列表，
                // 键鼠焦点与选中态就是现成的。
                ui.allocate_ui_with_layout(
                    Vec2::new(132.0, body_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        let kinds: [(Option<Kind>, &str); 4] = [
                            (None, "全部"),
                            (Some(Kind::Filter), "修图滤镜"),
                            (Some(Kind::Decoder), "解码器"),
                            (Some(Kind::Export), "导出"),
                        ];
                        for (k, label) in kinds {
                            let mut n = state
                                .registry
                                .all()
                                .iter()
                                .filter(|p| k.is_none_or(|k| p.manifest.plugin.kind == k))
                                .count();
                            // 「全部」前面那个数要算上清单坏掉的，否则用户会觉得少了东西。
                            if k.is_none() {
                                n += state.registry.errors().len();
                            }
                            let text = format!("{label}  {n}");
                            if ui
                                .add(
                                    egui::Button::new(RichText::new(text).size(SIZE_BODY))
                                        .selected(state.market_filter == k),
                                )
                                .clicked()
                            {
                                state.market_filter = k;
                            }
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(crate::i18n::t!("来自 %APPDATA%\naevum-studio\\plugins"))
                                .size(10.0)
                                .color(dim()),
                        );
                        if ui
                            .add(egui::Button::new(
                                RichText::new(crate::i18n::t!("重新扫描")).size(SIZE_SMALL),
                            ))
                            .clicked()
                        {
                            rescan = true;
                        }
                    },
                );
                soft_sep(ui);

                // 右栏：搜索 + 列表
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), body_h),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut state.market_query)
                                    .hint_text(crate::i18n::t!("搜索插件、厂商"))
                                    .desired_width(240.0),
                            );
                            if !state.market_query.is_empty()
                                && ui
                                    .add(egui::Button::new(
                                        RichText::new(crate::i18n::t!("清除")).size(SIZE_SMALL),
                                    ))
                                    .clicked()
                            {
                                state.market_query.clear();
                            }
                        });
                        ui.add_space(4.0);

                        let q = state.market_query.to_lowercase();
                        let hits: Vec<usize> = state
                            .registry
                            .all()
                            .iter()
                            .enumerate()
                            .filter(|(_, p)| {
                                state
                                    .market_filter
                                    .is_none_or(|k| p.manifest.plugin.kind == k)
                                    && (q.is_empty()
                                        || p.manifest.plugin.name.to_lowercase().contains(&q)
                                        || p.manifest.plugin.vendor.to_lowercase().contains(&q)
                                        || p.manifest
                                            .plugin
                                            .description
                                            .to_lowercase()
                                            .contains(&q))
                            })
                            .map(|(i, _)| i)
                            .collect();

                        ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if hits.is_empty() {
                                    ui.add_space(12.0);
                                    ui.label(
                                        RichText::new(crate::i18n::t!("没有匹配的插件"))
                                            .size(SIZE_BODY)
                                            .color(dim()),
                                    );
                                }
                                for i in hits {
                                    let p = &state.registry.all()[i];
                                    let meta = &p.manifest.plugin;
                                    let enabled = p.enabled;
                                    // 行：名字 + 版本 + 厂商，右侧开关。
                                    ui.horizontal(|ui| {
                                        ui.set_min_height(26.0);
                                        ui.label(
                                            RichText::new(&meta.name).size(SIZE_BODY).color(text()),
                                        );
                                        ui.label(
                                            RichText::new(&meta.version)
                                                .size(SIZE_SMALL)
                                                .color(dim()),
                                        );
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                let mut on = enabled;
                                                if ui.checkbox(&mut on, "").changed() {
                                                    toggle = Some((meta.id.clone(), on));
                                                }
                                                ui.label(
                                                    RichText::new(meta.source.cn())
                                                        .size(10.0)
                                                        .color(dim()),
                                                );
                                                ui.label(
                                                    RichText::new(meta.kind.cn())
                                                        .size(10.0)
                                                        .color(dim()),
                                                );
                                            },
                                        );
                                    });
                                    if !meta.description.is_empty() {
                                        ui.label(
                                            RichText::new(&meta.description)
                                                .size(SIZE_SMALL)
                                                .color(dim()),
                                        );
                                    }
                                    ui.add_space(3.0);
                                    soft_sep(ui);
                                    ui.add_space(3.0);
                                }

                                // 坏清单单独列在最后：不藏，也不打断上面的列表。
                                if !state.registry.errors().is_empty() {
                                    ui.add_space(8.0);
                                    ui.label(
                                        RichText::new(crate::i18n::t!("清单有问题"))
                                            .size(SIZE_SMALL)
                                            .color(danger()),
                                    );
                                    for (path, why) in state.registry.errors() {
                                        ui.label(
                                            RichText::new(format!(
                                                "{}：{why}",
                                                path.parent()
                                                    .map(|d| d.display().to_string())
                                                    .unwrap_or_default()
                                            ))
                                            .size(10.0)
                                            .color(dim()),
                                        );
                                    }
                                }
                            });
                    },
                );
            });
        });
    state.market_open = open;
    if let Some((id, on)) = toggle {
        state.registry.set_enabled(&id, on);
        state.status = format!("{}：{}", id, if on { "已启用" } else { "已停用" });
    }
    if rescan {
        let root = crate::settings::plugin_dir();
        state.registry = aevum_core::plugin::Registry::default()
            .with_builtin(aevum_core::plugin::builtin(), &root)
            .merge(aevum_core::plugin::Registry::scan(
                &root,
                aevum_core::plugin::Source::User,
            ));
        state.status = crate::i18n::tr_fmt("已重新扫描 {}", &[&root.display()]);
    }
}

// ---------------------------------------------------------------- 外观与布局

/// 小标题：10px 等宽、压暗一档，与面板里的分组标题同一套语气。
fn look_section(ui: &mut Ui, title: &str) {
    let title = crate::i18n::tr(title);
    ui.add_space(6.0);
    ui.label(RichText::new(title).size(SIZE_SMALL).color(muted()));
}

/// 一行「标签 + 若干互斥选项」。
///
/// 七个控件用手写 `selectable_label` 会把这一屏写成 200 行；统一成这个形状之后，
/// 加一个布局轴只是加一行数据。
fn look_row<T: PartialEq + Copy>(
    ui: &mut Ui,
    label: &str,
    current: T,
    items: &[(T, &str)],
    mut set: impl FnMut(T),
) {
    let label = crate::i18n::tr(label);
    ui.horizontal(|ui| {
        ui.add_sized(
            [52.0, 20.0],
            egui::Label::new(RichText::new(label).size(SIZE_LABEL).color(dim())),
        );
        for (value, text) in items {
            if ui
                .selectable_label(current == *value, crate::i18n::tr(text))
                .clicked()
            {
                set(*value);
            }
        }
    });
}

/// 外观与布局：把 L1 参数直接摊在用户面前。
///
/// 面板自己不存样式，所有控件写回 `state.appearance` / `state.layout`，
/// 帧末比一次快照，变了才重算色板并落盘 —— 全程序只有这一条写路径。
pub(super) fn look_window(state: &mut AppState, ctx: &Context) {
    if !state.look_open {
        return;
    }
    let before = (state.appearance, state.layout);
    let before_lang = state.lang;
    let mut open = state.look_open;
    egui::Window::new(crate::i18n::t!("外观与布局"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .anchor(Align2::RIGHT_TOP, Vec2::new(-14.0, 52.0))
        // 与插件市场同一个标题栏口径：深色底、无高亮描边。
        .title_frame(title_frame())
        .show(ctx, |ui| {
            // 定宽而不是让内容撑：锚在右上时，egui 会按内容回缩，回缩到比一行控件
            // 还窄就会把左边的选项挤到窗口外面去。
            ui.set_min_width(316.0);
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 5.0);

            look_section(ui, "语言 / Language");
            ui.horizontal_wrapped(|ui| {
                for p in [
                    crate::i18n::LangPref::System,
                    crate::i18n::LangPref::Zh,
                    crate::i18n::LangPref::En,
                ] {
                    // 「跟随系统」是唯一需要翻译的一项；中文与 English 用各自语言
                    // 自己的写法，不翻 —— 只会英文的人要能认出那一项。
                    let name = match p {
                        crate::i18n::LangPref::System => crate::i18n::t!("跟随系统"),
                        crate::i18n::LangPref::Zh => crate::i18n::Lang::Zh.label(),
                        crate::i18n::LangPref::En => crate::i18n::Lang::En.label(),
                    };
                    if ui.selectable_label(state.lang == p, name).clicked() && state.lang != p {
                        state.lang = p;
                        // 立即生效：之后的绘制就走新语言。落盘交给帧末那条统一路径
                        // （见函数末尾），这里不自己写文件。
                        crate::i18n::set(p.resolve());
                    }
                }
            });

            look_section(ui, "明暗");
            ui.horizontal_wrapped(|ui| {
                for m in crate::settings::THEME_ORDER {
                    if ui
                        .selectable_label(state.appearance.theme == m, m.cn())
                        .clicked()
                    {
                        state.appearance.theme = m;
                    }
                }
            });

            look_section(ui, "皮肤 · 中性色倾向");
            ui.horizontal_wrapped(|ui| {
                for skin in Skin::ALL {
                    if ui
                        .selectable_label(state.appearance.skin == skin, skin.cn())
                        .clicked()
                    {
                        state.appearance.skin = skin;
                    }
                }
            });

            look_section(ui, "主色");
            ui.horizontal_wrapped(|ui| {
                for (h, s, l, name) in theme::ACCENT_PRESETS {
                    let col = theme::hsl(h, s, l);
                    let active = (state.appearance.accent_h - h).abs() < 1.0
                        && (state.appearance.accent_s - s).abs() < 2.0
                        && (state.appearance.accent_l - l).abs() < 2.0;
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                    let c = rect.center();
                    ui.painter().circle_filled(c, 8.0, col);
                    if active {
                        ui.painter()
                            .circle_stroke(c, 10.0, Stroke::new(1.5, text()));
                    }
                    if resp.clicked() {
                        state.appearance.accent_h = h;
                        state.appearance.accent_s = s;
                        state.appearance.accent_l = l;
                    }
                    resp.on_hover_text(name);
                }
                // 任意取色：给的是 RGB，换算成 HSL 再存，深浅档仍能由参数派生。
                let mut rgb = theme::accent_rgb();
                if ui.color_edit_button_srgb(&mut rgb).changed() {
                    let (h, s, l) = theme::rgb_to_hsl(rgb[0], rgb[1], rgb[2]);
                    state.appearance.accent_h = h;
                    state.appearance.accent_s = s;
                    state.appearance.accent_l = l;
                }
            });

            look_section(ui, "外壳");
            look_row(
                ui,
                "平台",
                state.appearance.os,
                &[
                    (platform::OsChoice::Auto, "跟随系统"),
                    (platform::OsChoice::Win, "Windows"),
                    (platform::OsChoice::Mac, "macOS"),
                ],
                |v| state.appearance.os = v,
            );

            look_section(ui, "形状与字号");
            ui.horizontal(|ui| {
                ui.add_sized(
                    [52.0, 20.0],
                    egui::Label::new(
                        RichText::new(crate::i18n::t!("圆角"))
                            .size(SIZE_LABEL)
                            .color(dim()),
                    ),
                );
                ui.add(
                    egui::Slider::new(&mut state.appearance.radius, 0.0..=16.0)
                        .max_decimals(0)
                        .suffix(" px"),
                );
            });
            // 密度：只动调整面板的行高，外壳骨架高度不跟——那是界面稿钉死的。
            let mut density = state.appearance.density;
            look_row(
                ui,
                "密度",
                density,
                &[
                    (theme::Density::Compact, "紧凑"),
                    (theme::Density::Standard, "标准"),
                    (theme::Density::Loose, "宽松"),
                ],
                |v| density = v,
            );
            state.appearance.density = density;
            let mut level = state.font_level;
            look_row(
                ui,
                "字号",
                level,
                &[(0, "小"), (1, "标准"), (2, "大"), (3, "特大")],
                |v| level = v,
            );
            if level != state.font_level {
                state.font_level = level;
                theme::set_fonts(ctx); // 字号档位变了才重装字体
            }

            look_section(ui, "布局");
            let preset = state.layout.preset();
            look_row(
                ui,
                "预设",
                preset,
                &[
                    (Some(crate::layout::LayoutPreset::Classic), "经典"),
                    (Some(crate::layout::LayoutPreset::Focus), "专注"),
                    (Some(crate::layout::LayoutPreset::Min), "极简"),
                ],
                |p| {
                    if let Some(p) = p {
                        state.layout = state.layout.with_preset(p);
                    }
                },
            );
            look_row(
                ui,
                "状态栏",
                state.layout.status,
                &[
                    (StatusSlot::Bottom, "底部"),
                    (StatusSlot::Top, "顶部"),
                    (StatusSlot::Hidden, "隐藏"),
                ],
                |v| state.layout.status = v,
            );
            look_row(
                ui,
                "工具轨",
                state.layout.rail,
                &[
                    (crate::layout::Side::Left, "左"),
                    (crate::layout::Side::Right, "右"),
                ],
                |v| state.layout.rail = v,
            );
            look_row(
                ui,
                "调整面板",
                state.layout.dock_side,
                &[
                    (crate::layout::Side::Right, "右"),
                    (crate::layout::Side::Left, "左"),
                ],
                |v| state.layout.dock_side = v,
            );
            look_row(
                ui,
                "胶片条",
                state.layout.film,
                &[
                    (crate::layout::FilmSlot::Shown, "显示"),
                    (crate::layout::FilmSlot::Hidden, "隐藏"),
                ],
                |v| state.layout.film = v,
            );

            ui.add_space(8.0);
            soft_sep(ui);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new(crate::i18n::t!("重置")).size(SIZE_BODY))
                    .clicked()
                {
                    state.appearance = Appearance::default();
                    state.layout = crate::layout::Layout::default();
                }
                if ui
                    .button(RichText::new(crate::i18n::t!("完成")).size(SIZE_BODY))
                    .clicked()
                {
                    state.look_open = false;
                }
                ui.label(
                    RichText::new(crate::i18n::t!("改动即时生效并记住"))
                        .size(10.0)
                        .color(dim()),
                );
            });
        });
    state.look_open = open;

    if (state.appearance, state.layout) != before {
        theme::set_appearance(state.appearance);
        theme::apply_style(ctx, FONT_ZOOMS[state.font_level.min(FONT_ZOOMS.len() - 1)]);
        crate::settings::save(&state.settings());
    } else if state.lang != before_lang {
        // 只换了语言：色板与样式不用重算，但设置文件必须落盘 —— 否则下次启动
        // 又回到旧语言，用户会以为切换没生效。仍然只有这一条写路径。
        crate::settings::save(&state.settings());
    }
}

pub(super) fn about_window(state: &mut AppState, ctx: &Context) {
    if !state.about_open {
        return;
    }
    let mut open = true;
    egui::Window::new(crate::i18n::t!("快捷键与关于"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(430.0)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .title_frame(title_frame())
        .show(ctx, |ui| {
            ui.label(
                RichText::new(aevum_core::brand::TITLE)
                    .size(15.0)
                    .color(text())
                    .strong(),
            );
            ui.label(
                RichText::new(crate::i18n::t!("Rust + egui · 核心数值以 Python 金标为准"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
            ui.add_space(8.0);
            egui::Grid::new("shortcut-grid")
                .num_columns(2)
                .spacing([20.0, 3.0])
                .show(ui, |ui| {
                    for (key, desc) in [
                        ("Ctrl+O", "打开序列"),
                        ("Ctrl+Shift+O", "选照片（可多张）"),
                        ("Ctrl+E", "导出当前帧"),
                        ("Ctrl+Z", "撤销"),
                        ("Ctrl+Shift+Z / Ctrl+Y", "重做"),
                        ("Ctrl+0 / Ctrl+1", "适应窗口 / 实际像素"),
                        ("Ctrl+Alt+1…6", "切换模块"),
                        ("H / Z / C", "抓手 / 缩放 / 裁剪"),
                        ("G / \\", "网格 / 前后对比"),
                        ("空格", "播放 / 暂停"),
                        ("← →", "逐帧（Shift 一次十帧）"),
                        ("1–5 / 0", "星级 / 清除星级"),
                        ("6–9", "红黄绿蓝色标"),
                        ("P / X", "选用 / 排除"),
                        ("F", "适应窗口"),
                    ] {
                        ui.label(
                            RichText::new(key)
                                .size(SIZE_SMALL)
                                .monospace()
                                .color(accent()),
                        );
                        ui.label(RichText::new(desc).size(SIZE_SMALL).color(muted()));
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            soft_sep(ui);
            ui.horizontal(|ui| {
                ui.label(theme::caps("GOLDEN 15/15 · 1e-4"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .button(RichText::new(crate::i18n::t!("知道了")).size(SIZE_SMALL))
                        .clicked()
                    {
                        state.about_open = false;
                    }
                });
            });
        });
    if !open {
        state.about_open = false;
    }
}
