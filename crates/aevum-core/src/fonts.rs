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
