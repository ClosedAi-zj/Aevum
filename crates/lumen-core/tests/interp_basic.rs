//! 关键帧插值：中点、端点、数组/曲线、无关键帧回退。

use std::collections::BTreeMap;

use lumen_core::grade::GradeParams;
use lumen_core::interp::{interpolate, params_at};
use serde_json::Value;

fn kf(pairs: &[(usize, GradeParams)]) -> BTreeMap<usize, GradeParams> {
    pairs.iter().cloned().collect()
}

#[test]
fn midpoint_and_endpoints() {
    let base = GradeParams {
        exposure: -3.0,
        ..Default::default()
    };
    let mut a = GradeParams::default();
    a.exposure = 0.0;
    a.contrast = 10.0;
    let mut b = GradeParams::default();
    b.exposure = 2.0;
    b.contrast = 30.0;
    let keyframes = kf(&[(0, a.clone()), (10, b.clone())]);

    let mid = params_at(&base, &keyframes, 5);
    assert!((mid.exposure - 1.0).abs() < 1e-12);
    assert!((mid.contrast - 20.0).abs() < 1e-12);

    // 关键帧本身与两端之外都取最近的一侧，不套 base
    assert_eq!(params_at(&base, &keyframes, 0), a);
    assert_eq!(params_at(&base, &keyframes, 10), b);
    assert_eq!(params_at(&base, &keyframes, 99), b);
}

#[test]
fn no_keyframes_falls_back_to_base() {
    let base = GradeParams {
        exposure: 0.7,
        saturation: 12.0,
        ..Default::default()
    };
    assert_eq!(params_at(&base, &BTreeMap::new(), 42), base);
}

#[test]
fn single_keyframe_wins_everywhere() {
    let base = GradeParams::default();
    let mut only = GradeParams::default();
    only.temperature = 33.0;
    let keyframes = kf(&[(7, only.clone())]);
    assert_eq!(params_at(&base, &keyframes, 0), only);
    assert_eq!(params_at(&base, &keyframes, 7), only);
    assert_eq!(params_at(&base, &keyframes, 70), only);
}

#[test]
fn arrays_and_curves() {
    let mut a = GradeParams::default();
    a.hsl_h = [0.0; 8];
    a.mono_h = [0.0; 8];
    a.curve_rgb = vec![[0.0, 0.0], [255.0, 200.0]];
    a.curve_r = vec![[0.0, 0.0], [128.0, 100.0], [255.0, 255.0]];
    let mut b = GradeParams::default();
    b.hsl_h = [10.0; 8];
    b.mono_h = [20.0; 8];
    b.curve_rgb = vec![[0.0, 40.0], [255.0, 240.0]];
    b.curve_r = vec![[0.0, 20.0], [255.0, 220.0]];

    let keyframes = kf(&[(0, a.clone()), (10, b.clone())]);
    let mid = params_at(&a, &keyframes, 5);

    assert!(mid.hsl_h.iter().all(|v| (*v - 5.0).abs() < 1e-12));
    assert!(mid.mono_h.iter().all(|v| (*v - 10.0).abs() < 1e-12));
    assert_eq!(mid.curve_rgb, vec![[0.0, 20.0], [255.0, 220.0]]);

    // 控制点 x 不一致时取较近的一侧
    let near_a = params_at(&a, &keyframes, 2);
    assert_eq!(near_a.curve_r, a.curve_r);
    let near_b = params_at(&a, &keyframes, 8);
    assert_eq!(near_b.curve_r, b.curve_r);
}

#[test]
fn masks_interpolate_by_id() {
    let mut a = GradeParams::default();
    a.masks = vec![serde_json::json!({
        "id": "m1", "x1": 0.2, "amount": 100.0, "params": {"exposure": -1.0},
    })];
    let mut b = GradeParams::default();
    b.masks = vec![serde_json::json!({
        "id": "m1", "x1": 0.6, "amount": 50.0, "params": {"exposure": 1.0},
    })];
    let keyframes = kf(&[(0, a.clone()), (10, b.clone())]);

    let early = params_at(&a, &keyframes, 2).masks;
    assert_eq!(early.len(), 1);
    assert!((early[0]["x1"].as_f64().unwrap() - 0.28).abs() < 1e-9);
    assert!((early[0]["amount"].as_f64().unwrap() - 90.0).abs() < 1e-9);
    assert!((early[0]["params"]["exposure"].as_f64().unwrap() + 0.6).abs() < 1e-9);

    let mid = params_at(&a, &keyframes, 5).masks;
    assert!((mid[0]["x1"].as_f64().unwrap() - 0.4).abs() < 1e-9);
    assert!((mid[0]["amount"].as_f64().unwrap() - 75.0).abs() < 1e-9);
}

