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

/// 真机 JPEG 样张必须解成 `Ok`，不是「具名失败也行」。
///
/// 这条单独成判据的原因写在事故里：JPEG 是完整支持的格式，但 1.5.1 的完整性
/// 校验按「最后一个 EOI 之后只许填充」判，把真机 Panasonic（主流 EOI 后带
/// 376 字节厂商尾注）误杀成「文件不完整」。矩阵判据抓不到它——矩阵的通过
/// 条件是「落在三类之一」，`DecodeFailed` 恰好算绿。已支持格式的真机样张
/// 标准必须更高：解不出 `Ok` 永远红。
///
/// Real-camera JPEG samples must decode to `Ok`, not merely a "named failure".
///
/// The reason this is a separate assertion is written in an incident: JPEG is a fully
/// supported format, but the 1.5.1 completeness check judged "only padding may follow
/// the last EOI" and killed a real Panasonic file (376 bytes of vendor trailer after the
/// main EOI) as "incomplete". The matrix cannot catch it -- its pass condition is "lands
/// in one of the classes", and `DecodeFailed` counts as green. A real sample of a
/// supported format must be held to a higher bar: failing to decode to `Ok` is always red.
#[test]
fn every_camera_jpeg_sample_decodes_ok() {
    let jpegs: Vec<PathBuf> = samples()
        .into_iter()
        .filter(|p| {
            p.extension().is_some_and(|e| {
                matches!(e.to_string_lossy().to_lowercase().as_str(), "jpg" | "jpeg")
            })
        })
        .collect();
    if jpegs.is_empty() {
        eprintln!("跳过：没有 JPEG 样张");
        return;
    }
    for path in &jpegs {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| metrics::decode(path)));
        match outcome {
            Err(_) => panic!("{name}：解码 panic 了"),
            Ok(Ok(img)) => {
                assert!(
                    img.width() > 0 && img.height() > 0,
                    "{name}：解出 0 像素等于没解"
                );
            }
            Ok(Err(err)) => panic!(
                "{name}：JPEG 是完整支持的格式，真机样张解成 `{}` 不许过——这大概率是 \
                 完整性校验或解码路径又误杀了一种真机写卡形状（{err}）",
                classify(&err)
            ),
        }
    }
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

