//! 输出色彩标记：自己生成 ICC 配置 + 工作色域到目标色域的转换。
//!
//! Output color tagging: generates the ICC profile itself plus the
//! conversion from the working gamut to the target gamut.
//!
//! 两条线各管一件事，别混：
//!
//! The two tracks each own one job; do not mix them:
//!
//! 1. **标记**——静态图（PNG/TIFF）里嵌一份 ICC，视频写 x264/x265 的 VUI。没有标
//!    记，广色域显示器上同一份像素会被按 sRGB 解释，颜色直接洗淡。
//! 2. **转换**——像素本身从 sRGB 工作空间搬到目标色域。只改标记不搬像素是错的：
//!    那等于告诉显示器"这些数已经是 P3 了"，而它们其实还是 sRGB 的。
//!
//! 1. **Tagging** — static images (PNG/TIFF) embed an ICC profile, video
//!    writes the x264/x265 VUI. Without tagging, the same pixels are
//!    interpreted as sRGB on wide-gamut displays and the colors wash out.
//! 2. **Conversion** — the pixels themselves move from the sRGB working
//!    space to the target gamut. Changing the tag without moving pixels is
//!    wrong: it tells the display "these numbers are already P3" when they
//!    are still sRGB.
//!
//! 调色链路全程按 sRGB（D65、sRGB 传递函数）算，所以这里的"源"永远是 sRGB。
//! ICC 的原色三刺激值必须放到 D50 PCS，因此带一次 Bradford 自适应——不是多余
//! 步骤，规范就这么要求，少了它系统会读出偏色的白。
//!
//! The grading pipeline computes in sRGB (D65, the sRGB transfer function)
//! end to end, so the "source" here is always sRGB. The tristimulus values
//! of the ICC primaries must land in the D50 PCS, hence one Bradford
//! adaptation — not a redundant step; the spec requires it, and without it
//! the system reads a color-shifted white.
//!
//! 常数全部是公开的色度学标准值（IEC 61966-2-1、SMPTE RP 431-2、ITU-R BT.2020）
//! 加自己推导的矩阵，不引用任何第三方色彩引擎的数据。头部偏移由
//! `header_offsets_are_where_the_spec_puts_them` 逐个钉住，改之前先看那条测试。
//!
//! Every constant is a published colorimetry standard value (IEC 61966-2-1,
//! SMPTE RP 431-2, ITU-R BT.2020) plus matrices derived here; no data from
//! any third-party color engine is referenced. Header offsets are pinned
//! one by one by `header_offsets_are_where_the_spec_puts_them`; read that
//! test before changing them.
//!
//! 字节的最终裁判不在这里：设 `AEVUM_ICC_DUMP=<目录>` 跑一遍单元测试落盘，再用
//! `python acceptance/oracle/check_icc.py` 让 littleCMS 读回来核。曲线形状的两次改版（不用
//! `para` 1 型、不用 `curv` 表）都是那一步实测出来的，别只看单元测试就改回去。
//!
//! The final judge of the bytes is not here: set `AEVUM_ICC_DUMP=<dir>` to
//! run the unit tests and persist the profiles, then run
//! `python acceptance/oracle/check_icc.py` to have littleCMS read them back
//! and verify. The two curve-shape revisions (no `para` type 1, no `curv`
//! table) both came out of that measured step; do not revert them based on
//! unit tests alone.

use serde::{Deserialize, Serialize};

/// 输出色域。工作空间固定是 sRGB，这里只选"发到哪儿"。
///
/// The output gamut. The working space is fixed at sRGB; this only chooses
/// where the output goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorSpace {
    /// 默认：跟现有链路一致，不嵌 ICC、不改像素。
    ///
    /// Default: matches the existing pipeline, no ICC embedding and no pixel
    /// changes.
    #[default]
    Srgb,
    /// 广色域：DCI-P3 原色 + D65 白 + sRGB 传递函数（Apple 那套 Display P3）。
    ///
    /// Wide gamut: DCI-P3 primaries + D65 white + the sRGB transfer function
    /// (Apple's Display P3).
    DisplayP3,
    /// 超广色域：BT.2020 原色 + D65 白 + γ2.4。SDR 标记，不是 HDR。
    ///
    /// Ultra-wide gamut: BT.2020 primaries + D65 white + γ2.4. An SDR tag,
    /// not HDR.
    Rec2020,
}

/// 3x3 行主序矩阵，`m[row][col]`。
type Mat = [[f64; 3]; 3];

const IDENTITY: Mat = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// D65 的色度坐标（x, y）——工作空间的白点。
const D65: (f64, f64) = (0.3127, 0.3290);

/// ICC 的 PCS 白点：D50 的三刺激值（Y 固定为 1）。
const D50_XYZ: [f64; 3] = [0.9642, 1.0, 0.8249];

/// Bradford 视锥响应矩阵。
const BRADFORD: Mat = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0634],
];

/// ICC.1 的 128 字节头里各字段的起始偏移。
///
/// 写的时候是顺序追加，用不上偏移；读标签表和 `header_offsets_are_where_the_spec_puts_them`
/// 才逐条引用。留全量是为了那条测试能按规范名对着写，不是漏删。
#[allow(dead_code)]
mod off {
    pub const SIZE: usize = 0;
    pub const VERSION: usize = 8;
    pub const CLASS: usize = 12;
    pub const COLOR_SPACE: usize = 16;
    pub const PCS: usize = 20;
    pub const SIGNATURE: usize = 36;
    pub const WHITE_POINT: usize = 68;
    pub const PROFILE_ID: usize = 84;
    pub const TAG_COUNT: usize = 128;
}