#[test]
fn unmatched_masks_fade_out_and_in() {
    let mut a = GradeParams::default();
    a.masks = vec![serde_json::json!({"id": "gone", "amount": 80.0})];
    let mut b = GradeParams::default();
    b.masks = vec![serde_json::json!({"id": "new", "amount": 60.0})];
    let keyframes = kf(&[(0, a.clone()), (10, b.clone())]);

    // 不在两端共享的蒙版按 t 淡出/淡入，而不是突然切换
    let early = params_at(&a, &keyframes, 2).masks;
    assert_eq!(early.len(), 2);
    assert!((early[0]["amount"].as_f64().unwrap() - 64.0).abs() < 1e-9);
    assert!((early[1]["amount"].as_f64().unwrap() - 12.0).abs() < 1e-9);
}

/// 把 a 的所有数值叶子改成 b，逐项验证中点等于两端平均，
/// 这样新增字段漏插值会立刻暴露，不必手抄 59 个字段名。
#[test]
fn every_numeric_field_interpolates() {
    let a = GradeParams::default();
    let mut json = serde_json::to_value(&a).unwrap();
    bump_numbers(&mut json);
    let b: GradeParams = serde_json::from_value(json).unwrap();

    let mid = interpolate(&a, &b, 0.5);
    let actual = serde_json::to_value(&mid).unwrap();
    let left = serde_json::to_value(&a).unwrap();
    let right = serde_json::to_value(&b).unwrap();
    let count = assert_midpoint(&actual, &left, &right, "");
    assert!(count >= 100, "数值叶子只覆盖了 {count} 个");
}

fn bump_numbers(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for value in map.values_mut() {
                bump_numbers(value);
            }
        }
        Value::Array(items) => {
            // 曲线是 [[x, y], ...]：只动 y，保持 x 序列一致
            if items.iter().all(Value::is_array) {
                for point in items {
                    if let Value::Array(pair) = point
                        && let Some(y) = pair.get_mut(1)
                    {
                        bump_numbers(y);
                    }
                }
            } else {
                for item in items {
                    if item.is_number() {
                        bump_numbers(item);
                    }
                }
            }
        }
        Value::Number(n) => {
            let x = n.as_f64().unwrap();
            *n = serde_json::Number::from_f64(x + 10.0).unwrap();
        }
        _ => {}
    }
}

fn assert_midpoint(actual: &Value, a: &Value, b: &Value, path: &str) -> usize {
    match (actual, a, b) {
        (Value::Number(m), Value::Number(x), Value::Number(y)) => {
            let expect = (x.as_f64().unwrap() + y.as_f64().unwrap()) * 0.5;
            let got = m.as_f64().unwrap();
            assert!(
                (got - expect).abs() < 1e-9,
                "{path}: 中点期望 {expect}，实得 {got}"
            );
            1
        }
        (Value::Array(m), Value::Array(x), Value::Array(y)) => {
            assert_eq!(m.len(), x.len(), "{path} 长度不同");
            assert_eq!(m.len(), y.len(), "{path} 长度不同");
            m.iter()
                .zip(x)
                .zip(y)
                .enumerate()
                .map(|(i, ((mv, xv), yv))| assert_midpoint(mv, xv, yv, &format!("{path}[{i}]")))
                .sum()
        }
        (Value::Object(m), Value::Object(x), Value::Object(y)) => x
            .keys()
            .map(|key| {
                assert_midpoint(
                    m.get(key).unwrap_or(&Value::Null),
                    x.get(key).unwrap(),
                    y.get(key).unwrap(),
                    &format!("{path}.{key}"),
                )
            })
            .sum(),
        _ => 0,
    }
}
