//! MCP 会话状态。
//!
//! 一个进程只有一份会话，请求在 stdio 循环里串行处理，所以这里不需要复杂的
//! 并发保护；外层用 `Mutex` 是为了让 `dispatch` 的签名不暴露可变全局。
//!
//! 耗时的测光/对齐/导出既可以同步跑，也可以带 `background: true` 丢到后台线程。
//! 后台任务通过 [`JobShared`] 里的原子量与调用方共享进度和取消标志；同步任务也
//! 复用同一个槽位，这样 `aevum_align_status` 在任务结束后仍能看到上次的进度。
//!
//! MCP session state.
//!
//! One process holds exactly one session and the stdio loop handles requests one at
//! a time, so no elaborate concurrency protection is needed here; the outer `Mutex`
//! exists so `dispatch` does not expose a mutable global in its signature.
//!
//! Time-consuming measuring / aligning / exporting can run synchronously, or be
//! thrown onto a background thread with `background: true`. Background jobs share
//! progress and the cancel flag with the caller through the atomics in
//! [`JobShared`]; synchronous jobs reuse the same slot, so `aevum_align_status` can
//! still report the previous run after a job ends.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use aevum_core::align::{AlignReport, CropRect};
use aevum_core::deflicker::{Analysis, FrameMeans};
use aevum_core::develop::Ev;
use aevum_core::grade::GradeParams;
use aevum_core::interp;
use aevum_core::metrics::{self, FrameMetrics};
use aevum_core::project::{self, AlignState, ProjectFile};
use aevum_core::queue::{self, QueueItem, QueueStatus};
use aevum_core::sequence::Sequence;

/// 测光缩略图的长边，和 GUI 的加载路径保持一致，数值可直接对照。
///
/// Long side of the measurement thumbnail, matching the GUI loading path so the
/// numbers can be compared directly.
pub const MEASURE_LONG_SIDE: u32 = 512;
/// 超过这个长边的写操作要显式 confirm。
///
/// Write operations above this long side need an explicit confirm.
pub const MAX_LONG_SIDE: u32 = 4096;
/// 逐帧数组只回传这么多点，避免把几十兆 JSON 灌给 agent。
///
/// Return at most this many points per frame array, so tens of megabytes of JSON
/// are not dumped at an agent.
pub const MAX_SERIES: usize = 64;

/// 任务阶段，和 `export::Stage` 对齐后再加一个「空闲」。
///
/// Job stage; aligned with `export::Stage` plus one extra step for idle.
pub const STAGE_IDLE: u8 = 0;
pub const STAGE_FRAMES: u8 = 1;
pub const STAGE_ENCODE: u8 = 2;

/// 后台/同步任务的类型。
///
/// The kind of background or synchronous job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Align,
    Measure,
    Export,
    Queue,
}

impl JobKind {
    pub fn code(self) -> u8 {
        match self {
            JobKind::Align => 1,
            JobKind::Measure => 2,
            JobKind::Export => 3,
            JobKind::Queue => 4,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(JobKind::Align),
            2 => Some(JobKind::Measure),
            3 => Some(JobKind::Export),
            4 => Some(JobKind::Queue),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            JobKind::Align => "align",
            JobKind::Measure => "measure",
            JobKind::Export => "export",
            JobKind::Queue => "queue",
        }
    }
}

/// 任务完成后的产物。后台线程只负责写进 [`JobShared`]，
/// 由下一次请求在 [`Session::poll_job`] 里落到会话状态。
///
/// What a finished job produces. The background thread only writes into
/// [`JobShared`]; the next request commits it to the session in
/// [`Session::poll_job`].
pub enum JobOutcome {
    Align {
        report: AlignReport,
        reference: usize,
        estimator: &'static str,
    },
    Measure {
        metrics: Vec<Option<FrameMetrics>>,
        def_means: Vec<Option<FrameMeans>>,
        issues: Vec<(usize, String)>,
    },
    Export {
        path: PathBuf,
        summary: serde_json::Value,
    },
    /// 批量队列：逐段状态按索引回填到 `Session::queue`。
    ///
    /// Batch queue: per-segment status is written back into `Session::queue` by
    /// index.
    Queue {
        rows: Vec<(usize, QueueStatus, String)>,
        ok: usize,
        failed: usize,
        cancelled: bool,
    },
}

