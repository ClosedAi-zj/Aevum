//! 金字塔合成：Burt–Adelson 拉普拉斯塔上的多尺度焦点融合。
//!
//! # 为什么是「多尺度的 Weighted」
//!
//! [`crate::focus::Method::Weighted`] 用**一个**权重面把整帧混一遍：权重在纹理处很陡，
//! 但低频（亮度底座、大面积渐变）也跟着同一个权重走，于是纹理边界外会出现反转光晕。
//! 金字塔把融合拆到不同尺度上：细层只决定「这一小块细节从哪帧来」，粗层只决定
//! 「底色是多少」，互不串扰。`W_l = GaussPyr(m^4)` 与 Weighted 用的是同一个测度、
//! 同一个四次方，所以 `levels = 1`（塔里只有一层、没有下采样）时必须**严格**退化成
//! Weighted——这既是实现自检锚，也是 `P-C1` 判据。
//!
//! # 为什么最粗两层等权
//!
//! 连续 α(l) 收紧曲线没有判据可锚、不可复现。这里改成分段常量：最粗的两层用
//! `1/count`，其余层用 `m^4` 归一。等权让最粗层退化成「全帧平均底图」，掐断低频层
//! 的高频扩散（光晕的主源），也让常数场塌缩天然成立。`levels < 3` 时只有一层能收紧，
//! `levels = 1` 时一层都不收紧（否则会破坏退化一致性）。
//!
//! # 内存与耗时（T10 实测，2026-11-03）
//!
//! 逐帧流式：**按层下采样 → 边算 L_l 边融进输出塔 → 只留相邻两层**，绝不把 32 帧
//! 的完整塔一起驻留。塌缩从最粗层起**原地**逐层上采样累加，不另开全分辨率缓冲。
//!
//! 实测口径：release 构建、24 MP（6000×4000）× 8 帧、`levels = auto = 6`、
//! 同一台机器（i5-12600KF / 31.8 GB RAM / Windows）各跑 3 次取中位数，峰值取进程
//! `PeakWorkingSetSize`。输入 8 帧本身驻留 2.32 GB（8 × 288 MB + 解码器余量），表里
//! 「融合增量」是峰值减去融合开始前的常驻，才是本法自己的开销。
//!
//! | 方法 | 墙钟 | 进程峰值 | 融合增量 |
//! |---|---|---|---|
//! | Weighted | 1.95 s | 2.88 GB | 732 MB |
//! | Pyramid（优化前） | 29.7 s | 4.43 GB | **2.10 GB** |
//! | Pyramid（优化后·单线程轮） | 24.4 s | 4.43 GB | **2.10 GB** |
//! | Pyramid（优化后·多线程轮） | 5.97 s | 4.14 GB | **1.82 GB** |
//! | Pyramid（优化后·测度链一遍轮） | **4.05 s** | **3.86 GB** | **1.54 GB** |
//! | 优化收益 | 本轮墙钟 −28%（5.60 → 4.05 s，同轮 Weighted 0.95 s，倍数 5.9× → 4.3×） | −6.7% | −15% |
//!
//! 优化三件事（2026-11-03 单线程轮）：① `focus::measure_peak`——逐帧每层只算一遍改进
//! 拉普拉斯取逐像素最大再盒平均，Pass A 的分位线改由子采样峰值估计。`t = 0` 时与旧路径
//! 逐像素严格等价（第四轮回归实测：128×128 夹具 49152/49152 像素 max|Δ| = 0）；`t > 0`
//! 时**不再等价**——`box(max ml)` 与 `max(box ml)` 不可交换，分位线也取自不同分布：
//! 纹理尺度与盒窗（radius = 2）可比时差异可观（块状纹理夹具 threshold = 25 实测 65%
//! 像素不同、max|Δ| = 0.23），纹理细于窗宽时趋近零；② `reduce` 重写为可分离行流式
//! 加环形缓冲，消掉每输出像素两次 `collect()` 堆分配；③ Pass B 权重面 `.clone()` 改
//! `Cow::Borrowed`，省掉每帧每层一次 128 MB 纯拷贝。
//!
//! 多线程一轮（2026-11-03 同日第三轮）：把「同层内逐像素独立」的运算全部按行带并行
//! （`par.rs`，`std::thread::scope`，零新依赖）：测度链（γ2.0 亮度、改进拉普拉斯、
//! 盒平均输出趟、跨帧取最大）、`reduce`（每带自带 5 行环形缓冲，带界重复算一两行）、
//! `accumulate_upsample`、`laplacian_from_gaussian`（**逐像素融合**上采样与减法，省掉
//! 一张 288 MB 全分辨率中间面与一整面写+读）、`accumulate_level`、Pass B 的权重四次方。
//! 帧循环保持串行（out_tower 的 f32 累加顺序不动），积分图构建、直方图分位、塌缩的
//! 层间顺序也不动——每个输出像素的浮点加法序列与串行版逐一相同，**全程逐位不变**：
//! 探针 `T10_DUMP` 改前改后各 288 MB 逐字节 `cmp` 相同，Weighted 校验和不变。峰值内存
//! **反而下降**（4.43 → 4.14 GB）：并行省的不只是时间，融合拉普拉斯少了一张中间面，
//! 驻留基线也跟着降了 288 MB。多线程轮的 Weighted 基线复测为 2.285 s / 3.09 GB /
//! 768 MB（机器抖动），优化后 Weighted 自己也降到 1.31 s——测度链并行它同样受益；
//! 倍数一律按**同轮**基线计。
//!
//! 各轮实测的 Weighted 基线有抖动（优化前那轮 1.97 s / 3.19 GB / 864 MB，单线程轮
//! 1.95 s / 2.88 GB / 732 MB，多线程轮 2.285 s / 3.09 GB / 768 MB，本轮 0.95 s /
//! 3.09 GB / 768 MB），倍数一律按**同轮**基线计。早先按「输出塔 384 MB +
//! 权重塔 128 MB」推算的 ≈1.0 GB **是错的**（差 2 倍多）：那个算式漏了 Pass A 的
//! `best` / `gate` 两个全分辨率面（各 96 MB）、掩膜塔（128 MB）、每帧的测度面（96 MB）
//! 与当前帧的拉普拉斯面（288 MB）。这些数都是实测值，不是推算值；改实现就重测，
//! 别照着旧数字改注释。
//!
//! 测度链一遍轮（2026-11-03 同日第四轮）：Pass A 与 Pass B 曾各自把整条测度链
//! （亮度 → 改进拉普拉斯 → 积分图 → 盒平均）跑一遍，每帧两遍。现在 Pass A 一趟同时
//! 产出峰值面（跨帧取 ml 最大，最后一次盒平均，求分位线）和每帧**盒平均后**的测度面
//! 缓存，Pass B 直接取用、用完即释放（8 帧 × 96 MB，随帧衰减）。同时：① 第 0 层不再
//! `from_rgb` 整幅拷贝，`reduce` / 拉普拉斯直接借用帧数据；② 逐层 scratch（高斯面、
//! 权重面、最细层权重 w0、tap 表）预分配一次跨帧复用；③ 拉普拉斯**不再落盘**，
//! `fine − upsample(coarse)` 在 `accumulate_laplacian_level` 处逐像素现算（f32 减法
//! 两版都只舍入一次，逐位等价），省掉每帧每层一张面的写+读；④ gate / weak 两次全图
//! 遍历合成一次，weak 面直接由 `masks[0]`（即 gate 本身）替代；⑤ 逐层上/下采样 tap 表
//! 每层只建一次。分层实测（同一轮、min-of-N）：levels=1 1.21 s、levels=2 3.35 s、
//! levels=6 4.05 s——「进入多尺度的固定开销」（levels=2 − levels=1）从 4.11 s 压到
//! **2.14 s**。峰值内存 4.14 → **3.86 GB**：缓存加了 768 MB，但测度链 churn、gate/weak/
//! best 三张常驻面、掩膜塔 level-0 克隆与整张拉普拉斯面省掉了更多。逐位不变：探针
//! `T10_DUMP` 改前改后各 288 MB 逐字节 `cmp` 相同，checksum 不变。
//! 代价说明：Pass B 期间测度缓存驻留最高约 7×96 MB，是峰值内存的主要新增项；若
//! 再压内存，只能减缓存面或换编码（f16 不满足逐位要求），无免费午餐。
//!
//! 第五轮（同日）：先打点拿分段构成（同轮 3 次取中位，中位 5.01 s，本机当轮比上表慢：
//! 同轮 Weighted 1.58 s）：Pass A 0.84 s（其中 build_sat 0.27 s）、第 0 层 2.41 s、
//! 深层 0.86 s、塌缩 0.77 s、归一 0.06 s。两个候选杠杆都实测掉了：
//! ① tap 表扁平化——同机交替 A/B 两轮、各 3 次取中位，旧 4.972 / 4.576 s 对新
//! 4.901 / 4.698 s，**噪声级中性**（输出校验和逐位相同），已回退；
//! ② build_sat 列带并行——0.20–0.24 → 0.33–0.35 s，**负收益**，已回退（原因见
//! `build_sat` 注释）。第 0 层 2.41 s 的结构性原因是 `accumulate_laplacian_level`
//! 每通道一条 30 项串行加法链，受 f32 加法延迟（4 cycle）限制而非带宽：24 MP×8 帧
//! ÷ 10 线程 × 每像素约 360 cycle ≈ 2.3 s，与实测吻合。**跨像素条带展开已实施并
//! 实测**（每像素自己的加法顺序不变，两个相邻像素 × 3 通道 = 6 条独立链同时在飞）：
//! 同轮同机、Weighted 1.619 s 作基线，旧版 4.349 s（2.69×）→ 新版 **3.375 s
//! （2.08×）**，提速 **22.4%**，输出校验和 10682679.462086 逐位相同；≤3× 基线达标。
//!
//! Round five (same day): stage instrumentation first (median of 3 in one session; the
//! machine was slower than the table above -- same-session Weighted was 1.58 s): Pass A
//! 0.84 s (build_sat 0.27 s of it), level 0 2.41 s, deep levels 0.86 s, collapse 0.77 s,
//! normalise 0.06 s. Both candidate levers were settled by measurement:
//! ① flattening the tap tables -- two interleaved A/B rounds on one machine, median of 3
//! each, 4.972 / 4.576 s (old) against 4.901 / 4.698 s (new), **neutral within noise**
//! (output checksum bitwise identical), reverted;
//! ② column-band parallel build_sat -- 0.20-0.24 -> 0.33-0.35 s, a **net loss**, reverted
//! (reason in the `build_sat` docs). Level 0's 2.41 s is structurally a 30-term serial
//! f32 addition chain per channel in `accumulate_laplacian_level`, bound by add latency
//! (4 cycles) rather than bandwidth: 24 MP x 8 frames / 10 threads x ~360 cycles per
//! pixel is about 2.3 s, matching the measurement. **Pixel-strip unrolling is implemented
//! and measured** (each pixel keeps its own addition order; two adjacent pixels x 3
//! channels = six independent chains run side by side): same session and machine, with
//! Weighted at 1.619 s as the baseline, the old code took 4.349 s (2.69x) and the new
//! code **3.375 s (2.08x)** -- **22.4% faster**, output checksum 10682679.462086 bitwise
//! identical; the <=3x baseline is met.
//!
//! 历史注：多线程轮之前，本法慢主要慢在 `focus::measure_plane` 每帧跑两遍（Pass A 求
//! 分位线、Pass B 求权重）；本轮起每帧只跑一遍，见上。
//!
//! `feather` / `smooth_z` 在本法下**不适用**：融合只在塔各层上按权重归一，没有
//! 深度跳变、没有羽化掩膜，也没有模值滤波。调用方传了也只读校验、不生效。
//!
//! # Pyramid fusion
//!
//! Burt-Adelson Laplacian fusion on multiple scales.
//!
//! # Why it is "multi-scale Weighted"
//!
//! [`crate::focus::Method::Weighted`] blends the whole frame with **one** weight plane. The
//! weight is steep at texture, but low frequencies (the base tone, broad gradients) follow
//! that same weight, which produces inversion halos just outside texture boundaries. A
//! pyramid splits the blend across scales: fine levels decide which frame supplies a detail,
//! coarse levels decide the base tone, and neither leaks into the other. `W_l =
//! GaussPyr(m^4)` uses the very same measure and the very same fourth power as Weighted,
//! so `levels = 1` (one level, no downsampling) must **strictly** degenerate to Weighted --
//! both an implementation anchor and assertion `P-C1`.
//!
//! # Why the two coarsest levels use equal weights
//!
//! A continuous alpha(l) tightening curve has no assertion to anchor it and is not
//! reproducible. The constant-segment rule is: the two coarsest levels use `1/count`, all
//! other levels use normalised `m^4`. Equal weights turn the coarsest level into a
//! whole-frame average base, cutting the high-frequency spread at low frequencies (the
//! main source of halos) and making the constant-field collapse hold by construction. With
//! `levels < 3` only one level is tightened; with `levels = 1` none is, or the degeneration
//! would break.
//!
//! # Memory and time (measured for T10, 2026-11-03)
//!
//! Streaming, one frame at a time: reduce level by level, compute `L_l` and fold it into the
//! output tower immediately, keeping only two adjacent planes. The full towers of all 32
//! frames are never resident. Collapse runs **in place** from the coarsest level, with no
//! extra full-resolution buffer.
//!
//! Measured how: release build, 24 MP (6000x4000) x 8 frames, `levels = auto = 6`, one
//! machine (i5-12600KF / 31.8 GB RAM / Windows), three runs each, median; peak is the
//! process's `PeakWorkingSetSize`. The eight input frames alone stay resident at 2.32 GB
//! (8 x 288 MB plus decoder slack); the "fusion delta" column is the peak minus what was
//! already resident when fusion started, i.e. this method's own cost.
//!
//! | Method | Wall clock | Process peak | Fusion delta |
//! |---|---|---|---|
//! | Weighted | 1.97 s | 3.19 GB | 864 MB |
//! | Pyramid (before) | 29.7 s | 4.43 GB | **2.10 GB** |
//! | Pyramid (after, single-thread round) | 24.4 s | 4.43 GB | **2.10 GB** |
//! | Pyramid (after, multithread round) | 5.97 s | 4.14 GB | **1.82 GB** |
//! | Pyramid (after, measure-chain-once round) | **4.05 s** | **3.86 GB** | **1.54 GB** |
//! | Gain | this round −28% wall (5.60 → 4.05 s, same-session Weighted 0.95 s, ratio 5.9x → 4.3x) | −6.7% | −15% |
//!
//! The single-thread round did three things (2026-11-03): ① `focus::measure_peak` -- the
//! modified Laplacian once per frame per level, per-pixel max, then one box average, with
//! Pass A's percentile cut estimated from the subsampled peaks (`t = 0` strictly equivalent,
//! `t > 0` not, see the round-4 regression notes); ② `reduce` rewritten as a separable
//! row-streaming pass with a ring buffer, removing two `collect()` heap allocations per
//! output pixel; ③ Pass B's weight planes switched from `.clone()` to `Cow::Borrowed`,
//! saving one 128 MB pure copy per frame per level.
//!
//! The multithread round (same day, third round) parallelises everything that is
//! "per-pixel independent within a level" by row bands (`par.rs`, `std::thread::scope`,
//! zero new dependencies): the measure chain (γ2.0 luma, modified Laplacian, box-average
//! output pass, cross-frame max), `reduce` (each band keeps its own 5-row ring, one or two
//! horizontal rows recomputed at band edges), `accumulate_upsample`,
//! `laplacian_from_gaussian` (upsampling and subtraction **fused per pixel**, dropping one
//! 288 MB full-resolution intermediate plane and a whole-plane write+read),
//! `accumulate_level`, and Pass B's fourth-power weights. The frame loop stays serial (the
//! f32 accumulation order into `out_tower` is untouched), and so do the summed-area table
//! build, the histogram percentile and the collapse's level order -- every output pixel's
//! float addition sequence matches the serial version exactly, **bitwise unchanged**: the
//! probe's `T10_DUMP` dumps before/after (288 MB each) compare equal byte for byte with
//! `cmp`, and the Weighted checksum is unchanged. The peak memory **went down** (4.43 →
//! 4.14 GB): parallelism saved more than time -- the fused Laplacian drops an intermediate
//! plane, so the resident baseline shrank by 288 MB too. The multithread round's Weighted
//! baseline re-measured at 2.285 s / 3.09 GB / 768 MB (machine jitter); after the change
//! Weighted itself dropped to 1.31 s -- the parallelised measure chain benefits it as well.
//! Ratios are always computed against the **same-session** baseline.
//!
//! The earlier arithmetic ("output tower 384 MB + weight tower 128 MB") guessed about
//! 1.0 GB and **is wrong by more than 2x**: it forgot Pass A's two full-resolution `best` /
//! `gate` planes (96 MB each), the mask tower (128 MB), the per-frame measure plane (96 MB)
//! and the current frame's Laplacian (288 MB). Historically the wall clock was dominated by
//! `focus::measure_plane` running **twice** per frame (Pass A for the percentile cut, Pass B
//! for the weights); since the measure-chain-once round it runs **once** -- see below.
//! These are measurements, not estimates: change the implementation and
//! re-measure -- do not edit the comment to match the old numbers.
//!
//! The measure-chain-once round (same day, fourth round): Pass A and Pass B used to each
//! run the whole measure chain (luma → modified Laplacian → summed-area table → box
//! average) per frame -- twice per frame in total. Pass A now produces both the peak plane
//! (cross-frame ml maxima, box-averaged once, for the percentile cut) and, per frame, the
//! **box-averaged** measure plane in one pass; the planes are cached for Pass B, which
//! consumes and releases them frame by frame (8 x 96 MB, decaying). Also: ① level 0 no
//! longer `from_rgb`-copies the whole frame -- `reduce` and the Laplacian borrow the frame
//! data directly; ② per-level scratch (Gaussian planes, weight planes, the finest-level
//! weight `w0`, the tap tables) is preallocated once and reused across frames; ③ the
//! Laplacian is **never materialised** -- `fine - upsample(coarse)` is computed per pixel
//! right in `accumulate_laplacian_level` (the f32 subtraction rounds once in both forms,
//! bitwise equivalent), saving a whole-plane write+read per frame per level; ④ the two
//! whole-image passes building `gate` and `weak` merged into one, with `weak` replaced by
//! `masks[0]` (which *is* gate); ⑤ the up/down-sampling tap tables are built once per
//! level. Level-swept, same session, min-of-N: levels=1 1.21 s, levels=2 3.35 s, levels=6
//! 4.05 s -- the "fixed cost of entering multi-scale" (levels=2 − levels=1) dropped from
//! 4.11 s to **2.14 s**. Peak memory 4.14 → **3.86 GB**: the measure cache adds 768 MB,
//! but the measure-chain churn, the three resident `best`/`gate`/`weak` planes, the mask
//! tower's level-0 clone and the whole Laplacian plane saved more. Bitwise unchanged: the
//! probe's `T10_DUMP` dumps before/after (288 MB each) compare equal byte for byte with
//! `cmp`, checksum identical. Cost note: during Pass B the measure cache holds up to
//! ~7 x 96 MB resident -- the main new contributor to peak memory; shrinking it further
//! means fewer cached planes or a different encoding (f16 fails the bitwise requirement).
//!
//! `feather` and `smooth_z` do **not apply** here: fusion is a per-level weighted
//! normalisation, with no depth jumps, no feather mask and no mode filter. Callers may pass
//! them; they are validated but not read.

