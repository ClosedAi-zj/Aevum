//! 运镜框：运镜框的拾取、拖拽、吸附、缓动与复位。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! The motion box: Picking, dragging, snapping, easing and resetting the motion box.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    /// 画布是不是在拖运镜取景框：这时预览画的是取景基准那一幅（不是框里的内容），
    /// 两个框只是盖在上面的矢量，所以拖动一个像素都不重画。裁剪态优先——
    /// 同一时刻只有一层矢量框拿鼠标。
    ///
    /// Whether the canvas is dragging the motion viewfinder: the preview then draws the reference
    /// frame (not the contents of the box), and the two boxes are only a vector layer on top, so
    /// dragging repaints nothing.
    /// Crop mode wins -- only one vector layer holds the mouse at a time.
    pub fn motion_editing(&self) -> bool {
        self.motion_picking && !self.crop_editing()
    }

    /// 开关画布的取景框编辑：进入时要重画一次「整幅」预览，离开时要重画回
    /// 框里的内容，两头都得脏一次。
    ///
    /// Toggle canvas viewfinder editing: entering must repaint the whole-frame preview once,
    /// and leaving must repaint the contents of the box once -- both ends need one dirty
    /// frame.
    pub fn set_motion_picking(&mut self, on: bool) {
        if self.motion_picking == on {
            return;
        }
        self.motion_picking = on;
        // 与裁剪工具抢同一只手鼠标：哪一头开，另一头的拖拽就当场作废。
        if on {
            self.crop_drag = None;
            if self.crop_editing() {
                self.tool = Tool::Hand;
            }
        } else {
            self.motion_drag = None;
        }
        self.preview_dirty = true;
    }

    pub fn set_motion_box(&mut self, which: MotionBox) {
        if self.motion_box == which {
            return;
        }
        self.motion_box = which;
        self.motion_drag = None;
    }

    /// 取景框归一化的基准那一幅（拉直 + 导出取景之后的像素框，对齐坐标）。
    /// 画布把预览铺回这一幅，框才画在它对得上的位置上。
    ///
    /// The frame the viewfinder is normalised against (the pixel box after straightening and
    /// export cropping, in align coordinates).
    /// The canvas lays the preview back onto this frame, which is what puts the box where it
    /// visually belongs.
    pub fn motion_base_rect(&self) -> CropRect {
        let (w, h) = self.resolution.unwrap_or((1, 1));
        self.effective_crop()
            .unwrap_or(CropRect { x: 0, y: 0, w, h })
    }

    /// 取景框换算成基准那一幅里的整数像素框（对齐坐标，带基准原点偏移）。
    /// 画布画它、拖它都走这个口径，归一化与像素的往返只在这里发生一次。
    ///
    /// Convert a viewfinder into an integer pixel box inside the reference frame (align
    /// coordinates, with the reference origin offset applied).
    /// Drawing and dragging both go through this definition; the normalised to pixel round trip
    /// happens exactly once, here.
    pub fn motion_rect_of(&self, v: Viewfinder) -> CropRect {
        let base = self.motion_base_rect();
        let v = v.normalized();
        let local = crop::norm_rect(
            v.x * f64::from(base.w),
            v.y * f64::from(base.h),
            (v.x + v.w) * f64::from(base.w),
            (v.y + v.h) * f64::from(base.h),
            base.w,
            base.h,
        );
        CropRect {
            x: base.x + local.x,
            y: base.y + local.y,
            w: local.w,
            h: local.h,
        }
    }

    /// 某一端当前占的那块像素。
    ///
    /// The pixels either end currently occupies.
    pub fn motion_box_rect(&self, which: MotionBox) -> CropRect {
        self.motion_rect_of(match which {
            MotionBox::Start => self.motion.from,
            MotionBox::End => self.motion.to,
        })
    }

    /// 选中帧上运镜走到的那一格：画布上给一条「现在在哪」的参照框。
    ///
    /// Where the motion has reached on the selected frame: a reference box on the canvas saying
    /// "you are here".
    pub fn motion_current_rect(&self) -> Option<CropRect> {
        let n = self.frame_count();
        self.motion_view_at(self.selected.min(n.saturating_sub(1)), n)
            .map(|v| self.motion_rect_of(v))
    }

    /// 拖出来的整数像素框写回归一化取景框。基准尺寸不整除时按像素认，
    /// 画布上看到的就是最后留下的画面。
    fn motion_box_write(&mut self, r: CropRect) {
        let base = self.motion_base_rect();
        let (bw, bh) = (f64::from(base.w).max(1.0), f64::from(base.h).max(1.0));
        let v = Viewfinder {
            x: (f64::from(r.x) - f64::from(base.x)) / bw,
            y: (f64::from(r.y) - f64::from(base.y)) / bh,
            w: f64::from(r.w) / bw,
            h: f64::from(r.h) / bh,
        }
        .normalized();
        if self.motion_box == MotionBox::Start {
            self.motion.from = v;
        } else {
            self.motion.to = v;
        }
    }

    /// 运镜框的比例钉在输出画幅上：框和最后那一条一样形，画布上看到的就是成片，
    /// 免得拖完还要在心里再裁一次。
    ///
    /// 比要约到最简（3840×2160 → 16:9）：`crop` 的定比例几何以「两个单位」为最小
    /// 步长，直接拿像素尺寸当比例会算出一个比画面还宽的框，落点就被夹飞了。
    /// 约完仍然撑不进基准那一幅（小图配超宽画幅）就退回自由比例，让 `view_window`
    /// 在导出时收那一格。
    fn motion_box_aspect(&self) -> Option<(u32, u32)> {
        let (ow, oh) = self.motion_output_size();
        let (fw, fh) = self.motion_frame_size();
        if ow < 2 || oh < 2 {
            return None;
        }
        let (mut a, mut b) = (ow, oh);
        while b != 0 {
            let t = a % b;
            a = b;
            b = t;
        }
        let (num, den) = (ow / a.max(1), oh / a.max(1));
        (num * 2 <= fw && den * 2 <= fh).then_some((num, den))
    }

    /// 按下画布：把手=缩放，框内=平移，框外=重拉。与裁剪同一套判定，只是坐标
    /// 换算落在基准那一幅上、落账写进 `motion.from` 或 `motion.to`。
    ///
    /// 与 `crop_begin` 同一条规矩：按下只摆姿态，不落账不记历史。「点一下就
    /// 偷偷启用运镜并挤出一条历史」是 v0.3.0 的实测毛病；启用发生在真的拖动时。
    ///
    /// Mouse down on the canvas: handle = resize, inside = pan, outside = start a new box. The
    /// same test as cropping, except the coordinate conversion lands on the reference frame and
    /// the result is written into `motion.from` or `motion.to`.
    ///
    /// Same rule as `crop_begin`: mouse down only sets the posture, writes nothing and records no
    /// history. "One click silently enables the motion and squeezes out a history entry" was a
    /// real problem measured in v0.3.0; enabling happens on an actual drag.
    pub fn motion_begin(&mut self, at: [f64; 2], hit: crop::Hit) {
        let cur = self.motion_box_rect(self.motion_box);
        self.motion_drag = Some(match hit {
            crop::Hit::Corner(handle) | crop::Hit::Edge(handle) => {
                CropDrag::Resize { handle, orig: cur }
            }
            crop::Hit::Inside => CropDrag::Move {
                orig: cur,
                grab: [at[0] - f64::from(cur.x), at[1] - f64::from(cur.y)],
            },
            crop::Hit::Outside => CropDrag::Draw { anchor: at },
        });
    }

    /// 拖动中：只改草稿对应的取景框，不重画预览。
    ///
    /// During the drag: only the draft's viewfinder changes, and the preview is not
    /// repainted.
    pub fn motion_drag_to(&mut self, at: [f64; 2]) {
        let Some(drag) = self.motion_drag else { return };
        let base = self.motion_base_rect();
        if base.w < 2 || base.h < 2 {
            return;
        }
        let aspect = self.motion_box_aspect();
        // 拖拽坐标是基准那一幅的局部像素，几何就在局部里算，宽高自然兜得住。
        let (bx, by) = (f64::from(base.x), f64::from(base.y));
        let local = |r: CropRect| CropRect {
            x: r.x - base.x,
            y: r.y - base.y,
            w: r.w,
            h: r.h,
        };
        let next = match drag {
            CropDrag::Draw { anchor } => crop::draw_rect(
                (anchor[0] - bx, anchor[1] - by),
                at[0] - bx,
                at[1] - by,
                base.w,
                base.h,
                aspect,
            ),
            CropDrag::Move { orig, grab } => crop::move_rect(
                local(orig),
                at[0] - bx - grab[0],
                at[1] - by - grab[1],
                base.w,
                base.h,
            ),
            CropDrag::Resize { handle, orig } => crop::resize_rect(
                handle,
                local(orig),
                at[0] - bx,
                at[1] - by,
                base.w,
                base.h,
                aspect,
            ),
        };
        // 开关没推上去就拖：框是有效的，运镜就该生效，不能让人白拖一趟。
        // 放在真拖动这里而不是 begin：纯点击不启用、不记历史。
        self.motion.enabled = true;
        self.motion_box_write(CropRect {
            x: base.x + next.x,
            y: base.y + next.y,
            w: next.w,
            h: next.h,
        });
        history::note(
            "motion-drag",
            crate::i18n::tr_fmt(
                "运镜{} {} × {}",
                &[&crate::i18n::tr(self.motion_box.cn()), &next.w, &next.h],
            ),
        );
    }

    /// 松手：预览不用重画（拖的那一层是矢量），只把这次的尺寸留在历史上。
    ///
    /// Mouse up: the preview needs no repaint (the dragged layer is a vector); only this size is
    /// left in the history.
    pub fn motion_end(&mut self) {
        self.motion_drag = None;
    }

    /// 方向键微调当前拖的那一端：`step` 一般给 2，按住 Shift 给 10。只在取景框编辑态用。
    ///
    /// Arrow-key nudge of the end being dragged: `step` is normally 2, and 10 with Shift held.
    /// Used only in viewfinder editing mode.
    pub fn motion_nudge(&mut self, dx: i64, dy: i64, step: u32) {
        let base = self.motion_base_rect();
        if base.w < 2 || base.h < 2 {
            return;
        }
        let cur = self.motion_box_rect(self.motion_box);
        let local = CropRect {
            x: cur.x - base.x,
            y: cur.y - base.y,
            w: cur.w,
            h: cur.h,
        };
        let moved = crop::nudge_rect(local, dx, dy, step, base.w, base.h);
        self.motion_box_write(CropRect {
            x: base.x + moved.x,
            y: base.y + moved.y,
            w: moved.w,
            h: moved.h,
        });
        history::note(
            "motion-nudge",
            crate::i18n::tr_fmt(
                "运镜{}微调 {} {}",
                &[
                    &crate::i18n::tr(self.motion_box.cn()),
                    &format!("{dx:+}"),
                    &format!("{dy:+}"),
                ],
            ),
        );
    }

    /// 开关运镜：关掉就是不动，预览得从「框里的内容」重画回整幅。
    ///
    /// Toggle the motion: turning it off means no movement, so the preview must repaint from the
    /// contents of the box back to the whole frame.
    pub fn set_motion_enabled(&mut self, on: bool) {
        if self.motion.enabled == on {
            return;
        }
        self.motion.enabled = on;
        self.preview_dirty = true;
        history::note("motion-toggle", if on { "开启运镜" } else { "关闭运镜" });
    }

    pub fn set_motion_ease(&mut self, e: Ease) {
        if self.motion.ease == e {
            return;
        }
        self.motion.ease = e;
        self.preview_dirty = true;
        history::note(
            "motion-ease",
            crate::i18n::tr_fmt("运动曲线 {}", &[&crate::i18n::tr(e.cn())]),
        );
    }

    /// 起点终点互换：倒放同一条运动，框不用重拖。
    ///
    /// Swap start and end: plays the same movement backwards without dragging the boxes
    /// again.
    pub fn motion_swap(&mut self) {
        std::mem::swap(&mut self.motion.from, &mut self.motion.to);
        self.preview_dirty = true;
        history::note("motion-swap", "运镜反向");
    }

    /// 把当前拖的那一端铺满整幅：想「从全景推到细节」时先按一下，省得拉把手拉半天。
    ///
    /// Fill the whole frame with the end being dragged: press this first when you want a push-in
    /// from the whole view to a detail, instead of hauling the handle around.
    pub fn motion_fill_box(&mut self) {
        match self.motion_box {
            MotionBox::Start => self.motion.from = crop::FULL,
            MotionBox::End => self.motion.to = crop::FULL,
        }
        self.motion.enabled = true;
        self.preview_dirty = true;
        history::note(
            "motion-fill",
            crate::i18n::tr_fmt("运镜{}铺满", &[&crate::i18n::tr(self.motion_box.cn())]),
        );
    }

    /// 清除运镜：整块回到默认（关着、起点整幅、终点往里收一档）。
    ///
    /// Clear the motion: the whole block returns to its defaults (off, start = whole frame, end
    /// inset by one step).
    pub fn motion_reset(&mut self) {
        if self.motion == Motion::default() {
            return;
        }
        self.motion = Motion::default();
        self.motion_drag = None;
        self.preview_dirty = true;
        history::note("motion-reset", "清除运镜");
    }
}
