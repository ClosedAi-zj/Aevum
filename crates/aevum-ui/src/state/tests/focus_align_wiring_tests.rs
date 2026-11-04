use super::*;

/// 确定性像素哈希，返回 0..1（与 core 判据 `focus_align.rs` 同一口径）。
///
/// Deterministic per-pixel hash in 0..1 (same convention as the core assertions in
/// `focus_align.rs`).
fn hash01(x: u32, y: u32, seed: u64) -> f32 {
    let mut n = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ seed.wrapping_mul(0x1656_67B1_9E37_79F9);
    n ^= n >> 30;
    n = n.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    n ^= n >> 27;
    (((n >> 40) & 0xFFFF) as f32) / 65535.0
}

/// 白噪声底图按双线性位移采样后量化成 8bit（`dst = src(x - off)`）。
///
/// A white-noise base sampled at a bilinear offset and quantised to 8 bit
/// (`dst = src(x - off)`).
fn shifted_noise(w: u32, h: u32, seed: u64, off: [f64; 2]) -> image::RgbImage {
    image::RgbImage::from_fn(w, h, |x, y| {
        let sx = x as f64 - off[0];
        let sy = y as f64 - off[1];
        let fx = sx.floor();
        let fy = sy.floor();
        let tx = (sx - fx) as f32;
        let ty = (sy - fy) as f32;
        let x0 = (fx as i64).clamp(0, w as i64 - 1) as u32;
        let y0 = (fy as i64).clamp(0, h as i64 - 1) as u32;
        let xa = (x0 + 1).min(w - 1);
        let ya = (y0 + 1).min(h - 1);
        let p00 = hash01(x0, y0, seed);
        let p10 = hash01(xa, y0, seed);
        let p01 = hash01(x0, ya, seed);
        let p11 = hash01(xa, ya, seed);
        let top = p00 + (p10 - p00) * tx;
        let bottom = p01 + (p11 - p01) * tx;
        let v = (top + (bottom - top) * ty).clamp(0.0, 1.0);
        let c = (v * 255.0).round() as u8;
        image::Rgb([c, c, c])
    })
}

/// 写一段序列：`frames` 是每帧的双线性位移。
///
/// Write a sequence, one bilinear offset per frame.
fn write_sequence(root: &Path, w: u32, h: u32, seed: u64, frames: &[[f64; 2]]) {
    std::fs::create_dir_all(root).expect("建序列目录");
    for (i, off) in frames.iter().enumerate() {
        shifted_noise(w, h, seed, *off)
            .save(root.join(format!("f{i:03}.png")))
            .expect("写帧");
    }
}

fn temp_root(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("aevum-align-{tag}-{}-{nanos}", std::process::id()))
}

