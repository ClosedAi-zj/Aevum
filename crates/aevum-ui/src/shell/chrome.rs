//! 窗口镶边：拖拽缩放区、三枚窗口按钮（mac 红绿灯 / Win 几何）。

// 拆分自 shell.rs 的完整 use 树（先全量继承，unused 项按 test 目标警告剪除）。
use crate::platform;
use crate::theme::*;
use egui::{Color32, Context, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};
// 拆分自 shell.rs 的 use 树：本模块先全量继承，unused 项按编译警告逐个清。

const RESIZE_ZONE: f32 = 5.0;

// ---- 窗口按钮（Windows 式排布 + 本项目材质的悬停形状）---------------------
// 无边框自绘 chrome 的交互契约不变：还是三枚可点的窗口按钮，贴右上角。
// 形态跟 Windows——按钮铺满菜单行高、命中区是竖长条、顺序是 最小化/最大化/关闭，
// 字形是横线、方框、叉。个性化在于不画 Win 那种硬边方块底：悬停是圆角胶囊，
// 只有关闭上 danger 红底配白叉，其余色值全部走 theme 的 token。

/// 单枚按钮宽度。
const WINBTN_W: f32 = 42.0;

/// 三枚按钮的总宽，菜单行右侧要给它们让出来的避让区。
const WINBTN_ZONE_W: f32 = WINBTN_W * 3.0;

/// 悬停胶囊相对按钮内缩多少：留缝，三枚不会糊成一整块。
const WINBTN_INSET: f32 = 4.0;

/// 字形半径（横线半宽 / 方框半宽）：10px 见方，压得住 36px 的行高。
const WINBTN_R: f32 = 5.0;

/// 字形笔画宽度。
const WINBTN_STROKE: f32 = 1.2;

/// 关闭悬停的红：Windows 的关闭红压深一档，白叉对比才有 5.6:1。
const WIN_CLOSE_HOT: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);

/// 无边框窗口的缩放热区；最大化时交给系统。
pub(super) fn window_resize(ui: &mut Ui, ctx: &Context, full: Rect) {
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let z = RESIZE_ZONE;
    let c = 12.0;
    let (l, t, r, b) = (full.left(), full.top(), full.right(), full.bottom());
    let handles = [
        (
            Rect::from_min_max(Pos2::new(l, t), Pos2::new(l + c, t + c)),
            egui::ResizeDirection::NorthWest,
            CursorIcon::ResizeNwSe,
            "nw",
        ),
        (
            Rect::from_min_max(Pos2::new(r - c, t), Pos2::new(r, t + c)),
            egui::ResizeDirection::NorthEast,
            CursorIcon::ResizeNeSw,
            "ne",
        ),
        (
            Rect::from_min_max(Pos2::new(l, b - c), Pos2::new(l + c, b)),
            egui::ResizeDirection::SouthWest,
            CursorIcon::ResizeNeSw,
            "sw",
        ),
        (
            Rect::from_min_max(Pos2::new(r - c, b - c), Pos2::new(r, b)),
            egui::ResizeDirection::SouthEast,
            CursorIcon::ResizeNwSe,
            "se",
        ),
        (
            Rect::from_min_max(Pos2::new(l + c, t), Pos2::new(r - c, t + z)),
            egui::ResizeDirection::North,
            CursorIcon::ResizeVertical,
            "n",
        ),
        (
            Rect::from_min_max(Pos2::new(l + c, b - z), Pos2::new(r - c, b)),
            egui::ResizeDirection::South,
            CursorIcon::ResizeVertical,
            "s",
        ),
        (
            Rect::from_min_max(Pos2::new(l, t + c), Pos2::new(l + z, b - c)),
            egui::ResizeDirection::West,
            CursorIcon::ResizeHorizontal,
            "w",
        ),
        (
            Rect::from_min_max(Pos2::new(r - z, t + c), Pos2::new(r, b - c)),
            egui::ResizeDirection::East,
            CursorIcon::ResizeHorizontal,
            "e",
        ),
    ];
    for (rect, dir, icon, id) in handles {
        let resp = ui.interact(rect, ui.make_persistent_id(("resize", id)), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ctx.set_cursor_icon(icon);
        }
        if resp.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        }
    }
}

/// 无边框窗口的三枚按钮：最小化 / 缩放（最大化或还原）/ 关闭，贴右上角。
///
/// 排布与命中区跟 Windows：竖长条铺满菜单行，最大化时字形换成叠框，点下去就是
/// 那个动作。个性化只落在形状与配色上——悬停画圆角胶囊、关闭才上红底，
/// 字形一律按物理像素对齐，1.2px 的线不许发灰。
pub(super) fn window_buttons(ui: &mut Ui, ctx: &Context, full: Rect) {
    let c = platform::chrome();
    if c.traffic_lights {
        mac_lights(ui, ctx, full);
    } else {
        win_buttons(ui, ctx, full);
    }
}

