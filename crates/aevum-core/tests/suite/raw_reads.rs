//! RAW 直读：手搓 TIFF 字节，验证图层挑选、预览兜底与坏文件不炸。
//!
//! 这台机器上没有任何厂商 RAW 样本（dev 树、release 树、图片与下载目录都搜过），
//! 所以测试自带一个多图层 TIFF 写入器：条带区 → 长值区 → 若干 IFD，层与层之间用
//! IFD 链和 SubIFD(330) 互指，与真实厂商文件的形状一致。预览用的 JPEG 由 `image`
//! 的编码器现写一张，保证 SOI/EOI 是真结构而不是手摆的字节。
//!
//! 断言全部落在**像素值**上：只返回 `Ok` 不足以证明解对了。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use image::codecs::jpeg::JpegEncoder;
use image::{Rgb, RgbImage};

use aevum_core::raw::{self, RawPath, Rgb16Image};

// --------------------------------------------------------------- TIFF 写入器

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
    /// 数据区里第 k 块的偏移数组（条带偏移走这条）。
    Data(Vec<usize>),
    /// 数据区里第 k 块的长度数组（条带字节数走这条）。
    DataLens(Vec<usize>),
    /// 某个图层在文件里的落点数组（SubIFD 走这条）。
    Layers(Vec<usize>),
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

fn e_data(tag: u16, idx: &[usize]) -> Ent {
    Ent {
        tag,
        val: Val::Data(idx.to_vec()),
    }
}

fn e_data_lens(tag: u16, idx: &[usize]) -> Ent {
    Ent {
        tag,
        val: Val::DataLens(idx.to_vec()),
    }
}

fn set(entries: &mut Vec<Ent>, e: Ent) {
    entries.retain(|x| x.tag != e.tag);
    entries.push(e);
}

/// 一个 IFD：标记 + 链指针 + SubIFD 指向的图层号。
#[derive(Clone)]
struct Layer {
    entries: Vec<Ent>,
    next: Option<usize>,
    sub: Vec<usize>,
}

fn layer(entries: Vec<Ent>) -> Layer {
    Layer {
        entries,
        next: None,
        sub: Vec::new(),
    }
}

fn chained(entries: Vec<Ent>, next: usize) -> Layer {
    Layer {
        entries,
        next: Some(next),
        sub: Vec::new(),
    }
}

fn subbed(entries: Vec<Ent>, sub: &[usize]) -> Layer {
    Layer {
        entries,
        next: None,
        sub: sub.to_vec(),
    }
}

struct Doc {
    end: End,
    /// TIFF magic：42 正统、0x0202 是 NEF、0x4F52 是 ORF 的 `RO`、0x0055 是松下 RW2。
    magic: u16,
    /// 紧跟在 8 字节头之后的容器签名，CR2 用 `CR`。
    sig: Option<&'static [u8; 2]>,
    /// 数据区，条带与内嵌 JPEG 都放这里。
    data: Vec<Vec<u8>>,
    /// 图层。第 0 层是 IFD0。
    layers: Vec<Layer>,
}

fn doc(layers: Vec<Layer>) -> Doc {
    Doc {
        end: End::Little,
        magic: 42,
        sig: None,
        data: Vec::new(),
        layers,
    }
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

/// (类型号, 元素个数, 载荷)。`layer_at` 只在写第二遍时才是真落点，第一遍全 0，
/// 长度与真值无关，所以两遍的布局完全一致。
fn encode(
    val: &Val,
    end: End,
    layer_at: &[usize],
    data_at: &[usize],
    data_lens: &[usize],
) -> (u16, u32, Vec<u8>) {
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
            (2, out.len() as u32, out)
        }
        Val::Rat(v) => {
            for (n, d) in v {
                push32(&mut out, *n, end);
                push32(&mut out, *d, end);
            }
            (5, v.len() as u32, out)
        }
        Val::Data(idx) => {
            for i in idx {
                push32(&mut out, data_at.get(*i).copied().unwrap_or(0) as u32, end);
            }
            (4, idx.len() as u32, out)
        }
        Val::DataLens(idx) => {
            for i in idx {
                push32(
                    &mut out,
                    data_lens.get(*i).copied().unwrap_or(0) as u32,
                    end,
                );
            }
            (4, idx.len() as u32, out)
        }
        Val::Layers(idx) => {
            for i in idx {
                push32(&mut out, layer_at.get(*i).copied().unwrap_or(0) as u32, end);
            }
            (4, idx.len() as u32, out)
        }
    }
}

