//! 筛选台：把「哪些帧该扔」从肉眼判断变成一条**可解释**的规则。
//!
//! 长延时的废片不是「拍坏了」，而是**光变了**：云影掠过、太阳被遮、拍摄者调了曝光、
//! 中途有人走过。数万帧的序列里这类帧占三到五成，逐张拖缩略图是唯一办法——这正是
//! 竞品把「亮度过滤」锁在付费墙里的原因，也是本模块存在的理由。
//!
//! 三条判定，全部以**序列自己的中位数**为基准，不用绝对阈值（不同场景的绝对亮度差
//! 三四个 EV，固定阈值放到另一条序列上必然错）：
//!
//! 1. **曝光偏离**：每帧相对中位数的 EV 超出区间就建议剔除；
//! 2. **突变**：相邻帧的 EV 跳变超过阈值——只提示，不自动剔除。真正的 Sunset 渐变
//!    里也可能有一次云开，机器不该替人做这个决定；
//! 3. **虚焦**：拉普拉斯方差低于序列中位数的某个比例。
//!
//! 判定只产出**建议**：是否落到 `flags` 上由界面决定，所以「算错了」永远只是多按一次
//! 撤销，不会悄悄改掉用户的序列。
//!
//! # Culling: an explainable rule instead of a fast pair of eyes
//!
//! Bad frames in a long timelapse are rarely "broken": the **light changed** -- a cloud shadow
//! crossed, the sun went behind something, someone nudged the exposure, a person walked through.
//! In a sequence of tens of thousands of frames such frames are 30-50% of the total, and dragging
//! thumbnails one by one is the only option -- which is exactly why competitors gate "brightness
//! filtering" behind a paywall.
//!
//! Three judgements, all relative to the **sequence's own median** rather than absolute thresholds
//! (absolute brightness differs by three or four EV between scenes, so a fixed threshold is
//! guaranteed to be wrong on the next sequence):
//!
//! 1. **Exposure deviation**: a frame whose EV relative to the median leaves the band is suggested
//!    for rejection;
//! 2. **Jumps**: a step between adjacent frames above the threshold is only *reported*, never
//!    auto-rejected -- a real sunset ramp can contain a sudden cloud opening, and the machine
//!    should not make that call;
//! 3. **Softness**: Laplacian variance below a fraction of the sequence median.
//!
//! The result is always a **suggestion**: whether it lands on `flags` is the UI's decision, so a
//! wrong call costs one undo instead of silently editing the user's sequence.

use std::path::{Path, PathBuf};

use crate::paths::canonical_loose;

use serde::{Deserialize, Serialize};

/// 筛选规则。数值一律用 EV（相对中位数）与比例，不用绝对亮度。
///
/// Culling rule. Every number is in EV (relative to the median) or a ratio, never absolute
/// brightness.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CullRule {
    /// 下界（EV，相对中位数）。低于它建议剔除。
    ///
    /// Lower bound (EV, relative to the median); below it a frame is suggested for rejection.
    pub ev_low: f64,
    /// 上界（EV，相对中位数）。高于它建议剔除。
    ///
    /// Upper bound (EV, relative to the median); above it a frame is suggested for rejection.
    pub ev_high: f64,
    /// 相邻帧跳变阈值（EV）。`0` 表示不查突变。
    ///
    /// Jump threshold between adjacent frames (EV). `0` disables the check.
    pub jump_ev: f64,
    /// 虚焦下限：清晰度低于序列中位数的这个比例即判虚焦。`0` 表示不查虚焦。
    ///
    /// Softness floor: sharpness below this fraction of the sequence median counts as soft.
    /// `0` disables the check.
    pub sharp_ratio: f64,
}

impl Default for CullRule {
    fn default() -> Self {
        // 默认值来自两条真实序列的分布：日间延时逐帧 EV 的标准差约 0.25，±1 EV 之外
        // 一定是光变了而不是正常起伏；突变 1 EV 在 2 秒间隔的序列里足够大，正常云影
        // 走不过去。
        //
        // Defaults come from two real sequences: frame-to-frame EV deviation has a standard
        // deviation of about 0.25, so anything beyond ±1 EV means the light actually changed;
        // a 1 EV step is larger than a passing cloud shadow at a 2 s interval.
        Self {
            ev_low: -1.0,
            ev_high: 1.0,
            jump_ev: 1.0,
            sharp_ratio: 0.0,
        }
    }
}

