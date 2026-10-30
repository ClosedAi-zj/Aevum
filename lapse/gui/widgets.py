"""跨模块共用的控件。"""
import os
import subprocess
import weakref

from PySide6 import QtCore, QtGui, QtWidgets

from lapse.ff import NO_WINDOW
from lapse.gui import icons, theme


def reveal(path):
    """在文件管理器里打开成品：目录直接进去，文件则选中它。"""
    if not path or not os.path.exists(path):
        return False
    p = os.path.normpath(path)
    subprocess.Popen(["explorer", p] if os.path.isdir(p)
                     else ["explorer", "/select,", p],
                     creationflags=NO_WINDOW)
    return True


def icon_button(name, tip, role="dim", size=18, box=32, checkable=False,
                checked_role=None, parent=None):
    """图标按钮：只留一枚图标，名字和用法写在悬停提示里。

    工具栏一排小动作全用文字会把面板挤成一堵字墙；图标 + tooltip 是
    Lightroom / Capture One 的常规做法。颜色登记的是角色，切皮肤自动重画。
    """
    b = QtWidgets.QToolButton(parent)
    b.setObjectName("Icon")
    b.setAccessibleName(name)
    b.setCheckable(checkable)
    b.setAutoRaise(True)
    b.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
    b.setFixedSize(box, box)
    b.setToolTip(tip)
    icons.bind(b, name, role, size, checked_role)
    return b


def label_button(btn, name, tip=None, role="dim", size=15):
    """保留文字的按钮补一枚图标（主操作仍写清楚做什么，只是更好认）。"""
    icons.bind(btn, name, role, size)
    if tip:
        btn.setToolTip(tip)
    return btn


def field_block(label, widget, hint=""):
    """标签在上、控件在下的参数块，可选第三行一句短说明。

    四列网格里比"左标签右控件"更整齐：控件宽度跟着列走，
    不会出现同一行一个 110px、一个 500px 的参差感。

    hint 是常驻的那一行（不是 tooltip）：用户"不敢调"多半因为只看得到控件
    名字，鼠标不悬停就永远不知道往哪边调会怎样。文案来自 `hints.line()`。
    """
    box = QtWidgets.QWidget()
    v = QtWidgets.QVBoxLayout(box)
    v.setContentsMargins(0, 0, 0, 0)
    v.setSpacing(3)
    lb = QtWidgets.QLabel(label)
    lb.setObjectName("FieldLabel")
    v.addWidget(lb)
    v.addWidget(widget)
    box.hint_label = None
    if hint:
        hl = QtWidgets.QLabel(hint)
        hl.setObjectName("FieldHint")
        hl.setWordWrap(True)
        v.addWidget(hl)
        box.hint_label = hl
    return box


