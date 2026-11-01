//! HDR 合并：把同一场景的多张不同曝光融成一张（Mertens 曝光融合）。
//!
//! 不做相机响应标定、不还原辐射度：按「曝光好 × 饱和 × 对比」三个公开公式给每个像素
//! 打分，再按拉普拉斯金字塔逐层加权塌缩——亮部听欠曝那张的、暗部听过曝那张的，
//! 权重在金字塔里平滑过渡，接缝处不会出硬边。
//!
//! HDR merge: fuse several exposures of the same scene into one image (Mertens exposure
//! fusion).
//!
//! No camera response calibration and no radiance recovery: each pixel is scored by the three
//! published formulas "well-exposed x saturated x contrast", then collapsed level by level
//! through a Laplacian pyramid with weights — highlights follow the underexposed frame, shadows
//! follow the overexposed frame, and the weights transition smoothly across the pyramid so a
//! seam shows no hard edge.

use rayon::prelude::*;

/// 一次最多融几张：全图缓冲要同时在内存里，得给上限一个说法。
///
/// Maximum frames per fusion: every full-resolution buffer lives in memory at once, so the
/// limit needs a stated bound.
pub const MAX_FRAMES: usize = 9;

#[derive(Debug)]
pub enum HdrError {
    TooFew(usize),
    TooMany(usize),
    SizeMismatch {
        aw: u32,
        ah: u32,
        b: usize,
        bw: u32,
        bh: u32,
    },
    Read(String),
}

impl crate::errmsg::Msg for HdrError {
    fn msg(&self) -> (&'static str, Vec<crate::errmsg::Arg<'_>>) {
        match self {
            Self::TooFew(v) => (
                "HDR 合并至少要 2 张，实得 {} 张",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::TooMany(v) => (
                "HDR 合并一次最多 {} 张，实得 {} 张",
                vec![
                    crate::errmsg::Arg::Text(MAX_FRAMES.to_string()),
                    crate::errmsg::Arg::Text(v.to_string()),
                ],
            ),
            Self::SizeMismatch { aw, ah, b, bw, bh } => (
                "第 0 张是 {}×{}，第 {} 张是 {}×{}：尺寸不一致，先对齐或裁成同尺寸",
                vec![
                    crate::errmsg::Arg::Text(aw.to_string()),
                    crate::errmsg::Arg::Text(ah.to_string()),
                    crate::errmsg::Arg::Text(b.to_string()),
                    crate::errmsg::Arg::Text(bw.to_string()),
                    crate::errmsg::Arg::Text(bh.to_string()),
                ],
            ),
            Self::Read(v) => (
                "读图失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
        }
    }
}

crate::impl_msg_display!(HdrError);

impl std::error::Error for HdrError {}

/// 读一张图成 f32 RGB（0..1）：与导出共用同一条解码路，方向与 RAW 语义一致。
///
/// Reads one image into f32 RGB (0..1) over the decode path shared with export, oriented as
/// RAW semantics require.
pub fn load(path: &std::path::Path) -> Result<(Vec<f32>, u32, u32), HdrError> {
    crate::export::load_rgb_f32(path).map_err(|e| HdrError::Read(e.to_string()))
}

/// 文件级入口：读盘、核尺寸、融合。位移/裁切由调用方先按对齐结果处理好再传进来。
///
/// File-level entry point: read from disk, check the sizes, fuse. The caller applies the
/// shift and crop from the alignment result before passing frames in.
pub fn merge_paths(paths: &[&std::path::Path]) -> Result<(Vec<f32>, u32, u32), HdrError> {
    check_count(paths.len())?;
    let mut frames = Vec::with_capacity(paths.len());
    let mut size = None;
    for (i, path) in paths.iter().enumerate() {
        let (rgb, w, h) = load(path)?;
        match size {
            None => size = Some((w, h)),
            Some((sw, sh)) if (sw, sh) != (w, h) => {
                return Err(HdrError::SizeMismatch {
                    aw: sw,
                    ah: sh,
                    b: i,
                    bw: w,
                    bh: h,
                });
            }
            Some(_) => {}
        }
        frames.push(rgb);
    }
    let (w, h) = size.unwrap_or((0, 0));
    let refs: Vec<&[f32]> = frames.iter().map(|f| f.as_slice()).collect();
    Ok((merge(&refs, w as usize, h as usize), w, h))
}

fn check_count(n: usize) -> Result<(), HdrError> {
    if n < 2 {
        return Err(HdrError::TooFew(n));
    }
    if n > MAX_FRAMES {
        return Err(HdrError::TooMany(n));
    }
    Ok(())
}

/// 缓冲级融合：`frames` 每项是 `w*h*3` 的 f32 RGB（0..1），返回同尺寸一张。
///
/// Buffer-level fusion: each entry of `frames` is `w*h*3` f32 RGB (0..1); one image of the
/// same size is returned.
pub fn merge(frames: &[&[f32]], w: usize, h: usize) -> Vec<f32> {
    let n = frames.len();
    if n == 0 || w == 0 || h == 0 {
        return Vec::new();
    }
    if n == 1 {
        return frames[0].to_vec();
    }
    // 第 0 层权重：三因子相乘，再跨输入逐像素归一。
    let raw: Vec<Vec<f32>> = frames.par_iter().map(|f| weight_map(f, w, h)).collect();
    let weights: Vec<Vec<f32>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let mut out = vec![0.0f32; w * h];
            out.par_iter_mut().enumerate().for_each(|(p, slot)| {
                let sum: f32 = raw.iter().map(|map| map[p]).sum();
                *slot = if sum > 1e-9 {
                    raw[i][p] / sum
                } else {
                    1.0 / n as f32
                };
            });
            out
        })
        .collect();

