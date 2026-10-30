//! 序列体检：从逐帧测光里挑出坏帧 / 黑帧 / 虚焦 / 亮度离群 / 重复 / 缺口。
//!
//! 判定口径与 `oracle` 那一侧的 Python 实现（`lapse/qa.py`）逐条对齐，
//! 本模块不读图、不起线程：输入是 `metrics::FrameMetrics` 的逐帧表，
//! 输出是一张可展示的清单，界面只负责画它。
//!
//! 为什么不用绝对阈值：
//! - `sharp` 是 0~255 灰度上的拉普拉斯方差，量纲随分辨率、降噪与构图变化，
//!   写死一个小数只对某种归一化成立。
//! - 延时序列的亮度本身就在变（日转夜能差十几档），拿全序列中位数算 z 分数
//!   会把日出和日落两头全报成离群——误报一多，这张表就没人看了。
//!
//! 所以判定改成**跟左右邻居各比一次**：渐变时左偏正、右偏负（自己正好落在趋势线上），
//! 台阶处一侧偏离≈0，两种都归零；只有单帧被碰歪、虚焦、曝光飞了才两侧同向。
//! 门槛 = max(绝对下限, sig × 本序列偏离的中位数)，噪声大的序列自动放宽，
//! 干净序列仍能抓到 5% 的亮度跳变。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::metrics::FrameMetrics;

/// 一帧的体检输入：路径（缺口按编号找）、文件大小（重复帧比对）、测光结果。
#[derive(Clone, Debug)]
pub struct Probe {
    pub dir: String,
    pub name: String,
    pub size: i64,
    pub sample: Option<FrameMetrics>,
}

/// 体检参数。默认值沿用 Python 侧调好的那组，界面上只暴露阈值松紧一档。
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// 亮度离群的相对门槛（本序列波动的倍数）。
    pub sig: f64,
    /// 虚焦的相对门槛。
    pub blur_sig: f64,
    /// 平均亮度低于它就是黑帧（0~1 域）。
    pub black: f64,
    /// 左右各看几帧。
    pub window: usize,
    /// 亮度跳变的绝对下限（对数域，0.05 ≈ 亮度 5%）。
    pub luma_floor: f64,
    /// 清晰度掉多少才算虚焦（对数域，0.30 ≈ 邻居的 26%）。
    pub blur_floor: f64,
    pub gaps: bool,
    pub dup: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            sig: 3.0,
            blur_sig: 3.0,
            black: 0.01,
            window: 9,
            luma_floor: 0.05,
            blur_floor: 0.30,
            gaps: true,
            dup: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Read,
    Black,
    Blur,
    Dup,
    LumaOut,
    Gap,
}

impl Kind {
    /// 对外稳定的机器名（命令返回、日志与筛选都用它）。
    pub fn id(self) -> &'static str {
        match self {
            Kind::Read => "read",
            Kind::Black => "black",
            Kind::Blur => "blur",
            Kind::Dup => "dup",
            Kind::LumaOut => "luma_out",
            Kind::Gap => "gap",
        }
    }

    pub fn cn(self) -> &'static str {
        match self {
            Kind::Read => "读不出来",
            Kind::Black => "几乎全黑",
            Kind::Blur => "疑似虚焦",
            Kind::Dup => "疑似重复",
            Kind::LumaOut => "亮度离群",
            Kind::Gap => "序列缺口",
        }
    }

    /// 缺口对应的文件根本不存在，无从「排除这几帧」，只能在表里告知。
    pub fn excludable(self) -> bool {
        self != Kind::Gap
    }

    fn order(self) -> usize {
        match self {
            Kind::Read => 0,
            Kind::Black => 1,
            Kind::Blur => 2,
            Kind::Dup => 3,
            Kind::LumaOut => 4,
            Kind::Gap => 5,
        }
    }
}

/// 一条发现。`frame` 是 0 基帧号（对外展示与命令返回时统一转 1 基）。
#[derive(Clone, Debug)]
pub struct Finding {
    pub frame: usize,
    pub kind: Kind,
    /// 判定用的数值：黑帧是亮度、离群也是亮度、虚焦是清晰度、缺口是缺失帧数。
    pub value: f64,
    pub note: String,
}

impl Finding {
    /// 命令与无头工具共用的对外形状：帧号 1 基，机器名与中文标签都给。
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "frame": self.frame + 1,
            "kind": self.kind.id(),
            "label": self.kind.cn(),
            "value": (self.value * 1e6).round() / 1e6,
            "note": self.note,
            "excludable": self.kind.excludable(),
        })
    }
}

