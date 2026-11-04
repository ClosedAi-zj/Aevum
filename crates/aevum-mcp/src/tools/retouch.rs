//! 修复与几何域：修复笔触、透视校正在会话参数上的增删与查询。
//!
//! 边界：只改 `GradeParams::retouch` 与 `GradeParams::upright` 两个字段，不碰像素、
//! 不写文件。真正把它们变成画面的是 `export` 那条渲染链——本域只负责把「修哪儿、
//! 拉多正」记下来。`auto` 那一路例外：它需要读一张缩图来估消失点，读的是源文件，
//! 但**不写**源文件。
//!
//! 坐标一律是**归一化**的（`x` 相对宽、`y` 相对高，半径与仿制偏移相对长边），
//! 因为同一份参数要在预览（1600 边）与导出（6000 边）上落到同一块区域。给像素
//! 坐标的调用方会被明确拒绝，而不是换算成一个大得离谱的笔触。
//!
//! The repair and geometry domain: adding, removing and querying healing strokes and
//! perspective correction on the session parameters.
//!
//! Boundary: it only touches `GradeParams::retouch` and `GradeParams::upright`; it never
//! touches pixels and never writes files. Turning them into an image is the `export` render
//! path — this domain only records "where to repair, how straight". The `auto` path is the
//! exception: it reads a downscaled frame to estimate the vanishing point, but never writes
//! the source.
//!
//! Coordinates are always **normalized** (`x` against the width, `y` against the height, the
//! radius and clone offset against the long edge), because one parameter set has to land on
//! the same area in the preview (1600px) and the export (6000px). Callers passing pixel
//! coordinates are rejected outright rather than turned into an absurdly large stroke.

use aevum_core::metrics;
use aevum_core::perspective::{self, Quad};
use aevum_core::retouch::SpotSpec;
use serde_json::{Value, json};

use super::shared::{echo_text, opt_str, opt_u32};
use crate::session::Session;

/// 自动直立读图时的长边。够估消失点（`auto_upright` 自己会再缩到 640），
/// 又不会为了找两条线付一次全幅解码。
///
/// Long edge used when auto-upright decodes a frame. Enough to estimate a vanishing point
/// (`auto_upright` shrinks to 640 itself) without paying for a full-size decode to find two
/// lines.
const AUTO_SIDE: u32 = 1200;

/// 修复笔触的增删查。
///
/// Add, remove and list healing strokes.
pub(super) fn retouch_edit(session: &mut Session, args: &Value) -> Result<Value, String> {
    let op = opt_str(args, "op")?.unwrap_or("list");
    match op {
        "list" => {}
        "add" => {
            let list = args
                .get("spots")
                .and_then(|v| v.as_array())
                .ok_or_else(|| "add 需要 spots 数组".to_owned())?;
            if list.is_empty() {
                return Err("spots 是空数组：没有要加的笔触".to_owned());
            }
            // 先全量校验再落地：一半加进去、一半报错，会让调用方以为整批都成了。
            //
            // Validate everything before writing: half a batch applied plus an error leaves
            // the caller thinking the whole batch went in.
            let mut parsed = Vec::with_capacity(list.len());
            for (i, item) in list.iter().enumerate() {
                parsed.push(parse_spot(item).map_err(|e| format!("spots[{i}]：{e}"))?);
            }
            session.params.retouch.extend(parsed);
        }
        "undo" => {
            let n = opt_u32(args, "count")?.unwrap_or(1) as usize;
            if n == 0 {
                return Err("count 至少是 1".to_owned());
            }
            let have = session.params.retouch.len();
            if have == 0 {
                return Err("当前没有修复笔触可撤".to_owned());
            }
            let cut = n.min(have);
            session.params.retouch.truncate(have - cut);
        }
        "clear" => {
            if session.params.retouch.is_empty() {
                return Ok(json!({"count": 0, "spots": [], "note": "本来就没有笔触"}));
            }
            session.params.retouch.clear();
        }
        other => {
            return Err(format!(
                "op 只能是 list/add/undo/clear，收到 {}",
                echo_text(other)
            ));
        }
    }
    Ok(json!({
        "count": session.params.retouch.len(),
        "spots": session.params.retouch.iter().map(|s| json!({
            "x": s.x, "y": s.y, "r": s.r, "feather": s.feather,
            "source": s.source.map(|(dx, dy)| json!([dx, dy])),
        })).collect::<Vec<_>>(),
    }))
}

