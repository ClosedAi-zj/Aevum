//! 光时 · Lumen Studio 核心库。
//!
//! 这里只放与界面无关的东西：序列发现、逐帧测光、去闪烁、曲线与调色参数。
//! 所有数值口径都以 `oracle/golden` 里冻结的 Python 实现为基准，
//! 改算法先跑 `cargo test -p lumen-core --test golden`。

pub mod ai_denoise;
pub mod align;
pub mod audio;
pub mod auto;
pub mod colorspace;
pub mod crop;
pub mod cube;
pub mod dcp;
pub mod deflicker;
pub mod denoise_chroma_weights;
pub mod denoise_weights;
pub mod develop;
pub mod dng;
pub mod exif;
pub mod export;
pub mod fonts;
pub mod grade;
pub mod hdr;
pub mod interp;
pub mod journal;
pub mod lcp;
pub mod lensdb;
pub mod library;
pub mod mask;
pub mod measures;
pub mod metrics;
pub mod pano;
pub mod plugin;
pub mod plugin_host;
pub mod preview;
pub mod project;
pub mod qa;
pub mod queue;
pub mod raw;
pub mod retime;
pub mod sequence;
pub mod temp_track;
pub mod warp;
pub mod watermark;
pub mod xmp;
pub mod xmp_map;

pub use align::{AlignOptions, AlignReport, CropRect, Estimator};
pub use audio::{AudioCodec, AudioOptions};
pub use auto::{AutoParams, auto_params};
pub use deflicker::{Analysis, FrameMeans, Stats};
pub use develop::Ev;
pub use export::{OutputMode, RenderOptions};
pub use metrics::FrameMetrics;
pub use preview::{render as render_preview, render_sketch};
pub use project::ProjectFile;
pub use sequence::{Frame, Sequence};
