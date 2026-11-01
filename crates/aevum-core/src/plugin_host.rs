//! 插件宿主：把滤镜扔进独立进程跑，通过 stdin/stdout 交换 tile。
//!
//! 为什么一定要出进程：第三方滤镜是 C/C++ 编译出来的，一个空指针就是整进程
//! 退出。用户在两小时蒙版上按一下 Nik，崩的是整个 Aevum，连"刚才那步"都没了。
//! 出了进程，最坏情况是"这个插件挂了，其它照常"。
//!
//! 协议刻意做得笨：**没有共享内存、没有回调、没有异步**。
//!
//! ```text
//! 宿主 → 插件    {"t":"hello","protocol":1,"id":"…","params":{…}}\n
//! 插件 → 宿主    {"t":"ready","protocol":1}\n
//! 宿主 → 插件    {"t":"tile","seq":0,"x":0,"y":0,"w":512,"h":512,"px":"rgb8","len":786432}\n
//!                <786432 字节原始像素>
//! 插件 → 宿主    {"t":"tile",…}\n<字节>
//! 宿主 → 插件    {"t":"end"}\n
//! 插件退出
//! ```
//!
//! 每一帧都是"一行 JSON + 可选的裸字节"。这样做的好处是抓包能看懂、
//! 用 shell 也能调（`echo '...' | invert.exe`），坏处是每帧多一次 syscall ——
//! 512×512 的 tile 是 786 KB，多一次 write 的代价可以忽略。
//!
//! 像素只用 `rgb8`。不做 `rgba16`、不做平面格式：色彩管理在宿主侧统一做，
//! 插件拿到的一律是已经进工作空间的 sRGB 8 位。插件越笨，能出错的地方越少。
//!
//! Plugin host: filters run in a separate process and exchange tiles over stdin/stdout.
//!
//! Why the extra process is mandatory: third-party filters are built from C/C++, and one null
//! pointer takes the whole process down.
//! When a user hits Nik on a two-hour mask, the crash takes down all of Aevum Studio and the last
//! step with it.
//! Out of process, the worst case is that this plugin dies while everything else keeps running.
//!
//! The protocol is deliberately dumb: **no shared memory, no callbacks, no async**.
//!
//! ```text
//! 宿主 → 插件    {"t":"hello","protocol":1,"id":"…","params":{…}}\n
//! 插件 → 宿主    {"t":"ready","protocol":1}\n
//! 宿主 → 插件    {"t":"tile","seq":0,"x":0,"y":0,"w":512,"h":512,"px":"rgb8","len":786432}\n
//!                <786432 字节原始像素>
//! 插件 → 宿主    {"t":"tile",…}\n<字节>
//! 宿主 → 插件    {"t":"end"}\n
//! 插件退出
//! ```
//!
//! Every frame is one line of JSON plus optional raw bytes.
//! A traffic capture stays readable and the plugin can be driven from a shell.
//! A call such as `echo '...' | invert.exe` works as well.
//! The cost is one extra syscall per frame: a 512×512 tile is 786 KB, so one more write is
//! negligible.
//!
//! Pixels are always `rgb8`.
//! No `rgba16` and no planar formats: color management lives in the host only.
//! A plugin only ever receives 8-bit sRGB already inside the working space.
//! The dumber the plugin, the fewer places it can fail.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 协议版本。改了消息形状就要加一，宿主同时接受 `<=` 自己版本的所有插件。
///
/// Protocol version. Bump it whenever the message shape changes; the host accepts every plugin
/// whose version is `<=` its own.
pub const PROTOCOL: u32 = 1;
/// 单帧上限：512×512×3 ≈ 786 KB，留一倍余量挡住"len 被写成了 40 亿"。
///
/// Per-frame ceiling: a 512×512×3 tile is ≈ 786 KB, and the extra headroom blocks the case where
/// `len` was written as four billion.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
/// 协议帧（一行 JSON）的长度上限。像素帧有 [`MAX_FRAME`] 顶着，JSON 帧
/// 没有 —— 坏插件可以一口气吐一条不带换行的 GB 级"行"，把宿主吃成 OOM
/// （审查确认）。正常帧最长的是出错说明，几 KB 封顶，4 MB 已是千倍余量。
///
/// Length ceiling for a protocol frame, which is one line of JSON.
/// Pixel frames are bounded by [`MAX_FRAME`]; JSON frames were not, so a bad plugin could emit a
/// single GB-sized line with no newline and run the host out of memory (confirmed in review).
/// The longest legitimate frame is an error message of a few KB, so 4 MB is a thousandfold margin.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// 一个 tile 的边长。512 是内部预览的常用块大小，也是缓存对齐的整数倍。
///
/// Edge length of one tile. 512 is the usual block size for internal previews and an exact
/// multiple of the cache alignment.
pub const TILE: u32 = 512;
/// 默认超时。超过就当插件卡死，杀掉进程、把错误交回去。
///
/// Default timeout. Exceeding it counts as a stuck plugin: the process is killed and the error is
/// handed back.
pub const TIMEOUT: Duration = Duration::from_secs(20);
/// 握手超时。插件要加载自己的依赖（大滤镜可能拖进上百 MB 的 DLL），
/// 起步比单块宽，但必须有限 —— 否则一个启动即挂死的插件会让
/// [`Host::start`] 永远阻塞在等 Ready 上（审查发现的挂死路径）。
///
/// Handshake timeout. A plugin has to load its own dependencies, and a large filter can drag in
/// hundreds of MB of DLLs, so the startup budget is wider than a single tile.
/// It must still be finite: a plugin that hangs on start would otherwise block [`Host::start`]
/// forever while waiting for Ready (a hang found in review).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// 一条协议消息。
///
/// One protocol message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    /// 握手：宿主告诉插件"你是谁、参数是什么"。
    ///
    /// Handshake: the host tells the plugin what it is and which parameters it gets.
    Hello {
        protocol: u32,
        id: String,
        #[serde(default)]
        params: BTreeMap<String, f64>,
    },
    /// 插件确认握手成功。协议版本不匹配时插件应当回 [`Msg::Error`]。
    ///
    /// The plugin confirms the handshake. On a version mismatch the plugin should answer with
    /// [`Msg::Error`].
    Ready { protocol: u32 },
    /// 一块像素。`len` 是随后紧跟的字节数，必须等于 `w * h * 3`。
    ///
    /// One tile of pixels. `len` is the number of bytes that follow and must equal `w * h * 3`.
    Tile {
        seq: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        px: String,
        len: usize,
    },
    /// 干完了，插件可以退出。
    ///
    /// Work is done; the plugin may exit.
    End,
    /// 宿主不要结果了（用户取消、或者超时）。
    ///
    /// The host no longer wants the result, either because the user cancelled or because a
    /// timeout fired.
    Cancel,
    /// 任何一端出错。
    ///
    /// Either side failed.
    Error { msg: String },
}