class Group(QtWidgets.QWidget):
    """检查器式分组：左边 chevron、中间标题、右边可选的本组复位。

    标题行是一个可点击的普通 QWidget，不用 QPushButton 的图标+文字居中，
    这样窄面板里标题能稳定左对齐。对齐/合成/裁剪/调色台共用同一种分组。
    """

    def __init__(self, title, parent=None, collapsed=False, reset_fn=None,
                 on_toggle=None):
        super().__init__(parent)
        self._title = title
        self._reset_fn = reset_fn
        self._on_toggle = on_toggle
        self.open = not collapsed

        self.header = QtWidgets.QWidget(self)
        self.header.setObjectName("GroupHeader")
        self.header.setAttribute(QtCore.Qt.WidgetAttribute.WA_Hover)
        self.header.setFixedHeight(34)
        self.header.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        h = QtWidgets.QHBoxLayout(self.header)
        h.setContentsMargins(8, 0, 6, 0)
        h.setSpacing(5)

        self.chev = QtWidgets.QToolButton(self.header)
        self.chev.setObjectName("GroupChevron")
        self.chev.setAutoRaise(True)
        self.chev.setIconSize(QtCore.QSize(14, 14))
        self.chev.setFixedSize(18, 18)
        self.chev.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self.title_label = QtWidgets.QLabel(title, self.header)
        self.title_label.setObjectName("GroupTitle")
        self.btn_reset = QtWidgets.QToolButton(self.header)
        self.btn_reset.setObjectName("GroupReset")
        self.btn_reset.setAutoRaise(True)
        self.btn_reset.setIconSize(QtCore.QSize(13, 13))
        icons.bind(self.btn_reset, "rotate-ccw", "off", 13)
        self.btn_reset.setFixedSize(22, 22)
        self.btn_reset.setToolTip("恢复本组默认值")
        self.btn_reset.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self.btn_reset.setVisible(reset_fn is not None)
        self.btn_reset.clicked.connect(self.reset)

        h.addWidget(self.chev)
        h.addWidget(self.title_label)
        h.addStretch(1)
        h.addWidget(self.btn_reset)

        self.body = QtWidgets.QWidget(self)
        self.body.setObjectName("GroupBody")
        self.form = QtWidgets.QVBoxLayout(self.body)
        self.form.setContentsMargins(12, 9, 12, 11)
        self.form.setSpacing(5)
        lay = QtWidgets.QVBoxLayout(self)
        lay.setContentsMargins(0, 0, 0, 0)
        lay.setSpacing(0)
        lay.addWidget(self.header)
        lay.addWidget(self.body)
        # 竖直方向必须是 Fixed：滚动区开着 setWidgetResizable，Preferred 会把
        # 面板剩下的空白摊给每个分组，折叠之后就成了标题行之间的大空档
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Preferred,
                           QtWidgets.QSizePolicy.Policy.Fixed)

        for w in (self.header, self.title_label, self.chev):
            w.installEventFilter(self)
        self.body.setVisible(self.open)
        self._sync_chevron()

    def eventFilter(self, obj, e):
        if (obj in (self.header, self.title_label, self.chev)
                and e.type() == QtCore.QEvent.Type.MouseButtonPress
                and e.button() == QtCore.Qt.MouseButton.LeftButton):
            self.toggle()
            return True
        return super().eventFilter(obj, e)

    def toggle(self):
        self.set_collapsed(self.open)

    def _sync_chevron(self):
        icons.repaint(self.chev, "chevron-down" if self.open
                      else "chevron-right", "dim", 14)
        if self._on_toggle is not None:
            self._on_toggle(self.open)

    def add(self, w):
        self.form.addWidget(w)

    def grid(self, hspacing=10, vspacing=7):
        """在分组里放一个无内边距的网格，返回布局给调用方填控件。"""
        holder = QtWidgets.QWidget(self.body)
        g = QtWidgets.QGridLayout(holder)
        g.setContentsMargins(0, 0, 0, 0)
        g.setHorizontalSpacing(hspacing)
        g.setVerticalSpacing(vspacing)
        self.form.addWidget(holder)
        return g

    def vbox(self, spacing=4):
        holder = QtWidgets.QWidget(self.body)
        v = QtWidgets.QVBoxLayout(holder)
        v.setContentsMargins(0, 0, 0, 0)
        v.setSpacing(spacing)
        self.form.addWidget(holder)
        return v

    def reset(self):
        if self._reset_fn is not None:
            self._reset_fn()

    def set_collapsed(self, on):
        self.open = not on
        self.body.setVisible(not on)
        self._sync_chevron()


