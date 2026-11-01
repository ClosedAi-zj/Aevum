//! 自动校正：一次给出白平衡与整套影调，而不是在当前值上再叠一档曝光。
//!
//! 提案先在长边 256 的代理图上按 1/50/99 百分位算出来，再把提案真的走一遍
//! 逐点链来验收：两端贴边的像素比例不许比原图多，中间调必须更靠近目标。
//! 不达标就按档位先收风格项（白色/对比/黑位），实在不行才收曝光——所以已经
//! 曝好的图不会被越点越亮，夜景也不会被硬拉到中灰后爆掉路灯。
//!
//! 口径与 Python 金标 `grade.auto_params` 一致。
//!
//! Auto correction: produces white balance and a full tonal set in one pass instead of stacking
//! another exposure step on top of the current values.
//!
//! The proposal is first computed on a 256-long-side proxy using the 1/50/99 percentiles, then
//! actually run through the per-pixel chain for acceptance: the fraction of pixels pinned at
//! either end may not exceed the original, and the midtones must land closer to the target.
//! When that fails, stylistic terms (whites / contrast / blacks) are pulled back step by step,
//! and only as a last resort is exposure pulled — so an already well-exposed image does not get
//! brighter with each click, and a night scene is not forced to mid-gray with blown street
//! lamps.
//!
//! Conventions match the Python golden `grade.auto_params`.

use crate::develop::{self, Ev, Opts};
use crate::grade::GradeParams;

