//! 几何 / 修复 / 光学 / 画幅四组。
//!
//! The geometry, retouch, optics and frame groups.

// 共用件（theme / i18n / 兄弟模块的函数）由 mod.rs 的 use 树统一带入。
//
// Shared pieces (theme, i18n, sibling helpers) come in through mod.rs's use tree.
use super::develop::{lens_profile_body, spec_field, sub_head};
use super::*;
/// 几何：透视校正。自动一路只修梯形（竖线收敛到一个画外消失点），不修旋转、
/// 不修枕形畸变——证据不足时宁可不动作，也不猜一个把画面撕歪的四边形。
///
/// Geometry: perspective correction. The automatic path fixes keystone only (vertical lines
/// converging to an off-frame vanishing point); it does not fix rotation or pincushion. With
/// insufficient evidence it does nothing rather than guess a quad that warps the frame.
pub(super) fn adjust_geometry(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "geometry", "几何", false) {
        // 卡中卡（设计稿 .grp）：整组一张圆角卡，组头与内容都在卡里。
        // egui::Frame 在内容之前铺底、之后补描边——即时模式唯一可靠的
        // "背景先行"路径（其余方案：后画盖先画、RAII 拿不到终点、上一帧
        // 矩形有时序毛刺，均已试过并否决）。
        //
        // Card-in-card (the design's .grp): one rounded card per group, header and
        // body inside. egui::Frame paints the fill before the content and the stroke
        // after -- the one reliable "background first" path in immediate mode.
        // (Rejected alternatives: painting later covers earlier so the fill must go
        // first; RAII cannot reach the body's end; last-frame rects lag a frame on
        // expand/collapse.)
        egui::Frame::NONE
            .fill(theme::chrome())
            .stroke(egui::Stroke::new(1.0, theme::divider()))
            .corner_radius((theme::radius_panel() - 4.0).max(2.0))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |_ui| false);
    }
    let mut auto = false;
    let mut clear = false;
    let mut corners = false;
    ui.horizontal(|ui| {
        auto = bar_button(ui, "自动直立")
            .on_hover_text(crate::i18n::t!(
                "找画面里的竖直主直线，把梯形拉正；只修上下透视，不修旋转与畸变"
            ))
            .clicked();
        // 「四角」是自动直立的兜底：自动解要求画面里真有一条像样的竖直主直线，室内
        // 广角、纯风景、对着墙拍的片子经常一条都没有。这时只能手动把四个角拖到画面的
        // 方角上——ACR 里这个入口叫 Transform 的角点手柄。开着时按钮变「完成」。
        //
        // "Corners" is the fallback for auto-upright: the automatic solve needs a decent
        // dominant vertical, and wide interiors, pure landscapes or anything shot against a
        // wall often have none. Then the four corners have to be dragged onto the square
        // object by hand -- the corner handles of Transform in ACR. While on, the chip reads
        // "Done".
        corners = bar_chip(
            ui,
            if state.upright_editing {
                "完成"
            } else {
                "四角"
            },
            state.upright_editing,
        )
        .on_hover_text(crate::i18n::t!(
            "拖四个角对齐画面里的方形物体；双击某个角放回原处"
        ))
        .clicked();
        clear = bar_button(ui, "清除")
            .on_hover_text(crate::i18n::t!("卸下透视校正，回到整幅画面"))
            .clicked();
    });
    if corners {
        let was = state.upright_editing;
        let now = state.upright_edit_toggle();
        if !was && !now {
            state.auto_note = Some(
                crate::i18n::t!("画布当前不是整幅（拉直 / 推近 / 裁剪中），四角抓不到；先复位")
                    .to_owned(),
            );
        }
    }
    if auto {
        state.auto_note = Some(match state.upright_auto() {
            Ok(()) => {
                crate::i18n::t!("已按竖直主直线拉正；四边形之外的一圈被裁掉了，这是校正的代价")
                    .to_owned()
            }
            Err(e) => e,
        });
    }
    let mut changed = false;
    if clear {
        changed |= state.upright_clear();
    }
    // 状态行报的是「上边比下边窄多少」：梯形校正的全部意义就在这个数上，比四个角点
    // 的坐标好读得多。单位方（没有动作）按「未校正」显示，不报 0.0%。
    //
    // The status line reports how much narrower the top edge is than the bottom: that ratio is
    // the whole point of keystone correction and reads far better than four corner coordinates.
    // The unit square (no action) shows as "not corrected" rather than 0.0%.
    match state.params.upright {
        Some(q) if !q.is_identity() => {
            let top = (q.p[1].0 - q.p[0].0).abs();
            let bottom = (q.p[2].0 - q.p[3].0).abs();
            let pct = if bottom > 1e-9 {
                (bottom - top) / bottom * 100.0
            } else {
                0.0
            };
            let label = if pct.abs() < 0.05 {
                crate::i18n::t!("已启用（四边形接近整幅）").to_owned()
            } else if pct > 0.0 {
                crate::i18n::tr_fmt("已拉正 · 上边比下边窄 {:.1}%", &[&pct])
            } else {
                crate::i18n::tr_fmt("已拉正 · 上边比下边宽 {:.1}%", &[&(-pct)])
            };
            kv(ui, "状态", label, text());
        }
        _ => kv(ui, "状态", crate::i18n::t!("未校正"), dim()),
    }
    if state.upright_editing {
        note(
            ui,
            crate::i18n::t!("画布上四个圆点就是四角：拖到方形物体的边上，双击某个角放回原处"),
        );
    }
    changed
}

