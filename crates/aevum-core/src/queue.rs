//! 渲染队列：一次排若干段序列，逐段渲成调色后的成片。
//!
//! 队列里存渲染意图（源目录、成片路径、规格）加**加入那一刻的工程快照**。
//! 渲染一段时优先用它的快照 —— "排队后再改调色不必重排"承诺的就是这个；
//! 老队列项和外部构造的项没有快照，回落到从它自己的 `.aevum/project.json`
//! 现读（自动保存可能已把加入之后的调色写进去，读到什么算什么）。
//!
//! 中间帧落在成片旁边的 `<片名>_帧/`；`keep_frames` 为假时编码成功后清掉。
//! `resume` 为真的那一段只补没写出来的中间帧，跑崩了重排队不必从头再渲一遍。
//! 某一项失败不拖死整队：记下原因，继续下一项。
//!
//! Render queue: several sequences are queued at once and each is rendered
//! into a graded final render, one after another.
//!
//! The queue stores the render intent (source directory, final-render path,
//! specs) plus the project snapshot taken at enqueue time. Rendering a
//! segment prefers its snapshot: that is exactly the promise of "regrade
//! after queueing without requeueing". Old queue items and externally built
//! items have no snapshot and fall back to reading `.aevum/project.json`
//! fresh (autosave may already have written the post-enqueue grade; whatever
//! it reads is what it gets).
//!
//! Intermediate frames land in `<video-name>_帧/` next to the final render
//! and are cleared after successful encoding when `keep_frames` is false. A
//! segment with `resume` true only fills in intermediate frames not yet
//! written, so requeueing after a crash does not rerender from scratch. One
//! failing item does not drag down the whole queue: the reason is recorded
//! and the next item proceeds.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::audio::AudioOptions;
use crate::colorspace::ColorSpace;
use crate::export::{
    self, Codec, ExportError, JobProgress, OutputMode, OutputSharpen, RenderOptions,
    SharpenStrength, StillFormat,
};
use crate::grade::GradeParams;
use crate::interp;
use crate::project::{self, ProjectFile};
use crate::sequence::{Sequence, SequenceError};

pub const MAX_ITEMS: usize = 200;
pub const QUEUE_FILE: &str = "render_queue.json";

pub const PRESETS: &[&str] = &[
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
    "placebo",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum QueueStatus {
    #[default]
    Pending,
    Done,
    Failed,
}

impl QueueStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "待渲",
            Self::Done => "完成",
            Self::Failed => "失败",
        }
    }
}

