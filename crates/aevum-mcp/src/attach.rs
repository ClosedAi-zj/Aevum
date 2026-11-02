//! 附着模式：把 MCP 工具代理到已经打开的 GUI。
//!
//! `aevum.exe --mcp --attach <control.json>` 只读 GUI 写下的
//! `<root>/.aevum/control.json`，stdin/stdout 上照常完成 MCP 握手；`tools/list`
//! 与 `tools/call` 转成控制通道的 JSON-RPC（本机 HTTP `POST /rpc`）。这样
//! 同一个 exe 既能无头跑（`--mcp`），也能驱动正在运行的界面，改动立刻上屏。
//!
//! 协议握手（initialize / ping / notifications）全部本地处理：即使 GUI 暂时没开，
//! 客户端也能完成握手，真正调用工具时才连回环，并拿到中文错误。
//!
//! Attach mode: proxy MCP tools to a GUI that is already open.
//!
//! `aevum.exe --mcp --attach <control.json>` only reads the `<root>/.aevum/control.json`
//! the GUI wrote, and completes the MCP handshake over stdin/stdout as usual;
//! `tools/list` and `tools/call` become JSON-RPC on the control channel (a local
//! HTTP `POST /rpc`). One executable therefore runs both headless (`--mcp`) and as
//! a driver for a live interface, where every change shows up on screen at once.
//!
//! The protocol handshake (initialize / ping / notifications) is handled entirely
//! locally: even with the GUI closed, a client can finish the handshake, and only a
//! real tool call reaches the loopback and gets an error message.

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
///
/// The only fields actually needed from the control file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlTarget {
    pub port: u16,
    pub token: String,
    pub root: String,
    pub version: String,
}

