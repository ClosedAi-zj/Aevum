//! EXIF 读取：方向、机身、曝光三要素。
//!
//! 只解析 JPEG 的 APP1 与 TIFF 的 IFD，不引第三方 EXIF 库：位域就这么几个，
//! 手写比让依赖树长一圈划算。读不到一律给中性值，绝不报错——EXIF 缺字段的
//! 相机太多了，硬要解析会把能开的图挡在门外。

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// 方向：1 原样，2~8 是镜像/旋转组合（语义见 `oriented`）。
pub const TAG_ORIENTATION: u16 = 0x0112;
const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_ISO: u16 = 0x8827;
const TAG_EXPOSURE: u16 = 0x829A;
const TAG_FNUM: u16 = 0x829D;
const TAG_TV: u16 = 0x9201;
const TAG_AV: u16 = 0x9202;
const TAG_FOCAL: u16 = 0x920A;
const TAG_DATETIME: u16 = 0x9003;
const TAG_LENS: u16 = 0xA434;
const TAG_EXIF_IFD: u16 = 0x8769;
/// 说明 / 创作者 / 版权：IFD0 的三个 ASCII 标签，写元数据只动这三个。
const TAG_DESCRIPTION: u16 = 0x010E;
const TAG_ARTIST: u16 = 0x013B;
const TAG_COPYRIGHT: u16 = 0x8298;
/// Windows 的 `XPTitle`（UCS2 小端）：EXIF 没有 ASCII 的标题标签，用它兜。
const TAG_XP_TITLE: u16 = 0x9C9B;

/// 一次最多取这么多字节：IFD 条目是连续摆放的，整块读一次就够。
/// 上限按 4096 条算，畸形数量换不来大块内存。
const MAX_BLOCK: usize = 2 + 12 * 4096;
/// 文本标签的读取上限。自己也写这几个标签（见 `with_metadata`），所以要放得下
/// 一条完整的说明；`project::Metadata` 的截断上限跟这个数对齐，写进去才读得回来。
const MAX_TEXT: usize = 512;
/// APP1 里可能塞整张预览图。超过这个大小跳过它继续找下一段，不为此吃内存。
const MAX_APP1: usize = 4 * 1024 * 1024;
/// JPEG 一段最长 65535 字节，扣掉段长 2 字节与 `Exif\0\0` 头，TIFF 体只剩这么多。
const MAX_TIFF: usize = 65535 - 2 - 6;

/// 一个文件读出来的 EXIF。全字段可选：什么都读不到是正常状态，不是错误。
#[derive(Clone, Debug, PartialEq)]
pub struct Exif {
    /// 1~8；读不到或越界都是 1。
    pub orientation: u8,
    numbers: BTreeMap<u16, f64>,
    texts: BTreeMap<u16, String>,
}

/// 中性值：方向按 1 走「原样」，0 在 EXIF 里是非法写法，别让调用方特判。
impl Default for Exif {
    fn default() -> Self {
        Self {
            orientation: 1,
            numbers: BTreeMap::new(),
            texts: BTreeMap::new(),
        }
    }
}

impl Exif {
    fn number(&self, tag: u16) -> Option<f64> {
        self.numbers.get(&tag).copied().filter(|v| v.is_finite())
    }

