//! 图库浏览与整理：图库的扫描与元数据落盘、整理与堆叠、集合行、缩略图预热与关键帧导航。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Browsing and organising the library: Scanning the library and persisting its metadata, organising and stacking, collection rows, thumbnail warm-up and keyframe navigation.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

/// 图库缩略图在飞标记的存活上限——与 `ops_cache` 的三处缓存同一口径
/// （10 秒没回包就重新派发）。
///
/// Lifetime cap for in-flight library thumbnails -- the same rule the three caches in
/// `ops_cache` use (re-dispatch when nothing comes back within ten seconds).
const LIB_IN_FLIGHT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 清掉超时的图库缩略图在飞标记。
///
/// Drops expired in-flight library-thumbnail markers.
fn expire_lib_in_flight(map: &mut HashMap<PathBuf, std::time::Instant>) {
    let now = std::time::Instant::now();
    map.retain(|_, at| now.duration_since(*at) < LIB_IN_FLIGHT_TIMEOUT);
}

impl AppState {
    // ---------------------------------------------------------------- 图库

    pub fn lib_save(&self) {
        let store = aevum_core::library::Store {
            roots: self.lib_roots.clone(),
            collections: self.lib_cols.clone(),
        };
        let _ = aevum_core::library::save(&aevum_core::library::default_path(), &store);
    }

    /// 展开一个根目录：编目（含评级/色标/旗标）从该目录自己的工程文件折出来。
    ///
    /// Expand one root folder: the catalogue (with rating / colour label / flag) is folded out
    /// of that folder's own project file.
    pub fn lib_scan(&mut self, index: usize) {
        while self.lib_entries.len() < self.lib_roots.len() {
            self.lib_entries.push(Vec::new());
        }
        let Some(dir) = self.lib_roots.get(index).cloned() else {
            return;
        };
        self.lib_entries[index] = aevum_core::library::scan_root(&dir).unwrap_or_default();
        self.lib_sel = Some(index);
        self.lib_sel_file = None;
        self.lib_checked.clear();
        self.lib_anchor = None;
        self.lib_col = None;
        self.lib_load_meta(index);
    }

    /// 把某个目录工程里的说明与版权读进信息面板的编辑缓冲。
    /// 只在展开/重扫目录时调用：面板每帧重绘，逐帧读会把正在输入的内容冲掉。
    ///
    /// Read a folder project's description and copyright into the info panel's editing buffer.
    /// Called only when expanding or rescanning a folder: the panel repaints every frame, and
    /// reading per frame would wipe what is being typed.
    pub fn lib_load_meta(&mut self, index: usize) {
        let Some(dir) = self.lib_roots.get(index).cloned() else {
            return;
        };
        self.lib_meta = aevum_core::library::metadata(&dir);
        self.lib_meta_root = Some(dir);
    }

    /// 信息面板的「存」：写回该目录的工程文件，其余字段原样保留。
    ///
    /// The info panel's Save: writes back to that folder's project file and keeps every other
    /// field as it is.
    pub fn lib_save_meta(&mut self) -> Result<(), String> {
        let dir = self
            .lib_meta_root
            .clone()
            .ok_or_else(|| "还没选定图库目录".to_owned())?;
        let saved = aevum_core::library::set_metadata(&dir, &self.lib_meta)
            .map_err(|e| crate::i18n::tr_fmt("写元数据失败：{}", &[&e]))?;
        self.lib_meta = saved.clone();
        // 这个目录正好开着：内存那份也得跟上，否则自动保存会用旧值把它盖回去。
        if self.seq.as_ref().is_some_and(|s| s.root == dir) {
            self.meta = saved.clone();
        }
        self.status = if saved.is_empty() {
            "已清空这个目录的说明与版权".to_owned()
        } else {
            "说明与版权已写进这个目录的工程文件".to_owned()
        };
        Ok(())
    }

    /// 图库加目录：同一目录只留一行并挪到末尾，随即展开编目。
    ///
    /// Add a folder to the library: the same folder keeps one row and moves to the end, then
    /// its catalogue is expanded immediately.
    pub fn lib_add(&mut self, dir: &Path) -> usize {
        let index = aevum_core::library::add(&mut self.lib_roots, dir);
        while self.lib_entries.len() < self.lib_roots.len() {
            self.lib_entries.push(Vec::new());
        }
        self.lib_save();
        self.lib_scan(index);
        index
    }

