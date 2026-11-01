//! AI 降噪：两个 4 层 3×3 卷积的残差网络，亮度一路、色度一路。
//!
//! 权重是 `tools/train_denoise.py` 用本机真实延时帧自训出来的
//! （见 `denoise_weights.rs` / `denoise_chroma_weights.rs` 头注释），推理只在这里
//! 做，没有外部依赖。
//!
//! 两路分开训：三通道合训时亮度梯度会把主干占满，色度输出停在恒等（实测 5000
//! 轮后色度 PSNR 仍为 0.00 dB）。亮度网输入 Rec.601 亮度，色度网输入
//! (Cb, Cr) = (B − Y, R − Y)——有符号色差在灰上恒为 0，色度降噪不会把颜色拖偏，
//! 两路也不会互相串。输出都是增量，回写后按同一组定义反解 RGB。
//!
//! 分块推理：每块算完只取中心，周围留 [`RADIUS`] 像素的重叠区供感受野吃，
//! 于是 24MP 也不会一次分配 `w·h·12` 的中间层。
//!
//! x86_64 上有 AVX2+FMA 就走手写向量路径（12 通道补到 16），否则退回标量；
//! 两条路的结果在 1e-6 量级内一致。5984×3959 单帧两路合计约 2.3 秒、
//! 1600×1067 预览约 0.2 秒（release、16 线程、min-of-5 量法）；标量路径
//! 单网就要约 5.7 秒，向量化是这项滑杆能用的前提。
//!
//! AI denoise: two residual networks of four 3×3 convolutions, one path for luminance and one
//! for chroma.
//!
//! The weights are self-trained by `tools/train_denoise.py` on real timelapse frames from this
//! machine (see the header comments of `denoise_weights.rs` /
//! `denoise_chroma_weights.rs`). Inference happens only here, with no external dependency.
//!
//! The two paths are trained separately: joint three-channel training lets the luminance
//! gradient fill the trunk and the chroma output stalls at identity (measured: chroma PSNR was
//! still 0.00 dB after 5000 epochs). The luminance net takes Rec.601 luma as input, and the
//! chroma net takes (Cb, Cr) = (B − Y, R − Y) — a signed color difference is identically zero
//! on gray, so chroma denoise never drags colors off and the two paths do not bleed into each
//! other. Both nets output deltas, and RGB is recovered through the same definitions on
//! write-back.
//!
//! Tiled inference: only the center of each tile is kept, with an overlap of [`RADIUS`] pixels
//! around it to feed the receptive field, so a 24MP image never allocates a `w·h·12`
//! intermediate buffer in one go.
//!
//! On x86_64 with AVX2+FMA a hand-written vector path runs (12 channels padded to 16),
//! otherwise the code falls back to scalar; the two paths agree to within about 1e-6.
//! A single 5984×3959 frame takes about 2.3 s for both paths and a 1600×1067 preview about
//! 0.2 s (release, 16 threads, min-of-5 timing); the scalar path alone needs about 5.7 s per
//! net, so vectorization is the precondition for this slider being usable.

use rayon::prelude::*;

use crate::denoise_chroma_weights as cw;
use crate::denoise_weights as wt;

/// 4 层 3×3 的感受野半径（像素）。
///
/// Receptive field radius of the four 3×3 layers, in pixels.
pub const RADIUS: usize = 4;

/// 两个网络的中间层通道数都必须与 SIMD 路径的常量一致。
const _: () = assert!(cw::CHANNELS == wt::CHANNELS);

const C: usize = wt::CHANNELS;
const CC: usize = cw::CHANNELS;

/// 输出块边长；太小则重叠区占比高、太大则单块内存高（256²×12×4B ≈ 3MB）。
const TILE: usize = 256;

/// 12 通道在 SIMD 路径里补到 16（两个 ymm），标量路径按同一 stride 读。
const PAD: usize = 16;

/// 权重换成 `[tap][cin][pad]`：内层沿 cout 累加，`acc[c] += s * w[c]` 两侧都是
/// 定长数组，编译器能整段向量化。原来的 `[cout][tap][cin]` 布局沿 cin 做点积，
/// 长度 12 定长却跨步读，实测退化成标量（慢 4–6 倍）。
fn transpose(src: &[f32], cin: usize, cout: usize, pad: usize) -> Vec<f32> {
    let mut t = vec![0.0f32; 9 * cin * pad];
    for c in 0..cout {
        for k in 0..9 {
            for i in 0..cin {
                t[(k * cin + i) * pad + c] = src[c * cin * 9 + k * cin + i];
            }
        }
    }
    t
}

