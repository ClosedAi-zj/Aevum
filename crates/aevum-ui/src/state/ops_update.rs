//! 内置更新器的界面侧接线：启动静默检查、手动检查、下载安装与回滚。
//!
//! 本文件只做**编排**，一件判断都不自己发明：版本比较、资产挑选、校验和、解包、
//! 替换脚本全部调 `aevum_core::update` 的现成纯函数。这里负责的是把它们串成一条
//! 不阻塞界面线程、可重复点击、迟到回包不会串台的后台流程。
//!
//! # 三条硬规矩
//!
//! 1. **界面线程绝不阻塞**：检查、下载、校验、解包全在 `thread::spawn` 里，结果经
//!    [`Msg`] 回包，带 `update_job` 编号防迟到回包乱序、防重复点击（阶段机
//!    [`UpdatePhase`] 就是那把锁）。
//! 2. **进度不回消息**：`download_to_file` 每 50 ms 回调一次，一秒钟二十条消息只为
//!    搬一个数没有意义，而且界面卡两帧就积压出二十条过期读数。进度写进
//!    `Arc<Mutex<(u64, u64)>>`，窗口自己每帧读一次、自己请求重绘。
//! 3. **失败不出弹窗**：静默检查失败只在更新窗口里留一行原因，绝不打断用户正在做的
//!    事——更新是后台杂务，不是业务。
//!
//! # 为什么安装脚本要「备好」再让用户点
//!
//! 下载、校验、解包、写脚本做完之后，程序**不立刻退出**：窗口显示「已准备好」并给出
//! 一颗「立即重启并安装」。理由是这三件事里任何一件失败（断网、校验不过、杀软锁文件）
//! 都应该让用户看见并留在原地，而不是眼睁睁看着程序自己关掉又没装成。真正的替换动作
//! 只有「拉起脚本 + 退出」一步，那一步在用户点下去之后瞬间完成，界面线程无阻塞。
//!
//! UI-side wiring for the built-in updater: the silent startup check, the manual check,
//! the download-and-install job, and rollback.
//!
//! This file only **orchestrates** and invents no judgement of its own: version compare,
//! asset pick, checksum, unpacking and the replacement script all call the existing pure
//! functions in `aevum_core::update`. What lives here is the wiring: a background flow that
//! never blocks the UI thread, tolerates repeated clicks, and cannot cross-talk between
//! late replies.
//!
//! # Three hard rules
//!
//! 1. **The UI thread never blocks**: check, download, verify and unpack all run inside
//!    `thread::spawn`, and results come back as [`Msg`] with an `update_job` number that
//!    both drops late replies and prevents double clicks (the phase machine
//!    [`UpdatePhase`] is that lock).
//! 2. **Progress does not travel as messages**: `download_to_file` calls back every 50 ms,
//!    so one message per tick is twenty messages a second to move a single number, and a
//!    two-frame UI stall piles up twenty stale readings. Progress goes into a shared
//!    `Arc<Mutex<(u64, u64)>>` instead, which the window reads once per frame and drives
//!    its own repaint from.
//! 3. **Failures never pop a dialog**: a failed silent check leaves one line of reason in
//!    the update window and nothing else -- updating is background housekeeping, not the
//!    user's work.
//!
//! # Why the installer is "prepared" and then clicked
//!
//! After downloading, verifying, unpacking and writing the script, the program does **not**
//! quit by itself: the window says "ready" and offers "restart and install". The reason is
//! that any of those three steps can fail (offline, bad checksum, an antivirus holding the
//! file), and the user should see that and stay put instead of watching the app close
//! without having installed anything. The actual replacement is only "launch the script,
//! then exit", which completes instantly when the user clicks and blocks nothing.

use std::path::{Path, PathBuf};use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use aevum_core::update;

use super::*;

/// 静默检查的最小间隔（秒）：一天。
///
/// Minimum interval between silent checks, in seconds: one day.
const AUTO_CHECK_SECS: u64 = 24 * 60 * 60;

/// 现在（Unix 秒）。取不到时间就返回 0：那会让「该查了」判定为假，宁可不查。
///
/// `pub(super)` 而非私有：结论回包的处理在 `ops_session.rs` 的 `poll` 里，它也要
/// 写这一项时间戳；两边各写一遍取值逻辑，迟早会出现「一边按秒、一边按毫秒」。
///
/// Now, in Unix seconds. A failure to read the clock yields 0, which makes the "due"
/// test false -- better to skip than to hammer.
///
/// `pub(super)` rather than private: the reply is handled in `poll` over in
/// `ops_session.rs`, which writes this same timestamp. Two copies of the clock logic
/// would eventually disagree about seconds versus milliseconds.
pub(super) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 路径的展示文本（Windows 下就是带盘符的原样）。
///
/// The display text of a path (on Windows, the unchanged `C:\...` form).
fn path_text(p: &Path) -> String {
    p.display().to_string()
}

