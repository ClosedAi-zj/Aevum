"""预览视图：缩放 / 平移 / 前后分割对比 / 局部蒙版把手。"""
import weakref

import numpy as np
from PySide6 import QtCore, QtGui, QtWidgets

from reference import grade as gc
from reference.gui import theme

# 画布底色是"设进去"的刷子，不像 QSS 会自动跟随皮肤：换肤时统一重设
_VIEWS = []

# 蒙版范围叠加图只需要指示范围，512 长边放大后靠平滑插值过渡就够了；
# 逐像素算权重场会让拖动把手每帧都卡一下
MASK_CAP = 512


def _mtype(m):
    return m.get("kind") or m.get("type") or "linear"


def _range_on(d):
    return bool(((d or {}).get("range") or {}).get("lum_on"))


def _uses_range(m):
    return _range_on(m) or any(_range_on(c)
                               for c in (m.get("components") or ()))


def _num(v, default):
    try:
        return float(default if v is None else v)
    except (TypeError, ValueError):
        return float(default)


def _recolor():
    live = []
    for ref in _VIEWS:
        v = ref()
        if v is not None:
            v.setBackgroundBrush(QtGui.QColor(theme.CANVAS))
            live.append(ref)
    _VIEWS[:] = live


theme.on_change(_recolor)


