//! 图库：跨目录的根目录清单、手动集合，以及把每个目录编目成「文件 × 评级/色标/旗标」。
//!
//! 清单只存根目录路径与集合成员，落在漫游配置目录（`galleries.json`）；评级、色标、
//! 旗标仍住在各自序列的 `.lumen/project.json` 里——图库只读写它们，不做第二份真相，
//! 在序列里改完评分回到图库看，永远是新的。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::project;
use crate::sequence::Sequence;

pub const LIBRARY_FILE: &str = "galleries.json";

/// 清单上限：图库是导航用的，攒几百个目录只会让面板滚不到底。
const MAX_ROOTS: usize = 64;
/// 集合同理：每个集合存成员路径，不是把照片搬进别处。
const MAX_COLLECTIONS: usize = 32;

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct LibraryFile {
    version: u32,
    roots: Vec<String>,
    /// v2 才有的手动集合；v1 清单读进来是空集合列表。
    collections: Vec<Collection>,
}

/// 手动集合：一组跨目录的照片路径。只存路径，评级/色标照旧从各自工程文件读。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    pub name: String,
    pub files: Vec<String>,
}

/// 清单的全部内容：根目录 + 集合。
#[derive(Clone, Debug, Default)]
pub struct Store {
    pub roots: Vec<PathBuf>,
    pub collections: Vec<Collection>,
}

/// 编目里的一张照片：路径加上从该目录工程文件里读出的整理状态。
#[derive(Clone, Debug, PartialEq)]
pub struct LibFile {
    pub path: PathBuf,
    pub name: String,
    /// 自然序帧号（0 开始），也就是工程文件里 ratings/flags/labels 的键。
    pub index: usize,
    pub rating: u8,
    pub label: u8,
    pub flag: i8,
    /// 关键词（已归一化、按字典序去重）；筛选与右键菜单都看它。
    pub keywords: Vec<String>,
    /// 所在堆叠的序号（`ProjectFile.stacks` 的下标）；没叠就是 None。
    pub stack: Option<usize>,
}

impl LibFile {
    fn unrated(path: PathBuf, name: String, index: usize) -> Self {
        Self {
            path,
            name,
            index,
            rating: 0,
            label: 0,
            flag: 0,
            keywords: Vec::new(),
            stack: None,
        }
    }
}

/// 编目筛选：星级下限、只看选用、指定色标（0 = 任意）、指定关键词（空 = 任意）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub stars: u8,
    pub picked: bool,
    pub label: u8,
    pub keyword: String,
}

impl Filter {
    pub fn keeps(&self, f: &LibFile) -> bool {
        f.rating >= self.stars
            && (!self.picked || f.flag > 0)
            && (self.label == 0 || f.label == self.label)
            && (self.keyword.is_empty() || f.keywords.iter().any(|k| k == &self.keyword))
    }

    pub fn active(&self) -> bool {
        self.stars > 0 || self.picked || self.label > 0 || !self.keyword.is_empty()
    }
}

/// 评级/旗标/色标的一次改写：`None` 表示这项不动，`0` 表示清掉。
#[derive(Clone, Copy, Debug, Default)]
pub struct Organize {
    pub rating: Option<u8>,
    pub flag: Option<i8>,
    pub label: Option<u8>,
}

/// 清单默认放漫游配置目录；`LUMEN_LIBRARY` 可指到别处（测试与便携盘用）。
pub fn default_path() -> PathBuf {
    if let Some(path) = std::env::var_os("LUMEN_LIBRARY")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("lumen-studio").join(LIBRARY_FILE)
}

/// 读清单；文件缺失或坏档都当作空清单，图库面板照常能加目录。
pub fn load(path: &Path) -> Store {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Store::default();
    };
    let Ok(file) = serde_json::from_str::<LibraryFile>(&text) else {
        return Store::default();
    };
    Store {
        roots: file.roots.into_iter().map(PathBuf::from).collect(),
        collections: file
            .collections
            .into_iter()
            .filter(|c| !c.name.trim().is_empty())
            .collect(),
    }
}

