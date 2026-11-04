use super::*;

pub(super) fn two_frame_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("aevum-hist-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    for i in 0..2u8 {
        image::RgbImage::from_pixel(8, 6, image::Rgb([i * 20, 40, 60]))
            .save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    dir
}

/// 模拟界面帧：开一帧 → 直接改状态（相当于推子动了）→ 帧末落账。
fn edit_in_frame(state: &mut AppState, key: &str, label: &str, f: impl FnOnce(&mut AppState)) {
    state.history.begin_frame();
    f(state);
    history::note(key, label);
    state.history_commit();
}

#[test]
fn command_writes_undo_and_redo() {
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5}))
        .expect("改曝光");
    state
        .run_ui_command(&ctx, "grade.set", &json!({"contrast": 12}))
        .expect("改对比");
    assert_eq!(state.history.steps().len(), 3, "第 0 步是初始状态");

    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤对比");
    assert_eq!(state.params.contrast, 0.0);
    assert_eq!(state.params.exposure, 0.5, "只退一步，别的改动不动");

    state
        .run_ui_command(&ctx, "history.redo", &json!({}))
        .expect("重做对比");
    assert_eq!(state.params.contrast, 12.0);

    let err = state
        .run_ui_command(&ctx, "history.redo", &json!({}))
        .unwrap_err();
    assert!(err.contains("重做"), "{err}");
    // 撤到底：第 0 步是起点，前面没有可回的状态。
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("再撤");
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤到起点");
    let err = state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .unwrap_err();
    assert!(err.contains("撤销"), "{err}");
    assert_eq!(state.params.exposure, 0.0);
}

#[test]
fn drag_merges_contiguous_frames_into_one_step() {
    let ctx = Context::default();
    let mut state = AppState::new();
    for k in 1..=5i32 {
        let label = crate::i18n::tr_fmt("曝光 {}", &[&k]);
        edit_in_frame(&mut state, "曝光", &label, |s| {
            s.params.exposure = k as f64
        });
    }
    assert_eq!(state.history.steps().len(), 2, "一根推子拖 5 帧只算一步");
    assert_eq!(state.history.steps()[1].label, "曝光 5", "留最后一次的文案");
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤销");
    assert_eq!(state.params.exposure, 0.0, "撤销回到拖动之前");

    // 换控件就开新步。
    edit_in_frame(&mut state, "对比度", "对比度 3", |s| {
        s.params.contrast = 3.0
    });
    assert_eq!(state.history.steps().len(), 2);
}

/// v0.3.0 实测回归：在裁剪态点一下画布（没拖），历史不该多出一条
/// 「裁剪 768 × 368」。按下只摆姿态，真的拖出位移才落 `user_crop`、
/// 才登记历史；运镜同理，点一下也不许把开关偷偷打开。
#[test]
fn crop_click_without_drag_leaves_no_history() {
    let mut state = AppState::new();
    state.resolution = Some((60, 48));
    let base = state.history.steps().len();
    state.history.begin_frame();

    state.crop_begin([20.0, 20.0], crop::Hit::Inside);
    state.crop_end();
    assert_eq!(state.user_crop, None, "纯点击不固化取景框");
    state.history_commit();
    assert_eq!(state.history.steps().len(), base, "点一下画布不产生历史步");

    // 运镜同一条规矩：按下又松开，开关不动、历史不动。
    let enabled_before = state.motion.enabled;
    state.motion_begin([10.0, 10.0], crop::Hit::Inside);
    state.motion_end();
    assert_eq!(state.motion.enabled, enabled_before);
}