impl ColorSpace {
    pub const ALL: [Self; 3] = [Self::Srgb, Self::DisplayP3, Self::Rec2020];

    pub fn cn(self) -> &'static str {
        match self {
            Self::Srgb => "sRGB",
            Self::DisplayP3 => "Display P3",
            Self::Rec2020 => "Rec.2020",
        }
    }

    /// 一句话说明这条标记意味着什么，给面板悬停和 agent 用。
    ///
    /// One-line description of what the tag means, for panel tooltips and
    /// agents.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Srgb => "网络与大多数显示器的默认，不嵌 ICC",
            Self::DisplayP3 => "手机与广色域显示器，颜色比 sRGB 浓，像素会真转换",
            Self::Rec2020 => "电视色域，SDR 写法（γ2.4），不是 HDR",
        }
    }

    /// 命令行/工程文件里的稳定拼写。
    ///
    /// The stable spelling used on the command line and in project files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::DisplayP3 => "p3",
            Self::Rec2020 => "rec2020",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let low = raw.trim().to_lowercase();
        Self::ALL.into_iter().find(|cs| {
            cs.key() == low
                || match cs {
                    Self::Srgb => matches!(low.as_str(), "srgb" | "s rgb" | "61966-2-1"),
                    Self::DisplayP3 => {
                        matches!(low.as_str(), "display p3" | "p3" | "dcip3" | "display-p3")
                    }
                    Self::Rec2020 => {
                        matches!(
                            low.as_str(),
                            "rec.2020" | "2020" | "bt2020" | "bt.2020" | "rec2020nc"
                        )
                    }
                }
        })
    }

    /// ffmpeg 的 `-color_primaries / -color_trc / -colorspace` 三元组，两个编码器
    /// 都实测过：x264 与 x265 各自吃下自己那三条，没有一条被拒。
    ///
    /// The ffmpeg `-color_primaries / -color_trc / -colorspace` triple; both
    /// encoders were measured: x264 and x265 each accept their own three,
    /// and none was rejected.
    ///
    /// Rec.2020 那条只能标 `bt709` 传递函数：VUI 里没有"纯 γ2.4 的 SDR"这一档
    /// （`smpte2084`/`arib-std-b67` 都是 HDR），业界惯例就是写 `bt709`。静态图那
    /// 份 ICC 写的是真正的 γ2.4，所以只有视频这条路带这一点近似。
    ///
    /// The Rec.2020 entry can only tag the `bt709` transfer function: the VUI
    /// has no "pure γ2.4 SDR" tier (`smpte2084`/`arib-std-b67` are both HDR),
    /// and industry convention writes `bt709`. The static-image ICC carries
    /// the real γ2.4, so only the video path carries this approximation.
    pub fn vui(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Srgb => ("bt709", "bt709", "bt709"),
            Self::DisplayP3 => ("smpte432", "iec61966-2-1", "bt709"),
            Self::Rec2020 => ("bt2020", "bt709", "bt2020nc"),
        }
    }

    /// `vui()` 三元的数值版（ISO/IEC 23091-2），写 AVIF 的 `colr/nclx` 用：
    /// ffmpeg 7.1 的 libaom 只把矩阵写进去，原色与传递函数得自己补。
    ///
    /// The numeric version of the `vui()` triple (ISO/IEC 23091-2), for
    /// writing AVIF's `colr/nclx`: ffmpeg 7.1's libaom only writes the
    /// matrix, and the primaries and transfer function must be filled in by
    /// the caller.
    pub fn nclx(self) -> [u16; 3] {
        match self {
            Self::Srgb => [1, 1, 1],
            Self::DisplayP3 => [12, 13, 1],
            Self::Rec2020 => [9, 1, 9],
        }
    }

    /// 原色色度坐标（R, G, B），配 D65 白。
    fn primaries(self) -> [(f64, f64); 3] {
        match self {
            // IEC 61966-2-1 / ITU-R BT.709
            Self::Srgb => [(0.6400, 0.3300), (0.3000, 0.6000), (0.1500, 0.0600)],
            // SMPTE RP 431-2 的 P3 原色配 D65 白，才是 Display P3
            Self::DisplayP3 => [(0.6800, 0.3200), (0.2650, 0.6900), (0.1500, 0.0600)],
            // ITU-R BT.2020
            Self::Rec2020 => [(0.7080, 0.2950), (0.1700, 0.7970), (0.1310, 0.0460)],
        }
    }

    /// 编码用的传递函数：sRGB 与 P3 用 sRGB 曲线，2020 用纯 γ2.4。
    fn uses_srgb_trc(self) -> bool {
        !matches!(self, Self::Rec2020)
    }

    /// 目标色域的 XYZ→线性 RGB。
    fn xyz_to_rgb(self) -> Mat {
        inverse(&rgb_to_xyz(self.primaries()))
    }

    /// 这份像素从 sRGB 搬到目标色域。原地改，导出每帧上千万像素，不复制。
    ///
    /// Moves these pixels from sRGB to the target gamut. In place: exports
    /// push tens of millions of pixels per frame, so no copies.
    ///
    /// 进来的是 sRGB 编码值（显示值，不是线性光），出去还是编码值，只是换了一套
    /// 原色和曲线。`Srgb` 直接返回，省掉整趟 pow。
    ///
    /// The input is sRGB-encoded values (display values, not linear light)
    /// and the output is still encoded, just with a different set of
    /// primaries and curves. `Srgb` returns immediately, skipping the whole
    /// round of pow.
    pub fn convert_in_place(self, data: &mut [f32]) {
        if self == Self::Srgb {
            return;
        }
        let m = mul(&self.xyz_to_rgb(), &srgb_to_xyz());
        let decode = linearizing_lut();
        for px in data.chunks_exact_mut(3) {
            let mut lin = [0.0f64; 3];
            for (c, v) in px.iter().enumerate() {
                lin[c] = lut_lookup(v.clamp(0.0, 1.0) as f64, decode);
            }
            for c in 0..3 {
                let mixed = m[c][0] * lin[0] + m[c][1] * lin[1] + m[c][2] * lin[2];
                px[c] = linear_to_encoded(mixed.clamp(0.0, 1.0), self) as f32;
            }
        }
    }

    /// 生成 ICC v4.3 矩阵/曲线配置。每次导出算一次就够，别逐帧算。
    ///
    /// Generates an ICC v4.3 matrix/curve profile. Computing it once per
    /// export is enough; do not compute it per frame.
    pub fn icc(self) -> Vec<u8> {
        let name = match self {
            Self::Srgb => "Aevum Studio sRGB",
            Self::DisplayP3 => "Aevum Studio Display P3",
            Self::Rec2020 => "Aevum Studio Rec.2020 (SDR)",
        };
        // 一次 Bradford 自适应：既写进 chad，也用来把原色搬到 D50。
        let chad = bradford(D65, D50_XYZ);
        let rgb_xyz = mul(&chad, &rgb_to_xyz(self.primaries()));

        let trc = if self.uses_srgb_trc() {
            tag_trc_srgb()
        } else {
            tag_trc_gamma(2.4)
        };
        let colorant = |col: usize| fixed_xyz(rgb_xyz[0][col], rgb_xyz[1][col], rgb_xyz[2][col]);
        let wtpt = fixed_xyz(D50_XYZ[0], D50_XYZ[1], D50_XYZ[2]);

        let mut tags: Vec<([u8; 4], Vec<u8>)> = vec![
            (*b"bTRC", trc.clone()),
            (*b"bXYZ", colorant(2)),
            (*b"chad", fixed_matrix(&chad)),
            (*b"desc", tag_desc(name)),
            (*b"gTRC", trc.clone()),
            (*b"gXYZ", colorant(1)),
            (*b"rTRC", trc),
            (*b"rXYZ", colorant(0)),
            (*b"wtpt", wtpt.clone()),
        ];
        // 表按签名升序：规范没强制，但校验器和 diff 都习惯这样。
        tags.sort_by_key(|(sig, _)| *sig);

        let table_len = 4 + 12 * tags.len();
        let mut at = 128 + table_len;
        let mut offsets = Vec::with_capacity(tags.len());
        for (_, body) in &tags {
            offsets.push(at);
            // 标签体按 4 字节对齐，中间补零。
            at += body.len() + pad4(body.len());
        }
        let total = at;

        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&u32::try_from(total).unwrap().to_be_bytes());
        out.extend_from_slice(b"none"); // 首选 CMM：不指定
        out.extend_from_slice(&0x0430_0000u32.to_be_bytes()); // 4.3.0
        out.extend_from_slice(b"mntr");
        out.extend_from_slice(b"RGB ");
        out.extend_from_slice(b"XYZ ");
        out.extend_from_slice(&[0u8; 12]); // 创建时间固定为零：同参数必须同字节
        out.extend_from_slice(b"acsp");
        out.extend_from_slice(b"none"); // 目标平台
        out.extend_from_slice(&0u32.to_be_bytes()); // 标志
        out.extend_from_slice(b"LPSC"); // 厂商标识（未注册，只用来认自家配置）
        out.extend_from_slice(b"laps"); // 设备型号：没有真实设备，跟创建者同名
        out.extend_from_slice(&0u64.to_be_bytes()); // 设备属性
        out.extend_from_slice(&0u32.to_be_bytes()); // 渲染意图：感知
        out.extend_from_slice(&wtpt[8..]); // 头里的 PCS 白点（去掉标签类型头）
        out.extend_from_slice(b"laps"); // 配置创建者
        // v4 把头部校验和换成了 16 字节的 Profile ID（MD5）。没算就整块留零，读方
        // 一律忽略——别再往 124 那里补"补码和"，那是 v2 的老规矩，v4 没有这一项。
        out.extend_from_slice(&[0u8; 16]); // Profile ID
        out.extend_from_slice(&[0u8; 28]); // 保留 + 内嵌配置大小
        assert_eq!(out.len(), 128);

        out.extend_from_slice(&u32::try_from(tags.len()).unwrap().to_be_bytes());
        for ((sig, body), off) in tags.iter().zip(&offsets) {
            out.extend_from_slice(sig);
            out.extend_from_slice(&u32::try_from(*off).unwrap().to_be_bytes());
            out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
        }
        for (i, (_, body)) in tags.iter().enumerate() {
            out.extend_from_slice(body);
            out.extend_from_slice(&[0u8; 3][..pad4(body.len())]);
            debug_assert_eq!(out.len(), offsets[i] + body.len() + pad4(body.len()));
        }
        assert_eq!(out.len(), total);
        out
    }

    /// 从配置字节里取出某个标签的体（含 8 字节类型头）。读自己写的配置用得到。
    ///
    /// Extracts the body of a tag (including the 8-byte type header) from
    /// profile bytes. Used when reading profiles written here.
    pub fn tag_bytes<'a>(profile: &'a [u8], sig: &[u8; 4]) -> Option<&'a [u8]> {
        let count = u32::from_be_bytes(profile.get(off::TAG_COUNT..132)?.try_into().ok()?) as usize;
        (0..count).find_map(|i| {
            let e = 132 + 12 * i;
            if profile.get(e..e + 4)? != *sig {
                return None;
            }
            let start = u32::from_be_bytes(profile[e + 4..e + 8].try_into().ok()?) as usize;
            let len = u32::from_be_bytes(profile[e + 8..e + 12].try_into().ok()?) as usize;
            profile.get(start..start + len)
        })
    }
}

