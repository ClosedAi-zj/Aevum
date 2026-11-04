//! 平台交付预设：抖音 / 视频号 / 小红书 / B 站 / 母版的口径，一张表，改表不改码。
//!
//! 为什么要这一层：导出的四个数字（画幅、帧率、编码器、质量档）是**平台口径**，不是
//! 创作参数。用户记不住，agent 也不该猜；散在面板里让人一项项填，等于每次导出都重做
//! 一遍调研。把它们收进一张表，内置一份，用户在 `<配置目录>/delivery-presets.toml`
//! 里就能改——加平台、调质量档都不用重新编译。
//!
//! 为什么用 CRF 而不是固定码率：延时摄影的画面内容变化极大（云在走、光在变），固定码率
//! 对「静止三秒 + 一道闪电」这种镜头要么浪费要么糊。CRF 保的是质量下限，码率随内容走。
//! 平台真正的硬约束是**体积上限**，那是另一回事——这里不把它伪装成一个码率参数，免得
//! 面板上出现一个「设了也不保证」的旋钮。
//!
//! 为什么 `fps` 是 `Option`：帧率对延时摄影是**创作节奏**（24 帧的云和 30 帧的云不是同
//! 一种时间感）。预设里给 `Some(30)` 的只有那些确实按 30 帧分发的短视频平台；母版与长
//! 视频平台给 `None`，意思是「不碰你自己定的节奏」。
//!
//! Why this layer exists: the four numbers an export needs (frame size, rate, encoder,
//! quality) are **platform conventions**, not creative parameters. Users cannot be expected
//! to remember them and agents should not guess; scattering them across the panel means
//! redoing the same research on every export. They live in one table, shipped built in, and
//! the user can edit `<config dir>/delivery-presets.toml` -- adding a platform or retuning
//! the quality target needs no rebuild.
//!
//! Why CRF instead of a fixed bitrate: timelapse content varies wildly (clouds move, light
//! changes), and a fixed bitrate either wastes bits or smears the "three still seconds then a
//! lightning bolt" shot. CRF guarantees a quality floor and lets the bitrate follow the
//! content. The real platform constraint is a **size cap**, which is a different thing; this
//! module does not disguise it as a bitrate knob that cannot actually promise anything.
//!
//! Why `fps` is an `Option`: frame rate is **creative pacing** for timelapse (24 fps clouds
//! and 30 fps clouds are not the same sense of time). Only the short-video platforms that
//! really distribute at 30 fps carry `Some(30)`; masters and long-form platforms carry
//! `None`, meaning "do not touch the pacing you chose".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::export::{Codec, RenderOptions};

/// 一个平台交付口径。字段全部直接映射到 [`RenderOptions`]，没有隐藏行为。
///
/// One platform delivery convention. Every field maps straight onto [`RenderOptions`]; there
/// is no hidden behaviour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeliveryPreset {
    /// 稳定拼写（命令行与 MCP 用它）：`douyin-vertical`。改名字会断脚本，只加不改。
    ///
    /// Stable spelling used by the command line and MCP (`douyin-vertical`). Renaming breaks
    /// scripts, so keys are added, never renamed.
    pub key: String,
    pub name_cn: String,
    pub name_en: String,
    /// 精确输出画幅：等比 cover 放大后居中裁切，与面板上「4K / 1080p」同一口径。
    ///
    /// Exact output frame: cover-scale, then crop centered -- the same convention as the
    /// "4K / 1080p" choice in the panel.
    pub width: u32,
    pub height: u32,
    /// `None` = 保留用户当前的帧率（母版与长视频平台）；`Some(30)` = 平台按 30 帧分发。
    ///
    /// `None` keeps the user's current frame rate (masters, long-form platforms);
    /// `Some(30)` marks a platform that distributes at 30 fps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(with = "codec_text")]
    pub codec: Codec,
    /// 质量档（1~51，越小越好）。平台口径的差异主要体现在这里。
    ///
    /// Quality target (1-51, lower is better). Most of the platform-to-platform difference
    /// lives here.
    pub crf: u8,
    /// 编码器速度档（ffmpeg `-preset`）。默认 `medium`：再慢收益很小，再快画质掉得明显。
    ///
    /// Encoder speed preset (ffmpeg `-preset`). Defaults to `medium`: slower buys little,
    /// faster visibly costs quality.
    #[serde(default = "default_encoder_preset")]
    pub encoder_preset: String,
    #[serde(default)]
    pub hint_cn: String,
    #[serde(default)]
    pub hint_en: String,
}