/// 原子写清单：tmp + rename，坏在半路也不吞旧档。
pub fn save(path: &Path, store: &Store) -> Result<(), std::io::Error> {
    let file = LibraryFile {
        version: 2,
        roots: store
            .roots
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        collections: store.collections.clone(),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(&tmp, path)
}

fn key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// 加一个根目录：同一目录只留一行，重复加入等于挪到末尾。
pub fn add(roots: &mut Vec<PathBuf>, dir: &Path) -> usize {
    let k = key(dir);
    roots.retain(|r| key(r) != k);
    roots.push(dir.to_path_buf());
    if roots.len() > MAX_ROOTS {
        let drop = roots.len() - MAX_ROOTS;
        roots.drain(0..drop);
    }
    roots.len() - 1
}

pub fn remove(roots: &mut Vec<PathBuf>, index: usize) -> Option<PathBuf> {
    if index >= roots.len() {
        return None;
    }
    Some(roots.remove(index))
}

/// 建一个集合；同名集合直接复用（返回已有序号）。名字空白时叫「未命名集合」。
pub fn collection_create(cols: &mut Vec<Collection>, name: &str) -> usize {
    let name = name.trim();
    let name = if name.is_empty() {
        "未命名集合"
    } else {
        name
    };
    if let Some(i) = cols.iter().position(|c| c.name == name) {
        return i;
    }
    if cols.len() >= MAX_COLLECTIONS {
        cols.remove(0);
    }
    cols.push(Collection {
        name: name.to_owned(),
        files: Vec::new(),
    });
    cols.len() - 1
}

pub fn collection_delete(cols: &mut Vec<Collection>, index: usize) -> Option<Collection> {
    (index < cols.len()).then(|| cols.remove(index))
}

/// 把一张照片放进集合；已经在集合里返回 `false`。
pub fn collection_put(cols: &mut [Collection], index: usize, path: &Path) -> bool {
    let Some(c) = cols.get_mut(index) else {
        return false;
    };
    let k = key(path);
    if c.files.iter().any(|f| key(Path::new(f)) == k) {
        return false;
    }
    c.files.push(path.display().to_string());
    true
}

/// 从集合里拿掉一张照片；本来就不在返回 `false`。
pub fn collection_drop(cols: &mut [Collection], index: usize, path: &Path) -> bool {
    let Some(c) = cols.get_mut(index) else {
        return false;
    };
    let k = key(path);
    let before = c.files.len();
    c.files.retain(|f| key(Path::new(f)) != k);
    c.files.len() != before
}

/// 改一个目录里某帧的整理状态并原子落盘。只碰评级/旗标/色标三张表，
/// 其余字段原样保留；`0` 表示清掉这一项（不写 key）。
pub fn organize(dir: &Path, index: usize, patch: &Organize) -> Result<(), project::ProjectError> {
    organize_many(dir, &[index], patch)
}

/// 一次改一批帧：整批只读写一次工程文件，批量挑片不会变成 N 次落盘。
pub fn organize_many(
    dir: &Path,
    frames: &[usize],
    patch: &Organize,
) -> Result<(), project::ProjectError> {
    let path = project::project_path(dir);
    let mut proj = match project::load(&path) {
        Ok(p) => p,
        // 比程序新的工程看不懂也不能覆盖，原样报上去。
        Err(e @ project::ProjectError::Version(_)) => return Err(e),
        Err(_) => project::ProjectFile::default(),
    };
    for &index in frames {
        if let Some(rating) = patch.rating {
            if rating == 0 {
                proj.ratings.remove(&index);
            } else {
                proj.ratings.insert(index, rating.min(5));
            }
        }
        if let Some(flag) = patch.flag {
            if flag == 0 {
                proj.flags.remove(&index);
            } else {
                proj.flags.insert(index, flag.signum());
            }
        }
        if let Some(label) = patch.label {
            if label == 0 {
                proj.labels.remove(&index);
            } else {
                proj.labels.insert(index, label.min(4));
            }
        }
    }
    project::save(&path, &proj)
}

/// 关键词最长字符数：再长在行里、在筛选条里都只会被截断。
pub const MAX_KEYWORD_CHARS: usize = 48;

/// 归一化一个关键词：去掉首尾空白、把中间的连续空白压成一个、超长截断。
/// 空白或只剩空白的返回 `None`。
pub fn clean_keyword(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    for word in raw.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        return None;
    }
    Some(out.chars().take(MAX_KEYWORD_CHARS).collect())
}