/// 建议剔除的原因。一次只报最靠前的那一条，界面据此显示一句话。
///
/// Why a frame is suggested for rejection. Only the first applicable reason is reported, so the
/// UI can show a single sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    /// 读不出来（文件损坏、格式不支持）。
    ///
    /// Unreadable (corrupt file or unsupported format).
    Unreadable,
    /// 偏暗，低于 `ev_low`。
    ///
    /// Too dark: below `ev_low`.
    TooDark,
    /// 偏亮，高于 `ev_high`。
    ///
    /// Too bright: above `ev_high`.
    TooBright,
    /// 虚焦。
    ///
    /// Out of focus.
    Soft,
}

impl Reason {
    /// 稳定的短键，落盘与命令面用它。
    ///
    /// Stable short key, used on disk and on the command surface.
    pub fn key(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::TooDark => "too-dark",
            Self::TooBright => "too-bright",
            Self::Soft => "soft",
        }
    }

    /// 中文原名，作为可翻译的模板键。
    ///
    /// Chinese source name, used as the translatable template key.
    pub fn cn(self) -> &'static str {
        match self {
            Self::Unreadable => "读不出来",
            Self::TooDark => "偏暗",
            Self::TooBright => "偏亮",
            Self::Soft => "虚焦",
        }
    }
}

/// 单帧的判定结果。
///
/// The verdict for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameVerdict {
    /// 是否建议剔除。
    ///
    /// Whether rejection is suggested.
    pub reject: bool,
    /// 建议剔除的原因；`reject` 为假时恒为 `None`。
    ///
    /// Why rejection is suggested; always `None` when `reject` is false.
    pub reason: Option<Reason>,
}

impl FrameVerdict {
    const KEEP: FrameVerdict = FrameVerdict {
        reject: false,
        reason: None,
    };

    fn with(reason: Reason) -> FrameVerdict {
        FrameVerdict {
            reject: true,
            reason: Some(reason),
        }
    }
}

/// 整条序列的判定结果。
///
/// The verdict for a whole sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    /// 逐帧判定，顺序与输入一致。
    ///
    /// Per-frame verdicts, in input order.
    pub frames: Vec<FrameVerdict>,
    /// 突变提示：`(帧下标, 跳变 EV 的绝对值)`。只是提示，不影响 `frames.reject`。
    ///
    /// Jump hints: `(frame index, absolute EV step)`. Hints only; they do not affect
    /// `frames[..].reject`.
    pub jumps: Vec<(usize, f64)>,
}

impl Verdict {
    /// 建议剔除的帧数。
    ///
    /// How many frames are suggested for rejection.
    pub fn rejected(&self) -> usize {
        self.frames.iter().filter(|f| f.reject).count()
    }

    /// 逐帧判定里被建议剔除的下标。
    ///
    /// Indices suggested for rejection.
    pub fn rejected_indices(&self) -> Vec<usize> {
        self.frames
            .iter()
            .enumerate()
            .filter(|(_, f)| f.reject)
            .map(|(i, _)| i)
            .collect()
    }
}

/// 一帧亮度折成 EV（以 0.18 中灰为 0）；亮度非正时返回 `None`。
///
/// Fold a frame's brightness into EV (0.18 mid grey is 0); returns `None` for non-positive
/// brightness.
pub fn ev(mean: f64) -> Option<f64> {
    if mean.is_finite() && mean > 0.0 {
        Some((mean / 0.18).log2())
    } else {
        None
    }
}

