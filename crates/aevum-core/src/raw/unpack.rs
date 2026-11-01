//! 采样位打包原语：把条带里的字节还原成每采样一个 `u16`。
//!
//! Sample bit-packing primitives: restores the bytes of a strip into one
//! `u16` per sample.
//!
//! 这里实现的打包**比策略层实际允许的多**。`MsbRow`/`LsbRow` 有逐位往返测试，但
//! `crate::raw` 的元数据层只在 8/16 位容器（`U8`/`U16Le`/`U16Be`）上走传感器路径。
//! 理由是长度算术区分不了写法：14 位、8160 列时，「每行独立打包」与「整图连续打包」的
//! 条带总字节数完全相同，猜错的后果是整幅图横向错位而不是一个错误，所以宁可退到预览兜底。
//!
//! What is implemented here is **more than the policy layer actually
//! allows**. `MsbRow`/`LsbRow` have bit-exact round-trip tests, but the
//! metadata layer of `crate::raw` only takes the sensor path on 8/16-bit
//! containers (`U8`/`U16Le`/`U16Be`). The reason is that length arithmetic
//! cannot tell the layouts apart: at 14 bits and 8160 columns, per-row
//! packing and whole-image continuous packing give the same total strip
//! byte count, and guessing wrong shifts the whole image horizontally
//! instead of raising one error, so falling back to the preview is
//! preferred.
//!
//! 留着这两个模式不是给「以后随便接上」留后门，而是让位打包这件事有一份可以逐位核对的
//! 实现：将来有能核对的规范条文与真实样本时，只需要在上层加一条有依据的选择规则，
//! 不用重写算术。
//!
//! Keeping these two modes is not a back door for wiring them up casually
//! later; it gives bit packing an implementation that can be verified bit by
//! bit. Once spec clauses and real samples that can be checked against
//! exist, only an evidence-backed selection rule needs to be added upstream;
//! the arithmetic itself does not need rewriting.

/// 一种采样排列方式。
///
/// One way of arranging samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packing {
    /// 每采样 1 字节。
    ///
    /// One byte per sample.
    U8,
    /// 每采样 2 字节，小端。
    ///
    /// Two bytes per sample, little-endian.
    U16Le,
    /// 每采样 2 字节，大端。
    ///
    /// Two bytes per sample, big-endian.
    U16Be,
    /// `bits` 位一行连续打包，高位在前（TIFF FillOrder=1 的说法）。
    ///
    /// `bits` bits packed continuously per row, most significant bit first
    /// (TIFF FillOrder=1 wording).
    MsbRow { bits: u8 },
    /// `bits` 位一行连续打包，低位在前（TIFF FillOrder=2 的说法）。
    ///
    /// `bits` bits packed continuously per row, least significant bit first
    /// (TIFF FillOrder=2 wording).
    LsbRow { bits: u8 },
}

/// 打包模式支持的最大位宽；再宽就放不进 `u16` 采样。
const MAX_PACK_BITS: u8 = 16;

impl Packing {
    /// 每采样的有效位宽。
    ///
    /// The effective bit width per sample.
    pub fn bits(self) -> u16 {
        match self {
            Self::U8 => 8,
            Self::U16Le | Self::U16Be => 16,
            Self::MsbRow { bits } | Self::LsbRow { bits } => u16::from(bits),
        }
    }

    /// 是不是「每采样占整数字节」的容器写法。只有容器写法能被元数据层选中。
    ///
    /// Whether this is a container layout with a whole number of bytes per
    /// sample. Only container layouts can be picked by the metadata layer.
    pub fn is_container(self) -> bool {
        matches!(self, Self::U8 | Self::U16Le | Self::U16Be)
    }

    /// 一行 `width` 个采样要占多少字节；位宽非法或算不下返回 `None`。
    ///
    /// How many bytes a row of `width` samples occupies; returns `None` when
    /// the bit width is invalid or the arithmetic overflows.
    pub fn row_bytes(self, width: usize) -> Option<usize> {
        let bits = self.bits();
        if bits == 0 || bits > u16::from(MAX_PACK_BITS) {
            return None;
        }
        let per_row = match self {
            Self::U8 => width.checked_mul(bits as usize / 8)?,
            Self::U16Le | Self::U16Be => width.checked_mul(2)?,
            // 行内打包：向上取整到字节，行尾剩下的位是填充。
            _ => {
                let total_bits = (width as u64).checked_mul(u64::from(bits))?;
                usize::try_from(total_bits.div_ceil(8)).ok()?
            }
        };
        Some(per_row)
    }