/// 一次给一批帧加词/去词，整批只读写一次工程文件；返回实际改动过的帧数。
/// 同一个词在一帧里只留一份，帧内按字典序排——筛选与右键菜单不用再排。
pub fn keyword_edit(
    dir: &Path,
    frames: &[usize],
    add: &[String],
    remove: &[String],
) -> Result<usize, project::ProjectError> {
    let add: Vec<String> = add.iter().filter_map(|w| clean_keyword(w)).collect();
    let remove: Vec<String> = remove.iter().filter_map(|w| clean_keyword(w)).collect();
    if add.is_empty() && remove.is_empty() {
        return Ok(0);
    }
    let path = project::project_path(dir);
    let mut proj = match project::load(&path) {
        Ok(p) => p,
        Err(e @ project::ProjectError::Version(_)) => return Err(e),
        Err(_) => project::ProjectFile::default(),
    };
    let mut touched = 0;
    for &index in frames {
        let mut words = proj.keywords.remove(&index).unwrap_or_default();
        let before = words.clone();
        words.retain(|w| !remove.iter().any(|r| r == w));
        for w in &add {
            if !words.contains(w) {
                words.push(w.clone());
            }
        }
        words.sort();
        words.dedup();
        if words == before {
            // 没变就不写回，避免给工程文件添一个空的键。
            if !before.is_empty() {
                proj.keywords.insert(index, words);
            }
            continue;
        }
        touched += 1;
        if words.is_empty() {
            proj.keywords.remove(&index);
        } else {
            proj.keywords.insert(index, words);
        }
    }
    if touched > 0 {
        project::save(&path, &proj)?;
    }
    Ok(touched)
}

/// 编目里出现过的所有关键词与出现次数，按词排序；筛选下拉与命令面共用。
pub fn keyword_counts(files: &[LibFile]) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for f in files {
        for w in &f.keywords {
            *counts.entry(w.as_str()).or_default() += 1;
        }
    }
    counts.into_iter().map(|(w, n)| (w.to_owned(), n)).collect()
}

/// 读一个目录的说明与版权信息。没有工程文件、或读不出来（坏 JSON）都当四项全空：
/// 图库面板不能因为一个目录的工程坏了就打不开。
pub fn metadata(dir: &Path) -> project::Metadata {
    project::load(&project::project_path(dir))
        .map(|p| p.metadata)
        .unwrap_or_default()
}

/// 写一个目录的说明与版权信息：一次 load / 改 / 原子 save，评级、关键词、调色、
/// 导出设置等其余字段原样保留。返回真正落盘的那一份（已归一化、已截断）。
/// 工程比程序新时按 `ProjectError::Version` 报上去，绝不覆盖。
pub fn set_metadata(
    dir: &Path,
    meta: &project::Metadata,
) -> Result<project::Metadata, project::ProjectError> {
    let path = project::project_path(dir);
    let mut proj = match project::load(&path) {
        Ok(p) => p,
        Err(e @ project::ProjectError::Version(_)) => return Err(e),
        Err(_) => project::ProjectFile::default(),
    };
    let cleaned = meta.cleaned();
    if proj.metadata == cleaned {
        return Ok(cleaned);
    }
    proj.metadata = cleaned.clone();
    project::save(&path, &proj)?;
    Ok(cleaned)
}

// ---------------------------------------------------------------- 堆叠

/// 归一化堆叠：每叠去重升序、不足 2 帧的丢掉、同一帧只留在最先出现的叠里、
/// 叠按头帧排序。落盘与读回都走它，所以界面看到的和文件里的是同一份结构。
pub fn normalize_stacks(raw: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Vec<usize>> = Vec::new();
    for group in raw {
        let mut members: Vec<usize> = group.iter().copied().filter(|f| seen.insert(*f)).collect();
        members.sort_unstable();
        members.dedup();
        if members.len() >= 2 {
            out.push(members);
        }
    }
    out.sort_by(|a, b| a[0].cmp(&b[0]));
    out
}

/// 一帧落在哪一叠（叠序号）；没叠返回 None。
pub fn stack_of(stacks: &[Vec<usize>], frame: usize) -> Option<usize> {
    stacks
        .iter()
        .position(|group| group.binary_search(&frame).is_ok())
}

