// 双击 exe 不该在界面后面挂一个黑控制台，所以 Windows 下按 GUI 子系统编译。
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

use aevum_ui::state::Module;
use aevum_ui::{AevumApp, Startup};
use eframe::egui;

/// GUI 子系统下从资源管理器启动时，标准流可能是无效句柄，`println!` 会因写失败
/// 直接 panic——自检成功路上不该有这种可能。写失败一律吞掉，退出码才是真信号。
fn say(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{msg}");
}

fn warn(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{msg}");
}

macro_rules! say {
    () => {
        say("")
    };
    ($($t:tt)*) => { say(&format!($($t)*)) };
}

macro_rules! warn {
    () => {
        warn("")
    };
    ($($t:tt)*) => { warn(&format!($($t)*)) };
}

fn main() -> ExitCode {
    // 冷启动计时从**进程进来第一行**开始：argv 解析、配置读取、字体装配、窗口创建、
    // 首帧布局都算在内。放到 eframe 起窗之后就只能量到「窗口起来以后」，
    // 而那正是用户最不在意的一段。
    //
    // Cold-start clock starts on the very first line of `main`: argv parsing, settings,
    // font assembly, window creation and the first frame are all inside it.
    let boot_clock = std::time::Instant::now();
    // --mcp 必须在任何 eframe/窗口初始化之前处理：agent 调用时不能弹窗，
    // 也不能因为没有显示器就失败。
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if argv
        .iter()
        .any(|arg| arg.to_string_lossy().eq_ignore_ascii_case("--mcp"))
    {
        // --mcp 单独运行时仍是无头 stdio server；带 --attach 时改成回环代理，
        // 把工具调用转给正在运行的 GUI，界面里能实时看到变化。
        return match aevum_mcp::attach_path(&argv) {
            Ok(Some(path)) => aevum_mcp::run_stdio_attach(path),
            Ok(None) => aevum_mcp::run_stdio(),
            Err(e) => {
                warn!("{e}");
                ExitCode::from(2)
            }
        };
    }

    let mut open: Option<PathBuf> = None;
    let mut demo = false;
    let mut shot: Option<PathBuf> = None;
    let mut module: Option<Module> = None;
    let mut selftest = false;
    let mut bench_ui = false;
    let mut bench_preview = false;
    let mut bench_startup = false;
    let mut demo_edit = false;
    let mut shot_queue = false;
    let mut shot_end = false;
    let mut shot_group: Option<String> = None;
    let mut shot_channels = false;
    let mut shot_dock: Option<String> = None;
    let mut shot_crop = false;
    let mut shot_rotate = false;
    let mut shot_output = false;
    let mut shot_watermark = false;
    let mut shot_retime = false;
    let mut shot_motion = false;
    let mut shot_qa = false;
    let mut shot_brush = false;
    let mut shot_library = false;
    let mut shot_collection = false;
    let mut shot_libmeta = false;
    let mut shot_libstack = false;
    let mut shot_plugin: Option<String> = None;
    let mut shot_market = false;

    let mut args = argv.into_iter();
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy().to_string();
        match text.as_str() {
            "--selftest" => selftest = true,
            "--bench-ui" => bench_ui = true,
            "--bench-preview" => bench_preview = true,
            "--bench-startup" => bench_startup = true,
            "--demo" => demo = true,
            "--demo-edit" => demo_edit = true,
            "--shot-queue" => shot_queue = true,
            "--shot-plugin" => shot_plugin = args.next().and_then(|v| v.into_string().ok()),
            "--shot-market" => shot_market = true,
            "--shot-end" => shot_end = true,
            "--shot-channels" => shot_channels = true,
            "--shot-crop" => shot_crop = true,
            "--shot-rotate" => shot_rotate = true,
            "--shot-output" => shot_output = true,
            "--shot-watermark" => shot_watermark = true,
            "--shot-retime" => shot_retime = true,
            "--shot-motion" => shot_motion = true,
            "--shot-qa" => shot_qa = true,
            "--shot-brush" => shot_brush = true,
            "--shot-library" => shot_library = true,
            "--shot-collection" => shot_collection = true,
            "--shot-libmeta" => shot_libmeta = true,
            "--shot-libstack" => shot_libstack = true,
            "--shot-dock" => match args.next() {
                Some(name) => shot_dock = Some(name.to_string_lossy().to_string()),
                None => {
                    warn!("--shot-dock 需要一个面板名");
                    return ExitCode::from(2);
                }
            },
            "--shot-group" => match args.next() {
                Some(key) => shot_group = Some(key.to_string_lossy().to_string()),
                None => {
                    warn!("--shot-group 需要一个分组名");
                    return ExitCode::from(2);
                }
            },
            "--shot" => match args.next() {
                Some(path) => shot = Some(PathBuf::from(path)),
                None => {
                    warn!("--shot 需要一个输出路径");
                    return ExitCode::from(2);
                }
            },
            "--open" => match args.next() {
                Some(path) => open = Some(PathBuf::from(path)),
                None => {
                    warn!("--open 需要一个目录");
                    return ExitCode::from(2);
                }
            },
            "--module" => match args.next().map(|v| parse_module(&v.to_string_lossy())) {
                Some(Some(m)) => module = Some(m),
                Some(None) => {
                    warn!("--module 只认 序列/去闪/显影/构图/成片/声音 或 0-5");
                    return ExitCode::from(2);
                }
                None => {
                    warn!("--module 需要一个模块名");
                    return ExitCode::from(2);
                }
            },
            "-h" | "--help" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            other if !other.starts_with('-') => open = Some(PathBuf::from(arg)),
            other => {
                warn!("未知参数：{other}");
                print_help();
                return ExitCode::from(2);
            }
        }
    }

    if bench_ui {
        return match aevum_ui::bench_ui() {
            Ok(msg) => {
                say!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                warn!("bench 失败：{e}");
                ExitCode::FAILURE
            }
        };
    }

    if bench_preview {
        return match aevum_ui::preview_first_screen() {
            Ok(msg) => {
                say!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                warn!("首屏预览基准失败：{e}");
                ExitCode::FAILURE
            }
        };
    }

    if selftest {
        return match aevum_ui::selftest() {
            Ok(msg) => {
                say!("selftest ok：{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                warn!("selftest 失败：{e}");
                ExitCode::FAILURE
            }
        };
    }

    let startup = match (shot, open, demo) {
        (Some(out), open, _) => Startup::Shot {
            out,
            open,
            module,
            edit: demo_edit,
            queue: shot_queue,
            end: shot_end,
            group: shot_group,
            channels: shot_channels,
            dock: shot_dock,
            crop: shot_crop,
            rotate: shot_rotate,
            output: shot_output,
            watermark: shot_watermark,
            retime: shot_retime,
            motion: shot_motion,
            qa: shot_qa,
            brush: shot_brush,
            library: shot_library,
            collection: shot_collection,
            libmeta: shot_libmeta,
            libstack: shot_libstack,
            plugin: shot_plugin,
            market: shot_market,
        },
        (None, Some(dir), _) => Startup::Open(dir),
        (None, None, true) => Startup::Demo,
        (None, None, false) => Startup::Empty,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 1000.0])
            .with_min_inner_size([1180.0, 760.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_icon(window_icon()),
        ..Default::default()
    };

    // 只在 `--bench-startup` 时把首帧和「进程进来」之间的时长钉住；平时这条路上
    // 一个多出来的 Instant 都不留。
    //
    // Only under `--bench-startup` is the first frame pinned to the process entry time;
    // the normal path carries no extra state.
    let boot_probe = bench_startup.then_some(boot_clock);

    match eframe::run_native(
        aevum_core::brand::TITLE,
        options,
        Box::new(move |cc| {
            let app = AevumApp::new(cc, startup);
            Ok(match boot_probe {
                Some(started) => Box::new(StartupProbe::new(app, started)) as Box<dyn eframe::App>,
                None => Box::new(app) as Box<dyn eframe::App>,
            })
        }),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            warn!("启动失败：{e}");
            ExitCode::FAILURE
        }
    }
}

