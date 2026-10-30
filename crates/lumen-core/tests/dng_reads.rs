//! DNG 读取：手搓 TIFF 字节，验证 CFA 解码、deflate、电平映射与坏文件不炸。
//!
//! 两边都没有样例 .dng，所以测试自带一个最小 IFD 写入器和一个定长霍夫曼
//! deflate 编码器：`flate2` 只是 image 的传递依赖、不是 lumen-core 的依赖，
//! 不能拿来当 dev-dependency，于是自己写——顺便验证解码器对「裸 deflate 流」
//! 和「流尾多余字节（真实写入器的 adler32）」两种在野写法的容忍度。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lumen_core::dcp;
use lumen_core::dng::{self, DngError, Rgb16Image};

#[derive(Clone, Copy)]
enum End {
    Little,
    Big,
}

#[derive(Clone)]
enum Val {
    Bytes(Vec<u8>),
    U16(Vec<u16>),
    U32(Vec<u32>),
    Ascii(String),
    Rat(Vec<(u32, u32)>),
    SRat(Vec<(i32, i32)>),
}

#[derive(Clone)]
struct Ent {
    tag: u16,
    val: Val,
}

fn e_u16(tag: u16, v: u16) -> Ent {
    Ent {
        tag,
        val: Val::U16(vec![v]),
    }
}

fn e_u16s(tag: u16, v: &[u16]) -> Ent {
    Ent {
        tag,
        val: Val::U16(v.to_vec()),
    }
}

fn e_u32(tag: u16, v: u32) -> Ent {
    Ent {
        tag,
        val: Val::U32(vec![v]),
    }
}

fn e_u32s(tag: u16, v: &[u32]) -> Ent {
    Ent {
        tag,
        val: Val::U32(v.to_vec()),
    }
}

fn e_bytes(tag: u16, v: &[u8]) -> Ent {
    Ent {
        tag,
        val: Val::Bytes(v.to_vec()),
    }
}

fn e_ascii(tag: u16, s: &str) -> Ent {
    Ent {
        tag,
        val: Val::Ascii(s.to_string()),
    }
}

fn e_rat(tag: u16, v: &[(u32, u32)]) -> Ent {
    Ent {
        tag,
        val: Val::Rat(v.to_vec()),
    }
}

fn e_srat(tag: u16, v: &[(i32, i32)]) -> Ent {
    Ent {
        tag,
        val: Val::SRat(v.to_vec()),
    }
}

/// 覆盖同名标记。
fn set(entries: &mut Vec<Ent>, e: Ent) {
    entries.retain(|x| x.tag != e.tag);
    entries.push(e);
}

fn drop_tag(entries: &mut Vec<Ent>, tag: u16) {
    entries.retain(|x| x.tag != tag);
}

fn push16(out: &mut Vec<u8>, v: u16, end: End) {
    match end {
        End::Little => out.extend_from_slice(&v.to_le_bytes()),
        End::Big => out.extend_from_slice(&v.to_be_bytes()),
    }
}

fn push32(out: &mut Vec<u8>, v: u32, end: End) {
    match end {
        End::Little => out.extend_from_slice(&v.to_le_bytes()),
        End::Big => out.extend_from_slice(&v.to_be_bytes()),
    }
}

/// (类型号, 元素个数, 载荷字节)。
fn encode(val: &Val, end: End) -> (u16, u32, Vec<u8>) {
    let mut out = Vec::new();
    match val {
        Val::Bytes(v) => {
            out.extend_from_slice(v);
            (1, v.len() as u32, out)
        }
        Val::U16(v) => {
            for x in v {
                push16(&mut out, *x, end);
            }
            (3, v.len() as u32, out)
        }
        Val::U32(v) => {
            for x in v {
                push32(&mut out, *x, end);
            }
            (4, v.len() as u32, out)
        }
        Val::Ascii(s) => {
            out.extend_from_slice(s.as_bytes());
            out.push(0);
            // ASCII 的 count 含结束符，正好等于载荷长度。
            (2, out.len() as u32, out)
        }
        Val::Rat(v) => {
            for (n, d) in v {
                push32(&mut out, *n, end);
                push32(&mut out, *d, end);
            }
            (5, v.len() as u32, out)
        }
        Val::SRat(v) => {
            for (n, d) in v {
                // SRATIONAL：分子分母都是有符号 32 位，按补码入流。
                push32(&mut out, *n as u32, end);
                push32(&mut out, *d as u32, end);
            }
            (10, v.len() as u32, out)
        }
    }
}

