//! 工程文件：让参数、评级、关键帧、对齐与成片设置随序列一起落盘。
//!
//! 位置固定为 `<序列根>/.aevum/project.json`；缺字段一律取默认值，
//! 所以旧工程、只写了一半的工程都能打开。保存先写临时文件再替换。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::align::{AlignIssue, CropRect};
use crate::audio::AudioOptions;
use crate::grade::GradeParams;

pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AlignState {
    pub reference: usize,
    pub estimator: String,
    pub offsets: Vec<[f64; 2]>,
    pub scores: Vec<f64>,
    pub issues: Vec<AlignIssue>,
    pub crop: Option<CropRect>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderState {
    pub mode: String,
    pub format: String,
    /// 有损容器（JPEG）的压缩质量 1~100；无损档忽略。
    pub quality: u8,
    /// 把源 JPEG 的 EXIF 块搬进静帧输出。
    pub keep_exif: bool,
    /// 每张静帧旁写一个同名 `.xmp` 旁车（`dc:` 四项 + 这一张的调色参数）。
    pub xmp_sidecar: bool,
    /// 输出锐化用途档键（`off`/`screen`/`glossy`/`matte`）。
    pub sharpen_for: String,
    /// 输出锐化强度档键（`low`/`standard`/`high`）。
    pub sharpen_strength: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub codec: String,
    pub crf: u8,
    pub preset: String,
    pub out_dir: String,
    pub video_out: String,
    pub audio: String,
    /// 配乐的延迟 / 音量 / 淡入淡出 / 循环 / 编码；旧工程缺省即为默认。
    pub audio_opts: AudioOptions,
    pub long_side: Option<u32>,
    pub keep_frames: bool,
    /// 输出色域键（`srgb`/`p3`/`rec2020`）；旧工程没这个字段时按 sRGB 走，行为不变。
    pub color_space: String,
    /// 水印整块设定；旧工程没有这一段就是没开水印。
    pub watermark: crate::watermark::Watermark,
    /// 时间重映射（变速 + 运动模糊）；旧工程没这一段就是 1:1 原速。
    pub retime: crate::retime::Retime,
    /// 运镜（Ken Burns）：取景框从起点走到终点；旧工程没这一段就是不开。
    pub motion: crate::crop::Motion,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            mode: "video".to_owned(),
            format: "png8".to_owned(),
            quality: 90,
            keep_exif: false,
            xmp_sidecar: false,
            sharpen_for: crate::export::OutputSharpen::default().key().to_owned(),
            sharpen_strength: crate::export::SharpenStrength::default().key().to_owned(),
            width: 3840,
            height: 2160,
            fps: 24.0,
            codec: "h265".to_owned(),
            crf: 18,
            preset: "medium".to_owned(),
            out_dir: String::new(),
            video_out: String::new(),
            audio: String::new(),
            audio_opts: AudioOptions::default(),
            long_side: None,
            keep_frames: false,
            color_space: crate::colorspace::ColorSpace::default().key().to_owned(),
            watermark: crate::watermark::Watermark::default(),
            retime: crate::retime::Retime::default(),
            motion: crate::crop::Motion::default(),
        }
    }
}

/// 一个目录的说明与版权信息（IPTC 的那几项，按都柏林核心写进 XMP / EXIF）。
///
/// 跟着工程文件走，不另开数据库：图库、序列与导出读的是同一份，改一处三边都认。
/// 全字段可选，空字符串就是没填——写元数据时按空跳过，不往文件里塞空标签。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    /// 标题（`dc:title` / EXIF 无对应，只进 XMP 与视频容器）。
    pub title: String,
    /// 说明 / 字幕（`dc:description` / EXIF `ImageDescription`）。
    pub description: String,
    /// 创作者（`dc:creator` / EXIF `Artist`）。
    pub creator: String,
    /// 版权声明（`dc:rights` / EXIF `Copyright`）。
    pub copyright: String,
}

/// 各字段的收口上限，都要留得住 `exif` 那边读回来的余地：
/// ASCII 标签按字节数读（`exif::MAX_TEXT` = 512 字节，含结尾 NUL），截断了多字节
/// 字符会让 UTF-8 解码失败、自己写的说明自己读不回来；标题走 UCS2，按字符算。
const LIMIT_TITLE_CHARS: usize = 200;
const LIMIT_DESCRIPTION_BYTES: usize = 480;
const LIMIT_PERSON_BYTES: usize = 120;

