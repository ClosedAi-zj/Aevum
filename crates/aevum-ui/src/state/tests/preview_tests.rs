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
    )
    .expect("渲染拉直预览");
    assert_eq!(size, [9, 9]);
    let bright = |x: usize, y: usize| graded.pixels[y * 9 + x].r() > 200;
    assert!(bright(4, 2), "中心右边的点该转到上方");
    assert!(!bright(6, 4), "原来的位置不该还留着白的");
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
