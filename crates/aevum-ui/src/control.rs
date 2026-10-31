//! GUI 回环控制：在本机 `127.0.0.1` 上开一条极小的命令通道。
//!
//! 对应计划 8.1 的三种运行形态：GUI 自己起服务并写 `<root>/.aevum/control.json`，
//! `aevum.exe --mcp --attach <control.json>` 再把 MCP 工具代理到这条通道。
//!
//! 设计约束：
//! - 只绑 `127.0.0.1`，端口随机；令牌每次启动重新生成，不落盘到别处。
//! - 传输是手写的 HTTP/1.1（一次请求一个连接）承载单个 JSON-RPC 2.0 对象，
//!   只服务 `POST /rpc` 与 `GET /health`；请求体上限 1 MiB，读写都有超时。
//! - 命令**不在这里执行**：一条 [`ControlEvent`] 投递到 UI 线程，结果经一次性
//!   reply 通道回传，保证 agent 的修改和人在界面上的操作走同一条状态路径。
//! - 无窗口单测通过 [`CommandExecutor`] 注入 mock，不需要真实 GUI。
//!
//! 安全边界：绝不删除素材，只删自己写下的那份 control.json；令牌不对就不动文件。

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 请求体上限：命令参数都是小对象，1 MiB 足够，也挡住误发整帧数据。
pub const MAX_BODY: usize = 1024 * 1024;
/// 请求行 / 请求头各自的上限。
const MAX_LINE: usize = 8 * 1024;
const MAX_HEAD: usize = 32 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(8);
const WRITE_TIMEOUT: Duration = Duration::from_secs(8);
/// UI 线程最多等这么久；超时把错误交给调用方，而不是无限占着连接。
pub const UI_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------- control.json

/// `<root>/.aevum/control.json` 的内容。
///
/// 客户端只依赖 `port` + `token`；其余字段用于诊断「这是哪一次运行写下的文件」。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlFile {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
    /// Unix 秒。
    pub started: u64,
    /// 序列根目录，方便人读；令牌才是真正的凭据。
    #[serde(default)]
    pub root: String,
}

/// `<root>/.aevum/control.json`（护带目录尚未迁移时落在旧目录里）。
pub fn control_path(root: &Path) -> PathBuf {
    aevum_core::project::guard_dir(root).join("control.json")
}

impl ControlFile {
    /// 原子写：先写同目录临时文件，再改名覆盖。失败时绝不删旧文件。
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let dir = path
            .parent()
            .ok_or_else(|| "control.json 路径没有父目录".to_owned())?;
        fs::create_dir_all(dir).map_err(|e| format!("创建护带目录失败：{e}"))?;
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("序列化控制文件失败：{e}"))?;
        let tmp = dir.join(format!(".control.json.{}.tmp", std::process::id()));
        fs::write(&tmp, text.as_bytes()).map_err(|e| format!("写控制文件失败：{e}"))?;
        match fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(format!("替换控制文件失败：{e}"))
            }
        }
    }

    /// 读回控制文件；错误文案直接给 attach 模式用。
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("读取控制文件失败（{}）：{e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("控制文件格式不对：{e}"))
    }
}

// ---------------------------------------------------------------- 命令执行器

/// UI 线程侧收到的执行请求。`reply` 是一次性回复通道。
pub struct ControlEvent {
    pub id: String,
    pub args: Value,
    pub reply: Sender<Result<Value, String>>,
}

/// 在界面线程执行一条命令。实现可以是闭包（见 `AppState::sync_control`），
/// 单测里换成 mock 即可脱离窗口。
pub trait CommandExecutor: Send + Sync + 'static {
    fn execute(&self, id: &str, args: &Value) -> Result<Value, String>;
}

impl<F> CommandExecutor for F
where
    F: Fn(&str, &Value) -> Result<Value, String> + Send + Sync + 'static,
{
    fn execute(&self, id: &str, args: &Value) -> Result<Value, String> {
        self(id, args)
    }
}

// ---------------------------------------------------------------- 命令清单

/// 一条可被控制通道 / AI 面板 / MCP 附着模式调用的命令。
pub struct CommandSpec {
    pub id: &'static str,
    /// 一句话说明，直接显示在 AI 面板与 MCP 的 tool description 里。
    pub summary: &'static str,
    /// 参数提示（中文短句）。
    pub params: &'static str,
    /// JSON Schema 原文，按需解析，避免启动时构造一堆对象。
    schema: &'static str,
}

impl CommandSpec {
    pub fn schema(&self) -> Value {
        serde_json::from_str(self.schema).unwrap_or_else(|_| json!({"type": "object"}))
    }
}