/// 一组发现的对外形状。
pub fn findings_json(v: &[Finding]) -> Vec<serde_json::Value> {
    v.iter().map(Finding::json).collect()
}

/// 少于这么多可判定的帧就不做离群判定（统计没意义）。
pub const MIN_SAMPLES: usize = 8;
/// 左右各至少几帧才构成可信的局部中位数。
pub const MIN_SIDE: usize = 3;

/// 单帧体检输入：只取目录、文件名与文件大小（一次 `stat`），不解码。
/// 分批跑长任务时用它逐帧攒表，进度与取消都能按帧粒度报。
pub fn probe(path: &Path, sample: Option<FrameMetrics>) -> Probe {
    Probe {
        dir: path
            .parent()
            .map(|d| d.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
        name: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        size: std::fs::metadata(path)
            .map(|m| m.len() as i64)
            .unwrap_or(-1),
        sample,
    }
}

/// 从磁盘取整段序列的体检输入：只逐帧 `stat`，测光表由调用方给（界面里已经随导入
/// 算好了，不该为同一批帧再解一遍）。
pub fn probes(paths: &[PathBuf], samples: &[Option<FrameMetrics>]) -> Vec<Probe> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| probe(p, samples.get(i).copied().flatten()))
        .collect()
}

/// 逐帧体检，返回按（帧号，类型）排序的清单。
pub fn audit(v: &[Probe], opts: &Options) -> Vec<Finding> {
    let n = v.len();
    let mut found: BTreeMap<usize, Vec<Finding>> = BTreeMap::new();
    let mut add = |i: usize, kind: Kind, value: f64, note: String| {
        found.entry(i).or_default().push(Finding {
            frame: i,
            kind,
            value,
            note,
        });
    };

    let means: Vec<f64> = v
        .iter()
        .map(|p| p.sample.map_or(f64::NAN, |m| m.mean))
        .collect();
    let sharps: Vec<f64> = v
        .iter()
        .map(|p| p.sample.map_or(f64::NAN, |m| m.sharp))
        .collect();

    for (i, p) in v.iter().enumerate() {
        match p.sample {
            None => add(
                i,
                Kind::Read,
                0.0,
                "这一帧读不出来（文件损坏或不是图片）".to_owned(),
            ),
            Some(m) if m.mean < opts.black => add(
                i,
                Kind::Black,
                m.mean,
                format!("亮度只有 {:.4}，可能是误拍或镜头盖", m.mean),
            ),
            Some(_) => {}
        }
    }

    let half = opts.window.max(MIN_SIDE);
    if n >= MIN_SAMPLES {
        let log_sharp: Vec<f64> = sharps
            .iter()
            .map(|x| {
                if x.is_finite() {
                    x.max(1e-6).ln()
                } else {
                    f64::NAN
                }
            })
            .collect();
        let (dev, sgn, judged) = resid(&log_sharp, half);
        let (thr, _) = threshold(&dev, &judged, opts.blur_sig, opts.blur_floor);
        for i in 0..n {
            if dev[i] > thr && sgn[i] < 0.0 {
                add(
                    i,
                    Kind::Blur,
                    sharps[i],
                    format!(
                        "清晰度只有左右邻居的 {}，大概虚焦或抖糊了",
                        pct((-dev[i]).exp())
                    ),
                );
            }
        }
        let log_mean: Vec<f64> = means
            .iter()
            .map(|x| {
                if x.is_finite() {
                    x.max(1e-6).ln()
                } else {
                    f64::NAN
                }
            })
            .collect();
        let (dev, sgn, judged) = resid(&log_mean, half);
        let (thr, _) = threshold(&dev, &judged, opts.sig, opts.luma_floor);
        for i in 0..n {
            if dev[i] > thr {
                add(
                    i,
                    Kind::LumaOut,
                    means[i],
                    format!(
                        "亮度比左右邻居的中位数{} {:.0}%（{:+.1} 档）",
                        if sgn[i] > 0.0 { "高" } else { "低" },
                        100.0 * dev[i].exp_m1(),
                        sgn[i] * dev[i] / std::f64::consts::LN_2,
                    ),
                );
            }
        }
    }

    if opts.dup && n >= 2 {
        for i in 1..n {
            let (a, b) = (&v[i - 1], &v[i]);
            let (Some(x), Some(y)) = (a.sample, b.sample) else {
                continue;
            };
            let same_size = b.size > 0 && b.size == a.size;
            let same_measure = [x.mean, x.median, x.center, x.sharp]
                .iter()
                .zip([y.mean, y.median, y.center, y.sharp])
                .all(|(p, q)| (p - q).abs() < 1e-9);
            if same_size && same_measure {
                add(
                    i,
                    Kind::Dup,
                    means[i],
                    "与上一帧的测量和文件大小完全一致，可能是快门没动作".to_owned(),
                );
            }
        }
    }

    // 黑帧/坏帧的虚焦与离群表现只是同一件事的副作用，不重复列；
    // 曝光飞了的帧对比度也塌了，清晰度结论并入亮度离群。
    for items in found.values_mut() {
        let kinds: Vec<Kind> = items.iter().map(|f| f.kind).collect();
        let mut drop: Vec<Kind> = Vec::new();
        if kinds.contains(&Kind::Read) || kinds.contains(&Kind::Black) {
            drop.push(Kind::Blur);
            drop.push(Kind::LumaOut);
        }
        if kinds.contains(&Kind::LumaOut) {
            drop.push(Kind::Blur);
        }
        if !drop.is_empty() {
            items.retain(|f| !drop.contains(&f.kind));
        }
    }

    let mut out: Vec<Finding> = found.into_values().flatten().collect();
    if opts.gaps {
        for (after, missing, msg) in gaps(v) {
            out.push(Finding {
                frame: after,
                kind: Kind::Gap,
                value: missing as f64,
                note: msg,
            });
        }
    }
    out.sort_by_key(|f| (f.frame, f.kind.order()));
    out
}

