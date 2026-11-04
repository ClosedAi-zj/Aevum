//! 无头工具面测试：对齐、关键帧插值、后台进度/取消与参数错误。
//!
//! 素材在临时目录里现生成：同一张程序纹理按已知位移平移，模板匹配应当
//! 找回相反的补偿量；导出再验证裁切与关键帧插值真的落到像素上。
//! 会话是进程级单例，所有用例用一把锁串行，避免互相踩状态。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use aevum_mcp::{dispatch, reset_session};

static LOCK: Mutex<()> = Mutex::new(());

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aevum-mcp-tools-{tag}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 确定性程序纹理：几组不同频率的波形叠一点哈希噪声，保证 2D 上唯一可匹配。
fn texture(x: i32, y: i32) -> [u8; 3] {
    let (fx, fy) = (x as f64, y as f64);
    let h = (x as i64 * 374_761_393 + y as i64 * 668_265_263) & 0x7fff_ffff;
    let noise = ((h ^ (h >> 13)) & 0xff) as f64;
    let v = 120.0
        + 55.0 * (fx * 0.17).sin()
        + 38.0 * (fy * 0.23).cos()
        + 22.0 * ((fx + fy) * 0.09).sin()
        + 10.0 * (noise / 255.0);
    let v = v.clamp(0.0, 255.0) as u8;
    [
        v,
        (v as f64 * 0.82 + fx * 0.12).clamp(0.0, 255.0) as u8,
        (v as f64 * 0.64 + fy * 0.18).clamp(0.0, 255.0) as u8,
    ]
}

/// 生成 3 帧：第 i 帧内容整体平移 `SHIFTS[i]`，对齐补偿量应当是它的相反数。
const SHIFTS: [(i32, i32); 3] = [(0, 0), (3, -2), (-2, 4)];

fn make_sequence(dir: &Path) {
    std::fs::create_dir_all(dir).expect("建序列目录");
    for (i, (dx, dy)) in SHIFTS.iter().enumerate() {
        let image = image::RgbImage::from_fn(128, 96, |x, y| {
            image::Rgb(texture(x as i32 - dx, y as i32 - dy))
        });
        image
            .save(dir.join(format!("f{i:02}.png")))
            .expect("写测试帧");
    }
}

fn call(tool: &str, args: Value) -> Result<Value, String> {
    dispatch(tool, &args)
}

fn ok(tool: &str, args: Value) -> Value {
    call(tool, args).unwrap_or_else(|e| panic!("{tool} 失败：{e}"))
}

fn mean_luma(path: &Path) -> f64 {
    let image = image::open(path)
        .unwrap_or_else(|e| panic!("读导出帧失败 {}：{e}", path.display()))
        .to_rgb8();
    let sum: f64 = image
        .pixels()
        .map(|p| (p.0[0] as f64 + p.0[1] as f64 + p.0[2] as f64) / 3.0)
        .sum();
    sum / (image.width() * image.height()).max(1) as f64
}

