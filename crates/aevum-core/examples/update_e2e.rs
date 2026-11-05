//! 更新链的真机联调探针：对着一个**本地假发布**把 下载 → 校验 → 解包 → 替换 → 回滚 跑一遍。
//!
//! 为什么需要它：`aevum_core::update` 的判据全都不联网、不落真文件，因此它们证明不了
//! 「批处理真的能在这台机器上把这棵树换掉」。而这个链路上出过的每一个真故障（UTF-8 中文
//! 路径被 OEM 代码页吞掉、PATH 上的 Unix `find` 让等待循环失效）都只在真跑时才现形。
//!
//! 用法见 `docs/分发与更新.md`「真机联调」。四个模式：
//!
//! | 模式 | 做什么 |
//! |---|---|
//! | `install`（默认） | 走完整链路并拉起安装脚本 |
//! | `prepare` | 只准备（下载/校验/解包/写脚本），不拉起，供人工观察 |
//! | `rollback` | 只写并拉起回滚脚本（验证「回到上一版」） |
//! | `locked` | 用 `share_mode(0)` 占住新 exe，验证「十五次重试后回滚」 |
//!
//! 环境变量：`E2E_API`（假发布地址）、`E2E_TARGET`（被替换的 exe）、`E2E_WORK`（工作目录）、
//! `E2E_CURRENT`（当前版本，默认 1.9.2）、`E2E_MODE`。
//!
//! # 与 `ops_update::run_install` 的关系
//!
//! 调用顺序逐行对齐（检查 → 下载 → 取清单 → 校验 → 解包 → 写脚本 → 拉起），但**不共享
//! 代码**：那个函数在 `aevum-ui` 里且要 `AppState`，本程序是 `aevum-core` 的 example。改
//! 那边的时候要跟着看一眼这里，否则联调跑的是旧顺序。
//!
//! End-to-end probe for the update chain: run 下载 → 校验 → 解包 → 替换 → 回滚 against a
//! **local fake release**.
//!
//! Why this exists: every `aevum_core::update` judgement is offline and never touches a real
//! file, so none of them can prove "the batch really swaps this tree on this machine". And
//! every real failure this chain has had (a UTF-8 Chinese path swallowed by the OEM code page,
//! a Unix `find` on PATH defeating the wait loop) only appeared when it was really run.
//!
//! See `docs/分发与更新.md` for the walkthrough. Four modes: `install` (default, full chain),
//! `prepare` (prepare only), `rollback` (write and launch the rollback script), `locked`
//! (hold the new exe with `share_mode(0)` and watch the fifteen retries end in a rollback).
//!
//! Environment: `E2E_API`, `E2E_TARGET`, `E2E_WORK`, `E2E_CURRENT`, `E2E_MODE`.
//!
//! # Relationship to `ops_update::run_install`
//!
//! The call order is line-for-line the same, but the code is **not shared**: that function
//! lives in `aevum-ui`, needs an `AppState`, and this is a `aevum-core` example. Whoever
//! changes one should glance at the other, or the probe will keep rehearsing an old order.

use std::path::PathBuf;
use std::process::Command;

use aevum_core::update;

/// 读一个路径环境变量，把 Git Bash 传来的正斜杠掰回反斜杠。
///
/// 只在这一层掰：生产代码里的 `current_exe()` 与 `env::temp_dir()` 给的都是反斜杠，不掰
/// 回来等于在测「Git Bash 的写法能不能跑」，而不是产品路径能不能跑。
///
/// Read a path environment variable, folding Git Bash's forward slashes back.
///
/// Only this layer folds: production always gets backslashes from `current_exe()` and
/// `env::temp_dir()`, and not folding would measure "does Git Bash's spelling work" instead of
/// "does the product's path work".
fn env_path(key: &str) -> PathBuf {
    let raw = std::env::var(key).unwrap_or_else(|_| panic!("缺少环境变量 {key}"));
    PathBuf::from(raw.replace('/', "\\"))
}

/// 起一个分离的批处理：`cmd /c start "" <script> <args...>`，与 `ops_update::spawn_detached`
/// 同形。
///
/// Launch a detached batch exactly like `ops_update::spawn_detached` does.
///
/// 不等子进程是有意的：脚本的第一步就是「等本进程退出」，这里等它就是互相等死锁。
/// Not waiting is deliberate: the script's first step is "wait for this process to exit",
/// so waiting here would deadlock the two.
#[allow(clippy::zombie_processes)]
fn spawn_detached(script: &std::path::Path, args: &[&std::ffi::OsStr], dir: &std::path::Path) {
    let mut cmd = Command::new("cmd");
    cmd.arg("/c")
        .arg("start")
        .arg("")
        .arg(script.as_os_str())
        .current_dir(dir);
    for a in args {
        cmd.arg(a);
    }
    let child = cmd.spawn().expect("拉起批处理");
    println!("[spawn] pid={} script={}", child.id(), script.display());
}

