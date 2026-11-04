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

/// 从 rawler 的 `RawImage` 得到 EXIF 方向码（1–8），并在 rawler 缺席时回落 `exif::read`。
///
/// rawler 0.8.0 把 `RawImage.orientation` **硬编码成 Normal**（rawimage.rs 两处
/// `orientation: Orientation::Normal, //cam.orientation, // TODO fixme`），所以这个
/// match 对任何文件都吐 1——竖拍（EXIF Orientation 5–8）在这里躺平：画布横躺、
/// 草图同躺，只有走内嵌 JPEG 的缩略图是正的（那边用 `exif::read`）。实测
/// PANA0001.RW2（Orientation=8）：后端报 1、像素 6000×4000 未转正。
///
/// 所以 **1 不可信，要回落**：`crate::exif::read` 读标准 Orientation 标签，RW2/NEF/
/// ARW/DNG 的 TIFF 头里都有。后端哪天真报了非 1（比如 CR3 的 BMFF 容器，exif::read
/// 进不去、只能靠容器解出方向），**以后端为准**——回落只发生在后端说「没转」的时候，
/// 两条信息源不打架。
///
/// EXIF orientation code (1-8) from rawler's `RawImage`, falling back to `exif::read` when
/// rawler has nothing. rawler 0.8.0 **hardcodes** `RawImage.orientation` to Normal (two
/// `// TODO fixme` sites in rawimage.rs), so the match below yields 1 for every file and
/// portrait shots (EXIF Orientation 5-8) lie flat here: canvas sideways, sketch sideways,
/// while the embedded-JPEG thumbnail stands upright (that path uses `exif::read`). Measured
/// on PANA0001.RW2 (Orientation=8): backend reports 1, pixels 6000x4000 unrotated.
///
/// So **1 is not trustworthy and triggers the fallback**: `crate::exif::read` reads the
/// standard Orientation tag from the TIFF header, which RW2/NEF/ARW/DNG all carry. If the
/// backend ever reports a real non-1 value (e.g. CR3's BMFF container, which `exif::read`
/// cannot enter and only the container parse can orient), **the backend wins** -- the
/// fallback fires only when the backend says "no rotation", so the two sources never fight.
fn resolve_orientation(img: &RawImage, path: &Path) -> u8 {
    let from_backend = match img.orientation {
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
    if from_backend == 1 {
        crate::exif::read(path).orientation
    } else {
        from_backend
    }
}

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
    // rawler 吃的是不可信字节，它内部有 `bits.rs` 那种按下标直取的读法——0.7.2 在
    // iPhone 的 LinearRaw DNG 上就越界 panic 过一次（见 docs/RAW兼容性-根因与防线.md）。
    // 0.8.0 自己也兜了一层，但我们不把「别人兜了」当成前提：守卫在这里，换实现、
    // 换版本、换后端都还是这一道。
    //
    // rawler eats untrusted bytes and reads by index inside (the `bits.rs` style). 0.7.2
    // once panicked out of bounds on an iPhone LinearRaw DNG (see the RAW compatibility
    // doc). 0.8.0 guards it internally too, but "someone else guards it" is not a premise:
    // the guard lives here, so swapping the implementation, the version or the backend keeps
    // the same boundary.
    let img = super::guard::decode_guard(NAME, || {
        rawler::decode_file(path).map_err(|e| RawError::Rawler(e.to_string()))
    })?;
    // 方向的真相源与回落见 `resolve_orientation` 的文档：rawler 0.8.0 硬编码 Normal，
    // 后端报 1 时回落 exif::read，两条信息源不打架。
    let orientation = resolve_orientation(&img, path);
    let (image, _full) = to_rgb16(img, false)?;
    Ok(RawDecode {
        image,
        path: RawPath::Sensor,
        backend: NAME,
        orientation,
    })
}

