//! 漫游配置目录：`%APPDATA%/aevum-studio`，首启把旧目录并过来。
//!
//! 为什么收敛到这一个模块：这个「APPDATA + 目录名」的判断原来在
//! `ui/settings.rs`、`core/library.rs`、`core/queue.rs` 各抄了一遍（4 处），
//! 谁都没做 `lapse-studio` 回退。结果是用 Python 版升上来的用户，
//! `settings.json` / `galleries.json` / `plugins/` 已经静默丢过一次。
//! 本轮顺手补上，别再丢第二次。
//!
//! Roaming config directory: `%APPDATA%/aevum-studio`; on first launch the legacy directory
//! is merged in.
//!
//! Why this lives in one module: the "APPDATA + directory name" decision used to be copied
//! into `ui/settings.rs`, `core/library.rs` and `core/queue.rs` (4 sites), and none of them
//! fell back to `lapse-studio`. Users who upgraded from the Python build therefore lost
//! `settings.json` / `galleries.json` / `plugins/` once, silently. That is fixed here so it
//! does not happen a second time.

use std::path::{Path, PathBuf};

use crate::brand;
use crate::env;

/// 迁移完成标记。存在即认为「已经扫过旧目录」，永不再扫。
///
/// 为什么要哨兵而不是每次判存在：否则用户在新目录里删掉一个插件，下次启动
/// 又会从旧目录把它搬回来，等于删不掉。
const SENTINEL: &str = "adopt-sentinel";

/// 配置目录（**不保证存在**）。
///
/// Config directory (**not guaranteed to exist**).
pub fn dir() -> PathBuf {
    base().join(brand::APP_DIR)
}

/// 漫游根的上一级：`%APPDATA%`，取不到就退到系统临时目录。
///
/// Parent of the roaming root: `%APPDATA%`, falling back to the system temporary directory.
pub fn base() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// 一次性迁移的结果，只用于日志与测试断言。
///
/// Result of the one-shot migration, used for logging and test assertions only.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 整目录 rename 成功时，记录来源目录。
    ///
    /// Source directory, recorded when the whole-directory rename succeeded.
    pub renamed: Option<PathBuf>,
    /// 逐项并进来的条目。
    ///
    /// Entries merged in one by one.
    pub merged: Vec<PathBuf>,
    /// 目标已有同名条目、故保留新者的条目名。
    ///
    /// Names skipped because the target already had an entry with that name.
    pub skipped: Vec<String>,
}

impl Report {
    /// 有没有真的动过东西。
    ///
    /// Whether anything was actually touched.
    pub fn did_something(&self) -> bool {
        self.renamed.is_some() || !self.merged.is_empty()
    }
}

/// 首启迁移，作用在真实的 `%APPDATA%` 上。
///
/// First-launch migration, applied to the real `%APPDATA%`.
pub fn adopt() -> Report {
    adopt_in(&base())
}

