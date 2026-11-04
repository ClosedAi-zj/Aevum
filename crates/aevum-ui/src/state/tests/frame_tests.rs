use super::*;

/// 浮窗位置的记录规则：拖动中不记、同值不置脏、真移动了才置脏。
///
/// 这三条合起来才是「能跨启动记住位置」而不「每帧写一次文件」：
/// 拖动中坐标每帧都变，记了就变成写风暴；同值重复记会让脏标记永远亮着，
/// 帧尾保存每帧跑一次。
///
/// The rules for recording a window position: not while dragging, not for a repeat
/// value, and only a real move sets the dirty flag. Together they mean "remember the
/// position across launches" without "write the file every frame".
#[test]
fn window_position_is_recorded_only_after_the_drag_ends() {
    let mut state = AppState::new();
    state.settings_dirty = false;

    // 按着键拖到 A：不记。
    state.note_win_pos("market", egui::pos2(200.0, 120.0), true);
    assert!(state.win_pos.is_empty(), "拖动中就记了位置");
    assert!(!state.settings_dirty, "拖动中置了脏标记");

    // 松手后落在 A：记下来并置脏。
    state.note_win_pos("market", egui::pos2(200.0, 120.0), false);
    assert_eq!(state.win_pos.get("market"), Some(&[200.0, 120.0]));
    assert!(state.settings_dirty);

    // 同一个位置再来一帧：不许再置脏（否则帧尾保存每帧跑一次）。
    state.settings_dirty = false;
    state.note_win_pos("market", egui::pos2(200.0, 120.0), false);
    assert!(!state.settings_dirty, "同值重复记录了");

    // 拖到 B：脏标记必须重新亮。
    state.note_win_pos("market", egui::pos2(640.0, 360.0), false);
    assert_eq!(state.win_pos.get("market"), Some(&[640.0, 360.0]));
    assert!(state.settings_dirty);
}

/// 选了 HDR 之后，色域与容器必须被补齐到唯一成立的组合。
///
/// 这条挡的是「面板让用户选一个必然失败的组合」：HDR 配 PNG 会在按下导出之后
/// 才被 core 的守卫拒绝，用户已经在等结果了才挨一句错误。`apply_dynamic` 把
/// 组合补齐（可见地改，不许偷偷改），所以这里逐项断言补齐后的三个值。
///
/// Choosing HDR must fill in the only valid combination. This stops the panel from
/// letting the user pick something that can only fail: HDR with PNG is rejected by core's
/// guard only after the export button is pressed, when the user is already waiting for a
/// result. `apply_dynamic` fills the combination in (visibly, never silently), so all
/// three values are asserted after the call.
#[test]
fn hdr_forces_rec2020_and_avif() {
    let mut state = AppState::new();
    // 默认模式是**成片**（`AppState::new` 里就是 Video），而这条判的是静帧那条路，
    // 所以必须先显式切过来——否则它验的是成片的补齐逻辑，名字与实际不符。
    //
    // The default mode is **video** (that is what `AppState::new` sets), and this test is
    // about the stills path, so the mode has to be switched explicitly first; otherwise it
    // measures the video fill-in while claiming to measure stills.
    state.render_mode = OutputMode::Stills;
    state.render_dynamic = OutputDynamic::Sdr;
    state.render_color_space = ColorSpace::Srgb;
    state.render_format = StillFormat::Png8;

    state.render_dynamic = OutputDynamic::HdrPq;
    state.apply_dynamic();
    assert_eq!(state.render_color_space, ColorSpace::Rec2020);
    assert_eq!(
        state.render_format,
        StillFormat::Avif,
        "HDR 配 PNG 会被 core 拒绝，面板必须当场改掉"
    );

    // 回到 SDR 不许把用户的容器选择一起翻掉（只补不成立的组合）。
    state.render_dynamic = OutputDynamic::Sdr;
    state.apply_dynamic();
    assert_eq!(state.render_format, StillFormat::Avif, "SDR 不需要改容器");
}