use crate::focus::{self, Options};

/// 二项式 5-tap 核（下采样用）。和恒为 1，常数场经过 reduce 仍是常数场。
///
/// The binomial 5-tap kernel used for reduction; it sums to 1, so a constant field stays
/// constant through `reduce`.
const K5: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// 权重和低于这个数就认为「没有任何帧认领这一格」，退回参考帧。
///
/// A weight sum below this means no frame claims the pixel; fall back to the reference.
const WSUM_EPS: f32 = 1e-6;

/// 显式层数的硬上限：超过它一律夹紧，不许拿一张 48px 小图开 100 层。
///
/// Hard ceiling for an explicit level count; anything above it is clamped so a 48 px test
/// frame can never request a hundred levels.
pub const MAX_LEVELS: usize = 12;

/// 一层平面：自带尺寸与通道数，`data` 按 `y * w * channels + x * channels + c` 排布。
///
/// RGB 塔层是 3 通道，权重与掩膜塔层是 1 通道。尺寸随层走，不给上层留「第 l 层
/// 到底多大」的心算负担——下采样遇到奇数边长是 `ceil(w/2)`，不是 `w >> l`。
///
/// One plane of a tower: it carries its own size and channel count, with `data` laid out as
/// `y * w * channels + x * channels + c`. RGB levels have 3 channels; weight and mask
/// levels have 1. Keeping the size with the plane removes the need to recompute "how large
/// is level l" upstairs: reduction of an odd side is `ceil(w/2)`, not `w >> l`.
#[derive(Clone, Debug)]
pub struct Plane {
    /// 宽（像素）。
    ///
    /// Width in pixels.
    pub w: usize,
    /// 高（像素）。
    ///
    /// Height in pixels.
    pub h: usize,
    /// 通道数（RGB = 3，权重/掩膜 = 1）。
    ///
    /// Channel count (RGB = 3, weight/mask = 1).
    pub channels: usize,
    /// 交错存放的样本。
    ///
    /// Interleaved samples.
    pub data: Vec<f32>,
}