static K1: std::sync::LazyLock<Vec<f32>> =
    std::sync::LazyLock::new(|| transpose(&wt::W1, 1, C, PAD));
static K2: std::sync::LazyLock<Vec<f32>> =
    std::sync::LazyLock::new(|| transpose(&wt::W2, C, C, PAD));
static K3: std::sync::LazyLock<Vec<f32>> =
    std::sync::LazyLock::new(|| transpose(&wt::W3, C, C, PAD));
static K4: std::sync::LazyLock<Vec<f32>> = std::sync::LazyLock::new(|| transpose(&wt::W4, C, 1, 1));

static CK1: std::sync::LazyLock<Vec<f32>> =
    std::sync::LazyLock::new(|| transpose(&cw::W1, 2, CC, PAD));
static CK2: std::sync::LazyLock<Vec<f32>> =
    std::sync::LazyLock::new(|| transpose(&cw::W2, CC, CC, PAD));
static CK3: std::sync::LazyLock<Vec<f32>> =
    std::sync::LazyLock::new(|| transpose(&cw::W3, CC, CC, PAD));
static CK4: std::sync::LazyLock<Vec<f32>> =
    std::sync::LazyLock::new(|| transpose(&cw::W4, CC, 2, 2));

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
#[allow(clippy::too_many_arguments)]
unsafe fn conv12_row<const CIN: usize>(
    input: &[f32],
    y: usize,
    w: usize,
    pw: usize,
    wts: &[f32],
    bias: &[f32; PAD],
    relu: bool,
    orow: &mut [f32],
) {
    use std::arch::x86_64::*;
    unsafe {
        let b0 = _mm256_loadu_ps(bias.as_ptr());
        let b1 = _mm256_loadu_ps(bias.as_ptr().add(8));
        let zero = _mm256_setzero_ps();
        for x in 0..w {
            let mut a0 = b0;
            let mut a1 = b1;
            for dy in 0..3usize {
                let in_row = &input[(y + dy) * pw * CIN..][..pw * CIN];
                for dx in 0..3usize {
                    let src = in_row.as_ptr().add((x + dx) * CIN);
                    let base = (dy * 3 + dx) * CIN * PAD;
                    let wp = wts.as_ptr().add(base);
                    for i in 0..CIN {
                        let s = _mm256_broadcast_ss(&*src.add(i));
                        let w0 = _mm256_loadu_ps(wp.add(i * PAD));
                        let w1 = _mm256_loadu_ps(wp.add(i * PAD + 8));
                        a0 = _mm256_fmadd_ps(s, w0, a0);
                        a1 = _mm256_fmadd_ps(s, w1, a1);
                    }
                }
            }
            if relu {
                a0 = _mm256_max_ps(a0, zero);
                a1 = _mm256_max_ps(a1, zero);
            }
            let o = orow.as_mut_ptr().add((x + 1) * C);
            _mm256_storeu_ps(o, a0);
            _mm_storeu_ps(o.add(8), _mm256_castps256_ps128(a1));
        }
    }
}

/// COUT=12 的卷积走 AVX2：外层照旧按行 rayon，每行调一次带 `target_feature`
/// 的函数（闭包本身不吃 target_feature，所以并行切在行上）。
#[cfg(target_arch = "x86_64")]
fn conv12_avx2<const CIN: usize>(
    input: &[f32],
    w: usize,
    h: usize,
    wts: &[f32],
    bias: &[f32],
    relu: bool,
    out: &mut [f32],
) {
    let pw = w + 2;
    clear_border(out, pw, h + 2, C);
    let row = pw * C;
    let mut b = [0.0f32; PAD];
    b[..C].copy_from_slice(&bias[..C]);
    // 内部有效区是 padded 行 1..=h，写满整段：少写一行就靠 halo 余量兜着，
    // 下一层还会把上一行的残值当输入读进来。
    out[row..(h + 1) * row]
        .par_chunks_mut(row)
        .with_min_len(1)
        .enumerate()
        .for_each(|(y, orow)| {
            // 可用性在 conv_into 里查过一次，这里只受同一份条件保护。
            unsafe { conv12_row::<CIN>(input, y, w, pw, wts, &b, relu, orow) };
        });
}

