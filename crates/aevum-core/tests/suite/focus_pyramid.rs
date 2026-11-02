//! 金字塔合成法的六条判据（P-C1…P-C6）。
//!
//! # 为什么单独一个文件
//!
//! `focus.rs` 的单元判据管的是深度图与加权平均，夹具是那两条的形状。金字塔要验的是
//! 另一件事：**塔的每一层有没有按该层的口径融合**。六条各自钉一个失效模式
//! （退化、常数场、分带、兜底、光晕、参数边界），放在同一处才看得出口径之间
//! 有没有互相打架。
//!
//! # 夹具都是 48×48
//!
//! auto 层数在 48px 上落到下限 3，所有判据都在同一个确定的塔高上跑；层数显式给
//! 1 / 12 的用例单独标出。尺寸小不是偷懒：过冲、常数场、分带这些性质在这一尺度
//! 上就能测出来，而 24 MP 的峰值内存与耗时由 T10 单独实测，不在这里假装测过。
//!
//! # Six assertions for pyramid fusion (P-C1...P-C6)
//!
//! # Why a separate file
//!
//! The unit assertions in `focus.rs` cover the depth map and the weighted average, with
//! fixtures shaped for those two. The pyramid has to prove something else: **every level
//! of the tower is fused according to that level's rule**. Each of the six assertions pins
//! one failure mode (degeneration, constant field, band selection, fallback, halo,
//! parameter bounds); keeping them together is what shows whether the rules conflict.
//!
//! # All fixtures are 48x48
//!
//! Auto levels hit the lower bound of 3 on a 48 px frame, so every assertion runs on one
//! known tower height; the cases with explicit levels 1 / 12 are called out. Small is not
//! laziness: overshoot, constant fields and band selection are all observable at this
//! scale, while 24 MP peak memory and timing are measured separately by T10 and are not
//! pretended to be measured here.

use aevum_core::focus::{Method, Options, focus_stack};

const W: usize = 48;
const H: usize = 48;

/// 平坦色块。
///
/// A flat colour.
fn flat(v: f32) -> Vec<f32> {
    vec![v; W * H * 3]
}

/// 竖条纹栅栏（高频纹理），周期 4px。
///
/// Vertical plank stripes (high-frequency texture), 4 px period.
fn planks(phase: usize) -> Vec<f32> {
    let mut f = vec![0.0f32; W * H * 3];
    for y in 0..H {
        for x in 0..W {
            let v = if ((x + phase) / 2).is_multiple_of(2) {
                0.8
            } else {
                0.2
            };
            let j = (y * W + x) * 3;
            f[j] = v;
            f[j + 1] = v;
            f[j + 2] = v;
        }
    }
    f
}

/// 横条纹栅栏。
///
/// Horizontal plank stripes.
fn planks_h() -> Vec<f32> {
    let mut f = vec![0.0f32; W * H * 3];
    for y in 0..H {
        for x in 0..W {
            let v = if (y / 2).is_multiple_of(2) { 0.8 } else { 0.2 };
            let j = (y * W + x) * 3;
            f[j] = v;
            f[j + 1] = v;
            f[j + 2] = v;
        }
    }
    f
}

/// 5×5 盒模糊，当「失焦」用。
///
/// 5x5 box blur standing in for defocus.
fn blur5(f: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0f32; f.len()];
    for y in 0..H {
        for x in 0..W {
            for c in 0..3 {
                let mut acc = 0.0f32;
                let mut cnt = 0u32;
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let yy = (y as i32 + dy).clamp(0, H as i32 - 1) as usize;
                        let xx = (x as i32 + dx).clamp(0, W as i32 - 1) as usize;
                        acc += f[(yy * W + xx) * 3 + c];
                        cnt += 1;
                    }
                }
                out[(y * W + x) * 3 + c] = acc / cnt as f32;
            }
        }
    }
    out
}

/// 金字塔参数；故意把 `feather` / `smooth_z` 塞成非零，验的是「本法不读它们」。
///
/// Pyramid options; `feather` / `smooth_z` are deliberately non-zero to prove this
/// method never reads them.
fn pyramid(levels: usize, threshold: f32, radius: usize) -> Options {
    Options {
        method: Method::Pyramid,
        levels,
        threshold,
        radius,
        smooth_z: 3,
        feather: 12.0,
    }
}