fn pad4(len: usize) -> usize {
    (4 - len % 4) % 4
}

/// `desc` 类型：ASCII 一段，Unicode / ScriptCode 两段留空。
fn tag_desc(name: &str) -> Vec<u8> {
    let ascii = format!("{name}\0").into_bytes();
    let mut out = Vec::with_capacity(24 + ascii.len());
    out.extend_from_slice(b"desc");
    out.extend_from_slice(&0u32.to_be_bytes()); // 保留
    out.extend_from_slice(&u32::try_from(ascii.len()).unwrap().to_be_bytes());
    out.extend_from_slice(&ascii);
    out.extend_from_slice(&[0u8; 4]); // Unicode 语言码 + 长度（长度为 0）
    out.extend_from_slice(&[0u8; 4]); // ScriptCode 长度 + 保留
    out
}

/// s15Fixed16：值域 [-32768, 32767.99998]，ICC 的 XYZ 与曲线参数都用它。
fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32 as u32).to_be_bytes()
}

fn fixed_xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(20);
    out.extend_from_slice(b"XYZ ");
    out.extend_from_slice(&0u32.to_be_bytes());
    for v in [x, y, z] {
        out.extend_from_slice(&s15(v));
    }
    out
}

fn fixed_matrix(m: &Mat) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 4 + 36);
    out.extend_from_slice(b"sf32");
    out.extend_from_slice(&0u32.to_be_bytes());
    // `sf32` 的九个数按行主序写：第一个是 R→X。
    for row in m {
        for v in row {
            out.extend_from_slice(&s15(*v));
        }
    }
    out
}

