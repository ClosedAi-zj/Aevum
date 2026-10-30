# -*- coding: utf-8 -*-
"""裁剪工具：图片与视频的空间裁切，外加 Ken Burns 运镜。

选区几何、运镜取景框插值与 ffmpeg 命令都在 lapse/crop.py，这里只管三件事：
把画面显示出来、让用户像 Photoshop 那样框选、把结果写成新文件（原文件一律不动）。
图片直接切像素另存副本；视频走比特流无损 / 重编码，进度与取消归外壳。
运镜是另一条路：每帧都要重画，所以必然重新编码。
"""
import math
import os
import re
import shutil
import subprocess

import numpy as np
from PySide6 import QtCore, QtGui, QtWidgets

from lapse import crop as cp
from lapse import ff as ffmod
from lapse import imio, video
from lapse.ff import NO_WINDOW, unique_path
from lapse.gui import assemble as asm
from lapse.gui import hints
from lapse.gui import job as jobs
from lapse.gui import theme, widgets
from lapse.gui.grader import exporter, pipeline
from lapse.gui.module import Module
from lapse.gui.workbench import APP_TITLE

PREVIEW_LONG = 1280        # 预览帧长边：够看清构图，抽一帧也快
PRESETS = ffmod.PRESET_LABELS
IMG_EXT = tuple(imio.IMG_EXTENSIONS) + tuple(imio.RAW_EXTENSIONS)
CRF_MIN, CRF_MAX, CRF_DEF = 10, 22, 14
CRF_WORDS = ((12, "接近无损"), (16, "视觉无损"), (19, "轻微痕迹"), (99, "细节开始丢"))
# 三种输出模式各自对应的讲解条目（文案在 hints 里，界面只管挂）
MODE_KEYS = {"copy": "裁剪/比特流无损", "lossless": "裁剪/数学无损",
             "crf": "裁剪/视觉无损"}
KB_CR_DEF = 17             # 运镜成片是被重新画过的，比静态裁剪多留一档余量
KB_FPS_DEF = 30
KB_SEC_DEF = 6.0
KB_SIDE_DEF = "3840 (UHD 4K)"
KB_DIGITS = 5              # kb00000.jpg：前缀加编号才认得出是一个序列
KB_RENDER_SPAN = 0.7       # 一项里"重画帧"占的进度权重，剩下的给编码
KB_SHRINK = 0.15           # 默认终点往里收的比例：一打开就该看到镜头在动


def kb_num(label, default):
    """从 "3840 (UHD 4K)" 或 "30" 这种标签里取前导数字，取不到就按默认。"""
    m = re.search(r"\d+", str(label))
    return int(m.group()) if m else default


def crf_word(v):
    for lim, w in CRF_WORDS:
        if v <= lim:
            return w
    return "细节开始丢"


class CropView(QtWidgets.QWidget):
    """中间帧 + 可拖拽选区：框内移动、边/角调整（定比例精确锁比）、方向键微调。

    运镜时另一个取景框也画在这里（虚线 + 指向它的箭头），否则用户看不出
    镜头要走多远、往哪儿走。
    """

    selectionChanged = QtCore.Signal(object)
    kbSwitchRequested = QtCore.Signal()
    HANDLE = 5

    def __init__(self, parent=None):
        super().__init__(parent)
        self.pixmap = None
        self.src_w = 0
        self.src_h = 0
        self.sel = None                 # 源像素 (x, y, w, h)
        self.aspect = None
        self.drag = None                # draw / move / resize
        self.anchor = None
        self.handle = None
        self.orig = None
        self.move_off = None
        self.kb_alt = None              # 运镜里当前没在改的那个框
        self.kb_alt_name = ""
        self.setMinimumSize(320, 180)
        self.setMouseTracking(True)
        self.setFocusPolicy(QtCore.Qt.FocusPolicy.StrongFocus)

    # ---- 数据 ----
    def set_frame(self, pm, w, h):
        """pm 是预览帧，w/h 是它的显示源尺寸（probe 出来的 w/h，不含裁切窗口）。"""
        self.pixmap = pm
        self.src_w, self.src_h = w, h
        self.update()

    def set_sel(self, sel, emit=True):
        self.sel = sel
        if sel is not None:
            self._sync(sel)
        else:
            self.update()

    def set_kb_alt(self, rect, name=""):
        self.kb_alt = rect
        self.kb_alt_name = name
        self.update()

    def _sync(self, sel):
        self.selectionChanged.emit(sel)
        self.update()

    # ---- 坐标换算 ----
    def img_rect(self):
        """画面在控件里的实际位置：等比适配、居中，上下可能留黑边。"""
        if self.pixmap is None or not self.src_w:
            return QtCore.QRectF()
        r = QtCore.QRectF(self.rect()).adjusted(8, 8, -8, -8)
        s = min(r.width() / self.pixmap.width(), r.height() / self.pixmap.height())
        w, h = self.pixmap.width() * s, self.pixmap.height() * s
        return QtCore.QRectF(r.x() + (r.width() - w) / 2,
                             r.y() + (r.height() - h) / 2, w, h)

    def scale(self):
        r = self.img_rect()
        return r.width() / self.src_w if self.src_w else 1.0

    def to_src(self, pt):
        r, s = self.img_rect(), self.scale()
        return ((pt.x() - r.x()) / s, (pt.y() - r.y()) / s)

    def view_rect(self, rect):
        """源像素矩形 -> 屏幕矩形。"""
        r, s = self.img_rect(), self.scale()
        x, y, w, h = rect
        return QtCore.QRectF(r.x() + x * s, r.y() + y * s, w * s, h * s)

    def sel_view(self):
        return self.view_rect(self.sel)

    # ---- 交互 ----
    def mousePressEvent(self, e):
        if self.pixmap is None or e.button() != QtCore.Qt.MouseButton.LeftButton:
            return
        self.setFocus()
        pos = e.position()
        if self.sel is not None and self.kb_alt:
            # 点到另一个框上就先切过去改它：运镜要来回调两个框，
            # 每次都得去右侧点切换按钮太割裂
            on_sel = cp.hit_test(tuple(self.sel_view().getRect()), pos.x(), pos.y())
            on_alt = cp.hit_test(tuple(self.view_rect(self.kb_alt).getRect()),
                                 pos.x(), pos.y())
            if on_alt != "out" and on_sel == "out":
                self.kbSwitchRequested.emit()
                # 不 return：切过去之后这次按下直接落在新选区上，接着拖
                if self.sel is None:
                    return
        sx, sy = self.to_src(pos)
        W, H = self.src_w, self.src_h
        if self.sel is None:
            self.drag = "draw"
            self.anchor = (cp.even(sx), cp.even(sy))
            self.sel = cp.draw_rect(self.anchor, sx, sy, W, H, self.aspect)
        else:
            hit = cp.hit_test(tuple(self.sel_view().getRect()), pos.x(), pos.y())
            if hit.startswith("corner:") or hit.startswith("edge:"):
                self.drag = "resize"
                self.handle = hit.split(":")[-1]
                self.orig = self.sel
            elif hit == "in":
                self.drag = "move"
                self.move_off = (sx - self.sel[0], sy - self.sel[1])
            else:
                self.drag = "draw"
                self.anchor = (cp.even(sx), cp.even(sy))
                self.sel = cp.draw_rect(self.anchor, sx, sy, W, H, self.aspect)
        self._sync(self.sel)

    def mouseMoveEvent(self, e):
        if self.pixmap is None:
            return
        if self.drag:
            sx, sy = self.to_src(e.position())
            W, H = self.src_w, self.src_h
            if self.drag == "draw":
                self.sel = cp.draw_rect(self.anchor, sx, sy, W, H, self.aspect)
            elif self.drag == "move":
                self.sel = cp.move_rect(self.sel, sx - self.move_off[0],
                                        sy - self.move_off[1], W, H)
            else:
                self.sel = cp.resize_rect(self.handle, self.orig, sx, sy,
                                          W, H, self.aspect)
            self._sync(self.sel)
        else:
            self._set_cursor(self._hit(e.position()))

    def mouseReleaseEvent(self, e):
        if not self.drag:
            return
        self.drag = self.anchor = self.handle = self.orig = self.move_off = None
        if self.sel:
            self.sel = cp.norm_rect(*self.sel[:2], self.sel[0] + self.sel[2],
                                    self.sel[1] + self.sel[3],
                                    self.src_w, self.src_h)
            self._sync(self.sel)

    def _hit(self, pt):
        if self.sel is None:
            return "out"
        return cp.hit_test(tuple(self.sel_view().getRect()), pt.x(), pt.y())

    CURSORS = {
        "in": QtCore.Qt.CursorShape.SizeAllCursor,
        "out": QtCore.Qt.CursorShape.CrossCursor,
        "nw": QtCore.Qt.CursorShape.SizeFDiagCursor,
        "se": QtCore.Qt.CursorShape.SizeFDiagCursor,
        "ne": QtCore.Qt.CursorShape.SizeBDiagCursor,
        "sw": QtCore.Qt.CursorShape.SizeBDiagCursor,
        "n": QtCore.Qt.CursorShape.SizeVerCursor,
        "s": QtCore.Qt.CursorShape.SizeVerCursor,
        "e": QtCore.Qt.CursorShape.SizeHorCursor,
        "w": QtCore.Qt.CursorShape.SizeHorCursor,
    }

    def _set_cursor(self, hit):
        self.setCursor(self.CURSORS[hit.split(":")[-1] if ":" in hit else hit])

    def keyPressEvent(self, e):
        if self.sel and e.key() in (QtCore.Qt.Key.Key_Left, QtCore.Qt.Key.Key_Right,
                                    QtCore.Qt.Key.Key_Up, QtCore.Qt.Key.Key_Down):
            step = 20 if e.modifiers() & QtCore.Qt.KeyboardModifier.ShiftModifier else 2
            d = {QtCore.Qt.Key.Key_Left: (-1, 0), QtCore.Qt.Key.Key_Right: (1, 0),
                 QtCore.Qt.Key.Key_Up: (0, -1), QtCore.Qt.Key.Key_Down: (0, 1)}[e.key()]
            self.sel = cp.nudge_rect(self.sel, d[0], d[1], step,
                                     self.src_w, self.src_h)
            self._sync(self.sel)
            return
        super().keyPressEvent(e)

    # ---- 绘制 ----
    def paintEvent(self, _e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)
        p.fillRect(self.rect(), QtGui.QColor(theme.FIELD))
        if self.pixmap is None:
            p.setPen(QtGui.QColor(theme.TEXT_DIM))
            p.drawText(self.rect(), QtCore.Qt.AlignmentFlag.AlignCenter,
                       "选择图片或视频后：空白处拖出新选区，框内拖动移动，"
                       "边/角拖动调整大小，方向键微调（Shift 加速）")
            return
        r = self.img_rect()
        p.setRenderHint(QtGui.QPainter.RenderHint.SmoothPixmapTransform)
        p.drawPixmap(r.toRect(), self.pixmap)
        if self.sel is None:
            return
        s = self.sel_view()
        full = self.rect()
        dim = QtGui.QColor(0, 0, 0, 150)
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(dim)
        for rc in (QtCore.QRectF(0, 0, full.width(), s.top()),
                   QtCore.QRectF(0, s.bottom(), full.width(),
                                 full.height() - s.bottom()),
                   QtCore.QRectF(0, s.top(), s.left(), s.height()),
                   QtCore.QRectF(s.right(), s.top(),
                                 full.width() - s.right(), s.height())):
            p.drawRect(rc)
        # 三分构图线：延时片构图全靠它，选区里不看等于闭眼裁
        p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
        p.setPen(QtGui.QPen(QtGui.QColor(255, 255, 255, 55), 1))
        for k in (1, 2):
            p.drawLine(QtCore.QPointF(s.left() + s.width() * k / 3, s.top()),
                       QtCore.QPointF(s.left() + s.width() * k / 3, s.bottom()))
            p.drawLine(QtCore.QPointF(s.left(), s.top() + s.height() * k / 3),
                       QtCore.QPointF(s.right(), s.top() + s.height() * k / 3))
        p.setPen(QtGui.QPen(QtGui.QColor(theme.ACCENT), 2))
        p.drawRect(s)
        hs = self.HANDLE
        p.setPen(QtGui.QPen(QtGui.QColor("#000000"), 1))
        p.setBrush(QtGui.QColor(theme.HANDLE))
        for hx, hy in self.handle_pts(s):
            p.drawRect(QtCore.QRectF(hx - hs, hy - hs, hs * 2, hs * 2))
        txt = f"{self.sel[2]}×{self.sel[3]}"
        p.setPen(QtGui.QColor(theme.ACCENT_HI))
        ty = s.top() - 6
        if ty < 14:
            ty = s.bottom() + 14
        p.drawText(QtCore.QPointF(s.left(), ty), txt)
        self._paint_kb(p, s)

    def _paint_kb(self, p, s):
        """另一个取景框画成虚线，再从当前框中心引一支箭头过去。

        运镜的方向和幅度光看两个数字没有概念，看这条线一眼就懂。
        """
        if not self.kb_alt:
            return
        a = self.view_rect(self.kb_alt)
        col = QtGui.QColor(theme.ACCENT_DIM)
        p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
        p.setPen(QtGui.QPen(col, 1.5, QtCore.Qt.PenStyle.DashLine))
        p.drawRect(a)
        c1, c2 = s.center(), a.center()
        dx, dy = c2.x() - c1.x(), c2.y() - c1.y()
        ln = math.hypot(dx, dy)
        p.setPen(QtGui.QPen(col, 1.4))
        p.drawLine(c1, c2)
        if ln > 18:
            ux, uy = dx / ln, dy / ln
            for r, i in ((-0.87, 0.5), (-0.87, -0.5)):     # 两支 30° 倒钩
                p.drawLine(c2, QtCore.QPointF(c2.x() + (ux * r - uy * i) * 13,
                                              c2.y() + (ux * i + uy * r) * 13))
        p.setPen(col)
        ty = a.top() - 6
        if ty < 14:
            ty = a.bottom() + 14
        p.drawText(QtCore.QPointF(a.left(), ty),
                   self.kb_alt_name or "另一端")

    @staticmethod
    def handle_pts(s):
        cx, cy = s.center().x(), s.center().y()
        return [(s.left(), s.top()), (cx, s.top()), (s.right(), s.top()),
                (s.right(), cy), (s.right(), s.bottom()), (cx, s.bottom()),
                (s.left(), s.bottom()), (s.left(), cy)]


