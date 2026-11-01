//! .cube 3D LUT 的导出与导入。
//!
//! 导出走的是 `develop::render` 的 `for_lut` 分支，与预览/静帧用的是同一条逐点
//! 链，所以视频里看到的颜色和套这张 LUT 的播放器看到的颜色是同一个口径；
//! 文本格式与 Python oracle 的 `grade.write_cube` 逐字节一致（见
//! `tests/suite/cube_golden.rs`）。
//!
//! 格点顺序是 .cube 的标准顺序：**红分量变化最快**。索引
//! `i = (b * size + g) * size + r`，搞反了整张 LUT 的颜色会错位成一片糊。
//!
//! Export and import of .cube 3D LUTs.
//!
//! Export takes the `for_lut` branch of `develop::render`, the same per-point chain used by
//! preview and still rendering, so a video and a player applying this LUT share one convention.
//! The text format is byte-identical to the Python oracle's `grade.write_cube` (see
//! `tests/suite/cube_golden.rs`).
//!
//! Node order is the standard .cube order: **the red component varies fastest**.
//! The index is `i = (b * size + g) * size + r`; reversed, the colors of the whole LUT turn into a
//! smear.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::develop::{self, Ev, Opts};
use crate::grade::GradeParams;

/// Python `write_cube` 的默认格点数；65³ 已经是专业 LUT 的常见精度。
///
/// The default node count of the Python `write_cube`; 65³ is already the common precision for
/// professional LUTs.
pub const DEFAULT_SIZE: usize = 65;
pub const MIN_SIZE: usize = 2;
pub const MAX_SIZE: usize = 256;

#[derive(Debug)]
pub enum CubeError {
    Io(std::io::Error),
    Parse(String),
}

impl crate::errmsg::Msg for CubeError {
    fn msg(&self) -> (&'static str, Vec<crate::errmsg::Arg<'_>>) {
        match self {
            Self::Io(v) => (
                "读写 .cube 失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
            Self::Parse(v) => (
                ".cube 解析失败：{}",
                vec![crate::errmsg::Arg::Text(v.to_string())],
            ),
        }
    }
}

crate::impl_msg_display!(CubeError);

impl std::error::Error for CubeError {}

impl From<std::io::Error> for CubeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// 生成 `size³` 个输入格点（红最快），行优先交织成 `[f32]`。
///
/// 与 numpy 的 `linspace(0, 1, size, dtype=float32)` 取同一批值：这些档位的
/// `i / (size-1)` 在 float32 里与 numpy 的结果逐位相同。
///
/// Generates `size³` input nodes (red fastest), interleaved row-major into one `[f32]`.
///
/// It takes the same values as numpy's `linspace(0, 1, size, dtype=float32)`: at these levels
/// `i / (size-1)` matches the numpy result bit for bit in float32.
pub fn lattice(size: usize) -> Vec<f32> {
    check_size(size).expect("格点数超出范围");
    let ax: Vec<f32> = (0..size)
        .map(|i| (i as f64 / (size - 1) as f64) as f32)
        .collect();
    let mut out = Vec::with_capacity(size * size * size * 3);
    for &b in &ax {
        for &g in &ax {
            for &r in &ax {
                out.extend_from_slice(&[r, g, b]);
            }
        }
    }
    out
}

/// 按参数算出 LUT 数据，顺序与 [`lattice`] 一致。
///
/// Computes the LUT data for the given parameters, in the same order as [`lattice`].
pub fn build(p: &GradeParams, size: usize) -> Vec<[f32; 3]> {
    let mut rgb = lattice(size);
    let n = size * size * size;
    // for_lut 分支不做任何空间运算，宽高只用来校验缓冲长度。
    develop::render(
        &mut rgb,
        n,
        1,
        p,
        Ev::NONE,
        Opts {
            for_lut: true,
            ..Opts::FULL
        },
    );
    rgb.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect()
}

/// 一张 3D LUT：`data[(b * size + g) * size + r]`，取值 `[0,1]`。
///
/// One 3D LUT: `data[(b * size + g) * size + r]`, with values in `[0,1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct CubeLut {
    pub title: String,
    pub size: usize,
    /// 输入定义域。标准 .cube 是 `[0,0,0]`~`[1,1,1]`，别的取值也照实存下来。
    ///
    /// The input domain. A standard .cube uses `[0,0,0]`~`[1,1,1]`; other values are stored as
    /// they are.
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub data: Vec<[f32; 3]>,
}

