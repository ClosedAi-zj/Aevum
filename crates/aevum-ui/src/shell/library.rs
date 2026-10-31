//! 图库：目录树/筛选/元数据/目录视图/堆叠与关键字应用。

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::UV;
use super::common::note;
use super::menu::menu_style;
use super::tabs::meta_editor;
use super::timeline::label_color;
use super::toolbar::bar_button;
use crate::icons::{self, Glyph};
use crate::state::{AppState, LABEL_NAMES};
use crate::theme::{self, *};
use egui::{
    Align, Align2, Color32, Context, Key, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
    Ui, UiBuilder, Vec2,
};
use std::path::PathBuf;
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

// ---------------------------------------------------------------- 图库

/// 图库面板：根目录清单 + 手动集合 → 点开看编目（缩略图 + 可编辑的评级/旗标/色标）。
/// 双击照片或「打开」把那个目录开成序列。整理状态写回各目录自己的工程文件，
/// 图库不留第二份真相；挑出来的照片用集合跨目录收在一起，成员也只记路径。
pub(super) fn library_tab(state: &mut AppState, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let p = ui.painter().clone();
    theme::rule(
        &p,
        ui.cursor().left(),
        ui.cursor().right(),
        ui.cursor().top(),
        border(),
    );
    ui.add_space(6.0);
    if state.lib_col.is_some() {
        collection_view(state, ui, &ctx);
    } else if let Some(index) = state.lib_sel {
        catalog_view(state, ui, &ctx, index);
    } else {
        roots_view(state, ui, &ctx);
    }
}

/// 起始页：集合区 + 根目录清单。
fn roots_view(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    ui.label(egui::RichText::new(crate::i18n::t!("集合")).strong());
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.lib_col_name)
                .hint_text(crate::i18n::t!("集合名"))
                .desired_width(108.0),
        );
        if ui
            .button(crate::i18n::t!("＋ 创建"))
            .on_hover_text(crate::i18n::t!(
                "把挑出来的照片收进一个跨目录集合；成员只记路径，照片留在原处"
            ))
            .clicked()
        {
            let name = state.lib_col_name.clone();
            state.lib_collection_create(&name);
            state.lib_col_name.clear();
        }
    });
    ui.add_space(2.0);
    let mut i = 0;
    while i < state.lib_cols.len() {
        let (name, count) = (
            state.lib_cols[i].name.clone(),
            state.lib_cols[i].files.len(),
        );
        let mut deleted = false;
        ui.horizontal(|ui| {
            let open = ui
                .add(
                    egui::Label::new(egui::RichText::new(&name).strong())
                        .truncate()
                        .sense(Sense::click()),
                )
                .on_hover_text(crate::i18n::t!("点开看集合成员"));
            ui.label(egui::RichText::new(format!("{count} 张")).weak());
            if open.clicked() {
                state.lib_col = Some(i);
                state.lib_sel = None;
                state.lib_sel_file = None;
            }
            if ui
                .button("删除")
                .on_hover_text(crate::i18n::t!("删集合本身，不动照片与各目录工程"))
                .clicked()
            {
                state.lib_collection_delete(i);
                deleted = true;
            }
        });
        if deleted {
            continue;
        }
        i += 1;
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::t!("目录")).strong());
        if ui
            .button(crate::i18n::t!("＋ 添加"))
            .on_hover_text(crate::i18n::t!(
                "把一个素材目录加进图库（清单存到漫游配置目录，跨会话常驻）"
            ))
            .clicked()
            && let Some(dir) = rfd::FileDialog::new().pick_folder()
        {
            state.lib_add(&dir);
        }
    });
    ui.add_space(4.0);
    if state.lib_roots.is_empty() {
        note(
            ui,
            "图库还是空的。把几个素材目录加进来，跨目录浏览、评分与挑选。",
        );
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("library-roots")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut i = 0;
            while i < state.lib_roots.len() {
                let (dir, summary) = {
                    let dir = state.lib_roots[i].clone();
                    let summary = match state.lib_entries.get(i) {
                        Some(files) if !files.is_empty() => aevum_core::library::summarize(files),
                        _ => String::new(),
                    };
                    (dir, summary)
                };
                let name = dir
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| dir.display().to_string());
                let mut removed = false;
                ui.horizontal(|ui| {
                    let name_resp = ui
                        .add(
                            egui::Label::new(egui::RichText::new(&name).strong())
                                .truncate()
                                .sense(Sense::click()),
                        )
                        .on_hover_text(format!("{}\n点开看这个目录的编目", dir.display()));
                    if !summary.is_empty() {
                        ui.label(egui::RichText::new(&summary).weak());
                    }
                    if ui
                        .button("打开")
                        .on_hover_text(crate::i18n::t!("把这个目录开成序列"))
                        .clicked()
                    {
                        state.open(ctx, dir.clone());
                    }
                    if ui
                        .button("移除")
                        .on_hover_text(crate::i18n::t!("从图库清单移除（不动磁盘上的文件）"))
                        .clicked()
                    {
                        state.lib_remove(i);
                        removed = true;
                    }
                    if name_resp.clicked() {
                        state.lib_scan(i);
                    }
                });
                if removed {
                    continue;
                }
                i += 1;
            }
        });
}

