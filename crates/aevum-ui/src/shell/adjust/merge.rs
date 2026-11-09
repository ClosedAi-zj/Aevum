//! 合成模块：HDR / 全景 / 星空堆栈 / 时间合成 / 焦点合成，
//! 以及取帧行、对齐提示与它们的单测。
//!
//! The merge module: HDR, panorama, star stack, time blend and focus stack, plus the
//! frame-span row, the alignment hints and their unit criteria.

// 共用件（theme / i18n / 兄弟模块的函数）由 mod.rs 的 use 树统一带入。
//
// Shared pieces (theme, i18n, sibling helpers) come in through mod.rs's use tree.
use super::*;
fn hdr_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let busy = state.merge_kind == Some(MergeKind::Hdr);
    // 快照对账：参数真的变了才置落盘脏标记（见 stack_group 的说明）。
    let before_around = state.hdr_around;
    let cap = both_sides_caption(state.hdr_around);
    span_row(
        ui,
        crate::i18n::t!("从当前帧起，前后各取"),
        &mut state.hdr_around,
        1..=4,
        busy,
        cap,
    );
    let go = ui
        .add_enabled(
            !busy,
            egui::Button::new(
                RichText::new(if busy {
                    crate::i18n::t!("合并中…")
                } else {
                    crate::i18n::t!("合并")
                })
                .size(SIZE_CHROME),
            )
            .min_size(Vec2::new(0.0, BAR_H))
            .corner_radius(radius_control()),
        )
        .on_hover_text(crate::i18n::t!(
            "把这一段包围曝光融成一张静帧，写进输出目录"
        ))
        .clicked();
    if go {
        let args = serde_json::json!({"around": state.hdr_around, "confirm": true});
        if let Err(e) = state.run_ui_command(ctx, "hdr.merge", &args) {
            state.hdr_error = Some(e);
            state.hdr_last = None;
        }
    }
    if state.hdr_around != before_around {
        state.settings_dirty = true;
    }
    // 合并是后台任务：结果与失败原因由 `Msg::Merged` 写回 hdr_last / hdr_error。
    merge_progress_row(state, ui, MergeKind::Hdr);
    if let Some(last) = state.hdr_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    if let Some(err) = state.hdr_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
}

fn pano_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    let busy = state.merge_kind == Some(MergeKind::Pano);
    span_row(
        ui,
        crate::i18n::t!("当前帧后再拼"),
        &mut state.pano_around,
        1..=11,
        busy,
        crate::i18n::t!("张").to_owned(),
    );
    let go = ui
        .add_enabled(
            !busy,
            egui::Button::new(
                RichText::new(if busy {
                    crate::i18n::t!("拼接中…")
                } else {
                    crate::i18n::t!("拼接")
                })
                .size(SIZE_CHROME),
            )
            .min_size(Vec2::new(0.0, BAR_H))
            .corner_radius(radius_control()),
        )
        .on_hover_text(crate::i18n::t!(
            "把当前帧与其后相邻几张接成一张全景，写进输出目录"
        ))
        .clicked();
    if go {
        // pano_around 就是「当前帧之后再接几张」，与 pano.merge 的 around 同义。
        let args = serde_json::json!({"around": state.pano_around, "confirm": true});
        if let Err(e) = state.run_ui_command(ctx, "pano.merge", &args) {
            state.pano_error = Some(e);
            state.pano_last = None;
        }
    }
    merge_progress_row(state, ui, MergeKind::Pano);
    if let Some(last) = state.pano_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    if let Some(err) = state.pano_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
}

/// 「星空堆栈」分组：选统计量、调半径，一键合成一张。
///
/// Star-stacking group: pick the statistic, set the radius, combine into one still.
fn stack_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    use aevum_core::stack::Mode;
    let busy = state.merge_kind == Some(MergeKind::Stack);
    // 快照对账：合并参数是全局偏好（settings.json），本帧内真的变了才置落盘脏
    // 标记——帧尾的单一出口统一写文件，没变就不写，免得把 settings.json 当草稿纸。
    //
    // Snapshot reconciliation: the merge parameters are a global preference
    // (settings.json), and the dirty flag goes up only when something actually changed
    // within this frame -- the single frame-end exit writes the file, so an unchanged
    // frame never turns settings.json into scratch paper.
    let snapshot = (
        state.stack_around,
        state.stack_mode,
        state.stack_sigma,
        state.stack_fill,
    );
    let cap = both_sides_caption(state.stack_around);
    span_row(
        ui,
        crate::i18n::t!("从当前帧起，前后各取"),
        &mut state.stack_around,
        1..=32,
        busy,
        cap,
    );
    ui.horizontal_wrapped(|ui| {
        for mode in Mode::ALL {
            let picked = bar_chip(ui, mode.cn(), state.stack_mode == mode)
                .on_hover_text(crate::i18n::tr(match mode {
                    Mode::Lighten => "取最亮：星轨连成线，云与车流也连成线",
                    Mode::Average => "算术平均：随机噪声按 √N 下降，移动物变成半透明鬼影",
                    Mode::Median => "逐通道中值：卫星、飞机、单帧热噪点被整帧丢掉",
                    Mode::SigmaClip => "σ 截尾平均：先剔离群再平均，杂散光与热噪点都压得住",
                }))
                .clicked();
            if picked && !busy {
                state.stack_mode = mode;
            }
        }
    });
    if state.stack_mode == Mode::SigmaClip {
        let mut k = state.stack_sigma as f64;
        if slider(
            ui,
            "stack_clip_sigma",
            "截尾 k·σ",
            &mut k,
            1.0,
            8.0,
            3.0,
            1,
            false,
        ) {
            state.stack_sigma = k as f32;
        }
    }
    ui.checkbox(
        &mut state.stack_fill,
        crate::i18n::t!("补洞：把合成后仍全黑的像素用邻域补上"),
    );
    let go = ui
        .add_enabled(
            !busy,
            egui::Button::new(RichText::new(if busy {
                crate::i18n::t!("堆栈中…")
            } else {
                crate::i18n::t!("堆栈")
            }))
            .min_size(Vec2::new(0.0, BAR_H))
            .corner_radius(radius_control()),
        )
        .on_hover_text(crate::i18n::t!(
            "把当前帧前后若干帧按所选统计量合成一张，写进输出目录"
        ))
        .clicked();
    if go {
        let args = serde_json::json!({
            "around": state.stack_around,
            "mode": state.stack_mode.key(),
            "clip_sigma": state.stack_sigma,
            "fill_gaps": state.stack_fill,
            "confirm": true,
        });
        if let Err(e) = state.run_ui_command(ctx, "stack.merge", &args) {
            state.stack_error = Some(e);
            state.stack_last = None;
        }
    }
    merge_progress_row(state, ui, MergeKind::Stack);
    if let Some(last) = state.stack_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    if let Some(err) = state.stack_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
    if snapshot
        != (
            state.stack_around,
            state.stack_mode,
            state.stack_sigma,
            state.stack_fill,
        )
    {
        state.settings_dirty = true;
    }
}

