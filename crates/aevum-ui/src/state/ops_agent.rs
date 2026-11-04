//! AI 面板：一键修片、去闪重算、按 EV 序列拟合，以及预设与参数的套用和分组变更判定。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! The AI panel: One-click edits, deflicker recomputation, EV-series fitting, plus applying presets and parameters and deciding which groups changed.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ AI 面板

    /// 解析并执行一行「命令 ID + JSON 参数」，结果写入最近记录。
    ///
    /// Parse and execute one line of "command ID + JSON arguments"; the result goes into the
    /// recent records.
    pub fn run_ai_line(&mut self, ctx: &Context, line: &str) -> Result<Value, String> {
        let line = line.trim();
        if line.is_empty() {
            return Err("先输入一条命令，例如 sequence.info".to_owned());
        }
        let split = line.find(char::is_whitespace).unwrap_or(line.len());
        let id = &line[..split];
        let rest = line[split..].trim();
        let args = if rest.is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(rest)
                .map_err(|e| crate::i18n::tr_fmt("参数不是合法 JSON：{}", &[&e]))?
        };
        if !args.is_object() {
            return Err("参数必须是 JSON 对象，例如 {\"exposure\":0.5}".to_owned());
        }
        let result = self.run_ui_command(ctx, id, &args);
        let (ok, detail) = match &result {
            Ok(value) => (
                true,
                serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned()),
            ),
            Err(message) => (false, message.clone()),
        };
        self.ai_log.push(AiLogEntry {
            input: line.to_owned(),
            ok,
            detail: truncate(&detail, 600),
        });
        let keep = 8;
        if self.ai_log.len() > keep {
            let drop = self.ai_log.len() - keep;
            self.ai_log.drain(0..drop);
        }
        result
    }

    pub fn recompute_deflicker(&mut self) {
        // 去闪输入（测光/窗口/强度/通道）或结果一变就换版：曲线缓存键之一。
        self.corr_version += 1;
        if self.metrics.is_empty() {
            self.analysis = None;
            self.corr.clear();
            self.corr_rgb.clear();
            return;
        }
        // 线性光测光是 Python 金标的口径；拿不到（演示序列、自检）才退回 gamma 均值。
        let measured = self.def_means.iter().any(|m| m.is_some());
        let (window, strength, multi) = (self.def_window, self.def_strength, self.def_multi);
        let align = self.def_align_exif;
        let a = if self.def_channels && measured {
            let rgb: Vec<Option<[f64; 3]>> =
                self.def_means.iter().map(|m| m.map(|m| m.rgb)).collect();
            let evs = self.exif_evs(rgb.len());
            if align {
                deflicker::analyze_rgb_exif(&rgb, &evs, window, strength, multi)
            } else {
                deflicker::analyze_rgb(&rgb, window, strength, multi)
            }
        } else if measured {
            let luma: Vec<Option<f64>> = self.def_means.iter().map(|m| m.map(|m| m.luma)).collect();
            let evs = self.exif_evs(luma.len());
            if align {
                deflicker::analyze_luma_exif(&luma, &evs, window, strength, multi)
            } else {
                deflicker::analyze_luma(&luma, window, strength, multi)
            }
        } else {
            let luma: Vec<Option<f64>> = self
                .metrics
                .iter()
                .map(|m| m.map(|m| m.mean * 255.0))
                .collect();
            let evs = self.exif_evs(luma.len());
            if align {
                deflicker::analyze_luma_exif(&luma, &evs, window, strength, multi)
            } else {
                deflicker::analyze_luma(&luma, window, strength, multi)
            }
        };
        self.corr = a
            .exposure_per_frame()
            .into_iter()
            .map(|v| v as f32)
            .collect();
        self.corr_rgb = a
            .chan_residual_per_frame()
            .into_iter()
            .map(|c| [c[0] as f32, c[1] as f32, c[2] as f32])
            .collect();
        self.analysis = Some(a);
        self.preview_dirty = true;
    }

    /// 每帧的相对 EV（缺三要素的是 `None`），长度**钉死为 `n`**：去闪分析是逐行对齐的，
    /// 表短了会让校正比帧数少，表长了会在尾部多出没人认领的行。
    ///
    /// Per-frame relative EV (`None` where the triplet is missing), length **pinned to `n`**:
    /// the deflicker analysis is row-aligned, so a short table would yield fewer corrections
    /// than frames and a long one would leave unclaimed rows at the tail.
    fn exif_evs(&self, n: usize) -> Vec<Option<f64>> {
        (0..n)
            .map(|i| self.exif.get(i).and_then(|e| e.ev()))
            .collect()
    }

    /// 有曝光三要素的帧数。面板拿它说明预对齐到底生效了几帧——一个 EV 都没有时开关
    /// 是空转的，不说清楚就是假开关。
    ///
    /// How many frames carry the exposure triplet. The panel uses it to say how many frames
    /// the pre-alignment actually covers: with no EV at all the switch is a no-op, and
    /// staying silent about that would be a fake switch.
    pub fn exif_ev_frames(&self) -> usize {
        self.exif.iter().filter(|e| e.ev().is_some()).count()
    }

    pub fn apply_preset(&mut self, name: &str) {
        self.params = grade::preset(name);
        self.preset = name.to_owned();
        self.auto_note = None;
        self.reset_mask_state();
        self.preview_dirty = true;
        history::note(
            "preset",
            crate::i18n::tr_fmt("预设 {}", &[&crate::i18n::tr(name)]),
        );
    }

    pub fn reset_params(&mut self) {
        self.params = GradeParams::default();
        self.preset = "原始".to_owned();
        self.auto_note = None;
        self.reset_mask_state();
        self.preview_dirty = true;
        history::note("reset", "复位全部参数");
    }

    /// 选项栏上下文徽标（界面稿 .ctx .n「N 项调整」）：有几组参数偏离默认值。
    /// 按「组」计数而不是按参数 —— 徽标只回答"动没动过、动了多大片"，
    /// 逐参数细数是 dock 页签自己的职责。
    ///
    /// The context badge in the options bar (the ".ctx .n" entry in the design): how many
    /// groups deviate from their defaults.
    /// Counted by group rather than by parameter -- the badge only answers "was anything
    /// touched, and how much of it", while per-parameter detail is the dock tabs' job.
    pub fn changed_groups(&self) -> usize {
        // 与 `GROUP_SPECS` 里显影模块的组一一对应（ACR 九分区去掉蒙版/蒙版参数
        // 这两组纯动作组——它们没有「偏离默认值」可言；外观挂着 .cube，算一组）。
        //
        // One entry per Develop group in `GROUP_SPECS` that owns grading parameters
        // (the ACR nine sections minus Masks/mask-params, which are action groups with
        // nothing to deviate from; Look counts because it holds a .cube path).
        const KEYS: [&str; 8] = [
            "basic", "curve", "mixer", "grade", "detail", "optics", "effects", "look",
        ];
        KEYS.iter().filter(|k| self.group_changed(k)).count()
    }

    /// 分组头右缘的 accent 圆点开关：这一组里有参数偏离默认值才亮。
    /// 字段清单与 `shell::reset_group` 的复位口径一一对应 —— 那边复位哪些字段，
    /// 这边就比对哪些字段，两边必须同进退。
    ///
    /// The accent dot at the right edge of a group header: lit when any parameter in the
    /// group deviates from its default.
    /// The field list corresponds one-to-one with `shell::reset_group` -- whichever fields
    /// that one resets, this one compares; the two must move together.
    pub fn group_changed(&self, key: &str) -> bool {
        let d = GradeParams::default();
        let p = &self.params;
        // 推子参数组：按规格表比对（唯一事实源，见 controls 模块头注释）。
        // 表外的组内字段——参数曲线的分界、黑白色带、分级的色轮——单独比。
        if crate::controls::specs_of_group(key).next().is_some() {
            let hit = crate::controls::specs_of_group(key)
                .any(|s| crate::controls::param_get(p, s.field) != s.default);
            let extra = match key {
                // 曲线区（ACR 的 Curve 面板）：参数曲线 / 点曲线 / 色阶三种调法共用
                // 一个分区。表里只有 HDR 尾部一条，圆点还得看三条分界、五档色阶
                // 与四条点曲线。
                "curve" => {
                    p.par_split1 != d.par_split1
                        || p.par_split2 != d.par_split2
                        || p.par_split3 != d.par_split3
                        || p.lv_black != d.lv_black
                        || p.lv_white != d.lv_white
                        || p.lv_gamma != d.lv_gamma
                        || p.lv_out_black != d.lv_out_black
                        || p.lv_out_white != d.lv_out_white
                        || p.curve_rgb != d.curve_rgb
                        || p.curve_r != d.curve_r
                        || p.curve_g != d.curve_g
                        || p.curve_b != d.curve_b
                }
                // 颜色混合器（HSL / 黑白两种模式）：表里只有黑白强度一条，
                // 八段色带与八段黑白亮度单独比。
                "mixer" => {
                    p.hsl_h != d.hsl_h
                        || p.hsl_s != d.hsl_s
                        || p.hsl_l != d.hsl_l
                        || p.mono_h != d.mono_h
                }
                "grade" => {
                    p.grade_sh_h != d.grade_sh_h
                        || p.grade_sh_s != d.grade_sh_s
                        || p.grade_sh_l != d.grade_sh_l
                        || p.grade_mid_h != d.grade_mid_h
                        || p.grade_mid_s != d.grade_mid_s
                        || p.grade_mid_l != d.grade_mid_l
                        || p.grade_hi_h != d.grade_hi_h
                        || p.grade_hi_s != d.grade_hi_s
                        || p.grade_hi_l != d.grade_hi_l
                }
                _ => false,
            };
            return hit || extra;
        }
        // 表外组只剩外观一条（蒙版/蒙版参数是动作组，从不亮圆点）。
        //
        // Look is the only group left with no spec table; Masks/mask-params are action
        // groups and never light the dot.
        match key {
            "look" => !p.look_cube.is_empty(),
            _ => false,
        }
    }
}
