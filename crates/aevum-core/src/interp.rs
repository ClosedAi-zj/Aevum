//! 关键帧之间的参数插值。
//!
//! 口径与 Python `acceptance/reference/sequence.interpolate` 对齐：数值线性插值、HSL 数组
//! 逐元素插值、曲线按 x 对齐后在 y 上插值；蒙版按稳定 id 配对后逐项插值。

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::Excluded;

use serde_json::{Map, Value, json};

use crate::grade::GradeParams;
use crate::mask::as_group;

/// 按关键帧插值出第 `index` 帧的参数；没有关键帧时返回 `base`。
pub fn params_at(
    base: &GradeParams,
    keyframes: &BTreeMap<usize, GradeParams>,
    index: usize,
) -> GradeParams {
    if keyframes.is_empty() {
        return base.clone();
    }
    if let Some(p) = keyframes.get(&index) {
        return p.clone();
    }
    let lo = keyframes.range(..index).next_back();
    let hi = keyframes
        .range((Excluded(index), std::ops::Bound::Unbounded))
        .next();
    match (lo, hi) {
        (Some((ia, a)), Some((ib, b))) => {
            // 区间两端都有效，b > a（BTreeMap 有序）
            interpolate(a, b, (index - ia) as f64 / (ib - ia) as f64)
        }
        // 端点之外取最近的关键帧
        (Some((_, a)), None) => a.clone(),
        (None, Some((_, b))) => b.clone(),
        (None, None) => base.clone(),
    }
}

/// 两组参数之间线性插值，`t` 在 `[0, 1]`。
pub fn interpolate(a: &GradeParams, b: &GradeParams, t: f64) -> GradeParams {
    if t <= 0.0 {
        return a.clone();
    }
    if t >= 1.0 {
        return b.clone();
    }
    let mut out = a.clone();

    // 逐字段复制粘贴 59 个标量太容易漏，统一走宏。
    macro_rules! lerp {
        ($($field:ident),+ $(,)?) => {
            $(
                out.$field = a.$field + (b.$field - a.$field) * t;
            )+
        };
    }

    lerp!(
        exposure,
        hl_rolloff,
        contrast,
        contrast_linear,
        black_soft,
        white_soft,
        highlights,
        shadows,
        whites,
        blacks,
        temperature,
        tint,
        vibrance,
        saturation,
        clarity,
        texture,
        dehaze,
        dehaze_dark,
        dehaze_contrast,
        grain,
        grain_size,
        grain_freq,
        grain_lum,
        vignette,
        vig_midpoint,
        vig_roundness,
        vig_feather,
        vig_hl_contrast,
        sharp_amount,
        sharp_radius,
        sharp_detail,
        sharp_masking,
        sharp_protect,
        nr_luma,
        nr_color,
        nr_luma_detail,
        nr_luma_contrast,
        nr_color_smooth,
        nr_multiscale,
        nr_sigma_manual,
        nr_ai,
        grade_global_h,
        grade_global_s,
        grade_global_l,
        grade_sh_h,
        grade_sh_s,
        grade_sh_l,
        grade_mid_h,
        grade_mid_s,
        grade_mid_l,
        grade_hi_h,
        grade_hi_s,
        grade_hi_l,
        grade_balance,
        grade_blending,
        lv_black,
        lv_white,
        lv_gamma,
        lv_out_black,
        lv_out_white,
        par_shadows,
        par_darks,
        par_lights,
        par_highlights,
        par_split1,
        par_split2,
        par_split3,
        mono,
        curve_hdr_tail,
        lens_distort,
        lens_vignette,
        lens_fringe,
    );

    // HSL 与单色调色是定长数组，逐元素插值。
    out.mono_h = lerp8(&a.mono_h, &b.mono_h, t);
    out.hsl_h = lerp8(&a.hsl_h, &b.hsl_h, t);
    out.hsl_s = lerp8(&a.hsl_s, &b.hsl_s, t);
    out.hsl_l = lerp8(&a.hsl_l, &b.hsl_l, t);

    out.curve_rgb = lerp_curve(&a.curve_rgb, &b.curve_rgb, t);
    out.curve_r = lerp_curve(&a.curve_r, &b.curve_r, t);
    out.curve_g = lerp_curve(&a.curve_g, &b.curve_g, t);
    out.curve_b = lerp_curve(&a.curve_b, &b.curve_b, t);

    // 蒙版按稳定 id 配对：几何与参数可以插值，布尔开关按中点切换。
    out.masks = masks_mid(&a.masks, &b.masks, t);

    out
}