/// 布局：8 字节头 → 条带数据 → 长值区 → IFD0（文件末尾）。
///
/// 条带固定从偏移 8 开始，坏偏移的用例能自己算出合法值；IFD 放最后，于是
/// 截断测试里任何一刀都会砍掉 IFD 或条带，不会留下「碰巧还能解」的文件。
/// 调用方给了 273/279 就不自动生成。
fn build(end: End, entries: &[Ent], strips: &[Vec<u8>]) -> Vec<u8> {
    let mut all = entries.to_vec();
    let mut offsets = Vec::new();
    let mut counts = Vec::new();
    let mut cursor = 8usize;
    for s in strips {
        offsets.push(cursor as u32);
        counts.push(s.len() as u32);
        cursor += s.len();
    }
    if !all.iter().any(|e| e.tag == 273) {
        all.push(e_u32s(273, &offsets));
    }
    if !all.iter().any(|e| e.tag == 279) {
        all.push(e_u32s(279, &counts));
    }
    all.sort_by_key(|e| e.tag);
    let coded: Vec<(u16, u32, Vec<u8>)> = all.iter().map(|e| encode(&e.val, end)).collect();

    let strip_total: usize = strips.iter().map(|s| s.len()).sum();
    let extra_total: usize = coded
        .iter()
        .filter(|(_, _, p)| p.len() > 4)
        .map(|(_, _, p)| p.len())
        .sum();
    let ifd_at = 8 + strip_total + extra_total;

    // 长值按标记顺序排在条带之后，先算出各自的落点。
    let mut value_at = Vec::new();
    let mut at = 8 + strip_total;
    for (_, _, payload) in &coded {
        if payload.len() > 4 {
            value_at.push(at as u32);
            at += payload.len();
        } else {
            value_at.push(0);
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(match end {
        End::Little => b"II",
        End::Big => b"MM",
    });
    push16(&mut out, 42, end);
    push32(&mut out, ifd_at as u32, end);
    for s in strips {
        out.extend_from_slice(s);
    }
    for (_, _, payload) in &coded {
        if payload.len() > 4 {
            out.extend_from_slice(payload);
        }
    }
    push16(&mut out, coded.len() as u16, end);
    for (i, (typ, count, payload)) in coded.iter().enumerate() {
        push16(&mut out, all[i].tag, end);
        push16(&mut out, *typ, end);
        push32(&mut out, *count, end);
        if payload.len() <= 4 {
            let mut inline = payload.clone();
            inline.resize(4, 0);
            out.extend_from_slice(&inline);
        } else {
            push32(&mut out, value_at[i], end);
        }
    }
    push32(&mut out, 0, end);
    out
}

/// 常用标记：单通道 CFA、不压缩、2×2 RGGB、整图一条带。
fn base(w: u32, h: u32, bits: u16) -> Vec<Ent> {
    vec![
        e_u32(256, w),
        e_u32(257, h),
        e_u16s(371, &[bits]),
        e_u16(277, 1),
        e_u16(262, 32803),
        e_u16(259, 1),
        e_u16(284, 1),
        e_u16(278, h as u16),
        e_u16s(33421, &[2, 2]),
        e_bytes(33422, &[2, 3, 3, 4]),
        e_bytes(50706, &[1, 4, 0, 0]),
        e_ascii(271, "TestMake"),
        e_ascii(272, "TestModel"),
    ]
}

fn samples(end: End, bits: u16, vals: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in vals {
        if bits == 8 {
            out.push(*v as u8);
        } else {
            push16(&mut out, *v, end);
        }
    }
    out
}

/// 整图一种色的 RGGB 马赛克。
fn rggb(w: usize, h: usize, r: u16, g: u16, b: u16) -> Vec<u16> {
    rggb_blocks(w, h, &|_, _| (r, g, b))
}

/// 按 2×2 块给色的马赛克，用来检查空间位置没有解反。
fn rggb_blocks(w: usize, h: usize, block: &dyn Fn(usize, usize) -> (u16, u16, u16)) -> Vec<u16> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = block(x / 2, y / 2);
            out.push(match (y % 2, x % 2) {
                (0, 0) => r,
                (0, 1) | (1, 0) => g,
                _ => b,
            });
        }
    }
    out
}

/// 未压缩（stored）deflate 块：每块 64 字节，最后一块带 BFINAL。
/// 定长霍夫曼流的「前缀」可能已经含完整语义（截掉尾巴照样解得开），所以
/// 需要确定性坏流时用这个：stored 块声明的 LEN 与实际字节数一一绑定，
/// 少一个字节就必须报错。顺带补上解码器里 BTYPE=00 那条平时没人走的路。
fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![data]
    } else {
        data.chunks(64).collect()
    };
    let mut out = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let len = chunk.len() as u16;
        let mut w = BitWriter {
            out: Vec::new(),
            cur: 0,
            n: 0,
        };
        w.bits((i + 1 == chunks.len()) as u32, 1); // BFINAL
        w.bits(0, 2); // BTYPE = 00，stored
        // 块头只有 3 位，flush 补的那半个零字节就是解码器要对齐丢掉的位。
        let mut head = w.flush();
        head.extend_from_slice(&len.to_le_bytes());
        head.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&head);
        out.extend_from_slice(chunk);
    }
    out
}

fn one_strip(end: End, bits: u16, vals: &[u16]) -> Vec<Vec<u8>> {
    vec![samples(end, bits, vals)]
}

fn decoded(data: &[u8], what: &str) -> Rgb16Image {
    match dng::read_bytes(data) {
        Ok(img) => img,
        Err(e) => panic!("{what} 应当解得开，实际报 {e}"),
    }
}

fn pixel(img: &Rgb16Image, x: u32, y: u32) -> [u16; 3] {
    img.get_pixel(x, y).0
}

fn same(a: &Rgb16Image, b: &Rgb16Image) -> bool {
    a.width() == b.width()
        && a.height() == b.height()
        && a.pixels().zip(b.pixels()).all(|(x, y)| x.0 == y.0)
}

// ------------------------------------------------------------- deflate 编码器

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

struct BitWriter {
    out: Vec<u8>,
    cur: u32,
    n: u32,
}

impl BitWriter {
    /// deflate 的位序：数值按 LSB 先行入流。
    fn bits(&mut self, v: u32, count: u32) {
        self.cur |= v << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push((self.cur & 0xff) as u8);
            self.cur >>= 8;
            self.n -= 8;
        }
    }

    /// 霍夫曼码反过来写（码字的 MSB 先进流）。
    fn code(&mut self, code: u32, count: u32) {
        let mut rev = 0u32;
        for i in 0..count {
            if code & (1 << i) != 0 {
                rev |= 1 << (count - 1 - i);
            }
        }
        self.bits(rev, count);
    }

    fn flush(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.cur & 0xff) as u8);
        }
        self.out
    }
}

/// 定长表下 literal/length 符号的码字与位宽。
fn lit_code(s: u32) -> (u32, u32) {
    match s {
        0..=143 => (0x30 + s, 8),
        144..=255 => (0x190 + s - 144, 9),
        256..=279 => (s - 256, 7),
        _ => (0xC0 + s - 280, 8),
    }
}

/// 表里不超过 value 的最大基值 → (下标, 附加位宽, 附加位值)。
fn back_sym(bases: &[u16], extras: &[u8], value: usize) -> (usize, u32, u32) {
    for i in (0..bases.len()).rev() {
        if value >= bases[i] as usize {
            return (i, extras[i] as u32, (value - bases[i] as usize) as u32);
        }
    }
    unreachable!("测试编码器收到小于 3 的长度或距离");
}

