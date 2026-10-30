# -*- coding: utf-8 -*-
"""配乐模块：把一段音频写进视频文件。

画面一律不重编码（-c:v copy），只重排容器里的音频；延迟、音量、淡入淡出、
循环的算式与命令都在 lapse/audio.py，这里只负责取值、校验和那条时间线。
成片写完通过 Material 交给调色台与裁剪，跟合成、裁剪一样接进流水线。
"""
import math
import os

from PySide6 import QtCore, QtGui, QtWidgets

from lapse import audio as au
from lapse import ff as ffmod
from lapse.ff import unique_path
from lapse.gui import hints
from lapse.gui import job as jobs
from lapse.gui import theme, widgets
from lapse.gui.module import Module
from lapse.gui.workbench import APP_TITLE


class MixTimeline(QtWidgets.QWidget):
    """画面条 + 配乐条：晚进几秒、什么时候淡出、有没有铺满，一眼看得出来。"""

    BAR = 22

    def __init__(self, parent=None):
        super().__init__(parent)
        self.vdur = self.adur = self.end = self.out = 0.0
        self.opt = dict(au.DEFAULTS)
        self.setMinimumHeight(112)
        self.setSizePolicy(QtWidgets.QSizePolicy.Policy.Expanding,
                           QtWidgets.QSizePolicy.Policy.Preferred)

    def set_state(self, vdur, adur, opt):
        self.vdur, self.adur = max(0.0, float(vdur)), max(0.0, float(adur))
        self.opt = dict(opt)
        self.end = au.music_end(self.vdur, self.adur, self.opt)
        self.out = au.total_dur(self.vdur, self.adur, self.opt)
        self.update()

    def paintEvent(self, _e):
        p = QtGui.QPainter(self)
        p.setRenderHint(QtGui.QPainter.RenderHint.Antialiasing)
        p.fillRect(self.rect(), QtGui.QColor(theme.FIELD))
        r = QtCore.QRectF(self.rect()).adjusted(12, 22, -12, -26)
        if self.vdur <= 0:
            p.setPen(QtGui.QColor(theme.TEXT_OFF))
            p.drawText(self.rect(), QtCore.Qt.AlignmentFlag.AlignCenter,
                       "选一段视频和一段配乐后，这里画出两者的长度关系")
            return
        span = max(self.out, 0.001)

        def sx(t):
            return r.x() + r.width() * min(max(t / span, 0.0), 1.0)

        # 画面条
        vy = r.top()
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER_HI), 1))
        p.setBrush(QtGui.QColor(theme.BG3))
        p.drawRoundedRect(QtCore.QRectF(sx(0), vy, sx(self.vdur) - r.x(),
                                        self.BAR), 4, 4)
        p.setPen(QtGui.QColor(theme.TEXT_DIM))
        p.drawText(QtCore.QRectF(r.x(), vy - 16, r.width(), 14),
                   QtCore.Qt.AlignmentFlag.AlignLeft,
                   f"画面 {self.vdur:.1f}s")

        # 配乐条：淡入淡出画成斜坡，循环画成一段段的接缝
        my = vy + self.BAR + 14
        h = self.BAR + 6
        start = au.num(self.opt, "delay")
        end = self.end
        p.setPen(QtCore.Qt.PenStyle.NoPen)
        p.setBrush(QtGui.QColor(theme.ACCENT))
        w = max(2.0, sx(end) - sx(start))
        p.drawRoundedRect(QtCore.QRectF(sx(start), my, w, h), 4, 4)
        loop = bool(self.opt.get("loop")) and self.adur > 0
        if loop:
            p.setPen(QtGui.QPen(QtGui.QColor(theme.ACCENT_HI), 1,
                                QtCore.Qt.PenStyle.DashLine))
            k = self.adur
            while k < end - 0.01:
                p.drawLine(QtCore.QPointF(sx(start + k), my + 2),
                           QtCore.QPointF(sx(start + k), my + h - 2))
                k += self.adur
        # 淡入淡出按包络的形状画：淡入是从无到满的三角，淡出是收回去的三角
        for t0, d in ((au.num(self.opt, "fade_in"), "in"),
                      (au.num(self.opt, "fade_out"), "out")):
            if t0 <= 0:
                continue
            p.setBrush(QtGui.QColor(255, 255, 255, 70))
            if d == "in":
                a, b = sx(start), sx(min(start + t0, end))
                pts = [(a, my + h), (b, my), (b, my + h)]
            else:
                a, b = sx(max(start, end - t0)), sx(end)
                pts = [(a, my), (a, my + h), (b, my)]
            p.drawPolygon(QtGui.QPolygonF(
                [QtCore.QPointF(x, y) for x, y in pts]))
        p.setPen(QtGui.QColor(theme.TEXT_DIM))
        p.drawText(QtCore.QRectF(r.x(), my - 16, r.width(), 14),
                   QtCore.Qt.AlignmentFlag.AlignLeft,
                   f"配乐 {self.adur:.1f}s" + ("" if not loop else "（循环）"))

        # 片尾：配乐越过这条线就是在画面结束之后还在响
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BAD), 1,
                             QtCore.Qt.PenStyle.DashLine))
        p.drawLine(QtCore.QPointF(sx(self.vdur), vy - 4),
                   QtCore.QPointF(sx(self.vdur), my + h + 4))
        # 配乐没铺满时，画面条尾部那段是没声音的
        if not loop and end < self.vdur - 0.05:
            p.setPen(QtGui.QPen(QtGui.QColor(theme.TEXT_OFF), 1,
                                QtCore.Qt.PenStyle.DotLine))
            p.drawLine(QtCore.QPointF(sx(end), my + h + 6),
                       QtCore.QPointF(sx(self.vdur), my + h + 6))
            p.setPen(QtGui.QColor(theme.TEXT_OFF))
            p.drawText(QtCore.QRectF(sx(end), my + h + 8,
                                     max(0.0, sx(self.vdur) - sx(end)), 14),
                       QtCore.Qt.AlignmentFlag.AlignCenter, "这一段静音")

        # 刻度
        step = next((s for s in (1, 2, 5, 10, 15, 30, 60, 120)
                     if span / s <= 8), 300)
        f = p.font()
        f.setPixelSize(10)
        p.setFont(f)
        p.setPen(QtGui.QPen(QtGui.QColor(theme.SEP), 1))
        for k in range(0, int(math.ceil(span)) + 1, step):
            x = sx(k)
            p.drawLine(QtCore.QPointF(x, my + h + 4),
                       QtCore.QPointF(x, my + h + 9))
            p.setPen(QtGui.QColor(theme.TEXT_OFF))
            p.drawText(QtCore.QRectF(x - 24, my + h + 8, 48, 14),
                       QtCore.Qt.AlignmentFlag.AlignCenter, f"{k}s")
            p.setPen(QtGui.QPen(QtGui.QColor(theme.SEP), 1))
        p.setPen(QtGui.QPen(QtGui.QColor(theme.BORDER), 1))
        p.drawLine(QtCore.QPointF(r.x(), my + h + 4),
                   QtCore.QPointF(r.right(), my + h + 4))


