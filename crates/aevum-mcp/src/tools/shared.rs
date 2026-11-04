//! 跨域共用的参数解析、输出口径与安全检查。
//!
//! 边界：这里只放**不带业务语义**的东西——从 `Value` 里取参数、把参数翻译成
//! `aevum_core` 的类型、批量写之前的 confirm 校验、路径绝对化、回显文案的截断。
//! 一个函数如果只有一个域在用，就该待在那个域里，不要往这儿搬：这个模块越大，
//! 各域之间的耦合就越像一团（拆 `tools.rs` 本来就是为了把耦合看清楚）。
//!
//! `X264_PRESETS` 是唯一的例外：它只被同在此处的 `parse_codec` 读，两样东西必须
//! 放在一起，否则一处改、另一处忘。
//!
//! Shared argument parsing, output conventions, and safety checks across domains.
//!
//! Boundary: nothing with business semantics belongs here -- reading an argument out of a
//! `Value`, translating it into an `aevum_core` type, the confirm check before a batch write,
//! making a path absolute, clipping text for an error message. A helper used by exactly one
//! domain belongs in that domain: the bigger this module grows, the more the domains look
//! like one knot, and untangling that knot is the whole point of splitting `tools.rs`.
//!
//! `X264_PRESETS` is the one exception: only `parse_codec`, also here, reads it, and the two
//! must stay together or one gets changed while the other is forgotten.

use std::path::{Path, PathBuf};

use aevum_core::align::Estimator;
use aevum_core::audio::{AudioCodec, AudioOptions};
use aevum_core::colorspace::ColorSpace;
use aevum_core::crop::Motion;
use aevum_core::errmsg::clip_echo;
use aevum_core::export::{Codec, OutputDynamic, OutputSharpen, SharpenStrength, StillFormat};
use aevum_core::watermark::Watermark;
use serde_json::{Value, json};

use crate::session::{MAX_LONG_SIDE, MAX_SERIES, Session, normalize};

// ------------------------------------------------------------------ 参数与安全检查
pub(super) const X264_PRESETS: &[&str] = &[
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
    "placebo",
];

pub(super) fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("缺少字符串参数 {key}"))
}

pub(super) fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是字符串")),
    }
}

pub(super) fn opt_u32(args: &Value, key: &str) -> Result<Option<u32>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let n = value
                .as_u64()
                .or_else(|| {
                    value.as_f64().and_then(|f| {
                        (f.is_finite() && f >= 0.0 && f.fract() == 0.0).then_some(f as u64)
                    })
                })
                .and_then(|n| u32::try_from(n).ok());
            n.map(Some).ok_or_else(|| format!("{key} 必须是非负整数"))
        }
    }
}

pub(super) fn opt_f64(args: &Value, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是有限数值")),
    }
}

pub(super) fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是布尔值")),
    }
}