/// 定长霍夫曼 + 贪心回指的裸 deflate 流（无 zlib 头）。
fn deflate_raw(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter {
        out: Vec::new(),
        cur: 0,
        n: 0,
    };
    w.bits(1, 1); // BFINAL
    w.bits(1, 2); // BTYPE = 01，定长表
    let mut i = 0usize;
    while i < data.len() {
        let mut best = 0usize;
        let mut best_dist = 0usize;
        for (cand, _) in data[..i].iter().enumerate().rev().take(32768) {
            let mut l = 0usize;
            while l < 258 && i + l < data.len() && data[cand + l] == data[i + l] {
                l += 1;
            }
            if l >= 3 && l > best {
                best = l;
                best_dist = i - cand;
                if best == 258 {
                    break;
                }
            }
        }
        if best >= 3 {
            let (li, lextra, lval) = back_sym(&LEN_BASE, &LEN_EXTRA, best);
            let (sym, len) = lit_code(257 + li as u32);
            w.code(sym, len);
            w.bits(lval, lextra);
            let (di, dextra, dval) = back_sym(&DIST_BASE, &DIST_EXTRA, best_dist);
            // 定长表里距离就是 5 位码，码字等于符号下标。
            w.code(di as u32, 5);
            w.bits(dval, dextra);
            i += best;
        } else {
            let (sym, len) = lit_code(data[i] as u32);
            w.code(sym, len);
            i += 1;
        }
    }
    let (sym, len) = lit_code(256);
    w.code(sym, len);
    w.flush()
}

/// 带 2 字节 zlib 头 + 4 字节尾巴（真实写入器写 adler32，值在这里无所谓）的条带。
fn deflate_strip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x9C];
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&data.len().to_le_bytes());
    out
}

/// 横向差分（TIFF Predictor=2）编码：按行独立，采样宽度 1。
fn horizontal_difference(vals: &[u16], width: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(vals.len());
    for row in vals.chunks(width) {
        out.push(row[0]);
        for w in row.windows(2) {
            out.push(w[1].wrapping_sub(w[0]));
        }
    }
    out
}

/// 测试临时目录：`crates/lumen-core/tests/_tmp_dng/<tag>_<pid>_<时间戳>`。
///
/// 落在 crate 内而不是系统 temp，出问题时直接进目录就能看文件；用 `Drop`
/// 收尾，断言失败的展开也会删掉本次的子目录，跑完不在仓库里留残骸。
struct TmpDir {
    path: PathBuf,
}

impl TmpDir {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("时钟正常")
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/_tmp_dng");
        let path = root.join(format!("{tag}_{}_{stamp}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self { path }
    }
}

impl std::ops::Deref for TmpDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        // 公共父目录只在该删干净了才删得掉：还有并行用例在用时保留。
        if let Some(root) = self.path.parent() {
            let _ = std::fs::remove_dir(root);
        }
    }
}

fn write_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, data).expect("写临时文件");
    path
}

// ---------------------------------------------------------------------- 用例

#[test]
fn uncompressed_16bit_cfa_demosaics() {
    let mut entries = base(8, 8, 16);
    set(&mut entries, e_u16(50717, 16383));
    let data = build(
        End::Little,
        &entries,
        &one_strip(End::Little, 16, &rggb(8, 8, 16383, 8000, 100)),
    );
    let img = decoded(&data, "不压缩 16 位 CFA");
    assert_eq!((img.width(), img.height()), (8, 8));
    // 红多绿中蓝少：每个像素都要成立，绝不能解成蓝 dominant。
    for y in 0..8 {
        for x in 0..8 {
            let [r, g, b] = pixel(&img, x, y);
            assert!(r > g && g > b, "({x},{y}) 得到 {r},{g},{b}");
        }
    }
    // 逐位钉住 2×2 单元平均 + 整数电平映射 (v-black)*65535/(white-black)：
    // 16383→65535、8000→32001（8000*65535/16383 的整数商）、100→400。
    assert_eq!(pixel(&img, 0, 0), [65535, 32001, 400]);
    assert_eq!(pixel(&img, 7, 7), [65535, 32001, 400]);
}

#[test]
fn spatial_blocks_are_not_mirrored() {
    let mut entries = base(8, 4, 16);
    set(&mut entries, e_u16(50717, 16383));
    let mosaic = rggb_blocks(8, 4, &|bx, _| {
        if bx < 2 {
            (16383, 4000, 100)
        } else {
            (100, 4000, 16383)
        }
    });
    let img = decoded(
        &build(End::Little, &entries, &one_strip(End::Little, 16, &mosaic)),
        "左右分色的图",
    );
    let [r, _, b] = pixel(&img, 0, 0);
    assert!(r > b, "左上应当偏红，得到 {r},{b}");
    let [r, _, b] = pixel(&img, 7, 3);
    assert!(b > r, "右下应当偏蓝，得到 {r},{b}");
}

#[test]
fn black_and_white_levels_map_to_full_range() {
    let mut entries = base(4, 4, 16);
    set(&mut entries, e_rat(50714, &[(1000, 1)]));
    set(&mut entries, e_u16(50717, 5000));
    let dark = decoded(
        &build(
            End::Little,
            &entries,
            &one_strip(End::Little, 16, &rggb(4, 4, 1000, 1000, 1000)),
        ),
        "黑场图",
    );
    assert_eq!(pixel(&dark, 2, 2), [0, 0, 0], "黑场必须归零");
    let bright = decoded(
        &build(
            End::Little,
            &entries,
            &one_strip(End::Little, 16, &rggb(4, 4, 5000, 5000, 5000)),
        ),
        "白场图",
    );
    assert_eq!(pixel(&bright, 1, 3), [65535, 65535, 65535], "白场必须拉满");

    // 多值黑场取平均：(500+1000+1500+1000)/4 = 1000。
    let mut avg = base(4, 4, 16);
    set(&mut avg, e_u16s(50714, &[500, 1000, 1500, 1000]));
    set(&mut avg, e_u16s(50717, &[4000, 5000]));
    let info = match dng::info_bytes(&build(
        End::Little,
        &avg,
        &one_strip(End::Little, 16, &rggb(4, 4, 1000, 1000, 1000)),
    )) {
        Ok(i) => i,
        Err(e) => panic!("多值电平读不出来：{e}"),
    };
    assert_eq!(
        (info.black, info.white),
        (1000, 5000),
        "黑场取平均、白场取最大"
    );
}

