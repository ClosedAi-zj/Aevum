"""缩略图浏览器：图库网格 / 显影底部胶片条，后台生成缩略图。"""
import hashlib
import math
import os

import cv2
import numpy as np
from PySide6 import QtCore, QtGui, QtWidgets

from reference import imio, luma
from reference.gui import hints, prefs, theme
from reference.gui.grader import pipeline
from reference.gui.grader import ratings as rating_mod


def _disk_path(path, size):
    """文件路径 + 修改时间 + 尺寸 + 缩略图边长 → 磁盘缓存位置。

    修改时间与文件大小一起进键：原图被覆盖后旧缓存自然失效，不用额外的
    版本字段，也不会读到张冠李戴的缩略图。
    """
    if not prefs.thumb_cache_enabled():
        return None
    try:
        st = os.stat(path)
    except OSError:
        return None
    raw = (f"{os.path.normcase(os.path.abspath(path))}|{st.st_mtime_ns}"
           f"|{st.st_size}|{size}")
    h = hashlib.sha1(raw.encode("utf-8")).hexdigest()
    return os.path.join(prefs.thumb_cache_dir(), h[:2], h[2:] + ".jpg")


def _load_disk(cache_path):
    if not cache_path or not os.path.isfile(cache_path):
        return None
    try:
        buf = np.fromfile(cache_path, dtype=np.uint8)
        im = cv2.imdecode(buf, cv2.IMREAD_COLOR)
        return None if im is None else cv2.cvtColor(im, cv2.COLOR_BGR2RGB)
    except Exception:
        return None


def _save_disk(cache_path, rgb):
    """先写 .part 再原子改名：中途退出不会留下半张图。"""
    if not cache_path:
        return
    try:
        d = os.path.dirname(cache_path)
        os.makedirs(d, exist_ok=True)
        ok, buf = cv2.imencode(
            ".jpg", cv2.cvtColor(rgb, cv2.COLOR_RGB2BGR),
            [cv2.IMWRITE_JPEG_QUALITY, 88])
        if not ok:
            return
        tmp = cache_path + ".part"
        buf.tofile(tmp)
        os.replace(tmp, cache_path)
    except Exception:
        pass


class ThumbWorker(QtCore.QObject):
    ready = QtCore.Signal(str, object)      # path, QImage|None

    def __init__(self, parent=None):
        super().__init__(parent)
        self._mutex = QtCore.QMutex()
        self._cond = QtCore.QWaitCondition()
        self._queue = []
        self._stop = False
        self._gen = 0
        self.cache = {}                     # (path, size) -> QImage|None

    def enqueue(self, paths, gen, size):
        with QtCore.QMutexLocker(self._mutex):
            if gen != self._gen:
                self._queue = []
                self._gen = gen
            self._queue.append((list(paths), size))
            self._cond.wakeOne()

    def loop(self):
        while True:
            with QtCore.QMutexLocker(self._mutex):
                while not self._queue and not self._stop:
                    self._cond.wait(self._mutex)
                if self._stop:
                    return
                batch, size = self._queue[0]
                self._queue = self._queue[1:]
            for p in batch:
                with QtCore.QMutexLocker(self._mutex):
                    if self._queue:
                        break            # 有新任务就丢弃当前批次剩余
                    if self._stop:
                        return
                key = (p, size)
                q = self.cache.get(key)
                if q is None:
                    disk = _disk_path(p, size)
                    rgb = _load_disk(disk)
                    if rgb is not None:
                        q = pipeline.to_qimage_u8(rgb)
                    else:
                        rgb = pipeline.thumb_u8(p, size)
                        q = (pipeline.to_qimage_u8(rgb)
                             if rgb is not None else None)
                        if rgb is not None:
                            _save_disk(disk, rgb)
                    self.cache[key] = q
                self.ready.emit(p, q)

    def shutdown(self):
        with QtCore.QMutexLocker(self._mutex):
            self._stop = True
            self._cond.wakeAll()


class LumaWorker(QtCore.QThread):
    """后台测逐帧亮度；结果按代次回传，换素材后旧结果自动作废。"""

    ready = QtCore.Signal(int, object, object)

    def __init__(self, gen, paths, parent=None):
        super().__init__(parent)
        self.gen = gen
        self.paths = list(paths)
        self._stop = False

    def request_stop(self):
        self._stop = True

    def run(self):
        try:
            vals, issues = luma.measure_luma(
                self.paths, 512, cancel=lambda: self._stop)
        except Exception:
            return
        if not self._stop:
            self.ready.emit(self.gen, vals, issues)