fn default_encoder_preset() -> String {
    "medium".to_owned()
}

/// `Codec` 在 TOML 里写成 `"h264"` / `"h265"`：手写表的人不该被迫拼 Rust 枚举名。
///
/// `Codec` is spelled `"h264"` / `"h265"` in TOML: someone editing the table by hand should
/// not have to guess a Rust enum variant.
mod codec_text {
    use super::*;

    pub(super) fn serialize<S: Serializer>(codec: &Codec, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(codec.key())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Codec, D::Error> {
        let raw = String::deserialize(d)?;
        Codec::parse(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "不认识的编码器 {raw:?}：写 h264 或 h265 / Unknown codec {raw:?}: write h264 or h265"
            ))
        })
    }
}

impl DeliveryPreset {
    pub fn name(&self, chinese: bool) -> &str {
        if chinese {
            &self.name_cn
        } else {
            &self.name_en
        }
    }

    pub fn hint(&self, chinese: bool) -> &str {
        if chinese {
            &self.hint_cn
        } else {
            &self.hint_en
        }
    }

    /// 把整条口径写进渲染选项。**只动它负责的字段**，其余（裁切、拉直、运镜、水印、
    /// 色域、水印、配乐）一律不碰——预设是「交付口径」，不是「重置导出设置」。
    ///
    /// Writes the whole convention into the render options. It touches **only the fields it
    /// owns**; crop, straighten, camera move, watermark, colour space and audio are left
    /// alone. A preset is a delivery convention, not a reset button.
    pub fn apply_to(&self, options: &mut RenderOptions) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err(format!(
                "预设 {} 的画幅不能是 0×0 / preset {} declares a 0x0 frame",
                self.key, self.key
            ));
        }
        if let Some(fps) = self.fps {
            if !(fps.is_finite() && fps > 0.0) {
                return Err(format!(
                    "预设 {} 的帧率不合法（{fps}）/ preset {} has an invalid rate ({fps})",
                    self.key, self.key
                ));
            }
            options.fps = fps;
        }
        // `long_side` 与 `target_size` 同时给时以 `target_size` 为准，但留着旧的 `long_side`
        // 会让面板继续显示「按长边」的旧状态，所以显式清掉。
        //
        // `target_size` wins when both are set, but a leftover `long_side` keeps the panel
        // showing the old "by long side" state, so clear it explicitly.
        options.long_side = None;
        options.target_size = Some((self.width, self.height));
        options.codec = self.codec;
        options.crf = self.crf;
        options.preset = self.encoder_preset.clone();
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        if self.key.trim().is_empty() {
            return Err("预设缺 key / a preset is missing its key".to_owned());
        }
        if self.width == 0 || self.height == 0 {
            return Err(format!(
                "预设 {} 的画幅是 0 / preset {} has a 0 frame",
                self.key, self.key
            ));
        }
        // H.264 的 CRF 上限是 51；超出就是写错了，宁可在这里报出来，也不要等 ffmpeg 报。
        //
        // H.264 tops out at CRF 51; anything beyond is a typo and is better reported here
        // than by ffmpeg halfway through an encode.
        if !(1..=51).contains(&self.crf) {
            return Err(format!(
                "预设 {} 的 CRF {} 超出 1~51 / preset {} has CRF {} outside 1-51",
                self.key, self.crf, self.key, self.crf
            ));
        }
        if let Some(fps) = self.fps
            && !(fps.is_finite() && fps > 0.0)
        {
            return Err(format!(
                "预设 {} 的帧率不合法 / preset {} has an invalid rate",
                self.key, self.key
            ));
        }
        Ok(())
    }
}