impl Plane {
    /// 全零平面。
    ///
    /// An all-zero plane.
    pub fn zeros(w: usize, h: usize, channels: usize) -> Self {
        Self {
            w,
            h,
            channels,
            data: vec![0.0f32; w * h * channels],
        }
    }

    /// 由一帧交错 RGB 拷出最细层。
    ///
    /// Copy the finest level out of an interleaved RGB frame.
    pub fn from_rgb(frame: &[f32], w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            channels: 3,
            data: frame.to_vec(),
        }
    }

    /// 由一张单通道平面拷出最细层。
    ///
    /// Copy the finest level out of a single-channel plane.
    pub fn from_mono(plane: &[f32], w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            channels: 1,
            data: plane.to_vec(),
        }
    }

    /// 第 `p` 个像素的第 `c` 通道在 `data` 里的起点。
    ///
    /// Start offset of channel `c` of pixel `p` inside `data`.
    #[inline]
    fn offset(&self, p: usize, c: usize) -> usize {
        p * self.channels + c
    }
}

/// 解析层数：`0 = auto`，显式值夹紧到 `1..=MAX_LEVELS`。
///
/// auto 口径是 `clamp(floor(log2(min(w,h)/32)), 3, 8)`：24 MP（6000×4000）得 6，
/// 48px 判据夹具落到下限 3。夹紧是**硬要求**：不夹紧时 48px 的 auto 会算出 0 层，
/// 输出塔为空，塌缩直接越界；显式 1000 层也会把小图一路降到 1×1 再反复空转。
///
/// Resolve the level count: `0 = auto`, explicit values clamp into `1..=MAX_LEVELS`.
///
/// Auto is `clamp(floor(log2(min(w,h)/32)), 3, 8)`: 24 MP (6000x4000) gives 6 and a 48 px
/// fixture hits the lower bound of 3. The clamp is a hard requirement: without it a 48 px
/// auto resolves to 0 levels, the output tower is empty and collapse indexes out of bounds;
/// an explicit 1000 would drive a small frame down to 1x1 and then spin on empty levels.
pub fn resolve_levels(w: usize, h: usize, requested: usize) -> usize {
    if requested != 0 {
        return requested.clamp(1, MAX_LEVELS);
    }
    let short = w.min(h).max(1) as f64;
    let raw = (short / 32.0).log2().floor();
    // log2 的参数恒 > 0；NaN 只可能来自 0×∞ 之类，这里不会出现，仍做一次兜底。
    //
    // The log2 argument is always > 0; NaN cannot arise here, but guard anyway.
    if !raw.is_finite() {
        return 3;
    }
    (raw as i64).clamp(3, 8) as usize
}

/// 每一层的尺寸，长度恰为 `levels`。
///
/// The size of every level, with exactly `levels` entries.
pub fn level_dims(w: usize, h: usize, levels: usize) -> Vec<(usize, usize)> {
    let mut dims = Vec::with_capacity(levels);
    let (mut cw, mut ch) = (w.max(1), h.max(1));
    for _ in 0..levels {
        dims.push((cw, ch));
        cw = cw.div_ceil(2);
        ch = ch.div_ceil(2);
    }
    dims
}

/// 5-tap 二项式下采样（逐轴可分离、边界夹边），尺寸取 `ceil(src/2)`。
///
/// 夹边时五个 tap 仍然全部参与（重复的只是样本坐标），核和恒为 1，所以**不需要**
/// 再归一也不会让边缘发暗；这是 P-C2「常数场逐点不变」的一半保证。
///
/// 实现是**可分离的逐行流式**：先横向 5-tap、再纵向 5-tap，横向结果按源行缓存 5 行
/// （环形），所以每个源行只横向算一次、常驻内存只有 5 行。这与早先那版直接写
/// 25 tap 的二维卷积在**实数上完全等价**（`K5[ky] * K5[kx]` 就是可分离核），差别只在
/// f32 的求和顺序（~1e-7 量级，见模块头的实测表）。
///
/// 为什么值得改：老版本对每个输出像素都 `collect()` 两个 `Vec` 当下标表（`ys` / `xs`），
/// 一张 24 MP 图单次 `reduce` 就要 1200 万次堆分配；而 `reduce` 是「每帧 × 每层 ×
/// 两座塔」调用的，光分配就吃掉十几秒。现在下标表按列/按行各建一次，内层循环里
/// 一次分配都没有。
///
/// Separable binomial 5-tap reduction with clamped edges and `ceil(src/2)` output size.
///
/// All five taps still participate at a clamped edge (only the sample coordinate repeats),
/// so the kernel sum stays 1 with no renormalisation and no dark border; that is half of
/// `P-C2`'s "a constant field stays constant" guarantee.
///
/// The implementation is **separable and row-streaming**: a horizontal 5-tap followed by a
/// vertical 5-tap, with five horizontally reduced rows kept in a ring buffer, so each source
/// row is reduced exactly once and the resident scratch is five rows. This is **exactly
/// equivalent** to the earlier direct 25-tap 2D convolution in real arithmetic
/// (`K5[ky] * K5[kx]` is a separable kernel); only the f32 summation order differs (order
/// 1e-7, quantified in the module header's measured table).
///
/// Why it was worth changing: the old version `collect()`ed two `Vec`s of indices for
/// **every output pixel** (`ys` / `xs`), so a single `reduce` on a 24 MP frame made twelve
/// million heap allocations -- and `reduce` runs per frame, per level, for two towers.
pub fn reduce(src: &Plane) -> Plane {
    let dw = src.w.div_ceil(2);
    let dh = src.h.div_ceil(2);
    let mut out = Plane::zeros(dw, dh, src.channels);
    reduce_into(&src.data, src.w, src.h, src.channels, &mut out);
    out
}

/// [`reduce`] 的**写入既有缓冲**版：源是裸切片视图（`sw`×`sh`×`ch`），输出写进
/// 调用方预分配好的 `dst`（尺寸必须是 `ceil(sw/2)`×`ceil(sh/2)`×`ch`）。
///
/// `fuse_pyramid` 的帧循环是串行的，每层的目标尺寸固定，所以这些缓冲可以每帧
/// 复用——旧版每帧每层 fresh 分配（最细层一张 288 MB 拉普拉斯面 + 两座塔的下采样
/// 输出，每帧约 1 GB 的分配 churn）。运算序列与 [`reduce`] 逐一相同，逐位不变。
///
/// The write-into-buffer form of [`reduce`]: the source is a bare slice view
/// (`sw`×`sh`×`ch`), the output goes into a caller-preallocated `dst` (sized exactly
/// `ceil(sw/2)`×`ceil(sh/2)`×`ch`). `fuse_pyramid`'s frame loop is serial and each
/// level's output size is fixed, so these buffers are reused every frame -- the old code
/// freshly allocated per frame per level (a 288 MB Laplacian plane at the finest level
/// plus both towers' reductions, ~1 GB of allocation churn per frame). The arithmetic
/// matches [`reduce`] exactly, bitwise.
fn reduce_into(src: &[f32], sw: usize, sh: usize, ch: usize, dst: &mut Plane) {
    let dw = dst.w;
    let dh = dst.h;
    if dw == 0 || dh == 0 || ch == 0 {
        return;
    }
    // 横向 5-tap 的源列下标：每个目标列算一次，全图复用。
    //
    // Horizontal source indices: computed once per target column.
    let xi: Vec<[usize; 5]> = (0..dw)
        .map(|x| {
            let sx = (2 * x) as isize;
            let mut t = [0usize; 5];
            for (k, slot) in t.iter_mut().enumerate() {
                *slot = (sx + k as isize - 2).clamp(0, sw as isize - 1) as usize;
            }
            t
        })
        .collect();
    // 环形缓冲：输出行 y 要源行 2y-2..=2y+2，相邻输出行只滑动两行，所以 5 个槽刚好
    // 装下当前活跃窗口；源行按升序按需计算，绝不重算。按**输出行带**并行：每带自带
    // 一个 5 行环形缓冲，只在带界上重复算一两行横向卷积（同一行两次算出的值逐位相同，
    // 只是重复劳动）；每带的行内运算与串行版逐一相同，逐位不变。
    //
    // Ring buffer: output row y needs source rows 2y-2..=2y+2 and consecutive output rows
    // slide by two, so five slots cover the live window; source rows are computed on demand
    // in increasing order and never recomputed. Parallelised by **output row bands**: each
    // band keeps its own 5-row ring and recomputes one or two horizontal rows at band edges
    // (the same row computed twice is bitwise identical, just duplicated work); the
    // per-row arithmetic inside a band matches the serial version exactly, bitwise.
    let out = dst.data.as_mut_slice();
    crate::par::for_bands(dh, out, dw * ch, |y0, y1, oband| {
        let mut ring: Vec<Vec<f32>> = (0..5).map(|_| vec![0.0f32; dw * ch]).collect();
        let mut done_row: isize = -1;
        for (i, y) in (y0..y1).enumerate() {
            let sy = (2 * y) as isize;
            let mut need = [0usize; 5];
            for (k, slot) in need.iter_mut().enumerate() {
                *slot = (sy + k as isize - 2).clamp(0, sh as isize - 1) as usize;
            }
            for &r in &need {
                if (r as isize) <= done_row {
                    continue;
                }
                let slot = r % 5;
                let dst_row = &mut ring[slot];
                let src_row = r * sw * ch;
                for (x, idx) in xi.iter().enumerate() {
                    let d = x * ch;
                    for c in 0..ch {
                        let mut acc = 0.0f32;
                        for k in 0..5 {
                            acc += K5[k] * src[src_row + idx[k] * ch + c];
                        }
                        dst_row[d + c] = acc;
                    }
                }
                done_row = r as isize;
            }
            for x in 0..dw {
                let d = (i * dw + x) * ch;
                for c in 0..ch {
                    let mut acc = 0.0f32;
                    for k in 0..5 {
                        acc += K5[k] * ring[need[k] % 5][x * ch + c];
                    }
                    oband[d + c] = acc;
                }
            }
        }
    });
}

