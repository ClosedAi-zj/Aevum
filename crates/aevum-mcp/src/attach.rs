//! 附着模式：把 MCP 工具代理到已经打开的 GUI。
//!
//! `aevum.exe --mcp --attach <control.json>` 只读 GUI 写下的
//! `<root>/.aevum/control.json`，stdin/stdout 上照常完成 MCP 握手；`tools/list`
//! 与 `tools/call` 转成控制通道的 JSON-RPC（本机 HTTP `POST /rpc`）。这样
//! 同一个 exe 既能无头跑（`--mcp`），也能驱动正在运行的界面，改动立刻上屏。
//!
//! 协议握手（initialize / ping / notifications）全部本地处理：即使 GUI 暂时没开，
//! 客户端也能完成握手，真正调用工具时才连回环，并拿到中文错误。

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use crate::server::{LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};

const MAX_RESPONSE: usize = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

/// 控制文件里真正需要的字段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlTarget {
    pub port: u16,
    pub token: String,
    pub root: String,
    pub version: String,
}

/// 兜底工具表：GUI 没开 / 读不到 `system.commands` 时也能回答 `tools/list`。
///
/// 与 `aevum-ui` 的 `control::command_catalog` 是同一批命令 ID；描述在这里
/// 压缩成一行，GUI 在线时以 GUI 返回的说明为准。
const FALLBACK_TOOLS: &[(&str, &str)] = &[
    (
        "sequence.info",
        "读取当前序列概览：根目录、帧数、分辨率、缺帧、去闪与对齐摘要。无参数。",
    ),
    (
        "sequence.open",
        "打开帧文件夹作为当前序列。参数：{\"root\": \"路径\"}。",
    ),
    (
        "sequence.check",
        "序列体检（后台）：挑出坏帧/黑帧/虚焦/重复/亮度跳变/编号缺口。参数可选 {\"sig\": 3}，0.5~20，越小越灵敏。立刻返回已启动，用 sequence.status 查结果。",
    ),
    (
        "sequence.status",
        "读取体检进度与最近一次清单：busy/done/total、分类计数、逐条发现（帧号 1 基）。无参数。",
    ),
    (
        "sequence.cancel",
        "请求取消正在跑的体检，在一批文件之间生效。无参数。",
    ),
    (
        "frame.select",
        "选中某一帧（1 开始）。参数：{\"index\": 1}。",
    ),
    (
        "ui.module",
        "切换模块。参数：{\"name\": \"序列|去闪|显影|构图|成片|声音\" 或 0-5}。",
    ),
    (
        "ui.dock",
        "切换并展开右侧面板。参数：{\"name\": \"调整|信息|关键帧|AI\" 或 0-3}。",
    ),
    ("grade.get", "读取当前调色参数与预设名。无参数。"),
    (
        "grade.auto",
        "自动校正：按 1/50/99 百分位给出白平衡与整套影调，写绝对覆盖值；同一帧重复调用结果不变。无参数。",
    ),
    (
        "grade.set",
        "按字段合并调色参数，例如 {\"exposure\":0.5,\"contrast\":12}；未知字段报错。",
    ),
    (
        "grade.preset",
        "套用内置预设。参数：{\"name\": \"原始|鲜艳风光|落日暖调|冷色电影…\"}。",
    ),
    ("grade.reset", "恢复默认调色参数。无参数。"),
    (
        "grade.copy",
        "命名调色副本（同一序列多套调色并存）：op=list/save/apply/delete/rename（省=list），name=副本名，save 同名需 overwrite:true，rename 用 to；导出/队列可按副本名指定用哪一套。",
    ),
    (
        "xmp.import",
        "读入 Lightroom / Camera Raw 的 .xmp 预设并替换当前调色参数。参数：{\"path\": \"a.xmp\"}。",
    ),
    (
        "xmp.export",
        "把当前调色参数写成 .xmp 预设。参数：{\"path\": \"a.xmp\", \"confirm\": false}。",
    ),
    (
        "cube.import",
        "把外部 .cube 三维 LUT 挂成「外观」（主链之前先套一层查表）。参数：{\"path\": \"a.cube\", \"apply\": true}。",
    ),
    (
        "cube.export",
        "把当前调色参数导出成 .cube（只含逐点运算）。参数：{\"path\": \"a.cube\", \"size\": 65, \"confirm\": false}。",
    ),
    (
        "audio.config",
        "读取或修改配乐设置。参数：path / delay / gain / fade_in / fade_out / loop / trim / acodec；无参数即查询。",
    ),
    (
        "deflicker.set",
        "读取或修改去闪烁设置。参数：window / strength / multi / channels；无参数即查询。",
    ),
    (
        "crop.set",
        "读取或修改取景（裁剪框 + 拉直 + 画幅比例）。参数：x / y / width / height（源像素，四个一起给）、rotate（度，-45..45，正角=画面逆时针）、aspect（原生|16:9|4:3|1:1|9:16）、reset；无参数即查询。",
    ),
    (
        "lens.set",
        "读取或开关镜头配置文件校正（EXIF 匹配 lensfun 数据库折出的畸变/横向色差/暗角标定，先于手动三项生效）。参数：enabled（开关）、rematch（true=按当前 EXIF 重配）；无参数即查询。",
    ),
    (
        "profile.set",
        "读取或套用 Adobe 相机/镜头配置。参数：kind（dcp=相机配置，仅 RAW/DNG；lcp=镜头配置）、path（指定文件）、auto（按 EXIF 自动匹配）、clear（清除）；无 path/auto/clear 即查询。",
    ),
    (
        "library.add",
        "把一个素材目录加进图库清单（跨会话常驻）并编目。参数：path（素材目录）。",
    ),
    (
        "library.remove",
        "从图库清单移除一个目录，不动磁盘上的文件。参数：index（1 开始）。",
    ),
    (
        "library.list",
        "读图库：无参数列根目录清单；带 index 编目该目录并回逐文件评级/旗标/色标。",
    ),
    (
        "library.open",
        "把图库里的一个目录开成序列。参数：index（1 开始）。",
    ),
    (
        "library.organize",
        "在图库里改一张或一批照片的评级/旗标/色标，写回所在目录的工程文件（批量只写一次盘）。参数：frame（单帧）或 frames（帧号数组），rating（0-5）、flag（-1/0/1）、label（0-4）、root（目录序号，可省）。",
    ),
    (
        "library.keyword",
        "给图库照片加/去关键词，写回所在目录的工程文件（批量只写一次盘）。参数：add / remove（字符串按逗号分号拆，或字符串数组），frame 或 frames，root（可省）。",
    ),
    (
        "library.metadata",
        "读/写图库里某个目录的说明与版权信息，原子写进该目录工程文件（没打开、甚至空的目录也行）。参数：root（可省）、title、description、creator、copyright（空串或 null 清空）。不带参数只查询。",
    ),
    (
        "library.stack",
        "图库堆叠：把同目录的几张照片并成一叠、拆散一叠，或按文件名前缀自动分叠（只改编目折叠，不搬文件）。参数：op（list/create/dissolve/auto，可省=list）、root（可省）、frames（create 用，帧号数组，至少两个）、stack（dissolve 用，叠号 1 开始）。",
    ),
    (
        "collection.create",
        "新建跨目录手动集合并返回序号；同名复用。参数：name。",
    ),
    (
        "collection.add",
        "把图库照片加入集合。参数：index（集合序号）或 name、frame、root（可省）。",
    ),
    (
        "collection.remove",
        "把照片从集合里拿掉。参数：index 或 name，加 path 或 frame+root。",
    ),
    (
        "collection.delete",
        "删掉集合本身，不动照片。参数：index 或 name。",
    ),
    (
        "collection.list",
        "列出手动集合；带 index 或 name 回该集合的成员路径。",
    ),
    (
        "render.set",
        "读取或修改成片输出设置。参数：mode（stills|video）、format（png8|tiff16|jpeg|webp）、quality（1-100，只对 jpeg）、keep_exif（搬源 JPEG 的 EXIF）、xmp_sidecar（每张静帧旁写同名 .xmp：dc: 说明版权四项 + crs: 这一张的调色）、resume（断点续跑：已写完的帧原样留着，只补缺的）、sharpen_for（off|screen|glossy|matte）、sharpen_strength（low|standard|high）、color_space、watermark（对象：enabled/kind/text/image/spot/opacity/size/margin/color，只写要改的键）、width+height、out_dir；无参数即查询。",
    ),
    (
        "retime.set",
        "读取或修改时间重映射（变速 + 运动模糊）。参数：enabled（总开关，关着导出走 1:1 原路）、speed（全程倍速，如 2 压帧 / 0.5 拆帧）、keys（速度曲线数组 [{at,speed}]，at 是源进度 0-1 不是输出秒）、ease（缓入缓出对象 {peak 必填, edge=1, in_at=0.25, out_at=0.75}）、shutter（曝光区间占比 0-1：0 锐利插帧，1 把跨过的源帧按重叠时长全平均）、reset:true 清回默认；speed/keys/ease 三选一，无参数即查询。",
    ),
    (
        "retime.status",
        "当前变速设定 + 按序列帧数换算的输出帧数、时长；倍速填错（0、负数、超上限）在这里报，不用等导出。无参数。",
    ),
    (
        "motion.set",
        "读取或修改运镜（Ken Burns）：取景框从起点走到终点。参数：enabled（总开关，关着或两个框重合时导出不重采样）、from/to（取景框，[x,y,w,h] 或 {x,y,w,h}，相对拉直+裁切后那一幅归一化 0-1，越界由内核夹回画面内）、ease（smooth 缓入缓出|linear 匀速|in 越推越快|out 越推越慢）、swap:true 起止互换、refit:[宽,高] 换画幅比例时把两个框一起重铺、reset:true 清回默认；无参数即查询。",
    ),
    (
        "motion.status",
        "当前运镜设定：两个框、缓动档、是否真的在动，外加作用的那一幅尺寸、输出画幅与两端的推近倍数（>1 说明已经在拿少数像素撑大画面）。无参数。",
    ),
    (
        "mask.list",
        "列出局部蒙版：形状、组件数、强度与局部参数。无参数。",
    ),
    (
        "mask.add",
        "新增一张局部蒙版并选中。参数：{\"kind\": \"linear|radial|brush\"}，缺省 linear。",
    ),
    (
        "mask.remove",
        "删除指定序号的局部蒙版（只改工程参数，不动素材）。参数：{\"index\": 1}。",
    ),
    (
        "mask.update",
        "改蒙版强度、反选、局部参数、组内形状几何、亮度或颜色范围（颜色按角度：hue/hue_width/hue_feather/hue_sat）；画笔形状用 geometry.strokes 整组替换笔画（每条 {points:[[x,y]…], radius?, hardness?, flow?, erase?}，坐标 0~1 归一化）。参数：index + component? / amount? / params? / geometry? / range?。",
    ),
    (
        "mask.component",
        "在一张蒙版组里加或删一个形状（减去/交集的布尔选区）。参数：index + action? / kind?（linear|radial|brush） / component?。",
    ),
    (
        "align.run",
        "启动序列对齐（长任务，立即返回）。参数：{\"reference\": 1, \"estimator\": \"template|phase\"}。",
    ),
    ("align.status", "查询对齐进度与报告摘要。无参数。"),
    (
        "export.video",
        "导出并编码视频（长任务，立即返回）。参数：fps / crf / codec / color_space / copy?（调色副本名）/ width+height / out_dir / confirm: true。",
    ),
    (
        "export.stills",
        "导出静帧序列（长任务，立即返回）。参数：format / quality / color_space / copy?（调色副本名）/ width+height / out_dir / confirm: true。",
    ),
    ("export.status", "查询导出进度与最近输出。无参数。"),
    (
        "export.cancel",
        "请求取消导出；已写下的素材不删除。无参数。",
    ),
    (
        "hdr.merge",
        "把同场景多张不同曝光融成一张静帧（Mertens 曝光融合，批量写，必须 confirm: true；长任务，立即返回，进度用 merge.status）。参数：frames?（帧下标数组，0 开始）或 center?+around?（默认当前帧±1，around 1..4）/ out_dir?（默认序列根下 HDR/）/ confirm: true。",
    ),
    (
        "pano.merge",
        "把相邻几张有重叠的照片拼成一张宽幅全景（特征点匹配 + RANSAC 单应 + 羽化融合，批量写，必须 confirm: true；长任务，立即返回，进度用 merge.status）。参数：frames?（帧下标数组，0 开始，按接片顺序）或 start?+around?（默认当前帧起向后 1 张，around 1..11）/ out_dir?（默认序列根下 全景/）/ confirm: true。",
    ),
    (
        "merge.status",
        "查询 HDR / 全景合并的进度与最近一次成片（busy / kind / done / total / result / error）。无参数。",
    ),
    (
        "merge.cancel",
        "请求取消正在进行的 HDR / 全景合并；在两帧之间生效。无参数。",
    ),
    ("project.save", "写入 <序列>/.aevum/project.json。无参数。"),
    ("project.get", "读取工程快照与路径。无参数。"),
    (
        "project.metadata",
        "读/写当前序列的说明与版权信息，改动立刻落盘并随导出进 EXIF/容器标签。参数：title、description、creator、copyright（都可省；空串或 null 清空）。不带参数只查询。",
    ),
    (
        "queue.list",
        "列出批量渲染队列（与 GUI 共用 render_queue.json）。无参数。",
    ),
    (
        "queue.add",
        "把序列加入渲染队列。参数：root? / out? / width+height? / fps? / codec? / crf? / preset? / keep_frames? / resume?（断点续跑，只补缺的帧） / copy?（该段用哪套调色副本）。",
    ),
    ("queue.remove", "从队列移除一段。参数：{\"index\": 1}。"),
    ("queue.clear", "清空队列，不动已渲染的成片。无参数。"),
    (
        "queue.run",
        "逐段渲染队列（长任务，立即返回）。参数：{\"confirm\": true}。",
    ),
    ("queue.status", "查询队列进度与每段结果。无参数。"),
    ("queue.cancel", "请求取消队列；已完成的段保留结果。无参数。"),
    (
        "ui.screenshot",
        "窗口截图到 PNG（异步）。参数：{\"path\": \"D:/shots/a.png\", \"confirm\": false}。",
    ),
    (
        "journal.list",
        "读取当前序列的操作日志末尾。参数：{\"limit\": 50}。",
    ),
    (
        "history.list",
        "列撤销历史：每步的说明、是否可撤销、当前停在哪一步。无参数。",
    ),
    (
        "history.undo",
        "撤销最近一步；回到那之前的状态（视图缩放与选帧不算步）。无参数。",
    ),
    (
        "history.redo",
        "重做一步（撤销之后往回放）。没有可重做的改动时报错。无参数。",
    ),
    (
        "history.jump",
        "跳到历史里的任意一步。参数：{\"index\": 3}（从 1 开始）。",
    ),
];

