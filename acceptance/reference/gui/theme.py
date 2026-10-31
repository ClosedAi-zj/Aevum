"""可换皮肤的主题系统：token 表 → 扁平 QSS + QPalette + 原生标题栏与窗口弧线。

层次约定（从底到顶，越靠后的操作面越亮一档）：
    FIELD  画布与输入框   →  BG   窗口与内容面  →  BG2  栏与卡片  →  BG3  控件

扁平化：全界面没有渐变和阴影，控件不靠描边分层，只有输入框留 1px 描边
（它们的 FIELD 底色比窗口还深，去掉描边在浅色皮肤上和背景糊成一片）；
圆角统一走 R_SM/R_MD/R_LG/R_XL 四档，需要药丸形时用 R_PILL。

皮肤只改数值不改结构：所有界面代码读的是本模块的同名常量
（theme.ACCENT、theme.TEXT_DIM…），切换皮肤时这些常量会被重写，
正在绘制的控件下一次 paintEvent 就自动换色，不需要重建窗口。
唯一不能自动跟的是已经栅格化进 QPixmap 的图标颜色，所以 icons.py
里用 bind() 登记"控件-图标-颜色角色"，切皮肤时由 refresh_icons() 重画。

新增皮肤必须补全整套 token（缺一个就在 _build_qss 里 KeyError），
并且过 theme.audit()：每套皮肤逐对算 WCAG 对比度，正文 4.5、
次级与强调色文字 3.0、按钮上的字 4.5，不达标的 (皮肤, 前景/底色, 比值)
会被列出来，tests/test_theme.py 守住这条。

字体是随包分发的开源字体（assets/fonts）：Inter 管西文与数字，
Noto Sans SC 管中文；QApplication 按家族回退，QSS 里不再写 font-family。
"""
import os
import sys

from PySide6 import QtCore, QtGui, QtWidgets

