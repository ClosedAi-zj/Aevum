//! 修复与几何：画布落点与取景换算、修复笔触的增删、自动直立与清除。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Retouching and geometry: Canvas-to-frame mapping, adding and undoing repair strokes, auto upright and clearing it.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    // ------------------------------------------------------------ 修复与几何

    /// 预览实际使用的那块取景（非裁剪态）。`None` 表示预览显示整幅。
    /// 与 `ensure_preview` 用的是同一段判断——两处一旦分叉，画布上的落点就会错位。
    ///
    /// The framing the preview actually uses (outside crop-edit mode). `None` means the preview
    /// shows the whole frame. This is the same decision `ensure_preview` makes: if the two
    /// diverge, taps on the canvas land in the wrong place.
    pub fn preview_crop(&self) -> Option<CropRect> {
        if self.crop_editing() {
            return (self.rotate != 0.0).then(|| self.crop_view());
        }
        let base = if self.align_preview {
            self.alignment_at(self.selected).0
        } else {
            None
        };
        self.aspect_crop(base)
    }

    /// 画布归一化坐标 → 整幅原图归一化坐标。
    ///
    /// Canvas-normalized coordinates → whole-frame normalized coordinates.
    pub fn canvas_to_frame(&self, u: f64, v: f64) -> (f64, f64) {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        match self.preview_crop() {
            Some(c) if c.w > 0 && c.h > 0 => (
                (f64::from(c.x) + u * f64::from(c.w)) / f64::from(w.max(1)),
                (f64::from(c.y) + v * f64::from(c.h)) / f64::from(h.max(1)),
            ),
            _ => (u, v),
        }
    }

    /// 整幅原图归一化坐标 → 画布归一化坐标（落点轮廓回画用）。
    ///
    /// Whole-frame normalized coordinates → canvas-normalized coordinates, used to draw the
    /// outlines of placed strokes.
    pub fn frame_to_canvas(&self, u: f64, v: f64) -> (f64, f64) {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        match self.preview_crop() {
            Some(c) if c.w > 0 && c.h > 0 => (
                (u * f64::from(w.max(1)) - f64::from(c.x)) / f64::from(c.w),
                (v * f64::from(h.max(1)) - f64::from(c.y)) / f64::from(c.h),
            ),
            _ => (u, v),
        }
    }

    /// 现在能不能在画布上落点。拉直、推近取景、裁剪态下画布显示的都不是整幅，
    /// 落点会错位——宁可不接，也不要让用户修错地方。
    ///
    /// Whether a tap can land right now. Under straightening, a zoomed viewfinder or crop-edit
    /// mode the canvas does not show the whole frame, so taps would land in the wrong place:
    /// better to refuse than to repair the wrong spot.
    pub fn heal_can_place(&self) -> bool {
        if self.crop_editing() || self.rotate != 0.0 {
            return false;
        }
        let n = self.frame_count();
        if n == 0 {
            return false;
        }
        self.motion_view_at(self.selected.min(n.saturating_sub(1)), n)
            .is_none()
    }

    /// 修复/透视改了参数之后统一走这里：预览重画、预设转自定义、历史记一条。
    ///
    /// Everything that changes repair or perspective parameters ends here: the preview is
    /// redrawn, the preset turns custom, and one history entry is recorded.
    pub fn retouch_changed(&mut self, note: &str) {
        self.preview_dirty = true;
        if self.preset != "自定义" {
            self.preset = "自定义".to_owned();
        }
        history::note("retouch", note);
    }

    /// 落一笔。`at` 是**整幅归一化**坐标；返回值表示这一下到底有没有落成——
    /// 仿制模式下第一下是取源，落不成。
    ///
    /// Lay one stroke. `at` is in **whole-frame normalized** coordinates; the return value says
    /// whether it actually landed — in clone mode the first tap only picks the source.
    pub fn heal_add(&mut self, at: (f64, f64)) -> bool {
        let r = (self.heal_radius / 100.0).clamp(0.001, f64::from(retouch::SpotSpec::MAX_RADIUS))
            as f32;
        let source = if self.heal_clone {
            let Some(src) = self.heal_src else {
                self.heal_src = Some((at.0 as f32, at.1 as f32));
                self.preview_dirty = true;
                return false;
            };
            // 偏移在像素域算、再折成「长边单位」：x 的归一化分母是宽，y 的是高，
            // 直接相减会得到一个非等比缩放过的偏移。
            //
            // The offset is computed in pixels and then folded into long-edge units: `x` is
            // normalized by the width and `y` by the height, so subtracting them directly
            // would give a non-uniformly scaled offset.
            let (w, h) = self.resolution.unwrap_or((1, 1));
            let long = w.max(h).max(1) as f32;
            Some((
                (at.0 as f32 - src.0) * w as f32 / long,
                (at.1 as f32 - src.1) * h as f32 / long,
            ))
        } else {
            None
        };
        self.params.retouch.push(retouch::SpotSpec {
            x: at.0 as f32,
            y: at.1 as f32,
            r,
            feather: (self.heal_feather / 100.0) as f32,
            source,
        });
        self.retouch_changed("修复笔触");
        true
    }

    /// 撤掉最后一笔；画面上没有笔触时返回 `false`（不产生一条空历史）。
    ///
    /// Drop the last stroke; returns `false` when there is none, so no empty history entry is
    /// recorded.
    pub fn heal_undo(&mut self) -> bool {
        if self.params.retouch.pop().is_none() {
            return false;
        }
        self.retouch_changed("撤销修复笔触");
        true
    }

    /// 清空全部笔触。
    ///
    /// Clear every stroke.
    pub fn heal_clear(&mut self) -> bool {
        if self.params.retouch.is_empty() {
            return false;
        }
        self.params.retouch.clear();
        self.retouch_changed("清空修复笔触");
        true
    }

    /// 自动直立：用当前帧的草图找主竖线，反推梯形四边形。
    /// 返回 `Err` 时给的是给用户看的一句话，界面直接显示。
    ///
    /// Auto-upright: find the dominant vertical lines in the current frame's sketch and derive
    /// the keystone quad. On `Err` the string is meant for the user and shown verbatim.
    pub fn upright_auto(&mut self) -> Result<(), String> {
        let Some(path) = self
            .seq
            .as_ref()
            .and_then(|s| s.frames.get(self.selected))
            .map(|f| f.path.clone())
        else {
            return Err(crate::i18n::t!("先打开一张照片").to_owned());
        };
        let stamp = source_stamp(&path);
        // 草图就够：`auto_upright` 自己会缩到 640 边再找线，解全幅只是白花几百毫秒。
        //
        // The sketch is enough: `auto_upright` shrinks to a 640px edge before looking for
        // lines, so decoding full size would just waste a few hundred milliseconds.
        let (buf, w, h, _) = decoded_preview_source(&path, stamp, true, "")?;
        let q = perspective::auto_upright(&buf, w, h)
            .ok_or_else(|| crate::i18n::t!("画面里没有可用的竖直主直线").to_owned())?;
        let q = q
            .clamped()
            .ok_or_else(|| crate::i18n::t!("算出的四边形不合法").to_owned())?;
        self.params.upright = Some(q);
        self.retouch_changed("自动直立");
        Ok(())
    }

    /// 卸下透视校正（回到整幅）。
    ///
    /// Drop the perspective correction (back to the whole frame).
    pub fn upright_clear(&mut self) -> bool {
        if self.params.upright.take().is_none() {
            return false;
        }
        self.retouch_changed("清除透视校正");
        true
    }
}
