//! 插件体系：清单格式、注册表、菜单挂载点。
//!
//! 三件事在这里定死，后面再改代价很贵：
//!
//! 1. **插件分三类**（`Filter` / `Decoder` / `Export`），不是万能 trait。
//!    一次调色、一个 RAW 解码器、一个导出目标，三者的输入输出形状根本不同，
//!    硬塞进一个 `trait Plugin` 会得到一堆 `Option<&mut Frame>` 参数和
//!    "这类插件不支持这个能力"的运行时判断。
//! 2. **菜单由插件声明挂在哪，宿主不替它编**。`[menu] path = "plug/nik"` 是
//!    "挂在「插件」菜单的「Nik Collection」组下"。路径找不到父级就落进
//!    「其他」，绝不静默消失 —— 插件装了却看不见，是比装不上更糟的体验。
//! 3. **内置插件也走 manifest**（`source = "embedded"`）。Camera Raw 与 Nik
//!    是随包带的，但它们在注册表里和市场装来的插件是同一种东西，只是目录不同。
//!    一旦给内置插件开后门（绕过 manifest 直接写死菜单），市场装的同类插件
//!    就永远是二等公民，参数面板、禁用、崩溃计数都得写两套。
//!
//! 这里只管**元数据**，不管执行：宿主进程、tile 协议在 `aevum-ui` 侧另起。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 清单文件名。目录插件与内置插件都用它。
pub const MANIFEST: &str = "plugin.toml";

/// 插件类别。决定宿主怎么调它，也决定菜单允许挂在哪些根下。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// 拿一张图、出另一张图（Nik 全套、Camera Raw 的细节面板）。
    Filter,
    /// 把 RAW 字节解成线性图（Camera Raw、PureRAW）。
    Decoder,
    /// 把成片写出去（Exporter、打印、上传）。
    Export,
}

impl Kind {
    pub fn cn(self) -> &'static str {
        match self {
            Kind::Filter => "滤镜",
            Kind::Decoder => "解码器",
            Kind::Export => "导出",
        }
    }

    /// 这类插件允许挂的菜单根。解码器挂「文件」，滤镜挂「插件」，
    /// 导出挂「文件 · 导出」，其余一律归到「插件 · 其他」。
    pub fn allowed_roots(self) -> &'static [&'static str] {
        match self {
            Kind::Filter => &["plug"],
            Kind::Decoder => &["file", "plug"],
            Kind::Export => &["file", "plug"],
        }
    }
}

/// 插件从哪来。只影响权限与展示，不影响调用方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// 随程序发布，清单编译进二进制，目录在 `plugins/`。
    Embedded,
    /// 随安装包附带但独立成目录（Nik 这类第三方二进制）。
    #[default]
    Bundled,
    /// 用户从市场装的。
    User,
}

impl Source {
    pub fn cn(self) -> &'static str {
        match self {
            Source::Embedded => "内置",
            Source::Bundled => "随包",
            Source::User => "市场安装",
        }
    }
}

/// 一个参数。面板由它生成，插件自己不画 UI。
///
/// 这是"插件参数和宿主参数长得一样"的前提：`GradeParams` 的七十五个字段
/// 也走同一张表的形状（`min`/`max`/`default`/`decimals`/`unit`），于是
/// 插件面板不用为了"跟宿主不一致"再写一套控件。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    pub key: String,
    pub label: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// 小数位。0 = 整数档（对比度、饱和度），2 = 保留两位（曝光 EV）。
    #[serde(default)]
    pub decimals: usize,
    /// 单位后缀，空串表示没有。`%`、`EV`、`px`。
    #[serde(default)]
    pub unit: String,
}

impl ParamSpec {
    /// 值落到合法区间内。插件传来的数不可信，越界一律夹住。
    pub fn clamp(&self, v: f64) -> f64 {
        if v.is_nan() {
            return self.default;
        }
        v.clamp(self.min, self.max)
    }