class Cropper(Module):
    name = "裁剪"

    def __init__(self, shell):
        super().__init__(shell)
        self.info = None
        self.is_image = False
        self.out_manual = False
        self._own_out = None            # 自己刚写出的文件，别被流水线灌回来重载
        self._stage = ""
        self._run_info = None       # 这次裁剪的选区与方式，结果卡用它讲数字
        self.kb_a = None                # 运镜起点框（源像素）
        self.kb_b = None                # 运镜终点框
        self.kb_edit = "a"              # 画布现在在改哪一个

    def top_hint(self):
        """顶栏只报状态：源素材规格 + 走到哪一步；旋钮的说明在控件下面那行。"""
        if not self.info:
            return ""
        i = self.info
        base = (f"{i['w']}×{i['h']}" if i.get("image")
                else f"{i['w']}×{i['h']} · {i['dur']:.1f}s")
        return f"{base} · {self._stage}" if self._stage else base

    # ------------------------------------------------------------------ UI
    def build(self):
        page = QtWidgets.QWidget()
        v = QtWidgets.QVBoxLayout(page)
        v.setContentsMargins(14, 12, 14, 12)
        v.setSpacing(10)

        top = QtWidgets.QHBoxLayout()
        top.setSpacing(8)
        self.btn_file = widgets.icon_button(
            "folder-open", "选择图片或视频（Ctrl+O）")
        self.btn_file.clicked.connect(self.choose_file)
        self.ed_path = QtWidgets.QLineEdit()
        self.ed_path.setPlaceholderText("图片 / 视频路径；也可以把素材直接拖进窗口")
        self.btn_reload = widgets.icon_button(
            "refresh", "重新读取当前素材")
        self.btn_reload.clicked.connect(lambda: self.load(self.ed_path.text()))
        self.btn_preview = widgets.icon_button(
            "play", "到工作台查看（视频可实时套调色参数）")
        self.btn_preview.clicked.connect(self.play_in_grader)
        top.addWidget(self.btn_file)
        top.addWidget(self.ed_path, 1)
        top.addWidget(self.btn_preview)
        top.addWidget(self.btn_reload)
        v.addLayout(top)

        self.lb_info = QtWidgets.QLabel("（未选择视频）")
        self.lb_info.setObjectName("Info")
        self.lb_info.setWordWrap(True)
        v.addWidget(self.lb_info)

        body = QtWidgets.QHBoxLayout()
        body.setSpacing(12)

        left = QtWidgets.QVBoxLayout()
        left.setSpacing(10)
        self.view = CropView()
        self.view.selectionChanged.connect(self._on_sel)
        left.addWidget(self.view, 3)
        self.log_view = QtWidgets.QPlainTextEdit()
        self.log_view.setObjectName("Log")
        self.log_view.setReadOnly(True)
        self.log_view.setPlaceholderText(
            "选区坐标、执行命令与进度写在这里。")
        self.log_view.setLineWrapMode(
            QtWidgets.QPlainTextEdit.LineWrapMode.NoWrap)
        self.log_view.setMinimumHeight(88)
        self.result = widgets.ResultCard("裁剪结果")
        self.result.set_detail(self.log_view)
        self.result.open_requested.connect(widgets.reveal)
        left.addWidget(self.result)
        body.addLayout(left, 1)

        insp = QtWidgets.QWidget()
        side = QtWidgets.QVBoxLayout(insp)
        side.setContentsMargins(0, 0, 4, 0)
        side.setSpacing(10)

        bar = QtWidgets.QHBoxLayout()
        bar.setSpacing(6)
        self.btn_run = QtWidgets.QPushButton("开始裁剪")
        self.btn_run.setObjectName("Primary")
        widgets.label_button(self.btn_run, "裁剪", role="on_accent")
        self.btn_run.clicked.connect(self.run)
        self.btn_open = widgets.icon_button(
            "folder-open", "在文件管理器里打开输出目录")
        self.btn_open.setEnabled(False)
        self.btn_open.clicked.connect(self.open_out)
        bar.addWidget(self.btn_run)
        bar.addStretch(1)
        bar.addWidget(self.btn_open)
        side.addLayout(bar)

        side.addWidget(self._build_sel_box())
        self.box_kb = self._build_kb_box()
        side.addWidget(self.box_kb)
        self.box_mode = self._build_mode_box()
        side.addWidget(self.box_mode)
        side.addWidget(self._build_out_box())
        side.addStretch(1)

        scroll = QtWidgets.QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QtWidgets.QFrame.Shape.NoFrame)
        scroll.setHorizontalScrollBarPolicy(
            QtCore.Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        scroll.setWidget(insp)
        scroll.setFixedWidth(374)
        body.addWidget(scroll)
        v.addLayout(body, 1)

        self.ed_path.textChanged.connect(self._path_typed)
        self.view.kbSwitchRequested.connect(self._kb_switch)
        self._debounce = QtCore.QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.setInterval(600)
        self._debounce.timeout.connect(lambda: self.load(self.ed_path.text()))
        self._restore()
        self._sync_mode()
        return page

    def _build_sel_box(self):
        box = widgets.Group("选区")
        g = box.grid()
        g.setHorizontalSpacing(10)
        g.setVerticalSpacing(7)

        row = QtWidgets.QWidget()
        r = QtWidgets.QHBoxLayout(row)
        r.setContentsMargins(0, 0, 0, 0)
        r.setSpacing(6)
        self.cb_aspect = QtWidgets.QComboBox()
        self.cb_aspect.addItems([n for n, _ in cp.ASPECTS])
        self.cb_aspect.setCurrentText("16:9")
        self.cb_aspect.setSizePolicy(QtWidgets.QSizePolicy.Policy.Ignored,
                                     QtWidgets.QSizePolicy.Policy.Fixed)
        hints.attach(self.cb_aspect, "裁剪/比例")
        self.cb_aspect.currentTextChanged.connect(self._aspect_changed)
        self.btn_center = widgets.icon_button(
            "split", "把选区居中到当前比例", box=30, size=17)
        self.btn_center.clicked.connect(self._center)
        r.addWidget(self.cb_aspect, 1)
        r.addWidget(self.btn_center)
        g.addWidget(widgets.field_block(
            "宽高比", row, hints.line("裁剪/比例")), 0, 0, 1, 2)

        self.sp = {}
        nums = QtWidgets.QGridLayout()
        nums.setHorizontalSpacing(6)
        nums.setVerticalSpacing(6)
        for i, (key, name) in enumerate(
                (("x", "X"), ("y", "Y"), ("w", "宽"), ("h", "高"))):
            r, c = divmod(i, 2)
            lb = self._flabel(name)
            lb.setFixedWidth(20)
            b = QtWidgets.QSpinBox()
            b.setRange(0, 99998)
            b.setSingleStep(2)
            b.setButtonSymbols(QtWidgets.QAbstractSpinBox.ButtonSymbols.NoButtons)
            b.setObjectName("ValBox")
            b.setFrame(False)
            b.setMinimumHeight(24)
            b.setAlignment(QtCore.Qt.AlignmentFlag.AlignRight)
            b.editingFinished.connect(self._field_apply)
            hints.attach(b, "裁剪/选区", label=lb)
            nums.addWidget(lb, r, c * 2)
            nums.addWidget(b, r, c * 2 + 1)
            self.sp[key] = b
        nums.setColumnStretch(1, 1)
        nums.setColumnStretch(3, 1)
        g.addLayout(nums, 1, 0, 1, 2)

        sel = QtWidgets.QLabel(hints.line("裁剪/选区"))
        sel.setObjectName("FieldHint")
        sel.setWordWrap(True)
        g.addWidget(sel, 2, 0, 1, 2)
        self.lb_out = QtWidgets.QLabel("")
        self.lb_out.setObjectName("FieldHint")
        self.lb_out.setWordWrap(True)
        g.addWidget(self.lb_out, 3, 0, 1, 2)
        g.setColumnStretch(0, 1)
        g.setColumnStretch(1, 1)
        return box

    def _flabel(self, text):
        lb = QtWidgets.QLabel(text)
        lb.setObjectName("FieldLabel")
        return lb

    def _build_kb_box(self):
        """运镜一组：开关 + 起止两个框 + 长度/缓动/成片规格。

        勾掉之后整组参数收起来，「输出模式」那组再长回去 —— 运镜每帧都要
        重画，无损那条路走不通，两组同时摆着只会让人以为能一起用。
        """
        box = widgets.Group("运镜（Ken Burns）")
        v = box.vbox(spacing=7)
        self.ck_kb = QtWidgets.QCheckBox("给这段画面加推拉摇移")
        hints.attach(self.ck_kb, "裁剪/运镜")
        v.addWidget(self.ck_kb)

        body = QtWidgets.QWidget()
        b = QtWidgets.QVBoxLayout(body)
        b.setContentsMargins(0, 0, 0, 0)
        b.setSpacing(7)

        edit = QtWidgets.QWidget()
        r = QtWidgets.QHBoxLayout(edit)
        r.setContentsMargins(0, 0, 0, 0)
        r.setSpacing(6)
        grp = QtWidgets.QButtonGroup(edit)
        grp.setExclusive(True)
        self.kb_btns = {}
        for key, text in (("a", "改起点"), ("b", "改终点")):
            btn = QtWidgets.QToolButton()
            btn.setText(text)
            btn.setCheckable(True)
            btn.setChecked(key == "a")
            hints.attach(btn, "裁剪/运镜端点")
            grp.addButton(btn)
            r.addWidget(btn, 1)
            self.kb_btns[key] = btn
            btn.clicked.connect(lambda _c=False, k=key: self._kb_use(k))
        b.addWidget(edit)
        self.lb_kb_rects = QtWidgets.QLabel("")
        self.lb_kb_rects.setObjectName("FieldHint")
        self.lb_kb_rects.setWordWrap(True)
        b.addWidget(self.lb_kb_rects)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        row.addWidget(self._flabel("时长"))
        self.sp_kb_sec = QtWidgets.QDoubleSpinBox()
        self.sp_kb_sec.setRange(0.5, 600.0)
        self.sp_kb_sec.setSingleStep(0.5)
        self.sp_kb_sec.setValue(KB_SEC_DEF)
        self.sp_kb_sec.setSuffix(" 秒")
        self.sp_kb_sec.setButtonSymbols(
            QtWidgets.QAbstractSpinBox.ButtonSymbols.NoButtons)
        self.sp_kb_sec.setObjectName("ValBox")
        self.sp_kb_sec.setAlignment(QtCore.Qt.AlignmentFlag.AlignRight)
        hints.attach(self.sp_kb_sec, "裁剪/运镜时长")
        self.sp_kb_sec.valueChanged.connect(self._kb_note)
        row.addWidget(self.sp_kb_sec, 1)
        row.addWidget(self._flabel("帧率"))
        self.cb_kb_fps = self._kb_combo(asm.FPS_CHOICES, str(KB_FPS_DEF))
        hints.attach(self.cb_kb_fps, "裁剪/运镜时长")
        self.cb_kb_fps.currentTextChanged.connect(self._kb_note)
        row.addWidget(self.cb_kb_fps, 1)
        b.addLayout(row)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        row.addWidget(self._flabel("缓动"))
        self.cb_kb_ease = QtWidgets.QComboBox()
        for key, label in cp.KB_EASES:
            self.cb_kb_ease.addItem(label, key)
        hints.attach(self.cb_kb_ease, "裁剪/运镜缓动")
        row.addWidget(self.cb_kb_ease, 1)
        row.addWidget(self._flabel("长边"))
        self.cb_kb_side = self._kb_combo(asm.SIDE_CHOICES, KB_SIDE_DEF)
        hints.attach(self.cb_kb_side, "裁剪/运镜尺寸")
        self.cb_kb_side.currentTextChanged.connect(self._kb_note)
        row.addWidget(self.cb_kb_side, 1)
        b.addLayout(row)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        row.addWidget(self._flabel("编码"))
        self.cb_kb_codec = QtWidgets.QComboBox()
        self.cb_kb_codec.addItems([t for t, _ in asm.CODEC_CHOICES])
        hints.attach(self.cb_kb_codec, "裁剪/运镜尺寸")
        row.addWidget(self.cb_kb_codec, 1)
        b.addLayout(row)

        q = QtWidgets.QHBoxLayout()
        q.setSpacing(6)
        q.addWidget(self._flabel("CRF"))
        self.kb_sl_crf = widgets.Slider()
        self.kb_sl_crf.setRange(CRF_MIN, CRF_MAX)
        self.kb_sl_crf.setValue(KB_CR_DEF)
        hints.attach(self.kb_sl_crf, "裁剪/质量")
        self.kb_sl_crf.valueChanged.connect(
            lambda v: self.kb_lb_crf.setText(f"{v}  {crf_word(v)}"))
        q.addWidget(self.kb_sl_crf, 1)
        self.kb_lb_crf = QtWidgets.QLabel()
        self.kb_lb_crf.setObjectName("ValueLabel")
        hints.attach(self.kb_lb_crf, "裁剪/质量")
        q.addWidget(self.kb_lb_crf)
        b.addLayout(q)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        row.addWidget(self._flabel("编码速度"))
        self.kb_cb_preset = QtWidgets.QComboBox()
        for label, value in PRESETS:
            self.kb_cb_preset.addItem(label, value)
        self.kb_cb_preset.setCurrentIndex(self.kb_cb_preset.findData("medium"))
        hints.attach(self.kb_cb_preset, "裁剪/编码速度")
        row.addWidget(self.kb_cb_preset, 1)
        b.addLayout(row)

        self.lb_kb = QtWidgets.QLabel(hints.line("裁剪/运镜"))
        self.lb_kb.setObjectName("FieldHint")
        self.lb_kb.setWordWrap(True)
        b.addWidget(self.lb_kb)
        body.setVisible(False)
        v.addWidget(body)
        self.kb_body = body
        # 等整组控件都建好了再连：_kb_on_changed 会读写上面这一排
        self.ck_kb.toggled.connect(self._kb_on_changed)
        self.kb_lb_crf.setText(f"{KB_CR_DEF}  {crf_word(KB_CR_DEF)}")
        return box

    def _kb_combo(self, items, current):
        """可输入的档位下拉：列表外的大小/帧率也让人填得进去（跟合成页一致）。"""
        cb = QtWidgets.QComboBox()
        cb.setEditable(True)
        cb.addItems([str(x) for x in items])
        i = cb.findText(str(current))
        cb.setCurrentIndex(i if i >= 0 else 0)
        return cb

    # ------------------------------------------------------------------ 运镜
    def _kb_on(self):
        return self.ck_kb.isChecked()

    def _kb_on_changed(self, on):
        """勾上才长出这一组参数，同时把「输出模式」收掉：运镜必然逐帧重画，
        无损那三条路一条都走不通，两组同时摆着只会让人以为能一起用。"""
        self.kb_body.setVisible(on)
        self.cb_aspect.model().item(0).setEnabled(not on)     # "自由"
        if on and self.view.aspect is None:
            self.cb_aspect.setCurrentText("16:9")   # 触发 _aspect_changed 重锁两框
        self._sync_mode()
        self.out_manual = False
        self._refresh_out()
        if self.info:
            if on:
                self._kb_init_rects()
            else:
                self.view.set_kb_alt(None)
                self.view.set_sel(cp.center_rect(
                    self.info["w"], self.info["h"], self.view.aspect))

    def _kb_init_rects(self):
        """起点铺满当前比例，终点往里收一点：默认就是"向前推进"，一开就能
        看到镜头在动，而不是两个重合的框。"""
        W, H = self.info["w"], self.info["h"]
        if self.kb_a is None:
            self.kb_a = cp.center_rect(W, H, self.view.aspect)
        if self.kb_b is None:
            x, y, w, h = self.kb_a
            w2 = max(2, cp.even(w * (1.0 - KB_SHRINK)))
            h2 = max(2, cp.even(h * (1.0 - KB_SHRINK)))
            self.kb_b = cp.norm_rect(x + (w - w2) / 2.0, y + (h - h2) / 2.0,
                                     x + (w + w2) / 2.0, y + (h + h2) / 2.0, W, H)
        self._kb_show_endpoints()

    def _kb_use(self, key):
        self.kb_edit = key
        self._kb_show_endpoints()

    def _kb_switch(self):
        self._kb_use("b" if self.kb_edit == "a" else "a")

    def _kb_show_endpoints(self):
        """把"正在改的那个框"当作选区显示，另一个画成虚线并标出它是起点还是终点。"""
        other = "b" if self.kb_edit == "a" else "a"
        self.view.set_kb_alt(getattr(self, "kb_" + other),
                             "终点" if other == "b" else "起点")
        cur = getattr(self, "kb_" + self.kb_edit)
        if cur is not None:
            self.view.set_sel(cur)
        self.kb_btns[self.kb_edit].setChecked(True)
        self._kb_note()

    def _kb_fps(self):
        return max(1, kb_num(self.cb_kb_fps.currentText(), KB_FPS_DEF))

    def _kb_frames(self):
        """图片按"想要几秒"算帧数；视频一条片子有多长是素材定的，
        运镜只改编框不改时长。"""
        fps = self._kb_fps()
        if self.info and not self.is_image:
            return max(2, int(round(self.info["dur"] * fps)))
        return max(2, int(round(self.sp_kb_sec.value() * fps)))

    def _kb_note(self):
        """把这次运镜到底产出什么写成一行数字，光看控件猜不出来。"""
        if not self._kb_on():
            self.lb_kb.setText(hints.line("裁剪/运镜"))
            return
        self.sp_kb_sec.setEnabled(self.info is None or self.is_image)
        if not self.info or not self.kb_a or not self.kb_b:
            self.lb_kb.setText(hints.line("裁剪/运镜端点"))
            return
        self.lb_kb_rects.setText(
            f"起点 {self.kb_a[2]}×{self.kb_a[3]} · "
            f"终点 {self.kb_b[2]}×{self.kb_b[3]}（现在改的是"
            f"{'起点' if self.kb_edit == 'a' else '终点'}）")
        fps = self._kb_fps()
        n = self._kb_frames()
        w, h = cp.kb_size(self.kb_b, kb_num(self.cb_kb_side.currentText(), 3840))
        z = cp.kb_zoom(self.kb_b, w, h) / cp.kb_zoom(self.kb_a, w, h)
        if 0.99 <= z <= 1.01:
            move = "只平移，画面大小不变"
        elif z > 1.0:
            move = f"推进 {z:.2f}×"
        else:
            move = f"拉远 {1.0 / z:.2f}×"
        self.lb_kb.setText(f"{n} 帧 · {n / float(fps):.1f}s @ {fps}fps · "
                           f"成片 {w}×{h} · {move}")

    def _kb_plan(self, dst):
        """主线程把控件读干净，worker 只拿这份快照（跨线程读控件是老坑）。"""
        W, H = self.info["w"], self.info["h"]
        side = kb_num(self.cb_kb_side.currentText(), 3840)
        # 成片比例跟终点框走：框按哪个比例拉的，片子就是哪个比例
        w, h = cp.kb_size(self.kb_b, side)
        fps = self._kb_fps()
        n = self._kb_frames()
        codec = dict(asm.CODEC_CHOICES).get(
            self.cb_kb_codec.currentText(), "hevc")
        crf = int(self.kb_sl_crf.value())
        preset = self.kb_cb_preset.currentData() or "medium"
        cmd, _ow, _oh = video.build_cmd(
            self.shell.ff, os.path.splitext(dst)[0] + "_运镜帧", dst, fps,
            None, w, crf, preset, codec,
            f"kb%0{KB_DIGITS}d.jpg", 0, n, w, h, 0, False, progress=True,
            audio_src=None if self.is_image else self.info["path"])
        return {"src": self.info["path"], "image": self.is_image,
                "ff": self.shell.ff,
                "W": W, "H": H, "a": self.kb_a, "b": self.kb_b, "n": n,
                "fps": fps, "ease": self.cb_kb_ease.currentData() or cp.KB_EASE_DEF,
                "w": w, "h": h, "codec": codec, "crf": crf, "preset": preset,
                "out": dst,
                "frames_dir": os.path.splitext(dst)[0] + "_运镜帧", "cmd": cmd}

    def _build_mode_box(self):
        box = widgets.Group("输出模式")
        v = box.vbox(spacing=2)
        self.rb = {}
        for key, title, desc in cp.MODES:
            # QRadioButton 不会换行，说明单独一行小字缩到标题下面，点标题照样选
            r = QtWidgets.QRadioButton(title)
            hints.attach(r, MODE_KEYS[key])
            v.addWidget(r)
            self.rb[key] = r
            d = QtWidgets.QLabel(desc)
            d.setObjectName("ModeHint")
            d.setWordWrap(True)
            hints.attach(d, MODE_KEYS[key])
            v.addWidget(d)
        self.rb["copy"].setChecked(True)

        self.cr_row = QtWidgets.QWidget()
        cl = QtWidgets.QHBoxLayout(self.cr_row)
        cl.setContentsMargins(0, 0, 0, 0)
        cl.addWidget(self._flabel("CRF"))
        self.sl_crf = widgets.Slider()
        self.sl_crf.setRange(CRF_MIN, CRF_MAX)
        self.sl_crf.setValue(CRF_DEF)
        hints.attach(self.sl_crf, "裁剪/质量")
        self.sl_crf.valueChanged.connect(self._on_crf)
        cl.addWidget(self.sl_crf, 1)
        self.lb_crf = QtWidgets.QLabel()
        self.lb_crf.setObjectName("ValueLabel")
        hints.attach(self.lb_crf, "裁剪/质量")
        cl.addWidget(self.lb_crf)
        v.addWidget(self.cr_row)

        pr = QtWidgets.QHBoxLayout()
        pr.addWidget(self._flabel("编码速度"))
        self.cb_preset = QtWidgets.QComboBox()
        for label, value in PRESETS:
            self.cb_preset.addItem(label, value)
        self.cb_preset.setCurrentIndex(self.cb_preset.findData("medium"))
        hints.attach(self.cb_preset, "裁剪/编码速度")
        pr.addWidget(self.cb_preset, 1)
        v.addLayout(pr)
        # 等 CRF 与编码速度都建好了再连：_sync_mode 会同时读写这一排控件
        for r in self.rb.values():
            r.toggled.connect(self._sync_mode)
        return box

    def _build_out_box(self):
        box = widgets.Group("输出")
        g = box.grid(vspacing=6)
        g.setHorizontalSpacing(10)
        g.setVerticalSpacing(6)
        self.ed_out = QtWidgets.QLineEdit()
        self.ed_out.setPlaceholderText("默认在原文件旁生成 _crop 副本，原文件不动")
        self.ed_out.textEdited.connect(self._mark_manual)
        self.btn_save = QtWidgets.QPushButton("另存为…")
        self.btn_save.setObjectName("ToolBtn")
        self.btn_save.clicked.connect(self.choose_out)
        g.addWidget(self.ed_out, 0, 0)
        g.addWidget(self.btn_save, 0, 1)
        hint = QtWidgets.QLabel("视频无损模式只改写码流窗口，不重新编码，秒级完成")
        hint.setObjectName("FieldHint")
        hint.setWordWrap(True)
        g.addWidget(hint, 1, 0, 1, 2)
        g.setColumnStretch(0, 1)
        self.lb_out_note = hint
        return box

    # ------------------------------------------------------------------ 读取
    def choose_file(self):
        p, _ = QtWidgets.QFileDialog.getOpenFileName(
            self.shell, "选择图片或视频", "",
            "图片与视频 (*.jpg *.jpeg *.png *.tif *.tiff *.bmp *.webp "
            "*.rw2 *.cr2 *.cr3 *.nef *.arw *.raf *.orf *.dng *.pef *.srw "
            "*.mp4 *.mov *.mkv *.m4v *.avi *.ts *.mts *.m2ts *.webm *.wmv "
            "*.hevc *.h264);;图片 (*.jpg *.jpeg *.png *.tif *.tiff *.bmp "
            "*.webp *.rw2 *.cr2 *.cr3 *.nef *.arw *.raf *.orf *.dng);;"
            "视频 (*.mp4 *.mov *.mkv *.m4v *.avi *.ts *.mts *.m2ts *.webm "
            "*.wmv *.hevc *.h264);;所有文件 *.*")
        if p:
            self.load(p)

    def _path_typed(self, text):
        if text.strip():
            self._debounce.start()

    def load(self, path):
        path = (path or "").strip().strip('"')
        if not path or not os.path.isfile(path):
            return
        self._debounce.stop()
        ext = os.path.splitext(path)[1].lower()
        if ext in IMG_EXT:
            self._load_image(path)
        else:
            self._load_video(path)

    def _load_image(self, path):
        im = pipeline.load_u8(path)
        if im is None:
            self.lb_info.setText(f"读不到图片：{path}")
            self.lb_info.setObjectName("InfoBad")
            self._repolish(self.lb_info)
            return
        self.is_image = True
        h, w = im.shape[:2]
        small = pipeline.fit_long(im, PREVIEW_LONG)
        pm = QtGui.QPixmap.fromImage(pipeline.to_qimage_u8(small))
        self.info = {"path": path, "w": w, "h": h, "dur": 0.0, "fps": 0.0,
                     "codec": os.path.splitext(path)[1].lstrip(".").upper(),
                     "pixfmt": "", "coded": (w, h), "conf": (0, 0, 0, 0),
                     "rotated": False, "image": True}
        self._show_loaded(pm)

    def _load_video(self, path):
        if not self.shell.ff:
            self._no_ffmpeg()
            return
        bundle = ffmod.import_bundle(self.shell.ff, path)
        info = bundle["info"] if bundle else None
        if not info:
            self.lb_info.setText(f"读不到视频流：{path}")
            self.lb_info.setObjectName("InfoBad")
            self._repolish(self.lb_info)
            return
        self.is_image = False
        info = dict(info)
        # 选区坐标系 = ffmpeg 滤镜看到的那一帧：自动旋转已经把长短边换过来了，
        # 按存储尺寸选区会在竖拍片上把框转歪 90°
        if info.get("angle") in (90, 270):
            info["w"], info["h"] = info["h"], info["w"]
        self.info = info
        pm = self._preview_pixmap(bundle, info)
        if pm.isNull():
            self.warn("抽帧失败，ffmpeg 没吐出预览画面。")
            return
        self._show_loaded(pm)

    def _preview_pixmap(self, bundle, info):
        """选区底下那一帧：能吃导入时解好的就吃，省一次全分辨率解码。

        非方形像素除外——预取那张按显示比例拉长了，而这里的选区坐标系是存储
        像素，两者对不上，必须另解一张。
        """
        pm = QtGui.QPixmap()
        if bundle and bundle.get("png") and \
                tuple(info.get("sar") or (1, 1)) == (1, 1):
            pm.loadFromData(bundle["png"], "PNG")
            return pm
        t = info["dur"] / 2 if info["dur"] > 0 else 0
        scale = PREVIEW_LONG / max(info["w"], info["h"]) if \
            max(info["w"], info["h"]) > PREVIEW_LONG else 1.0
        r = ffmod.run([self.shell.ff, "-hide_banner", "-loglevel", "error",
                       "-ss", f"{t:.3f}", "-i", info["path"], "-frames:v", "1",
                       "-vf", f"scale=floor(iw*{scale:.6f}/2)*2:"
                              f"floor(ih*{scale:.6f}/2)*2",
                       "-f", "image2pipe", "-c:v", "png", "pipe:1"])
        if r.stdout:
            pm.loadFromData(r.stdout, "PNG")
        return pm

    def _show_loaded(self, pm):
        path = self.info["path"]
        ed = self.ed_path
        if os.path.normpath(path) != os.path.normpath(ed.text()):
            ed.blockSignals(True)
            ed.setText(os.path.normpath(path))
            ed.blockSignals(False)
        self.lb_info.setObjectName("Info")
        self._repolish(self.lb_info)
        self.lb_info.setText(self._describe(self.info))
        self._stage = ""
        self.sync_page_hint()
        # 预览帧的显示尺寸就是选区坐标系：编码尺寸多出来的边由 conf 处理
        self.view.set_frame(pm, self.info["w"], self.info["h"])
        self._own_out = None
        self.btn_open.setEnabled(False)
        self.btn_run.setText("裁剪并另存" if self.is_image else "开始裁剪")
        self._refresh_out()
        self.view.set_sel(None)
        self.view.set_kb_alt(None)
        self.kb_a = self.kb_b = None    # 换素材，旧画面的取景框尺寸没有意义
        self._aspect_changed()
        self._sync_mode()

    @staticmethod
    def _repolish(w):
        st = w.style()
        st.unpolish(w)
        st.polish(w)

    def _describe(self, info):
        if info.get("image"):
            raw = "  RAW 预览为半尺寸，裁剪按原文件全分辨率输出" \
                if imio.is_raw(info["path"]) else ""
            return (f"{info['codec']}  {info['w']}×{info['h']}  "
                    f"{os.path.getsize(info['path']) / 1048576:.1f} MB"
                    f"{raw}  裁剪只写副本，原图不动")
        conf = "" if info["conf"] == (0, 0, 0, 0) else \
            f"  裁切窗口 {info['conf']}"
        return (f"{info['codec']}  {info['w']}x{info['h']}  "
                f"{info['fps']:.3g}fps  {info['dur']:.1f}s  "
                f"{info['pixfmt'] or '—'}  编码 {info['coded'][0]}x{info['coded'][1]}"
                f"{conf}"
                + ("  ⚠ 含旋转元数据，比特流无损不可用" if info["rotated"] else "")
                + ("  ⚠ 非方形像素，选区按存储像素算"
                   if tuple(info.get("sar") or (1, 1)) != (1, 1) else ""))

    def _refresh_out(self):
        if self.out_manual or not self.info:
            return
        path = self.info["path"]
        stem = os.path.splitext(path)[0]
        if self._kb_on():
            self.ed_out.setText(unique_path(stem + "_运镜.mp4"))
            return
        if self.is_image:
            ext = os.path.splitext(path)[1].lower()
            if ext not in (".png", ".tif", ".tiff"):
                ext = ".jpg"          # RAW / JPG 都出 JPG 副本
        else:
            ext = ".mp4"
        self.ed_out.setText(unique_path(stem + "_crop" + ext))

    def _mark_manual(self, _text):
        self.out_manual = True

    def choose_out(self):
        if self.is_image:
            ext = os.path.splitext(self.ed_out.text())[1].lower() or ".jpg"
            filt = f"图片 (*{ext})"
        else:
            ext, filt = ".mp4", "MP4 视频 (*.mp4)"
        p, _ = QtWidgets.QFileDialog.getSaveFileName(
            self.shell, "输出文件", self.ed_out.text(), filt)
        if p:
            self.out_manual = True
            if not p.lower().endswith(ext):
                p += ext
            self.ed_out.setText(os.path.normpath(p))

    # ------------------------------------------------------------------ 选区
    def _aspect_changed(self, _name=None):
        self.view.aspect = cp.aspect_of(self.cb_aspect.currentText())
        if not self.info:
            return
        W, H = self.info["w"], self.info["h"]
        if self._kb_on():
            # 两个框必须同一个比例，否则逐帧重画会一路把画面拉变形
            if self.kb_a is None or self.kb_b is None:
                self._kb_init_rects()
            else:
                # 一起按比例重铺：只各夹一次会把两个框挤成同一个，运镜幅度归零
                self.kb_a, self.kb_b = cp.kb_refit(
                    self.kb_a, self.kb_b, W, H, self.view.aspect)
                self._kb_show_endpoints()
            return
        sel = cp.center_rect(W, H, self.view.aspect) if self.view.sel is None \
            else cp.refit_rect(self.view.sel, W, H, self.view.aspect)
        self.view.set_sel(sel)

    def _center(self):
        if self.info:
            self.view.set_sel(cp.center_rect(self.info["w"], self.info["h"],
                                             self.view.aspect))

    def _on_sel(self, sel):
        for key, v in zip(("x", "y", "w", "h"), sel):
            b = self.sp[key]
            b.blockSignals(True)
            b.setValue(v)
            b.blockSignals(False)
        self.lb_out.setText(f"输出 {sel[2]}x{sel[3]}（原 "
                            f"{self.info['w']}x{self.info['h']}）")
        if self._kb_on():
            # 画布上正在改的那个框就是当前端点，别攒成两份状态
            setattr(self, "kb_" + self.kb_edit, sel)
            self._kb_note()

    def _field_apply(self):
        if not self.info or self.view.drag:
            return
        x = self.sp["x"].value()
        y = self.sp["y"].value()
        w = self.sp["w"].value()
        h = self.sp["h"].value()
        self.view.set_sel(cp.norm_rect(x, y, x + w, y + h,
                                       self.info["w"], self.info["h"]))

    def _on_crf(self, v):
        self.lb_crf.setText(f"{v}  {crf_word(v)}")
        self._sync_mode()

    def _mode(self):
        for key, r in self.rb.items():
            if r.isChecked():
                return key
        return "copy"

    def _sync_mode(self):
        """图片没有编码模式这一说，整组收起来；视频里 copy 不重编码，
        CRF 与编码速度就该灰掉——能看见的控件必须能用。"""
        if self._kb_on():
            # 运镜每帧重画，无损与比特流都用不上；用它自己那排编码参数
            self.box_mode.setVisible(False)
            self.lb_out_note.setText("运镜会逐帧重画再重新编码，帧目录是中间产物，出片后自动删除")
            self.btn_run.setText("生成运镜成片")
            return
        self.box_mode.setVisible(not self.is_image)
        if self.is_image:
            self.lb_out_note.setText("图片直接切像素另存副本，画质与原图像素一致")
            self.btn_run.setText("裁剪并另存")
            return
        self.lb_out_note.setText("视频无损模式只改写码流窗口，不重新编码，秒级完成")
        self.btn_run.setText("开始裁剪")
        m = self._mode()
        self.rb["copy"].setEnabled(self.info is None
                                   or cp.copy_supported(self.info))
        if self.info is not None and m == "copy" and \
                not cp.copy_supported(self.info):
            self.rb["crf"].setChecked(True)      # 触发 toggled → 再同步一次
            return
        self.cr_row.setEnabled(m == "crf")
        self.cb_preset.setEnabled(m in ("lossless", "crf"))

    def _preset(self):
        return self.cb_preset.currentData() or \
            ffmod.preset_value(self.cb_preset.currentText())

    # ------------------------------------------------------------------ 执行
    def run(self):
        if not self.info:
            self.warn("请先选择一个图片或视频。")
            return
        if self._kb_on():
            self.run_kb()
            return
        if not self.view.sel:
            self.warn("请先在画面上框选要保留的区域。")
            return
        x, y, w, h = self.view.sel
        W, H = self.info["w"], self.info["h"]
        if (w, h) == (W - W % 2, H - H % 2):
            self.warn("选区就是整幅画面，裁了等于没裁。")
            return
        dst = self.ed_out.text().strip().strip('"') \
            or os.path.splitext(self.info["path"])[0] + "_crop"
        if self.is_image:
            if not os.path.splitext(dst)[1]:
                dst += ".jpg"
        elif not dst.lower().endswith(".mp4"):
            dst += ".mp4"          # 没扩展名 ffmpeg 猜不出封装格式
        dst = unique_path(dst)
        od = os.path.dirname(dst)
        if od:
            os.makedirs(od, exist_ok=True)
        self.ed_out.setText(dst)
        if self.is_image:
            self._run_image(dst, (x, y, w, h))
            return
        if not self.shell.ff:
            self._no_ffmpeg()
            return

        m = self._mode()
        if m == "copy":
            cmd = cp.build_copy_cmd(self.shell.ff, self.info["path"], dst,
                                    self.info, self.view.sel)
            desc = "比特流无损（只改写码流裁切窗口）"
        elif m == "lossless":
            cmd = cp.build_encode_cmd(self.shell.ff, self.info["path"], dst,
                                      self.view.sel, True, None, self._preset())
            desc = "数学无损（x265 lossless）"
        else:
            cmd = cp.build_encode_cmd(self.shell.ff, self.info["path"], dst,
                                      self.view.sel, False,
                                      int(self.sl_crf.value()), self._preset())
            desc = f"视觉无损（CRF {int(self.sl_crf.value())}：" \
                   f"{crf_word(int(self.sl_crf.value()))}）"

        self.log(f"模式: {desc}")
        self.log(f"选区: x={x} y={y} {w}x{h}（原 {W}x{H}）")
        self._save()
        self._run_info = {"w": w, "h": h, "W": W, "H": H,
                          "desc": desc, "dur": self.info["dur"]}
        job = jobs.Job([(f"{w}x{h}", cmd,
                          jobs.Meter("time", self.info["dur"]), dst, None)])
        job.log.connect(self.log)
        if self.shell.start_job(job, self._finished, self.result):
            self.btn_run.setEnabled(False)
            self.result.set_status("run", f"裁剪中 · {w}×{h}")
            self.result.set_metrics([])
            self.result.set_path(None)
            self.result.set_next("")

    def _run_image(self, dst, sel):
        """图片裁剪：按原文件全分辨率切像素，写副本，原图不碰。"""
        QtWidgets.QApplication.setOverrideCursor(
            QtCore.Qt.CursorShape.WaitCursor)
        try:
            full = exporter.load_full(self.info["path"])
        finally:
            QtWidgets.QApplication.restoreOverrideCursor()
        if full is None:
            why = "读不到原图：" + self.info["path"]
            self.result.set_status("bad", "裁剪失败", why)
            self.warn("裁剪失败：读不到原图 " + self.info["path"])
            return
        fh, fw = full.shape[:2]
        # 预览可能是缩过的（RAW 半尺寸、长边 1280），选区按比例放大回原图
        sx, sy = fw / self.info["w"], fh / self.info["h"]
        x, y, w, h = sel
        X = min(fw - 2, max(0, int(round(x * sx))))
        Y = min(fh - 2, max(0, int(round(y * sy))))
        W = max(2, min(fw - X, int(round(w * sx))))
        H = max(2, min(fh - Y, int(round(h * sy))))
        crop = full[Y:Y + H, X:X + W]
        ok = exporter.save_image(dst, crop, 95)
        self.log(f"图片: {self.info['w']}x{self.info['h']} → {W}x{H}"
                 f"（原图 {fw}x{fh}）")
        if not ok:
            why = "写不进 " + dst
            self.result.set_status("bad", "裁剪失败", why)
            self.warn("裁剪失败：写不进 " + dst)
            return
        self._own_out = dst
        self.btn_open.setEnabled(True)
        self._stage = "已裁剪"
        self.sync_page_hint()
        self._run_info = {"w": W, "h": H, "W": fw, "H": fh, "dur": None,
                          "desc": "按原图全分辨率切像素（原图不动）"}
        self._show_crop(dst)
        self.log(f"\n完成 → {dst}")
        self.status(f"已裁剪  {os.path.basename(dst)}")
        self.shell.material.set_image(dst)

    # ------------------------------------------------------------------ 运镜执行
    def run_kb(self):
        """校验 + 接单：渲帧和编码都在 KenBurns worker 里，这里不碰像素。"""
        if not self.shell.ff:
            self._no_ffmpeg()
            return
        if not self.kb_a or not self.kb_b:
            self.warn("先在画面上框出起点，再点「改终点」框出终点。")
            return
        if tuple(self.kb_a) == tuple(self.kb_b):
            self.warn("起点和终点一模一样，镜头不会动。\n"
                      "想推近就把终点框缩小一点，想摇移就把终点挪个位置。")
            return
        if not self.is_image and self.info["dur"] <= 0:
            self.warn("读不出这条视频的时长，运镜没法定帧数。")
            return
        dst = self.ed_out.text().strip().strip('"') \
            or os.path.splitext(self.info["path"])[0] + "_运镜"
        if not dst.lower().endswith(".mp4"):
            dst += ".mp4"          # 没扩展名 ffmpeg 猜不出封装格式
        dst = unique_path(dst)
        od = os.path.dirname(dst)
        if od:
            os.makedirs(od, exist_ok=True)
        self.ed_out.setText(dst)
        plan = self._kb_plan(dst)
        w, h = plan["w"], plan["h"]
        self.log(f"运镜: 起点 {plan['a']} → 终点 "
                 f"({plan['b'][0]:.0f},{plan['b'][1]:.0f},{plan['b'][2]:.0f},"
                 f"{plan['b'][3]:.0f})")
        self.log(f"成片: {w}x{h}  {plan['n']} 帧 @ {plan['fps']}fps  "
                 f"{plan['codec']}  CRF {plan['crf']}  "
                 f"{self.cb_kb_ease.currentText()}")
        self._save()
        self._run_info = {"w": w, "h": h, "W": self.info["w"], "H": self.info["h"],
                          "n": plan["n"], "fps": plan["fps"],
                          "dur": plan["n"] / float(plan["fps"]),
                          "zoom": cp.kb_zoom(plan["b"], w, h) /
                          cp.kb_zoom(plan["a"], w, h),
                          "desc": f"{'HEVC' if plan['codec'] == 'hevc' else 'H.264'}"
                                  f" CRF {plan['crf']} · "
                                  f"{self.cb_kb_ease.currentText()}",
                          "audio": not self.is_image}
        job = KenBurns(plan)
        job.log.connect(self.log)
        if self.shell.start_job(job, self._kb_done, self.result):
            self.btn_run.setEnabled(False)
            self.result.set_status("run", f"运镜中 · {w}×{h}")
            self.result.set_metrics([])
            self.result.set_path(None)
            self.result.set_next("")

    def _kb_done(self, ok, res):
        self.btn_run.setEnabled(True)
        if ok and res:
            self._own_out = res[0]
            self.btn_open.setEnabled(True)
            self._stage = "已生成运镜"
            self.sync_page_hint()
            self._show_kb(res[0])
            self.log(f"\n完成 → {res[0]}")
            self.status(f"运镜成片  {os.path.basename(res[0])}")
            self.out_manual = False
            self.shell.material.set_video(res[0])   # 流水线：交给调色台
        elif res == "cancelled":
            self.log("已取消，成片与中间帧都删掉了")
            self.result.set_status("idle", "已取消 · 半成品已删除")
            self.result.set_next("运镜没有断点续跑，直接再来一次。")
            self.status("已取消")
        else:
            self.log(f"失败：{res}")
            self.result.set_status("bad", "运镜失败", res)
            self.result.set_next("展开技术细节看 ffmpeg 最后几行。")
            self.status("运镜失败，详见日志")

    KB_METRICS = (
        ("成片尺寸", "运镜成片的画面大小。它由终点取景框的比例决定，"
                   "所以推近之后不会突然变形或出现黑边。"),
        ("帧数", "重画出来的帧数。图片按「时长×帧率」算；视频按它自己的长度算，"
                "运镜不改这条片子有多长。"),
        ("时长", "成片播放多久。跟源视频不一样时说明帧率或帧数算错了。"),
        ("运镜幅度", "终点相对起点放大多少倍。倍数越大越费画质：推得很近等于"
                    "拿少数像素撑满 4K，会开始发软。"),
        ("方式", "缓动与编码档位。缓入缓出最像真镜头；运镜必然重新编码，"
                 "所以这里没有无损可选。"),
        ("音轨", "视频运镜会把原音轨复制过去；图片没有声音，成片自然静音。"),
        ("文件体积", "成片大小。帧目录是中间产物，出片后就删了，不占地方。"),
    )

    def _show_kb(self, dst):
        i = self._run_info or {}
        w, h, n, fps = i.get("w"), i.get("h"), i.get("n"), i.get("fps")
        z = i.get("zoom") or 1.0
        try:
            size = jobs.human_mb(os.path.getsize(dst))
        except OSError:
            size = "—"
        if 0.99 <= z <= 1.01:
            move = "平移（大小不变）"
        elif z > 1.0:
            move = f"推进 {z:.2f}×"
        else:
            move = f"拉远 {1.0 / z:.2f}×"
        soft = z > 2.0
        vals = [f"{w}×{h}" if w else "—",
                str(n) if n else "—",
                f"{n / float(fps):.1f}s" if n and fps else "—",
                move, i.get("desc") or "—",
                "跟随源视频" if i.get("audio") else "无（图片）",
                size]
        self.result.set_metrics(
            [(name, val, tip) for (name, tip), val in zip(self.KB_METRICS, vals)])
        self.result.set_status(
            "warn" if soft else "ok",
            f"运镜完成 · {w}×{h} · {move} · {size}",
            "推得超过 2× 会开始发软，想让画面更实就缩小运镜幅度或降低长边"
            if soft else None)
        self.result.set_path(dst, "成片：")
        self.result.set_next(
            "切到调色台接着调，或把这条片子丢回渲染队列批量出片。")

    # 输出尺寸/裁掉比例这些数字，用户最容易看错的是"裁掉多少"和
    # "这个模式到底损不损画质"，所以每个都配一句解释。
    METRICS = (
        ("输出尺寸", "裁完剩下多大的画面。要出 4K 就得保证这里长边≥3840，"
                  "否则合成时会被放大。"),
        ("裁掉", "被丢弃的画面面积百分比。裁得越多，构图和机位误差的容错越小。"),
        ("原画面", "源素材的分辨率，用来对照输出尺寸掉多少。"),
        ("方式", "比特流无损最快且不损画质但只能少数编码用；数学无损画质完全"
                "不变、文件很大；视觉无损按 CRF 重编码，一般选它。"),
        ("时长", "裁剪不改时长，这里应与源一致；不一致说明封装或帧率出问题。"),
        ("文件体积", "成片大小。选无损/比特流时体积接近源文件；突然小很多"
                  "多半是分辨率被改小或提前截断。"),
    )

    def _show_crop(self, dst):
        i = self._run_info or {}
        w, h, W, H = i.get("w"), i.get("h"), i.get("W"), i.get("H")
        loss = (100.0 * (1 - w * h / float(W * H))
                if w and h and W and H else None)
        try:
            size = jobs.human_mb(os.path.getsize(dst))
        except OSError:
            size = "—"
        dur = i.get("dur")
        vals = [f"{w}×{h}" if w else "—",
                "—" if loss is None else f"{loss:.1f}%",
                f"{W}×{H}" if W else "—",
                i.get("desc") or "—",
                "—" if dur is None else f"{dur:.1f}s",
                size]
        self.result.set_metrics(
            [(name, val, tip) for (name, tip), val in zip(self.METRICS, vals)])
        # 裁到长边不足 3840 值得提醒：后面出 4K 会被放大
        thin = w and max(w, h) < 3840
        self.result.set_status(
            "warn" if thin else "ok",
            f"裁剪完成 · {w}×{h} · {size}",
            "输出长边不足 3840，再出 4K 会被放大" if thin else None)
        self.result.set_path(dst, "成品：")
        self.result.set_next(
            "切到调色台接着改，或回合成模块用新序列重出。"
            if not thin else "想保住 4K，就重新框一个更大的选区再裁一次。")

    def _finished(self, ok, res):
        self.btn_run.setEnabled(True)
        if ok and res:
            self._own_out = res[0]
            self.btn_open.setEnabled(True)
            self._stage = "已裁剪"
            self.sync_page_hint()
            self._show_crop(res[0])
            self.log(f"\n完成 → {res[0]}")
            self.status(f"裁剪完成  {os.path.basename(res[0])}")
            self.out_manual = False
            self.shell.material.set_video(res[0])   # 流水线：交给调色台
        elif res == "cancelled":
            self.log("已取消，半成品已删除")
            self.result.set_status("idle", "已取消 · 半成品已删除")
            self.result.set_next("原文件没动过，可以直接再来一次。")
            self.status("已取消")
        else:
            self.log(f"失败：{res}")
            self.result.set_status("bad", "裁剪失败", res)
            self.result.set_next("展开技术细节看 ffmpeg 最后几行。")
            self.status("裁剪失败，详见日志")

    def open_out(self):
        widgets.reveal(self.ed_out.text().strip().strip('"') or None)

    def _no_ffmpeg(self):
        QtWidgets.QMessageBox.critical(
            self.shell, APP_TITLE,
            "未找到 ffmpeg，无法裁剪视频（图片裁剪不受影响）。\n请在运行本工具的 "
            "Python 环境里 pip install imageio-ffmpeg。")

    def play_in_grader(self):
        """把当前素材交给工作台：视频有播放条，图片直接进调色画布。"""
        path = (self.ed_path.text() or "").strip()
        if not path or not os.path.isfile(path):
            self.warn("先选一个图片或视频")
            return
        dev = self.shell.module("调色台")
        if dev is None:
            return
        if self.is_image:
            # set_image 返回 False 说明素材本来就是它，不会发信号，得手动加载
            if not self.shell.material.set_image(path):
                dev.load_image(path)
        elif not self.shell.material.set_video(path):
            dev.load_video(path)
        self.shell.set_module("调色台")

    # ------------------------------------------------------------------ 流水线
    def on_material(self, kind, path):
        if kind not in ("video", "image") or not path:
            return
        if self._own_out and os.path.normpath(path) == \
                os.path.normpath(self._own_out):
            return                      # 自己刚写的成片，别把它当新素材重载
        self.out_manual = False
        self.load(path)

    def log(self, text):
        self.log_view.appendPlainText(text)
        sb = self.log_view.verticalScrollBar()
        sb.setValue(sb.maximum())

    # ------------------------------------------------------------------ 记忆
    def _restore(self):
        s = self.settings()
        if s.contains(f"{self.name}/aspect"):
            self.cb_aspect.setCurrentText(
                s.value(f"{self.name}/aspect", "16:9"))
        if s.contains(f"{self.name}/crf"):
            self.sl_crf.setValue(int(s.value(f"{self.name}/crf", CRF_DEF)))
        if s.contains(f"{self.name}/preset"):
            i = self.cb_preset.findData(s.value(f"{self.name}/preset", "medium"))
            if i < 0:
                i = self.cb_preset.findData(
                    ffmod.preset_value(s.value(f"{self.name}/preset", "")))
            if i >= 0:
                self.cb_preset.setCurrentIndex(i)
        mode = s.value(f"{self.name}/mode", "copy", type=str)
        if mode in self.rb:
            self.rb[mode].setChecked(True)
        if s.contains(f"{self.name}/kb"):
            self.ck_kb.setChecked(s.value(f"{self.name}/kb", "0") == "1")
        if s.contains(f"{self.name}/kb_sec"):
            self.sp_kb_sec.setValue(float(s.value(f"{self.name}/kb_sec", KB_SEC_DEF)))
        for key, cb in (("kb_fps", self.cb_kb_fps), ("kb_side", self.cb_kb_side),
                        ("kb_codec", self.cb_kb_codec)):
            if s.contains(f"{self.name}/{key}"):
                cb.setCurrentText(s.value(f"{self.name}/{key}", ""))
        i = self.cb_kb_ease.findData(
            s.value(f"{self.name}/kb_ease", cp.KB_EASE_DEF))
        if i >= 0:
            self.cb_kb_ease.setCurrentIndex(i)
        if s.contains(f"{self.name}/kb_crf"):
            self.kb_sl_crf.setValue(int(s.value(f"{self.name}/kb_crf", KB_CR_DEF)))
        i = self.kb_cb_preset.findData(
            s.value(f"{self.name}/kb_preset", "medium"))
        if i >= 0:
            self.kb_cb_preset.setCurrentIndex(i)

    def _save(self):
        s = self.settings()
        s.setValue(f"{self.name}/aspect", self.cb_aspect.currentText())
        s.setValue(f"{self.name}/mode", self._mode())
        s.setValue(f"{self.name}/crf", self.sl_crf.value())
        s.setValue(f"{self.name}/preset", self._preset())
        s.setValue(f"{self.name}/kb", "1" if self._kb_on() else "0")
        s.setValue(f"{self.name}/kb_sec", self.sp_kb_sec.value())
        s.setValue(f"{self.name}/kb_fps", self.cb_kb_fps.currentText())
        s.setValue(f"{self.name}/kb_side", self.cb_kb_side.currentText())
        s.setValue(f"{self.name}/kb_ease", self.cb_kb_ease.currentData())
        s.setValue(f"{self.name}/kb_codec", self.cb_kb_codec.currentText())
        s.setValue(f"{self.name}/kb_crf", self.kb_sl_crf.value())
        s.setValue(f"{self.name}/kb_preset", self.kb_cb_preset.currentData())

    def shutdown(self):
        self._save()


class KenBurns(jobs.Job):
    """运镜成片：逐帧重画取景框 → 写 JPG 序列 → 交给 ffmpeg 编码。

    为什么不在 ffmpeg 里做：crop 的尺寸必须恒定，zoompan 只能在整像素上跳，
    两者都跟不上一条随时间连续变化的浮点取景框，推近时画面会一帧一帧地蹦。
    帧目录是纯中间产物，出片、取消或失败之后一律删掉（运镜没有续跑语义）。
    """

    def __init__(self, plan, parent=None):
        super().__init__([], parent)
        self.plan = plan
        self.timeline = ["重画取景框", "编码成片"]
        self._dir = None

    def run(self):
        p = self.plan
        try:
            os.makedirs(p["frames_dir"], exist_ok=True)
        except OSError as e:
            self.done.emit(False, f"建不出帧目录：{e}")
            return
        self._dir = p["frames_dir"]
        self.stage.emit(0, "run", "")
        made, err = self._render(p["frames_dir"])
        if self._stop:
            self.stage.emit(0, "bad", "已取消")
            self._drop_all()
            self.done.emit(False, "cancelled")
            return
        if err or made < 2:
            self.stage.emit(0, "bad", err[:24] if err else f"只有 {made} 帧")
            self._drop_all()
            self.done.emit(False, err or f"只画出 {made} 帧，不够成一条片子")
            return
        self.stage.emit(0, "ok", f"{made} 帧")
        self.stage.emit(1, "run", "")
        rc, misc = self._run_one(p["cmd"], jobs.Meter("frame", made), None,
                                 KB_RENDER_SPAN, 1.0 - KB_RENDER_SPAN)
        self._drop_dir(self._dir)
        if self._stop:
            self.stage.emit(1, "bad", "已取消")
            self._drop_out(p["out"])
            self.done.emit(False, "cancelled")
            return
        if rc != 0 or not os.path.exists(p["out"]):
            self.stage.emit(1, "bad", f"退出码 {rc}")
            for m in misc:
                self.log.emit(m)
            self._drop_out(p["out"])
            self.done.emit(False, f"ffmpeg 退出码 {rc}：{p['out']}")
            return
        self.stage.emit(1, "ok", f"{os.path.getsize(p['out']) / 1e6:.1f} MB")
        self.done.emit(True, [p["out"]])

    def _render(self, fd):
        """按缓动曲线把 n 个取景框画成 n 张 JPG；返回 (张数, 错误说明)。"""
        p = self.plan
        rects = cp.kb_rects(p["a"], p["b"], p["n"], p["ease"])
        it = self._sources()
        last, sx, sy, made = None, 1.0, 1.0, 0
        try:
            for rect in rects:
                if self._stop:
                    break
                img = next(it, None)
                if img is not None:
                    last = img
                    if p["image"]:
                        # 选区量在预览尺寸上，全分辨率要按比例放大回原图
                        fh, fw = img.shape[:2]
                        sx, sy = fw / float(p["W"]), fh / float(p["H"])
                if last is None:
                    return made, "读不到画面：源素材解码失败"
                x, y, w, h = rect
                try:
                    frame = cp.kb_frame(last, (x * sx, y * sy, w * sx, h * sy),
                                        p["w"], p["h"])
                except ValueError as e:
                    return made, f"取景框算到画面外了：{e}"
                if frame.dtype != np.float32:
                    frame = frame.astype(np.float32) / 255.0
                if not exporter.save_image(
                        os.path.join(fd, f"kb{made:0{KB_DIGITS}d}.jpg"),
                        frame, 95):
                    return made, f"写不进帧文件：{fd}"
                made += 1
                self.progress.emit(KB_RENDER_SPAN * made / float(len(rects)),
                                   f"重画 {made}/{len(rects)}")
        finally:
            it.close()        # 提前退出时把解码器收掉，别留一个还在写管道的进程
        return made, ""

    def _sources(self):
        """图片只解一次；视频流式解码，取满就停，不把整条片子读进内存。"""
        p = self.plan
        if p["image"]:
            img = exporter.load_full(p["src"])
            if img is not None:
                yield img
            return
        cmd = [p["ff"], "-y", "-hide_banner", "-loglevel", "error",
               "-i", p["src"], "-vf", f"scale={p['W']}:{p['H']}",
               "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]
        try:
            proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                    stderr=subprocess.DEVNULL,
                                    creationflags=NO_WINDOW)
        except OSError as e:
            self.log.emit(f"无法启动 ffmpeg 解码：{e}")
            return
        self._proc = proc
        stride = int(p["W"]) * int(p["H"]) * 3
        try:
            while True:
                buf = proc.stdout.read(stride)
                if len(buf) < stride:
                    return
                yield np.frombuffer(buf, dtype=np.uint8).reshape(
                    p["H"], p["W"], 3)
        finally:
            try:
                proc.stdout.close()
            except (OSError, ValueError):
                pass
            if proc.poll() is None:
                proc.terminate()
            proc.wait()
            if self._proc is proc:
                self._proc = None

    def _drop_all(self):
        self._drop_out(self.plan["out"])
        self._drop_dir(self._dir)

    def _drop_out(self, out):
        try:
            if out and os.path.exists(out):
                os.remove(out)
                self.log.emit(f"已删除未完成的 {out}")
        except OSError as e:
            self.log.emit(f"删除失败 {out}: {e}")

    def _drop_dir(self, d):
        try:
            if d and os.path.isdir(d):
                shutil.rmtree(d)
                self.log.emit(f"已清理中间帧目录 {os.path.basename(d)}")
        except OSError as e:
            self.log.emit(f"清理帧目录失败 {d}: {e}")