/// 按文件名前缀自动分叠：去掉扩展名与结尾的数字串后同前缀、且 ≥2 张的归一叠。
/// 一个目录里装着好几段延时（`A001_0001.jpg`…`B002_0001.jpg`…）时，这就是「同一段」。
pub fn auto_stacks(names: &[String]) -> Vec<Vec<usize>> {
    let mut groups: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (index, name) in names.iter().enumerate() {
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
        let prefix = stem.trim_end_matches(|c: char| c.is_ascii_digit());
        if prefix.is_empty() || prefix == stem {
            continue;
        }
        groups.entry(prefix.to_owned()).or_default().push(index);
    }
    normalize_stacks(&groups.into_values().collect::<Vec<_>>())
}

/// 读一个目录的堆叠（已归一化）。没有工程文件就是没叠。
pub fn stacks(dir: &Path) -> Vec<Vec<usize>> {
    project::load(&project::project_path(dir))
        .map(|p| normalize_stacks(&p.stacks))
        .unwrap_or_default()
}

fn load_for_edit(dir: &Path) -> Result<project::ProjectFile, project::ProjectError> {
    let path = project::project_path(dir);
    match project::load(&path) {
        Ok(p) => Ok(p),
        Err(e @ project::ProjectError::Version(_)) => Err(e),
        Err(_) => Ok(project::ProjectFile::default()),
    }
}

/// 把一组帧合并成一叠：这些帧先从其它叠里摘出来（摘空/摘剩 1 帧的叠随之消失），
/// 新叠按头帧插回原位。返回落盘后的全部堆叠。
pub fn stack_create(
    dir: &Path,
    frames: &[usize],
) -> Result<Vec<Vec<usize>>, project::ProjectError> {
    let mut members: Vec<usize> = frames.to_vec();
    members.sort_unstable();
    members.dedup();
    if members.len() < 2 {
        return Ok(stacks(dir));
    }
    let path = project::project_path(dir);
    let mut proj = load_for_edit(dir)?;
    let next = normalize_stacks(&[members]);
    let mut rest: Vec<Vec<usize>> = proj
        .stacks
        .iter()
        .map(|group| {
            group
                .iter()
                .copied()
                .filter(|f| stack_of(&next, *f).is_none())
                .collect()
        })
        .collect();
    rest.push(next.into_iter().next().expect("归一后必有一叠"));
    let normalized = normalize_stacks(&rest);
    if normalized == normalize_stacks(&proj.stacks) {
        return Ok(normalized);
    }
    proj.stacks = normalized.clone();
    project::save(&path, &proj)?;
    Ok(normalized)
}

/// 拆掉一叠（成员回到不叠的状态）。序号越界当没这叠。返回落盘后的全部堆叠。
pub fn stack_dissolve(dir: &Path, stack: usize) -> Result<Vec<Vec<usize>>, project::ProjectError> {
    let path = project::project_path(dir);
    let mut proj = load_for_edit(dir)?;
    let current = normalize_stacks(&proj.stacks);
    if stack >= current.len() {
        return Ok(current);
    }
    let rest: Vec<Vec<usize>> = current
        .into_iter()
        .enumerate()
        .filter(|(i, _)| *i != stack)
        .map(|(_, group)| group)
        .collect();
    proj.stacks = rest.clone();
    project::save(&path, &proj)?;
    Ok(rest)
}

/// 按文件名前缀重新分叠（覆盖现有堆叠）。names 的顺序即帧号顺序。
pub fn stack_auto(dir: &Path, names: &[String]) -> Result<Vec<Vec<usize>>, project::ProjectError> {
    let path = project::project_path(dir);
    let mut proj = load_for_edit(dir)?;
    let next = auto_stacks(names);
    if next == normalize_stacks(&proj.stacks) {
        return Ok(next);
    }
    proj.stacks = next.clone();
    project::save(&path, &proj)?;
    Ok(next)
}

/// 把一个目录编目：图片按自然序列出，评级/色标/旗标/关键词从 `.lumen/project.json`
/// 折到每张照片上。目录不存在或没有可用图片返回空——图库面板按空目录显示，
/// 不算错误；真正的 IO 错误（权限等）才往上抛。
pub fn scan_root(dir: &Path) -> std::io::Result<Vec<LibFile>> {
    let seq = match Sequence::open(dir) {
        Ok(seq) => seq,
        Err(crate::sequence::SequenceError::Io(e)) => return Err(e),
        Err(_) => return Ok(Vec::new()),
    };
    let proj = project::load(&project::project_path(dir)).unwrap_or_default();
    let stacks = normalize_stacks(&proj.stacks);
    let mut files: Vec<LibFile> = seq
        .frames
        .iter()
        .enumerate()
        .map(|(i, f)| LibFile::unrated(f.path.clone(), f.name.clone(), i))
        .collect();
    for f in files.iter_mut() {
        f.rating = *proj.ratings.get(&f.index).unwrap_or(&0);
        f.flag = *proj.flags.get(&f.index).unwrap_or(&0);
        f.label = *proj.labels.get(&f.index).unwrap_or(&0);
        f.keywords = proj.keywords.get(&f.index).cloned().unwrap_or_default();
        f.stack = stack_of(&stacks, f.index);
    }
    Ok(files)
}

