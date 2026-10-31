"""右侧工具面板：可折叠分组、滑块行、HSL、曲线、预设。"""
import copy

import numpy as np
from PySide6 import QtCore, QtGui, QtWidgets

from reference import grade as gc
from reference.gui import theme
from reference.gui.grader import params
from reference.gui.widgets import Group, Slider, icon_button

_DEFAULTS = gc.defaults()

# 色温/色调是有方向的：轨道直接画成 Lightroom 那种蓝→黄、绿→品红色带
_GRADIENTS = {
    "temperature": ("#1d4ed8", "#f59e0b"),
    "tint": ("#16a34a", "#e879f9"),
}
# 色彩分级的色相滑块铺一整圈色轮：0 红 60 黄 120 绿 180 青 240 蓝 300 洋红
_HUE_WHEEL = ("#ef4444", "#eab308", "#22c55e", "#06b6d4",
              "#3b82f6", "#a855f7", "#ef4444")
for _k in ("grade_global_h", "grade_sh_h", "grade_mid_h", "grade_hi_h"):
    _GRADIENTS[_k] = _HUE_WHEEL


def decimals_of(fmt):
    try:
        return int(fmt.split(".")[1].rstrip("f}"))
    except (IndexError, ValueError):
        return 0


class SliderRow(QtWidgets.QWidget):
    """名称 + 滑块 + 精确数值。拖动实时预览，松手算一步历史。

    交互向 Lightroom / Capture One 看齐：双击轨道或名称复位，Alt 拖动精调，
    改过的数值用强调色标出来。mask 不为 None 时这一行编辑的是那张局部蒙版的
    增量（中性位是 0，复位也写回 0）。
    """

    def __init__(self, store, label, key, lo, hi, step, fmt, idx=None,
                 name_w=84, hint="", mask=None, attr=None, neutral=None,
                 comp=None, getter=None, setter=None):
        super().__init__()
        self.store = store
        self.key, self.idx = key, idx
        self.mask, self.attr, self.comp = mask, attr, comp
        self._getter, self._setter = getter, setter
        self.neutral_override = neutral
        self.lo, self.hi, self.step = lo, hi, step
        self._busy = False

        lay = QtWidgets.QHBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(8)

        self.name = QtWidgets.QLabel(label)
        self.name.setObjectName("NameLabel")
        self.name.setFixedWidth(name_w)
        tip = self._tip(hint)
        self.name.setToolTip(tip)
        self.name.installEventFilter(self)

        self.n = max(1, int(round((hi - lo) / step)))
        self.slider = Slider(neutral=int(round((0 - lo) / step)),
                             gradient=_GRADIENTS.get(key))
        self.slider.setRange(0, self.n)
        self.slider.setToolTip(tip)
        self.slider.valueChanged.connect(self._from_slider)
        self.slider.sliderPressed.connect(store.begin_edit)
        self.slider.sliderReleased.connect(store.end_edit)
        self.slider.reset_requested.connect(self.reset)

        self.spin = QtWidgets.QDoubleSpinBox()
        self.spin.setObjectName("ValBox")
        self.spin.setFrame(False)          # 外框交给 QLineEdit，见 theme.py 注释
        self.spin.setDecimals(decimals_of(fmt))
        self.spin.setRange(lo, hi)
        self.spin.setSingleStep(step)
        self.spin.setFixedWidth(60)
        self.spin.setAlignment(QtCore.Qt.AlignmentFlag.AlignRight
                               | QtCore.Qt.AlignmentFlag.AlignVCenter)
        self.spin.setButtonSymbols(
            QtWidgets.QAbstractSpinBox.ButtonSymbols.NoButtons)
        self.spin.setKeyboardTracking(False)
        self.spin.setToolTip(tip)
        self.spin.valueChanged.connect(self._from_spin)

        lay.addWidget(self.name)
        lay.addWidget(self.slider, 1)
        lay.addWidget(self.spin)
        self.sync()

    def _tip(self, hint):
        """每个控件都要说清方向：轨道中央是中性位，往两边走是加/减。"""
        line = self.key + (f"[{self.idx}]" if self.idx is not None else "")
        if hint:
            line += "\n" + hint
        elif self.lo < 0 and self.hi > 0:
            line += f"\n左 = {self.lo:g}，中 = 0（无改动），右 = +{self.hi:g}"
        else:
            line += f"\n左 = {self.lo:g}，右 = {self.hi:g}"
        return line + "\n双击复位 · 按住 Alt 拖动精调"

    def value(self):
        if self._getter is not None:
            return self._getter()
        if self.mask is not None:
            if self.attr:
                return self.store.mask_attr(self.mask, self.attr,
                                            j=self.comp)
            return self.store.mask_value(self.mask, self.key)
        return self.store.value(self.key, self.idx)

    def _write(self, v):
        if self._setter is not None:
            self._setter(v)
        elif self.mask is not None:
            if self.attr:
                self.store.mask_set_attr(self.mask, self.attr, v,
                                         j=self.comp)
            else:
                self.store.mask_set(self.mask, self.key, v)
        else:
            self.store.set(self.key, v, self.idx)

    def _from_slider(self, i):
        if self._busy:
            return
        self._write(self.lo + i * self.step)

    def _from_spin(self, v):
        if self._busy:
            return
        self.store.begin_edit()
        self._write(float(v))
        self.store.end_edit()

    @property
    def _neutral(self):
        if self.neutral_override is not None:
            return float(self.neutral_override)
        if self.mask is not None:
            # 空间项没填过时读的是全局默认（如半径 1.0），复位也回那里
            if self.key in gc.MASK_SPATIAL and not self.attr:
                return float(_DEFAULTS.get(self.key, 0.0))
            return 0.0                       # 蒙版写的是增量，0 = 跟随全局
        d = _DEFAULTS.get(self.key, 0.0)
        return d[self.idx] if self.idx is not None else d

    def sync(self):
        v = self.value()
        self._busy = True
        self.slider.setValue(int(round((v - self.lo) / self.step)))
        self.spin.setValue(float(v))
        self._busy = False
        mod = abs(float(v) - float(self._neutral)) > 1e-9
        le = self.spin.lineEdit()
        if le is not None and le.property("modified") != mod:
            le.setProperty("modified", mod)
            le.style().unpolish(le)
            le.style().polish(le)

    def reset(self):
        self.store.push()
        self._write(self._neutral)
        self.sync()

    def eventFilter(self, obj, e):
        if (obj is self.name
                and e.type() == QtCore.QEvent.Type.MouseButtonDblClick):
            self.reset()
            return True
        return super().eventFilter(obj, e)


