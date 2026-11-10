//! 双语完整性：把界面**真的画一遍**，再问「有没有哪条文案没译文」。
//!
//! # 为什么不是「扫一遍源码里的中文字面量」
//!
//! 试过，扫不干净。界面文案有三条来源：
//!
//! 1. 调用点上的字面量 —— `t!("曝光")`，grep 得到；
//! 2. **辅助函数的实参** —— `bar_button(ui, "导出")`，翻译下沉在 `bar_button`
//!    内部，字面量本身不带 `t!`；
//! 3. **数据表** —— `controls::SPECS` 的 `label`、`grade::PRESET_NAMES`、
//!    `DockTab::cn()` 的 match 臂。这些根本没有「调用点」可言，值是 `const`
//!    数据，只能由运行期查表翻译。
//!
//! 前两条靠 `tools/i18n_extract.py` 抽；第三条它看不见——正是上一轮「英文界面里
//! 滑块标签和 dock 标签还是中文」的成因。所以这里换个方向：不猜源码，**直接画**。
//! `tr()` 在 debug 构建里把落空的键记进探针（见 `i18n::take_untranslated`），
//! 画完读探针，空即通过。
//!
//! 两条测试分工：
//!
//! * [`english_ui_has_no_untranslated_text`] —— 运行期真值，覆盖所有模块 × 所有
//!   dock 标签 × 所有弹窗；
//! * [`data_table_labels_are_all_translated`] —— 静态表，逐个枚举 `SPECS` /
//!   `PRESET_NAMES` / 各 `cn()` 枚举。它比第一条快得多，出错时也更指向具体哪条，
//!   留着当第一道闸。

use aevum_ui::i18n::{self, Lang};

/// 缺译探针是进程级静态，而 `cargo test` 默认并行：两个扫描测试各画一遍界面，
/// 一边 `take` 一边另一边的帧还在往里写，结果会时好时坏。三处碰探针的测试
/// 都先拿这把锁，宁可串行——这是回归基线，不能看心情通过。
///
/// 锁本身在 `common` 里，因为 `i18n_errors` 也要改当前语言；两个测试文件现在编进
/// 同一个可执行文件（台账 C1），各拿各的锁等于没锁。见 `common.rs`。
///
/// The probe is a process-wide static and `cargo test` is parallel by default: two
/// scanning tests paint the UI at once, one `take`s while the other is still writing, and
/// the result flips run to run. Every test that touches the probe takes this lock.
///
/// The lock lives in `common` because `i18n_errors` switches the language too, and both
/// files are now compiled into one executable (ledger item C1); separate locks would be no
/// lock at all. See `common.rs`.
fn probe_guard() -> std::sync::MutexGuard<'static, ()> {
    crate::common::i18n_guard()
}
use aevum_ui::state::{AppState, DockTab, Module};
use aevum_ui::{shell, theme};

/// 与 `--selftest` 同一块画布：1600×1000，最大纹理 8192。
fn raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        max_texture_side: Some(8192),
        ..Default::default()
    }
}

/// 画一帧，顺带把这一帧**画出来的每一个汉字串**收进 `screen`。
///
/// 收的是渲染结果而不是调用点：只要某个字上屏了，不管它是字面量、辅助函数的实参、
/// 数据表里的一格，还是 `format!` 拼出来的，这里都看得见。缺译探针查的是「键不在
/// 表里」，这一条查的是「屏幕上还有中文」——后者才是用户看到的东西。
fn frame(state: &mut AppState, ctx: &egui::Context, screen: &mut Vec<String>) {
    let mut out = ctx.run_ui(raw_input(), |ui| shell::ui(state, ui));
    for cs in &out.shapes {
        collect_text(&cs.shape, screen);
    }
    out.textures_delta.clear();
}

fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(t) => {
            let text = t.galley.text();
            if i18n::needs_translation(text) {
                out.push(text.to_owned());
            }
        }
        egui::Shape::Vec(shapes) => {
            for s in shapes {
                collect_text(s, out);
            }
        }
        _ => {}
    }
}