/// 3×3 卷积。`input`/`out` 都带 1 像素零边：`w`/`h` 是内部有效区，缓冲实际是
/// `(h+2) × (w+2) × 通道`（交错存）。零边准备好了，内层就没有任何越界分支。
///
/// 通道数走 const 泛型，权重是已经转置好的 `[tap][cin][pad]`。
#[allow(clippy::too_many_arguments)]
fn conv_into<const CIN: usize, const COUT: usize>(
    input: &[f32],
    w: usize,
    h: usize,
    wts: &[f32],
    bias: &[f32],
    relu: bool,
    out: &mut [f32],
) {
    #[cfg(target_arch = "x86_64")]
    if COUT == C && simd_available() {
        conv12_avx2::<CIN>(input, w, h, wts, bias, relu, out);
        return;
    }
    conv_into_scalar::<CIN, COUT>(input, w, h, wts, bias, relu, out);
}

#[cfg(target_arch = "x86_64")]
fn simd_available() -> bool {
    std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma")
}

#[allow(clippy::too_many_arguments)]
fn conv_into_scalar<const CIN: usize, const COUT: usize>(
    input: &[f32],
    w: usize,
    h: usize,
    wts: &[f32],
    bias: &[f32],
    relu: bool,
    out: &mut [f32],
) {
    // 标量与矢量共用一份补零权重：COUT=12 时行宽 16，其余按 COUT。
    let wstride = if COUT == C { PAD } else { COUT };
    let pw = w + 2;
    clear_border(out, pw, h + 2, COUT);
    let row = pw * COUT;
    out[row..(h + 1) * row]
        .par_chunks_mut(row)
        .with_min_len(1)
        .enumerate()
        .for_each(|(y, orow)| {
            for x in 0..w {
                let mut acc = [0.0f32; COUT];
                acc.copy_from_slice(bias);
                for dy in 0..3usize {
                    let in_row = &input[(y + dy) * pw * CIN..][..pw * CIN];
                    for dx in 0..3usize {
                        let src: &[f32; CIN] = in_row[(x + dx) * CIN..][..CIN].try_into().unwrap();
                        let base = (dy * 3 + dx) * CIN * wstride;
                        for (i, s) in src.iter().enumerate() {
                            let wr: &[f32; COUT] =
                                wts[base + i * wstride..][..COUT].try_into().unwrap();
                            for c in 0..COUT {
                                acc[c] += s * wr[c];
                            }
                        }
                    }
                }
                let o = &mut orow[(x + 1) * COUT..(x + 1) * COUT + COUT];
                for (c, v) in o.iter_mut().enumerate() {
                    *v = if relu { acc[c].max(0.0) } else { acc[c] };
                }
            }
        });
}

/// 把 `(ph × pw × c)` 缓冲的一圈边框清零（只清边，不整块 memset）。
fn clear_border(buf: &mut [f32], pw: usize, ph: usize, c: usize) {
    let row = pw * c;
    for v in buf[..row].iter_mut() {
        *v = 0.0;
    }
    for v in buf[(ph - 1) * row..].iter_mut() {
        *v = 0.0;
    }
    for y in 1..ph - 1 {
        let r = &mut buf[y * row..(y + 1) * row];
        for v in r[..c].iter_mut() {
            *v = 0.0;
        }
        for v in r[row - c..].iter_mut() {
            *v = 0.0;
        }
    }
}

/// 分块推理要用的中间层缓冲，按最大块一次备好，块间反复用。
struct Scratch {
    f1: Vec<f32>,
    f2: Vec<f32>,
}

impl Scratch {
    fn new() -> Self {
        let span = (TILE + 2 * RADIUS + 2) * (TILE + 2 * RADIUS + 2);
        let wide = span * C.max(CC);
        Self {
            f1: vec![0.0; wide],
            f2: vec![0.0; wide],
        }
    }
}