/// 取帧行里「这个数到底代表几帧」的括号说明：前后各取。
///
/// The caption that says how many frames a span figure really means: both sides.
fn both_sides_caption(around: u32) -> String {
    // 前后各 N 帧 = 当前帧 + 两侧各 N，共 2N+1。写清楚，省得用户按 N 去算。
    crate::i18n::tr_fmt("帧（共 {} 帧）", &[&(around * 2 + 1)])
}

/// 取帧行里「这个数到底代表几帧」的括号说明：只往后取，带硬上限。
///
/// The caption that says how many frames a span figure really means: forward only, with
/// the hard cap.
fn forward_caption(around: u32, cap: usize) -> String {
    // 只往后取 N 帧 = 当前帧 + 之后 N 帧，共 N+1；超过 cap 就得分段。
    crate::i18n::tr_fmt("帧（共 {} 帧，上限 {}）", &[&(around + 1), &cap])
}

/// 取帧行：「说明 + 数字 + 括号解释」，**按实测宽度选行数**。
///
/// 为什么不用 `ui.horizontal_wrapped`：egui 的折行布局对**文本**不可靠——在
/// `horizontal_wrapped` 里，一个需要折行的 `Label` 会被重新按整行宽度排版并画回
/// 行首，于是「从当前帧起，前后各取」和「帧（共 3 帧）」叠在同一位置（判据
/// `ui_tokens::text_never_overlaps_in_any_module` 当场红）。而原来的 `ui.horizontal`
/// 不许折行，英文
/// 「From the current frame, take 1 frames on each side (3 frames in all)」有 418px，
/// 比 344px 的面板宽 86px —— 后果不是「文字被裁」这么轻：egui 在内容溢出时会把
/// 面板 rect 整体往右推（`set_rect_size` 保右边界），右坞一挪，后面声明的工具轨和
/// 中间画布跟着错位，画布底色直接盖掉面板左缘的页签。
///
/// 所以这里自己量：三段都用 `layout_no_wrap` 量出真实宽度，塞得下就一行，
/// 塞不下就把说明提出来单独一行（垂直布局里的 `Label` 折行是可靠的），再不行
/// 三段各一行。行数只由实测宽度决定，与语言、字号档位都无关。
///
/// The frame-span row: "lead-in + number + parenthetical", **line count chosen from
/// measured widths**.
///
/// Why not `ui.horizontal_wrapped`: egui's wrapping layout is unreliable for *text* --
/// a `Label` that needs to wrap is re-laid out at full row width and painted back at the
/// row origin, so "从当前帧起，前后各取" and "帧（共 3 帧）" land on top of each other
/// (`ui_tokens::text_never_overlaps_in_any_module` catches it). The old `ui.horizontal`
/// cannot wrap at all, and the English "From the current frame, take 1 frames on each
/// side (3 frames in all)" measures 418 px against a 344 px panel -- 86 px too wide. The
/// damage is worse than clipped text: egui pushes the whole panel rect right when content
/// overflows (`set_rect_size` keeps the right edge), and with the dock moved the tool rail
/// and the central canvas declared after it shift too, painting the canvas fill over the
/// dock's left edge.
///
/// So the widths are measured here: `layout_no_wrap` for each piece, one row when it
/// fits, otherwise the lead-in gets its own line (a `Label` wraps reliably in a vertical
/// layout), and three rows when even the number plus the caption is too wide. The line
/// count follows measurement alone -- independent of language and font step.
fn span_row(
    ui: &mut Ui,
    lead: &str,
    n: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    busy: bool,
    caption: String,
) {
    let f = font(SIZE_SMALL);
    let p = ui.painter().clone();
    let lead_label = || RichText::new(lead).size(SIZE_SMALL).color(dim());
    let w_lead = p.layout_no_wrap(lead.to_owned(), f.clone(), dim()).size().x;
    let w_cap = p.layout_no_wrap(caption.clone(), f.clone(), dim()).size().x;
    let gap = ui.spacing().item_spacing.x;
    // DragValue 按内容自适应宽，`interact_size.x` 是它的下限：按下限估，宁可早折行。
    let dv = ui.spacing().interact_size.x;
    let avail = ui.available_width();
    let number = |ui: &mut Ui, n: &mut u32| {
        ui.add_enabled(
            !busy,
            egui::DragValue::new(n).range(range.clone()).speed(0.2),
        );
    };
    if w_lead + gap + dv + gap + w_cap <= avail {
        ui.horizontal(|ui| {
            ui.label(lead_label());
            number(ui, n);
            ui.label(RichText::new(caption).size(SIZE_SMALL).color(dim()));
        });
    } else if dv + gap + w_cap <= avail {
        ui.label(lead_label());
        ui.horizontal(|ui| {
            number(ui, n);
            ui.label(RichText::new(caption).size(SIZE_SMALL).color(dim()));
        });
    } else {
        ui.label(lead_label());
        number(ui, n);
        ui.label(RichText::new(caption).size(SIZE_SMALL).color(dim()));
    }
}