/// 冷启动探针：把 [`AevumApp`] 包一层，首帧画完就打一行、然后关窗退出。
///
/// # 口径
///
/// 「启动到首帧」= `main()` 第一行到**第一次 `ui()` 返回**的墙钟，含 argv 解析、
/// `settings.json` 读取、字体装配、窗口创建、首帧布局与绘制提交。**不含 GPU 呈现**
/// （egui 把形状交了就算，真正上屏在这一帧之后）——我们卡的是 CPU 侧的启动成本，
/// 交叉编译机/CI 上显卡差异不该进这个数。
///
/// 为什么不用外面「等窗口出现」：那要轮询 `MainWindowHandle`，Windows 的窗口
/// 动画、DWM 合成会把噪声推到 200ms 以上，量不出代码退化；要那个数请用
/// `tools/ux_probe.ps1`（它是给「人感受到的可交互时刻」用的，不是给回归用的）。
///
/// # Timing basis
///
/// "Startup to first frame" is the wall clock from the first line of `main()` to the
/// return of the first `ui()`: argv parsing, settings load, font assembly, window
/// creation, first-frame layout and draw submission. It **excludes GPU presentation**;
/// this is the CPU-side startup cost, so graphics-driver differences do not leak in.
///
/// A process-side "wait for the window to appear" would add Windows animation and DWM
/// compositing noise (200ms+), which cannot show a code regression. `tools/ux_probe.ps1`
/// measures that user-perceived moment instead and is not used as a fence.
struct StartupProbe {
    inner: AevumApp,
    started: std::time::Instant,
    reported: bool,
}

impl StartupProbe {
    fn new(inner: AevumApp, started: std::time::Instant) -> Self {
        Self {
            inner,
            started,
            reported: false,
        }
    }
}

