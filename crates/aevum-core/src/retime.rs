//! 时间重映射（变速 + 运动模糊）。
//!
//! 延时摄影的原始帧是「等时间间隔采样」，成片要的是「等播放时间输出」，
//! 两者之间差一条速度曲线：常速 1× 就是一帧对一帧；2× 是两帧压成一帧；
//! 0.5× 是把一帧拆成两帧（相邻两帧交叉淡化）。压得太快会一顿一顿的
//! （judder），所以引入「快门」：一帧输出对应的曝光区间内，把跨过的源帧
//! 按重叠时长加权平均——这正是延时圈常用的 accumulate-and-average，
//! 车流变成光带、水变绸缎，而不靠光流补帧。
//!
//! 口径都锁在这里，导出与界面只负责取计划与铺像素：
//! - 速度键用**源进度**（0..1）而不是输出秒，这样「序列走到 20% 开始加速」
//!   与成片时长互不依赖——时长是速度的积分结果，不是输入。
//! - 输出帧数 `n_out = ⌊n_src · ∫₀¹ du/speed(u)⌋`，常速时恰好等于 `n_src`。
//! - 快门 = 曝光区间 / 该帧跨过的源帧跨度；0 = 只做相邻两帧的线性插值。

use serde::{Deserialize, Serialize};

/// 一条速度键：源进度 `at`（0..1）处的倍速。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpeedKey {
    pub at: f64,
    pub speed: f64,
}

/// 重映射设定；`enabled = false` 时导出走原来的 1:1 快路，一个像素都不动。
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Retime {
    pub enabled: bool,
    /// 按 `at` 升序；空表等于全程 1×。
    pub keys: Vec<SpeedKey>,
    /// 曝光区间占本帧源跨度的比例，0..1；0 是锐利插帧，1 是把跨过的帧全平均。
    pub shutter: f64,
}

/// 一次输出帧数上限：慢放拉太长等于用户填错了倍速，宁可当场拒绝。
pub const MAX_OUTPUT_FRAMES: usize = 200_000;

impl Retime {
    pub fn uniform(speed: f64, shutter: f64) -> Self {
        Self {
            enabled: true,
            keys: vec![SpeedKey { at: 0.0, speed }, SpeedKey { at: 1.0, speed }],
            shutter,
        }
    }

    /// 匀加速进、匀减速出：中段 `peak` 倍速，两头 `edge` 倍速。
    pub fn ease(edge: f64, peak: f64, in_at: f64, out_at: f64, shutter: f64) -> Self {
        Self {
            enabled: true,
            keys: vec![
                SpeedKey {
                    at: 0.0,
                    speed: edge,
                },
                SpeedKey {
                    at: in_at.clamp(0.0, 1.0),
                    speed: peak,
                },
                SpeedKey {
                    at: out_at.clamp(0.0, 1.0).max(in_at.clamp(0.0, 1.0)),
                    speed: peak,
                },
                SpeedKey {
                    at: 1.0,
                    speed: edge,
                },
            ],
            shutter,
        }
    }

    /// 关掉时等于没参与；开着但常速零快门也当没参与（省一次重采样）。
    pub fn is_identity(&self) -> bool {
        if !self.enabled {
            return true;
        }
        self.shutter <= 0.0 && self.speeds_flat(1.0)
    }

    /// 整条曲线是否恒等于某个常数（键里允许出现浮点误差，1e-9 内当相等）。
    fn speeds_flat(&self, want: f64) -> bool {
        self.keys.iter().all(|k| (k.speed - want).abs() <= 1e-9)
    }

    /// 校验并规整：键排序、去重同位、快门夹进 0..1。错误文案直接可展示。
    pub fn normalized(&self) -> Result<Self, String> {
        if !self.enabled {
            return Ok(Self::default());
        }
        for key in &self.keys {
            if !key.at.is_finite() || !key.speed.is_finite() {
                return Err("速度键里的数字得是有限值".to_owned());
            }
            if key.speed <= 0.0 {
                return Err(format!(
                    "倍速必须大于 0（源进度 {:.4} 处是 {}×）；要停住就把倍速写得很小，别写 0",
                    key.at, key.speed
                ));
            }
        }
        let mut keys = self.keys.clone();
        keys.sort_by(|a, b| a.at.total_cmp(&b.at));
        // 同一位置只留第一笔（排序是稳定的，即输入里靠前的那条）。
        keys.dedup_by(|a, b| (a.at - b.at).abs() <= 1e-12);
        Ok(Self {
            enabled: true,
            keys,
            shutter: self.shutter.clamp(0.0, 1.0),
        })
    }
}

