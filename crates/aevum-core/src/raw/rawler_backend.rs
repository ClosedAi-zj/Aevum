//! rawler 适配器（feature `raw-rawler`，默认开启）。
//!
//! rawler（dnglab 同源、纯 Rust、MIT OR Apache-2.0）补齐内置路径的两块短板：
//! CR3 的 CRX 压缩与 12/14 位行内打包——两者在内置路径里都退预览，这里走传感器直解。
//!
//! 与内置路径的口径对齐：
//! - 去拜尔**复用 `cfa::demosaic`**，与 DNG 路径同一套粗估器，两条路解出来的图不打架；
//! - 白平衡只用文件里的增益系数（`wb_coeffs`），缺了就不调色——不猜；
//! - 电平扣黑场后拉满 16 位，口径同 `cfa::scale_levels`；
//! - 方向**不在这里转**：`metrics::decode` 统一按 EXIF 方向 `oriented`，与其它后端一致。
//!
//! rawler 解不动的文件由 `backend::full` 路由退回内置后端，这里只管把认识的那部分解好。
//!
//! rawler adapter (feature `raw-rawler`, enabled by default).
//!
//! rawler (same lineage as dnglab, pure Rust, MIT OR Apache-2.0) closes two gaps in the
//! built-in path: CR3's CRX compression and 12/14-bit in-row packing. The built-in path falls
//! back to preview for both; this path decodes directly from the sensor.
//!
//! Conventions aligned with the built-in path:
//! - Demosaic **reuses `cfa::demosaic`**, the same coarse estimator as the DNG path, so images
//!   decoded by the two paths do not disagree;
//! - White balance uses only the gain coefficients in the file (`wb_coeffs`); when they are
//!   missing nothing is graded — no guessing;
//! - Levels subtract the black point and scale to the full 16-bit range, the same convention
//!   as `cfa::scale_levels`;
//! - Orientation **is not applied here**: `metrics::decode` applies the EXIF orientation
//!   uniformly via `oriented`, consistent with the other backends.
//!
//! Files rawler cannot decode are routed back to the built-in backend by `backend::full`; this
//! module only decodes the part it recognizes.

use std::path::Path;

use rawler::RawImage;
use rawler::rawimage::RawPhotometricInterpretation;

use super::backend::{RawBackend, RawDecode};
use super::cfa;
use super::{RawError, RawPath, Rgb16Image};

/// 后端名，进 `RawDecode.backend` 与 `active_backend_name()`。
///
/// Backend name, carried in `RawDecode.backend` and `active_backend_name()`.
pub const NAME: &str = "rawler";

/// trait 形态的适配器：留给将来「按后端显式选择」的调用方（如诊断面板强制
/// 指定后端对比解码结果）。当前路由走 [`decode_full`] 直接函数入口。
///
/// Trait-shaped adapter reserved for future callers that select a backend explicitly (for
/// example a diagnostics panel forcing one backend to compare decode results). The current
/// routing goes through the direct [`decode_full`] function entry point.
#[allow(dead_code)]
pub struct RawlerBackend;

impl RawBackend for RawlerBackend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn full(&self, path: &Path) -> Result<RawDecode, RawError> {
        decode_full(path)
    }
}

/// 路由入口：解 rawler 认识的文件，报告传感器直解路径。
///
/// Routing entry point: decodes files rawler recognizes and reports the sensor-direct path.
pub fn decode_full(path: &Path) -> Result<RawDecode, RawError> {
    let img = rawler::decode_file(path).map_err(|e| RawError::Rawler(e.to_string()))?;
    // 方向跟着 rawler 走：CR3 是 BMFF 容器，我们的 exif::read 读不到它的
    // 方向标记，而 rawler 解容器时顺带就有——竖拍躺平就是漏了这一步。
    let orientation = match img.orientation {
        rawler::Orientation::Normal => 1,
        rawler::Orientation::HorizontalFlip => 2,
        rawler::Orientation::Rotate180 => 3,
        rawler::Orientation::VerticalFlip => 4,
        rawler::Orientation::Transpose => 5,
        rawler::Orientation::Rotate90 => 6,
        rawler::Orientation::Transverse => 7,
        rawler::Orientation::Rotate270 => 8,
        rawler::Orientation::Unknown => 1,
    };
    let image = to_rgb16(img)?;
    Ok(RawDecode {
        image,
        path: RawPath::Sensor,
        backend: NAME,
        orientation,
    })
}