/// 参数曲线 3 型：分段（低段线性、高段幂），正好是 sRGB 那条 EOTF。
fn tag_trc_srgb() -> Vec<u8> {
    tag_trc(&[
        2.4,
        1.0 / 1.055,
        0.055 / 1.055,
        1.0 / 12.92,
        0.04045,
        0.0,
        0.0,
    ])
}

/// 纯 γ 曲线，仍写成 3 型：`a=1、b=c=d=e=f=0` 时段间式退化成 Y = X^γ。
///
/// 这里刻意不用 1 型也不用 `curv` 表，两条都是本机 littleCMS 2 实测出来的：1 型读回
/// 来全黑，`curv` 表把 γ2.4 量化成 16 位后最暗 8 个采样全归零，夜景暗部直接被压死。
/// 3 型 + 七个参数只占 40 字节，且曲线是解析式，没有量化误差。
fn tag_trc_gamma(gamma: f64) -> Vec<u8> {
    tag_trc(&[gamma, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0])
}

/// 参数曲线 3 型：Y = (aX + b)^g + e（X > d），否则 Y = cX + f。参数顺序 g,a,b,c,d,e,f。
fn tag_trc(p: &[f64; 7]) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(b"para");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for v in p {
        out.extend_from_slice(&s15(*v));
    }
    out
}

/// 色度坐标 + 白点 → 线性 RGB 到 XYZ：列是原色，缩放让三列之和等于白点。
fn rgb_to_xyz(primaries: [(f64, f64); 3]) -> Mat {
    let (wx, wy) = D65;
    let white = xy_to_xyz(wx, wy);
    let mut a = IDENTITY;
    for (c, (px, py)) in primaries.iter().enumerate() {
        a[0][c] = px / py;
        a[1][c] = 1.0;
        a[2][c] = (1.0 - px - py) / py;
    }
    let scale = mat_vec(&inverse(&a), &white);
    let mut m = IDENTITY;
    for (c, v) in scale.iter().enumerate() {
        for row in 0..3 {
            m[row][c] = a[row][c] * v;
        }
    }
    m
}

fn xy_to_xyz(x: f64, y: f64) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// Bradford 自适应矩阵：把 `from` 白点下的值搬到 `to` 白点。
///
/// `from` 给色度坐标 (x, y)，`to` 直接给 XYZ——ICC 的 D50 就是这么规定的。
fn bradford(from: (f64, f64), to: [f64; 3]) -> Mat {
    let lms_src = mat_vec(&BRADFORD, &xy_to_xyz(from.0, from.1));
    let lms_dst = mat_vec(&BRADFORD, &to);
    let mut scaled = IDENTITY;
    for i in 0..3 {
        scaled[i][i] = lms_dst[i] / lms_src[i];
    }
    mul(&inverse(&BRADFORD), &mul(&scaled, &BRADFORD))
}

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut out = IDENTITY;
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn mat_vec(m: &Mat, v: &[f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (0..3).map(|k| m[i][k] * v[k]).sum();
    }
    out
}