impl Metadata {
    /// 四项全空（或只剩空白）。导出按它决定要不要动元数据。
    pub fn is_empty(&self) -> bool {
        [
            &self.title,
            &self.description,
            &self.creator,
            &self.copyright,
        ]
        .iter()
        .all(|s| s.trim().is_empty())
    }

    /// 收干净的副本：去首尾空白、把换行与连排空白折成单个空格、按上限截断。
    /// EXIF 的 ASCII 标签放不下换行，说明里的换行也得折掉。
    pub fn cleaned(&self) -> Self {
        Self {
            title: self
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(LIMIT_TITLE_CHARS)
                .collect(),
            description: cut_bytes(&self.description, LIMIT_DESCRIPTION_BYTES),
            creator: cut_bytes(&self.creator, LIMIT_PERSON_BYTES),
            copyright: cut_bytes(&self.copyright, LIMIT_PERSON_BYTES),
        }
    }

    /// 只留填了的项，给导出与 XMP 用：`(键, 值)` 的非空子集，值已归一化。
    pub fn present(&self) -> Vec<(&'static str, String)> {
        let c = self.cleaned();
        [
            ("title", c.title),
            ("description", c.description),
            ("creator", c.creator),
            ("copyright", c.copyright),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .collect()
    }
}

/// 折空白并按字节数截断，截在字符边界上（切半个汉字会让整段文本解不出来）。
fn cut_bytes(raw: &str, bytes: usize) -> String {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= bytes {
        return flat;
    }
    let mut end = bytes;
    while end > 0 && !flat.is_char_boundary(end) {
        end -= 1;
    }
    flat[..end].to_owned()
}

/// 一套命名调色：与主链同构（基础参数 + 关键帧）。同一个序列的多版调色
/// 并存互不覆盖，随时取回当前，或指定给某一次导出 / 队列段。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GradeCopy {
    pub name: String,
    pub params: GradeParams,
    pub keyframes: BTreeMap<usize, GradeParams>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectFile {
    pub version: u32,
    pub root: String,
    pub reference: usize,
    pub params: GradeParams,
    pub preset: String,
    pub ratings: BTreeMap<usize, u8>,
    pub flags: BTreeMap<usize, i8>,
    pub labels: BTreeMap<usize, u8>,
    /// 逐帧关键词（归一化、排序）；图库按它筛选，序列里也能读。
    pub keywords: BTreeMap<usize, Vec<String>>,
    /// 堆叠：一叠 = 一组帧索引（0 起）。落盘前经 `library::normalize_stacks`
    /// 归一（升序、≥2 帧、叠间不重叠、按头帧排序），旧工程没这一段就是没叠。
    pub stacks: Vec<Vec<usize>>,
    /// 这个目录的说明与版权信息；旧工程没这一段就是四项全空。
    pub metadata: Metadata,
    pub keyframes: BTreeMap<usize, GradeParams>,
    /// 命名调色副本；旧工程没这一段就是没有副本。
    pub copies: Vec<GradeCopy>,
    pub def_window: f64,
    pub def_strength: f64,
    pub def_multi: bool,
    /// 逐帧去闪烁曝光补偿（EV），渲染队列据此复现效果。
    pub def_offsets: Vec<f32>,
    /// 分通道去闪的逐帧色差残差（R,G,B）；单通道去闪时全为零。
    pub def_chan: Vec<[f32; 3]>,
    /// 去闪烁是否按 R/G/B 各测各的（压彩色场景的色闪）。
    pub def_channels: bool,
    /// 画幅比例标签（`原生 / 16:9 / 4:3 / 1:1 / 9:16`）。裁剪框是按这个比例定形的，
    /// 两者必须一起存，否则重新打开工程时框和比例会对不上。
    pub aspect: String,
    /// 用户拖出来的裁剪框（对齐后坐标，原图像素）。`None` = 没手动裁，
    /// 取景按画幅比例在对齐安全框内居中内接。GUI 恢复的是这一项，
    /// 好让框还能接着拖；`crop` 是它叠完对齐与画幅之后的最终取景。
    pub user_crop: Option<CropRect>,
    /// 拉直角度（度，正角=画面逆时针）。取景框是在转过之后的帧里量的，
    /// 所以角度和框必须一起存：只存框会在重开时把画面转回去、框却还留着。
    pub rotate: f64,
    /// 导出取景：GUI 把对齐裁切、用户框、画幅比例叠好后写下来的最终框，
    /// 渲染队列照它裁，不再自己重算几何。
    pub crop: Option<CropRect>,
    pub align: Option<AlignState>,
    pub render: RenderState,
    pub font_level: usize,
}

impl Default for ProjectFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            root: String::new(),
            reference: 0,
            params: GradeParams::default(),
            preset: "原始".to_owned(),
            ratings: BTreeMap::new(),
            flags: BTreeMap::new(),
            labels: BTreeMap::new(),
            keywords: BTreeMap::new(),
            stacks: Vec::new(),
            metadata: Metadata::default(),
            keyframes: BTreeMap::new(),
            copies: Vec::new(),
            def_window: 15.0,
            def_strength: 100.0,
            def_multi: false,
            def_offsets: Vec::new(),
            def_chan: Vec::new(),
            def_channels: false,
            aspect: String::new(),
            user_crop: None,
            rotate: 0.0,
            crop: None,
            align: None,
            render: RenderState::default(),
            font_level: 0,
        }
    }
}

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error("读写工程失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("工程文件不是合法 JSON：{0}")]
    Json(#[from] serde_json::Error),
    #[error("工程版本 {0} 比程序支持的 {VERSION} 新，请升级程序")]
    Version(u32),
    /// 护带路径上蹲着一个同名**文件**（不是目录）。
    ///
    /// 不拦的话，`create_dir_all` 会抛出 `os error 183`（Windows「文件已存在」）或
    /// `EEXIST`——用户看到的是一串系统错误码，完全猜不到根因是自己帧文件夹里
    /// 多了个叫 `.aevum` 的文件。宁可提前判掉，给一句能照着做的说明。
    #[error(
        "{} 上有个同名文件、不是目录，工程存不进去；请把它移走或改名后重试（程序没有动它）",
        .0.display()
    )]
    GuardBlockedByFile(std::path::PathBuf),
}

