//! 随包分发的字体字节。
//!
//! 界面（`aevum-ui::theme`）和水印栅格化（`watermark`）读的是同一份常量：
//! Noto Sans SC 500 一份就 10 MB，两个 crate 各 `include_bytes!` 一次，
//! 产物就要多背 10 MB，所以字节只在这里落一次，别人再借。
//!
//! Font bytes shipped with the application.
//!
//! The UI (`aevum-ui::theme`) and watermark rasterization (`watermark`) read the same constants:
//! one copy of Noto Sans SC 500 alone weighs 10 MB, and if each crate ran its own
//! `include_bytes!`, the binary would carry another 10 MB, so the bytes live here once
//! and everyone else borrows them.

/// Inter 可变字体：拉丁字母与数字，界面正文与水印默认那行字都靠它。
///
/// Inter variable font: Latin letters and digits. UI body text and the default
/// watermark line both rely on it.
pub const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");

/// Noto Sans SC 500 静态字重。可变字体的默认字重是 100（Thin），
/// 直接嵌进来中文细得像虚线，所以用 fontTools 实例化出的 500。
///
/// Noto Sans SC 500 static weight. The variable font's default weight is 100 (Thin),
/// and embedding it directly renders Chinese strokes hairline-thin, so the 500
/// instance produced with fontTools is used instead.
pub const NOTO_SC: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSC-500.ttf");

/// 粗体子集：分区标题与次级小标题专用，由 `tools/font_subset.py` 从可变字体
/// 切出（wght=700 + 只留界面源码里出现过的字）。
///
/// 为什么是子集而不是完整字重：完整的 Noto Sans SC 一份 10.6 MB，界面上真正
/// 需要粗体的只有十几处标题；1281 个 CJK 字的 700 子集是 442 KB。缺字不会变
/// 豆腐块——`theme::set_fonts` 把常规体排在粗体后面当兜底，缺的字回落成常规。
///
/// Bold subsets: for section headings and sub-headings only, cut from the variable
/// font by `tools/font_subset.py` (wght=700, keeping only characters that appear in
/// the UI source).
///
/// Why a subset rather than a full weight: a full Noto Sans SC is 10.6 MB and only a
/// dozen headings need bold; the 700 subset of 1281 CJK characters is 442 KB. A
/// missing glyph is not tofu -- `theme::set_fonts` lists the regular family behind
/// the bold one, so missing characters fall back to regular.
pub const INTER_BOLD: &[u8] = include_bytes!("../../../assets/fonts/Inter-700-Subset.ttf");

/// 见 [`INTER_BOLD`]。
///
/// See [`INTER_BOLD`].
pub const NOTO_SC_BOLD: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSC-700-Subset.ttf");