/// 筛选条：星级下限、只看选用、色标、关键词；右边给出「留下的 / 总数」。
fn lib_filter_bar(
    state: &mut AppState,
    ui: &mut Ui,
    total: usize,
    shown: usize,
    keywords: &[(String, usize)],
) {
    let p = ui.painter().clone();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::t!("筛选")).weak());
        for s in 1..=5u8 {
            let on = state.lib_filter.stars >= s;
            let resp = ui
                .add(
                    egui::Button::new(egui::RichText::new("★").size(11.0).color(if on {
                        star()
                    } else {
                        dim()
                    }))
                    .frame(false),
                )
                .on_hover_text(format!("只看 ≥{s} 星；点已亮的星退回一档"));
            if resp.clicked() {
                state.lib_filter.stars = if state.lib_filter.stars == s {
                    s - 1
                } else {
                    s
                };
            }
        }
        let picked = state.lib_filter.picked;
        if ui
            .selectable_label(picked, "选用")
            .on_hover_text(crate::i18n::t!("只看旗标为「选用」的"))
            .clicked()
        {
            state.lib_filter.picked = !picked;
        }
        for l in 1..=4u8 {
            let on = state.lib_filter.label == l;
            let (r, resp) = ui.allocate_exact_size(Vec2::new(15.0, 15.0), Sense::click());
            let color = label_color(l);
            if on {
                p.circle_filled(r.center(), 5.0, color);
            } else {
                p.circle_stroke(r.center(), 4.5, Stroke::new(1.0, color.gamma_multiply(0.7)));
            }
            let resp = resp.on_hover_text(format!("只看{}色标", LABEL_NAMES[l as usize]));
            if resp.clicked() {
                state.lib_filter.label = if on { 0 } else { l };
            }
        }
        if state.lib_filter.active() && ui.small_button("清除").clicked() {
            state.lib_filter = aevum_core::library::Filter::default();
        }
        ui.label(egui::RichText::new(format!("{shown} / {total}")).weak());
    });
    // 词表可能很长：单独一行，别把星级与色标挤出面板。
    if !keywords.is_empty() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::t!("词")).weak());
            let active = state.lib_filter.keyword.clone();
            let label = if active.is_empty() {
                "全部".to_owned()
            } else {
                active.clone()
            };
            ui.menu_button(egui::RichText::new(label).size(11.0), |ui| {
                if ui.button(crate::i18n::t!("全部")).clicked() {
                    state.lib_filter.keyword.clear();
                    ui.close();
                }
                for (word, count) in keywords {
                    let on = *word == active;
                    if ui
                        .selectable_label(on, format!("{word}  {count}"))
                        .clicked()
                    {
                        state.lib_filter.keyword = if on { String::new() } else { word.clone() };
                        ui.close();
                    }
                }
            });
            if !active.is_empty() && ui.small_button("×").clicked() {
                state.lib_filter.keyword.clear();
            }
        });
    }
}