impl Msg {
    fn to_line(&self) -> Result<String, String> {
        let mut s = serde_json::to_string(self).map_err(|e| format!("编码协议帧失败：{e}"))?;
        s.push('\n');
        Ok(s)
    }
}

/// 一次调用的结果。
///
/// Result of one call.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 从发 hello 到收完最后一块花了多久。界面用它决定要不要提示"这个插件有点慢"。
    ///
    /// Wall-clock time from sending hello to receiving the last tile. The UI uses it to decide
    /// whether to warn that the plugin is slow.
    pub elapsed: Duration,
}

/// 用户取消时的约定错误串。界面侧靠它区分"用户取消"与"真失败"：
/// 取消只清状态、不进错误横幅、不计崩溃。判断用 `contains`，
/// 与导出、体检那条口径一致。
///
/// The agreed error string for a user cancellation.
/// The UI uses it to separate a cancellation from a real failure: a cancellation only clears
/// state, never reaches the error banner, and is not counted as a crash.
/// Matching uses `contains`, the same convention as export and the self-check.
pub const ERR_CANCELLED: &str = "已取消";

/// 跑起来的插件进程。
///
/// A running plugin process.
pub struct Host {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    id: String,
    /// 看门票据：每次布防 +1，旧看门线程醒来发现自己的票据过期就自行退出。
    /// 用计数而不是布尔，是因为「解除→重新布防」可能落在旧线程睡眠的间隙
    /// 里 —— 布尔分不清"这是谁的看门"，会出现旧线程替新任务守到点、把
    /// 刚布防的新超时误杀的竞态。
    watch: Arc<AtomicU64>,
}

impl Host {
    /// 起进程并完成握手。
    ///
    /// `entry` 是可执行文件绝对路径；工作目录设成清单所在目录，
    /// 插件按相对路径找自己的资源才不会失败。
    ///
    /// Spawns the process and completes the handshake.
    ///
    /// `entry` is the absolute path of the executable.
    /// The working directory is set to the directory of the manifest, so a plugin that resolves
    /// its own resources by relative path still works.
    pub fn start(entry: &Path, id: &str, params: &BTreeMap<String, f64>) -> Result<Self, String> {
        Self::start_with(entry, id, params, HANDSHAKE_TIMEOUT)
    }

