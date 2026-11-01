//! 环境变量的唯一取用口：`AEVUM_<NAME>` 优先，`LUMEN_<NAME>` 回退。
//!
//! 为什么要有这层：同一套 if-else 会被抄到 13 个变量、9 个文件里，抄漏一个就变成
//! 「这个变量用老名字不管用」的诡异 bug，而且这种 bug 只在便携盘用户身上出现。
//!
//! 口径说明：`var_str` 把**空串按未设置处理**。这与仓库原有的
//! `if let Some(p) = std::env::var_os(X) && !p.is_empty()` 写法一致，
//! 所以把调用点换成 `var_str` 时可以放心删掉那个 `!p.is_empty()`。
//!
//! The single access point for environment variables: `AEVUM_<NAME>` takes
//! priority, `LUMEN_<NAME>` is the fallback.
//!
//! Why this layer exists: the same if-else used to be copied across 13
//! variables and 9 files, and one missed copy turned into the odd bug of "a
//! variable that does not work under the old name", a bug that only showed
//! up for portable-drive users.
//!
//! Convention note: `var_str` treats an **empty string as unset**. This
//! matches the repository's original `if let Some(p) = std::env::var_os(X)
//! && !p.is_empty()` pattern, so call sites can safely drop that
//! `!p.is_empty()` when switching to `var_str`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::brand;

/// 全局诊断队列：override 路径用不了之类的「必须有人看见」的提示压在这里。
/// GUI 每帧用 `drain_notes` 取走展示；无头模式靠 `note` 自带的 stderr 行兜底。
static NOTES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

/// 测试串行锁：NOTES 是进程级共享的，会 drain 它的测试（notice 用例与跑完整
/// 界面帧的用例——`shell::ui` 每帧第一件事就是 drain）都得持锁，否则并行跑
/// 会互相捞走对方的条目（实测偶发「同一句只该留一条：[]」）。
static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 持住提示队列的测试锁，直到返回的 guard 被丢弃。仅供测试使用。
#[doc(hidden)]
pub fn lock_notes_for_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 队列上限：超过就丢弃并记一句截断提示。诊断不该无限吃内存。
const NOTE_LIMIT: usize = 32;
/// 截断提示（只记一次）。
const NOTE_TRUNCATED: &str = "（诊断提示过多，其余已省略）";

/// 取一个带前缀的环境变量（`OsString` 版，给路径用）。
///
/// Reads one prefixed environment variable (`OsString` version, for paths).
pub fn var_os(name: &str) -> Option<OsString> {
    var_os_with(name, |k| std::env::var_os(k))
}

/// 字符串版；**空串按未设置处理**。
///
/// String version; an **empty string counts as unset**.
pub fn var_str(name: &str) -> Option<String> {
    var_os(name)
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

/// 测试缝：把 getter 抽成参数，单测可以喂一张假表，
/// 不必 `unsafe { env::set_var }`（edition 2024 下它是 unsafe，而且进程级变量
/// 在并行测试里会互相踩）。
///
/// `pub` 而不是 `pub(crate)`：`aevum-ui` 与 `aevum-mcp` 的测试也要用它，
/// 但**产品代码只用 `var_os` / `var_str`**，别直接调这个。
///
/// Test seam: the getter is a parameter, so unit tests can feed a fake table
/// instead of `unsafe { env::set_var }` (it is unsafe under edition 2024,
/// and process-level variables stomp on each other in parallel tests).
///
/// `pub` rather than `pub(crate)`: tests in `aevum-ui` and `aevum-mcp` need
/// it too, but **product code uses only `var_os` / `var_str`** and must not
/// call this directly.
pub fn var_os_with(name: &str, get: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    let fresh = format!("{}{name}", brand::ENV_PREFIX);
    if let Some(v) = get(&fresh) {
        // 新名设了就用新名，即使旧名也设着——新名优先是这层的唯一承诺。
        return Some(v);
    }
    let legacy = format!("{}{name}", brand::LEGACY_ENV_PREFIX);
    let v = get(&legacy)?;
    note_legacy(&legacy);
    Some(v)
}

/// 弃用提示：整个进程只写一行，stderr。
///
/// GUI 子系统下 stderr 不可见（无害）；MCP stdio 模式下 stdout 是协议通道、
/// stderr 是日志，安全。绝不写进 MCP 响应体——多一行「你用了旧名」只烧 token。
fn note_legacy(legacy: &str) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let stripped = legacy
            .strip_prefix(brand::LEGACY_ENV_PREFIX)
            .unwrap_or(legacy);
        let fresh = format!("{}{stripped}", brand::ENV_PREFIX);
        eprintln!(
            "{}: 检出旧环境变量 {legacy}（已生效）；请改用 {fresh}。",
            brand::BIN
        );
    });
}

