//! 调色域：参数读写、预设、命名副本、关键帧。
//!
//! 边界：只动会话里的 `GradeParams` 与关键帧表，不碰像素、不读写文件——把参数变成
//! 图像是 `export`/`merge` 的活。关键帧的插值口径（两端之外沿用最近的关键帧）是预览
//! 与导出共用的约定，改这里等于同时改两条路径。
//!
//! The grading domain: reading and merging parameters, presets, named copies, keyframes.
//!
//! Boundary: it only touches `GradeParams` and the keyframe table in the session; it never
//! touches pixels and never reads or writes files -- turning parameters into an image is the
//! job of `export`/`merge`. The keyframe interpolation convention (outside the range the
//! nearest keyframe wins) is shared by preview and export, so changing it changes both paths
//! at once.

use aevum_core::grade::{self, GradeParams, PRESET_NAMES};
use aevum_core::interp;
use aevum_core::project;
use serde_json::{Value, json};

use super::shared::{arg_frame, arg_str, echo_text, opt_bool, opt_u32};
use crate::session::Session;

pub(super) fn grade_get(session: &mut Session) -> Result<Value, String> {
    let copies = session.copies();
    let active = copies
        .iter()
        .find(|c| c.params == session.params && c.keyframes == session.keyframes)
        .map(|c| c.name.clone());
    Ok(json!({
        "preset": session.preset,
        "params": to_value!(&session.params),
        "copies": copies
            .iter()
            .map(|c| json!({"name": c.name, "keyframes": c.keyframes.len()}))
            .collect::<Vec<_>>(),
        "active_copy": active,
    }))
}

pub(super) fn grade_set(session: &mut Session, args: &Value) -> Result<Value, String> {
    let partial = args
        .get("params")
        .ok_or_else(|| "缺少 params 对象".to_owned())?;
    session.params = merge_params(&session.params, partial)?;
    session.preset = "自定义".to_owned();
    grade_get(session)
}

/// 把部分字段合并进一组参数；未知字段直接报错，避免 agent 以为改成功了。
fn merge_params(base: &GradeParams, partial: &Value) -> Result<GradeParams, String> {
    let object = partial
        .as_object()
        .ok_or_else(|| "params 必须是对象".to_owned())?;
    let mut current = to_value!(base);
    let current_object = current
        .as_object_mut()
        .expect("GradeParams 一定序列化成对象");
    let mut unknown = Vec::new();
    for (key, value) in object {
        if current_object.contains_key(key) {
            current_object.insert(key.clone(), value.clone());
        } else {
            unknown.push(key.clone());
        }
    }
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!("未知调色字段：{}", echo_text(&unknown.join(", "))));
    }
    serde_json::from_value(current).map_err(|e| format!("参数格式不对：{e}"))
}

pub(super) fn grade_preset(session: &mut Session, args: &Value) -> Result<Value, String> {
    let name = arg_str(args, "name")?;
    if !PRESET_NAMES.contains(&name) {
        return Err(format!(
            "未知预设 {}；可用：{}",
            echo_text(name),
            PRESET_NAMES.join("、")
        ));
    }
    session.params = grade::preset(name);
    session.preset = name.to_owned();
    grade_get(session)
}

pub(super) fn grade_reset(session: &mut Session) -> Result<Value, String> {
    session.params = GradeParams::default();
    session.preset = "原始".to_owned();
    grade_get(session)
}