/// 一个目录的说明与版权：读写的是该目录 .aevum/project.json 里的 metadata。
/// 切目录时先重载缓冲，免得把上一个目录没存的值写进这一个目录的工程文件。
fn lib_meta_panel(state: &mut AppState, ui: &mut Ui, index: usize) {
    let dir = state.lib_roots.get(index).cloned().unwrap_or_default();
    if state.lib_meta_root.as_deref() != Some(dir.as_path()) {
        state.lib_load_meta(index);
    }
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        meta_editor(ui, &mut state.lib_meta);
        ui.horizontal(|ui| {
            if bar_button(ui, "存").clicked()
                && let Err(e) = state.lib_save_meta()
            {
                state.error = Some(e);
            }
            if bar_button(ui, "重新载入").clicked() {
                state.lib_load_meta(index);
                state.status = "已重新读取这个目录的说明与版权".to_owned();
            }
            let filled = state.lib_meta.present().len();
            if filled > 0 {
                ui.label(
                    RichText::new(format!("已填 {filled} 项"))
                        .size(SIZE_SMALL)
                        .color(accent()),
                );
            }
        });
        note(
            ui,
            "写进这个目录的工程文件；导出时随静帧进 EXIF、随成片进容器标签。",
        );
    });
    ui.add_space(4.0);
}

