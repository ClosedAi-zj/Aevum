//! 调色副本：调色副本的索引、保存、套用、删除与改名。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Grade copies: Indexing, saving, applying, deleting and renaming grade copies.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ 调色副本

    pub fn copy_index(&self, name: &str) -> Option<usize> {
        self.copies.iter().position(|c| c.name == name)
    }

    /// 当前调色等于哪套副本：整表现算，不留一个改参后会过期的字段。
    ///
    /// Which copy the current grade equals: computed from the whole table every time, with no
    /// field that could go stale after a parameter change.
    pub fn active_copy(&self) -> Option<&str> {
        self.copies
            .iter()
            .find(|c| c.params == self.params && c.keyframes == self.keyframes)
            .map(|c| c.name.as_str())
    }

    pub fn save_copy(&mut self, name: &str, overwrite: bool) -> Result<usize, String> {
        let name = clean_copy_name(name)?;
        match self.copy_index(&name) {
            Some(_) if !overwrite => Err(crate::i18n::tr_fmt(
                "副本「{}」已存在；要覆盖给 overwrite: true",
                &[&name],
            )),
            Some(i) => {
                self.copies[i].params = self.params.clone();
                self.copies[i].keyframes = self.keyframes.clone();
                history::note("copy-save", crate::i18n::tr_fmt("覆盖副本 {}", &[&name]));
                Ok(i)
            }
            None => {
                self.copies.push(GradeCopy {
                    name: name.clone(),
                    params: self.params.clone(),
                    keyframes: self.keyframes.clone(),
                });
                history::note("copy-save", crate::i18n::tr_fmt("存副本 {}", &[&name]));
                Ok(self.copies.len() - 1)
            }
        }
    }

    pub fn apply_copy(&mut self, name: &str) -> Result<(), String> {
        let Some(copy) = self.copies.iter().find(|c| c.name == name).cloned() else {
            return Err(crate::i18n::tr_fmt("没有叫 {} 的副本", &[&name]));
        };
        self.params = copy.params;
        self.keyframes = copy.keyframes;
        // 存中文标识而不是译文：`preset` 会写进工程文件，也要和 `PRESET_NAMES`
        // 逐字比较，翻了之后中文会话里会看到上一次英文会话留下的 "Copy x"。
        // 显示时由 `i18n::tr_preset` 认「副本 」前缀再翻。
        self.preset = format!("副本 {}", copy.name);
        self.preview_dirty = true;
        history::note(
            "copy-apply",
            crate::i18n::tr_fmt("套用副本 {}", &[&copy.name]),
        );
        Ok(())
    }

    pub fn delete_copy(&mut self, name: &str) -> Result<(), String> {
        let Some(i) = self.copy_index(name) else {
            return Err(crate::i18n::tr_fmt("没有叫 {} 的副本", &[&name]));
        };
        self.copies.remove(i);
        history::note("copy-del", crate::i18n::tr_fmt("删副本 {}", &[&name]));
        Ok(())
    }

    pub fn rename_copy(&mut self, from: &str, to: &str) -> Result<(), String> {
        let to = clean_copy_name(to)?;
        let Some(i) = self.copy_index(from) else {
            return Err(crate::i18n::tr_fmt("没有叫 {} 的副本", &[&from]));
        };
        if self.copy_index(&to).is_some() {
            return Err(crate::i18n::tr_fmt("副本「{}」已存在", &[&to]));
        }
        self.copies[i].name = to.clone();
        history::note(
            "copy-rename",
            crate::i18n::tr_fmt("副本 {} 改名 {}", &[&from, &to]),
        );
        Ok(())
    }

    pub(super) fn unload_jobs(&mut self) {
        self.cancel_export.store(true, Ordering::SeqCst);
        self.preview_inflight = false;
        self.align_busy = false;
        self.export_busy = false;
        self.export_stage = None;
        self.align_done.store(0, Ordering::Relaxed);
        self.export_done.store(0, Ordering::Relaxed);
        self.export_total.store(0, Ordering::Relaxed);
        self.export_stage_shared.store(0, Ordering::Relaxed);
        // 插件在飞也要收：换序列时旧结果没有意义，让任务自己撞取消旗。
        if self.plugin_job.is_some() {
            self.plugin_cancel();
        }
    }
}
