//! 解码守卫：把第三方解码器在坏字节上的 panic 变成一条可读的错误。
//!
//! # 为什么需要这一层
//!
//! 解码器吃的是**不可信的二进制**：相机写坏的文件、下载了一半的文件、别人发来的
//! 样张。这类输入的规范化程度远低于「程序自己拼出来的结构」，而解码器内部到处是
//! `buf[i]`、`unwrap()`、`expect()`——它们的前提是「文件按规范写」，一旦不成立就是
//! 越界或空值 panic。
//!
//! Rust 的 `Result` 只覆盖**作者想到并返回了错误**的那部分。库里没被想到的路径
//! （`rawler 0.7.2` 在 iPhone 的 LinearRaw DNG 上读越界就是这种）会直接 panic：
//! 在主线程上是崩溃，在 rayon 工作线程上是**这张图静默地变成半成品**——线程死了，
//! 结果集里那一格没人填，主流程照常返回 `Ok`。后者比崩溃危险得多，因为它看起来
//! 像成功。
//!
//! 所以边界要自己兜：`catch_unwind` 把 panic 收成一个具名错误，再交给既有的错误
//! 分级去决定「跳过 / 退预览 / 报给用户」。
//!
//! # 为什么 `UnwindSafe` 在这里站得住
//!
//! `catch_unwind` 要求闭包 `UnwindSafe`，本质是问「panic 之后继续用捕获的状态安全
//! 吗」。这里的调用全部是**只读解码**：输入是一段 `&[u8]` 或一个路径，输出是全新的
//! 像素缓冲，解码器自身在闭包里被消费掉（`decode_file`/`decode` 都是 `self` 语义）。
//! panic 之后没有任何半成品被复用——闭包外拿到的要么是完整的 `Ok`，要么是一个
//! `DecoderPanic`，中间态随闭包的栈一起丢掉。个别持有 `&mut` 的适配器（`ImageReader`
//! 在闭包内被 move 进去）用 `AssertUnwindSafe` 明确认账，理由同上：那个 `&mut` 只指向
//! 闭包自己拥有的 reader，不会漏到外面。
//!
//! # Decode guard: turn a third-party decoder's panic on bad bytes into a readable error
//!
//! Decoders eat **untrusted binaries**: files a camera wrote badly, half-downloaded files,
//! samples from other people. That input is far less canonical than structures a program
//! built itself, and decoders are full of `buf[i]`, `unwrap()` and `expect()` whose premise
//! is "the file follows the spec". When it does not, the result is an out-of-bounds read or
//! a panic on an empty value.
//!
//! Rust's `Result` only covers the paths the author thought of and returned an error on. A
//! path nobody thought of (such as `rawler 0.7.2` reading past the end of an iPhone
//! LinearRaw DNG) panics outright: on the main thread that is a crash, and on a rayon worker
//! it is **that one frame silently turning into a half-decoded image** -- the thread dies,
//! nobody fills that slot, and the main flow still returns `Ok`. The latter is far more
//! dangerous because it looks like success.
//!
//! So the boundary has to catch it itself: `catch_unwind` turns the panic into a named error
//! and hands it to the existing error classification, which decides whether to skip the
//! frame, fall back to the preview, or tell the user.
//!
//! # Why `UnwindSafe` holds here
//!
//! `catch_unwind` requires an `UnwindSafe` closure, which really asks "is it safe to keep
//! using the captured state after a panic". Every call here is a **read-only decode**: the
//! input is a `&[u8]` or a path, the output is a fresh pixel buffer, and the decoder itself
//! is consumed inside the closure (`decode_file`/`decode` both take `self`). Nothing
//! half-finished is reused after a panic -- the caller sees either a complete `Ok` or a
//! `DecoderPanic`, and any intermediate state dies with the closure's stack. The few adapters
//! holding a `&mut` (`ImageReader` is moved into the closure) acknowledge it with
//! `AssertUnwindSafe` for the same reason: that reference only points at the reader the
//! closure owns and cannot escape.

use std::any::Any;
use std::panic::{UnwindSafe, catch_unwind};

use crate::errmsg::clip_echo;

use super::RawError;

/// 一次被挡下的解码 panic：谁解的、负载原文说了什么。
///
/// A decode panic that was caught: who was decoding, and what the payload said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecoderPanic {
    /// 解码器名（`"rawler"`、`"image"`、`"aevum"`），进错误串的第一段。
    ///
    /// Decoder name (`"rawler"`, `"image"`, `"aevum"`), the first clause of the message.
    pub backend: &'static str,
    /// panic 负载里的文字。只有负载是 `&str`/`String` 时才取得到，其余是 `None`——
    /// 不为了凑一句话去 `Debug` 一个内部类型的 panic 负载，那样只会漏出实现细节。
    ///
    /// Text from the panic payload. It is only available when the payload is a `&str` or a
    /// `String`; otherwise `None` -- Debug-printing an internal payload type just to have a
    /// sentence would leak implementation details.
    pub detail: Option<String>,
}

