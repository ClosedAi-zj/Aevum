use super::*;

#[test]
fn a_stale_control_event_is_refused_rather_than_executed() {
    // 回归：executor 的 recv_timeout 超时后，事件曾留在 control_rx 里
    // 等下一帧照常执行 —— agent 超时重试会让批量写命令跑两遍。
    let ctx = Context::default();
    let mut state = AppState::new();
    // 一条"投递了很久才轮到"的命令：客户端那边早已超时。
    let (reply, reply_rx) = channel();
    state
        .control_tx
        .send(ControlEvent {
            id: "grade.set".to_owned(),
            args: json!({"exposure": 9.9}),
            queued_at: Instant::now() - control::UI_TIMEOUT - Duration::from_secs(1),
            reply,
        })
        .expect("发一条迟到的命令");
    state.drain_control_events(&ctx);
    let err = reply_rx
        .recv()
        .expect("迟到的命令也必须有回复，语义才完整")
        .expect_err("迟到的命令应当被拒绝而不是执行");
    assert!(err.contains("放弃执行"), "{err}");
    // 拒绝 = 不执行，不是"执行了再告诉客户端晚了"：参数必须原封不动。
    assert_eq!(state.params.exposure, 0.0, "迟到的命令不该改参数");

    // 时效内的命令照常执行，没被误伤。
    let (reply, reply_rx) = channel();
    state
        .control_tx
        .send(ControlEvent {
            id: "grade.set".to_owned(),
            args: json!({"exposure": 0.5}),
            queued_at: Instant::now(),
            reply,
        })
        .expect("发一条新鲜的命令");
    state.drain_control_events(&ctx);
    let out = reply_rx
        .recv()
        .expect("新鲜的命令应当有回复")
        .expect("应当执行");
    assert_eq!(out["params"]["exposure"], json!(0.5));
    assert_eq!(state.params.exposure, 0.5);
}

#[test]
fn queue_add_snapshots_the_project_at_enqueue_time() {
    // 回归：渲染曾一律读磁盘上的 project.json —— 自动保存可能已把
    // 排队之后的调色写进去，"每段读取加入时的工程参数"就落空了。
    // 现在加入那一刻把工程快照进队列项。
    let dir = temp_dir("queuesnap");
    let img = image::RgbImage::from_pixel(64, 48, image::Rgb([34, 34, 34]));
    img.save(dir.join("dark.png")).expect("写帧");
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.7}))
        .expect("调色");
    state.add_current_to_queue().expect("加入队列");
    let snap = state.queue[0]
        .project
        .as_ref()
        .expect("加入队列应当带上工程快照");
    assert_eq!(snap.params.exposure, 0.7, "快照必须是加入那一刻的参数");

    // 排队后再改调色：快照原地不动，渲染读到的还是加入时的参数。
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": -2.0}))
        .expect("排队后继续调色");
    let snap = state.queue[0].project.as_ref().unwrap();
    assert_eq!(snap.params.exposure, 0.7, "快照不该被排队后的调色带跑");
    let _ = std::fs::remove_dir_all(&dir);
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("aevum-ui-cmd-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

#[test]
fn profile_set_validates_kind_and_clears() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let err = state
        .run_ui_command(&ctx, "profile.set", &json!({}))
        .unwrap_err();
    assert!(err.contains("kind"), "{err}");
    let err = state
        .run_ui_command(&ctx, "profile.set", &json!({"kind": "xmp"}))
        .unwrap_err();
    assert!(err.contains("未知"), "{err}");
    let err = state
        .run_ui_command(&ctx, "profile.set", &json!({"kind": "dcp", "path": "  "}))
        .unwrap_err();
    assert!(err.contains("path"), "{err}");
    let out = state
        .run_ui_command(&ctx, "profile.set", &json!({"kind": "dcp", "clear": true}))
        .expect("清相机配置");
    assert_eq!(out["dcp"]["path"], json!(""));
    assert_eq!(out["dcp"]["name"], json!(""));
    let out = state
        .run_ui_command(&ctx, "profile.set", &json!({"kind": "lcp", "clear": true}))
        .expect("清镜头配置");
    assert!(out["lcp"]["correction"].is_null());
    // 没有序列/EXIF 时自动匹配只留说明，不动参数。
    state.match_lcp_profile();
    assert!(state.lcp_note.contains("EXIF"), "{}", state.lcp_note);
    state.match_dcp_profile();
    assert!(state.dcp_note.contains("EXIF"), "{}", state.dcp_note);
}

#[test]
fn grade_set_merges_and_rejects_unknown_fields() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let value = state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5, "contrast": 12}))
        .expect("合并参数");
    assert_eq!(value["params"]["exposure"], json!(0.5));
    assert_eq!(value["params"]["contrast"], json!(12.0));
    assert_eq!(value["preset"], json!("自定义"));

    let err = state
        .run_ui_command(&ctx, "grade.set", &json!({"params": {"bogus": 1}}))
        .unwrap_err();
    assert!(err.contains("bogus"), "{err}");
}

#[test]
fn grade_auto_command_writes_absolute_values_and_repeats_identically() {
    let dir = temp_dir("auto");
    let img = image::RgbImage::from_pixel(64, 48, image::Rgb([34, 34, 34]));
    img.save(dir.join("dark.png")).expect("写暗帧");
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    state.params.exposure = 2.5;
    let first = state
        .run_ui_command(&ctx, "grade.auto", &json!({}))
        .expect("自动校正");
    let exposure = first["params"]["exposure"].as_f64().expect("数值");
    assert!(exposure > 0.3, "暗图该提亮：{first}");
    assert!(first["note"].as_str().unwrap_or("").contains("自动"));
    let second = state
        .run_ui_command(&ctx, "grade.auto", &json!({}))
        .expect("再自动一次");
    assert_eq!(
        first["params"], second["params"],
        "同一帧重复自动校正必须给出同一组绝对值"
    );
}

#[test]
fn mask_commands_add_update_and_remove() {
    let dir = temp_dir("mask");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");

    let added = state
        .run_ui_command(&ctx, "mask.add", &json!({"kind": "linear"}))
        .expect("加蒙版");
    assert_eq!(added["count"], 1);
    assert_eq!(added["added"], 1);
    assert_eq!(added["selected"], 1);
    assert_eq!(state.preset, "自定义");

    let updated = state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({
                "index": 1,
                "amount": 80,
                "invert": true,
                "params": {"exposure": 1.5, "contrast": null},
                "geometry": {
                    "x1": 0.2, "y1": 0.3, "x2": 0.8, "y2": 0.7,
                    "angle": 400.0, "feather": 20
                }
            }),
        )
        .expect("改蒙版");
    assert_eq!(updated["updated"], 1);
    let parsed = mask::parse_mask(&state.params.masks[0]).expect("改完仍可解析");
    assert_eq!(parsed.amount, 80.0);
    assert!(parsed.invert);
    assert_eq!(parsed.params.get("exposure"), Some(&1.5));
    assert!(!parsed.params.contains_key("contrast"));
    let comp = &parsed.components[0];
    assert_eq!(comp.feather, 20.0);
    assert_eq!(comp.x1, 0.2);
    assert!((comp.angle - 40.0).abs() < 1e-9, "{}", comp.angle);

    let err = state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({"index": 1, "params": {"bogus": 1}}),
        )
        .unwrap_err();
    assert!(err.contains("bogus"), "{err}");
    let err = state
        .run_ui_command(&ctx, "mask.update", &json!({"index": 2}))
        .unwrap_err();
    assert!(err.contains("1..=1"), "{err}");

    let removed = state
        .run_ui_command(&ctx, "mask.remove", &json!({"index": 1}))
        .expect("删蒙版");
    assert_eq!(removed["count"], 0);
    assert!(state.params.masks.is_empty());
    assert_eq!(state.mask_sel, None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lens_profile_matches_from_exif_and_toggles() {
    let dir = temp_dir("lensdb");
    let db_path = dir.join("test-lensfun.xml");
    std::fs::write(
        &db_path,
        r#"<?xml version="1.0"?>
<lensdatabase version="1">
  <camera>
<maker>Nikon</maker>
<model>Z 8</model>
<cropfactor>1.0</cropfactor>
  </camera>
  <lens>
<maker>Nikon</maker>
<model>NIKKOR Z 24-70mm f/2.8 S</model>
<cropfactor>1.0</cropfactor>
<calibration>
  <distortion model="ptlens" focal="24" a="0.012" b="-0.031" c="0.018"/>
  <vignetting model="pa" focal="24" aperture="2.8" distance="10" k1="-0.39" k2="0.35" k3="-0.12"/>
</calibration>
  </lens>
</lensdatabase>
"#,
    )
    .expect("写测试库");
    unsafe { std::env::set_var("AEVUM_LENSFUN", db_path.as_os_str()) };

    let mut state = AppState::new();
    // EXIF 三件套命中：折出的校正进参数，面板说明带上型号。
    state.lens_match_apply(
        "NIKON CORPORATION",
        "NIKON Z 8",
        "NIKKOR Z 24-70mm f/2.8 S",
        47.0,
        Some(2.8),
    );
    assert!(state.params.lens_profile.is_some());
    assert!(state.lens_profile_note.contains("NIKKOR Z 24-70mm"));
    // 命令面查询与开关：关掉暂存、再开恢复。
    let out = state.cmd_lens_set(&json!({})).expect("查询");
    assert_eq!(out["profile"]["lens"], json!("NIKKOR Z 24-70mm f/2.8 S"));
    assert_eq!(out["profile"]["dist"]["model"], json!("ptlens"));
    let out = state.cmd_lens_set(&json!({"enabled": false})).expect("关");
    assert!(out["profile"].is_null());
    assert!(state.params.lens_profile.is_none());
    let out = state.cmd_lens_set(&json!({"enabled": true})).expect("开");
    assert!(out["profile"].is_object());
    // 没命中的镜头：参数不动，留一行说明。
    state
        .cmd_lens_set(&json!({"enabled": false}))
        .expect("再关");
    state.lens_match_apply("Canon", "EOS R5", "RF 100mm", 100.0, None);
    assert!(state.params.lens_profile.is_none());
    assert!(state.lens_profile_note.contains("没有"));
    // grade.set 合并别的字段不会把已匹配的配置抖掉。
    state.lens_match_apply(
        "NIKON CORPORATION",
        "NIKON Z 8",
        "NIKKOR Z 24-70mm f/2.8 S",
        47.0,
        Some(2.8),
    );
    let ctx = Context::default();
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.5}))
        .expect("改曝光");
    assert!(
        state.params.lens_profile.is_some(),
        "grade.set 保留镜头配置"
    );
    unsafe { std::env::remove_var("AEVUM_LENSFUN") };
    let _ = std::fs::remove_dir_all(&dir);
}

/// `AEVUM_LIBRARY` 是进程级环境变量：两个用它的测试并行跑时会把对方的图库清单
/// 指到别处（一边 set、一边 remove），读到空清单就索引越界。串起来跑。
static LIBRARY_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn library_commands_catalog_and_open() {
    let _env = LIBRARY_ENV
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = temp_dir("library");
    let store = dir.join("galleries.json");
    // 清单写到临时处，别把测试目录记进用户真的图库。
    unsafe { std::env::set_var("AEVUM_LIBRARY", store.as_os_str()) };
    let mut roots = dir.clone();
    roots.push("seq");
    std::fs::create_dir_all(&roots).unwrap();
    for i in 0..3u8 {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 20, 40, 60]));
        img.save(roots.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    std::fs::create_dir_all(aevum_core::project::guard_dir(&roots)).unwrap();
    let mut proj = aevum_core::project::ProjectFile::default();
    proj.ratings.insert(1, 4);
    proj.flags.insert(2, 1);
    aevum_core::project::save(&aevum_core::project::project_path(&roots), &proj).expect("写工程");

    let ctx = Context::default();
    let mut state = AppState::new();
    assert!(state.lib_roots.is_empty(), "临时清单从空开始");

    // 加目录：编目即扫描，汇总带上张数。
    let out = state
        .run_ui_command(
            &ctx,
            "library.add",
            &json!({"path": roots.display().to_string()}),
        )
        .expect("图库加目录");
    assert_eq!(out["index"], json!(1));
    assert_eq!(out["summary"], json!("3 张 · 1 星级 · 1 选用"));

    // 清单确实落到了 AEVUM_LIBRARY 指的地方。
    assert_eq!(state.lib_roots, vec![roots.clone()]);

    // 列清单 + 编目：逐文件评级/旗标跟着自然序帧号走。
    let out = state
        .run_ui_command(&ctx, "library.list", &json!({"index": 1}))
        .expect("列图库");
    assert_eq!(out["roots"][0]["scanned"], json!(true));
    assert_eq!(out["roots"][0]["files"].as_array().unwrap().len(), 3);
    assert_eq!(out["roots"][0]["files"][1]["rating"], json!(4));
    assert_eq!(out["roots"][0]["files"][2]["flag"], json!(1));
    assert_eq!(out["roots"][0]["files"][1]["frame"], json!(2));

    // 写回：library.organize 只改目标帧，其它帧原样；内存编目同步更新。
    let out = state
        .run_ui_command(
            &ctx,
            "library.organize",
            &json!({"frame": 2, "rating": 5, "label": 2}),
        )
        .expect("改评级与色标");
    assert_eq!(out["rating"], json!(5));
    assert_eq!(out["label"], json!(2));
    let reloaded =
        aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
    assert_eq!(reloaded.ratings.get(&1), Some(&5));
    assert_eq!(reloaded.flags.get(&2), Some(&1), "旗标不动");
    assert_eq!(reloaded.labels.get(&1), Some(&2));
    assert_eq!(state.lib_entries[0][1].rating, 5, "内存编目同步");

    // 多选：Ctrl 加选后键盘目标变成整批，整批一次写回。
    state.lib_click(0, false, false);
    state.lib_click(2, true, false);
    assert_eq!(state.lib_checked.len(), 2);
    let changed = state
        .lib_organize_targets(aevum_core::library::Organize {
            flag: Some(-1),
            ..Default::default()
        })
        .expect("批量改旗标");
    assert_eq!(changed, 2);
    let reloaded =
        aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
    assert_eq!(reloaded.flags.get(&0), Some(&-1));
    assert_eq!(reloaded.flags.get(&2), Some(&-1));
    assert_eq!(reloaded.ratings.get(&1), Some(&5), "别的帧不动");
    // Shift 连选从锚点铺到点击行。
    state.lib_click(0, false, false);
    state.lib_click(2, false, true);
    assert_eq!(state.lib_checked.len(), 3);

    // 命令面同一条口径：frames 数组一次改多张。
    let out = state
        .run_ui_command(
            &ctx,
            "library.organize",
            &json!({"frames": [1, 3], "rating": 3}),
        )
        .expect("批量命令");
    assert_eq!(out["count"], json!(2));
    assert_eq!(out["frames"], json!([1, 3]));
    for bad in [
        json!({"frame": 1, "frames": [1, 2], "rating": 3}),
        json!({"frames": [], "rating": 3}),
        json!({"frames": [0], "rating": 3}),
        json!({"frames": [4], "rating": 3}),
        json!({"frames": "1,2", "rating": 3}),
    ] {
        assert!(
            state
                .run_ui_command(&ctx, "library.organize", &bad)
                .is_err(),
            "{bad}"
        );
    }

    // 坏参不落盘。
    for bad in [
        json!({"frame": 0, "rating": 1}),
        json!({"frame": 4, "rating": 1}),
        json!({"frame": 1, "rating": 6}),
        json!({"frame": 1, "label": 5}),
        json!({"frame": 1, "flag": 2}),
        json!({"frame": 1}),
    ] {
        assert!(
            state
                .run_ui_command(&ctx, "library.organize", &bad)
                .is_err(),
            "{bad}"
        );
    }

    // 关键词：字符串按逗号拆、归一化去重；批量一次落盘；编目与词表同步。
    let out = state
        .run_ui_command(
            &ctx,
            "library.keyword",
            &json!({"frame": 1, "add": "日出， 前景 "}),
        )
        .expect("加词");
    assert_eq!(out["keywords"], json!(["前景", "日出"]), "字典序");
    assert_eq!(out["touched"], json!(1));
    let out = state
        .run_ui_command(
            &ctx,
            "library.keyword",
            &json!({"frames": [1, 2], "add": ["日出", "日出"], "remove": "云海"}),
        )
        .expect("批量改词");
    assert_eq!(out["touched"], json!(1), "没变的帧不算改动");
    let reloaded =
        aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
    assert_eq!(reloaded.keywords.get(&0).unwrap(), &["前景", "日出"]);
    assert_eq!(reloaded.keywords.get(&1).unwrap(), &["日出"]);
    let out = state
        .run_ui_command(&ctx, "library.list", &json!({"index": 1}))
        .expect("列编目");
    assert_eq!(
        out["roots"][0]["files"][0]["keywords"],
        json!(["前景", "日出"])
    );
    assert_eq!(out["roots"][0]["files"][1]["keywords"], json!(["日出"]));
    assert_eq!(
        out["roots"][0]["keywords"],
        json!([{"keyword": "前景", "count": 1}, {"keyword": "日出", "count": 2}])
    );
    // 按词筛选：只留下带这个词的行。
    state.lib_filter.keyword = "日出".to_owned();
    assert!(state.lib_filter.keeps(&state.lib_entries[0][0]));
    assert!(!state.lib_filter.keeps(&state.lib_entries[0][2]));
    state.lib_filter = aevum_core::library::Filter::default();
    for bad in [
        json!({"frame": 1}),
        json!({"frame": 1, "add": "   "}),
        json!({"frame": 1, "frames": [1], "add": "x"}),
        json!({"frame": 1, "add": 3}),
    ] {
        assert!(
            state.run_ui_command(&ctx, "library.keyword", &bad).is_err(),
            "{bad}"
        );
    }

    // 说明与版权：目录级，落进这个目录的工程文件；不带参数只查询、不抹值。
    let out = state
        .run_ui_command(
            &ctx,
            "library.metadata",
            &json!({"title": " 云海  日出 ", "copyright": "© 2026 张三"}),
        )
        .expect("写元数据");
    assert_eq!(out["title"], json!("云海 日出"), "空白折叠");
    assert_eq!(out["saved"], json!(true));
    assert_eq!(out["filled"], json!(true));
    assert_eq!(
        state.lib_meta.title, "云海 日出",
        "图库信息面板的缓冲跟着刷新"
    );
    let reloaded =
        aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
    assert_eq!(reloaded.metadata.title, "云海 日出");
    assert_eq!(reloaded.metadata.copyright, "© 2026 张三");
    assert_eq!(reloaded.metadata.description, "");
    let out = state
        .run_ui_command(&ctx, "library.metadata", &json!({}))
        .expect("查元数据");
    assert_eq!(out["saved"], json!(false), "只读不写盘");
    assert_eq!(out["copyright"], json!("© 2026 张三"));
    let out = state
        .run_ui_command(
            &ctx,
            "library.metadata",
            &json!({"title": "", "copyright": null}),
        )
        .expect("清元数据");
    assert_eq!(out["filled"], json!(false));
    let reloaded =
        aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
    assert_eq!(reloaded.metadata, aevum_core::project::Metadata::default());
    assert_eq!(
        reloaded.keywords.get(&0).unwrap(),
        &["前景", "日出"],
        "别动关键词"
    );
    for bad in [json!({"title": 3}), json!({"root": 9})] {
        assert!(
            state
                .run_ui_command(&ctx, "library.metadata", &bad)
                .is_err(),
            "{bad}"
        );
    }

    // 集合：新建、加入、去重、落盘、移出、删除。
    let out = state
        .run_ui_command(&ctx, "collection.create", &json!({"name": "精选"}))
        .expect("建集合");
    assert_eq!(out["index"], json!(1));
    let out = state
        .run_ui_command(&ctx, "collection.add", &json!({"name": "精选", "frame": 1}))
        .expect("加入集合");
    assert_eq!(out["added"], json!(true));
    let out = state
        .run_ui_command(&ctx, "collection.add", &json!({"index": 1, "frame": 1}))
        .expect("重复加入");
    assert_eq!(out["added"], json!(false), "同一张不重复");
    state
        .run_ui_command(&ctx, "collection.add", &json!({"index": 1, "frame": 3}))
        .expect("再加一张");
    let out = state
        .run_ui_command(&ctx, "collection.list", &json!({"index": 1}))
        .expect("列集合");
    assert_eq!(out["collections"][0]["count"], json!(2));
    assert_eq!(out["collections"][0]["files"].as_array().unwrap().len(), 2);
    let saved = aevum_core::library::load(&store);
    assert_eq!(saved.collections[0].files.len(), 2, "集合随清单落盘");
    // 一次收一批：重复路径只算一次，整批只落一次盘。
    state.lib_collection_create("备选");
    let batch = vec![
        state.lib_entries[0][0].path.clone(),
        state.lib_entries[0][2].path.clone(),
        state.lib_entries[0][0].path.clone(),
    ];
    assert_eq!(state.lib_collection_put_many(1, &batch), 2);
    assert_eq!(state.lib_cols[1].files.len(), 2);
    assert!(state.lib_collection_delete(1));
    let out = state
        .run_ui_command(&ctx, "collection.remove", &json!({"index": 1, "frame": 1}))
        .expect("移出集合");
    assert_eq!(out["removed"], json!(true));
    assert_eq!(out["count"], json!(1));
    // 集合视图的行能对上编目里的帧号与评级。
    let rows = state.lib_collection_rows(0);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].index, 2);
    let out = state
        .run_ui_command(&ctx, "collection.delete", &json!({"index": 1}))
        .expect("删集合");
    assert_eq!(out["deleted"], json!("精选"));
    assert!(state.lib_cols.is_empty());

    // 越界与坏参都要报错。
    let err = state
        .run_ui_command(&ctx, "library.remove", &json!({"index": 2}))
        .unwrap_err();
    assert!(err.contains("1..=1"), "{err}");
    let err = state
        .run_ui_command(&ctx, "library.add", &json!({"path": "不存在的目录"}))
        .unwrap_err();
    assert!(err.contains("不是目录"), "{err}");

    // 从图库打开序列：与 sequence.open 同一口径。
    let out = state
        .run_ui_command(&ctx, "library.open", &json!({"index": 1}))
        .expect("从图库打开");
    assert_eq!(out["opened"], json!(true));
    assert_eq!(out["frames"], json!(3));
    assert_eq!(state.frame_count(), 3);
    // 打开序列时关键词也读进来，序列这边保存工程不会把它们写丢。
    assert_eq!(state.keywords.get(&1), Some(&vec!["日出".to_owned()]));

    // 移除目录：清单落盘，编目跟着清。
    let out = state
        .run_ui_command(&ctx, "library.remove", &json!({"index": 1}))
        .expect("移除目录");
    assert_eq!(out["roots"], json!(0));
    assert_eq!(state.lib_sel, None);
    assert_eq!(state.lib_entries.len(), 0);

    unsafe { std::env::remove_var("AEVUM_LIBRARY") };
    let _ = std::fs::remove_dir_all(&dir);
}