#[test]
fn align_keyframes_export_and_cancel() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    // 未打开序列：结构化错误，不 panic。
    let not_open = call("aevum_align", json!({})).unwrap_err();
    assert!(not_open.contains("aevum_open_sequence"), "{not_open}");

    let temp = TempDir::new("main");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    let opened = ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );
    assert_eq!(opened["frames"], 3);

    // 参数错误：未知估计器 / 参考帧越界 / 窗口、搜索半径越界。
    let bad = call("aevum_align", json!({"estimator": "orb"})).unwrap_err();
    assert!(bad.contains("template") && bad.contains("phase"), "{bad}");
    let bad = call("aevum_align", json!({"reference": 3})).unwrap_err();
    assert!(bad.contains("参考帧") && bad.contains("超出范围"), "{bad}");
    let bad = call("aevum_align", json!({"smooth_window": 1000})).unwrap_err();
    assert!(bad.contains("smooth_window"), "{bad}");
    let bad = call("aevum_align", json!({"max_shift": 5000})).unwrap_err();
    assert!(bad.contains("max_shift"), "{bad}");
    let bad = call("aevum_align", json!({"min_score": 2.0})).unwrap_err();
    assert!(bad.contains("min_score"), "{bad}");

    // 后台对齐：先拿 started，再轮询状态直到完成。
    let started = ok("aevum_align", json!({"background": true, "reference": 0}));
    assert_eq!(started["started"], true);
    assert_eq!(started["background"], true);
    let mut status = Value::Null;
    for _ in 0..600 {
        status = ok("aevum_align_status", json!({}));
        if status["running"] == json!(false) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(status["running"], json!(false), "{status}");
    assert!(status["last_align"].is_object(), "{status}");
    assert_eq!(status["last_align"]["frames"], 3);

    // 同步对齐：模板匹配应找回与内容平移相反的补偿量。
    let report = ok(
        "aevum_align",
        json!({"reference": 0, "estimator": "template", "smooth": false}),
    );
    assert_eq!(report["frames"], 3);
    assert_eq!(report["reference"], 0);
    assert_eq!(report["estimator"], "template");
    assert_eq!(report["stored"], true);
    let series = report["series"].as_array().expect("series");
    for (i, &(dx, dy)) in SHIFTS.iter().enumerate() {
        let point = series
            .iter()
            .find(|p| p["frame"] == json!(i))
            .unwrap_or_else(|| panic!("缺少第 {i} 帧"));
        let got_x = point["dx"].as_f64().expect("dx");
        let got_y = point["dy"].as_f64().expect("dy");
        // 补偿量 = -内容平移；允许亚像素拟合误差。
        assert!(
            (got_x - (-(dx as f64))).abs() < 1.5,
            "第 {i} 帧 dx={got_x}，期望 {}",
            -dx
        );
        assert!(
            (got_y - (-(dy as f64))).abs() < 1.5,
            "第 {i} 帧 dy={got_y}，期望 {}",
            -dy
        );
    }
    let crop = &report["crop"];
    let width = crop["w"].as_u64().expect("裁切宽") as u32;
    let height = crop["h"].as_u64().expect("裁切高") as u32;
    assert!(width > 0 && width <= 128, "{crop}");
    assert!(height > 0 && height <= 96, "{crop}");
    assert!(report["quality"]["kept"].as_u64().is_some());

    // 关键帧参数：第 0 帧 -2EV、第 2 帧 +2EV，第 1 帧应插到中间。
    let added = ok(
        "aevum_keyframe_add",
        json!({"frame": 0, "params": {"exposure": -2.0}}),
    );
    assert_eq!(added["count"], 1);
    let added = ok(
        "aevum_keyframe_add",
        json!({"frame": 2, "params": {"exposure": 2.0}}),
    );
    assert_eq!(added["frames"], json!([0, 2]));
    let listed = ok("aevum_keyframe_list", json!({"frame": 1}));
    assert_eq!(listed["at"]["is_keyframe"], false);
    let mid = listed["at"]["params"]["exposure"]
        .as_f64()
        .expect("插值曝光");
    assert!(mid.abs() < 1e-9, "第 1 帧曝光应插值到 0，实际 {mid}");

    // 导出：裁切与关键帧插值都落到输出像素上。
    let out_dir = temp.path().join("stills");
    let exported = ok(
        "aevum_export_stills",
        json!({
            "out_dir": out_dir.display().to_string(),
            "confirm": true
        }),
    );
    assert_eq!(exported["frames"], 3);
    assert_eq!(exported["aligned"], true);
    assert_eq!(exported["keyframes_applied"], true);
    assert_eq!(exported["crop"], report["crop"]);
    let first = image::open(out_dir.join("frame_000000.png")).expect("第 0 帧");
    // 导出会给编码器规整到偶数宽高，裁切框是奇数时少 1 px。
    assert_eq!((first.width(), first.height()), (width & !1, height & !1));
    let luma_dark = mean_luma(&out_dir.join("frame_000000.png"));
    let luma_mid = mean_luma(&out_dir.join("frame_000001.png"));
    let luma_bright = mean_luma(&out_dir.join("frame_000002.png"));
    assert!(
        luma_dark < luma_mid && luma_mid < luma_bright,
        "-2EV / 0EV / +2EV 的亮度应递增：{luma_dark:.1} {luma_mid:.1} {luma_bright:.1}"
    );

    // 精确目标尺寸：width/height 成对给出时 cover 放大后居中裁切，输出就是目标尺寸。
    let exact = temp.path().join("exact");
    let exported = ok(
        "aevum_export_stills",
        json!({
            "out_dir": exact.display().to_string(),
            "width": 96,
            "height": 96,
            "confirm": true
        }),
    );
    assert_eq!(exported["target_size"], json!([96, 96]));
    let first = image::open(exact.join("frame_000000.png")).expect("精确尺寸首帧");
    assert_eq!((first.width(), first.height()), (96, 96));

    // 与 long_side 冲突、奇数目标都要给出可读错误。
    let conflict = call(
        "aevum_export_stills",
        json!({
            "out_dir": temp.path().join("conflict").display().to_string(),
            "width": 96,
            "height": 96,
            "long_side": 512,
            "confirm": true
        }),
    )
    .unwrap_err();
    assert!(conflict.contains("二选一"), "{conflict}");
    let odd = call(
        "aevum_export_stills",
        json!({
            "out_dir": temp.path().join("odd").display().to_string(),
            "width": 95,
            "height": 96,
            "confirm": true
        }),
    )
    .unwrap_err();
    assert!(odd.contains("偶数"), "{odd}");

    // 有损容器：后缀跟着容器走，回执把质量与输出锐化档说清楚。
    let jpeg_dir = temp.path().join("jpeg");
    let exported = ok(
        "aevum_export_stills",
        json!({
            "out_dir": jpeg_dir.display().to_string(),
            "format": "jpeg",
            "quality": 60,
            "sharpen_for": "screen",
            "confirm": true
        }),
    );
    assert_eq!(exported["format"], json!("jpeg"));
    assert_eq!(exported["quality"], json!(60));
    assert_eq!(exported["sharpen_for"], json!("screen"));
    let first = image::open(jpeg_dir.join("frame_000000.jpg")).expect("JPEG 首帧");
    assert_eq!((first.width(), first.height()), (width & !1, height & !1));

    let webp_dir = temp.path().join("webp");
    ok(
        "aevum_export_stills",
        json!({
            "out_dir": webp_dir.display().to_string(),
            "format": "webp",
            "confirm": true
        }),
    );
    assert!(
        webp_dir.join("frame_000000.webp").is_file(),
        "WebP 后缀该跟着容器"
    );

    // 有损档当不了 ffmpeg 的中间帧：参数校验阶段就撞回，一帧都不渲染。
    let bad = call(
        "aevum_export_video",
        json!({
            "out_dir": temp.path().join("rejected").display().to_string(),
            "video_out": temp.path().join("rejected.mp4").display().to_string(),
            "format": "jpeg",
            "confirm": true
        }),
    )
    .unwrap_err();
    assert!(bad.contains("只能出静帧"), "{bad}");
    assert!(
        !temp.path().join("rejected").exists(),
        "被拒的导出不能留下中间帧"
    );

    // 质量越界与认不出的容器都要说话。
    let bad = call(
        "aevum_export_stills",
        json!({
            "out_dir": temp.path().join("bad-quality").display().to_string(),
            "quality": 0,
            "confirm": true
        }),
    )
    .unwrap_err();
    assert!(bad.contains("quality 要在 1..100"), "{bad}");
    let bad = call(
        "aevum_export_stills",
        json!({
            "out_dir": temp.path().join("bad-format").display().to_string(),
            "format": "heif",
            "confirm": true
        }),
    )
    .unwrap_err();
    assert!(bad.contains("未知格式 heif"), "{bad}");

    // 工程保存要带上关键帧与对齐结果。
    let saved = ok("aevum_project_save", json!({}));
    assert_eq!(saved["saved"], true);
    let project = ok("aevum_project_get", json!({}));
    assert_eq!(
        project["project"]["keyframes"].as_object().map(|m| m.len()),
        Some(2)
    );
    assert_eq!(
        project["project"]["align"]["offsets"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );

    // 删除关键帧：存在时成功，重复删除是结构化错误。
    let removed = ok("aevum_keyframe_remove", json!({"frame": 2}));
    assert_eq!(removed["frames"], json!([0]));
    let missing = call("aevum_keyframe_remove", json!({"frame": 2})).unwrap_err();
    assert!(missing.contains("没有关键帧"), "{missing}");

    // 空闲取消：两次都成功，且语义一致（幂等）。
    let cancel_one = ok("aevum_cancel", json!({}));
    let cancel_two = ok("aevum_cancel", json!({}));
    assert_eq!(cancel_one["cancelled"], false);
    assert_eq!(cancel_two["cancelled"], false);
    assert_eq!(cancel_one["was_running"], false);
}

/// 无头导出带上 `retime`：压帧真的少写文件、慢放真的多写，坏曲线给可读错误。
#[test]
fn retime_arg_remaps_the_exported_frames() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    let temp = TempDir::new("retime");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );

    let pngs = |dir: &Path| {
        std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(|e| e.ok()).count())
            .unwrap_or(0)
    };

    // 2×：3 帧压成 1 帧（∫du/2 = 0.5，⌊3×0.5⌋ = 1）。
    let fast = temp.path().join("fast");
    let done = ok(
        "aevum_export_stills",
        json!({
            "out_dir": fast.display().to_string(),
            "retime": {"speed": 2.0},
            "confirm": true,
        }),
    );
    assert_eq!(done["frames"], 1, "输出帧数按速度曲线换算");
    assert_eq!(done["source_frames"], 3);
    assert_eq!(done["retime"]["frames"], 1);
    assert_eq!(pngs(&fast), 1);

    // 0.5×：拆帧，写满 6 张。
    let slow = temp.path().join("slow");
    let done = ok(
        "aevum_export_stills",
        json!({
            "out_dir": slow.display().to_string(),
            "retime": {"speed": 0.5, "shutter": 0.5},
            "confirm": true,
        }),
    );
    assert_eq!(done["frames"], 6);
    assert_eq!(pngs(&slow), 6);

    // 不传 retime：还是 1:1 三张。
    let plain = temp.path().join("plain");
    let done = ok(
        "aevum_export_stills",
        json!({"out_dir": plain.display().to_string(), "confirm": true}),
    );
    assert_eq!(done["frames"], 3);
    assert_eq!(done["retime"], Value::Null);

    // 坏参数在启动前就拦住，一个像素都不写。
    let bad = call(
        "aevum_export_stills",
        json!({
            "out_dir": temp.path().join("bad").display().to_string(),
            "retime": {"speed": 0},
            "confirm": true,
        }),
    )
    .unwrap_err();
    assert!(bad.contains("倍速必须大于 0"), "{bad}");
    let both = call(
        "aevum_export_stills",
        json!({
            "out_dir": temp.path().join("bad2").display().to_string(),
            "retime": {"speed": 2.0, "keys": [{"at": 0.0, "speed": 2.0}]},
            "confirm": true,
        }),
    )
    .unwrap_err();
    assert!(both.contains("只能选一种写法"), "{both}");
    let loose = call(
        "aevum_export_stills",
        json!({
            "out_dir": temp.path().join("bad3").display().to_string(),
            "retime": {"shutter": 1.5},
            "confirm": true,
        }),
    )
    .unwrap_err();
    assert!(loose.contains("shutter"), "{loose}");
    assert!(!temp.path().join("bad").exists(), "拦下时不该建输出目录");
}