/// 跨线程共享的任务槽：进度、取消标志和最终结果。
///
/// 同步任务也走这里，只是没有后台线程；`running` 在任务结束后归零，
/// 原子量保留最后一次的值供 `aevum_align_status` 展示。
///
/// The cross-thread job slot: progress, cancel flag and final result.
///
/// Synchronous jobs go through it too, only without a background thread;
/// `running` drops back to zero when a job ends while the atomics keep the last
/// values around for `aevum_align_status` to display.
pub struct JobShared {
    kind: AtomicU8,
    running: AtomicBool,
    cancel: AtomicBool,
    done: AtomicUsize,
    total: AtomicUsize,
    stage: AtomicU8,
    /// 每 `begin` 一次加一；进度通知用它判断这次调用有没有真的起过任务。
    ///
    /// Incremented on every `begin`; progress notifications use it to tell
    /// whether this call actually started a job.
    serial: AtomicU64,
    elapsed_ms: AtomicU64,
    started: Mutex<Option<Instant>>,
    outcome: Mutex<Option<Result<JobOutcome, String>>>,
}

impl JobShared {
    pub fn new() -> Self {
        Self {
            kind: AtomicU8::new(0),
            running: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            done: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            stage: AtomicU8::new(STAGE_IDLE),
            serial: AtomicU64::new(0),
            elapsed_ms: AtomicU64::new(0),
            started: Mutex::new(None),
            outcome: Mutex::new(None),
        }
    }

    /// 开始一个新任务：清掉上一轮的取消标志和结果。同步、后台都从这里起步。
    ///
    /// Start a new job: clear the previous cancel flag and result. Both
    /// synchronous and background jobs start here.
    pub fn begin(&self, kind: JobKind, total: usize) {
        self.kind.store(kind.code(), Ordering::SeqCst);
        self.serial.fetch_add(1, Ordering::SeqCst);
        self.cancel.store(false, Ordering::SeqCst);
        self.done.store(0, Ordering::SeqCst);
        self.total.store(total, Ordering::SeqCst);
        self.stage.store(STAGE_IDLE, Ordering::SeqCst);
        self.elapsed_ms.store(0, Ordering::SeqCst);
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        *self.outcome.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.running.store(true, Ordering::SeqCst);
    }

    pub fn progress(&self, done: usize, total: usize) {
        self.done.store(done, Ordering::SeqCst);
        self.total.store(total, Ordering::SeqCst);
    }

    pub fn set_stage(&self, stage: u8, done: usize, total: usize) {
        self.stage.store(stage, Ordering::SeqCst);
        self.progress(done, total);
    }

    /// 正常结束。`outcome` 已经由调用方直接落到会话，不在这里重复保存。
    ///
    /// Finish successfully. The caller has already committed `outcome` to the
    /// session, so it is not stored again here.
    pub fn finish_ok(&self) {
        self.record_elapsed();
        self.running.store(false, Ordering::SeqCst);
    }

    /// 后台线程的结束口：结果留给下一次 `poll_job` 取走。
    ///
    /// The exit for a background thread: the result is left for the next
    /// `poll_job` to pick up.
    pub fn finish(&self, result: Result<JobOutcome, String>) {
        self.record_elapsed();
        *self.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        self.running.store(false, Ordering::SeqCst);
    }

