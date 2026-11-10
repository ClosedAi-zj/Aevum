//! 静态守门：界面「一套口径」不许被随手写死破坏。
//!
//! # 这些测试为什么存在
//!
//! 用户反馈里最刺人的一句是「这些缺陷要我自己一个个指出来」。回头看，被指出的
//! 几条其实都是**同一种病**：某个数值在别处已经收敛成了 token，但有一两个调用点
//! 还留着当年的字面量，于是画出来「差一点」——肉眼能看出不对，代码里搜不到。
//!
//! * 标题条写死 `corner_radius(0.0)` → 整窗三只角圆、上两角方（`UI-1`）；
//! * `Window::anchor(…)` → egui 源码注释原话 "Anchoring also makes the window
//!   immovable"，四个浮窗全都拖不动（`UI-2`）；
//! * 浮窗里混用 `ui.separator()` → 通栏实线把面板切成两截，与同时改过的软分割线
//!   并排出现（`UI-1`）；
//! * 设置入口只挂在菜单里 → 语言与外观「功能在但没人找得到」（`UI-7`）。
//!
//! 这些都能在**编译前的静态层**拦住，而且拦的成本极低：扫源码文本即可。所以这里
//! 不做像素比对（那要跑起渲染器、要维护基线图，重且脆），只钉四条不变式：
//!
//! 1. 浮窗不许 `anchor`（一 anchor 就 immovable）；
//! 2. 浮窗的标题条只有一套（`title_frame()`），且圆角只认 `radius_window()`；
//! 3. 浮窗内的分割只用软线（`soft_sep`），不用 `ui.separator()`；
//! 4. 圆角实参要么是 `0`/`0.0`（显式的直角），要么来自 `theme::radius_*` 一族。
//!
//! 另外两条钉的是「口径本身还站得住」：刻度有序、入口常驻。
//!
//! # 为什么不扫 `Color32`
//!
//! 试过：`shell/` 下直写颜色有 60+ 处，绝大多数是画布叠加层（取景框阴影、蒙版
//! 手柄、直方图渐变）与 macOS 交通灯的固有色——那些颜色**本来就是像素级常量**，
//! 收进 token 只会把 `theme.rs` 变成垃圾场。所以颜色不设静态禁令，交给
//! `theme.rs` 里那批对比度测试（WCAG AA）与 `--selftest` 兜。

use std::path::{Path, PathBuf};

/// 把整行注释抹成等长空格：保留字节偏移，后续 `line_of` 仍指得准。
///
/// 只抹「整行以 `//` 开头」的注释。要遮的正是文档注释——`windows.rs` 的模块注释里
/// 有一句「此前这里写死 `corner_radius(0.0)`」，那是**历史说明**，不是调用点；
/// 不遮它，这条测试会去审一句注释。
fn without_line_comments(src: &str) -> String {
    src.split('\n')
        .map(|line| {
            if line.trim_start().starts_with("//") {
                " ".repeat(line.len())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 取 `open` 处括号里的**第一个**实参区间（字节偏移，右开）。
///
/// 只做括号与字符串配平：足够应付 `corner_radius(…)` 这类单参调用。
fn first_arg(src: &str, open: usize) -> Option<(usize, usize)> {
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let start = open + 1;
    let mut i = open;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((start, i));
                }
            }
            b',' if depth == 1 => return Some((start, i)),
            _ => {}
        }
        i += 1;
    }
    None
}

