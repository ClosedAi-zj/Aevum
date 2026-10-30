//! 性能基准：五场景 release Instant 实测（性能重构设计-2026-10-30 §5）。
//!
//! 无显示环境跑 egui 帧循环，覆盖缓存设计针对的每帧重复构建路径：
//! - S1  Develop，1 张线性蒙版选中，指针悬停画布静止（E1+E2+E3）
//! - S2  1 张三形状组（线性+径向+200 点笔画画笔）选中，悬停（最坏情况分配）
//! - S3  曲线组展开（RGB 通道），无交互（E4）
//! - S4  Develop 无蒙版、直方图可见，无交互（E6）
//! - S5  600 帧序列、底部亮度曲线可见，无交互（E5）
//!
//! 跑法（release 才算数）：
//! `cargo test --release -p aevum-ui --lib bench -- --ignored --nocapture`

use std::time::Instant;

use aevum_core::mask::{self, ShapeKind};
use egui::{Event, Pos2, RawInput};
use serde_json::json;

use crate::shell;
use crate::state::{AppState, DockTab, Module};

/// 1600×1067 画幅、指针停在画布中央（约 (800, 400)），与设计文档 §5 一致。
fn input(hover_canvas: bool) -> RawInput {
    let mut input = RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(1600.0, 1067.0),
        )),
        max_texture_side: Some(8192),
        ..Default::default()
    };
    if hover_canvas {
        input.events.push(Event::PointerMoved(Pos2::new(800.0, 400.0)));
    }
    input
}

/// 场景骨架：演示序列（无磁盘读写）+ 合成纹理 + Develop 模块。
fn base(ctx: &egui::Context) -> AppState {
    let mut s = AppState::demo();
    s.install_demo_textures(ctx);
    s.module = Module::Develop;
    s
}

/// S1：清掉演示蒙版，换一张单形状线性蒙版并选中。
fn scenario_s1(ctx: &egui::Context) -> AppState {
    let mut s = base(ctx);
    s.params.masks.clear();
    let mut m = mask::new_mask(ShapeKind::Linear);
    if let Some(comp) = m["components"][0].as_object_mut() {
        comp.insert("x1".to_owned(), json!(0.30));
        comp.insert("y1".to_owned(), json!(0.40));
        comp.insert("x2".to_owned(), json!(0.50));
        comp.insert("y2".to_owned(), json!(0.72));
        comp.insert("feather".to_owned(), json!(45.0));
    }
    s.params.masks.push(m);
    s.mask_sel = Some(0);
    s.collapsed.remove("masks");
    s
}

/// S2：三形状组（线性 + 径向 + 200 点笔画 brush），最坏情况的每帧分配。
fn scenario_s2(ctx: &egui::Context) -> AppState {
    let mut s = base(ctx);
    s.params.masks.clear();
    let mut m = mask::new_mask(ShapeKind::Linear);
    let comps = m["components"].as_array_mut().unwrap();
    comps.push(mask::new_component(ShapeKind::Radial));
    let mut brush = mask::new_component(ShapeKind::Brush);
    // 一笔画 200 个点：坐标沿对角线排布，覆盖画笔渲染整条路径。
    let points: Vec<[f64; 2]> = (0..200)
        .map(|i| {
            let t = i as f64 / 199.0;
            [0.10 + 0.6 * t, 0.20 + 0.5 * t]
        })
        .collect();
    brush["strokes"] = json!([{ "points": points, "erase": false }]);
    comps.push(brush);
    s.params.masks.push(m);
    s.mask_sel = Some(0);
    s.collapsed.remove("masks");
    s
}

/// S3：曲线组展开（RGB 通道），dock 停在调整页。
fn scenario_s3(ctx: &egui::Context) -> AppState {
    let mut s = base(ctx);
    s.dock_tab = DockTab::Adjust;
    s.collapsed.remove("curve");
    s.curve_chan = 0;
    s
}

/// S4：无蒙版、直方图可见（demo 纹理自带一份 hist）。
fn scenario_s4(ctx: &egui::Context) -> AppState {
    let mut s = base(ctx);
    s.params.masks.clear();
    s.mask_sel = None;
    s.dock_tab = DockTab::Adjust;
    s
}

/// S5：600 帧序列 + 底部去闪亮度曲线可见。
fn scenario_s5(ctx: &egui::Context) -> AppState {
    let mut s = base(ctx);
    use aevum_core::metrics::FrameMetrics;
    use aevum_core::sequence::{Frame, Sequence};
    let n = 600usize;
    let frames: Vec<Frame> = (0..n)
        .map(|i| Frame {
            index: i,
            name: format!("bench_{i:04}.jpg"),
            path: std::path::PathBuf::from(format!("bench_{i:04}.jpg")),
        })
        .collect();
    s.seq = Some(Sequence {
        root: std::path::PathBuf::from("基准序列"),
        frames,
    });
    s.metrics = (0..n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let flicker = 0.03 * (i as f64 * 0.13).sin();
            let mean = 0.30 + 0.15 * t + flicker;
            Some(FrameMetrics {
                mean,
                median: mean,
                center: mean * 1.04,
                sharp: 118.0,
            })
        })
        .collect();
    s.def_means = s
        .metrics
        .iter()
        .map(|m| {
            m.map(|m| aevum_core::deflicker::FrameMeans {
                luma: m.mean,
                rgb: [m.mean; 3],
            })
        })
        .collect();
    s.recompute_deflicker();
    s.show_curves = true;
    s
}