    fn record_elapsed(&self) {
        let elapsed = self
            .started
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0);
        self.elapsed_ms.store(elapsed, Ordering::SeqCst);
    }

    pub fn take_outcome(&self) -> Option<Result<JobOutcome, String>> {
        self.outcome
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    pub fn kind(&self) -> Option<JobKind> {
        JobKind::from_code(self.kind.load(Ordering::SeqCst))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// 给 core 的 `&AtomicBool` 参数用；标志位与取消请求是同一个内存。
    ///
    /// For core's `&AtomicBool` parameters; the flag and the cancel request share
    /// the same memory.
    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }

    /// 请求取消；返回请求前是否真的在跑。空闲时也安全（幂等）。
    ///
    /// Request cancellation; returns whether the job was really running before
    /// the request. Safe when idle (idempotent).
    pub fn cancel(&self) -> bool {
        let running = self.is_running();
        if running {
            self.cancel.store(true, Ordering::SeqCst);
        }
        running
    }

    pub fn done(&self) -> usize {
        self.done.load(Ordering::SeqCst)
    }

    pub fn total(&self) -> usize {
        self.total.load(Ordering::SeqCst)
    }

    pub fn stage(&self) -> u8 {
        self.stage.load(Ordering::SeqCst)
    }

    /// 任务启动序号，用于进度通知区分"本次调用"和"上一轮留下的状态"。
    ///
    /// The job start serial, used by progress notifications to tell this call
    /// apart from state left by the previous run.
    pub fn serial(&self) -> u64 {
        self.serial.load(Ordering::SeqCst)
    }

    pub fn elapsed_ms(&self) -> u64 {
        if self.is_running() {
            // 运行中按墙钟估算，结束后用冻结值，避免读表越读越大。
            self.started
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .map(|start| start.elapsed().as_millis() as u64)
                .unwrap_or(0)
        } else {
            self.elapsed_ms.load(Ordering::SeqCst)
        }
    }
}

/// 正在后台运行的任务。`Session` 被替换前必须先等它结束。
///
/// A job running in the background. It must be finished before `Session` is
/// replaced.
pub struct RunningJob {
    pub shared: Arc<JobShared>,
    pub handle: JoinHandle<()>,
}

pub struct Session {
    pub seq: Option<Sequence>,
    pub params: GradeParams,
    pub preset: String,
    /// 逐帧关键帧参数；导出和预览都通过 `interp::params_at` 取当前帧的值。
    ///
    /// Per-frame keyframe parameters; export and preview both read the current
    /// frame's values through `interp::params_at`.
    pub keyframes: BTreeMap<usize, GradeParams>,
    pub def_window: f64,
    pub def_strength: f64,
    pub def_multi: bool,
    /// 分通道（RGB）去闪烁；与 GUI 的同名开关同一口径。
    ///
    /// Per-channel (RGB) deflicker; same convention as the GUI toggle of the same
    /// name.
    pub def_channels: bool,
    pub analysis: Option<Analysis>,
    pub metrics: Option<Vec<Option<FrameMetrics>>>,
    /// 线性光测光，分通道去闪的输入；随测光那趟解码一起算，不重复读图。
    ///
    /// Linear-light measurements, the input for per-channel deflicker; computed
    /// during the same decode as metering, so frames are not read twice.
    pub def_means: Vec<Option<FrameMeans>>,
    pub issues: Vec<(usize, String)>,
    /// 打开序列时读到的工程，保存时在其上覆盖当前参数，保留评级/关键帧。
    ///
    /// The project read when the sequence was opened; saving overwrites the
    /// current parameters on top of it, keeping ratings and keyframes.
    pub project: Option<ProjectFile>,
    /// 最近一次对齐结果；结构直接用工程里的 `AlignState`，导出和保存共享同一份数据。
    ///
    /// The most recent alignment result; the type is the project's `AlignState`,
    /// so export and save share one copy of the data.
    pub last_align: Option<AlignState>,
    /// 对齐帧尺寸；从工程恢复时未知，保持 `None`。
    ///
    /// Aligned frame size; unknown when restored from a project, so it stays
    /// `None`.
    pub last_align_size: Option<[u32; 2]>,
    pub last_align_elapsed: f64,
    /// 低分阈值，仅用于状态报告里统计低分帧。
    ///
    /// Low-score threshold, used only to count low-score frames in status
    /// reports.
    pub last_align_min_score: f64,
    pub last_align_smoothed: bool,
    pub last_export: Option<PathBuf>,
    pub last_export_summary: Option<serde_json::Value>,
    pub last_preview: Option<PathBuf>,
    pub last_error: Option<String>,
    /// 批量队列；换序列时保留，和 GUI 的 `render_queue.json` 同一份语义。
    ///
    /// Batch queue; kept across sequence switches, with the same semantics as the
    /// GUI's `render_queue.json`.
    pub queue: Vec<QueueItem>,
    pub queue_path: PathBuf,
    pub queue_loaded: bool,
    pub shared: Arc<JobShared>,
    pub job: Option<RunningJob>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            seq: None,
            params: GradeParams::default(),
            preset: "原始".to_owned(),
            keyframes: BTreeMap::new(),
            def_window: 15.0,
            def_strength: 100.0,
            def_multi: false,
            def_channels: false,
            analysis: None,
            metrics: None,
            def_means: Vec::new(),
            issues: Vec::new(),
            project: None,
            last_align: None,
            last_align_size: None,
            last_align_elapsed: 0.0,
            last_align_min_score: 0.35,
            last_align_smoothed: false,
            last_export: None,
            last_export_summary: None,
            last_preview: None,
            last_error: None,
            queue: Vec::new(),
            queue_path: queue::default_path(),
            queue_loaded: false,
            shared: Arc::new(JobShared::new()),
            job: None,
        }
    }
}