#[test]
fn deflate_strips_match_uncompressed() {
    let mosaic = rggb_blocks(8, 8, &|bx, by| {
        (
            1000 + bx as u16 * 3000,
            2000 + by as u16 * 1111,
            600 + bx as u16 * 700,
        )
    });
    let raw = samples(End::Little, 16, &mosaic);
    // 每 4 行一条带：16 位下每条带 32 个采样 = 64 字节。
    let plain = {
        let mut e = base(8, 8, 16);
        set(&mut e, e_u16(50717, 16383));
        set(&mut e, e_u16(278, 4));
        build(End::Little, &e, &[raw[..64].to_vec(), raw[64..].to_vec()])
    };
    let zipped = {
        let mut e = base(8, 8, 16);
        set(&mut e, e_u16(50717, 16383));
        set(&mut e, e_u16(278, 4));
        set(&mut e, e_u16(259, 8));
        build(
            End::Little,
            &e,
            &[deflate_strip(&raw[..64]), deflate_strip(&raw[64..])],
        )
    };
    let a = decoded(&plain, "不压缩版本");
    let b = decoded(&zipped, "deflate 版本");
    assert!(same(&a, &b), "压缩与否必须逐像素一致");
    // 编码器确实写了回指（而不是逐字节 literal），否则这个测试就是在自说自话。
    assert!(
        deflate_strip(&[0u8; 128]).len() < 40,
        "定长表 + 贪心匹配应当能压住重复字节"
    );

    // 没有 zlib 头的裸 deflate 也要能解，走退回重试那条路。
    let bare = {
        let mut e = base(8, 8, 16);
        set(&mut e, e_u16(50717, 16383));
        set(&mut e, e_u16(259, 8));
        build(End::Little, &e, &[deflate_raw(&raw)])
    };
    assert!(same(&a, &decoded(&bare, "裸 deflate 版本")));
}

#[test]
fn stored_blocks_decode_and_truncation_is_deterministic() {
    // 压缩方式 8 的流里 BTYPE=00（stored）块是合法写法，解码器必须有这条路；
    // 前面的用例全是定长霍夫曼块，这里专门补上它。
    let mut entries = base(8, 8, 16);
    set(&mut entries, e_u16(50717, 16383));
    set(&mut entries, e_u16(259, 8));
    let raw = samples(End::Little, 16, &rggb(8, 8, 16383, 8000, 100));
    let img = decoded(
        &build(End::Little, &entries, &[deflate_stored(&raw)]),
        "stored 块条带",
    );
    assert_eq!(pixel(&img, 0, 0), [65535, 32001, 400]);
    assert_eq!(pixel(&img, 7, 7), [65535, 32001, 400]);

    // 块头声明 64 字节却少给几个字节：stored 的长度与实际数据一一绑定，
    // 任何一刀都是错误，绝不返回填了零的半张图。
    for drop in [1usize, 4, 8] {
        let mut s = deflate_stored(&raw);
        s.truncate(s.len() - drop);
        let err = dng::read_bytes(&build(End::Little, &entries, &[s])).unwrap_err();
        assert!(
            matches!(err, DngError::Deflate(_)),
            "截掉 {drop} 字节得到 {err}"
        );
    }

    // 同一个 stored 流套上 zlib 头：跳过两个字节后必须解出同样的像素。
    let mut zipped = vec![0x78, 0x9C];
    zipped.extend_from_slice(&deflate_stored(&raw));
    assert!(same(
        &img,
        &decoded(
            &build(End::Little, &entries, &[zipped]),
            "带 zlib 头的 stored 条带"
        )
    ));
}

#[test]
fn predictor_two_round_trips() {
    let mosaic = rggb(8, 4, 9000, 4000, 1500);
    let mut entries = base(8, 4, 16);
    set(&mut entries, e_u16(50717, 16383));
    set(&mut entries, e_u16(317, 2));
    let diffed = horizontal_difference(&mosaic, 8);
    let img = decoded(
        &build(End::Little, &entries, &one_strip(End::Little, 16, &diffed)),
        "横向差分条带",
    );
    let mut plain = base(8, 4, 16);
    set(&mut plain, e_u16(50717, 16383));
    assert!(same(
        &img,
        &decoded(
            &build(End::Little, &plain, &one_strip(End::Little, 16, &mosaic)),
            "不压缩版本"
        )
    ));
}

#[test]
fn big_endian_and_8bit_sources() {
    let mut entries = base(4, 4, 16);
    set(&mut entries, e_u16(50717, 16383));
    let img = decoded(
        &build(
            End::Big,
            &entries,
            &one_strip(End::Big, 16, &rggb(4, 4, 16383, 8000, 100)),
        ),
        "大端文件",
    );
    assert_eq!(pixel(&img, 3, 3), [65535, 32001, 400]);

    // 8 位源：没有白场标记时按 255 满量程，128 恰好映到 32896。
    let eight = base(4, 4, 8);
    let data = build(
        End::Little,
        &eight,
        &one_strip(End::Little, 8, &rggb(4, 4, 255, 128, 0)),
    );
    assert_eq!(pixel(&decoded(&data, "8 位文件"), 0, 0), [65535, 32896, 0]);
    let info = match dng::info_bytes(&data) {
        Ok(i) => i,
        Err(e) => panic!("8 位元数据读不出来：{e}"),
    };
    assert_eq!((info.bits, info.white), (8, 255));
}

#[test]
fn linear_raw_without_cfa_is_gray() {
    let mut entries = base(4, 4, 16);
    set(&mut entries, e_u16(262, 34892));
    set(&mut entries, e_u16(50717, 16383));
    drop_tag(&mut entries, 33421);
    drop_tag(&mut entries, 33422);
    let img = decoded(
        &build(
            End::Little,
            &entries,
            &one_strip(End::Little, 16, &[16383u16; 16]),
        ),
        "线性原始数据",
    );
    assert_eq!(pixel(&img, 2, 1), [65535, 65535, 65535]);
}