/// 成片选 HDR 时，编码器与色域必须被补到唯一成立的组合（HEVC + Rec.2020）。
///
/// 与静帧那条（AVIF + Rec.2020）分开断言，因为它们的「装得下的容器」不是一个东西：
/// 静帧是 AVIF，成片是 H.265。共用一条判据会让其中一条路悄悄退回默认。
///
/// Choosing HDR for video must fill in the only valid combination (HEVC + Rec.2020).
/// Asserted separately from the stills case (AVIF + Rec.2020) because "the container that
/// can hold it" differs: AVIF for stills, H.265 for video. One shared criterion would let
/// either path quietly fall back to defaults.
#[test]
fn hdr_for_video_forces_hevc_and_rec2020() {
    let mut state = AppState::new();
    state.render_mode = OutputMode::Video;
    state.codec = "H.264".to_owned();
    state.render_color_space = ColorSpace::Srgb;
    state.render_dynamic = OutputDynamic::HdrPq;
    state.apply_dynamic();
    assert_eq!(state.codec, "HEVC", "HDR 成片还挂在 H.264 上，导出必被拒");
    assert_eq!(state.render_color_space, ColorSpace::Rec2020);

    // 静帧那条不许被成片的规则带跑。
    state.render_mode = OutputMode::Stills;
    state.render_format = StillFormat::Png8;
    state.apply_dynamic();
    assert_eq!(state.render_format, StillFormat::Avif);
}

/// 「预览白以上」是观看方式，必须跟着用户走：进 settings 载荷、读回来还开着。
///
/// 老文件没有这一项时按**关**处理——不能默认开，否则所有人第一次打开都会看到
/// 略微压暗的白，还以为调色坏了。
///
/// "Preview above white" is a way of looking and must travel with the user: into the
/// settings payload, back on after relaunch. An old file without the field means **off** —
/// defaulting to on would dim everyone's whites on first launch and read as a broken grade.
#[test]
fn hdr_preview_toggle_persists_and_defaults_to_off() {
    let mut state = AppState::new();
    assert!(!state.hdr_preview, "默认必须是关的");

    state.hdr_preview = true;
    let saved = state.settings();
    assert_eq!(saved.hdr_preview, Some(true), "没进 settings 载荷");

    let mut fresh = AppState::new();
    fresh.adopt_settings(saved);
    assert!(fresh.hdr_preview, "读回来关了");

    let mut old_file = AppState::new();
    old_file.hdr_preview = true;
    old_file.adopt_settings(crate::settings::Settings::default());
    assert!(!old_file.hdr_preview, "老文件缺这一项时应当是关");
}

/// 预设栏三节：拖动换序 → 进 settings 载荷 → 读回来还是那个次序。
///
/// 顺序判据用**字面值**，不用「和默认一样」这种相对说法：一起改了默认次序的话，
/// 相对判据会跟着一起漂。
///
/// The three preset sections: drag to reorder -> into the settings payload -> read back
/// in that order. The assertion uses **literal** values rather than "same as default",
/// which would drift along if someone changed the default.
#[test]
fn preset_sections_reorder_and_persist() {
    let mut state = AppState::new();
    assert_eq!(state.preset_order, vec!["builtin", "snapshots", "copies"]);

    state.settings_dirty = false;
    state.move_preset_section("copies", 0);
    assert_eq!(state.preset_order, vec!["copies", "builtin", "snapshots"]);
    assert!(state.settings_dirty, "换序后没置脏，关掉就丢");

    // 拖回原位不算改动。
    state.settings_dirty = false;
    state.move_preset_section("copies", 0);
    assert!(!state.settings_dirty, "原地放下也置了脏");

    // 越界夹住，不是崩掉也不是乱插。
    state.move_preset_section("builtin", 99);
    assert_eq!(state.preset_order, vec!["copies", "snapshots", "builtin"]);

    let saved = state.settings();
    assert_eq!(
        saved.preset_order.as_deref(),
        Some(
            [
                "copies".to_owned(),
                "snapshots".to_owned(),
                "builtin".to_owned()
            ]
            .as_slice()
        )
    );
    let mut fresh = AppState::new();
    fresh.adopt_settings(saved);
    assert_eq!(fresh.preset_order, vec!["copies", "snapshots", "builtin"]);
}

