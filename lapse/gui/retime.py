# -*- coding: utf-8 -*-
"""运动模块：变速（含速度曲线）与运动模糊，视频和图片序列都能吃。

映射与取样表全在 `lapse/motion.py`：一条"输出帧 → 源帧位置"的映射同时决定
两件事——疏密决定快慢（变速曲线），每张画面按窗口混哪几帧决定糊不糊（运动模糊）。
所以这里只管取值、预览和渲染，算法能被单测钉死。

渲染是流式的：ffmpeg 把源解码成 rawvideo 一路喂进来，按取样表加权混帧后写成
JPG，再交给 `video.build_cmd` 编码。帧目录是中间产物，出片、取消、失败都删干净。
"""
import os
import re
import shutil
import subprocess

import numpy as np
from PySide6 import QtCore, QtGui, QtWidgets

from lapse import ff as ffmod
from lapse import imio, motion, video
from lapse.ff import NO_WINDOW, unique_path
from lapse.gui import assemble as asm
from lapse.gui import hints
from lapse.gui import job as jobs
from lapse.gui import theme, widgets
from lapse.gui.grader import exporter
from lapse.gui.module import Module
from lapse.gui.workbench import APP_TITLE

RATE_MIN, RATE_MAX, RATE_DEF = 0.10, 20.0, 1.0
SEG_MIN, SEG_MAX, SEG_DEF = 0.10, 5.0, 1.0
SRC_FPS_DEF = 120.0          # 图片序列没有帧率，"原速"由用户给；跟合成页一致
STRENGTH_DEF = 70            # 100% 是全糊：默认留三成清晰，动的地方拖尾、静止处干净
CRF_MIN, CRF_MAX, CRF_DEF = 8, 35, 18
FRONT = "mt"                 # mt00000.jpg：跟运镜的 kb 前缀区分开，别互相吃帧
DIGITS = 5
MIX_SPAN = 0.7               # 一项里"混帧"占的进度权重，剩下的给编码
OUT_SUFFIX = "_变速"
DIR_SUFFIX = "_变速帧"

AU_AUTO, AU_KEEP, AU_MUTE = "auto", "keep", "mute"
AU_CHOICES = ((AU_AUTO, "自动：跟着倍速一起变"),
              (AU_KEEP, "保持原声不变速"),
              (AU_MUTE, "静音：不要音轨"))
MODE_ITEMS = [(label, key) for key, label in motion.MODE_LABELS]


def num(label, default):
    """从 "3840 (UHD 4K)" 或 "29.97" 这种标签里取前导数字。"""
    m = re.search(r"\d+(?:\.\d+)?", str(label))
    return float(m.group()) if m else default


def out_size(W, H, long_side):
    """长边给足、按源比例取偶数（竖幅源时长度落在高上）。"""
    long_side = max(16, int(round(long_side)))
    if W >= H:
        return long_side, max(2, video.even(long_side * H / float(W)))
    return max(2, video.even(long_side * W / float(H))), long_side


class SpeedGraph(QtWidgets.QWidget):
    """倍速轮廓：横轴是源片进度，纵轴是那一刻的倍速。

    虚线就是原速 1×：线压到虚线下面是慢放，抬上去是快放。起/中/止三个数
    改完必须马上看到形状，否则"变速曲线"就变成三个猜不出结果的数字。
    """

    PAD = 8.0

    def __init__(self, parent=None):
        super().__init__(parent)
        self.values = None
        self.setFixedHeight(92)
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Expanding,
                           QtWidgets.QSizePolicy.Policy.Fixed)

    def set_speeds(self, values):
        self.values = None if values is None else np.asarray(values, float)
        self.update()

    def paintEvent(self, _e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)
        r = QtCore.QRectF(self.rect()).adjusted(1, 1, -1, -1)
        p.fillRect(r, QtGui.QColor(theme.FIELD))
        p.setPen(QtGui.QPen(QtGui.QColor(theme.SEP), 1))
        p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
        p.drawRoundedRect(r, 8, 8)
        if self.values is None or len(self.values) < 2:
            p.setPen(QtGui.QColor(theme.TEXT_OFF))
            p.drawText(r.adjusted(12, 0, -12, 0),
                       QtCore.Qt.AlignmentFlag.AlignCenter,
                       "勾上「变速曲线」后这里画出倍速随时间的变化")
            p.end()
            return
        v = self.values
        box = r.adjusted(self.PAD, self.PAD, -self.PAD, -self.PAD)
        lo, hi = min(1.0, float(v.min())), max(1.0, float(v.max()))
        if hi - lo < 1e-6:
            hi, lo = hi + 0.5, lo - 0.5

        def x_at(i):
            return box.x() + box.width() * i / (len(v) - 1)

        def y_at(x):
            return box.bottom() - (x - lo) / (hi - lo) * box.height()

        y1 = y_at(1.0)
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER_HI), 1,
                            QtCore.Qt.PenStyle.DashLine))
        p.drawLine(QtCore.QPointF(box.x(), y1), QtCore.QPointF(box.right(), y1))
        pts = QtGui.QPolygonF([QtCore.QPointF(x_at(i), y_at(x))
                               for i, x in enumerate(v)])
        # 曲线与 1× 线之间填一层强调色：哪一段在慢放一眼就看得出来
        fill = QtGui.QColor(theme.ACCENT)
        fill.setAlpha(60)
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(fill)
        p.drawPolygon(QtGui.QPolygonF([QtCore.QPointF(box.x(), y1)]
                                      + list(pts)
                                      + [QtCore.QPointF(box.right(), y1)]))
        p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
        p.setPen(QtGui.QPen(QtGui.QColor(theme.ACCENT_HI), 1.8))
        p.drawPolyline(pts)
        f = self.font()
        f.setPixelSize(11)
        p.setFont(f)
        p.setPen(QtGui.QColor(theme.TEXT_DIM))
        p.drawText(QtCore.QPointF(box.x() + 2, box.top() + 11),
                   f"最快 {v.max():.2f}×")
        p.drawText(QtCore.QPointF(box.x() + 2, box.bottom() - 2),
                   f"最慢 {v.min():.2f}×")
        p.drawText(QtCore.QPointF(box.right() - 24, y1 - 4), "1×")
        p.end()