/// 序列根目录下的护带目录：工程文件、控制令牌、操作日志都在这儿。
/// 名字只这一处定义，`journal`、`control` 与迁移脚本都跟着它走。
pub const GUARD_DIR: &str = crate::brand::GUARD_DIR;

/// 更早的护带目录名，**从新到旧**。`.lumen` 是 2026-10 之前，`.lapse` 更早。
pub const LEGACY_GUARD_DIRS: [&str; 2] = crate::brand::LEGACY_GUARD_DIRS;

/// 判定过的护带目录：进程内每个目录只探一次（探针是真建真删文件），
/// 也只喊一次。`guard_dir` 挂在 UI 渲染路径上，每帧会被 `project_path()` 调好几次，
/// 不缓存的话光探测就把渲染帧预算吃掉了。
static GUARD_PROBED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

/// 只读探测：现在该用哪个护带目录。**不搬、不建、不返回错误。**
///
/// 优先级：`.aevum` > `.lumen` > `.lapse` > 默认 `.aevum`（尚无护带时）。
///
/// 所有路径构造函数都走这里，因此「搬不动」的场景下读写会自动落到旧目录，
/// 不会出现「读旧的、写新的」把数据劈成两半。
///
/// 解析完会顺手判一次可写性（结果按目录缓存）：盘只读、护带路径被同名文件占住时
/// 压一条 `env::note`。这不是「报错」而是「让界面看得见」——本函数仍然正常返回路径，
/// 读写照旧走它，只是用户不会再在点保存的那一刻才发现存不进去。
///
/// 注意**不在这里顺手搬**：本函数会在 UI 渲染路径与写日志时被调用，
/// 搬目录是写操作，绝不能藏在「读路径」的语义里。搬只发生在 `adopt_guard_dir`。
pub fn guard_dir(root: &Path) -> PathBuf {
    let dir = resolved_guard_dir(root);
    // 解析出目录之后再判「能不能写」：搬不动、盘只读、被同名文件占住——这三种
    // 情况以前只有 stderr 一行，GUI 用户完全看不见，点保存才发现存不进去。
    report_guard_once(&dir);
    dir
}

/// 只解析、不探测：优先级与判定口径见 [`guard_dir`]。
fn resolved_guard_dir(root: &Path) -> PathBuf {
    let fresh = root.join(GUARD_DIR);
    // 判「是不是目录」而不是「存不存在」：`.aevum` 万一是同名**文件**（用户手滑建了个
    // 文件、或者别的工具写出来的），把它当目录用会让后续 `create_dir_all` 失败、
    // 保存直接报错。新旧目录用同一个判据，读和写才不会各说各话。
    if fresh.is_dir() {
        return fresh;
    }
    for old in LEGACY_GUARD_DIRS {
        let p = root.join(old);
        if p.is_dir() {
            return p;
        }
    }
    // 还没有护带：返回新名字的路径。首次写入时由调用方 `create_dir_all` 建出来。
    fresh
}

