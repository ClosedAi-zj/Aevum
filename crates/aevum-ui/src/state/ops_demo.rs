//! 演示数据：演示模式的进入与预置编辑、演示贴图的安装。
//!
//! 本文件是 `AppState` 操作面按域拆出来的一块，从 `state.rs` 原样搬来，只挪位置不改行为。
//! 拆分的缘由与边界见 `state.rs` 末尾的 `mod` 列表。
//!
//! Demo data: Entering demo mode, applying the canned edits, and installing the demo textures.
//!
//! One slice of `AppState`'s operation surface, split by domain and moved verbatim out
//! of `state.rs` -- relocated, not rewritten. The rationale and the boundaries live in
//! the `mod` list at the end of `state.rs`.

use super::*;

impl AppState {
    /// 测试与截图用的演示状态：不碰磁盘，序列与测光都是合成的。
    ///
    /// Demo state for tests and screenshots: touches no disk, with a synthetic sequence and
    /// synthetic metering.
    pub fn demo() -> Self {
        use aevum_core::sequence::Frame;
        let n = 48usize;
        let frames: Vec<Frame> = (0..n)
            .map(|i| Frame {
                index: i,
                name: format!("demo_{i:04}.jpg"),
                path: PathBuf::from(format!("demo_{i:04}.jpg")),
            })
            .collect();
        let mut s = Self::new();
        s.seq = Some(Sequence {
            root: PathBuf::from("演示序列"),
            frames,
        });
        s.metrics = (0..n)
            .map(|i| {
                let t = i as f64 / n as f64;
                let flicker = if i % 7 == 0 { -0.04 } else { 0.0 };
                let mean = 0.28 + 0.18 * t + flicker;
                Some(FrameMetrics {
                    mean,
                    median: mean,
                    center: mean * 1.05,
                    sharp: 120.0 + 10.0 * (i as f64).sin(),
                })
            })
            .collect();
        // 线性光测光也合成一份，形状跟着胶片条同调：每 7 帧一次整体掉档（曝光跳，
        // 三通道一起掉），每 5 帧一次偏绿（色闪，只有分通道看得见）。演示序列不带
        // 这两样，验收时「分通道」就是个死控件。
        s.def_means = (0..n)
            .map(|i| {
                let t = i as f64 / n as f64;
                let base = 0.045 + 0.03 * t;
                let scale = if i % 7 == 0 { 0.8 } else { 1.0 };
                let green = if i % 5 == 0 { 0.05 } else { 0.0 };
                let rgb = [
                    base * scale,
                    (base * 1.1 + green) * scale,
                    base * 1.3 * scale,
                ];
                let luma = deflicker::LUMA_RGB
                    .iter()
                    .zip(rgb)
                    .map(|(w, v)| w * v)
                    .sum::<f64>();
                Some(FrameMeans { luma, rgb })
            })
            .collect();
        s.issues = vec![(17, "解码失败：演示用缺帧".to_owned())];
        s.resolution = Some((4096, 2160));
        s.recompute_deflicker();
        s.ratings = (0..n)
            .filter_map(|i| match i % 11 {
                0 => Some((i, 5)),
                4 => Some((i, 3)),
                7 => Some((i, 4)),
                _ => None,
            })
            .collect();
        s.flags = (0..n)
            .filter_map(|i| match i % 9 {
                0 => Some((i, 1)),
                5 => Some((i, -1)),
                _ => None,
            })
            .collect();
        s.labels = (0..n)
            .filter(|i| i % 13 == 0)
            .map(|i| (i, 1 + (i / 13) as u8 % 4))
            .collect();
        s.status = "48 帧 · 演示序列（无磁盘读写）".to_owned();
        s.module = Module::Develop;
        s.selected = 12;
        s.apply_demo_edit();
        // 演示编辑不算用户改动：历史从这一份状态重新开始。
        s.history_reset("演示序列");
        s.headless = true;
        s
    }

