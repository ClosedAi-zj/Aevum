# -*- coding: utf-8 -*-
"""起始页：没有素材时，把"下一步能做什么"做成可以直接点的入口。

空白画布上只放一行灰字，用户既不知道能拖什么，也不知道工具其实随时可用。
这里给三件事：打开素材、打开序列文件夹、最近项目；再加一排工具捷径。
"""
import os
import weakref

from PySide6 import QtCore, QtGui, QtWidgets

from reference.gui import icons, theme

_PAGES = []


def _recolor():
    live = []
    for ref in _PAGES:
        page = ref()
        if page is not None:
            page._sync_icons()
            live.append(ref)
    _PAGES[:] = live


theme.on_change(_recolor)


class _Card(QtWidgets.QFrame):
    clicked = QtCore.Signal()

    def __init__(self, icon_name, title, hint, primary=False, parent=None):
        super().__init__(parent)
        self._icon_name = icon_name
        self._primary = primary
        self.setObjectName("StartCard")
        self.setAttribute(QtCore.Qt.WidgetAttribute.WA_StyledBackground)
        self.setAttribute(QtCore.Qt.WidgetAttribute.WA_Hover)
        self.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self.setMinimumHeight(92)

        lay = QtWidgets.QHBoxLayout(self)
        lay.setContentsMargins(18, 14, 18, 14)
        lay.setSpacing(14)
        self.icon = QtWidgets.QLabel(self)
        self.icon.setFixedSize(30, 30)
        lay.addWidget(self.icon, 0, QtCore.Qt.AlignmentFlag.AlignVCenter)

        col = QtWidgets.QVBoxLayout()
        col.setSpacing(3)
        t = QtWidgets.QLabel(title, self)
        t.setObjectName("StartCardTitle")
        h = QtWidgets.QLabel(hint, self)
        h.setObjectName("StartCardHint")
        for w in (t, h):
            w.setAttribute(QtCore.Qt.WidgetAttribute.WA_TransparentForMouseEvents)
            col.addWidget(w)
        lay.addLayout(col, 1)
        self._sync_icon()

    def _sync_icon(self):
        role = theme.ACCENT_HI if self._primary else theme.TEXT_DIM
        self.icon.setPixmap(icons.icon(self._icon_name, role, 28).pixmap(28, 28))

    def mouseReleaseEvent(self, e):
        if (e.button() == QtCore.Qt.MouseButton.LeftButton
                and self.rect().contains(e.position().toPoint())):
            self.clicked.emit()
            e.accept()
            return
        super().mouseReleaseEvent(e)


class StartPage(QtWidgets.QWidget):
    open_media = QtCore.Signal()
    open_folder = QtCore.Signal()
    recent = QtCore.Signal(str)
    tool = QtCore.Signal(str)

    def __init__(self, parent=None):
        super().__init__(parent)
        _PAGES.append(weakref.ref(self))
        self.setObjectName("StartPage")

        outer = QtWidgets.QVBoxLayout(self)
        outer.setContentsMargins(40, 24, 40, 24)
        outer.setSpacing(0)
        outer.addStretch(1)

        box = QtWidgets.QWidget(self)
        box.setMaximumWidth(760)
        box.setMinimumWidth(520)
        v = QtWidgets.QVBoxLayout(box)
        v.setContentsMargins(0, 0, 0, 0)
        v.setSpacing(10)

        self.lb_title = QtWidgets.QLabel("开始一段延时", box)
        self.lb_title.setObjectName("StartTitle")
        self.lb_sub = QtWidgets.QLabel(
            "照片、RAW、视频和整段序列都在同一个窗口里完成", box)
        self.lb_sub.setObjectName("StartSub")
        v.addWidget(self.lb_title)
        v.addWidget(self.lb_sub)
        v.addSpacing(14)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(12)
        self.card_media = _Card(
            "打开素材", "打开素材", "照片 · RAW · 视频", primary=True)
        self.card_folder = _Card(
            "folder-open", "打开序列文件夹", "整段延时序列按文件名排序")
        self.card_media.clicked.connect(self.open_media)
        self.card_folder.clicked.connect(self.open_folder)
        row.addWidget(self.card_media, 1)
        row.addWidget(self.card_folder, 1)
        v.addLayout(row)
        v.addSpacing(8)

        tools = QtWidgets.QHBoxLayout()
        tools.setSpacing(6)
        self.lb_tools = QtWidgets.QLabel("也可以直接用", box)
        self.lb_tools.setObjectName("StartSection")
        tools.addWidget(self.lb_tools)
        self.tool_btns = []
        for name, tip in (("对齐", "不需要先导入：进去手动选序列目录"),
                          ("合成", "不需要先导入：进去手动选序列目录"),
                          ("裁剪", "图片和视频都可以裁"),
                          ("配乐", "给成片加一段背景音乐"),
                          ("运动", "变速与运动模糊，视频和序列都能吃")):
            b = QtWidgets.QToolButton(box)
            b.setObjectName("StartTool")
            b.setText(name)
            b.setToolButtonStyle(
                QtCore.Qt.ToolButtonStyle.ToolButtonTextBesideIcon)
            icons.bind(b, name, "dim", 15)
            b.setIconSize(QtCore.QSize(15, 15))
            b.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
            b.setToolTip(tip)
            b.clicked.connect(lambda _=False, n=name: self.tool.emit(n))
            tools.addWidget(b)
            self.tool_btns.append(b)
        tools.addStretch(1)
        v.addLayout(tools)

        self.lb_recent = QtWidgets.QLabel("最近打开", box)
        self.lb_recent.setObjectName("StartSection")
        v.addWidget(self.lb_recent)
        self.recent_box = QtWidgets.QWidget(box)
        self.recent_lay = QtWidgets.QVBoxLayout(self.recent_box)
        self.recent_lay.setContentsMargins(0, 0, 0, 0)
        self.recent_lay.setSpacing(2)
        v.addWidget(self.recent_box)
        self.lb_recent.hide()
        self.recent_box.hide()

        self.lb_drop = QtWidgets.QLabel("把文件或文件夹拖进窗口任意位置", box)
        self.lb_drop.setObjectName("StartHint")
        v.addSpacing(10)
        v.addWidget(self.lb_drop)

        outer.addWidget(box, 0, QtCore.Qt.AlignmentFlag.AlignHCenter)
        outer.addStretch(1)

    def _sync_icons(self):
        for card in (self.card_media, self.card_folder):
            card._sync_icon()

    def set_recent(self, paths):
        while self.recent_lay.count():
            item = self.recent_lay.takeAt(0)
            w = item.widget()
            if w is not None:
                # deleteLater 要等事件循环空闲才真删；先 hide，避免新旧
                # 按钮在同一位置叠一帧
                w.hide()
                w.deleteLater()
        paths = [p for p in (paths or ()) if os.path.exists(p)]
        self.lb_recent.setVisible(bool(paths))
        self.recent_box.setVisible(bool(paths))
        for path in paths[:5]:
            name = os.path.basename(path.rstrip("\\/")) or path
            kind = "序列" if os.path.isdir(path) else "文件"
            b = QtWidgets.QPushButton(f"{name}    {kind}", self.recent_box)
            b.setObjectName("StartRecent")
            b.setToolTip(path)
            b.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
            b.clicked.connect(lambda _=False, p=path: self.recent.emit(p))
            self.recent_lay.addWidget(b)