class FramePreview(QtWidgets.QWidget):
    """等比显示一帧，带空状态和左下角说明。

    对齐/合成页的中央画布：有素材时给参考帧，没有时把下一步说清楚。
    解码只在路径变化时做一次，窗口缩放只重画，不重读原片。
    """

    def __init__(self, parent=None, empty="把素材拖进窗口，或点右上角「打开素材」"):
        super().__init__(parent)
        self.setObjectName("FramePreview")
        self.setMinimumSize(320, 200)
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Expanding,
                           QtWidgets.QSizePolicy.Policy.Expanding)
        self._path = None
        self._pm = None
        self._caption = ""
        self._empty = empty

    # ---- 数据 ----
    def clear(self, text=None):
        self._path = None
        self._pm = None
        self._caption = ""
        if text is not None:
            self._empty = text
        self.update()

    def set_caption(self, text):
        if text != self._caption:
            self._caption = text
            self.update()

    def has_image(self):
        return self._pm is not None

    def set_pixmap(self, pm, caption=""):
        """直接给一张已经解码好的图（视频抽帧走这里，不用先落临时文件）。"""
        if pm is None or pm.isNull():
            return False
        self._path = None
        self._pm = pm
        self._caption = caption
        self.update()
        return True

    def set_frame(self, path, caption=""):
        """读一帧显示；同一路径重复调用只更新说明。失败返回 False。"""
        if path and path == self._path and self._pm is not None:
            self.set_caption(caption)
            return True
        try:
            from lapse import imio
            arr = imio.decode_image(path, bps=8, half_size=imio.is_raw(path))
        except Exception:
            arr = None
        if arr is None:
            self.clear("读不出这一帧的预览\n" + os.path.basename(path or ""))
            return False
        h, w = arr.shape[:2]
        if max(h, w) > 1600:
            import cv2
            s = 1600.0 / max(h, w)
            arr = cv2.resize(arr, (int(round(w * s)), int(round(h * s))),
                             interpolation=cv2.INTER_AREA)
            h, w = arr.shape[:2]
        if not arr.flags["C_CONTIGUOUS"]:
            arr = arr.copy()
        img = QtGui.QImage(arr.data, w, h, arr.strides[0],
                           QtGui.QImage.Format.Format_BGR888).copy()
        self._path = path
        self._pm = QtGui.QPixmap.fromImage(img)
        self._caption = caption
        self.update()
        return True

    # ---- 绘制 ----
    def paintEvent(self, _e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)
        p.fillRect(self.rect(), QtGui.QColor(theme.FIELD))
        frame = QtCore.QRectF(self.rect()).adjusted(1, 1, -1, -1)
        p.setPen(QtGui.QPen(QtGui.QColor(theme.SEP), 1))
        p.setBrush(QtCore.Qt.BrushStyle.NoBrush)
        p.drawRoundedRect(frame, 10, 10)

        if self._pm is None:
            p.setPen(QtGui.QColor(theme.TEXT_OFF))
            f = self.font()
            f.setPixelSize(15)
            p.setFont(f)
            p.drawText(frame.adjusted(28, 24, -28, -24),
                       QtCore.Qt.AlignmentFlag.AlignCenter
                       | QtCore.Qt.TextFlag.TextWordWrap, self._empty)
            p.end()
            return

        box = frame.adjusted(10, 10, -10, -10)
        s = self._pm.size()
        s.scale(box.size().toSize(),
                QtCore.Qt.AspectRatioMode.KeepAspectRatio)
        x = box.x() + (box.width() - s.width()) / 2
        y = box.y() + (box.height() - s.height()) / 2
        target = QtCore.QRectF(x, y, s.width(), s.height())
        p.setRenderHint(QtGui.QPainter.RenderHint.SmoothPixmapTransform)
        p.drawPixmap(target, self._pm, QtCore.QRectF(self._pm.rect()))

        if self._caption:
            fm = p.fontMetrics()
            tw = min(fm.horizontalAdvance(self._caption) + 18,
                     max(120, target.width() - 20))
            th = fm.height() + 8
            pill = QtCore.QRectF(target.x() + 12,
                                 target.bottom() - th - 12, tw, th)
            p.setPen(QtGui.QPen(QtGui.QColor(*theme.GRID), 1))
            p.setBrush(QtGui.QColor(*theme.PILL))
            p.drawRoundedRect(pill, th / 2, th / 2)
            p.setPen(QtGui.QColor(theme.TEXT_DIM))
            p.drawText(pill.adjusted(9, 0, -9, 0),
                       QtCore.Qt.AlignmentFlag.AlignVCenter
                       | QtCore.Qt.AlignmentFlag.AlignLeft,
                       fm.elidedText(self._caption,
                                     QtCore.Qt.TextElideMode.ElideMiddle,
                                     int(tw - 18)))
        p.end()