/// 布局：8 字节头（+ 容器签名）→ 数据区 → 长值区 → 各 IFD。
///
/// 数据区在最前，于是条带偏移是小数字、坏偏移的用例能自己算出合法值；IFD 全在末尾，
/// 截断测试里任何一刀都会砍掉 IFD 或数据，不会留下「碰巧还能解」的文件。
fn build(d: &Doc) -> Vec<u8> {
    let per_layer: Vec<Vec<Ent>> = d
        .layers
        .iter()
        .map(|l| {
            let mut es = l.entries.clone();
            if !l.sub.is_empty() {
                es.push(Ent {
                    tag: 330,
                    val: Val::Layers(l.sub.clone()),
                });
            }
            es.sort_by_key(|e| e.tag);
            es
        })
        .collect();

    let sig_len = d.sig.map_or(0, |s| s.len());
    let mut cursor = 8 + sig_len;
    let data_at: Vec<usize> = d
        .data
        .iter()
        .map(|b| {
            let at = cursor;
            cursor += b.len();
            at
        })
        .collect();
    let data_lens: Vec<usize> = d.data.iter().map(|b| b.len()).collect();
    let zero_layers = vec![0usize; d.layers.len()];

    // 第一遍只要长度：长值进值区，短值内联。
    let sizing: Vec<Vec<(u16, u32, Vec<u8>)>> = per_layer
        .iter()
        .map(|es| {
            es.iter()
                .map(|e| encode(&e.val, d.end, &zero_layers, &data_at, &data_lens))
                .collect()
        })
        .collect();
    let value_total: usize = sizing
        .iter()
        .flat_map(|l| l.iter())
        .filter(|(_, _, p)| p.len() > 4)
        .map(|(_, _, p)| p.len())
        .sum();

    let mut layer_at = Vec::new();
    let mut at = cursor + value_total;
    for l in &sizing {
        layer_at.push(at);
        at += 2 + 12 * l.len() + 4;
    }

    // 第二遍填真落点；长度与第一遍一致，所以只是把 0 换成实数。
    let coded: Vec<Vec<(u16, u32, Vec<u8>)>> = per_layer
        .iter()
        .map(|es| {
            es.iter()
                .map(|e| encode(&e.val, d.end, &layer_at, &data_at, &data_lens))
                .collect()
        })
        .collect();
    let value_at: Vec<u32> = {
        let mut v = Vec::new();
        let mut p = cursor;
        for (_, _, payload) in coded.iter().flat_map(|l| l.iter()) {
            if payload.len() > 4 {
                v.push(p as u32);
                p += payload.len();
            } else {
                v.push(0);
            }
        }
        v
    };

    let mut out = Vec::new();
    out.extend_from_slice(match d.end {
        End::Little => b"II",
        End::Big => b"MM",
    });
    push16(&mut out, d.magic, d.end);
    push32(
        &mut out,
        layer_at.first().copied().unwrap_or(0) as u32,
        d.end,
    );
    if let Some(sig) = d.sig {
        out.extend_from_slice(sig);
    }
    for b in &d.data {
        out.extend_from_slice(b);
    }
    for payload in coded
        .iter()
        .flat_map(|l| l.iter())
        .filter(|(_, _, p)| p.len() > 4)
    {
        out.extend_from_slice(&payload.2);
    }
    for (li, l) in coded.iter().enumerate() {
        push16(&mut out, l.len() as u16, d.end);
        for (ei, (typ, count, payload)) in l.iter().enumerate() {
            push16(&mut out, per_layer[li][ei].tag, d.end);
            push16(&mut out, *typ, d.end);
            push32(&mut out, *count, d.end);
            if payload.len() <= 4 {
                let mut inline = payload.clone();
                inline.resize(4, 0);
                out.extend_from_slice(&inline);
            } else {
                // 值区的落点按「整份文档的标记顺序」排，这里重新数一遍。
                push32(&mut out, value_at[flat_index(&coded, li, ei)], d.end);
            }
        }
        push32(
            &mut out,
            d.layers[li]
                .next
                .and_then(|n| layer_at.get(n).copied())
                .unwrap_or(0) as u32,
            d.end,
        );
    }
    out
}

/// 第 `li` 层第 `ei` 条在全部标记里的序号（`value_at` 是按这个顺序排的）。
fn flat_index(coded: &[Vec<(u16, u32, Vec<u8>)>], li: usize, ei: usize) -> usize {
    coded[..li].iter().map(|l| l.len()).sum::<usize>() + ei
}

// --------------------------------------------------------------- 图层与素材

/// 单通道 CFA 图层的常用标记：不压缩、整图一条带、2×2 RGGB。
fn cfa_entries(w: u32, h: u32, bits: u16, strip: usize) -> Vec<Ent> {
    vec![
        e_u32(256, w),
        e_u32(257, h),
        e_u16s(371, &[bits]),
        e_u16(277, 1),
        e_u16(262, 32803),
        e_u16(259, 1),
        e_u16(284, 1),
        e_u32(278, h),
        e_data(273, &[strip]),
        e_data_lens(279, &[strip]),
        e_u16s(33421, &[2, 2]),
        e_bytes(33422, &[2, 3, 3, 4]),
    ]
}

/// 一条单色 RGGB 马赛克，16 位小端容器。
fn rggb16(w: usize, h: usize, r: u16, g: u16, b: u16) -> Vec<u16> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            out.push(match (y % 2, x % 2) {
                (0, 0) => r,
                (0, 1) | (1, 0) => g,
                _ => b,
            });
        }
    }
    out
}

fn le16(vals: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vals.len() * 2);
    for v in vals {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn jpeg(w: u32, h: u32, color: [u8; 3]) -> Vec<u8> {
    let img = RgbImage::from_pixel(w, h, Rgb(color));
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 95)
        .encode_image(&img)
        .expect("写 JPEG");
    assert!(out.starts_with(&[0xFF, 0xD8, 0xFF]), "编码器要写 SOI");
    assert!(out.ends_with(&[0xFF, 0xD9]), "编码器要写 EOI");
    out
}

fn pixel(img: &Rgb16Image, x: u32, y: u32) -> [u16; 3] {
    img.get_pixel(x, y).0
}

fn decoded(data: &[u8], what: &str) -> (Rgb16Image, RawPath) {
    match raw::read_bytes(data) {
        Ok(v) => v,
        Err(e) => panic!("{what} 应当解得开，实际报 {e}"),
    }
}

fn sensor(data: &[u8], what: &str) -> Rgb16Image {
    let (img, path) = decoded(data, what);
    assert_eq!(path, RawPath::Sensor, "{what} 应当走传感器路径");
    img
}

// ------------------------------------------------------------------ 临时目录

struct TmpDir {
    path: PathBuf,
}

impl TmpDir {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("时钟正常")
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/_tmp_raw");
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
        // 只删自己这一层，**不删公共父目录**：`remove_dir(root)` 只在父目录恰好空时成功，
        // 于是并行用例之间有一个窗口——A 用完删掉根目录，而 B 刚 `create_dir_all` 完自己的
        // 子目录、还没写文件，B 的写入就会 `NotFound`（2026-11-02 在门槛里真红过一次）。
        // 根目录留着没有代价：`.gitignore` 的 `tests/_tmp*/` 已经盖住它。
        //
        // Remove only this level, **never the shared parent**: `remove_dir(root)` succeeds
        // only when the parent happens to be empty, which opens a window between parallel
        // tests -- A finishes and removes the root while B has just created its own subtree
        // and has not written yet, so B's write fails with `NotFound` (this really turned the
        // gate red once on 2026-11-02). Leaving the root costs nothing: `.gitignore`'s
        // `tests/_tmp*/` already covers it.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_file(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = dir.join(name);
    // 目录可能被并行用例的清理顺手删掉（见 `Drop`）：写之前确保在。
    // The directory may have been removed by a parallel test's cleanup (see `Drop`); make
    // sure it exists before writing.
    let _ = std::fs::create_dir_all(dir);
    std::fs::write(&path, data).expect("写临时文件");
    path
}

// ---------------------------------------------------------------------- 用例

