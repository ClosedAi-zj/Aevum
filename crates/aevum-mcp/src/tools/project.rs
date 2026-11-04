//! 工程与外观域：工程文件读写、XMP 元数据、.cube 外观的导入导出。
//!
//! 边界：管「参数怎么存下来、怎么跟别的软件换」——工程是 `.aevum/project.json`，
//! XMP 是 Lightroom 认的那一份，CUBE 是调色结果的外部化。这里不跑像素：导出 .cube
//! 用的是调色链本身，但入口在 `aevum_core::cube`。
//!
//! The project and look domain: project file I/O, XMP metadata, and .cube import/export.
//!
//! Boundary: how parameters are persisted and how they travel to other software --
//! `.aevum/project.json` for the project, XMP for Lightroom-compatible metadata, CUBE for an
//! externalised grade. No pixels run here: exporting a .cube does use the grading chain, but
//! the entry point is `aevum_core::cube`.

use std::fs;
use std::path::Path;

use aevum_core::cube;
use aevum_core::project;
use aevum_core::xmp;
use serde_json::{Value, json};

use super::shared::{absolute, arg_str, confirm, echo_path, opt_bool, opt_u32};
use crate::session::Session;

pub(super) fn project_get(session: &mut Session) -> Result<Value, String> {
    let snapshot = session.snapshot_project()?;
    let path = session.project_path()?;
    Ok(json!({
        "path": path.display().to_string(),
        "exists": path.is_file(),
        "project": to_value!(&snapshot),
    }))
}

pub(super) fn project_save(session: &mut Session) -> Result<Value, String> {
    let snapshot = session.snapshot_project()?;
    let path = session.project_path()?;
    project::save(&path, &snapshot).map_err(|e| format!("保存工程失败：{e}"))?;
    session.project = Some(snapshot);
    Ok(json!({"path": path.display().to_string(), "saved": true}))
}

pub(super) fn journal_list(session: &Session, args: &Value) -> Result<Value, String> {
    let root = session.require_root()?;
    let limit = opt_u32(args, "limit")?.unwrap_or(50).clamp(1, 500) as usize;
    let entries = aevum_core::journal::tail(&root, limit);
    Ok(json!({
        "path": aevum_core::journal::path(&root).display().to_string(),
        "count": entries.len(),
        "entries": entries,
    }))
}

pub(super) fn xmp_read(args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path.is_file() {
        return Err(format!("文件不存在：{}", echo_path(&path)));
    }
    let params = xmp::read(&path).map_err(|e| format!("读 XMP 失败：{e}"))?;
    Ok(json!({
        "path": path.display().to_string(),
        "params": to_value!(&params),
    }))
}

pub(super) fn xmp_write(session: &mut Session, args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
    {
        return Err("path 必须以 .xmp 结尾".to_owned());
    }
    if session.is_source(&path) {
        return Err(format!("拒绝覆盖源图：{}", echo_path(&path)));
    }
    if path.exists() && !confirm(args) {
        return Err(format!(
            "目标已存在，需 confirm: true：{}",
            echo_path(&path)
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    xmp::write(&path, &session.params).map_err(|e| format!("写 XMP 失败：{e}"))?;
    Ok(json!({
        "path": path.display().to_string(),
        "preset": session.preset,
    }))
}

/// 挂一张外部 .cube 作为「外观」：解析、报出格点信息，默认同时应用到会话参数。
pub(super) fn cube_import(session: &mut Session, args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path.is_file() {
        return Err(format!("文件不存在：{}", echo_path(&path)));
    }
    let key = path.to_string_lossy().into_owned();
    let lut = cube::refresh(&key).map_err(|e| format!("读 .cube 失败：{e}"))?;
    let info = cube::describe(&lut);
    let apply = opt_bool(args, "apply")?.unwrap_or(true);
    if apply {
        session.params.look_cube = key.clone();
        session.preset = "自定义".to_owned();
    }
    Ok(json!({
        "path": key,
        "title": info.title,
        "size": info.size,
        "nodes": info.nodes,
        "domain_min": info.domain_min,
        "domain_max": info.domain_max,
        "neutral": info.neutral,
        "applied": apply,
    }))
}

/// 把当前参数烘成一张 .cube（只含逐点运算，与 Python 的 `write_cube` 同一条路）。
pub(super) fn cube_export(session: &Session, args: &Value) -> Result<Value, String> {
    let path = absolute(Path::new(arg_str(args, "path")?))?;
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cube"))
    {
        return Err("path 必须以 .cube 结尾".to_owned());
    }
    if path.exists() && !confirm(args) {
        return Err(format!(
            "目标已存在，需 confirm: true：{}",
            echo_path(&path)
        ));
    }
    let size = opt_u32(args, "size")?.unwrap_or(cube::DEFAULT_SIZE as u32) as usize;
    if !(cube::MIN_SIZE..=cube::MAX_SIZE).contains(&size) {
        return Err(format!(
            "size 只能在 {}~{} 之间",
            cube::MIN_SIZE,
            cube::MAX_SIZE
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
    }
    let text =
        cube::export_text(&session.params, size).map_err(|e| format!("写 .cube 失败：{e}"))?;
    fs::write(&path, &text).map_err(|e| format!("写 .cube 失败：{e}"))?;
    Ok(json!({
        "path": path.display().to_string(),
        "size": size,
        "nodes": size * size * size,
        "bytes": text.len(),
        "look": session.params.look_cube,
    }))
}