/// 源进度 `u`（0..1）处的倍速：键之间线性，两头钳到首尾键。空表 1×。
pub fn speed_at(keys: &[SpeedKey], u: f64) -> f64 {
    if keys.is_empty() {
        return 1.0;
    }
    if u <= keys[0].at {
        return keys[0].speed;
    }
    let last = keys[keys.len() - 1];
    if u >= last.at {
        return last.speed;
    }
    for win in keys.windows(2) {
        let (a, b) = (win[0], win[1]);
        if u >= a.at && u <= b.at {
            let span = b.at - a.at;
            if span <= 0.0 {
                return b.speed;
            }
            let t = (u - a.at) / span;
            return a.speed + (b.speed - a.speed) * t;
        }
    }
    last.speed
}

/// `∫₀ᵘ dv / speed(v)`：把「源进度」换算成「输出帧编号」的那把尺子。
///
/// 倍速分段线性，倒数不再线性，所以逐段辛普森积分：段内先按关键点上割，
/// 每段 32 个面板（65 个取样点）——倍速曲线再弯也在这尺度上够平，误差
/// 远小于 1e-9 帧，还保证同输入同输出（导出要可复现）。
pub fn cost_at(keys: &[SpeedKey], u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    if keys.is_empty() || u <= 0.0 {
        return u;
    }
    // 折点只取区间内部的：端点处的键不产生新的分段，速度在两头钳到首尾键。
    let mut knots: Vec<f64> = keys
        .iter()
        .map(|k| k.at.clamp(0.0, 1.0))
        .filter(|at| *at > 0.0 && *at < u)
        .collect();
    knots.sort_by(f64::total_cmp);
    knots.dedup();
    let mut pts = Vec::with_capacity(knots.len() + 2);
    pts.push(0.0);
    pts.extend(knots);
    pts.push(u);

    let mut total = 0.0;
    for win in pts.windows(2) {
        let (a, b) = (win[0], win[1]);
        if b <= a {
            continue;
        }
        total += simpson(keys, a, b);
    }
    total
}

const PANELS: usize = 32;

/// 辛普森积分：面板数取偶数，取样点 2·PANELS+1。
fn simpson(keys: &[SpeedKey], a: f64, b: f64) -> f64 {
    let panels = PANELS * 2;
    let h = (b - a) / panels as f64;
    let mut sum = 1.0 / speed_at(keys, a) + 1.0 / speed_at(keys, b);
    for i in 1..panels {
        let w = if i % 2 == 1 { 4.0 } else { 2.0 };
        sum += w / speed_at(keys, a + h * i as f64);
    }
    sum * h / 3.0
}

