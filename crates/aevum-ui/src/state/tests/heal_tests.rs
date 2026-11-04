//! 修复画笔与透视校正的界面状态口径。
//!
//! 这里只测**界面层**的行为：落点怎么换算、仿制源怎么取、撤销与清空在空表上做不做
//! 动作、以及裁剪态下为什么拒绝落点。像素算法与渲染链的顺序契约在
//! `aevum-core` 的 `retouch` 单元测试与 `tests/suite/retouch_pipeline.rs` 里。
//!
//! UI-state conventions for the healing brush and perspective correction.
//!
//! Only the **interface layer** lives here: how taps map to frame coordinates, how a clone
//! source is picked, whether undo and clear act on an empty list, and why taps are refused
//! under a crop. The pixel algorithm and the render-chain order contract live in
//! `aevum-core`'s `retouch` unit tests and `tests/suite/retouch_pipeline.rs`.

use super::*;

/// 落一点的半径口径：推子是百分比，落盘是**相对长边**的比例；夹在 (0, MAX_RADIUS]。
///
/// Radius convention: the slider is a percentage and the stored value is a ratio of the
/// **long edge**, clamped into (0, MAX_RADIUS].
#[test]
fn a_stroke_takes_the_radius_from_the_slider_at_landing_time() {
    let mut st = AppState::new();
    st.heal_radius = 3.0;
    st.heal_feather = 40.0;
    assert!(st.heal_add((0.25, 0.75)));
    let s = *st.params.retouch.last().expect("应当落下一笔");
    assert!((s.r - 0.03).abs() < 1e-6, "半径口径变了：{}", s.r);
    assert!(
        (s.feather - 0.4).abs() < 1e-6,
        "羽化口径变了：{}",
        s.feather
    );
    assert!((s.x - 0.25).abs() < 1e-6 && (s.y - 0.75).abs() < 1e-6);

    // 改推子只影响下一笔：已经落下的那一点不许跟着变——否则用户调完半径，
    // 前面已经修好的地方会一起重算，看起来像「画面自己变了」。
    //
    // Moving the slider affects only the next stroke: an already-placed one must not follow,
    // otherwise adjusting the radius recomputes everything already repaired and reads as
    // "the picture changed by itself".
    st.heal_radius = 8.0;
    assert!(st.heal_add((0.5, 0.5)));
    assert!((st.params.retouch[0].r - 0.03).abs() < 1e-6, "旧笔触被改了");
    assert!(
        (st.params.retouch[1].r - 0.08).abs() < 1e-6,
        "新笔触没吃到新半径"
    );
}

/// 半径推子超上限（> 10% 长边）时被钳住：单点修复不该变成整幅求解。
///
/// A slider value above the 10%-of-long-edge ceiling is clamped: one spot must not become a
/// whole-frame solve.
#[test]
fn an_over_range_radius_is_clamped_before_it_is_stored() {
    let mut st = AppState::new();
    st.heal_radius = 30.0;
    assert!(st.heal_add((0.5, 0.5)));
    let s = st.params.retouch.last().expect("应当落下一笔");
    assert!(
        s.r <= aevum_core::retouch::SpotSpec::MAX_RADIUS + 1e-6,
        "半径没被钳住：{}",
        s.r
    );
}

/// 仿制模式：第一下只取源、不落笔；第二下起落笔，并且源的偏移按「长边单位」换算。
///
/// Clone mode: the first tap only picks the source; later taps place strokes, with the source
/// offset expressed in long-edge units.
#[test]
fn the_first_clone_tap_picks_a_source_and_the_next_ones_lay_strokes() {
    let mut st = AppState::new();
    st.resolution = Some((4000, 3000));
    st.heal_clone = true;
    assert!(!st.heal_add((0.6, 0.5)), "第一下不该落笔");
    assert!(st.params.retouch.is_empty(), "取源不该产生笔触");
    assert_eq!(st.heal_src, Some((0.6, 0.5)));

    assert!(st.heal_add((0.5, 0.5)), "第二下应当落笔");
    let s = st.params.retouch.last().expect("应当落下一笔");
    // 偏移 = (0.5 - 0.6) * 4000 / 4000 = -0.1（长边 4000 = 宽），y 方向 0。
    //
    // Offset = (0.5 - 0.6) * 4000 / 4000 = -0.1 (the long edge is the width here), y is 0.
    let (dx, dy) = s.source.expect("仿制笔触必须带源偏移");
    assert!((dx + 0.1).abs() < 1e-5, "横向偏移错了：{dx}");
    assert!(dy.abs() < 1e-5, "纵向偏移应当是 0：{dy}");
}

