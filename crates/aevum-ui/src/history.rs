//! 撤销 / 重做：可撤销状态的线性历史。
//!
//! 为什么不是 op_journal.jsonl 反向回放（计划里最初的想法）：
//! - 日志只记「这一次传了什么」，而 `grade.set` 是字段合并，日志里既没有没被提到的
//!   字段的原值，也没有蒙版数组的旧内容；
//! - 面板推子直接改 `state.params`，根本不走命令注册表（见 `shell::slider`），
//!   所以日志里压根没有人在界面上做的大部分改动。
//!
//! 结论：历史在内存里存整份快照，日志继续只管审计，两者互不依赖。
//!
//! 每步存「这步做完之后的状态」，`cursor` 指向当前生效的那步。撤销 = 光标左移并回放
//! 快照，重做 = 右移并回放快照，因此不需要给每种写入手写反向操作。
//!
//! 什么时候记一步：界面帧末尾把当前状态和 `cursor` 处的快照比一下，不一样就记。
//! 这样任何改动路径都被覆盖（面板推子、色轮、画布手柄、以后新加的控件），不靠每个
//! 控件自己记得登记；控件登记的只用来当那一行的文案。
//!
//! 拖动一根推子会连着改几十帧：连续帧、同一个控件的改动合并成一步（`frame` 相邻才合），
//! 换控件或换命令就开新步。判据用帧号而不是时间，测试里是确定的。
//!
//! Undo / redo: a linear history of undoable state.
//!
//! Why not reverse replay of `op_journal.jsonl` (the plan's original idea):
//! - The log only records "what was passed this time", while `grade.set` merges
//!   fields: the log has neither the prior values of fields nobody mentioned nor the
//!   old contents of the mask array;
//! - Panel faders write `state.params` directly and never go through the command
//!   registry (see `shell::slider`), so most of what a human does in the UI never
//!   appears in the log at all.
//!
//! Conclusion: history keeps whole snapshots in memory and the log stays purely an
//! audit trail; neither depends on the other.
//!
//! Each step stores "the state after this step", and `cursor` points at the step in
//! effect. Undo = move the cursor left and replay that snapshot; redo = move right
//! and replay. No per-write inverse operation has to be hand-written.
//!
//! When a step is recorded: at the end of a UI frame, compare the current state
//! against the snapshot at `cursor` and record if they differ. That covers every
//! mutation path (panel faders, colour wheels, canvas handles, whatever control is
//! added later) without each one having to remember to register itself; registration
//! only supplies the wording of that row.
//!
//! Dragging one fader mutates across dozens of frames: consecutive-frame changes of
//! the same control merge into one step (only adjacent `frame` values merge);
//! switching control or command opens a new step. The criterion is the frame number,
//! not wall time, so tests are deterministic.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use aevum_core::align::CropRect;
use aevum_core::audio::AudioOptions;
use aevum_core::colorspace::ColorSpace;
use aevum_core::crop::Motion;
use aevum_core::export::{OutputDynamic, OutputMode, OutputSharpen, SharpenStrength, StillFormat};
use aevum_core::grade::GradeParams;
use aevum_core::project::GradeCopy;
use aevum_core::retime::Retime;
use aevum_core::watermark::Watermark;

use crate::state::Aspect;

/// 历史上限：一步几千字节，100 步在几 MB 量级，够覆盖一整个调色会话。
///
/// History cap: a step is a few kilobytes, so 100 steps is a few MB -- enough for a
/// whole grading session.
pub const HISTORY_LIMIT: usize = 100;