/// 按序列自身的离散度给出一条默认区间（EV，相对中位数）。
///
/// 用 **MAD**（中位数绝对偏差）而不是标准差：一次日落渐变里，标准差把「光在变」当成
/// 「离散大」，于是区间被拉宽到什么都判不出来；而真正的废片（少数、极端）会把标准差
/// 拉得更宽，形成自我抵消。MAD 只看中位附近的那一半，区间收紧到真正异常才越界。
///
/// 下界不低于 ±0.5 EV：太窄的区间在一条干净的序列上也会乱报，宁可漏判不可误杀。
///
/// Suggests a default band (EV, relative to the median) from the sequence's own spread.
///
/// Uses **MAD** (median absolute deviation) rather than the standard deviation: in a sunset ramp
/// the standard deviation reads "the light is changing" as "large spread" and widens the band
/// until nothing can be judged, while a handful of truly bad frames widens it further -- the two
/// effects cancel out. MAD only looks at the middle half, so the band tightens to where a frame is
/// actually anomalous. The half-width never drops below 0.5 EV: too narrow a band fires on a clean
/// sequence, and a miss is cheaper than a false accusation.
pub fn suggest_band(means: &[Option<f64>]) -> (f64, f64) {
    let mut evs: Vec<f64> = means.iter().filter_map(|m| m.and_then(ev)).collect();
    let Some(mid) = median(&mut evs) else {
        return (-1.0, 1.0);
    };
    let mut dev: Vec<f64> = evs.iter().map(|e| (e - mid).abs()).collect();
    let mad = median(&mut dev).unwrap_or(0.0);
    let sigma = 1.4826 * mad;
    let half = (3.0 * sigma).max(0.5);
    (-half, half)
}

/// 按规则判定整条序列。
///
/// `means` 是逐帧平均亮度（`None` = 读不出来），`sharp` 是逐帧清晰度（拉普拉斯方差）。
/// 两个切片长度必须与序列帧数一致；不等长时按较短的算，不会 panic。
///
/// Judge a whole sequence against a rule.
///
/// `means` holds per-frame mean brightness (`None` = unreadable) and `sharp` per-frame sharpness
/// (Laplacian variance). The two slices must match the frame count; on a length mismatch the
/// shorter one wins and nothing panics.
pub fn evaluate(means: &[Option<f64>], sharp: &[Option<f64>], rule: &CullRule) -> Verdict {
    let mut frames = Vec::with_capacity(means.len());

    // 有效帧的 EV 中位数：用中位数而不是均值，是因为要判的恰恰是离群帧——
    // 均值会被它们拽走，中位数不会。
    //
    // Median EV over the valid frames: the outliers are exactly what we are looking for, and a
    // median is not dragged by them the way a mean would be.
    let mut evs: Vec<f64> = means.iter().filter_map(|m| m.and_then(ev)).collect();
    let ref_ev = median(&mut evs);

    let mut sharps: Vec<f64> = sharp
        .iter()
        .filter_map(|s| s.filter(|v| v.is_finite() && *v > 0.0))
        .collect();
    let ref_sharp = median(&mut sharps);

    let mut jumps = Vec::new();
    let mut prev: Option<f64> = None;

    for (i, m) in means.iter().enumerate() {
        let Some(mean) = m.filter(|v| v.is_finite() && *v > 0.0) else {
            frames.push(FrameVerdict::with(Reason::Unreadable));
            prev = None;
            continue;
        };
        let Some(cur) = ev(mean) else {
            frames.push(FrameVerdict::with(Reason::Unreadable));
            prev = None;
            continue;
        };
        let rel = ref_ev.map_or(0.0, |r| cur - r);

        if rel < rule.ev_low {
            frames.push(FrameVerdict::with(Reason::TooDark));
        } else if rel > rule.ev_high {
            frames.push(FrameVerdict::with(Reason::TooBright));
        } else if rule.sharp_ratio > 0.0 {
            match (sharp.get(i).copied().flatten(), ref_sharp) {
                (Some(s), Some(r)) if s.is_finite() && s < r * rule.sharp_ratio => {
                    frames.push(FrameVerdict::with(Reason::Soft));
                }
                _ => frames.push(FrameVerdict::KEEP),
            }
        } else {
            frames.push(FrameVerdict::KEEP);
        }

        // 突变要跨过「读不出来的帧」重新起算：中间断了就没有「相邻」可言。
        //
        // Jumps restart after an unreadable frame: with a hole in between there is no
        // "adjacent" to speak of.
        if rule.jump_ev > 0.0
            && let Some(prev_ev) = prev
            && (cur - prev_ev).abs() > rule.jump_ev
        {
            jumps.push((i, (cur - prev_ev).abs()));
        }
        prev = Some(cur);
    }

    Verdict { frames, jumps }
}