fn lerp8(a: &[f64; 8], b: &[f64; 8], t: f64) -> [f64; 8] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

fn mask_key(v: &Value, index: usize) -> String {
    v.get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("__{index}"))
}

fn amount_of(v: &Value) -> f64 {
    match v.get("amount") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(_) => 0.0,
        None => 100.0,
    }
}

fn bool_of(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x.abs() > 1e-12),
        Some(Value::String(s)) => !s.is_empty() && s != "0" && s != "false",
        _ => false,
    }
}

fn lerp_key(
    a: &Map<String, Value>,
    b: &Map<String, Value>,
    key: &str,
    t: f64,
    default: f64,
) -> Option<f64> {
    if !a.contains_key(key) && !b.contains_key(key) {
        return None;
    }
    let va = a.get(key).and_then(Value::as_f64).unwrap_or(default);
    let vb = b.get(key).and_then(Value::as_f64).unwrap_or(va);
    Some(va + (vb - va) * t)
}

fn params_mid(a: &Value, b: &Value, t: f64) -> Value {
    let empty = Map::new();
    let da = a.as_object().unwrap_or(&empty);
    let db = b.as_object().unwrap_or(&empty);
    let keys: BTreeSet<&str> = da.keys().chain(db.keys()).map(String::as_str).collect();
    let mut out = Map::new();
    for k in keys {
        let va = da.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let vb = db.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        out.insert(k.to_owned(), json!(va + (vb - va) * t));
    }
    Value::Object(out)
}

fn mask_mid(a: &Value, b: &Value, t: f64) -> Value {
    let mut out = a.clone();
    let (Some(ao), Some(bo)) = (a.as_object(), b.as_object()) else {
        return out;
    };
    let target = out.as_object_mut().expect("mask_mid 目标不是对象");
    for key in [
        "x1", "y1", "x2", "y2", "feather", "angle", "midpoint", "amount",
    ] {
        if let Some(v) = lerp_key(ao, bo, key, t, 0.0) {
            target.insert(key.to_owned(), json!(v));
        }
    }
    if bool_of(ao.get("invert")) != bool_of(bo.get("invert")) {
        target.insert("invert".to_owned(), json!(if t >= 0.5 { 1 } else { 0 }));
    }
    target.insert(
        "params".to_owned(),
        params_mid(
            a.get("params").unwrap_or(&Value::Null),
            b.get("params").unwrap_or(&Value::Null),
            t,
        ),
    );
    out
}

fn comp_mid(a: &Value, b: &Value, t: f64) -> Value {
    let mut out = a.clone();
    let (Some(ao), Some(bo)) = (a.as_object(), b.as_object()) else {
        return out;
    };
    let target = out.as_object_mut().expect("comp_mid 目标不是对象");
    for key in ["x1", "y1", "x2", "y2", "feather", "angle", "midpoint"] {
        if let Some(v) = lerp_key(ao, bo, key, t, 0.0) {
            target.insert(key.to_owned(), json!(v));
        }
    }
    if bool_of(ao.get("invert")) != bool_of(bo.get("invert")) {
        target.insert("invert".to_owned(), json!(if t >= 0.5 { 1 } else { 0 }));
    }
    let blend = if t < 0.5 {
        ao.get("blend")
    } else {
        bo.get("blend")
    };
    target.insert(
        "blend".to_owned(),
        blend.cloned().unwrap_or_else(|| json!("add")),
    );
    out
}

