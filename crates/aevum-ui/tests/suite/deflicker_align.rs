//! 面板上「按 EXIF 预对齐」这个开关，到底有没有把 EXIF 的台阶折进校正表。
//!
//! 为什么单独一条判据：core 已经把 `analyze_luma_exif` 本身咬死了（场景域残差降一个
//! 量级、日转夜必须还回来、缺 EV 的帧校正为 0）。但面板走的是另一条线：`recompute_deflicker`
//! 自己拼 EV 表、自己挑分析函数。这里接线错了——比如开关存在但没传进分析、或者 EV 表
//! 长度对不上帧数——core 全绿，用户勾了开关却什么都没发生（这正是「假开关」）。
//!
//! EXIF 由**真的字节**造出来（小端 TIFF，只带 ISO / 快门 / 光圈），EV 交给
//! `exif::read_block` 去读：这样断言的是「面板 → 分析」的整条路径，而不是
//! 我们自己对 EV 的想象。
//!
//! Why a separate assertion: core already nails `analyze_luma_exif` itself (an order of
//! magnitude less pre-deflicker residual, the day-night transition handed back, correction 0
//! for frames without an EV). The panel walks a different path: `recompute_deflicker` builds
//! the EV table and picks the analyzer. A wrong wire there — the switch stored but never
//! passed down, or a table whose length disagrees with the frame count — leaves core green
//! while the user ticks the box and nothing happens (exactly a fake switch).
//!
//! The EXIF is built from **real bytes** (a little-endian TIFF carrying only ISO / shutter /
//! aperture) and the EV comes from `exif::read_block`, so what is asserted is the whole
//! panel-to-analysis path, not our idea of what an EV is.

use aevum_core::deflicker::FrameMeans;
use aevum_core::exif::{self, Exif};
use aevum_core::metrics::FrameMetrics;
use aevum_ui::state::AppState;

/// 一帧的时长分子（毫秒）。基准 20 ms = 1/50 s，每补偿一档翻倍。
const BASE_MS: u32 = 20;

/// 造一段只带曝光三要素的小端 TIFF：ISO 200、f/2.8、快门由 `exposure_ms` 决定。
///
/// 标签号按 EXIF 标准写死（0x8827 ISO、0x829A ExposureTime、0x829D FNumber）——这是
/// 平台事实，不从被测代码里取，否则判据会跟着实现一起漂。
///
/// A little-endian TIFF carrying only the exposure triplet: ISO 200, f/2.8, shutter from
/// `exposure_ms`. The tag numbers are the EXIF standard, hard-coded here on purpose: taking
/// them from the code under test would let the assertion drift with the implementation.
fn tiff_with_triplet(exposure_ms: u32) -> Vec<u8> {
    const TAG_EXPOSURE: u16 = 0x829A;
    const TAG_FNUM: u16 = 0x829D;
    const TAG_ISO: u16 = 0x8827;
    // IFD0 从偏移 8 开始：3 条 12 字节条目 + 2 字节数量 + 4 字节 next 指针。
    const VALUES_AT: u32 = 8 + 2 + 3 * 12 + 4;

    let mut out = b"II".to_vec();
    out.extend(42u16.to_le_bytes());
    out.extend(8u32.to_le_bytes());
    out.extend(3u16.to_le_bytes());
    let mut entry = |tag: u16, typ: u16, count: u32, value: u32| {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(value.to_le_bytes());
    };
    // 快门：RATIONAL（类型 5），值在值区；两个有理数各占 8 字节。
    entry(TAG_EXPOSURE, 5, 1, VALUES_AT);
    // 光圈 f/2.8 = 28/10，紧跟在快门后面。
    entry(TAG_FNUM, 5, 1, VALUES_AT + 8);
    // ISO 200：SHORT（类型 3）内联在值域里。
    entry(TAG_ISO, 3, 1, 200);
    out.extend(0u32.to_le_bytes());
    out.extend(exposure_ms.to_le_bytes());
    out.extend(1000u32.to_le_bytes());
    out.extend(28u32.to_le_bytes());
    out.extend(10u32.to_le_bytes());
    out
}