/// 修复：画笔设定 + 已落笔触的撤销。
///
/// 落点在画布上（工具条选「修复」），不在这个组里——参数面板负责「笔有多粗」，
/// 画布负责「点在哪儿」，两者分开才不会做成一个「先输入坐标再回车」的表单。
///
/// Repair: brush settings plus undo for placed strokes.
///
/// Strokes land on the canvas (pick "Heal" in the tool bar), not in this group: the panel owns
/// "how big is the brush", the canvas owns "where it goes". Keeping them apart avoids turning
/// this into a form where you type coordinates and press enter.
pub(super) fn adjust_retouch(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "retouch", "修复", false) {
        // 卡中卡（设计稿 .grp）：整组一张圆角卡，组头与内容都在卡里。
        // egui::Frame 在内容之前铺底、之后补描边——即时模式唯一可靠的
        // "背景先行"路径（其余方案：后画盖先画、RAII 拿不到终点、上一帧
        // 矩形有时序毛刺，均已试过并否决）。
        //
        // Card-in-card (the design's .grp): one rounded card per group, header and
        // body inside. egui::Frame paints the fill before the content and the stroke
        // after -- the one reliable "background first" path in immediate mode.
        // (Rejected alternatives: painting later covers earlier so the fill must go
        // first; RAII cannot reach the body's end; last-frame rects lag a frame on
        // expand/collapse.)
        egui::Frame::NONE
            .fill(theme::chrome())
            .stroke(egui::Stroke::new(1.0, theme::divider()))
            .corner_radius((theme::radius_panel() - 4.0).max(2.0))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |_ui| false);
    }
    let mut changed = false;
    let mode = ui
        .horizontal(|ui| {
            let mut hit = false;
            if bar_chip(ui, "修复", !state.heal_clone).clicked() && state.heal_clone {
                state.heal_clone = false;
                state.heal_src = None;
                hit = true;
            }
            if bar_chip(ui, "仿制", state.heal_clone).clicked() && !state.heal_clone {
                state.heal_clone = true;
                state.heal_src = None;
                hit = true;
            }
            hit
        })
        .inner;
    changed |= mode;

    changed |= slider_row(
        ui,
        "heal-radius",
        "半径",
        &mut state.heal_radius,
        0.5,
        8.0,
        2.0,
        1,
        false,
        "%",
        None,
        true,
        "占画面长边的百分比：拖到 2% 大致是一颗灰尘，6% 以上是抹掉一根电线",
    );
    changed |= slider_row(
        ui,
        "heal-feather",
        "羽化",
        &mut state.heal_feather,
        0.0,
        100.0,
        35.0,
        0,
        false,
        "%",
        None,
        true,
        "边缘过渡占半径的比例：0 是硬边，100 从中心淡到边",
    );

    let n = state.params.retouch.len();
    kv(
        ui,
        "笔触",
        if n == 0 {
            crate::i18n::t!("还没有").to_owned()
        } else {
            crate::i18n::tr_fmt("{} 笔", &[&n])
        },
        if n == 0 { dim() } else { text() },
    );
    let mut undo = false;
    let mut clear = false;
    ui.horizontal(|ui| {
        undo = bar_chip(ui, "撤销一笔", false)
            .on_hover_text(crate::i18n::t!("去掉最后落下的那一笔"))
            .clicked();
        clear = bar_chip(ui, "全部清空", false)
            .on_hover_text(crate::i18n::t!("去掉这台相机这一帧上的全部修复笔触"))
            .clicked();
    });
    if undo {
        changed |= state.heal_undo();
    }
    if clear {
        changed |= state.heal_clear();
    }

    // 仿制模式下报一句「下一步是取源」：不然点了没反应，人会以为坏了。
    //
    // In clone mode, say "next tap picks the source": otherwise the first tap does nothing and
    // reads as broken.
    if state.heal_clone {
        kv(
            ui,
            "仿制源",
            match state.heal_src {
                Some(_) => crate::i18n::t!("已取；再点画面即落笔").to_owned(),
                None => crate::i18n::t!("在画面上点一下取源").to_owned(),
            },
            if state.heal_src.is_some() {
                accent()
            } else {
                dim()
            },
        );
    }
    if !state.heal_can_place() {
        note(
            ui,
            crate::i18n::t!("画布当前不是整幅（拉直 / 推近 / 裁剪中），先复位再落点"),
        );
    }
    changed
}