fn value_map(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn range_mid(a: &Value, b: &Value, t: f64) -> Value {
    let ra = value_map(a.get("range").unwrap_or(&Value::Null));
    let rb = value_map(b.get("range").unwrap_or(&Value::Null));
    let lum_on = if t < 0.5 {
        ra.get("lum_on")
    } else {
        rb.get("lum_on")
    };
    let lerp = |key: &str, default: f64| {
        let va = ra.get(key).and_then(Value::as_f64).unwrap_or(default);
        let vb = rb.get(key).and_then(Value::as_f64).unwrap_or(default);
        json!(va + (vb - va) * t)
    };
    json!({
        "lum_on": if bool_of(lum_on) { 1 } else { 0 },
        "lum_min": lerp("lum_min", 0.0),
        "lum_max": lerp("lum_max", 1.0),
        "lum_feather": lerp("lum_feather", 0.1),
    })
}

fn group_mid(a: &Value, b: &Value, t: f64) -> Value {
    let mut out = a.clone();
    let (Some(ao), Some(bo)) = (a.as_object(), b.as_object()) else {
        return out;
    };
    let target = out.as_object_mut().expect("group_mid 目标不是对象");
    let amount_a = amount_of(&Value::Object(ao.clone()));
    let amount_b = amount_of(&Value::Object(bo.clone()));
    target.insert(
        "amount".to_owned(),
        json!(amount_a + (amount_b - amount_a) * t),
    );
    if bool_of(ao.get("invert")) != bool_of(bo.get("invert")) {
        target.insert("invert".to_owned(), json!(if t >= 0.5 { 1 } else { 0 }));
    }
    let has_range = ao
        .get("range")
        .and_then(Value::as_object)
        .is_some_and(|m| !m.is_empty())
        || bo
            .get("range")
            .and_then(Value::as_object)
            .is_some_and(|m| !m.is_empty());
    if has_range {
        target.insert("range".to_owned(), range_mid(a, b, t));
    }
    target.insert(
        "params".to_owned(),
        params_mid(
            a.get("params").unwrap_or(&Value::Null),
            b.get("params").unwrap_or(&Value::Null),
            t,
        ),
    );

    let ca = a
        .get("components")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let cb = b
        .get("components")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let by_b: BTreeMap<String, Value> = cb
        .iter()
        .enumerate()
        .map(|(j, c)| (mask_key(c, j), c.clone()))
        .collect();
    let mut used = BTreeSet::new();
    let mut comps = Vec::new();
    for (i, c0) in ca.iter().enumerate() {
        let key = mask_key(c0, i);
        let Some(c1) = by_b.get(&key) else {
            if t < 0.5 {
                comps.push(c0.clone());
            }
            continue;
        };
        used.insert(key);
        comps.push(comp_mid(c0, c1, t));
    }
    if t >= 0.5 {
        for (j, c1) in cb.iter().enumerate() {
            if !used.contains(&mask_key(c1, j)) {
                comps.push(c1.clone());
            }
        }
    }
    target.insert("components".to_owned(), Value::Array(comps));
    out
}

fn masks_mid(a: &[Value], b: &[Value], t: f64) -> Vec<Value> {
    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    let grouped = a.iter().chain(b).any(|m| m.get("components").is_some());
    let ma: Vec<Value> = if grouped {
        a.iter().map(as_group).collect()
    } else {
        a.to_vec()
    };
    let mb: Vec<Value> = if grouped {
        b.iter().map(as_group).collect()
    } else {
        b.to_vec()
    };
    let by_b: BTreeMap<String, Value> = mb
        .iter()
        .enumerate()
        .map(|(j, m)| (mask_key(m, j), m.clone()))
        .collect();
    let mut used = BTreeSet::new();
    let mut out = Vec::new();
    for (i, m0) in ma.iter().enumerate() {
        let key = mask_key(m0, i);
        let Some(m1) = by_b.get(&key) else {
            let mut m = m0.clone();
            if let Some(obj) = m.as_object_mut() {
                obj.insert("amount".to_owned(), json!(amount_of(m0) * (1.0 - t)));
            }
            out.push(m);
            continue;
        };
        used.insert(key);
        out.push(if grouped {
            group_mid(m0, m1, t)
        } else {
            mask_mid(m0, m1, t)
        });
    }
    for (j, m1) in mb.iter().enumerate() {
        if used.contains(&mask_key(m1, j)) {
            continue;
        }
        let mut m = m1.clone();
        if let Some(obj) = m.as_object_mut() {
            obj.insert("amount".to_owned(), json!(amount_of(m1) * t));
        }
        out.push(m);
    }
    out
}

/// 两条曲线：x 序列一致就逐点 lerp y；否则取较近的一侧。
fn lerp_curve(a: &[[f64; 2]], b: &[[f64; 2]], t: f64) -> Vec<[f64; 2]> {
    if a.len() == b.len() && a.iter().zip(b).all(|(p, q)| p[0] == q[0]) {
        return a
            .iter()
            .zip(b)
            .map(|(p, q)| [p[0], p[1] + (q[1] - p[1]) * t])
            .collect();
    }
    if t < 0.5 { a.to_vec() } else { b.to_vec() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_copies() {
        let a = GradeParams {
            exposure: -1.0,
            ..Default::default()
        };
        let b = GradeParams {
            exposure: 2.0,
            ..Default::default()
        };
        assert_eq!(interpolate(&a, &b, 0.0).exposure, -1.0);
        assert_eq!(interpolate(&a, &b, 1.0).exposure, 2.0);
    }

    #[test]
    fn mismatched_curve_takes_nearest() {
        let a = GradeParams {
            curve_rgb: vec![[0.0, 0.0], [128.0, 64.0], [255.0, 255.0]],
            ..Default::default()
        };
        let b = GradeParams {
            curve_rgb: vec![[0.0, 10.0], [255.0, 245.0]],
            ..Default::default()
        };
        assert_eq!(interpolate(&a, &b, 0.4).curve_rgb, a.curve_rgb);
        assert_eq!(interpolate(&a, &b, 0.6).curve_rgb, b.curve_rgb);
    }

    #[test]
    fn arrays_are_elementwise() {
        let a = GradeParams {
            hsl_h: [0.0; 8],
            ..Default::default()
        };
        let mut b = GradeParams::default();
        b.hsl_h = [10.0; 8];
        let c = interpolate(&a, &b, 0.25);
        assert!(c.hsl_h.iter().all(|v| (*v - 2.5).abs() < 1e-12));
    }

    #[test]
    fn masks_match_by_id_and_lerp() {
        let mut a = GradeParams::default();
        a.masks = vec![serde_json::json!({
            "id": "m1", "type": "linear", "x1": 0.2, "y1": 0.1,
            "x2": 0.4, "y2": 0.5, "feather": 20.0, "amount": 100.0,
            "params": {"exposure": -1.0},
        })];
        let mut b = GradeParams::default();
        b.masks = vec![serde_json::json!({
            "id": "m1", "type": "linear", "x1": 0.4, "y1": 0.3,
            "x2": 0.6, "y2": 0.9, "feather": 60.0, "amount": 50.0,
            "params": {"exposure": 1.0, "contrast": 20.0},
        })];
        let c = interpolate(&a, &b, 0.25);
        let m = &c.masks[0];
        assert!((m["x1"].as_f64().unwrap() - 0.25).abs() < 1e-9);
        assert!((m["y2"].as_f64().unwrap() - 0.6).abs() < 1e-9);
        assert!((m["feather"].as_f64().unwrap() - 30.0).abs() < 1e-9);
        assert!((m["amount"].as_f64().unwrap() - 87.5).abs() < 1e-9);
        assert!((m["params"]["exposure"].as_f64().unwrap() + 0.5).abs() < 1e-9);
        assert!((m["params"]["contrast"].as_f64().unwrap() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn unmatched_masks_fade_out_and_in() {
        let mut a = GradeParams::default();
        a.masks = vec![serde_json::json!({
            "id": "gone", "type": "radial", "amount": 80.0,
            "params": {"exposure": 1.0},
        })];
        let mut b = GradeParams::default();
        b.masks = vec![serde_json::json!({
            "id": "new", "type": "radial", "amount": 60.0,
            "params": {"exposure": -1.0},
        })];
        let c = interpolate(&a, &b, 0.25);
        assert_eq!(c.masks.len(), 2);
        assert!((c.masks[0]["amount"].as_f64().unwrap() - 60.0).abs() < 1e-9);
        assert!((c.masks[1]["amount"].as_f64().unwrap() - 15.0).abs() < 1e-9);
    }

    #[test]
    fn group_components_interpolate_and_blend_switches() {
        let mut a = GradeParams::default();
        a.masks = vec![serde_json::json!({
            "id": "g", "amount": 100.0,
            "params": {"exposure": 0.0},
            "components": [
                {"id": "c1", "kind": "linear", "x1": 0.5, "y1": 0.2,
                 "x2": 0.5, "y2": 0.6, "feather": 10.0, "blend": "subtract"},
            ],
        })];
        let mut b = GradeParams::default();
        b.masks = vec![serde_json::json!({
            "id": "g", "amount": 100.0,
            "params": {"exposure": 0.0},
            "components": [
                {"id": "c1", "kind": "linear", "x1": 0.5, "y1": 0.4,
                 "x2": 0.5, "y2": 0.8, "feather": 30.0, "blend": "add"},
            ],
        })];
        let early = interpolate(&a, &b, 0.25);
        let component = &early.masks[0]["components"][0];
        assert!((component["y1"].as_f64().unwrap() - 0.25).abs() < 1e-9);
        assert_eq!(component["blend"], "subtract");
        let late = interpolate(&a, &b, 0.75);
        assert_eq!(late.masks[0]["components"][0]["blend"], "add");
    }
}
