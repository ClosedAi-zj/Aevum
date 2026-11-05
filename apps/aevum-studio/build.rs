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

/// 版本资源里的产品名与厂商名。
///
/// build 脚本先于依赖库编译，拿不到 `aevum_core::brand::TITLE`，只能在这里各写一份。
/// 为了不让两份漂移，`tests/cli.rs` 有一条判据把两处逐字比对。
///
/// Product and vendor names for the version resource.
///
/// The build script runs before dependency crates are compiled, so it cannot reach
/// `aevum_core::brand::TITLE` and has to spell the name out here. A test in
/// `tests/cli.rs` compares the two copies verbatim so they cannot drift.
const PRODUCT_NAME_CN: &str = "恒晷 · Aevum Studio";
const PRODUCT_NAME_EN: &str = "Aevum Studio";
const COMPANY: &str = "Aevum Studio contributors";

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

/// 把 `1.9.4` 这样的版本号拆成 rc 要求的四段数字。
///
/// `FILEVERSION` 只吃数字，`1.9.4-beta.1` 这种就得先把每段的尾部切掉；切不掉的部分
/// 按 0 算。属性页「文件版本」显示的正是这四个数，带后缀的完整串另由字符串表给出。
///
/// Split a version such as `1.9.4` into the four numbers rc demands.
///
/// `FILEVERSION` accepts digits only, so `1.9.4-beta.1` has each part truncated at the
/// first non-digit; whatever cannot be parsed becomes 0. These four numbers are what
/// Explorer shows as "file version", while the string table keeps the full text.
fn version_quad(version: &str) -> [u32; 4] {
    let mut quad = [0u32; 4];
    for (slot, part) in quad.iter_mut().zip(version.split('.')) {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        *slot = digits.parse().unwrap_or(0);
    }
    quad
}

/// 生成资源脚本正文：一个可选的 ICON 行，加上一块 `VERSIONINFO`。
///
/// 里面有两个 `StringFileInfo` 块——`040904B0`（en-US）与 `080404B0`（zh-CN），都声明
/// 代码页 1200（Unicode）。Explorer 会挑与界面语言匹配的那块，挑不到再退回第一块，
/// 于是中英文系统上看到的都不是空白。
///
/// 刻意不 `#include` 任何头文件：`VERSIONINFO` 是 rc 的保留字，`FILEOS`/`FILETYPE`
/// 的取值这里直接写数值。原代码不 include 是为了换台机器不用配 SDK 头路径，这一条
/// 现在依然成立。
///
/// Build the resource script body: an optional ICON line plus one `VERSIONINFO` block.
///
/// Two `StringFileInfo` blocks ship -- `040904B0` (en-US) and `080404B0` (zh-CN), both
/// declaring code page 1200 (Unicode). Explorer picks the block matching the UI language
/// and falls back to the first, so neither locale shows a blank sheet.
///
/// Nothing is `#include`d: `VERSIONINFO` is built into rc and the `FILEOS`/`FILETYPE`
/// values are spelled as numbers, which keeps the build independent of SDK headers.
fn rc_source(icon: bool, version: &str) -> String {
    let [a, b, c, d] = version_quad(version);
    let mut text = String::new();
    if icon {
        text.push_str("1 ICON \"app.ico\"\n\n");
    }
    let body = r#"1 VERSIONINFO
FILEVERSION __QUAD__
PRODUCTVERSION __QUAD__
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "__COMPANY__"
      VALUE "FileDescription", "__EN__"
      VALUE "FileVersion", "__VER__"
      VALUE "InternalName", "aevum"
      VALUE "LegalCopyright", "Copyright (c) 2026 Aevum Studio contributors. MIT License."
      VALUE "OriginalFilename", "aevum.exe"
      VALUE "ProductName", "__EN__"
      VALUE "ProductVersion", "__VER__"
    END
    BLOCK "080404B0"
    BEGIN
      VALUE "CompanyName", "__COMPANY__"
      VALUE "FileDescription", "__CN__"
      VALUE "FileVersion", "__VER__"
      VALUE "InternalName", "aevum"
      VALUE "LegalCopyright", "Copyright (c) 2026 Aevum Studio contributors. MIT License."
      VALUE "OriginalFilename", "aevum.exe"
      VALUE "ProductName", "__CN__"
      VALUE "ProductVersion", "__VER__"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200, 0x804, 1200
  END
END
"#;
    text.push_str(
        &body
            .replace("__QUAD__", &format!("{a},{b},{c},{d}"))
            .replace("__COMPANY__", COMPANY)
            .replace("__EN__", PRODUCT_NAME_EN)
            .replace("__CN__", PRODUCT_NAME_CN)
            .replace("__VER__", version),
    );
    text
}

/// 用 UTF-16LE（带 BOM）写资源脚本。
///
/// 这条不是洁癖：rc.exe 对不带 BOM 的文件按**系统代码页**解码，中文产品名在英文
/// Windows 上会变成问号或乱码，而 CI 的 runner 默认就是英文。UTF-16LE 由 BOM 明示，
/// 与宿主机语言无关——和更新脚本那次踩的代码页坑是同一类问题。
///
/// Write the resource script as UTF-16LE with a BOM.
///
/// Not fussiness: without a BOM, rc.exe decodes the file in the **system code page**, so
/// a Chinese product name turns into question marks on English Windows -- and CI runners
/// default to English. The BOM makes UTF-16LE explicit and host-language independent,
/// the same class of bug as the code-page trap in the update script.
fn write_rc_utf16(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(path, bytes)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("../../assets/app.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    // 版本号来自 Cargo.toml；不盯住它，提版后资源段会停在旧号上。
    // The version comes from Cargo.toml; without this the resource keeps the old number.
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("Cargo.toml").display()
    );
    let Some(rc) = rc_exe() else {
        println!("cargo:warning=没找到 rc.exe（Windows SDK），exe 不带图标与版本信息");
        return;
    };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    // 图标是可选件：缺了只是文件看着简陋，不该连带把版本信息一起丢掉。
    // The icon is optional: without it the file looks plainer, which is no reason to
    // drop the version block along with it.
    let icon = ico.is_file() && fs::copy(&ico, out.join("app.ico")).is_ok();
    if !icon {
        println!("cargo:warning=没有可用的 assets/app.ico，exe 只带版本信息、不带文件图标");
    }

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    if write_rc_utf16(&out.join("app.rc"), &rc_source(icon, &version)).is_err() {
        println!("cargo:warning=写 app.rc 失败，exe 不带图标与版本信息");
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