/// 拖动合并的帧距容差：同键改动相隔不超过这么多帧仍算同一次拖动。
///
/// 严格「相邻帧」在真机上会断链——鼠标事件不是每帧都有，手一停（哪怕半帧的
/// 抖动）判据就失效，一次拖动被拆成十几条历史（v0.3.0 实测的「裁剪 768 × 368」
/// 刷屏就是这么来的）。60fps 下 30 帧 ≈ 半秒：手停半秒以上再动，才算下一次拖动。
///
/// Frame-distance tolerance for drag merging: same-key changes less than this many
/// frames apart still count as one drag.
///
/// Strict "adjacent frame" breaks on real hardware -- mouse events do not arrive every
/// frame, and the moment the hand pauses (even half a frame of jitter) the criterion
/// fails, splitting one drag into a dozen history rows (this is exactly the "crop
/// 768 x 368" spam measured in v0.3.0). 30 frames at 60fps is about half a second:
/// pausing longer than that before moving again counts as the next drag.
pub const MERGE_FRAME_TOLERANCE: i64 = 30;

/// 可撤销状态的快照。
///
/// 只收「参数类」字段。视图状态（缩放、平移、面板开合、当前帧、模块）和只读结果
/// （测光、直方图、队列进度）都不进历史——PS/LR 也不会因为你滚了一下鼠标就让你撤销。
/// 对齐产生的 `align_report` 同样不在这里：对齐是外部效果，见 [`Step::revertible`]。
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub params: GradeParams,
    pub preset: String,
    pub def_window: f64,
    pub def_strength: f64,
    pub def_multi: bool,
    pub def_channels: bool,
    pub aspect: Aspect,
    /// 用户拖出来的裁剪框；和 `aspect` 一样是「构图类」设置，撤销要一起回。
    ///
    /// The crop box the user dragged out; a composition-class setting like `aspect`, so
    /// undo must restore it together.
    pub crop: Option<CropRect>,
    /// 拉直角度（度）。和 `crop` 必须同进同退：框是量在转过之后的那一帧上的。
    ///
    /// Straightening angle (degrees). Must move together with `crop`: the box is measured
    /// on the frame after rotation.
    pub rotate: f64,
    pub grid: bool,
    pub cross: bool,
    pub split: bool,
    pub keyframes: BTreeMap<usize, GradeParams>,
    /// 命名调色副本整表：存/删/改名副本本身也可撤销。
    ///
    /// The whole table of named grading copies: saving, deleting and renaming a copy are
    /// themselves undoable.
    pub copies: Vec<GradeCopy>,
    pub ratings: HashMap<usize, u8>,
    pub flags: HashMap<usize, i8>,
    pub labels: HashMap<usize, u8>,
    pub keywords: HashMap<usize, Vec<String>>,
    pub output_w: u32,
    pub output_h: u32,
    pub output_fps: f32,
    pub codec: String,
    pub render_mode: OutputMode,
    pub render_format: StillFormat,
    pub render_quality: u8,
    pub render_keep_exif: bool,
    pub render_xmp_sidecar: bool,
    pub render_sharpen_for: OutputSharpen,
    pub render_sharpen_strength: SharpenStrength,
    pub render_color_space: ColorSpace,
    /// 输出动态范围（SDR / HDR PQ）。跟色域一样是成片级设置，一起进撤销栈。
    ///
    /// Output dynamic range (SDR / HDR PQ). A per-export setting like the colour space, so it
    /// goes into the undo stack with it.
    pub render_dynamic: OutputDynamic,
    /// 水印整块：开关、内容、位置、不透明度一起进同一步，撤销不许只回一半。
    ///
    /// The watermark as one block: enable, content, position and opacity go into the same
    /// step; undo must not restore half of it.
    pub render_watermark: Watermark,
    /// 变速整块：开关、速度键、快门一起进同一步——只回一半会算出另一个帧数。
    ///
    /// The retime block as one: enable, speed keys and shutter go into the same step --
    /// restoring half would compute a different frame count.
    pub retime: Retime,
    /// 运镜整块：开关、起点框、终点框、缓动一起进同一步，撤销不许只回一半。
    ///
    /// The motion block as one: enable, start box, end box and easing go into the same
    /// step; undo must not restore half of it.
    pub motion: Motion,
    pub render_crf: u8,
    pub render_preset: String,
    pub render_keep_frames: bool,
    pub render_resume: bool,
    pub render_audio: Option<PathBuf>,
    pub render_audio_opts: AudioOptions,
}

