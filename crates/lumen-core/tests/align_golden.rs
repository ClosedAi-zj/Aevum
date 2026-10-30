//! 用冻结金标验收模板估计器。
//!
//! 素材（`aligned_lr3/aligned_0000.jpg` …）不在仓库里，找不到时直接跳过，
//! 保证克隆仓库后 `cargo test` 仍然可跑。金标本身只读，不因实现调整。

use std::path::PathBuf;
use std::sync::Mutex;

use lumen_core::align::{self, AlignOptions, Estimator};

/// 合理容差：位移 0.15 px、分数 0.01。
/// 本机实测最大位移偏差约 6e-5 px、分数偏差约 9e-6，远小于容差。
const OFFSET_TOL: f64 = 0.15;
const SCORE_TOL: f64 = 0.01;

#[test]
fn template_matches_python_golden() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let frames_dir = root.join("../../../pythonProject4/aligned_lr3");
    if !frames_dir.join("aligned_0000.jpg").exists() {
        eprintln!("没有素材 {}，跳过金标比对", frames_dir.display());
        return;
    }
    let golden_path = root.join("../../oracle/golden/align.json");
    let golden: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden_path).expect("读不到金标"))
            .expect("金标不是合法 JSON");

    let names: Vec<&str> = golden["frames"]
        .as_array()
        .expect("frames 缺失")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let paths: Vec<PathBuf> = names.iter().map(|n| frames_dir.join(n)).collect();
    let expected_offsets = golden["offsets"].as_array().expect("offsets 缺失");
    let expected_scores = golden["scores"].as_array().expect("scores 缺失");
    assert_eq!(paths.len(), expected_offsets.len());

    let options = AlignOptions {
        reference: 0,
        estimator: Estimator::Template,
        ..Default::default()
    };

    // 进度必须单调、总数正确、最后一帧为总数。
    let seen = Mutex::new(Vec::new());
    let report = align::measure_with_progress(&paths, &options, &|done, total| {
        assert_eq!(total, paths.len(), "进度总数不对");
        seen.lock().unwrap().push(done);
    })
    .expect("对齐失败");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(*seen.last().unwrap(), paths.len(), "进度没有走到总数");
        assert!(
            seen.windows(2).all(|w| w[1] > w[0]),
            "进度回调不是严格递增：{seen:?}"
        );
    }

    assert_eq!(report.offsets.len(), paths.len());
    assert_eq!(report.scores.len(), paths.len());
    assert!(
        report.issues.is_empty(),
        "金标序列不该有异常帧：{:?}",
        report.issues
    );

    let mut max_off = 0.0f64;
    let mut max_score = 0.0f64;
    for (i, want) in expected_offsets.iter().enumerate() {
        let w = want.as_array().unwrap();
        let (wx, wy) = (w[0].as_f64().unwrap(), w[1].as_f64().unwrap());
        let got = report.offsets[i];
        let dev = (got[0] - wx).abs().max((got[1] - wy).abs());
        max_off = max_off.max(dev);
        assert!(
            dev <= OFFSET_TOL,
            "第 {i} 帧位移偏差 {dev:.4} px 超过 {OFFSET_TOL}：金标 ({wx:.5}, {wy:.5})，实得 ({:.5}, {:.5})",
            got[0],
            got[1]
        );
    }
    for (i, want) in expected_scores.iter().enumerate() {
        let ws = want.as_f64().unwrap();
        let dev = (report.scores[i] - ws).abs();
        max_score = max_score.max(dev);
        assert!(
            dev <= SCORE_TOL,
            "第 {i} 帧分数偏差 {dev:.5} 超过 {SCORE_TOL}：金标 {ws:.6}，实得 {:.6}",
            report.scores[i]
        );
    }
    eprintln!(
        "金标比对通过：最大位移偏差 {max_off:.5} px，最大分数偏差 {max_score:.6}（容差 {OFFSET_TOL} px / {SCORE_TOL}）"
    );
}