# ------------------------------------------------------------------ 皮肤表
# 每套皮肤给全同一批 token，缺一个就会在 _build 时报 KeyError 而不是静默出丑。
_SKIN_BASE = {
    "深空": dict(
        label="深空 · zinc + indigo", dark=True,
        BG="#09090b", BG2="#0e0e11", BG3="#18181b", FIELD="#060607",
        HOVER="#222227", PRESSED="#141417",
        BORDER="#27272a", BORDER_HI="#3f3f46", SEP="#1c1c1f",
        TEXT="#fafafa", TEXT_SOFT="#d4d4d8", TEXT_DIM="#b8b8c1",
        TEXT_OFF="#8b8b95",
        ACCENT="#4f46e5", ACCENT_HI="#818cf8", ACCENT_DIM="#4338ca",
        ACCENT_SOFT="#1e1b4b", ON="#eef2ff",
        ON_ACCENT="#ffffff", ON_ACCENT_HI="#1e1b4b",
        OK="#34d399", BAD="#fb7185", HANDLE="#fafafa",
        SOFT="rgba(79, 70, 229, 0.16)", SOFT_HI="rgba(79, 70, 229, 0.26)",
        WASH="rgba(255, 255, 255, 0.06)",
        GRID=(255, 255, 255, 18), HIST_L=(226, 226, 230, 175),
        VEC=(235, 200, 120), PILL=(9, 9, 11, 205), CANVAS="#060607",
    ),
    "石墨": dict(
        label="石墨 · 中性 + 青", dark=True,
        BG="#0c0d0f", BG2="#121417", BG3="#1b1e23", FIELD="#08090a",
        HOVER="#252a31", PRESSED="#171a1f",
        BORDER="#2a2f37", BORDER_HI="#3d444e", SEP="#1a1d22",
        TEXT="#f3f5f7", TEXT_SOFT="#d3d8de", TEXT_DIM="#b3bac4",
        TEXT_OFF="#868e99",
        ACCENT="#38bdf8", ACCENT_HI="#7dd3fc", ACCENT_DIM="#0ea5e9",
        ACCENT_SOFT="#082f49", ON="#e0f2fe",
        ON_ACCENT="#052733", ON_ACCENT_HI="#052733",
        OK="#34d399", BAD="#fb7185", HANDLE="#f3f5f7",
        SOFT="rgba(56, 189, 248, 0.16)", SOFT_HI="rgba(56, 189, 248, 0.26)",
        WASH="rgba(255, 255, 255, 0.06)",
        GRID=(255, 255, 255, 18), HIST_L=(226, 228, 232, 175),
        VEC=(235, 210, 140), PILL=(8, 9, 10, 205), CANVAS="#08090a",
    ),
    "午夜": dict(
        label="午夜 · 蓝黑 + 琥珀", dark=True,
        BG="#0b1020", BG2="#0f1529", BG3="#171f38", FIELD="#070b18",
        HOVER="#1f2846", PRESSED="#141b31",
        BORDER="#25304f", BORDER_HI="#374465", SEP="#141a2c",
        TEXT="#eef2ff", TEXT_SOFT="#cbd5e1", TEXT_DIM="#a9b4c9",
        TEXT_OFF="#7f8aa3",
        ACCENT="#f59e0b", ACCENT_HI="#fbbf24", ACCENT_DIM="#d97706",
        ACCENT_SOFT="#3a2a08", ON="#fef3c7",
        ON_ACCENT="#1a1204", ON_ACCENT_HI="#1a1204",
        OK="#34d399", BAD="#fb7185", HANDLE="#fff7ed",
        SOFT="rgba(245, 158, 11, 0.16)", SOFT_HI="rgba(245, 158, 11, 0.26)",
        WASH="rgba(255, 255, 255, 0.06)",
        GRID=(255, 255, 255, 20), HIST_L=(226, 228, 240, 175),
        VEC=(240, 205, 130), PILL=(7, 11, 24, 205), CANVAS="#070b18",
    ),
    "青瓷": dict(
        label="青瓷 · 松墨 + 瓷青", dark=True,
        BG="#0a0f0e", BG2="#101716", BG3="#17201e", FIELD="#070b0a",
        HOVER="#202b29", PRESSED="#131a19",
        BORDER="#2a3735", BORDER_HI="#3f514e", SEP="#161d1c",
        TEXT="#eef4f2", TEXT_SOFT="#c9d6d3", TEXT_DIM="#a8b8b5",
        TEXT_OFF="#869895",
        ACCENT="#2dd4bf", ACCENT_HI="#5eead4", ACCENT_DIM="#0d9488",
        ACCENT_SOFT="#0b3a35", ON="#d9fbf3",
        ON_ACCENT="#04211f", ON_ACCENT_HI="#04211f",
        OK="#4ade80", BAD="#fb7185", HANDLE="#eef4f2",
        SOFT="rgba(45, 212, 191, 0.16)", SOFT_HI="rgba(45, 212, 191, 0.26)",
        WASH="rgba(255, 255, 255, 0.06)",
        GRID=(255, 255, 255, 18), HIST_L=(224, 236, 232, 175),
        VEC=(200, 235, 220), PILL=(7, 11, 10, 205), CANVAS="#070b0a",
    ),
    "藤紫": dict(
        label="藤紫 · 暗紫 + 兰", dark=True,
        BG="#14101a", BG2="#1a1523", BG3="#241d30", FIELD="#0f0c14",
        HOVER="#2f2640", PRESSED="#231c2f",
        BORDER="#3a3049", BORDER_HI="#504363", SEP="#1e1826",
        TEXT="#f4f1f8", TEXT_SOFT="#d8d2e2", TEXT_DIM="#b8b0c6",
        TEXT_OFF="#948ba6",
        ACCENT="#c084fc", ACCENT_HI="#d8b4fe", ACCENT_DIM="#a855f7",
        ACCENT_SOFT="#3b1d54", ON="#f3e8ff",
        ON_ACCENT="#22072f", ON_ACCENT_HI="#22072f",
        OK="#34d399", BAD="#fb7185", HANDLE="#f8f5fb",
        SOFT="rgba(192, 132, 252, 0.16)", SOFT_HI="rgba(192, 132, 252, 0.26)",
        WASH="rgba(255, 255, 255, 0.06)",
        GRID=(255, 255, 255, 18), HIST_L=(232, 228, 240, 175),
        VEC=(225, 200, 240), PILL=(15, 12, 20, 205), CANVAS="#0f0c14",
    ),
    "影棚": dict(
        label="影棚 · 中性灰 + 雾蓝（调色推荐）", dark=True,
        BG="#24262a", BG2="#2b2e33", BG3="#363a40", FIELD="#1e2023",
        HOVER="#41454c", PRESSED="#303338",
        BORDER="#4c525a", BORDER_HI="#626973", SEP="#303338",
        TEXT="#f2f3f5", TEXT_SOFT="#d6d9de", TEXT_DIM="#b6bbc3",
        TEXT_OFF="#969ca5",
        ACCENT="#6ea8fe", ACCENT_HI="#9ec5ff", ACCENT_DIM="#4888e0",
        ACCENT_SOFT="#1e3252", ON="#e8f1ff",
        ON_ACCENT="#06172c", ON_ACCENT_HI="#06172c",
        OK="#5dd39e", BAD="#ff8f8f", HANDLE="#f2f3f5",
        SOFT="rgba(110, 168, 254, 0.18)", SOFT_HI="rgba(110, 168, 254, 0.30)",
        WASH="rgba(255, 255, 255, 0.08)",
        GRID=(255, 255, 255, 22), HIST_L=(236, 238, 242, 180),
        VEC=(205, 225, 250), PILL=(30, 32, 35, 210), CANVAS="#141619",
    ),
    "高对比": dict(
        label="高对比 · 纯黑 + 黄", dark=True,
        BG="#000000", BG2="#0b0b0b", BG3="#1b1b1b", FIELD="#000000",
        HOVER="#303030", PRESSED="#101010",
        BORDER="#6f6f6f", BORDER_HI="#a3a3a3", SEP="#3d3d3d",
        TEXT="#ffffff", TEXT_SOFT="#f2f2f2", TEXT_DIM="#d2d2d2",
        TEXT_OFF="#adadad",
        ACCENT="#ffd60a", ACCENT_HI="#ffe45c", ACCENT_DIM="#c9a200",
        ACCENT_SOFT="#3a2f00", ON="#fff8dc",
        ON_ACCENT="#000000", ON_ACCENT_HI="#000000",
        OK="#00e676", BAD="#ff6b6b", HANDLE="#ffffff",
        SOFT="rgba(255, 214, 10, 0.22)", SOFT_HI="rgba(255, 214, 10, 0.34)",
        WASH="rgba(255, 255, 255, 0.14)",
        GRID=(255, 255, 255, 44), HIST_L=(255, 255, 255, 215),
        VEC=(255, 214, 10), PILL=(0, 0, 0, 240), CANVAS="#0a0a0a",
    ),
    "晨白": dict(
        label="晨白 · 浅色", dark=False,
        BG="#f6f6f7", BG2="#eeeff1", BG3="#ffffff", FIELD="#ffffff",
        HOVER="#e6e6ea", PRESSED="#dbdbe1",
        BORDER="#dedee3", BORDER_HI="#c2c2cb", SEP="#e7e7ec",
        TEXT="#17171b", TEXT_SOFT="#3b3b43", TEXT_DIM="#55555f",
        TEXT_OFF="#75757e",
        ACCENT="#4f46e5", ACCENT_HI="#4338ca", ACCENT_DIM="#3730a3",
        ACCENT_SOFT="#e4e2ff", ON="#241f5c",
        ON_ACCENT="#ffffff", ON_ACCENT_HI="#ffffff",
        OK="#059669", BAD="#e11d48", HANDLE="#ffffff",
        SOFT="rgba(79, 70, 229, 0.12)", SOFT_HI="rgba(79, 70, 229, 0.20)",
        WASH="rgba(0, 0, 0, 0.05)",
        GRID=(0, 0, 0, 24), HIST_L=(60, 60, 70, 175),
        VEC=(120, 90, 20), PILL=(255, 255, 255, 215), CANVAS="#c9c9d1",
    ),
    "暖沙": dict(
        label="暖沙 · 米纸 + 赭", dark=False,
        BG="#faf7f2", BG2="#f2ece3", BG3="#ffffff", FIELD="#ffffff",
        HOVER="#e9e1d5", PRESSED="#ded5c6",
        BORDER="#d9d0c1", BORDER_HI="#bcb0a0", SEP="#ebe4d9",
        TEXT="#241f19", TEXT_SOFT="#4a4239", TEXT_DIM="#665c50",
        TEXT_OFF="#82786a",
        ACCENT="#b45309", ACCENT_HI="#92400e", ACCENT_DIM="#7c2d12",
        ACCENT_SOFT="#f6e6d2", ON="#2b1806",
        ON_ACCENT="#ffffff", ON_ACCENT_HI="#ffffff",
        OK="#15803d", BAD="#be123c", HANDLE="#ffffff",
        SOFT="rgba(180, 83, 9, 0.14)", SOFT_HI="rgba(180, 83, 9, 0.22)",
        WASH="rgba(0, 0, 0, 0.05)",
        GRID=(0, 0, 0, 26), HIST_L=(58, 50, 40, 175),
        VEC=(140, 96, 24), PILL=(255, 255, 255, 220), CANVAS="#d6d0c8",
    ),
}