/// 整张表。内置一份，文件覆盖，顺序稳定（面板按这个顺序排）。
///
/// The whole table: built in, overridden by a file, with a stable order (the panel sorts by
/// it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeliveryTable {
    #[serde(default)]
    pub presets: Vec<DeliveryPreset>,
}

impl DeliveryTable {
    /// 内置口径。数值来自各平台公开的交付建议，**是起点不是圣旨**：平台会改口径，
    /// 改的时候只需要编辑 `delivery-presets.toml`，不要来动这段代码。
    ///
    /// Built-in conventions, taken from each platform's published delivery guidance. They
    /// are **a starting point, not gospel**: platforms change, and when they do, edit
    /// `delivery-presets.toml` rather than this function.
    pub fn builtin() -> Self {
        let p = |key: &str,
                 name_cn: &str,
                 name_en: &str,
                 (w, h): (u32, u32),
                 fps: Option<f64>,
                 codec: Codec,
                 crf: u8,
                 hint_cn: &str,
                 hint_en: &str| DeliveryPreset {
            key: key.to_owned(),
            name_cn: name_cn.to_owned(),
            name_en: name_en.to_owned(),
            width: w,
            height: h,
            fps,
            codec,
            crf,
            encoder_preset: default_encoder_preset(),
            hint_cn: hint_cn.to_owned(),
            hint_en: hint_en.to_owned(),
        };
        Self {
            presets: vec![
                p(
                    "douyin-vertical",
                    "抖音竖屏",
                    "Douyin vertical",
                    (1080, 1920),
                    Some(30.0),
                    Codec::H264,
                    20,
                    "1080×1920 · 30fps · H.264 · CRF 20：竖屏短视频的通行口径",
                    "1080x1920, 30fps, H.264, CRF 20: the common short-video convention",
                ),
                p(
                    "channels-vertical",
                    "视频号竖屏",
                    "WeChat Channels vertical",
                    (1080, 1920),
                    Some(30.0),
                    Codec::H264,
                    20,
                    "1080×1920 · 30fps · H.264 · CRF 20：与抖音同画幅，便于一稿两投",
                    "1080x1920, 30fps, H.264, CRF 20: same frame as Douyin, one cut for both",
                ),
                p(
                    "xiaohongshu-vertical",
                    "小红书竖屏",
                    "Xiaohongshu vertical",
                    (1080, 1440),
                    Some(30.0),
                    Codec::H264,
                    20,
                    "1080×1440（3:4）· 30fps · H.264 · CRF 20：图文流里的竖版比例",
                    "1080x1440 (3:4), 30fps, H.264, CRF 20: the vertical ratio of the feed",
                ),
                p(
                    "bilibili-1080p",
                    "B 站 1080p",
                    "Bilibili 1080p",
                    (1920, 1080),
                    None,
                    Codec::H264,
                    18,
                    "1920×1080 · 保留你的节奏 · H.264 · CRF 18",
                    "1920x1080, keeps your pacing, H.264, CRF 18",
                ),
                p(
                    "bilibili-4k",
                    "B 站 4K",
                    "Bilibili 4K",
                    (3840, 2160),
                    None,
                    Codec::H265,
                    20,
                    "3840×2160 · 保留你的节奏 · H.265 · CRF 20：4K 用 HEVC 省一半码率",
                    "3840x2160, keeps your pacing, H.265, CRF 20: HEVC halves the bitrate at 4K",
                ),
                p(
                    "master-1080p",
                    "母版 1080p",
                    "Master 1080p",
                    (1920, 1080),
                    None,
                    Codec::H265,
                    16,
                    "1920×1080 · 保留你的节奏 · H.265 · CRF 16：给二次剪辑留余量",
                    "1920x1080, keeps your pacing, H.265, CRF 16: headroom for a second cut",
                ),
            ],
        }
    }

    /// 用户可编辑的表：`<配置目录>/delivery-presets.toml`。
    ///
    /// The user-editable table at `<config dir>/delivery-presets.toml`.
    pub fn path() -> PathBuf {
        crate::config_dir::dir().join("delivery-presets.toml")
    }