/// rawler 的 `RawImage` → 我们的 `Rgb16Image`：裁有效区、扣电平、去拜尔、白平衡。
fn to_rgb16(img: RawImage) -> Result<Rgb16Image, RawError> {
    let (w, h) = (img.width, img.height);
    if w == 0
        || h == 0
        || u64::from(w as u32).saturating_mul(u64::from(h as u32)) > super::MAX_PIXELS
    {
        return Err(RawError::TooLarge {
            width: w as u32,
            height: h as u32,
        });
    }

    // 电平：黑场取各通道均值，白场取最保守（最小）的一路——按最严的那路扣，
    // 高光宁可略压也不削顶。缺标记时 rawler 自己会填位深满量程。
    let blacks = img.blacklevel.as_vec();
    let black = if blacks.is_empty() {
        0.0f32
    } else {
        blacks.iter().sum::<f32>() / blacks.len() as f32
    };
    let white = img
        .whitelevel
        .0
        .iter()
        .copied()
        .min()
        .unwrap_or(u16::MAX as u32) as f32;
    if white <= black {
        return Err(RawError::BadLevels {
            black: black.round() as u32,
            white: white.round() as u32,
        });
    }

    // 有效区：rawler 不裁遮蔽边，`active_area` 说了算。没有就当全幅。
    let (ox, oy, w, h) = match img.active_area {
        Some(r) if r.d.w > 0 && r.d.h > 0 => {
            let ox = r.p.x.clamp(0, img.width.saturating_sub(1));
            let oy = r.p.y.clamp(0, img.height.saturating_sub(1));
            let w = r.d.w.min(img.width - ox);
            let h = r.d.h.min(img.height - oy);
            (ox, oy, w, h)
        }
        _ => (0, 0, img.width, img.height),
    };

    let mut rgb = match img.cpp {
        // 单通道：马赛克 → 扣电平 → 去拜尔（内置同一套粗估器）。
        // active_area 裁剪偏移为奇数时 2×2 图案错相位——新左上角是原图
        // (ox,oy) 像素，图案必须按偏移的奇偶平移，否则 R/B 互换整图换色。
        1 => {
            let cfa = cfa_pattern(&img)?.map(|p| shift_cfa(p, ox, oy));
            let mut mosaic = crop_samples(&img, ox, oy, w, h, 1, black, white)?;
            cfa::scale_levels(&mut mosaic, black.round() as u32, white.round() as u32);
            // 走方法注册表：诊断面板可切换（cell_average 为默认，与内置路径同口径）。
            cfa::demosaic_with(cfa::selected_demosaic(), &mosaic, w, h, cfa)
        }
        // 三通道：线性 RGB，逐元素扣电平即可（scale_levels 对交错缓冲逐元素算术不变）。
        3 => {
            let mut rgb = crop_samples(&img, ox, oy, w, h, 3, black, white)?;
            cfa::scale_levels(&mut rgb, black.round() as u32, white.round() as u32);
            rgb
        }
        other => return Err(RawError::UnsupportedSamples(other as u16)),
    };

    // 白平衡增益：按绿通道归一的定点增益，与内置 `apply_gains` 同一套算术。
    if let Some(gains) = wb_gains(img.wb_coeffs) {
        cfa::apply_gains(&mut rgb, gains);
    }

    Rgb16Image::from_raw(w as u32, h as u32, rgb).ok_or(RawError::TooLarge {
        width: w as u32,
        height: h as u32,
    })
}