/// 竖幅时偏移必须按高换算：4000×3000 是横幅，换成 3000×4000 之后同一份归一化偏移
/// 在像素域必须指向同一处——这里直接比「按长边单位」的期望值。
///
/// On a portrait frame the offset must be scaled by the height: the same normalized offset has
/// to land on the same pixels, so the expectation is computed against the long edge.
#[test]
fn a_portrait_frame_scales_the_clone_offset_by_its_own_axes() {
    let mut st = AppState::new();
    st.resolution = Some((3000, 4000));
    st.heal_clone = true;
    let _ = st.heal_add((0.5, 0.6));
    assert!(st.heal_add((0.5, 0.5)));
    let s = st.params.retouch.last().expect("应当落下一笔");
    let (dx, dy) = s.source.expect("仿制笔触必须带源偏移");
    assert!(dx.abs() < 1e-5, "横向不该有偏移：{dx}");
    // dy = (0.5 - 0.6) * 4000 / 4000 = -0.1
    assert!((dy + 0.1).abs() < 1e-5, "纵向偏移错了：{dy}");
}

/// 撤销与清空在空表上都是「没做事」：返回 false，且不把状态标脏——否则按一下就把
/// 预览重画一遍，还多一条历史。
///
/// Undo and clear on an empty list both report "nothing happened": `false`, and the state is
/// not marked dirty — otherwise a click redraws the preview and adds a history entry.
#[test]
fn undo_and_clear_are_no_ops_on_an_empty_list() {
    let mut st = AppState::new();
    st.preview_dirty = false;
    assert!(!st.heal_undo());
    assert!(!st.heal_clear());
    assert!(!st.preview_dirty, "空表上的撤销/清空不该标脏");

    st.heal_radius = 2.0;
    assert!(st.heal_add((0.5, 0.5)));
    assert!(st.heal_undo());
    assert!(st.params.retouch.is_empty());
    assert!(!st.heal_undo(), "已经空了，再撤一次不该报成功");
}

/// 落笔会转「自定义」预设并标脏预览：这是用户能看见的后果，不能被静默吞掉。
///
/// Laying a stroke turns the preset custom and marks the preview dirty: those are user-visible
/// consequences and must not be swallowed silently.
#[test]
fn a_stroke_marks_the_preset_custom_and_the_preview_dirty() {
    let mut st = AppState::new();
    st.preset = "通透风光".to_owned();
    st.preview_dirty = false;
    assert!(st.heal_add((0.5, 0.5)));
    assert_eq!(st.preset, "自定义");
    assert!(st.preview_dirty);
}