/// 后台线程里的那一次检查：一次网络请求，产出结论 + 正文 + 清单地址。
///
/// 为什么带一个 `RefCell` 把 JSON 抄下来：`check_remote` 只往回带「结论」，发布说明
/// 正文和 `SHA256SUMS` 的地址都不在结论里；再取一次网络会拿到**另一份**快照（发布
/// 可能刚更新），于是窗口显示的说明和实际要装的包可能不是同一次发布的。这里复用之
/// 前那一次下载的字节，保证三者同源。
///
/// The one check that runs on the background thread: a single network request producing
/// the conclusion plus the body and the manifest URL.
///
/// Why a `RefCell` stashes the JSON: `check_remote` only hands back the *conclusion*, and
/// neither the release-notes body nor the `SHA256SUMS` URL is part of it. A second request
/// would fetch **another** snapshot (a release may have just landed), so the notes on
/// screen might belong to a different release than the package about to be installed. The
/// bytes of the first fetch are reused so all three stay from one release.
fn run_check(api: &str) -> UpdateCheckOut {
    // `Arc<Mutex<..>>` 而不是 `RefCell`：`Fetcher` 是 `&dyn Fn(..) + 'static`，闭包里
    // 借一个外部 `RefCell` 会让它拿不到 `'static`。共享所有权既满足约束，也顺手把
    // 「写」和「读」之间那点借用关系换成了一次无竞争的加锁。
    //
    // `Arc<Mutex<..>>` rather than `RefCell`: `Fetcher` is `&dyn Fn(..) + 'static`, and a
    // closure borrowing an outside `RefCell` cannot be `'static`. Shared ownership satisfies
    // the bound and turns the write/read borrow into one uncontended lock.
    let stash: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let sink = stash.clone();
    let fetch = move |url: &str| -> Result<Vec<u8>, String> {
        let bytes = update::fetch(url)?;
        if let Ok(text) = std::str::from_utf8(&bytes)
            && let Ok(mut slot) = sink.lock()
        {
            *slot = Some(text.to_owned());
        }
        Ok(bytes)
    };
    let fetcher: &update::Fetcher = &fetch;
    let check = update::check_remote(
        fetcher,
        update::current_version(),
        api,
        update::ASSET_SUFFIX,
    );

    let mut body = String::new();
    let mut sums_url = None;
    let text = stash.lock().ok().and_then(|slot| slot.clone());
    if let Some(text) = text.as_deref()
        && let Ok(rel) = update::parse_release(text)
    {
        body = rel.body;
        // `SHA256SUMS` 也是发布资产，但它不是「要装的包」，所以 `UpdateCheck` 里没有
        // 它的位置，只能自己按名字挑。
        //
        // `SHA256SUMS` is a release asset too, but it is not "the package to install",
        // so `UpdateCheck` has no slot for it and it is picked here by name.
        sums_url = rel
            .assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(update::SUMS_ASSET))
            .map(|a| a.url.clone());
    }

    UpdateCheckOut {
        check,
        body,
        sums_url,
    }
}

/// 更新专用的临时目录：`%TEMP%/aevum-update-<pid>`。
///
/// 带上 pid：同一台机器上开两个实例不会互相覆盖半截包。**不带时间戳**，而且进来先清空：
/// 同一个进程里反复点「下载并安装」（上次校验没过、杀软锁了文件）不该每点一次就在
/// `%TEMP%` 里多留一份 56 MB 的 zip。进程退出后这份残留由系统清，和所有临时文件一样。
///
/// The update's own temp directory: `%TEMP%/aevum-update-<pid>`.
///
/// pid goes in the name so two instances on one machine cannot overwrite each other's
/// half-downloaded package. There is deliberately **no** timestamp, and the directory is
/// wiped on entry: retrying "download and install" in the same process (a failed checksum,
/// an antivirus lock) must not leave another 56 MB zip behind on every attempt. Whatever
/// remains after the process exits is cleaned up by the OS, like any other temp file.
fn work_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("aevum-update-{}", std::process::id()));
    // 先清再建：上一轮失败留下的半截包如果不删，`expand_exe` 的清空逻辑会把它当输入。
    // Clear before create: a half package left by a failed attempt would otherwise be
    // treated as input when `expand_exe` wipes its own subdirectory.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| {
        crate::i18n::tr_fmt(
            "建不了更新临时目录（{}）：{}",
            &[&path_text(&dir), &aevum_core::errmsg::clip_echo(&e.to_string())],
        )
    })?;
    Ok(dir)
}