    let levels = pyramid_levels(w, h);
    // 塌缩累加器：每层一张 RGB，逐帧往里加「该层权重 × 该层拉普拉斯」。
    let mut acc: Vec<Vec<f32>> = (0..=levels)
        .map(|l| {
            let (lw, lh) = level_size(w, h, l);
            vec![0.0f32; lw * lh * 3]
        })
        .collect();
    for i in 0..n {
        accumulate(frames[i], &weights[i], w, h, levels, &mut acc);
    }

    // 从最粗一层往下塌缩：上采样到下一层尺寸再相加。
    let mut out = acc.pop().expect("至少一层");
    for l in (0..levels).rev() {
        let (lw, lh) = level_size(w, h, l);
        let up = upsample(&out, level_size(w, h, l + 1), (lw, lh));
        out = acc.pop().expect("层数对得上");
        out.par_iter_mut().zip(up).for_each(|(slot, v)| *slot += v);
    }
    out.par_iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
    out
}

fn pyramid_levels(w: usize, h: usize) -> usize {
    let small = w.min(h).max(1);
    let log = (usize::BITS - small.leading_zeros()) as usize;
    log.saturating_sub(4).clamp(1, 6)
}

fn level_size(w: usize, h: usize, level: usize) -> (usize, usize) {
    ((w.max(1) >> level).max(1), (h.max(1) >> level).max(1))
}

/// 曝光好 × 饱和 × 对比（Mertens 三因子，指数都取 1）。
fn weight_map(rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
    let px = w * h;
    let luma: Vec<f32> = (0..px)
        .into_par_iter()
        .map(|p| 0.299 * rgb[p * 3] + 0.587 * rgb[p * 3 + 1] + 0.114 * rgb[p * 3 + 2])
        .collect();
    (0..px)
        .into_par_iter()
        .map(|p| {
            let (r, g, b) = (rgb[p * 3], rgb[p * 3 + 1], rgb[p * 3 + 2]);
            // 曝光好：三通道都离 0.5 近才高分（σ=0.2，论文同值）。
            let exposed = (-((r - 0.5).powi(2) + (g - 0.5).powi(2) + (b - 0.5).powi(2))
                / (2.0 * 0.2 * 0.2))
                .exp();
            // 饱和：三通道相对均值的标准差，灰像素拿不到分。
            let mean = (r + g + b) / 3.0;
            let sat = (((r - mean).powi(2) + (g - mean).powi(2) + (b - mean).powi(2)) / 3.0).sqrt();
            // 对比：亮度拉普拉斯的绝对值，平坦区不抢权重。
            let x = p % w;
            let y = p / w;
            let center = luma[p];
            let left = if x > 0 { luma[p - 1] } else { center };
            let right = if x + 1 < w { luma[p + 1] } else { center };
            let up = if y > 0 { luma[p - w] } else { center };
            let down = if y + 1 < h { luma[p + w] } else { center };
            let contrast = (left + right + up + down - 4.0 * center).abs();
            exposed * sat * contrast
        })
        .collect()
}