class ImageView(QtWidgets.QGraphicsView):
    split_changed = QtCore.Signal(float)
    zoom_changed = QtCore.Signal(float)
    # 拖动蒙版把手：(组, 组件, "a"/"b"/"body", u, v)
    mask_dragged = QtCore.Signal(int, int, str, float, float)
    mask_component_selected = QtCore.Signal(int)
    mask_drag_state = QtCore.Signal(bool)
    mask_overlay_toggle = QtCore.Signal()

    def __init__(self, parent=None):
        super().__init__(parent)
        _VIEWS.append(weakref.ref(self))
        self.setScene(QtWidgets.QGraphicsScene(self))
        self.setRenderHint(QtGui.QPainter.Antialiasing)
        self.setTransformationAnchor(QtWidgets.QGraphicsView.ViewportAnchor.NoAnchor)
        self.setResizeAnchor(QtWidgets.QGraphicsView.ViewportAnchor.AnchorViewCenter)
        self.setHorizontalScrollBarPolicy(QtCore.Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.setVerticalScrollBarPolicy(QtCore.Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.setBackgroundBrush(QtGui.QColor(theme.CANVAS))
        self.setDragMode(QtWidgets.QGraphicsView.DragMode.ScrollHandDrag)
        self.setMouseTracking(True)

        self.before = self.scene().addPixmap(QtGui.QPixmap())
        self.ref = self.scene().addPixmap(QtGui.QPixmap())
        self.ref.setZValue(-1)
        self.ref.hide()
        self.after = self.scene().addPixmap(QtGui.QPixmap())
        self.after.setZValue(1)
        self.after_full = QtGui.QPixmap()
        self.ref_full = QtGui.QPixmap()
        self._pix_w = self._pix_h = 0

        self.split = 0.5
        self.compare = False
        self._zoom = 1.0
        self._fit_zoom = 1.0
        self._drag_split = False
        # 局部蒙版：几何直接从画面上拖，视图只存引用（store 原地改这些字典）
        self.masks = []
        self.mask_sel = -1
        self.mask_comp = 0
        self.mask_show = False
        self._mask_pm = {}
        self._mask_drag = None
        self._src_img = None          # 亮度范围蒙版的取样源（当前底图）
        self._mask_lum = None         # 512 长边亮度图，用到才算
        self._lum_rev = 0
        # 载入时模块可能还在后台、右栏也没显示，那次 fit 的视口是错的；
        # 记录缩放是不是"自动适应"，等真正显示/改尺寸时再补一次
        self._auto_fit = True
        self._fit_timer = QtCore.QTimer(self)
        self._fit_timer.setSingleShot(True)
        self._fit_timer.timeout.connect(self.fit)

        # 空状态：没素材时 QGraphicsView 就是一块黑，先把下一步说清楚
        self.empty = QtWidgets.QLabel(
            "把照片 / 视频拖进窗口\n\n"
            "或用「文件 → 打开」选择素材\n"
            "素材会按 对齐 → 合成 → 裁剪 → 调色 的顺序自动传下去",
            self.viewport())
        self.empty.setObjectName("EmptyHint")
        self.empty.setAlignment(QtCore.Qt.AlignmentFlag.AlignCenter)
        self.empty.setAttribute(
            QtCore.Qt.WidgetAttribute.WA_TransparentForMouseEvents)

    def resizeEvent(self, e):
        super().resizeEvent(e)
        self.empty.resize(self.viewport().size())
        self._refit_later()

    def showEvent(self, e):
        super().showEvent(e)
        self._refit_later()

    def _refit_later(self):
        if self._auto_fit and self._pix_w and self.isVisible():
            self._fit_timer.start(0)

    # ---- 内容 ----
    def set_source(self, qimg: QtGui.QImage):
        """设置“原图”底图（只加载时调用一次）。"""
        self._pix_w, self._pix_h = qimg.width(), qimg.height()
        self.before.setPixmap(QtGui.QPixmap.fromImage(qimg))
        self.scene().setSceneRect(0, 0, self._pix_w, self._pix_h)
        self._src_img = qimg
        self._mask_lum = None          # 亮度范围要按当前画面重算
        self._lum_rev += 1
        self._mask_pm.clear()          # 权重图按预览尺寸缓存，换素材就作废
        self.empty.hide()
        self._scale_ref()
        self.fit()

    def set_result(self, qimg: QtGui.QImage):
        """渲染结果可能是草图（比底图小一档），用条目缩放对齐，不做放大插值。

        用 x/y 两个比例而不是统一缩放：草图尺寸是整除出来的，与底图差一个
        像素，统一缩放会让右/下边缘错位。
        """
        pm = QtGui.QPixmap.fromImage(qimg)
        self.after_full = pm
        if self._pix_w:
            self.after.setTransform(QtGui.QTransform(
                self._pix_w / pm.width(), 0, 0, self._pix_h / pm.height(), 0, 0))
        self._apply_clip()

    def _scale_ref(self):
        if self.ref_full.isNull() or not self._pix_w:
            self.ref.hide()
            return
        w, h = self.ref_full.width(), self.ref_full.height()
        if w <= 0 or h <= 0:
            return
        self.ref.setTransform(QtGui.QTransform(
            self._pix_w / w, 0, 0, self._pix_h / h, 0, 0))
        self.ref.setPos(0, 0)

    def _apply_clip(self):
        # QGraphicsItem 没有 setClipRect：按分割线裁出需要的那一半像素图。
        pm = self.after_full
        if pm.isNull() or not self._pix_w:
            return
        pw, ph = pm.width(), pm.height()
        use_ref = bool(self.compare and not self.ref_full.isNull())
        self.ref.setVisible(use_ref)
        self.before.setVisible(self.compare and not use_ref)
        if not self.compare:
            self.after.setPixmap(pm)
            self.after.setPos(0, 0)
            return
        x = int(round(min(max(self.split, 0.0), 1.0) * pw))
        x = max(1, min(x, pw - 1))       # copy 空矩形会把画面清空
        if use_ref:
            # 参考帧占左侧：结果只保留分割线右边的部分
            self.after.setPixmap(pm.copy(QtCore.QRect(x, 0, pw - x, ph)))
            self.after.setPos(self._pix_w * x / pw, 0)
        else:
            # 前后对比：左侧是结果，右侧露出底图（原片）
            self.after.setPixmap(pm.copy(QtCore.QRect(0, 0, x, ph)))
            self.after.setPos(0, 0)

    def set_reference(self, qimg):
        """锁定参考帧：分割对比时左半显示它、右半显示当前结果。"""
        self.ref_full = (QtGui.QPixmap() if qimg is None
                         else QtGui.QPixmap.fromImage(qimg))
        if self.ref_full.isNull():
            self.ref.setPixmap(QtGui.QPixmap())
            self.ref.hide()
        else:
            self.ref.setPixmap(self.ref_full)
            self._scale_ref()
        self._apply_clip()
        self.viewport().update()

    def set_compare(self, on):
        self.compare = bool(on)
        self._apply_clip()
        self.viewport().update()

    def set_split(self, v):
        self.split = min(max(v, 0.0), 1.0)
        self._apply_clip()
        self.viewport().update()
        self.split_changed.emit(self.split)

    # ---- 缩放 ----
    def fit(self):
        if not self._pix_w:
            return
        self._auto_fit = True
        self._fit_timer.stop()
        self.resetTransform()
        self.fitInView(self.scene().sceneRect(),
                       QtCore.Qt.AspectRatioMode.KeepAspectRatio)
        self._zoom = self._fit_zoom = self.transform().m11()
        self.setRenderHint(QtGui.QPainter.RenderHint.SmoothPixmapTransform,
                           True)
        self.zoom_changed.emit(self._zoom)

    def zoom_to(self, z, center_vp=None):
        z = min(max(z, 0.02), 16.0)
        self._scale(z / max(self._zoom, 1e-6), center_vp)

    def zoom_by(self, factor, center_vp=None):
        self._scale(factor, center_vp)

    def _scale(self, factor, center_vp=None):
        self._auto_fit = False
        if center_vp is None:
            center_vp = QtCore.QPointF(self.viewport().width() / 2,
                                       self.viewport().height() / 2)
        keep = self.mapToScene(center_vp.toPoint())
        self.scale(factor, factor)
        drift = keep - self.mapToScene(center_vp.toPoint())
        if abs(drift.x()) > 1e-6 or abs(drift.y()) > 1e-6:
            self.translate(drift.x(), drift.y())
        self._zoom = self.transform().m11()
        # 放大到接近逐像素时关闭平滑，方便看细节与合焦
        self.setRenderHint(QtGui.QPainter.RenderHint.SmoothPixmapTransform,
                           self._zoom < 1.5)
        self.zoom_changed.emit(self._zoom)

    def wheelEvent(self, e):
        if not self._pix_w:
            return
        # 裸滚轮是"移动视线"：审片时滚一下就放大，视图会跳得没法看。
        # 缩放必须带 Ctrl（和 PS / LR 一致），或直接双击画布。
        if e.modifiers() & QtCore.Qt.KeyboardModifier.ControlModifier:
            # 按实际滚动量连续缩放，而不是一格固定 1.25：高分辨率滚轮和
            # 触控板会送来 15/30/120 不等的 delta，固定步长会一卡一跳。
            d = e.angleDelta().y()
            if not d:
                d = e.pixelDelta().y() * 8
            if not d:
                return
            factor = 1.25 ** (d / 120.0)
            self.zoom_by(min(max(factor, 0.5), 2.0), e.position())
            e.accept()
            return
        super().wheelEvent(e)

    def mouseDoubleClickEvent(self, e):
        # 双击 = 适应窗口 ⇄ 100%，以落点为中心；拖蒙版/分割线时不抢
        if (e.button() == QtCore.Qt.MouseButton.LeftButton and self._pix_w
                and self._mask_drag is None and not self._drag_split):
            if self._auto_fit:
                self.zoom_to(1.0, e.position())
            else:
                self.fit()
            e.accept()
            return
        super().mouseDoubleClickEvent(e)

    # ---- 局部蒙版 ----
    def set_masks(self, masks):
        """参数变了就重新喂一次（undo/切图会换掉整个列表对象）。"""
        self.masks = list(masks or ())
        if len(self._mask_pm) > 12:
            self._mask_pm.clear()          # 拖动手柄会留下一堆旧几何的图
        self.viewport().update()

    def set_mask_sel(self, i):
        if int(i) != self.mask_sel:
            self.mask_comp = 0
        self.mask_sel = int(i)
        self.viewport().update()

    def set_mask_comp(self, j):
        if int(j) != self.mask_comp:
            self.mask_comp = max(0, int(j))
            self.viewport().update()

    def _sel_comp(self):
        if not (0 <= self.mask_sel < len(self.masks)):
            return None
        comps = self.masks[self.mask_sel].get("components") or [self.masks[self.mask_sel]]
        self.mask_comp = max(0, min(self.mask_comp, len(comps) - 1))
        return comps[self.mask_comp]

    def set_mask_overlay(self, on):
        self.mask_show = bool(on)
        self.viewport().update()

    def _mask_points(self, comp):
        """把手的场景坐标。linear 拖两端定方向、拖中点整体挪；
        radial 中点是圆心（挪位置），端点定半径。角度在此统一叠加。"""
        W, H = self._pix_w, self._pix_h
        ax, ay, bx, by = gc.mask_line_px(comp, W, H)
        if _mtype(comp) == "radial":
            return {"body": (ax, ay), "b": (bx, by)}
        return {"a": (ax, ay), "b": (bx, by),
                "body": ((ax + bx) / 2.0, (ay + by) / 2.0)}

    def _mask_store_pos(self, i, j, px, py):
        """显示坐标 -> 存入组件的归一化坐标（角度要反旋回去）。"""
        comp = (self.masks[i].get("components") or [self.masks[i]])[j]
        W, H = max(1, self._pix_w), max(1, self._pix_h)
        ang = _num(comp.get("angle"), 0.0)
        if ang:
            rad = np.deg2rad(-ang)
            co, si = np.cos(rad), np.sin(rad)
            if _mtype(comp) == "radial":
                ox, oy = _num(comp["x1"], 0) * W, _num(comp["y1"], 0) * H
            else:
                ox = (_num(comp["x1"], 0) + _num(comp["x2"], 0)) * 0.5 * W
                oy = (_num(comp["y1"], 0) + _num(comp["y2"], 0)) * 0.5 * H
            dx, dy = px - ox, py - oy
            px, py = ox + dx * co - dy * si, oy + dx * si + dy * co
        return (float(np.clip(px / W, 0.0, 1.0)),
                float(np.clip(py / H, 0.0, 1.0)))

    def _mask_pos(self, e):
        p = self.mapToScene(e.position().toPoint())
        return (p.x() / max(1, self._pix_w), p.y() / max(1, self._pix_h))

    def _mask_hit(self, pos):
        """返回 (组件下标, 把手名)；当前选中组里没有命中则 None。"""
        if not (0 <= self.mask_sel < len(self.masks)) or not self._pix_w:
            return None
        g = self.masks[self.mask_sel]
        comps = g.get("components") or [g]
        best, bd = None, 11.0          # 命中半径按视口像素算，与缩放无关
        for j, comp in enumerate(comps):
            for kind, (px, py) in self._mask_points(comp).items():
                vp = self.mapFromScene(QtCore.QPointF(px, py))
                d = ((vp.x() - pos.x()) ** 2 + (vp.y() - pos.y()) ** 2) ** 0.5
                if j == self.mask_comp:
                    d -= 2.0           # 多个组件把手重叠时优先当前形状
                if d < bd:
                    best, bd = (j, kind), d
        return best

    def _lum_small(self):
        """当前底图的 512 长边亮度，只给亮度范围蒙版和范围叠加用。"""
        if self._mask_lum is None and self._src_img is not None:
            q = self._src_img
            s = min(1.0, MASK_CAP / max(q.width(), q.height(), 1))
            w = max(2, int(round(q.width() * s)))
            h = max(2, int(round(q.height() * s)))
            im = q.scaled(w, h, QtCore.Qt.AspectRatioMode.IgnoreAspectRatio,
                          QtCore.Qt.TransformationMode.SmoothTransformation)
            im = im.convertToFormat(QtGui.QImage.Format.Format_RGB888)
            buf = np.frombuffer(im.constBits(), dtype=np.uint8)
            buf = buf.reshape(im.height(), im.bytesPerLine())
            buf = buf[:, :w * 3].reshape(h, w, 3)
            self._mask_lum = (buf.astype(np.float32) / 255.0) @ gc.LUMA
        return self._mask_lum

    @staticmethod
    def _rsig(rm):
        if not rm:
            return None
        return (1 if rm.get("lum_on") else 0,
                round(_num(rm.get("lum_min"), 0.0), 3),
                round(_num(rm.get("lum_max"), 1.0), 3),
                round(_num(rm.get("lum_feather"), 0.1), 3))

    def _group_sig(self, g):
        parts = [round(_num(g.get("amount"), 100.0), 1),
                 1 if g.get("invert") else 0, self._rsig(g.get("range"))]
        for c in (g.get("components") or (g,)):
            parts.append((_mtype(c),
                          round(_num(c.get("x1"), 0), 4),
                          round(_num(c.get("y1"), 0), 4),
                          round(_num(c.get("x2"), 0), 4),
                          round(_num(c.get("y2"), 0), 4),
                          round(_num(c.get("feather"), 0), 1),
                          1 if c.get("invert") else 0,
                          round(_num(c.get("angle"), 0.0), 2),
                          round(_num(c.get("midpoint"), 50.0), 2),
                          c.get("blend", "add"), self._rsig(c.get("range"))))
        return tuple(parts)

    def _field_pm(self, m, alpha):
        lum = self._lum_small() if _uses_range(m) else None
        key = (self._group_sig(m), self._pix_w, self._pix_h, alpha,
               self._lum_rev if lum is not None else -1)
        pm = self._mask_pm.get(key)
        if pm is None:
            s = min(1.0, MASK_CAP / max(self._pix_w, self._pix_h))
            w = max(2, int(round(self._pix_w * s)))
            h = max(2, int(round(self._pix_h * s)))
            field = gc.mask_field(h, w, m, lum=lum)
            if field is None:
                return None
            a = (np.clip(field, 0.0, 1.0) * alpha).astype(np.uint8)
            f = a.astype(np.float32) / 255.0          # ARGB32_Premultiplied
            buf = np.zeros((h, w, 4), dtype=np.uint8)
            buf[..., 0] = (58 * f).astype(np.uint8)
            buf[..., 1] = (58 * f).astype(np.uint8)
            buf[..., 2] = (236 * f).astype(np.uint8)
            buf[..., 3] = a
            img = QtGui.QImage(buf.data, w, h, 4 * w,
                               QtGui.QImage.Format.Format_ARGB32_Premultiplied).copy()
            pm = QtGui.QPixmap.fromImage(img)
            self._mask_pm[key] = pm
        return pm

    def _draw_masks(self, painter):
        if not self.masks or not self._pix_w:
            return
        area = QtCore.QRectF(0, 0, self._pix_w, self._pix_h)
        if self.mask_show:
            # 权重图最长边只有 512，铺回原图尺寸要靠平滑插值，否则羽化会看成台阶
            painter.setRenderHint(QtGui.QPainter.SmoothPixmapTransform, True)
            for i, m in enumerate(self.masks):
                pm = self._field_pm(m, 170 if i == self.mask_sel else 96)
                if pm is None:
                    continue
                # PySide6 不接受 drawPixmap(QRectF, QPixmap)，必须补上源矩形
                painter.drawPixmap(area, pm, QtCore.QRectF(pm.rect()))
        if 0 <= self.mask_sel < len(self.masks):
            self._draw_handles(painter, self.masks[self.mask_sel])

    def _draw_shape(self, painter, comp, pts):
        if _mtype(comp) == "radial":
            cx, cy = pts["body"]
            ex, ey = pts["b"]
            W, H = self._pix_w, self._pix_h
            rx = abs(_num(comp["x2"], 0) - _num(comp["x1"], 0)) * W
            ry = abs(_num(comp["y2"], 0) - _num(comp["y1"], 0)) * H
            if ry < max(H * 0.01, 1.0):        # 与 grade._comp_field 同一套回退
                ry = max(rx, 1.0)
            elif rx < max(W * 0.01, 1.0):
                rx = max(ry, 1.0)
            painter.save()
            painter.translate(cx, cy)
            painter.rotate(_num(comp.get("angle"), 0.0))
            painter.drawEllipse(QtCore.QPointF(0, 0), rx, ry)
            painter.restore()
            painter.drawLine(QtCore.QPointF(cx, cy), QtCore.QPointF(ex, ey))
        else:
            painter.drawLine(QtCore.QPointF(*pts["a"]), QtCore.QPointF(*pts["b"]))

    def _draw_handles(self, painter, g):
        """选中组：当前组件亮色带把手，其余组件暗色只画轮廓。"""
        comps = g.get("components") or [g]
        self.mask_comp = max(0, min(self.mask_comp, len(comps) - 1))
        r = 6.0 / max(self._zoom, 1e-6)
        for j, comp in enumerate(comps):
            active = j == self.mask_comp
            pts = self._mask_points(comp)
            pen = QtGui.QPen(QtGui.QColor(theme.ACCENT_HI if active
                                          else theme.TEXT_OFF), 0)
            pen.setCosmetic(True)
            pen.setWidthF(1.6 if active else 1.1)
            if comp.get("blend") not in (None, "add"):
                pen.setStyle(QtCore.Qt.PenStyle.DashLine)
            painter.setPen(pen)
            painter.setBrush(QtCore.Qt.BrushStyle.NoBrush)
            self._draw_shape(painter, comp, pts)
            if not active:
                continue
            for kind, (px, py) in pts.items():
                c = QtCore.QPointF(px, py)
                if kind == "body":
                    painter.setBrush(QtGui.QColor(theme.ACCENT))
                    painter.drawRect(QtCore.QRectF(c.x() - r, c.y() - r,
                                                   2 * r, 2 * r))
                else:
                    painter.setBrush(QtGui.QColor(theme.CANVAS))
                    painter.drawEllipse(c, r, r)
        painter.setBrush(QtCore.Qt.BrushStyle.NoBrush)

    # ---- 分割线 ----
    def _split_x_view(self):
        return self.mapFromScene(QtCore.QPointF(self._pix_w * self.split, 0)).x()

    def _near_split(self, pos):
        return abs(self._split_x_view() - pos.x()) < 7

    def mousePressEvent(self, e):
        if (self.compare and e.button() == QtCore.Qt.MouseButton.LeftButton
                and self._near_split(e.position())):
            self._drag_split = True
            self.setCursor(QtCore.Qt.CursorShape.ClosedHandCursor)
            e.accept()
            return
        if (e.button() == QtCore.Qt.MouseButton.LeftButton
                and self._mask_drag is None):
            hit = self._mask_hit(e.position())
            if hit is not None:
                j, kind = hit
                if j != self.mask_comp:
                    self.set_mask_comp(j)
                    self.mask_component_selected.emit(j)
                self._mask_drag = (self.mask_sel, j, kind, *self._mask_pos(e))
                self.mask_drag_state.emit(True)
                e.accept()
                return
        super().mousePressEvent(e)

    def mouseMoveEvent(self, e):
        if self._drag_split:
            self.set_split(e.position().x() / max(1, self.viewport().width()))
            e.accept()
            return
        if self._mask_drag is not None:
            i, j, kind, lu, lv = self._mask_drag
            u, v = self._mask_pos(e)
            if kind == "body":
                self.mask_dragged.emit(i, j, kind, u - lu, v - lv)
            else:
                # 把手显示在旋转后的位置，写回前反旋，否则一转角度就跳
                su, sv = self._mask_store_pos(
                    i, j, u * self._pix_w, v * self._pix_h)
                self.mask_dragged.emit(i, j, kind, su, sv)
            self._mask_drag = (i, j, kind, u, v)
            e.accept()
            return
        if self._mask_hit(e.position()) is not None:
            self.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        elif self.compare:
            self.setCursor(QtCore.Qt.CursorShape.SplitHCursor
                           if self._near_split(e.position())
                           else QtCore.Qt.CursorShape.OpenHandCursor)
        else:
            self.unsetCursor()
        super().mouseMoveEvent(e)

    def mouseReleaseEvent(self, e):
        if self._drag_split:
            self._drag_split = False
            self.unsetCursor()
            e.accept()
            return
        if self._mask_drag is not None:
            self._mask_drag = None
            self.unsetCursor()
            self.mask_drag_state.emit(False)
            e.accept()
            return
        super().mouseReleaseEvent(e)

    def drawForeground(self, painter, rect):
        super().drawForeground(painter, rect)
        if not self._pix_w:
            return
        painter.save()
        self._draw_masks(painter)
        if self.compare:
            painter.setPen(QtGui.QPen(QtGui.QColor("#ffffff"), 0))
            x = self._pix_w * self.split
            painter.drawLine(QtCore.QPointF(x, rect.top()),
                             QtCore.QPointF(x, rect.bottom()))
            # 中央把手
            cy = (rect.top() + rect.bottom()) / 2
            r = 11 / max(self._zoom, 1e-6)
            painter.setBrush(QtGui.QColor(theme.ACCENT))
            painter.drawEllipse(QtCore.QPointF(x, cy), r, r)
            if not self.ref_full.isNull():
                self._draw_ref_labels(painter)
        painter.restore()

    def _draw_ref_labels(self, painter):
        """参考帧模式下给两侧贴标签，避免把左边的旧帧当成当前帧。"""
        z = max(self._zoom, 1e-6)
        f = painter.font()
        f.setPixelSize(max(8, int(12 / z)))
        painter.setFont(f)
        fm = QtGui.QFontMetricsF(f)
        pad = 7 / z
        top = fm.ascent() + pad
        painter.setPen(QtCore.Qt.PenStyle.NoPen)
        for text, right in (("参考", False), ("当前", True)):
            tw = fm.horizontalAdvance(text)
            x = self._pix_w - pad - tw if right else pad
            box = QtCore.QRectF(x - pad * 0.6, top - fm.ascent() - pad * 0.2,
                                tw + pad * 1.2, fm.height())
            painter.setBrush(QtGui.QColor(0, 0, 0, 150))
            painter.drawRoundedRect(box, pad * 0.4, pad * 0.4)
            painter.setPen(QtGui.QColor("#ffffff"))
            painter.drawText(QtCore.QPointF(x, top), text)
            painter.setPen(QtCore.Qt.PenStyle.NoPen)

    # ---- 快捷键 ----
    def keyPressEvent(self, e):
        k = e.key()
        if k == QtCore.Qt.Key.Key_0:
            self.fit()
        elif k == QtCore.Qt.Key.Key_1:
            self.zoom_to(1.0)
        elif k == QtCore.Qt.Key.Key_2:
            self.zoom_to(2.0)
        elif k == QtCore.Qt.Key.Key_Home:
            self.set_split(0.5)
        elif k == QtCore.Qt.Key.Key_M:
            self.mask_overlay_toggle.emit()
        elif self.compare and k in (QtCore.Qt.Key.Key_Left,
                                    QtCore.Qt.Key.Key_Right):
            step = 0.01 if e.modifiers() & QtCore.Qt.KeyboardModifier.ShiftModifier else 0.002
            self.set_split(self.split + (step if k == QtCore.Qt.Key.Key_Right
                                        else -step))
        else:
            super().keyPressEvent(e)

    def zoom_label(self):
        return f"{self._zoom * 100:.0f}%"