/// 画布换算必须成对：`canvas_to_frame` 与 `frame_to_canvas` 互逆，全幅下是恒等，
/// 非整幅时中心点仍然落在中心。
///
/// The canvas mapping must come in pairs: `canvas_to_frame` and `frame_to_canvas` are inverses,
/// the identity when the preview shows the whole frame, and the centre stays the centre when
/// it does not.
#[test]
fn canvas_and_frame_coordinates_are_inverses() {
    let mut st = AppState::new();
    st.resolution = Some((4000, 3000));
    st.aspect = Aspect::Native;

    // 全幅：画面中心 ↔ 整幅中心。
    //
    // Whole frame: canvas centre maps to frame centre.
    let (fu, fv) = st.canvas_to_frame(0.5, 0.5);
    assert!((fu - 0.5).abs() < 1e-9 && (fv - 0.5).abs() < 1e-9);

    // 4:3 裁剪（横幅 4000×3000 本身就是 4:3，用 1:1 才能真的裁出东西）。
    //
    // A 1:1 crop actually cuts something out of a 4:3 frame.
    st.aspect = Aspect::Square;
    let crop = st.preview_crop().expect("方形比例应当产生一块取景");
    assert!(crop.w < 4000, "方形比例没有裁剪：{crop:?}");
    let (fu, fv) = st.canvas_to_frame(0.5, 0.5);
    let (bu, bv) = st.frame_to_canvas(fu, fv);
    assert!(
        (bu - 0.5).abs() < 1e-9 && (bv - 0.5).abs() < 1e-9,
        "往返不一致"
    );

    // 取景左上角在画布上是 (0,0)，映射到整幅就是取景框的角。
    //
    // The crop's top-left corner is (0,0) on the canvas and maps to the crop's own corner.
    let (u0, v0) = st.canvas_to_frame(0.0, 0.0);
    assert!((u0 - f64::from(crop.x) / 4000.0).abs() < 1e-9);
    assert!((v0 - f64::from(crop.y) / 3000.0).abs() < 1e-9);
}