/// 等合并收尾（与 `command_tests` 里那份同构；那边是私有 helper，不跨模块借）。
///
/// Wait for the merge to finish (same shape as the private helper in `command_tests`,
/// which cannot be borrowed across modules).
fn wait_merge(state: &mut AppState, ctx: &Context) {
    for _ in 0..4000 {
        state.poll(ctx);
        if !state.merge_busy {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("合并任务没收尾");
}

/// 跑一次 `focus.merge`，返回（启动回执, 结果 JSON, 成片路径）。
///
/// Run one `focus.merge`, returning (start reply, result JSON, still path).
fn run_focus(
    state: &mut AppState,
    ctx: &Context,
    dir: &Path,
    sub: &str,
    extra: Value,
) -> (Value, Value, PathBuf) {
    let mut args = json!({
        "frames": [0, 1, 2],
        "out_dir": dir.join(sub).display().to_string(),
        "confirm": true,
        "threshold": 0.0,
        "feather": 0.0,
    });
    if let (Some(dst), Some(src)) = (args.as_object_mut(), extra.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let started = state
        .run_ui_command(ctx, "focus.merge", &args)
        .expect("启动焦点合成");
    wait_merge(state, ctx);
    let result = state.merge_result.clone().expect("合并结果落账");
    let path = PathBuf::from(result["path"].as_str().expect("成片路径"));
    (started, result, path)
}

fn read_rgb(path: &Path) -> image::RgbImage {
    image::open(path).expect("读回成片").to_rgb8()
}

/// X-C1：`focus.merge` 省略 `align` 与写 `align: false` 逐点一致，且不套任何自动对齐。
/// 反向对照：真正的错位栈在 `align: true` 下会走另一条几何（A-C2 覆盖）。
///
/// X-C1: omitting `align` in `focus.merge` is point-for-point identical to writing
/// `align: false`, and applies no auto alignment at all.
#[test]
fn omitting_align_is_the_same_as_align_false_and_touches_nothing() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = temp_root("x-c1");
    // 三个帧里有一处 8 px 错位：如果代码偷偷跑了估计，输出几何就会变。
    //
    // One of the three frames is 8 px off: if the code secretly estimated anything,
    // the output geometry would change.
    write_sequence(
        &dir.join("src"),
        512,
        512,
        7,
        &[[0.0, 0.0], [8.0, 0.0], [0.0, -8.0]],
    );
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.join("src").display().to_string()}),
        )
        .expect("打开序列");

    let (started_off, result_off, path_off) = run_focus(&mut state, &ctx, &dir, "省略", json!({}));
    let (started_false, result_false, path_false) =
        run_focus(&mut state, &ctx, &dir, "显式关", json!({"align": false}));

    assert_eq!(started_off["align"], json!(false), "省略 align 应回 false");
    assert_eq!(started_false["align"], json!(false), "显式关应回 false");
    assert_eq!(
        result_off["align_status"],
        json!("off"),
        "省略 align 应是 off"
    );
    assert_eq!(result_false["align_status"], json!("off"), "显式关应是 off");
    assert_eq!(result_off["align_gain"], json!(0.0), "off 时增益必须是 0");
    // 结果 JSON 里**不许**出现任何带「reason」的键：中文模板键一律不外发
    // （MCP / 脚本没有 i18n 漏斗），而报告结构里的那个 reason 字段已经整个删掉了。
    // 这里按「键名含 reason」扫而不是写死一个键名——写死只挡得住叫那个名字的字段，
    // 换个名字（`note`、`why`）就绕过去了。
    //
    // No key containing "reason" may appear in the result JSON: Chinese template keys
    // are never emitted (MCP and scripts have no i18n funnel), and the report's reason
    // field is gone entirely. Scanning for the substring "reason" rather than one hard
    // coded key stops any renamed comeback (`note`, `why`, ...) of a field that was
    // removed outright from slipping back in.
    let stray = result_off
        .as_object()
        .expect("结果 JSON 是对象")
        .keys()
        .find(|k| k.contains("reason"));
    assert!(
        stray.is_none(),
        "结果 JSON 不该有任何 reason 字段：{stray:?}"
    );
    // 机器键必须与 `AlignStatus::key()` 逐字一致，且整段 JSON 里不许有汉字——
    // 这份 JSON 会经 attached MCP 原样送到前端，中文模板键漏出去就等于把中文塞给
    // 英文用户。
    //
    // The machine key must match `AlignStatus::key()` verbatim, and the whole JSON must
    // contain no CJK -- it travels through attached MCP to clients as-is, so a leaked
    // Chinese template key would put Chinese in front of English users.
    // 状态键本身必须是纯 ASCII 机器键（整段 JSON 的汉字检查放在 MCP 侧那条判据：
    // 这里的成片路径带着中文输出目录名，整段扫汉字会假红）。
    //
    // The status key itself must be a pure-ASCII machine key (the whole-JSON CJK check
    // lives in the MCP-side assertion: this fixture's output path carries a Chinese
    // directory name, so scanning the entire JSON would false-red here).
    let status = result_off["align_status"].as_str().expect("状态键是字符串");
    assert!(status.is_ascii(), "状态键必须是 ASCII 机器键：{status}");
    assert!(
        [
            AlignStatus::Adopted,
            AlignStatus::RevertedInsufficientGain,
            AlignStatus::Failed
        ]
        .iter()
        .any(|s| s.key() == status)
            || status == "off",
        "状态键只能来自 key() 或 off：{status}"
    );

    let a = read_rgb(&path_off);
    let b = read_rgb(&path_false);
    assert_eq!((a.width(), a.height()), (512, 512), "align:false 不该裁切");
    assert_eq!((b.width(), b.height()), (512, 512), "align:false 不该裁切");
    let mut diff = 0i64;
    let mut max_d = 0i32;
    for (p, q) in a.pixels().zip(b.pixels()) {
        for c in 0..3 {
            let d = (p[c] as i32 - q[c] as i32).abs();
            if d != 0 {
                diff += 1;
            }
            max_d = max_d.max(d);
        }
    }
    eprintln!("X-C1 省略 vs 显式 false：不同通道数 {diff}，max|Δ| = {max_d}");
    assert_eq!(diff, 0, "省略 align 与 align:false 必须逐点一致");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A-C2：8 px 错位栈（白噪声底图）在 `align: true` 下报 `adopted`，且增益达阈值。