/// 半分辨率解码：同一趟 rawler 解析，去拜尔只算 2×2 单元估计，不展开回全分辨率。
///
/// 首屏草图只要 800 边，而 [`decode_full`] 会先把 24MP 的单元估计展开成 24MP RGB 再
/// 缩——展开写出去的 144MB，缩小时原样读回来。这里直接交半尺寸（传感器边长的一半），
/// 交出去的像素仍是同一套 2×2 均值与同一套白平衡增益，只是不再展开。
///
/// 返回（半尺寸像素、全分辨率边长（未转正））。三通道线性数据（rawler 报 `cpp == 3`）
/// 没有 CFA 可估，直接报错交给调用方退回整幅解码——那条路本来就不是 24MP 的常态。
///
/// Half-resolution decode: the same rawler parse, with demosaic stopping at the 2x2 cell
/// estimates instead of expanding them back to full resolution. Returns (half-size pixels,
/// full-resolution side lengths before orientation). Three-channel linear data (`cpp == 3`)
/// has no CFA to estimate and reports an error so the caller falls back to the full decode.
pub fn decode_half(path: &Path) -> Result<(Rgb16Image, (u32, u32), u8), RawError> {
    let img = super::guard::decode_guard(NAME, || {
        rawler::decode_file(path).map_err(|e| RawError::Rawler(e.to_string()))
    })?;
    // 方向码与 `decode_full` 同一出处（`resolve_orientation`）：草图与全量要按同一个
    // 方向摆。
    // The orientation code shares `decode_full`'s source (`resolve_orientation`): the sketch
    // and the full preview must be upright the same way.
    let orientation = resolve_orientation(&img, path);
    let (image, full) = to_rgb16(img, true)?;
    Ok((image, full, orientation))
}

/// 一张 RAW 的几何与电平：解码与「只看元数据」共用的唯一来源。
///
/// Geometry and levels of one RAW: the single source shared by decoding and "metadata only".
struct Geometry {
    ox: usize,
    oy: usize,
    w: usize,
    h: usize,
    black: f32,
    white: f32,
}

/// 算几何与电平，并在这里做「这张图解不解得动」的判定（尺寸越界、白场不高于黑场）。
///
/// 判定放在几何里是刻意的：`probe` 说「能解」而 `decode` 说「电平坏了」，就是两条路
/// 又分叉了；判定只有一份，结论就不会打架。
///
/// Geometry, levels, and the "is this decodable at all" verdict (size overflow, white level
/// not above black). Keeping the verdict here is deliberate: if `probe` says "decodable" while
/// `decode` rejects bad levels, the two paths have forked again.
fn geometry(img: &RawImage) -> Result<Geometry, RawError> {
    let (fw, fh) = (img.width, img.height);
    if fw == 0
        || fh == 0
        || u64::from(fw as u32).saturating_mul(u64::from(fh as u32)) > super::MAX_PIXELS
    {
        return Err(RawError::TooLarge {
            width: fw as u32,
            height: fh as u32,
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
    Ok(Geometry {
        ox,
        oy,
        w,
        h,
        black,
        white,
    })
}

/// 不解像素地问 rawler「这份文件它认不认，认的话是什么」。
///
/// `dummy = true` 让 rawler 跳过像素解码（只解析容器与标签），成本是整解的零头；而
/// 尺寸与「能不能解」的判定走同一个 [`geometry`]，所以它给出的答案与整解**不可能**
/// 不一致。这就是 `raw::info` 与 `raw::read` 从此同源的地方。
///
/// 与整解一样套 `decode_guard`：rawler 读的是不可信字节，`bits.rs` 那种按下标直取
/// 的写法在 0.7.2 上越界 panic 过一次。只看元数据也要经过同一道边界。
///
/// Ask rawler without decoding pixels: does it recognize this file, and as what?
///
/// `dummy = true` skips pixel decoding (container and tags only) at a fraction of the cost,
/// and the size plus the "decodable" verdict go through the same [`geometry`], so this answer
/// **cannot** disagree with the full decode. That is where `raw::info` and `raw::read` stop
/// diverging. The guard applies here too: rawler reads untrusted bytes and panicked out of
/// bounds in its `bits.rs` index-based reads back in 0.7.2.
pub fn probe(path: &Path) -> Result<super::RawProbe, RawError> {
    let img = super::guard::decode_guard(NAME, || {
        let src =
            rawler::rawsource::RawSource::new(path).map_err(|e| RawError::Rawler(e.to_string()))?;
        let decoder = rawler::get_decoder(&src).map_err(|e| RawError::Rawler(e.to_string()))?;
        decoder
            .raw_image(&src, &rawler::decoders::RawDecodeParams::default(), true)
            .map_err(|e| RawError::Rawler(e.to_string()))
    })?;
    let g = geometry(&img)?;
    // 与整解同一口径：单通道（CFA）与三通道（LinearRaw）解得动，别的一律「样本数不支持」。
    if img.cpp != 1 && img.cpp != 3 {
        return Err(RawError::UnsupportedSamples(img.cpp as u16));
    }
    Ok(super::RawProbe {
        width: g.w as u32,
        height: g.h as u32,
        // 位深如实报（12/14/16 都是真的）；`RawInfo.bits` 的读者只用来判断「是不是高位深」。
        bits: img.bps.clamp(1, 16) as u16,
        make: non_empty(&img.make),
        model: non_empty(&img.model),
    })
}

/// 空串当「没有」：rawler 常给 `""` 而不是 `None`，直接塞进面板会显示一个空机型。
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_owned())
    }
}

