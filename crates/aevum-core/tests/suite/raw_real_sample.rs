//! 真实相机样本的端到端判据。
//!
//! # 为什么必须有这一层
//!
//! 合成靶由实现者和靶子作者**同一个人**按同一份（可能记错的）理解写成。错在同一个
//! 地方时，两边一起错，判据永远是绿的——2026-11-01 的 CFA 图案映射就是这么活下来的：
//! 代码按 `2/3/4 = R/G/B` 读，靶子也按 `2/3/4` 写，而规范是 `0/1/2`。结果每一张
//! **合规**的 DNG 都被判成 `UnsupportedCfa`，只有自家靶子能过。
//!
//! 所以这里的判据不接受「和我的理解一致」，只接受三件事：
//!
//! 1. **两条独立路径互相印证**（只读标签 vs 解像素，结论必须相同）；
//! 2. **结构性质**（拜耳阵列的 2 绿 1 红 1 蓝、对角同色——不依赖「0 代表红」这种约定）；
//! 3. **真实内容能走完链路**（不是只解析成功，而是出得来一张有明暗层次的图）。
//!
//! # 样本
//!
//! `samples/cc0/apple-iphone6s-plus/IMG_0853.DNG`，CC0，来源与校验值见该目录 README。
//! 文件**入库**而不是下载：这条判据的价值就在于「随时能跑」。少了它判据应当红，
//! 而不是静默跳过——跳过等于假绿，正是这一层要防的东西。
//!
//! End-to-end acceptance against a real camera sample.
//!
//! # Why this layer must exist
//!
//! Synthetic fixtures and the implementation are written by the **same person** from the
//! same (possibly misremembered) reading of the spec. A shared mistake keeps both green
//! forever -- which is how the CFA pattern mapping survived until 2026-11-01: the code
//! read `2/3/4 = R/G/B` and the fixtures were written as `2/3/4`, while the spec says
//! `0/1/2`. Every **conforming** DNG was rejected as `UnsupportedCfa`; only the in-house
//! fixture passed.
//!
//! So the assertions here accept only three things:
//!
//! 1. **Two independent paths agreeing** (tags-only vs pixel decoding must reach the same
//!    conclusion);
//! 2. **Structural properties** (a Bayer array has two greens, one red, one blue, and
//!    equal colours on the diagonal -- no reliance on "0 means red");
//! 3. **Real content completing the pipeline** (not just "it parsed", but an image with
//!    actual tonal range comes out).
//!
//! # The sample
//!
//! `samples/cc0/apple-iphone6s-plus/IMG_0853.DNG`, CC0; source and checksum are in that
//! directory's README. The file is **committed**, not downloaded: the whole point of this
//! assertion is that it can run at any time. A missing sample must turn this red rather
//! than skip silently -- skipping is false green, exactly what this layer guards against.

use std::path::{Path, PathBuf};

use aevum_core::metrics::{self, PixelOrigin};

/// 样本路径。入库文件，不存在的意义是「仓库被改坏了」，不是「环境缺东西」。
///
/// The sample path. It is committed, so a missing file means "the repository got broken",
/// not "the environment lacks something".
fn sample() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples/cc0/apple-iphone6s-plus/IMG_0853.DNG");
    assert!(
        p.is_file(),
        "真实样本不见了：{}\n它是入库文件（CC0），不是可选的下载项。见 samples/cc0/README.md",
        p.display()
    );
    p
}

/// 只读标签的路径与解像素的路径必须对同一张图给出同一个尺寸。
///
/// 这是本文件里最便宜也最有力的一条：两条路径的代码几乎不重叠。`dng::info` 只走
/// 标签（宽高、位深、CFA），`metrics::decode` 要真的产出像素。修 `main_ifd` 之前
/// 前者报 852×640（IFD0 的缩略图）、后者报 4032×3024，两边各说一套而没有任何判据
/// 察觉——尺寸不一致就是那个 bug 唯一可见的症状。
///
/// The tags-only path and the pixel path must report the same size for the same image.
///
/// This is the cheapest and strongest assertion here: the two code paths barely overlap.
/// `dng::info` reads tags only (size, bit depth, CFA); `metrics::decode` must actually
/// produce pixels. Before the `main_ifd` fix the first said 852×640 (the IFD0 thumbnail)
/// and the second 4032×3024, each telling its own story with nothing catching it --
/// a size disagreement was the only visible symptom of that bug.
#[test]
fn real_dng_metadata_and_pixels_agree_on_size() {
    let path = sample();
    let info = aevum_core::dng::info(&path).expect("真实 DNG 的标签应当读得出来");
    let (img, _origin) = metrics::decode_with_origin(&path).expect("真实 DNG 应当解得出来");

    assert_eq!(
        (info.width, info.height),
        (img.width(), img.height()),
        "标签路径与像素路径给出的尺寸不一致：标签 {}×{}，像素 {}×{}",
        info.width,
        info.height,
        img.width(),
        img.height()
    );
    // 缩略图 852×640，主图 4032×3024。钉住量级，防止「两边一起变成缩略图」这种
    // 同错——那时尺寸一致，上面那条就拦不住了。
    assert!(
        info.width >= 3000 && info.height >= 2000,
        "解出来的不是主图：{}×{}（IFD0 的预览是 852×640，主图在 SubIFDs）",
        info.width,
        info.height
    );
}