    /// 解一行。`out` 必须正好 `width` 个采样，`src` 至少要有整行字节，
    /// 剩下的是行尾填充；任何一条不满足都返回 `false`，调用方按布局判断失败处理。
    ///
    /// Unpacks one row. `out` must hold exactly `width` samples, and `src`
    /// must carry at least the full row of bytes; the remainder is row-end
    /// padding. Any violated condition returns `false`, and the caller
    /// handles it as a layout failure.
    pub fn unpack_row(self, src: &[u8], width: usize, out: &mut [u16]) -> bool {
        if out.len() != width || width == 0 {
            return false;
        }
        let Some(row) = self.row_bytes(width) else {
            return false;
        };
        if src.len() < row {
            return false;
        }
        match self {
            Self::U8 => {
                for (o, b) in out.iter_mut().zip(&src[..row]) {
                    *o = u16::from(*b);
                }
                true
            }
            Self::U16Le => {
                for (o, b) in out.iter_mut().zip(src.chunks_exact(2)) {
                    *o = u16::from_le_bytes([b[0], b[1]]);
                }
                true
            }
            Self::U16Be => {
                for (o, b) in out.iter_mut().zip(src.chunks_exact(2)) {
                    *o = u16::from_be_bytes([b[0], b[1]]);
                }
                true
            }
            Self::MsbRow { bits } => unpack_msb(src, bits, out),
            Self::LsbRow { bits } => unpack_lsb(src, bits, out),
        }
    }