SKINS = tuple(_SKIN_BASE)
DEFAULT_SKIN = "深空"

# 扁平化的形状尺度：只有这四档半径，控件靠色面分层而不是描边+阴影。
# R_PILL 交给 QSS 自己夹到高度一半，得到真药丸。
R_SM, R_MD, R_LG, R_XL = 6, 8, 10, 12
R_PILL = 999

# 窗口弧线：DWMWA_WINDOW_CORNER_PREFERENCE，只有 Win11 认，老系统设了会返回失败。
# -1 表示不干预，交给系统默认。
CORNER_MODES = (("跟随系统", -1), ("大圆角", 2), ("小圆角", 3), ("直角", 1))
CORNER = 2
CORNER_HR = None        # 最近一次设置圆角的 HRESULT（0=成功，非 0=系统不认）

# 字号四档：界面里每一个字号都要过 fs()，档位只改这一个乘数。
# 全用 px 不用 pt——pt 会随系统 DPI 二次换算，而 QSS 里清一色是 px，
# 混着写就会出现"标题还没正文大"的怪比例。
FONT_LEVELS = (("紧凑", 0.88), ("标准", 1.0), ("宽松", 1.12), ("特大", 1.30))
DEFAULT_FONT = "标准"
FONT = DEFAULT_FONT
FONT_SCALE = 1.0
FONT_BASE = 15            # 正文基准，和 QSS 里 `*` 那条同源


def fs(px):
    """基准 px → 当前档位的 px；下限 10，再小就只是看着像字。"""
    return max(10, int(round(px * FONT_SCALE)))

# 图标颜色角色：控件只登记角色，具体色值切皮肤时再取
ICON_ROLES = {
    "dim": "TEXT_DIM", "soft": "TEXT_SOFT", "text": "TEXT", "off": "TEXT_OFF",
    "accent": "ACCENT", "accent_hi": "ACCENT_HI", "on": "ON",
    "border": "BORDER_HI", "ok": "OK", "bad": "BAD",
}

_token_names = tuple(k for k in _SKIN_BASE[DEFAULT_SKIN] if k != "label")


# ------------------------------------------------------------ 文字/底色兼容性
# WCAG 相对亮度比值。正文与按钮上的字要求 4.5（AA 正常字号），
# 次级说明、页签、强调色数值这些"扫读"用途要求 3.0（AA 大字/图形档）。
_CONTRAST_RULES = (
    ("TEXT", ("BG", "BG2", "BG3", "FIELD"), 4.5),
    ("TEXT_SOFT", ("BG", "BG2", "BG3"), 4.5),
    ("TEXT_DIM", ("BG", "BG2", "BG3"), 3.0),
    ("TEXT_OFF", ("BG", "BG2", "BG3"), 3.0),
    ("ACCENT_HI", ("BG", "BG2", "BG3"), 3.0),
    ("OK", ("BG", "BG2", "BG3"), 3.0),
    ("BAD", ("BG", "BG2", "BG3"), 3.0),
    ("ON_ACCENT", ("ACCENT", "ACCENT_DIM"), 4.5),
    ("ON_ACCENT_HI", ("ACCENT_HI",), 4.5),
)
# 选中态是"强调色洗一层"叠在面板上，前景写在合成后的底色上。
# PILL 是浮层（拖放遮罩、胶片角标）的底色，同样叠在面板上算。
_CONTRAST_WASHES = (("ON", "SOFT"), ("ON", "SOFT_HI"), ("ON", "PILL"))


def _rgba(spec):
    """'#rrggbb' / 'rgba(r, g, b, a)' / (r, g, b) → (r, g, b, 0..1)。"""
    if isinstance(spec, (tuple, list)):
        v = [int(x) for x in spec]
        return (v[0], v[1], v[2], v[3] / 255.0 if len(v) > 3 else 1.0)
    s = str(spec).strip()
    if s.startswith("rgba"):
        n = s[s.index("(") + 1:s.index(")")].split(",")
        return (int(float(n[0])), int(float(n[1])), int(float(n[2])),
                float(n[3]))
    if s.startswith("#"):
        h = s[1:]
        if len(h) == 3:
            h = "".join(c * 2 for c in h)
        return (int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16), 1.0)
    raise ValueError(f"看不懂的颜色：{spec!r}")


def mix(fg, bg):
    """把带 alpha 的 fg 叠到 bg 上（QSS 就是这个语义），返回不透明 (r, g, b)。"""
    r1, g1, b1, a = _rgba(fg)
    r2, g2, b2, _ = _rgba(bg)
    return (round(r1 * a + r2 * (1 - a)),
            round(g1 * a + g2 * (1 - a)),
            round(b1 * a + b2 * (1 - a)))


def _lum(rgb):
    def lin(c):
        c = c / 255.0
        return c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4
    r, g, b = rgb[:3]
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)


def contrast(fg, bg):
    """fg 写在 bg 上的对比度（1..21）。fg 若是半透明先合成，和屏幕上看到的一致。"""
    a, b = _lum(mix(fg, bg)), _lum(_rgba(bg)[:3])
    hi, lo = max(a, b), min(a, b)
    return (hi + 0.05) / (lo + 0.05)


def audit(key=None):
    """逐皮肤体检：返回 [(皮肤, 前景token, 背景token, 实际比值, 门槛)]。"""
    bad = []
    for k in ((key,) if key else SKINS):
        T = _SKIN_BASE[k]
        pairs = [(T[fg], T[bg], f"{fg}/{bg}", need)
                 for fg, bgs, need in _CONTRAST_RULES for bg in bgs]
        pairs += [(T[fg], mix(T[w], T[bg]), f"{fg}@{w}/{bg}", 3.0)
                  for fg, w in _CONTRAST_WASHES for bg in ("BG", "BG2", "BG3")]
        for fg, bg, where, need in pairs:
            r = contrast(fg, bg)
            if r < need:
                bad.append((k, where, round(r, 2), need))
    return bad