/// 中位数；空输入返回 `None`。会就地把输入排序。
///
/// Median; `None` on empty input. Sorts the input in place.
fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[mid]
    } else {
        (v[mid - 1] + v[mid]) * 0.5
    })
}

/// 剔除成子序列：把没被拒收的帧复制成一条新序列。
///
/// Extract a subsequence: copy every frame that was not rejected into a new sequence.
///
/// 路径安全（[`check_target`]）先于一切 IO；复制时保留 mtime，这样测光缓存
/// 的 `(mtime, len)` 键在新序列里继续命中，几百帧的账不用重算。
///
/// Path safety ([`check_target`]) comes before any IO; copies preserve mtime so the metering
/// cache's `(mtime, len)` keys keep hitting in the new sequence and a few hundred frames of
/// measurements are not recomputed.
pub struct ExtractPlan {
    /// 会复制哪些帧（源绝对路径，按传入顺序）。
    ///
    /// Which frames would be copied (source absolute paths, in the given order).
    pub kept: Vec<PathBuf>,
    /// 这些帧的总字节数（体积预估的依据）。
    ///
    /// Total bytes of those frames (the size estimate).
    pub bytes: u64,
}

/// 目标路径不安全的原因。
///
/// Why a target path is unsafe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetError {
    /// 目标就是源序列根，或落在源序列根里面——往自己肚子里复制只会越滚越大。
    ///
    /// The target is the source root or inside it -- copying into itself only grows.
    InsideSource,
    /// 目标是盘符根或没有父目录——整盘根目录不是一个序列该待的地方。
    ///
    /// The target is a drive root or has no parent -- a drive root is no home for a sequence.
    DriveRoot,
    /// 目标已存在且非空——不覆盖任何东西，让用户换一个目录。
    ///
    /// The target exists and is non-empty -- nothing gets overwritten; pick another folder.
    NotEmpty,
}

/// 抽取失败的原因：目标不安全，或复制途中 IO 出错。
///
/// Why an extraction failed: an unsafe target, or an IO error mid-copy.
#[derive(Debug)]
pub enum ExtractError {
    Target(TargetError),
    Io(std::io::Error),
}

impl TargetError {
    pub fn cn(self) -> &'static str {
        match self {
            Self::InsideSource => "目标不能是源序列根或其内部",
            Self::DriveRoot => "目标不能是盘符根或没有父目录的路径",
            Self::NotEmpty => "目标目录已存在且非空：换一个目录，或先清空它",
        }
    }
}

/// 目标路径安全检查：不是源内部、不是盘根、不存在或为空。
///
/// Target path safety: not inside the source, not a drive root, absent or empty.
pub fn check_target(source_root: &Path, target: &Path) -> Result<(), TargetError> {
    // 盘根（`E://`）与无父目录的路径都不收。
    //
    // Drive roots (`E://`) and parentless paths are both refused.
    if target
        .parent()
        .map(|p| p.as_os_str().is_empty())
        .unwrap_or(true)
    {
        return Err(TargetError::DriveRoot);
    }
    let src = canonical_loose(source_root);
    let dst = canonical_loose(target);
    if dst.starts_with(&src) {
        return Err(TargetError::InsideSource);
    }
    if let Ok(mut it) = std::fs::read_dir(target)
        && it.next().is_some()
    {
        return Err(TargetError::NotEmpty);
    }
    Ok(())
}

/// 体积预估：保留帧的个数与总字节，一个字节都不写。
///
/// Size estimate: how many frames would be copied and their total bytes, without writing
/// anything.
pub fn plan_extract(kept: &[PathBuf]) -> ExtractPlan {
    let bytes = kept
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    ExtractPlan {
        kept: kept.to_vec(),
        bytes,
    }
}