/// 2×2 图案按裁剪偏移平移相位：`p` 是相对原图 (0,0) 的布局，
/// 裁剪后新图案的第 (r,c) 格是原图第 (r+dy, c+dx) 格的颜色。
fn shift_cfa(p: [u8; 4], dx: usize, dy: usize) -> [u8; 4] {
    let at = |r: usize, c: usize| p[((r + dy) & 1) * 2 + ((c + dx) & 1)];
    [at(0, 0), at(0, 1), at(1, 0), at(1, 1)]
}

/// 从 rawler 的 CFA 声明里取 2×2 图案。取值编号与我们一致（0=R, 1=G, 2=B）；
/// 非 2×2 或含 C/M/Y 的图案需要色彩矩阵，不猜，交回路由退内置（内置会退预览）。
fn cfa_pattern(img: &RawImage) -> Result<Option<[u8; 4]>, RawError> {
    match &img.photometric {
        RawPhotometricInterpretation::Cfa(cfg) => {
            if cfg.cfa.width != 2 || cfg.cfa.height != 2 {
                return Err(RawError::UnsupportedCfa);
            }
            let mut out = [0u8; 4];
            for (i, (row, col)) in [(0, 0), (0, 1), (1, 0), (1, 1)].iter().enumerate() {
                let color = cfg.cfa.color_at(*row, *col);
                if color > 2 {
                    return Err(RawError::UnsupportedCfa);
                }
                out[i] = color as u8;
            }
            Ok(Some(out))
        }
        // 线性原始数据与普通灰度都没有 CFA；demosaic 对 None 走三通道同值。
        RawPhotometricInterpretation::LinearRaw | RawPhotometricInterpretation::BlackIsZero => {
            Ok(None)
        }
    }
}

/// 抠出有效区采样并规整到 16 位。`Integer` 数据原样拷贝（电平交给
/// `scale_levels`），`Float` 数据按电平映射到 16 位量程。
#[allow(clippy::too_many_arguments)]
fn crop_samples(
    img: &RawImage,
    ox: usize,
    oy: usize,
    w: usize,
    h: usize,
    cpp: usize,
    black: f32,
    white: f32,
) -> Result<Vec<u16>, RawError> {
    let src_w = img.width;
    let row_bytes = src_w * cpp;
    let range = (white - black).max(1.0);
    let map = |v: f32| -> u16 {
        (((v - black) / range) * 65535.0)
            .clamp(0.0, 65535.0)
            .round() as u16
    };
    let mut out = Vec::with_capacity(w * h * cpp);
    match &img.data {
        rawler::RawImageData::Integer(data) => {
            if data.len() < (oy + h) * row_bytes {
                return Err(RawError::Truncated);
            }
            for y in oy..oy + h {
                out.extend_from_slice(
                    &data[y * row_bytes + ox * cpp..y * row_bytes + (ox + w) * cpp],
                );
            }
        }
        rawler::RawImageData::Float(data) => {
            if data.len() < (oy + h) * row_bytes {
                return Err(RawError::Truncated);
            }
            for y in oy..oy + h {
                for v in &data[y * row_bytes + ox * cpp..y * row_bytes + (ox + w) * cpp] {
                    out.push(map(*v));
                }
            }
        }
    }
    Ok(out)
}

/// `wb_coeffs`（RGBE 序的增益）→ 绿通道归一的定点增益（1/65536）。
/// 系数缺失或非正就当没有白平衡信息，宁可原样返回也不拿可疑系数刷全图。
fn wb_gains(coeffs: [f32; 4]) -> Option<[u32; 3]> {
    let green = coeffs[1];
    if !green.is_finite() || green <= 0.0 {
        return None;
    }
    let mut gains = [65536u32; 3];
    for (c, g) in gains.iter_mut().enumerate() {
        let v = coeffs[c];
        if !v.is_finite() || v <= 0.0 {
            return None;
        }
        // 上限 16×：再大的增益只会把高光整片推成纯白，那是失真不是校正。
        let ratio = (v / green).clamp(1.0 / 16.0, 16.0);
        *g = (ratio * 65536.0).round() as u32;
    }
    Some(gains)
}