/// 解析一个笔触。越界口径分两级：
///
/// - 几何/索引类（坐标 x/y、quad 角点、帧下标）：越界一律报错而不是悄悄钳 ——
///   钳完之后 agent 以为设成了 A，用户看到的是 B。
/// - 旋钮类（radius/feather/ratio 等连续量）：允许钳位，不存在「以为设成了 A」
///   的歧义风险。
///
/// Parse one stroke. The out-of-range policy has two tiers:
///
/// - Geometry/index values (x/y coordinates, quad corners, frame indices): out-of-range
///   is always an error rather than a silent clamp — a clamp leaves the agent believing
///   it set A while the user sees B.
/// - Knob-like continuous quantities (radius/feather/ratio etc.): clamping is allowed,
///   since there is no "thought it set A" ambiguity.
fn parse_spot(item: &Value) -> Result<SpotSpec, String> {
    let num = |key: &str| -> Result<f32, String> {
        let v = item
            .get(key)
            .and_then(|v| v.as_f64())
            .ok_or_else(|| format!("缺少 {key}（数字，归一化）"))?;
        Ok(v as f32)
    };
    let x = num("x")?;
    let y = num("y")?;
    let r = num("r")?;
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err(format!("x/y 必须在 [0,1]（归一化坐标），收到 {x}/{y}"));
    }
    // 先判 is_finite：NaN 两个比较都为假，只写 `r <= 0.0` 会让 NaN 溜过去，
    // 而 NaN 半径一路传到泊松求解就是整幅画面按分钟算。
    //
    // Check is_finite first: both comparisons are false for NaN, so a bare `r <= 0.0`
    // would let NaN through, and a NaN radius reaching the Poisson solve means a
    // full-frame job measured in minutes.
    if !r.is_finite() || r <= 0.0 || r > SpotSpec::MAX_RADIUS {
        return Err(format!(
            "r 必须在 (0, {}]（相对长边），收到 {r}",
            SpotSpec::MAX_RADIUS
        ));
    }
    let feather = item
        .get("feather")
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(0.35);
    if !(0.0..=1.0).contains(&feather) {
        return Err(format!("feather 必须在 [0,1]，收到 {feather}"));
    }
    let source = match item.get("source") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let pair = v
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or_else(|| "source 必须是 [dx, dy]（相对长边）".to_owned())?;
            let dx = pair[0]
                .as_f64()
                .ok_or_else(|| "source[0] 不是数字".to_owned())? as f32;
            let dy = pair[1]
                .as_f64()
                .ok_or_else(|| "source[1] 不是数字".to_owned())? as f32;
            if !dx.is_finite() || !dy.is_finite() || (dx == 0.0 && dy == 0.0) {
                return Err("source 的偏移不能是 0 或非有限值（那等于原地复制）".to_owned());
            }
            Some((dx, dy))
        }
    };
    Ok(SpotSpec {
        x,
        y,
        r,
        feather,
        source,
    })
}

/// 透视校正的读写。
///
/// Read and write the perspective correction.
pub(super) fn upright_set(session: &mut Session, args: &Value) -> Result<Value, String> {
    let op = opt_str(args, "op")?.unwrap_or("get");
    match op {
        "get" => {}
        "set" => {
            let quad = args.get("quad").and_then(|v| v.as_array()).ok_or_else(|| {
                "set 需要 quad：四个 [x, y] 角点（左上→右上→右下→左下）".to_owned()
            })?;
            if quad.len() != 4 {
                return Err(format!("quad 需要 4 个角点，收到 {}", quad.len()));
            }
            let mut p = [(0.0f64, 0.0f64); 4];
            for (i, item) in quad.iter().enumerate() {
                let pair = item
                    .as_array()
                    .filter(|a| a.len() == 2)
                    .ok_or_else(|| format!("quad[{i}] 不是 [x, y]"))?;
                let x = pair[0]
                    .as_f64()
                    .ok_or_else(|| format!("quad[{i}][0] 不是数字"))?;
                let y = pair[1]
                    .as_f64()
                    .ok_or_else(|| format!("quad[{i}][1] 不是数字"))?;
                if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
                    return Err(format!("quad[{i}] 越界：({x}, {y}) 不在 [0,1] 内"));
                }
                p[i] = (x, y);
            }
            let q = Quad { p };
            q.clamped().ok_or_else(|| "四边形含非法值".to_owned())?;
            // 退化四边形（三点共线、面积为零）在这里就拒掉：放行等于让渲染阶段
            // 静默不动作，调用方还以为是设成功了。
            //
            // A degenerate quad (three collinear points, zero area) is rejected here: letting
            // it through would make the render stage silently do nothing while the caller
            // believes the set succeeded.
            if perspective::homography(&q).is_none() {
                return Err("四边形退化（三点共线或面积为零），无法求单应".to_owned());
            }
            session.params.upright = Some(q);
        }
        "auto" => {
            let frame = opt_u32(args, "frame")?.unwrap_or(0) as usize;
            let path = {
                let seq = session.require_seq()?;
                if seq.is_empty() {
                    // 空序列时 `seq.len() - 1` 会下溢，先在这里挡掉。
                    //
                    // An empty sequence would make `seq.len() - 1` underflow; block it here first.
                    return Err("序列没有帧".to_owned());
                }
                if frame >= seq.len() {
                    return Err(format!("frame {frame} 超出范围 0..{}", seq.len() - 1));
                }
                seq.frames[frame].path.clone()
            };
            let rgb =
                metrics::decode_rgb(&path, AUTO_SIDE).map_err(|e| format!("读图失败：{e}"))?;
            let (w, h) = (rgb.width() as usize, rgb.height() as usize);
            let buf: Vec<f32> = rgb.as_raw().iter().map(|v| *v as f32 / 255.0).collect();
            let q = perspective::auto_upright(&buf, w, h).ok_or_else(|| {
                "画面里没有可用的竖直主直线（竖线少于两条、近乎平行、或消失点不可靠）".to_owned()
            })?;
            let q = q.clamped().ok_or_else(|| "算出的四边形不合法".to_owned())?;
            session.params.upright = Some(q);
        }
        "clear" => {
            session.params.upright = None;
        }
        other => {
            return Err(format!(
                "op 只能是 get/set/auto/clear，收到 {}",
                echo_text(other)
            ));
        }
    }
    Ok(json!({
        "upright": session.params.upright.map(|q| json!(q.p)),
        "identity": session.params.upright.is_none_or(|q| q.is_identity()),
        "note": "四边形是归一化坐标（左上→右上→右下→左下）；null 表示不校正",
    }))
}