_SLIDERS = []              # 活着的推子（弱引用）：换字号档时行高要跟着变


def _rescale_sliders():
    live = []
    for ref in _SLIDERS:
        s = ref()
        if s is not None:
            s.setFixedHeight(theme.fs(s.ROW))
            live.append(ref)
    _SLIDERS[:] = live


theme.on_change(_rescale_sliders)


class Slider(QtWidgets.QSlider):
    """现代推子：细轨道 + 白色圆点手柄，填充从中性位起算。

    QSS 的 sub-page 只会从最小值起画，做不出"以中性位为原点"的填充，
    所以整条轨道自己绘制；数值范围与交互仍交给 QSlider。
    neutral 留空表示最小值即起点（单向参数，如 CRF），不画中央刻痕。
    gradient 给色温/色调这类有方向的参数画一条色带，和 Lightroom 一样。
    """

    HANDLE = 13              # 点再大就从"推子"变成"旋钮"了
    TRACK = 4
    ROW = 22                 # 行高基准，随字号档一起缩放
    FINE = 0.2               # Alt 精调：鼠标位移只取 20%

    reset_requested = QtCore.Signal()

    def __init__(self, parent=None, neutral=None, gradient=None):
        super().__init__(QtCore.Qt.Orientation.Horizontal, parent)
        self._neutral = neutral
        self._gradient = gradient
        self.setFixedHeight(theme.fs(self.ROW))
        _SLIDERS.append(weakref.ref(self))
        self.setMouseTracking(True)
        self.setCursor(QtCore.Qt.CursorShape.PointingHandCursor)
        self._hot = False
        self._jump = False
        self._fine = False
        self._press_x = 0.0
        self._press_value = 0

    @property
    def neutral(self):
        return self.minimum() if self._neutral is None else self._neutral

    @property
    def gradient(self):
        return self._gradient

    # ---- 位置 <-> 像素 ----
    def _px(self, pos):
        span = self.width() - self.HANDLE - 2
        f = (pos - self.minimum()) / max(1, self.maximum() - self.minimum())
        return 1 + self.HANDLE / 2 + f * span

    def _pos(self, x):
        span = self.width() - self.HANDLE - 2
        f = (x - 1 - self.HANDLE / 2) / max(1.0, span)
        return int(round(self.minimum() + f * (self.maximum() - self.minimum())))

    def _fine_pos(self, x):
        """Alt 拖动：从按下时的值起算，鼠标走 5px 只相当于普通拖动的 1px。"""
        span = self.width() - self.HANDLE - 2
        if span <= 0:
            return self._press_value
        f = (x - self._press_x) / span * self.FINE
        return int(round(self._press_value
                         + f * (self.maximum() - self.minimum())))

    # ---- 交互：点轨道任意位置即取值，按住可连拖 ----
    def mousePressEvent(self, e):
        if e.button() == QtCore.Qt.MouseButton.LeftButton:
            self._jump = True
            self._fine = bool(e.modifiers()
                              & QtCore.Qt.KeyboardModifier.AltModifier)
            self._press_x = e.position().x()
            self._press_value = self.sliderPosition()
            self.sliderPressed.emit()
            if not self._fine:
                self.setSliderPosition(self._pos(self._press_x))
            e.accept()
            return
        super().mousePressEvent(e)

    def mouseMoveEvent(self, e):
        if self._jump:
            self.setSliderPosition(self._fine_pos(e.position().x())
                                   if self._fine
                                   else self._pos(e.position().x()))
            e.accept()
            return
        super().mouseMoveEvent(e)

    def mouseReleaseEvent(self, e):
        if self._jump:
            self._jump = False
            self._fine = False
            self.sliderReleased.emit()
            e.accept()
            return
        super().mouseReleaseEvent(e)

    def mouseDoubleClickEvent(self, e):
        if e.button() == QtCore.Qt.MouseButton.LeftButton:
            self._jump = False
            self._fine = False
            self.reset_requested.emit()
            e.accept()
            return
        super().mouseDoubleClickEvent(e)

    def wheelEvent(self, e):
        """滚轮微调也算一步历史（否则改完撤不掉）。"""
        before = self.sliderPosition()
        super().wheelEvent(e)
        if self.sliderPosition() != before:
            self.sliderPressed.emit()
            self.sliderReleased.emit()

    def enterEvent(self, e):
        self._hot = True
        self.update()
        super().enterEvent(e)

    def leaveEvent(self, e):
        self._hot = False
        self.update()
        super().leaveEvent(e)

    def paintEvent(self, e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        cy = self.height() / 2
        x0, x1 = self._px(self.minimum()), self._px(self.maximum())
        hv = self._px(self.sliderPosition())
        nv = self._px(min(max(self.neutral, self.minimum()), self.maximum()))
        en = not self.isEnabled()
        # 悬停/按住/键盘聚焦：白点长一档，填充同时亮一档。
        # 以前是在点外面再套一圈描边，那圈比点本身大一截，一排推子同时
        # 亮起来时环环比数值还抢眼；点也不能染成强调色——它压在同样是
        # 强调色的填充上，一染就糊进轨道里看不见了。
        lit = (self._hot or self.isSliderDown()
               or (self.hasFocus() and not en))

        # 轨道：有方向感的参数画渐变，其余画中性轨道 + 从中性位起的填充
        if self._gradient and not en:
            grad = QtGui.QLinearGradient(x0, cy, x1, cy)
            n = len(self._gradient)
            for i, col in enumerate(self._gradient):
                c = QtGui.QColor(col)
                c.setAlpha(205)
                grad.setColorAt(i / max(1, n - 1), c)
            p.setBrush(QtGui.QBrush(grad))
            p.drawRoundedRect(QtCore.QRectF(x0, cy - self.TRACK / 2,
                                            x1 - x0, self.TRACK),
                              self.TRACK / 2, self.TRACK / 2)
        else:
            p.setBrush(QtGui.QColor(theme.BORDER if not en else theme.SEP))
            p.drawRoundedRect(QtCore.QRectF(x0, cy - self.TRACK / 2,
                                            x1 - x0, self.TRACK),
                              self.TRACK / 2, self.TRACK / 2)
            if abs(hv - nv) > 0.5:
                p.setBrush(QtGui.QColor(theme.BORDER_HI if en
                                        else theme.ACCENT_HI if lit
                                        else theme.ACCENT))
                p.drawRoundedRect(
                    QtCore.QRectF(min(nv, hv), cy - self.TRACK / 2,
                                  abs(hv - nv), self.TRACK),
                    self.TRACK / 2, self.TRACK / 2)
        # 中性位刻痕：只有双极性参数（轨道中间是"无改动"）才画
        if self.neutral not in (self.minimum(), self.maximum()):
            p.setBrush(QtGui.QColor(theme.TEXT_OFF if not en else theme.SEP))
            p.drawRoundedRect(QtCore.QRectF(nv - 1, cy - 4, 2, 8), 1, 1)

        d = self.HANDLE + (3 if self.isSliderDown() else 2 if lit else 0)
        rect = QtCore.QRectF(hv - d / 2, cy - d / 2, d, d)
        p.setBrush(QtGui.QColor(theme.TEXT_OFF if en
                                else theme.HANDLE))
        p.setPen(QtGui.QPen(QtGui.QColor(0, 0, 0, 110 if not lit else 150), 1))
        p.drawEllipse(rect.adjusted(0.5, 0.5, -0.5, -0.5))
        p.end()


class ResultCard(QtWidgets.QFrame):
    """一次运行的结果：一句结论 + 走了哪几步 + 几个关键数字 + 技术细节。

    以前任务跑完只往一个纯文本框里刷屏，"成没成、成品在哪、下一步干什么"
    全埋在几百行里。这里把结果分层：状态在最上，跑到哪一步是一条条时间线，
    数字排成 chips，ffmpeg 那些原始输出折进"技术细节"。日志控件还是原来那个
    QPlainTextEdit，模块照旧 appendPlainText，只是换了个住处。

    卡面只放事实，解释（为什么是这个结论、下一步做什么）拼成状态点的悬停；
    高度跟着内容走，没跑过时它就是薄薄一条标题。
    """

    open_requested = QtCore.Signal(str)

    # kind -> (图标, 颜色角色)
    MARKS = {"idle": ("circle", "off"), "wait": ("circle", "off"),
             "run": ("refresh", "accent_hi"),
             "ok": ("check", "ok"), "warn": ("slash", "bad"),
             "bad": ("x", "bad")}
    COLS = 3

    def __init__(self, title="最近一次结果", parent=None):
        super().__init__(parent)
        self.setObjectName("ResultCard")
        v = QtWidgets.QVBoxLayout(self)
        v.setContentsMargins(12, 10, 10, 10)
        v.setSpacing(8)

        head = QtWidgets.QHBoxLayout()
        head.setSpacing(7)
        self.btn_mark = QtWidgets.QToolButton(self)
        self.btn_mark.setObjectName("Icon")
        self.btn_mark.setFixedSize(22, 22)
        self.btn_mark.setAutoRaise(True)
        # 它只是状态的图标，不是按钮：不给焦点、不接任何信号
        self.btn_mark.setFocusPolicy(QtCore.Qt.FocusPolicy.NoFocus)
        self.btn_mark.setCursor(QtCore.Qt.CursorShape.ArrowCursor)
        icons.bind(self.btn_mark, "circle", "off", 17)
        self.lb_title = QtWidgets.QLabel(title, self)
        self.lb_title.setObjectName("ResultTitle")
        self.lb_title.setTextInteractionFlags(
            QtCore.Qt.TextInteractionFlag.TextSelectableByMouse)
        self.btn_open = icon_button("folder-open", "打开成品所在位置", box=28,
                                    size=16, parent=self)
        self.btn_open.setVisible(False)
        self.btn_open.clicked.connect(
            lambda: self.open_requested.emit(self._path or ""))
        self.btn_more = icon_button("chevron-right", "看技术细节（完整日志）",
                                    box=28, size=16, parent=self)
        self.btn_more.setCheckable(True)
        self.btn_more.toggled.connect(self._show_details)
        # clicked 只有真人在点时才发，代码里 setChecked 不会触发；
        # 有了这条，自动展开/收起就不会覆盖用户自己的选择。
        self.btn_more.clicked.connect(self._user_toggled)
        head.addWidget(self.btn_mark)
        head.addWidget(self.lb_title, 1)
        head.addWidget(self.btn_open)
        head.addWidget(self.btn_more)
        v.addLayout(head)

        # 步骤时间线：跑到哪一步、哪一步出的问题，一眼看到
        self.box_steps = QtWidgets.QWidget(self)
        sv = QtWidgets.QVBoxLayout(self.box_steps)
        sv.setContentsMargins(0, 0, 0, 0)
        sv.setSpacing(3)
        self.box_steps.setVisible(False)
        v.addWidget(self.box_steps)

        self.box_metrics = QtWidgets.QWidget(self)
        self.grid = QtWidgets.QGridLayout(self.box_metrics)
        self.grid.setContentsMargins(0, 0, 0, 0)
        self.grid.setHorizontalSpacing(6)
        self.grid.setVerticalSpacing(6)
        self.box_metrics.setVisible(False)
        v.addWidget(self.box_metrics)

        self.lb_path = QtWidgets.QLabel("", self)
        self.lb_path.setObjectName("ResultPath")
        self.lb_path.setWordWrap(True)
        self.lb_path.setTextInteractionFlags(
            QtCore.Qt.TextInteractionFlag.TextSelectableByMouse)
        self.lb_path.setVisible(False)
        v.addWidget(self.lb_path)

        self.box_detail = QtWidgets.QWidget(self)
        dv = QtWidgets.QVBoxLayout(self.box_detail)
        dv.setContentsMargins(0, 0, 0, 0)
        dv.setSpacing(4)
        lb_raw = QtWidgets.QLabel("技术细节", self.box_detail)
        lb_raw.setObjectName("RawTitle")
        dv.addWidget(lb_raw)
        self._detail = None
        self.box_detail.setVisible(False)
        v.addWidget(self.box_detail, 1)

        # 卡片只占内容那么高。以前它跟着左栏吃多余高度，没跑的时候
        # 底下一大片空白，看着就是个空文本框；省下来的高度还给画布。
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Preferred,
                           QtWidgets.QSizePolicy.Policy.Maximum)

        self._path = None
        self._tip = ""
        self._next = ""
        self._metrics = []
        self._steps = []
        self._rows = []
        self._auto_open = False
        self.btn_more.setEnabled(False)   # 有日志可看才亮起来，见 set_detail
        self.set_status("idle", title)

    # ------------------------------------------------------------------ 内容
    def set_status(self, kind, text, tip=None):
        """结论先行：图标 + 一句话。kind 取 MARKS 里的键。"""
        self._kind = kind
        mark, role = self.MARKS.get(kind, self.MARKS["idle"])
        icons.repaint(self.btn_mark, mark, role, 17)
        self.lb_title.setText(text)
        self._tip = tip or ""
        self._sync_tip()
        # 跑的时候把日志摊开：进度条只说百分比，日志才说在干哪一步。
        # 出结论后再折回去；但这只对"是我们自己展开的"成立，
        # 用户手动摊开的就别抢他的按钮。
        if kind == "run":
            if not self.btn_more.isChecked():
                self._auto_open = True
                self.btn_more.setChecked(True)
        elif self._auto_open:
            self.btn_more.setChecked(False)
            self._auto_open = False
        return self

    def _user_toggled(self):
        """来自真人的一次点击：从这一刻起不再自动收/展。"""
        self._auto_open = False

    def set_metrics(self, items):
        """[(键, 值)] 或 [(键, 值, 悬停详解)]；空列表就把这一行收掉。"""
        self._metrics = [(i[0], i[1], i[2] if len(i) > 2 else None)
                         for i in items]
        while self.grid.count():
            old = self.grid.takeAt(0).widget()
            if old is not None:
                # 先脱离控件树再排队销毁：只 deleteLater 的话，
                # 下一轮事件循环之前旧数字还会在卡片上闪一下
                old.setParent(None)
                old.deleteLater()
        for i, item in enumerate(self._metrics):
            key, val, tip = item
            chip = self._chip(key, val, tip)
            r, c = divmod(i, self.COLS)
            self.grid.addWidget(chip, r, c)
        for c in range(self.COLS):
            self.grid.setColumnStretch(c, 1)
        self.box_metrics.setVisible(bool(self._metrics))
        return self

    def _chip(self, key, val, tip):
        chip = QtWidgets.QFrame(self.box_metrics)
        chip.setObjectName("Metric")
        cv = QtWidgets.QVBoxLayout(chip)
        cv.setContentsMargins(9, 6, 9, 7)
        cv.setSpacing(1)
        lb_v = QtWidgets.QLabel(str(val), chip)
        lb_v.setObjectName("MetricVal")
        lb_k = QtWidgets.QLabel(key, chip)
        lb_k.setObjectName("MetricKey")
        if tip:
            chip.setToolTip(f"{key}：{tip}")
            lb_v.setToolTip(f"{key}：{tip}")
        cv.addWidget(lb_v)
        cv.addWidget(lb_k)
        return chip

    # ---------------------------------------------------------------- 时间线
    def set_steps(self, labels):
        """开跑前给一串步骤名，之后用 stage() 逐条点亮。

        只有一步时整条收起来：一行"完成"没有信息量，还把关键数字挤下去。
        """
        self._steps = [[lab, "wait", ""] for lab in labels]
        for i, st in enumerate(self._steps):
            self._paint_step(i, *st)
        for row in self._rows[len(self._steps):]:
            row.setVisible(False)
        self.box_steps.setVisible(len(self._steps) > 1)
        return self

    def stage(self, i, state, note=""):
        """第 i 步换状态：run 正在做 / ok 做完 / bad 出错；note 补一句结果。"""
        if 0 <= i < len(self._steps):
            self._steps[i][1] = state
            if note:
                self._steps[i][2] = note
            self._paint_step(i, *self._steps[i])
        return self

    def steps(self):
        """现在这条时间线：[(名称, 状态, 补注)]。"""
        return [tuple(s) for s in self._steps]

    def _paint_step(self, i, label, state, note):
        while len(self._rows) <= i:
            self._rows.append(self._make_step_row())
        row = self._rows[i]
        mark, role = self.MARKS.get(state, self.MARKS["wait"])
        icons.repaint(row.mark, mark, role, 15)
        row.lb.setText(label)
        row.note.setText(note)
        row.note.setVisible(bool(note))
        row.setVisible(True)

    def _make_step_row(self):
        row = QtWidgets.QWidget(self.box_steps)
        h = QtWidgets.QHBoxLayout(row)
        h.setContentsMargins(0, 0, 0, 0)
        h.setSpacing(7)
        row.mark = QtWidgets.QToolButton(row)
        row.mark.setObjectName("Icon")
        row.mark.setFixedSize(20, 20)
        row.mark.setAutoRaise(True)
        # 和状态点一样：只是画个记号，不接信号、不吃焦点
        row.mark.setFocusPolicy(QtCore.Qt.FocusPolicy.NoFocus)
        row.mark.setCursor(QtCore.Qt.CursorShape.ArrowCursor)
        row.lb = QtWidgets.QLabel(row)
        row.lb.setObjectName("StepLabel")
        row.note = QtWidgets.QLabel(row)
        row.note.setObjectName("StepNote")
        h.addWidget(row.mark)
        h.addWidget(row.lb)
        # 补注跟着步骤名走，不钉到右边界：结果卡横跨整窗，
        # 推到最右边时"写出序列"和它的进度隔着两千像素，等于没写。
        h.addWidget(row.note)
        h.addStretch(1)
        self.box_steps.layout().addWidget(row)
        return row

    def set_next(self, text):
        """下一步去哪儿是解释，不是事实 —— 这句只进悬停，不占卡面一行。"""
        self._next = text or ""
        self._sync_tip()
        return self

    def next_hint(self):
        """现在这条"下一步"（悬停里的最后一行）。"""
        return self._next

    def _sync_tip(self):
        """结论 + 状态解释 + 下一步，拼成悬停文字，点和标题都能触发。"""
        tip = "\n".join(p for p in (self.lb_title.text(), self._tip,
                                    self._next) if p)
        self.btn_mark.setToolTip(tip)
        self.lb_title.setToolTip(tip)

    def set_path(self, path, label=None):
        self._path = path
        show = bool(path)
        self.lb_path.setText((label or "成品：") + (path or ""))
        self.lb_path.setVisible(show)
        self.btn_open.setVisible(show)
        self.btn_open.setToolTip("在文件管理器里打开\n" + (path or ""))
        return self

    def set_detail(self, widget):
        """把模块原来那个日志框搬进折叠区（不新建、不改名，测试照旧找得到）。"""
        self._detail = widget
        self._detail.setParent(self.box_detail)
        # 卡片现在按内容算高，折叠区自己说得出要多高才看得清几行日志
        widget.setMinimumHeight(max(widget.minimumHeight(), 140))
        self.box_detail.layout().addWidget(self._detail)
        self.btn_more.setEnabled(True)
        return self

    # ------------------------------------------------------------------ 折叠
    def _show_details(self, on):
        on = bool(on) and self._detail is not None
        self.box_detail.setVisible(on)
        icons.repaint(self.btn_more, "chevron-down" if on else "chevron-right",
                      "off", 16)
        self.btn_more.setToolTip("收起技术细节" if on
                                 else "看技术细节（完整日志）")

    def details_open(self):
        return self.btn_more.isChecked()

    def status_kind(self):
        """当前结论的类别（idle/run/ok/warn/bad），给测试和配色用。"""
        return self._kind

    def metric_items(self):
        """卡片上现在显示的数字：[(键, 值, 解释或 None)]。"""
        return list(self._metrics)
