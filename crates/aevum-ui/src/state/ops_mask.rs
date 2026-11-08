//! 局部蒙版的编辑动作：蒙版的选中与增删、组件增删，以及「蒙版变了」的标脏。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Local mask edits: Selecting, adding and removing masks and their components, and marking the mask state dirty.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ 局部蒙版

    /// 新建一张单组件蒙版并选中；返回 0 基序号。
    ///
    /// Create a single-component mask and select it; returns its 0-based index.
    pub fn add_mask(&mut self, kind: &str) -> usize {
        let shape = match kind {
            "radial" => mask::ShapeKind::Radial,
            "brush" => mask::ShapeKind::Brush,
            _ => mask::ShapeKind::Linear,
        };
        self.params.masks.push(mask::new_mask(shape));
        let index = self.params.masks.len() - 1;
        self.select_mask(index);
        self.masks_changed();
        index
    }

    pub fn remove_mask(&mut self, index: usize) -> bool {
        if index >= self.params.masks.len() {
            return false;
        }
        self.params.masks.remove(index);
        self.mask_sel = match self.mask_sel {
            Some(sel) if sel == index => {
                if self.params.masks.is_empty() {
                    None
                } else {
                    Some(sel.min(self.params.masks.len() - 1))
                }
            }
            Some(sel) if sel > index => Some(sel - 1),
            other => other,
        };
        if self.mask_drag.is_some_and(|(i, _)| i == index) {
            self.mask_drag = None;
            self.mask_move_anchor = None;
        }
        if self.mask_paint.is_some_and(|(i, _)| i == index) {
            self.mask_paint = None;
        }
        self.mask_comp = 0;
        self.masks_changed();
        true
    }

    /// 选中某张蒙版，并回到组内第一个形状。
    ///
    /// Select one mask and return to the first shape inside its group.
    pub fn select_mask(&mut self, index: usize) {
        self.mask_sel = Some(index);
        self.mask_comp = 0;
    }

    /// 组内加一个形状；扁平老格式先升级成单组件组。返回新形状的 0 基序号。
    ///
    /// Add a shape to a group; a legacy flat mask is upgraded to a single-component group
    /// first. Returns the new shape's 0-based index.
    pub fn add_mask_component(&mut self, index: usize, kind: &str) -> Option<usize> {
        if index >= self.params.masks.len() {
            return None;
        }
        let shape = match kind {
            "radial" => mask::ShapeKind::Radial,
            "brush" => mask::ShapeKind::Brush,
            _ => mask::ShapeKind::Linear,
        };
        let added = {
            let raw = &mut self.params.masks[index];
            if !raw
                .as_object()
                .is_some_and(|o| o.contains_key("components"))
            {
                *raw = mask::as_group(raw);
            }
            {
                let list = raw.get_mut("components").and_then(Value::as_array_mut)?;
                list.push(mask::new_component(shape));
                list.len() - 1
            }
        };
        if self.mask_sel == Some(index) {
            self.mask_comp = added;
        }
        if self.mask_drag.is_some_and(|(i, _)| i == index) {
            self.mask_drag = None;
            self.mask_move_anchor = None;
        }
        self.masks_changed();
        Some(added)
    }

    /// 删掉组内一个形状；只剩一个时不删，蒙版不能空组。
    ///
    /// Remove one shape from a group; the last one is kept, because a mask cannot be an
    /// empty group.
    pub fn remove_mask_component(&mut self, index: usize, comp: usize) -> bool {
        if index >= self.params.masks.len() {
            return false;
        }
        let left = {
            let Some(list) = self.params.masks[index]
                .get_mut("components")
                .and_then(Value::as_array_mut)
            else {
                return false;
            };
            if list.len() < 2 || comp >= list.len() {
                return false;
            }
            list.remove(comp);
            list.len()
        };
        if self.mask_sel == Some(index) && self.mask_comp >= left {
            self.mask_comp = left - 1;
        }
        if self.mask_drag.is_some_and(|(i, _)| i == index) {
            self.mask_drag = None;
            self.mask_move_anchor = None;
        }
        self.masks_changed();
        true
    }

    /// 蒙版改动后统一走这里：重画预览，并把预设标成自定义。
    ///
    /// The single path after any mask change: re-render the preview and mark the preset as
    /// custom.
    pub fn masks_changed(&mut self) {
        self.preview_dirty = true;
        // 指纹本来会自校验，这里整体清一道是保守兜底（宁可多失效）。
        self.mask_geo_cache = Default::default();
        if self.preset != "自定义" {
            self.preset = "自定义".to_owned();
        }
        // 蒙版的所有写路径（面板、画布手柄、命令）最后都汇到这里，历史记一处就够。
        history::note("mask", "改局部蒙版");
    }
}