/// 「护带路径被同名文件占住」的统一文案。
///
/// `guard_dir` 与 `adopt_guard_dir_with` 都会撞上这一种，用同一句话是为了让
/// `env::note` 的内容去重生效：用户只看到一条，而不是同一件事的两条转述。
fn blocked_by_file_note(dir: &Path) -> String {
    format!(
        "护带目录 {} 被同名文件占住，改动将无法保存；请把那个文件移走或改名（程序没有动它）",
        crate::env::absolute_for_show(dir)
    )
}

/// 护带目录不可写时的说明；`None` 表示没发现问题。调用方保证 `dir` 已存在。
fn guard_blocked_reason(dir: &Path) -> Option<String> {
    let shown = crate::env::absolute_for_show(dir);
    // 同名文件把护带路径占住：这是「不可写」里最容易被误读成程序坏了的一种，
    // 原因必须单独点名，否则用户只会看到保存时的一句系统错误码。
    if !dir.is_dir() {
        return Some(blocked_by_file_note(dir));
    }
    if crate::env::dir_is_writable(dir) {
        return None;
    }
    Some(format!(
        "护带目录 {shown} 只读或权限不足（探针文件建不出来），改动将无法保存；\
         请给它写权限，或把序列移到可写的盘上"
    ))
}

/// 每个目录只探一次、只告一次。
///
/// 目录**不存在**时直接放行：首次写入会把它建出来，判成不可写会让每个新序列
/// 一打开就误报一条。这种情况也不占缓存——它真被建出来之后还要再判一次。
fn report_guard_once(dir: &Path) {
    if !dir.exists() {
        return;
    }
    let seen = GUARD_PROBED.get_or_init(|| Mutex::new(HashSet::new()));
    {
        // 中毒锁里通常还留着有用数据，取出来继续用，别在这里 panic。
        let mut seen = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // 先登记再探测：即使探测本身 panic（不该发生），也不会变成每帧重试。
        if !seen.insert(dir.to_path_buf()) {
            return;
        }
    }
    if let Some(msg) = guard_blocked_reason(dir) {
        crate::env::note(msg);
    }
}

/// 打开序列时调一次：把**最近的那个**旧护带目录搬成 `.aevum`。
///
/// 返回真正在用的目录——搬不动时就是旧的那份，调用方照旧能用它读写。
///
/// 只搬最近的一个：`.lumen` 与 `.lapse` 同时存在说明中间跨了两个版本，
/// 更老那份可能有用户手工留下的东西，猜它会覆盖数据。
///
/// **幂等**：第一步就判 `.aevum` 是不是目录，搬过一次之后每次打开都是 O(1) 的 `is_dir()`。
///
/// **不覆盖**：`.aevum` 若被一个同名**文件**占着，一律不搬——Windows 上往一个已存在的
/// 文件上 rename 会把它抹掉，那是静默的数据丢失。这种情况退回旧目录，什么都不动。
pub fn adopt_guard_dir(root: &Path) -> PathBuf {
    adopt_guard_dir_with(root, |from, to| std::fs::rename(from, to))
}

