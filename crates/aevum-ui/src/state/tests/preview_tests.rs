use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aevum-ui-prev-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn render(path: &std::path::Path, sketch: bool) -> PreviewParts {
    render_preview_file(
        path,
        &GradeParams::default(),
        Ev::NONE,
        None,
        None,
        Framing::default(),
        None,
        sketch,
        // 测试按原口径：草图 = 半尺寸。真实拖动走全分辨率，见
        // `ensure_preview` 的 `decode_half`。
        //
        // The test keeps the original reading: sketch means half size. Real dragging uses
        // full resolution -- see `decode_half` in `ensure_preview`.
        sketch,
        false,
        false,
    )
    .expect("渲染预览")
}

/// 草图贴图减半，但报给画布的逻辑尺寸必须和全量一致，不然拖动时画布会跳一下。
#[test]
fn sketch_texture_is_half_but_logical_size_matches_full() {
    let dir = temp_dir("sketch");
    let path = dir.join("frame.png");
    let mut img = image::RgbImage::new(120, 80);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgb([(x * 2) as u8, (y * 3) as u8, ((x + y) * 3) as u8]);
    }
    img.save(&path).expect("写测试图");

    let full = render(&path, false);
    let sketch = render(&path, true);
    assert_eq!(full.2, [120, 80], "全量逻辑尺寸");
    assert_eq!(sketch.2, full.2, "草图的逻辑尺寸应与全量一致");
    assert_eq!(full.0.size, [120, 80], "全量贴图尺寸");
    assert_eq!(sketch.0.size, [60, 40], "草图贴图应是半分辨率");
    assert_eq!(sketch.0.pixels.len(), 60 * 40, "草图像素数应与贴图尺寸一致");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 打开「白以上检视」后，**穿过整条预览路径**的高光真的没被削到白。