pub(super) fn grade_copy(session: &mut Session, args: &Value) -> Result<Value, String> {
    let op = args.get("op").and_then(Value::as_str).unwrap_or("list");
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let check_name = |name: &str| -> Result<(), String> {
        if name.is_empty() {
            return Err("name 不能为空".to_owned());
        }
        if name.chars().count() > 40 {
            return Err("副本名至多 40 个字符".to_owned());
        }
        Ok(())
    };
    match op {
        "list" => grade_get(session),
        "save" => {
            check_name(&name)?;
            let overwrite = opt_bool(args, "overwrite")?.unwrap_or(false);
            let params = session.params.clone();
            let keyframes = session.keyframes.clone();
            let index =
                session.with_copies(|copies| match copies.iter().position(|c| c.name == name) {
                    Some(_) if !overwrite => Err(format!(
                        "副本「{}」已存在；要覆盖给 overwrite: true",
                        echo_text(&name)
                    )),
                    Some(i) => {
                        copies[i].params = params.clone();
                        copies[i].keyframes = keyframes.clone();
                        Ok(i)
                    }
                    None => {
                        copies.push(project::GradeCopy {
                            name: name.clone(),
                            params,
                            keyframes,
                        });
                        Ok(copies.len() - 1)
                    }
                })?;
            Ok(json!({
                "saved": name,
                "index": index + 1,
                "copies": session.copies().len(),
            }))
        }
        "apply" => {
            let Some(copy) = session.copies().into_iter().find(|c| c.name == name) else {
                return Err(format!("没有叫 {} 的副本", echo_text(&name)));
            };
            session.params = copy.params;
            session.keyframes = copy.keyframes;
            session.preset = format!("副本 {}", copy.name);
            grade_get(session)
        }
        "delete" => {
            check_name(&name)?;
            let removed = session.with_copies(|copies| copies.iter().position(|c| c.name == name));
            let Some(i) = removed else {
                return Err(format!("没有叫 {} 的副本", echo_text(&name)));
            };
            session.with_copies(|copies| {
                copies.remove(i);
            });
            Ok(json!({"deleted": name, "copies": session.copies().len()}))
        }
        "rename" => {
            check_name(&name)?;
            let to = args
                .get("to")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            check_name(&to)?;
            session.with_copies(|copies| {
                if copies.iter().any(|c| c.name == to) {
                    return Err(format!("副本「{}」已存在", echo_text(&to)));
                }
                let Some(slot) = copies.iter_mut().find(|c| c.name == name) else {
                    return Err(format!("没有叫 {} 的副本", echo_text(&name)));
                };
                slot.name = to.clone();
                Ok(())
            })?;
            Ok(json!({"renamed": to, "copies": session.copies().len()}))
        }
        other => Err(format!(
            "未知 op {}；可用 list / save / apply / delete / rename",
            echo_text(other)
        )),
    }
}

// ------------------------------------------------------------------ 关键帧

pub(super) fn keyframe_add(session: &mut Session, args: &Value) -> Result<Value, String> {
    let frame = arg_frame(session, args, "frame")?;
    // 不传 params 时把当前基准参数钉在该帧；传了就在基准上做字段合并。
    let params = match args.get("params") {
        None | Some(Value::Null) => session.params.clone(),
        Some(partial) => merge_params(&session.params, partial)?,
    };
    session.keyframes.insert(frame, params);
    keyframe_state(session)
}

pub(super) fn keyframe_remove(session: &mut Session, args: &Value) -> Result<Value, String> {
    let frame = arg_frame(session, args, "frame")?;
    if session.keyframes.remove(&frame).is_none() {
        return Err(format!("第 {frame} 帧没有关键帧"));
    }
    keyframe_state(session)
}

pub(super) fn keyframe_list(session: &mut Session, args: &Value) -> Result<Value, String> {
    let mut out = keyframe_state(session)?;
    // 可选 frame：顺便给出该帧插值后的参数，等价于 GUI 里跳到某帧。
    if let Some(frame) = opt_u32(args, "frame")? {
        let index = frame as usize;
        let n = session.require_seq()?.len();
        if index >= n {
            return Err(format!("frame {index} 超出范围 0..{}", n.saturating_sub(1)));
        }
        let params = interp::params_at(&session.params, &session.keyframes, index);
        out["at"] = json!({
            "frame": index,
            "is_keyframe": session.keyframes.contains_key(&index),
            "params": to_value!(&params),
        });
    }
    Ok(out)
}

fn keyframe_state(session: &Session) -> Result<Value, String> {
    let frames: Vec<usize> = session.keyframes.keys().copied().collect();
    let mut keyframes = Vec::with_capacity(frames.len());
    for frame in &frames {
        let params = session.keyframes.get(frame).expect("刚刚从同一张表取出");
        keyframes.push(json!({"frame": frame, "params": to_value!(params)}));
    }
    Ok(json!({
        "count": frames.len(),
        "frames": frames,
        "keyframes": keyframes,
        "base": to_value!(&session.params),
        "preset": session.preset,
        "note": "两帧之间线性插值，两端之外沿用最近的关键帧；导出与预览都会生效。"
    }))
}

// ------------------------------------------------------------------ 预览与导出
