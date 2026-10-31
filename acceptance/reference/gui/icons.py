"""界面图标：Lucide 线稿（ISC 许可）经 QSvgRenderer 渲染成矢量图标。

path 数据是 Lucide 原样的 24×24 视图框，颜色在渲染时注入，所以同一枚图标
可以出正常/悬停/选中三档颜色。SVG 按 3 倍像素栅格化，高分屏下依旧干净。

皮肤会改颜色，而图标一旦画进 QPixmap 就冻住了 —— 所以按钮不要直接
setIcon(icon(...))，而是用 bind(按钮, 图标名, 颜色角色)：颜色角色在换皮肤
时由 theme.on_change 触发 refresh() 重新取色重画。
"""
import weakref

from PySide6 import QtCore, QtGui, QtSvg

from reference.gui import theme

SIZE = 24
_SCALE = 3
_STROKE = 1.9

# Lucide 原始 path（https://lucide.dev，ISC License）
_SVG = {
    # focus：对齐 —— 取景框四角 + 中心圆点
    "对齐": """
        <circle cx="12" cy="12" r="3"/>
        <path d="M3 7V5a2 2 0 0 1 2-2h2"/>
        <path d="M17 3h2a2 2 0 0 1 2 2v2"/>
        <path d="M21 17v2a2 2 0 0 1-2 2h-2"/>
        <path d="M7 21H5a2 2 0 0 1-2-2v-2"/>""",
    # clapperboard：合成 —— 场记板
    "合成": """
        <path d="M20.2 6 3 11l-.9-2.4c-.3-1.1.3-2.2 1.3-2.5l13.5-4c
                 1.1-.3 2.2.3 2.5 1.3Z"/>
        <path d="m6.2 5.3 3.1 3.9"/>
        <path d="m12.4 3.4 3.1 4"/>
        <path d="M3 11h18v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"/>""",
    # crop：裁剪 —— 两道交叠的裁切标尺
    "裁剪": """
        <path d="M6 2v14a2 2 0 0 0 2 2h14"/>
        <path d="M18 22V8a2 2 0 0 0-2-2H2"/>""",
    # music：配乐 —— 一串连在谱线上的音符
    "配乐": """
        <path d="M9 18V5l12-2v13"/>
        <circle cx="6" cy="18" r="3"/>
        <circle cx="18" cy="16" r="3"/>""",
    # gauge：运动 —— 指针偏离正中，表示倍速/快慢
    "运动": """
        <path d="m12 14 4-4"/>
        <path d="M3.34 19a10 10 0 1 1 17.32 0"/>""",
    # sliders-horizontal：调色台 —— 三条推子
    "调色台": """
        <line x1="21" x2="14" y1="4" y2="4"/>
        <line x1="10" x2="3" y1="4" y2="4"/>
        <line x1="21" x2="12" y1="12" y2="12"/>
        <line x1="8" x2="3" y1="12" y2="12"/>
        <line x1="21" x2="16" y1="20" y2="20"/>
        <line x1="12" x2="3" y1="20" y2="20"/>
        <line x1="14" x2="14" y1="2" y2="6"/>
        <line x1="8" x2="8" y1="10" y2="14"/>
        <line x1="16" x2="16" y1="18" y2="22"/>""",
    # folder-plus：打开素材
    "打开素材": """
        <path d="M12 10v6"/>
        <path d="M9 13h6"/>
        <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6
                 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>""",
    # folder-open：打开文件夹
    "folder-open": """
        <path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2
                 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1
                 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"/>""",
    # chevron-down / chevron-right：分组展开态
    "chevron-down": """<path d="m6 9 6 6 6-6"/>""",
    "chevron-right": """<path d="m9 6 6 6-6 6"/>""",
    # rotate-ccw：分组复位
    "rotate-ccw": """
        <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/>
        <path d="M3 3v5h5"/>""",
    # undo-2 / redo-2：撤销 / 重做
    "undo": """
        <path d="M9 14 4 9l5-5"/>
        <path d="M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5a5.5 5.5 0 0 1-5.5 5.5H11"/>""",
    "redo": """
        <path d="m15 14 5-5-5-5"/>
        <path d="M20 9H9.5a5.5 5.5 0 0 0-5.5 5.5a5.5 5.5 0 0 0 5.5 5.5H13"/>""",
    # sparkles：自动校正
    "自动": """
        <path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1
                 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0
                 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0
                 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582
                 6.135a.5.5 0 0 1-.963 0z"/>
        <path d="M20 3v4"/><path d="M22 5h-4"/>
        <path d="M4 17v2"/><path d="M5 18H3"/>""",
    # ---- 播放控制 ----
    "play": """<path d="M5 5.607a1 1 0 0 1 1.4-.18l9.6 6.4a1 1 0 0 1 0
               1.662l-9.6 6.4A1 1 0 0 1 5 18.392z"/>""",
    "pause": """<rect x="7" y="5" width="3.5" height="14" rx="1"/>
                <rect x="13.5" y="5" width="3.5" height="14" rx="1"/>""",
    "step-back": """<path d="M19 20 9 12l10-8z"/><path d="M5 19V5"/>""",
    "step-forward": """<path d="m5 4 10 8-10 8z"/><path d="M19 5v14"/>""",
    "repeat": """
        <path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/>
        <path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>""",
    "film": """
        <rect width="18" height="18" x="3" y="3" rx="2"/>
        <path d="M7 3v18"/><path d="M17 3v18"/><path d="M3 7.5h4"/>
        <path d="M3 12h18"/><path d="M3 16.5h4"/><path d="M17 7.5h4"/>
        <path d="M17 16.5h4"/>""",
    # ---- 子模式 / 常用动作 ----
    "grid": """
        <rect width="7" height="7" x="3" y="3" rx="1.5"/>
        <rect width="7" height="7" x="14" y="3" rx="1.5"/>
        <rect width="7" height="7" x="14" y="14" rx="1.5"/>
        <rect width="7" height="7" x="3" y="14" rx="1.5"/>""",
    "sun": """
        <circle cx="12" cy="12" r="4"/>
        <path d="M12 2v2"/><path d="M12 20v2"/><path d="M2 12h2"/>
        <path d="M20 12h2"/><path d="m4.93 4.93 1.41 1.41"/>
        <path d="m17.66 17.66 1.41 1.41"/><path d="m6.34 17.66-1.41 1.41"/>
        <path d="m19.07 4.93-1.41 1.41"/>""",
    "download": """
        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
        <path d="m7 10 5 5 5-5"/><path d="M12 15V3"/>""",
    "split": """<rect width="18" height="18" x="3" y="3" rx="2.5"/>
                <path d="M12 3v18"/>""",
    "pipette": """
        <path d="m2 22 1-1h3l9-9"/><path d="M3 21v-3l9-9"/>
        <path d="m15 6 3.4-3.4a2.1 2.1 0 1 1 3 3L18 9l.4.4a2.1 2.1 0 1
                 1-3 3l-3.8-3.8a2.1 2.1 0 1 1 3-3l.4.4Z"/>""",
    "levels": """
        <path d="M4 20h16"/><path d="M7 20V9"/><path d="M12 20V4"/>
        <path d="M17 20v-7"/>""",
    "contrast": """
        <circle cx="12" cy="12" r="9"/>
        <path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor"/>""",
    "droplet": """<path d="M12 22a7 7 0 0 0 7-7c0-2-1-3.9-3-5.5s-3.5-4-4-6.5
                  c-.5 2.5-2 4.9-4 6.5C6 11.1 5 13 5 15a7 7 0 0 0 7 7z"/>""",
    "eye": """
        <path d="M2.06 12.35a1 1 0 0 1 0-.7 10.75 10.75 0 0 1 19.87 0 1 1 0
                 0 1 0 .7 10.75 10.75 0 0 1-19.87 0"/>
        <circle cx="12" cy="12" r="3"/>""",
    "mask": """
        <path d="M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z"/>
        <path d="M12 3a6 6 0 0 0 0 18"/>""",
    "history": """
        <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/>
        <path d="M3 3v5h5"/><path d="M12 7v5l4 2"/>""",
    "refresh": """
        <path d="M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8"/>
        <path d="M3 3v5h5"/><path d="M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0
                 0 6.74-2.74L21 16"/>
        <path d="M21 21v-5h-5"/>""",
    "zoom-in": """
        <circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>
        <path d="M11 8v6"/><path d="M8 11h6"/>""",
    "x": """<path d="M18 6 6 18"/><path d="m6 6 12 12"/>""",
    "check": """<path d="M20 6 9 17l-5-5"/>""",
    "palette": """
        <path d="M12 2C6.5 2 2 6.5 2 12s4.5 10 10 10c.9 0 1.6-.7 1.6-1.7
                 0-.4-.2-.8-.4-1.1-.3-.3-.4-.7-.4-1.1a1.6 1.6 0 0 1 1.7-1.7h2
                 c3 0 5.5-2.5 5.5-5.5C22 6 17.5 2 12 2z"/>
        <circle cx="7.5" cy="11.5" r="1.2"/><circle cx="11" cy="7" r="1.2"/>
        <circle cx="16" cy="9.5" r="1.2"/>""",
    "plus": """<path d="M5 12h14"/><path d="M12 5v14"/>""",
    "trash": """
        <path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/>
        <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>
        <path d="M10 11v6"/><path d="M14 11v6"/>""",
    # settings：设置 —— Lucide 齿轮
    "settings": """
        <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2
                 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0
                 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2
                 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1
                 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0
                 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0
                 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0
                 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2
                 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0
                 1-1-1.73V4a2 2 0 0 0-2-2z"/>
        <circle cx="12" cy="12" r="3"/>""",
    "circle": """<circle cx="12" cy="12" r="9"/>""",
    "slash": """<path d="M6 3h4l4 18h-4"/>""",
}