    /// 按 `decimals` 格式化，签名与单位一起给（`+0.35 EV`、`72%`）。
    pub fn format(&self, v: f64) -> String {
        let mut s = format!("{:.*}", self.decimals, v);
        if self.decimals > 0 && v > 0.0 {
            s.insert(0, '+');
        }
        if self.unit.is_empty() {
            s
        } else if self.unit == "%" {
            format!("{s}%")
        } else {
            format!("{s} {}", self.unit)
        }
    }
}

/// 菜单挂载点。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    /// 斜杠分隔的路径，第一段是菜单根，最后一段是分组。
    /// `plug/nik` = 「插件」→「Nik Collection」。
    pub path: String,
    /// 菜单项文字。留空就用插件名。
    #[serde(default)]
    pub label: String,
}

/// 清单主体。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    /// 反向域名，全局唯一。市场靠它判断"这个是同一个插件的哪个版本"。
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub vendor: String,
    pub kind: Kind,
    /// 入口可执行文件的相对路径。内置解码器没有独立进程时留空。
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub source: Source,
    /// 一句话说明，菜单悬停时显示。
    #[serde(default)]
    pub description: String,
}

/// `plugin.toml` 的整体结构。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub plugin: Meta,
    pub menu: Menu,
    #[serde(default)]
    pub params: Params,
}

/// 参数表。TOML 里写作 `[[params.item]]`，读出来是 `params.item`。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Params {
    #[serde(default)]
    pub item: Vec<ParamSpec>,
}

impl Manifest {
    /// 解析一份清单文本。错误文案直接给人看（哪个字段、为什么不行）。
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Self = toml::from_str(text).map_err(|e| format!("清单格式不对：{e}"))?;
        m.validate()?;
        Ok(m)
    }

    /// 校验。宁可加载时就报错，也不要等到用户点了菜单才崩。
    pub fn validate(&self) -> Result<(), String> {
        let id = &self.plugin.id;
        if id.is_empty() || !id.contains('.') {
            return Err(format!("插件 id 要是反向域名（如 `nik.color-efex`），现在是 `{id}`"));
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        {
            return Err(format!("插件 id 只允许小写字母、数字、点和连字符：`{id}`"));
        }
        if self.plugin.name.trim().is_empty() {
            return Err("插件名不能为空".to_owned());
        }
        if self.menu.path.trim_matches('/').is_empty() {
            return Err("菜单路径不能为空；至少写一段，如 `plug`".to_owned());
        }
        if let Some(bad) = self
            .menu
            .path
            .split('/')
            .find(|s| s.trim().is_empty())
        {
            let _ = bad;
            return Err(format!("菜单路径有空段：`{}`", self.menu.path));
        }
        // 参数：key 唯一、范围非退化。重复 key 会让面板显示两个同名滑块，
        // 后写的那个悄悄覆盖前一个 —— 这种错最难查。
        let mut seen = std::collections::BTreeSet::new();
        for p in &self.params.item {
            if p.key.trim().is_empty() {
                return Err(format!("`{}` 里有一个参数没写 key", self.plugin.id));
            }
            if !seen.insert(p.key.as_str()) {
                return Err(format!("`{}` 里参数 key 重复：`{}`", self.plugin.id, p.key));
            }
            // partial_cmp 而不是 `!(a < b)`：NaN 时 partial_cmp 是 None、同样走报错，
            // 语义与旧写法一致，但读的人不用想浮点序的事。
            if !p.min.partial_cmp(&p.max).is_some_and(|o| o == std::cmp::Ordering::Less) {
                return Err(format!(
                    "参数 `{}` 的范围不成立：min {} ≥ max {}",
                    p.key, p.min, p.max
                ));
            }
            if p.default < p.min || p.default > p.max {
                return Err(format!(
                    "参数 `{}` 的默认值 {} 不在 [{}, {}] 内",
                    p.key, p.default, p.min, p.max
                ));
            }
            if p.decimals > 4 {
                return Err(format!("参数 `{}` 的小数位最多 4 位", p.key));
            }
        }
        Ok(())
    }

    /// 菜单实际挂载路径。父级不认识就落进 `plug/other`（见模块注释第 2 条）。
    pub fn menu_path(&self) -> String {
        let raw = self.menu.path.trim_matches('/');
        let mut parts = raw.split('/');
        let root = parts.next().unwrap_or("");
        let known = ["file", "edit", "develop", "view", "module", "plug", "help"];
        if !known.contains(&root) {
            return "plug/other".to_owned();
        }
        if !self.plugin.kind.allowed_roots().contains(&root) {
            // 滤镜挂到「文件」下会被当成文件操作，用户的预期完全错位。
            return format!("plug/{}", parts.next().unwrap_or("other"));
        }
        raw.to_owned()
    }

    /// 菜单项文字：没写标签就用插件名。
    pub fn menu_label(&self) -> &str {
        if self.menu.label.trim().is_empty() {
            &self.plugin.name
        } else {
            &self.menu.label
        }
    }
}