/// 落盘用的资产文件名：只取最后一段。
///
/// 资产名来自网络，`..\..\x.zip` 这种名字必须在这里就被削成一个裸文件名，否则
/// 下载目标会跑出临时目录。空名字直接拒绝——写到一个没有名字的文件上只会让后面
/// 的校验报一个与真实原因无关的错。
///
/// The file name used on disk: the last component only.
///
/// Asset names come from the network, so a `..\..\x.zip` style name must be cut down to a
/// bare file name here, or the download target escapes the temp directory. An empty name is
/// rejected outright: writing to a nameless file only makes the later checksum fail for an
/// unrelated reason.
fn asset_file_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(crate::i18n::t!("发布信息里的资产名是空的").to_owned());
    }
    match Path::new(name).file_name() {
        Some(f) if !f.is_empty() => Ok(f.to_string_lossy().into_owned()),
        _ => Err(crate::i18n::tr_fmt(
            "资产名不能用：{}",
            &[&aevum_core::errmsg::clip_echo(name)],
        )),
    }
}

/// 当前进程的可执行文件路径。
///
/// The executable path of the current process.
fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| {
        crate::i18n::tr_fmt(
            "找不到当前程序路径：{}",
            &[&aevum_core::errmsg::clip_echo(&e.to_string())],
        )
    })
}

/// `<exe>.old`：安装脚本留下的上一版备份（回滚的原料）。
///
/// 拼接方式必须与 `aevum_core::update::install_script` 里的 `{target}.old` 逐字一致，
/// 否则这里判「有备份」而脚本找不到文件。
///
/// `<exe>.old`: the previous build kept by the install script (the raw material for a
/// rollback).
///
/// The concatenation must match `{target}.old` in `aevum_core::update::install_script` byte
/// for byte, or this side claims a backup exists while the script cannot find it.
fn rollback_backup(target_exe: &Path) -> PathBuf {
    PathBuf::from(format!("{}.old", target_exe.display()))
}

/// 用 `cmd /c start` 把批处理拉起来，本进程不等待。
///
/// 为什么必须走 `start`：脚本第一步就是「等本进程退出」。用 `cmd /c <脚本>` 直接跑，
/// 父进程被杀的时机就决定了脚本能不能活下去；`start` 把脚本交给外壳，进程树一断它
/// 照跑。
///
/// `start ""` 那个空参数是**窗口标题位**：省掉它，路径里带空格时第一个带引号的参数
/// 会被当成标题吞掉，脚本根本不会被执行。
///
/// Launch the batch file through `cmd /c start` without waiting for it.
///
/// Why `start` is mandatory: the script's first step is "wait for this process to exit".
/// Running `cmd /c <script>` directly ties the script's survival to our own exit timing,
/// while `start` hands it to the shell so it keeps running after the process tree is gone.
///
/// The empty `""` argument is the **window-title slot**: without it, the first quoted
/// argument is swallowed as the title when the path contains spaces, and the script never
/// runs.
fn spawn_detached(script: &Path) -> Result<(), std::io::Error> {
    let mut cmd = Command::new("cmd");
    cmd.arg("/c").arg("start").arg("").arg(script.as_os_str());
    // 工作目录设在脚本旁边：脚本里写的是绝对路径，但某些系统上 `start` 会先按当前
    // 目录解析相对项，指过去一步就少一类意外。
    //
    // The working directory is set next to the script: its contents are absolute paths,
    // but on some systems `start` resolves relative items against the current directory
    // first, and pointing there removes one class of surprise.
    if let Some(dir) = script.parent() {
        cmd.current_dir(dir);
    }
    cmd.spawn().map(|_child| ())
}