/// 首批命令清单。
///
/// 这里是唯一的一份「ID + 说明 + 参数 + Schema」；执行体在
/// `AppState::run_ui_command`，MCP 附着模式通过 `system.commands` 读这份清单。
pub fn command_catalog() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "sequence.info",
            summary: "当前序列概览：根目录、帧数、分辨率、缺帧、去闪与对齐摘要。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "sequence.open",
            summary: "打开一个帧文件夹作为当前序列；界面会立刻切过去并重新测光。",
            params: "root：帧文件夹路径",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"root":{"type":"string","description":"帧文件夹路径（绝对或相对）"}},"required":["root"]}"#,
        },
        CommandSpec {
            id: "sequence.check",
            summary: "序列体检：后台挑出坏帧 / 黑帧 / 虚焦 / 重复 / 亮度跳变 / 编号缺口，立刻返回「已启动」。",
            params: "sig：阈值松紧，0.5~20，默认沿用面板上的值（越小越灵敏）",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"sig":{"type":"number","minimum":0.5,"maximum":20,"description":"判定门槛是本序列局部波动的多少倍；越小越灵敏"}}}"#,
        },
        CommandSpec {
            id: "sequence.status",
            summary: "读取体检进度与最近一次清单：忙碌标记、done/total、分类计数、逐条发现。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "sequence.cancel",
            summary: "请求取消正在跑的体检；在一批文件之间生效，清单保持上一次结果。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "frame.select",
            summary: "选中某一帧（1 开始）。",
            params: "index：帧号，从 1 开始",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"帧号，从 1 开始"}},"required":["index"]}"#,
        },
        CommandSpec {
            id: "ui.module",
            summary: "切换右侧模块：序列 / 去闪 / 显影 / 构图 / 成片 / 声音。",
            params: "name：模块名或 0-5",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string","description":"序列/去闪/显影/构图/成片/声音，或 0-5"}},"required":["name"]}"#,
        },
        CommandSpec {
            id: "ui.dock",
            summary: "切换右侧面板并展开它：调整 / 信息 / 关键帧 / AI / 历史。",
            params: "name：面板名或 0-4",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string","description":"调整/信息/关键帧/AI/历史，或 0-4"}},"required":["name"]}"#,
        },
        CommandSpec {
            id: "grade.get",
            summary: "读取当前调色参数与预设名。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "grade.auto",
            summary: "自动校正：按 1/50/99 百分位给出白平衡与整套影调，写绝对覆盖值；同一帧连点结果不变。已经曝好的图可能只校白平衡。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "grade.set",
            summary: "按字段合并调色参数，例如 {\"exposure\":0.5,\"contrast\":12}；未知字段报错。",
            params: "任意 GradeParams 字段（也可包在 params 对象里）",
            schema: r#"{"type":"object","additionalProperties":true,"properties":{"params":{"type":"object","description":"GradeParams 的部分字段：exposure/contrast/highlights/shadows/whites/blacks/temperature/tint/vibrance/saturation/clarity/dehaze/grain/vignette/曲线与 HSL 等","additionalProperties":true}},"description":"字段可以直接放顶层，也可以放进 params 对象"}"#,
        },
        CommandSpec {
            id: "grade.preset",
            summary: "套用一个内置调色预设。",
            params: "name：预设名（原始/鲜艳风光/落日暖调/冷色电影…）",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string","description":"原始/鲜艳风光/落日暖调/冷色电影/复古胶片/黑白对比/夜景增强/通透风光/柔和人像/青橙电影"}},"required":["name"]}"#,
        },
        CommandSpec {
            id: "grade.reset",
            summary: "把调色参数恢复为默认值（预设「原始」）。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "grade.copy",
            summary: "命名调色副本：同一序列多套调色并存。list 列清单与当前命中的副本；save 把当前调色+关键帧存成副本（同名需 overwrite）；apply 取回当前；delete / rename 管理清单。导出与队列可按副本名指定用哪一套。",
            params: "op?：list/save/apply/delete/rename（省=list）；name?：副本名；overwrite?：save 同名覆盖；to?：rename 的新名字",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"op":{"type":"string","enum":["list","save","apply","delete","rename"],"description":"省略即 list"},"name":{"type":"string","description":"副本名（save/apply/delete 必填，rename 填旧名）"},"overwrite":{"type":"boolean","description":"save 时同名覆盖；默认报错"},"to":{"type":"string","description":"rename 的新名字"}}}"#,
        },
        CommandSpec {
            id: "xmp.import",
            summary: "读入 Lightroom / Camera Raw 的 .xmp 预设并整体替换当前调色参数。",
            params: "path：.xmp 文件路径",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","description":"要读入的 .xmp 预设路径"}},"required":["path"]}"#,
        },
        CommandSpec {
            id: "xmp.export",
            summary: "把当前调色参数写成 .xmp 预设；覆盖已有文件需 confirm: true。",
            params: "path：以 .xmp 结尾；confirm?：覆盖时必填",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","description":"输出的 .xmp 路径"},"confirm":{"type":"boolean","description":"目标已存在时必须为 true"}},"required":["path"]}"#,
        },
        CommandSpec {
            id: "cube.import",
            summary: "读入一张 .cube 三维查找表挂成「外观」，套在整条显影链之前；apply:false 只体检不改参数。",
            params: "path：.cube 文件；apply?：默认 true",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","description":"要读入的 .cube 外观路径"},"apply":{"type":"boolean","description":"false 时只解析并回报规格，不改变当前参数"}},"required":["path"]}"#,
        },
        CommandSpec {
            id: "cube.export",
            summary: "把当前调色（含已挂的外观）烘成 .cube 三维查找表，交给其它软件用；覆盖已有文件需 confirm: true。",
            params: "path：以 .cube 结尾；size?：格点数 2~256，默认 65；confirm?：覆盖时必填",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","description":"输出的 .cube 路径"},"size":{"type":"integer","minimum":2,"maximum":256,"description":"立方格点数，65 为专业常见精度；17 适合手机滤镜槽位"},"confirm":{"type":"boolean","description":"目标已存在时必须为 true"}},"required":["path"]}"#,
        },
        CommandSpec {
            id: "audio.config",
            summary: "读取或修改配乐设置（路径、延迟、音量、淡入淡出、循环、编码）；无参数即查询。",
            params: "path?（空串/null 清空）/delay?/gain?/fade_in?/fade_out?/loop?/trim?/acodec?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"path":{"type":["string","null"],"description":"配乐文件路径；空串或 null 清空配乐"},"delay":{"type":"number","minimum":0,"maximum":3600,"description":"延后进入（秒）"},"gain":{"type":"number","minimum":0,"maximum":400,"description":"音量百分比，100 为原始"},"fade_in":{"type":"number","minimum":0,"maximum":3600,"description":"淡入时长（秒）"},"fade_out":{"type":"number","minimum":0,"maximum":3600,"description":"淡出时长（秒）"},"loop":{"type":"boolean","description":"配乐短于画面时循环铺满"},"trim":{"type":"boolean","description":"配乐长于画面时截到画面结束"},"acodec":{"type":"string","enum":["auto","copy","aac128","aac192","aac256","aac320"]}}}"#,
        },
        CommandSpec {
            id: "mask.list",
            summary: "列出当前序列的局部蒙版：形状、组件数、强度与局部参数。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "mask.add",
            summary: "新增一张局部蒙版并选中，随后可用 mask.update 调几何与参数。",
            params: "kind?：linear（默认）/ radial / brush",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"kind":{"type":"string","enum":["linear","radial","brush"],"description":"线性渐变、径向椭圆或画笔（笔画在 mask.update 的 geometry.strokes 里给）"}}}"#,
        },
        CommandSpec {
            id: "mask.remove",
            summary: "删除指定序号的局部蒙版（只改工程参数，不动素材）。",
            params: "index：蒙版序号，从 1 开始",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"蒙版序号，从 1 开始"}},"required":["index"]}"#,
        },
        CommandSpec {
            id: "mask.update",
            summary: "修改蒙版强度、反选、局部参数增量、组内形状的叠加方式与几何，或整组亮度/颜色范围（亮度 0~1，色相按角度）；画笔形状用 geometry.strokes 整组替换笔画。",
            params: "index；component?；amount?；invert?；params?{曝光…}；geometry?{x1,y1,x2,y2,feather,midpoint,angle,kind,invert,blend,strokes?}；range?{lum_on,lum_min,lum_max,lum_feather,hue_on,hue,hue_width,hue_feather,hue_sat}",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"蒙版序号，从 1 开始"},"component":{"type":"integer","minimum":0,"description":"组内形状序号，从 0 开始，默认 0"},"amount":{"type":"number","minimum":0,"maximum":100},"invert":{"type":"boolean","description":"整张蒙版反选"},"params":{"type":"object","description":"局部参数增量，null 表示删除该键","additionalProperties":{"type":["number","null"]}},"geometry":{"type":"object","description":"归一化画布几何；画笔形状的笔画用 strokes 整组替换","additionalProperties":false,"properties":{"kind":{"type":"string","enum":["linear","radial","brush"]},"x1":{"type":"number"},"y1":{"type":"number"},"x2":{"type":"number"},"y2":{"type":"number"},"feather":{"type":"number","minimum":0,"maximum":100},"midpoint":{"type":"number","minimum":0,"maximum":100},"angle":{"type":"number"},"invert":{"type":"boolean","description":"当前形状反选"},"blend":{"type":"string","enum":["add","subtract","intersect"],"description":"与前面形状的布尔关系"},"strokes":{"type":"array","description":"画笔笔画整组替换；每条 {points:[[x,y]…], radius?, hardness?, flow?, erase?}，坐标 0~1 归一化，radius 按画面短边百分比（0.2~50），hardness/flow 0~1，erase=true 挖掉选区","items":{"type":"object","additionalProperties":false,"properties":{"points":{"type":"array","items":{"type":"array","items":{"type":"number","minimum":0,"maximum":1},"minItems":2,"maxItems":2},"minItems":1},"radius":{"type":"number","minimum":0.002,"maximum":0.5},"hardness":{"type":"number","minimum":0,"maximum":1},"flow":{"type":"number","exclusiveMinimum":0,"maximum":1},"erase":{"type":"boolean"}}}}},"range":{"type":"object","description":"整组亮度与颜色范围；亮度 0~1，色相按角度（hue 0~360，hue_width 0~360，hue_feather 0~180，hue_sat 饱和度下限 0~1）","additionalProperties":false,"properties":{"lum_on":{"type":"boolean"},"lum_min":{"type":"number","minimum":0,"maximum":1},"lum_max":{"type":"number","minimum":0,"maximum":1},"lum_feather":{"type":"number","minimum":0,"maximum":1},"hue_on":{"type":"boolean","description":"启用颜色范围"},"hue":{"type":"number","minimum":0,"maximum":360,"description":"目标色相（角度）"},"hue_width":{"type":"number","minimum":0,"maximum":360,"description":"收进来的色相带宽"},"hue_feather":{"type":"number","minimum":0,"maximum":180,"description":"带外淡出角度"},"hue_sat":{"type":"number","minimum":0,"maximum":1,"description":"饱和度下限，近灰像素落选"}}}},"required":["index"]}"#,
        },
        CommandSpec {
            id: "mask.component",
            summary: "在一张蒙版组里加或删一个形状；减去/交集的布尔选区就是这么搭出来的。",
            params: "index；action?：add（默认）/ remove；kind?：linear / radial / brush；component?：删除时的形状序号，从 0 开始",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"蒙版序号，从 1 开始"},"action":{"type":"string","enum":["add","remove"],"description":"加形状或删形状，默认 add"},"kind":{"type":"string","enum":["linear","radial","brush"],"description":"加形状时的形状，默认 linear"},"component":{"type":"integer","minimum":0,"description":"删除的形状序号，从 0 开始，默认当前选中"}},"required":["index"]}"#,
        },
        CommandSpec {
            id: "deflicker.set",
            summary: "读取或修改去闪烁设置（窗口、强度、多遍、分通道）；任何改动立刻重算并回摘要。",
            params: "window?/strength?/multi?/channels?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"window":{"type":"number","minimum":3,"maximum":199,"description":"平滑窗口，自动取奇数"},"strength":{"type":"number","minimum":0,"maximum":150,"description":"校正强度百分比，100 为完全校正"},"multi":{"type":"boolean","description":"长序列多遍窗口"},"channels":{"type":"boolean","description":"分通道（RGB）去闪烁，治彩色闪烁"}}}"#,
        },
        CommandSpec {
            id: "crop.set",
            summary: "读取或改写取景：手动裁剪框、拉直角度、画幅比例，一次命令定好最后留下的画面。",
            params: "x?/y?/width?/height?（四个一起给，源像素）/ rotate? / aspect? / reset?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"x":{"type":"number","minimum":0,"description":"取景框左上角 X（源像素，自动取偶）"},"y":{"type":"number","minimum":0,"description":"取景框左上角 Y（源像素，自动取偶）"},"width":{"type":"number","minimum":2,"description":"取景框宽（源像素，自动取偶）"},"height":{"type":"number","minimum":2,"description":"取景框高（源像素，自动取偶）"},"rotate":{"type":"number","minimum":-45,"maximum":45,"description":"拉直角度（度，正角=画面逆时针，绕画面中心）；取景框量的是转过之后的那一帧，0.2° 以内当没转"},"aspect":{"type":"string","enum":["原生","16:9","4:3","1:1","9:16"],"description":"画幅比例；改比例时已有取景框绕中心重铺"},"reset":{"type":"boolean","description":"true=取消手动裁剪与拉直，回到按比例内接的默认取景"}}}"#,
        },
        CommandSpec {
            id: "lens.set",
            summary: "读取或开关镜头配置文件校正：EXIF 匹配 lensfun 数据库折出的畸变/横向色差/暗角标定，先于手动三项生效；rematch 按当前 EXIF 重新匹配。",
            params: "enabled? / rematch?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"enabled":{"type":"boolean","description":"自动匹配开关；关掉暂存命中的校正，再开原样恢复"},"rematch":{"type":"boolean","description":"true=按当前 EXIF 重新跑一遍匹配（需要磁盘上有 lensfun 数据库）"}}}"#,
        },
        CommandSpec {
            id: "profile.set",
            summary: "读取或套用 Adobe 相机/镜头配置：kind=dcp 走 Adobe Camera Profile（只接 RAW/DNG 原始数据，接管解码后的白平衡、颜色矩阵与色调曲线）；kind=lcp 走 Adobe Lens Profile（按 EXIF 选帧，畸变/横向色差/暗角先于手动三项生效）。不带 path/auto/clear 即查询。",
            params: "kind（dcp|lcp）；path? / auto? / clear?",
            schema: r#"{"type":"object","additionalProperties":false,"required":["kind"],"properties":{"kind":{"type":"string","enum":["dcp","lcp"],"description":"dcp=相机配置，lcp=镜头配置"},"path":{"type":"string","description":"指定 .dcp 或 .lcp 文件；机型/镜头对不上会报错"},"auto":{"type":"boolean","description":"按当前 EXIF 在默认目录里自动匹配（设 AEVUM_DCP / AEVUM_LCP 可换目录）"},"clear":{"type":"boolean","description":"清掉当前配置，回到未套用"}}}"#,
        },
        CommandSpec {
            id: "library.add",
            summary: "把一个素材目录加进图库清单（跨会话常驻），随即编目并返回目录汇总。",
            params: "path：素材目录",
            schema: r#"{"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string","description":"素材目录；同一目录重复加入等于挪到末尾"}}}"#,
        },
        CommandSpec {
            id: "library.remove",
            summary: "从图库清单移除一个目录；不动磁盘上的文件与工程。",
            params: "index：图库里的目录序号（1 开始）",
            schema: r#"{"type":"object","additionalProperties":false,"required":["index"],"properties":{"index":{"type":"integer","minimum":1,"description":"library.list 给出的目录序号"}}}"#,
        },
        CommandSpec {
            id: "library.list",
            summary: "读图库：无参数列出根目录清单；带 index 编目该目录并列出逐文件评级/旗标/色标。",
            params: "index?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"给 1..=目录数就编目该目录并在 roots[index-1].files 里回逐条编目"}}}"#,
        },
        CommandSpec {
            id: "library.open",
            summary: "把图库里的一个目录开成序列（与 sequence.open 同一口径：失败报错，不吞旧序列）。",
            params: "index：图库里的目录序号（1 开始）",
            schema: r#"{"type":"object","additionalProperties":false,"required":["index"],"properties":{"index":{"type":"integer","minimum":1,"description":"library.list 给出的目录序号"}}}"#,
        },
        CommandSpec {
            id: "library.organize",
            summary: "在图库里改一张或一批照片的评级/旗标/色标，原子写回它们所在目录的工程文件；批量只写一次盘，其它帧与其它字段原样保留。",
            params: "frame / frames? / rating? / flag? / label? / root?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"root":{"type":"integer","minimum":1,"description":"图库目录序号；省略用当前展开的目录"},"frame":{"type":"integer","minimum":1,"description":"该目录编目的帧号（library.list 的 files[].frame），与 frames 二选一"},"frames":{"type":"array","items":{"type":"integer","minimum":1},"description":"一次改多帧（批量挑片），与 frame 二选一"},"rating":{"type":"integer","minimum":0,"maximum":5,"description":"星级；0 表示清掉"},"flag":{"type":"integer","enum":[-1,0,1],"description":"-1 排除 / 0 取消 / 1 选用"},"label":{"type":"integer","minimum":0,"maximum":4,"description":"色标：0 无、1 红、2 黄、3 绿、4 蓝"}}}"#,
        },
        CommandSpec {
            id: "library.keyword",
            summary: "给图库里的一张或一批照片加/去关键词，原子写回它们所在目录的工程文件；批量只写一次盘，词表归一化、去重、按字典序存。",
            params: "add?/remove? + frame/frames 二选一 + root?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"root":{"type":"integer","minimum":1,"description":"图库目录序号；省略用当前展开的目录"},"frame":{"type":"integer","minimum":1,"description":"该目录编目的帧号，与 frames 二选一"},"frames":{"type":"array","items":{"type":"integer","minimum":1},"description":"一次改多帧，与 frame 二选一"},"add":{"type":["string","array"],"description":"要加的词：字符串可按逗号/分号拆开，或直接给字符串数组"},"remove":{"type":["string","array"],"description":"要去的词，格式同 add"}}}"#,
        },
        CommandSpec {
            id: "library.metadata",
            summary: "读/写图库里某个目录的说明与版权信息，原子写进该目录的工程文件；没打开的目录也能写，空目录也行。不带参数只查询。",
            params: "root? + title? / description? / creator? / copyright?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"root":{"type":"integer","minimum":1,"description":"图库目录序号；省略用当前展开的目录"},"title":{"type":["string","null"],"description":"标题；写入 EXIF XPTitle"},"description":{"type":["string","null"],"description":"说明；写入 EXIF ImageDescription"},"creator":{"type":["string","null"],"description":"创作者/署名；写入 EXIF Artist"},"copyright":{"type":["string","null"],"description":"版权声明；写入 EXIF Copyright"}}}"#,
        },
        CommandSpec {
            id: "library.stack",
            summary: "图库堆叠：把同一目录里的几张照片并成一叠、拆散一叠，或按文件名前缀自动分叠；叠只是编目里的折叠视图，不搬文件、不动评级与关键词。",
            params: "op?（list/create/dissolve/auto） + root? + frames?（create） / stack?（dissolve）",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"op":{"type":"string","enum":["list","create","dissolve","auto"],"description":"省略或 list 只查询；create 把 frames 并成一叠；dissolve 拆掉 stack 那一叠；auto 按文件名前缀（去扩展名与结尾数字串）重排整个目录的堆叠"},"root":{"type":"integer","minimum":1,"description":"图库目录序号；省略用当前展开的目录"},"frames":{"type":"array","items":{"type":"integer","minimum":1},"description":"create 用：该目录编目的帧号，至少两个"},"stack":{"type":"integer","minimum":1,"description":"dissolve 用：叠号（1 开始，按上一次返回的 stacks 数）"}}}"#,
        },
        CommandSpec {
            id: "collection.create",
            summary: "新建一个跨目录手动集合；同名集合直接复用（返回已有序号）。",
            params: "name：集合名（空白叫「未命名集合」）",
            schema: r#"{"type":"object","additionalProperties":false,"required":["name"],"properties":{"name":{"type":"string","description":"集合名；只做跨目录导航，不搬动照片"}}}"#,
        },
        CommandSpec {
            id: "collection.add",
            summary: "把图库里的一张照片放进集合（只记路径，照片留在原处）；已在集合里返回 added=false。",
            params: "index?/name? + frame + root?",
            schema: r#"{"type":"object","additionalProperties":false,"required":["frame"],"properties":{"index":{"type":"integer","minimum":1,"description":"集合序号（1 开始）"},"name":{"type":"string","description":"集合名（与 index 二选一）"},"root":{"type":"integer","minimum":1,"description":"图库目录序号；省略用当前展开的目录"},"frame":{"type":"integer","minimum":1,"description":"该目录编目的帧号"}}}"#,
        },
        CommandSpec {
            id: "collection.remove",
            summary: "把一张照片从集合里拿掉：按 path 或按 root+frame 定位，照片与工程文件不动。",
            params: "index?/name? + path 或 frame+root?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"集合序号（1 开始）"},"name":{"type":"string","description":"集合名（与 index 二选一）"},"path":{"type":"string","description":"成员路径；给了就不看 root/frame"},"root":{"type":"integer","minimum":1,"description":"图库目录序号；省略用当前展开的目录"},"frame":{"type":"integer","minimum":1,"description":"该目录编目的帧号"}}}"#,
        },
        CommandSpec {
            id: "collection.delete",
            summary: "删掉一个集合本身；照片、各目录工程与根目录清单都不动。",
            params: "index?/name?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"集合序号（1 开始）"},"name":{"type":"string","description":"集合名（与 index 二选一）"}}}"#,
        },
        CommandSpec {
            id: "collection.list",
            summary: "列出手动集合（序号、名字、张数）；带 index 或 name 时回该集合的成员路径。",
            params: "index?/name?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"回这个集合的 files"},"name":{"type":"string","description":"按名字回 files（与 index 二选一）"}}}"#,
        },
        CommandSpec {
            id: "render.set",
            summary: "读取或改写成片输出设置：容器、JPEG 质量、EXIF 搬运、XMP 旁车、断点续跑、输出锐化、色域、水印、规格与目录；无参数即查询。",
            params: "mode?/format?/quality?/keep_exif?/xmp_sidecar?/resume?/sharpen_for?/sharpen_strength?/color_space?/watermark?（对象：enabled/kind/text/image/spot/opacity/size/margin/color）/width?+height?/out_dir?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"mode":{"type":"string","enum":["stills","video"],"description":"输出模式；video 的中间帧只能无损容器"},"format":{"type":"string","enum":["png8","tiff16","jpeg","webp","avif"],"description":"静帧容器；jpeg 与 avif 有损看 quality，webp 是 VP8L 无损"},"quality":{"type":"integer","minimum":1,"maximum":100,"description":"压缩质量，只对 jpeg / avif 生效；PNG/TIFF/WebP 是无损档"},"keep_exif":{"type":"boolean","description":"把源 JPEG 的 EXIF 块原样搬进 PNG/JPEG/WebP（机身、曝光、版权都在里面）；TIFF 容器写不进去"},"xmp_sidecar":{"type":"boolean","description":"每张导出的静帧旁边写一个同名 .xmp 旁车：dc: 的说明与版权四项 + crs: 的这一张调色参数，Lightroom / Camera Raw 直接认；视频模式的中间帧不写"},"resume":{"type":"boolean","description":"断点续跑：输出目录里已经写完的帧（连带该带的 .xmp 旁车）原样留着，这一趟只补没写出来的那些；改了参数想重导就别开，也别指望它认得出旧帧已经过期。成片模式下开了它必然走「先落中间帧再编码」两段路，管道直连那条不落盘、没得补"},"sharpen_for":{"type":"string","enum":["off","screen","glossy","matte"],"description":"输出锐化用途档，作用在缩放到最终尺寸之后；off 时导出像素与预览逐位一致"},"sharpen_strength":{"type":"string","enum":["low","standard","high"],"description":"输出锐化强度档，只乘强度不改半径"},"color_space":{"type":"string","enum":["srgb","p3","rec2020"],"description":"输出色域；静态图嵌对应 ICC，视频写对应 VUI"},"watermark":{"type":"object","additionalProperties":false,"description":"水印整块设定：只写要改的那几键，其余保持当前值","properties":{"enabled":{"type":"boolean","description":"盖章开关；关着导出像素一个不动"},"kind":{"type":"string","enum":["text","image"],"description":"内容种类，两种互斥"},"text":{"type":"string","description":"文字正文，一行或多行；空白等于没内容"},"image":{"type":"string","description":"图片水印路径，带 alpha 就按 alpha 叠；空串表示清空"},"spot":{"type":"string","enum":["top-left","top-center","top-right","middle-left","center","middle-right","bottom-left","bottom-center","bottom-right"],"description":"九宫格位置（屏幕坐标，上为画面顶）"},"opacity":{"type":"integer","minimum":0,"maximum":100,"description":"不透明度百分比；0 等于不盖"},"size":{"type":"integer","minimum":0,"maximum":40,"description":"相对输出长边的百分比：文字是大写高，图片是宽"},"margin":{"type":"integer","minimum":0,"maximum":40,"description":"靠边八格让开的距离，相对输出短边的百分比"},"color":{"type":"array","items":{"type":"integer","minimum":0,"maximum":255},"minItems":3,"maxItems":3,"description":"文字颜色 sRGB 三元组；图片忽略"}}},"width":{"type":"integer","minimum":2,"description":"输出宽（偶数）；与 height 同时给"},"height":{"type":"integer","minimum":2,"description":"输出高（偶数）；与 width 同时给"},"out_dir":{"type":"string","description":"输出目录，不能是序列根或其上级"}}}"#,
        },
        CommandSpec {
            id: "retime.set",
            summary: "读取或改写时间重映射（变速 + 运动模糊）：全程倍速、多点速度曲线、缓入缓出、快门；无参数即查询。",
            params: "enabled?/speed?/keys?（数组 [{at,speed}]）/ease?（对象 {edge,peak,in_at,out_at}）/shutter?（0..1）/reset?（true）；speed、keys、ease 三种写法互斥",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"enabled":{"type":"boolean","description":"变速总开关；关着导出走原来的 1:1 快路，一个像素都不动"},"speed":{"type":"number","exclusiveMinimum":0,"description":"全程一个倍速：2 是两帧压成一帧、0.5 是一帧拆成两帧；与 keys/ease 互斥"},"keys":{"type":"array","minItems":1,"description":"多点速度曲线，按源进度排；同一位置只留第一笔","items":{"type":"object","additionalProperties":false,"required":["at","speed"],"properties":{"at":{"type":"number","minimum":0,"maximum":1,"description":"源进度（0=序列开头，1=结尾），不是输出秒"},"speed":{"type":"number","exclusiveMinimum":0,"description":"该处倍速；要停住写得很小，别写 0"}}}},"ease":{"type":"object","additionalProperties":false,"required":["peak"],"description":"缓入缓出：两头 edge 倍速，中段 peak 倍速，在 in_at 加速到位、out_at 减速回来","properties":{"edge":{"type":"number","exclusiveMinimum":0,"description":"进出头的倍速，默认 1"},"peak":{"type":"number","exclusiveMinimum":0,"description":"中段倍速"},"in_at":{"type":"number","minimum":0,"maximum":1,"description":"加速到位的源进度，默认 0.25"},"out_at":{"type":"number","minimum":0,"maximum":1,"description":"开始减速的源进度，默认 0.75"}}},"shutter":{"type":"number","minimum":0,"maximum":1,"description":"曝光区间占本帧源跨度的比例：0 是锐利插帧，1 是把跨过的源帧全按重叠时长平均（车流变光带）"},"reset":{"type":"boolean","description":"true = 清回默认（关变速、常速、零快门）"}}}"#,
        },
        CommandSpec {
            id: "retime.status",
            summary: "当前变速设定，外加按序列帧数换算的输出帧数、时长与错误（倍速填错在这儿报，不用等导出）。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "motion.set",
            summary: "读取或改写运镜（Ken Burns）：取景框从起点走到终点，配缓动曲线；无参数即查询。",
            params: "enabled?/from?/to?（对象 {x,y,w,h} 或数组 [x,y,w,h]，归一化 0..1）/ease?（smooth|linear|in|out）/swap?（true 起止互换）/refit?（[宽,高] 换画幅比例时把两个框一起重铺）/reset?（true）",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"enabled":{"type":"boolean","description":"运镜总开关；关着或两个框重合时导出走原来的快路，一个像素都不重采样"},"from":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4,"description":"起点取景框 [x, y, 宽, 高]，相对拉直+裁切后那一幅的归一化值（0..1）；越界与退化由内核夹回画面内。也接受 {x,y,w,h} 对象写法"},"to":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4,"description":"终点取景框，写法同 from；默认往里收到 0.85"},"ease":{"type":"string","enum":["smooth","linear","in","out"],"description":"走动节奏：smooth 缓入缓出、linear 匀速、in 越推越快、out 越推越慢"},"swap":{"type":"boolean","description":"true = 起点与终点互换（拉远变推近）"},"refit":{"type":"array","items":{"type":"number","exclusiveMinimum":0},"minItems":2,"maxItems":2,"description":"换画幅比例时把两个框一起重铺 [宽, 高]：按起点框的变化等比例带过去终点框，各自重铺会把运镜幅度归零"},"reset":{"type":"boolean","description":"true = 清回默认（关运镜、整幅起、内收止、缓入缓出）"}}}"#,
        },
        CommandSpec {
            id: "motion.status",
            summary: "当前运镜设定：两个框、缓动档、是否真的在动，外加作用的那一幅尺寸、输出画幅与两端推近倍数。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "align.run",
            summary: "启动序列对齐（长任务，立即返回）；进度用 align.status 查。",
            params: "reference?：参考帧 1 开始；estimator?：template/phase",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"reference":{"type":"integer","minimum":1,"description":"参考帧，从 1 开始"},"estimator":{"type":"string","enum":["template","phase"],"description":"模板匹配或相位相关"}}}"#,
        },
        CommandSpec {
            id: "align.status",
            summary: "对齐进度与最近一次报告摘要。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "export.video",
            summary: "导出帧序列并编码成视频（批量写，必须 confirm: true）；长任务，立即返回。",
            params: "fps?/crf?/codec?/format?（只能 png8/tiff16）/color_space?/copy?/width?+height?/out_dir?/confirm",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"fps":{"type":"number","exclusiveMinimum":0,"maximum":240},"crf":{"type":"integer","minimum":0,"maximum":51},"codec":{"type":"string","enum":["h264","h265"]},"format":{"type":"string","enum":["png8","tiff16"],"description":"中间帧容器；有损档（jpeg/webp）只当最终交付，不能喂 ffmpeg"},"color_space":{"type":"string","enum":["srgb","p3","rec2020"],"description":"输出色域；静态图嵌对应 ICC，视频写对应 VUI"},"copy":{"type":"string","description":"这一次导出改用某套命名调色副本的调色；省略用面板当前调色"},"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1},"out_dir":{"type":"string","description":"输出目录，不能是序列根或其上级"},"confirm":{"type":"boolean","description":"批量写操作必须为 true"}},"required":["confirm"]}"#,
        },
        CommandSpec {
            id: "export.stills",
            summary: "把整个序列导出为静帧序列（批量写，必须 confirm: true）；长任务，立即返回。",
            params: "format?/quality?/color_space?/copy?/width?+height?/out_dir?/confirm",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"format":{"type":"string","enum":["png8","tiff16","jpeg","webp","avif"],"description":"静帧容器；jpeg 与 avif 看 quality，webp 是 VP8L 无损档"},"quality":{"type":"integer","minimum":1,"maximum":100,"description":"压缩质量，只对 jpeg / avif 生效"},"color_space":{"type":"string","enum":["srgb","p3","rec2020"],"description":"输出色域；静态图嵌对应 ICC，视频写对应 VUI"},"copy":{"type":"string","description":"这一次导出改用某套命名调色副本的调色；省略用面板当前调色"},"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1},"out_dir":{"type":"string","description":"输出目录，不能是序列根或其上级"},"confirm":{"type":"boolean","description":"批量写操作必须为 true"}},"required":["confirm"]}"#,
        },
        CommandSpec {
            id: "export.status",
            summary: "导出进度、阶段（渲染帧 / 编码）与最近一次输出。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "export.cancel",
            summary: "请求取消正在进行的导出；已经写下的素材不会被删除。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "hdr.merge",
            summary: "把同场景的多张不同曝光融成一张静帧（Mertens 曝光融合，批量写，必须 confirm: true）；跑过对齐会先套统一裁切与位移，尺寸对不齐会拒绝。长任务，立即返回，进度用 merge.status 查。",
            params: "frames?（帧下标数组，0 开始）或 center?+around?（默认当前帧±1）；out_dir?（默认序列根下 HDR/）；confirm",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"frames":{"type":"array","items":{"type":"integer","minimum":0},"description":"要融合的帧下标（0 开始）；给了就忽略 center/around"},"center":{"type":"integer","minimum":0,"description":"包围中心帧下标（0 开始）；省略用当前选中帧"},"around":{"type":"integer","minimum":1,"maximum":4,"description":"中心前后各取几帧（1..4，最多凑成 9 张）；省略=1"},"out_dir":{"type":"string","description":"输出目录，不能是序列根或其上级；省略用序列根下 HDR/"},"confirm":{"type":"boolean","description":"批量写操作必须为 true"}},"required":["confirm"]}"#,
        },
        CommandSpec {
            id: "pano.merge",
            summary: "把相邻几张有重叠的照片拼成一张宽幅全景（特征点匹配 + RANSAC 单应 + 羽化融合，批量写，必须 confirm: true）；不套对齐位移，靠特征点自己求几何。长任务，立即返回，进度用 merge.status 查。",
            params: "frames?（帧下标数组，0 开始）或 start?+around?（默认当前帧起向后 1 张）；out_dir?（默认序列根下 全景/）；confirm",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"frames":{"type":"array","items":{"type":"integer","minimum":0},"description":"要拼接的帧下标（0 开始，按接片顺序）；给了就忽略 start/around"},"start":{"type":"integer","minimum":0,"description":"起始帧下标（0 开始）；省略用当前选中帧"},"around":{"type":"integer","minimum":1,"maximum":11,"description":"起始帧之后再接几张（1..11，最多凑成 12 张）；省略=1"},"out_dir":{"type":"string","description":"输出目录，不能是序列根或其上级；省略用序列根下 全景/"},"confirm":{"type":"boolean","description":"批量写操作必须为 true"}},"required":["confirm"]}"#,
        },
        CommandSpec {
            id: "merge.status",
            summary: "HDR / 全景合并的进度与最近一次成片：busy、kind、done/total、result（含 path、width、height）、error。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "merge.cancel",
            summary: "请求取消正在进行的 HDR / 全景合并；核心算法是一整块，取消在两帧之间生效，已读进来的帧不会留下半成品。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "queue.list",
            summary: "列出批量渲染队列：每段的源目录、成片路径、规格与状态。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "queue.add",
            summary: "把一段序列排进渲染队列；参数省略时沿用当前序列与成片设置。",
            params: "root?/out?/width?+height?/fps?/codec?/crf?/preset?/keep_frames?/resume?/copy?",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"root":{"type":"string","description":"源帧文件夹；省略用当前序列"},"out":{"type":"string","description":"成片路径（.mp4/.mkv/.mov）；省略按序列名生成"},"width":{"type":"integer","minimum":0},"height":{"type":"integer","minimum":0},"fps":{"type":"number","minimum":1,"maximum":240},"codec":{"type":"string","enum":["h264","h265"]},"crf":{"type":"integer","minimum":0,"maximum":51},"preset":{"type":"string","enum":["ultrafast","superfast","veryfast","faster","fast","medium","slow","slower","veryslow","placebo"]},"keep_frames":{"type":"boolean","description":"保留中间帧目录"},"resume":{"type":"boolean","description":"断点续跑：这一段失败后重排队时，已经写完的中间帧原样留着，只补没写出来的；省略沿用当前输出面板上的续跑开关"},"copy":{"type":"string","description":"该段改用序列工程里某套命名调色副本渲染；省略用工程主调色"}}}"#,
        },
        CommandSpec {
            id: "queue.remove",
            summary: "从队列里移除一段（只改队列，不动任何文件）。",
            params: "index：队列序号，从 1 开始",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"队列序号，从 1 开始"}},"required":["index"]}"#,
        },
        CommandSpec {
            id: "queue.clear",
            summary: "清空渲染队列（不删除已渲出的成片）。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "queue.run",
            summary: "按顺序渲染整个队列（批量写视频，必须 confirm: true）；长任务，立即返回。",
            params: "confirm",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"confirm":{"type":"boolean","description":"批量写操作必须为 true"}},"required":["confirm"]}"#,
        },
        CommandSpec {
            id: "queue.status",
            summary: "队列是否在跑、当前第几段、当前段的渲帧/编码进度与各段状态统计。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "queue.cancel",
            summary: "请求取消队列；已渲完的段保留，未开始的段仍为待渲。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "project.save",
            summary: "把当前参数与去闪烁设置写入 <序列>/.aevum/project.json。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "project.get",
            summary: "返回工程快照与磁盘路径。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "project.metadata",
            summary: "读/写当前序列的说明与版权信息（标题、说明、创作者、版权），改动立刻写进 <序列>/.aevum/project.json；导出时随静帧进 EXIF、随成片进容器标签。不带参数只查询。",
            params: "title? / description? / creator? / copyright?（给空串或 null 就是清空）",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"title":{"type":["string","null"],"description":"标题；写入 EXIF XPTitle（超长自动截断）"},"description":{"type":["string","null"],"description":"说明；写入 EXIF ImageDescription，换行折成空格"},"creator":{"type":["string","null"],"description":"创作者/署名；写入 EXIF Artist"},"copyright":{"type":["string","null"],"description":"版权声明；写入 EXIF Copyright"}}}"#,
        },
        CommandSpec {
            id: "journal.list",
            summary: "读取当前序列的操作日志末尾（只记成功的写操作）。",
            params: "limit：最多返回条数，默认 50",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"limit":{"type":"integer","minimum":1,"maximum":500,"description":"最多返回条数，默认 50"}}}"#,
        },
        CommandSpec {
            id: "history.list",
            summary: "列撤销历史：每步的说明、是否可撤销、当前停在哪一步。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "history.undo",
            summary: "撤销最近一步；回到那之前的状态。没有可撤的改动时报错。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "history.redo",
            summary: "重做一步（撤销之后往回放）。没有可重做的改动时报错。",
            params: "无参数",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
        },
        CommandSpec {
            id: "history.jump",
            summary: "跳到历史里的任意一步（1 开始），等同点历史面板那一行。",
            params: "index：步号，从 1 开始",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"index":{"type":"integer","minimum":1,"description":"第几步，从 1 开始"}},"required":["index"]}"#,
        },
        CommandSpec {
            id: "ui.screenshot",
            summary: "把整个窗口截图写到指定 PNG（异步完成，立即返回 started）。",
            params: "path：输出 .png；覆盖已有文件需 confirm",
            schema: r#"{"type":"object","additionalProperties":false,"properties":{"path":{"type":"string","description":"输出 PNG 路径"},"confirm":{"type":"boolean","description":"覆盖已有文件时必需"}},"required":["path"]}"#,
        },
    ]
}