impl Session {
    pub fn require_seq(&self) -> Result<&Sequence, String> {
        self.seq
            .as_ref()
            .ok_or_else(|| "还没有打开序列，先调用 aevum_open_sequence".to_owned())
    }

    pub fn require_root(&self) -> Result<PathBuf, String> {
        Ok(self.require_seq()?.root.clone())
    }

    /// 是否有后台任务还在跑。同步任务持锁期间别的请求进不来，不会走到这里。
    ///
    /// Whether a background job is still running. A synchronous job holds the
    /// lock, so other requests cannot reach here.
    pub fn is_running(&self) -> bool {
        self.job.as_ref().is_some_and(|j| j.shared.is_running())
    }

    /// 登记一个后台任务。
    ///
    /// Register a background job.
    pub fn start_background(&mut self, shared: Arc<JobShared>, handle: JoinHandle<()>) {
        self.job = Some(RunningJob { shared, handle });
    }

    /// 收掉已经结束的后台任务，把结果落进会话。没结束就什么都不做。
    ///
    /// Reap a finished background job and commit its result to the session. Does
    /// nothing while it is still running.
    pub fn poll_job(&mut self) {
        let finished = self
            .job
            .as_ref()
            .is_some_and(|job| job.handle.is_finished());
        if !finished {
            return;
        }
        let job = self.job.take().expect("刚刚检查过任务句柄");
        let _ = job.handle.join();
        match job.shared.take_outcome() {
            Some(Ok(outcome)) => self.apply_outcome(outcome),
            Some(Err(e)) => self.last_error = Some(e),
            None => self.last_error = Some("后台任务结束但没有返回结果".to_owned()),
        }
    }