/// macOS 的三枚交通灯：关闭 / 最小化 / 缩放，直径 12、间距 8、贴左上。
///
/// 不聚焦时三个圆点一起褪成中性灰 —— 那是 macOS 的原生行为，也是"这个窗口
/// 现在不接收键盘"的唯一视觉信号，不该省掉。
fn mac_lights(ui: &mut Ui, ctx: &Context, full: Rect) {
    let p = ui.painter().clone();
    let focused = platform::focused(ctx);
    let d = platform::Chrome::LIGHT_D;
    let r = d * 0.5;
    let y = full.center().y;
    let x0 = full.left() + platform::Chrome::LIGHT_X + r;
    // 系统语义色，不跟主题走：这三个色在任何壁纸上都要认得出是交通灯。
    const CLOSE: Color32 = Color32::from_rgb(0xFF, 0x5F, 0x57);
    const MIN: Color32 = Color32::from_rgb(0xFE, 0xBC, 0x2E);
    const ZOOM: Color32 = Color32::from_rgb(0x28, 0xC8, 0x40);
    const IDLE: Color32 = Color32::from_rgb(0x4A, 0x4A, 0x4E);
    let colors = [CLOSE, MIN, ZOOM];
    let glyphs = ['×', '−', '+'];
    // 悬停提示在绑定处就翻：这三条是数组里的活字面量，没有调用点可翻。
    let tips = [
        crate::i18n::tr("关闭"),
        crate::i18n::tr("最小化"),
        crate::i18n::tr("缩放"),
    ];
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let step = d + platform::Chrome::LIGHT_GAP;
    for i in 0..3 {
        let center = Pos2::new(x0 + i as f32 * step, y);
        let hit = Rect::from_center_size(center, Vec2::splat(d + 6.0));
        let resp = ui.interact(hit, ui.make_persistent_id(("light", i)), Sense::click());
        let fill = if focused { colors[i] } else { IDLE };
        p.circle_filled(center, r, fill);
        // 悬停才显字形：平时是纯色圆点，鼠标压上去才读得出这一枚是什么。
        if resp.hovered() && focused {
            let ink = Color32::from_black_alpha(150);
            let g = p.layout_no_wrap(glyphs[i].to_string(), egui::FontId::proportional(10.0), ink);
            p.galley(
                Pos2::new(center.x - g.size().x * 0.5, center.y - g.size().y * 0.5),
                g,
                ink,
            );
        }
        let clicked = resp.clicked();
        resp.on_hover_text(tips[i]);
        if clicked {
            match i {
                0 => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                1 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                _ => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
            }
        }
    }
}

/// Windows 的三枚窗口按钮：最小化 / 最大化或还原 / 关闭，贴右上、铺满行高。
fn win_buttons(ui: &mut Ui, ctx: &Context, full: Rect) {
    let p = ui.painter().clone();
    let focused = platform::focused(ctx);
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let ppp = ctx.pixels_per_point().max(1.0);
    let snap = |v: f32| (v * ppp).round() / ppp;
    let zone = full.right() - WINBTN_ZONE_W;
    for i in 0..3 {
        let hit = Rect::from_min_max(
            Pos2::new(zone + i as f32 * WINBTN_W, full.top()),
            Pos2::new(zone + (i as f32 + 1.0) * WINBTN_W, full.bottom()),
        );
        let resp = ui.interact(hit, ui.make_persistent_id(("win-btn", i)), Sense::click());
        let pressed = resp.is_pointer_button_down_on();
        let close = i == 2;
        if resp.hovered() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
            // 关闭是红底白叉，其余两枚跟着面板的悬停层级走。
            let fill = if close {
                WIN_CLOSE_HOT
            } else if pressed {
                elev_hover()
            } else {
                elev()
            };
            p.rect_filled(hit.shrink(WINBTN_INSET), radius_control(), fill);
        }
        let color = if !focused {
            dim()
        } else if close || pressed {
            Color32::WHITE
        } else {
            text()
        };
        let c = Pos2::new(snap(hit.center().x), snap(hit.center().y));
        let st = Stroke::new(WINBTN_STROKE, color);
        let r = WINBTN_R;
        match i {
            0 => {
                p.line_segment([Pos2::new(c.x - r, c.y), Pos2::new(c.x + r, c.y)], st);
            }
            1 if !maximized => {
                p.rect_stroke(
                    Rect::from_center_size(c, Vec2::splat(r * 2.0)),
                    1,
                    st,
                    StrokeKind::Inside,
                );
            }
            1 => {
                // 还原态：前一枚方框，后边只露上边和右边两条折角，
                // 不靠填充遮挡，压在什么底色上都是这两个形状。
                let front = Rect::from_center_size(
                    Pos2::new(c.x - 1.5, c.y + 1.5),
                    Vec2::splat(r * 2.0 - 2.0),
                );
                p.rect_stroke(front, 1, st, StrokeKind::Inside);
                let back = front.translate(Vec2::new(3.0, -3.0));
                p.line_segment([back.left_top(), back.right_top()], st);
                p.line_segment([back.right_top(), back.right_bottom()], st);
            }
            _ => {
                p.line_segment(
                    [Pos2::new(c.x - r, c.y - r), Pos2::new(c.x + r, c.y + r)],
                    st,
                );
                p.line_segment(
                    [Pos2::new(c.x + r, c.y - r), Pos2::new(c.x - r, c.y + r)],
                    st,
                );
            }
        }
        let tip = crate::i18n::tr(match i {
            0 => "最小化",
            1 if maximized => "还原窗口大小",
            1 => "最大化窗口",
            _ => "关闭",
        });
        // on_hover_text 会接管 Response，所以点击判定在它之前问完。
        let clicked = resp.clicked();
        resp.on_hover_text(tip);
        if clicked {
            match i {
                0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                1 => ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized)),
                _ => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }
}