    fn text(&self, tag: u16) -> Option<&str> {
        self.texts
            .get(&tag)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    pub fn make(&self) -> Option<&str> {
        self.text(TAG_MAKE)
    }

    pub fn model(&self) -> Option<&str> {
        self.text(TAG_MODEL)
    }

    pub fn lens(&self) -> Option<&str> {
        self.text(TAG_LENS)
    }

    pub fn datetime(&self) -> Option<&str> {
        self.text(TAG_DATETIME)
    }

    /// 说明（`ImageDescription`）。写出来的元数据靠这几个读回来验证。
    pub fn description(&self) -> Option<&str> {
        self.text(TAG_DESCRIPTION)
    }

    /// 标题（`XPTitle`）。EXIF 没有 ASCII 的标题标签，用的是 UCS2 那个。
    pub fn title(&self) -> Option<&str> {
        self.text(TAG_XP_TITLE)
    }

    /// 创作者（`Artist`）。
    pub fn artist(&self) -> Option<&str> {
        self.text(TAG_ARTIST)
    }

    /// 版权声明（`Copyright`）。
    pub fn copyright(&self) -> Option<&str> {
        self.text(TAG_COPYRIGHT)
    }

    pub fn iso(&self) -> Option<f64> {
        self.number(TAG_ISO)
    }

    pub fn focal_mm(&self) -> Option<f64> {
        self.number(TAG_FOCAL)
    }

    /// 曝光时间（秒）。只给 APEX TV 的机器在这里折算。
    pub fn exposure_sec(&self) -> Option<f64> {
        match self.number(TAG_EXPOSURE) {
            Some(v) if v > 0.0 => Some(v),
            _ => self
                .number(TAG_TV)
                .filter(|tv| tv.abs() < 64.0)
                .map(|tv| 2f64.powf(-tv)),
        }
    }

    /// 光圈 F 值，同上走 AV 折算。
    pub fn fnum(&self) -> Option<f64> {
        match self.number(TAG_FNUM) {
            Some(v) if v > 0.0 => Some(v),
            _ => self
                .number(TAG_AV)
                .filter(|av| av.abs() < 64.0)
                .map(|av| 2f64.powf(av / 2.0)),
        }
    }

    /// 机身 + 镜头一行，给「信息」面板当标题；什么都没有返回 `None`。
    pub fn camera_line(&self) -> Option<String> {
        let model = self.model().or(self.make())?;
        Some(match self.lens() {
            Some(lens) => format!("{model} · {lens}"),
            None => model.to_owned(),
        })
    }

    /// 快门写成 1/200 这种，比 0.005 好读；长于 1 秒写成 `1.3"`。
    pub fn shutter_text(&self) -> Option<String> {
        let sec = self.exposure_sec()?;
        Some(if sec >= 1.0 {
            format!("{sec:.1}\"")
        } else {
            format!("1/{:.0}", 1.0 / sec)
        })
    }

    /// 相对档数 EV，以 ISO100 / 1 秒 / f1.0 为 0 点。绝对值没意义，只用来看跨帧
    /// 有没有台阶；三要素读不全返回 `None`，宁可缺也不猜——猜错会让台阶检测误判。
    pub fn ev(&self) -> Option<f64> {
        let iso = self.iso()?;
        let exp = self.exposure_sec()?;
        let fnum = self.fnum()?;
        if iso <= 0.0 || exp <= 0.0 || fnum <= 0.0 {
            return None;
        }
        Some((iso / 100.0).log2() - exp.log2() + 2.0 * fnum.log2())
    }
}

/// 读一个文件的 EXIF。失败（不是图片、没 EXIF、截断）给中性 `Exif`。
///
/// 每帧解码都会走这里一次，所以按块定位读、不做整读：一张 4K 的 16bit TIFF
/// 有几十 MB，而方向和曝光都在两个 IFD 加几十字节的值里。
pub fn read(path: impl AsRef<Path>) -> Exif {
    let path = path.as_ref();
    let Ok(file) = File::open(path) else {
        return Exif::default();
    };
    let mut f = BufReader::new(file);
    let mut head = [0u8; 2];
    if f.read_exact(&mut head).is_err() {
        return Exif::default();
    }
    match &head {
        b"II" | b"MM" => parse(&mut Source::File(f)).unwrap_or_default(),
        b"\xFF\xD8" => {
            f.seek(SeekFrom::Start(2)).ok();
            match app1_exif(&mut f) {
                Some(body) => parse_mem(&body).unwrap_or_default(),
                None => Exif::default(),
            }
        }
        _ => Exif::default(),
    }
}

/// 一批帧的 EXIF，并行读。序列导入时跑一次，界面与曝光台阶检测都吃这份缓存。
pub fn read_many(paths: &[std::path::PathBuf]) -> Vec<Exif> {
    use rayon::prelude::*;
    paths.par_iter().map(read).collect::<Vec<_>>()
}

/// 取源文件里可以整块搬走的 EXIF：JPEG 的 APP1 去掉 `Exif\0\0` 头之后那段 TIFF 结构。
///
/// 只有 JPEG 能这么搬。TIFF/RAW 的 IFD 里偏移指向的是整个文件，把那段字节塞进
/// PNG/JPEG/WebP 就是坏数据；容器不支持的写法宁可不写，也不要写出打不开的文件。
/// 读不到（不是 JPEG、没 APP1、截断）给 `None`，导出照常继续。
pub fn transfer_block(path: impl AsRef<Path>) -> Option<Vec<u8>> {
    let mut f = BufReader::new(File::open(path).ok()?);
    let mut head = [0u8; 2];
    f.read_exact(&mut head).ok()?;
    if &head != b"\xFF\xD8" {
        return None;
    }
    f.seek(SeekFrom::Start(2)).ok()?;
    app1_exif(&mut f)
}

/// 直接解析一段 TIFF 结构（`transfer_block` 的产物、或自己写出来的块），不经文件。
/// 读不出给中性 `Exif`，跟 `read` 一个口径。
pub fn read_block(block: &[u8]) -> Exif {
    parse_mem(block).unwrap_or_default()
}

/// 把说明 / 创作者 / 版权写成 EXIF 块，`src` 是从源 JPEG 整块搬来的 TIFF（可省）。
///
/// 有源块时**不重排原数据**：新 IFD0 追加到块尾，原有条目逐字照搬，只把头部那个
/// IFD0 指针改过去——原有值偏移与 Exif/GPS 子 IFD 指针因此全都还指得对；反过来
/// 把条目挪到前面重写会让这些偏移集体作废，得到一份能解析却全是错值的 EXIF。
/// 元数据全空时原样返回源块（源块也没有就给 `None`，导出不写 EXIF）。
/// 标题（`dc:title`）在 EXIF 里没有对应标签，只进 XMP 与视频容器。
pub fn with_metadata(src: Option<&[u8]>, meta: &crate::project::Metadata) -> Option<Vec<u8>> {
    let fields = meta_fields(meta);
    match src.filter(|b| !b.is_empty()) {
        Some(block) => {
            if fields.is_empty() {
                return Some(block.to_vec());
            }
            // 合并后超出 JPEG 单段上限就退回只写元数据的小块：宁少带源 EXIF，
            // 也不能写出一个装不进 APP1 的块。
            merge_block(block, &fields).or_else(|| Some(fresh_block(&fields)))
        }
        None => (!fields.is_empty()).then(|| fresh_block(&fields)),
    }
}

/// 要写进 IFD0 的标签：`(标签, 类型, 值字节)`，空的跳过（不往文件里塞空标签）。
/// 说明/创作者/版权是 ASCII（类型 2，带一个结尾 NUL）；标题没有 ASCII 对应标签，
/// 走 Windows 那套 `XPTitle`（类型 1，UCS2 小端 + 两个零字节结尾），资源管理器与
/// 多数看图软件都认。
fn meta_fields(meta: &crate::project::Metadata) -> Vec<(u16, u16, Vec<u8>)> {
    let c = meta.cleaned();
    let mut out: Vec<(u16, u16, Vec<u8>)> = Vec::new();
    if !c.title.is_empty() {
        let mut body: Vec<u8> = Vec::new();
        for unit in c.title.encode_utf16() {
            body.extend_from_slice(&unit.to_le_bytes());
        }
        body.extend_from_slice(&[0, 0]);
        out.push((TAG_XP_TITLE, 1, body));
    }
    for (tag, text) in [
        (TAG_DESCRIPTION, c.description),
        (TAG_ARTIST, c.creator),
        (TAG_COPYRIGHT, c.copyright),
    ] {
        if text.is_empty() {
            continue;
        }
        let mut body = text.into_bytes();
        body.push(0);
        out.push((tag, 2, body));
    }
    out
}

/// 新建一个只带元数据的小端 TIFF：头部 + IFD0 + 值区。
fn fresh_block(fields: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let (ifd, values) = assemble(Order::Little, &[], fields, 8);
    let mut out = Vec::with_capacity(8 + ifd.len() + values.len());
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend(ifd);
    out.extend(values);
    out
}

/// 在源块尾部另起一份 IFD0：原有数据一个字节不动，只改头部的 IFD0 指针。
fn merge_block(block: &[u8], fields: &[(u16, u16, Vec<u8>)]) -> Option<Vec<u8>> {
    let order = match block.get(0..2)? {
        b"II" => Order::Little,
        b"MM" => Order::Big,
        _ => return None,
    };
    let ifd0 = order.u32(block, 4)? as usize;
    let count = order.u16(block, ifd0)? as usize;
    // 条目多到这个数就说明块不是我们认识的 IFD，别在它后面续写。
    if count == 0 || count > 512 {
        return None;
    }
    let mut kept: Vec<[u8; 12]> = Vec::with_capacity(count);
    for i in 0..count {
        let at = ifd0 + 2 + 12 * i;
        let raw: [u8; 12] = block.get(at..at + 12)?.try_into().ok()?;
        let tag = order.u16(&raw, 0)?;
        // 同标签由新值接管：留着会一份 IFD 里出现两次，读的人挑哪条不一定。
        if fields.iter().any(|(t, _, _)| *t == tag) {
            continue;
        }
        kept.push(raw);
    }

    let mut out = block.to_vec();
    // TIFF 要求偏移落在偶数上，接着尾部续写前先补齐。
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let ifd_at = out.len();
    let (ifd, values) = assemble(order, &kept, fields, ifd_at);
    if out.len() + ifd.len() + values.len() > MAX_TIFF {
        return None;
    }
    out.extend(ifd);
    out.extend(values);
    put_u32(order, &mut out, 4, ifd_at as u32);
    Some(out)
}

/// 拼一份 IFD：`kept` 是照搬的原条目，`fields` 是新写的标签，合起来按标签升序
/// （TIFF 要求），返回 (IFD 字节, 值区字节)。`ifd_at` 是 IFD 在块里的偏移，
/// 外置值的偏移要按它算。
fn assemble(
    order: Order,
    kept: &[[u8; 12]],
    fields: &[(u16, u16, Vec<u8>)],
    ifd_at: usize,
) -> (Vec<u8>, Vec<u8>) {
    let total = kept.len() + fields.len();
    let mut values: Vec<u8> = Vec::new();
    let mut entries: Vec<(u16, [u8; 12])> = Vec::with_capacity(total);

    for raw in kept {
        let tag = order.u16(raw, 0).unwrap_or(0);
        entries.push((tag, *raw));
    }
    for (tag, typ, body) in fields {
        let mut entry = [0u8; 12];
        put_u16(order, &mut entry, 0, *tag);
        put_u16(order, &mut entry, 2, *typ);
        put_u32(order, &mut entry, 4, body.len() as u32);
        if body.len() <= 4 {
            entry[8..8 + body.len()].copy_from_slice(body);
        } else {
            let at = ifd_at + 2 + 12 * total + 4 + values.len();
            put_u32(order, &mut entry, 8, at as u32);
            values.extend_from_slice(body);
        }
        entries.push((*tag, entry));
    }
    entries.sort_by_key(|(tag, _)| *tag);

    // 长度是定死的（数量 2 + 条目 12n + next 指针 4），一次开够再按位置填。
    let mut ifd = vec![0u8; 2 + 12 * total + 4];
    put_u16(order, &mut ifd, 0, total as u16);
    for (i, (_, raw)) in entries.iter().enumerate() {
        ifd[2 + 12 * i..2 + 12 * i + 12].copy_from_slice(raw);
    }
    // 没有第二个 IFD（缩略图那一份），next 指针留在末尾写 0。
    (ifd, values)
}

/// 往缓冲区的 `at` 处按块自己的字节序写一个 u16；缓冲区不够长就补齐。
fn put_u16(order: Order, buf: &mut [u8], at: usize, v: u16) {
    let raw = match order {
        Order::Little => v.to_le_bytes(),
        Order::Big => v.to_be_bytes(),
    };
    buf[at..at + 2].copy_from_slice(&raw);
}

/// 同上写 u32。
fn put_u32(order: Order, buf: &mut [u8], at: usize, v: u32) {
    let raw = match order {
        Order::Little => v.to_le_bytes(),
        Order::Big => v.to_be_bytes(),
    };
    buf[at..at + 4].copy_from_slice(&raw);
}

/// JPEG 的 APP1 很小，整块进内存再解析；TIFF 走文件定位读。
enum Source<'a> {
    Mem(&'a [u8]),
    File(BufReader<File>),
}

impl Source<'_> {
    fn size(&self) -> u64 {
        match self {
            Self::Mem(d) => d.len() as u64,
            Self::File(f) => f.get_ref().metadata().map(|m| m.len()).unwrap_or(0),
        }
    }

