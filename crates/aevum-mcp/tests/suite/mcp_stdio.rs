//! 端到端协议测试：真的起子进程、真的走 stdin/stdout。
//!
//! 不碰 GUI 和显示器；素材是临时生成的 3 张 48×32 PNG。

use std::io::{BufRead, BufReader, Lines, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const TOOLS: &[&str] = &[
    "aevum_open_sequence",
    "aevum_sequence_info",
    "aevum_measure",
    "aevum_deflicker_analyze",
    "aevum_grade_get",
    "aevum_grade_set",
    "aevum_grade_preset",
    "aevum_grade_reset",
    "aevum_grade_copy",
    "aevum_keyframe_add",
    "aevum_keyframe_remove",
    "aevum_keyframe_list",
    "aevum_render_preview",
    "aevum_export_stills",
    "aevum_export_video",
    "aevum_hdr_merge",
    "aevum_pano_merge",
    "aevum_focus_merge",
    "aevum_stack_merge",
    "aevum_timeblend_merge",
    "aevum_cull_evaluate",
    "aevum_cull_extract",
    "aevum_project_get",
    "aevum_project_save",
    "aevum_journal_list",
    "aevum_xmp_read",
    "aevum_xmp_write",
    "aevum_cube_import",
    "aevum_cube_export",
    "aevum_align",
    "aevum_align_status",
    "aevum_cancel",
    "aevum_queue_list",
    "aevum_queue_add",
    "aevum_queue_remove",
    "aevum_queue_clear",
    "aevum_queue_run",
    "aevum_queue_status",
    "aevum_queue_cancel",
    "aevum_sequences_scan",
    "aevum_batch_run",
];

struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    /// 子进程的配置目录。字段只为了在 `drop` 时跟着删掉目录，不参与断言。
    _config: TempDir,
}