///
/// A-C2: an 8 px-offset stack on white noise reports `adopted` under `align: true`, with
/// gain at or above the threshold.
#[test]
fn align_true_adopts_a_shifted_stack() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = temp_root("a-c2");
    write_sequence(
        &dir.join("src"),
        512,
        512,
        7,
        &[[0.0, 0.0], [8.0, 0.0], [0.0, -8.0]],
    );
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.join("src").display().to_string()}),
        )
        .expect("打开序列");
    let (started, result, path) = run_focus(&mut state, &ctx, &dir, "对齐", json!({"align": true}));
    let gain = result["align_gain"].as_f64().expect("增益是数");
    eprintln!(
        "A-C2 align_status={} align_gain={gain:.6} size={}x{}",
        result["align_status"], result["width"], result["height"],
    );
    assert_eq!(started["align"], json!(true), "启动回执应回 true");
    assert_eq!(
        result["align_status"],
        json!(AlignStatus::Adopted.key()),
        "状态键必须来自 AlignStatus::key()，不是手写串"
    );
    assert!(
        gain >= f64::from(focus_align::GAIN_MIN),
        "增益 {gain} 未达阈值 {}",
        focus_align::GAIN_MIN
    );
    // 采用后按估计位移裁到公共窗口：比原图小，且成片仍然合法。
    //
    // After adoption the output is cropped to the common window: smaller than the
    // input, and still a legal image.
    let w = result["width"].as_u64().expect("宽");
    let h = result["height"].as_u64().expect("高");
    assert!(w < 512 && h < 512, "采用后应裁掉错位边界：{w}x{h}");
    let img = read_rgb(&path);
    assert_eq!((img.width() as u64, img.height() as u64), (w, h));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 接缝淡化自检：均匀位移场下逐块 warp 必须与单一全局位移**逐点一致**——块与块
/// 位移相同，淡化权重归一后不该留下任何接缝或暗带。这条钉住 `warp_by_block_field`
/// 的权重归一，避免「看起来能出图、其实块界有一条 24px 灰带」这种哑故障。
///
/// Seam-feather self-check: with a uniform shift field the per-block warp must match one
/// global translation **point for point** -- identical block shifts plus normalised
/// feather weights may not leave any seam or dark band. This pins the normalisation in
/// `warp_by_block_field` and rules out the silent failure where the image looks fine but
/// a 24 px grey band runs along every block boundary.
#[test]
fn uniform_block_field_matches_one_global_translation() {
    let (w, h) = (240usize, 180usize);
    let src: Vec<f32> = (0..w * h * 3)
        .map(|i| {
            let p = i / 3;
            let (x, y) = (p % w, p / w);
            ((x * 7 + y * 13) % 251) as f32 / 255.0
        })
        .collect();
    let shift = [5.5, -3.25];
    let field = vec![shift; focus_align::GRID_COLS * focus_align::GRID_ROWS];
    let (block, bw, bh) = focus_align::warp_by_block_field(&src, w, h, &field, None);
    let mut once = Vec::new();
    let (gw, gh) = warp::transform_into(
        &src,
        w,
        h,
        Framing {
            angle_deg: 0.0,
            off: shift,
        },
        None,
        &mut once,
    );
    assert_eq!((bw, bh), (gw, gh), "尺寸应与全局 warp 一致");
    let mut max_d = 0.0f32;
    let mut worst = (0usize, 0usize, 0usize);
    for p in 0..w * h {
        for c in 0..3 {
            let i = p * 3 + c;
            let d = (block[i] - once[i]).abs();
            if d > max_d {
                max_d = d;
                worst = (p % w, p / w, c);
            }
        }
    }
    eprintln!("逐块 vs 全局（均匀 5.5/−3.25px）：max|Δ| = {max_d}，最差于 {worst:?}");
    assert!(
        max_d <= 1e-5,
        "均匀位移场下逐块 warp 应与全局一致，实测 max|Δ|={max_d} @ {worst:?}"
    );
}

/// A-C3：干净栈（0.5 px 级抖动）在 `align: true` 下报收益不足并回退，成片与
/// `align: false` 逐点一致。
///
/// A-C3: a clean stack (0.5 px-class jitter) reports insufficient gain and reverts
/// under `align: true`; the still is point-for-point identical to `align: false`.
#[test]
fn align_true_reverts_a_clean_stack_without_touching_a_pixel() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = temp_root("a-c3");
    write_sequence(
        &dir.join("src"),
        512,
        512,
        13,
        &[[0.0, 0.0], [0.5, 0.0], [0.0, -0.5]],
    );
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.join("src").display().to_string()}),
        )
        .expect("打开序列");
    let (_, off, path_off) = run_focus(&mut state, &ctx, &dir, "关", json!({"align": false}));
    let (_, on, path_on) = run_focus(&mut state, &ctx, &dir, "开", json!({"align": true}));
    let gain = on["align_gain"].as_f64().expect("增益是数");
    eprintln!(
        "A-C3 align_status={} align_gain={gain:.6} size={}x{}",
        on["align_status"], on["width"], on["height"],
    );
    assert_eq!(
        on["align_status"],
        json!(AlignStatus::RevertedInsufficientGain.key()),
        "干净栈应因收益不足回退"
    );
    assert!(
        gain < f64::from(focus_align::GAIN_MIN),
        "干净栈增益 {gain} 不该达标"
    );
    assert_eq!(off["align_status"], json!("off"));
    // 回退就是不碰：尺寸不变、逐点一致。
    //
    // Reverting means touching nothing: same size, same pixels.
    assert_eq!(on["width"], off["width"], "回退不该改宽");
    assert_eq!(on["height"], off["height"], "回退不该改高");
    let a = read_rgb(&path_off);
    let b = read_rgb(&path_on);
    let mut diff = 0i64;
    let mut max_d = 0i32;
    for (p, q) in a.pixels().zip(b.pixels()) {
        for c in 0..3 {
            let d = (p[c] as i32 - q[c] as i32).abs();
            if d != 0 {
                diff += 1;
            }
            max_d = max_d.max(d);
        }
    }
    eprintln!("A-C3 开 vs 关：不同通道数 {diff}，max|Δ| = {max_d}");
    assert_eq!(diff, 0, "回退后成片必须与 align:false 逐点一致");
    let _ = std::fs::remove_dir_all(&dir);
}