/// 一次拖动里鼠标停了几帧（帧距不再严格相邻）不能把一步拆成十几步；
/// 停得足够久（超过容差）才允许开新步。
#[test]
fn drag_merge_tolerates_a_few_idle_frames() {
    let mut state = AppState::new();
    state.resolution = Some((60, 48));
    let base = state.history.steps().len();

    // 第 1 帧开始拖，第 2 帧继续，第 3、4 帧停手（没有改动），第 5 帧接着拖。
    state.history.begin_frame();
    state.crop_begin([0.0, 0.0], crop::Hit::Outside);
    state.crop_drag_to([20.0, 20.0]);
    state.history_commit();
    let after_first = state.history.steps().len();
    assert_eq!(after_first, base + 1, "第一段拖动 = 一步");

    for _ in 0..3 {
        state.history.begin_frame();
        state.history_commit();
    }
    state.history.begin_frame();
    state.crop_drag_to([40.0, 30.0]);
    state.history_commit();
    assert_eq!(
        state.history.steps().len(),
        after_first,
        "停三帧仍在容差内，并成同一步"
    );
    assert_eq!(
        state.history.steps().last().map(|s| s.label.as_str()),
        Some("裁剪 40 × 30"),
        "合并步的文案跟最后状态走"
    );

    // 超过容差（31 帧）再拖：这才是一次新的编辑，开新步。
    for _ in 0..(crate::history::MERGE_FRAME_TOLERANCE as usize) {
        state.history.begin_frame();
        state.history_commit();
    }
    state.history.begin_frame();
    state.crop_drag_to([10.0, 10.0]);
    state.history_commit();
    assert_eq!(
        state.history.steps().len(),
        after_first + 1,
        "隔了半秒以上，算新的一步"
    );
}

#[test]
fn unnotured_widgets_still_enter_history() {
    // 面板上直接赋值、忘了登记文案的控件（选项栏那些 chip）靠帧末比对兜住。
    let ctx = Context::default();
    let mut state = AppState::new();
    state.history.begin_frame();
    state.grid = true;
    state.history_commit();
    assert_eq!(state.history.steps().len(), 2);
    assert_eq!(
        state.history.steps()[1].label,
        "网格",
        "比对出的字段名当文案"
    );

    // 没有改动的帧不产生步。
    state.history.begin_frame();
    state.history_commit();
    assert_eq!(state.history.steps().len(), 2);

    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤销");
    assert!(!state.grid);
}

#[test]
fn command_and_slider_do_not_double_record() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = two_frame_dir("dedup");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    // 命令内部会走 masks_changed → 登记一次；命令自己也记一步，只能留一行。
    state
        .run_ui_command(&ctx, "mask.add", &json!({"kind": "linear"}))
        .expect("加蒙版");
    state.history.begin_frame();
    state.history_commit();
    assert_eq!(
        state.history.steps().len(),
        2,
        "起点 + 加蒙版，命令不算两步"
    );
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤销");
    assert!(state.params.masks.is_empty(), "撤销把蒙版也带回去");
    assert_eq!(state.mask_sel, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ratings_and_keyframes_are_undoable() {
    let ctx = Context::default();
    let mut state = AppState::new();
    edit_in_frame(&mut state, "rating", "帧 0001 评 3 星", |s| {
        s.set_rating(0, 3)
    });
    assert_eq!(state.ratings.get(&0), Some(&3));
    edit_in_frame(&mut state, "keyframe-add", "记录关键帧", |s| {
        s.add_keyframe()
    });
    assert_eq!(state.keyframes.len(), 1);
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤关键帧");
    assert!(state.keyframes.is_empty());
    assert_eq!(
        state.ratings.get(&0),
        Some(&3),
        "星级是另一步，不该被顺手撤掉"
    );
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤星级");
    assert!(state.ratings.is_empty());
}

#[test]
fn external_steps_warn_and_keep_products() {
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5}))
        .expect("改曝光");
    state
        .run_ui_command(&ctx, "queue.clear", &json!({}))
        .expect("清队列");
    assert_eq!(state.history.steps().len(), 3);
    let out = state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤到外部步");
    assert_eq!(out["at"], 2);
    assert!(
        out["warning"].as_str().unwrap_or("").contains("产物"),
        "外部效果要提醒：{out}"
    );
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤曝光");
    assert_eq!(state.params.exposure, 0.0);
}

