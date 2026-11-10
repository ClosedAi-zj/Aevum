//! 蒙版域：几何解析、画布 overlay 与手柄、画笔、面板与缓存（性能重构设计 §7）。
//!
//! Mask domain: geometry resolution, canvas overlay and handles, brush, panel and
//! caches (performance refactor design section 7).

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use super::common::group_header;
use super::common::note;
use super::common::slider;
use super::toolbar::bar_button;
use super::toolbar::bar_chip;
use crate::history;
use crate::icons::{self, Glyph};
use crate::state::{AppState, Module, Tool};
use crate::theme::*;
use egui::{Align, Align2, Color32, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, Vec2};
use std::sync::Arc;
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

const MASK_SLIDERS: &[(&str, &str, f64, f64, usize)] = &[
    ("exposure", "曝光", -5.0, 5.0, 2),
    ("contrast", "对比度", -100.0, 100.0, 0),
    ("highlights", "高光", -100.0, 100.0, 0),
    ("shadows", "阴影", -100.0, 100.0, 0),
    ("whites", "白色", -100.0, 100.0, 0),
    ("blacks", "黑色", -100.0, 100.0, 0),
    ("temperature", "色温", -100.0, 100.0, 0),
    ("tint", "色调", -100.0, 100.0, 0),
    ("dehaze", "去朦胧", -100.0, 100.0, 0),
    ("vibrance", "自然饱和度", -100.0, 100.0, 0),
    ("saturation", "饱和度", -100.0, 100.0, 0),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct MaskGeo {
    pub(crate) radial: bool,
    pub(crate) brush: bool,
    pub(crate) x1: f64,
    pub(crate) y1: f64,
    pub(crate) x2: f64,
    pub(crate) y2: f64,
    pub(crate) feather: f64,
    pub(crate) midpoint: f64,
    pub(crate) angle: f64,
}

/// 单张蒙版几何 + 派生屏幕折线的三层缓存（性能重构设计 §3 第 1 层，E1/E2/E3）。
///
/// 第 1 层按参数指纹自校验：`params.masks[sel]` 变了指纹必变，重解析几何；
/// 第 2 层派生量（手柄 / 径向轮廓 / 笔画折线）按「指纹命中 + 画布 img 未变」
/// 复用：img 变了（缩放/平移/布局）只重算派生，几何解析不重算。
/// `masks_changed()` 与 `reset_session` 另行整体清空作保守兜底。
#[derive(Default)]
pub struct MaskGeoCache {
    /// 参数指纹；0 表示空缓存。
    pub(crate) fp: u64,
    /// 与指纹对应的形状几何。
    pub(crate) geos: Vec<MaskGeo>,
    /// 派生量生成时的画布矩形（None 表示派生量未建）。
    pub(crate) img: Option<Rect>,
    /// 派生量生成时的像素尺寸（resolution/preview_size 映射基数）。
    pub(crate) px: (f64, f64),
    /// 手柄所属的当前形状序号（切换形状必须重建手柄）。
    pub(crate) comp: usize,
    /// 当前形状的手柄（overlay 每帧 ：7378 用）。
    pub(crate) handles: Vec<(Pos2, bool)>,
    /// 每形状的径向轮廓；非径向形状对应空 Vec。
    pub(crate) radial: Vec<Vec<Pos2>>,
    /// 每形状的笔画折线（画笔蒙版）；非画笔形状对应空 Vec。
    pub(crate) strokes: Vec<Vec<(Vec<Pos2>, bool)>>,
}

/// 直方图 4 个 Mesh 形状的缓存（E6）：键 = (hist_version, rect)。
/// `Shape::Mesh` 持 `Arc<Mesh>`（epaint 0.37 已确认），命中时 clone 零拷贝。
///
/// Cache for the four histogram `Mesh` shapes (E6): key = (hist_version, rect).
/// `Shape::Mesh` holds `Arc<Mesh>` (confirmed in epaint 0.37), so a hit clones with
/// zero copy.
pub struct HistMeshCache {
    pub(crate) version: u64,
    pub(crate) rect: Rect,
    pub(crate) shapes: Vec<Shape>,
}

/// 去闪亮度曲线两条折线点集的缓存（E5）：键 = (corr_version, 帧数, inner rect)。
///
/// Cache for the two polyline point sets of the deflicker luminance curve (E5):
/// key = (corr_version, frame count, inner rect).
pub struct DeflCurveCache {
    pub(crate) version: u64,
    pub(crate) n: usize,
    pub(crate) rect: Rect,
    pub(crate) pts_raw: Arc<[Pos2]>,
    pub(crate) pts_fix: Arc<[Pos2]>,
    /// 折线值域（坐标轴标签用），与点集同批生成。
    pub(crate) lo: f64,
    pub(crate) hi: f64,
}

/// 指纹专用 hasher：fx 风格的乘旋混合（std 自带，零依赖）。
/// SipHash（DefaultHasher）对 200 点笔画一次指纹要 ~2.5µs，比 JSON 解析还贵；
/// 这个 hasher 每写入一个 u64 只有两次整数运算，微基准见 docs/基准-对比。
struct FpHasher(u64);

impl FpHasher {
    const K: u64 = 0x517c_c1b7_2722_0a95;

    fn mix(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(Self::K);
    }
}

impl std::hash::Hasher for FpHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            self.mix(u64::from_le_bytes(buf));
        }
    }
    fn write_u8(&mut self, v: u8) {
        self.mix(v as u64);
    }
    fn write_u64(&mut self, v: u64) {
        self.mix(v);
    }
    fn write_usize(&mut self, v: usize) {
        self.mix(v as u64);
    }
}

/// 单张蒙版几何指纹：只纳入 `mask_geo_of` 实际读取的字段。
/// f64 一律 `to_bits()` 进 hasher（禁 `to_string()`——每帧分配，得不偿失）。
pub(crate) fn mask_geo_fingerprint(raw: &serde_json::Value) -> u64 {
    use std::hash::Hasher;
    let mut h = FpHasher(0xcbf2_9ce4_8422_2325); // FNV 偏移基做起点
    match raw.get("components").and_then(serde_json::Value::as_array) {
        Some(list) => {
            h.write_u8(1);
            h.write_usize(list.len());
            for comp in list.iter().filter_map(|c| c.as_object()) {
                mask_geo_hash_comp(&mut h, comp);
            }
        }
        None => {
            // 扁平老格式：整个对象就是那一个形状。
            h.write_u8(0);
            if let Some(obj) = raw.as_object() {
                mask_geo_hash_comp(&mut h, obj);
            }
        }
    }
    h.finish()
}

/// 形状对象的指纹分量：9 个几何字段 + strokes 点序列。
fn mask_geo_hash_comp(h: &mut FpHasher, comp: &serde_json::Map<String, serde_json::Value>) {
    use std::hash::Hasher;
    h.write_u8((is_radial(comp) as u8) | ((is_brush(comp) as u8) << 1));
    // 默认值与 `mask_geo_of` 一一对应：缺键与显式同值必须产出同一指纹，
    // 否则会出现「指纹没变、几何变了」的假命中。
    let radial = is_radial(comp);
    for (key, default) in [
        ("x1", 0.5),
        ("y1", 0.5),
        ("x2", 0.5),
        ("y2", 0.5),
        ("feather", if radial { 35.0 } else { 45.0 }),
        ("midpoint", 50.0),
        ("angle", 0.0),
    ] {
        h.write_u64(mask_number(comp, key, default).to_bits());
    }
    for stroke in comp
        .get("strokes")
        .and_then(serde_json::Value::as_array)
        .map(|list| list.as_slice())
        .unwrap_or(&[])
    {
        let Some(obj) = stroke.as_object() else {
            continue;
        };
        h.write_u8(mask_flag(obj.get("erase")) as u8);
        for pt in obj
            .get("points")
            .and_then(serde_json::Value::as_array)
            .map(|list| list.as_slice())
            .unwrap_or(&[])
        {
            let Some(a) = pt.as_array() else {
                continue;
            };
            for v in a.iter().take(2) {
                h.write_u64(v.as_f64().unwrap_or(0.0).to_bits());
            }
        }
    }
}

/// 组内形状的布尔方式，与 `MASK_BLEND_LABELS` 同序（存盘用左边那串）。
const MASK_BLENDS: [&str; 3] = ["add", "subtract", "intersect"];

const MASK_BLEND_LABELS: [&str; 3] = ["叠加", "减去", "交集"];

/// 亮度范围的界面默认值：存的是 0~1，滑杆按 0~100 显示（同 Python `RANGE_NEW`）。
const LUM_DEFAULT: [f64; 3] = [0.0, 50.0, 10.0];

/// 颜色范围的界面默认值：色相 0~360，宽度 0~180，羽化 0~90（角度）。
const HUE_DEFAULT: [f64; 3] = [0.0, 40.0, 20.0];

#[derive(Clone)]
struct MaskView {
    geo: MaskGeo,
    blend: usize,
    comp_invert: bool,
    amount: f64,
    invert: bool,
    range_on: bool,
    /// 下限 / 上限 / 范围羽化，0~100。
    lum: [f64; 3],
    hue_on: bool,
    /// 目标色相 / 带宽 / 羽化，角度。
    hue: [f64; 3],
    params: std::collections::BTreeMap<String, f64>,
}