/// 一帧的贡献：权重逐层下采样、像素做拉普拉斯层，逐层加进累加器。
fn accumulate(
    img: &[f32],
    weight: &[f32],
    w: usize,
    h: usize,
    levels: usize,
    acc: &mut [Vec<f32>],
) {
    let mut cur = img.to_vec();
    let mut wcur = weight.to_vec();
    let mut size = (w, h);
    for l in 0..levels {
        let next_size = level_size(w, h, l + 1);
        let next = downsample(&cur, size, next_size);
        let back = upsample(&next, next_size, size);
        add_weighted_laplacian(&cur, &back, &wcur, &mut acc[l]);
        wcur = downsample(&wcur, size, next_size);
        cur = next;
        size = next_size;
    }
    add_weighted_flat(&cur, &wcur, &mut acc[levels]);
}

fn add_weighted_laplacian(cur: &[f32], back: &[f32], weight: &[f32], acc: &mut [f32]) {
    acc.par_chunks_mut(3)
        .zip(
            cur.par_chunks(3)
                .zip(back.par_chunks(3))
                .zip(weight.par_iter()),
        )
        .for_each(|(slot, ((c, b), w))| {
            for k in 0..3 {
                slot[k] += w * (c[k] - b[k]);
            }
        });
}

fn add_weighted_flat(cur: &[f32], weight: &[f32], acc: &mut [f32]) {
    acc.par_chunks_mut(3)
        .zip(cur.par_chunks(3).zip(weight.par_iter()))
        .for_each(|(slot, (c, w))| {
            for k in 0..3 {
                slot[k] += w * c[k];
            }
        });
}

/// 二项核 [1 4 6 4 1]/16 可分离模糊后隔点取样；边界复制。
fn downsample(src: &[f32], (sw, sh): (usize, usize), (dw, dh): (usize, usize)) -> Vec<f32> {
    let channels = src.len() / (sw * sh).max(1);
    let kern = [1.0f32, 4.0, 6.0, 4.0, 1.0];
    let norm: f32 = kern.iter().sum();
    let mut tmp = vec![0.0f32; src.len()];
    tmp.par_chunks_mut(sw * channels)
        .zip(src.par_chunks(sw * channels))
        .for_each(|(row_out, row_in)| {
            for x in 0..sw {
                for c in 0..channels {
                    let mut sum = 0.0;
                    for (k, kv) in kern.iter().enumerate() {
                        let sx = (x as isize + k as isize - 2).clamp(0, sw as isize - 1);
                        sum += kv * row_in[sx as usize * channels + c];
                    }
                    row_out[x * channels + c] = sum / norm;
                }
            }
        });
    let mut out = vec![0.0f32; dw * dh * channels];
    out.par_chunks_mut(dw * channels)
        .enumerate()
        .for_each(|(dy, row_out)| {
            let sy = (dy * 2).min(sh.saturating_sub(1));
            for dx in 0..dw {
                let sx = (dx * 2).min(sw.saturating_sub(1));
                for c in 0..channels {
                    let mut sum = 0.0;
                    for (k, kv) in kern.iter().enumerate() {
                        let sy2 = (sy as isize + k as isize - 2).clamp(0, sh as isize - 1);
                        sum += kv * tmp[sy2 as usize * sw * channels + sx * channels + c];
                    }
                    row_out[dx * channels + c] = sum / norm;
                }
            }
        });
    out
}