/// 后台线程里的下载安装准备：下载 → 取清单 → 校验 → 解包 → 写脚本。
///
/// 顺序与 `aevum_core::update::download_verified` 的取舍一致（先清单后资产），区别只有
/// 一处：这里资产是**先落盘**的（进度要真实字节数），所以清单在下载之后取。校验仍然
/// 用的是「新取的清单 + 落盘的字节」，中间没有信任断点。
///
/// 全部成功才返回脚本路径；任何一步失败都返回中文原因，由窗口用红字显示。
///
/// The download-and-prepare step on the background thread: download, fetch the manifest,
/// verify, unpack, write the script.
///
/// The order follows `aevum_core::update::download_verified` (manifest first, asset second)
/// with one difference: the asset is written to disk **first** here, because real byte
/// counts are what the progress bar needs, so the manifest is fetched afterwards.
/// Verification still uses "freshly fetched manifest + bytes on disk", with no gap of trust
/// in between.
///
/// The script path is returned only when everything succeeded; any failed step returns a
/// Chinese reason for the window to show in red.
fn run_install(
    asset: update::ReleaseAsset,
    version: String,
    sums_url: String,
    progress: Arc<Mutex<(u64, u64)>>,
) -> Result<PreparedInstall, String> {
    let target = current_exe()?;
    // 批处理只认一个 exe 路径，所以「要替换的那个文件名」必须现在就定下来：安装目录
    // 可以改名，镜像名跟着走。
    //
    // The batch takes a single exe path, so the file name to replace is fixed here: the
    // install directory may be renamed, and the image name follows it.
    let exe_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| {
            crate::i18n::t!("当前程序没有文件名，无法确定要替换哪一个 exe").to_owned()
        })?;
    // `%` 与 `"` 在 CMD 里无法安全转义：先拒绝，别生成一个必然出错的脚本。
    //
    // `%` and `"` cannot be escaped safely in CMD: reject up front instead of emitting a
    // script that is guaranteed to misbehave.
    update::check_install_dir(&target)?;

    let work = work_dir()?;
    let zip = work.join(asset_file_name(&asset.name)?);

    let mut on_progress = |done: u64, total: u64| {
        // 锁中毒就当没进度：进度条停住比 panic 好，而且这条路径不该有人 panic。
        //
        // A poisoned lock simply means no progress: a frozen bar beats a panic, and
        // nothing on this path should be panicking in the first place.
        if let Ok(mut slot) = progress.lock() {
            *slot = (done, total);
        }
    };
    update::download_to_file(&asset, &zip, &mut on_progress)?;

    let sums_bytes = update::fetch(&sums_url)?;
    let sums_text = String::from_utf8(sums_bytes)
        .map_err(|_| crate::i18n::t!("SHA256SUMS 不是 UTF-8 文本").to_owned())?;
    let bytes = std::fs::read(&zip).map_err(|e| {
        crate::i18n::tr_fmt(
            "读不回下载的压缩包（{}）：{}",
            &[
                &path_text(&zip),
                &aevum_core::errmsg::clip_echo(&e.to_string()),
            ],
        )
    })?;
    update::verify(&bytes, &sums_text, &asset.name)?;

    let unpack = work.join("unpacked");
    let new_exe = update::expand_exe(&zip, &unpack, &exe_name)?;

    let script = work.join("apply-update.cmd");
    std::fs::write(&script, update::install_script(&new_exe, &target)).map_err(|e| {
        crate::i18n::tr_fmt(
            "写不了安装脚本（{}）：{}",
            &[
                &path_text(&script),
                &aevum_core::errmsg::clip_echo(&e.to_string()),
            ],
        )
    })?;

    Ok(PreparedInstall { version, script })
}

impl AppState {
    /// 距上次检查是否已满 24 小时（`true` = 该查了）。
    ///
    /// 系统时钟被调回过去时差值是负的，`saturating_sub` 归零 → 判定为「还没到」，
    /// 于是不会因为改时间而反复发请求。
    ///
    /// Whether 24 hours have passed since the last check (`true` = due).
    ///
    /// If the system clock is set back the difference is negative, `saturating_sub`
    /// clamps it to zero, and the answer is "not yet" -- changing the clock cannot cause a
    /// storm of requests.
    pub fn update_check_due(&self) -> bool {
        match self.last_update_check {
            Some(at) => unix_now().saturating_sub(at) >= AUTO_CHECK_SECS,
            None => true,
        }
    }

    /// 启动时的静默检查：一天最多一次，失败不弹窗、不打断。
    ///
    /// 发现新版本同样不弹窗——只把菜单项文案换成「有新版本 vX.Y.Z…」，用户自己想看
    /// 的时候点进去。
    ///
    /// The silent check at startup: at most once a day, and a failure never pops a dialog
    /// or interrupts anything.
    ///
    /// Finding a new version does not pop a window either -- only the menu item becomes
    /// "new version vX.Y.Z ...", and the user opens it when they want to.
    pub fn startup_update_check(&mut self, ctx: &Context) {
        if !self.update_check_due() {
            return;
        }
        self.spawn_update_check(ctx);
    }