/// 逐点最大绝对偏差。
///
/// Maximum absolute per-sample difference.
fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b.iter())
        .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
}

/// 单通道样本的值（三帧同色，取 R 就够）。
///
/// One sample's value (all channels equal in these fixtures, so R suffices).
fn px(out: &[f32], x: usize, y: usize) -> f32 {
    out[(y * W + x) * 3]
}

/// 三帧三带：上 1/3 只有帧 0 清晰，中 1/3 只有帧 1，下 1/3 只有帧 2。
///
/// Three frames, three bands: only frame 0 is sharp in the top third, frame 1 in the
/// middle and frame 2 in the bottom.
fn three_bands() -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let sharp0 = planks(0);
    let sharp1 = planks_h();
    let sharp2 = planks(2);
    let mut f0 = blur5(&sharp1);
    f0[..W * 16 * 3].copy_from_slice(&sharp0[..W * 16 * 3]);
    let mut f1 = blur5(&sharp0);
    f1[W * 16 * 3..W * 32 * 3].copy_from_slice(&sharp1[W * 16 * 3..W * 32 * 3]);
    let mut f2 = blur5(&sharp0);
    f2[W * 32 * 3..].copy_from_slice(&sharp2[W * 32 * 3..]);
    (f0, f1, f2)
}

/// P-C1：`levels=1` 的 Pyramid 与同参数 Weighted 逐点一致。
///
/// 负向锚写在判据里：把 `levels` 传 3 就不许再相等。外部的负向验证还会把
/// `pyramid.rs` 的 `m^4` 临时改成 `m^2`，这条必须红。
///
/// P-C1: `levels=1` Pyramid matches Weighted with the same options, sample by sample.
///
/// A negative anchor lives inside the assertion: `levels=3` must not match. The external
/// negative check also temporarily changes `m^4` to `m^2` in `pyramid.rs`; this must go red.
#[test]
fn p_c1_levels_one_degenerates_to_weighted() {
    let a = planks(0);
    let b = planks_h();
    let c = flat(0.55);
    let frames: [&[f32]; 3] = [&a, &b, &c];
    let weighted = Options {
        method: Method::Weighted,
        radius: 2,
        threshold: 0.0,
        smooth_z: 3,
        feather: 12.0,
        levels: 0,
    };
    let one = pyramid(1, 0.0, 2);
    let out_w = focus_stack(&frames, W, H, &weighted).expect("weighted stack");
    let out_1 = focus_stack(&frames, W, H, &one).expect("pyramid levels=1");
    let delta = max_abs_diff(&out_w, &out_1);
    eprintln!("P-C1 levels=1 与 Weighted 的逐点最大偏差 = {delta}");
    assert!(
        delta < 1e-5,
        "levels=1 必须严格退化为 Weighted，实际 max|Δ|={delta}"
    );
    // 反向锚：多尺度真的改变了结果，这条才不是「两个实现都没跑」的假绿。
    //
    // Reverse anchor: multi-scale really changes the result, so this is not a false green
    // where neither implementation ran.
    let three = pyramid(3, 0.0, 2);
    let out_3 = focus_stack(&frames, W, H, &three).expect("pyramid levels=3");
    assert!(
        max_abs_diff(&out_w, &out_3) > 1e-4,
        "levels=3 不该与 Weighted 逐点一致（说明塔没起作用）"
    );
    assert_eq!(Method::parse("pyramid"), Some(Method::Pyramid));
    assert_eq!(Method::Pyramid.cn(), "金字塔");
}

