use super::*;
use aevum_core::mask::{self, ShapeKind};

#[test]
fn reset_session_clears_all_caches() {
    let mut st = AppState::new();
    st.params.masks.push(mask::new_mask(ShapeKind::Linear));
    let _ = st.mask_geos_cached(0);
    st.curve_lut_cache = Some(crate::widgets::CurveLutCache {
        fp: 7,
        lut: vec![0.0; 129],
    });
    st.hist_mesh_cache = Some(crate::shell::HistMeshCache {
        version: 1,
        rect: egui::Rect::ZERO,
        shapes: Vec::new(),
    });
    st.defl_curve_cache = Some(crate::shell::DeflCurveCache {
        version: 1,
        n: 1,
        rect: egui::Rect::ZERO,
        pts_raw: [].into(),
        pts_fix: [].into(),
        lo: 0.0,
        hi: 1.0,
    });
    st.reset_session();
    assert_eq!(st.mask_geo_cache.fp, 0, "会话清理必须清空蒙版缓存");
    assert!(st.curve_lut_cache.is_none());
    assert!(st.hist_mesh_cache.is_none());
    assert!(st.defl_curve_cache.is_none());
    // 会话清单本体逐字段核（E7 收编后的回归防线）。
    assert!(st.plugin_result.is_none());
    assert_eq!(st.plugin_stamp, 0);
    assert_eq!(st.plugin_progress, (0, 0));
    assert_eq!(st.preview_rgb, None);
    assert!(st.clip_mask.is_none());
    assert_eq!(st.clip_mask_stamp, u64::MAX);
    assert_eq!(st.mask_paint, None);
    assert_eq!(st.preview_size, [0, 0]);
    assert!(!st.preview_dirty);
    assert!(!st.preview_inflight);
    assert!(st.thumbs.is_empty());
    assert!(st.thumb_pending.is_empty());
}

#[test]
fn reset_mask_state_clears_five_fields() {
    let mut st = AppState::new();
    st.mask_sel = Some(0);
    st.mask_comp = 1;
    st.mask_drag = Some((0, 1));
    st.mask_pick = true;
    st.mask_pick_img = Some(ColorImage::new([2, 2], vec![egui::Color32::BLACK; 4]));
    st.reset_mask_state();
    assert_eq!(st.mask_sel, None);
    assert_eq!(st.mask_comp, 0);
    assert_eq!(st.mask_drag, None);
    assert!(!st.mask_pick);
    assert!(st.mask_pick_img.is_none());
}