/// 注册表里的一项：清单 + 它从哪读来的 + 是否启用。
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub manifest: Manifest,
    /// 清单所在目录。`entry` 相对它解析。
    pub dir: PathBuf,
    pub enabled: bool,
}

impl Loaded {
    /// 入口可执行文件的绝对路径；没有入口（内置实现）返回 `None`。
    pub fn entry_path(&self) -> Option<PathBuf> {
        if self.manifest.plugin.entry.trim().is_empty() {
            None
        } else {
            Some(self.dir.join(self.manifest.plugin.entry.trim()))
        }
    }
}

/// 全部插件 + 加载期的错误。
///
/// 错误不吞：一个写坏的清单不该让整排插件消失，但也不能让它静默 ——
/// 用户会以为"我明明装了"。错误留在注册表里，由界面在插件菜单底部显示一条。
#[derive(Clone, Debug, Default)]
pub struct Registry {
    plugins: Vec<Loaded>,
    errors: Vec<(PathBuf, String)>,
}

/// 菜单树的一个节点：分组 + 组下的插件。
#[derive(Clone, Debug, PartialEq)]
pub struct MenuGroup {
    /// 分组名（路径第二段），界面自己翻成中文标题。
    pub group: String,
    pub items: Vec<usize>,
}

impl Registry {
    /// 扫一个目录下的全部插件：`<root>/<任意>/plugin.toml`。
    ///
    /// 只扫一层子目录，不递归：插件目录里可能有作者自己的资源树，
    /// 递归会把它当成插件再报一堆错。
    pub fn scan(root: &Path, source: Source) -> Self {
        let mut reg = Self::default();
        let Ok(entries) = std::fs::read_dir(root) else {
            return reg;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        // 目录序固定，菜单顺序才不会每次启动都跳。
        dirs.sort();
        for dir in dirs {
            let path = dir.join(MANIFEST);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match Manifest::parse(&text) {
                Ok(mut m) => {
                    // 目录里写的 source 只是建议，实际来源以调用方为准：
                    // 市场装进 `plugins/` 的插件不该自称 embedded。
                    m.plugin.source = source;
                    if reg.plugins.iter().any(|p| p.manifest.plugin.id == m.plugin.id) {
                        reg.errors
                            .push((path, format!("插件 id 重复：`{}`", m.plugin.id)));
                        continue;
                    }
                    reg.plugins.push(Loaded {
                        manifest: m,
                        dir,
                        enabled: true,
                    });
                }
                Err(e) => reg.errors.push((path, e)),
            }
        }
        reg
    }

    /// 内置插件：清单直接写在代码里，目录指到 `plugins/` 下。
    ///
    /// 它们和扫出来的插件走同一张表，唯一区别是没有 `plugin.toml` 文件读。
    pub fn with_builtin(mut self, builtin: Vec<Manifest>, root: &Path) -> Self {
        for m in builtin {
            if self.plugins.iter().any(|p| p.manifest.plugin.id == m.plugin.id) {
                // 市场装了同名插件：以文件为准（用户能自己更新版本），
                // 内置那份只当默认值。
                continue;
            }
            let dir = root.join(m.plugin.id.replace('.', "-"));
            self.plugins.push(Loaded {
                manifest: m,
                dir,
                enabled: true,
            });
        }
        self
    }

    /// 合并两个注册表（内置 + 用户）。后者优先，id 冲突时保留后者。
    pub fn merge(mut self, other: Registry) -> Self {
        for p in other.plugins {
            if let Some(slot) = self
                .plugins
                .iter_mut()
                .find(|q| q.manifest.plugin.id == p.manifest.plugin.id)
            {
                *slot = p;
            } else {
                self.plugins.push(p);
            }
        }
        self.errors.extend(other.errors);
        self
    }

    pub fn all(&self) -> &[Loaded] {
        &self.plugins
    }

    pub fn errors(&self) -> &[(PathBuf, String)] {
        &self.errors
    }

    pub fn get(&self, id: &str) -> Option<&Loaded> {
        self.plugins.iter().find(|p| p.manifest.plugin.id == id)
    }

    pub fn set_enabled(&mut self, id: &str, on: bool) {
        if let Some(p) = self.plugins.iter_mut().find(|p| p.manifest.plugin.id == id) {
            p.enabled = on;
        }
    }

    /// 按类别归类，市场的左栏用它。
    pub fn by_kind(&self) -> BTreeMap<&'static str, Vec<usize>> {
        let mut out: BTreeMap<&'static str, Vec<usize>> = BTreeMap::new();
        for (i, p) in self.plugins.iter().enumerate() {
            out.entry(p.manifest.plugin.kind.cn()).or_default().push(i);
        }
        out
    }

