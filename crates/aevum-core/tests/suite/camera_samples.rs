//! 相机样张矩阵：每一张都必须落在**一个说得出名字的结果**上。
//!
//! # 这一层补的是什么
//!
//! `samples/cc0/` 的判据证明「至少有一台 iPhone 的 DNG 能解」。它证明不了第二台、
//! 第二个厂商、第二种压缩方式。1.5 候选第一次拿十个真实样张跑，就抓到
//! `rawler 0.7.2` 在 iPhone 的 LinearRaw DNG 上越界 panic，而 panic 发生在它自己的
//! rayon 工作线程里被吞掉——主流程返回 `Ok`，拿到的是部分未线性化的脏数据。
//!
//! 所以这里的判据只问三件事，每件都是「一个样本测不出来」的：
//!
//! 1. **绝不 panic**：整次解码用 `catch_unwind` 包住，捕获到就红并打印文件名。
//!    这条是事故现场的直接还原——坏文件不该有能力掀翻进程。
//! 2. **结果必须是具名的**：`Ok`（尺寸 > 0），或 `UnsupportedFormat` /
//!    `DecodeFailed` / `DecoderPanic` 三类之一。落到别的形状上就红——那意味着
//!    出现了一种我们没登记过的失败方式。
//! 3. **不支持的格式要说名字**：HEIC 是 `UnsupportedFormat` 且串里带 `heic`，
//!    不是「格式无法确定」。
//!
//! # 样张不入库，所以缺目录是 skip 而不是 fail
//!
//! `samples/cameras/` 的文件体积 164 MB、部分来源不明，不随仓库分发（见该目录
//! README）。本机没有样张时判据打一行说明就返回——这是正常状态。**但「有文件却没
//! 通过」永远是红**：那说明某台相机/某种压缩被解坏了，正是这一层存在的理由。
//! 与 `samples/cc0/` 的取舍相反：那边是入库文件，缺失意味着仓库被改坏，所以那边红。
//!
//! # The camera sample matrix: every file must land on a **named** outcome
//!
//! # What this layer adds
//!
//! The assertions in `samples/cc0/` prove "at least one iPhone DNG decodes". They cannot
//! prove a second body, a second vendor or a second compression scheme. The first run of the
//! 1.5 candidate against ten real samples caught `rawler 0.7.2` panicking out of bounds on an
//! iPhone LinearRaw DNG, with the panic swallowed inside its own rayon worker -- the main
//! flow returned `Ok` with partially un-linearised garbage.
//!
//! So the assertions here ask only three things, each of which a single sample cannot answer:
//!
//! 1. **Never panic**: the whole decode is wrapped in `catch_unwind`; a catch fails the test
//!    and prints the file name. This restates the incident directly -- a bad file must not be
//!    able to take the process down.
//! 2. **The outcome must be named**: `Ok` (non-zero size), or one of `UnsupportedFormat` /
//!    `DecodeFailed` / `DecoderPanic`. Anything else fails the test, because it means a
//!    failure mode we have not registered has appeared.
//! 3. **Unsupported formats are named**: HEIC is `UnsupportedFormat` with `heic` in the
//!    message, not "could not determine the format".
//!
//! # Samples are not committed, so a missing directory skips rather than fails
//!
//! The files under `samples/cameras/` total 164 MB and are partly of unknown provenance, so
//! they are not distributed with the repository (see that directory's README). Without
//! samples the assertion prints a line and returns -- the normal state. **A file that is
//! present and does not pass is always red**: it means some body or compression decodes
//! wrongly, which is the entire reason this layer exists. The trade-off is the opposite of
//! `samples/cc0/`: those files are committed, so a missing one means the repository is broken
//! and that assertion goes red.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use aevum_core::metrics::{self, MetricsError};
use aevum_core::raw::RawError;

/// 样张目录。不入库，所以可能不存在。
///
/// The sample directory. Not committed, so it may not exist.
fn sample_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("cameras")
}

/// 目录里的样张（跳过 README，跳过缩略图缓存目录）。排序后返回，失败信息才可复现。
///
/// The samples in the directory (READMEs and the thumbnail cache directory skipped), sorted
/// so failure messages are reproducible.
fn samples() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sample_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            !name.starts_with("README")
        })
        .collect();
    out.sort();
    out
}