/// 给 `system.commands` 用的 JSON 形态。
pub fn catalog_json() -> Vec<Value> {
    command_catalog()
        .into_iter()
        .map(|spec| {
            json!({
                "id": spec.id,
                "summary": spec.summary,
                "params": spec.params,
                "schema": spec.schema(),
            })
        })
        .collect()
}

/// 给 AI 面板与文档用的附着命令（同一 exe，不需要第二个程序）。
pub fn attach_command(control_path: &Path) -> String {
    format!(
        "\"{}\" --mcp --attach \"{}\"",
        executable_path(),
        control_path.display()
    )
}

/// MCP 客户端通用的 `mcpServers` 配置片段，整段复制即可粘贴。
pub fn attach_config(control_path: &Path) -> String {
    serde_json::to_string_pretty(&json!({
        "mcpServers": {
            "aevum-studio": {
                "command": executable_path(),
                "args": ["--mcp", "--attach", control_path.display().to_string()],
            }
        }
    }))
    .unwrap_or_else(|_| "{}".to_owned())
}

/// 当前 exe 的完整路径；拿不到就退回文件名，至少命令还能读。
fn executable_path() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "aevum.exe".to_owned())
}

// ---------------------------------------------------------------- 服务端

/// 运行中的控制服务。Drop 时停线程，并只删除自己写下的 control.json。
pub struct ControlServer {
    file: ControlFile,
    path: PathBuf,
    root: PathBuf,
    shutdown: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl ControlServer {
    /// 绑定随机端口、写 control.json、起接受连接的线程。
    pub fn start(root: &Path, executor: Arc<dyn CommandExecutor>) -> Result<Self, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|e| format!("控制服务无法绑定 127.0.0.1：{e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("读取控制端口失败：{e}"))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("控制服务设置非阻塞失败：{e}"))?;

        let file = ControlFile {
            port,
            token: random_token(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            started: unix_secs(),
            root: root.display().to_string(),
        };
        let path = control_path(root);
        let info = file.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();

        let accept = thread::Builder::new()
            .name("aevum-control".to_owned())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let info = info.clone();
                            let executor = executor.clone();
                            // 每个连接一条线程：慢客户端不会拖住 UI 或其它连接。
                            let _ = thread::Builder::new()
                                .name("aevum-control-conn".to_owned())
                                .spawn(move || {
                                    let _ = serve_stream(stream, &info, executor.as_ref());
                                });
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(15));
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| format!("控制服务线程启动失败：{e}"))?;

        if let Err(e) = file.write(&path) {
            shutdown.store(true, Ordering::Relaxed);
            let _ = accept.join();
            return Err(e);
        }

        Ok(Self {
            file,
            path,
            root: root.to_path_buf(),
            shutdown,
            accept: Some(accept),
        })
    }