/// 焦点组的「先对齐」提示：序列开着、还没对齐、且这一组帧够长时才值得提醒。
///
/// 手持拍的对焦包围常有几像素位移：不对齐直接融，过渡带上会出现重影，而重影看起来
/// 恰恰像「深度图选错了帧」——先提一句，省得用户去拧 radius 和 threshold。
///
/// The focus group's "align first" hint: only when a sequence is open, it has not been
/// aligned yet, and the bracket is long enough for the drift to matter.
///
/// A handheld focus bracket usually drifts a few pixels. Fusing without aligning leaves
/// ghosting across the transition band, and ghosting looks exactly like "the depth map
/// picked the wrong frame" -- say it here instead of letting the user chase `radius` and
/// `threshold`.
fn focus_align_hint(state: &AppState) -> Option<String> {
    if state.seq.is_none() || state.align_report.is_some() {
        return None;
    }
    // 2 帧谈不上过渡带：不对齐的代价要 3 帧起才看得出来。
    if state.focus_around < 2 {
        return None;
    }
    // 文案在这里就翻好：调用点只负责上屏，别再各自包一层。
    //
    // The text is translated right here; call sites only put it on screen.
    Some(crate::i18n::tr_fmt(
        "这组帧还没对齐：手持拍的焦包围常有几像素位移，直接融会在过渡带留下重影。先跑一次对齐再合成。",
        &[],
    ))
}

/// 自动对齐的三态状态行：`(文案, 颜色)`，没有可交代的就返回 `None`。
///
/// 数据源是最近一次合并的结果 JSON（`kind == "focus"` 时才看）：增益与状态都是当时
/// 真实算出来的，面板只做格式转换——**不重算、不推断、不美化**。`"off"` 与字段缺失
/// 都不出行，因为「没跑」不值得占一行。
///
/// The three-state status line of auto alignment: `(text, colour)`, or `None` when there is
/// nothing to report.
///
/// The data comes from the most recent merge result JSON (only read when `kind == "focus"`):
/// both the status and the gain were computed for real back then, and the panel only formats
/// them -- it never recomputes, infers or prettifies. `"off"` and a missing field both emit
/// no row, because "it did not run" does not deserve a line.
fn focus_align_status_row(state: &AppState) -> Option<(String, egui::Color32)> {
    let result = state.merge_result.as_ref()?;
    if result.get("kind").and_then(|v| v.as_str()) != Some("focus") {
        return None;
    }
    let status = result.get("align_status").and_then(|v| v.as_str())?;
    let gain = result.get("align_gain").and_then(|v| v.as_f64());
    let percent = |g: f64| format!("{:+.1}%", g * 100.0);
    match status {
        "adopted" => Some((
            crate::i18n::tr_fmt(
                "自动对齐：已采用（增益 {}）",
                &[&gain.map(percent).unwrap_or_else(|| "—".to_owned())],
            ),
            accent(),
        )),
        "reverted_insufficient_gain" => Some((
            crate::i18n::tr_fmt(
                "自动对齐：收益不足已回退（{}），按未对齐几何出图",
                &[&gain.map(percent).unwrap_or_else(|| "—".to_owned())],
            ),
            dim(),
        )),
        "failed" => Some((
            crate::i18n::t!(
                "自动对齐：估计失败，已按未对齐几何出图；建议用三脚架重拍，或关闭自动对齐后重试"
            )
            .to_owned(),
            danger(),
        )),
        _ => None,
    }
}