/// 主图在 SubIFD 里，IFD0 只有机身名——只读 IFD0 的解析器会在这里认错图层。
#[test]
fn cfa_in_a_subifd_decodes_on_sensor_path() {
    let mosaic = rggb16(8, 8, 1000, 500, 200);
    let raw_ifd = layer(vec![
        e_ascii(271, "Panasonic"),
        e_ascii(272, "DC-GH5"),
        e_u32s(330, &[1]), // 显式写一遍也会被 build 的自动 330 覆盖，这里靠 subbed 生成
    ]);
    let _ = raw_ifd;
    let d = Doc {
        end: End::Little,
        magic: 42,
        sig: None,
        data: vec![le16(&mosaic)],
        layers: vec![
            subbed(
                vec![e_ascii(271, "Panasonic"), e_ascii(272, "DC-GH5")],
                &[1],
            ),
            layer(cfa_entries(8, 8, 16, 0)),
        ],
    };
    let data = build(&d);
    let img = sensor(&data, "SubIFD 里的 CFA");
    assert_eq!((img.width(), img.height()), (8, 8));
    // 没有白场标记 → 按 16 位满量程，缩放是恒等；2×2 单元平均直接给回写入的三色。
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(pixel(&img, x, y), [1000, 500, 200]);
        }
    }
    let info = raw::info_bytes(&data).expect("读元数据");
    assert_eq!(info.path, RawPath::Sensor);
    assert_eq!(info.bits, 16);
    assert_eq!(info.camera_line().as_deref(), Some("Panasonic DC-GH5"));
    assert_eq!(info.container, "TIFF");
}

/// ORF 的 `II`+`RO` 头 + 主图挂在 IFD 链的下一层。
#[test]
fn orf_ro_magic_and_ifd_chain_reach_the_raw_ifd() {
    let mosaic = rggb16(4, 4, 4000, 2000, 1000);
    let d = Doc {
        end: End::Little,
        magic: 0x4F52,
        sig: None,
        data: vec![le16(&mosaic)],
        layers: vec![
            // IFD0 是一张 2×2 的小图，缺条带标记：会被拒，但面积小，不该顶掉主图的报错。
            chained(
                vec![
                    e_u32(256, 2),
                    e_u32(257, 2),
                    e_u16(259, 1),
                    e_u16(262, 2),
                    e_u16(277, 3),
                ],
                1,
            ),
            layer(cfa_entries(4, 4, 16, 0)),
        ],
    };
    let img = sensor(&build(&d), "ORF 头的链上主图");
    assert_eq!(pixel(&img, 0, 0), [4000, 2000, 1000]);
    assert_eq!(pixel(&img, 3, 3), [4000, 2000, 1000]);
    let info = raw::info_bytes(&build(&d)).expect("读元数据");
    assert_eq!(info.container, "TIFF/RO");
}

/// NEF 的 magic `0x0202` 与 CR2 的头后签名都要认下来，且容器名可区分。
#[test]
fn vendor_header_magics_are_recognised() {
    let mosaic = rggb16(4, 4, 700, 300, 100);
    let nef = Doc {
        magic: 0x0202,
        layers: vec![layer(cfa_entries(4, 4, 16, 0))],
        data: vec![le16(&mosaic)],
        ..doc(Vec::new())
    };
    let img = sensor(&build(&nef), "NEF 头");
    assert_eq!(pixel(&img, 1, 1), [700, 300, 100]);
    assert_eq!(raw::info_bytes(&build(&nef)).unwrap().container, "TIFF/NEF");

    let cr2 = Doc {
        magic: 42,
        sig: Some(b"CR"),
        layers: vec![layer(cfa_entries(4, 4, 16, 0))],
        data: vec![le16(&mosaic)],
        ..doc(Vec::new())
    };
    // 签名占了 8..10，数据区从 10 开始：头认下来、条带偏移仍然自洽。
    assert_eq!(raw::info_bytes(&build(&cr2)).unwrap().container, "TIFF/CR2");
    let img = sensor(&build(&cr2), "CR2 头");
    assert_eq!(pixel(&img, 2, 2), [700, 300, 100]);
}

/// 松下 RW2 的真头是 `II` + `0x0055`（DC-S5M2 实拍文件），不是 BigTIFF 的 `0x0255`。
/// 这条是从「dist exe 打不开真实 RW2、报 magic 0x0055」回归出来的。
#[test]
fn panasonic_rw2_header_is_recognised() {
    let mosaic = rggb16(4, 4, 900, 500, 200);
    let d = Doc {
        magic: 0x0055,
        layers: vec![layer(cfa_entries(4, 4, 16, 0))],
        data: vec![le16(&mosaic)],
        ..doc(Vec::new())
    };
    let img = sensor(&build(&d), "RW2 的 0x0055 头");
    assert_eq!(pixel(&img, 2, 2), [900, 500, 200]);
    assert_eq!(raw::info_bytes(&build(&d)).unwrap().container, "TIFF/RW2");
}

/// 大端容器：`MM` + 42，16 位采样按大端读。
#[test]
fn big_endian_rows_are_read_in_order() {
    let mut be = Vec::new();
    for v in rggb16(4, 4, 600, 400, 200) {
        be.extend_from_slice(&v.to_be_bytes());
    }
    let d = Doc {
        end: End::Big,
        layers: vec![layer(cfa_entries(4, 4, 16, 0))],
        data: vec![be],
        ..doc(Vec::new())
    };
    let img = sensor(&build(&d), "大端 TIFF");
    assert_eq!(pixel(&img, 0, 0), [600, 400, 200]);
    assert_eq!(pixel(&img, 3, 3), [600, 400, 200]);
}

/// 每行补齐字节的条带（CR2 那一类写法）：步幅整除、余量有限才认，超出就当不认识。
#[test]
fn row_padded_strips_decode_and_excessive_pad_is_refused() {
    // 4 列 16 位 = 每行 8 字节，再各补 2 字节 → 步幅 10。
    let mut padded = Vec::new();
    for row in rggb16(4, 4, 900, 600, 300).chunks(4) {
        for v in row {
            padded.extend_from_slice(&v.to_le_bytes());
        }
        padded.extend_from_slice(&[0, 0]);
    }
    let d = Doc {
        data: vec![padded.clone()],
        layers: vec![layer(cfa_entries(4, 4, 16, 0))],
        ..doc(Vec::new())
    };
    let img = sensor(&build(&d), "行补齐条带");
    assert_eq!(pixel(&img, 0, 0), [900, 600, 300]);

    // 每行补 600 字节：余量比一行采样还宽，说明布局判断不可信，必须报错而不是照解。
    let mut fat = Vec::new();
    for row in rggb16(4, 4, 900, 600, 300).chunks(4) {
        for v in row {
            fat.extend_from_slice(&v.to_le_bytes());
        }
        fat.resize(fat.len() + 600, 0);
    }
    let fat_doc = Doc {
        data: vec![fat],
        layers: vec![layer(cfa_entries(4, 4, 16, 0))],
        ..doc(Vec::new())
    };
    let err = raw::read_bytes(&build(&fat_doc)).unwrap_err();
    assert!(
        format!("{err}").contains("条带字节数"),
        "大余量应当报布局错，实际 {err}"
    );
    assert!(raw::RawError::soft_fallback(&err), "布局错可以退预览");
}