/// 从入口参数里找 `--attach <path>` / `--attach=<path>`。
///
/// 只有 `--mcp` 存在时调用方才会进来；单独 `--mcp` 返回 `Ok(None)`，
/// 继续走无头模式。
pub fn attach_path(argv: &[OsString]) -> Result<Option<PathBuf>, String> {
    let mut found: Option<PathBuf> = None;
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy();
        if text == "--attach" {
            let Some(value) = iter.next() else {
                return Err("--attach 需要一个 control.json 路径".to_owned());
            };
            let path = value.to_string_lossy().trim().to_owned();
            if path.is_empty() {
                return Err("--attach 的路径为空".to_owned());
            }
            found = Some(PathBuf::from(path));
        } else if let Some(rest) = text.strip_prefix("--attach=") {
            let path = rest.trim();
            if path.is_empty() {
                return Err("--attach 的路径为空".to_owned());
            }
            found = Some(PathBuf::from(path));
        }
    }
    Ok(found)
}

/// 读 control.json。错误文案要能让用户知道下一步该做什么。
pub fn load_target(path: &Path) -> Result<ControlTarget, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "读取控制文件失败（{}）：{e}\n先启动 aevum-studio 并打开一个序列，再用 AI 面板里显示的 control.json 路径。",
            path.display()
        )
    })?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("控制文件不是合法 JSON（{}）：{e}", path.display()))?;
    let port = value
        .get("port")
        .and_then(Value::as_u64)
        .filter(|port| (1..=65535).contains(port))
        .ok_or_else(|| "控制文件缺少有效的 port（1-65535）".to_owned())? as u16;
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "控制文件缺少 token".to_owned())?
        .to_owned();
    Ok(ControlTarget {
        port,
        token,
        root: value
            .get("root")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        version: value
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// 附着的 stdio MCP 入口。
pub fn run_stdio_attach(path: PathBuf) -> ExitCode {
    let target = match load_target(&path) {
        Ok(target) => target,
        Err(e) => {
            eprintln!("aevum-mcp: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "aevum-mcp: 附着模式 · 控制端口 127.0.0.1:{} · 控制文件 {}",
        target.port,
        path.display()
    );
    let stdin = io::stdin();
    let stdout = io::stdout();
    if let Err(e) = serve(stdin.lock(), stdout.lock(), &target) {
        eprintln!("aevum-mcp: stdio 结束：{e}");
    }
    ExitCode::SUCCESS
}

/// 逐行读 MCP 请求、逐行写响应；抽出来便于脱离进程做协议测试。
pub fn serve<R: BufRead, W: Write>(
    mut input: R,
    mut output: W,
    target: &ControlTarget,
) -> io::Result<()> {
    let mut line = String::new();
    let mut warned = false;
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            break;
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(text) {
            Ok(value) => handle(value, target, &mut warned),
            Err(e) => Some(error_response(
                Value::Null,
                -32700,
                &format!("JSON 解析失败：{e}"),
            )),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

fn handle(message: Value, target: &ControlTarget, warned: &mut bool) -> Option<Value> {
    let id = message.get("id").cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return id.map(|id| error_response(id, -32600, "缺少 method"));
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        // 通知没有 id，不需要响应。
        return None;
    };
    let response = match method {
        "initialize" => ok_response(id, initialize(&params, target)),
        "ping" => ok_response(id, json!({})),
        "tools/list" => ok_response(id, json!({"tools": tool_list(target, warned)})),
        "tools/call" => call_tool(id, &params, target),
        _ => error_response(id, -32601, &format!("未知方法：{method}")),
    };
    Some(response)
}

fn initialize(params: &Value, target: &ControlTarget) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = match requested {
        Some(version) if SUPPORTED_PROTOCOL_VERSIONS.contains(&version) => version,
        _ => LATEST_PROTOCOL_VERSION,
    };
    let where_ = if target.root.is_empty() {
        String::new()
    } else {
        format!("（序列 {}）", target.root)
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {
            "name": "aevum-studio",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": format!(
            "附着模式：工具代理到正在运行的 Aevum Studio GUI{where_}，界面会实时显示修改。批量写与覆盖已有输出需要参数 confirm: true。"
        )
    })
}

fn tool_list(target: &ControlTarget, warned: &mut bool) -> Vec<Value> {
    match rpc(target, "system.commands", &json!({})) {
        Ok(result) => {
            let commands = result
                .get("commands")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let tools: Vec<Value> = commands
                .iter()
                .filter_map(|command| {
                    let id = command.get("id").and_then(Value::as_str)?;
                    let summary = command
                        .get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or("Aevum Studio 命令");
                    let params = command
                        .get("params")
                        .and_then(Value::as_str)
                        .unwrap_or("见 inputSchema");
                    let schema = command
                        .get("schema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"}));
                    Some(json!({
                        "name": mcp_name(id),
                        "description": format!("{summary}（参数：{params}）"),
                        "inputSchema": schema,
                    }))
                })
                .collect();
            if !tools.is_empty() {
                return tools;
            }
            fallback_tools()
        }
        Err(e) => {
            if !*warned {
                *warned = true;
                eprintln!("aevum-mcp: 读不到 GUI 命令清单，先用内置兜底列表：{e}");
            }
            fallback_tools()
        }
    }
}

fn fallback_tools() -> Vec<Value> {
    FALLBACK_TOOLS
        .iter()
        .map(|(id, description)| {
            json!({
                "name": mcp_name(id),
                "description": *description,
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": description,
                },
            })
        })
        .collect()
}