    /// 结束一个同步任务：成功时落状态，失败时记录错误并原样返回。
    ///
    /// Finish a synchronous job: commit state on success, record the error and
    /// return it unchanged on failure.
    pub fn complete_job(&mut self, result: Result<JobOutcome, String>) -> Result<(), String> {
        match result {
            Ok(outcome) => {
                self.shared.finish_ok();
                self.apply_outcome(outcome);
                Ok(())
            }
            Err(e) => {
                self.shared.finish(Err(e.clone()));
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    fn apply_outcome(&mut self, outcome: JobOutcome) {
        match outcome {
            JobOutcome::Align {
                report,
                reference,
                estimator,
            } => {
                self.last_align_size = Some(report.size);
                self.last_align_elapsed = report.elapsed_secs;
                self.last_align = Some(AlignState {
                    reference,
                    estimator: estimator.to_owned(),
                    offsets: report.offsets,
                    scores: report.scores,
                    issues: report.issues,
                    crop: Some(report.crop),
                });
                self.last_error = None;
            }
            JobOutcome::Measure {
                metrics,
                def_means,
                issues,
            } => {
                self.metrics = Some(metrics);
                self.def_means = def_means;
                self.issues = issues;
                self.analysis = None;
                self.last_error = None;
            }
            JobOutcome::Export { path, summary } => {
                self.last_export = Some(path);
                self.last_export_summary = Some(summary);
                self.last_error = None;
            }
            JobOutcome::Queue {
                rows,
                ok,
                failed,
                cancelled,
            } => {
                for (index, status, note) in rows {
                    if let Some(item) = self.queue.get_mut(index) {
                        item.status = status;
                        item.note = note;
                    }
                }
                self.last_error = if failed > 0 {
                    Some(format!(
                        "队列有 {failed} 段失败（完成 {ok} 段），见 aevum_queue_status"
                    ))
                } else if cancelled {
                    Some("队列已取消（已完成的段保留结果）".to_owned())
                } else {
                    None
                };
            }
        }
    }

    /// 请求取消当前后台任务；返回被取消的任务类型（空闲时返回 `None`）。
    ///
    /// Request cancellation of the current background job; returns the cancelled
    /// job kind (`None` when idle).
    pub fn request_cancel(&mut self) -> Option<JobKind> {
        self.poll_job();
        let job = self.job.as_ref()?;
        if !job.shared.is_running() {
            return None;
        }
        job.shared.cancel();
        job.shared.kind()
    }

    pub fn cancel_requested(&self) -> bool {
        self.shared.cancel_requested()
    }

    /// 当前帧自己的参数：关键帧之间线性插值，没有关键帧时就是基准参数。
    ///
    /// Each frame's own parameters: linear interpolation between keyframes, and
    /// the base parameters when there are none.
    pub fn frame_params(&self, n: usize) -> Vec<GradeParams> {
        (0..n)
            .map(|i| interp::params_at(&self.params, &self.keyframes, i))
            .collect()
    }

    /// 指定调色副本的逐帧参数；空名 = 主调色。名字对不上现在就报错。
    ///
    /// Per-frame parameters of a named grading copy; an empty name means the
    /// primary grade. An unknown name fails right here.
    pub fn frame_params_of(&self, n: usize, copy: &str) -> Result<Vec<GradeParams>, String> {
        if copy.is_empty() {
            return Ok(self.frame_params(n));
        }
        let found = self
            .project
            .as_ref()
            .and_then(|p| p.copies.iter().find(|c| c.name == copy));
        let Some(found) = found else {
            return Err(format!("序列工程里没有叫 {copy} 的调色副本"));
        };
        Ok((0..n)
            .map(|i| interp::params_at(&found.params, &found.keyframes, i))
            .collect())
    }

    /// 命名调色副本整表（工程文件里那一份）；还没有工程时是空表。
    ///
    /// The whole named-copy table (the one in the project file); empty until a
    /// project exists.
    pub fn copies(&self) -> Vec<project::GradeCopy> {
        self.project
            .as_ref()
            .map(|p| p.copies.clone())
            .unwrap_or_default()
    }

    /// 副本表的读写入口：无头会话的副本住在工程文件快照里，随 aevum_project_save 落盘。
    ///
    /// Read/write entry for the copy table: a headless session keeps its copies in
    /// the project snapshot, persisted by `aevum_project_save`.
    pub fn with_copies<T>(&mut self, f: impl FnOnce(&mut Vec<project::GradeCopy>) -> T) -> T {
        let project = self.project.get_or_insert_with(ProjectFile::default);
        f(&mut project.copies)
    }

    /// 导出要用的几何：有对齐结果就用统一裁切框 + 逐帧位移，否则全画幅零位移。
    ///
    /// The geometry export needs: the shared crop box plus per-frame offsets when
    /// an alignment exists, otherwise the full frame with zero offsets.
    pub fn alignment(&self, n: usize) -> (Option<CropRect>, Vec<[f64; 2]>) {
        match &self.last_align {
            Some(a) if a.offsets.len() == n => (a.crop, a.offsets.clone()),
            _ => (None, vec![[0.0, 0.0]; n]),
        }
    }

    /// 第一次需要测光时按 512 长边跑一遍，之后复用。
    ///
    /// 分块调用 core 的并行测光，块与块之间检查取消请求，这样后台测光能被
    /// `aevum_cancel` 在几帧之内停下，而不是等整段跑完。
    ///
    /// Measure once at a 512 long side the first time it is needed, then reuse.
    ///
    /// Calls core's parallel measurement in chunks and checks for cancellation
    /// between chunks, so a background measurement stops within a few frames of
    /// `aevum_cancel` instead of running to the end.
    pub fn ensure_metrics(&mut self) -> Result<(), String> {
        if self.metrics.is_some() {
            return Ok(());
        }
        // 后台任务共享同一套进度/取消槽位，不能同时再起一轮测光。
        if self.is_running() {
            return Err("有后台任务正在运行，先等它结束或调用 aevum_cancel".to_owned());
        }
        let paths = self.require_seq()?.paths();
        if paths.is_empty() {
            return Err("序列没有帧".to_owned());
        }
        let own_job = !self.shared.is_running();
        if own_job {
            self.shared.begin(JobKind::Measure, paths.len());
        }
        let shared = Arc::clone(&self.shared);
        let result = measure_paths(&paths, &shared);
        if own_job {
            match &result {
                Ok(_) => shared.finish_ok(),
                Err(e) => shared.finish(Err(e.clone())),
            }
        }
        let outcome = result?;
        self.apply_outcome(outcome);
        Ok(())
    }

    /// 去闪烁曝光补偿；没有分析结果时不偷偷补，返回全零。
    ///
    /// 单通道分析时 `chan` 全零，与只补亮度的老口径逐位一致。
    ///
    /// Deflicker exposure compensation; with no analysis it does not quietly
    /// compensate and returns zeros.
    ///
    /// With single-channel analysis `chan` is all zeros, bit-identical to the old
    /// luminance-only convention.
    pub fn gains(&self, n: usize) -> Vec<Ev> {
        match &self.analysis {
            Some(a) => {
                let offsets = a.exposure_per_frame();
                let chan = a.chan_residual_per_frame();
                (0..n)
                    .map(|i| Ev {
                        add: offsets.get(i).copied().unwrap_or(0.0) as f32,
                        chan: chan.get(i).copied().unwrap_or([0.0; 3]).map(|v| v as f32),
                    })
                    .collect()
            }
            None => vec![Ev::NONE; n],
        }
    }

    pub fn gain_at(&self, frame: usize) -> Ev {
        self.gains(frame + 1).into_iter().last().unwrap_or(Ev::NONE)
    }

    pub fn is_source(&self, path: &Path) -> bool {
        let Some(seq) = &self.seq else {
            return false;
        };
        let target = normalize(path);
        seq.frames
            .iter()
            .any(|f| same_path(&normalize(&f.path), &target))
    }

    /// 当前状态对应的工程快照；保留工程里与调色无关的评级等字段。
    ///
    /// The project snapshot matching the current state; unrelated fields such as
    /// ratings are carried over from the existing project.
    pub fn snapshot_project(&self) -> Result<ProjectFile, String> {
        let root = self.require_root()?;
        let mut project = self.project.clone().unwrap_or_default();
        project.version = project::VERSION;
        project.root = root.display().to_string();
        project.params = self.params.clone();
        project.preset = self.preset.clone();
        project.keyframes = self.keyframes.clone();
        project.def_window = self.def_window;
        project.def_strength = self.def_strength;
        project.def_multi = self.def_multi;
        project.def_channels = self.def_channels;
        if let Some(seq) = &self.seq
            && self.analysis.is_some()
        {
            let gains = self.gains(seq.len());
            project.def_offsets = gains.iter().map(|g| g.add).collect();
            project.def_chan = gains.iter().map(|g| g.chan).collect();
        }
        if let Some(align) = &self.last_align {
            project.reference = align.reference;
            project.crop = align.crop;
            project.align = Some(align.clone());
        }
        Ok(project)
    }

    pub fn project_path(&self) -> Result<PathBuf, String> {
        Ok(project::project_path(&self.require_root()?))
    }

    /// 这个序列的说明与版权：导出时随静帧进 EXIF、随成片进容器标签。
    ///
    /// This sequence's description and copyright: at export they go into EXIF for
    /// stills and into container tags for films.
    pub fn metadata(&self) -> project::Metadata {
        self.project
            .as_ref()
            .map(|p| p.metadata.clone())
            .unwrap_or_default()
    }

    /// 第一次用到队列时从磁盘读一次，之后以会话为准。
    ///
    /// Read the queue from disk once, the first time it is used; afterwards the
    /// session is authoritative.
    pub fn ensure_queue_loaded(&mut self) {
        if self.queue_loaded {
            return;
        }
        self.queue = queue::load(&self.queue_path);
        self.queue_loaded = true;
    }

    pub fn save_queue(&self) -> Result<(), String> {
        queue::save(&self.queue_path, &self.queue).map_err(|e| format!("队列保存失败：{e}"))
    }
}

/// 逐帧测光（可被打断）。调用方负责 `shared.begin(JobKind::Measure, n)`。
///
/// Per-frame measurement (interruptible). The caller is responsible for
/// `shared.begin(JobKind::Measure, n)`.
pub fn measure_paths(paths: &[PathBuf], shared: &JobShared) -> Result<JobOutcome, String> {
    let n = paths.len();
    if n == 0 {
        return Err("序列没有帧".to_owned());
    }
    // 约 4 块，单块最多 64 帧：既保留 core 的并行度，又让取消有几帧的响应时间。
    let chunk = n.div_ceil(4).clamp(1, 64);
    let mut measured: Vec<Option<FrameMetrics>> = vec![None; n];
    let mut defs: Vec<Option<FrameMeans>> = vec![None; n];
    let mut issues = Vec::new();
    let mut done = 0usize;
    for part in paths.chunks(chunk) {
        if shared.cancel_requested() {
            return Err("测光已取消".to_owned());
        }
        let (values, part_defs, errors) = metrics::measure_many(part, MEASURE_LONG_SIDE);
        for (i, value) in values.into_iter().enumerate() {
            measured[done + i] = value;
        }
        for (i, value) in part_defs.into_iter().enumerate() {
            defs[done + i] = value;
        }
        for (i, error) in errors {
            issues.push((done + i, error));
        }
        done += part.len();
        shared.progress(done, n);
    }
    if shared.cancel_requested() {
        return Err("测光已取消".to_owned());
    }
    if measured.iter().all(Option::is_none) {
        return Err("所有帧都读不出来，检查文件是否损坏".to_owned());
    }
    Ok(JobOutcome::Measure {
        metrics: measured,
        def_means: defs,
        issues,
    })
}

/// 统一成绝对路径再比较，避免 `..` 和相对路径绕过安全判断。
///
/// 这里只做词法清理、不碰文件系统：写目标可能还不存在，canonicalize 会失败，
/// 而 Windows 自己会词法解析 `..`，不清理就可能把输出写回源目录。
///
/// Make the path absolute before comparing, so `..` and relative paths cannot slip
/// past the safety checks.
///
/// This is lexical cleanup only and touches no filesystem: the write target may not
/// exist yet (canonicalize would fail), and Windows resolves `..` lexically itself,
/// so skipping it could write output back into the source directory.
pub fn normalize(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            // 绝对路径的根之上还是根，pop 失败时直接忽略。
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Windows 路径大小写不敏感，安全判断不能只比字符串。
///
/// Windows paths are case-insensitive, so a safety check cannot compare strings
/// alone.
pub fn same_path(a: &Path, b: &Path) -> bool {
    component_starts_with(a, b) && component_starts_with(b, a)
}

pub fn path_starts_with(child: &Path, parent: &Path) -> bool {
    component_starts_with(child, parent)
}

fn component_starts_with(child: &Path, parent: &Path) -> bool {
    let mut child = child.components();
    for expected in parent.components() {
        match child.next() {
            Some(actual) if component_eq(actual, expected) => {}
            _ => return false,
        }
    }
    true
}

#[cfg(windows)]
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

#[cfg(not(windows))]
fn component_eq(a: Component<'_>, b: Component<'_>) -> bool {
    a == b
}