/// P-C2：全帧常值 v，单帧与多帧、auto 与显式层数，输出都逐点 = v。
///
/// 负向验证：把 `pyramid::taps` 里的 `v / sum` 归一去掉，常数场会在每次
/// 上采样后被放大，这条立刻红。
///
/// P-C2: a constant field stays exactly constant, for one frame and many, auto and explicit.
///
/// Negative check: drop the `v / sum` normalisation in `pyramid::taps` and every upsample
/// magnifies the constant, so this goes red.
#[test]
fn p_c2_a_constant_field_collapses_to_itself() {
    // 先直接钉住核本身：把一张常数高斯塔转成拉普拉斯塔再塌缩回来，必须逐点还原。
    // 端到端那条会被「无细节区整块退回参考帧」兜住，掩盖掉上采样核不归一的错，
    // 所以这一条单独验 `upsample` ——P-C2 的负向验证打的就是这里。
    //
    // Pin the kernel itself first: turn a constant Gaussian tower into a Laplacian tower
    // and collapse it back; every sample must be restored. The end-to-end case below is
    // masked by the "featureless areas fall back to the reference frame" rule, which would
    // hide an unnormalised upsample kernel -- so `upsample` gets its own assertion here.
    // This is what P-C2's negative check targets.
    use aevum_core::pyramid::{
        Plane, collapse_in_place, gaussian_pyramid, laplacian_from_gaussian,
    };
    for v in [0.25f32, 0.8] {
        let src = flat(v);
        let g = gaussian_pyramid(&src, W, H, 4);
        let mut tower: Vec<Plane> = g
            .iter()
            .enumerate()
            .map(|(l, fine)| {
                if l + 1 < g.len() {
                    laplacian_from_gaussian(fine, &g[l + 1])
                } else {
                    fine.clone()
                }
            })
            .collect();
        let rebuilt = collapse_in_place(&mut tower);
        let drift = rebuilt.iter().fold(0.0f32, |m, &x| m.max((x - v).abs()));
        assert!(drift <= 1e-6, "常数塔塌缩后漂移 {drift}：上采样核没有归一");
    }
    for v in [0.0f32, 0.25, 0.8] {
        let frames: Vec<Vec<f32>> = (0..3).map(|_| flat(v)).collect();
        let borrowed: Vec<&[f32]> = frames.iter().map(Vec::as_slice).collect();
        let single = focus_stack(&[borrowed[0]], W, H, &pyramid(0, 0.0, 2)).expect("single");
        assert!(
            single.iter().all(|&x| (x - v).abs() <= 1e-6),
            "单帧常数场 v={v} 被改动了"
        );
        for levels in [0usize, 1, 3, 6] {
            let out = focus_stack(&borrowed, W, H, &pyramid(levels, 0.0, 2))
                .unwrap_or_else(|e| panic!("v={v} levels={levels} 应 Ok，实得 {e:?}"));
            assert_eq!(out.len(), W * H * 3);
            let drift = out.iter().fold(0.0f32, |m, &x| m.max((x - v).abs()));
            assert!(drift <= 1e-6, "常数场 v={v} levels={levels} 漂移了 {drift}");
        }
    }
}

/// P-C3：三帧三带，各带中心像素 ≈ 持纹理帧的值。
///
/// 负向验证：去掉 `fuse_pyramid` 末尾按 `wsum` 归一的循环，输出会整体变亮，
/// 三带中心一起偏离。
///
/// P-C3: in the three-band fixture, each band's centre takes the sharp frame's value.
///
/// Negative check: remove the final `wsum` normalisation in `fuse_pyramid`; the whole
/// result brightens and all three centres drift.
#[test]
fn p_c3_each_band_takes_the_frame_holding_its_texture() {
    let (f0, f1, f2) = three_bands();
    let frames: [&[f32]; 3] = [&f0, &f1, &f2];
    let out = focus_stack(&frames, W, H, &pyramid(3, 0.0, 2)).expect("pyramid stack");
    // 带中心离分界 8 行以上，`radius=2` 的测度窗口吃不到邻区。
    //
    // Centres are more than 8 rows from a seam, outside the radius-2 measure window.
    let probes: [(usize, usize, &[f32]); 3] = [(8, 8, &f0), (8, 24, &f1), (8, 40, &f2)];
    for (x, y, sharp) in probes {
        let got = px(&out, x, y);
        let want = px(sharp, x, y);
        assert!(
            (got - want).abs() < 0.05,
            "带中心 ({x},{y}) 应为持纹理帧的 {want}，实际 {got}"
        );
    }
}