/// {类型机器名: 帧数}，给界面写「发现 3 处：几乎全黑 1 / 疑似虚焦 1 …」。
pub fn count_by_kind(findings: &[Finding]) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for f in findings {
        *out.entry(f.kind.id()).or_insert(0usize) += 1;
    }
    out
}

/// 把「是邻居的几成」写成人话：糊到 1/250 时报成 0% 等于没说。
fn pct(r: f64) -> String {
    if r >= 0.1 {
        format!("{:.0}%", 100.0 * r)
    } else {
        format!("1/{:.0}", 1.0 / r.max(1e-9))
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_unstable_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) * 0.5
    }
}

/// log 序列 -> (同向偏离量, 方向, 是否判得到)。
///
/// 左偏离 = y[i] − 左侧窗中位数，右偏离 = y[i] − 右侧窗中位数。渐变时两者异号、
/// 台阶时一侧接近 0，都归零；离群帧两侧同号，取较小者当偏离量，所以要求异常
/// 既突出又孤立。够不到 `MIN_SIDE` 个邻居的帧头帧尾判不到（dev 记 0）。
fn resid(y: &[f64], half: usize) -> (Vec<f64>, Vec<f64>, Vec<bool>) {
    let n = y.len();
    let mut dev = vec![0.0; n];
    let mut sgn = vec![0.0; n];
    let mut judged = vec![false; n];
    for i in 0..n {
        if !y[i].is_finite() {
            continue;
        }
        let left: Vec<f64> = y[i.saturating_sub(half)..i]
            .iter()
            .copied()
            .filter(|x| x.is_finite())
            .collect();
        let right: Vec<f64> = y[(i + 1).min(n)..(i + 1 + half).min(n)]
            .iter()
            .copied()
            .filter(|x| x.is_finite())
            .collect();
        if left.len() < MIN_SIDE || right.len() < MIN_SIDE {
            continue;
        }
        judged[i] = true;
        let rl = y[i] - median(left);
        let rr = y[i] - median(right);
        if rl * rr <= 0.0 {
            continue;
        }
        dev[i] = rl.abs().min(rr.abs());
        sgn[i] = if rl > 0.0 { 1.0 } else { -1.0 };
    }
    (dev, sgn, judged)
}

/// 门槛 = max(绝对下限, sig × 本序列偏离中位数)。
///
/// 中位数把判不到/被趋势吃掉的那些帧（dev=0）也算进去，所以干净序列的门槛
/// 就等于下限，不会因为只有一两个离群帧把自己的统计量抬起来。
fn threshold(dev: &[f64], judged: &[bool], sig: f64, floor: f64) -> (f64, f64) {
    if judged.iter().filter(|b| **b).count() < MIN_SAMPLES {
        return (f64::INFINITY, 0.0);
    }
    let jitter = median(dev.to_vec());
    (floor.max(sig * jitter), jitter)
}

