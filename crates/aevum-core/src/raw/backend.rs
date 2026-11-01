//! 可替换的 RAW 解码后端：[`RawBackend`] trait + 内置后端 + 路由。
//!
//! M2 之前，`raw::read` 只有一条内置双路径（CFA 直解 + 内嵌预览兜底）。这一层把它
//! 接口化：导出全分辨率走 [`full`] 路由，画布快路径走 [`preview`] 路由，后端本身
//! 可替换——接 rawler 补齐 CR3-CRX 与 12/14 位打包的传感器直解，libraw（或别的）
//! 将来也在这留座。
//!
//! 路由口径（两条都如实报告走了哪个后端与哪条路，见 [`RawDecode`]）：
//! - **full**：优先第三方后端（传感器直解），它解不动再退内置双路径——内置的
//!   错误串对 TIFF 结构更具体，退到它报错才有诊断价值。
//! - **preview**：一律先扫内嵌 JPEG（机身预览常常就是全尺寸，比任何解码都快一个
//!   量级），没有预览才退 [`full`]。
//!
//! 位深口径不变：两条路都返回 `Rgb16Image`，保真度只由 [`RawPath`] 表达。
//!
//! Pluggable RAW decode backends: the [`RawBackend`] trait, built-in
//! backends, and routing.
//!
//! Before M2, `raw::read` had only one built-in dual path (direct CFA decode
//! with the embedded preview as fallback). This layer turns that into an
//! interface: full-resolution export goes through the [`full`] route, the
//! canvas fast path goes through the [`preview`] route, and the backend
//! itself is replaceable. rawler plugs in to cover CR3-CRX and direct sensor
//! decode of 12/14-bit packed data; libraw (or something else) may take a
//! seat here later.
//!
//! Routing convention (both routes faithfully report which backend and which
//! path were used, see [`RawDecode`]):
//! - **full**: prefers the third-party backend (direct sensor decode) and
//!   falls back to the built-in dual path when it cannot decode. The
//!   built-in error strings are more specific about TIFF structure, so only
//!   an error from the fallback carries diagnostic value.
//! - **preview**: always scans the embedded JPEG first (in-camera previews
//!   are often full size and an order of magnitude faster than any decode);
//!   only when no preview exists does it fall back to [`full`].
//!
//! The bit-depth convention is unchanged: both routes return `Rgb16Image`;
//! fidelity is expressed only by [`RawPath`].

use std::path::Path;
use std::sync::Mutex;

use super::{RawError, RawPath, Rgb16Image};

/// 最近一次「第三方后端失败、退回内置」的原因。库代码不往 stderr 喷字——
/// 观测权交给调用方：诊断面板按需读 [`last_fallback_note`]，没人读就安静待着。
static FALLBACK_NOTE: Mutex<Option<String>> = Mutex::new(None);

