//! XMP 预设与 .cube 外观：XMP 预设的导入导出，以及 .cube 外观的导入导出与截图。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! XMP presets and .cube looks: Importing and exporting XMP presets, and importing/exporting .cube looks plus screenshots.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ XMP 预设

    /// 导入 XMP 预设：整体替换全局调色参数。
    ///
    /// 与 Python `read_xmp` + `merge` 同口径：预设没写的键回到默认值，
    /// 局部蒙版也随默认值清空（Lightroom 预设不含本工程的蒙版模型）。
    ///
    /// Import an XMP preset: replaces the global grading parameters wholesale.
    ///
    /// Same definition as Python's `read_xmp` + `merge`: keys the preset does not mention
    /// return to their defaults, and local masks are cleared along with them (a Lightroom
    /// preset does not carry this project's mask model).
    pub fn import_xmp_preset(&mut self, path: &Path) -> Result<Value, String> {
        let params =
            xmp::read(path).map_err(|e| crate::i18n::tr_fmt("读不进这个预设：{}", &[&e]))?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "XMP 预设".to_owned());
        self.params = params;
        self.preset = name.clone();
        self.reset_mask_state();
        self.auto_note = None;
        self.preview_dirty = true;
        self.status = crate::i18n::tr_fmt("已导入 XMP 预设 {}", &[&name]);
        let mut out = self.grade_json();
        if let Some(object) = out.as_object_mut() {
            object.insert("path".to_owned(), json!(path.display().to_string()));
            object.insert("imported".to_owned(), json!(name));
        }
        Ok(out)
    }

    /// 把当前参数写成 XMP 预设；覆盖已有文件必须显式确认。
    ///
    /// Write the current parameters out as an XMP preset; overwriting an existing file
    /// requires explicit confirmation.
    pub fn export_xmp_preset(&mut self, path: &Path, confirm: bool) -> Result<Value, String> {
        let is_xmp = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xmp"));
        if !is_xmp {
            return Err("path 必须以 .xmp 结尾".to_owned());
        }
        if path.exists() && !confirm {
            return Err(crate::i18n::tr_fmt(
                "目标已存在，需 confirm: true：{}",
                &[&path.display()],
            ));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)
                .map_err(|e| crate::i18n::tr_fmt("创建目录失败：{}", &[&e]))?;
        }
        xmp::write(path, &self.params)
            .map_err(|e| crate::i18n::tr_fmt("写 XMP 失败：{}", &[&e]))?;
        self.status = crate::i18n::tr_fmt("已导出 XMP 预设 {}", &[&path.display()]);
        Ok(json!({
            "path": path.display().to_string(),
            "preset": self.preset,
        }))
    }

    pub(super) fn cmd_xmp_import(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        if !path.is_file() {
            return Err(crate::i18n::tr_fmt("文件不存在：{}", &[&path.display()]));
        }
        self.import_xmp_preset(&path)
    }

    pub(super) fn cmd_xmp_export(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        self.export_xmp_preset(&path, confirm(args))
    }

    // ------------------------------------------------------------ .cube 外观命令

    /// 读入一张 .cube 并挂成"外观"（`apply: false` 时只体检、不改参数）。
    pub(super) fn cmd_cube_import(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        if !path.is_file() {
            return Err(crate::i18n::tr_fmt("文件不存在：{}", &[&path.display()]));
        }
        let apply = args.get("apply").and_then(Value::as_bool).unwrap_or(true);
        let key = path.display().to_string();
        let lut =
            cube::refresh(&key).map_err(|e| crate::i18n::tr_fmt("读 .cube 失败：{}", &[&e]))?;
        let info = cube::describe(&lut);
        let mut result = json!({
            "path": key,
            "title": info.title,
            "size": info.size,
            "nodes": info.nodes,
            "domain_min": info.domain_min,
            "domain_max": info.domain_max,
            "neutral": info.neutral,
            "applied": false,
        });
        if apply {
            self.params.look_cube = key.clone();
            // 挂外观就是想看到它：顺手摘掉外观组的旁路（UI 与 MCP 同走这里）。
            //
            // Applying a LUT means wanting to see it: lift the Look group's bypass
            // (both the UI dialog and MCP land here).
            self.bypassed.remove("look");
            self.preview_dirty = true;
            self.preset = "自定义".to_owned();
            self.status = crate::i18n::tr_fmt("已挂外观 {}", &[&key]);
            result["applied"] = json!(true);
        }
        Ok(result)
    }

    /// 把当前调色（含已挂的外观）烘成一张 .cube 交出去。
    pub(super) fn cmd_cube_export(&mut self, args: &Value) -> Result<Value, String> {
        let path = PathBuf::from(arg_str(args, "path")?);
        let is_cube = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cube"));
        if !is_cube {
            return Err("path 必须以 .cube 结尾".to_owned());
        }
        let size = opt_u32(args, "size")?.unwrap_or(cube::DEFAULT_SIZE as u32);
        if !(cube::MIN_SIZE as u32..=cube::MAX_SIZE as u32).contains(&size) {
            return Err(crate::i18n::tr_fmt(
                "size 必须在 {}..={}",
                &[&cube::MIN_SIZE, &cube::MAX_SIZE],
            ));
        }
        if path.exists() && !confirm(args) {
            return Err(crate::i18n::tr_fmt(
                "目标已存在，需 confirm: true：{}",
                &[&path.display()],
            ));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)
                .map_err(|e| crate::i18n::tr_fmt("创建目录失败：{}", &[&e]))?;
        }
        let params = self.params.clone();
        let size = size as usize;
        cube::export(&path, &params, size)
            .map_err(|e| crate::i18n::tr_fmt("写 .cube 失败：{}", &[&e]))?;
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.status = crate::i18n::tr_fmt("已导出外观 {}", &[&path.display()]);
        Ok(json!({
            "path": path.display().to_string(),
            "size": size,
            "nodes": size * size * size,
            "bytes": bytes,
            "look": if params.look_cube.is_empty() { Value::Null } else { json!(params.look_cube) },
        }))
    }

    pub(super) fn cmd_screenshot(&mut self, ctx: &Context, args: &Value) -> Result<Value, String> {
        let raw = arg_str(args, "path")?;
        let path = PathBuf::from(raw);
        let is_png = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if !is_png {
            return Err("path 必须以 .png 结尾".to_owned());
        }
        if let Ok(seq) = self.require_sequence()
            && seq.frames.iter().any(|frame| path_eq(&frame.path, &path))
        {
            return Err(crate::i18n::tr_fmt("拒绝覆盖素材：{}", &[&path.display()]));
        }
        if path.exists() && !confirm(args) {
            return Err(crate::i18n::tr_fmt(
                "目标已存在，需 confirm: true：{}",
                &[&path.display()],
            ));
        }
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)
                .map_err(|e| crate::i18n::tr_fmt("创建目录失败：{}", &[&e]))?;
        }
        let out = path.clone();
        ctx.request_screenshot(move |image| {
            if let Err(e) = control::save_png(&out, &image) {
                eprintln!("截图保存失败：{e}");
            }
        });
        Ok(json!({
            "started": true,
            "path": path.display().to_string(),
            "note": "截图在下一帧写盘；稍后确认文件即可",
        }))
    }
}