#[test]
fn label_table_splits_revertible_from_external() {
    let set = AppState::history_label("grade.set", &json!({"exposure": 0.5})).expect("改参数可撤");
    assert!(set.2);
    assert_eq!(set.1, "改 曝光", "历史行说中文");
    let many = AppState::history_label(
        "grade.set",
        &json!({"exposure": 0.5, "contrast": 12, "sharp_amount": 40, "nr_color": 30}),
    )
    .expect("一次改多项");
    // serde_json 的 map 按字典序走，所以这一行的字段顺序是确定的。
    assert_eq!(many.1, "改 对比度、曝光、降噪…", "同组参数并成一个词");
    let mask = AppState::history_label("mask.add", &json!({"kind": "radial"})).expect("加蒙版可撤");
    assert_eq!(mask.1, "加径向蒙版");
    assert_eq!(
        AppState::history_label("mask.add", &json!({}))
            .expect("缺省 kind")
            .1,
        "加线性蒙版"
    );
    for id in ["align.run", "export.video", "project.save", "queue.run"] {
        let step = AppState::history_label(id, &json!({})).unwrap_or_else(|| panic!("{id} 要记步"));
        assert!(!step.2, "{id} 是外部效果");
    }
    assert!(
        AppState::history_label("grade.get", &json!({})).is_none(),
        "只读命令不记步"
    );
}

#[test]
fn jump_and_history_json_agree_with_the_panel() {
    let ctx = Context::default();
    let mut state = AppState::new();
    for k in 1..=3i32 {
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": k as f64}))
            .expect("改曝光");
    }
    let list = state
        .run_ui_command(&ctx, "history.list", &json!({}))
        .expect("列历史");
    assert_eq!(list["count"], 4);
    assert_eq!(list["current"], 4);
    assert_eq!(list["steps"][0]["step"], "空状态");

    // 同样的值再下一次命令：什么都没变，不占一步。
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 3.0}))
        .expect("重复设置");
    assert_eq!(
        state
            .run_ui_command(&ctx, "history.list", &json!({}))
            .expect("列历史")["count"],
        4
    );

    state
        .run_ui_command(&ctx, "history.jump", &json!({"index": 2}))
        .expect("跳到第 2 步");
    assert_eq!(state.params.exposure, 1.0, "第 2 步是第一次改动之后");
    let err = state
        .run_ui_command(&ctx, "history.jump", &json!({"index": 9}))
        .unwrap_err();
    assert!(err.contains("超出范围"), "{err}");
    state
        .run_ui_command(&ctx, "history.redo", &json!({}))
        .expect("重做");
    assert_eq!(state.params.exposure, 2.0);
}

#[test]
fn history_is_bounded() {
    let ctx = Context::default();
    let mut state = AppState::new();
    for k in 0..(history::HISTORY_LIMIT as i32 + 40) {
        state
            .run_ui_command(&ctx, "grade.set", &json!({"exposure": k as f64}))
            .expect("改曝光");
    }
    assert_eq!(state.history.steps().len(), history::HISTORY_LIMIT);
    assert_eq!(state.history.cursor(), history::HISTORY_LIMIT - 1);
    assert!(state.history.can_undo());
}

#[test]
fn open_resets_the_timeline() {
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5}))
        .expect("改曝光");
    let dir = two_frame_dir("reset");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    assert_eq!(state.history.steps().len(), 1, "换序列属于新的一条时间线");
    assert_eq!(state.history.steps()[0].label, "打开序列");
    assert!(
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .is_err()
    );
    state
        .run_ui_command(&ctx, "grade.reset", &json!({}))
        .expect("复位");
    assert_eq!(
        state.history.steps().len(),
        1,
        "打开序列已经把参数清成默认，复位是空操作"
    );
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.4}))
        .expect("改曝光");
    assert_eq!(
        state.history.steps().len(),
        2,
        "新序列的第一次改动接在起点后面"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