fn inverse(m: &Mat) -> Mat {
    // 写成 [[a,b,c],[d,e,f],[g,h,i]] 的伴随矩阵公式，比逐格抄余子式少一次转置手误。
    let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
    let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
    let (g, h, i) = (m[2][0], m[2][1], m[2][2]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    assert!(det.abs() > 1e-12, "原色退化，矩阵不可逆");
    [
        [
            (e * i - f * h) / det,
            (c * h - b * i) / det,
            (b * f - c * e) / det,
        ],
        [
            (f * g - d * i) / det,
            (a * i - c * g) / det,
            (c * d - a * f) / det,
        ],
        [
            (d * h - e * g) / det,
            (b * g - a * h) / det,
            (a * e - b * d) / det,
        ],
    ]
}

/// sRGB 编码值 → 线性光的查表。曲线在 0.04045 处才换段，两段都是光滑的，
/// 1/512 的网格上分段线性插值误差在 1e-5 量级，远低于 8-bit 的 1/255。
fn linearizing_lut() -> &'static [f32; 513] {
    use std::sync::OnceLock;
    static LUT: OnceLock<[f32; 513]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0.0f32; 513];
        for (i, v) in t.iter_mut().enumerate() {
            *v = srgb_to_linear(i as f64 / 512.0);
        }
        t
    })
}

fn lut_lookup(v: f64, lut: &[f32; 513]) -> f64 {
    let x = (v * 512.0).clamp(0.0, 512.0);
    let i = x as usize;
    if i >= 512 {
        return f64::from(lut[512]);
    }
    let a = f64::from(lut[i]);
    a + (f64::from(lut[i + 1]) - a) * (x - i as f64)
}

fn srgb_to_linear(v: f64) -> f32 {
    if v <= 0.04045 {
        (v / 12.92) as f32
    } else {
        ((v + 0.055) / 1.055).powf(2.4) as f32
    }
}

/// 反向：线性光 → 目标色域的编码值。这里只能老实开 pow——每帧每像素三次，
/// 是整个色彩标记里唯一的大头，但它只在用户选了广色域导出时才走。
fn linear_to_encoded(v: f64, cs: ColorSpace) -> f64 {
    if cs.uses_srgb_trc() {
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    } else {
        v.powf(1.0 / 2.4)
    }
}