/// 给定输出帧号（整数部分即源进度尺上的第几帧），反解源位置（帧坐标，连续）。
///
/// `i / n_src = cost(u)`，左边是已知的输出进度，右边单调递增，二分足够稳。
fn source_position(keys: &[SpeedKey], n_src: usize, frame: f64) -> f64 {
    if n_src == 0 {
        return 0.0;
    }
    let want = frame / n_src as f64;
    let total = cost_at(keys, 1.0);
    if want <= 0.0 {
        return 0.0;
    }
    if want >= total {
        return n_src as f64;
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..64 {
        let mid = (lo + hi) * 0.5;
        if cost_at(keys, mid) < want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) * 0.5 * n_src as f64
}

/// 一个输出帧的取样计划：`(源帧号, 权重)`，权重升序、和为 1。
#[derive(Clone, Debug, PartialEq)]
pub struct Samples(pub Vec<(usize, f32)>);

impl Samples {
    pub fn single(index: usize) -> Self {
        Self(vec![(index, 1.0)])
    }

    pub fn is_single(&self) -> bool {
        self.0.len() == 1 && (self.0[0].1 - 1.0).abs() <= 1e-6
    }

    /// 按索引合并权重，顺手丢掉 0 权。
    fn add(&mut self, index: usize, weight: f64) {
        if weight <= 0.0 {
            return;
        }
        if let Some(slot) = self.0.iter_mut().find(|slot| slot.0 == index) {
            slot.1 += weight as f32;
        } else {
            self.0.push((index, weight as f32));
        }
    }

    /// 舍掉可忽略的尾权、按源帧号升序、归一化成和为 1。
    fn finish(mut self) -> Self {
        self.0.retain(|slot| slot.1 > 1e-7);
        self.0.sort_by_key(|slot| slot.0);
        let sum: f32 = self.0.iter().map(|slot| slot.1).sum();
        if sum <= 0.0 {
            return Self::single(0);
        }
        for slot in &mut self.0 {
            slot.1 /= sum;
        }
        self
    }

    /// 曝光窗口 `[a, a + shutter·(end-a)]` 落在源帧格 `k..k+1` 上的重叠积分。
    ///
    /// 零快门（窗口退化）退化成 `a` 处的两点线性插值：常速整数定位时恰好命中
    /// 单帧，所以 1×/快门 0 是逐像素不变的；慢放时就是相邻两帧交叉淡化。
    pub fn from_window(a: f64, end: f64, shutter: f64, n_src: usize) -> Self {
        let last = n_src.saturating_sub(1);
        let anchor = a.clamp(0.0, last as f64);
        let width = (end - a).max(0.0) * shutter.clamp(0.0, 1.0);
        let mut out = Self(Vec::with_capacity(4));
        if width <= 1e-9 {
            let cell = anchor.floor().max(0.0) as usize;
            let frac = anchor - cell as f64;
            out.add(cell.min(last), 1.0 - frac);
            out.add((cell + 1).min(last), frac);
        } else {
            let w1 = (anchor + width).min(n_src as f64);
            let span = (w1 - anchor).max(1e-12);
            let first = anchor.floor() as usize;
            let stop = w1.ceil() as usize;
            for cell in first..stop.max(first + 1) {
                let overlap = (w1.min(cell as f64 + 1.0) - anchor.max(cell as f64)).max(0.0);
                out.add(cell.min(last), overlap / span);
            }
        }
        out.finish()
    }
}

/// 整段重映射计划：每个输出帧取哪些源帧、各占多少。
#[derive(Clone, Debug)]
pub struct Plan {
    pub per_frame: Vec<Samples>,
    /// 每个输出帧落在源序列上的连续位置（帧坐标），供面板回显与调试。
    pub anchors: Vec<f64>,
    /// 源帧数 → 输出帧数换算后的播放时长（秒），给面板与命令回显用。
    pub seconds: f64,
}

impl Plan {
    pub fn frames(&self) -> usize {
        self.per_frame.len()
    }
}

/// 只按速度曲线算输出帧数，不建取样计划：面板每帧都要读数，建 20 万帧的计划算不起。
///
/// 与 `plan` 共用同一句话（帧数上限的文案也在这里），所以面板读数与导出实际
/// 写出的帧数不会各说各的。
pub fn output_frames(keys: &[SpeedKey], n_src: usize) -> Result<usize, String> {
    let frames = (cost_at(keys, 1.0) * n_src as f64).floor().max(1.0) as usize;
    if frames > MAX_OUTPUT_FRAMES {
        return Err(format!(
            "变速后会有 {frames} 帧输出，超过上限 {MAX_OUTPUT_FRAMES}：倍速太小或序列太长，把倍速提上来或先裁一段"
        ));
    }
    Ok(frames)
}

/// 算计划。`fps` 只用来报时长，不影响取样。
pub fn plan(retime: &Retime, n_src: usize, fps: f64) -> Result<Plan, String> {
    let retime = retime.normalized()?;
    if n_src == 0 {
        return Err("序列里没有帧，重映射无从下手".to_owned());
    }
    if !(fps.is_finite() && fps > 0.0) {
        return Err(format!("帧率得是正数，实得 {fps}"));
    }
    if !retime.enabled {
        return Ok(Plan {
            per_frame: (0..n_src).map(Samples::single).collect(),
            anchors: (0..n_src).map(|i| i as f64).collect(),
            seconds: n_src as f64 / fps,
        });
    }
    let keys = retime.keys.as_slice();
    let frames = output_frames(keys, n_src)?;

    // 先一次性反解每个输出帧边界的源位置，再逐帧积分曝光窗口：
    // 位置只由速度曲线决定，与快门无关，所以改快门不会改帧序。
    let pos: Vec<f64> = (0..=frames)
        .map(|i| source_position(keys, n_src, i as f64))
        .collect();
    let per_frame = (0..frames)
        .map(|i| Samples::from_window(pos[i], pos[i + 1], retime.shutter, n_src))
        .collect();
    Ok(Plan {
        per_frame,
        anchors: pos[..frames].to_vec(),
        seconds: frames as f64 / fps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pairs: &[(f64, f64)]) -> Vec<SpeedKey> {
        pairs
            .iter()
            .map(|(at, speed)| SpeedKey {
                at: *at,
                speed: *speed,
            })
            .collect()
    }

    fn weight_sum(samples: &Samples) -> f32 {
        samples.0.iter().map(|slot| slot.1).sum()
    }

    #[test]
    fn speed_is_linear_between_keys_and_clamped_outside() {
        let k = keys(&[(0.0, 1.0), (0.5, 3.0), (1.0, 2.0)]);
        assert_eq!(speed_at(&k, -0.2), 1.0);
        assert_eq!(speed_at(&k, 0.25), 2.0);
        assert_eq!(speed_at(&k, 0.5), 3.0);
        assert_eq!(speed_at(&k, 0.75), 2.5);
        assert_eq!(speed_at(&k, 1.4), 2.0);
        assert_eq!(speed_at(&[], 0.7), 1.0);
    }

    #[test]
    fn cost_is_the_integral_of_inverse_speed() {
        assert_eq!(cost_at(&[], 0.25), 0.25);
        // 1×→3× 线性：∫₀¹ du/(1+2u) = ln3/2
        let ramp = keys(&[(0.0, 1.0), (1.0, 3.0)]);
        assert!((cost_at(&ramp, 1.0) - 0.549_306_144_334_054_9).abs() < 1e-7);
        // 1×→2×→1× 对称：两段各 ln2/2
        let hill = keys(&[(0.0, 1.0), (0.5, 2.0), (1.0, 1.0)]);
        assert!((cost_at(&hill, 1.0) - std::f64::consts::LN_2).abs() < 1e-7);
        // 常速不改变进度
        let flat = keys(&[(0.0, 1.0), (1.0, 1.0)]);
        assert!((cost_at(&flat, 0.4) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn cost_splits_at_every_key() {
        // 折点在 0.5：前半 1×→4× 线性，后半恒 4×。积分必须逐段做，不能拿一条
        // 直线糊过整条曲线。
        let k = keys(&[(0.0, 1.0), (0.5, 4.0), (1.0, 4.0)]);
        let head = 4f64.ln() / 6.0; // ∫₀^0.5 du/(1+6u)
        assert!((cost_at(&k, 0.5) - head).abs() < 1e-7);
        assert!((cost_at(&k, 0.75) - (head + 0.0625)).abs() < 1e-7);
        assert!((cost_at(&k, 1.0) - (head + 0.125)).abs() < 1e-7);
    }

    #[test]
    fn disabled_or_empty_keys_stay_one_to_one() {
        let p = plan(&Retime::default(), 12, 24.0).unwrap();
        assert_eq!(p.frames(), 12);
        assert_eq!(p.seconds, 0.5);
        assert_eq!(p.anchors.len(), 12);
        for (i, samples) in p.per_frame.iter().enumerate() {
            assert!(samples.is_single());
            assert_eq!(samples.0[0].0, i);
            assert_eq!(p.anchors[i], i as f64);
        }
        let open = Retime {
            enabled: true,
            keys: vec![],
            shutter: 0.0,
        };
        assert!(open.is_identity());
        let q = plan(&open, 12, 24.0).unwrap();
        assert_eq!(q.per_frame, p.per_frame);
    }

    #[test]
    fn one_x_zero_shutter_hits_exactly_one_source_frame() {
        let r = Retime::uniform(1.0, 0.0);
        assert!(r.is_identity());
        let p = plan(&r, 8, 25.0).unwrap();
        assert_eq!(p.frames(), 8);
        for (i, samples) in p.per_frame.iter().enumerate() {
            assert_eq!(samples.0, vec![(i, 1.0)]);
        }
    }

    #[test]
    fn double_speed_without_shutter_drops_every_other_frame() {
        let p = plan(&Retime::uniform(2.0, 0.0), 10, 25.0).unwrap();
        assert_eq!(p.frames(), 5);
        let idx: Vec<usize> = p.per_frame.iter().map(|s| s.0[0].0).collect();
        assert_eq!(idx, vec![0, 2, 4, 6, 8]);
    }

    #[test]
    fn full_shutter_averages_the_frames_the_exposure_crosses() {
        let p = plan(&Retime::uniform(2.0, 1.0), 10, 25.0).unwrap();
        assert_eq!(p.frames(), 5);
        assert_eq!(p.per_frame[0].0, vec![(0, 0.5), (1, 0.5)]);
        assert_eq!(p.per_frame[4].0, vec![(8, 0.5), (9, 0.5)]);
    }

    #[test]
    fn partial_shutter_blends_in_proportion_to_overlap() {
        // 4× + 快门 0.375：曝光窗口宽 1.5 格，跨过的两格按 2/3 : 1/3 分。
        let p = plan(&Retime::uniform(4.0, 0.375), 40, 25.0).unwrap();
        assert_eq!(p.frames(), 10);
        let s = &p.per_frame[3];
        assert_eq!(s.0.len(), 2);
        assert_eq!(s.0[0].0, 12);
        assert_eq!(s.0[1].0, 13);
        assert!((s.0[0].1 - 2.0 / 3.0).abs() < 1e-6, "{s:?}");
        assert!((s.0[1].1 - 1.0 / 3.0).abs() < 1e-6, "{s:?}");
    }

    #[test]
    fn slow_motion_dissolves_but_the_last_frame_stays_clean() {
        let p = plan(&Retime::uniform(0.5, 0.0), 4, 24.0).unwrap();
        assert_eq!(p.frames(), 8);
        assert_eq!(p.seconds, 8.0 / 24.0);
        assert_eq!(p.per_frame[0].0, vec![(0, 1.0)]);
        assert_eq!(p.per_frame[1].0, vec![(0, 0.5), (1, 0.5)]);
        assert_eq!(p.per_frame[2].0, vec![(1, 1.0)]);
        // 末尾落在最后一格的一半上：没有下一帧可淡，权重全给最后一帧，不做淡出。
        assert_eq!(p.per_frame[7].0, vec![(3, 1.0)]);
    }

    #[test]
    fn ramp_spreads_steps_at_the_ends_and_gathers_in_the_middle() {
        let r = Retime::ease(1.0, 4.0, 0.4, 0.6, 0.0);
        let p = plan(&r, 200, 25.0).unwrap();
        assert!(p.frames() > 50 && p.frames() < 200, "{} 帧", p.frames());
        let steps: Vec<f64> = p.anchors.windows(2).map(|w| w[1] - w[0]).collect();
        let head = steps[0];
        let mid = steps[steps.len() / 2];
        let tail = steps[steps.len() - 1];
        assert!(
            mid > head * 3.0 && mid > tail * 3.0,
            "head {head}, mid {mid}, tail {tail}"
        );
        let half = steps.len() / 2;
        for win in steps[..half].windows(2) {
            assert!(win[1] >= win[0] - 1e-9, "加速段步长在缩：{win:?}");
        }
        for win in steps[half..].windows(2) {
            assert!(win[1] <= win[0] + 1e-9, "减速段步长在涨：{win:?}");
        }
        assert!(p.anchors.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn every_output_frame_weighs_exactly_one() {
        for speed in [0.5f64, 1.0, 2.7, 8.0] {
            for shutter in [0.0f64, 0.2, 0.5, 1.0] {
                let p = plan(&Retime::uniform(speed, shutter), 37, 24.0).unwrap();
                assert!(!p.per_frame.is_empty(), "{speed}/{shutter}");
                for s in &p.per_frame {
                    assert!(
                        (weight_sum(s) - 1.0).abs() < 1e-5,
                        "{speed}/{shutter}: {s:?}"
                    );
                    assert!(s.0.windows(2).all(|w| w[0].0 < w[1].0), "{s:?}");
                    assert!(
                        s.0.iter().all(|slot| slot.0 < 37 && slot.1 > 0.0),
                        "{speed}/{shutter}: {s:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn plan_is_reproducible_frame_by_frame() {
        let r = Retime::ease(1.0, 3.0, 0.3, 0.7, 0.5);
        let a = plan(&r, 60, 25.0).unwrap();
        let b = plan(&r, 60, 25.0).unwrap();
        assert_eq!(a.per_frame, b.per_frame);
        assert_eq!(a.anchors, b.anchors);
    }

    #[test]
    fn normalizing_sorts_dedups_and_clamps_the_shutter() {
        let r = Retime {
            enabled: true,
            keys: keys(&[(0.5, 2.0), (0.0, 2.0), (0.5, 2.0)]),
            shutter: 3.0,
        };
        let n = r.normalized().unwrap();
        assert_eq!(n.shutter, 1.0);
        assert_eq!(n.keys.len(), 2);
        assert_eq!(n.keys[0].at, 0.0);
        assert_eq!(n.keys[1].at, 0.5);
        // 关掉就整体退场
        let off = Retime {
            enabled: false,
            keys: keys(&[(0.0, -1.0)]),
            shutter: 9.0,
        };
        assert_eq!(off.normalized().unwrap(), Retime::default());
    }

    #[test]
    fn rejects_bad_speeds_frames_and_output_size() {
        let zero = Retime {
            enabled: true,
            keys: keys(&[(0.0, 1.0), (1.0, 0.0)]),
            shutter: 0.5,
        };
        assert!(zero.normalized().unwrap_err().contains("倍速必须大于 0"));
        let nan = Retime {
            enabled: true,
            keys: keys(&[(0.0, f64::NAN)]),
            shutter: 0.0,
        };
        assert!(nan.normalized().unwrap_err().contains("有限值"));
        assert!(
            plan(&Retime::uniform(2.0, 0.0), 0, 25.0)
                .unwrap_err()
                .contains("没有帧")
        );
        assert!(
            plan(&Retime::uniform(2.0, 0.0), 10, 0.0)
                .unwrap_err()
                .contains("帧率")
        );
        let err = plan(&Retime::uniform(0.0001, 0.0), 1_000, 25.0).unwrap_err();
        assert!(err.contains(&MAX_OUTPUT_FRAMES.to_string()), "{err}");
    }

    #[test]
    fn heavy_fast_forward_still_produces_one_frame() {
        let p = plan(&Retime::uniform(500.0, 1.0), 100, 25.0).unwrap();
        assert_eq!(p.frames(), 1);
        assert_eq!(p.per_frame[0].0[0].0, 0);
        assert!((weight_sum(&p.per_frame[0]) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn quick_frame_count_agrees_with_the_full_plan() {
        // 面板每帧都读数，用的是只积分曲线的快路；它必须和真计划一个数。
        for retime in [
            Retime::uniform(2.0, 0.0),
            Retime::uniform(0.5, 1.0),
            Retime::ease(1.0, 4.0, 0.2, 0.8, 0.5),
            Retime {
                enabled: true,
                keys: vec![
                    SpeedKey {
                        at: 0.0,
                        speed: 0.2,
                    },
                    SpeedKey {
                        at: 1.0,
                        speed: 12.0,
                    },
                ],
                shutter: 1.0,
            },
        ] {
            let keys = retime.normalized().unwrap().keys;
            let want = plan(&retime, 120, 24.0).unwrap().frames();
            assert_eq!(output_frames(&keys, 120).unwrap(), want, "{retime:?}");
        }
    }
}