impl<'de> Deserialize<'de> for QueueStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        Ok(match text.to_ascii_lowercase().as_str() {
            "done" | "完成" => Self::Done,
            "failed" | "失败" => Self::Failed,
            _ => Self::Pending,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueItem {
    pub root: String,
    pub out: String,
    /// 目标画幅；宽高都是 0 时保持源尺寸。
    ///
    /// Target aspect and size; when both width and height are 0, the source
    /// size is kept.
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub codec: String,
    pub crf: u8,
    pub preset: String,
    pub keep_frames: bool,
    /// 断点续跑：这一段的输出目录里已经写完的帧原样留着，只补没写出来的。
    ///
    /// Resume: frames already written in this segment's output directory are
    /// kept as-is, and only the missing ones are filled in.
    pub resume: bool,
    /// 输出色域键（`srgb`/`p3`/`rec2020`）。留空表示跟随该项自己工程文件里的设置。
    ///
    /// Output gamut key (`srgb`/`p3`/`rec2020`). Empty means follow the
    /// setting in this item's own project file.
    pub color_space: String,
    /// 指定用该序列工程文件里某套调色副本渲染；留空 = 工程里的主调色。
    ///
    /// Renders with a named grading variant from the sequence's project
    /// file; empty = the project's main grade.
    pub copy: String,
    /// 加入队列那一刻的工程快照（完整 [`ProjectFile`]）。渲染优先用它，
    /// 没有才回落磁盘上的 project.json —— 老队列项没有这一段，行为与
    /// 以前一致。之前渲染一律读磁盘：自动保存可能已经把加入之后的调色
    /// 写进去，"排队后再改调色不必重排"就落空了（审查确认）。
    ///
    /// Project snapshot taken at enqueue time (a complete [`ProjectFile`]).
    /// Rendering prefers it and only falls back to the on-disk project.json
    /// when it is absent; old queue items lack this field and behave exactly
    /// as before. Rendering used to always read the disk, where autosave may
    /// already have written the post-enqueue grade, breaking the "regrade
    /// after queueing without requeueing" promise (confirmed by review).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectFile>,
    pub status: QueueStatus,
    pub note: String,
}

impl Default for QueueItem {
    fn default() -> Self {
        Self {
            root: String::new(),
            out: String::new(),
            width: 3840,
            height: 2160,
            fps: 24.0,
            codec: "h265".to_owned(),
            crf: 18,
            preset: "medium".to_owned(),
            keep_frames: false,
            resume: false,
            color_space: String::new(),
            copy: String::new(),
            project: None,
            status: QueueStatus::Pending,
            note: String::new(),
        }
    }
}

impl QueueItem {
    pub fn new(root: impl AsRef<Path>, out: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().display().to_string(),
            out: out.as_ref().display().to_string(),
            ..Self::default()
        }
    }

    /// 规整路径与规格；坏值拉回安全默认，保证读旧文件不会整行丢掉。
    ///
    /// Normalizes paths and specs; bad values are pulled back to safe
    /// defaults so reading an old file never drops the whole row.
    pub fn normalized(mut self) -> Result<Self, String> {
        self.root = normalized_path(&self.root)?.display().to_string();
        self.out = normalized_path(&self.out)?.display().to_string();
        if !(self.fps.is_finite() && (1.0..=240.0).contains(&self.fps)) {
            self.fps = 24.0;
        }
        if self.crf > 51 {
            self.crf = 18;
        }
        if !PRESETS.contains(&self.preset.as_str()) {
            self.preset = "medium".to_owned();
        }
        if self.codec != "h264" {
            self.codec = "h265".to_owned();
        }
        match (self.width, self.height) {
            (0, 0) => {}
            (w, h) if w >= 2 && h >= 2 => {
                self.width = w & !1;
                self.height = h & !1;
            }
            _ => {
                self.width = 0;
                self.height = 0;
            }
        }
        Ok(self)
    }

    /// 加入队列前的检查：目录得真的在，成片别写成源帧。
    ///
    /// Checks before enqueueing: the directory must really exist, and the
    /// final render must not overwrite a source frame.
    pub fn clean(self) -> Result<Self, String> {
        let item = self.normalized()?;
        let root = PathBuf::from(&item.root);
        if !root.is_dir() {
            return Err(format!("序列目录不存在：{}", item.root));
        }
        let out = PathBuf::from(&item.out);
        if !out
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "mkv" | "mov"))
        {
            return Err("成片扩展名必须是 .mp4 / .mkv / .mov".to_owned());
        }
        Ok(item)
    }
}

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("{0}")]
    Invalid(String),
    #[error("打开序列失败：{0}")]
    Sequence(#[from] SequenceError),
    #[error("{0}")]
    Project(#[from] project::ProjectError),
    #[error("{0}")]
    Export(#[from] ExportError),
}

/// `<成片去扩展名>_帧`，跟着成片走，删成片时好一起找。
///
/// `<final-render stem>_帧`, sitting next to the final render so it can be
/// found together when the render is deleted.
pub fn frames_dir(out: &Path) -> PathBuf {
    let Some(name) = out.file_name() else {
        return out.with_extension("_帧");
    };
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| name.to_os_string());
    let mut dir_name = stem;
    dir_name.push("_帧");
    out.with_file_name(dir_name)
}

/// 队列文件默认放在漫游配置目录；`AEVUM_RENDER_QUEUE`（旧名 `LUMEN_RENDER_QUEUE` 回退）
/// 可把它指到别处。
///
/// The queue file lives in the roaming config directory by default;
/// `AEVUM_RENDER_QUEUE` (legacy name `LUMEN_RENDER_QUEUE` as fallback) can
/// point it elsewhere.
pub fn default_path() -> PathBuf {
    crate::config_dir::ensure_adopted();
    if let Some(path) = crate::env::var_os("RENDER_QUEUE")
        && !path.is_empty()
    {
        let path = PathBuf::from(path);
        // 不回退是产品承诺；只把 override 用不了这件事喊出来（带缓存）。
        crate::env::check_override_cached("RENDER_QUEUE", &path, crate::env::Target::File);
        return path;
    }
    crate::config_dir::dir().join(QUEUE_FILE)
}

pub fn load(path: &Path) -> Vec<QueueItem> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_str::<QueueFile>(&text) else {
        return Vec::new();
    };
    file.items
        .into_iter()
        .filter_map(|item| item.normalized().ok())
        .take(MAX_ITEMS)
        .collect()
}