/// 同目录同前缀的编号缺口：[(该缺口后一帧的下标, 缺失个数, 文字)]。
///
/// 编号取文件名末尾的连续数字；一个分组里不足两个编号文件就不算序列。
fn gaps(v: &[Probe]) -> Vec<(usize, usize, String)> {
    let mut groups: BTreeMap<(String, String), Vec<(u64, usize)>> = BTreeMap::new();
    for (i, p) in v.iter().enumerate() {
        let Some((prefix, num)) = numbered(&p.name) else {
            continue;
        };
        groups
            .entry((p.dir.clone(), prefix))
            .or_default()
            .push((num, i));
    }
    let mut out = Vec::new();
    for items in groups.values() {
        if items.len() < 2 {
            continue;
        }
        let mut items = items.clone();
        items.sort_by_key(|(num, _)| *num);
        let present: std::collections::HashSet<u64> = items.iter().map(|(n, _)| *n).collect();
        let lo = items[0].0;
        let hi = items[items.len() - 1].0;
        let missing: Vec<u64> = (lo..=hi).filter(|n| !present.contains(n)).collect();
        if missing.is_empty() {
            continue;
        }
        let mut runs: Vec<Vec<u64>> = Vec::new();
        let mut cur = vec![missing[0]];
        for &m in &missing[1..] {
            if m == cur[cur.len() - 1] + 1 {
                cur.push(m);
            } else {
                runs.push(std::mem::take(&mut cur));
                cur = vec![m];
            }
        }
        runs.push(cur);
        for r in runs {
            let Some((_, after)) = items.iter().find(|(n, _)| *n > r[r.len() - 1]) else {
                continue;
            };
            let span = if r.len() == 1 {
                format!("{}", r[0])
            } else {
                format!("{}–{}", r[0], r[r.len() - 1])
            };
            out.push((
                *after,
                r.len(),
                format!("缺 {} 帧（编号 {}）：成片里会按上一帧停留", r.len(), span),
            ));
        }
    }
    out
}

