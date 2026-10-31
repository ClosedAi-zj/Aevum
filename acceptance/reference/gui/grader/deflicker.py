# -*- coding: utf-8 -*-
"""序列去闪烁对话框：后台测光、偏移曲线、窗口/强度调整与应用。

分析只读缩略图并放在后台线程；进度与取消沿用外壳状态栏那一条，
所以对话框不能设成模态，否则取消按钮点不到。测光结果只是一组亮度值，
调整窗口或强度即时重算，不必重新扫一遍几千帧。
分通道（R/G/B）换的是测光种类，必须重扫；两遍只换算法，跟着即时重算。
"""
import os

from PySide6 import QtCore, QtGui, QtWidgets

from reference import deflicker as flicker
from reference.gui import hints, job as jobs, theme


class AnalyzeJob(jobs.Job):
    """逐帧测光的后台任务，走通用 Job 的进度/取消信号。"""

    def __init__(self, paths, window=15, strength=100, channels=False,
                 multi=False):
        super().__init__([])
        self.paths = list(paths)
        self.window = int(window)
        self.strength = int(strength)
        self.channels = bool(channels)
        self.multi = bool(multi)

    def run(self):
        try:
            res = flicker.scan(
                self.paths, self.window, self.strength,
                progress=lambda f, t: self.progress.emit(f, "去闪烁 " + t),
                cancel=lambda: self.stopping,
                channels=self.channels, multi=self.multi)
        except Exception as e:
            self.log.emit(f"去闪烁分析失败：{e}")
            self.done.emit(False, str(e))
            return
        if res is None or self.stopping:
            self.done.emit(False, "cancelled")
            return
        if res["bad"]:
            self.log.emit(f"跳过 {len(res['bad'])} 张无法读取的图片")
        self.done.emit(True, res)


# 三条曲线的颜色就是通道本身，跟着皮肤走会看不懂哪条是红。
CHAN_COLORS = ("#f87171", "#4ade80", "#60a5fa")
CHAN_NAMES = ("红", "绿", "蓝")


class CurveCanvas(QtWidgets.QWidget):
    """偏移曲线：横轴帧，纵轴 EV；零线以上提亮、以下压暗。

    分通道测光时同一个坐标里画 R/G/B 三条，整帧亮度时只有一条。
    """

    def __init__(self):
        super().__init__()
        self._rows = []          # [[v], [v], ...] 或 [[r,g,b], ...]
        self._chan = False
        self.setMinimumHeight(180)
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Expanding,
                           QtWidgets.QSizePolicy.Policy.Expanding)

    def set_offsets(self, offsets):
        rows = []
        for v in offsets:
            if isinstance(v, (list, tuple)):
                rows.append([float(x) for x in v][:3])
            else:
                rows.append([float(v)])
        self._chan = len(rows) > 1 and all(len(r) == 3 for r in rows)
        self._rows = rows
        self.update()

    def clear(self):
        self._rows = []
        self._chan = False
        self.update()

    def paintEvent(self, _ev):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing, True)
        r = self.rect().adjusted(1, 1, -1, -1)
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(QtGui.QColor(theme.BG3))
        p.drawRoundedRect(r, 8, 8)

        box = r.adjusted(44, 16, -14, -26)
        if box.width() < 20 or box.height() < 20:
            return
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER), 1))
        p.drawLine(box.left(), box.center().y(), box.right(), box.center().y())

        rows = self._rows
        if not rows:
            p.setPen(QtGui.QColor(theme.TEXT_DIM))
            p.drawText(box, QtCore.Qt.AlignmentFlag.AlignCenter, "尚未分析")
            return

        flat = [v for row in rows for v in row]
        mx = max(0.2, max(abs(v) for v in flat) * 1.15)
        grid = QtGui.QPen(QtGui.QColor(*theme.GRID), 1,
                          QtCore.Qt.PenStyle.DotLine)
        p.setPen(grid)
        for v in (-mx * 0.5, mx * 0.5):
            y = box.center().y() - v / mx * box.height() * 0.5
            p.drawLine(box.left(), int(y), box.right(), int(y))

        def pt(i, v):
            x = (box.left() + (box.width() * i / max(1, len(rows) - 1))
                 if len(rows) > 1 else box.center().x())
            y = box.center().y() - (v / mx) * box.height() * 0.5
            return QtCore.QPointF(x, y)

        def curve(col):
            path = QtGui.QPainterPath(pt(0, 0.0))
            for i, row in enumerate(rows):
                path.lineTo(pt(i, row[col]))
            return path

        if not self._chan:
            path = curve(0)
            fill = QtGui.QPainterPath(path)
            fill.lineTo(pt(len(rows) - 1, 0.0))
            fill.lineTo(pt(0, 0.0))
            fill.closeSubpath()
            c = QtGui.QColor(theme.ACCENT)
            c.setAlpha(38)
            p.setPen(QtCore.Qt.PenStyle.NoPen)
            p.setBrush(c)
            p.drawPath(fill)
            p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
            p.setPen(QtGui.QPen(QtGui.QColor(theme.ACCENT_HI), 1.6))
            p.drawPath(path)
        else:
            p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
            for col in range(3):
                p.setPen(QtGui.QPen(QtGui.QColor(CHAN_COLORS[col]), 1.5))
                p.drawPath(curve(col))
            # 通道之间没有「谁盖住谁」，只把最靠下那条的名字排在图例里
            x = box.left() + 6
            for col in range(3):
                p.setPen(QtGui.QColor(CHAN_COLORS[col]))
                p.drawText(QtCore.QRectF(x, box.top() - 1, 34, 16),
                           QtCore.Qt.AlignmentFlag.AlignLeft,
                           CHAN_NAMES[col])
                x += 30

        p.setPen(QtGui.QColor(theme.TEXT_DIM))
        f = p.font()
        f.setPixelSize(theme.fs(12))
        p.setFont(f)
        p.drawText(QtCore.QRectF(r.left() + 6, box.top() - 10, 40, 16),
                   QtCore.Qt.AlignmentFlag.AlignLeft, f"+{mx:.2f}")
        p.drawText(QtCore.QRectF(r.left() + 6, box.bottom() + 2, 40, 16),
                   QtCore.Qt.AlignmentFlag.AlignLeft, f"-{mx:.2f}")
        p.drawText(QtCore.QRectF(r.left() + 6, box.center().y() - 8, 34, 16),
                   QtCore.Qt.AlignmentFlag.AlignLeft, "0")
        p.drawText(QtCore.QRectF(box.left(), r.bottom() - 18, 60, 16),
                   QtCore.Qt.AlignmentFlag.AlignLeft, "1")
        p.drawText(QtCore.QRectF(box.right() - 70, r.bottom() - 18, 70, 16),
                   QtCore.Qt.AlignmentFlag.AlignRight, f"{len(rows)}")