    /// 手动检查：菜单项与窗口里的「检查更新 / 重试」都走这里，**不受 24 小时节流**。
    ///
    /// 节流是给「没人要求过的自动检查」用的；用户明确点了就不能拿缓存敷衍他。
    ///
    /// The manual check: the menu item and the window's "check / retry" both land here, and
    /// it is **not** subject to the 24-hour throttle.
    ///
    /// The throttle exists for an automatic check nobody asked for; an explicit click must
    /// not be answered with a cached result.
    pub fn manual_update_check(&mut self, ctx: &Context) {
        self.update_error = None;
        self.spawn_update_check(ctx);
    }

    /// 有新版本可提示时的版本号（菜单文案用）。
    ///
    /// 判定只认 `UpdateCheck::installable()`：远端更新、但本平台没有对应资产（或者远端
    /// 是预发布）时**不提示**——那种「新版本」点进去也装不了。
    ///
    /// The version to advertise in the menu, when one is available.
    ///
    /// The test is `UpdateCheck::installable()` alone: a newer remote with no asset for
    /// this platform (or a pre-release) is **not** advertised, because such an "update"
    /// cannot be installed anyway.
    pub fn update_available(&self) -> Option<&str> {
        let check = self.update_check.as_ref()?;
        if check.installable() {
            check.latest.as_deref()
        } else {
            None
        }
    }