class CurveCanvas(QtWidgets.QWidget):
    """色调曲线：点击加控制点，拖动调明暗，右键删除。"""

    def __init__(self, store, key, parent=None, size=250):
        super().__init__(parent)
        self.store = store
        self.key = key
        self.setFixedSize(size, size)
        self.setObjectName("Canvas")
        self.setMouseTracking(True)
        self._sel = None

    PAD = 12

    def pts(self):
        return self.store.p[self.key]

    def _to_widget(self, x, y):
        s = (self.width() - 2 * self.PAD) / 255.0
        return (self.PAD + x * s, self.height() - self.PAD - y * s)

    def _from_widget(self, px, py):
        s = (self.width() - 2 * self.PAD) / 255.0
        x = (px - self.PAD) / s
        y = (self.height() - self.PAD - py) / s
        return x, y

    def set_key(self, key):
        self.key = key
        self._sel = None
        self.update()

    def mousePressEvent(self, e):
        if e.button() != QtCore.Qt.MouseButton.LeftButton:
            return
        x, y = self._from_widget(e.position().x(), e.position().y())
        pts = self.pts()
        best, bd = None, 9.0
        for i, (px, py) in enumerate(pts):
            wx, wy = self._to_widget(px, py)
            d = ((wx - e.position().x()) ** 2 + (wy - e.position().y()) ** 2) ** 0.5
            if d < bd:
                best, bd = i, d
        if best is None:                       # 空白处：插入新点
            xi = int(np.clip(round(x), 1, 254))
            if any(abs(px - xi) < 0.5 for px, _ in pts):
                return
            self.store.push()
            pts.append([xi, int(np.clip(round(y), 0, 255))])
            pts.sort(key=lambda q: q[0])
            self._sel = next(i for i, q in enumerate(pts) if q[0] == xi)
            self.store.changed.emit("curve")
        else:
            self._sel = best
            self.store.begin_edit()
        self.update()

    def mouseMoveEvent(self, e):
        if self._sel is None:
            return
        pts = self.pts()
        x, y = self._from_widget(e.position().x(), e.position().y())
        pts[self._sel][1] = int(np.clip(round(y), 0, 255))
        self.store.changed.emit("curve")
        self.update()

    def mouseReleaseEvent(self, e):
        if self._sel is not None:
            self.store.end_edit()
            self._sel = None

    def mouseDoubleClickEvent(self, e):
        x, y = self._from_widget(e.position().x(), e.position().y())
        pts = self.pts()
        for i, (px, py) in enumerate(pts):
            if i in (0, len(pts) - 1):
                continue
            wx, wy = self._to_widget(px, py)
            if ((wx - e.position().x()) ** 2
                    + (wy - e.position().y()) ** 2) ** 0.5 < 9:
                self.store.push()
                pts.pop(i)
                self.store.changed.emit("curve")
                self.update()
                return

    def paintEvent(self, e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.Antialiasing)
        p.fillRect(self.rect(), QtGui.QColor(theme.FIELD))
        w, h = self.width(), self.height()
        p.setPen(QtGui.QPen(QtGui.QColor(*theme.GRID), 1))
        for i in range(1, 4):
            p.drawLine(QtCore.QPoint(w * i // 4, self.PAD),
                       QtCore.QPoint(w * i // 4, h - self.PAD))
            p.drawLine(QtCore.QPoint(self.PAD, h * i // 4),
                       QtCore.QPoint(w - self.PAD, h * i // 4))
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER), 1,
                            QtCore.Qt.PenStyle.DashLine))
        p.drawLine(QtCore.QPoint(*[int(v) for v in self._to_widget(0, 0)]),
                   QtCore.QPoint(*[int(v) for v in self._to_widget(255, 255)]))
        try:
            lut = gc.pchip_lut(self.pts())
        except Exception:                       # 控制点被拖成非法序列
            lut = None
        if lut is not None:
            path = QtGui.QPainterPath()
            for i in range(256):
                qx, qy = self._to_widget(i, float(lut[i]))
                if i == 0:
                    path.moveTo(qx, qy)
                else:
                    path.lineTo(qx, qy)
            p.setPen(QtGui.QPen(QtGui.QColor(theme.ACCENT), 1.6))
            p.drawPath(path)
        for i, (px, py) in enumerate(self.pts()):
            qx, qy = self._to_widget(px, py)
            sel = i == self._sel
            p.setBrush(QtGui.QColor(theme.ACCENT if sel else theme.TEXT_OFF))
            p.setPen(QtCore.Qt.PenStyle.NoPen)
            p.drawEllipse(QtCore.QPointF(qx, qy), 4 if sel else 3,
                          4 if sel else 3)
        p.setBrush(QtGui.QColor(*theme.GRID))
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.drawRoundedRect(self.rect().adjusted(1, 1, -1, -1), 6, 6)
        p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER), 1))
        p.drawRoundedRect(QtCore.QRectF(self.rect()).adjusted(0.5, 0.5, -.5, -.5),
                          6, 6)
        p.end()