class LumaCurve(QtWidgets.QWidget):
    """胶片条上方的逐帧亮度曲线：能一眼看出哪几帧在闪，也能拖出曝光过渡。

    只读时它只是测量条；打开 `set_editable(True)` 之后按住上下拖就是给那一帧
    写曝光偏移（EV），开发模块把它落成关键帧，于是日转夜那条坡可以手改。
    拖的时候画两条线：实测（原来的填充曲线）和"按现在的曝光会亮成什么样"。
    """

    seeked = QtCore.Signal(int)
    evBegin = QtCore.Signal(int)             # 按下：先跳到这一帧，改的才是看到的
    evEdit = QtCore.Signal(int, float)       # 帧号 + 相对按下起点的 EV 增量
    evEnd = QtCore.Signal()                  # 松手：这一次拖动算一步撤销

    EV_FULL = 4.0            # 拖满整个绘图高度 = 4 档，和曝光滑块同量程
    PAD = 4.0

    def __init__(self, parent=None):
        super().__init__(parent)
        self.values = []
        self.bad = set()
        self.steps = []
        self.current = 0
        self.editable = False
        self.evs = []
        self.corrected = []
        self.drag = None
        self.setFixedHeight(46)
        self.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self._sync_tip()

    def set_values(self, values, bad=()):
        self.values = [float(v) if v is not None else None for v in values]
        self.bad = set(bad)
        # 重新测量后旧偏移要立刻贴到新数据上，否则"校正后"那条线会留在上一批数字上
        if self.evs:
            self.corrected = [self._apply_ev(v, e) if v is not None else None
                              for v, e in zip(self.values, self.evs)]
        self.update()

    def set_editable(self, on):
        """打开后按住曲线上下拖就是改这一帧的曝光；单击仍然只是跳帧。"""
        on = bool(on)
        if on == self.editable:
            return
        self.editable = on
        if not on:
            self.drag = None
        self._sync_tip()
        self.update()

    def set_offsets(self, evs):
        """逐帧实际吃到的曝光偏移（EV）：画成第二条"校正后"的线。"""
        out = []
        for v in evs or ():
            try:
                out.append(float(v))
            except (TypeError, ValueError):
                out.append(0.0)
        self.evs = out
        self.corrected = [self._apply_ev(v, e) if v is not None else None
                          for v, e in zip(self.values, out)] if out else []
        self.update()

    @staticmethod
    def _apply_ev(value, ev):
        """把 EV 叠到 gamma 编码的实测亮度上（近似：编码值 × 2^(EV/2.2)）。"""
        if value is None:
            return None
        return min(1.0, max(0.0, float(value) * (2.0 ** (float(ev) / 2.2))))

    def _ghost(self):
        """实测和结果差得出来才画第二条：完全重合时白线只会把测量线盖掉。"""
        for v, c in zip(self.values, self.corrected):
            if v is not None and c is not None and abs(v - c) > 0.004:
                return self.corrected
        return None

    def set_steps(self, steps):
        """曝光台阶：[(帧号, 档差, 标签)]，画成竖线并写短标签。"""
        self.steps = [(int(i), float(d), str(label))
                      for i, d, label in steps]
        self._sync_tip()
        self.update()

    def _sync_tip(self):
        tip = ["逐帧亮度：点曲线跳到对应帧；红点是读不出的帧"]
        if self.editable:
            tip.append(hints.text("显影/亮度曲线"))
        if self.steps:
            tip.append("曝光台阶：")
            tip.extend(f"第 {i + 1} 帧  {label}" for i, _d, label in self.steps)
        self.setToolTip("\n".join(tip))

    def clear(self):
        self.values = []
        self.bad = set()
        self.evs = []
        self.corrected = []
        self.drag = None
        self.update()

    def set_current(self, i):
        self.current = int(i)
        self.update()

    def _plot(self):
        """绘图区：给出高度换算与取某帧的横坐标，绘制和拖拽共用一套几何。"""
        r = self.rect().adjusted(0, 0, 0, -1)
        h = max(1.0, r.height() - 2 * self.PAD)
        return r, h

    def _index_at(self, x):
        n = len(self.values)
        if n < 2:
            return 0
        # 和 paintEvent 里同一个横坐标公式：按下哪帧就改哪帧，别差半帧
        return int(round(min(max(x / max(1, self.width() - 1), 0.0), 1.0) * (n - 1)))

    def mousePressEvent(self, e):
        n = len(self.values)
        if n > 1 and e.button() == QtCore.Qt.MouseButton.LeftButton:
            i = self._index_at(e.position().x())
            if self.editable:
                self.drag = [i, float(e.position().y()), 0.0]
                self.setCursor(QtCore.Qt.CursorShape.SizeVerCursor)
                self.evBegin.emit(i)
                self.update()
            else:
                self.seeked.emit(i)
            e.accept()
            return
        super().mousePressEvent(e)

    def mouseMoveEvent(self, e):
        d = self.drag
        if d and e.buttons() & QtCore.Qt.MouseButton.LeftButton:
            _r, h = self._plot()
            step = self.EV_FULL / h
            if e.modifiers() & QtCore.Qt.KeyboardModifier.AltModifier:
                step /= 5.0                      # 与滑块同一套精调比例
            d[2] = (d[1] - float(e.position().y())) * step
            self.evEdit.emit(d[0], d[2])
            self.update()
            e.accept()
            return
        super().mouseMoveEvent(e)

    def mouseReleaseEvent(self, e):
        if self.drag and e.button() == QtCore.Qt.MouseButton.LeftButton:
            self.drag = None
            self.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
            self.evEnd.emit()
            self.update()
            e.accept()
            return
        super().mouseReleaseEvent(e)

    def paintEvent(self, _e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing, True)
        r, h = self._plot()
        p.fillRect(r, QtGui.QColor(theme.BG2))
        vals = self.values
        if len(vals) < 2:
            p.end()
            return
        # 上下界把"校正后"那条也算进来：拖出坡时两条线都要看得见
        ghost = self._ghost()
        good = [v for v in vals if v is not None]
        if ghost:
            good += [v for v in ghost if v is not None]
        if not good:
            p.end()
            return
        lo, hi = min(good), max(good)
        if hi - lo < 0.02:
            mid = (hi + lo) * 0.5
            lo, hi = mid - 0.01, mid + 0.01
        pad = self.PAD

        def pt(i, v):
            x = r.left() + i / (len(vals) - 1) * max(1, r.width() - 1)
            y = r.bottom() - pad - (v - lo) / (hi - lo) * h
            return QtCore.QPointF(x, y)

        path = QtGui.QPainterPath()
        started = False
        for i, v in enumerate(vals):
            if v is None:
                started = False
                continue
            if not started:
                path.moveTo(pt(i, v))
                started = True
            else:
                path.lineTo(pt(i, v))
        fill = QtGui.QPainterPath(path)
        fill.lineTo(r.right(), r.bottom())
        fill.lineTo(r.left(), r.bottom())
        fill.closeSubpath()
        c = QtGui.QColor(theme.ACCENT)
        c.setAlpha(30)
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(c)
        p.drawPath(fill)
        p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
        p.setPen(QtGui.QPen(QtGui.QColor(theme.ACCENT_HI), 1.2))
        p.drawPath(path)

        if ghost:
            cp = QtGui.QPainterPath()
            started = False
            for i, v in enumerate(ghost[:len(vals)]):
                if v is None:
                    started = False
                    continue
                if not started:
                    cp.moveTo(pt(i, v))
                    started = True
                else:
                    cp.lineTo(pt(i, v))
            p.setPen(QtGui.QPen(QtGui.QColor(theme.TEXT), 1.3))
            p.drawPath(cp)

        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(QtGui.QColor("#e05a5a"))
        for i in self.bad:
            if 0 <= i < len(vals):
                x = r.left() + i / (len(vals) - 1) * max(1, r.width() - 1)
                p.drawEllipse(QtCore.QPointF(x, r.bottom() - 3), 2.0, 2.0)

        if self.steps:
            marks = [(r.left() + idx / (len(vals) - 1) * max(1, r.width() - 1), d)
                     for idx, d, _label in self.steps if 0 <= idx < len(vals)]
            p.setPen(QtGui.QPen(QtGui.QColor(*theme.VEC), 1,
                                QtCore.Qt.PenStyle.DashLine))
            for x, _d in marks:
                p.drawLine(QtCore.QPointF(x, r.top()),
                           QtCore.QPointF(x, r.bottom()))
            if len(marks) <= 8:
                f = p.font()
                f.setPixelSize(theme.fs(11))
                p.setFont(f)
                p.setPen(QtGui.QColor(*theme.VEC))
                last = -999.0
                for x, d in marks:
                    if x - last < 26:
                        continue
                    p.drawText(QtCore.QRectF(x - 22, r.top(), 44, 12),
                               QtCore.Qt.AlignmentFlag.AlignCenter,
                               f"{d:+.1f}")
                    last = x

        if 0 <= self.current < len(vals):
            x = r.left() + self.current / (len(vals) - 1) * max(
                1, r.width() - 1)
            p.setPen(QtGui.QPen(QtGui.QColor("#ffffff"), 1))
            p.drawLine(QtCore.QPointF(x, r.top()),
                       QtCore.QPointF(x, r.bottom()))
        if self.drag and abs(self.drag[2]) > 1e-4 and self.editable:
            # 拖的时候把改了多少档写在点旁边：不写就没人敢确定自己拖到了哪儿
            i, _y0, dv = self.drag
            v = (self.corrected[i] if i < len(self.corrected)
                 and self.corrected[i] is not None
                 else (vals[i] if i < len(vals) and vals[i] is not None else None))
            if v is not None:
                f = p.font()
                f.setPixelSize(theme.fs(12))
                p.setFont(f)
                p.setPen(QtGui.QColor(theme.TEXT))
                box = QtCore.QRectF(min(max(pt(i, v).x() - 40, r.left()),
                                        max(r.left(), r.right() - 80)),
                                    r.top() + 1, 80, 13)
                p.drawText(box, QtCore.Qt.AlignmentFlag.AlignCenter,
                           f"第 {i + 1} 帧 {dv:+.2f} EV")
        p.end()