/// 跑一块：`lr` 是带零边的 `(ch+2) × (cw+2)` 亮度，`blk` 收 `th × tw` 的增量。
fn run_luma(
    s: &mut Scratch,
    lr: &[f32],
    cw: usize,
    ch: usize,
    th: usize,
    tw: usize,
    blk: &mut [f32],
) {
    let pw = cw + 2;
    let n = pw * (ch + 2);
    conv_into::<1, C>(lr, cw, ch, &K1, &wt::B1, true, &mut s.f1[..n * C]);
    conv_into::<C, C>(
        &s.f1[..n * C],
        cw,
        ch,
        &K2,
        &wt::B2,
        true,
        &mut s.f2[..n * C],
    );
    conv_into::<C, C>(
        &s.f2[..n * C],
        cw,
        ch,
        &K3,
        &wt::B3,
        true,
        &mut s.f1[..n * C],
    );
    conv_into::<C, 1>(&s.f1[..n * C], cw, ch, &K4, &wt::B4, false, &mut s.f2[..n]);
    for y in 0..th {
        let src = (y + RADIUS + 1) * pw + RADIUS + 1;
        blk[y * tw..(y + 1) * tw].copy_from_slice(&s.f2[src..src + tw]);
    }
}

/// 跑一块：`lr` 是带零边的 `(ch+2) × (cw+2)` 的 (Cb, Cr)，`blk` 收两路增量。
fn run_chroma(
    s: &mut Scratch,
    lr: &[f32],
    cw: usize,
    ch: usize,
    th: usize,
    tw: usize,
    blk: &mut [f32],
) {
    let pw = cw + 2;
    let n = pw * (ch + 2);
    conv_into::<2, CC>(lr, cw, ch, &CK1, &cw::B1, true, &mut s.f1[..n * CC]);
    conv_into::<CC, CC>(
        &s.f1[..n * CC],
        cw,
        ch,
        &CK2,
        &cw::B2,
        true,
        &mut s.f2[..n * CC],
    );
    conv_into::<CC, CC>(
        &s.f2[..n * CC],
        cw,
        ch,
        &CK3,
        &cw::B3,
        true,
        &mut s.f1[..n * CC],
    );
    conv_into::<CC, 2>(
        &s.f1[..n * CC],
        cw,
        ch,
        &CK4,
        &cw::B4,
        false,
        &mut s.f2[..n * 2],
    );
    for y in 0..th {
        let src = ((y + RADIUS + 1) * pw + RADIUS + 1) * 2;
        blk[y * tw * 2..(y + 1) * tw * 2].copy_from_slice(&s.f2[src..src + tw * 2]);
    }
}

/// 跑一块的入口：`(scratch, 带零边输入, 宽, 高, 输出行, 输出列, 输出)`。
type BlockRun = fn(&mut Scratch, &[f32], usize, usize, usize, usize, &mut [f32]);

/// 在若干平面上按 [`TILE`] 分块跑一个网络，块外按边缘像素复制（推理时图像外的
/// 邻域用复制值，比补零更接近真实画面）。返回 `w × h × NOUT` 的增量。
fn delta_planes<const NIN: usize, const NOUT: usize>(
    planes: [&[f32]; NIN],
    w: usize,
    h: usize,
    run: BlockRun,
) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h * NOUT];
    let mut s = Scratch::new();
    let mut blk = vec![0.0f32; TILE * TILE * NOUT];
    let mut lr = vec![0.0f32; (TILE + 2 * RADIUS + 2) * (TILE + 2 * RADIUS + 2) * NIN];
    let mut y0 = 0;
    while y0 < h {
        let th = TILE.min(h - y0);
        let mut x0 = 0;
        while x0 < w {
            let tw = TILE.min(w - x0);
            let cw = tw + 2 * RADIUS;
            let ch = th + 2 * RADIUS;
            let pw = cw + 2;
            let ph = ch + 2;
            clear_border(&mut lr, pw, ph, NIN);
            for y in 0..ch {
                let sy = (y0 + y).saturating_sub(RADIUS).min(h - 1);
                for x in 0..cw {
                    let sx = (x0 + x).saturating_sub(RADIUS).min(w - 1);
                    let o = ((y + 1) * pw + x + 1) * NIN;
                    for (i, plane) in planes.iter().enumerate() {
                        lr[o + i] = plane[sy * w + sx];
                    }
                }
            }
            run(
                &mut s,
                &lr[..pw * ph * NIN],
                cw,
                ch,
                th,
                tw,
                &mut blk[..th * tw * NOUT],
            );
            for y in 0..th {
                let dst = (y0 + y) * w * NOUT + x0 * NOUT;
                out[dst..dst + tw * NOUT].copy_from_slice(&blk[y * tw * NOUT..(y + 1) * tw * NOUT]);
            }
            x0 += tw;
        }
        y0 += th;
    }
    out
}