/// 组内第 `at` 个形状的存储对象；扁平老格式整个对象就是那一个形状。
pub(crate) fn mask_comp_object(
    raw: &serde_json::Value,
    at: usize,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    let object = raw.as_object()?;
    if let Some(list) = object
        .get("components")
        .and_then(serde_json::Value::as_array)
    {
        let slot = at.min(list.len().saturating_sub(1));
        list.get(slot)?.as_object()
    } else {
        Some(object)
    }
}

fn mask_comp_object_mut(
    raw: &mut serde_json::Value,
    at: usize,
) -> Option<&mut serde_json::Map<String, serde_json::Value>> {
    let object = raw.as_object_mut()?;
    let grouped = object
        .get("components")
        .is_some_and(serde_json::Value::is_array);
    if grouped {
        let list = object.get_mut("components")?.as_array_mut()?;
        let slot = at.min(list.len().saturating_sub(1));
        return list.get_mut(slot)?.as_object_mut();
    }
    Some(object)
}

/// 组内形状数；空组按一个算，免得面板拿到 0 去减一。
fn mask_comp_count(raw: &serde_json::Value) -> usize {
    raw.get("components")
        .and_then(serde_json::Value::as_array)
        .map(|list| list.len().max(1))
        .unwrap_or(1)
}

fn mask_geo_of(comp: &serde_json::Map<String, serde_json::Value>) -> MaskGeo {
    let radial = is_radial(comp);
    MaskGeo {
        radial,
        brush: is_brush(comp),
        x1: mask_number(comp, "x1", 0.5),
        y1: mask_number(comp, "y1", 0.5),
        x2: mask_number(comp, "x2", 0.5),
        y2: mask_number(comp, "y2", 0.5),
        feather: mask_number(comp, "feather", if radial { 35.0 } else { 45.0 }),
        midpoint: mask_number(comp, "midpoint", 50.0),
        angle: mask_number(comp, "angle", 0.0),
    }
}

/// 画布要画的形状轮廓：一组里每个形状都要看得见，布尔选区才知道减掉了什么。
pub(crate) fn mask_geos(raw: &serde_json::Value) -> Vec<MaskGeo> {
    let Some(object) = raw.as_object() else {
        return Vec::new();
    };
    match object
        .get("components")
        .and_then(serde_json::Value::as_array)
    {
        Some(list) => list
            .iter()
            .filter_map(|c| c.as_object())
            .map(mask_geo_of)
            .collect(),
        None => vec![mask_geo_of(object)],
    }
}

/// 组级亮度范围，返回（是否启用，下限，上限，羽化），后三个已换成 0~100。
fn mask_range_of(object: &serde_json::Map<String, serde_json::Value>) -> (bool, [f64; 3]) {
    let range = object.get("range").and_then(serde_json::Value::as_object);
    let on = range
        .and_then(|r| r.get("lum_on"))
        .is_some_and(|v| v.as_bool().unwrap_or(false) || v.as_f64().is_some_and(|n| n != 0.0));
    let mut values = LUM_DEFAULT;
    for (k, key) in ["lum_min", "lum_max", "lum_feather"].iter().enumerate() {
        if let Some(v) = range
            .and_then(|r| r.get(*key))
            .and_then(serde_json::Value::as_f64)
            .filter(|v| v.is_finite())
        {
            values[k] = (v * 100.0).clamp(0.0, 100.0);
        }
    }
    (on, values)
}

/// 组级颜色范围：目标色相 / 带宽 / 羽化，全是角度。
fn mask_hue_of(object: &serde_json::Map<String, serde_json::Value>) -> (bool, [f64; 3]) {
    let range = object.get("range").and_then(serde_json::Value::as_object);
    let on = range
        .and_then(|r| r.get("hue_on"))
        .is_some_and(|v| v.as_bool().unwrap_or(false) || v.as_f64().is_some_and(|n| n != 0.0));
    let mut values = HUE_DEFAULT;
    for (k, key) in ["hue", "hue_width", "hue_feather"].iter().enumerate() {
        if let Some(v) = range
            .and_then(|r| r.get(*key))
            .and_then(serde_json::Value::as_f64)
            .filter(|v| v.is_finite())
        {
            values[k] = v.clamp(
                0.0,
                if k == 0 {
                    360.0
                } else if k == 1 {
                    180.0
                } else {
                    90.0
                },
            );
        }
    }
    (on, values)
}

fn blend_index(value: Option<&serde_json::Value>) -> usize {
    match value.and_then(serde_json::Value::as_str) {
        Some("subtract") => 1,
        Some("intersect") => 2,
        _ => 0,
    }
}

pub(crate) fn mask_number(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: f64,
) -> f64 {
    object
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .filter(|v| v.is_finite())
        .unwrap_or(default)
}

/// 存盘里布尔值可能是 true 也可能是 1（Python 老工程这么写）。
pub(crate) fn mask_flag(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| v.as_bool().unwrap_or(false) || v.as_f64().is_some_and(|n| n != 0.0))
}

fn mask_view(raw: &serde_json::Value, at: usize) -> Option<MaskView> {
    let object = raw.as_object()?;
    let comp = mask_comp_object(raw, at)?;
    let mut params = std::collections::BTreeMap::new();
    for key in aevum_core::mask::MASK_KEYS {
        let value = object
            .get("params")
            .and_then(serde_json::Value::as_object)
            .and_then(|list| list.get(*key))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        params.insert((*key).to_owned(), value);
    }
    let (range_on, lum) = mask_range_of(object);
    let (hue_on, hue) = mask_hue_of(object);
    Some(MaskView {
        geo: mask_geo_of(comp),
        blend: blend_index(comp.get("blend")),
        comp_invert: mask_flag(comp.get("invert")),
        amount: mask_number(object, "amount", 100.0),
        invert: mask_flag(object.get("invert")),
        range_on,
        lum,
        hue_on,
        hue,
        params,
    })
}

pub(crate) fn is_radial(comp: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["kind", "type"].iter().any(|key| {
        comp.get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("radial"))
    })
}

pub(crate) fn is_brush(comp: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["kind", "type"].iter().any(|key| {
        comp.get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("brush"))
    })
}

fn mask_write_geo(comp: &mut serde_json::Map<String, serde_json::Value>, geo: &MaskGeo) {
    comp.insert(
        "kind".to_owned(),
        serde_json::json!(if geo.brush {
            "brush"
        } else if geo.radial {
            "radial"
        } else {
            "linear"
        }),
    );
    // 画笔没有渐变几何；这些字段对它是摆设，别覆盖也不要紧，但不写少一份噪音。
    if geo.brush {
        return;
    }
    comp.insert("x1".to_owned(), serde_json::json!(geo.x1));
    comp.insert("y1".to_owned(), serde_json::json!(geo.y1));
    comp.insert("x2".to_owned(), serde_json::json!(geo.x2));
    comp.insert("y2".to_owned(), serde_json::json!(geo.y2));
    comp.insert("feather".to_owned(), serde_json::json!(geo.feather));
    comp.insert("midpoint".to_owned(), serde_json::json!(geo.midpoint));
    comp.insert("angle".to_owned(), serde_json::json!(geo.angle));
}

fn mask_write(raw: &mut serde_json::Value, at: usize, view: &MaskView) {
    let grouped = raw
        .as_object()
        .is_some_and(|o| o.contains_key("components"));
    if !grouped {
        *raw = aevum_core::mask::as_group(raw);
    }
    {
        let Some(object) = raw.as_object_mut() else {
            return;
        };
        object.insert("amount".to_owned(), serde_json::json!(view.amount));
        object.insert("invert".to_owned(), serde_json::json!(view.invert));
        // 没勾上且老数据里本来就没有，就别凭空长出一个 range 字段。
        // 已有的 range 要合并着写：整块替换会把另一种范围（亮度/颜色）的键抹掉。
        if view.range_on || view.hue_on || object.contains_key("range") {
            let mut r = object
                .get("range")
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let (mut lo, mut hi) = (view.lum[0], view.lum[1]);
            if hi < lo {
                std::mem::swap(&mut lo, &mut hi);
            }
            r.insert("lum_on".to_owned(), serde_json::json!(view.range_on));
            r.insert("lum_min".to_owned(), serde_json::json!(lo / 100.0));
            r.insert("lum_max".to_owned(), serde_json::json!(hi / 100.0));
            r.insert(
                "lum_feather".to_owned(),
                serde_json::json!((view.lum[2] / 100.0).max(0.001)),
            );
            if view.hue_on || r.contains_key("hue_on") {
                r.insert("hue_on".to_owned(), serde_json::json!(view.hue_on));
                r.insert("hue".to_owned(), serde_json::json!(view.hue[0]));
                r.insert("hue_width".to_owned(), serde_json::json!(view.hue[1]));
                r.insert(
                    "hue_feather".to_owned(),
                    serde_json::json!(view.hue[2].max(1.0)),
                );
            }
            object.insert("range".to_owned(), serde_json::Value::Object(r));
        }
        let params = object
            .entry("params".to_owned())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(params) = params.as_object_mut() {
            for (key, value) in &view.params {
                if value.abs() <= 1e-9 {
                    params.remove(key);
                } else {
                    params.insert(key.clone(), serde_json::json!(value));
                }
            }
        }
    }
    if let Some(comp) = mask_comp_object_mut(raw, at) {
        mask_write_geo(comp, &view.geo);
        comp.insert(
            "blend".to_owned(),
            serde_json::json!(MASK_BLENDS[view.blend.min(MASK_BLENDS.len() - 1)]),
        );
        comp.insert("invert".to_owned(), serde_json::json!(view.comp_invert));
    }
}