/// override 路径的目标形态。
///
/// Shape of the target behind an override path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// 落盘文件（settings/library/queue 的 json）。
    ///
    /// A file persisted to disk (the settings/library/queue json).
    File,
    /// 目录本身（留给后续 project 等用）。
    ///
    /// The directory itself (reserved for future uses such as project).
    Dir,
}

/// 压一条进程级诊断提示：进全局队列（按内容去重，上限 32 条），
/// 同时打一行 stderr——GUI 子系统下 stderr 不可见（无害），
/// 无头与命令行模式下这条就是用户唯一能看见的出口。
///
/// 重复消息照样打 stderr 但只入队一次：stderr 是「现场」，队列是「待办」。
///
/// Pushes one process-level diagnostic note: it enters the global queue
/// (deduplicated by content, capped at 32 entries) and also prints one
/// stderr line. stderr is invisible under the GUI subsystem (harmless); in
/// headless and command-line modes this line is the only thing the user
/// sees.
///
/// Repeated messages still print to stderr but enter the queue only once:
/// stderr is the live log, the queue is the to-do list.
pub fn note(msg: impl Into<String>) {
    let msg = msg.into();
    eprintln!("提示：{msg}");
    let queue = NOTES.get_or_init(|| Mutex::new(Vec::new()));
    // 中毒锁里通常还留着有用数据，取出来继续用，别在这里 panic。
    let mut q = queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if q.iter().any(|m| m == &msg) {
        return;
    }
    if q.len() >= NOTE_LIMIT {
        if !q.iter().any(|m| m == NOTE_TRUNCATED) {
            q.push(NOTE_TRUNCATED.to_owned());
        }
        return;
    }
    q.push(msg);
}

/// 取走全部提示（清空队列），给 GUI 每帧轮询用。
///
/// Takes all notes (emptying the queue), for per-frame polling by the GUI.
pub fn drain_notes() -> Vec<String> {
    let queue = NOTES.get_or_init(|| Mutex::new(Vec::new()));
    let mut q = queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::take(&mut *q)
}

/// 快照但不清空，给测试与无头模式用。
///
/// Snapshots without clearing, for tests and headless mode.
pub fn notes_snapshot() -> Vec<String> {
    let queue = NOTES.get_or_init(|| Mutex::new(Vec::new()));
    let q = queue
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    q.clone()
}

/// 校验 override 路径并把问题喊出来：同一（变量，路径）组合整个进程只探一次、
/// 只告一次。返回值与 [`check_override`] 相同；`None` 表示没问题。
///
/// 为什么缓存：`path()` 这三个口子每帧可能被调多次，探针是真实的建删文件
/// 操作，不能每次都来一遍；告警同理，响一次就够了，刷屏只会把它淹掉。
///
/// Validates an override path and reports problems: the same (variable,
/// path) pair is probed and warned about only once per process. The return
/// value matches [`check_override`]; `None` means no problem.
///
/// Why cached: the `path()` entry points may be called many times per frame,
/// and the probe performs real file create/delete operations that must not
/// run every time. Warnings likewise: one alert is enough, and flooding the
/// screen only drowns it.
pub fn check_override_cached(var: &'static str, path: &Path, target: Target) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = format!("{var}\u{1}{}", path.display());
    if let Some(hit) = map.get(&key) {
        return hit.clone();
    }
    let verdict = check_override(var, path, target);
    if let Some(msg) = &verdict {
        note(msg.clone());
    }
    map.insert(key, verdict.clone());
    verdict
}