/// 一个目录的编目：筛选条 + 逐行列表。行内改评级/旗标/色标当场写回工程文件。
fn catalog_view(state: &mut AppState, ui: &mut Ui, ctx: &Context, index: usize) {
    let dir = state.lib_roots.get(index).cloned().unwrap_or_default();
    ui.horizontal(|ui| {
        if ui.button(crate::i18n::t!("← 目录")).clicked() {
            state.lib_sel = None;
            state.lib_sel_file = None;
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    dir.file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| dir.display().to_string()),
                )
                .strong(),
            )
            .truncate(),
        );
        if ui.button(crate::i18n::t!("重扫")).clicked() {
            state.lib_scan(index);
        }
        let open = state.lib_meta_open;
        if ui
            // ▲▼ 在嵌入字体的字形表里，▴▾ 不在（会出豆腐块）。
            .button(if open { "信息 ▲" } else { "信息 ▼" })
            .on_hover_text(crate::i18n::t!("这个目录的说明与版权信息"))
            .clicked()
        {
            state.lib_meta_open = !open;
        }
    });
    ui.add_space(4.0);
    // 信息面板放在「有没有帧」的判断之前：一个空目录也该能写版权信息。
    if state.lib_meta_open {
        lib_meta_panel(state, ui, index);
    }
    // 只拿索引与可见行的克隆：编目上万帧时也不该每帧深拷贝整本。
    let total = state.lib_entries.get(index).map(|v| v.len()).unwrap_or(0);
    if total == 0 {
        note(ui, "这个目录里没有可用的帧，或还没编目。");
        return;
    }
    // 堆叠：折叠时一叠只露头一张（叠里最先通过筛选的那张），展开才逐张列出。
    let (visible, stack_counts): (Vec<usize>, Vec<usize>) = {
        let entries = &state.lib_entries[index];
        let widest = entries
            .iter()
            .filter_map(|f| f.stack)
            .max()
            .map_or(0, |s| s + 1);
        let mut counts = vec![0usize; widest];
        for f in entries {
            if let Some(s) = f.stack
                && s < widest
            {
                counts[s] += 1;
            }
        }
        let mut rows = Vec::new();
        let mut head_shown: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for (i, f) in entries.iter().enumerate() {
            if !state.lib_filter.keeps(f) {
                continue;
            }
            if let Some(s) = f.stack
                && !state.lib_stack_open.contains(&(index, s))
                && !head_shown.insert(s)
            {
                continue;
            }
            rows.push(i);
        }
        (rows, counts)
    };
    let keywords = state
        .lib_entries
        .get(index)
        .map(|files| aevum_core::library::keyword_counts(files))
        .unwrap_or_default();
    lib_filter_bar(state, ui, total, visible.len(), &keywords);
    if visible.is_empty() {
        note(ui, "筛选后没有剩下的照片。");
        return;
    }
    // 被筛掉的行不能留在选择里，否则键盘会改到看不见的照片。
    state
        .lib_checked
        .retain(|row| visible.binary_search(row).is_ok());
    if !state.lib_checked.is_empty() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("已选 {} 张", state.lib_checked.len()))
                    .size(11.0)
                    .color(accent()),
            );
            for i in 0..state.lib_cols.len() {
                let name = state.lib_cols[i].name.clone();
                if ui.small_button(format!("→ {name}")).clicked() {
                    let paths: Vec<PathBuf> = state
                        .lib_checked
                        .iter()
                        .filter_map(|&row| state.lib_entries[index].get(row))
                        .map(|e| e.path.clone())
                        .collect();
                    let added = state.lib_collection_put_many(i, &paths);
                    state.status = format!("「{name}」新收 {added} 张");
                }
            }
            if state.lib_checked.len() >= 2
                && ui
                    .small_button("并成一叠")
                    .on_hover_text(crate::i18n::t!(
                        "把勾选的照片折叠成一叠；右键「堆叠」可拆散"
                    ))
                    .clicked()
            {
                match state.lib_stack_targets() {
                    Ok(n) => state.status = format!("已并成一叠（{n} 张）"),
                    Err(e) => state.error = Some(e),
                }
            }
            if ui.small_button("清除").clicked() {
                state.lib_checked.clear();
            }
        });
    }
    if state.lib_sel_file.is_some() {
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.lib_keyword)
                    .hint_text(crate::i18n::t!("关键词，逗号分隔"))
                    .desired_width(120.0),
            );
            let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if ui.small_button("＋ 加词").clicked() || entered {
                let words = split_words(&state.lib_keyword);
                if !words.is_empty() {
                    lib_apply_keywords(state, &words, &[]);
                    state.lib_keyword.clear();
                }
            }
        });
    }
    ui.label(
        egui::RichText::new(
            "单击选中；Ctrl 加选、Shift 连选；0~5 评星、P/X 旗标、6~9 色标；右键集合、关键词与堆叠",
        )
        .size(11.0)
        .color(dim()),
    );
    const ROW_H: f32 = 72.0;
    egui::ScrollArea::vertical()
        .id_salt("library-files")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, visible.len(), |ui, range| {
            let paths: Vec<PathBuf> = {
                let entries = &state.lib_entries[index];
                visible[range.clone()]
                    .iter()
                    .map(|&i| entries[i].path.clone())
                    .collect()
            };
            state.lib_ensure_thumb_paths(ctx, paths);
            for vi in range {
                let row = visible[vi];
                let f = state.lib_entries[index][row].clone();
                // 叠头才挂角标：一叠几张、当前展开没有，点它切换折叠。
                let badge = f.stack.map(|s| StackBadge {
                    count: stack_counts.get(s).copied().unwrap_or(0),
                    open: state.lib_stack_open.contains(&(index, s)),
                });
                let selected = state.lib_sel_file == Some(row);
                let checked = state.lib_checked.contains(&row);
                // 行响应要自己有 click 感：allocate_ui 给的是 hover，点不中整行。
                // 先占位注册整行，子控件后加、在重叠处优先，星标与色标照旧能点。
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), ROW_H - 8.0),
                    Sense::click(),
                );
                let mut row_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                library_row(
                    state,
                    &mut row_ui,
                    &f,
                    Some(index),
                    selected,
                    checked,
                    badge,
                );
                if resp.clicked() {
                    let (ctrl, shift) =
                        ui.input(|i| (i.modifiers.ctrl || i.modifiers.command, i.modifiers.shift));
                    state.lib_click(row, ctrl, shift);
                }
                if resp.double_clicked() {
                    state.open(ctx, f.path.clone());
                }
                let targets: Vec<usize> = if state.lib_checked.contains(&row) {
                    state.lib_checked.iter().copied().collect()
                } else {
                    vec![row]
                };
                let f2 = f.clone();
                resp.context_menu(|ui| {
                    menu_style(ui);
                    lib_row_menu(state, ui, index, &f2, &targets, &keywords);
                });
                resp.on_hover_text(format!("{}\n双击打开这个目录", f.path.display()));
            }
        });
}