/// 焦点合成：一组不同对焦距离的帧融成一张全景深。
///
/// Focus stacking: fuse a bracket taken at different focus distances into one all-in-focus
/// image.
fn focus_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    use aevum_core::focus::Method;
    let busy = state.merge_kind == Some(MergeKind::Focus);
    // 快照对账：本帧内任何参数或取帧变了，旧深度图预览就地作废——宁可让用户
    // 再点一次，也不挂一张与当前参数对不上的图误导选区。
    //
    // Snapshot reconciliation: if any parameter or the frame selection changed within
    // this frame, the old depth-map preview is voided on the spot -- asking the user
    // to press again beats hanging a picture that disagrees with the current
    // parameters and misleads the selection.
    let focus_snapshot = (
        state.focus_around,
        state.focus_method,
        state.focus_radius,
        state.focus_threshold,
        state.focus_smooth_z,
        state.focus_feather,
        // 自动对齐也是「影响成片」的参数：改了就要置落盘脏标记、并作废旧深度图预览
        // （预览走的是未对齐几何，勾上对齐后它就不再代表成片了）。
        //
        // Auto alignment also changes the delivered image: flipping it must mark the
        // settings dirty and void the stale depth-map preview (that preview follows the
        // unaligned geometry and no longer represents the still once alignment is on).
        state.focus_align,
    );
    // 没对齐就先说一句，并就地给个按钮：让用户自己去别处找「开始对齐」，这句提示
    // 就只是抱怨。
    //
    // Say it up front, with the button right here: a hint that only complains would send
    // the user hunting for the align control elsewhere in the panel.
    if let Some(hint) = focus_align_hint(state) {
        ui.add(egui::Label::new(RichText::new(hint).size(SIZE_SMALL).color(accent())).wrap());
        let aligning = state.align_busy;
        if ui
            .add_enabled(
                !aligning && !busy,
                egui::Button::new(
                    RichText::new(if aligning {
                        crate::i18n::t!("正在对齐…")
                    } else {
                        crate::i18n::t!("先对齐序列")
                    })
                    .size(SIZE_SMALL),
                ),
            )
            .on_hover_text(crate::i18n::t!(
                "对齐估出每帧位移与统一裁切，焦点合成会自动套上；不对齐直接融，过渡带会出重影"
            ))
            .clicked()
        {
            state.run_align(ui.ctx());
        }
    }
    ui.horizontal_wrapped(|ui| {
        for method in Method::ALL {
            let picked = bar_chip(ui, method.cn(), state.focus_method == method)
                .on_hover_text(crate::i18n::tr(match method {
                    Method::DepthMap => {
                        "逐像素取测度最强的帧；测度低于阈值的平坦区整块用参考帧兜底，过渡带羽化——保色彩、不出斑块"
                    }
                    Method::Weighted => {
                        "测度四次方当权重直接加权；最简单，低噪栈表现好，强纹理处可能轻微发糊"
                    }
                    Method::Pyramid => {
                        "拉普拉斯塔按层融合：细层取清晰细节，最粗两层等权当底图；多尺度自带过渡，羽化在金字塔法下不适用"
                    }
                }))
                .clicked();
            if picked && !busy {
                // 切到金字塔法时深度图预览已无意义（它不产生深度图）：就地清掉纹理与
                // 说明，而不是等本帧末的快照比对——那会让旧图在同一帧里闪一下就消失。
                //
                // Switching to the pyramid method makes the depth-map preview meaningless
                // (it produces no depth map): clear the texture and caption right here
                // instead of waiting for the end-of-frame snapshot comparison, which would
                // flash the stale image for the rest of this frame.
                if method == Method::Pyramid && state.focus_method != Method::Pyramid {
                    state.focus_depth_tex = None;
                    state.focus_depth_note = None;
                }
                state.focus_method = method;
            }
        }
    });
    // 合成前自动对齐（默认关）：breathing 补偿是「多一步、贵一点、可能回退」的事，
    // 不能替用户默认打开；勾上才做，且做没做、收益多少都会在状态行如实交代。
    //
    // Pre-fusion auto alignment (off by default): breathing compensation costs an extra
    // pass and may well revert, so it is never turned on behind the user's back. Whether it
    // ran and how much it gained is reported honestly below.
    ui.add_enabled(
        !busy,
        egui::Checkbox::new(&mut state.focus_align, crate::i18n::t!("合成前自动对齐")),
    )
    .on_hover_text(crate::i18n::t!(
        "先估每帧的 8×6 网格位移，再逐块补偿：对焦时的呼吸式轻微缩放也能压住。收益不足 0.5% 会自动回退成不做（成片与关闭时逐点一致）；估计失败照样出图，只是按未对齐几何。默认关闭"
    ));
    let cap = forward_caption(state.focus_around, aevum_core::focus::MAX_FRAMES);
    span_row(
        ui,
        crate::i18n::t!("从当前帧起，向后多取"),
        &mut state.focus_around,
        1..=31,
        busy,
        cap,
    );
    // 测度半径与无细节阈值是深度图法的两根主梁，各挂一句口径说明。
    //
    // The measure radius and the featureless threshold are the two girders of the depth-map
    // method, each with its stated convention.
    let mut radius = state.focus_radius as f64;
    if slider_row(
        ui,
        "focus_radius",
        "测度半径",
        &mut radius,
        0.0,
        32.0,
        2.0,
        0,
        false,
        "px",
        None,
        false,
        "焦点测度的分析窗口半径：越大越不受噪点干扰，越小越能分辨紧挨着的前后景；0 = 逐像素原始响应",
    ) {
        state.focus_radius = radius as u32;
    }
    let mut threshold = state.focus_threshold as f64;
    if slider_row(
        ui,
        "focus_threshold",
        "无细节阈值",
        &mut threshold,
        0.0,
        100.0,
        0.0,
        0,
        false,
        "%",
        None,
        false,
        "全图最佳测度的分位数：低于这条线的像素整块判给参考帧（序列中位帧）。表盘、天空、抛光面这类平坦区就靠它不出斑块；0 = 关闭兜底",
    ) {
        state.focus_threshold = threshold as f32;
    }
    if state.focus_method == Method::DepthMap {
        let mut smooth = state.focus_smooth_z as f64;
        if slider_row(
            ui,
            "focus_smooth_z",
            "深度去斑",
            &mut smooth,
            0.0,
            16.0,
            1.0,
            0,
            false,
            "px",
            None,
            false,
            "深度图的模值滤波半径：把孤立的深度点并进周围多数派，免得单点噪点切出一小块另一帧",
        ) {
            state.focus_smooth_z = smooth as u32;
        }
        let mut feather = state.focus_feather as f64;
        if slider_row(
            ui,
            "focus_feather",
            "过渡羽化",
            &mut feather,
            0.0,
            64.0,
            2.0,
            1,
            false,
            "px",
            None,
            false,
            "深度跳变处两张素材按高斯权重交叠的宽度；小于 0.5 px 视为硬切",
        ) {
            state.focus_feather = feather as f32;
        }
    } else if state.focus_method == Method::Pyramid {
        // 切到金字塔法时这两根滑杆会整块消失：不给一句交代，用户会以为功能丢了
        // （PRD PYR-2 要求「显式说明不适用」，不许静默）。
        //
        // Switching to the pyramid method makes both sliders disappear wholesale; leaving it
        // unsaid reads as "the feature vanished" (PRD PYR-2 demands an explicit "not
        // applicable" statement, never silence).
        ui.add(
            egui::Label::new(
                RichText::new(crate::i18n::t!(
                    "深度去斑与过渡羽化：金字塔法不适用（多尺度融合自带过渡）"
                ))
                .size(SIZE_SMALL)
                .color(dim()),
            )
            .wrap(),
        );
    }
    ui.horizontal(|ui| {
        // 预览按钮在合成按钮左边：一个只看不写（前台算降采样归属图），
        // 一个写文件（后台跑全分辨率）。
        //
        // The preview button sits left of the merge button: one looks without writing
        // (a downsampled ownership map computed in the foreground), one writes files
        // (full resolution on the background thread).
        // 金字塔法不产生深度图：按钮置灰，而不是让用户点完发现没反应（PRD PYR-2 的
        // 「禁止伪预览」）。
        //
        // The pyramid method produces no depth map: disable the button instead of letting the
        // user click and see nothing (PRD PYR-2's "no fake preview").
        let pyramid = state.focus_method == Method::Pyramid;
        let can_preview = !busy && state.seq.is_some() && !pyramid;
        if ui
            .add_enabled(
                can_preview,
                egui::Button::new(RichText::new(crate::i18n::t!("预览深度图")).size(SIZE_SMALL))
                    .min_size(Vec2::new(0.0, BAR_H))
                    .corner_radius(radius_control()),
            )
            .on_hover_text(crate::i18n::t!(
                "按当前取帧与滑杆参数算一张降采样的深度归属图：每帧一种颜色，参考帧是灰色；只算预览，不写文件"
            ))
            .clicked()
        {
            state.focus_depth_preview(ctx);
        }
        // 置灰的原因必须写在按钮旁边，不能只靠「按不下去」让人猜。
        //
        // The reason for the greyed-out state belongs right next to the button; "it does not
        // respond" is not an explanation.
        if pyramid {
            ui.label(
                RichText::new(crate::i18n::t!("金字塔法不产生深度图"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        }
        let go = ui
            .add_enabled(
                !busy,
                egui::Button::new(RichText::new(if busy {
                    crate::i18n::t!("合成中…")
                } else {
                    crate::i18n::t!("开始合成")
                }))
                .min_size(Vec2::new(0.0, BAR_H))
                .corner_radius(radius_control()),
            )
            .on_hover_text(crate::i18n::t!(
                "把当前帧起若干帧按所选方法融成一张全景深，写进输出目录"
            ))
            .clicked();
        if go {
            let args = serde_json::json!({
                "around": state.focus_around,
                "method": state.focus_method.key(),
                "radius": state.focus_radius,
                "threshold": state.focus_threshold,
                "smooth_z": state.focus_smooth_z,
                "feather": state.focus_feather,
                // 面板把勾选状态**显式**传下去：命令面的缺省值仍是 false（X-C1 红线，
                // 脚本面必须确定），不靠「缺省即面板意图」这种隐式约定。
                //
                // The panel passes the checkbox state **explicitly**: the command's default
                // stays false (X-C1's red line -- scripts need determinism), so this never
                // leans on an implicit "the default is whatever the panel wants".
                "align": state.focus_align,
                "confirm": true,
            });
            if let Err(e) = state.run_ui_command(ctx, "focus.merge", &args) {
                state.focus_error = Some(e);
                state.focus_last = None;
            }
        }
    });
    merge_progress_row(state, ui, MergeKind::Focus);
    if let Some(last) = state.focus_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    // 自动对齐的三态如实上屏：面板**不重算**增益、也不把失败显示成成功。
    //
    // The three states of auto alignment go on screen as they are: the panel never recomputes
    // the gain and never dresses a failure up as success.
    if let Some((text, colour)) = focus_align_status_row(state) {
        ui.add(egui::Label::new(RichText::new(text).size(SIZE_SMALL).color(colour)).wrap());
    }
    if let Some(err) = state.focus_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
    // 深度图预览：按可用宽度缩放、保持纵横比（与时间合成预览同一条路）；
    // 说明行挂尺寸 / 耗时 / 参考帧，失败时挂原因。金字塔法下不画（它不产生深度图，
    // 残留纹理只会误导），这是「禁止伪预览」的最后一道兜底。
    //
    // Depth-map preview: scaled to the available width, aspect preserved (the same path as
    // the time-blend preview); the caption carries size / elapsed / reference frame, or the
    // failure reason. Nothing is drawn under the pyramid method (it produces no depth map,
    // and a leftover texture only misleads) -- the last guard of "no fake preview".
    if state.focus_method != Method::Pyramid
        && let Some(tex) = state.focus_depth_tex.as_ref()
    {
        let size = tex.size_vec2();
        let avail = ui.available_width();
        let scale = if size.x > 0.0 && avail > 0.0 {
            (avail / size.x).min(1.0)
        } else {
            1.0
        };
        ui.add(egui::Image::new((tex.id(), size * scale)));
    }
    if let Some(note) = state.focus_depth_note.clone() {
        ui.label(RichText::new(note).size(SIZE_SMALL).color(dim()));
    }
    // 参数或取帧动了 → 旧预览作废（见函数头的快照对账）；合并参数是全局偏好，
    // 真的变了就置落盘脏标记（与 stack_group 同一口径）。
    if focus_snapshot
        != (
            state.focus_around,
            state.focus_method,
            state.focus_radius,
            state.focus_threshold,
            state.focus_smooth_z,
            state.focus_feather,
            state.focus_align,
        )
    {
        state.focus_depth_tex = None;
        state.focus_depth_note = None;
        state.settings_dirty = true;
    }
}

/// 「时间合成」分组：天地合成或时间切片，一次把不同时刻铺进一张。
///
/// Time-blending group: sky-ground or time slice, laying several moments into one frame.
fn timeblend_group(state: &mut AppState, ui: &mut Ui, ctx: &Context) {
    use aevum_core::timeblend::Layout;
    let busy = state.merge_kind == Some(MergeKind::TimeBlend);
    // 快照对账：合并参数是全局偏好，本帧内真的变了才置落盘脏标记（stack_group 有口径说明）。
    let snapshot = (
        state.tb_around,
        state.tb_layout,
        state.tb_bands,
        state.tb_feather,
        state.tb_horizon,
        state.tb_horizon_auto,
        state.tb_deghost,
        state.tb_match,
    );
    ui.horizontal_wrapped(|ui| {
        for layout in Layout::ALL {
            let picked = bar_chip(ui, layout.cn(), state.tb_layout == layout)
                .on_hover_text(crate::i18n::tr(match layout {
                    Layout::SkyGround => {
                        "按地平线分天地：天取最后一帧（日落后），地取第一帧（地面还亮着）"
                    }
                    Layout::Bands => "横向分带：一带取一帧，从早到晚铺满整幅，云被拉成一段时间",
                }))
                .clicked();
            if picked && !busy {
                state.tb_layout = layout;
            }
        }
    });
    let cap = forward_caption(state.tb_around, aevum_core::timeblend::MAX_FRAMES);
    span_row(
        ui,
        crate::i18n::t!("从当前帧起，向后多取"),
        &mut state.tb_around,
        1..=31,
        busy,
        cap,
    );
    if state.tb_layout == Layout::Bands {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("带数"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
            ui.add_enabled(
                !busy,
                egui::DragValue::new(&mut state.tb_bands)
                    .range(1..=32)
                    .speed(0.2),
            );
        });
    } else {
        ui.checkbox(
            &mut state.tb_horizon_auto,
            crate::i18n::t!("自动地平线：按行亮度梯度找天与地的分界"),
        )
        .on_hover_text(crate::i18n::t!(
            "把所有帧逐行平均成亮度剖面，找最强的纵向台阶；找不到（阴天平光、纯天空）退回手动值"
        ));
        if !state.tb_horizon_auto {
            let mut h = state.tb_horizon as f64;
            if slider(ui, "tb_horizon", "地平线", &mut h, 0.0, 1.0, 0.5, 2, false) {
                state.tb_horizon = h as f32;
            }
        }
    }
    // 去鬼影走 slider_row：提示语挂在滑杆上（tip 参数），0 = 关闭。
    //
    // Deghosting goes through slider_row so the tip rides on the slider itself; 0 = off.
    let mut dg = state.tb_deghost as f64;
    if slider_row(
        ui,
        "tb_deghost",
        "去鬼影",
        &mut dg,
        0.0,
        2.0,
        0.5,
        2,
        false,
        "",
        None,
        false,
        "0 = 关闭。羽化区里云、树、车流这类移动物会拖成两份；某帧某块与自己邻域的偏离超过这个值（EV）就被剔掉，换干净帧顶上",
    ) {
        state.tb_deghost = dg as f32;
    }
    let mut f = state.tb_feather as f64;
    if slider(ui, "tb_feather", "羽化", &mut f, 0.0, 0.5, 0.04, 3, false) {
        state.tb_feather = f as f32;
    }
    ui.checkbox(
        &mut state.tb_match,
        crate::i18n::t!("曝光匹配：先把来源帧亮度对齐到参考帧，接缝才没有台阶"),
    );
    let go = ui
        .add_enabled(
            !busy,
            egui::Button::new(RichText::new(if busy {
                crate::i18n::t!("合成中…")
            } else {
                crate::i18n::t!("开始合成")
            }))
            .min_size(Vec2::new(0.0, BAR_H))
            .corner_radius(radius_control()),
        )
        .on_hover_text(crate::i18n::t!(
            "把当前帧起若干帧按所选布局合成为一张，写进输出目录"
        ))
        .clicked();
    if go {
        let args = serde_json::json!({
            "around": state.tb_around,
            "layout": state.tb_layout.key(),
            "bands": state.tb_bands,
            "feather": state.tb_feather,
            "horizon": state.tb_horizon,
            "horizon_auto": state.tb_horizon_auto,
            "deghost": state.tb_deghost,
            "match_exposure": state.tb_match,
            "confirm": true,
        });
        if let Err(e) = state.run_ui_command(ctx, "timeblend.merge", &args) {
            state.tb_error = Some(e);
            state.tb_last = None;
        }
    }
    merge_progress_row(state, ui, MergeKind::TimeBlend);
    if let Some(last) = state.tb_last.clone() {
        ui.label(
            RichText::new(crate::i18n::tr_fmt("已写出 {}", &[&last]))
                .size(SIZE_SMALL)
                .color(accent()),
        );
    }
    if let Some(err) = state.tb_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
    if snapshot
        != (
            state.tb_around,
            state.tb_layout,
            state.tb_bands,
            state.tb_feather,
            state.tb_horizon,
            state.tb_horizon_auto,
            state.tb_deghost,
            state.tb_match,
        )
    {
        state.settings_dirty = true;
    }
    // 预览：参数或帧选择变了就自动在后台重算一张半分辨率小图，按可用宽度缩放展示。
    state.timeblend_preview_tick(ctx);
    if let Some((_, tex)) = &state.tb_preview {
        let size = tex.size_vec2();
        let avail = ui.available_width();
        let scale = if size.x > 0.0 && avail > 0.0 {
            (avail / size.x).min(1.0)
        } else {
            1.0
        };
        ui.add(egui::Image::new((tex.id(), size * scale)));
    }
    if state.tb_preview_inflight {
        ui.label(
            RichText::new(crate::i18n::t!("预览计算中…"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
    }
    if let Some(err) = state.tb_preview_error.clone() {
        ui.label(RichText::new(err).size(SIZE_SMALL).color(danger()));
    }
}

/// 合并任务的进度条 + 取消按钮：只在这一类任务在跑时出现。
/// 「读取 k / n+1」的最后一格留给融合或拼接加写盘，那段没法细分进度。
fn merge_progress_row(state: &mut AppState, ui: &mut Ui, kind: MergeKind) {
    let Some((running, done, total)) = state.merge_progress() else {
        return;
    };
    if running != kind {
        return;
    }
    let frac = if total > 0 {
        done as f32 / total as f32
    } else {
        0.0
    };
    ui.add(
        egui::ProgressBar::new(frac)
            .desired_height(4.0)
            .text(RichText::new(format!("{done} / {total}")).size(SIZE_SMALL)),
    );
    if bar_button(ui, "取消合并").clicked() {
        state.stop_merge();
    }
}

/// 合成模块：HDR / 全景 / 星空堆栈 / 时间合成 / 焦点合成。
///
/// 2026-11-04 从显影面板整体迁出（docs/界面布局-ACR化-2026-11-04.md §4）：
/// 「多帧进来、一张图出去」的文件级工具不该排在调色推子的尾巴上。函数体只是
/// 五个既有组的重新集合，逻辑零改动。
///
/// The Merge module: HDR / panorama / star-stack / time-blend / focus-stack. Moved
/// wholesale out of Develop on 2026-11-04 (plan §4): file-level "frames in, one image
/// out" tools do not belong at the tail of the grading sliders. This body merely
/// re-collects the five existing groups; zero logic changes.
pub(super) fn adjust_merge(state: &mut AppState, ui: &mut Ui) {
    ui.add_space(4.0);
    let ctx = ui.ctx().clone();
    if group_header(ui, state, "hdr", "HDR 合并", false) {
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
                hdr_group(state, ui, &ctx);
            });
    }
    if group_header(ui, state, "pano", "全景合并", false) {
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
                pano_group(state, ui, &ctx);
            });
    }
    if group_header(ui, state, "stack", "星空堆栈", false) {
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
                stack_group(state, ui, &ctx);
            });
    }
    if group_header(ui, state, "timeblend", "时间合成", false) {
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
                timeblend_group(state, ui, &ctx);
            });
    }
    if group_header(ui, state, "focus", "焦点合成", false) {
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
                focus_group(state, ui, &ctx);
            });
    }
}