pub(super) fn confirm(args: &Value) -> bool {
    args.get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `watermark` 参数：整块按 `Watermark` 自己的 serde 形状收，键名与工程文件、
/// `render.set` 一致。这里没有「面板当前值」可合并，所以只写部分键时，没写的
/// 键取默认值——不传整个参数就等于没开水印。
pub(super) fn opt_watermark(args: &Value) -> Result<Watermark, String> {
    match args.get("watermark") {
        None | Some(Value::Null) => Ok(Watermark::default()),
        Some(v) => {
            serde_json::from_value(v.clone()).map_err(|e| format!("watermark 参数不对：{e}"))
        }
    }
}

/// `motion` 参数 → 运镜设定：没传就沿用会话工程里那一份（与队列同口径），传了就在
/// 当前值上打补丁——面板上定好的取景框不该因为这次没写这个键就悄悄丢掉。
pub(super) fn motion_arg(session: &Session, args: &Value) -> Result<Motion, String> {
    let base = session
        .project
        .as_ref()
        .map(|p| p.render.motion)
        .unwrap_or_default();
    match args.get("motion").filter(|v| !v.is_null()) {
        None => Ok(base),
        Some(raw) => raw
            .as_object()
            .ok_or_else(|| "motion 要写成对象：{ enabled, from, to, ease, swap, refit }".to_owned())
            .and_then(|_| base.patched(raw)),
    }
}

/// `aevum_export_video` 的配乐选项；全部可省，缺省与 GUI 默认一致。
pub(super) fn audio_options(args: &Value) -> Result<AudioOptions, String> {
    let mut opts = AudioOptions::default();
    if let Some(delay) = opt_f64(args, "delay")? {
        if !(0.0..=3600.0).contains(&delay) {
            return Err("delay 必须在 0..3600 秒".to_owned());
        }
        opts.delay = delay;
    }
    if let Some(gain) = opt_f64(args, "gain")? {
        if !(0.0..=400.0).contains(&gain) {
            return Err("gain 必须在 0..400（100 为原始音量）".to_owned());
        }
        opts.gain = gain;
    }
    if let Some(fade) = opt_f64(args, "fade_in")? {
        if !(0.0..=3600.0).contains(&fade) {
            return Err("fade_in 必须在 0..3600 秒".to_owned());
        }
        opts.fade_in = fade;
    }
    if let Some(fade) = opt_f64(args, "fade_out")? {
        if !(0.0..=3600.0).contains(&fade) {
            return Err("fade_out 必须在 0..3600 秒".to_owned());
        }
        opts.fade_out = fade;
    }
    if let Some(looped) = opt_bool(args, "loop")? {
        opts.looped = looped;
    }
    if let Some(trim) = opt_bool(args, "trim")? {
        opts.trim = trim;
    }
    if let Some(codec) = opt_str(args, "acodec")? {
        opts.acodec = AudioCodec::parse(codec).ok_or_else(|| {
            format!(
                "未知 acodec {}；可用 auto / copy / aac128 / aac192 / aac256 / aac320",
                echo_text(codec)
            )
        })?;
    }
    Ok(opts)
}

pub(super) fn require_batch_confirm(args: &Value, tool: &str) -> Result<(), String> {
    if confirm(args) {
        Ok(())
    } else {
        Err(format!("{tool} 是批量写操作，需参数 confirm: true"))
    }
}

pub(super) fn check_long_side(long_side: Option<u32>, args: &Value) -> Result<(), String> {
    match long_side {
        Some(side) if side > MAX_LONG_SIDE && !confirm(args) => Err(format!(
            "long_side {side} 超过 {MAX_LONG_SIDE}，需 confirm: true"
        )),
        _ => Ok(()),
    }
}

/// 精确输出宽高（可选）：`width` 与 `height` 必须同时给，偶数且 ≥2。
///
/// 与 `long_side` 二选一，避免「按长边缩」和「精确画幅」同时生效时含义打架。
pub(super) fn target_size_arg(args: &Value) -> Result<Option<(u32, u32)>, String> {
    match (opt_u32(args, "width")?, opt_u32(args, "height")?) {
        (Some(width), Some(height)) => {
            if args.get("long_side").is_some() {
                return Err("width/height 与 long_side 只能二选一".to_owned());
            }
            if width < 2 || height < 2 || width % 2 == 1 || height % 2 == 1 {
                return Err(format!(
                    "width/height 必须是偶数且不小于 2：{width}×{height}"
                ));
            }
            Ok(Some((width, height)))
        }
        (None, None) => Ok(None),
        _ => Err("width 与 height 需同时给出（或都不给）".to_owned()),
    }
}

/// 把用户给的字符串塞进错误串的唯一写法：截到 `MAX_ECHO_CHARS`。
///
/// The one way to put a caller-supplied string into an error string: clip it to
/// `MAX_ECHO_CHARS`.
pub(crate) fn echo_text(text: &str) -> String {
    clip_echo(text)
}

/// 把路径塞进错误串的唯一写法：先显示成字符串，再截到 `MAX_ECHO_CHARS`。
///
/// 为什么要有这个漏斗：路径来自调用方，可以长到几十 KB。`format!("…：{}", path.display())`
/// 这种写法在 1.4.0 之前散在十来个地方，改一处漏一处；收成一个函数之后，新代码要么用它，
/// 要么一眼就能在看 diff 时被发现。
///
/// The one way to put a path into an error string: render it, then clip it to
/// `MAX_ECHO_CHARS`.
///
/// Why a funnel: paths come from the caller and can be tens of KB long. Before 1.4.0 the
/// `format!("...: {}", path.display())` shape was scattered over a dozen sites and every fix
/// missed one; with a single function, new code either calls it or stands out in review.
pub(super) fn echo_path(path: &Path) -> String {
    echo_text(&path.display().to_string())
}

pub(super) fn absolute(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("路径为空".to_owned());
    }
    Ok(normalize(path))
}

pub(super) fn parse_format(name: &str) -> Result<StillFormat, String> {
    StillFormat::parse(name).ok_or_else(|| {
        format!(
            "未知格式 {}；可用 {}",
            echo_text(name.trim()),
            StillFormat::ALL
                .iter()
                .map(|f| f.key())
                .collect::<Vec<_>>()
                .join(" / ")
        )
    })
}

/// 有损容器只当最终交付：视频那条路的中间帧必须是 PNG/TIFF。
pub(super) fn check_video_format(format: StillFormat) -> Result<(), String> {
    if format.frames_ok() {
        return Ok(());
    }
    Err(format!(
        "{} 只能出静帧；视频的中间帧要 png8 或 tiff16",
        format.cn()
    ))
}

pub(super) fn opt_quality(args: &Value) -> Result<u8, String> {
    match opt_u32(args, "quality")? {
        Some(q) if (1..=100).contains(&q) => Ok(q as u8),
        Some(q) => Err(format!("quality 要在 1..100：{q}")),
        None => Ok(90),
    }
}