/// 竖拍 RAW 在**每条解码路**上都必须转正：全量、草图、逻辑尺寸，宽高关系跟着
/// EXIF 方向码走，且彼此一致。
///
/// 事故：README 主图里夜景城市横躺 90°，而胶片条缩略图是正的。根因是 rawler 0.8.0
/// 把 `RawImage.orientation` 硬编码成 Normal（rawimage.rs 两处 `// TODO fixme`），
/// 后端对任何文件都报 1，`decode` 信了它就不转——只有走内嵌 JPEG 的缩略图用了
/// `exif::read` 才是正的。修复是后端报 1 时回落 `exif::read`（`resolve_orientation`）。
///
/// 判据的不变量：`read_full` 的像素是**未转正**的传感器排布（它的宽高关系当基准），
/// 方向码 2–4 只翻不换边、5–8 换边；所以「解码结果是否横」必须等于
/// 「传感器横 XOR 方向码换边」。全量与草图两条路各自核一遍，互相不一致也红。
///
/// Portrait RAWs must come out upright on **every** decode route: full preview, sketch and
/// the logical size must all follow the EXIF orientation code and agree with each other.
///
/// Incident: the README hero showed a night city lying on its side while the filmstrip
/// thumbnail stood upright. Root cause: rawler 0.8.0 hardcodes `RawImage.orientation` to
/// Normal (two `// TODO fixme` sites in rawimage.rs), so the backend reported 1 for every
/// file and `decode` trusted it -- only the embedded-JPEG thumbnails used `exif::read` and
/// came out upright. The fix falls back to `exif::read` when the backend reports 1
/// (`resolve_orientation`).
///
/// The invariant: `read_full`'s pixels are the **unrotated** sensor layout (its width/height
/// relation is the baseline); codes 2-4 flip without swapping sides, 5-8 swap. So "decoded is
/// landscape" must equal "sensor is landscape XOR the code swaps". Both routes are checked
/// independently, and disagreeing with each other is red too.
#[test]
fn every_raw_sample_is_upright_in_every_route() {
    use aevum_core::raw;

    let files: Vec<PathBuf> = samples()
        .into_iter()
        .filter(|p| raw::is_raw_name(p))
        .collect();
    if files.is_empty() {
        eprintln!("跳过：没有 RAW 样张");
        return;
    }
    for path in &files {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // read_full 自身也包住：它跑的同样是不可信字节。
        let full = std::panic::catch_unwind(AssertUnwindSafe(|| raw::read_full(path)));
        let Ok(Ok((mosaic, _route, backend_orientation))) = full else {
            // 解不动的样张归矩阵判据管；这里只对解得动的立规矩。
            eprintln!("{name}: 解码失败（归矩阵判据管），跳过");
            continue;
        };
        // 方向码的**基准是 exif::read，不是后端**：基准若也从解码栈里拿，撤掉回落
        // 的变异会让两边一起变，判据自洽地绿（本轮牙口测试真踩了）。exif::read 直接
        // 读文件头，独立于解码栈。后端非 1 时（CR3 那类 exif 读不到的容器）以后端为准；
        // exif 读出了非 1 时，后端必须与它一致——这正是「报 1 时回落」的合同。
        let exif_orientation = aevum_core::exif::read(path).orientation;
        if exif_orientation != 1 {
            assert_eq!(
                backend_orientation, exif_orientation,
                "{name}: exif 方向码 {exif_orientation}，后端却报 {backend_orientation}——\
                 竖拍在这里躺平（README 主图事故的根因）"
            );
        }
        let orientation = if exif_orientation != 1 {
            exif_orientation
        } else {
            backend_orientation
        };
        let sensor_landscape = mosaic.width() > mosaic.height();
        let swaps = (5..=8).contains(&orientation);
        let expect_landscape = sensor_landscape != swaps;

        let decoded = metrics::decode_rgb(path, 200)
            .unwrap_or_else(|e| panic!("{name}: 全量解码失败（{e}），不配「能解却躺平」"));
        assert_eq!(
            decoded.width() > decoded.height(),
            expect_landscape,
            "{name}: 方向码 {orientation}，传感器{}、全量解出 {}×{}——宽高关系没跟着方向码走\
            （画布躺平就是这么来的）",
            if sensor_landscape { "横" } else { "竖" },
            decoded.width(),
            decoded.height()
        );

        let sketch = metrics::decode_preview_sketch(path, "", 200, 400)
            .unwrap_or_else(|e| panic!("{name}: 草图解码失败（{e}）"));
        assert_eq!(
            sketch.w > sketch.h,
            expect_landscape,
            "{name}: 草图 {}×{} 与全量方向不一致",
            sketch.w,
            sketch.h
        );
        assert_eq!(
            sketch.full.0 > sketch.full.1,
            expect_landscape,
            "{name}: 草图报的全量逻辑尺寸 {:?} 方向不对",
            sketch.full
        );
    }
}

// ---------------------------------------------------------------- 两条路必须同源