impl Server {
    fn start() -> Self {
        // 子进程是个真二进制，`Session::default()` 会走到
        // `config_dir::ensure_adopted()` 去扫 `%APPDATA%`。测试不能去动开发机上的
        // 真实配置——那里有开发者的预设、图库，还有上一版留下的 `lumen-studio/`，
        // 一旦被搬走就找不回来了。所以把四个 override 全指到临时目录：
        // `config_dir::portable()` 看到任一 override 就整段跳过首启迁移。
        let config = TempDir::new("cfg");
        let mut child = Command::new(env!("CARGO_BIN_EXE_aevum-mcp"))
            .env("AEVUM_SETTINGS", config.path().join("settings.json"))
            .env("AEVUM_LIBRARY", config.path().join("galleries.json"))
            .env(
                "AEVUM_RENDER_QUEUE",
                config.path().join("render_queue.json"),
            )
            .env("AEVUM_PLUGINS", config.path().join("plugins"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("启动 aevum-mcp 子进程");
        let stdin = child.stdin.take().expect("子进程 stdin");
        let stdout = child.stdout.take().expect("子进程 stdout");
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
            _config: config,
        }
    }

    fn send(&mut self, message: &Value) {
        let text = serde_json::to_string(message).expect("序列化请求");
        writeln!(self.stdin, "{text}").expect("写 stdin");
        self.stdin.flush().expect("flush stdin");
    }

    fn recv(&mut self) -> Value {
        let line = self
            .lines
            .next()
            .expect("服务端提前退出")
            .expect("读 stdout");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("响应不是 JSON：{e}\n{line}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let response = self.recv();
        assert_eq!(response["id"], json!(id), "响应 id 不匹配：{response}");
        response
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": tool, "arguments": arguments}))["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("aevum-mcp-{tag}-{}-{stamp}", std::process::id()));
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

fn make_sequence(dir: &Path, frames: usize) {
    std::fs::create_dir_all(dir).expect("建序列目录");
    for i in 0..frames {
        let base = 50 + i as u8 * 45;
        let image = image::RgbImage::from_fn(48, 32, |x, y| {
            let shade = base.saturating_add(((x + y) / 6) as u8);
            image::Rgb([
                shade,
                shade.saturating_sub(12),
                255u8.saturating_sub(shade / 2),
            ])
        });
        image
            .save(dir.join(format!("f{i:02}.png")))
            .expect("写测试帧");
    }
}

/// 全景测试序列：中灰背景上放 40 个「灰度各不相同」的方块，每张左移 24 像素取样。
/// 每个方块角点是全图唯一的局部结构，特征匹配不会张冠李戴，平移可稳定复原。
fn make_pano_sequence(dir: &Path, frames: usize) {
    std::fs::create_dir_all(dir).expect("建序列目录");
    fn blob(k: usize) -> (i64, i64, i64, u8) {
        let bx = ((k * 37) % 100) as i64;
        let by = (((k * 23) % 40) + 4) as i64;
        let size = 4 + (k % 3) as i64;
        let gray = (25.0 + 205.0 * (k as f32) / 40.0) as u8;
        (bx, by, size, gray)
    }
    for i in 0..frames {
        let shift = 24 * i as i64;
        let image = image::RgbImage::from_fn(64, 48, |x, y| {
            let wx = x as i64 + shift;
            let wy = y as i64;
            let mut v = 128u8;
            for k in 0..40 {
                let (bx, by, size, gray) = blob(k);
                if wx >= bx && wx < bx + size && wy >= by && wy < by + size {
                    v = gray;
                    break;
                }
            }
            image::Rgb([v, v, v])
        });
        image
            .save(dir.join(format!("p{i:02}.png")))
            .expect("写全景测试帧");
    }
}

fn ok_json(result: &Value) -> Value {
    assert_eq!(result["isError"], json!(false), "工具应成功：{result}");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("tools/call 缺少 text");
    serde_json::from_str(text).unwrap_or_else(|e| panic!("工具输出不是 JSON：{e}\n{text}"))
}

/// 一个工具调用失败时取出错误文案，顺带断言里面该有某个词。
///
/// Pull the error text out of a failed tool call, asserting it mentions `needle`.
fn error_text(result: &Value, needle: &str) -> String {
    assert_eq!(result["isError"], json!(true), "{result}");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("错误文案不是字符串")
        .to_owned();
    assert!(
        text.contains(needle),
        "该报的是「{needle}」：{}",
        head(&text)
    );
    text
}

/// 断言里能打印的那一段：开头 60 字。
///
/// 漏出去时原文是两万字，照抄进 panic 消息会把 CI 日志刷爆——判据自己也得守这条。
///
/// The printable head: 60 characters. When something leaks the raw string is 20 KB, and
/// copying it into the panic message would flood the CI log, so the assertion obeys the
/// same rule it enforces.
fn head(text: &str) -> String {
    let mut out: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        out.push('…');
    }
    out
}

/// 错误文案必须有界：截断后 264 字上下，没截断就是两万字，差一个量级。
///
/// `clipped` 说明这一条本来就该被截（路径或名字够长），顺手也钉住「短文案别凭空多出
/// 省略号」。路径与 agent 传来的名字走的是同一套判据，所以文案里说的是「输入」。
///
/// The error text must stay bounded: about 264 characters clipped, 20 000 unclipped.
/// `clipped` says whether this one is long enough to be cut, which also pins the other
/// direction: a short message must not grow an ellipsis out of nowhere. Paths and the
/// names an agent sends share this assertion, hence the generic wording.
fn assert_bounded(text: &str, clipped: bool) {
    let width = text.chars().count();
    assert!(
        width < 400,
        "输入被原样回显了，{width} 字符：{}",
        head(text)
    );
    assert_eq!(
        text.contains('…'),
        clipped,
        "省略号该不该有对不上：{}",
        head(text)
    );
}

#[test]
fn stdio_session_end_to_end() {
    let temp = TempDir::new("e2e");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir, 3);
    let mut server = Server::start();

    // initialize：认识的版本原样回显。
    let init = server.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}
        }),
    );
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert!(result["capabilities"]["tools"].is_object());
    assert_eq!(result["serverInfo"]["name"], "aevum-studio");
    assert_eq!(result["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    server.notify("notifications/initialized");

    // ping 证明 notification 没有被当成请求回包。
    assert_eq!(server.request("ping", json!({}))["result"], json!({}));

    // tools/list：清单里的工具一个不少，每个都有 JSON Schema。
    let listed = server.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools 数组");
    assert_eq!(tools.len(), TOOLS.len());
    for expected in TOOLS {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == *expected)
            .unwrap_or_else(|| panic!("缺少工具 {expected}"));
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
    }

    // C15：`tools/list` 里**不许**出现任何旧工具名（`lumen_*`）。
    //
    // 旧名是静默别名，只接受调用、不对外宣传。这一条守的是「改了一半」：
    // 如果哪天有人把别名条目误加进 commands()，客户端会同时看到新旧两套名字，
    // agent 很可能挑旧的用，然后在别的客户端上失效。
    for tool in tools {
        let name = tool["name"].as_str().expect("工具名");
        assert!(
            !name.starts_with("lumen_"),
            "tools/list 不该出现旧工具名：{name}"
        );
        assert!(name.starts_with("aevum_"), "工具名必须带规范前缀：{name}");
    }

    // 打开临时序列。
    let opened = ok_json(&server.call(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    ));
    assert_eq!(opened["frames"], 3);
    assert_eq!(opened["name"], "frames");
    assert_eq!(opened["project_loaded"], false);

    // 部分字段调色，合并到当前参数。
    let graded = ok_json(&server.call(
        "aevum_grade_set",
        json!({"params": {"exposure": 0.5, "contrast": 12.0, "temperature": 8}}),
    ));
    assert_eq!(graded["preset"], "自定义");
    assert_eq!(graded["params"]["exposure"], 0.5);
    assert_eq!(graded["params"]["contrast"], 12.0);

    let info = ok_json(&server.call("aevum_sequence_info", json!({})));
    assert_eq!(info["frames"], 3);
    assert_eq!(info["params"]["exposure"], 0.5);
    assert_eq!(info["measured"], false);
    // 拍摄参数概览：这套夹具是 PNG，本来就没有 EXIF。字段必须在，agent 才能
    // 分清「文件里没有」和「还没查」；方向按中性 1 走，不因为读不到就报 0。
    assert_eq!(info["capture"]["orientation"], 1);
    assert_eq!(info["capture"]["orientation_mixed"], false);
    assert_eq!(info["capture"]["ev_read"], 0);
    assert!(info["capture"]["camera"].is_null());

    // 测光摘要：3 帧都有效，序列数组不会超长。
    let measured = ok_json(&server.call("aevum_measure", json!({})));
    assert_eq!(measured["frames"], 3);
    assert_eq!(measured["valid"], 3);
    assert_eq!(measured["stats"]["mean"]["count"], 3);
    assert_eq!(measured["series"].as_array().map(Vec::len), Some(3));

    let analyzed = ok_json(&server.call(
        "aevum_deflicker_analyze",
        json!({"window": 3, "strength": 100, "multi": false}),
    ));
    assert_eq!(analyzed["frames"], 3);
    assert!(analyzed["stats"]["jitter_before"].as_f64().is_some());
    assert_eq!(analyzed["channels"], json!(false));

    // 分通道：统计里要标成多通道，series 每项额外给出 R/G/B 三条 EV。
    let rgb = ok_json(&server.call(
        "aevum_deflicker_analyze",
        json!({"window": 3, "channels": true}),
    ));
    assert_eq!(rgb["channels"], json!(true));
    assert_eq!(rgb["stats"]["channels"], json!(true));
    assert_eq!(
        rgb["series"][0]["rgb"].as_array().map(Vec::len),
        Some(3),
        "分通道要把三通道 EV 报出来"
    );

    // 退回单通道，后面按老口径断言。
    ok_json(&server.call(
        "aevum_deflicker_analyze",
        json!({"window": 3, "channels": false}),
    ));

    // 预览落 PNG，尺寸保持源图（不放大）。
    let preview_path = temp.path().join("preview.png");
    let preview = ok_json(&server.call(
        "aevum_render_preview",
        json!({"frame": 1, "side": 64, "out": preview_path.display().to_string()}),
    ));
    assert_eq!(preview["width"], 48);
    assert_eq!(preview["height"], 32);
    assert!(preview_path.is_file());
    assert!(std::fs::metadata(&preview_path).expect("预览文件").len() > 0);
    // 单通道时三通道 EV 必须与整帧 EV 相同：老口径不能被动过。
    assert_eq!(preview["gain_rgb_ev"]["r"], preview["gain_ev"]);
    assert_eq!(preview["gain_rgb_ev"]["g"], preview["gain_ev"]);
    assert_eq!(preview["gain_rgb_ev"]["b"], preview["gain_ev"]);

    // 超过 4096 的预览要 confirm，避免 agent 无意间渲出巨大图。
    let too_big = server.call(
        "aevum_render_preview",
        json!({
            "out": temp.path().join("huge.png").display().to_string(),
            "side": 5000
        }),
    );
    assert_eq!(too_big["isError"], json!(true));
    assert!(!temp.path().join("huge.png").exists());

    // 批量写必须带 confirm。
    let refused = server.call(
        "aevum_export_stills",
        json!({"out_dir": temp.path().join("stills").display().to_string()}),
    );
    assert_eq!(refused["isError"], json!(true));
    assert!(
        refused["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("confirm")),
        "{refused}"
    );

    let stills_dir = temp.path().join("stills");
    let exported = ok_json(&server.call(
        "aevum_export_stills",
        json!({
            "out_dir": stills_dir.display().to_string(),
            "format": "png8",
            "confirm": true
        }),
    ));
    assert_eq!(exported["frames"], 3);
    for i in 0..3 {
        assert!(
            stills_dir.join(format!("frame_{i:06}.png")).is_file(),
            "缺少第 {i} 帧"
        );
    }
    assert!(
        !stills_dir.join("frame_000000.xmp").exists(),
        "没要旁车却写出来了"
    );

    // 断点续跑：已经写完的帧原样留着，只补缺的那一张，而且结果要说得清这一趟跳了几帧。
    std::fs::write(stills_dir.join("frame_000000.png"), b"sentinel").expect("写哨兵帧");
    std::fs::remove_file(stills_dir.join("frame_000001.png")).expect("删掉一帧");
    let resumed = ok_json(&server.call(
        "aevum_export_stills",
        json!({
            "out_dir": stills_dir.display().to_string(),
            "format": "png8",
            "resume": true,
            "confirm": true
        }),
    ));
    assert_eq!(resumed["skipped"], 2, "续跑报不出跳过了哪几帧");
    let kept = std::fs::read(stills_dir.join("frame_000000.png")).expect("读回哨兵帧");
    assert_eq!(kept, b"sentinel".as_slice(), "续跑把已经写完的帧重渲掉了");
    assert!(
        stills_dir.join("frame_000001.png").is_file(),
        "缺的那一帧没补回来"
    );
    // 不开 resume 时同一批必须全部重渲，跳过的读数也就得是 0。
    let rerun = ok_json(&server.call(
        "aevum_export_stills",
        json!({
            "out_dir": stills_dir.display().to_string(),
            "format": "png8",
            "confirm": true
        }),
    ));
    assert_eq!(rerun["skipped"], 0);
    assert!(
        std::fs::read(stills_dir.join("frame_000000.png"))
            .expect("读回第一帧")
            .len()
            > b"sentinel".len(),
        "没开续跑却把哨兵帧留下了"
    );

    // 旁车开关：勾上时每张静帧旁边多一个同名 .xmp（dc: 四项 + crs: 调色）。
    let sidecar_dir = temp.path().join("stills-xmp");
    let with_sidecar = ok_json(&server.call(
        "aevum_export_stills",
        json!({
            "out_dir": sidecar_dir.display().to_string(),
            "format": "png8",
            "xmp_sidecar": true,
            "confirm": true
        }),
    ));
    assert_eq!(with_sidecar["frames"], 3);
    for i in 0..3 {
        assert!(
            sidecar_dir.join(format!("frame_{i:06}.xmp")).is_file(),
            "缺少第 {i} 帧的旁车"
        );
    }

    // 命名调色副本：存一套、改参后按名取回，导出也能指定用这一套。
    let saved = ok_json(&server.call("aevum_grade_copy", json!({"op": "save", "name": "无头版"})));
    assert_eq!(saved["index"], 1);
    let _ = ok_json(&server.call("aevum_grade_set", json!({"params": {"exposure": -1.5}})));
    let applied =
        ok_json(&server.call("aevum_grade_copy", json!({"op": "apply", "name": "无头版"})));
    assert_eq!(applied["active_copy"], "无头版");
    assert_eq!(applied["params"]["exposure"], 0.5);
    let copy_dir = temp.path().join("stills-copy");
    let copied = ok_json(&server.call(
        "aevum_export_stills",
        json!({
            "out_dir": copy_dir.display().to_string(),
            "copy": "无头版",
            "confirm": true
        }),
    ));
    assert_eq!(copied["frames"], 3);
    assert!(copy_dir.join("frame_000000.png").is_file());
    let bad_copy = server.call(
        "aevum_export_stills",
        json!({
            "out_dir": temp.path().join("stills-nocopy").display().to_string(),
            "copy": "没有这版",
            "confirm": true
        }),
    );
    assert_eq!(bad_copy["isError"], json!(true));

    // 即使 confirm 了，也不许把输出写回序列根目录（会污染/清理源帧）。
    let into_root = server.call(
        "aevum_export_stills",
        json!({"out_dir": seq_dir.display().to_string(), "confirm": true}),
    );
    assert_eq!(into_root["isError"], json!(true));
    // 只数源帧：`.aevum` 里已经有工程与操作日志，不能当作被写进根目录。
    let source_frames = std::fs::read_dir(&seq_dir)
        .expect("源目录")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .count();
    assert_eq!(source_frames, 3);

    // HDR 合并：三张不同亮度的帧融成一张；没跑对齐时按原尺寸直接融。
    let hdr_refused = server.call("aevum_hdr_merge", json!({"frames": [0, 1, 2]}));
    assert_eq!(hdr_refused["isError"], json!(true));
    assert!(
        hdr_refused["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("confirm")),
        "{hdr_refused}"
    );
    let hdr_dir = temp.path().join("hdr");
    let merged = ok_json(&server.call(
        "aevum_hdr_merge",
        json!({
            "out_dir": hdr_dir.display().to_string(),
            "frames": [0, 1, 2],
            "format": "png8",
            "confirm": true
        }),
    ));
    assert_eq!(merged["frames"], 3);
    assert_eq!(merged["width"], 48);
    assert_eq!(merged["height"], 32);
    let hdr_path = merged["path"].as_str().expect("HDR 成片路径");
    assert!(
        std::path::Path::new(hdr_path).is_file(),
        "HDR 成片没写出来：{hdr_path}"
    );
    // 少于 2 帧、越界帧号、写回序列根都要拦下。
    let single = server.call("aevum_hdr_merge", json!({"frames": [0], "confirm": true}));
    assert_eq!(single["isError"], json!(true));
    let oob = server.call(
        "aevum_hdr_merge",
        json!({"frames": [0, 9], "confirm": true}),
    );
    assert_eq!(oob["isError"], json!(true));
    let hdr_root = server.call(
        "aevum_hdr_merge",
        json!({"frames": [0, 1], "out_dir": seq_dir.display().to_string(), "confirm": true}),
    );
    assert_eq!(hdr_root["isError"], json!(true));

    // XMP 往返：先写（不存在无需 confirm），再读回同一个曝光。
    let xmp_path = temp.path().join("look.xmp");
    let written = ok_json(&server.call(
        "aevum_xmp_write",
        json!({"path": xmp_path.display().to_string()}),
    ));
    // 套用副本会把预设名写成「副本 ×」：旁车如实记录当前这一套的来历。
    assert_eq!(written["preset"], "副本 无头版");
    let read_back = ok_json(&server.call(
        "aevum_xmp_read",
        json!({"path": xmp_path.display().to_string()}),
    ));
    assert_eq!(read_back["params"]["exposure"], 0.5);

    // .cube 往返：把当前参数烘成一张小 LUT，再当「外观」挂回来，最后摘下，
    // 免得后面按无外观的链路做的静帧/工程断言被这张表带偏。
    let cube_path = temp.path().join("look.cube");
    let cube_text = cube_path.display().to_string();
    let exported =
        ok_json(&server.call("aevum_cube_export", json!({"path": cube_text, "size": 17})));
    assert_eq!(exported["size"], json!(17));
    assert_eq!(exported["nodes"], json!(17 * 17 * 17));
    assert!(cube_path.is_file(), "导出的 .cube 没落盘");
    let refused = server.call("aevum_cube_export", json!({"path": cube_text, "size": 17}));
    assert_eq!(refused["isError"], json!(true));
    assert!(
        refused["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("confirm")),
        "重名导出应提示 confirm：{refused}"
    );
    let imported = ok_json(&server.call("aevum_cube_import", json!({"path": cube_text})));
    assert_eq!(imported["title"], json!("grade"));
    assert_eq!(
        imported["neutral"],
        json!(false),
        "曝光 0.5 的表不该是恒等表"
    );
    assert_eq!(imported["applied"], json!(true));
    let with_look = ok_json(&server.call("aevum_grade_get", json!({})));
    assert_eq!(
        with_look["params"]["look_cube"].as_str(),
        imported["path"].as_str(),
        "应用外观后参数里的路径要和导入返回的一致"
    );
    // apply:false 只解析不动参数。
    let peeked = ok_json(&server.call(
        "aevum_cube_import",
        json!({"path": cube_text, "apply": false}),
    ));
    assert_eq!(peeked["applied"], json!(false));
    let after_peek = ok_json(&server.call("aevum_grade_get", json!({})));
    assert_eq!(
        after_peek["params"]["look_cube"].as_str(),
        imported["path"].as_str(),
        "apply:false 不该改参数"
    );
    ok_json(&server.call("aevum_grade_set", json!({"params": {"look_cube": ""}})));

    // 工程落盘。
    let saved = ok_json(&server.call("aevum_project_save", json!({})));
    assert_eq!(saved["saved"], true);
    assert!(seq_dir.join(".aevum").join("project.json").is_file());
    let project = ok_json(&server.call("aevum_project_get", json!({})));
    assert_eq!(project["exists"], true);
    assert_eq!(project["project"]["params"]["exposure"], 0.5);

    // 关键帧：加、查、删；删除不存在的帧是业务错误而不是协议错误。
    let added = ok_json(&server.call(
        "aevum_keyframe_add",
        json!({"frame": 1, "params": {"exposure": 0.75}}),
    ));
    assert_eq!(added["count"], 1);
    assert_eq!(added["frames"], json!([1]));
    let listed = ok_json(&server.call("aevum_keyframe_list", json!({"frame": 1})));
    assert_eq!(listed["keyframes"][0]["frame"], 1);
    assert_eq!(listed["at"]["is_keyframe"], true);
    assert_eq!(listed["at"]["params"]["exposure"], 0.75);
    let removed = ok_json(&server.call("aevum_keyframe_remove", json!({"frame": 1})));
    assert_eq!(removed["count"], 0);
    let missing_kf = server.call("aevum_keyframe_remove", json!({"frame": 1}));
    assert_eq!(missing_kf["isError"], json!(true));

    // align：真实跑一遍模板匹配，报告存进会话。
    let align = ok_json(&server.call(
        "aevum_align",
        json!({"reference": 0, "estimator": "template"}),
    ));
    assert_eq!(align["frames"], 3);
    assert_eq!(align["reference"], 0);
    assert_eq!(align["stored"], true);
    assert!(align["crop"]["w"].as_u64().is_some_and(|w| w > 0));
    assert_eq!(align["series"].as_array().map(Vec::len), Some(3));
    assert!(align["quality"]["kept"].as_u64().is_some());

    // 状态：同步任务已结束，但上次报告摘要还在。
    let status = ok_json(&server.call("aevum_align_status", json!({})));
    assert_eq!(status["running"], false);
    assert_eq!(status["last_align"]["frames"], 3);
    assert_eq!(status["last_align"]["crop"], align["crop"]);

    // 操作日志：成功的写操作按时间顺序落进 <序列>/.aevum/op_journal.jsonl。
    let journal_file = seq_dir.join(".aevum").join("op_journal.jsonl");
    assert!(journal_file.is_file(), "缺少操作日志");
    let journal_path_text = journal_file.display().to_string();
    let journal = ok_json(&server.call("aevum_journal_list", json!({"limit": 500})));
    assert_eq!(journal["path"].as_str(), Some(journal_path_text.as_str()));
    let entries = journal["entries"].as_array().expect("日志数组");
    assert_eq!(journal["count"].as_u64(), Some(entries.len() as u64));
    let ops: Vec<&str> = entries
        .iter()
        .map(|e| e["op"].as_str().expect("op"))
        .collect();
    for expected in [
        "aevum_grade_set",
        "aevum_export_stills",
        "aevum_xmp_write",
        "aevum_cube_export",
        "aevum_cube_import",
        "aevum_project_save",
        "aevum_keyframe_add",
        "aevum_keyframe_remove",
        "aevum_align",
    ] {
        assert!(ops.contains(&expected), "日志缺少 {expected}：{ops:?}");
    }
    assert!(entries.iter().all(|e| e["source"] == "mcp"));
    let stamps: Vec<u64> = entries
        .iter()
        .map(|e| e["ts"].as_u64().expect("ts"))
        .collect();
    assert!(
        stamps.windows(2).all(|pair| pair[0] <= pair[1]),
        "{stamps:?}"
    );
    // limit 只取末尾几条，顺序不变。
    let tail = ok_json(&server.call("aevum_journal_list", json!({"limit": 3})));
    assert_eq!(tail["count"], json!(3));
    let tail_ops: Vec<&str> = tail["entries"]
        .as_array()
        .expect("末尾日志")
        .iter()
        .map(|e| e["op"].as_str().expect("op"))
        .collect();
    assert_eq!(tail_ops, ops[ops.len() - 3..]);
    // 只落成功的写操作：读接口和失败的工具都不进日志。
    let bad = server.call("aevum_keyframe_remove", json!({"frame": 9}));
    assert_eq!(bad["isError"], json!(true));
    let after = ok_json(&server.call("aevum_journal_list", json!({"limit": 500})));
    assert_eq!(after["count"], journal["count"]);

    // cancel 空闲时也成功，重复调用幂等。
    let cancel_one = ok_json(&server.call("aevum_cancel", json!({})));
    let cancel_two = ok_json(&server.call("aevum_cancel", json!({})));
    assert_eq!(cancel_one["cancelled"], false);
    assert_eq!(cancel_two["cancelled"], false);

    // 对齐参数错误走结构化中文错误。
    let bad_estimator = server.call("aevum_align", json!({"estimator": "nope"}));
    assert_eq!(bad_estimator["isError"], json!(true));
    assert!(
        bad_estimator["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("template") && t.contains("phase")),
        "{bad_estimator}"
    );
    let bad_reference = server.call("aevum_align", json!({"reference": 99}));
    assert_eq!(bad_reference["isError"], json!(true));

    // 未知方法走 JSON-RPC -32601，而不是工具错误。
    let unknown = server.request("resources/list", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32601));

    // 未知工具是 invalid params，且进程仍然活着。
    let unknown_tool = server.request("tools/call", json!({"name": "aevum_nope", "arguments": {}}));
    assert_eq!(unknown_tool["error"]["code"], json!(-32602));
    assert_eq!(server.request("ping", json!({}))["result"], json!({}));
}

#[test]
fn progress_notifications_follow_job_call() {
    let temp = TempDir::new("progress");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir, 3);
    let mut server = Server::start();

    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");
    ok_json(&server.call(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    ));

    // 带 progressToken 的长任务：响应之前必须先收到 notifications/progress。
    let id = server.next_id;
    server.next_id += 1;
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "aevum_measure",
            "arguments": {},
            "_meta": {"progressToken": "p-measure"}
        }
    }));

    let mut notifications = Vec::new();
    let response = loop {
        let line = server.recv();
        if line["method"] == "notifications/progress" {
            notifications.push(line);
            continue;
        }
        break line;
    };
    assert_eq!(response["id"], json!(id));
    assert_eq!(response["result"]["isError"], json!(false), "{response}");
    assert!(
        !notifications.is_empty(),
        "长任务没有发进度通知：{response}"
    );

    let mut last = -1i64;
    for note in &notifications {
        assert_eq!(note["params"]["progressToken"], json!("p-measure"));
        assert_eq!(note["params"]["total"], json!(100));
        assert!(
            note["params"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "{note}"
        );
        let progress = note["params"]["progress"].as_i64().expect("progress");
        assert!(progress >= last, "进度倒退：{notifications:?}");
        assert!((0..=100).contains(&progress), "{note}");
        last = progress;
    }
    assert_eq!(last, 100, "最后一条进度不是 100：{notifications:?}");

    // 非任务的工具带令牌也不发通知：下一条必须是响应本身。
    let id = server.next_id;
    server.next_id += 1;
    server.send(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "aevum_grade_get",
            "arguments": {},
            "_meta": {"progressToken": "p-grade"}
        }
    }));
    let response = server.recv();
    assert_eq!(
        response["id"],
        json!(id),
        "非任务工具不该发通知：{response}"
    );
    assert_eq!(response["result"]["isError"], json!(false));
}

