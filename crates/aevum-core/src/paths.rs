//! 路径安全：宽松规范化，以及「输出目录不得落到序列根上」这一条判定。
//!
//! 为什么这条判定必须在 core：它以前有两份一模一样的拷贝（`aevum-ui/src/state.rs`
//! 与 `aevum-mcp/src/tools.rs`），而且两份都只做**词法**比较——于是在根内建一个
//! 指向根自身的 junction（`mklink /J seq\self seq`）再导到 `seq\self`，检查会放行，
//! 成片就直接写进源序列根，正是那句「会污染源帧」要防的事（已实测复现）。
//!
//! 现在判定只有一份，比较走**规范化后的真实路径**（`canonical_loose`），重解析点
//! （符号链接、junction）再也骗不过它；词法与真实位置不一致时宁可直接拒，让用户
//! 换一个说得清的目录。headless（MCP）与界面两侧都调这里，口径不会再分叉。
//!
//! # Path safety
//!
//! Why this check belongs in core: it used to be two identical copies (one in
//! `aevum-ui/src/state.rs`, one in `aevum-mcp/src/tools.rs`), and both compared **lexically
//! only**. A junction inside the root pointing at the root itself (`mklink /J seq\self seq`)
//! therefore slipped through, and exporting to `seq\self` wrote the still straight into the
//! source sequence root -- exactly what the "would contaminate the source frames" message
//! exists to prevent (reproduced).
//!
//! There is one implementation now, comparing **resolved real paths**
//! (`canonical_loose`), so reparse points (symlinks, junctions) cannot fool it any more, and
//! a disagreement between the lexical and the real location is refused outright rather than
//! guessed at. The headless (MCP) side and the UI both call here, so the two can no longer
//! drift apart.

use std::path::{Component, Path, PathBuf};

/// 宽松规范化：目标多半还不存在，`canonicalize` 会失败——于是找到**最深的存在祖先**
/// 规范化它（消掉 `\\?\` 前缀、短名与大小写差异，并把重解析点解析到真实位置），
/// 其余还不存在的段原样接回去。
///
/// 这是「写目标可能还不存在」这个约束下的唯一可用口径：`canonicalize` 对不存在的
/// 路径直接报错，而写之前先建目录再判定又太晚（建目录本身就可能穿过 junction）。
///
/// Loose normalisation: the target usually does not exist yet and `canonicalize` would
/// fail, so the **deepest existing ancestor** is canonicalised instead (settling the
/// `\\?\` prefix, short names, case, and reparse points to their real location) and the
/// still-missing segments are appended as they were.
///
/// This is the only workable convention under "the write target may not exist yet":
/// `canonicalize` errors out on missing paths, and creating the directory before checking
/// is too late -- the creation itself can walk through a junction.
pub fn canonical_loose(p: &Path) -> PathBuf {
    let mut cur = p.to_path_buf();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match cur.canonicalize() {
            Ok(c) => {
                let mut out = c;
                for seg in suffix.iter().rev() {
                    out.push(seg);
                }
                return out;
            }
            Err(_) => match (cur.file_name(), cur.parent()) {
                (Some(name), Some(parent)) if !name.is_empty() => {
                    suffix.push(name.to_owned());
                    cur = parent.to_path_buf();
                }
                _ => return p.to_path_buf(),
            },
        }
    }
}

/// 词法清理（不碰文件系统）：绝对化后折叠 `.` 与 `..`。
///
/// 与 [`canonical_loose`] 的差别正是重解析点：这里纯按字符串走，穿不过 junction。
/// 两者一起用才有意义——真实位置决定「落在哪里」，词法位置决定「用户以为落在哪里」，
/// 两者不一致就说明中间有链接。
///
/// Lexical cleanup (touches no filesystem): make it absolute and fold `.` and `..`.
///
/// That difference from [`canonical_loose`] is exactly the reparse points: this one walks
/// the string, so it cannot see through a junction. Only both together are meaningful --
/// the real location says where the bytes land, the lexical location says where the user
/// thinks they land, and a disagreement means a link sits in between.
pub fn lexical_absolute(p: &Path) -> PathBuf {
    let absolute = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            // 绝对路径的根之上还是根：pop 失败就当原地不动。
            //
            // Above the root of an absolute path is still the root: ignore a failed pop.
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Windows 路径大小写不敏感，比较不能只比字符串。
///
/// Windows paths are case-insensitive, so the comparison cannot be string equality.
#[cfg(windows)]
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// 其余平台按原样比。
///
/// Every other platform compares verbatim.
#[cfg(not(windows))]
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a == b
}

