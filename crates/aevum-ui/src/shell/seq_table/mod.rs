//! 序列帧表（v10.6 第二个工作台）：逐帧表格 + 左侧预览/摘要卡。
//!
//! 规格来源：`docs/aevum-v10.6-rust-replica-plan.md`（§2 量化、§6 绘制）与
//! `docs/aevum-v10.6-complete.html#vTable`（只读设计稿）。
//!
//! # 骨架
//!
//! ```text
//! 页头 64      序列帧表 · 1,387 帧 · 24 fps · 亮度 …        [保存 XMP] [导出和渲染…]
//! 工具条 36    关键帧向导 至前向导 自动过渡 预览 去闪 …                每帧渲染 · 参数过渡 · 间隔 2s
//! 主体         左 300：预览卡(280 图) + 摘要卡 + 自动过渡卡 │ 右：帧表卡（表头/范围条/行/页脚）
//! ```
//!
//! # Skeleton
//!
//! Spec: the v10.6 replica plan (§2 measurements, §6 drawing) and the read-only mockup
//! `docs/aevum-v10.6-complete.html#vTable`.

mod preview;
mod table;

use egui::{Context, Ui};

use super::bench;
use crate::state::AppState;
use crate::theme::bench_bg;

/// 左侧预览列宽：设计稿 `#vTable .ws-body{grid-template-columns:300px minmax(0,1fr)}`。
///
/// Left preview column width, per the design's table body grid.
const SIDE_W: f32 = 300.0;

/// 画整页帧表。
///
/// Draw the whole frame-table page.
pub(super) fn page(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let full = ui.max_rect();
    let p = ui.painter().clone();
    p.rect_filled(full, 0.0, bench_bg());

    let n = state.frame_count();
    let sub = if n == 0 {
        crate::i18n::t!("还没有序列 · 文件 → 打开照片…").to_string()
    } else {
        // 设计稿页头副行：`1,387 帧 · 24 fps · 亮度 0.213 · 关键帧 5`。
        //
        // The design's header subtitle: frames, rate, luma and keyframe count.
        let mean = state
            .metrics
            .get(state.selected)
            .and_then(|m| m.as_ref())
            .map(|m| crate::i18n::tr_fmt(" · 亮度 {}", &[&format!("{:.3}", m.mean)]))
            .unwrap_or_default();
        crate::i18n::tr_fmt(
            "{} 帧 · {} fps{} · 关键帧 {}",
            &[
                &preview::thousands(n),
                &format!("{:.0}", state.output_fps),
                &mean,
                &state.keyframes.len(),
            ],
        )
    };
    let (head, back) = bench::page_head(ui, crate::i18n::t!("序列帧表"), &sub, f32::MAX, 16.0);
    // 页头左端的返回块：帧表页没有侧栏也没有画布，这里是唯一的可见出口。
    //
    // The back chip at the head's left edge: the table bench has neither rails nor
    // canvas, so this is the only visible way out.
    if back {
        state.set_bench(super::Bench::Develop);
        return;
    }
    // 页头右侧动作：只留「导出和渲染…」并接上真实入口。原来还有一颗「保存 XMP」，
    // 但工程里没有「批量写边车」的实现——画出来点了没反应，是假控件，删掉比留着诚实。
    //
    // One head action, wired to the real entry point. The old "Save XMP" button had no
    // backing implementation anywhere in the project -- a painted control that did
    // nothing when clicked, so it is removed rather than left as a decoy.
    let (_r1, go) = bench::head_action(
        ui,
        head.right(),
        head.center().y,
        crate::i18n::t!("导出和渲染…"),
        true,
    );
    if go.clicked() {
        state.request_export_confirm(ctx, false);
    }

    // 二级工具条整条删除：这五枚按钮（关键帧向导 / 至前向导 / 自动过渡 / 预览 / 去闪）
    // 在工程里都没有对应实现——画得出来、点了没反应，是假控件。删掉比留着诚实，
    // 版式上页头与表格之间也不再夹一条空带。
    //
    // The secondary strip is gone: none of those five buttons has an implementation in
    // this project (they painted and did nothing). Removing them beats a row of decoys,
    // and it drops a dead band between the head and the table.
    let body = egui::Rect::from_min_max(
        egui::Pos2::new(full.left(), head.bottom() + 12.0),
        egui::Pos2::new(full.right() - 16.0, full.bottom() - 16.0),
    );
    let left = egui::Rect::from_min_size(
        body.left_top() + egui::vec2(16.0, 0.0),
        egui::vec2(SIDE_W, body.height()),
    );
    let right = egui::Rect::from_min_max(
        egui::Pos2::new(left.right() + 12.0, body.top()),
        body.right_bottom(),
    );
    preview::column(state, ui, left);
    table::card(state, ui, right);
}
