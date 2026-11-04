use super::*;

/// 颜色映射判据：帧号 0..n 产出互不相同的颜色，同帧号同色，参考帧是灰的。
///
/// Depth-preview colour contract: frame numbers 0..n yield pairwise distinct
/// colours, the same frame number always yields the same colour, and the
/// reference frame is grey.
#[test]
fn depth_preview_colors_are_distinct_per_frame_stable_and_grey_for_the_reference() {
    for count in [2usize, 3, 8, 32] {
        let reference = (count / 2) as u16;
        let mut seen: Vec<[u8; 3]> = Vec::with_capacity(count);
        for owner in 0..count as u16 {
            let c = depth_preview_color(owner, count, reference);
            assert!(!seen.contains(&c), "第 {owner} 帧的颜色撞了: {c:?}");
            seen.push(c);
        }
        // 稳定性：同一个帧号算两遍，逐位相等。
        for owner in 0..count as u16 {
            assert_eq!(
                depth_preview_color(owner, count, reference),
                depth_preview_color(owner, count, reference),
            );
        }
        // 参考帧单列灰色，且不与任何普通帧号撞色。
        assert_eq!(
            depth_preview_color(reference, count, reference),
            [128, 128, 128]
        );
    }
}

/// 下采样判据：已知小图分块平均后中心值正确；空图 / 0 尺寸原样返回不 panic。
///
/// Downscale contract: a known small image block-averages to the correct centre
/// value; empty or zero-size input returns untouched without panicking.
#[test]
fn downscale_long_edge_averages_blocks_and_never_panics_on_degenerate_input() {
    // 4×4 常量图，步长 2：每块平均值就是常量本身。
    let flat: Vec<f32> = vec![0.5; 4 * 4 * 3];
    let (out, w, h) = downscale_long_edge(flat.clone(), 4, 4, 2);
    assert_eq!((w, h), (2, 2));
    assert!(out.iter().all(|&v| (v - 0.5).abs() < 1e-6));
    // 中心值判据用非平坦图：右下 2×2 块里放四个已知值，块平均 = 0.5。
    let mut img = vec![0.0f32; 4 * 4 * 3];
    for (dy, row) in [[0.2f32, 0.4], [0.6, 0.8]].iter().enumerate() {
        for (dx, v) in row.iter().enumerate() {
            let (x, y) = (2 + dx, 2 + dy);
            for c in 0..3 {
                img[(y * 4 + x) * 3 + c] = *v;
            }
        }
    }
    let (out, w, h) = downscale_long_edge(img, 4, 4, 2);
    assert_eq!((w, h), (2, 2));
    let center = out[(2 + 1) * 3];
    let expect = (0.2 + 0.4 + 0.6 + 0.8) / 4.0;
    assert!((center - expect).abs() < 1e-6, "中心 {center} vs {expect}");
    // 负向：空图、0 尺寸、上限 0、长边不足——一律原样返回，不 panic、不改值。
    let (out, w, h) = downscale_long_edge(Vec::new(), 0, 0, 1024);
    assert_eq!((w, h, out.len()), (0, 0, 0));
    let (out, w, h) = downscale_long_edge(flat, 4, 4, 1024);
    assert_eq!((w, h, out.len()), (4, 4, 4 * 4 * 3));
    let (out, w, h) = downscale_long_edge(vec![0.25; 9], 0, 3, 1024);
    assert_eq!((w, h, out.len()), (0, 3, 9));
}

/// 端到端：三条横带各归一帧的序列，按下预览后纹理与说明行都要落地，
/// 且参考帧说明指向中位帧。
///
/// End to end: a three-band sequence where each band owns one frame; after the
/// preview runs, both the texture and the caption land, and the caption points at
/// the middle frame as the reference.
#[test]
fn focus_depth_preview_lands_a_texture_and_names_the_reference_frame() {
    let ctx = Context::default();
    let mut state = AppState::new();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("aevum-focus-depth-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    // 48×48 三条 16 行横带：带 i 只有第 i 帧带纹理（竖条纹），其余帧是平场。
    let planks = |x: usize| if (x / 2).is_multiple_of(2) { 200u8 } else { 40 };
    for i in 0..3u8 {
        let mut img = image::RgbImage::from_pixel(48, 48, image::Rgb([128, 128, 128]));
        for y in (i as u32 * 16)..((i as u32 + 1) * 16) {
            for x in 0..48u32 {
                let v = planks(x as usize);
                img.put_pixel(x, y, image::Rgb([v, v, v]));
            }
        }
        img.save(dir.join(format!("f{i:02}.png")))
            .expect("写测试帧");
    }
    state.seq = Some(Sequence::open(&dir).expect("开测试序列"));
    state.selected = 0;
    state.focus_depth_preview(&ctx);
    let tex = state.focus_depth_tex.as_ref().expect("预览该产出一张纹理");
    assert_eq!(tex.size(), [48, 48], "小图不越降采样上限，尺寸原样");
    let note = state.focus_depth_note.clone().expect("预览该有说明行");
    // 参考帧 = 中位帧 = 下标 1（0 起）；说明行要点名它（帧号沿用命令通道的 0 起）。
    assert!(note.contains("第 1 帧"), "说明行该点名参考帧: {note}");
    assert!(note.contains("参考帧"), "{note}");
    // 负向：单帧序列（单张图）不给预览——至少要 2 帧才有归属可言。
    let single = std::env::temp_dir().join(format!(
        "aevum-focus-depth-single-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&single).expect("建临时目录");
    image::RgbImage::from_pixel(8, 8, image::Rgb([100, 100, 100]))
        .save(single.join("f00.png"))
        .expect("写单帧");
    let mut state2 = AppState::new();
    state2.seq = Some(
        Sequence::open_files(std::slice::from_ref(&single.join("f00.png"))).expect("开单帧序列"),
    );
    state2.focus_depth_preview(&ctx);
    assert!(state2.focus_depth_tex.is_none(), "单帧序列不该产出预览");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&single);
}
