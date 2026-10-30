//! 机型镜头配置文件：读磁盘上的 lensfun 数据库，按 EXIF 的厂商/机型/镜头/
//! 焦距匹配校正系数（畸变 / 横向色差 / 暗角），折成一个可放进调色参数的
//! [`Correction`]，在显影链最前面（手动畸变滑杆之前）应用到像素。
//!
//! 数据库不随程序分发：按 `AEVUM_LENSFUN` → exe 同目录与 `tools/` 下的
//! `lensfun/` 目录 → 传入路径查找，找不到就整体降级为「没有配置文件」，
//! 手动三项照常可用。解析器只认 lensfun XML 用到的那一小撮语法，
//! 模型公式来自 lensfun 公开文档：
//!
//! - 畸变（目标坐标 = 校正目标半径，自变量是未畸变半径 Ru）：
//!   poly3 `Rd = Ru·(1+k1·Ru²)`、poly5 `Rd = Ru·(1+k1·Ru²+k2·Ru⁴)`、
//!   ptlens `Rd = Ru·(a·Ru³+b·Ru²+c·Ru+1)`；
//! - 横向色差 poly3：红/蓝平面相对绿平面的径向缩放 `f = v + b·r²`；
//! - 暗角 pa：`v = 1 + k1·r² + k2·r⁴ + k3·r⁶`。
//!
//! 半径统一用「tan 单位」：`r = 到中心的像素距离 × 传感器对角线(mm)
//! ÷ 画面对角线(px) ÷ 焦距(mm)`。这是 lensfun 修正器同一套归一化
//! （对角线按 `hypot(36,24)/裁切系数` 取毫米数），系数本身与机身无关，
//! 机身裁切系数只参与 px↔mm 换算，因此不做跨裁切的系数缩放。
//! 畸变校正需要在逐像素方向上解 `Ru·P(Ru) = Rd`：多项式在常用范围内
//! 单调，用牛顿迭代在一张 2048 档的半径查找表上解一次，逐像素查表插值。
//!
//! 与 lensfun 的已知口径差（都有意为之，注释即文档）：
//! - 畸变/色差/暗角共用同一条自中心出发的径向轴，忽略数据库里的
//!   `center-x/center-y`（绝大多数标定不写，写了也接近零）；
//! - 色差系数按目标半径求值后乘在未畸变半径上——色差本身是 1e-4 量级
//!   的微缩放，复合顺序带来的差是更高阶小量；
//! - 暗角忽略 distance 维：同一 (焦距, 光圈) 有多档距离时取最远一档
//!   （风光/延时最相关）；
//! - TCA 只支持 poly3 标定（数据库里 acm 标定的镜头视为没有色差数据）。

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// 传感器对角线换算基准：全幅（36×24mm）对角线的毫米数。
const FULL_FRAME_DIAGONAL_MM: f64 = 43.2666153005;

/// 畸变模型。系数含义随模型不同，见 [`Correction::dist`]。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum DistModel {
    #[default]
    #[serde(rename = "poly3")]
    Poly3,
    #[serde(rename = "poly5")]
    Poly5,
    #[serde(rename = "ptlens")]
    Ptlens,
}

/// 一次匹配折出来的校正系数。三段各自独立：数据库里哪段有标定就带上哪段，
/// 全空等于没有配置文件。放进 [`crate::grade::GradeParams`]（默认 `None`），
/// 预览、导出、队列自然全通。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Correction {
    /// 命中的数据库镜头型号（面板与命令面显示用）。
    #[serde(default)]
    pub lens: String,
    /// 畸变：(模型, [k1] / [k1,k2] / [a,b,c])。
    pub dist: Option<(DistModel, [f64; 3])>,
    /// 横向色差 poly3：[br, vr, bb, vb]，红/蓝平面 `f = v + b·r²`。
    pub tca: Option<[f64; 4]>,
    /// 暗角 pa：[k1, k2, k3]。
    pub vig: Option<[f64; 3]>,
    /// 插值后的标定焦距（名义值，mm），tan 单位换算用。
    pub focal_mm: f64,
    /// 机身裁切系数（px↔mm 换算用），取不到时回退镜头标定裁切、再回退全幅。
    pub crop: f64,
}

impl Correction {
    pub fn is_empty(&self) -> bool {
        self.dist.is_none() && self.tca.is_none() && self.vig.is_none()
    }

    /// 命令面与面板共用的 JSON 形状：查到的可以原样读回来。
    pub fn to_json(&self) -> serde_json::Value {
        let (model, k): (&str, [f64; 3]) = match self.dist {
            Some((DistModel::Poly3, k)) => ("poly3", k),
            Some((DistModel::Poly5, k)) => ("poly5", k),
            Some((DistModel::Ptlens, k)) => ("ptlens", k),
            None => ("none", [0.0; 3]),
        };
        serde_json::json!({
            "lens": self.lens,
            "focal_mm": self.focal_mm,
            "crop": self.crop,
            "dist": self.dist.map(|_| serde_json::json!({"model": model, "k": k})),
            "tca": self.tca,
            "vig": self.vig,
        })
    }

    /// 畸变多项式 P(Ru)（`Rd = Ru·P`）。
    fn poly(&self, ru: f64) -> f64 {
        let ru2 = ru * ru;
        match self.dist {
            Some((DistModel::Poly3, k)) => 1.0 + k[0] * ru2,
            Some((DistModel::Poly5, k)) => 1.0 + k[0] * ru2 + k[1] * ru2 * ru2,
            Some((DistModel::Ptlens, k)) => 1.0 + k[0] * ru2 * ru + k[1] * ru2 + k[2] * ru,
            None => 1.0,
        }
    }

