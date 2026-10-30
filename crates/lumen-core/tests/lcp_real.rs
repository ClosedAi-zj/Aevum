//! 真机 LCP 验证：3633 份在野配置的写法差异只有真文件才盖得住。
//! 没装 Adobe 镜头配置就跳过。
//!
//! ```bash
//! cargo test -p lumen-core --test lcp_real -- --nocapture
//! ```

use std::path::{Path, PathBuf};

use lumen_core::lcp;

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
            continue;
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lcp"))
        {
            out.push(path);
        }
    }
}

#[test]
fn real_adobe_lens_profiles_parse_and_match() {
    let Some(dir) = lcp::default_directory() else {
        eprintln!("没找到 LCP 目录（或没设 LUMEN_LCP）：跳过真机镜头配置");
        return;
    };
    eprintln!("LCP 目录 = {}", dir.display());
    let mut files = Vec::new();
    collect(&dir, &mut files);
    assert!(!files.is_empty(), "目录里没有 *.lcp");

    let mut ok = 0usize;
    let mut frames = 0usize;
    let mut multi = 0usize;
    let mut bad: Vec<(PathBuf, String)> = Vec::new();
    for path in &files {
        match lcp::load(path) {
            Ok(file) => {
                ok += 1;
                let n = file.frame_count();
                frames += n;
                multi += usize::from(n > 1);
            }
            Err(e) => bad.push((path.clone(), e)),
        }
    }
    eprintln!(
        "解析成功 {ok}/{}，标定帧 {frames} 个，多帧配置 {multi} 份",
        files.len()
    );
    for (path, e) in &bad {
        eprintln!("  失败：{} → {e}", path.display());
    }
    assert!(
        bad.len() * 200 < files.len(),
        "{} 份配置解析失败，超过 0.5%：{:?}",
        bad.len(),
        bad.iter().take(5).collect::<Vec<_>>()
    );
    assert!(frames > files.len(), "多数配置应不止一帧");
    assert!(multi > 0, "真实库里应有多焦距配置");

    // 分段暗角表：Canon EF-M 32mm 的每一帧都带 `(r, V)` 控制点。核对 r 轴口径
    // （末点 = 画幅角半径）与单调性——这两条是敢用表代替多项式的依据。
    let Some(pw) = files.iter().find(|p| {
        let s = p.to_string_lossy().replace('\\', "/");
        s.contains("/Canon/") && s.contains("EF-M 32mm f1.4")
    }) else {
        eprintln!("没有 EF-M 32mm 的样本，跳过分段暗角核对");
        return;
    };
    let file = lcp::load(pw).expect("分段暗角样本应能解析");
    let req = lcp::Request {
        make: "Canon".to_owned(),
        model: "Canon EOS M10".to_owned(),
        lens: "EF-M32mm f/1.4 STM".to_owned(),
        focal_mm: 32.0,
        focal35_mm: None,
        aperture: Some(6.0),
    };
    let corr = lcp::match_correction(&file, &req).expect("分段暗角样本应能匹配");
    let curve = corr.vig_curve.as_deref().expect("暗角表要读进来");
    let last = *curve.last().unwrap();
    eprintln!(
        "分段暗角：{} 个控制点，末点 ({:.6}, {:.6})",
        curve.len(),
        last.0,
        last.1
    );
    assert!(curve.len() >= 3);
    // hypot(5208/2, 3476/2) / (1.5055 × 5208) = 0.3992。
    assert!((last.0 - 0.3992).abs() < 2e-3, "末点半径 {}", last.0);
    // f/6 的那一帧几乎不暗（衰减 98%），但必须仍是一次衰减而不是增益。
    assert!(last.1 > 0.5 && last.1 <= 1.0, "角上衰减 {}", last.1);
    assert!(
        curve.windows(2).all(|w| w[1].1 <= w[0].1 + 1e-9),
        "表应单调不增：{curve:?}"
    );

    // 取一份已知写法的配置，走完整的选帧 → 折系数。
    let Some(sample) = files.iter().find(|p| {
        let s = p.to_string_lossy().replace('\\', "/");
        s.contains("/Panasonic/") && s.contains("Laowa 10mm F2.8 Zero-D FF")
    }) else {
        eprintln!("本机没有 Laowa 10mm 的配置样本，跳过选帧核对");
        return;
    };
    let file = lcp::load(sample).expect("样本应能解析");
    let req = lcp::Request {
        make: "Panasonic".to_owned(),
        model: "Panasonic DC-S1".to_owned(),
        lens: "Laowa 10mm F2.8 Zero-D FF".to_owned(),
        focal_mm: 10.0,
        focal35_mm: None,
        aperture: Some(2.8),
    };
    let corr = lcp::match_correction(&file, &req).expect("样本应能匹配");
    eprintln!(
        "样本匹配：{} / {} / {:.1}mm / dist={:?}",
        corr.profile, corr.lens, corr.focal35_mm, corr.dist
    );
    assert!(corr.lens.contains("Laowa"));
    assert!(corr.dist.is_some() || corr.fisheye.is_some() || corr.vig.is_some());
    // 这份样本的两帧各标一半：畸变只在第一帧，暗角在第二帧；合并后两项都该在。
    let dist = corr.dist.expect("样本应有畸变标定");
    assert!(dist[0].abs() > 1e-6, "畸变系数不该被空模型盖成零：{dist:?}");
    assert!(corr.vig.is_some(), "样本第二帧的暗角应该带上");
    assert!(corr.norm[0] > 0.0);
}