/// 历史里的一步。
#[derive(Clone, Debug)]
pub struct Step {
    /// 面板上显示的一句话，例如「曝光 +0.35」「蒙版 增加」。
    ///
    /// One line shown in the panel, e.g. "Exposure +0.35" or "Mask add".
    pub label: String,
    /// 合并拖动用的键：同一个控件连续改动只算一步；空键表示不参与合并。
    ///
    /// The merge key for dragging: consecutive changes of the same control count as one
    /// step; an empty key opts out of merging.
    pub key: String,
    /// 这一步改动的「类」：与上一步状态比，第一个不同的字段名（[`History::hint_between`]）。
    ///
    /// 同类判据给「没登记控件的路径」用：画布拖框的中间态走登记键（`crop-drag`），
    /// 收尾的回声（松手把铺满的框退回默认）走字段比对，两条路落的是同一类改动，
    /// 应当并成一步而不是各占一行（v1.0.1 实测「裁剪框 / 裁剪 4000 × 6000」成对刷屏）。
    ///
    /// The "class" of this change: the first field name that differs from the previous
    /// state ([`History::hint_between`]).
    ///
    /// Class matching serves the paths that register no control: the mid-drag state of a
    /// canvas crop box goes through the registered key (`crop-drag`), while the closing
    /// echo (releasing a full-bleed box back to the default) goes through field
    /// comparison. Both are the same class of change and should merge into one step
    /// instead of taking a row each (the paired "crop box / crop 4000 x 6000" spam
    /// measured in v1.0.1).
    pub cat: &'static str,
    /// true = 这步走的是字段比对路径（没有控件登记文案），合并时按 [`Step::cat`] 认类；
    /// false = 控件登记过键，合并只认键——两个不同滑杆挨得再近也不许并成一步。
    ///
    /// true = this step came through field comparison (no control registered a label), so
    /// merging matches on [`Step::cat`]; false = a control registered a key, so merging
    /// matches the key only -- two different sliders never merge no matter how close
    /// together they were moved.
    pub cat_merge: bool,
    /// 产生这步的界面帧序号；命令与外部来源填 -1，因此永不与上一帧合并。
    ///
    /// Index of the UI frame that produced this step; commands and external sources use
    /// -1, so they never merge with the previous frame.
    pub frame: i64,
    /// false = 这步的效果已经落在盘上（导出、对齐、存工程），撤销只回参数，产物不删。
    ///
    /// false = the effect of this step has already landed on disk (export, alignment,
    /// project save); undo restores parameters only and never deletes the artefacts.
    pub revertible: bool,
    /// 这步做完之后的状态。
    ///
    /// The state after this step.
    pub snap: Snapshot,
}

/// 一次撤销 / 重做的结果。
#[derive(Clone, Debug)]
pub struct Move {
    /// 被撤掉或重放的那一步的说明。
    ///
    /// Description of the step being undone or replayed.
    pub label: String,
    /// 该步是否带外部效果（需要提醒用户产物不会被删除）。
    ///
    /// Whether the step has external effects (the user must be told the artefacts will
    /// not be deleted).
    pub external: bool,
    /// 要回放的目标状态。
    ///
    /// The target state to replay.
    pub snap: Snapshot,
    /// 移动后的光标（0 基）。
    ///
    /// The cursor after the move (0-based).
    pub cursor: usize,
    /// 历史总步数。
    ///
    /// Total number of steps in the history.
    pub total: usize,
    /// 光标之后还剩几步可以重做。
    ///
    /// How many steps remain after the cursor that can be redone.
    pub redoable: usize,
}