    /// 同 [`Host::start`]，握手超时可调 —— 测试用它把 30 秒压成几百毫秒。
    fn start_with(
        entry: &Path,
        id: &str,
        params: &BTreeMap<String, f64>,
        handshake_timeout: Duration,
    ) -> Result<Self, String> {
        if !entry.is_file() {
            return Err(format!("插件入口不存在：{}", entry.display()));
        }
        let mut child = Command::new(entry)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr 继承而不是吞掉：插件自己的日志要能在控制台看见，
            // 否则插件作者根本没法调试。
            .stderr(Stdio::inherit())
            .current_dir(entry.parent().unwrap_or(Path::new(".")))
            .spawn()
            .map_err(|e| format!("起不了插件进程（{}）：{e}", entry.display()))?;
        let stdin = child.stdin.take().ok_or("插件进程没有 stdin")?;
        let stdout = child.stdout.take().ok_or("插件进程没有 stdout")?;
        let mut host = Self {
            child,
            stdin,
            out: BufReader::with_capacity(64 * 1024, stdout),
            id: id.to_owned(),
            watch: Arc::new(AtomicU64::new(0)),
        };
        host.write(&Msg::Hello {
            protocol: PROTOCOL,
            id: id.to_owned(),
            params: params.clone(),
        })?;
        // 握手也是一次没有读超时的阻塞读，必须立刻布防。之前 watchdog
        // 只在 run_tiles 前 arm：插件启动即挂死的话，Host::start 会永远
        // 卡在等 Ready 上，任务栏永远"运行中"，常规路径没有任何兜底。
        host.arm_watchdog(handshake_timeout);
        let handshaked = match host.read_msg() {
            Ok(Msg::Ready { protocol }) if protocol >= PROTOCOL => Ok(()),
            Ok(Msg::Ready { protocol }) => Err(format!(
                "插件协议太旧（它说 {protocol}，宿主需要 {PROTOCOL}）：{id}"
            )),
            Ok(Msg::Error { msg }) => Err(format!("插件拒绝启动：{msg}")),
            Ok(other) => Err(format!("握手时收到意料之外的消息：{other:?}")),
            Err(e) => Err(e),
        };
        match handshaked {
            Ok(()) => {
                // 握手成功先解除看门：从这里到 apply 之间没有阻塞读，
                // 留着看门反而会把"加载慢但正常"的插件误杀。
                // apply_progress 会用 TIMEOUT 重新布防。
                host.disarm();
                Ok(host)
            }
            Err(e) => {
                // 失败也要收尸：进程还挂着就杀干净，别留一个僵尸占着管道。
                host.abort();
                Err(e)
            }
        }
    }

    /// 把一整张图丢进去，拿回一整张图。
    ///
    /// 分块是协议层的事：调用方给整图，这里按 [`TILE`] 切开逐块往返。
    /// 之所以不让调用方自己切，是因为"插件必须收整块"这条约束不该漏到界面层。
    /// 不需要进度与取消的调用方用这个入口；要进度/取消走 [`Host::apply_progress`]。
    ///
    /// Sends a whole image in and gets a whole image back.
    ///
    /// Tiling belongs to the protocol layer: the caller passes the full image and this splits it
    /// by [`TILE`], round-tripping one block at a time.
    /// The caller does not tile itself because the rule that a plugin receives only whole blocks
    /// must not leak into the UI layer.
    /// Callers that need neither progress nor cancellation use this entry point; for either, use
    /// [`Host::apply_progress`].
    pub fn apply(&mut self, w: u32, h: u32, rgb: &[u8]) -> Result<Applied, String> {
        self.apply_progress(w, h, rgb, |_, _| true)
    }

    /// 同 [`Host::apply`]，但每块收发之间向调用方报进度，并允许中途取消。
    ///
    /// `progress(done, total)` 的参数是 (已完成块数, 总块数)，总块数 =
    /// ceil(w/TILE) × ceil(h/TILE)。回调在**每块发送之前**调用一次，返回 `false`
    /// 表示用户要取消：宿主立刻杀掉子进程、等它退干净（不留僵尸占管道），
    /// 然后返回 [`ERR_CANCELLED`]。全部块收完之后还会补一报 `(total, total)`，
    /// 让进度条能真正走到头 —— 那一次的返回值忽略，图已经处理完了。
    ///
    /// Same as [`Host::apply`], but reports progress between blocks and allows cancellation.
    ///
    /// The arguments of `progress(done, total)` are the finished block count and the total block
    /// count, where the total is ceil(w/TILE) × ceil(h/TILE).
    /// The callback runs once **before each block is sent**; returning `false` means the user
    /// wants to cancel: the host kills the child at once, waits for it to exit cleanly so no
    /// zombie holds the pipe, and then returns [`ERR_CANCELLED`].
    /// Once every block is in, one more `(total, total)` report follows so the progress bar can
    /// reach the end; the return value of that call is ignored because the image is already done.
    pub fn apply_progress(
        &mut self,
        w: u32,
        h: u32,
        rgb: &[u8],
        mut progress: impl FnMut(u32, u32) -> bool,
    ) -> Result<Applied, String> {
        let want = (w as usize) * (h as usize) * 3;
        if rgb.len() != want {
            return Err(format!(
                "像素数与尺寸不符：{}×{} 需要 {want} 字节，收到 {}",
                w,
                h,
                rgb.len()
            ));
        }
        // 分块往返拆到内部闭包：任何一环出错（写帧、读帧、块尺寸不符）都要
        // 走同一个收尾——把可能还活着的子进程杀干净再报错。之前错误直接 `?`
        // 上升，僵尸进程会一直占着管道（审查发现的泄漏路径）。
        let started = Instant::now();
        self.arm_watchdog(TIMEOUT);
        match self.run_tiles(w, h, rgb, &mut progress) {
            Ok(applied) => {
                // 正常收工先解除看门，再算账返回。
                self.disarm();
                Ok(applied)
            }
            Err(e) => {
                if e != ERR_CANCELLED {
                    // 用户取消已经自己 abort 过了，别杀两次。
                    self.abort();
                }
                Err(e)
            }
        }
        .map(|mut applied| {
            applied.elapsed = started.elapsed();
            applied
        })
    }