    pub fn file(&self) -> &ControlFile {
        &self.file
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
        // 只删自己写的那一份：文件和内存里的 pid + 令牌都对得上才动手。
        if let Ok(current) = ControlFile::read(&self.path)
            && current.pid == self.file.pid
            && current.token == self.file.token
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn serve_stream(
    stream: TcpStream,
    info: &ControlFile,
    executor: &dyn CommandExecutor,
) -> io::Result<()> {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    handle_http(&mut reader, &mut writer, &info.token, info, executor)
}

// ---------------------------------------------------------------- HTTP

/// 处理一个 HTTP/1.1 请求。抽成泛型是为了在单测里用 Cursor 驱动整条链路。
pub fn handle_http<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    token: &str,
    info: &ControlFile,
    executor: &dyn CommandExecutor,
) -> io::Result<()> {
    let mut request_line = String::new();
    if read_line_limited(input, &mut request_line, MAX_LINE)? == 0 {
        return Ok(()); // 连上就断，不是错误。
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let target = parts.next().unwrap_or("").to_owned();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return write_message(out, 400, "只支持 HTTP/1.1");
    }
    let path = target.split('?').next().unwrap_or("").to_owned();

    let mut content_length: Option<usize> = None;
    let mut auth: Option<String> = None;
    let mut head_bytes = request_line.len();
    loop {
        let mut line = String::new();
        let n = read_line_limited(input, &mut line, MAX_LINE)?;
        if n == 0 {
            break;
        }
        head_bytes += line.len();
        if head_bytes > MAX_HEAD {
            return write_message(out, 431, "请求头过大");
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return write_message(out, 400, "请求头格式不对");
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                let parsed = value.parse::<usize>().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Content-Length 不是整数")
                })?;
                content_length = Some(parsed);
            }
            "authorization" => auth = Some(value.to_owned()),
            "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                return write_message(out, 400, "不支持分块传输，请带 Content-Length");
            }
            _ => {}
        }
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => write_response(
            out,
            200,
            &json!({
                "ok": true,
                "app": "aevum-studio",
                "version": info.version,
                "pid": info.pid,
                "port": info.port,
            }),
            false,
        ),
        ("POST", "/rpc") => {
            let header_ok = auth.as_deref().is_some_and(|h| bearer_matches(h, token));
            let Some(len) = content_length else {
                return write_message(out, 411, "缺少 Content-Length（需要一个 JSON 请求体）");
            };
            if len > MAX_BODY {
                return write_message(
                    out,
                    413,
                    &format!("请求体 {len} 字节，超过上限 {MAX_BODY} 字节"),
                );
            }
            let mut body = vec![0u8; len];
            if len > 0 {
                input.read_exact(&mut body)?;
            }
            let parsed = if body.is_empty() {
                None
            } else {
                serde_json::from_slice::<Value>(&body).ok()
            };
            // 令牌可以放 Authorization 头，也可以放 JSON 顶层 "token"。
            let token_ok = header_ok
                || parsed
                    .as_ref()
                    .is_some_and(|value| json_token_matches(value, token));
            if !token_ok {
                return write_response(
                    out,
                    401,
                    &json!({"error": {"code": 401, "message": "令牌无效或缺失：请使用 .aevum/control.json 里的 token"}}),
                    true,
                );
            }
            let Some(request) = parsed else {
                return write_response(
                    out,
                    200,
                    &rpc_error(
                        Value::Null,
                        -32700,
                        if body.is_empty() {
                            "请求体为空：需要一个 JSON-RPC 2.0 对象"
                        } else {
                            "请求体不是合法 JSON"
                        },
                    ),
                    false,
                );
            };
            let response = rpc_response(request, info, executor);
            write_response(out, 200, &response, false)
        }
        ("POST", _) => write_message(out, 404, "只支持 POST /rpc"),
        ("GET", _) => write_message(out, 404, "只支持 GET /health"),
        _ => write_message(out, 405, "只支持 GET /health 与 POST /rpc"),
    }
}