class DeflickerDialog(QtWidgets.QDialog):
    def __init__(self, dev, items):
        super().__init__(dev.shell)
        self.dev = dev
        self.items = list(items)
        self._means = None
        self._offsets = None
        self._stats = {}
        self._job = None
        self._closing = False
        self.setWindowTitle("序列去闪烁")
        self.setWindowFlag(
            QtCore.Qt.WindowType.WindowContextHelpButtonHint, False)
        self.setModal(False)
        self.resize(700, 540)
        self.setMinimumSize(560, 460)
        self._build()
        self._load_existing()

    # ------------------------------------------------------------------ 装配
    def _build(self):
        v = QtWidgets.QVBoxLayout(self)
        v.setContentsMargins(18, 16, 18, 14)
        v.setSpacing(10)

        title = QtWidgets.QLabel("序列去闪烁")
        title.setObjectName("SettingsTitle")
        v.addWidget(title)
        n = len(self.items)
        lb = QtWidgets.QLabel(f"{n} 帧 · 只读取缩略图测光，不改原片")
        lb.setObjectName("SettingsHint")
        lb.setWordWrap(True)
        v.addWidget(lb)

        row = QtWidgets.QHBoxLayout()
        row.setSpacing(10)
        lb_w = QtWidgets.QLabel("平滑窗口")
        row.addWidget(lb_w)
        self.sp_window = QtWidgets.QSpinBox()
        self.sp_window.setRange(3, 199)
        self.sp_window.setSingleStep(2)
        self.sp_window.setValue(15)
        self.sp_window.setSuffix(" 帧")
        hints.attach(self.sp_window, "去闪/窗口", label=lb_w)
        row.addWidget(self.sp_window)
        row.addSpacing(12)
        lb_s = QtWidgets.QLabel("强度")
        row.addWidget(lb_s)
        self.sl_strength = QtWidgets.QSlider(QtCore.Qt.Orientation.Horizontal)
        self.sl_strength.setRange(0, 100)
        self.sl_strength.setValue(100)
        self.sl_strength.setFixedWidth(150)
        hints.attach(self.sl_strength, "去闪/强度", label=lb_s)
        self.lb_strength = QtWidgets.QLabel("100%")
        self.lb_strength.setFixedWidth(42)
        hints.attach(self.lb_strength, "去闪/强度")
        row.addWidget(self.sl_strength)
        row.addWidget(self.lb_strength)
        row.addStretch(1)
        v.addLayout(row)

        opt = QtWidgets.QHBoxLayout()
        opt.setSpacing(14)
        self.cb_channels = QtWidgets.QCheckBox("分通道测光（R/G/B）")
        hints.attach(self.cb_channels, "去闪/分通道")
        self.cb_channels.toggled.connect(self._on_channels)
        self.cb_multi = QtWidgets.QCheckBox("两遍校正")
        hints.attach(self.cb_multi, "去闪/多遍")
        self.cb_multi.toggled.connect(self._recompute)
        opt.addWidget(self.cb_channels)
        opt.addWidget(self.cb_multi)
        opt.addStretch(1)
        v.addLayout(opt)

        line = QtWidgets.QLabel(hints.line("去闪/窗口") + " · "
                                + hints.line("去闪/强度") + " · "
                                + hints.line("去闪/分通道"))
        line.setObjectName("SettingsHint")
        line.setWordWrap(True)
        v.addWidget(line)

        self.canvas = CurveCanvas()
        v.addWidget(self.canvas, 1)

        self.lb_stats = QtWidgets.QLabel("")
        self.lb_stats.setObjectName("SettingsPath")
        self.lb_stats.setWordWrap(True)
        v.addWidget(self.lb_stats)
        self.lb_state = QtWidgets.QLabel("尚未分析")
        self.lb_state.setObjectName("SettingsHint")
        v.addWidget(self.lb_state)

        foot = QtWidgets.QHBoxLayout()
        self.btn_analyze = QtWidgets.QPushButton("分析序列")
        hints.attach(self.btn_analyze, "去闪/分析")
        self.btn_analyze.clicked.connect(self._analyze)
        self.btn_clear = QtWidgets.QPushButton("清除")
        hints.attach(self.btn_clear, "去闪/清除")
        self.btn_clear.clicked.connect(self._clear)
        self.btn_apply = QtWidgets.QPushButton("应用")
        hints.attach(self.btn_apply, "去闪/应用")
        self.btn_apply.setObjectName("Primary")
        self.btn_apply.clicked.connect(self._apply)
        self.btn_close = QtWidgets.QPushButton("关闭")
        self.btn_close.clicked.connect(self.reject)
        foot.addWidget(self.btn_analyze)
        foot.addWidget(self.btn_clear)
        foot.addStretch(1)
        foot.addWidget(self.btn_close)
        foot.addWidget(self.btn_apply)
        v.addLayout(foot)

        self.sp_window.valueChanged.connect(self._recompute)
        self.sl_strength.valueChanged.connect(self._on_strength)
        self.btn_apply.setEnabled(False)
        self.btn_clear.setEnabled(False)

    def _load_existing(self):
        data = self.dev.current_deflicker()
        if not data:
            return
        self._means = list(data.get("means") or [])
        self.sp_window.blockSignals(True)
        self.sp_window.setValue(int(data.get("window", 15)))
        self.sp_window.blockSignals(False)
        self.sl_strength.blockSignals(True)
        self.sl_strength.setValue(int(data.get("strength", 100)))
        self.sl_strength.blockSignals(False)
        self.lb_strength.setText(f"{self.sl_strength.value()}%")
        self._set_flags(bool(data.get("channels")), bool(data.get("multi")))
        self._recompute()
        self.lb_state.setText("已加载上次的测光结果，可直接调窗口或强度")

    def _set_flags(self, channels, multi):
        for w, v in ((self.cb_channels, channels), (self.cb_multi, multi)):
            w.blockSignals(True)
            w.setChecked(bool(v))
            w.blockSignals(False)

    def _means_are_channels(self):
        for row in (self._means or []):
            if row is not None:
                return isinstance(row, (list, tuple))
        return self.cb_channels.isChecked()

    def _on_channels(self, on):
        """换测光种类得重扫：手头只有一套数，补不出另一种曲线。"""
        if self._means and self._means_are_channels() != bool(on):
            self._means = None
            self._offsets = None
            self._stats = {}
            self.canvas.clear()
            self.lb_stats.setText("")
            self.lb_state.setText("整帧亮度和分通道不能共用测光，请重新分析")
            self.btn_apply.setEnabled(False)
            self.btn_clear.setEnabled(False)
            return
        self._recompute()

    def _on_strength(self, v):
        self.lb_strength.setText(f"{v}%")
        self._recompute()

    # ------------------------------------------------------------------ 分析
    def _analyze(self):
        if self._job is not None:
            return
        if len(self.items) < 2:
            QtWidgets.QMessageBox.information(
                self, "序列去闪烁", "至少需要两张图片才能分析闪烁。")
            return
        job = AnalyzeJob(self.items, self.sp_window.value(),
                         self.sl_strength.value(),
                         self.cb_channels.isChecked(),
                         self.cb_multi.isChecked())
        self._job = job
        self._set_running(True)
        self.lb_state.setText("正在逐帧测光…")
        if not self.dev.start_job(job, self._analyzed):
            self._job = None
            self._set_running(False)
            self.lb_state.setText("已有任务在跑，先等它结束或取消")

    def _analyzed(self, ok, res):
        self._job = None
        self._set_running(False)
        if self._closing:
            self._closing = False
            super().close()
            return
        if not ok:
            self.lb_state.setText(
                "已取消分析" if res == "cancelled" else f"分析失败：{res}")
            return
        self._means = list(res.get("means") or [])
        self._recompute()
        bad = (res.get("stats") or {}).get("bad", 0)
        self.lb_state.setText(
            f"分析完成{('，跳过 ' + str(bad) + ' 张坏图') if bad else ''}")

    def _recompute(self, *_):
        if not self._means:
            return
        offs, stats = flicker.analyze_means(
            self._means, self.sp_window.value(), self.sl_strength.value(),
            self.cb_multi.isChecked())
        self._offsets = offs.tolist()
        self._stats = dict(stats)
        self.canvas.set_offsets(self._offsets)
        self._show_stats()
        self.btn_apply.setEnabled(True)
        self.btn_clear.setEnabled(True)

    def _show_stats(self):
        s = self._stats
        if not s:
            self.lb_stats.setText("")
            return
        txt = ("相邻帧亮度抖动 P95：{:.3f} → {:.3f} EV · 最大校正 ±{:.2f} EV "
               "· 平均补 {:.2f} EV".format(
                   s.get("jitter_before", 0.0), s.get("jitter_after", 0.0),
                   s.get("max_ev", 0.0), s.get("mean_abs_ev", 0.0)))
        if s.get("channels"):
            txt += (" · 色闪 {:.3f} → {:.3f} EV".format(
                s.get("color_before", 0.0), s.get("color_after", 0.0)))
        if s.get("channels") and self.cb_multi.isChecked():
            txt += " · 两遍"
        self.lb_stats.setText(txt)

    def _set_running(self, on):
        for w in (self.sp_window, self.sl_strength, self.btn_analyze,
                  self.cb_channels, self.cb_multi):
            w.setEnabled(not on)
        self.btn_apply.setEnabled(not on and bool(self._offsets))
        self.btn_clear.setEnabled(not on and bool(self._means))
        self.btn_analyze.setText("分析中…" if on else "分析序列")

    # ------------------------------------------------------------------ 应用
    def _apply(self):
        if not self._offsets or not self._means:
            return
        names = [os.path.basename(p) for p in self.items]
        cur = [os.path.basename(p) for p in self.dev.photo_items()]
        if names != cur:
            QtWidgets.QMessageBox.warning(
                self, "序列去闪烁",
                "素材列表已经变了，请关掉这个窗口，重新打开去闪烁。")
            return
        data = {
            "files": names,
            "offsets": self._offsets,
            "means": self._means,
            "window": self.sp_window.value(),
            "strength": self.sl_strength.value(),
            "channels": bool(self._stats.get("channels")),
            "multi": bool(self.cb_multi.isChecked()),
        }
        self.dev.set_deflicker(data, persist=True)
        self.accept()

    def _clear(self):
        self.dev.clear_deflicker()
        self._means = None
        self._offsets = None
        self._stats = {}
        self.canvas.clear()
        self.lb_stats.setText("")
        self.lb_state.setText("已清除去闪烁校正")
        self.btn_apply.setEnabled(False)
        self.btn_clear.setEnabled(False)

    # ------------------------------------------------------------------ 收尾
    def closeEvent(self, ev):
        if self._job is not None and self._job.isRunning():
            self._closing = True
            self.lb_state.setText("正在取消分析…")
            self._job.cancel()
            ev.ignore()
            return
        super().closeEvent(ev)
