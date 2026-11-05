//! 版本号只有一个读取点：`brand::version()`。
//!
//! # 为什么这条要写成判据
//!
//! 版本号错了不会崩、不会红、不会有人当场发现——它只在用户报障时说错一句话。而「各写一遍
//! `env!("CARGO_PKG_VERSION")`」这件事，代码评审时几乎看不出来：每一处单独看都对。
//! 2026-11-05 这一轮就有五处（插件清单、控制文件、MCP `serverInfo`、配置目录哨兵、更新链）
//! 各写了一遍，其中三处的数值只有靠「工作区四个成员版本恰好同步」才碰巧一致。
//!
//! 判据只看**生产源码**（`crates/*/src`、`apps/*/src`）：测试里读这个宏是正当的，它们拿它
//! 当期望值去比对外报出的版本，那正是这条判据想保住的东西。
//!
//! # Why this is a test
//!
//! A wrong version number does not crash, does not go red, and nobody notices on the day --
//! it only makes support answer the wrong question. And "one `env!("CARGO_PKG_VERSION")`
//! per call site" is nearly invisible in review, because each site looks right alone. The
//! 2026-11-05 round had five such sites (plugin manifest, control file, MCP `serverInfo`,
//! config sentinel, update chain), and three of them agreed only because the four workspace
//! members happen to share a version.
//!
//! The check looks at **production source** only (`crates/*/src`, `apps/*/src`): tests may
//! read the macro legitimately, as the expected value to compare against what is reported --
//! exactly what this test exists to protect.

use std::path::{Path, PathBuf};

/// 允许出现编译期版本宏的唯一文件（相对工作区根）。
///
/// The only file allowed to read the compile-time version macro, relative to the root.
const ALLOWED: &str = "crates/aevum-core/src/brand.rs";

/// 递归收集 `dir` 下的 `.rs` 文件。
///
/// Collect the `.rs` files under `dir`, recursively.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_the_brand_module_reads_the_compile_time_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["crates", "apps"] {
        // 逐 crate 展开，只进 src：tests/ 与 build.rs 不在扫描范围。
        // Expand per crate and enter src only; tests/ and build.rs are out of scope.
        let Ok(crates) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for c in crates.flatten() {
            collect(&c.path().join("src"), &mut files);
        }
    }
    assert!(
        files.len() > 50,
        "只扫到 {} 个源文件，路径推断大概错了",
        files.len()
    );

    let mut offenders = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel == ALLOWED {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.contains(r#"env!("CARGO_PKG_VERSION")"#) {
            offenders.push(rel);
        }
    }
    assert!(
        offenders.is_empty(),
        "版本号只能从 brand::version() 读；这些文件又各写了一遍编译期宏：{offenders:?}"
    );
}

#[test]
fn the_banner_and_the_package_version_agree() {
    assert_eq!(
        aevum_core::brand::version(),
        env!("CARGO_PKG_VERSION"),
        "brand::version() 必须与 Cargo.toml 的版本一致"
    );
    let line = aevum_core::brand::version_line();
    assert_eq!(
        line,
        format!(
            "{} {}",
            aevum_core::brand::TITLE,
            aevum_core::brand::version()
        )
    );
    assert!(!line.contains('\n') && !line.ends_with(' '));
}