/// GUI 离线时的**描述表**：只提供一行中文说明，工具清单本体另有出处。
///
/// 清单本体是 [`aevum_core::mcp_routes::COMMANDS`]（命令目录的 ID 副本）。
/// 这里写到的命令用这段说明，没写到的用 [`GUI_OFFLINE_DESCRIPTION`] 兜底。
/// 拆开的好处是「清单完整」与「文案齐全」互不牵制：补一条新命令时，
/// 忘了写描述最坏也就是说明笼统一句，不会让命令本身缺席。
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
        "读取或修改取景（裁剪框 + 拉直 + 画幅比例）。参数：x / y / width / height（源像素，四个一起给）、rotate（度，-45..45，正角=画面逆时针）、aspect（原生|16:9|3:2|4:3|5:4|1:1|4:5|9:16|2:3）、reset；无参数即查询。",
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
        "cull.evaluate",
        "按四条判据逐帧裁决并给出理由（曝光越界 / 相邻突变 / 清晰度低于中位比例 / 读不出来），不吃图、立即返回。参数：suggest?（用序列 MAD 推曝光区间）/ ev_low? / ev_high? / jump_ev? / sharp_ratio? / apply?（true 则把判废的帧标成排除）。",
    ),
    (
        "cull.extract",
        "把没被拒收的帧复制成一条新序列（剔除成子序列）：路径安全先行（不能是源内部、盘根或非空目录），保留 mtime、测光缓存一并带走；开发参数不跟过去。参数：target（目标目录绝对路径）/ dry_run?（只报帧数与体积）/ confirm: true（非 dry_run 必须）。",
    ),
    (
        "focus.merge",
        "把一组不同对焦距离的帧融成一张全景深（焦点合成，批量写，必须 confirm: true；长任务，立即返回，进度用 merge.status）。深度图法：逐像素取测度最强的帧，测度低于阈值的无细节区整块用参考帧兜底，过渡带羽化；加权平均法：测度四次方当权重。参数：frames?（帧下标数组，0 开始）或 center?+around?（默认当前帧±7，around 1..31）/ method?（depthmap / weighted）/ radius?（测度半径 0..32，默认 2）/ threshold?（无细节阈值百分位 0..100，0 = 关）/ smooth_z?（深度图去斑 0..16）/ feather?（羽化半径 0..64 px）/ out_dir?（默认序列根下 焦点合成/）/ confirm: true。",
    ),
    (
        "stack.merge",
        "把同机位的一段帧按统计量合成一张（星轨 lighten / 降噪 average / 去卫星 median / σ 截尾 sigma-clip，批量写，必须 confirm: true；长任务，立即返回，进度用 merge.status）。参数：frames?（帧下标数组，0 开始）或 center?+around?（默认当前帧±4，around 1..32）/ mode? / clip_sigma? / fill_gaps? / out_dir?（默认序列根下 堆栈/）/ confirm: true。",
    ),
    (
        "timeblend.merge",
        "把不同时刻的帧合成为一张（sky-ground 天取末帧地取首帧，bands 横向分带一带一帧；批量写，必须 confirm: true；长任务，立即返回，进度用 merge.status）。参数：frames?（帧下标数组，0 开始，升序=时间顺序）或 start?+around?（默认当前帧起 8 帧）/ layout?（sky-ground|bands）/ bands? / feather? / horizon? / horizon_auto?（自动推地平线）/ deghost?（去鬼影阈值 EV，0 = 关，默认 0.5）/ match_exposure? / out_dir?（默认序列根下 时间合成/）/ confirm: true。",
    ),
    (
        "merge.status",
        "查询多帧合并（HDR / 全景 / 星空堆栈 / 时间合成）的进度与最近一次成片（busy / kind / done / total / result / error）。无参数。",
    ),
    (
        "merge.cancel",
        "请求取消正在进行的多帧合并（HDR / 全景 / 星空堆栈 / 时间合成）；在两帧之间生效。无参数。",
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
///
/// Find `--attach <path>` / `--attach=<path>` among the entry arguments.
///
/// The caller only gets here when `--mcp` is present; a bare `--mcp` returns
/// `Ok(None)` and headless mode continues.
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
///
/// Read control.json. The error message has to tell the user what to do next.
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
///
/// The stdio MCP entry point for attach mode.
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
///
/// Read MCP requests line by line and write responses line by line; extracted so
/// the protocol can be tested without a process.
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
            // GUI 比命令行新时，命令目录里会多出本机不认识的新 ID。这类命令
            // **列出来也调不通**（本地解析不到 → 报「未知工具」），agent 只会拿着
            // 一条看着合法的工具名反复试错。直接不列，并提示一次，让人去对齐版本。
            let mut unlisted: Vec<&str> = Vec::new();
            let tools: Vec<Value> = commands
                .iter()
                .filter_map(|command| {
                    let id = command.get("id").and_then(Value::as_str)?;
                    if !aevum_core::mcp_routes::COMMANDS.contains(&id) {
                        unlisted.push(id);
                        return None;
                    }
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
            if !unlisted.is_empty() && !*warned {
                *warned = true;
                // 带版本号：两边都报得出自己是什么版本时，人一眼就知道差了哪一版。
                let gui = if target.version.is_empty() {
                    String::new()
                } else {
                    format!(" {}", target.version)
                };
                eprintln!(
                    "aevum-mcp: GUI{gui} 报了 {} 条本机命令行不认识的新命令，已从 tools/list 里跳过：{}；\
                     两边版本可能不一致，建议一起升级",
                    unlisted.len(),
                    unlisted.join("、")
                );
            }
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

/// GUI 离线时的工具清单：由命令目录派生，与在线时同为 77 条。
///
/// 清单本体不再单独维护。从前这里直接遍历 [`FALLBACK_TOOLS`]，于是「描述表」与
/// 「清单」是同一份东西：加一条命令时必须记得同时补一条描述，否则它就从离线清单里
/// 消失，而且看不出来是漏了。现在清单来自 [`aevum_core::mcp_routes::COMMANDS`]，
/// 描述表只负责文案，漏写一条只会让说明笼统些。
fn fallback_tools() -> Vec<Value> {
    aevum_core::mcp_routes::COMMANDS
        .iter()
        .map(|id| {
            let description = FALLBACK_TOOLS
                .iter()
                .find(|(command, _)| command == id)
                .map(|(_, text)| *text)
                .unwrap_or(GUI_OFFLINE_DESCRIPTION);
            json!({
                "name": mcp_name(id),
                "description": description,
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": true,
                    "description": description,
                },
            })
        })
        .collect()
}

/// 描述表没覆盖到的命令共用这一句。
///
/// 眼下 77 条命令的描述都在 [`FALLBACK_TOOLS`] 里，所以这段文案还走不到。
/// 留着是因为清单来自命令目录、描述靠人工补：将来新增命令时，
/// 中间那段「清单已有、文案未写」的空窗不该让 agent 读到空字符串。
const GUI_OFFLINE_DESCRIPTION: &str =
    "（GUI 离线，暂无详细说明；启动 GUI 后重新拉取 tools/list 可看到参数说明）";

fn call_tool(id: Value, params: &Value, target: &ControlTarget) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error_response(id, -32602, "tools/call 缺少 name");
    };
    let Some(command) = resolve_command(name) else {
        return error_response(id, -32602, &unknown_tool_message(name));
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
/// 实现只有一份，在 `aevum_core::mcp_routes`——本函数只是本地别名。
///
/// MCP tool name: `sequence.info` → `aevum_sequence_info`.
///
/// Strict clients accept `[A-Za-z0-9_-]` only, so tool names cannot carry dots;
/// command IDs keep theirs. There is one implementation, in
/// `aevum_core::mcp_routes` — this function is just a local alias.
pub fn mcp_name(command: &str) -> String {
    aevum_core::mcp_routes::mcp_name(command)
}

/// 工具名 → 命令 ID。
///
/// 两条查表路径合起来覆盖两个命名空间：
/// 1. 服务模式的 34 个工具名（`aevum_xmp_read` 这种与命令 ID 对不齐的历史拼法也在此）；
/// 2. GUI 命令目录的 77 个名字（`aevum_library_add`、`aevum_history_undo` 等）。
///
/// 旧前缀 `lumen_*` 先规范化，于是新旧名走**同一条**路，行为逐字节相同。
///
/// 返回 `None` 的语义是「这个名字既不是工具、也不是 GUI 命令」；
/// 「工具存在但 GUI 没这项能力」也是 `None`，调用方用 [`is_known_tool`] 区分。
///
/// Tool name → command ID.
///
/// Two lookup paths together cover two namespaces:
/// 1. the 34 tool names of server mode (`aevum_xmp_read` and other historical
///    spellings that do not line up with a command ID are in here too);
/// 2. the 77 names in the GUI command catalogue (`aevum_library_add`,
///    `aevum_history_undo` and so on).
///
/// The old `lumen_*` prefix is canonicalised first, so old and new names take
/// **the same** path and behave byte-identically.
///
/// `None` means "this name is neither a tool nor a GUI command"; "the tool exists
/// but the GUI has no such capability" is also `None`, and callers tell them apart
/// with [`is_known_tool`].
pub fn resolve_command(name: &str) -> Option<&'static str> {
    let canonical = aevum_core::brand::canonical_tool(name);
    let name = canonical.as_ref();
    if let Some(command) = aevum_core::mcp_routes::tool_command(name) {
        return Some(command);
    }
    // 直接给命令 ID 也认：控制通道的原生客户端就是这么传的，
    // 早先的 `resolve_command("grade.set")` 一直返回 `Some("grade.set")`。
    if let Some(command) = aevum_core::mcp_routes::COMMANDS
        .iter()
        .copied()
        .find(|id| *id == name)
    {
        return Some(command);
    }
    aevum_core::mcp_routes::command_for_mcp_name(name)
}

/// 这个名字是不是已知的服务模式工具（不区分附着模式能不能跑）。
///
/// Whether this is a known server-mode tool (regardless of whether attach mode can
/// run it).
pub fn is_known_tool(name: &str) -> bool {
    let canonical = aevum_core::brand::canonical_tool(name);
    aevum_core::mcp_routes::is_known_tool(canonical.as_ref())
}

/// 解析失败时的提示语：把「名字拼错」和「GUI 没这项能力」分开说。
///
/// 以前两种情况共用一句「未知工具」。可 `aevum_measure` 这类名字在服务模式的
/// `tools/list` 里是实打实存在的，用户把同一条客户端配置切到附着模式之后照旧调用，
/// 得到「未知工具」，于是一直在检查自己的拼写。
fn unknown_tool_message(name: &str) -> String {
    if is_known_tool(name) {
        format!(
            "{name} 只在服务模式可用：GUI 控制通道没有对应命令，--attach 附着跑不了它。\
             改用 aevum --mcp 直连，或 tools/list 查看附着模式支持的 77 条命令"
        )
    } else {
        format!("未知工具：{name}（附着模式只暴露 GUI 命令，用 tools/list 查看）")
    }
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
///
/// Read one HTTP/1.1 response: status line, headers, then a body delimited by
/// Content-Length or by EOF.
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

    /// 附着模式的工具清单是从 GUI 命令目录现生成的（77 条），所以**每一个**名字
    /// 都必须能路由回去。少一条就是「列在 tools/list 里、一调就报未知工具」。
    ///
    /// 这是守卫而非回归：修复前那条兜底表恰好也是 77 条完整覆盖，所以本测试在旧代码上
    /// 同样会绿。它防的是将来——清单与路由表分家之后，谁也别想靠"我记得补了"过关。
    #[test]
    fn every_catalog_command_is_routable_in_attach_mode() {
        for id in aevum_core::mcp_routes::COMMANDS {
            let name = mcp_name(id);
            assert_eq!(resolve_command(&name), Some(*id), "{name} 路由不到 {id}");
            // 控制通道的原生客户端直接传命令 ID，也得认
            assert_eq!(resolve_command(id), Some(*id), "{id} 本身应当可路由");
        }
    }

    /// 服务模式的 34 个工具名一个都不能漏。历史上漏的正是 `xmp_read/write`
    /// （命令叫 import/export）与 `align`（命令叫 align.run）。
    #[test]
    fn every_server_tool_name_is_recognized() {
        for route in aevum_core::mcp_routes::TOOL_ROUTES {
            assert!(is_known_tool(route.tool), "{} 没被认出来", route.tool);
            match route.command {
                Some(command) => assert_eq!(
                    resolve_command(route.tool),
                    Some(command),
                    "{} 应当路由到 {command}",
                    route.tool
                ),
                None => assert_eq!(
                    resolve_command(route.tool),
                    None,
                    "{} 在 GUI 侧没有对应命令，不该有路由",
                    route.tool
                ),
            }
        }
    }

    /// 离线清单必须和在线清单一样长（77 条），只允许描述详细度不同。
    /// 同一个客户端换台机器（GUI 开没开）不该看到两套工具集。
    #[test]
    fn offline_list_covers_the_whole_catalog() {
        let tools = fallback_tools();
        let names: Vec<String> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(names.len(), aevum_core::mcp_routes::COMMANDS.len());
        for id in aevum_core::mcp_routes::COMMANDS {
            let name = mcp_name(id);
            assert!(names.contains(&name), "离线清单缺 {name}");
        }
        // 描述不能空着：空字符串会让 agent 以为这个工具没有说明
        for tool in &tools {
            assert!(!tool["description"].as_str().unwrap_or("").is_empty());
        }
    }

    /// 描述补充表里**不能有**不存在的命令：写错了不会报错，只会让那段文案
    /// 永远匹配不上、静默失效——而它的作者以为自己写好了。
    #[test]
    fn fallback_descriptions_only_describe_real_commands() {
        for (command, _) in FALLBACK_TOOLS {
            assert!(
                aevum_core::mcp_routes::COMMANDS.contains(command),
                "FALLBACK_TOOLS 里的 {command} 不在命令目录里"
            );
        }
    }

    /// 「名字拼错」与「GUI 没这项能力」必须是两句不同的话。
    /// 共用一句「未知工具」会让用户对着正确的拼写反复检查。
    #[test]
    fn unknown_tool_message_distinguishes_two_causes() {
        let gui_lacks = unknown_tool_message("aevum_measure");
        assert!(gui_lacks.contains("只在服务模式"), "{gui_lacks}");
        assert!(!gui_lacks.contains("未知工具"), "{gui_lacks}");
        // 旧前缀也要识别成同一个工具
        assert!(unknown_tool_message("lumen_measure").contains("只在服务模式"));

        let typo = unknown_tool_message("aevum_nope");
        assert!(typo.contains("未知工具"), "{typo}");
        assert!(!typo.contains("只在服务模式"), "{typo}");
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

// ---------------------------------------------------------------- QA 独立验收
//
// 刻意单独一个模块：上面的 `tests` 是实现者写的，这里只做**独立复核**——
// 「列出来的都能调」「调得动的是对的那条」「报错分得清是哪一种」。
// 只加测试，不碰实现。

/// 独立验收测试：由验收者另写一遍，**故意不复用** `tests` 里的断言。
///
/// 同一个错误假设若被同一个人抄两遍，测试只是把错误说了两次。所以这里从公开行为
/// 出发另立视角：走真实的 stdio 会话、起一个假控制端口，检查「`tools/list` 里出现的
/// 名字都能调用、调用都落到该落的命令」，并穷举名字被改动后不会误命中。
///
/// `qa_newer_gui_extra_command_is_skipped_not_listed` 覆盖跨版本：GUI 比命令行新时，
/// 多出来的命令**不进** `tools/list`（列出来也调不通，只会让 agent 反复试错），
/// 同时在 stderr 提示一句让人去对齐版本。
#[cfg(test)]
mod qa_acceptance {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn catalog() -> &'static [&'static str] {
        aevum_core::mcp_routes::COMMANDS
    }

    fn target_at(port: u16) -> ControlTarget {
        ControlTarget {
            port,
            token: "t".to_owned(),
            root: String::new(),
            version: String::new(),
        }
    }

    /// 跑一段 stdio 会话，返回逐行解析好的响应。
    fn stdio_session(requests: &[Value], port: u16) -> Vec<Value> {
        let mut input = String::new();
        for request in requests {
            input.push_str(&serde_json::to_string(request).expect("请求可序列化"));
            input.push('\n');
        }
        let mut output = Vec::new();
        serve(
            Cursor::new(input.into_bytes()),
            &mut output,
            &target_at(port),
        )
        .expect("stdio 会话不该断");
        String::from_utf8(output)
            .expect("响应是 UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("响应是 JSON"))
            .collect()
    }

    fn call(id: u64, name: &str) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
               "params": {"name": name, "arguments": {}}})
    }

    fn list(id: u64) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {}})
    }

    /// 工具（错误响应）里的文案
    fn error_text(response: &Value) -> String {
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// 工具调用结果里的正文
    fn result_text(response: &Value) -> String {
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    // ------------------------------------------------------------ 2. 路由完备性

    #[test]
    fn qa_every_catalog_name_routes_back_to_its_own_command() {
        for id in catalog() {
            let name = mcp_name(id);
            assert_eq!(
                resolve_command(&name),
                Some(*id),
                "{name} 在 tools/list 里出现过，却路由不到 {id}"
            );
            // 控制通道原生客户端直接传命令 ID
            assert_eq!(resolve_command(id), Some(*id), "{id} 本身不可路由");
        }
    }

    #[test]
    fn qa_every_server_tool_name_matches_its_declared_route() {
        for route in aevum_core::mcp_routes::TOOL_ROUTES {
            assert!(is_known_tool(route.tool), "{} 不是已知工具", route.tool);
            match route.command {
                Some(command) => {
                    assert_eq!(
                        resolve_command(route.tool),
                        Some(command),
                        "{} 应当路由到 {command}",
                        route.tool
                    );
                    assert!(
                        catalog().contains(&command),
                        "{} 指向的 {command} 不在命令目录里",
                        route.tool
                    );
                }
                None => {
                    assert_eq!(
                        resolve_command(route.tool),
                        None,
                        "{} 在 GUI 侧没有命令，不该解析出路由",
                        route.tool
                    );
                }
            }
        }
    }

    /// 特别点名的三组：同名双入口、旧前缀、历史错位。
    #[test]
    fn qa_named_pairs_resolve_to_the_same_command() {
        // align.run 有两个可用名字
        assert_eq!(resolve_command("aevum_align"), Some("align.run"));
        assert_eq!(resolve_command("aevum_align_run"), Some("align.run"));
        assert_eq!(
            resolve_command("aevum_align"),
            resolve_command("aevum_align_run")
        );

        // sequence.info：新名、旧名、以及机械名三者必须一致
        let new = resolve_command("aevum_sequence_info");
        assert_eq!(new, Some("sequence.info"));
        assert_eq!(resolve_command("lumen_sequence_info"), new);
        assert_eq!(
            resolve_command("lumen_sequence_info"),
            resolve_command("aevum_sequence_info")
        );

        // 历史错位：read/write 与 import/export 指同一件事，都得通
        assert_eq!(resolve_command("aevum_xmp_read"), Some("xmp.import"));
        assert_eq!(resolve_command("aevum_xmp_import"), Some("xmp.import"));
        assert_eq!(resolve_command("aevum_xmp_write"), Some("xmp.export"));
        assert_eq!(resolve_command("aevum_xmp_export"), Some("xmp.export"));
        // 旧名前缀下也要落到同一个命令
        assert_eq!(resolve_command("lumen_xmp_read"), Some("xmp.import"));
        assert_eq!(resolve_command("lumen_xmp_write"), Some("xmp.export"));

        // 其余「词序一致」的不需要别名，机械名就对
        assert_eq!(
            resolve_command("aevum_open_sequence"),
            Some("sequence.open")
        );
        assert_eq!(resolve_command("aevum_open_sequence_run"), None);
    }

    /// 旧前缀与新前缀：77 条命令 + 34 个工具，逐个比对必须逐字节相同。
    #[test]
    fn qa_legacy_prefix_agrees_with_new_prefix_everywhere() {
        for id in catalog() {
            let suffix = id.replace('.', "_");
            assert_eq!(
                resolve_command(&format!("lumen_{suffix}")),
                resolve_command(&format!("aevum_{suffix}")),
                "{id} 的新旧前缀解析结果不一致"
            );
        }
        for route in aevum_core::mcp_routes::TOOL_ROUTES {
            let suffix = route.tool.trim_start_matches("aevum_");
            assert_eq!(
                resolve_command(&format!("lumen_{suffix}")),
                resolve_command(route.tool),
                "{} 的新旧前缀解析结果不一致",
                route.tool
            );
        }
        // 裸前缀 / 大小写不对，都不能瞎命中
        assert_eq!(resolve_command("lumen_"), None);
        assert_eq!(resolve_command("aevum_"), None);
        assert_eq!(resolve_command("lumen_nope"), None);
    }

    // ------------------------------------------------------------ 3. 反例攻击

    #[test]
    fn qa_mutated_names_never_resolve() {
        // 一律期望 None：这些都是「不在 tools/list 里」的写法，
        // 悄悄放行等于给 agent 一条看不见的后门。
        for name in [
            "AEVUM_GRADE_GET",
            "Aevum_grade_get",
            "aevum_GRADE_GET",
            "aevum_grade.set",
            "aevum.sequence.info",
            "aevum__grade_get",
            "aevum_grade__get",
            "_aevum_grade_get",
            "aevum_grade_get_",
            "aevum_grade_get_extra",
            " aevum_grade_get",
            "aevum_grade_get ",
            "aevum_grade get",
            "aevum-grade-get",
            "evum_grade_get",
            "aevum_",
            "lumen_",
            "",
            "grade",
            "grade.Set",
            "GRADE.SET",
            "aevum_lumen_grade_get",
            "lumen_aevum_grade_get",
            "aevum_align.status",
            "aevum_sequence.info",
        ] {
            assert_eq!(resolve_command(name), None, "{name:?} 不该被解析出来");
        }

        // 反过来：规范化之后的正确写法必须通
        for (name, command) in [
            ("aevum_grade_get", "grade.get"),
            ("aevum_grade_auto", "grade.auto"),
            ("aevum_library_stack", "library.stack"),
            ("aevum_mask_component", "mask.component"),
            ("aevum_history_jump", "history.jump"),
            ("aevum_ui_screenshot", "ui.screenshot"),
            ("aevum_sequence_check", "sequence.check"),
        ] {
            assert_eq!(resolve_command(name), Some(command), "{name} 应当通");
        }
    }

    /// 查表顺序陷阱：`resolve_command` 先查 TOOL_ROUTES，再查命令 ID，最后机械匹配。
    /// 如果某个工具名在机械匹配下能命中**另一条**命令，就会「后一条被前一条盖住」。
    /// 这里把 34 个工具名逐个展开机械匹配集合，要求它要么为空、要么只含自己那条。
    /// 扫描「工具名 × 机械可达的命令」，返回会被抢走的名字。
    ///
    /// 抽成函数是为了做**反例自检**：一个永远返回空表的检测器同样「全绿」，
    /// 那不能当证据。
    fn find_shadows(
        routes: &[(&str, Option<&str>)],
        commands: &[&str],
    ) -> BTreeMap<String, Vec<String>> {
        let mut shadows: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (tool, command) in routes {
            // 机械路径（mcp_name(命令) == 这个名字）能命中哪几条命令
            let mechanical: Vec<&str> = commands
                .iter()
                .copied()
                .filter(|id| mcp_name(id) == *tool)
                .collect();
            for hit in mechanical {
                // 命中自己那条命令没问题；命中别人就是覆盖
                let expected = matches!(command, Some(own) if *own == hit);
                if !expected {
                    shadows
                        .entry((*tool).to_owned())
                        .or_default()
                        .push(hit.to_owned());
                }
            }
        }
        shadows
    }

    #[test]
    fn qa_no_tool_name_is_shadowed_by_a_different_command() {
        let real: Vec<(&str, Option<&str>)> = aevum_core::mcp_routes::TOOL_ROUTES
            .iter()
            .map(|route| (route.tool, route.command))
            .collect();
        let shadows = find_shadows(&real, catalog());
        assert!(
            shadows.is_empty(),
            "这些工具名会被另一条命令抢走（后一条覆盖前一条）：{shadows:?}"
        );

        // 反例自检：假设有人加了一条 `cancel` 命令，`aevum_cancel` 仍然标着
        // 「GUI 没这项能力」——它会被静默抢走，检测器必须报出来。
        // 同理，若命令目录里另有一条 `xmp.read`，`aevum_xmp_read` 就会同时被
        // 登记到 `xmp.import` 又能机械命中 `xmp.read`，也必须报出来。
        let probe = find_shadows(
            &[
                ("aevum_cancel", None),
                ("aevum_xmp_read", Some("xmp.import")),
            ],
            &["cancel", "xmp.import", "xmp.read"],
        );
        assert_eq!(
            probe.get("aevum_cancel"),
            Some(&vec!["cancel".to_owned()]),
            "检测逻辑抓不到「标了服务模式却被机械匹配抢走」"
        );
        assert_eq!(
            probe.get("aevum_xmp_read"),
            Some(&vec!["xmp.read".to_owned()]),
            "检测逻辑抓不到「指向了另一条命令」"
        );
        // 正常的那条不该被误报
        assert_eq!(
            find_shadows(&[("aevum_xmp_read", Some("xmp.import"))], catalog()),
            BTreeMap::new()
        );

        // 三条查表路径里的第二条（直接传命令 ID）也不能抢名字：
        // 命令 ID 是「组.动作」、工具名是 aevum_ 前缀无点号，两个命名空间必须不相交。
        for route in aevum_core::mcp_routes::TOOL_ROUTES {
            assert!(
                !catalog().contains(&route.tool),
                "命令目录里出现了与工具同名的 ID：{}",
                route.tool
            );
        }
    }

    /// 命令名之间也不能撞：两条命令若只差 `.` 与 `_`，机械工具名会重名，
    /// 其中一条就永远调不到。
    #[test]
    fn qa_no_two_commands_share_a_tool_name() {
        let mut by_name: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for id in catalog() {
            by_name.entry(mcp_name(id)).or_default().push(id);
        }
        let clashes: BTreeMap<&String, &Vec<&str>> =
            by_name.iter().filter(|(_, ids)| ids.len() > 1).collect();
        assert!(clashes.is_empty(), "工具名重名：{clashes:?}");
        assert_eq!(by_name.len(), 77, "77 条命令应当正好生成 77 个工具名");
    }

    /// 离线兜底清单：77 条、不重名、描述不空、每条都能路由。
    /// 走 `serve` 的 `tools/list` 而不是直接调私有函数——这样验的是客户端真正看到的报文。
    #[test]
    fn qa_offline_tool_list_is_well_formed() {
        // 端口 1 不会有人监听：tools/list 必然回落到内置清单。
        let responses = stdio_session(&[list(1)], 1);
        let tools = responses[0]["result"]["tools"]
            .as_array()
            .expect("tools 数组")
            .clone();
        assert_eq!(tools.len(), 77, "离线清单不是 77 条");

        let mut names = Vec::new();
        let mut described = 0usize;
        for tool in &tools {
            let name = tool["name"].as_str().expect("name 是字符串").to_owned();
            let description = tool["description"].as_str().unwrap_or_default();
            assert!(!description.trim().is_empty(), "{name} 的描述是空的");
            assert!(
                tool["inputSchema"].is_object(),
                "{name} 的 inputSchema 不是对象"
            );
            if !description.contains("GUI 离线") {
                described += 1;
            }
            assert!(
                resolve_command(&name).is_some(),
                "离线清单里的 {name} 路由不到任何命令"
            );
            names.push(name);
        }
        let unique: BTreeSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), 77, "离线清单里有重名工具");

        // 期望集合：77 条命令的机械名，逐个比对（顺序无关）
        let expected: BTreeSet<String> = catalog().iter().map(|id| mcp_name(id)).collect();
        let got: BTreeSet<String> = names.iter().cloned().collect();
        assert_eq!(got, expected, "离线清单与命令目录不是同一套名字");
        assert!(
            described >= 60,
            "只有 {described} 条写了具体描述，其余全是通用说明"
        );
    }

    // ------------------------------------------------------------ 4. 报错路径

    #[test]
    fn qa_error_branches_are_reachable_at_runtime() {
        // 这些名字都过不了 resolve_command，于是走 unknown_tool_message。
        let gui_lacks = [
            "aevum_measure",
            "aevum_deflicker_analyze",
            "aevum_keyframe_add",
            "aevum_keyframe_remove",
            "aevum_keyframe_list",
            "aevum_render_preview",
            "aevum_cancel",
        ];
        let mut requests = Vec::new();
        for (index, name) in gui_lacks.iter().enumerate() {
            requests.push(call(index as u64 + 1, name));
        }
        // 旧前缀 + 两个拼错的 + 一个大小写错的
        requests.push(call(100, "lumen_measure"));
        requests.push(call(101, "lumen_keyframe_list"));
        requests.push(call(102, "aevum_nope"));
        requests.push(call(103, "AEVUM_GRADE_GET"));

        let responses = stdio_session(&requests, 1);
        assert_eq!(responses.len(), requests.len(), "每条请求都该有响应");

        for (index, name) in gui_lacks.iter().enumerate() {
            let text = error_text(&responses[index]);
            assert_eq!(responses[index]["error"]["code"], json!(-32602));
            assert!(
                text.contains("只在服务模式"),
                "{name} 应当报「只在服务模式可用」，实际：{text}"
            );
            assert!(!text.contains("未知工具"), "{name} 被误判成拼错了：{text}");
            assert!(text.contains("77"), "{name} 的提示没给出核对办法：{text}");
        }

        // 旧名要归到「GUI 没能力」，不是「拼错」
        for offset in [100usize, 101] {
            let index = offset - 100 + gui_lacks.len();
            let text = error_text(&responses[index]);
            assert!(
                text.contains("只在服务模式"),
                "旧名应当报「只在服务模式可用」，实际：{text}"
            );
            assert!(!text.contains("未知工具"), "旧名被误判成拼错：{text}");
        }

        // 拼错的必须报「未知工具」
        for offset in [102usize, 103] {
            let index = offset - 100 + gui_lacks.len();
            let text = error_text(&responses[index]);
            assert!(
                text.contains("未知工具"),
                "拼错的名字应当报「未知工具」，实际：{text}"
            );
            assert!(!text.contains("只在服务模式"), "{text}");
        }
    }

    // ------------------------------------------------------------ 在线链路（假 GUI）

    /// 假 GUI：`system.commands` 回它的命令目录，其它方法把收到的命令 ID 原样回显。
    /// 目录外的方法按 `control.rs::rpc_response` 的口径回 -32601。
    struct FakeGui {
        port: u16,
        seen: JoinHandle<Vec<String>>,
    }

    fn spawn_fake_gui(catalog: Vec<(String, String)>, expected: usize) -> FakeGui {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("绑定假控制端口");
        // 非阻塞 + 截止时间：连接数对不上时宁可报错，也不要让整个测试二进制挂住。
        listener.set_nonblocking(true).expect("设置非阻塞");
        let port = listener.local_addr().expect("本地地址").port();
        let seen = std::thread::spawn(move || {
            let mut methods = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while methods.len() < expected {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "假 GUI 等不到第 {} 个连接（预期共 {expected} 个）",
                            methods.len() + 1
                        );
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("接受控制连接失败：{e}"),
                };
                stream.set_nonblocking(false).expect("恢复阻塞模式");
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("设置读超时");
                let mut reader = BufReader::new(stream.try_clone().expect("克隆流"));
                let mut request_line = String::new();
                reader.read_line(&mut request_line).expect("读请求行");
                assert!(
                    request_line.starts_with("POST /rpc HTTP/1.1"),
                    "{request_line}"
                );
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).expect("读头") == 0 {
                        break;
                    }
                    let line = line.trim_end_matches(['\r', '\n']);
                    if line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.trim().eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().expect("Content-Length");
                    }
                }
                let mut body = vec![0u8; length];
                reader.read_exact(&mut body).expect("读请求体");
                let request: Value = serde_json::from_slice(&body).expect("请求是 JSON");
                let method = request["method"].as_str().unwrap_or_default().to_owned();
                methods.push(method.clone());

                let payload = if method == "system.commands" {
                    let commands: Vec<Value> = catalog
                        .iter()
                        .map(|(id, summary)| {
                            json!({
                                "id": id,
                                "summary": summary,
                                "params": "无参数",
                                "schema": {"type": "object"},
                            })
                        })
                        .collect();
                    json!({"jsonrpc": "2.0", "id": 1, "result": {"commands": commands}})
                } else if catalog.iter().any(|(id, _)| *id == method) {
                    json!({"jsonrpc": "2.0", "id": 1, "result": {"echo": method}})
                } else {
                    json!({"jsonrpc": "2.0", "id": 1,
                           "error": {"code": -32601, "message": format!("未知命令：{method}")}})
                };
                let text = serde_json::to_string(&payload).expect("响应可序列化");
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    text.len(),
                    text
                )
                .expect("写响应");
                stream.flush().expect("刷新");
            }
            methods
        });
        FakeGui { port, seen }
    }

    fn full_catalog() -> Vec<(String, String)> {
        catalog()
            .iter()
            .map(|id| ((*id).to_owned(), format!("{id} 的说明")))
            .collect()
    }

    /// 在线：`tools/list` 列出的每一条都必须调得动，而且要落到**它自己**那条命令。
    #[test]
    fn qa_online_list_entries_are_all_callable_and_land_on_the_right_command() {
        let gui = spawn_fake_gui(full_catalog(), 1 + 77);
        let mut requests = vec![list(0)];
        for (index, id) in catalog().iter().enumerate() {
            requests.push(call(index as u64 + 1, &mcp_name(id)));
        }
        let responses = stdio_session(&requests, gui.port);
        let seen = gui.seen.join().expect("假 GUI 线程");

        let tools = responses[0]["result"]["tools"]
            .as_array()
            .expect("tools 数组");
        assert_eq!(tools.len(), 77, "在线清单不是 77 条");

        let listed: BTreeSet<String> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
            .collect();
        let expected: BTreeSet<String> = catalog().iter().map(|id| mcp_name(id)).collect();
        assert_eq!(listed, expected, "在线清单与命令目录不是同一套名字");

        for (index, id) in catalog().iter().enumerate() {
            let response = &responses[index + 1];
            assert_eq!(
                response["result"]["isError"],
                json!(false),
                "{id} 调不动：{response}"
            );
            let echoed: Value = serde_json::from_str(&result_text(response)).expect("回显是 JSON");
            assert_eq!(echoed["echo"], json!(id), "{id} 落到了别的命令：{echoed}");
        }

        // 第一条是拉清单，其后每条都必须正好是它对应的命令 ID，一个不多一个不少
        assert_eq!(seen[0], "system.commands");
        let forwarded: BTreeSet<String> = seen[1..].iter().cloned().collect();
        let expected_ids: BTreeSet<String> = catalog().iter().map(|id| (*id).to_owned()).collect();
        assert_eq!(forwarded, expected_ids, "转给 GUI 的命令集合不对");
        assert_eq!(seen.len(), 78);
    }

    /// 跨版本推演之一：**旧 GUI（60 条）+ 新 MCP（77 条）**。
    /// `tools/list` 从 GUI 取，于是只列 60 条，而这 60 条都能路由——
    /// 不会产生新的「列出来调不动」。本地多出来的 16 条没被列出，但若被硬调，
    /// 会被 GUI 以 -32601 拒掉，落到 isError:true 的业务错误（不挂、不报「未知工具」）。
    #[test]
    fn qa_older_gui_subset_is_listed_and_callable() {
        let older = full_catalog().into_iter().take(60).collect::<Vec<_>>();
        let absent: Vec<&str> = catalog().iter().skip(60).copied().collect();
        assert_eq!(absent.len(), 17, "跨版本场景按 60/77 推演");

        let gui = spawn_fake_gui(older, 1 + 60 + 1);
        let mut requests = vec![list(0)];
        for (index, id) in catalog().iter().take(60).enumerate() {
            requests.push(call(index as u64 + 1, &mcp_name(id)));
        }
        // 再硬调一条旧 GUI 没有的
        requests.push(call(999, &mcp_name(absent[0])));

        let responses = stdio_session(&requests, gui.port);
        let _ = gui.seen.join().expect("假 GUI 线程");

        let tools = responses[0]["result"]["tools"]
            .as_array()
            .expect("tools 数组");
        assert_eq!(tools.len(), 60, "旧 GUI 只报 60 条");

        for (index, id) in catalog().iter().take(60).enumerate() {
            let response = &responses[index + 1];
            assert_eq!(
                response["result"]["isError"],
                json!(false),
                "{id} 在旧 GUI 上列出来了却调不动：{response}"
            );
            let echoed: Value = serde_json::from_str(&result_text(response)).expect("回显是 JSON");
            assert_eq!(echoed["echo"], json!(id));
        }

        // 本地表里多出来的那条：路由得到（本地表是新的），但 GUI 拒收 → 业务错误
        let extra = &responses[61];
        assert!(extra.get("error").is_none(), "不该是协议错误：{extra}");
        assert_eq!(extra["result"]["isError"], json!(true));
        let text = result_text(extra);
        assert!(text.contains("未知命令"), "应当是 GUI 侧的未知命令：{text}");
        assert!(
            !text.contains("只在服务模式"),
            "不该说成「只在服务模式」：{text}"
        );
    }

    /// 跨版本推演之二：**新 GUI（77+）+ 旧命令行**。
    ///
    /// `tools/list` 从 GUI 取、路由表在本地，这个组合本来会制造出「列出来、永远调不通」
    /// 的工具。现在改成不列：agent 拿到一条看着合法的工具名只会反复试错，而人从启动时
    /// 那句「GUI 报了 N 条本机不认识的命令」就能看出两边版本没对齐。
    #[test]
    fn qa_newer_gui_extra_command_is_skipped_not_listed() {
        let mut newer = full_catalog();
        newer.push(("library.tags".to_owned(), "新命令".to_owned()));
        // 只有一次连接：那条新命令不进列表，也就没人会去调它。
        let gui = spawn_fake_gui(newer, 1);
        let responses = stdio_session(&[list(0)], gui.port);
        let _ = gui.seen.join().expect("假 GUI 线程");

        let tools = responses[0]["result"]["tools"]
            .as_array()
            .expect("tools 数组");
        assert_eq!(tools.len(), 77, "本机不认识的新命令不该出现在列表里");
        assert!(
            !tools
                .iter()
                .any(|tool| tool["name"] == json!("aevum_library_tags")),
            "新命令被列出来了 —— 它调不通，列出来只会让 agent 反复试错"
        );
        // 原有 77 条一条不能少：过滤只该去掉「不认识」，不该误伤
        for id in aevum_core::mcp_routes::COMMANDS {
            let name = mcp_name(id);
            assert!(
                tools.iter().any(|tool| tool["name"] == json!(name)),
                "过滤误伤了 {name}"
            );
        }
    }
}