/// 输出锐化两个档：默认 `off`，无头导出与 GUI 面板的缺省一致。
pub(super) fn opt_sharpen(args: &Value) -> Result<(OutputSharpen, SharpenStrength), String> {
    let for_ = match opt_str(args, "sharpen_for")? {
        Some(raw) => OutputSharpen::parse(raw).ok_or_else(|| {
            format!(
                "未知锐化用途 {}；可用 off / screen / glossy / matte",
                echo_text(raw)
            )
        })?,
        None => OutputSharpen::Off,
    };
    let strength = match opt_str(args, "sharpen_strength")? {
        Some(raw) => SharpenStrength::parse(raw).ok_or_else(|| {
            format!(
                "未知锐化强度 {}；可用 low / standard / high",
                echo_text(raw)
            )
        })?,
        None => SharpenStrength::Standard,
    };
    Ok((for_, strength))
}

/// `color_space` 参数：省略即 sRGB（跟改造前的导出行为一致）。
pub(super) fn opt_color_space(args: &Value) -> Result<ColorSpace, String> {
    match opt_str(args, "color_space")? {
        Some(raw) => ColorSpace::parse(raw)
            .ok_or_else(|| format!("未知色域 {}；可用 srgb / p3 / rec2020", echo_text(raw))),
        None => Ok(ColorSpace::default()),
    }
}

/// `dynamic` 参数：省略即 SDR。给了 `hdr-pq` 就顺手把色域补成 Rec.2020——那是
/// 唯一的合法组合，让调用方为这件事多写一个参数只是制造一次必然的报错。
///
/// 补完之后仍然走 `check_dynamic`：容器装不下（PNG/TIFF/JPEG/WebP）照样报错，
/// 别在这里悄悄退回 SDR。
///
/// The `dynamic` argument: SDR when omitted. Passing `hdr-pq` also fills in Rec.2020 — that
/// is the only legal combination, and making callers spell it out only creates a guaranteed
/// error. `check_dynamic` still runs afterwards: a container that cannot hold it
/// (PNG/TIFF/JPEG/WebP) errors out rather than quietly falling back to SDR.
pub(super) fn opt_dynamic(args: &Value) -> Result<OutputDynamic, String> {
    match opt_str(args, "dynamic")? {
        Some(raw) => OutputDynamic::parse(raw)
            .ok_or_else(|| format!("未知动态范围 {}；可用 sdr / hdr-pq", echo_text(raw))),
        None => Ok(OutputDynamic::default()),
    }
}

/// 按动态范围把色域补成合法组合。
///
/// Fills in a legal color space for the given dynamic range.
pub(super) fn color_space_for(dynamic: OutputDynamic, chosen: ColorSpace) -> ColorSpace {
    match dynamic {
        OutputDynamic::Sdr => chosen,
        OutputDynamic::HdrPq => ColorSpace::Rec2020,
    }
}

pub(super) fn format_name(format: StillFormat) -> &'static str {
    format.key()
}

pub(super) fn parse_codec(name: &str) -> Result<Codec, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "h264" | "264" | "avc" => Ok(Codec::H264),
        "h265" | "265" | "hevc" => Ok(Codec::H265),
        other => Err(format!("未知编码 {}；可用 h264 / h265", echo_text(other))),
    }
}

pub(super) fn parse_estimator(name: &str) -> Result<Estimator, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "template" | "tpl" => Ok(Estimator::Template),
        "phase" => Ok(Estimator::Phase),
        other => Err(format!(
            "未知 estimator {}；可用 template / phase",
            echo_text(other)
        )),
    }
}

pub(super) fn estimator_name(estimator: Estimator) -> &'static str {
    match estimator {
        Estimator::Template => "template",
        Estimator::Phase => "phase",
    }
}

/// 必填帧号：缺参、非整数、越界都返回结构化中文错误。
pub(super) fn arg_frame(session: &Session, args: &Value, key: &str) -> Result<usize, String> {
    let frame = opt_u32(args, key)?.ok_or_else(|| format!("缺少整数参数 {key}"))? as usize;
    let n = session.require_seq()?.len();
    if frame >= n {
        return Err(format!("{key} {frame} 超出范围 0..{}", n.saturating_sub(1)));
    }
    Ok(frame)
}

pub(super) fn stride_for(n: usize) -> usize {
    n.div_ceil(MAX_SERIES).max(1)
}

pub(super) fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

pub(super) fn summarize(values: impl Iterator<Item = Option<f64>>) -> Value {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut count = 0usize;
    for value in values.flatten().filter(|v| v.is_finite()) {
        min = min.min(value);
        max = max.max(value);
        sum += value;
        count += 1;
    }
    if count == 0 {
        return json!({"count": 0});
    }
    json!({
        "count": count,
        "min": round6(min),
        "max": round6(max),
        "avg": round6(sum / count as f64),
    })
}

// ------------------------------------------------------------------ 注册表