/// 光学区：ACR 的 Optics 面板——手动校正三项在前，相机/镜头配置文件在后
/// （配置文件折出的畸变/色差/暗角先于手动值生效，排在后面读起来是"标定打底"）。
///
/// Optics, mirroring ACR: the three manual corrections first, then the camera/lens
/// profiles (what the profile derives is applied ahead of the manual sliders, so
/// reading it last says "the calibration is the base layer").
pub(super) fn adjust_optics(state: &mut AppState, ui: &mut Ui) -> bool {
    if !group_header(ui, state, "optics", "光学", true) {
        // 卡中卡（设计稿 .grp）：整组一张圆角卡，组头与内容都在卡里。
        // egui::Frame 在内容之前铺底、之后补描边——即时模式唯一可靠的
        // "背景先行"路径（其余方案：后画盖先画、RAII 拿不到终点、上一帧
        // 矩形有时序毛刺，均已试过并否决）。
        //
        // Card-in-card (the design's .grp): one rounded card per group, header and
        // body inside. egui::Frame paints the fill before the content and the stroke
        // after -- the one reliable "background first" path in immediate mode.
        // (Rejected alternatives: painting later covers earlier so the fill must go
        // first; RAII cannot reach the body's end; last-frame rects lag a frame on
        // expand/collapse.)
        egui::Frame::NONE
            .fill(theme::chrome())
            .stroke(egui::Stroke::new(1.0, theme::divider()))
            .corner_radius((theme::radius_panel() - 4.0).max(2.0))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |_ui| false);
    }
    let mut changed = false;
    sub_head(ui, crate::i18n::t!("镜头校正"));
    changed |= lens_body(state, ui);
    sub_head(ui, crate::i18n::t!("镜头配置"));
    let ctx = ui.ctx().clone();
    lens_profile_body(state, &ctx, ui);
    changed
}

/// 镜头校正：畸变、暗角补偿、去紫边；三项都是空间操作，导出同样生效。
/// 顶上一行是机型匹配：EXIF 对 lensfun 数据库折出的标定系数先走一步。
/// 光学区的第一段，不带组头。
///
/// Lens correction: distortion, vignette compensation, defringe -- all spatial and
/// applied on export too. The top row is EXIF-based matching against the lensfun
/// database. The first band of the Optics section, without a header of its own.
fn lens_body(state: &mut AppState, ui: &mut Ui) -> bool {
    let mut changed = false;
    let mut auto = state.params.lens_profile.is_some();
    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut auto, crate::i18n::t!("自动匹配"))
            .on_hover_text(crate::i18n::t!("按 EXIF 的机身/镜头/焦距在 lensfun 数据库里找标定，\n自动折出畸变、横向色差与暗角校正；\n找不到库或没命中时只剩手动三项。"))
            .changed()
        {
            if auto {
                if let Some(c) = state.lens_profile_saved.take() {
                    state.params.lens_profile = Some(c);
                } else {
                    state.match_lens_profile();
                }
            } else {
                state.lens_profile_saved = state.params.lens_profile.take();
            }
            state.preview_dirty = true;
            changed = true;
        }
        if ui
            .button(crate::i18n::t!("重配"))
            .on_hover_text(crate::i18n::t!("按当前 EXIF 重新跑一遍匹配"))
            .clicked()
        {
            state.match_lens_profile();
            state.preview_dirty = true;
            changed = true;
        }
    });
    if !state.lens_profile_note.is_empty() {
        ui.label(egui::RichText::new(&state.lens_profile_note).weak());
    }
    ui.add_space(2.0);
    for f in [Field::LensDistort, Field::LensVignette, Field::LensFringe] {
        changed |= spec_field(ui, state, f);
    }
    changed
}