/// 首启迁移，作用在指定根上（测试缝）。
///
/// 三条规则：
/// 1. 目标目录不存在 + 有旧目录 → 整目录 `rename`（同卷、原子、最快）。
///    此时**旧路径消失**，但一个文件都没删——内容整体挪到了新路径。
/// 2. 目标目录已存在 + 有旧目录 → 逐项并：**目标缺什么补什么，目标有的以目标为准**，
///    此时旧目录**原样保留**（用户随时能回去翻）。
/// 3. 任何情况下都不删文件：最坏也只是原地不动。
///
/// 冲突为什么以新目录为准：新目录一定是新版本写的，旧目录只可能是更老的版本遗留。
/// 用老覆盖新会把用户在新版本里的设置打回去。合并分支里旧目录不删，判错了也能人工找回。
///
/// **幂等**：靠哨兵文件保证只扫一次。
///
/// First-launch migration, applied to the given root (test seam).
///
/// Three rules:
/// 1. Target absent + legacy directory present → `rename` the whole directory (same volume,
///    atomic, fastest). The **old path disappears**, but no file is deleted: everything moved.
/// 2. Target present + legacy directory present → merge item by item: **fill in what the
///    target lacks, keep what it has**; the legacy directory stays untouched for inspection.
/// 3. No file is deleted under any circumstance: the worst case is that nothing moves.
///
/// Conflicts resolve in favor of the new directory because the new directory is always
/// written by a newer build, while the legacy one can only be left over from an older
/// version. Overwriting new settings with old ones would roll the user's configuration back.
/// The merge branch never deletes the legacy directory, so a wrong call is still recoverable.
///
/// **Idempotent**: a sentinel file guarantees the scan runs once.
pub fn adopt_in(base: &Path) -> Report {
    let fresh = base.join(brand::APP_DIR);
    let mut report = Report::default();

    if fresh.join(SENTINEL).exists() {
        return report;
    }

    // 从新到旧找第一个存在的旧目录。只搬最近的那个：两个都在说明中间跨了两个版本，
    // 更老那份可能有用户手工留下的东西，猜它会覆盖数据。
    let Some(old) = brand::LEGACY_APP_DIRS
        .iter()
        .map(|d| base.join(d))
        .find(|p| p.is_dir())
    else {
        // 没有旧目录：什么都不做，也不建空目录。
        // 不写哨兵是有意的——此时重扫的成本只是两次 is_dir，可忽略。
        return report;
    };

    if fresh.exists() {
        merge_tree(&old, &fresh, &mut report);
        // 合并分支原来一声不响：用户下次打开会发现「有几个设置自己变回来了」，
        // 却找不到任何线索。合并是有副作用的写操作，必须留下痕迹。
        if report.did_something() {
            eprintln!(
                "{}: 配置目录已合并：{} 里缺的项已补进 {}（同名以新目录为准）；\
                 旧目录 {} 保留，确认无误后可自行删除",
                brand::BIN,
                old.display(),
                fresh.display(),
                old.display()
            );
        }
    } else {
        match std::fs::rename(&old, &fresh) {
            Ok(()) => report.renamed = Some(old.clone()),
            // 跨卷、或目录里有文件被占用（杀软正在扫 .exe 插件）时 rename 会失败。
            // 失败后如果什么都不做，用户就「看起来配置丢了」——所以退化成逐项 copy。
            Err(_) => merge_tree(&old, &fresh, &mut report),
        }
    }

    // 哨兵：写在新目录里（rename 成功时新目录已存在）。
    if std::fs::create_dir_all(&fresh).is_ok() {
        let note = format!("{} {}\n", brand::BIN, env!("CARGO_PKG_VERSION"));
        let _ = std::fs::write(fresh.join(SENTINEL), note);
    }

    report
}

/// 逐项把 `from` 的**顶层条目**并进 `to`：目标缺什么补什么，目标有的跳过。
///
/// 不写死文件名（`settings.json`/`galleries.json`/`render_queue.json`/`plugins/`），
/// 按「目录下所有顶层条目」搬 —— 以后加文件不会忘。
fn merge_tree(from: &Path, to: &Path, report: &mut Report) {
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    if std::fs::create_dir_all(to).is_err() {
        return;
    }
    for entry in entries.flatten() {
        let name = entry.file_name();
        let src = entry.path();
        let dst = to.join(&name);
        if dst.exists() {
            // 目标已有：以目标为准。
            report.skipped.push(name.to_string_lossy().into_owned());
            continue;
        }
        if copy_entry(&src, &dst).is_ok() {
            report.merged.push(src);
        }
    }
}