/// 14 位打包（ARW/RW2 声称的写法）在传感器路径上被拒，但同一文件里的全尺寸
/// 预览要能被认领，并且明确报告 `RawPath::Preview`。
#[test]
fn packed_bits_fall_back_to_embedded_preview() {
    let big = jpeg(16, 8, [200, 100, 50]);
    let mut d = Doc {
        // 主图按 14 位声明：长度算术区分不了两种打包，只能拒。
        data: vec![vec![0u8; 14], big.clone()],
        layers: vec![
            subbed(
                vec![e_ascii(271, "Sony"), e_ascii(272, "ILCE-7M4")],
                &[1, 2],
            ),
            layer({
                let mut e = cfa_entries(8, 8, 14, 0);
                set(&mut e, e_u32(278, 8));
                e
            }),
            // 全尺寸预览图层：压缩方式 6，跨度是真 JPEG。
            layer(vec![
                e_u32(256, 16),
                e_u32(257, 8),
                e_u16(277, 3),
                e_u16(262, 6),
                e_u16(259, 6),
                e_u32(278, 8),
                e_data(273, &[1]),
                e_data_lens(279, &[1]),
            ]),
        ],
        ..doc(Vec::new())
    };
    // 预览条带的字节数要写真值：14 那位深声明走 e_u16s(371,&[14])，已在 cfa_entries 里。
    set(&mut d.layers[1].entries, e_u16s(371, &[14]));
    let data = build(&d);
    let (img, path) = decoded(&data, "14 位打包 + 预览");
    assert_eq!(path, RawPath::Preview, "必须报告走的是预览");
    assert_eq!((img.width(), img.height()), (16, 8));
    // 预览是 8 位，按 v*257 顶到 16 位：颜色要接近写入的纯色，且 R>G>B 关系不能反。
    let [r, g, b] = pixel(&img, 8, 4);
    assert!(r > g && g > b, "预览通道关系不能反，得到 {r},{g},{b}");
    assert!(
        r.abs_diff(200 * 257) < 6 * 257 && g.abs_diff(100 * 257) < 6 * 257,
        "预览亮度应当接近写入值，得到 {r},{g},{b}"
    );

    let info = raw::info_bytes(&data).expect("预览也要能读元数据");
    assert_eq!(info.path, RawPath::Preview);
    assert_eq!(info.bits, 8, "预览按真实位深报 8");
    assert_eq!(info.width, 16);
    assert_eq!(info.camera_line().as_deref(), Some("Sony ILCE-7M4"));
    assert!(info.fidelity_line().contains("内嵌 JPEG 预览"));
    assert_eq!(
        info.path.describe(),
        "机身内嵌 JPEG 预览（真实 8 位，非传感器数据）"
    );
    // 面板那一行不换行，紧凑口径必须短到放得下，且仍然说清是预览。
    assert_eq!(info.path.short(), "机身预览");
}

/// 找最大的那张完整预览：小缩略图不能顶掉全尺寸预览。
#[test]
fn largest_complete_jpeg_wins() {
    let small = jpeg(4, 4, [10, 200, 10]);
    let big = jpeg(24, 12, [200, 10, 10]);
    let d = Doc {
        data: vec![small, big],
        layers: vec![layer(vec![e_ascii(271, "Nikon"), e_ascii(272, "Z6 II")])],
        ..doc(Vec::new())
    };
    let data = build(&d);
    let info = raw::info_bytes(&data).expect("读元数据");
    assert_eq!((info.width, info.height), (24, 12), "应当挑最大的预览");
    let (img, path) = decoded(&data, "两张预览");
    assert_eq!(path, RawPath::Preview);
    let [r, g, b] = pixel(&img, 12, 6);
    assert!(r > g && r > b, "大图是红色，得到 {r},{g},{b}");
}

/// 假 SOI（后面没有走得通的段结构）不能被当成预览。
#[test]
fn fake_soi_bytes_are_not_a_preview() {
    // 头是 TIFF，主图被拒；数据里只有一段 `FF D8 FF` 开头、段结构走不通的字节。
    let bait = vec![0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x01, 0x02];
    let d = Doc {
        data: vec![vec![0u8; 4], bait],
        layers: vec![layer(cfa_entries(2, 2, 12, 0))],
        ..doc(Vec::new())
    };
    let err = raw::read_bytes(&build(&d)).unwrap_err();
    match err {
        raw::RawError::NoRawData(_) => {}
        other => panic!("应当报「没有可用影像」，实际 {other}"),
    }
}

/// 既不能解 CFA、又没有预览：给具体的具名错误，不是空错误、更不是错像素的 `Ok`。
#[test]
fn nothing_decodable_is_a_specific_error() {
    // 无损 JPEG 压缩的主图（NEF/CR2 的真实写法）+ 没有预览。
    let mut e = cfa_entries(8, 8, 16, 0);
    set(&mut e, e_u16(259, 4));
    let d = Doc {
        data: vec![vec![0u8; 128]],
        layers: vec![layer(e)],
        ..doc(Vec::new())
    };
    let err = raw::read_bytes(&build(&d)).unwrap_err();
    let text = format!("{err}");
    assert!(
        text.contains("没有可解码的原始影像"),
        "应当说清没兜住，实际 {text}"
    );
    assert!(
        text.contains("压缩方式 4"),
        "还要带上被拒的理由，实际 {text}"
    );
    assert!(matches!(err, raw::RawError::NoRawData(_)));
    // 没有压缩方式标记 / 没有 IFD 的破文件同样要报错。
    assert!(raw::read_bytes(b"II\x2a\x00\x08\x00\x00\x00").is_err());
    assert!(matches!(
        raw::read_bytes(b"EXIF\x00\x00\x00\x00").unwrap_err(),
        raw::RawError::NotTiff
    ));
}