    /// 分块收发主体：由 [`Host::apply_progress`] 包裹，出错时由它收尾。
    fn run_tiles(
        &mut self,
        w: u32,
        h: u32,
        rgb: &[u8],
        progress: &mut impl FnMut(u32, u32) -> bool,
    ) -> Result<Applied, String> {
        let want = (w as usize) * (h as usize) * 3;
        let cols = w.div_ceil(TILE);
        let rows = h.div_ceil(TILE);
        let total = cols * rows;
        let mut out = vec![0u8; want];
        let mut seq = 0u32;
        let mut done = 0u32;
        let mut y = 0;
        while y < h {
            let mut x = 0;
            while x < w {
                // 取消检查点：每块的收发之间。这里退出是最干净的时机 ——
                // 没有半块像素悬在管道里。
                if !progress(done, total) {
                    self.abort();
                    return Err(ERR_CANCELLED.to_owned());
                }
                let tw = TILE.min(w - x);
                let th = TILE.min(h - y);
                let mut block = Vec::with_capacity((tw * th * 3) as usize);
                for row in 0..th {
                    let start = (((y + row) * w + x) * 3) as usize;
                    let end = start + (tw * 3) as usize;
                    block.extend_from_slice(&rgb[start..end]);
                }
                self.write(&Msg::Tile {
                    seq,
                    x,
                    y,
                    w: tw,
                    h: th,
                    px: "rgb8".to_owned(),
                    len: block.len(),
                })?;
                self.stdin
                    .write_all(&block)
                    .map_err(|e| format!("发送像素失败：{e}"))?;
                self.stdin
                    .flush()
                    .map_err(|e| format!("发送像素失败：{e}"))?;

                let (head, back) = self.read_tile()?;
                if head.w != tw || head.h != th {
                    return Err(format!(
                        "插件返回的块尺寸不对：要 {tw}×{th}，给 {}×{}",
                        head.w, head.h
                    ));
                }
                for row in 0..th {
                    let src = (row * tw * 3) as usize;
                    let dst = (((y + row) * w + x) * 3) as usize;
                    out[dst..dst + (tw * 3) as usize]
                        .copy_from_slice(&back[src..src + (tw * 3) as usize]);
                }
                seq += 1;
                done += 1;
                x += tw;
            }
            y += TILE.min(h - y);
        }
        // 最后一报：done 追平 total，进度条才能到 100%。返回值忽略 ——
        // 这之后再"取消"已经没有东西可取消了。
        let _ = progress(total, total);
        self.write(&Msg::End)?;
        Ok(Applied {
            pixels: out,
            width: w,
            height: h,
            // 计时在外层统一盖：这里拿不到 started。
            elapsed: Duration::ZERO,
        })
    }