class CurvePanel(QtWidgets.QWidget):
    CHANNELS = [("curve_rgb", "RGB"), ("curve_r", "红"),
                ("curve_g", "绿"), ("curve_b", "蓝")]

    def __init__(self, store, parent=None):
        super().__init__(parent)
        self.store = store
        lay = QtWidgets.QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(6)
        bar = QtWidgets.QHBoxLayout()
        bar.setSpacing(4)
        self.btns = {}
        for key, label in self.CHANNELS:
            b = QtWidgets.QPushButton(label)
            b.setObjectName("Chip")
            b.setCheckable(True)
            b.setChecked(key == "curve_rgb")
            b.clicked.connect(lambda _=False, k=key: self.set_channel(k))
            bar.addWidget(b)
            self.btns[key] = b
        br = QtWidgets.QPushButton("复位")
        br.setObjectName("Chip")
        br.clicked.connect(self.reset_channel)
        bar.addWidget(br)
        bar.addStretch(1)
        lay.addLayout(bar)
        self.canvas = CurveCanvas(store, "curve_rgb", self)
        # 250px 的方形画布在 374px 右栏里居中，别贴着左边缘
        lay.addWidget(self.canvas, 0, QtCore.Qt.AlignmentFlag.AlignHCenter)
        self.channel = "curve_rgb"

    def set_channel(self, key):
        self.channel = key
        self.canvas.set_key(key)
        for k, b in self.btns.items():
            b.setChecked(k == key)

    def reset_channel(self):
        self.store.push()
        self.store.p[self.channel] = [[0, 0], [255, 255]]
        self.store.changed.emit("curve")
        self.canvas.update()

    def sync(self):
        self.canvas.update()


class HSLPanel(QtWidgets.QWidget):
    AXIS = [("hsl_h", "色相", "左 = 逆时针转色相，右 = 顺时针（最大 ±40°）"),
            ("hsl_s", "饱和度", "右 = 这一档颜色更艳，左 = 更灰"),
            ("hsl_l", "明亮度", "右 = 这一档颜色更亮，左 = 更暗")]

    def __init__(self, store, parent=None):
        super().__init__(parent)
        self.store = store
        self.rows = {}
        lay = QtWidgets.QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(5)
        bar = QtWidgets.QHBoxLayout()
        bar.setSpacing(4)
        self.btns = {}
        for key, label, _hint in self.AXIS:
            b = QtWidgets.QPushButton(label)
            b.setObjectName("Chip")
            b.setCheckable(True)
            b.clicked.connect(lambda _=False, k=key: self.show_axis(k))
            bar.addWidget(b)
            self.btns[key] = b
        bar.addStretch(1)
        lay.addLayout(bar)

        for key, _label, hint in self.AXIS:
            rows = []
            for i, cname in enumerate(gc.HSL_NAMES):
                r = SliderRow(store, cname, key, -100, 100, 1, "{:+.0f}",
                              idx=i, name_w=52, hint=hint)
                r.setVisible(False)
                lay.addWidget(r)
                rows.append(r)
            self.rows[key] = rows
        self.axis = None
        self.show_axis("hsl_h")

    def show_axis(self, key):
        self.axis = key
        for k, rows in self.rows.items():
            for r in rows:
                r.setVisible(k == key)
        for k, b in self.btns.items():
            b.setChecked(k == key)

    def sync(self):
        for rows in self.rows.values():
            for r in rows:
                r.sync()


