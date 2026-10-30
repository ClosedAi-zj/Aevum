//! 内嵌预览：在容器里找机身自己写好的那张完整 JPEG，交给 `image` 的解码器。
//!
//! 这是「通用兜底」的另一半。认领一个候选的条件不是「开头像 JPEG」，而是「段结构能从
//! SOI 一路走到 EOI」：每个标记、每段长度都要落在文件里，走不通就换下一个候选。这样
//! 恶意构造的 `FF D8` 字节串最多让我们白走几步（有全局步数预算），拿不到分配大内存的
//! 机会——真正的解码分配发生在 `image` 那边，而且我们先把声明尺寸钉在 `MAX_PIXELS` 内、
//! 再给它显式的 `Limits`。
//!
//! 只取**像素最多**的那张完整预览。找不到最大的那张（例如它超过尺寸上限）时直接报错，
//! 不退去用更小的一缩略图：把 160×120 的缩略图当成全尺寸预览喂进序列，导出时会得到
//! 一张糊图，而界面上一切正常。

use std::io::Cursor;

use image::{ColorType, DynamicImage, ImageFormat, ImageReader, Limits};

use super::{MAX_PIXELS, RawError};
use crate::dng::Rgb16Image;

/// 一次找预览能走的总步数（段头与熵数据扫描共用一份预算）。预算耗尽只会得到「没找到」，
/// 不会得到半个像素：最坏情况就是线性扫一遍文件的一部分。
const MAX_STEPS: usize = 8_000_000;
/// 最多检查这么多 SOI 候选。真实文件里预览 + 缩略图也就几张。
const MAX_CANDIDATES: usize = 64;
/// JPEG 段数上限；预览图的段数是几十量级，上限挡的是长度自指的坏流。
const MAX_SEGMENTS: usize = 8192;

pub(crate) struct Found<'a> {
    pub(crate) span: &'a [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// 找到 `d` 里像素最多的完整 JPEG 跨度。
pub(crate) fn find(d: &[u8]) -> Option<Found<'_>> {
    let mut best: Option<Found<'_>> = None;
    let mut best_px = 0u64;
    let mut at = 0usize;
    let mut steps = MAX_STEPS;
    let mut checked = 0usize;
    while checked < MAX_CANDIDATES {
        let Some(hit) = next_soi(d, at) else {
            break;
        };
        at = hit + 1;
        checked += 1;
        let Some(end) = span_end(d, hit, &mut steps) else {
            continue;
        };
        at = end;
        let span = d.get(hit..end)?;
        let (w, h) = declared_size(span)?;
        let px = u64::from(w).checked_mul(u64::from(h))?;
        if px > best_px {
            best_px = px;
            best = Some(Found {
                span,
                width: w,
                height: h,
            });
        }
        if steps == 0 {
            break;
        }
    }
    best
}

/// 从 `from` 起找下一个 `FF D8 FF`。要求第三个字节也是 `FF`（SOI 之后必须紧跟标记头），
/// 一下就能滤掉随机数据里的假信号。
fn next_soi(d: &[u8], from: usize) -> Option<usize> {
    let tail = d.get(from..)?;
    let mut i = 0usize;
    while i + 3 <= tail.len() {
        if tail[i..].starts_with(&[0xFFu8, 0xD8, 0xFF]) {
            return Some(from + i);
        }
        i += 1;
    }
    None
}

/// 从 `at` 处的 SOI 走一遍 JPEG 的段结构，返回 EOI 之后的下标。
///
/// 只前进不回头，每一步都靠 `get` 收口：越界、保留标记、预算耗尽都是 `None`。
fn span_end(d: &[u8], at: usize, steps: &mut usize) -> Option<usize> {
    if !d.get(at..at + 2)?.starts_with(&[0xFFu8, 0xD8]) {
        return None;
    }
    let mut pos = at + 2;
    for _ in 0..MAX_SEGMENTS {
        *steps = steps.checked_sub(1)?;
        if d.get(pos)? != &0xFF {
            return None;
        }
        let marker = *d.get(pos + 1)?;
        pos += 2;
        match marker {
            // EOI：跨度到此为止。
            0xD9 => return Some(pos),
            // TEM 与 RST 没有长度字段。
            0x01 | 0xD0..=0xD7 => {}
            // SOS 之后先有段头，再是不带结构的熵数据。
            0xDA => {
                pos = segment_past(d, pos)?;
                pos = entropy_past(d, pos, steps)?;
            }
            _ => pos = segment_past(d, pos)?,
        }
    }
    None
}

