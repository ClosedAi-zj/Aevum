//! 命令行的对外契约（版本、帮助、不认识的参数），以及 exe 里那份给 Explorer 看的版本资源。
//!
//! # 为什么这里要真的把 exe 跑起来
//!
//! 这三条判断的是**进程行为**——退出码、写到哪个流、有没有起窗口——任何只用
//! `match` 语句就能自证的写法都测不到它们。具体来说，本文件盯的是三件曾经会悄悄
//! 坏掉的事：
//!
//! 1. `--version` 不存在（用户报障时唯一能问的就是「你装的是哪一版」）；
//! 2. `--version` 顺手把窗口起了（无显示器环境里会挂住，脚本里也会卡死）；
//! 3. 不认识的参数被**静默接受**（拼错 `--shot` 会变成「正常启动但没截图」）。
//!
//! 用 `CARGO_BIN_EXE_aevum` 拿被测二进制，所以断言的是**这一版刚要发出去的那个
//! 可执行文件**，不是某条代码路径的替身。
//!
//! # Why this spawns the real exe
//!
//! These judge **process behaviour** -- exit codes, which stream is written, whether a
//! window is created -- none of which a `match` arm can prove by itself. Three things
//! used to break silently here: `--version` did not exist (the first question in any
//! support thread is "which build do you have"); handling it started a window (which
//! hangs headless and deadlocks scripts); and an unrecognised flag was accepted quietly
//! (a typo in `--shot` became "starts fine, writes no screenshot").
//!
//! `CARGO_BIN_EXE_aevum` points at the very executable this release ships, not a stand-in.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 被测二进制。`CARGO_BIN_EXE_<bin 名>` 由 cargo 在编译期注入。
///
/// The binary under test; cargo injects `CARGO_BIN_EXE_<bin>` at compile time.
fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_aevum")
}

/// 跑一次并带上超时。
///
/// 超时不是防抖，是**判据本身**：`--version` 若走了起窗口那条路，进程会一直活着，
/// 无显示器机器上更是直接挂死。没有这条上限，这个测试就变成了「卡住」而不是「失败」。
///
/// Run once with a deadline.
///
/// The deadline is part of the assertion, not a flake guard: if `--version` took the
/// window path the process would stay alive, and on a headless machine it would hang
/// outright -- without a cap this test would stall instead of failing.
fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let mut child = Command::new(exe())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("被测 exe 应当能启动");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait().expect("等待子进程") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("{args:?} 20 秒内没有退出——版本/帮助路径不该进入窗口循环");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let out = child.wait_with_output().expect("收集子进程输出");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn version_prints_one_parseable_line_and_exits_clean() {
    let (code, stdout, stderr) = run(&["--version"]);
    assert_eq!(code, Some(0), "--version 必须是干净退出，stderr={stderr}");
    assert_eq!(
        stdout.lines().count(),
        1,
        "--version 只该打一行：{stdout:?}"
    );
    let line = stdout.trim_end_matches(['\r', '\n']);
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        line,
        format!("{} {}", aevum_core::brand::TITLE, version),
        "版本行必须是「标题 + 版本号」，脚本靠末尾那个 token 取值"
    );
    assert!(
        line.ends_with(version),
        "版本号必须在行尾，中间不许夹别的字：{line:?}"
    );
    assert!(stderr.is_empty(), "成功路径不该写 stderr：{stderr:?}");
}

#[test]
fn the_short_and_long_version_flags_are_the_same_output() {
    let (short_code, short, _) = run(&["-V"]);
    let (long_code, long, _) = run(&["--version"]);
    assert_eq!(short_code, Some(0));
    assert_eq!(long_code, Some(0));
    assert_eq!(short, long, "-V 与 --version 必须逐字节相同");
}

#[test]
fn help_lists_the_version_flag() {
    let (code, stdout, _) = run(&["--help"]);
    assert_eq!(code, Some(0));
    // 帮助里能查到才算「可发现」：只实现不写进帮助，等于没人知道它存在。
    // Discoverable only if it is listed; an undocumented flag is a flag nobody uses.
    assert!(
        stdout.contains("-V, --version"),
        "帮助里没有版本号开关：\n{stdout}"
    );
    assert!(
        stdout.contains(aevum_core::brand::TITLE),
        "帮助首行应是产品标题"
    );
}

