//! 品牌的唯一真相来源。改名时只改这个文件，其余代码一律引用常量。
//!
//! 为什么单独立一个模块：这一轮从 Lumen 改成 Aevum，名字同时是 crate 名、二进制名、
//! 目录名、环境变量前缀、工具名前缀、配置目录名、比色空间描述、XMP 工具串。散在
//! 100 多个文件里改，下次再改就得再找一遍；集中之后，「改名」退化成「改这 20 行 + 跑 sed」。
//!
//! 本模块**零依赖**：不 use 任何 crate 内的东西，好让 `env` / `config_dir` / `project`
//! 都能安全地引用它而不形成环。
//!
//! The single source of truth for branding. When renaming, change only this file;
//! all other code refers to the constants.
//!
//! Why a separate module: this round renamed Lumen to Aevum, and the name doubles as the
//! crate name, binary name, directory name, environment variable prefix, tool name prefix,
//! config directory name, color space description, and XMP tool string. Editing it across
//! 100+ files would make the next rename another hunt; keeping it here turns a rename into
//! editing these 20 lines and running sed.
//!
//! This module has **zero dependencies**: it uses nothing from inside the crate, so that
//! `env` / `config_dir` / `project` can all reference it safely without forming a cycle.

/// 英文展示名（README、关于框、比色空间描述里用）。
///
/// English display name (used in the README, the about box, and the color space description).
pub const NAME_EN: &str = "Aevum Studio";
/// 中文展示名。
///
/// Chinese display name.
pub const NAME_CN: &str = "恒晷";
/// 窗口标题：中英并置，中间用中点分隔。
///
/// Window title: Chinese and English side by side, separated by a middle dot.
pub const TITLE: &str = "恒晷 · Aevum Studio";

/// 可执行文件名（不含扩展名）。
///
/// Executable file name (without extension).
pub const BIN: &str = "aevum";
/// 各 crate 的公共前缀。
///
/// Common prefix for all crates.
pub const CRATE_PREFIX: &str = "aevum-";

/// 漫游配置目录名：`%APPDATA%/aevum-studio`。
///
/// Roaming config directory name: `%APPDATA%/aevum-studio`.
pub const APP_DIR: &str = "aevum-studio";
/// 更早的配置目录名，**从新到旧**排列。首启迁移按这个顺序找。
///
/// Earlier config directory names, ordered **from newest to oldest**.
/// First-launch migration probes in this order.
pub const LEGACY_APP_DIRS: [&str; 2] = ["lumen-studio", "lapse-studio"];

/// 序列护带目录名：`<序列>/.aevum`。
///
/// Sequence guard directory name: `<sequence>/.aevum`.
pub const GUARD_DIR: &str = ".aevum";
/// 更早的护带目录名，**从新到旧**排列。打开序列时按这个顺序找。
///
/// Earlier guard directory names, ordered **from newest to oldest**.
/// Opening a sequence probes in this order.
pub const LEGACY_GUARD_DIRS: [&str; 2] = [".lumen", ".lapse"];

/// 环境变量前缀。
///
/// Environment variable prefix.
pub const ENV_PREFIX: &str = "AEVUM_";
/// 旧环境变量前缀，读不到新名时回退。
///
/// Legacy environment variable prefix; used as a fall back when the new name is absent.
pub const LEGACY_ENV_PREFIX: &str = "LUMEN_";

/// MCP 工具名前缀。
///
/// MCP tool name prefix.
pub const TOOL_PREFIX: &str = "aevum_";
/// 旧 MCP 工具名前缀，作为静默别名。
///
/// Legacy MCP tool name prefix, kept as a silent alias.
pub const LEGACY_TOOL_PREFIX: &str = "lumen_";

/// `lumen_grade_get` → `aevum_grade_get`；不是旧名就原样借用。
///
/// 不做存在性校验——校验交给各调用点的命令表，这里只管命名空间。
/// 返回 `Cow` 是为了「本来就是新名」的绝大多数调用不分配内存。
///
/// 幂等：`aevum_x` 不以 `lumen_` 开头，原样返回。
///
/// `lumen_grade_get` → `aevum_grade_get`; anything not carrying the old name is borrowed as is.
///
/// No existence check is performed here; validation belongs to the command tables at each
/// call site, and this function only handles the namespace. The return type is `Cow` so that
/// the vast majority of calls, already on new names, allocate nothing.
///
/// Idempotent: `aevum_x` does not start with `lumen_` and is returned as is.
pub fn canonical_tool<'a>(name: &'a str) -> std::borrow::Cow<'a, str> {
    match name.strip_prefix(LEGACY_TOOL_PREFIX) {
        Some(rest) => std::borrow::Cow::Owned(format!("{TOOL_PREFIX}{rest}")),
        None => std::borrow::Cow::Borrowed(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C12：旧工具名被规范化成新名。
    #[test]
    fn legacy_tool_name_is_canonicalized() {
        assert_eq!(canonical_tool("lumen_grade_get"), "aevum_grade_get");
        // 带多个下划线的也照样转
        assert_eq!(canonical_tool("lumen_open_sequence"), "aevum_open_sequence");
        // 裸前缀本身（理论上不存在，但要保证不 panic 也不变成怪东西）
        assert_eq!(canonical_tool("lumen_"), "aevum_");
    }

    /// C12：新名原样返回（幂等），且走的是 Borrowed 分支不分配。
    #[test]
    fn canonical_name_is_passed_through() {
        assert_eq!(canonical_tool("aevum_grade_get"), "aevum_grade_get");
        assert!(matches!(
            canonical_tool("aevum_grade_get"),
            std::borrow::Cow::Borrowed(_)
        ));
        // 点号命名的控制面命令不含工具前缀，必须原样保留
        assert_eq!(canonical_tool("grade.get"), "grade.get");
        // 中间出现 lumen 不算前缀
        assert_eq!(canonical_tool("aevum_lumen_x"), "aevum_lumen_x");
        assert_eq!(canonical_tool(""), "");
    }

    /// 兼容层的常量必须是「新名的确是新名、旧名的确在列表里」，
    /// 否则 B2 的护带链与回退逻辑会静默走错分支。
    #[test]
    fn brand_constants_are_consistent() {
        assert_eq!(GUARD_DIR, ".aevum");
        assert_eq!(LEGACY_GUARD_DIRS, [".lumen", ".lapse"]);
        assert!(!LEGACY_GUARD_DIRS.contains(&GUARD_DIR));
        assert_eq!(APP_DIR, "aevum-studio");
        assert!(!LEGACY_APP_DIRS.contains(&APP_DIR));
        assert_eq!(ENV_PREFIX, "AEVUM_");
        assert_eq!(LEGACY_ENV_PREFIX, "LUMEN_");
        assert_eq!(TOOL_PREFIX, "aevum_");
        assert_eq!(LEGACY_TOOL_PREFIX, "lumen_");
        // 等长替换是本轮兼容性论证的前提：ICC `desc` 段与 XMP 长度都不许漂移。
        assert_eq!(NAME_EN.len(), "Lumen Studio".len());
        assert_eq!(ENV_PREFIX.len(), LEGACY_ENV_PREFIX.len());
        assert_eq!(TOOL_PREFIX.len(), LEGACY_TOOL_PREFIX.len());
        assert_eq!(GUARD_DIR.len(), LEGACY_GUARD_DIRS[0].len());
        assert_eq!(APP_DIR.len(), LEGACY_APP_DIRS[0].len());
    }
}