/// 切到 `lang`，把整个界面画一遍，返回这一遍里落空的键。
fn sweep(lang: Lang) -> (Vec<String>, Vec<String>) {
    i18n::set(lang);
    let _ = i18n::take_untranslated();

    let ctx = egui::Context::default();
    theme::apply(&ctx, 1.0);
    let mut state = AppState::demo();
    state.install_demo_textures(&ctx);
    // 工程元数据是**用户数据**，不是界面文案：演示工程里写着中文的标题、创作者、
    // 版权，切到英文后当然还是中文。不清掉的话每一帧都会把它们报成漏译。
    neutralise_demo_data(&mut state);
    let mut screen: Vec<String> = Vec::new();

    // 头两帧会把字体与纹理的冷启动开销吃掉，读数才是稳态。
    frame(&mut state, &ctx, &mut screen);
    frame(&mut state, &ctx, &mut screen);

    // 六个模块 × 六个 dock 标签：dock 只画当前那一页，不逐个切就是漏的。
    //
    // 每个模块跑两遍：先默认态（低频组是折叠的），再全展开。折叠组里的文案
    // ——相机镜头配置、水印、HDR/全景合并那些——默认态根本不画，只扫默认态
    // 就等于把这些段落的翻译验收整个跳过了。
    for module in Module::ALL {
        state.module = module;
        state.collapsed = aevum_ui::state::default_collapsed();
        for tab in DockTab::ALL {
            state.dock_tab = tab;
            frame(&mut state, &ctx, &mut screen);
        }
        state.collapsed.clear();
        for tab in DockTab::ALL {
            state.dock_tab = tab;
            frame(&mut state, &ctx, &mut screen);
        }
    }

    // 弹窗。逐个开关：同时开着会互相压在最上层，后开的那个把先开的挡住，
    // 被挡住的窗口这一帧不画字，等于白扫。
    for which in 0..4 {
        match which {
            0 => state.look_open = true,
            1 => state.about_open = true,
            2 => state.market_open = true,
            _ => state.show_curves = true,
        }
        frame(&mut state, &ctx, &mut screen);
        match which {
            0 => state.look_open = false,
            1 => state.about_open = false,
            2 => state.market_open = false,
            _ => state.show_curves = false,
        }
    }

    // 覆盖洞：上面每一帧都基于 AppState::demo()（有纹理、有直方图、序列在、
    // 队列空），空态引导文案与帧表工作区的表头/页脚/摘要卡**根本不渲染**。
    // 于是「调用点把中文字面量直接喂 p.text」能在两条判据全绿时长期潜伏。
    // 补两类帧：空序列（画布 empty_state）与 Bench::Table 工作台。
    //
    // Coverage hole: every frame above used AppState::demo() (textures, histogram,
    // a sequence and an empty queue), so the empty-state prompts and the frame-table
    // bench header/footer/summary cards were never painted. Literal Chinese strings
    // survived there while both assertions stayed green. Add two frames: an empty
    // sequence and the Table bench.
    let mut empty_app = AppState::new();
    frame(&mut empty_app, &ctx, &mut screen);
    state.bench = aevum_ui::shell::Bench::Table;
    frame(&mut state, &ctx, &mut screen);
    state.bench = aevum_ui::shell::Bench::Develop;

    screen.sort_unstable();
    screen.dedup();
    (i18n::take_untranslated(), screen)
}

/// 把演示工程里属于「用户自己写的东西」的那几项换成 ASCII，留下纯界面文案。
fn neutralise_demo_data(state: &mut AppState) {
    let meta = &mut state.meta;
    meta.title = "Demo".into();
    meta.description = "Demo".into();
    meta.creator = "Demo".into();
    meta.copyright = "(c) 2026 Demo".into();
    // 状态栏与体检清单里预置的那几句也是演示内容（`AppState::demo()` 造的假数据），
    // 不是界面文案：真跑起来它们由事件处理那条路写，那条路由 `format!` 那批统一接管。
    state.status = "Demo sequence · 48 frames".into();
    state.issues = vec![(17, "demo: missing frame".to_owned())];
}

/// 英文界面上不许有一个中文键落空。
///
/// 失败信息直接给出补表用的 `("中文", ""),` 行——照着填 `i18n_en.rs` 即可。
#[test]
fn english_ui_has_no_untranslated_text() {
    let _guard = probe_guard();
    let (missing, _) = sweep(Lang::EN);
    // 探针必须在每台机器上都能真的记到东西，否则这条测试会「静默通过」。
    // 用一个肯定不在表里的键自检一次。
    assert!(
        i18n::has_in(Lang::EN, "曝光"),
        "英文表被清空过？连「曝光」都查不到，探针的可靠性无从谈起"
    );
    assert!(
        missing.is_empty(),
        "英文界面还有 {} 条没译文，照着补 crates/aevum-ui/src/i18n_en.rs：\n{}",
        missing.len(),
        report(&missing)
    );
}