/// 无细节夹具：帧 0 纯平 0.5 + 中央一块清晰纹理，帧 1（参考帧）0.45 底 + 缓纹波，
/// 帧 2 与帧 0 同。
///
/// A featureless fixture: frame 0 is flat 0.5 plus a central sharp patch, frame 1 (the
/// reference) is a 0.45 base with a gentle ripple, frame 2 equals frame 0.
fn featureless() -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let mut f0 = flat(0.5);
    for y in 16..32 {
        for x in 16..32 {
            let j = (y * W + x) * 3;
            let v = if ((y * W + x) / 2).is_multiple_of(2) {
                0.8
            } else {
                0.2
            };
            f0[j] = v;
            f0[j + 1] = v;
            f0[j + 2] = v;
        }
    }
    let mut f1 = vec![0.0f32; W * H * 3];
    for y in 0..H {
        for x in 0..W {
            let v = 0.45 + 0.01 * (x as f32 * 0.5).sin();
            let j = (y * W + x) * 3;
            f1[j] = v;
            f1[j + 1] = v;
            f1[j + 2] = v;
        }
    }
    let f2 = f0.clone();
    (f0, f1, f2)
}

/// P-C4：`threshold=90` 平坦区输出 = 参考帧；`threshold=0` 跟随权重场（argmax），
/// 不再钉死在参考帧上。
///
/// 负向验证：去掉 `fuse_pyramid` 里「`wsum < 1e-6` → 参考帧」的回填，平坦区会
/// 变成 0 或 NaN，本条立刻红。
///
/// P-C4: `threshold=90` hands the flat area to the reference frame; `threshold=0`
/// follows the weight field (argmax) instead of pinning to the reference.
///
/// Negative check: remove the "`wsum < 1e-6` -> reference frame" fill in `fuse_pyramid`;
/// the flat area becomes 0 or NaN and this goes red.
#[test]
fn p_c4_featureless_falls_back_to_the_reference_frame() {
    let (f0, f1, f2) = featureless();
    let frames: [&[f32]; 3] = [&f0, &f1, &f2];
    let on = focus_stack(&frames, W, H, &pyramid(3, 90.0, 2)).expect("threshold=90");
    let off = focus_stack(&frames, W, H, &pyramid(3, 0.0, 2)).expect("threshold=0");
    let p = (4 * W + 4) * 3;
    assert!(
        (on[p] - f1[p]).abs() < 1e-4,
        "threshold=90 的平坦区应逐点等于参考帧 {}，实际 {}",
        f1[p],
        on[p]
    );
    assert!(
        on.iter().all(|v| v.is_finite()),
        "threshold=90 的成片里不能有非有限值"
    );
    assert!(
        (off[p] - f1[p]).abs() > 0.01,
        "threshold=0 时平坦区不该仍钉死在参考帧上（应跟权重场）"
    );
    assert!(
        off.iter().all(|v| v.is_finite()),
        "threshold=0 的成片里不能有非有限值"
    );
    // 纹理块中心不受兜底影响：门限只割掉弱权重，不割细节。
    //
    // The patch centre is untouched: the cut removes weak weights, not detail.
    let patch = (24 * W + 24) * 3;
    assert!(
        (on[patch] - f0[patch]).abs() < 0.05,
        "纹理块中心仍应取清晰帧 {}，实际 {}",
        f0[patch],
        on[patch]
    );
}

/// 亮底暗纹夹具：亮底 0.9，中央一块暗方 0.1；帧 0 清晰、帧 1 模糊、帧 2 纯亮底。
///
/// A bright-base / dark-texture fixture: a 0.9 base with a central 0.1 square; frame 0
/// is sharp, frame 1 blurred, frame 2 is the bare bright base.
fn bright_dark() -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let mut sharp = flat(0.9);
    for y in 16..32 {
        for x in 16..32 {
            let j = (y * W + x) * 3;
            let v = if ((x + y) / 2).is_multiple_of(2) {
                0.1
            } else {
                0.35
            };
            sharp[j] = v;
            sharp[j + 1] = v;
            sharp[j + 2] = v;
        }
    }
    // 三帧是同一景物的三档失焦，不是「一张有暗块、两张没有」——后者会把
    // 低频底图本身变成差异源，测的就不是权重规则而是帧内容了。
    //
    // All three frames are the same scene at three defocus levels, not "one dark square,
    // two bare fields": the latter turns the low-frequency base itself into a difference,
    // so the assertion would be measuring frame content instead of the weight rule.
    let once = blur5(&sharp);
    let twice = blur5(&once);
    (sharp, once, twice)
}