/// X-C2（面板侧）：方法 chip 恰好三个、中文名逐字固定、`method=pyramid` 被命令面
/// 接受。三项缺一都会让「面板选了但命令不认识」或「命令接受但面板没有入口」复活。
///
/// X-C2 (panel side): exactly three method chips, their Chinese names fixed verbatim,
/// and `method=pyramid` accepted by the command surface. Losing any one of the three
/// would revive either "the panel picked it but the command does not know it" or "the
/// command takes it but the panel has no entry".
#[test]
fn panel_offers_exactly_three_focus_methods_and_the_command_takes_pyramid() {
    use aevum_core::focus::Method;
    assert_eq!(Method::ALL.len(), 3, "方法 chip 必须恰好三个");
    let names: Vec<&str> = Method::ALL.iter().map(|m| m.cn()).collect();
    assert_eq!(names, ["深度图", "加权平均", "金字塔"], "中文名逐字固定");
    let keys: Vec<&str> = Method::ALL.iter().map(|m| m.key()).collect();
    assert_eq!(keys, ["depthmap", "weighted", "pyramid"]);
    for m in Method::ALL {
        assert_eq!(Method::parse(m.key()), Some(m), "key 必须能解析回自身");
    }

    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = temp_root("x-c2");
    write_sequence(
        &dir.join("src"),
        64,
        64,
        5,
        &[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
    );
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.join("src").display().to_string()}),
        )
        .expect("打开序列");
    let started = state
        .run_ui_command(
            &ctx,
            "focus.merge",
            &json!({
                "frames": [0, 1, 2],
                "out_dir": dir.join("金字塔").display().to_string(),
                "method": "pyramid",
                "confirm": true,
            }),
        )
        .expect("命令面必须接受 method=pyramid");
    assert_eq!(started["kind"], json!("focus"));
    wait_merge(&mut state, &ctx);
    let result = state.merge_result.clone().expect("合并结果落账");
    assert_eq!(result["kind"], json!("focus"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// X-C3：`method=pyramid` 时传 `feather` 不报错，且输出与不传 `feather` **逐点一致**
/// ——金字塔的融合按层归一，羽化在这里不适用（不读、不报错、不影响）。
///
/// X-C3: with `method=pyramid`, passing `feather` neither errors nor changes the output
/// **point for point** -- pyramid fusion normalises per level, so feathering does not
/// apply here (not read, not rejected, not influential).
#[test]
fn pyramid_ignores_feather_without_erroring_or_changing_a_pixel() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let dir = temp_root("x-c3");
    write_sequence(
        &dir.join("src"),
        96,
        72,
        29,
        &[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
    );
    state
        .run_ui_command(
            &ctx,
            "sequence.open",
            &json!({"root": dir.join("src").display().to_string()}),
        )
        .expect("打开序列");
    let (_, _, plain) = run_focus(
        &mut state,
        &ctx,
        &dir,
        "不传羽化",
        json!({"method": "pyramid"}),
    );
    let (_, _, feathered) = run_focus(
        &mut state,
        &ctx,
        &dir,
        "传羽化",
        json!({"method": "pyramid", "feather": 12.0}),
    );
    let a = read_rgb(&plain);
    let b = read_rgb(&feathered);
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    let mut max_d = 0i32;
    let mut diff = 0i64;
    for (p, q) in a.pixels().zip(b.pixels()) {
        for c in 0..3 {
            let d = (p[c] as i32 - q[c] as i32).abs();
            if d != 0 {
                diff += 1;
            }
            max_d = max_d.max(d);
        }
    }
    eprintln!("X-C3 pyramid 传 feather 12 vs 不传：不同通道数 {diff}，max|Δ| = {max_d}");
    assert_eq!(
        max_d, 0,
        "金字塔法下 feather 必须不影响输出，实测 max|Δ|={max_d}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