/// 落盘的次序可能是坏的：老文件没有这一项、手删一节、手写未知键、重复。
/// 四种都得开得起来，并且结果恒为完整排列（界面那边就不必再防御）。
///
/// The persisted order can be bad: absent in an old file, a section deleted by hand, an
/// unknown key typed in, a duplicate. All four must still open, and the result is always
/// a full permutation, so the drawing side needs no defensive code.
#[test]
fn preset_order_from_a_broken_file_is_normalised() {
    let norm =
        |v: &[&str]| normalise_preset_order(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    // 老文件 / 空：回到默认次序。
    assert_eq!(norm(&[]), vec!["builtin", "snapshots", "copies"]);
    // 2026-11-04 起历史一节删除（dock「历史」页接管），老落盘里的 "history"
    // 是未知键：丢掉、缺的 "copies" 按默认次序补到末尾。
    //
    // The history section was removed on 2026-11-04 (the dock's History tab takes
    // over); a persisted "history" key is unknown now: dropped, with the missing
    // "copies" appended in default order.
    assert_eq!(
        norm(&["history", "builtin"]),
        vec!["builtin", "snapshots", "copies"]
    );
    // 手删一节：缺的按默认次序补到末尾。
    assert_eq!(
        norm(&["copies", "builtin"]),
        vec!["copies", "builtin", "snapshots"]
    );
    // 未知键：丢掉。
    assert_eq!(
        norm(&["bogus", "builtin", "snapshots"]),
        vec!["builtin", "snapshots", "copies"]
    );
    // 重复：只留第一次。
    assert_eq!(
        norm(&["snapshots", "snapshots", "copies"]),
        vec!["snapshots", "copies", "builtin"]
    );
    // 全是未知键：等于空。
    assert_eq!(norm(&["x", "y"]), vec!["builtin", "snapshots", "copies"]);
}

/// 位置要真的进 `settings.json` 的载荷，并且在读回来之后灌进状态。
#[test]
fn window_positions_go_through_settings() {
    let mut state = AppState::new();
    state.note_win_pos("look", egui::pos2(24.0, 68.0), false);
    let saved = state.settings();
    assert_eq!(
        saved.win_pos.as_ref().and_then(|m| m.get("look")),
        Some(&[24.0, 68.0]),
        "位置没进 settings 载荷"
    );

    let mut fresh = AppState::new();
    fresh.adopt_settings(saved);
    assert_eq!(fresh.win_pos.get("look"), Some(&[24.0, 68.0]));
}

fn rect(c: CropRect) -> (u32, u32, u32, u32) {
    (c.x, c.y, c.w, c.h)
}

/// 画幅取景必须居中、贴边、不越界，且和画布上画的框同一套数；
/// 四条边全部成对，4:2:0 编码才不会偷偷挪一像素。
#[test]
fn aspect_crop_centers_inside_the_frame() {
    let res = Some((6000, 4000));
    assert_eq!(
        rect(fit_aspect(None, res, Aspect::Wide).unwrap()),
        (0, 312, 6000, 3374)
    );
    assert_eq!(
        rect(fit_aspect(None, res, Aspect::Vertical).unwrap()),
        (1874, 0, 2250, 4000)
    );
    assert_eq!(
        rect(fit_aspect(None, res, Aspect::Square).unwrap()),
        (1000, 0, 4000, 4000)
    );
    assert!(fit_aspect(None, res, Aspect::Native).is_none());
    assert!(fit_aspect(None, None, Aspect::Wide).is_none());
}

/// 对齐裁切先叠一层：画幅在对齐后的框里内接，不能跑出对齐框。
#[test]
fn aspect_crop_nests_inside_alignment_crop() {
    let base = CropRect {
        x: 100,
        y: 200,
        w: 5000,
        h: 3000,
    };
    let c = fit_aspect(Some(base), Some((6000, 4000)), Aspect::Square).unwrap();
    assert_eq!(rect(c), (1100, 200, 3000, 3000));
    let wide = fit_aspect(Some(base), Some((6000, 4000)), Aspect::Wide).unwrap();
    assert_eq!(rect(wide), (100, 294, 5000, 2812));
    assert!(wide.y + wide.h <= base.y + base.h, "不许越出对齐裁切");
}

/// 拖框交互走的是 `aevum_core::crop` 那一套几何：拉、推、挪、微调都留在画面内，
/// 坐标成对；松手拖满一格退回默认取景，不白占一次重画。
#[test]
fn crop_drag_roundtrip_uses_the_core_geometry() {
    let mut state = AppState::new();
    state.resolution = Some((60, 48));
    assert_eq!(rect(state.crop_box()), (0, 0, 60, 48), "没裁过就是整帧");

    state.crop_begin([20.0, 20.0], crop::Hit::Outside);
    state.crop_drag_to([40.0, 40.0]);
    state.crop_end();
    assert_eq!(state.user_crop.map(rect), Some((20, 20, 20, 20)));

    state.crop_begin([25.0, 25.0], crop::Hit::Inside);
    state.crop_drag_to([30.0, 35.0]);
    // 平移后左上角照样取偶：25 落回 24，这是 Python 侧同一条规则。
    assert_eq!(state.user_crop.map(rect), Some((24, 28, 20, 20)));

    state.crop_begin([45.0, 44.0], crop::Hit::Corner(crop::Handle::Se));
    state.crop_drag_to([50.0, 45.0]);
    state.crop_end();
    assert_eq!(
        state.user_crop.map(rect),
        Some((24, 28, 26, 16)),
        "拉右下角只动那两边，尺寸取偶"
    );

    state.crop_nudge(-1, 1, 4);
    assert_eq!(state.user_crop.map(rect), Some((20, 32, 26, 16)));

    state.crop_begin([0.0, 0.0], crop::Hit::Outside);
    state.crop_drag_to([60.0, 48.0]);
    state.crop_end();
    assert_eq!(state.user_crop, None);

    // 没有用户框时，画幅比例就是取景框，和画布上画的同一套数（顶边取偶后 6）。
    state.set_aspect(Aspect::Wide);
    assert_eq!(rect(state.crop_box()), (0, 6, 60, 34));

    // 用户框优先，比例只负责在它里面内接；改比例会把框绕中心重铺。
    state.user_crop = Some(CropRect {
        x: 10,
        y: 10,
        w: 40,
        h: 30,
    });
    // f32 的比例精度决定这里落到 22 而不是 23，取景只需保持居中且不越出用户框。
    assert_eq!(rect(state.effective_crop().unwrap()), (10, 14, 40, 22));
    state.set_aspect(Aspect::Square);
    // 重铺后的左上角照样取偶：5 落回 4，与 Python 的 norm_rect 同一口径。
    assert_eq!(state.user_crop.map(rect), Some((10, 4, 40, 40)));
    assert_eq!(rect(state.effective_crop().unwrap()), (10, 4, 40, 40));
}

/// 一次拖动 = 一步历史：拖框的中间态走登记键（`crop-drag`），松手把铺满的框
/// 退回默认走字段比对——两条路并成一步，不许「裁剪框 / 裁剪 4000 × 6000」
/// 成对刷屏（v1.0.1 用户实测的历史面板就是这副样子）。
#[test]
fn crop_drag_and_release_end_up_as_one_history_step() {
    let mut state = AppState::new();
    state.resolution = Some((60, 48));
    let base = state.history.steps().len();

    // 一次完整的「拉一个铺满画面的框」：摆姿态、三帧拖动、松手退回默认。
    state.history.begin_frame();
    state.crop_begin([0.0, 0.0], crop::Hit::Outside);
    state.history_commit();

    for (x, y) in [(20.0, 20.0), (50.0, 40.0), (60.0, 48.0)] {
        state.history.begin_frame();
        state.crop_drag_to([x, y]);
        state.history_commit();
    }

    state.history.begin_frame();
    state.crop_end();
    state.history_commit();

    let labels: Vec<_> = state.history.steps().iter().map(|s| &s.label).collect();
    assert_eq!(
        state.history.steps().len(),
        base + 1,
        "整次拖动只允许落一步，实际落了这些：{labels:?}"
    );
    assert!(
        labels.last().is_some_and(|l| l.contains("裁剪")),
        "那一步的文案应该跟着框的尺寸走：{labels:?}"
    );
}

/// 拉直之后，画布上能拖的范围只有安全框：伸进空白角的那部分当场被削回来，
/// 画布上的框和导出的框才是同一个矩形。
#[test]
fn crop_drag_stays_inside_the_straighten_safe_box() {
    let mut state = AppState::new();
    state.resolution = Some((60, 48));
    state.set_rotate(12.0);
    let view = state.crop_view();
    assert!(
        view.x > 0 && view.y > 0 && view.w < 60 && view.h < 48,
        "转过 12° 后安全框应收进画面里"
    );
    assert_eq!(rect(state.crop_box()), rect(view), "没拖过框就是可见区");

    // 从角上拖满整个画面：削边后正好铺满安全框，等价于没裁。
    state.crop_begin([0.0, 0.0], crop::Hit::Outside);
    state.crop_drag_to([60.0, 48.0]);
    state.crop_end();
    assert_eq!(state.user_crop, None, "铺满可见区不该留下一层白裁");

    // 只拖左上半幅：左上角在空白里，落点得顶到安全框的左沿和上沿。
    state.crop_begin([0.0, 0.0], crop::Hit::Outside);
    state.crop_drag_to([30.0, 24.0]);
    state.crop_end();
    let box_ = state.user_crop.expect("半幅拖框应留下手动取景");
    assert_eq!((box_.x, box_.y), (view.x, view.y), "被削到安全框的左上角");
    assert!(box_.x + box_.w <= view.x + view.w && box_.y + box_.h <= view.y + view.h);
    assert_eq!(
        rect(state.effective_crop().unwrap()),
        rect(box_),
        "导出框与画布框同源"
    );

    // 先拉直再加大角度：已有的框得跟着往里收，不能留在空白角上。
    state.set_rotate(0.0);
    state.user_crop = Some(CropRect {
        x: 2,
        y: 2,
        w: 56,
        h: 44,
    });
    state.set_rotate(25.0);
    let safe = state.crop_view();
    let cur = state.user_crop.expect("削边不该把框清没");
    assert!(
        cur.x >= safe.x
            && cur.y >= safe.y
            && cur.x + cur.w <= safe.x + safe.w
            && cur.y + cur.h <= safe.y + safe.h,
        "角度加大后框不许伸进空白角"
    );
}

/// 拖取景框走的是裁剪那套几何，只是落账写进归一化的两端：像素框 ↔ 归一化
/// 必须往返一致，画布上的框和导出里的窗口才是同一个矩形。
#[test]
fn motion_drag_lays_the_box_on_the_output_aspect() {
    let mut state = AppState::new();
    state.resolution = Some((60, 48));
    state.output_w = 60;
    state.output_h = 48;
    state.set_motion_picking(true);
    assert!(state.motion_editing(), "拖取景框态接手画布");
    // 默认终点往里收一档：按 5:4 取偶后落在 (4,4,50,40)。
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::End)),
        (4, 4, 50, 40),
        "起点整幅、终点内收，一开就看得见幅度"
    );

    state.motion_begin([10.0, 10.0], crop::Hit::Outside);
    state.motion_drag_to([35.0, 30.0]);
    state.motion_end();
    assert!(state.motion.enabled, "给了框就当真空开运镜");
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::End)),
        (10, 10, 20, 16),
        "定比例拉框：两轴取更大的那个推进，宽高成对"
    );
    // 归一化写回再换算回来，一个像素都不差。
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::End)),
        (10, 10, 20, 16)
    );

    state.motion_begin([20.0, 20.0], crop::Hit::Inside);
    state.motion_drag_to([25.0, 28.0]);
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::End)),
        (14, 18, 20, 16),
        "框内平移只挪位置，左上角照样取偶"
    );

    state.motion_begin([35.0, 34.0], crop::Hit::Corner(crop::Handle::Se));
    state.motion_drag_to([45.0, 42.0]);
    state.motion_end();
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::End)),
        (14, 18, 30, 24),
        "拉右下角以对角为锚，比例不破"
    );

    state.motion_nudge(1, -1, 2);
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::End)),
        (16, 16, 30, 24)
    );

    // 起点那一端照旧：拖终点不该把起点带跑。
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::Start)),
        (0, 0, 60, 48)
    );
}