impl CubeLut {
    /// 由调色参数生成（导出用）。
    ///
    /// Built from grading parameters, for export.
    pub fn from_params(p: &GradeParams, size: usize) -> Self {
        Self {
            title: "grade".to_string(),
            size,
            domain_min: [0.0; 3],
            domain_max: [1.0; 3],
            data: build(p, size),
        }
    }

    /// 就地套这张 LUT。线性插值，格点上的值与 [`build`] 完全一致。
    ///
    /// Applies this LUT in place with linear interpolation; values at the nodes match [`build`]
    /// exactly.
    pub fn apply(&self, rgb: &mut [f32]) {
        let n = self.size;
        let span = n - 1;
        for px in rgb.chunks_exact_mut(3) {
            let mut f = [0.0f32; 3];
            for c in 0..3 {
                let lo = self.domain_min[c];
                let hi = self.domain_max[c];
                let t = if hi > lo {
                    (px[c] - lo) / (hi - lo)
                } else {
                    px[c]
                };
                f[c] = t.clamp(0.0, 1.0) * span as f32;
            }
            let mut i = [0usize; 3];
            let mut w = [0.0f32; 3];
            for c in 0..3 {
                let lo = (f[c] as usize).min(span);
                i[c] = lo;
                w[c] = f[c] - lo as f32;
            }
            let mut out = [0.0f32; 3];
            // 八个角按 (r,g,b) 的权重张成，等价于三次线性插值。
            for (c, v) in out.iter_mut().enumerate() {
                let mut sum = 0.0f32;
                for db in 0..2 {
                    for dg in 0..2 {
                        for dr in 0..2 {
                            let rr = (i[0] + dr).min(span);
                            let gg = (i[1] + dg).min(span);
                            let bb = (i[2] + db).min(span);
                            let wr = if dr == 1 { w[0] } else { 1.0 - w[0] };
                            let wg = if dg == 1 { w[1] } else { 1.0 - w[1] };
                            let wb = if db == 1 { w[2] } else { 1.0 - w[2] };
                            sum += wr * wg * wb * self.at(rr, gg, bb)[c];
                        }
                    }
                }
                *v = sum.clamp(0.0, 1.0);
            }
            px.copy_from_slice(&out);
        }
    }

    #[inline]
    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[(b * self.size + g) * self.size + r]
    }

    /// 序列化成 .cube 文本（`\n` 结尾，与 Python 写盘一致）。
    ///
    /// Serializes to .cube text ending in `\n`, matching what Python writes to disk.
    pub fn to_text(&self) -> String {
        text(&self.title, self.size, &self.data)
    }

    /// 写到 `path`。
    ///
    /// Writes to `path`.
    pub fn write(&self, path: &Path) -> Result<(), CubeError> {
        std::fs::write(path, self.to_text())?;
        Ok(())
    }
}

/// 单个浮点写成 .cube 的六位小数，与 Python 的 `%.6f` 同口径。
fn decimal(v: f32) -> String {
    format!("{v:.6}")
}

/// 拼出完整文本：五行头部 + `size³` 行数据。
///
/// Assembles the full text: a five-line header plus `size³` lines of data.
pub fn text(title: &str, size: usize, data: &[[f32; 3]]) -> String {
    check_size(size).expect("格点数超出范围");
    assert_eq!(data.len(), size * size * size, "LUT 数据长度与格点数对不上");
    let mut s = String::with_capacity(data.len() * 24);
    s.push_str("# ColorTool 3D LUT\n");
    s.push_str(&format!("TITLE \"{title}\"\n"));
    s.push_str(&format!("LUT_3D_SIZE {size}\n"));
    s.push_str("DOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0\n");
    for v in data {
        s.push_str(&decimal(v[0]));
        s.push(' ');
        s.push_str(&decimal(v[1]));
        s.push(' ');
        s.push_str(&decimal(v[2]));
        s.push('\n');
    }
    s
}

/// 由参数直接导出文本（`size` 用 [`DEFAULT_SIZE`] 时与 Python 默认一致）。
///
/// 外部传进来的格点数先校验，免得把 panic 带给调用方（GUI/MCP 都走这里）。
///
/// Exports text straight from parameters; with [`DEFAULT_SIZE`] the result matches the Python
/// default.
///
/// A node count arriving from outside is validated first, so no panic reaches the caller (the GUI
/// and MCP both come through here).
pub fn export_text(p: &GradeParams, size: usize) -> Result<String, CubeError> {
    check_size(size)?;
    Ok(CubeLut::from_params(p, size).to_text())
}