/// JSON-RPC 2.0 分发：`system.*` 本地处理，其余按命令 ID 交给执行器。
pub fn rpc_response(request: Value, info: &ControlFile, executor: &dyn CommandExecutor) -> Value {
    let Value::Object(object) = request else {
        return rpc_error(Value::Null, -32600, "请求必须是单个 JSON-RPC 2.0 对象");
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    if let Some(version) = object.get("jsonrpc").and_then(Value::as_str)
        && version != "2.0"
    {
        return rpc_error(id, -32600, "jsonrpc 必须是 \"2.0\"");
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return rpc_error(id, -32600, "缺少 method（要执行的命令 ID）");
    };
    let params = match object.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(value @ Value::Object(_)) => value.clone(),
        Some(_) => return rpc_error(id, -32602, "params 必须是对象"),
    };

    let known = command_catalog().iter().any(|spec| spec.id == method);
    let result = match method {
        "system.info" => Ok(json!({
            "app": "aevum-studio",
            "version": info.version,
            "pid": info.pid,
            "port": info.port,
            "root": info.root,
            "started": info.started,
        })),
        "system.commands" => Ok(json!({"commands": catalog_json()})),
        other if known => executor.execute(other, &params),
        other => return rpc_error(id, -32601, &format!("未知命令：{other}")),
    };
    match result {
        Ok(value) => json!({"jsonrpc": "2.0", "id": id, "result": value}),
        Err(message) => rpc_error(id, -32000, &message),
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn bearer_matches(header: &str, token: &str) -> bool {
    let mut parts = header.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(scheme), Some(value)) if scheme.eq_ignore_ascii_case("bearer") => ct_eq(value, token),
        _ => false,
    }
}