impl eframe::App for StartupProbe {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut self.inner, ui, frame);
        if self.reported {
            return;
        }
        self.reported = true;
        let ms = self.started.elapsed().as_secs_f64() * 1000.0;
        say!("启动到首帧 = {ms:.0}ms（窗口创建 + 首帧 UI + 首个贴图上传）");
        // 立刻关窗：基准进程不许挂在事件循环里等用户。
        //
        // Close right away so the benchmark process does not sit in the event loop.
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        eframe::App::clear_color(&self.inner, visuals)
    }
}

fn print_help() {
    say!("{}", aevum_core::brand::TITLE);
    say!();
    say!("用法：aevum [目录] [选项]");
    say!();
    say!("  <目录>            打开一个帧序列");
    say!("  --open <目录>     同上");
    say!("  --demo            用合成演示序列启动");
    say!("  --shot <png>      截图到指定文件后退出（缺省用演示序列）");
    say!("  --demo-edit       截图前套一套演示编辑（径向蒙版）");
    say!("  --shot-queue      截图前用几行演示数据填满渲染队列");
    say!("  --shot-end        截图前把右侧面板滚到底，用来拍长面板的下半截");
    say!("  --shot-group <名> 截图前展开并把某个分组滚到面板顶部，如 masks、color");
    say!("  --shot-channels   截图前勾上「分通道（RGB 色闪）」去闪，用来拍色闪被压住的状态");
    say!("  --shot-crop       截图前用 crop.set 拉一个内收的取景框，拍裁剪态的画布");
    say!("  --shot-rotate     截图前用 crop.set 拉直 6°，拍削过空白角的取景态画布");
    say!(
        "  --shot-output     截图前用 render.set 改成静帧 JPEG + 质量 80 + 输出锐化，拍出输出面板的成片档"
    );
    say!(
        "  --shot-watermark  截图前用 render.set 盖上右下文字水印（正文 + 位置 + 不透明度），拍水印组"
    );
    say!(
        "  --shot-retime     截图前用 retime.set 套一条缓入缓出曲线 + 快门，拍「变速 · 运动模糊」组"
    );
    say!(
        "  --shot-motion     截图前用 motion.set 摆开起点/终点两个框并开「拖取景框」，拍运镜组与画布"
    );
    say!("  --shot-brush      截图前用 mask.add + mask.update 画一笔画笔蒙版，拍画笔组");
    say!("  --shot-library    截图前造两个演示目录（带评分）用 library.add 加进图库，拍图库面板");
    say!("  --shot-collection 配合 --shot-library：打开演示集合「精选」，拍集合视图");
    say!("  --shot-libmeta    配合 --shot-library：给演示目录写说明与版权并展开信息面板");
    say!("  --shot-libstack   配合 --shot-library：给演示目录造两叠并展开一叠，拍堆叠折叠态");
    say!(
        "  --shot-dock <名>  截图前切到右侧面板：调整/信息/关键帧/AI/历史（拍「历史」会先造几步）"
    );
    say!("  --module <模块>   截图时停在指定模块：序列/去闪/显影/构图/成片/声音");
    say!("  --selftest        无显示器跑一遍布局自检");
    say!("  --bench-ui        无显示器跑帧时基准（面板切换 / 滑块拖动，打印中位与 p95）");
    say!(
        "  --bench-preview   无显示器跑首屏预览基准（清缓存后量解码/调色/成图，样张取自 samples/cameras）"
    );
    say!("  --bench-startup   起窗量到首帧画完为止（毫秒），打印后立刻退出；脚本跑 5 次取中位");
    say!("  --mcp             以 MCP stdio server 无头运行，供 agent 调用");
    say!("  --mcp --attach <control.json>");
    say!("                    附着到正在运行的 GUI，把工具调用转给窗口（界面实时更新）");
    say!("  -h, --help        显示这段说明");
}

/// 任务栏与 Alt-Tab 上的图标。文件图标走 build.rs 的资源段，那是另一条路，两边都得有。
fn window_icon() -> egui::IconData {
    const SIDE: u32 = 256;
    let png = include_bytes!("../../../assets/app.png");
    let rgba = image::load_from_memory(png)
        .map(|img| {
            image::imageops::resize(
                &img.to_rgba8(),
                SIDE,
                SIDE,
                image::imageops::FilterType::Lanczos3,
            )
        })
        .expect("assets/app.png 是随 exe 编进来的，不该读不出");
    egui::IconData {
        rgba: rgba.into_raw(),
        width: SIDE,
        height: SIDE,
    }
}

fn parse_module(name: &str) -> Option<Module> {
    match name.trim() {
        "序列" | "sequence" | "seq" | "0" => Some(Module::Sequence),
        "去闪" | "deflicker" | "def" | "1" => Some(Module::Deflicker),
        "显影" | "develop" | "dev" | "2" => Some(Module::Develop),
        "构图" | "frame" | "3" => Some(Module::Frame),
        "成片" | "render" | "4" => Some(Module::Render),
        "声音" | "audio" | "5" => Some(Module::Audio),
        _ => None,
    }
}