/// 执行复制：帧进目标根（平铺，重名直接报错），测光缓存跟着走。
/// 返回实际写入的字节数。源序列一个字节都不动。
///
/// Run the copy: frames land flat in the target root (duplicate names are an error) and the
/// metering cache comes along. Returns the bytes actually written. The source is never
/// touched.
pub fn extract(source_root: &Path, kept: &[PathBuf], target: &Path) -> Result<u64, ExtractError> {
    check_target(source_root, target).map_err(ExtractError::Target)?;
    std::fs::create_dir_all(target).map_err(ExtractError::Io)?;
    let mut written = 0u64;
    let mut seen = std::collections::BTreeSet::new();
    for src in kept {
        let name = src
            .file_name()
            .ok_or_else(|| {
                ExtractError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "frame without a file name",
                ))
            })?
            .to_owned();
        if !seen.insert(name.clone()) {
            return Err(ExtractError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("duplicate file name {name:?}"),
            )));
        }
        let dst = target.join(&name);
        let meta = std::fs::metadata(src).map_err(ExtractError::Io)?;
        let modified = meta.modified().map_err(ExtractError::Io)?;
        std::fs::copy(src, &dst).map_err(ExtractError::Io)?;
        // mtime 原样带过去：测光缓存按 (mtime, len) 记账，时间一变整册重测。
        //
        // Carry the mtime over: the metering cache keys on (mtime, len), and a new time would
        // force a full re-measure.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&dst)
            .map_err(ExtractError::Io)?;
        f.set_times(std::fs::FileTimes::new().set_modified(modified))
            .map_err(ExtractError::Io)?;
        written += meta.len();
    }
    carry_metering_cache(source_root, target)?;
    Ok(written)
}