_CACHE = {}


def has(name):
    return name in _SVG


def _pixmap(name, color, size=SIZE):
    key = (name, color, size)
    cached = _CACHE.get(key)
    if cached is not None:
        return cached
    doc = (
        '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" '
        'viewBox="0 0 24 24" fill="none" stroke="' + color + '" '
        'stroke-width="' + str(_STROKE) + '" stroke-linecap="round" '
        'stroke-linejoin="round">' + _SVG[name] + "</svg>"
    )
    renderer = QtSvg.QSvgRenderer(QtCore.QByteArray(doc.encode("utf-8")))
    pm = QtGui.QPixmap(size * _SCALE, size * _SCALE)
    pm.fill(QtCore.Qt.GlobalColor.transparent)
    pm.setDevicePixelRatio(_SCALE)
    p = QtGui.QPainter(pm)
    p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)
    renderer.render(p, QtCore.QRectF(0, 0, size, size))
    p.end()
    _CACHE[key] = pm
    return pm


N = QtGui.QIcon.Mode.Normal
A = QtGui.QIcon.Mode.Active
D = QtGui.QIcon.Mode.Disabled
OFF = QtGui.QIcon.State.Off
ON = QtGui.QIcon.State.On


def icon(name, color=None, size=16, active=None, checked=None):
    """通用线稿图标。

    color 省略时按当前皮肤取 TEXT_DIM —— 换皮肤要重画，请用 bind() 而不是这个。
    checked 给可勾选按钮补 State.On 那一档：Qt 对选中按钮取的正是这一张。
    """
    color = color or theme.TEXT_DIM
    active = active or theme.TEXT
    ic = QtGui.QIcon()
    ic.addPixmap(_pixmap(name, color, size), N, OFF)
    ic.addPixmap(_pixmap(name, active, size), A, OFF)
    ic.addPixmap(_pixmap(name, theme.BORDER_HI, size), D, OFF)
    if checked:
        ic.addPixmap(_pixmap(name, checked, size), N, ON)
        ic.addPixmap(_pixmap(name, checked, size), A, ON)
    return ic


