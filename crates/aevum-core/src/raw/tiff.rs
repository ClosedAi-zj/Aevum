//! 通用 TIFF 层：厂商 magic、IFD 链、SubIFD、标记取值。
//!
//! 这是项目里第三份 TIFF 解析，和前两份的分工是刻意的：
//!
//! - `exif::` 面向 JPEG 边车，语义是「读不到就给中性值」，从不报错；
//! - `dng::Tiff` 只看 IFD0、只认 magic 42，语义是「不像 DNG 就当场拒掉」；
//! - 这边要认 `II`+`RO`（Olympus ORF/Leica RWL）、`II`+`0x0202`（Nikon NEF）、
//!   `II`+`0x0055`（松下 RW2）这类非 42 的头，还要顺着 IFD 链与 SubIFD（330）把所有候选 IFD 摊开——厂商文件的主图
//!   常常不在 IFD0，只读 IFD0 的 `dng::Tiff` 在这里会认错图层。
//!
//! 三层合并的代价是必须挑一套折中的错误语义，要么稀释 DNG 的严格性、要么把 EXIF 的
//! 容错变成报错，所以宁可各写一份、各自约束。
//!
//! 输入按敌意数据处理（用户会把任何来源的文件改个后缀喂进来）：每个偏移与长度都过
//! `range`/`get`，越界只会变成错误值，不会 panic，也不会静默少读一截。
//!
//! Generic TIFF layer: vendor magics, the IFD chain, SubIFDs, tag value access.
//!
//! This is the third TIFF parser in the project, and the split from the other two is
//! deliberate:
//!
//! - `exif::` serves JPEG sidecars; its convention is to return a neutral value when a tag
//!   cannot be read, and never to error;
//! - `dng::Tiff` reads IFD0 only and accepts magic 42 only; its convention is to reject a
//!   file on the spot when it does not look like DNG;
//! - this layer accepts non-42 headers: `II`+`RO` (Olympus ORF/Leica RWL), `II`+`0x0202`
//!   (Nikon NEF), `II`+`0x0055` (Panasonic RW2). It also walks the IFD chain and SubIFDs
//!   (330) to enumerate every candidate IFD: the main image is often not in IFD0, so the
//!   IFD0-only `dng::Tiff` picks the wrong layer here.
//!
//! Merging the three layers would force one compromise error convention: either DNG's
//! strictness is diluted or EXIF's tolerance becomes an error. A separate parser per layer,
//! each with its own constraints, is the cheaper option.
//!
//! Input is treated as hostile (a file from any source arrives with a renamed extension):
//! every offset and length goes through `range`/`get`, so an out-of-range read yields an
//! error value instead of a panic, and nothing is silently truncated.

use std::collections::VecDeque;

use super::RawError;

/// 单个标记值（条带数组、SubIFD 数组、CFA 图案）的字节上限。
const MAX_VALUE_BYTES: usize = 4 << 20;
/// 一次最多摊开这么多 IFD（链 + SubIFD 合并计数）。真实机身几十层封顶，
/// 上限挡的是自指链与伪造的 SubIFD 数组。
const MAX_IFDS: usize = 256;
/// 一个 SubIFD 数组最多取多少个偏移。
const MAX_SUBIFDS: usize = 128;
/// IFD 入口只认这两个下限：0~7 是头部，指向头部说明是坏文件。
const MIN_IFD_OFFSET: usize = 8;

/// SubIFD 标记（TIFF 6.0 的正统编号）。主图经常挂在这里而不是 IFD0。
const TAG_SUB_IFDS: u16 = 330;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Little,
    Big,
}

impl Order {
    fn u16(self, b: &[u8]) -> u16 {
        match self {
            Self::Little => u16::from_le_bytes([b[0], b[1]]),
            Self::Big => u16::from_be_bytes([b[0], b[1]]),
        }
    }

    fn u32(self, b: &[u8]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            Self::Big => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        }
    }
}