#[test]
fn export_video_audio_options_are_validated() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    let temp = TempDir::new("audio");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );
    let out_dir = temp.path().join("out");
    std::fs::create_dir_all(&out_dir).expect("建输出目录");
    let video_out = temp.path().join("cut.mp4");

    let call_with = |extra: Value| {
        let mut args = json!({
            "out_dir": out_dir.display().to_string(),
            "video_out": video_out.display().to_string(),
            "confirm": true,
        });
        let map = args.as_object_mut().expect("参数对象");
        for (key, value) in extra.as_object().expect("附加参数").clone() {
            map.insert(key, value);
        }
        call("aevum_export_video", args)
    };

    for (extra, needle) in [
        (json!({"delay": -1.0}), "delay"),
        (json!({"gain": 900.0}), "gain"),
        (json!({"fade_in": -0.5}), "fade_in"),
        (json!({"fade_out": "long"}), "fade_out"),
        (json!({"loop": "yes"}), "loop"),
        (json!({"trim": 1}), "trim"),
        (json!({"acodec": "flac"}), "acodec"),
        (
            json!({"audio": temp.path().join("missing.mp3").display().to_string()}),
            "不存在",
        ),
    ] {
        let err = call_with(extra.clone()).expect_err("应当拒绝");
        assert!(err.contains(needle), "{extra}：{err}");
    }
}

/// 交付预设：清单可发现、数值逐项对得上内置表、拼错 key 立刻报错并给出可用清单。
///
/// 分工说明：「预设真的进了渲染」由 core 的
/// `export_video::every_number_of_the_vertical_delivery_preset_reaches_the_render` 咬住
/// （命令层 + 像素层）；这里只验工具面的**可发现性**与**报错顺序**。两条都不需要
/// ffmpeg，所以在任何机器上都有结果，不会因为缺 ffmpeg 变成空壳。
///
/// Delivery presets: the list is discoverable, its numbers match the built-in table, and a
/// typo is rejected with the available keys. "The preset really reaches the render" is
/// covered in core (command layer + pixel layer); this checks discoverability and error
/// ordering only. Neither needs ffmpeg, so this test is never an empty shell.
#[test]
fn a_delivery_preset_is_discoverable_and_a_typo_is_reported_before_ffmpeg() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    let temp = TempDir::new("delivery");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );

    let info = ok("aevum_sequence_info", json!({}));
    let list = info["delivery"]
        .as_array()
        .expect("aevum_sequence_info 必须带 delivery 清单：agent 要传 delivery，就得先知道 key");
    let douyin = list
        .iter()
        .find(|p| p["key"] == "douyin-vertical")
        .expect("清单里必须有抖音竖屏");
    assert_eq!(douyin["width"], 1080);
    assert_eq!(douyin["height"], 1920);
    assert_eq!(douyin["fps"], 30.0);
    assert_eq!(douyin["codec"], "h264");
    assert_eq!(douyin["crf"], 20);

    let out_dir = temp.path().join("out");
    let video_out = temp.path().join("clip.mp4");
    let err = call(
        "aevum_export_video",
        json!({
            "out_dir": out_dir.display().to_string(),
            "video_out": video_out.display().to_string(),
            "delivery": "douyin-verticle",
            "confirm": true,
        }),
    )
    .expect_err("拼错的 key 必须被拒绝");
    assert!(err.contains("未知交付预设"), "{err}");
    assert!(
        err.contains("douyin-vertical"),
        "报错必须把可用 key 带回去，agent 不该靠猜第二遍：{err}"
    );
    assert!(
        !err.contains("ffmpeg"),
        "拼错 key 不该先撞上「找不到 ffmpeg」——校验要在干活之前：{err}"
    );
}