/// 取景框归一化的基准就是导出取景那一幅：画幅一改，基准自己换形，框按份额跟着走，
/// 不需要再重铺一次；小图撑不下定比例的最小步长时退回自由比例。
#[test]
fn motion_box_follows_the_export_framing_basis() {
    let mut state = AppState::new();
    state.resolution = Some((60, 48));
    // 输出画幅跟基准同一个比例（60:34 → 30:17），框才铺得满整幅。
    state.output_w = 60;
    state.output_h = 34;
    state.set_motion_picking(true);
    state.set_aspect(Aspect::Wide);
    let base = state.motion_base_rect();
    assert_eq!(rect(base), (0, 6, 60, 34), "基准跟着画幅换形");
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::Start)),
        rect(base),
        "起点铺满基准 = 画布上看到的就是那一条"
    );

    // 从画面外拖进来：框只能落在基准那一幅里，越界的部分被夹回边沿。
    state.motion_begin([-8.0, -8.0], crop::Hit::Outside);
    state.motion_drag_to([70.0, 60.0]);
    state.motion_end();
    assert_eq!(rect(state.motion_box_rect(MotionBox::End)), rect(base));
    assert_eq!(state.motion.to, crop::FULL, "铺满基准就是整幅，不留一圈白");
    assert!(
        !state.motion.is_active(),
        "两框重合时导出走快路，一像素都不重采样"
    );

    // 输出画幅约完仍撑不进基准（小图配超宽比）：退回自由比例，别把框夹飞。
    state.set_aspect(Aspect::Native);
    state.output_w = 4096;
    state.output_h = 2160;
    state.motion_begin([10.0, 10.0], crop::Hit::Outside);
    state.motion_drag_to([35.0, 30.0]);
    state.motion_end();
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::End)),
        (10, 10, 24, 20),
        "自由比例：框就是两个角夹出来的那块，宽高取偶"
    );

    // 铺满按钮直接把当前那一端顶到整幅，不用把手拉半天。
    state.motion_box = MotionBox::Start;
    state.motion_fill_box();
    assert_eq!(
        rect(state.motion_box_rect(MotionBox::Start)),
        (0, 0, 60, 48)
    );
}