class Retime(Module):
    name = "运动"

    def __init__(self, shell):
        super().__init__(shell)
        self.src = None
        self.out_manual = False
        self._own_out = None            # 自己刚写的成片，别被流水线灌回来重载
        self._stage = ""
        self._run_info = None       # 这次渲染定格的参数，结果卡用它讲数字
        self._built = False         # 控件没建齐之前，联动函数一律不动
        self._preset_guard = False  # 选预设时别让 setValue 再绕回预设下拉框

    def top_hint(self):
        """顶栏只报状态：源素材规模 + 走到哪一步；旋钮的说明常驻在控件下面。"""
        if not self.src:
            return ""
        s = self.src
        base = (f"{s['n']} 帧 · {s['w']}×{s['h']}" if s["kind"] == "series"
                else f"{s['w']}×{s['h']} · {s['dur']:.1f}s")
        return f"{base} · {self._stage}" if self._stage else base

    # ------------------------------------------------------------------ UI
    def build(self):
        page = QtWidgets.QWidget()
        v = QtWidgets.QVBoxLayout(page)
        v.setContentsMargins(14, 12, 14, 12)
        v.setSpacing(10)

        top = QtWidgets.QHBoxLayout()
        top.setSpacing(8)
        self.btn_dir = widgets.icon_button(
            "folder-open", "选择图片序列目录（对齐或合成用的那种）")
        self.btn_dir.clicked.connect(self.choose_dir)
        self.btn_file = widgets.icon_button("film", "选择一个视频文件")
        self.btn_file.clicked.connect(self.choose_file)
        self.ed_src = QtWidgets.QLineEdit()
        self.ed_src.setPlaceholderText(
            "视频文件或图片序列目录；也可以把素材直接拖进窗口")
        hints.attach(self.ed_src, "运动/素材")
        self.btn_reload = widgets.icon_button("refresh", "重新读取当前素材")
        self.btn_reload.clicked.connect(lambda: self.load(self.ed_src.text()))
        self.btn_preview = widgets.icon_button(
            "play", "到工作台查看（视频可实时套调色参数）")
        self.btn_preview.clicked.connect(self.play_in_grader)
        top.addWidget(self.btn_dir)
        top.addWidget(self.btn_file)
        top.addWidget(self.ed_src, 1)
        top.addWidget(self.btn_preview)
        top.addWidget(self.btn_reload)
        v.addLayout(top)

        self.lb_info = QtWidgets.QLabel("（未选择素材）")
        self.lb_info.setObjectName("Info")
        self.lb_info.setWordWrap(True)
        v.addWidget(self.lb_info)

        body = QtWidgets.QHBoxLayout()
        body.setSpacing(12)

        left = QtWidgets.QVBoxLayout()
        left.setSpacing(10)
        self.preview = widgets.FramePreview(
            empty="选一条视频或一个序列目录，这里显示参考画面\n"
                  "变速与运动模糊都按这条素材的时间轴算")
        left.addWidget(self.preview, 3)
        self.log_view = QtWidgets.QPlainTextEdit()
        self.log_view.setObjectName("Log")
        self.log_view.setReadOnly(True)
        self.log_view.setPlaceholderText(
            "帧数映射、混帧窗口与每步进度写在这里。")
        self.log_view.setLineWrapMode(
            QtWidgets.QPlainTextEdit.LineWrapMode.NoWrap)
        self.log_view.setMinimumHeight(88)
        self.result = widgets.ResultCard("运动结果")
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
        self.btn_run = QtWidgets.QPushButton("生成变速成片")
        self.btn_run.setObjectName("Primary")
        widgets.label_button(self.btn_run, "运动", role="on_accent")
        self.btn_run.clicked.connect(self.run)
        self.btn_open = widgets.icon_button(
            "folder-open", "在文件管理器里打开输出目录")
        self.btn_open.setEnabled(False)
        self.btn_open.clicked.connect(self.open_out)
        bar.addWidget(self.btn_run)
        bar.addStretch(1)
        bar.addWidget(self.btn_open)
        side.addLayout(bar)

        side.addWidget(self._build_speed_box())
        side.addWidget(self._build_mix_box())
        side.addWidget(self._build_out_box())
        side.addWidget(self._build_file_box())
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

        self.ed_src.textChanged.connect(self._path_typed)
        self.ed_out.textEdited.connect(self._mark_manual)
        self._debounce = QtCore.QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.setInterval(600)
        self._debounce.timeout.connect(lambda: self.load(self.ed_src.text()))

        self._built = True
        self._restore()
        self._sync_curve()
        self._on_strength(self.sl_strength.value())
        self._on_crf(self.sl_crf.value())
        self._note()
        return page

    def _flabel(self, text):
        lb = QtWidgets.QLabel(text)
        lb.setObjectName("FieldLabel")
        return lb

    def _spin(self, lo, hi, val, step, suffix, decimals=2, key=None):
        sp = QtWidgets.QDoubleSpinBox()
        sp.setRange(lo, hi)
        sp.setValue(val)
        sp.setSingleStep(step)
        sp.setDecimals(decimals)
        sp.setSuffix(suffix)
        sp.setButtonSymbols(QtWidgets.QAbstractSpinBox.ButtonSymbols.NoButtons)
        sp.setObjectName("ValBox")
        sp.setMinimumHeight(24)
        sp.setAlignment(QtCore.Qt.AlignmentFlag.AlignRight)
        if key:
            hints.attach(sp, key)
        sp.valueChanged.connect(self._note)
        return sp

    def _combo(self, items, editable=False, current=None, by_data=False,
               key=None):
        cb = QtWidgets.QComboBox()
        for it in items:
            if isinstance(it, tuple):
                cb.addItem(it[0], it[1])
            else:
                cb.addItem(str(it))
        i = (cb.findData(current) if by_data else cb.findText(str(current))) \
            if current is not None else -1
        if i >= 0:
            cb.setCurrentIndex(i)
        elif editable and current is not None:
            cb.setEditable(True)
            cb.setCurrentText(str(current))
        cb.setEditable(editable)
        cb.setSizePolicy(QtWidgets.QSizePolicy.Policy.Ignored,
                         QtWidgets.QSizePolicy.Policy.Fixed)
        if key:
            hints.attach(cb, key)
        cb.currentTextChanged.connect(self._note)
        return cb

    def _build_speed_box(self):
        box = widgets.Group("变速")
        v = box.vbox(spacing=7)

        self.sp_rate = self._spin(RATE_MIN, RATE_MAX, RATE_DEF, 0.05, " ×",
                                 key="运动/倍速")
        v.addWidget(widgets.field_block("倍速", self.sp_rate,
                                        hints.line("运动/倍速")))

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        self.ck_curve = QtWidgets.QCheckBox("变速曲线")
        hints.attach(self.ck_curve, "运动/曲线")
        row.addWidget(self.ck_curve)
        self.cb_shape = self._combo([(n, n) for n, _ in motion.SHAPE_PRESETS],
                                    current="匀速", key="运动/曲线预设")
        row.addWidget(self.cb_shape, 1)
        v.addLayout(row)

        self.seg_row = QtWidgets.QWidget()
        g = QtWidgets.QGridLayout(self.seg_row)
        g.setContentsMargins(0, 0, 0, 0)
        g.setHorizontalSpacing(6)
        g.setVerticalSpacing(4)
        self.seg = {}
        for i, (key, label) in enumerate((("a", "起点"), ("m", "中点"),
                                          ("b", "止点"))):
            sp = self._spin(SEG_MIN, SEG_MAX, SEG_DEF, 0.05, " ×",
                            key="运动/曲线端点")
            self.seg[key] = sp
            lb = self._flabel(label)
            hints.attach(lb, "运动/曲线端点")
            g.addWidget(lb, 0, i)
            g.addWidget(sp, 1, i)
        v.addWidget(self.seg_row)

        self.graph = SpeedGraph()
        hints.attach(self.graph, "运动/轮廓图")
        v.addWidget(self.graph)

        self.sp_src_fps = self._spin(1.0, 1000.0, SRC_FPS_DEF, 1.0, " fps",
                                    decimals=3, key="运动/源帧率")
        v.addWidget(widgets.field_block("源帧率", self.sp_src_fps,
                                        hints.line("运动/源帧率")))

        self.lb_plan = QtWidgets.QLabel("")
        self.lb_plan.setObjectName("FieldHint")
        self.lb_plan.setWordWrap(True)
        v.addWidget(self.lb_plan)

        self.ck_curve.toggled.connect(self._sync_curve)
        self.cb_shape.currentTextChanged.connect(self._apply_preset)
        return box

    def _build_mix_box(self):
        box = widgets.Group("运动模糊")
        v = box.vbox(spacing=7)
        self.cb_mode = self._combo(MODE_ITEMS, current=motion.MIX,
                                   by_data=True, key="运动/混合模式")
        v.addWidget(widgets.field_block("画面混合", self.cb_mode,
                                        hints.line("运动/混合模式")))

        self.cb_taps = self._combo([n for n in motion.TAP_CHOICES],
                                   current=str(motion.TAP_DEF),
                                   key="运动/取样数")
        self.sl_strength = widgets.Slider()
        self.sl_strength.setRange(0, 100)
        self.sl_strength.setValue(STRENGTH_DEF)
        hints.attach(self.sl_strength, "运动/混合强度")
        self.lb_strength = QtWidgets.QLabel()
        self.lb_strength.setObjectName("ValueLabel")
        hints.attach(self.lb_strength, "运动/混合强度")

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(6)
        row.addWidget(widgets.field_block("取样帧数", self.cb_taps))
        q = QtWidgets.QVBoxLayout()
        q.setSpacing(3)
        head = QtWidgets.QHBoxLayout()
        head.setSpacing(6)
        head.addWidget(self.sl_strength, 1)
        head.addWidget(self.lb_strength)
        q.addWidget(self._flabel("模糊强度"))
        q.addLayout(head)
        row.addLayout(q, 1)
        v.addLayout(row)

        self.lb_mix = QtWidgets.QLabel("")
        self.lb_mix.setObjectName("FieldHint")
        self.lb_mix.setWordWrap(True)
        v.addWidget(self.lb_mix)

        self.cb_mode.currentIndexChanged.connect(self._sync_mix)
        self.sl_strength.valueChanged.connect(self._on_strength)
        return box

    def _build_out_box(self):
        box = widgets.Group("成片")
        g = box.grid(vspacing=6)
        g.setHorizontalSpacing(10)

        self.cb_fps = self._combo(asm.FPS_CHOICES, editable=True, current="30",
                                 key="运动/帧率")
        self.cb_side = self._combo(asm.SIDE_CHOICES, editable=True,
                                  current=asm.SIDE_CHOICES[0],
                                  key="运动/长边")
        self.ck_follow = QtWidgets.QCheckBox("长边跟源画面（不放大不缩小）")
        hints.attach(self.ck_follow, "运动/跟源尺寸")
        self.cb_codec = self._combo([t for t, _ in asm.CODEC_CHOICES],
                                    current=asm.CODEC_CHOICES[0][0],
                                    key="运动/编码器")
        self.cb_enc = self._combo([(n, v) for n, v in ffmod.PRESET_LABELS],
                                  by_data=True, current="medium",
                                  key="运动/编码速度")
        self.cb_audio = self._combo([(lab, key) for key, lab in AU_CHOICES],
                                    by_data=True, current=AU_AUTO,
                                    key="运动/音轨")

        self.crf_row = QtWidgets.QWidget()
        cl = QtWidgets.QHBoxLayout(self.crf_row)
        cl.setContentsMargins(0, 0, 0, 0)
        self.sl_crf = widgets.Slider()
        self.sl_crf.setRange(CRF_MIN, CRF_MAX)
        self.sl_crf.setValue(CRF_DEF)
        hints.attach(self.sl_crf, "运动/质量")
        cl.addWidget(self.sl_crf, 1)
        self.lb_crf = QtWidgets.QLabel()
        self.lb_crf.setObjectName("ValueLabel")
        hints.attach(self.lb_crf, "运动/质量")
        cl.addWidget(self.lb_crf)

        def drop(label, w, key, row, col, span=1):
            g.addWidget(widgets.field_block(label, w, hints.line(key)),
                        row, col, 1, span)

        drop("成片帧率", self.cb_fps, "运动/帧率", 0, 0)
        drop("成片长边", self.cb_side, "运动/长边", 0, 1)
        g.addWidget(self.ck_follow, 1, 0, 1, 2)
        drop("编码器", self.cb_codec, "运动/编码器", 2, 0)
        drop("编码速度", self.cb_enc, "运动/编码速度", 2, 1)
        drop("质量 CRF（左=最清晰）", self.crf_row, "运动/质量", 3, 0, 2)
        drop("音轨", self.cb_audio, "运动/音轨", 4, 0, 2)
        g.setColumnStretch(0, 1)
        g.setColumnStretch(1, 1)

        self.ck_follow.toggled.connect(self._sync_side)
        self.sl_crf.valueChanged.connect(self._on_crf)
        return box

    def _build_file_box(self):
        box = widgets.Group("输出文件")
        g = box.grid(vspacing=6)
        g.setHorizontalSpacing(10)
        self.ed_out = QtWidgets.QLineEdit()
        self.ed_out.setPlaceholderText(
            "默认在素材旁生成 _变速 副本，原素材不动")
        hints.attach(self.ed_out, "运动/输出文件")
        self.btn_save = QtWidgets.QPushButton("另存为…")
        self.btn_save.setObjectName("ToolBtn")
        self.btn_save.clicked.connect(self.choose_out)
        g.addWidget(self.ed_out, 0, 0)
        g.addWidget(self.btn_save, 0, 1)
        hint = QtWidgets.QLabel(
            "重定时要逐帧重算，必然重新编码；帧目录是中间产物，出片后自动删除")
        hint.setObjectName("FieldHint")
        hint.setWordWrap(True)
        g.addWidget(hint, 1, 0, 1, 2)
        g.setColumnStretch(0, 1)
        return box

    # ------------------------------------------------------------------ 取值
    def _curve_on(self):
        return self.ck_curve.isChecked()

    def _shape(self):
        """勾上曲线才给轮廓，否则返回 None（=恒定倍速，走最短那条路）。"""
        if not self._curve_on():
            return None
        return motion.speed_shape(self.seg["a"].value(), self.seg["m"].value(),
                                  self.seg["b"].value())

    def _mode(self):
        return self.cb_mode.currentData() or motion.OFF

    def _taps(self):
        return int(num(self.cb_taps.currentText(), motion.TAP_DEF))

    def _strength(self):
        return self.sl_strength.value() / 100.0

    def _rate(self):
        return float(self.sp_rate.value())

    def _src_fps(self):
        return max(1e-3, float(self.sp_src_fps.value()))

    def _out_fps(self):
        return num(self.cb_fps.currentText(), 0.0)

    def _long_side(self):
        if self.ck_follow.isChecked():
            return max(self.src["w"], self.src["h"]) if self.src else 0
        return num(self.cb_side.currentText(), 0)

    def _preset(self):
        return self.cb_enc.currentData() or ffmod.preset_value(
            self.cb_enc.currentText())

    def _codec(self):
        return dict(asm.CODEC_CHOICES).get(self.cb_codec.currentText(), "hevc")

    def _audio(self):
        return self.cb_audio.currentData() or AU_AUTO

    # ------------------------------------------------------------------ 联动
    def _apply_preset(self, name):
        """选预设就把起/中/止填进去：曲线管相对快慢，倍速旋钮管整段快慢。"""
        vals = dict(motion.SHAPE_PRESETS).get(name)
        if vals is None or self._preset_guard or not self._built:
            return
        self._preset_guard = True
        try:
            for key, val in zip(("a", "m", "b"), vals):
                self.seg[key].setValue(val)
        finally:
            self._preset_guard = False

    def _sync_curve(self, _on=None):
        """不勾曲线时三个端点旋钮就该灰掉：看得见却不能用的控件最误导人。"""
        if not self._built:
            return
        on = self._curve_on()
        for sp in self.seg.values():
            sp.setEnabled(on)
        self.cb_shape.setEnabled(on)
        self.graph.setVisible(on)
        self.graph.set_speeds(self._shape() * self._rate() if on else None)
        self._note()

    def _sync_mix(self):
        """取样数与强度只对「运动模糊」有意义，另两种模式用不到窗口。"""
        if not self._built:
            return
        blur = self._mode() == motion.BLUR
        self.cb_taps.setEnabled(blur)
        self.sl_strength.setEnabled(blur)

    def _sync_side(self, _on=None):
        if not self._built:
            return
        self.cb_side.setEnabled(not self.ck_follow.isChecked())
        self._note()

    def _on_strength(self, v):
        self.lb_strength.setText(f"{int(v)}%")
        if self._built:
            self._note()

    def _on_crf(self, v):
        self.lb_crf.setText(f"{v}  {asm.crf_word(v)}")

    def _note(self):
        """把这次重定时到底产出什么写成一行数字，光看控件猜不出来。"""
        if not self._built:
            return
        self._sync_mix()
        if not self.src:
            self.lb_plan.setText("先选一条视频或一个图片序列目录。")
            self.lb_mix.setText(hints.line("运动/混合模式"))
            return
        est = self._estimate()
        if est is None:
            return
        pos, out_n, total, avg = est
        fps = self._out_fps()
        self.lb_plan.setText(
            f"源 {self.src['n']} 帧 → 成片 {out_n} 帧 · {total:.1f}s @ {fps:g}fps"
            f" · 平均 {avg:.2f}×")
        if abs(self._rate() - 1.0) < 1e-3 and not self._curve_on() and \
                abs(fps - self._src_fps()) < 1e-3:
            self.lb_plan.setText("倍速 1× 且帧率没变：等于原样重编码一遍，"
                                 "改倍速、曲线或帧率才有意义")
        if self._mode() == motion.BLUR:
            self.lb_mix.setText(
                f"运动模糊：平均每张混 {self._avg_window(pos):.1f} 帧，"
                f"画面走得越快糊得越多")
        elif self._mode() == motion.MIX:
            self.lb_mix.setText("邻帧混合：按位置补中间帧，慢放不会一顿一顿")
        else:
            self.lb_mix.setText("不混合：慢放会看到逐帧停顿，想顺一点就换邻帧混合")
        if self._curve_on():
            self.graph.set_speeds(self._shape() * self._rate())
        self._refresh_out()

    def _estimate(self):
        """源素材 + 现在的参数 → (位置映射, 成片帧数, 成片时长, 平均倍速)。"""
        fps = self._out_fps()
        side = self._long_side()
        if not self.src:
            return None
        if fps <= 0 or side < 16:
            self.lb_plan.setText("成片帧率与长边必须是正数。")
            return None
        try:
            pos, out_n, total = motion.retime(
                self.src["n"], self._src_fps(), fps, self._shape(), self._rate())
        except ValueError as e:
            self.lb_plan.setText(str(e))
            return None
        return pos, out_n, total, motion.avg_speed(
            self.src["n"], self._src_fps(), total)

    def _avg_window(self, pos):
        """平均混帧窗口宽度（帧）：告诉用户模糊大概吃掉多少帧。"""
        return float(np.mean(motion.half_spans(pos) * 2.0 + 1.0))

    # ------------------------------------------------------------------ 读取
    def choose_dir(self):
        d = QtWidgets.QFileDialog.getExistingDirectory(
            self.shell, "选择图片序列目录")
        if d:
            self.load(os.path.normpath(d))

    def choose_file(self):
        p, _ = QtWidgets.QFileDialog.getOpenFileName(
            self.shell, "选择视频", "",
            "视频 (*.mp4 *.mov *.mkv *.m4v *.avi *.ts *.mts *.m2ts *.webm "
            "*.wmv *.hevc *.h264);;所有文件 *.*")
        if p:
            self.load(p)

    def _path_typed(self, text):
        if text.strip():
            self._debounce.start()

    def load(self, path):
        path = (path or "").strip().strip('"')
        if not path:
            return
        if os.path.isdir(path):
            self._load_series(path)
        elif os.path.isfile(path):
            self._load_video(path)

    def _load_series(self, d):
        try:
            s = video.scan_series(d, allow_gaps=True)
        except video.SeriesError as e:
            self.src = None
            self._set_info(str(e), False)
            self.preview.clear("这个目录里没有可用的图片序列")
            self._note()
            return
        w, h = imio.image_size(os.path.join(d, s["sample"]))
        if not w or not h:
            self.src = None
            self._set_info("读不出图片尺寸（未安装 cv2）", False)
            self._note()
            return
        self.src = {"kind": "series", "path": d, "series": s,
                    "n": s["last"] - s["first"] + 1, "w": w, "h": h,
                    "dur": 0.0, "audio": False}
        self._set_info(
            f"序列  {s['prefix']}{s['first']:0{s['width']}d}.."
            f"{s['last']:0{s['width']}d}   共 {self.src['n']} 帧   "
            f"{w}x{h}"
            + (f"   缺口 {len(s['gaps'])} 处（按上一帧停留）"
               if s.get("gaps") else ""), not s.get("gaps"))
        self.preview.set_frame(os.path.join(d, s["sample"]),
                               f"首帧  {s['sample']}  {w}x{h}")
        self._after_load()

    def _load_video(self, path):
        if not self.shell.ff:
            self._no_ffmpeg()
            return
        bundle = ffmod.import_bundle(self.shell.ff, path)
        info = bundle["info"] if bundle else None
        if not info:
            self.src = None
            self._set_info(f"读不到视频流：{path}", False)
            self.preview.clear("读不出这条视频的画面")
            self._note()
            return
        if info["fps"] <= 0 or info["dur"] <= 0:
            self.src = None
            self._set_info("读不出这条视频的帧率或时长，重定时没法定位置", False)
            self._note()
            return
        self.src = {"kind": "video", "path": path, "series": None,
                    "n": max(2, int(round(info["dur"] * info["fps"]))),
                    "w": info["w"], "h": info["h"], "dur": info["dur"],
                    "audio": bool(info.get("audio"))}
        self._set_info(
            f"{info['codec']}  {ffmod.disp_text(info)}  {info['fps']:.3g}fps  "
            f"{info['dur']:.1f}s   约 {self.src['n']} 帧   "
            + ("自带音轨" if info.get("audio") else "无音轨"), True)
        pm = QtGui.QPixmap()
        # 这一帧只是给预览格用，直接吃导入时解好的那张，别再全分辨率解一遍
        if bundle and bundle.get("png"):
            pm.loadFromData(bundle["png"], "PNG")
        if not pm.isNull():
            self.preview.set_pixmap(pm, f"预览帧  {pm.width()}x{pm.height()}")
        self._after_load(info["fps"])

    def _after_load(self, fps=None):
        """换素材后把"原速"对齐到源：视频用探测值，序列沿用上次的设置。"""
        self._debounce.stop()
        path = self.src["path"]
        if os.path.normpath(path) != os.path.normpath(self.ed_src.text()):
            self.ed_src.blockSignals(True)
            self.ed_src.setText(os.path.normpath(path))
            self.ed_src.blockSignals(False)
        if fps:
            for w, val in ((self.sp_src_fps, round(float(fps), 3)),
                           (self.cb_fps, str(int(round(fps))))):
                w.blockSignals(True)
                if isinstance(w, QtWidgets.QComboBox):
                    w.setCurrentText(val)
                else:
                    w.setValue(val)
                w.blockSignals(False)
        self._own_out = None
        self.out_manual = False
        self.btn_open.setEnabled(False)
        self._stage = ""
        self.sync_page_hint()
        self._note()

    def _set_info(self, text, ok):
        self.lb_info.setText(text)
        self.lb_info.setObjectName("Info" if ok else "InfoBad")
        st = self.lb_info.style()
        st.unpolish(self.lb_info)
        st.polish(self.lb_info)
        if not ok:
            self._stage = ""
        self.sync_page_hint()

    def _refresh_out(self):
        if self.out_manual or not self.src:
            return
        p = self.src["path"]
        if self.src["kind"] == "series":
            stem = os.path.abspath(os.path.join(
                p.rstrip("\\/"), "..",
                os.path.basename(p.rstrip("\\/")) + OUT_SUFFIX))
        else:
            stem = os.path.splitext(p)[0] + OUT_SUFFIX
        self.ed_out.setText(stem + ".mp4")

    def _mark_manual(self, _text=None):
        self.out_manual = True

    def choose_out(self):
        p, _ = QtWidgets.QFileDialog.getSaveFileName(
            self.shell, "输出文件", self.ed_out.text(), "MP4 视频 (*.mp4)")
        if p:
            self.out_manual = True
            if not p.lower().endswith(".mp4"):
                p += ".mp4"
            self.ed_out.setText(os.path.normpath(p))

    def open_out(self):
        widgets.reveal(self.ed_out.text().strip().strip('"') or None)

    # ------------------------------------------------------------------ 执行
    def run(self):
        if not self.src:
            self.warn("请先选择一条视频或一个图片序列目录。")
            return
        if not self.shell.ff:
            self._no_ffmpeg()
            return
        est = self._estimate()
        if est is None:
            self.warn("成片帧率、长边与源帧率必须是正数，素材至少要 2 帧。")
            return
        pos, out_n, total, avg = est
        if out_n < 2:
            self.warn(f"算出来只剩 {out_n} 帧，不成片。\n"
                      f"把倍速往 1× 收一收，或把成片帧率调高。")
            return
        dst = self.ed_out.text().strip().strip('"')
        if not dst:
            dst = os.path.splitext(self.src["path"])[0] + OUT_SUFFIX
        if not dst.lower().endswith(".mp4"):
            dst += ".mp4"          # 没扩展名 ffmpeg 猜不出封装格式
        dst = unique_path(dst)
        od = os.path.dirname(dst)
        if od:
            os.makedirs(od, exist_ok=True)
        try:
            plan = self._plan(dst, pos)
        except video.SeriesError as e:
            self.warn(str(e))
            return
        self.ed_out.setText(dst)
        self.out_manual = True
        self.log(f"素材: {self.src['kind']}  {self.src['n']} 帧  "
                 f"{self.src['w']}x{self.src['h']}  源帧率 {self._src_fps():g}")
        seg = ("曲线 " + "/".join(f"{self.seg[k].value():g}" for k in "amb")
               if self._curve_on() else "匀速")
        self.log(f"变速: {self._rate():g}×  {seg}  →  {out_n} 帧 @ "
                 f"{self._out_fps():g}fps  {total:.1f}s  平均 {avg:.2f}×")
        self.log(f"混合: {dict(motion.MODE_LABELS)[self._mode()]}  "
                 f"取样 {self._taps()}  强度 {int(self._strength() * 100)}%")
        self.log(f"成片: {plan['w']}x{plan['h']}  {plan['codec']}  "
                 f"CRF {plan['crf']}（{asm.crf_word(plan['crf'])}）  "
                 f"{self.cb_enc.currentText()}  音轨 "
                 f"{self.cb_audio.currentText()}")
        self._save()
        self._run_info = {"w": plan["w"], "h": plan["h"], "n": out_n,
                          "src_n": self.src["n"], "fps": self._out_fps(),
                          "total": total, "avg": avg, "rate": self._rate(),
                          "curve": self._curve_on(),
                          "mode": self._mode(), "taps": self._taps(),
                          "strength": self._strength(),
                          "audio": plan["audio_src"] is not None,
                          "filter": plan["audio_filter"],
                          "kind": self.src["kind"]}
        job = RetimeJob(plan)
        job.log.connect(self.log)
        if plan["cleanup"]:
            job.finished.connect(plan["cleanup"])
        if self.shell.start_job(job, self._done, self.result):
            self.btn_run.setEnabled(False)
            self.result.set_status("run", f"重定时中 · {plan['w']}×{plan['h']}")
            self.result.set_metrics([])
            self.result.set_path(None)
            self.result.set_next("")
        elif plan["cleanup"]:
            plan["cleanup"]()

    def _plan(self, dst, pos):
        """主线程把控件读干净，worker 只拿这份快照（跨线程读控件是老坑）。"""
        s = self.src
        w, h = out_size(s["w"], s["h"], self._long_side())
        rows = motion.plan_taps(pos, s["n"], self._mode(), self._taps(),
                                self._strength())
        frames_dir = os.path.splitext(dst)[0] + DIR_SUFFIX
        in_dir = pattern = None
        start = 0
        cleanup = None
        if s["kind"] == "series":
            in_dir, pattern, start, _cnt, cleanup = video.fill_gaps(
                s["path"], s["series"])
        audio_src, audio_filter = self._audio_args()
        cmd, _ow, _oh = video.build_cmd(
            self.shell.ff, frames_dir, dst, self._out_fps(), None, w,
            int(self.sl_crf.value()), self._preset(), self._codec(),
            f"{FRONT}%0{DIGITS}d.jpg", 0, len(rows), w, h, 0, False,
            progress=True, audio_src=audio_src, audio_filter=audio_filter)
        return {"ff": self.shell.ff, "kind": s["kind"], "src": s["path"],
                "dir": in_dir, "pattern": pattern, "start": start,
                "w": w, "h": h, "src_n": s["n"], "rows": rows, "out": dst,
                "frames_dir": frames_dir, "cmd": cmd, "crf": int(self.sl_crf.value()),
                "codec": self._codec(),
                "audio_src": audio_src, "audio_filter": audio_filter,
                "cleanup": cleanup}

    def _audio_args(self):
        """音轨三种策略。能不能跟着倍速走，取决于是不是恒定倍速。

        变速曲线是一条随时间变化的映射，一条 atempo 串不出这种效果，
        所以勾了曲线只能保持原速或静音——这一点必须在界面上说出来。
        """
        if self.src["kind"] != "video" or not self.src["audio"]:
            return None, None
        policy = self._audio()
        if policy == AU_MUTE:
            return None, None
        if policy == AU_KEEP or not motion.is_flat(self._shape()):
            return self.src["path"], None
        return self.src["path"], (motion.atempo(self._rate()) or None)

    def _done(self, ok, res):
        self.btn_run.setEnabled(True)
        if ok and res:
            self._own_out = res[0]
            self.btn_open.setEnabled(True)
            self._stage = "已重定时"
            self.sync_page_hint()
            self._show(res[0])
            self.log(f"\n完成 → {res[0]}")
            self.status(f"变速成片  {os.path.basename(res[0])}")
            self.out_manual = False
            self.shell.material.set_video(res[0])   # 流水线：交给调色台/裁剪
        elif res == "cancelled":
            self.log("已取消，成片与中间帧都删掉了")
            self.result.set_status("idle", "已取消 · 半成品已删除")
            self.result.set_next("重定时没有断点续跑，直接再来一次。")
            self.status("已取消")
        else:
            self.log(f"失败：{res}")
            self.result.set_status("bad", "重定时失败", res)
            self.result.set_next("展开技术细节看 ffmpeg 最后几行。")
            self.status("重定时失败，详见日志")

    # 每个数字配一句"它是怎么来的、多大算正常"：成片看不懂的时候，
    # 用户其实不知道该改倍速、改帧率还是回去改曲线。
    METRICS = (
        ("成片帧数", "重定时之后一共多少张画面。它由源帧数除以倍速算出来，"
                  "不是设出来的：越快越少。"),
        ("成片时长", "帧数 ÷ 成片帧率。想落在某一段时长，改倍速或改帧率都行。"),
        ("平均倍速", "整段平均快了多少倍。开了变速曲线时这里只是平均值，"
                  "各段真正的快慢看上面那张轮廓图。"),
        ("源→成片", "源素材多少帧被压成或拉成了现在这些帧。"),
        ("画面规格", "成片长宽与帧率。勾了「长边跟源画面」时这里就是源尺寸。"),
        ("画面混合", "每张画面混了几帧源、强度多少。邻帧混合管顺不顺，"
                  "运动模糊管拖不拖尾。"),
        ("音轨", "声音跟没跟着变。曲线变速时一条 atempo 做不到随时间变快，"
               "只能保持原速。"),
        ("文件体积", "成片大小。重定时必然重新编码，体积比源大是正常的。"),
    )

    def _show(self, dst):
        i = self._run_info or {}
        n, fps = i.get("n"), i.get("fps")
        w, h = i.get("w"), i.get("h")
        try:
            size = jobs.human_mb(os.path.getsize(dst))
        except OSError:
            size = "—"
        mix = {motion.OFF: "不混合",
               motion.MIX: "邻帧混合",
               motion.BLUR: f"运动模糊 {i.get('taps')} 帧 · "
                            f"{int(i.get('strength', 0) * 100)}%"
               }.get(i.get("mode", motion.OFF), "—")
        if not i.get("audio"):
            au = "无（序列素材或静音）"
        elif i.get("filter"):
            au = f"跟着倍速（{i['filter']}）"
        elif i.get("curve"):
            au = "原速（曲线变速只能保持原声）"
        else:
            au = "原速"
        vals = [str(n) if n else "—",
                f"{i.get('total', 0):.1f}s" if n else "—",
                f"{i.get('avg', 0):.2f}×",
                f"{i.get('src_n')} → {n}" if n else "—",
                f"{w}×{h} @ {fps:g}fps" if w and fps else "—",
                mix, au, size]
        self.result.set_metrics(
            [(name, val, tip) for (name, tip), val in zip(self.METRICS, vals)])
        kind, note = "ok", None
        if i.get("mode") == motion.OFF and i.get("rate", 1.0) < 0.8:
            kind = "warn"
            note = "慢放了但没开混合，会看到逐帧停顿；换「邻帧混合」更顺"
        elif i.get("curve") and i.get("audio"):
            kind = "warn"
            note = "变速曲线下的音轨只能保持原速，画面和声音会逐渐对不上"
        elif i.get("avg", 1.0) > 8:
            kind = "warn"
            note = "快放超过 8×，画面会开始一跳一跳；降倍速或回去调合成帧率"
        self.result.set_status(
            kind, f"重定时完成 · {n} 帧 · {i.get('total', 0):.1f}s · {size}",
            note)
        self.result.set_path(dst, "成片：")
        self.result.set_next(
            "切到调色台接着调，或把这条片子丢回渲染队列批量出片。")

    def play_in_grader(self):
        """把当前素材交给工作台：序列进调色台图库，视频走播放条。"""
        path = (self.ed_src.text() or "").strip().strip('"')
        if not path or not os.path.exists(path):
            self.warn("先选一条视频或一个序列目录")
            return
        dev = self.shell.module("调色台")
        if dev is None:
            return
        if os.path.isdir(path):
            self.shell.material.set_series(path)
        elif not self.shell.material.set_video(path):
            dev.load_video(path)
        self.shell.set_module("调色台")

    # ------------------------------------------------------------------ 流水线
    def on_material(self, kind, path):
        if kind not in ("video", "series") or not path:
            return
        if self._own_out and os.path.normpath(path) == \
                os.path.normpath(self._own_out):
            return              # 自己刚写的成片，别把它当新素材重载
        self.out_manual = False
        self.load(path)

    def log(self, text):
        self.log_view.appendPlainText(text)
        sb = self.log_view.verticalScrollBar()
        sb.setValue(sb.maximum())

    def _no_ffmpeg(self):
        QtWidgets.QMessageBox.critical(
            self.shell, APP_TITLE,
            "未找到 ffmpeg，无法重定时。\n请在运行本工具的 Python 环境里 "
            "pip install imageio-ffmpeg。")

    # ------------------------------------------------------------------ 记忆
    def _pick(self, cb, value, by_data=False):
        """把记住的值放回下拉框：找不到就保持当前项，可输入的还接受原样文本。"""
        i = cb.findData(value) if by_data else cb.findText(value)
        if i >= 0:
            cb.setCurrentIndex(i)
        elif cb.isEditable():
            cb.setCurrentText(value)

    def _restore(self):
        s = self.settings()
        k = f"{self.name}/"
        if s.contains(k + "rate"):
            self.sp_rate.setValue(float(s.value(k + "rate", RATE_DEF)))
        if s.contains(k + "curve"):
            self.ck_curve.setChecked(s.value(k + "curve", "0") == "1")
        for key in ("a", "m", "b"):
            if s.contains(k + "seg_" + key):
                self.seg[key].setValue(float(s.value(k + "seg_" + key, SEG_DEF)))
        self._pick(self.cb_shape, s.value(k + "shape", "匀速", type=str))
        self._pick(self.cb_mode, s.value(k + "mode", motion.MIX, type=str),
                   by_data=True)
        self._pick(self.cb_taps, s.value(k + "taps", str(motion.TAP_DEF),
                                        type=str))
        if s.contains(k + "strength"):
            self.sl_strength.setValue(int(s.value(k + "strength", STRENGTH_DEF)))
        self._pick(self.cb_fps, s.value(k + "fps", "30", type=str))
        self._pick(self.cb_side, s.value(k + "side", asm.SIDE_CHOICES[0],
                                        type=str))
        if s.contains(k + "follow"):
            self.ck_follow.setChecked(s.value(k + "follow", "1") == "1")
        self._pick(self.cb_codec, s.value(k + "codec",
                                         asm.CODEC_CHOICES[0][0], type=str))
        self._pick(self.cb_enc, ffmod.preset_value(
            s.value(k + "enc", "medium", type=str)), by_data=True)
        self._pick(self.cb_audio, s.value(k + "audio", AU_AUTO, type=str),
                   by_data=True)
        if s.contains(k + "src_fps"):
            self.sp_src_fps.setValue(float(s.value(k + "src_fps", SRC_FPS_DEF)))
        if s.contains(k + "crf"):
            self.sl_crf.setValue(int(s.value(k + "crf", CRF_DEF)))

    def _save(self):
        s = self.settings()
        k = f"{self.name}/"
        s.setValue(k + "rate", self.sp_rate.value())
        s.setValue(k + "curve", "1" if self._curve_on() else "0")
        for key in ("a", "m", "b"):
            s.setValue(k + "seg_" + key, self.seg[key].value())
        s.setValue(k + "shape", self.cb_shape.currentText())
        s.setValue(k + "mode", self._mode())
        s.setValue(k + "taps", self.cb_taps.currentText())
        s.setValue(k + "strength", self.sl_strength.value())
        s.setValue(k + "fps", self.cb_fps.currentText())
        s.setValue(k + "side", self.cb_side.currentText())
        s.setValue(k + "follow", "1" if self.ck_follow.isChecked() else "0")
        s.setValue(k + "codec", self.cb_codec.currentText())
        s.setValue(k + "enc", self._preset())
        s.setValue(k + "audio", self._audio())
        s.setValue(k + "src_fps", self.sp_src_fps.value())
        s.setValue(k + "crf", self.sl_crf.value())

    def shutdown(self):
        self._save()