#[test]
fn keyframe_and_measure_parameter_errors() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    // 缺 root。
    let bad = call("aevum_open_sequence", json!({})).unwrap_err();
    assert!(bad.contains("root"), "{bad}");

    let temp = TempDir::new("errors");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );

    // 缺帧号 / 越界 / 未知字段。
    let bad = call("aevum_keyframe_add", json!({"params": {"exposure": 1}})).unwrap_err();
    assert!(bad.contains("frame"), "{bad}");
    let bad = call("aevum_keyframe_add", json!({"frame": 9})).unwrap_err();
    assert!(bad.contains("超出范围"), "{bad}");
    let bad = call(
        "aevum_keyframe_add",
        json!({"frame": 0, "params": {"exposure": 1, "no_such_field": 2}}),
    )
    .unwrap_err();
    assert!(
        bad.contains("未知调色字段") && bad.contains("no_such_field"),
        "{bad}"
    );
    let bad = call("aevum_keyframe_list", json!({"frame": 5})).unwrap_err();
    assert!(bad.contains("超出范围"), "{bad}");
    let bad = call("aevum_keyframe_remove", json!({})).unwrap_err();
    assert!(bad.contains("frame"), "{bad}");

    // 测光缓存与强制刷新。
    let measured = ok("aevum_measure", json!({}));
    assert_eq!(measured["valid"], 3);
    let refreshed = ok("aevum_measure", json!({"refresh": true}));
    assert_eq!(refreshed["frames"], 3);
    assert_eq!(refreshed["long_side"], 512);
}

#[test]
fn cancel_interrupts_background_align() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    // 用更多、更大的帧，让后台对齐有足够时间接收取消请求。
    let temp = TempDir::new("cancel");
    let seq_dir = temp.path().join("frames");
    std::fs::create_dir_all(&seq_dir).expect("建序列目录");
    for i in 0..24 {
        let (dx, dy) = (i % 5, i % 3);
        let image = image::RgbImage::from_fn(320, 240, |x, y| {
            image::Rgb(texture(x as i32 - dx, y as i32 - dy))
        });
        image
            .save(seq_dir.join(format!("f{i:03}.png")))
            .expect("写测试帧");
    }
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );
    let started = ok("aevum_align", json!({"background": true}));
    assert_eq!(started["started"], true);

    let cancel = ok("aevum_cancel", json!({}));
    // 任务可能已经跑完；只要取消调用本身成功、字段齐全即可。
    assert!(cancel["cancelled"].is_boolean());
    assert!(cancel["was_running"].is_boolean());

    // 幂等：再取消一次也成功。
    let again = ok("aevum_cancel", json!({}));
    assert!(again["cancelled"].is_boolean());

    // 等后台收尾，确认进程状态没有卡在 running。
    let mut status = Value::Null;
    for _ in 0..1000 {
        status = ok("aevum_align_status", json!({}));
        if status["running"] == json!(false) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(status["running"], json!(false), "{status}");
    // 取消命中时报告为空、错误为「已取消」；没命中就是正常报告。两者都不 panic。
    assert!(
        status["last_align"].is_object() || status["last_error"].is_string(),
        "{status}"
    );
}

#[test]
fn queue_tools_add_list_and_guard_run() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = TempDir::new("queue");
    let queue_file = temp.path().join("render_queue.json");
    // SAFETY: 本文件所有用例共用 LOCK 串行执行，不会并发读写这个变量；
    // 它只影响本进程的默认队列路径，测试结束就移除。
    unsafe { std::env::set_var("AEVUM_RENDER_QUEUE", queue_file.as_os_str()) };
    reset_session();

    let first = temp.path().join("first");
    let second = temp.path().join("second");
    make_sequence(&first);
    make_sequence(&second);

    let empty = ok("aevum_queue_list", json!({}));
    assert_eq!(empty["count"], 0);
    assert_eq!(empty["path"], json!(queue_file.display().to_string()));

    ok(
        "aevum_open_sequence",
        json!({"root": first.display().to_string()}),
    );
    let added = ok("aevum_queue_add", json!({}));
    assert_eq!(added["added"], 1);
    assert!(
        added["items"][0]["out"]
            .as_str()
            .unwrap()
            .ends_with("first_成片.mp4"),
        "{}",
        added["items"][0]["out"]
    );

    let added = ok(
        "aevum_queue_add",
        json!({
            "root": second.display().to_string(),
            "out": temp.path().join("second.mp4").display().to_string(),
            "width": 640, "height": 360, "fps": 30,
            "codec": "avc", "crf": 20, "preset": "slow", "keep_frames": true,
        }),
    );
    assert_eq!(added["count"], 2);
    assert_eq!(added["items"][1]["codec"], json!("h264"));
    assert_eq!(added["items"][1]["width"], json!(640));
    // 重复加入同一序列等于更新并挪到队尾。
    let dedup = ok(
        "aevum_queue_add",
        json!({"root": first.display().to_string()}),
    );
    assert_eq!(dedup["count"], 2);
    assert_eq!(dedup["added"], 2);
    assert!(queue_file.is_file(), "队列应立刻落盘");

    for (args, needle) in [
        (json!({"width": 640}), "同时"),
        (json!({"width": 641, "height": 360}), "偶数"),
        (json!({"fps": 0}), "fps"),
        (json!({"crf": 52}), "crf"),
        (json!({"codec": "av1"}), "h264"),
        (json!({"preset": "turbo"}), "预设"),
        (
            json!({"root": temp.path().join("missing").display().to_string()}),
            "不存在",
        ),
    ] {
        let err = call("aevum_queue_add", args.clone()).unwrap_err();
        assert!(err.contains(needle), "{args}：{err}");
    }

    let err = call("aevum_queue_run", json!({})).unwrap_err();
    assert!(err.contains("confirm"), "{err}");
    let status = ok("aevum_queue_status", json!({}));
    assert_eq!(status["running"], json!(false));
    assert_eq!(status["queue"]["pending"], 2);

    // 换序列不丢队列。
    ok(
        "aevum_open_sequence",
        json!({"root": first.display().to_string()}),
    );
    assert_eq!(ok("aevum_queue_list", json!({}))["count"], 2);

    let err = call("aevum_queue_remove", json!({"index": 3})).unwrap_err();
    assert!(err.contains("1..=2"), "{err}");
    let removed = ok("aevum_queue_remove", json!({"index": 2}));
    assert_eq!(removed["count"], 1);
    let cleared = ok("aevum_queue_clear", json!({}));
    assert_eq!(cleared["cleared"], 1);
    assert_eq!(ok("aevum_queue_list", json!({}))["count"], 0);

    let err = call("aevum_queue_run", json!({"confirm": true})).unwrap_err();
    assert!(err.contains("空的"), "{err}");
    let cancel = ok("aevum_queue_cancel", json!({}));
    assert_eq!(cancel["cancelled"], json!(false));

    unsafe { std::env::remove_var("AEVUM_RENDER_QUEUE") };
    reset_session();
}