    pub fn lib_remove(&mut self, index: usize) {
        if aevum_core::library::remove(&mut self.lib_roots, index).is_some() {
            if index < self.lib_entries.len() {
                self.lib_entries.remove(index);
            }
            self.lib_sel = None;
            self.lib_sel_file = None;
            self.lib_checked.clear();
            self.lib_anchor = None;
            // 目录序号整体前移，展开集里的键会对到别的目录：一次清空最省事。
            self.lib_stack_open.clear();
            self.lib_save();
        }
    }

    /// 在图库里直接改一张的评级/旗标/色标：写回该目录自己的工程文件，
    /// 内存编目同步改，不重扫整个目录。
    ///
    /// Change one image's rating / flag / colour label straight from the library: written back
    /// to that folder's own project file, with the in-memory catalogue updated in step and no
    /// whole-folder rescan.
    pub fn lib_organize(
        &mut self,
        root: usize,
        frame: usize,
        patch: aevum_core::library::Organize,
    ) -> Result<(), String> {
        let Some(dir) = self.lib_roots.get(root).cloned() else {
            return Err(crate::i18n::tr_fmt(
                "目录序号超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        };
        aevum_core::library::organize(&dir, frame, &patch)
            .map_err(|e| crate::i18n::err_text(&e))?;
        if let Some(f) = self
            .lib_entries
            .get_mut(root)
            .and_then(|v| v.get_mut(frame))
        {
            if let Some(rating) = patch.rating {
                f.rating = rating.min(5);
            }
            if let Some(flag) = patch.flag {
                f.flag = flag.signum();
            }
            if let Some(label) = patch.label {
                f.label = label.min(4);
            }
        }
        Ok(())
    }

    /// 批量改一类帧：整批一次落盘，内存编目同步改。
    ///
    /// Change a class of frames in bulk: one write for the whole batch, with the in-memory
    /// catalogue updated in step.
    pub fn lib_organize_many(
        &mut self,
        root: usize,
        frames: &[usize],
        patch: aevum_core::library::Organize,
    ) -> Result<(), String> {
        let Some(dir) = self.lib_roots.get(root).cloned() else {
            return Err(crate::i18n::tr_fmt(
                "目录序号超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        };
        if frames.is_empty() {
            return Err("没有选中的照片".to_owned());
        }
        aevum_core::library::organize_many(&dir, frames, &patch)
            .map_err(|e| crate::i18n::err_text(&e))?;
        if let Some(entries) = self.lib_entries.get_mut(root) {
            for &frame in frames {
                let Some(f) = entries.get_mut(frame) else {
                    continue;
                };
                if let Some(rating) = patch.rating {
                    f.rating = rating.min(5);
                }
                if let Some(flag) = patch.flag {
                    f.flag = flag.signum();
                }
                if let Some(label) = patch.label {
                    f.label = label.min(4);
                }
            }
        }
        Ok(())
    }

    /// 编目行被单击：普通点选一行，Ctrl 切换勾选，Shift 从锚点连选。
    ///
    /// A catalogue row was clicked: a plain click selects one row, Ctrl toggles its check
    /// mark, Shift extends from the anchor.
    pub fn lib_click(&mut self, row: usize, ctrl: bool, shift: bool) {
        if shift && let Some(anchor) = self.lib_anchor {
            let (lo, hi) = (anchor.min(row), anchor.max(row));
            self.lib_checked.extend(lo..=hi);
            self.lib_sel_file = Some(row);
            return;
        }
        if ctrl {
            if !self.lib_checked.insert(row) {
                self.lib_checked.remove(&row);
            }
            self.lib_sel_file = Some(row);
            self.lib_anchor = Some(row);
            return;
        }
        self.lib_checked.clear();
        self.lib_checked.insert(row);
        self.lib_sel_file = Some(row);
        self.lib_anchor = Some(row);
    }

    /// 键盘/右键的目标行：有勾选就作用于全部勾选，否则只作用于选中行。
    ///
    /// The rows the keyboard and context menu act on: every checked row when there are any,
    /// otherwise just the selected row.
    pub fn lib_targets(&self) -> Vec<(usize, usize)> {
        let Some(root) = self.lib_sel else {
            return Vec::new();
        };
        if self.lib_checked.is_empty() {
            return self
                .lib_sel_file
                .map(|f| vec![(root, f)])
                .unwrap_or_default();
        }
        self.lib_checked.iter().map(|&f| (root, f)).collect()
    }

    /// 对当前目标行套一次整理状态；返回改了几张。
    ///
    /// Apply one organise state to the current target rows; returns how many images
    /// changed.
    pub fn lib_organize_targets(
        &mut self,
        patch: aevum_core::library::Organize,
    ) -> Result<usize, String> {
        let targets = self.lib_targets();
        let Some(&(root, _)) = targets.first() else {
            return Err("还没选中照片".to_owned());
        };
        let frames: Vec<usize> = targets.iter().map(|&(_, f)| f).collect();
        self.lib_organize_many(root, &frames, patch)?;
        Ok(frames.len())
    }

    /// 给目标行加/去关键词（勾选优先）；整批只读写一次工程文件。
    ///
    /// Add / remove keywords on the target rows (checked rows take priority); the project file
    /// is read and written once for the whole batch.
    pub fn lib_keyword_edit(&mut self, add: &[String], remove: &[String]) -> Result<usize, String> {
        let targets = self.lib_targets();
        let Some(&(root, _)) = targets.first() else {
            return Err("还没选中照片".to_owned());
        };
        let frames: Vec<usize> = targets.iter().map(|&(_, f)| f).collect();
        self.lib_keyword_edit_many(root, &frames, add, remove)
    }

    pub fn lib_keyword_edit_many(
        &mut self,
        root: usize,
        frames: &[usize],
        add: &[String],
        remove: &[String],
    ) -> Result<usize, String> {
        let Some(dir) = self.lib_roots.get(root).cloned() else {
            return Err(crate::i18n::tr_fmt(
                "目录序号超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        };
        if frames.is_empty() {
            return Err("没有选中的照片".to_owned());
        }
        let touched = aevum_core::library::keyword_edit(&dir, frames, add, remove)
            .map_err(|e| crate::i18n::err_text(&e))?;
        // 内存编目同步：词表归一化、排序的规则只在 core 里有一份，改完重读一次工程。
        let proj =
            aevum_core::project::load(&aevum_core::project::project_path(&dir)).unwrap_or_default();
        if let Some(entries) = self.lib_entries.get_mut(root) {
            for &frame in frames {
                if let Some(f) = entries.get_mut(frame) {
                    f.keywords = proj.keywords.get(&frame).cloned().unwrap_or_default();
                }
            }
        }
        Ok(touched)
    }

    /// 堆叠改完之后把结果同步回内存：编目行的 `stack` 全部重算（叠号会整体挪），
    /// 打开着的序列正好在这个目录时 `self.stacks` 也跟着走，否则自动保存会用旧值盖回去。
    fn lib_stack_sync(&mut self, root: usize, stacks: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        if let Some(entries) = self.lib_entries.get_mut(root) {
            for f in entries.iter_mut() {
                f.stack = aevum_core::library::stack_of(&stacks, f.index);
            }
        }
        // 叠号会整体挪动，旧的展开集对不上新叠：这个目录一律收回折叠态。
        self.lib_stack_open.retain(|&(r, _)| r != root);
        if let Some(dir) = self.lib_roots.get(root)
            && self.seq.as_ref().is_some_and(|s| &s.root == dir)
        {
            self.stacks = stacks.clone();
        }
        stacks
    }

    pub(super) fn lib_dir(&self, root: usize) -> Result<PathBuf, String> {
        self.lib_roots.get(root).cloned().ok_or_else(|| {
            crate::i18n::tr_fmt("目录序号超出范围：1..={}", &[&self.lib_roots.len()])
        })
    }

    /// 并成一叠：写一次盘，返回归一化后的全部堆叠。少于两张由调用方拦。
    ///
    /// Merge into a stack: one disk write, returns all stacks after normalisation. Fewer than
    /// two images is rejected by the caller.
    pub fn lib_stack_create(
        &mut self,
        root: usize,
        frames: &[usize],
    ) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let stacks = aevum_core::library::stack_create(&dir, frames)
            .map_err(|e| crate::i18n::err_text(&e))?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 拆散一叠（叠号 0 起）；越界当没这叠。
    ///
    /// Dissolve one stack (0-based stack index); out of range is treated as no such stack.
    pub fn lib_stack_dissolve(
        &mut self,
        root: usize,
        stack: usize,
    ) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let stacks = aevum_core::library::stack_dissolve(&dir, stack)
            .map_err(|e| crate::i18n::err_text(&e))?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 按文件名前缀自动分叠（`A001_0001`…/`B002_0001`… 各成一叠）：整目录重写堆叠，
    /// 评级、关键词、元数据一律不动。
    ///
    /// Group automatically by filename prefix (`A001_0001`..., `B002_0001`... each become a
    /// stack): rewrites the stacks of the whole folder and leaves ratings, keywords and
    /// metadata untouched.
    pub fn lib_stack_auto(&mut self, root: usize) -> Result<Vec<Vec<usize>>, String> {
        let dir = self.lib_dir(root)?;
        let names: Vec<String> = match self.lib_entries.get(root) {
            Some(entries) if !entries.is_empty() => {
                entries.iter().map(|f| f.name.clone()).collect()
            }
            _ => return Err("这个目录还没编目：先展开它".to_owned()),
        };
        let stacks =
            aevum_core::library::stack_auto(&dir, &names).map_err(|e| crate::i18n::err_text(&e))?;
        Ok(self.lib_stack_sync(root, stacks))
    }

    /// 把目标行（勾选优先）并成一叠；返回这一叠的张数。
    ///
    /// Merge the target rows (checked rows take priority) into one stack; returns its size.
    pub fn lib_stack_targets(&mut self) -> Result<usize, String> {
        let targets = self.lib_targets();
        let Some(&(root, first)) = targets.first() else {
            return Err("还没选中照片".to_owned());
        };
        let frames: Vec<usize> = targets.iter().map(|&(_, f)| f).collect();
        if frames.len() < 2 {
            return Err("堆叠至少要选两张照片（勾住它们再试）".to_owned());
        }
        let stacks = self.lib_stack_create(root, &frames)?;
        Ok(stacks
            .get(aevum_core::library::stack_of(&stacks, first).unwrap_or(usize::MAX))
            .map(|g| g.len())
            .unwrap_or(0))
    }

    /// 拆散目标行所在的那一叠；返回被放出来的张数。目标行不在叠里就是错。
    ///
    /// Dissolve the stack the target rows belong to; returns how many images were released.
    /// An error when the target rows are in no stack.
    pub fn lib_stack_dissolve_targets(&mut self) -> Result<usize, String> {
        let targets = self.lib_targets();
        let Some(&(root, first)) = targets.first() else {
            return Err("还没选中照片".to_owned());
        };
        let Some(stack) = self
            .lib_entries
            .get(root)
            .and_then(|entries| entries.get(first))
            .and_then(|f| f.stack)
        else {
            return Err("这张照片不在任何叠里".to_owned());
        };
        let members = self
            .lib_entries
            .get(root)
            .map(|entries| entries.iter().filter(|f| f.stack == Some(stack)).count())
            .unwrap_or(0);
        self.lib_stack_dissolve(root, stack)?;
        Ok(members)
    }

    /// 选中的编目行 →（目录序号, 帧号, 文件），键盘与「加入集合」共用。
    ///
    /// The selected catalogue row -> (folder index, frame index, file); shared by the keyboard
    /// and "add to collection".
    pub fn lib_selected(&self) -> Option<(usize, usize, aevum_core::library::LibFile)> {
        let (root, frame) = (self.lib_sel?, self.lib_sel_file?);
        let f = self.lib_entries.get(root)?.get(frame)?.clone();
        Some((root, frame, f))
    }

    /// 编目里上下挪选中行；跳过被筛掉的行。
    ///
    /// Move the selected row up or down in the catalogue, skipping filtered-out rows.
    pub fn lib_step_row(&mut self, delta: i64) {
        let Some(root) = self.lib_sel else { return };
        let Some(entries) = self.lib_entries.get(root) else {
            return;
        };
        let n = entries.len() as i64;
        if n == 0 {
            return;
        }
        let step = delta.signum();
        let mut i = self
            .lib_sel_file
            .map(|v| v as i64)
            .unwrap_or(if step > 0 { -1 } else { n });
        loop {
            i += step;
            if i < 0 || i >= n {
                return;
            }
            if self.lib_filter.keeps(&entries[i as usize]) {
                self.lib_sel_file = Some(i as usize);
                self.lib_checked.clear();
                self.lib_anchor = Some(i as usize);
                return;
            }
        }
    }

    pub fn lib_collection_create(&mut self, name: &str) -> usize {
        let index = aevum_core::library::collection_create(&mut self.lib_cols, name);
        self.lib_save();
        index
    }

    pub fn lib_collection_delete(&mut self, index: usize) -> bool {
        let deleted = aevum_core::library::collection_delete(&mut self.lib_cols, index).is_some();
        if deleted {
            self.lib_col = None;
            self.lib_save();
        }
        deleted
    }

    /// 把一张照片放进集合；已存在返回 `false`。
    ///
    /// Put one photo into a collection; returns `false` when it is already there.
    pub fn lib_collection_put(&mut self, col: usize, path: &Path) -> Result<bool, String> {
        if col >= self.lib_cols.len() {
            return Err(crate::i18n::tr_fmt(
                "集合序号超出范围：1..={}",
                &[&self.lib_cols.len()],
            ));
        }
        let added = aevum_core::library::collection_put(&mut self.lib_cols, col, path);
        if added {
            self.lib_save();
        }
        Ok(added)
    }

    /// 一次把几张放进集合；返回新加进去的张数（已在里面的不算）。
    ///
    /// Put several photos into a collection at once; returns how many were newly added (ones
    /// already inside do not count).
    pub fn lib_collection_put_many(&mut self, col: usize, paths: &[PathBuf]) -> usize {
        let mut added = 0;
        for path in paths {
            if aevum_core::library::collection_put(&mut self.lib_cols, col, path) {
                added += 1;
            }
        }
        if added > 0 {
            self.lib_save();
        }
        added
    }

    pub fn lib_collection_drop(&mut self, col: usize, path: &Path) -> bool {
        let dropped = aevum_core::library::collection_drop(&mut self.lib_cols, col, path);
        if dropped {
            self.lib_save();
        }
        dropped
    }

    /// 集合视图的行：成员路径先在已编目的根目录里找（评级与帧号跟着来），
    /// 找不到的给一行只带路径的占位——目录还没展开过时也能看集合有哪些照片。
    ///
    /// 查找先建一次 `HashMap`（路径 → 编目条目）再逐行取：集合视图每帧重画，
    /// 成员 × 编目规模的线性 `find` 一帧跑一遍就是 O(成员×编目) 的白功。
    ///
    /// The rows of the collection view: member paths are looked up in the catalogued roots
    /// first (so ratings and frame indices come along), and misses get a path-only placeholder
    /// row -- the collection's contents are visible even before a folder has ever been
    /// expanded.
    ///
    /// The lookup builds one `HashMap` (path → catalogued entry) up front and reads rows
    /// from it: the view repaints every frame, and a linear `find` per member over the
    /// catalogue is O(members x catalogue) of wasted work per frame.
    pub fn lib_collection_rows(&self, col: usize) -> Vec<aevum_core::library::LibFile> {
        let Some(collection) = self.lib_cols.get(col) else {
            return Vec::new();
        };
        let index: std::collections::HashMap<&Path, &aevum_core::library::LibFile> = self
            .lib_entries
            .iter()
            .flatten()
            .map(|f| (f.path.as_path(), f))
            .collect();
        collection
            .files
            .iter()
            .map(|path| {
                let path = PathBuf::from(path);
                index
                    .get(path.as_path())
                    .map(|f| (*f).clone())
                    .unwrap_or_else(|| aevum_core::library::LibFile {
                        name: path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string()),
                        path,
                        index: 0,
                        rating: 0,
                        label: 0,
                        flag: 0,
                        keywords: Vec::new(),
                        // 集合是跨目录的路径清单，没有"这个目录里的第几叠"可言。
                        stack: None,
                    })
            })
            .collect()
    }

    /// 图库可见行的缩略图：按路径键，后台线程与序列缩略图共用同一个池。
    ///
    /// Thumbnails for the visible library rows: keyed by path, and the background thread
    /// shares the same pool as the sequence thumbnails.
    pub fn lib_ensure_thumbs(&mut self, ctx: &Context, lo: usize, hi: usize) {
        let Some(index) = self.lib_sel else { return };
        let Some(entries) = self.lib_entries.get(index) else {
            return;
        };
        let hi = hi.min(entries.len());
        let paths: Vec<PathBuf> = entries
            .get(lo..hi)
            .unwrap_or(&[])
            .iter()
            .map(|f| f.path.clone())
            .collect();
        self.lib_ensure_thumb_paths(ctx, paths);
    }

    /// 任意一批照片的缩略图（集合视图也走这里）。
    ///
    /// Thumbnails for an arbitrary batch of photos (the collection view goes through here
    /// too).
    pub fn lib_ensure_thumb_paths(&mut self, ctx: &Context, paths: Vec<PathBuf>) {
        if self.headless {
            return;
        }
        // 僵尸自愈：派发超时的路径视为丢失，允许重新派发（否则解码失败一次
        // 就永远没有缩略图）。
        //
        // Zombie self-heal: paths dispatched too long ago count as lost and may be
        // re-dispatched -- otherwise one failed decode means no thumbnail forever.
        expire_lib_in_flight(&mut self.lib_thumb_pending);
        let mut missing: Vec<PathBuf> = Vec::new();
        for path in paths {
            if self.lib_thumbs.contains_key(&path) || self.lib_thumb_pending.contains_key(&path) {
                continue;
            }
            missing.push(path);
            if missing.len() >= 24 {
                break;
            }
        }
        if missing.is_empty() {
            return;
        }
        let dispatched = std::time::Instant::now();
        for p in &missing {
            self.lib_thumb_pending.insert(p.clone(), dispatched);
        }
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let out: Vec<LibThumbMsg> = thumb_pool().install(|| {
                missing
                    .into_par_iter()
                    .filter_map(|path| {
                        let img = metrics::thumbnail(&path, THUMB_SIDE).ok()?;
                        let size = [img.width(), img.height()];
                        let image = ColorImage::from_rgba_unmultiplied(
                            [size[0] as usize, size[1] as usize],
                            img.as_raw(),
                        );
                        Some(LibThumbMsg { path, image, size })
                    })
                    .collect()
            });
            let _ = tx.send(Msg::LibThumbs(out));
            ctx2.request_repaint();
        });
    }

    pub fn keyframe_near(&self, tol: usize) -> Option<usize> {
        self.keyframes
            .keys()
            .find(|k| (**k as i64 - self.selected as i64).unsigned_abs() as usize <= tol)
            .copied()
    }

    pub fn add_keyframe(&mut self) {
        self.keyframes.insert(self.selected, self.params.clone());
        history::note(
            "keyframe-add",
            crate::i18n::tr_fmt("记录关键帧 帧{}", &[&format!("{:04}", self.selected + 1)]),
        );
    }

    pub fn remove_keyframe(&mut self, i: usize) {
        if self.keyframes.remove(&i).is_some() {
            history::note(
                "keyframe-del",
                crate::i18n::tr_fmt("删除关键帧 帧{}", &[&format!("{:04}", i + 1)]),
            );
        }
    }

    pub fn goto_keyframe(&mut self, i: usize) {
        if let Some(p) = self.keyframes.get(&i).cloned() {
            self.selected = i.min(self.frame_count().saturating_sub(1));
            self.params = p;
            self.preset = "关键帧".to_owned();
            self.preview_dirty = true;
            self.scroll_to_selected = true;
            history::note(
                "keyframe-goto",
                crate::i18n::tr_fmt("套用关键帧 帧{}", &[&format!("{:04}", i + 1)]),
            );
        }
    }
}