/// 掩膜塔用的下采样：2×2 取最大，不做平滑。
///
/// 0/1 的「无细节」标记若被高斯抹成 0.25，粗层就会拿等权把参考帧混没；取最大能
/// 让「这一格里只要有一个弱像素，整格都算弱」——保守地退回参考帧，不造。
///
/// Reduction for the mask tower: a 2x2 max, no smoothing.
///
/// If a 0/1 "no detail" flag were Gaussian-blurred into 0.25, the coarse levels would let
/// equal weights wash the reference frame away. A max keeps "one weak pixel in this cell
/// makes the whole cell weak": conservatively fall back instead of inventing.
fn reduce_mask(src: &[f32], sw: usize, sh: usize) -> (Vec<f32>, usize, usize) {
    let dw = sw.div_ceil(2);
    let dh = sh.div_ceil(2);
    let mut out = vec![0.0f32; dw * dh];
    for y in 0..dh {
        for x in 0..dw {
            let mut best = 0.0f32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let yy = (2 * y + dy).min(sh - 1);
                    let xx = (2 * x + dx).min(sw - 1);
                    best = best.max(src[yy * sw + xx]);
                }
            }
            out[y * dw + x] = best;
        }
    }
    (out, dw, dh)
}

/// 0/1 门限塔：第 0 层就是输入本身（**接管所有权，不克隆**——旧版 `cur.clone()`
/// 在最细层白拷 96 MB），以后每层 2×2 取最大。最后一层的 reduce 结果不会再用，
/// 不算。
///
/// A 0/1 gate pyramid: level 0 is the input itself (ownership taken, no clone -- the old
/// `cur.clone()` wastefully copied 96 MB at the finest level), later levels are 2x2
/// maxima. The last level's reduce output is never read, so it is not computed.
fn mask_pyramid(gate: Vec<f32>, w: usize, h: usize, levels: usize) -> Vec<Vec<f32>> {
    let mut tower = Vec::with_capacity(levels);
    let mut cur = gate;
    let (mut cw, mut ch) = (w, h);
    for l in 0..levels {
        tower.push(std::mem::take(&mut cur));
        if l + 1 < levels {
            let next = reduce_mask(&tower[l], cw, ch);
            cur = next.0;
            cw = next.1;
            ch = next.2;
        }
    }
    tower
}

/// 上采样的一维 tap：把目标坐标 `x` 映回源坐标 `x/2`。
///
/// 偶数目标点用 `[1,4,6,4,1]/16`；奇数（半像素）点用相邻两个整数相核的平均
/// `[0.5,2.5,5,5,2.5,0.5]/16`。两者和都是 1，边界夹边后再显式归一一次——
/// 这就是 P-C2 负向验证里「去掉归一就会漂移」的那一处。
///
/// One-dimensional interpolation taps: target coordinate `x` maps back to `x/2` in the
/// source. Even targets use `[1,4,6,4,1]/16`; odd (half-pixel) targets use the average of
/// two adjacent integer kernels, `[0.5,2.5,5,5,2.5,0.5]/16`. Both sum to 1, and we
/// normalise once more after clamping -- the exact spot whose removal makes `P-C2` drift.
fn taps(dst_len: usize, src_len: usize) -> Vec<Vec<(usize, f32)>> {
    let even = [1.0f32, 4.0, 6.0, 4.0, 1.0];
    let odd = [0.5f32, 2.5, 5.0, 5.0, 2.5, 0.5];
    let mut all = Vec::with_capacity(dst_len);
    for x in 0..dst_len {
        let base = (x / 2) as i32;
        let raw: &[f32] = if x % 2 == 0 { &even } else { &odd };
        let start = base - 2;
        // 先按 5-tap / 半像素核的原始系数取 tap（和为 16），夹边之后**统一归一一次**：
        // 这一步是常数场不变的唯一保证，也是 P-C2 负向验证要拿掉的那一行。
        //
        // Collect taps with the raw 5-tap / half-pixel coefficients (sum 16), then
        // normalise **once** after clamping. This is the sole guarantee that a constant
        // field stays constant, and the exact line P-C2's negative check removes.
        let list: Vec<(usize, f32)> = raw
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                let idx = (start + k as i32).clamp(0, src_len as i32 - 1) as usize;
                (idx, v)
            })
            .collect();
        let sum: f32 = list.iter().map(|t| t.1).sum();
        all.push(list.into_iter().map(|(idx, v)| (idx, v / sum)).collect());
    }
    all
}

/// 一维插值 tap 表：每个目标坐标一列 `(源下标, 权重)`。
///
/// A one-dimensional tap table: one `(source index, weight)` list per target coordinate.
type TapTable = Vec<Vec<(usize, f32)>>;

/// 把 `src` 上采样到 `(tw, th)`；核归一，常数场不变。
///
/// Upsample `src` to `(tw, th)` with a normalised kernel; a constant field is preserved.
pub fn upsample(src: &Plane, tw: usize, th: usize) -> Plane {
    let mut out = Plane::zeros(tw, th, src.channels);
    accumulate_upsample(src, &mut out);
    out
}

/// 把 `src` 上采样后**原地累加**进 `dst`（`dst` 的尺寸就是目标尺寸）。
///
/// 塌缩用的就是这条：不另开一张全分辨率缓冲，直接把粗层的插值结果加进细层。
/// 逐像素独立（每个像素只读写自己的格子），按行带并行；每像素的 tap 遍历顺序
/// （ytaps 外层、xtaps 内层）与串行版一致，逐位不变。
///
/// Upsample `src` and accumulate it **in place** into `dst` (`dst` carries the target
/// size). Collapse uses exactly this: no extra full-resolution buffer, just add the coarse
/// interpolation into the finer level. Per-pixel independent (each pixel touches only its
/// own cell), parallelised by row bands; the per-pixel tap order (ytaps outer, xtaps
/// inner) matches the serial version, bitwise unchanged.
pub fn accumulate_upsample(src: &Plane, dst: &mut Plane) {
    let xt = taps(dst.w, src.w);
    let yt = taps(dst.h, src.h);
    let row = dst.w * dst.channels;
    crate::par::for_bands(dst.h, &mut dst.data, row, |y0, y1, band| {
        for (i, ytaps) in yt[y0..y1].iter().enumerate() {
            for (x, xtaps) in xt.iter().enumerate() {
                let d = (i * dst.w + x) * dst.channels;
                for c in 0..dst.channels.min(src.channels) {
                    let mut acc = 0.0f32;
                    for &(sy, wy) in ytaps {
                        for &(sx, wx) in xtaps {
                            acc += wy * wx * src.data[(sy * src.w + sx) * src.channels + c];
                        }
                    }
                    band[d + c] += acc;
                }
            }
        }
    });
}

/// 由一帧交错 RGB 建高斯塔：`G_0` 是原帧，之后逐级 `reduce`，长度恰为 `levels`。
///
/// `fuse_pyramid` 自己不用它（那边逐帧流式、只留相邻两层），这里是给库调用方与
/// 判据用的**整塔视图**：常数场塌缩、逐层尺寸、构造自己的融合塔都能从这一个入口验。
///
/// Build a Gaussian tower from one interleaved RGB frame: `G_0` is the frame itself and
/// each later level is a `reduce`, with exactly `levels` entries.
///
/// `fuse_pyramid` does not use it (it streams frames, keeping only two adjacent planes);
/// this is the **whole-tower view** for library callers and assertions -- constant-field
/// collapse, per-level sizes and building a custom fusion tower all enter here.
pub fn gaussian_pyramid(frame: &[f32], w: usize, h: usize, levels: usize) -> Vec<Plane> {
    let mut cur = Plane::from_rgb(frame, w, h);
    let mut tower = Vec::with_capacity(levels);
    for l in 0..levels {
        if l + 1 == levels {
            tower.push(cur);
            break;
        }
        tower.push(cur.clone());
        cur = reduce(&cur);
    }
    tower
}

/// `L_l = G_l − upsample(G_{l+1})`。两个参数就是「相邻的那一对」，不给整塔。
///
/// 上采样与减法**逐像素融合**：不再先写一张全分辨率 `up` 中间面（最细层 288 MB）
/// 再整面读回相减，而是在每个目标像素处按同样的 tap 顺序算出插值值直接从细层里减。
/// 每像素的运算序列（ytaps 外层、xtaps 内层求和，再做 `fine − acc`）与旧的两步版
/// 逐一相同，逐位不变；省掉一次 288 MB 的分配与一整面的写+读。
///
/// `L_l = G_l - upsample(G_{l+1})`. The two arguments are exactly the adjacent pair; no
/// whole tower is needed. Upsampling and subtraction are **fused per pixel**: instead of
/// writing a full-resolution intermediate `up` plane (288 MB at the finest level) and
/// reading it back for the subtraction, each target pixel computes the interpolated value
/// with the same tap order and subtracts it from the fine plane right there. The per-pixel
/// arithmetic sequence (ytaps outer, xtaps inner sum, then `fine - acc`) matches the old
/// two-step form exactly, bitwise unchanged; it saves one 288 MB allocation plus a full
/// plane write+read.
pub fn laplacian_from_gaussian(fine: &Plane, coarse: &Plane) -> Plane {
    let mut out = Plane::zeros(fine.w, fine.h, fine.channels);
    laplacian_into(&fine.data, fine.w, fine.h, coarse, &mut out);
    out
}

/// [`laplacian_from_gaussian`] 的**写入既有缓冲**版：细层是裸切片视图（最细层直接
/// 借用输入帧本身，省掉一次全幅 288 MB 拷贝），输出写进预分配的 `dst`。
///
/// 运算序列与 [`laplacian_from_gaussian`] 逐一相同（ytaps 外层、xtaps 内层求和，
/// `fine − acc`），逐位不变。
///
/// The write-into-buffer form of [`laplacian_from_gaussian`]: the fine plane is a bare
/// slice view (at the finest level it borrows the input frame itself, saving one full
/// 288 MB copy), the output goes into a preallocated `dst`. The arithmetic matches
/// [`laplacian_from_gaussian`] exactly (ytaps outer, xtaps inner sum, then `fine - acc`),
/// bitwise unchanged.
fn laplacian_into(fine: &[f32], fw: usize, fh: usize, coarse: &Plane, dst: &mut Plane) {
    let xt = taps(fw, coarse.w);
    let yt = taps(fh, coarse.h);
    let row = fw * dst.channels;
    let out = dst.data.as_mut_slice();
    crate::par::for_bands(fh, out, row, |y0, y1, band| {
        for (i, ytaps) in yt[y0..y1].iter().enumerate() {
            for (x, xtaps) in xt.iter().enumerate() {
                // `d` 是带内偏移（band 从行 y0 起），读 fine 用绝对偏移 `fa`。
                //
                // `d` is the band-local offset (the band starts at row y0); reading `fine`
                // uses the absolute offset `fa`.
                let d = (i * fw + x) * dst.channels;
                let fa = ((y0 + i) * fw + x) * dst.channels;
                for c in 0..dst.channels {
                    let mut acc = 0.0f32;
                    if c < coarse.channels {
                        for &(sy, wy) in ytaps {
                            for &(sx, wx) in xtaps {
                                acc += wy
                                    * wx
                                    * coarse.data[(sy * coarse.w + sx) * coarse.channels + c];
                            }
                        }
                    }
                    band[d + c] = fine[fa + c] - acc;
                }
            }
        }
    });
}

