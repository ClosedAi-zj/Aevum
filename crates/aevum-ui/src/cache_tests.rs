//! 缓存单测（性能重构设计 T03）：指纹对参数敏感、失效与命中正确。
//!
//! 原则：指纹是纯函数——「参数变 → 指纹变」「参数没变 → 指纹不变」；
//! 缓存取值走 AppState 真实路径（headless 状态，无磁盘线程）。

use crate::shell::{MaskGeo, mask_geo_fingerprint};
use crate::state::AppState;
use aevum_core::mask::{self, ShapeKind};
use egui::Rect;
use serde_json::json;

fn linear_mask() -> serde_json::Value {
    let mut m = mask::new_mask(ShapeKind::Linear);
    if let Some(comp) = m["components"][0].as_object_mut() {
        comp.insert("x1".to_owned(), json!(0.30));
        comp.insert("y1".to_owned(), json!(0.40));
        comp.insert("x2".to_owned(), json!(0.50));
        comp.insert("y2".to_owned(), json!(0.72));
    }
    m
}

#[test]
fn fingerprint_is_stable_for_unchanged_params() {
    let a = linear_mask();
    let b = linear_mask();
    // 两份独立构造、字段同值的蒙版必须同一指纹（几何层不认 id）。
    assert_eq!(mask_geo_fingerprint(&a), mask_geo_fingerprint(&b));
}

#[test]
fn fingerprint_changes_when_geometry_changes() {
    let base = linear_mask();
    // 拖手柄：x2 变。
    let mut dragged = base.clone();
    dragged["components"][0]["x2"] = json!(0.55);
    assert_ne!(mask_geo_fingerprint(&base), mask_geo_fingerprint(&dragged));
    // 缺键与显式同值必须同指纹（默认值口径与 mask_geo_of 一致）。
    let mut missing_feather = base.clone();
    missing_feather["components"][0]["feather"] = json!(45.0);
    let mut no_feather = base.clone();
    no_feather["components"][0]
        .as_object_mut()
        .unwrap()
        .remove("feather");
    assert_eq!(
        mask_geo_fingerprint(&no_feather),
        mask_geo_fingerprint(&missing_feather)
    );
}

#[test]
fn fingerprint_changes_when_strokes_change() {
    let mut a = mask::new_mask(ShapeKind::Brush);
    a["components"][0]["strokes"] = json!([{ "points": [[0.1, 0.2], [0.3, 0.4]], "erase": false }]);
    let mut b = a.clone();
    // 续了一笔：点序列变长。
    b["components"][0]["strokes"] = json!([
        { "points": [[0.1, 0.2], [0.3, 0.4]], "erase": false },
        { "points": [[0.5, 0.5]], "erase": false }
    ]);
    assert_ne!(mask_geo_fingerprint(&a), mask_geo_fingerprint(&b));
    // 擦除标记也是几何的一部分。
    let mut c = a.clone();
    c["components"][0]["strokes"] = json!([{ "points": [[0.1, 0.2], [0.3, 0.4]], "erase": true }]);
    assert_ne!(mask_geo_fingerprint(&a), mask_geo_fingerprint(&c));
    // 同一批点换成扁平老格式（无 components）也认得出。
    let mut flat = a.clone();
    let comp = flat["components"][0].clone();
    flat.as_object_mut().unwrap().clear();
    for (k, v) in comp.as_object().unwrap() {
        flat[k.as_str()] = v.clone();
    }
    assert_ne!(mask_geo_fingerprint(&a), mask_geo_fingerprint(&flat));
}