def _asset_path(*parts):
    base = os.path.dirname(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__))))
    if getattr(sys, "frozen", False) and not os.path.isdir(os.path.join(base, "assets")):
        base = getattr(sys, "_MEIPASS", base)
    return os.path.join(base, *parts)


_CHECK_ICON = _asset_path("assets", "icons", "check.svg").replace("\\", "/")
_CHEVRON_UP = _asset_path("assets", "icons", "chevron-up.svg").replace("\\", "/")
_CHEVRON_DOWN = _asset_path("assets", "icons", "chevron-down.svg").replace("\\", "/")


def _build_qss(T):
    """由 token 表生成整份 QSS。结构和皮肤无关，皮肤只填颜色。

    扁平化约定：分层只靠色面（BG→BG2→BG3→FIELD）和一条 1px 分隔线，
    按钮与卡片不描边、不变边框色，全界面没有渐变。输入框保留 1px 描边——
    它们填的是比窗口更深的 FIELD，去掉描边在浅色皮肤上和背景糊成一片。
    """
    return f"""
* {{ font-size: {fs(15)}px; outline: none; }}
QMainWindow, QWidget {{ background: {T['BG']}; color: {T['TEXT']}; }}
QToolTip {{ background: {T['BG3']}; color: {T['TEXT']};
            border: 1px solid {T['BORDER']}; border-radius: {R_MD}px;
            padding: 7px 10px; }}

/* ---- 顶部：工具标签 + 项目栏，没有左侧模块栏 ---- */
#TopBar {{ background: {T['BG2']}; border-bottom: 1px solid {T['SEP']}; }}
#TopLogo {{ background: transparent; }}
#ToolTabs {{ background: transparent; }}
QToolButton#ToolTab {{ background: transparent; border: none;
    border-radius: {R_SM}px; padding: 5px 10px; color: {T['TEXT_DIM']};
    font-size: {fs(14)}px; font-weight: 500; }}
QToolButton#ToolTab:hover {{ background: {T['HOVER']}; color: {T['TEXT']}; }}
QToolButton#ToolTab:checked {{ background: {T['SOFT']}; color: {T['ON']}; }}
QToolButton#ToolTab:disabled {{ color: {T['TEXT_OFF']}; background: transparent; }}
QLabel#PageTitle {{ color: {T['TEXT']}; font-size: {fs(18)}px; font-weight: 600; }}
QLabel#PageHint {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px; padding-left: 2px; }}
QLabel#DropHint {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px; }}
QLabel#MaterialChip {{ background: {T['BG3']}; color: {T['TEXT_DIM']};
    border: none; border-radius: {R_PILL}px; padding: 4px 12px; }}
QToolButton#OpenMedia {{ background: {T['ACCENT']}; color: {T['ON_ACCENT']};
    border: none; border-radius: {R_MD}px; padding: 6px 12px;
    font-weight: 600; }}
QToolButton#OpenMedia:hover {{ background: {T['ACCENT_HI']};
    color: {T['ON_ACCENT_HI']}; }}
QToolButton#OpenMedia:pressed {{ background: {T['ACCENT_DIM']};
    color: {T['ON_ACCENT']}; }}
QToolButton#OpenMedia::menu-button {{ background: transparent;
    border-left: 1px solid {T['WASH']}; width: 18px; }}
QToolButton#OpenMedia::menu-arrow {{ width: 7px; height: 7px; }}

/* 调色台子模式栏：贴着顶部素材栏下方，用圆角药丸 */
#SubBar {{ background: {T['BG']}; border-bottom: 1px solid {T['SEP']}; }}
#SubBar QPushButton {{ background: transparent; border: none;
    border-radius: {R_SM}px; padding: 6px 14px; color: {T['TEXT_DIM']};
    font-size: {fs(14)}px; font-weight: 500; }}
#SubBar QPushButton:hover {{ color: {T['TEXT']}; background: {T['HOVER']}; }}
#SubBar QPushButton:checked {{ background: {T['SOFT']}; color: {T['ON']}; }}
#SubBar QPushButton:disabled {{ color: {T['TEXT_OFF']};
    background: transparent; }}
#Title {{ color: {T['TEXT_DIM']}; }}

/* dock 标题：只留一条窄边，不要系统自带的按钮位 */
QDockWidget {{ background: {T['BG']}; border: 1px solid {T['SEP']};
    titlebar-close-icon: none; titlebar-normal-icon: none; }}
QDockWidget::title {{ background: {T['BG2']}; color: {T['TEXT_DIM']};
    padding: 7px 12px; font-size: {fs(13)}px; font-weight: 600;
    border-bottom: 1px solid {T['SEP']}; }}
/* 停靠面板之间的那条可拖拽缝：默认是灰色点阵，很上世纪 */
QMainWindow::separator {{ background: {T['BG']}; width: 3px; height: 3px; }}
QMainWindow::separator:hover {{ background: {T['ACCENT_DIM']}; }}

/* ---- 面板分组（手风琴）---- */
#GroupHeader {{ background: {T['BG2']}; border-bottom: 1px solid {T['SEP']}; }}
#GroupHeader:hover {{ background: {T['HOVER']}; }}
QLabel#GroupTitle {{ color: {T['TEXT']}; font-size: {fs(14)}px; font-weight: 600;
    background: transparent; }}
QToolButton#GroupChevron {{ background: transparent; border: none; padding: 0; }}
QToolButton#GroupReset {{ background: transparent; border: none;
    border-radius: {R_SM}px; padding: 2px; }}
QToolButton#GroupReset:hover {{ background: {T['BG3']}; }}
#GroupBody {{ background: {T['BG']}; }}
#DockTitle {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px; font-weight: 600; }}
/* 非默认值像 Capture One 一样用强调色，扫一眼就知道动过哪些参数 */
QLineEdit[modified="true"] {{ color: {T['ACCENT_HI']}; }}

/* ---- 预览底部信息栏与播放条 ---- */
#ViewerFooter {{ background: {T['BG2']}; border-top: 1px solid {T['SEP']}; }}
QLabel#ViewerFile {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px; }}
QLabel#ViewerMeta {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px; }}
QLabel#ViewerZoom {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px; }}
QToolButton#ViewerZoom {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px;
    font-weight: 500; background: transparent; border: none;
    border-radius: {R_SM}px; padding: 2px 6px; margin-right: 2px; }}
QToolButton#ViewerZoom:hover {{ color: {T['TEXT']}; background: {T['HOVER']}; }}
QToolButton#ViewerZoom::menu-indicator {{ image: none; width: 0; }}
#TransportBar {{ background: {T['BG2']}; border-top: 1px solid {T['SEP']}; }}
QLabel#TimeCode {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px;
    font-weight: 500; }}

QLabel {{ background: transparent; }}
QLabel#ValueLabel {{ color: {T['ACCENT_HI']}; font-size: {fs(13)}px;
    font-weight: 500; }}
/* 滑块名是面板里最常读的字，比次级说明亮一档 */
QLabel#NameLabel {{ color: {T['TEXT_SOFT']}; font-weight: 500; }}
QLabel#NameLabel:hover {{ color: {T['TEXT']}; }}

/* ---- 按钮：纯色面 + 圆角，不描边、不变边框色（扁平）---- */
QPushButton {{ background: {T['BG3']}; color: {T['TEXT']};
    border: none; border-radius: {R_MD}px;
    padding: 6px 14px; font-weight: 500; }}
QPushButton:hover {{ background: {T['HOVER']}; }}
QPushButton:pressed {{ background: {T['PRESSED']}; }}
QPushButton:disabled {{ background: transparent; color: {T['TEXT_OFF']}; }}
QPushButton:checked {{ background: {T['SOFT']}; color: {T['ON']}; }}
/* 图标按钮：幽灵底 + 悬停泛光，选中填强调色底，和工具栏一样安静 */
QToolButton#Icon {{ background: transparent; border: none;
    border-radius: {R_MD}px; padding: 0; }}
QToolButton#Icon:hover {{ background: {T['HOVER']}; }}
QToolButton#Icon:pressed {{ background: {T['PRESSED']}; }}
QToolButton#Icon:checked {{ background: {T['SOFT']}; }}
QToolButton#Icon:disabled {{ background: transparent; }}
/* 栏内的次级文字按钮（撤销/重做）：比页签矮一档，靠色面区分 */
QPushButton#ToolBtn {{ background: {T['BG3']}; border: none;
    border-radius: {R_MD}px; padding: 5px 12px; color: {T['TEXT']}; }}
QPushButton#ToolBtn:hover {{ background: {T['HOVER']}; }}
QPushButton#ToolBtn:pressed {{ background: {T['PRESSED']}; }}
QPushButton#ToolBtn:disabled {{ background: transparent;
    color: {T['TEXT_OFF']}; }}
/* 面板内的紧凑按钮（预设、通道切换）：默认内边距在窄面板里排不下三个 */
QPushButton#Chip {{ border-radius: {R_SM}px; padding: 5px 8px; }}
QPushButton#Primary {{ background: {T['ACCENT']}; color: {T['ON_ACCENT']};
    border: none; font-weight: 600; padding: 7px 18px; }}
QPushButton#Primary:hover {{ background: {T['ACCENT_HI']};
    color: {T['ON_ACCENT_HI']}; }}
QPushButton#Primary:pressed {{ background: {T['ACCENT_DIM']};
    color: {T['ON_ACCENT']}; }}
QPushButton#Primary:disabled {{ background: {T['BG3']};
    color: {T['TEXT_OFF']}; }}

/* ---- 输入：数字框的边框只画在内部 QLineEdit 上。
   Qt 的样式表一旦给 QAbstractSpinBox 本身写 border（哪怕 border: none），
   就会把内部 QLineEdit 压成 1px 宽（NoButtons 时尤甚），数值直接看不见。
   外框由代码 setFrame(False) 关掉，这里只碰 QLineEdit。---- */
QLineEdit, QComboBox {{ background: {T['FIELD']}; border: 1px solid {T['BORDER']};
    border-radius: {R_MD}px; padding: 5px 9px; color: {T['TEXT']};
    selection-background-color: {T['ACCENT']};
    selection-color: {T['ON_ACCENT']}; }}
QSpinBox QLineEdit, QDoubleSpinBox QLineEdit {{
    background: {T['FIELD']}; border: 1px solid {T['BORDER']};
    border-radius: {R_MD}px; padding: 4px 7px; color: {T['TEXT']};
    selection-background-color: {T['ACCENT']};
    selection-color: {T['ON_ACCENT']}; }}
QLineEdit:focus, QComboBox:focus, QSpinBox QLineEdit:focus,
QDoubleSpinBox QLineEdit:focus {{ border-color: {T['ACCENT']}; }}
/* 滑块行右侧的数值：平时隐形，悬停/聚焦才显出输入框 */
QDoubleSpinBox#ValBox QLineEdit, QSpinBox#ValBox QLineEdit {{
    background: transparent; border: 1px solid transparent; text-align: right; }}
QDoubleSpinBox#ValBox:hover QLineEdit, QSpinBox#ValBox:hover QLineEdit {{
    background: {T['FIELD']}; border-color: {T['BORDER']}; }}
QDoubleSpinBox#ValBox QLineEdit:focus, QSpinBox#ValBox QLineEdit:focus {{
    border-color: {T['ACCENT']}; background: {T['FIELD']}; }}
QComboBox QAbstractItemView {{ background: {T['BG3']};
    border: 1px solid {T['BORDER']}; color: {T['TEXT']};
    selection-background-color: {T['SOFT_HI']}; color: {T['TEXT']};
    outline: none; padding: 4px; }}
QComboBox QAbstractItemView::item {{ padding: 6px 10px;
    border-radius: {R_SM}px; }}
QComboBox QAbstractItemView:selected {{ background: {T['SOFT_HI']};
    color: {T['ON']}; }}

QComboBox::drop-down {{ border: none; width: 22px; }}
QComboBox::down-arrow {{ image: url("{_CHEVRON_DOWN}"); width: 10px; height: 10px; }}

/* 数字框：给 QSpinBox 本体描边会压掉内部 QLineEdit（NoButtons 时只剩 1px），
   所以只给带 boxed 属性的常规输入框上边框；滑块行的 ValBox 保持无框。 */
QSpinBox[boxed="true"], QDoubleSpinBox[boxed="true"] {{
    background: {T['FIELD']}; border: 1px solid {T['BORDER']};
    border-radius: {R_MD}px; padding: 4px 22px 4px 8px; color: {T['TEXT']}; }}
QSpinBox[boxed="true"]:focus, QDoubleSpinBox[boxed="true"]:focus {{
    border-color: {T['ACCENT']}; }}
QSpinBox[boxed="true"]::up-button, QDoubleSpinBox[boxed="true"]::up-button {{
    subcontrol-origin: border; subcontrol-position: top right;
    width: 18px; border: none; background: transparent; }}
QSpinBox[boxed="true"]::down-button, QDoubleSpinBox[boxed="true"]::down-button {{
    subcontrol-origin: border; subcontrol-position: bottom right;
    width: 18px; border: none; background: transparent; }}
QSpinBox[boxed="true"]::up-arrow, QDoubleSpinBox[boxed="true"]::up-arrow {{
    image: url("{_CHEVRON_UP}"); width: 9px; height: 9px; }}
QSpinBox[boxed="true"]::down-arrow, QDoubleSpinBox[boxed="true"]::down-arrow {{
    image: url("{_CHEVRON_DOWN}"); width: 9px; height: 9px; }}

/* ---- 复选/单选：纯色指示器，不做内发光渐变 ---- */
QCheckBox, QRadioButton {{ background: transparent; spacing: 8px; }}
QCheckBox::indicator {{ width: 15px; height: 15px; border-radius: 4px;
    border: 1px solid {T['BORDER_HI']}; background: {T['FIELD']}; }}
QCheckBox::indicator:hover {{ border-color: {T['TEXT_OFF']}; }}
QCheckBox::indicator:checked {{ border: none; background: transparent;
    image: url("{_CHECK_ICON}"); }}
QCheckBox::indicator:disabled {{ border-color: {T['SEP']};
    background: {T['BG2']}; }}
QRadioButton::indicator {{ width: 15px; height: 15px;
    border-radius: {R_MD}px; border: 1px solid {T['BORDER_HI']};
    background: {T['FIELD']}; }}
QRadioButton::indicator:hover {{ border-color: {T['TEXT_OFF']}; }}
QRadioButton::indicator:checked {{ border: 1px solid {T['ACCENT']};
    background: {T['ACCENT']}; }}
QRadioButton::indicator:disabled {{ border-color: {T['SEP']};
    background: {T['BG2']}; }}

/* ---- 缩略图：选中用描边，不要整块底色 ---- */
QListWidget, QListView {{ background: {T['BG']}; border: none; padding: 4px; }}
QListWidget::item, QListView::item {{ border: 2px solid transparent;
    border-radius: {R_MD}px; color: {T['TEXT_DIM']}; }}
QListWidget::item:hover, QListView::item:hover {{ background: {T['HOVER']}; }}
QListWidget::item:selected, QListView::item:selected {{
    background: {T['SOFT']}; border-color: {T['ACCENT']}; color: {T['TEXT']}; }}

QScrollArea {{ border: none; background: {T['BG']}; }}
QScrollArea > QWidget > QWidget {{ background: {T['BG']}; }}

/* ---- 菜单 ---- */
QMenuBar {{ background: {T['BG2']}; color: {T['TEXT']};
    border-bottom: 1px solid {T['SEP']}; padding: 1px 4px; }}
QMenuBar::item {{ background: transparent; padding: 6px 12px;
    border-radius: {R_SM}px; }}
QMenuBar::item:selected {{ background: {T['HOVER']}; }}
QMenu {{ background: {T['BG3']}; border: 1px solid {T['BORDER']};
    border-radius: {R_LG}px; padding: 5px; }}
QMenu::item {{ padding: 7px 28px 7px 16px; border-radius: {R_SM}px; }}
QMenu::item:selected {{ background: {T['SOFT_HI']}; color: {T['ON']}; }}
QMenu::separator {{ height: 1px; background: {T['BORDER']}; margin: 5px 8px; }}
QMenu::indicator {{ width: 15px; height: 15px; }}

/* ---- 状态栏与进度 ---- */
QStatusBar {{ background: {T['BG2']}; color: {T['TEXT_OFF']};
    border-top: 1px solid {T['SEP']}; }}
QStatusBar::item {{ border: none; }}
QProgressBar {{ background: {T['FIELD']}; border: none;
    border-radius: 4px; height: 8px; text-align: center; color: {T['TEXT']}; }}
QProgressBar::chunk {{ background: {T['ACCENT']}; border-radius: 4px; }}

/* ---- 通用文本/信息行 ---- */
QPlainTextEdit#Log {{ background: {T['FIELD']}; color: {T['TEXT_DIM']};
    border: 1px solid {T['SEP']}; border-radius: {R_MD}px;
    font-family: Consolas; font-size: {fs(13)}px; }}
QLabel#Info {{ color: {T['TEXT_DIM']}; }}
QLabel#InfoBad {{ color: {T['BAD']}; }}
QLabel#Hint {{ color: {T['OK']}; }}
QLabel#FieldLabel {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px;
    font-weight: 500; }}
QLabel#FieldHint {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px; }}
QLabel#CardKey {{ color: {T['TEXT_OFF']}; }}
QLabel#CardVal {{ color: {T['TEXT']}; }}
/* ---- 结果卡：结论、关键数字、折叠日志三层 ---- */
QFrame#ResultCard {{ background: {T['BG2']}; border: none;
    border-radius: {R_LG}px; }}
QLabel#ResultTitle {{ color: {T['TEXT']}; font-size: {fs(15)}px; font-weight: 600; }}
QFrame#Metric {{ background: {T['BG3']}; border: none;
    border-radius: {R_MD}px; }}
QLabel#MetricVal {{ color: {T['TEXT']}; font-size: {fs(15)}px; font-weight: 600; }}
QLabel#MetricKey {{ color: {T['TEXT_OFF']}; font-size: {fs(12)}px; }}
QLabel#StepLabel {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px; }}
QLabel#StepNote {{ color: {T['TEXT_OFF']}; font-size: {fs(12)}px; }}
QLabel#RawTitle {{ color: {T['TEXT_OFF']}; font-size: {fs(12)}px;
    font-weight: 600; }}
QLabel#ResultPath {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px; }}
/* ---- 起始页：没有素材时把入口做成卡片 ---- */
#StartPage {{ background: {T['BG']}; }}
QLabel#StartTitle {{ color: {T['TEXT']}; font-size: {fs(26)}px; font-weight: 600; }}
QLabel#StartSub {{ color: {T['TEXT_OFF']}; font-size: {fs(14)}px; }}
QFrame#StartCard {{ background: {T['BG2']}; border: none;
    border-radius: {R_XL}px; }}
QFrame#StartCard:hover {{ background: {T['BG3']}; }}
QLabel#StartCardTitle {{ color: {T['TEXT']}; font-size: {fs(16)}px;
    font-weight: 600; }}
QLabel#StartCardHint {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px; }}
QLabel#StartSection {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px;
    font-weight: 600; }}
QToolButton#StartTool {{ background: transparent; border: none;
    border-radius: {R_SM}px; padding: 4px 9px; color: {T['TEXT_DIM']}; }}
QToolButton#StartTool:hover {{ background: {T['HOVER']}; color: {T['TEXT']}; }}
QPushButton#StartRecent {{ background: transparent; border: none;
    border-radius: {R_SM}px; padding: 7px 10px; text-align: left;
    color: {T['TEXT_DIM']}; }}
QPushButton#StartRecent:hover {{ background: {T['BG2']}; color: {T['TEXT']}; }}
QLabel#StartHint {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px; }}

/* ---- 设置：左侧导航 + 右侧表单 ---- */
QLabel#SettingsTitle {{ color: {T['TEXT']}; font-size: {fs(18)}px;
    font-weight: 600; }}
QLabel#SettingsHint {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px; }}
QLabel#SettingsPath {{ color: {T['TEXT_DIM']}; font-size: {fs(13)}px; }}
QListWidget#SettingsNav {{ background: {T['BG2']};
    border-right: 1px solid {T['SEP']}; padding: 8px 6px; }}
QListWidget#SettingsNav::item {{ padding: 9px 12px;
    border-radius: {R_MD}px; color: {T['TEXT_DIM']}; }}
QListWidget#SettingsNav::item:hover {{ background: {T['HOVER']}; }}
QListWidget#SettingsNav::item:selected {{ background: {T['SOFT']};
    color: {T['ON']}; }}
QPushButton#Danger {{ color: {T['BAD']}; }}
QPushButton#Danger:hover {{ background: {T['HOVER']}; }}
QLabel#AboutName {{ color: {T['TEXT']}; font-size: {fs(20)}px; font-weight: 600; }}

/* 空画布上的引导：比正文暗两档，不跟素材抢注意力 */
QLabel#EmptyHint {{ color: {T['TEXT_OFF']}; font-size: {fs(15)}px; }}
/* 选项下面那行说明：贴紧自己的单选框，组与组之间才留白 */
QLabel#ModeHint {{ color: {T['TEXT_OFF']}; font-size: {fs(13)}px;
    margin: -3px 0 5px 22px; }}

/* ---- 滚动条：细、无箭头、悬停变亮 ---- */
QScrollBar:vertical {{ background: transparent; width: 10px;
    margin: 2px 2px 2px 0; }}
QScrollBar::handle:vertical {{ background: {T['BORDER']}; border-radius: 4px;
    min-height: 28px; }}
QScrollBar::handle:vertical:hover {{ background: {T['BORDER_HI']}; }}
QScrollBar:horizontal {{ background: transparent; height: 10px;
    margin: 0 2px 2px 2px; }}
QScrollBar::handle:horizontal {{ background: {T['BORDER']}; border-radius: 4px;
    min-width: 28px; }}
QScrollBar::handle:horizontal:hover {{ background: {T['BORDER_HI']}; }}
QScrollBar::add-line, QScrollBar::sub-line {{ height: 0; width: 0; }}
QScrollBar::add-page, QScrollBar::sub-page {{ background: transparent; }}

/* ---- 播放进度条：拿 QSlider 当时间轴，轨道细、手柄小 ---- */
QSlider::groove:horizontal {{ height: 4px; background: {T['BORDER']};
    border-radius: 2px; }}
QSlider::sub-page:horizontal {{ background: {T['ACCENT']}; border-radius: 2px; }}
QSlider::handle:horizontal {{ background: {T['HANDLE']}; width: 12px;
    margin: -5px 0; border-radius: 6px; }}
QSlider::handle:horizontal:hover {{ background: {T['ACCENT_HI']}; }}
"""