/// 从最粗层起原地逐层上采样累加，返回最细层的数据（塔内不再保留副本）。
///
/// Collapse from the coarsest level in place by accumulating upsampled values downwards,
/// then return the finest level's data without keeping another copy in the tower.
pub fn collapse_in_place(tower: &mut [Plane]) -> Vec<f32> {
    if tower.is_empty() {
        return Vec::new();
    }
    for l in (0..tower.len() - 1).rev() {
        let (lo, hi) = tower.split_at_mut(l + 1);
        accumulate_upsample(&hi[0], &mut lo[l]);
    }
    std::mem::take(&mut tower[0].data)
}

/// γ2.0 亮度，写入复用缓冲。与 `focus::modified_laplacian` 的灰度趟运算序列逐一
/// 相同（focus.rs 是只读的另一位工程师成果，这里按原式在本地实现以换取串行段的
/// 缓冲复用），逐位不变。
///
/// γ2.0 luma into a reused buffer. The arithmetic matches `focus::modified_laplacian`'s
/// luma pass exactly (focus.rs is another engineer's read-only work; the same formula is
/// reimplemented here to get buffer reuse in the serial segment), bitwise unchanged.
fn luma_into(frame: &[f32], w: usize, h: usize, gray: &mut [f32]) {
    crate::par::for_bands(h, gray, w, |y0, _y1, g| {
        for (i, row) in g.chunks_mut(w).enumerate() {
            let y = y0 + i;
            for (x, cell) in row.iter_mut().enumerate() {
                let j = (y * w + x) * 3;
                let luma = 0.2126 * frame[j] + 0.7152 * frame[j + 1] + 0.0722 * frame[j + 2];
                *cell = luma.max(0.0).sqrt();
            }
        }
    });
}

/// 改进拉普拉斯（边界夹边取），写入复用缓冲。与 `focus::modified_laplacian` 的
/// 拉普拉斯趟逐一相同，逐位不变。
///
/// Modified Laplacian (clamped edges) into a reused buffer. Matches
/// `focus::modified_laplacian`'s Laplacian pass exactly, bitwise unchanged.
fn ml_into(gray: &[f32], w: usize, h: usize, out: &mut [f32]) {
    crate::par::for_bands(h, out, w, |y0, _y1, o| {
        for (i, row) in o.chunks_mut(w).enumerate() {
            let y = y0 + i;
            for (x, cell) in row.iter_mut().enumerate() {
                let c = gray[y * w + x];
                let l = gray[y * w + x.saturating_sub(1)];
                let r = gray[y * w + (x + 1).min(w - 1)];
                let u = gray[y.saturating_sub(1) * w + x];
                let d = gray[(y + 1).min(h - 1) * w + x];
                *cell = (2.0 * c - l - r).abs() + (2.0 * c - u - d).abs();
            }
        }
    });
}

/// 积分图构建（行间串行归约，口径与 `focus::box_average` 逐字一致），写入复用缓冲。
/// `sat` 必须满足第 0 行与第 0 列为 0：首次分配即为零，此后这些格子从不被写。
///
/// 性能轮试过按行块拆两步并行（横向 rowsum 按行并行 + 纵向按列带扫描，每元素的加法
/// 序列都能逐位保持），24 MP 实测**净负收益**：build_sat 段 0.20–0.24 s（串行）→
/// 0.33–0.35 s（并行，+50%，同轮 5 次取中位）。结构性原因：纵向链条
/// `sat[y+1] = sat[y] + rowsum[y]` 的前缀依赖在 x 上，列带并行必须先把 rowsum 整面
/// 算出来存住——多出一张 w×h 的 f64 中间面写一遍读一遍（24 MP 每次 +384 MB 流量，
/// 总流量 480 → 864 MB），带宽聚合省下的时间填不平这笔账。逐位一致的积分图构建本质
/// 是内存带宽瓶颈，串行单趟已接近该结构下的最优，**保留串行**。
///
/// Summed-area table build (serial row-to-row reduction, literally the same convention as
/// `focus::box_average`) into a reused buffer. `sat`'s row 0 and column 0 must be zero:
/// fresh allocations are, and those cells are never written afterwards.
///
/// The performance round tried a row-block two-step parallel form (horizontal rowsums by
/// row plus a vertical sweep by column band, every element's addition sequence bitwise
/// preserved); at 24 MP it measured a clear **net loss**: the build_sat segment went from
/// 0.20-0.24 s (serial) to 0.33-0.35 s (parallel, +50%, median of 5, same session). The
/// structural reason: the vertical chain `sat[y+1] = sat[y] + rowsum[y]` has its prefix
/// dependency along x, so column-band parallelism must first materialise the whole rowsum
/// plane -- one extra w x h f64 intermediate written and read back (+384 MB of traffic per
/// call at 24 MP, total 480 -> 864 MB), and bandwidth aggregation cannot pay for it. A
/// bitwise-exact SAT build is memory-bandwidth bound, and the serial single pass is
/// already near-optimal for this structure, so the **serial form stays**.
fn build_sat(ml: &[f32], w: usize, h: usize, sat: &mut [f64]) {
    for y in 0..h {
        let mut row_sum = 0.0f64;
        for x in 0..w {
            row_sum += ml[y * w + x] as f64;
            sat[(y + 1) * (w + 1) + (x + 1)] = sat[y * (w + 1) + (x + 1)] + row_sum;
        }
    }
}

/// 由积分图出盒平均结果，写入复用缓冲。与 `focus::box_average` 的输出趟逐一相同
///（窗口收缩、`(sum / area as f64) as f32`），逐位不变。
///
/// Box-average output pass from a built table into a reused buffer. Matches
/// `focus::box_average`'s output pass exactly (shrinking windows,
/// `(sum / area as f64) as f32`), bitwise unchanged.
fn box_from_sat(sat: &[f64], w: usize, h: usize, radius: usize, out: &mut [f32]) {
    crate::par::for_bands(h, out, w, |y0, _y1, band| {
        for (i, row) in band.chunks_mut(w).enumerate() {
            let y = y0 + i;
            let top = y.saturating_sub(radius);
            let bottom = (y + radius + 1).min(h);
            for (x, cell) in row.iter_mut().enumerate() {
                let left = x.saturating_sub(radius);
                let right = (x + radius + 1).min(w);
                let area = ((right - left) * (bottom - top)) as f32;
                let sum = sat[bottom * (w + 1) + right] + sat[top * (w + 1) + left]
                    - sat[top * (w + 1) + right]
                    - sat[bottom * (w + 1) + left];
                *cell = (sum / area as f64) as f32;
            }
        }
    });
}

/// 4096 桶直方图求「权重下限分位」，口径与 `compute_owner_map` 逐字一致。
///
/// The 4096-bin histogram for the lower weight percentile, identical in convention to
/// `compute_owner_map`.
fn percentile_cut(best: &[f32], t: f32) -> Option<f32> {
    if t <= 0.0 || !t.is_finite() || best.is_empty() {
        return None;
    }
    let max = best.iter().cloned().fold(0.0f32, f32::max);
    if max <= 0.0 {
        return None;
    }
    let bins = 4096usize;
    let mut hist = vec![0u64; bins];
    let scale = (bins - 1) as f32 / max;
    for &b in best {
        hist[((b * scale) as usize).min(bins - 1)] += 1;
    }
    let want = ((t / 100.0) as f64 * best.len() as f64).floor() as u64;
    let mut acc = 0u64;
    let mut cut_bin = bins - 1;
    for (i, c) in hist.iter().enumerate() {
        acc += c;
        if acc >= want.max(1) {
            cut_bin = i;
            break;
        }
    }
    Some(cut_bin as f32 / scale)
}

/// 单尺度路径：`levels = 1` 时塔里只有 `G_0`，融合就是 Weighted 的逐点加权。
///
/// 这里**特意另写一遍**而不去调 Weighted：左边是「被测的实现」，右边是「已知正确的
/// 参照」，两边必须互相独立才有判据价值（把 `m^4` 改成 `m^2` 时 P-C1 要能红）。
/// 运算顺序与 Weighted 完全一致，所以逐点偏差是 0，不是「约等于」。
///
/// The single-scale path: at `levels = 1` the tower holds only `G_0`, so fusion is exactly
/// Weighted's per-pixel blend.
///
/// This is deliberately a second implementation rather than a call into Weighted: the
/// left side is the code under test and the right side the known-good reference, and they
/// must stay independent for the assertion to mean anything (change `m^4` to `m^2` and
/// `P-C1` must go red). The operation order matches Weighted exactly, so the per-pixel
/// difference is 0 rather than "about equal".
fn fuse_single_scale(frames: &[&[f32]], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let n = w * h;
    let mut out = vec![0.0f32; n * 3];
    let mut wsum = vec![0.0f32; n];
    for f in frames {
        let m = focus::measure_plane(f, w, h, radius);
        for p in 0..n {
            let mm = m[p];
            let wt = mm * mm * mm * mm;
            if wt > 0.0 {
                let j = p * 3;
                out[j] += wt * f[j];
                out[j + 1] += wt * f[j + 1];
                out[j + 2] += wt * f[j + 2];
                wsum[p] += wt;
            }
        }
    }
    let rf = frames[frames.len() / 2];
    for p in 0..n {
        let j = p * 3;
        if wsum[p] > 0.0 {
            out[j] /= wsum[p];
            out[j + 1] /= wsum[p];
            out[j + 2] /= wsum[p];
        } else {
            out[j] = rf[j];
            out[j + 1] = rf[j + 1];
            out[j + 2] = rf[j + 2];
        }
    }
    out
}

/// 把一帧在某一层的贡献按权重累进输出塔和权重和塔。
///
/// 逐像素独立（每个像素只读写自己的格子），按行带并行；每像素的累加顺序与串行版
/// 一致，逐位不变。帧间顺序仍由调用方的串行帧循环保证。
///
/// Accumulate one frame's contribution at one level into the output and weight-sum towers.
/// Per-pixel independent (each pixel touches only its own cell), parallelised by row
/// bands; the per-pixel accumulation order matches the serial version, bitwise. The
/// inter-frame order is still guaranteed by the caller's serial frame loop.
fn accumulate_level(out: &mut Plane, wsum: &mut [f32], plane: &Plane, weight: &[f32]) {
    let w = out.w;
    crate::par::for_bands2(
        out.h,
        &mut out.data,
        w * 3,
        wsum,
        w,
        |y0, _y1, oband, wband| {
            for (i, wrow) in wband.chunks_mut(w).enumerate() {
                let y = y0 + i;
                for (x, ws) in wrow.iter_mut().enumerate() {
                    let p = y * w + x;
                    let wt = weight[p];
                    if wt > 0.0 {
                        let o = p * 3;
                        let b = (i * w + x) * 3;
                        for c in 0..3 {
                            oband[b + c] += wt * plane.data[o + c];
                        }
                        *ws += wt;
                    }
                }
            }
        },
    );
}