/// `raw::info` 与 `raw::read` 对同一份样张必须给出**同一个结论**：走哪条路、多大、
/// 哪个机型。
///
/// # 这条判据是从一个真实缺陷里长出来的
///
/// 2026-11-04 第一次把两张路径并排打印出来：Canon CR3 与四张 Panasonic RW2，
/// `info` 说「机身内嵌 JPEG 预览（真实 8 位）」，`read` 交回的却是 16 位传感器数据
/// （CR3 连尺寸都不一样：6960×4640 对 6982×4654）。两处各有各的分支判断——`info`
/// 只问内置解析器，`read` 先问 rawler——于是必然漂移。
///
/// 用户看到的是「这张 RAW 没有余量」，于是不会去修，而实际数据是好的；`dcp` 还据此
/// 误拒了相机配置（「这一帧走的是机身内嵌 JPEG 预览，套不了相机配置」）。所以这条
/// 判据不问「结果对不对」，只问「两处说的是不是同一件事」——后者是前者的前提。
///
/// # 断的是不变量，不是某台相机
///
/// * `info` 成功 ⇒ `read` 成功，路径相同，宽高逐项相等；
/// * `info` 失败 ⇒ `read` 也必须失败（拒绝口径可能不同，但「解得开/解不开」不许分歧）。
///
/// 宽高按**未转正**的口径比（`raw::read` 交的像素就是传感器排布），竖拍不会被方向码
/// 拌住。
///
/// `raw::info` and `raw::read` must reach the **same conclusion** about a sample: which
/// route, what size, which camera.
///
/// # Where this assertion comes from
///
/// On 2026-11-04 two routes were printed side by side for the first time: a Canon CR3 and
/// four Panasonic RW2 files where `info` said "camera embedded JPEG preview (truly 8-bit)"
/// while `read` handed back 16-bit sensor data (the CR3 even disagreed on size:
/// 6960x4640 vs 6982x4654). Each had its own branch logic -- `info` only asked the built-in
/// parser, `read` asked rawler first -- so they were bound to drift.
///
/// The user sees "this RAW has no headroom" and never edits it, while the data is fine; `dcp`
/// also refused camera profiles because of it. So this assertion does not ask "is the answer
/// right" but "do the two say the same thing" -- which is the precondition for the former.
///
/// # What it pins is an invariant, not one camera
///
/// * `info` succeeds => `read` succeeds, same route, identical width and height;
/// * `info` fails => `read` must fail too (the wording may differ, "decodable or not" may not).
///
/// Sizes are compared **unrotated** (`raw::read` hands back the sensor layout), so portrait
/// frames are not tripped up by the orientation code.
#[test]
fn info_and_read_agree_on_every_camera_sample() {
    use aevum_core::raw;

    let files: Vec<PathBuf> = samples()
        .into_iter()
        .filter(|p| raw::is_raw_name(p))
        .collect();
    if files.is_empty() {
        eprintln!(
            "跳过：{} 里没有 RAW 样张（样张不入库，清单见该目录 README）",
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
        // 两条路各自包住：两边读的都是不可信字节，谁 panic 都该红在本条而不是掀翻进程。
        // Each route is wrapped: both read untrusted bytes, and a panic must fail here
        // instead of taking the process down.
        let info = std::panic::catch_unwind(AssertUnwindSafe(|| raw::info(path)));
        let read = std::panic::catch_unwind(AssertUnwindSafe(|| raw::read(path)));

        match (info, read) {
            (Err(_), _) => panic!("{name}: raw::info panic 了"),
            (Ok(_), Err(_)) => panic!("{name}: raw::read panic 了"),
            (Ok(Err(_)), Ok(Err(_))) => {
                report.push_str(&format!("{name}: 两条路都拒绝\n"));
            }
            (Ok(Err(e)), Ok(Ok((img, route)))) => panic!(
                "{name}: info 说「解不开」（{e}），read 却交回 {route:?} {}×{}——两条路的\
                 判断已经分叉，用户看到的保真度不是他手里的数据",
                img.width(),
                img.height()
            ),
            (Ok(Ok(i)), Ok(Err(e))) => panic!(
                "{name}: info 说能解（{:?} {}×{}），read 却失败（{e}）——两条路分叉",
                i.path, i.width, i.height
            ),
            (Ok(Ok(i)), Ok(Ok((img, route)))) => {
                assert_eq!(
                    i.path,
                    route,
                    "{name}: info 报 {:?}（{}×{}），read 交回 {route:?}（{}×{}）——同一条文件\
                     两条路各说一套；这正是 2026-11-04 那个「CR3/RW2 被报成 8 位预览」的缺陷",
                    i.path,
                    i.width,
                    i.height,
                    img.width(),
                    img.height()
                );
                // 未转正口径：`raw::read` 交的是传感器排布，与 `info` 的宽高同一个坐标系。
                assert_eq!(
                    (i.width, i.height),
                    (img.width(), img.height()),
                    "{name}: 尺寸对不上——info 报 {}×{}，read 解出 {}×{}，用户看到的尺寸与\
                     拿到的像素不是一个东西",
                    i.width,
                    i.height,
                    img.width(),
                    img.height()
                );
                report.push_str(&format!("{name}: {:?} {}×{}\n", i.path, i.width, i.height));
            }
        }
    }
    eprintln!("两条路一致性（{} 张）：\n{report}", files.len());
}

/// 每张样张的期望口径必须**登记在表里**，未登记的新样张一律红。
///
/// # 为什么需要一张表
///
/// 上面两条判据只保证「具名」和「两处一致」。它们抓不到最要紧的一种退化：某台原本
/// 拿得到传感器数据的机身，**两处一起**退成了内嵌预览。那时两条判据都还是绿的
/// （具名：`Preview` 也行；一致：两处都说 `Preview`），而用户手里的 16 位余量已经没了。
///
/// 所以这里登记「这台机身应该拿到什么」。上游 rawler 换版本、我们改路由、加了新
/// 样张——只要结果与表不符就红，逼着人来判断「是修好了还是坏了」，而不是让退化静静
/// 发生。C8 的纪律「新增格式/机型：先让矩阵判据红一次」在这里变成了机制：新样张不
/// 填表本身就是红。
///
/// 表里登记的尺寸是**未转正**的有效区尺寸（与 `raw::info`/`raw::read` 同一坐标系）。
/// 拒绝类不给尺寸。
///
/// Every sample's expected verdict must be **registered**, and an unregistered new sample is
/// always red.
///
/// # Why a table is needed
///
/// The two assertions above only guarantee "named" and "the two agree". They cannot catch the
/// degradation that matters most: a body that used to give sensor data **degrades on both
/// routes at once** to the embedded preview. Both assertions would still be green (named:
/// `Preview` counts; consistent: both say `Preview`) while the user's 16-bit headroom is gone.
///
/// So this table registers "what this body should give". A rawler upgrade, a routing change or
/// a new sample that does not match goes red and forces a human decision -- "fixed or broken?"
/// -- instead of letting degradation happen quietly. C8's rule ("a new format or body: make the
/// matrix go red first") becomes mechanical here: an unfilled row *is* the red.
///
/// Sizes are **unrotated** active-area sizes (same coordinate system as `raw::info`/
/// `raw::read`). Rejections carry no size.
#[test]
fn every_camera_sample_matches_the_registered_verdict() {
    use aevum_core::raw;

    /// 期望口径。`Sensor`/`Preview` 必须给尺寸；`Reject` 必须不给。
    ///
    /// The expected verdict. `Sensor`/`Preview` must carry a size; `Reject` must not.
    struct Expect {
        file: &'static str,
        route: Option<raw::RawPath>,
        size: Option<(u32, u32)>,
        /// 为什么是这条路——半年后回来看这一行的人要先知道原因。
        ///
        /// Why this route -- whoever reads this row in six months needs the reason first.
        why: &'static str,
    }

    const TABLE: &[Expect] = &[
        Expect {
            file: "Canon - EOS R6 Mark III - RAW (3_2).CR3",
            route: Some(raw::RawPath::Sensor),
            size: Some((6982, 4654)),
            why: "CR3 的 CRX 压缩只有 rawler 解得动；退预览就等于丢掉整个传感器的余量",
        },
        Expect {
            file: "IMG_1159.HEIC",
            route: None,
            size: None,
            why: "HEIC 这一版不做，按后缀具名拒绝（UNSUPPORTED_EXTENSIONS）",
        },
        Expect {
            file: "IMG_1381.DNG",
            route: Some(raw::RawPath::Preview),
            size: Some((4032, 3024)),
            why: "iPhone 的 LinearRaw 让 rawler 内部越界（已自兜），内置解析器只认单通道 CFA，只能退内嵌预览",
        },
        Expect {
            file: "IMG_9590.ARW",
            route: None,
            size: None,
            why: "不是 TIFF 家族容器（NotTiff），rawler 也认不出机型——缩略图仍能从容器里的整段 JPEG 出",
        },
        Expect {
            file: "Nikon - Z6_3 - 8bit compressed (3_2).NEF",
            route: Some(raw::RawPath::Preview),
            size: Some((3984, 2656)),
            why: "Nikon HighEfficiencyStar 压缩 rawler 0.8.0 还不支持，退内嵌预览",
        },
        Expect {
            file: "PANA0001.RW2",
            route: Some(raw::RawPath::Sensor),
            size: Some((6000, 4000)),
            why: "主流 RW2，rawler 传感器直解",
        },
        Expect {
            file: "PANA0198.JPG",
            route: None,
            size: None,
            why: "普通 JPEG，不走 RAW 支路（归 JPEG 判据管）",
        },
        Expect {
            file: "PANA0450.RW2",
            route: Some(raw::RawPath::Sensor),
            size: Some((6000, 4000)),
            why: "主流 RW2，rawler 传感器直解",
        },
        Expect {
            file: "PANA1631.RW2",
            route: Some(raw::RawPath::Sensor),
            size: Some((6000, 4000)),
            why: "主流 RW2，rawler 传感器直解（竖拍，尺寸仍未转正）",
        },
        Expect {
            file: "PANA180.RW2",
            route: Some(raw::RawPath::Sensor),
            size: Some((6000, 4000)),
            why: "主流 RW2，rawler 传感器直解",
        },
        Expect {
            file: "Sony - ILCE-7M5 - 14bit (3_2).ARW",
            route: Some(raw::RawPath::Preview),
            size: Some((4608, 3072)),
            why: "arw6 机型配置还没有（rawler 报 Unknown camera, mode arw6），退内嵌预览",
        },
    ];

    let files = samples();
    if files.is_empty() {
        eprintln!(
            "跳过：{} 里没有样张（样张不入库，清单见该目录 README）",
            sample_dir().display()
        );
        return;
    }

    // 反查：先看有没有「目录里有、表里没有」——那是新样张，必须先把表填上。
    // Reverse check first: "present but unregistered" is a new sample and the table must come
    // first.
    for path in &files {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        assert!(
            TABLE.iter().any(|e| e.file == name),
            "{name}: 这张样张没登记期望口径。先跑 `cargo run -p aevum-core --example \
             camera_matrix` 抄一行进 TABLE，再判断它是「该能解」还是「这一版不做」。\n\
             不登记就没人能发现它退化了——两台相机都说「预览」，判据全是绿的。"
        );
    }

    for exp in TABLE {
        let Some(path) = files.iter().find(|p| {
            p.file_name()
                .is_some_and(|n| n == std::ffi::OsStr::new(exp.file))
        }) else {
            // 样张不入库，换台机器就少几张——这是正常状态，不是失败。
            // Samples are not committed; a fresh machine has fewer of them. Normal, not a
            // failure.
            eprintln!("跳过 {}：本机没有这张样张", exp.file);
            continue;
        };
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let info = raw::info(path);
            let read = raw::read(path);
            (info, read)
        }));
        let (info, read) = match outcome {
            Ok(v) => v,
            Err(_) => panic!("{}: 解码 panic 了", exp.file),
        };

        match (exp.route, exp.size) {
            (None, None) => {
                assert!(
                    info.is_err(),
                    "{}: 表里登记「拒绝」（{}），实际能解且报 {:?}——要么这一版真的补上了\
                     （该更新表和文档），要么路由把本来拒绝的东西放进来了",
                    exp.file,
                    exp.why,
                    info.map(|i| i.path)
                );
                assert!(
                    read.is_err(),
                    "{}: 表里登记「拒绝」，read 却成功了",
                    exp.file
                );
            }
            (Some(want), Some((w, h))) => {
                let info = info.unwrap_or_else(|e| {
                    panic!(
                        "{}: 表里登记 {want:?} {}×{}（{}），info 却拒绝：{e}",
                        exp.file, w, h, exp.why
                    )
                });
                assert_eq!(
                    (info.path, info.width, info.height),
                    (want, w, h),
                    "{}: 与登记口径不符（登记 {:?} {}×{}，理由：{}）——上游换版本或路由变了？\
                     先判断是修好了还是退化了，再改这张表",
                    exp.file,
                    want,
                    w,
                    h,
                    exp.why
                );
                let (img, route) = read.unwrap_or_else(|e| {
                    panic!("{}: 表里登记 {want:?}，read 却失败：{e}", exp.file)
                });
                assert_eq!(
                    (route, img.width(), img.height()),
                    (want, w, h),
                    "{}: read 与登记口径不符",
                    exp.file
                );
            }
            // 表本身写错了：一条登记要么全有要么全无。
            // The row itself is malformed: a verdict either has all fields or none.
            _ => panic!(
                "{}: TABLE 行写错了——route 与 size 必须同时给或同时不给",
                exp.file
            ),
        }
    }
}