class _FilmList(QtWidgets.QListWidget):
    """只横向滚动的胶片条：滚动条会抢走缩略图的高度，统一用滚轮。"""

    def __init__(self, browser=None, parent=None):
        super().__init__(parent)
        self.browser = browser

    def wheelEvent(self, ev):
        if (self.flow() == QtWidgets.QListView.Flow.LeftToRight
                and not self.wrapping()):
            d = ev.pixelDelta().x() or ev.angleDelta().y()
            bar = self.horizontalScrollBar()
            bar.setValue(bar.value() - d)
            ev.accept()
            return
        super().wheelEvent(ev)

    def keyPressEvent(self, ev):
        b = self.browser
        if b is not None and b.rate_by_key(ev.key()):
            ev.accept()
            return
        super().keyPressEvent(ev)

    def contextMenuEvent(self, ev):
        b = self.browser
        if b is not None and b.has_ratings():
            it = self.item(self.indexAt(ev.pos()).row())
            if it is not None and not self.isSelected(it):
                self.setCurrentItem(it)
            b.show_rating_menu(ev.globalPos())
            ev.accept()
            return
        super().contextMenuEvent(ev)


class RatingDelegate(QtWidgets.QStyledItemDelegate):
    """缩略图右下角画星级、左上角画色标方块。网格与胶片条共用一套。"""

    STAR_R = 4.4
    STAR_GAP = 11.0
    CHIP = 11

    def __init__(self, browser):
        super().__init__(browser.list)
        self.browser = browser

    def paint(self, painter, option, index):
        super().paint(painter, option, index)
        path = index.data(QtCore.Qt.ItemDataRole.UserRole)
        if not path:
            return
        stars, label = self.browser.rating_for(path)
        if not stars and label == "none":
            return
        box = self._image_rect(option, index)
        painter.save()
        painter.setRenderHint(
            QtGui.QPainter.RenderHint.Antialiasing, True)
        if label != "none":
            rgb = rating_mod.color_rgb(label) or (200, 200, 200)
            painter.setPen(QtGui.QPen(QtGui.QColor(0, 0, 0, 140), 1))
            painter.setBrush(QtGui.QColor(*rgb))
            painter.drawRoundedRect(box.left() + 5, box.top() + 5,
                                    self.CHIP, self.CHIP, 2.5, 2.5)
        if stars:
            gold = QtGui.QColor("#f2c14e")
            edge = QtGui.QPen(QtGui.QColor(0, 0, 0, 170), 0.9)
            cy = box.bottom() - 9.0
            for k in range(stars):
                cx = box.right() - 9.0 - k * self.STAR_GAP
                painter.setPen(edge)
                painter.setBrush(gold)
                painter.drawPolygon(_star_polygon(cx, cy, self.STAR_R))
        painter.restore()

    def _icon_rect(self, option):
        """缩略图真正占的那块：星级要贴着图，不能压到网格下方的文件名。"""
        r = QtCore.QRect()
        w = option.widget
        try:
            style = w.style() if w is not None else self.parent().style()
            r = style.subElementRect(
                QtWidgets.QStyle.SubElement.SE_ItemViewItemDecoration,
                option, w)
        except Exception:
            r = QtCore.QRect()
        if r.width() < 24 or r.height() < 18:
            r = self._fallback_rect(option)
        return r

    def _fallback_rect(self, option):
        rect = option.rect
        icon = self.browser.list.iconSize()
        return QtCore.QRect(rect.center().x() - icon.width() // 2,
                           rect.top() + 2, icon.width(), icon.height())

    def _image_rect(self, option, index):
        """缩略图真正画出来的那块。

        样式给的装饰区是格子按 iconSize 算的，图按比例缩放后左右会留白；标记
        照装饰区画就会飘到图外面去，所以按实际位图尺寸缩回去。
        """
        box = self._icon_rect(option)
        icon = index.data(QtCore.Qt.ItemDataRole.DecorationRole)
        if not isinstance(icon, QtGui.QIcon):
            return box
        pm = icon.pixmap(box.size())
        if pm.isNull() or pm.width() >= box.width():
            return box
        return QtCore.QRect(
            box.center().x() - pm.width() // 2,
            box.center().y() - pm.height() // 2, pm.width(), pm.height())


def _star_polygon(cx, cy, r):
    pts = []
    for k in range(10):
        ang = -math.pi / 2 + k * math.pi / 5
        rad = r if k % 2 == 0 else r * 0.44
        pts.append(QtCore.QPointF(cx + rad * math.cos(ang),
                                  cy + rad * math.sin(ang)))
    return QtGui.QPolygonF(pts)


class Browser(QtWidgets.QWidget):
    """图库网格 / 显影底部胶片条。缩略图在后台线程生成。"""
    picked = QtCore.Signal(int)      # 单击选中
    activated = QtCore.Signal(int)   # 双击进入显影
    rated = QtCore.Signal()          # 评过级：让外部刷新状态与筛选

    GRID_CELL = QtCore.QSize(184, 140)     # 网格单元格：图标 + 下方文件名
    STRIP_CELL = QtCore.QSize(128, 88)     # 胶片条单元格：只有图标

    def __init__(self, parent=None):
        super().__init__(parent)
        self.paths = []
        self.index = 0
        self.gen = 0
        self.size = 128
        self.mode = None
        self.ratings = None
        self._filter = {}
        self._qa_kinds = {}
        self.rows = []              # 视图行 -> self.paths 下标
        self._row_of = {}           # 路径 -> 视图行
        self._just_rated = []       # 最近一次 rate() 打的那几帧
        self._luma_values = None
        self._luma_full = None
        self._luma_bad = set()
        self._luma_paths = []
        self._luma_sig = None
        self._luma_workers = []

        lay = QtWidgets.QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        self.curve = LumaCurve(self)
        self.curve.hide()
        self.curve.seeked.connect(self._seek)
        lay.addWidget(self.curve)
        self.list = _FilmList(self, self)
        self.list.setViewMode(QtWidgets.QListView.ViewMode.IconMode)
        self.list.setResizeMode(QtWidgets.QListView.ResizeMode.Adjust)
        self.list.setMovement(QtWidgets.QListView.Movement.Static)
        self.list.setHorizontalScrollMode(
            QtWidgets.QAbstractItemView.ScrollMode.ScrollPerPixel)
        self.list.setSpacing(4)
        self.list.setUniformItemSizes(True)
        self.list.setSelectionMode(
            QtWidgets.QAbstractItemView.SelectionMode.SingleSelection)
        self.list.setWordWrap(False)
        self.list.setItemDelegate(RatingDelegate(self))
        self.list.setFocusPolicy(
            QtCore.Qt.FocusPolicy.StrongFocus)
        self.list.setToolTip(
            "按 1~5 打星、0 清除；右键选色标。网格页可多选批量打分。")
        self.list.itemClicked.connect(
            lambda it: self.picked.emit(self._path_index(it)))
        self.list.itemDoubleClicked.connect(
            lambda it: self.activated.emit(self._path_index(it)))
        lay.addWidget(self.list)

        self.thread = QtCore.QThread(self)
        self.worker = ThumbWorker()
        self.worker.moveToThread(self.thread)
        self.thread.started.connect(self.worker.loop)
        self.worker.ready.connect(self._on_ready)
        self.thread.start()

    # ---- 模式 ----
    def set_mode(self, mode):
        if mode == self.mode:
            return
        self.mode = mode
        grid = mode == "grid"
        cell = self.GRID_CELL if grid else self.STRIP_CELL
        self.list.setViewMode(QtWidgets.QListView.ViewMode.IconMode)
        self.list.setFlow(QtWidgets.QListView.Flow.LeftToRight)
        self.list.setWrapping(grid)
        # Batched：只重排进入视口的项，大序列首屏不卡。
        self.list.setLayoutMode(QtWidgets.QListView.LayoutMode.Batched)
        self.list.setSelectionMode(
            QtWidgets.QAbstractItemView.SelectionMode.ExtendedSelection
            if grid else
            QtWidgets.QAbstractItemView.SelectionMode.SingleSelection)
        # 胶片条隐藏横向滚动条，改用滚轮；触控板和左右键仍然可用。
        self.list.setHorizontalScrollBarPolicy(
            QtCore.Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.list.setVerticalScrollBarPolicy(
            QtCore.Qt.ScrollBarPolicy.ScrollBarAsNeeded if grid else
            QtCore.Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        # IconMode 不给 gridSize/sizeHint 时，项会塌成一行的文字高度。
        self.list.setGridSize(cell)
        self.list.setIconSize(QtCore.QSize(
            cell.width() - 12, cell.height() - (28 if grid else 8)))
        self.size = 220 if grid else 110
        self.set_items(self.paths, self.index)
        self._apply_luma()
        self._sync_min_height()

    def _sync_min_height(self):
        """胶片条不能矮过一个单元格。

        视口一被压到格子以下，缩略图下沿和贴在下面的星级就一起裁没了——亮度曲
        线出现时最容易撞车，所以高度得跟着曲线一起算。
        """
        if self.mode != "strip":
            self.setMinimumHeight(0)
            return
        curve = 0
        if self.curve.isVisibleTo(self):
            # 曲线是 setFixedHeight 的，sizeHint 在这种控件上是 -1，得读上限
            curve = max(self.curve.maximumHeight(), self.curve.height())
        # 18 是给列表边框和视口边距留的：少 2px 就把星级压到裁切线上
        self.setMinimumHeight(self.STRIP_CELL.height() + 18 + curve)

    def minimumSizeHint(self):
        return QtCore.QSize(120, 60)

    # ---- 内容 ----
    def set_items(self, paths, current=0):
        paths = list(paths)
        changed = paths != self.paths
        self.paths = paths
        self.index = max(0, min(current, len(self.paths) - 1))
        if changed:
            self.gen += 1
            self._reset_luma()
        self._rebuild()

    def _rebuild(self, keep=()):
        """按当前筛选重建视图；self.paths 始终保持全量，导出与体检看它。

        keep 是刚打过星的那几帧：clear() 会把选中一起抹掉，重建后得把它们
        重新选上，否则批量打分一按第二下就只选中一帧了。
        """
        cell = self.GRID_CELL if self.mode == "grid" else self.STRIP_CELL
        grid = self.mode == "grid"
        self.rows = [i for i, p in enumerate(self.paths) if self._keep(p)]
        self._row_of = {self.paths[i]: r
                        for r, i in enumerate(self.rows)}
        self.list.clear()
        for i in self.rows:
            p = self.paths[i]
            it = QtWidgets.QListWidgetItem()
            it.setText(os.path.splitext(os.path.basename(p))[0]
                       if grid else "")
            it.setSizeHint(cell)
            it.setToolTip(p)
            it.setData(QtCore.Qt.ItemDataRole.UserRole, p)
            self.list.addItem(it)
        if self.rows:
            rows = [self._row_of[p] for p in keep if p in self._row_of]
            if rows:
                self._select_rows(rows)
            else:
                row = self._row_of.get(self.paths[self.index])
                if row is not None:
                    self.list.setCurrentRow(row)
            self.worker.enqueue([self.paths[i] for i in self.rows],
                                self.gen, self.size)
            self._start_luma()
        else:
            self._apply_luma()

    def _select_rows(self, rows):
        """把视图里的这几行重新选上，当前行停在第一个。"""
        sm = self.list.selectionModel()
        model = self.list.model()
        if sm is None or model is None:
            return
        # 顺序不能反：IconMode 下 setCurrentRow 会把选中收成它自己那一行
        self.list.setCurrentRow(rows[0])
        sel = QtCore.QItemSelection()
        for r in rows:
            idx = model.index(r, 0)
            sel.select(idx, idx)
        sm.select(sel, QtCore.QItemSelectionModel.SelectionFlag.ClearAndSelect)

    def _path_index(self, it):
        row = self.list.row(it)
        return self.rows[row] if 0 <= row < len(self.rows) else -1

    def _on_ready(self, path, q):
        if q is None:
            return
        row = self._row_of.get(path)
        if row is None:
            return
        it = self.list.item(row)
        if it is None:
            return
        # QPixmap 只能在 GUI 线程建；工作线程交过来的是 QImage
        it.setIcon(QtGui.QIcon(QtGui.QPixmap.fromImage(q)))

    def set_current(self, i):
        if 0 <= i < len(self.paths):
            self.index = i
            row = self._row_of.get(self.paths[i])
            if row is not None:
                self.list.setCurrentRow(row)
            self.curve.set_current(i)

    # ---- 评级 ----
    def set_ratings(self, ratings):
        """挂上这个序列的评级表；换素材时由调色台重建。"""
        self.ratings = ratings
        self.list.viewport().update()

    def has_ratings(self):
        return self.ratings is not None

    def rating_for(self, path):
        return self.ratings.get(path) if self.ratings is not None else (0, "none")

    def selected_paths(self):
        """当前选中的原始路径（网格页可多选）。"""
        rows = set()
        sm = self.list.selectionModel()
        if sm is not None:
            for idx in sm.selectedIndexes():
                rows.add(idx.row())
        if not rows:
            cur = self.list.currentRow()
            if cur >= 0:
                rows.add(cur)
        out = []
        for r in sorted(rows):
            if 0 <= r < len(self.rows):
                i = self.rows[r]
                if 0 <= i < len(self.paths):
                    out.append(self.paths[i])
        return out

    def rate(self, stars=None, label=None):
        """给选中帧打星 / 标色；参数为 None 表示那一项保持不变。返回改了几帧。"""
        if self.ratings is None:
            return 0
        paths = self.selected_paths()
        if not paths:
            return 0
        for p in paths:
            self.ratings.apply(p, stars, label)
        self.ratings.save()
        self._just_rated = list(paths)
        if self._filter.get("min_stars") or self._filter.get("label"):
            self._rebuild(paths)   # 星级变了，进出筛选的帧跟着变
        else:
            self.list.viewport().update()
        self.rated.emit()
        return len(paths)

    def just_rated(self):
        """刚打过星的那几帧。筛选会把它们踢出视图，状态栏只能报这个。"""
        return list(self._just_rated)

    def rate_by_key(self, key):
        """键盘 0~5 设星；吃掉这个键才返回 True。"""
        digits = {QtCore.Qt.Key.Key_0: 0, QtCore.Qt.Key.Key_1: 1,
                  QtCore.Qt.Key.Key_2: 2, QtCore.Qt.Key.Key_3: 3,
                  QtCore.Qt.Key.Key_4: 4, QtCore.Qt.Key.Key_5: 5,
                  QtCore.Qt.Key.Key_Clear: 0}
        n = digits.get(int(key))
        if n is None:
            return False
        return self.rate(n) > 0

    def build_rating_menu(self):
        """右键菜单单独建出来：测试要检查条目，也要能真的触发某一项。"""
        menu = QtWidgets.QMenu(self)
        m_stars = menu.addMenu("星级")
        for n in range(rating_mod.MAX_STARS, -1, -1):
            act = m_stars.addAction("%d 星" % n if n else "清除星级")
            act.triggered.connect(lambda _=False, k=n: self.rate(k))
        m_colors = menu.addMenu("色标")
        for color in rating_mod.COLORS:
            act = m_colors.addAction(rating_mod.color_name(color))
            rgb = rating_mod.color_rgb(color)
            if rgb:
                pm = QtGui.QPixmap(12, 12)
                pm.fill(QtGui.QColor(*rgb))
                act.setIcon(QtGui.QIcon(pm))
            act.triggered.connect(lambda _=False, v=color: self.rate(None, v))
        act = menu.addAction("清除评级")
        act.triggered.connect(lambda: self.rate(0, "none"))
        return menu

    def show_rating_menu(self, pos):
        menu = self.build_rating_menu()
        menu.exec(pos)
        menu.deleteLater()

    # ---- 筛选（只动视图，self.paths 始终是全量）----
    def set_filter(self, spec):
        spec = dict(spec or {})
        if spec == self._filter:
            return
        self._filter = spec
        self._rebuild()

    def filter_spec(self):
        return dict(self._filter)

    def set_qa_kinds(self, mapping):
        """体检结论：normcase 路径 -> 异常类型元组。没跑过就是空表。"""
        self._qa_kinds = dict(mapping or {})
        if self._filter.get("qa_only"):
            self._rebuild()

    def _kinds_for(self, path):
        return self._qa_kinds.get(
            os.path.normcase(os.path.normpath(path)), ())

    def _keep(self, path):
        f = self._filter
        if not f:
            return True
        need = int(f.get("min_stars") or 0)
        if need and self.rating_for(path)[0] < need:
            return False
        want_label = f.get("label")
        if want_label and self.rating_for(path)[1] != want_label:
            return False
        if f.get("qa_only"):
            kinds = self._kinds_for(path)
            if not kinds:
                return False
            want_kind = f.get("qa_kind")
            if want_kind and want_kind not in kinds:
                return False
        return True

    def filtered_paths(self):
        """当前视图里的帧，按原顺序；「只导出筛选结果」用它。"""
        return [self.paths[i] for i in self.rows if 0 <= i < len(self.paths)]

    # ---- 亮度曲线 ----
    def set_steps(self, steps):
        self.curve.set_steps(steps)

    def set_curve_editable(self, on):
        self.curve.set_editable(on)

    def set_curve_offsets(self, evs):
        self.curve.set_offsets(evs)

    def _reset_luma(self):
        self._luma_values = None
        self._luma_full = None
        self._luma_bad = set()
        self._luma_paths = []
        self._luma_sig = None
        self.curve.clear()
        self.curve.set_steps([])
        for w in self._luma_workers:
            w.request_stop()

    def _start_luma(self):
        if self.mode != "strip":
            self._apply_luma()
            return
        imgs = [p for p in self.paths
                if os.path.splitext(p)[1].lower() in imio.IMG_EXTENSIONS
                or imio.is_raw(p)]
        sig = tuple(imgs)
        if sig == self._luma_sig:
            self._apply_luma()
            return
        self._luma_values = None
        self._luma_full = None
        self._luma_bad = set()
        self._luma_paths = imgs
        self._luma_sig = sig
        self.curve.clear()
        for w in self._luma_workers:
            w.request_stop()
        if len(imgs) < 2:
            return
        w = LumaWorker(self.gen, imgs, self)
        self._luma_workers.append(w)
        w.ready.connect(self._on_luma)
        w.finished.connect(lambda w=w: self._forget_worker(w))
        w.start()

    def _forget_worker(self, w):
        try:
            self._luma_workers.remove(w)
        except ValueError:
            pass

    def _on_luma(self, gen, vals, issues):
        if gen != self.gen or vals is None:
            return
        idx = {p: i for i, p in enumerate(self.paths)}
        out = [None] * len(self.paths)
        full = [None] * len(self.paths)
        bad = set()
        for p, v in zip(self._luma_paths, vals):
            i = idx.get(p)
            if i is None:
                continue
            if v is None:
                bad.add(i)
            else:
                out[i] = v[0]
                full[i] = v
        self._luma_values = out
        self._luma_full = full
        self._luma_bad = bad
        self._apply_luma()

    def _apply_luma(self):
        vals = self._luma_values
        show = (self.mode == "strip" and vals is not None
                and sum(v is not None for v in vals) >= 2)
        self.curve.setVisible(show)
        self._sync_min_height()
        if show:
            self.curve.set_values(vals, self._luma_bad)
            self.curve.set_current(self.index)

    def means_for(self, paths):
        """按路径取已测好的亮度均值；覆盖不足返回 None（台阶检测省一次全扫）。"""
        vals = self._luma_values
        if not vals:
            return None
        table = {p: v for p, v in zip(self.paths, vals)}
        out = [table.get(p) for p in paths]
        if sum(v is not None for v in out) < max(2, len(paths) // 2):
            return None
        return out

    def luma_for(self, paths):
        """按路径取完整测量 (mean, median, center, sharp)。

        体检要用清晰度，而曲线只存均值，所以这里单独留一份。覆盖不满就返回
        None，让调用方自己扫一遍——半套测量算不出可靠的离群门槛。
        """
        full = self._luma_full
        if not full or len(full) != len(self.paths):
            return None
        table = dict(zip(self.paths, full))
        out = [table.get(p) for p in paths]
        if any(v is None for v in out):
            return None
        return out

    def _seek(self, i):
        if 0 <= i < len(self.paths):
            self.set_current(i)
            self.picked.emit(i)

    def shutdown(self):
        for w in list(self._luma_workers):
            w.request_stop()
        for w in list(self._luma_workers):
            w.wait(3000)
        self.worker.shutdown()
        self.thread.quit()
        self.thread.wait(2000)
