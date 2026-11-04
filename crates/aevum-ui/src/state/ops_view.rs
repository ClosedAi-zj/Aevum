//! 工具、模块与视图状态：当前工具、模块、画幅与旋转角的切换，是界面的状态出口。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Tool, module and view state: Switching the current tool, module, aspect and rotation -- the UI state outputs.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    pub fn set_aspect(&mut self, a: Aspect) {
        if self.aspect == a {
            return;
        }
        self.aspect = a;
        // 比例变了，用户框绕自己的中心重铺，不把它甩到别处去。
        if let (Some(u), Some((w, h))) = (self.user_crop, self.resolution) {
            self.user_crop = Some(crop::refit_rect(u, w, h, a.pair()));
        }
        // 运镜的两个框不用重铺：取景基准就是导出取景那一幅，画幅一改基准自己就换形，
        // 框按归一化跟着走，像素比例自动贴上新比例。再 refit 一次就是双重收缩，
        // 选好的构图会凭空多推近一档。
        self.preview_dirty = true;
    }

    /// 拉直：把整幅按 `deg` 度（正角=画面逆时针）转过去。0 附近吸一下，
    /// 因为推子停在 -0.3 这种值上时，人想要的一定是「没转」。
    ///
    /// Straighten: rotate the whole frame by `deg` degrees (positive = image
    /// counter-clockwise). It snaps near 0, because when the fader sits at -0.3 the user
    /// certainly meant "not rotated".
    pub fn set_rotate(&mut self, deg: f64) {
        let snapped = if deg.abs() <= 0.2 { 0.0 } else { deg };
        if (self.rotate - snapped).abs() <= 1e-6 {
            return;
        }
        self.rotate = snapped;
        // 角度是绕中心转的，安全框只会往里收：已有框伸进新空白角的话当场削回来，
        // 免得画布上一个框、导出另一个框。
        if let Some(cur) = self.user_crop {
            self.user_crop = Some(self.clamp_to_view(cur));
        }
        self.preview_dirty = true;
    }

    pub fn set_tool(&mut self, t: Tool) {
        // 裁剪工具和运镜拖框抢的是同一只手鼠标：切到裁剪就退出取景框编辑。
        if t == Tool::Crop {
            self.set_motion_picking(false);
        }
        // 修复画笔与四角拖拽抢的也是同一只手：选了画笔就退出四角编辑，否则拖下去
        // 只会落一笔到画面上，而四边形一动不动。
        //
        // The repair brush and corner dragging compete for the same hand: picking the brush
        // leaves corner mode, otherwise dragging lays a stroke while the quad never moves.
        if t == Tool::Heal && self.upright_editing {
            self.upright_edit_toggle();
        }
        let before = (self.crop_editing(), self.motion_editing());
        self.tool = t;
        if before != (self.crop_editing(), self.motion_editing()) {
            self.crop_drag = None;
            self.motion_drag = None;
            self.preview_dirty = true;
        }
    }

    pub fn set_module(&mut self, m: Module) {
        let before = (self.crop_editing(), self.motion_editing());
        self.module = m;
        if before != (self.crop_editing(), self.motion_editing()) {
            self.crop_drag = None;
            self.motion_drag = None;
            self.preview_dirty = true;
        }
    }
}
