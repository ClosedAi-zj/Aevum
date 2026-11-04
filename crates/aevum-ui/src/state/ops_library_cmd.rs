//! 图库与集合命令：图库的增删查开、关键词与元数据、堆叠与整理，以及集合的增删改查。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Library and collection commands: Adding, removing, listing and opening library entries; keywords and metadata; stacking and organising; plus collection CRUD.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ---------------------------------------------------------------- 图库命令

    pub(super) fn cmd_library_add(&mut self, args: &Value) -> Result<Value, String> {
        let raw = arg_str(args, "path")?;
        let path = PathBuf::from(raw);
        if !path.is_dir() {
            return Err(crate::i18n::tr_fmt("不是目录：{}", &[&path.display()]));
        }
        let index = self.lib_add(&path);
        let summary = self
            .lib_entries
            .get(index)
            .map(|f| aevum_core::library::summarize(f))
            .unwrap_or_default();
        self.status = crate::i18n::tr_fmt("图库加目录：{}", &[&path.display()]);
        Ok(json!({
            "index": index + 1,
            "roots": self.lib_roots.len(),
            "summary": summary,
        }))
    }

    pub(super) fn cmd_library_remove(&mut self, args: &Value) -> Result<Value, String> {
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        if index == 0 || index as usize > self.lib_roots.len() {
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        }
        let path = self.lib_roots[index as usize - 1].display().to_string();
        self.lib_remove(index as usize - 1);
        self.status = crate::i18n::tr_fmt("图库移除目录：{}", &[&path]);
        Ok(json!({"removed": path, "roots": self.lib_roots.len()}))
    }

    pub(super) fn cmd_library_list(&mut self, args: &Value) -> Result<Value, String> {
        let scan = opt_u32(args, "index")?;
        if let Some(n) = scan {
            if n == 0 || n as usize > self.lib_roots.len() {
                return Err(crate::i18n::tr_fmt(
                    "index 超出范围：1..={}",
                    &[&self.lib_roots.len()],
                ));
            }
            self.lib_scan(n as usize - 1);
        }
        let roots: Vec<Value> = self
            .lib_roots
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let files = self.lib_entries.get(i);
                let mut row = json!({
                    "index": i + 1,
                    "path": r.display().to_string(),
                    "name": r
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| r.display().to_string()),
                    "scanned": files.is_some_and(|f| !f.is_empty()),
                });
                if let Some(f) = files.filter(|f| !f.is_empty()) {
                    row["summary"] = json!(aevum_core::library::summarize(f));
                    if scan == Some(i as u32 + 1) {
                        row["files"] = json!(
                            f.iter()
                                .map(|e| json!({
                                    "frame": e.index + 1,
                                    "name": e.name,
                                    "path": e.path.display().to_string(),
                                    "rating": e.rating,
                                    "flag": e.flag,
                                    "label": e.label,
                                    "keywords": e.keywords,
                                }))
                                .collect::<Vec<_>>()
                        );
                        row["keywords"] = json!(
                            aevum_core::library::keyword_counts(f)
                                .into_iter()
                                .map(|(w, n)| json!({"keyword": w, "count": n}))
                                .collect::<Vec<_>>()
                        );
                    }
                }
                row
            })
            .collect();
        let collections: Vec<Value> = self
            .lib_cols
            .iter()
            .enumerate()
            .map(|(i, c)| json!({"index": i + 1, "name": c.name, "count": c.files.len()}))
            .collect();
        Ok(json!({
            "roots": roots,
            "collections": collections,
            "selected": self.lib_sel.map(|i| i + 1),
        }))
    }

    pub(super) fn cmd_library_open(
        &mut self,
        ctx: &Context,
        args: &Value,
    ) -> Result<Value, String> {
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        if index == 0 || index as usize > self.lib_roots.len() {
            return Err(crate::i18n::tr_fmt(
                "index 超出范围：1..={}",
                &[&self.lib_roots.len()],
            ));
        }
        let dir = self.lib_roots[index as usize - 1].clone();
        // 与 sequence.open 同一套口径：reload_token 没动说明打开失败，
        // 别把旧序列当成新目录回给 agent。
        let token = self.reload_token;
        self.open(ctx, dir);
        if self.reload_token == token {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "打开失败：这里没有可用的图片".to_owned()));
        }
        let mut info = self.cmd_sequence_info()?;
        if let Some(object) = info.as_object_mut() {
            object.insert("opened".to_owned(), json!(true));
        }
        Ok(info)
    }

    /// 图库里的根目录序号：省略就用当前展开的那个。
    fn lib_root_arg(&self, args: &Value) -> Result<usize, String> {
        match opt_u32(args, "root")? {
            Some(0) => Err("root 从 1 开始".to_owned()),
            Some(n) if n as usize > self.lib_roots.len() => Err(crate::i18n::tr_fmt(
                "root 超出范围：1..={}",
                &[&self.lib_roots.len()],
            )),
            Some(n) => Ok(n as usize - 1),
            None => self
                .lib_sel
                .ok_or_else(|| "还没展开图库目录：先给 root，或 library.list 带 index".to_owned()),
        }
    }

    /// 编目里的帧号（1 开始）→（目录序号, 帧序），顺带校验帧数。
    fn lib_frame_arg(&self, args: &Value, root: usize) -> Result<(usize, usize), String> {
        let frame =
            opt_u32(args, "frame")?.ok_or_else(|| "缺少整数参数 frame（从 1 开始）".to_owned())?;
        let count = self.lib_entries.get(root).map(|v| v.len()).unwrap_or(0);
        if count == 0 {
            return Err("这个目录还没编目：先 library.list 带 index".to_owned());
        }
        if frame == 0 || frame as usize > count {
            return Err(crate::i18n::tr_fmt("frame 超出范围：1..={}", &[&count]));
        }
        Ok((root, frame as usize - 1))
    }

    /// `frame`（单帧）或 `frames`（数组）→ 0 开始的帧号；两者只能给一个。
    fn lib_frames_arg(&self, args: &Value, root: usize) -> Result<Vec<usize>, String> {
        match args.get("frames") {
            None | Some(Value::Null) => {
                let (_, frame) = self.lib_frame_arg(args, root)?;
                Ok(vec![frame])
            }
            Some(Value::Array(raw)) => {
                if !matches!(args.get("frame"), None | Some(Value::Null)) {
                    return Err("frame 与 frames 只能给一个".to_owned());
                }
                if raw.is_empty() {
                    return Err("frames 不能是空数组".to_owned());
                }
                let count = self.lib_entries.get(root).map(|v| v.len()).unwrap_or(0);
                if count == 0 {
                    return Err("这个目录还没编目：先 library.list 带 index".to_owned());
                }
                raw.iter()
                    .map(|v| {
                        v.as_u64()
                            .filter(|&n| n >= 1 && n as usize <= count)
                            .map(|n| n as usize - 1)
                            .ok_or_else(|| {
                                crate::i18n::tr_fmt("frames 里的帧号超出范围：1..={}", &[&count])
                            })
                    })
                    .collect()
            }
            Some(_) => Err("frames 必须是帧号数组".to_owned()),
        }
    }

    pub(super) fn cmd_library_keyword(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.lib_root_arg(args)?;
        let frames = self.lib_frames_arg(args, root)?;
        let add = words_arg(args, "add")?;
        let remove = words_arg(args, "remove")?;
        if add.is_empty() && remove.is_empty() {
            return Err("至少给 add 或 remove（关键词）".to_owned());
        }
        let touched = self.lib_keyword_edit_many(root, &frames, &add, &remove)?;
        let words = self
            .lib_entries
            .get(root)
            .and_then(|v| v.get(frames[0]))
            .map(|f| f.keywords.clone())
            .unwrap_or_default();
        self.status = crate::i18n::tr_fmt(
            "图库：{} 张改词（+{} / -{}），第 {} 帧现有 {} 个",
            &[
                &touched,
                &add.len(),
                &remove.len(),
                &(frames[0] + 1),
                &words.len(),
            ],
        );
        Ok(json!({
            "root": root + 1,
            "frames": frames.iter().map(|f| f + 1).collect::<Vec<_>>(),
            "touched": touched,
            "keywords": words,
        }))
    }

    /// 图库里某个目录的说明与版权：不带参数是查询，带了就改那几项并立刻落盘。
    /// 目录正好是打开着的序列时以内存那份为准，改完两边同步——不然面板上还没
    /// 自动保存的编辑会被这条命令读丢，或者被工程写回盖掉。
    pub(super) fn cmd_library_metadata(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.lib_root_arg(args)?;
        let dir = self
            .lib_roots
            .get(root)
            .cloned()
            .ok_or_else(|| "这个目录已经从图库移除了".to_owned())?;
        let open_here = self.seq.as_ref().is_some_and(|s| s.root == dir);
        let mut meta = if open_here {
            self.meta.clone()
        } else {
            aevum_core::library::metadata(&dir)
        };
        let changed = apply_metadata_args(&mut meta, args)?;
        if changed {
            meta = aevum_core::library::set_metadata(&dir, &meta)
                .map_err(|e| crate::i18n::tr_fmt("写元数据失败：{}", &[&e]))?;
            if open_here {
                self.meta = meta.clone();
            }
            if self.lib_meta_root.as_deref() == Some(dir.as_path()) {
                self.lib_meta = meta.clone();
            }
            // 目录名先算成局部变量：`tr_fmt` 只认 `{}`，把整条链式调用塞进实参
            // 会让这一行长得读不动。
            let dir_name = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| dir.display().to_string());
            self.status = crate::i18n::tr_fmt("图库：{} 的说明与版权已写入工程", &[&dir_name]);
        }
        let mut out = metadata_json(&meta);
        out["root"] = json!(root + 1);
        out["path"] = json!(project::project_path(&dir).display().to_string());
        out["saved"] = json!(changed);
        Ok(out)
    }

    /// 图库堆叠：`op` = list（默认）/ create / dissolve / auto，都原子写回该目录的工程文件。
    /// 叠号在返回里一律 1 开始，帧号也一样；create 会把成员从原叠里摘走，
    /// 老叠摘剩一张就自动消失，所以每次都要看返回的整份堆叠，别缓存旧叠号。
    pub(super) fn cmd_library_stack(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.lib_root_arg(args)?;
        let op = match args.get("op") {
            None | Some(Value::Null) => "list",
            Some(Value::String(s)) => s.as_str(),
            Some(_) => return Err("op 必须是字符串".to_owned()),
        };
        let dir = self.lib_dir(root)?;
        let stacks = match op {
            "list" => aevum_core::library::stacks(&dir),
            "create" => {
                let frames = self.lib_frames_arg(args, root)?;
                if frames.len() < 2 {
                    return Err("堆叠至少要两帧：用 frames 数组一次给全".to_owned());
                }
                self.lib_stack_create(root, &frames)?
            }
            "dissolve" => {
                let n = opt_u32(args, "stack")?
                    .ok_or_else(|| "dissolve 要带 stack（叠号，1 开始）".to_owned())?;
                if n == 0 {
                    return Err("stack 从 1 开始".to_owned());
                }
                self.lib_stack_dissolve(root, n as usize - 1)?
            }
            "auto" => self.lib_stack_auto(root)?,
            other => {
                return Err(crate::i18n::tr_fmt(
                    "认不出来的 op：{}（要 list / create / dissolve / auto）",
                    &[&other],
                ));
            }
        };
        if op != "list" {
            let dir_name = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| dir.display().to_string());
            self.status = crate::i18n::tr_fmt("图库：{} 现在有 {} 叠", &[&dir_name, &stacks.len()]);
        }
        let rows: Vec<Value> = stacks
            .iter()
            .enumerate()
            .map(|(i, group)| {
                json!({
                    "stack": i + 1,
                    "head": group[0] + 1,
                    "count": group.len(),
                    "frames": group.iter().map(|f| f + 1).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(json!({
            "root": root + 1,
            "op": op,
            "count": stacks.len(),
            "stacks": rows,
        }))
    }

    pub(super) fn cmd_library_organize(&mut self, args: &Value) -> Result<Value, String> {
        let root = self.lib_root_arg(args)?;
        let patch = self.organize_patch(args)?;
        let frames = self.lib_frames_arg(args, root)?;
        if let [frame] = frames[..] {
            self.lib_organize(root, frame, patch)?;
            let f = self
                .lib_entries
                .get(root)
                .and_then(|v| v.get(frame))
                .cloned()
                .ok_or_else(|| "编目行没了（目录可能已移除）".to_owned())?;
            // 标记与色标名都是中文表里的取值，各自过一遍 tr 再拼进句子。
            let flag = match f.flag {
                1 => "选用",
                -1 => "排除",
                _ => "未标记",
            };
            self.status = crate::i18n::tr_fmt(
                "图库 {}：{} 星 · {} · 色标{}",
                &[
                    &f.name,
                    &f.rating,
                    &crate::i18n::tr(flag),
                    &crate::i18n::tr(LABEL_NAMES[f.label.min(4) as usize]),
                ],
            );
            return Ok(json!({
                "root": root + 1,
                "frame": frame + 1,
                "name": f.name,
                "rating": f.rating,
                "flag": f.flag,
                "label": f.label,
            }));
        }
        // 批量的写回在 lib_organize_many 里一次落盘，这里只补内存与汇报。
        self.lib_organize_many(root, &frames, patch)?;
        let flag = match patch.flag.unwrap_or(0) {
            1 => "选用",
            -1 => "排除",
            _ => "未标记",
        };
        self.status = crate::i18n::tr_fmt(
            "图库：{} 张已整理（{} 星 · {} · 色标{}）",
            &[
                &frames.len(),
                &patch.rating.unwrap_or(0).min(5),
                &crate::i18n::tr(flag),
                &crate::i18n::tr(LABEL_NAMES[patch.label.unwrap_or(0).min(4) as usize]),
            ],
        );
        let frames: Vec<usize> = frames.iter().map(|f| f + 1).collect();
        Ok(json!({
            "root": root + 1,
            "frames": frames,
            "count": frames.len(),
        }))
    }

    /// `rating` / `flag` / `label` 三个整理参数：范围校验 + 至少给一个。
    fn organize_patch(&self, args: &Value) -> Result<aevum_core::library::Organize, String> {
        let rating = opt_u32(args, "rating")?;
        let label = opt_u32(args, "label")?;
        let flag = match args.get("flag") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_i64()
                    .filter(|f| (-1..=1).contains(f))
                    .ok_or_else(|| "flag 只能是 -1（排除）/ 0 / 1（选用）".to_owned())?,
            ),
        };
        if let Some(r) = rating
            && r > 5
        {
            return Err("rating 只能是 0~5".to_owned());
        }
        if let Some(l) = label
            && l > 4
        {
            return Err("label 只能是 0~4（0=无，1=红，2=黄，3=绿，4=蓝）".to_owned());
        }
        if rating.is_none() && label.is_none() && flag.is_none() {
            return Err("至少给 rating / flag / label 之一".to_owned());
        }
        Ok(aevum_core::library::Organize {
            rating: rating.map(|r| r as u8),
            flag: flag.map(|f| f as i8),
            label: label.map(|l| l as u8),
        })
    }

    /// 集合寻址：`index`（1 开始）或 `name` 二选一。
    fn collection_at(&self, args: &Value) -> Result<usize, String> {
        if let Some(n) = opt_u32(args, "index")? {
            if n == 0 || n as usize > self.lib_cols.len() {
                return Err(crate::i18n::tr_fmt(
                    "index 超出范围：1..={}",
                    &[&self.lib_cols.len()],
                ));
            }
            return Ok(n as usize - 1);
        }
        let name = opt_str(args, "name")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "缺少 index（1 开始）或 name（集合名）".to_owned())?;
        self.lib_cols
            .iter()
            .position(|c| c.name == name)
            .ok_or_else(|| {
                crate::i18n::tr_fmt("没有叫「{}」的集合：先 collection.create", &[&name])
            })
    }

    pub(super) fn cmd_collection_create(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let index = self.lib_collection_create(name);
        let name = self.lib_cols[index].name.clone();
        self.status = crate::i18n::tr_fmt("新建集合「{}」", &[&name]);
        Ok(json!({
            "index": index + 1,
            "name": name,
            "count": self.lib_cols[index].files.len(),
        }))
    }

    pub(super) fn cmd_collection_add(&mut self, args: &Value) -> Result<Value, String> {
        let col = self.collection_at(args)?;
        let root = self.lib_root_arg(args)?;
        let (root, frame) = self.lib_frame_arg(args, root)?;
        let path = self.lib_entries[root][frame].path.clone();
        let added = self.lib_collection_put(col, &path)?;
        let c = &self.lib_cols[col];
        self.status = if added {
            crate::i18n::tr_fmt("「{}」加入集合「{}」", &[&path.display(), &c.name])
        } else {
            crate::i18n::tr_fmt("「{}」已经在集合「{}」里", &[&path.display(), &c.name])
        };
        Ok(json!({
            "index": col + 1,
            "name": c.name,
            "added": added,
            "count": c.files.len(),
        }))
    }

    pub(super) fn cmd_collection_remove(&mut self, args: &Value) -> Result<Value, String> {
        let col = self.collection_at(args)?;
        // 成员按路径寻址：集合视图里还没编目的行也能拿掉。
        let path = match opt_str(args, "path")?
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(path) => PathBuf::from(path),
            None => {
                let root = self.lib_root_arg(args)?;
                let (root, frame) = self.lib_frame_arg(args, root)?;
                self.lib_entries[root][frame].path.clone()
            }
        };
        let removed = self.lib_collection_drop(col, &path);
        let c = &self.lib_cols[col];
        Ok(json!({
            "index": col + 1,
            "name": c.name,
            "removed": removed,
            "count": c.files.len(),
        }))
    }

    pub(super) fn cmd_collection_delete(&mut self, args: &Value) -> Result<Value, String> {
        let col = self.collection_at(args)?;
        let name = self
            .lib_cols
            .get(col)
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.lib_collection_delete(col);
        self.status = crate::i18n::tr_fmt("删除集合「{}」", &[&name]);
        Ok(json!({"deleted": name, "collections": self.lib_cols.len()}))
    }

    pub(super) fn cmd_collection_list(&mut self, args: &Value) -> Result<Value, String> {
        let open = match opt_u32(args, "index")? {
            Some(n) => Some(self.collection_at(&json!({"index": n}))?),
            None => match opt_str(args, "name")? {
                Some(_) => Some(self.collection_at(args)?),
                None => None,
            },
        };
        let rows: Vec<Value> = self
            .lib_cols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut row = json!({
                    "index": i + 1,
                    "name": c.name,
                    "count": c.files.len(),
                });
                if open == Some(i) {
                    row["files"] = json!(c.files);
                }
                row
            })
            .collect();
        Ok(json!({"collections": rows}))
    }

    pub(super) fn cmd_frame_select(&mut self, args: &Value) -> Result<Value, String> {
        let n = self.require_sequence()?.len();
        let index =
            opt_u32(args, "index")?.ok_or_else(|| "缺少整数参数 index（从 1 开始）".to_owned())?;
        if index == 0 || index as usize > n {
            return Err(crate::i18n::tr_fmt("index 超出范围：1..={}", &[&n]));
        }
        self.set_frame(index as usize - 1);
        self.preview_dirty = true;
        let name = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.name.clone())
            .unwrap_or_default();
        self.status = crate::i18n::tr_fmt("已跳到第 {} 帧 · {}", &[&(self.selected + 1), &name]);
        Ok(json!({
            "selected": self.selected + 1,
            "frames": n,
            "name": name,
        }))
    }

    pub(super) fn cmd_ui_module(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let module = Module::from_name(name).ok_or_else(|| {
            crate::i18n::tr_fmt(
                "未知模块 {}；可用 序列/去闪/显影/构图/成片/声音/合成 或 0-6",
                &[&name],
            )
        })?;
        self.set_module(module);
        self.status = crate::i18n::tr_fmt("模块：{}", &[&crate::i18n::tr(module.cn())]);
        Ok(json!({"module": module.cn(), "index": module.index()}))
    }

    pub(super) fn cmd_ui_dock(&mut self, args: &Value) -> Result<Value, String> {
        let name = arg_str(args, "name")?;
        let tab = DockTab::from_name(name).ok_or_else(|| {
            crate::i18n::tr_fmt("未知面板 {}；可用 调整/信息/关键帧/AI 或 0-3", &[&name])
        })?;
        self.dock_tab = tab;
        self.show_dock = true;
        self.status = crate::i18n::tr_fmt("面板：{}", &[&crate::i18n::tr(tab.cn())]);
        Ok(json!({
            "tab": tab.cn(),
            "index": tab.index(),
            "visible": self.show_dock,
        }))
    }

    pub(super) fn grade_json(&self) -> Value {
        json!({
            "preset": self.preset,
            "params": serde_json::to_value(&self.params).unwrap_or(Value::Null),
            "copies": self.copies.iter().map(|c| json!({
                "name": c.name,
                "keyframes": c.keyframes.len(),
            })).collect::<Vec<_>>(),
            "active_copy": self.active_copy(),
        })
    }

    /// 命名调色副本：list / save / apply / delete / rename。
    /// 存与覆盖记的是「当前调色 + 关键帧」整份；套用把副本取回当前。
    pub(super) fn cmd_grade_copy(&mut self, args: &Value) -> Result<Value, String> {
        let op = args.get("op").and_then(Value::as_str).unwrap_or("list");
        let name = args.get("name").and_then(Value::as_str).unwrap_or("");
        match op {
            "list" => Ok(self.grade_json()),
            "save" => {
                let overwrite = args
                    .get("overwrite")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let index = self.save_copy(name, overwrite)?;
                Ok(json!({
                    "saved": self.copies[index].name,
                    "index": index + 1,
                    "copies": self.copies.len(),
                }))
            }
            "apply" => {
                self.apply_copy(name)?;
                Ok(self.grade_json())
            }
            "delete" => {
                self.delete_copy(name)?;
                Ok(json!({"deleted": name, "copies": self.copies.len()}))
            }
            "rename" => {
                let to = args.get("to").and_then(Value::as_str).unwrap_or("");
                self.rename_copy(name, to)?;
                Ok(json!({"renamed": to, "copies": self.copies.len()}))
            }
            other => Err(crate::i18n::tr_fmt(
                "未知 op {}；可用 list / save / apply / delete / rename",
                &[&other],
            )),
        }
    }
}