/// 目录里编目条目的汇总：总数与星级数，给根目录行当副标题。
pub fn summarize(files: &[LibFile]) -> String {
    let rated = files.iter().filter(|f| f.rating > 0).count();
    let picked = files.iter().filter(|f| f.flag > 0).count();
    match (rated, picked) {
        (0, 0) => format!("{} 张", files.len()),
        (_, 0) => format!("{} 张 · {} 张有星级", files.len(), rated),
        (0, _) => format!("{} 张 · {} 张选用", files.len(), picked),
        (_, _) => format!("{} 张 · {} 星级 · {} 选用", files.len(), rated, picked),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lumen-library-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").expect("写空文件");
    }

    #[test]
    fn roots_roundtrip_and_dedupe() {
        let dir = temp_dir("roots");
        let store = dir.join("galleries.json");
        let a = dir.join("a");
        let b = dir.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let mut roots: Vec<PathBuf> = Vec::new();
        add(&mut roots, &a);
        add(&mut roots, &b);
        add(&mut roots, &a);
        assert_eq!(roots.len(), 2);
        assert_eq!(roots.last(), Some(&a), "重复加入挪到末尾");

        let mut lib = Store {
            roots: roots.clone(),
            collections: Vec::new(),
        };
        save(&store, &lib).expect("存清单");
        let loaded = load(&store);
        assert_eq!(loaded.roots.len(), 2);

        assert_eq!(remove(&mut lib.roots, 5), None);
        assert_eq!(
            remove(&mut lib.roots, 0).as_deref(),
            Some(b.as_path()),
            "b 已挪到末尾"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v1_list_without_collections_still_loads() {
        let dir = temp_dir("v1");
        let store = dir.join("galleries.json");
        std::fs::write(&store, r#"{"version":1,"roots":["C:\\a"]}"#).unwrap();
        let lib = load(&store);
        assert_eq!(lib.roots.len(), 1);
        assert!(lib.collections.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collections_roundtrip_and_dedupe() {
        let dir = temp_dir("cols");
        let store = dir.join("galleries.json");
        let a = dir.join("a.png");
        let b = dir.join("b.png");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();

        let mut cols: Vec<Collection> = Vec::new();
        let i = collection_create(&mut cols, "  精选 ");
        assert_eq!(i, 0);
        assert_eq!(cols[0].name, "精选");
        assert_eq!(collection_create(&mut cols, "精选"), 0, "同名复用");
        assert!(collection_put(&mut cols, 0, &a));
        assert!(!collection_put(&mut cols, 0, &a), "同一张不重复");
        assert!(collection_put(&mut cols, 0, &b));
        assert_eq!(cols[0].files.len(), 2);

        save(
            &store,
            &Store {
                roots: Vec::new(),
                collections: cols.clone(),
            },
        )
        .expect("存清单");
        let loaded = load(&store);
        assert_eq!(loaded.collections, cols, "集合随清单落盘再读回");

        assert!(collection_drop(&mut cols, 0, &a));
        assert!(!collection_drop(&mut cols, 0, &a));
        assert_eq!(cols[0].files.len(), 1);
        assert_eq!(
            collection_delete(&mut cols, 0).map(|c| c.name),
            Some("精选".to_owned())
        );
        assert!(cols.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filter_keeps_by_stars_picked_and_label() {
        let f = |rating, flag, label| LibFile {
            path: PathBuf::from("x.png"),
            name: "x.png".to_owned(),
            index: 0,
            rating,
            flag,
            label,
            keywords: Vec::new(),
            stack: None,
        };
        let none = Filter::default();
        assert!(!none.active());
        assert!(none.keeps(&f(0, 0, 0)));

        let stars = Filter {
            stars: 3,
            ..Default::default()
        };
        assert!(stars.keeps(&f(3, 0, 0)));
        assert!(!stars.keeps(&f(2, 0, 0)));

        let picked = Filter {
            picked: true,
            ..Default::default()
        };
        assert!(picked.keeps(&f(0, 1, 0)));
        assert!(!picked.keeps(&f(0, -1, 0)));

        let label = Filter {
            label: 2,
            ..Default::default()
        };
        assert!(label.keeps(&f(0, 0, 2)));
        assert!(!label.keeps(&f(0, 0, 1)));

        let mut keyed = f(0, 0, 0);
        keyed.keywords = vec!["日出".to_owned(), "云海".to_owned()];
        let kw = Filter {
            keyword: "日出".to_owned(),
            ..Default::default()
        };
        assert!(kw.keeps(&keyed));
        assert!(!kw.keeps(&f(0, 0, 0)));
        assert!(stars.active() && picked.active() && label.active() && kw.active());
    }

    #[test]
    fn scan_root_maps_ratings_by_natural_index() {
        let dir = temp_dir("scan");
        for name in ["f2.png", "f10.png", "f1.png"] {
            touch(&dir, name);
        }
        std::fs::create_dir_all(dir.join(".lumen")).unwrap();
        let mut proj = project::ProjectFile::default();
        proj.ratings.insert(1, 4);
        proj.labels.insert(2, 3);
        proj.flags.insert(0, 1);
        project::save(&project::project_path(&dir), &proj).expect("写工程");

        let files = scan_root(&dir).expect("编目");
        assert_eq!(files.len(), 3);
        // 自然序：f1 < f2 < f10，工程文件里的键就是这套顺序的帧号。
        assert_eq!(files[0].name, "f1.png");
        assert_eq!(files[1].name, "f2.png");
        assert_eq!(files[2].name, "f10.png");
        assert_eq!(files[0].index, 0);
        assert_eq!(files[2].index, 2);
        assert_eq!(files[0].flag, 1);
        assert_eq!(files[1].rating, 4);
        assert_eq!(files[2].label, 3);
        assert_eq!(summarize(&files), "3 张 · 1 星级 · 1 选用");

        // 空目录与缺工程的目录：编目不报错，全是未评级。
        let empty = temp_dir("empty");
        let none = scan_root(&empty).expect("空目录");
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn organize_writes_only_the_given_field() {
        let dir = temp_dir("organize");
        for name in ["f1.png", "f2.png"] {
            touch(&dir, name);
        }
        let mut proj = project::ProjectFile::default();
        proj.ratings.insert(0, 2);
        proj.flags.insert(0, 1);
        proj.labels.insert(0, 3);
        proj.preset = "开发预设".to_owned();
        project::save(&project::project_path(&dir), &proj).expect("写工程");

        organize(
            &dir,
            1,
            &Organize {
                rating: Some(5),
                ..Default::default()
            },
        )
        .expect("改评级");
        let after = project::load(&project::project_path(&dir)).expect("读工程");
        assert_eq!(after.ratings.get(&1), Some(&5));
        assert_eq!(after.ratings.get(&0), Some(&2), "别的帧不动");
        assert_eq!(after.flags.get(&0), Some(&1), "旗标不动");
        assert_eq!(after.labels.get(&0), Some(&3), "色标不动");
        assert_eq!(after.preset, "开发预设", "其它字段原样保留");

        // 0 表示清掉；越界的星级/色标按上限夹住。
        organize(
            &dir,
            0,
            &Organize {
                rating: Some(0),
                label: Some(9),
                flag: Some(0),
            },
        )
        .expect("清评级、改色标、清旗标");
        let after = project::load(&project::project_path(&dir)).expect("读工程");
        assert!(!after.ratings.contains_key(&0));
        assert!(!after.flags.contains_key(&0));
        assert_eq!(after.labels.get(&0), Some(&4));

        // 批量：一次读写落到所有目标帧，没列的帧不动。
        organize_many(
            &dir,
            &[0, 1],
            &Organize {
                rating: Some(3),
                ..Default::default()
            },
        )
        .expect("批量改评级");
        let after = project::load(&project::project_path(&dir)).expect("读工程");
        assert_eq!(after.ratings.get(&0), Some(&3));
        assert_eq!(after.ratings.get(&1), Some(&3));

        // 没有工程文件的目录：从默认工程起步也能写。
        let fresh = temp_dir("organize-fresh");
        organize(
            &fresh,
            0,
            &Organize {
                flag: Some(-1),
                ..Default::default()
            },
        )
        .expect("新工程");
        let after = project::load(&project::project_path(&fresh)).expect("读工程");
        assert_eq!(after.flags.get(&0), Some(&-1));

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&fresh);
    }

    #[test]
    fn keywords_clean_edit_and_count() {
        assert_eq!(
            clean_keyword("  日出   前景 "),
            Some("日出 前景".to_owned())
        );
        assert_eq!(clean_keyword("   "), None);
        let long = "词".repeat(MAX_KEYWORD_CHARS + 10);
        assert_eq!(
            clean_keyword(&long).unwrap().chars().count(),
            MAX_KEYWORD_CHARS
        );

        let dir = temp_dir("keywords");
        for name in ["f1.png", "f2.png"] {
            touch(&dir, name);
        }
        // 加词：归一化、去重、排序；没给目录工程文件也能从默认工程起步。
        let added = keyword_edit(
            &dir,
            &[0],
            &["日出".to_owned(), " 云海 ".to_owned(), "日出".to_owned()],
            &[],
        )
        .expect("加词");
        assert_eq!(added, 1);
        let proj = project::load(&project::project_path(&dir)).expect("读工程");
        assert_eq!(
            proj.keywords.get(&0).unwrap(),
            &["云海".to_owned(), "日出".to_owned()],
            "帧内按字典序，重复只留一份"
        );

        // 批量：一帧加词、一帧去词，各自生效；没变化的帧不算改动。
        let added = keyword_edit(&dir, &[0, 1], &["日出".to_owned()], &["云海".to_owned()])
            .expect("批量改词");
        assert_eq!(added, 2);
        let proj = project::load(&project::project_path(&dir)).expect("读工程");
        assert_eq!(proj.keywords.get(&0).unwrap(), &["日出".to_owned()]);
        assert_eq!(proj.keywords.get(&1).unwrap(), &["日出".to_owned()]);
        assert_eq!(
            keyword_edit(&dir, &[1], &["日出".to_owned()], &[]).unwrap(),
            0
        );

        // 去掉最后一个词：键也一并删掉，工程里不留空数组。
        keyword_edit(&dir, &[1], &[], &["日出".to_owned()]).expect("去词");
        let proj = project::load(&project::project_path(&dir)).expect("读工程");
        assert!(!proj.keywords.contains_key(&1));

        // 编目把词带出来，计数按词去重统计；筛选按词收窄。
        let files = scan_root(&dir).expect("编目");
        assert_eq!(files[0].keywords, vec!["日出".to_owned()]);
        assert_eq!(files[1].keywords, Vec::<String>::new());
        assert_eq!(
            keyword_counts(&files),
            vec![("日出".to_owned(), 1)],
            "同一帧里重复的词只算一次"
        );
        let filter = Filter {
            keyword: "日出".to_owned(),
            ..Default::default()
        };
        assert!(filter.keeps(&files[0]));
        assert!(!filter.keeps(&files[1]));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_store_reads_as_empty() {
        let dir = temp_dir("bad");
        let store = dir.join("galleries.json");
        std::fs::write(&store, "{not json").unwrap();
        assert!(load(&store).roots.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn metadata_lives_in_the_directory_project() {
        let dir = temp_dir("meta");
        touch(&dir, "a.jpg");
        // 先留下别的状态：写元数据不许把评级与关键词抹掉。
        organize(
            &dir,
            0,
            &Organize {
                rating: Some(4),
                ..Default::default()
            },
        )
        .unwrap();
        keyword_edit(&dir, &[0], &["日出".to_owned()], &[]).unwrap();
        assert!(metadata(&dir).is_empty(), "新目录不该凭空有元数据");

        let saved = set_metadata(
            &dir,
            &project::Metadata {
                title: "  云海  ".to_owned(),
                description: "第一场\n日出".to_owned(),
                creator: "张三".to_owned(),
                copyright: "© 2026 张三".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(saved.title, "云海", "首尾空白没去掉");
        assert_eq!(saved.description, "第一场 日出", "换行该折成空格");
        assert_eq!(metadata(&dir), saved);
        let proj = project::load(&project::project_path(&dir)).unwrap();
        assert_eq!(proj.ratings.get(&0), Some(&4), "评级被抹掉了");
        assert_eq!(proj.keywords.get(&0).unwrap(), &["日出".to_owned()]);

        // 四项全空就是清掉，照样落盘，不能留着上一版。
        set_metadata(&dir, &project::Metadata::default()).unwrap();
        assert!(metadata(&dir).is_empty());
        let proj = project::load(&project::project_path(&dir)).unwrap();
        assert_eq!(proj.ratings.get(&0), Some(&4), "清元数据时把评级也清了");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newer_project_is_never_overwritten_by_metadata() {
        let dir = temp_dir("meta-version");
        let path = project::project_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!("{{\"version\":{},\"root\":\"x\"}}", project::VERSION + 1),
        )
        .unwrap();
        assert!(matches!(
            set_metadata(
                &dir,
                &project::Metadata {
                    creator: "张三".to_owned(),
                    ..Default::default()
                }
            ),
            Err(project::ProjectError::Version(_))
        ));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"root\""), "比程序新的工程被覆盖了");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stacks_normalize_merge_and_dissolve() {
        assert_eq!(
            normalize_stacks(&[vec![3, 1, 3], vec![1], vec![5, 4]]),
            vec![vec![1, 3], vec![4, 5]],
            "去重、被摘空的叠丢掉、按头帧排序"
        );
        assert_eq!(
            normalize_stacks(&[vec![2, 4], vec![4, 6]]),
            vec![vec![2, 4]],
            "同一帧只留在先出现的叠里"
        );
        assert_eq!(stack_of(&[vec![1, 2]], 2), Some(0));
        assert_eq!(stack_of(&[vec![1, 2]], 0), None);

        // 自动分叠：结尾数字串去掉后同前缀的归一叠；纯数字名与单张不叠。
        let names: Vec<String> = [
            "A01_0001.jpg",
            "A01_0002.jpg",
            "A01_0003.jpg",
            "B02_0001.jpg",
            "0001.jpg",
            "solo.jpg",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(auto_stacks(&names), vec![vec![0, 1, 2]]);

        let dir = temp_dir("stacks");
        for name in &names {
            touch(&dir, name);
        }
        assert!(stacks(&dir).is_empty());
        let got = stack_auto(&dir, &names).unwrap();
        assert_eq!(got, vec![vec![0, 1, 2]]);
        assert_eq!(stacks(&dir), got);
        let files = scan_root(&dir).unwrap();
        assert_eq!(files[1].stack, Some(0));
        assert_eq!(files[4].stack, None);

        // 手动合并跨叠的帧：被借走的帧从老叠里摘掉，老叠剩 2 帧还在。
        let got = stack_create(&dir, &[3, 4, 1]).unwrap();
        assert_eq!(got, vec![vec![0, 2], vec![1, 3, 4]]);
        // 再借一帧把老叠摘剩 1 帧，它就消失。
        let got = stack_create(&dir, &[2, 5]).unwrap();
        assert_eq!(got, vec![vec![1, 3, 4], vec![2, 5]], "摘剩 1 帧的叠该消失");
        // 拆掉之后回到没叠；越界序号当没这叠。
        let got = stack_dissolve(&dir, 0).unwrap();
        assert_eq!(got, vec![vec![2, 5]]);
        assert!(stack_dissolve(&dir, 0).unwrap().is_empty());
        assert!(stack_dissolve(&dir, 9).unwrap().is_empty(), "越界当没这叠");

        // 写堆叠不许抹掉评级与关键词。
        organize(
            &dir,
            0,
            &Organize {
                rating: Some(5),
                ..Default::default()
            },
        )
        .unwrap();
        keyword_edit(&dir, &[0], &["日出".to_owned()], &[]).unwrap();
        stack_create(&dir, &[0, 2]).unwrap();
        let proj = project::load(&project::project_path(&dir)).unwrap();
        assert_eq!(proj.ratings.get(&0), Some(&5));
        assert_eq!(proj.keywords.get(&0).unwrap(), &["日出".to_owned()]);
        assert_eq!(proj.stacks, vec![vec![0, 2]]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newer_project_is_never_overwritten_by_stacks() {
        let dir = temp_dir("stacks-version");
        let path = project::project_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!("{{\"version\":{},\"root\":\"x\"}}", project::VERSION + 1),
        )
        .unwrap();
        assert!(matches!(
            stack_create(&dir, &[0, 1]),
            Err(project::ProjectError::Version(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