/// 跑一个场景：50 帧预热 + 400 帧计时，返回 (avg, p95) 帧毫秒。
fn run_frames(state: &mut AppState, ctx: &egui::Context, hover: bool) -> (f64, f64) {
    const WARMUP: usize = 50;
    const MEASURE: usize = 400;
    let mut samples: Vec<f64> = Vec::with_capacity(MEASURE);
    for frame in 0..WARMUP + MEASURE {
        let t0 = Instant::now();
        let mut out = ctx.run_ui(input(hover), |ui| shell::ui(state, ui));
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        out.shapes.clear();
        out.textures_delta.clear();
        if frame >= WARMUP {
            samples.push(dt);
        }
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    let avg = samples.iter().sum::<f64>() / samples.len() as f64;
    let p95 = samples[(samples.len() as f64 * 0.95) as usize % samples.len()];
    (avg, p95)
}

fn report(name: &str, state: AppState, ctx: &egui::Context, hover: bool) {
    let mut state = state;
    // 缓存首建发生在前两帧；计时循环自带 50 帧预热，稳态即缓存命中态。
    let (avg, p95) = run_frames(&mut state, ctx, hover);
    println!("{name}  avg={avg:.3}ms  p95={p95:.3}ms");
}

#[test]
#[ignore = "性能基准：cargo test --release -p aevum-ui --lib -- --ignored --nocapture bench"]
fn bench_scenarios() {
    for (name, hover) in [
        ("S1", true),
        ("S2", true),
        ("S3", false),
        ("S4", false),
        ("S5", false),
    ] {
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx, 1.0);
        let state = match name {
            "S1" => scenario_s1(&ctx),
            "S2" => scenario_s2(&ctx),
            "S3" => scenario_s3(&ctx),
            "S4" => scenario_s4(&ctx),
            _ => scenario_s5(&ctx),
        };
        report(&format!("{name} "), state, &ctx, hover);
    }
}

/// 路径级微基准：改前每帧执行的构建 vs 改后缓存命中的取值，
/// 单位 µs/次——给「缓存命中改善数字」一条直接的证据链。
#[test]
#[ignore = "微基准：cargo test --release -p aevum-ui --lib -- --ignored --nocapture bench_micro"]
fn bench_micro() {
    const N: usize = 2000;
    let ctx = egui::Context::default();
    crate::theme::apply(&ctx, 1.0);

    // M1 蒙版几何：S2 同款三形状组（含 200 点笔画）。
    let mut state = scenario_s2(&ctx);
    let raw = state.params.masks[0].clone();

    let t0 = Instant::now();
    let mut sink = 0usize;
    for _ in 0..N {
        sink += crate::shell::mask_geos(&raw).len(); // 改前路径：逐键 JSON 解析
    }
    let old_geo = t0.elapsed().as_secs_f64() * 1e6 / N as f64;

    let _ = state.mask_geos_cached(0); // 首建
    let t0 = Instant::now();
    for _ in 0..N {
        sink += state.mask_geos_cached(0).len(); // 改后路径：指纹命中
    }
    let new_geo = t0.elapsed().as_secs_f64() * 1e6 / N as f64;
    assert!(sink <= N * 8);

    // M1b 常规情形：单形状线性蒙版（无笔画）。
    let mut linear_state = scenario_s1(&ctx);
    let linear_raw = linear_state.params.masks[0].clone();
    let t0 = Instant::now();
    for _ in 0..N {
        sink += crate::shell::mask_geos(&linear_raw).len();
    }
    let old_geo1 = t0.elapsed().as_secs_f64() * 1e6 / N as f64;
    let _ = linear_state.mask_geos_cached(0);
    let t0 = Instant::now();
    for _ in 0..N {
        sink += linear_state.mask_geos_cached(0).len();
    }
    let new_geo1 = t0.elapsed().as_secs_f64() * 1e6 / N as f64;

    // M2 曲线 LUT：RGB 通道默认点。
    let pts = vec![[0.0, 0.0], [128.0, 128.0], [255.0, 255.0]];
    let t0 = Instant::now();
    let mut sink2 = 0.0f32;
    for _ in 0..N {
        sink2 += aevum_core::grade::pchip_lut(&pts, 129).iter().sum::<f32>(); // 改前路径：129 点 PCHIP 重算
    }
    let old_lut = t0.elapsed().as_secs_f64() * 1e6 / N as f64;

    let t0 = Instant::now();
    for _ in 0..N {
        sink2 += crate::widgets::curve_lut_fingerprint(&pts) as f32; // 改后路径：指纹比对
    }
    let new_lut = t0.elapsed().as_secs_f64() * 1e6 / N as f64;
    assert!(sink2.is_finite());

    println!("M1 mask_geos     old={old_geo:.2}µs  cached={new_geo:.2}µs  speedup={:.1}x", old_geo / new_geo);
    println!("M1b linear-only  old={old_geo1:.2}µs  cached={new_geo1:.2}µs  speedup={:.1}x", old_geo1 / new_geo1);
    println!("M2 pchip_lut     old={old_lut:.2}µs  fp-hit={new_lut:.2}µs  speedup={:.1}x", old_lut / new_lut);
}