fn call_tool(id: Value, params: &Value, target: &ControlTarget) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error_response(id, -32602, "tools/call 缺少 name");
    };
    let Some(command) = resolve_command(name) else {
        return error_response(
            id,
            -32602,
            &format!("未知工具：{name}（附着模式只暴露 GUI 命令，用 tools/list 查看）"),
        );
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return error_response(id, -32602, "arguments 必须是对象");
    }
    match rpc(target, command, &arguments) {
        Ok(value) => {
            let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned());
            ok_response(
                id,
                json!({
                    "content": [{"type": "text", "text": text}],
                    "isError": false
                }),
            )
        }
        // 业务失败不是协议错误：客户端继续握着手，agent 能读到原因。
        Err(message) => ok_response(
            id,
            json!({
                "content": [{"type": "text", "text": message}],
                "isError": true
            }),
        ),
    }
}

/// MCP 工具名：`sequence.info` → `aevum_sequence_info`。
///
/// 严格客户端只接受 `[A-Za-z0-9_-]`，所以工具名不能带点；命令 ID 仍保留点号。
pub fn mcp_name(command: &str) -> String {
    format!("aevum_{}", command.replace('.', "_"))
}

/// 工具名 → 命令 ID。兼容旧名字（`lumen_*`）与词序不一致的两个命令。
///
/// 先做一次命名空间规范化：`lumen_open_sequence` → `aevum_open_sequence`，
/// 于是**旧名和新名走同一条查表路径**，行为逐字节相同。
pub fn resolve_command(name: &str) -> Option<&'static str> {
    let name = aevum_core::brand::canonical_tool(name);
    let name = name.as_ref();
    const ALIASES: &[(&str, &str)] = &[
        // 只有「工具名与 mcp_name(命令 ID) 对不上」的才需要显式条目。
        // `align.run` 的工具名是 `aevum_align`（去掉了 `.run`），但
        // `mcp_name("align.run")` = `aevum_align_run`，所以它必须显式列出来。
        ("aevum_align", "align.run"),
        ("aevum_open_sequence", "sequence.open"),
        ("aevum_sequence_info", "sequence.info"),
    ];
    if let Some((_, command)) = ALIASES.iter().find(|(alias, _)| *alias == name) {
        return Some(command);
    }
    FALLBACK_TOOLS
        .iter()
        .map(|(id, _)| *id)
        .find(|id| mcp_name(id) == name || *id == name)
}

