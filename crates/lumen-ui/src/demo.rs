//! 演示序列：程序生成 48 张日落到夜色的帧，用于截图与无素材时的自检。
//!
//! 只写系统临时目录，不碰用户素材；同参数每次生成结果一致。

use std::path::PathBuf;

const W: u32 = 1280;
const H: u32 = 720;
const COUNT: u32 = 48;

pub fn ensure_demo() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("lumen_studio_demo");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for i in 0..COUNT {
        let path = dir.join(format!("demo_{i:04}.jpg"));
        if path.exists() {
            continue;
        }
        let img = render_frame(i);
        img.save(&path).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// 演示配乐：3 秒 44.1 kHz 单声道 WAV，给「声音」模块截图用。
///
/// 只是一段逐渐变弱的和弦，够 ffmpeg 探出编码与时长；不碰用户素材。
pub fn ensure_demo_audio() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("lumen_studio_demo");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("score.wav");
    if path.exists() {
        return Ok(path);
    }
    const RATE: u32 = 44_100;
    const SECONDS: f32 = 3.0;
    let total = (RATE as f32 * SECONDS) as u32;
    let mut pcm = Vec::with_capacity(total as usize * 2);
    for n in 0..total {
        let t = n as f32 / RATE as f32;
        let env = (1.0 - t / SECONDS).max(0.0).powf(0.6);
        let tone = (t * 220.0 * std::f32::consts::TAU).sin() * 0.5
            + (t * 277.18 * std::f32::consts::TAU).sin() * 0.3
            + (t * 329.63 * std::f32::consts::TAU).sin() * 0.2;
        let sample = (tone * env * 0.35 * i16::MAX as f32) as i16;
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes()); // 字节率
    wav.extend_from_slice(&2u16.to_le_bytes()); // 块对齐
    wav.extend_from_slice(&16u16.to_le_bytes()); // 位深
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm);
    std::fs::write(&path, wav).map_err(|e| e.to_string())?;
    Ok(path)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn render_frame(i: u32) -> image::RgbImage {
    let t = i as f32 / (COUNT - 1) as f32;
    // 每隔 7 帧压暗一点：给去闪烁一段真实的抖动。
    let flicker = if i.is_multiple_of(7) {
        0.90
    } else if i.is_multiple_of(11) {
        0.96
    } else {
        1.0
    };
    let sun_x = 0.30 + 0.42 * t;
    let sun_y = 0.50 + 0.10 * t;
    let mut img = image::RgbImage::new(W, H);
    let mut seed = 0x9E37_79B9u32.wrapping_add(i.wrapping_mul(7919));
    for y in 0..H {
        let v = y as f32 / H as f32;
        for x in 0..W {
            let u = x as f32 / W as f32;
            // 天空：顶部靛蓝 → 地平线琥珀
            let horizon = 0.66;
            let k = (v / horizon).clamp(0.0, 1.0);
            let k = k * k * (3.0 - 2.0 * k);
            let mut r = mix(16.0, 236.0, k);
            let mut g = mix(22.0, 142.0, k);
            let mut b = mix(46.0, 78.0, k);
            // 落日与光晕
            let dx = (u - sun_x) * (W as f32 / H as f32);
            let dy = v - sun_y;
            let d = (dx * dx + dy * dy).sqrt();
            let glow = (-d * 9.0).exp() * (0.55 + 0.15 * t);
            r += glow * 210.0;
            g += glow * 120.0;
            b += glow * 30.0;
            if d < 0.026 {
                r = 255.0;
                g = 238.0;
                b = 196.0;
            }
            // 山脊剪影
            let ridge = 0.80 + 0.028 * (u * 7.5 + 1.2).sin() + 0.016 * (u * 21.0 + 0.4).sin();
            if v > ridge {
                let depth = (v - ridge) * 3.0;
                r = mix(10.0, 4.0, depth);
                g = mix(12.0, 5.0, depth);
                b = mix(16.0, 8.0, depth);
            }
            // 星点（只在上半天空，逐帧固定）
            if v < 0.45 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                if seed % 1301 < 3 {
                    let star = 120.0 + (t * 80.0);
                    r += star;
                    g += star;
                    b += star * 1.05;
                }
            }
            // 颗粒
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let grain = ((seed >> 24) as f32 / 255.0 - 0.5) * 5.0;
            let c = [
                ((r * flicker + grain).clamp(0.0, 255.0)) as u8,
                ((g * flicker + grain).clamp(0.0, 255.0)) as u8,
                ((b * flicker + grain).clamp(0.0, 255.0)) as u8,
            ];
            img.put_pixel(x, y, image::Rgb(c));
        }
    }
    img
}
