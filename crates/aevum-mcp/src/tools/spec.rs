//! 工具清单与参数 schema：`tools/list` 的数据源。
//!
//! 边界：只有 `build_commands` 和它用到的 schema 片段（`object` / `retime_schema` /
//! `motion_schema`）。改这里等于改 agent 看到的 API 面：`tests/suite/tools_list_snapshot.rs`
//! 把整份清单逐字节冻住，不是有意的改动一律红。工具实现在各自的功能域模块里，
//! 这个文件不许出现任何业务逻辑——清单与实现分开，才能一眼看出「词」和「事」对不上。
//!
//! The tool catalogue and its argument schemas: the data source for `tools/list`.
//!
//! Boundary: only `build_commands` plus the schema fragments it uses (`object`,
//! `retime_schema`, `motion_schema`). Editing this file edits the API surface an agent sees:
//! `tests/suite/tools_list_snapshot.rs` freezes the whole catalogue byte for byte, and any
//! change that is not deliberate goes red. The implementations live in the domain modules;
//! no business logic is allowed here -- keeping the words apart from the deeds is what makes
//! a name that no longer matches its behaviour visible at a glance.
//!
//! # 每个条目都要带 `hints`
//!
//! `hints` 直接变成 MCP 的 `annotations`，宿主靠它在**调用前**提示用户「这是只读」还是
//! 「这会覆盖文件」。判定口径写在 [`crate::Hints`] 的文档里，这里只强调两条容易走偏的：
//!
//! - **改内存参数不算破坏性**。`aevum_grade_set` 覆盖的是可撤销的参数，不是用户数据；
//!   `aevum_xmp_write` 覆盖的是用户指定的、可能被别的序列或别的软件共用的文件——后者
//!   才是 `DESTRUCTIVE`。分界线是「这个路径是不是本序列自己的账本」。
//! - **混合 op 的工具取最坏情况**。`aevum_grade_copy` 的 `op=delete` 是删除、
//!   `aevum_retouch` 的 `op=add` 是追加，都按最危险的那一种标注；宿主宁可多提示一次。
//!
//! # Every entry carries `hints`
//!
//! `hints` becomes MCP's `annotations`, which is how a host warns the user **before** a
//! call whether a tool is read-only or overwrites files. The judgement rules live in
//! [`crate::Hints`]; two easy-to-get-wrong ones are worth repeating:
//!
//! - **In-memory parameter changes are not destructive.** `aevum_grade_set` overwrites
//!   reversible parameters, not user data; `aevum_xmp_write` overwrites a file the user
//!   named, which another sequence or another app may share -- that is `DESTRUCTIVE`. The
//!   line is whether the path belongs to this sequence's own bookkeeping.
//! - **Tools with mixed ops take the worst case.** `aevum_grade_copy` has `op=delete` and
//!   `aevum_retouch` has `op=add`; both are annotated as the more dangerous of their modes.
//!   A host would rather warn once too often.

use serde_json::{Value, json};

use crate::{Command, Hints};

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// `retime` 参数的 schema：变速 + 运动模糊，两种导出共用同一套口径。
fn retime_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "description": "时间重映射（变速 + 运动模糊）；省略就是 1:1 原速，导出像素与不开时逐位一致",
        "properties": {
            "enabled": {"type": "boolean", "default": true, "description": "关掉等于不做变速"},
            "speed": {"type": "number", "exclusiveMinimum": 0, "description": "全程一个倍速：2 是两帧压一帧，0.5 是一帧拆两帧；与 keys/ease 互斥"},
            "keys": {"type": "array", "minItems": 1, "description": "多点速度曲线，键之间线性", "items": {"type": "object", "additionalProperties": false, "required": ["at", "speed"], "properties": {"at": {"type": "number", "minimum": 0, "maximum": 1, "description": "源进度（0=序列开头，1=结尾），不是输出秒"}, "speed": {"type": "number", "exclusiveMinimum": 0, "description": "该处倍速；要停住写得很小，别写 0"}}}},
            "ease": {"type": "object", "additionalProperties": false, "required": ["peak"], "description": "缓入缓出：两头 edge 倍速，中段 peak 倍速", "properties": {"edge": {"type": "number", "exclusiveMinimum": 0, "default": 1}, "peak": {"type": "number", "exclusiveMinimum": 0}, "in_at": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.25}, "out_at": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.75}}},
            "shutter": {"type": "number", "minimum": 0, "maximum": 1, "default": 0, "description": "曝光区间占本帧源跨度的比例：0 只交叉淡化相邻两帧，1 把跨过的源帧按重叠时长全平均（车流变光带）；不做光流补帧"}
        }
    })
}