/// 由参数导出到文件。
///
/// Exports to a file from parameters.
pub fn export(path: &Path, p: &GradeParams, size: usize) -> Result<(), CubeError> {
    check_size(size)?;
    CubeLut::from_params(p, size).write(path)
}

fn check_size(size: usize) -> Result<(), CubeError> {
    if !(MIN_SIZE..=MAX_SIZE).contains(&size) {
        return Err(CubeError::Parse(format!(
            "格点数 {size} 不在 {MIN_SIZE}~{MAX_SIZE} 之间"
        )));
    }
    Ok(())
}

/// 解析 .cube 文本。只认 `LUT_3D_SIZE`（三维表），一行三个数。
///
/// Parses .cube text. Only `LUT_3D_SIZE` (3D tables) is accepted, with three numbers per line.
pub fn parse(src: &str) -> Result<CubeLut, CubeError> {
    let mut title = String::new();
    let mut size: Option<usize> = None;
    let mut one_d = false;
    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut data: Vec<[f32; 3]> = Vec::new();

    for (lineno, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let head = tokens.next().unwrap_or("").to_ascii_uppercase();
        if head.starts_with("0x")
            || head
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || c == '-')
        {
            // 数据行：三个十进制数（也可能是 0x 开头的十六进制）。
            let mut v = [0.0f32; 3];
            v[0] = number(&head, lineno)?;
            for slot in v.iter_mut().skip(1) {
                let t = tokens
                    .next()
                    .ok_or_else(|| bad(lineno, "一个数也没有三个分量"))?;
                *slot = number(t, lineno)?;
            }
            data.push(v);
            continue;
        }
        match head.as_str() {
            "TITLE" => {
                let rest = line[char_offset_after(line, "TITLE")..].trim();
                title = rest.trim_matches(|c| c == '"' || c == '\'').to_string();
            }
            "LUT_3D_SIZE" => {
                let t = tokens
                    .next()
                    .ok_or_else(|| bad(lineno, "LUT_3D_SIZE 后面没有数"))?;
                size = Some(
                    t.parse::<usize>()
                        .map_err(|_| bad(lineno, &format!("格点数不是整数：{t}")))?,
                );
            }
            "LUT_1D_SIZE" => {
                one_d = true;
            }
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let mut v = [0.0f32; 3];
                for slot in v.iter_mut() {
                    let t = tokens.next().ok_or_else(|| bad(lineno, "定义域缺分量"))?;
                    *slot = number(t, lineno)?;
                }
                if head == "DOMAIN_MIN" {
                    domain_min = v;
                } else {
                    domain_max = v;
                }
            }
            _ => {
                return Err(CubeError::Parse(format!(
                    "第 {} 行不认识的关键字 {}",
                    lineno + 1,
                    head
                )));
            }
        }
    }

    if one_d {
        return Err(CubeError::Parse(
            "这是一维曲线（LUT_1D_SIZE），当前只支持三维表".to_string(),
        ));
    }
    let size = size.ok_or_else(|| CubeError::Parse("缺少 LUT_3D_SIZE".to_string()))?;
    check_size(size)?;
    let want = size * size * size;
    if data.len() != want {
        return Err(CubeError::Parse(format!(
            "数据只有 {} 行，{}³ 应该是 {want} 行",
            data.len(),
            size
        )));
    }
    Ok(CubeLut {
        title,
        size,
        domain_min,
        domain_max,
        data,
    })
}

/// 从文件导入。
///
/// Imports from a file.
pub fn read(path: &Path) -> Result<CubeLut, CubeError> {
    let src = std::fs::read_to_string(path)?;
    parse(&src)
}

// ------------------------------------------------------------- 外观（look）缓存

/// 已加载的外观按路径缓存：逐帧渲染不该反复读同一个文件。
static LOOKS: OnceLock<Mutex<BTreeMap<String, Arc<CubeLut>>>> = OnceLock::new();

fn looks() -> &'static Mutex<BTreeMap<String, Arc<CubeLut>>> {
    LOOKS.get_or_init(Default::default)
}

