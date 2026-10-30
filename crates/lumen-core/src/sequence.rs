//! 序列：把文件夹里的帧按自然顺序排好。
//!
//! 只做发现有与排序，不解码。读图在 `metrics` 里，按需并行。

use std::path::{Path, PathBuf};

use thiserror::Error;

/// 能当帧用的扩展名，必须和 `metrics::decode` 真解得开的格式对齐。
///
/// 相机 RAW 里只有 DNG 认：CR2/NEF/ARW 这些是厂商私有压缩，解不开却先认领，
/// 用户看到的就是一整段「读图失败」。XMP 更不能进来——它是调色参数的旁车文件，
/// 就写在帧旁边，当帧用等于自己给自己造坏帧。
///
/// 厂商 RAW（rw2/nef/arw/cr2/cr3/orf/rwl）现在由 `raw::read` 兜底：传感器 CFA 或
/// 机身内嵌预览两条路都走得通，位深口径只有一套。
const IMAGE_EXT: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "tif", "tiff", "webp", "bmp", "dng", "rw2", "nef", "arw", "cr2",
    "cr3", "orf", "rwl",
];

#[derive(Debug, Error)]
pub enum SequenceError {
    #[error("读取目录失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("这个文件夹里没有可用的帧")]
    Empty,
    #[error("这些文件里有我们解不开的格式")]
    NoUsableFile,
    #[error("选中的照片不在同一个文件夹，先放进一个目录")]
    NoCommonRoot,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub index: usize,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Sequence {
    pub root: PathBuf,
    pub frames: Vec<Frame>,
}

impl Sequence {
    /// 打开一个帧文件夹。顺序是自然序：`f9` 排在 `f10` 前面。
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, SequenceError> {
        let root = dir.as_ref().to_path_buf();
        let entries: Vec<PathBuf> = std::fs::read_dir(&root)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_image(p))
            .collect();
        Self::build(root, entries)
    }

    /// 打开明确给定的几张照片，可能是单张。
    ///
    /// 单张延时帧、或者从胶片目录里挑出来的一段，都不该被迫先包成文件夹才能处理：
    /// 目录里剩下的照片不是素材，拿它们凑成序列等于替用户改了他没要的帧。
    /// 护带（`.lumen/`、control.json、工程文件）仍然落在这几张照片的公共父目录。
    pub fn open_files(paths: &[PathBuf]) -> Result<Self, SequenceError> {
        let entries: Vec<PathBuf> = paths
            .iter()
            .filter(|p| p.is_file() && is_image(p))
            .cloned()
            .collect();
        if entries.is_empty() {
            return Err(SequenceError::NoUsableFile);
        }
        let root = common_parent(&entries).ok_or(SequenceError::NoCommonRoot)?;
        Self::build(root, entries)
    }

    /// 文件或目录都收：拖进来什么就开什么。
    pub fn open_any(path: impl AsRef<Path>) -> Result<Self, SequenceError> {
        let path = path.as_ref();
        if path.is_dir() {
            Self::open(path)
        } else {
            Self::open_files(std::slice::from_ref(&path.to_path_buf()))
        }
    }

    fn build(root: PathBuf, mut entries: Vec<PathBuf>) -> Result<Self, SequenceError> {
        if entries.is_empty() {
            return Err(SequenceError::Empty);
        }
        // 旧工程的护带目录叫 `.lapse`，打开时顺手搬成 `.lumen`：
        // 参数、评级、关键帧都是同一份 JSON，搬完就能接着用。
        crate::project::migrate_guard_dir(&root);
        entries.sort_by(|a, b| {
            let an = a.file_name().map(|s| s.to_string_lossy().to_string());
            let bn = b.file_name().map(|s| s.to_string_lossy().to_string());
            match (an, bn) {
                (Some(x), Some(y)) => natural_cmp(&x, &y),
                _ => a.cmp(b),
            }
        });
        let frames = entries
            .into_iter()
            .enumerate()
            .map(|(index, path)| Frame {
                index,
                name: path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                path,
            })
            .collect();
        Ok(Self { root, frames })
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.frames.iter().map(|f| f.path.clone()).collect()
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// 只有一帧时素材就是那张照片，标题该报照片名而不是它所在的文件夹名。
    pub fn display_name(&self) -> String {
        if self.is_single()
            && let Some(stem) = self.frames[0]
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        {
            return stem;
        }
        self.name()
    }

    /// 单张照片：序列级的活（对齐、去闪、关键帧）没有意义，界面据此收掉。
    pub fn is_single(&self) -> bool {
        self.frames.len() == 1
    }
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXT.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// 这个扩展名能不能当帧用。界面用它决定文件选择器放行哪些照片。
pub fn is_supported_extension(ext: &str) -> bool {
    IMAGE_EXT.contains(&ext.to_ascii_lowercase().as_str())
}

/// 能直接拖进来的扩展名列表，给文件对话框当过滤器用。
pub fn image_extensions() -> &'static [&'static str] {
    IMAGE_EXT
}

/// 几张照片的公共父目录；只有一张时就是它自己的目录。
fn common_parent(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut iter = paths.iter().filter_map(|p| p.parent());
    let mut base = iter.next()?.to_path_buf();
    for parent in iter {
        while !parent.starts_with(&base) {
            if !base.pop() {
                return None;
            }
        }
    }
    Some(base)
}

/// 自然序比较：数字段按数值比，其余按字符比。
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.char_indices().peekable();
    let mut bi = b.char_indices().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some((ap, ac)), Some((bp, bc))) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let a_run = digit_run(a, ap);
                    let b_run = digit_run(b, bp);
                    let av = a_run.trim_start_matches('0');
                    let bv = b_run.trim_start_matches('0');
                    let ord = av.len().cmp(&bv.len()).then_with(|| av.cmp(bv));
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                    // 数值相同（0007 与 7）时，位宽长的排前面，保证稳定。
                    let ord = b_run.len().cmp(&a_run.len());
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                    for _ in 0..a_run.len() {
                        ai.next();
                    }
                    for _ in 0..b_run.len() {
                        bi.next();
                    }
                } else {
                    let ord = ac.cmp(&bc);
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

fn digit_run(s: &str, start: usize) -> &str {
    let rest = &s[start..];
    let end = rest
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    &rest[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_puts_9_before_10() {
        let mut v = vec!["f10.jpg", "f9.jpg", "f100.jpg", "f0010.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["f9.jpg", "f0010.jpg", "f10.jpg", "f100.jpg"]);
    }
}
