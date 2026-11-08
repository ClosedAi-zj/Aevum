//! 单帧导出的落点与对话框纪律。
//!
//! # 这一对判据挡的是什么
//!
//! 用户 2026-11-08 报告「点导出弹两下文件框」。查下去是两个叠加的问题：
//!
//! 1. `export_current` 直接开 `rfd::FileDialog::save_file()`，不设起始目录；
//! 2. 它的落点**不读** `render_out_dir` —— 用户在导出对话框里刚选过的目录被丢掉，
//!    于是必须再选一次文件名，一次手势里出现两个系统对话框。
//!
//! 修法是把"目录即输出"作为默认：路径由纯函数 [`auto_frame_dst`] 算出来，不弹框；
//! 想要别处走 `export_current_as`，那是唯一保留的保存对话框。
//!
//! 两条判据分别钉住这两半：一条量路径规则，一条量"生产代码里只剩一个保存对话框"。
//! 缺任何一条，另一半就会悄悄长回来——这正是第一次修这个问题时发生的事。
//!
//! What this pair of fences guards against.
//!
//! On 2026-11-08 the user reported "clicking export pops two file dialogs". Two problems
//! stacked: `export_current` opened a save dialog with no starting directory, and its
//! destination ignored `render_out_dir`, so the folder the user had just picked in the export
//! dialog was thrown away and a second picker appeared. The fix makes "the directory is the
//! output" the default, computed by the pure function [`auto_frame_dst`]; anything else goes
//! through `export_current_as`, the one remaining save dialog. One fence measures the path
//! rule, the other measures "only one save dialog left in production code".

use std::path::{Path, PathBuf};

/// 读源码并去掉整行注释：注释里引用的 `.save_file(` 不该被算成调用点。
///
/// Read a source file with whole-line comments removed, so a `.save_file(` quoted in a comment
/// is not counted as a call site.
fn clean(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let raw =
        std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("读不到 {rel}：{e}"));
    raw.lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 默认落点必须落在面板的输出目录里，且带 `_graded` 后缀。
///
/// 这是「目录即输出」的另一半：只要有人把落点改回"对话框返回值"或者改到别处，
/// 这条测试就会红。
///
/// The default destination must land inside the panel's output directory and carry the
/// `_graded` suffix. The other half of "the directory is the output": if someone points it
/// back at a dialog return value or anywhere else, this goes red.
#[test]
fn auto_frame_path_lands_in_render_out_dir() {
    let fmt = aevum_core::export::StillFormat::Png8;

    // 面板设了输出目录：照着它走。
    //
    // The panel has an output directory: use it.
    let got = aevum_ui::state::auto_frame_dst(
        &PathBuf::from("D:/work/成片"),
        &PathBuf::from("D:/work/黄山"),
        "PANA0001",
        fmt,
    );
    assert_eq!(
        got,
        PathBuf::from("D:/work/成片/PANA0001_graded.png"),
        "默认落点没跟着面板的输出目录走"
    );

    // 面板没设：落到 `<序列根>/成片`，与批量导出的 `out_root` 同一条规则。
    //
    // Empty on the panel: `<sequence root>/成片`, the same rule batch export uses.
    let got = aevum_ui::state::auto_frame_dst(
        &PathBuf::new(),
        &PathBuf::from("D:/work/黄山"),
        "PANA0001",
        fmt,
    );
    assert_eq!(
        got,
        PathBuf::from("D:/work/黄山/成片/PANA0001_graded.png"),
        "面板空着时应当落到 <序列根>/成片"
    );

    // 换容器时后缀跟着换，不出现 `.jpg` 里装着 PNG 字节。
    //
    // A different container swaps the suffix, so a `.jpg` never holds PNG bytes.
    let got = aevum_ui::state::auto_frame_dst(
        &PathBuf::from("D:/work/成片"),
        &PathBuf::from("D:/work/黄山"),
        "PANA0001",
        aevum_core::export::StillFormat::Jpeg,
    );
    assert_eq!(got.extension().and_then(|e| e.to_str()), Some("jpg"));
}

/// 生产代码里**只能有一个**保存对话框，且它属于「另存为」。
///
/// `export_current` 那条默认路径一旦再开对话框，用户就会重新掉进"弹两下"的坑里。
/// 判据只看一处文件：单帧导出的实现文件。
///
/// Production code may contain **exactly one** save dialog, and it belongs to "save as".
/// If the default path opens one again, the two-dialog trap is back. The fence only looks at
/// the one file that implements single-frame export.
#[test]
fn single_frame_export_never_prompts_for_a_path() {
    let src = clean("state/ops_export_write.rs");

    let calls = src.matches(".save_file()").count();
    assert_eq!(
        calls, 1,
        "单帧导出里有 {calls} 处 save_file：默认路径不许弹保存框，只允许「另存为」一处"
    );

    // 那一处必须落在 pick_frame_path 里，也就是「另存为」的路径。
    //
    // That one call must sit inside `pick_frame_path` -- the save-as path.
    let pick = src
        .find("fn pick_frame_path")
        .expect("找不到 pick_frame_path：另存为的入口被删了？");
    let call = src.find(".save_file()").expect("上面刚数到 1 处");
    assert!(
        call > pick,
        "保存对话框不在 pick_frame_path 里——默认导出路径又开始弹框了"
    );

    // 起始目录必须设置：否则对话框每次从系统默认位置打开，用户得重新找一遍。
    //
    // The starting directory must be set, or every dialog opens somewhere the user has to
    // navigate away from.
    assert!(src.contains(".set_directory("), "另存为没有设置起始目录");
}