    /// 读用户表；没有文件用内置，文件坏了也退回内置（并喊一声，别静默）。
    ///
    /// Loads the user table. No file means built-in; a broken file also falls back to
    /// built-in, with one line on stderr rather than silence.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match Self::parse(&text) {
                Ok(table) => Self::builtin().merged_with(table),
                Err(e) => {
                    eprintln!(
                        "交付预设表读不懂，用内置口径：{}（{e}）\n\
                         Delivery preset table unreadable, using built-ins: {} ({e})",
                        path.display(),
                        path.display()
                    );
                    Self::builtin()
                }
            },
            // 没有文件是常态（绝大多数用户不改它），不是错误。
            //
            // A missing file is the normal case (most users never edit it), not an error.
            Err(_) => Self::builtin(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let table: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        for p in &table.presets {
            p.validate()?;
        }
        // 同 key 写两遍是表写错了：安静地取后者会让人以为前一条生效。
        //
        // Two entries with the same key is a table bug: silently keeping the latter makes
        // people believe the former is in effect.
        for (i, p) in table.presets.iter().enumerate() {
            if table.presets[..i].iter().any(|q| q.key == p.key) {
                return Err(format!("预设 key 重复：{} / duplicate preset key", p.key));
            }
        }
        Ok(table)
    }

    /// 内置打底，文件覆盖：同 key 整条替换，新 key 追加在末尾。
    ///
    /// Built-ins first, file on top: a matching key replaces the whole entry, a new key is
    /// appended.
    pub fn merged_with(self, over: Self) -> Self {
        let mut presets = self.presets;
        for p in over.presets {
            match presets.iter_mut().find(|q| q.key == p.key) {
                Some(slot) => *slot = p,
                None => presets.push(p),
            }
        }
        Self { presets }
    }

    pub fn get(&self, key: &str) -> Option<&DeliveryPreset> {
        self.presets.iter().find(|p| p.key == key)
    }

    pub fn keys(&self) -> Vec<&str> {
        self.presets.iter().map(|p| p.key.as_str()).collect()
    }

    /// 把当前表写成一份可编辑的 TOML（`--write-delivery-presets` 用，用户照着改）。
    ///
    /// Renders the current table as an editable TOML file (for
    /// `--write-delivery-presets`), so users have something to edit.
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = self
            .to_toml()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_table_is_valid_and_every_key_is_unique() {
        let table = DeliveryTable::builtin();
        assert!(
            table.presets.len() >= 5,
            "内置口径太少：{}",
            table.presets.len()
        );
        for p in &table.presets {
            p.validate()
                .unwrap_or_else(|e| panic!("内置预设不合法：{e}"));
        }
        let mut keys = table.keys();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "内置表里有重复 key");
    }

    /// 竖屏短视频：四个数字一个不许错，且不碰用户自己的创作设置。
    ///
    /// The vertical short-video preset: every one of the four numbers exact, and none of the
    /// user's creative settings touched.
    #[test]
    fn the_vertical_preset_writes_the_numbers_it_declares_and_nothing_else() {
        let table = DeliveryTable::builtin();
        let p = table.get("douyin-vertical").expect("内置表必须有抖音竖屏");
        let mut options = RenderOptions {
            // 故意给一组非默认的创作设置：预设不该把它们一起改掉。
            //
            // Deliberately non-default creative settings: the preset must leave them alone.
            crop: None,
            rotate: 3.5,
            fps: 12.0,
            long_side: Some(2560),
            codec: Codec::H265,
            crf: 30,
            preset: "slow".to_owned(),
            ..RenderOptions::default()
        };

        p.apply_to(&mut options).expect("内置预设必须能应用");

        assert_eq!(options.target_size, Some((1080, 1920)));
        assert_eq!(options.long_side, None, "旧的按长边缩放必须被清掉");
        assert_eq!(options.fps, 30.0);
        assert_eq!(options.codec, Codec::H264);
        assert_eq!(options.crf, 20);
        assert_eq!(options.preset, "medium");
        // 不属于交付口径的字段原样留着。
        //
        // Fields outside the delivery convention stay as they were.
        assert_eq!(options.rotate, 3.5, "拉直角度不是交付口径，不许被预设改掉");
    }

    /// 母版预设 `fps: None`：能改画幅，但**不动**用户定的节奏。
    ///
    /// The master preset's `fps: None` changes the frame but leaves the user's pacing alone.
    #[test]
    fn a_preset_without_a_rate_keeps_the_users_pacing() {
        let table = DeliveryTable::builtin();
        let p = table.get("master-1080p").expect("内置表必须有母版");
        let mut options = RenderOptions {
            fps: 12.0,
            ..RenderOptions::default()
        };
        p.apply_to(&mut options).unwrap();
        assert_eq!(options.fps, 12.0, "fps=None 的预设不许碰用户的帧率");
        assert_eq!(options.target_size, Some((1920, 1080)));
    }

    /// 改表不改码：文件里的同 key 覆盖数值、新 key 追加，内置项仍在。
    ///
    /// Change the table, not the code: a matching key overrides its numbers, a new key is
    /// appended, and the built-ins stay.
    #[test]
    fn a_file_overrides_numbers_appends_new_keys_and_keeps_builtins() {
        let text = r#"
[[presets]]
key = "douyin-vertical"
name_cn = "抖音竖屏（我的）"
name_en = "Douyin vertical (mine)"
width = 1080
height = 1920
fps = 60.0
codec = "h265"
crf = 17

[[presets]]
key = "my-own"
name_cn = "我的平台"
name_en = "My platform"
width = 1440
height = 1080
codec = "h264"
crf = 19
"#;
        let over = DeliveryTable::parse(text).expect("表必须能解析");
        let table = DeliveryTable::builtin().merged_with(over);

        let mine = table.get("douyin-vertical").unwrap();
        assert_eq!(mine.crf, 17, "文件里的数值必须覆盖内置");
        assert_eq!(mine.fps, Some(60.0));
        assert_eq!(mine.codec, Codec::H265);
        assert_eq!(mine.encoder_preset, "medium", "缺省字段走默认值");

        let own = table.get("my-own").expect("新 key 必须被追加");
        assert_eq!((own.width, own.height), (1440, 1080));
        assert_eq!(own.fps, None, "没写 fps 就是 None（不动节奏）");

        assert!(
            table.get("bilibili-4k").is_some(),
            "没被覆盖的内置项必须还在"
        );
        // 顺序：内置项按原序，新项追加在末尾。
        //
        // Order: built-ins keep their order, new entries go last.
        assert_eq!(table.presets.last().unwrap().key, "my-own");
        assert_eq!(table.presets[0].key, "douyin-vertical");
    }

    /// 读不懂的表不许静默生效半张：退回内置（这条判据咬的是「不 panic 也不空表」）。
    ///
    /// An unreadable table must not half-apply: fall back to built-ins (this bites "does not
    /// panic and is not empty").
    #[test]
    fn a_broken_file_falls_back_to_the_builtin_table() {
        let dir = std::env::temp_dir().join(format!("aevum-delivery-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("delivery-presets.toml");
        std::fs::write(&path, "[[presets]\nkey = ").unwrap();

        let table = DeliveryTable::load_from(&path);
        assert!(
            table.get("douyin-vertical").is_some(),
            "坏表必须退回内置口径，不能是空表"
        );

        // 语义错误（CRF 越界）同样退回内置，而不是带着错值跑。
        //
        // A semantic error (CRF out of range) also falls back instead of running with it.
        std::fs::write(
            &path,
            "[[presets]]\nkey=\"x\"\nname_cn=\"x\"\nname_en=\"x\"\nwidth=1\nheight=1\ncodec=\"h264\"\ncrf=99\n",
        )
        .unwrap();
        let table = DeliveryTable::load_from(&path);
        assert!(table.get("x").is_none(), "CRF 越界的预设不许进表");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 写出去的模板必须能被自己读回来——否则「照着改」就是一个陷阱。
    ///
    /// The written template must parse back; otherwise "edit this file" is a trap.
    #[test]
    fn the_written_template_round_trips() {
        let builtin = DeliveryTable::builtin();
        let text = builtin.to_toml().expect("内置表必须能序列化");
        let back = DeliveryTable::parse(&text).expect("自己写的模板必须能读回来");
        assert_eq!(back, builtin, "序列化再解析必须逐字段相等");
    }
}