#[test]
fn an_unknown_flag_is_rejected_instead_of_ignored() {
    let (code, _, stderr) = run(&["--definitely-not-a-flag"]);
    assert_eq!(
        code,
        Some(2),
        "不认识的参数必须是非零退出，否则拼错的 --shot 会变成静默不截图"
    );
    assert!(
        stderr.contains("--definitely-not-a-flag"),
        "错误信息要点名那个参数，不然用户不知道哪个拼错了：{stderr:?}"
    );
}

#[test]
fn a_typo_of_a_real_flag_is_not_treated_as_a_directory() {
    // `--shot` 拼成 `--shot2` 时，旧行为是当成目录打开、界面正常起来、截图没有。
    // 这条把「否」钉住：任何以 - 开头又没匹配上的串，都必须走拒绝那条路。
    // A typo such as `--shot2` used to be taken as a directory: the UI came up and no
    // screenshot appeared. This pins the "no": anything starting with `-` that matches
    // nothing must take the reject path.
    let (code, _, stderr) = run(&["--shot2", "E:/tmp/should-not-be-opened"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("--shot2"), "{stderr:?}");
}

/// 读 exe 版本资源里的一个字段，也就是资源管理器「详细信息」页显示的那一列。
///
/// 用 PowerShell 而不是解析 PE：`VersionInfo` 由 Windows 自己解资源段，省掉一个
/// 只为测试而生的 PE 解析依赖，也没有「自己解析自己、解析错了还绿」的风险。
/// 输出统一转 UTF-8，否则中文产品名在英文 runner 上会变成乱码。
///
/// Read one field of the exe version resource -- the very column Explorer's "Details"
/// tab shows.
///
/// PowerShell rather than a hand-rolled PE parser: Windows itself decodes the resource
/// section, which avoids a test-only dependency and any chance of a parser that "passes"
/// because it is wrong in the same way as the code under test. Output is forced to UTF-8
/// so a Chinese product name survives an English runner.
fn version_info(field: &str) -> String {
    let script = format!(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
         (Get-Item -LiteralPath '{}').VersionInfo.{field}",
        exe()
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .expect("应当能调起 powershell 读版本资源");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_binary_carries_a_version_resource() {
    // 没有版本资源时，右键属性「详细信息」整页空白——对一份发给别人用的软件来说，
    // 这比没有签名更先被看见。
    // Without a version resource the whole "Details" page is blank, which is what people
    // notice first -- well before they notice the absence of a signature.
    let product = version_info("ProductName");
    assert!(
        !product.is_empty(),
        "exe 没有版本资源：属性页「详细信息」会是空白。检查构建时是否找到了 rc.exe"
    );
    assert!(
        product.contains("Aevum"),
        "产品名应包含品牌名，实际是 {product:?}"
    );
    assert!(
        !version_info("FileDescription").is_empty(),
        "文件说明不能为空"
    );

    let file_version = version_info("FileVersion");
    let pkg = env!("CARGO_PKG_VERSION");
    assert!(
        file_version.starts_with(pkg),
        "文件版本 {file_version:?} 应以包版本 {pkg} 开头——提了版本号却没重编资源段就会这样"
    );
}

#[test]
fn the_resource_product_name_matches_the_brand_constant() {
    // build 脚本先于依赖库编译，拿不到 brand::TITLE，只能在 build.rs 里再写一份。
    // 这份判据就是那两处之间的绳子：谁改了名字而没改另一个，这里立刻红。
    // The build script runs before dependency crates compile and cannot read
    // brand::TITLE, so build.rs keeps a second copy; this test is the rope between them.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs"))
        .expect("应当能读到 build.rs");
    let line = src
        .lines()
        .find(|line| line.starts_with("const PRODUCT_NAME_CN: &str = "))
        .expect("build.rs 里应当有 PRODUCT_NAME_CN 常量");
    let value = line.split('"').nth(1).expect("常量应当带一个字符串字面量");
    assert_eq!(
        value,
        aevum_core::brand::TITLE,
        "build.rs 的产品名与 brand::TITLE 漂移了：窗口标题与属性页会各显示一个"
    );
}