/// 文件名 → (去掉尾部编号的前缀, 编号)。取法是「去掉末尾那一串连续 ASCII 数字」，
/// 与 Python 的 `^(.*?)(\d+)$` 等价（前缀最短、数字最长）。没有尾部数字、或数字长到
/// `u64` 装不下（那是文件名里带了别的东西，不是相机编号）都算不上一帧编号。
fn numbered(name: &str) -> Option<(String, u64)> {
    let stem = match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    };
    let bytes = stem.as_bytes();
    let mut start = bytes.len();
    while start > 0 && bytes[start - 1].is_ascii_digit() {
        start -= 1;
    }
    if start == bytes.len() {
        return None;
    }
    let num: u64 = stem[start..].parse().ok()?;
    Some((stem[..start].to_owned(), num))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samp(mean: f64, sharp: f64) -> Option<FrameMetrics> {
        Some(FrameMetrics {
            mean,
            median: mean,
            center: mean,
            sharp,
        })
    }

    fn probe(name: &str, size: i64, sample: Option<FrameMetrics>) -> Probe {
        Probe {
            dir: "D:/seq".to_owned(),
            name: name.to_owned(),
            size,
            sample,
        }
    }

    /// 一条「干净」序列：亮度缓慢渐变、清晰度带 ±1% 抖动、每帧大小不同。
    fn clean(n: usize) -> Vec<Probe> {
        (0..n)
            .map(|i| {
                let t = i as f64;
                let mean = 0.55 * (1.0 - t / (n as f64) * 0.25);
                let jitter = 1.0 + ((i * 7) % 13) as f64 * 0.0015;
                probe(
                    &format!("IMG_{:04}.png", i + 1),
                    40_000_000 + i as i64 * 977,
                    samp(mean, 800.0 * jitter),
                )
            })
            .collect()
    }

    fn kinds_at(v: &[Finding], frame: usize) -> Vec<Kind> {
        v.iter()
            .filter(|f| f.frame == frame)
            .map(|f| f.kind)
            .collect()
    }

    #[test]
    fn a_clean_sequence_reports_nothing() {
        // 误报一多这张表就没人看了：渐变 + 轻微抖动必须一条都不报。
        let v = clean(60);
        let found = audit(&v, &Options::default());
        assert!(found.is_empty(), "干净序列报出了 {:?}", names(&found));
    }

    #[test]
    fn day_to_night_ramp_is_not_a_hundred_outliers() {
        // 日转夜跨 5 档：拿全序列中位数算 z 分数会把两头全报成离群。
        let n = 200;
        let v: Vec<Probe> = (0..n)
            .map(|i| {
                let mean = 0.8 * 2.0f64.powf(-5.0 * i as f64 / (n - 1) as f64);
                probe(
                    &format!("IMG_{:04}.png", i + 1),
                    40_000_000 + i as i64,
                    samp(mean, 900.0),
                )
            })
            .collect();
        assert!(
            audit(&v, &Options::default()).is_empty(),
            "渐变被判成离群了"
        );
    }

    #[test]
    fn black_blur_and_exposure_step_are_caught() {
        let mut v = clean(30);
        v[5].sample = samp(0.002, 800.0); // 镜头盖
        let m12 = v[12].sample.unwrap().mean;
        v[12].sample = samp(m12, 40.0); // 糊了：只有邻居的 5%
        let m20 = v[20].sample.unwrap().mean;
        v[20].sample = samp(m20 * 3.0, 800.0); // 曝光飞了
        let found = audit(&v, &Options::default());
        assert_eq!(kinds_at(&found, 5), [Kind::Black]);
        assert_eq!(kinds_at(&found, 12), [Kind::Blur]);
        assert_eq!(kinds_at(&found, 20), [Kind::LumaOut]);
        assert_eq!(found.len(), 3, "多报了 {:?}", names(&found));
        let blur = found.iter().find(|f| f.frame == 12).unwrap();
        // 只有邻居的 5%：低于 10% 时写成「1/20」而不是「0%」，报 0% 等于没说。
        assert!(
            blur.note.contains("1/20"),
            "虚焦文案没给出比例：{}",
            blur.note
        );
        assert_eq!(pct(0.4), "40%");
        assert_eq!(pct(0.004), "1/250");
    }

    #[test]
    fn a_noisy_sequence_gets_a_wider_gate() {
        // 同样一档跳变：抖动小的序列要抓到，抖动大的序列不能满屏误报。
        let spike = |mul: f64| {
            (0..30)
                .map(|i| {
                    let wobble = 1.0 + ((i * 11) % 7) as f64 * mul;
                    let mean = 0.4 * wobble * if i == 15 { 1.25 } else { 1.0 };
                    probe(
                        &format!("IMG_{:04}.png", i + 1),
                        40_000_000 + i as i64,
                        samp(mean, 800.0),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert!(
            audit(&spike(0.002), &Options::default())
                .iter()
                .any(|f| f.kind == Kind::LumaOut),
            "干净序列里 25% 的台阶漏了"
        );
        let loud = audit(&spike(0.25), &Options::default());
        assert!(
            !loud.iter().any(|f| f.kind == Kind::LumaOut),
            "噪声序列里 25% 的抖动被当成了离群：{:?}",
            names(&loud)
        );
    }

    #[test]
    fn unreadable_frame_does_not_also_pile_on_blur_and_outlier() {
        let mut v = clean(24);
        v[9].sample = None;
        let found = audit(&v, &Options::default());
        assert_eq!(kinds_at(&found, 9), [Kind::Read]);
        assert_eq!(found.len(), 1, "副作用没压住：{:?}", names(&found));
        assert_eq!(found[0].note, "这一帧读不出来（文件损坏或不是图片）");
    }

    #[test]
    fn gaps_land_on_the_frame_after_the_hole() {
        let mut idx = 0i64;
        let v: Vec<Probe> = (1..=20)
            .filter(|i| ![7, 8, 15].contains(i))
            .map(|i| {
                idx += 1;
                probe(
                    &format!("IMG_{i:04}.png"),
                    40_000_000 + idx,
                    samp(0.4, 800.0),
                )
            })
            .collect();
        let found = audit(&v, &Options::default());
        let gap: Vec<&Finding> = found.iter().filter(|f| f.kind == Kind::Gap).collect();
        assert_eq!(gap.len(), 2, "缺口清单不对：{:?}", names(&found));
        // 7、8 连成一整段，落在它们后面的那一帧（数组下标 6 = 第 9 张）；15 单独一段。
        assert_eq!((gap[0].frame, gap[0].value), (6, 2.0));
        assert!(
            gap[0].note.contains("缺 2 帧（编号 7–8）"),
            "{}",
            gap[0].note
        );
        assert_eq!((gap[1].frame, gap[1].value), (12, 1.0));
        assert!(gap[1].note.contains("编号 15）"), "{}", gap[1].note);
        assert!(!Kind::Gap.excludable() && Kind::Black.excludable());
    }

    #[test]
    fn different_prefixes_are_numbered_separately() {
        let mut v = Vec::new();
        for i in 1..=10u64 {
            let n = v.len() as i64;
            v.push(probe(
                &format!("A{i:03}.png"),
                40_000_000 + n,
                samp(0.4, 800.0),
            ));
        }
        // 另一个前缀缺第 5 张：不能把 A 的编号也算进同一串。
        for i in 1..=10u64 {
            if i == 5 {
                continue;
            }
            let n = v.len() as i64;
            v.push(probe(
                &format!("B{i:03}.png"),
                40_000_000 + n,
                samp(0.4, 800.0),
            ));
        }
        let found = audit(&v, &Options::default());
        let gaps: Vec<&Finding> = found.iter().filter(|f| f.kind == Kind::Gap).collect();
        assert_eq!(gaps.len(), 1, "前缀分组不对：{:?}", names(&found));
        assert!(gaps[0].note.contains("编号 5）"), "{}", gaps[0].note);
    }

    #[test]
    fn a_duplicate_needs_both_the_size_and_the_measurement() {
        let mut v = clean(20);
        v[11].sample = v[10].sample;
        v[11].size = v[10].size;
        assert!(
            audit(&v, &Options::default())
                .iter()
                .any(|f| f.frame == 11 && f.kind == Kind::Dup)
        );
        // 文件大小不同（重编码过）就不算「快门没动作」。
        v[11].size += 1;
        assert!(
            !audit(&v, &Options::default())
                .iter()
                .any(|f| f.kind == Kind::Dup)
        );
    }

    #[test]
    fn short_sequences_skip_the_statistics() {
        let mut v = clean(6);
        v[3].sample = samp(0.9, 800.0);
        let found = audit(&v, &Options::default());
        assert!(!found.iter().any(|f| f.kind == Kind::LumaOut));
        // 黑帧是绝对判定，跟统计无关，短序列照样要报。
        v[3].sample = samp(0.001, 800.0);
        assert!(
            audit(&v, &Options::default())
                .iter()
                .any(|f| f.kind == Kind::Black)
        );
    }

    #[test]
    fn labels_counts_and_switches() {
        let mut v = clean(30);
        v[4].sample = None;
        v[9].sample = samp(0.001, 800.0);
        let found = audit(&v, &Options::default());
        let counts = count_by_kind(&found);
        assert_eq!(counts.get("read"), Some(&1));
        assert_eq!(counts.get("black"), Some(&1));
        assert_eq!(Kind::LumaOut.id(), "luma_out");
        assert_eq!(Kind::Dup.cn(), "疑似重复");
        // 关掉某项判定就不该出现该类。
        let off = Options {
            gaps: false,
            dup: false,
            ..Options::default()
        };
        assert!(!audit(&v, &off).iter().any(|f| f.kind == Kind::Gap));
    }

    #[test]
    fn numbered_reads_the_trailing_run_only() {
        assert_eq!(numbered("IMG_0007.png"), Some(("IMG_".to_owned(), 7)));
        assert_eq!(numbered("A1b2.png"), Some(("A1b".to_owned(), 2)));
        assert_eq!(numbered("0001.tif"), Some(("".to_owned(), 1)));
        assert_eq!(numbered("no_digits.png"), None);
        assert_eq!(numbered("DSC099999999999999999999.jpg"), None);
    }

    #[test]
    fn probes_read_name_size_and_keep_the_sample() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("aevum-qa-{}", nanos));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("IMG_0001.png");
        let b = dir.join("IMG_0002.png");
        std::fs::write(&a, b"1234567").unwrap();
        let samples = [samp(0.4, 800.0), None];
        let v = probes(&[a.clone(), b.clone()], &samples);
        assert_eq!(v[0].name, "IMG_0001.png");
        assert_eq!(v[0].size, 7);
        assert_eq!(v[1].size, -1, "读不到的文件不该给个像样的大小");
        assert!(v[0].sample.is_some() && v[1].sample.is_none());
        assert_eq!(
            v[0].dir,
            dir.to_string_lossy().to_lowercase(),
            "目录键要能分组同前缀的不同目录"
        );
        // 缺口正好用得上这份真实文件名：第 2 张不存在，就不该报缺帧。
        let found = audit(&v, &Options::default());
        assert!(!found.iter().any(|f| f.kind == Kind::Gap));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn names(v: &[Finding]) -> Vec<String> {
        v.iter()
            .map(|f| format!("{}:{}", f.frame, f.kind.id()))
            .collect()
    }
}