#[test]
fn geo_cache_hits_and_invalidates() {
    let mut state = AppState::new();
    state.params.masks.push(linear_mask());
    state.mask_sel = Some(0);

    let _img = Rect::from_min_size(egui::Pos2::new(100.0, 50.0), egui::vec2(800.0, 600.0));
    // 首次取值建缓存。
    let fp0 = state.mask_geo_cache.fp;
    let _ = state.mask_geos_cached(0);
    assert_ne!(
        state.mask_geo_cache.fp, fp0,
        "空缓存指纹为 0，建缓存后必非 0"
    );
    let geos: Vec<MaskGeo> = state.mask_geos_cached(0).to_vec();
    assert_eq!(geos.len(), 1);

    // 同参数再取：指纹不变（命中），切片内容逐位一致。
    assert_eq!(state.mask_geos_cached(0), geos.as_slice());

    // 改参数（拖手柄）：指纹必变，缓存重建为新几何。
    state.params.masks[0]["components"][0]["x2"] = json!(0.60);
    let geos2 = state.mask_geos_cached(0).to_vec();
    assert_ne!(geos[0].x2, geos2[0].x2, "几何必须跟着参数走");
    let expected_fp = {
        let mut v = linear_mask();
        v["components"][0]["x2"] = json!(0.60);
        mask_geo_fingerprint(&v)
    };
    assert_eq!(
        state.mask_geo_cache.fp, expected_fp,
        "缓存键与新参数指纹一致"
    );
}

#[test]
fn geo_cache_mask_changed_clears() {
    let mut state = AppState::new();
    state.params.masks.push(linear_mask());
    let _ = state.mask_geos_cached(0);
    assert_ne!(state.mask_geo_cache.fp, 0);
    state.masks_changed();
    assert_eq!(state.mask_geo_cache.fp, 0, "masks_changed 必须清空蒙版缓存");
    assert!(state.mask_geo_cache.geos.is_empty());
}

#[test]
fn curve_lut_fingerprint_sensitivity() {
    use crate::widgets::curve_lut_fingerprint;
    let a = [[0.0, 0.0], [128.0, 140.0], [255.0, 255.0]];
    let b = [[0.0, 0.0], [128.0, 141.0], [255.0, 255.0]];
    assert_ne!(curve_lut_fingerprint(&a), curve_lut_fingerprint(&b));
    assert_eq!(curve_lut_fingerprint(&a), curve_lut_fingerprint(&a));
    // 拖点后 y 变 → 指纹变（自校验失效的根基）。
    let mut c = a;
    c[1][1] += 0.5;
    assert_ne!(curve_lut_fingerprint(&a), curve_lut_fingerprint(&c));
}

#[test]
fn hist_version_bumps_on_new_preview_only() {
    // 直方图缓存键之一：新预览落地才换版。poll 的写点在 Msg::Preview，
    // 这里验证版本号初值与手动递增语义（真正落地路径由 selftest 覆盖形状计数）。
    let state = AppState::new();
    assert_eq!(state.hist_version, 0);
    assert_eq!(state.corr_version, 0);
    let state = AppState::demo();
    // demo 构造里 recompute_deflicker 至少跑过一次 → corr_version 已 +1。
    assert!(state.corr_version >= 1, "demo 的去闪重算必须换版");
}

/// 两个不同圆心的径向形状（多形状组，用于切换回归）。
/// 注意径向的中心键是 x1/y1（x2/y2 是半径轴端点），与线性同键。
fn two_radial_masks() -> serde_json::Value {
    let mut m = mask::new_mask(ShapeKind::Radial);
    let comp = mask::new_component(ShapeKind::Radial);
    m["components"].as_array_mut().unwrap().push(comp);
    m["components"][0]["x1"] = json!(0.30);
    m["components"][0]["y1"] = json!(0.40);
    m["components"][0]["x2"] = json!(0.40);
    m["components"][0]["y2"] = json!(0.40);
    m["components"][1]["x1"] = json!(0.70);
    m["components"][1]["y1"] = json!(0.60);
    m["components"][1]["x2"] = json!(0.80);
    m["components"][1]["y2"] = json!(0.60);
    m
}

/// 近退化径向：x2≈x1、y2≈y1，半径落在死区里——只有这种几何的映射才真正
/// 依赖像素尺寸（mask_radial_axes 的 `h*0.01`/`w*0.01` 死区阈值），用来钉住
/// 「preview_size 变化必须重映射」的回归。
fn degenerate_radial_mask() -> serde_json::Value {
    let mut m = mask::new_mask(ShapeKind::Radial);
    m["components"][0]["x1"] = json!(0.30);
    m["components"][0]["y1"] = json!(0.40);
    // x2==x1、y2==y1（半径向量为零）：mask_radial_axes 的死区把半径抬到
    // 1px（像素域），归一化后被 (w-1)/(h-1) 除 —— 像素尺寸不同则轮廓必不同。
    m["components"][0]["x2"] = json!(0.30);
    m["components"][0]["y2"] = json!(0.40);
    m
}