fn motion_schema() -> Value {
    json!({
        "type": "object",
        "description": "运镜（Ken Burns）：取景框从起点走到终点。省略就沿用会话工程里那一份；工程里也没有就是不开，导出像素与不开时逐位一致。",
        "additionalProperties": false,
        "properties": {
            "enabled": {"type": "boolean", "description": "总开关；关着、或两个框重合，导出一像素都不重采样"},
            "from": {"type": "array", "minItems": 4, "maxItems": 4, "items": {"type": "number"}, "description": "起点框 [x, y, 宽, 高]，相对拉直+裁切后那一幅归一化 0..1；也接受 {x,y,w,h}。越界由内核夹回画面内"},
            "to": {"type": "array", "minItems": 4, "maxItems": 4, "items": {"type": "number"}, "description": "终点框，写法同 from"},
            "ease": {"type": "string", "enum": ["smooth", "linear", "in", "out"], "description": "走动节奏：smooth 缓入缓出、linear 匀速、in 越推越快、out 越推越慢"},
            "swap": {"type": "boolean", "description": "true = 起点与终点互换"},
            "refit": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number", "exclusiveMinimum": 0}, "description": "[宽, 高] 新画幅跨度：按起点框的变化把终点框等比例带过去"},
            "reset": {"type": "boolean", "description": "true = 整块清回默认（关运镜、整幅起、内收止、缓入缓出）"}
        }
    })
}