# 模块级 token 常量（初次导入即默认皮肤）
SKIN = DEFAULT_SKIN
TOK = dict(_SKIN_BASE[DEFAULT_SKIN])
globals().update({k: TOK[k] for k in _token_names})
QSS = _build_qss(TOK)


def label(key):
    return _SKIN_BASE[key]["label"]


def corner_label(mode):
    return dict((m, name) for name, m in CORNER_MODES).get(
        mode, CORNER_MODES[0][0])


def is_dark():
    return TOK["dark"]


def _register(filename, fallback):
    path = _asset_path("assets", "fonts", filename)
    if not os.path.exists(path):
        return fallback
    fid = QtGui.QFontDatabase.addApplicationFont(path)
    names = QtGui.QFontDatabase.applicationFontFamilies(fid) if fid >= 0 else []
    return names[0] if names else fallback


def _load_fonts():
    """返回按字符回退的家族链：Inter → 雅黑 UI → Noto Sans SC。

    顺序不能乱：回退是"第一个含该字形的家族"赢，雅黑若排在 Inter 前面，
    西文和数字就全成雅黑的了。Windows 上中文走系统雅黑 UI——它带 TrueType
    hinting，13-15px 的中文笔画比 Noto Sans SC 实得多（屏幕像素探针实测）；
    Noto 留在链尾给非 Windows 平台兜底。
    """
    families = [_register("Inter-Variable.ttf", "Inter")]
    if sys.platform == "win32" and QtGui.QFontDatabase.hasFamily(
            "Microsoft YaHei UI"):
        families.append("Microsoft YaHei UI")
    families.append(_register("NotoSansSC-Variable.ttf", "Noto Sans SC"))
    return families


