"""示波器：RGB 叠加直方图 / 亮度直方图 / 波形 / 矢量图。"""
import numpy as np
from PySide6 import QtCore, QtGui, QtWidgets

from lapse.gui import theme

LUMA = np.array([0.2126, 0.7152, 0.0722], dtype=np.float32)
CH_COL = ((255, 90, 90), (120, 235, 120), (95, 160, 255))
VEC_N = 128            # 矢量图色度网格；64 时放大后颗粒感明显


def _bins(u8, ch):
    return np.bincount(u8[..., ch].ravel(), minlength=256).astype(np.float64)


class Scopes(QtWidgets.QWidget):
    MODES = ("直方图", "波形", "矢量")

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setMinimumHeight(190)
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Expanding,
                           QtWidgets.QSizePolicy.Policy.Expanding)
        self.setToolTip("直方图两角亮红 = 黑场/白场溢出；"
                        "「亮度」切到明度直方图")
        self.u8 = None
        self.mode = "直方图"
        self.luma_only = False
        self._hist = None            # 缓存 (3,256) 归一化
        self._wave = None
        self._vec = None

        lay = QtWidgets.QVBoxLayout(self)
        # 与右侧工具面板同样的内缩；页签贴边会被窗口或滚动条裁掉
        lay.setContentsMargins(10, 8, 10, 8)
        lay.setSpacing(6)
        bar = QtWidgets.QHBoxLayout()
        bar.setSpacing(4)
        for m in self.MODES:
            b = QtWidgets.QPushButton(m)
            b.setObjectName("Chip")
            b.setCheckable(True)
            b.setChecked(m == self.mode)
            b.clicked.connect(lambda _=False, x=m: self.set_mode(x))
            bar.addWidget(b)
        bar.addStretch(1)
        self.btn_luma = QtWidgets.QPushButton("亮度")
        self.btn_luma.setObjectName("Chip")
        self.btn_luma.setCheckable(True)
        self.btn_luma.clicked.connect(self.toggle_luma)
        bar.addWidget(self.btn_luma)
        lay.addLayout(bar)

        self.canvas = _ScopeCanvas(self)
        lay.addWidget(self.canvas, 1)

    def toggle_luma(self):
        self.luma_only = self.btn_luma.isChecked()
        self.canvas.update()

    def set_mode(self, m):
        self.mode = m
        self.canvas.update()
        for child in self.findChildren(QtWidgets.QPushButton):
            if child.text() in self.MODES:
                child.setChecked(child.text() == m)

    def set_image(self, u8):
        if u8 is None:
            return
        self.u8 = u8
        self._hist = self._wave = self._vec = None
        self.canvas.update()

    # ---- 数据（惰性计算，按图像对象缓存）----
    def hist(self):
        if self._hist is None and self.u8 is not None:
            h = np.stack([_bins(self.u8, c) for c in range(3)])
            m = h.max()
            self._hist = h / m if m > 0 else h
        return self._hist

    def luma_hist(self):
        if self.u8 is None:
            return None
        l = (self.u8.astype(np.float32) @ LUMA)
        return _bins(l.astype(np.uint8), 0)

    def wave(self):
        """返回 (256, 3, 3)：每列 x 位置上 R/G/B 的 1/50/95 百分位。"""
        if self._wave is None and self.u8 is not None:
            H, W = self.u8.shape[:2]
            step = max(1, W // 256)
            cols = self.u8[:, ::step, :].astype(np.float32)
            n = cols.shape[1]
            idx = np.linspace(0, n - 1, 256).round().astype(int)
            cols = cols[:, idx, :]
            flat = cols.reshape(cols.shape[0], 256, 3)   # (rows, col, ch) 未排序
            out = np.empty((256, 3, 3), dtype=np.float32)
            for c in range(3):
                v = flat[..., c].reshape(-1, 256)
                s = np.sort(v, axis=0)
                m = s.shape[0]
                out[:, c, 0] = s[max(int(m * 0.01), 0)]
                out[:, c, 1] = s[m // 2]
                out[:, c, 2] = s[min(int(m * 0.95), m - 1)]
            self._wave = out
        return self._wave

    def vectorscope(self):
        """返回 (VEC_N×VEC_N 计数图, 最大计数)：Cb/Cr 投影。"""
        if self._vec is None and self.u8 is not None:
            x = self.u8.astype(np.float32) / 255.0
            sub = x[::4, ::4, :]
            b = (sub[..., 2] - sub[..., 0]) * 0.5
            r = (sub[..., 1] - (sub[..., 0] * 0.299 + sub[..., 2] * 0.114)
                 / 0.413) * 0.5
            n = VEC_N
            ix = np.clip(((b + 0.5) * n).astype(np.int32), 0, n - 1).ravel()
            iy = np.clip(((r + 0.5) * n).astype(np.int32), 0, n - 1).ravel()
            hist = np.bincount(iy * n + ix, minlength=n * n).reshape(n, n)
            self._vec = (hist, float(hist.max() or 1))
        return self._vec


class _ScopeCanvas(QtWidgets.QWidget):
    def __init__(self, owner):
        super().__init__()
        self.owner = owner
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Expanding,
                           QtWidgets.QSizePolicy.Policy.Expanding)

    def paintEvent(self, e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.Antialiasing)
        r = self.rect().adjusted(1, 1, -1, -1)
        p.fillRect(r, QtGui.QColor(theme.FIELD))
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER), 1))
        p.drawRect(r)
        m = self.owner.mode
        if m == "直方图":
            self._hist(p, r)
        elif m == "波形":
            self._wave(p, r)
        else:
            self._vec(p, r)
        p.end()

    def _grid(self, p, r, nx=4, ny=4):
        p.setPen(QtGui.QPen(QtGui.QColor(*theme.GRID), 1))
        for i in range(1, nx):
            x = r.left() + r.width() * i / nx
            p.drawLine(QtCore.QPoint(int(x), r.top()),
                       QtCore.QPoint(int(x), r.bottom()))
        for j in range(1, ny):
            y = r.top() + r.height() * j / ny
            p.drawLine(QtCore.QPoint(r.left(), int(y)),
                       QtCore.QPoint(r.right(), int(y)))

    def _hist(self, p, r):
        self._grid(p, r)
        h = self.owner.hist()
        if h is None:
            return
        if self.owner.luma_only:
            lh = self.owner.luma_hist()
            mx = lh.max() or 1.0
            path = QtGui.QPainterPath()
            path.moveTo(r.left(), r.bottom())
            for i in range(256):
                path.lineTo(r.left() + r.width() * i / 255.0,
                            r.bottom() - r.height() * lh[i] / mx)
            path.lineTo(r.right(), r.bottom())
            path.closeSubpath()
            p.fillPath(path, QtGui.QColor(*theme.HIST_L))
            return
        paths = []
        for c in range(3):
            path = QtGui.QPainterPath()
            path.moveTo(r.left(), r.bottom())
            for i in range(256):
                path.lineTo(r.left() + r.width() * i / 255.0,
                            r.bottom() - r.height() * h[c, i])
            path.lineTo(r.right(), r.bottom())
            path.closeSubpath()
            paths.append(path)
        # 三通道叠加：用 screen 混合，重叠处变亮而不是糊成一片灰
        p.setCompositionMode(
            QtGui.QPainter.CompositionMode.CompositionMode_Screen)
        for c, path in enumerate(paths):
            p.fillPath(path, QtGui.QColor(*CH_COL[c], 90))
        p.setCompositionMode(
            QtGui.QPainter.CompositionMode.CompositionMode_SourceOver)
        for c, path in enumerate(paths):
            p.setPen(QtGui.QPen(QtGui.QColor(*CH_COL[c], 200), 1))
            p.drawPath(path)
        self._clipping(p, r, h)

    def _clipping(self, p, r, h):
        """黑场/白场溢出：直方图两角的小三角亮红，比整条红边更安静。"""
        low = h[:, :2].max() > 0.12
        high = h[:, -2:].max() > 0.12
        for left, clip, right in ((r.left() + 2, low, False),
                                  (r.right() - 9, high, True)):
            path = QtGui.QPainterPath()
            if right:
                path.moveTo(left, r.top() + 2)
                path.lineTo(left + 7, r.top() + 2)
                path.lineTo(left + 7, r.top() + 9)
            else:
                path.moveTo(left, r.top() + 2)
                path.lineTo(left + 7, r.top() + 2)
                path.lineTo(left, r.top() + 9)
            path.closeSubpath()
            p.fillPath(path, QtGui.QColor(
                theme.BAD if clip else QtGui.QColor(*theme.GRID)))
        if low or high:
            p.setPen(QtGui.QPen(QtGui.QColor(theme.BAD), 1))
            if low:
                p.drawLine(r.topLeft(), r.bottomLeft())
            if high:
                p.drawLine(r.topRight(), r.bottomRight())

    def _wave(self, p, r):
        self._grid(p, r)
        w = self.owner.wave()
        if w is None:
            return
        xs = r.left() + r.width() * np.arange(256) / 255.0
        bot, hgt = r.bottom(), r.height()
        for c in range(3):
            lo = bot - hgt * w[:, c, 0] / 255.0
            hi = bot - hgt * w[:, c, 2] / 255.0
            med = bot - hgt * w[:, c, 1] / 255.0
            col = QtGui.QColor(*CH_COL[c])
            p.setPen(QtGui.QPen(col, 1))
            band = QtGui.QPainterPath()
            band.moveTo(float(xs[0]), float(lo[0]))
            for i in range(1, 256):
                band.lineTo(float(xs[i]), float(lo[i]))
            for i in range(255, -1, -1):
                band.lineTo(float(xs[i]), float(hi[i]))
            band.closeSubpath()
            p.fillPath(band, QtGui.QColor(col.red(), col.green(), col.blue(), 40))
            p.drawPath(band)
            med_p = QtGui.QPainterPath()
            med_p.moveTo(float(xs[0]), float(med[0]))
            for i in range(1, 256):
                med_p.lineTo(float(xs[i]), float(med[i]))
            p.setPen(QtGui.QPen(col.lighter(130), 1))
            p.drawPath(med_p)

    def _vec(self, p, r):
        v = self.owner.vectorscope()
        if v is None:
            return
        hist, mx = v
        side = min(r.width(), r.height()) - 8
        cx = r.center().x()
        cy = r.center().y()
        # 中心十字 + 肤色线
        p.setPen(QtGui.QPen(QtGui.QColor(*theme.GRID), 1))
        p.drawLine(QtCore.QPoint(int(cx - side / 2), int(cy)),
                   QtCore.QPoint(int(cx + side / 2), int(cy)))
        p.drawLine(QtCore.QPoint(int(cx), int(cy - side / 2)),
                   QtCore.QPoint(int(cx), int(cy + side / 2)))
        img = np.empty((VEC_N, VEC_N, 4), dtype=np.uint8)
        dens = np.clip(np.log1p(hist) / np.log1p(mx), 0, 1)
        img[..., 0] = theme.VEC[0]
        img[..., 1] = theme.VEC[1]
        img[..., 2] = theme.VEC[2]
        img[..., 3] = (dens * 255).astype(np.uint8)
        qimg = QtGui.QImage(img.data, VEC_N, VEC_N, VEC_N * 4,
                           QtGui.QImage.Format.Format_RGBA8888).copy()
        tgt = QtCore.QRectF(cx - side / 2, cy - side / 2, side, side)
        p.setRenderHint(QtGui.QPainter.SmoothPixmapTransform)
        p.drawImage(tgt, qimg)
        p.setPen(QtGui.QPen(QtGui.QColor(120, 235, 120, 120), 1))
        p.drawLine(QtCore.QPoint(int(cx), int(cy)),
                   QtCore.QPoint(int(cx + side * 0.33), int(cy - side * 0.18)))