    /// 取 `at..at+n`；越界、超大、读失败都是 `None`，不 panic 也不放大分配。
    fn bytes(&mut self, at: u64, n: usize) -> Option<Vec<u8>> {
        if n == 0 || n > MAX_BLOCK {
            return None;
        }
        let end = at.checked_add(n as u64)?;
        if end > self.size() {
            return None;
        }
        match self {
            Self::Mem(d) => {
                let start = usize::try_from(at).ok()?;
                let end = usize::try_from(end).ok()?;
                Some(d[start..end].to_vec())
            }
            Self::File(f) => {
                let mut buf = vec![0u8; n];
                f.seek(SeekFrom::Start(at)).ok()?;
                f.read_exact(&mut buf).ok()?;
                Some(buf)
            }
        }
    }
}

/// 扫 JPEG 段找带 `Exif\0\0` 头的 APP1。遇到 SOS 就停，后面是压缩数据。
///
/// APP1 也可能是 XMP 之类，所以不是 Exif 头的 APP1 要跳过继续找，不能一上来就收工。
fn app1_exif<R: Read + Seek>(f: &mut R) -> Option<Vec<u8>> {
    for _ in 0..128 {
        let mut mark = [0u8; 2];
        if f.read_exact(&mut mark).is_err() || mark[0] != 0xFF {
            return None;
        }
        let kind = mark[1];
        if kind == 0xDA {
            return None;
        }
        // 独立标记（无长度字段）与填充字节：直接看下一个。
        if kind == 0x00
            || kind == 0x01
            || kind == 0xD8
            || kind == 0xD9
            || (0xD0..=0xD7).contains(&kind)
        {
            continue;
        }
        let mut len = [0u8; 2];
        f.read_exact(&mut len).ok()?;
        let body = u16::from_be_bytes(len) as usize;
        if body < 2 {
            return None;
        }
        let body = body - 2;
        if kind != 0xE1 || body > MAX_APP1 {
            f.seek(SeekFrom::Current(body as i64)).ok()?;
            continue;
        }
        let mut seg = vec![0u8; body];
        f.read_exact(&mut seg).ok()?;
        if let Some(rest) = seg.strip_prefix(b"Exif\0\0" as &[u8]) {
            return Some(rest.to_vec());
        }
    }
    None
}