/// 行右键菜单：加入集合、评级、旗标、色标。`frames` 是这次要一起改的行
/// （右键点在勾选行上就作用于全部勾选，否则只作用于这一行）。
fn lib_row_menu(
    state: &mut AppState,
    ui: &mut Ui,
    root: usize,
    f: &aevum_core::library::LibFile,
    frames: &[usize],
    keywords: &[(String, usize)],
) {
    let title = if frames.len() > 1 {
        format!("{} 等 {} 张", f.name, frames.len())
    } else {
        f.name.clone()
    };
    ui.label(egui::RichText::new(title).size(SIZE_SMALL).color(dim()));
    ui.menu_button(crate::i18n::t!("加入集合"), |ui| {
        if state.lib_cols.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::t!("还没有集合，先在面板顶部建一个")).weak());
        }
        for i in 0..state.lib_cols.len() {
            let name = state.lib_cols[i].name.clone();
            if ui.button(name).clicked() {
                let paths: Vec<PathBuf> = frames
                    .iter()
                    .filter_map(|&frame| state.lib_entries.get(root)?.get(frame))
                    .map(|e| e.path.clone())
                    .collect();
                state.lib_collection_put_many(i, &paths);
                ui.close();
            }
        }
    });
    ui.menu_button(crate::i18n::t!("关键词"), |ui| {
        if !f.keywords.is_empty() {
            for word in &f.keywords {
                if ui.button(format!("－ {word}")).clicked() {
                    lib_apply_keywords_frames(state, root, frames, &[], std::slice::from_ref(word));
                    ui.close();
                }
            }
            ui.separator();
        }
        let addable: Vec<&String> = keywords
            .iter()
            .map(|(word, _)| word)
            .filter(|word| !f.keywords.contains(*word))
            .collect();
        if addable.is_empty() {
            ui.label(
                egui::RichText::new(crate::i18n::t!("没有可快速添加的词；用面板里的输入框"))
                    .size(10.0)
                    .weak(),
            );
        }
        for word in addable {
            if ui.button(format!("＋ {word}")).clicked() {
                lib_apply_keywords_frames(state, root, frames, std::slice::from_ref(word), &[]);
                ui.close();
            }
        }
    });
    for stars in 1..=5u8 {
        if ui.button(format!("{stars} 星")).clicked() {
            lib_apply_frames(
                state,
                root,
                frames,
                aevum_core::library::Organize {
                    rating: Some(stars),
                    ..Default::default()
                },
            );
            ui.close();
        }
    }
    if ui.button(crate::i18n::t!("清除星级")).clicked() {
        lib_apply_frames(
            state,
            root,
            frames,
            aevum_core::library::Organize {
                rating: Some(0),
                ..Default::default()
            },
        );
        ui.close();
    }
    ui.separator();
    for (label, flag) in [("标记为选用", 1i8), ("标记为排除", -1)] {
        if ui.button(label).clicked() {
            lib_apply_frames(
                state,
                root,
                frames,
                aevum_core::library::Organize {
                    flag: Some(flag),
                    ..Default::default()
                },
            );
            ui.close();
        }
    }
    ui.separator();
    for l in 1..=4u8 {
        if ui
            .button(format!("{}色标", LABEL_NAMES[l as usize]))
            .clicked()
        {
            lib_apply_frames(
                state,
                root,
                frames,
                aevum_core::library::Organize {
                    label: Some(l),
                    ..Default::default()
                },
            );
            ui.close();
        }
    }
    ui.separator();
    ui.menu_button(crate::i18n::t!("堆叠"), |ui| {
        if frames.len() >= 2 {
            if ui
                .button(format!("把这 {} 张并成一叠", frames.len()))
                .clicked()
            {
                let r = state.lib_stack_create(root, frames);
                lib_stack_act(state, r, "已并成一叠");
                ui.close();
            }
        } else {
            ui.label(
                egui::RichText::new(crate::i18n::t!("勾住两张以上才能并叠"))
                    .size(10.0)
                    .weak(),
            );
        }
        if let Some(s) = f.stack
            && ui.button(format!("拆散第 {} 叠", s + 1)).clicked()
        {
            let r = state.lib_stack_dissolve(root, s);
            lib_stack_act(state, r, "已拆散这一叠");
            ui.close();
        }
        ui.separator();
        if ui.button(crate::i18n::t!("按文件名自动分叠")).clicked() {
            let r = state.lib_stack_auto(root);
            lib_stack_act(state, r, "自动分叠");
            ui.close();
        }
        let count = state
            .lib_entries
            .get(root)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| e.stack)
                    .max()
                    .map_or(0, |s| s + 1)
            })
            .unwrap_or(0);
        if count > 0 {
            if ui.button(crate::i18n::t!("全部展开")).clicked() {
                state.lib_stack_open.extend((0..count).map(|s| (root, s)));
                ui.close();
            }
            if ui.button(crate::i18n::t!("全部收起")).clicked() {
                state.lib_stack_open.retain(|&(r, _)| r != root);
                ui.close();
            }
        }
    });
}