/// C13：用**旧工具名**（改名前的 `lumen_*`）调用，结果必须与用新名**逐字节相同**。
///
/// 为什么强调逐字节：别名层如果「顺手」在响应里加一句「你用了旧名」，agent 的
/// 上下文就多一行无用信息、烧 token，还可能干扰它读结果。静默别名是刻意的设计。
///
/// 同时覆盖错误路径：旧名拼错时，错误文案里出现的是**规范名**（`aevum_nope`），
/// 这是有意的——用户按提示改成新名就对了。
#[test]
fn c13_legacy_tool_name_matches_canonical_byte_for_byte() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    let temp = TempDir::new("c13");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );

    // 成对的（旧名, 新名）。既有读操作，也有需要会话状态的。
    for (old, new) in [
        ("lumen_sequence_info", "aevum_sequence_info"),
        ("lumen_grade_get", "aevum_grade_get"),
        ("lumen_project_get", "aevum_project_get"),
        ("lumen_journal_list", "aevum_journal_list"),
        ("lumen_queue_list", "aevum_queue_list"),
        ("lumen_align_status", "aevum_align_status"),
        // 写操作也验一条：别名不能改变写行为的判定
        ("lumen_grade_set", "aevum_grade_set"),
    ] {
        let via_old = call(old, json!({}));
        let via_new = call(new, json!({}));
        assert_eq!(
            format!("{via_old:?}"),
            format!("{via_new:?}"),
            "{old} 与 {new} 的结果必须完全一致"
        );
    }

    // 错误路径：旧名拼错 → 报错文案用规范名，且新旧两条路径文案一致。
    let bad_old = call("lumen_nope", json!({})).unwrap_err();
    let bad_new = call("aevum_nope", json!({})).unwrap_err();
    assert_eq!(bad_old, bad_new, "两条路径的错误文案必须一致");
    assert!(
        bad_old.contains("aevum_nope"),
        "错误文案里应当是规范名：{bad_old}"
    );

    reset_session();
}

/// 穷举守卫：**每一个** MCP 工具都必须对 `is_write_op` 有明确表态。
///
/// 这张表是手写的、故意的。价值在于：新建一个工具时如果忘了登记 `is_write_op`，
/// 这里会红，而不是上线之后**操作日志静默少一行**。
///
/// 这正是本轮设计文档 §5 R9 点名的最担心的失效模式：编译通过、其它测试全绿、
/// 只有日志悄悄缺了一条，谁都不会发现。
#[test]
fn every_tool_has_an_explicit_write_classification() {
    use aevum_core::journal::is_write_op;
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    // true = 会改文件/工程/队列/导出的成功调用，必须进 op_journal.jsonl
    let expected: BTreeMap<&str, bool> = BTreeMap::from([
        ("aevum_open_sequence", false),
        ("aevum_sequence_info", false),
        ("aevum_measure", false),
        ("aevum_deflicker_analyze", false),
        ("aevum_grade_get", false),
        ("aevum_grade_set", true),
        ("aevum_grade_preset", true),
        ("aevum_grade_reset", true),
        ("aevum_grade_copy", true),
        ("aevum_keyframe_add", true),
        ("aevum_keyframe_remove", true),
        ("aevum_keyframe_list", false),
        ("aevum_render_preview", false),
        ("aevum_export_stills", true),
        ("aevum_export_video", true),
        ("aevum_hdr_merge", true),
        ("aevum_pano_merge", true),
        ("aevum_focus_merge", true),
        ("aevum_stack_merge", true),
        ("aevum_timeblend_merge", true),
        ("aevum_cull_evaluate", true),
        ("aevum_cull_extract", true),
        ("aevum_project_get", false),
        ("aevum_project_save", true),
        ("aevum_journal_list", false),
        ("aevum_xmp_read", false),
        ("aevum_xmp_write", true),
        ("aevum_cube_import", true),
        ("aevum_cube_export", true),
        ("aevum_align", true),
        ("aevum_align_status", false),
        ("aevum_cancel", false),
        ("aevum_queue_list", false),
        ("aevum_queue_add", true),
        ("aevum_queue_remove", true),
        ("aevum_queue_clear", true),
        ("aevum_queue_run", true),
        ("aevum_queue_status", false),
        ("aevum_queue_cancel", false),
    ]);

    let actual: BTreeSet<&str> = aevum_mcp::commands().iter().map(|c| c.id).collect();

    // 1. 表与实现必须一一对应：多一个工具、少一个工具都要在这里停下来。
    for id in &actual {
        assert!(
            expected.contains_key(id),
            "工具 {id} 没有在 every_tool_has_an_explicit_write_classification 里表态：\
             请先想清楚它算不算写操作，再补进这张表"
        );
    }
    for id in expected.keys() {
        assert!(actual.contains(id), "表里登记了 {id}，但 commands() 里没有");
    }

    // 2. 每一格的对错。
    for (id, &should_be_write) in &expected {
        assert_eq!(
            is_write_op(id),
            should_be_write,
            "{id} 的 is_write_op 与分类表不一致（期望 {should_be_write}）"
        );
        // 3. 旧名必须与规范名同判：别名不能改变「算不算写」。
        let legacy = id.replacen("aevum_", "lumen_", 1);
        assert_eq!(
            is_write_op(&legacy),
            should_be_write,
            "{legacy} 作为旧名，判定必须与 {id} 一致"
        );
    }
}