/// 合成日转夜（手动圣杯）：场景光 8 EV 匀速变暗，相机在 M 档每 20 帧拧一档补偿，
/// 外加 ±0.03 EV 的测光误差。画面被拉平 → 只有约 1 EV 的锯齿；EXIF 留着 8 EV 的台阶。
///
/// 单帧亮度与 EV 的口径和 core 的 `holy_grail` 夹具一致；差别只在 EV 是**从字节里读
/// 出来的**：快门每补偿一档翻倍，`exif::ev()` 自然给出低一档。
///
/// A synthetic day-to-night (manual holy grail): the scene drops 8 EV steadily while the
/// camera gains a stop every 20 frames, plus ±0.03 EV of metering error. The picture is held
/// flat (~1 EV sawtooth) while the EXIF keeps an 8 EV staircase. Brightness and EV follow the
/// same convention as core's `holy_grail` fixture; the difference is that the EV is **read
/// out of bytes**: the shutter doubles per stop, so `exif::ev()` reports one stop lower.
fn holy_grail(n: usize) -> (Vec<FrameMeans>, Vec<FrameMetrics>, Vec<Exif>) {
    let mut means = Vec::with_capacity(n);
    let mut metrics = Vec::with_capacity(n);
    let mut exifs = Vec::with_capacity(n);
    for i in 0..n {
        let scene = -8.0 * i as f64 / (n - 1).max(1) as f64;
        let comp = (i / 20) as u32;
        let noise = 0.03 * (i as f64 * 1.7).sin();
        let raw = 0.5 * 2f64.powf(scene + comp as f64 + noise);
        means.push(FrameMeans {
            luma: raw,
            rgb: [raw; 3],
        });
        metrics.push(FrameMetrics {
            mean: raw,
            median: raw,
            center: raw,
            sharp: 100.0,
        });
        exifs.push(exif::read_block(&tiff_with_triplet(BASE_MS << comp)));
    }
    (means, metrics, exifs)
}

/// 校正表的落差（最大 − 最小，EV）。这就是观众会看到的日夜过渡幅度。
///
/// The spread of the correction table (max - min, EV): the day-night range the viewer sees.
fn spread(corr: &[f32]) -> f64 {
    let max = corr.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let min = corr.iter().copied().fold(f32::INFINITY, f32::min);
    (max - min) as f64
}

fn state_with_grail() -> AppState {
    let (means, metrics, exifs) = holy_grail(160);
    let mut state = AppState::default();
    state.def_means = means.into_iter().map(Some).collect();
    state.metrics = metrics.into_iter().map(Some).collect();
    state.exif = exifs;
    state
}

/// 勾上开关：校正表必须把 EXIF 的台阶折进来（日转夜被还回来）；取消勾选必须回到原来的
/// 口径。开关没接进分析（永远走普通去闪）必红。
///
/// Ticking the box must fold the EXIF staircase into the correction table (the day-night
/// transition comes back); unticking must return to the plain convention. Red when the
/// switch never reaches the analysis and plain deflicker always runs.
#[test]
fn the_exif_switch_hands_the_day_night_transition_back_to_the_correction_table() {
    let mut state = state_with_grail();
    assert_eq!(state.exif_ev_frames(), 160, "夹具：每帧都该有曝光三要素");

    state.def_align_exif = false;
    state.recompute_deflicker();
    let flat = spread(&state.corr);
    assert!(
        flat < 2.0,
        "夹具失效：不对齐时相机把画面拉平了，校正表不该有大落差（实得 {flat:.2} EV）"
    );

    state.def_align_exif = true;
    state.recompute_deflicker();
    let restored = spread(&state.corr);
    assert!(
        restored > 6.0,
        "勾了预对齐，校正表却只还原了 {restored:.2} EV——开关没接进分析"
    );
    // 两遍的长度必须一致：EV 表短了会让分析的行数变少，校正与帧就对不上了。
    assert_eq!(
        state.corr.len(),
        160,
        "校正表必须逐帧，长度不能跟着 EV 表缩水"
    );
    assert!(
        state.corr_rgb.len() == 160,
        "分通道残差同样逐帧，长度不能缩水"
    );

    state.def_align_exif = false;
    state.recompute_deflicker();
    assert!(
        (spread(&state.corr) - flat).abs() < 1e-6,
        "关掉开关必须逐位回到不开时的口径"
    );
}

/// 没有 EXIF 的帧：开关勾着也不能造出假对齐——校正表必须与不开时逐位一致，
/// `exif_ev_frames` 为 0（面板据此亮出「不会生效」的提示）。
///
/// Frames with no EXIF: the switch must not invent an alignment -- the table stays
/// bit-identical to the unaligned one and `exif_ev_frames` reports 0, which is what makes the
/// panel show its "has no effect" note.
#[test]
fn a_sequence_without_exposure_tags_is_not_silently_re_analysed() {
    let mut state = state_with_grail();
    state.exif.clear();
    state.def_align_exif = false;
    state.recompute_deflicker();
    let plain = state.corr.clone();

    state.def_align_exif = true;
    state.recompute_deflicker();
    assert_eq!(state.exif_ev_frames(), 0, "没有 EXIF 时可用 EV 必须是 0");
    assert_eq!(
        state.corr, plain,
        "没有 EV 时预对齐必须等于没开，而不是造一个假对齐"
    );
}