class LevelsCanvas(QtWidgets.QWidget):
    """色阶画布：直方图 + 输入黑/灰/白三个把手 + 输出黑白场条。

    Photoshop / Lightroom 的 Levels 就是拖把手的，不给数字框；这里数值仍然只有
    store 里那一份，画布只是它的另一种编辑方式（拖动即实时预览，松手算一步）。
    """

    PAD = 8
    GAMMA_HI = 5.0                     # 0.2~5.0 在对数轴上正好对称

    def __init__(self, store, parent=None):
        super().__init__(parent)
        self.store = store
        self.setMinimumHeight(136)
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Expanding,
                           QtWidgets.QSizePolicy.Policy.Fixed)
        self.setObjectName("Canvas")
        self.setMouseTracking(True)
        self.setToolTip("拖动把手：黑场 / 灰点 / 白场在上排，输出黑白场在下排\n"
                        "双击复位本组")
        self._u8 = None
        self._hist = None
        self._drag = None

    # ---- 数据 ----
    def set_hist(self, u8):
        """吃渲染结果的降采样 uint8（和示波器同一份数据，不另算一次全图）。"""
        self._u8 = u8
        self._hist = None
        if self.isVisible():
            self.update()

    def _get_hist(self):
        if self._hist is None and self._u8 is not None:
            lum = (self._u8.astype(np.float32) @ gc.LUMA).astype(np.uint8)
            h = np.bincount(lum.ravel(), minlength=256).astype(np.float32)
            m = float(h.max())
            self._hist = h / m if m > 0 else h
        return self._hist

    # ---- 坐标 <-> 数值 ----
    def _span(self):
        return max(1.0, self.width() - 2 * self.PAD)

    def _x(self, v):
        return self.PAD + v / 255.0 * self._span()

    def _v(self, px):
        return float(np.clip((px - self.PAD) / self._span() * 255.0, 0.0, 255.0))

    def _gx(self, g):
        t = 0.5 + np.log2(max(float(g), 1e-3)) / (2.0 * np.log2(self.GAMMA_HI))
        return self.PAD + float(np.clip(t, 0.0, 1.0)) * self._span()

    def _g(self, px):
        t = (px - self.PAD) / self._span()
        return float(np.clip(2.0 ** ((t - 0.5) * 2.0 * np.log2(self.GAMMA_HI)),
                             1.0 / self.GAMMA_HI, self.GAMMA_HI))

    def handles(self):
        p = self.store.p
        y_in = self.height() - 52
        y_out = self.height() - 12
        return {"black": (self._x(p["lv_black"]), y_in),
                "gamma": (self._gx(p["lv_gamma"]), y_in),
                "white": (self._x(p["lv_white"]), y_in),
                "out_black": (self._x(p["lv_out_black"]), y_out),
                "out_white": (self._x(p["lv_out_white"]), y_out)}

    # ---- 交互 ----
    def mousePressEvent(self, e):
        if e.button() != QtCore.Qt.MouseButton.LeftButton:
            return
        best, bd = None, 12.0
        for nm, (hx, hy) in self.handles().items():
            d = ((hx - e.position().x()) ** 2
                 + (hy - e.position().y()) ** 2) ** 0.5
            if d < bd:
                best, bd = nm, d
        if best is not None:
            self._drag = best
            self.store.begin_edit()
            e.accept()

    def mouseMoveEvent(self, e):
        x = e.position().x()
        if self._drag is None:
            near = any(abs(hx - x) < 10 for hx, _ in self.handles().values())
            self.setCursor(QtCore.Qt.CursorShape.SizeHorCursor
                           if near else QtCore.Qt.CursorShape.ArrowCursor)
            return
        p, s = self.store.p, self.store
        v = self._v(x)
        if self._drag == "black":
            s.set("lv_black", min(v, p["lv_white"] - 1.0))
        elif self._drag == "white":
            s.set("lv_white", max(v, p["lv_black"] + 1.0))
        elif self._drag == "gamma":
            s.set("lv_gamma", self._g(x))
        elif self._drag == "out_black":
            s.set("lv_out_black", min(v, p["lv_out_white"] - 1.0))
        else:
            s.set("lv_out_white", max(v, p["lv_out_black"] + 1.0))
        self.update()

    def mouseReleaseEvent(self, e):
        if self._drag is not None:
            self._drag = None
            self.store.end_edit()
            e.accept()

    def mouseDoubleClickEvent(self, _e):
        self.store.reset_keys(params.LEVEL_KEYS)
        self.update()

    # ---- 绘制 ----
    def paintEvent(self, _e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.Antialiasing)
        p.fillRect(self.rect(), QtGui.QColor(theme.FIELD))
        w, h = self.width(), self.height()
        left, right = self.PAD, w - self.PAD
        top, base = 16, h - 58

        hist = self._get_hist()
        if hist is not None:
            path = QtGui.QPainterPath(QtCore.QPointF(left, base))
            step = (right - left) / 255.0
            for i in range(256):
                path.lineTo(left + i * step, base - float(hist[i]) * (base - top))
            path.lineTo(right, base)
            path.closeSubpath()
            p.setPen(QtCore.Qt.PenStyle.NoPen)
            p.setBrush(QtGui.QColor(*theme.HIST_L))
            p.drawPath(path)
        else:
            p.setPen(QtGui.QPen(QtGui.QColor(theme.SEP), 1))
            p.drawLine(QtCore.QPoint(int(left), int(base)),
                       QtCore.QPoint(int(right), int(base)))

        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER), 1))
        for i in range(1, 4):
            gx = left + (right - left) * i / 4.0
            p.drawLine(QtCore.QPointF(gx, top), QtCore.QPointF(gx, base))

        # 输入把手那一排：一条黑到白的色带，把手压在带子上
        band = QtCore.QRectF(left, base + 4, right - left, 9)
        grad = QtGui.QLinearGradient(band.topLeft(), band.topRight())
        grad.setColorAt(0.0, QtGui.QColor("#000000"))
        grad.setColorAt(1.0, QtGui.QColor("#ffffff"))
        p.fillRect(band, QtGui.QBrush(grad))

        hs = self.handles()
        for nm in ("black", "gamma", "white"):
            hx, hy = hs[nm]
            col = QtGui.QColor(theme.TEXT if nm != "gamma" else theme.ACCENT)
            p.setPen(QtCore.Qt.PenStyle.NoPen)
            p.setBrush(col)
            if nm == "gamma":
                p.drawEllipse(QtCore.QPointF(hx, base + 8.5), 4.5, 4.5)
            else:
                p.drawPolygon(QtGui.QPolygonF(
                    [QtCore.QPointF(hx - 5, base + 17),
                     QtCore.QPointF(hx + 5, base + 17),
                     QtCore.QPointF(hx, base + 6)]))

        # 输出黑白场条
        oy = h - 24
        obar = QtCore.QRectF(left, oy, right - left, 9)
        og = QtGui.QLinearGradient(obar.topLeft(), obar.topRight())
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(QtGui.QColor(theme.PRESSED))
        p.drawRect(obar)
        ob, ow = (self.store.p["lv_out_black"], self.store.p["lv_out_white"])
        seg = QtCore.QRectF(self._x(ob), oy, max(1.0, self._x(ow) - self._x(ob)), 9)
        g2 = QtGui.QLinearGradient(seg.topLeft(), seg.topRight())
        g2.setColorAt(0.0, QtGui.QColor("#000000"))
        g2.setColorAt(1.0, QtGui.QColor("#ffffff"))
        p.fillRect(seg, QtGui.QBrush(g2))
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER), 1))
        p.drawRect(obar)
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(QtGui.QColor(theme.TEXT))
        for nm in ("out_black", "out_white"):
            hx, _ = hs[nm]
            p.drawPolygon(QtGui.QPolygonF(
                [QtCore.QPointF(hx - 5, oy + 15),
                 QtCore.QPointF(hx + 5, oy + 15),
                 QtCore.QPointF(hx, oy + 5)]))

        p.setPen(QtGui.QColor(theme.TEXT_OFF))
        f = p.font()
        f.setPixelSize(10)
        p.setFont(f)
        p.drawText(QtCore.QRectF(0, 1, w - 2, 14),
                   QtCore.Qt.AlignmentFlag.AlignRight,
                   f"黑 {ob:.0f} · 灰 {self.store.p['lv_gamma']:.2f} · "
                   f"白 {ow:.0f}")
        p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER), 1))
        p.drawRoundedRect(QtCore.QRectF(self.rect()).adjusted(0.5, 0.5, -.5, -.5),
                          6, 6)
        p.end()