/// 工具集与 `aevum_core::mcp_routes::TOOL_ROUTES` 必须一一对应。
///
/// 那张表记录「每个工具在附着模式下落到哪条 GUI 命令」。多一个工具没登记，
/// 附着模式就路由不到它；登记了一个不存在的工具，就是一条永远不会被调到的死路。
///
/// 价值同上一张表：新建工具时忘了登记，这里会红在本地，而不是上线之后
/// 由用户报告「附着模式不认识 aevum_xxx」。
#[test]
fn tool_set_matches_the_mcp_route_table() {
    use std::collections::BTreeSet;

    let actual: BTreeSet<&str> = aevum_mcp::commands().iter().map(|c| c.id).collect();
    let routed: BTreeSet<&str> = aevum_core::mcp_routes::TOOL_ROUTES
        .iter()
        .map(|route| route.tool)
        .collect();

    let unrouted: Vec<&str> = actual.difference(&routed).copied().collect();
    let phantom: Vec<&str> = routed.difference(&actual).copied().collect();
    assert!(
        unrouted.is_empty(),
        "这些工具没登记在 TOOL_ROUTES 里，附着模式路由不到：{unrouted:?}"
    );
    assert!(
        phantom.is_empty(),
        "TOOL_ROUTES 登记了不存在的工具：{phantom:?}"
    );
}

/// 反向守卫：路径表里标了「GUI 有对应命令」的，那条命令必须真的在命令目录里。
/// 这一条由 core 自己的测试保证，这里再从 mcp 视角确认一遍调用得到。
#[test]
fn routed_commands_point_at_real_catalog_entries() {
    for route in aevum_core::mcp_routes::TOOL_ROUTES {
        let Some(command) = route.command else {
            continue;
        };
        assert!(
            aevum_core::mcp_routes::COMMANDS.contains(&command),
            "{} 指向的 {command} 不在命令目录里",
            route.tool
        );
    }
}

/// 无头堆栈 / 时间合成：小尺寸合成序列上各跑一条正例（成片存在、尺寸对），
/// 再各钉一条负例（缺 confirm、未知枚举值）。
#[test]
fn stack_and_timeblend_merge_end_to_end() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    let temp = TempDir::new("stack-tb");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );

    // —— aevum_stack_merge ——
    // 负例先行：缺 confirm 的批量写必须被拒；未知统计量必须被拒。
    let bad = call("aevum_stack_merge", json!({"frames": [0, 1, 2]})).unwrap_err();
    assert!(bad.contains("confirm"), "{bad}");
    let bad = call(
        "aevum_stack_merge",
        json!({"frames": [0, 1, 2], "mode": "geometric", "confirm": true}),
    )
    .unwrap_err();
    assert!(
        bad.contains("未知堆栈模式") && bad.contains("lighten"),
        "{bad}"
    );
    // 正例：三帧取平均，成片存在且与源帧同尺寸。
    let out = temp.path().join("stack-out");
    let result = ok(
        "aevum_stack_merge",
        json!({
            "frames": [0, 1, 2],
            "mode": "average",
            "out_dir": out.display().to_string(),
            "confirm": true,
        }),
    );
    assert_eq!(result["kind"], "stack");
    assert_eq!(result["frames"], 3);
    let path = PathBuf::from(result["path"].as_str().expect("成片路径"));
    assert!(path.is_file(), "成片该存在：{}", path.display());
    let image = image::open(&path).expect("读成片");
    assert_eq!((image.width(), image.height()), (128, 96));

    // —— aevum_timeblend_merge ——
    // 负例：未知布局；bands 超过帧数。
    let bad = call(
        "aevum_timeblend_merge",
        json!({"frames": [0, 1, 2], "layout": "radial", "confirm": true}),
    )
    .unwrap_err();
    assert!(
        bad.contains("未知布局") && bad.contains("sky-ground"),
        "{bad}"
    );
    let bad = call(
        "aevum_timeblend_merge",
        json!({"frames": [0, 1, 2], "layout": "bands", "bands": 5, "confirm": true}),
    )
    .unwrap_err();
    assert!(bad.contains("bands"), "{bad}");
    // 正例：三带时间切片，一带一帧，成片存在且同尺寸。
    let out = temp.path().join("tb-out");
    let result = ok(
        "aevum_timeblend_merge",
        json!({
            "frames": [0, 1, 2],
            "layout": "bands",
            "bands": 3,
            "out_dir": out.display().to_string(),
            "confirm": true,
        }),
    );
    assert_eq!(result["kind"], "timeblend");
    let path = PathBuf::from(result["path"].as_str().expect("成片路径"));
    assert!(path.is_file(), "成片该存在：{}", path.display());
    let image = image::open(&path).expect("读成片");
    assert_eq!((image.width(), image.height()), (128, 96));

    reset_session();
}

