//! 单帧导出的落点：跟着面板目录走、同名可跳过、导出中不重入。
//!
//! 这三条是「点导出弹两下」那次修复的行为面。前两条在 `tests/suite/export_path.rs`
//! 只量了纯函数与源码形状；这里用真实的 `AppState` 走一遍，确认**决策**也对：
//! 面板的 `render_out_dir` 真的被读、`render_resume` 真的会挡住重复导出、
//! 导出中再点不会叠第二趟任务。
//!
//! Single-frame export destination: follow the panel directory, skip duplicates, no re-entry.
//!
//! The suite file `tests/suite/export_path.rs` measures the pure function and the source shape;
//! these run a real `AppState` and check the **decision**: `render_out_dir` is really read,
//! `render_resume` really blocks a repeat export, and a click during export does not stack a
//! second job.

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aevum-ui-export-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 默认落点以面板的输出目录为准，文件名带 `_graded`。
///
/// The default destination follows the panel's output directory, with the `_graded` suffix.
#[test]
fn auto_frame_path_follows_the_panel_output_dir() {
    let mut s = AppState::demo();
    // `demo()` 默认选中的是第 12 帧，显式固定，免得测试跟着演示数据漂。
    //
    // `demo()` starts on frame 12; pin it so the test does not drift with the demo data.
    s.selected = 0;
    s.render_out_dir = PathBuf::from("D:/work/成片");
    s.render_format = aevum_core::export::StillFormat::Png8;

    let got = s.auto_frame_path().expect("演示序列应当有落点");
    assert_eq!(
        got,
        PathBuf::from("D:/work/成片").join("demo_0000_graded.png"),
        "落点没跟着面板的输出目录走"
    );
}

/// 面板空着就落到 `<序列根>/成片`，与批量导出同一条规则。
///
/// An empty panel field falls back to `<sequence root>/成片`, the same rule batch export uses.
#[test]
fn auto_frame_path_falls_back_to_the_sequence_folder() {
    let mut s = AppState::demo();
    s.selected = 0;
    s.render_out_dir = PathBuf::new();
    s.render_format = aevum_core::export::StillFormat::Png8;

    let got = s.auto_frame_path().expect("演示序列应当有落点");
    assert_eq!(
        got,
        PathBuf::from("演示序列")
            .join("成片")
            .join("demo_0000_graded.png")
    );
}

/// 开着「跳过同名」且目标已存在 → 不导出，并在状态栏说明原因。
///
/// With "skip existing" on and the target present, do nothing and say why in the status bar.
#[test]
fn auto_frame_path_skips_an_existing_file_when_resume_is_on() {
    let dir = temp_dir("resume");
    let mut s = AppState::demo();
    s.render_out_dir = dir.clone();
    s.render_format = aevum_core::export::StillFormat::Png8;
    s.render_resume = true;

    let dst = s.auto_frame_path().expect("第一次应当有落点");
    std::fs::write(&dst, b"placeholder").expect("放一个占位文件");

    assert!(
        s.auto_frame_path().is_none(),
        "同名文件已存在、开着「跳过同名」时不该再导一遍"
    );
    assert!(
        s.status.contains("已存在"),
        "跳过时状态栏要说清原因，否则用户以为点了没反应：{}",
        s.status
    );

    // 关掉「跳过同名」就该照常覆盖。
    //
    // Turning "skip existing" off lets it overwrite again.
    s.render_resume = false;
    assert!(s.auto_frame_path().is_some(), "关掉跳过之后应当允许覆盖");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 导出进行中再点一次，不许叠第二趟任务。
///
/// Clicking again mid-export must not stack a second job.
#[test]
fn auto_frame_path_refuses_a_second_job() {
    let mut s = AppState::demo();
    s.render_out_dir = PathBuf::from("D:/work/成片");
    s.export_busy = true;
    assert!(
        s.auto_frame_path().is_none(),
        "导出中又点了一次，不能起第二趟"
    );
}