#[test]
fn color_and_white_balance_tags_are_skipped_not_misread() {
    // 回归用例：DNG 的电平与色彩标记编号是邻居——50714 BlackLevel、50717 WhiteLevel、
    // 50721 ColorMatrix1、50727 AnalogBalance、50728 AsShotNeutral。这版不做白平衡也不做
    // 色彩矩阵，后三个必须按未知标记跳过；一旦编号串了，它们的大数值就会被当成电平，
    // 整张图跟着塌。所以把它们写成「足以撑爆电平」的值，要求逐位和不带它们时一致。
    let vals = rggb(8, 8, 16383, 8000, 100);
    let mut entries = base(8, 8, 16);
    set(&mut entries, e_u16(50717, 16383));
    let clean = decoded(
        &build(End::Little, &entries, &one_strip(End::Little, 16, &vals)),
        "不带色彩标记",
    );
    assert_eq!(pixel(&clean, 0, 0), [65535, 32001, 400]);

    // 黑场顺便写成 SRATIONAL（规范允许的写法之一），值还是 0。
    set(&mut entries, e_srat(50714, &[(0, 1)]));
    set(
        &mut entries,
        e_rat(
            50721,
            &[
                (9_000_000, 1),
                (1, 1),
                (1, 1),
                (1, 1),
                (9_000_000, 1),
                (1, 1),
                (1, 1),
                (1, 1),
                (9_000_000, 1),
            ],
        ),
    );
    set(
        &mut entries,
        e_rat(50727, &[(3_000_000, 1), (2_000_000, 1), (1, 1)]),
    );
    set(
        &mut entries,
        e_rat(50728, &[(1, 1_000_000), (1, 1_000_000), (1, 1_000_000)]),
    );
    let loaded = build(End::Little, &entries, &one_strip(End::Little, 16, &vals));
    let noisy = decoded(&loaded, "带 ColorMatrix/AnalogBalance/AsShotNeutral");
    assert!(
        same(&clean, &noisy),
        "色彩与白平衡标记只能被跳过，不许动像素"
    );

    let info = match dng::info_bytes(&loaded) {
        Ok(i) => i,
        Err(e) => panic!("带色彩标记的元数据读不出来：{e}"),
    };
    assert_eq!((info.black, info.white), (0, 16383), "电平只认 50714/50717");
    // 大端同样一遍：编号串了的话两种字节序都会中招。
    let big = decoded(
        &build(End::Big, &entries, &one_strip(End::Big, 16, &vals)),
        "大端 + 色彩标记",
    );
    assert_eq!(pixel(&big, 4, 4), [65535, 32001, 400]);
}

#[test]
fn info_and_camera_line() {
    let mut entries = base(12, 6, 16);
    set(&mut entries, e_ascii(271, "Sony"));
    set(&mut entries, e_ascii(272, "ILCE-7RM5"));
    set(&mut entries, e_u16(50714, 2048));
    set(&mut entries, e_u16(50717, 16383));
    let data = build(
        End::Little,
        &entries,
        &one_strip(End::Little, 16, &rggb(12, 6, 2048, 2048, 2048)),
    );
    let info = match dng::info_bytes(&data) {
        Ok(i) => i,
        Err(e) => panic!("元数据读不出来：{e}"),
    };
    assert_eq!((info.width, info.height, info.bits), (12, 6, 16));
    assert_eq!(info.camera_line().as_deref(), Some("Sony ILCE-7RM5"));
    assert_eq!((info.black, info.white), (2048, 16383));

    // Model 已含 Make 前缀时不重复一遍。
    let mut dup = base(4, 4, 16);
    set(&mut dup, e_ascii(271, "Canon"));
    set(&mut dup, e_ascii(272, "Canon EOS R5"));
    let info = match dng::info_bytes(&build(
        End::Little,
        &dup,
        &one_strip(End::Little, 16, &rggb(4, 4, 1, 1, 1)),
    )) {
        Ok(i) => i,
        Err(e) => panic!("元数据读不出来：{e}"),
    };
    assert_eq!(info.camera_line().as_deref(), Some("Canon EOS R5"));

    // 只写 UniqueCameraModel 的转换工具：Model 空着时拿它兜底。
    let mut unique = base(4, 4, 16);
    drop_tag(&mut unique, 271);
    drop_tag(&mut unique, 272);
    set(&mut unique, e_ascii(50708, "Nikon Z6 II"));
    let info = match dng::info_bytes(&build(
        End::Little,
        &unique,
        &one_strip(End::Little, 16, &rggb(4, 4, 1, 1, 1)),
    )) {
        Ok(i) => i,
        Err(e) => panic!("元数据读不出来：{e}"),
    };
    assert_eq!(info.model.as_deref(), Some("Nikon Z6 II"));
    assert_eq!(info.camera_line().as_deref(), Some("Nikon Z6 II"));
}

#[test]
fn is_dng_by_extension_or_header() {
    let dir = TmpDir::new("is_dng");
    let data = build(
        End::Little,
        &base(4, 4, 16),
        &one_strip(End::Little, 16, &rggb(4, 4, 1, 1, 1)),
    );
    assert!(dng::is_dng(write_file(&dir, "frame.dng", &data)));
    // 扩展名不可信时靠 DNGVersion 标记认出来。
    assert!(dng::is_dng(write_file(&dir, "frame.bin", &data)));
    // 普通 TIFF（没有 DNGVersion）不算 DNG。
    let mut plain = base(4, 4, 16);
    drop_tag(&mut plain, 50706);
    assert!(!dng::is_dng(write_file(
        &dir,
        "plain.bin",
        &build(
            End::Little,
            &plain,
            &one_strip(End::Little, 16, &rggb(4, 4, 1, 1, 1))
        ),
    )));
    assert!(!dng::is_dng(write_file(
        &dir,
        "junk.bin",
        b"\x89PNG\r\n\x1a\njunkjunk"
    )));
    // 空 .dng 靠扩展名先认领，读的时候再报错——文档里写明了这个取舍。
    let empty = write_file(&dir, "empty.dng", b"");
    assert!(dng::is_dng(&empty));
    assert!(dng::read(&empty).is_err());
}

#[test]
fn path_read_matches_bytes_read() {
    let dir = TmpDir::new("path");
    let mut entries = base(8, 8, 16);
    set(&mut entries, e_u16(50717, 16383));
    let data = build(
        End::Little,
        &entries,
        &one_strip(End::Little, 16, &rggb(8, 8, 16383, 8000, 100)),
    );
    let path = write_file(&dir, "frame.dng", &data);
    let by_path = match dng::read(&path) {
        Ok(img) => img,
        Err(e) => panic!("按路径解码失败：{e}"),
    };
    assert!(same(&by_path, &decoded(&data, "按字节解码")));
    assert_eq!(
        dng::info(&path).expect("按路径读元数据"),
        dng::info_bytes(&data).expect("按字节读元数据")
    );
}