/// BigTIFF(43) 与 RRIF(`II`+`UU`) 各自给具名错误，不当成普通 TIFF 硬读。
#[test]
fn bigtiff_and_rrif_are_named_rejections() {
    for (bytes, want) in [
        (b"II\x2b\x00\x08\x00\x00\x00".as_slice(), 43u16),
        (b"II\x55\x55\x08\x00\x00\x00".as_slice(), 0x5555),
        (b"II\x2a\x01\x08\x00\x00\x00".as_slice(), 0x012A),
    ] {
        let err = raw::read_bytes(bytes).unwrap_err();
        let text = format!("{err}");
        assert!(
            text.contains(&format!("{want:#06x}")) || text.contains(&want.to_string()),
            "magic {want} 要能在错误里认出来，实际 {text}"
        );
        assert!(
            matches!(err, raw::RawError::BigTiff(_) | raw::RawError::BadMagic(_)),
            "应当是具名 magic 错误，实际 {err:?}"
        );
    }
    // BigTIFF 的拒绝不是「布局不认识」，不许悄悄退到预览。
    assert!(!raw::RawError::soft_fallback(
        &raw::read_bytes(b"II\x2b\x00\x08\x00\x00\x00").unwrap_err()
    ));
}

/// CR3 是 ISO base media 容器：只找预览，绝不解 CRX。
#[test]
fn bmff_container_is_preview_only() {
    let payload = jpeg(12, 6, [30, 30, 220]);
    let mut d = Doc {
        data: vec![payload],
        layers: Vec::new(),
        ..doc(Vec::new())
    };
    // 直接拼个 ftyp 头：CR3 里 TIFF 层根本不存在，所以这里不写 IFD。
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&24u32.to_be_bytes());
    bytes.extend_from_slice(b"ftypcrx ");
    bytes.extend_from_slice(&[0u8; 8]);
    bytes.extend_from_slice(&d.data[0]);
    let (img, path) = decoded(&bytes, "CR3 容器");
    assert_eq!(path, RawPath::Preview, "CR3 只可能走预览");
    assert_eq!((img.width(), img.height()), (12, 6));
    let info = raw::info_bytes(&bytes).expect("CR3 也能读尺寸");
    assert_eq!(info.container, "ISO/BMFF");
    assert_eq!(info.path, RawPath::Preview);
    // 容器里没有完整 JPEG（EOI 被切掉）时报具名错误。
    let mut truncated = bytes.clone();
    truncated.truncate(truncated.len() - 2);
    assert!(matches!(
        raw::read_bytes(&truncated).unwrap_err(),
        raw::RawError::NoContainerPreview
    ));
    let _ = &mut d;
}

/// AsShotNeutral 存在时按绿通道归一做三通道增益；不存在时绝不调色。
#[test]
fn as_shot_neutral_gains_are_fixed_point_and_exact() {
    let mosaic = rggb16(4, 4, 1000, 1000, 1000);
    let mut e = cfa_entries(4, 4, 16, 0);
    // 中性 R:G:B = 0.5 : 1 : 0.25 → 增益 2× / 1× / 4×，定点 131072 / 65536 / 262144。
    set(&mut e, e_rat(50728, &[(1, 2), (1, 1), (1, 4)]));
    let img = sensor(
        &build(&Doc {
            data: vec![le16(&mosaic)],
            layers: vec![layer(e)],
            ..doc(Vec::new())
        }),
        "带 AsShotNeutral 的图",
    );
    assert_eq!(pixel(&img, 0, 0), [2000, 1000, 4000]);

    // 增益顶到 16× 就停：再大只会把高光刷成纯白。
    let mut wild = cfa_entries(4, 4, 16, 0);
    set(&mut wild, e_rat(50728, &[(1, 1000), (1, 1), (1, 1)]));
    let img = sensor(
        &build(&Doc {
            data: vec![le16(&rggb16(4, 4, 1000, 1000, 1000))],
            layers: vec![layer(wild)],
            ..doc(Vec::new())
        }),
        "极端中性值",
    );
    assert_eq!(pixel(&img, 0, 0), [16000, 1000, 1000]);
}

/// 白场标记决定映射终点：14 位机身写 16383 时必须按声明拉伸，不能顶回满量程。
#[test]
fn declared_white_level_scales_bits() {
    let mut e = cfa_entries(4, 4, 16, 0);
    set(&mut e, e_u16(50717, 16383));
    set(&mut e, e_u16(50714, 1000));
    let img = sensor(
        &build(&Doc {
            data: vec![le16(&rggb16(4, 4, 16383, 1000, 1000))],
            layers: vec![layer(e)],
            ..doc(Vec::new())
        }),
        "带电平的图",
    );
    // (16383-1000)*65535/(16383-1000) = 65535，黑场扣完归零。
    assert_eq!(pixel(&img, 0, 0), [65535, 0, 0]);
}

/// 线性原始数据（34892，无 CFA）当亮度处理；图案不明的 32803 必须拒。
#[test]
fn linear_raw_without_cfa_is_luminance() {
    let mut e = cfa_entries(4, 4, 16, 0);
    set(&mut e, e_u16(262, 34892));
    e.retain(|x| x.tag != 33421 && x.tag != 33422);
    let img = sensor(
        &build(&Doc {
            data: vec![le16(&[100u16; 16])],
            layers: vec![layer(e)],
            ..doc(Vec::new())
        }),
        "线性原始数据",
    );
    assert_eq!(pixel(&img, 2, 2), [100, 100, 100]);

    // 同样的数据声明成 CFA 却没有图案：不能猜 RGBG，退预览（这里没预览 → 报错）。
    let mut missing = cfa_entries(4, 4, 16, 0);
    missing.retain(|x| x.tag != 33421 && x.tag != 33422);
    let err = raw::read_bytes(&build(&Doc {
        data: vec![le16(&[100u16; 16])],
        layers: vec![layer(missing)],
        ..doc(Vec::new())
    }))
    .unwrap_err();
    assert!(
        format!("{err}").contains("缺少必要标记 33421"),
        "实际 {err}"
    );
}