_FAMILIES = None          # 字体只注册一次，切皮肤不重复加载


def _set_titlebar(win):
    """把原生标题栏刷成当前皮肤的颜色，并按设置给窗口弧线。

    QSS 管不到系统标题栏；不设的话，暗色界面顶着一根白条，一眼就露馅。
    """
    if sys.platform != "win32":
        return
    try:
        hwnd = int(win.winId())
    except (AttributeError, RuntimeError):
        return
    import ctypes
    try:
        dwm = ctypes.windll.dwmapi
    except (AttributeError, OSError):
        return

    def set_attr(attr, value):
        v = ctypes.c_int(value)
        return dwm.DwmSetWindowAttribute(ctypes.c_void_p(hwnd), attr,
                                         ctypes.byref(v), ctypes.sizeof(v))

    # 20 是 Win10 20H1+ 与 Win11 的编号；更早的预览版用 19
    dark = 1 if TOK["dark"] else 0
    if set_attr(20, dark) != 0:
        set_attr(19, dark)

    def colorref(hex_color):
        # Win32 是 0x00BBGGRR，QSS 是 #RRGGBB，顺序反过来
        return int(hex_color[5:7] + hex_color[3:5] + hex_color[1:3], 16)

    # Win11 才认的几项；老系统返回失败，忽略即可
    global CORNER_HR
    if CORNER >= 0:
        CORNER_HR = set_attr(33, CORNER)     # DWMWA_WINDOW_CORNER_PREFERENCE
    set_attr(35, colorref(TOK["BG2"]))    # DWMWA_CAPTION_COLOR
    set_attr(36, colorref(TOK["TEXT"]))   # DWMWA_TEXT_COLOR
    set_attr(34, colorref(TOK["SEP"]))    # DWMWA_BORDER_COLOR