    /// 截图用的一套演示编辑：一张径向局部蒙版，选中并展开面板。
    ///
    /// The demo edit used for screenshots: one radial local mask, selected, with its panel
    /// expanded.
    pub fn apply_demo_edit(&mut self) {
        let mut demo_mask = mask::new_mask(mask::ShapeKind::Radial);
        if let Some(comp) = demo_mask
            .get_mut("components")
            .and_then(Value::as_array_mut)
            .and_then(|list| list.first_mut())
            .and_then(Value::as_object_mut)
        {
            comp.insert("x1".to_owned(), json!(0.36));
            comp.insert("y1".to_owned(), json!(0.46));
            comp.insert("x2".to_owned(), json!(0.60));
            comp.insert("y2".to_owned(), json!(0.72));
            comp.insert("feather".to_owned(), json!(55));
        }
        demo_mask["amount"] = json!(85);
        demo_mask["invert"] = json!(false);
        demo_mask["params"] = json!({"exposure": 0.9, "vibrance": 20});
        // 再挖一个线性形状进去，演示与截图都能直接看到布尔和亮度范围。
        if let Some(list) = demo_mask["components"].as_array_mut() {
            let mut cut = mask::new_component(mask::ShapeKind::Linear);
            cut["blend"] = json!("subtract");
            cut["x1"] = json!(0.08);
            cut["y1"] = json!(0.70);
            cut["x2"] = json!(0.42);
            cut["y2"] = json!(0.86);
            cut["feather"] = json!(28);
            list.push(cut);
        }
        demo_mask["range"] = json!({
            "lum_on": true, "lum_min": 0.12, "lum_max": 0.62, "lum_feather": 0.08
        });
        self.params.masks.push(demo_mask);
        self.mask_sel = Some(self.params.masks.len() - 1);
        self.preset = "自定义".to_owned();
        self.collapsed.insert("basic");
        self.collapsed.insert("optics");
        self.collapsed.insert("effects");
        self.collapsed.remove("masks");
        self.preview_dirty = true;
    }

    /// 给无显示环境铺一套合成纹理，让画布与胶片条的绘制路径也被走到。
    ///
    /// Lay down a set of synthetic textures for display-less environments, so the canvas and
    /// filmstrip drawing paths are exercised too.
    pub fn install_demo_textures(&mut self, ctx: &Context) {
        let (w, h) = (960usize, 540usize);
        let mut pixels = Vec::with_capacity(w * h);
        let mut hist = [[0u32; 256]; 3];
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / w as f32;
                let v = y as f32 / h as f32;
                let horizon = 0.62;
                let k = (v / horizon).clamp(0.0, 1.0);
                let k = k * k * (3.0 - 2.0 * k);
                let r = (22.0 + (240.0 - 22.0) * k) * (1.0 - v * 0.35);
                let g = (30.0 + (150.0 - 30.0) * k) * (1.0 - v * 0.35);
                let b = (60.0 + (92.0 - 60.0) * k) * (1.0 - v * 0.15);
                let mut c = [
                    (r + t * 6.0).clamp(0.0, 255.0) as u8,
                    (g + t * 4.0).clamp(0.0, 255.0) as u8,
                    b.clamp(0.0, 255.0) as u8,
                ];
                if v > 0.72 + (t * 9.0).sin() * 0.02 {
                    c = [12, 14, 18];
                }
                for (ch, value) in c.iter().enumerate() {
                    hist[ch][*value as usize] += 1;
                }
                pixels.push(Color32::from_rgb(c[0], c[1], c[2]));
            }
        }
        self.hist = Some(hist);
        self.texture = Some(ctx.load_texture(
            "demo.preview",
            ColorImage::new([w, h], pixels),
            TextureOptions::LINEAR,
        ));
        self.texture_orig = self.texture.clone();
        self.preview_size = [w as u32, h as u32];
        // 合成直方图也是一次「预览落地」：直方图 Mesh 缓存换版。
        self.hist_version += 1;
        let n = self.frame_count();
        for i in 0..n {
            let shade = 30u8 + (i as u8).wrapping_mul(3);
            let img = ColorImage::filled([128, 72], Color32::from_rgb(shade, shade / 2, 40));
            let tex = ctx.load_texture(format!("demo.thumb.{i}"), img, TextureOptions::LINEAR);
            self.thumbs.insert(
                i,
                Thumb {
                    texture: tex,
                    size: [128, 72],
                },
            );
        }
        self.headless = false;
        self.preview_dirty = false;
    }
}