impl DecoderPanic {
    /// 负载原文。调用方要拼「为什么」时读它，没有就是 `None`（不是空串）。
    ///
    /// The payload text. Callers read it to spell out "why"; absent means `None`, not `""`.
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// 从 panic 负载里摘出一句能给人看的话。
///
/// `&str` 与 `String` 两种形态都认（`panic!("...")`、`panic_any(String)`、`assert!`
/// 的默认负载各不相同），其余一律 `None`。截断走 [`clip_echo`]：一个 20 KB 的断言
/// 消息灌进错误串会把界面的信息栏和 agent 的上下文一起填满。
///
/// Pull one human-readable line out of a panic payload.
///
/// Both `&str` and `String` are recognized (`panic!("...")`, `panic_any(String)` and the
/// default `assert!` payloads all differ), and anything else is `None`. Truncation goes
/// through [`clip_echo`]: a 20 KB assertion message would fill both the info bar and an
/// agent's context.
fn payload_text(payload: &(dyn Any + Send)) -> Option<String> {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return Some(clip_echo(s));
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return Some(clip_echo(s));
    }
    None
}

/// 把一段可能 panic 的调用包起来，保留调用方自己的返回值类型。
///
/// 这是原语：[`decode_guard`] 是它在 `RawError` 上的特化，而 `metrics` / `export` 那
/// 几处边界要保住各自的错误类型（普通 JPEG 不该被报成「RAW 解码失败」），所以直接用它。
///
/// Wrap a call that may panic while preserving the caller's own return type.
///
/// This is the primitive: [`decode_guard`] is its `RawError` specialization, while the
/// boundaries in `metrics` / `export` must keep their own error types (a plain JPEG must not
/// be reported as "RAW decode failed"), so they call this directly.
pub fn catch_decoder_panic<T>(
    what: &'static str,
    f: impl FnOnce() -> T + UnwindSafe,
) -> Result<T, DecoderPanic> {
    match catch_unwind(f) {
        Ok(value) => Ok(value),
        // panic 钩子已经把「哪一行」打到 stderr 了，这里只负责把它变成一次可跳过的
        // 失败。故意不装静音钩子：解码器 panic 是真 bug，日志里必须留痕。
        //
        // The panic hook has already printed "which line" to stderr; this layer only turns
        // it into a skippable failure. Deliberately no silencing hook: a decoder panic is a
        // real bug and must leave a trace in the log.
        Err(payload) => Err(DecoderPanic {
            backend: what,
            detail: payload_text(&*payload),
        }),
    }
}

/// 在不可信二进制上跑第三方解码器：库内部 panic（索引越界、unwrap）必须变成一个可读
/// 的错误，而不是穿透成崩溃或静默脏数据。
///
/// 闭包里只能做解码：见模块头对 `UnwindSafe` 的说明。
///
/// Run a third-party decoder over untrusted bytes: a panic inside the library
/// (out-of-bounds index, unwrap) must become a readable error instead of breaking through as
/// a crash or as silently dirty data.
///
/// The closure must do nothing but decode; see the module docs on `UnwindSafe`.
pub fn decode_guard<T>(
    what: &'static str,
    f: impl FnOnce() -> Result<T, RawError> + UnwindSafe,
) -> Result<T, RawError> {
    match catch_decoder_panic(what, f) {
        Ok(inner) => inner,
        Err(panic) => Err(RawError::from(panic)),
    }
}

impl From<DecoderPanic> for RawError {
    fn from(p: DecoderPanic) -> Self {
        Self::DecoderPanic {
            backend: p.backend,
            detail: p.detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解码器 panic 必须变成具名错误，而不是穿透。
    ///
    /// A decoder panic must become a named error instead of breaking through.
    #[test]
    fn a_panicking_decoder_becomes_an_error_not_a_crash() {
        let err = decode_guard::<u8>("test", || panic!("boom")).unwrap_err();
        match &err {
            RawError::DecoderPanic { backend, .. } => assert_eq!(*backend, "test"),
            other => panic!("应当是 DecoderPanic，实际是 {other:?}"),
        }
        let text = err.to_string();
        assert!(
            text.contains("test"),
            "错误串里要有后端名，否则用户不知道该怪谁：{text}"
        );
    }

    /// 负载是 `String` 时要原样带出来（`panic!` 与 `assert!` 的负载类型并不统一，
    /// 这条锁的是「认得 `String`」这一半）。
    ///
    /// A `String` payload must be carried through verbatim (`panic!` and `assert!` do not
    /// agree on payload types; this locks the "`String` is recognized" half).
    #[test]
    fn the_payload_text_survives_into_the_message() {
        let err = decode_guard::<u8>("rawler", || std::panic::panic_any(String::from("索引越界")))
            .unwrap_err();
        match &err {
            RawError::DecoderPanic { detail, .. } => {
                assert_eq!(detail.as_deref(), Some("索引越界"));
            }
            other => panic!("应当是 DecoderPanic，实际是 {other:?}"),
        }
        assert!(err.to_string().contains("索引越界"));
    }

    /// 不 panic 的路径一位都不许变：守卫只管异常，不管正常值。
    ///
    /// The non-panicking path must not change by a single bit: the guard only handles the
    /// abnormal case.
    #[test]
    fn a_healthy_decode_passes_through_untouched() {
        let ok: Result<u32, RawError> = decode_guard("test", || Ok(7));
        assert_eq!(ok.ok(), Some(7));
        let err: Result<u32, RawError> = decode_guard("test", || Err(RawError::Truncated));
        assert!(matches!(err, Err(RawError::Truncated)));
    }

    /// 负载不是文字时留空，不硬凑。
    ///
    /// A non-text payload stays empty instead of being made up.
    #[test]
    fn a_non_text_payload_leaves_the_detail_empty() {
        let err = decode_guard::<u8>("test", || std::panic::panic_any(42u8)).unwrap_err();
        match &err {
            RawError::DecoderPanic { detail, .. } => assert!(detail.is_none()),
            other => panic!("应当是 DecoderPanic，实际是 {other:?}"),
        }
    }
}