/// 瓦片存储与低位在前都不在这条路上，各自具名拒绝。
#[test]
fn tiles_and_fill_order_two_are_refused() {
    let mut tiled = cfa_entries(4, 4, 16, 0);
    set(&mut tiled, e_u32(322, 64)); // TileWidth
    set(&mut tiled, e_u16(262, 34892));
    tiled.retain(|x| x.tag != 33421 && x.tag != 33422);
    let err = raw::read_bytes(&build(&Doc {
        data: vec![vec![0u8; 32]],
        layers: vec![layer(tiled)],
        ..doc(Vec::new())
    }))
    .unwrap_err();
    assert!(format!("{err}").contains("瓦片"), "实际 {err}");

    let mut order2 = cfa_entries(4, 4, 16, 0);
    set(&mut order2, e_u16(327, 2));
    let err = raw::read_bytes(&build(&Doc {
        data: vec![le16(&rggb16(4, 4, 1, 1, 1))],
        layers: vec![layer(order2)],
        ..doc(Vec::new())
    }))
    .unwrap_err();
    assert!(format!("{err}").contains("FillOrder 2"), "实际 {err}");
}

/// 打包原语的往返：12 位（2 采样 3 字节，等于「4 采样挤进 3 个长字」那一族的字节数）
/// 与 14 位，MSB/LSB 两种位序都要逐位还原。
#[test]
fn packed_rows_roundtrip_through_unpack() {
    use aevum_core::raw::unpack::Packing;
    for bits in [10u8, 12, 14] {
        let values: Vec<u16> = (0..37)
            .map(|i| (i as u16 * 1373) & ((1u32 << bits) - 1) as u16)
            .collect();
        for p in [Packing::MsbRow { bits }, Packing::LsbRow { bits }] {
            let mut bytes = Vec::new();
            assert!(p.pack_row(&values, &mut bytes), "{p:?} 写入失败");
            assert_eq!(bytes.len(), p.row_bytes(values.len()).unwrap());
            let mut back = vec![0u16; values.len()];
            assert!(
                p.unpack_row(&bytes, values.len(), &mut back),
                "{p:?} 解包失败"
            );
            assert_eq!(back, values, "{p:?} 往返不一致");
        }
    }
    // 两种位序给出不同字节：证明上面的往返不是「怎么读都一样」。
    let mut msb = Vec::new();
    let mut lsb = Vec::new();
    assert!(Packing::MsbRow { bits: 12 }.pack_row(&[0xfff, 0x000, 0xabc, 0x123], &mut msb));
    assert!(Packing::LsbRow { bits: 12 }.pack_row(&[0xfff, 0x000, 0xabc, 0x123], &mut lsb));
    assert_ne!(msb, lsb);
    assert_eq!(msb.len(), 6);
    // 行内打包模式不会被子串误伤：短于整行的输入必须失败。
    let mut out = [0u16; 4];
    assert!(!Packing::MsbRow { bits: 12 }.unpack_row(&msb[..5], 4, &mut out));
}

/// 恶意偏移与长度只能换来错误，不能换来 panic 或半张图。
#[test]
fn hostile_offsets_and_lengths_error_without_panicking() {
    let mosaic = rggb16(8, 8, 1000, 500, 200);
    let good = build(&Doc {
        data: vec![le16(&mosaic)],
        layers: vec![layer(cfa_entries(8, 8, 16, 0))],
        ..doc(Vec::new())
    });
    assert!(raw::read_bytes(&good).is_ok(), "基准文件要能解");

    let mut corruptions: Vec<(&str, Vec<u8>)> = Vec::new();

    // IFD0 偏移指回头部 / 指到文件外 / 溢出。
    for (name, off) in [("头内", 0u32), ("头内2", 4u32), ("文件外", 0xFFFF_FFF0u32)] {
        let mut b = good.clone();
        b[4..8].copy_from_slice(&off.to_le_bytes());
        corruptions.push((name, b));
    }
    // 条带偏移/长度改写：数据区从 8 开始，标记块在 273/279 的槽位里。
    for (name, value) in [
        ("条带偏移越界", 0xFFFF_FFFEu32),
        ("条带偏移巨大", 1u32 << 31),
        ("条带长度巨大", u32::MAX),
    ] {
        let mut b = good.clone();
        // IFD 里第 8 条是 273（按标记号排序后），它的值槽在条尾 4 字节。
        let ifd_at = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
        let n = u16::from_le_bytes(b[ifd_at..ifd_at + 2].try_into().unwrap()) as usize;
        let mut patched = false;
        for i in 0..n {
            let rec = ifd_at + 2 + i * 12;
            let tag = u16::from_le_bytes(b[rec..rec + 2].try_into().unwrap());
            if tag == 273 || tag == 279 {
                let typ = u16::from_le_bytes(b[rec + 2..rec + 4].try_into().unwrap());
                let count = u32::from_le_bytes(b[rec + 4..rec + 8].try_into().unwrap());
                // 只改内联写法（8 个 LONG 在真实文件里是外置的），外置就改指针。
                if count == 1 && typ == 4 {
                    b[rec + 8..rec + 12].copy_from_slice(&value.to_le_bytes());
                    patched = true;
                } else {
                    let at = u32::from_le_bytes(b[rec + 8..rec + 12].try_into().unwrap()) as usize;
                    for v in &mut b[at..at + (count as usize * 4)] {
                        *v = value.to_le_bytes()[*v as usize % 4];
                    }
                    patched = true;
                }
            }
        }
        assert!(patched, "{name} 没找到可改的条带标记");
        corruptions.push((name, b));
    }

    // 宽高改大：超出像素上限。
    let mut huge = good.clone();
    {
        let ifd_at = u32::from_le_bytes(huge[4..8].try_into().unwrap()) as usize;
        let n = u16::from_le_bytes(huge[ifd_at..ifd_at + 2].try_into().unwrap()) as usize;
        for i in 0..n {
            let rec = ifd_at + 2 + i * 12;
            let tag = u16::from_le_bytes(huge[rec..rec + 2].try_into().unwrap());
            if tag == 256 || tag == 257 {
                huge[rec + 8..rec + 12].copy_from_slice(&(100_000u32).to_le_bytes());
            }
        }
    }
    corruptions.push(("宽高巨大", huge));

    // 截断：每一刀都必须只是错误。
    let cuts: Vec<usize> = (1..good.len()).step_by(good.len() / 24).collect();
    for cut in cuts {
        let mut b = good.clone();
        b.truncate(cut);
        corruptions.push(("截断", b));
    }
    // 全零与随机尾巴。
    corruptions.push(("全零", vec![0u8; 64]));
    let mut tail = good.clone();
    tail.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0x00, 0x11]);
    corruptions.push(("尾部假 SOI", tail));

    for (name, bytes) in corruptions {
        // 这里不要求「一定失败」——某些改写后文件仍然自洽可解；要验的是绝不 panic、
        // 且失败时给的是这个模块的错误类型而不是不可区分的空错误。
        match raw::read_bytes(&bytes) {
            Ok(_) => {}
            Err(e) => {
                let text = format!("{e}");
                assert!(!text.is_empty(), "{name} 的错误必须是可读的");
                assert!(
                    !matches!(e, raw::RawError::Io(_)),
                    "{name} 不该冒名 IO 错误"
                );
            }
        }
    }
}