/// 最近一次 RAW 全分辨率解码里 rawler 降级内置的原因（`路径: 错误`）。
/// 之后 rawler 成功解码一次就清空；从未降级过返回 `None`。
///
/// Reason for the most recent fallback from rawler to the built-in backend
/// during a full-resolution RAW decode (`path: error`). Cleared after the
/// next successful rawler decode; `None` if rawler never fell back.
pub fn last_fallback_note() -> Option<String> {
    FALLBACK_NOTE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// 记下一次降级。锁被毒化说明有线程 panic 过，恢复后照写——诊断信息丢不得。
fn note_fallback(path: &Path, e: &RawError) {
    *FALLBACK_NOTE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(format!("{path:?}: {e}"));
}

/// rawler 成功时清掉旧降级记录，避免上一次的残影贴在新文件上。
fn clear_fallback_note() {
    *FALLBACK_NOTE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// 一次解码的完整报告：像素 + 保真度 + 后端名。
///
/// Full report of one decode: pixels + fidelity + backend name.
#[derive(Clone, Debug)]
pub struct RawDecode {
    pub image: Rgb16Image,
    /// 传感器直解还是内嵌预览。UI 要标注保真度读它，别猜错误串。
    ///
    /// Direct sensor decode or embedded preview. The UI reads this to label
    /// fidelity instead of guessing from error strings.
    pub path: RawPath,
    /// 出这份像素的后端（`"rawler"`、`"builtin"`…）。
    ///
    /// Backend that produced these pixels (`"rawler"`, `"builtin"`, ...).
    pub backend: &'static str,
    /// EXIF 方向码（1..8，1=不转）。后端自己最清楚：CR3 是 BMFF 容器，
    /// `exif::read` 读不到它的方向，rawler 能。调用方拿它去转正，别再自己读。
    ///
    /// EXIF orientation code (1..8, 1 = no rotation). Each backend knows
    /// best: CR3 is a BMFF container whose orientation `exif::read` cannot
    /// see but rawler can. Callers use this value to upright the image
    /// instead of reading EXIF again.
    pub orientation: u8,
}

/// 一个 RAW 解码后端。实现必须线程安全（多帧并行测光会同时调进来）。
///
/// A RAW decode backend. Implementations must be thread safe (parallel
/// metering of multiple frames calls in concurrently).
pub trait RawBackend: Sync {
    fn name(&self) -> &'static str;
    /// 全分辨率解码：导出、测光、画布主图走这条。
    ///
    /// Full-resolution decode: used by export, metering, and the canvas main
    /// image.
    fn full(&self, path: &Path) -> Result<RawDecode, RawError>;
}

/// 内置后端：包装 v0.3.0 就有的双路径（`read_bytes`），行为逐位不变。
///
/// The built-in backend: wraps the dual path that existed since v0.3.0
/// (`read_bytes`), with bit-for-bit identical behavior.
pub struct BuiltinBackend;

/// 内置后端在 `RawDecode.backend` 里的名字。
///
/// Name the built-in backend reports in `RawDecode.backend`.
pub const BUILTIN_NAME: &str = "builtin";

impl RawBackend for BuiltinBackend {
    fn name(&self) -> &'static str {
        BUILTIN_NAME
    }

    fn full(&self, path: &Path) -> Result<RawDecode, RawError> {
        let data = super::load(path, super::MAX_FILE_BYTES)?;
        let (image, raw_path) = super::read_bytes(&data)?;
        // TIFF 系容器的方向 exif::read 一直读得到；BMFF（CR3）读不到时这里
        // 如实给 1，由 rawler 后端补齐——它解得动的文件轮不到这个默认值。
        Ok(RawDecode {
            image,
            path: raw_path,
            backend: self.name(),
            orientation: crate::exif::read(path).orientation,
        })
    }
}

/// 导出全分辨率的路由入口。`raw::read` 对外交付的就是它。
///
/// Routing entry for full-resolution export. This is what `raw::read` hands
/// out externally.
pub fn full(path: &Path) -> Result<RawDecode, RawError> {
    #[cfg(feature = "raw-rawler")]
    {
        // 第三方后端在前：CRX、12/14 位打包这些内置拒绝的布局只有它解得动。
        // 它失败不直接报错——退内置，让 TIFF 结构问题拿到内置的具体错误串；
        // 但失败原因要留下，否则「格式不支持」和「数据坏了」无从分辨。
        match super::rawler_backend::decode_full(path) {
            Ok(decoded) => {
                clear_fallback_note();
                return Ok(decoded);
            }
            // 退内置不直接报错——让 TIFF 结构问题拿到内置的具体错误串；
            // 但失败原因要留下，否则「格式不支持」和「数据坏了」无从分辨。
            Err(e) => note_fallback(path, &e),
        }
    }
    BuiltinBackend.full(path)
}

/// 画布快路径：优先内嵌 JPEG 预览，没有预览才整图解码。
///
/// 机身预览往往就是全尺寸，解它比任何传感器直解都便宜；24MP RAW 完整解码要
/// 300ms 上下，抽内嵌预览是其十分之一。返回 [`RawPath::Preview`] 时调用方要如实
/// 标注保真度。
///
/// Canvas fast path: prefers the embedded JPEG preview and decodes the whole
/// image only when no preview exists.
///
/// In-camera previews are often full size, and decoding one is cheaper than
/// any direct sensor decode; a full 24MP RAW decode takes around 300ms while
/// extracting the embedded preview takes about a tenth of that. When
/// [`RawPath::Preview`] is returned, callers must label fidelity honestly.
pub fn preview(path: &Path) -> Result<RawDecode, RawError> {
    let data = super::load(path, super::MAX_FILE_BYTES)?;
    if let Some(found) = super::preview::find(&data) {
        return Ok(RawDecode {
            image: super::preview::decode(&found)?,
            path: RawPath::Preview,
            backend: BUILTIN_NAME,
            orientation: crate::exif::read(path).orientation,
        });
    }
    // 预览没找到：数据已读完，别再读一遍文件。
    decode_loaded_full(path)
}

/// `preview` 退到整图解码的内部捷径：字节已在手里时免掉第二次 `load`。
#[allow(unused_variables)]
fn decode_loaded_full(path: &Path) -> Result<RawDecode, RawError> {
    #[cfg(feature = "raw-rawler")]
    {
        match super::rawler_backend::decode_full(path) {
            Ok(decoded) => {
                clear_fallback_note();
                return Ok(decoded);
            }
            // 退内置不直接报错——让 TIFF 结构问题拿到内置的具体错误串；
            // 但失败原因要留下，否则「格式不支持」和「数据坏了」无从分辨。
            Err(e) => note_fallback(path, &e),
        }
    }
    BuiltinBackend.full(path)
}

/// 当前编译进来的主后端名，给「信息」面板与诊断日志用。
///
/// Name of the primary backend compiled in, for the info panel and
/// diagnostic logs.
pub fn active_backend_name() -> &'static str {
    #[cfg(feature = "raw-rawler")]
    {
        super::rawler_backend::NAME
    }
    #[cfg(not(feature = "raw-rawler"))]
    {
        BUILTIN_NAME
    }
}