fn main() {
    let api = std::env::var("E2E_API").expect("E2E_API");
    let target = env_path("E2E_TARGET");
    let work = env_path("E2E_WORK");
    let current = std::env::var("E2E_CURRENT").unwrap_or_else(|_| "1.9.2".to_string());
    let mode = std::env::var("E2E_MODE").unwrap_or_else(|_| "install".to_string());

    let image = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .expect("目标没有文件名");
    std::fs::create_dir_all(&work).expect("建工作目录");

    // 回滚模式不联网：只写脚本、只拉起。
    //
    // The rollback mode needs no network: write the script, launch it.
    if mode == "rollback" {
        let script = work.join("rollback.cmd");
        std::fs::write(&script, update::rollback_script()).expect("写回滚脚本");
        spawn_detached(
            &script,
            &[target.as_os_str(), std::ffi::OsStr::new(&image)],
            &work,
        );
        return;
    }

    // 1. 检查：fetch → parse_release → check_release，与 `run_check` 同序同函数
    let json =
        String::from_utf8(update::fetch(&api).expect("取发布 JSON")).expect("JSON 不是 UTF-8");
    let release = update::parse_release(&json).expect("解析发布");
    let check = update::check_release(&current, &json, update::ASSET_SUFFIX);
    println!(
        "[check] installable={} latest={:?} note={}",
        check.installable(),
        check.latest,
        check.note
    );
    assert!(check.installable(), "假发布应当被判为可安装");
    let asset = check.asset.clone().expect("资产");
    let sums = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(update::SUMS_ASSET))
        .expect("清单");
    println!(
        "[check] asset={} size={} sums={}",
        asset.name, asset.size, sums.url
    );

    // 2. 下载（落盘 + 真实字节进度）
    let zip = work.join("pkg.zip");
    let mut marks: Vec<(u64, u64)> = Vec::new();
    update::download_to_file(&asset, &zip, &mut |done, total| {
        marks.push((done, total));
    })
    .expect("下载");
    let on_disk = std::fs::metadata(&zip).expect("读包大小").len();
    println!(
        "[download] callbacks={} first={:?} last={:?} bytes={} declared={}",
        marks.len(),
        marks.first(),
        marks.last(),
        on_disk,
        asset.size
    );
    assert_eq!(on_disk, asset.size, "落盘字节必须等于发布声明的大小");
    assert!(marks.len() >= 2, "进度至少回调两次（0 与完成）");

    // 3. 清单 + 校验
    let sums_bytes = update::fetch(&sums.url).expect("取清单");
    let sums_text = String::from_utf8(sums_bytes).expect("清单不是 UTF-8");
    let bytes = std::fs::read(&zip).expect("读回压缩包");
    match update::verify(&bytes, &sums_text, &asset.name) {
        Ok(()) => println!("[verify] ok"),
        Err(e) => println!("[verify] FAILED: {e}"),
    }

    // 4. 解包：包里同时有 aevum.exe 与 tools/ffmpeg.exe，必须挑到同名那个
    let unpack = work.join("unpacked");
    let new_exe = update::expand_exe(&zip, &unpack, &image).expect("解包");
    println!("[expand] picked={}", new_exe.display());
    assert!(
        new_exe
            .file_name()
            .map(|n| n.to_string_lossy().eq_ignore_ascii_case(&image))
            .unwrap_or(false),
        "解包必须挑同名 exe，而不是 tools/ffmpeg.exe"
    );

    // 5. 两份脚本（正文纯 ASCII，路径当参数传）
    update::check_install_dir(&target).expect("安装目录可交给批处理");
    let script = work.join("apply-update.cmd");
    std::fs::write(&script, update::install_script()).expect("写安装脚本");
    let rollback = work.join("rollback.cmd");
    std::fs::write(&rollback, update::rollback_script()).expect("写回滚脚本");
    println!("[script] apply={}", script.display());
    println!("[script] rollback={}", rollback.display());
    println!("[script] backup={}.old", target.display());

    if mode == "prepare" {
        println!("[spawn] 跳过（E2E_MODE=prepare）");
        return;
    }

    if mode == "locked" {
        // 杀软/索引器的替身：share_mode(0) 打开新 exe，`move` 必然失败，脚本应在十五次
        // 重试后把 `.old` 搬回去。
        //
        // Stand-in for an antivirus: open the new exe with share_mode(0) so `move` must fail;
        // the script should restore `.old` after fifteen retries.
        use std::os::windows::fs::OpenOptionsExt;
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&new_exe)
            .expect("锁住新 exe");
        spawn_detached(
            &script,
            &[
                new_exe.as_os_str(),
                target.as_os_str(),
                std::ffi::OsStr::new(&image),
            ],
            &work,
        );
        println!("[locked] 锁住 {}", new_exe.display());
        std::thread::sleep(std::time::Duration::from_secs(20));
        drop(held);
        println!("[locked] 20s 后：脚本还在={}", script.exists());
        println!("[locked] 新 exe 还在={}", new_exe.exists());
        println!(
            "[locked] 备份还在={}",
            PathBuf::from(format!("{}.old", target.display())).exists()
        );
        return;
    }

    // 6. 拉起安装脚本
    spawn_detached(
        &script,
        &[
            new_exe.as_os_str(),
            target.as_os_str(),
            std::ffi::OsStr::new(&image),
        ],
        &work,
    );
}