/// IFD 链成环与图层爆炸：必须终止并报错，不能把 CPU 吃光。
#[test]
fn ifd_loops_and_explosion_terminate() {
    let entries = cfa_entries(4, 4, 16, 0);
    // 自指链：IFD0 的下一个还是 IFD0。见过就跳过，所以主图照旧能解。
    let mut loop_doc = Doc {
        data: vec![le16(&rggb16(4, 4, 5, 5, 5))],
        layers: vec![chained(entries.clone(), 0)],
        ..doc(Vec::new())
    };
    let img = sensor(&build(&loop_doc), "自指链");
    assert_eq!(pixel(&img, 0, 0), [5, 5, 5]);

    // SubIFD 自指同理。
    loop_doc.layers = vec![subbed(entries.clone(), &[0])];
    let img = sensor(&build(&loop_doc), "SubIFD 自指");
    assert_eq!(pixel(&img, 1, 1), [5, 5, 5]);

    // 图层爆炸：链上挂 300 层空 IFD，超过 MAX_IFDS。
    let layers: Vec<Layer> = (0..300).map(|i| chained(Vec::new(), i + 1)).collect();
    let crowded = Doc {
        data: Vec::new(),
        layers,
        ..doc(Vec::new())
    };
    let err = raw::read_bytes(&build(&crowded)).unwrap_err();
    assert!(format!("{err}").contains("IFD 数量"), "实际 {err}");
}

#[test]
fn is_raw_name_covers_every_extension() {
    for ext in ["rw2", "nef", "arw", "cr2", "cr3", "orf", "rwl"] {
        assert!(
            raw::is_raw_name(format!("IMG_0001.{ext}")),
            "{ext} 应当认领"
        );
        assert!(raw::is_raw_name(format!("IMG_0001.{}", ext.to_uppercase())));
        assert!(raw::is_raw_name(std::path::Path::new("/tmp/a.Rw2")));
    }
    for name in [
        "IMG_0001.jpg",
        "IMG_0001.jpeg",
        "IMG_0001.png",
        "IMG_0001.tif",
        "IMG_0001.tiff",
        "IMG_0001.webp",
        "IMG_0001.bmp",
        "IMG_0001.dng",
        "IMG_0001",
        "noext",
    ] {
        assert!(!raw::is_raw_name(name), "{name} 不该被这个模块认领");
    }
    // .dng 归 dng::is_named_dng，两套后缀表不能重叠。
    assert!(aevum_core::dng::is_named_dng("a.dng"));
    // 内容嗅探只用于目录扫描，改名的 RAW 在这里被认领、在 is_raw_name 不被认领。
    let dir = TmpDir::new("sniff");
    let renamed = write_file(
        &dir,
        "frame.bin",
        &build(&Doc {
            data: vec![le16(&rggb16(4, 4, 1, 2, 3))],
            layers: vec![layer(cfa_entries(4, 4, 16, 0))],
            ..doc(Vec::new())
        }),
    );
    assert!(raw::is_raw(&renamed));
    assert!(!raw::is_raw_name(&renamed));
    assert!(!raw::is_raw(write_file(&dir, "note.txt", b"just text")));
}

/// 逐帧热路径要便宜：`is_raw_name` 只看后缀，不读文件；`metrics::decode` 分流后
/// 位深口径仍是一条（`decode_rgb_f32` 把它当高位深源）。
#[test]
fn metrics_decode_routes_raw_names_as_deep_sources() {
    let dir = TmpDir::new("metrics");
    let mosaic = rggb16(8, 8, 1000, 500, 200);
    let data = build(&Doc {
        data: vec![le16(&mosaic)],
        layers: vec![layer(cfa_entries(8, 8, 16, 0))],
        ..doc(Vec::new())
    });
    let path = write_file(&dir, "frame.arw", &data);
    let img = aevum_core::metrics::decode(&path).expect("RAW 也走统一解码口子");
    assert_eq!(img.color(), image::ColorType::Rgb16, "必须报高位深");
    let (rgb, w, h, deep) = aevum_core::metrics::decode_rgb_f32(&path).expect("f32 解码");
    assert_eq!((w, h), (8, 8));
    assert!(deep, "RAW 传感器路径要按高位深源处理");
    // 2026-11-07 起传感器出口做 sRGB 显示编码（「复位变黑」根因修复）：
    // 这里的期望值同步为 encode(sRGB) 后的结果，透传的旧口径已删。
    //
    // Since 2026-11-07 the sensor exit display-encodes to sRGB (the "reset goes
    // black" fix); expectations follow the encoded values, the old linear
    // pass-through is gone.
    let enc = |v: f32| {
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    };
    assert!((rgb[0] - enc(1000.0 / 65535.0)).abs() < 1e-6);
    assert!((rgb[1] - enc(500.0 / 65535.0)).abs() < 1e-6);
    assert!((rgb[2] - enc(200.0 / 65535.0)).abs() < 1e-6);

    // 预览兜底的同一口径：仍是 Rgb16，深标志为真，但值只有 8 位精度。
    let preview_file = write_file(
        &dir,
        "preview.nef",
        &build(&Doc {
            data: vec![vec![0u8; 8], jpeg(8, 4, [10, 20, 30])],
            layers: vec![layer({
                let mut e = cfa_entries(4, 4, 12, 0);
                set(&mut e, e_u16s(371, &[12]));
                e
            })],
            ..doc(Vec::new())
        }),
    );
    let img = aevum_core::metrics::decode(&preview_file).expect("预览也走同一口子");
    assert_eq!(img.color(), image::ColorType::Rgb16);
    let (_, _, _, deep) = aevum_core::metrics::decode_rgb_f32(&preview_file).expect("f32");
    assert!(deep, "预览顶到 16 位后位深口径不能分裂");
}

// -------------------------------------------------------------- M2 后端路由