/// 把一层的拉普拉斯贡献**不落盘**地累进输出塔和权重和塔：`L = fine − upsample(coarse)`
/// 逐像素现算，算完立刻 `out += wt * L`、`wsum += wt`。
///
/// 逐位不变论证：旧两步版先 `lap[c] = fine[c] − acc`（f32 舍入一次、落盘）再
/// `out[c] += wt * lap[c]`；这里 `(fine[c] − acc)` 同样只做一次 f32 舍入就直接乘
/// `wt`——减法、乘法、加法的舍入序列逐一相同。tap 遍历顺序（ytaps 外层、xtaps 内层）
/// 与 `laplacian_into` 一致；`wt <= 0` 的像素旧版算了 lap 但不使用，这里干脆不算。
/// 按行带并行（逐位不变），帧间顺序由调用方的串行帧循环保证。
///
/// Accumulate one level's Laplacian contribution **without materialising it**:
/// `L = fine - upsample(coarse)` is computed per pixel and immediately folded as
/// `out += wt * L`, `wsum += wt`. Bitwise argument: the old two-step form first stored
/// `lap[c] = fine[c] - acc` (one f32 rounding) then did `out[c] += wt * lap[c]`; here
/// `(fine[c] - acc)` takes the same single f32 rounding before multiplying by `wt` -- the
/// subtraction/multiplication/addition rounding sequence matches exactly. The tap order
/// (ytaps outer, xtaps inner) matches `laplacian_into`; pixels with `wt <= 0` had their
/// lap computed but unused in the old code, and are simply not computed here. Parallelised
/// by row bands (bitwise unchanged); the inter-frame order is guaranteed by the caller's
/// serial frame loop.
/// 条带宽度：一次同时推进两个相邻像素。之所以是**常量而不是参数**：条带体里每条像素
/// 的累加器是分开命名的局部变量（`a00..a02` / `a10..a12`），改这个数必须同时改函数体。
///
/// 两个像素 × 三个通道 = 六条独立链，足以遮住 f32 加法 4 cycle 的延迟，又不至于把 tap
/// 游标挤出寄存器。
///
/// Strip width: two adjacent pixels advance together. It is a **constant rather than a
/// parameter** on purpose: the strip body names each pixel's accumulators separately
/// (`a00..a02` / `a10..a12`), so changing this number means changing the body too.
///
/// Two pixels x three channels is six independent chains -- enough to hide the 4-cycle f32
/// add latency without pushing the tap cursors out of registers.
const STRIP: usize = 2;

#[allow(clippy::too_many_arguments)]
fn accumulate_laplacian_level(
    out: &mut Plane,
    wsum: &mut [f32],
    fine: &[f32],
    coarse: &Plane,
    xt: &[Vec<(usize, f32)>],
    yt: &[Vec<(usize, f32)>],
    weight: &[f32],
) {
    let w = out.w;
    let cw = coarse.w;
    let ch = coarse.channels;
    let cd = coarse.data.as_slice();
    // 条带快路径只对 3 通道成立（通道下标直接写成 0/1/2）；其余通道数整行走标量路径。
    //
    // The strip fast path only holds for three channels (the channel index is written out
    // as 0/1/2); any other channel count takes the scalar path for the whole row.
    let strip = ch == 3;
    crate::par::for_bands2(
        out.h,
        &mut out.data,
        w * 3,
        wsum,
        w,
        |y0, _y1, oband, wband| {
            for (i, wrow) in wband.chunks_mut(w).enumerate() {
                let y = y0 + i;
                let ytaps = &yt[y];
                let base = y * w;
                let mut x = 0;
                // 条带主体：STRIP 个相邻像素 × 3 个通道 = 6 条互不相关的累加链同时在飞。
                // 每个像素每条通道的 tap 遍历次序（ytaps 外层、xtaps 内层）与逐像素版
                // 完全相同，`wy * wx` 也仍是那一次舍入（三条通道共享同一个 `k` 是同一个
                // 表达式的同一个值），所以每个像素的浮点加法序列逐项不变。
                //
                // Strip body: STRIP adjacent pixels x 3 channels = six unrelated
                // accumulation chains in flight. Every pixel's per-channel tap order
                // (ytaps outer, xtaps inner) is exactly the scalar one, and `wy * wx` is
                // still that single rounding (the three channels share one `k`, which is
                // the same value of the same expression), so each pixel's float addition
                // sequence is unchanged term by term.
                while strip && x + STRIP <= w {
                    let wt0 = weight[base + x];
                    let wt1 = weight[base + x + 1];
                    let e0 = &xt[x];
                    let e1 = &xt[x + 1];
                    let o = (base + x) * 3;
                    let b = (i * w + x) * 3;
                    let mut a00 = 0.0f32;
                    let mut a01 = 0.0f32;
                    let mut a02 = 0.0f32;
                    let mut a10 = 0.0f32;
                    let mut a11 = 0.0f32;
                    let mut a12 = 0.0f32;
                    // 一个像素三个通道各走完自己的 xtaps；`wy` / `row` 来自外层 ytap 迭代。
                    //
                    // One pixel's three channels walk their own xtaps; `wy` / `row` come
                    // from the enclosing ytap iteration.
                    macro_rules! step {
                        ($a:ident, $b:ident, $c:ident, $t:expr, $row:ident, $wy:ident, $cd:ident) => {
                            for &(sx, wx) in $t {
                                let k = $wy * wx;
                                let s = $row + sx * 3;
                                $a += k * $cd[s];
                                $b += k * $cd[s + 1];
                                $c += k * $cd[s + 2];
                            }
                        };
                    }
                    for &(sy, wy) in ytaps {
                        let row = sy * cw * ch;
                        step!(a00, a01, a02, e0, row, wy, cd);
                        step!(a10, a11, a12, e1, row, wy, cd);
                    }
                    // `wt > 0` 是逐像素的：某格权重为 0 只跳过它自己的写入，另一格的链
                    // 与写入照常进行。
                    //
                    // `wt > 0` is per pixel: a zero weight only skips that pixel's own
                    // write, the other pixel's chain and write still happen.
                    if wt0 > 0.0 {
                        oband[b] += wt0 * (fine[o] - a00);
                        oband[b + 1] += wt0 * (fine[o + 1] - a01);
                        oband[b + 2] += wt0 * (fine[o + 2] - a02);
                        wrow[x] += wt0;
                    }
                    if wt1 > 0.0 {
                        oband[b + 3] += wt1 * (fine[o + 3] - a10);
                        oband[b + 4] += wt1 * (fine[o + 4] - a11);
                        oband[b + 5] += wt1 * (fine[o + 5] - a12);
                        wrow[x + 1] += wt1;
                    }
                    x += STRIP;
                }
                // 行尾不足一个条带的部分（以及非 3 通道的整行）逐像素处理，算式与
                // 展开前逐字相同。
                //
                // The row tail shorter than one strip (and any non-3-channel row) runs the
                // scalar path, verbatim the pre-unrolling formula.
                for xx in x..w {
                    let p = base + xx;
                    let wt = weight[p];
                    if wt > 0.0 {
                        let o = p * 3;
                        let b = (i * w + xx) * 3;
                        let xtaps = &xt[xx];
                        for c in 0..3 {
                            let mut acc = 0.0f32;
                            if c < ch {
                                for &(sy, wy) in ytaps {
                                    for &(sx, wx) in xtaps {
                                        acc += wy * wx * coarse.data[(sy * cw + sx) * ch + c];
                                    }
                                }
                            }
                            oband[b + c] += wt * (fine[o + c] - acc);
                        }
                        wrow[xx] += wt;
                    }
                }
            }
        },
    );
}

