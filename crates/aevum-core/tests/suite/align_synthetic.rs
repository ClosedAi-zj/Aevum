//! 合成纹理验证：已知位移、平滑保留/压平、公共裁切不出画、残差相关。

use std::path::{Path, PathBuf};

use aevum_core::align::{self, AlignOptions, CropRect, Estimator};

/// 确定性纹理：低频结构 + 一点细颗粒，既有足够对比又不会像纯噪声那样
/// 在 JPEG/CLAHE 之后整片翻掉。
fn texture(w: u32, h: u32) -> image::RgbImage {
    let mut img = image::RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let fx = (x as f64 * 0.23).sin() * (y as f64 * 0.17).cos();
            let fy = ((x as f64 + y as f64) * 0.11).sin();
            let grain = ((x as u64 * 2_654_435_761 + y as u64 * 40_503) >> 7) % 61;
            let grain = grain as f64 / 61.0 - 0.5;
            let v = (128.0 + 70.0 * fx + 35.0 * fy + 30.0 * grain).clamp(0.0, 255.0);
            let v = v as u8;
            img.put_pixel(x, y, image::Rgb([v, v, v]));
        }
    }
    img
}

fn crop_img(src: &image::RgbImage, x0: u32, y0: u32, w: u32, h: u32) -> image::RgbImage {
    image::imageops::crop_imm(src, x0, y0, w, h).to_image()
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aevum-align-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录失败");
    dir
}

/// `ref` 取 src(14,12)，`cur` 取 src(21,8)：cur 的内容相对 ref 左移 7、下移 4，
/// 因此需要施加的位移是 (+7, -4)。
fn shifted_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let src = texture(288, 208);
    let r = crop_img(&src, 14, 12, 256, 192);
    let c = crop_img(&src, 21, 8, 256, 192);
    let ref_path = dir.join("ref.png");
    let cur_path = dir.join("cur.png");
    r.save(&ref_path).expect("写参考帧失败");
    c.save(&cur_path).expect("写当前帧失败");
    (ref_path, cur_path)
}

#[test]
fn template_recovers_known_shift() {
    let dir = tmp_dir("shift");
    let (ref_path, cur_path) = shifted_pair(&dir);
    let options = AlignOptions {
        reference: 0,
        estimator: Estimator::Template,
        ..Default::default()
    };
    let report = align::measure(&[ref_path, cur_path], &options).expect("对齐失败");
    let off = report.offsets[1];
    assert!(
        (off[0] - 7.0).abs() <= 0.6 && (off[1] + 4.0).abs() <= 0.6,
        "已知位移 (+7,-4) 测得 ({:.3}, {:.3})，误差超过 0.6 px",
        off[0],
        off[1]
    );
    assert!(
        report.scores[1] > 0.9,
        "同源图像匹配分只有 {:.3}",
        report.scores[1]
    );
}

#[test]
fn smooth_preserves_high_score_and_flattens_low_score() {
    let n = 21;
    let mut offsets: Vec<[f64; 2]> = (0..n).map(|i| [i as f64 * 0.1, i as f64 * 0.05]).collect();
    let mut scores = vec![0.9; n];
    // 6 号：真实抖动（高分）→ 原值保留。
    offsets[6] = [60.0, 70.0];
    // 14 号：误匹配（低分）→ 被中值压平。
    offsets[14] = [60.0, 70.0];
    scores[14] = 0.05;

    let kept = align::smooth_offsets(&mut offsets, &scores, &AlignOptions::default());
    assert!(kept.contains(&6), "高分跳变帧应被记录：{kept:?}");
    assert!(kept.contains(&14), "低分跳变帧也应被识别：{kept:?}");
    assert!(
        (offsets[6][0] - 60.0).abs() < 1e-9 && (offsets[6][1] - 70.0).abs() < 1e-9,
        "高分跳变被误压平：{:?}",
        offsets[6]
    );
    assert!(
        offsets[14][0].abs() < 5.0 && offsets[14][1].abs() < 5.0,
        "低分跳变没有被压平：{:?}",
        offsets[14]
    );
}