    /// 菜单树：把所有**启用**的插件按 `menu_path` 归组，返回按分组名排序的列表。
    ///
    /// 组内顺序按插件名排，不按加载顺序：用户装了两个插件，名字相邻才找得到。
    pub fn menu_tree(&self) -> Vec<MenuGroup> {
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, p) in self.plugins.iter().enumerate() {
            if !p.enabled {
                continue;
            }
            let path = p.manifest.menu_path();
            if path.starts_with("file") {
                // 文件类（解码器 / 导出）由各自的菜单直接列，不进插件树。
                continue;
            }
            let group = path.split('/').nth(1).unwrap_or("other").to_owned();
            groups.entry(group).or_default().push(i);
        }
        groups
            .into_iter()
            .map(|(group, mut items)| {
                items.sort_by(|a, b| {
                    self.plugins[*a]
                        .manifest
                        .plugin
                        .name
                        .cmp(&self.plugins[*b].manifest.plugin.name)
                });
                MenuGroup { group, items }
            })
            .collect()
    }

    /// 某类插件的全部条目（解码器列表、导出目标列表用）。
    pub fn of_kind(&self, kind: Kind) -> Vec<usize> {
        self.plugins
            .iter()
            .enumerate()
            .filter(|(_, p)| p.enabled && p.manifest.plugin.kind == kind)
            .map(|(i, _)| i)
            .collect()
    }
}

// ---------------------------------------------------------------- 内置清单