pub struct History {
    steps: Vec<Step>,
    cursor: usize,
    frame: i64,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            cursor: 0,
            frame: 0,
            limit: HISTORY_LIMIT,
        }
    }

    /// 换序列 / 关序列：历史重新开始，第 0 步就是当前状态。
    ///
    /// 文案在这里落账时翻成当前语言：历史行是 `p.text(...)` 直接画的，没有
    /// 显示期再翻一次的机会。**键不翻**——键只用来合并相邻的同类改动，
    /// 翻了会让「切语言」凭空拆出新的一步。
    ///
    /// Switching or closing a sequence: the history restarts, and step 0 is the current
    /// state.
    ///
    /// The label is translated into the current language as it is recorded: history rows
    /// are drawn directly with `p.text(...)`, with no second chance to translate at
    /// display time. **Keys are never translated** -- a key only merges adjacent
    /// same-class changes, and translating it would split a phantom new step the moment
    /// the language is switched.
    pub fn reset(&mut self, snap: Snapshot, label: &str) {
        self.steps = vec![Step {
            label: crate::i18n::tr(label).to_owned(),
            key: String::new(),
            cat: "",
            cat_merge: false,
            frame: -1,
            revertible: true,
            snap,
        }];
        self.cursor = 0;
        self.frame = 0;
    }

    /// 每个界面帧开头调一次：给这一帧编号，连续帧才能合并成一步。
    ///
    /// Called once at the start of every UI frame: numbers the frame so consecutive
    /// frames can merge into one step.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.steps.len()
    }

    /// 当前生效状态（回放它 = 什么都不变）。
    ///
    /// The state in effect (replaying it changes nothing).
    pub fn current(&self) -> Option<&Snapshot> {
        self.steps.get(self.cursor).map(|s| &s.snap)
    }

    /// 记一步参数改动。连续帧 + 同一个 key 会并进最后一步，否则新开一步。
    ///
    /// Record one parameter change. Consecutive frames with the same key merge into the
    /// last step; otherwise a new step opens.
    pub fn push(&mut self, snap: Snapshot, key: &str, label: String) {
        self.push_with(snap, key, label, true, self.frame, false)
    }

    /// 记一步「控件没登记」的改动：键与类都用字段比对出的类名，合并按类认。
    ///
    /// 判据是状态本身（[`History::dirty`]），所以画布手柄、色轮这类没走 `note`
    /// 的改动照样进历史；它们的收尾回声（如松手退回默认框）也走这条路，
    /// 类相同就并进前面那一步，不再各占一行。
    ///
    /// Record a change where no control registered anything: both key and class come from
    /// the field-comparison class name, and merging matches on the class.
    ///
    /// The criterion is the state itself ([`History::dirty`]), so changes that never
    /// called `note` -- canvas handles, colour wheels -- still reach the history; their
    /// closing echo (such as releasing back to the default box) takes this path too, and
    /// merges into the previous step when the class matches instead of taking a row.
    pub fn push_diff(&mut self, snap: Snapshot, hint: &'static str) {
        self.push_with(snap, hint, hint.to_owned(), true, self.frame, true)
    }

    /// 记一步外部效果：留在历史里，但标明不可撤销。
    ///
    /// Record a step with external effects: it stays in the history but is marked
    /// non-revertible.
    pub fn push_external(&mut self, snap: Snapshot, label: String) {
        self.push_with(snap, "", label, false, -1, false)
    }

    fn push_with(
        &mut self,
        snap: Snapshot,
        key: &str,
        label: String,
        revertible: bool,
        frame: i64,
        cat_merge: bool,
    ) {
        // 先把文案翻成当前语言再落账：历史行是直接画的，显示期没有第二次机会。
        // `tr` 对已经是英文的串是恒等（查不到就返回原值，且不含汉字时不记缺译），
        // 所以上层用 `tr_fmt` 拼好的句子再流经这里也安全。
        let label = crate::i18n::tr(&label).to_owned();
        if self.steps.is_empty() {
            // 没 reset 过（无头模式直接下命令）：第一次改动就是这条时间线的起点，
            // 前面没有可回的状态，所以只留一步，不凭空造一个「初始状态」。
            let base = if label.is_empty() {
                "初始状态"
            } else {
                &label
            };
            self.reset(snap.clone(), base);
            return;
        }
        // 这一步改的是哪一类：与光标处的状态比，第一个不同的字段名。
        // 只在确有差异时才走到这里（`dirty` 已挡住无改动），所以类名必然非空。
        let cat = Self::hint_between(&self.steps[self.cursor].snap, &snap);
        let tip = &self.steps[self.cursor];
        let merge = revertible
            && !key.is_empty()
            && self.cursor + 1 == self.steps.len()
            && frame > 0
            && tip.revertible
            && tip.frame >= 0
            && frame - tip.frame <= MERGE_FRAME_TOLERANCE
            && (
                // 同一个控件登记的连续改动：认键（两个不同滑杆挨得再近也不并）。
                tip.key == key
                // 类相同的「回声」：登记路径与字段比对路径各记一半（拖框走登记键、
                // 松手退回默认走字段比对），不并就是成对刷屏。至少一边是字段比对
                // 路径才许按类并——两个不同滑杆同类（都是「调参数」）不能误并。
                || (cat == tip.cat && (tip.cat_merge || cat_merge))
            );
        if merge {
            // 一次拖动：起点在光标之前，不动；只把「做完之后的状态」往后推。
            let tip = &mut self.steps[self.cursor];
            tip.snap = snap;
            tip.label = label;
            tip.frame = frame;
            tip.cat = cat;
            tip.cat_merge = tip.cat_merge && cat_merge;
            return;
        }
        self.steps.truncate(self.cursor + 1);
        self.steps.push(Step {
            label,
            key: key.to_owned(),
            cat,
            cat_merge,
            frame,
            revertible,
            snap,
        });
        self.cursor = self.steps.len() - 1;
        while self.steps.len() > self.limit {
            self.steps.remove(0);
            self.cursor -= 1;
        }
    }

    pub fn undo(&mut self) -> Option<Move> {
        if !self.can_undo() {
            return None;
        }
        self.cursor -= 1;
        let left = &self.steps[self.cursor + 1];
        Some(Move {
            label: left.label.clone(),
            external: !left.revertible,
            snap: self.steps[self.cursor].snap.clone(),
            cursor: self.cursor,
            total: self.steps.len(),
            redoable: self.steps.len() - self.cursor - 1,
        })
    }

    pub fn redo(&mut self) -> Option<Move> {
        if !self.can_redo() {
            return None;
        }
        self.cursor += 1;
        let back = &self.steps[self.cursor];
        Some(Move {
            label: back.label.clone(),
            external: !back.revertible,
            snap: back.snap.clone(),
            cursor: self.cursor,
            total: self.steps.len(),
            redoable: self.steps.len() - self.cursor - 1,
        })
    }

    /// 点历史面板的任意一步：回到那之后的状态。越界返回 None。
    ///
    /// Click any step in the history panel: return to the state after it. Out of range
    /// returns None.
    pub fn jump(&mut self, index: usize) -> Option<Move> {
        if index >= self.steps.len() {
            return None;
        }
        let external = !self.steps[index].revertible && index > self.cursor;
        self.cursor = index;
        let step = &self.steps[index];
        Some(Move {
            label: step.label.clone(),
            external,
            snap: step.snap.clone(),
            cursor: self.cursor,
            total: self.steps.len(),
            redoable: self.steps.len() - self.cursor - 1,
        })
    }

    /// 光标之前有几步可撤。
    ///
    /// How many steps before the cursor can be undone.
    pub fn undoable(&self) -> usize {
        self.cursor
    }

    /// 本帧有没有改动：和光标处的快照比。这是记步的唯一判据，所以控件忘了登记文案
    /// 也不会漏掉撤销——最多只是那一行写得笼统些。
    ///
    /// Whether this frame changed anything: compare against the snapshot at the cursor.
    /// This is the only criterion for recording a step, so a control that forgot to
    /// register a label still cannot lose its undo -- the row is just worded more
    /// generically.
    pub fn dirty(&self, now: &Snapshot) -> bool {
        self.current() != Some(now)
    }

    /// 控件没登记文案时，用第一个不同的字段名当那一行的说明，同时当合并键。
    ///
    /// When no control registered a label, use the first differing field name as that
    /// row's description and as the merge key.
    pub fn diff_hint(&self, now: &Snapshot) -> &'static str {
        match self.current() {
            Some(base) => Self::hint_between(base, now),
            None => "",
        }
    }

    /// 两份快照之间第一个不同的字段名：既是没登记时的行说明，也是合并用的「类」。
    ///
    /// The first field name that differs between two snapshots: both the row description
    /// when nothing was registered and the "class" used for merging.
    pub fn hint_between(base: &Snapshot, now: &Snapshot) -> &'static str {
        macro_rules! changed {
            ($field:ident, $name:expr) => {
                if base.$field != now.$field {
                    return $name;
                }
            };
        }
        changed!(params, "调参数");
        changed!(preset, "换预设");
        changed!(def_window, "去闪窗口");
        changed!(def_strength, "去闪强度");
        changed!(def_multi, "去闪模式");
        changed!(def_channels, "去闪通道");
        changed!(aspect, "画面比例");
        changed!(crop, "裁剪框");
        changed!(rotate, "拉直");
        changed!(grid, "网格");
        changed!(cross, "十字线");
        changed!(split, "分割对比");
        changed!(keyframes, "关键帧");
        changed!(copies, "调色副本");
        changed!(ratings, "星级");
        changed!(flags, "标记");
        changed!(labels, "色彩标记");
        changed!(output_w, "输出尺寸");
        changed!(output_h, "输出尺寸");
        changed!(output_fps, "输出帧率");
        changed!(codec, "编码器");
        changed!(render_mode, "输出模式");
        changed!(render_format, "静帧格式");
        changed!(render_quality, "静帧质量");
        changed!(render_keep_exif, "保留 EXIF");
        changed!(render_xmp_sidecar, "XMP 旁车");
        changed!(render_sharpen_for, "输出锐化");
        changed!(render_sharpen_strength, "输出锐化强度");
        changed!(render_color_space, "输出色域");
        changed!(render_dynamic, "输出动态范围");
        changed!(render_watermark, "水印");
        changed!(retime, "变速");
        changed!(render_crf, "编码速度");
        changed!(render_preset, "渲染预设");
        changed!(render_keep_frames, "保留中间帧");
        changed!(render_resume, "断点续跑");
        changed!(render_audio, "配乐");
        changed!(render_audio_opts, "配乐设置");
        "改状态"
    }
}