/// 取某个函数定义的函数体文本（按花括号配平）。找不到签名就返回 `None`。
///
/// 用来把「这句 token 是不是在这个函数里取的」问清楚，而不是全文件模糊匹配——
/// 全文件匹配会被隔壁函数的同名调用蒙过去。
fn fn_body<'a>(src: &'a str, signature: &str) -> Option<&'a str> {
    let at = src.find(signature)?;
    let open = src[at..].find('{')? + at;
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut i = open;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&src[open..i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 把 `open` 处括号里的实参按顶层逗号切开（字节偏移，右开）。
///
/// `first_arg` 只认第一个；`group_header` 的键是第三个实参，所以要多参版本。
/// 跳过字符串里的逗号与括号这一层必须做：实参里会有 `format!("{}, {}")`。
fn args_of(src: &str, open: usize) -> Vec<(usize, usize)> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut start = open + 1;
    let mut i = open;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    out.push((start, i));
                    return out;
                }
            }
            b',' if depth == 1 => {
                out.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn line_of(src: &str, at: usize) -> usize {
    src[..at].bytes().filter(|b| *b == b'\n').count() + 1
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// 读一份源码并去掉整行注释；文件不在就当作空串（测试自己会 assert 扫到了东西）。
fn read_clean(rel: &str) -> String {
    let p = src_dir().join(rel);
    let raw = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读不到 {p:?}：{e}"));
    without_line_comments(&raw)
}

/// 读一份源码；若它已经被拆成一个模块目录（`shell/adjust`），把目录里的 `*.rs`
/// 按文件名序拼起来再读。判据盯的是「这个模块里出现过哪些东西」，不是它分成几个
/// 文件——按路径写死会让拆文件变成红，那是把版式当契约。
///
/// Reads one source file; if it has been split into a module directory (`shell/adjust`),
/// concatenates that directory's `*.rs` in filename order. The criterion is about what the
/// module mentions, not how many files it lives in -- pinning the path would turn a file
/// split into a red, which mistakes layout for contract.
fn read_clean_any(rel: &str) -> String {
    let p = src_dir().join(rel);
    if p.is_file() {
        return read_clean(rel);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&p)
        .unwrap_or_else(|e| panic!("读不到 {p:?}：{e}"))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|q| q.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "{p:?} 里一个 .rs 都没有");
    let mut out = String::new();
    for q in files {
        let raw = std::fs::read_to_string(&q).unwrap_or_else(|e| panic!("读不到 {q:?}：{e}"));
        out.push_str(&raw);
        out.push('\n');
    }
    without_line_comments(&out)
}

#[test]
fn no_floating_window_is_anchored() {
    // egui 0.37 的 `Window::anchor` 会把窗口钉死：源码注释原话
    // "Anchoring also makes the window immovable"。想要「首开在某个位置、之后能拖」，
    // 唯一正解是 `default_pos`（首帧生效，用户拖动后不再干预）。
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    assert!(files.len() > 20, "源码目录没扫到东西，路径口径变了");

    let mut hits = Vec::new();
    for path in &files {
        let src = without_line_comments(&std::fs::read_to_string(path).expect("读源码"));
        for (pos, _) in src.match_indices(".anchor(") {
            hits.push(format!(
                "{}:{}",
                path.strip_prefix(src_dir()).unwrap_or(path).display(),
                line_of(&src, pos)
            ));
        }
    }

    assert!(
        hits.is_empty(),
        "这些地方用了 `.anchor(…)`，浮窗会变成拖不动的死窗（{} 处）：\n{}\n\n\
         改法：`.default_pos(ctx.input(|i| i.viewport_rect()).… )` —— 首开位置照旧，\
         之后交给用户拖。位置若要跨启动记忆，存 settings.json，别用 anchor 变相实现。",
        hits.len(),
        hits.join("\n"),
    );
}

/// 每个浮窗都必须**取位置**（`restored_pos`）并**写回位置**（`remember_pos`）。
///
/// 这条挡的是「新加一个浮窗就忘了接位置持久化」——写漏的时候程序不报错、
/// 界面也正常，只是那个窗每次启动都回默认位，没人会注意到。
///
/// 判据是三个计数相等：`egui::Window::new(` 出现几次，就必须各有几次
/// `restored_pos(` / `remember_pos(`。数相等而配错对（A 窗取、B 窗写）也可能，
/// 所以再按窗口块切一遍：每个 `Window::new(` 到下一个之间必须两者都有。
///
/// Every floating window must **read** its position (`restored_pos`) and **write** it
/// back (`remember_pos`). This catches "new floating window, forgot to hook up position
/// persistence" -- which fails silently: nothing errors, the UI looks fine, the window
/// just opens at its default spot every launch and nobody notices.
///
/// The criterion is three equal counts, then a per-block check (a window that reads
/// someone else's position and writes yet another would still balance the counts).
#[test]
fn every_floating_window_restores_and_records_its_position() {
    let src = read_clean("shell/windows.rs");
    let starts: Vec<usize> = src
        .match_indices("egui::Window::new(")
        .map(|(p, _)| p)
        .collect();
    assert!(
        starts.len() >= 3,
        "只找到 {} 个浮窗，扫描口径变了",
        starts.len()
    );

    for (i, start) in starts.iter().enumerate() {
        // 块 = 本窗口的 `Window::new(` 到下一个之间。`restored_pos` 写在
        // `Window::new` **之前**（位置得先算），所以往前多看一段。
        let head = src[..*start].rfind("fn ").unwrap_or(0);
        let block = &src[head..*start];
        let tail_end = starts.get(i + 1).copied().unwrap_or(src.len());
        let body = &src[*start..tail_end];
        let name = src[*start..]
            .split('\n')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        assert!(
            block.contains("restored_pos("),
            "第 {} 个浮窗（{name}）没有经 restored_pos 取位置——重启后回默认位：\n{block}",
            i + 1
        );
        assert!(
            body.contains("remember_pos("),
            "第 {} 个浮窗（{name}）没有 remember_pos 写回位置——拖了记不住：\n{name}",
            i + 1
        );
    }
}

/// 预设栏三节必须按 `state.preset_order` 排，且节头挂拖拽。
///
/// 这条挡的是「有人把次序写回代码里」——写死了照样能编译、能显示，只是用户拖过
/// 的次序下次启动就没了，而且没有任何测试会红。
///
/// The three preset sections must follow `state.preset_order` and their headers must be
/// drag handles. This catches "someone hard-codes the order again": that compiles and
/// displays fine, it just silently drops the order the user arranged.
#[test]
fn preset_sections_are_ordered_by_state_and_draggable() {
    // 2026-11-04 起预设/快照/副本住「预设与快照」浮窗（windows.rs），判据跟着搬：
    // 两个文件都不许把节头次序写死。
    //
    // Since 2026-11-04 presets/snapshots/copies live in the Presets & Snapshots window
    // (windows.rs); the criteria moved with it: neither file may hard-code section order.
    for file in ["shell/windows.rs", "shell/common.rs"] {
        let src = read_clean(file);
        assert!(
            !src.contains("side_head(ui, \"内置\"")
                && !src.contains("side_head(ui, \"快照\"")
                && !src.contains("side_head(ui, \"历史\"")
                && !src.contains("side_head(ui, \"调色副本\""),
            "{file} 出现了写死的节头——次序应该由 `preset_order` 决定"
        );
    }
    let src = read_clean("shell/windows.rs");
    assert!(
        src.contains("state.preset_order"),
        "预设窗没有读 `state.preset_order`——次序又被写死进代码了"
    );
    assert!(
        src.contains("move_preset_section("),
        "预设窗没有调用 `move_preset_section`——拖了也不会换序"
    );
}

#[test]
fn floating_windows_share_one_title_frame() {
    // 「四个圆角弧度一致」不是靠逐个窗口调参调出来的，而是靠**只有一个标题条实现**。
    // 这条测试钉住：windows.rs 里每个 `egui::Window` 都必须挂同一个 `title_frame()`。
    let src = read_clean("shell/windows.rs");
    let starts: Vec<usize> = src
        .match_indices("egui::Window::new(")
        .map(|(p, _)| p)
        .collect();
    assert!(
        starts.len() >= 3,
        "windows.rs 里只找到 {} 个浮窗，预期至少 3 个（插件 / 外观与布局 / 快捷键与关于）。\
         是不是换了构造方式？换了就把这条测试的口径一起改。",
        starts.len()
    );

    let mut missing = Vec::new();
    for (i, start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(src.len());
        let chain = &src[*start..end];
        // 只看 `.show(` 之前那段 builder 链：`.show()` 的闭包里是窗内容，不算。
        let head = chain.find(".show(").map(|k| &chain[..k]).unwrap_or(chain);
        if !head.contains(".title_frame(title_frame())") {
            missing.push(line_of(&src, *start));
        }
    }

    assert!(
        missing.is_empty(),
        "这些浮窗没挂 `title_frame()`（windows.rs 第 {:?} 行）：标题条会退回默认方角，\
         压下窗体的圆角，于是整窗四角看起来不一致。",
        missing
    );
}

#[test]
fn one_radius_for_the_whole_window() {
    // 标题条的圆角必须由上两角给满、下两角归零（下两角是内容区，给了会是内凹）。
    // 数值只认 `radius_window()`：写死 `0.0` 就是 UI-1 那个缺陷的原始形态。
    let src = read_clean("shell/windows.rs");

    let radius_sites: Vec<usize> = src
        .match_indices("corner_radius(")
        .map(|(p, _)| p)
        .collect();
    assert_eq!(
        radius_sites.len(),
        1,
        "windows.rs 里出现了 {} 处 `corner_radius(`，预期恰好 1 处（在 `title_frame()` 里）。\
         多出来的那处会让某一个窗口/面板的弧度与整窗对不上——这正是 UI-1 的病因。",
        radius_sites.len()
    );

    let open = radius_sites[0] + "corner_radius".len();
    let arg = first_arg(&src, open).expect("corner_radius 的实参没配平");
    let arg = src[arg.0..arg.1].trim_end();

    // 弧度是按 `u8` 传的，所以调用点写的是局部变量（`nw: r`）；数值仍必须来自
    // `radius_window()`——于是断言分两层：外层函数体里有 token，内层四角引同一个值。
    let body = fn_body(&src, "fn title_frame(").expect("找不到 `title_frame()`");
    assert!(
        body.contains("radius_window()"),
        "标题条的弧度不是从 `radius_window()` 取的。写死的弧度没法跟控件圆角联动，\
         用户一调外观档位，标题条就会和窗体对不上。"
    );

    let corner = |side: &str| -> Option<String> {
        let at = arg.find(&format!("{side}:"))?;
        let rest = &arg[at + side.len() + 1..];
        let value: String = rest
            .chars()
            .take_while(|c| *c != ',' && *c != '}')
            .collect();
        Some(value.trim().to_string())
    };

    let nw = corner("nw").unwrap_or_else(|| panic!("标题条缺 `nw` 角。实参：{arg}"));
    let ne = corner("ne").unwrap_or_else(|| panic!("标题条缺 `ne` 角。实参：{arg}"));
    assert_eq!(
        nw, ne,
        "上两角弧度不一致（nw={nw} / ne={ne}）：这就是「四个角看着不一样」的直接来路。"
    );
    assert_ne!(
        nw, "0",
        "上两角是 0 就是方角标题条压在圆角窗体上——UI-1 的原始缺陷。"
    );

    for side in ["sw", "se"] {
        let v = corner(side).unwrap_or_else(|| panic!("标题条缺 `{side}` 角。实参：{arg}"));
        assert_eq!(
            v, "0",
            "标题条的下两角必须归零（接的是方角内容区，给了圆角会内凹），现在是 {v}。"
        );
    }
}

#[test]
fn floating_windows_use_soft_separators() {
    // 浮窗里的分节线走 `soft_sep`：左右缩进 12px + 压暗。通栏 `ui.separator()`
    // 读起来是「把窗口切成两半」，缩进浅线读起来才是「分节」。
    let src = read_clean("shell/windows.rs");

    let raw: Vec<usize> = src
        .match_indices("ui.separator()")
        .map(|(p, _)| p)
        .collect();
    assert!(
        raw.is_empty(),
        "windows.rs 里还有 {} 处 `ui.separator()`（第 {:?} 行）：通栏实线会把面板读成两截，\
         与别处的软分割线并排就显脏。换成 `soft_sep(ui)`。",
        raw.len(),
        raw.iter().map(|p| line_of(&src, *p)).collect::<Vec<_>>()
    );

    assert!(
        src.contains("fn soft_sep("),
        "`soft_sep` 不见了。分割样式只有这一个实现，删了它各处就会各写各的。"
    );
    let uses = src.matches("soft_sep(ui)").count();
    assert!(
        uses >= 5,
        "只在 {} 处用了 `soft_sep(ui)`，浮窗里的分节少得可疑——是不是又混回了别的画法？",
        uses
    );
}

#[test]
fn corner_radius_arguments_are_theme_tokens() {
    // 圆角只有三个来源：`0`/`0.0`（显式直角）、`theme::radius_*` 一族、或包成
    // `egui::CornerRadius { … }` 分角（分角内部的值同样得来自 token）。
    // 写死 `corner_radius(3.0)` 这类字面量，就是「看起来差不多」的源头。
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);

    let mut hits = Vec::new();
    for path in &files {
        let src = without_line_comments(&std::fs::read_to_string(path).expect("读源码"));
        for (pos, _) in src.match_indices("corner_radius(") {
            let open = pos + "corner_radius".len();
            let Some(span) = first_arg(&src, open) else {
                continue;
            };
            let arg = src[span.0..span.1].trim();
            let literal = arg
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || c == '-');
            // 直角是有意义的显式选择，放行；其余数字一律要 token。
            let square = arg == "0" || arg == "0.0";
            if literal && !square {
                hits.push(format!(
                    "{}:{}  corner_radius({arg})",
                    path.strip_prefix(src_dir()).unwrap_or(path).display(),
                    line_of(&src, pos),
                ));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "这些圆角写死了数值（{} 处）：\n{}\n\n\
         改法：控件用 `theme::radius_control()`、窗用 `radius_window()`、容器用 \
         `radius_panel()`；确实是直角就写明 `0`，让它一眼看出是故意的。",
        hits.len(),
        hits.join("\n"),
    );
}

#[test]
fn radius_scale_stays_ordered() {
    // 三档圆角必须是一个递增序列：窗 > 容器 > 控件。任何一档被改成常量或写反，
    // 组合出来的界面就会「窗口比按钮还方」。
    use aevum_ui::theme;
    let control = theme::radius_control();
    let panel = theme::radius_panel();
    let window = theme::radius_window();
    assert!(control >= 0.0, "控件圆角不能是负的：{control}");
    assert!(
        control < panel && panel < window,
        "圆角刻度失序：控件 {control} / 容器 {panel} / 窗 {window}，应为 控件 < 容器 < 窗"
    );
}

#[test]
fn type_scale_stays_ordered() {
    // 字号同口径：小 < 标签 < 正文 < 标题。有一档写反，面板就会「说明文字比标题还大」。
    use aevum_ui::theme;
    let s = theme::SIZE_SMALL;
    let l = theme::SIZE_LABEL;
    let b = theme::SIZE_BODY;
    let h = theme::SIZE_HEADING;
    assert!(
        s < l && l < b && b < h,
        "字号刻度失序：小 {s} / 标签 {l} / 正文 {b} / 标题 {h}"
    );
}

#[test]
fn appearance_entry_point_stays_in_the_top_bar() {
    // UI-7 的教训：语言与外观**功能早就有**，但入口只挂在「文件 → 外观」之类的地方，
    // 用户找不到，于是反馈成「承诺的功能没做」。这条钉住顶栏那枚齿轮别再被收走。
    let src = read_clean("shell/menu.rs");
    assert!(
        src.contains("Glyph::Gear"),
        "顶栏常驻的设置齿轮不见了。它是外观 / 语言 / 主题的唯一可见入口——\
         撤回它，那些功能就等于不存在。"
    );
    let gear = src.find("Glyph::Gear").expect("刚断言过");
    let tail = &src[gear..];
    let window_end = tail.find("\n}");
    let scope = window_end.map(|k| &tail[..k]).unwrap_or(tail);
    assert!(
        scope.contains("look_open = true"),
        "齿轮还在，但点了不开外观窗（附近找不到 `look_open = true`）。"
    );
}

#[test]
fn panel_groups_match_the_registry() {
    // `state::GROUP_SPECS` 一张表管三件事：首屏折叠、右键「只看这一组」、落盘回查。
    // 三者都靠**键名对上**，而键名是字符串 —— 打错一个字母不会编译失败，只会
    // 悄悄失效（默认折叠不生效、或者落盘读回来被当垃圾键丢掉）。这条测试把
    // 「源码里的 group_header 调用」和表逐条对齐。
    use aevum_ui::state::GROUP_SPECS;
    use std::collections::BTreeSet;

    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    let mut seen: Vec<(String, String, usize)> = Vec::new();
    for path in &files {
        let src = without_line_comments(&std::fs::read_to_string(path).expect("读源码"));
        for (pos, _) in src.match_indices("group_header(") {
            let open = pos + "group_header".len();
            // 第 3 个实参才是键（ui, state, key, title, resettable）。
            let Some(span) = args_of(&src, open).into_iter().nth(2) else {
                continue;
            };
            let arg = src[span.0..span.1].trim();
            let Some(key) = arg.strip_prefix('"').and_then(|r| r.split('"').next()) else {
                continue; // 键不是字面量：动态拼的组名登记不了，也进不了设置文件。
            };
            seen.push((
                key.to_owned(),
                path.strip_prefix(src_dir())
                    .unwrap_or(path)
                    .display()
                    .to_string(),
                line_of(&src, pos),
            ));
        }
    }

    assert!(
        seen.len() > 40,
        "只扫到 {} 个 group_header 调用，路径或调用形状变了？",
        seen.len()
    );

    let registry: BTreeSet<&str> = GROUP_SPECS.iter().map(|s| s.key).collect();
    assert_eq!(
        registry.len(),
        GROUP_SPECS.len(),
        "登记表里有重复键：折叠状态会写到同一个键上，两个组一起动"
    );

    let used: BTreeSet<&str> = seen.iter().map(|(k, _, _)| k.as_str()).collect();
    let unregistered: Vec<String> = seen
        .iter()
        .filter(|(k, _, _)| !registry.contains(k.as_str()))
        .map(|(k, f, l)| format!("{f}:{l}  \"{k}\""))
        .collect();
    assert!(
        unregistered.is_empty(),
        "这些分组头没在 `state::GROUP_SPECS` 里登记（{} 处）：\n{}\n\n\
         不登记的后果是静默失效：首屏默认折叠不认它、右键「只看这一组」漏掉它、\
         折叠状态也存不进 settings.json。补一行 `in_module(…)` / `in_tab(…)`。",
        unregistered.len(),
        unregistered.join("\n"),
    );

    let orphan: Vec<&str> = registry.difference(&used).copied().collect();
    assert!(
        orphan.is_empty(),
        "登记表里这些键没有任何分组头在用（{:?}）：改名或删组时忘了同步表。\
         它们会一直往 settings.json 里写一个没人读的键。",
        orphan
    );
}

/// 重叠自检：每个模块画一帧，同一行里的文本矩形不许相交。
///
/// 用户的原话是「底部或者其他地方存在字符重叠问题，请自检，不要出现这种明显的
/// 低级错误」。这类缺陷静态扫源码抓不到——两条文案各自都没写错，错的是排布：
/// 一条用了魔法数字的 x，另一条按字符数截断却在窄面板里溢出。所以这里画一帧取
/// 真值：收每个 Text shape 的**可见**矩形（扣掉 clip），按行聚类后比 x 区间。
///
/// 三条口径，都是被误报逼出来的：
///
/// * 同一串文字平移 ≤1.5px 再画一次 = 投影/描边（胶片条序号就是黑影 + 白字），
///   不算重叠；
/// * 只算可见部分——被滚动区裁掉的半截文本按露出的那段比，否则滚动区外的
///   内容会被当成"压在状态栏上"；
/// * 容差 1px：kerning 让相邻字段首尾差零点几像素是正常的。
///
/// Overlap self-check: for every module, draw one frame and assert that text rects on
/// the same row do not intersect. This is the class of bug static source scans cannot
/// see -- each string is fine on its own, the fault is in the layout (a magic-number
/// x in one place, a character-count truncation overflowing a narrow panel in another).
///
/// Three rules, each forced by a false positive: a same-string repeat offset by
/// <=1.5 px is a drop shadow, not an overlap; only the clipped-visible part counts;
/// and 1 px of tolerance absorbs kerning.
#[test]
fn text_never_overlaps_in_any_module() {
    use aevum_ui::state::{AppState, Module};
    // 语言是进程级全局态，i18n 那几个测试会在别的线程里切它：不拿同一把锁，
    // 这一帧可能画着英文、下一帧画中文，宽度一变就排出用户永远看不到的错位。
    // 中英各跑一遍——英文文案更长，本来就更容易挤，只测中文等于放过了半边。
    //
    // The language is process-global and the i18n tests switch it from other threads:
    // without the shared lock a frame can paint English and the next Chinese, and the
    // width change produces a misalignment no user ever sees. Both languages are swept;
    // English runs longer and is the more likely to crowd.
    let _lang_guard = crate::common::i18n_guard();
    let mut hits: Vec<String> = Vec::new();
    for lang in [aevum_ui::i18n::Lang::ZH, aevum_ui::i18n::Lang::EN] {
        aevum_ui::i18n::set(lang);
        let ctx = egui::Context::default();
        aevum_ui::theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        for module in Module::ALL {
            state.module = module;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 1000.0),
                )),
                max_texture_side: Some(8192),
                ..Default::default()
            };
            // 每个模块先画一帧热身再取数据：egui 首帧还没拿到 galley 尺寸，
            // 顶栏这类「按实测宽度排」的段落会在那一帧挤在一起，取首帧等于
            // 判一个用户看不见的暂态。第二帧才是屏幕上稳定的样子。
            //
            // One warm-up frame per module before measuring: egui has no galley sizes on the
            // first frame, so anything laid out from measured widths bunches up there. That
            // transient is never on screen; the second frame is what the user sees.
            // 三帧热身：egui 的面板宽度按内容收敛（切模块后 dock 内容一变，
            // 右侧面板宽度会连着几帧往 372 走），画布右边界也跟着挪。这些帧都是
            // 16ms 级的过渡，用户看不见，判据却会当成错位。
            //
            // Three warm-up frames: egui converges panel widths from content (switching
            // modules changes the dock's content, and its width walks back to 372 over a few
            // frames), so the canvas right edge moves with it. Those are 16 ms transients the
            // user never sees, but the sweep would read them as overlaps.
            for _ in 0..3 {
                let mut warm = ctx.run_ui(input.clone(), |ui| aevum_ui::shell::ui(&mut state, ui));
                warm.textures_delta.clear();
            }
            let mut out = ctx.run_ui(input, |ui| aevum_ui::shell::ui(&mut state, ui));
            let mut rows = Vec::new();
            collect_text_rects(&out.shapes, &mut rows);
            out.textures_delta.clear();
            rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            for i in 0..rows.len() {
                for j in (i + 1)..rows.len() {
                    if (rows[i].0 - rows[j].0).abs() > 3.0 {
                        break;
                    }
                    let overlap = rows[i].2.min(rows[j].2) - rows[i].1.max(rows[j].1);
                    let shadow = rows[i].3 == rows[j].3
                        && (rows[i].1 - rows[j].1).abs() <= 1.5
                        && (rows[i].0 - rows[j].0).abs() <= 1.5;
                    if overlap > 1.0 && !shadow {
                        hits.push(format!(
                            "{:?}/{:?} y={:.1}  「{}」×「{}」  重叠 {:.1}px",
                            lang, module, rows[i].0, rows[i].3, rows[j].3, overlap
                        ));
                    }
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "同一条水平线上有 {} 处文字互相压着（用户会直接骂「低级错误」）：\n{}\n\n         修法看来源：魔法数字定位 → 按实测 galley 宽度排；路径/长文案 → \
         `Label::truncate()` 按可用宽度截；键值两列 → `kv()` 已经按剩余宽度裁，\
         不要绕过它自己 `p.text` 右对齐。",
        hits.len(),
        hits.join("\n"),
    );
}