/// P-C5：纹理边界外 `2^levels` px 影响区内，输出不得越过输入帧的逐点包络。
///
/// 负向验证：把 `fuse_pyramid` 的 `tight_from` 改成 `levels`（即所有层都用 `m^4`），
/// 粗层跟着暗方走，边界外出现低于逐点最小值的下冲，这条红。
///
/// P-C5: within `2^levels` px outside the texture boundary, the output must not leave the
/// per-pixel envelope of the input frames.
///
/// Negative check: set `tight_from = levels` in `fuse_pyramid` (all levels use `m^4`);
/// the coarse levels follow the dark square and an under-shoot appears outside the edge.
#[test]
fn p_c5_no_inversion_overshoot_outside_the_texture() {
    let (f0, f1, f2) = bright_dark();
    let frames: [&[f32]; 3] = [&f0, &f1, &f2];
    let levels = 3usize;
    let reach = 1usize << levels;
    let out = focus_stack(&frames, W, H, &pyramid(levels, 0.0, 2)).expect("pyramid stack");
    let mut worst = 0.0f32;
    for y in 0..H {
        for x in 0..W {
            // 只验「纹理边界外、影响区内」：方内和离得太远的地方不算本题。
            //
            // Only outside the texture but inside the influence radius; the square itself
            // and far-away pixels are not this assertion's subject.
            let inside = (16..32).contains(&x) && (16..32).contains(&y);
            let dist = if inside {
                0
            } else {
                let dx = if x < 16 { 16 - x } else { x.saturating_sub(31) };
                let dy = if y < 16 { 16 - y } else { y.saturating_sub(31) };
                dx.max(dy)
            };
            if inside || dist == 0 || dist > reach {
                continue;
            }
            let j = (y * W + x) * 3;
            let lo = f0[j].min(f1[j]).min(f2[j]);
            let hi = f0[j].max(f1[j]).max(f2[j]);
            let got = out[j];
            let over = (lo - got).max(got - hi);
            worst = worst.max(over);
            // 1e-2 是拉普拉斯重构的固有振铃容差：正向上实测最大 0.0058，
            // 负向（全层 m^4）实测 0.026，是容差的 2.6 倍以上，判据有牙。
            //
            // 1e-2 is the intrinsic ringing tolerance of Laplacian reconstruction: the
            // healthy implementation measures 0.0058 worst, while the negative (all
            // levels on m^4) measures 0.026 -- well above the tolerance.
            assert!(
                over <= 1e-2,
                "({x},{y}) 越过输入包络 [{lo},{hi}]：got={got}，超出 {over}"
            );
        }
    }
    eprintln!("P-C5 包络最大越界量 = {worst}");
}

/// P-C6：参数边界全部返回 Ok、输出有限；越界层数静默夹紧。
///
/// 负向验证：去掉 `resolve_levels` auto 路径的 `clamp(3, 8)`，48px 图会算出 0 层，
/// 输出塔为空、塌缩越界，本条红。
///
/// P-C6: every parameter boundary returns Ok with finite output; out-of-range levels are
/// silently clamped.
///
/// Negative check: remove the `clamp(3, 8)` in `resolve_levels`' auto path. A 48 px frame
/// resolves to 0 levels, the output tower is empty and collapse panics here.
#[test]
fn p_c6_parameter_edges_stay_finite() {
    let (f0, f1, f2) = three_bands();
    let frames: [&[f32]; 3] = [&f0, &f1, &f2];
    for levels in [0usize, 1, 12, usize::MAX] {
        for threshold in [0.0f32, 90.0, f32::NAN] {
            for radius in [0usize, 32] {
                let opts = pyramid(levels, threshold, radius);
                let out = focus_stack(&frames, W, H, &opts).unwrap_or_else(|e| {
                    panic!(
                        "levels={levels} threshold={threshold} radius={radius} 应 Ok，实得 {e:?}"
                    )
                });
                assert_eq!(out.len(), W * H * 3);
                assert!(
                    out.iter().all(|v| v.is_finite()),
                    "levels={levels} threshold={threshold} radius={radius} 输出有非有限值"
                );
            }
        }
    }
    assert_eq!(aevum_core::pyramid::resolve_levels(W, H, 0), 3);
    assert_eq!(aevum_core::pyramid::resolve_levels(6000, 4000, 0), 6);
    assert_eq!(aevum_core::pyramid::resolve_levels(W, H, usize::MAX), 12);
    assert_eq!(aevum_core::pyramid::resolve_levels(W, H, 1), 1);
}