/// 裁剪态、拉直态下不允许落点：那时画布显示的不是整幅，落点会错位。
///
/// Taps are refused in crop-edit and straightened states: the canvas is not showing the whole
/// frame, so a tap would land in the wrong place.
#[test]
fn taps_are_refused_when_the_canvas_is_not_the_whole_frame() {
    let dir = std::env::temp_dir().join(format!("aevum-heal-place-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    image::RgbImage::new(32, 24)
        .save(dir.join("f001.png"))
        .expect("写测试图");

    let mut st = AppState::new();
    st.resolution = Some((4000, 3000));
    assert!(
        !st.heal_can_place(),
        "没有打开任何帧时不该允许落点——修复要落到某一帧的参数上"
    );

    st.seq = Some(aevum_core::sequence::Sequence::open(&dir).expect("开序列"));
    st.tool = Tool::Heal;
    assert!(st.heal_can_place(), "修复工具 + 整幅应当允许落点");

    st.tool = Tool::Crop;
    assert!(!st.heal_can_place(), "裁剪态不该允许落点");
    st.tool = Tool::Heal;
    st.rotate = 5.0;
    assert!(!st.heal_can_place(), "拉直态不该允许落点");
    st.rotate = 0.0;
    assert!(st.heal_can_place(), "复位之后应当恢复");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 没有打开照片时自动直立报一句人话，不 panic、也不写参数。
///
/// With no photo open, auto-upright returns a readable message instead of panicking or writing
/// parameters.
#[test]
fn auto_upright_without_a_photo_reports_instead_of_panicking() {
    let mut st = AppState::new();
    let err = st.upright_auto().expect_err("没开照片应当报错");
    assert!(err.contains("照片"), "报错要指向原因：{err}");
    assert!(st.params.upright.is_none(), "失败时不许写参数");
}

/// 卸下校正是一次真正的状态变更；本来就没有校正时返回 false 且不标脏。
///
/// Dropping the correction is a real state change; with no correction present it returns
/// `false` and does not mark the state dirty.
#[test]
fn clearing_an_absent_correction_is_a_no_op() {
    let mut st = AppState::new();
    st.preview_dirty = false;
    assert!(!st.upright_clear());
    assert!(!st.preview_dirty);
    st.params.upright = Some(aevum_core::perspective::Quad {
        p: [(0.05, 0.0), (0.95, 0.0), (1.0, 1.0), (0.0, 1.0)],
    });
    assert!(st.upright_clear());
    assert!(st.params.upright.is_none());
    assert!(st.preview_dirty);
}

// ─────────────────────────────── 四角拖拽 ───────────────────────────────
// Corner dragging

/// 开一个只有一帧的最小序列，供四角拖拽的前置条件（有帧、有尺寸）用。
///
/// A minimal one-frame sequence, so the corner-drag preconditions (a frame, a size) hold.
fn one_frame_state(tag: &str) -> (AppState, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("aevum-upright-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    image::RgbImage::new(32, 24)
        .save(dir.join("f001.png"))
        .expect("写测试图");
    let mut st = AppState::new();
    st.resolution = Some((4000, 3000));
    st.seq = Some(aevum_core::sequence::Sequence::open(&dir).expect("开序列"));
    (st, dir)
}

/// 进四角模式时种一个单位方，退出时把它收走：工程里不该留一个什么都不做的参数。
///
/// Entering corner mode seeds the unit square and leaving drops it again: a parameter that
/// does nothing has no business staying in the project file.
#[test]
fn corner_mode_seeds_a_unit_square_and_drops_it_on_exit() {
    let (mut st, dir) = one_frame_state("seed");
    assert!(!st.upright_editing);
    assert!(st.upright_edit_toggle(), "整幅 + 有帧，应当进得去");
    assert!(st.upright_editing);
    let q = st.params.upright.expect("进入时应种下四边形");
    assert!(q.is_identity(), "种下的必须是单位方：预览不该跳变");

    assert!(!st.upright_edit_toggle(), "再点一次就退出");
    assert!(!st.upright_editing);
    assert!(st.params.upright.is_none(), "没动过的单位方要收走");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 拖一个角只动那一个；位置没变时不标脏（拖动每帧都会调到这里）。
///
/// Dragging one corner moves only that one, and an unchanged position marks nothing dirty
/// (this runs on every frame of a drag).
#[test]
fn dragging_a_corner_moves_only_that_corner() {
    let (mut st, dir) = one_frame_state("drag");
    assert!(st.upright_edit_toggle());
    let before = st.params.upright.expect("四边形").p;

    st.preview_dirty = false;
    assert!(st.upright_drag_corner(1, 0.7, 0.05), "应当改动第二个角");
    let after = st.params.upright.expect("四边形").p;
    assert!((after[1].0 - 0.7).abs() < 1e-6 && (after[1].1 - 0.05).abs() < 1e-6);
    for i in [0, 2, 3] {
        assert_eq!(after[i], before[i], "第 {i} 个角不该跟着动");
    }
    assert!(st.preview_dirty, "改动之后要标脏");

    st.preview_dirty = false;
    assert!(
        !st.upright_drag_corner(1, 0.7, 0.05),
        "同一个位置再报一次不算改动"
    );
    assert!(!st.preview_dirty, "没动就不该重画");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 角点夹在画幅里，非有限值直接忽略：拖到画面外不该把四边形送到 [0,1] 之外。
///
/// Corners clamp into the frame and non-finite values are ignored: dragging outside must not
/// push the quad beyond [0,1].
#[test]
fn corners_clamp_into_the_frame() {
    let (mut st, dir) = one_frame_state("clamp");
    assert!(st.upright_edit_toggle());
    assert!(st.upright_drag_corner(0, -0.5, 1.7));
    let q = st.params.upright.expect("四边形");
    assert_eq!(q.p[0], (0.0, 1.0), "越界的角要夹回来：{:?}", q.p[0]);

    let before = st.params.upright.expect("四边形").p;
    assert!(!st.upright_drag_corner(0, f64::NAN, 0.5), "NaN 要忽略");
    assert!(!st.upright_drag_corner(9, 0.5, 0.5), "越界的角号要忽略");
    assert_eq!(st.params.upright.expect("四边形").p, before);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 松手才记历史，单位方不记：一次拖拽只该留下一条撤销记录。
///
/// History is recorded on release and not for the unit square: one drag leaves exactly one
/// undo entry.
#[test]
fn the_quad_commits_once_on_release() {
    let (mut st, dir) = one_frame_state("commit");
    assert!(st.upright_edit_toggle());
    assert!(!st.upright_commit(), "单位方不该记一条历史");
    assert!(st.upright_drag_corner(2, 0.95, 0.95));
    assert!(st.upright_commit(), "有动作之后松手要记一条");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 双击一个角把它放回单位方的位置，别的角不动。
///
/// Double-clicking a corner puts it back where the unit square has it, leaving the others
/// alone.
#[test]
fn double_click_puts_one_corner_back() {
    let (mut st, dir) = one_frame_state("reset");
    assert!(st.upright_edit_toggle());
    assert!(st.upright_drag_corner(0, 0.2, 0.1));
    assert!(st.upright_drag_corner(3, 0.1, 0.9));
    assert!(st.upright_reset_corner(0), "第一个角应当被放回去");
    let q = st.params.upright.expect("四边形");
    assert_eq!(q.p[0], (0.0, 0.0));
    assert!((q.p[3].0 - 0.1).abs() < 1e-6, "别的角不该被动");
    assert!(!st.upright_reset_corner(0), "已经在原位就返回 false");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 裁剪态、拉直态、没有帧时进不去：那时四个角里有几个根本不在屏幕上。
///
/// Corner mode is refused while cropping, straightened, or with no frame: some of the four
/// corners are simply not on the canvas then.
#[test]
fn corner_mode_is_refused_when_the_whole_frame_is_not_visible() {
    let mut bare = AppState::new();
    bare.resolution = Some((4000, 3000));
    assert!(!bare.upright_edit_toggle(), "没有帧不该进得去");
    assert!(!bare.upright_editing);

    let (mut st, dir) = one_frame_state("refuse");
    st.tool = Tool::Crop;
    assert!(!st.upright_edit_toggle(), "裁剪态不该进得去");
    st.tool = Tool::Hand;
    st.rotate = 4.0;
    assert!(!st.upright_edit_toggle(), "拉直态不该进得去");
    st.rotate = 0.0;
    assert!(st.upright_edit_toggle(), "复位之后应当恢复");

    // 画幅裁剪同理：16:9 的取景把上下两条边切到了画面之外。
    //
    // Same for an aspect crop: a 16:9 framing puts the top and bottom edges outside.
    st.upright_edit_toggle();
    st.aspect = Aspect::Wide;
    assert!(!st.upright_can_drag(), "画幅裁剪之后角就不在屏幕上了");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 拖四角时预览要显示未校正的画面（否则等于对着已经拉正的图猜原来的角），
/// 但 `params` 与导出用的那份一个字都不许动。
///
/// While dragging corners the preview must show the uncorrected frame (otherwise you are
/// guessing old corner positions on an already-corrected picture), yet `params` -- what export
/// uses -- must not change at all.
#[test]
fn the_preview_shows_the_uncorrected_frame_while_dragging() {
    let (mut st, dir) = one_frame_state("preview");
    st.params.upright = Some(aevum_core::perspective::Quad {
        p: [(0.05, 0.02), (0.95, 0.0), (1.0, 1.0), (0.0, 0.98)],
    });
    assert_eq!(st.preview_params().upright, st.params.upright);

    assert!(st.upright_edit_toggle());
    assert!(
        st.preview_params().upright.is_none(),
        "编辑中预览必须是不带校正的"
    );
    assert!(st.params.upright.is_some(), "真参数不许动——那是导出的依据");

    st.upright_edit_toggle();
    assert_eq!(st.preview_params().upright, st.params.upright, "退出后复原");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 选修复画笔时自动退出四角模式：两者抢的是同一只手。
///
/// Picking the repair brush leaves corner mode, since the two compete for the same hand.
#[test]
fn choosing_the_heal_brush_leaves_corner_mode() {
    let (mut st, dir) = one_frame_state("tool");
    assert!(st.upright_edit_toggle());
    st.set_tool(Tool::Heal);
    assert!(!st.upright_editing, "选了画笔就该退出四角");
    assert!(st.params.upright.is_none(), "没动过的四边形要收走");

    let _ = std::fs::remove_dir_all(&dir);
}