pub(super) fn adjust_masks(state: &mut AppState, ui: &mut Ui) {
    if !group_header(ui, state, "masks", "局部蒙版", false) {
        return;
    }
    let count = state.params.masks.len();
    let mut add: Option<&'static str> = None;
    let mut remove: Option<usize> = None;
    let mut select: Option<usize> = None;
    ui.horizontal(|ui| {
        if bar_button(ui, "+ 线性").clicked() {
            add = Some("linear");
        }
        if bar_button(ui, "+ 径向").clicked() {
            add = Some("radial");
        }
        if bar_button(ui, "+ 画笔").clicked() {
            add = Some("brush");
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(crate::i18n::tr_fmt("{} 张", &[&count]))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        });
    });
    ui.add_space(3.0);

    for i in 0..count {
        let selected = state.mask_sel == Some(i);
        // 缓存取值：面板行只需要形状个数与第一个形状的类型（E3）。
        // amount 先取（原始顺序是 raw 借用贯穿整行），语义不变：同一份 raw。
        let amount = state.params.masks[i]
            .get("amount")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(100.0);
        let shapes = state.mask_geos_cached(i);
        let comps = shapes.len().max(1);
        // 三段各自翻译再拼：形状名是枚举式的词，段数与百分比是数字。
        // 整串当一个键翻不了——里面带着会变的数量与百分比。
        let kind = if shapes.first().is_some_and(|g| g.brush) {
            crate::i18n::t!("画笔")
        } else if shapes.first().is_some_and(|g| g.radial) {
            crate::i18n::t!("径向")
        } else {
            crate::i18n::t!("线性")
        };
        let mut label =
            crate::i18n::tr_fmt("{}  {}  {}%", &[&(i + 1), &kind, &format!("{amount:.0}")]);
        if comps > 1 {
            label.push_str(&crate::i18n::tr_fmt("  · {} 形状", &[&comps]));
        }
        let (rect, row) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 21.0), Sense::click());
        let p = ui.painter();
        if selected || row.hovered() {
            p.rect_filled(
                rect,
                radius_control(),
                if selected { palette().selected } else { elev() },
            );
        }
        if selected {
            p.rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(2.0, rect.height())),
                0,
                accent(),
            );
        }
        p.text(
            Pos2::new(rect.left() + 9.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font(SIZE_SMALL),
            if selected { text() } else { muted() },
        );
        let del = Rect::from_center_size(
            Pos2::new(rect.right() - 10.0, rect.center().y),
            Vec2::splat(16.0),
        );
        let dr = ui.interact(del, ui.make_persistent_id(("mask-del", i)), Sense::click());
        if dr.hovered() {
            p.rect_filled(del, radius_control(), elev_hover());
        }
        icons::paint(
            p,
            del.shrink(5.0),
            Glyph::Close,
            if dr.hovered() { text() } else { muted() },
        );
        if dr.clicked() {
            remove = Some(i);
        } else if row.clicked() {
            select = Some(i);
        }
    }

    if let Some(kind) = add {
        state.add_mask(kind);
    }
    if let Some(i) = remove {
        state.remove_mask(i);
    }
    if let Some(i) = select {
        state.mask_sel = Some(i);
        state.preview_dirty = true;
    }

    let count = state.params.masks.len();
    let Some(sel) = state.mask_sel.filter(|i| *i < count) else {
        state.mask_sel = None;
        note(ui, "尚无蒙版");
        return;
    };
    let mut changed = false;

    // 形状条：一组里可以叠多个线性/径向，布尔方式决定后面形状怎么参与选区。
    let comps = mask_comp_count(&state.params.masks[sel]);
    let mut comp_action = CompAction::None;
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("形状"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        for c in 0..comps {
            if bar_chip(ui, &format!("{}", c + 1), state.mask_comp == c).clicked() {
                comp_action = CompAction::Select(c);
            }
        }
        if bar_button(ui, "+ 线性").clicked() {
            comp_action = CompAction::Add("linear");
        }
        if bar_button(ui, "+ 径向").clicked() {
            comp_action = CompAction::Add("radial");
        }
        if bar_button(ui, "+ 画笔").clicked() {
            comp_action = CompAction::Add("brush");
        }
        if comps > 1 && bar_button(ui, "删形状").clicked() {
            comp_action = CompAction::Remove;
        }
    });
    match comp_action {
        CompAction::None => {}
        CompAction::Select(c) => {
            state.mask_comp = c;
        }
        CompAction::Add(kind) => {
            state.add_mask_component(sel, kind);
            return;
        }
        CompAction::Remove => {
            state.remove_mask_component(sel, state.mask_comp);
            return;
        }
    }
    let Some(mut view) = mask_view(&state.params.masks[sel], state.mask_comp) else {
        return;
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(crate::i18n::t!("布尔"))
                .size(SIZE_SMALL)
                .color(dim()),
        );
        for (index, label) in MASK_BLEND_LABELS.iter().enumerate() {
            let tip = match index {
                0 => "并入已有选区",
                1 => "从已有选区里挖掉这块",
                _ => "只保留与已有选区的重叠",
            };
            if bar_chip(ui, label, view.blend == index)
                .on_hover_text(tip)
                .clicked()
            {
                view.blend = index;
                changed = true;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(crate::i18n::tr_fmt(
                    "第 {} 张 · 形状 {}",
                    &[&(sel + 1), &(state.mask_comp + 1)],
                ))
                .size(SIZE_SMALL)
                .color(dim()),
            );
        });
    });
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let mut comp_invert = view.comp_invert;
        if ui
            .checkbox(
                &mut comp_invert,
                RichText::new(crate::i18n::t!("形状反选")).size(SIZE_SMALL),
            )
            .changed()
        {
            view.comp_invert = comp_invert;
            changed = true;
        }
        let mut invert = view.invert;
        if ui
            .checkbox(
                &mut invert,
                RichText::new(crate::i18n::t!("整组反选")).size(SIZE_SMALL),
            )
            .changed()
        {
            view.invert = invert;
            changed = true;
        }
    });
    changed |= slider(
        ui,
        ("mask", sel, "amount"),
        "强度",
        &mut view.amount,
        0.0,
        100.0,
        100.0,
        0,
        false,
    );
    if view.geo.brush {
        // 画笔没有渐变几何：羽化/中点/角度换成下一笔的圆章设定。
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(crate::i18n::t!("下一笔"))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
            let mut erase = state.brush_erase;
            if ui
                .checkbox(
                    &mut erase,
                    RichText::new(crate::i18n::t!("橡皮擦")).size(SIZE_SMALL),
                )
                .on_hover_text(crate::i18n::t!(
                    "勾上后画的每一笔都从选区里挖掉；画完想补回来就再点一次取消勾选。"
                ))
                .changed()
            {
                state.brush_erase = erase;
            }
            if bar_button(ui, "清空笔画").clicked() {
                brush_clear(state, sel, state.mask_comp);
            }
        });
        let mut radius = state.brush_radius;
        if slider(
            ui,
            ("brush", "radius"),
            "半径",
            &mut radius,
            0.2,
            50.0,
            6.0,
            1,
            false,
        ) {
            state.brush_radius = radius;
        }
        let mut hardness = state.brush_hardness;
        if slider(
            ui,
            ("brush", "hardness"),
            "硬度",
            &mut hardness,
            0.0,
            100.0,
            70.0,
            0,
            false,
        ) {
            state.brush_hardness = hardness;
        }
        let mut flow = state.brush_flow;
        if slider(
            ui,
            ("brush", "flow"),
            "流量",
            &mut flow,
            1.0,
            100.0,
            100.0,
            0,
            false,
        ) {
            state.brush_flow = flow;
        }
        note(ui, "这三个推子只影响接下来的笔画。");
    } else {
        changed |= slider(
            ui,
            ("mask", sel, state.mask_comp, "feather"),
            "羽化",
            &mut view.geo.feather,
            0.0,
            100.0,
            if view.geo.radial { 35.0 } else { 45.0 },
            0,
            false,
        );
        changed |= slider(
            ui,
            ("mask", sel, state.mask_comp, "midpoint"),
            "中点",
            &mut view.geo.midpoint,
            0.0,
            100.0,
            50.0,
            0,
            false,
        );
        changed |= slider(
            ui,
            ("mask", sel, state.mask_comp, "angle"),
            "角度",
            &mut view.geo.angle,
            0.0,
            360.0,
            0.0,
            0,
            false,
        );
    }

    ui.add_space(2.0);
    let mut range_on = view.range_on;
    if ui
        .checkbox(
            &mut range_on,
            RichText::new(crate::i18n::t!("亮度范围")).size(SIZE_SMALL),
        )
        .changed()
    {
        view.range_on = range_on;
        changed = true;
    }
    if view.range_on {
        changed |= slider(
            ui,
            ("mask", sel, "lum_min"),
            "亮度下限",
            &mut view.lum[0],
            0.0,
            100.0,
            LUM_DEFAULT[0],
            0,
            false,
        );
        changed |= slider(
            ui,
            ("mask", sel, "lum_max"),
            "亮度上限",
            &mut view.lum[1],
            0.0,
            100.0,
            LUM_DEFAULT[1],
            0,
            false,
        );
        changed |= slider(
            ui,
            ("mask", sel, "lum_feather"),
            "范围羽化",
            &mut view.lum[2],
            0.0,
            100.0,
            LUM_DEFAULT[2],
            0,
            false,
        );
    }

    ui.add_space(2.0);
    let mut hue_on = view.hue_on;
    if ui
        .checkbox(
            &mut hue_on,
            RichText::new(crate::i18n::t!("颜色范围")).size(SIZE_SMALL),
        )
        .changed()
    {
        view.hue_on = hue_on;
        if !hue_on {
            state.mask_pick = false;
        }
        changed = true;
    }
    if view.hue_on {
        changed |= slider(
            ui,
            ("mask", sel, "hue"),
            "目标色相",
            &mut view.hue[0],
            0.0,
            360.0,
            HUE_DEFAULT[0],
            0,
            false,
        );
        changed |= slider(
            ui,
            ("mask", sel, "hue_width"),
            "色相宽度",
            &mut view.hue[1],
            0.0,
            180.0,
            HUE_DEFAULT[1],
            0,
            false,
        );
        changed |= slider(
            ui,
            ("mask", sel, "hue_feather"),
            "范围羽化",
            &mut view.hue[2],
            0.0,
            90.0,
            HUE_DEFAULT[2],
            0,
            false,
        );
        ui.horizontal(|ui| {
            if bar_button(
                ui,
                if state.mask_pick {
                    "点画布取色…"
                } else {
                    "吸管取色"
                },
            )
            .clicked()
            {
                state.mask_pick = !state.mask_pick;
            }
            let (rect, _) = ui.allocate_exact_size(Vec2::new(30.0, 15.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, radius_chip(), hue_color(view.hue[0]));
            ui.label(
                RichText::new(format!("{:.0}°", view.hue[0]))
                    .size(SIZE_SMALL)
                    .color(dim()),
            );
        });
        note(ui, "颜色范围按色相圈选一段，近灰像素自动落选。");
    }

    if group_header(ui, state, "mask-params", "局部参数", true) {
        for (key, label, min, max, decimals) in MASK_SLIDERS {
            let value = view.params.entry((*key).to_owned()).or_insert(0.0);
            changed |= slider(
                ui,
                ("mask", sel, *key),
                label,
                value,
                *min,
                *max,
                0.0,
                *decimals,
                true,
            );
        }
        note(
            ui,
            "局部参数是相对全局的增量，只在蒙版权重内生效；0 表示跟全局一致。",
        );
    }

    if changed {
        mask_write(&mut state.params.masks[sel], state.mask_comp, &view);
        state.masks_changed();
    }
}