pub(super) fn build_commands() -> Vec<Command> {
    vec![
        Command {
            id: "aevum_open_sequence",
            hints: Hints::WRITE,
            description: "打开一个帧文件夹作为当前序列；已有 .aevum/project.json 时自动恢复参数。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "帧文件夹路径（绝对或相对）"}
                }),
                &["root"],
            ),
        },
        Command {
            id: "aevum_sequence_info",
            hints: Hints::READ,
            description: "返回当前序列的根目录、帧数、调色参数、去闪烁设置与最近输出。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_measure",
            hints: Hints::WRITE,
            description: "逐帧测光（均值/中值/中心/锐度），返回统计与降采样序列；先复用会话内存与盘上 `<序列根>/.aevum/measures.json`（与 GUI 同一本账），refresh 可强制重算。",
            input_schema: object(
                json!({
                    "refresh": {"type": "boolean", "default": false, "description": "true 时忽略会话内存与盘上账上的测光结果，重新逐帧测量（测出来的数仍写回账）"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台测光并立即返回，用 aevum_align_status 查进度"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_deflicker_analyze",
            hints: Hints::WRITE,
            description: "分析曝光闪烁并保存逐帧 EV 补偿；之后预览与导出会自动叠加。channels 为 true 时按 R/G/B 三通道分别校正，压的是彩色闪烁。align_exif 为 true 时先用 EXIF 的 ISO/快门/光圈把相机补偿剥掉（日转夜/圣杯延时），只压真正的测光误差。",
            input_schema: object(
                json!({
                    "window": {"type": "number", "minimum": 3, "maximum": 199, "description": "平滑窗口（奇数，默认 15）"},
                    "strength": {"type": "number", "minimum": 0, "maximum": 100, "description": "校正强度百分比（默认 100）"},
                    "multi": {"type": "boolean", "description": "长序列多遍窗口（默认 false）"},
                    "channels": {"type": "boolean", "description": "分通道（RGB）去闪烁，治色闪；默认 false 只补亮度"},
                    "align_exif": {"type": "boolean", "description": "按 EXIF 曝光三要素预对齐：日转夜延时里相机在不停补偿，不对齐的话去闪会把日夜过渡当闪烁压掉。缺三要素的帧不对齐也不猜；一个都没有时这一项等于没开"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_grade_get",
            hints: Hints::READ,
            description: "读取当前调色参数与预设名。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_grade_set",
            hints: Hints::WRITE,
            description: "按字段合并调色参数，例如 {\"exposure\":0.5,\"contrast\":12}；未知字段会报错。",
            input_schema: object(
                json!({
                    "params": {
                        "type": "object",
                        "description": "GradeParams 的部分字段：exposure/contrast/highlights/shadows/whites/blacks/temperature/tint/vibrance/saturation/clarity/texture/dehaze/grain/vignette/曲线与 HSL 等",
                        "additionalProperties": true
                    }
                }),
                &["params"],
            ),
        },
        Command {
            id: "aevum_grade_preset",
            hints: Hints::WRITE,
            description: "应用一个内置调色预设（原始、鲜艳风光、落日暖调、冷色电影等）。",
            input_schema: object(
                json!({
                    "name": {"type": "string", "description": "预设名：原始/鲜艳风光/落日暖调/冷色电影/复古胶片/黑白对比/夜景增强/通透风光/柔和人像/青橙电影"}
                }),
                &["name"],
            ),
        },
        Command {
            id: "aevum_grade_reset",
            hints: Hints::WRITE,
            description: "把调色参数恢复为默认值（预设「原始」）。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_grade_copy",
            hints: Hints::WRITE_APPEND,
            description: "命名调色副本：同一序列多套调色并存。op=list/save/apply/delete/rename（省=list）；save 把当前调色+关键帧存成 name（同名需 overwrite:true），apply 取回当前；导出与队列可按副本名指定用哪一套。",
            input_schema: object(
                json!({
                    "op": {"type": "string", "enum": ["list", "save", "apply", "delete", "rename"], "description": "省略即 list"},
                    "name": {"type": "string", "description": "副本名（save/apply/delete 必填，rename 填旧名）"},
                    "overwrite": {"type": "boolean", "description": "save 同名覆盖；默认报错"},
                    "to": {"type": "string", "description": "rename 的新名字"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_keyframe_add",
            hints: Hints::WRITE,
            description: "在第 frame 帧钉一个关键帧；不传 params 时用当前基准参数，传了则在其上做字段合并。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "帧号"},
                    "params": {
                        "type": "object",
                        "description": "可选的部分调色字段，与 aevum_grade_set 的 params 同口径",
                        "additionalProperties": true
                    }
                }),
                &["frame"],
            ),
        },
        Command {
            id: "aevum_keyframe_remove",
            hints: Hints::WRITE,
            description: "删除第 frame 帧的关键帧；该帧没有关键帧时返回结构化错误。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "帧号"}
                }),
                &["frame"],
            ),
        },
        Command {
            id: "aevum_keyframe_list",
            hints: Hints::READ,
            description: "列出全部关键帧（帧号与参数）；可选 frame 顺便返回该帧插值后的参数。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "可选：查询该帧插值后的参数"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_retouch",
            hints: Hints::WRITE_APPEND,
            description: "修复画笔与仿制图章的笔触表：op=list/add/undo/clear（省=list）。坐标归一化（x 相对宽、y 相对高，r 与 source 相对长边），因为同一份参数要在预览与导出上落到同一块区域；按落盘顺序依次作用，后一笔看得到前一笔的结果。",
            input_schema: object(
                json!({
                    "op": {"type": "string", "enum": ["list", "add", "undo", "clear"], "description": "省略即 list"},
                    "spots": {
                        "type": "array",
                        "description": "op=add 时的笔触（可一次给多笔）",
                        "items": {
                            "type": "object",
                            "properties": {
                                "x": {"type": "number", "minimum": 0.0, "maximum": 1.0, "description": "中心横坐标，归一化"},
                                "y": {"type": "number", "minimum": 0.0, "maximum": 1.0, "description": "中心纵坐标，归一化"},
                                "r": {"type": "number", "exclusiveMinimum": 0.0, "maximum": 0.1, "description": "半径，相对长边"},
                                "feather": {"type": "number", "minimum": 0.0, "maximum": 1.0, "description": "羽化占半径的比例，默认 0.35"},
                                "source": {
                                    "type": "array",
                                    "description": "仿制源偏移 [dx, dy]（相对长边）；省略即修复（斑内解拉普拉斯）",
                                    "items": {"type": "number"},
                                    "minItems": 2,
                                    "maxItems": 2
                                }
                            },
                            "required": ["x", "y", "r"],
                            "additionalProperties": false
                        }
                    },
                    "count": {"type": "integer", "minimum": 1, "description": "op=undo 撤几笔，默认 1"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_upright",
            hints: Hints::WRITE,
            description: "透视校正（梯形）：op=get/set/auto/clear（省=get）。auto 用当前帧估竖直主直线并反推四边形，只修上下透视、不修旋转与畸变，证据不足会明确报错而不是硬猜；校正后四边形之外的一圈被裁掉。",
            input_schema: object(
                json!({
                    "op": {"type": "string", "enum": ["get", "set", "auto", "clear"], "description": "省略即 get"},
                    "quad": {
                        "type": "array",
                        "description": "op=set：四个 [x, y] 角点，左上→右上→右下→左下，归一化",
                        "items": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2},
                        "minItems": 4,
                        "maxItems": 4
                    },
                    "frame": {"type": "integer", "minimum": 0, "description": "op=auto：用哪一帧估消失点，默认 0"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_render_preview",
            hints: Hints::WRITE,
            description: "渲染一帧带当前调色、关键帧插值与去闪烁补偿的预览 PNG，返回落盘路径与尺寸。",
            input_schema: object(
                json!({
                    "frame": {"type": "integer", "minimum": 0, "description": "帧号，默认 0"},
                    "side": {"type": "integer", "minimum": 1, "description": "预览长边像素，默认 1280；超过 4096 需 confirm"},
                    "out": {"type": "string", "description": "输出 PNG 路径；已有文件需 confirm"},
                    "confirm": {"type": "boolean", "description": "覆盖已有文件或 side > 4096 时必需"}
                }),
                &["out"],
            ),
        },
        Command {
            id: "aevum_export_stills",
            hints: Hints::DESTRUCTIVE,
            description: "把整个序列按当前调色、关键帧插值、去闪烁补偿与对齐结果导出为静帧序列（PNG/TIFF/JPEG/WebP/AVIF，批量写，需 confirm）；可选 retime 变速、运动模糊与运镜（Ken Burns）。",
            input_schema: object(
                json!({
                    "out_dir": {"type": "string", "description": "输出帧目录（不能是序列根或其上级）"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8", "description": "AVIF 走随包 ffmpeg 的 libaom-av1，色域写 nclx 标记、不带 ICC/EXIF，逐帧起进程会比较慢"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "每张静帧旁边写一个同名 .xmp 旁车：dc: 的说明与版权四项（取自会话工程的元数据）+ crs: 的这一张调色参数"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：输出目录里已经写完的帧（连带该带的 .xmp）原样留着，这一趟只补没写出来的那些；上次跑到一半断了、这次接着导就用它，改了调色想重导就别开"},
                    "copy": {"type": "string", "description": "改用某套命名调色副本的调色导出；省略用会话当前调色"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时像素真转换并嵌对应 ICC（p3=Display P3，rec2020=BT.2020 SDR）"},
                    "long_side": {"type": "integer", "minimum": 1, "description": "按长边缩放；与 width/height 二选一"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；cover 放大后居中裁切）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给）"},
                    "retime": retime_schema(),
                    "motion": motion_schema(),
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台导出并立即返回，用 aevum_align_status 查进度"}
                }),
                &["out_dir"],
            ),
        },
        Command {
            id: "aevum_export_video",
            hints: Hints::DESTRUCTIVE,
            description: "导出帧序列并用 ffmpeg 编码成 H.264/H.265 视频；自动套用关键帧插值、去闪烁与对齐裁切，可选 retime 变速、运动模糊与运镜（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "out_dir": {"type": "string", "description": "中间帧目录（不能是序列根或其上级）"},
                    "video_out": {"type": "string", "description": "成片文件路径，如 D:/out/clip.mp4"},
                    "codec": {"type": "string", "enum": ["h265", "h264"], "default": "h265"},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 51, "default": 18},
                    "preset": {"type": "string", "default": "medium", "description": "x264/x265 速度档：ultrafast…veryslow"},
                    "fps": {"type": "number", "exclusiveMinimum": 0, "maximum": 240, "default": 24},
                    "long_side": {"type": "integer", "minimum": 1, "description": "按长边缩放；与 width/height 二选一"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；cover 放大后居中裁切）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给）"},
                    "audio": {"type": "string", "description": "可选配乐文件；序列本身没有原声，因此是替换式混入"},
                    "delay": {"type": "number", "minimum": 0, "maximum": 3600, "description": "配乐延后进入（秒）"},
                    "gain": {"type": "number", "minimum": 0, "maximum": 400, "description": "配乐音量百分比，100 为原始"},
                    "fade_in": {"type": "number", "minimum": 0, "maximum": 3600, "description": "淡入时长（秒）"},
                    "fade_out": {"type": "number", "minimum": 0, "maximum": 3600, "description": "淡出时长（秒）"},
                    "loop": {"type": "boolean", "default": false, "description": "配乐短于画面时循环铺满"},
                    "trim": {"type": "boolean", "default": true, "description": "配乐长于画面时截到画面结束"},
                    "acodec": {"type": "string", "enum": ["auto", "copy", "aac128", "aac192", "aac256", "aac320"], "default": "auto", "description": "配乐编码；自动模式能复制就复制，否则 AAC 192k"},
                    "keep_frames": {"type": "boolean", "default": false, "description": "编码后保留中间帧"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：输出目录里已经写完的帧原样留着，这一趟只补没写出来的那些（改了参数想重导就别开）"},
                    "copy": {"type": "string", "description": "改用某套命名调色副本的调色导出；省略用会话当前调色"},
                    "format": {"type": "string", "enum": ["png8", "tiff16"], "default": "png8"},
                    "delivery": {"type": "string", "description": "平台交付预设 key（如 douyin-vertical / bilibili-1080p）：一次填好画幅、帧率、编码器与质量档。显式给的 fps/crf/codec/preset/width/height/long_side 仍会覆盖它。可用 key 见 aevum_sequence_info 的 delivery 列表；预设表在配置目录的 delivery-presets.toml，改表不用改代码"},
                    "retime": retime_schema(),
                    "motion": motion_schema(),
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围；成片目前只出 SDR，传 hdr-pq 会直接报错（HDR 视频编码还没做），静帧的 HDR 走 export_stills + avif"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；决定 VUI 的 primaries/transfer/matrix 与中间帧的 ICC，rec2020 的传递函数在 VUI 里只能标 bt709"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台导出并立即返回，用 aevum_align_status 查进度"}
                }),
                &["out_dir", "video_out"],
            ),
        },
        Command {
            id: "aevum_hdr_merge",
            hints: Hints::DESTRUCTIVE,
            description: "把同场景多张不同曝光融成一张静帧（Mertens 曝光融合）；自动套用对齐的统一裁切与逐帧位移，尺寸对不齐会拒绝（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要融合的帧下标（0 开始）；给了就忽略 center/around"},
                    "center": {"type": "integer", "minimum": 0, "description": "包围中心帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 4, "default": 1, "description": "中心前后各取几帧（1..4，最多凑成 9 张）"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 HDR/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_focus_merge",
            hints: Hints::DESTRUCTIVE,
            description: "把一组不同对焦距离的帧融成一张全景深（焦点合成：深度图法逐像素取测度最强的帧、无细节区兜底参考帧、过渡带羽化；加权平均法按测度四次方混合）；自动套用对齐的统一裁切与逐帧位移，尺寸对不齐会拒绝（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "参与合成的帧下标（0 开始）；给了就忽略 start/around"},
                    "start": {"type": "integer", "minimum": 0, "description": "起始帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": (aevum_core::focus::MAX_FRAMES - 1), "default": 7, "description": "起始帧之后再取几帧（1..31）；省略=7（共 8 帧）"},
                    "method": {"type": "string", "enum": ["depthmap", "weighted", "pyramid"], "default": "depthmap", "description": "depthmap 逐像素选测度最强的帧（无细节区兜底参考帧）；weighted 按测度四次方加权混合；pyramid 拉普拉斯塔按层融合（多尺度自带过渡，feather 与 depth-preview 不适用）"},
                    "align": {"type": "boolean", "default": false, "description": "合成前自动对齐：先估每帧 8×6 网格位移再逐块补偿，压住对焦呼吸的轻微缩放。默认关闭；收益不足 0.5% 自动回退成不做（成片与关闭时逐点一致）；估计失败照样出图，只是按未对齐几何"},
                    "radius": {"type": "integer", "minimum": 0, "maximum": 32, "default": 2, "description": "焦点测度的分析半径（px），0 = 逐像素原始响应"},
                    "threshold": {"type": "number", "minimum": 0, "maximum": 100, "default": 0, "description": "无细节阈值（全图最佳测度的百分位，0..100）：低于这条线的像素整块判给参考帧；0 = 关闭兜底"},
                    "smooth_z": {"type": "integer", "minimum": 0, "maximum": 16, "default": 1, "description": "深度图去斑半径（px，模值滤波），只对 depthmap 生效"},
                    "feather": {"type": "number", "minimum": 0, "maximum": 64, "default": 2, "description": "过渡羽化半径（px），小于 0.5 视为硬切，只对 depthmap 生效"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 焦点合成/"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台合成并立即返回，用 aevum_align_status 查进度，aevum_cancel 取消；长任务期间 status / cancel 都能应答"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_pano_merge",
            hints: Hints::DESTRUCTIVE,
            description: "把相邻几张有重叠的照片拼成一张宽幅全景（特征点匹配 + RANSAC 单应 + 羽化融合）；不套对齐位移，靠特征点自己求几何（批量写，需 confirm）。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要拼接的帧下标（0 开始，按接片顺序）；给了就忽略 start/around"},
                    "start": {"type": "integer", "minimum": 0, "description": "起始帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 11, "default": 1, "description": "起始帧之后再接几张（1..11，最多凑成 12 张）"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 全景/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_stack_merge",
            hints: Hints::DESTRUCTIVE,
            description: "把同机位的一段帧按统计量合成一张：星轨取最亮、降噪取平均、去卫星取中值、杂散光与热噪点做 σ 截尾（批量写，需 confirm）；自动套用对齐的统一裁切与逐帧位移，尺寸对不齐会拒绝。无头会话没有「当前帧」，省 center 就取序列中间那段。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要堆栈的帧下标（0 开始）；给了就忽略 center/around"},
                    "center": {"type": "integer", "minimum": 0, "description": "包围中心帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 32, "default": 4, "description": "中心前后各取几帧（1..32）；省略=4（共 9 帧）"},
                    "mode": {"type": "string", "enum": ["lighten", "average", "median", "sigma-clip"], "default": "lighten", "description": "统计量：星轨用 lighten、降噪用 average、去卫星用 median、杂散光用 sigma-clip"},
                    "clip_sigma": {"type": "number", "exclusiveMinimum": 0, "maximum": 10, "default": 3, "description": "σ 截尾的倍数，常用 2–4；只对 sigma-clip 生效"},
                    "fill_gaps": {"type": "boolean", "default": true, "description": "合成后仍全黑的像素用 3×3 邻域补上，闭合星轨之间的空隙"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 堆栈/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_timeblend_merge",
            hints: Hints::DESTRUCTIVE,
            description: "把不同时刻的帧合成为一张：天地合成（天取末帧、地取首帧，可自动推地平线）或时间切片（横向分带，一带一帧、从早到晚铺满整幅），羽化区里的移动物可自动去鬼影（批量写，需 confirm）；自动套用对齐的统一裁切与逐帧位移，帧号升序即时间顺序。无头会话没有「当前帧」，省 start 就从序列中位帧起取一段。",
            input_schema: object(
                json!({
                    "frames": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "参与合成的帧下标（0 开始，升序即时间顺序）；给了就忽略 start/around"},
                    "start": {"type": "integer", "minimum": 0, "description": "起始帧下标（0 开始）；省略取序列中间帧"},
                    "around": {"type": "integer", "minimum": 1, "maximum": 31, "default": 7, "description": "起始帧之后再取几帧（1..31）；省略=7（共 8 帧）"},
                    "layout": {"type": "string", "enum": ["sky-ground", "bands"], "default": "sky-ground", "description": "sky-ground 按地平线分天地；bands 横向分带、一带一帧"},
                    "bands": {"type": "integer", "minimum": 1, "maximum": 32, "default": 8, "description": "带数，只对 bands 生效；等于帧数时一带一帧"},
                    "feather": {"type": "number", "minimum": 0, "maximum": 0.5, "default": 0.04, "description": "羽化宽度占画面高度的比例，0.04 约等于 4%"},
                    "horizon": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.5, "description": "地平线位置（从顶部算起的比例），只对 sky-ground 生效；horizon_auto 为 true 时忽略"},
                    "horizon_auto": {"type": "boolean", "default": false, "description": "按行亮度梯度自动推地平线（找不到退回 horizon）"},
                    "deghost": {"type": "number", "minimum": 0, "maximum": 4, "default": 0.5, "description": "去鬼影阈值（EV）：某帧某块与自己 3×3 空间中位数偏离超过它就按标签衰减权重；0 = 关闭"},
                    "match_exposure": {"type": "boolean", "default": true, "description": "混之前把来源帧亮度对齐到参考帧（钳 ±2 EV），否则接缝处是一道台阶"},
                    "out_dir": {"type": "string", "description": "输出目录（不能是序列根或其上级）；省略用序列根下 时间合成/"},
                    "format": {"type": "string", "enum": ["png8", "tiff16", "jpeg", "webp", "avif"], "default": "png8"},
                    "quality": {"type": "integer", "minimum": 1, "maximum": 100, "description": "压缩质量，只对 jpeg / avif 生效"},
                    "dynamic": {"type": "string", "enum": ["sdr", "hdr-pq"], "default": "sdr", "description": "输出动态范围：sdr=标准，hdr-pq=PQ（SMPTE ST 2084）+ BT.2020 + 10bit，参考白按 203 nits 落地、>1 的高光沿曲线尾部继续上升；hdr-pq 目前只有 avif 装得下，色域固定 rec2020（传别的会报错）"},
                    "color_space": {"type": "string", "enum": ["srgb", "p3", "rec2020"], "default": "srgb", "description": "输出色域；非 sRGB 时嵌对应 ICC"},
                    "xmp_sidecar": {"type": "boolean", "default": false, "description": "在成片旁写同名 .xmp（dc 四项 + crs 调色）"},
                    "keep_exif": {"type": "boolean", "default": false, "description": "把首帧的 EXIF 块搬进成片"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_cull_evaluate",
            hints: Hints::READ,
            description: "按四条判据逐帧裁决并给出理由：曝光越界（EV）、与相邻帧的突变、清晰度低于序列中位的比例、读不出来；立即返回。测光与 GUI 同一条路径（512 长边）：先读盘上 `<序列根>/.aevum/measures.json`，没有的帧才解码，测完按同一格式写回。所以 GUI 已经测过的序列，无头这边直接出裁决、不再解一遍图，反之亦然；缓存读写失败会静默退化成现测，不影响裁决。账的键是 `(mtime, 字节数)`：文件内容变了但这两项没变（复制工具保留时间戳覆盖同名文件）时，用 `aevum_measure --refresh` 强制重算再评估。只评估不标记：GUI 侧的 apply（把判废帧标成「排除」）是会话态，无头剔除走 aevum_cull_extract 的 exclude 参数。",
            input_schema: object(
                json!({
                    "suggest": {"type": "boolean", "description": "用序列自身的亮度中位数绝对偏差推曝光区间（3×1.4826×MAD，下限 0.5 EV）；给了则覆盖 ev_low/ev_high"},
                    "ev_low": {"type": "number", "minimum": -10, "maximum": 0, "description": "曝光下限（EV，相对 18% 中灰）；省略=-1"},
                    "ev_high": {"type": "number", "minimum": 0, "maximum": 10, "description": "曝光上限（EV）；省略=1"},
                    "jump_ev": {"type": "number", "minimum": 0, "maximum": 10, "description": "与相邻帧的突变阈值（EV）；0 关闭这条判据；省略=1"},
                    "sharp_ratio": {"type": "number", "minimum": 0, "maximum": 1, "description": "清晰度低于序列中位的这个比例就判废；0 关闭这条判据；省略=0"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_cull_extract",
            hints: Hints::WRITE,
            description: "把保留下来的帧复制成一条新序列（剔除成子序列）：路径安全检查先行（不能是源内部、盘根或非空目录），复制保留 mtime、测光缓存一并带走（新序列不用重测）。exclude 给要剔除的帧下标（通常来自 aevum_cull_evaluate 的 indices）；dry_run: true 只报帧数与体积，不写任何东西；非 dry_run 需 confirm。",
            input_schema: object(
                json!({
                    "target": {"type": "string", "description": "目标目录（绝对路径）；不能是源序列根或其内部、盘符根、已存在且非空的目录"},
                    "exclude": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "要剔除的帧下标（0 开始）；省略=全部保留"},
                    "dry_run": {"type": "boolean", "description": "true 只报会复制几帧、共多少字节，不写任何东西"},
                    "confirm": {"type": "boolean", "description": "非 dry_run 时必须为 true"}
                }),
                &["target"],
            ),
        },
        Command {
            id: "aevum_project_get",
            hints: Hints::READ,
            description: "返回当前会话对应的工程内容（含参数、去闪烁设置）与磁盘上的工程路径。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_project_save",
            hints: Hints::WRITE,
            description: "把当前参数与去闪烁设置写入 <序列>/.aevum/project.json，保留评级与关键帧。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_journal_list",
            hints: Hints::READ,
            description: "读取当前序列的操作日志末尾（只记成功的写操作，一行一条 JSON）。",
            input_schema: object(
                json!({
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50, "description": "最多返回条数"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_xmp_read",
            hints: Hints::READ,
            description: "读取一个 XMP 预设/旁车文件，返回可用的调色参数（不自动应用）。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": "XMP 文件路径"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "aevum_xmp_write",
            hints: Hints::DESTRUCTIVE,
            description: "把当前调色参数写成一个 XMP 文件；覆盖已有文件需 confirm。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": "输出 .xmp 路径"},
                    "confirm": {"type": "boolean", "description": "覆盖已有文件时必需"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "aevum_cube_import",
            hints: Hints::WRITE,
            description: "把一张外部 .cube 三维 LUT 挂成「外观」（主链之前先套一层查表）；返回格点信息，apply=false 只解析不生效。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": ".cube 文件路径"},
                    "apply": {"type": "boolean", "default": true, "description": "是否立刻应用到当前参数"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "aevum_cube_export",
            hints: Hints::DESTRUCTIVE,
            description: "把当前调色参数导出成 .cube LUT（只含逐点运算，覆盖已有文件需 confirm）。size 默认 65。",
            input_schema: object(
                json!({
                    "path": {"type": "string", "description": "输出 .cube 路径"},
                    "size": {"type": "integer", "minimum": 2, "maximum": 256, "default": 65, "description": "立方体边长"},
                    "confirm": {"type": "boolean", "description": "覆盖已有文件时必需"}
                }),
                &["path"],
            ),
        },
        Command {
            id: "aevum_align",
            hints: Hints::WRITE,
            description: "估计每帧相对参考帧的位移并计算统一裁切框；结果存入会话，导出时自动套用。",
            input_schema: object(
                json!({
                    "reference": {"type": "integer", "minimum": 0, "default": 0, "description": "参考帧下标"},
                    "estimator": {"type": "string", "enum": ["template", "phase"], "default": "template", "description": "template=底部模板匹配，phase=相位相关"},
                    "smooth": {"type": "boolean", "default": false, "description": "是否平滑位移；默认不平滑，与 GUI 对齐结果一致"},
                    "smooth_window": {"type": "integer", "minimum": 0, "maximum": 999, "description": "平滑窗口；0 关闭，smooth=true 且不填时用 15"},
                    "max_shift": {"type": "integer", "minimum": 0, "maximum": 4096, "default": 64, "description": "搜索半径（像素）"},
                    "jump_px": {"type": "number", "minimum": 0, "default": 24, "description": "相邻帧跳变阈值（像素）"},
                    "min_score": {"type": "number", "minimum": -1, "maximum": 1, "default": 0.35, "description": "低于该匹配分记为低分帧"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台对齐并立即返回，用 aevum_align_status 查进度"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_align_status",
            hints: Hints::READ,
            description: "查询后台任务（对齐/测光/导出）的进度、是否运行中、上次对齐报告摘要与最近错误。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_cancel",
            hints: Hints::WRITE,
            description: "请求取消正在运行的后台对齐/测光/导出/队列；空闲时也返回成功，可重复调用（幂等）。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_queue_list",
            hints: Hints::READ,
            description: "列出批量渲染队列（与 GUI 共用 render_queue.json）；工程参数在渲染时从各自的 .aevum/project.json 读取。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_queue_add",
            hints: Hints::WRITE_APPEND,
            description: "把一条序列加入渲染队列；同一目录重复加入等于更新规格并挪到队尾。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "帧文件夹；省略时用当前序列"},
                    "out": {"type": "string", "description": "成片路径（.mp4/.mkv/.mov）；省略时放在序列目录下"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；省略=原生）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给；省略=原生）"},
                    "fps": {"type": "number", "exclusiveMinimum": 0, "maximum": 240, "default": 24},
                    "codec": {"type": "string", "enum": ["h265", "h264"], "default": "h265"},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 51, "default": 18},
                    "preset": {"type": "string", "default": "medium", "description": "x264/x265 速度档：ultrafast…veryslow"},
                    "keep_frames": {"type": "boolean", "default": false, "description": "编码后保留中间帧"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：输出目录里已经写完的帧原样留着，这一趟只补没写出来的那些（改了参数想重导就别开）"},
                    "copy": {"type": "string", "description": "该段改用序列工程里某套命名调色副本渲染；省略用工程主调色"}
                }),
                &[],
            ),
        },
        Command {
            id: "aevum_queue_remove",
            hints: Hints::WRITE,
            description: "从渲染队列移除一段（只改队列，不动素材）。",
            input_schema: object(
                json!({
                    "index": {"type": "integer", "minimum": 1, "description": "队列序号，从 1 开始"}
                }),
                &["index"],
            ),
        },
        Command {
            id: "aevum_queue_clear",
            hints: Hints::WRITE,
            description: "清空渲染队列（不删除已渲染的成片）；正在渲染时拒绝。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_queue_run",
            hints: Hints::DESTRUCTIVE_APPEND,
            description: "逐段渲染整个队列（批量写，必须 confirm）；默认同步跑完，background=true 时立即返回。",
            input_schema: object(
                json!({
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台渲染并立即返回，用 aevum_queue_status 查进度"}
                }),
                &["confirm"],
            ),
        },
        Command {
            id: "aevum_queue_status",
            hints: Hints::READ,
            description: "查询队列进度（段数、当前阶段、每段最近结果）与队列文件路径。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_queue_cancel",
            hints: Hints::WRITE,
            description: "请求取消正在渲染的队列；已完成的段保留结果，未开始的段保持待渲。",
            input_schema: object(json!({}), &[]),
        },
        Command {
            id: "aevum_sequences_scan",
            hints: Hints::READ,
            description: "枚举目录下的序列（只看不写）：根目录自己 + 往下 depth 层子目录；同时列出「有图却读不出来」的目录与原因。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "父目录；序列通常是它的子目录"},
                    "depth": {"type": "integer", "minimum": 0, "maximum": 4, "default": 1, "description": "往下找几层；0 = 只看这个目录自己"}
                }),
                &["root"],
            ),
        },
        Command {
            id: "aevum_batch_run",
            hints: Hints::DESTRUCTIVE_APPEND,
            description: "无人值守批量：扫目录下所有序列 → 入队 → 直接渲染，一次调用返回逐段结果（成功数/失败数/每段原因）。默认先清空队列，append=true 才追加。",
            input_schema: object(
                json!({
                    "root": {"type": "string", "description": "父目录；下面的序列全部参与"},
                    "depth": {"type": "integer", "minimum": 0, "maximum": 4, "default": 1, "description": "往下找几层；0 = 只看这个目录自己"},
                    "confirm": {"type": "boolean", "description": "批量写操作必须为 true"},
                    "append": {"type": "boolean", "default": false, "description": "true 时保留队列里已有的段并追加；默认先清空"},
                    "background": {"type": "boolean", "default": false, "description": "true 时后台渲染并立即返回，用 aevum_queue_status 查进度"},
                    "out": {"type": "string", "description": "只在扫到单条序列时可用；多条时省略（每段默认写 <序列目录>/<名字>_成片.mp4）"},
                    "width": {"type": "integer", "minimum": 2, "description": "精确输出宽（偶数，需与 height 同时给；省略=原生）"},
                    "height": {"type": "integer", "minimum": 2, "description": "精确输出高（偶数，需与 width 同时给；省略=原生）"},
                    "fps": {"type": "number", "exclusiveMinimum": 0, "maximum": 240, "default": 24},
                    "codec": {"type": "string", "enum": ["h265", "h264"], "default": "h265"},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 51, "default": 18},
                    "preset": {"type": "string", "default": "medium", "description": "x264/x265 速度档：ultrafast…veryslow"},
                    "keep_frames": {"type": "boolean", "default": false, "description": "编码后保留中间帧"},
                    "resume": {"type": "boolean", "default": false, "description": "断点续跑：已经写完的帧原样留着，这一趟只补没写出来的那些"},
                    "copy": {"type": "string", "description": "各段统一改用序列工程里某套命名调色副本渲染；省略用工程主调色"}
                }),
                &["root", "confirm"],
            ),
        },
    ]
}
