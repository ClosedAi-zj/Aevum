//! 参数拟合与验收用的度量工具：合成靶 + 客观指标。
//!
//! 存在理由：算法里那些系数不能靠「看着差不多」定，改一个数到底「变好还是变坏」
//! 必须有可复现的判据。这里按 `docs/adobe-algorithm-atlas.md` §46.2 的口径
//! 实现四类指标（MTF50 / halo / 噪声方差 / PSNR），让每个参数都能被扫参
//! 而不是拍脑袋：
//!
//! - 锐化：MTF50 提升 1.2–1.6×，halo < 3%
//! - 降噪：噪声方差下降 > 50%，MTF 保持 > 85%
//! - 去马赛克：合成 PSNR > 42 dB
//!
//! 约定：图像一律是交错 RGB 的 `f32`，与 `develop::spatial` 的入口一致，
//! 取值 0..1（线性或 sRGB 编码都可以，只要前后一致）。
//!
//! 每个测试文件 `mod support;` 引入后只会用到其中一部分，故整体关掉死代码告警。

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// 合成靶
// ---------------------------------------------------------------------------

/// 横向正弦光栅（灰度，三通道相同）。`freq` 单位是 cycles/pixel。
///
/// 用来测 MTF：把「已知振幅的正弦」喂进去，量出来还有多少振幅。
/// 振幅取小（0.1 量级）以让锐化/降噪停留在近似线性区，避免限幅与阈值
/// 把非线性混进传递函数里。
pub fn sine_target(w: usize, h: usize, freq: f32, amp: f32, mid: f32) -> Vec<f32> {
    let mut img = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let v = mid + amp * (std::f32::consts::TAU * freq * x as f32).sin();
            let i = (y * w + x) * 3;
            img[i] = v;
            img[i + 1] = v;
            img[i + 2] = v;
        }
    }
    img
}

/// 竖直阶跃边（左暗右亮），用于测 halo 过冲。
pub fn step_target(w: usize, h: usize, low: f32, high: f32) -> Vec<f32> {
    let mut img = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let v = if x < w / 2 { low } else { high };
            let i = (y * w + x) * 3;
            img[i] = v;
            img[i + 1] = v;
            img[i + 2] = v;
        }
    }
    img
}

/// 均匀灰底加可复现噪声（xorshift，不用外部 rand）。
pub fn noise_target(w: usize, h: usize, mid: f32, sigma: f32, seed: u32) -> Vec<f32> {
    let mut state = seed | 1;
    let mut next = || {
        // xorshift32 → 近似标准正态（两次均匀相加，够用且可复现）
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let u1 = (state >> 8) as f32 / 16_777_216.0;
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let u2 = (state >> 8) as f32 / 16_777_216.0;
        (u1 + u2 - 1.0) * 1.732_050_8 // 方差归一到 1
    };
    let mut img = vec![0.0f32; w * h * 3];
    for px in img.chunks_exact_mut(3) {
        let v = mid + sigma * next();
        px[0] = v;
        px[1] = v;
        px[2] = v;
    }
    img
}

