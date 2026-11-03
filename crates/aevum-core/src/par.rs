//! 内部并行助手：`std::thread::scope` 按行分块。
//!
//! 服务对象是「逐像素独立」或「逐行独立」的运算（塔的 reduce/上采样、逐元素混合、
//! 测度链）。调用方必须保证：每个输出像素的浮点加法序列与串行版**完全相同**——
//! 这里只改「谁算哪一块」，不改任何运算的结合顺序，所以结果逐位不变。
//! 跨像素的归约（帧间累加、积分图构建、直方图分位）**不许**走这里。
//!
//! 可变输出不走共享捕获（`&mut` 不是 `Sync`），而是由这里按行带切成互斥的
//! `&mut` 切片逐带借出；只读输入（源面、tap 表）由闭包自己共享捕获。
//!
//! An internal helper: `std::thread::scope` with row-band splitting.
//!
//! It serves per-pixel or per-row independent work (tower reduce/upsample, elementwise
//! blending, the measure chain). Callers must guarantee that each output pixel's float
//! addition sequence is **identical** to the serial version -- this only changes "who
//! computes which block", never the association order of any arithmetic, so results are
//! bitwise unchanged. Cross-pixel reductions (frame accumulation, summed-area tables,
//! histogram percentiles) must **not** go through here.
//!
//! Mutable outputs are not captured shared (`&mut` is not `Sync`); instead they are split
//! here into disjoint `&mut` row-band slices lent out per band, while read-only inputs
//! (source planes, tap tables) are captured shared by the closure itself.

/// 参与并行的线程数上限：机器的逻辑核数。
///
/// Thread cap: the machine's logical core count.
pub(crate) fn thread_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// 把 `rows` 行切成连续行带；行数太少就整段一条（不值得起线程）。
///
/// Split `rows` into contiguous bands; few rows collapse into a single band (spawning is
/// not worth it).
fn row_bands(rows: usize) -> Vec<(usize, usize)> {
    if rows < 128 {
        return if rows == 0 {
            Vec::new()
        } else {
            vec![(0, rows)]
        };
    }
    let threads = thread_count().min(rows);
    let band = rows.div_ceil(threads);
    let mut bands = Vec::with_capacity(threads);
    let mut start = 0;
    while start < rows {
        let end = (start + band).min(rows);
        bands.push((start, end));
        start = end;
    }
    bands
}

/// 按行带并行：把 `a` 按 `row_a` 一行切成与行带对齐的互斥 `&mut` 切片，逐带调
/// `f(y0, y1, a_band)`（`y1` 不含，切片从行 `y0` 起）。
///
/// Parallelise by row bands: `a` is cut into disjoint `&mut` slices of `row_a` elements
/// aligned with the bands, and `f(y0, y1, a_band)` runs per band (`y1` exclusive, the
/// slice starts at row `y0`).
pub(crate) fn for_bands<'a, A, F>(rows: usize, a: &'a mut [A], row_a: usize, f: F)
where
    A: Send + 'a,
    F: Fn(usize, usize, &mut [A]) + Send + Sync + 'a,
{
    let bands = row_bands(rows);
    if bands.len() <= 1 {
        if let Some(&(y0, y1)) = bands.first() {
            f(y0, y1, &mut a[y0 * row_a..y1 * row_a]);
        }
        return;
    }
    std::thread::scope(|s| {
        let mut rest = a;
        let run = &f;
        for &(y0, y1) in &bands {
            let (head, tail) = rest.split_at_mut((y1 - y0) * row_a);
            rest = tail;
            s.spawn(move || run(y0, y1, head));
        }
    });
}

/// 同 [`for_bands`]，但带**两个**按行对齐的可变输出（如输出塔 + 权重和塔）。
///
/// Like [`for_bands`], but with **two** row-aligned mutable outputs (e.g. the output tower
/// plus the weight-sum tower).
pub(crate) fn for_bands2<'a, A, B, F>(
    rows: usize,
    a: &'a mut [A],
    row_a: usize,
    b: &'a mut [B],
    row_b: usize,
    f: F,
) where
    A: Send + 'a,
    B: Send + 'a,
    F: Fn(usize, usize, &mut [A], &mut [B]) + Send + Sync + 'a,
{
    let bands = row_bands(rows);
    if bands.len() <= 1 {
        if let Some(&(y0, y1)) = bands.first() {
            f(
                y0,
                y1,
                &mut a[y0 * row_a..y1 * row_a],
                &mut b[y0 * row_b..y1 * row_b],
            );
        }
        return;
    }
    std::thread::scope(|s| {
        let mut rest_a = a;
        let mut rest_b = b;
        let run = &f;
        for &(y0, y1) in &bands {
            let (head_a, tail_a) = rest_a.split_at_mut((y1 - y0) * row_a);
            let (head_b, tail_b) = rest_b.split_at_mut((y1 - y0) * row_b);
            rest_a = tail_a;
            rest_b = tail_b;
            s.spawn(move || run(y0, y1, head_a, head_b));
        }
    });
}