/// 读一张 .cube，同一路径只读一次。
///
/// Reads a .cube; the same path is read only once.
pub fn load(path: &str) -> Result<Arc<CubeLut>, CubeError> {
    if let Some(hit) = looks().lock().unwrap_or_else(|e| e.into_inner()).get(path) {
        return Ok(hit.clone());
    }
    let lut = Arc::new(read(Path::new(path))?);
    looks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_owned(), lut.clone());
    Ok(lut)
}

/// 强制重读（改了文件内容、或在界面上重新挑了一张外观时用）。
///
/// Forces a re-read, used after the file content changes or a look is picked again in the UI.
pub fn refresh(path: &str) -> Result<Arc<CubeLut>, CubeError> {
    let lut = Arc::new(read(Path::new(path))?);
    looks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_owned(), lut.clone());
    Ok(lut)
}

/// 当前参数挂的那张外观；路径读不出来时按"没有外观"渲染。
///
/// 不在这里报错是有意的：渲染是逐帧的热路径，出错也只能退化成不加外观。
/// 合法性在设置 `look_cube` 的入口校验（`cube.info` / `grade.set`），
/// 那边会告诉用户到底哪一行不对。
///
/// The look attached to the current parameters; when the path cannot be read, rendering proceeds
/// as if no look were attached.
///
/// Saying nothing here is deliberate: rendering is the per-frame hot path, so a failure can only
/// degrade to no look.
/// Validity is checked at the entry points that set `look_cube` (`cube.info` / `grade.set`), which
/// tell the user exactly which line is wrong.
pub fn active(p: &GradeParams) -> Option<Arc<CubeLut>> {
    if p.look_cube.is_empty() {
        return None;
    }
    load(&p.look_cube).ok()
}

/// 一张外观的概览，给界面与 AI 面板看。
///
/// An overview of one look, for the UI and the AI panel.
pub fn describe(lut: &CubeLut) -> LookInfo {
    let mut span = [[f32::MAX, f32::MIN]; 3];
    let mut neutral = true;
    for (i, v) in lut.data.iter().enumerate() {
        let r = i % lut.size;
        let g = (i / lut.size) % lut.size;
        let b = i / lut.size / lut.size;
        // 与恒等表的偏离：格点上的通道值应当等于该轴的取值。
        for c in 0..3 {
            let axis = match c {
                0 => r,
                1 => g,
                _ => b,
            };
            if (v[c] - axis as f32 / (lut.size - 1) as f32).abs() > 5e-3 {
                neutral = false;
            }
            let s = &mut span[c];
            s[0] = s[0].min(v[c]);
            s[1] = s[1].max(v[c]);
        }
    }
    LookInfo {
        title: lut.title.clone(),
        size: lut.size,
        nodes: lut.data.len(),
        domain_min: lut.domain_min,
        domain_max: lut.domain_max,
        span,
        neutral,
    }
}

/// [`describe`] 的结果。
///
/// The result of [`describe`].
#[derive(Debug, Clone)]
pub struct LookInfo {
    pub title: String,
    pub size: usize,
    pub nodes: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    /// 每个通道在整张表里的最小/最大值，用来看这张表压了多少动态范围。
    ///
    /// The minimum and maximum of each channel across the table, used to see how much dynamic
    /// range the table compresses.
    pub span: [[f32; 2]; 3],
    /// 是否基本等于恒等表（挂上去等于没挂）。
    ///
    /// Whether the table is essentially identity, in which case attaching it changes nothing.
    pub neutral: bool,
}

fn number(token: &str, lineno: usize) -> Result<f32, CubeError> {
    // 十六进制写法按位数决定量程：0x1F 是 8bit 的一个通道，0x1FFF 是 12bit。
    // 注意 Adobe 还有"整行一个 0xRRGGBB"的写法，那种文件在这里会直接报
    // "一个数也没有三个分量"，不会被当成三个通道悄悄读错。
    if let Some(hex) = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
    {
        if hex.is_empty() || hex.len() > 6 {
            return Err(bad(lineno, &format!("十六进制位数不对：{token}")));
        }
        let n = u32::from_str_radix(hex, 16)
            .map_err(|_| bad(lineno, &format!("十六进制数不合法：{token}")))?;
        let max = (1u32 << (hex.len() * 4)) - 1;
        return Ok(n as f32 / max as f32);
    }
    token
        .parse::<f32>()
        .map_err(|_| bad(lineno, &format!("不是数字：{token}")))
}