/// 按强度（0–1）做 AI 降噪：`strength` 是增量回写比例，0 时原样返回。
///
/// Runs AI denoise at the given strength (0–1): `strength` scales the write-back of the deltas,
/// and 0 returns the input unchanged.
pub fn apply(rgb: &mut [f32], w: usize, h: usize, strength: f32) {
    let k = strength.clamp(0.0, 1.0);
    if k == 0.0 || w == 0 || h == 0 {
        return;
    }
    let luma: Vec<f32> = rgb
        .par_chunks_exact(3)
        .map(|p| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])
        .collect();
    let mut cb = Vec::with_capacity(w * h);
    let mut cr = Vec::with_capacity(w * h);
    for (p, y) in rgb.chunks_exact(3).zip(&luma) {
        cb.push(p[2] - y);
        cr.push(p[0] - y);
    }
    let dy = delta_planes::<1, 1>([&luma], w, h, run_luma);
    let dc = delta_planes::<2, 2>([&cb, &cr], w, h, run_chroma);
    rgb.par_chunks_exact_mut(3)
        .zip(dy.par_iter())
        .zip(dc.par_chunks_exact(2))
        .for_each(|((p, dy), dc)| {
            let y = 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
            let y2 = y + dy * k;
            let cb2 = p[2] - y + dc[0] * k;
            let cr2 = p[0] - y + dc[1] * k;
            p[0] = y2 + cr2;
            p[2] = y2 + cb2;
            p[1] = (y2 - 0.299 * p[0] - 0.114 * p[2]) / 0.587;
        });
    rgb.par_iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定种子的均匀噪声（±0.5），乘系数当高斯用：单测里不求分布，只求可复现。
    fn noise(seed: u64, n: usize) -> Vec<f32> {
        let mut s = seed as u32;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s as f32 / 4_294_967_296.0) - 0.5
            })
            .collect()
    }

    fn gray(w: usize, h: usize, v: f32) -> Vec<f32> {
        vec![v; w * h * 3]
    }

    fn rms(a: &[f32], b: &[f32]) -> f32 {
        let s: f32 = a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>() / a.len() as f32;
        s.sqrt()
    }

    /// 逐像素加同一份噪声（三通道同值）：这样 RGB 空间的 RMS 就等于亮度 RMS，
    /// 亮度降噪器才量得准。
    fn add_gray_noise(rgb: &mut [f32], seed: u64, amp: f32) {
        let n = noise(seed, rgb.len() / 3);
        for (i, p) in rgb.chunks_exact_mut(3).enumerate() {
            for v in p.iter_mut() {
                *v += n[i] * amp;
            }
        }
    }

    fn luma_of(rgb: &[f32]) -> Vec<f32> {
        rgb.chunks_exact(3)
            .map(|p| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])
            .collect()
    }

    #[test]
    fn zero_strength_is_noop() {
        let mut img = gray(64, 48, 0.4);
        let before = img.clone();
        apply(&mut img, 64, 48, 0.0);
        assert_eq!(img, before);
    }

    /// AVX2 路径与标量路径必须落在同一份结果上：两条路读同一份补零权重，
    /// 差别只该是浮点累加顺序。
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn simd_layer_matches_scalar() {
        if !simd_available() {
            return;
        }
        let (w, h) = (37usize, 21usize);
        let pw = w + 2;
        let ph = h + 2;
        let single: Vec<f32> = noise(5, pw * ph).iter().map(|v| v * 2.0).collect();
        let dual: Vec<f32> = noise(9, pw * ph * 2).iter().map(|v| v * 2.0).collect();
        let hidden: Vec<f32> = noise(5, pw * ph * C).iter().map(|v| v * 2.0).collect();
        let mut a = vec![0.0f32; pw * ph * C];
        let mut b = vec![0.0f32; pw * ph * C];
        let max_diff = |a: &[f32], b: &[f32]| {
            let mut max = 0.0f32;
            for (x, y) in a.iter().zip(b) {
                max = max.max((x - y).abs());
            }
            max
        };
        conv_into_scalar::<1, C>(&single, w, h, &K1, &wt::B1, true, &mut a);
        conv12_avx2::<1>(&single, w, h, &K1, &wt::B1, true, &mut b);
        let d = max_diff(&a, &b);
        assert!(d < 1e-5, "亮度第一层 SIMD 与标量差太多：{d}");
        conv_into_scalar::<2, CC>(&dual, w, h, &CK1, &cw::B1, true, &mut a);
        conv12_avx2::<2>(&dual, w, h, &CK1, &cw::B1, true, &mut b);
        let d = max_diff(&a, &b);
        assert!(d < 1e-5, "色度第一层 SIMD 与标量差太多：{d}");
        conv_into_scalar::<C, C>(&hidden, w, h, &K2, &wt::B2, true, &mut a);
        conv12_avx2::<C>(&hidden, w, h, &K2, &wt::B2, true, &mut b);
        let d = max_diff(&a, &b);
        assert!(d < 1e-4, "亮度中间层 SIMD 与标量差太多：{d}");
        conv_into_scalar::<CC, CC>(&hidden, w, h, &CK3, &cw::B3, true, &mut a);
        conv12_avx2::<CC>(&hidden, w, h, &CK3, &cw::B3, true, &mut b);
        let d = max_diff(&a, &b);
        assert!(d < 1e-4, "色度中间层 SIMD 与标量差太多：{d}");
    }

    /// 按定义直接算一层卷积（只回内部有效区），用来核对快路径的覆盖范围。
    fn reference<const CIN: usize, const COUT: usize>(
        input: &[f32],
        w: usize,
        h: usize,
        wts: &[f32],
        bias: &[f32],
        relu: bool,
    ) -> Vec<f32> {
        let pw = w + 2;
        let stride = if COUT == C { PAD } else { COUT };
        let mut out = vec![0.0f32; w * h * COUT];
        for y in 0..h {
            for x in 0..w {
                for c in 0..COUT {
                    let mut acc = bias[c];
                    for dy in 0..3usize {
                        for dx in 0..3usize {
                            for i in 0..CIN {
                                let s = input[((y + dy) * pw + x + dx) * CIN + i];
                                let k = ((dy * 3 + dx) * CIN + i) * stride + c;
                                acc += s * wts[k];
                            }
                        }
                    }
                    out[(y * w + x) * COUT + c] = if relu { acc.max(0.0) } else { acc };
                }
            }
        }
        out
    }

    /// 快路径必须写满整个内部有效区：以前 `out[row..h*row]` 少写最后一行，
    /// 残值顺着层间往下传，等于每个 256 行块底下一条 1px 的错误线。
    /// 先把输出灌满毒值再逐格比对定义，漏写哪一格都看得见。
    fn check_rows<const CIN: usize, const COUT: usize>(
        seed: u64,
        name: &str,
        wts: &[f32],
        bias: &[f32],
        relu: bool,
    ) {
        let (w, h) = (9usize, 5usize);
        let (pw, ph) = (w + 2, h + 2);
        let input: Vec<f32> = noise(seed, pw * ph * CIN).iter().map(|v| v * 2.0).collect();
        let mut out = vec![7.0f32; pw * ph * COUT];
        conv_into::<CIN, COUT>(&input, w, h, wts, bias, relu, &mut out);
        let want = reference::<CIN, COUT>(&input, w, h, wts, bias, relu);
        for y in 0..h {
            for x in 0..w {
                for c in 0..COUT {
                    let got = out[((y + 1) * pw + (x + 1)) * COUT + c];
                    let e = want[(y * w + x) * COUT + c];
                    assert!(
                        (got - e).abs() < 2e-3,
                        "{name} 第 {y} 行第 {x} 列通道 {c}（最后一行 {h}）：{got} != {e}"
                    );
                }
            }
        }
    }

    #[test]
    fn conv_covers_every_inner_row() {
        check_rows::<1, C>(17, "亮度第一层", &K1, &wt::B1, true);
        check_rows::<C, C>(31, "亮度隐藏层", &K2, &wt::B2, true);
        check_rows::<C, 1>(41, "亮度末层", &K4, &wt::B4, false);
        check_rows::<2, CC>(53, "色度第一层", &CK1, &cw::B1, true);
    }

    #[test]
    fn deterministic() {
        let base = gray(96, 72, 0.45);
        let make = |_: ()| {
            let mut img = base.clone();
            add_gray_noise(&mut img, 7, 0.06);
            img
        };
        let mut a = make(());
        let mut b = make(());
        apply(&mut a, 96, 72, 1.0);
        apply(&mut b, 96, 72, 1.0);
        assert_eq!(a, b, "同输入同强度必须逐字相同");
    }

    #[test]
    fn flat_field_keeps_level() {
        let mut img = gray(80, 60, 0.5);
        add_gray_noise(&mut img, 11, 0.05);
        let before = luma_of(&img);
        apply(&mut img, 80, 60, 1.0);
        let after = luma_of(&img);
        let mean: f32 = after.iter().sum::<f32>() / after.len() as f32;
        assert!((mean - 0.5).abs() < 0.01, "均值漂了：{mean}");
        let std = |v: &[f32], m: f32| {
            (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt()
        };
        let s0 = std(&before, 0.5);
        let s1 = std(&after, 0.5);
        assert!(s1 < s0 * 0.6, "平坦区降噪不足：{s0} → {s1}");
    }

    #[test]
    fn chroma_noise_is_reduced() {
        // 只给 B 通道加噪：Y 与 Cr 干净、Cb 带噪。模型要把色度压下来，
        // 而且不能把平均颜色拉走。
        let (w, h) = (96usize, 64usize);
        let mut img = vec![0.0f32; w * h * 3];
        for p in img.chunks_exact_mut(3) {
            p[0] = 0.52;
            p[1] = 0.35;
            p[2] = 0.30;
        }
        let mut noisy = img.clone();
        let n = noise(31, w * h);
        for (i, p) in noisy.chunks_exact_mut(3).enumerate() {
            p[2] += n[i] * 0.05;
        }
        let cb = |img: &[f32]| -> Vec<f32> {
            img.chunks_exact(3)
                .map(|p| {
                    let y = 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2];
                    p[2] - y
                })
                .collect()
        };
        let clean = cb(&img);
        let before = rms(&cb(&noisy), &clean);
        apply(&mut noisy, w, h, 1.0);
        let after = rms(&cb(&noisy), &clean);
        assert!(after < before * 0.8, "色度降噪太弱：{before} → {after}");
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let (got, want) = (mean(&cb(&noisy)), mean(&clean));
        assert!((got - want).abs() < 0.01, "色度均值漂了：{got} vs {want}");
    }

    #[test]
    fn reduces_noise_and_keeps_both_sides() {
        // 左 0.25 / 右 0.75 的竖边，边长取 97（不是 TILE 的整数倍，顺便测分块拼接）。
        let (w, h) = (97usize, 64usize);
        let mut clean = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.25 } else { 0.75 };
                for c in 0..3 {
                    clean[(y * w + x) * 3 + c] = v;
                }
            }
        }
        let mut noisy = clean.clone();
        add_gray_noise(&mut noisy, 23, 0.07);
        let clean_y = luma_of(&clean);
        let before = rms(&luma_of(&noisy), &clean_y);
        apply(&mut noisy, w, h, 1.0);
        let after = rms(&luma_of(&noisy), &clean_y);
        assert!(after < before * 0.7, "降噪太弱：{before} → {after}");

        let side = |img: &[f32], left: bool| {
            let mut sum = 0.0f32;
            let mut cnt = 0;
            for y in 8..h - 8 {
                for x in 8..w - 8 {
                    let is_left = x < w / 2 - 8;
                    let is_right = x > w / 2 + 8;
                    if (left && is_left) || (!left && is_right) {
                        sum += img[(y * w + x) * 3 + 1];
                        cnt += 1;
                    }
                }
            }
            sum / cnt as f32
        };
        assert!((side(&noisy, true) - 0.25).abs() < 0.02, "左半压暗");
        assert!((side(&noisy, false) - 0.75).abs() < 0.02, "右半拉亮");
    }
}