/// 把一次解码的失败收进「登记过的三类」。
///
/// `DecodeFailed` 是**归并**：容器不对（`NotTiff`）、压缩不支持（`Rawler`）、
/// 普通图片解不开（`Image`）、DNG 结构不认识（`Dng`）在用户看来是同一件事——
/// 「这张图这一版解不出来」。分开记没有诊断价值，反而让判据跟着错误枚举的粒度漂移。
///
/// Fold a decode failure into the three registered classes.
///
/// `DecodeFailed` is a **merge**: a wrong container (`NotTiff`), an unsupported compression
/// (`Rawler`), an undecodable plain picture (`Image`) and an unknown DNG structure (`Dng`)
/// are the same thing to the user -- "this version cannot decode this image". Splitting them
/// adds no diagnostic value and only makes the assertion drift with the error enum's grain.
fn classify(err: &MetricsError) -> &'static str {
    match err {
        MetricsError::Raw(RawError::UnsupportedFormat { .. }) => "UnsupportedFormat",
        MetricsError::Raw(RawError::DecoderPanic { .. }) | MetricsError::DecoderPanic(_) => {
            "DecoderPanic"
        }
        MetricsError::Raw(_)
        | MetricsError::Image(_)
        | MetricsError::Dng(_)
        | MetricsError::Profile(_) => "DecodeFailed",
        // IO 单列：读不到文件是环境问题，不是解码结论，判据要能把它区分出来。
        // `Io` gets its own class: failing to read is an environment problem, not a decode
        // verdict, and the assertion must be able to tell them apart.
        MetricsError::Io(_) => "Io",
    }
}

/// 每个样张都必须落在一个具名结果上，且解码过程绝不 panic。
///
/// Every sample must land on a named outcome, and decoding must never panic.
#[test]
fn every_camera_sample_ends_in_a_named_outcome() {
    let files = samples();
    if files.is_empty() {
        eprintln!(
            "跳过：{} 里没有样张（样张不入库，清单见该目录 README）",
            sample_dir().display()
        );
        return;
    }

    let mut report = String::new();
    for path in &files {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // 整次调用包住：`catch_unwind` 是判据的第一条，也是事故现场的那一条。
        // `AssertUnwindSafe` 的理由同 `raw::guard`：只读解码，panic 后不复用半成品。
        //
        // The whole call is wrapped: `catch_unwind` is the first assertion and the one the
        // incident was about. `AssertUnwindSafe` for the same reason as in `raw::guard`:
        // read-only decode, nothing half-built is reused after a panic.
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| metrics::decode(path)));
        match outcome {
            Err(_) => panic!("{name}：解码 panic 了。坏文件不许有能力掀翻进程"),
            Ok(Ok(img)) => {
                assert!(
                    img.width() > 0 && img.height() > 0,
                    "{name}：解出 0 像素等于没解"
                );
                report.push_str(&format!("{name}: OK {}×{}\n", img.width(), img.height()));
            }
            Ok(Err(err)) => {
                let class = classify(&err);
                assert!(
                    matches!(class, "UnsupportedFormat" | "DecodeFailed" | "DecoderPanic"),
                    "{name}：落在未登记的结果 `{class}` 上，判据要求具名（{err}）"
                );
                report.push_str(&format!("{name}: {class}（{err}）\n"));
            }
        }
    }
    // 失败信息里带上全表：只看第一行红的人也能知道其余的现状。
    //
    // Print the whole table: whoever sees only the first red line still learns the rest.
    eprintln!("相机样张矩阵（{} 张）：\n{report}", files.len());
}

/// HEIC 必须按后缀具名拒绝，不许「猜格式再报格式不明」。
///
/// 这条单独成判据而不是并进矩阵：矩阵里它只是「三类之一」，而这里的断言是
/// **必须是那一类**。HEIC 是最容易被误当「原图」拖进来的格式，口径错了用户
/// 会以为文件坏了。
///
/// HEIC must be rejected by suffix with a name, never "guess the format, then report an
/// unknown format".
///
/// It is a separate assertion rather than a row in the matrix because there it is only "one of
/// three classes", while here the requirement is **that specific class**. HEIC is the format
/// most easily mistaken for an "original" and dragged in, and the wrong wording makes users
/// think their file is corrupt.
#[test]
fn heic_is_rejected_by_name_not_guessed() {
    let Some(heic) = samples().into_iter().find(|p| {
        p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("heic"))
    }) else {
        eprintln!("跳过：没有 HEIC 样张");
        return;
    };
    let err = metrics::decode(&heic).expect_err("HEIC 这一版不做，理应拒绝");
    match &err {
        MetricsError::Raw(RawError::UnsupportedFormat { ext }) => {
            assert_eq!(ext, "heic", "扩展名要归一成小写");
        }
        other => panic!("HEIC 应当是 UnsupportedFormat，实际是 {other:?}"),
    }
    assert!(
        err.to_string().contains("heic"),
        "错误串里要有 `heic`，否则用户不知道该转什么：{err}"
    );
}