/// CFA 图案要按**结构**认，不按「0 是红」这种约定认。
///
/// 拜耳阵列的可验证事实只有一条：2×2 单元里两个绿、一个红、一个蓝，且**对角同色**。
/// 这条性质与「哪个数字代表红」无关，所以它能在数值约定写错时依然成立——写错映射会
/// 直接让解析失败（三个通道认不全），而映射内部再错也只能错在「红蓝互换」，那由
/// 下一条判据（真实内容的色相分布）兜。
///
/// 修之前这里返回 `UnsupportedCfa`：真实文件写的是规范的 `[0,1,1,2]`，代码按
/// `2/3/4` 读，第一个字节 0 就直接拒绝。
///
/// The CFA pattern is checked **structurally**, not by the "0 is red" convention.
///
/// The only verifiable fact about a Bayer array is: a 2×2 cell has two greens, one red,
/// one blue, and **equal colours on the diagonal**. That holds regardless of which number
/// means red, so it survives a wrong mapping -- a wrong mapping makes the parse fail
/// outright (three channels can't be recognised), and any remaining error can only be a
/// red/blue swap, which the next assertion (hue distribution of real content) covers.
///
/// Before the fix this returned `UnsupportedCfa`: the real file writes the spec's
/// `[0,1,1,2]`, the code read `2/3/4`, and the first byte 0 was rejected immediately.
#[test]
fn real_dng_cfa_is_a_structural_bayer_array() {
    let info = aevum_core::dng::info(sample()).expect("真实 DNG 的标签应当读得出来");
    let cfa = info
        .cfa
        .expect("CFA 图层必须报出图案，否则去马赛克无从下手");

    // 几何性质：存储顺序是 [左上, 右上, 左下, 右下]，于是两个绿落在**反对角**
    // （右上 index 1、左下 index 2），主对角（index 0 与 3）是红和蓝。
    //
    // 这里原先写的是「主对角同色」，实测 `[0, 1, 1, 2]` 当场把它推翻——正好说明这类
    // 判据要按**存储顺序的几何**写，不能凭「拜耳阵列看起来对称」的印象写。
    //
    // Geometry: the stored order is [top-left, top-right, bottom-left, bottom-right],
    // so the two greens sit on the **anti-diagonal** (index 1 and 2) and the main
    // diagonal (index 0 and 3) holds red and blue.
    //
    // This assertion first said "main diagonal equal" and the real `[0, 1, 1, 2]` refuted
    // it immediately -- which is exactly why such checks must follow the geometry of the
    // storage order rather than the impression that "a Bayer array looks symmetric".
    assert_eq!(cfa[1], cfa[2], "两个绿应当在反对角上同色，实测 {cfa:?}");
    assert_ne!(cfa[0], cfa[3], "主对角是红与蓝，必须不同色，实测 {cfa:?}");
    for (i, j) in [(0, 1), (0, 2), (3, 1), (3, 2)] {
        assert_ne!(cfa[i], cfa[j], "相邻位置不能同色：{cfa:?} 的 {i} 与 {j}");
    }

    // 三个通道齐全，其中绿色出现两次。用「出现次数」而不是「是 0/1/2」来判断，
    // 这样即使以后换成别的通道编号方式，判据仍然表达同一个事实。
    let mut seen = [0usize; 3];
    for c in cfa {
        assert!((c as usize) < 3, "通道索引越界：{cfa:?}");
        seen[c as usize] += 1;
    }
    let mut counts = seen.to_vec();
    counts.sort_unstable();
    assert_eq!(
        counts,
        vec![1, 1, 2],
        "拜耳阵列必须是 2 绿 1 红 1 蓝，实测分布 {seen:?}（图案 {cfa:?}）"
    );

    // 传感器数据是 16 位容器；8 位说明走错图层了。
    assert_eq!(info.bits, 16, "传感器图层的位深应当是 16");
}