#[test]
fn pano_merge_stitches_overlapping_frames() {
    let temp = TempDir::new("pano");
    let seq_dir = temp.path().join("frames");
    make_pano_sequence(&seq_dir, 3);
    let mut server = Server::start();
    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");
    ok_json(&server.call(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    ));

    // 没 confirm 要拦下。
    let refused = server.call("aevum_pano_merge", json!({"frames": [0, 1, 2]}));
    assert_eq!(refused["isError"], json!(true));
    assert!(
        refused["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("confirm")),
        "{refused}"
    );

    // 三张左移接片拼成一张更宽的：高度不变（纯平移），宽度接近 64+2×24。
    let pano_dir = temp.path().join("pano");
    let merged = ok_json(&server.call(
        "aevum_pano_merge",
        json!({
            "out_dir": pano_dir.display().to_string(),
            "frames": [0, 1, 2],
            "format": "png8",
            "confirm": true
        }),
    ));
    assert_eq!(merged["frames"], 3);
    assert_eq!(merged["height"], 48);
    let width = merged["width"].as_u64().expect("宽度");
    assert!(width > 64, "拼接后应更宽：{width}");
    assert!(width <= 64 + 48 + 8, "宽度不该发散：{width}");
    let path = merged["path"].as_str().expect("全景成片路径");
    assert!(std::path::Path::new(path).is_file(), "没写出来：{path}");

    // 少于 2 帧、越界、写回序列根都要拦下。
    let single = server.call("aevum_pano_merge", json!({"frames": [0], "confirm": true}));
    assert_eq!(single["isError"], json!(true));
    let oob = server.call(
        "aevum_pano_merge",
        json!({"frames": [0, 9], "confirm": true}),
    );
    assert_eq!(oob["isError"], json!(true));
    let into_root = server.call(
        "aevum_pano_merge",
        json!({"frames": [0, 1], "out_dir": seq_dir.display().to_string(), "confirm": true}),
    );
    assert_eq!(into_root["isError"], json!(true));
}

/// 焦点测试序列：三条 16 像素横带，第 k 条只有第 k 帧带纹理，其余带盒模糊。
///
/// Focus test sequence: three 16 px horizontal bands; only frame k carries texture in
/// band k, every other band is box-blurred.
fn make_focus_sequence(dir: &Path, frames: usize) {
    make_focus_sequence_sized(dir, frames, 48);
}

/// 同上一条，但边长可调：取消判据要靠「解码一张图需要几毫秒」来保证
/// `aevum_cancel` 必然赶在某次帧间检查之前到位，48×32 太快压不住竞态。
fn make_focus_sequence_sized(dir: &Path, frames: usize, side: usize) {
    std::fs::create_dir_all(dir).expect("建序列目录");
    // 竖条纹：周期 4px，`(x/2)%2` 在 200/55 之间交替。对比度要足够大，
    // 8 位量化与羽化都吃不出亏损。
    //
    // Vertical stripes, a 4 px period alternating 200/55: the contrast must be wide
    // enough that neither 8-bit quantisation nor feathering can eat it.
    fn stripes(x: usize) -> u8 {
        if (x / 2).is_multiple_of(2) { 200 } else { 55 }
    }
    // 沿 x 的 4 点平均：窗口宽度正好等于条纹周期，条纹因此被压成一条平线 127，
    // 当「失焦」用——它的对比度恰好归零，白送一个反向对照。
    //
    // Four-tap average along x: the window is exactly one stripe period wide, so the
    // stripes collapse into the constant 127, standing in for defocus. Its contrast is
    // exactly zero, which makes it a free negative control.
    fn blurred(x: usize) -> u8 {
        let mut acc = 0u32;
        for d in 0..4 {
            acc += u32::from(stripes((x + d) % 4));
        }
        (acc / 4) as u8
    }
    let band = side / frames;
    for k in 0..frames {
        let lo = k * band;
        let image = image::RgbImage::from_fn(side as u32, side as u32, |x, y| {
            let (x, y) = (x as usize, y as usize);
            let v = if y >= lo && y < lo + band {
                stripes(x)
            } else {
                blurred(x)
            };
            image::Rgb([v, v, v])
        });
        image
            .save(dir.join(format!("f{k:02}.png")))
            .expect("写焦点测试帧");
    }
}

/// 一块横带里「相邻两列灰度差的平均值」：每列都在变说明这段有纹理，糊了就归零。
///
/// Mean absolute horizontal neighbour difference over a band: it stays high while every
/// column changes (texture) and drops to zero once the band is defocused.
fn band_contrast(rgb: &[f32], w: usize, y0: usize, y1: usize) -> f32 {
    let (mut acc, mut cnt) = (0.0f32, 0u32);
    for y in y0..y1 {
        for x in 1..w {
            let a = rgb[(y * w + x) * 3];
            let b = rgb[(y * w + x - 1) * 3];
            acc += (a - b).abs();
            cnt += 1;
        }
    }
    if cnt == 0 { 0.0 } else { acc / cnt as f32 }
}

/// X-C2：`tools/list` 报出的 `focus_merge` schema 必须**恰三值**，且顺序与 `Method::ALL` 逐字一致。
///
/// 为什么连顺序一起钉：客户端（agent 尤其）常按枚举顺序给用户列选项。缺一个值意味着
/// 某个方法从界面上消失；多一个值意味着某个 key 一调就报错；顺序乱了会让「第 N 个方法」
/// 这类位置化选择在两端错位。这条判据不需要素材，只走 `tools/list`，所以单独一个测试，
/// 红了就知道是清单写歪了，而不是合成算法出了问题。
///
/// X-C2: the `focus_merge` schema reported by `tools/list` must have **exactly three**
/// method values, in the same order as `Method::ALL`.
///
/// Why pin the order too: clients (agents especially) often present the options in enum
/// order. A missing value hides a method, an extra one turns into a runtime error, and a
/// reordered list makes any position-based pick disagree across the two sides. This needs
/// no fixtures -- it only walks `tools/list` -- so it lives in its own test: red means the
/// catalogue is wrong, not that the fusing algorithm broke.
#[test]
fn the_focus_merge_schema_lists_exactly_three_methods_and_an_optional_align() {
    let mut server = Server::start();
    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");

    let listed = server.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools 数组");
    let focus_tool = tools
        .iter()
        .find(|tool| tool["name"] == "aevum_focus_merge")
        .expect("tools/list 里没有 aevum_focus_merge");
    let schema = &focus_tool["inputSchema"];

    let methods: Vec<&str> = schema["properties"]["method"]["enum"]
        .as_array()
        .expect("method 枚举")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let expected: Vec<&str> = aevum_core::focus::Method::ALL
        .iter()
        .map(|m| m.key())
        .collect();
    assert_eq!(
        methods, expected,
        "schema 的方法枚举与 Method::ALL 不一致：{methods:?}"
    );

    // `align` 必须是可选布尔且 schema 里写死缺省 false：客户端不该靠猜缺省值。
    //
    // `align` must be an optional boolean with `false` written into the schema: clients
    // should never have to guess a default.
    let align_prop = &schema["properties"]["align"];
    assert_eq!(
        align_prop["type"], "boolean",
        "align 必须是布尔：{align_prop}"
    );
    assert_eq!(
        align_prop["default"],
        json!(false),
        "align 的 schema 缺省必须是 false：{align_prop}"
    );
    // `align` 不能进 required：它是可选开关，进了 required 老客户端会当场被卡住。
    //
    // `align` must not be required: it is an optional switch, and requiring it would
    // break every client that was written before it existed.
    let required = schema["required"].as_array().expect("required 数组");
    assert!(
        !required.iter().any(|v| v.as_str() == Some("align")),
        "align 不该进 required：{required:?}"
    );
}

#[test]
fn focus_merge_recovers_each_band_from_the_frame_that_holds_its_texture() {
    let temp = TempDir::new("focus");
    let seq_dir = temp.path().join("frames");
    make_focus_sequence(&seq_dir, 3);
    let mut server = Server::start();
    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");

    // 清单里必须有它，否则 agent 连发现都发现不了。
    let listed = server.request("tools/list", json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools 数组")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"aevum_focus_merge"),
        "tools/list 里没有焦点合成：{names:?}"
    );

    ok_json(&server.call(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    ));

    let out_dir = temp.path().join("focus-out");
    // 没 confirm 要拦下，而且不能留下目录——无头批处理里空目录也是副作用。
    let refused = server.call(
        "aevum_focus_merge",
        json!({"frames": [0, 1, 2], "out_dir": out_dir.display().to_string()}),
    );
    assert_eq!(refused["isError"], json!(true), "{refused}");
    assert!(
        refused["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("confirm")),
        "{refused}"
    );
    assert!(!out_dir.exists(), "被拦下的调用不该建目录");

    // 未知融合方式要报错，不能悄悄落到缺省值上。
    // 坏值不能用 "pyramid"：1.5 起它是合法的金字塔方法名，
    // 用它会把这条「未知 key 必须报错」的语义验成值法。
    //
    // The bad value cannot be "pyramid": from 1.5 it is a legal pyramid-method key,
    // which would turn this "unknown key must error" assertion into a legal-value check.
    let unknown = server.call(
        "aevum_focus_merge",
        json!({
            "frames": [0, 1, 2],
            "method": "laplacian",
            "out_dir": out_dir.display().to_string(),
            "confirm": true
        }),
    );
    assert_eq!(unknown["isError"], json!(true), "{unknown}");
    assert!(
        unknown["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("未知融合方式")),
        "{unknown}"
    );

    // 正常融合：三条带各取到持有纹理的那帧，成片因此每条带都有对比度。
    let merged = ok_json(&server.call(
        "aevum_focus_merge",
        json!({
            "frames": [0, 1, 2],
            "out_dir": out_dir.display().to_string(),
            "format": "png8",
            "confirm": true
        }),
    ));
    assert_eq!(merged["kind"], "focus", "{merged}");
    assert_eq!(merged["frames"], 3);
    assert_eq!(merged["width"], 48);
    assert_eq!(merged["height"], 48);
    // 结果 JSON 不许夹带中文：这份 JSON 会原样送到 MCP 客户端，那边没有 i18n 漏斗，
    // 一个中文模板键就是一个英文用户看不懂的字段。对齐三态只走机器键。
    //
    // The result JSON must carry no Chinese: it goes to MCP clients as-is, and there is no
    // i18n funnel on that side -- one Chinese template key is one field an English user
    // cannot read. The alignment states travel as machine keys only.
    let text = merged.to_string();
    assert!(
        !text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "焦点合成结果 JSON 含中文：{text}"
    );
    // 结果里**不许**出现任何带「reason」的键。按子串扫而不是写死键名：写死只挡得住
    // 叫那个名字的字段，换个名字（`note`、`why`）就绕过去了。
    //
    // No key containing "reason" may appear in the result. Scanning for the substring rather
    // than one hard coded key stops any renamed comeback (`note`, `why`, ...) of a field
    // that was removed outright from slipping back in.
    let stray = merged
        .as_object()
        .expect("结果 JSON 是对象")
        .keys()
        .find(|k| k.contains("reason"))
        .cloned();
    assert!(
        stray.is_none(),
        "结果 JSON 不该有任何 reason 字段：{stray:?}"
    );
    // 状态键若存在，只能是 key() 那三个之一（或 off）；这里 headless 路径不跑自动对齐，
    // 因此不该出现任何非 off 的值——出现就说明有人把 align 结果接歪了。
    //
    // If a status key is present it must be one of the three `key()` values (or "off");
    // the headless path does not run auto alignment here, so anything but "off" would mean
    // the align result got wired in wrong.
    if let Some(status) = merged.get("align_status").and_then(|v| v.as_str()) {
        use aevum_core::focus_align::AlignStatus;
        let legal = [
            AlignStatus::Adopted,
            AlignStatus::RevertedInsufficientGain,
            AlignStatus::Failed,
        ]
        .iter()
        .any(|s| s.key() == status)
            || status == "off";
        assert!(legal, "align_status 不是 key() 产出的机器键：{status}");
        assert!(status.is_ascii(), "align_status 必须是 ASCII：{status}");
    }

    // 省略 `align` 必须与显式 `align: false` 逐点一致。
    //
    // 缺省值只有在与写出来的那个值等价时才叫缺省：不跑这一步，脚本可以不传、agent
    // 可以传 false，两条路各自演化后就会产出两张不同的成片，而没人会去比。
    //
    // Omitting `align` must be pixel-identical to `align: false`.
    //
    // A default is only a default if it equals the explicit value: without this step a
    // script may omit it while an agent passes `false`, the two paths drift apart, and
    // nothing ever compares the resulting images.
    let no_align_dir = temp.path().join("focus-out-noalign");
    let explicit_off = ok_json(&server.call(
        "aevum_focus_merge",
        json!({
            "frames": [0, 1, 2],
            "out_dir": no_align_dir.display().to_string(),
            "format": "png8",
            "align": false,
            "confirm": true
        }),
    ));
    assert_eq!(
        merged.get("align_status").and_then(|v| v.as_str()),
        Some("off"),
        "省略 align 时状态必须是 off：{merged}"
    );
    assert_eq!(
        explicit_off.get("align_status").and_then(|v| v.as_str()),
        Some("off"),
        "显式 align=false 时状态必须是 off：{explicit_off}"
    );

    let path = merged["path"].as_str().expect("成片路径");
    assert!(
        path.ends_with("focus_000000_3f.png"),
        "文件名要带首帧与帧数：{path}"
    );
    let (rgb, w, h) = aevum_core::export::load_rgb_f32(Path::new(path)).expect("读回成片");
    let w = w as usize;
    assert_eq!((w, h), (48, 48));

    // 接上一条：两张成片逐点比对。用 max|Δ| 而不是「差不多」，因为这条判据守的是
    // 「完全同一条代码路径」，任何非零差都说明缺省分支被单独走到了。
    //
    // Continuing from above: compare the two images pixel by pixel. `max|Δ|` rather than an
    // approximate match, because this guards "the very same code path" -- any non-zero
    // difference means the default branch was taken separately.
    let off_path = explicit_off["path"].as_str().expect("成片路径");
    let (rgb_off, ow, oh) =
        aevum_core::export::load_rgb_f32(Path::new(off_path)).expect("读回成片");
    assert_eq!((ow as usize, oh), (48, 48));
    assert_eq!(rgb.len(), rgb_off.len(), "两张成片尺寸必须一致");
    let max_diff = rgb
        .iter()
        .zip(rgb_off.iter())
        .fold(0.0f32, |acc, (a, b)| acc.max((a - b).abs()));
    assert_eq!(
        max_diff, 0.0,
        "省略 align 与 align=false 的成片必须逐点一致，实测 max|Δ| = {max_diff}"
    );
    // 每条带的中心 4 行：分界线两侧各留 radius + smooth_z + feather 的边距，
    // 边界上那一两行归谁在物理上就是模糊的（方法的属性，不是缺陷）。
    //
    // The middle four rows of each band: a margin of radius + smooth_z + feather is
    // left on each side of a seam, since ownership of the boundary rows is genuinely
    // ambiguous (a property of the method, not a defect).
    let fused = [
        band_contrast(&rgb, w, 6, 10),
        band_contrast(&rgb, w, 22, 26),
        band_contrast(&rgb, w, 38, 42),
    ];
    for (k, c) in fused.iter().enumerate() {
        assert!(*c > 0.2, "第 {k} 条带该取到持有纹理的那帧，实测对比度 {c}");
    }
    // 反向对照：第 0 帧只在第 0 条带清晰，第 1、2 条带是糊的，对比度必须接近零。
    // 少了这一段，上面那三行断言就只是在测「图里有东西」，而不是测「逐带取对了帧」。
    //
    // Negative control: frame 0 is only sharp in band 0; bands 1 and 2 are defocused and
    // must read near zero. Without this, the three assertions above would only prove
    // "something got written", not "every band picked its own frame".
    let (f0, fw, _fh) =
        aevum_core::export::load_rgb_f32(&seq_dir.join("f00.png")).expect("读回帧 0");
    for (k, (y0, y1)) in [(22usize, 26usize), (38, 42)].into_iter().enumerate() {
        let base = band_contrast(&f0, fw as usize, y0, y1);
        assert!(
            base < 0.02,
            "第 {} 条带在帧 0 里应当是糊的，实测 {base}",
            k + 1
        );
    }

    // 少于 2 帧、写回序列根都要拦下。
    let single = server.call(
        "aevum_focus_merge",
        json!({"frames": [0], "out_dir": out_dir.display().to_string(), "confirm": true}),
    );
    assert_eq!(single["isError"], json!(true), "{single}");
    let into_root = server.call(
        "aevum_focus_merge",
        json!({"frames": [0, 1], "out_dir": seq_dir.display().to_string(), "confirm": true}),
    );
    assert_eq!(into_root["isError"], json!(true), "{into_root}");
}

/// 20 KB 的路径不许原样回显：错误串是给人（和 agent）读的，不是把输入搬回来的通道。
///
/// A 20 KB path must not be echoed verbatim: an error string is there to be read, not to
/// carry the input back.
#[test]
fn a_huge_sequence_path_is_clipped_in_the_error_text() {
    let mut server = Server::start();
    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");
    // 20 KB 的不存在路径：`is_dir()` 当场否掉，不会真去碰盘，所以这条判据在任何
    // 机器上都跑得出来。
    //
    // A 20 KB path that does not exist: `is_dir()` rejects it without touching the disk,
    // so this runs anywhere.
    let long = format!("Z:/{}/x", "a".repeat(20_000));
    let text = error_text(
        &server.call("aevum_open_sequence", json!({"root": long})),
        "不是文件夹",
    );
    assert_bounded(&text, true);
}

/// 同一类回显扫一遍：路径进错误串一律截断，不管它从哪个工具漏出来。
///
/// 为什么要有第二处：`display()` 进错误串的写法原先散在十来个地方，一处一处补就是
/// 打地鼠。这里走两条真实的 stdio 分支，钉住「新工具照抄老写法」会立刻红。
///
/// One sweep for the whole family: a path entering an error string is always clipped, no
/// matter which tool leaks it.
///
/// Why a second site: the `display()`-into-error-string shape used to sit in a dozen
/// places, and patching them one at a time is whack-a-mole. This walks two real stdio
/// branches so that a new tool copying an old shape turns red immediately.
#[test]
fn every_tool_clips_the_path_it_echoes_in_an_error() {
    let temp = TempDir::new("echo");
    let seq_dir = temp.path().join("frames");
    make_sequence(&seq_dir, 3);
    let mut server = Server::start();
    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");
    ok_json(&server.call(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    ));

    // ① 「音频文件不存在」：20 000 字的不存在路径，`is_file()` 当场否掉，不碰盘。
    //    这条能同时钉住「截断」与「留省略号」。
    //
    // "Audio file not found": a 20 000-character path that does not exist, rejected by
    // `is_file()` without touching the disk. This one pins both the cut and the ellipsis.
    let out_dir = temp.path().join("out");
    let long = format!("Z:/{}/a.wav", "a".repeat(20_000));
    let text = error_text(
        &server.call(
            "aevum_export_video",
            json!({
                "out_dir": out_dir.display().to_string(),
                "video_out": out_dir.join("v.mp4").display().to_string(),
                "audio": long
            }),
        ),
        "音频文件不存在",
    );
    assert_bounded(&text, true);

    // ② 「拒绝覆盖源图」：拿序列里真实的一帧当输出口。这条路径是短的（`is_source`
    //    要求与真实帧逐段相等，20 KB 的文件名在盘上根本存不下），所以它钉的是
    //    「同一个函数没把短文案改坏」——不该凭空多出省略号。
    //
    // "Refusing to overwrite a source frame": point the output at a real frame. This path
    // is short (the check compares against real frame paths, and a 20 KB file name cannot
    // exist on disk), so it pins the other direction: the shared helper must not add an
    // ellipsis to a short path.
    let frame0 = seq_dir.join("f00.png");
    let text = error_text(
        &server.call(
            "aevum_render_preview",
            json!({"out": frame0.display().to_string()}),
        ),
        "拒绝覆盖源图",
    );
    assert_bounded(&text, false);
}

/// 同一类回显的另一半：agent 传进来的**字符串**（不是路径）也不许原样回传。
///
/// 为什么不是十五处各来一条：`echo_text` 是个漏斗，逐点写判据只会让用例越长越没人看。
/// 这里挑两条真经 stdio 走到的分支——20 000 字的未知调色字段名（`tools.rs` 那一族）与
/// 20 000 字的未知工具名（`server.rs`）——任何一处把 `echo_text` 摘掉，文案当场涨回两万字。
///
/// The other half of the same family: a **string** an agent sent in (not a path) must not
/// come back verbatim either.
///
/// Why not one case per site: `echo_text` is a funnel, and fifteen near-identical
/// assertions would only make this file longer. Two branches that really travel over stdio
/// — a 20 000-character unknown grade field name (the `tools.rs` family) and a 20 000
/// character unknown tool name (`server.rs`) — turn the message back into 20 KB the moment
/// either clip is taken out.
#[test]
fn every_name_the_agent_sends_is_clipped_in_the_error_text() {
    let mut server = Server::start();
    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");

    // ① 未知调色字段：20 000 字的键名。`merge_params` 只拿它做字符串比较，不碰盘，
    //    所以这条在任何机器上都跑得出来。它钉的是工具返回里的 `isError` 文案。
    //
    // Unknown grade field: a 20 000-character key. `merge_params` only compares it as a
    // string and never touches the disk, so this runs anywhere. It pins the `isError` text
    // a tool hands back.
    let long_key = format!("k{}", "a".repeat(20_000));
    let mut partial = serde_json::Map::new();
    partial.insert(long_key, json!(1));
    let text = error_text(
        &server.call("aevum_grade_set", json!({"params": Value::Object(partial)})),
        "未知调色字段",
    );
    assert_bounded(&text, true);

    // ② 未知工具名：走 JSON-RPC 的 -32602，文案在 `error.message` 里而不是 `content`。
    //    这条钉的是协议层自己的报错——它连工具都没进，agent 更容易撞上。
    //
    // Unknown tool name: the JSON-RPC -32602 path, whose text lives in `error.message`
    // rather than `content`. This pins the protocol layer's own message, which an agent
    // hits before any tool runs.
    let long_tool = format!("aevum_{}", "b".repeat(20_000));
    let bad = server.request("tools/call", json!({"name": long_tool, "arguments": {}}));
    assert_eq!(
        bad["error"]["code"],
        json!(-32602),
        "错误码该是 -32602：{}",
        head(&bad.to_string())
    );
    let text = bad["error"]["message"]
        .as_str()
        .expect("错误消息不是字符串")
        .to_owned();
    assert!(
        text.contains("未知工具"),
        "该报的是「未知工具」：{}",
        head(&text)
    );
    assert_bounded(&text, true);
}

/// blocks 端到端（台账开放项收口）：`align: true` 时调用侧必须把 core 估出的
/// **逐块**位移场原样交给 warp。
///
/// 夹具的位移必须带全局分量：采纳决策（`gain_stats`）用**全局代表位移**算收益，
/// 纯块内局部位移的全局增益为 0、按设计就会回退。所以 f1 = 全局平移 6px + 上半幅
/// 再多 4px：全局分量让对齐被采纳，块内偏差让「全局平移」与「逐块场」的成片可分。
/// 正确的逐块场把上半幅挪 10px、下半幅 6px，warp 后 f1 ≡ f0，融合成片 ≈ f0；
/// 丢掉块场（不平移）或退回全局平移，都会留下某个半幅的残余错位，成片与 f0 的
/// 逐像素差当场涨上去——红。条纹周期 16px：所有位移都落在同一个相关峰上，无歧义。
///
/// End-to-end for the block field (closing the ledger open item): with `align: true`
/// the caller must hand core's **per-block** shift field to the warp unchanged.
///
/// The fixture's shift must carry a global component: the adoption decision
/// (`gain_stats`) is computed with the **global representative shift**, so a purely
/// block-local shift has zero global gain and reverts by design. f1 therefore equals a
/// global 6 px shift plus 4 px extra on the top half: the global part gets the
/// alignment adopted, the block-local part is what separates "global translation" from
/// "per-block field" in the output. A correct per-block field moves the top blocks 10 px
/// and the bottom 6 px, so the warped f1 equals f0 and the fusion tracks f0; dropping
/// the field (identity) or falling back to a global translation leaves residual
/// misalignment in one half and the pixel difference against f0 jumps -- red. Stripe
/// period 16 px keeps every shift on the same correlation peak, unambiguously.
#[test]
fn focus_merge_alignment_warps_by_the_estimated_block_field_not_a_global_translation() {
    let temp = TempDir::new("focus-blocks");
    let seq_dir = temp.path().join("frames");
    // 确定性白噪声：hash 取高位字节。宽频谱让相位相关只有一个峰，错位的两帧
    // 加权平均直接摊成平场、测度归零——增益统计对这种纹理最敏感。
    //
    // Deterministic white noise: high byte of an integer hash. The broadband spectrum
    // gives phase correlation a single peak, and averaging misaligned copies flattens
    // the measure to zero -- the gain statistic is most sensitive on this texture.
    let noise = |x: u32, y: u32| -> u8 {
        let h = x.wrapping_mul(73_856_093) ^ y.wrapping_mul(83_492_791);
        (h >> 24) as u8
    };
    std::fs::create_dir_all(&seq_dir).expect("建序列目录");
    // f0：原始噪声。f1：全局平移 6px，上半幅再多 2px（合计 8px）。
    for (name, top_shift, bottom_shift) in [("f00.png", 0u32, 0u32), ("f01.png", 8u32, 6u32)] {
        let img = image::RgbImage::from_fn(512, 384, |x, y| {
            // 位移场在 1/2 luma 代理上估：512×384 源图 → 256×192 代理 → 块
            // 32×32 代理像素；上半幅 192 行 = 3 个整块行，48 个块全部有效。
            //
            // The shift field is estimated on the 1/2-luma proxy: a 512x384 source gives
            // a 256x192 proxy and 32x32-px blocks; the top 192 rows are three whole block
            // rows and all 48 blocks are valid.
            let v = if y < 192 {
                noise(x + top_shift, y)
            } else {
                noise(x + bottom_shift, y)
            };
            image::Rgb([v, v, v])
        });
        img.save(seq_dir.join(name)).expect("写帧");
    }
    let mut server = Server::start();
    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");
    ok_json(&server.call(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    ));

    let out_dir = temp.path().join("out");
    let merged = ok_json(&server.call(
        "aevum_focus_merge",
        json!({
            "frames": [0, 1],
            // 必须用 weighted：depthmap 是逐像素硬选帧，两帧都锐时选谁测度都满格，
            // 对相位错乱不敏感，增益恒为 0 → 永远回退。加权平均才会把错位当成
            // 「互相抵消」降测度，对齐的收益才显现。
            //
            // Must be weighted: depthmap hard-picks a frame per pixel, and when both
            // frames are sharp its measure is full either way -- it is insensitive to
            // phase scrambling, so the gain stays 0 and it always reverts. Weighted
            // averaging treats misalignment as mutual cancellation, which is what makes
            // the alignment gain visible.
            "method": "weighted",
            "out_dir": out_dir.display().to_string(),
            "format": "png8",
            "align": true,
            "confirm": true
        }),
    ));
    // 对齐必须真被采纳：夹具把半幅的相位反转了，对齐收益远高于 GAIN_MIN。
    //
    // The alignment must actually be adopted: inverting half the frame's phase gives a
    // gain far above GAIN_MIN.
    assert_eq!(
        merged.get("align_status").and_then(|v| v.as_str()),
        Some("adopted"),
        "{merged}"
    );
    let gain = merged
        .get("align_gain")
        .and_then(|v| v.as_f64())
        .expect("align_gain");
    assert!(gain > 0.0, "采纳时增益必须为正：{merged}");

    let path = merged["path"].as_str().expect("成片路径");
    let (rgb, w, h) = aevum_core::export::load_rgb_f32(Path::new(path)).expect("读回成片");
    let (w, h) = (w as usize, h as usize);
    assert_eq!(
        (w, h),
        (
            merged["width"].as_u64().expect("width") as usize,
            merged["height"].as_u64().expect("height") as usize
        )
    );
    let (f0, fw, fh) = aevum_core::export::load_rgb_f32(&seq_dir.join("f00.png")).expect("读 f0");
    assert_eq!((fw, fh), (512, 384));

    // 公共窗口的落点由位移决定（上半幅 −4px），不写死：在 0..=8 里找让**两半幅同时**
    // 最贴 f0 的横向偏移，再用这个偏移做逐像素比较。全局平移或丢块场的变异没有
    // 任何一个偏移能让两半幅同时贴上 f0——这就是红与绿的分界。
    //
    // The common window's offset follows the shifts (-4 px on the top half) and is not
    // hard coded: scan 0..=8 for the horizontal offset that brings **both halves** closest
    // to f0 at once, then compare pixel-wise under that offset. Neither the dropped-field
    // nor the global-translation mutation has any single offset that aligns both halves
    // simultaneously -- that is the green/red line.
    let mean_abs_diff = |ox: usize| -> f32 {
        // 避开裁切边与块接缝羽化带：行取 8..184 与 200..376，列取 8..w-8。
        let mut acc = 0.0f64;
        let mut cnt = 0u64;
        for y in 0..h {
            if !((8..184).contains(&y) || (200..376).contains(&y)) {
                continue;
            }
            for x in 8..w.saturating_sub(8) {
                let o = (y * w + x) * 3;
                let r = (y * 512 + x + ox) * 3;
                for c in 0..3 {
                    acc += f64::from((rgb[o + c] - f0[r + c]).abs());
                    cnt += 1;
                }
            }
        }
        (acc / cnt as f64) as f32
    };
    let (best_ox, best) = (0..=16usize)
        .map(|ox| (ox, mean_abs_diff(ox)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("非空");
    assert!(
        best < 0.05,
        "成片两半幅必须同时贴回 f0（best_ox={best_ox}, mean|Δ|={best}）：块场被丢或退回全局平移都会红"
    );
}

/// 合并期间 cancel 必须能被受理：`background: true` 把合成起成后台任务，
/// stdio 主循环空闲下来，`aevum_cancel` 在长任务运行中就能置位旗标；解码
/// 循环在帧与帧之间读到旗标就停，报「已取消」，不落成片。
///
/// 确定性来源：1200×1200 的 PNG 解码一张要几毫秒，而 cancel 请求在 started
/// 回包之后微秒级就能到位——取消检查点必然被取消先赶上有余。
///
/// Cancellation must land mid-merge: `background: true` turns the merge into a
/// background job, so the stdio main loop is free to serve `aevum_cancel` while
/// it runs; the decode loop checks the flag between frames, reports 「已取消」
/// and writes no output.
///
/// Why this is deterministic: decoding one 1200x1200 PNG takes milliseconds while
/// the cancel request lands microseconds after the started reply -- the flag is
/// set well before any late checkpoint could matter.
#[test]
fn focus_merge_background_answers_cancel_mid_merge() {
    let temp = TempDir::new("focus-cancel");
    let seq_dir = temp.path().join("frames");
    make_focus_sequence_sized(&seq_dir, 4, 1200);
    let mut server = Server::start();
    server.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    server.notify("notifications/initialized");
    ok_json(&server.call(
        "aevum_open_sequence",
        json!({"root": seq_dir.display().to_string()}),
    ));

    let out_dir = temp.path().join("focus-cancel-out");
    let started = ok_json(&server.call(
        "aevum_focus_merge",
        json!({
            "frames": [0, 1, 2, 3],
            "out_dir": out_dir.display().to_string(),
            "background": true,
            "confirm": true
        }),
    ));
    assert_eq!(started["background"], json!(true), "{started}");

    // 任务运行中请求取消：必须受理（cancelled=true），且登记的是 export 类任务。
    let cancelled = ok_json(&server.call("aevum_cancel", json!({})));
    assert_eq!(cancelled["cancelled"], json!(true), "{cancelled}");
    assert_eq!(cancelled["job"], json!("export"), "{cancelled}");

    // 轮询到任务停下，然后它必须报「已取消」而不是把成片写完。
    let mut status = ok_json(&server.call("aevum_align_status", json!({})));
    for _ in 0..400 {
        if status["running"] == json!(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
        status = ok_json(&server.call("aevum_align_status", json!({})));
    }
    assert_eq!(status["running"], json!(false), "{status}");
    let error = status["last_error"].as_str().unwrap_or_default();
    assert!(error.contains("已取消"), "该报「已取消」：{status}");
    let leftover = std::fs::read_dir(&out_dir)
        .expect("输出目录应已建出")
        .count();
    assert_eq!(leftover, 0, "取消后不该落成片：{leftover} 个文件");
}