/// 无头筛选：evaluate 出逐帧裁决（结构完整），extract 按显式 exclude 出成片；
/// 缺 confirm、下标越界与「全剔光」各有负例。
#[test]
fn cull_evaluate_and_extract_end_to_end() {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_session();

    let temp = TempDir::new("cull");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    ok(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    );

    // —— aevum_cull_evaluate ——
    // 负例：曝光下限高于上限，当场拒绝。
    let bad = call(
        "aevum_cull_evaluate",
        json!({"ev_low": 1.0, "ev_high": -1.0}),
    )
    .unwrap_err();
    assert!(bad.contains("下限"), "{bad}");
    // 正例：默认判据出逐帧裁决；结构完整、帧数对得上。
    let result = ok("aevum_cull_evaluate", json!({}));
    assert_eq!(result["frames"].as_array().expect("逐帧裁决").len(), 3);
    assert_eq!(
        result["rejected"].as_u64().expect("判废数"),
        result["indices"].as_array().expect("判废下标").len() as u64
    );
    assert!(result["band"].is_array());

    // —— aevum_cull_extract ——
    // 负例：非 dry_run 缺 confirm；exclude 下标越界；全部剔除等于没有可留的帧。
    let target = temp.path().join("kept");
    let bad = call(
        "aevum_cull_extract",
        json!({"target": target.display().to_string(), "exclude": [0]}),
    )
    .unwrap_err();
    assert!(bad.contains("confirm"), "{bad}");
    let bad = call(
        "aevum_cull_extract",
        json!({"target": target.display().to_string(), "exclude": [99], "confirm": true}),
    )
    .unwrap_err();
    assert!(bad.contains("越界"), "{bad}");
    let bad = call(
        "aevum_cull_extract",
        json!({
            "target": target.display().to_string(),
            "exclude": [0, 1, 2],
            "confirm": true,
        }),
    )
    .unwrap_err();
    assert!(bad.contains("没有可保留的帧"), "{bad}");
    // dry_run 不写任何东西、也不要 confirm。
    let dry = ok(
        "aevum_cull_extract",
        json!({
            "target": target.display().to_string(),
            "exclude": [0],
            "dry_run": true,
        }),
    );
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["frames"], 2);
    assert!(!target.exists(), "dry_run 不该建目录");
    // 正例：剔掉第 0 帧，复制两帧进目标目录，逐字节数得出来。
    let result = ok(
        "aevum_cull_extract",
        json!({
            "target": target.display().to_string(),
            "exclude": [0],
            "confirm": true,
        }),
    );
    assert_eq!(result["copied"], 2);
    assert_eq!(result["excluded"], 1);
    // 只数帧：目标目录里还会带上 `.aevum/measures.json`（提取按设计把测光账一起
    // 搬走，无头测光和 GUI 现在共用同一本账，所以这里会真的存在）。
    //
    // Count frames only: the target also carries `.aevum/measures.json` (extraction ships
    // the metering ledger by design, and headless and the GUI now share one, so it is
    // actually there).
    let copied = std::fs::read_dir(&target)
        .expect("目标目录")
        .filter(|e| {
            e.as_ref()
                .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "png"))
        })
        .count();
    assert_eq!(copied, 2, "目标目录该有两帧");

    reset_session();
}

/// 把图片内容换成等长垃圾、再把 mtime 拨回原值：`(mtime, len)` 缓存键原样保住，
/// 解码却必然失败。这样「还能出结果」只可能来自盘上缓存，而不可能是又解了一遍图。
///
/// Replace a frame's bytes with same-length garbage and put the original mtime back: the
/// `(mtime, len)` cache key survives while decoding cannot succeed. Anything that still
/// yields numbers therefore came from the on-disk cache, not from another decode.
fn corrupt_keeping_cache_key(path: &Path) {
    let meta = std::fs::metadata(path).expect("读原图元数据");
    let modified = meta.modified().expect("原图 mtime");
    let len = meta.len();
    std::fs::write(path, vec![0u8; len as usize]).expect("覆盖原图内容");
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("重开原图")
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("恢复 mtime");
    let after = std::fs::metadata(path).expect("读改写后的元数据");
    assert_eq!(after.len(), len, "缓存键的长度必须不变");
    assert_eq!(
        after.modified().unwrap(),
        modified,
        "缓存键的 mtime 必须不变"
    );
}

/// 两个 f64 相差几个 ULP。只用于非负量（亮度、锐度），负数的位序不是单调的。
///
/// How many ULPs apart two f64 are. Only for non-negative values (luma, sharpness); the
/// bit order of negatives is not monotonic.
fn ulps_apart(a: f64, b: f64) -> u64 {
    (a.to_bits() as i128 - b.to_bits() as i128).unsigned_abs() as u64
}

/// 现测与读账之间允许的全部偏差：**不超过 1 ULP**。
///
/// 这 1 ULP 不是这次改动带来的，是 `serde_json` 默认的浮点解析只为速度、不保证
/// 逐位往返（`float_roundtrip` 特性没开）——GUI 第二次打开同一序列时也吃同一份
/// 精度。判据咬死它：再多一位就说明无头侧换了算法（长边、色彩口径或缩放方式）。
///
/// The entire budget between a fresh measure and a cache read: **at most 1 ULP**.
///
/// That ULP is not from this change; `serde_json`'s default float parsing favours speed
/// and does not promise a bit-exact round trip (`float_roundtrip` is off), and the GUI
/// eats the same precision on its second open. The assertion pins it: one bit more means
/// the headless side changed the algorithm (long side, colour weights or resampling).
fn assert_metrics_close(
    got: &aevum_core::metrics::FrameMetrics,
    want: &aevum_core::metrics::FrameMetrics,
    tag: &str,
) {
    for (name, a, b) in [
        ("mean", got.mean, want.mean),
        ("median", got.median, want.median),
        ("center", got.center, want.center),
        ("sharp", got.sharp, want.sharp),
    ] {
        assert!(
            ulps_apart(a, b) <= 1,
            "{tag} {name} 差了 {} ULP：{a} vs {b}（换算法了？）",
            ulps_apart(a, b)
        );
    }
}

/// 同一份账上读两次必须逐位相同：两边的差只该来自落盘精度，不该来自实现。
///
/// Two reads of the same ledger must be bit-identical: any difference must come from the
/// storage precision, never from the implementation.
fn assert_metrics_bit_eq(
    got: &aevum_core::metrics::FrameMetrics,
    want: &aevum_core::metrics::FrameMetrics,
    tag: &str,
) {
    assert_eq!(got.mean.to_bits(), want.mean.to_bits(), "{tag} mean");
    assert_eq!(got.median.to_bits(), want.median.to_bits(), "{tag} median");
    assert_eq!(got.center.to_bits(), want.center.to_bits(), "{tag} center");
    assert_eq!(got.sharp.to_bits(), want.sharp.to_bits(), "{tag} sharp");
}

fn means_of(v: &[Option<aevum_core::metrics::FrameMetrics>]) -> Vec<Option<f64>> {
    v.iter().map(|m| m.map(|x| x.mean)).collect()
}

fn sharp_of(v: &[Option<aevum_core::metrics::FrameMetrics>]) -> Vec<Option<f64>> {
    v.iter().map(|m| m.map(|x| x.sharp)).collect()
}