/// 头部自称的容器种类。报错时要能指名道姓地说「这个 magic 我认/不认」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Header {
    /// 正统 TIFF：`II`/`MM` + 42。
    Classic,
    /// Canon CR2：TIFF 头 + 偏移 8 处的 ASCII `CR`。
    CanonCr2,
    /// Nikon NEF：magic `0x0202`。
    NikonNef,
    /// `II` + `RO`：Olympus ORF/Leica RWL 共用这一族头。
    RoMagic,
    /// 松下 RW2：`II` + `0x0055`，首个 IFD 固定在 0x18（头里那 16 字节是机身序列号）。
    Rw2,
}

pub(crate) struct Tiff<'a> {
    d: &'a [u8],
    pub(crate) order: Order,
    pub(crate) header: Header,
}

pub(crate) struct Entry<'a> {
    pub(crate) typ: u16,
    pub(crate) data: &'a [u8],
}

fn unit_bytes(typ: u16) -> Option<usize> {
    match typ {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

fn out_of_range(start: u64, len: u64) -> RawError {
    RawError::StripOutOfRange {
        offset: start,
        length: len,
    }
}

/// 定长单元迭代；尾巴凑不齐一个单元就是坏数据，不静默截断。
fn walk<T, F>(data: &[u8], size: usize, mut f: F) -> Result<Vec<T>, RawError>
where
    F: FnMut(&[u8]) -> T,
{
    // `size` 只来自内部常量（2/4/8），这里仍按 0 收口：`size` 为 0 时
    // `is_multiple_of` 会直接 panic，所以先短路掉。
    if size == 0 || !data.len().is_multiple_of(size) {
        return Err(RawError::Truncated);
    }
    Ok(data.chunks(size).map(&mut f).collect())
}

/// 认头认到 magic 这一层，返回字节序与容器种类。
///
/// 像素解码（`Tiff::parse`）与 EXIF 读取（`crate::exif`）必须共用这一份白名单：两边各写
/// 一套的话，厂商 RAW 就会出现「像素解得开、方向读不出」的裂缝——松下 RW2 的竖拍图整段
/// 躺平就是这么来的（magic `0x0055` 被 EXIF 那侧写死的 `== 42` 拒了，方向退回 1、不转）。
pub(crate) fn header_at(d: &[u8]) -> Result<(Order, Header), RawError> {
    let head = d.get(0..8).ok_or(RawError::NotTiff)?;
    let order = match &head[0..2] {
        b"II" => Order::Little,
        b"MM" => Order::Big,
        _ => return Err(RawError::NotTiff),
    };
    let magic = order.u16(&head[2..4]);
    let header = match magic {
        // CR2 的原始数据在后面的 IFD 里，头本身是合法 TIFF，所以靠偏移 8 的 `CR` 认领。
        // 只递进 8 字节头部的调用方（EXIF 那侧）不会命中这条，落进 `Classic`，语义不变。
        42 if order == Order::Little && d.get(8..10) == Some(b"CR") => Header::CanonCr2,
        42 => Header::Classic,
        0x0202 => Header::NikonNef,
        // `RO` 两个字符按小端读就是 0x4F52；大端写法没在野过，落到 `BadMagic`。
        0x4F52 if order == Order::Little => Header::RoMagic,
        // 松下 RW2 把版本号写成 0x55（`II` + `U\0`）。大端的 0x255 才是 BigTIFF，
        // 所以这条只认小端，别把两种容器混成一个 magic。
        0x0055 if order == Order::Little => Header::Rw2,
        43 | 0x0255 | 0x5555 => return Err(RawError::BigTiff(magic)),
        other => return Err(RawError::BadMagic(other)),
    };
    Ok((order, header))
}

impl<'a> Tiff<'a> {
    /// 认头。BigTIFF（magic 43）与 RRIF（`II`+`UU`）单独给具名错误：它们的偏移是 64 位、
    /// 还要读 8 字节的 IFD 计数，跟这层的 32 位假设混在一起只会让两个语义都变模糊。
    pub(crate) fn parse(d: &'a [u8]) -> Result<Self, RawError> {
        let (order, header) = header_at(d)?;
        Ok(Self { d, order, header })
    }

    fn u16(&self, at: usize) -> Result<u16, RawError> {
        let b = self.d.get(at..at + 2).ok_or(RawError::Truncated)?;
        Ok(self.order.u16(b))
    }

    fn u32(&self, at: usize) -> Result<u32, RawError> {
        let b = self.d.get(at..at + 4).ok_or(RawError::Truncated)?;
        Ok(self.order.u32(b))
    }

    /// 唯一的取字节口子：全 checked 算术，坏偏移/长度只会得到 `StripOutOfRange`。
    pub(crate) fn range(&self, start: u64, len: u64) -> Result<&'a [u8], RawError> {
        let bad = || out_of_range(start, len);
        let start = usize::try_from(start).map_err(|_| bad())?;
        let len = usize::try_from(len).map_err(|_| bad())?;
        let end = start.checked_add(len).ok_or_else(bad)?;
        self.d.get(start..end).ok_or_else(bad)
    }

    /// 链 + SubIFD 广度展开，返回**去重后**的全部候选 IFD 偏移。
    ///
    /// 成环的链在这里自然终止（见过的偏移不再入队），所以不需要额外的环检测报错；
    /// 但总数超限说明这个文件的 IFD 结构不是相机写的，直接拒。
    pub(crate) fn candidates(&self) -> Result<Vec<usize>, RawError> {
        let first = self.ifd0()?;
        let mut seen: Vec<usize> = Vec::new();
        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(first);
        while let Some(ifd) = queue.pop_front() {
            if seen.len() >= MAX_IFDS {
                return Err(RawError::TooManyIfds(MAX_IFDS));
            }
            if seen.contains(&ifd) {
                continue;
            }
            seen.push(ifd);
            if let Some(next) = self.next_ifd(ifd)? {
                queue.push_back(next);
            }
            for sub in self.subifds(ifd)? {
                queue.push_back(sub);
            }
        }
        Ok(seen)
    }

    fn ifd0(&self) -> Result<usize, RawError> {
        let off = self.u32(4)? as usize;
        if off < MIN_IFD_OFFSET {
            return Err(RawError::Truncated);
        }
        Ok(off)
    }

    /// 当前 IFD 之后的下一个 IFD 偏移；0 表示链到头。
    fn next_ifd(&self, ifd: usize) -> Result<Option<usize>, RawError> {
        let at = self.ifd_end(ifd)?;
        let next = self.u32(at)?;
        Ok(usize::try_from(next).ok().filter(|v| *v >= MIN_IFD_OFFSET))
    }

    /// IFD 尾指针的位置：计数 + 12 字节一项。溢出与越界都收在 `Truncated`。
    fn ifd_end(&self, ifd: usize) -> Result<usize, RawError> {
        let entries = u64::from(self.u16(ifd)?)
            .checked_mul(12)
            .ok_or(RawError::Truncated)?;
        let tail = (ifd as u64)
            .checked_add(2)
            .and_then(|v| v.checked_add(entries))
            .ok_or(RawError::Truncated)?;
        // 尾指针本身要占 4 字节，落在文件里才算合法 IFD。
        let end = tail.checked_add(4).ok_or(RawError::Truncated)?;
        if end > self.d.len() as u64 {
            return Err(RawError::Truncated);
        }
        usize::try_from(tail).map_err(|_| RawError::Truncated)
    }

    /// 这个 IFD 声明的子 IFD 列表。类型只认 LONG/SHORT，别的写法一律当没有。
    fn subifds(&self, ifd: usize) -> Result<Vec<usize>, RawError> {
        let Some(entry) = self.entry(ifd, TAG_SUB_IFDS)? else {
            return Ok(Vec::new());
        };
        if !matches!(entry.typ, 3 | 4) {
            return Err(RawError::BadTag(TAG_SUB_IFDS));
        }
        let values = self.scalars(&entry, TAG_SUB_IFDS)?;
        if values.len() > MAX_SUBIFDS {
            return Err(RawError::TooManyIfds(MAX_SUBIFDS));
        }
        let limit = self.d.len() as u64;
        Ok(values
            .iter()
            .filter_map(|v| usize::try_from(*v).ok())
            .filter(|at| *at >= MIN_IFD_OFFSET && (*at as u64) < limit)
            .collect())
    }

    /// IFD 线性扫描找标记：只有几十项，不值得建表。重复标记取第一个。
    pub(crate) fn entry(&self, ifd: usize, tag: u16) -> Result<Option<Entry<'a>>, RawError> {
        let count = self.u16(ifd)? as usize;
        for i in 0..count {
            let at = ifd
                .checked_add(2)
                .and_then(|v| v.checked_add(i.checked_mul(12)?))
                .ok_or(RawError::Truncated)?;
            let rec = self.d.get(at..at + 12).ok_or(RawError::Truncated)?;
            if self.order.u16(&rec[0..2]) != tag {
                continue;
            }
            let typ = self.order.u16(&rec[2..4]);
            let count = self.order.u32(&rec[4..8]);
            let unit = unit_bytes(typ).ok_or(RawError::BadTag(tag))?;
            let total = unit
                .checked_mul(count as usize)
                .ok_or(RawError::BadTag(tag))?;
            let data = if total <= 4 {
                // 短值直接放在偏移字段里，只取实际长度。
                rec.get(8..8 + total).ok_or(RawError::BadTag(tag))?
            } else {
                if total > MAX_VALUE_BYTES {
                    return Err(RawError::BadTag(tag));
                }
                let at = u64::from(self.order.u32(&rec[8..12]));
                self.range(at, total as u64)
                    .map_err(|_| out_of_range(at, total as u64))?
            };
            return Ok(Some(Entry { typ, data }));
        }
        Ok(None)
    }

    pub(crate) fn required(&self, ifd: usize, tag: u16) -> Result<Entry<'a>, RawError> {
        self.entry(ifd, tag)?.ok_or(RawError::MissingTag(tag))
    }

    /// 有没有这个标记（不取值），用于挑主图候选。
    pub(crate) fn has(&self, ifd: usize, tag: u16) -> Result<bool, RawError> {
        Ok(self.entry(ifd, tag)?.is_some())
    }

    /// SHORT/LONG/BYTE 标量，统一收进 u32。
    pub(crate) fn scalar(&self, e: &Entry<'_>, tag: u16) -> Result<u32, RawError> {
        match e.typ {
            3 if e.data.len() >= 2 => Ok(self.order.u16(e.data) as u32),
            4 if e.data.len() >= 4 => Ok(self.order.u32(e.data)),
            1 if !e.data.is_empty() => Ok(e.data[0] as u32),
            _ => Err(RawError::BadTag(tag)),
        }
    }

    /// SHORT/LONG/BYTE 数组，收进 u32。条带偏移、字节数、CFA 图案走这里。
    pub(crate) fn scalars(&self, e: &Entry<'_>, tag: u16) -> Result<Vec<u32>, RawError> {
        let order = self.order;
        match e.typ {
            3 => walk(e.data, 2, |b| order.u16(b) as u32),
            4 => walk(e.data, 4, |b| order.u32(b)),
            1 => Ok(e.data.iter().map(|b| *b as u32).collect()),
            _ => Err(RawError::BadTag(tag)),
        }
    }

    /// 电平与白平衡类标记：SHORT/LONG/RATIONAL/SRATIONAL，值可能是小数。
    pub(crate) fn numbers(&self, e: &Entry<'_>, tag: u16) -> Result<Vec<f64>, RawError> {
        let order = self.order;
        match e.typ {
            3 => walk(e.data, 2, |b| order.u16(b) as f64),
            4 => walk(e.data, 4, |b| order.u32(b) as f64),
            5 => walk(e.data, 8, |b| {
                // 分母为 0 的有理数是坏值：整条标记当读不到，别让一个字段废掉一张图。
                let den = order.u32(&b[4..8]);
                if den == 0 {
                    f64::NAN
                } else {
                    order.u32(&b[0..4]) as f64 / den as f64
                }
            }),
            10 => walk(e.data, 8, |b| {
                let num = order.u32(&b[0..4]) as i32;
                let den = order.u32(&b[4..8]) as i32;
                if den == 0 {
                    f64::NAN
                } else {
                    num as f64 / den as f64
                }
            }),
            _ => Err(RawError::BadTag(tag)),
        }
    }

    pub(crate) fn text(&self, e: &Entry<'_>) -> Option<String> {
        if e.typ != 2 {
            return None;
        }
        let end = e.data.iter().position(|b| *b == 0).unwrap_or(e.data.len());
        let s = String::from_utf8_lossy(&e.data[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}
