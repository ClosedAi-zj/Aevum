//! 蒙版命令：蒙版的增删改与组件录入，供 agent 与命令文件调用。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Mask commands: Adding, removing and updating masks and their components, as reached by the agent and by command files.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ 蒙版命令

    pub(super) fn mask_list_json(&self) -> Value {
        let list: Vec<Value> = self
            .params
            .masks
            .iter()
            .enumerate()
            .map(|(i, raw)| {
                let parsed = mask::parse_mask(raw);
                let shapes: Vec<Value> = parsed
                    .as_ref()
                    .map(|m| {
                        m.components
                            .iter()
                            .map(|c| {
                                json!({
                                    "kind": c.shape.as_str(),
                                    "blend": c.blend.as_str(),
                                    "invert": c.invert,
                                    "x1": c.x1,
                                    "y1": c.y1,
                                    "x2": c.x2,
                                    "y2": c.y2,
                                    "feather": c.feather,
                                    "midpoint": c.midpoint,
                                    "angle": c.angle,
                                    "strokes": if c.shape == mask::ShapeKind::Brush {
                                        json!(c.strokes.len())
                                    } else {
                                        Value::Null
                                    },
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({
                    "index": i + 1,
                    "id": parsed.as_ref().map(|m| m.id.clone()).unwrap_or_default(),
                    "shape": shapes
                        .first()
                        .and_then(|s| s["kind"].as_str())
                        .unwrap_or("linear"),
                    "components": shapes.len(),
                    "shapes": shapes,
                    "amount": parsed.as_ref().map(|m| m.amount).unwrap_or(0.0),
                    "invert": parsed.as_ref().map(|m| m.invert).unwrap_or(false),
                    "range": parsed
                        .as_ref()
                        .and_then(|m| m.range.as_ref())
                        .map(lum_range_json),
                    "params": parsed.as_ref().map(|m| m.params.clone()).unwrap_or_default(),
                })
            })
            .collect();
        json!({
            "count": list.len(),
            "selected": self.mask_sel.map(|i| i + 1),
            "component": self.mask_comp + 1,
            "masks": list,
        })
    }

    pub(super) fn cmd_mask_add(&mut self, args: &Value) -> Result<Value, String> {
        self.require_sequence()?;
        let kind = opt_str(args, "kind")?.unwrap_or("linear");
        let kind = kind.trim().to_ascii_lowercase();
        if kind != "linear" && kind != "radial" && kind != "brush" {
            return Err(crate::i18n::tr_fmt(
                "未知蒙版形状 {}；可用 linear / radial / brush",
                &[&kind],
            ));
        }
        let index = self.add_mask(&kind);
        let shape = match kind.as_str() {
            "radial" => "径向",
            "brush" => "画笔",
            _ => "线性",
        };
        self.status = crate::i18n::tr_fmt(
            "已添加{}蒙版；拖动画布手柄调整范围",
            &[&crate::i18n::tr(shape)],
        );
        let mut out = self.mask_list_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("added".to_owned(), json!(index + 1));
        }
        Ok(out)
    }

    pub(super) fn cmd_mask_remove(&mut self, args: &Value) -> Result<Value, String> {
        self.require_sequence()?;
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        if index == 0 || index as usize > self.params.masks.len() {
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.params.masks.len()],
            ));
        }
        self.remove_mask(index as usize - 1);
        self.status = crate::i18n::tr_fmt("已删除第 {} 张蒙版", &[&index]);
        let mut out = self.mask_list_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("removed".to_owned(), json!(index));
        }
        Ok(out)
    }

    pub(super) fn cmd_mask_update(&mut self, args: &Value) -> Result<Value, String> {
        self.require_sequence()?;
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        let slot = index as usize;
        if slot == 0 || slot > self.params.masks.len() {
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.params.masks.len()],
            ));
        }
        let component = opt_u32(args, "component")?.unwrap_or(0) as usize;
        let mut raw = self.params.masks[slot - 1].clone();
        let grouped = raw
            .as_object()
            .is_some_and(|o| o.contains_key("components"));
        let shapes = mask::parse_mask(&raw)
            .map(|m| m.components.len())
            .unwrap_or(0)
            .max(1);
        if component >= shapes {
            return Err(crate::i18n::tr_fmt(
                "component 超出范围：0..{}",
                &[&(shapes - 1)],
            ));
        }

        if let Some(amount) = opt_f64(args, "amount")? {
            let object = raw
                .as_object_mut()
                .ok_or_else(|| "蒙版格式不对：不是对象".to_owned())?;
            object.insert("amount".to_owned(), json!(amount.clamp(0.0, 100.0)));
        }
        if let Some(invert) = args.get("invert") {
            let value = invert
                .as_bool()
                .ok_or_else(|| "invert 必须是布尔值".to_owned())?;
            let object = raw
                .as_object_mut()
                .ok_or_else(|| "蒙版格式不对：不是对象".to_owned())?;
            object.insert("invert".to_owned(), json!(value));
        }
        if let Some(params) = args.get("params") {
            let params = params
                .as_object()
                .ok_or_else(|| "params 必须是对象".to_owned())?;
            if !grouped {
                // 扁平老格式没有 params 字段，parse_mask 也不读它；先转成组再改。
                raw = mask::as_group(&raw);
            }
            let object = raw
                .as_object_mut()
                .ok_or_else(|| "蒙版格式不对：不是对象".to_owned())?;
            let target = object
                .entry("params".to_owned())
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| "蒙版 params 格式不对".to_owned())?;
            let mut unknown = Vec::new();
            for (key, value) in params {
                if !mask::MASK_KEYS.contains(&key.as_str()) {
                    unknown.push(key.clone());
                    continue;
                }
                if value.is_null() {
                    target.remove(key);
                } else {
                    let number = value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| {
                        crate::i18n::tr_fmt("局部参数 {} 必须是有限数值或 null", &[&key])
                    })?;
                    target.insert(key.clone(), json!(number));
                }
            }
            if !unknown.is_empty() {
                unknown.sort();
                return Err(crate::i18n::tr_fmt(
                    "未知局部参数：{}",
                    &[&unknown.join("、")],
                ));
            }
        }
        if let Some(geometry) = args.get("geometry") {
            let geometry = geometry
                .as_object()
                .ok_or_else(|| "geometry 必须是对象".to_owned())?;
            apply_mask_geometry(&mut raw, component, geometry)?;
        }
        if let Some(range) = args.get("range") {
            let range = range
                .as_object()
                .ok_or_else(|| "range 必须是对象".to_owned())?;
            apply_mask_range(&mut raw, range)?;
        }

        // 先确认改完还能解析，再落回参数，避免坏数据覆盖好的蒙版。
        if mask::parse_mask(&raw).is_none() {
            return Err("改完的蒙版无法解析；请检查 geometry / invert 字段".to_owned());
        }
        self.params.masks[slot - 1] = raw;
        self.mask_sel = Some(slot - 1);
        self.mask_comp = component;
        self.masks_changed();
        self.status = crate::i18n::tr_fmt("已更新第 {} 张蒙版（来自命令通道）", &[&index]);
        let mut out = self.mask_list_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("updated".to_owned(), json!(index));
        }
        Ok(out)
    }

    /// 组内形状的增删：`action` 为 add（默认）或 remove。
    pub(super) fn cmd_mask_component(&mut self, args: &Value) -> Result<Value, String> {
        self.require_sequence()?;
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        let masks = self.params.masks.len();
        if index == 0 || index as usize > masks {
            return Err(crate::i18n::tr_fmt("index 超出范围：1..={}", &[&masks]));
        }
        let slot = index as usize - 1;
        let action = opt_str(args, "action")?
            .unwrap_or("add")
            .trim()
            .to_ascii_lowercase();
        let (detail, message) = match action.as_str() {
            "add" => {
                let kind = opt_str(args, "kind")?
                    .unwrap_or("linear")
                    .trim()
                    .to_ascii_lowercase();
                if kind != "linear" && kind != "radial" && kind != "brush" {
                    return Err(crate::i18n::tr_fmt(
                        "未知蒙版形状 {}；可用 linear / radial / brush",
                        &[&kind],
                    ));
                }
                let added = self
                    .add_mask_component(slot, &kind)
                    .ok_or_else(|| "这一组蒙版格式不对，加不了形状".to_owned())?;
                let shape = match kind.as_str() {
                    "radial" => "径向",
                    "brush" => "画笔",
                    _ => "线性",
                };
                (
                    json!(added + 1),
                    crate::i18n::tr_fmt(
                        "已在第 {} 张蒙版里加了第 {} 个{}形状",
                        &[&index, &(added + 1), &crate::i18n::tr(shape)],
                    ),
                )
            }
            "remove" => {
                let current = if self.mask_sel == Some(slot) {
                    self.mask_comp
                } else {
                    0
                };
                let comp = opt_u32(args, "component")?.unwrap_or(current as u32) as usize;
                if !self.remove_mask_component(slot, comp) {
                    return Err("这一组只剩一个形状；删整张蒙版请用 mask.remove".to_owned());
                }
                (
                    json!(comp + 1),
                    crate::i18n::tr_fmt(
                        "已删掉第 {} 张蒙版里的第 {} 个形状",
                        &[&index, &(comp + 1)],
                    ),
                )
            }
            other => {
                return Err(crate::i18n::tr_fmt(
                    "未知动作 {}；可用 add / remove",
                    &[&other],
                ));
            }
        };
        self.status = message;
        let mut out = self.mask_list_json();
        if let Some(object) = out.as_object_mut() {
            object.insert(action.to_owned(), detail);
        }
        Ok(out)
    }
}