    /// 只看不跑：让插件报告自己的状态（调试用）。
    ///
    /// Read only, nothing runs: reports the plugin state for debugging.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 子进程是否已经退出。取消收尾之后用来验尸：退出才是收干净了。
    ///
    /// Whether the child process has already exited. Used to inspect the aftermath of a
    /// cancellation: only an exited process counts as a clean cleanup.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// 用户取消后的干净收尾：杀进程、等它退。进程一死，管道两端自然断开 ——
    /// 之后任何读写都会失败，宿主不会再被误用。
    fn abort(&mut self) {
        self.disarm();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// 收工。先好好说一声，再等它自己退；不按时退就杀。
    ///
    /// Finish up. Says goodbye first, then waits for the plugin to exit on its own; a plugin that
    /// misses the deadline is killed.
    pub fn shutdown(mut self) {
        self.disarm();
        let _ = self.write(&Msg::End);
        let _ = self.stdin.flush();
        drop(self.stdin);
        // 给 2 秒自己退，不退就杀：插件卡在收尾里不该让整个程序关不掉。
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// 起一个"到点就杀"的看门线程。
    ///
    /// std 的管道没有读超时，唯一可靠的办法是从外面把进程杀掉 ——
    /// 进程一死，阻塞中的 `read` 立刻返回 EOF，主线程就能报错返回。
    /// 之前这根线程到点只会 eprintln 一声就完事（主线程还卡在 read 上，
    /// 根本看不见），超时保护等于没有 —— 现在直接按 pid 补刀。
    ///
    /// 看门以"票据"认主：布防时把 [`Self::watch`] 加一，线程只认自己
    /// 布防那一刻的票号。解除（[`Self::disarm`]）或新一轮布防都会把票号
    /// 顶掉，旧线程醒来发现过期就退出 —— 用计数而不是布尔，是因为
    /// "解除→重新布防"可能落在旧线程睡眠的间隙里，布尔分不清是谁的看门。
    fn arm_watchdog(&self, timeout: Duration) {
        let watch = Arc::clone(&self.watch);
        let ticket = watch.fetch_add(1, Ordering::Relaxed) + 1;
        let pid = self.child.id();
        std::thread::spawn(move || {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if watch.load(Ordering::Relaxed) != ticket {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if watch.load(Ordering::Relaxed) == ticket {
                // 子进程句柄归主线程所有，但按 pid 强杀是跨线程安全的：
                // 进程一死，主线程阻塞中的 read 立刻 EOF 报错返回。
                eprintln!("[aevum] 插件进程 {pid} 超时 {timeout:?}，强制中断");
                Self::kill_pid(pid);
            }
        });
    }

    /// 解除当前所有看门：把票据往前推，睡眠中的看门线程醒来发现自己
    /// 过期就退出。`fetch_add` 而不是 `compare_exchange` —— "解除"的
    /// 语义就是顶掉现存布防，不必知道自己布防时的票号是多少。
    fn disarm(&self) {
        self.watch.fetch_add(1, Ordering::Relaxed);
    }

    fn write(&mut self, msg: &Msg) -> Result<(), String> {
        let line = msg.to_line()?;
        self.stdin
            .write_all(line.as_bytes())
            .map_err(|e| format!("写协议帧失败（进程可能已退出）：{e}"))?;
        self.stdin.flush().map_err(|e| format!("写协议帧失败：{e}"))
    }

    /// 按 pid 强杀插件进程（看门线程用：子进程句柄归主线程，但 pid 是全局的）。
    /// 进程可能已经自己退了——两种情况都当作成功，错误一律吞掉。
    fn kill_pid(pid: u32) {
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW：控制台应用被杀前别再弹一个黑框。
            use std::os::windows::process::CommandExt;
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .creation_flags(0x0800_0000)
                .output();
        }
        #[cfg(not(windows))]
        {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
        }
    }

    fn read_msg(&mut self) -> Result<Msg, String> {
        let Some(line) = read_line_limited(&mut self.out, MAX_LINE)? else {
            return Err("插件进程提前退出（没等到回复）".to_owned());
        };
        serde_json::from_str(line.trim()).map_err(|e| format!("插件发出的帧不是合法 JSON：{e}"))
    }

    /// 读一块像素：先一行 JSON，再按 `len` 读裸字节。
    fn read_tile(&mut self) -> Result<(TileHead, Vec<u8>), String> {
        match self.read_msg()? {
            Msg::Tile { w, h, len, px, .. } => {
                if px != "rgb8" {
                    return Err(format!("插件回了不认识的像素格式：{px}"));
                }
                if len != (w as usize) * (h as usize) * 3 {
                    return Err(format!("插件自报的字节数对不上：{w}×{h} 却说 {len} 字节"));
                }
                if len > MAX_FRAME {
                    return Err(format!("插件要发 {len} 字节，超过单帧上限 {MAX_FRAME}"));
                }
                let mut buf = vec![0u8; len];
                self.out
                    .read_exact(&mut buf)
                    .map_err(|e| format!("读像素失败：{e}"))?;
                Ok((TileHead { w, h }, buf))
            }
            Msg::Error { msg } => Err(format!("插件报错：{msg}")),
            other => Err(format!("期待一块像素，收到：{other:?}")),
        }
    }
}

struct TileHead {
    w: u32,
    h: u32,
}

/// 读一行协议帧，长度超过 `limit` 立刻报错断开。
///
/// `BufRead::read_line` 会一直吃到换行为止，中间没有任何上限 —— 对面的
/// 进程（不管是坏插件还是被攻破的宿主）只要不停吐字节就能把这一侧吃成
/// OOM。`None` = 对端干净关闭且没给任何字节（EOF），调用方按"提前退出"
/// 组织自己的错误话术；超限/IO 错误直接 Err。
fn read_line_limited(reader: &mut impl BufRead, limit: usize) -> Result<Option<String>, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    loop {
        let available = match reader.fill_buf() {
            Ok(slice) => slice,
            Err(e) => return Err(format!("读协议帧失败：{e}")),
        };
        if available.is_empty() {
            // EOF：有半行也算一行（对端没写完就退了，内容交上去让它自己
            // 报"不是合法 JSON"），一字节没有才是真正的"什么都没等到"。
            return if buf.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(buf)
                    .map(Some)
                    .map_err(|e| format!("协议帧不是 UTF-8：{e}"))
            };
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                if buf.len() + pos > limit {
                    return Err(format!("协议帧超过 {limit} 字节上限，疑似坏进程在刷屏"));
                }
                buf.extend_from_slice(&available[..pos]);
                reader.consume(pos + 1);
                return String::from_utf8(buf)
                    .map(Some)
                    .map_err(|e| format!("协议帧不是 UTF-8：{e}"));
            }
            None => {
                if buf.len() + available.len() > limit {
                    // 不用等读完这一行：已经注定超限，立刻断省内存。
                    return Err(format!("协议帧超过 {limit} 字节上限，疑似坏进程在刷屏"));
                }
                buf.extend_from_slice(available);
                let len = available.len();
                reader.consume(len);
            }
        }
    }
}

