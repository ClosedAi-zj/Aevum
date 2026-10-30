//! 运镜（Ken Burns）接进导出：取景框逐帧走动、推近走双线性、缩小走面积平均、
//! 关着或两框重合时一个像素都不重采样，以及框跟着**输出**帧号走（与变速同路）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use image::{Rgb, RgbImage};
use lumen_core::crop::{Ease, FULL, Motion, Viewfinder};
use lumen_core::develop::Ev;
use lumen_core::export::{
    OutputMode, RenderOptions, StillFormat, export_stills, export_stills_planned, plan_for,
};
use lumen_core::grade::GradeParams;
use lumen_core::retime::Retime;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lumen-motion-{tag}-{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn stills_options(out_dir: &Path) -> RenderOptions {
    RenderOptions {
        mode: OutputMode::Stills,
        format: StillFormat::Png8,
        out_dir: out_dir.to_path_buf(),
        ..RenderOptions::default()
    }
}

fn box_to(x: f64, y: f64, w: f64, h: f64) -> Motion {
    Motion {
        enabled: true,
        from: FULL,
        to: Viewfinder { x, y, w, h },
        ease: Ease::Linear,
    }
}

fn export(paths: &[PathBuf], options: &RenderOptions) -> Vec<PathBuf> {
    let params = vec![GradeParams::default(); paths.len()];
    let gains = vec![Ev::NONE; paths.len()];
    let offsets = vec![[0.0f64; 2]; paths.len()];
    let cancel = AtomicBool::new(false);
    export_stills(paths, &params, &gains, &offsets, options, &cancel, &|_| {}).unwrap()
}

/// `w × h` 的黑场，x/y ∈ `80..100` 一块白方：推近之后它该落在哪儿能算死。
fn block_source(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let mut img = RgbImage::new(120, 120);
    for y in 80..100u32 {
        for x in 80..100u32 {
            img.put_pixel(x, y, Rgb([255, 255, 255]));
        }
    }
    let path = dir.join("block.png");
    img.save(&path).unwrap();
    path
}

/// 400×300 的竖条纹（4 像素一周期）：面积平均会把它压成中性灰，
/// 双线性则留下一明一暗的假条纹——正好用来分开运镜的两条采样路。
fn stripe_source(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let mut img = RgbImage::new(400, 300);
    for y in 0..300u32 {
        for x in 0..400u32 {
            let v = if x % 4 < 2 { 255 } else { 0 };
            img.put_pixel(x, y, Rgb([v, v, v]));
        }
    }
    let path = dir.join("stripe.png");
    img.save(&path).unwrap();
    path
}

fn row(img: &RgbImage, y: u32) -> Vec<u8> {
    (0..img.width()).map(|x| img[(x, y)][0]).collect()
}

#[test]
fn the_box_walks_across_the_sequence() {
    let td = TempDir::new("walk");
    let src = block_source(&td.path.join("src"));
    let paths = vec![src.clone(); 3];

    let mut opt = stills_options(&td.path.join("out"));
    opt.motion = box_to(0.5, 0.5, 0.5, 0.5);
    let files = export(&paths, &opt);

    // 起点那帧：框铺满整幅 = 1:1，双线性正好落在整数格 → 与不动的导出逐字节相同。
    let plain = export(&paths, &stills_options(&td.path.join("plain")));
    assert_eq!(
        fs::read(&files[0]).unwrap(),
        fs::read(&plain[0]).unwrap(),
        "起点那帧被白白重采样了一次"
    );
    let first = image::open(&files[0]).unwrap().to_rgb8();
    assert_eq!(first[(90, 90)], Rgb([255, 255, 255]), "白块该还在源位");
    assert_eq!(first[(60, 60)], Rgb([0, 0, 0]));

    // 中间那帧：框 = 四元组的平均 (30,30,90,90)，缩到 0.75 → 白块整体往左上挪、还变大。
    let mid = image::open(&files[1]).unwrap().to_rgb8();
    assert_eq!(mid[(70, 70)], Rgb([255, 255, 255]));
    assert_eq!(mid[(85, 85)], Rgb([255, 255, 255]));
    assert_eq!(mid[(60, 60)], Rgb([0, 0, 0]));
    assert_eq!(mid[(95, 95)], Rgb([0, 0, 0]));

    // 末帧：右下 1/4 铺满整幅 = 推近 2×，白块（源 80..100）落到输出的 40..80。
    let last = image::open(&files[2]).unwrap().to_rgb8();
    assert_eq!(last[(60, 60)], Rgb([255, 255, 255]), "推近后白块该在正中");
    assert_eq!(last[(41, 41)], Rgb([255, 255, 255]));
    assert_eq!(last[(10, 10)], Rgb([0, 0, 0]));
    assert_eq!(last[(100, 100)], Rgb([0, 0, 0]));
    // 亚像素的那一格：(39,39) 采到源 (79.25, 79.25)，横竖各只有四分之一落进白块，
    // 双线性给出 255 × 0.25 × 0.25 ≈ 16 的爬坡值，而不是硬切成 0 或 255。
    let ramp = last[(39, 39)][0];
    assert!(
        (f64::from(ramp) - 15.9375).abs() < 1.0,
        "边缘没有被亚像素插值：{ramp}"
    );
    assert_eq!(last[(30, 30)], Rgb([0, 0, 0]));
}

#[test]
fn the_box_follows_the_output_frames_when_speed_is_on() {
    let td = TempDir::new("retime");
    let src = block_source(&td.path.join("src"));
    let paths = vec![src; 4];
    let params = vec![GradeParams::default(); 4];
    let gains = vec![Ev::NONE; 4];
    let offsets = vec![[0.0f64; 2]; 4];
    let cancel = AtomicBool::new(false);

    let mut opt = stills_options(&td.path.join("out"));
    opt.motion = box_to(0.5, 0.5, 0.5, 0.5);
    // 2× 变速把 4 帧压成 2 帧：运镜必须在这 2 帧里走完，而不是停在半路。
    let plan = plan_for(&Retime::uniform(2.0, 0.0), 4, 24.0).unwrap();
    let files = export_stills_planned(
        &paths,
        &params,
        &gains,
        &offsets,
        &opt,
        plan.as_ref(),
        &cancel,
        &|_| {},
    )
    .unwrap();
    assert_eq!(files.len(), 2, "2× 变速该只出 2 帧");
    let first = image::open(&files[0]).unwrap().to_rgb8();
    assert_eq!(first[(90, 90)], Rgb([255, 255, 255]), "首帧该停在起点框");
    assert_eq!(first[(60, 60)], Rgb([0, 0, 0]));
    let last = image::open(&files[1]).unwrap().to_rgb8();
    assert_eq!(
        last[(60, 60)],
        Rgb([255, 255, 255]),
        "末帧没走到终点框：变速把运镜截短了"
    );
    assert_eq!(last[(100, 100)], Rgb([0, 0, 0]));
}

#[test]
fn a_shrinking_box_averages_while_a_pushing_one_keeps_the_stripes() {
    let td = TempDir::new("sample");
    let src = stripe_source(&td.path.join("src"));
    // 两帧：末帧的框才走到终点，起点那帧还是整幅。
    let paths = vec![src; 2];

    // 整幅 → 100×75 是 4× 平均，终点框收到 0.9 是 3.6×：面积平均把条纹压成一片灰，
    // 只剩采不到整周期的那点火纹。
    let opt = RenderOptions {
        motion: box_to(0.0, 0.0, 0.9, 0.9),
        long_side: Some(100),
        ..stills_options(&td.path.join("area"))
    };
    let area = export(&paths, &opt);
    let img = image::open(&area[1]).unwrap().to_rgb8();
    let flat = row(&img, img.height() / 2);
    let spread = flat.iter().max().unwrap() - flat.iter().min().unwrap();
    let mean = flat.iter().map(|v| *v as f64).sum::<f64>() / flat.len() as f64;
    assert!(
        spread < 100,
        "缩小那一路退成了单点采样（起伏 {spread}）：面积平均没接上？{flat:?}"
    );
    assert!(
        mean > 100.0 && mean < 155.0,
        "面积平均该把黑白条纹压成中性灰：{mean}"
    );

    // 同一张源、同一个输出尺寸，只取左边一小块再铺满 = 推近：条纹必须还看得出明暗，
    // 两条采样路真的分了家。
    let opt2 = RenderOptions {
        motion: box_to(0.0, 0.4, 0.125, 0.1111),
        ..opt.clone()
    };
    let linear = export(&paths, &opt2);
    let img = image::open(&linear[1]).unwrap().to_rgb8();
    let striped = row(&img, img.height() / 2);
    let spread = striped.iter().max().unwrap() - striped.iter().min().unwrap();
    assert!(
        spread > 200,
        "推近那一路也被平均成灰了（起伏只有 {spread}）：双线性没接上？"
    );
}

#[test]
fn a_closed_or_still_box_renders_untouched() {
    let td = TempDir::new("noop");
    let src = block_source(&td.path.join("src"));
    let paths = vec![src; 2];
    let plain = export(&paths, &stills_options(&td.path.join("plain")));

    // 开关开着但两框重合：不该重采样，一像素都不许变。
    let mut equal = stills_options(&td.path.join("equal"));
    equal.motion = Motion {
        enabled: true,
        from: FULL,
        to: FULL,
        ease: Ease::Smooth,
    };
    let same = export(&paths, &equal);
    for (a, b) in same.iter().zip(&plain) {
        assert_eq!(
            fs::read(a).unwrap(),
            fs::read(b).unwrap(),
            "两框重合还去重采样"
        );
    }

    // 开关关着：哪怕两框不同也完全不动。
    let mut off = stills_options(&td.path.join("off"));
    off.motion = box_to(0.5, 0.5, 0.5, 0.5);
    off.motion.enabled = false;
    let same = export(&paths, &off);
    for (a, b) in same.iter().zip(&plain) {
        assert_eq!(
            fs::read(a).unwrap(),
            fs::read(b).unwrap(),
            "运镜关着还动了像素"
        );
    }
}