    /// 反向打包一行，只用来做往返核对（测试与写入侧自检）。超出位宽的值直接返回
    /// `false` 并且不保证 `out` 的状态，调用方不要拿它当编码器用。
    ///
    /// Packs one row in reverse, only for round-trip checking (tests and
    /// writer-side self-checks). Values beyond the bit width return `false`
    /// immediately, and the state of `out` is not guaranteed; do not use
    /// this as an encoder.
    pub fn pack_row(self, values: &[u16], out: &mut Vec<u8>) -> bool {
        let bits = self.bits();
        if bits == 0 || values.is_empty() {
            return false;
        }
        let max = (1u32 << bits) - 1;
        if values.iter().any(|v| u32::from(*v) > max) {
            return false;
        }
        match self {
            Self::U8 => {
                for v in values {
                    out.push(*v as u8);
                }
            }
            Self::U16Le => {
                for v in values {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            Self::U16Be => {
                for v in values {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
            Self::MsbRow { bits } => return pack_msb(values, bits, out),
            Self::LsbRow { bits } => return pack_lsb(values, bits, out),
        }
        true
    }
}

/// MSB 先行取位：位流从字节最高位开始，一个采样跨字节时高半字节在前。
fn unpack_msb(src: &[u8], bits: u8, out: &mut [u16]) -> bool {
    if bits == 0 || bits > MAX_PACK_BITS {
        return false;
    }
    let mut acc: u32 = 0;
    let mut held: u32 = 0;
    let mut at = 0usize;
    for o in out.iter_mut() {
        while held < u32::from(bits) {
            let b = match src.get(at) {
                Some(b) => u32::from(*b),
                None => return false,
            };
            at += 1;
            acc = (acc << 8) | b;
            held += 8;
        }
        held -= u32::from(bits);
        *o = (acc >> held) as u16;
        acc &= if held == 0 { 0 } else { (1u32 << held) - 1 };
    }
    true
}

/// LSB 先行取位：位流从字节最低位开始。
fn unpack_lsb(src: &[u8], bits: u8, out: &mut [u16]) -> bool {
    if bits == 0 || bits > MAX_PACK_BITS {
        return false;
    }
    let mut acc: u32 = 0;
    let mut held: u32 = 0;
    let mut at = 0usize;
    for o in out.iter_mut() {
        while held < u32::from(bits) {
            let b = match src.get(at) {
                Some(b) => u32::from(*b),
                None => return false,
            };
            at += 1;
            acc |= b << held;
            held += 8;
        }
        *o = (acc & ((1u32 << u32::from(bits)) - 1)) as u16;
        acc >>= u32::from(bits);
        held -= u32::from(bits);
    }
    true
}

/// 按 MSB 先行写位流；与 `unpack_msb` 严格对称，行尾残余位左对齐补零。
fn pack_msb(values: &[u16], bits: u8, out: &mut Vec<u8>) -> bool {
    if bits == 0 || bits > MAX_PACK_BITS {
        return false;
    }
    let width = u32::from(bits);
    let mut acc: u32 = 0;
    let mut held: u32 = 0;
    for v in values {
        acc = (acc << width) | u32::from(*v);
        held += width;
        while held >= 8 {
            held -= 8;
            out.push(((acc >> held) & 0xff) as u8);
        }
        acc &= if held == 0 { 0 } else { (1u32 << held) - 1 };
    }
    if held > 0 {
        out.push(((acc << (8 - held)) & 0xff) as u8);
    }
    true
}

/// 按 LSB 先行写位流；与 `unpack_lsb` 严格对称。
fn pack_lsb(values: &[u16], bits: u8, out: &mut Vec<u8>) -> bool {
    if bits == 0 || bits > MAX_PACK_BITS {
        return false;
    }
    let mut acc: u32 = 0;
    let mut held: u32 = 0;
    for v in values {
        acc |= u32::from(*v) << held;
        held += u32::from(bits);
        while held >= 8 {
            out.push((acc & 0xff) as u8);
            acc >>= 8;
            held -= 8;
        }
    }
    if held > 0 {
        out.push((acc & 0xff) as u8);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐位往返：解出来的行必须和原样一模一样，且字节数正好等于 `row_bytes`。
    fn roundtrip(packing: Packing, values: &[u16]) {
        let mut bytes = Vec::new();
        assert!(packing.pack_row(values, &mut bytes), "打包失败");
        let want = packing.row_bytes(values.len()).expect("行宽可算");
        assert_eq!(bytes.len(), want, "{packing:?} 的行字节数");
        let mut out = vec![0u16; values.len()];
        assert!(packing.unpack_row(&bytes, values.len(), &mut out));
        assert_eq!(out, values.to_vec(), "{packing:?} 往返不一致");
    }

    #[test]
    fn container_packings_roundtrip() {
        let v = [1u16, 255, 256, 4095, 65535, 0, 32768];
        for p in [Packing::U8, Packing::U16Le, Packing::U16Be] {
            if p == Packing::U8 {
                let small: Vec<u16> = v.iter().map(|x| *x % 256).collect();
                roundtrip(p, &small);
                assert_eq!(p.row_bytes(3), Some(3));
            } else {
                roundtrip(p, &v);
                assert_eq!(p.row_bytes(3), Some(6));
            }
        }
    }

    #[test]
    fn packed_rows_roundtrip_for_common_bit_depths() {
        // 12 位（4 采样 3 长字那一族）与 14 位（ARW/RW2 声称的行内打包）都走同一套
        // 连续位流算术，这里逐位钉住它自洽：解包与打包互为逆。
        let cases: Vec<u16> = (0..17u32).map(|i| ((i * 2731) % 4096) as u16).collect();
        let cases14: Vec<u16> = (0..17u32).map(|i| ((i * 27311) % 16384) as u16).collect();
        for bits in [9u8, 10, 12, 14, 16] {
            let src: Vec<u16> = cases
                .iter()
                .map(|v| v & ((1u32 << bits) - 1) as u16)
                .collect();
            roundtrip(Packing::MsbRow { bits }, &src);
            roundtrip(Packing::LsbRow { bits }, &src);
        }
        roundtrip(Packing::MsbRow { bits: 14 }, &cases14);
        roundtrip(Packing::LsbRow { bits: 14 }, &cases14);
    }

    #[test]
    fn msb_and_lsb_packing_differ() {
        // 两种位序必须给出**不同**的字节，否则上面的往返测试等于什么都没验。
        let v = [0xf00u16, 0x0f0, 0x00f, 0xabc];
        let mut msb = Vec::new();
        let mut lsb = Vec::new();
        assert!(Packing::MsbRow { bits: 12 }.pack_row(&v, &mut msb));
        assert!(Packing::LsbRow { bits: 12 }.pack_row(&v, &mut lsb));
        assert_ne!(msb, lsb);
        // 12 位 × 4 采样 = 48 位 = 6 字节，与「4 采样挤进 3 个长字」的字节数一致。
        assert_eq!(msb.len(), 6);
        assert_eq!(Packing::MsbRow { bits: 12 }.row_bytes(4), Some(6));
    }

    #[test]
    fn row_needs_every_byte_it_claims() {
        let mut out = [0u16; 4];
        // 14 位 4 采样要 7 字节，只给 6 字节必须失败而不是少解一个。
        assert!(!Packing::MsbRow { bits: 14 }.unpack_row(&[0u8; 6], 4, &mut out));
        // out 长度与 width 不符同样拒：调用方算错行宽不该被静默吞掉。
        assert!(!Packing::U8.unpack_row(&[0u8; 4], 5, &mut out));
        // 行尾多余字节是填充，允许存在，只要够解 width 个采样。
        assert!(Packing::U8.unpack_row(&[7, 8, 9, 10, 0, 0], 4, &mut out));
        assert_eq!(out, [7, 8, 9, 10]);
    }

    #[test]
    fn bad_bit_widths_are_rejected_not_garbage() {
        let mut out = [0u16; 2];
        assert_eq!(Packing::MsbRow { bits: 0 }.row_bytes(2), None);
        assert_eq!(Packing::LsbRow { bits: 17 }.row_bytes(2), None);
        assert!(!Packing::MsbRow { bits: 0 }.unpack_row(&[0, 0, 0], 2, &mut out));
        assert!(!Packing::MsbRow { bits: 24 }.unpack_row(&[0; 8], 2, &mut out));
        assert!(!Packing::MsbRow { bits: 0 }.pack_row(&[1, 2], &mut Vec::new()));
    }

    #[test]
    fn pack_row_refuses_over_range_values() {
        // 12 位装不下 4096，写进去只会污染邻居采样，宁可不写。
        let mut out = Vec::new();
        assert!(!Packing::MsbRow { bits: 12 }.pack_row(&[4096], &mut out));
        assert!(Packing::MsbRow { bits: 12 }.pack_row(&[4095], &mut out));
    }
}