/// 堆叠操作的统一收尾：写盘成功报叠数，失败进错误条（写盘失败时内存编目没动过）。
fn lib_stack_act(state: &mut AppState, r: Result<Vec<Vec<usize>>, String>, what: &str) {
    match r {
        Ok(stacks) => {
            let frames: usize = stacks.iter().map(|g| g.len()).sum();
            state.status = format!("{what}：这个目录现在 {} 叠共 {frames} 张", stacks.len());
        }
        Err(e) => state.error = Some(e),
    }
}

/// 集合视图：成员按路径列，能在已编目的目录里对上就带评级与帧号。
fn collection_view(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let Some(col) = state.lib_col else { return };
    let Some(collection) = state.lib_cols.get(col).cloned() else {
        state.lib_col = None;
        return;
    };
    ui.horizontal(|ui| {
        if ui.button(crate::i18n::t!("← 集合")).clicked() {
            state.lib_col = None;
        }
        ui.add(egui::Label::new(egui::RichText::new(&collection.name).strong()).truncate());
        ui.label(egui::RichText::new(format!("{} 张", collection.files.len())).weak());
    });
    ui.add_space(4.0);
    let rows = state.lib_collection_rows(col);
    if rows.is_empty() {
        note(
            ui,
            "集合还是空的。展开一个目录，选中照片后在右键菜单里「加入集合」。",
        );
        return;
    }
    const ROW_H: f32 = 72.0;
    egui::ScrollArea::vertical()
        .id_salt("library-collection")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            let paths: Vec<PathBuf> = rows[range.clone()].iter().map(|f| f.path.clone()).collect();
            state.lib_ensure_thumb_paths(ctx, paths);
            for vi in range {
                let f = rows[vi].clone();
                // 成员能不能写回，取决于它所在的目录在不在图库编目里。
                let root_of = state
                    .lib_entries
                    .iter()
                    .position(|files| files.iter().any(|e| e.path == f.path));
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), ROW_H - 8.0),
                    Sense::click(),
                );
                let mut row_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                library_row(state, &mut row_ui, &f, root_of, false, false, None);
                if resp.double_clicked() {
                    state.open(ctx, f.path.clone());
                }
                let f2 = f.clone();
                resp.context_menu(|ui| {
                    menu_style(ui);
                    ui.label(egui::RichText::new(&f2.name).size(SIZE_SMALL).color(dim()));
                    if ui.button(crate::i18n::t!("从集合移除")).clicked() {
                        state.lib_collection_drop(col, &f2.path);
                        ui.close();
                    }
                    if ui.button(crate::i18n::t!("打开所在目录")).clicked() {
                        if let Some(dir) = f2.path.parent() {
                            state.open(ctx, dir.to_path_buf());
                        }
                        ui.close();
                    }
                    ui.separator();
                    for stars in 1..=5u8 {
                        if ui.button(format!("{stars} 星")).clicked() {
                            if let Some(root) = root_of {
                                lib_apply(
                                    state,
                                    root,
                                    f2.index,
                                    aevum_core::library::Organize {
                                        rating: Some(stars),
                                        ..Default::default()
                                    },
                                );
                            }
                            ui.close();
                        }
                    }
                });
                resp.on_hover_text(f.path.display().to_string());
            }
        });
}

