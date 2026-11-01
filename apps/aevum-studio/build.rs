//! 把 `assets/app.ico` 编进 exe 的资源段。
//!
//! 少了这一步，Explorer、任务栏快捷方式和「发送到桌面」上看到的就是默认白纸：
//! 窗口图标是运行时给的，文件图标只能来自 PE 资源段。
//!
//! `rc.exe` 只有装了 Windows SDK 的机器上才有，找不到就跳过并留一条 warning——
//! CI 上照样绿，只是那份包不带文件图标。
//!
//! Embed `assets/app.ico` into the executable's resource section.
//!
//! Without this step, Explorer, taskbar shortcuts and "send to desktop" all show the
//! default blank page: the window icon is supplied at runtime, but the file icon can only
//! come from the PE resource section.
//!
//! `rc.exe` exists only on machines with the Windows SDK; when it cannot be found the step
//! is skipped with a warning -- CI still goes green, the package just ships without a file
//! icon.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SDK_BIN: &str = r"C:\Program Files (x86)\Windows Kits\10\bin";

/// 依次试 `RC` 环境变量、PATH、Windows SDK 的 bin\<版本>\x64。
fn rc_exe() -> Option<PathBuf> {
    if let Some(from_env) = env::var_os("RC") {
        let path = PathBuf::from(from_env);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(on_path) = env::var_os("PATH").and_then(|dirs| {
        env::split_paths(&dirs)
            .map(|dir| dir.join("rc.exe"))
            .find(|cand| cand.is_file())
    }) {
        return Some(on_path);
    }
    let mut versions = fs::read_dir(SDK_BIN)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("x64").join("rc.exe"))
        .filter(|cand| cand.is_file())
        .collect::<Vec<_>>();
    // 目录名是 10.0.26100.0 这种四段版本号，按数字比才不会挑中旧版。
    versions.sort_by_key(|path| {
        path.parent()
            .and_then(|x64| x64.parent())
            .and_then(|version| version.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    });
    versions.pop()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("../../assets/app.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    if !ico.is_file() {
        println!("cargo:warning=没有 assets/app.ico，exe 不带文件图标");
        return;
    }
    let Some(rc) = rc_exe() else {
        println!("cargo:warning=没找到 rc.exe（Windows SDK），exe 不带文件图标");
        return;
    };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    if fs::copy(&ico, out.join("app.ico")).is_err() {
        println!("cargo:warning=复制 app.ico 失败，exe 不带文件图标");
        return;
    }
    // 资源脚本里只写 ICON，不 include Windows.h：那样 rc 需要 SDK 头文件路径，
    // 换机器就容易翻车。一个图标不需要任何符号定义。
    if fs::write(out.join("app.rc"), "1 ICON \"app.ico\"\n").is_err() {
        println!("cargo:warning=写 app.rc 失败，exe 不带文件图标");
        return;
    }
    let built = Command::new(&rc)
        .args(["/nologo", "/fo", "app.res", "app.rc"])
        .current_dir(&out)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    let res = out.join("app.res");
    if !built || !res.is_file() {
        println!("cargo:warning=rc.exe 没编出 app.res，exe 不带文件图标");
        return;
    }
    println!("cargo:rustc-link-arg-bins={}", res.display());
}