#[test]
fn every_truncation_errors_and_never_panics() {
    let mut entries = base(8, 8, 16);
    set(&mut entries, e_u16(50717, 16383));
    set(&mut entries, e_u16(259, 8));
    set(&mut entries, e_u16(278, 4));
    let raw = samples(End::Little, 16, &rggb(8, 8, 16383, 8000, 100));
    let full = build(
        End::Little,
        &entries,
        &[deflate_strip(&raw[..64]), deflate_strip(&raw[64..])],
    );
    assert!(dng::read_bytes(&full).is_ok());
    // 文件尾 4 字节是「下一个 IFD」的指针，读取器不需要它，所以只允许这一小段截断
    // 仍然可解；其余任何一刀都砍在条带数据或 IFD 上，必须报错而不是崩。
    for cut in 0..full.len() {
        let short_by = full.len() - cut;
        let result = dng::read_bytes(&full[..cut]);
        if short_by > 4 {
            let err = match result {
                Ok(_) => panic!("截断到 {cut} 字节（少 {short_by} 字节）居然解开了"),
                Err(e) => e,
            };
            assert!(
                !matches!(err, DngError::Io(_)),
                "截断 {cut} 报成了 IO：{err}"
            );
        }
        // 元数据入口同样不能炸；能读出来时尺寸必须还是声明的那对值。
        if let Ok(info) = dng::info_bytes(&full[..cut]) {
            assert_eq!((info.width, info.height), (8, 8), "截断 {cut} 读出别的尺寸");
        }
    }
    assert!(
        dng::read_bytes(&full[..full.len() - 4]).is_ok(),
        "只丢指针尾巴不该报错"
    );
}

#[test]
fn hostile_files_are_rejected() {
    let strip = one_strip(End::Little, 16, &rggb(8, 8, 1000, 1000, 1000));
    let with = |f: &dyn Fn(&mut Vec<Ent>)| {
        let mut e = base(8, 8, 16);
        f(&mut e);
        build(End::Little, &e, &strip)
    };

    for code in [7u16, 32773, 34892, 34933, 50000] {
        let err = dng::read_bytes(&with(&|e| set(e, e_u16(259, code)))).unwrap_err();
        assert!(
            matches!(err, DngError::UnsupportedCompression(c) if c == code),
            "压缩 {code} 得到 {err}"
        );
    }
    for bits in [1u16, 12, 14, 24, 32] {
        let err = dng::read_bytes(&with(&|e| set(e, e_u16s(371, &[bits])))).unwrap_err();
        assert!(
            matches!(err, DngError::UnsupportedBits(b) if b == bits),
            "位深 {bits} 得到 {err}"
        );
    }
    // 两个采样两个位深的写法在位深之前就被拒。
    assert!(matches!(
        dng::read_bytes(&with(&|e| {
            set(e, e_u16s(371, &[16, 16]));
            set(e, e_u16(277, 2));
        })),
        Err(DngError::UnsupportedSamples(2))
    ));

    for photo in [1u16, 2, 32804] {
        assert!(matches!(
            dng::read_bytes(&with(&|e| set(e, e_u16(262, photo)))),
            Err(DngError::UnsupportedPhotometric(p)) if p == photo
        ));
    }
    assert!(matches!(
        dng::read_bytes(&with(&|e| set(e, e_u16(284, 3)))),
        Err(DngError::UnsupportedPlanar(3))
    ));

    // 4×4 图案（四拜尔）与含青/洋红的图案都要色彩矩阵或专门模板，这版拒绝。
    assert!(matches!(
        dng::read_bytes(&with(&|e| {
            set(e, e_u16s(33421, &[4, 4]));
            set(
                e,
                e_bytes(33422, &[2, 3, 3, 4, 3, 2, 4, 3, 2, 3, 3, 4, 3, 2, 4, 3]),
            );
        })),
        Err(DngError::UnsupportedCfa)
    ));
    assert!(matches!(
        dng::read_bytes(&with(&|e| set(e, e_bytes(33422, &[1, 3, 3, 5])))),
        Err(DngError::UnsupportedCfa)
    ));
    // 32803 少了 CFA 图案没法去拜尔。
    assert!(matches!(
        dng::read_bytes(&with(&|e| drop_tag(e, 33421))),
        Err(DngError::MissingTag(33421))
    ));
    // 34892 少了图案是合法的（线性数据），但少了必需标记就要报缺失。
    assert!(matches!(
        dng::read_bytes(&with(&|e| drop_tag(e, 277))),
        Err(DngError::MissingTag(277))
    ));

    // 黑场不低于白场。
    assert!(matches!(
        dng::read_bytes(&with(&|e| {
            set(e, e_u16(50714, 5000));
            set(e, e_u16(50717, 5000));
        })),
        Err(DngError::BadLevels { .. })
    ));

    // 非法头部与 magic；BIGTIFF（magic 43）也走这条路报错。
    assert!(matches!(
        dng::read_bytes(b"II\x2b\x00\x08\x00\x00\x00"),
        Err(DngError::NotTiff)
    ));
    assert!(matches!(
        dng::read_bytes(b"MM\x00\x2b\x08\x00\x00\x00"),
        Err(DngError::NotTiff)
    ));
    assert!(matches!(
        dng::read_bytes(b"EXIF\x00\x00\x00\x00"),
        Err(DngError::NotTiff)
    ));

    // IFD 偏移指回头部。
    let mut bogus = build(End::Little, &base(8, 8, 16), &strip);
    bogus[4..8].copy_from_slice(&0u32.to_le_bytes());
    assert!(dng::read_bytes(&bogus).is_err());
    // 条带偏移出界（条带区从 8 开始，故意指到文件尾之外）。
    assert!(matches!(
        dng::read_bytes(&with(&|e| {
            set(e, e_u32s(273, &[u32::MAX]));
        })),
        Err(DngError::StripOutOfRange { .. })
    ));
    // 标记类型写成 ASCII 的宽度：类型不符按坏标记处理。
    assert!(matches!(
        dng::read_bytes(&with(&|e| {
            drop_tag(e, 256);
            e.push(e_ascii(256, "8"));
        })),
        Err(DngError::BadTag(256))
    ));
}

