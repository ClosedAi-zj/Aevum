//! 配乐混流：长度算式与 ffmpeg 音频滤镜链。
//!
//! 口径对齐 Python `acceptance/reference/audio.py`：延迟用 `adelay` 补真静音采样，
//! 循环用 `-stream_loop`，淡入淡出与音量走滤镜；序列本身没有原声，
//! 所以配乐是替换式的，不做双路混合。
//!
//! Music muxing: duration arithmetic and the ffmpeg audio filter chain.
//!
//! The convention matches Python `acceptance/reference/audio.py`: `adelay` pads with true
//! silent samples, `-stream_loop` handles looping, and fades and volume go through filters.
//! The sequence itself carries no production sound, so the music is a replacement rather than
//! a two-path mix.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 成片容器里能原样放的音频编码；其余得转 AAC。
const COPYABLE: [&str; 3] = ["aac", "mp3", "alac"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    #[default]
    Auto,
    Copy,
    Aac128,
    Aac192,
    Aac256,
    Aac320,
}

impl AudioCodec {
    /// AAC 目标码率；`Auto` / `Copy` 没有固定码率。
    ///
    /// AAC target bitrate; `Auto` and `Copy` have no fixed bitrate.
    pub fn bitrate(self) -> Option<&'static str> {
        match self {
            AudioCodec::Aac128 => Some("128k"),
            AudioCodec::Aac192 => Some("192k"),
            AudioCodec::Aac256 => Some("256k"),
            AudioCodec::Aac320 => Some("320k"),
            AudioCodec::Auto | AudioCodec::Copy => None,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "copy" => Some(Self::Copy),
            "aac128" => Some(Self::Aac128),
            "aac192" => Some(Self::Aac192),
            "aac256" => Some(Self::Aac256),
            "aac320" => Some(Self::Aac320),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioOptions {
    /// 相对画面起点延后进入（秒）。
    ///
    /// Delay before the music enters, relative to the start of the picture (seconds).
    pub delay: f64,
    /// 配乐音量百分比，100 为原始。
    ///
    /// Music volume percentage, where 100 is the original level.
    pub gain: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    /// 配乐比画面短时循环铺满。
    ///
    /// Loop the music to fill the picture when the music is shorter.
    #[serde(rename = "loop")]
    pub looped: bool,
    /// 配乐比画面长时截到画面结束。
    ///
    /// Trim the music at the end of the picture when the music is longer.
    pub trim: bool,
    pub acodec: AudioCodec,
}

impl Default for AudioOptions {
    fn default() -> Self {
        Self {
            delay: 0.0,
            gain: 100.0,
            fade_in: 0.0,
            fade_out: 0.0,
            looped: false,
            trim: true,
            acodec: AudioCodec::Auto,
        }
    }
}

impl AudioOptions {
    /// 滤镜链是否需要重编音频：延后、改音量、淡入淡出都算。
    ///
    /// Whether the filter chain requires re-encoding the audio: a delay, a volume change, or a
    /// fade all count.
    pub fn needs_filter(&self) -> bool {
        self.delay > 0.0
            || (self.gain - 100.0).abs() > 0.5
            || self.fade_in > 0.0
            || self.fade_out > 0.0
    }

    /// 配乐在成片里收尾的时刻（输出时间轴，秒）。
    ///
    /// The moment the music finishes in the final render, on the output timeline in seconds.
    pub fn music_end(&self, video_dur: f64, audio_dur: f64) -> f64 {
        if self.looped {
            return video_dur;
        }
        let end = self.delay + audio_dur;
        if self.trim { end.min(video_dur) } else { end }
    }

    /// 成片时长：不截断时配乐可能比画面长，尾部只剩声音。
    ///
    /// Duration of the final render: without trimming the music may outlast the picture, leaving
    /// only sound at the tail.
    pub fn total_dur(&self, video_dur: f64, audio_dur: f64) -> f64 {
        video_dur.max(self.music_end(video_dur, audio_dur))
    }

    /// 该给 `-t` 的时长；循环必须限长，否则配乐无限铺下去。
    /// 不循环也不截断时由流自身结束，不需要 `-t`。
    ///
    /// The duration to pass to `-t`; a loop must be bounded or the music plays on without end.
    /// When neither looping nor trimming is set, the stream ends on its own and `-t` is
    /// unnecessary.
    pub fn limit_dur(&self, video_dur: f64, _audio_dur: f64) -> Option<f64> {
        (self.looped || self.trim).then_some(video_dur)
    }

    /// 这条音频能不能一个比特都不改地塞进目标容器。
    ///
    /// Whether this audio stream fits into the target container without a single bit changed.
    pub fn copy_ok(&self, out_ext: &str, codec: &str) -> bool {
        if self.needs_filter() {
            return false;
        }
        if out_ext.eq_ignore_ascii_case("mkv") {
            return true;
        }
        let codec = codec.to_ascii_lowercase();
        COPYABLE.iter().any(|c| codec.starts_with(c))
    }

    /// 滤镜链与最终映射的音频标签；`None` 表示不需要滤镜。
    ///
    /// 画面来自 PNG 帧、没有原声，所以这里只处理配乐这一路。
    ///
    /// The filter chain and the audio label of the final mapping; `None` means no filter is
    /// needed.
    ///
    /// The picture comes from PNG frames and carries no production sound, so only the music path
    /// is handled here.
    pub fn filter(&self, video_dur: f64, audio_dur: f64) -> Option<AudioFilter> {
        if !self.needs_filter() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        if self.delay > 0.0 {
            parts.push(format!(
                "adelay={}:all=1",
                (self.delay * 1000.0).round() as i64
            ));
        }
        if (self.gain - 100.0).abs() > 0.5 {
            parts.push(format!("volume={:.4}", self.gain / 100.0));
        }
        if self.fade_in > 0.0 {
            parts.push(format!(
                "afade=t=in:st={:.3}:d={:.3}",
                self.delay, self.fade_in
            ));
        }
        if self.fade_out > 0.0 {
            // 淡出落在配乐真正收尾处；循环/截断会把落点拉到视频末尾。
            let st = (self.music_end(video_dur, audio_dur) - self.fade_out).max(self.delay);
            parts.push(format!("afade=t=out:st={st:.3}:d={:.3}", self.fade_out));
        }
        let chain = if parts.is_empty() {
            "anull".to_owned()
        } else {
            parts.join(",")
        };
        Some(AudioFilter {
            complex: format!("[1:a]{chain}[mu]"),
            label: "[mu]".to_owned(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFilter {
    /// 传给 `-filter_complex` 的字符串。
    ///
    /// The string passed to `-filter_complex`.
    pub complex: String,
    /// 映射到输出音轨的标签。
    ///
    /// The label mapped onto the output audio track.
    pub label: String,
}

/// 配乐探针结果：编码名与时长（秒）。
///
/// Music probe result: codec name and duration in seconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioProbe {
    pub codec: String,
    pub duration: f64,
}

impl AudioProbe {
    pub fn has_stream(&self) -> bool {
        !self.codec.is_empty()
    }
}

/// 解析 `ffmpeg -hide_banner -i <file>` 的 stderr。
///
/// 与 Python `ff.probe_audio` 同口径：认 `Audio: <codec>` 与 `Duration: h:m:s`。
///
/// Parses the stderr of `ffmpeg -hide_banner -i <file>`.
///
/// The convention matches Python `ff.probe_audio`: `Audio: <codec>` and `Duration: h:m:s` are
/// recognized.
pub fn parse_probe(stderr: &str) -> AudioProbe {
    let mut probe = AudioProbe::default();
    for line in stderr.lines() {
        let line = line.trim();
        if probe.codec.is_empty()
            && let Some(rest) = line.split_once("Audio: ").map(|(_, v)| v)
        {
            let codec: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if !codec.is_empty() {
                probe.codec = codec;
            }
        }
        if probe.duration == 0.0
            && let Some(rest) = line.split_once("Duration:").map(|(_, v)| v.trim())
            && let Some(text) = rest.split(',').next()
        {
            probe.duration = parse_duration(text);
        }
    }
    probe
}

fn parse_duration(text: &str) -> f64 {
    let mut parts = text.trim().split(':');
    let (Some(h), Some(m), Some(s)) = (parts.next(), parts.next(), parts.next()) else {
        return 0.0;
    };
    let h: f64 = h.trim().parse().unwrap_or(0.0);
    let m: f64 = m.trim().parse().unwrap_or(0.0);
    let s: f64 = s.trim().parse().unwrap_or(0.0);
    if !(h.is_finite() && m.is_finite() && s.is_finite()) {
        return 0.0;
    }
    h * 3600.0 + m * 60.0 + s
}

/// 配乐文件扩展名是否在支持列表里。
///
/// Whether the music file extension is in the supported list.
pub fn is_audio_path(path: &Path) -> bool {
    const EXTS: [&str; 11] = [
        "mp3", "wav", "aac", "m4a", "m4b", "flac", "ogg", "opus", "wma", "aif", "aiff",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_do_not_need_a_filter() {
        let opt = AudioOptions::default();
        assert!(!opt.needs_filter());
        assert!(opt.filter(10.0, 10.0).is_none());
        assert_eq!(opt.limit_dur(10.0, 10.0), Some(10.0));
        assert_eq!(opt.total_dur(10.0, 10.0), 10.0);
    }

    #[test]
    fn delay_and_fades_go_into_the_chain() {
        let opt = AudioOptions {
            delay: 1.5,
            gain: 80.0,
            fade_in: 2.0,
            fade_out: 3.0,
            ..Default::default()
        };
        let filter = opt.filter(10.0, 8.0).expect("需要滤镜");
        assert_eq!(
            filter.complex,
            "[1:a]adelay=1500:all=1,volume=0.8000,afade=t=in:st=1.500:d=2.000,afade=t=out:st=6.500:d=3.000[mu]"
        );
        assert_eq!(filter.label, "[mu]");
    }

    #[test]
    fn music_end_follows_loop_and_trim() {
        let mut opt = AudioOptions::default();
        assert_eq!(opt.music_end(10.0, 4.0), 4.0);
        assert_eq!(opt.music_end(10.0, 16.0), 10.0);
        opt.trim = false;
        assert_eq!(opt.music_end(10.0, 16.0), 16.0);
        assert_eq!(opt.total_dur(10.0, 16.0), 16.0);
        assert_eq!(opt.limit_dur(10.0, 16.0), None);
        opt.looped = true;
        assert_eq!(opt.music_end(10.0, 4.0), 10.0);
        assert_eq!(opt.limit_dur(10.0, 4.0), Some(10.0));
    }

    #[test]
    fn loop_with_fade_out_lands_at_video_end() {
        let opt = AudioOptions {
            looped: true,
            fade_out: 2.0,
            ..Default::default()
        };
        let filter = opt.filter(10.0, 3.0).expect("需要滤镜");
        assert!(filter.complex.contains("afade=t=out:st=8.000:d=2.000"));
    }

    #[test]
    fn copy_only_without_filters() {
        let opt = AudioOptions::default();
        assert!(opt.copy_ok("mp4", "aac"));
        assert!(opt.copy_ok("mkv", "flac"));
        assert!(!opt.copy_ok("mp4", "flac"));
        assert!(opt.copy_ok("MP4", "mp3"));
        let faded = AudioOptions {
            fade_in: 1.0,
            ..Default::default()
        };
        assert!(!faded.copy_ok("mkv", "aac"));
    }

    #[test]
    fn probe_parser_reads_codec_and_duration() {
        let stderr = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'a.m4a':\n  \
             Duration: 00:01:02.50, start: 0.000000, bitrate: 128 kb/s\n    \
             Stream #0:0(und): Audio: aac (LC) (mp4a / 0x6134706D), 44100 Hz, stereo, fltp, 127 kb/s";
        let probe = parse_probe(stderr);
        assert_eq!(probe.codec, "aac");
        assert!((probe.duration - 62.5).abs() < 1e-9);
        assert!(probe.has_stream());
        assert!(!parse_probe("photo.png: Invalid data").has_stream());
    }

    #[test]
    fn duration_parser_rejects_garbage() {
        assert_eq!(parse_duration("N/A"), 0.0);
        assert_eq!(parse_duration("00:00:01.250"), 1.25);
        assert_eq!(parse_duration("01:02:03.5"), 3723.5);
    }
}