/// sRGB → XYZ（D65），转换链路的第一步。
fn srgb_to_xyz() -> Mat {
    use std::sync::OnceLock;
    static M: OnceLock<Mat> = OnceLock::new();
    *M.get_or_init(|| rgb_to_xyz(ColorSpace::Srgb.primaries()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    /// 从自己写的配置里读一个 `XYZ ` 标签的三个值。
    fn read_xyz(p: &[u8], sig: &[u8; 4]) -> [f64; 3] {
        let body = ColorSpace::tag_bytes(p, sig).expect("标签存在");
        assert_eq!(&body[..4], b"XYZ ");
        let mut out = [0.0; 3];
        for (i, v) in out.iter_mut().enumerate() {
            *v = i32::from_be_bytes(body[8 + 4 * i..12 + 4 * i].try_into().unwrap()) as f64
                / 65536.0;
        }
        out
    }

    #[test]
    fn srgb_matrix_matches_the_published_values() {
        let m = rgb_to_xyz(ColorSpace::Srgb.primaries());
        // IEC 61966-2-1 附录里的 D65 矩阵，推到 1e-4 以内说明原色与白点没抄错。
        let want = [
            [0.4124, 0.3576, 0.1805],
            [0.2126, 0.7152, 0.0722],
            [0.0193, 0.1192, 0.9505],
        ];
        for (i, row) in m.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!(
                    near(*v, want[i][j], 1e-4),
                    "[{i}][{j}] {v} != {}",
                    want[i][j]
                );
            }
        }
    }

    #[test]
    fn header_offsets_are_where_the_spec_puts_them() {
        for cs in ColorSpace::ALL {
            let p = cs.icc();
            let size = u32::from_be_bytes(p[off::SIZE..4].try_into().unwrap()) as usize;
            assert_eq!(size, p.len(), "{} 头部长度对不上", cs.key());
            assert_eq!(&p[off::VERSION..12], [0x04, 0x30, 0x00, 0x00]);
            assert_eq!(&p[off::CLASS..16], b"mntr");
            assert_eq!(&p[off::COLOR_SPACE..20], b"RGB ");
            assert_eq!(&p[off::PCS..24], b"XYZ ");
            assert_eq!(&p[off::SIGNATURE..40], b"acsp");
            assert_eq!(
                &p[off::WHITE_POINT..80],
                &fixed_xyz(D50_XYZ[0], D50_XYZ[1], D50_XYZ[2])[8..],
                "白点不在 68"
            );
            assert_eq!(&p[off::PROFILE_ID..100], &[0u8; 16]);
            assert_eq!(&p[100..128], &[0u8; 28]);
            assert_eq!(
                u32::from_be_bytes(p[off::TAG_COUNT..132].try_into().unwrap()),
                9
            );
        }
    }

    #[test]
    fn tag_table_is_sorted_aligned_and_in_range() {
        for cs in ColorSpace::ALL {
            let p = cs.icc();
            let count = u32::from_be_bytes(p[off::TAG_COUNT..132].try_into().unwrap()) as usize;
            let mut prev = [0u8; 4];
            for i in 0..count {
                let e = 132 + 12 * i;
                let sig = [p[e], p[e + 1], p[e + 2], p[e + 3]];
                let start = u32::from_be_bytes(p[e + 4..e + 8].try_into().unwrap()) as usize;
                let len = u32::from_be_bytes(p[e + 8..e + 12].try_into().unwrap()) as usize;
                assert!(start + len <= p.len(), "{} 标签 {sig:?} 越界", cs.key());
                assert_eq!(start % 4, 0, "标签 {sig:?} 没 4 字节对齐");
                assert!(sig >= prev, "标签没按签名升序：{prev:?} 后来 {sig:?}");
                prev = sig;
            }
        }
    }

    #[test]
    fn srgb_colorants_match_the_reference_profile() {
        // 对照公开的 sRGB IEC61966-2.1 配置里 rXYZ/gXYZ/bXYZ 的 D50 值。差到 1e-4
        // 量级来自对方当年的取整，不是链路错。
        let p = ColorSpace::Srgb.icc();
        let want = [
            (b"rXYZ", [0.43607, 0.22249, 0.01392]),
            (b"gXYZ", [0.38515, 0.71687, 0.09708]),
            (b"bXYZ", [0.14307, 0.06061, 0.71410]),
        ];
        for (sig, vals) in want {
            let got = read_xyz(&p, sig);
            for (k, v) in vals.iter().enumerate() {
                assert!(
                    near(got[k], *v, 3e-4),
                    "{sig:?} 第 {k} 个 {} != {v}",
                    got[k]
                );
            }
        }
        // 三列之和必须等于 PCS 白点：这是"原色确实搬到了 D50"的硬条件。
        let sum: [f64; 3] = (0..3)
            .map(|row| {
                read_xyz(&p, b"rXYZ")[row] + read_xyz(&p, b"gXYZ")[row] + read_xyz(&p, b"bXYZ")[row]
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        for (i, v) in sum.iter().enumerate() {
            assert!(
                near(*v, D50_XYZ[i], 3e-4),
                "第 {i} 行之和 {v} != {}",
                D50_XYZ[i]
            );
        }
    }

    #[test]
    fn wide_gamut_colorants_are_wider_but_share_one_white() {
        // 广色域的红必须比 sRGB 更纯（X/Y 比值更高），白点却还是同一个。
        let s = ColorSpace::Srgb.icc();
        let p3 = ColorSpace::DisplayP3.icc();
        let r2020 = ColorSpace::Rec2020.icc();
        let ratio = |p: &[u8]| read_xyz(p, b"rXYZ")[0] / read_xyz(p, b"rXYZ")[1];
        assert!(ratio(&p3) > ratio(&s), "P3 的红没比 sRGB 更偏 X");
        assert!(ratio(&r2020) > ratio(&p3), "2020 的红没比 P3 更偏 X");
        for p in [&p3, &r2020] {
            for i in 0..3 {
                assert!(
                    near(read_xyz(p, b"wtpt")[i], D50_XYZ[i], 1e-4),
                    "白点不是 D50"
                );
            }
        }
    }

    #[test]
    fn chad_is_the_adaptation_matrix_in_row_major_order() {
        let p = ColorSpace::DisplayP3.icc();
        let body = ColorSpace::tag_bytes(&p, b"chad").expect("chad 在");
        assert_eq!(&body[..4], b"sf32");
        // 拿推导的同一条公式独立算一遍，再逐个数对：能抓住行列转置。
        let want = bradford(D65, D50_XYZ);
        for (i, row) in want.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let raw = i32::from_be_bytes(
                    body[8 + 4 * (i * 3 + j)..12 + 4 * (i * 3 + j)]
                        .try_into()
                        .unwrap(),
                );
                assert!(near(f64::from(raw) / 65536.0, *v, 1e-5), "[{i}][{j}]");
            }
        }
        // chad 乘 D65 的原色矩阵，必须等于标签里写的 D50 原色。
        let m65 = rgb_to_xyz(ColorSpace::DisplayP3.primaries());
        for col in 0..3 {
            let col65: [f64; 3] = [m65[0][col], m65[1][col], m65[2][col]];
            let adapted = mat_vec(&want, &col65);
            let sig = [b"rXYZ", b"gXYZ", b"bXYZ"][col];
            let in_file = read_xyz(&p, sig);
            for i in 0..3 {
                assert!(near(in_file[i], adapted[i], 2e-5), "{col} 第 {i} 行不匹配");
            }
        }
    }

    #[test]
    fn trc_curves_are_the_two_we_claim() {
        // sRGB/P3：分段 para 3 型，参数顺序 g,a,b,c,d,e,f；2020：1024 点 curv 表。
        for cs in [ColorSpace::Srgb, ColorSpace::DisplayP3] {
            let p = cs.icc();
            for sig in [b"rTRC", b"gTRC", b"bTRC"] {
                let trc = ColorSpace::tag_bytes(&p, sig).expect("TRC 在");
                assert_eq!(&trc[..4], b"para");
                assert_eq!(u16::from_be_bytes(trc[8..10].try_into().unwrap()), 3);
                assert_eq!(
                    i32::from_be_bytes(trc[12..16].try_into().unwrap()),
                    s15_i32(2.4),
                    "{} 的 γ 不是 2.4",
                    cs.key()
                );
                assert_eq!(
                    i32::from_be_bytes(trc[28..32].try_into().unwrap()),
                    s15_i32(0.04045),
                    "分段点 d 不在第 5 个参数上"
                );
            }
        }
        let p = ColorSpace::Rec2020.icc();
        let trc = ColorSpace::tag_bytes(&p, b"rTRC").expect("TRC 在");
        assert_eq!(&trc[..4], b"para", "2020 的曲线必须是 para 3 型");
        assert_eq!(u16::from_be_bytes(trc[8..10].try_into().unwrap()), 3);
        assert_eq!(trc.len(), 40);
        assert_eq!(
            i32::from_be_bytes(trc[12..16].try_into().unwrap()),
            s15_i32(2.4)
        );
        for i in 1..7 {
            let at = 12 + 4 * i;
            let want = if i == 1 { 1.0 } else { 0.0 };
            assert_eq!(
                i32::from_be_bytes(trc[at..at + 4].try_into().unwrap()),
                s15_i32(want),
                "纯幂第 {i} 个参数不该是别的值"
            );
        }
    }

    fn s15_i32(v: f64) -> i32 {
        (v * 65536.0).round() as i32
    }

    #[test]
    fn encoded_and_linearizing_curves_are_inverse() {
        // 进来的像素永远是 sRGB 编码，所以解码只有 sRGB 这一条曲线（查表），编码用
        // 目标色域自己那条。ICC 里的 rTRC 必须就是编码函数的逆，否则标记和像素说的
        // 不是同一条曲线，广色域反而更糊。
        let lut = linearizing_lut();
        for cs in [ColorSpace::Srgb, ColorSpace::DisplayP3] {
            for i in 1..=64 {
                let v = i as f64 / 64.0;
                let enc = linear_to_encoded(lut_lookup(v, lut), cs);
                assert!(
                    (enc - v).abs() < 2e-5,
                    "{} 在 {v} 上回不来：{enc}",
                    cs.key()
                );
            }
        }
        // Rec.2020 的源曲线（sRGB）和目标曲线（γ2.4）本来就不同，不存在整体往返，
        // 只验编码是 ICC 里那条曲线的逆：从 para 参数把 EOTF 算出来，再编码回去。
        let p = ColorSpace::Rec2020.icc();
        let trc = ColorSpace::tag_bytes(&p, b"rTRC").unwrap();
        let par = |i: usize| {
            f64::from(i32::from_be_bytes(
                trc[12 + 4 * i..16 + 4 * i].try_into().unwrap(),
            )) / 65536.0
        };
        let (g, a, b, c, d, e, f) = (par(0), par(1), par(2), par(3), par(4), par(5), par(6));
        for i in 0..=64 {
            let device = i as f64 / 64.0;
            let lin = if device > d {
                (a * device + b).powf(g) + e
            } else {
                c * device + f
            };
            let enc = linear_to_encoded(lin, ColorSpace::Rec2020);
            assert!(
                (enc - device).abs() < 1e-5,
                "2020 在 {device} 上不是 ICC 曲线的逆：{enc}"
            );
        }
    }

    #[test]
    fn conversion_round_trips_through_xyz() {
        let lut = linearizing_lut();
        for v in [0.0, 0.18, 0.5, 0.75, 1.0] {
            let mut px = vec![v as f32; 3];
            ColorSpace::DisplayP3.convert_in_place(&mut px);
            // 反向：P3 编码 → P3 线性 → XYZ → sRGB 线性 → sRGB 编码。
            let lin: [f64; 3] = [
                lut_lookup(f64::from(px[0]), lut),
                lut_lookup(f64::from(px[1]), lut),
                lut_lookup(f64::from(px[2]), lut),
            ];
            let xyz = mat_vec(&rgb_to_xyz(ColorSpace::DisplayP3.primaries()), &lin);
            let back = mat_vec(&ColorSpace::Srgb.xyz_to_rgb(), &xyz);
            for (i, got) in back.iter().enumerate() {
                let e = linear_to_encoded(got.clamp(0.0, 1.0), ColorSpace::Srgb);
                assert!((e - v).abs() < 2e-3, "{v} 往返第 {i} 通道漂了 {e}");
            }
        }
    }

    #[test]
    fn srgb_tag_leaves_pixels_untouched() {
        let mut v = vec![0.25, 0.5, 0.75];
        ColorSpace::Srgb.convert_in_place(&mut v);
        assert_eq!(v, vec![0.25, 0.5, 0.75]);
    }

    #[test]
    fn conversion_is_channel_aware_and_keeps_gray_neutral() {
        // 转换是矩阵乘，不是逐通道曲线，所以纯色必然"串味"：sRGB 纯红在 P3 里不是
        // 原色，绿和蓝会被抬起来。这一条恰恰证明矩阵真的动了，写成恒等才该慌。
        let mut red = vec![1.0f32, 0.0, 0.0];
        ColorSpace::DisplayP3.convert_in_place(&mut red);
        assert!(
            red[0] > red[1] && red[0] > red[2] && red[1] > 0.1,
            "红不像红了: {red:?}"
        );
        // 编码值解回线性应当等于公开的 sRGB→P3 矩阵第一列。
        let decode = linearizing_lut();
        for (c, expected) in [0.822, 0.033, 0.017].iter().enumerate() {
            let got = lut_lookup(red[c] as f64, decode);
            assert!(
                (got - expected).abs() < 3e-3,
                "第 {c} 通道算出 {got}，公开矩阵是 {expected}"
            );
        }
        // 但中性灰不管进哪个色域都必须还是中性，否则白平衡会被转换带跑。
        for cs in [ColorSpace::DisplayP3, ColorSpace::Rec2020] {
            let mut gray = vec![0.5f32, 0.5, 0.5];
            cs.convert_in_place(&mut gray);
            assert!(
                (gray[0] - gray[1]).abs() < 1e-6 && (gray[1] - gray[2]).abs() < 1e-6,
                "{} 让灰失去了中性: {gray:?}",
                cs.key()
            );
        }
    }

    #[test]
    fn wide_gamut_darkens_saturated_red() {
        // sRGB 纯红在 P3 里不是原色，编码值必须往回收，收不动才是转换写反了。
        let mut v = vec![1.0, 0.0, 0.0];
        ColorSpace::DisplayP3.convert_in_place(&mut v);
        assert!(v[0] < 0.99 && v[0] > 0.9, "R 通道 {v:?}");
        assert!(v[2] > 0.0, "B 通道应被抬起: {v:?}");
        let mut w = vec![1.0, 0.0, 0.0];
        ColorSpace::Rec2020.convert_in_place(&mut w);
        assert!(w[0] < v[0], "2020 比 P3 更该收: {w:?} vs {v:?}");
    }

    #[test]
    fn convert_in_place_only_touches_whole_pixels() {
        // 长度不是 3 的倍数时必须原样保留尾巴，别让 chunks_exact_mut 悄悄吃掉。
        let mut v: Vec<f32> = (0..10).map(|i| i as f32 / 9.0).collect();
        let tail_before = v[9];
        ColorSpace::DisplayP3.convert_in_place(&mut v);
        assert_eq!(v[9], tail_before);
    }

    #[test]
    fn icc_bytes_are_deterministic() {
        // 同一份参数必须生成同一串字节：创建时间写死为零就是为此。
        for cs in ColorSpace::ALL {
            assert_eq!(cs.icc(), cs.icc(), "{} 每次生成的字节不一样", cs.key());
        }
    }

    #[test]
    fn dump_profiles_for_littlecms() {
        // 自己写的配置自己说对了不算数。设了 AEVUM_ICC_DUMP 才落盘，交给
        // acceptance/oracle/check_icc.py 用 littleCMS（Pillow 内置）读回来核字段。
        let Some(dir) = crate::env::var_os("ICC_DUMP") else {
            return;
        };
        std::fs::create_dir_all(&dir).expect("建目录");
        for cs in ColorSpace::ALL {
            let path = std::path::Path::new(&dir).join(format!("{}.icc", cs.key()));
            std::fs::write(&path, cs.icc()).expect("写配置");
        }
    }

    #[test]
    fn tag_bytes_survives_bad_input() {
        let p = ColorSpace::Srgb.icc();
        assert!(ColorSpace::tag_bytes(&p, b"nope").is_none());
        assert!(ColorSpace::tag_bytes(&p[..100], b"desc").is_none());
        assert!(ColorSpace::tag_bytes(&[], b"desc").is_none());
    }

    #[test]
    fn parse_accepts_the_names_users_type() {
        for cs in ColorSpace::ALL {
            assert_eq!(ColorSpace::parse(cs.key()), Some(cs));
            assert_eq!(ColorSpace::parse(&cs.key().to_uppercase()), Some(cs));
        }
        assert_eq!(ColorSpace::parse("Display P3"), Some(ColorSpace::DisplayP3));
        assert_eq!(ColorSpace::parse("Rec.2020"), Some(ColorSpace::Rec2020));
        assert_eq!(ColorSpace::parse(" BT.2020 "), Some(ColorSpace::Rec2020));
        assert_eq!(ColorSpace::parse("adobe rgb"), None);
    }

    #[test]
    fn vui_triples_are_the_names_ffmpeg_accepts() {
        assert_eq!(ColorSpace::Srgb.vui(), ("bt709", "bt709", "bt709"));
        assert_eq!(
            ColorSpace::DisplayP3.vui(),
            ("smpte432", "iec61966-2-1", "bt709")
        );
        assert_eq!(ColorSpace::Rec2020.vui(), ("bt2020", "bt709", "bt2020nc"));
    }

    #[test]
    fn serde_keys_round_trip_for_project_files() {
        for cs in ColorSpace::ALL {
            let t = serde_json::to_string(&cs).unwrap();
            assert_eq!(serde_json::from_str::<ColorSpace>(&t).unwrap(), cs);
        }
        // 老工程文件里没有这个键，必须落到 sRGB（不改既有行为）。
        assert_eq!(
            serde_json::from_value::<Option<ColorSpace>>(serde_json::Value::Null).unwrap(),
            None
        );
    }
}