/// 把测光缓存整份带到目标序列：**缓存只是加速层，缺失不算失败**。
///
/// 键是相对路径 + (mtime, len)，平铺且保留了 mtime 时新序列直接命中；子目录素材的键
/// 对不上，退化为一次全量重测，同样无害。所以这条路上任何一种「带不过去」都不该让
/// 整次导出失败——早先它是硬 `copy`，撞上后台测光线程的替换窗口就报 `os error 2`，
/// 一个可选加速层把导出拖下水（实测偶发，日志里是「复制失败：系统找不到指定的文件」）。
///
/// 做法：`NotFound` 重试三次（每次 10 ms，窗口是微秒级，等一轮基本就过了），仍然
/// `NotFound` 就跳过；其它 IO 错误（权限、磁盘满）照旧往上抛，那是真问题。
///
/// Carry the metering cache over to the target sequence: **the cache is only an
/// acceleration layer, so a missing one is not a failure**.
///
/// Keys are relative paths plus (mtime, len), so a flat layout with preserved mtimes hits
/// directly, and sub-folder material misses and degrades to one full re-measure -- equally
/// harmless. Nothing on this path should fail the export, then: it used to be a hard `copy`
/// that hit the background metering thread's replace window and reported `os error 2`, an
/// optional accelerator sinking the export (observed intermittently; the log says "copy
/// failed: the system cannot find the file specified").
///
/// So: `NotFound` is retried three times 10 ms apart (the window is microseconds, so one
/// wait usually clears it) and then skipped; every other IO error (permissions, a full
/// disk) still propagates -- those are real problems.
fn carry_metering_cache(source_root: &Path, target: &Path) -> Result<(), ExtractError> {
    // 源上没有缓存：没什么可带的，也不必在目标里建一个空护带目录。
    //
    // No cache at the source: nothing to carry, and no empty guard directory to create.
    let cache = crate::measures::cache_path(source_root);
    if !cache.is_file() {
        return Ok(());
    }
    let dst = crate::measures::cache_path(target);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(ExtractError::Io)?;
    }
    let mut retries = 0u32;
    loop {
        match std::fs::copy(&cache, &dst) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 撞上「写临时文件 → 替换」之间的窗口，或缓存被删了。
                //
                // The replace window, or the cache really is gone.
                if retries >= 3 {
                    return Ok(());
                }
                retries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(ExtractError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一帧 0.18 中灰 = 0 EV；把 EV 换回线性亮度，测试里读起来更直观。
    ///
    /// 0.18 mid grey is 0 EV; converting back to linear keeps the tests readable.
    fn from_ev(e: f64) -> f64 {
        0.18 * 2f64.powf(e)
    }

    fn means_of(evs: &[f64]) -> Vec<Option<f64>> {
        evs.iter().map(|e| Some(from_ev(*e))).collect()
    }

    fn flat_sharp(n: usize) -> Vec<Option<f64>> {
        vec![Some(100.0); n]
    }

    #[test]
    fn a_steady_ramp_is_never_culled_by_the_suggested_band() {
        // 日落渐变：整条序列平滑走了 3 档。**默认的 ±1 EV 会误杀两头**——这正是
        // `suggest_band` 存在的理由：区间必须由序列自己的离散度长出来。
        //
        // A sunset ramp covering three stops. The fixed ±1 EV default would kill both ends --
        // which is exactly why `suggest_band` derives the band from the sequence's own spread.
        let evs: Vec<f64> = (0..60).map(|i| -1.5 + 3.0 * i as f64 / 59.0).collect();
        let means = means_of(&evs);
        let (lo, hi) = suggest_band(&means);
        let rule = CullRule {
            ev_low: lo,
            ev_high: hi,
            ..CullRule::default()
        };
        let v = evaluate(&means, &flat_sharp(60), &rule);
        assert_eq!(
            v.rejected(),
            0,
            "平滑渐变不该有帧被剔除（区间 {lo:.2}..{hi:.2}）：{:?}",
            v.frames.iter().filter(|f| f.reject).count()
        );
        assert!(v.jumps.is_empty(), "平滑渐变的跳变：{:?}", v.jumps);

        // 反过来也要成立：固定窄区间时，渐变两端**确实**会被判出去——说明判定不是永远放行。
        //
        // The converse must hold too: with a fixed narrow band the ramp ends *are* culled, so the
        // judgement is not simply "always pass".
        let narrow = CullRule {
            ev_low: -1.0,
            ev_high: 1.0,
            ..CullRule::default()
        };
        assert!(evaluate(&means, &flat_sharp(60), &narrow).rejected() > 0);
    }

    #[test]
    fn the_suggested_band_admits_a_clean_ramp_and_rejects_a_true_outlier() {
        let mut evs: Vec<f64> = (0..40).map(|i| -1.0 + 2.0 * i as f64 / 39.0).collect();
        evs[20] = -4.0; // 一帧被遮住 / one frame blacked out
        let means = means_of(&evs);
        let (lo, hi) = suggest_band(&means);
        let rule = CullRule {
            ev_low: lo,
            ev_high: hi,
            ..CullRule::default()
        };
        let v = evaluate(&means, &flat_sharp(40), &rule);
        assert_eq!(
            v.rejected_indices(),
            vec![20],
            "只该揪出那一帧（区间 {lo:.2}..{hi:.2}）"
        );
    }

    #[test]
    fn frames_far_from_the_median_are_both_sides() {
        // 中间 20 帧正常，两端各塞 3 帧极端值：偏暗与偏亮都要报出来，方向不能混。
        //
        // Twenty normal frames with three extreme ones at each end: both directions must be
        // reported and never mixed up.
        let mut evs = vec![0.0; 20];
        evs[0..3].fill(-3.0);
        evs[17..20].fill(2.5);
        let v = evaluate(&means_of(&evs), &flat_sharp(20), &CullRule::default());
        assert_eq!(v.rejected_indices(), vec![0, 1, 2, 17, 18, 19]);
        assert_eq!(v.frames[0].reason, Some(Reason::TooDark));
        assert_eq!(v.frames[19].reason, Some(Reason::TooBright));
    }

    #[test]
    fn a_minority_of_bad_frames_does_not_move_the_reference() {
        // 40% 是废片时，均值会被拽走 0.8 EV，中位数不动——这正是用中位数的原因。
        //
        // With 40% bad frames a mean would shift by 0.8 EV while the median holds -- which is
        // exactly why the median is used.
        let mut evs = vec![0.0; 12];
        evs[0..5].fill(-2.0);
        let v = evaluate(&means_of(&evs), &flat_sharp(12), &CullRule::default());
        assert_eq!(v.rejected(), 5, "只有那 5 帧该被剔除");
        assert!(v.frames[5..].iter().all(|f| !f.reject));
    }

    #[test]
    fn unreadable_frames_are_reported_and_never_break_the_chain() {
        let means = vec![Some(from_ev(0.0)), None, Some(from_ev(0.0))];
        let v = evaluate(&means, &flat_sharp(3), &CullRule::default());
        assert_eq!(v.frames[1].reason, Some(Reason::Unreadable));
        assert!(v.frames[1].reject);
        // 中间断了一帧，两侧之间不该报「相邻跳变」。
        //
        // With a hole in the middle there is no adjacent step across it.
        assert!(v.jumps.is_empty());
    }

    #[test]
    fn a_single_spike_is_reported_as_a_jump_but_not_rejected() {
        // 只跳一帧又跳回来：曝光仍在区间内，所以不该剔除，但必须作为突变提示出来。
        //
        // One frame jumps and comes back: exposure stays inside the band, so it must not be
        // rejected, but the step has to be reported.
        let mut evs = vec![0.0; 10];
        evs[5] = 0.9;
        let rule = CullRule {
            jump_ev: 0.5,
            ..CullRule::default()
        };
        let v = evaluate(&means_of(&evs), &flat_sharp(10), &rule);
        assert_eq!(v.rejected(), 0);
        assert_eq!(v.jumps.len(), 2, "进出各一次跳变：{:?}", v.jumps);
        assert_eq!(v.jumps[0].0, 5);
    }

    #[test]
    fn softness_culls_only_below_the_ratio() {
        let means = means_of(&[0.0; 6]);
        let mut sharp = vec![Some(100.0); 6];
        sharp[2] = Some(20.0);
        let rule = CullRule {
            sharp_ratio: 0.5,
            ..CullRule::default()
        };
        let v = evaluate(&means, &sharp, &rule);
        assert_eq!(v.rejected_indices(), vec![2]);
        assert_eq!(v.frames[2].reason, Some(Reason::Soft));
    }

    #[test]
    fn exposure_outranks_softness() {
        // 同时偏暗又虚焦时报偏暗：一次只说一句，先说更要紧的那句。
        //
        // Dark and soft at once reports dark: one sentence at a time, the more important one.
        let means = vec![Some(from_ev(-3.0)), Some(from_ev(0.0))];
        let sharp = vec![Some(10.0), Some(100.0)];
        let rule = CullRule {
            sharp_ratio: 0.5,
            ..CullRule::default()
        };
        let v = evaluate(&means, &sharp, &rule);
        assert_eq!(v.frames[0].reason, Some(Reason::TooDark));
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let v = evaluate(&[], &[], &CullRule::default());
        assert!(v.frames.is_empty());

        let v = evaluate(&[Some(0.2)], &[Some(1.0)], &CullRule::default());
        assert_eq!(v.rejected(), 0, "只有一帧时没有基准，不该判任何帧");

        // 长度不一致时按较短的算，不会越界。
        //
        // On a length mismatch the shorter slice wins and nothing goes out of bounds.
        let v = evaluate(
            &means_of(&[0.0, 1.0, 2.0]),
            &[Some(1.0)],
            &CullRule::default(),
        );
        assert_eq!(v.frames.len(), 3);
    }

    #[test]
    fn zero_and_non_finite_brightness_is_unreadable() {
        let means = vec![Some(0.0), Some(f64::NAN), Some(from_ev(0.0))];
        let v = evaluate(&means, &flat_sharp(3), &CullRule::default());
        assert_eq!(v.frames[0].reason, Some(Reason::Unreadable));
        assert_eq!(v.frames[1].reason, Some(Reason::Unreadable));
        assert!(!v.frames[2].reject);
    }

    /// 抽取：保留帧逐份复制、mtime 原样、测光缓存跟着走，源序列一个字节不动。
    ///
    /// Extraction: kept frames copied one by one, mtimes preserved, the metering cache along,
    /// and the source not touched by a single byte.
    #[test]
    fn extract_copies_kept_frames_and_carries_the_metering_cache() {
        let base = std::env::temp_dir().join(format!(
            "aevum-extract-core-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let src = base.join("src");
        std::fs::create_dir_all(src.join(".aevum")).expect("建源序列");
        let mut src_bytes = Vec::new();
        for (i, v) in [10u8, 20, 30].iter().enumerate() {
            let img = image::RgbImage::from_fn(4, 4, |_, _| image::Rgb([*v, *v, *v]));
            let p = src.join(format!("s{i:03}.png"));
            img.save(&p).expect("写源帧");
            src_bytes.push(std::fs::read(&p).expect("读源帧"));
        }
        // 源序列留一份测光缓存，抽取后必须原样出现在目标。
        //
        // Leave a metering cache in the source; it must reappear unchanged in the target.
        std::fs::write(src.join(".aevum/measures.json"), b"{\"version\":1}").expect("写缓存");

        let kept = vec![src.join("s000.png"), src.join("s002.png")];
        let plan = plan_extract(&kept);
        assert_eq!(plan.kept.len(), 2);
        assert!(plan.bytes > 0, "体积预估要拿到实际字节");

        let dst = base.join("dst");
        let written = extract(&src, &kept, &dst).expect("抽取");
        // 目标帧数 = 保留数，且内容逐字节一致（加 .aevum 目录一共三项）。
        //
        // Target frame count = kept count, contents match byte for byte (two frames plus the
        // .aevum folder).
        let names: Vec<_> = std::fs::read_dir(&dst)
            .expect("列目标")
            .map(|e| e.expect("条目").file_name())
            .collect();
        assert_eq!(names.len(), 3);
        for name in ["s000.png", "s002.png"] {
            let copied = std::fs::read(dst.join(name)).expect("读复制帧");
            let want = &src_bytes[if name == "s000.png" { 0 } else { 2 }];
            assert_eq!(&copied, want, "{name} 内容不一致");
        }
        // mtime 原样：测光缓存的 (mtime, len) 键要继续命中。
        //
        // The mtime carries over: the cache's (mtime, len) keys must keep hitting.
        for name in ["s000.png", "s002.png"] {
            let a = std::fs::metadata(src.join(name))
                .expect("源元数据")
                .modified()
                .expect("源 mtime");
            let b = std::fs::metadata(dst.join(name))
                .expect("目标元数据")
                .modified()
                .expect("目标 mtime");
            assert_eq!(a, b, "{name} 的 mtime 没保留");
        }
        // 测光缓存跟着走。
        //
        // The metering cache comes along.
        assert_eq!(
            std::fs::read(dst.join(".aevum/measures.json")).expect("读目标缓存"),
            b"{\"version\":1}"
        );
        assert!(written > 0);
        // 源序列一个字节都不动。
        //
        // The source is untouched.
        for (i, bytes) in src_bytes.iter().enumerate() {
            assert_eq!(
                &std::fs::read(src.join(format!("s{i:03}.png"))).expect("再读源帧"),
                bytes,
                "第 {i} 帧被改写了"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 目标路径安全：源内部、盘根、非空目录都拒绝，且拒绝发生在任何写入之前。
    ///
    /// Target safety: inside the source, a drive root and a non-empty folder are all refused,
    /// before anything is written.
    #[test]
    fn extract_refuses_unsafe_targets() {
        let base = std::env::temp_dir().join(format!(
            "aevum-extract-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let src = base.join("src");
        std::fs::create_dir_all(&src).expect("建源");
        let img = image::RgbImage::from_fn(4, 4, |_, _| image::Rgb([8, 8, 8]));
        img.save(src.join("s000.png")).expect("写帧");

        // 源内部（包括源根本身）。
        //
        // Inside the source (including the root itself).
        assert_eq!(
            check_target(&src, &src.join("sub")),
            Err(TargetError::InsideSource)
        );
        assert_eq!(check_target(&src, &src), Err(TargetError::InsideSource));
        // 盘根：`E:////` 没有更上层的父目录。
        //
        // A drive root: `E:////` has no parent above it.
        assert_eq!(
            check_target(&src, std::path::Path::new("E:////")),
            Err(TargetError::DriveRoot)
        );
        // 非空目录。
        //
        // A non-empty folder.
        let occupied = base.join("occupied");
        std::fs::create_dir_all(&occupied).expect("建占用目录");
        std::fs::write(occupied.join("x.txt"), b"x").expect("占位");
        assert_eq!(check_target(&src, &occupied), Err(TargetError::NotEmpty));
        // 占用目录里一个文件都不该被写出来。
        //
        // Not a single file may appear inside the occupied folder.
        assert_eq!(std::fs::read_dir(&occupied).expect("列占用").count(), 1);
        // 空目录与不存在的目录都放行。
        //
        // Empty and absent folders both pass.
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).expect("建空目录");
        assert_eq!(check_target(&src, &empty), Ok(()));
        assert_eq!(check_target(&src, &base.join("absent")), Ok(()));
        let _ = std::fs::remove_dir_all(&base);
    }
}