def _colors(role):
    """颜色角色 → 当前皮肤下的三档色值。"""
    T = theme.TOK
    if role == "on_accent":
        return T["ON_ACCENT"], T["ON_ACCENT_HI"], T["TEXT_OFF"]
    if role == "accent":
        return T["ACCENT"], T["ACCENT_HI"], T["BORDER_HI"]
    base = {"dim": "TEXT_DIM", "text": "TEXT", "soft": "TEXT_SOFT",
            "off": "TEXT_OFF", "on": "ON", "accent_hi": "ACCENT_HI",
            "ok": "OK", "bad": "BAD", "border": "BORDER_HI"}
    key = base.get(role, "TEXT_DIM")
    return T[key], T["TEXT"], T["BORDER_HI"]


# (弱引用控件, 图标名, 角色, 尺寸, 选中色角色)
_BINDINGS = []


def bind(widget, name, role="dim", size=16, checked_role=None):
    """把图标接到控件上并登记：换皮肤时自动重画。"""
    _BINDINGS.append((weakref.ref(widget), name, role, size, checked_role))
    _repaint(_BINDINGS[-1])
    return widget


def _repaint(entry):
    ref, name, role, size, checked_role = entry
    w = ref()
    if w is None:
        return
    col, act, dis = _colors(role)
    chk = theme.TOK[checked_role.upper()] if checked_role else None
    ic = QtGui.QIcon()
    ic.addPixmap(_pixmap(name, col, size), N, OFF)
    ic.addPixmap(_pixmap(name, act, size), A, OFF)
    ic.addPixmap(_pixmap(name, dis, size), D, OFF)
    if chk:
        ic.addPixmap(_pixmap(name, chk, size), N, ON)
        ic.addPixmap(_pixmap(name, chk, size), A, ON)
    w.setIcon(ic)
    w.setIconSize(QtCore.QSize(size, size))


