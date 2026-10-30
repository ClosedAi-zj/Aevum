//! 真机 DCP 验证：合成字节拦不住真实写入器的字段摆放。本机装了 Adobe
//! Camera Raw 就有 4000+ 份在野配置，全部过一遍解析；没装就跳过。
//!
//! ```bash
//! cargo test -p lumen-core --test dcp_real -- --nocapture
//! ```

use std::path::{Path, PathBuf};

use lumen_core::dcp;

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
            .is_some_and(|e| e.eq_ignore_ascii_case("dcp"))
        {
            out.push(path);
        }
    }
}

#[test]
fn real_adobe_profiles_parse() {
    let Some(dir) = dcp::default_directory() else {
        eprintln!("没找到 DCP 目录（或没设 LUMEN_DCP）：跳过真机配置");
        return;
    };
    eprintln!("DCP 目录 = {}", dir.display());
    let mut files = Vec::new();
    collect(&dir, &mut files);
    assert!(!files.is_empty(), "目录里没有 *.dcp");

    let mut ok = 0usize;
    let mut hue_sat = 0usize;
    let mut look = 0usize;
    let mut bad: Vec<(PathBuf, String)> = Vec::new();
    for path in &files {
        match dcp::Profile::load(path) {
            Ok(p) => {
                ok += 1;
                let j = p.to_json();
                if j["hue_sat_map"] == "applied" {
                    hue_sat += 1;
                }
                if j["look_table"] == "applied" {
                    look += 1;
                }
            }
            Err(e) => bad.push((path.clone(), e)),
        }
    }
    eprintln!(
        "解析成功 {ok}/{}，带 HueSatMap {hue_sat}，带 LookTable {look}",
        files.len()
    );
    for (path, e) in &bad {
        eprintln!("  失败：{} → {e}", path.display());
    }
    // 解析器只认矩阵与曲线，不认厂商自造扩展；真实库里的失败要压到个位数。
    assert!(
        bad.len() * 200 < files.len(),
        "{} 份配置解析失败，超过 0.5%：{:?}",
        bad.len(),
        bad.iter().take(5).collect::<Vec<_>>()
    );
    // 表解析不能只靠合成字节：在野配置里这两张表必须真的读得出来。
    if files.len() > 100 {
        assert!(hue_sat > 0, "真实库里没有一份 HueSatMap 读进来");
        assert!(look > 0, "真实库里没有一份 LookTable 读进来");
    }

    // 松下机身：配置里的 UniqueCameraModel 应与 EXIF 口径对得上。
    let Some(panasonic) = files
        .iter()
        .find(|p| p.to_string_lossy().contains("Panasonic DC-G9"))
    else {
        eprintln!("本机配置库里没有 Panasonic DC-G9，跳过机型核对");
        return;
    };
    let profile = dcp::Profile::load(panasonic).expect("DC-G9 配置应能解析");
    let json = profile.to_json();
    eprintln!(
        "DC-G9 配置：{} / 相机 {} / 矩阵 {} 份 / HueSatMap {:?} / LookTable {:?}",
        profile.name, profile.camera, json["matrices"], json["hue_sat_dims"], json["look_dims"]
    );
    assert!(profile.matches("Panasonic", "DC-G9"), "机型应命中");
    assert!(!profile.matches("Canon", "EOS R5"), "错配不能命中");

    let auto = dcp::auto_match("Panasonic", "DC-G9").expect("自动匹配 DC-G9");
    assert!(auto.matches("Panasonic", "DC-G9"));
    eprintln!("自动匹配 = {}", auto.path);
}