class AudioMix(Module):
    name = "配乐"

    def __init__(self, shell):
        super().__init__(shell)
        self.vinfo = None
        self.ainfo = None
        self.info_err = ""
        self.out_manual = False
        self._own_out = None            # 自己刚写出的成片，别当新素材灌回来
        self._pending = None            # (路径, 载入方法)：防抖到期再读
        self._run_info = None       # 这次混音的口径，结果卡用它讲数字

    def top_hint(self):
        # 顶栏那句就是时长算式的结果：改了任何长度相关的旋钮都该看见它变
        return self.hint()

    def hint(self):
        if not self.vinfo or not self.ainfo:
            return ""
        o = self._opts()
        vd, ad = self.vinfo["dur"], self.ainfo["dur"]
        return (f"画面 {vd:.1f}s  配乐 {ad:.1f}s"
                f"  →  成片 {au.total_dur(vd, ad, o):.1f}s")

    # ------------------------------------------------------------------ UI
    def build(self):
        page = QtWidgets.QWidget()
        v = QtWidgets.QVBoxLayout(page)
        v.setContentsMargins(14, 12, 14, 12)
        v.setSpacing(10)

        self.ed_video = self._line("视频路径；也可以把素材直接拖进窗口")
        self.ed_video.textChanged.connect(
            lambda t: self._typed(t, self._load_video))
        self.ed_audio = self._line("配乐文件（mp3 / wav / aac / m4a / flac / ogg）")
        self.ed_audio.textChanged.connect(
            lambda t: self._typed(t, self._load_audio))
        v.addLayout(self._row("视频", self.ed_video, [
            ("folder-open", "选择视频", self.choose_video),
            ("play", "到工作台看这支片", self.play_in_grader),
            ("refresh", "重新读取这支视频",
             lambda: self._load_video(self.ed_video.text())),
        ]))
        v.addLayout(self._row("配乐", self.ed_audio, [
            ("folder-open", "选择音频文件", self.choose_audio),
            ("x", "清空配乐（只剩原声）", self.clear_audio),
        ]))

        self.lb_info = QtWidgets.QLabel("（未选择素材）")
        self.lb_info.setObjectName("Info")
        self.lb_info.setWordWrap(True)
        v.addWidget(self.lb_info)

        body = QtWidgets.QHBoxLayout()
        body.setSpacing(12)
        left = QtWidgets.QVBoxLayout()
        left.setSpacing(10)
        self.timeline = MixTimeline()
        left.addWidget(self.timeline)
        self.log_view = QtWidgets.QPlainTextEdit()
        self.log_view.setObjectName("Log")
        self.log_view.setReadOnly(True)
        self.log_view.setLineWrapMode(
            QtWidgets.QPlainTextEdit.LineWrapMode.NoWrap)
        self.log_view.setPlaceholderText(
            "实际执行的 ffmpeg 命令写在这里。")
        self.result = widgets.ResultCard("配乐结果")
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
        self.btn_run = QtWidgets.QPushButton("开始配乐")
        self.btn_run.setObjectName("Primary")
        widgets.label_button(self.btn_run, "配乐", role="on_accent")
        self.btn_run.clicked.connect(self.run)
        self.btn_open = widgets.icon_button(
            "folder-open", "在文件管理器里打开输出文件")
        self.btn_open.setEnabled(False)
        self.btn_open.clicked.connect(self.open_out)
        bar.addWidget(self.btn_run)
        bar.addStretch(1)
        bar.addWidget(self.btn_open)
        side.addLayout(bar)

        side.addWidget(self._build_track_box())
        side.addWidget(self._build_shape_box())
        side.addWidget(self._build_len_box())
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

        self._debounce = QtCore.QTimer(self)
        self._debounce.setSingleShot(True)
        self._debounce.setInterval(600)
        self._debounce.timeout.connect(self._flush_typed)
        self._restore()
        self._sync()
        return page

    def _line(self, ph):
        e = QtWidgets.QLineEdit()
        e.setPlaceholderText(ph)
        return e

    def _row(self, cap, edit, buttons):
        r = QtWidgets.QHBoxLayout()
        r.setSpacing(6)
        lb = QtWidgets.QLabel(cap)
        lb.setObjectName("FieldLabel")
        lb.setFixedWidth(28)
        r.addWidget(lb)
        r.addWidget(edit, 1)
        for name, tip, fn in buttons:
            b = widgets.icon_button(name, tip, box=30, size=17)
            b.clicked.connect(fn)
            r.addWidget(b)
        return r

    def _flabel(self, text):
        lb = QtWidgets.QLabel(text)
        lb.setObjectName("FieldLabel")
        return lb

    def _build_track_box(self):
        box = widgets.Group("音轨", reset_fn=self._reset_track)
        v = box.vbox(spacing=2)
        self.rb = {}
        self.mode_desc = {}
        for key, title, desc in au.MODES:
            r = QtWidgets.QRadioButton(title)
            r.setToolTip(desc)
            v.addWidget(r)
            self.rb[key] = r
            d = QtWidgets.QLabel(desc)
            d.setObjectName("ModeHint")
            d.setWordWrap(True)
            v.addWidget(d)
            self.mode_desc[key] = title
        self.rb["replace"].setChecked(True)

        self.lb_gain_v = self._vlabel()
        self.lb_orig_v = self._vlabel()
        g = box.grid()
        self.sp_delay = self._dspin(0, 600, 0.5, " 秒")
        hints.attach(self.sp_delay, "配乐/延迟")
        self.sp_delay.valueChanged.connect(self._sync)
        g.addWidget(widgets.field_block(
            "配乐晚进入", self.sp_delay, hints.line("配乐/延迟")), 0, 0)
        self.sl_gain = widgets.Slider(neutral=100)
        self.sl_gain.setRange(0, 200)
        self.sl_gain.setValue(100)
        hints.attach(self.sl_gain, "配乐/配乐音量")
        hints.attach(self.lb_gain_v, "配乐/配乐音量")
        g.addWidget(widgets.field_block("配乐音量", self._slider_row(
            self.sl_gain, self.lb_gain_v), hints.line("配乐/配乐音量")), 0, 1)
        self.sl_orig = widgets.Slider(neutral=100)
        self.sl_orig.setRange(0, 200)
        self.sl_orig.setValue(100)
        hints.attach(self.sl_orig, "配乐/原声音量")
        hints.attach(self.lb_orig_v, "配乐/原声音量")
        box.add(widgets.field_block("原声音量（仅混合）", self._slider_row(
            self.sl_orig, self.lb_orig_v), hints.line("配乐/原声音量")))
        for r in self.rb.values():
            r.toggled.connect(self._sync)
        return box

    def _vlabel(self):
        lb = QtWidgets.QLabel("100%")
        lb.setObjectName("ValueLabel")
        return lb

    def _slider_row(self, sl, lb):
        w = QtWidgets.QWidget()
        h = QtWidgets.QHBoxLayout(w)
        h.setContentsMargins(0, 0, 0, 0)
        h.setSpacing(6)
        h.addWidget(sl, 1)
        h.addWidget(lb)
        sl.valueChanged.connect(self._sync)
        return w

    def _build_shape_box(self):
        box = widgets.Group("淡入淡出", reset_fn=self._reset_shape)
        g = box.grid()
        self.sp_fin = self._dspin(0, 30, 0.5, " 秒")
        hints.attach(self.sp_fin, "配乐/淡入")
        self.sp_fout = self._dspin(0, 30, 0.5, " 秒")
        hints.attach(self.sp_fout, "配乐/淡出")
        g.addWidget(widgets.field_block(
            "淡入", self.sp_fin, hints.line("配乐/淡入")), 0, 0)
        g.addWidget(widgets.field_block(
            "淡出", self.sp_fout, hints.line("配乐/淡出")), 0, 1)
        for s in (self.sp_fin, self.sp_fout):
            s.valueChanged.connect(self._sync)
        return box

    def _build_len_box(self):
        box = widgets.Group("长度处理", reset_fn=self._reset_len)
        v = box.vbox(spacing=3)
        self.cb_loop = QtWidgets.QCheckBox("配乐比画面短就循环铺满")
        hints.attach(self.cb_loop, "配乐/循环")
        self.cb_trim = QtWidgets.QCheckBox("配乐比画面长就截到片尾")
        hints.attach(self.cb_trim, "配乐/截断")
        v.addWidget(self.cb_loop)
        v.addWidget(self.cb_trim)
        self.lb_len = QtWidgets.QLabel("")
        self.lb_len.setObjectName("FieldHint")
        self.lb_len.setWordWrap(True)
        v.addWidget(self.lb_len)
        for c in (self.cb_loop, self.cb_trim):
            c.toggled.connect(self._sync)
        return box

    def _build_out_box(self):
        box = widgets.Group("输出与编码")
        g = box.grid()
        self.cb_acodec = QtWidgets.QComboBox()
        for label, value in au.ACODECS:
            self.cb_acodec.addItem(label, value)
        hints.attach(self.cb_acodec, "配乐/编码")
        self.cb_acodec.currentIndexChanged.connect(self._sync)
        g.addWidget(widgets.field_block(
            "音频编码", self.cb_acodec, hints.line("配乐/编码")), 0, 0, 1, 2)

        self.ed_out = QtWidgets.QLineEdit()
        self.ed_out.setPlaceholderText("默认在视频旁生成 _mix 副本，原文件不动")
        self.ed_out.textEdited.connect(self._mark_manual)
        self.btn_save = QtWidgets.QPushButton("另存为…")
        self.btn_save.setObjectName("ToolBtn")
        self.btn_save.clicked.connect(self.choose_out)
        g.addWidget(self.ed_out, 1, 0)
        g.addWidget(self.btn_save, 1, 1)
        self.lb_ac_hint = QtWidgets.QLabel("")
        self.lb_ac_hint.setObjectName("FieldHint")
        self.lb_ac_hint.setWordWrap(True)
        g.addWidget(self.lb_ac_hint, 2, 0, 1, 2)
        g.setColumnStretch(0, 1)
        return box

    def _dspin(self, lo, hi, step, suffix):
        s = QtWidgets.QDoubleSpinBox()
        s.setRange(lo, hi)
        s.setSingleStep(step)
        s.setDecimals(1)
        s.setSuffix(suffix)
        s.setAlignment(QtCore.Qt.AlignmentFlag.AlignRight)
        return s

    # ------------------------------------------------------------------ 复位
    def _reset_track(self):
        self.rb["replace"].setChecked(True)
        self.sp_delay.blockSignals(True)
        self.sp_delay.setValue(0.0)
        self.sp_delay.blockSignals(False)
        self.sl_gain.setValue(au.DEFAULTS["gain"])
        self.sl_orig.setValue(au.DEFAULTS["orig_gain"])
        self._sync()

    def _reset_shape(self):
        self.sp_fin.setValue(0.0)
        self.sp_fout.setValue(0.0)

    def _reset_len(self):
        self.cb_loop.setChecked(False)
        self.cb_trim.setChecked(True)

    # ------------------------------------------------------------------ 取值
    def _opts(self):
        return {"mode": self._mode(),
                "delay": self.sp_delay.value(),
                "gain": self.sl_gain.value(),
                "orig_gain": self.sl_orig.value(),
                "fade_in": self.sp_fin.value(),
                "fade_out": self.sp_fout.value(),
                "loop": self.cb_loop.isChecked(),
                "trim": self.cb_trim.isChecked(),
                "acodec": self.cb_acodec.currentData() or "auto"}

    def _mode(self):
        for key, r in self.rb.items():
            if r.isChecked():
                return key
        return "replace"

    # ------------------------------------------------------------------ 素材
    def choose_video(self):
        p, _ = QtWidgets.QFileDialog.getOpenFileName(
            self.shell, "选择视频", "",
            "视频 (*.mp4 *.mov *.m4v *.mkv *.avi *.ts *.mts *.m2ts *.webm *.wmv "
            "*.hevc *.h264);;所有文件 *.*")
        if p:
            self._load_video(p)

    def choose_audio(self):
        p, _ = QtWidgets.QFileDialog.getOpenFileName(
            self.shell, "选择配乐", "",
            "音频 (*.mp3 *.wav *.aac *.m4a *.flac *.ogg *.opus *.wma *.aif "
            "*.aiff);;所有文件 *.*")
        if p:
            self._load_audio(p)

    def clear_audio(self):
        self.ed_audio.clear()
        self.ainfo = None
        self.info_err = ""
        self._sync()

    def _typed(self, text, fn):
        if text.strip():
            self._pending = (text.strip(), fn)
            self._debounce.start()

    def _flush_typed(self):
        """手打路径别边打字边探测：停顿下来再读那一条。"""
        pending, self._pending = self._pending, None
        if pending:
            pending[1](pending[0])

    def _load_video(self, path):
        path = (path or "").strip().strip('"')
        if not path or not os.path.isfile(path):
            return
        self._debounce.stop()
        if not self.shell.ff:
            self._no_ffmpeg()
            return
        info = ffmod.video_info(self.shell.ff, path)
        if not info:
            self.vinfo = None
            self.info_err = f"读不到视频流：{path}"
            self._set_text(self.ed_video, path)
            self._sync()
            return
        self.vinfo = info
        self.info_err = ""
        self._set_text(self.ed_video, path)
        self._own_out = None
        self.out_manual = False
        self.log(f"视频: {self._vline()}")
        self._sync()

    def _load_audio(self, path):
        path = (path or "").strip().strip('"')
        if not path:
            self.ainfo = None
            self.info_err = ""
            self._sync()
            return
        if not os.path.isfile(path):
            return
        self._debounce.stop()
        if not self.shell.ff:
            self._no_ffmpeg()
            return
        info = ffmod.probe_audio(self.shell.ff, path)
        if not info:
            self.ainfo = None
            self.info_err = f"这个文件里没有音频流：{os.path.basename(path)}"
            self._sync()
            return
        self.ainfo = info
        self.info_err = ""
        self._set_text(self.ed_audio, path)
        self._own_out = None
        self.out_manual = False
        self.log(f"配乐: {self._aline()}")
        self._sync()

    @staticmethod
    def _set_text(edit, path):
        norm = os.path.normpath(path)
        if os.path.normpath(edit.text()) != norm:
            edit.blockSignals(True)
            edit.setText(norm)
            edit.blockSignals(False)

    def _set_info(self, text, bad):
        """信息条只有一处在写：改角色时顺手重刷样式，否则红色会赖着不走。"""
        role = "InfoBad" if bad else "Info"
        if self.lb_info.objectName() != role:
            self.lb_info.setObjectName(role)
            self._repolish(self.lb_info)
        self.lb_info.setText(text)

    @staticmethod
    def _repolish(w):
        st = w.style()
        st.unpolish(w)
        st.polish(w)

    def _vline(self):
        i = self.vinfo
        return (f"{i['codec']}  {ffmod.disp_text(i)}  {i['fps']:.3g}fps  "
                f"{i['dur']:.1f}s  " + ("自带音轨" if i["audio"] else "无音轨"))

    def _aline(self):
        i = self.ainfo
        return (f"{i['codec']}  {i['dur']:.1f}s  {i['sr'] or '?'}Hz  "
                f"{i['layout'] or '声道未知'}")

    # ------------------------------------------------------------------ 同步
    def _sync(self):
        parts = []
        if self.vinfo:
            parts.append("视频 " + self._vline())
        if self.ainfo:
            parts.append("配乐 " + self._aline())
        # 探测失败那句话要一直顶到用户换素材为止，不能被半截正常信息冲掉
        if self.info_err:
            self._set_info(self.info_err, bad=True)
        else:
            self._set_info("   ·   ".join(parts) or "（未选择素材）", bad=False)
        self.lb_gain_v.setText(f"{self.sl_gain.value()}%")
        self.lb_orig_v.setText(f"{self.sl_orig.value()}%")

        mix = self._mode() == "mix"
        # 视频本来就没声音，混合这条路是死的：留着只会让 ffmpeg 报流匹配失败
        self.rb["mix"].setEnabled(self.vinfo is not None
                                 and bool(self.vinfo["audio"]))
        if mix and not self.rb["mix"].isEnabled():
            self.rb["replace"].setChecked(True)
            mix = False
        self.sl_orig.setEnabled(mix)

        opt = self._opts()
        vdur = self.vinfo["dur"] if self.vinfo else 0.0
        adur = self.ainfo["dur"] if self.ainfo else 0.0
        self.timeline.set_state(vdur, adur, opt)
        self.lb_len.setText(self._len_note(vdur, adur, opt))
        self._sync_acodec(opt)
        self._refresh_out()
        self.status(self.hint())
        self.sync_page_hint()

    def _len_note(self, vdur, adur, opt):
        if not vdur or not adur:
            return "选好两段素材后这里算出成片长度。"
        end = au.music_end(vdur, adur, opt)
        out = au.total_dur(vdur, adur, opt)
        if opt["loop"]:
            return f"配乐整段反复铺到片尾，成片 {out:.1f}s。"
        if end < vdur - 0.05:
            return (f"配乐在 {end:.1f}s 就停了，最后 {vdur - end:.1f}s 没有声音"
                    f"（想铺满就勾「循环」）。成片 {out:.1f}s。")
        if end > vdur + 0.05:
            return (f"配乐比画面长 {end - vdur:.1f}s，画面结束后还在响"
                    f"（想对齐片尾就勾「截到片尾」）。成片 {out:.1f}s。")
        # 截断生效时别报"正好"：那段配乐是被切掉的，用户得知道少了什么
        cut = adur - max(0.0, end - au.num(opt, "delay"))
        if cut > 0.05:
            return f"配乐放不进画面，片尾截掉 {cut:.1f}s。成片 {out:.1f}s。"
        return f"配乐正好收到片尾。成片 {out:.1f}s。"

    def _sync_acodec(self, opt):
        """把做不到的选项就地灰掉，而不是等 ffmpeg 报错。"""
        ext = self._out_ext()
        codec = (self.ainfo or {}).get("codec")
        can = self.ainfo is not None and au.copy_ok(ext, codec, opt)
        m = self.cb_acodec.model()
        i = self.cb_acodec.findData("copy")
        m.item(i).setEnabled(can)
        if not can and self.cb_acodec.currentData() == "copy":
            self.cb_acodec.blockSignals(True)
            self.cb_acodec.setCurrentIndex(self.cb_acodec.findData("auto"))
            self.cb_acodec.blockSignals(False)
        if self.cb_acodec.currentData() == "copy":
            self.lb_ac_hint.setText("不转码：音频原样搬进容器，最快也最保真。")
        elif au.needs_filter(opt):
            self.lb_ac_hint.setText(
                "延迟、音量、淡入淡出、混音都要重编音频，所以这里必然转码；"
                "画面仍然一个比特都不改。")
        elif not can:
            self.lb_ac_hint.setText(
                f"{(codec or '未知').upper()} 放进 {ext} 容器播放器不一定认，"
                f"自动会转成 AAC。")
        else:
            self.lb_ac_hint.setText("自动：能原样放就复制，放不进容器才转 AAC。")

    def _out_ext(self):
        t = os.path.splitext(self.ed_out.text())[1].lower()
        if t in au.VIDEO_OUT_EXTS:
            return t
        if self.vinfo:
            s = os.path.splitext(self.vinfo["path"])[1].lower()
            if s in au.VIDEO_OUT_EXTS:
                return s
        return ".mp4"

    def _refresh_out(self):
        if self.out_manual or not self.vinfo:
            return
        stem = os.path.splitext(self.vinfo["path"])[0]
        self.ed_out.setText(unique_path(stem + "_mix" + self._out_ext()))

    def _mark_manual(self, _text):
        self.out_manual = True

    def choose_out(self):
        p, _ = QtWidgets.QFileDialog.getSaveFileName(
            self.shell, "输出文件", self.ed_out.text(),
            f"视频 (*{self._out_ext()});;MP4 (*.mp4);;Matroska (*.mkv)")
        if p:
            self.out_manual = True
            if os.path.splitext(p)[1].lower() not in au.VIDEO_OUT_EXTS:
                p += self._out_ext()
            self.ed_out.setText(os.path.normpath(p))

    # ------------------------------------------------------------------ 执行
    def run(self):
        if not self.shell.ff:
            self._no_ffmpeg()
            return
        if not self.vinfo:
            self.warn("先选一个视频。")
            return
        if not self.ainfo:
            self.warn("先选一段配乐音频。")
            return
        dst = self.ed_out.text().strip().strip('"')
        if not dst:
            self.warn("请填写输出文件路径。")
            return
        if os.path.splitext(dst)[1].lower() not in au.VIDEO_OUT_EXTS:
            dst += self._out_ext()          # 没扩展名 ffmpeg 猜不出封装格式
        dst = unique_path(dst)
        od = os.path.dirname(dst)
        if od:
            os.makedirs(od, exist_ok=True)
        self.ed_out.setText(dst)

        opt = self._opts()
        cmd, dur = au.build_cmd(self.shell.ff, self.vinfo["path"],
                                self.ainfo["path"], dst, self.vinfo,
                                self.ainfo, opt)
        orig = f"  原声 {opt['orig_gain']}%" if opt["mode"] == "mix" else ""
        self.log(f"模式: {self.mode_desc[opt['mode']]}  "
                 f"延迟 {opt['delay']:.1f}s  配乐 {opt['gain']}%{orig}")
        self.log(f"淡入 {opt['fade_in']:.1f}s  淡出 {opt['fade_out']:.1f}s  "
                 f"循环 {'开' if opt['loop'] else '关'}  "
                 f"截断 {'开' if opt['trim'] else '关'}")
        self.log("画面流复制，不重新编码。")
        self._save()
        # 定格这次跑的口径：结果卡要说"刚才那版是怎么拼的"，
        # 而不是你事后又改过的滑块
        self._run_info = {"opt": dict(opt), "dur": dur,
                          "vd": self.vinfo["dur"], "ad": self.ainfo["dur"],
                          "mode": self.mode_desc[opt["mode"]]}
        job = jobs.Job([(None, cmd, jobs.Meter("time", dur), dst, None)])
        job.log.connect(self.log)
        if self.shell.start_job(
                job, lambda ok, res: self._finished(dst, ok, res),
                self.result):
            self.btn_run.setEnabled(False)
            self.result.set_status("run", f"混音中 · 成片 {dur:.1f}s")
            self.result.set_metrics([])
            self.result.set_path(None)
            self.result.set_next("")

    # 配乐最容易看不懂的是"时长怎么算出来的"和"画面有没有被重编码"，
    # 所以这两件事各占一个指标。
    METRICS = (
        ("成片时长", "画面时长与配乐长度按循环/截断规则算出来的结果。"
                  "比画面长就多循环了一段，比画面短是关掉了循环。"),
        ("画面时长", "源视频长度。混音不改画面，成片应等于它（除非音乐更短且不循环）。"),
        ("配乐时长", "音频文件本身长度，和循环开关一起决定成片多长。"),
        ("处理方式", "换成静音、只留原声、还是两者按比例混合。"
                  "混合时这里写的原声比例就是背景音被压到多少。"),
        ("淡入淡出", "音乐进出的秒数。成片一头一尾各留 1~2s 才不会突然响、突然停。"),
        ("文件体积", "画面是流复制的，体积应当和源文件接近；小很多说明画面被截断了。"),
    )

    def _show_result(self, dst):
        i = getattr(self, "_run_info", None) or {}
        o = i.get("opt") or {}
        try:
            size = jobs.human_mb(os.path.getsize(dst))
        except OSError:
            size = "—"
        vd, ad = i.get("vd"), i.get("ad")
        loop = "循环" if o.get("loop") else "不循环"
        trim = "截断" if o.get("trim") else "留完整"
        vals = [f"{i.get('dur', 0):.1f}s" if i.get("dur") else "—",
                "—" if vd is None else f"{vd:.1f}s",
                "—" if ad is None else f"{ad:.1f}s",
                f"{i.get('mode', '')}（配乐 {o.get('gain', 0):.0f}%）",
                f"{loop} · {trim}",
                size]
        self.result.set_metrics(
            [(name, val, tip) for (name, tip), val in zip(self.METRICS, vals)])
        # 音乐比画面短得多又不循环，成片后半段就是静音
        short = vd and ad and not o.get("loop") and ad < vd * 0.6
        self.result.set_status(
            "warn" if short else "ok",
            f"配乐完成 · {i.get('dur', 0):.1f}s · {size}",
            "音乐比画面短且不循环，后半段会没有配乐" if short else None)
        self.result.set_path(dst, "成品：")
        self.result.set_next(
            "打开成片听一遍；要接着调色就切到调色台，这个成品已经在那边了。")

    def _finished(self, dst, ok, res):
        self.btn_run.setEnabled(True)
        if ok and res:
            self._own_out = dst
            self.btn_open.setEnabled(True)
            self.out_manual = False
            self._show_result(dst)
            self.log(f"\n完成 → {dst}")
            self.status(f"配乐完成  {os.path.basename(dst)}")
            self.shell.material.set_video(dst)   # 流水线：交给裁剪/调色台
        elif res == "cancelled":
            self.log("已取消，半成品已删除")
            self.result.set_status("idle", "已取消 · 半成品已删除")
            self.result.set_next("原文件没动，可以直接再来一次。")
            self.status("已取消")
        else:
            self.log(f"失败：{res}")
            self.result.set_status("bad", "配乐失败", res)
            self.result.set_next("展开技术细节看 ffmpeg 最后几行。")
            self.status("配乐失败，详见日志")

    def open_out(self):
        widgets.reveal(self.ed_out.text().strip().strip('"') or None)

    def _no_ffmpeg(self):
        QtWidgets.QMessageBox.critical(
            self.shell, APP_TITLE,
            "未找到 ffmpeg，无法混流。\n请在运行本工具的 Python 环境里 "
            "pip install imageio-ffmpeg。")

    def play_in_grader(self):
        """把当前视频交给调色台播放：这里只混音，看画面还是去那边。"""
        path = (self.ed_video.text() or "").strip()
        if not path or not os.path.isfile(path):
            self.warn("先选一个视频")
            return
        dev = self.shell.module("调色台")
        if dev is not None and not self.shell.material.set_video(path):
            dev.load_video(path)
        self.shell.set_module("调色台")

    # ------------------------------------------------------------------ 流水线
    def on_material(self, kind, path):
        if kind != "video" or not path:
            return
        if self._own_out and os.path.normpath(path) == \
                os.path.normpath(self._own_out):
            return
        self._load_video(path)

    def log(self, text):
        if text:
            self.log_view.appendPlainText(text)
            sb = self.log_view.verticalScrollBar()
            sb.setValue(sb.maximum())

    # ------------------------------------------------------------------ 记忆
    def _restore(self):
        s = self.settings()
        mode = s.value(f"{self.name}/mode", "replace", type=str)
        if mode in self.rb:
            self.rb[mode].setChecked(True)
        self.sp_delay.setValue(float(s.value(f"{self.name}/delay", 0.0)))
        self.sl_gain.setValue(int(s.value(f"{self.name}/gain", 100)))
        self.sl_orig.setValue(int(s.value(f"{self.name}/orig_gain", 100)))
        self.sp_fin.setValue(float(s.value(f"{self.name}/fade_in", 0.0)))
        self.sp_fout.setValue(float(s.value(f"{self.name}/fade_out", 0.0)))
        self.cb_loop.setChecked(s.value(f"{self.name}/loop", "0") == "1")
        self.cb_trim.setChecked(s.value(f"{self.name}/trim", "1") == "1")
        i = self.cb_acodec.findData(s.value(f"{self.name}/acodec", "auto"))
        if i >= 0:
            self.cb_acodec.setCurrentIndex(i)

    def _save(self):
        s = self.settings()
        s.setValue(f"{self.name}/mode", self._mode())
        s.setValue(f"{self.name}/delay", self.sp_delay.value())
        s.setValue(f"{self.name}/gain", self.sl_gain.value())
        s.setValue(f"{self.name}/orig_gain", self.sl_orig.value())
        s.setValue(f"{self.name}/fade_in", self.sp_fin.value())
        s.setValue(f"{self.name}/fade_out", self.sp_fout.value())
        s.setValue(f"{self.name}/loop",
                   "1" if self.cb_loop.isChecked() else "0")
        s.setValue(f"{self.name}/trim",
                   "1" if self.cb_trim.isChecked() else "0")
        s.setValue(f"{self.name}/acodec", self.cb_acodec.currentData())

    def shutdown(self):
        self._save()
