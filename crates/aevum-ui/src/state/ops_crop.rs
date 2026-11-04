//! 画幅、拉直与裁剪：画幅与有效裁剪的计算、裁剪框的拖拽与吸附、拉直边界与旋转后的取景。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Aspect, straightening and cropping: Computing the aspect and effective crop, dragging and snapping the crop box, and the straighten bounds and post-rotation framing.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    /// 对齐用的几何：裁切框 + 当前帧位移；没跑过对齐就是全画幅零位移。
    pub(super) fn alignment_at(&self, i: usize) -> (Option<CropRect>, [f64; 2]) {
        match &self.align_report {
            Some(r) => (
                Some(r.crop),
                r.offsets.get(i).copied().unwrap_or([0.0, 0.0]),
            ),
            None => (None, [0.0, 0.0]),
        }
    }

    /// 拉直削边：整幅转过 `rotate` 之后仍然完全有像素的那块居中区域。
    /// 它既是导出的上限，也是画布上拖框的上限——框外没有像素，拖出去只能削成空白。
    fn straighten_bounds(&self, base: Option<CropRect>) -> Option<CropRect> {
        if self.rotate.abs() <= 1e-6 {
            return base;
        }
        let (rw, rh) = self.resolution?;
        let safe = crop::safe_rect(rw, rh, self.rotate, self.aspect.pair());
        Some(match base {
            Some(b) => crop::intersect(b, safe).unwrap_or(safe),
            None => safe,
        })
    }

    /// 画幅比例锁定的取景框（原图像素）：用户框优先，没拖过框才按比例内接。
    pub(super) fn aspect_crop(&self, base: Option<CropRect>) -> Option<CropRect> {
        let base = self.straighten_bounds(base);
        let Some(user) = self.user_crop else {
            // 原生比例不内接，这时直接返回拉直削出来的框：丢掉它等于把转出去的
            // 反射角留在导出里。
            return fit_aspect(base, self.resolution, self.aspect).or(base);
        };
        // 用户框是事实来源，对齐安全框只负责削边；削完再兜一次比例，
        // 免得导出的边比画布上看到的多出一条。
        let framed = match base {
            Some(b) => crop::intersect(user, b).unwrap_or(user),
            None => user,
        };
        fit_aspect(Some(framed), self.resolution, self.aspect).or(Some(framed))
    }

    /// 导出取景：对齐裁切（始终生效，和队列同口径）再叠用户框与画幅比例。
    pub(super) fn effective_crop(&self) -> Option<CropRect> {
        self.aspect_crop(self.align_report.as_ref().map(|r| r.crop))
    }

    /// 画布当前是不是裁剪态：裁剪工具或构图模块。这时预览画整帧，
    /// 取景框只是盖在上面的一层矢量，拖它不重画像素。
    ///
    /// Whether the canvas is in crop mode: the crop tool, or the composition module. In that
    /// mode the preview draws the whole frame and the viewfinder is only a vector layer on top,
    /// so dragging it repaints no pixels.
    pub fn crop_editing(&self) -> bool {
        self.tool == Tool::Crop || self.module == Module::Frame
    }

    /// 画布上是「像素不动、只拖矢量框」的两种态之一：拖裁剪框或拖运镜取景框。
    /// 这两种态下预览都不把取景框叠进像素，所以拖动跟手。
    ///
    /// Whether the canvas is in one of the two states where "pixels stay put, only a vector
    /// box is dragged": dragging the crop box, or dragging the motion viewfinder. In both, the
    /// preview does not bake the box into the pixels, which is what makes the drag feel attached
    /// to the cursor.
    pub fn vector_editing(&self) -> bool {
        self.crop_editing() || self.motion_editing()
    }

    /// 画布上要画的那个框：拖过的用拖出来的，没拖过给当前取景（再兜底整块可见区）。
    ///
    /// The box to draw on the canvas: the dragged one when there is one, otherwise the current
    /// framing (with the whole visible area as a final fallback).
    pub fn crop_box(&self) -> CropRect {
        self.user_crop
            .or_else(|| self.effective_crop())
            .unwrap_or_else(|| self.crop_view())
    }

    /// 画布上画出来的那块像素（旋转后坐标）：整幅，或拉直削出来的安全区。
    /// 裁剪态的覆盖层按它换算屏幕坐标，所以框永远只在有像素的地方走。
    ///
    /// The block of pixels drawn on the canvas (post-rotation coordinates): the whole frame,
    /// or the safe area left after straightening.
    /// The crop-mode overlay converts screen coordinates against it, so the box can never travel
    /// where there are no pixels.
    pub fn crop_view(&self) -> CropRect {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        let full = CropRect { x: 0, y: 0, w, h };
        match self.straighten_bounds(None) {
            Some(safe) => crop::intersect(safe, full).unwrap_or(full),
            None => full,
        }
    }

    /// 拖出来的框不能伸进转出来的空白角：越出去的部分只是反射像素。
    pub(super) fn clamp_to_view(&self, r: CropRect) -> CropRect {
        match self.straighten_bounds(None) {
            Some(safe) => crop::intersect(r, safe).unwrap_or(safe),
            None => r,
        }
    }

    /// 按下画布：命中把手就缩放，命中框内就平移，落在框外就重拉一个框。
    ///
    /// 这里只摆拖拽姿态，不写 `user_crop`、不登记历史——「按下」本身不是编辑。
    /// 框的固化和历史登记都在 `crop_drag_to`：一次拖动 = 一步历史，
    /// 点一下画布什么都不会留下（v0.3.0 之前纯点击也会挤出一条裁剪历史）。
    ///
    /// Mouse down on the canvas: hitting a handle resizes, hitting inside the box pans, and
    /// landing outside starts a new box.
    ///
    /// This only sets the drag posture; it does not write `user_crop` and registers no history --
    /// "mouse down" on its own is not an edit. Committing the box and recording history both
    /// happen in `crop_drag_to`: one drag equals one history step, and clicking the canvas leaves
    /// nothing behind (before v0.3.0 a bare click squeezed out a crop history entry).
    pub fn crop_begin(&mut self, at: [f64; 2], hit: crop::Hit) {
        let cur = self.crop_box();
        self.crop_drag = Some(match hit {
            crop::Hit::Corner(handle) | crop::Hit::Edge(handle) => {
                CropDrag::Resize { handle, orig: cur }
            }
            crop::Hit::Inside => CropDrag::Move {
                orig: cur,
                grab: [at[0] - cur.x as f64, at[1] - cur.y as f64],
            },
            crop::Hit::Outside => CropDrag::Draw { anchor: at },
        });
    }

    /// 拖动中：几何规则全在 `aevum_core::crop`，这里只喂坐标。
    ///
    /// During the drag: all the geometry rules live in `aevum_core::crop`; this only feeds
    /// coordinates.
    pub fn crop_drag_to(&mut self, at: [f64; 2]) {
        let Some(drag) = self.crop_drag else { return };
        let Some((w, h)) = self.resolution else {
            return;
        };
        let aspect = self.aspect.pair();
        let next = match drag {
            CropDrag::Draw { anchor } => {
                crop::draw_rect((anchor[0], anchor[1]), at[0], at[1], w, h, aspect)
            }
            CropDrag::Move { orig, grab } => {
                crop::move_rect(orig, at[0] - grab[0], at[1] - grab[1], w, h)
            }
            CropDrag::Resize { handle, orig } => {
                crop::resize_rect(handle, orig, at[0], at[1], w, h, aspect)
            }
        };
        let next = self.clamp_to_view(next);
        self.user_crop = Some(next);
        // 每帧都记一次同一个键：连续拖动合成一步，文案跟着框的尺寸走。
        history::note(
            "crop-drag",
            crate::i18n::tr_fmt("裁剪 {} × {}", &[&next.w, &next.h]),
        );
    }

    /// 松手：框铺满可见区等于没裁，退回默认取景，省掉一次无谓的重画。
    ///
    /// Mouse up: a box covering the whole visible area means no crop at all, so it falls back to
    /// the default framing and saves a pointless repaint.
    pub fn crop_end(&mut self) {
        self.crop_drag = None;
        let view = self.crop_view();
        if let Some(u) = self.user_crop
            && (u.x, u.y, u.w, u.h) == (view.x, view.y, view.w, view.h)
        {
            self.user_crop = None;
            // 退回默认也是一次取景改动：登记进同一条拖动键，收尾并进这一步，
            // 而不是让字段比对路径在下一帧另起一行「裁剪框」（成对刷屏的另一半）。
            history::note(
                "crop-drag",
                crate::i18n::tr_fmt("裁剪 {} × {}", &[&view.w, &view.h]),
            );
        }
    }

    /// 方向键微调取景框：`step` 一般给 2，按住 Shift 给 10。
    ///
    /// Arrow-key nudge of the viewfinder: `step` is normally 2, and 10 with Shift held.
    pub fn crop_nudge(&mut self, dx: i64, dy: i64, step: u32) {
        let Some((w, h)) = self.resolution else {
            return;
        };
        let cur = self.crop_box();
        self.user_crop = Some(self.clamp_to_view(crop::nudge_rect(cur, dx, dy, step, w, h)));
        history::note(
            "crop-nudge",
            crate::i18n::tr_fmt("裁剪微调 {} {}", &[&format!("{dx:+}"), &format!("{dy:+}")]),
        );
    }

    /// 取消手动裁剪，回到「按比例内接」的默认取景。
    ///
    /// Cancel the manual crop and return to the default "aspect ratio inscribed" framing.
    pub fn crop_reset(&mut self) {
        if self.user_crop.take().is_some() {
            self.crop_drag = None;
            self.preview_dirty = true;
            history::note("crop-reset", "取消裁剪");
        }
    }
}