// ---------------------------------------------------------------- 控制通道客户端

fn rpc(target: &ControlTarget, method: &str, params: &Value) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let response = post_rpc(target.port, &target.token, &body)?;
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("控制通道返回未知错误");
        return Err(message.to_owned());
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn post_rpc(port: u16, token: &str, body: &Value) -> Result<Value, String> {
    let payload = serde_json::to_vec(body).map_err(|e| format!("序列化请求失败：{e}"))?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| {
        format!(
            "GUI 未运行或控制端口 127.0.0.1:{port} 不可达：{e}\n先启动 aevum-studio 并打开序列，再用最新的 control.json。"
        )
    })?;
    let _ = stream.set_read_timeout(Some(RESPONSE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    write!(
        stream,
        "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    )
    .map_err(|e| format!("写控制通道失败：{e}"))?;
    stream
        .write_all(&payload)
        .map_err(|e| format!("写控制通道失败：{e}"))?;
    stream.flush().map_err(|e| format!("写控制通道失败：{e}"))?;

    let mut reader = BufReader::new(stream);
    let (status, text) = read_http_response(&mut reader)?;
    if status == 401 {
        return Err(
            "令牌无效或已过期：GUI 每次启动都会换新令牌，请用最新的 .aevum/control.json".to_owned(),
        );
    }
    if status != 200 {
        return Err(format!(
            "控制通道返回 HTTP {status}：{}",
            extract_message(&text)
        ));
    }
    serde_json::from_str(&text).map_err(|e| format!("控制通道响应不是 JSON：{e}"))
}

/// 读一条 HTTP/1.1 响应（含状态行、头、按 Content-Length 或 EOF 的正文）。
pub fn read_http_response<R: BufRead>(reader: &mut R) -> Result<(u16, String), String> {
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| format!("读控制通道响应失败：{e}"))?;
    if status_line.is_empty() {
        return Err("控制通道没有返回响应（连接被关闭）".to_owned());
    }
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("控制通道状态行不认识：{}", status_line.trim()))?;

    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("读控制通道响应头失败：{e}"))?;
        if n == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse::<usize>().ok();
        }
    }

    let mut body = String::new();
    match content_length {
        Some(len) if len <= MAX_RESPONSE => {
            let mut buf = vec![0u8; len];
            reader
                .read_exact(&mut buf)
                .map_err(|e| format!("读控制通道响应体失败：{e}"))?;
            body = String::from_utf8_lossy(&buf).into_owned();
        }
        Some(len) => return Err(format!("控制通道响应过大：{len} 字节")),
        None => {
            reader
                .take(MAX_RESPONSE as u64)
                .read_to_string(&mut body)
                .map_err(|e| format!("读控制通道响应体失败：{e}"))?;
        }
    }
    Ok((status, body))
}