#[test]
fn huge_declared_sizes_error_without_allocating() {
    // 声明 4 GB 的 StripByteCounts：越界检查在分配之前，报错而不是申请内存。
    let mut entries = base(8, 8, 16);
    set(&mut entries, e_u16(259, 8));
    set(&mut entries, e_u32s(273, &[8]));
    set(&mut entries, e_u32s(279, &[4_000_000_000]));
    let err =
        dng::read_bytes(&build(End::Little, &entries, &[deflate_strip(&[0u8; 16])])).unwrap_err();
    assert!(
        matches!(err, DngError::StripOutOfRange { length, .. } if length == 4_000_000_000),
        "得到 {err}"
    );

    // 伪造的超大宽高：分配像素缓冲之前就被尺寸上限挡住。
    let mut huge = base(1, 1, 16);
    set(&mut huge, e_u32(256, 60_000));
    set(&mut huge, e_u32(257, 60_000));
    set(&mut huge, e_u32(278, 60_000));
    let err = dng::read_bytes(&build(End::Little, &huge, &[Vec::new()])).unwrap_err();
    assert!(matches!(err, DngError::TooLarge { .. }), "得到 {err}");

    // 条带数组爆炸：几十万个偏移按坏标记处理。
    let many: Vec<u32> = vec![8; 200_001];
    let mut crowded = base(8, 8, 16);
    set(&mut crowded, e_u32s(273, &many));
    set(&mut crowded, e_u32s(279, &many));
    assert!(matches!(
        dng::read_bytes(&build(End::Little, &crowded, &[Vec::new()])),
        Err(DngError::BadTag(273))
    ));

    // 条带数量与宽高对不上。
    let mut mismatch = base(8, 8, 16);
    set(&mut mismatch, e_u16(278, 2));
    let err = dng::read_bytes(&build(
        End::Little,
        &mismatch,
        &one_strip(End::Little, 16, &rggb(8, 8, 1, 1, 1)),
    ))
    .unwrap_err();
    assert!(
        matches!(err, DngError::StripCountMismatch { found: 1, want: 4 }),
        "得到 {err}"
    );

    // 条带比解码需要的短。
    let mut short = base(8, 8, 16);
    set(&mut short, e_u32s(279, &[2]));
    assert!(matches!(
        dng::read_bytes(&build(
            End::Little,
            &short,
            &one_strip(End::Little, 16, &rggb(8, 8, 1, 1, 1))
        )),
        Err(DngError::StripSizeMismatch { .. })
    ));

    // 坏 deflate 流：长度对不上就报错，绝不返回半套像素。
    let mut zip = base(8, 8, 16);
    set(&mut zip, e_u16(259, 8));
    let garbage = build(
        End::Little,
        &zip,
        &[vec![0x78, 0x9C, 0xFF, 0xFF, 0xFF, 0xFF]],
    );
    assert!(matches!(
        dng::read_bytes(&garbage),
        Err(DngError::Deflate(_))
    ));
    // 只留 zlib 头 + 3 字节：末块没读完，位流耗尽或长度对不上都必须报错。
    // 注意别用「截断后再补零」——补零仍是一条合法的 deflate 流，能正常解完。
    let cut_stream = {
        let s = deflate_strip(&samples(End::Little, 16, &rggb(8, 8, 1, 1, 1)));
        build(End::Little, &zip, &[s[..5].to_vec()])
    };
    assert!(dng::read_bytes(&cut_stream).is_err());
    // 流声明得比条带应有长度更长：也不返回半套像素。
    let too_long = build(
        End::Little,
        &zip,
        &[deflate_strip(&samples(
            End::Little,
            16,
            &vec![7u16; 8 * 8 * 2],
        ))],
    );
    assert!(dng::read_bytes(&too_long).is_err());
    // 条带解出的字节数比应有的少：也是错误，不是半张图。
    let too_short = build(End::Little, &zip, &[deflate_strip(&[0u8; 16])]);
    assert!(matches!(
        dng::read_bytes(&too_short),
        Err(DngError::StripSizeMismatch { .. })
    ));
}