pub(super) fn adjust_frame(state: &mut AppState, ui: &mut Ui) {
    if group_header(ui, state, "frame", "画幅", true) {
        // 卡中卡（设计稿 .grp）：整组一张圆角卡，组头与内容都在卡里。
        // egui::Frame 在内容之前铺底、之后补描边——即时模式唯一可靠的
        // "背景先行"路径（其余方案：后画盖先画、RAII 拿不到终点、上一帧
        // 矩形有时序毛刺，均已试过并否决）。
        //
        // Card-in-card (the design's .grp): one rounded card per group, header and
        // body inside. egui::Frame paints the fill before the content and the stroke
        // after -- the one reliable "background first" path in immediate mode.
        // (Rejected alternatives: painting later covers earlier so the fill must go
        // first; RAII cannot reach the body's end; last-frame rects lag a frame on
        // expand/collapse.)
        egui::Frame::NONE
            .fill(theme::chrome())
            .stroke(egui::Stroke::new(1.0, theme::divider()))
            .corner_radius((theme::radius_panel() - 4.0).max(2.0))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                // 比例胶囊 9 枚，336px 面板一行装不下——horizontal_wrapped 会在第 8 枚
                // 后孤行挂一枚「2:3」，读作排版事故。改成 5+4 两行均衡排。
                //
                // Nine aspect chips do not fit one row of the 336 px panel; horizontal wrap
                // stranded "2:3" alone on line two, which reads as a layout accident. Lay
                // them out as two balanced rows of 5 and 4.
                for row in Aspect::ALL.chunks(5) {
                    ui.horizontal(|ui| {
                        for &a in row {
                            if bar_chip(ui, a.label(), state.aspect == a).clicked() {
                                state.set_aspect(a);
                            }
                        }
                    });
                }
                if state.resolution.is_some() {
                    ui.add_space(4.0);
                    // 推子只管取值，0 附近吸附和重画标记都交给 set_rotate。
                    let mut deg = state.rotate;
                    if slider(
                        ui,
                        "frame.rotate",
                        "拉直",
                        &mut deg,
                        -45.0,
                        45.0,
                        0.0,
                        1,
                        true,
                    ) {
                        state.set_rotate(deg);
                    }
                }
                ui.add_space(4.0);
                if state.resolution.is_some() {
                    let sel = state.crop_box();
                    kv(
                        ui,
                        "取景框",
                        format!("{} × {} @ {}, {}", sel.w, sel.h, sel.x, sel.y),
                        text(),
                    );
                    match state.aspect.ratio() {
                        Some(r) => kv(ui, "画幅比例", format!("{r:.3}"), muted()),
                        None => kv(ui, "画幅比例", "跟随原生", muted()),
                    }
                    ui.horizontal_wrapped(|ui| {
                        if bar_chip(ui, "拖框裁剪", state.tool == Tool::Crop).clicked() {
                            state.set_tool(Tool::Crop);
                        }
                        if (state.user_crop.is_some() || state.rotate != 0.0)
                            && ui.small_button(crate::i18n::t!("取消取景")).clicked()
                        {
                            state.crop_reset();
                            state.set_rotate(0.0);
                        }
                    });
                } else {
                    kv(ui, "画幅比例", "跟随原生", muted());
                }
            });
    }
    if group_header(ui, state, "frame-guides", "辅助线", false) {
        // 卡中卡（设计稿 .grp）：整组一张圆角卡，组头与内容都在卡里。
        // egui::Frame 在内容之前铺底、之后补描边——即时模式唯一可靠的
        // "背景先行"路径（其余方案：后画盖先画、RAII 拿不到终点、上一帧
        // 矩形有时序毛刺，均已试过并否决）。
        //
        // Card-in-card (the design's .grp): one rounded card per group, header and
        // body inside. egui::Frame paints the fill before the content and the stroke
        // after -- the one reliable "background first" path in immediate mode.
        // (Rejected alternatives: painting later covers earlier so the fill must go
        // first; RAII cannot reach the body's end; last-frame rects lag a frame on
        // expand/collapse.)
        egui::Frame::NONE
            .fill(theme::chrome())
            .stroke(egui::Stroke::new(1.0, theme::divider()))
            .corner_radius((theme::radius_panel() - 4.0).max(2.0))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.checkbox(
                    &mut state.grid,
                    RichText::new(crate::i18n::t!("三分线")).size(SIZE_SMALL),
                );
                ui.checkbox(
                    &mut state.cross,
                    RichText::new(crate::i18n::t!("中心十字")).size(SIZE_SMALL),
                );
                ui.checkbox(
                    &mut state.split,
                    RichText::new(crate::i18n::t!("前后分割")).size(SIZE_SMALL),
                );
            });
    }
}