    /// 起一次检查任务。调用方负责决定「该不该起」（节流 / 阶段机）。
    ///
    /// 阶段机在这里判：已经在跑或已经备好安装时直接返回，于是连点二十下菜单也只会
    /// 发一次请求——这就是防重复点击的那把锁，不需要额外的 `busy` 布尔。
    ///
    /// Launch one check job. The caller decides whether it *should* run (throttle / phase
    /// machine).
    ///
    /// The phase machine is checked here: a job already running, or an install already
    /// prepared, means an immediate return, so twenty menu clicks still send one request --
    /// that is the double-click lock, and no extra `busy` boolean is needed.
    fn spawn_update_check(&mut self, ctx: &Context) {
        if self.update_phase != UpdatePhase::Idle {
            return;
        }
        self.update_job += 1;
        let token = self.update_job;
        self.update_phase = UpdatePhase::Checking;
        self.update_error = None;
        // 重新检查就把上一次备好的安装作废：那个包里装的可能是旧版。
        //
        // Re-checking invalidates a prepared install: that package may hold an older build.
        self.update_ready = None;
        self.status = crate::i18n::t!("正在检查更新…").to_owned();

        let api = update::latest_url();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let out = run_check(&api);
            let _ = tx.send(Msg::UpdateChecked {
                token,
                result: Box::new(out),
            });
            // 后台线程也能请求重绘（`Context` 是 Send+Sync 的句柄），这一下是为了让
            // 菜单文案在结论落地当帧就变，而不是等用户下次动鼠标。
            //
            // A background thread may request a repaint too (the `Context` is a Send +
            // Sync handle); this one makes the menu label change on the very frame the
            // conclusion lands instead of waiting for the next mouse move.
            ctx2.request_repaint();
        });
    }

    /// 开始下载并准备安装（后台线程）。返回 `Err` 只表示「现在起不了这个任务」。
    ///
    /// Start downloading and prepare the install on a background thread. An `Err` only
    /// means "this job cannot be started right now".
    pub fn start_install(&mut self, ctx: &Context) -> Result<(), String> {
        if self.update_phase != UpdatePhase::Idle {
            return Err(crate::i18n::t!("更新任务已经在跑了").to_owned());
        }
        let check = self
            .update_check
            .as_ref()
            .ok_or_else(|| crate::i18n::t!("还没有检查结果：先检查更新").to_owned())?;
        let asset = check
            .asset
            .clone()
            .ok_or_else(|| crate::i18n::t!("这次结果里没有可下载的安装包").to_owned())?;
        let version = check.latest.clone().unwrap_or_default();
        let sums_url = self.update_sums_url.clone().ok_or_else(|| {
            crate::i18n::t!("发布里没有找到 SHA256SUMS 清单，无法校验下载").to_owned()
        })?;

        self.update_job += 1;
        let job = self.update_job;
        self.update_phase = UpdatePhase::Downloading;
        self.update_error = None;
        self.update_ready = None;
        // 进度归零：上一轮的 100% 不该在新一轮的第一帧被当成「快好了」。
        //
        // Reset the progress: last round's 100% must not read as "nearly done" on the
        // first frame of this one.
        if let Ok(mut slot) = self.update_progress.lock() {
            *slot = (0, asset.size);
        }
        self.status = crate::i18n::t!("正在下载更新…").to_owned();

        let progress = self.update_progress.clone();
        let tx = self.tx.clone();
        let ctx2 = ctx.clone();
        thread::spawn(move || {
            let result = run_install(asset, version, sums_url, progress);
            let _ = tx.send(Msg::UpdatePrepared { job, result });
            ctx2.request_repaint();
        });
        Ok(())
    }

    /// 读一次下载进度（界面线程调用）。
    ///
    /// Read the download progress once (called on the UI thread).
    pub fn update_progress_snapshot(&self) -> (u64, u64) {
        self.update_progress
            .lock()
            .map(|slot| *slot)
            .unwrap_or((0, 0))
    }

    /// 拉起已备好的安装脚本，随后请求关闭主窗口。
    ///
    /// 顺序不能反：脚本第一步要等本进程退出，所以必须先让它跑起来再关自己。
    /// 这里只做「拉起 + 请求关闭」两件瞬间完成的事，界面线程无阻塞。
    ///
    /// Launch the prepared install script, then ask the main window to close.
    ///
    /// The order cannot be swapped: the script's first step waits for this process to
    /// exit, so it must be running before we close ourselves. Only "launch" and "request
    /// close" happen here, both instant, so the UI thread never blocks.
    pub fn launch_installer(&mut self, ctx: &Context) -> Result<(), String> {
        let Some((version, script)) = self.update_ready.clone() else {
            return Err(crate::i18n::t!("还没有备好安装包：先下载并校验").to_owned());
        };
        spawn_detached(&script).map_err(|e| {
            crate::i18n::tr_fmt(
                "拉起安装脚本失败（{}）：{}",
                &[
                    &path_text(&script),
                    &aevum_core::errmsg::clip_echo(&e.to_string()),
                ],
            )
        })?;
        self.status = crate::i18n::tr_fmt("正在安装 {}，程序即将退出", &[&version]);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        Ok(())
    }

    /// 程序旁边有没有上一版备份（`<exe>.old`）。
    ///
    /// 判据是**文件存在**，不是「装过更新」：用户可以自己删掉 `.old`，那时菜单里就
    /// 不该出现一条点了会失败的「回滚」。
    ///
    /// Whether a previous build sits next to the executable (`<exe>.old`).
    ///
    /// The test is **file existence**, not "an update once happened": a user may delete the
    /// `.old` file, and the menu must not then offer a rollback that cannot work.
    pub fn has_rollback_backup(&self) -> bool {
        current_exe()
            .map(|target| rollback_backup(&target).is_file())
            .unwrap_or(false)
    }

    /// 回滚到上一版：写脚本 → 拉起 → 请求关闭主窗口。
    ///
    /// 与安装同构（都是「先起脚本，再退出」），差别只在脚本内容：回滚把当前版改名成
    /// `.failed` 留着排查，再把 `.old` 搬回来启动。
    ///
    /// Roll back to the previous build: write the script, launch it, request the main
    /// window to close.
    ///
    /// Structurally identical to installing ("launch the script, then exit"); only the
    /// script differs: a rollback renames the current build to `.failed` for diagnosis and
    /// moves `.old` back into place.
    pub fn launch_rollback(&mut self, ctx: &Context) -> Result<(), String> {
        let target = current_exe()?;
        update::check_install_dir(&target)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let script = std::env::temp_dir().join(format!("aevum-rollback-{stamp}.cmd"));
        std::fs::write(&script, update::rollback_script(&target)).map_err(|e| {
            crate::i18n::tr_fmt(
                "写不了回滚脚本（{}）：{}",
                &[
                    &path_text(&script),
                    &aevum_core::errmsg::clip_echo(&e.to_string()),
                ],
            )
        })?;
        spawn_detached(&script).map_err(|e| {
            crate::i18n::tr_fmt(
                "拉起回滚脚本失败（{}）：{}",
                &[
                    &path_text(&script),
                    &aevum_core::errmsg::clip_echo(&e.to_string()),
                ],
            )
        })?;
        self.status = crate::i18n::t!("正在回滚，程序即将退出").to_owned();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        Ok(())
    }
}