/// 递归复制（`fs::copy` 只处理普通文件）。
fn copy_entry(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for child in std::fs::read_dir(src)?.flatten() {
            copy_entry(&child.path(), &dst.join(child.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// 便携模式 / 测试模式下不要碰 `%APPDATA%`：
/// 任一 override 变量（新名或旧名）被设置，就认为用户自己指了路径。
///
/// 这条同时保护两件事：① 便携盘用户真实的 `%APPDATA%` 不被写；
/// ② 集成测试不会去搬开发机上的配置。
fn portable() -> bool {
    ["SETTINGS", "PLUGINS", "LIBRARY", "RENDER_QUEUE"]
        .iter()
        .any(|n| env::var_os(n).is_some())
}

/// 进程内只跑一次；由 `settings::path` / `library::default_path` /
/// `queue::default_path` 各自的第一行调用。
///
/// 放在「第一个读配置的 API」而不是 `main()`：这样 GUI、MCP 无头、`--selftest`、
/// 集成测试**四个入口都自动覆盖**，不需要在四处各写一遍启动代码。
///
/// Runs once per process; called from the first line of `settings::path`,
/// `library::default_path` and `queue::default_path`.
///
/// Hooking the first API that reads configuration instead of `main()` covers **all four
/// entry points** at once: the GUI, headless MCP, `--selftest` and integration tests. No
/// startup code is duplicated across four places.
pub fn ensure_adopted() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if portable() {
            return;
        }
        let report = adopt();
        if let Some(from) = &report.renamed {
            eprintln!(
                "{}: 配置目录已迁移：{} → {}",
                brand::BIN,
                from.display(),
                dir().display()
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个自清理的临时目录。测试**只碰这里**，绝不碰真实 `%APPDATA%`。
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "aevum-cfg-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, text).unwrap();
    }

    /// C9：旧目录存在、新目录不存在 → **整目录搬**（不是逐项 copy）。
    #[test]
    fn legacy_dir_is_renamed_when_target_absent() {
        let root = TempRoot::new("rename");
        let old = root.0.join("lumen-studio");
        write(&old.join("settings.json"), "{\"font_level\":2}");
        write(&old.join("plugins/p/plugin.toml"), "x");

        let report = adopt_in(&root.0);

        assert_eq!(report.renamed, Some(old.clone()), "应当走整目录 rename");
        assert!(report.merged.is_empty());
        let fresh = root.0.join(brand::APP_DIR);
        assert_eq!(
            std::fs::read_to_string(fresh.join("settings.json")).unwrap(),
            "{\"font_level\":2}"
        );
        assert!(fresh.join("plugins/p/plugin.toml").exists(), "子树要跟着搬");
        assert!(!old.exists(), "rename 之后旧路径不该还在");
        assert!(fresh.join(SENTINEL).exists(), "要写哨兵");
    }

    /// C9 补充：更早的 `lapse-studio` 也要认（本轮之前从来没做过这个回退）。
    #[test]
    fn older_lapse_dir_is_also_adopted() {
        let root = TempRoot::new("lapse");
        write(&root.0.join("lapse-studio/settings.json"), "{}");
        let report = adopt_in(&root.0);
        assert!(report.renamed.is_some());
        assert!(root.0.join(brand::APP_DIR).join("settings.json").exists());
    }

    /// C10：两边都有 → 新目录的文件**不被覆盖**，缺的补齐。
    #[test]
    fn existing_target_wins_and_missing_entries_are_filled() {
        let root = TempRoot::new("merge");
        write(&root.0.join("aevum-studio/settings.json"), "NEW");
        write(&root.0.join("aevum-studio/only-new.json"), "n");
        write(&root.0.join("lumen-studio/settings.json"), "OLD");
        write(&root.0.join("lumen-studio/galleries.json"), "g");
        write(&root.0.join("lumen-studio/plugins/x/plugin.toml"), "x");

        let report = adopt_in(&root.0);

        let fresh = root.0.join(brand::APP_DIR);
        assert_eq!(
            std::fs::read_to_string(fresh.join("settings.json")).unwrap(),
            "NEW",
            "目标已有的文件不能被子目录覆盖"
        );
        assert!(fresh.join("only-new.json").exists());
        assert_eq!(
            std::fs::read_to_string(fresh.join("galleries.json")).unwrap(),
            "g",
            "目标缺的要补齐"
        );
        assert!(fresh.join("plugins/x/plugin.toml").exists(), "子树要递归补");
        assert!(report.skipped.iter().any(|s| s == "settings.json"));
        assert!(root.0.join("lumen-studio").is_dir(), "旧目录一律不删");
    }

    /// C11：哨兵存在 → 什么都不做（用户在新目录删掉的文件不会复活）。
    #[test]
    fn sentinel_prevents_resurrection() {
        let root = TempRoot::new("sentinel");
        write(&root.0.join("lumen-studio/removed-by-user.json"), "old");
        write(&root.0.join("aevum-studio").join(SENTINEL), "aevum 0.5.0\n");
        // 用户已经在新目录里删掉了这个文件
        assert!(!root.0.join("aevum-studio/removed-by-user.json").exists());

        let report = adopt_in(&root.0);

        assert_eq!(report, Report::default(), "有哨兵就必须完全不动");
        assert!(
            !root.0.join("aevum-studio/removed-by-user.json").exists(),
            "删掉的文件不能被搬回来"
        );
    }

    /// 没有旧目录时不能凭空造出配置目录（否则每个只用默认路径的用户
    /// 都会被写一个空 `%APPDATA%/aevum-studio`）。
    #[test]
    fn no_legacy_dir_creates_nothing() {
        let root = TempRoot::new("empty");
        let report = adopt_in(&root.0);
        assert_eq!(report, Report::default());
        assert!(!root.0.join(brand::APP_DIR).exists(), "不该建空目录");
    }

    /// 二次调用必须幂等（哨兵生效）。
    #[test]
    fn second_run_is_a_noop() {
        let root = TempRoot::new("idem");
        write(&root.0.join("lumen-studio/a.json"), "a");
        let first = adopt_in(&root.0);
        assert!(first.did_something());
        let second = adopt_in(&root.0);
        assert_eq!(second, Report::default());
    }
}