class _TitleBarFilter(QtCore.QObject):
    """顶层窗口一显示就刷标题栏：主窗口、消息框、文件对话框都盖到。"""

    def eventFilter(self, obj, ev):
        if ev.type() == QtCore.QEvent.Type.Show and isinstance(
                obj, (QtWidgets.QMainWindow, QtWidgets.QDialog)):
            _set_titlebar(obj)
        return False


def _palette():
    pal = QtGui.QPalette()
    T = TOK
    c = {
        QtGui.QPalette.Window: T["BG"],
        QtGui.QPalette.WindowText: T["TEXT"],
        QtGui.QPalette.Base: T["FIELD"],
        QtGui.QPalette.AlternateBase: T["BG2"],
        QtGui.QPalette.ToolTipBase: T["BG3"],
        QtGui.QPalette.ToolTipText: T["TEXT"],
        QtGui.QPalette.Text: T["TEXT"],
        QtGui.QPalette.Button: T["BG3"],
        QtGui.QPalette.ButtonText: T["TEXT"],
        QtGui.QPalette.BrightText: "#ffffff",
        QtGui.QPalette.Highlight: T["ACCENT"],
        QtGui.QPalette.HighlightedText: T["ON_ACCENT"],
        QtGui.QPalette.Link: T["ACCENT_HI"],
        QtGui.QPalette.PlaceholderText: T["TEXT_OFF"],
    }
    for role, col in c.items():
        pal.setColor(role, QtGui.QColor(col))
    for state in (QtGui.QPalette.Active, QtGui.QPalette.Inactive,
                  QtGui.QPalette.Disabled):
        pal.setColor(state, QtGui.QPalette.Window, QtGui.QColor(T["BG"]))
    pal.setColor(QtGui.QPalette.Disabled, QtGui.QPalette.Text,
                 QtGui.QColor(T["TEXT_OFF"]))
    pal.setColor(QtGui.QPalette.Disabled, QtGui.QPalette.ButtonText,
                 QtGui.QColor(T["TEXT_OFF"]))
    return pal