#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

impl Order {
    fn u16(self, d: &[u8], at: usize) -> Option<u16> {
        let raw: [u8; 2] = d.get(at..at + 2)?.try_into().ok()?;
        Some(match self {
            Self::Little => u16::from_le_bytes(raw),
            Self::Big => u16::from_be_bytes(raw),
        })
    }

    fn u32(self, d: &[u8], at: usize) -> Option<u32> {
        let raw: [u8; 4] = d.get(at..at + 4)?.try_into().ok()?;
        Some(match self {
            Self::Little => u32::from_le_bytes(raw),
            Self::Big => u32::from_be_bytes(raw),
        })
    }

    /// FLOAT 类型跟着 TIFF 的字节序走，不能直接 `f32::from_bits`（那是小端）。
    fn f32(self, d: &[u8], at: usize) -> Option<f64> {
        let raw: [u8; 4] = d.get(at..at + 4)?.try_into().ok()?;
        Some(match self {
            Self::Little => f32::from_le_bytes(raw),
            Self::Big => f32::from_be_bytes(raw),
        } as f64)
    }
}

/// 每个类型占几字节；不在表里的类型当不可读跳过。
fn unit_bytes(typ: u16) -> Option<usize> {
    match typ {
        1 | 2 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

/// 只解析内存里的 TIFF（JPEG 的 APP1 体、单测构造的字节）。
fn parse_mem(data: &[u8]) -> Option<Exif> {
    parse(&mut Source::Mem(data))
}

/// Windows 那组 `XP*` 标签（标题/备注/关键词/主题/作者），值是 UCS2 而不是 ASCII。
fn is_xp(tag: u16) -> bool {
    (0x9C9B..=0x9C9F).contains(&tag)
}

/// 走 IFD0 与 Exif 子 IFD。条目块整读一次，外置值按标签单独定位读。
fn parse(src: &mut Source) -> Option<Exif> {
    let head = src.bytes(0, 8)?;
    let order = match &head[..2] {
        b"II" => Order::Little,
        b"MM" => Order::Big,
        _ => return None,
    };
    // magic 白名单跟像素解码共用同一份（`raw::tiff::header_at`）。这里曾写死 `== 42`，
    // 于是松下 RW2（0x0055）、奥林巴斯 ORF（0x4F52）、尼康 NEF（0x0202）解得开像素却读不到
    // 方向，竖拍序列整段躺平。BigTIFF 照旧拒：它的偏移是 64 位，这层按 32 位读。
    if crate::raw::tiff::header_at(&head).is_err() {
        return None;
    }

    let mut exif = Exif::default();
    // ISO/快门/光圈几乎都在 Exif 子 IFD，只扫 IFD0 会读空。
    let mut queue = vec![u64::from(order.u32(&head, 4)?)];
    let mut visited: Vec<u64> = Vec::new();
    while let Some(off) = queue.pop() {
        if off < 8 || visited.contains(&off) || visited.len() >= 8 {
            continue;
        }
        visited.push(off);
        let Some(raw) = read_ifd(src, order, off) else {
            continue;
        };
        for chunk in raw.chunks_exact(12) {
            let tag = order.u16(chunk, 0)?;
            let typ = order.u16(chunk, 2).unwrap_or(0);
            let num = order.u32(chunk, 4).unwrap_or(0);
            if tag == TAG_EXIF_IFD {
                if let Some(next) = order.u32(chunk, 8) {
                    queue.push(u64::from(next));
                }
                continue;
            }
            match read_value(src, order, tag, typ, num, chunk) {
                Some(Value::Num(v)) => {
                    exif.numbers.insert(tag, v);
                }
                Some(Value::Text(s)) => {
                    exif.texts.insert(tag, s);
                }
                None => {}
            }
        }
    }
    // 只认落在 1~8 的整数值；别的写法宁可当没方向。
    exif.orientation = match exif.numbers.get(&TAG_ORIENTATION) {
        Some(v) if (1.0..=8.0).contains(v) && v.fract() == 0.0 => *v as u8,
        _ => 1,
    };
    Some(exif)
}

/// 取一个 IFD 的条目区（数量 + 条目本身，不含 next 指针）。
fn read_ifd(src: &mut Source, order: Order, off: u64) -> Option<Vec<u8>> {
    let count = order.u16(&src.bytes(off, 2)?, 0)?;
    if count == 0 {
        return Some(Vec::new());
    }
    src.bytes(off + 2, count.min(4096) as usize * 12)
}

enum Value {
    Num(f64),
    Text(String),
}

/// 值域：总量不超过 4 字节的内联在条目里，否则当偏移去读。
/// 多值只取第一个，所以有理数固定读 8 字节、文本按 `MAX_TEXT` 收口。
fn read_value(
    src: &mut Source,
    order: Order,
    tag: u16,
    typ: u16,
    num: u32,
    chunk: &[u8],
) -> Option<Value> {
    if num == 0 {
        return None;
    }
    let unit = unit_bytes(typ)?;
    let inline = unit
        .checked_mul(num as usize)
        .is_some_and(|total| total <= 4);
    let raw: Vec<u8> = if inline {
        chunk.get(8..12)?.to_vec()
    } else {
        let at = u64::from(order.u32(chunk, 8)?);
        match typ {
            2 => src.bytes(at, num.min(MAX_TEXT as u32) as usize)?,
            // XP* 是 UCS2 字节数组，得整段读回来才能解码；按 1 字节读只会拿到半个字符。
            1 if is_xp(tag) => src.bytes(at, unit.checked_mul(num as usize)?.min(MAX_TEXT * 2))?,
            5 | 10 => src.bytes(at, 8)?,
            _ => src.bytes(at, unit)?,
        }
    };
    let raw = raw.as_slice();
    match typ {
        // XP*：UCS2 小端，读到第一对零为止（内联值尾部的填充零一并吃掉）。
        1 if is_xp(tag) => {
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            let text = String::from_utf16_lossy(&units[..end]).trim().to_owned();
            (!text.is_empty()).then_some(Value::Text(text))
        }
        // ASCII：截到第一个 NUL。出现控制字符就当读不到，别把二进制当机身名。
        2 => {
            let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            let text = std::str::from_utf8(&raw[..end]).ok()?;
            if text.is_empty() || text.chars().any(|c| c.is_control()) {
                None
            } else {
                Some(Value::Text(text.trim().to_owned()))
            }
        }
        1 => Some(Value::Num(*raw.first()? as f64)),
        // 有些写入器把曝光时间/光圈错写成两个 SHORT 当分数对（Python 基准同款兼容）。
        3 if num == 2 && matches!(tag, TAG_EXPOSURE | TAG_FNUM) => {
            let den = order.u16(raw, 2)?;
            if den == 0 {
                return None;
            }
            Some(Value::Num(order.u16(raw, 0)? as f64 / den as f64))
        }
        3 => Some(Value::Num(order.u16(raw, 0)? as f64)),
        4 => Some(Value::Num(order.u32(raw, 0)? as f64)),
        9 => Some(Value::Num(order.u32(raw, 0)? as i32 as f64)),
        11 => order.f32(raw, 0).map(Value::Num),
        5 => read_rational(order, raw, false).map(Value::Num),
        10 => read_rational(order, raw, true).map(Value::Num),
        _ => None,
    }
}

fn read_rational(order: Order, raw: &[u8], signed: bool) -> Option<f64> {
    let num = if signed {
        order.u32(raw, 0)? as i32 as f64
    } else {
        order.u32(raw, 0)? as f64
    };
    let den = order.u32(raw, 4)?;
    (den != 0).then(|| num / den as f64)
}

/// 按 EXIF 方向把图转正。1 或越界原样返回，不复制。
///
/// 五个解码路径共用这一处，方向的语义只有一份。
pub fn oriented(img: image::DynamicImage, orientation: u8) -> image::DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        // 5/7 是对角镜像：image 没有 transpose，等价写法是转 90° 再翻一次。
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate90().flipv(),
        8 => img.rotate270(),
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16b(v: u16) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }

    /// 一条 12 字节的 IFD 记录：tag、类型、数量、值域（内联值本身或外置数据偏移）。
    fn entry(tag: u16, typ: u16, num: u32, value: u32) -> Vec<u8> {
        let mut out = u16b(tag);
        out.extend(u16b(typ));
        out.extend(num.to_le_bytes());
        out.extend(value.to_le_bytes());
        assert_eq!(out.len(), 12);
        out
    }

    /// 造一个 IFD0 直接从偏移 8 开始的 TIFF；外置值由调用方接在 `tail` 里。
    fn ifd0(entries: &[Vec<u8>], tail: &[u8]) -> Vec<u8> {
        let mut out = b"II".to_vec();
        out.extend(u16b(42));
        out.extend(8u32.to_le_bytes());
        out.extend(u16b(entries.len() as u16));
        for e in entries {
            out.extend(e);
        }
        out.extend(0u32.to_le_bytes());
        assert_eq!(out.len(), 8 + 2 + entries.len() * 12 + 4);
        out.extend(tail);
        out
    }

    /// 同 `ifd0`，但 magic 由调用方指定：厂商 RAW 用的不是 42（松下 RW2 写 0x0055、
    /// 奥林巴斯 ORF 写 0x4F52、尼康 NEF 写 0x0202），这一族头以前会被这里整段拒掉。
    fn ifd0_magic(magic: u16, orientation: u32) -> Vec<u8> {
        let mut out = b"II".to_vec();
        out.extend(u16b(magic));
        out.extend(8u32.to_le_bytes());
        out.extend(u16b(1));
        out.extend(entry(TAG_ORIENTATION, 3, 1, orientation));
        out.extend(0u32.to_le_bytes());
        out
    }

    /// 带 Exif 子 IFD 的完整样例：IFD0 放 Model，子 IFD 放曝光时间与 ISO。
    fn tiff_with_subifd() -> Vec<u8> {
        // Model 有 6 字节（含 NUL），超过 4 字节必须外置；偏移按各段长度依次算。
        let text = b"DC-G9\0";
        let sub_at = 8 + 2 + 2 * 12 + 4;
        let rational_at = sub_at + 2 + 2 * 12 + 4;
        let text_at = rational_at + 8;
        let mut file = ifd0(
            &[
                entry(TAG_MODEL, 2, text.len() as u32, text_at as u32),
                entry(TAG_EXIF_IFD, 4, 1, sub_at as u32),
            ],
            &[],
        );
        file.extend(u16b(2));
        file.extend(entry(TAG_EXPOSURE, 5, 1, rational_at as u32));
        file.extend(entry(TAG_ISO, 3, 1, 400));
        file.extend(0u32.to_le_bytes());
        file.extend(1u32.to_le_bytes());
        file.extend(200u32.to_le_bytes());
        file.extend(text);
        assert_eq!(file.len(), text_at + text.len());
        file
    }

    fn write_temp(tag: &str, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aevum-exif-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("写临时文件");
        path
    }

    #[test]
    fn read_from_disk_handles_tiff_jpeg_and_big_offsets() {
        let tiff = write_temp(
            "disk",
            "a.tif",
            &ifd0(&[entry(TAG_ORIENTATION, 3, 1, 8)], &[]),
        );
        assert_eq!(read(&tiff).orientation, 8);

        let mut jpeg = b"\xFF\xD8".to_vec();
        let mut body = b"Exif\0\0".to_vec();
        body.extend(ifd0(&[entry(TAG_ORIENTATION, 3, 1, 6)], &[]));
        let len = (body.len() + 2) as u16;
        jpeg.extend([0xFF, 0xE1]);
        jpeg.extend(len.to_be_bytes());
        jpeg.extend(body);
        jpeg.extend([0xFF, 0xDA, 0x00, 0x02]);
        let jpeg = write_temp("disk", "b.jpg", &jpeg);
        assert_eq!(read(&jpeg).orientation, 6);

        // 写入器把 IFD 放在图像数据之后（大 TIFF 常见）：定位读要能找到，
        // 而不是靠「读文件头几十 MB」蒙对。这里用 MM 顺带测大端。
        let mut late = b"MM".to_vec();
        late.extend(42u16.to_be_bytes());
        let ifd_at: u32 = 8 + 1024 * 1024;
        late.extend(ifd_at.to_be_bytes());
        late.resize(ifd_at as usize, 0xAA);
        late.extend(1u16.to_be_bytes());
        late.extend(TAG_ORIENTATION.to_be_bytes());
        late.extend(3u16.to_be_bytes());
        late.extend(1u32.to_be_bytes());
        // 大端的 SHORT 内联值是左对齐的：数值 3 落在前两字节。
        late.extend([0, 3, 0, 0]);
        late.extend(0u32.to_be_bytes());
        let late = write_temp("disk", "late.tif", &late);
        assert_eq!(read(&late).orientation, 3, "IFD 靠后也要读到");

        // 不是图片：中性值，不报错。
        let junk = write_temp("disk", "c.bin", b"RIFF____");
        assert_eq!(read(&junk), Exif::default());
    }

    #[test]
    fn orientation_out_of_range_is_neutral() {
        let file = ifd0(&[entry(TAG_ORIENTATION, 3, 1, 6)], &[]);
        assert_eq!(parse_mem(&file).unwrap().orientation, 6);

        let bogus = ifd0(&[entry(TAG_ORIENTATION, 3, 1, 11)], &[]);
        assert_eq!(parse_mem(&bogus).unwrap().orientation, 1);

        assert_eq!(parse_mem(b"nope").unwrap_or_default().orientation, 1);
    }

    /// 厂商 RAW 的 magic 必须照样读到方向。这条是回归钉子：EXIF 那侧曾经写死
    /// `magic == 42`，于是松下 RW2 竖拍序列解得开像素、读不出 orientation，整段躺平显示。
    /// 白名单现在与 `raw::tiff::header_at` 共用一份，别再退回两套判定。
    #[test]
    fn vendor_magics_still_yield_orientation() {
        for magic in [42u16, 0x0202, 0x4F52, 0x0055] {
            let file = ifd0_magic(magic, 8);
            assert_eq!(
                parse_mem(&file).unwrap_or_default().orientation,
                8,
                "magic {magic:#06x} 该读到方向 8"
            );
        }
    }

    /// BigTIFF/RRIF 仍然不认：偏移是 64 位、IFD 计数是 8 字节，按 32 位读会读出假方向，
    /// 那比读不到更糟（宁可不转，也别把图转歪）。
    #[test]
    fn bigtiff_magic_is_still_refused() {
        for magic in [43u16, 0x0255, 0x5555] {
            let file = ifd0_magic(magic, 8);
            assert_eq!(
                parse_mem(&file).unwrap_or_default().orientation,
                1,
                "magic {magic:#06x} 是 BigTIFF 家族，不该按 32 位读"
            );
        }
    }

    #[test]
    fn follows_exif_subifd_for_rational_and_text() {
        let exif = parse_mem(&tiff_with_subifd()).expect("能解析");
        assert_eq!(exif.model(), Some("DC-G9"));
        assert_eq!(exif.iso(), Some(400.0));
        assert_eq!(exif.exposure_sec(), Some(0.005));
        assert_eq!(exif.shutter_text().as_deref(), Some("1/200"));
        assert_eq!(exif.camera_line().as_deref(), Some("DC-G9"));
    }

    #[test]
    fn truncated_or_bogus_offsets_never_panic() {
        let whole = tiff_with_subifd();
        for cut in [0, 4, 8, 20, 40, 60] {
            let _ = parse_mem(&whole[..cut]);
        }
        // 曝光时间分母为 0：当读不到，不能变成 inf 或除零。
        // 一条记录的 IFD 占 26 字节，所以外置值前垫 4 字节让有理数落在偏移 30。
        let mut tail = vec![0u8; 4];
        tail.extend(5u32.to_le_bytes());
        tail.extend(0u32.to_le_bytes());
        let exif = parse_mem(&ifd0(&[entry(TAG_EXPOSURE, 5, 1, 30)], &tail)).expect("能解析");
        assert_eq!(exif.exposure_sec(), None);

        // 偏移指向文件外、数量吹成天文数字：只给 Err 级的「读不到」，不吃内存。
        let far = ifd0(&[entry(TAG_ISO, 3, 1_000_000, 4_000_000_000)], &[]);
        assert_eq!(parse_mem(&far).unwrap().iso(), None);
        let lie = ifd0(&[entry(TAG_MODEL, 2, 6, 999_999)], &[]);
        assert_eq!(parse_mem(&lie).unwrap().model(), None);
    }

    #[test]
    fn exposure_written_as_two_shorts_reads_as_a_fraction() {
        // 有的写入器把 1/200 写成两个 SHORT（分子在前）：值域 4 字节 = [1,0,200,0]。
        let file = ifd0(
            &[entry(
                TAG_EXPOSURE,
                3,
                2,
                u32::from_le_bytes([1, 0, 200, 0]),
            )],
            &[],
        );
        assert_eq!(parse_mem(&file).unwrap().exposure_sec(), Some(0.005));
    }

    #[test]
    fn ev_needs_all_three_legs() {
        let mut exif = Exif::default();
        assert_eq!(exif.ev(), None);
        exif.numbers.insert(TAG_ISO, 400.0);
        exif.numbers.insert(TAG_EXPOSURE, 0.005);
        assert_eq!(exif.ev(), None, "缺光圈不猜");
        exif.numbers.insert(TAG_FNUM, 2.0);
        // log2(4) - log2(1/200) + 2*log2(2) = 2 + 7.6439 + 2
        let ev = exif.ev().expect("三要素齐了");
        assert!((ev - (2.0 + (200.0f64).log2() + 2.0)).abs() < 1e-9, "{ev}");
    }

    #[test]
    fn apex_tv_and_av_fill_missing_exposure() {
        // 机身只写 APEX 的样例：TV 7.643856 = 1/200，AV 2.97 = f/2.8 的常规写法。
        let mut exif = Exif::default();
        exif.numbers.insert(TAG_TV, 7.643856);
        exif.numbers.insert(TAG_AV, 2.97);
        assert!((exif.exposure_sec().unwrap() - 0.005).abs() < 1e-6);
        assert!((exif.fnum().unwrap() - 2.8).abs() < 1e-3);
        // 超出合理范围的 APEX 不当曝光用：垃圾值会让 EV 飘到几十档。
        let mut junk = Exif::default();
        junk.numbers.insert(TAG_TV, 4096.0);
        assert_eq!(junk.exposure_sec(), None);
    }

    #[test]
    fn orientation_matches_the_python_oracle() {
        // 2 行 3 列的标号图：期望值按 imio.apply_orientation 的数组语义手推。
        // src(x,y) = 1 + x + 3y → 第一行 1 2 3，第二行 4 5 6。
        let img =
            image::RgbImage::from_fn(3, 2, |x, y| image::Rgb([1 + x as u8 + 3 * y as u8, 0, 0]));
        let src = image::DynamicImage::ImageRgb8(img);
        let at = |im: &image::DynamicImage, x: u32, y: u32| -> u8 {
            im.as_rgb8().unwrap().get_pixel(x, y).0[0]
        };
        // o=5 transpose：dst[y][x] = src[x][y]
        let t = oriented(src.clone(), 5);
        assert_eq!((t.width(), t.height()), (2, 3));
        assert_eq!([at(&t, 0, 0), at(&t, 1, 0), at(&t, 0, 1)], [1, 4, 2]);
        // o=6 顺时针 90°
        let cw = oriented(src.clone(), 6);
        assert_eq!((cw.width(), cw.height()), (2, 3));
        assert_eq!([at(&cw, 0, 0), at(&cw, 1, 0), at(&cw, 1, 2)], [4, 1, 3]);
        // o=7 transpose 再双向翻转：dst[y][x] = src[H-1-y][W-1-x]
        let diag = oriented(src.clone(), 7);
        assert_eq!((diag.width(), diag.height()), (2, 3));
        assert_eq!(
            [at(&diag, 0, 0), at(&diag, 1, 0), at(&diag, 0, 1)],
            [6, 3, 5]
        );
        // o=8 逆时针 90°：左上角走到左下
        let ccw = oriented(src.clone(), 8);
        assert_eq!([at(&ccw, 0, 2), at(&ccw, 1, 0), at(&ccw, 0, 0)], [1, 6, 3]);
        // o=3 只翻不改轴向
        let half = oriented(src.clone(), 3);
        assert_eq!((half.width(), half.height()), (3, 2));
        assert_eq!([at(&half, 0, 0), at(&half, 2, 1)], [6, 1]);
        assert_eq!(at(&oriented(src, 1), 0, 0), 1);
    }

    #[test]
    fn jpeg_app1_is_found_past_the_thumbnail_free_segments() {
        // FFD8 + APP0(短) + DQT + XMP 的 APP1 + Exif 的 APP1：前面全要跳过。
        let mut jpeg = b"\xFF\xD8".to_vec();
        jpeg.extend([0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        jpeg.extend([0xFF, 0xDB, 0x00, 0x03, 0x00]);
        let xmp = b"http://ns.adobe.com/xap/1.0/".to_vec();
        let len = (xmp.len() + 2) as u16;
        jpeg.extend([0xFF, 0xE1]);
        jpeg.extend(len.to_be_bytes());
        jpeg.extend(xmp);
        let mut body = b"Exif\0\0".to_vec();
        body.extend(ifd0(&[entry(TAG_ORIENTATION, 3, 1, 8)], &[]));
        let len = (body.len() + 2) as u16;
        jpeg.extend([0xFF, 0xE1]);
        jpeg.extend(len.to_be_bytes());
        jpeg.extend(body);
        jpeg.extend([0xFF, 0xDA, 0x00, 0x02]);

        let mut cur = std::io::Cursor::new(jpeg);
        cur.set_position(2);
        let found = app1_exif(&mut cur).expect("跳过 XMP 找到 Exif APP1");
        assert_eq!(parse_mem(&found).unwrap().orientation, 8);
    }

    // ------------------------------------------------------------ 元数据写入

    fn meta() -> crate::project::Metadata {
        crate::project::Metadata {
            title: "云海日出".to_owned(),
            description: "松下 G9 延时序列\n 第二场".to_owned(),
            creator: "张三".to_owned(),
            copyright: "© 2026 张三".to_owned(),
        }
    }

    #[test]
    fn metadata_block_roundtrips() {
        let block = with_metadata(None, &meta()).expect("新建块");
        let back = read_block(&block);
        assert_eq!(back.title(), Some("云海日出"), "XPTitle 没写进去");
        assert_eq!(
            back.description(),
            Some("松下 G9 延时序列 第二场"),
            "换行该被折成空格"
        );
        assert_eq!(back.artist(), Some("张三"));
        assert_eq!(back.copyright(), Some("© 2026 张三"));
    }

    #[test]
    fn empty_metadata_leaves_the_block_alone() {
        assert!(
            with_metadata(None, &Default::default()).is_none(),
            "没填元数据不该凭空造块"
        );
        // 一条 Make 外置在偏移 26 的最小源块。
        let src = ifd0(&[entry(TAG_MAKE, 2, 6, 26)], b"LUMEN\0");
        assert_eq!(with_metadata(Some(&src), &Default::default()).unwrap(), src);
    }

    #[test]
    fn merge_keeps_source_entries_and_subifd_offsets() {
        let src = tiff_with_subifd();
        let block = with_metadata(Some(&src), &meta()).expect("合并块");
        // 原数据区一个字节没动，只有头部的 IFD0 指针改指到尾部新表。
        assert_eq!(&block[..4], &src[..4]);
        assert_eq!(&block[8..src.len()], &src[8..], "重排了原数据区");
        assert!(block.len() > src.len(), "新 IFD 该接在尾部");
        let back = read_block(&block);
        assert_eq!(back.model(), Some("DC-G9"), "原条目丢了");
        assert_eq!(back.exposure_sec(), Some(0.005), "子 IFD 指针被作废");
        assert_eq!(back.iso(), Some(400.0));
        assert_eq!(back.artist(), Some("张三"));
    }

    #[test]
    fn merge_follows_source_byte_order() {
        // 大端源块：IFD0 一条 Make = "LUMEN"，值外置在偏移 26。
        let mut src = b"MM".to_vec();
        src.extend(42u16.to_be_bytes());
        src.extend(8u32.to_be_bytes());
        src.extend(1u16.to_be_bytes());
        src.extend(TAG_MAKE.to_be_bytes());
        src.extend(2u16.to_be_bytes());
        src.extend(6u32.to_be_bytes());
        src.extend(26u32.to_be_bytes());
        src.extend(0u32.to_be_bytes());
        src.extend(b"LUMEN\0");

        let block = with_metadata(Some(&src), &meta()).expect("大端合并");
        assert_eq!(&block[..2], b"MM", "字节序被改了");
        let back = read_block(&block);
        assert_eq!(back.make(), Some("LUMEN"));
        assert_eq!(back.copyright(), Some("© 2026 张三"));
    }

    #[test]
    fn merge_replaces_the_tags_it_writes() {
        // 源块自己带一条 Artist：合并后只能剩一条，而且是新值。
        let text = b"OLD\0";
        let at = 8 + 2 + 12 + 4;
        let src = ifd0(&[entry(TAG_ARTIST, 2, text.len() as u32, at as u32)], text);
        let block = with_metadata(Some(&src), &meta()).expect("合并");
        assert_eq!(read_block(&block).artist(), Some("张三"));

        let ifd_at = u32::from_le_bytes(block[4..8].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(block[ifd_at..ifd_at + 2].try_into().unwrap()) as usize;
        let tags: Vec<u16> = (0..count)
            .map(|i| {
                let e = ifd_at + 2 + 12 * i;
                u16::from_le_bytes(block[e..e + 2].try_into().unwrap())
            })
            .collect();
        assert_eq!(tags.iter().filter(|t| **t == TAG_ARTIST).count(), 1);
        assert_eq!(
            tags,
            {
                let mut sorted = tags.clone();
                sorted.sort_unstable();
                sorted
            },
            "条目必须按标签升序"
        );
    }

    #[test]
    fn long_values_are_cut_to_what_the_reader_takes() {
        let long = crate::project::Metadata {
            description: "长".repeat(900),
            ..Default::default()
        };
        let block = with_metadata(None, &long).expect("块");
        assert!(block.len() <= MAX_TIFF, "块超出 JPEG 单段上限");
        let back = read_block(&block).description().unwrap().to_owned();
        // 截在字符边界上：多字节字符被切一半会让整段 UTF-8 解不出来。
        assert_eq!(back, long.cleaned().description);
        assert!(back.len() <= 480);
        assert!(!back.is_empty());
    }

    #[test]
    fn oversized_source_falls_back_to_metadata_only() {
        // 源块塞满到超过 APP1 能装的程度：合并装不下，退回只写元数据的小块。
        let text = vec![b'x'; 66_000];
        let at = 8 + 2 + 12 + 4;
        let src = ifd0(
            &[entry(TAG_DESCRIPTION, 2, text.len() as u32, at as u32)],
            &text,
        );
        let block = with_metadata(Some(&src), &meta()).expect("退回小块");
        assert!(block.len() < src.len(), "没退回小块");
        assert!(block.len() <= MAX_TIFF);
        assert_eq!(read_block(&block).artist(), Some("张三"));
    }

    #[test]
    fn garbage_source_does_not_panic() {
        for src in [&b""[..], &b"XX"[..], &[0, 1, 2, 3, 4, 5, 6, 7][..]] {
            let _ = with_metadata(Some(src), &meta());
        }
    }
}