fn collect_text_rects(
    shapes: &[egui::epaint::ClippedShape],
    out: &mut Vec<(f32, f32, f32, String)>,
) {
    for cs in shapes {
        walk_shape(&cs.shape, cs.clip_rect, out);
    }
}

fn walk_shape(shape: &egui::Shape, clip: egui::Rect, out: &mut Vec<(f32, f32, f32, String)>) {
    match shape {
        egui::Shape::Text(t) => {
            let r = t.galley.rect.translate(t.pos.to_vec2());
            // 被滚动区裁掉的文本不会上屏，不该算进重叠。
            let vis = clip.intersect(r);
            if vis.width() < 2.0 || vis.height() < 2.0 {
                return;
            }
            let txt = t.galley.text().to_owned();
            if !txt.trim().is_empty() {
                // 用可见部分的左右边界比较：一半在裁切线外的文本只按露出的那段算。
                out.push((vis.center().y, vis.left(), vis.right(), txt));
            }
        }
        egui::Shape::Vec(v) => {
            for s in v {
                walk_shape(s, clip, out);
            }
        }
        _ => {}
    }
}

/// 显影面板有一半分区按**显式字段清单**摆推子（`spec_field(Field::…)`，
/// 见 `docs/界面布局-ACR化-2026-11-04.md` §3）：基本分白平衡/影调/偏好三档，
/// 曲线走模式片，混合器两种模式，光学与效果各分两段。
///
/// 清单和规格表是两份手写事实，最容易出的事故是：表里加了一根推子，清单忘了补，
/// 那根推子从此永不上屏——不报错、不告警，只有用户发现「某个参数消失了」。
/// 这条判据把两份对上：凡走显式清单的组，表里每条规格的字段都必须在
/// `adjust.rs` 里出现。
///
/// `detail` 与 `grade` 仍按 `specs_of_group(…)` 整组铺，不在本条范围内。
///
/// Half the Develop sections lay their sliders out from explicit field lists
/// (`spec_field(Field::…)`, plan §3). The list and the spec table are two hand-written
/// facts, and the classic accident is: a slider is added to the table, the list is not
/// updated, and that slider never reaches the screen -- no error, no warning, only a
/// user noticing "a parameter disappeared". This test puts the two side by side.
/// `detail` and `grade` still iterate `specs_of_group(…)` and are out of scope.
#[test]
fn explicit_develop_field_lists_cover_the_spec_table() {
    let controls = read_clean("controls.rs");
    let adjust = read_clean_any("shell/adjust");
    let listed: std::collections::HashSet<String> = adjust
        .match_indices("Field::")
        .map(|(i, _)| {
            adjust[i + "Field::".len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        listed.len() > 20,
        "adjust.rs 里只数到 {} 个 Field::，解析口径变了？",
        listed.len()
    );
    let mut missing = Vec::new();
    let mut checked = 0usize;
    for (variant, group) in spec_fields(&controls) {
        if matches!(
            group.as_str(),
            "basic" | "curve" | "mixer" | "optics" | "effects"
        ) {
            checked += 1;
            if !listed.contains(&variant) {
                missing.push(format!("{group} / Field::{variant}"));
            }
        }
    }
    // 解析器自己也要有牙口：真扫到三十多条才算跑了这一趟，否则「解析出空表 →
    // 没有缺项 → 通过」会变成一条永远绿的假判据。
    assert!(
        checked >= 30,
        "只核对到 {checked} 条规格——表解析口径变了，这条判据等于没跑"
    );
    assert!(
        missing.is_empty(),
        "规格表里有 {} 条参数没被显式清单摆上屏（参数会静默消失）：\n{}\n\n\
         补 `spec_field(ui, state, Field::…)` 进对应的 sub_head 段。",
        missing.len(),
        missing.join("\n"),
    );
}

/// 从 `controls.rs` 的 `SPECS` 表里取每个规格的 (字段变体, 分组)。
///
/// 手写解析：按 `spec!(` 切段，段内第一个「逗号 → 大写标识符 → 逗号」就是字段
/// 变体（前两个参数是两个字符串），最后一个白名单分组串就是分组名。
/// 不引正则依赖，正因为表本身的形状是固定的。
fn spec_fields(src: &str) -> Vec<(String, String)> {
    const GROUPS: [&str; 7] = [
        "basic", "effects", "optics", "mixer", "grade", "detail", "curve",
    ];
    let start = src.find("pub const SPECS").unwrap_or(0);
    let mut out = Vec::new();
    for seg in src[start..].split("spec!(").skip(1) {
        let seg = seg.split("];").next().unwrap_or(seg);
        let Some(variant) = first_type_arg(seg) else {
            continue;
        };
        let mut best: Option<(usize, &str)> = None;
        for g in GROUPS {
            if let Some(p) = seg.rfind(&format!("\"{g}\""))
                && best.is_none_or(|(bp, _)| p > bp)
            {
                best = Some((p, g));
            }
        }
        if let Some((_, g)) = best {
            out.push((variant, g.to_owned()));
        }
    }
    out
}

/// 段内第一个类型参数：逗号后紧跟大写标识符、再以逗号收尾。
fn first_type_arg(seg: &str) -> Option<String> {
    let b = seg.as_bytes();
    let is_ws = |c: u8| (c as char).is_whitespace();
    for i in 0..b.len() {
        if b[i] != b',' {
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && is_ws(b[j]) {
            j += 1;
        }
        if j >= b.len() || !(b[j] as char).is_ascii_uppercase() {
            continue;
        }
        let mut k = j;
        while k < b.len() && (b[k] as char).is_alphanumeric() {
            k += 1;
        }
        let ident = &seg[j..k];
        let mut m = k;
        while m < b.len() && is_ws(b[m]) {
            m += 1;
        }
        if m < b.len() && (b[m] == b',' || b[m] == b'}') && ident.len() > 2 {
            return Some(ident.to_owned());
        }
    }
    None
}

#[test]
fn panel_files_never_hardcode_font_sizes() {
    // 「字体大小不同」（用户截图）：面板里出现 `.size(9.0)` 这类裸数字就是
    // 第二套字阶的入口。字号只许走 theme 的 SIZE_* token（zoom 缩放才能全局生效）。
    // 数字字体（mono 数值读数）同样走 SIZE_*；egui 内部默认（DragValue 无显式字号）
    // 不在此列——那走全局 Style。
    use std::path::Path;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell");
    let mut offenders = Vec::new();
    for entry in walkdir_lite(&root) {
        let src = std::fs::read_to_string(&entry).unwrap_or_default();
        for (i, line) in src.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue;
            }
            // .size( 后面是裸数字（非 SIZE_* / 变量）
            if let Some(k) = t.find(".size(") {
                let rest = &t[k + 6..];
                let starts_digit = rest.chars().next().is_some_and(|c| c.is_ascii_digit());
                if starts_digit {
                    offenders.push(format!(
                        "{}:{}",
                        entry.file_name().unwrap().to_string_lossy(),
                        i + 1
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "面板出现硬编码字号（必须走 SIZE_* token）：{:?}",
        offenders
    );
}

/// 只向下收两层：src/shell/<file>.rs 与 src/shell/<dir>/<file>.rs，够了。
fn walkdir_lite(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&p) {
                for s in sub.flatten() {
                    if s.path().extension().is_some_and(|x| x == "rs") {
                        out.push(s.path());
                    }
                }
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out
}

#[test]
fn look_window_uses_warn_once_for_the_contrast_warning() {
    // 「颜色不统一」的静态防线（用户截图：橙字夹在白灰文字里无章法）：
    // 警示色是语义色不是装饰色——外观窗内 warn() 只许出现在明暗微调的
    // 对比度警告那一行；再想加警示必须先改这条判据说清楚理由。
    let src = read_clean("shell/windows.rs");
    let n = src.matches("warn()").count();
    assert!(
        n <= 1,
        "外观窗出现了 {n} 处 warn()——警示色只许用于明暗微调的对比度警告，多了就是第二套装饰色"
    );
}

/// 画布呼吸带只有一个来源：`theme::CANVAS_GAP` 定义一次，且只在
/// `layout::canvas_insets` 里被加一次。
///
/// 这条挡的是「同一条缝在不同地方各加一遍」：拖宽面板时中央区会少 10px、
/// 画布内部浮层又多让 10px，两条边就对不齐，而这种偏差只在窄窗 + 左坞的组合里
/// 看得出来。`shell/mod.rs` 必须消费那个纯函数，不许自己再算一遍。
///
/// The canvas gutter has one source: `theme::CANVAS_GAP` is defined once and added
/// exactly once, in `layout::canvas_insets`. This blocks the same seam being added twice
/// in different places -- widening the dock would then cost the centre 10 px while the
/// canvas overlays yielded another 10, and the two edges would disagree only in the
/// narrow-window + left-dock combination. `shell/mod.rs` has to consume the pure
/// function instead of recomputing the insets.
#[test]
fn canvas_gap_has_one_source() {
    const NAME: &str = "CANVAS_GAP";
    let mut defs = 0usize;
    let mut uses = Vec::new();
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    assert!(files.len() > 20, "源码目录没扫到东西，路径口径变了");
    for path in &files {
        let raw = std::fs::read_to_string(path).expect("读源码");
        let clean = without_line_comments(&raw);
        // 单元测试里也会引用常量做断言，那不是生产代码的「第二个来源」。
        //
        // Unit tests reference the constant too; those are not a second source in
        // production code.
        let code = clean.split("#[cfg(test)]").next().unwrap_or(&clean);
        for (pos, _) in code.match_indices(NAME) {
            let line = code[..pos].lines().last().unwrap_or("");
            if line.contains("pub const") {
                defs += 1;
            } else {
                uses.push(path.to_string_lossy().into_owned());
            }
        }
    }
    assert_eq!(
        defs, 1,
        "CANVAS_GAP 必须只在 theme.rs 定义一次，实测 {defs} 次"
    );
    assert_eq!(
        uses.len(),
        1,
        "CANVAS_GAP 只许在 canvas_insets 里加一次，实际出现在 {uses:?}"
    );
    assert!(
        uses[0].ends_with("layout.rs"),
        "唯一的使用点应当是 layout.rs，实际是 {}",
        uses[0]
    );
    let shell = read_clean("shell/mod.rs");
    assert!(
        shell.contains("canvas_insets(") && shell.contains("centre_rect("),
        "shell 必须消费 layout 的让位量与中央区计算，不许就地再算"
    );
    assert!(
        !shell.contains("+ 10.0") && !shell.contains("+ CANVAS_GAP"),
        "shell 里出现了第二处 10px，呼吸带不再单源"
    );
}

/// 流程化界面必须真的挂到外壳上：标题栏步骤条 + 左侧面板 + 落盘开关，
/// 少一处就是「按钮点了没反应」。
///
/// The workflow UI must actually be wired into the shell: title-bar step bar, left panel
/// and persisted switches. Missing any one of them reads as "clicking does nothing".
#[test]
fn flow_ui_is_wired_into_the_shell() {
    let shell = read_clean("shell/mod.rs");
    assert!(
        shell.contains("flow_panel::flow_panel_visible")
            && shell.contains("flow_panel::flow_panel("),
        "左侧工作流面板没有挂进 shell 的区域装配"
    );
    // 2026-11-08 用户裁决：标题栏步骤条撤除——单张不该有步骤仪式，延时的
    // 步骤列表住进工作流面板。
    //
    // User decision 2026-11-08: the title-bar step bar is gone; the step list lives in
    // the workflow panel.
    let menu = read_clean("shell/menu.rs");
    assert!(!menu.contains("flow::step_bar("), "标题栏不该再挂步骤条");
    let panel = read_clean("shell/flow_panel.rs");
    assert!(
        panel.contains("flow::step_states")
            && panel.contains("flow::display_step")
            && panel.contains("flow::next_step"),
        "工作流面板没接步骤派生三件套"
    );
    let flow = read_clean("shell/flow.rs");
    assert!(
        !flow.contains("pub fn step_bar"),
        "step_bar 应当随标题栏步骤条一起删除"
    );
    assert!(
        !flow.contains("FlowTask::Develop]"),
        "单张修图不该出现在 ALL 里"
    );
    let theme = read_clean("theme.rs");
    for k in ["STEP_H", "FLOW_W", "FLOW_MIN_W"] {
        assert!(
            theme.contains(&format!("pub const {k}")),
            "流程尺寸 {k} 没进 theme.rs"
        );
    }
    let settings = read_clean("settings.rs");
    for k in ["show_dock", "show_flow", "show_left", "flow_task"] {
        assert!(
            settings.contains(&format!("pub {k}:")),
            "设置项 {k} 没进 settings.json 的形状"
        );
    }
    let session = read_clean("state/ops_session.rs");
    assert!(
        session.contains("layout_switches") && session.contains("self.settings_dirty = true"),
        "帧首的布局开关自动收账没接上"
    );
}

/// 子窗口 / Modal 的尺寸不许再就地写数字：全部走 `theme::WIN_*` / `MODAL_*`
/// 与那几个纯函数，否则 1280×800 下又会顶出视口。
///
/// Window and modal sizes must not be written as local numbers any more: every one goes
/// through `theme::WIN_*` / `MODAL_*` and the pure helpers, or it drifts off a 1280x800
/// viewport again.
#[test]
fn window_sizes_come_from_theme() {
    const CALLS: &[&str] = &[
        ".default_width(",
        ".default_height(",
        ".default_size(",
        ".min_size(",
        ".max_width(",
        ".max_height(",
        ".set_width(",
        ".set_min_width(",
        ".set_max_height(",
    ];
    for rel in ["shell/windows.rs", "shell/palette.rs"] {
        for (i, line) in read_clean(rel).lines().enumerate() {
            if !CALLS.iter().any(|c| line.contains(c)) {
                continue;
            }
            // 按钮自己的行高（`.min_size(Vec2::new(0.0, 26.0))`）不是浮窗尺寸，
            // 不该被窗口判据管；窗口的 min_size 一律走 `theme::win_min(...)`。
            //
            // A button's row height is not a window size; window minimums always come
            // from `theme::win_min(...)`.
            if line.contains(".min_size(Vec2::new(0.0,") {
                continue;
            }
            let code = line.split("//").next().unwrap_or(line);
            let numeric = code
                .as_bytes()
                .windows(3)
                .any(|w| w[0].is_ascii_digit() && w[1] == b'.' && w[2].is_ascii_digit());
            assert!(!numeric, "{rel}:{} 又写死了尺寸：{}", i + 1, code.trim());
        }
    }
    // 四个 Modal 都要过 modal_box，才能统一拿到 62% 高度上限。
    //
    // All four modals must go through modal_box to get the 62% height cap.
    // windows 两处（回滚 / 错误）+ 命令面板一处 + 导出对话框一处 = 四个 Modal 全走同一条。
    //
    // Two in windows (rollback / error) + the palette + the export dialog: all four modals
    // go through the same entry.
    let windows = read_clean("shell/windows.rs");
    assert_eq!(
        windows.matches("modal_box(ui, ").count(),
        2,
        "windows.rs 里应当有两处 Modal 走 modal_box"
    );
    let palette = read_clean("shell/palette.rs");
    assert_eq!(
        palette.matches("windows::modal_box(ui, ").count(),
        1,
        "命令面板也要走 modal_box"
    );
    let dlg = read_clean("shell/export_dialog.rs");
    assert_eq!(
        dlg.matches("modal_box(ui, ").count(),
        1,
        "导出对话框也要走 modal_box"
    );
    let theme = read_clean("theme.rs");
    assert!(
        theme.contains("pub const MODAL_MAX_H_FRAC") && theme.contains("pub fn modal_max_h"),
        "Modal 高度上限没进 theme.rs"
    );
    assert!(
        theme.contains("pub fn win_max") && theme.contains("pub fn win_default"),
        "视口夹取没进 theme.rs"
    );
}

/// 导出确认必须挂在独立的三栏模块里：PS 的导出是「范围 / 格式 / 摘要」并排读的，
/// 塞回 windows.rs 的单列表单就会退回"盲签"。
///
/// The export confirmation must live in its own three-column module: PS reads scope,
/// format and summary side by side, and folding it back into a single-column form in
/// windows.rs would return to signing blind.
#[test]
fn export_dialog_is_the_three_column_mount() {
    let dlg = read_clean("shell/export_dialog.rs");
    for k in [
        "pub fn confirm_export_modal",
        "fn scope_column(",
        "fn format_column(",
        "fn summary_column(",
        "MODAL_W_EXPORT",
    ] {
        assert!(dlg.contains(k), "export_dialog.rs 缺 {k}");
    }
    assert_eq!(
        dlg.matches("column(ui, COL_").count(),
        3,
        "三栏少了一栏，摘要就没了"
    );
    assert!(
        !read_clean("shell/windows.rs").contains("confirm_export_modal"),
        "导出确认还留在 windows.rs，旧的单列表单仍在被使用"
    );
    let shell = read_clean("shell/mod.rs");
    assert!(
        shell.contains("export_dialog::export_confirm_window"),
        "导出确认没挂回帧尾"
    );
    let theme = read_clean("theme.rs");
    assert!(
        theme.contains("pub const MODAL_W_EXPORT"),
        "导出框宽度没进 theme.rs"
    );
}

/// 系统对话框必须带属主窗口：2026-11-08 实测无属主时对话框弹到另一台显示器、
/// 主窗口在对话框打开期间被整个隐藏。裸 `rfd::FileDialog::new()` 只许出现在
/// `win_parent.rs` 的构造器里。
///
/// System dialogs must carry an owner window: measured on 2026-11-08, a parentless
/// dialog landed on another monitor and the main window was hidden while it was open.
/// A bare `rfd::FileDialog::new()` is allowed only inside the `win_parent.rs`
/// constructor.
#[test]
fn every_file_dialog_carries_the_window_owner() {
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    assert!(files.len() > 20, "源码目录没扫到东西，路径口径变了");
    for path in &files {
        let raw = std::fs::read_to_string(path).expect("读源码");
        let clean = without_line_comments(&raw);
        let hits = clean.matches("rfd::FileDialog::new()").count();
        if hits == 0 {
            continue;
        }
        let name = path.to_string_lossy().replace('\\', "/");
        assert!(
            name.ends_with("win_parent.rs"),
            "{name} 里还有 {hits} 处裸 rfd::FileDialog::new()——没有属主，对话框会弹到别的显示器并藏起主窗口"
        );
    }
    let parent = read_clean("win_parent.rs");
    assert!(
        parent.contains("pub fn file_dialog()") && parent.contains("set_parent"),
        "win_parent.rs 没提供带属主的构造器"
    );
    let app = read_clean("app.rs");
    assert!(
        app.contains("win_parent::init("),
        "属主句柄没在启动时初始化"
    );
    // rfd 的四个同步取值方法只许在对话框中枢的工作线程里出现：任何一处跑到
    // `App::ui` 调用栈上，主窗口就会在弹窗期间被藏起来（2026-11-08 实测）。
    //
    // rfd's four blocking getters may appear only inside the hub's worker thread; one
    // call on the `App::ui` stack hides the main window while the dialog is open
    // (measured 2026-11-08).
    for path in &files {
        let raw = std::fs::read_to_string(path).expect("读源码");
        let clean = without_line_comments(&raw);
        let name = path.to_string_lossy().replace('\\', "/");
        for token in [
            ".pick_file()",
            ".pick_files()",
            ".pick_folder()",
            ".save_file()",
        ] {
            let hits = clean.matches(token).count();
            if hits == 0 {
                continue;
            }
            assert!(
                name.ends_with("ops_dialog.rs"),
                "{name} 里出现了 {token}——对话框跑到 UI 线程上了，主窗口会被藏起来"
            );
        }
    }
    let hub = read_clean("state/ops_dialog.rs");
    assert!(
        hub.contains("pub(crate) fn request_dialog(")
            && hub.contains("fn spawn_pending_dialog(")
            && hub.contains("fn drain_dialog_replies("),
        "对话框中枢缺入队 / 起飞 / 收账三件套"
    );
}

/// 顶栏矢量图标不许压到「任务 / Tasks」文字。
///
/// `text_never_overlaps_in_any_module` 只收集 `Shape::Text`，所以齿轮这种
/// `Shape::Path` / `LineSegment` 盖在字上是它的盲区：中文「务」被齿轮打花、英文
/// "Tasks" 的 ks 上骑着齿轮，两个判据都全绿过。这里补上图标侧，取每个图标
/// **叶子线段/路径**的可见包围盒（不取 `Shape::Vec` 的并集，否则一个大包围盒会
/// 把整条顶栏都算成图标），与任务胶囊文字做同排水平相交判定。
///
/// Top-bar vector icons must not cover the "任务 / Tasks" label. The existing text-vs-text
/// check only collects `Shape::Text`, so a gear drawn as paths over the glyphs slipped
/// through in both languages. Collect each icon's leaf segment/path bounding box instead
/// (not the `Shape::Vec` union, which would swallow the whole bar) and compare on the
/// same baseline.
#[test]
fn top_bar_icons_never_cover_the_tasks_label() {
    use aevum_ui::state::AppState;
    let _lang_guard = crate::common::i18n_guard();
    let mut hits: Vec<String> = Vec::new();
    for lang in [aevum_ui::i18n::Lang::ZH, aevum_ui::i18n::Lang::EN] {
        aevum_ui::i18n::set(lang);
        let ctx = egui::Context::default();
        aevum_ui::theme::apply(&ctx, 1.0);
        let mut state = AppState::demo();
        state.install_demo_textures(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1000.0),
            )),
            max_texture_side: Some(8192),
            ..Default::default()
        };
        // 与文字重叠判据同一套热身：首帧 galley 宽度未收敛，排布是暂态。
        for _ in 0..3 {
            let mut warm = ctx.run_ui(input.clone(), |ui| aevum_ui::shell::ui(&mut state, ui));
            warm.textures_delta.clear();
        }
        let mut out = ctx.run_ui(input, |ui| aevum_ui::shell::ui(&mut state, ui));
        let mut texts = Vec::new();
        collect_text_rects(&out.shapes, &mut texts);
        let mut icons = Vec::new();
        for cs in &out.shapes {
            walk_icon_shapes(&cs.shape, &mut icons);
        }
        out.textures_delta.clear();
        let tasks: Vec<_> = texts
            .iter()
            .filter(|t| t.0 < 60.0 && matches!(t.3.trim(), "任务" | "Tasks"))
            .collect();
        assert!(
            !tasks.is_empty(),
            "{lang:?} 顶栏没画任务胶囊文字，判据取样失效"
        );
        for t in &tasks {
            for r in &icons {
                if (r.center().y - t.0).abs() > 20.0 {
                    continue;
                }
                let overlap = t.2.min(r.max.x) - t.1.max(r.min.x);
                if overlap > 1.0 {
                    hits.push(format!(
                        "{lang:?} 「{}」被图标包围盒 {:.0?} 压住 {:.1}px",
                        t.3, r, overlap
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "顶栏图标压住了任务胶囊文字（齿轮必须锚在胶囊左缘之外）：\n{}",
        hits.join("\n")
    );
}

/// 只收图标的叶子线段 / 路径：背景块、下划线、滚动条都是 `Shape::Rect`，不进这里。
fn walk_icon_shapes(shape: &egui::Shape, out: &mut Vec<egui::Rect>) {
    match shape {
        egui::Shape::Vec(v) => {
            for s in v {
                walk_icon_shapes(s, out);
            }
        }
        egui::Shape::LineSegment { .. } | egui::Shape::Path(_) => {
            let r = shape.visual_bounding_rect();
            if r.width() <= 32.0 && r.height() <= 32.0 && r.center().y < 60.0 {
                out.push(r);
            }
        }
        _ => {}
    }
}