def apply_tokens(key):
    """只换 token 与 QSS，不碰 QApplication：测试里可以单独调用。"""
    global SKIN, TOK, QSS
    if key not in _SKIN_BASE:
        raise KeyError(key)
    SKIN = key
    TOK = dict(_SKIN_BASE[key])
    globals().update({k: TOK[k] for k in _token_names})
    QSS = _build_qss(TOK)
    return TOK


_on_change = []            # [(对象, 可调用)]，icons 与外壳往里登记


def on_change(fn):
    _on_change.append(fn)


def apply(app, skin=None):
    """启动时调用：Fusion 风格 + 随包字体 + 皮肤（默认深空）。"""
    global _FAMILIES
    app.setStyle("Fusion")
    if _FAMILIES is None:
        _FAMILIES = _load_fonts()
    font = QtGui.QFont()
    font.setFamilies(_FAMILIES)
    font.setPixelSize(fs(FONT_BASE))
    # 保留 ClearType（子像素 AA）：探针实测关掉它之后 13-15px 的中文笔画
    # 发虚是"不清晰"的主因之一。早年为压 indigo 按钮上白字的彩边关掉过，
    # 但彩边只在饱和底色上轻微可见，而发虚是全屏每一行字都在付代价；
    # 强调色按钮上的字一律 600 字重，粗笔画本身就能盖住彩边。
    font.setStyleStrategy(QtGui.QFont.StyleStrategy.PreferAntialias)
    app.setFont(font)
    set_skin(app, skin or SKIN)
    # 必须挂在 app 上保活；标题栏只影响原生框架，与 QSS 无关
    app._titlebar_filter = _TitleBarFilter(app)
    app.installEventFilter(app._titlebar_filter)


def _refresh_titlebars(app):
    for w in app.topLevelWidgets():
        if isinstance(w, (QtWidgets.QMainWindow, QtWidgets.QDialog)) \
                and w.isVisible():
            _set_titlebar(w)


def set_corner_mode(mode):
    """改窗口弧线（-1 跟随系统 / 1 直角 / 2 大圆角 / 3 小圆角）。

    Win10 不认这个属性，设了只是没效果；CORNER_HR 留着给测试判断支持情况。
    """
    global CORNER
    CORNER = int(mode)
    app = QtWidgets.QApplication.instance()
    if app is not None:
        _refresh_titlebars(app)
    return CORNER


def set_skin(app, key):
    """运行时换皮肤：重写 token → 重新生成 QSS/调色板 → 通知图标重画。"""
    if key not in _SKIN_BASE:
        key = DEFAULT_SKIN
    apply_tokens(key)
    app.setPalette(_palette())
    app.setStyleSheet(QSS)
    for fn in list(_on_change):
        fn()
    _refresh_titlebars(app)
    return key


def set_font(app, key):
    """换字号档：重算乘数 → 整份重建 QSS → 改应用字体 → 通知自绘控件。

    字号是生成 QSS 时算进去的，只改 QApplication 字体没用：QSS 里那些
    13px 的说明文字会纹丝不动，档位看起来就"只生效了一半"。
    """
    global FONT, FONT_SCALE
    table = dict(FONT_LEVELS)
    if key not in table:
        key = DEFAULT_FONT
    FONT = key
    FONT_SCALE = table[key]
    if app is not None:
        f = app.font()
        f.setPixelSize(fs(FONT_BASE))
        app.setFont(f)
        set_skin(app, SKIN)
    return FONT