fn json_token_matches(value: &Value, token: &str) -> bool {
    value
        .get("token")
        .and_then(Value::as_str)
        .is_some_and(|given| ct_eq(given, token))
}

/// 定长比较，别因为提前返回把令牌长度/前缀暴露给本机其它进程。
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn read_line_limited<R: BufRead>(input: &mut R, buf: &mut String, max: usize) -> io::Result<usize> {
    // 多读一字节就能发现超长行；Take 保证不会把整个恶意流读进内存。
    let mut limited = input.take(max as u64 + 1);
    let n = limited.read_line(buf)?;
    if n > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "请求行或请求头过长",
        ));
    }
    Ok(n)
}

fn write_message<W: Write>(out: &mut W, status: u16, message: &str) -> io::Result<()> {
    write_response(
        out,
        status,
        &json!({"error": {"code": status as i64, "message": message}}),
        false,
    )
}

fn write_response<W: Write>(
    out: &mut W,
    status: u16,
    body: &Value,
    www_authenticate: bool,
) -> io::Result<()> {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_owned());
    write!(out, "HTTP/1.1 {status} {}\r\n", reason(status))?;
    out.write_all(b"Content-Type: application/json; charset=utf-8\r\n")?;
    write!(out, "Content-Length: {}\r\n", text.len())?;
    if www_authenticate {
        out.write_all(b"WWW-Authenticate: Bearer\r\n")?;
    }
    out.write_all(b"Cache-Control: no-store\r\nConnection: close\r\n\r\n")?;
    out.write_all(text.as_bytes())?;
    out.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    }
}