/// 带注入缝的版本：`rename` 由调用方给。
///
/// 为什么要这个缝：「rename 失败 → 继续用旧目录」是本模块**最高危**的一条路径
/// （走错就是把用户的参数弄丢），而它在真实文件系统上**造不出稳定的触发条件**——
/// Windows 上只读属性、文件占用都不一定拦得住 rename，用 chmod 写出来的测试在
/// Windows 上是空转的：测不到，而且看不出来没测到。所以把 rename 变成参数，
/// 测试直接传一个必然失败的假 renamer，就能在任何平台上钉死这条路径。
pub fn adopt_guard_dir_with(
    root: &Path,
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> PathBuf {
    let fresh = root.join(GUARD_DIR);
    if fresh.is_dir() {
        return fresh;
    }
    // 新名路径上占着一个**同名文件**（用户手滑、别的工具写出来的）。
    // 此时绝不能往里 rename：Windows 的 MoveFileEx(REPLACE_EXISTING) 会把那个文件直接抹掉，
    // 用户的东西就这么没了，而且没有任何提示。宁可退回旧目录继续读写，什么都不动。
    let blocked = fresh.exists();
    for old in LEGACY_GUARD_DIRS {
        let p = root.join(old);
        if !p.is_dir() {
            continue;
        }
        if blocked {
            eprintln!(
                "{}: {} 是个文件、不是目录，不能往里搬；继续用 {}（那个文件没动，请自行处理）",
                crate::brand::BIN,
                fresh.display(),
                p.display()
            );
            // stderr 在 GUI 子系统下看不见：这条同样要进界面告警队列，否则用户
            // 永远不知道自己的工程为什么还留在旧目录里。
            crate::env::note(format!(
                "护带目录 {} 被同名文件占住，搬不过去；工程继续存在 {}，\
                 请把那个文件移走或改名（程序没有动它）",
                crate::env::absolute_for_show(&fresh),
                crate::env::absolute_for_show(&p)
            ));
            return p;
        }
        return match rename(&p, &fresh) {
            Ok(()) => {
                // 成功路径也要一行日志：用户有权知道自己的目录被搬过、搬到哪去了。
                // 没有这行，他会以为工程被重置了。
                eprintln!(
                    "{}: 护带目录已迁移：{} → {}",
                    crate::brand::BIN,
                    p.display(),
                    fresh.display()
                );
                if old != LEGACY_GUARD_DIRS[0] {
                    // 搬的是更早那一级（说明更新的那级不在）。不替他猜、也不替他合并。
                    eprintln!(
                        "{}: 提示：这次搬的是更早的 {}；若你更常用 {}，请手动把内容合并进来。",
                        crate::brand::BIN,
                        old,
                        LEGACY_GUARD_DIRS[0]
                    );
                }
                fresh
            }
            // 只读盘 / 文件被占：rename 会失败。此时**继续用旧目录读写**，
            // 而不是降级成只读——只读会让「保存」静默失败，用户以为存上了。
            Err(e) => {
                eprintln!(
                    "{}: 护带目录搬不动（{}）：{e}；继续用 {}",
                    crate::brand::BIN,
                    p.display(),
                    p.display()
                );
                let (used, target) = (
                    crate::env::absolute_for_show(&p),
                    crate::env::absolute_for_show(&fresh),
                );
                // 搬不动本身不致命（旧目录还能写），但如果旧目录也写不进去，
                // 那就是「改动将无法保存」这一级的事故，必须说到点子上。
                let msg = if crate::env::dir_is_writable(&p) {
                    format!(
                        "护带目录 {used} 搬不动到 {target}（{e}），改动继续存在 {used}；\
                         请检查 {target} 所在盘是否只读、或该路径是否被占用"
                    )
                } else {
                    format!(
                        "护带目录 {used} 搬不动到 {target}（{e}），且 {used} 本身不可写，\
                         改动将无法保存；请给它写权限，或把序列移到可写的盘上"
                    )
                };
                crate::env::note(msg);
                p
            }
        };
    }
    // 两个旧目录都没有：返回新路径，首次写入时建。
    //
    // 但如果这条路径被同名**文件**占着，必须当场说清楚。否则这条链会一路沉默到
    // 用户点保存的那一刻，然后只看到 `os error 183`——而那句话里不含任何关于
    // 「你目录里有个叫 .aevum 的文件」的信息，用户只会以为程序坏了。
    if blocked {
        eprintln!(
            "{}: {} 是个文件、不是目录，工程将无法保存；请把它移走或改名（程序没有动它）",
            crate::brand::BIN,
            fresh.display()
        );
        // 与 `guard_dir` 共用同一句话：两边都会走到，靠内容去重只留一条。
        crate::env::note(blocked_by_file_note(&fresh));
    }
    fresh
}

/// `<root>/.aevum/project.json`（护带目录尚未迁移时落在旧目录里）。
pub fn project_path(root: &Path) -> PathBuf {
    guard_dir(root).join("project.json")
}

pub fn load(path: &Path) -> Result<ProjectFile, ProjectError> {
    // 主文件读不了/坏了回落 .bak：save 换新文件前会把旧文件原子地改名成
    // bak，bak 里是上一次确认可用的版本。bak 不存在或也坏才交出原始错误。
    let primary = match std::fs::read_to_string(path) {
        Ok(text) => parse_project(&text),
        Err(e) => Err(ProjectError::from(e)),
    };
    match primary {
        Ok(p) => Ok(p),
        Err(primary_err) => {
            let bak = path.with_extension("json.bak");
            if bak.is_file()
                && let Ok(text) = std::fs::read_to_string(&bak)
                && let Ok(p) = parse_project(&text)
            {
                return Ok(p);
            }
            Err(primary_err)
        }
    }
}

fn parse_project(text: &str) -> Result<ProjectFile, ProjectError> {
    let project: ProjectFile = serde_json::from_str(text)?;
    if project.version > VERSION {
        return Err(ProjectError::Version(project.version));
    }
    Ok(project)
}

/// 先写临时文件并落盘，再把旧文件原子挪成 .bak、新文件原子换上来。
///
/// 之前的做法是 tmp → `remove_file` → `rename`：remove 与 rename 之间
/// 崩一下，整个工程就没了（审查确认）。现在与 `queue::save` 同一套
/// 样板：rename 是原子的，旧文件先改名成 bak、任何一步失败旧文件都
/// 还在，换失败就滚回去；成功后 bak 删掉。
pub fn save(path: &Path, project: &ProjectFile) -> Result<(), ProjectError> {
    if let Some(dir) = path.parent() {
        // 目标被同名文件占住时，`create_dir_all` 抛的是系统级错误码，用户读不懂。
        // 提前判掉：`.aevum` 是个文件 → 说清楚「有个同名文件」，而不是 os error 183。
        if !dir.is_dir() && dir.exists() {
            return Err(ProjectError::GuardBlockedByFile(dir.to_path_buf()));
        }
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(project)?;
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        // 换名前先把临时文件刷到盘上：不 sync 的话，掉电后 rename 可能已
        // 完成而内容还没落盘，"新"文件就是半截的。
        f.sync_all()?;
    }
    // Windows 的 rename 不能覆盖已存在的目标，旧文件得先挪走 —— 挪成 bak
    // 而不是 remove：中间崩了旧文件也还在。
    let bak = path.with_extension("json.bak");
    if path.exists() {
        std::fs::rename(path, &bak)?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        // 换不上去就把旧的还回去，别让工程人间蒸发。
        if bak.exists() {
            let _ = std::fs::rename(&bak, path);
        }
        return Err(e.into());
    }
    let _ = std::fs::remove_file(&bak);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "aevum-project-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_roundtrips_and_leaves_no_debris() {
        let dir = temp_dir("save");
        let path = dir.join("project.json");
        let mut project = ProjectFile::default();
        project.params.exposure = 0.4;
        save(&path, &project).expect("第一次保存");
        let mut project2 = project.clone();
        project2.params.exposure = -1.5;
        save(&path, &project2).expect("覆盖保存");
        // 覆盖保存后磁盘上是新值，bak 也清掉了，不留 .tmp 残骸。
        let back = load(&path).expect("读回");
        assert_eq!(back.params.exposure, -1.5);
        assert!(
            !path.with_extension("json.bak").exists(),
            "bak 应当在成功后清掉"
        );
        assert!(
            !path.with_extension("json.tmp").exists(),
            "tmp 应当被换成名"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_falls_back_to_the_bak_when_the_main_file_is_broken() {
        // 回归：主文件损坏（半截写入/磁盘坏块）时，只要 bak 还在就不该
        // 丢工程 —— bak 里是上一次确认可用的版本。
        let dir = temp_dir("bak");
        let path = dir.join("project.json");
        let mut project = ProjectFile::default();
        project.params.exposure = 2.25;
        save(&path, &project).expect("保存一份好工程");
        // 手动造出"主文件坏、bak 还是上一版"的状态（save 成功后 bak 已清，
        // 所以 bak 也手工写）。
        std::fs::write(&path, "{\"version\":1,\"params\":").unwrap();
        std::fs::write(
            path.with_extension("json.bak"),
            serde_json::to_string_pretty(&project).unwrap(),
        )
        .unwrap();
        let back = load(&path).expect("主文件坏了应当回落 bak");
        assert_eq!(back.params.exposure, 2.25, "回落到的应当是上一版");
        // bak 也没有，才交出原始错误。
        std::fs::remove_file(path.with_extension("json.bak")).unwrap();
        let err = load(&path).expect_err("主文件坏且无 bak 应当报错");
        assert!(
            matches!(err, ProjectError::Json(_)),
            "报的应当是解析错误：{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_of_a_missing_file_reports_io_error_not_silent_default() {
        let dir = temp_dir("missing");
        let path = dir.join("nope.json");
        let err = load(&path).expect_err("文件不存在应当报错");
        assert!(matches!(err, ProjectError::Io(_)), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