fn extract_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn target() -> ControlTarget {
        ControlTarget {
            port: 1,
            token: "t".to_owned(),
            root: String::new(),
            version: String::new(),
        }
    }

    #[test]
    fn attach_path_accepts_both_forms() {
        let argv: Vec<OsString> = ["--mcp", "--attach", "C:/x/control.json"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(
            attach_path(&argv).unwrap(),
            Some(PathBuf::from("C:/x/control.json"))
        );

        let argv: Vec<OsString> = ["--mcp", "--attach=C:/y/control.json"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(
            attach_path(&argv).unwrap(),
            Some(PathBuf::from("C:/y/control.json"))
        );

        let argv: Vec<OsString> = ["--mcp"].iter().map(OsString::from).collect();
        assert_eq!(attach_path(&argv).unwrap(), None);
    }

    #[test]
    fn attach_path_reports_missing_value() {
        let argv: Vec<OsString> = ["--mcp", "--attach"].iter().map(OsString::from).collect();
        let err = attach_path(&argv).unwrap_err();
        assert!(err.contains("--attach"), "{err}");

        let argv: Vec<OsString> = ["--mcp", "--attach="].iter().map(OsString::from).collect();
        assert!(attach_path(&argv).is_err());
    }

    #[test]
    fn tool_names_roundtrip() {
        assert_eq!(mcp_name("sequence.info"), "aevum_sequence_info");
        assert_eq!(
            resolve_command("aevum_sequence_info"),
            Some("sequence.info")
        );
        assert_eq!(resolve_command("aevum_grade_set"), Some("grade.set"));
        assert_eq!(resolve_command("grade.set"), Some("grade.set"));
        assert_eq!(resolve_command("aevum_align"), Some("align.run"));
        assert_eq!(resolve_command("aevum_export_video"), Some("export.video"));
        assert_eq!(resolve_command("aevum_nope"), None);
    }

    /// C14：附着模式下用**旧工具名**（改名前的 `lumen_*`）也要能解析到同一个命令。
    ///
    /// 附着模式走的是另一条入口（不经 `server::call_tool`），别名层漏在这里的话，
    /// 老客户端点一个 `lumen_open_sequence` 会得到「未知工具」，而它明明在 list 里过过。
    #[test]
    fn c14_legacy_tool_names_resolve_in_attach_mode() {
        // 词序不一致的两个：必须靠显式 ALIASES 命中
        assert_eq!(
            resolve_command("lumen_open_sequence"),
            Some("sequence.open")
        );
        assert_eq!(
            resolve_command("lumen_sequence_info"),
            Some("sequence.info")
        );
        // 词序一致的：靠 FALLBACK 循环里 mcp_name() 命中
        assert_eq!(resolve_command("lumen_align"), Some("align.run"));
        assert_eq!(resolve_command("lumen_grade_set"), Some("grade.set"));
        assert_eq!(resolve_command("lumen_export_video"), Some("export.video"));
        // 旧名与新名必须落到**同一个**命令上
        for (old, new) in [
            ("lumen_open_sequence", "aevum_open_sequence"),
            ("lumen_grade_set", "aevum_grade_set"),
            ("lumen_align", "aevum_align"),
        ] {
            assert_eq!(
                resolve_command(old),
                resolve_command(new),
                "{old} 与 {new} 必须解析到同一个命令"
            );
        }
        // 旧名拼错了也不能瞎命中
        assert_eq!(resolve_command("lumen_nope"), None);
    }

    #[test]
    fn initialize_echoes_known_version() {
        let result = initialize(&json!({"protocolVersion": "2025-06-18"}), &target());
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "aevum-studio");
        assert!(result["instructions"].as_str().unwrap().contains("附着"));
    }

    #[test]
    fn http_response_parser_reads_length_body() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}";
        let mut cursor = Cursor::new(raw.as_bytes().to_vec());
        let (status, body) = read_http_response(&mut cursor).expect("解析响应");
        assert_eq!(status, 200);
        assert_eq!(body, "{\"ok\":true}");
    }

    #[test]
    fn http_response_parser_reads_until_eof_without_length() {
        let raw = "HTTP/1.1 401 Unauthorized\r\n\r\nnope";
        let mut cursor = Cursor::new(raw.as_bytes().to_vec());
        let (status, body) = read_http_response(&mut cursor).expect("解析响应");
        assert_eq!(status, 401);
        assert_eq!(body, "nope");
    }

    #[test]
    fn http_response_parser_rejects_garbage() {
        let mut cursor = Cursor::new(b"HTTP/1.1\r\n\r\n".to_vec());
        assert!(read_http_response(&mut cursor).is_err());
    }

    #[test]
    fn load_target_validates_fields() {
        let dir = std::env::temp_dir().join(format!(
            "aevum-attach-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control.json");

        let err = load_target(&path).unwrap_err();
        assert!(err.contains("读取控制文件失败"), "{err}");

        std::fs::write(&path, "{}").unwrap();
        let err = load_target(&path).unwrap_err();
        assert!(err.contains("port"), "{err}");

        std::fs::write(&path, r#"{"port": 5000, "token": "abc", "root": "D:/x"}"#).unwrap();
        let target = load_target(&path).expect("读取控制文件");
        assert_eq!(target.port, 5000);
        assert_eq!(target.token, "abc");
        assert_eq!(target.root, "D:/x");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serve_handles_handshake_without_gui() {
        // 端口 1 不会有人监听：tools/list 应回落到内置清单而不是断线。
        let input = Cursor::new(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\
              {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n\
              {\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"aevum_grade_get\",\"arguments\":{}}}\n"
                .to_vec(),
        );
        let mut output = Vec::new();
        serve(input, &mut output, &target()).expect("stdio 结束");
        let lines: Vec<&str> = std::str::from_utf8(&output).unwrap().lines().collect();
        assert_eq!(lines.len(), 3, "{:?}", lines);
        let init: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(init["id"], json!(1));
        let list: Value = serde_json::from_str(lines[1]).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert!(
            tools
                .iter()
                .any(|tool| tool["name"] == json!("aevum_grade_get"))
        );
        // 撤销/历史是 GUI 状态的一部分，兜底清单里也要有，agent 才找得到。
        for name in [
            "aevum_history_list",
            "aevum_history_undo",
            "aevum_history_jump",
        ] {
            assert!(
                tools.iter().any(|tool| tool["name"] == json!(name)),
                "兜底清单缺 {name}"
            );
        }
        let call: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(call["result"]["isError"], json!(true));
        let text = call["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("不可达"), "{text}");
    }

    #[test]
    fn tools_call_is_forwarded_to_the_control_port() {
        use std::net::TcpListener;

        // 起一个假 GUI：只收一条 POST /rpc，验证请求体与令牌，再回一条结果。
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("绑定假控制端口");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("接受控制连接");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            assert!(
                request_line.starts_with("POST /rpc HTTP/1.1"),
                "{request_line}"
            );

            let mut length = 0usize;
            let mut auth = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let line = line.trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    match name.trim().to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "authorization" => auth = value.trim().to_owned(),
                        _ => {}
                    }
                }
            }
            assert_eq!(auth, "Bearer t");
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).expect("请求是 JSON");
            assert_eq!(request["method"], json!("grade.set"));
            assert_eq!(request["params"]["exposure"], json!(0.5));

            let body = r#"{"jsonrpc":"2.0","id":1,"result":{"preset":"自定义"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            stream.flush().unwrap();
        });

        let target = ControlTarget {
            port,
            token: "t".to_owned(),
            root: String::new(),
            version: String::new(),
        };
        let input = Cursor::new(
            b"{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"aevum_grade_set\",\"arguments\":{\"exposure\":0.5}}}\n"
                .to_vec(),
        );
        let mut output = Vec::new();
        serve(input, &mut output, &target).expect("stdio 结束");
        server.join().expect("假 GUI 线程");

        let response: Value = serde_json::from_slice(&output).expect("响应是 JSON");
        assert_eq!(response["id"], json!(5));
        assert_eq!(response["result"]["isError"], json!(false));
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("自定义"), "{text}");
    }
}