/// 图库堆叠：并叠 / 拆叠 / 按文件名自动分叠都写回该目录的工程文件，评级与关键词
/// 一律不动；这个目录正好开着序列时内存那份也得跟上，否则自动保存会用旧值盖回去。
#[test]
fn library_stack_commands_fold_and_unfold() {
    let _env = LIBRARY_ENV
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = temp_dir("library_stack");
    let store = dir.join("galleries.json");
    unsafe { std::env::set_var("AEVUM_LIBRARY", store.as_os_str()) };
    let mut roots = dir.clone();
    roots.push("seq");
    std::fs::create_dir_all(&roots).unwrap();
    for name in [
        "A001_0001.png",
        "A001_0002.png",
        "A001_0003.png",
        "B002_0001.png",
        "B002_0002.png",
        "单张.png",
    ] {
        image::RgbImage::from_pixel(8, 6, image::Rgb([10, 20, 30]))
            .save(roots.join(name))
            .expect("写测试帧");
    }
    std::fs::create_dir_all(aevum_core::project::guard_dir(&roots)).unwrap();
    let mut proj = aevum_core::project::ProjectFile::default();
    proj.ratings.insert(0, 5);
    proj.keywords.insert(1, vec!["日出".to_owned()]);
    aevum_core::project::save(&aevum_core::project::project_path(&roots), &proj).expect("写工程");

    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "library.add",
            &json!({"path": roots.display().to_string()}),
        )
        .expect("图库加目录");

    // 没叠：list 是空表，编目行也没有叠号。
    let out = state
        .run_ui_command(&ctx, "library.stack", &json!({}))
        .expect("查堆叠");
    assert_eq!(out["op"], json!("list"));
    assert_eq!(out["count"], json!(0));
    assert!(state.lib_entries[0].iter().all(|f| f.stack.is_none()));

    // 并叠：帧号 1 开始，返回整份堆叠；编目行立刻标上叠号。
    let out = state
        .run_ui_command(
            &ctx,
            "library.stack",
            &json!({"op": "create", "frames": [1, 2]}),
        )
        .expect("并叠");
    assert_eq!(out["count"], json!(1));
    assert_eq!(
        out["stacks"][0],
        json!({"stack": 1, "head": 1, "count": 2, "frames": [1, 2]})
    );
    assert_eq!(state.lib_entries[0][0].stack, Some(0));
    assert_eq!(state.lib_entries[0][1].stack, Some(0));
    assert_eq!(state.lib_entries[0][2].stack, None, "没勾的不进叠");
    let reloaded =
        aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
    assert_eq!(reloaded.stacks, vec![vec![0, 1]]);
    assert_eq!(reloaded.ratings.get(&0), Some(&5), "评级不动");
    assert_eq!(reloaded.keywords.get(&1).unwrap(), &["日出"], "关键词不动");

    // 跨叠搬帧：老叠被摘剩一张就消失，新叠按头帧排回去。
    let out = state
        .run_ui_command(
            &ctx,
            "library.stack",
            &json!({"op": "create", "frames": [2, 4]}),
        )
        .expect("跨叠并叠");
    assert_eq!(
        out["stacks"],
        json!([{"stack": 1, "head": 2, "count": 2, "frames": [2, 4]}])
    );
    assert_eq!(state.lib_entries[0][0].stack, None, "被摘走的帧回到散张");
    assert_eq!(state.lib_entries[0][3].stack, Some(0));

    // 拆叠：叠号越界当没这叠，不报错也不改盘。
    let out = state
        .run_ui_command(
            &ctx,
            "library.stack",
            &json!({"op": "dissolve", "stack": 9}),
        )
        .expect("越界叠号");
    assert_eq!(out["count"], json!(1));
    let out = state
        .run_ui_command(
            &ctx,
            "library.stack",
            &json!({"op": "dissolve", "stack": 1}),
        )
        .expect("拆叠");
    assert_eq!(out["count"], json!(0));
    assert!(state.lib_entries[0].iter().all(|f| f.stack.is_none()));

    // 自动分叠：去扩展名与结尾数字串后同前缀的归一叠，单张不成叠。
    let out = state
        .run_ui_command(&ctx, "library.stack", &json!({"op": "auto"}))
        .expect("自动分叠");
    assert_eq!(out["count"], json!(2));
    assert_eq!(out["stacks"][0]["frames"], json!([1, 2, 3]));
    assert_eq!(out["stacks"][1]["frames"], json!([4, 5]));
    assert_eq!(
        state.lib_entries[0][5].stack, None,
        "「单张」没前缀，不成叠"
    );

    // 这个目录开着序列：工程读进来的堆叠与命令改完的堆叠是同一份。
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": roots.display().to_string()}),
        )
        .expect("打开序列");
    assert_eq!(state.stacks, vec![vec![0, 1, 2], vec![3, 4]]);
    let out = state
        .run_ui_command(
            &ctx,
            "library.stack",
            &json!({"op": "create", "frames": [1, 6]}),
        )
        .expect("开着序列时并叠");
    assert_eq!(
        out["stacks"],
        json!([
            {"stack": 1, "head": 1, "count": 2, "frames": [1, 6]},
            {"stack": 2, "head": 2, "count": 2, "frames": [2, 3]},
            {"stack": 3, "head": 4, "count": 2, "frames": [4, 5]},
        ]),
        "第 1 帧被摘走，老叠剩 2、3 两张；B002 那叠没碰"
    );
    assert_eq!(
        state.stacks,
        vec![vec![0, 5], vec![1, 2], vec![3, 4]],
        "内存那份跟上，自动保存才不会盖回去"
    );
    assert_eq!(
        state.project_snapshot().stacks,
        vec![vec![0, 5], vec![1, 2], vec![3, 4]]
    );

    // 界面上那条路：勾选优先，少于两张要拦住。
    state.lib_click(3, false, false);
    let err = state.lib_stack_targets().unwrap_err();
    assert!(err.contains("至少"), "{err}");
    state.lib_click(4, false, false);
    state.lib_click(5, true, false);
    assert_eq!(state.lib_stack_targets().expect("勾选并叠"), 2);
    assert_eq!(state.lib_entries[0][4].stack, state.lib_entries[0][5].stack);
    // 拆散目标行所在的那一叠：返回被放出来的张数。
    assert_eq!(state.lib_stack_dissolve_targets().expect("拆叠"), 2);
    let err = state.lib_stack_dissolve_targets().unwrap_err();
    assert!(err.contains("不在任何叠里"), "{err}");

    // 叠号一改，旧的展开集就对不上号：一律收回折叠态。
    state.lib_stack_open.insert((0, 0));
    state.lib_stack_auto(0).expect("再自动分叠");
    assert!(state.lib_stack_open.is_empty(), "展开集该清掉");

    for bad in [
        json!({"op": "nope"}),
        json!({"op": 3}),
        json!({"op": "create", "frames": [1]}),
        json!({"op": "create", "frame": 1}),
        json!({"op": "create", "frames": [1, 9]}),
        json!({"op": "dissolve"}),
        json!({"op": "dissolve", "stack": 0}),
        json!({"root": 9}),
    ] {
        assert!(
            state.run_ui_command(&ctx, "library.stack", &bad).is_err(),
            "{bad}"
        );
    }
    let reloaded =
        aevum_core::project::load(&aevum_core::project::project_path(&roots)).expect("读工程");
    assert_eq!(reloaded.ratings.get(&0), Some(&5), "坏参不落盘、也不动评级");

    unsafe { std::env::remove_var("AEVUM_LIBRARY") };
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mask_brush_command_replaces_strokes() {
    let dir = temp_dir("mask_brush");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");

    let added = state
        .run_ui_command(&ctx, "mask.add", &json!({"kind": "brush"}))
        .expect("加画笔蒙版");
    assert_eq!(added["masks"][0]["shape"], json!("brush"));

    // strokes 整组替换 + 局部参数，一条命令把蒙版画活：先涂后擦。
    let updated = state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({
                "index": 1,
                "params": {"exposure": 0.8},
                "geometry": {"strokes": [
                    {"points": [[0.5, 0.5], [0.7, 0.5]], "radius": 0.1,
                     "hardness": 0.8, "flow": 1.0},
                    {"points": [[0.5, 0.5]], "radius": 0.05, "erase": true}
                ]}
            }),
        )
        .expect("写笔画");
    assert_eq!(updated["masks"][0]["shapes"][0]["strokes"], json!(2));
    assert_eq!(updated["masks"][0]["shapes"][0]["kind"], json!("brush"));
    let parsed = mask::parse_mask(&state.params.masks[0]).expect("画笔蒙版解析得动");
    assert_eq!(parsed.components[0].shape, mask::ShapeKind::Brush);
    assert_eq!(parsed.components[0].strokes.len(), 2);
    assert!(parsed.components[0].strokes[1].erase);
    let mut m = parsed;
    m.build_field(40, 40, None, None);
    assert!(
        m.field[20 * 40 + 20] < 1e-6,
        "中心先涂后擦该是 0：{}",
        m.field[20 * 40 + 20]
    );
    assert!(m.field[20 * 40 + 27] > 0.9, "笔画末端还在");

    // 再发一次 strokes 是替换不是追加；缺省键按 core 默认补。
    state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({
                "index": 1,
                "geometry": {"strokes": [{"points": [[0.9, 0.9]]}]}
            }),
        )
        .expect("替换笔画");
    let parsed = mask::parse_mask(&state.params.masks[0]).unwrap();
    assert_eq!(parsed.components[0].strokes.len(), 1);
    assert_eq!(parsed.components[0].strokes[0].points.len(), 1);
    assert!(
        (parsed.components[0].strokes[0].radius - 0.06).abs() < 1e-9,
        "缺省半径 6%"
    );

    // 坏笔画要报错且不落盘：未知字段、点不是 [x, y]。
    let err = state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({
                "index": 1,
                "geometry": {"strokes": [{"points": [[0.5, 0.5]], "nope": 1}]}
            }),
        )
        .unwrap_err();
    assert!(err.contains("nope"), "{err}");
    let err = state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({"index": 1, "geometry": {"strokes": [{"points": [[0.5]]}]}}),
        )
        .unwrap_err();
    assert!(err.contains("[x, y]"), "{err}");
    let parsed = mask::parse_mask(&state.params.masks[0]).unwrap();
    assert_eq!(parsed.components[0].strokes.len(), 1, "失败的替换不落盘");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mask_component_boolean_and_luminance_range() {
    let dir = temp_dir("mask_comp");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    state
        .run_ui_command(&ctx, "mask.add", &json!({"kind": "radial"}))
        .expect("加蒙版");

    // 组里再加一个线性形状，并自动选中新形状。
    let added = state
        .run_ui_command(
            &ctx,
            "mask.component",
            &json!({"index": 1, "kind": "linear"}),
        )
        .expect("加形状");
    assert_eq!(added["add"], json!(2));
    assert_eq!(added["masks"][0]["components"], json!(2));
    assert_eq!(added["component"], json!(2));
    assert_eq!(state.mask_comp, 1);
    assert!(mask::parse_mask(&state.params.masks[0]).is_some());

    // 第二个形状用「减去」挖掉一块，几何只改它自己。
    let updated = state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({
                "index": 1,
                "component": 1,
                "geometry": {"blend": "subtract", "x1": 0.1, "feather": 30},
                "range": {"lum_on": true, "lum_min": 0.2, "lum_max": 0.9, "lum_feather": 5}
            }),
        )
        .expect("改形状与亮度范围");
    let parsed = mask::parse_mask(&state.params.masks[0]).expect("仍可解析");
    assert_eq!(parsed.components[0].blend, mask::Blend::Add);
    assert_eq!(parsed.components[1].blend, mask::Blend::Subtract);
    assert_eq!(parsed.components[1].x1, 0.1);
    assert_eq!(parsed.components[0].x1, 0.5, "别把第一个形状也改了");
    let range = parsed.range.as_ref().expect("亮度范围已存盘");
    assert!(range.on);
    assert_eq!(range.min, 0.2);
    assert_eq!(range.max, 0.9);
    assert_eq!(range.feather, 1.0, "超出 0~1 的羽化按上限收");
    assert_eq!(updated["masks"][0]["range"]["lum_on"], json!(true));
    assert_eq!(
        updated["component"],
        json!(2),
        "mask.update 把编辑目标停在 component 1"
    );

    // 反常识参数：越界 component、未知 range 字段、越界 blend 都要报错且不落盘。
    let err = state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({"index": 1, "component": 2, "amount": 50}),
        )
        .unwrap_err();
    assert!(err.contains("component 超出范围"), "{err}");
    let err = state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({"index": 1, "range": {"lum_min": 0.3, "bogus": 1}}),
        )
        .unwrap_err();
    assert!(err.contains("bogus"), "{err}");
    assert_eq!(
        mask::parse_mask(&state.params.masks[0])
            .expect("坏请求没写坏蒙版")
            .amount,
        100.0
    );

    // 颜色范围：hue_* 一组键落进同一个 range 对象，亮度键不丢。
    state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({"index": 1, "range": {"hue_on": true, "hue": 210,
                    "hue_width": 70, "hue_feather": 25, "hue_sat": 0.1}}),
        )
        .expect("写颜色范围");
    let parsed = mask::parse_mask(&state.params.masks[0]).expect("仍可解析");
    let hue = parsed.hue.as_ref().expect("颜色范围已存盘");
    assert!(hue.on);
    assert_eq!(hue.hue, 210.0);
    assert_eq!(hue.width, 70.0);
    assert_eq!(hue.feather, 25.0);
    assert_eq!(hue.sat_min, 0.1);
    let range = parsed.range.as_ref().expect("亮度范围还在");
    assert!(range.on, "颜色范围落盘不能抹掉亮度范围");
    assert_eq!(range.min, 0.2);
    // 只给部分 hue 键：缺的按已存/默认补齐，不开 on。
    state
        .run_ui_command(
            &ctx,
            "mask.update",
            &json!({"index": 1, "range": {"hue": 30}}),
        )
        .expect("只改目标色相");
    let parsed = mask::parse_mask(&state.params.masks[0]).expect("仍可解析");
    let hue = parsed.hue.as_ref().expect("颜色范围还在");
    assert_eq!(hue.hue, 30.0);
    assert!(hue.on, "已开启的颜色范围不许被单键更新关掉");
    assert_eq!(hue.width, 70.0);

    // 删形状：删到只剩一个之后不许再删，整张蒙版请用 mask.remove。
    let removed = state
        .run_ui_command(
            &ctx,
            "mask.component",
            &json!({"index": 1, "action": "remove", "component": 1}),
        )
        .expect("删形状");
    assert_eq!(removed["remove"], json!(2));
    assert_eq!(removed["masks"][0]["components"], json!(1));
    let err = state
        .run_ui_command(
            &ctx,
            "mask.component",
            &json!({"index": 1, "action": "remove", "component": 0}),
        )
        .unwrap_err();
    assert!(err.contains("只剩一个形状"), "{err}");
    assert_eq!(
        mask::parse_mask(&state.params.masks[0])
            .expect("仍是单组")
            .components
            .len(),
        1
    );

    // 非法 kind / action 走清晰报错，不给半截状态。
    assert!(
        state
            .run_ui_command(
                &ctx,
                "mask.component",
                &json!({"index": 1, "kind": "banana"})
            )
            .unwrap_err()
            .contains("banana")
    );
    assert!(
        state
            .run_ui_command(
                &ctx,
                "mask.component",
                &json!({"index": 1, "action": "invert"})
            )
            .unwrap_err()
            .contains("未知动作")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn queue_commands_add_dedupe_and_guard_run() {
    let dir = temp_dir("queue");
    let first = dir.join("first");
    let second = dir.join("second");
    for root in [&first, &second] {
        std::fs::create_dir_all(root).expect("建序列目录");
        for i in 0..2u8 {
            let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
            img.save(root.join(format!("f{i:03}.png")))
                .expect("写测试帧");
        }
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    // 别碰真实漫游配置里的 render_queue.json。
    state.queue_path = dir.join("render_queue.json");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": first.display().to_string()}),
        )
        .expect("打开序列");

    // 不带参数：当前序列 + 默认成片名。
    let added = state
        .run_ui_command(&ctx, "queue.add", &json!({}))
        .expect("加入当前序列");
    assert_eq!(added["added"], json!(1));
    assert_eq!(added["count"], json!(1));
    let item = &added["items"][0];
    assert_eq!(item["root"], json!(first.display().to_string()));
    assert!(
        item["out"].as_str().unwrap().ends_with("first_成片.mp4"),
        "{}",
        item["out"]
    );
    assert_eq!(item["status"], json!("待渲"));

    // 另一条序列：显式规格；再次加入同一目录是更新而不是新增。
    let out = dir.join("second.mp4");
    state
        .run_ui_command(
            &ctx,
            "queue.add",
            &json!({
                "root": second.display().to_string(),
                "out": out.display().to_string(),
                "width": 1920, "height": 1080, "fps": 30,
                "codec": "hevc", "crf": 20, "preset": "slow",
                "keep_frames": true,
            }),
        )
        .expect("加入第二段");
    let dedup = state
        .run_ui_command(
            &ctx,
            "queue.add",
            &json!({"root": first.display().to_string()}),
        )
        .expect("重复加入");
    assert_eq!(dedup["count"], json!(2));
    // 重复加入会挪到队尾（与 Python 版一致），所以这一段成了第 2 项。
    assert_eq!(dedup["added"], json!(2));
    assert_eq!(
        dedup["items"][1]["root"],
        json!(first.display().to_string())
    );
    let second_item = &dedup["items"][0];
    assert_eq!(second_item["codec"], json!("h265"));
    assert_eq!(second_item["width"], json!(1920));
    assert_eq!(second_item["keep_frames"], json!(true));

    for (args, needle) in [
        (json!({"width": 0}), "同时"),
        (json!({"width": 1921, "height": 1080}), "偶数"),
        (json!({"fps": 0}), "fps"),
        (json!({"crf": 52}), "crf"),
        (json!({"codec": "av1"}), "h264"),
        (json!({"preset": "turbo"}), "预设"),
        (
            json!({"root": dir.join("missing").display().to_string()}),
            "不存在",
        ),
    ] {
        let err = state.run_ui_command(&ctx, "queue.add", &args).unwrap_err();
        assert!(err.contains(needle), "{args}：{err}");
    }

    // 批量写操作必须确认；无界面模式不能自己跑队列。
    let err = state
        .run_ui_command(&ctx, "queue.run", &json!({}))
        .unwrap_err();
    assert!(err.contains("confirm"), "{err}");
    state.headless = true;
    let err = state
        .run_ui_command(&ctx, "queue.run", &json!({"confirm": true}))
        .unwrap_err();
    assert!(err.contains("无界面"), "{err}");
    state.headless = false;

    let err = state
        .run_ui_command(&ctx, "queue.remove", &json!({"index": 3}))
        .unwrap_err();
    assert!(err.contains("1..=2"), "{err}");
    let removed = state
        .run_ui_command(&ctx, "queue.remove", &json!({"index": 2}))
        .expect("移除排在队尾的第一段");
    assert_eq!(removed["items"], json!(1));
    let status = state
        .run_ui_command(&ctx, "queue.status", &json!({}))
        .expect("队列状态");
    assert_eq!(status["pending"], json!(1));
    assert_eq!(status["busy"], json!(false));
    assert!(status["progress"].is_null());

    // 队列变化立即落盘，换一次启动也能读回。
    let on_disk = queue::load(&state.queue_path);
    assert_eq!(on_disk.len(), 1);
    assert_eq!(on_disk[0].root, second.display().to_string());

    let cleared = state
        .run_ui_command(&ctx, "queue.clear", &json!({}))
        .expect("清空队列");
    assert_eq!(cleared["cleared"], json!(1));
    assert!(queue::load(&state.queue_path).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn xmp_import_and_export_roundtrip() {
    const PRESET: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
crs:Version="17.3.1"
crs:Exposure2012="1.25"
crs:Contrast2012="18"
crs:Vibrance="30">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    let dir = temp_dir("xmp");
    let preset = dir.join("Look.xmp");
    std::fs::write(&preset, PRESET).expect("写测试预设");
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .params
        .masks
        .push(mask::new_mask(mask::ShapeKind::Radial));
    state.mask_sel = Some(0);

    let imported = state
        .run_ui_command(
            &ctx,
            "xmp.import",
            &json!({"path": preset.display().to_string()}),
        )
        .expect("导入预设");
    assert_eq!(imported["imported"], json!("Look"));
    assert_eq!(state.preset, "Look");
    assert!((state.params.exposure - 1.25).abs() < 1e-12);
    assert!((state.params.contrast - 18.0).abs() < 1e-12);
    assert!((state.params.vibrance - 30.0).abs() < 1e-12);
    // 预设之外没有的键回到默认；蒙版不属于 XMP 模型，随默认清空。
    assert!(state.params.masks.is_empty());
    assert_eq!(state.mask_sel, None);

    let err = state
        .run_ui_command(
            &ctx,
            "xmp.export",
            &json!({"path": dir.join("bad.txt").display().to_string()}),
        )
        .unwrap_err();
    assert!(err.contains(".xmp"), "{err}");

    let out = dir.join("out.xmp");
    state
        .run_ui_command(
            &ctx,
            "xmp.export",
            &json!({"path": out.display().to_string()}),
        )
        .expect("导出预设");
    assert!(out.is_file());
    let back = xmp::read(&out).expect("读回预设");
    assert!((back.exposure - 1.25).abs() < 1e-9);
    assert!((back.contrast - 18.0).abs() < 1e-9);

    let err = state
        .run_ui_command(
            &ctx,
            "xmp.export",
            &json!({"path": out.display().to_string()}),
        )
        .unwrap_err();
    assert!(err.contains("confirm"), "{err}");
    state
        .run_ui_command(
            &ctx,
            "xmp.export",
            &json!({"path": out.display().to_string(), "confirm": true}),
        )
        .expect("确认后覆盖");

    let err = state
        .run_ui_command(
            &ctx,
            "xmp.import",
            &json!({"path": dir.join("nope.xmp").display().to_string()}),
        )
        .unwrap_err();
    assert!(err.contains("不存在"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cube_look_export_and_import_roundtrip() {
    let dir = temp_dir("cube");
    let ctx = Context::default();
    let mut state = AppState::new();

    state
        .run_ui_command(
            &ctx,
            "grade.set",
            &json!({"exposure": 0.6, "contrast": 20.0}),
        )
        .expect("设置参数");
    let out = dir.join("look.cube");
    let exported = state
        .run_ui_command(
            &ctx,
            "cube.export",
            &json!({"path": out.display().to_string(), "size": 17}),
        )
        .expect("导出 .cube");
    assert_eq!(exported["size"], json!(17));
    assert_eq!(exported["nodes"], json!(17 * 17 * 17));
    assert!(out.is_file(), "导出的 .cube 没落盘");

    let err = state
        .run_ui_command(
            &ctx,
            "cube.export",
            &json!({"path": dir.join("bad.txt").display().to_string()}),
        )
        .unwrap_err();
    assert!(err.contains(".cube"), "{err}");
    let err = state
        .run_ui_command(
            &ctx,
            "cube.export",
            &json!({"path": out.display().to_string(), "size": 17}),
        )
        .unwrap_err();
    assert!(err.contains("confirm"), "{err}");
    let err = state
        .run_ui_command(
            &ctx,
            "cube.export",
            &json!({"path": out.display().to_string(), "size": 512, "confirm": true}),
        )
        .unwrap_err();
    assert!(err.contains("256"), "{err}");

    // 挂上：默认 apply，参数与预设都要跟着改，否则界面表现为"改了没反应"。
    let imported = state
        .run_ui_command(
            &ctx,
            "cube.import",
            &json!({"path": out.display().to_string()}),
        )
        .expect("导入外观");
    assert_eq!(imported["applied"], json!(true));
    assert_eq!(imported["size"], json!(17));
    assert_eq!(imported["title"], json!("grade"));
    assert_eq!(
        imported["neutral"],
        json!(false),
        "曝光 0.6 的表不该被判成恒等"
    );
    assert_eq!(state.params.look_cube, out.display().to_string());
    assert_eq!(state.preset, "自定义");
    assert!(state.preview_dirty);

    // 只体检不改参数。
    let peeked = state
        .run_ui_command(
            &ctx,
            "cube.import",
            &json!({"path": out.display().to_string(), "apply": false}),
        )
        .expect("只解析");
    assert_eq!(peeked["applied"], json!(false));

    // 路径改动当场校验：坏文件不能拖到渲染时才静默退化成"没挂"。
    let err = state
        .run_ui_command(
            &ctx,
            "grade.set",
            &json!({"look_cube": dir.join("nope.cube").display().to_string()}),
        )
        .unwrap_err();
    assert!(err.contains("读不出来"), "{err}");
    let junk = dir.join("junk.cube");
    std::fs::write(&junk, "LUT_3D_SIZE 17\n0.0 0.0 0.0\n").expect("写坏表");
    let err = state
        .run_ui_command(
            &ctx,
            "cube.import",
            &json!({"path": junk.display().to_string()}),
        )
        .unwrap_err();
    assert!(err.contains("读 .cube"), "{err}");

    state
        .run_ui_command(&ctx, "grade.set", &json!({"look_cube": ""}))
        .expect("摘掉外观");
    assert_eq!(state.params.look_cube, "");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn audio_config_updates_options_and_validates() {
    let dir = temp_dir("audio");
    let music = dir.join("score.wav");
    std::fs::write(&music, b"RIFF-not-really-a-wave").unwrap();
    let ctx = Context::default();
    let mut state = AppState::new();
    state.headless = true;

    let out = state
        .run_ui_command(
            &ctx,
            "audio.config",
            &json!({
                "path": music.display().to_string(),
                "delay": 1.5,
                "gain": 80.0,
                "fade_in": 2.0,
                "fade_out": 3.0,
                "loop": true,
                "trim": false,
                "acodec": "aac192",
            }),
        )
        .expect("配置配乐");
    assert_eq!(state.render_audio.as_deref(), Some(music.as_path()));
    assert!((state.render_audio_opts.delay - 1.5).abs() < 1e-12);
    assert!((state.render_audio_opts.gain - 80.0).abs() < 1e-12);
    assert!((state.render_audio_opts.fade_in - 2.0).abs() < 1e-12);
    assert!((state.render_audio_opts.fade_out - 3.0).abs() < 1e-12);
    assert!(state.render_audio_opts.looped);
    assert!(!state.render_audio_opts.trim);
    assert_eq!(state.render_audio_opts.acodec, AudioCodec::Aac192);
    assert_eq!(out["options"]["acodec"], json!("aac192"));
    assert_eq!(out["options"]["loop"], json!(true));

    std::fs::write(dir.join("note.txt"), b"x").unwrap();
    for (args, needle) in [
        (json!({"acodec": "flac"}), "acodec"),
        (json!({"delay": -1.0}), "delay"),
        (json!({"gain": 900.0}), "gain"),
        (
            json!({"path": dir.join("missing.wav").display().to_string()}),
            "不存在",
        ),
        (
            json!({"path": dir.join("note.txt").display().to_string()}),
            "扩展名",
        ),
    ] {
        let err = state
            .run_ui_command(&ctx, "audio.config", &args)
            .unwrap_err();
        assert!(err.contains(needle), "{args}：{err}");
    }

    // 空参数只查询，不改设置。
    let status = state
        .run_ui_command(&ctx, "audio.config", &json!({}))
        .expect("查询配乐");
    assert_eq!(status["path"], json!(music.display().to_string()));
    assert!(status["total_seconds"].is_null());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ui_module_accepts_names_and_indexes() {
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(&ctx, "ui.module", &json!({"name": "成片"}))
        .expect("中文模块名");
    assert_eq!(state.module, Module::Render);
    state
        .run_ui_command(&ctx, "ui.module", &json!({"name": "1"}))
        .expect("序号");
    assert_eq!(state.module, Module::Deflicker);
    let err = state
        .run_ui_command(&ctx, "ui.module", &json!({"name": "不存在"}))
        .unwrap_err();
    assert!(err.contains("未知模块"), "{err}");
}

#[test]
fn ui_dock_opens_the_requested_panel() {
    let ctx = Context::default();
    let mut state = AppState::new();
    state.show_dock = false;
    let opened = state
        .run_ui_command(&ctx, "ui.dock", &json!({"name": "AI"}))
        .expect("中文面板名");
    assert_eq!(state.dock_tab, DockTab::Ai);
    assert!(state.show_dock);
    assert_eq!(opened["index"], 3);
    state
        .run_ui_command(&ctx, "ui.dock", &json!({"name": "1"}))
        .expect("序号");
    assert_eq!(state.dock_tab, DockTab::Info);
    let err = state
        .run_ui_command(&ctx, "ui.dock", &json!({"name": "不存在"}))
        .unwrap_err();
    assert!(err.contains("未知面板"), "{err}");
}

/// 分通道去闪要有线性测光才有意义；开了之后偏移必须拆成"曝光 + 色残差"，
/// 关掉要逐位退回单亮度口径。
#[test]
fn per_channel_deflicker_splits_exposure_and_color_residual() {
    let ctx = Context::default();
    let mut state = AppState::demo();
    let n = state.metrics.len();
    state.def_means.clear();
    state
        .run_ui_command(&ctx, "deflicker.set", &json!({"channels": true}))
        .expect("切分通道");
    assert!(
        state.corr_rgb.iter().all(|c| *c == [0.0; 3]),
        "没有线性测光时不该硬编出色闪残差"
    );

    // 每 7 帧一次绿色通道跳变：正是亮度单通道压不住的彩色闪烁。
    state.def_means = (0..n)
        .map(|i| {
            let kick = if i % 7 == 0 { 0.06 } else { 0.0 };
            Some(FrameMeans {
                luma: 0.2,
                rgb: [0.12, 0.1 + kick, 0.08],
            })
        })
        .collect();
    state.recompute_deflicker();
    let a = state.analysis.clone().expect("分析过了");
    assert!(a.stats.channels, "统计要说这是分通道结果");
    let hot = state.corr_rgb[7];
    assert!(
        hot[1].abs() > 1e-4,
        "跳变帧的绿通道该被补回来，实得 {hot:?}"
    );
    for (i, c) in state.corr_rgb.iter().enumerate() {
        for (k, v) in c.iter().enumerate() {
            let want = a.offset(i, k) as f32;
            assert!(
                (state.corr[i] + v - want).abs() < 1e-6,
                "[{i}][{k}] 曝光 + 残差没还原通道偏移"
            );
        }
    }
    assert_eq!(state.ev_at(7).chan, hot, "预览取的就是这份合成");

    state
        .run_ui_command(&ctx, "deflicker.set", &json!({"channels": false}))
        .expect("关分通道");
    assert!(!state.analysis.as_ref().unwrap().stats.channels);
    assert!(
        state.corr_rgb.iter().all(|c| *c == [0.0; 3]),
        "关掉后必须逐帧只剩亮度"
    );
}

/// 演示序列自带两种闪烁：每 7 帧整体掉档（亮度）+ 每 5 帧偏绿（色闪）。
/// 开分通道必须能看到色闪被压下去，否则这个开关在验收时无从判断。
#[test]
fn demo_sequence_exercises_the_channel_toggle() {
    let mut state = AppState::demo();
    let plain = state.analysis.clone().unwrap().stats;
    assert!(!plain.channels);
    assert!(plain.jitter_after < plain.jitter_before, "亮度闪烁该被压住");
    state.def_channels = true;
    state.recompute_deflicker();
    let st = state.analysis.clone().unwrap().stats;
    assert!(st.channels && st.color_before > 0.0);
    assert!(
        st.color_after < st.color_before,
        "分通道该压住色闪：{} → {}",
        st.color_before,
        st.color_after
    );
    assert!(
        state.corr_rgb.iter().any(|c| c.iter().any(|v| *v != 0.0)),
        "演示序列开了分通道就该有残差"
    );
}

#[test]
fn commands_without_a_sequence_return_readable_errors() {
    let ctx = Context::default();
    let mut state = AppState::new();
    for (id, args) in [
        ("sequence.info", json!({})),
        ("frame.select", json!({"index": 1})),
        ("project.get", json!({})),
        ("project.metadata", json!({})),
    ] {
        let err = state.run_ui_command(&ctx, id, &args).unwrap_err();
        assert!(err.contains("还没有打开序列"), "{id}: {err}");
    }
}

#[test]
fn project_metadata_command_saves_and_reaches_export() {
    let dir = temp_dir("meta");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    let out = state
        .run_ui_command(
            &ctx,
            "project.metadata",
            &json!({
                "title": "云海日出",
                "description": "间隔 3s\n牛背山 东平台",
                "creator": "张三",
                "copyright": "© 2026 张三",
            }),
        )
        .expect("写信息");
    assert_eq!(out["saved"], json!(true));
    assert_eq!(
        out["description"],
        json!("间隔 3s 牛背山 东平台"),
        "换行折成空格"
    );
    assert_eq!(
        out["path"],
        json!(project::project_path(&dir).display().to_string())
    );
    let on_disk = project::load(&project::project_path(&dir)).expect("读工程");
    assert_eq!(on_disk.metadata.title, "云海日出");
    assert_eq!(on_disk.metadata.creator, "张三");
    assert_eq!(on_disk.metadata.copyright, "© 2026 张三");
    // 导出选项要带上这一份，静帧的 EXIF 与成片的容器标签才有得写。
    assert_eq!(state.frame_render_options().metadata, on_disk.metadata);
    assert_eq!(state.project_snapshot().metadata, on_disk.metadata);
    // 空串清空，四项都空就是没填。
    let out = state
        .run_ui_command(
            &ctx,
            "project.metadata",
            &json!({"title": null, "description": "", "creator": "", "copyright": ""}),
        )
        .expect("清信息");
    assert_eq!(out["filled"], json!(false));
    let on_disk = project::load(&project::project_path(&dir)).expect("读工程");
    assert_eq!(on_disk.metadata, project::Metadata::default());
    assert!(
        state
            .run_ui_command(&ctx, "project.metadata", &json!({"title": 3}))
            .is_err()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_commands_are_journaled_without_recording_reads_or_failures() {
    let dir = temp_dir("journal");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    let journal_file = aevum_core::journal::path(&dir);
    assert!(!journal_file.exists(), "只读命令不该写日志");

    state
        .run_ui_command(&ctx, "grade.set", &json!({"params": {"exposure": 0.4}}))
        .expect("调色");
    let err = state
        .run_ui_command(&ctx, "grade.set", &json!({"params": {}}))
        .unwrap_err();
    assert!(err.contains("没有要设置的调色字段"), "{err}");

    let listed = state
        .run_ui_command(&ctx, "journal.list", &json!({"limit": 20}))
        .expect("读日志");
    assert_eq!(listed["path"], json!(journal_file.display().to_string()));
    let entries = listed["entries"].as_array().expect("日志数组");
    assert_eq!(entries.len(), 1, "失败与只读命令都不该进日志：{listed}");
    assert_eq!(entries[0]["source"], "gui");
    assert_eq!(entries[0]["op"], "grade.set");
    assert_eq!(entries[0]["detail"]["params"]["exposure"], 0.4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_catalog_command_reaches_the_executor() {
    // 命令清单和执行器的 match 是两份代码；这条测试防止只加了一边。
    let ctx = Context::default();
    let mut state = AppState::new();
    for spec in crate::control::command_catalog() {
        if let Err(e) = state.run_ui_command(&ctx, spec.id, &json!({})) {
            assert!(!e.starts_with("未知命令"), "{} 没接上执行器：{e}", spec.id);
        }
    }
}

/// 取景框命令：写框、换比例、取消、撤销走的是画布上同一套几何。
#[test]
fn crop_set_writes_rect_aspect_and_undoes() {
    let dir = temp_dir("crop");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(64, 48, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    // 分辨率是预览解码回来才回填的，单测里没有那条异步链，直接给定。
    state.resolution = Some((64, 48));

    let out = state
        .run_ui_command(
            &ctx,
            "crop.set",
            &json!({"x": 8, "y": 6, "width": 32, "height": 24}),
        )
        .expect("写取景框");
    assert_eq!(
        out["manual"],
        json!({"x": 8, "y": 6, "width": 32, "height": 24})
    );
    assert_eq!(
        out["effective"], out["manual"],
        "原生比例下取景框就是用户框"
    );

    // 换方形：已有框绕自己的中心重铺成画面里放得下的最大方形。
    let out = state
        .run_ui_command(&ctx, "crop.set", &json!({"aspect": "1:1"}))
        .expect("换画幅");
    assert_eq!(out["aspect"], json!("1:1"));
    assert_eq!(
        out["manual"],
        json!({"x": 8, "y": 2, "width": 32, "height": 32})
    );

    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤画幅");
    assert_eq!(state.aspect, Aspect::Native);
    assert_eq!(
        state.user_crop.map(|c| (c.x, c.y, c.w, c.h)),
        Some((8, 6, 32, 24))
    );

    let out = state
        .run_ui_command(&ctx, "crop.set", &json!({"reset": true}))
        .expect("取消裁剪");
    assert_eq!(out["manual"], json!(null));
    assert_eq!(state.user_crop, None);

    // 空参数是读回，不该占一步历史。
    let steps = state.history.steps().len();
    let out = state
        .run_ui_command(&ctx, "crop.set", &json!({}))
        .expect("读回取景");
    assert_eq!(out["frame"], json!({"width": 64, "height": 48}));
    assert_eq!(state.history.steps().len(), steps, "只读命令不记步");

    // 半套坐标和认不出的比例都得报错，不能静默画个歪框。
    let err = state
        .run_ui_command(&ctx, "crop.set", &json!({"x": 4, "width": 10}))
        .unwrap_err();
    assert!(err.contains("x / y / width / height"), "{err}");
    let err = state
        .run_ui_command(&ctx, "crop.set", &json!({"aspect": "banana"}))
        .unwrap_err();
    assert!(err.contains("画幅比例"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 输出设置命令：容器、质量、EXIF、锐化、色域、规格一处写定，撤销走同一份快照。
#[test]
fn render_set_writes_the_output_knobs_and_undoes() {
    let dir = temp_dir("render");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(64, 48, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    state.resolution = Some((64, 48));

    // 空参数是读回，默认是视频 + PNG 无损档。
    let out = state
        .run_ui_command(&ctx, "render.set", &json!({}))
        .expect("读回输出设置");
    assert_eq!(out["mode"], json!("video"));
    assert_eq!(out["format"], json!("png8"));
    assert_eq!(out["quality_applies"], json!(false));
    assert_eq!(out["sharpen_for"], json!("off"));

    // 视频模式的中间帧只能无损，有损容器现在就撞回。
    let err = state
        .run_ui_command(&ctx, "render.set", &json!({"format": "jpeg"}))
        .unwrap_err();
    assert!(err.contains("只能出静帧"), "{err}");

    let out = state
        .run_ui_command(
            &ctx,
            "render.set",
            &json!({
                "mode": "stills",
                "format": "jpeg",
                "quality": 72,
                "keep_exif": true,
                "xmp_sidecar": true,
                "sharpen_for": "screen",
                "sharpen_strength": "high",
                "color_space": "p3",
                "width": 1920,
                "height": 1080,
            }),
        )
        .expect("改成 JPEG 成片");
    assert_eq!(out["format"], json!("jpeg"));
    assert_eq!(out["quality"], json!(72));
    assert_eq!(out["quality_applies"], json!(true));
    assert_eq!(out["exif_applies"], json!(true));
    assert_eq!(out["xmp_sidecar"], json!(true));
    assert_eq!(out["sidecar_applies"], json!(true), "静帧模式该发旁车");
    assert_eq!(out["sharpen_for"], json!("screen"));
    assert_eq!(out["sharpen_strength"], json!("high"));
    assert_eq!(out["color_space"], json!("p3"));

    // WebP 是无损档，质量参数对它不起作用。
    let out = state
        .run_ui_command(&ctx, "render.set", &json!({"format": "webp"}))
        .expect("换 WebP");
    assert_eq!(out["quality_applies"], json!(false));

    // 切回视频：容器是有损档就整条命令拒绝，不留「视频模式 + WebP 中间帧」。
    let err = state
        .run_ui_command(&ctx, "render.set", &json!({"mode": "video"}))
        .unwrap_err();
    assert!(err.contains("只能出静帧"), "{err}");
    assert_eq!(
        state.render_mode,
        OutputMode::Stills,
        "被拒的命令不该改一半"
    );
    let out = state
        .run_ui_command(
            &ctx,
            "render.set",
            &json!({"mode": "video", "format": "png8"}),
        )
        .expect("换回视频 + PNG");
    assert_eq!(out["mode"], json!("video"));
    assert_eq!(out["format"], json!("png8"));
    assert_eq!(
        out["sidecar_applies"],
        json!(false),
        "视频模式的中间帧不发旁车"
    );

    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤回静帧");
    assert_eq!(state.render_format, StillFormat::WebP);
    assert_eq!(state.render_mode, OutputMode::Stills);
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤 JPEG");
    assert_eq!(state.render_format, StillFormat::Jpeg);
    assert_eq!(state.render_quality, 72);
    assert_eq!(state.render_sharpen_for, OutputSharpen::Screen);
    assert_eq!((state.output_w, state.output_h), (1920, 1080));
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤整套成片设置");
    assert_eq!(state.render_mode, OutputMode::Video);
    assert_eq!(state.render_format, StillFormat::Png8);
    assert_eq!(state.render_quality, 90);
    assert!(!state.render_keep_exif);
    assert!(!state.render_xmp_sidecar, "撤销要连旁车开关一起退回去");
    assert_eq!(state.render_sharpen_for, OutputSharpen::Off);
    assert_eq!((state.output_w, state.output_h), (3840, 2160));

    // 半套规格、越界质量、认不出的容器都要说话。
    let err = state
        .run_ui_command(&ctx, "render.set", &json!({"width": 1920}))
        .unwrap_err();
    assert!(err.contains("同时给出"), "{err}");
    let err = state
        .run_ui_command(&ctx, "render.set", &json!({"quality": 0}))
        .unwrap_err();
    assert!(err.contains("quality"), "{err}");
    let err = state
        .run_ui_command(&ctx, "render.set", &json!({"format": "banana"}))
        .unwrap_err();
    assert!(err.contains("未知容器"), "{err}");

    // 水印整块合并：只写 opacity 不许把正文清空，认不出的键要说话。
    let out = state
        .run_ui_command(
            &ctx,
            "render.set",
            &json!({
                "watermark": {
                    "enabled": true,
                    "text": "© 恒晷",
                    "spot": "top-left",
                    "opacity": 70,
                }
            }),
        )
        .expect("开水印");
    assert_eq!(out["watermark"]["enabled"], json!(true));
    assert_eq!(out["watermark"]["text"], json!("© 恒晷"));
    assert_eq!(out["watermark"]["spot"], json!("top-left"));
    assert_eq!(out["watermark_applies"], json!(true));
    assert_eq!(state.render_watermark.opacity, 70);
    assert_eq!(state.render_watermark.size, 4, "没写的键保持当前值");
    assert_eq!(state.render_watermark.margin, 2);
    assert_eq!(state.render_watermark.kind, WatermarkKind::Text);

    let out = state
        .run_ui_command(&ctx, "render.set", &json!({"watermark": {"opacity": 30}}))
        .expect("只改不透明度");
    assert_eq!(
        out["watermark"]["text"],
        json!("© 恒晷"),
        "只写一个键不该动正文"
    );
    assert_eq!(state.render_watermark.opacity, 30);

    let err = state
        .run_ui_command(
            &ctx,
            "render.set",
            &json!({"watermark": {"spot": "banana"}}),
        )
        .unwrap_err();
    assert!(err.contains("水印参数"), "{err}");
    let err = state
        .run_ui_command(&ctx, "render.set", &json!({"watermark": {"nope": 1}}))
        .unwrap_err();
    assert!(err.contains("未知的水印参数"), "{err}");
    let err = state
        .run_ui_command(
            &ctx,
            "render.set",
            &json!({"watermark": {"image": "E:/没有这张图.png"}}),
        )
        .unwrap_err();
    assert!(err.contains("水印图片不存在"), "{err}");
    let err = state
        .run_ui_command(&ctx, "render.set", &json!({"watermark": {"size": 90}}))
        .unwrap_err();
    assert!(err.contains("size"), "{err}");

    // 撤销：水印整块同进同退，不许只回一半。
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤改不透明度");
    assert_eq!(state.render_watermark.text, "© 恒晷");
    assert_eq!(state.render_watermark.opacity, 70);
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤开水印");
    assert_eq!(state.render_watermark, Watermark::default());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 变速：三种曲线写法、快门与读数走同一条命令通道；整块进撤销，坏参数不留半套。
#[test]
fn retime_set_takes_every_curve_shape_and_undoes() {
    let dir = temp_dir("retime");
    for i in 0..8u8 {
        let img = image::RgbImage::from_pixel(16, 12, image::Rgb([i * 8, 30, 60]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");

    // 没开变速：读数就是源帧数，帧率 24 下 8 帧 = 1/3 秒。
    let out = state
        .run_ui_command(&ctx, "retime.status", &json!({}))
        .expect("读变速");
    assert_eq!(out["enabled"], json!(false));
    assert_eq!(out["identity"], json!(true));
    assert_eq!(out["frames"], json!(8));
    assert!(
        (out["seconds"].as_f64().unwrap() - 8.0 / 24.0).abs() < 1e-6,
        "{}",
        out["seconds"]
    );

    let out = state
        .run_ui_command(&ctx, "retime.set", &json!({"speed": 2.0}))
        .expect("设倍速");
    assert_eq!(out["enabled"], json!(true), "给了曲线就自动开变速");
    assert_eq!(out["frames"], json!(4), "2× 把 8 帧压成 4 帧");
    assert_eq!(out["keys"].as_array().unwrap().len(), 2);

    let out = state
        .run_ui_command(&ctx, "retime.set", &json!({"shutter": 0.5}))
        .expect("设快门");
    assert_eq!(out["shutter"], json!(0.5));
    assert_eq!(out["frames"], json!(4), "快门只改混多少帧，不改输出帧数");

    let out = state
        .run_ui_command(
            &ctx,
            "retime.set",
            &json!({"keys": [
                {"at": 0.0, "speed": 1.0},
                {"at": 0.5, "speed": 4.0},
                {"at": 1.0, "speed": 1.0},
            ]}),
        )
        .expect("写速度曲线");
    assert_eq!(out["keys"].as_array().unwrap().len(), 3);
    let frames = out["frames"].as_u64().unwrap();
    assert!((2..8).contains(&frames), "两头常速中段 4×：{frames} 帧");

    let out = state
        .run_ui_command(&ctx, "retime.set", &json!({"ease": {"peak": 4.0}}))
        .expect("缓入缓出");
    assert_eq!(out["keys"].as_array().unwrap().len(), 4);

    // 坏参数整条拒绝，不许留下半套曲线。
    let err = state
        .run_ui_command(
            &ctx,
            "retime.set",
            &json!({"speed": 2.0, "keys": [{"at": 0.0, "speed": 2.0}]}),
        )
        .unwrap_err();
    assert!(err.contains("只能选一种写法"), "{err}");
    let err = state
        .run_ui_command(&ctx, "retime.set", &json!({"speed": 0}))
        .unwrap_err();
    assert!(err.contains("倍速必须大于 0"), "{err}");
    let err = state
        .run_ui_command(&ctx, "retime.set", &json!({"shutter": 1.5}))
        .unwrap_err();
    assert!(err.contains("shutter"), "{err}");
    let err = state
        .run_ui_command(&ctx, "retime.set", &json!({"keys": [{"at": 0.5}]}))
        .unwrap_err();
    assert!(err.contains("speed"), "{err}");
    assert_eq!(state.retime.keys.len(), 4, "失败的命令一个键都没动");

    // 撤销：开关、曲线、快门同进同退，四步回到默认。
    for _ in 0..4 {
        state
            .run_ui_command(&ctx, "history.undo", &json!({}))
            .expect("撤销变速");
    }
    assert_eq!(state.retime, Retime::default(), "变速整块回到默认");

    let out = state
        .run_ui_command(&ctx, "retime.set", &json!({"speed": 0.5}))
        .expect("慢放");
    assert_eq!(out["frames"], json!(16), "0.5× 把 8 帧拆成 16 帧");

    // 关变速只改开关：曲线留着，读数回 1:1，开回来还是同一条曲线。
    let out = state
        .run_ui_command(&ctx, "retime.set", &json!({"enabled": false}))
        .expect("关变速");
    assert_eq!(out["identity"], json!(true));
    assert_eq!(out["frames"], json!(8));
    assert_eq!(state.retime.keys.len(), 2);
    let out = state
        .run_ui_command(&ctx, "retime.set", &json!({"reset": true}))
        .expect("清变速");
    assert_eq!(state.retime, Retime::default());
    assert!(out["keys"].as_array().unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 变速跟着工程文件走：存盘、重新载入后曲线与快门原样回来。
#[test]
fn retime_survives_a_project_roundtrip() {
    let dir = temp_dir("retime-proj");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(16, 12, image::Rgb([i * 40, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    state
        .run_ui_command(
            &ctx,
            "retime.set",
            &json!({"keys": [{"at": 0.0, "speed": 1.0}, {"at": 1.0, "speed": 3.0}], "shutter": 0.75}),
        )
        .expect("写曲线");
    state
        .run_ui_command(&ctx, "project.save", &json!({}))
        .expect("存工程");

    let mut fresh = AppState::new();
    fresh
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("重新打开");
    assert_eq!(fresh.retime, state.retime, "工程载入把变速整块带回来");
    assert_eq!(fresh.retime.shutter, 0.75);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 运镜：框、曲线、反向、换比例都走同一条命令通道；坏参数不动状态，
/// 撤销一步回整块（开关不许只回一半），工程往返把两个框原样带回。
#[test]
fn motion_set_walks_the_box_and_undoes_as_one_block() {
    let dir = temp_dir("motion");
    for i in 0..4u8 {
        let img = image::RgbImage::from_pixel(16, 12, image::Rgb([i * 40, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    // 推近倍数按输出画幅算：这里让成片就是 16×12，读数才对得上肉眼。
    state.resolution = Some((16, 12));
    state.output_w = 16;
    state.output_h = 12;

    let out = state
        .run_ui_command(&ctx, "motion.status", &json!({}))
        .expect("读运镜");
    assert_eq!(out["enabled"], json!(false));
    assert_eq!(out["active"], json!(false), "关着就不动");
    assert_eq!(out["from"], json!([0.0, 0.0, 1.0, 1.0]));
    assert_eq!(out["frame"], json!([16, 12]));
    assert_eq!(out["source_frames"], json!(4));

    // 只写终点：开关自动推上去，起点照旧。
    let out = state
        .run_ui_command(&ctx, "motion.set", &json!({"to": [0.25, 0.25, 0.5, 0.5]}))
        .expect("写终点框");
    assert_eq!(out["enabled"], json!(true));
    assert_eq!(out["active"], json!(true));
    assert_eq!(out["to"], json!([0.25, 0.25, 0.5, 0.5]));
    assert_eq!(out["from"], json!([0.0, 0.0, 1.0, 1.0]));
    // 终点框 8×6 撑满 16×12 就是 2×，起点铺满整幅是 1×。
    assert_eq!(out["zoom_to"], json!(2.0));
    assert_eq!(out["zoom_from"], json!(1.0));

    let out = state
        .run_ui_command(&ctx, "motion.set", &json!({"ease": "linear"}))
        .expect("换曲线");
    assert_eq!(out["ease"], json!("linear"));
    assert_eq!(out["ease_cn"], json!("匀速"));
    assert_eq!(out["to"], json!([0.25, 0.25, 0.5, 0.5]), "换曲线不动框");

    let err = state
        .run_ui_command(&ctx, "motion.set", &json!({"ease": "bouncy"}))
        .unwrap_err();
    assert!(err.contains("smooth / linear / in / out"), "{err}");
    assert_eq!(state.motion.ease, Ease::Linear, "失败的命令一个键都没动");
    let err = state
        .run_ui_command(&ctx, "motion.set", &json!({"from": [0.1, 0.2]}))
        .unwrap_err();
    assert!(err.contains("from 要写成四个数"), "{err}");

    let out = state
        .run_ui_command(&ctx, "motion.set", &json!({"swap": true}))
        .expect("反向");
    assert_eq!(out["from"], json!([0.25, 0.25, 0.5, 0.5]));
    assert_eq!(out["to"], json!([0.0, 0.0, 1.0, 1.0]));

    // 换比例重铺：两个框一起按新跨度走，幅度不会被夹成同一个框。
    let out = state
        .run_ui_command(&ctx, "motion.set", &json!({"refit": [1.0, 0.5]}))
        .expect("重铺");
    assert_eq!(out["from"][3], json!(0.5));
    assert!(out["from"] != out["to"], "重铺不许把运镜幅度抹平");

    // 撤销一步回整块：一次命令改两处（终点框 + 曲线），撤销不许只回一半。
    let before = state.motion;
    state
        .run_ui_command(
            &ctx,
            "motion.set",
            &json!({"to": [0.4, 0.4, 0.4, 0.4], "ease": "in"}),
        )
        .expect("一次改两处");
    assert_ne!(state.motion, before);
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤销运镜");
    assert_eq!(state.motion, before, "开关、框、曲线一起退，不许只回一半");
    state
        .run_ui_command(&ctx, "history.redo", &json!({}))
        .expect("重做运镜");
    assert_eq!(state.motion.ease, Ease::In);
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("再撤一次");

    let out = state
        .run_ui_command(
            &ctx,
            "motion.set",
            &json!({"to": { "x": 0.2, "y": 0.1, "w": 0.6, "h": 0.8 }}),
        )
        .expect("对象写法");
    assert_eq!(out["to"], json!([0.2, 0.1, 0.6, 0.8]));
    state
        .run_ui_command(&ctx, "project.save", &json!({}))
        .expect("存工程");
    let mut fresh = AppState::new();
    fresh
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("重新打开");
    assert_eq!(fresh.motion, state.motion, "工程载入把运镜整块带回来");

    let out = state
        .run_ui_command(&ctx, "motion.set", &json!({"reset": true}))
        .expect("清运镜");
    assert_eq!(state.motion, Motion::default());
    assert_eq!(out["enabled"], json!(false));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 拉直：角度进命令、进导出取景、进撤销，且和框一起走同一份快照。
#[test]
fn straighten_shrinks_the_export_framing_and_undoes() {
    let dir = temp_dir("straighten");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(64, 48, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    state.resolution = Some((64, 48));

    // 0°、原生比例、没拖过框：不裁，取景是空的。
    let out = state
        .run_ui_command(&ctx, "crop.set", &json!({}))
        .expect("读回取景");
    assert_eq!(out["rotate"], json!(0.0));
    assert_eq!(out["effective"], json!(null));

    let out = state
        .run_ui_command(&ctx, "crop.set", &json!({"rotate": 5.0}))
        .expect("拉直 5°");
    assert_eq!(out["rotate"], json!(5.0));
    let eff = &out["effective"];
    let (x, y) = (eff["x"].as_u64().unwrap(), eff["y"].as_u64().unwrap());
    let (w, h) = (
        eff["width"].as_u64().unwrap(),
        eff["height"].as_u64().unwrap(),
    );
    assert!(x > 0 && y > 0, "转过之后取景该往里收：{out}");
    assert!(w < 64 && h < 48, "安全框该比整幅小：{out}");
    assert_eq!(x % 2, 0);
    assert_eq!(y % 2, 0);

    // 0.2° 以内当没转：不然推子停在 -0.1 时导出一圈无谓的黑边。
    state.set_rotate(0.1);
    assert_eq!(state.rotate, 0.0);

    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤拉直");
    assert_eq!(state.rotate, 0.0);
    assert_eq!(state.effective_crop(), None, "撤到底该回到不取景");

    // 角度和框是一套设置：拉直后取消取景，两者一起回默认。
    state
        .run_ui_command(&ctx, "crop.set", &json!({"rotate": -8.0}))
        .expect("拉直 -8°");
    state
        .run_ui_command(
            &ctx,
            "crop.set",
            &json!({"x": 4, "y": 4, "width": 40, "height": 30}),
        )
        .expect("写框");
    let out = state
        .run_ui_command(&ctx, "crop.set", &json!({"reset": true}))
        .expect("取消取景");
    assert_eq!(out["rotate"], json!(0.0));
    assert_eq!(out["manual"], json!(null));
    assert_eq!(state.user_crop, None);

    let err = state
        .run_ui_command(&ctx, "crop.set", &json!({"rotate": 90.0}))
        .unwrap_err();
    assert!(err.contains("-45..45"), "{err}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_and_select_work_and_export_needs_confirm() {
    let dir = temp_dir("open");
    for i in 0..2u8 {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    let info = state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    assert_eq!(info["frames"], json!(2));
    assert_eq!(info["opened"], json!(true));

    // 打开一个空文件夹必须报错，不能把旧序列当成新打开的结果。
    let empty = temp_dir("empty");
    let err = state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": empty.display().to_string()}),
        )
        .unwrap_err();
    assert!(!err.is_empty());
    assert_eq!(state.frame_count(), 2, "失败时旧序列应保持原样");
    let _ = std::fs::remove_dir_all(&empty);

    state
        .run_ui_command(&ctx, "frame.select", &json!({"index": 2}))
        .expect("选第二帧");
    assert_eq!(state.selected, 1);
    let err = state
        .run_ui_command(&ctx, "frame.select", &json!({"index": 3}))
        .unwrap_err();
    assert!(err.contains("1..=2"), "{err}");

    let err = state
        .run_ui_command(&ctx, "export.video", &json!({"confirm": false}))
        .unwrap_err();
    assert!(err.contains("confirm"), "{err}");

    // 就算工程文件把 video_out 指向素材，也必须拒绝写入。
    state.render_video_out = dir.join("f000.png");
    let err = state
        .run_ui_command(&ctx, "export.video", &json!({"confirm": true}))
        .unwrap_err();
    assert!(err.contains("拒绝覆盖素材"), "{err}");

    let project = state
        .run_ui_command(&ctx, "project.get", &json!({}))
        .expect("读工程");
    assert!(project["path"].as_str().unwrap().ends_with("project.json"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ai_line_records_success_and_failure() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let value = state
        .run_ai_line(&ctx, "grade.get")
        .expect("执行 grade.get");
    assert!(value["preset"].is_string());
    let err = state
        .run_ai_line(&ctx, "grade.set {\"bogus\": 1}")
        .unwrap_err();
    assert!(err.contains("bogus"), "{err}");
    assert_eq!(state.ai_log.len(), 2);
    assert!(state.ai_log[0].ok);
    assert!(!state.ai_log[1].ok);

    let err = state.run_ai_line(&ctx, "grade.set {oops").unwrap_err();
    assert!(err.contains("JSON"), "{err}");
}

#[test]
fn screenshot_requires_png_and_never_overwrites_sources() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let err = state
        .run_ui_command(&ctx, "ui.screenshot", &json!({"path": "shot.jpg"}))
        .unwrap_err();
    assert!(err.contains("png"), "{err}");
}

#[test]
fn export_cancel_is_idempotent() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let value = state
        .run_ui_command(&ctx, "export.cancel", &json!({}))
        .expect("取消");
    assert_eq!(value["cancelling"], json!(false));
}

#[test]
fn out_dir_safety_rejects_root_and_parent() {
    let root = temp_dir("safe");
    assert!(ensure_out_dir_safe(&root, &root).is_err());
    assert!(ensure_out_dir_safe(&root, root.parent().unwrap()).is_err());
    assert!(ensure_out_dir_safe(&root, &root.join("成片")).is_ok());
    let _ = std::fs::remove_dir_all(&root);
}

/// 「导出当前帧」这条单帧路以前无条件走 8bit：批量导出早就保位深了，两条路
/// 对同一个源给出不同精度是说不过去的。这条守住 GUI 侧不再截位深。
#[test]
fn single_frame_export_keeps_16bit_source() {
    use image::{ImageBuffer, Rgb};

    let dir = temp_dir("bitdepth");
    let params = GradeParams::default();

    let deep = dir.join("deep.png");
    let mut img: ImageBuffer<Rgb<u16>, Vec<u16>> =
        ImageBuffer::from_pixel(2, 1, Rgb([1000, 20000, 40000]));
    img.get_pixel_mut(1, 0).0[0] = 1010;
    img.save(&deep).unwrap();
    let deep_out = dir.join("deep_graded.png");
    render_full_file(FrameExport {
        src: &deep,
        dst: &deep_out,
        params: &params,
        gain: Ev::NONE,
        full_size: None,
        crop: None,
        framing: Framing::default(),
        render: RenderOptions::still(StillFormat::Png8),
    })
    .expect("导出 16bit 帧");
    let got = image::open(&deep_out).unwrap().to_rgb16();
    assert_eq!(
        image::open(&deep_out).unwrap().color(),
        image::ColorType::Rgb16
    );
    let a = got.get_pixel(0, 0).0[0];
    let b = got.get_pixel(1, 0).0[0];
    assert!(a.abs_diff(b) >= 8, "相邻 16bit 级又被并了：{a} vs {b}");

    let flat = dir.join("flat.png");
    image::RgbImage::from_pixel(2, 1, Rgb([4, 78, 156]))
        .save(&flat)
        .unwrap();
    let flat_out = dir.join("flat_graded.png");
    render_full_file(FrameExport {
        src: &flat,
        dst: &flat_out,
        params: &params,
        gain: Ev::NONE,
        full_size: None,
        crop: None,
        framing: Framing::default(),
        render: RenderOptions::still(StillFormat::Png8),
    })
    .expect("导出 8bit 帧");
    assert_eq!(
        image::open(&flat_out).unwrap().color(),
        image::ColorType::Rgb8,
        "8bit 源不该被顺手升成 16bit，白让文件大一倍"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 单帧导出真正读面板那排旋钮：容器、色域搬运、输出锐化都要落到磁盘上。
/// 这条以前是静默忽略——面板切 JPEG 却写 PNG，选 P3 却不搬像素。
#[test]
fn single_frame_export_follows_the_output_panel() {
    use image::Rgb;

    let dir = temp_dir("panel");
    let src = dir.join("edge.png");
    // 一半饱和红、一半暗灰：红是 P3 里收得动的原色，边本身又是强亮度边，
    // 色域搬运和输出锐化两件事都能在这一张图上验出来。
    image::RgbImage::from_fn(160, 90, |x, _| {
        if x < 80 {
            Rgb([255, 0, 0])
        } else {
            Rgb([30, 30, 30])
        }
    })
    .save(&src)
    .unwrap();

    // 容器：面板切到 JPEG，落盘的字节就得是 JPEG。
    let jpg = dir.join("edge.jpg");
    export_still(
        &src,
        &jpg,
        RenderOptions {
            quality: 92,
            ..RenderOptions::still(StillFormat::Jpeg)
        },
    );
    let bytes = std::fs::read(&jpg).unwrap();
    assert_eq!(&bytes[..2], b"\xFF\xD8", "选了 JPEG 写出来的却不是 JPEG");
    assert_eq!(
        image::open(&jpg).unwrap().color(),
        image::ColorType::Rgb8,
        "JPEG 没有 16bit 档，深源在这里要落到 8bit"
    );

    // WebP 也是静帧容器，无损但另一套封装。
    let webp = dir.join("edge.webp");
    export_still(&src, &webp, RenderOptions::still(StillFormat::WebP));
    let webp_bytes = std::fs::read(&webp).unwrap();
    assert_eq!(&webp_bytes[8..12], b"WEBP", "选了 WebP 写出来的却不是 WebP");

    // 色域：Display P3 会真的搬像素，sRGB 档一个字节都不动。
    let srgb = dir.join("srgb.png");
    export_still(&src, &srgb, RenderOptions::still(StillFormat::Png8));
    let p3 = dir.join("p3.png");
    export_still(
        &src,
        &p3,
        RenderOptions {
            color_space: ColorSpace::DisplayP3,
            ..RenderOptions::still(StillFormat::Png8)
        },
    );
    let base = image::open(&srgb).unwrap().to_rgb8();
    let moved = image::open(&p3).unwrap().to_rgb8();
    assert_ne!(
        base.as_raw(),
        moved.as_raw(),
        "选了 Display P3 却一个像素都没搬"
    );
    // 方向也要对：饱和红在 P3 里不是原色，编码值必须收、蓝必须被抬起。
    let red_off = base.get_pixel(10, 45).0;
    let red_on = moved.get_pixel(10, 45).0;
    assert!(
        red_on[0] < red_off[0] && red_on[2] > red_off[2],
        "P3 转换方向反了：{red_off:?} → {red_on:?}"
    );

    // 锐化：默认关，开到「屏幕」只许在边上动手。
    let soft = dir.join("soft.png");
    export_still(&src, &soft, RenderOptions::still(StillFormat::Png8));
    let hard = dir.join("hard.png");
    export_still(
        &src,
        &hard,
        RenderOptions {
            sharpen_for: OutputSharpen::Screen,
            ..RenderOptions::still(StillFormat::Png8)
        },
    );
    let off = image::open(&soft).unwrap().to_rgb8();
    let on = image::open(&hard).unwrap().to_rgb8();
    assert_eq!(
        off.as_raw(),
        image::open(&src).unwrap().to_rgb8().as_raw(),
        "默认设定下单帧导出要和源逐位一致"
    );
    assert_eq!(off.as_raw(), base.as_raw(), "锐化默认没关");
    assert_ne!(on.as_raw(), off.as_raw(), "输出锐化对单帧这条路是空转");
    for y in 0..90u32 {
        assert_eq!(off.get_pixel(5, y).0, on.get_pixel(5, y).0, "平场被锐化了");
        assert_eq!(
            off.get_pixel(155, y).0,
            on.get_pixel(155, y).0,
            "平场被锐化了"
        );
    }
    assert!(
        (78..82).any(|x| off.get_pixel(x, 45).0 != on.get_pixel(x, 45).0),
        "边缘一点变化都没有，输出锐化等于没接上"
    );

    // XMP 旁车：勾上就在静帧旁边多一个同名 .xmp（dc: 四项 + crs: 调色），
    // 没勾的那些输出一份旁车都不许有。
    assert!(!dir.join("srgb.xmp").exists(), "没勾旁车却写出来了");
    let sidecar_png = dir.join("sidecar.png");
    export_still(
        &src,
        &sidecar_png,
        RenderOptions {
            xmp_sidecar: true,
            metadata: project::Metadata {
                title: "云海 & 日出".to_owned(),
                creator: "张三".to_owned(),
                ..project::Metadata::default()
            },
            ..RenderOptions::still(StillFormat::Png8)
        },
    );
    let text = std::fs::read_to_string(dir.join("sidecar.xmp")).expect("旁车没写出来");
    assert!(
        text.contains("云海 &amp; 日出"),
        "标题没做 XML 转义：{text}"
    );
    assert!(
        text.contains("<rdf:li>张三</rdf:li>"),
        "创作者没进 dc:creator"
    );
    assert!(text.contains("crs:Exposure2012"), "旁车缺 crs: 参数");
    assert!(!text.contains("dc:description"), "空字段不该写成空标签");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 副本的存/覆盖/套用/删都走命令通道，且每一步都能撤销。
#[test]
fn grade_copies_save_apply_delete_and_undo() {
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": 0.8}))
        .expect("改参");
    state
        .run_ui_command(&ctx, "grade.copy", &json!({"op": "save", "name": "日光版"}))
        .expect("存副本");
    let err = state
        .run_ui_command(&ctx, "grade.copy", &json!({"op": "save", "name": "日光版"}))
        .unwrap_err();
    assert!(err.contains("overwrite"), "{err}");
    state
        .run_ui_command(
            &ctx,
            "grade.copy",
            &json!({"op": "save", "name": "日光版", "overwrite": true}),
        )
        .expect("覆盖");
    state
        .run_ui_command(&ctx, "grade.set", &json!({"exposure": -0.4}))
        .expect("再改参");
    let out = state
        .run_ui_command(
            &ctx,
            "grade.copy",
            &json!({"op": "apply", "name": "日光版"}),
        )
        .expect("套用");
    assert_eq!(out["params"]["exposure"], json!(0.8));
    assert_eq!(out["active_copy"], json!("日光版"));
    // 套用后再动一下参数，当前调色就不等于任何副本了。
    state
        .run_ui_command(&ctx, "grade.set", &json!({"contrast": 12.0}))
        .expect("改");
    let out = state
        .run_ui_command(&ctx, "grade.copy", &json!({}))
        .expect("list");
    assert_eq!(out["active_copy"], json!(null));
    assert_eq!(out["copies"].as_array().unwrap().len(), 1);
    state
        .run_ui_command(
            &ctx,
            "grade.copy",
            &json!({"op": "delete", "name": "日光版"}),
        )
        .expect("删");
    let out = state
        .run_ui_command(&ctx, "grade.copy", &json!({}))
        .expect("list");
    assert!(out["copies"].as_array().unwrap().is_empty());
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤删除");
    let out = state
        .run_ui_command(&ctx, "grade.copy", &json!({}))
        .expect("list");
    assert_eq!(out["copies"][0]["name"], json!("日光版"));
    state
        .run_ui_command(&ctx, "history.undo", &json!({}))
        .expect("撤改参");
    // 退回到「刚套用完」那一步：参数回来了，副本还在。
    assert!((state.params.exposure - 0.8).abs() < 1e-4);
    assert_eq!(state.active_copy(), Some("日光版"));
}

/// 导出指定副本：名字不对要在启动前拦下；对了才起后台任务。
#[test]
fn export_stills_validates_the_copy_name_first() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = super::history_tests::two_frame_dir("copy-export");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    let out = dir.join("成片");
    let err = state
        .run_ui_command(
            &ctx,
            "export.stills",
            &json!({"confirm": true, "copy": "没有这版", "out_dir": out.display().to_string()}),
        )
        .unwrap_err();
    assert!(err.contains("没有叫 没有这版 的副本"), "{err}");
    assert!(!state.export_busy, "校验失败不该起导出");
    state
        .run_ui_command(
            &ctx,
            "grade.copy",
            &json!({"op": "save", "name": "没有这版"}),
        )
        .expect("存副本");
    let ok = state
        .run_ui_command(
            &ctx,
            "export.stills",
            &json!({"confirm": true, "copy": "没有这版", "out_dir": out.display().to_string()}),
        )
        .expect("按副本导出");
    assert_eq!(ok["copy"], json!("没有这版"));
    state.stop_export();
    let _ = std::fs::remove_dir_all(&dir);
}

/// HDR 合并：批量写要 confirm、至少 2 帧；正常两帧融成一张并落地成文件。
/// 命令只负责启动后台任务，成片路径要等 `Msg::Merged` 落账才拿得到。
#[test]
fn hdr_merge_fuses_the_bracket_and_guards_input() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = super::history_tests::two_frame_dir("hdr-merge");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    let need_confirm = state
        .run_ui_command(&ctx, "hdr.merge", &json!({"frames": [0, 1]}))
        .unwrap_err();
    assert!(need_confirm.contains("confirm"), "{need_confirm}");
    let too_few = state
        .run_ui_command(&ctx, "hdr.merge", &json!({"frames": [0], "confirm": true}))
        .unwrap_err();
    assert!(too_few.contains("至少要 2 帧"), "{too_few}");
    assert!(!state.merge_busy, "校验失败不该起任务");
    // 上一轮留下的取消旗（哪怕已经立着）不能传给新任务：共用一根旗时新任务
    // `store(false)` 会复活旧 job，反过来旧旗也会立刻取消新 job。
    state.cancel_merge.store(true, Ordering::SeqCst);
    let old_flag = state.cancel_merge.clone();
    let out_dir = dir.join("HDR");
    let ok = state
        .run_ui_command(
            &ctx,
            "hdr.merge",
            &json!({"frames": [0, 1], "out_dir": out_dir.display().to_string(), "confirm": true}),
        )
        .expect("启动 HDR 合并");
    assert_eq!(ok["started"], json!(true));
    assert_eq!(ok["kind"], json!("hdr"));
    assert_eq!(ok["frames"], json!(2));
    assert!(state.merge_busy, "启动后应标记忙碌");
    assert!(
        !Arc::ptr_eq(&old_flag, &state.cancel_merge),
        "每个合并 job 必须新开一根取消旗"
    );
    assert!(
        !state.cancel_merge.load(Ordering::SeqCst),
        "新任务不能被上一轮的旗立刻取消"
    );
    wait_merge(&mut state, &ctx);
    let done = state.merge_result.clone().expect("合并结果落账");
    assert_eq!(done["kind"], json!("hdr"));
    assert_eq!(done["width"], json!(8));
    assert_eq!(done["height"], json!(6));
    let path = done["path"].as_str().expect("成片路径");
    assert!(Path::new(path).is_file(), "HDR 成片没写出来：{path}");
    assert_eq!(state.hdr_last.as_deref(), Some(path), "面板回显要跟着落账");
    assert_eq!(state.merge_status_json()["busy"], json!(false));
    assert_eq!(
        state.merge_status_json()["done"],
        state.merge_status_json()["total"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 三张左移 24 像素的接片序列：中灰背景 + 40 个灰度各异的方块，角点全图唯一。
fn pano_frame_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("aevum-pano-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    fn blob(k: usize) -> (i64, i64, i64, u8) {
        let bx = ((k * 37) % 100) as i64;
        let by = (((k * 23) % 40) + 4) as i64;
        let size = 4 + (k % 3) as i64;
        let gray = (25.0 + 205.0 * (k as f32) / 40.0) as u8;
        (bx, by, size, gray)
    }
    for i in 0..3u32 {
        let shift = 24 * i as i64;
        let image = image::RgbImage::from_fn(64, 48, |x, y| {
            let wx = x as i64 + shift;
            let wy = y as i64;
            let mut v = 128u8;
            for k in 0..40 {
                let (bx, by, size, gray) = blob(k);
                if wx >= bx && wx < bx + size && wy >= by && wy < by + size {
                    v = gray;
                    break;
                }
            }
            image::Rgb([v, v, v])
        });
        image
            .save(dir.join(format!("p{i:03}.png")))
            .expect("写全景测试帧");
    }
    dir
}

/// 全景拼接：命令只启动后台任务；三张接片拼成更宽的一张并落地。
#[test]
fn pano_merge_stitches_and_guards_input() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = pano_frame_dir("cmd");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    let need_confirm = state
        .run_ui_command(&ctx, "pano.merge", &json!({"frames": [0, 1, 2]}))
        .unwrap_err();
    assert!(need_confirm.contains("confirm"), "{need_confirm}");
    let too_few = state
        .run_ui_command(&ctx, "pano.merge", &json!({"frames": [0], "confirm": true}))
        .unwrap_err();
    assert!(too_few.contains("至少要 2 帧"), "{too_few}");
    let out_dir = dir.join("全景");
    let ok = state
        .run_ui_command(
            &ctx,
            "pano.merge",
            &json!({"frames": [0, 1, 2], "out_dir": out_dir.display().to_string(), "confirm": true}),
        )
        .expect("启动全景拼接");
    assert_eq!(ok["started"], json!(true));
    assert_eq!(ok["kind"], json!("pano"));
    assert_eq!(ok["frames"], json!(3));
    // 拼接期间同一类任务不许再起一次（面板按钮置灰之外的第二道门）。
    let busy = state
        .run_ui_command(
            &ctx,
            "pano.merge",
            &json!({"frames": [0, 1, 2], "confirm": true}),
        )
        .unwrap_err();
    assert!(busy.contains("全景拼接在进行中"), "{busy}");
    let hdr_busy = state
        .run_ui_command(
            &ctx,
            "hdr.merge",
            &json!({"frames": [0, 1], "confirm": true}),
        )
        .unwrap_err();
    assert!(hdr_busy.contains("全景拼接在进行中"), "{hdr_busy}");
    wait_merge(&mut state, &ctx);
    let done = state.merge_result.clone().expect("拼接结果落账");
    assert_eq!(done["kind"], json!("pano"));
    assert_eq!(done["height"], json!(48));
    let width = done["width"].as_u64().expect("宽度");
    assert!(width > 64, "拼接后应更宽：{width}");
    let path = done["path"].as_str().expect("成片路径");
    assert!(Path::new(path).is_file(), "全景成片没写出来：{path}");
    assert_eq!(state.pano_last.as_deref(), Some(path));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 同机位的三帧测试素材：底子一样，第二帧多一颗「星」，第三帧多一个「热噪点」。
///
/// Three fixed-camera test frames: same base, with one "star" added in the second and one
/// "hot pixel" in the third.
fn stack_frame_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("aevum-stack-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    for i in 0..3usize {
        let image = image::RgbImage::from_fn(32, 24, |x, y| {
            let mut v = 24u8;
            if i == 1 && (8..10).contains(&x) && (16..18).contains(&y) {
                v = 240; // 星点 / a star
            }
            if i == 2 && (20..22).contains(&x) && (16..18).contains(&y) {
                v = 200; // 热噪点 / a hot pixel
            }
            image::Rgb([v, v, v])
        });
        image
            .save(dir.join(format!("s{i:03}.png")))
            .expect("写堆栈测试帧");
    }
    dir
}

/// 星空堆栈：命令只启动后台任务；成片落地，且亮化真的把星点留下来了。
///
/// Star stacking: the command only starts the job; the still lands on disk, and lighten
/// really keeps the star.
#[test]
fn stack_merge_writes_one_still_and_guards_input() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = stack_frame_dir("cmd");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    let need_confirm = state
        .run_ui_command(&ctx, "stack.merge", &json!({"frames": [0, 1, 2]}))
        .unwrap_err();
    assert!(need_confirm.contains("confirm"), "{need_confirm}");
    let too_few = state
        .run_ui_command(
            &ctx,
            "stack.merge",
            &json!({"frames": [0], "confirm": true}),
        )
        .unwrap_err();
    assert!(too_few.contains("至少要 2 帧"), "{too_few}");
    let bad_mode = state
        .run_ui_command(
            &ctx,
            "stack.merge",
            &json!({"frames": [0, 1], "mode": "median-ish", "confirm": true}),
        )
        .unwrap_err();
    assert!(bad_mode.contains("未知堆栈模式"), "{bad_mode}");
    let bad_sigma = state
        .run_ui_command(
            &ctx,
            "stack.merge",
            &json!({"frames": [0, 1], "mode": "sigma-clip", "clip_sigma": 0.0, "confirm": true}),
        )
        .unwrap_err();
    assert!(bad_sigma.contains("clip_sigma"), "{bad_sigma}");
    let out_dir = dir.join("堆栈");
    let ok = state
        .run_ui_command(
            &ctx,
            "stack.merge",
            &json!({
                "frames": [0, 1, 2],
                "mode": "lighten",
                "out_dir": out_dir.display().to_string(),
                "confirm": true
            }),
        )
        .expect("启动堆栈");
    assert_eq!(ok["started"], json!(true));
    assert_eq!(ok["kind"], json!("stack"));
    assert_eq!(ok["frames"], json!(3));
    // 同一时刻只许跑一项：另一条命令也得被挡。
    let busy = state
        .run_ui_command(
            &ctx,
            "timeblend.merge",
            &json!({"frames": [0, 1], "confirm": true}),
        )
        .unwrap_err();
    assert!(busy.contains("星空堆栈在进行中"), "{busy}");
    wait_merge(&mut state, &ctx);
    let done = state.merge_result.clone().expect("堆栈结果落账");
    assert_eq!(done["kind"], json!("stack"));
    assert_eq!(done["width"], json!(32));
    assert_eq!(done["height"], json!(24));
    let path = done["path"].as_str().expect("成片路径");
    let still = image::open(path).expect("读回成片").to_rgb8();
    // 亮化：第二帧的星点必须留在原位（否则就是模式没被吃进去）。
    let star = still.get_pixel(8, 16)[0];
    assert!(star > 200, "星点没留下：{star}");
    assert_eq!(state.stack_last.as_deref(), Some(path));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 时间合成：天地合成把末帧放到天上、首帧放地上，并写出成片。
///
/// Time blending: sky-ground puts the last frame in the sky and the first on the ground,
/// and writes the still.
#[test]
fn timeblend_merge_splits_sky_and_ground() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = stack_frame_dir("tb");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    let need_confirm = state
        .run_ui_command(&ctx, "timeblend.merge", &json!({"frames": [0, 1]}))
        .unwrap_err();
    assert!(need_confirm.contains("confirm"), "{need_confirm}");
    let bad_bands = state
        .run_ui_command(
            &ctx,
            "timeblend.merge",
            // bands 只对时间切片校验，所以这条要显式带上 layout。
            //
            // bands is only validated for the time slice, so pass the layout explicitly.
            &json!({"frames": [0, 1], "layout": "bands", "bands": 9, "confirm": true}),
        )
        .unwrap_err();
    assert!(bad_bands.contains("bands 要在"), "{bad_bands}");
    let bad_layout = state
        .run_ui_command(
            &ctx,
            "timeblend.merge",
            &json!({"frames": [0, 1], "layout": "diagonal", "confirm": true}),
        )
        .unwrap_err();
    assert!(bad_layout.contains("未知布局"), "{bad_layout}");
    let out_dir = dir.join("时间合成");
    let ok = state
        .run_ui_command(
            &ctx,
            "timeblend.merge",
            &json!({
                "frames": [0, 1],
                "layout": "bands",
                "bands": 2,
                "out_dir": out_dir.display().to_string(),
                "confirm": true
            }),
        )
        .expect("启动时间合成");
    assert_eq!(ok["kind"], json!("timeblend"));
    wait_merge(&mut state, &ctx);
    let done = state.merge_result.clone().expect("合成结果落账");
    assert_eq!(done["kind"], json!("timeblend"));
    let path = done["path"].as_str().expect("成片路径");
    let still = image::open(path).expect("读回成片").to_rgb8();
    // 上一带取自首帧（没有星点），下一带取自末帧（有热噪点）。
    let top = still.get_pixel(8, 2)[0];
    let bottom = still.get_pixel(8, 16)[0];
    assert!(top < 60, "上半幅应来自首帧：{top}");
    assert!(bottom > 150, "下半幅应来自末帧：{bottom}");
    assert_eq!(state.tb_last.as_deref(), Some(path));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 预览：同参数不重发；改 bands 后自动重算，结构与最终成片一致（上带首帧暗、下带末帧亮）。
///
/// Preview: identical parameters never re-dispatch; changing bands re-runs it, and the
/// structure matches the final still (dark first-frame band on top, bright last-frame
/// band below).
#[test]
fn timeblend_preview_refreshes_with_parameters_and_matches_the_final_still() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = stack_frame_dir("tbpv");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    state.tb_layout = aevum_core::timeblend::Layout::Bands;
    state.tb_bands = 2;
    state.tb_around = 1;
    state.timeblend_preview_tick(&ctx);
    assert!(state.tb_preview_inflight, "首趟应已派发预览任务");
    wait_preview(&mut state, &ctx);
    let (key0, tex0) = state.tb_preview.clone().expect("首趟预览上屏");
    assert_eq!(tex0.size(), [16, 12], "预览应是成片的一半（32×24 → 16×12）");
    let img0 = state.tb_preview_img.clone().expect("预览像素应随纹理落账");
    let lum = |img: &ColorImage, x: usize, y: usize| img.pixels[y * 16 + x].r();
    let top = lum(&img0, 8, 2);
    // 星点在半分辨率的 (4,8)：末帧（帧 1）在整幅 (8..10,16..18) 的亮块，折半正好一格。
    let bottom = lum(&img0, 4, 8);
    assert!(top < 80, "上带应来自首帧（暗）：{top}");
    assert!(bottom > 140, "下带应带着末帧的星点（亮）：{bottom}");
    // 负向：同参数再 tick 不得重发（在飞标记不置位、指纹不变）。
    state.timeblend_preview_tick(&ctx);
    assert!(!state.tb_preview_inflight, "同参数不该重发预览");
    // 改 bands → 指纹变化 → 自动重算：bands=1 时全图来自首帧，下带也该暗。
    state.tb_bands = 1;
    state.timeblend_preview_tick(&ctx);
    assert!(state.tb_preview_inflight, "改了 bands 应重发预览");
    wait_preview(&mut state, &ctx);
    let key1 = state.tb_preview_key;
    assert_ne!(key0, key1, "参数指纹应变化");
    let img1 = state.tb_preview_img.clone().expect("第二趟预览像素落账");
    let bottom1 = lum(&img1, 4, 8);
    assert!(
        bottom1 < 80,
        "bands=1 时全图来自首帧，星点位置也该暗：{bottom1}"
    );
    // 与最终成片同构图：同一参数跑一次真合成，成片折半后的明暗结构与预览一致。
    let out_dir = dir.join("预览对照");
    let ok = state
        .run_ui_command(
            &ctx,
            "timeblend.merge",
            &json!({
                "frames": [0, 1],
                "layout": "bands",
                "bands": 2,
                "out_dir": out_dir.display().to_string(),
                "confirm": true
            }),
        )
        .expect("启动对照合成");
    assert_eq!(ok["kind"], json!("timeblend"));
    wait_merge(&mut state, &ctx);
    let path = state.merge_result.as_ref().expect("成片落账")["path"]
        .as_str()
        .expect("成片路径")
        .to_owned();
    let still = image::open(&path).expect("读回成片").to_rgb8();
    assert_eq!(still.dimensions(), (32, 24));
    // 折半（2×2 均值）后与预览逐点比对：γ 编码路径不同，留 32 的容差。
    for (py, sy) in [(2usize, 4usize), (8usize, 16usize)] {
        for px in [4usize, 8, 12] {
            let sx = px * 2;
            let mut sum = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    sum += still.get_pixel((sx + dx) as u32, (sy + dy) as u32)[0] as u32;
                }
            }
            let fin = (sum / 4) as i32;
            let pv = lum(&img0, px, py) as i32;
            assert!(
                (fin - pv).abs() <= 32,
                "预览 ({px},{py})={pv} 与成片折半 ({sx},{sy})={fin} 结构不一致"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 自动地平线：素材在 30% 处有天/地分界，开自动后分界真的搬过去；
/// 顺带验 deghost 越界要被拒。
///
/// Auto horizon: the scene carries a sky-ground boundary at 30%, and with auto on the
/// split really moves there; also checks that an out-of-range deghost is refused.
#[test]
fn timeblend_auto_horizon_moves_the_split_and_deghost_is_guarded() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("aevum-tbauto-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // 40×40，行 0..12 是天（30%）：早晚帧的天与地亮度都不同——
    // 否则 30%–50% 之间的行读不出选了哪帧。
    //
    // 40×40 with rows 0..12 as sky (30%); sky and ground both differ between the frames,
    // otherwise the rows between 30% and 50% cannot tell which frame was picked.
    let shade = |i: usize, y: u32| -> u8 {
        match (i, y < 12) {
            (0, true) => 80,  // 早帧的天 / early sky
            (0, false) => 40, // 早帧的地 / early ground
            (_, true) => 220, // 末帧的天 / late sky
            (_, false) => 90, // 末帧的地 / late ground
        }
    };
    for i in 0..3usize {
        let img = image::RgbImage::from_fn(40, 40, |_, y| {
            let v = shade(i, y);
            image::Rgb([v, v, v])
        });
        img.save(dir.join(format!("s{i:03}.png"))).expect("写帧");
    }
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    // 两趟各用各的输出目录：同一目录同名文件会把上一趟的成片覆盖掉。
    //
    // One output folder per run: the same folder and filename would overwrite the previous
    // still.
    let args = |extra: &Value| {
        let out_dir = if extra.get("horizon_auto").and_then(Value::as_bool) == Some(true) {
            dir.join("时间合成-自动")
        } else {
            dir.join("时间合成-手动")
        };
        let mut a = json!({
            "frames": [0, 2],
            "layout": "sky-ground",
            "feather": 0.05,
            "match_exposure": false,
            "out_dir": out_dir.display().to_string(),
            "confirm": true,
        });
        if let Some(obj) = a.as_object_mut()
            && let Some(map) = extra.as_object()
        {
            for (k, v) in map {
                obj.insert(k.clone(), v.clone());
            }
        }
        a
    };
    let bad_deghost = state
        .run_ui_command(&ctx, "timeblend.merge", &args(&json!({"deghost": 5.0})))
        .unwrap_err();
    assert!(bad_deghost.contains("deghost"), "{bad_deghost}");

    let run = |state: &mut AppState, extra: &Value| {
        state
            .run_ui_command(&ctx2(&ctx), "timeblend.merge", &args(extra))
            .expect("启动合成");
        wait_merge(state, &ctx2(&ctx));
        state.merge_result.clone().expect("合成结果落账")["path"]
            .as_str()
            .expect("成片路径")
            .to_owned()
    };
    // ctx 只能借一次，包一层省得抄两遍。
    //
    // The context borrows once; wrap it so the two runs do not duplicate the dance.
    fn ctx2(ctx: &Context) -> Context {
        ctx.clone()
    }
    let auto_path = run(&mut state, &json!({"horizon_auto": true, "deghost": 0.0}));
    let manual_path = run(&mut state, &json!({"horizon_auto": false, "deghost": 0.0}));
    let auto = image::open(&auto_path).expect("读回自动成片").to_rgb8();
    let manual = image::open(&manual_path).expect("读回手动成片").to_rgb8();
    // 自动：分界在 30% → 行 6 是末帧的亮天，行 19（30%–50% 之间）是首帧的暗地。
    //
    // Auto: the boundary sits at 30% → row 6 is the late frame's bright sky, row 19
    // (between 30% and 50%) is the early frame's dark ground.
    let at = |img: &image::RgbImage, y: u32| img.get_pixel(8, y)[0];
    assert!(at(&auto, 6) > 150, "行 6 应是末帧的亮天：{}", at(&auto, 6));
    assert!(
        at(&auto, 19) < 60,
        "行 19 应是首帧的暗地：{}",
        at(&auto, 19)
    );
    // 反向验证：停在默认 50% 时，行 19 按天混合，明显亮于首帧的暗地。
    //
    // Negative control: stuck at the default 50%, row 19 blends sky and is clearly
    // brighter than the early frame's dark ground.
    assert!(
        at(&manual, 19) > at(&auto, 19) + 15,
        "停在 50% 时行 19 应更亮：auto {} vs manual {}",
        at(&auto, 19),
        at(&manual, 19)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 焦点合成端到端：三帧各管一条横带，成片每带取到持有清晰纹理的那帧；
/// 兜底开到 100 分位后整幅退回参考帧（负向验证）；未知方法与缺 confirm 都在起任务前被拒。
///
/// Focus stacking end to end: three frames owning one band each, the still takes every
/// band from the frame holding its texture; a 100th-percentile fallback sends the whole
/// image back to the reference frame (negative check); an unknown method and a missing
/// confirm are both refused before a job starts.
#[test]
fn focus_merge_takes_each_band_from_the_frame_that_holds_the_texture() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("aevum-focus-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // 48×48，三条 16 行横带：带 i 只有第 i 帧带纹理，其余帧是 128 平场。
    // 竖条纹周期 4（两列 200 / 两列 60），横条纹同参数；第三条带整体错开 2 px，
    // 这样它不会被误判成第一条带的帧。
    //
    // 48x48, three 16-row bands: only frame i carries texture in band i, the others are a
    // flat 128 there. Vertical straps have a 4 px period (two columns 200 / two 60) and the
    // horizontal ones match; band three is shifted by 2 px so it cannot pass for frame 0.
    let vpos = |x: u32| if (x / 2).is_multiple_of(2) { 200u8 } else { 60 };
    let hpos = |y: u32| if (y / 2).is_multiple_of(2) { 200u8 } else { 60 };
    for i in 0..3u32 {
        let img = image::RgbImage::from_fn(48, 48, |x, y| {
            let band = y / 16;
            let v = if band == i {
                if i == 1 {
                    hpos(y)
                } else if i == 0 {
                    vpos(x)
                } else {
                    vpos(x + 2)
                }
            } else {
                128
            };
            image::Rgb([v, v, v])
        });
        img.save(dir.join(format!("f{i:03}.png"))).expect("写帧");
    }
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    // 参数错与缺 confirm 先拦：错误该在起任务之前出来。
    //
    // A bad parameter and a missing confirm are stopped first: the error must surface
    // before any job starts.
    // 坏值不能用 "pyramid"：1.5 起它是合法方法名，会让这条「未知 key 必须报错」的语义失效。
    //
    // The bad value cannot be "pyramid": from 1.5 it is a legal method key, which
    // would invalidate this "unknown key must error" assertion.
    let bad = state
        .run_ui_command(
            &ctx,
            "focus.merge",
            &json!({"method": "laplacian", "confirm": true}),
        )
        .unwrap_err();
    assert!(bad.contains("depthmap"), "{bad}");
    let no_confirm = state
        .run_ui_command(&ctx, "focus.merge", &json!({"frames": [0, 1, 2]}))
        .unwrap_err();
    assert!(no_confirm.contains("confirm"), "{no_confirm}");
    // 两趟各用各的输出目录：同目录同名会把上一趟的成片覆盖掉。
    //
    // One output folder per run: the same folder and filename would overwrite the
    // previous still.
    let run = |state: &mut AppState, extra: &Value, sub: &str, aligned: bool| {
        let mut a = json!({
            "frames": [0, 1, 2],
            "out_dir": dir.join(sub).display().to_string(),
            "confirm": true,
        });
        if let Some(obj) = a.as_object_mut()
            && let Some(map) = extra.as_object()
        {
            for (k, v) in map {
                obj.insert(k.clone(), v.clone());
            }
        }
        let started = state
            .run_ui_command(&ctx2(&ctx), "focus.merge", &a)
            .expect("启动焦点合成");
        assert_eq!(started["kind"], json!("focus"), "任务种类应是 focus");
        // 这次套没套对齐位移，返回里就得说清楚，别让人去翻对齐报告。
        //
        // Whether the alignment offsets were applied belongs in the reply; nobody
        // should have to dig through the align report for it.
        assert_eq!(
            started["aligned"],
            json!(aligned),
            "返回里的 aligned 与序列的对齐状态不符"
        );
        wait_merge(state, &ctx2(&ctx));
        state.merge_result.clone().expect("合成结果落账")["path"]
            .as_str()
            .expect("成片路径")
            .to_owned()
    };
    fn ctx2(ctx: &Context) -> Context {
        ctx.clone()
    }
    let sharp = run(&mut state, &json!({"threshold": 0}), "焦点合成-默认", false);
    let img = image::open(&sharp).expect("读回成片").to_rgb8();
    let at = |x: u32, y: u32| img.get_pixel(x, y)[0];
    // 第一条带：x=0 亮纹 200、x=2 暗纹 60——只有帧 0 在这里清晰。
    assert_eq!(at(0, 8), 200, "第一条带应取帧 0 的亮纹");
    assert_eq!(at(2, 8), 60, "第一条带应取帧 0 的暗纹");
    // 第二条带：y=24 亮纹、y=26 暗纹——只有帧 1 在这里清晰。
    assert_eq!(at(8, 24), 200, "第二条带应取帧 1 的亮纹");
    assert_eq!(at(8, 26), 60, "第二条带应取帧 1 的暗纹");
    // 第三条带：错开 2 px 的竖条纹，x=0 暗、x=2 亮——取的是帧 2，不是帧 0。
    assert_eq!(at(0, 40), 60, "第三条带应取帧 2（错位条纹）的暗纹");
    assert_eq!(at(2, 40), 200, "第三条带应取帧 2 的亮纹");
    // 负向验证：兜底开到 100 分位＝全图都算无细节 → 整幅退回参考帧（帧 1），
    // 第一条带于是变成 128 平场。
    //
    // Negative check: a 100th-percentile fallback treats everything as featureless, so
    // the whole image returns to the reference frame (frame 1) and band one goes flat 128.
    let fallback = run(
        &mut state,
        &json!({"threshold": 100}),
        "焦点合成-兜底",
        false,
    );
    let img2 = image::open(&fallback).expect("读回兜底成片").to_rgb8();
    let v = img2.get_pixel(0, 8)[0];
    assert!(
        (v as i32 - 128).abs() <= 2,
        "兜底后第一条带应是参考帧的平场 128，实际 {v}"
    );
    // 对齐过之后再跑一趟：返回里的 aligned 要翻成 true，成片照样出。
    //
    // Once the sequence is aligned the reply must flip to true, and the still still
    // gets written.
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
    let aligned_path = run(
        &mut state,
        &json!({"threshold": 0}),
        "焦点合成-已对齐",
        true,
    );
    assert!(
        std::path::Path::new(&aligned_path).is_file(),
        "已对齐时也该出片：{aligned_path}"
    );
    state.align_report = None;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 剔除成子序列：dry_run 只报数不写盘，confirm 复制保留帧；目标在源内部被拒。
///
/// Extraction: a dry run only reports without writing, confirm copies the kept frames, and
/// a target inside the source is refused.
#[test]
fn cull_extract_reports_on_dry_run_and_copies_on_confirm() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = stack_frame_dir("extract");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    // 中间那帧标成排除：保留帧应为 2。
    //
    // Mark the middle frame excluded: two frames should be kept.
    state.flag_many(&[1], -1);
    // 源序列放一份测光缓存，导出后必须原样出现在目标（不用重测）。
    //
    // Leave a metering cache in the source; it must reappear unchanged in the target (no
    // re-measure).
    std::fs::create_dir_all(dir.join(".aevum")).expect("建护带");
    std::fs::write(dir.join(".aevum/measures.json"), b"{\"version\":1}").expect("写缓存");
    let inside = dir.join("子序列");
    let need_target = state
        .run_ui_command(&ctx, "cull.extract", &json!({}))
        .unwrap_err();
    assert!(need_target.contains("target"), "{need_target}");
    let refused = state
        .run_ui_command(
            &ctx,
            "cull.extract",
            &json!({"target": inside.display().to_string(), "dry_run": true}),
        )
        .unwrap_err();
    assert!(refused.contains("源序列根"), "{refused}");
    // dry_run：只报数，目标目录不许被创建。
    //
    // Dry run: report only, and the target folder must not be created.
    let target = dir
        .parent()
        .unwrap()
        .join(format!("aevum-extract-out-{}", std::process::id()));
    let dry = state
        .run_ui_command(
            &ctx,
            "cull.extract",
            &json!({"target": target.display().to_string(), "dry_run": true}),
        )
        .expect("预估");
    assert_eq!(dry["dry_run"], json!(true));
    assert_eq!(dry["frames"], json!(2));
    assert_eq!(dry["excluded"], json!(1));
    assert!(dry["bytes"].as_u64().expect("体积") > 0);
    assert!(!target.exists(), "dry_run 不许写盘");
    // 没 confirm 的真复制被拒。
    //
    // A real copy without confirm is refused.
    let no_confirm = state
        .run_ui_command(
            &ctx,
            "cull.extract",
            &json!({"target": target.display().to_string()}),
        )
        .unwrap_err();
    assert!(no_confirm.contains("confirm"), "{no_confirm}");
    let ok = state
        .run_ui_command(
            &ctx,
            "cull.extract",
            &json!({"target": target.display().to_string(), "confirm": true}),
        )
        .expect("导出");
    assert_eq!(ok["copied"], json!(2));
    let got: Vec<_> = std::fs::read_dir(&target)
        .expect("列目标")
        .map(|e| e.expect("条目").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(got.len(), 3, "两帧 + .aevum：{got:?}");
    assert!(
        std::fs::read(dir.join("s001.png")).is_ok(),
        "被排除的帧也要留在源序列里（源只读）"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&target);
}

/// 单测没有界面帧，等后台合并收尾只能自己把消息队列抽干。
/// 轮询等预览回包：与 wait_merge 同款，监视的是预览在飞标记。
fn wait_preview(state: &mut AppState, ctx: &Context) {
    for _ in 0..2000 {
        state.poll(ctx);
        if !state.tb_preview_inflight {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("预览任务没收尾");
}

fn wait_merge(state: &mut AppState, ctx: &Context) {
    for _ in 0..2000 {
        state.poll(ctx);
        if !state.merge_busy {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("合并任务没收尾");
}

/// 筛选台：只报数不动标记；apply 才写旗标，而且一个素材都不碰。
///
/// The cull station: report only by default; `apply` is what writes flags, and it never
/// touches a single media file.
#[test]
fn cull_evaluate_reports_then_marks_on_request() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = stack_frame_dir("cull");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    // 测光换成手写数据：中间那帧暗了 3 EV，其余两帧在 0 EV。
    let metrics = |mean: f64| FrameMetrics {
        mean,
        median: mean,
        center: mean,
        sharp: 100.0,
    };
    state.metrics = vec![
        Some(metrics(0.18)),
        Some(metrics(0.0225)),
        Some(metrics(0.18)),
    ];
    let before: Vec<Vec<u8>> = (0..3)
        .map(|i| std::fs::read(dir.join(format!("s{i:03}.png"))).expect("读原帧"))
        .collect();

    let dry = state
        .run_ui_command(&ctx, "cull.evaluate", &json!({}))
        .expect("评估");
    assert_eq!(dry["rejected"], json!(1), "{dry}");
    assert_eq!(dry["applied"], json!(0));
    assert_eq!(dry["indices"], json!([1]));
    assert_eq!(dry["frames"][1]["reason"], json!("too-dark"));
    assert_eq!(state.flag_at(1), 0, "只评估不该动旗标");

    // 把区间放到 ±4 EV：那条 −3 EV 的帧就在带内了，一帧都不该判出去
    // （上一遍判出 1 帧，这一遍 0 帧，才说明阈值真的被吃进去）。
    let wide = state
        .run_ui_command(
            &ctx,
            "cull.evaluate",
            &json!({"ev_low": -4.0, "ev_high": 4.0}),
        )
        .expect("宽区间评估");
    assert_eq!(wide["rejected"], json!(0), "{wide}");

    let applied = state
        .run_ui_command(
            &ctx,
            "cull.evaluate",
            &json!({"apply": true, "ev_low": -1.0, "ev_high": 1.0}),
        )
        .expect("评估并标废");
    assert_eq!(applied["applied"], json!(1));
    assert_eq!(state.flag_at(1), -1, "判废的帧要落成排除");
    assert_eq!(state.flag_at(0), 0, "干净的帧不该被标");
    assert!(state.cull_note.is_some(), "结论要留在面板上");

    // 下限高于上限：直接报错，不许静默换成一个空区间。
    let reversed = state
        .run_ui_command(
            &ctx,
            "cull.evaluate",
            &json!({"ev_low": 1.0, "ev_high": -1.0}),
        )
        .unwrap_err();
    assert!(reversed.contains("不该高于上限"), "{reversed}");

    // 素材一个字节都不该变：这条命令只读测光表。
    for (i, bytes) in before.iter().enumerate() {
        let now = std::fs::read(dir.join(format!("s{i:03}.png"))).expect("再读原帧");
        assert_eq!(&now, bytes, "第 {i} 帧被改写了");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 叠印文字：没有 EXIF 时只出帧号；日期按人读得顺的写法换。
///
/// The overlay line: frame number only without EXIF, and dates rewritten the readable way.
#[test]
fn stamp_line_degrades_and_pretty_prints() {
    assert_eq!(stamp_line(2, 12, None), "#0003/12");
    let bare = Exif::default();
    assert_eq!(stamp_line(0, 1, Some(&bare)), "#0001/1");
    assert_eq!(
        pretty_datetime("2026:11:02 18:24:07"),
        "2026-11-02 18:24:07"
    );
    assert_eq!(pretty_datetime("garbage"), "garbage");
    assert_eq!(pretty_datetime(""), "");
}

/// 没跑任务时 merge.status / merge.cancel 都要答得清楚，不能报错。
#[test]
fn merge_status_reports_idle_without_a_task() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let status = state
        .run_ui_command(&ctx, "merge.status", &json!({}))
        .expect("查询合并状态");
    assert_eq!(status["busy"], json!(false));
    assert_eq!(status["kind"], Value::Null);
    assert_eq!(status["result"], Value::Null);
    assert_eq!(status["error"], Value::Null);
    let cancelled = state
        .run_ui_command(&ctx, "merge.cancel", &json!({}))
        .expect("取消合并");
    assert_eq!(cancelled["cancelling"], json!(false));
    assert!(
        cancelled["note"]
            .as_str()
            .unwrap_or_default()
            .contains("没有进行中"),
        "{cancelled}"
    );
}

/// 取消旗只在真跑着任务时立；`merge_busy` 只由 `poll` 清零，所以后台线程
/// 抢先跑完也不会让这里的断言变绿——忙碌期的拒绝入口另有测试覆盖。
#[test]
fn merge_cancel_only_sets_the_flag_while_running() {
    let mut state = AppState::new();
    state.stop_merge();
    assert!(
        !state.cancel_merge.load(Ordering::SeqCst),
        "空闲时不该转取消旗"
    );
    state.merge_busy = true;
    state.merge_kind = Some(MergeKind::Hdr);
    state.stop_merge();
    assert!(
        state.cancel_merge.load(Ordering::SeqCst),
        "忙碌时应转取消旗"
    );
    assert!(state.status.contains("正在取消"), "{}", state.status);
}

/// 预览失败只解预览在飞标记，不许碰导出的独占锁：否则一次解码失败就能让用户
/// 在导出仍在写盘时起第二路导出，两份线程写同一个文件。
#[test]
fn preview_failure_keeps_the_export_lock() {
    let ctx = Context::default();
    let mut state = AppState::new();
    state.export_busy = true;
    state.preview_inflight = true;
    state
        .tx
        .send(Msg::PreviewFailed("预览失败：坏帧".to_owned()))
        .expect("投递预览失败");
    state.poll(&ctx);
    assert!(state.export_busy, "预览失败不该解导出锁");
    assert!(!state.preview_inflight, "预览在飞标记要落回来");
    assert!(
        state.error.as_deref().unwrap_or_default().contains("坏帧"),
        "失败原因要进错误条：{:?}",
        state.error
    );
}

/// 合并回包按 job 对号：旧任务（已换序列）的迟到回包不能清掉当前任务的忙碌标记；
/// 当前任务的回包即使 token 已过期，也要负责清标记，只是不落账。
#[test]
fn stale_merge_reply_cannot_clear_the_current_run() {
    let ctx = Context::default();
    let mut state = AppState::new();
    state.merge_busy = true;
    state.merge_kind = Some(MergeKind::Hdr);
    state.merge_job = 7;
    state.reload_token = 3;
    state
        .tx
        .send(Msg::Merged {
            token: 3,
            job: 6,
            kind: MergeKind::Hdr,
            result: Err("旧任务".to_owned()),
        })
        .expect("投递旧回包");
    state.poll(&ctx);
    assert!(state.merge_busy, "旧 job 的回包不该清当前任务的忙碌标记");
    assert_eq!(state.merge_kind, Some(MergeKind::Hdr));
    assert!(state.merge_error.is_none(), "旧 job 的失败也不该落账");

    state
        .tx
        .send(Msg::Merged {
            token: 2,
            job: 7,
            kind: MergeKind::Hdr,
            result: Ok(json!({"path": "旧序列成片.avif"})),
        })
        .expect("投递当前任务回包");
    state.poll(&ctx);
    assert!(!state.merge_busy, "当前任务的回包要负责清忙碌标记");
    assert!(
        state.merge_result.is_none(),
        "换过序列的成片路径不该落到新序列的账上"
    );
}

/// 换序列只转取消旗，不抢清 `merge_busy`：清早了用户就能再起一个合并，而旧线程
/// 还在往上一份成片里写。
#[test]
fn sequence_switch_cancels_but_does_not_release_merge_busy() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = super::history_tests::two_frame_dir("merge-switch");
    state.merge_busy = true;
    state.merge_kind = Some(MergeKind::Hdr);
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    assert!(state.merge_busy, "忙碌标记要等旧 job 自己的回包清");
    assert!(
        state.cancel_merge.load(Ordering::SeqCst),
        "换序列要转取消旗"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 体检用的序列：编号 001…026，缺 010；005 全黑、014 糊成一团、020 与 021 逐字节相同。
///
/// 每帧都是同一张 8×6 色盘的换序——48 格各用一次，所以**均值一分不动**，只有纹理
/// 在变。这样体检报出来的离群只可能是真坏帧，而不是我造数据的手法。
fn qa_frame_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("aevum-qa-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let palette: Vec<u8> = (0..48u32).map(|k| (24 + k * 168 / 47) as u8).collect();
    let grid = |shift: usize| -> image::RgbImage {
        image::RgbImage::from_fn(64, 48, |x, y| {
            let cell = (y as usize / 8) * 8 + x as usize / 8;
            let v = palette[(cell + shift) % 48];
            image::Rgb([v, v, v])
        })
    };
    for num in 1..=26usize {
        if num == 10 {
            continue;
        }
        let img = match num {
            5 => image::RgbImage::from_pixel(64, 48, image::Rgb([0, 0, 0])),
            14 => image::imageops::blur(&grid(num), 3.0),
            21 => grid(20),
            _ => grid(num),
        };
        img.save(dir.join(format!("frame_{num:03}.png")))
            .expect("写体检测试帧");
    }
    dir
}

/// 等后台测光收尾——没测完就跑体检，每一帧都会被判成「读不出来」。
fn wait_measure(state: &mut AppState, ctx: &Context) {
    for _ in 0..4000 {
        state.poll(ctx);
        if state.measuring.is_none() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("测光没收尾");
}

/// 打开序列后的「正在测量 N 帧…」必须站得住：状态栏不能在这一趟里被一句看起来
/// 像汇总的文案提前顶掉；`measuring` 要带着帧数，直到 `Msg::Loaded` 落账才清。
///
/// Opening a sequence must keep "measuring N frames…" standing: this pass must not
/// overwrite it with copy that reads like a finished summary, and `measuring` carries the
/// frame count until `Msg::Loaded` lands.
#[test]
fn opening_a_sequence_keeps_the_measuring_status_until_loaded() {
    let dir = temp_dir("measure-status");
    for i in 0..3u8 {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([i * 10, 20, 30]));
        img.save(dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    let ctx = Context::default();
    let mut state = AppState::new();
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    assert_eq!(
        state.measuring,
        Some(3),
        "测光期间要记住在测多少帧，而不是一个裸 bool"
    );
    assert!(
        state.status.contains("正在测量") && state.status.contains('3'),
        "状态栏在测光完成前就被改写了：{}",
        state.status
    );
    wait_measure(&mut state, &ctx);
    assert!(state.measuring.is_none(), "测光结束后要清掉");
    assert!(
        state.status.contains("可读"),
        "测光完成后的汇总没落账：{}",
        state.status
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 「约剩」要有两个有跨度的采样点才出现；done=0 或已到终点都不显示，换任务重新开表。
///
/// The remaining-time estimate appears only after two samples with a real time span; it
/// stays hidden at done=0 or at completion, and a task switch starts a fresh window.
#[test]
fn eta_needs_two_samples_and_resets_between_tasks() {
    let mut state = AppState::new();
    assert_eq!(
        state.eta_secs("导出", 0, 10),
        None,
        "done=0 不该显示剩余时间"
    );
    assert_eq!(state.eta_secs("导出", 3, 3), None, "已到终点不该显示");
    assert_eq!(
        state.eta_secs("导出", 1, 10),
        None,
        "只有一个采样点算不出速率"
    );
    std::thread::sleep(Duration::from_millis(30));
    let eta = state.eta_secs("导出", 2, 10).expect("两个采样点应给出估计");
    assert!(eta > 0, "剩余秒数为 0：{eta}");
    assert_eq!(
        state.eta_secs("合并", 3, 10),
        None,
        "换了任务要重新开表，不能借上一轮的速率"
    );
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        state.eta_secs("合并", 4, 10).is_some(),
        "新任务第二点之后应重新有估计"
    );
}

fn wait_qa(state: &mut AppState, ctx: &Context) {
    for _ in 0..4000 {
        state.poll(ctx);
        if !state.qa_busy {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("体检没收尾");
}

/// 体检：命令只启动后台任务，清单由 `poll` 落账；四类问题各就各位、坏输入先挡住。
#[test]
fn sequence_check_runs_in_background_and_lists_findings() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = qa_frame_dir("check");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    // 门槛越界、没有序列这两种输入都不该起线程。
    let bad_sig = state
        .run_ui_command(&ctx, "sequence.check", &json!({"sig": 0.1}))
        .unwrap_err();
    assert!(bad_sig.contains("0.5~20"), "{bad_sig}");
    assert!(!state.qa_busy, "校验失败不该起任务");
    wait_measure(&mut state, &ctx);
    assert_eq!(state.frame_count(), 25, "缺 010 应该是 25 帧");

    let idle = state
        .run_ui_command(&ctx, "sequence.status", &json!({}))
        .expect("空闲状态");
    assert_eq!(idle["busy"], json!(false));
    assert_eq!(idle["ran"], json!(false));
    assert!(idle["findings"].as_array().unwrap().is_empty());

    let ok = state
        .run_ui_command(&ctx, "sequence.check", &json!({"sig": 3.0}))
        .expect("启动体检");
    assert_eq!(ok["started"], json!(true));
    assert_eq!(ok["frames"], json!(25));
    assert_eq!(ok["sig"], json!(3.0));
    assert!(state.qa_busy, "启动后应标记忙碌");
    let again = state
        .run_ui_command(&ctx, "sequence.check", &json!({}))
        .unwrap_err();
    assert!(again.contains("体检正在跑"), "{again}");
    wait_qa(&mut state, &ctx);

    let status = state
        .run_ui_command(&ctx, "sequence.status", &json!({}))
        .expect("体检状态");
    assert_eq!(status["busy"], json!(false));
    assert_eq!(status["ran"], json!(true));
    assert_eq!(status["error"], Value::Null);
    // 对外帧号是 1 基下标：005 全黑→5、缺口后那帧 011→10、014 糊→13、重复报在后者 021→20。
    let got: Vec<(u64, String)> = status["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["frame"].as_u64().unwrap(),
                f["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(got.contains(&(5, "black".to_owned())), "{got:?}");
    assert!(got.contains(&(10, "gap".to_owned())), "{got:?}");
    assert!(got.contains(&(13, "blur".to_owned())), "{got:?}");
    assert!(got.contains(&(20, "dup".to_owned())), "{got:?}");
    assert!(
        !got.iter().any(|(_, k)| k == "read" || k == "luma_out"),
        "坏帧与亮度离群都不该有：{got:?}"
    );
    // 缺口没有对应文件，不能勾「排除这几帧」；其余都可排除。
    let gap = status["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == json!("gap"))
        .expect("缺口条目");
    assert_eq!(gap["excludable"], json!(false));
    assert_eq!(gap["label"], json!("序列缺口"));
    assert_eq!(status["counts"]["black"], json!(1));
    assert_eq!(status["counts"]["gap"], json!(1));
    assert_eq!(status["counts"]["dup"], json!(1));
    assert_eq!(status["counts"]["blur"], json!(1));

    // 阈值写进状态：面板下次直接沿用，`sig` 缺省时也读它。
    assert_eq!(state.qa_sig, 3.0);
    assert!(state.status.contains("体检完成"), "{}", state.status);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 松一档门槛应当只让清单变长，不会把上面那条判据反过来。
#[test]
fn looser_threshold_adds_findings_without_removing_them() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = qa_frame_dir("tight");
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.display().to_string()}),
        )
        .expect("打开序列");
    wait_measure(&mut state, &ctx);
    state
        .run_ui_command(&ctx, "sequence.check", &json!({"sig": 3.0}))
        .expect("严体检");
    wait_qa(&mut state, &ctx);
    let strict: Vec<(u64, String)> = state.qa_status_json()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["frame"].as_u64().unwrap(),
                f["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();

    state
        .run_ui_command(&ctx, "sequence.check", &json!({"sig": 0.5}))
        .expect("松体检");
    wait_qa(&mut state, &ctx);
    let loose: Vec<(u64, String)> = state.qa_status_json()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["frame"].as_u64().unwrap(),
                f["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    for item in &strict {
        assert!(
            loose.contains(item),
            "{item:?} 在严清单里，松清单里反而没了"
        );
    }
    assert!(loose.len() >= strict.len());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 取消旗只在真跑着任务时立；`qa_busy` 只由 `poll` 清零。
#[test]
fn qa_cancel_only_sets_the_flag_while_running() {
    let mut state = AppState::new();
    state.stop_qa();
    assert!(
        !state.cancel_qa.load(Ordering::SeqCst),
        "空闲时不该转取消旗"
    );
    state.qa_busy = true;
    state.stop_qa();
    assert!(state.cancel_qa.load(Ordering::SeqCst), "忙碌时应转取消旗");
    assert!(state.status.contains("正在取消"), "{}", state.status);
    assert_eq!(state.qa_progress(), Some((0, 0)));
}

/// 单帧导出的一句话入口：测试里只关心成片档旋钮，几何一律走默认。
fn export_still(src: &Path, dst: &Path, render: RenderOptions) {
    let params = GradeParams::default();
    render_full_file(FrameExport {
        src,
        dst,
        params: &params,
        gain: Ev::NONE,
        full_size: None,
        crop: None,
        framing: Framing::default(),
        render,
    })
    .expect("导出当前帧");
}

/// 面板那六个旋钮要一个不落地进单帧导出；漏掉哪个都是「设了没用」。
/// 几何与视频专用的字段保持默认——「导出当前帧」不许偷偷改画幅。
#[test]
fn frame_export_reads_every_output_knob() {
    let mut state = AppState::new();
    state.render_format = StillFormat::Jpeg;
    state.render_quality = 63;
    state.render_keep_exif = true;
    state.render_sharpen_for = OutputSharpen::Glossy;
    state.render_sharpen_strength = SharpenStrength::High;
    state.render_color_space = ColorSpace::Rec2020;
    state.render_watermark.enabled = true;
    state.render_watermark.text = "© 恒晷".to_owned();

    let o = state.frame_render_options();
    assert_eq!(o.mode, OutputMode::Stills);
    assert_eq!(o.format, StillFormat::Jpeg);
    assert_eq!(o.quality, 63);
    assert!(o.keep_exif);
    assert_eq!(o.sharpen_for, OutputSharpen::Glossy);
    assert_eq!(o.sharpen_strength, SharpenStrength::High);
    assert_eq!(o.color_space, ColorSpace::Rec2020);
    assert!(o.watermark.is_on(), "水印没跟进单帧导出");
    assert!(o.crop.is_none() && o.target_size.is_none());
    assert!(o.out_dir.as_os_str().is_empty() && o.video_out.as_os_str().is_empty());
}

/// 对话框里的后缀要跟面板选的容器一致：编码器认字节不认文件名，
/// 用户在「另存为」里手输 .png 也不能把 JPEG 说成 PNG。
#[test]
fn force_container_rewrites_the_extension() {
    let jpg = PathBuf::from("D:/x/帧_0001_graded.png");
    assert_eq!(
        force_container(jpg.clone(), StillFormat::Jpeg)
            .extension()
            .unwrap(),
        "jpg"
    );
    // 已经是目标容器就别再动，避免把 `.JPG` 这类大小写改写掉。
    assert_eq!(
        force_container(PathBuf::from("D:/x/a.JPG"), StillFormat::Jpeg).to_string_lossy(),
        "D:/x/a.JPG"
    );
    assert_eq!(
        force_container(PathBuf::from("D:/x/a"), StillFormat::Tiff16).to_string_lossy(),
        "D:/x/a.tif"
    );
}

/// 剪切遮罩只罩住真正裁切的像素：全白标红、全黑标蓝（两开时）、
/// 中间调与「只红通道满」的半裁切不算。
#[test]
fn clip_mask_marks_only_fully_clipped_pixels() {
    // 白 / 黑 / 灰 / 只 R 顶满
    let rgb = vec![255, 255, 255, 0, 0, 0, 128, 128, 128, 255, 200, 60];
    let img = clip_mask_image(&rgb, [4, 1], true, true).expect("有开关就有图");
    let px = |i: usize| {
        let p = img.pixels[i];
        [p.r(), p.g(), p.b(), p.a()]
    };
    assert_eq!(px(0)[3], 170, "纯白 → 高光裁切");
    // Color32 内部预乘 alpha：红 255 × 170/255 = 170，绿蓝远小于红。
    assert_eq!(px(0)[0], 170, "罩色预乘后仍是红主导");
    assert!(px(0)[1] < 100 && px(0)[2] < 100, "绿蓝通道远小于红");
    assert_eq!(px(1)[3], 170, "纯黑 → 阴影裁切");
    assert_eq!(px(1)[2], 170, "罩色预乘后蓝主导");
    assert!(px(1)[0] < 100 && px(1)[1] < 100, "红绿通道远小于蓝");
    assert_eq!(px(2)[3], 0, "中间调不罩");
    assert_eq!(px(3)[3], 0, "单通道顶满不算裁切");
    // 只开阴影：白的也不罩了。
    let img = clip_mask_image(&rgb, [4, 1], false, true).unwrap();
    assert_eq!(img.pixels[0].a(), 0);
    assert_eq!(img.pixels[1].a(), 170);
    // 像素不足时拒绝生成，不 panic。
    assert!(clip_mask_image(&rgb[..3], [4, 1], true, true).is_none());
}

/// J 键循环：关 → 高光 → 两开 → 阴影 → 关，五拍走完一圈。
#[test]
fn clip_cycle_walks_off_high_both_shadow_off() {
    let mut state = AppState::new();
    assert!(!state.clip_highlights && !state.clip_shadows);
    state.clip_cycle();
    assert!(state.clip_highlights && !state.clip_shadows, "第一拍：高光");
    state.clip_cycle();
    assert!(state.clip_highlights && state.clip_shadows, "第二拍：两开");
    state.clip_cycle();
    assert!(!state.clip_highlights && state.clip_shadows, "第三拍：阴影");
    state.clip_cycle();
    assert!(
        !state.clip_highlights && !state.clip_shadows,
        "第四拍：回关"
    );
}