/// 无头测光先读 GUI 落在盘上的 `.aevum/measures.json`：原图已不可解码仍然出数，
/// 数值与 GUI 那趟的偏差 ≤ 1 ULP，裁决一条不差。
///
/// 变异验证：把 `session::measure_paths` 里的 `measures::measure_sequence` 改回
/// `metrics::measure_many`，这条必红（「所有帧都读不出来」）。
///
/// Headless metering reads the `.aevum/measures.json` the GUI left on disk: with the
/// originals no longer decodable it still produces numbers, within 1 ULP of the GUI pass,
/// with not one verdict changed.
///
/// Mutation check: swap `measures::measure_sequence` back to `metrics::measure_many` in
/// `session::measure_paths` and this goes red ("no frame could be read").
#[test]
fn headless_metrics_read_the_gui_disk_cache_without_decoding() {
    let temp = TempDir::new("cache-read");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    let paths: Vec<PathBuf> = (0..3)
        .map(|i| seq_dir.join(format!("f{i:02}.png")))
        .collect();

    let cache = aevum_core::measures::cache_path(&seq_dir);
    assert!(!cache.is_file(), "序列目录还不该有测光缓存");
    // GUI 侧入口先测一遍，账必须真的落盘。
    let (gui_metrics, gui_means, issues) =
        aevum_core::measures::measure_sequence(&seq_dir, &paths, 512);
    assert!(issues.is_empty(), "{issues:?}");
    assert!(cache.is_file(), "GUI 侧测完必须落盘：{}", cache.display());

    // 抹掉原图内容、保住缓存键。先证明这笔抹除是动真格的：现测必须全军覆没。
    for p in &paths {
        corrupt_keeping_cache_key(p);
    }
    let (probe, _, probe_issues) = aevum_core::metrics::measure_many(&paths, 512);
    assert!(
        probe.iter().all(Option::is_none) && probe_issues.len() == paths.len(),
        "原图还能解码，这条判据就没咬住任何东西：{probe_issues:?}"
    );

    // 新建无头会话：不解图也必须把这一序列的测光填出来。
    let mut session = aevum_mcp::Session {
        seq: Some(aevum_core::sequence::Sequence::open(&seq_dir).expect("扫序列目录")),
        ..Default::default()
    };
    session
        .ensure_metrics()
        .expect("原图已不可解码，无头仍应命中盘上测光缓存");

    let got = session.metrics.as_ref().expect("ensure_metrics 刚灌过测光");
    assert_eq!(got.len(), paths.len());
    for i in 0..paths.len() {
        let got_m = got[i].expect("命中缓存就该有指标");
        let want_m = gui_metrics[i].expect("GUI 那趟也测到了这一帧");
        assert_metrics_close(&got_m, &want_m, &format!("第 {i} 帧"));
        let got_d = session.def_means[i].expect("命中缓存就该有线性测光");
        let want_d = gui_means[i].expect("GUI 那趟也有线性测光");
        assert!(
            ulps_apart(got_d.luma, want_d.luma) <= 1,
            "第 {i} 帧 luma：{} vs {}",
            got_d.luma,
            want_d.luma
        );
        for c in 0..3 {
            assert!(
                ulps_apart(got_d.rgb[c], want_d.rgb[c]) <= 1,
                "第 {i} 帧 rgb[{c}]：{} vs {}",
                got_d.rgb[c],
                want_d.rgb[c]
            );
        }
    }

    // GUI 自己也拿同一份账再读一次：两边读的是同一本账，数必须逐位相同。
    let (gui_cached, _, issues) = aevum_core::measures::measure_sequence(&seq_dir, &paths, 512);
    assert!(issues.is_empty(), "{issues:?}");
    for i in 0..paths.len() {
        assert_metrics_bit_eq(
            &got[i].expect("无头读到了账"),
            &gui_cached[i].expect("GUI 读到了账"),
            &format!("第 {i} 帧"),
        );
    }

    // 1 ULP 的落盘精度不许改变任何一条裁决。
    let rule = aevum_core::cull::CullRule::default();
    let fresh = aevum_core::cull::evaluate(&means_of(&gui_metrics), &sharp_of(&gui_metrics), &rule);
    let cached = aevum_core::cull::evaluate(&means_of(got), &sharp_of(got), &rule);
    assert_eq!(cached.frames, fresh.frames, "读账后的逐帧裁决与现测不一致");
    assert_eq!(
        cached.rejected_indices(),
        fresh.rejected_indices(),
        "读账后的判废下标与现测不一致"
    );
}

/// 反向：无头先测，写回的账必须是 GUI 读得懂的同一份格式——原图抹掉后 GUI 侧
/// 入口仍能拿到同样的数（偏差 ≤ 1 ULP，同为落盘精度）。
///
/// The other direction: what headless writes must be the same format the GUI reads, so
/// after the originals are gone the GUI entry point still returns the same numbers
/// (within 1 ULP, the storage precision again).
#[test]
fn headless_metrics_write_a_cache_the_gui_can_read() {
    let temp = TempDir::new("cache-write");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir);
    let paths: Vec<PathBuf> = (0..3)
        .map(|i| seq_dir.join(format!("f{i:02}.png")))
        .collect();

    let cache = aevum_core::measures::cache_path(&seq_dir);
    assert!(!cache.is_file(), "序列目录还不该有测光缓存");

    let mut session = aevum_mcp::Session {
        seq: Some(aevum_core::sequence::Sequence::open(&seq_dir).expect("扫序列目录")),
        ..Default::default()
    };
    session.ensure_metrics().expect("无头测光");
    assert!(
        cache.is_file(),
        "无头测完必须按 GUI 的格式落盘：{}",
        cache.display()
    );
    let headless = session.metrics.clone().expect("刚测过");

    for p in &paths {
        corrupt_keeping_cache_key(p);
    }
    let (gui_metrics, _, issues) = aevum_core::measures::measure_sequence(&seq_dir, &paths, 512);
    assert!(issues.is_empty(), "GUI 读同一份账不该报错：{issues:?}");
    for i in 0..paths.len() {
        let got_m = gui_metrics[i].expect("GUI 命中无头写的账");
        let want_m = headless[i].expect("无头那趟测到了这一帧");
        assert_metrics_close(&got_m, &want_m, &format!("第 {i} 帧"));
    }
}