#[cfg(test)]
/// 焦点组的「先对齐」提示与取帧说明的算术。
mod focus_hint_tests {
    use super::*;
    use aevum_core::align::{AlignReport, CropRect};

    /// 一张 48×48 的临时序列，够焦点组合成用。
    fn open_three_frames(state: &mut AppState, ctx: &Context, tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aevum-hint-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        for i in 0..3u32 {
            let img = image::RgbImage::from_fn(48, 48, |x, _y| {
                let v = if (x / 2).is_multiple_of(2) { 200u8 } else { 60 };
                image::Rgb([v, v, v.wrapping_add(i as u8)])
            });
            img.save(dir.join(format!("f{i:03}.png"))).expect("写帧");
        }
        state
            .run_ui_command(
                ctx,
                "sequence.open",
                &serde_json::json!({"root": dir.display().to_string()}),
            )
            .expect("打开序列");
        dir
    }

    /// 该提示的时候提示，不该提示的时候闭嘴：没序列、只取 2 帧、已经对齐过。
    ///
    /// The hint shows exactly when it should: no sequence, a two-frame bracket and an
    /// already-aligned sequence all stay quiet.
    #[test]
    fn the_align_hint_shows_only_for_a_real_unaligned_bracket() {
        let ctx = Context::default();
        let mut state = AppState::new();
        // 新状态：没有序列 → 什么都不说。
        assert!(state.seq.is_none(), "新状态本来就没有序列");
        assert!(focus_align_hint(&state).is_none(), "没有序列时不该提对齐");
        // 挂上序列、还没对齐、默认 around = 7（共 8 帧）→ 该提示。
        let dir = open_three_frames(&mut state, &ctx, "hint");
        let hint = focus_align_hint(&state).expect("没对齐就该提示");
        assert!(hint.contains("对齐"), "提示里得说清该干什么：{hint}");
        // 只取 2 帧：谈不上过渡带。
        state.focus_around = 1;
        assert!(focus_align_hint(&state).is_none(), "2 帧也提示，是噪音");
        // 对齐过了就别再提。
        state.focus_around = 7;
        state.align_report = Some(AlignReport {
            size: [48, 48],
            offsets: vec![[0.0, 0.0]; 3],
            scores: vec![1.0; 3],
            issues: Vec::new(),
            crop: CropRect {
                x: 0,
                y: 0,
                w: 48,
                h: 48,
            },
            elapsed_secs: 0.0,
        });
        assert!(focus_align_hint(&state).is_none(), "对齐过了还在提示");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 取帧说明必须把这个数换算成真正的帧数：前后各 N 是 2N+1，只往后 N 是 N+1。
    ///
    /// The caption must turn the knob value into the real frame count: both sides is
    /// 2N+1, forward only is N+1.
    #[test]
    fn the_span_caption_states_the_real_frame_count() {
        let both = both_sides_caption(4);
        assert!(both.contains('9'), "前后各 4 帧共 9 帧：{both}");
        assert!(!both.contains("上限"), "前后各取没有硬上限：{both}");
        let forward = forward_caption(7, aevum_core::focus::MAX_FRAMES);
        assert!(forward.contains('8'), "往后 7 帧共 8 帧：{forward}");
        assert!(
            forward.contains(&aevum_core::focus::MAX_FRAMES.to_string()),
            "说明里要带上硬上限：{forward}"
        );
        // 负向对照：把常见误读（把旋钮值当帧数）写进去就会露馅。
        //
        // Negative control: the usual misreading -- taking the knob value for the frame
        // count -- would show up right here.
        assert_ne!(
            forward_caption(7, aevum_core::focus::MAX_FRAMES),
            forward_caption(8, aevum_core::focus::MAX_FRAMES),
            "不同帧数必须给出不同说明"
        );
    }

    /// 三态状态行：只说真话、只说一次。
    ///
    /// `off` 与缺字段一律不出行（没跑不值得占一行）；`adopted` 用强调色并给出带符号的
    /// 百分比；回退用次要色；**失败必须是危险色**——把失败画成成功是这条判据守的东西。
    ///
    /// The three-state row tells the truth, once: `off` and a missing field emit nothing
    /// ("it did not run" is not worth a line); `adopted` uses the accent colour and a signed
    /// percentage; a revert uses the secondary colour; **a failure must be the danger
    /// colour** -- dressing a failure as success is exactly what this assertion guards.
    #[test]
    fn the_status_row_reports_three_states_and_stays_silent_when_off() {
        let mut state = AppState::new();
        // 没有结果 → 不出行。
        assert!(focus_align_status_row(&state).is_none(), "没结果不该出行");
        // 非焦点合并的结果 → 也不出行（同一个 `merge_result` 字段被五种合并共用）。
        //
        // A non-focus result emits nothing either: all five merge kinds share one
        // `merge_result` field.
        state.merge_result = Some(serde_json::json!({"kind": "hdr", "align_status": "adopted"}));
        assert!(
            focus_align_status_row(&state).is_none(),
            "HDR 的结果不该借用这一行"
        );

        // 夹具里**不再**放一个独立的 reason 字段：`FocusAlignReport` 早就不带它了，
        // 结果 JSON 也不发它，面板是从 `align_status` 选整句、从 `align_gain` 取数字。
        // 夹具里留着那个字段会让后来的人以为面板读它——QA 做过实验，把字段全删掉这几条
        // 判据仍然全绿，说明它一直是摆设。
        //
        // The fixtures deliberately carry **no** separate reason field: the report has not
        // carried one for a while, the result JSON never emits one, and the panel picks a
        // whole sentence from `align_status` plus a number from `align_gain`. Keeping that
        // field in the fixture would teach the next reader that the panel reads it; QA
        // deleted them and every assertion here still passed, so it was decoration.
        state.merge_result = Some(serde_json::json!({
            "kind": "focus", "align_status": "off", "align_gain": 0.0,
        }));
        assert!(focus_align_status_row(&state).is_none(), "off 不该出行");

        state.merge_result = Some(serde_json::json!({
            "kind": "focus", "align_status": "adopted", "align_gain": 1.110231,
        }));
        let (text, colour) = focus_align_status_row(&state).expect("adopted 必须出行");
        assert!(text.contains("已采用"), "文案要说清采用了：{text}");
        assert!(text.contains("+111.0%"), "增益要带符号保留一位小数：{text}");
        assert_eq!(colour, accent(), "采用是好事，用强调色");

        state.merge_result = Some(serde_json::json!({
            "kind": "focus", "align_status": "reverted_insufficient_gain",
            "align_gain": -0.018796,
        }));
        let (text, colour) = focus_align_status_row(&state).expect("回退必须出行");
        assert!(text.contains("回退"), "文案要说清回退了：{text}");
        assert!(text.contains("-1.9%"), "负增益也要带符号：{text}");
        assert_eq!(colour, dim(), "回退是中性结果，不用危险色");

        state.merge_result = Some(serde_json::json!({
            "kind": "focus", "align_status": "failed", "align_gain": 0.0,
        }));
        let (text, colour) = focus_align_status_row(&state).expect("失败必须出行");
        assert!(text.contains("失败"), "失败要直说：{text}");
        assert_eq!(colour, danger(), "失败必须是危险色，不许画成成功");
        assert_ne!(colour, accent(), "危险色不能与强调色相同");
    }
}
