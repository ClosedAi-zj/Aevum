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

/// 单位方吸附容差：四角全距单位方小于它就整体归位（2e-3，与 core `quad_from_pair`
/// 的容差守卫同档）。残差小于这个量在预览上无从分辨，留在参数里却让每次渲染
/// 都走全幅重采样而不是单位快路。
///
/// Unit-square snap tolerance: when every corner is closer than this (2e-3, the same class
/// as core's `quad_from_pair` tolerance guard) the quad snaps to the unit square. A smaller
/// residual is invisible in the preview, yet left in the parameters it would push every
/// render down the full-frame resample path instead of the unit fast path.
const UPRIGHT_SNAP_EPS: f64 = 2e-3;

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

    /// 四角拖拽能不能开工：整幅画面得**全部**显示在画布上，四个角才抓得到。
    ///
    /// 与修复画笔同一套拒绝态（裁剪中 / 拉直 / 运镜推近 / 无帧），另加一条画幅裁剪：
    /// 3:2 的片子选了 16:9 之后上下两条边已经在画面之外，那两个角在屏幕上根本不出现。
    ///
    /// Whether corner dragging can start: the whole frame has to be **fully** on the canvas,
    /// otherwise the corners cannot be grabbed.
    ///
    /// The same refusal states as the repair brush (cropping, straightened, motion box pushed
    /// in, no frame) plus one of its own: with an aspect crop such as 16:9 on a 3:2 frame the
    /// top and bottom edges are already outside the canvas, so those two corners never appear.
    pub fn upright_can_drag(&self) -> bool {
        if self.crop_editing() || self.rotate != 0.0 {
            return false;
        }
        let n = self.frame_count();
        if n == 0 {
            return false;
        }
        if self
            .motion_view_at(self.selected.min(n.saturating_sub(1)), n)
            .is_some()
        {
            return false;
        }
        self.preview_crop().is_none()
    }

    /// 开关四角拖拽。返回是否真的停在编辑态里（开不起来时为 `false`）。
    ///
    /// 进入时若还没有四边形，先种一个**单位方**：单位方就是「不动作」，预览因此不会跳变，
    /// 而四个把手立刻出现在画面四角，可以开始拖。退出时若四边形仍是单位方就删掉——
    /// 工程文件里不该留一个什么都不做的参数。
    ///
    /// Toggles corner dragging; returns whether it is now in edit mode (`false` if it could not
    /// start).
    ///
    /// Entering with no quad seeds the **unit square**: the unit square means "do nothing", so
    /// the preview does not jump, while four handles appear at the frame's corners ready to
    /// drag. Leaving with the quad still at the unit square removes it -- a parameter that does
    /// nothing has no business staying in the project file.
    pub fn upright_edit_toggle(&mut self) -> bool {
        if self.upright_editing {
            self.upright_editing = false;
            if self
                .params
                .upright
                .as_ref()
                .is_some_and(|q| q.is_identity())
            {
                self.params.upright = None;
            }
            self.preview_dirty = true;
            return false;
        }
        if !self.upright_can_drag() {
            return false;
        }
        if self.params.upright.is_none() {
            self.params.upright = Some(perspective::Quad::default());
        }
        self.upright_editing = true;
        self.preview_dirty = true;
        true
    }

    /// 四个角在**画布**归一坐标里的位置（画四边形与把手用）。没有四边形时按单位方给。
    ///
    /// The four corners in **canvas**-normalized coordinates, for drawing the quad and its
    /// handles. Falls back to the unit square when there is no quad.
    pub fn upright_corners(&self) -> [(f32, f32); 4] {
        let q = self.params.upright.unwrap_or_default();
        let mut out = [(0.0f32, 0.0f32); 4];
        for (i, (u, v)) in q.p.iter().enumerate() {
            let (cu, cv) = self.frame_to_canvas(*u, *v);
            out[i] = (cu as f32, cv as f32);
        }
        out
    }

    /// 把第 `i` 个角拖到画布位置 `(cu, cv)`。夹进 `[0,1]`，非有限值忽略，没动就返回 `false`。
    ///
    /// **拖动途中只标脏、不记历史**：一次拖拽每帧都会调到这里，逐帧记历史会把撤销栈刷爆；
    /// 松手时由 [`Self::upright_commit`] 记一条。
    ///
    /// Drags corner `i` to canvas position `(cu, cv)`. Clamped to `[0,1]`, non-finite values
    /// ignored, and a no-op returns `false`.
    ///
    /// **Mid-drag this only marks the preview dirty and records no history**: the call happens
    /// every frame of a drag, and one history entry per frame would bury the undo stack. A
    /// single entry is recorded on release by [`Self::upright_commit`].
    pub fn upright_drag_corner(&mut self, i: usize, cu: f64, cv: f64) -> bool {
        if i >= 4 {
            return false;
        }
        let (u, v) = self.canvas_to_frame(cu, cv);
        if !(u.is_finite() && v.is_finite()) {
            return false;
        }
        let at = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
        let mut q = self.params.upright.unwrap_or_default();
        if q.p[i] == at {
            return false;
        }
        q.p[i] = at;
        self.params.upright = Some(q);
        self.preview_dirty = true;
        true
    }

    /// 松手：四边形真有动作才记一条历史（单位方不记，否则空拖一下也会留痕）。
    ///
    /// 提交时做两道几何收口（与 MCP 侧 `upright.set` 的拒绝口径一致）：
    ///
    /// 1. **吸附**：四角都距单位方不到 [`UPRIGHT_SNAP_EPS`]（2e-3，与
    ///    `quad_from_pair` 的容差守卫同档）就整体归位单位方——残差留在参数里
    ///    会让每次渲染都付一遍全幅重采样，而视觉上与「没动」无从区分。
    /// 2. **退化校验**：三点共线 / 面积为零（蝴蝶形四边形）求不出单应，渲染阶段
    ///    会静默不动作；当场拒绝、写一句具名提示，而不是把坏参数持久化。
    ///
    /// On release: record one history entry only if the quad actually does something. The unit
    /// square records nothing, so an idle drag leaves no trace.
    ///
    /// The commit runs two geometric guards (matching the MCP side's `upright.set` rejection):
    ///
    /// 1. **Snap**: when every corner sits within [`UPRIGHT_SNAP_EPS`] (2e-3, the same class
    ///    as `quad_from_pair`'s tolerance guard) of the unit square, the quad snaps back to
    ///    the unit square -- a residual left in the parameters would pay a full-frame
    ///    resample on every render while looking indistinguishable from "untouched".
    /// 2. **Degeneracy check**: a collinear / zero-area quad (a butterfly shape) admits no
    ///    homography and the render stage would silently do nothing; the commit refuses it
    ///    with a named message instead of persisting a dead parameter.
    pub fn upright_commit(&mut self) -> bool {
        let Some(mut q) = self.params.upright else {
            return false;
        };
        // 吸附在退化校验之前：被吸回单位方就直接走「没动作」这条路。
        //
        // Snap before the degeneracy check: a snapped unit square takes the "no-op" path.
        let unit = perspective::Quad::default();
        if q.p
            .iter()
            .zip(unit.p)
            .all(|((x, y), (ux, uy))| (x - ux).hypot(y - uy) < UPRIGHT_SNAP_EPS)
        {
            q = unit;
            self.params.upright = Some(q);
            return false;
        }
        if q.is_identity() {
            return false;
        }
        // 退化四边形拒绝写入：撤掉这次拖动留下的四边形，给一句具名提示。
        //
        // A degenerate quad is refused: drop the quad this drag left behind and say so.
        if perspective::homography(&q).is_none() {
            self.params.upright = None;
            self.auto_note =
                Some(crate::i18n::t!("四角交叉，校正未生效——请把角点拖回画面内四角").to_owned());
            self.preview_dirty = true;
            return false;
        }
        self.retouch_changed("透视四角");
        true
    }

    /// 把一个角放回单位方的位置（双击角）。返回是否真的动了。
    ///
    /// Puts one corner back where the unit square has it (double-clicking a corner). Returns
    /// whether anything moved.
    pub fn upright_reset_corner(&mut self, i: usize) -> bool {
        if i >= 4 {
            return false;
        }
        let unit = perspective::Quad::default();
        let Some(mut q) = self.params.upright else {
            return false;
        };
        if q.p[i] == unit.p[i] {
            return false;
        }
        q.p[i] = unit.p[i];
        self.params.upright = Some(q);
        self.retouch_changed("透视四角复位");
        true
    }
}