const TARGET_MED: f64 = 0.46;
const CLIP_TOL: f64 = 0.002;
const MAX_EV: f64 = 1.5;
const SCALES: [f64; 6] = [1.0, 0.7, 0.5, 0.35, 0.2, 0.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoParams {
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub temperature: f64,
    pub tint: f64,
}

impl AutoParams {
    /// 影调是不是真的动了；全 0 表示这张图本来就在安全范围内。
    ///
    /// Whether the tone actually moved; all zeros mean the image was already within the safe
    /// range.
    pub fn touches_tone(self) -> bool {
        self.exposure != 0.0
            || self.contrast != 0.0
            || self.highlights != 0.0
            || self.shadows != 0.0
            || self.whites != 0.0
            || self.blacks != 0.0
    }
}

/// `probe` 是 sRGB 编码的 `[0,1]` 像素；返回绝对值，可直接覆盖对应参数。
///
/// `probe` holds sRGB-encoded `[0,1]` pixels; the result is absolute and can overwrite the
/// corresponding parameters directly.
pub fn auto_params(probe: &[f32], w: usize, h: usize) -> AutoParams {
    if w == 0 || h == 0 || probe.len() < w * h * 3 {
        return AutoParams {
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            temperature: 0.0,
            tint: 0.0,
        };
    }
    let lum0 = luminance(probe, w * h);
    let (ref_hi, ref_lo) = clip_fracs(&lum0);
    let p50 = percentile(&mut lum0.clone(), 50.0);

    let mut ev = (TARGET_MED / p50.max(1e-3)).log2().clamp(-MAX_EV, MAX_EV);
    if ev.abs() < 0.1 {
        ev = 0.0;
    }

    let l2: Vec<f64> = lum0
        .iter()
        .map(|v| (v * 2f64.powf(ev)).clamp(0.0, 1.0))
        .collect();
    let q1 = percentile(&mut l2.clone(), 1.0);
    let q99 = percentile(&mut l2.clone(), 99.0);

    let protect = [
        ("highlights", (-(q99 - 0.94) * 350.0).clamp(-70.0, 0.0)),
        ("shadows", ((0.12 - q1) * 260.0).clamp(0.0, 60.0)),
    ];
    let risky = [
        ("exposure", ev),
        ("whites", ((0.94 - q99) * 250.0).clamp(0.0, 35.0)),
        ("contrast", ((0.75 - (q99 - q1)) * 80.0).clamp(-15.0, 30.0)),
        ("blacks", (-(q1 - 0.07) * 220.0).clamp(-20.0, 0.0)),
    ];

    let (temperature, tint) = white_balance(probe, &lum0);

    let mut keep = Proposal::default();
    keep.set("temperature", temperature);
    keep.set("tint", tint);
    if auto_safe(render(probe, w, h, &keep), (ref_hi, ref_lo)) {
        for (k, v) in protect {
            keep.set(k, v);
        }
    }
    let ref2 = render(probe, w, h, &keep);

    let mut out = risky_proposal(&risky, 0.0, 0.0);
    for (fe, ft) in SCALES
        .iter()
        .map(|t| (1.0, *t))
        .chain(SCALES[1..].iter().map(|e| (*e, *e)))
    {
        let mut q = Proposal::default();
        q.set("temperature", temperature);
        q.set("tint", tint);
        if let Some(v) = keep.get("highlights") {
            q.set("highlights", v);
        }
        if let Some(v) = keep.get("shadows") {
            q.set("shadows", v);
        }
        let scaled = risky_proposal(&risky, fe, ft);
        for key in ["exposure", "whites", "contrast", "blacks"] {
            if let Some(v) = scaled.get(key) {
                q.set(key, v);
            }
        }
        let verdict = auto_better(render(probe, w, h, &q), ref2);
        out = q;
        if verdict {
            break;
        }
    }

    AutoParams {
        exposure: round2(out.get("exposure").unwrap_or(0.0)),
        contrast: out.get("contrast").unwrap_or(0.0).round(),
        highlights: out.get("highlights").unwrap_or(0.0).round(),
        shadows: out.get("shadows").unwrap_or(0.0).round(),
        whites: out.get("whites").unwrap_or(0.0).round(),
        blacks: out.get("blacks").unwrap_or(0.0).round(),
        temperature: temperature.round(),
        tint: tint.round(),
    }
}

fn risky_proposal(risky: &[(&str, f64)], fe: f64, ft: f64) -> Proposal {
    let mut q = Proposal::default();
    for (k, v) in risky {
        let f = if *k == "exposure" { fe } else { ft };
        q.set(k, v * f);
    }
    q
}

#[derive(Default)]
struct Proposal {
    exposure: Option<f64>,
    contrast: Option<f64>,
    highlights: Option<f64>,
    shadows: Option<f64>,
    whites: Option<f64>,
    blacks: Option<f64>,
    temperature: Option<f64>,
    tint: Option<f64>,
}

impl Proposal {
    fn set(&mut self, key: &str, v: f64) {
        match key {
            "exposure" => self.exposure = Some(v),
            "contrast" => self.contrast = Some(v),
            "highlights" => self.highlights = Some(v),
            "shadows" => self.shadows = Some(v),
            "whites" => self.whites = Some(v),
            "blacks" => self.blacks = Some(v),
            "temperature" => self.temperature = Some(v),
            "tint" => self.tint = Some(v),
            _ => {}
        }
    }

    fn get(&self, key: &str) -> Option<f64> {
        match key {
            "exposure" => self.exposure,
            "contrast" => self.contrast,
            "highlights" => self.highlights,
            "shadows" => self.shadows,
            "whites" => self.whites,
            "blacks" => self.blacks,
            "temperature" => self.temperature,
            "tint" => self.tint,
            _ => None,
        }
    }

    fn grade(&self) -> GradeParams {
        let mut p = GradeParams::default();
        p.exposure = self.exposure.unwrap_or(0.0);
        p.contrast = self.contrast.unwrap_or(0.0);
        p.highlights = self.highlights.unwrap_or(0.0);
        p.shadows = self.shadows.unwrap_or(0.0);
        p.whites = self.whites.unwrap_or(0.0);
        p.blacks = self.blacks.unwrap_or(0.0);
        p.temperature = self.temperature.unwrap_or(0.0);
        p.tint = self.tint.unwrap_or(0.0);
        p
    }
}

/// 验收一律走真实逐点链：影调四段是乘加混在一起的，拿百分位估算会低估贴边。
fn render(probe: &[f32], w: usize, h: usize, q: &Proposal) -> (f64, f64, f64) {
    let mut px = probe.to_vec();
    develop::render(
        &mut px,
        w,
        h,
        &q.grade(),
        Ev::NONE,
        Opts {
            sketch: false,
            for_lut: true,
            local: false,
            hdr: false,
        },
    );
    let lum = luminance(&px, w * h);
    let (hi, lo) = clip_fracs(&lum);
    (hi, lo, percentile(&mut lum.clone(), 50.0))
}

fn auto_safe(cur: (f64, f64, f64), (ref_hi, ref_lo): (f64, f64)) -> bool {
    cur.0 <= ref_hi + CLIP_TOL && cur.1 <= ref_lo + CLIP_TOL
}

fn auto_better(cur: (f64, f64, f64), ref2: (f64, f64, f64)) -> bool {
    auto_safe(cur, (ref2.0, ref2.1))
        && (cur.2 - TARGET_MED).abs() <= (ref2.2 - TARGET_MED).abs() + 1e-3
}

fn luminance(px: &[f32], n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let r = px[i * 3] as f64;
            let g = px[i * 3 + 1] as f64;
            let b = px[i * 3 + 2] as f64;
            (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 1.0)
        })
        .collect()
}

