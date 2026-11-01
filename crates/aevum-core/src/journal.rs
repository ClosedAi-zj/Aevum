//! 操作日志：把会改工程/文件/队列成功操作追加进 `<序列>/.aevum/op_journal.jsonl`。
//!
//! 一行一条 JSON，字段固定为 `ts`（Unix 毫秒）、`source`（gui/mcp）、`op`（命令 ID）
//! 和 `detail`（调用参数）。只记成功调用，失败留给调用方错误通道。
//! 日志只追加、不轮转：它用于事后追查"这张图是谁导的"，不是回放引擎。
//!
//! Operation journal: appends successful operations that touch a project, files or the queue to
//! `<序列>/.aevum/op_journal.jsonl`.
//!
//! One JSON object per line, with fixed fields `ts` (Unix milliseconds), `source` (gui/mcp),
//! `op` (command ID) and `detail` (call arguments). Only successful calls are recorded; failures
//! go through the caller's error channel. The journal only appends and never rotates: it serves
//! to trace "who exported this image" after the fact, not to act as a replay engine.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub const FILE: &str = "op_journal.jsonl";

/// GUI 主线程、attach 回环和 MCP 后台线程可能同时写同一个文件，串行化追加。
static LOCK: Mutex<()> = Mutex::new(());

pub fn path(root: &Path) -> PathBuf {
    crate::project::guard_dir(root).join(FILE)
}

/// 哪些命令算"写"：会改文件、工程、队列或导出的成功调用才进日志。
///
/// 读命令（get/list/info/status）和纯计算（测光、去闪分析）不记，避免把日志
/// 变成请求流水。GUI 与 MCP 的 ID 各记各的，`source` 字段已经能区分来源。
///
/// Which commands count as "writes": only successful calls that modify files, the project, the
/// queue or an export reach the journal.
///
/// Read commands (get/list/info/status) and pure computation (metering, deflicker analysis) are
/// not recorded, so the journal does not degrade into a request log. GUI and MCP keep separate
/// IDs, and the `source` field already distinguishes the origin.
pub fn is_write_op(op: &str) -> bool {
    // 这张表是**手抄的**（同时列 GUI 点号 ID 与 MCP 下划线 ID），改名时最容易漏。
    // 别名层已经会把名字规范化，这里再规范化一次是双保险：万一有调用点绕过了
    // 别名层，也不会静默丢日志——日志少一行没人会发现，这是本轮唯一「错了不报错」
    // 的改名点。
    let op = crate::brand::canonical_tool(op);
    let op = op.as_ref();
    matches!(
        op,
        "grade.set"
            | "grade.preset"
            | "grade.reset"
            | "grade.copy"
            | "xmp.import"
            | "xmp.export"
            | "cube.import"
            | "cube.export"
            | "audio.config"
            | "mask.add"
            | "mask.remove"
            | "mask.update"
            | "mask.component"
            | "align.run"
            | "export.video"
            | "export.stills"
            | "hdr.merge"
            | "pano.merge"
            | "queue.add"
            | "queue.remove"
            | "queue.clear"
            | "queue.run"
            | "project.save"
            | "aevum_grade_set"
            | "aevum_grade_preset"
            | "aevum_grade_reset"
            | "aevum_grade_copy"
            | "aevum_xmp_write"
            | "aevum_cube_import"
            | "aevum_cube_export"
            | "aevum_keyframe_add"
            | "aevum_keyframe_remove"
            | "aevum_align"
            | "aevum_export_stills"
            | "aevum_export_video"
            | "aevum_hdr_merge"
            | "aevum_pano_merge"
            | "aevum_queue_add"
            | "aevum_queue_remove"
            | "aevum_queue_clear"
            | "aevum_queue_run"
            | "aevum_project_save"
    )
}

/// 追加一条。调用方一般不关心失败——日志写不进去不能拖垮导出。
///
/// Appends one entry. Callers usually ignore failures: an unwritable journal must not take
/// down an export.
pub fn append(root: &Path, source: &str, op: &str, detail: &Value) -> std::io::Result<()> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = crate::project::guard_dir(root);
    fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = serde_json::to_string(&json!({
        "ts": ts,
        "source": source,
        "op": op,
        "detail": detail,
    }))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path(root))?;
    writeln!(file, "{line}")
}

/// 读末尾 `limit` 条（按写入顺序）；坏行跳过，不存在的日志返回空。
///
/// Reads the last `limit` entries in write order; malformed lines are skipped and a missing
/// journal returns empty.
pub fn tail(root: &Path, limit: usize) -> Vec<Value> {
    if limit == 0 {
        return Vec::new();
    }
    let Ok(text) = fs::read_to_string(path(root)) else {
        return Vec::new();
    };
    let mut entries: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let keep = entries.len().saturating_sub(limit);
    entries.drain(..keep);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "aevum-journal-{tag}-{}-{stamp}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn append_and_tail_keep_order_and_skip_bad_lines() {
        let temp = TempDir::new("roundtrip");
        let root = &temp.0;
        assert!(tail(root, 10).is_empty(), "没有日志时返回空");

        append(root, "gui", "grade.set", &json!({"exposure": 0.5})).unwrap();
        append(
            root,
            "mcp",
            "export.video",
            &json!({"video_out": "D:/out.mp4"}),
        )
        .unwrap();
        append(root, "mcp", "project.save", &json!({})).unwrap();

        let entries = tail(root, 2);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["op"], json!("export.video"));
        assert_eq!(entries[1]["op"], json!("project.save"));
        assert_eq!(entries[0]["source"], json!("mcp"));
        assert!(entries[0]["ts"].as_u64().is_some());

        // 坏行不会让整个日志不可读。
        let mut file = OpenOptions::new().append(true).open(path(root)).unwrap();
        writeln!(file, "not json").unwrap();
        let entries = tail(root, 10);
        assert_eq!(entries.len(), 3);
        assert_eq!(tail(root, 0).len(), 0);

        assert!(is_write_op("aevum_export_video"));
        assert!(is_write_op("queue.run"));
        assert!(!is_write_op("grade.get"));
        assert!(!is_write_op("aevum_measure"));
    }

    /// 旧工具名（改名前的 `lumen_*`）必须仍然被判成写操作。
    ///
    /// 这条测试守的是一个**不会报错的失效模式**：如果 `is_write_op` 只认新名，
    /// 那么用旧名调 MCP 的写操作会「成功执行、但不进 op_journal.jsonl」。
    /// 编译通过、测试全绿、日志静默少一行——本轮最担心的一个点。
    #[test]
    fn legacy_lumen_tool_names_still_count_as_writes() {
        // 每一个都对应表里一条 aevum_* 条目，改名前后必须等价。
        for legacy in [
            "lumen_grade_set",
            "lumen_grade_preset",
            "lumen_grade_reset",
            "lumen_grade_copy",
            "lumen_xmp_write",
            "lumen_cube_import",
            "lumen_cube_export",
            "lumen_keyframe_add",
            "lumen_keyframe_remove",
            "lumen_align",
            "lumen_export_stills",
            "lumen_export_video",
            "lumen_hdr_merge",
            "lumen_pano_merge",
            "lumen_queue_add",
            "lumen_queue_remove",
            "lumen_queue_clear",
            "lumen_queue_run",
            "lumen_project_save",
        ] {
            assert!(is_write_op(legacy), "{legacy} 是写操作却没登记");
        }
        // 读操作不能因为别名被误判成写。
        assert!(!is_write_op("lumen_grade_get"));
        assert!(!is_write_op("lumen_measure"));
    }
}