    /// P 的导数（牛顿迭代用）。
    fn poly_deriv(&self, ru: f64) -> f64 {
        let ru2 = ru * ru;
        match self.dist {
            Some((DistModel::Poly3, k)) => 2.0 * k[0] * ru,
            Some((DistModel::Poly5, k)) => 2.0 * k[0] * ru + 4.0 * k[1] * ru2 * ru,
            Some((DistModel::Ptlens, k)) => 3.0 * k[0] * ru2 + 2.0 * k[1] * ru + k[2],
            None => 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// 数据库模型
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct DistPoint {
    pub focal: f64,
    pub model: DistModel,
    pub k: [f64; 3],
}

#[derive(Clone, Debug, Default)]
pub struct TcaPoint {
    pub focal: f64,
    /// [br, vr, bb, vb]（poly3；acm 标定的点不收）。
    pub k: [f64; 4],
}

#[derive(Clone, Debug, Default)]
pub struct VigPoint {
    pub focal: f64,
    pub aperture: f64,
    pub distance: f64,
    pub k: [f64; 3],
}

#[derive(Clone, Debug, Default)]
pub struct LensEntry {
    pub maker: String,
    pub model: String,
    pub crop: Option<f64>,
    pub dist: Vec<DistPoint>,
    pub tca: Vec<TcaPoint>,
    pub vig: Vec<VigPoint>,
}

#[derive(Clone, Debug, Default)]
pub struct CameraEntry {
    pub maker: String,
    pub model: String,
    pub crop: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct LensDb {
    pub lenses: Vec<LensEntry>,
    pub cameras: Vec<CameraEntry>,
}

// ---------------------------------------------------------------------------
// 数据库查找
// ---------------------------------------------------------------------------

/// 按与 ffmpeg 同一套顺序找 lensfun 数据库目录：`AEVUM_LENSFUN`（目录或
/// 单个 xml）→ exe 同目录与 `tools/` 下的 `lensfun/`。返回目录内全部
/// `*.xml`（直接给文件时就是那一个）。
pub fn find_database_files() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(p) = crate::env::var_os("LENSFUN") {
        dirs.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.join("lensfun"));
        dirs.push(dir.join("tools").join("lensfun"));
    }
    for d in dirs {
        let files = xml_files_of(&d);
        if !files.is_empty() {
            return files;
        }
    }
    Vec::new()
}

fn xml_files_of(d: &Path) -> Vec<PathBuf> {
    if d.is_file() {
        return vec![d.to_path_buf()];
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("xml"))
        .collect();
    out.sort();
    out
}

/// 读一组 XML 合成一个库；坏文件跳过（返回成功读入的文件数）。
pub fn load_files(files: &[PathBuf]) -> (LensDb, usize) {
    let mut db = LensDb::default();
    let mut ok = 0;
    for f in files {
        if let Ok(text) = std::fs::read_to_string(f)
            && let Ok(parsed) = parse_db(&text)
        {
            db.lenses.extend(parsed.lenses);
            db.cameras.extend(parsed.cameras);
            ok += 1;
        }
    }
    (db, ok)
}

/// 一步到位：找库、读库。没有库或读不动都返回空库（调用方按空处理）。
pub fn load_auto() -> LensDb {
    let files = find_database_files();
    if files.is_empty() {
        return LensDb::default();
    }
    load_files(&files).0
}

// ---------------------------------------------------------------------------
// 匹配
// ---------------------------------------------------------------------------

/// 归一化：小写、只留字母数字。`NIKON CORPORATION` → `nikoncorporation`，
/// 再由 [`strip_maker_suffixes`] 把公司后缀削掉。
fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// 削掉厂商名里的公司后缀（EXIF 与数据库的写法差）。
fn strip_maker_suffixes(s: &str) -> String {
    for suffix in [
        "corporation",
        "company",
        "optical",
        "imaging",
        "digital",
        "solutions",
        "inc",
        "ltd",
        "corp",
        "co",
    ] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            return strip_maker_suffixes(stripped);
        }
    }
    s.to_owned()
}

fn maker_key(s: &str) -> String {
    strip_maker_suffixes(&norm(s))
}

/// 厂商匹配：剥后缀后互相包含（`omdigitalsolutions` vs `omdigitalsolution` 这类）。
fn maker_matches(a: &str, b: &str) -> bool {
    let (a, b) = (maker_key(a), maker_key(b));
    !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
}

/// 把厂商名从型号开头削掉（`Nikon Z 8` → `Z 8`），剩下的做互相包含匹配。
fn model_key(maker: &str, model: &str) -> String {
    let m = norm(model);
    let mk = norm(maker);
    m.strip_prefix(&mk).unwrap_or(&m).to_owned()
}

fn model_matches(maker_a: &str, model_a: &str, maker_b: &str, model_b: &str) -> bool {
    let a = model_key(maker_a, model_a);
    let b = model_key(maker_b, model_b);
    !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
}

/// 按厂商+机型找机身。多个匹配取型号最短的一个（最具体）。
pub fn find_camera<'a>(db: &'a LensDb, make: &str, model: &str) -> Option<&'a CameraEntry> {
    db.cameras
        .iter()
        .filter(|c| maker_matches(make, &c.maker) && model_matches(make, model, &c.maker, &c.model))
        .min_by_key(|c| c.model.len())
}