class MaskPanel(QtWidgets.QWidget):
    """局部蒙版列表 + 当前组的形状与参数。

    一个组 = 多个形状（线性 / 径向）的布尔组合 + 一套「相对全局」的增量。
    几何直接在画面上拖（ImageView 画把手），这里只管选中和数值。
    """

    selected = QtCore.Signal(int)
    component_selected = QtCore.Signal(int)
    overlay_toggled = QtCore.Signal(bool)

    def __init__(self, store, parent=None):
        super().__init__(parent)
        self.store = store
        self.sel = -1
        self.comp = 0
        self.rows = []
        self.chips = []
        self.comp_chips = []
        self._built = None
        self._adj_open = False
        self._det_open = False

        lay = QtWidgets.QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(6)

        bar = QtWidgets.QHBoxLayout()
        bar.setSpacing(4)
        self.btn_lin = icon_button(
            "slash", "新建线性渐变蒙版：拖一条线，起点一侧受影响\n"
                     "斜着拖也能用；角度滑块可再旋转\n压天空、加渐变最常用",
            "dim", 15)
        self.btn_rad = icon_button(
            "circle", "新建径向蒙版：圈内受影响，斜拖得到椭圆\n"
                      "角度滑块可旋转椭圆；提亮人脸、局部加饱和", "dim", 15)
        self.btn_del = icon_button("trash", "删除当前蒙版组", "off", 15)
        self.btn_eye = icon_button(
            "eye", "叠加显示所有蒙版范围（红=选区，M 键切换）", "dim", 15,
            checkable=True, checked_role="accent_hi")
        self.btn_lin.clicked.connect(lambda: self._add("linear"))
        self.btn_rad.clicked.connect(lambda: self._add("radial"))
        self.btn_del.clicked.connect(self._delete)
        self.btn_eye.toggled.connect(self._toggle_overlay)
        for b in (self.btn_lin, self.btn_rad, self.btn_del, self.btn_eye):
            bar.addWidget(b)
        bar.addStretch(1)
        lay.addLayout(bar)

        self.grid = QtWidgets.QGridLayout()
        self.grid.setSpacing(4)
        lay.addLayout(self.grid)

        self.empty = QtWidgets.QLabel(
            "还没有局部蒙版：「╱」新建线性渐变，「○」新建圆形。\n"
            "一个组可叠多个形状：同一组里第一个形状是选区，后面可选减去/交集。")
        self.empty.setObjectName("NameLabel")
        self.empty.setWordWrap(True)
        lay.addWidget(self.empty)

        self.body = QtWidgets.QVBoxLayout()
        self.body.setContentsMargins(0, 0, 0, 0)
        self.body.setSpacing(5)
        lay.addLayout(self.body)

        self.hint = QtWidgets.QLabel("画面上拖把手改范围；虚线形状为减去/交集")
        self.hint.setObjectName("ViewerMeta")
        self.hint.setVisible(False)
        lay.addWidget(self.hint)

    # ---- 结构变化 ----
    def toggle_overlay(self):
        """M 键：和「眼睛」按钮走同一条路，保持两者状态一致。"""
        self.btn_eye.toggle()

    def _add(self, kind):
        i = self.store.mask_add(kind)
        self.sel, self.comp = i, 0
        self._built = None
        self.sync()
        self.component_selected.emit(0)

    def _delete(self):
        if self.sel < 0:
            return
        self.store.mask_remove(self.sel)
        self.sel, self.comp = -1, 0
        self._built = None
        self.sync()

    def _toggle_overlay(self, on):
        self.overlay_toggled.emit(bool(on))

    def select(self, i):
        self.sel, self.comp = i, 0
        self._built = None
        self.sync()
        self.selected.emit(self.sel)
        self.component_selected.emit(0)

    def set_comp(self, j):
        """画布点到别的形状把手时由外部调用；不重复发 selected。"""
        j = max(0, int(j))
        if j == self.comp:
            return
        self.comp = j
        self._built = None
        self.sync()
        self.component_selected.emit(self.comp)

    def _add_comp(self, kind):
        if self.sel < 0:
            return
        j = self.store.component_add(self.sel, kind)
        if j < 0:
            return
        self.comp = j
        self._built = None
        self.sync()
        self.component_selected.emit(j)

    def _del_comp(self):
        if self.sel < 0:
            return
        self.store.component_remove(self.sel, self.comp)
        self.comp = max(0, min(self.comp, self.store.comp_count(self.sel) - 1))
        self._built = None
        self.sync()
        self.component_selected.emit(self.comp)

    def _invert(self, on):
        if self.sel < 0:
            return
        if bool(self.store.mask_attr(self.sel, "invert")) != bool(on):
            self.store.mask_toggle(self.sel)

    def _invert_comp(self, on):
        if self.sel < 0:
            return
        j = self.comp
        if bool(self.store.mask_attr(self.sel, "invert", j=j)) != bool(on):
            self.store.component_toggle(self.sel, j)

    def _blend(self, index):
        if self.sel < 0:
            return
        self.store.component_blend(self.sel, self.comp,
                                   self.cb_blend.itemData(index))

    def _range_toggle(self, on):
        if self.sel >= 0:
            self.store.mask_range_on(self.sel, on)
            self._built = None
            self.sync()

    def _clear(self, lay):
        while lay.count():
            it = lay.takeAt(0)
            w = it.widget()
            if w is not None:
                # 必须先 hide：deleteLater 要等事件循环回到 exec 才真删，
                # 这期间它还留着上一次的几何，画在面板上就是重影
                w.hide()
                w.deleteLater()
            elif it.layout() is not None:
                self._clear(it.layout())
                it.layout().deleteLater()

    def _group_tip(self, i):
        m = self.store.mask(i) or {}
        n = self.store.comp_count(i)
        kinds = []
        for j in range(n):
            c = self.store.comp(i, j) or {}
            kinds.append("线性" if (c.get("kind") or c.get("type")) == "linear"
                         else "径向")
        return (f"第 {i + 1} 组 · {' / '.join(kinds) or '空'}"
                + ("（整组反选）" if m.get("invert") else "")
                + f"\n{self.store.mask_range(i).get('lum_on') and '含亮度范围' or '几何蒙版'}")

    def _rebuild(self):
        n, sel = self.store.mask_count(), self.sel
        ncomp = self.store.comp_count(sel) if 0 <= sel < n else 0
        if ncomp:
            self.comp = max(0, min(self.comp, ncomp - 1))
        else:
            self.comp = 0
        self._clear(self.grid)
        self.chips = []
        for i in range(n):
            b = QtWidgets.QPushButton(f"{i + 1}")
            b.setObjectName("Chip")
            b.setCheckable(True)
            b.setChecked(i == sel)
            b.setToolTip(self._group_tip(i))
            b.setFixedWidth(30)
            b.clicked.connect(lambda _=False, k=i: self.select(k))
            self.grid.addWidget(b, i // 6, i % 6)
            self.chips.append(b)

        self._clear(self.body)
        self.rows, self.comp_chips = [], []
        if 0 <= sel < n:
            # 形状条：组内组件切换 + 追加/删除
            comp_row = QtWidgets.QWidget()
            cr = QtWidgets.QHBoxLayout(comp_row)
            cr.setContentsMargins(0, 0, 0, 0)
            cr.setSpacing(4)
            for j in range(ncomp):
                b = QtWidgets.QPushButton(str(j + 1))
                b.setObjectName("Chip")
                b.setCheckable(True)
                b.setChecked(j == self.comp)
                b.setFixedWidth(28)
                b.clicked.connect(lambda _=False, k=j: self.set_comp(k))
                cr.addWidget(b)
                self.comp_chips.append(b)
            cr.addStretch(1)
            self.btn_comp_lin = icon_button(
                "slash", "在组内加一个线性形状（默认叠加）", "dim", 14, 26)
            self.btn_comp_lin.clicked.connect(
                lambda: self._add_comp("linear"))
            cr.addWidget(self.btn_comp_lin)
            self.btn_comp_rad = icon_button(
                "circle", "在组内加一个径向形状（默认叠加）", "dim", 14, 26)
            self.btn_comp_rad.clicked.connect(
                lambda: self._add_comp("radial"))
            cr.addWidget(self.btn_comp_rad)
            self.btn_comp_del = icon_button(
                "trash", "删除当前形状（至少保留一个）", "off", 14, 26)
            self.btn_comp_del.clicked.connect(self._del_comp)
            self.btn_comp_del.setEnabled(ncomp > 1)
            cr.addWidget(self.btn_comp_del)
            self.body.addWidget(comp_row)

            # 当前形状的合成方式
            opt_row = QtWidgets.QWidget()
            orow = QtWidgets.QHBoxLayout(opt_row)
            orow.setContentsMargins(0, 0, 0, 0)
            orow.setSpacing(6)
            self.cb_blend = QtWidgets.QComboBox()
            self.cb_blend.addItem("叠加", "add")
            self.cb_blend.addItem("减去", "subtract")
            self.cb_blend.addItem("交集", "intersect")
            self.cb_blend.setToolTip(
                "这个形状与前面形状的关系：叠加 = 并入选区，\n"
                "减去 = 从选区挖掉，交集 = 只保留共同部分")
            self.cb_blend.setFixedWidth(78)
            self.cb_blend.currentIndexChanged.connect(self._blend)
            orow.addWidget(self.cb_blend)
            self.btn_comp_inv = QtWidgets.QPushButton("形状反选")
            self.btn_comp_inv.setObjectName("Chip")
            self.btn_comp_inv.setCheckable(True)
            self.btn_comp_inv.setToolTip("只反转当前形状的里外")
            self.btn_comp_inv.toggled.connect(self._invert_comp)
            orow.addWidget(self.btn_comp_inv)
            orow.addStretch(1)
            self.body.addWidget(opt_row)

            r = SliderRow(self.store, "羽化", "feather", 0, 100, 1, "{:.0f}",
                          mask=sel, attr="feather", comp=self.comp, name_w=52,
                          hint="右 = 过渡带更宽；0 = 硬边")
            self.rows.append(r)
            self.body.addWidget(r)
            for label, attr, neutral, hint in (
                    ("角度", "angle", 0.0,
                     "旋转当前形状；斜向拖动也能得到椭圆"),
                    ("中点", "midpoint", 50.0,
                     "50 = 默认过渡位置；左 = 靠近起点，右 = 靠近终点")):
                r = SliderRow(self.store, label, attr, -180 if attr == "angle"
                              else 0, 180 if attr == "angle" else 100, 1,
                              "{:+.0f}" if attr == "angle" else "{:.0f}",
                              mask=sel, attr=attr, comp=self.comp, name_w=52,
                              hint=hint, neutral=neutral)
                self.rows.append(r)
                self.body.addWidget(r)

            grp_row = QtWidgets.QWidget()
            grow = QtWidgets.QHBoxLayout(grp_row)
            grow.setContentsMargins(0, 0, 0, 0)
            grow.setSpacing(6)
            self.cb_range = QtWidgets.QCheckBox("亮度范围")
            self.cb_range.setToolTip(
                "只让一定亮度范围内生效：压暗部不碰天空\n"
                "视频导出按首帧亮度计算静态权重")
            self.cb_range.toggled.connect(self._range_toggle)
            grow.addWidget(self.cb_range)
            self.btn_inv = QtWidgets.QPushButton("整组反选")
            self.btn_inv.setObjectName("Chip")
            self.btn_inv.setCheckable(True)
            self.btn_inv.setToolTip("整组选区与未选中的互换")
            self.btn_inv.toggled.connect(self._invert)
            grow.addWidget(self.btn_inv)
            grow.addStretch(1)
            self.body.addWidget(grp_row)

            r = SliderRow(self.store, "强度", "amount", 0, 100, 1, "{:.0f}",
                          mask=sel, attr="amount", name_w=52,
                          hint="按比例削弱整组；0 = 完全不生效", neutral=100.0)
            self.rows.append(r)
            self.body.addWidget(r)

            ron = bool(self.store.mask_range(sel).get("lum_on"))
            if ron:
                for label, key, neutral, hint in (
                        ("亮度下限", "lum_min", 0.0,
                         "低于它的像素不参与；区间内为满分"),
                        ("亮度上限", "lum_max", 100.0,
                         "高于它的像素不参与"),
                        ("范围羽化", "lum_feather", 10.0,
                         "范围边界向外柔化的宽度")):
                    r = SliderRow(
                        self.store, label, key, 0, 100, 1, "{:.0f}",
                        mask=sel, name_w=52, hint=hint, neutral=neutral,
                        getter=lambda k=key: self.store.range_value(sel, k),
                        setter=lambda v, k=key: self.store.range_set(sel, k, v))
                    self.rows.append(r)
                    self.body.addWidget(r)

            for title, attr, specs in (
                    ("调整", "_adj_open", params.MASK_SLIDERS),
                    ("局部细节", "_det_open", params.MASK_SPATIAL_SLIDERS)):
                sub = Group(
                    title, collapsed=not getattr(self, attr),
                    on_toggle=lambda on, a=attr: setattr(self, a, on))
                for key, label, lo, hi, step, fmt, hint in specs:
                    r = SliderRow(self.store, label, key, lo, hi, step, fmt,
                                  idx=None, name_w=60, hint=hint, mask=sel)
                    self.rows.append(r)
                    sub.add(r)
                self.body.addWidget(sub)
            self._sync_selection()
        self.empty.setVisible(n == 0)
        self.hint.setVisible(0 <= sel < n)
        self.btn_del.setEnabled(0 <= sel < n)
        self._built = (n, sel, self.comp, ncomp,
                       bool(0 <= sel < n
                            and self.store.mask_range(sel).get("lum_on")))

    def _sync_selection(self):
        """把当前组的反选、当前形状的混合方式/反选回填到控件。"""
        if not (0 <= self.sel < self.store.mask_count()):
            return
        for j, b in enumerate(self.comp_chips):
            b.setChecked(j == self.comp)
            c = self.store.comp(self.sel, j) or {}
            kind = "线性" if (c.get("kind") or c.get("type")) == "linear" else "径向"
            blend = {"subtract": "减去", "intersect": "交集"}.get(
                c.get("blend", "add"), "叠加")
            b.setToolTip(f"形状 {j + 1} · {kind} · {blend}"
                         + ("（已反选）" if c.get("invert") else ""))
        self.btn_inv.blockSignals(True)
        self.btn_inv.setChecked(bool(self.store.mask_attr(self.sel, "invert")))
        self.btn_inv.blockSignals(False)
        c = self.store.comp(self.sel, self.comp) or {}
        self.cb_blend.blockSignals(True)
        self.cb_blend.setCurrentIndex(
            max(0, self.cb_blend.findData(c.get("blend", "add"))))
        self.cb_blend.blockSignals(False)
        self.btn_comp_inv.blockSignals(True)
        self.btn_comp_inv.setChecked(bool(c.get("invert")))
        self.btn_comp_inv.blockSignals(False)
        self.cb_range.blockSignals(True)
        self.cb_range.setChecked(
            bool(self.store.mask_range(self.sel).get("lum_on")))
        self.cb_range.blockSignals(False)

    # ---- 同步 ----
    def sync(self):
        n = self.store.mask_count()
        if self.sel >= n:
            self.sel = n - 1
        if self.sel < 0 and n:
            self.sel = 0
        ncomp = self.store.comp_count(self.sel) if self.sel >= 0 else 0
        if ncomp:
            self.comp = max(0, min(self.comp, ncomp - 1))
        else:
            self.comp = 0
        ron = bool(0 <= self.sel < n
                   and self.store.mask_range(self.sel).get("lum_on"))
        if (n, self.sel, self.comp, ncomp, ron) != self._built:
            self._rebuild()
        else:
            for r in self.rows:
                r.sync()
            for i, b in enumerate(self.chips):
                b.setToolTip(self._group_tip(i))
            if 0 <= self.sel < n:
                self._sync_selection()
        self.selected.emit(self.sel)