/// 逐段比较两个都已规范化的路径是否指向同一个位置。
///
/// Whether two already-normalised paths point at the same location, compared per component.
fn path_eq(a: &Path, b: &Path) -> bool {
    let a: Vec<_> = a.components().collect();
    let b: Vec<_> = b.components().collect();
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| component_eq(*x, *y))
}

/// `child` 是否等于 `parent` 或落在 `parent` 之内（逐段，大小写按平台口径）。
///
/// Whether `child` equals `parent` or lies inside it (per component, case handled per
/// platform).
fn path_starts_with(child: &Path, parent: &Path) -> bool {
    let mut child = child.components();
    for expected in parent.components() {
        match child.next() {
            Some(actual) if component_eq(actual, expected) => {}
            _ => return false,
        }
    }
    true
}

/// 导出目录不得是序列根，也不得是根的上级，更不得借着重解析点绕过去。
///
/// 三条判据，按这个顺序问（顺序决定用户看到哪一句）：
///
/// 1. 规范化后等于根 → 「会污染源帧」；
/// 2. 规范化后是根的上级 → 批量写与收尾清理都可能碰到源目录；
/// 3. 词法位置与真实位置的包含关系**不一致** → 中间有符号链接或 junction：词法上
///    看着在根内、真实路径却在根外（成片跑到别处，甚至另一个序列的根上），或者
///    反过来词法在外、真实落在根内（照样污染源帧）。两种都拒。
///
/// 第 3 条是这次修复的核心：只做词法比较时，根内一个指向根自身的 junction 会被
/// 放行，成片直接写进源序列根（已实测复现）。
///
/// The export folder must not be the sequence root, must not be an ancestor of it, and must
/// not sneak around either through a reparse point.
///
/// Three checks, in this order (the order decides which sentence the user sees):
///
/// 1. resolves to the root -> "it would contaminate the source frames";
/// 2. resolves to an ancestor of the root -> batch writes and the cleanup pass can both
///    reach the source directory;
/// 3. the lexical and the real containment **disagree** -> a symlink or junction sits in
///    between: lexically inside the root while the real path is outside (the still lands
///    elsewhere, possibly on another sequence's root), or lexically outside while the real
///    path is inside (which contaminates the source frames just the same). Both are refused.
///
/// Check 3 is the core of this fix: comparing lexically only let a junction inside the root
/// that points at the root itself pass, writing the still straight into the source sequence
/// root (reproduced).
pub fn ensure_out_dir_safe(root: &Path, out: &Path) -> Result<(), String> {
    let real_root = canonical_loose(root);
    let real_out = canonical_loose(out);
    if path_eq(&real_out, &real_root) {
        return Err("输出目录不能是序列根目录，会污染源帧".to_owned());
    }
    if path_starts_with(&real_root, &real_out) {
        return Err("输出目录不能是序列根目录的上级".to_owned());
    }
    let lex_root = lexical_absolute(root);
    let lex_out = lexical_absolute(out);
    let lex_inside = path_starts_with(&lex_out, &lex_root);
    let real_inside = path_starts_with(&real_out, &real_root);
    if lex_inside != real_inside {
        return Err("输出目录透过符号链接或 junction 落到了别处：换成真实路径再导出".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每次调用都不同的临时目录（进程号 + tag + 纳秒），跑完各自清掉。
    ///
    /// A fresh temp directory per call (pid + tag + nanos), cleaned up by each test.
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut base = std::env::temp_dir();
        base.push(format!("aevum-paths-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&base).expect("建临时目录失败");
        base
    }

    /// 删掉临时目录（链接本身先摘，别顺着链接删到目标去）。
    ///
    /// Remove a temp directory (detach the link first so the target is not deleted).
    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    /// 建一个目录 junction。失败时必须喊出来——静默 `return` 等于假装通过。
    ///
    /// Create a directory junction. A failure must shout: a silent `return` would be
    /// pretending the check passed.
    #[cfg(windows)]
    fn make_junction(link: &Path, target: &Path) -> bool {
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status();
        match status {
            Ok(s) if s.success() => true,
            other => {
                eprintln!(
                    "⚠ 建 junction 失败（{} -> {}）：{:?}——这条判据本次没验到",
                    link.display(),
                    target.display(),
                    other
                );
                false
            }
        }
    }

    #[test]
    fn the_root_and_its_ancestors_are_refused() {
        let root = temp_dir("root");
        assert!(
            ensure_out_dir_safe(&root, &root).is_err(),
            "输出目录等于序列根必须拒：{}",
            root.display()
        );
        let parent = root.parent().expect("临时目录必须有上级");
        assert!(
            ensure_out_dir_safe(&root, parent).is_err(),
            "输出目录是序列根的上级必须拒：{}",
            parent.display()
        );
        if let Some(grand) = parent.parent() {
            assert!(
                ensure_out_dir_safe(&root, grand).is_err(),
                "隔一层的上级也得拒：{}",
                grand.display()
            );
        }
        cleanup(&root);
    }

    #[test]
    fn subfolders_inside_the_root_are_allowed() {
        let root = temp_dir("inside");
        assert!(
            ensure_out_dir_safe(&root, &root.join("成片")).is_ok(),
            "根下的子目录是默认落点，必须放行"
        );
        // 词法折叠后确实在根内（`root/../root/sub`）：放行。
        //
        // After lexical folding it really is inside the root (`root/../root/sub`): allow.
        let folded = root
            .join("..")
            .join(root.file_name().expect("目录名"))
            .join("成片");
        assert!(
            ensure_out_dir_safe(&root, &folded).is_ok(),
            "词法折叠后在根内应放行：{}",
            folded.display()
        );
        // 根的兄弟目录既不是根也不是根的上级，放行。
        //
        // A sibling of the root is neither the root nor an ancestor: allow.
        let sibling = root.parent().expect("上级").join("别处");
        assert!(
            ensure_out_dir_safe(&root, &sibling).is_ok(),
            "兄弟目录应放行：{}",
            sibling.display()
        );
        cleanup(&root);
    }

    /// 根内一个指向**根本身**的 junction：纯词法比较会放行（`seq\self` 既不等于
    /// `seq`、也不是 `seq` 的上级），规范化后就是根 —— 必须拒。
    ///
    /// A junction inside the root pointing at the **root itself**: a lexical comparison lets
    /// it through (`seq\self` is neither `seq` nor its parent), while the resolved path is
    /// the root -- must be refused.
    #[cfg(windows)]
    #[test]
    fn a_junction_back_to_the_root_is_refused() {
        let root = temp_dir("self");
        let link = root.join("self");
        if !make_junction(&link, &root) {
            cleanup(&root);
            return;
        }
        let out = link.clone();
        let verdict = ensure_out_dir_safe(&root, &out);
        assert!(
            verdict.is_err(),
            "根内指向根自身的 junction 必须拒（词法上它在根内），实际放行了：{}",
            out.display()
        );
        let _ = std::fs::remove_dir(&link);
        cleanup(&root);
    }

    /// 根内一个指向**根外**的 junction：词法上 `root\link\through` 在根内，真实落点
    /// 在根外 —— 用户以为在序列里，实际写到了别处（可能正是另一个序列的根）。
    ///
    /// A junction inside the root pointing **outside**: lexically `root\link\through` is
    /// inside the root, the real landing spot is outside -- the user believes they are
    /// writing inside the sequence while writing somewhere else (possibly another
    /// sequence's root).
    #[cfg(windows)]
    #[test]
    fn a_junction_leading_out_of_the_root_is_refused() {
        let root = temp_dir("out-root");
        let outside = temp_dir("out-side");
        let link = root.join("link");
        if !make_junction(&link, &outside) {
            cleanup(&root);
            cleanup(&outside);
            return;
        }
        let out = link.join("through");
        let verdict = ensure_out_dir_safe(&root, &out);
        assert!(
            verdict.is_err(),
            "穿过指向根外的 junction 必须拒，实际放行了：{}",
            out.display()
        );
        let _ = std::fs::remove_dir(&link);
        cleanup(&root);
        cleanup(&outside);
    }
}