/// rawler 的 `RawImage` → 我们的 `Rgb16Image`：裁有效区、扣电平、去拜尔、白平衡。
///
/// `half` 为真时只算 2×2 单元估计（半尺寸），见 [`decode_half`]；否则走全分辨率。
/// 第二个返回值是全分辨率的（未转正）边长，`half` 为假时就是图像自己的边长。
///
/// rawler's `RawImage` -> our `Rgb16Image`: crop to the active area, subtract levels,
/// demosaic, white balance. With `half` the demosaic stops at the 2x2 estimates (see
/// [`decode_half`]). The second return value is the full-resolution (unrotated) side
/// lengths; without `half` that is simply the image's own size.
fn to_rgb16(img: RawImage, half: bool) -> Result<(Rgb16Image, (u32, u32)), RawError> {
    // 几何与电平由 [`geometry`] 一处算。这不是为了少几行：`probe` 不解像素也要给出
    // 同一个答案（尺寸、能不能解），两处各写一遍就会慢慢漂开——实测漂开过：信息面板
    // 报 6960×4640 而解码出 6982×4654，用户看到的尺寸与拿到的像素不是一个东西。
    //
    // Geometry and levels come from [`geometry`] alone. Not to save lines: `probe` must give
    // the same answer without decoding pixels, and two copies drift apart -- which happened:
    // the info panel said 6960x4640 while the decode produced 6982x4654.
    let g = geometry(&img)?;
    let (ox, oy, w, h) = (g.ox, g.oy, g.w, g.h);
    let black = g.black;
    let white = g.white;
    let mut out_w = w;
    let mut out_h = h;
    let mut rgb = match img.cpp {
        // 单通道：马赛克 → 扣电平 → 去拜尔（内置同一套粗估器）。
        // active_area 裁剪偏移为奇数时 2×2 图案错相位——新左上角是原图
        // (ox,oy) 像素，图案必须按偏移的奇偶平移，否则 R/B 互换整图换色。
        1 => {
            let cfa = cfa_pattern(&img)?.map(|p| shift_cfa(p, ox, oy));
            let mut mosaic = crop_samples(&img, ox, oy, w, h, 1, black, white)?;
            cfa::scale_levels(&mut mosaic, black.round() as u32, white.round() as u32);
            if half {
                // 半尺寸：直接交 2×2 单元估计，省掉展开与随后的整幅缩读。
                // Half size: hand out the 2x2 estimates, skipping the expansion and the
                // full-width resample read that follows it.
                let (rgb, hw, hh) = cfa::demosaic_half(&mosaic, w, h, cfa);
                out_w = hw;
                out_h = hh;
                rgb
            } else {
                // 走方法注册表：诊断面板可切换（cell_average 为默认，与内置路径同口径）。
                cfa::demosaic_with(cfa::selected_demosaic(), &mosaic, w, h, cfa)
            }
        }
        // 三通道：线性 RGB，逐元素扣电平即可（scale_levels 对交错缓冲逐元素算术不变）。
        3 if !half => {
            let mut rgb = crop_samples(&img, ox, oy, w, h, 3, black, white)?;
            cfa::scale_levels(&mut rgb, black.round() as u32, white.round() as u32);
            rgb
        }
        // 三通道没有 CFA 可估：半尺寸这条路说不出口，交回调用方退回整幅解码。
        // Three channels have no CFA to estimate; the half route does not apply here.
        3 => return Err(RawError::UnsupportedSamples(3)),
        other => return Err(RawError::UnsupportedSamples(other as u16)),
    };

    // 白平衡增益：按绿通道归一的定点增益，与内置 `apply_gains` 同一套算术。
    if let Some(gains) = wb_gains(img.wb_coeffs) {
        cfa::apply_gains(&mut rgb, gains);
    }

    let image =
        Rgb16Image::from_raw(out_w as u32, out_h as u32, rgb).ok_or(RawError::TooLarge {
            width: out_w as u32,
            height: out_h as u32,
        })?;
    Ok((image, (w as u32, h as u32)))
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