class RetimeJob(jobs.Job):
    """重定时成片：流式解码 → 按取样表混帧 → 写 JPG → 交给 ffmpeg 编码。

    为什么不在 ffmpeg 里做：setpts 只能整段一种速度，minterpolate 也拿不到
    "这张输出帧按快门窗口要混哪几帧"这种随速度变化的取样表；权重必须在 numpy
    里算。帧目录是纯中间产物，出片、取消或失败之后一律删掉（没有续跑语义）。
    """

    def __init__(self, plan, parent=None):
        super().__init__([], parent)
        self.plan = plan
        self.timeline = ["按速度曲线混帧", "编码成片"]
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
        made, err = self._mix(p["frames_dir"])
        if self._stop:
            self.stage.emit(0, "bad", "已取消")
            self._drop_all()
            self.done.emit(False, "cancelled")
            return
        if err or made < 2:
            self.stage.emit(0, "bad", err[:24] if err else f"只有 {made} 帧")
            self._drop_all()
            self.done.emit(False, err or f"只混出 {made} 帧，不够成一条片子")
            return
        self.stage.emit(0, "ok", f"{made} 帧")
        self.stage.emit(1, "run", "")
        rc, misc = self._run_one(p["cmd"], jobs.Meter("frame", made), None,
                                 MIX_SPAN, 1.0 - MIX_SPAN)
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

    def _mix(self, fd):
        """按取样表混帧；返回 (写出的张数, 错误说明)。

        解码是顺序的、取样号也只往前走，所以缓冲区只留"后面还没画的行还会用到"
        的那几帧：后缀最小值决定什么时候可以丢。只有快放得很凶时窗口才变宽。
        """
        p = self.plan
        rows = p["rows"]
        if not rows:
            return 0, "取样表是空的"
        w, h = p["w"], p["h"]
        his = np.fromiter((r[-1][0] for r in rows), np.int64, len(rows))
        los = np.fromiter((r[0][0] for r in rows), np.int64, len(rows))
        need = np.minimum.accumulate(los[::-1])[::-1]
        buf = {}
        acc = np.empty((h, w, 3), np.float32)
        tmp = np.empty((h, w, 3), np.float32)
        total = len(rows)
        made = 0
        seen = 0
        it = self._sources()
        try:
            for frame in it:
                if self._stop:
                    break
                j = seen
                seen += 1
                buf[j] = frame
                while made < total and his[made] <= j:
                    row = rows[made]
                    if len(row) == 1:
                        np.multiply(buf[row[0][0]], 1.0 / 255.0, out=acc)
                    else:
                        # 权重直接乘在 0~255 的整数帧上会留在原尺度，
                        # save_image 要的是 [0,1]，所以每一份都顺手除掉 255
                        acc[...] = 0.0
                        for idx, wt in row:
                            np.multiply(buf[idx], wt / 255.0, out=tmp)
                            acc += tmp
                    if not exporter.save_image(
                            os.path.join(
                                fd, f"{FRONT}{made:0{DIGITS}d}.jpg"), acc, 95):
                        return made, f"写不进帧文件：{fd}"
                    made += 1
                    low = int(need[made]) if made < total else j + 1
                    for key in [key for key in buf if key < low]:
                        del buf[key]
                    if made % 20 == 0 or made == total:
                        self.progress.emit(MIX_SPAN * made / float(total),
                                           f"混帧 {made}/{total}")
                if made >= total:
                    break
        finally:
            it.close()
        if made < total:
            self.log.emit(
                f"源素材只解出 {seen} 帧（按 {p['src_n']} 帧排的表），"
                f"成片尾部 {total - made} 帧取不到画面，已少掉")
        return made, ""

    def _sources(self):
        """流式解码成片尺寸的 RGB 帧；取满就停，不把整条片子读进内存。"""
        p = self.plan
        cmd = [p["ff"], "-hide_banner", "-loglevel", "error"]
        if p["kind"] == "series":
            cmd += ["-start_number", str(p["start"]),
                    "-i", os.path.join(p["dir"], p["pattern"])]
        else:
            cmd += ["-i", p["src"]]
        cmd += ["-vf", f"scale={p['w']}:{p['h']}",
                "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]
        try:
            proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                    stderr=subprocess.DEVNULL,
                                    creationflags=NO_WINDOW)
        except OSError as e:
            self.log.emit(f"无法启动 ffmpeg 解码：{e}")
            return
        self._proc = proc
        stride = int(p["w"]) * int(p["h"]) * 3
        try:
            while True:
                raw = proc.stdout.read(stride)
                if len(raw) < stride:
                    return
                yield np.frombuffer(raw, dtype=np.uint8).reshape(
                    p["h"], p["w"], 3)
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