// ---------------------------------------------------------------- 胶片条与画布同源

/// 胶片条的显影源必须与画布走同一条解码路：128px 与 1600px 的亮度只允许差一点
/// 采样噪声，不能再出现「机身内嵌 JPEG 比 RAW 亮近一档」那种整档偏差。
///
/// # 这条判据钉的是哪一次报障
///
/// 2026-11-07 用户报「小图与大图曝光不同」，问「哪个与实图不一样」。定位结果是胶片条
/// 那一侧：它抓的是机身按 As Shot 出的 8 位 JPEG（同一帧 128px 均值 78.9），画布解
/// RAW（62.0）。机身 JPEG 是「相机认为的样子」，不是这条显影链的输入。
///
/// # 两头都要钉
///
/// * 新源必须贴着画布（差值 < 4 个码值）：两边口径再分叉就红；
/// * 内嵌 JPEG 必须仍然明显更亮（> 10 个码值）：这一半证明「它确实是另一套色调」——
///   哪天有人把 `thumbnail()` 悄悄换成 RAW 解码，这条判据的前提就没了，也该有人
///   回来看一眼而不是让它绿着。
///
/// The filmstrip source must take the canvas's decoding route: 128 px and 1600 px may
/// differ by sampling noise only, never by the near-stop gap of the camera's embedded
/// JPEG (78.9 vs 62.0 mean, the user's 2026-11-07 report). Both ends are pinned: the new
/// source must stay close to the canvas, and the embedded JPEG must stay clearly brighter,
/// which is what proves the two are genuinely different tone renderings.
#[test]
fn filmstrip_source_shares_the_canvas_tone() {
    let path = sample_dir().join("PANA1631.RW2");
    if !path.is_file() {
        eprintln!("缺样张 {}，跳过", path.display());
        return;
    }
    let mean_f32 =
        |rgb: &[f32]| rgb.iter().map(|v| *v as f64).sum::<f64>() / rgb.len() as f64 * 255.0;
    let (thumb, w, h) = metrics::decode_thumb_source(&path, "", 128)
        .unwrap_or_else(|e| panic!("胶片条源解码失败：{e}"));
    assert!(w > 0 && h > 0, "胶片条源尺寸为 0");
    let (canvas, cw, ch) = metrics::decode_profile_preview(&path, "", 1600)
        .unwrap_or_else(|e| panic!("画布源解码失败：{e}"));
    assert!(cw > 0 && ch > 0, "画布源尺寸为 0");

    let (dt, dc) = (mean_f32(&thumb), mean_f32(&canvas));
    assert!(
        (dt - dc).abs() < 4.0,
        "胶片条源均值 {dt:.2} 与画布源 {dc:.2} 差得太多——两边的解码口径又分叉了\
         （目标：同一套色彩链，只差半尺寸去马赛克的采样）"
    );

    let embedded =
        metrics::thumbnail(&path, 128).unwrap_or_else(|e| panic!("内嵌预览解码失败：{e}"));
    let de = embedded
        .pixels()
        .map(|p| (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0)
        .sum::<f64>()
        / (embedded.width() as f64 * embedded.height() as f64);
    assert!(
        de - dc > 10.0,
        "内嵌 JPEG 均值 {de:.2} 与画布 {dc:.2} 几乎一样——`thumbnail()` 的取图口径变了，\
         这条判据「胶片条不能走内嵌 JPEG」的前提没了，请重新确认后再改"
    );
}