def repaint(widget, name, role="dim", size=16, checked_role=None):
    """状态变了（展开/折叠、播放/暂停）就换图标，不新增登记。"""
    for i, entry in enumerate(_BINDINGS):
        if entry[0]() is widget:
            _BINDINGS[i] = (entry[0], name, entry[2], size, entry[4])
            _repaint(_BINDINGS[i])
            return
    bind(widget, name, role, size, checked_role)


def refresh():
    """换皮肤后重画所有登记过的图标（模块栏的四色状态图标另走一条）。"""
    for entry in list(_BINDINGS):
        if entry[2] != "module":
            _repaint(entry)


def module_icon(name):
    """模块栏用：常规灰 / 悬停白 / 选中强调色 / 禁用暗灰。"""
    body = name if name in _SVG else "对齐"
    ic = QtGui.QIcon()
    T = theme.TOK
    for color, mode, state in (
            (T["TEXT_DIM"], N, OFF),
            (T["TEXT"], A, OFF),
            (T["ACCENT_HI"], N, ON),
            (T["ACCENT_HI"], A, ON),
            (T["BORDER_HI"], D, OFF)):
        ic.addPixmap(_pixmap(body, color), mode, state)
    return ic


def bind_module(widget, name):
    """模块栏按钮：四档状态色（常规/悬停/选中/禁用），皮肤一变就换色。"""
    _BINDINGS.append((weakref.ref(widget), name, "module", 24, None))
    _paint_module(widget, name)
    return widget


def _paint_module(widget, name):
    widget.setIcon(module_icon(name))
    widget.setIconSize(QtCore.QSize(24, 24))


theme.on_change(refresh)


def refresh_modules():
    for entry in list(_BINDINGS):
        if entry[2] == "module":
            w = entry[0]()
            if w is not None:
                _paint_module(w, entry[1])


theme.on_change(refresh_modules)