fn clip_fracs(lum: &[f64]) -> (f64, f64) {
    let n = lum.len().max(1) as f64;
    let hi = lum.iter().filter(|v| **v >= 0.995).count() as f64 / n;
    let lo = lum.iter().filter(|v| **v <= 0.005).count() as f64 / n;
    (hi, lo)
}

/// 线性插值百分位，与 NumPy 的默认 method="linear" 同一口径。
fn percentile(v: &mut [f64], q: f64) -> f64 {
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let k = (n - 1) as f64 * q / 100.0;
    let lo = k.floor() as usize;
    let hi = k.ceil() as usize;
    if lo == hi {
        v[lo]
    } else {
        v[lo] + (v[hi] - v[lo]) * (k - lo as f64)
    }
}

fn white_balance(probe: &[f32], lum: &[f64]) -> (f64, f64) {
    let n = lum.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    let mut selected = 0usize;
    let mut r = Vec::new();
    let mut g = Vec::new();
    let mut b = Vec::new();
    for i in 0..n {
        let [pr, pg, pb] = [probe[i * 3], probe[i * 3 + 1], probe[i * 3 + 2]];
        let mx = pr.max(pg).max(pb);
        let mn = pr.min(pg).min(pb);
        let sat = (mx - mn) / mx.max(1e-4);
        if lum[i] > 0.08 && lum[i] < 0.92 && sat < 0.35 {
            selected += 1;
            r.push(pr as f64);
            g.push(pg as f64);
            b.push(pb as f64);
        }
    }
    if selected <= n / 50 {
        return (0.0, 0.0);
    }
    let (r, g, b) = (
        percentile(&mut r, 50.0),
        percentile(&mut g, 50.0).max(1e-4),
        percentile(&mut b, 50.0),
    );
    let tint = (((b / g - r / g) / 0.60) * 100.0).clamp(-80.0, 80.0);
    let green = g / (r * b).max(1e-4).sqrt();
    let temp = ((green - 1.0) / 0.28 * 100.0).clamp(-60.0, 60.0);
    (temp, tint)
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(r: f32, g: f32, b: f32) -> Vec<f32> {
        [r, g, b].repeat(4096)
    }

    #[test]
    fn dark_probe_gets_positive_exposure_and_idempotent_repeat() {
        let px = flat(0.12, 0.12, 0.12);
        let q = auto_params(&px, 64, 64);
        assert!(q.exposure > 0.3, "暗图该提亮，得到 {}", q.exposure);
        let again = auto_params(&px, 64, 64);
        assert_eq!(q, again, "同一张图重复自动校正必须给出同一组值");
    }

    #[test]
    fn already_mid_grey_does_not_drift_per_click() {
        let px = flat(0.46, 0.46, 0.46);
        let q = auto_params(&px, 64, 64);
        assert!(q.exposure.abs() < 0.1, "中间调不该再拉曝光：{}", q.exposure);
        assert!(!q.touches_tone() || q.contrast.abs() < 5.0);
    }

    #[test]
    fn blown_probe_never_pushes_more_clipping() {
        let px = flat(1.0, 1.0, 1.0);
        let q = auto_params(&px, 64, 64);
        assert!(q.exposure <= 0.0, "过曝图不许再加曝光：{}", q.exposure);
    }

    #[test]
    fn casts_are_pulled_towards_neutral() {
        let px = flat(0.34, 0.40, 0.44);
        let q = auto_params(&px, 64, 64);
        assert!(q.temperature > 1.0, "蓝偏该给正色温：{q:?}");
    }
}