// ---------------------------------------------------------------- 杂项

/// 随机令牌：系统时间 + 进程号 + `RandomState` 的多轮派生，输出 64 位十六进制。
///
/// 不引 rand：这条通道只在本机、生命周期短，`RandomState` 的种子已由标准库
/// 从系统随机源取过，再混入时间与 pid 足够避免同机猜测。
pub fn random_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut out = String::with_capacity(64);
    for round in 0..4u64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u32(std::process::id());
        hasher.write_u64(round);
        hasher.write_usize(&out as *const String as usize);
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 把 `--shot` 与 `ui.screenshot` 共用的写 PNG 逻辑放在这里，入口只调一处。
pub fn save_png(path: &Path, image: &egui::ColorImage) -> Result<(), String> {
    let [w, h] = image.size;
    let mut buf = Vec::with_capacity(w * h * 4);
    for px in &image.pixels {
        buf.extend_from_slice(&[px.r(), px.g(), px.b(), px.a()]);
    }
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }
    image::save_buffer(path, &buf, w as u32, h as u32, image::ColorType::Rgba8)
        .map_err(|e| e.to_string())
}

/// UI 线程持有一份 Sender，服务端线程持有一份；Sender 不是 Sync，用 Mutex 包一层。
pub type SharedSender<T> = Arc<Mutex<Sender<T>>>;

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Mock;

    impl CommandExecutor for Mock {
        fn execute(&self, id: &str, args: &Value) -> Result<Value, String> {
            match id {
                "grade.get" => Ok(json!({"preset": "原始", "args": args})),
                "grade.set" => Err("未知调色字段：bogus".to_owned()),
                other => Err(format!("未知命令：{other}")),
            }
        }
    }

    fn info() -> ControlFile {
        ControlFile {
            port: 45678,
            token: "secret".to_owned(),
            pid: 42,
            version: "0.1.0".to_owned(),
            started: 1,
            root: "D:/frames".to_owned(),
        }
    }

    fn build_request(auth: Option<&str>, body: &str) -> String {
        let mut req = String::from("POST /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\n");
        if let Some(auth) = auth {
            req.push_str(&format!("Authorization: {auth}\r\n"));
        }
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        ));
        req.push_str(body);
        req
    }

    fn run(request: &str) -> (u16, Value) {
        let mut input = Cursor::new(request.as_bytes().to_vec());
        let mut out = Vec::new();
        handle_http(&mut input, &mut out, "secret", &info(), &Mock).expect("处理请求");
        let text = String::from_utf8(out).expect("响应是 UTF-8");
        let (head, body) = text.split_once("\r\n\r\n").expect("响应缺空行");
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("状态行");
        (status, serde_json::from_str(body).expect("响应体是 JSON"))
    }

    #[test]
    fn bearer_token_is_accepted() {
        let body = r#"{"jsonrpc":"2.0","id":7,"method":"grade.get","params":{}}"#;
        let (status, value) = run(&build_request(Some("Bearer secret"), body));
        assert_eq!(status, 200);
        assert_eq!(value["id"], json!(7));
        assert_eq!(value["result"]["preset"], json!("原始"));
    }

    #[test]
    fn token_inside_body_is_accepted() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"grade.get","params":{},"token":"secret"}"#;
        let (status, value) = run(&build_request(None, body));
        assert_eq!(status, 200);
        assert_eq!(value["result"]["preset"], json!("原始"));
    }

    #[test]
    fn missing_or_wrong_token_is_unauthorized() {
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"grade.get","params":{}}"#;
        let (status, value) = run(&build_request(None, body));
        assert_eq!(status, 401);
        assert!(value["error"]["message"].as_str().unwrap().contains("令牌"));

        let (status, _) = run(&build_request(Some("Bearer nope"), body));
        assert_eq!(status, 401);
    }

    #[test]
    fn command_error_is_jsonrpc_error() {
        let body = r#"{"jsonrpc":"2.0","id":2,"method":"grade.set","params":{"bogus":1}}"#;
        let (status, value) = run(&build_request(Some("Bearer secret"), body));
        assert_eq!(status, 200);
        assert_eq!(value["error"]["code"], json!(-32000));
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("bogus")
        );
    }

    #[test]
    fn unknown_command_is_rejected() {
        let body = r#"{"jsonrpc":"2.0","id":3,"method":"no.such","params":{}}"#;
        let (_, value) = run(&build_request(Some("Bearer secret"), body));
        assert_eq!(value["error"]["code"], json!(-32601));
    }

    #[test]
    fn oversized_body_is_rejected_before_reading() {
        let headers = format!(
            "POST /rpc HTTP/1.1\r\nAuthorization: Bearer secret\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        let (status, value) = run(&headers);
        assert_eq!(status, 413);
        assert!(value["error"]["message"].as_str().unwrap().contains("上限"));
    }

    #[test]
    fn health_needs_no_token() {
        let request = "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        let (status, value) = run(request);
        assert_eq!(status, 200);
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["port"], json!(45678));
    }

    #[test]
    fn system_commands_lists_the_catalog() {
        let body = r#"{"jsonrpc":"2.0","id":4,"method":"system.commands","params":{}}"#;
        let (_, value) = run(&build_request(Some("Bearer secret"), body));
        let commands = value["result"]["commands"].as_array().expect("命令数组");
        assert!(commands.iter().any(|c| c["id"] == json!("grade.set")));
        assert!(commands.iter().all(|c| c["schema"].is_object()));
    }

    #[test]
    fn token_is_hex_and_not_repeated() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn catalog_ids_are_unique() {
        let mut ids: Vec<&str> = command_catalog().iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "命令 ID 不能重复");
    }

    /// 命令目录与 `aevum_core::mcp_routes::COMMANDS` 必须逐条一致。
    ///
    /// 那张表是 core 侧的 ID 副本，附着模式的工具名生成与路由全靠它。
    /// 副本漂一条，就是「GUI 能执行、MCP 却路由不到」或者反过来——
    /// 而这种错在运行时会表现成「未知工具」，没人会想到是两张表对不上。
    /// 只比集合不比顺序：顺序是给人读的，不影响路由。
    #[test]
    fn catalog_matches_core_command_table() {
        use std::collections::BTreeSet;
        let actual: BTreeSet<&str> = command_catalog().iter().map(|spec| spec.id).collect();
        let expected: BTreeSet<&str> = aevum_core::mcp_routes::COMMANDS.iter().copied().collect();
        let missing: Vec<&str> = expected.difference(&actual).copied().collect();
        let extra: Vec<&str> = actual.difference(&expected).copied().collect();
        assert!(missing.is_empty(), "命令目录缺这些 ID：{missing:?}");
        assert!(extra.is_empty(), "命令目录多出这些 ID：{extra:?}");
    }

    #[test]
    fn attach_command_and_config_mention_the_control_file() {
        let path = Path::new("D:/frames/.aevum/control.json");
        let command = attach_command(path);
        assert!(command.contains("--mcp --attach"), "{command}");
        assert!(command.contains("control.json"), "{command}");

        let config: Value = serde_json::from_str(&attach_config(path)).expect("配置片段是 JSON");
        let args = config["mcpServers"]["aevum-studio"]["args"]
            .as_array()
            .expect("args 数组");
        assert!(args.iter().any(|arg| arg == &json!("--attach")));
        assert!(
            args.iter()
                .any(|arg| arg.as_str().is_some_and(|s| s.ends_with("control.json")))
        );
    }

    #[test]
    fn control_file_roundtrip_and_cleanup() {
        let dir = temp_dir("file");
        fs::write(dir.join("keep.txt"), b"keep").unwrap();
        let server = ControlServer::start(&dir, Arc::new(Mock)).expect("启动控制服务");
        let path = server.path().to_path_buf();
        assert_eq!(path, control_path(&dir));

        let read = ControlFile::read(&path).expect("读控制文件");
        assert_eq!(read.token, server.file().token);
        assert!(read.port > 0);
        assert_eq!(read.pid, std::process::id());
        assert!(read.root.contains("aevum-control-test"));

        drop(server);
        assert!(!path.exists(), "退出时应删除 control.json");
        assert!(dir.join("keep.txt").exists(), "不能删除别的文件");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn drop_keeps_a_foreign_control_file() {
        let dir = temp_dir("foreign");
        let path = control_path(&dir);
        let foreign = ControlFile {
            port: 1,
            token: "someone-else".to_owned(),
            pid: 999,
            version: "0.1.0".to_owned(),
            started: 2,
            root: dir.display().to_string(),
        };
        let server = ControlServer::start(&dir, Arc::new(Mock)).expect("启动控制服务");
        // 模拟另一个实例后来覆盖了同路径文件。
        foreign.write(&path).unwrap();
        drop(server);
        assert!(path.exists(), "令牌/path 对不上时不能删别人的文件");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loopback_roundtrip_over_tcp() {
        let dir = temp_dir("tcp");
        let server = ControlServer::start(&dir, Arc::new(Mock)).expect("启动控制服务");
        let file = server.file().clone();
        let body = r#"{"jsonrpc":"2.0","id":9,"method":"grade.get","params":{}}"#;
        let request = build_request(Some(&format!("Bearer {}", file.token)), body);

        let mut stream =
            TcpStream::connect((Ipv4Addr::LOCALHOST, file.port)).expect("连接控制端口");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("读响应");
        let (head, body) = response.split_once("\r\n\r\n").expect("响应缺空行");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let value: Value = serde_json::from_str(body).expect("JSON");
        assert_eq!(value["result"]["preset"], json!("原始"));

        drop(server);
        let _ = fs::remove_dir_all(&dir);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aevum-control-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }
}