/// 可选的高斯模糊，用来模拟「镜头 + 抗锯齿滤镜」把边缘弄软。
///
/// 合成靶本身是数字理想边（MTF 直到 Nyquist 都不掉），直接拿去锐化没有
/// 提升空间可言；真实照片的 MTF50 落在 0.2–0.35 cycles/px，所以扫参前
/// 先加一层模糊，才有「锐化把 MTF50 推回去」这件事可测。
pub fn gaussian_blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma <= 0.0 {
        return src.to_vec();
    }
    let radius = (sigma * 3.0).ceil() as isize;
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut sum = 0.0f32;
    for i in -radius..=radius {
        let v = (-(i as f32).powi(2) / (2.0 * sigma * sigma)).exp();
        kernel.push(v);
        sum += v;
    }
    for v in &mut kernel {
        *v /= sum;
    }

    let mut tmp = vec![0.0f32; src.len()];
    // 横
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for (k, kv) in kernel.iter().enumerate() {
                let sx = (x as isize + k as isize - radius).clamp(0, w as isize - 1) as usize;
                let i = (y * w + sx) * 3;
                for c in 0..3 {
                    acc[c] += src[i + c] * kv;
                }
            }
            let i = (y * w + x) * 3;
            tmp[i..i + 3].copy_from_slice(&acc);
        }
    }
    // 纵
    let mut out = vec![0.0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            for (k, kv) in kernel.iter().enumerate() {
                let sy = (y as isize + k as isize - radius).clamp(0, h as isize - 1) as usize;
                let i = (sy * w + x) * 3;
                for c in 0..3 {
                    acc[c] += tmp[i + c] * kv;
                }
            }
            let i = (y * w + x) * 3;
            out[i..i + 3].copy_from_slice(&acc);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 指标
// ---------------------------------------------------------------------------

/// 单频率分量幅度：把一行（取绿色通道）投影到 `freq` 上。
///
/// 比 max−min 稳：噪声和更高次谐波不会算进来，量的是「这一根谱线」还剩多少。
///
/// 先加 Hann 窗再投影，否则图像两端被截断会把能量漏到邻近频点（扫参时
/// 低频段漏得尤其明显）。窗的相干增益是 0.5，正弦单边幅度是 N/2，两者相乘
/// 得 N/4，所以标定系数是 `4/w`。
pub fn amp_at(img: &[f32], w: usize, y: usize, freq: f32) -> f32 {
    if w < 4 {
        return 0.0;
    }
    let mut re = 0.0f32;
    let mut im = 0.0f32;
    for x in 0..w {
        let v = img[(y * w + x) * 3 + 1];
        let hann = 0.5 - 0.5 * (std::f32::consts::TAU * x as f32 / (w - 1) as f32).cos();
        let ph = std::f32::consts::TAU * freq * x as f32;
        re += v * hann * ph.cos();
        im -= v * hann * ph.sin();
    }
    (re * re + im * im).sqrt() * 4.0 / w as f32
}

/// 多行平均的单频幅度（避开首末 2 行，减小边界影响）。
pub fn amp_at_mean(img: &[f32], w: usize, h: usize, freq: f32) -> f32 {
    if h <= 4 {
        return amp_at(img, w, 0, freq);
    }
    let rows: Vec<usize> = (2..h - 2).collect();
    let sum: f32 = rows.iter().map(|&y| amp_at(img, w, y, freq)).sum();
    sum / rows.len() as f32
}

/// 逐频率测 MTF：**每个频点单独生成正弦靶**、过一遍 `process`、量剩余振幅。
///
/// 为什么不能「生成一幅 0.1 的正弦、再扫所有频点」：加了 Hann 窗以后，
/// 观测频率必须与信号频率严格相等才有正交性。差 0.006 cyc/px 在 256 px 上
/// 就错开 1.5 个周期，测出来是 0.267 这样的假值（不是 1.0）。所以每个频点
/// 配一幅对应频率的靶，这也更贴近「拍一组不同频率的靶标」的真实做法。
pub fn mtf_through<F>(
    w: usize,
    h: usize,
    amp: f32,
    mid: f32,
    freqs: &[f32],
    process: F,
) -> Vec<(f32, f32)>
where
    F: Fn(&mut Vec<f32>),
{
    freqs
        .iter()
        .map(|&f| {
            let mut img = sine_target(w, h, f, amp, mid);
            process(&mut img);
            (f, amp_at_mean(&img, w, h, f) / amp)
        })
        .collect()
}

/// 由 MTF 曲线插值求 50% 截止频率（cycles/pixel）。
///
/// 返回 `(mtf50, exact)`：`exact=false` 表示曲线在给定范围内没有穿过 0.5
/// （全程高于或低于），此时给端点并让调用方自己判断可信度——不要静默
/// 当成有效值用。
pub fn mtf50(points: &[(f32, f32)]) -> (f32, bool) {
    for pair in points.windows(2) {
        let (f0, m0) = pair[0];
        let (f1, m1) = pair[1];
        if m0 >= 0.5 && m1 < 0.5 {
            let t = (m0 - 0.5) / (m0 - m1);
            return (f0 + (f1 - f0) * t, true);
        }
    }
    match points.last() {
        Some(&(f, m)) if m >= 0.5 => (f, false),
        Some(&(f, _)) => (f, false),
        None => (0.0, false),
    }
}

/// 锐化把 MTF50 抬高了多少倍。
pub fn mtf50_gain(base: &[(f32, f32)], sharp: &[(f32, f32)]) -> f32 {
    let (b, _) = mtf50(base);
    let (s, _) = mtf50(sharp);
    if b <= 0.0 { f32::NAN } else { s / b }
}

/// 取某一个行的剖面（绿色通道）。
pub fn row_profile(img: &[f32], w: usize, y: usize) -> Vec<f32> {
    (0..w).map(|x| img[(y * w + x) * 3 + 1]).collect()
}

/// 阶跃边的过冲比：`(峰值 − 高台) / 台阶高度`，取正侧最大值。
///
/// §46.2 的 `halo < 3%` 就是这个量。负侧的对称下冲不另算——USM 的两侧
/// 过冲幅度基本相同，量一侧即可。
pub fn halo_ratio(img: &[f32], w: usize, h: usize, high: f32, low: f32) -> f32 {
    let step = (high - low).abs();
    if step <= 0.0 {
        return 0.0;
    }
    let mut peak = f32::NEG_INFINITY;
    for y in 2..h.saturating_sub(2).max(3) {
        let row = row_profile(img, w, y);
        // 只看后 2/3（越过台阶之后），避开左边界
        for &v in row.iter().skip(w / 2).take(w / 2) {
            peak = peak.max(v);
        }
    }
    ((peak - high) / step).max(0.0)
}

/// 方差（绿色通道，整幅）。
pub fn variance(img: &[f32]) -> f32 {
    let vals: Vec<f32> = img.chunks_exact(3).map(|p| p[1]).collect();
    if vals.is_empty() {
        return 0.0;
    }
    let mean = vals.iter().sum::<f32>() / vals.len() as f32;
    vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32
}

/// 两幅同尺寸图的 PSNR（dB）。
pub fn psnr(a: &[f32], b: &[f32], peak: f32) -> f32 {
    assert_eq!(a.len(), b.len(), "PSNR 要求同尺寸");
    if a.is_empty() {
        return f32::INFINITY;
    }
    let mse: f32 = a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f32>() / a.len() as f32;
    if mse <= 0.0 {
        return f32::INFINITY;
    }
    10.0 * (peak * peak / mse).log10()
}

/// 扫 MTF 用的标准频点：0.02..0.45，等间隔 24 点。
pub fn mtf_freqs() -> Vec<f32> {
    (0..24).map(|i| 0.02 + 0.43 * i as f32 / 23.0).collect()
}