thread_local! {
    /// 本帧里最后一次改动：(合并键, 展示文案)。
    static PENDING: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// 控件改完值后登记一下；`shell::ui` 在帧末把这条并进历史。
///
/// 走线程局部而不是 `&mut AppState`，是因为推子拿到的是 `&mut f64`——那个引用借的正是
/// `state.params`，再传 `&mut state` 就编译不过。界面帧是单线程顺序执行，登记与取用都在
/// 同一帧内，没有并发问题。
///
/// Call after a control changes its value; `shell::ui` merges this note into the
/// history at the end of the frame.
///
/// A thread local rather than `&mut AppState`, because a fader holds `&mut f64` --
/// that reference borrows `state.params`, so passing `&mut state` as well would not
/// compile. UI frames run single-threaded and in order, and the note is both written
/// and taken within one frame, so there is no concurrency issue.
pub fn note(key: &str, label: impl Into<String>) {
    // 同上：文案在这里就翻掉。纯文案（`"水印正文"`）靠这一层就够，不需要
    // 每个调用点都包 `t!`；带数值的句子仍要在调用点用 `tr_fmt` 拼，
    // 因为 `tr` 认的是整条模板，替换过占位符的串查不到。
    let label = crate::i18n::tr(&label.into()).to_owned();
    PENDING.with(|p| {
        *p.borrow_mut() = Some((key.to_owned(), label));
    });
}

/// 帧末取走并清空。
///
/// Take and clear at the end of the frame.
pub fn take_note() -> Option<(String, String)> {
    PENDING.with(|p| p.borrow_mut().take())
}

/// 丢弃本帧尚未落账的改动（换序列、复位历史时用）。
///
/// Discard changes not yet recorded this frame (used when switching sequences or
/// resetting the history).
pub fn clear_note() {
    PENDING.with(|p| *p.borrow_mut() = None);
}
