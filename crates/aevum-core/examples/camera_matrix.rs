//! 打印 `samples/cameras/` 里每个文件的**能力矩阵**：它到底落在哪条路上。
//!
//! # 为什么是一个可执行例子而不是一段文档
//!
//! 「这张样张能解」这句话有三个不同的含义，用户看不出区别，代价却完全不同：
//!
//! * `Sensor`——真的拿到了传感器数据（CFA 直解，16 位），后期有全部余量；
//! * `Preview`——拿到的是机身内嵌 JPEG 预览（真实 8 位），**不是**传感器数据；
//! * 具名拒绝——这一版不做（HEIC），或这台机身/这种压缩还没有解码器。
//!
//! 三者都返回一张「看起来正常」的图，所以只有代码能说清是哪一种。这个例子把矩阵
//! **打出来**，让「填期望表」变成抄一行输出，而不是凭记忆写。
//!
//! # 用法
//!
//! ```text
//! cargo run -p aevum-core --example camera_matrix            # 表格
//! cargo run -p aevum-core --example camera_matrix -- --check # 只报「与期望表不一致」
//! ```
//!
//! `--check` 与 `tests/suite/camera_samples.rs` 的期望表同源：那里是判据，这里是
//! 人看的版本。两边不一致时以判据为准——判据跑在 CI 上，这个例子只跑在人手上。
//!
//! Prints the capability matrix of `samples/cameras/`: which route every file really takes.
//!
//! "This sample decodes" has three meanings the user cannot tell apart, with very different
//! costs: `Sensor` (real sensor data, 16-bit), `Preview` (the camera's embedded JPEG, truly
//! 8-bit), or a named rejection. All three return a picture that looks fine, so only code can
//! say which one happened. This example prints the matrix, so filling in the expectation
//! table is copying a line instead of recalling one.

use std::path::{Path, PathBuf};
use std::time::Instant;

use aevum_core::metrics;
use aevum_core::raw;

/// 样张目录（不入库，可能不存在）。
fn sample_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("cameras")
}

fn main() {
    let check = std::env::args().any(|a| a == "--check");
    let dir = sample_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        println!("没有样张目录：{}（不入库，属正常）", dir.display());
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            !name.starts_with("README")
        })
        .collect();
    files.sort();

    println!(
        "{:<44} {:<8} {:<8} {:<13} {:<13} {:<5} {:<6} camera",
        "file", "info", "read", "info size", "read size", "bits", "ms"
    );
    for f in &files {
        let name = f
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // 行宽有限：长名截断到 42 字符，全名在上面那个 `files` 排序里已经唯一。
        let short: String = if name.chars().count() > 42 {
            name.chars().take(39).collect::<String>() + "..."
        } else {
            name
        };
        // **两条路都要问**：`info` 只读标签/容器（快），`read` 真解像素。两者对同一张
        // 文件报不同的路径，就是「信息面板标注的保真度」与「用户实际拿到的数据」不一致
        // ——那正是 C8 说的「预览与传感器数据长得一样」。
        //
        // Ask **both** routes: `info` reads tags only (fast), `read` decodes pixels. If the
        // two disagree on the same file, the fidelity the info panel shows is not the data
        // the user got -- exactly the ambiguity C8 is about.
        let info = raw::info(f);
        let t = Instant::now();
        let read = raw::read(f);
        let ms = t.elapsed().as_millis();

        let info_route = match &info {
            Ok(i) => match i.path {
                raw::RawPath::Sensor => "Sensor",
                raw::RawPath::Preview => "Preview",
            },
            Err(_) => "Err",
        };
        let info_size = match &info {
            Ok(i) => format!("{}x{}", i.width, i.height),
            Err(_) => "-".to_owned(),
        };
        let camera = match &info {
            Ok(i) => i.camera_line().unwrap_or_else(|| "-".to_owned()),
            Err(_) => "-".to_owned(),
        };

        match &read {
            Ok((img, path)) => {
                let read_route = match path {
                    raw::RawPath::Sensor => "Sensor",
                    raw::RawPath::Preview => "Preview",
                };
                let mismatch = if info_route == read_route {
                    ""
                } else {
                    "  << 不一致"
                };
                println!(
                    "{:<44} {:<8} {:<8} {:<13} {:<13} {:<5} {:<6} {}{}",
                    short,
                    info_route,
                    read_route,
                    info_size,
                    format!("{}x{}", img.width(), img.height()),
                    16,
                    ms,
                    camera,
                    mismatch
                );
            }
            Err(err) => {
                // 不是 RAW（JPG/PNG）、或这一版明确不做（HEIC）、或容器不认识：
                // 统一交给主解码口子再问一次，拿它真正的落点。
                let detail = match metrics::decode_with_origin(f) {
                    Ok((img, origin)) => {
                        format!("Image {}x{} via {origin:?}", img.width(), img.height())
                    }
                    Err(e2) => one_line(&e2.to_string()),
                };
                println!(
                    "{:<44} {:<8} {:<8} {:<13} {:<13} {:<5} {:<6} {:<13} {}",
                    short, info_route, "Err", info_size, "-", "-", ms, camera, detail
                );
                let _ = err;
            }
        }
    }

    if check {
        println!(
            "\n--check 只做提示：判据在 tests/suite/camera_samples.rs，\n\
             那边不一致才红。这里是给人看的版本。"
        );
    }
}

/// 错误串压成一行（去掉换行与重复空白），表格才不破版。
fn one_line(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 90 {
        flat.chars().take(87).collect::<String>() + "..."
    } else {
        flat
    }
}