// ------------------------------------------------------------ 插件侧（供示例用）

/// 插件端的一条连接。示例插件直接用它写 main。
///
/// 放在核心里而不是示例里，是为了让"写一个插件"只有二十行 ——
/// 作者该操心的是滤镜本身，不是协议。
///
/// One connection on the plugin side. Example plugins use it directly in `main`.
///
/// It lives in the core crate rather than the examples so that writing a plugin takes about
/// twenty lines: the author deals with the filter itself, not the protocol.
pub struct PluginSide {
    stdin: std::io::Stdout,
    out: BufReader<std::io::Stdin>,
    pub params: BTreeMap<String, f64>,
    pub id: String,
}

/// 一块瓦片 `(seq, x, y, w, h, 像素)`。`None` = 宿主说了 `end` / `cancel`。
type TileResult = Result<Option<(u32, u32, u32, u32, u32, Vec<u8>)>, String>;

impl PluginSide {
    /// 握手。返回 Err 表示宿主发了不认识的帧，插件应当退出。
    ///
    /// Handshake. An Err means the host sent a frame this side does not understand, and the
    /// plugin should exit.
    pub fn handshake() -> Result<Self, String> {
        let mut side = Self {
            stdin: std::io::stdout(),
            out: BufReader::new(std::io::stdin()),
            params: BTreeMap::new(),
            id: String::new(),
        };
        let Some(line) = read_line_limited(&mut side.out, MAX_LINE)? else {
            return Err("宿主没发 hello 就关了管道".to_owned());
        };
        match serde_json::from_str::<Msg>(line.trim()).map_err(|e| e.to_string())? {
            Msg::Hello {
                protocol,
                id,
                params,
            } => {
                if protocol > PROTOCOL {
                    side.send(&Msg::Error {
                        msg: format!("插件只认协议 {PROTOCOL}，宿主用的是 {protocol}"),
                    })?;
                    return Err("协议版本不兼容".to_owned());
                }
                side.id = id;
                side.params = params;
                side.send(&Msg::Ready { protocol: PROTOCOL })?;
                Ok(side)
            }
            other => Err(format!("期待 hello，收到 {other:?}")),
        }
    }