/// 按厂商+镜头描述找镜头。EXIF 的 LensModel 往往不带厂商前缀
/// （`EF24-105mm f/4L IS USM`），数据库一般带（`Canon EF 24-105mm ...`），
/// 互相包含能对上；多个匹配取型号最短的。
pub fn find_lens<'a>(db: &'a LensDb, make: &str, lens: &str) -> Option<&'a LensEntry> {
    db.lenses
        .iter()
        .filter(|l| maker_matches(make, &l.maker) && model_matches(make, lens, &l.maker, &l.model))
        .min_by_key(|l| l.model.len())
}

// ---------------------------------------------------------------------------
// 焦距插值
// ---------------------------------------------------------------------------

fn interp_scalar(points: &[(f64, f64)], focal: f64) -> f64 {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if pts.is_empty() {
        return 0.0;
    }
    if focal <= pts[0].0 {
        return pts[0].1;
    }
    if focal >= pts[pts.len() - 1].0 {
        return pts[pts.len() - 1].1;
    }
    for w in pts.windows(2) {
        let (f0, v0) = w[0];
        let (f1, v1) = w[1];
        if focal >= f0 && focal <= f1 {
            if f1 <= f0 {
                return v1;
            }
            let t = (focal - f0) / (f1 - f0);
            return v0 + (v1 - v0) * t;
        }
    }
    pts[pts.len() - 1].1
}

