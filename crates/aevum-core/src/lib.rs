//! 恒晷 · Aevum Studio 核心库。
//!
//! 这里只放与界面无关的东西：序列发现、逐帧测光、去闪烁、曲线与调色参数。
//! 所有数值口径都以 `acceptance/oracle/golden` 里冻结的 Python 实现为基准，
//! 改算法先跑 `cargo test -p aevum-core --test golden`。
//!
//! The core library of Aevum Studio.
//!
//! This crate holds only what is independent of the user interface: sequence discovery,
//! per-frame light metering, deflicker, curves, and grading parameters.
//! All numeric conventions follow the Python implementation frozen in `acceptance/oracle/golden`;
//! before changing any algorithm, run `cargo test -p aevum-core --test golden` first.

pub mod ai_denoise;
pub mod align;
pub mod audio;
pub mod auto;
pub mod brand;
pub mod colorspace;
pub mod config_dir;
pub mod crop;
pub mod cube;
pub mod cull;
pub mod dcp;
pub mod deflicker;
pub mod denoise_chroma_weights;
pub mod denoise_weights;
pub mod develop;
pub mod dng;
pub mod env;
pub mod errmsg;
pub mod exif;
pub mod export;
pub mod focus;
pub mod fonts;
pub mod grade;
pub mod hdr;
pub mod hdr_output;
pub mod interp;
pub mod journal;
pub mod lcp;
pub mod lensdb;
pub mod library;
pub mod mask;
pub mod mcp_routes;
pub mod measures;
pub mod metrics;
pub mod pano;
pub mod paths;
pub mod plugin;
pub mod plugin_host;
pub mod preview;
pub mod project;
pub mod qa;
pub mod queue;
pub mod raw;
pub mod retime;
pub mod sequence;
pub mod stack;
pub mod temp_track;
pub mod timeblend;
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
pub use preview::{
    FOLD_KNEE, fold_above_white_in_place, render as render_preview,
    render_hdr as render_preview_hdr, render_hdr_sketch as render_preview_hdr_sketch,
    render_sketch,
};
pub use project::ProjectFile;
pub use sequence::{Frame, Sequence};