/// 屏幕上不许出现中文。这条比上面那条严：它不管文案是怎么来的。
#[test]
fn no_chinese_is_painted_in_english_mode() {
    let _guard = probe_guard();
    let (_, painted) = sweep(Lang::EN);
    // 语言选择器上「中文」那一项按惯例用该语言自己的写法，不翻译。
    // · 「中文」：语言选择器上那一项按惯例用该语言自己的写法。
    // · 「演示序列」：`AppState::demo()` 自造的目录名，属于演示数据而不是界面文案。
    const ALLOWED: &[&str] = &["中文", "演示序列"];
    let bad: Vec<String> = painted
        .into_iter()
        .filter(|s| !ALLOWED.contains(&s.as_str()))
        .collect();
    assert!(
        bad.is_empty(),
        "英文界面上还画着 {} 条中文，逐条查渲染路径（字面量 / 辅助函数 / 数据表 / format!）：\n{}",
        bad.len(),
        report(&bad)
    );
}

fn report(items: &[String]) -> String {
    items
        .iter()
        .map(|k| format!("    ({k:?}, \"\"),"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 探针本身得能报错——否则上面那条测试看到的是「一切正常」而不是「没在记账」。
#[test]
fn the_miss_recorder_actually_records() {
    let _guard = probe_guard();
    i18n::set(Lang::EN);
    let _ = i18n::take_untranslated();
    let probe = "__i18n_probe__不该出现的键";
    assert_eq!(i18n::tr(probe), probe, "落空时必须原样返回，不能变成空串");
    assert_eq!(i18n::take_untranslated(), vec![probe.to_owned()]);
    // 取走即清空，不该重复计数。
    assert!(i18n::take_untranslated().is_empty());
    i18n::set(Lang::ZH);
}

/// 数据表里的标签逐条核对，**每一种翻译语言各查一遍**。
///
/// 这一条不画界面，所以快；它拦的是「新加了一条推子/预设，忘了补某张表」，
/// 那时候 `SPECS` 里多一行，这里立刻点名，并指出缺的是哪种语言。
///
/// 判据遍历 `Lang::all()` 而不是写死英文：加一种语言时它自动把新表也管上，不需要
/// 改这条测试。查表也**不看当前语言**——测试是并行的，`current()` 是全局状态，
/// 依赖它会让结果随时机漂移（这次改造正是被这一点先照出来的）。
#[test]
fn data_table_labels_are_all_translated() {
    let mut missing: Vec<String> = Vec::new();
    // `AI`、`Windows`、`macOS` 这类本来两边写法相同，没有译文是正常的。
    let want = |text: &'static str, missing: &mut Vec<String>| {
        if text.is_empty() || !i18n::needs_translation(text) {
            return;
        }
        for l in Lang::all() {
            if l.catalog().is_source() {
                continue;
            }
            if !i18n::has_in(l, text) {
                missing.push(format!("{} · {text}", l.code()));
            }
        }
    };

    // 推子的标签与 tooltip。空串表示「不挂」，不是文案。
    for s in aevum_ui::controls::SPECS {
        want(s.label, &mut missing);
        want(s.tip, &mut missing);
    }

    // 调色预设名。文件名/命令行里的短名不在这里，那是稳定 id，不翻。
    for name in aevum_core::grade::PRESET_NAMES {
        want(name, &mut missing);
    }

    // 各枚举的界面名。`cn()` 返回的是中文原文，也就是查表的键。
    for m in Module::ALL {
        want(m.cn(), &mut missing);
    }
    for t in DockTab::ALL {
        want(t.cn(), &mut missing);
    }
    for t in aevum_ui::state::Tool::ALL {
        want(t.cn(), &mut missing);
        want(t.hint(), &mut missing);
    }
    for m in aevum_ui::theme::ThemeMode::ALL {
        want(m.cn(), &mut missing);
    }
    for s in aevum_ui::theme::Skin::ALL {
        want(s.cn(), &mut missing);
    }
    for d in aevum_ui::theme::Density::ALL {
        want(d.cn(), &mut missing);
    }
    for s in aevum_ui::layout::StatusSlot::ALL {
        want(s.cn(), &mut missing);
    }
    for p in aevum_ui::layout::LayoutPreset::ALL {
        want(p.cn(), &mut missing);
    }
    for o in aevum_ui::platform::OsChoice::ALL {
        want(o.cn(), &mut missing);
    }

    missing.sort_unstable();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "数据表里还有 {} 条标签没译文（格式是「语言 · 中文原文」）：\n{}",
        missing.len(),
        missing
            .iter()
            .map(|k| format!("    {k}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