/// 随包插件的清单。
///
/// 写在这里而不是落成文件：它们是**程序的一部分**，版本跟程序一起走；
/// 落成 `plugin.toml` 反而会被人手改坏，改坏了又没人知道该恢复成什么。
/// 需要覆盖参数的高级用户走 `plugins/<id>/plugin.toml`，那一条路径是市场装的。
pub fn builtin() -> Vec<Manifest> {
    let mut out = Vec::new();

    // Camera Raw：解码器 + 细节面板。入口留空 —— 它由宿主内部实现，
    // 但注册表里跟第三方解码器同构，将来换成独立进程只改 entry。
    out.push(Manifest {
        plugin: Meta {
            id: "adobe.camera-raw".to_owned(),
            name: "Camera Raw".to_owned(),
            version: "17.0".to_owned(),
            vendor: "Adobe".to_owned(),
            kind: Kind::Decoder,
            entry: String::new(),
            source: Source::Embedded,
            description: "RAW 解码与细节处理：去马赛克、光学、降噪、锐化".to_owned(),
        },
        menu: Menu {
            path: "file/raw".to_owned(),
            label: "用 Camera Raw 重新解码…".to_owned(),
        },
        params: Params {
            item: vec![
                ParamSpec {
                    key: "sharpening".to_owned(),
                    label: "锐化".to_owned(),
                    min: 0.0,
                    max: 150.0,
                    default: 40.0,
                    decimals: 0,
                    unit: String::new(),
                },
                ParamSpec {
                    key: "noise_luminance".to_owned(),
                    label: "亮度降噪".to_owned(),
                    min: 0.0,
                    max: 100.0,
                    default: 0.0,
                    decimals: 0,
                    unit: String::new(),
                },
                ParamSpec {
                    key: "noise_color".to_owned(),
                    label: "颜色降噪".to_owned(),
                    min: 0.0,
                    max: 100.0,
                    default: 25.0,
                    decimals: 0,
                    unit: String::new(),
                },
            ],
        },
    });

    // Nik Collection 6：八支滤镜，同一个菜单组。
    let nik = [
        ("color-efex", "Color Efex Pro", "滤镜与调色：55 组预设、控制点"),
        ("silver-efex", "Silver Efex Pro", "黑白转换与胶片模拟"),
        ("analog-efex", "Analog Efex Pro", "模拟相机与胶片质感"),
        ("viveza", "Viveza", "局部亮度与色彩，控制点驱动"),
        ("dfine", "Dfine", "按图像自动分析的分区降噪"),
        ("hdr-efex", "HDR Efex Pro", "单张与包围曝光的 HDR 合成"),
        ("sharpener", "Sharpener Pro", "输出锐化与 RAW 预锐化"),
        ("perspective", "Perspective Efex", "透视校正与移轴"),
    ];
    for (slug, name, desc) in nik {
        out.push(Manifest {
            plugin: Meta {
                id: format!("nik.{slug}"),
                name: name.to_owned(),
                version: "6.3".to_owned(),
                vendor: "DxO".to_owned(),
                kind: Kind::Filter,
                // Nik 是货真价实的第三方二进制，必须走独立进程。
                entry: format!("bin/{slug}.exe"),
                source: Source::Bundled,
                description: desc.to_owned(),
            },
            menu: Menu {
                path: "plug/nik".to_owned(),
                label: format!("{name}…"),
            },
            params: Params { item: Vec::new() },
        });
    }

    // 计算与合成：这类是本项目自己实现的，但同样进注册表 ——
    // 界面不必为"内置功能"和"插件"写两套菜单。
    let compute = [
        ("aevum.hdr-merge", "HDR 合并", "plug/compute", "包围曝光合成 32 位线性"),
        ("aevum.pano", "全景拼接", "plug/compute", "多帧拼接与投影"),
        ("aevum.focus-stack", "焦点堆叠", "plug/compute", "景深合成"),
        ("aevum.denoise-ai", "AI 降噪", "plug/compute", "神经网络降噪"),
        ("aevum.lens-correct", "镜头校正", "plug/compute", "按机型匹配畸变与暗角"),
    ];
    for (id, name, path, desc) in compute {
        out.push(Manifest {
            plugin: Meta {
                id: id.to_owned(),
                name: name.to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                vendor: "Aevum".to_owned(),
                kind: Kind::Filter,
                entry: String::new(),
                source: Source::Embedded,
                description: desc.to_owned(),
            },
            menu: Menu {
                path: path.to_owned(),
                label: format!("{name}…"),
            },
            params: Params { item: Vec::new() },
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(key: &str, min: f64, max: f64, default: f64) -> ParamSpec {
        ParamSpec {
            key: key.to_owned(),
            label: key.to_owned(),
            min,
            max,
            default,
            decimals: 0,
            unit: String::new(),
        }
    }

    fn manifest(id: &str, kind: Kind, path: &str) -> Manifest {
        Manifest {
            plugin: Meta {
                id: id.to_owned(),
                name: id.to_owned(),
                version: "1.0".to_owned(),
                vendor: String::new(),
                kind,
                entry: String::new(),
                source: Source::User,
                description: String::new(),
            },
            menu: Menu {
                path: path.to_owned(),
                label: String::new(),
            },
            params: Params::default(),
        }
    }

    #[test]
    fn parses_a_real_manifest() {
        let text = r#"
[plugin]
id = "nik.color-efex"
name = "Color Efex Pro"
version = "6.3"
vendor = "DxO"
kind = "filter"
entry = "bin/color-efex.exe"
source = "bundled"
description = "滤镜与调色"

[menu]
path = "plug/nik"
label = "Color Efex Pro…"

[[params.item]]
key = "strength"
label = "强度"
min = 0.0
max = 100.0
default = 50.0
decimals = 0
unit = "%"
"#;
        let m = Manifest::parse(text).expect("应当解析成功");
        assert_eq!(m.plugin.kind, Kind::Filter);
        assert_eq!(m.menu_path(), "plug/nik");
        assert_eq!(m.menu_label(), "Color Efex Pro…");
        assert_eq!(m.params.item.len(), 1);
        assert_eq!(m.params.item[0].format(72.0), "72%");
    }

    #[test]
    fn rejects_broken_manifests_with_a_readable_reason() {
        // id 不是反向域名
        let mut m = manifest("color-efex", Kind::Filter, "plug/nik");
        assert!(m.validate().is_err());
        m.plugin.id = "nik.color-efex".to_owned();
        assert!(m.validate().is_ok());

        // 范围退化
        let mut m = manifest("nik.x", Kind::Filter, "plug/nik");
        m.params.item = vec![spec("k", 10.0, 10.0, 10.0)];
        assert!(m.validate().is_err());

        // 默认值越界
        let mut m = manifest("nik.x", Kind::Filter, "plug/nik");
        m.params.item = vec![spec("k", 0.0, 1.0, 2.0)];
        assert!(m.validate().is_err());

        // key 重复
        let mut m = manifest("nik.x", Kind::Filter, "plug/nik");
        m.params.item = vec![spec("k", 0.0, 1.0, 0.0), spec("k", 0.0, 1.0, 0.0)];
        assert!(m.validate().is_err());
    }

    #[test]
    fn unknown_menu_parent_falls_back_instead_of_disappearing() {
        let m = manifest("nik.x", Kind::Filter, "nothing/here");
        assert_eq!(m.menu_path(), "plug/other");
        // 滤镜挂「文件」下会被误读成文件操作，改挂到「插件」的同一组。
        let m = manifest("nik.x", Kind::Filter, "file/nik");
        assert_eq!(m.menu_path(), "plug/nik");
        // 解码器允许挂「文件」。
        let m = manifest("adobe.camera-raw", Kind::Decoder, "file/raw");
        assert_eq!(m.menu_path(), "file/raw");
    }

    #[test]
    fn menu_tree_sorts_groups_and_items() {
        let reg = Registry::default()
            .with_builtin(
                vec![
                    manifest("nik.zeta", Kind::Filter, "plug/nik"),
                    manifest("nik.alpha", Kind::Filter, "plug/nik"),
                    manifest("aevum.hdr", Kind::Filter, "plug/compute"),
                    manifest("adobe.camera-raw", Kind::Decoder, "file/raw"),
                ],
                Path::new("/plugins"),
            );
        let tree = reg.menu_tree();
        assert_eq!(tree.len(), 2);
        // BTreeMap 保证分组按名字排：compute 在 nik 前。
        assert_eq!(tree[0].group, "compute");
        assert_eq!(tree[1].group, "nik");
        // 组内按插件名排；解码器不进插件树。
        let names: Vec<&str> = tree[1]
            .items
            .iter()
            .map(|i| reg.all()[*i].manifest.plugin.name.as_str())
            .collect();
        assert_eq!(names, vec!["nik.alpha", "nik.zeta"]);
    }

    #[test]
    fn disabled_plugins_leave_the_menu_but_stay_registered() {
        let mut reg = Registry::default().with_builtin(
            vec![manifest("nik.alpha", Kind::Filter, "plug/nik")],
            Path::new("/plugins"),
        );
        assert_eq!(reg.menu_tree().len(), 1);
        reg.set_enabled("nik.alpha", false);
        assert!(reg.menu_tree().is_empty());
        assert!(reg.get("nik.alpha").is_some());
        assert!(reg.of_kind(Kind::Filter).is_empty());
    }

    #[test]
    fn scan_reports_bad_files_without_dropping_good_ones() {
        let root = std::env::temp_dir().join(format!("aevum-plug-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let good = root.join("good");
        let bad = root.join("bad");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            good.join(MANIFEST),
            "[plugin]\nid = \"nik.good\"\nname = \"Good\"\nversion = \"1\"\nkind = \"filter\"\n[menu]\npath = \"plug/nik\"\n",
        )
        .unwrap();
        std::fs::write(bad.join(MANIFEST), "[plugin]\nid = \"没写全\"\n").unwrap();

        let reg = Registry::scan(&root, Source::User);
        assert_eq!(reg.all().len(), 1, "坏清单不能连累好清单");
        assert_eq!(reg.errors().len(), 1);
        assert_eq!(reg.all()[0].manifest.plugin.id, "nik.good");
        // 扫出来的来源以调用方为准，不信目录里写的。
        assert_eq!(reg.all()[0].manifest.plugin.source, Source::User);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn builtin_registry_is_self_consistent() {
        let reg = Registry::default().with_builtin(builtin(), Path::new("/plugins"));
        // 内置清单必须全部通过校验：它们编译进二进制，坏一个就是产品带伤发布。
        for p in reg.all() {
            p.manifest
                .validate()
                .unwrap_or_else(|e| panic!("内置清单坏了：{e}"));
        }
        // 八支 Nik 落在同一组，Camera Raw 是解码器。
        let tree = reg.menu_tree();
        let nik = tree.iter().find(|g| g.group == "nik").expect("要有 nik 组");
        assert_eq!(nik.items.len(), 8);
        assert_eq!(reg.of_kind(Kind::Decoder).len(), 1);
        assert!(reg.errors().is_empty());
    }

    #[test]
    fn repo_sample_manifest_stays_parseable() {
        // 仓库里 `plugins/example-invert` 是给人抄的样板。样板一旦写坏，
        // 所有第三方作者都会照着错的那份抄，所以钉一条测试看着它。
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        if !dir.exists() {
            return; // 发布包里不带样例目录
        }
        let reg = Registry::scan(&dir, Source::User);
        assert!(reg.errors().is_empty(), "样例清单有错：{:?}", reg.errors());
        let p = reg.get("aevum.example-invert").expect("要能扫到样例插件");
        assert_eq!(p.manifest.menu_path(), "plug/example");
        assert_eq!(p.manifest.params.item.len(), 1);
    }

    #[test]
    fn param_format_signs_positive_decimals_only() {
        let mut p = spec("ev", -5.0, 5.0, 0.0);
        p.decimals = 2;
        p.unit = "EV".to_owned();
        assert_eq!(p.format(0.35), "+0.35 EV");
        assert_eq!(p.format(-1.2), "-1.20 EV");
        assert_eq!(p.format(0.0), "0.00 EV");
        // 整数档不加正号：面板上「+72%」比「72%」吵。
        let mut q = spec("pct", 0.0, 100.0, 0.0);
        q.unit = "%".to_owned();
        assert_eq!(q.format(72.0), "72%");
        // NaN 回落到默认值，不输出 "NaN"。
        assert_eq!(p.clamp(f64::NAN), 0.0);
        assert_eq!(p.clamp(99.0), 5.0);
    }
}