fn lib_apply(
    state: &mut AppState,
    root: usize,
    frame: usize,
    patch: aevum_core::library::Organize,
) {
    if let Err(e) = state.lib_organize(root, frame, patch) {
        state.error = Some(e);
    }
}

/// 一次改一批帧，整批只落一次盘。
fn lib_apply_frames(
    state: &mut AppState,
    root: usize,
    frames: &[usize],
    patch: aevum_core::library::Organize,
) {
    if let Err(e) = state.lib_organize_many(root, frames, patch) {
        state.error = Some(e);
    }
}

/// 键盘改的是当前目标行（勾选优先，否则选中行）。
pub(super) fn lib_apply_targets(state: &mut AppState, patch: aevum_core::library::Organize) {
    if let Err(e) = state.lib_organize_targets(patch) {
        state.error = Some(e);
    }
}

/// 面板输入框里的一串词：英文逗号、中文逗号、分号都拆。
fn split_words(raw: &str) -> Vec<String> {
    raw.split([',', '，', ';', '；'])
        .filter_map(aevum_core::library::clean_keyword)
        .collect()
}

fn lib_apply_keywords(state: &mut AppState, add: &[String], remove: &[String]) {
    if let Err(e) = state.lib_keyword_edit(add, remove) {
        state.error = Some(e);
    }
}

fn lib_apply_keywords_frames(
    state: &mut AppState,
    root: usize,
    frames: &[usize],
    add: &[String],
    remove: &[String],
) {
    if let Err(e) = state.lib_keyword_edit_many(root, frames, add, remove) {
        state.error = Some(e);
    }
}

/// 编目里的一行：缩略图 + 文件名 + 可点的星级/旗标/色标。
/// 叠头角标：这一叠几张、当前展开没有。只在编目视图里给，集合视图没有"叠"。
struct StackBadge {
    count: usize,
    open: bool,
}