/// 形状条上这一帧要做的事；改完直接重画整块面板，避免拿着旧 view 继续写。
enum CompAction {
    None,
    Select(usize),
    Add(&'static str),
    Remove,
}

pub(super) fn mask_pixel_size(state: &AppState) -> (f64, f64) {
    if let Some((w, h)) = state.resolution {
        return (w.max(2) as f64, h.max(2) as f64);
    }
    if state.preview_size[0] > 0 && state.preview_size[1] > 0 {
        return (state.preview_size[0] as f64, state.preview_size[1] as f64);
    }
    (16.0, 9.0)
}

/// 把存储坐标按角度绕中点旋转成画布上真正看到的线段端点。
fn mask_linear_ends(geo: &MaskGeo, w: f64, h: f64) -> ((f64, f64), (f64, f64)) {
    let (mut ax, mut ay) = (geo.x1 * (w - 1.0), geo.y1 * (h - 1.0));
    let (mut bx, mut by) = (geo.x2 * (w - 1.0), geo.y2 * (h - 1.0));
    if geo.angle.abs() > 1e-9 {
        let (sin, cos) = geo.angle.to_radians().sin_cos();
        let (mx, my) = ((ax + bx) * 0.5, (ay + by) * 0.5);
        let (dx, dy) = (ax - mx, ay - my);
        ax = mx + dx * cos - dy * sin;
        ay = my + dx * sin + dy * cos;
        let (dx, dy) = (bx - mx, by - my);
        bx = mx + dx * cos - dy * sin;
        by = my + dx * sin + dy * cos;
    }
    (
        (ax / (w - 1.0), ay / (h - 1.0)),
        (bx / (w - 1.0), by / (h - 1.0)),
    )
}

fn mask_radial_axes(geo: &MaskGeo, w: f64, h: f64) -> (f64, f64, f64, f64) {
    let dx = (geo.x2 - geo.x1) * (w - 1.0);
    let dy = (geo.y2 - geo.y1) * (h - 1.0);
    let mut rx = dx.abs();
    let mut ry = dy.abs();
    if ry < (h * 0.01).max(1.0) {
        ry = rx.max(1.0);
    } else if rx < (w * 0.01).max(1.0) {
        rx = ry.max(1.0);
    }
    (rx, ry, dx, dy)
}

pub(crate) fn mask_radial_points(geo: &MaskGeo, img: Rect, w: f64, h: f64) -> Vec<Pos2> {
    let (rx, ry, _, _) = mask_radial_axes(geo, w, h);
    let (sin, cos) = geo.angle.to_radians().sin_cos();
    let cx = geo.x1 * (w - 1.0);
    let cy = geo.y1 * (h - 1.0);
    let mut points = Vec::with_capacity(65);
    for k in 0..=64 {
        let t = k as f64 / 64.0 * std::f64::consts::TAU;
        let (u, v) = (rx * t.cos(), ry * t.sin());
        let px = u * cos - v * sin;
        let py = u * sin + v * cos;
        points.push(mask_screen(
            img,
            (cx + px) / (w - 1.0),
            (cy + py) / (h - 1.0),
        ));
    }
    points
}

fn mask_screen(img: Rect, x: f64, y: f64) -> Pos2 {
    Pos2::new(
        img.left() + x as f32 * img.width(),
        img.top() + y as f32 * img.height(),
    )
}

fn mask_norm(img: Rect, pos: Pos2) -> (f64, f64) {
    (
        ((pos.x - img.left()) / img.width().max(1.0)) as f64,
        ((pos.y - img.top()) / img.height().max(1.0)) as f64,
    )
}

/// 吸管取色：把点到的像素色相写进选中蒙版的颜色范围并开启它。
/// 采样自当前预览（所见即所得）；取完就退出取色态。
pub(super) fn mask_pick_color(state: &mut AppState, img: Rect, pos: Pos2, sel: usize) {
    state.mask_pick = false;
    let Some(image) = state.mask_pick_img.take() else {
        return;
    };
    let (nx, ny) = mask_norm(img, pos);
    let (iw, ih) = (image.width().max(1), image.height().max(1));
    let x = (nx.clamp(0.0, 1.0) * (iw - 1) as f64).round() as usize;
    let y = (ny.clamp(0.0, 1.0) * (ih - 1) as f64).round() as usize;
    let px = image.pixels[y * iw + x];
    let (r, g, b) = (
        f64::from(px.r()) / 255.0,
        f64::from(px.g()) / 255.0,
        f64::from(px.b()) / 255.0,
    );
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let chroma = max - min;
    let hue = if chroma <= 1e-9 {
        0.0
    } else {
        let (sec, frac) = if max == r {
            ((g - b) / chroma, 0.0)
        } else if max == g {
            ((b - r) / chroma, 2.0)
        } else {
            ((r - g) / chroma, 4.0)
        };
        ((sec + frac) / 6.0).rem_euclid(1.0) * 360.0
    };
    let Some(raw) = state.params.masks.get_mut(sel) else {
        return;
    };
    if !raw
        .as_object()
        .is_some_and(|o| o.contains_key("components"))
    {
        *raw = aevum_core::mask::as_group(raw);
    }
    let Some(object) = raw.as_object_mut() else {
        return;
    };
    let mut range = object
        .get("range")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    range.insert("hue_on".to_owned(), serde_json::json!(true));
    range.insert("hue".to_owned(), serde_json::json!(hue));
    range.insert("hue_width".to_owned(), serde_json::json!(40.0));
    range.insert("hue_feather".to_owned(), serde_json::json!(20.0));
    object.insert("range".to_owned(), serde_json::Value::Object(range));
    state.masks_changed();
}

/// 手柄屏幕位置：线性是两端，径向是圆心与半径点；画笔没有手柄。
pub(crate) fn mask_handles(geo: &MaskGeo, img: Rect, w: f64, h: f64) -> Vec<(Pos2, bool)> {
    if geo.brush {
        return Vec::new();
    }
    if geo.radial {
        let (_, _, dx, dy) = mask_radial_axes(geo, w, h);
        let (sin, cos) = geo.angle.to_radians().sin_cos();
        let hx = geo.x1 + (dx * cos - dy * sin) / (w - 1.0);
        let hy = geo.y1 + (dx * sin + dy * cos) / (h - 1.0);
        vec![
            (mask_screen(img, geo.x1, geo.y1), true),
            (mask_screen(img, hx, hy), false),
        ]
    } else {
        let (a, b) = mask_linear_ends(geo, w, h);
        vec![
            (mask_screen(img, a.0, a.1), true),
            (mask_screen(img, b.0, b.1), false),
        ]
    }
}

fn mask_hit(geo: &MaskGeo, img: Rect, w: f64, h: f64, pos: Pos2) -> Option<u8> {
    if geo.brush {
        return None;
    }
    let handles = mask_handles(geo, img, w, h);
    for (index, (handle, _)) in handles.iter().enumerate() {
        if handle.distance(pos) <= 11.0 {
            return Some(index as u8);
        }
    }
    None
}

/// 虚拟手柄：抓在形状主体上（不在任何手柄附近）= 整体移动。
///
/// Virtual handle: grabbed on the shape body (near no handle) = move the whole shape.
pub(crate) const HANDLE_MOVE: u8 = 2;

/// 指针是否落在**当前选中形状的主体**上（radial：圆内；linear：轴线两侧 24px
/// 的带内）。命中返回按下时刻的端点快照 `[x1,y1,x2,y2]`，整体移动以它为基准，
/// 松手即弃。画笔没有主体，永远 None。
///
/// Whether the pointer lands on the **body of the selected shape** (radial: inside the
/// disc; linear: within 24 px of the axis). A hit returns the endpoint snapshot
/// `[x1,y1,x2,y2]` taken at press time -- the basis for the whole-shape move, dropped on
/// release. Brush shapes have no body and always return None.
pub(crate) fn mask_body_geo(state: &AppState, img: Rect, pos: Pos2) -> Option<[f64; 4]> {
    let raw = state.params.masks.get(state.mask_sel?)?;
    let geo = mask_comp_object(raw, state.mask_comp).map(mask_geo_of)?;
    if geo.brush {
        return None;
    }
    let handles = mask_handles(
        &geo,
        img,
        mask_pixel_size(state).0,
        mask_pixel_size(state).1,
    );
    if geo.radial {
        // 圆内：圆心到手柄 1（半径端点）的距离就是盘面半径。
        let (center, edge) = (handles[0].0, handles[1].0);
        (pos.distance(center) <= center.distance(edge)).then_some([geo.x1, geo.y1, geo.x2, geo.y2])
    } else {
        // 轴带：点到线段 a-b 的距离。参数化投影 clamp 到 [0,1]。
        let (a, b) = (handles[0].0, handles[1].0);
        let ab = b - a;
        let t = ((pos - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
        let foot = a + ab * t;
        (pos.distance(foot) <= 24.0).then_some([geo.x1, geo.y1, geo.x2, geo.y2])
    }
}

pub(super) fn mask_hit_any(
    geos: &[MaskGeo],
    img: Rect,
    w: f64,
    h: f64,
    pos: Pos2,
    current: usize,
) -> Option<(usize, u8)> {
    // 先查正在编辑的形状：重叠时手柄该归亮的那一个。
    let current_hit = geos
        .get(current)
        .and_then(|geo| mask_hit(geo, img, w, h, pos))
        .map(|handle| (current, handle));
    if current_hit.is_some() {
        return current_hit;
    }
    geos.iter()
        .enumerate()
        .rev()
        .find_map(|(index, geo)| mask_hit(geo, img, w, h, pos).map(|handle| (index, handle)))
}

pub(super) fn mask_drag_handle(state: &mut AppState, img: Rect, pos: Pos2) {
    let Some((index, handle)) = state.mask_drag else {
        return;
    };
    let comp = state.mask_comp;
    let Some(raw) = state.params.masks.get(index) else {
        state.mask_drag = None;
        return;
    };
    // 整体移动：位移 = 当前指针 - 按下锚点，作用在按下时刻的几何快照上。
    // 绝对定位会让蒙版瞬移（指针抓的点和形状原点不重合），必须走 delta。
    //
    // Whole-shape move: delta = current pointer - press anchor, applied to the geometry
    // snapshot taken at press. Absolute positioning would teleport the mask (the grab
    // point does not coincide with the shape origin), so it has to be a delta.
    if handle == HANDLE_MOVE {
        let Some((from, snap)) = state.mask_move_anchor else {
            state.mask_drag = None;
            return;
        };
        let (nx, ny) = mask_norm(img, pos);
        let (fx, fy) = mask_norm(img, from);
        let (dx, dy) = (nx - fx, ny - fy);
        // 快照里已有四个端点，不需要再读当前几何；先算完再取可变借用写回。
        //
        // The snapshot already carries the four endpoints; no need to read the current
        // geometry -- compute first, then take the mutable borrow to write back.
        let mut geo = MaskGeo {
            radial: true,
            brush: false,
            x1: (snap[0] + dx).clamp(-0.5, 1.5),
            y1: (snap[1] + dy).clamp(-0.5, 1.5),
            x2: (snap[2] + dx).clamp(-0.5, 1.5),
            y2: (snap[3] + dy).clamp(-0.5, 1.5),
            feather: 0.0,
            midpoint: 0.0,
            angle: 0.0,
        };
        let write = state
            .params
            .masks
            .get_mut(index)
            .and_then(|r| mask_comp_object_mut(r, comp));
        let Some(comp_mut) = write else {
            return;
        };
        // 只搬端点：feather / midpoint / angle 保留盘上原值。
        //
        // Only the endpoints move: feather / midpoint / angle keep their on-disc values.
        let cur = mask_geo_of(comp_mut);
        geo.feather = cur.feather;
        geo.midpoint = cur.midpoint;
        geo.angle = cur.angle;
        geo.radial = cur.radial;
        geo.brush = cur.brush;
        mask_write_geo(comp_mut, &geo);
        state.masks_changed();
        return;
    }
    let Some(mut geo) = mask_comp_object(raw, comp).map(mask_geo_of) else {
        return;
    };
    let (w, h) = mask_pixel_size(state);
    let (nx, ny) = mask_norm(img, pos);
    let (px, py) = (nx * (w - 1.0), ny * (h - 1.0));
    let (sin, cos) = geo.angle.to_radians().sin_cos();
    if geo.radial {
        let (cx, cy) = (geo.x1 * (w - 1.0), geo.y1 * (h - 1.0));
        if !handle_is_center(handle) {
            let (dx, dy) = (px - cx, py - cy);
            let (ux, uy) = if geo.angle.abs() > 1e-9 {
                (dx * cos + dy * sin, -dx * sin + dy * cos)
            } else {
                (dx, dy)
            };
            geo.x2 = ((cx + ux) / (w - 1.0)).clamp(-0.5, 1.5);
            geo.y2 = ((cy + uy) / (h - 1.0)).clamp(-0.5, 1.5);
        } else {
            let nx = nx.clamp(-0.5, 1.5);
            let ny = ny.clamp(-0.5, 1.5);
            let dx = nx - geo.x1;
            let dy = ny - geo.y1;
            geo.x1 = nx;
            geo.y1 = ny;
            geo.x2 = (geo.x2 + dx).clamp(-0.5, 1.5);
            geo.y2 = (geo.y2 + dy).clamp(-0.5, 1.5);
        }
    } else {
        let (mx, my) = (
            (geo.x1 + geo.x2) * 0.5 * (w - 1.0),
            (geo.y1 + geo.y2) * 0.5 * (h - 1.0),
        );
        let (dx, dy) = (px - mx, py - my);
        let (ux, uy) = if geo.angle.abs() > 1e-9 {
            (dx * cos + dy * sin, -dx * sin + dy * cos)
        } else {
            (dx, dy)
        };
        let nx = ((mx + ux) / (w - 1.0)).clamp(-0.5, 1.5);
        let ny = ((my + uy) / (h - 1.0)).clamp(-0.5, 1.5);
        if handle_is_center(handle) {
            geo.x1 = nx;
            geo.y1 = ny;
        } else {
            geo.x2 = nx;
            geo.y2 = ny;
        }
    }
    let write = state
        .params
        .masks
        .get_mut(index)
        .and_then(|raw| mask_comp_object_mut(raw, comp));
    if let Some(comp) = write {
        mask_write_geo(comp, &geo);
    }
    state.masks_changed();
}

fn handle_is_center(handle: u8) -> bool {
    handle == 0
}

pub(super) fn mask_comp_is_brush(state: &AppState, sel: usize, comp: usize) -> bool {
    state
        .params
        .masks
        .get(sel)
        .and_then(|raw| mask_comp_object(raw, comp))
        .is_some_and(is_brush)
}

/// 画笔圆圈在画布上的半径：core 按 `radius × 场短边` 盖章，屏幕上跟着预览缩放走。
fn brush_screen_radius(state: &AppState, img: Rect) -> f32 {
    let (gw, gh) = mask_pixel_size(state);
    let r_field = (state.brush_radius / 100.0).clamp(0.002, 0.5) * gw.min(gh);
    ((r_field * img.width() as f64 / gw.max(1.0)) as f32).max(2.0)
}

fn brush_note(state: &AppState) {
    history::note(
        "mask.brush",
        if state.brush_erase {
            "画笔 擦除"
        } else {
            "画笔"
        },
    );
}

/// 起笔：把当前画笔设定存成一条新笔画，第一点就是按下的位置。
/// 色相（0~360）转满饱和的示意色，画在颜色范围的色块里。
fn hue_color(deg: f64) -> Color32 {
    let h = deg.rem_euclid(360.0) / 60.0;
    let f = h - h.floor();
    let (r, g, b) = match h.floor() as i64 % 6 {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    };
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

pub(super) fn brush_begin(state: &mut AppState, img: Rect, pos: Pos2) {
    let Some((sel, comp)) = state.mask_paint else {
        return;
    };
    let (nx, ny) = mask_norm(img, pos);
    let stroke = serde_json::json!({
        "points": [[nx.clamp(0.0, 1.0), ny.clamp(0.0, 1.0)]],
        "radius": (state.brush_radius / 100.0).clamp(0.002, 0.5),
        "hardness": (state.brush_hardness / 100.0).clamp(0.0, 1.0),
        "flow": (state.brush_flow / 100.0).clamp(0.01, 1.0),
        "erase": state.brush_erase as u8,
    });
    if let Some(raw) = state.params.masks.get_mut(sel)
        && let Some(obj) = mask_comp_object_mut(raw, comp)
    {
        let list = obj
            .entry("strokes".to_owned())
            .or_insert_with(|| serde_json::json!([]));
        if let Some(arr) = list.as_array_mut() {
            // 与 core 的上限同口径：满了挤掉最老的一笔，不许无限膨胀。
            if arr.len() >= aevum_core::mask::MAX_STROKES {
                arr.remove(0);
            }
            arr.push(stroke);
        }
    }
    state.masks_changed();
    brush_note(state);
}

/// 拖动中加点：隔开约 2.5 屏幕像素才记一点，点数封顶与 core 一致。
pub(super) fn brush_extend(state: &mut AppState, img: Rect, pos: Pos2) {
    let Some((sel, comp)) = state.mask_paint else {
        return;
    };
    let (nx, ny) = mask_norm(img, pos);
    let (nx, ny) = (nx.clamp(0.0, 1.0), ny.clamp(0.0, 1.0));
    if let Some(raw) = state.params.masks.get_mut(sel)
        && let Some(obj) = mask_comp_object_mut(raw, comp)
        && let Some(arr) = obj
            .get_mut("strokes")
            .and_then(serde_json::Value::as_array_mut)
        && let Some(last) = arr.last_mut().and_then(serde_json::Value::as_object_mut)
        && let Some(points) = last
            .get_mut("points")
            .and_then(serde_json::Value::as_array_mut)
        && let Some(tail) = points.last().and_then(serde_json::Value::as_array)
        && let (Some(lx), Some(ly)) = (
            tail.first().and_then(serde_json::Value::as_f64),
            tail.get(1).and_then(serde_json::Value::as_f64),
        )
    {
        let dist = ((nx - lx) * img.width() as f64).hypot((ny - ly) * img.height() as f64);
        if dist < 2.5 || points.len() >= aevum_core::mask::MAX_STROKE_POINTS {
            return;
        }
        points.push(serde_json::json!([nx, ny]));
    }
    state.masks_changed();
    brush_note(state);
}

/// 清空这个画笔组件的全部笔画。
fn brush_clear(state: &mut AppState, sel: usize, comp: usize) {
    if let Some(raw) = state.params.masks.get_mut(sel)
        && let Some(obj) = mask_comp_object_mut(raw, comp)
    {
        obj.insert("strokes".to_owned(), serde_json::json!([]));
    }
    state.masks_changed();
    history::note("mask.brush", "画笔 清空");
}

pub(super) fn draw_mask_overlay(
    p: &egui::Painter,
    state: &mut AppState,
    img: Rect,
    hover: Option<Pos2>,
) {
    if state.module != Module::Develop || state.tool == Tool::Crop {
        return;
    }
    let Some(sel) = state.mask_sel else {
        return;
    };
    if state.params.masks.get(sel).is_none() {
        return;
    };
    // 缓存取值：指纹命中零分配（E1）。一帧一次 prep，随后的读取全是纯读零哈希。
    let (w, h) = mask_pixel_size(state);
    let count = state.mask_prep(sel, img, w, h);
    let Some(current) = count.checked_sub(1).map(|last| state.mask_comp.min(last)) else {
        return;
    };
    let geos: Vec<MaskGeo> = (0..count).filter_map(|i| state.mask_geo_get(i)).collect();
    for (index, geo) in geos.iter().enumerate() {
        let active = index == current;
        let line = Stroke::new(
            if active { 1.2 } else { 0.9 },
            if active {
                Color32::from_white_alpha(215)
            } else {
                Color32::from_white_alpha(85)
            },
        );
        if geo.brush {
            for (points, erase) in state.mask_strokes_now(index) {
                if points.len() > 1 {
                    let stroke = if *erase {
                        Stroke::new(
                            line.width,
                            Color32::from_rgba_unmultiplied(255, 110, 110, 190),
                        )
                    } else {
                        line
                    };
                    p.add(egui::Shape::line(points.clone(), stroke));
                }
            }
            continue;
        }
        if geo.radial {
            let points = state.mask_radial_now(index);
            if points.len() > 1 {
                p.add(egui::Shape::closed_line(points.to_vec(), line));
            }
        } else {
            let (a, b) = mask_linear_ends(geo, w, h);
            let a = mask_screen(img, a.0, a.1);
            let b = mask_screen(img, b.0, b.1);
            p.line_segment([a, b], line);
        }
    }
    let geo = geos[current];
    if geo.brush {
        // 画笔没有手柄：光标位置画一个跟着半径走的圈，橡皮擦态加一圈红。
        if let Some(pos) = hover.filter(|pos| img.contains(*pos)) {
            let r = brush_screen_radius(state, img);
            p.circle_stroke(pos, r, Stroke::new(1.0, accent()));
            if state.brush_erase {
                p.circle_stroke(
                    pos,
                    (r * 0.55).max(2.5),
                    Stroke::new(0.9, Color32::from_rgb(255, 110, 110)),
                );
            }
        }
    } else if geo.radial {
        let center = mask_screen(img, geo.x1, geo.y1);
        p.circle_stroke(center, 3.5, Stroke::new(1.0, accent()));
    } else {
        let (a, b) = mask_linear_ends(&geo, w, h);
        let a = mask_screen(img, a.0, a.1);
        let b = mask_screen(img, b.0, b.1);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = dx.hypot(dy).max(1e-3);
        let normal = Vec2::new(-dy / len, dx / len) * 22.0;
        let mid = Pos2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
        p.line_segment(
            [mid - normal, mid + normal],
            Stroke::new(0.9, Color32::from_white_alpha(90)),
        );
    }
    let drag = state.mask_drag;
    for (index, (pos, _)) in state.mask_handles_now().iter().enumerate() {
        let active = drag == Some((sel, index as u8));
        p.circle_filled(*pos, if active { 5.2 } else { 4.2 }, Color32::WHITE);
        p.circle_stroke(
            *pos,
            if active { 6.4 } else { 5.4 },
            Stroke::new(1.0, accent()),
        );
    }
}

/// 一条笔画的屏幕折线与擦除标记；单点笔画返回单点，画圆点。
pub(crate) fn mask_stroke_lines(
    comp: &serde_json::Map<String, serde_json::Value>,
    img: Rect,
) -> Vec<(Vec<Pos2>, bool)> {
    comp.get("strokes")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    let obj = s.as_object()?;
                    let mut points = Vec::new();
                    for pt in obj.get("points")?.as_array()? {
                        let a = pt.as_array()?;
                        points.push(mask_screen(img, a.first()?.as_f64()?, a.get(1)?.as_f64()?));
                    }
                    if points.is_empty() {
                        return None;
                    }
                    Some((points, mask_flag(obj.get("erase"))))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod mask_geometry_tests {
    use super::*;

    fn geo(radial: bool, x1: f64, y1: f64, x2: f64, y2: f64, angle: f64) -> MaskGeo {
        MaskGeo {
            radial,
            brush: false,
            x1,
            y1,
            x2,
            y2,
            feather: 45.0,
            midpoint: 50.0,
            angle,
        }
    }

    #[test]
    fn linear_ends_rotate_about_the_midpoint() {
        let g = geo(false, 0.25, 0.5, 0.75, 0.5, 0.0);
        let ((ax, ay), (bx, by)) = mask_linear_ends(&g, 101.0, 101.0);
        assert!((ax - 0.25).abs() < 1e-9);
        assert!((ay - 0.5).abs() < 1e-9);
        assert!((bx - 0.75).abs() < 1e-9);
        assert!((by - 0.5).abs() < 1e-9);

        // 90° 后线段竖起来，中点不变。
        let g = geo(false, 0.25, 0.5, 0.75, 0.5, 90.0);
        let ((ax, ay), (bx, by)) = mask_linear_ends(&g, 101.0, 101.0);
        let (mx, my) = ((ax + bx) * 0.5, (ay + by) * 0.5);
        assert!((mx - 0.5).abs() < 1e-9, "{mx}");
        assert!((my - 0.5).abs() < 1e-9, "{my}");
        assert!((ax - bx).abs() < 1e-9, "{ax} {bx}");
        assert!((ay - by).abs() > 0.4, "{ay} {by}");
    }

    #[test]
    fn radial_axes_fall_back_to_a_circle() {
        // 只横向拖：ry 极小，按 Python 规则补成 rx。
        let g = geo(true, 0.5, 0.5, 0.75, 0.5, 0.0);
        let (rx, ry, _, _) = mask_radial_axes(&g, 101.0, 101.0);
        assert!((rx - 25.0).abs() < 1e-9, "{rx}");
        assert!((ry - rx).abs() < 1e-9, "{ry}");

        // 只纵向拖：rx 极小，补成 ry。
        let g = geo(true, 0.5, 0.5, 0.5, 0.75, 0.0);
        let (rx, ry, _, _) = mask_radial_axes(&g, 101.0, 101.0);
        assert!((ry - 25.0).abs() < 1e-9, "{ry}");
        assert!((rx - ry).abs() < 1e-9, "{rx}");
    }

    #[test]
    fn radial_size_handle_sits_at_the_edge_point() {
        let img = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(200.0, 100.0));
        let g = geo(true, 0.5, 0.5, 0.75, 0.75, 0.0);
        let handles = mask_handles(&g, img, 201.0, 101.0);
        assert!((handles[0].0.x - 110.0).abs() < 1e-3);
        assert!((handles[0].0.y - 70.0).abs() < 1e-3);
        assert!((handles[1].0.x - 160.0).abs() < 1e-3, "{:?}", handles[1].0);
        assert!((handles[1].0.y - 95.0).abs() < 1e-3, "{:?}", handles[1].0);
    }

    #[test]
    fn screen_and_normalized_roundtrip() {
        let img = Rect::from_min_size(Pos2::new(4.0, 8.0), Vec2::new(300.0, 150.0));
        let pos = mask_screen(img, 0.25, 0.6);
        let (x, y) = mask_norm(img, pos);
        assert!((x - 0.25).abs() < 1e-6);
        assert!((y - 0.6).abs() < 1e-6);
    }
}

#[cfg(test)]
mod mask_panel_tests {
    use super::*;
    use crate::shell::ui;
    use crate::theme;
    use aevum_core::mask::{self, ShapeKind};
    use egui::Context;
    use serde_json::json;

    fn group_two_shapes() -> serde_json::Value {
        let mut raw = mask::new_mask(ShapeKind::Radial);
        raw["components"]
            .as_array_mut()
            .expect("新蒙版本来就带一个形状")
            .push(mask::new_component(ShapeKind::Linear));
        raw
    }

    #[test]
    fn panel_writes_only_the_selected_shape() {
        let mut raw = group_two_shapes();
        let mut view = mask_view(&raw, 1).expect("第二个形状读得出来");
        assert!(!view.geo.radial, "扁平 kind 没读对");

        view.blend = 1;
        view.comp_invert = true;
        view.geo.x1 = 0.11;
        view.geo.feather = 60.0;
        view.amount = 70.0;
        view.invert = true;
        view.range_on = true;
        view.lum = [80.0, 20.0, 5.0];
        view.params.insert("exposure".to_owned(), 1.25);
        mask_write(&mut raw, 1, &view);

        let first = mask_comp_object(&raw, 0).expect("第一个形状还在");
        assert_eq!(first["x1"], json!(0.5), "改第二个形状不该串到第一个");
        assert_eq!(first["blend"], json!("add"));
        let second = mask_comp_object(&raw, 1).expect("第二个形状还在");
        assert_eq!(second["x1"], json!(0.11));
        assert_eq!(second["feather"], json!(60.0));
        assert_eq!(second["blend"], json!("subtract"));
        assert_eq!(second["invert"], json!(true));
        assert_eq!(raw["amount"], json!(70.0));
        assert_eq!(raw["invert"], json!(true));
        // 下限/上限写反了要自动对调，存盘永远 min <= max。
        assert_eq!(raw["range"]["lum_min"], json!(0.2));
        assert_eq!(raw["range"]["lum_max"], json!(0.8));
        assert_eq!(raw["range"]["lum_feather"], json!(0.05));
        assert_eq!(raw["params"]["exposure"], json!(1.25));
        assert!(mask::parse_mask(&raw).is_some(), "面板写出来的必须解析得动");

        // 颜色范围：已存的 hue_* 键不许被亮度范围的编辑抹掉；反过来也一样。
        let view = mask_view(&raw, 1).expect("读回");
        let mut view = view;
        view.hue_on = true;
        view.hue = [210.0, 70.0, 25.0];
        mask_write(&mut raw, 1, &view);
        assert_eq!(raw["range"]["hue"], json!(210.0));
        assert_eq!(
            raw["range"]["lum_min"],
            json!(0.2),
            "开颜色范围不抹亮度范围"
        );
        let view = mask_view(&raw, 1).expect("读回");
        let mut view = view;
        view.lum = [10.0, 90.0, 20.0];
        mask_write(&mut raw, 1, &view);
        assert_eq!(raw["range"]["hue"], json!(210.0), "改亮度范围不抹颜色范围");
        assert_eq!(raw["range"]["hue_on"], json!(true));
        let parsed = mask::parse_mask(&raw).expect("仍可解析");
        let hue = parsed.hue.as_ref().expect("面板写回的颜色范围进 core");
        assert_eq!(hue.hue, 210.0);
    }

    #[test]
    fn flat_legacy_mask_upgrades_on_first_edit() {
        let mut raw = json!({
            "id": "m1", "type": "radial",
            "x1": 0.4, "y1": 0.4, "x2": 0.7, "y2": 0.5,
            "feather": 30.0, "invert": 0, "angle": 0.0,
            "midpoint": 50.0, "amount": 100.0,
            "params": {"exposure": 1.0},
        });
        let view = mask_view(&raw, 0).expect("扁平老蒙版也读得出几何");
        assert!(view.geo.radial, "老格式的类型字段在 type 上");
        let mut view = view;
        view.geo.feather = 12.0;
        view.range_on = true;
        mask_write(&mut raw, 0, &view);

        let parsed = mask::parse_mask(&raw).expect("升级后仍可解析");
        assert_eq!(parsed.components.len(), 1);
        assert_eq!(parsed.components[0].shape, ShapeKind::Radial);
        assert_eq!(parsed.components[0].feather, 12.0);
        assert_eq!(
            parsed.params.get("exposure"),
            Some(&1.0),
            "老局部参数要留着"
        );
        assert_eq!(raw["range"]["lum_on"], json!(true));
        assert_eq!(
            raw["range"]["lum_max"],
            json!(0.5),
            "没动过的范围用默认值补"
        );
    }

    fn shape(radial: bool, x1: f64, y1: f64, x2: f64, y2: f64) -> MaskGeo {
        MaskGeo {
            radial,
            brush: false,
            x1,
            y1,
            x2,
            y2,
            feather: 45.0,
            midpoint: 50.0,
            angle: 0.0,
        }
    }

    #[test]
    fn hit_prefers_the_shape_being_edited() {
        let img = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0));
        let a = shape(false, 0.25, 0.5, 0.75, 0.5);
        let b = shape(false, 0.5, 0.2, 0.5, 0.8);
        let geos = vec![a, b];
        // 两个形状的手柄互不重叠：命中谁就是谁。
        let on_b = mask_handles(&b, img, 201.0, 101.0)[1].0;
        assert_eq!(
            mask_hit_any(&geos, img, 201.0, 101.0, on_b, 0),
            Some((1, 1)),
            "点另一个形状的手柄要顺带选中它"
        );
        // 完全重合时，编辑中的那一个优先。
        let same = vec![a, a];
        let on_a = mask_handles(&a, img, 201.0, 101.0)[0].0;
        assert_eq!(
            mask_hit_any(&same, img, 201.0, 101.0, on_a, 1),
            Some((1, 0)),
            "重叠时手柄归亮的那个"
        );
    }

    #[test]
    fn brush_shapes_have_no_handles() {
        let img = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0));
        let raw = mask::new_component(ShapeKind::Brush);
        let geo = mask_geo_of(raw.as_object().expect("新组件是对象"));
        assert!(geo.brush);
        assert!(mask_handles(&geo, img, 200.0, 100.0).is_empty());
        assert_eq!(
            mask_hit(&geo, img, 200.0, 100.0, Pos2::new(100.0, 50.0)),
            None,
            "画布上按到画笔组件不该当成拖手柄"
        );
    }