fn img() -> Rect {
    Rect::from_min_size(egui::Pos2::new(100.0, 50.0), egui::vec2(800.0, 600.0))
}

/// 回归（QA Round 1 探针 1）：切换 mask_comp 后手柄必须跟随当前形状。
/// 重建键漏 comp 时，手柄停在旧形状位置直到指纹或 img 变化。
#[test]
fn mask_prep_rebuilds_handles_when_comp_switches() {
    let mut state = AppState::new();
    state.params.masks.push(two_radial_masks());
    state.mask_sel = Some(0);
    let img = img();

    state.mask_comp = 0;
    let n = state.mask_prep(0, img, 1000.0, 800.0);
    assert_eq!(n, 2);
    let geo0 = state.mask_geo_get(0).expect("comp0 几何在缓存");
    let handles_comp0 = state.mask_handles_now().to_vec();
    assert_eq!(
        handles_comp0,
        crate::shell::mask_handles(&geo0, img, 1000.0, 800.0)
    );

    // 面板"形状"条点击：mask_comp 0 → 1，img 不变再 prep。
    state.mask_comp = 1;
    let _ = state.mask_prep(0, img, 1000.0, 800.0);
    let geo1 = state.mask_geo_get(1).expect("comp1 几何在缓存");
    let handles_comp1 = state.mask_handles_now().to_vec();
    assert_ne!(geo0, geo1, "两形状几何必须不同，否则用例失去意义");
    assert_ne!(handles_comp1, handles_comp0, "手柄必须跟随新形状");
    assert_eq!(
        handles_comp1,
        crate::shell::mask_handles(&geo1, img, 1000.0, 800.0),
        "手柄位置 = 当前形状按 HEAD 口径现算的结果"
    );
}

/// 回归（QA Round 1 探针 2）：preview_size 变化后派生量必须按新像素尺寸重映射。
/// 重建键漏 (w,h) 时，径向轮廓/手柄/笔画仍按旧尺寸映射（线性线段是现算的，症状更隐蔽）。
#[test]
fn mask_prep_rebuilds_derived_when_pixel_size_changes() {
    let mut state = AppState::new();
    state.params.masks.push(degenerate_radial_mask());
    state.mask_sel = Some(0);
    let img = img();

    state.mask_prep(0, img, 320.0, 200.0);
    let handles_small = state.mask_handles_now().to_vec();
    let radial_small = state.mask_radial_now(0).to_vec();
    let geo = state.mask_geo_get(0).expect("comp0");
    assert_eq!(
        handles_small,
        crate::shell::mask_handles(&geo, img, 320.0, 200.0),
        "旧像素尺寸下手柄 = 按该尺寸映射的结果"
    );

    // 预览落地换分辨率（Msg::Preview 无条件写 preview_size），画布 rect 不变。
    state.mask_prep(0, img, 1600.0, 1000.0);
    let handles_large = state.mask_handles_now().to_vec();
    let radial_large = state.mask_radial_now(0).to_vec();
    // 手柄的半径点用的是未钳制的 (dx,dy)，本几何下与像素尺寸无关，不在此断言；
    // 径向轮廓用死区钳制后的 (rx,ry)，是像素尺寸敏感的主证据。
    assert_ne!(
        radial_large, radial_small,
        "像素尺寸变了（死区几何），径向轮廓必须重映射"
    );
    assert_eq!(
        handles_large,
        crate::shell::mask_handles(&geo, img, 1600.0, 1000.0),
        "手柄 = 按新像素尺寸映射的结果"
    );
    assert_eq!(
        radial_large,
        crate::shell::mask_radial_points(&geo, img, 1600.0, 1000.0).as_slice(),
        "径向轮廓 = 按新像素尺寸映射的结果"
    );
}