fn library_row(
    state: &mut AppState,
    ui: &mut Ui,
    f: &aevum_core::library::LibFile,
    root: Option<usize>,
    selected: bool,
    checked: bool,
    badge: Option<StackBadge>,
) {
    let p = ui.painter().clone();
    let r = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), 64.0));
    if selected || checked {
        p.rect_filled(r, radius_control(), accent_dim());
    }
    if checked {
        p.rect_stroke(
            r,
            radius_control(),
            Stroke::new(1.0, accent()),
            StrokeKind::Inside,
        );
    }
    ui.horizontal(|ui| {
        const THUMB: f32 = 60.0;
        let (trect, _) = ui.allocate_exact_size(Vec2::splat(THUMB), Sense::hover());
        p.rect_filled(trect, radius_control(), canvas());
        if let Some(t) = state.lib_thumbs.get(&f.path) {
            let (tw, th) = (t.size[0] as f32, t.size[1] as f32);
            let scale = (trect.width() / tw).min(trect.height() / th);
            let img = Rect::from_center_size(trect.center(), Vec2::new(tw * scale, th * scale));
            p.image(t.texture.id(), img, UV, Color32::WHITE);
        } else {
            icons::paint(
                &p,
                Rect::from_center_size(trect.center(), Vec2::splat(18.0)),
                Glyph::Frame,
                dim(),
            );
        }
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if let (Some(badge), Some(root), Some(s)) = (badge, root, f.stack) {
                    // 角标是自绘的：▸▾ 这类小三角不在嵌入字体的字形表里，会出豆腐块。
                    let (brect, bresp) =
                        ui.allocate_exact_size(Vec2::new(28.0, 15.0), Sense::click());
                    if bresp.hovered() {
                        p.rect_filled(brect, radius_control(), elev());
                    }
                    icons::paint(
                        &p,
                        Rect::from_center_size(
                            Pos2::new(brect.left() + 7.0, brect.center().y),
                            Vec2::splat(9.0),
                        ),
                        if badge.open {
                            Glyph::ChevronDown
                        } else {
                            Glyph::ChevronRight
                        },
                        if bresp.hovered() { text() } else { accent() },
                    );
                    p.text(
                        Pos2::new(brect.left() + 14.0, brect.center().y),
                        Align2::LEFT_CENTER,
                        badge.count.to_string(),
                        font(SIZE_SMALL),
                        accent(),
                    );
                    let verb = if badge.open { "收起" } else { "展开" };
                    let clicked = bresp.clicked();
                    bresp.on_hover_text(format!("{verb}这一叠（共 {} 张）", badge.count));
                    if clicked && !state.lib_stack_open.insert((root, s)) {
                        state.lib_stack_open.remove(&(root, s));
                    }
                }
                ui.add(egui::Label::new(egui::RichText::new(&f.name).size(12.0)).truncate());
            });
            ui.horizontal(|ui| {
                for s in 1..=5u8 {
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::click());
                    icons::paint(
                        &p,
                        r,
                        if s <= f.rating {
                            Glyph::Star
                        } else {
                            Glyph::StarEmpty
                        },
                        if s <= f.rating { star() } else { dim() },
                    );
                    if resp.on_hover_text(format!("评 {s} 星")).clicked()
                        && let Some(root) = root
                    {
                        let rating = if f.rating == s { 0 } else { s };
                        lib_apply(
                            state,
                            root,
                            f.index,
                            aevum_core::library::Organize {
                                rating: Some(rating),
                                ..Default::default()
                            },
                        );
                    }
                }
                let (fr, fresp) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::click());
                icons::paint(
                    &p,
                    fr,
                    if f.flag < 0 {
                        Glyph::Close
                    } else {
                        Glyph::Flag
                    },
                    match f.flag {
                        1 => ok(),
                        -1 => danger(),
                        _ => dim(),
                    },
                );
                if fresp
                    .on_hover_text(crate::i18n::t!("旗标：选用 / 未标记"))
                    .clicked()
                    && let Some(root) = root
                {
                    let flag = if f.flag == 1 { 0 } else { 1 };
                    lib_apply(
                        state,
                        root,
                        f.index,
                        aevum_core::library::Organize {
                            flag: Some(flag),
                            ..Default::default()
                        },
                    );
                }
                let (lr, lresp) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::click());
                if f.label > 0 {
                    p.circle_filled(lr.center(), 4.5, label_color(f.label));
                } else {
                    p.circle_stroke(lr.center(), 4.5, Stroke::new(1.0, dim()));
                }
                if lresp
                    .on_hover_text(crate::i18n::t!("色标：红→黄→绿→蓝→无"))
                    .clicked()
                    && let Some(root) = root
                {
                    lib_apply(
                        state,
                        root,
                        f.index,
                        aevum_core::library::Organize {
                            label: Some((f.label.min(4) + 1) % 5),
                            ..Default::default()
                        },
                    );
                }
                if !f.keywords.is_empty() {
                    let text = f
                        .keywords
                        .iter()
                        .map(|w| format!("#{w}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).size(10.0).color(accent()))
                            .truncate(),
                    )
                    .on_hover_text(crate::i18n::t!("右键「关键词」加词或去词；筛选条按词收窄"));
                }
            });
        });
    });
}

// ---------------------------------------------------------------- 关于

// ---------------------------------------------------------------- 插件

/// 菜单分组的中文标题。清单里写的是机器名（`nik` / `compute`），
/// 翻成人话是宿主的事 —— 让插件作者填中文，市场里就会出现五种叫法的同一个组。
pub(super) fn group_title(group: &str) -> &str {
    match group {
        "nik" => "Nik Collection",
        "compute" => "计算与合成",
        "adobe" => "Adobe",
        "other" => "其他",
        _ => group,
    }
}