    #[test]
    fn brush_mask_paints_parses_and_survives_panel_writes() {
        let mut state = AppState::demo();
        let index = state.add_mask("brush");
        state.params.masks[index]["params"] = json!({"exposure": 0.5});
        state.mask_sel = Some(index);
        state.mask_paint = Some((index, 0));
        let img = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0));
        brush_begin(&mut state, img, Pos2::new(100.0, 50.0));
        brush_extend(&mut state, img, Pos2::new(130.0, 50.0));
        brush_extend(&mut state, img, Pos2::new(131.0, 50.0));
        state.mask_paint = None;

        let parsed = mask::parse_mask(&state.params.masks[index]).expect("画笔蒙版解析得动");
        assert_eq!(parsed.components[0].shape, ShapeKind::Brush);
        let strokes = &parsed.components[0].strokes;
        assert_eq!(strokes.len(), 1);
        assert_eq!(strokes[0].points.len(), 2, "太近的点不入列");
        assert!((strokes[0].radius - 0.06).abs() < 1e-9, "默认半径 6%");
        assert!((strokes[0].hardness - 0.7).abs() < 1e-9);
        let mut m = parsed;
        m.build_field(40, 40, None, None);
        assert!(m.field[20 * 40 + 20] > 0.9, "笔画中心该有权重");
        assert!(m.field[0] < 1e-6, "角落不该有权重");

        // 面板写一轮（布尔路径）：kind 不许被抹回 linear，笔画也得原样留着。
        let mut view = mask_view(&state.params.masks[index], 0).expect("画笔也读得出 view");
        assert!(view.geo.brush);
        view.blend = 1;
        mask_write(&mut state.params.masks[index], 0, &view);
        let reparsed = mask::parse_mask(&state.params.masks[index]).unwrap();
        assert_eq!(reparsed.components[0].shape, ShapeKind::Brush);
        assert_eq!(reparsed.components[0].strokes.len(), 1);
        assert_eq!(reparsed.components[0].blend, mask::Blend::Subtract);
    }

    fn paint_count(ctx: &Context, state: &mut AppState) -> usize {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0))),
                max_texture_side: Some(8192),
                ..Default::default()
            },
            |panel| ui(state, panel),
        );
        let count = out.shapes.len();
        out.textures_delta.clear();
        count
    }

    #[test]
    fn added_shape_shows_up_in_the_panel_and_on_the_canvas() {
        let ctx = egui::Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        // 蒙版组现在收在「更多分组」后面：本判据看的是蒙版面板的内容，先展开。
        // The mask group sits behind "more groups" now; this assertion is about the
        // panel's content, so open it first.
        state.show_more_groups = true;
        state.install_demo_textures(&ctx);
        // 走两遍，跟 --selftest 一样把字体与纹理的冷启动排除掉。
        let one = paint_count(&ctx, &mut state);
        let _ = paint_count(&ctx, &mut state);
        assert!(one > 100, "演示态只画出 {one} 个形状");

        let before = mask_comp_count(&state.params.masks[0]);
        state
            .add_mask_component(0, "linear")
            .expect("演示蒙版加得进新形状");
        assert_eq!(mask_comp_count(&state.params.masks[0]), before + 1);
        assert_eq!(state.mask_comp, before, "新形状要立刻成为编辑目标");
        let two = paint_count(&ctx, &mut state);
        assert!(two > one, "新形状的轮廓与形状条没画出来：{one} → {two}");

        // 换选中组时编辑目标回到第一个形状。
        state.select_mask(0);
        assert_eq!(state.mask_comp, 0);
        // 删到只剩一个形状后不许再删，蒙版不能空组；越界序号也不能误删。
        while mask_comp_count(&state.params.masks[0]) > 1 {
            let last = mask_comp_count(&state.params.masks[0]) - 1;
            assert!(state.remove_mask_component(0, last));
        }
        assert!(!state.remove_mask_component(0, 0), "最后一个形状删不掉");
        assert!(
            !state.remove_mask_component(0, 9),
            "越界的形状序号不能误删别的形状"
        );
        assert_eq!(mask_comp_count(&state.params.masks[0]), 1);
    }

    /// 回归锚：用户实测「移动蒙版会移动图片」。抓在主体上必须整体移动形状、
    /// 两个端点同步平移，且不动 feather / angle。
    ///
    /// Regression anchor: the user measured "moving the mask moves the image". A body
    /// grab must translate the whole shape -- both endpoints by the same delta -- and
    /// leave feather / angle alone.

    #[test]
    fn body_drag_translates_both_endpoints() {
        let mut state = AppState::new();
        state.add_mask("linear");
        let img = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 500.0));

        // 按下锚点：指针 (400,200)，快照 = 当前几何。
        let geo0 = {
            let raw = &state.params.masks[state.mask_sel.unwrap()];
            let comp = mask_comp_object(raw, state.mask_comp).expect("组件在");
            let g = mask_geo_of(comp);
            [g.x1, g.y1, g.x2, g.y2]
        };
        let from = Pos2::new(400.0, 200.0);
        state.mask_drag = Some((0, HANDLE_MOVE));
        state.mask_move_anchor = Some((from, geo0));

        // 拖到 (500, 250)：归一化位移 = (0.1, 0.1)。
        mask_drag_handle(&mut state, img, Pos2::new(500.0, 250.0));

        let geo1 = {
            let raw = &state.params.masks[state.mask_sel.unwrap()];
            let comp = mask_comp_object(raw, state.mask_comp).expect("组件在");
            mask_geo_of(comp)
        };
        let d = (0.1, 0.1);
        // Pos2 是 f32：归一化位移带 1e-7 级噪声，容差放到 1e-6。
        assert!(
            (geo1.x1 - geo0[0] - d.0).abs() < 1e-6,
            "端点 A.x 没跟上位移"
        );
        assert!(
            (geo1.y1 - geo0[1] - d.1).abs() < 1e-6,
            "端点 A.y 没跟上位移"
        );
        assert!(
            (geo1.x2 - geo0[2] - d.0).abs() < 1e-6,
            "端点 B.x 没同步平移"
        );
        assert!(
            (geo1.y2 - geo0[3] - d.1).abs() < 1e-6,
            "端点 B.y 没同步平移"
        );
        // feather 是 new_mask 的初值：整体移动不许动它（先读初值再比对）。
        let feather0 = {
            let raw = &state.params.masks[0];
            let comp = mask_comp_object(raw, state.mask_comp).expect("组件在");
            mask_geo_of(comp).feather
        };
        assert!(
            (geo1.feather - feather0).abs() < 1e-6,
            "整体移动动了 feather"
        );
    }

    /// 主体命中：radial 圆内 Some、圆外 None；linear 轴带 24px 内 Some、远处 None；
    /// 画笔永远 None（画笔的按下是起笔，不是移动）。
    ///
    /// Body hit: radial inside Some / outside None; linear within the 24 px band Some /
    /// far None; brush always None (a press on a brush starts a stroke, not a move).
    #[test]
    fn body_hit_follows_the_shape() {
        let mut state = AppState::new();
        state.add_mask("radial");
        let img = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 500.0));
        // 默认 radial 在画幅中央、半径约短边 1/4：往中心放指针。
        let center = {
            let raw = &state.params.masks[0];
            let comp = mask_comp_object(raw, 0).expect("组件在");
            let g = mask_geo_of(comp);
            mask_screen(img, g.x1, g.y1)
        };
        assert!(
            mask_body_geo(&state, img, center).is_some(),
            "圆心必须在主体内"
        );
        let far = Pos2::new(10.0, 10.0);
        let inside = mask_body_geo(&state, img, far).is_some();
        // 圆可能大到盖住 (10,10)：用「命中则快照形状正确」兜底，不硬编码内外。
        if let Some(snap) = mask_body_geo(&state, img, far) {
            assert_eq!(snap.len(), 4);
        }
        let _ = inside;

        // linear：轴带内 Some、垂直远离 24px 外 None。
        state.add_mask("linear");
        state.select_mask(1);
        let ends = {
            let raw = &state.params.masks[1];
            let comp = mask_comp_object(raw, 0).expect("组件在");
            let g = mask_geo_of(comp);
            (mask_screen(img, g.x1, g.y1), mask_screen(img, g.x2, g.y2))
        };
        let mid = ends.0.lerp(ends.1, 0.5);
        assert!(
            mask_body_geo(&state, img, mid).is_some(),
            "轴线中点必须在带内"
        );
        let off = Pos2::new(mid.x, mid.y + 80.0);
        // 80px 垂直距离超出 24px 带（除非轴线本身接近竖直——默认是水平的）。
        if (ends.1.y - ends.0.y).abs() < 40.0 {
            assert!(mask_body_geo(&state, img, off).is_none(), "带外不该命中");
        }

        // 画笔：没有主体。
        state.add_mask("brush");
        state.select_mask(2);
        assert!(mask_body_geo(&state, img, center).is_none(), "画笔没有主体");
    }
}