/// 双线性上采样到目标尺寸（不假设正好 2 倍，最粗层可能是奇数边）。
fn upsample(src: &[f32], (sw, sh): (usize, usize), (dw, dh): (usize, usize)) -> Vec<f32> {
    let channels = src.len() / (sw * sh).max(1);
    let mut out = vec![0.0f32; dw * dh * channels];
    out.par_chunks_mut(dw * channels)
        .enumerate()
        .for_each(|(dy, row_out)| {
            let fy = if dh > 1 {
                dy as f32 * (sh - 1) as f32 / (dh - 1) as f32
            } else {
                0.0
            };
            let y0 = fy.floor() as usize;
            let y1 = (y0 + 1).min(sh - 1);
            let ty = fy - y0 as f32;
            for dx in 0..dw {
                let fx = if dw > 1 {
                    dx as f32 * (sw - 1) as f32 / (dw - 1) as f32
                } else {
                    0.0
                };
                let x0 = fx.floor() as usize;
                let x1 = (x0 + 1).min(sw - 1);
                let tx = fx - x0 as f32;
                for c in 0..channels {
                    let v00 = src[(y0 * sw + x0) * channels + c];
                    let v10 = src[(y0 * sw + x1) * channels + c];
                    let v01 = src[(y1 * sw + x0) * channels + c];
                    let v11 = src[(y1 * sw + x1) * channels + c];
                    let top = v00 + (v10 - v00) * tx;
                    let bot = v01 + (v11 - v01) * tx;
                    row_out[dx * channels + c] = top + (bot - top) * ty;
                }
            }
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(v: f32, w: usize, h: usize) -> Vec<f32> {
        vec![v; w * h * 3]
    }

    #[test]
    fn identical_frames_fuse_to_themselves() {
        let a = flat(0.42, 32, 24);
        let b = flat(0.42, 32, 24);
        let refs = [a.as_slice(), b.as_slice()];
        let out = merge(&refs, 32, 24);
        assert_eq!(out.len(), 32 * 24 * 3);
        for v in &out {
            assert!((v - 0.42).abs() < 1e-3, "同图相融不该变：{v}");
        }
    }

    #[test]
    fn fused_highlights_listen_to_the_underexposed_frame() {
        // 场景：左半暗、右半亮。过曝那张右半 clipping 成 1.0，欠曝那张右半还在中段。
        let w = 64;
        let h = 16;
        let scene: Vec<f32> = (0..w * h)
            .flat_map(|p| {
                let v = if p % w < w / 2 { 0.15 } else { 0.75 };
                [v, v, v]
            })
            .collect();
        let over: Vec<f32> = scene.iter().map(|v| (v * 3.0).min(1.0)).collect();
        let under: Vec<f32> = scene.iter().map(|v| v / 3.0).collect();
        let refs = [over.as_slice(), under.as_slice(), scene.as_slice()];
        let out = merge(&refs, w, h);
        // 亮区：过曝那张已经 clip 死，融合值必须明显低于 1.0（听另外两张的）。
        let right = out[(8 * w + w - 4) * 3];
        assert!(
            right < 0.95,
            "亮部还贴着 clipping 值，说明权重没起作用：{right}"
        );
        assert!(right > 0.3, "亮部不该被压到暗部去：{right}");
        for v in &out {
            assert!((0.0..=1.0).contains(v), "融合结果出界：{v}");
        }
    }

    #[test]
    fn flat_scene_has_no_contrast_weight() {
        let rgb = flat(0.6, 8, 8);
        let w = weight_map(&rgb, 8, 8);
        assert_eq!(w.len(), 64);
        assert!(w.iter().all(|v| *v == 0.0), "平坦图不该有对比度分");
    }

    #[test]
    fn count_limits_speak_chinese() {
        let err = merge_paths(&[]).unwrap_err();
        assert!(err.to_string().contains("至少要 2 张"), "{err}");
        let many: Vec<&std::path::Path> = (0..MAX_FRAMES + 1)
            .map(|_| std::path::Path::new("nope.png"))
            .collect();
        let err = merge_paths(&many).unwrap_err();
        assert!(err.to_string().contains("最多"), "{err}");
    }

    #[test]
    fn mismatched_sizes_are_refused() {
        let dir = std::env::temp_dir().join(format!("aevum-hdr-size-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        image::RgbImage::from_pixel(8, 6, image::Rgb([40, 50, 60]))
            .save(&a)
            .unwrap();
        image::RgbImage::from_pixel(9, 6, image::Rgb([40, 50, 60]))
            .save(&b)
            .unwrap();
        let err = merge_paths(&[&a, &b]).unwrap_err();
        assert!(err.to_string().contains("尺寸不一致"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