/// 裁剪框与运镜框抢的是同一只手鼠标：谁开谁拿把手，另一头的半截拖拽当场作废。
#[test]
fn motion_picking_and_crop_never_share_the_mouse() {
    let mut state = AppState::new();
    state.resolution = Some((60, 48));
    state.set_motion_picking(true);
    assert!(state.motion_editing());

    // 切裁剪工具就退出取景框编辑：两头都要把手，只能留一个。
    state.set_tool(Tool::Crop);
    assert!(!state.motion_picking, "选裁剪工具就退出取景框编辑");
    assert!(state.crop_editing() && !state.motion_editing());

    // 反过来：在裁剪态里按「拖取景框」，裁剪工具当场放下，运镜才握得住把手。
    state.set_module(Module::Frame);
    state.set_motion_picking(true);
    assert_eq!(state.tool, Tool::Hand, "切回运镜时把裁剪工具放下");
    assert!(state.motion_picking);
    assert!(
        !state.motion_editing(),
        "构图模块里裁剪框优先，运镜框让开一层"
    );
    state.set_module(Module::Render);
    assert!(state.motion_editing(), "回到成片模块运镜框接手");

    // 另一头的半截拖拽不能留在手里。
    state.crop_begin([10.0, 10.0], crop::Hit::Outside);
    assert!(state.crop_drag.is_some());
    state.set_motion_picking(false);
    state.set_motion_picking(true);
    assert!(state.crop_drag.is_none(), "进运镜就把裁剪那半截丢掉");
    state.motion_begin([0.0, 0.0], crop::Hit::Outside);
    assert!(state.motion_drag.is_some());
    state.set_tool(Tool::Crop);
    assert!(state.motion_drag.is_none(), "出运镜就把取景框那半截丢掉");

    // 离开/进入取景框编辑各重画一次：一次是整幅，一次是框里的内容。
    state.preview_dirty = false;
    state.set_motion_picking(true);
    assert!(state.preview_dirty);
    state.preview_dirty = false;
    state.set_motion_picking(false);
    assert!(state.preview_dirty);
}