/// DNG 只有自家解码器认，所以整条流水线都得接上：扫描认领 → 共享解码口 → 导出成片。
#[test]
fn dng_frames_reach_the_export_pipeline() {
    use lumen_core::export::{OutputMode, RenderOptions, export_stills};
    use lumen_core::grade::GradeParams;
    use lumen_core::{metrics, sequence};
    use std::sync::atomic::AtomicBool;

    let dir = TmpDir::new("pipeline");
    let data = build(
        End::Little,
        &base(8, 8, 16),
        &one_strip(End::Little, 16, &rggb(8, 8, 65535, 0, 0)),
    );
    write_file(&dir, "frame_0001.dng", &data);
    // 旁车不算帧：XMP 就写在帧旁边，当帧用等于自造坏帧。
    write_file(&dir, "frame_0001.xmp", b"<x:xmpmeta/>");

    let seq = sequence::Sequence::open(&*dir).expect("打开序列");
    assert_eq!(seq.len(), 1, "只有 .dng 该被认领");

    let frame = &seq.frames[0].path;
    let img = metrics::decode(frame).expect("共享解码口认 DNG");
    assert_eq!(img.color(), image::ColorType::Rgb16);
    assert_eq!(img.to_rgb8().get_pixel(3, 5).0, [255, 0, 0]);
    let (_, _, _, deep) = metrics::decode_rgb_f32(frame).expect("按源位深解码");
    assert!(deep, "DNG 解出来是 16bit，导出不能从 8bit 起步");

    let options = RenderOptions {
        mode: OutputMode::Stills,
        out_dir: dir.join("stills"),
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let outs = export_stills(
        std::slice::from_ref(frame),
        &[GradeParams::default()],
        &[lumen_core::develop::Ev::NONE],
        &[[0.0f64; 2]],
        &options,
        &cancel,
        &|_| {},
    )
    .expect("导出一帧 DNG");
    let png = image::open(&outs[0]).expect("读回 PNG");
    assert_eq!((png.width(), png.height()), (8, 8));
    assert_eq!(png.to_rgb8().get_pixel(3, 5).0, [255, 0, 0]);
}

// ------------------------------------------------------- DCP 相机配置

/// sRGB(D65) → XYZ：与 `dcp` 里 D50→sRGB 的常数矩阵互逆。配置用它时矩阵这一段
/// 原样放行，端到端只剩「白平衡 → 曲线 → 编码」，期望值能自己算出来。
const SRGB_TO_XYZ: [[(i32, i32); 3]; 3] = [
    [
        (4_360_747, 10_000_000),
        (3_850_649, 10_000_000),
        (1_430_804, 10_000_000),
    ],
    [
        (2_225_045, 10_000_000),
        (7_168_786, 10_000_000),
        (606_169, 10_000_000),
    ],
    [
        (139_322, 10_000_000),
        (971_045, 10_000_000),
        (7_141_733, 10_000_000),
    ],
];

/// DCP 的最小写入器：magic 换成 "CR"（0x4352），其余布局同 `build`，没有条带。
fn build_dcp(end: End, entries: &[Ent]) -> Vec<u8> {
    let mut all = entries.to_vec();
    all.sort_by_key(|e| e.tag);
    let coded: Vec<(u16, u32, Vec<u8>)> = all.iter().map(|e| encode(&e.val, end)).collect();
    let extra_total: usize = coded
        .iter()
        .filter(|(_, _, p)| p.len() > 4)
        .map(|(_, _, p)| p.len())
        .sum();
    let ifd_at = 8 + extra_total;
    let mut value_at = Vec::new();
    let mut at = 8usize;
    for (_, _, payload) in &coded {
        if payload.len() > 4 {
            value_at.push(at as u32);
            at += payload.len();
        } else {
            value_at.push(0);
        }
    }

    let mut out = Vec::new();
    out.extend_from_slice(match end {
        End::Little => b"II",
        End::Big => b"MM",
    });
    push16(&mut out, 0x4352, end);
    push32(&mut out, ifd_at as u32, end);
    for (_, _, payload) in &coded {
        if payload.len() > 4 {
            out.extend_from_slice(payload);
        }
    }
    push16(&mut out, coded.len() as u16, end);
    for (i, (typ, count, payload)) in coded.iter().enumerate() {
        push16(&mut out, all[i].tag, end);
        push16(&mut out, *typ, end);
        push32(&mut out, *count, end);
        if payload.len() <= 4 {
            let mut inline = payload.clone();
            inline.resize(4, 0);
            out.extend_from_slice(&inline);
        } else {
            push32(&mut out, value_at[i], end);
        }
    }
    push32(&mut out, 0, end);
    out
}

/// 一份给 `TestMake TestModel` 的配置：ForwardMatrix1 = sRGB→XYZ，
/// `curve` 是 ProfileToneCurve 的 (x, y) 有理数对，空表表示不写。
fn dcp_entries(camera: &str, curve: &[(i32, i32)]) -> Vec<Ent> {
    let mut flat = Vec::new();
    for row in SRGB_TO_XYZ {
        flat.extend_from_slice(&row);
    }
    let mut entries = vec![
        e_ascii(50708, camera),
        e_ascii(50936, "Lumen Test"),
        e_srat(50964, &flat),
    ];
    if !curve.is_empty() {
        entries.push(e_srat(50940, curve));
    }
    entries
}

fn srgb_encode(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

#[test]
fn dcp_decode_does_neutral_matrix_and_tone_curve() {
    let dir = TmpDir::new("dcp_decode");
    let mut entries = base(8, 8, 16);
    // 白平衡 [0.5, 1, 0.25] → 增益 [2, 1, 4]；三个通道乘完都该落到 32768。
    set(&mut entries, e_rat(50728, &[(1, 2), (1, 1), (1, 4)]));
    let dng = write_file(
        &dir,
        "frame.dng",
        &build(
            End::Little,
            &entries,
            &one_strip(End::Little, 16, &rggb(8, 8, 16384, 32768, 8192)),
        ),
    );

    let plain = write_file(
        &dir,
        "cam.dcp",
        &build_dcp(End::Little, &dcp_entries("TestMake TestModel", &[])),
    );
    let (rgb, w, h, deep) = dcp::decode(&dng, &plain).expect("DNG + 配置应当解得开");
    assert_eq!((w, h, deep), (8, 8, true));
    let want = srgb_encode(32768.0 / 65535.0);
    for p in rgb.chunks_exact(3) {
        for v in p {
            assert!(
                (f64::from(*v) - want).abs() < 2e-3,
                "白平衡后应当是该灰阶 {want}，得到 {v}"
            );
        }
    }

    // 曲线把线性值减半：点是 (x, y) 序列，即 0→0、1→0.25，线性 0.5 落在 0.125。
    let curved = write_file(
        &dir,
        "curved.dcp",
        &build_dcp(
            End::Little,
            &dcp_entries("TestMake TestModel", &[(0, 1), (0, 1), (1, 1), (1, 4)]),
        ),
    );
    let (rgb, _, _, _) = dcp::decode(&dng, &curved).expect("带曲线的配置也解得开");
    let want = srgb_encode(32768.0 / 65535.0 / 4.0);
    for p in rgb.chunks_exact(3) {
        for v in p {
            assert!(
                (f64::from(*v) - want).abs() < 2e-3,
                "曲线后应当是 {want}，得到 {v}"
            );
        }
    }
}

#[test]
fn dcp_decode_rejects_wrong_source_and_camera() {
    let dir = TmpDir::new("dcp_reject");
    let profile = write_file(
        &dir,
        "cam.dcp",
        &build_dcp(End::Little, &dcp_entries("TestMake TestModel", &[])),
    );

    let no_neutral = write_file(
        &dir,
        "no_neutral.dng",
        &build(
            End::Little,
            &base(8, 8, 16),
            &one_strip(End::Little, 16, &rggb(8, 8, 16384, 32768, 8192)),
        ),
    );
    let e = dcp::decode(&no_neutral, &profile).expect_err("缺白平衡必须报错");
    assert!(e.contains("AsShotNeutral"), "{e}");

    let jpeg = write_file(&dir, "frame.jpg", b"not really an image");
    let e = dcp::decode(&jpeg, &profile).expect_err("JPEG 不能套相机配置");
    assert!(e.contains("只用于 RAW/DNG"), "{e}");

    let mut entries = base(8, 8, 16);
    set(&mut entries, e_rat(50728, &[(1, 1), (1, 1), (1, 1)]));
    let dng = write_file(
        &dir,
        "frame.dng",
        &build(
            End::Little,
            &entries,
            &one_strip(End::Little, 16, &rggb(8, 8, 16384, 32768, 8192)),
        ),
    );
    let other = write_file(
        &dir,
        "other.dcp",
        &build_dcp(End::Little, &dcp_entries("Other Cam", &[])),
    );
    let e = dcp::decode(&dng, &other).expect_err("机型对不上必须拒绝");
    assert!(e.contains("相机配置是给"), "{e}");
}