#[test]
fn smooth_window_zero_leaves_offsets_alone() {
    let mut offsets = vec![[1.0, 2.0], [30.0, -40.0], [3.0, 4.0]];
    let before = offsets.clone();
    let options = AlignOptions {
        smooth_window: 0,
        ..Default::default()
    };
    let kept = align::smooth_offsets(&mut offsets, &[0.9, 0.9, 0.9], &options);
    assert!(kept.is_empty());
    assert_eq!(offsets, before);
}

fn assert_crop_inside(crop: CropRect, offsets: &[[f64; 2]], w: u32, h: u32, tag: &str) {
    assert!(crop.w > 0 && crop.h > 0, "{tag}: 裁切框宽高必须为正");
    assert!(
        crop.x + crop.w <= w && crop.y + crop.h <= h,
        "{tag}: 裁切框超出画幅"
    );
    for o in offsets {
        assert!(
            crop.x as f64 - o[0] >= -1e-9,
            "{tag}: 左边缘出画（offset {o:?}）"
        );
        assert!(
            (crop.x + crop.w) as f64 - o[0] <= w as f64 + 1e-9,
            "{tag}: 右边缘出画（offset {o:?}）"
        );
        assert!(
            crop.y as f64 - o[1] >= -1e-9,
            "{tag}: 上边缘出画（offset {o:?}）"
        );
        assert!(
            (crop.y + crop.h) as f64 - o[1] <= h as f64 + 1e-9,
            "{tag}: 下边缘出画（offset {o:?}）"
        );
    }
}

#[test]
fn phase_estimator_returns_finite_offsets() {
    let dir = tmp_dir("phase");
    let (ref_path, cur_path) = shifted_pair(&dir);
    let options = AlignOptions {
        reference: 0,
        estimator: Estimator::Phase,
        ..Default::default()
    };
    let report = align::measure(&[ref_path, cur_path], &options).expect("相位相关失败");
    assert_eq!(report.offsets.len(), 2);
    assert!(
        report.offsets.iter().flatten().all(|v| v.is_finite()),
        "相位相关出现非法位移：{:?}",
        report.offsets
    );
    assert!(report.crop.w > 0 && report.crop.h > 0);
}

#[test]
fn common_crop_covers_symmetric_and_asymmetric_offsets() {
    let (w, h) = (640u32, 480u32);

    let sym = [[0.0, 0.0], [5.0, -5.0], [-5.0, 5.0]];
    let crop = align::common_crop(&sym, w, h);
    assert_eq!((crop.x, crop.y, crop.w, crop.h), (5, 5, 630, 470));
    assert_crop_inside(crop, &sym, w, h, "symmetric");

    let asym = [[3.2, -7.5], [0.0, 0.0], [-11.3, 2.4]];
    let crop = align::common_crop(&asym, w, h);
    assert_eq!((crop.x, crop.y, crop.w, crop.h), (4, 3, 624, 469));
    assert_crop_inside(crop, &asym, w, h, "asymmetric");

    let none: [[f64; 2]; 0] = [];
    let full = align::common_crop(&none, w, h);
    assert_eq!((full.x, full.y, full.w, full.h), (0, 0, w, h));
}

#[test]
fn residual_scores_recover_alignment() {
    let dir = tmp_dir("residual");
    let (ref_path, cur_path) = shifted_pair(&dir);
    let offsets = [[0.0, 0.0], [7.0, -4.0]];
    let crop = align::common_crop(&offsets, 256, 192);
    let scores =
        align::residual_scores(&[ref_path, cur_path], &offsets, crop, 0).expect("残差相关失败");
    assert_eq!(scores.len(), 2);
    assert!(
        (scores[0] - 1.0).abs() < 1e-9,
        "参考帧应为 1.0，实得 {}",
        scores[0]
    );
    assert!(
        scores[1] > 0.98,
        "warp 后底部 ROI 残差相关只有 {:.4}，说明位移符号或采样口径不对",
        scores[1]
    );
}