/// 跳过带长度的段：长度字段含自身两字节，至少 2。
fn segment_past(d: &[u8], at: usize) -> Option<usize> {
    let len = usize::from(be16(d, at)?);
    if len < 2 {
        return None;
    }
    let next = at.checked_add(len)?;
    // 段尾必须在文件里；否则下一跳的 `get` 也会拒，但这里先拒掉能少绕几步。
    if next > d.len() {
        return None;
    }
    Some(next)
}

/// 熵数据里没有结构，只能找下一个真正的标记：跳过填充 `00`、重复 `FF` 与段内 RST。
fn entropy_past(d: &[u8], at: usize, steps: &mut usize) -> Option<usize> {
    let mut pos = at;
    loop {
        *steps = steps.checked_sub(1)?;
        let b = *d.get(pos)?;
        if b != 0xFF {
            pos += 1;
            continue;
        }
        let marker = *d.get(pos + 1)?;
        if marker == 0x00 || marker == 0xFF {
            pos += 2;
            continue;
        }
        if (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }
        return Some(pos);
    }
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

/// 在长边不少于 `min_side` 的候选里挑像素最少的那张。
///
/// 缩略图的正确方向：够用就行，解码越便宜越好。机身常同时写 1920 边与全尺寸两张，
/// 128 边的胶片条要的是前者——解它十几毫秒，解全尺寸那张要一百多毫秒。
pub(crate) fn find_at_least(d: &[u8], min_side: u32) -> Option<Found<'_>> {
    let mut best: Option<Found<'_>> = None;
    let mut best_px = u64::MAX;
    let mut at = 0usize;
    let mut steps = MAX_STEPS;
    let mut checked = 0usize;
    while checked < MAX_CANDIDATES {
        let Some(hit) = next_soi(d, at) else {
            break;
        };
        at = hit + 1;
        checked += 1;
        let Some(end) = span_end(d, hit, &mut steps) else {
            continue;
        };
        at = end;
        let span = d.get(hit..end)?;
        let (w, h) = declared_size(span)?;
        let px = u64::from(w).checked_mul(u64::from(h))?;
        if w.max(h) >= min_side && px < best_px {
            best_px = px;
            best = Some(Found {
                span,
                width: w,
                height: h,
            });
        }
        if steps == 0 {
            break;
        }
    }
    best
}

/// 用 `image` 自己报的尺寸挑预览：不重复实现一遍 SOF 解析，也不信跨度字节数。
fn declared_size(span: &[u8]) -> Option<(u32, u32)> {
    let reader = ImageReader::with_format(Cursor::new(span), ImageFormat::Jpeg);
    reader.into_dimensions().ok()
}

/// 解一张预览成 16 位 RGB。
pub(crate) fn decode(found: &Found<'_>) -> Result<Rgb16Image, RawError> {
    let px = u64::from(found.width).checked_mul(u64::from(found.height));
    match px {
        Some(px) if px <= MAX_PIXELS && found.width > 0 && found.height > 0 => {}
        _ => {
            return Err(RawError::PreviewTooLarge {
                width: found.width,
                height: found.height,
            });
        }
    }
    let mut reader = ImageReader::with_format(Cursor::new(found.span), ImageFormat::Jpeg);
    // 显式上限：声明尺寸已经查过，这里再给解码器一道闸，防它在熵数据里按另一个
    // 更大的 SOF 分配。`Limits` 的字段是公开的，只能建好再改。
    let mut limits = Limits::default();
    limits.max_image_width = Some(found.width);
    limits.max_image_height = Some(found.height);
    reader.limits(limits);
    let img = reader.decode()?;
    Ok(to_rgb16(&img))
}

/// 预览的位深语义只有一条：跟传感器路径一样返回 16 位。
///
/// 8 位预览顶到 16 位是**位宽表示**（`v * 257`，两端满量程对齐），不是新增精度——
/// 下游 `metrics::decode_rgb_f32` 用颜色类型判断「是不是高位深源」，两条路返回同一个
/// 类型才不会各说各话。保真度由 `RawPath::Preview` 说明，不由位深冒充。
fn to_rgb16(img: &DynamicImage) -> Rgb16Image {
    let deep = matches!(
        img.color(),
        ColorType::L16 | ColorType::La16 | ColorType::Rgb16 | ColorType::Rgba16
    );
    if deep {
        return img.to_rgb16();
    }
    let rgb8 = img.to_rgb8();
    let mut out = Rgb16Image::new(rgb8.width(), rgb8.height());
    for (src, dst) in rgb8.pixels().zip(out.pixels_mut()) {
        dst.0 = [
            u16::from(src.0[0]) * 257,
            u16::from(src.0[1]) * 257,
            u16::from(src.0[2]) * 257,
        ];
    }
    out
}