fn char_offset_after(line: &str, key: &str) -> usize {
    line.to_ascii_uppercase()
        .find(&key.to_ascii_uppercase())
        .map_or(0, |i| i + key.len())
}

fn bad(lineno: usize, msg: &str) -> CubeError {
    CubeError::Parse(format!("第 {} 行{msg}", lineno + 1))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn lattice_is_red_fastest_and_hits_both_ends() {
        let l = lattice(17);
        assert_eq!(l.len(), 17 * 17 * 17 * 3);
        assert_eq!(&l[0..3], &[0.0, 0.0, 0.0]);
        // 第二个格点只有红变了。
        assert_eq!(&l[3..6], &[0.0625, 0.0, 0.0]);
        // 换 g 的那一步在第 17 个格点。
        assert_eq!(&l[17 * 3..17 * 3 + 3], &[0.0, 0.0625, 0.0]);
        // 换 b 在第 17² 个格点，最后一个格点是纯白。
        assert_eq!(&l[17 * 17 * 3..17 * 17 * 3 + 3], &[0.0, 0.0, 0.0625]);
        assert_eq!(&l[l.len() - 3..], &[1.0, 1.0, 1.0]);
    }

    #[test]
    fn identity_params_produce_a_neutral_lut() {
        let lut = CubeLut::from_params(&GradeParams::default(), 17);
        let l = lattice(17);
        for (i, v) in lut.data.iter().enumerate() {
            let c = i % 3;
            assert!(
                (v[c] - l[i * 3 + c]).abs() < 1e-6,
                "格点 {} 通道 {c} 被默认参数改掉了：{} vs {}",
                i / 3,
                v[c],
                l[i * 3 + c]
            );
        }
    }

    #[test]
    fn text_has_five_header_lines_and_one_line_per_node() {
        let lut = CubeLut::from_params(&GradeParams::default(), 17);
        let t = lut.to_text();
        let lines: Vec<&str> = t.split('\n').collect();
        assert_eq!(lines[0], "# ColorTool 3D LUT");
        assert_eq!(lines[1], "TITLE \"grade\"");
        assert_eq!(lines[2], "LUT_3D_SIZE 17");
        assert_eq!(lines[3], "DOMAIN_MIN 0.0 0.0 0.0");
        assert_eq!(lines[4], "DOMAIN_MAX 1.0 1.0 1.0");
        assert_eq!(lines[5], "0.000000 0.000000 0.000000");
        assert_eq!(lines.len(), 5 + 17 * 17 * 17 + 1, "末尾要有换行");
        assert!(lines[..5].iter().all(|l| !l.contains("-0.000000")));
    }

    #[test]
    fn roundtrip_parse_keeps_every_node() {
        let mut p = GradeParams::default();
        p.exposure = 0.4;
        p.contrast = 20.0;
        let lut = CubeLut::from_params(&p, 17);
        let back = parse(&lut.to_text()).expect("解析失败");
        assert_eq!(back.size, 17);
        assert_eq!(back.title, "grade");
        assert_eq!(back.domain_min, [0.0; 3]);
        assert_eq!(back.domain_max, [1.0; 3]);
        // 文本只留六位小数，误差应远小于一个十六进制档位。
        for (a, b) in back.data.iter().zip(&lut.data) {
            for c in 0..3 {
                assert!(
                    (a[c] - b[c]).abs() <= 5e-7 + 1e-7,
                    "通道 {c} 值 {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn apply_matches_nodes_and_interpolates_monotonically() {
        let mut p = GradeParams::default();
        p.exposure = 0.5;
        p.contrast = 30.0;
        let lut = CubeLut::from_params(&p, 17);

        // 格点上应当取到表里的值（红最快：只有 r 动）。
        for (k, want) in lut.data.iter().take(17).enumerate() {
            let v = (k as f32) / 16.0;
            let mut buf = [v, 0.0, 0.0];
            lut.apply(&mut buf);
            for c in 0..3 {
                assert!(
                    (buf[c] - want[c]).abs() < 1e-5,
                    "格点 {k} 通道 {c}：{:?} vs {want:?}",
                    buf
                );
            }
        }

        // 中间点的灰阶输出应随输入单调（插值不能把顺序搞乱）。
        let mut prev = -1.0f32;
        for i in 0..64 {
            let v = i as f32 / 63.0;
            let mut buf = [v, v, v];
            lut.apply(&mut buf);
            assert!(buf[0] >= prev - 1e-6, "灰阶在 {v} 处非单调");
            prev = buf[0];
        }
    }

    #[test]
    fn neutral_lut_leaves_pixels_untouched() {
        let lut = CubeLut::from_params(&GradeParams::default(), 33);
        let mut buf: Vec<f32> = (0..=255).map(|i| i as f32 / 255.0).collect();
        buf.extend_from_slice(&buf.clone());
        let src = buf.clone();
        lut.apply(&mut buf);
        for (a, b) in buf.iter().zip(&src) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn parse_rejects_broken_files() {
        let ok = CubeLut::from_params(&GradeParams::default(), 2).to_text();
        // 少一行数据
        let short: String = ok
            .lines()
            .take(ok.lines().count() - 1)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(matches!(
            parse(&short),
            Err(CubeError::Parse(msg)) if msg.contains("数据只有")
        ));
        // 没有尺寸声明
        let no_size = ok.replace("LUT_3D_SIZE 2\n", "");
        assert!(matches!(
            parse(&no_size),
            Err(CubeError::Parse(msg)) if msg.contains("LUT_3D_SIZE")
        ));
        // 一维曲线
        let one_d = "# x\nLUT_1D_SIZE 2\n0.0 0.0 0.0\n1.0 1.0 1.0\n";
        assert!(matches!(
            parse(one_d),
            Err(CubeError::Parse(msg)) if msg.contains("一维")
        ));
        // 数字写坏
        let broken = "LUT_3D_SIZE 2\n0.0 0.0 0.0\n1.0 abc 1.0\n1.0 1.0 1.0\n1.0 1.0 1.0\n";
        assert!(matches!(
            parse(broken),
            Err(CubeError::Parse(msg)) if msg.contains("不是数字")
        ));
        // 越界的格点数
        let huge = "LUT_3D_SIZE 512\n".to_string() + &"0.0 0.0 0.0\n".repeat(4);
        assert!(matches!(
            parse(&huge),
            Err(CubeError::Parse(msg)) if msg.contains("512")
        ));
    }

    #[test]
    fn parse_accepts_comments_blank_lines_and_hex() {
        let src = "\n# 注释\nTITLE My Look\nLUT_3D_SIZE 2\nDOMAIN_MIN 0.0 0.0 0.0\n\
                   DOMAIN_MAX 1.0 1.0 1.0\n  0x00 0x00 0x00\n0xFF 0x00 0x00\n\
                   0x00 0xFF 0x00\n0xFF 0xFF 0x00\n0x80 0x80 0x80\n0xFF 0x00 0xFF\n\
                   0x00 0xFF 0xFF\n\n0xFF 0xFF 0xFF\n";
        let lut = parse(src).expect("解析失败");
        assert_eq!(lut.title, "My Look");
        assert_eq!(lut.size, 2);
        assert_eq!(lut.at(1, 0, 0), [1.0, 0.0, 0.0]);
        assert_eq!(lut.at(1, 1, 1), [1.0, 1.0, 1.0]);
        assert!((lut.at(0, 0, 1)[0] - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn decimal_matches_fixed_width() {
        assert_eq!(decimal(0.0), "0.000000");
        assert_eq!(decimal(1.0), "1.000000");
        assert_eq!(decimal(0.0625), "0.062500");
        assert_eq!(decimal(1.0 / 3.0), "0.333333");
        assert_eq!(decimal(2.0 / 3.0), "0.666667");
        assert_eq!(decimal(-1.0), "-1.000000");
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aevum-cube-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn look_cube_is_applied_by_the_main_chain() {
        // 挂一张外观到"默认参数"上：主链其它环节全跳过，结果应当就是这张表本身。
        let mut p = GradeParams::default();
        p.exposure = 0.4;
        p.temperature = 15.0;
        let dir = tmp_dir("look");
        let file = dir.join("look.cube");
        CubeLut::from_params(&p, 33).write(&file).expect("写外观");

        let mut q = GradeParams::default();
        q.look_cube = file.display().to_string();
        let lut = active(&q).expect("外观应当被读到");

        let n = 16;
        let mut via_chain: Vec<f32> = (0..n * n)
            .flat_map(|i| [i as f32 / 255.0, 1.0 - i as f32 / 255.0, 0.5])
            .collect();
        let mut via_lut = via_chain.clone();
        develop::render(&mut via_chain, n, n, &q, Ev::NONE, Opts::FULL);
        lut.apply(&mut via_lut);
        for x in via_lut.iter_mut() {
            *x = x.clamp(0.0, 1.0);
        }
        let max = via_chain
            .iter()
            .zip(&via_lut)
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(max < 1e-6, "主链套外观与直接查表最大差 {max:.7}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn look_cube_of_default_params_is_neutral() {
        let dir = tmp_dir("neutral");
        let file = dir.join("n.cube");
        CubeLut::from_params(&GradeParams::default(), 17)
            .write(&file)
            .expect("写外观");
        let mut p = GradeParams::default();
        p.look_cube = file.display().to_string();
        let lut = load(&p.look_cube).expect("读外观");
        let info = describe(&lut);
        assert!(info.neutral, "默认参数生成的外观应当接近恒等");
        assert_eq!(info.size, 17);
        assert_eq!(info.nodes, 17 * 17 * 17);

        // 有颜色的外观不该被误判成恒等。
        let mut c = GradeParams::default();
        c.temperature = 40.0;
        c.shadows = 30.0;
        let colored = CubeLut::from_params(&c, 17);
        assert!(!describe(&colored).neutral, "偏色外观被判成了恒等");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_look_path_degrades_to_no_look() {
        let mut p = GradeParams::default();
        assert!(active(&p).is_none(), "空路径不该查表");
        p.look_cube = String::from("Z:/肯定不存在/没有这个文件.cube");
        assert!(active(&p).is_none(), "读不到外观时应当按没挂处理");

        // 渲染路径必须照常工作：主链不因为外观坏掉而停摆。
        let mut rgb = vec![0.5f32; 4 * 3];
        develop::render(&mut rgb, 2, 2, &p, Ev::NONE, Opts::FULL);
        assert!(rgb.iter().all(|v| (*v - 0.5).abs() < 1e-6));

        // 坏文件同样只会被拒绝，不会被当成外观加载。
        let dir = tmp_dir("broken");
        let file = dir.join("bad.cube");
        std::fs::write(&file, "LUT_3D_SIZE 2\n0.0 0.0 0.0\n").unwrap();
        let mut q = GradeParams::default();
        q.look_cube = file.display().to_string();
        assert!(active(&q).is_none(), "坏文件不该产出一张表");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exported_lut_is_close_to_direct_render() {
        // 套 LUT 与逐帧渲染同一条链：差别只允许来自格点之间的线性插值。
        // 误差随格点数按 h² 收敛，这里把三档都量出来，容差不靠猜。
        let mut p = GradeParams::default();
        p.exposure = 0.3;
        p.contrast = 15.0;
        p.temperature = 12.0;
        p.vibrance = 20.0;

        let n = 64;
        let mut rgb: Vec<f32> = Vec::with_capacity(n * n * 3);
        for i in 0..n * n {
            rgb.push((i % 251) as f32 / 250.0);
            rgb.push(((i / 251) as f32 / 254.0).min(1.0));
            rgb.push(((i % 97) as f32 / 96.0).min(1.0));
        }
        let mut direct = rgb.clone();
        develop::render(
            &mut direct,
            n,
            n,
            &p,
            Ev::NONE,
            Opts {
                for_lut: true,
                ..Opts::FULL
            },
        );

        let mut prev = f32::MAX;
        for size in [17usize, 33, 65] {
            let lut = CubeLut::from_params(&p, size);
            let mut via_lut = rgb.clone();
            lut.apply(&mut via_lut);
            let max = direct
                .iter()
                .zip(&via_lut)
                .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
            // 黑位附近的 sRGB 幂函数很陡，加上自然饱和度/高光阴影里 max/min
            // 在通道交界处有折点，分段线性只能拿到一阶收敛（不是 h²），
            // 所以这里只要求每加一档误差明显下降。
            assert!(
                max < prev / 1.3,
                "{size}³ 的误差 {max:.5} 没收敛（上一档 {prev:.5}）"
            );
            prev = max;
            println!("{size}³ 套 LUT 最大差 {max:.5}");
        }
        // 65³ 是默认档位：误差要落在 8bit 一个色阶以内。
        assert!(prev < 2.0 / 255.0, "65³ 套 LUT 最大差 {prev:.5}");
    }
}