///
/// 这条和 `aevum-core` 里那三条不是一回事：那三条量的是折线，这条量的是
/// `render_preview_file` 交出来的那张贴图——渲染、折线、成图（`ColorImage`）全走完。
/// 中间任何一步（比如成图时又钳一次）把白以上压掉，这里就会看到 255。
///
/// With the inspection on, highlights that went **through the whole preview path** are
/// really not clipped to white. Not the same as the three tests in `aevum-core` — those
/// measure the curve, this measures the texture `render_preview_file` hands back: render,
/// fold and `ColorImage` conversion all done. Any step in between clamping again (for
/// instance at texture creation) shows up here as 255.
#[test]
fn hdr_inspection_survives_the_whole_preview_path() {
    let dir = temp_dir("hdrfold");
    let path = dir.join("frame.png");
    image::RgbImage::from_pixel(24, 16, image::Rgb([200, 180, 160]))
        .save(&path)
        .expect("写测试图");

    // +3EV 把 0.78 编码推到 1.8 上下：SDR 路径必然削到白，HDR 路径有东西可折。
    let params = GradeParams {
        exposure: 3.0,
        ..Default::default()
    };
    let one = |fold: bool| {
        render_preview_file(
            &path,
            &params,
            Ev::NONE,
            None,
            None,
            Framing::default(),
            None,
            false,
            false,
            false,
            fold,
        )
        .expect("渲染预览")
        .0
    };
    let max = |img: &egui::ColorImage| img.pixels.iter().map(|p| p.r()).max().unwrap_or(0);

    let sdr = max(&one(false));
    let hdr = max(&one(true));
    assert_eq!(sdr, 255, "SDR 路径都不到白，这条靶子没有判别力");
    assert!(
        (231..255).contains(&hdr),
        "打开检视后峰值 {hdr}，不在 (231, 255) 里——白以上要么没折进来，要么被削平"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 画布上那张图和导出走的是同一趟取景几何：转过 90°，白点就该出现在画面上方。
#[test]
fn preview_straighten_rotates_the_pixels_it_shows() {
    let dir = temp_dir("rotate");
    let path = dir.join("frame.png");
    let mut img = image::RgbImage::new(9, 9);
    *img.get_pixel_mut(6, 4) = image::Rgb([255, 255, 255]);
    img.save(&path).expect("写测试图");

    let (graded, _, size, _) = render_preview_file(
        &path,
        &GradeParams::default(),
        Ev::NONE,
        None,
        None,
        Framing {
            angle_deg: 90.0,
            off: [0.0, 0.0],
        },
        None,
        false,
        false,
        false,
        false,
    )
    .expect("渲染拉直预览");
    assert_eq!(size, [9, 9]);
    let bright = |x: usize, y: usize| graded.pixels[y * 9 + x].r() > 200;
    assert!(bright(4, 2), "中心右边的点该转到上方");
    assert!(!bright(6, 4), "原来的位置不该还留着白的");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 拖动草图**必须与全量同分辨率**；冷首屏仍必须走半尺寸快路。
///
/// 这两条是一对，缺一条就会退回去：曾经「草图」把所有省法绑在一个布尔上，拖动期间
/// 画布拿到 800px 贴图放大到 1600 逻辑宽，用户看到的就是糊（2026-11-08 实测反馈）。
/// 现在 `sketch`（减 pass）与 `decode_half`（减分辨率）分开，**只有一帧画面都还没
/// 出来时才减分辨率**——那一次值得拿清晰度换延迟，之后一秒都不该再换。
///
/// 判据用像素尺寸说话，不看时间：1200×900 的源在全量档解成 1200×900、半尺寸档解成
/// 800×600，两个数字必须分别落在它们该在的地方。
///
/// A drag sketch **must keep the full resolution**; a cold first screen must still take the
/// half-size fast route. These two are a pair -- dropping one lets the old behaviour come
/// back, where "sketch" carried every shortcut on one boolean and a drag showed an 800 px
/// texture stretched to a 1600 px logical width (user report, 2026-11-08). `sketch` (fewer
/// passes) and `decode_half` (fewer pixels) are separate now, and pixels are only dropped
/// while nothing is on screen yet. The assertion speaks in pixel sizes, not timings: a
/// 1200x900 source decodes to 1200x900 in the full tier and 800x600 in the half tier.
#[test]
fn a_drag_sketch_keeps_full_resolution() {
    let dir = temp_dir("drag-sharp");
    let path = dir.join("frame.png");
    let mut img = image::RgbImage::new(1200, 900);
    for (x, y, px) in img.enumerate_pixels_mut() {
        // 有横向渐变，后面若要比清晰度也有东西可比。
        //
        // A horizontal ramp, so a sharpness comparison has something to work with.
        *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, 96]);
    }
    img.save(&path).expect("写测试图");

    // 量的是**贴图自身的像素尺寸**（`graded.size`），不是返回值里那个 `size`：
    // 后者是全量逻辑尺寸，半尺寸草图也报同一个数（画布要按它铺满），拿它断言看不出
    // 分辨率差别——第一版就是这么假绿的。
    //
    // This measures the **texture's own pixel size** (`graded.size`), not the returned
    // `size`: the latter is the logical full size and a half-size sketch reports the same
    // number (the canvas lays out against it), so asserting on it cannot see the difference --
    // the first cut of this test was falsely green for exactly that reason.
    let render = |sketch: bool, decode_half: bool| {
        render_preview_file(
            &path,
            &GradeParams::default(),
            Ev::NONE,
            None,
            None,
            Framing::default(),
            None,
            sketch,
            decode_half,
            false,
            false,
        )
        .expect("渲染预览")
        .0
        .size
    };

    let full = render(false, false);
    // 拖动：减 pass 不减分辨率 —— 与全量同样大小，画布上不会糊。
    //
    // Drag: fewer passes, same resolution -- the same size as the full frame, so nothing blurs.
    assert_eq!(
        render(true, false),
        full,
        "拖动草图的像素尺寸必须与全量一致，否则画布上是放大的糊图"
    );

    // 冷首屏：减分辨率抢时间。这一档不许被上面那条改掉。只要求「真的小了」，
    // 不绑具体的减半算法（RAW 走传感器一半再缩，位图直接缩一半，两者数值不同）。
    //
    // Cold first screen: halve the pixels to win latency. The check above must not erase this.
    // It only requires "actually smaller" and does not pin the exact halving route (RAW halves
    // the sensor then scales, a bitmap halves directly -- the two give different numbers).
    let cold = render(true, true);
    assert!(
        cold[0] < full[0] && cold[1] < full[1],
        "冷首屏草图没有走半尺寸快路：{cold:?} 不小于全量 {full:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 预览的取景框要和导出同一条采样路：把右下 1/4 铺满整幅，落在框内角上那颗白点
/// 就该被推到输出的左上角，而不是留在源位。
#[test]
fn preview_view_pushes_in_the_pixels_it_shows() {
    let dir = temp_dir("view");
    let path = dir.join("frame.png");
    let mut img = image::RgbImage::new(8, 8);
    *img.get_pixel_mut(4, 4) = image::Rgb([255, 255, 255]);
    img.save(&path).expect("写测试图");

    let plain = render(&path, false).0;
    let (graded, _, size, _) = render_preview_file(
        &path,
        &GradeParams::default(),
        Ev::NONE,
        None,
        None,
        Framing::default(),
        Some(Viewfinder {
            x: 0.5,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        }),
        false,
        false,
        false,
        false,
    )
    .expect("渲染运镜预览");
    assert_eq!(size, [8, 8], "取景框不该改输出尺寸");
    assert!(plain.pixels[4 * 8 + 4].r() > 200, "不动时白点在源位");
    // 窗口 (4,4,4,4) 铺到 8×8 = 推近 2×：采样点 px=3.75+0.5col，源 (4,4) 那点白
    // 被双线性摊成 0.75/0.75/0.25 的权重表——四格 143、八格 48、一格 16，剩下全黑。
    // 逐格对数，比「有没有变亮」钉得住采样相位。
    let w = |i: usize| -> f32 {
        match i {
            0 | 1 => 0.75,
            2 => 0.25,
            _ => 0.0,
        }
    };
    for y in 0..8usize {
        for x in 0..8usize {
            let want = (255.0 * w(x) * w(y)).round() as i32;
            let got = graded.pixels[y * 8 + x].r() as i32;
            assert!(
                (got - want).abs() <= 2,
                "({x},{y}) 的亮度 {got} 对不上推近 2× 的双线性权重表：期望 {want}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 单帧导出的 HDR 分支必须真的走 `render_hdr`：曝光拉高后白以上要活到写盘，
/// 而 SDR 那条路会把它钳成纯白。两条逐笔对比，防止开关又被写回恒 SDR。
#[test]
fn still_export_keeps_highlights_only_on_the_hdr_path() {
    let params = GradeParams {
        exposure: 4.0,
        ..GradeParams::default()
    };
    let mut hdr = vec![0.5f32, 0.5, 0.5];
    grade_still_buffer(&mut hdr, 1, 1, &params, Ev::NONE, OutputDynamic::HdrPq);
    let peak = hdr.iter().copied().fold(0.0f32, f32::max);
    assert!(peak > 1.0, "HDR 单帧导出不该把白以上钳掉：{peak}");
    let mut sdr = vec![0.5f32, 0.5, 0.5];
    grade_still_buffer(&mut sdr, 1, 1, &params, Ev::NONE, OutputDynamic::Sdr);
    assert!(
        sdr.iter().all(|v| *v <= 1.0 + 1e-6),
        "SDR 必须钳到白：{sdr:?}"
    );
}