/// 判定 override 路径当前能不能用。不打印、不建目录、不删用户文件。
///
/// **不回退是产品承诺**（设任一 override 即视为便携模式，绝不碰 `%APPDATA%`），
/// 所以本函数只判定与措辞，用不用这个路径是调用方的事。
/// 可写性一律以「探针文件能不能建出来」为准——Windows 上目录的只读位
/// 语义不同，`permissions().readonly()` 对目录不可信；目标文件本身的
/// 只读位是实打实挡写入的，作为补充原因说明。
///
/// Judges whether an override path is currently usable. No printing, no
/// directory creation, no deleting user files.
///
/// **No fallback is a product promise** (setting any override means portable
/// mode, and `%APPDATA%` is never touched), so this function only judges and
/// words the verdict; whether to use the path is up to the caller.
/// Writability is always decided by whether a probe file can be created.
/// On Windows the directory read-only bit has different semantics and
/// `permissions().readonly()` is not trustworthy for directories; the
/// read-only bit on the target file itself does genuinely block writes and
/// serves as a supplementary reason.
pub fn check_override(var: &'static str, path: &Path, target: Target) -> Option<String> {
    let shown = absolute_for_show(path);
    let tip = "；把该变量去掉以使用默认配置，或改指一个可写的位置。";
    match target {
        Target::File => {
            if path.is_file() {
                let readonly = std::fs::metadata(path).is_ok_and(|m| m.permissions().readonly());
                if readonly {
                    return Some(format!(
                        "{var} 指向的 {shown} 已存在但被设为只读，改动将无法保存{tip}"
                    ));
                }
            }
            match usable_parent(path.parent()) {
                Ok(()) => None,
                Err(reason) => Some(format!(
                    "{var} 指向的 {shown} 不可用：{reason}，设置读不到也存不下{tip}"
                )),
            }
        }
        Target::Dir => {
            if path.is_dir() {
                if dir_is_writable(path) {
                    None
                } else {
                    Some(format!(
                        "{var} 指向的目录 {shown} 存在但不可写（探针文件建不出来，\
                         可能只读或权限不足）{tip}"
                    ))
                }
            } else {
                match usable_parent(path.parent()) {
                    Ok(()) => None,
                    Err(reason) => Some(format!("{var} 指向的目录 {shown} 不可用：{reason}{tip}")),
                }
            }
        }
    }
}

/// 父目录能否容纳目标（文件或子目录）。只判不建：最多上溯一层，
/// 无限上溯会把「D 盘根目录可写」当成什么都能救，报出来的原因反而误导人。
fn usable_parent(parent: Option<&Path>) -> Result<(), String> {
    let Some(parent) = parent else {
        return Err("没有父目录".to_owned());
    };
    if parent.is_dir() {
        return if dir_is_writable(parent) {
            Ok(())
        } else {
            Err(format!(
                "父目录 {} 不可写（探针文件建不出来，可能只读或权限不足）",
                absolute_for_show(parent)
            ))
        };
    }
    if parent.exists() {
        return Err(format!("父目录 {} 不是一个目录", absolute_for_show(parent)));
    }
    // 父不存在：看它的父能不能建出东西（能 ⇒ 这一层建目录是可预期的）。
    match parent.parent() {
        Some(grand) if grand.is_dir() && dir_is_writable(grand) => Ok(()),
        _ => Err(format!(
            "父目录 {} 不存在且无法创建",
            absolute_for_show(parent)
        )),
    }
}

/// 在 `dir` 里建一个探针文件再立刻删掉。探针名带 pid 与纳秒避免撞车；
/// `create_new` 保证不覆盖既有文件，就算删除失败也不至于踩坏用户数据。
///
/// `pub` 是给 `project::guard_dir` 用的：护带目录能不能写必须用同一把尺子量，
/// 各抄一份探针迟早会有一份忘了删探针文件。
///
/// Creates a probe file inside `dir` and immediately deletes it. The probe
/// name carries the pid and nanoseconds to avoid collisions; `create_new`
/// guarantees no existing file is overwritten, so even a failed delete does
/// not damage user data.
///
/// `pub` exists for `project::guard_dir`: whether a guarded directory is
/// writable must be measured with the same ruler, and copies of the probe
/// logic will eventually forget to delete the probe file.
pub fn dir_is_writable(dir: &Path) -> bool {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let probe = dir.join(format!(".aevum-probe-{}-{nanos}", std::process::id()));
    match std::fs::File::create_new(&probe) {
        Ok(_) => {
            // 探针必须不留痕迹：正常路径这里一定删得掉，删失败算不可写偏保守，
            // 但绝没有「探测完留一堆隐藏文件」的风险。
            std::fs::remove_file(&probe).is_ok()
        }
        // PermissionDenied（只读/ACL）直接判不可写；其余错误同样建不出来。
        Err(_) => false,
    }
}