pub fn save(path: &Path, items: &[QueueItem]) -> Result<(), std::io::Error> {
    let file = QueueFile {
        version: 1,
        saved: String::new(),
        items: items.to_vec(),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    // rename 覆盖不了已存在的目标（Windows 尤其如此），所以要先挪走旧清单。
    // 不能直接 remove：rename 万一失败，用户的队列清单就没了。先改名成
    // .bak——rename 是原子的，任何一步失败旧文件都还在。
    let bak = path.with_extension("json.bak");
    if path.exists() {
        std::fs::rename(path, &bak)?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        // 换不上去就把旧的还回去，别让清单人间蒸发。
        if bak.exists() {
            let _ = std::fs::rename(&bak, path);
        }
        return Err(e);
    }
    let _ = std::fs::remove_file(&bak);
    Ok(())
}

/// 同一目录只留一行：重复加入等于更新规格并退回待渲。
///
/// One row per directory: adding a duplicate updates the specs and resets
/// the status to pending.
pub fn add(items: &mut Vec<QueueItem>, item: QueueItem) -> Result<usize, String> {
    let item = item.clean()?;
    let key = normcase(Path::new(&item.root));
    items.retain(|r| normcase(Path::new(&r.root)) != key);
    items.push(QueueItem {
        status: QueueStatus::Pending,
        note: String::new(),
        ..item
    });
    if items.len() > MAX_ITEMS {
        let drop = items.len() - MAX_ITEMS;
        items.drain(0..drop);
    }
    Ok(items.len() - 1)
}

/// 渲染一项：开序列 → 读它自己的工程 → 调色落帧 → ffmpeg 编码。
///
/// 取消时返回 `ExportError::Cancelled`；已写出的中间帧保留（下次重渲覆盖）。
///
/// Renders one item: open the sequence, read its own project, grade to
/// frames, then encode with ffmpeg.
///
/// Returns `ExportError::Cancelled` on cancellation; intermediate frames
/// already written are kept (overwritten on the next rerender).
pub fn render_item(
    item: &QueueItem,
    ffmpeg: &Path,
    cancel: &AtomicBool,
    progress: &(dyn Fn(JobProgress) + Sync),
) -> Result<PathBuf, QueueError> {
    let root = PathBuf::from(&item.root);
    let out = PathBuf::from(&item.out);
    let seq = Sequence::open(&root)?;
    let paths = seq.paths();
    if paths.is_empty() {
        return Err(QueueError::Invalid("序列里没有可用的帧".to_owned()));
    }
    let frames = frames_dir(&out);
    if same_path(&frames, &root) {
        return Err(QueueError::Invalid("中间帧目录不能是源序列目录".to_owned()));
    }
    if seq.frames.iter().any(|f| same_path(&f.path, &out)) {
        return Err(QueueError::Invalid("成片路径指向了源序列里的帧".to_owned()));
    }

    // 优先用加入时快照：排队后用户继续调色、自动保存更新了磁盘工程，
    // 都不影响这一段该渲成什么。没有快照（老队列文件）才现读磁盘。
    let project = match &item.project {
        Some(snapshot) => snapshot.clone(),
        None => {
            let project_path = project::project_path(&root);
            if project_path.is_file() {
                project::load(&project_path)?
            } else {
                ProjectFile::default()
            }
        }
    };

    let n = paths.len();
    // 指定了副本就用那套调色渲染这一段；名字对不上现在就报错，别等渲完才发现不对。
    let grade = if item.copy.is_empty() {
        None
    } else {
        Some(
            project
                .copies
                .iter()
                .find(|c| c.name == item.copy)
                .ok_or_else(|| {
                    QueueError::Invalid(format!("序列工程里没有叫 {} 的调色副本", item.copy))
                })?,
        )
    };
    let (base_params, base_keys) = match &grade {
        Some(copy) => (&copy.params, &copy.keyframes),
        None => (&project.params, &project.keyframes),
    };
    let params: Vec<GradeParams> = (0..n)
        .map(|i| interp::params_at(base_params, base_keys, i))
        .collect();
    let mut gains = vec![crate::develop::Ev::NONE; n];
    if project.def_offsets.len() == n {
        for (i, v) in project.def_offsets.iter().enumerate() {
            gains[i].add = *v;
        }
    }
    if project.def_chan.len() == n {
        for (i, c) in project.def_chan.iter().enumerate() {
            gains[i].chan = *c;
        }
    }
    let mut offsets = vec![[0.0, 0.0]; n];
    // 工程里的 `crop` 是导出取景（GUI 已经把对齐裁切和画幅比例叠好了）；老工程没写
    // 这一项时才退回对齐裁切。
    let crop = project
        .crop
        .or_else(|| project.align.as_ref().and_then(|a| a.crop));
    if let Some(align) = &project.align {
        for (i, off) in align.offsets.iter().take(n).enumerate() {
            offsets[i] = *off;
        }
    }
    let audio = match project.render.audio.trim() {
        "" => None,
        path => Some(PathBuf::from(path)),
    };
    let options = RenderOptions {
        mode: OutputMode::Video,
        format: StillFormat::Png8,
        quality: 90,
        keep_exif: false,
        // 说明与版权跟着工程走：排队之后成片容器里的标签不该悄悄丢掉。
        metadata: project.metadata.clone(),
        // 队列跑的是成片，中间帧不发旁车。
        xmp_sidecar: false,
        // 队列项跑的是成片，输出锐化沿用该项工程里的设置：面板上调好的档位
        // 排队之后不该悄悄丢掉。
        sharpen_for: OutputSharpen::parse(&project.render.sharpen_for).unwrap_or_default(),
        sharpen_strength: SharpenStrength::parse(&project.render.sharpen_strength)
            .unwrap_or_default(),
        out_dir: frames,
        video_out: out,
        crop,
        rotate: project.rotate,
        // 运镜跟变速、水印同理：这一段工程里开了取景框，成片就不该悄悄回到静止画面。
        motion: project.render.motion,
        long_side: None,
        target_size: (item.width >= 2 && item.height >= 2).then_some((item.width, item.height)),
        fps: item.fps,
        codec: if item.codec == "h264" {
            Codec::H264
        } else {
            Codec::H265
        },
        crf: item.crf,
        preset: item.preset.clone(),
        audio,
        audio_opts: AudioOptions::default(),
        keep_frames: item.keep_frames,
        resume: item.resume,
        // 队列项没写就回落到该项工程里的设置，两头都认不出来才用 sRGB。
        color_space: ColorSpace::parse(&item.color_space)
            .or_else(|| ColorSpace::parse(&project.render.color_space))
            .unwrap_or_default(),
        // 水印跟输出锐化同理：排队之后不该悄悄丢掉。
        watermark: project.render.watermark.clone(),
    };

    // 变速与调色、水印同理：这一段工程里开了重映射，成片就不该悄悄回到 1:1。
    let plan = export::plan_for(&project.render.retime, n, item.fps)?;
    let video = export::export_video_planned(
        ffmpeg,
        &paths,
        &params,
        &gains,
        &offsets,
        &options,
        plan.as_ref(),
        cancel,
        progress,
    )?;
    if !options.keep_frames {
        // 直连管道时这一层根本没建；留帧的老路走完编码会先把帧清掉，目录空了也收走。
        let _ = std::fs::remove_dir(&options.out_dir);
    }
    Ok(video)
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct QueueFile {
    version: u32,
    saved: String,
    items: Vec<QueueItem>,
}

fn normalized_path(text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("路径为空".to_owned());
    }
    Ok(lexical_absolute(Path::new(text)))
}

/// 只做词法清理、不碰文件系统：目标可能还不存在，canonicalize 会失败。
fn lexical_absolute(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn normcase(path: &Path) -> String {
    let text = lexical_absolute(path).display().to_string();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    normcase(a) == normcase(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("aevum-queue-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn frames_dir_sits_next_to_video() {
        assert_eq!(
            frames_dir(Path::new(r"C:\x\成片.mp4")),
            PathBuf::from(r"C:\x\成片_帧")
        );
        assert_eq!(
            frames_dir(Path::new("/tmp/a.mkv")),
            PathBuf::from("/tmp/a_帧")
        );
    }

    #[test]
    fn clean_normalizes_paths_and_specs() {
        let dir = temp_dir("clean");
        let item = QueueItem {
            root: format!("{}", dir.display()),
            out: format!("{}/a.mp4", dir.display()),
            width: 3841,
            height: 2159,
            fps: 0.0,
            codec: "hevc".to_owned(),
            crf: 99,
            preset: "turbo".to_owned(),
            ..QueueItem::default()
        }
        .clean()
        .unwrap();
        assert_eq!((item.width, item.height), (3840, 2158));
        assert_eq!(item.fps, 24.0);
        assert_eq!(item.crf, 18);
        assert_eq!(item.codec, "h265");
        assert_eq!(item.preset, "medium");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_rejects_missing_directory_and_bad_extension() {
        let dir = temp_dir("reject");
        let missing = QueueItem::new(dir.join("nope"), dir.join("a.mp4"))
            .clean()
            .unwrap_err();
        assert!(missing.contains("不存在"));
        let bad_ext = QueueItem::new(&dir, dir.join("a.png")).clean().unwrap_err();
        assert!(bad_ext.contains("扩展名"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_replaces_same_root_and_resets_status() {
        let a = temp_dir("a");
        let b = temp_dir("b");
        let mut items = Vec::new();
        let first = QueueItem {
            status: QueueStatus::Done,
            note: "old".to_owned(),
            ..QueueItem::new(&a, a.join("x.mp4"))
        };
        add(&mut items, first).unwrap();
        add(&mut items, QueueItem::new(&b, b.join("y.mp4"))).unwrap();
        let index = add(&mut items, QueueItem::new(&a, a.join("z.mp4"))).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(index, 1);
        assert_eq!(items[1].out, a.join("z.mp4").display().to_string());
        assert_eq!(items[1].status, QueueStatus::Pending);
        assert!(items[1].note.is_empty());
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn load_tolerates_missing_and_broken_files() {
        let dir = temp_dir("load");
        let path = dir.join(QUEUE_FILE);
        assert!(load(&path).is_empty());
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_empty());
        std::fs::write(
            &path,
            r#"{"items":[{"root":"C:/x","out":"C:/y.mp4","status":"完成"},
                          {"out":""}]}"#,
        )
        .unwrap();
        let items = load(&path);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, QueueStatus::Done);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_roundtrips() {
        let dir = temp_dir("save");
        let path = dir.join(QUEUE_FILE);
        let item = QueueItem {
            status: QueueStatus::Failed,
            note: "坏帧".to_owned(),
            ..QueueItem::new(&dir, dir.join("a.mp4"))
        };
        save(&path, std::slice::from_ref(&item)).unwrap();
        let back = load(&path);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].status, QueueStatus::Failed);
        assert_eq!(back[0].note, "坏帧");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_snapshot_roundtrips_and_old_files_stay_compatible() {
        // 新字段必须能带着快照落盘再读回来，一个字节都不能丢。
        let dir = temp_dir("snap");
        let path = dir.join(QUEUE_FILE);
        let mut project = ProjectFile::default();
        project.params.exposure = 1.25;
        project.preset = "自定义".to_owned();
        let item = QueueItem {
            project: Some(project),
            ..QueueItem::new(&dir, dir.join("a.mp4"))
        };
        save(&path, std::slice::from_ref(&item)).unwrap();
        let back = &load(&path)[0];
        let snap = back.project.as_ref().expect("快照应当跟着队列文件走");
        assert_eq!(snap.params.exposure, 1.25);
        assert_eq!(snap.preset, "自定义");

        // 老队列文件没有 project 字段：照常读回，快照为 None（渲染回落磁盘）。
        std::fs::write(
            &path,
            r#"{"items":[{"root":"C:/x","out":"C:/y.mp4","status":"待渲"}]}"#,
        )
        .unwrap();
        let old = load(&path);
        assert_eq!(old.len(), 1);
        assert!(old[0].project.is_none(), "老队列项不该凭空长出快照");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