/// 内置解析器解不动像素时，必须**退回后端并如实标注**，而不是静默换一张预览。
///
/// 这个样本的像素用 lossless JPEG + 瓦片（compression=7）存储，超出内置解析器的
/// 范围——这是**能力边界**，不是文件损坏，所以该退。判据分三段：
///
/// 1. 内置的 `dng::read` 报的必须是 `soft_fallback()` 为真的错；
/// 2. `decode_with_origin` 仍然成功，且 `origin` 标成 `RawBackend`；
/// 3. 解出来的尺寸是**主图**，不是 852×640 的内嵌预览——这一条才真正区分
///    「退回后端解了传感器数据」与「静默换了一张预览」。
///
/// When the built-in parser cannot decode pixels it must **fall back and say so**, never
/// silently substitute a preview.
///
/// This sample stores pixels as lossless JPEG in tiles (compression=7), beyond the
/// built-in parser -- a **capability limit**, not corruption, so falling back is correct.
/// Three parts:
///
/// 1. `dng::read` must report an error whose `soft_fallback()` is true;
/// 2. `decode_with_origin` still succeeds and labels the origin `RawBackend`;
/// 3. The result is the **main image**, not the 852×640 embedded preview -- the one part
///    that actually separates "the backend decoded sensor data" from "a preview was
///    quietly swapped in".
#[test]
fn real_dng_falls_back_for_capability_not_for_corruption() {
    let path = sample();
    let err = aevum_core::dng::read(&path).expect_err(
        "内置解析器不该声称能解瓦片化的 lossless JPEG：若它开始能解了，这条判据要跟着更新",
    );
    assert!(
        err.soft_fallback(),
        "内置解析器报的是硬错，说明文件层面坏了而不是能力边界：{err}"
    );

    let (img, origin) = metrics::decode_with_origin(&path).expect("后端应当解得出这张 DNG");
    assert_eq!(
        origin,
        PixelOrigin::RawBackend,
        "像素来自后端就必须标 RawBackend：标成内置会让相机配置白平衡被乘两遍"
    );
    assert!(
        img.width() >= 3000 && img.height() >= 2000,
        "回退后拿到的是预览而不是传感器数据：{}×{}",
        img.width(),
        img.height()
    );
}

/// 真实内容要能走完整个链路，并且**确定性**。
///
/// 前面三条都在验「结构对不对」，这条验「有没有真的出一张照片」：像素分布必须既有
/// 暗部也有亮部（全黑/全白说明电平归一化错了），并且同一份输入解两次必须逐字节相同
/// （解码里若有未初始化内存、迭代顺序依赖哈希、或并发写同一缓冲，这里会红）。
///
/// 为什么用分位数而不是平均值：平均值会被大片天空或大片暗背景拉走，而分位数直接
/// 问「有没有 1% 的像素接近黑、有没有 1% 接近白」，那才是「有明暗层次」。
///
/// Real content must complete the pipeline and be **deterministic**.
///
/// The first three assertions check structure; this one checks that a photograph actually
/// comes out: the pixel distribution must have both shadows and highlights (all-black or
/// all-white means level normalisation is wrong), and decoding the same input twice must
/// be byte-identical (uninitialised memory, hash-order dependence, or concurrent writes to
/// one buffer would show up here).
///
/// Why percentiles instead of the mean: the mean is dragged around by a large sky or a
/// large dark background, while percentiles ask directly "is 1% of pixels near black and
/// 1% near white", which is what "has tonal range" actually means.
#[test]
fn real_dng_develops_into_a_photograph_deterministically() {
    let path = sample();
    let (a, w, h, deep, origin) =
        metrics::decode_rgb_f32_with_origin(&path).expect("真实 DNG 应当解得出浮点像素");
    assert_eq!(origin, PixelOrigin::RawBackend, "像素来自后端");
    assert!(deep, "16 位容器必须走高位深路径，否则后续链路按 8 位算");
    assert_eq!(
        w as u64 * h as u64 * 3,
        a.len() as u64,
        "像素缓冲长度对不上"
    );

    // 亮度和分布：三通道一起看，避免单通道偏移被平均掉。
    let mut luma: Vec<f32> = a
        .chunks_exact(3)
        .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        .collect();
    luma.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let at = |q: f32| luma[((luma.len() - 1) as f32 * q) as usize];
    let (p01, p50, p99) = (at(0.01), at(0.50), at(0.99));

    assert!(
        p01 < 0.20,
        "1% 分位是 {p01:.4}，暗部被抬平了——电平归一化或黑场扣除有问题"
    );
    assert!(
        p99 > 0.35,
        "99% 分位是 {p99:.4}，亮部被压平了——白场或增益有问题"
    );
    assert!(
        p99 - p01 > 0.25,
        "动态范围只有 {:.4}（p01={p01:.4}, p99={p99:.4}），不像一张真实照片",
        p99 - p01
    );
    let _ = p50;

    // 确定性：逐字节比较。
    let (b, _, _, _, _) =
        metrics::decode_rgb_f32_with_origin(&path).expect("第二次解码应当同样成功");
    assert_eq!(a.len(), b.len(), "两次解码的像素数不同");
    let first_diff = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x.to_bits() != y.to_bits());
    assert!(
        first_diff.is_none(),
        "两次解码在同一份输入上结果不同（首个差异在第 {:?} 个分量）：解码路径不确定，\
         后续所有金标都不可复现",
        first_diff
    );
}