    pub fn send(&mut self, msg: &Msg) -> Result<(), String> {
        let line = msg.to_line()?;
        self.stdin
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())
    }

    /// 收一块像素 `(seq, x, y, w, h, 像素)`。返回 `None` 表示宿主说了 `end` 或 `cancel`。
    ///
    /// Receives one tile as `(seq, x, y, w, h, pixels)`. `None` means the host said `end` or
    /// `cancel`.
    pub fn recv_tile(&mut self) -> TileResult {
        let Some(line) = read_line_limited(&mut self.out, MAX_LINE)? else {
            return Ok(None);
        };
        match serde_json::from_str::<Msg>(line.trim()).map_err(|e| e.to_string())? {
            Msg::Tile {
                seq,
                x,
                y,
                w,
                h,
                len,
                ..
            } => {
                let mut buf = vec![0u8; len];
                self.out.read_exact(&mut buf).map_err(|e| e.to_string())?;
                Ok(Some((seq, x, y, w, h, buf)))
            }
            Msg::End | Msg::Cancel => Ok(None),
            Msg::Error { msg } => Err(msg),
            other => Err(format!("期待像素帧，收到 {other:?}")),
        }
    }

    /// 回一块像素。
    ///
    /// Sends one tile back.
    pub fn send_tile(
        &mut self,
        seq: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        px: &[u8],
    ) -> Result<(), String> {
        self.send(&Msg::Tile {
            seq,
            x,
            y,
            w,
            h,
            px: "rgb8".to_owned(),
            len: px.len(),
        })?;
        self.stdin.write_all(px).map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // 测试自己的"用户取消"旗，与宿主的看门票据（AtomicU64）无关。
    use std::sync::atomic::AtomicBool;

    /// 演示插件：把每个通道反相，按 `strength` 混回原图。
    ///
    /// 编成一个 example，测试直接起它 —— 这样测的是"真的跨进程跑"，
    /// 而不是在同进程里模拟一份协议。
    fn example_plugin(name: &str) -> Option<std::path::PathBuf> {
        // target/debug/deps/xxx.exe → target/debug/examples/<name>.exe
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.parent()?.join("examples");
        let file = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };
        let path = dir.join(file);
        path.is_file().then_some(path)
    }

    fn demo_plugin() -> Option<std::path::PathBuf> {
        example_plugin("invert")
    }

    /// 收到 hello 后装死不回的样本（examples/sleepy.rs）。
    fn sleepy_plugin() -> Option<std::path::PathBuf> {
        example_plugin("sleepy")
    }

    /// 回一句 error 然后赖着不退的样本（examples/grumpy.rs）。
    fn grumpy_plugin() -> Option<std::path::PathBuf> {
        example_plugin("grumpy")
    }

    #[test]
    fn round_trips_pixels_through_a_real_subprocess() {
        let Some(entry) = demo_plugin() else {
            // 没先 `cargo build --examples` 就跳过：这条测试证明的是跨进程链路，
            // 没有可执行文件就没有可测的东西，不该假通过也别假失败。
            eprintln!("跳过：没有构建 examples/invert，先跑 `cargo build --examples`");
            return;
        };
        let mut params = BTreeMap::new();
        params.insert("strength".to_owned(), 100.0);
        let mut host = Host::start(&entry, "aevum.example-invert", &params).expect("握手应当成功");

        // 33×17 非整块，顺手验证切块与拼回的行跨距。
        let (w, h) = (33u32, 17u32);
        let mut src = Vec::with_capacity((w * h * 3) as usize);
        for i in 0..(w * h) {
            src.push((i % 251) as u8);
            src.push((i % 241) as u8);
            src.push((i % 239) as u8);
        }
        let out = host.apply(w, h, &src).expect("反相应当跑通");
        assert_eq!(out.width, w);
        assert_eq!(out.height, h);
        assert_eq!(out.pixels.len(), src.len());
        // 100% 强度 = 完全反相。
        for (a, b) in src.iter().zip(out.pixels.iter()) {
            assert_eq!(*b, 255 - *a);
        }
        host.shutdown();
    }

    #[test]
    fn zero_strength_returns_the_input_untouched() {
        let Some(entry) = demo_plugin() else {
            return;
        };
        let mut params = BTreeMap::new();
        params.insert("strength".to_owned(), 0.0);
        let mut host = Host::start(&entry, "aevum.example-invert", &params).unwrap();
        let (w, h) = (8u32, 8u32);
        let src: Vec<u8> = (0..(w * h * 3)).map(|i| (i * 7 % 256) as u8).collect();
        let out = host.apply(w, h, &src).unwrap();
        assert_eq!(out.pixels, src, "0% 强度应当原样返回");
        host.shutdown();
    }

    #[test]
    fn a_missing_entry_fails_before_spawning_anything() {
        let err = Host::start(
            Path::new("没有这个文件.exe"),
            "aevum.none",
            &BTreeMap::new(),
        )
        .err()
        .expect("应当报错");
        assert!(err.contains("插件入口不存在"), "{err}");
    }

    #[test]
    fn size_mismatch_is_caught_before_the_process_sees_it() {
        let Some(entry) = demo_plugin() else {
            return;
        };
        let mut host = Host::start(&entry, "aevum.example-invert", &BTreeMap::new()).unwrap();
        let err = host.apply(4, 4, &[0u8; 10]).expect_err("应当报错");
        assert!(err.contains("像素数与尺寸不符"), "{err}");
        host.shutdown();
    }

    #[test]
    fn message_encoding_is_stable() {
        // 抓包看到的就是这一行，改字段名等于改协议。
        let line = Msg::Hello {
            protocol: 1,
            id: "nik.x".to_owned(),
            params: BTreeMap::new(),
        }
        .to_line()
        .unwrap();
        assert_eq!(
            line,
            "{\"t\":\"hello\",\"protocol\":1,\"id\":\"nik.x\",\"params\":{}}\n"
        );
        let back: Msg = serde_json::from_str(line.trim()).unwrap();
        assert!(matches!(back, Msg::Hello { protocol: 1, .. }));
    }

    #[test]
    fn progress_reaches_total_and_never_goes_backwards() {
        let Some(entry) = demo_plugin() else {
            return;
        };
        let mut host = Host::start(&entry, "aevum.example-invert", &BTreeMap::new()).unwrap();
        // 700×300 → ceil(700/512)×ceil(300/512) = 2×1 = 2 块，正好能看着进度走起来。
        let (w, h) = (700u32, 300u32);
        let src = vec![128u8; (w * h * 3) as usize];
        let mut calls: Vec<(u32, u32)> = Vec::new();
        let out = host
            .apply_progress(w, h, &src, |done, total| {
                calls.push((done, total));
                true
            })
            .expect("带进度的反相应当跑通");
        assert_eq!((out.width, out.height), (w, h));
        // 没传参数 = 满强度：128 全变 127（255−128）。
        assert!(
            out.pixels.iter().all(|&b| b == 127),
            "满强度反相应当把 128 变成 127"
        );
        // 最后一报到顶：进度条要能真正走到 100%。
        assert_eq!(
            *calls.last().expect("至少要报一次进度"),
            (2, 2),
            "收尾那一报应当是 (total, total)：{calls:?}"
        );
        // 总数不变、进度单调不减、且确实逐块报过（不止一两次心跳）。
        for pair in calls.windows(2) {
            assert_eq!(pair[0].1, pair[1].1, "总块数不该中途变：{calls:?}");
            assert!(pair[0].0 <= pair[1].0, "进度不该倒退：{calls:?}");
        }
        assert!(calls.len() >= 3, "每块收发之间都该报一次：{calls:?}");
        host.shutdown();
    }

    #[test]
    fn a_hung_plugin_cannot_block_the_handshake_forever() {
        // 回归：握手曾经没有看门（watchdog 只在 run_tiles 前布防），
        // 启动即挂死的插件会让 Host::start 永远阻塞，任务永远"运行中"。
        let Some(entry) = sleepy_plugin() else {
            eprintln!("跳过：没有构建 examples/sleepy，先跑 `cargo build --examples`");
            return;
        };
        let started = Instant::now();
        // Host 没有 Debug，用模式匹配取错误而不是 expect_err。
        let Err(err) = Host::start_with(
            &entry,
            "aevum.test-sleepy",
            &BTreeMap::new(),
            Duration::from_millis(500),
        ) else {
            panic!("挂死插件应当在握手超时后报错");
        };
        let took = started.elapsed();
        assert!(
            took < Duration::from_secs(10),
            "应当几百毫秒就报错返回，而不是挂死：{took:?}"
        );
        // 看门杀掉进程后，阻塞中的 read 立刻 EOF —— sleepy 自己睡 600 秒
        // 不会退，能读到 EOF 只可能是被看门补刀的。
        assert!(err.contains("没等到回复"), "{err}");
    }

    #[test]
    fn a_plugin_that_rejects_the_handshake_gets_killed_cleanly() {
        // 回归：握手失败路径曾经直接返回 Err 不收尸，grumpy 回完 error
        // 赖着不退，就会留一个僵尸进程占着管道。现在失败必须走 abort。
        let Some(entry) = grumpy_plugin() else {
            eprintln!("跳过：没有构建 examples/grumpy，先跑 `cargo build --examples`");
            return;
        };
        let Err(err) = Host::start(&entry, "aevum.test-grumpy", &BTreeMap::new()) else {
            panic!("拒绝握手的插件应当报错");
        };
        assert!(err.contains("插件拒绝启动"), "{err}");
    }

    #[test]
    fn oversized_lines_are_cut_off_instead_of_buffered_forever() {
        use std::io::Cursor;
        // 正常行照常读回。
        let mut r = Cursor::new(b"{\"t\":\"ready\"}\nrest".to_vec());
        let line = read_line_limited(&mut r, MAX_LINE).unwrap().unwrap();
        assert_eq!(line, "{\"t\":\"ready\"}");
        // 没换行、总量超限：不等读完，立刻报错 —— 这是 OOM 防线。
        let huge = vec![b'x'; MAX_LINE + 1];
        let mut r = Cursor::new(huge);
        let err = read_line_limited(&mut r, MAX_LINE).expect_err("超限行应当报错");
        assert!(err.contains("上限"), "{err}");
        // 半行 EOF：内容交上去让调用方报"不是合法 JSON"。
        let mut r = Cursor::new(b"{\"t\":\"ready\"}".to_vec());
        let line = read_line_limited(&mut r, MAX_LINE).unwrap().unwrap();
        assert_eq!(line, "{\"t\":\"ready\"}");
        // 干净 EOF：None，调用方报"提前退出"。
        let mut r = Cursor::new(Vec::new());
        assert!(read_line_limited(&mut r, MAX_LINE).unwrap().is_none());
    }

    #[test]
    fn a_firehose_plugin_is_cut_off_at_the_handshake() {
        // 回归：握手读曾用无上限的 read_line —— 坏插件狂吐不带换行的
        // 字节就能把宿主吃成 OOM。现在限长防线在握手就生效。
        let Some(entry) = example_plugin("firehose") else {
            eprintln!("跳过：没有构建 examples/firehose，先跑 `cargo build --examples`");
            return;
        };
        let Err(err) = Host::start(&entry, "aevum.test-firehose", &BTreeMap::new()) else {
            panic!("刷屏插件应当在握手上限处被断开");
        };
        assert!(err.contains("上限"), "{err}");
    }

    #[test]
    fn cancel_between_tiles_kills_the_child_and_reports_it() {
        let Some(entry) = demo_plugin() else {
            return;
        };
        let mut host = Host::start(&entry, "aevum.example-invert", &BTreeMap::new()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        // 600×100 → 2 块：收到第 1 块之后（下一次回调 done==1）置位取消，
        // 宿主应当在发第 2 块之前干净退出。
        let (w, h) = (600u32, 100u32);
        let src = vec![7u8; (w * h * 3) as usize];
        let result = host.apply_progress(w, h, &src, |done, _total| {
            if done >= 1 {
                flag.store(true, Ordering::Relaxed);
            }
            !flag.load(Ordering::Relaxed)
        });
        let err = result.expect_err("取消应当报错而不是给半个结果");
        assert!(err.contains(ERR_CANCELLED), "{err}");
        // 收尸：子进程必须已经退出，不能留一个僵尸占着管道。
        assert!(host.has_exited(), "取消之后子进程应当被杀干净");
        // 收尾后的宿主已不可用：再来一次要报错，而不是静默给出旧管道的结果。
        assert!(
            host.apply(4, 4, &[0u8; 48]).is_err(),
            "取消后的宿主不该还能干活"
        );
        // shutdown 走一遍正常收尾：子进程已退出，应当立刻返回而不是干等 2 秒。
        host.shutdown();
    }
}