/// 展示用绝对路径（字符串）：消息里必须能让用户在资源管理器里找到这个地方。
///
/// `pub` 是给 `project::guard_dir` 用的：告警文案里的路径口径要和 override 一致。
///
/// Absolute path for display (a string): messages must let the user find the
/// location in a file explorer.
///
/// `pub` exists for `project::guard_dir`: the path convention in warning
/// text must match the override checks.
pub fn absolute_for_show(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 把一张假表包成 getter。`var_os_with` 的测试缝就是为这个存在的：
    /// 不碰真实进程环境（edition 2024 的 `set_var` 是 unsafe，且并行测试会互踩）。
    fn fake(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key: &str| map.get(key).map(|v| OsString::from(v.clone()))
    }

    /// C6：只设旧名 → 取到值。
    #[test]
    fn legacy_only_is_read() {
        let get = fake(&[("LUMEN_FFMPEG", "D:/old/ffmpeg.exe")]);
        assert_eq!(
            var_os_with("FFMPEG", &get),
            Some(OsString::from("D:/old/ffmpeg.exe"))
        );
    }

    /// C7：新旧都设 → 取新名。
    #[test]
    fn fresh_wins_over_legacy() {
        let get = fake(&[
            ("AEVUM_FFMPEG", "D:/new/ffmpeg.exe"),
            ("LUMEN_FFMPEG", "D:/old/ffmpeg.exe"),
        ]);
        assert_eq!(
            var_os_with("FFMPEG", &get),
            Some(OsString::from("D:/new/ffmpeg.exe")),
            "新名必须优先，否则用户改了变量却不生效"
        );
    }

    /// C8：都没设 → None。
    #[test]
    fn neither_set_yields_none() {
        let get = fake(&[]);
        assert_eq!(var_os_with("FFMPEG", &get), None);
    }

    /// 前缀必须是**整体**前缀：`AEVUM_X` 不能被 `X` 之外的名字蹭到。
    #[test]
    fn only_exact_prefixed_names_match() {
        let get = fake(&[("AEVUM_FFMPEG_SRC", "D:/src"), ("LUMENLENS", "x")]);
        assert_eq!(var_os_with("FFMPEG", &get), None);
        assert_eq!(var_os_with("LENS", &get), None);
    }

    /// `var_str` 的空串口径：空串 == 未设置。这条与仓库原有写法保持一致，
    /// 换调用点时才能安全删掉 `!p.is_empty()`。
    #[test]
    fn empty_string_means_unset() {
        let get = fake(&[("AEVUM_SETTINGS", "")]);
        // var_os_with 本身不吞空串（它只管取名），空串过滤在 var_str 那一层
        assert_eq!(var_os_with("SETTINGS", &get), Some(OsString::from("")));
    }

    /// 自清理临时根。测试只碰这里，绝不去真 `%APPDATA%`。
    struct TempRoot(std::path::PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir()
                .join(format!("aevum-env-{tag}-{}-{nanos}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 统计快照里含指定子串的条数。全局队列被其它测试并发写入也无所谓，
    /// 子串是本用例唯一的临时路径，天然免疫。
    fn count_containing(sub: &str) -> usize {
        notes_snapshot().iter().filter(|m| m.contains(sub)).count()
    }

    /// 正常可写：不声不响是**对的**，别误报。
    #[test]
    fn writable_target_is_silent() {
        let root = TempRoot::new("ok");
        assert_eq!(
            check_override("SETTINGS", &root.path().join("settings.json"), Target::File),
            None
        );
        assert_eq!(
            check_override("PLUGINS", root.path(), Target::Dir),
            None,
            "目录型目标存在且可写同样静默"
        );
    }

    /// 父目录缺失且建不出来：路径里有一段是普通文件，目录树到那里就断了。
    /// （Windows 上目录只读位不被系统执行，构造「真只读」得动 ACL；用
    /// 「父不是目录」覆盖同一判定分支——探针建不出来 ⇒ 不可用。）
    #[test]
    fn missing_parent_that_cannot_be_created_is_reported() {
        let root = TempRoot::new("nocreate");
        // root/notadir 是文件 ⇒ root/notadir/sub 既建不出来也探不出可写。
        std::fs::write(root.path().join("notadir"), "x").unwrap();
        let target = root
            .path()
            .join("notadir")
            .join("sub")
            .join("settings.json");
        let msg =
            check_override("SETTINGS", &target, Target::File).expect("不可创建的父目录必须报出来");
        assert!(msg.contains("SETTINGS"), "消息必须点名变量：{msg}");
        assert!(msg.contains("settings.json"), "消息必须含路径：{msg}");
        assert!(
            msg.contains("去掉该变量") || msg.contains("默认配置"),
            "消息必须给下一步：{msg}"
        );
    }

    /// 父目录存在但探针建不出来（这里用「父是文件」触发）。
    #[test]
    fn unusable_parent_is_reported() {
        let root = TempRoot::new("badparent");
        std::fs::write(root.path().join("blocker"), "x").unwrap();
        let target = root.path().join("blocker").join("settings.json");
        let msg =
            check_override("LIBRARY", &target, Target::File).expect("不可写的父目录必须报出来");
        assert!(msg.contains("LIBRARY"), "消息必须点名变量：{msg}");
        assert!(msg.contains("settings.json"), "消息必须含路径：{msg}");
    }

    /// 目标文件已存在且被设只读：Windows 上这个位是真挡写入的。
    #[test]
    fn readonly_target_file_is_reported() {
        let root = TempRoot::new("rofile");
        let target = root.path().join("settings.json");
        std::fs::write(&target, "{}").unwrap();
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&target, perms).unwrap();
        let msg =
            check_override("SETTINGS", &target, Target::File).expect("只读目标文件必须报出来");
        assert!(msg.contains("SETTINGS"), "消息必须点名变量：{msg}");
        assert!(msg.contains("settings.json"), "消息必须含路径：{msg}");
        assert!(msg.contains("只读"), "失败原因要写明只读：{msg}");
    }

    /// 目录型：目录不存在但父可写 → 可创建，静默；目录存在但探针失败 → 报。
    #[test]
    fn dir_target_judged_by_probe() {
        let root = TempRoot::new("dirtarget");
        assert_eq!(
            check_override("PLUGINS", &root.path().join("plugins"), Target::Dir),
            None,
            "父可写 ⇒ 目录可创建，不算问题"
        );
        std::fs::write(root.path().join("blocker"), "x").unwrap();
        let msg = check_override(
            "PLUGINS",
            &root.path().join("blocker").join("plugins"),
            Target::Dir,
        )
        .expect("建不出来的目录必须报出来");
        assert!(msg.contains("PLUGINS"), "消息必须点名变量：{msg}");
    }

    /// 探针不残留：校验前后目录里文件数必须一个不差。
    #[test]
    fn probe_leaves_no_trace() {
        let root = TempRoot::new("probe");
        let dir = root.path().join("cfg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), "{}").unwrap();
        let before = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(
            check_override("SETTINGS", &dir.join("settings.json"), Target::File),
            None
        );
        let after = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(before, after, "探针文件不许留在用户磁盘上");
    }

    /// 同一句提示只入队一次。
    #[test]
    fn note_dedupes_by_content() {
        let msg = format!("aevum-env-test-去重-{}-专属句子", std::process::id());
        note(msg.clone());
        let before = count_containing(&msg);
        note(msg.clone());
        assert_eq!(count_containing(&msg), before, "同一句只该留一条");
    }
    /// 缓存生效：同一（变量，路径）连探两次只产生一条告警。
    #[test]
    fn cached_check_notes_only_once() {
        let root = TempRoot::new("cache");
        std::fs::write(root.path().join("blocker"), "x").unwrap();
        let target = root.path().join("blocker").join("q.json");
        let var = "RENDER_QUEUE";
        let first = check_override_cached(var, &target, Target::File);
        let hits = count_containing("q.json");
        let second = check_override_cached(var, &target, Target::File);
        assert!(first.is_some() && second.is_some(), "两次都该给出判定");
        assert_eq!(
            count_containing("q.json"),
            hits,
            "第二次只该命中缓存，不再压新告警"
        );
    }
}
