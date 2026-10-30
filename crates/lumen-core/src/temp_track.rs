//! 非线性色温轨道：滑块位置 ↔ 参数值的双向分段线性映射。
//!
//! `GradeParams.temperature` 的数值域不变（-100..100），落盘/历史/XMP 语义零变化；
//! 映射只作用于**滑块位置**：人手在轨道中段（常调区）每挪一像素对应的色温变化更小、
//! 两端（极端调法）更粗，中段密两端疏，锚点关于 0 对称。数字输入框、历史、落盘、
//! XMP 全部直接用原始值 `v`，serde 零影响。
//!
//! **退表机制**：把 [`TempTrack::LINEAR`] 换回去即恢复 v0.3.0 的线性轨道——
//! 表是数据不是算法，加一张色调表（tint）也只是换个锚点数组。
//!
//! 插值用分段线性而非 pchip：锚点表本身已是感知曲线，线性段足够平滑，
//! 单调、无过冲、正反可逆，退表成本最低。

/// 一条轨道：名字 + 锚点表 `[pos, val]`（按 pos 升序、单调、可逆）。
#[derive(Clone, Copy, Debug)]
pub struct TempTrack {
    pub name: &'static str,
    pub points: &'static [[f64; 2]],
}

impl TempTrack {
    /// 线性轨道（退表用）：v0.3.0 的行为，pos == val。
    pub const LINEAR: TempTrack = TempTrack {
        name: "linear",
        points: &[[-100.0, -100.0], [0.0, 0.0], [100.0, 100.0]],
    };

    /// 色温轨道（M2）：中段密两端疏，锚点关于 0 对称。
    ///
    /// ```text
    /// p:  -100  -70  -45  -25  -12  -6   0   6   12   25   45   70  100
    /// v:  -100  -85  -68  -48  -30  -16  0  16   30   48   68   85  100
    /// ```
    pub const COLOR_TEMPERATURE: TempTrack = TempTrack {
        name: "color_temperature",
        points: &[
            [-100.0, -100.0],
            [-70.0, -85.0],
            [-45.0, -68.0],
            [-25.0, -48.0],
            [-12.0, -30.0],
            [-6.0, -16.0],
            [0.0, 0.0],
            [6.0, 16.0],
            [12.0, 30.0],
            [25.0, 48.0],
            [45.0, 68.0],
            [70.0, 85.0],
            [100.0, 100.0],
        ],
    };

    /// 滑块位置 `p` → 参数值 `v`。
    pub fn pos_to_val(&self, p: f64) -> f64 {
        track_interp(self.points, p)
    }

    /// 参数值 `v` → 滑块位置 `p`（同一张表按 (val, pos) 反查，要求表单调）。
    pub fn val_to_pos(&self, v: f64) -> f64 {
        // 值列与位置列同升序是表契约（COLOR_TEMPERATURE/LINEAR 都满足），
        // 反查直接在值列上走同一套分段插值——手动扫段，不给拖动热路径留
        // 每帧一次的 swapped Vec 分配。
        let pts = self.points;
        debug_assert!(!pts.is_empty(), "轨道锚点表不能为空");
        debug_assert!(
            pts.windows(2).all(|w| w[0][1] <= w[1][1]),
            "轨道值列必须升序才能反查"
        );
        let first = pts[0];
        let last = pts[pts.len() - 1];
        if v <= first[1] {
            return first[0];
        }
        if v >= last[1] {
            return last[0];
        }
        let i = pts.partition_point(|pt| pt[1] <= v) - 1;
        let a = pts[i];
        let b = pts[i + 1];
        let t = (v - a[1]) / (b[1] - a[1]).max(1e-12);
        a[0] + (b[0] - a[0]) * t
    }
}

/// 在按 `[0]` 升序的点表上做分段线性插值，两端外夹取边界值。
fn track_interp(points: &[[f64; 2]], x: f64) -> f64 {
    debug_assert!(!points.is_empty(), "轨道锚点表不能为空");
    let first = points[0];
    let last = points[points.len() - 1];
    if x <= first[0] {
        return first[1];
    }
    if x >= last[0] {
        return last[1];
    }
    // x 严格落在 (first[0], last[0]) 内：partition_point 找到第一个大于 x 的段起点，
    // 退一步即 x 所在段的左端点。
    let i = points.partition_point(|pt| pt[0] <= x) - 1;
    let a = points[i];
    let b = points[i + 1];
    let t = (x - a[0]) / (b[0] - a[0]).max(1e-12);
    a[1] + (b[1] - a[1]) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两端点恒等：轨道不改变边界值。
    #[test]
    fn endpoints_are_identity() {
        for track in [TempTrack::LINEAR, TempTrack::COLOR_TEMPERATURE] {
            for p in [-100.0, 100.0] {
                assert_eq!(track.pos_to_val(p), p, "{} 端点变了", track.name);
                assert_eq!(track.val_to_pos(p), p, "{} 端点反查变了", track.name);
            }
        }
    }

    /// 中点对称：锚点表关于 0 对称，正负互为镜像。
    #[test]
    fn midpoint_is_symmetric() {
        let t = TempTrack::COLOR_TEMPERATURE;
        for p in [0.0, -6.0, -12.0, -25.0, -45.0, -70.0, 6.0, 12.0, 25.0, 45.0, 70.0] {
            let v = t.pos_to_val(p);
            assert_eq!(t.pos_to_val(-p), -v, "{} 在 ±{p} 不对称", t.name);
            assert_eq!(t.val_to_pos(-v), -p, "{} 反查在 ∓{v} 不对称", t.name);
        }
    }

    /// 可逆性：密集网格上 val_to_pos(pos_to_val(p)) 回到 p。
    #[test]
    fn roundtrip_is_identity() {
        for track in [TempTrack::LINEAR, TempTrack::COLOR_TEMPERATURE] {
            let n = 2001;
            for k in 0..n {
                let p = -100.0 + 200.0 * k as f64 / (n - 1) as f64;
                let v = track.pos_to_val(p);
                let back = track.val_to_pos(v);
                assert!(
                    (back - p).abs() < 1e-9,
                    "{} 在 p={p} 往返漂移：{back}",
                    track.name
                );
            }
        }
    }

    /// 单调不减：值随位置单调，保证「拖到哪是哪」没有倒挂。
    #[test]
    fn values_are_monotonic() {
        for track in [TempTrack::LINEAR, TempTrack::COLOR_TEMPERATURE] {
            let mut prev = f64::NEG_INFINITY;
            for k in 0..=2000 {
                let p = -100.0 + 200.0 * k as f64 / 2000.0;
                let v = track.pos_to_val(p);
                assert!(v >= prev - 1e-12, "{} 在 p={p} 倒挂", track.name);
                prev = v;
            }
        }
    }

    /// 线性轨道就是恒等映射（退表即回 v0.3.0 行为）。
    #[test]
    fn linear_track_is_identity() {
        for k in 0..=400 {
            let p = -100.0 + 200.0 * k as f64 / 400.0;
            assert!((TempTrack::LINEAR.pos_to_val(p) - p).abs() < 1e-12);
        }
    }

    /// 锚点处必须取表值本身（分段边界不能漂）。
    #[test]
    fn anchor_points_exact() {
        let t = TempTrack::COLOR_TEMPERATURE;
        for [p, v] in t.points {
            assert_eq!(t.pos_to_val(*p), *v);
            assert_eq!(t.val_to_pos(*v), *p);
        }
    }
}