#[cfg(test)]
mod chrome_tests {
    use super::*;
    use crate::shell::ui;
    use crate::state::AppState;
    use crate::theme;
    use egui::PointerButton;

    /// 菜单行右侧第 `i` 枚窗口按钮的中心（0=最小化，1=缩放，2=关闭）。
    fn win_btn_center(i: usize) -> Pos2 {
        let zone = 1600.0 - WINBTN_ZONE_W;
        Pos2::new(zone + (i as f32 + 0.5) * WINBTN_W, MENU_H * 0.5)
    }

    fn frame(pos: Pos2, pressed: bool, maximized: bool) -> egui::RawInput {
        let events = if pressed {
            vec![egui::Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }]
        } else {
            vec![egui::Event::PointerMoved(pos)]
        };
        frame_with(events, pos, maximized)
    }

    /// 抬起那一帧必须真是 `pressed: false` 的按钮事件，只发 PointerMoved 的话
    /// egui 的点击状态机停在"按住"，命令永远不发。
    fn frame_release(pos: Pos2, maximized: bool) -> egui::RawInput {
        frame_with(
            vec![egui::Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
            pos,
            maximized,
        )
    }

    fn frame_with(events: Vec<egui::Event>, _pos: Pos2, maximized: bool) -> egui::RawInput {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0))),
            max_texture_side: Some(8192),
            events,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .expect("默认输入带着根视口")
            .maximized = Some(maximized);
        input
    }

    /// 悬停 → 按下 → 抬起，把抬起那一帧根视口收到的窗口命令带回来。
    fn click_win_btn(i: usize, maximized: bool) -> Vec<egui::ViewportCommand> {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::new();
        let pos = win_btn_center(i);
        let inputs = [
            frame(pos, false, maximized),
            frame(pos, true, maximized),
            frame_release(pos, maximized),
        ];
        let mut commands = Vec::new();
        for input in inputs {
            let mut out = ctx.run_ui(input, |root| ui(&mut state, root));
            // 字形纹理的增量由宿主负责，测试里不接就得手动清掉，否则 Drop 时 panic。
            out.textures_delta.clear();
            // 只有一帧的根视口，取 values 而不是按 ROOT 查：ROOT 的 Id 拼写不是这里的重点。
            commands = out
                .viewport_output
                .values()
                .flat_map(|v| v.commands.clone())
                .collect();
        }
        commands
    }

    fn has(cmds: &[egui::ViewportCommand], want: &egui::ViewportCommand) -> bool {
        cmds.iter().any(|c| c == want)
    }

    /// 无边框外壳的命脉：三枚按钮必须各自发对命令，最大化时还要反过来。
    #[test]
    fn window_buttons_emit_their_viewport_commands() {
        let cmds = click_win_btn(0, false);
        assert!(
            has(&cmds, &egui::ViewportCommand::Minimized(true)),
            "最小化按钮没发命令：{cmds:?}"
        );
        let cmds = click_win_btn(1, false);
        assert!(
            has(&cmds, &egui::ViewportCommand::Maximized(true)),
            "最大化按钮没发命令：{cmds:?}"
        );
        let cmds = click_win_btn(1, true);
        assert!(
            has(&cmds, &egui::ViewportCommand::Maximized(false)),
            "已最大化时第二枚要还原而不是再最大化：{cmds:?}"
        );
        let cmds = click_win_btn(2, false);
        assert!(
            has(&cmds, &egui::ViewportCommand::Close),
            "关闭按钮没发命令：{cmds:?}"
        );
    }

    /// 悬停胶囊是这套 chrome 唯一的可点提示，画不出来就等于没有反馈。
    #[test]
    fn hovering_a_window_button_paints_its_pill() {
        let ctx = Context::default();
        theme::apply(&ctx, 1.0);
        let mut state = AppState::new();
        let mut cold = ctx.run_ui(
            frame(Pos2::new(700.0, MENU_H * 0.5), false, false),
            |root| ui(&mut state, root),
        );
        let cold_shapes = cold.shapes.len();
        cold.textures_delta.clear();
        let mut hot = ctx.run_ui(frame(win_btn_center(2), false, false), |root| {
            ui(&mut state, root)
        });
        let hot_shapes = hot.shapes.len();
        hot.textures_delta.clear();
        assert!(
            hot_shapes > cold_shapes,
            "关闭按钮悬停没多画出胶囊：{cold_shapes} → {hot_shapes}"
        );
    }
}