/// 焦距插值出一组校正。`aperture` 未知时暗角取光圈最大的一档（暗角最强、
/// 最值得修的那一档）。三段全空返回 None。
pub fn correction_for(
    entry: &LensEntry,
    focal: f64,
    aperture: Option<f64>,
    camera_crop: Option<f64>,
) -> Option<Correction> {
    if entry.dist.is_empty() && entry.tca.is_empty() && entry.vig.is_empty() {
        return None;
    }
    // 焦距缺省时取标定范围的中点。
    let mut focals: Vec<f64> = entry
        .dist
        .iter()
        .map(|d| d.focal)
        .chain(entry.tca.iter().map(|t| t.focal))
        .chain(entry.vig.iter().map(|v| v.focal))
        .filter(|f| *f > 0.0)
        .collect();
    focals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let focal = if focal > 0.0 {
        focal
    } else {
        focals.first()?.to_owned()
    };

    // 畸变：按最近焦距点的模型分组插值（相邻标定点模型混着时只信最近的一组）。
    let dist = if entry.dist.is_empty() {
        None
    } else {
        let model = entry
            .dist
            .iter()
            .min_by(|a, b| {
                (a.focal - focal)
                    .abs()
                    .partial_cmp(&(b.focal - focal).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|d| d.model)?;
        let pts: Vec<(f64, f64)> = entry
            .dist
            .iter()
            .filter(|d| d.model == model)
            .map(|d| (d.focal, d.k[0]))
            .collect();
        let mut k = [0.0f64; 3];
        k[0] = interp_scalar(&pts, focal);
        if model == DistModel::Poly5 || model == DistModel::Ptlens {
            let pts2: Vec<(f64, f64)> = entry
                .dist
                .iter()
                .filter(|d| d.model == model)
                .map(|d| (d.focal, d.k[1]))
                .collect();
            k[1] = interp_scalar(&pts2, focal);
        }
        if model == DistModel::Ptlens {
            let pts3: Vec<(f64, f64)> = entry
                .dist
                .iter()
                .filter(|d| d.model == model)
                .map(|d| (d.focal, d.k[2]))
                .collect();
            k[2] = interp_scalar(&pts3, focal);
        }
        Some((model, k))
    };

    let tca = if entry.tca.is_empty() {
        None
    } else {
        let mut k = [0.0f64; 4];
        for (i, slot) in k.iter_mut().enumerate() {
            let pts: Vec<(f64, f64)> = entry.tca.iter().map(|t| (t.focal, t.k[i])).collect();
            *slot = interp_scalar(&pts, focal);
        }
        Some(k)
    };

    let vig = if entry.vig.is_empty() {
        None
    } else {
        // 同一 (焦距, 光圈) 有多档距离时取最远一档；再按光圈挑档、按焦距插值。
        let mut best: Vec<VigPoint> = Vec::new();
        for v in &entry.vig {
            if let Some(b) = best
                .iter_mut()
                .find(|b| b.focal == v.focal && b.aperture == v.aperture)
            {
                if v.distance > b.distance {
                    *b = v.clone();
                }
            } else {
                best.push(v.clone());
            }
        }
        let apertures: Vec<f64> = {
            let mut a: Vec<f64> = best.iter().map(|v| v.aperture).collect();
            a.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
            a.dedup();
            a
        };
        let aperture = match aperture {
            Some(a) => apertures
                .iter()
                .copied()
                .min_by(|x, y| {
                    (x - a)
                        .abs()
                        .partial_cmp(&(y - a).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(0.0),
            None => apertures.first().copied().unwrap_or(0.0),
        };
        let mut k = [0.0f64; 3];
        for (i, slot) in k.iter_mut().enumerate() {
            let pts: Vec<(f64, f64)> = best
                .iter()
                .filter(|v| v.aperture == aperture)
                .map(|v| (v.focal, v.k[i]))
                .collect();
            *slot = interp_scalar(&pts, focal);
        }
        Some(k)
    };

    let corr = Correction {
        lens: entry.model.clone(),
        dist,
        tca,
        vig,
        focal_mm: focal,
        crop: camera_crop
            .filter(|c| *c > 0.0)
            .or(entry.crop)
            .unwrap_or(1.0),
    };
    if corr.is_empty() { None } else { Some(corr) }
}

/// EXIF 三件套一步匹配：机身定裁切系数、镜头定标定、焦距/光圈插值。
pub fn match_correction(
    db: &LensDb,
    make: &str,
    model: &str,
    lens: &str,
    focal_mm: f64,
    aperture: Option<f64>,
) -> Option<Correction> {
    let entry = find_lens(db, make, lens)?;
    let crop = find_camera(db, make, model).and_then(|c| c.crop);
    correction_for(entry, focal_mm, aperture, crop)
}

// ---------------------------------------------------------------------------
// 应用到像素
// ---------------------------------------------------------------------------

/// 把校正应用到一帧 sRGB `[0,1]` 浮点像素（行优先 RGB，与显影链同一缓冲
/// 格式）。就地修改。顺序：几何（畸变 + 色差一起重采样）→ 暗角 → 夹紧。
pub fn apply(rgb: &mut [f32], w: usize, h: usize, c: &Correction) {
    if w == 0 || h == 0 || c.is_empty() {
        return;
    }
    // tan 单位换算：px → 归一化半径（对角线口径，机身裁切系数参与 mm 换算）。
    // 对角线取 hypot(w-1, h-1)：与中心 (w-1)/2,(h-1)/2 一起，正好让角落
    // 像素落在 r_max 上。
    let diag_px = (((w - 1) * (w - 1) + (h - 1) * (h - 1)) as f64).sqrt();
    let k_norm = FULL_FRAME_DIAGONAL_MM / c.crop / (diag_px * c.focal_mm.max(1e-9));
    let cx = (w as f64 - 1.0) / 2.0;
    let cy = (h as f64 - 1.0) / 2.0;
    let rd_max = k_norm * (cx * cx + cy * cy).sqrt();

    // 逆畸变查找表：scale(Rd) = Ru/Rd。2048 档足够平滑，牛顿 5 轮收敛。
    let lut: Option<Vec<f32>> = if c.dist.is_some() && rd_max > 1e-12 {
        let n = 2048;
        let mut scale = vec![1.0f32; n];
        for (i, s) in scale.iter_mut().enumerate() {
            let rd = rd_max * (i as f64 + 0.5) / n as f64;
            let mut ru = rd;
            for _ in 0..5 {
                let f = ru * c.poly(ru) - rd;
                let d = ru * c.poly_deriv(ru) + c.poly(ru);
                if d.abs() < 1e-12 {
                    break;
                }
                ru -= f / d;
                if ru < 1e-9 {
                    ru = 1e-9;
                }
            }
            *s = (ru / rd) as f32;
        }
        Some(scale)
    } else {
        None
    };
    let n_lut = lut.as_ref().map_or(2048, |l| l.len());

    let tca = c.tca;

    let dist_scale = |rd: f64| -> f64 {
        match &lut {
            None => 1.0,
            Some(l) => {
                let t = (rd / rd_max * n_lut as f64 - 0.5).clamp(0.0, (n_lut - 1) as f64);
                let i = t as usize;
                let frac = (t - i as f64) as f32;
                let a = l[i];
                let b = l[(i + 1).min(n_lut - 1)];
                (a + (b - a) * frac) as f64
            }
        }
    };

    // 暗角系数（展开成局部量，热循环里不走 Option）。
    let (v1, v2, v3) = match c.vig {
        Some(k) => (k[0], k[1], k[2]),
        None => (0.0, 0.0, 0.0),
    };
    let has_vig = c.vig.is_some();

    // 采样源先复制一份：逐目标像素写回与跨行读源不能共用同一缓冲。
    // 只有畸变或色差才真的要读源——纯暗角不采样，复制整幅纯属白拷。
    let needs_src = c.dist.is_some() || tca.is_some();
    let src: Vec<f32> = if needs_src { rgb.to_vec() } else { Vec::new() };
    let src_ref = &src;
    rgb.par_chunks_exact_mut(w * 3)
        .enumerate()
        .for_each(|(y, row)| {
            let dy = y as f64 - cy;
            for (x, px) in row.chunks_exact_mut(3).enumerate() {
                let dx = x as f64 - cx;
                let rd = (dx * dx + dy * dy).sqrt() * k_norm;
                // 各通道采样缩放：绿通道只做畸变，红/蓝先乘色差缩放再过同一张逆畸变表。
                let mut s = [1.0f64; 3];
                if rd > 1e-12 {
                    if c.dist.is_some() {
                        s = [dist_scale(rd); 3];
                    }
                    if let Some([br, vr, bb, vb]) = tca {
                        let r2 = rd * rd;
                        let f_red = vr + br * r2;
                        let f_blue = vb + bb * r2;
                        s[0] *= f_red;
                        s[2] *= f_blue;
                    }
                }
                let vig = if has_vig {
                    let r2 = rd * rd;
                    (1.0 + v1 * r2 + v2 * r2 * r2 + v3 * r2 * r2 * r2).clamp(0.0, 4.0)
                } else {
                    1.0
                };
                if s == [1.0, 1.0, 1.0] {
                    if vig != 1.0 {
                        px[0] = (px[0] * vig as f32).clamp(0.0, 1.0);
                        px[1] = (px[1] * vig as f32).clamp(0.0, 1.0);
                        px[2] = (px[2] * vig as f32).clamp(0.0, 1.0);
                    }
                    continue;
                }
                for ch in 0..3 {
                    let sx = dx * s[ch] + cx;
                    let sy = dy * s[ch] + cy;
                    px[ch] = (sample(src_ref, w, h, sx, sy, ch) * vig as f32).clamp(0.0, 1.0);
                }
            }
        });
}

// ---------------------------------------------------------------------------
// XML 子集解析
// ---------------------------------------------------------------------------

/// 最小 XML 树：标签名、属性、文本、子节点。只求够解析 lensfun 数据库。
#[derive(Clone, Debug, Default)]
pub(crate) struct Node {
    pub(crate) name: String,
    pub(crate) attrs: Vec<(String, String)>,
    pub(crate) text: String,
    pub(crate) children: Vec<Node>,
}

impl Node {
    pub(crate) fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// 第一个非空子文本（lensfun 的 `<name>Nikon</name>` 这类）。
    pub(crate) fn text_of(&self, name: &str) -> Option<String> {
        self.child(name)
            .map(|c| c.text.trim().to_owned())
            .filter(|t| !t.is_empty())
    }
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let Some(end) = tail.find(';') else {
            // 悬空的 &：按字面收下。
            out.push('&');
            rest = &rest[pos + 1..];
            continue;
        };
        let ent = &tail[1..end];
        out.push_str(&match ent {
            "amp" => "&".to_owned(),
            "lt" => "<".to_owned(),
            "gt" => ">".to_owned(),
            "quot" => "\"".to_owned(),
            "apos" => "'".to_owned(),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32)
                .map(|c| c.to_string())
                .unwrap_or_default(),
            e if e.starts_with('#') => e[1..]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .map(|c| c.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        });
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// 解析出错带位置，方便报哪个文件坏。
pub(crate) fn parse_xml(src: &str) -> Result<Node, String> {
    let b = src.as_bytes();
    let mut stack: Vec<Node> = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    let flush_text = |stack: &mut Vec<Node>, text: &mut String| {
        if !text.is_empty() {
            if let Some(top) = stack.last_mut() {
                top.text.push_str(text);
            }
            text.clear();
        }
    };
    while i < b.len() {
        if b[i] == b'<' {
            let rest = &src[i..];
            let advance = if rest.starts_with("<!--") {
                src[i + 4..].find("-->").ok_or("注释没有闭合")? + 4 + 3
            } else if rest.starts_with("<![CDATA[") {
                let end = src[i + 9..].find("]]>").ok_or("CDATA 没有闭合")? + 9;
                text.push_str(&src[i + 9..i + end]);
                end + 3
            } else if rest.starts_with("<?") || rest.starts_with("<!") {
                // 声明 / DOCTYPE：跳到下一个 '>'（DOCTYPE 内部实体不支持，够用）。
                src[i..].find('>').ok_or("声明没有闭合")? + 1
            } else if rest.starts_with("</") {
                let end = src[i..].find('>').ok_or("结束标签没有闭合")?;
                let name = src[i + 2..i + end].trim();
                flush_text(&mut stack, &mut text);
                let node = stack.pop().ok_or_else(|| format!("多余的 </{name}>"))?;
                if node.name != name {
                    return Err(format!("标签不配对：<{}> vs </{name}>", node.name));
                }
                if let Some(top) = stack.last_mut() {
                    top.children.push(node);
                } else {
                    return Ok(node);
                }
                end + 1
            } else {
                // 开始标签：名字 + 属性，可能自闭合。
                let end = src[i..].find('>').ok_or("开始标签没有闭合")?;
                let inner = &src[i + 1..i + end];
                let self_closing = inner.ends_with('/');
                let inner = inner.trim_end_matches('/');
                let (name, attr_str) = match inner.find(|c: char| c.is_whitespace()) {
                    Some(p) => (&inner[..p], &inner[p..]),
                    None => (inner, ""),
                };
                flush_text(&mut stack, &mut text);
                let node = Node {
                    name: name.trim().to_owned(),
                    attrs: parse_attrs(attr_str)?,
                    ..Node::default()
                };
                if self_closing {
                    if let Some(top) = stack.last_mut() {
                        top.children.push(node);
                    } else {
                        return Ok(node);
                    }
                } else {
                    stack.push(node);
                }
                end + 1
            };
            i += advance;
        } else {
            let next = src[i..].find('<').map_or(src.len(), |p| i + p);
            text.push_str(&decode_entities(&src[i..next]));
            i = next;
        }
    }
    if let Some(top) = stack.last() {
        return Err(format!("标签 <{}> 没有闭合", top.name));
    }
    Err("空文档".to_owned())
}

fn parse_attrs(s: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let eq = rest
            .find('=')
            .ok_or_else(|| format!("属性缺等号：{rest:?}"))?;
        let key = rest[..eq].trim().to_owned();
        let after = rest[eq + 1..].trim_start();
        let quote = after
            .chars()
            .next()
            .ok_or_else(|| format!("属性缺值：{key}"))?;
        if quote != '"' && quote != '\'' {
            return Err(format!("属性值要加引号：{key}"));
        }
        let val_end = after[1..]
            .find(quote)
            .ok_or_else(|| format!("属性值没有闭合：{key}"))?;
        out.push((key, decode_entities(&after[1..1 + val_end])));
        rest = after[1 + val_end + 1..].trim_start();
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 数据库装配
// ---------------------------------------------------------------------------

fn text_num(node: &Node, name: &str) -> Option<f64> {
    node.text_of(name)?.parse().ok()
}

fn parse_db(src: &str) -> Result<LensDb, String> {
    let root = parse_xml(src)?;
    let mut db = LensDb::default();
    // 真实 lensfun：<camera> 与 <lens> 都是 <lensdatabase> 的直接子节点，
    // maker/model 是各自内部的文本子节点，没有「按厂商分组」那一层。
    for cam in root.children.iter().filter(|c| c.name == "camera") {
        db.cameras.push(CameraEntry {
            maker: cam.text_of("maker").unwrap_or_default(),
            model: cam.text_of("model").unwrap_or_default(),
            crop: text_num(cam, "cropfactor"),
        });
    }
    for lens in root.children.iter().filter(|c| c.name == "lens") {
        let mut entry = LensEntry {
            maker: lens.text_of("maker").unwrap_or_default(),
            model: lens.text_of("model").unwrap_or_default(),
            crop: text_num(lens, "cropfactor"),
            ..LensEntry::default()
        };
        for calib in lens.children.iter().filter(|c| c.name == "calibration") {
            for pt in &calib.children {
                let num = |key: &str| pt.attr(key).and_then(|v| v.parse().ok());
                match pt.name.as_str() {
                    "distortion" => {
                        let (Some(focal), Some(model)) = (num("focal"), pt.attr("model")) else {
                            continue;
                        };
                        let k = match model {
                            "poly3" => [num("k1").unwrap_or(0.0), 0.0, 0.0],
                            "poly5" => [num("k1").unwrap_or(0.0), num("k2").unwrap_or(0.0), 0.0],
                            "ptlens" => [
                                num("a").unwrap_or(0.0),
                                num("b").unwrap_or(0.0),
                                num("c").unwrap_or(0.0),
                            ],
                            // brunnen 等新模型不认，宁缺勿错。
                            _ => continue,
                        };
                        entry.dist.push(DistPoint {
                            focal,
                            model: match model {
                                "poly3" => DistModel::Poly3,
                                "poly5" => DistModel::Poly5,
                                _ => DistModel::Ptlens,
                            },
                            k,
                        });
                    }
                    "tca" if pt.attr("model") == Some("poly3") => {
                        let (Some(focal), (Some(br), Some(vr)), (Some(bb), Some(vb))) =
                            (num("focal"), (num("br"), num("vr")), (num("bb"), num("vb")))
                        else {
                            continue;
                        };
                        entry.tca.push(TcaPoint {
                            focal,
                            k: [br, vr, bb, vb],
                        });
                    }
                    "vignetting" if pt.attr("model") == Some("pa") => {
                        let (Some(focal), Some(aperture)) = (num("focal"), num("aperture")) else {
                            continue;
                        };
                        let (Some(k1), Some(k2), Some(k3)) = (num("k1"), num("k2"), num("k3"))
                        else {
                            continue;
                        };
                        entry.vig.push(VigPoint {
                            focal,
                            aperture,
                            distance: num("distance").unwrap_or(10.0),
                            k: [k1, k2, k3],
                        });
                    }
                    _ => {}
                }
            }
        }
        if !entry.model.is_empty() {
            db.lenses.push(entry);
        }
    }
    Ok(db)
}

/// 双线性采样 + 边缘复制（与 `develop::spatial::remap_replicate` 同一口径，
/// 这里要按通道走不同的缩放，单独写一份）。
/// 双线性采样单个通道。畸变重采样逐通道调用时，绝不共用一个通道——
/// 早先这里只取每像素的第 0 个分量，红通道被当成三个通道，整幅图会褪成暖灰。
fn sample(src: &[f32], w: usize, h: usize, x: f64, y: f64, ch: usize) -> f32 {
    let x0 = x.floor() as isize;
    let y0 = y.floor() as isize;
    let dx = (x - x0 as f64) as f32;
    let dy = (y - y0 as f64) as f32;
    let cx0 = x0.clamp(0, w as isize - 1) as usize;
    let cx1 = (x0 + 1).clamp(0, w as isize - 1) as usize;
    let cy0 = y0.clamp(0, h as isize - 1) as usize;
    let cy1 = (y0 + 1).clamp(0, h as isize - 1) as usize;
    let w00 = (1.0 - dx) * (1.0 - dy);
    let w10 = dx * (1.0 - dy);
    let w01 = (1.0 - dx) * dy;
    let w11 = dx * dy;
    src[(cy0 * w + cx0) * 3 + ch] * w00
        + src[(cy0 * w + cx1) * 3 + ch] * w10
        + src[(cy1 * w + cx0) * 3 + ch] * w01
        + src[(cy1 * w + cx1) * 3 + ch] * w11
}

// ---------------------------------------------------------------------------
// 测试：内嵌样本库覆盖解析、匹配、插值与应用
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<lensdatabase version="1">
  <!-- 注释与实体都要活下来 -->
  <camera>
    <maker>Nikon</maker>
    <model>Z 8</model>
    <cropfactor>1.0</cropfactor>
  </camera>
  <lens>
    <maker>Nikon</maker>
    <model>NIKKOR Z 24-70mm f/2.8 S</model>
    <mount>Nikon Z</mount>
    <cropfactor>1.0</cropfactor>
    <calibration>
      <distortion model="ptlens" focal="24" a="0.012" b="-0.031" c="0.018"/>
      <distortion model="ptlens" focal="70" a="0.001" b="-0.002" c="0.001"/>
      <tca model="poly3" focal="24" br="-0.00021" vr="1.00031" bb="-0.00018" vb="1.00022"/>
      <tca model="poly3" focal="70" br="-0.00005" vr="1.00005" bb="-0.00004" vb="1.00003"/>
      <vignetting model="pa" focal="24" aperture="2.8" distance="10" k1="-0.39" k2="0.35" k3="-0.12"/>
      <vignetting model="pa" focal="24" aperture="8" distance="10" k1="-0.05" k2="0.02" k3="-0.01"/>
    </calibration>
  </lens>
  <lens>
    <maker>Nikon</maker>
    <model>没有标定的老镜头</model>
  </lens>
</lensdatabase>
"#;

    fn fixture_db() -> LensDb {
        parse_db(FIXTURE).expect("样本库要能解析")
    }

    #[test]
    fn xml_parser_handles_declarations_comments_and_self_closing() {
        let root = parse_xml(FIXTURE).expect("样本要能解析");
        assert_eq!(root.name, "lensdatabase");
        assert_eq!(root.attr("version"), Some("1"));
        let cam = root.child("camera").expect("有 camera");
        assert_eq!(cam.text_of("maker").as_deref(), Some("Nikon"));
        let lens = root.child("lens").expect("有镜头");
        let calib = lens.child("calibration").expect("有标定");
        let dist = &calib.children[0];
        assert_eq!(dist.name, "distortion");
        assert_eq!(dist.attr("model"), Some("ptlens"));
        assert_eq!(dist.attr("a"), Some("0.012"));
        assert_eq!(decode_entities("a &amp; b &#65;"), "a & b A");
        assert_eq!(decode_entities("悬空 & 尾"), "悬空 & 尾");
    }

    #[test]
    fn parse_db_collects_cameras_and_lenses() {
        let db = fixture_db();
        assert_eq!(db.cameras.len(), 1);
        assert_eq!(db.cameras[0].model, "Z 8");
        assert_eq!(db.cameras[0].crop, Some(1.0));
        assert_eq!(db.lenses.len(), 2);
        let lens = &db.lenses[0];
        assert_eq!(lens.model, "NIKKOR Z 24-70mm f/2.8 S");
        assert_eq!(lens.dist.len(), 2);
        assert_eq!(lens.dist[0].model, DistModel::Ptlens);
        assert_eq!(lens.dist[0].k, [0.012, -0.031, 0.018]);
        assert_eq!(lens.tca[0].k, [-0.00021, 1.00031, -0.00018, 1.00022]);
        assert_eq!(lens.vig.len(), 2);
        assert_eq!(lens.vig[0].aperture, 2.8);
        // 没有标定的镜头也入库（三段全空，匹配得上但折不出校正）。
        assert!(db.lenses[1].dist.is_empty());
        // 空文件表不出事。
        let (db2, ok) = load_files(&[]);
        assert_eq!(ok, 0);
        assert!(db2.lenses.is_empty());
    }

    #[test]
    fn matching_tolerates_case_punctuation_and_maker_prefixes() {
        let db = fixture_db();
        let lens =
            find_lens(&db, "NIKON CORPORATION", "NIKKOR Z 24-70mm f/2.8 S").expect("全名要匹配");
        assert_eq!(lens.model, "NIKKOR Z 24-70mm f/2.8 S");
        // EXIF 只给焦段子串也行。
        let sub = find_lens(&db, "Nikon", "24-70mm f/2.8").expect("子串要匹配");
        assert_eq!(sub.model, "NIKKOR Z 24-70mm f/2.8 S");
        let cam = find_camera(&db, "NIKON CORPORATION", "NIKON Z 8").expect("机身要匹配");
        assert_eq!(cam.model, "Z 8");
        assert!(find_lens(&db, "Canon", "RF 24-70mm").is_none());
        assert!(find_camera(&db, "Sony", "A7 IV").is_none());
    }

    #[test]
    fn correction_interpolates_focal_and_clamps_beyond_range() {
        let db = fixture_db();
        let lens = find_lens(&db, "Nikon", "NIKKOR Z 24-70mm").unwrap();
        let mid = correction_for(lens, 47.0, Some(2.8), Some(1.0)).expect("有标定");
        let (_, k) = mid.dist.expect("畸变有标定");
        // 24 与 70 的中点插值。
        assert!((k[0] - (0.012 + 0.001) / 2.0).abs() < 1e-9);
        assert!((k[1] - (-0.031 - 0.002) / 2.0).abs() < 1e-9);
        assert!((k[2] - (0.018 + 0.001) / 2.0).abs() < 1e-9);
        // TCA 同焦距插值。
        let tca = mid.tca.expect("色差有标定");
        assert!((tca[0] - (-0.00021 - 0.00005) / 2.0).abs() < 1e-9);
        // 超出标定范围时夹到端点。
        let wide = correction_for(lens, 20.0, Some(2.8), None).unwrap();
        assert_eq!(wide.dist.unwrap().1[0], 0.012);
        assert_eq!(wide.focal_mm, 20.0);
        // 机身裁切缺省回退镜头标定，再缺省回退全幅。
        assert!((wide.crop - 1.0).abs() < 1e-9);
    }

    #[test]
    fn correction_picks_aperture_and_falls_back() {
        let db = fixture_db();
        let lens = find_lens(&db, "Nikon", "NIKKOR Z 24-70mm").unwrap();
        let f28 = correction_for(lens, 24.0, Some(2.8), None).unwrap();
        assert_eq!(f28.vig.unwrap()[0], -0.39);
        let f8 = correction_for(lens, 24.0, Some(8.0), None).unwrap();
        assert_eq!(f8.vig.unwrap()[0], -0.05);
        // 光圈未知取最亮一档（f 数最小、暗角最强、最值得修的那档）。
        let none = correction_for(lens, 24.0, None, None).unwrap();
        assert_eq!(none.vig.unwrap()[0], -0.39);
        // 没有标定的镜头折不出校正（CJK 型号不参与匹配，按索引取）。
        let bare = &db.lenses[1];
        assert!(correction_for(bare, 50.0, None, None).is_none());
        // 焦距未知回退到标定范围端点：这里唯一点是 24。
        let fallback = correction_for(&db.lenses[0], 0.0, Some(2.8), None).unwrap();
        assert!((fallback.focal_mm - 24.0).abs() < 1e-9);
    }

    #[test]
    fn match_correction_uses_camera_crop_and_reports_miss() {
        let db = fixture_db();
        let hit = match_correction(
            &db,
            "NIKON CORPORATION",
            "NIKON Z 8",
            "NIKKOR Z 24-70mm f/2.8 S",
            47.0,
            Some(2.8),
        );
        assert!(hit.is_some());
        let miss = match_correction(&db, "NIKON CORPORATION", "NIKON Z 8", "18-55mm", 18.0, None);
        assert!(miss.is_none());
    }

    #[test]
    fn apply_noop_when_correction_empty() {
        let mut a = vec![0.5f32; 7 * 5 * 3];
        let b = a.clone();
        apply(&mut a, 7, 5, &Correction::default());
        assert_eq!(a, b);
    }

    #[test]
    fn apply_vignetting_matches_formula() {
        // 全白图 + 纯暗角：中心不动，指定像素按 v = 1 + k1·r² 走。
        let w = 201;
        let h = 201;
        let mut px = vec![1.0f32; w * h * 3];
        let c = Correction {
            vig: Some([-0.5, 0.0, 0.0]),
            focal_mm: 24.0,
            crop: 1.0,
            ..Correction::default()
        };
        apply(&mut px, w, h, &c);
        let at = |x: usize, y: usize| px[(y * w + x) * 3];
        assert!((at(100, 100) - 1.0).abs() < 1e-6);
        // k_norm = 43.2666/(hypot(200,200)*24)，(150,100) 处离中心 50px。
        let k_norm = 43.2666153005 / (200f64.hypot(200.0) * 24.0);
        let r2 = (50.0 * k_norm).powi(2);
        let expect = 1.0 - 0.5 * r2;
        assert!((at(150, 100) - expect as f32).abs() < 1e-4);
        // 径向对称。
        assert!((at(150, 100) - at(50, 100)).abs() < 1e-6);
        assert!((at(150, 100) - at(100, 150)).abs() < 1e-6);
    }

    #[test]
    fn apply_distortion_follows_newton_solution() {
        // 水平梯度图（值 = x/200）：校正后某像素的值 = 采样位置的 x/200。
        let w = 201;
        let h = 201;
        let mut px = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = (x as f32 / 200.0).clamp(0.0, 1.0);
                px[(y * w + x) * 3] = v;
                px[(y * w + x) * 3 + 1] = v;
                px[(y * w + x) * 3 + 2] = v;
            }
        }
        let c = Correction {
            dist: Some((DistModel::Poly3, [-0.2, 0.0, 0.0])),
            focal_mm: 24.0,
            crop: 1.0,
            ..Correction::default()
        };
        apply(&mut px, w, h, &c);
        // 独立按公式解 Ru（f64 牛顿，跑到收敛），期望采样位置。
        let k_norm = 43.2666153005 / (200f64.hypot(200.0) * 24.0);
        let rd = 50.0 * k_norm;
        let mut ru = rd;
        for _ in 0..24 {
            let f = ru * (1.0 - 0.2 * ru * ru) - rd;
            let d = (1.0 - 0.2 * ru * ru) + ru * (-0.4 * ru);
            ru -= f / d;
        }
        let expect_x = 100.0 + 50.0 * (ru / rd);
        let got = px[(100 * w + 150) * 3];
        assert!(
            (got - (expect_x / 200.0) as f32).abs() < 2e-3,
            "got {got} want {}",
            expect_x / 200.0
        );
        // 中心不动。
        assert!((px[(100 * w + 100) * 3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn apply_tca_scales_red_blue_radially() {
        let w = 201;
        let h = 201;
        let mut px = vec![0.0f32; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = (x as f32 / 200.0).clamp(0.0, 1.0);
                px[(y * w + x) * 3] = v;
                px[(y * w + x) * 3 + 1] = v;
                px[(y * w + x) * 3 + 2] = v;
            }
        }
        let c = Correction {
            tca: Some([-0.01, 1.0, 0.02, 1.0]),
            focal_mm: 24.0,
            crop: 1.0,
            ..Correction::default()
        };
        apply(&mut px, w, h, &c);
        let k_norm = 43.2666153005 / (200f64.hypot(200.0) * 24.0);
        let r2 = (50.0 * k_norm).powi(2);
        // 无畸变时各通道按 f_ch 直接缩放采样半径。
        let red = px[(100 * w + 150) * 3];
        let green = px[(100 * w + 150) * 3 + 1];
        let blue = px[(100 * w + 150) * 3 + 2];
        assert!((green - 0.75).abs() < 1e-6);
        let want_red = (100.0 + 50.0 * (1.0 - 0.01 * r2)) / 200.0;
        let want_blue = (100.0 + 50.0 * (1.0 + 0.02 * r2)) / 200.0;
        assert!(
            (red - want_red as f32).abs() < 1e-4,
            "red {red} want {want_red}"
        );
        assert!((blue - want_blue as f32).abs() < 1e-4);
        // 绿通道不参与色差缩放。
        assert!((green - blue).abs() > 1e-4);
    }
}