/// 逐帧流式融合，返回线性 RGB `f32`（`w*h*3`）。
///
/// 输入契约与 [`crate::focus::focus_stack`] 相同（已对齐、逐帧 `w*h*3`）；调用方
/// 负责校验尺寸与帧数。`threshold` 在这里是**权重下限分位**：分位线以下的像素在
/// 所有层都不参与融合，整块退回参考帧（序列中位帧）——注意这不只影响最细层，
/// 掩膜塔会把它带到每一层，否则粗层的等权会把参考帧又混掉。
///
/// Stream the fusion frame by frame and return linear RGB `f32` (`w*h*3`).
///
/// The input contract matches [`crate::focus::focus_stack`] (already aligned, `w*h*3` per
/// frame); the caller validates sizes and frame count. Here `threshold` is the **lower
/// weight percentile**: pixels below the cut contribute nothing at any level and fall back
/// wholesale to the reference frame (the sequence's middle frame). This reaches every level
/// through the mask tower -- otherwise equal weights at coarse levels would blend the
/// reference frame away again.
pub fn fuse_pyramid(frames: &[&[f32]], w: usize, h: usize, options: &Options) -> Vec<f32> {
    let levels = resolve_levels(w, h, options.levels);
    let count = frames.len();
    let reference = count / 2;
    let radius = options.radius;
    if levels == 1 {
        // 没有多尺度结构，threshold 的权重分位兜底也就没有意义；退化要求覆盖全部参数。
        //
        // With no multi-scale structure the percentile gate has nothing to act on; the
        // degeneration must hold for every parameter set.
        return fuse_single_scale(frames, w, h, radius);
    }

    let t = if options.threshold.is_nan() {
        0.0
    } else {
        options.threshold.clamp(0.0, 100.0)
    };
    // Pass A（本轮重写）：测度链每帧只跑**一遍**。旧版 Pass A 调 `focus::measure_peak`、
    // Pass B 又对每帧调一遍完全相同的 `focus::measure_plane`——亮度 → 改进拉普拉斯 →
    // 积分图 → 盒平均整条链每帧跑两遍，这就是「进入多尺度」固定开销的大头。现在同一趟
    // 里：峰值面（跨帧取改进拉普拉斯最大，最后一次盒平均）求分位线，每帧**盒平均后**
    // 的测度面缓存进 `measures` 供 Pass B 直接使用。缓存 8 帧 × 96 MB 量级，Pass B
    // 逐帧消费、用完即释放。`radius = 0` 时盒平均是恒等，缓存面就是 ml 本身。
    // 逐位不变：峰值取最大与 `measure_peak` 同一比较同一顺序；每帧缓存面与
    // `measure_plane` 的返回值逐位相同（luma / 拉普拉斯 / 积分图 / 盒的算式与顺序
    // 原样照搬，缓冲复用不改变任何像素的运算序列）。
    //
    // Pass A (rewritten this round): the measure chain runs **once** per frame. The old
    // code called `focus::measure_peak` in Pass A and then re-ran the identical
    // `focus::measure_plane` per frame in Pass B -- luma, modified Laplacian, summed-area
    // table, box average, the whole chain twice per frame, which was the bulk of the
    // fixed cost of entering multi-scale. Now a single pass produces both the peak plane
    // (cross-frame modified-Laplacian maxima, box-averaged once, for the percentile cut)
    // and, per frame, the **box-averaged** measure plane cached in `measures` for Pass B
    // to consume directly. The cache is on the order of 8 x 96 MB and shrinks frame by
    // frame as Pass B consumes it. With `radius = 0` the box average is the identity and
    // the cached plane is ml itself. Bitwise unchanged: the running max uses the same
    // comparison in the same order as `measure_peak`, and each cached plane is bitwise
    // what `measure_plane` returned (the luma / Laplacian / SAT / box formulas are copied
    // verbatim; reusing buffers changes no pixel's arithmetic sequence).
    let n = w * h;
    let mut peak = vec![0.0f32; n];
    let mut gray = vec![0.0f32; n];
    let mut ml = vec![0.0f32; n];
    let mut sat = vec![0.0f64; (w + 1) * (h + 1)];
    let mut measures: Vec<Vec<f32>> = Vec::with_capacity(count);
    for f in frames {
        luma_into(f, w, h, &mut gray);
        ml_into(&gray, w, h, &mut ml);
        crate::par::for_bands(h, &mut peak, w, |y0, _y1, band| {
            for (i, row) in band.chunks_mut(w).enumerate() {
                let y = y0 + i;
                for (x, cell) in row.iter_mut().enumerate() {
                    let p = y * w + x;
                    if ml[p] > *cell {
                        *cell = ml[p];
                    }
                }
            }
        });
        let mut m = vec![0.0f32; n];
        if radius == 0 {
            m.copy_from_slice(&ml);
        } else {
            build_sat(&ml, w, h, &mut sat);
            box_from_sat(&sat, w, h, radius, &mut m);
        }
        measures.push(m);
    }
    drop(gray);
    drop(ml);
    let best: Vec<f32> = if radius == 0 {
        drop(sat);
        peak
    } else {
        let mut b = vec![0.0f32; n];
        build_sat(&peak, w, h, &mut sat);
        box_from_sat(&sat, w, h, radius, &mut b);
        drop(peak);
        drop(sat);
        b
    };
    let cut = percentile_cut(&best, t).unwrap_or(0.0);
    // 旧版这里 gate 一遍、weak 又一遍（两次全图遍历、两张常驻面）。现在只建 gate：
    // 「无细节」的整块退回在塌缩后直接查 `masks[0]`（它就是 gate 本身，逐位同值）。
    //
    // The old code built `gate` in one full pass and `weak` in another (two whole-image
    // traversals, two resident planes). Now only `gate` is built: the wholesale fallback
    // after collapse reads `masks[0]` directly (it *is* gate, bitwise the same values).
    let gate: Vec<f32> = best
        .iter()
        .map(|&b| if b > cut { 1.0 } else { 0.0 })
        .collect();
    drop(best);
    let masks = mask_pyramid(gate, w, h, levels);
    let dims = level_dims(w, h, levels);
    let mut out_tower: Vec<Plane> = dims
        .iter()
        .map(|&(dw, dh)| Plane::zeros(dw, dh, 3))
        .collect();
    let mut wsum_tower: Vec<Vec<f32>> =
        dims.iter().map(|&(dw, dh)| vec![0.0f32; dw * dh]).collect();
    // 最粗两层等权；`max(1)` 保证 levels=1 的退化路径不会误收紧，levels=2 只收紧一层。
    //
    // Equal weights on the two coarsest levels; `max(1)` keeps the levels=1 degeneration
    // untouched and tightens exactly one level when levels=2.
    let tight_from = (levels - 2).max(1);

    // Pass B 的逐层 scratch 预分配一次、跨帧复用（帧循环串行，不存在多线程共享可变
    // scratch 的问题）：`g_buf[l]` / `w_buf[l]` 是第 l+1 层的高斯/权重面，`w0` 是最细层
    // 权重面。拉普拉斯**不再落盘**——`fine − upsample(coarse)` 在累加处逐像素现算
    // （见 `accumulate_laplacian_level`），旧版这一面每帧每层一张（最细层 288 MB），
    // 写一遍读一遍纯属搬运。旧版每帧每层还要 fresh 分配这些面，每帧约 1 GB churn。
    //
    // Pass B's per-level scratch is preallocated once and reused across frames (the frame
    // loop is serial, so there is no shared-mutable-scratch hazard): `g_buf[l]` /
    // `w_buf[l]` hold level l+1's Gaussian/weight planes, `w0` the finest-level weight
    // plane. The Laplacian is **never materialised** -- `fine - upsample(coarse)` is
    // computed per pixel right at the accumulation site (see
    // `accumulate_laplacian_level`); the old code wrote a full plane per frame per level
    // (288 MB at the finest level) and read it straight back, pure shuffling, freshly
    // allocated every time (~1 GB churn per frame).
    let mut g_buf: Vec<Plane> = Vec::with_capacity(levels - 1);
    let mut w_buf: Vec<Plane> = Vec::with_capacity(levels - 1);
    for l in 1..levels {
        let (dw, dh) = dims[l];
        g_buf.push(Plane::zeros(dw, dh, 3));
        w_buf.push(Plane::zeros(dw, dh, 1));
    }
    let mut w0 = vec![0.0f32; n];
    // 上/下采样的 tap 表每层只算一次（旧版在 laplacian 里每帧每层重建，最细层一次
    // 一万个小 Vec）。表内容与 `taps` 逐位相同。
    //
    // The up/down-sampling tap tables are built once per level (the old code rebuilt them
    // inside the Laplacian every frame per level -- ten thousand small Vecs a time at the
    // finest level). The tables are bitwise what `taps` returns.
    let tap_cache: Vec<(TapTable, TapTable)> = (0..levels - 1)
        .map(|l| {
            let (fw, fh) = dims[l];
            let (cw, ch) = dims[l + 1];
            (taps(fw, cw), taps(fh, ch))
        })
        .collect();

    // Pass B：逐帧流式。第 0 层的「高斯面」**直接借用帧数据本身**（交错 RGB 的排布
    // 与 `Plane::from_rgb` 拷出来的完全一样，整幅 288 MB 拷贝纯属浪费），测度面从
    // `measures` 缓存取、用完即还。
    //
    // Pass B: stream frame by frame. Level 0's "Gaussian plane" **borrows the frame data
    // itself** (the interleaved RGB layout is exactly what `Plane::from_rgb` would copy,
    // so the full 288 MB copy was pure waste), and the measure plane comes from the
    // `measures` cache, released right after use.
    for (fi, f) in frames.iter().enumerate() {
        let m = std::mem::take(&mut measures[fi]);
        w0.fill(0.0);
        // 逐像素独立，按行带并行（逐位不变）。
        //
        // Per-pixel independent, parallelised by row bands (bitwise unchanged).
        crate::par::for_bands(h, &mut w0, w, |y0, _y1, band| {
            for (i, row) in band.chunks_mut(w).enumerate() {
                let y = y0 + i;
                for (x, cell) in row.iter_mut().enumerate() {
                    let p = y * w + x;
                    if masks[0][p] > 0.0 {
                        let mm = m[p];
                        *cell = mm * mm * mm * mm;
                    }
                }
            }
        });
        drop(m);

        for l in 0..levels - 1 {
            // 第 l 层的细层面：第 0 层借用帧数据与 w0，更粗层读上一层在 g_buf / w_buf
            // 里的输出。`split_at_mut` 让「读上一层」与「写本层输出」并存。
            //
            // Level l's fine planes: level 0 borrows the frame and w0; coarser levels read
            // the previous level's output in g_buf / w_buf. `split_at_mut` lets "read the
            // previous level" coexist with "write this level's output".
            let split_g;
            let split_w;
            let (fine, fw, fh, wsrc, g_next, w_next): (
                &[f32],
                usize,
                usize,
                &[f32],
                &mut Plane,
                &mut Plane,
            ) = if l == 0 {
                (f, w, h, &w0, &mut g_buf[0], &mut w_buf[0])
            } else {
                split_g = g_buf.split_at_mut(l);
                split_w = w_buf.split_at_mut(l);
                let gp = &split_g.0[l - 1];
                let wp = &split_w.0[l - 1];
                (
                    gp.data.as_slice(),
                    gp.w,
                    gp.h,
                    wp.data.as_slice(),
                    &mut split_g.1[0],
                    &mut split_w.1[0],
                )
            };
            reduce_into(fine, fw, fh, 3, g_next);
            reduce_into(wsrc, fw, fh, 1, w_next);
            let (dw, dh) = dims[l];
            let n = dw * dh;
            // 收紧层要现算一个权重面，其余层**直接借用**当前层的权重面（旧版在这里
            // `.clone()` 过一张纯拷贝）。
            //
            // Tightened levels need a freshly computed weight plane; every other level
            // **borrows** the current level's plane (the old code `.clone()`d a pure copy
            // here once).
            let weight: std::borrow::Cow<'_, [f32]> = if l >= tight_from {
                let inv = 1.0 / count as f32;
                std::borrow::Cow::Owned((0..n).map(|p| masks[l][p] * inv).collect())
            } else {
                std::borrow::Cow::Borrowed(wsrc)
            };
            let (xt, yt) = &tap_cache[l];
            accumulate_laplacian_level(
                &mut out_tower[l],
                &mut wsum_tower[l],
                fine,
                g_next,
                xt,
                yt,
                &weight,
            );
        }
        // 最粗层留 G_L（g_buf 的最后一个输出），永远是等权（levels-1 >= tight_from）。
        //
        // The coarsest level keeps G_L (g_buf's last output) and is always equal-weight
        // (levels-1 >= tight_from).
        let (dw, dh) = dims[levels - 1];
        let n = dw * dh;
        let inv = 1.0 / count as f32;
        let weight: Vec<f32> = (0..n).map(|p| masks[levels - 1][p] * inv).collect();
        accumulate_level(
            &mut out_tower[levels - 1],
            &mut wsum_tower[levels - 1],
            &g_buf[levels - 2],
            &weight,
        );
    }

    // 权重和近零的格子：由参考帧自己的拉普拉斯塔补上。参考帧的塔直接复用 g_buf /
    // lap_buf（尺寸恰好一致，逐位同一套 reduce / laplacian 算式）。
    //
    // For cells whose weight sum is near zero, fill from the reference frame's own
    // Laplacian tower. The reference tower reuses g_buf / lap_buf (the sizes match, and
    // the reduce / laplacian arithmetic is bitwise the same code path).
    let needs_fallback = wsum_tower.iter().any(|ws| ws.iter().any(|&v| v < WSUM_EPS));
    if needs_fallback {
        let rf = frames[reference];
        for l in 0..levels - 1 {
            let split_g;
            let (fine, fw, fh, g_next): (&[f32], usize, usize, &mut Plane) = if l == 0 {
                (rf, w, h, &mut g_buf[0])
            } else {
                split_g = g_buf.split_at_mut(l);
                let gp = &split_g.0[l - 1];
                (gp.data.as_slice(), gp.w, gp.h, &mut split_g.1[0])
            };
            reduce_into(fine, fw, fh, 3, g_next);
            // 兜底阶段测度缓存已全部释放，这一张逐层现算的拉普拉斯面是瞬态的。
            //
            // By fallback time the measure cache is fully released; this per-level
            // Laplacian plane is transient.
            let mut lap = Plane::zeros(fw, fh, 3);
            laplacian_into(fine, fw, fh, g_next, &mut lap);
            for p in 0..lap.w * lap.h {
                if wsum_tower[l][p] < WSUM_EPS {
                    for c in 0..3 {
                        let o = out_tower[l].offset(p, c);
                        out_tower[l].data[o] = lap.data[o];
                    }
                }
            }
        }
        let g = &g_buf[levels - 2];
        for p in 0..g.w * g.h {
            if wsum_tower[levels - 1][p] < WSUM_EPS {
                for c in 0..3 {
                    let o = out_tower[levels - 1].offset(p, c);
                    out_tower[levels - 1].data[o] = g.data[o];
                }
            }
        }
    }
    for l in 0..levels {
        let n = dims[l].0 * dims[l].1;
        for p in 0..n {
            let ws = wsum_tower[l][p];
            if ws >= WSUM_EPS {
                for c in 0..3 {
                    let o = out_tower[l].offset(p, c);
                    out_tower[l].data[o] /= ws;
                }
            }
        }
    }
    let mut out = collapse_in_place(&mut out_tower);
    // 无细节区（以及被分位线割掉的区）整块退回参考帧。塌缩是线性的，但各层的
    // 兜底范围不可能逐像素对齐；若只在细层兜底、粗层照旧等权混，就会在暗纹边界
    // 外留下一圈亮晕（P-C5 要掐掉的正是它）。在塌缩**之后**按最细层的门限整块
    // 覆盖，才是「这一格用参考帧」的准确含义，也不会把 NaN 带进成片。
    //
    // Featureless areas (and everything below the percentile cut) fall back wholesale to
    // the reference frame. Collapse is linear, but per-level fallback regions cannot align
    // pixel for pixel; falling back only at the fine level while coarse levels still blend
    // leaves a bright ring outside a dark edge -- exactly what P-C5 kills. Overwriting whole
    // pixels **after** collapse, using the finest-level gate, is the precise meaning of
    // "use the reference frame here", and keeps NaN out of the result.
    let rf = frames[reference];
    // masks[0] 就是 gate（逐位同值）；旧版在这里查的是由 gate 派生的 weak 布面。
    //
    // masks[0] *is* gate (bitwise the same values); the old code consulted `weak`, a bool
    // plane derived from gate.
    for p in 0..w * h {
        if masks[0][p] <= 0.0 {
            let j = p * 3;
            out[j] = rf[j];
            out[j + 1] = rf[j + 1];
            out[j + 2] = rf[j + 2];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LCG（xorshift64*）伪随机源：夹具必须可复现，不能用 `rand`。
    ///
    /// A deterministic LCG (xorshift64*) for fixtures; reproducibility matters, `rand` is
    /// not a dependency.
    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        fn next_u32(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            (x >> 32) as u32
        }

        /// `[0, 1)` 的 f32。
        ///
        /// An f32 in `[0, 1)`.
        fn unit(&mut self) -> f32 {
            (self.next_u32() >> 8) as f32 / (1u32 << 24) as f32
        }
    }

    /// `accumulate_laplacian_level` 的**逐字参照**：原循环原样抄下来（逐像素、逐通道、
    /// ytaps 外层 xtaps 内层），串行，不做任何条带展开。它代表改动前那条唯一的实现，
    /// 也是「展开版必须逐位等于谁」的答案。
    ///
    /// A **verbatim reference** for `accumulate_laplacian_level`: the original loop copied
    /// as-is (per pixel, per channel, ytaps outer and xtaps inner), serial, with no strip
    /// unrolling. It stands for the single implementation that existed before the change
    /// and answers "the unrolled version must be bitwise equal to what".
    fn accumulate_laplacian_level_reference(
        out: &mut Plane,
        wsum: &mut [f32],
        fine: &[f32],
        coarse: &Plane,
        xt: &[Vec<(usize, f32)>],
        yt: &[Vec<(usize, f32)>],
        weight: &[f32],
    ) {
        let w = out.w;
        for y in 0..out.h {
            let ytaps = &yt[y];
            for x in 0..w {
                let p = y * w + x;
                let wt = weight[p];
                if wt > 0.0 {
                    let o = p * 3;
                    let b = p * 3;
                    let xtaps = &xt[x];
                    for c in 0..3 {
                        let mut acc = 0.0f32;
                        if c < coarse.channels {
                            for &(sy, wy) in ytaps {
                                for &(sx, wx) in xtaps {
                                    acc += wy
                                        * wx
                                        * coarse.data[(sy * coarse.w + sx) * coarse.channels + c];
                                }
                            }
                        }
                        out.data[b + c] += wt * (fine[o + c] - acc);
                    }
                    wsum[p] += wt;
                }
            }
        }
    }

    /// 跑一组夹具，逐位比较 `accumulate_laplacian_level` 与逐字参照。
    ///
    /// Run one fixture and compare `accumulate_laplacian_level` against the verbatim
    /// reference, bit by bit.
    fn check_fixture(fw: usize, fh: usize, coarse_ch: usize, seed: u64) {
        let cw = fw.div_ceil(2);
        let ch = fh.div_ceil(2);
        let mut rng = Lcg::new(seed);

        // 细层故意不走 reduce 的尺寸（直接给一张随机面），只要求与 coarse 尺寸相容。
        //
        // The fine plane is random rather than a `reduce` output; only dimensional
        // compatibility with `coarse` is required.
        let fine: Vec<f32> = (0..fw * fh * 3).map(|_| rng.unit() * 2.0 - 1.0).collect();
        let coarse_data: Vec<f32> = (0..cw * ch * coarse_ch)
            .map(|_| rng.unit() * 2.0 - 1.0)
            .collect();
        let coarse = Plane {
            w: cw,
            h: ch,
            channels: coarse_ch,
            data: coarse_data,
        };
        // 权重面：每 5 个像素掐掉 1 个（wt = 0，走「跳过该像素」分支），其余取正值，
        // 且含小于 1 与大于 1 的量级，避免乘法退化成恒等。掐的是 `p % 5 == 4` 而不是
        // `== 0`：后者会让 1×1 夹具一个正权重都不剩。
        //
        // Weight plane: one pixel in five is zeroed (the "skip this pixel" branch), the
        // rest are positive and span orders below and above one so the multiply cannot
        // degenerate into an identity. The zeroed slot is `p % 5 == 4` rather than `== 0`,
        // otherwise the 1x1 fixture has no positive weight at all.
        let weight: Vec<f32> = (0..fw * fh)
            .map(|p| if p % 5 == 4 { 0.0 } else { rng.unit() * 2.0 })
            .collect();
        assert!(weight.iter().any(|&v| v > 0.0));
        if fw * fh >= 5 {
            assert!(weight.contains(&0.0));
        }

        let xt = taps(fw, cw);
        let yt = taps(fh, ch);

        let mut out_ref = Plane::zeros(fw, fh, 3);
        let mut wsum_ref = vec![0.0f32; fw * fh];
        accumulate_laplacian_level_reference(
            &mut out_ref,
            &mut wsum_ref,
            &fine,
            &coarse,
            &xt,
            &yt,
            &weight,
        );

        let mut out_new = Plane::zeros(fw, fh, 3);
        let mut wsum_new = vec![0.0f32; fw * fh];
        accumulate_laplacian_level(
            &mut out_new,
            &mut wsum_new,
            &fine,
            &coarse,
            &xt,
            &yt,
            &weight,
        );

        assert_eq!(wsum_ref.len(), wsum_new.len());
        for p in 0..fw * fh {
            assert_eq!(
                wsum_ref[p].to_bits(),
                wsum_new[p].to_bits(),
                "wsum 在第 {p} 个像素 ({}, {}) 不逐位相同",
                p % fw,
                p / fw
            );
        }
        for i in 0..fw * fh * 3 {
            assert_eq!(
                out_ref.data[i].to_bits(),
                out_new.data[i].to_bits(),
                "输出在第 {i} 个样本（像素 ({}, {}) 通道 {}）不逐位相同：ref={} new={}",
                (i / 3) % fw,
                i / 3 / fw,
                i % 3,
                out_ref.data[i],
                out_new.data[i]
            );
        }
    }

    /// 条带展开是**逐位安全**的优化：每个像素自己的加法链顺序不变，只是几条独立链
    /// 并排跑。判据用一张逐字参照实现做锚：任何改动（先乘后加换次序、交换两个 tap、
    /// 把 `wt * (fine - acc)` 改写成别的等价式）都必须让它变红。
    ///
    /// Strip unrolling is a **bitwise-safe** optimisation: each pixel keeps its own
    /// addition-chain order, several independent chains simply run side by side. The
    /// assertion anchors on a verbatim reference implementation, so any deviation (a
    /// different multiply/add order, two taps swapped, `wt * (fine - acc)` rewritten into
    /// any "equivalent" form) must turn it red.
    #[test]
    fn laplacian_strip_unrolling_bitwise_unchanged() {
        // 奇数宽高（行尾不足一个条带的尾巴必须走到）、常规尺寸（整条带铺满）、
        // 单通道粗层（`c < coarse.channels` 的另一侧）。
        //
        // Odd dimensions (the tail shorter than one strip must be exercised), an ordinary
        // size (strips tile the row exactly), and a single-channel coarse plane (the other
        // side of `c < coarse.channels`).
        for &(fw, fh) in &[(37usize, 23usize), (64, 16), (5, 3), (1, 1)] {
            check_fixture(fw, fh, 3, 0x2545_f491_4f6c_dd1d);
            check_fixture(fw, fh, 1, 0x9e37_79b9_7f4a_7c15);
        }
    }
}