/// `read` 经 `backend::full` 路由后语义不回退：无论路由到 rawler 还是内置后端，
/// 内置可解的手搓 TIFF 必须解出同样的分类（传感器路径）、同样的尺寸；
/// 落到内置时像素逐位一致。`--no-default-features` 下整个分支就是内置，
/// 测试照常通过。
#[test]
fn backend_full_keeps_builtin_semantics() {
    let dir = TmpDir::new("backend");
    let data = build(&Doc {
        data: vec![le16(&rggb16(8, 8, 1000, 500, 200))],
        layers: vec![layer(cfa_entries(8, 8, 16, 0))],
        ..doc(Vec::new())
    });
    let path = write_file(&dir, "frame.rw2", &data);

    let (via_read, path_kind) = raw::read(&path).expect("路由入口要能解");
    let (builtin, builtin_kind) = raw::read_bytes(&data).expect("内置路径要能解");
    assert_eq!(path_kind, builtin_kind, "路由不得改变保真度分类");
    assert_eq!(path_kind, RawPath::Sensor);
    assert_eq!(
        (via_read.width(), via_read.height()),
        (builtin.width(), builtin.height())
    );
    let report = raw::backend::full(&path).expect("RawDecode 报告");
    assert_eq!(report.path, path_kind, "RawDecode.path 与 read 的分类一致");
    assert!(!report.backend.is_empty(), "后端名要如实报告");
    if report.backend == raw::backend::BUILTIN_NAME {
        assert_eq!(via_read.as_raw(), builtin.as_raw(), "内置后端要逐位一致");
    }
}

/// `active_backend_name` 与 feature 对得上，给信息面板与诊断日志一个稳定口径。
#[test]
fn backend_name_reflects_feature() {
    let name = raw::backend::active_backend_name();
    assert!(!name.is_empty());
    #[cfg(feature = "raw-rawler")]
    assert_eq!(name, "rawler");
    #[cfg(not(feature = "raw-rawler"))]
    assert_eq!(name, "builtin");
}

/// 画布快路径：有内嵌预览时 `backend::preview` 必须走预览（快、且如实报告），
/// 无论 full 路由会选哪个后端。
#[test]
fn backend_preview_prefers_embedded_jpeg() {
    let dir = TmpDir::new("backend_preview");
    let path = write_file(
        &dir,
        "preview.orf",
        &build(&Doc {
            data: vec![vec![0u8; 8], jpeg(8, 4, [10, 20, 30])],
            layers: vec![layer({
                // 12 位 CFA：内置路径解不动（退预览的形状），后端路由必须一致。
                let mut e = cfa_entries(4, 4, 12, 0);
                set(&mut e, e_u16s(371, &[12]));
                e
            })],
            ..doc(Vec::new())
        }),
    );
    let decoded = raw::backend::preview(&path).expect("预览路由要能解");
    assert_eq!(decoded.path, RawPath::Preview, "有内嵌预览就该走预览");
    assert_eq!((decoded.image.width(), decoded.image.height()), (8, 4));
}

// ------------------------------------------------------- 认清但没解码器的后缀

/// 认得出来、但这一版没有解码器的后缀，必须报**名字**，不许掉进「格式无法确定」。
///
/// 判「不许」的方式：文件内容是一段纯垃圾（连 TIFF 头都不是），如果实现先去看内容，
/// 就会报 `NotTiff`；如果实现去问 `image` crate 猜格式，就会报「格式无法确定」。
/// 两种都不是用户能据以行动的话。**按后缀说的话只有一种**：「`heic` 暂不支持」。
///
/// A suffix we recognize but cannot decode in this version must be reported **by name** and
/// must not fall into "could not determine the format".
///
/// How "must not" is judged: the file content is pure garbage (not even a TIFF header), so
/// an implementation that looks at content first reports `NotTiff`, and one that asks the
/// `image` crate to guess reports "could not determine the format". Neither tells the user
/// what to do. **Speaking by suffix leaves exactly one answer**: "`heic` is not supported
/// yet".
#[test]
fn an_unknown_extension_is_named_not_guessed() {
    let dir = TmpDir::new("unsupported_ext");
    // 内容刻意是垃圾：判据要证明结论来自后缀而不是来自内容。
    let heic = write_file(&dir, "IMG_1159.HEIC", b"not a tiff, not a heif box either");
    let err = raw::info(&heic).expect_err("HEIC 这一版不做，不该给尺寸");
    match &err {
        raw::RawError::UnsupportedFormat { ext } => {
            assert_eq!(ext, "heic", "扩展名要归一成小写，否则英文界面里会漏成 HEIC");
        }
        other => panic!("应当是 UnsupportedFormat，实际是 {other:?}"),
    }
    assert!(
        err.to_string().contains("heic"),
        "错误串里要有后缀名，否则用户不知道该转什么：{err}"
    );
    assert!(
        err.to_string().contains("samples/cameras"),
        "要说清怎么办（放样张进来），否则这句话只是一句拒绝：{err}"
    );

    // 解码入口（测光走的那个门）必须给同一个结论：不能「读元数据」和「解像素」两套说法。
    let decoded = aevum_core::metrics::decode(&heic).expect_err("解码入口同样要按后缀拒绝");
    match decoded {
        aevum_core::metrics::MetricsError::Raw(raw::RawError::UnsupportedFormat { ext }) => {
            assert_eq!(ext, "heic");
        }
        other => panic!("解码入口应当是 UnsupportedFormat，实际是 {other:?}"),
    }

    // 各家 RAW 的后缀同理：这不是「HEIC 特例」，而是一张表。
    for (name, want) in [
        ("shot.raf", "raf"),
        ("shot.PEF", "pef"),
        ("shot.x3f", "x3f"),
        ("shot.3fr", "3fr"),
    ] {
        let path = write_file(&dir, name, b"\x00\x01\x02");
        match raw::info(&path) {
            Err(raw::RawError::UnsupportedFormat { ext }) => assert_eq!(ext, want, "{name}"),
            other => panic!("{name} 应当是 UnsupportedFormat，实际是 {other:?}"),
        }
    }

    // 反向：我们有解码器的后缀**不许**被这张表吃掉，否则就等于把功能删了。
    // 内容仍是垃圾，所以要到「容器不对」这一层，而不是「格式不支持」。
    let cr3 = write_file(&dir, "clip.CR3", b"\x00\x00\x00\x18ftypcrx ");
    if let Err(raw::RawError::UnsupportedFormat { ext }) = raw::info(&cr3) {
        panic!("CR3 是我们在做的格式，不许报 UnsupportedFormat（报的是 `{ext}`）");
    }
}
